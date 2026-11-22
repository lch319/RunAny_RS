//! 「生效条件」编辑窗口 —— AHK 里的 `#If`。
//!
//! ## 它解决什么
//!
//! 菜单项的「触发快捷键」是全局的：`Ctrl+Alt+F` 一旦注册上，在任何程序里按都是它。
//! 可现实里同一个组合常常想做不同的事 —— 资源管理器里想开文件夹、编辑器里想格式化。
//! 这正是「局部快捷键」的用武之地：一条菜单项可以同时有
//! **全局触发键**（哪儿都生效）和**局部键**（只在某些窗口里生效），
//! 这个窗口编辑的就是后者那串条件。
//!
//! 同一个组合被两条局部键抢？没关系，各自的条件互斥就行 ——
//! 带条件的键在冲突检测里不算「被占用」（见 `hotkey::sync`）。
//!
//! **不满足条件时连键都不吞**（钩子里当场判断，见 `when.rs`），那个组合在别的
//! 程序里还是它本来的意思。
//!
//! ## 为什么是一扇自己的窗
//!
//! 规则要多行、要对着「现在的前台」当场试、要一行一行告诉你哪条命中 ——
//! 编辑窗口左栏只有 380px，塞进去就什么都看不清了。和按键面板一个待遇：
//! 独立窗口，可以拉大、可以摆到副屏。
//!
//! ## 写盘时机
//!
//! 和按键面板一样：**点「应用」才写回设置**。边打字边写会让钩子每敲一个字就
//! 重建一次注册表（`settings::update` 会连带 `hotkey::sync`），打字打到一半按
//! 快捷键会撞上半截规则。窗口右上角 ✕ 和不「应用」就关 = 什么都不改。

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    div, px, size, AnyElement, AnyWindowHandle, App, AppContext, Bounds, ClickEvent, Context,
    Entity, FocusHandle, Global, MouseButton, MouseDownEvent, SharedString, Subscription, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};

use crate::chrome::{self, Chrome};
use crate::settings;
use crate::settings_ui::{ghost_button, primary_button_if};
use crate::text_area::TextArea;
use crate::text_input::TextInput;
use crate::theme;

const WIDTH: f32 = 960.0;
const HEIGHT: f32 = 700.0;
const MIN_WIDTH: f32 = 780.0;
const MIN_HEIGHT: f32 = 520.0;
/// 右栏宽度：「现在的前台」那张卡片要放得下一条完整路径
const RIGHT_W: f32 = 340.0;

pub struct WhenUiHandle(pub Cell<Option<AnyWindowHandle>>);

impl Global for WhenUiHandle {}

pub fn install(cx: &mut App) {
    cx.set_global(WhenUiHandle(Cell::new(None)));
}

/// 关掉窗口（没应用 = 什么都不改，内容只活在窗口里）。
pub fn close(cx: &mut App) {
    let Some(slot) = cx.try_global::<WhenUiHandle>() else {
        return;
    };
    let Some(handle) = slot.0.take() else {
        return;
    };
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

/// 设置里存着的那份条件文本（**局部快捷键**那一份）。
fn stored(id: &str) -> String {
    settings::read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .map(|item| item.hotkey_local_when.clone())
            .unwrap_or_default()
    })
}

/// 打开某个菜单项的「生效条件」窗口。
pub fn open(id: &str, cx: &mut App) {
    // 同一时间只留一扇：换一条菜单项时直接换掉旧的
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
    let id = id.to_string();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // 和设置 / 编辑窗口同一套外壳
            titlebar: Some(chrome::titlebar(crate::i18n::t("生效条件"))),
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
        move |_window, cx| cx.new(|cx| WhenUi::new(id, hwnd_cell, cx)),
    );

    let Ok(handle) = opened else {
        return;
    };
    let any: AnyWindowHandle = handle.into();
    if let Some(slot) = cx.try_global::<WhenUiHandle>() {
        slot.0.set(Some(any));
    }

    // HWND 交回视图（拖动标题栏、最大化还原都靠它）
    let hwnd = crate::win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        held.set(Some(hwnd));
        chrome::apply(hwnd);
    }
}

/// 「插入当前××」按钮插的是哪一项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Insert {
    Exe,
    Title,
    Class,
    Ctrl,
    Path,
    /// 鼠标底下那扇窗口的字段（写出来带 `mouse.` 前缀）
    MouseExe,
    MouseTitle,
    MouseClass,
    MousePath,
    MouseCtrl,
}

pub struct WhenUi {
    /// 编辑的是哪个菜单项（按 id 定位：编辑期间菜单树被拖动 / 增删也不指错人）
    id: String,
    chrome: Chrome,
    area: Entity<TextArea>,
    /// 最近一次读到的前台。我们自己在前台时是「最近一个别人的窗口」
    /// （`win32::preview_context`），否则用户对着自家窗口写条件就没意义了
    ctx: crate::when::Context,
    /// 正拖着「十字」去屏幕上取窗口（松手前不再重复发起）
    picking: bool,
    /// 「按条件填」面板展开着吗（不展开就只留一颗小按钮，不占地方）
    builder_open: bool,
    /// 面板里「比什么」：0 exe / 1 title / 2 class / 3 ctrl / 4 path
    builder_field: usize,
    /// 面板里「怎么比」：0 包含 / 1 全等 / 2 不是
    builder_mode: usize,
    /// 面板里填的值
    builder_value: Entity<TextInput>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl WhenUi {
    fn new(id: String, hwnd: Rc<Cell<Option<isize>>>, cx: &mut Context<Self>) -> Self {
        let initial = stored(&id);
        let area = cx.new(|cx| {
            TextArea::new(
                initial,
                // 占位提示会走 `shape_line`，里面**不能有换行**（gpui 会直接 panic）
                crate::i18n::t("一行一条，例如 QQ.exe，或者 QQ.exe && title:群聊"),
                11.5,
                cx,
            )
            .with_min_rows(9)
        });
        // 打字只重画（下面那排逐行判定要跟着变），**不重新读前台** ——
        // 一重新读，用户刚用「十字」取到的那扇窗口就被换成了「当前自己这个窗口」，
        // 卡片一片空白，后面几个「+ 当前标题」也就没东西可插了
        let sub = cx.observe(&area, |_this, _area, cx| cx.notify());
        let builder_value = cx.new(|cx| {
            TextInput::new(
                "",
                crate::i18n::t("填一个值，例如 QQ.exe / 群聊 / Button"),
                11.0,
                cx,
            )
        });
        Self {
            id,
            chrome: Chrome::new(hwnd),
            area,
            ctx: crate::win32::preview_context(),
            picking: false,
            builder_open: false,
            builder_field: 0,
            builder_mode: 0,
            builder_value,
            focus: cx.focus_handle(),
            _subs: vec![sub],
        }
    }

    fn text(&self, cx: &Context<Self>) -> String {
        self.area.read(cx).text()
    }

    /// 重新读一遍前台（开窗时、点「刷新」时调）。
    ///
    /// **不要接到输入框的 change 观察者上**：那样每敲一个字都会把用户用「十字」
    /// 亲手取到的窗口换成「当前自己这个窗口」，卡片变空白，后面的「+ 当前标题」
    /// 也就插不进东西了（见 `new()` 里那个只 `notify()` 的观察者）。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.ctx = crate::win32::preview_context();
        cx.notify();
    }

    /// 拖「十字」去屏幕上点一个窗口，把那个窗口的现场抄进来。
    ///
    /// 为什么不走 GPUI 的 `on_mouse_up`：手指一旦拖出本窗口，那个事件就收不到了
    /// （表现是「拖过去松手，什么都没变」）。这里每 60ms 问一次系统「左键还按着吗」，
    /// 松手那一刻用 `WindowFromPoint` 取窗口 —— 指针在哪儿都能取到。
    fn begin_pick(&mut self, cx: &mut Context<Self>) {
        if self.picking {
            return;
        }
        self.picking = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(60))
                    .await;
                let still = this.update(cx, |this, _| this.picking).unwrap_or(false);
                if !still {
                    return;
                }
                if crate::win32::left_button_down() {
                    continue;
                }
                // 松手了：光标底下那个窗口是谁的
                let ctx = {
                    let hwnd = crate::win32::window_at_cursor();
                    // 松开在自己窗口上（只是点了一下）不算
                    if hwnd == 0 || crate::win32::window_is_self(hwnd) {
                        None
                    } else {
                        Some(crate::win32::context_of_window(hwnd))
                    }
                };
                let _ = this.update(cx, |this, cx| {
                    this.picking = false;
                    if let Some(ctx) = ctx {
                        this.ctx = ctx;
                    }
                    cx.notify();
                });
                return;
            }
        })
        .detach();
    }

    /// 把「当前窗口的某一项」写成一条条件，接到规则后面。
    ///
    /// 值里有逗号 / `&&` / 开头的 `!` `=` `/` 的一律用引号包起来 ——
    /// 窗口标题是别人起的，里面什么字符都可能有，不能被当成规则语法。
    fn insert(&mut self, kind: Insert, cx: &mut Context<Self>) {
        let Some(line) = self.line_for(kind) else {
            return;
        };
        let mut next = self.text(cx).trim_end().to_string();
        if !next.is_empty() {
            next.push('\n');
        }
        next.push_str(&line);
        self.area.update(cx, |area, cx| area.set_text(next, cx));
        // 这里**不能** `refresh()`：取过窗口之后那份现场是用户亲手点的，
        // 插一条就被打回「猜的前台」，右边卡片会莫名跳一下
        cx.notify();
    }

    fn line_for(&self, kind: Insert) -> Option<String> {
        let ctx = &self.ctx;
        let (prefix, value) = match kind {
            Insert::Exe => ("exe:", ctx.exe.clone()),
            Insert::Title => ("title:", ctx.title.clone()),
            Insert::Class => ("class:", ctx.class.clone()),
            Insert::Ctrl => ("ctrl:", ctx.ctrl_class.clone()),
            Insert::Path => ("path:", ctx.path.clone()),
            // `mouse.` 那一组看的是鼠标底下那扇窗口，不是前台
            Insert::MouseExe => ("mouse.exe:", ctx.mouse_exe.clone()),
            Insert::MouseTitle => ("mouse.title:", ctx.mouse_title.clone()),
            Insert::MouseClass => ("mouse.class:", ctx.mouse_class.clone()),
            Insert::MousePath => ("mouse.path:", ctx.mouse_path.clone()),
            Insert::MouseCtrl => ("mouse.ctrl:", ctx.mouse_ctrl_class.clone()),
        };
        if value.trim().is_empty() {
            return None;
        }
        Some(format!("{prefix}{}", quote(&value)))
    }

    /// 写回设置（`settings::update` 里会连带重建快捷键注册表）。
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let id = self.id.clone();
        settings::update(cx, |settings| {
            if let Some(path) = settings.find_path(&id) {
                if let Some(item) = settings.node_mut(&path) {
                    item.hotkey_local_when = text;
                }
            }
        });
        window.remove_window();
    }

    fn header(&self, maximized: bool, cx: &mut Context<Self>) -> AnyElement {
        let chrome = self.chrome.clone();
        let chrome_max = self.chrome.clone();
        div()
            .id("when-header")
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .px(px(6.))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                chrome::drag_area("when-titlebar", &chrome)
                    .flex_1()
                    .min_w_0()
                    .child(chrome::title_row(true, crate::i18n::t("生效条件"))),
            )
            .child(chrome::caption_button("—", "when-min", |_event, window, _cx| {
                window.minimize_window();
            }))
            .child(chrome::caption_button(
                if maximized { "❐" } else { "□" },
                "when-max",
                move |_event, window, _cx| chrome_max.toggle_maximize(window),
            ))
            // ✕ = 不应用：窗口里这半截规则不该留下
            .child(chrome::caption_button_action(
                "✕",
                "when-close",
                cx,
                |_this, _: &ClickEvent, window, _cx| window.remove_window(),
            ))
            .into_any_element()
    }

    /// 「现在的前台是什么」卡片：条件里的 `exe:` `title:` 对着它抄。
    fn now_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let ctx = &self.ctx;
        let empty = ctx.exe.is_empty() && ctx.title.is_empty() && ctx.class.is_empty();
        let mut rows: Vec<(String, String)> = Vec::new();
        // 顺序和鼠标下那一组**必须一模一样**，否则两边对着看很容易抄错行
        rows.push((crate::i18n::t("程序").to_string(), ctx.exe.clone()));
        rows.push((crate::i18n::t("标题").to_string(), ctx.title.clone()));
        rows.push((crate::i18n::t("窗口类名").to_string(), ctx.class.clone()));
        rows.push((crate::i18n::t("完整路径").to_string(), ctx.path.clone()));
        rows.push((crate::i18n::t("光标下控件").to_string(), ctx.ctrl_class.clone()));

        // 一组「名字 + 值」的行。活动窗口和鼠标下窗口两组共用这一段，
        // 免得以后改样式只改到一半
        let facts = |rows: Vec<(String, String)>| {
            let mut list = div().flex().flex_col().gap(px(4.));
            for (name, value) in rows {
            let value = if value.trim().is_empty() {
                crate::i18n::t("（空）").to_string()
            } else {
                value
            };
            list = list.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(
                        div()
                            .w(px(68.))
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(theme::text_primary())
                            // 路径太长就省略头，尾部的 exe 名是重点
                            .child(value),
                    ),
            );
            }
            list
        };
        let list = facts(rows);
        // 鼠标底下那扇窗口：条件的 `mouse.` 前缀对着这一组抄。
        // 光标下控件不在这一组里 —— 它天生属于活动窗口那一侧。
        let mouse_list = facts(vec![
            (crate::i18n::t("程序").to_string(), ctx.mouse_exe.clone()),
            (crate::i18n::t("标题").to_string(), ctx.mouse_title.clone()),
            (crate::i18n::t("窗口类名").to_string(), ctx.mouse_class.clone()),
            (crate::i18n::t("完整路径").to_string(), ctx.mouse_path.clone()),
            (crate::i18n::t("光标下控件").to_string(), ctx.mouse_ctrl_class.clone()),
        ]);

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(9.))
            .rounded(px(8.))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text_primary())
                            .child(crate::i18n::t("现在的前台窗口")),
                    )
                    .child(
                        // 十字：按住拖到目标窗口上松手，就把那扇窗口的信息抄进来
                        div()
                            .id("when-pick")
                            .h(px(20.))
                            .px(px(7.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .text_size(px(10.))
                            .border_1()
                            .when(self.picking, |chip| {
                                chip.bg(theme::accent())
                                    .border_color(theme::accent())
                                    .text_color(theme::surface_elevated())
                            })
                            .when(!self.picking, |chip| {
                                chip.bg(theme::surface_elevated())
                                    .border_color(theme::border_strong())
                                    .text_color(theme::ui_text_secondary())
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_hover()))
                            })
                            .child(crate::i18n::t(if self.picking {
                                "松开即取…"
                            } else {
                                "◎ 十字"
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                    this.begin_pick(cx);
                                    window.refresh();
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id("when-refresh")
                            .h(px(20.))
                            .px(px(7.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .text_size(px(10.))
                            .text_color(theme::ui_text_secondary())
                            .bg(theme::surface_elevated())
                            .border_1()
                            .border_color(theme::border_strong())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .child(crate::i18n::t("刷新"))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.refresh(cx);
                                window.refresh();
                            })),
                    ),
            )
            .child(list)
            .child(
                div()
                    .pt(px(6.))
                    .border_t_1()
                    .border_color(theme::border())
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t("鼠标下的窗口（条件里写 mouse.）")),
            )
            .child(mouse_list)
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(if empty {
                        crate::i18n::t(
                            "还没读到别的窗口：按住上面的「◎ 十字」拖到目标程序上松手，或者去目标程序里点一下再回来点「刷新」",
                        )
                    } else {
                        crate::i18n::t(
                            "「◎ 十字」按住拖到任意窗口上松手，取的就是那一扇；什么都不拖时，显示的是「最近一个别的程序的窗口」",
                        )
                    }),
            )
            .into_any_element()
    }

    /// 语法速查（右栏底部那一小段灰字）。
    ///
    /// 带一颗「复制」：这几行本来就写得密，想拿去别处（或者贴给同事）时
    /// 框选不了，只能整块抄 —— 直接一键拷走省事。
    fn help_block(&self, cx: &mut Context<Self>) -> AnyElement {
        let lines = vec![
            crate::i18n::t(
                "一行一条，行与行之间是「或」（任意一行命中就生效）；留空 = 所有程序里都生效。同一行里 `&&` 是「且」，逗号是「或」",
            ),
            crate::i18n::t(
                "前缀：exe: 程序名 · path: 完整路径 · title: 窗口标题 · class: 窗口类名 · ctrl: 控件类名（活动窗口里、光标底下那个）。不写前缀就几样都试，哪个命中算哪个",
            ),
            crate::i18n::t(
                "前缀前加 mouse. 就改成看「鼠标底下那扇窗口」：mouse.exe: / mouse.path: / mouse.title: / mouse.class: / mouse.ctrl:。不写就看活动窗口（拿到焦点的那个）——两边都有这几样，只是看的窗口不同",
            ),
            crate::i18n::t(
                "值：默认「包含」，`*` `?` 通配，=x 全等，/正则/ 正则，\"带引号\" 原样比；前面加 `!` 取反。行首 # 或 // 是注释",
            ),
            crate::i18n::t(
                "条件不满足时这个键不会被吞掉：它在别的程序里还是本来的意思",
            ),
        ];
        let plain = lines.join("\n");
        let mut block = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(9.))
            .rounded(px(8.))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text_primary())
                            .child(crate::i18n::t("怎么写")),
                    )
                    .child(pick_chip(
                        "when-help-copy",
                        crate::i18n::t("⧉ 复制"),
                        false,
                        cx.listener(move |_this, _: &ClickEvent, window, cx| {
                            crate::win32::write_clipboard_text(&plain);
                            cx.notify();
                            window.refresh();
                        }),
                    )),
            );
        for line in lines {
            block = block.child(
                div()
                    .text_size(px(9.5))
                    .line_height(px(16.))
                    .text_color(theme::ui_text_tertiary())
                    .child(line),
            );
        }
        block.into_any_element()
    }

    /// 逐行判定：哪些行现在命中（写错的行标红）。
    fn status_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.text(cx);
        let statuses = crate::when::statuses(&text, &self.ctx);
        let conditions = crate::when::compile(&text);
        let problems = conditions.problems().to_vec();

        let mut strip = div().flex().items_center().gap(px(4.)).flex_wrap();
        if statuses.is_empty() {
            strip = strip.child(
                div()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "还没写条件 —— 这条快捷键在所有程序里都生效",
                    )),
            );
        } else {
            for status in &statuses {
                let (glyph, color) = if status.problem.is_some() {
                    ("!", theme::danger())
                } else if status.hit {
                    ("✓", theme::accent())
                } else {
                    ("·", theme::ui_text_tertiary())
                };
                let source = shorten(&status.source, 26);
                let mut chip = div()
                    .h(px(18.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .rounded(px(4.))
                    .bg(theme::surface_sunken())
                    .border_1()
                    .border_color(if status.problem.is_some() {
                        theme::danger()
                    } else {
                        theme::border()
                    })
                    .text_size(px(9.5))
                    .text_color(if status.problem.is_some() {
                        theme::danger()
                    } else {
                        theme::ui_text_secondary()
                    })
                    .child(format!("{}", status.line))
                    .child(
                        div()
                            .text_color(color)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(glyph),
                    )
                    .child(source);
                if let Some(problem) = &status.problem {
                    chip = chip.child(
                        div()
                            .text_color(theme::danger())
                            .child(format!("— {problem}")),
                    );
                }
                strip = strip.child(chip);
            }
        }

        let verdict = if !problems.is_empty() {
            Some(
                crate::i18n::t("有行写错了：这些行不会被触发（改对了才生效）").to_string(),
            )
        } else if conditions.is_empty() {
            // 空条件那句话上一行已经说了，不再重复一遍
            None
        } else if conditions.matches(&self.ctx) {
            Some(crate::i18n::t("当前窗口满足条件 → 这个键在这儿会触发").to_string())
        } else {
            Some(crate::i18n::t("当前窗口不满足条件 → 这个键在这儿还是原来的意思").to_string())
        };

        let mut block = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(strip);
        if let Some(verdict) = verdict {
            block = block.child(
                div()
                    .text_size(px(9.5))
                    .text_color(if !problems.is_empty() {
                        theme::danger()
                    } else if conditions.matches(&self.ctx) {
                        theme::accent()
                    } else {
                        theme::ui_text_tertiary()
                    })
                    .child(verdict),
            );
        }
        block.into_any_element()
    }

    /// 「+ 当前程序」这一排。
    fn insert_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div().flex().items_center().gap(px(4.)).flex_wrap().child(
            div()
                .flex_none()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t("插入")),
        );
        for (kind, label, id) in [
            (Insert::Exe, "当前程序", "when-insert-exe"),
            (Insert::Title, "当前标题", "when-insert-title"),
            (Insert::Class, "窗口类名", "when-insert-class"),
            (Insert::Ctrl, "控件", "when-insert-ctrl"),
            (Insert::Path, "完整路径", "when-insert-path"),
        ] {
            let empty = self.line_for(kind).is_none();
            row = row.child(insert_chip(label, id, empty, kind, cx));
        }
        // 第二排：鼠标底下那扇窗口（写出来是 `mouse.xxx:`）。
        // 和上面那排分开放，免得用户以为「当前程序」也能指鼠标下的那个。
        row = row.child(
            div()
                .flex_none()
                .pl(px(4.))
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t("鼠标下")),
        );
        for (kind, label, id) in [
            (Insert::MouseExe, "程序", "when-insert-mouse-exe"),
            (Insert::MouseTitle, "标题", "when-insert-mouse-title"),
            (Insert::MouseClass, "类名", "when-insert-mouse-class"),
            (Insert::MousePath, "路径", "when-insert-mouse-path"),
            (Insert::MouseCtrl, "控件", "when-insert-mouse-ctrl"),
        ] {
            let empty = self.line_for(kind).is_none();
            row = row.child(insert_chip(label, id, empty, kind, cx));
        }
        row.into_any_element()
    }

    /// 把「按条件填」那三格拼成一行，接到规则末尾。
    ///
    /// 生成的就是手写那一套：`exe:QQ.exe` / `title:=群聊` / `!class:Button`，
    /// 所以「按钮填一半、剩下手写」这两种用法能随意混着来。
    fn append_built(&mut self, cx: &mut Context<Self>) {
        let value = self.builder_value.read(cx).text().trim().to_string();
        if value.is_empty() {
            return;
        }
        let prefix = ["exe:", "title:", "class:", "ctrl:", "path:"]
            .get(self.builder_field)
            .copied()
            .unwrap_or("exe:");
        let line = match self.builder_mode {
            // 全等：`=x` 写在值前面（值本身不用再加引号，除非里面有逗号这类）
            1 => format!("{prefix}={}", quote(&value)),
            2 => format!("!{prefix}{}", quote(&value)),
            _ => format!("{prefix}{}", quote(&value)),
        };
        let mut next = self.text(cx).trim_end().to_string();
        if !next.is_empty() {
            next.push('\n');
        }
        next.push_str(&line);
        self.area.update(cx, |area, cx| area.set_text(next, cx));
        self.builder_value
            .update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    /// 「按条件填」：不懂语法也能一条一条拼出来，拼完还能接着手写。
    fn builder_row(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.builder_open {
            return div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(pick_chip(
                    "when-builder-open",
                    crate::i18n::t("按条件填（不用记语法）"),
                    false,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.builder_open = true;
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .into_any_element();
        }

        let fields = [
            (0usize, "程序"),
            (1, "标题"),
            (2, "窗口类名"),
            (3, "光标下控件"),
            (4, "完整路径"),
        ];
        let modes = [(0usize, "包含"), (1, "等于"), (2, "不是")];

        let mut head = div()
            .flex()
            .items_center()
            .gap(px(3.))
            .flex_wrap()
            .child(
                div()
                    .flex_none()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("比什么")),
            );
        for (index, label) in fields {
            head = head.child(pick_chip(
                format!("when-field-{index}"),
                label,
                self.builder_field == index,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.builder_field = index;
                    window.refresh();
                    cx.notify();
                }),
            ));
        }

        let mut how = div()
            .flex()
            .items_center()
            .gap(px(3.))
            .child(
                div()
                    .flex_none()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("怎么比")),
            );
        for (index, label) in modes {
            how = how.child(pick_chip(
                format!("when-mode-{index}"),
                label,
                self.builder_mode == index,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.builder_mode = index;
                    window.refresh();
                    cx.notify();
                }),
            ));
        }

        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(6.))
            .rounded(px(6.))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .child(head)
            .child(how)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(24.))
                            .px(px(6.))
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .bg(theme::surface_sunken())
                            .border_1()
                            .border_color(theme::border_strong())
                            .child(self.builder_value.clone()),
                    )
                    .child(pick_chip(
                        "when-builder-add",
                        crate::i18n::t("＋ 加一条"),
                        true,
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.append_built(cx);
                            window.refresh();
                        }),
                    ))
                    .child(pick_chip(
                        "when-builder-close",
                        crate::i18n::t("收起"),
                        false,
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.builder_open = false;
                            window.refresh();
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element()
    }
}

impl Render for WhenUi {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let maximized = self.chrome.maximized();
        let text = self.text(cx);
        let stored = stored(&self.id);
        let dirty = text != stored;

        let header = self.header(maximized, cx);

        // 标题 + 一句解释
        let intro = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t("这条局部快捷键只在满足条件的窗口里生效")),
            )
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "不满足条件时不吞键：那个组合在别的程序里还是它原来的意思",
                    )),
            );

        let editor = div()
            .w_full()
            .flex_1()
            .min_h_0()
            .p(px(6.))
            .rounded(px(6.))
            .bg(theme::surface_sunken())
            .border_1()
            .border_color(theme::border_strong())
            .child(div().size_full().child(self.area.clone()));

        let left = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(intro)
            .child(editor)
            .child(self.status_strip(cx))
            .child(self.insert_row(cx))
            .child(self.builder_row(cx));

        let right = div()
            .w(px(RIGHT_W))
            .flex_none()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(self.now_card(cx))
            .child(self.help_block(cx));

        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(10.))
            .p(px(12.))
            .child(left)
            .child(right);

        // 底部：清空 / 取消 / 应用
        let footer = div()
            .h(px(46.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(div().flex_none().child(ghost_button(
                "清空",
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.area.update(cx, |area, cx| area.set_text("", cx));
                    // 只是把条件清掉，跟「看哪扇窗口」无关 —— 不要顺手 refresh 把现场换掉
                    cx.notify();
                    window.refresh();
                }),
            )))
            .child(div().flex_1())
            .child(div().flex_none().child(ghost_button(
                "取消",
                cx.listener(|_this, _: &ClickEvent, window, _cx| window.remove_window()),
            )))
            .child(
                div()
                    .flex_none()
                    .child(primary_button_if(
                        "应用",
                        dirty,
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
            // Esc = 取消（没应用就不留痕）
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

/// 一颗「+ 当前程序」那样的小胶囊。
fn insert_chip(
    label: &'static str,
    id: &'static str,
    disabled: bool,
    kind: Insert,
    cx: &mut Context<WhenUi>,
) -> AnyElement {
    let mut chip = div()
        .id(SharedString::from(id))
        .h(px(20.))
        .px(px(7.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .text_size(px(9.5))
        .border_1()
        .border_color(theme::border_strong())
        .child(format!("+ {}", crate::i18n::t(label)));
    if disabled {
        // 读不到这一项（比如光标下没有控件）：点了也没东西可插，置灰
        return chip
            .text_color(theme::ui_text_tertiary())
            .bg(theme::surface())
            .into_any_element();
    }
    chip = chip
        .text_color(theme::ui_text_secondary())
        .bg(theme::surface_elevated())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.insert(kind, cx);
            window.refresh();
        }));
    chip.into_any_element()
}

/// 一颗「比什么 / 怎么比」那样可点的小胶囊（选中态用主题色）。
fn pick_chip(
    id: impl Into<SharedString>,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id.into())
        .flex_none()
        .h(px(20.))
        .px(px(7.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::border_strong()
        })
        .bg(if active {
            theme::accent_soft()
        } else {
            theme::surface_elevated()
        })
        .text_size(px(9.5))
        .text_color(if active {
            theme::accent()
        } else {
            theme::ui_text_secondary()
        })
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .child(label)
        .on_click(on_click)
        .into_any_element()
}

/// 写进条件文本时要不要加引号（加引号 = 原样比，里面的语法字符都当文字）。
fn quote(value: &str) -> String {
    let plain = !value.contains('"')
        && !value.contains(',')
        && !value.contains('，')
        && !value.contains("&&")
        && !value.contains('*')
        && !value.contains('?')
        && !value.starts_with('!')
        && !value.starts_with('=')
        && !value.starts_with('/')
        && !value.ends_with('/');
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "'"))
    }
}

/// 按字符截短（中文标题按字节截会切出半个字）。
fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}
