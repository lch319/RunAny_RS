//! 多行文本编辑框（悬停提示的「内容模板」用它）。
//!
//! 为什么另起一个而不是把 `text_input` 改成多行：那个是设置页 / 编辑窗口里
//! 几十个单行输入框在用的，多行要额外管换行、Home/End、上下移行、跨行选区、
//! 折行后的光标位置 —— 全塞进去容易顾此失彼。这里只用一份自己的排版结果。
//!
//! 排版靠 `shape_text`：它按 `\n` 切逻辑行，每行再按折行宽度切成若干「视觉行」。
//! 所以内部存两样东西：
//!
//! - `lines`：每个逻辑行一个 `WrappedLine`（行内已经折好）；
//! - `starts`：每个逻辑行在全文里的起始字节。
//!
//! 「全局字节偏移」和「屏幕坐标」之间的换算全在这两样上做（`position_for` /
//! `index_for`），光标、选区、上下移动、鼠标点选都用同一套，不会互相打架。

use std::ops::Range;

use gpui::prelude::*;
use gpui::{
    div, fill, point, px, relative, size, App, Bounds, ClipboardItem, Context, CursorStyle, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, SharedString, ShapedLine, Style,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

/// 默认至少显示几行（内容少的时候也别塌成一条线）
const MIN_ROWS: usize = 7;
/// 行高 = 字号 × 这个倍数
const LINE_HEIGHT_RATIO: f32 = 1.62;

pub struct TextArea {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// 上一次排版的结果（一个元素 = 一个逻辑行）
    lines: Vec<WrappedLine>,
    /// 每个逻辑行的起始字节
    starts: Vec<usize>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    font_size: f32,
    line_height: f32,
    /// 上一次折行用了多宽（还没排过版时是 None）
    wrap_width: Option<f32>,
    /// 上一次排出来一共几行（视觉行），元素定高用
    rows: usize,
    /// 至少显示几行（模板编辑器要高一点，排除规则这种矮一点）
    min_rows: usize,
    /// 撤销栈（Ctrl+Z / Ctrl+Y）
    history: crate::undo::History,
    /// 已经挂过失焦监听（`cx.on_blur` 需要有 window）
    blur_hooked: bool,
}

impl TextArea {
    pub fn new(
        initial: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        font_size: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let content: SharedString = initial.into();
        let starts = line_starts(&content);
        // 占位提示是**单行**画的（`shape_line` 不允许换行，给了就 panic），
        // 外面顺手写了个 `\n` 也不该把进程带走
        let placeholder: SharedString = placeholder.into();
        let placeholder: SharedString = if placeholder.contains('\n') {
            placeholder.replace('\n', " ").into()
        } else {
            placeholder
        };
        Self {
            focus_handle: cx.focus_handle(),
            content,
            placeholder,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            lines: Vec::new(),
            starts,
            last_bounds: None,
            is_selecting: false,
            font_size,
            line_height: (font_size * LINE_HEIGHT_RATIO).round().max(font_size + 2.0),
            wrap_width: None,
            rows: MIN_ROWS,
            min_rows: MIN_ROWS,
            history: crate::undo::History::default(),
            blur_hooked: false,
        }
    }

    /// 少显示几行（比如「一行一条」的排除规则，矮一点更合适）。
    pub fn with_min_rows(mut self, rows: usize) -> Self {
        self.min_rows = rows.max(1);
        self.rows = self.min_rows;
        self
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    /// 换内容（「恢复默认模板」/ 撤销之后用）。光标落到末尾。
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        // 换的是另一份内容：历史跟着清，否则撤销会把别处的内容搬过来
        self.history.clear();
        self.content = text.into();
        self.starts = line_starts(&self.content);
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    pub fn has_selection(&self) -> bool {
        !self.selected_range.is_empty()
    }

    pub fn do_copy(&mut self, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            return;
        }
        let text = self.content[self.selected_range.clone()].to_string();
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        // 双保险：有些程序会短暂占着剪贴板，gpui 那次可能悄悄失败了
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
            // 和单行输入框不一样：这里的换行是**有意义**的（一行一条），原样保留
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace_text_in_range(None, &text, window, cx);
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

    /// 在光标处插入一段文本（占位符胶囊用它）。
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.history
            .record(&self.content, self.selected_range.clone());
        let range = self.selected_range.clone();
        self.content =
            (self.content[0..range.start].to_owned() + text + &self.content[range.end..]).into();
        self.starts = line_starts(&self.content);
        let caret = range.start + text.len();
        self.selected_range = caret..caret;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    // ---- 光标 / 选区 ----

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
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
        cx.notify();
    }

    /// 光标在内容坐标系里的位置（相对文本左上角，不含内边距）
    fn position(&self, offset: usize) -> Point<Pixels> {
        position_for(&self.lines, &self.starts, px(self.line_height), offset)
    }

    /// 内容坐标系里的一点 → 全局字节偏移
    fn index_at(&self, p: Point<Pixels>) -> usize {
        index_for(&self.lines, &self.starts, px(self.line_height), p)
    }

    /// 当前逻辑行的起止（含行首 / 行尾，不含换行符）
    fn line_bounds(&self, offset: usize) -> (usize, usize) {
        let limit = offset.min(self.content.len());
        let start = self.content[..limit]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let mut rest = self.content[start..].char_indices();
        while let Some((index, ch)) = rest.next() {
            if ch == '\n' {
                return (start, start + index);
            }
        }
        (start, self.content.len())
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn move_horizontal(&mut self, forward: bool, extend: bool, cx: &mut Context<Self>) {
        // 有选区、又没按 shift：先收掉选区（和系统输入框一致）
        if !extend && !self.selected_range.is_empty() {
            let offset = if forward {
                self.selected_range.end
            } else {
                self.selected_range.start
            };
            self.move_to(offset, cx);
            return;
        }
        let cursor = self.cursor_offset();
        let offset = if forward {
            self.next_boundary(cursor)
        } else {
            self.previous_boundary(cursor)
        };
        if extend {
            self.select_to(offset, cx)
        } else {
            self.move_to(offset, cx)
        }
    }

    fn move_vertical(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let from = self.position(cursor);
        let line_h = self.line_height;
        let y = (f32::from(from.y) + if down { line_h } else { -line_h }).max(0.0);
        if !down && y <= 0.0 && f32::from(from.y) <= 0.0 {
            // 已经在第一行上面了：挪到全文开头（和编辑器一致）
            let offset = 0;
            if extend {
                self.select_to(offset, cx)
            } else {
                self.move_to(offset, cx)
            }
            return;
        }
        let offset = self.index_at(point(from.x, px(y)));
        if extend {
            self.select_to(offset, cx)
        } else {
            self.move_to(offset, cx)
        }
    }

    fn move_home(&mut self, extend: bool, cx: &mut Context<Self>) {
        let (start, _) = self.line_bounds(self.cursor_offset());
        if extend {
            self.select_to(start, cx)
        } else {
            self.move_to(start, cx)
        }
    }

    fn move_end(&mut self, extend: bool, cx: &mut Context<Self>) {
        let (_, end) = self.line_bounds(self.cursor_offset());
        if extend {
            self.select_to(end, cx)
        } else {
            self.move_to(end, cx)
        }
    }

    // ---- 鼠标 ----

    fn index_for_mouse(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds.as_ref() else {
            return self.cursor_offset();
        };
        if self.content.is_empty() {
            return 0;
        }
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        let local = point(
            (position.x - bounds.left()).max(px(0.)),
            position.y - bounds.top(),
        );
        self.index_at(local)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        let index = self.index_for_mouse(event.position);
        if event.modifiers.shift {
            self.select_to(index, cx);
        } else if let Some(count) = Some(event.click_count) {
            match count {
                // 双击选整行、三击全选：用户在这种「一行一条」的框里最常干的两件事
                2 => {
                    let (start, end) = self.line_bounds(index);
                    self.move_to(start, cx);
                    self.select_to(end, cx);
                }
                3 => self.do_select_all(cx),
                _ => self.move_to(index, cx),
            }
        } else {
            self.move_to(index, cx);
        }
        cx.notify();
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
            let index = self.index_for_mouse(event.position);
            self.select_to(index, cx);
        }
    }

    fn on_right_click(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let index = self.index_for_mouse(event.position);
        if !self.selected_range.contains(&index) {
            self.move_to(index, cx);
        }
        crate::text_menu::open(
            crate::text_menu::Target::Area(cx.entity()),
            window,
            event.position,
            cx,
        );
    }

    // ---- 按键 ----

    /// 所有按键都在这里处理（不注册 action）。
    ///
    /// 走 action 会跟单行输入框那套全局绑定抢：`backspace` 之类在 `main.rs` 里是
    /// 无上下文绑到 `text_input::Backspace` 的，同一个键再绑一份就变成「看谁先」。
    /// 直接在 `key_down` 里从 keystroke 判断，一个键只做一次，也不影响别处。
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
        self.starts = line_starts(&self.content);
        self.selected_range = crate::undo::clamp_caret(snap.caret, len);
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
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

    /// 失焦：把输入法正在组的那一段取消掉（候选窗会跟着收掉，退格也就好使了）。
    fn on_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        crate::win32::cancel_ime_composition_in(window);
        cx.notify();
    }

    fn on_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;
        let extend = modifiers.shift;

        if modifiers.control && !modifiers.alt {
            match key {
                "z" => {
                    // Ctrl+Shift+Z 也当重做（很多编辑器的习惯）
                    if modifiers.shift {
                        self.do_redo(cx);
                    } else {
                        self.do_undo(cx);
                    }
                    cx.stop_propagation();
                }
                "y" => {
                    self.do_redo(cx);
                    cx.stop_propagation();
                }
                "c" => {
                    self.do_copy(cx);
                    cx.stop_propagation();
                }
                "x" => {
                    self.do_cut(window, cx);
                    cx.stop_propagation();
                }
                "v" => {
                    self.do_paste(window, cx);
                    cx.stop_propagation();
                }
                "a" => {
                    self.do_select_all(cx);
                    cx.stop_propagation();
                }
                // 下面这些是「全选/移动」的变体，别让窗口去干别的
                "left" | "right" | "up" | "down" | "home" | "end" => {}
                _ => return,
            }
            return;
        }
        if modifiers.control || modifiers.alt {
            return;
        }

        match key {
            "backspace" => {
                if self.selected_range.is_empty() {
                    let previous = self.previous_boundary(self.cursor_offset());
                    self.select_to(previous, cx);
                }
                self.replace_text_in_range(None, "", window, cx);
                cx.stop_propagation();
            }
            "delete" => {
                if self.selected_range.is_empty() {
                    let next = self.next_boundary(self.cursor_offset());
                    self.select_to(next, cx);
                }
                self.replace_text_in_range(None, "", window, cx);
                cx.stop_propagation();
            }
            "left" => {
                self.move_horizontal(false, extend, cx);
                cx.stop_propagation();
            }
            "right" => {
                self.move_horizontal(true, extend, cx);
                cx.stop_propagation();
            }
            "up" => {
                self.move_vertical(false, extend, cx);
                cx.stop_propagation();
            }
            "down" => {
                self.move_vertical(true, extend, cx);
                cx.stop_propagation();
            }
            "home" => {
                self.move_home(extend, cx);
                cx.stop_propagation();
            }
            "end" => {
                self.move_end(extend, cx);
                cx.stop_propagation();
            }
            // 换行 = 下一条
            "enter" | "return" => {
                self.replace_text_in_range(None, "\n", window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    // ---- UTF-16 换算（IME / 平台回调都用这个）----

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
}

impl TextArea {
    /// 把输入法候选窗摆到光标下（多行框里要按行算 y）。
    fn sync_ime_position(&self, window: &mut Window) {
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let line_h = px(self.line_height);
        let caret = self
            .selected_range
            .start
            .min(self.content.len());
        let position = position_for(&self.lines, &self.starts, line_h, caret);
        crate::win32::place_ime_in(
            window,
            f32::from(bounds.left() + position.x),
            f32::from(bounds.top() + position.y),
            f32::from(line_h),
        );
    }
}

impl EntityInputHandler for TextArea {
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

        // 输入法组字过程中的反复替换不记（撤销一次退一个拼音字母没有意义）
        if self.marked_range.is_none() {
            self.history
                .record(&self.content, self.selected_range.clone());
        }
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        // 行首表要跟着内容重算，否则折行 / 上下移动会拿旧的分行去算
        self.starts = line_starts(&self.content);
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
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
        self.starts = line_starts(&self.content);
        self.marked_range = (!new_text.is_empty()).then_some(range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        // 每变一次组字内容就把候选窗重新摆到光标下（搜狗会复用同一个候选窗）
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
        let line_h = px(self.line_height);
        let range = self.range_from_utf16(&range_utf16);
        let start = position_for(&self.lines, &self.starts, line_h, range.start);
        let end = position_for(&self.lines, &self.starts, line_h, range.end);
        Some(Bounds::from_corners(
            point(bounds.left() + start.x, bounds.top() + start.y),
            point(bounds.left() + end.x, bounds.top() + end.y + line_h),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let local = bounds.localize(&point)?;
        Some(self.offset_to_utf16(self.index_at(local)))
    }
}

/// 全文里每个逻辑行的起始字节（`\n` 后面的那个位置）。
fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(8);
    starts.push(0);
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

/// 全局偏移 → (第几个逻辑行, 行内偏移)
fn split_offset(starts: &[usize], offset: usize) -> (usize, usize) {
    match starts.binary_search(&offset) {
        Ok(index) => (index, 0),
        Err(next) => {
            let index = next.saturating_sub(1);
            (index, offset - starts.get(index).copied().unwrap_or(0))
        }
    }
}

/// 全局偏移 → 内容坐标系里的点。
fn position_for(
    lines: &[WrappedLine],
    starts: &[usize],
    line_h: Pixels,
    offset: usize,
) -> Point<Pixels> {
    let (index, local) = split_offset(starts, offset);
    let mut y = 0.0;
    for line in lines.iter().take(index) {
        y += f32::from(line.size(line_h).height);
    }
    let Some(line) = lines.get(index) else {
        return point(px(0.), px(y));
    };
    let local = local.min(line.len());
    let p = line
        .position_for_index(local, line_h)
        .unwrap_or_else(|| point(px(0.), px(0.)));
    point(p.x, px(y + f32::from(p.y)))
}

/// 内容坐标系里的一点 → 全局偏移（点在行下面就算那一行的末尾）。
fn index_for(
    lines: &[WrappedLine],
    starts: &[usize],
    line_h: Pixels,
    p: Point<Pixels>,
) -> usize {
    let mut y = 0.0;
    for (index, line) in lines.iter().enumerate() {
        let height = f32::from(line.size(line_h).height);
        if p.y < px(y + height) || index + 1 == lines.len() {
            let local = point(p.x, px((f32::from(p.y) - y).max(0.0)));
            let offset = line
                .closest_index_for_position(local, line_h)
                .unwrap_or_else(|offset| offset);
            return starts.get(index).copied().unwrap_or(0) + offset;
        }
        y += height;
    }
    0
}

// ---------------------------------------------------------------------------
// 绘制
// ---------------------------------------------------------------------------

struct AreaElement {
    input: Entity<TextArea>,
}

struct AreaPrepaint {
    lines: Vec<WrappedLine>,
    starts: Vec<usize>,
    placeholder: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
    rows: usize,
}

impl IntoElement for AreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AreaElement {
    type RequestLayoutState = ();
    type PrepaintState = AreaPrepaint;

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
        let (line_height, rows) = {
            let input = self.input.read(cx);
            (input.line_height, input.rows.max(input.min_rows))
        };
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(line_height * rows as f32).into();
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
        let font_size = input.font_size;
        let line_h = px(input.line_height);
        let marked = input.marked_range.clone();
        let placeholder = input.placeholder.clone();
        let wrap_w = bounds.size.width;

        let base = gpui::font(theme::font_family());
        let run = TextRun {
            len: content.len(),
            font: base,
            color: theme::text_primary().into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = marked.as_ref() {
            vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(theme::accent().into()),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: content.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(content.clone(), px(font_size), &runs, Some(wrap_w), None)
            .map(|lines| lines.into_iter().collect())
            .unwrap_or_default();
        let starts = line_starts(&content);

        // 空内容时画占位提示（灰字）
        let placeholder_line = if content.is_empty() {
            let run = TextRun {
                len: placeholder.len(),
                font: gpui::font(theme::font_family()),
                color: theme::ui_text_tertiary().into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            Some(
                window
                    .text_system()
                    .shape_line(placeholder, px(font_size), &[run], None),
            )
        } else {
            None
        };

        let mut rows = 0;
        for line in &lines {
            rows += line.wrap_boundaries().len() + 1;
        }
        rows = rows.max(1);

        // 选区：一行一行、一段一段地画（折行后一行可能横跨好几个视觉行）
        let mut selection = Vec::new();
        if !selected_range.is_empty() {
            let lh = f32::from(line_h);
            let mut y = 0.0;
            for (index, line) in lines.iter().enumerate() {
                let start = starts.get(index).copied().unwrap_or(0);
                let end = start + line.len();
                let a = selected_range.start.max(start);
                let b = selected_range.end.min(end);
                let height = f32::from(line.size(line_h).height);
                if a < b {
                    let p1 = line
                        .position_for_index(a - start, line_h)
                        .unwrap_or_else(|| point(px(0.), px(0.)));
                    let p2 = line
                        .position_for_index(b - start, line_h)
                        .unwrap_or_else(|| point(px(0.), px(0.)));
                    let row1 = (f32::from(p1.y) / lh).round() as usize;
                    let row2 = (f32::from(p2.y) / lh).round() as usize;
                    for row in row1..=row2 {
                        let x1 = if row == row1 { f32::from(p1.x) } else { 0.0 };
                        let x2 = if row == row2 {
                            f32::from(p2.x)
                        } else {
                            f32::from(wrap_w)
                        };
                        if x2 > x1 {
                            selection.push(fill(
                                Bounds::new(
                                    point(
                                        bounds.left() + px(x1),
                                        bounds.top() + px(y + row as f32 * lh),
                                    ),
                                    size(px(x2 - x1), line_h),
                                ),
                                theme::accent_soft(),
                            ));
                        }
                    }
                }
                y += height;
            }
        }

        // 光标
        let cursor_pos = position_for(&lines, &starts, line_h, cursor);
        let cursor_quad = fill(
            Bounds::new(
                point(bounds.left() + cursor_pos.x, bounds.top() + cursor_pos.y),
                size(px(1.5), line_h),
            ),
            theme::accent(),
        );

        AreaPrepaint {
            lines,
            starts,
            placeholder: placeholder_line,
            cursor: Some(cursor_quad),
            selection,
            rows,
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

        for quad in prepaint.selection.drain(..) {
            window.paint_quad(quad);
        }

        let line_h = px(self.input.read(cx).line_height);
        if let Some(placeholder) = prepaint.placeholder.take() {
            placeholder
                .paint(bounds.origin, line_h, window, cx)
                .ok();
        }

        let mut y = bounds.top();
        for line in &prepaint.lines {
            let height = line.size(line_h).height;
            let line_bounds = Bounds::new(point(bounds.left(), y), size(bounds.size.width, height));
            line.paint(
                point(bounds.left(), y),
                line_h,
                TextAlign::Left,
                Some(line_bounds),
                window,
                cx,
            )
            .ok();
            y += height;
        }

        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        let lines = std::mem::take(&mut prepaint.lines);
        let starts = std::mem::take(&mut prepaint.starts);
        let rows = prepaint.rows;
        let wrap_width = f32::from(bounds.size.width);
        self.input.update(cx, |input, _cx| {
            input.lines = lines;
            input.starts = starts;
            input.rows = rows;
            input.wrap_width = Some(wrap_width);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.blur_hooked {
            self.blur_hooked = true;
            let handle = self.focus_handle.clone();
            cx.on_blur(&handle, window, |this, window, cx| this.on_blur(window, cx))
                .detach();
        }
        div()
            .flex()
            .size_full()
            .overflow_hidden()
            .key_context("TextArea")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .text_size(px(self.font_size))
            .text_color(theme::text_primary())
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_click))
            .child(AreaElement { input: cx.entity() })
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
