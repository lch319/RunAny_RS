//! 一列 = 一个无边框窗口的根视图。
//!
//! 列高由 `popup.rs` 按屏幕「工作区」高度夹过，所以这一列可能装不下所有条目：
//!
//! - 滚动偏移存在 `MenuState.scroll[level]`（键盘、滚轮、滚动条、箭头共用一份）；
//! - 只渲染「当前可见区间」的行（虚拟化），上千条也只画十几行；
//! - 右侧滚动条可以**拖动**（热区 12px，视觉 4px），点滑轨也能跳；
//! - 顶部/底部还有隐藏条目时出现三角箭头，**点一下滚一页**。

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Focusable,
    div, px, rgba, svg, AnyElement, ClickEvent, Context, ElementId, Entity, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollDelta, ScrollWheelEvent,
    Subscription, Task, Window,
};

use crate::layout;
use crate::menu::{MenuEntry, MenuItem};
use crate::popup;
use crate::state::MenuState;
use crate::theme;

/// 拖动滚动条时的抓取状态。
struct Drag {
    /// 按下时指针落在滑块内的偏移（像素）
    grab: f32,
    /// 滑轨可移动长度 = viewport - 滑块高
    track: f32,
    /// 这一列最多能滚多少像素
    max_scroll: f32,
}

pub struct ColumnView {
    state: Entity<MenuState>,
    level: usize,
    /// 窗口句柄：尺寸变化时要重建圆角裁剪区域
    hwnd: Option<isize>,
    scale: f32,
    applied_size: Option<(u32, u32)>,
    /// 正在拖滚动条（拖动期间禁用行悬停，免得顺手把子菜单展开）
    drag: Option<Drag>,
    /// 跨列悬停的延迟切换（存着才不会在计时途中被丢掉）
    hover_task: Option<Task<()>>,
    /// 指针在**空白处**待命的代号：一旦有某一行真的被悬停，就 +1 作废上一次
    blank_gen: u64,
    /// 「停在空白处就收起深层」的待办
    blank_task: Option<Task<()>>,
    /// 指针此刻停在哪一行（None = 在留白 / 不在这一列上）
    hovered_row: Option<usize>,
    /// 最后一次收到鼠标移动的时刻：跨列切换要等指针「停下来」才算数
    /// 等待生效的那一行 + 指针「进入这一行」的时刻。
    ///
    /// 计时口径是**在这一行上停了多久**，不是「鼠标多久没动」——后者手一抖、
    /// 或指针在行内挪几像素就被重置，指针明明停在行上、子菜单却永远不出来。
    pending: Option<(usize, std::time::Instant)>,
    /// 状态变了要重绘，订阅必须存住
    _obs: Subscription,
}

impl ColumnView {
    pub fn new(state: Entity<MenuState>, level: usize, scale: f32, cx: &mut Context<Self>) -> Self {
        let _obs = cx.observe(&state, |_this, _state, cx| cx.notify());
        Self {
            state,
            level,
            hwnd: None,
            scale,
            applied_size: None,
            drag: None,
            hover_task: None,
            blank_gen: 0,
            blank_task: None,
            hovered_row: None,
            pending: None,
            _obs,
        }
    }

    /// 窗口创建后才知道 HWND，回调里补上
    pub fn attach_window(&mut self, hwnd: isize) {
        self.hwnd = Some(hwnd);
        self.applied_size = None;
    }

    /// 菜单重新显示时调用：丢掉上一轮遗留的拖动状态。
    pub fn cancel_drag(&mut self) {
        self.drag = None;
        self.hover_task = None;
        self.pending = None;
    }

    /// 这一列当前的 (可视高度, 内容高度, 滚动上限)。
    fn metrics(&self, card_height: f32, cx: &Context<Self>) -> (f32, f32, f32) {
        let viewport = layout::row_viewport(self.level, card_height);
        let snapshot = self.state.read(cx);
        // 多列并排时内容高度取最高的那一列（滚动条也按它算）
        let content = layout::content_height(snapshot.entries(self.level));
        (viewport, content, (content - viewport).max(0.0))
    }

    /// 多列菜单里，鼠标从这一列挪到另一列时，一路上每行都会触发悬停 ——
    /// 如果照旧「一碰就切」，子菜单刚打开就会被路过的行抢走。
    ///
    /// 判据是「**指针在这一行上停了多久**」（见 `pending`），不是「鼠标停了多久」：
    /// 只要指针还在同一行里，手抖、挪几像素都不重新计时；换到另一行才重新开始。
    /// 指针在卡片上移动（行与行之间的留白、列表下方的空白都算）。
    ///
    /// 多列菜单里子菜单展开后会盖住右边一列：光标停在空白处如果不收，
    /// 右边那几项就永远点不到（用户报的就是这个）。这里排个 260ms 的待办，
    /// 期间只要真有某一行被悬停（`blank_gen` 变了）就作废 —— 正常在行上移动
    /// 不会误收。
    fn blank_move(&mut self, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            return;
        }
        // 指针正停在某一行上：这是在点菜，不是「停在留白处」，别排待办。
        // 少了这一条，指针停在有子菜单的那行 260ms 就会被收起再展开 —— 抽搐。
        if self.hovered_row.is_some() {
            return;
        }
        let level = self.level;
        let state = self.state.clone();
        if state.read(cx).path.len() <= level + 1 {
            return;
        }
        let generation = self.blank_gen;
        let hwnd = self.hwnd.unwrap_or(0);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(260))
                .await;
            let _ = this.update(cx, |this, cx| {
                // 这期间碰过任何一行，或者现在指针就在某一行上 → 不是在留白处
                if this.blank_gen != generation || this.hovered_row.is_some() {
                    return;
                }
                // 光标已经跑到别的窗口（比如进了展开的子菜单）→ 更不能收，
                // 一收就把用户正指着的子菜单关掉了（「移进去就消失」就是这个）
                if !crate::win32::cursor_in_window(hwnd) {
                    return;
                }
                let changed = this.state.update(cx, |state, _| state.collapse_below(level));
                if changed {
                    crate::popup::sync(cx);
                    cx.notify();
                }
            });
        });
        self.blank_task = Some(task);
    }

    /// 指针离开这一行（`cancel_hover`）就把这一行的待定项作废。
    ///
    /// 这两个值**必须很小**：手感上「鼠标一停子菜单就该在」。以前是 120/200ms，
    /// 慢得像是程序卡了（用户报的「子菜单展开速度慢了非常多」）。真正防误触的是
    /// 「指针离开这一行就作废」那一条 —— 快速划过去时每行都撑不满 25ms，
    /// 一个都不会被打开，所以不需要用长延迟去堵。
    const HOVER_REST_NEAR: Duration = Duration::from_millis(25);
    const HOVER_REST_FAR: Duration = Duration::from_millis(70);

    fn hover(&mut self, idx: usize, cx: &mut Context<Self>) {
        // **任何一次**行悬停都作废「空白处收起」的待办。
        //
        // 之前只在 `apply_hover` 里作废，可是指针在同一行里挪动时 `hover()` 会
        // 提前 return（已经是当前项了），作废根本没发生 —— 于是 260ms 一到照样
        // 收起，收起后指针又在一个行上，立刻再展开：用户看到的就是「一直抽搐」。
        self.blank_gen = self.blank_gen.wrapping_add(1);
        self.hovered_row = Some(idx);
        // 拖滚动条时鼠标划过行不该改变高亮（否则会把沿途的子菜单一个个展开）
        if self.drag.is_some() {
            return;
        }
        // 指针还在同一行：计时继续走，不重置（这是「停在这一行上」的关键）
        if self.pending.map(|(row, _)| row) == Some(idx) {
            return;
        }
        // 这一行已经是当前选中的那一项、而且它该展开的也展开了：不用再排一次。
        // 不做这个判断的话，指针在一行里挪一下就排一个 120ms 的定时器，
        // 每 120ms 白跑一次 `apply_hover`。
        {
            let snapshot = self.state.read(cx);
            let level = self.level;
            if snapshot.path.get(level) == Some(&idx)
                && (!snapshot.has_children(level, idx) || snapshot.path.len() > level + 1)
            {
                self.pending = None;
                return;
            }
        }
        let cross = self.crosses_open_column(idx, cx);
        if std::env::var_os("GPUI_MENU_DEBUG_HOVER").is_some() {
            eprintln!("[hover] level={} idx={} cross={}", self.level, idx, cross);
        }
        self.pending = Some((idx, std::time::Instant::now()));
        let rest = if cross {
            Self::HOVER_REST_FAR
        } else {
            Self::HOVER_REST_NEAR
        };
        // 换行就把上一行的计时任务顶掉（drop 掉即取消），
        // 后面那个 `pending` 判断再兜一层，双保险。
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(rest).await;
            let _ = this.update(cx, |this, cx| {
                if this.pending.map(|(row, _)| row) == Some(idx) {
                    this.pending = None;
                    this.apply_hover(idx, cx);
                }
            });
        }));
    }

    /// 这一列正开着子菜单，而鼠标从「当前选中项那一列」挪到了另一列。
    fn crosses_open_column(&self, idx: usize, cx: &Context<Self>) -> bool {
        let snapshot = self.state.read(cx);
        let level = self.level;
        if snapshot.columns() <= level + 1 {
            return false;
        }
        let entries = snapshot.entries(level);
        if idx >= entries.len() {
            return false;
        }
        let current = snapshot.path.get(level).copied().unwrap_or(0);
        layout::group_of(entries, current).0 != layout::group_of(entries, idx).0
    }

    /// 鼠标离开**某一行**：只作废这一行的待定切换（不然走了之后子菜单还会自己变）。
    ///
    /// 不能一刀切地把 `hover_task` 丢掉：GPUI 在「A 行 → B 行」这种同窗口换行时，
    /// 两个回调的先后顺序不保证，B 的 `on_hover(true)` 有可能先到；那时再被 A 的
    /// `false` 清掉，B 就永远等不到生效 —— 用户看到的就是「指针明明停在右列那一行
    /// 上，子菜单死活不出来」。
    fn cancel_hover(&mut self, idx: usize, _cx: &mut Context<Self>) {
        if self.hovered_row == Some(idx) {
            self.hovered_row = None;
        }
        if self.pending.map(|(row, _)| row) != Some(idx) {
            return;
        }
        if std::env::var_os("GPUI_MENU_DEBUG_HOVER").is_some() {
            eprintln!("[hover] level={} 指针离开 idx={}", self.level, idx);
        }
        self.pending = None;
        self.hover_task = None;
    }

    fn apply_hover(&mut self, idx: usize, cx: &mut Context<Self>) {
        let level = self.level;
        // 延迟期间菜单可能被筛短了（用户还在打字），越界的行直接忽略
        if self.state.read(cx).entries(level).get(idx).is_none() {
            return;
        }
        if std::env::var_os("GPUI_MENU_DEBUG_HOVER").is_some() {
            eprintln!("[hover] level={} apply idx={}", self.level, idx);
        }
        let state = self.state.clone();
        let changed = state.update(cx, |state, cx| {
            let changed = state.hover(level, idx);
            if changed {
                cx.notify();
            }
            changed
        });
        if changed {
            popup::sync(cx);
        }
    }

    /// 点父项只展开；点叶子执行完就关菜单。
    fn click(&mut self, idx: usize, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let level = self.level;
        let fired = state.update(cx, |state, cx| {
            let fired = state.click(level, idx);
            cx.notify();
            fired
        });
        match fired {
            Some((id, label, actions)) => popup::finish(id, label, actions, cx),
            None => popup::sync(cx),
        }
    }

    /// 右键某一项：弹「编辑 / 隐藏 / 删除」这几条快捷操作。
    fn context(&mut self, idx: usize, cx: &mut Context<Self>) {
        let id = self
            .state
            .read(cx)
            .item_at(self.level, idx)
            .map(|item| item.id.to_string());
        if let Some(id) = id {
            popup::open_item_context(cx, &id);
        }
    }

    /// 把这一列滚到 `target`（像素），返回是否真的动了。
    fn scroll_to(&mut self, target: f32, cx: &mut Context<Self>) -> bool {
        let level = self.level;
        let state = self.state.clone();
        let changed = state.update(cx, |state, cx| {
            let before = state.scroll_of(level);
            if (before - target).abs() < 0.01 {
                return false;
            }
            state.set_scroll(level, target);
            cx.notify();
            true
        });
        if changed {
            popup::sync(cx);
        }
        changed
    }

    /// 滚轮：一次滚 `delta` 像素，夹在 [0, max_scroll] 里。
    fn wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let card_height = f32::from(window.viewport_size().height);
        let delta = match event.delta {
            ScrollDelta::Pixels(point) => f32::from(point.y),
            ScrollDelta::Lines(point) => point.y * layout::row_height() * 3.0,
        };
        if delta.abs() < 0.01 {
            return;
        }
        let state = self.state.clone();
        let level = self.level;
        let changed = state.update(cx, |state, cx| {
            let changed = state.scroll_by(level, -delta, card_height);
            if changed {
                cx.notify();
            }
            changed
        });
        if changed {
            popup::sync(cx);
        }
    }

    /// 点三角箭头：滚一页（留一行重叠，不至于看丢位置）。
    fn page(&mut self, direction: f32, window: &mut Window, cx: &mut Context<Self>) {
        let card_height = f32::from(window.viewport_size().height);
        let (viewport, _, max_scroll) = self.metrics(card_height, cx);
        if max_scroll <= 0.5 {
            return;
        }
        let step = (viewport - layout::row_height()).max(layout::row_height());
        let state = self.state.clone();
        let level = self.level;
        let changed = state.update(cx, |state, cx| {
            let changed = state.scroll_by(level, direction * step, card_height);
            if changed {
                cx.notify();
            }
            changed
        });
        if changed {
            popup::sync(cx);
        }
    }

    /// 在滚动条上按下：命中滑块就开始拖，命中滑轨就把滑块中心跳过来再拖。
    fn scrollbar_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let card_height = f32::from(window.viewport_size().height);
        let (viewport, content, max_scroll) = self.metrics(card_height, cx);
        let scroll = self.state.read(cx).scroll_of(self.level);
        let Some((thumb_top, thumb_h)) =
            layout::scroll_thumb(viewport, content, scroll, max_scroll)
        else {
            return;
        };

        // 指针在行区域里的 y（0 = 行区域顶部）
        let y = f32::from(event.position.y) - layout::row_top(self.level);
        let inside_thumb = y >= thumb_top - 0.5 && y <= thumb_top + thumb_h + 0.5;
        let grab = if inside_thumb {
            y - thumb_top
        } else {
            thumb_h / 2.0 // 点滑轨：让滑块中心跟过来
        };
        let track = (viewport - thumb_h).max(1.0);
        self.drag = Some(Drag {
            grab,
            track,
            max_scroll,
        });
        if !inside_thumb {
            self.drag_to(y, cx);
        }
    }

    /// 拖动中：按「滑轨位移 → 内容位移」的比例换算滚动位置。
    fn drag_to(&mut self, y: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        let ratio = ((y - drag.grab) / drag.track).clamp(0.0, 1.0);
        let target = ratio * drag.max_scroll;
        self.scroll_to(target, cx);
    }

    fn drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.drag.is_none() {
            return;
        }
        // 在窗口外松开时不一定收得到 mouse_up，这里用「左键是否还按着」兜底，
        // 否则拖动状态会一直挂着，把后续的悬停高亮全挡掉。
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let y = f32::from(event.position.y) - layout::row_top(self.level);
        self.drag_to(y, cx);
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify(); // 滑块颜色从「按下」恢复
        }
    }
}

impl Render for ColumnView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 窗口尺寸变了（切换父项会让列变高/变矮）就重建圆角裁剪区域，
        // 否则区域还按旧尺寸，会把卡片截掉一块。
        let card_height = f32::from(window.viewport_size().height);
        if let Some(hwnd) = self.hwnd {
            let viewport = window.viewport_size();
            let key = (
                f32::from(viewport.width) as u32,
                f32::from(viewport.height) as u32,
            );
            if std::env::var_os("GPUI_MENU_DEBUG_SIZE").is_some() {
                let client = crate::win32::client_size(hwnd);
                println!(
                    "[size] #{} viewport={}x{} client={:?}",
                    self.level,
                    key.0,
                    key.1,
                    client.map(|(w, h)| (w as f32 / self.scale, h as f32 / self.scale))
                );
            }
            if self.applied_size != Some(key) {
                crate::win32::set_rounded_corners(hwnd, self.scale);
                self.applied_size = Some(key);
            }
        }

        let state = self.state.clone();
        let level = self.level;
        // 搜索框先建好：它要 `&mut Context`，而下面 `snapshot` 一直借着 `cx`
        let search_elements: Vec<AnyElement> = if level == 0 {
            let mut elements = Vec::new();
            if let Some(top) = layout::search_top(0) {
                elements.push(search_box(top, false, cx));
            }
            if let Some(bottom) = layout::search_bottom(0) {
                elements.push(search_box(bottom, true, cx));
            }
            elements
        } else {
            Vec::new()
        };
        let snapshot = state.read(cx);
        let entries = snapshot.entries(level);
        let selected = snapshot.path.get(level).copied();

        // 能显示多少行、能滚多远
        let viewport = layout::row_viewport(level, card_height);
        let content = layout::content_height(entries);
        let max_scroll = (content - viewport).max(0.0);
        let scroll = snapshot.scroll_of(level).clamp(0.0, max_scroll);
        let scrollable = max_scroll > 0.5;
        let has_more_above = scroll > 0.5;
        let has_more_below = scroll < max_scroll - 0.5;
        let thumb = layout::scroll_thumb(viewport, content, scroll, max_scroll);
        let dragging = self.drag.is_some();

        // 每一段（多列符切出来的列）各画一条竖着的堆叠，共用一个滚动偏移：
        // 滚轮 / 滚动条滚的是整张卡片，几列一起上下走（和系统多列菜单一个感觉）。
        let groups = layout::groups(entries);
        // 每段自己的宽度（固定模式全都一样；自动模式按这一段的内容算），
        // 必须和 `popup::sync` 摆窗口用的是同一套口径，否则行会错位
        let widths = {
            let family = crate::theme::font_family();
            let system: &gpui::TextSystem = window.text_system();
            layout::column_widths_with(entries, &|text, size| {
                layout::measure_text(system, &family, size, text)
            })
        };
        let mut stacks: Vec<AnyElement> = Vec::with_capacity(groups.len());
        let mut left = 0.0_f32;
        for (group, &(start, end)) in groups.iter().enumerate() {
            let panel = widths.get(group).copied().unwrap_or_else(layout::panel_width);
            let slice = &entries[start..end];
            // 虚拟化：只渲染可见区间，n 再大也只画十几行
            let (first, last) = layout::visible_rows_in(slice, scroll, viewport);
            let top_of_first = layout::row_offset(slice, first);
            let last_column = group + 1 == groups.len();

            // 行的可用宽度：面板宽 - 滚动条热区（只有最后一列要扣）- 行的左右留白
            let stack_w = panel
                - if last_column && scrollable {
                    layout::SCROLLBAR_HIT_W
                } else {
                    0.0
                };
            let row_w = (stack_w - 8.0 - 16.0).max(40.0);

            let mut stack = div()
                .absolute()
                .left(px(left))
                .w(px(stack_w))
                .top(px(top_of_first - scroll))
                .flex()
                .flex_col();
            for idx in first..last {
                let entry = &slice[idx];
                let row_idx = start + idx;
                stack = stack.child(match entry {
                    MenuEntry::Separator => separator(),
                    MenuEntry::ColumnBreak => div().into_any_element(),
                    MenuEntry::Item(item) => row(
                        level,
                        row_idx,
                        item,
                        selected == Some(row_idx),
                        row_w,
                        window,
                        cx,
                    ),
                });
            }
            stacks.push(stack.into_any_element());

            // 列与列之间那条细线（最后一列不画）：画在「这一段右边缘」和
            // 「下一段左边缘」的中间
            let next_left = left + panel + layout::COLUMN_GAP;
            if !last_column {
                stacks.push(
                    div()
                        .absolute()
                        .left(px(next_left - layout::COLUMN_GAP / 2.0))
                        .top(px(layout::row_top(level) - layout::chrome() + 4.0))
                        .w(px(1.))
                        .h(px((card_height - layout::row_top(level) - layout::row_bottom_edge(level) - 8.0)
                            .max(0.0)))
                        .bg(theme::border())
                        .into_any_element(),
                );
            }
            left = next_left;
        }

        // 卡片：白底 + 1px 描边（圆角交给窗口裁剪，见 win32::set_rounded_corners）。
        // 描边可以在设置里关掉 / 换色（见 `MenuSettings::menu_border`）；
        // 关掉时宽度也要跟着变 0，否则四周会空出一条没人画的边 ——
        // 这里的 1px 和 `layout::chrome()` 是同一个数。
        let mut card = div()
            .id(ElementId::Name(format!("menu-card-{level}").into()))
            .relative()
            .size_full()
            .bg(theme::surface_elevated())
            .when(layout::chrome() > 0.0, |card| {
                card.border_1().border_color(theme::border())
            })
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                this.wheel(event, window, cx);
            }))
            // 拖动滚动条时，指针几乎一定已经离开那条 12px 的滑轨了，
            // 所以 move / up 挂在铺满窗口的卡片上（父 hitbox 也在命中链里）。
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                // 指针在卡片上动（行与行之间的留白也算）
                this.drag_move(event, cx);
                // 停在空白处 → 把这一列往下的展开收掉，别挡着右边一列
                this.blank_move(cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    this.end_drag(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| this.end_drag(cx)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(px(layout::row_top(level) - layout::chrome()))
                    .h(px(viewport))
                    .overflow_hidden()
                    .children(stacks),
            );

        // 搜索框（只在第一列、且设置里开了它的时候）：顶一条或底一条
        let query = snapshot.query().to_string();
        card = card.children(search_elements);

        // 什么都没有（搜出来是空的）：给一句提示，别留一张空白卡片
        if entries.is_empty() && !query.trim().is_empty() {
            card = card.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(px(layout::row_top(level)))
                    .h(px(layout::row_height()))
                    .flex()
                    .items_center()
                    .px(px(12.0))
                    .text_size(theme::fs(11.0))
                    .text_color(theme::text_tertiary())
                    .child(crate::i18n::t("没有匹配的项")),
            );
        }

        // 顶部 / 底部还有隐藏条目时，画个可点的三角箭头
        if has_more_above {
            card = card.child(arrow(level, true, cx));
        }
        if has_more_below {
            card = card.child(arrow(level, false, cx));
        }

        // 滚动条：滑轨 12px 热区 + 4px 滑块；放在最后画，才盖得住箭头右端。
        // `.occlude()` 很关键：GPUI 把「命中的每个 hitbox」都当目标，
        // 不挡住的话点滑块会顺带点中底下的菜单行（直接把菜单项执行了）。
        // 同理，遮挡之后卡片的 move/up 收不到事件了，所以滑轨自己也要挂一份。
        if let Some((thumb_top, thumb_h)) = thumb {
            card = card.child(
                div()
                    .id(ElementId::Name(format!("menu-scroll-{level}").into()))
                    .occlude()
                    .absolute()
                    .right(px(0.0))
                    .top(px(layout::row_top(level)))
                    .w(px(layout::SCROLLBAR_HIT_W))
                    .h(px(viewport))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.scrollbar_down(event, window, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                        this.drag_move(event, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, cx| this.end_drag(cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, cx| this.end_drag(cx)),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                        this.wheel(event, window, cx);
                    }))
                    .child(
                        div()
                            .absolute()
                            .right(px(2.0))
                            .top(px(thumb_top))
                            .w(px(layout::SCROLLBAR_W))
                            .h(px(thumb_h))
                            .rounded(px(layout::SCROLLBAR_W / 2.0))
                            .bg(if dragging {
                                theme::accent_glow()
                            } else {
                                theme::scrollbar()
                            }),
                    ),
            );
        }
        card
    }
}

/// 第一列顶上 / 底下那条搜索框。
///
/// 是一个**真的输入框**（`text_input::TextInput` 实体）：
/// - 文字/中文输入法都由它自己接，输入一变就写进 `MenuState::query` 重新筛；
/// - 点一下就能定位光标（以前那版是个「长得像输入框」的 div，点了没反应、也打不了字）。
fn search_box(offset: f32, at_bottom: bool, cx: &mut Context<ColumnView>) -> AnyElement {
    let input = cx.global::<crate::popup::MenuHost>().search.clone();
    let focus = input.read(cx).focus_handle(cx);
    let mut boxed = div()
        .absolute()
        .left(px(6.0))
        .right(px(6.0))
        .h(px(layout::SEARCH_H - 6.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .rounded(px(5.0))
        .bg(theme::surface_sunken())
        .border_1()
        .border_color(theme::border_strong());
    boxed = if at_bottom {
        boxed.bottom(px(offset))
    } else {
        boxed.top(px(offset))
    };
    boxed
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .child(input),
        )
        // 点空白处也要把焦点给输入框（输入框自己只挪光标、不抢焦点）
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_this, _event: &MouseDownEvent, window, cx| {
                window.focus(&focus);
                cx.notify();
            }),
        )
        .into_any_element()
}

/// 顶部/底部的三角箭头（压在卡片边缘，只占 12px，不挡文字）。
/// 点一下滚一页。`.occlude()` 让箭头吃掉点击，不会穿透到下面那一行。
fn arrow(level: usize, up: bool, cx: &Context<ColumnView>) -> AnyElement {
    let name = if up {
        "menu-arrow-up"
    } else {
        "menu-arrow-down"
    };
    let base = div()
        .id(ElementId::Name(format!("{name}-{level}").into()))
        .occlude()
        .absolute()
        .left(px(0.0))
        .right(px(0.0))
        .h(px(layout::ARROW_H))
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::surface_elevated())
        .text_color(theme::text_secondary())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.page(if up { -1.0 } else { 1.0 }, window, cx);
        }));
    let base = if up {
        base.top(px(layout::chrome()))
    } else {
        base.bottom(px(layout::chrome()))
    };
    // 用内嵌 svg 而不是 ▲ / ▼ 字符：字号小的时候字符会被字体的行高截掉半截，
    // 在 2 倍缩放的屏幕上尤其明显（看着像一小块被裁掉的灰点）。
    base.child(
        svg()
            .path(if up {
                "icons/arrow-up.svg"
            } else {
                "icons/arrow-down.svg"
            })
            .w(px(12.0))
            .h(px(12.0))
            .flex_none()
            .text_color(theme::text_secondary()),
    )
    .into_any_element()
}

fn row(
    level: usize,
    idx: usize,
    item: &MenuItem,
    highlighted: bool,
    row_w: f32,
    window: &Window,
    cx: &Context<ColumnView>,
) -> AnyElement {
    // 悬停提示按 id 找设置，闭包里得要一份自己的
    let tip_id = item.id.to_string();
    let tip_id_nudged = tip_id.clone();
    let mut row = div()
        .id(ElementId::Name(format!("menu-row-{level}-{idx}").into()))
        .flex()
        .items_center()
        .gap_2()
        .h(px(layout::row_height()))
        .mx(px(4.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .bg(if highlighted {
            theme::row_selected()
        } else {
            rgba(0x00000000)
        })
        .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
            if *hovered {
                this.hover(idx, cx);
                // 停一会儿弹一张说明卡片（内容 / 延迟都在设置里，可以整体关掉）
                crate::tooltip::schedule(cx, &tip_id);
            } else {
                this.cancel_hover(idx, cx);
                crate::tooltip::cancel(cx, &tip_id);
            }
        }))
        // enter 事件偶尔会漏（比如菜单正好在光标底下弹出来）：
        // 鼠标在行里动了就补一次，`nudge` 只在「什么都没在等」时才真的排一次
        .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _window, cx| {
            // 覆盖「enter 漏了」的情况：指针在行内动，说明它确实在这一行上
            this.hover(idx, cx);
            crate::tooltip::nudge(cx, &tip_id_nudged);
        }))
        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
            this.click(idx, cx);
        }))
        // 右键：弹这一项的快捷操作（编辑 / 隐藏 / 删除 …）
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                this.context(idx, cx);
            }),
        );

    let badge_size = layout::badge_size();
    if let Some(badge) = &item.badge {
        // 图标位：字符/emoji 直接画（带个浅底小方块），`.svg` / `.ico` / `.png` 这类当图片画
        let image = crate::icon::is_image(badge);
        row = row.child(
            div()
                .w(px(badge_size))
                .h(px(badge_size))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .when(!image, |style| {
                    style.rounded(px(4.0)).bg(theme::badge_fill())
                })
                .child(crate::icon::render(
                    badge,
                    badge_size,
                    theme::text_secondary(),
                )),
        );
    }

    // 加速键和名称挨在一起（用文字之间的间距，不再吃行的大 gap）
    const ACCEL_GAP: f32 = 3.0;
    // 宽度跟着字号走：字号调大时一个字母也会变宽
    let accel_size = f32::from(theme::fs(10.5));
    let accel_w = accel_size + 4.0;
    let mut lead = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(ACCEL_GAP));

    if let Some(accel) = &item.accel {
        // 加速键：紧贴名称左侧，底下一道细线（系统菜单的老写法），颜色可单独配
        lead = lead.child(
            div()
                .flex_none()
                .w(px(accel_w))
                .flex()
                .items_center()
                .justify_center()
                .pb(px(1.0))
                .border_b_1()
                .border_color(theme::accel_underline())
                .text_size(px(accel_size))
                .font_family(theme::font_family())
                .text_color(theme::accel_text())
                .child(accel.to_string()),
        );
    }

    // 标签能用多宽：扣掉图标位 / 加速键 / 快捷键 / 箭头占的地方（gap 是 8）
    //
    // 备注不在这里画：它只是给自己看的备忘，挤在行尾反而把名称压短了
    // （现在进了悬停提示，见 tooltip）。
    const GAP: f32 = 8.0;
    let family = theme::font_family();
    let label_size = f32::from(theme::fs(13.0));
    let mut avail = row_w;
    if item.badge.is_some() {
        avail -= badge_size + GAP;
    }
    if item.accel.is_some() {
        avail -= accel_w + ACCEL_GAP;
    }
    if let Some(shortcut) = &item.shortcut {
        avail -= text_width(window, &family, f32::from(theme::fs(11.5)), shortcut) + GAP;
    }
    if item.has_children() {
        avail -= text_width(window, &family, label_size, "›") + GAP;
    }
    let label = fit_label(window, &family, label_size, &item.label, avail);

    lead = lead.child(
        // 文字比列宽长就省略号截断
        div()
            .flex_1()
            .min_w_0()
            .whitespace_nowrap()
            .text_size(px(label_size))
            .font_family(family.clone())
            .text_color(theme::text_primary())
            .child(label),
    );

    row = row.child(lead);

    if let Some(shortcut) = &item.shortcut {
        row = row.child(
            div()
                .flex_none()
                .text_size(theme::fs(11.5))
                .font_family(theme::font_family())
                .text_color(theme::text_secondary())
                .child(shortcut.clone()),
        );
    }

    if item.has_children() {
        row = row.child(
            div()
                .flex_none()
                .text_size(theme::fs(13.0))
                .font_family(theme::font_family())
                .text_color(theme::text_secondary())
                .child("›"),
        );
    }

    row.into_any_element()
}

/// 量一段文字在当前字体下有多宽。
///
/// 逐字累加 `advance`（而不是靠 `text_ellipsis()`）：gpui 的文本省略号只在
/// 「测量时就知道确切宽度」的情况下才生效，而 flex 里量的是内容宽度，
/// 结果就是文字被卡片硬裁掉、没有省略号。菜单的行宽是固定的，自己算更靠谱。
fn text_width(window: &Window, family: &gpui::SharedString, size: f32, text: &str) -> f32 {
    let text_system = window.text_system();
    let font = gpui::Font {
        family: family.clone(),
        features: Default::default(),
        fallbacks: None,
        weight: gpui::FontWeight::NORMAL,
        style: gpui::FontStyle::Normal,
    };
    let font_id = text_system.resolve_font(&font);
    text.chars()
        .map(|ch| {
            text_system
                .advance(font_id, px(size), ch)
                .map(|advance| f32::from(advance.width))
                .unwrap_or(0.0)
        })
        .sum()
}

/// 放不下就截断并补一个「…」。
fn fit_label(
    window: &Window,
    family: &gpui::SharedString,
    size: f32,
    label: &str,
    max_width: f32,
) -> String {
    if max_width <= 0.0 {
        return "…".to_string();
    }
    if text_width(window, family, size, label) <= max_width {
        return label.to_string();
    }
    let mut budget = max_width - text_width(window, family, size, "…");
    let mut out = String::new();
    for ch in label.chars() {
        let width = text_width(window, family, size, &ch.to_string());
        if width > budget {
            break;
        }
        budget -= width;
        out.push(ch);
    }
    out.push('…');
    out
}

fn separator() -> AnyElement {
    div()
        .h(px(layout::separator_height()))
        .flex()
        .items_center()
        .px(px(12.0))
        .child(div().h(px(1.0)).w_full().bg(theme::separator()))
        .into_any_element()
}
