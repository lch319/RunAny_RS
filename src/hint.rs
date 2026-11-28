//! 热字符串的提示小卡片。
//!
//! 敲到第 N 个字符时，在鼠标旁边浮一张小卡片，列出「以这几个字开头的短语」：
//!
//! ```text
//! huiz  →  你好，世界
//!          D:\工具\打招呼.exe
//! ```
//!
//! ## 几个必须守住的约束
//!
//! - **不能抢焦点**：用户正在别的程序里打字，焦点一跑就全乱了。
//!   所以窗口是 `focus: false` + `SW_SHOWNOACTIVATE` + `WS_EX_NOACTIVATE`。
//! - **不能挡鼠标**：默认整扇窗鼠标穿透（`win32::make_click_through`），
//!   点下去直接穿过卡片落到下面的程序里；只有用户打开「提示可以用鼠标点」时
//!   才摘掉穿透 —— 那时候鼠标悬停不消失、点某一条就直接执行那一条。
//! - **贴哪儿能选**：鼠标旁边（默认）或文字插入点旁边（`HintAnchor::Caret`），
//!   打字时眼睛在光标那儿，贴过去更顺手。
//! - **不能拖慢输入**：显示/隐藏都是 `SetWindowPos` 级别的小动作，
//!   窗口一启动就建好藏起来，之后只是摆位置 + 刷内容。
//!
//! 卡片自己会消失：`hint_ms` 到了就藏起来，并且告诉 `hotstring` 先安静一会儿
//! （用户还在打同一个词，每敲一个字弹一次太吵）。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, AnyWindowHandle, App, AppContext, Bounds, ClickEvent, ElementId, IntoElement, Render,
    Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};

use crate::hotstring::HintLine;
use crate::settings;
use crate::theme;
use crate::win32;

const PAD: f32 = 8.0;
const GAP: f32 = 1.0;
const LINE_H: f32 = 17.0;
const DIM_H: f32 = 15.0;
const MIN_W: f32 = 140.0;
const MAX_W: f32 = 380.0;

thread_local! {
    static LINES: RefCell<Vec<HintLine>> = const { RefCell::new(Vec::new()) };
    static SIZE: RefCell<(f32, f32)> = const { RefCell::new((MIN_W, 40.0)) };
    /// 鼠标正停在卡片上：这时候不许自动消失（用户可能正要选一条）
    static HOVERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// 「鼠标还在不在卡片上」的轮询跑着没有（只跑一份）
    static WATCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct HintCard;

impl Render for HintCard {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // 尺寸用「算好的那个数」，不用 size_full()：窗口是 SetWindowPos 改的，
        // 这一帧 gpui 手里还是旧 bounds，按旧尺寸排版会把文字排到窗口外面。
        let (width, height) = SIZE.with(|size| *size.borrow());
        let lines = LINES.with(|lines| lines.borrow().clone());
        let clickable = settings::hotstring_settings().hint_clickable;
        let mut card = div()
            .id("hint-card")
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .gap(px(GAP))
            .px(px(PAD))
            .py(px(PAD))
            .rounded(px(6.))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border_strong())
            .font_family(theme::font_family())
            .text_color(theme::text_primary())
            .overflow_hidden()
            // 鼠标停上来就先别消失（可能正要挑一条），移开再重新计时
            .on_hover(cx.listener(|_this, hovered: &bool, _window, cx| {
                on_hover_changed(*hovered, cx);
            }));
        for (index, line) in lines.into_iter().enumerate() {
            let can_click = clickable && !line.id.is_empty();
            let mut row = div()
                .id(ElementId::Name(format!("hint-line-{index}").into()))
                .w_full()
                .min_w_0()
                .h(px(if line.dim { DIM_H } else { LINE_H }))
                .flex_none()
                .flex()
                .items_center()
                .px(px(2.))
                .rounded(px(3.))
                .text_size(px(if line.dim { 10.5 } else { 11.5 }))
                .text_color(if line.dim {
                    theme::ui_text_tertiary()
                } else {
                    theme::text_primary()
                })
                .truncate()
                .child(line.text);
            if can_click {
                let id = line.id.clone();
                row = row
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(move |_this, _: &ClickEvent, _window, _cx| {
                        crate::hotstring::run_line(&id);
                    }));
            }
            card = card.child(row);
        }
        card
    }
}

/// 鼠标进出卡片：进来就取消超时；离开之后**一秒**再收（不是按 `hint_ms`）。
///
/// 离开时给的这一秒是固定值：鼠标一移开就消失太突然（用户可能正想点某一行），
/// 拖太久又挡视线。`hint_ms` 管的是「没人碰它时多久自动消失」，两个不是一回事。
const LEAVE_HIDE_MS: u64 = 1000;

fn on_hover_changed(hovered: bool, cx: &mut gpui::Context<HintCard>) {
    if hovered {
        HOVERED.with(|flag| flag.set(true));
        // 编号 +1 让已经排着的那次「到点收起」失效（醒来发现号变了，什么都不做）
        crate::hotstring::bump_hint_generation();
    } else {
        HOVERED.with(|flag| flag.set(false));
        let visible = HOST.with(|host| host.borrow().visible);
        if visible {
            arm_timer_for(cx, LEAVE_HIDE_MS);
        }
    }
}

struct Host {
    handle: Option<AnyWindowHandle>,
    hwnd: Option<isize>,
    visible: bool,
    scale: f32,
    building: bool,
}

thread_local! {
    static HOST: Rc<RefCell<Host>> = Rc::new(RefCell::new(Host {
        handle: None,
        hwnd: None,
        visible: false,
        scale: 1.0,
        building: false,
    }));
}

/// 启动时把窗口建好藏起来（和提示卡、菜单池一样，别等到要用才建）。
pub fn install(cx: &mut App) {
    if !cfg!(windows) {
        return;
    }
    let scale = crate::cursor::scale_factor(cx);
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(700))
            .await;
        let _ = cx.update(move |app| ensure_window(app, scale));
    })
    .detach();
}

fn ensure_window(cx: &mut App, scale: f32) -> bool {
    if HOST.with(|host| host.borrow().handle.is_some()) {
        return true;
    }
    if HOST.with(|host| host.borrow().building) {
        return false;
    }
    HOST.with(|host| host.borrow_mut().building = true);

    let bounds = Bounds::new(gpui::point(px(0.), px(0.)), gpui::size(px(MIN_W), px(40.)));
    let before = win32::zed_window_handles();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            window_decorations: Some(WindowDecorations::Client),
            kind: WindowKind::PopUp,
            focus: false,
            show: false,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        |_window, cx| cx.new(|_cx| HintCard),
    );
    HOST.with(|host| host.borrow_mut().building = false);
    let Ok(handle) = opened else {
        return false;
    };
    let hwnd = win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        win32::set_rounded_corners(hwnd, scale);
        // 鼠标穿透：卡片就贴在鼠标旁边，挡住点击比不提示还烦
        win32::make_click_through(hwnd);
        // 收成卡片真实大小：GPUI 建窗用的是 `CW_USEDEFAULT`，不收拾的话它是一张
        // 整屏大小的交换链（8 MB 上下）常驻着 —— 这张卡一年也用不了几次。
        win32::resize_window(hwnd, MIN_W, 40.0, scale);
        win32::hide_window(hwnd);
        // 先透明着画一帧，把渲染器点着（第一次提示才不用等）
        win32::set_window_opacity(hwnd, 0.0);
        win32::show_window(hwnd);
        let _ = handle.update(cx, |_, window, _| window.refresh());
        win32::hide_window(hwnd);
        win32::set_window_opacity(hwnd, 1.0);
    }
    HOST.with(|host| {
        let mut host = host.borrow_mut();
        host.handle = Some(handle.into());
        host.hwnd = hwnd;
        host.scale = scale;
    });
    true
}

/// 显示（或者用新内容刷新）提示卡片；`lines` 空 = 收起来。
thread_local! {
    /// 紧凑模式：只有一行短文字（快捷栏的悬停提示），
    /// 不要最小宽度、也不要那堆留白，并且摆在光标**上面**
    static COMPACT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// 悬停提示的「这一轮」代号：鼠标一移开就 +1，没排到队的旧任务自然作废
    static TIP_TICKET: std::sync::atomic::AtomicU64 = const { std::sync::atomic::AtomicU64::new(0) };
}

/// 带延迟的悬停提示（鼠标停够 `delay_ms` 才弹，中途移开就不弹）。
pub fn show_tip_delayed(cx: &mut App, text: &str, delay_ms: u64) {
    let ticket = TIP_TICKET.with(|slot| slot.fetch_add(1, std::sync::atomic::Ordering::SeqCst)) + 1;
    let text = text.to_string();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(delay_ms))
            .await;
        // 这期间光标已经移开了（ticket 变了）：不弹
        if TIP_TICKET.with(|slot| slot.load(std::sync::atomic::Ordering::SeqCst)) != ticket {
            return;
        }
        let _ = cx.update(|cx| show_tip(cx, &text));
    })
    .detach();
}

/// 快捷栏那种「只有一个词」的悬停提示：贴光标上方、宽度只裹住文字。
pub fn show_tip(cx: &mut App, text: &str) {
    COMPACT.with(|slot| slot.set(true));
    show(
        cx,
        vec![HintLine {
            text: text.to_string(),
            dim: false,
            id: String::new(),
        }],
    );
    COMPACT.with(|slot| slot.set(false));
}

pub fn show(cx: &mut App, lines: Vec<HintLine>) {
    if lines.is_empty() {
        hide();
        return;
    }
    let compact = COMPACT.with(|slot| slot.get());
    let cfg = settings::hotstring_settings();
    if !compact && (!cfg.enabled || cfg.hide_hint) {
        hide();
        return;
    }
    let scale = HOST.with(|host| host.borrow().scale);
    if !ensure_window(cx, scale) {
        return;
    }
    let (handle, hwnd) = HOST.with(|host| {
        let host = host.borrow();
        (host.handle, host.hwnd)
    });
    let Some(handle) = handle else { return };

    let width = if compact {
        estimate_width_compact(&lines)
    } else {
        estimate_width(&lines)
    };
    let height = PAD * 2.0
        + lines
            .iter()
            .map(|line| if line.dim { DIM_H } else { LINE_H })
            .sum::<f32>()
        + GAP * lines.len().saturating_sub(1) as f32
        + 2.0;
    SIZE.with(|size| *size.borrow_mut() = (width, height));

    // 位置 = 鼠标（或文字插入点）+ 设置里的偏移；出屏幕就夹回来
    let (cursor_pos, screen, anchor_scale) = crate::cursor::anchor(cx);
    let caret = caret_point(anchor_scale.max(0.01));
    let (base_x, base_y) = if compact {
        // 快捷栏的悬停提示：锚点永远是**鼠标**（不是文字插入点那个工字标）
        (f32::from(cursor_pos.x), f32::from(cursor_pos.y))
    } else {
        match cfg.anchor {
            settings::HintAnchor::Caret => match caret {
                // 光标有位置就贴它；目标程序报不出来（控制台等）就退回鼠标
                Some((x, y)) => (x, y),
                None => (f32::from(cursor_pos.x), f32::from(cursor_pos.y)),
            },
            settings::HintAnchor::Cursor => (f32::from(cursor_pos.x), f32::from(cursor_pos.y)),
        }
    };
    if std::env::var_os("GPUI_MENU_DEBUG_HINT").is_some() {
        eprintln!(
            "[hint] anchor={:?} caret={:?} cursor=({}, {}) base=({base_x}, {base_y}) scale={anchor_scale}",
            cfg.anchor,
            caret,
            f32::from(cursor_pos.x),
            f32::from(cursor_pos.y)
        );
    }
    let mut x = base_x + cfg.hint_x as f32;
    let mut y = base_y + cfg.hint_y as f32;
    if compact {
        // 贴在光标（这里是按钮）上方，居中一点
        x = base_x + 10.0 - width / 2.0;
        y = base_y - height - 10.0;
    }
    let left = f32::from(screen.origin.x) + 4.0;
    let top = f32::from(screen.origin.y) + 4.0;
    let right = f32::from(screen.origin.x) + f32::from(screen.size.width) - width - 4.0;
    let bottom = f32::from(screen.origin.y) + f32::from(screen.size.height) - height - 4.0;
    x = x.clamp(left, right.max(left));
    y = y.clamp(top, bottom.max(top));

    LINES.with(|current| *current.borrow_mut() = lines);
    if let Some(hwnd) = hwnd {
        // 「可以用鼠标点」= 摘掉穿透；否则保持穿透（挡住点击比不提示还烦）
        win32::set_click_through(hwnd, !cfg.hint_clickable);
        win32::fit_client_size(hwnd, width, height, scale);
        win32::move_window(
            hwnd,
            win32::px_to_physical(x - crate::layout::FRAME_INSET, scale),
            win32::px_to_physical(y, scale),
        );
        let opacity = (cfg.hint_opacity.clamp(20, 100) as f32) / 100.0;
        win32::set_window_opacity(hwnd, opacity);
        win32::show_window(hwnd);
        win32::raise_window(hwnd);
    }
    let _ = handle.update(cx, |_, window, _| window.refresh());
    HOST.with(|host| host.borrow_mut().visible = true);
    HOVERED.with(|flag| flag.set(false));

    arm_timer(cx);
    // 「鼠标停在卡片上就不消失、移开一秒后消失」用轮询实现：
    // 卡片是独立窗口，鼠标直接移出去时 GPUI 的 `on_hover(false)` 不一定会来。
    watch_cursor(cx);
}

/// 每 200ms 看一眼光标在不在卡片里，进出各处理一次。
///
/// 为什么不用 `on_hover`：提示卡片是 `WS_EX_NOACTIVATE` 的独立窗口，
/// 鼠标从菜单那边直接划过来、或者一下子移开，悬停事件都可能不来 ——
/// 表现就是「移开之后卡片一直挂着」。系统的光标位置每 200ms 问一次很便宜。
fn watch_cursor(cx: &mut App) {
    if WATCHING.with(|flag| flag.replace(true)) {
        return;
    }
    cx.spawn(async move |cx| {
        let mut inside = false;
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let (visible, hwnd) = HOST.with(|host| {
                let host = host.borrow();
                (host.visible, host.hwnd)
            });
            let Some(hwnd) = hwnd.filter(|_| visible) else {
                break;
            };
            let Some((x, y, w, h)) = win32::window_rect(hwnd) else {
                break;
            };
            let (cx_pos, cy_pos) = win32::cursor_position();
            let now_inside =
                cx_pos >= x && cx_pos < x + w && cy_pos >= y && cy_pos < y + h;
            match (inside, now_inside) {
                (false, true) => {
                    inside = true;
                    let _ = cx.update(|_app: &mut App| {
                        HOVERED.with(|flag| flag.set(true));
                        crate::hotstring::bump_hint_generation();
                    });
                }
                (true, false) => {
                    inside = false;
                    let _ = cx.update(|app: &mut App| {
                        HOVERED.with(|flag| flag.set(false));
                        arm_timer_for(app, LEAVE_HIDE_MS);
                    });
                }
                _ => {}
            }
        }
        WATCHING.with(|flag| flag.set(false));
    })
    .detach();
}

/// 文字插入点（逻辑像素，右下角）。`GetGUIThreadInfo` 给的是物理像素。
fn caret_point(scale: f32) -> Option<(f32, f32)> {
    let (_, _, right, bottom) = win32::caret_rect()?;
    Some((right as f32 / scale, bottom as f32 / scale))
}

/// 排一次「到点自己收起」。鼠标停在卡片上时不算（那时候不该消失）。
fn arm_timer(cx: &mut App) {
    let ms = settings::hotstring_settings().hint_ms.clamp(500, 60_000);
    arm_timer_for(cx, ms);
}

/// 同上，但用指定的时长（鼠标刚移开时用固定的一秒）。
fn arm_timer_for(cx: &mut App, ms: u64) {
    // 用「这一轮的编号」判断有没有被新内容 / 鼠标悬停顶掉
    let generation = crate::hotstring::hint_generation();
    let ms = ms.clamp(120, 60_000);
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(ms))
            .await;
        if crate::hotstring::hint_generation() != generation {
            return;
        }
        if HOVERED.with(|flag| flag.get()) {
            return;
        }
        let _ = cx.update(|_app| {
            hide();
            crate::hotstring::on_hint_expired();
        });
    })
    .detach();
}

/// 把提示收起来（设置改了 / 热字符串停了 / 超时）。
pub fn hide() {
    // 作废还没弹出来的悬停提示
    TIP_TICKET.with(|slot| slot.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
    HOVERED.with(|flag| flag.set(false));
    HOST.with(|host| {
        let mut host = host.borrow_mut();
        if let Some(hwnd) = host.hwnd {
            if host.visible {
                win32::hide_window(hwnd);
            }
        }
        host.visible = false;
    });
}

/// 卡片要多宽：按最长那行的字符估（中文一个字宽，ASCII 半个）。
/// 紧凑版：宽度就是文字宽度 + 内边距，不套 `MIN_W`（否则短词会留一大块空白）。
fn estimate_width_compact(lines: &[HintLine]) -> f32 {
    let mut widest: f32 = 0.0;
    for line in lines {
        let mut width = 0.0;
        for ch in line.text.chars() {
            width += if ch.is_ascii() { 6.6 } else { 12.0 };
        }
        widest = widest.max(width);
    }
    (widest + PAD * 2.0 + 4.0).max(28.0)
}

fn estimate_width(lines: &[HintLine]) -> f32 {
    let mut widest: f32 = 0.0;
    for line in lines {
        let mut width = 0.0;
        for ch in line.text.chars() {
            width += if ch.is_ascii() { 6.4 } else { 11.8 };
        }
        widest = widest.max(width);
    }
    (widest + PAD * 2.0 + 6.0).clamp(MIN_W, MAX_W)
}
