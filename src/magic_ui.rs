//! 「一键直达」的规则编辑窗口 —— 新建 / 修改一条（或一批）规则。
//!
//! ## 为什么单开一扇窗
//!
//! 以前「加一条规则」是在设置页里就地长出一行：类型只有一颗点了会循环切换的小胶囊，
//! 十七种比法要连点十六下才能从「正则」摸到「非空就行」；表达式一个单行框，
//! 想给同一个目标配五条后缀还得点五次「加一条规则」；目标还得手打菜单项的名字，
//! 一重名就绑错人。全塞进 380px 的左栏是没救的，所以拉出来单独一扇窗：
//!
//! - **类型是下拉列表**：点开一眼看全十七种，点哪颗是哪颗；
//! - **表达式可以多行**：一行一条，保存时一次追加成多条规则；
//! - **目标是「直达项」**：这里的项**不在菜单里出现**（挂在隐藏根层 `__magic__` 下），
//!   专门给规则用 —— 名字随便起，重名也不打架；新建完还能直接开动作编辑器配它要干什么。
//!
//! ## 写盘时机
//!
//! 和按键面板、生效条件窗口一样：**点「保存」才写**。窗口里改了半截不保存就关，
//! 设置里一个字都不动。

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    div, px, size, AnyElement, AnyWindowHandle, App, AppContext, Bounds, ClickEvent, Context,
    Entity, FocusHandle, Global, SharedString, Subscription, Window, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};

use crate::chrome::{self, Chrome};
use crate::magic::Kind;
use crate::settings;
use crate::settings_ui::{ghost_button, ghost_button_tagged, primary_button_if};
use crate::text_area::TextArea;
use crate::text_input::TextInput;
use crate::theme;

const WIDTH: f32 = 820.0;
const HEIGHT: f32 = 640.0;
const MIN_WIDTH: f32 = 660.0;
const MIN_HEIGHT: f32 = 480.0;

pub struct RuleUiHandle(pub Cell<Option<AnyWindowHandle>>);

impl Global for RuleUiHandle {}

pub fn install(cx: &mut App) {
    cx.set_global(RuleUiHandle(Cell::new(None)));
}

/// 关掉窗口（没保存 = 什么都不改，内容只活在窗口里）。
pub fn close(cx: &mut App) {
    let Some(slot) = cx.try_global::<RuleUiHandle>() else {
        return;
    };
    let Some(handle) = slot.0.take() else {
        return;
    };
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

/// 打开规则窗口。`rule = None` 是新建，`Some(id)` 是改那一条。
pub fn open(rule: Option<&str>, cx: &mut App) {
    close(cx);

    let mut width = WIDTH;
    let mut height = HEIGHT;
    if let Some(display) = cx.primary_display() {
        let screen = display.bounds();
        width = width.min(f32::from(screen.size.width) - 60.0);
        height = height.min(f32::from(screen.size.height) - 60.0);
    }
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);

    let hwnd_cell: Rc<Cell<Option<isize>>> = Rc::new(Cell::new(None));
    let held = hwnd_cell.clone();
    let before = crate::win32::zed_window_handles();
    let rule = rule.map(str::to_string);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(chrome::titlebar(crate::i18n::t("一键直达规则"))),
            window_decorations: Some(WindowDecorations::Client),
            kind: WindowKind::Normal,
            is_resizable: true,
            is_movable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
            focus: true,
            show: true,
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        move |_window, cx| cx.new(|cx| RuleUi::new(rule, hwnd_cell, cx)),
    );

    let Ok(handle) = opened else {
        return;
    };
    let any: AnyWindowHandle = handle.into();
    if let Some(slot) = cx.try_global::<RuleUiHandle>() {
        slot.0.set(Some(any));
    }

    let hwnd = crate::win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        held.set(Some(hwnd));
        chrome::apply(hwnd);
    }
}

pub struct RuleUi {
    /// 正在改哪条规则（`None` = 新建）
    rule: Option<String>,
    chrome: Chrome,
    kind: Kind,
    patterns: Entity<TextArea>,
    note: Entity<TextInput>,
    /// 命中之后跑谁（直达项的 id）
    target: Option<String>,
    /// 类型下拉开着吗
    kind_open: bool,
    /// 多行匹配（正则加 `(?s)`，`.` 能吃换行）
    multiline: bool,
    /// 正在新建直达项：那个名字框 + 建成子菜单吗
    new_name: Option<(Entity<TextInput>, bool)>,
    /// 底部红字（没选目标 / 没填表达式这类）
    error: Option<String>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl RuleUi {
    fn new(rule: Option<String>, hwnd: Rc<Cell<Option<isize>>>, cx: &mut Context<Self>) -> Self {
        let stored = rule.as_deref().and_then(|id| {
            settings::read(|settings| {
                settings
                    .magic
                    .rules
                    .iter()
                    .find(|rule| rule.id == id)
                    .cloned()
            })
        });
        let kind = stored.as_ref().map(|rule| rule.kind).unwrap_or(Kind::Regex);
        let initial = stored
            .as_ref()
            .map(|rule| rule.pattern.clone())
            .unwrap_or_default();
        let note_text = stored
            .as_ref()
            .map(|rule| rule.note.clone())
            .unwrap_or_default();
        // 一条规则配一个菜单项：新建规则时**当场建好**那个项，不再让用户
        // 从一堆已有项里挑（挑错了规则就永远指向别的东西）。
        let target = match stored.as_ref() {
            Some(rule) => Some(rule.target.clone()).filter(|target| !target.trim().is_empty()),
            None => crate::magic::create_target(cx, false).into(),
        };

        let patterns = cx.new(|cx| {
            TextArea::new(
                initial,
                crate::i18n::t("一行一条，例如 ^订单号[:：]\\s*(\\d+)$"),
                12.0,
                cx,
            )
            .with_min_rows(6)
        });
        let note = cx.new(|cx| {
            TextInput::new(
                note_text,
                crate::i18n::t("给自己看的备注（显示在规则行上）"),
                11.5,
                cx,
            )
        });

        // 直达项的名字 / 条数可能在另一扇窗（动作编辑器）里被改，跟着刷
        let mut subs = Vec::new();
        if let Some(store) = settings::store(cx) {
            subs.push(cx.observe(&store, |_this, _store, cx| cx.notify()));
        }

        Self {
            rule,
            chrome: Chrome::new(hwnd),
            kind,
            patterns,
            note,
            target,
            kind_open: false,
            multiline: stored.as_ref().map(|rule| rule.multiline).unwrap_or(false),
            new_name: None,
            error: None,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    /// 换一种比法（换了之后不需要表达式的种类就不看那个框了）。
    fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.kind_open = false;
        self.error = None;
        cx.notify();
    }

    /// 新建一个直达项（`submenu` = 建成子菜单），建完直接选中它。
    fn create_target(&mut self, name: &str, submenu: bool, cx: &mut Context<Self>) {
        let id = crate::magic::create_target(cx, submenu);
        let name = name.trim().to_string();
        if !name.is_empty() {
            let id_for_label = id.clone();
            settings::update(cx, move |settings| {
                if let Some(path) = settings.find_path(&id_for_label) {
                    if let Some(item) = settings.node_mut(&path) {
                        item.label = name;
                    }
                }
            });
        }
        self.target = Some(id);
        self.new_name = None;
        self.error = None;
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            self.error = Some(crate::i18n::t("先选一个「命中之后跑」的直达项").to_string());
            cx.notify();
            return;
        };
        let kind = self.kind;
        // 一条规则只有**一个**表达式，整段都算（多行匹配时表达式本身就能跨行）。
        // 以前是「一行一条、一次加好几条」，规则和菜单项的关系就糊了。
        let pattern = self.patterns.read(cx).text().trim().to_string();
        if kind.needs_pattern() && pattern.is_empty() {
            self.error = Some(crate::i18n::t("这种比法要填表达式").to_string());
            cx.notify();
            return;
        }
        if kind == Kind::Regex && !pattern.is_empty() {
            if let Err(err) = regex_check(&pattern) {
                self.error = Some(err);
                cx.notify();
                return;
            }
        }
        let note = self.note.read(cx).text();
        let multiline = self.multiline;
        match self.rule.clone() {
            Some(id) => crate::magic::update_rule_with(cx, &id, kind, &pattern, &target, &note, multiline),
            None => {
                crate::magic::add_rules_with(cx, kind, &[pattern], &target, &note, multiline);
            }
        }
        window.remove_window();
    }

    fn header(&self, maximized: bool, cx: &mut Context<Self>) -> AnyElement {
        let chrome = self.chrome.clone();
        let chrome_max = self.chrome.clone();
        let title = if self.rule.is_some() {
            crate::i18n::t("改一条规则")
        } else {
            crate::i18n::t("加规则")
        };
        div()
            .id("rule-header")
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .px(px(6.))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                chrome::drag_area("rule-titlebar", &chrome)
                    .flex_1()
                    .min_w_0()
                    .child(chrome::title_row(true, title)),
            )
            .child(chrome::caption_button("—", "rule-min", |_event, window, _cx| {
                window.minimize_window();
            }))
            .child(chrome::caption_button(
                if maximized { "❐" } else { "□" },
                "rule-max",
                move |_event, window, _cx| chrome_max.toggle_maximize(window),
            ))
            .child(chrome::caption_button_action(
                "✕",
                "rule-close",
                cx,
                |_this, _: &ClickEvent, window, _cx| window.remove_window(),
            ))
            .into_any_element()
    }

    /// 「比法」一栏：一颗下拉按钮 + 展开时的十七颗胶囊。
    fn kind_block(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(74.))
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("怎么比")),
                    )
                    .child(
                        div()
                            .id("rule-kind")
                            .h(px(26.))
                            .px(px(9.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(6.))
                            .bg(theme::surface_elevated())
                            .border_1()
                            .border_color(if self.kind_open {
                                theme::accent()
                            } else {
                                theme::border_strong()
                            })
                            .text_size(px(11.5))
                            .text_color(theme::text_primary())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .child(crate::i18n::t(self.kind.label()))
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme::ui_text_tertiary())
                                    .child(if self.kind_open { "▲" } else { "▼" }),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.kind_open = !this.kind_open;
                                window.refresh();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t(match self.kind {
                                Kind::Any => "只要选中内容不是空的就算命中",
                                Kind::Url => "看形状：http(s):// 、ftp:// 、www. 开头",
                                Kind::Color => "看形状：#abc / #aabbcc / rgb(...)",
                                Kind::Json => "能被解析成 JSON 就算命中",
                                Kind::File => "这个路径存在、而且是文件",
                                Kind::Folder => "这个路径存在、而且是文件夹",
                                _ if self.kind.needs_pattern() => "看表达式；抓到的内容有多行时记得开「多行匹配」",
                                _ => "看形状，不用填表达式",
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id("rule-multiline")
                            .flex()
                            .items_center()
                            .px(px(7.))
                            .py(px(3.))
                            .rounded(px(5.))
                            .cursor_pointer()
                            .bg(if self.multiline { theme::accent() } else { theme::track() })
                            .text_size(px(10.5))
                            .text_color(if self.multiline {
                                theme::surface_elevated()
                            } else {
                                theme::ui_text_secondary()
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.multiline = !this.multiline;
                                window.refresh();
                                cx.notify();
                            }))
                            .child(crate::i18n::t("多行匹配")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("选中内容有好几行时打开：`.` 能跨行")),
                    ),
            );

        if self.kind_open {
            let mut grid = div().flex().flex_wrap().gap(px(5.)).pl(px(82.));
            for kind in Kind::ALL {
                let selected = kind == self.kind;
                grid = grid.child(
                    div()
                        .id(SharedString::from(format!("rule-kind-{}", kind.key())))
                        .h(px(22.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .border_1()
                        .text_size(px(10.5))
                        .when(selected, |chip| {
                            chip.bg(theme::accent_soft())
                                .border_color(theme::accent())
                                .text_color(theme::accent())
                        })
                        .when(!selected, |chip| {
                            chip.bg(theme::surface())
                                .border_color(theme::border())
                                .text_color(theme::ui_text_secondary())
                                .hover(|style| style.bg(theme::surface_hover()))
                        })
                        .cursor_pointer()
                        .child(crate::i18n::t(kind.label()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.set_kind(kind, cx);
                            window.refresh();
                        })),
                );
            }
            block = block.child(grid);
        }
        block.into_any_element()
    }

    /// 「命中之后跑」：直达项列表 + 新建入口。
    fn target_block(&self, cx: &mut Context<Self>) -> AnyElement {
        let targets = crate::magic::existing_targets();
        let mut list: Vec<AnyElement> = Vec::new();

        if targets.is_empty() {
            list.push(
                div()
                    .px(px(6.))
                    .py(px(4.))
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "还没有直达项。点下面的「+ 新建」，它不在菜单里出现，只给规则用",
                    ))
                    .into_any_element(),
            );
        }

        // 一条规则只有一个项：列表里只留「这一条对应的那个」
        let targets: Vec<(String, String, bool)> = targets
            .into_iter()
            .filter(|(id, _, _)| Some(id.clone()) == self.target)
            .collect();
        for (id, label, submenu) in targets {
            let selected = self.target.as_deref() == Some(id.as_str());
            let label = if label.trim().is_empty() {
                crate::i18n::t("未命名的直达项").to_string()
            } else {
                label
            };
            let row_id = id.clone();
            let edit_id = id.clone();
            list.push(
                div()
                    .id(SharedString::from(format!("rule-target-{id}")))
                    .w_full()
                    .min_w_0()
                    .h(px(26.))
                    .px(px(7.))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if selected {
                        theme::accent()
                    } else {
                        theme::border()
                    })
                    .when(selected, |row| row.bg(theme::accent_soft()))
                    .when(!selected, |row| row.bg(theme::surface()))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(if selected {
                                theme::accent()
                            } else {
                                theme::ui_text_tertiary()
                            })
                            .child(if selected { "◉" } else { "○" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(theme::text_primary())
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(9.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t(if submenu {
                                "子菜单（命中时摊开这一支）"
                            } else {
                                "菜单项（命中时直接执行）"
                            })),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("rule-edit-{id}")))
                            .flex_none()
                            .h(px(20.))
                            .px(px(6.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_secondary())
                            .bg(theme::surface_elevated())
                            .border_1()
                            .border_color(theme::border_strong())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .child(crate::i18n::t("✎ 配动作"))
                            .on_click(cx.listener(move |_this, _: &ClickEvent, _window, cx| {
                                crate::item_editor::open(&edit_id, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.target = Some(row_id.clone());
                        this.error = None;
                        window.refresh();
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }

        let mut footer = div().flex().items_center().gap(px(6.)).pt(px(2.));
        match &self.new_name {
            Some((input, _)) => {
                footer = footer
                    .child(
                        div()
                            .w(px(220.))
                            .h(px(26.))
                            .px(px(7.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .bg(theme::surface_sunken())
                            .border_1()
                            .border_color(theme::border_strong())
                            .child(input.clone()),
                    )
                    .child(ghost_button_tagged(
                        "new-target-ok",
                        "建",
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            let Some((input, submenu)) = this.new_name.clone() else {
                                return;
                            };
                            let name = input.read(cx).text();
                            this.create_target(&name, submenu, cx);
                            window.refresh();
                        }),
                    ))
                    .child(ghost_button_tagged(
                        "new-target-cancel",
                        "算了",
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.new_name = None;
                            window.refresh();
                            cx.notify();
                        }),
                    ));
            }
            None => {
                for (submenu, label, tag) in [
                    (false, "＋ 新建直达项", "new-item"),
                    (true, "＋ 新建直达子菜单", "new-submenu"),
                ] {
                    footer = footer.child(ghost_button_tagged(
                        tag,
                        label,
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            // 输入框在点的时候才建：放在渲染里每帧都新建一个实体，
                            // 打一个字就换一个框，字永远敲不进去
                            let text = cx.new(|cx| {
                                TextInput::new(
                                    "",
                                    crate::i18n::t("起个名字，例如：用浏览器打开"),
                                    11.0,
                                    cx,
                                )
                            });
                            this.new_name = Some((text, submenu));
                            window.refresh();
                            cx.notify();
                        }),
                    ));
                }
            }
        }

        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(74.))
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("命中之后跑")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t(
                                "直达项不在菜单里露头，只为规则存在；「✎ 配动作」配它到底干什么",
                            )),
                    ),
            )
            .child(
                div()
                    .id("rule-targets")
                    .max_h(px(170.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .children(list),
            )
            .child(footer)
            .into_any_element()
    }
}

/// 一行正则能不能编译（拿给人看的那一句话）。
fn regex_check(pattern: &str) -> Result<(), String> {
    regex::RegexBuilder::new(pattern.trim())
        .case_insensitive(true)
        .build()
        .map(|_| ())
        .map_err(|err| {
            err.to_string()
                .lines()
                .next()
                .unwrap_or("正则写错了")
                .to_string()
        })
}

impl Render for RuleUi {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let maximized = self.chrome.maximized();
        let header = self.header(maximized, cx);

        let intro = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t(
                        "选中内容跟这条规则对上了，就直接跑它绑的直达项，不弹菜单",
                    )),
            )
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "规则表从上往下问，第一条命中的就执行 —— 宽泛的规则（比如「非空就行」）要放最后",
                    )),
            );

        let pattern_editor = div()
            .w_full()
            .h(px(150.))
            .p(px(6.))
            .rounded(px(6.))
            .bg(theme::surface_sunken())
            .border_1()
            .border_color(theme::border_strong())
            .child(div().size_full().child(self.patterns.clone()));

        let pattern_hint = if self.kind.needs_pattern() {
            crate::i18n::t(
                "一行一条：每行都是同一个类型、同一个目标的一条规则；改一条时多出来的行排它后面",
            )
        } else {
            crate::i18n::t("这种比法不用填表达式（表达式留着也不会被用）")
        };

        let pattern_block = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(74.))
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("匹配内容")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(pattern_hint),
                    ),
            )
            .child(
                div()
                    .flex()
                    .child(
                        div()
                            .w(px(74.))
                            .flex_none()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("例子")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t(self.kind.example())),
                    ),
            )
            .child(
                div()
                    .when(!self.kind.needs_pattern(), |block| block.opacity(0.55))
                    .child(pattern_editor),
            );

        let note_row = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .w(px(74.))
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("备注")),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(26.))
                    .px(px(7.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(theme::surface_sunken())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(self.note.clone()),
            );

        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(12.))
            .child(intro)
            .child(self.kind_block(cx))
            .child(pattern_block)
            .child(self.target_block(cx))
            .child(note_row)
            .children(self.error.clone().map(|error| {
                div()
                    .text_size(px(10.5))
                    .text_color(theme::danger())
                    .child(error)
            }));

        let can_save = self.target.is_some()
            && (!self.kind.needs_pattern() || !self.patterns.read(cx).text().trim().is_empty());

        let footer = div()
            .h(px(48.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "想写一批：一行一条，保存时一次全加上（顺序就是它们的优先级）",
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .child(ghost_button(
                        "取消",
                        cx.listener(|_this, _: &ClickEvent, window, _cx| window.remove_window()),
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .child(primary_button_if(
                        "保存",
                        can_save,
                        cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)),
                    )),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::surface_window())
            .text_color(theme::text_primary())
            .font_family(theme::font_family())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_this, event: &gpui::KeyDownEvent, window, _cx| {
                if matches!(event.keystroke.key.as_str(), "escape" | "esc") {
                    window.remove_window();
                }
            }))
            .child(header)
            .child(body)
            .child(footer)
    }
}
