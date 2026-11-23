//! 自动规则的编辑窗口（独立窗口，和「编辑菜单项」同一套外壳与布局）。
//!
//! - 顶部：标题栏（拖动 / 最小化 / 最大化 / 关闭）
//! - 左栏：规则本身（名称、检测次数与间隔、条件组合、条件表）—— **草稿**，
//!   点「应用 / 保存」才写回去，点「取消」直接丢
//! - 右栏：这条规则自己的菜单项，直接嵌一棵 `TreeView`（和「菜单项」页同一套：
//!   添加菜单项 / 子菜单 / 分隔线 / 多列符、拖动排序、编辑动作都在），改动即时生效
//! - 底部：保存 / 取消 / 应用

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    div, px, size, AnyElement, App, Bounds, Context, Entity, IntoElement, Render, SharedString,
    Styled, Subscription, Window, WindowBounds, WindowOptions,
};

use crate::settings::{self, AutoCondition, AutoRule};
use crate::settings_ui::{ghost_button_tagged, primary_button, segmented};
use crate::theme;

const WIDTH: f32 = 1040.0;
const HEIGHT: f32 = 700.0;
const MIN_WIDTH: f32 = 860.0;
const MIN_HEIGHT: f32 = 520.0;

struct RuleEditorHandle(gpui::AnyWindowHandle);
impl gpui::Global for RuleEditorHandle {}

pub fn open(rule_id: &str, cx: &mut App) {
    if let Some(handle) = cx.try_global::<RuleEditorHandle>().map(|handle| &handle.0) {
        let target = rule_id.to_string();
        let alive = handle
            .update(cx, |view, window, cx| {
                if let Ok(entity) = view.downcast::<RuleEditor>() {
                    entity.update(cx, |view, cx| view.set_target(target, cx));
                }
                window.activate_window();
            })
            .is_ok();
        if alive {
            return;
        }
    }

    // 容器先备好：右侧那棵树要有东西可挂
    let container = settings::update(cx, |settings| settings.ensure_rule_container(rule_id));

    let mut width = WIDTH;
    let mut height = HEIGHT;
    if let Some(display) = cx.primary_display() {
        let screen = display.bounds();
        width = width.min(f32::from(screen.size.width) - 80.0);
        height = height.min(f32::from(screen.size.height) - 80.0);
    }
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    let hwnd_cell: Rc<Cell<Option<isize>>> = Rc::new(Cell::new(None));
    let held = hwnd_cell.clone();
    let before = crate::win32::zed_window_handles();
    let id = rule_id.to_string();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(crate::chrome::titlebar(crate::i18n::t("编辑自动规则"))),
            window_decorations: Some(gpui::WindowDecorations::Client),
            kind: gpui::WindowKind::Normal,
            is_resizable: true,
            is_movable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
            focus: true,
            show: true,
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        move |_window, cx| cx.new(|cx| RuleEditor::new(id, container, hwnd_cell, cx)),
    );

    let Ok(handle) = opened else {
        return;
    };
    cx.set_global(RuleEditorHandle(gpui::AnyWindowHandle::from(handle)));

    let hwnd = crate::win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        held.set(Some(hwnd));
        crate::chrome::apply(hwnd);
    }
}

pub struct RuleEditor {
    rule_id: String,
    container_id: String,
    /// 左侧字段的草稿：点「应用 / 保存」才落盘
    draft: AutoRule,
    dirty: bool,
    inputs: HashMap<String, Entity<crate::text_input::TextInput>>,
    subs: Vec<Subscription>,
    picker: Option<usize>,
    tree: Entity<crate::tree_view::TreeView>,
    hwnd: Rc<Cell<Option<isize>>>,
}

impl RuleEditor {
    fn new(
        rule_id: String,
        container_id: String,
        hwnd: Rc<Cell<Option<isize>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = settings::read(|settings| {
            settings
                .auto_rules
                .iter()
                .find(|rule| rule.id == rule_id)
                .cloned()
                .unwrap_or_default()
        });
        let scope = crate::tree_view::Scope::Submenu(container_id.clone());
        let tree = cx.new(|cx| crate::tree_view::TreeView::new_compact(scope, cx));
        Self {
            rule_id,
            container_id,
            draft,
            dirty: false,
            inputs: HashMap::new(),
            subs: Vec::new(),
            picker: None,
            tree,
            hwnd,
        }
    }

    fn set_target(&mut self, rule_id: String, cx: &mut Context<Self>) {
        let (draft, container) = settings::read(|settings| {
            (
                settings
                    .auto_rules
                    .iter()
                    .find(|rule| rule.id == rule_id)
                    .cloned()
                    .unwrap_or_default(),
                settings::rule_container_id(&rule_id),
            )
        });
        self.rule_id = rule_id;
        self.container_id = container;
        self.draft = draft;
        self.dirty = false;
        self.inputs.clear();
        self.subs.clear();
        self.picker = None;
        let scope = crate::tree_view::Scope::Submenu(self.container_id.clone());
        self.tree = cx.new(|cx| crate::tree_view::TreeView::new_compact(scope, cx));
        cx.notify();
    }

    /// 把草稿写回设置（菜单项那一侧是即时写的，不经过草稿）。
    fn commit(&mut self, cx: &mut Context<Self>) {
        if !self.dirty {
            return;
        }
        let draft = self.draft.clone();
        settings::update(cx, |settings| {
            if let Some(rule) = settings.auto_rules.iter_mut().find(|r| r.id == draft.id) {
                rule.name = draft.name.clone();
                rule.rounds = draft.rounds;
                rule.interval_secs = draft.interval_secs;
                rule.any = draft.any;
                rule.conditions = draft.conditions.clone();
            }
        });
        self.dirty = false;
        cx.notify();
    }

    fn field(
        &mut self,
        key: String,
        initial: String,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> Entity<crate::text_input::TextInput> {
        if let Some(entity) = self.inputs.get(&key) {
            return entity.clone();
        }
        let entity = cx.new(|cx| crate::text_input::TextInput::new(initial, hint, 11.5, cx));
        let seen: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));
        let key_for_observer = key.clone();
        let sub = cx.observe(&entity, move |view, input, cx| {
            let text = input.read(cx).text();
            if seen.replace(Some(text.clone())).as_deref() == Some(text.as_str()) {
                return;
            }
            view.apply_draft(key_for_observer.clone(), text, cx);
        });
        self.subs.push(sub);
        self.inputs.insert(key, entity.clone());
        entity
    }

    /// 输入改动只动草稿。
    fn apply_draft(&mut self, key: String, text: String, cx: &mut Context<Self>) {
        let mut parts = key.split('|');
        let field = parts.next().unwrap_or_default();
        let index = parts.next().and_then(|value| value.parse::<usize>().ok());
        match field {
            "name" => self.draft.name = text,
            "rounds" => self.draft.rounds = text.trim().parse().unwrap_or(1).max(1),
            "interval" => self.draft.interval_secs = text.trim().parse().unwrap_or(10),
            "cond" => {
                if let Some(cond) = index.and_then(|i| self.draft.conditions.get_mut(i)) {
                    cond.value = text;
                }
            }
            _ => return,
        }
        self.dirty = true;
        cx.notify();
    }
}

impl Render for RuleEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = crate::chrome::Chrome::new(self.hwnd.clone());
        let maximized = window.is_maximized();
        let title = if self.draft.name.trim().is_empty() {
            crate::i18n::t("编辑自动规则").to_string()
        } else {
            self.draft.name.clone()
        };

        // ---- 标题栏
        let chrome_max = chrome.clone();
        let header = div()
            .id("rule-editor-header")
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .px(px(6.))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                crate::chrome::drag_area("rule-editor-titlebar", &chrome)
                    .flex_1()
                    .min_w_0()
                    .child(crate::chrome::title_row(true, crate::i18n::t("编辑自动规则"))),
            )
            .child(
                div()
                    .max_w(px(240.))
                    .flex_none()
                    .pr(px(6.))
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .truncate()
                    .child(title),
            )
            .child(crate::chrome::caption_button(
                "—",
                "rule-editor-min",
                |_e, window, _cx| {
                    window.minimize_window();
                },
            ))
            .child(crate::chrome::caption_button(
                if maximized { "❐" } else { "□" },
                "rule-editor-max",
                move |_e, window, _cx| chrome_max.toggle_maximize(window),
            ))
            .child(crate::chrome::caption_button(
                "✕",
                "rule-editor-close",
                |_e, window, _cx| window.remove_window(),
            ));

        // ---- 左栏（草稿）
        let name_input = self.field(
            "name|".to_string(),
            self.draft.name.clone(),
            "比如「连上公司 WiFi 就挂代理」",
            cx,
        );
        let rounds_input = self.field(
            "rounds|".to_string(),
            self.draft.rounds.max(1).to_string(),
            "次数",
            cx,
        );
        let interval_input = self.field(
            "interval|".to_string(),
            self.draft.interval_secs.to_string(),
            "秒",
            cx,
        );

        let mut left = div()
            .w(px(464.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .overflow_hidden();
        left = left
            .child(label("规则名称"))
            .child(boxed(name_input))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(label("检测"))
                    .child(small_box(rounds_input, 56.))
                    .child(label("次，每"))
                    .child(small_box(interval_input, 56.))
                    .child(label("秒一次")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(label("条件组合"))
                    .child(segmented(
                        "rule-combine",
                        &[(0, crate::i18n::t("全部满足")), (1, crate::i18n::t("任一满足"))],
                        if self.draft.any { 1 } else { 0 },
                        cx.listener(|this, picked: &u32, _window, cx| {
                            this.draft.any = *picked == 1;
                            this.dirty = true;
                            cx.notify();
                        }),
                    )),
            );

        let mut conditions = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .rounded(px(theme::RADIUS_SMALL))
            .border_1()
            .border_color(theme::border())
            .p(px(6.));
        if self.draft.conditions.is_empty() {
            conditions = conditions.child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("还没有条件，点下面的「增加条件」")),
            );
        }
        for index in 0..self.draft.conditions.len() {
            conditions = conditions.child(self.condition_row(index, cx));
        }
        conditions = conditions.child(ghost_button_tagged(
            "rule-add-condition",
            crate::i18n::t("增加条件"),
            cx.listener(|this, _: &gpui::ClickEvent, _window, cx| {
                this.draft.conditions.push(AutoCondition {
                    var: "wifi".to_string(),
                    op: "eq".to_string(),
                    value: String::new(),
                });
                this.dirty = true;
                cx.notify();
            }),
        ));
        left = left.child(conditions);

        // ---- 右栏（和菜单项页同一棵树）
        let right = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .pl(px(12.))
            .border_l_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.5))
                            .text_color(theme::text_primary())
                            .child(crate::i18n::t("要执行的菜单项")),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t(
                                "和「菜单项」页一样操作；命中后按顺序执行（这一侧的改动即时生效）",
                            )),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .child(self.tree.clone()),
            );

        // ---- 底部按钮
        let footer = div()
            .h(px(46.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(10.))
                    .text_color(if self.dirty {
                        theme::accent()
                    } else {
                        theme::ui_text_tertiary()
                    })
                    .child(if self.dirty {
                        crate::i18n::t("有未应用的改动：点「应用」写回，或「保存」写回并关闭")
                            .to_string()
                    } else {
                        crate::i18n::t("左边这些改动点「应用 / 保存」才生效").to_string()
                    }),
            )
            .child(ghost_button_tagged(
                "rule-cancel",
                crate::i18n::t("取消"),
                cx.listener(|_this, _: &gpui::ClickEvent, window, _cx| {
                    window.remove_window();
                }),
            ))
            .child(ghost_button_tagged(
                "rule-apply",
                crate::i18n::t("应用"),
                cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                    this.commit(cx);
                    window.refresh();
                }),
            ))
            .child(primary_button(
                crate::i18n::t("保存"),
                cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                    this.commit(cx);
                    window.remove_window();
                }),
            ));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::surface_window())
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .gap(px(12.))
                    .p(px(14.))
                    .child(left)
                    .child(right),
            )
            .child(footer)
            .into_any_element()
    }
}

impl RuleEditor {
    /// 一行条件（改的是草稿）。
    fn condition_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let cond = self.draft.conditions[index].clone();
        let picker_open = self.picker == Some(index);

        let mut row = div().flex().items_center().gap(px(4.)).w_full().min_w_0();
        row = row.child(
            div()
                .id(SharedString::from(format!("rule-var-{index}")))
                .flex_none()
                .w(px(104.))
                .h(px(24.))
                .px(px(6.))
                .flex()
                .items_center()
                .justify_between()
                .rounded(px(theme::RADIUS_SMALL))
                .bg(if picker_open {
                    theme::accent_soft()
                } else {
                    theme::surface()
                })
                .border_1()
                .border_color(theme::border_strong())
                .cursor_pointer()
                .text_size(px(10.5))
                .child(crate::autorule::variable_label(&cond.var))
                .child(div().text_color(theme::ui_text_tertiary()).child("▾"))
                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _window, cx| {
                    this.picker = if this.picker == Some(index) {
                        None
                    } else {
                        Some(index)
                    };
                    cx.notify();
                })),
        );

        let mut options: Vec<(u32, &'static str)> = Vec::new();
        for (position, (_, label)) in crate::autorule::OPS.iter().enumerate() {
            options.push((position as u32, label));
        }
        let current = crate::autorule::OPS
            .iter()
            .position(|(key, _)| *key == cond.op)
            .unwrap_or(0) as u32;
        row = row.child(segmented(
            "rule-op",
            &options,
            current,
            cx.listener(move |this, picked: &u32, _window, cx| {
                let op = crate::autorule::OPS
                    .get(*picked as usize)
                    .map(|(key, _)| key.to_string())
                    .unwrap_or_else(|| "eq".to_string());
                if let Some(cond) = this.draft.conditions.get_mut(index) {
                    cond.op = op;
                }
                this.dirty = true;
                cx.notify();
            }),
        ));

        let value_input = self.field(format!("cond|{index}"), cond.value.clone(), "值", cx);
        row = row.child(div().flex_1());
        row = row.child(
            div()
                .id(SharedString::from(format!("rule-delcond-{index}")))
                .flex_none()
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(10.))
                .text_color(theme::ui_text_tertiary())
                .hover(|style| style.bg(theme::surface_hover()))
                .child("✕")
                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _window, cx| {
                    if index < this.draft.conditions.len() {
                        this.draft.conditions.remove(index);
                    }
                    this.inputs.clear();
                    this.picker = None;
                    this.dirty = true;
                    cx.notify();
                })),
        );

        // 值单独一行：条件行的按钮已经占满宽度了，挤在同一行只剩几十像素，
        // 像 `QQ.exe` 这种值根本看不全
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .w_full()
            .min_w_0()
            .child(row)
            .child(boxed(value_input));
        if picker_open {
            let mut grid = div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap(px(3.))
                .p(px(5.))
                .rounded(px(theme::RADIUS_SMALL))
                .bg(theme::surface())
                .border_1()
                .border_color(theme::accent());
            for (key, label, _) in crate::autorule::VARIABLES {
                let active = cond.var == *key;
                let key_owned = key.to_string();
                grid = grid.child(
                    div()
                        .id(SharedString::from(format!("rule-pick-{index}-{key}")))
                        .px(px(7.))
                        .py(px(3.))
                        .rounded(px(4.))
                        .bg(if active {
                            theme::accent()
                        } else {
                            theme::surface_sunken()
                        })
                        .text_size(px(10.))
                        .text_color(if active {
                            theme::surface()
                        } else {
                            theme::ui_text_secondary()
                        })
                        .cursor_pointer()
                        .child(*label)
                        .on_click(cx.listener(
                            move |this, _: &gpui::ClickEvent, _window, cx| {
                                if let Some(cond) = this.draft.conditions.get_mut(index) {
                                    cond.var = key_owned.clone();
                                }
                                this.picker = None;
                                this.dirty = true;
                                cx.notify();
                            },
                        )),
                );
            }
            block = block.child(grid);
        }
        block.into_any_element()
    }
}

fn label(text: &'static str) -> AnyElement {
    div()
        .flex_none()
        .text_size(px(10.))
        .text_color(theme::ui_text_tertiary())
        .child(crate::i18n::t(text))
        .into_any_element()
}

fn boxed(input: Entity<crate::text_input::TextInput>) -> AnyElement {
    div()
        .w_full()
        .min_w_0()
        .h(px(26.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_SMALL))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border_strong())
        .child(input)
        .into_any_element()
}

fn small_box(input: Entity<crate::text_input::TextInput>, width: f32) -> AnyElement {
    div()
        .w(px(width))
        .h(px(26.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_SMALL))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border_strong())
        .child(input)
        .into_any_element()
}
