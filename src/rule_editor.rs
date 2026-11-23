//! 自动规则的编辑窗口（独立窗口，和「编辑菜单项」同一套外壳）。
//!
//! 左边管规则本身（名称 / 检测次数 / 间隔 / 条件），右边管这条规则**自己的**
//! 菜单项列表（可拖动排序）—— 和主菜单、托盘那套完全独立。

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    div, px, size, AnyElement, App, Bounds, Context, Entity, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Render, SharedString, Styled,
    Subscription, Window, WindowBounds, WindowOptions,
};

use crate::settings::{self, ActionKind, ActionStep, AutoCondition, ItemSetting, ItemKind};
use crate::settings_ui::{ghost_button_tagged, segmented};
use crate::theme;

const WIDTH: f32 = 920.0;
const HEIGHT: f32 = 660.0;
const MIN_WIDTH: f32 = 760.0;
const MIN_HEIGHT: f32 = 480.0;

struct Handle(gpui::AnyWindowHandle);
struct RuleEditorHandle(Handle);
impl gpui::Global for RuleEditorHandle {}

pub fn open(rule_id: &str, cx: &mut App) {
    if let Some(handle) = cx.try_global::<RuleEditorHandle>().map(|handle| &handle.0 .0) {
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
        move |_window, cx| cx.new(|cx| RuleEditor::new(id, hwnd_cell, cx)),
    );

    let Ok(handle) = opened else {
        return;
    };
    cx.set_global(RuleEditorHandle(Handle(gpui::AnyWindowHandle::from(handle))));

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
    inputs: HashMap<String, Entity<crate::text_input::TextInput>>,
    subs: Vec<Subscription>,
    /// 展开变量面板的条件下标
    picker: Option<usize>,
    /// 拖动中的菜单项下标 + 落点
    drag: Option<usize>,
    drop: Option<usize>,
    _hwnd: Rc<Cell<Option<isize>>>,
}

impl RuleEditor {
    fn new(
        rule_id: String,
        hwnd: Rc<Cell<Option<isize>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let _ = cx;
        Self {
            rule_id,
            inputs: HashMap::new(),
            subs: Vec::new(),
            picker: None,
            drag: None,
            drop: None,
            _hwnd: hwnd,
        }
    }

    fn set_target(&mut self, rule_id: String, cx: &mut Context<Self>) {
        self.rule_id = rule_id;
        self.inputs.clear();
        self.subs.clear();
        self.picker = None;
        cx.notify();
    }

    fn rule(&self) -> Option<settings::AutoRule> {
        settings::read(|settings| {
            settings
                .auto_rules
                .iter()
                .find(|rule| rule.id == self.rule_id)
                .cloned()
        })
    }

    /// 懒创建输入框；`key` 的格式和落盘字段一一对应。
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
            view.apply(key_for_observer.clone(), text, cx);
        });
        self.subs.push(sub);
        self.inputs.insert(key, entity.clone());
        entity
    }

    /// 字段改动落到设置里。键：`name|规则id`、`rounds|规则id`、`item|规则id|项下标` 等。
    fn apply(&mut self, key: String, text: String, cx: &mut Context<Self>) {
        let mut parts = key.split('|');
        let field = parts.next().unwrap_or_default().to_string();
        let rule_id = parts.next().unwrap_or_default().to_string();
        let index = parts.next().and_then(|value| value.parse::<usize>().ok());
        settings::update(cx, move |settings| {
            let Some(rule) = settings.auto_rules.iter_mut().find(|rule| rule.id == rule_id) else {
                return;
            };
            match field.as_str() {
                "name" => rule.name = text,
                "rounds" => rule.rounds = text.trim().parse().unwrap_or(1).max(1),
                "interval" => rule.interval_secs = text.trim().parse().unwrap_or(10),
                "cond" => {
                    if let Some(cond) = index.and_then(|i| rule.conditions.get_mut(i)) {
                        cond.value = text;
                    }
                }
                "item" => {
                    if let Some(item) = index.and_then(|i| rule.items.get_mut(i)) {
                        item.label = text;
                    }
                }
                "action" => {
                    if let Some(item) = index.and_then(|i| rule.items.get_mut(i)) {
                        if let Some(step) = item.actions.first_mut() {
                            step.value = text;
                        }
                    }
                }
                _ => {}
            }
        });
    }

    fn add_condition(&mut self, cx: &mut Context<Self>) {
        let rule_id = self.rule_id.clone();
        settings::update(cx, |settings| {
            if let Some(rule) = settings.auto_rules.iter_mut().find(|rule| rule.id == rule_id) {
                rule.conditions.push(AutoCondition {
                    var: "wifi".to_string(),
                    op: "eq".to_string(),
                    value: String::new(),
                });
            }
        });
        cx.notify();
    }

    fn add_item(&mut self, cx: &mut Context<Self>) {
        let rule_id = self.rule_id.clone();
        settings::update(cx, |settings| {
            if let Some(rule) = settings.auto_rules.iter_mut().find(|rule| rule.id == rule_id) {
                let mut item = ItemSetting::item(
                    format!("rule-item-{}", rule.items.len() + 1),
                    "新菜单项",
                );
                item.kind = ItemKind::Item;
                item.actions = vec![ActionStep::new(ActionKind::Run, "")];
                rule.items.push(item);
            }
        });
        cx.notify();
    }
}

impl Render for RuleEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        let Some(rule) = self.rule() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t("这条规则已经删掉了"))
                .into_any_element();
        };

        // ---------- 左栏：规则本身 ----------
        let name_input = self.field(
            format!("name|{}", rule.id),
            rule.name.clone(),
            "比如「连上公司 WiFi 就挂代理」",
            cx,
        );
        let rounds_input = self.field(
            format!("rounds|{}", rule.id),
            rule.rounds.max(1).to_string(),
            "次数",
            cx,
        );
        let interval_input = self.field(
            format!("interval|{}", rule.id),
            rule.interval_secs.to_string(),
            "秒",
            cx,
        );

        let rule_id = rule.id.clone();
        let mut left = div().w(px(430.)).flex_none().h_full().flex().flex_col().gap(px(8.));
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
                        if rule.any { 1 } else { 0 },
                        cx.listener(move |this, picked: &u32, window, cx| {
                            let any = *picked == 1;
                            let rule_id = this.rule_id.clone();
                            settings::update(cx, |settings| {
                                if let Some(rule) =
                                    settings.auto_rules.iter_mut().find(|r| r.id == rule_id)
                                {
                                    rule.any = any;
                                }
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    )),
            );

        // 条件表
        let mut conditions = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .rounded(px(theme::RADIUS_SMALL))
            .border_1()
            .border_color(theme::border())
            .p(px(6.));
        if rule.conditions.is_empty() {
            conditions = conditions.child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("还没有条件")),
            );
        }
        for (index, cond) in rule.conditions.iter().enumerate() {
            conditions = conditions.child(self.condition_row(&rule, index, cond, cx));
        }
        let mut add_cond = cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
            this.add_condition(cx);
            window.refresh();
        });
        conditions = conditions.child(ghost_button_tagged(
            format!("rule-add-cond|{}", rule.id),
            crate::i18n::t("增加条件"),
            move |event, window, cx| add_cond(event, window, cx),
        ));
        left = left.child(conditions);

        // ---------- 右栏：规则自己的菜单项 ----------
        let mut right = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .pl(px(12.))
            .border_l_1()
            .border_color(theme::border());
        right = right.child(
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
                        .child(crate::i18n::t("按住 ⠿ 可拖动排序；命中后按顺序执行")),
                ),
        );

        let mut list = div()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(5.))
            .overflow_hidden();
        if rule.items.is_empty() {
            list = list.child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("还没有菜单项，点左下角「添加菜单项」")),
            );
        }
        for (index, item) in rule.items.iter().enumerate() {
            list = list.child(self.item_row(&rule, index, item, cx));
        }
        let list = list
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    let Some(from) = this.drag.take() else {
                        return;
                    };
                    if let Some(to) = this.drop.take() {
                        if from != to {
                            let rule_id = this.rule_id.clone();
                            settings::update(cx, |settings| {
                                if let Some(rule) =
                                    settings.auto_rules.iter_mut().find(|r| r.id == rule_id)
                                {
                                    if from < rule.items.len() {
                                        let item = rule.items.remove(from);
                                        let to = to.min(rule.items.len());
                                        rule.items.insert(to, item);
                                    }
                                }
                            });
                            this.inputs.clear();
                        }
                    }
                    window.refresh();
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    if this.drag.take().is_some() {
                        this.drop = None;
                        window.refresh();
                        cx.notify();
                    }
                }),
            );
        right = right.child(list);

        let mut add_item = cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
            this.add_item(cx);
            window.refresh();
        });
        right = right.child(ghost_button_tagged(
            format!("rule-add-item|{}", rule.id),
            crate::i18n::t("添加菜单项"),
            move |event, window, cx| add_item(event, window, cx),
        ));

        div()
            .size_full()
            .flex()
            .gap(px(12.))
            .p(px(14.))
            .bg(theme::surface_window())
            .child(left)
            .child(right)
            .into_any_element()
    }
}

impl RuleEditor {
    /// 一行条件：变量按钮 + 比较（分段）+ 值 + 删除。
    fn condition_row(
        &mut self,
        rule: &settings::AutoRule,
        index: usize,
        cond: &AutoCondition,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rule_id = rule.id.clone();
        let picker_open = self.picker == Some(index);
        let mut row = div().flex().items_center().gap(px(4.)).w_full().min_w_0();

        // 变量：点开面板
        let id_var = rule_id.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("rule-var-{index}")))
                .flex_none()
                .w(px(112.))
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
                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
                    let _ = &id_var;
                    this.picker = if this.picker == Some(index) {
                        None
                    } else {
                        Some(index)
                    };
                    window.refresh();
                    cx.notify();
                })),
        );

        // 比较：6 段
        let mut options: Vec<(u32, &'static str)> = Vec::new();
        for (position, (_, label)) in crate::autorule::OPS.iter().enumerate() {
            options.push((position as u32, label));
        }
        let current = crate::autorule::OPS
            .iter()
            .position(|(key, _)| *key == cond.op)
            .unwrap_or(0) as u32;
        let id_op = rule_id.clone();
        row = row.child(segmented(
            "rule-op",
            &options,
            current,
            cx.listener(move |_this, picked: &u32, window, cx| {
                let op = crate::autorule::OPS
                    .get(*picked as usize)
                    .map(|(key, _)| key.to_string())
                    .unwrap_or_else(|| "eq".to_string());
                let rule_id = id_op.clone();
                settings::update(cx, |settings| {
                    if let Some(rule) = settings.auto_rules.iter_mut().find(|r| r.id == rule_id) {
                        if let Some(cond) = rule.conditions.get_mut(index) {
                            cond.op = op;
                        }
                    }
                });
                window.refresh();
                cx.notify();
            }),
        ));

        let value_input = self.field(
            format!("cond|{}|{}", rule.id, index),
            cond.value.clone(),
            "值",
            cx,
        );
        row = row.child(div().flex_1().min_w_0().child(boxed(value_input)));

        let id_del = rule_id.clone();
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
                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
                    let rule_id = id_del.clone();
                    settings::update(cx, |settings| {
                        if let Some(rule) =
                            settings.auto_rules.iter_mut().find(|r| r.id == rule_id)
                        {
                            if index < rule.conditions.len() {
                                rule.conditions.remove(index);
                            }
                        }
                    });
                    this.picker = None;
                    window.refresh();
                    cx.notify();
                })),
        );

        let mut block = div().flex().flex_col().gap(px(4.)).w_full().min_w_0().child(row);
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
                let rule_for_pick = rule.id.clone();
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
                            move |this, _: &gpui::ClickEvent, window, cx| {
                                let var = key_owned.clone();
                                let rule_id = rule_for_pick.clone();
                                settings::update(cx, |settings| {
                                    if let Some(rule) =
                                        settings.auto_rules.iter_mut().find(|r| r.id == rule_id)
                                    {
                                        if let Some(cond) = rule.conditions.get_mut(index) {
                                            cond.var = var;
                                        }
                                    }
                                });
                                this.picker = None;
                                window.refresh();
                                cx.notify();
                            },
                        )),
                );
            }
            block = block.child(grid);
        }
        block.into_any_element()
    }

    /// 右边的一行菜单项：⠿ + 名称 + 动作（类型 + 值）+ 删除。
    fn item_row(
        &mut self,
        rule: &settings::AutoRule,
        index: usize,
        item: &ItemSetting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dragging = self.drag == Some(index);
        let drop_here = self.drop == Some(index) && self.drag.is_some();
        let rule_id = rule.id.clone();

        let mut row = div()
            .id(SharedString::from(format!("rule-item-{index}")))
            .w_full()
            .min_w_0()
            .px(px(6.))
            .py(px(5.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .rounded(px(theme::RADIUS_SMALL))
            .border_1()
            .border_color(if drop_here {
                theme::accent()
            } else {
                theme::border()
            })
            .bg(theme::surface());
        // 拖动：鼠标经过哪一行就记成落点
        if self.drag.is_some() {
            row = row.on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _w, cx| {
                if this.drop != Some(index) {
                    this.drop = Some(index);
                    cx.notify();
                }
            }));
        }

        let mut head = div().flex().items_center().gap(px(6.)).w_full().min_w_0();
        let id_grip = rule_id.clone();
        head = head.child(
            div()
                .id(SharedString::from(format!("rule-grip-{index}")))
                .flex_none()
                .px(px(2.))
                .text_size(px(12.))
                .text_color(if dragging {
                    theme::accent()
                } else {
                    theme::ui_text_tertiary()
                })
                .cursor_pointer()
                .child("⠿")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        let _ = &id_grip;
                        this.drag = Some(index);
                        this.drop = Some(index);
                        window.refresh();
                        cx.notify();
                    }),
                ),
        );

        let label_input = self.field(
            format!("item|{}|{}", rule.id, index),
            item.label.clone(),
            "菜单项名字",
            cx,
        );
        head = head.child(div().flex_1().min_w_0().child(boxed(label_input)));

        let id_del = rule_id.clone();
        head = head.child(
            div()
                .id(SharedString::from(format!("rule-delitem-{index}")))
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
                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
                    let rule_id = id_del.clone();
                    settings::update(cx, |settings| {
                        if let Some(rule) =
                            settings.auto_rules.iter_mut().find(|r| r.id == rule_id)
                        {
                            if index < rule.items.len() {
                                rule.items.remove(index);
                            }
                        }
                    });
                    this.inputs.clear();
                    window.refresh();
                    cx.notify();
                })),
        );
        row = row.child(head);

        // 动作：类型 + 值（只编辑第一条动作，够用且不把界面撑爆）
        let kind = item
            .actions
            .first()
            .map(|step| step.kind)
            .unwrap_or(ActionKind::Run);
        let kind_value = match kind {
            ActionKind::Run => 0u32,
            ActionKind::Text => 1,
            ActionKind::Keys => 2,
            _ => 3,
        };
        let id_kind = rule_id.clone();
        let mut action_row = div().flex().items_center().gap(px(4.)).w_full().min_w_0();
        action_row = action_row.child(segmented(
            "rule-kind",
            &[
                (0, crate::i18n::t("运行程序")),
                (1, crate::i18n::t("发送文本")),
                (2, crate::i18n::t("发送按键")),
                (3, crate::i18n::t("内置动作")),
            ],
            kind_value,
            cx.listener(move |this, picked: &u32, window, cx| {
                let kind = match *picked {
                    1 => ActionKind::Text,
                    2 => ActionKind::Keys,
                    3 => ActionKind::Builtin,
                    _ => ActionKind::Run,
                };
                let rule_id = id_kind.clone();
                settings::update(cx, |settings| {
                    if let Some(rule) = settings.auto_rules.iter_mut().find(|r| r.id == rule_id) {
                        if let Some(item) = rule.items.get_mut(index) {
                            if item.actions.is_empty() {
                                item.actions.push(ActionStep::new(kind, ""));
                            } else if let Some(step) = item.actions.first_mut() {
                                step.kind = kind;
                            }
                        }
                    }
                });
                this.inputs.clear();
                window.refresh();
                cx.notify();
            }),
        ));

        let action_value = item
            .actions
            .first()
            .map(|step| step.value.clone())
            .unwrap_or_default();
        let hint = match kind {
            ActionKind::Run => "程序名或完整路径，比如 notepad.exe",
            ActionKind::Text => "要发送的文字，支持 {Enter} 之类",
            ActionKind::Keys => "按键，比如 Ctrl+Shift+Esc",
            _ => "内置动作的 id，比如 win.topmost",
        };
        let value_input = self.field(
            format!("action|{}|{}", rule.id, index),
            action_value,
            hint,
            cx,
        );
        action_row = action_row.child(div().flex_1().min_w_0().child(boxed(value_input)));
        row = row.child(action_row);
        row.into_any_element()
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
