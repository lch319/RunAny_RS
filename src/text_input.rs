//! 一个最小可用的文本输入框（含 IME、选区、剪贴板）。
//!
//! 直接取自 gpui 0.2.2 自带的 `examples/input.rs`（MIT/Apache），做了三处改动：
//! 1. 配色换成本项目的 `theme` 令牌；
//! 2. 去掉示例里的外框与「最近按键」演示，只留输入本体，方便嵌进设置页的行里；
//! 3. 增加 `new()` / `text()` / `set_text()`，让设置界面能读写内容。
//!
//! 为什么不用「监听 key_down 自己拼字符串」：那样收不到输入法（中文）的组字过程，
//! `EntityInputHandler` 才是正路 —— 中文、候选词、粘贴、选区都靠它。

use std::ops::Range;

use gpui::{actions, prelude::*};
use gpui::{
    div, fill, point, px, relative, size, App, Bounds, ClipboardItem, Context, CursorStyle,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, ShapedLine, SharedString,
    Style, TextRun, UTF16Selection, UnderlineStyle, Window,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
    ]
);

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// 没有内容时的字号（`window.text_style()` 取不到时兜底）
    font_size: f32,
    /// 撤销栈（Ctrl+Z / Ctrl+Y）
    history: crate::undo::History,
    /// 已经挂过「失焦」监听（`cx.on_blur` 只能在有 window 的地方挂，所以拖到第一次渲染）
    blur_hooked: bool,
}

impl TextInput {
    pub fn new(
        initial: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        font_size: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let content: SharedString = initial.into();
        let end = content.len();
        Self {
            focus_handle: cx.focus_handle(),
            content,
            placeholder: placeholder.into(),
            // 光标落在末尾
            selected_range: end..end,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            font_size,
            history: crate::undo::History::default(),
            blur_hooked: false,
        }
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
        // 这个动作我们处理了：别让 window 再跑一遍 key_down 兜底（否则中文输入法会重复选中）
        cx.stop_propagation();
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    // ---- 撤销 / 重做 ----

    fn snapshot(&self) -> crate::undo::Snapshot {
        crate::undo::Snapshot {
            text: self.content.to_string(),
            caret: self.selected_range.clone(),
        }
    }

    fn restore(&mut self, snap: crate::undo::Snapshot, cx: &mut Context<Self>) {
        let len = snap.text.len();
        self.content = snap.text.into();
        self.selected_range = crate::undo::clamp_caret(snap.caret, len);
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.do_undo(cx);
        cx.stop_propagation();
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        self.do_redo(cx);
        cx.stop_propagation();
    }

    /// 失焦：把输入法正在组的那一段取消掉。
    ///
    /// 搜狗这类输入法在「拼音打了一半、光标被点走」时，候选窗会留在屏幕上，
    /// 而且键盘还被它攥着 —— 这时候按退格既不删字也没反应，很像程序卡住了。
    /// 正常做法是失焦时主动告诉输入法「这段不要了」。
    fn on_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        crate::win32::cancel_ime_composition_in(window);
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        self.do_paste(window, cx);
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.do_copy(cx);
        cx.stop_propagation();
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.do_cut(window, cx);
        cx.stop_propagation();
    }

    // ---- 给右键菜单 / 按键兜底用的公开入口 ----

    /// 有选中内容吗（右键菜单里「复制 / 剪切」要不要点亮看它）。
    pub fn has_selection(&self) -> bool {
        !self.selected_range.is_empty()
    }

    pub fn do_copy(&mut self, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            return;
        }
        let text = self.content[self.selected_range.clone()].to_string();
        // 双保险：gpui 走平台剪贴板，失败时再自己写一次（有些程序会短暂占着剪贴板）
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        if crate::win32::read_clipboard_text().as_deref() != Some(text.as_str()) {
            crate::win32::write_clipboard_text(&text);
        }
    }

    pub fn do_cut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            return;
        }
        self.do_copy(cx);
        self.replace_text_in_range(None, "", window, cx);
    }

    pub fn do_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .or_else(crate::win32::read_clipboard_text);
        if let Some(text) = text {
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }

    pub fn do_select_all(&mut self, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    pub fn do_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
        self.replace_text_in_range(None, "", window, cx);
    }

    /// 还能撤销 / 重做吗（右键菜单那两条要不要点亮看它）。
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn do_undo(&mut self, cx: &mut Context<Self>) {
        let current = self.snapshot();
        if let Some(snap) = self.history.undo(current) {
            self.restore(snap, cx);
        }
    }

    pub fn do_redo(&mut self, cx: &mut Context<Self>) {
        let current = self.snapshot();
        if let Some(snap) = self.history.redo(current) {
            self.restore(snap, cx);
        }
    }

    /// 在光标处插入一段文本（编辑窗口的占位符胶囊用它）。
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.history
            .record(&self.content, self.selected_range.clone());
        let range = self.selected_range.clone();
        self.content =
            (self.content[0..range.start].to_owned() + text + &self.content[range.end..]).into();
        let caret = range.start + text.len();
        self.selected_range = caret..caret;
        self.marked_range = None;
        cx.notify();
    }

    /// 删掉光标前一个「字形簇」（菜单搜索框的退格用，不用走 action 派发）。
    pub fn delete_backward(&mut self, cx: &mut Context<Self>) {
        self.history
            .record(&self.content, self.selected_range.clone());
        if !self.selected_range.is_empty() {
            // 有选区就删选区
            self.content =
                (self.content[0..self.selected_range.start].to_owned()
                    + &self.content[self.selected_range.end..])
                    .into();
            let caret = self.selected_range.start;
            self.selected_range = caret..caret;
        } else {
            let end = self.cursor_offset();
            let start = self.previous_boundary(end);
            self.content = (self.content[0..start].to_owned() + &self.content[end..]).into();
            self.selected_range = start..start;
        }
        self.marked_range = None;
        cx.notify();
    }

    /// 现在是不是正在「组字」（中文输入法的候选还没上屏）。
    /// 组字期间，回车应该交给输入法确认候选，而不是触发菜单执行。
    pub fn is_composing(&self) -> bool {
        self.marked_range.is_some()
    }

    /// 右键：落在选区外面就把光标挪过去（和系统输入框一致），然后弹菜单。
    fn on_right_click(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let index = self.index_for_mouse_position(event.position);
        if !self.selected_range.contains(&index) {
            self.move_to(index, cx);
        }
        crate::text_menu::open(
            crate::text_menu::Target::Input(cx.entity()),
            window,
            event.position,
            cx,
        );
    }

    /// `Ctrl + C/V/X/A` 的兜底：万一键盘绑定没生效（换窗口、输入法抢键），
    /// 这里也能把事办了。动作那条路会 `stop_propagation`，所以不会重复执行。
    fn on_key_fallback(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        if !keystroke.modifiers.control || keystroke.modifiers.alt {
            return;
        }
        match keystroke.key.as_str() {
            // shift 的 ctrl+shift+z 也算重做
            "z" if keystroke.modifiers.shift => self.redo(&Redo, window, cx),
            "z" => self.undo(&Undo, window, cx),
            "y" => self.redo(&Redo, window, cx),
            "c" => self.do_copy(cx),
            "x" => self.do_cut(window, cx),
            "v" => self.do_paste(window, cx),
            "a" => self.do_select_all(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 点一下就聚焦：外层容器有时候也会在 mouse_down 里抢焦点（菜单项页那张
        // 大卡片以前就是这样），自己先拿一次能保证输入框始终能打字。
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    /// 换内容（编辑窗口切换目标项时复用同一个输入框）。
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        // 换的是另一份内容（切编辑目标、恢复默认），历史得跟着清 ——
        // 否则撤销会把上一个菜单项的文字搬过来
        self.history.clear();
        self.content = text.into();
        let end = self.content.len();
        self.selected_range = end..end;
        self.marked_range = None;
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl TextInput {
    /// 把输入法的组字窗 / 候选窗摆到光标处（组字过程中每帧调用，很便宜）。
    fn sync_ime_position(&self, window: &mut Window) {
        let (Some(bounds), Some(layout)) = (self.last_bounds, self.last_layout.as_ref()) else {
            return;
        };
        let caret = self.cursor_offset();
        let x = bounds.left() + layout.x_for_index(caret);
        crate::win32::place_ime_in(
            window,
            f32::from(x),
            f32::from(bounds.top()),
            f32::from(bounds.size.height),
        );
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        // 输入法组字过程中的反复替换不记快照：撤销一次退一个拼音字母，
        // 那是折腾，不是功能。组字开始前那一下（marked_range 还是空）会记。
        if self.marked_range.is_none() {
            self.history
                .record(&self.content, self.selected_range.clone());
        }
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        self.sync_ime_position(window);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        // 每变一次组字内容就把候选窗重新摆到光标下：搜狗会复用同一个候选窗，
        // 不主动设的话它会停在上一次的位置（「不在输入框旁边」）。
        self.sync_ime_position(window);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), theme::ui_text_tertiary().into())
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(1.5), bounds.bottom() - bounds.top()),
                    ),
                    theme::accent(),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    theme::accent_soft(),
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), window, cx)
            .unwrap();

        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 失焦监听只能在这里挂（要有 window）。挂一次就够，用标志位记着。
        if !self.blur_hooked {
            self.blur_hooked = true;
            let handle = self.focus_handle.clone();
            cx.on_blur(&handle, window, |this, window, cx| this.on_blur(window, cx))
                .detach();
        }
        div()
            .flex()
            .size_full()
            // 长文本（比如图标的绝对路径）必须裁在框里，不然字形会画到框外面，
            // 把右边挨着的按钮整个盖掉
            .overflow_hidden()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .text_size(px(self.font_size))
            .text_color(theme::text_primary())
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            // 绑定万一没命中，这里兜一层（动作命中时会 stop_propagation，不会重复执行）
            .on_key_down(cx.listener(Self::on_key_fallback))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            // 右键菜单：复制 / 剪切 / 粘贴 / 全选 / 清空
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_click))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
