//! 设置窗口 / 编辑窗口共用的「外壳」：自绘标题栏 + 系统的缩放边框。
//!
//! 这套写法照抄 Bolume（也是 Rust + GPUI）的设置窗口：
//!
//! ```text
//! WindowOptions {
//!     titlebar: Some(TitlebarOptions { appears_transparent: true, .. }),
//!     window_decorations: Some(WindowDecorations::Client),
//!     is_resizable: true, is_movable: true, is_minimizable: true,
//! }
//! ```
//!
//! 关键在于**不要去动窗口区域（`SetWindowRgn`）**。gpui 给带 `WS_THICKFRAME`
//! 的窗口铺满客户区（`WM_NCCALCSIZE` 返回 0），可 `DefWindowProc` 的
//! `WM_NCHITTEST` 仍然会在四边、四角给出 `HTLEFT` / `HTTOPRIGHT` 这些代码，
//! 于是拖边框、拖角改大小、双击标题栏、贴边吸附全都由系统完成。
//!
//! 之前为了让设置窗口「也是圆角」，我们用 `SetWindowRgn` 裁了一块圆角区域 ——
//! 区域会把窗口形状一起改掉，边框的命中区跟着变形，拖边框就时灵时不灵。
//! 现在圆角只在 Win11 上由 DWM 提供，Win10 上就老老实实是直角，
//! 换来的是 100% 可靠的原生缩放。

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    div, px, App, ClickEvent, Div, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString,
    Stateful, TitlebarOptions, Window,
};

use crate::theme;

/// 自绘标题栏的窗口参数（标题仍留给任务栏 / Alt+Tab 用）。
pub fn titlebar(title: &'static str) -> TitlebarOptions {
    TitlebarOptions {
        title: Some(title.into()),
        appears_transparent: true,
        ..Default::default()
    }
}

/// 窗口一建好就调用：Win11 让 DWM 裁系统圆角，Win10 什么都不做（直角）。
pub fn apply(hwnd: isize) {
    crate::win32::native_round_corners(hwnd);
}

/// 两个窗口都要记住的几样东西。
#[derive(Clone)]
pub struct Chrome {
    /// 窗口 HWND（gpui 不暴露，靠创建前后枚举窗口做差集拿到）
    pub hwnd: Rc<Cell<Option<isize>>>,
    /// 标题栏「按下了、还没拖动」的中间状态。
    ///
    /// 按下鼠标的瞬间就进系统拖动环会吞掉双击的第二下，所以要先等鼠标真的动。
    pub armed: Rc<Cell<bool>>,
}

impl Chrome {
    pub fn new(hwnd: Rc<Cell<Option<isize>>>) -> Self {
        Self {
            hwnd,
            armed: Rc::new(Cell::new(false)),
        }
    }

    pub fn maximized(&self) -> bool {
        self.hwnd
            .get()
            .map(crate::win32::is_maximized)
            .unwrap_or(false)
    }

    /// 最大化 / 还原（按钮和双击标题栏共用同一条路）。
    pub fn toggle_maximize(&self, window: &mut Window) {
        toggle_maximize(window, self);
    }
}

/// 最大化 / 还原。
///
/// 两个方向都走 `ShowWindowAsync`（`zoom_window` 内部就是异步的），
/// 让 `WM_SIZE` 排在当前这帧之后再送达。gpui 收到 `WM_SIZE` 会自己重排、
/// 自己记住窗口尺寸，所以这里**不需要**再算一遍「还原成多大」——
/// 手动 `window.resize()` 反而会和系统的还原尺寸打架。
pub fn toggle_maximize(window: &mut Window, chrome: &Chrome) {
    let Some(hwnd) = chrome.hwnd.get() else {
        return;
    };
    if crate::win32::is_maximized(hwnd) {
        crate::win32::restore_window(hwnd);
    } else {
        window.zoom_window();
    }
    window.refresh();
}

/// 标题栏上一块可拖动的区域。
///
/// - 按下：记一笔记号（双击则直接最大化 / 还原，跟系统标题栏一个手感）
/// - 移动：真的动了才交给系统拖（`WM_NCLBUTTONDOWN` + `HTCAPTION`，
///   之后移动、贴边吸附、双击还原都是 Windows 自己的事）
pub fn drag_area(id: &'static str, chrome: &Chrome) -> Stateful<Div> {
    let on_down = chrome.clone();
    let on_move = chrome.clone();
    let on_up = chrome.clone();

    div()
        .id(SharedString::from(id))
        .h_full()
        .flex()
        .items_center()
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, _cx| {
                if event.click_count >= 2 {
                    on_down.armed.set(false);
                    toggle_maximize(window, &on_down);
                } else {
                    on_down.armed.set(true);
                }
            },
        )
        .on_mouse_move(move |event: &MouseMoveEvent, _window, _cx| {
            if on_move.armed.get() && event.pressed_button == Some(MouseButton::Left) {
                on_move.armed.set(false);
                if let Some(hwnd) = on_move.hwnd.get() {
                    crate::win32::start_system_drag(hwnd, crate::win32::HT_CAPTION);
                }
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, _, _| on_up.armed.set(false))
}

/// 标题栏右上角的一颗窗口按钮（最小化 / 最大化 / 关闭）。
pub fn caption_button(
    glyph: &'static str,
    id: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .size(px(26.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .text_size(px(11.))
        .text_color(theme::ui_text_tertiary())
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme::surface_hover())
                .text_color(theme::text_primary())
        })
        .on_click(on_click)
        .child(glyph)
}

/// 和 `caption_button` 长得一样，但点击回调是「视图自己的 listener」。
///
/// 关闭按钮要做的不只是关窗（有未保存的改动还得先回滚），
/// 那需要 `&mut Context<View>`；`caption_button` 只给得到 `&mut App`。
pub fn caption_button_action<T: 'static>(
    glyph: &'static str,
    id: &'static str,
    cx: &mut gpui::Context<T>,
    on_click: impl Fn(&mut T, &ClickEvent, &mut Window, &mut gpui::Context<T>) + 'static,
) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .size(px(26.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .text_size(px(11.))
        .text_color(theme::ui_text_tertiary())
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme::surface_hover())
                .text_color(theme::text_primary())
        })
        .on_click(cx.listener(on_click))
        .child(glyph)
}

/// 标题栏左侧的「圆点 + 标题」块（两个窗口长得一致）。
pub fn title_row(icon: bool, title: impl Into<SharedString>) -> Div {
    let mut row = div().h_full().flex().items_center().gap(px(8.)).px(px(6.));
    if icon {
        row = row.child(
            div()
                .size(px(9.))
                .flex_none()
                .rounded_full()
                .bg(theme::accent()),
        );
    }
    row.child(
        div()
            .text_size(px(12.5))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::ui_text_secondary())
            .child(title.into()),
    )
}
