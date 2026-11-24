//! 一个很小的「确认」弹窗：一句话 + 取消 / 确认。
//!
//! 删除菜单项这种操作一次就能带走一整棵子树，又没法撤销 —— 光靠「按钮变红、
//! 再点一次」还是容易误触，所以这里给一个**独立窗口**：要先看清楚再去点确认。

use std::cell::RefCell;

use gpui::prelude::*;
use gpui::{
    div, px, size, App, Bounds, Context, IntoElement, Render, Styled, Window, WindowBounds,
    WindowOptions,
};

use crate::settings_ui::{ghost_button_tagged, primary_button};
use crate::theme;

const WIDTH: f32 = 380.0;
const HEIGHT: f32 = 168.0;

/// 要确认的事。目前只有「删菜单项」，以后加了别的危险操作就往下加。
#[derive(Clone, Debug)]
pub enum Ask {
    /// 删掉这些菜单项（连同子树）
    DeleteItems(Vec<String>),
}

impl Ask {
    fn message(&self) -> String {
        match self {
            Ask::DeleteItems(ids) => {
                if ids.len() == 1 {
                    crate::i18n::t("删除这个菜单项？它下面的整支子菜单也会一起删掉，删了没法撤销。")
                        .to_string()
                } else {
                    format!(
                        "{} 个菜单项",
                        ids.len()
                    ) + &crate::i18n::t("会被删掉（连子菜单一起），删了没法撤销。")
                }
            }
        }
    }
}

thread_local! {
    /// 待确认的操作（单线程，thread_local 就够）
    static PENDING: RefCell<Option<Ask>> = const { RefCell::new(None) };
}

struct ConfirmHandle(gpui::AnyWindowHandle);
impl gpui::Global for ConfirmHandle {}

/// 弹一个确认框。`on_ok` 在用户点「确认」时执行（点取消 / 关窗都不执行）。
pub fn ask(what: Ask, cx: &mut App) {
    let message = what.message();
    // 已经开着一个了：换成新的那条（不让两个确认框叠在一起）
    if let Some(handle) = cx.try_global::<ConfirmHandle>().map(|handle| &handle.0) {
        let alive = handle
            .update(cx, |_view, window, _cx| window.activate_window())
            .is_ok();
        if alive {
            PENDING.with(|slot| *slot.borrow_mut() = Some(what));
            cx.refresh_windows();
            return;
        }
    }

    PENDING.with(|slot| *slot.borrow_mut() = Some(what));

    let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(crate::chrome::titlebar(crate::i18n::t("确认"))),
            window_decorations: Some(gpui::WindowDecorations::Client),
            kind: gpui::WindowKind::Normal,
            is_resizable: false,
            is_movable: true,
            window_min_size: Some(size(px(WIDTH), px(HEIGHT))),
            focus: true,
            show: true,
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        |_window, cx| cx.new(Confirm::new),
    );

    let Ok(handle) = opened else {
        return;
    };
    cx.set_global(ConfirmHandle(gpui::AnyWindowHandle::from(handle)));
}

struct Confirm;

impl Confirm {
    fn new(_cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for Confirm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let message = PENDING
            .with(|slot| slot.borrow().as_ref().map(|ask| ask.message()))
            .unwrap_or_default();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::surface_window())
            .child(
                // 标题栏：拖得动，右上角能关
                div()
                    .h(px(34.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(10.))
                    .border_b_1()
                    .border_color(theme::border())
                    .bg(theme::surface())
                    .on_mouse_down(gpui::MouseButton::Left, |_event, window, _cx| {
                        window.start_window_move();
                    })
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.5))
                            .text_color(theme::text_primary())
                            .child(crate::i18n::t("确认")),
                    )
                    .child(
                        div()
                            .id("confirm-close")
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_size(px(11.))
                            .text_color(theme::ui_text_tertiary())
                            .hover(|style| style.bg(theme::surface_hover()))
                            .child("✕")
                            .on_click(cx.listener(|_this, _: &gpui::ClickEvent, window, _cx| {
                                window.remove_window();
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .px(px(16.))
                    .py(px(12.))
                    .text_size(px(11.5))
                    .text_color(theme::ui_text_secondary())
                    .child(message),
            )
            .child(
                div()
                    .h(px(44.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(14.))
                    .border_t_1()
                    .border_color(theme::border())
                    .child(ghost_button_tagged(
                        "confirm-cancel",
                        crate::i18n::t("取消"),
                        cx.listener(|_this, _: &gpui::ClickEvent, window, _cx| {
                            window.remove_window();
                        }),
                    ))
                    .child(primary_button(
                        crate::i18n::t("确认删除"),
                        cx.listener(|_this, _: &gpui::ClickEvent, window, cx| {
                            window.remove_window();
                            let pending = PENDING.with(|slot| slot.borrow_mut().take());
                            if let Some(ask) = pending {
                                run(ask, cx);
                            }
                        }),
                    )),
            )
            .into_any_element()
    }
}

/// 真正执行。
fn run(ask: Ask, cx: &mut App) {
    match ask {
        Ask::DeleteItems(ids) => {
            crate::settings::update(cx, |settings| {
                for id in &ids {
                    if let Some(path) = settings.find_path(id) {
                        settings.remove_at(&path);
                    }
                }
            });
            // 删完让所有窗口重画（菜单项页、规则窗口里的树都要跟着变）
            cx.refresh_windows();
        }
    }
}
