//! 主窗口：演示面板（快捷键提示、打开菜单 / 打开设置、最近一次选择）。
//!
//! 配色全部走 `theme` 令牌，所以换主题时这个窗口也会跟着变 —— 正好用来确认
//! 「主题真的生效了」。

use gpui::prelude::*;
use gpui::{
    div, px, ClickEvent, Context, Entity, FontWeight, Global, Render, SharedString, Subscription,
    Window,
};

use crate::popup;
use crate::settings;
use crate::settings_ui;
use crate::theme;

/// 应用共享状态：被弹出菜单写入，被主窗口读取。
#[derive(Default)]
pub struct AppState {
    /// 最近一次执行的菜单项文案
    pub last_action: Option<SharedString>,
    /// 累计执行次数
    pub hits: usize,
}

/// 把 `Entity<AppState>` 挂到 App 上，任意地方都能拿到。
pub struct AppStateHandle(pub Entity<AppState>);

impl Global for AppStateHandle {}

pub struct HostView {
    state: Entity<AppState>,
    /// 设置变了要重绘（换主题时这个窗口跟着变色）
    _settings: Subscription,
    _sub: Subscription,
}

impl HostView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&state, |_this, _state, cx| cx.notify());
        let settings_sub = settings::store(cx)
            .map(|store| cx.observe(&store, |_this, _store, cx| cx.notify()))
            .unwrap();
        Self {
            state,
            _settings: settings_sub,
            _sub: sub,
        }
    }
}

impl Render for HostView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (last, hits) = {
            let state = self.state.read(cx);
            (state.last_action.clone(), state.hits)
        };
        let last_line = match last {
            Some(label) if crate::i18n::is_english() => {
                format!("Last ran: {label} ({hits}×)")
            }
            Some(label) => format!("最近执行：{label}（第 {hits} 次）"),
            None => crate::i18n::t("最近执行：（还没有选择过菜单项）").to_string(),
        };
        let mode = settings::read(|settings| settings.theme_mode.label());
        let tone = crate::i18n::t(if theme::palette().dark {
            "深色"
        } else {
            "浅色"
        });

        div().size_full().flex().bg(theme::surface()).p(px(18.0)).child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .p(px(18.0))
                .bg(theme::surface_elevated())
                .rounded(px(theme::RADIUS_WINDOW))
                .border_1()
                .border_color(theme::border())
                .shadow_sm()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(15.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::text_primary())
                                        .child("RunAny_RS"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(theme::text_tertiary())
                                        .child(if crate::i18n::is_english() {
                                        format!("Theme · {mode} / {tone}")
                                    } else {
                                        format!("当前配色 · {mode} / {tone}")
                                    }),
                                ),
                        )
                        .child(
                            // 主题色点：一眼看出强调色
                            div()
                                .size(px(10.0))
                                .rounded_full()
                                .bg(theme::accent()),
                        ),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme::text_secondary())
                        .child({
                            // 跟着设置走：在「行为 → 全局快捷键」里改过这里也跟着变
                            let key = settings::read(|settings| settings.popup_hotkey.clone())
                                .trim()
                                .to_string();
                            if crate::i18n::is_english() {
                                format!("Hotkey {key} — pops up at the cursor in any app")
                            } else {
                                format!("全局快捷键 {key} —— 在任意应用里按下都会在鼠标处弹出")
                            }
                        }),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button(crate::i18n::t("打开菜单"), true).on_click(cx.listener(
                                |_this, _: &ClickEvent, _window, cx| {
                                    // Context<T> 可以 Deref 成 &mut App
                                    popup::toggle(cx);
                                },
                            )),
                        )
                        .child(button(crate::i18n::t("设置…"), false).on_click(cx.listener(
                            |_this, _: &ClickEvent, _window, cx| {
                                settings_ui::open(cx);
                            },
                        ))),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme::text_primary())
                        .child(last_line),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme::text_tertiary())
                        .child(crate::i18n::t("↑ ↓ 移动 · → 展开子菜单 · ← 返回上级 · Enter 执行 · Esc 或点击空白处关闭")),
                ),
        )
    }
}

/// 主窗口里的按钮：主按钮用强调色，次按钮用描边。
fn button(label: &'static str, primary: bool) -> gpui::Stateful<gpui::Div> {
    let base = div()
        .id(SharedString::from(format!("host-button-{label}")))
        .h(px(34.0))
        .px(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .cursor_pointer()
        .text_size(px(13.0))
        .hover(|style| style.opacity(0.88));
    if primary {
        base.bg(theme::accent())
            .text_color(theme::surface_elevated())
            .child(label)
    } else {
        base.bg(theme::surface_sunken())
            .border_1()
            .border_color(theme::border_strong())
            .text_color(theme::text_primary())
            .child(label)
    }
}
