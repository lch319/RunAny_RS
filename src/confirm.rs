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
    /// 发现新版本：问要不要去下载
    DownloadUpdate { version: String, url: String },
}

impl Ask {
    /// 确认按钮上的字（删除说「删除」，下载说「下载」）。
    fn ok_label(&self) -> &'static str {
        match self {
            Ask::DeleteItems(_) => crate::i18n::t("删除"),
            Ask::DownloadUpdate { .. } => crate::i18n::t("下载"),
        }
    }

    fn message(&self) -> String {
        match self {
            // 就一句话：用户要的是个「确认删除吗」，不需要再解释后果
            Ask::DeleteItems(_) => crate::i18n::t("确认删除吗？").to_string(),
            Ask::DownloadUpdate { version, .. } => {
                format!("发现新版本 {}，现在下载吗？", version)
            }
        }
    }
}

thread_local! {
    /// 待确认的操作（单线程，thread_local 就够）
    static PENDING: RefCell<Option<Ask>> = const { RefCell::new(None) };
    /// 被冻住的那扇窗口（弹窗期间不许点，关掉再放开）
    static FROZEN: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

/// 放开被冻住的窗口。
fn thaw() {
    let hwnd = FROZEN.with(|slot| slot.replace(0));
    crate::win32::enable_window(hwnd, true);
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

    // 冻住呼叫方（一般是设置页）：弹窗期间它不该还能点
    let caller = crate::win32::foreground_window();
    FROZEN.with(|slot| slot.set(caller));
    crate::win32::enable_window(caller, false);

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
        let (message, ok_label) = PENDING
            .with(|slot| {
                slot.borrow()
                    .as_ref()
                    .map(|ask| (ask.message(), ask.ok_label()))
            })
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
                                thaw();
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
                            thaw();
                            window.remove_window();
                        }),
                    ))
                    .child(primary_button(
                        ok_label,
                        cx.listener(|_this, _: &gpui::ClickEvent, window, cx| {
                            window.remove_window();
                            thaw();
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
        Ask::DownloadUpdate { url, .. } => {
            // 交给浏览器下载：自己下载再替换运行中的 exe 要写一堆「改名-重启」的
            // 逻辑，失败率高（杀软会拦），先让用户拿到文件最稳。
            crate::win32::open_url(&url);
        }
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
