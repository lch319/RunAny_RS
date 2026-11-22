//! 菜单项编辑窗口：左边基本信息，右边动作配置。
//!
//! - 普通项：右边是「动作列表」，按顺序执行，每条可以设「执行完等多少毫秒」；
//! - 子菜单：右边换成「子项列表」，子菜单本身不配动作（这是结构决定的不变量，
//!   不是靠用户自觉——`settings::normalize_tree` 会兜底）。
//!
//! 窗口是独立窗口（不是弹层）：设置页里一行放不下这么多字段，弹层又会被
//! 页面滚动裁掉。条目按 **id** 定位 —— 编辑期间在设置页里拖动排序、增删，
//! 这里也不会指错人。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    div, px, size, AnyElement, AnyWindowHandle, App, AppContext, Bounds, ClickEvent, Context,
    Entity, ExternalPaths, FocusHandle, Global, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent,
    ScrollHandle, SharedString, Subscription, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions,
};

use crate::chrome::{self, Chrome};
use crate::settings::run_opts::{OnRun, WindowState};
use crate::settings::{self, ActionKind, ActionStep, BuiltinAction, ItemKind};
use crate::settings_ui::{ghost_button, primary_button, segmented, switch};
use crate::text_input::TextInput;
use crate::theme;

// 默认开大一点：左栏那条「基本信息」竖着排下来（名称 / 图标 / 加速键 /
// 快捷键提示 / 触发键 / 备注）要能一眼看全，不然一进来就得先滚两下。
const WIDTH: f32 = 1240.0;
const HEIGHT: f32 = 920.0;
const MIN_WIDTH: f32 = 1000.0;
const MIN_HEIGHT: f32 = 560.0;
/// 左栏宽度：太窄的话「适用选中内容」那几排胶囊会各占一行，
/// 把「浮到根菜单以后放哪儿」挤到视口外面（用户会以为没做这个功能）。
const LEFT_W: f32 = 380.0;

pub struct ItemEditorHandle(pub AnyWindowHandle);

impl Global for ItemEditorHandle {}

thread_local! {
    /// 关窗兜底：打开窗口 / 上次应用时的快照。
    /// 右上角 ✕ 会自己回滚；这条只兜 Alt+F4、任务栏「关闭窗口」这类系统关窗。
    static CLOSE_GUARD: RefCell<Option<settings::MenuSettings>> = const { RefCell::new(None) };
    /// 调试用：「生效条件」窗口只自动弹一次（`GPUI_MENU_DEBUG_WHEN=1`）
    static DEBUG_WHEN_SHOT: Cell<bool> = const { Cell::new(false) };
}

fn arm_close_guard(snapshot: &settings::MenuSettings) {
    CLOSE_GUARD.with(|slot| *slot.borrow_mut() = Some(snapshot.clone()));
}

/// 编辑窗口还活着吗（`on_window_closed` 里用来判断关掉的是不是它）。
thread_local! {
    /// 切到「会清空」的生成方式时，等着二次确认的那一个。
    ///
    /// 为什么用 thread_local 而不是结构体字段：`segmented` 的点击回调签名是
    /// `Fn(&u32, &mut Window, &mut App)`，拿不到 `&mut Self`，而 GPUI 这层又不好
    /// 改成带 self 的闭包。编辑窗口同一时刻只有一个，放线程本地不会串。
    static AUTOFILL_ARM: std::cell::Cell<Option<settings::AutoFillMode>> =
        const { std::cell::Cell::new(None) };
}

/// 真正把生成方式写进设置；「会清空」的方式顺手清掉这一支现有的项。
fn apply_autofill_mode(id: &str, mode: settings::AutoFillMode, cx: &mut App) {
    let id = id.to_string();
    settings::update(cx, move |settings| {
        if let Some(path) = settings.find_path(&id) {
            if let Some(item) = settings.node_mut(&path) {
                item.autofill.mode = mode;
                if mode.clears() {
                    // 实时 / 仅启动时 = 这一支由文件夹说了算，手动加的项留着只会
                    // 在下次重建时被清掉，不如现在就清干净（用户已二次确认过）
                    item.children.clear();
                }
            }
        }
    });
    AUTOFILL_ARM.with(|slot| slot.set(None));
}

pub fn window_alive(cx: &mut App) -> bool {
    let Some(handle) = cx.try_global::<ItemEditorHandle>().map(|handle| handle.0) else {
        return false;
    };
    handle.update(cx, |_, _, _| ()).is_ok()
}

/// 窗口已经被关掉、而改动还没应用过 → 整体退回回滚点。
pub fn revert_if_dirty(cx: &mut App) {
    let snapshot = CLOSE_GUARD.with(|slot| slot.borrow_mut().take());
    let Some(snapshot) = snapshot else {
        return;
    };
    if settings::current() == snapshot {
        return;
    }
    settings::update(cx, |settings| *settings = snapshot);
}

/// 打开（或切到）某个条目的编辑窗口。
pub fn open(id: &str, cx: &mut App) {
    // 上一次可能停在「切到实时生成，等你确认」的半路上（窗口被关掉了）：
    // 下次打开不应该还悬着，一切从头来
    AUTOFILL_ARM.with(|slot| slot.set(None));
    if let Some(handle) = cx.try_global::<ItemEditorHandle>().map(|handle| handle.0) {
        let target = id.to_string();
        let alive = handle
            .update(cx, |view, window, cx| {
                if let Ok(entity) = view.downcast::<ItemEditor>() {
                    entity.update(cx, |view, cx| view.set_target(target, window, cx));
                }
                window.activate_window();
            })
            .is_ok();
        if alive {
            return;
        }
    }

    // 屏幕再小也别让窗口比屏幕还大
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
    let id = id.to_string();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // 和设置窗口同一套外壳（Bolume 写法）：自绘标题栏 + 系统缩放边
            titlebar: Some(chrome::titlebar(crate::i18n::t("编辑菜单项"))),
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
        move |_window, cx| cx.new(|cx| ItemEditor::new(id, hwnd_cell, cx)),
    );

    let Ok(handle) = opened else {
        return;
    };
    cx.set_global(ItemEditorHandle(handle.into()));

    // HWND 要交回视图：拖动标题栏、最大化 / 还原都靠它
    let hwnd = crate::win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        held.set(Some(hwnd));
        chrome::apply(hwnd);
    }
}

/// 一条动作对应的一对输入框（参数 / 间隔）。
/// 哪一栏的滚动条（两栏各有自己的滚动位置）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ColumnBar {
    Basic,
    Actions,
    /// 动作选择器里那张列表
    Picker,
}

/// 正在拖某一栏的滚动条。
#[derive(Clone, Copy)]
struct BarDrag {
    bar: ColumnBar,
    /// 按下点相对滑块顶部的偏移
    grab: f32,
    /// 滑轨可移动长度 = 视口 - 滑块高
    track: f32,
    max: f32,
}

/// 动作选择器列表的最大高度（再高就把编辑窗口顶出去了）
const PICKER_LIST_H: f32 = 190.0;

struct ActionRow {
    value: Entity<TextInput>,
    delay: Entity<TextInput>,
    _subs: Vec<Subscription>,
}

/// 「运行程序」的高级选项面板（浮在这个窗口上，填完一次性写回参数）。
struct RunPanel {
    index: usize,
    options: crate::runopts::Options,
    args: Entity<TextInput>,
    workdir: Entity<TextInput>,
    keys: Entity<TextInput>,
}

pub struct ItemEditor {
    id: String,
    /// 第几条动作在改「运行选项」（None = 面板没开）
    run_panel: Option<RunPanel>,
    chrome: Chrome,
    label: Entity<TextInput>,
    badge: Entity<TextInput>,
    accel: Entity<TextInput>,
    shortcut: Entity<TextInput>,
    /// 备注（显示在菜单项右侧的灰色小字）
    note: Entity<TextInput>,
    /// 真正的触发快捷键（在任何程序里按下去都生效）
    hotkey: Entity<TextInput>,
    /// 局部生效的快捷键（只在满足条件的窗口里才触发；空 = 没配）
    hotkey_local: Entity<TextInput>,
    /// 热字符串：在别的程序里敲完这串字就执行这一项
    hotstring: Entity<TextInput>,
    /// 「只对哪些后缀出现」的输入框（只有勾了「文件」的项才看得到）
    ext: Entity<TextInput>,
    rows: Vec<ActionRow>,
    /// 内置动作的文本参数输入框：`(第几条动作, 参数 key)`
    param_inputs: std::collections::HashMap<(usize, String), Entity<TextInput>>,
    /// 脚本动作的多行编辑框（按动作下标）
    script_areas: std::collections::HashMap<usize, Entity<crate::text_area::TextArea>>,
    /// 正在给第几条动作挑「内置动作」（动作库列表是展开还是收起）
    picker_for: Option<usize>,
    /// 第几条动作展开了「多媒体键」那片按钮（同一时间只开一片）
    media_open: Option<usize>,
    /// 子菜单「从文件夹自动生成」那一片是展开还是收起
    autofill_open: bool,
    /// 上一次点「立即生成」的结果（按钮下面那行灰字）
    autofill_msg: Option<String>,
    /// 上一次「测试执行」的结果（动作区底部那行小字）
    test_msg: Option<String>,
    /// 自动生成：文件夹（多行，一行一个）
    autofill_dirs: Entity<crate::text_area::TextArea>,
    /// 自动生成：后缀（推荐 `mp4|mkv`；老的空格 / 逗号写法也认）
    autofill_exts: Entity<TextInput>,
    /// 自动生成：一次最多几条
    autofill_limit: Entity<TextInput>,
    /// 动作选择器的搜索框（和动作库页同一个筛选口径，只在选择器展开时用）
    picker_query: Entity<TextInput>,
    /// 动作选择器当前选中的分类（None = 全部）
    picker_category: Option<crate::actions::Category>,
    /// 子菜单右侧那棵子树（和设置页「菜单项」完全同一套编辑器）
    children_tree: Entity<crate::tree_view::TreeView>,
    /// 上次同步动作行时的 (条目 id, 动作条数)：变了才重建输入框
    rows_key: (String, usize),
    /// 打开窗口 / 上次「应用」时的整份配置，取消就回滚到它
    committed: settings::MenuSettings,
    /// 调试用：还没自动弹过输入框右键菜单（`GPUI_MENU_DEBUG_TEXTMENU=1`）
    debug_textmenu_pending: bool,
    /// 左栏 / 右栏各自的滚动位置（`overflow_y_scroll` 自己不画条，我们照着它手画）
    basic_scroll: ScrollHandle,
    actions_scroll: ScrollHandle,
    /// 动作选择器那只列表的滚动位置
    picker_scroll: ScrollHandle,
    /// 正在拖的滚动条（松手就清掉）
    bar_drag: Option<BarDrag>,
    _store: Entity<settings::SettingsStore>,
    _subs: Vec<Subscription>,
    focus: FocusHandle,
}

impl ItemEditor {
    fn new(id: String, hwnd: Rc<Cell<Option<isize>>>, cx: &mut Context<Self>) -> Self {
        let (label, badge, accel, shortcut, hotkey) = read_fields(&id);
        let label_input = cx.new(|cx| TextInput::new(label, crate::i18n::t("菜单项文案"), 12.5, cx));
        let badge_input = cx.new(|cx| TextInput::new(badge, crate::i18n::t("字符 / 图片"), 12.5, cx));
        let accel_input = cx.new(|cx| TextInput::new(accel, crate::i18n::t("一个字母或数字"), 12.5, cx));
        let shortcut_input = cx.new(|cx| TextInput::new(shortcut, crate::i18n::t("例如 Ctrl+N"), 12.5, cx));
        let hotkey_input = cx.new(|cx| TextInput::new(hotkey, crate::i18n::t("例如 Ctrl+Alt+1"), 12.5, cx));
        // 局部快捷键：另一颗独立注册的键（只在满足条件的窗口里触发）
        let hotkey_local_input = cx.new(|cx| {
            TextInput::new(
                read_str(&id, |item| item.hotkey_local.clone()),
                crate::i18n::t("例如 RCtrl+1"),
                12.5,
                cx,
            )
        });
        // 热字符串：只存一串普通字符（打出来的，不是组合键）
        let hotstring_input = cx.new(|cx| {
            TextInput::new(read_hotstring(&id), crate::i18n::t("敲什么就触发，例如 ftw"), 12.5, cx)
        });
        // 备注：给自己看的说明，显示在菜单项右侧
        let note_input = cx.new(|cx| {
            TextInput::new(read_note(&id), crate::i18n::t("给自己看的备注（显示在菜单项右侧）"), 11.5, cx)
        });

        let mut subs = Vec::new();
        subs.push(observe_field(&label_input, cx, |view, text, cx| {
            view.apply_field(move |item| item.label = text, cx)
        }));
        subs.push(observe_field(&badge_input, cx, |view, text, cx| {
            // 文字型图标最多两个字；路径（.svg/.ico/.png）就整条留着
            let badge: String = if crate::icon::is_image(&text) {
                text.trim().to_string()
            } else {
                text.chars().take(2).collect()
            };
            view.apply_field(move |item| item.badge = badge, cx)
        }));
        subs.push(observe_field(&accel_input, cx, |view, text, cx| {
            // 只收第一个数字/字母，存成大写
            let accel = settings::accel_char(&text)
                .map(|c| c.to_string())
                .unwrap_or_default();
            view.apply_field(move |item| item.accel = accel, cx)
        }));
        subs.push(observe_field(&shortcut_input, cx, |view, text, cx| {
            view.apply_field(move |item| item.shortcut = text, cx)
        }));
        subs.push(observe_field(&hotkey_input, cx, |view, text, cx| {
            // 原样存（用户可能正打到一半）；注册那边认得出来的才真的注册，
            // 界面上会把不合法的写法标出来
            let text = text.trim().to_string();
            // 顺手把「快捷键提示」填上：设了触发键以后，右边那列该显示的就是这个组合。
            // 只在提示还是空、或者正好等于上一次自动填的值时覆盖 —— 用户手改过就不动他。
            let fill_hint = {
                let (hint, prev_hotkey) = settings::read(|settings| {
                    settings
                        .find_path(&view.id)
                        .and_then(|path| settings.node(&path))
                        .map(|item| (item.shortcut.clone(), item.hotkey.clone()))
                        .unwrap_or_default()
                });
                !text.is_empty() && (hint.trim().is_empty() || hint == prev_hotkey)
            };
            let hint_text = text.clone();
            let for_input = hint_text.clone();
            view.apply_field(move |item| {
                item.hotkey = text;
                if fill_hint {
                    item.shortcut = hint_text;
                }
            }, cx);
            if fill_hint {
                view.shortcut
                    .update(cx, |input, cx| input.set_text(for_input, cx));
            }
        }));
        subs.push(observe_field(&hotkey_local_input, cx, |view, text, cx| {
            let text = text.trim().to_string();
            view.apply_field(move |item| item.hotkey_local = text, cx)
        }));
        subs.push(observe_field(&note_input, cx, |view, text, cx| {
            let text = text.trim().to_string();
            view.apply_field(move |item| item.note = text, cx)
        }));
        // 热字符串：原样存（只去掉首尾空格）。空串 = 不参与。
        subs.push(observe_field(&hotstring_input, cx, |view, text, cx| {
            let text = text.trim().to_string();
            view.apply_field(move |item| item.hotstring = text, cx)
        }));

        // 后缀：只给「勾了文件」的项用。留空 = 所有文件，所以默认是空输入框。
        let ext_input = {
            let current = settings::read(|settings| {
                settings
                    .find_path(&id)
                    .and_then(|path| settings.node(&path))
                    .map(|item| item.file_exts.clone())
                    .unwrap_or_default()
            });
            let input = cx.new(|cx| TextInput::new(current, crate::i18n::t("mp4|mkv（留空 = 所有文件）"), 11.5, cx));
            subs.push(observe_field(&input, cx, |view, text, cx| {
                let text = text.trim().to_string();
                view.apply_field(move |item| item.file_exts = text, cx)
            }));
            input
        };

        let store = settings::store(cx).expect("设置还没安装");
        let store_sub = cx.observe(&store, |_this, _store, cx| cx.notify());
        subs.push(store_sub);

        // 动作选择器的搜索框：和「设置 → 动作库」那一页同一个搜索口径（多词是「且」）
        let picker_query = cx.new(|cx| {
            TextInput::new(
                "",
                crate::i18n::t("搜动作：音量、窗口、剪贴板…"),
                11.,
                cx,
            )
        });
        let picker_sub = cx.observe(&picker_query, |_this, _input, cx| cx.notify());
        subs.push(picker_sub);

        // 输入框右键菜单是「全局状态 + 根层浮层」：这里订阅它，右键一点就能重绘出来
        if let Some(menu) = crate::text_menu::store(cx) {
            subs.push(cx.observe(&menu, |_this, _menu, cx| cx.notify()));
        }

        // 子菜单「自动生成」的三个输入框。普通项用不上，但先建好：
        // 点「添加子菜单」把类型切过去时，这一段要能立刻显示、不用等重建。
        let (af_dirs, af_exts, af_limit) = settings::read(|settings| {
            let spec = settings
                .find_path(&id)
                .and_then(|path| settings.node(&path))
                .map(|item| item.autofill.clone())
                .unwrap_or_default();
            (
                spec.dirs,
                spec.exts,
                if spec.limit == 0 {
                    String::new()
                } else {
                    spec.limit.to_string()
                },
            )
        });
        let autofill_dirs = cx.new(|cx| {
            crate::text_area::TextArea::new(
                af_dirs,
                crate::i18n::t("一行一个文件夹，例如 D://影片"),
                11.,
                cx,
            )
            .with_min_rows(3)
        });
        let autofill_exts = cx.new(|cx| {
            TextInput::new(
                af_exts,
                crate::i18n::t("mp4|mkv；留空 = 所有文件"),
                11.5,
                cx,
            )
        });
        let autofill_limit = cx.new(|cx| TextInput::new(af_limit, "500", 11.5, cx));

        // 三个输入框都直接写回这一支的 `autofill`。写之前先比一下：
        // `observe` 连「光标动一下」都会触发，值没变就别把配置标成「未保存」。
        subs.push(cx.observe(&autofill_dirs, |view, area, cx| {
            let text = area.read(cx).text();
            let same = settings::read(|settings| {
                settings
                    .find_path(&view.id)
                    .and_then(|path| settings.node(&path))
                    .map(|item| item.autofill.dirs == text)
                    .unwrap_or(true)
            });
            if !same {
                view.apply_field(|item| item.autofill.dirs = text, cx);
            }
        }));
        subs.push(cx.observe(&autofill_exts, |view, input, cx| {
            let text = input.read(cx).text();
            let same = settings::read(|settings| {
                settings
                    .find_path(&view.id)
                    .and_then(|path| settings.node(&path))
                    .map(|item| item.autofill.exts == text)
                    .unwrap_or(true)
            });
            if !same {
                view.apply_field(|item| item.autofill.exts = text, cx);
            }
        }));
        subs.push(cx.observe(&autofill_limit, |view, input, cx| {
            // 填不出来的当 0（= 没设），生成时按默认 500 兜底
            let next: u32 = input
                .read(cx)
                .text()
                .trim()
                .parse()
                .unwrap_or(0)
                .min(5000);
            let same = settings::read(|settings| {
                settings
                    .find_path(&view.id)
                    .and_then(|path| settings.node(&path))
                    .map(|item| item.autofill.limit == next)
                    .unwrap_or(true)
            });
            if !same {
                view.apply_field(move |item| item.autofill.limit = next, cx);
            }
        }));
        // 开着自动生成的子菜单，进来就展开 —— 不然用户每次都得先点开才看得见
        // 自己的文件夹 / 后缀设定，很容易以为「设置丢了」
        let autofill_open = settings::read(|settings| {
            settings
                .item_ref(&id)
                .map(|item| item.autofill.enabled)
                .unwrap_or(false)
        });
        let mut this = Self {
            children_tree: cx.new(|cx| {
                crate::tree_view::TreeView::new(
                    crate::tree_view::Scope::Submenu(id.clone()),
                    cx,
                )
            }),
            id,
            run_panel: None,
            chrome: Chrome::new(hwnd),
            label: label_input,
            badge: badge_input,
            accel: accel_input,
            shortcut: shortcut_input,
            note: note_input,
            hotkey: hotkey_input,
            hotkey_local: hotkey_local_input,
            hotstring: hotstring_input,
            ext: ext_input,
            rows: Vec::new(),
            rows_key: (String::new(), usize::MAX),
            param_inputs: std::collections::HashMap::new(),
            script_areas: std::collections::HashMap::new(),
            picker_for: None,
            media_open: None,
            autofill_open,
            autofill_msg: None,
            test_msg: None,
            autofill_dirs,
            autofill_exts,
            autofill_limit,
            picker_query,
            picker_category: None,
            committed: settings::current(),
            debug_textmenu_pending: std::env::var_os("GPUI_MENU_DEBUG_TEXTMENU").is_some(),
            basic_scroll: ScrollHandle::new(),
            actions_scroll: ScrollHandle::new(),
            picker_scroll: ScrollHandle::new(),
            bar_drag: None,
            _store: store,
            _subs: subs,
            focus: cx.focus_handle(),
        };
        this.sync_rows(cx);
        // 调试用：`GPUI_MENU_DEBUG_PICKER_OPEN=1` 进来就把第一条动作的选择器摊开
        if std::env::var_os("GPUI_MENU_DEBUG_PICKER_OPEN").is_some() {
            this.picker_for = Some(0);
        }
        arm_close_guard(&this.committed);
        this
    }

    /// 换一个条目编辑（窗口复用，不重开）。
    fn set_target(&mut self, id: String, _window: &mut Window, cx: &mut Context<Self>) {
        if self.id == id {
            return;
        }
        self.id = id;
        let (label, badge, accel, shortcut, hotkey) = read_fields(&self.id);
        self.label.update(cx, |input, cx| input.set_text(label, cx));
        self.badge.update(cx, |input, cx| input.set_text(badge, cx));
        self.accel.update(cx, |input, cx| input.set_text(accel, cx));
        self.shortcut
            .update(cx, |input, cx| input.set_text(shortcut, cx));
        self.hotkey.update(cx, |input, cx| input.set_text(hotkey, cx));
        // 子菜单的「自动生成」表单也要跟着换人（输入框是长驻实体，不换就还挂着上一条的值）
        {
            let spec = settings::read(|settings| {
                settings
                    .find_path(&self.id)
                    .and_then(|path| settings.node(&path))
                    .map(|item| item.autofill.clone())
                    .unwrap_or_default()
            });
            self.autofill_dirs
                .update(cx, |area, cx| area.set_text(spec.dirs, cx));
            self.autofill_exts
                .update(cx, |input, cx| input.set_text(spec.exts, cx));
            self.autofill_limit.update(cx, |input, cx| {
                input.set_text(
                    if spec.limit == 0 {
                        String::new()
                    } else {
                        spec.limit.to_string()
                    },
                    cx,
                )
            });
            self.autofill_msg = None;
        }
        let local = read_str(&self.id, |item| item.hotkey_local.clone());
        self.hotkey_local
            .update(cx, |input, cx| input.set_text(local, cx));
        self.hotstring
            .update(cx, |input, cx| input.set_text(read_hotstring(&self.id), cx));
        self.note
            .update(cx, |input, cx| input.set_text(read_note(&self.id), cx));
        self.rows_key = (String::new(), usize::MAX);
        self.sync_rows(cx);
        // 右侧那棵树跟着换到新条目上（普通项时它不显示，但状态别留成上一个的）
        self.children_tree.update(cx, |tree, cx| {
            tree.set_scope(crate::tree_view::Scope::Submenu(self.id.clone()), cx)
        });
        cx.notify();
    }

    /// 改当前条目的某个字段（按 id 定位）。
    /// 换图标：把选中的图片收进 `Icon/badges/`，配置里只存相对路径 ——
    /// 整个程序目录拷到别的电脑上，图标也跟着走。
    fn set_badge_file(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let stored = crate::icon::import_badge(path)
            .unwrap_or_else(|| crate::icon::resolve(path));
        self.apply_field(move |item| item.badge = stored, cx);
        window.refresh();
        cx.notify();
    }

    /// 拖进来的文件里挑第一个图片当图标。
    ///
    /// 三个落点（小方块、输入框、整行）共用这一份：以前只认那个小方块，
    /// 用户在输入框上方松手就什么都不发生，以为拖放坏了。
    fn drop_icon(&mut self, paths: &[std::path::PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = paths
            .iter()
            .find(|path| crate::icon::is_image(&path.to_string_lossy()))
        else {
            return;
        };
        let path = path.to_string_lossy().into_owned();
        self.set_badge_file(&path, window, cx);
    }

    fn apply_field(&mut self, f: impl FnOnce(&mut settings::ItemSetting), cx: &mut App) {
        let id = self.id.clone();
        settings::update(cx, |settings| {
            if let Some(path) = settings.find_path(&id) {
                if let Some(item) = settings.node_mut(&path) {
                    f(item);
                }
            }
        });
    }

    /// 当前条目（克隆一份给渲染用）。
    fn node(&self) -> Option<settings::ItemSetting> {
        settings::read(|settings| {
            settings
                .find_path(&self.id)
                .and_then(|path| settings.node(&path).cloned())
        })
    }

    /// 动作行与设置对齐：条数或条目变了就重建输入框。
    fn sync_rows(&mut self, cx: &mut Context<Self>) {
        let node = self.node();
        let actions = node
            .as_ref()
            .map(|item| item.actions.clone())
            .unwrap_or_default();
        let key = (self.id.clone(), actions.len());
        if self.rows_key == key {
            return;
        }

        self.rows.clear();
        self.param_inputs.clear();
        self.script_areas.clear();
        let mut extra_subs: Vec<Subscription> = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            let value =
                cx.new(|cx| TextInput::new(action.value.clone(), action.kind.hint(), 11.5, cx));
            let delay = cx.new(|cx| {
                TextInput::new(
                    if action.delay_ms == 0 {
                        String::new()
                    } else {
                        action.delay_ms.to_string()
                    },
                    "0",
                    11.5,
                    cx,
                )
            });

            let mut subs = Vec::new();
            {
                let entity = value.clone();
                let seen: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));
                subs.push(cx.observe(&entity, move |view, input, cx| {
                    let text = input.read(cx).text();
                    if seen.replace(Some(text.clone())).as_deref() == Some(text.as_str()) {
                        return;
                    }
                    view.set_action_value(index, text, cx);
                }));
            }
            {
                let entity = delay.clone();
                let seen: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));
                subs.push(cx.observe(&entity, move |view, input, cx| {
                    let text = input.read(cx).text();
                    if seen.replace(Some(text.clone())).as_deref() == Some(text.as_str()) {
                        return;
                    }
                    view.set_action_delay(index, text, cx);
                }));
            }

            self.rows.push(ActionRow {
                value,
                delay,
                _subs: subs,
            });

            // 内置动作的文本参数
            if action.kind == ActionKind::Builtin {
                if let Some(def) = crate::actions::find(&action.value) {
                    for param in def.params {
                        if !matches!(param.kind, crate::actions::ParamKind::Text) {
                            continue;
                        }
                        let initial = action
                            .params
                            .get(param.key)
                            .cloned()
                            .unwrap_or_else(|| param.default.to_string());
                        let input = cx.new(|cx| {
                            TextInput::new(initial, param.hint, 11.5, cx)
                        });
                        let seen: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));
                        let key_name = param.key.to_string();
                        {
                            let entity = input.clone();
                            extra_subs.push(cx.observe(&entity, move |view, input, cx| {
                                let text = input.read(cx).text();
                                if seen.replace(Some(text.clone())).as_deref()
                                    == Some(text.as_str())
                                {
                                    return;
                                }
                                view.set_action_param(index, &key_name, text, cx);
                            }));
                        }
                        self.param_inputs.insert((index, param.key.to_string()), input);
                    }
                }
            }

            // 脚本动作：一段代码用一个多行框写
            if action.kind == ActionKind::Script {
                let area = cx.new(|cx| {
                    crate::text_area::TextArea::new(
                        action.value.clone(),
                        crate::i18n::t("AHK 代码，例如：Send \"^a\""),
                        11.5,
                        cx,
                    )
                    .with_min_rows(5)
                });
                let seen: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));
                {
                    let entity = area.clone();
                    extra_subs.push(cx.observe(&entity, move |view, area, cx| {
                        let text = area.read(cx).text();
                        if seen.replace(Some(text.clone())).as_deref() == Some(text.as_str()) {
                            return;
                        }
                        view.set_action_value(index, text, cx);
                    }));
                }
                self.script_areas.insert(index, area);
            }
        }
        // 参数 / 脚本输入框的订阅挂在对应动作行上，行被重建时一起丢掉
        if let Some(row) = self.rows.first_mut() {
            row._subs.extend(extra_subs);
        }
        self.rows_key = key;
    }

    /// 显式提交：强制写盘、刷新所有窗口，并把回滚点推到当前状态。
    fn apply_changes(&mut self, cx: &mut Context<Self>) {
        settings::update(cx, |_| {});
        self.committed = settings::current();
        arm_close_guard(&self.committed);
        cx.notify();
    }

    /// 取消：整份配置回滚到打开窗口（或上次应用）时的快照。
    fn cancel_changes(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.committed.clone();
        settings::update(cx, |settings| *settings = snapshot);
        arm_close_guard(&self.committed);
        self.rows_key = (String::new(), usize::MAX);
        self.sync_rows(cx);
        cx.notify();
    }

    fn set_action_value(&mut self, index: usize, text: String, cx: &mut App) {
        self.apply_field(
            move |item| {
                if let Some(step) = item.actions.get_mut(index) {
                    step.value = text;
                }
            },
            cx,
        );
    }

    /// 改内置动作的某个参数。
    fn set_action_param(&mut self, index: usize, key: &str, text: String, cx: &mut App) {
        let key = key.to_string();
        self.apply_field(
            move |item| {
                if let Some(step) = item.actions.get_mut(index) {
                    step.params.insert(key, text);
                }
            },
            cx,
        );
    }

    /// 从动作库里换一条内置动作（顺带把它的默认参数填上）。
    fn set_builtin_id(&mut self, index: usize, id: &str, cx: &mut Context<Self>) {
        let Some(def) = crate::actions::find(id) else {
            return;
        };
        let id = def.id.to_string();
        let params = crate::actions::defaults(def);
        self.apply_field(
            move |item| {
                if let Some(step) = item.actions.get_mut(index) {
                    step.kind = ActionKind::Builtin;
                    step.value = id;
                    step.params = params;
                }
            },
            cx,
        );
        self.picker_for = None;
        self.rows_key = (String::new(), usize::MAX);
        self.sync_rows(cx);
        cx.notify();
    }

    /// 换脚本语言（v1 / v2）。
    fn set_script_lang(&mut self, index: usize, lang: crate::actions::ScriptLang, cx: &mut App) {
        let key = lang.key().to_string();
        self.apply_field(
            move |item| {
                if let Some(step) = item.actions.get_mut(index) {
                    step.params.insert("lang".to_string(), key);
                }
            },
            cx,
        );
    }

    fn set_action_delay(&mut self, index: usize, text: String, cx: &mut App) {
        let digits: String = text.chars().filter(|ch| ch.is_ascii_digit()).collect();
        let delay: u32 = digits.parse().unwrap_or(0).min(60_000);
        self.apply_field(
            move |item| {
                if let Some(step) = item.actions.get_mut(index) {
                    step.delay_ms = delay;
                }
            },
            cx,
        );
    }

    fn set_action_kind(&mut self, index: usize, kind: ActionKind, cx: &mut Context<Self>) {
        self.apply_field(
            move |item| {
                if let Some(step) = item.actions.get_mut(index) {
                    step.kind = kind;
                    // 内置动作必须有合法值，换类型时给个默认
                    if kind == ActionKind::Builtin && BuiltinAction::from_key(&step.value).is_none()
                    {
                        step.value = BuiltinAction::default().key().to_string();
                    }
                }
            },
            cx,
        );
        self.rows_key = (String::new(), usize::MAX);
        self.sync_rows(cx);
        cx.notify();
    }

    /// 内置动作的配置块：当前是哪一条 + 「换一个…」+ 它的参数。
    fn builtin_param_block(
        &self,
        index: usize,
        action: &ActionStep,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = crate::actions::find(&action.value)
            .map(|def| crate::i18n::t(def.name).to_string())
            .or_else(|| {
                BuiltinAction::from_key(&action.value).map(|builtin| builtin.label().to_string())
            })
            .unwrap_or_else(|| crate::i18n::t("（未选择）").to_string());

        let mut column = div().flex().flex_col().gap(px(4.)).flex_1().min_w_0();
        let mut head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(11.5))
                    .text_color(theme::text_primary())
                    .child(name),
            );
        head = head.child(ghost_button(
            if self.picker_for == Some(index) {
                "收起"
            } else {
                "换一个…"
            },
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                let opening = this.picker_for != Some(index);
                this.picker_for = if opening { Some(index) } else { None };
                if opening {
                    // 每次展开都从「全部 + 空搜索」开始，不带上一次的筛选
                    this.picker_category = None;
                    let query = this.picker_query.clone();
                    query.update(cx, |input, cx| input.set_text("", cx));
                    // 列表位置回到最上面（上次可能停在半中间）
                    this.picker_scroll
                        .set_offset(gpui::point(px(0.), px(0.)));
                    // 滚动条的几何（视口多高、能滚多远）要等这一帧布局完才有：
                    // 下一帧再刷一次，不然刚展开的列表要等到有别的动静才画出条来
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(40))
                            .await;
                        let _ = this.update(cx, |_, cx| cx.notify());
                    })
                    .detach();
                }
                cx.notify();
                window.refresh();
            }),
        ));
        column = column.child(head);
        // 快捷按钮：动作库一共 47 条，但日常真要用的就是这几个（打开设置 /
        // 重启 / 退出 / 复制选中内容 …）。每次都得点开「换一个…」再搜一遍太慢，
        // 所以这 9 个一直摆在这儿，一键换过去。
        column = column.child(self.builtin_quick_row(index, &action.value, cx));

        if self.picker_for == Some(index) {
            column = column.child(self.action_picker(index, &action.value, cx));
        } else if let Some(def) = crate::actions::find(&action.value) {
            for param in def.params {
                column = column.child(self.param_control(index, param, action, cx));
            }
        }
        column.into_any_element()
    }

    /// 「常用内置动作」那一排小按钮（当前选中那条高亮）。
    fn builtin_quick_row(
        &self,
        index: usize,
        current: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div().flex().flex_wrap().items_center().gap(px(3.));
        row = row.child(
            div()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .pr(px(2.))
                .child(crate::i18n::t("常用")),
        );
        for builtin in BuiltinAction::ALL {
            let key = builtin.key();
            let active = current == key;
            row = row.child(
                div()
                    .id(SharedString::from(format!("quick-{index}-{key}")))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(9.))
                    .text_size(px(10.))
                    .cursor_pointer()
                    .when(active, |style| {
                        style
                            .bg(theme::accent_soft())
                            .text_color(theme::accent())
                    })
                    .when(!active, |style| {
                        style
                            .bg(theme::track())
                            .text_color(theme::ui_text_secondary())
                            .hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_builtin_id(index, key, cx);
                        window.refresh();
                    }))
                    .child(builtin.label()),
            );
        }
        row.into_any_element()
    }

    /// 动作库列表（带搜索 + 分类筛选，点一条就换成它）。
    ///
    /// 以前是一整张 47 条的静态列表，找一条得滚半天；现在和「设置 → 动作库」
    /// 用同一套搜索口径（名字 / 说明 / id / 分类都能命中，多词是「且」）。
    fn action_picker(&self, index: usize, current: &str, cx: &mut Context<Self>) -> AnyElement {
        let query = self.picker_query.read(cx).text();
        let hits = crate::actions::search(query.trim());
        let selected_category = self.picker_category;

        let search = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(3.))
            .pb(px(3.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .rounded(px(5.))
                    .bg(theme::surface())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(self.picker_query.clone()),
            );

        // 分类胶囊：只列当前搜索结果里还有东西的分类（带计数）
        let mut chips = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(3.))
            .px(px(3.))
            .pb(px(4.));
        let mut choices: Vec<(Option<crate::actions::Category>, &'static str, usize)> =
            vec![(None, crate::i18n::t("全部"), hits.len())];
        for category in crate::actions::Category::ALL {
            let count = hits.iter().filter(|def| def.category == category).count();
            if count > 0 {
                choices.push((Some(category), category.label(), count));
            }
        }
        for (category, label, count) in choices {
            let selected = selected_category == category;
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("pk-{:?}", category)))
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .text_size(px(9.5))
                    .cursor_pointer()
                    .when(selected, |style| {
                        style.bg(theme::accent_soft()).text_color(theme::accent())
                    })
                    .when(!selected, |style| {
                        style
                            .bg(theme::track())
                            .text_color(theme::ui_text_secondary())
                            .hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.picker_category = category;
                        cx.notify();
                    }))
                    .child(format!("{label} {count}")),
            );
        }

        let mut list = div().flex().flex_col().gap(px(1.));
        let mut shown = 0usize;
        for def in hits {
            if selected_category.map(|cat| cat != def.category).unwrap_or(false) {
                continue;
            }
            shown += 1;
            let selected = def.id == current;
            list = list.child(
                div()
                    .id(SharedString::from(format!("action-{index}-{}", def.id)))
                    .px(px(5.))
                    .py(px(3.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(selected, |style| style.bg(theme::accent_soft()))
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_builtin_id(index, def.id, cx);
                        window.refresh();
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(if selected {
                                        theme::accent()
                                    } else {
                                        theme::text_primary()
                                    })
                                    .child(crate::i18n::t(def.name)),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme::ui_text_tertiary())
                                    .child(crate::i18n::t(def.desc)),
                            ),
                    ),
            );
        }
        if shown == 0 {
            list = list.child(
                div()
                    .p(px(8.))
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("没找到对应的动作")),
            );
        }

        // 列表 + 一根能拖的滚动条：47 条动作装不进 190px，
        // 只靠滚轮太憋，得能抓着一路拉下去。
        let list_box = div()
            .relative()
            .child(
                div()
                    .id(SharedString::from(format!("picker-list-{index}")))
                    .max_h(px(PICKER_LIST_H))
                    .pr(px(10.))
                    .overflow_y_scroll()
                    .track_scroll(&self.picker_scroll)
                    .child(list),
            )
            .child(self.column_bar(ColumnBar::Picker, cx))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.bar_drag.is_some() {
                    this.drag_bar_to(f32::from(event.position.y), cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.bar_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.bar_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            );

        div()
            .id(SharedString::from(format!("picker-{index}")))
            .flex()
            .flex_col()
            .rounded(px(4.))
            .bg(theme::surface_sunken())
            .p(px(4.))
            .child(search)
            .child(chips)
            .child(list_box)
            .into_any_element()
    }

    /// 一个参数的控件：文本 / 数字 / 单选 / 开关。
    fn param_control(
        &self,
        index: usize,
        param: &crate::actions::ParamDef,
        action: &ActionStep,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = action
            .params
            .get(param.key)
            .cloned()
            .unwrap_or_else(|| param.default.to_string());

        let control = match &param.kind {
            crate::actions::ParamKind::Text => {
                match self.param_inputs.get(&(index, param.key.to_string())) {
                    Some(input) => text_box(input, 0.),
                    None => div().into_any_element(),
                }
            }
            crate::actions::ParamKind::Int {
                min,
                max,
                step,
                unit,
            } => {
                let value: i32 = current.trim().parse().unwrap_or(*min);
                let (min, max, step) = (*min, *max, *step);
                let key_minus = param.key.to_string();
                let key_plus = param.key.to_string();
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(small_button(
                        "−",
                        format!("pm-{index}-{}", param.key),
                        cx,
                        move |this, window, cx| {
                            let value = this
                                .action_at(index)
                                .and_then(|step| step.param(&key_minus).map(str::to_string))
                                .and_then(|text| text.parse::<i32>().ok())
                                .unwrap_or(min);
                            this.set_action_param(
                                index,
                                &key_minus,
                                (value - step).clamp(min, max).to_string(),
                                cx,
                            );
                            cx.notify();
                            window.refresh();
                        },
                    ))
                    .child(
                        div()
                            .min_w(px(44.))
                            .text_size(px(11.))
                            .text_color(theme::text_primary())
                            .child(format!("{value}{unit}")),
                    )
                    .child(small_button(
                        "+",
                        format!("pp-{index}-{}", param.key),
                        cx,
                        move |this, window, cx| {
                            let value = this
                                .action_at(index)
                                .and_then(|step| step.param(&key_plus).map(str::to_string))
                                .and_then(|text| text.parse::<i32>().ok())
                                .unwrap_or(min);
                            this.set_action_param(
                                index,
                                &key_plus,
                                (value + step).clamp(min, max).to_string(),
                                cx,
                            );
                            cx.notify();
                            window.refresh();
                        },
                    ))
                    .into_any_element()
            }
            crate::actions::ParamKind::Choice(options) => {
                let mut chips = div().flex().flex_wrap().items_center().gap(px(3.));
                for (value, label) in options.iter() {
                    let selected = current == *value;
                    let key = param.key.to_string();
                    let value = value.to_string();
                    chips = chips.child(
                        div()
                            .id(SharedString::from(format!(
                                "pc-{index}-{}-{value}",
                                param.key
                            )))
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .text_size(px(10.5))
                            .cursor_pointer()
                            .when(selected, |style| {
                                style.bg(theme::accent_soft()).text_color(theme::accent())
                            })
                            .when(!selected, |style| {
                                style
                                    .bg(theme::track())
                                    .text_color(theme::ui_text_secondary())
                                    .hover(|style| style.bg(theme::surface_hover()))
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.set_action_param(index, &key, value.clone(), cx);
                                cx.notify();
                                window.refresh();
                            }))
                            .child(crate::i18n::t(label)),
                    );
                }
                chips.into_any_element()
            }
            crate::actions::ParamKind::Bool => {
                let on = matches!(current.as_str(), "1" | "true" | "on");
                let key = param.key.to_string();
                div()
                    .id(SharedString::from(format!("pb-{index}-{}", param.key)))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .when(on, |style| style.bg(theme::accent_soft()).text_color(theme::accent()))
                    .when(!on, |style| {
                        style
                            .bg(theme::track())
                            .text_color(theme::ui_text_secondary())
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let on = this
                            .action_at(index)
                            .and_then(|step| step.param(&key).map(str::to_string))
                            .map(|text| matches!(text.as_str(), "1" | "true" | "on"))
                            .unwrap_or(false);
                        this.set_action_param(
                            index,
                            &key,
                            if on { "0" } else { "1" }.to_string(),
                            cx,
                        );
                        cx.notify();
                        window.refresh();
                    }))
                    .child(if on { "开" } else { "关" })
                    .into_any_element()
            }
        };

        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(param.label)),
            )
            .child(div().flex_1().min_w_0().child(control))
            .into_any_element()
    }

    /// 脚本动作：语言切换 + 一大段代码。
    fn script_block(
        &self,
        index: usize,
        action: &ActionStep,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lang = crate::actions::ScriptLang::from_key(action.param("lang").unwrap_or("v1"));
        let mut langs = div().flex().items_center().gap(px(3.));
        for choice in crate::actions::ScriptLang::ALL {
            let selected = choice == lang;
            langs = langs.child(
                div()
                    .id(SharedString::from(format!("lang-{index}-{}", choice.key())))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .when(selected, |style| {
                        style.bg(theme::accent_soft()).text_color(theme::accent())
                    })
                    .when(!selected, |style| {
                        style
                            .bg(theme::track())
                            .text_color(theme::ui_text_secondary())
                            .hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_script_lang(index, choice, cx);
                        cx.notify();
                        window.refresh();
                    }))
                    .child(choice.label()),
            );
        }

        let mut column = div().flex().flex_col().gap(px(4.)).flex_1().min_w_0();
        column = column.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(langs)
                .child(
                    div()
                        .flex_1()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("解释器在「设置 → 动作库」里指定，留空就自动找")),
                ),
        );
        if let Some(area) = self.script_areas.get(&index).cloned() {
            column = column.child(
                div()
                    .w_full()
                    .rounded(px(6.))
                    .bg(theme::surface_sunken())
                    .border_1()
                    .border_color(theme::border_strong())
                    .p(px(4.))
                    .child(area.clone()),
            );
            // 直接选一个 .ahk 文件：写进上面的框里，运行时按「脚本文件」跑
            // （文件在 Plugins\ahkv1|ahkv2 里的，顺手把版本切对）
            let pick_area = area.clone();
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(crate::settings_ui::ghost_button_tagged(
                        format!("script-file-{index}"),
                        "选 .ahk 文件…",
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            let Some(path) = crate::win32::pick_script_file() else {
                                return;
                            };
                            if let Some((_, Some(lang))) = crate::actions::resolve_script(&path) {
                                this.set_script_lang(index, lang, cx);
                            }
                            pick_area.update(cx, |area, cx| area.set_text(path, cx));
                            window.refresh();
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t(
                                "框里只写文件名也行：Plugins\\ahkv1 / ahkv2 里的脚本能直接按名字跑；脚本里的 %getZz% 会换成选中内容",
                            )),
                    ),
            );
        }
        column.into_any_element()
    }

    /// 第 `index` 条动作（只读）。
    /// 打开「运行选项」面板（管理员 / 已在运行的处置 / 工作目录 / 窗口状态）。
    fn open_run_panel(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(step) = self.action_at(index) else {
            return;
        };
        let options = crate::runopts::Options::from_params(&step.params);
        let args = cx.new(|cx| {
            TextInput::new(
                options.args.clone(),
                crate::i18n::t("例如 --fast 或一份文档的完整路径"),
                11.5,
                cx,
            )
        });
        let workdir = cx.new(|cx| {
            TextInput::new(
                options.workdir.clone().unwrap_or_default(),
                crate::i18n::t("留空 = 程序自己所在目录"),
                11.5,
                cx,
            )
        });
        let keys = cx.new(|cx| {
            TextInput::new(
                options.on_run_keys.clone(),
                crate::i18n::t("例如 Ctrl+S；激活窗口之后再发"),
                11.5,
                cx,
            )
        });
        self.run_panel = Some(RunPanel {
            index,
            options,
            args,
            workdir,
            keys,
        });
        cx.notify();
    }

    /// 「运行选项」面板（浮在整个编辑窗口上面）。
    fn run_panel_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.run_panel.as_ref()?;
        let program = self
            .action_at(panel.index)
            .map(|step| step.value)
            .unwrap_or_default();
        let options = panel.options.clone();

        // 一行小胶囊（窗口状态 / 已在运行时怎么办）
        let chip = |id: String, label: String, selected: bool| -> gpui::Stateful<gpui::Div> {
            div()
                .id(SharedString::from(id))
                .px(px(8.))
                .py(px(3.))
                .rounded(px(4.))
                .text_size(px(10.5))
                .cursor_pointer()
                .when(selected, |style| {
                    style.bg(theme::accent_soft()).text_color(theme::accent())
                })
                .when(!selected, |style| {
                    style
                        .bg(theme::track())
                        .text_color(theme::ui_text_secondary())
                        .hover(|style| style.bg(theme::surface_hover()))
                })
                .child(label)
        };

        let mut window_chips = div().flex().items_center().gap(px(4.));
        for state in WindowState::ALL {
            let selected = options.window == state;
            window_chips = window_chips.child(
                chip(
                    format!("run-window-{}", state.key()),
                    state.label().to_string(),
                    selected,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if let Some(panel) = this.run_panel.as_mut() {
                        panel.options.window = state;
                    }
                    window.refresh();
                    cx.notify();
                })),
            );
        }

        let mut onrun_chips = div().flex().flex_wrap().items_center().gap(px(4.));
        for mode in OnRun::ALL {
            let selected = options.on_run == mode;
            onrun_chips = onrun_chips.child(
                chip(
                    format!("run-onrun-{}", mode.key()),
                    mode.label().to_string(),
                    selected,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if let Some(panel) = this.run_panel.as_mut() {
                        panel.options.on_run = mode;
                    }
                    window.refresh();
                    cx.notify();
                })),
            );
        }

        let admin_on = options.as_admin;
        let admin_row = div()
            .id("run-admin")
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                if let Some(panel) = this.run_panel.as_mut() {
                    panel.options.as_admin = !panel.options.as_admin;
                }
                window.refresh();
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::text_primary())
                            .child(crate::i18n::t("以管理员身份运行")),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("会弹一次 UAC；程序本身没开「以管理员运行」也能靠这一项临时提权")),
                    ),
            )
            .child(switch(admin_on));

        let input_box = |input: &Entity<TextInput>| -> AnyElement {
            div()
                .w_full()
                .h(px(26.))
                .px(px(6.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .bg(theme::surface_sunken())
                .border_1()
                .border_color(theme::border_strong())
                .child(input.clone())
                .into_any_element()
        };

        let mut card = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .w(px(520.))
            .max_h(px(520.))
            .p(px(14.))
            .rounded(px(theme::RADIUS_CARD))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border_strong())
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(crate::i18n::t("运行选项")),
                    )
                    .child(
                        div()
                            .id("run-close")
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .text_size(px(11.))
                            .text_color(theme::ui_text_tertiary())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .child("✕")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.run_panel = None;
                                window.refresh();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .truncate()
                    .child(if program.trim().is_empty() {
                        crate::i18n::t("程序还没填：先在动作那一行写上 notepad.exe 这种").to_string()
                    } else {
                        program.clone()
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(label_small("参数"))
                    .child(input_box(&panel.args)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(label_small("工作文件夹"))
                    .child(input_box(&panel.workdir)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(label_small("窗口状态"))
                    .child(window_chips),
            )
            .child(admin_row)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(label_small("已经在运行时"))
                    .child(onrun_chips),
            );

        if options.on_run == OnRun::Keys {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(label_small("激活后发送的按键"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(div().flex_1().min_w_0().child(input_box(&panel.keys)))
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(26.))
                                    .flex()
                                    .items_center()
                                    .child(crate::keypad::open_button(
                                        "run-keys-pad",
                                        &panel.keys,
                                        true,
                                        // 发送按键是一串序列，没有「左右修饰键」这回事
                                        None,
                                    )),
                            ),
                    ),
            );
        }

        let submit = cx.listener(|this, _: &ClickEvent, window, cx| {
            let Some(panel) = this.run_panel.take() else {
                return;
            };
            let mut options = panel.options;
            options.args = panel.args.read(cx).text();
            options.workdir = Some(panel.workdir.read(cx).text())
                .map(|dir| dir.trim().to_string())
                .filter(|dir| !dir.is_empty());
            options.on_run_keys = panel.keys.read(cx).text();
            let index = panel.index;
            this.apply_field(
                move |item| {
                    if let Some(step) = item.actions.get_mut(index) {
                        options.write(&mut step.params);
                    }
                },
                cx,
            );
            window.refresh();
            cx.notify();
        });

        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .pt(px(2.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("留空 / 默认就是原来的行为：再启动一次")),
                )
                .when(!options.is_default(), |row| {
                    row.child(ghost_button(
                        crate::i18n::t("恢复默认"),
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            let Some(panel) = this.run_panel.as_mut() else {
                                return;
                            };
                            panel.options.reset();
                            panel.args.update(cx, |input, cx| input.set_text("", cx));
                            panel.workdir.update(cx, |input, cx| input.set_text("", cx));
                            panel.keys.update(cx, |input, cx| input.set_text("", cx));
                            window.refresh();
                            cx.notify();
                        }),
                    ))
                })
                .child(ghost_button(
                    crate::i18n::t("取消"),
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.run_panel = None;
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .child(primary_button(crate::i18n::t("确定"), submit)),
        );

        Some(
            div()
                .absolute()
                .left_0()
                .top_0()
                .size_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x000000aa))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.run_panel = None;
                        window.refresh();
                        cx.notify();
                    }),
                )
                .child(card)
                .into_any_element(),
        )
    }

    fn action_at(&self, index: usize) -> Option<ActionStep> {
        self.node()
            .and_then(|item| item.actions.get(index).cloned())
    }

    /// 把拖进窗口的文件变成动作：一个文件一条「运行程序」，参数写完整路径。
    ///
    /// 为什么这里不问「完整路径 / 裸文件名」：这个窗口是**专门配一条菜单项**的地方，
    /// 拖进来的就是那一个程序，写全路径最不容易出错；想改成裸名字直接在参数框里删。
    /// （列表那边的拖入才需要问，因为一次可能拖进来一堆。）
    fn accept_dropped(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let mut seen = std::collections::HashSet::new();
        let paths: Vec<std::path::PathBuf> = paths
            .into_iter()
            .filter(|path| path.exists() && seen.insert(path.clone()))
            .collect();
        let Some(first) = paths.first().cloned() else {
            return;
        };

        // 名称还空着就顺手填上（文件名去掉后缀），省得再手打一遍
        if self.label.read(cx).text().trim().is_empty() {
            let name = crate::tree_view::drop_label(&first);
            self.label.update(cx, |input, cx| input.set_text(name, cx));
        }

        self.apply_field(
            move |item| {
                for (index, path) in paths.iter().enumerate() {
                    let program = path.display().to_string();
                    if index == 0 {
                        // 已经有「运行程序」就替换参数，没有就补一条
                        match item
                            .actions
                            .iter_mut()
                            .find(|step| step.kind == ActionKind::Run)
                        {
                            Some(step) => step.value = program,
                            None => item
                                .actions
                                .push(ActionStep::new(ActionKind::Run, program)),
                        }
                    } else {
                        item.actions
                            .push(ActionStep::new(ActionKind::Run, program));
                    }
                }
            },
            cx,
        );
        // 动作条数变了：强制重建输入框，否则界面上还是旧的参数
        self.rows_key = (String::new(), usize::MAX);
        self.sync_rows(cx);
        cx.notify();
    }

    fn add_action(&mut self, cx: &mut Context<Self>) {
        self.apply_field(
            |item| {
                item.actions.push(ActionStep::default());
            },
            cx,
        );
        self.rows_key = (String::new(), usize::MAX);
        self.sync_rows(cx);
        cx.notify();
    }

    fn remove_action(&mut self, index: usize, cx: &mut Context<Self>) {
        self.apply_field(
            move |item| {
                if index < item.actions.len() {
                    item.actions.remove(index);
                }
            },
            cx,
        );
        self.rows_key = (String::new(), usize::MAX);
        self.sync_rows(cx);
        cx.notify();
    }

    fn move_action(&mut self, index: usize, delta: i32, cx: &mut Context<Self>) {
        self.apply_field(
            move |item| {
                let target = index as i32 + delta;
                if target >= 0 && (target as usize) < item.actions.len() {
                    item.actions.swap(index, target as usize);
                }
            },
            cx,
        );
        self.rows_key = (String::new(), usize::MAX);
        self.sync_rows(cx);
        cx.notify();
    }
}

/// 监听输入框，值真的变了才回调（避免预置文本触发一次写盘）。
fn observe_field<T: 'static>(
    input: &Entity<TextInput>,
    cx: &mut Context<T>,
    f: impl Fn(&mut T, String, &mut Context<T>) + 'static,
) -> Subscription {
    let entity = input.clone();
    let seen: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));
    cx.observe(&entity, move |view, input, cx| {
        let text = input.read(cx).text();
        if seen.replace(Some(text.clone())).as_deref() == Some(text.as_str()) {
            return;
        }
        f(view, text, cx);
    })
}

fn read_fields(id: &str) -> (String, String, String, String, String) {
    settings::read(|settings| {
        let Some(item) = settings.find_path(id).and_then(|path| settings.node(&path)) else {
            return (
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            );
        };
        (
            item.label.clone(),
            item.badge.clone(),
            item.accel.clone(),
            item.shortcut.clone(),
            item.hotkey.clone(),
        )
    })
}

/// 备注（单独读，免得把上面那个五元组改得到处都是）。
fn read_note(id: &str) -> String {
    settings::read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .map(|item| item.note.clone())
            .unwrap_or_default()
    })
}

/// 热字符串（也单独读）。
fn read_hotstring(id: &str) -> String {
    settings::read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .map(|item| item.hotstring.clone())
            .unwrap_or_default()
    })
}

/// 任意一个字符串字段（按键面板 / 局部快捷键这些新字段用，懒得为每个都写一遍）。
fn read_str(id: &str, f: impl Fn(&settings::ItemSetting) -> String) -> String {
    settings::read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .map(|item| f(item))
            .unwrap_or_default()
    })
}

thread_local! {
    /// 调试钩子只自动开一次「运行选项」面板（`GPUI_MENU_DEBUG_RUNPANEL`）
    static DEBUG_RUN_SHOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Render for ItemEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 调试用：`GPUI_MENU_DEBUG_RUNPANEL=1` 进窗口就把第一条「运行程序」的选项面板摆出来
        if std::env::var_os("GPUI_MENU_DEBUG_RUNPANEL").is_some()
            && self.run_panel.is_none()
            && !DEBUG_RUN_SHOT.with(|flag| flag.get())
            && matches!(self.action_at(0).map(|step| step.kind), Some(ActionKind::Run))
        {
            DEBUG_RUN_SHOT.with(|flag| flag.set(true));
            // 面板是浮层，但留个口子给「以后改成独立窗口」：走 spawn，不在 render 里动窗口
            self.open_run_panel(0, cx);
        }
        // 调试用：进窗口就弹出「名称」输入框的右键菜单（这台机器上模拟右键不可靠）
        if self.debug_textmenu_pending {
            self.debug_textmenu_pending = false;
            crate::text_menu::open(
                crate::text_menu::Target::Input(self.label.clone()),
                window,
                gpui::point(px(140.), px(180.)),
                cx,
            );
        }
        // 调试用：`GPUI_MENU_DEBUG_KEYPAD=1` 进来就把按键面板弹在被编辑项的
        // 「触发快捷键」上（这台机器上模拟点击不可靠，看版式用）
        if std::env::var_os("GPUI_MENU_DEBUG_KEYPAD").is_some()
            && !crate::keypad::any_open(cx)
            && !crate::keypad::debug_shot_taken()
        {
            crate::keypad::debug_mark_shot();
            crate::keypad::open(
                self.hotkey.clone(),
                false,
                Some(crate::keypad::SidedTarget::Item(self.id.clone())),
                cx,
            );
        }
        // 调试用：`GPUI_MENU_DEBUG_WHEN_OPEN=1` 进来就把「生效条件」窗口摆出来。
        // **不能在 render 里直接开窗**：gpui 这会儿正在建这一帧的元素树，
        // 再开一扇窗会把进程带崩（按键面板那个调试钩子就是这么崩的）。
        if std::env::var_os("GPUI_MENU_DEBUG_WHEN_OPEN").is_some()
            && !DEBUG_WHEN_SHOT.with(|flag| flag.get())
        {
            DEBUG_WHEN_SHOT.with(|flag| flag.set(true));
            let id = self.id.clone();
            cx.spawn(async move |_this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(120))
                    .await;
                let _ = cx.update(|app| crate::when_ui::open(&id, app));
            })
            .detach();
        }
        let maximized = self.chrome.maximized();
        let chrome = self.chrome.clone();
        let chrome_max = self.chrome.clone();

        let Some(node) = self.node() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::surface_window())
                .text_color(theme::ui_text_tertiary())
                .text_size(px(12.))
                .child(crate::i18n::t("这个菜单项已经被删掉了"));
        };
        self.sync_rows(cx);

        let title = if node.label.trim().is_empty() {
            crate::i18n::t("未命名").to_string()
        } else {
            // 标题只是「正在编辑哪一条」的提示，可以跟着语言走；
            // 下面的「名称」输入框里仍是存盘的原文，改的是真值。
            crate::i18n::t_dynamic(&node.label).to_string()
        };
        let is_submenu = node.kind == ItemKind::Submenu;

        let header = div()
            .id("editor-header")
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .px(px(6.))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                chrome::drag_area("editor-titlebar", &chrome)
                    .flex_1()
                    .min_w_0()
                    .child(chrome::title_row(true, crate::i18n::t("编辑菜单项"))),
            )
            .child(
                div()
                    .max_w(px(220.))
                    .flex_none()
                    .pr(px(6.))
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .truncate()
                    .child(title),
            )
            .child(chrome::caption_button(
                "—",
                "editor-min",
                |_event, window, _cx| {
                    window.minimize_window();
                },
            ))
            .child(chrome::caption_button(
                if maximized { "❐" } else { "□" },
                "editor-max",
                move |_event, window, _cx| chrome_max.toggle_maximize(window),
            ))
            // ✕ = 取消：有未应用的改动就先回滚再关窗，避免「点关闭却把改动留下了」
            .child(chrome::caption_button_action(
                "✕",
                "editor-close",
                cx,
                |this, _: &ClickEvent, window, cx| {
                    this.cancel_changes(cx);
                    window.remove_window();
                },
            ));

        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(10.))
            .p(px(12.))
            .child(self.basic_column(&node, cx))
            .child(self.action_column(&node, is_submenu, cx));

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::surface_window())
            .text_color(theme::text_primary())
            // 和设置窗口、菜单用同一款字体
            .font_family(theme::font_family())
            .track_focus(&self.focus)
            // Esc 先关输入框的右键菜单（有的话）
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if !matches!(event.keystroke.key.as_str(), "escape" | "esc") {
                        return;
                    }
                    if AUTOFILL_ARM.with(|slot| slot.get()).is_some() {
                        AUTOFILL_ARM.with(|slot| slot.set(None));
                        window.refresh();
                    } else if this.run_panel.is_some() {
                        this.run_panel = None;
                        window.refresh();
                    } else if crate::keypad::any_open(cx) {
                        crate::keypad::close(cx);
                        window.refresh();
                    } else if crate::text_menu::any_open(cx) {
                        crate::text_menu::close(cx);
                    }
                }),
            )
            // 从资源管理器把程序拖进来 = 给它加一条「运行程序」动作
            // （右侧子项列表自己有拖入处理，落在那里就走那边的逻辑）
            .on_drop::<ExternalPaths>(cx.listener(
                |this, paths: &ExternalPaths, _window, cx| {
                    this.accept_dropped(paths.paths().to_vec(), cx);
                },
            ))
            .child(header)
            .child(body)
            .child(self.footer(cx))
            // 「运行选项」是浮层，然后是输入框右键菜单
            .children(self.run_panel_layer(cx))
            .children(self.autofill_confirm_layer(cx))
            .children(crate::text_menu::layer(window, cx))
    }
}

impl ItemEditor {
    /// 左栏：只放“显示相关”的东西。
    fn basic_column(&self, node: &settings::ItemSetting, cx: &mut Context<Self>) -> AnyElement {
        let is_submenu = node.kind == ItemKind::Submenu;
        let path = settings::read(|settings| settings.find_path(&self.id));
        let is_root = path.as_ref().map(|p| p.len() == 1).unwrap_or(true);
        let child_count = node.children.len();

        // 字段越来越多，左栏改成可滚动：窗口矮的时候不至于把下半截截掉
        let rows = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title(crate::i18n::t("基本信息"), crate::i18n::t("只影响显示，不涉及动作")))
            .when(settings::is_tray(&self.id), |column| {
                column.child(
                    div()
                        .px(px(7.))
                        .py(px(5.))
                        .rounded(px(6.))
                        .bg(theme::accent_soft())
                        .text_size(px(9.5))
                        .text_color(theme::accent())
                        .child(crate::i18n::t("这一支只出现在托盘图标的右键菜单里，不会进弹出菜单；里面的子项就是托盘菜单的内容")),
                )
            })
            .child(field(crate::i18n::t("名称"), text_box(&self.label, 0.)))
            .child(field(crate::i18n::t("备注"), text_box(&self.note, 0.)))
            // 图标位：一个字符/emoji，或者一个图片文件（顺便能给个「选文件」）
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("图标位")),
                    )
                    .child(
                        div()
                            .id("editor-icon-row")
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            // 整行都能接图片：输入框上、方块上、按钮之间的空隙都算
                            // （方块自己有更靠里的一份，两个 handler 结果一样）
                            .on_drop::<ExternalPaths>(cx.listener(
                                |this, paths: &ExternalPaths, window, cx| {
                                    this.drop_icon(paths.paths(), window, cx);
                                },
                            ))
                            // 输入框吃满剩下来的宽度：`text_box` 在宽度 0 时是 `w_full`，
                            // 不套一层 flex_1 会把后面的按钮挤成 0 宽（原来就踩了这个坑）
                            .child(div().flex_1().min_w_0().child(text_box(&self.badge, 0.)))
                            // 当前图标长什么样，直接在这儿看（跟菜单里一样大）。
                            // 把图片拖到这个小方块上 = 换图标（会自动收进 Icon/ 目录）。
                            .child({
                                let glyph = settings::read(|settings| settings.badge_size)
                                    .clamp(12.0, 20.0);
                                div()
                                    .id("editor-icon-drop")
                                    .size(px(glyph + 8.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(5.))
                                    .bg(theme::surface_sunken())
                                    .border_1()
                                    .border_color(theme::border())
                                    .on_drop::<ExternalPaths>(cx.listener(
                                        |this, paths: &ExternalPaths, window, cx| {
                                            this.drop_icon(paths.paths(), window, cx);
                                        },
                                    ))
                                    .child(crate::icon::render(
                                        &preview_icon(node),
                                        glyph,
                                        theme::ui_text_secondary(),
                                    ))
                            })
                            .child(div().flex_none().child(small_button(
                                crate::i18n::t("选文件"),
                                "editor-icon-file".to_string(),
                                cx,
                                move |this, window, cx| {
                                    // 系统文件对话框是模态的，关掉之后才回来改设置
                                    let Some(path) = crate::win32::pick_image_file() else {
                                        return;
                                    };
                                    this.set_badge_file(&path, window, cx);
                                },
                            )))
                            .child(div().flex_none().child(small_button(
                                "✕",
                                "editor-icon-clear".to_string(),
                                cx,
                                move |this, window, cx| {
                                    this.apply_field(|item| item.badge.clear(), cx);
                                    window.refresh();
                                    cx.notify();
                                },
                            ))),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t(
                                "字符 / emoji，或 .svg .ico .png 路径。选文件、把图片直接拖到这整行（输入框或右边的方块）都行：图片会复制进程序目录的 Icon/badges，配置里只记相对路径，整个文件夹拷走图标也不断",
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(field(crate::i18n::t("加速键"), text_box(&self.accel, 52.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(field(crate::i18n::t("快捷键提示"), text_box(&self.shortcut, 0.))),
                    ),
            )
            // 触发快捷键：和上面那个「提示」不一样，它是真的注册到系统上的
            .child({
                let trigger = node.hotkey.trim().to_string();
                let invalid = crate::hotkey::reason(&trigger);
                let valid = invalid.is_none();
                // 同一个键被别处占了：这时候填了也不生效，得当场说清楚
                let taken = if trigger.is_empty() {
                    None
                } else {
                    crate::hotkey::taken_by(&trigger, Some(&self.id), node.hotkey_sided, "")
                };
                let hint = if trigger.is_empty() {
                    crate::i18n::t("留空 = 只能从菜单里点；点「键盘」点出组合键，或者自己填（例如 Ctrl+Alt+1）")
                        .to_string()
                } else if let Some(problem) = invalid {
                    problem.to_string()
                } else if let Some(who) = taken.clone() {
                    if crate::i18n::is_english() {
                        format!("Already used by \"{who}\" - only the first one fires; pick another combo")
                    } else {
                        format!("这个键已经被「{who}」占用了：同一条只有先设置的生效，换一个吧")
                    }
                } else if is_submenu {
                    crate::i18n::t("不管在哪个程序里按下，都会先抓选中内容、再只弹这一支").to_string()
                } else {
                    crate::i18n::t("不管在哪个程序里按下，都会先抓选中内容、再直接执行动作").to_string()
                };
                // 标题单独一行，下面一行是「输入框 + 录入 + ✕」：
                // 按钮和输入框同高、同一条中线，才不会一个高一个低
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("触发快捷键（全局，哪儿都生效）")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().child(text_box(&self.hotkey, 0.)))
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(24.))
                                    .flex()
                                    .items_center()
                                    .child(crate::keypad::open_button(
                                        "editor-hotkey-pad",
                                        &self.hotkey,
                                        false,
                                        Some(crate::keypad::SidedTarget::Item(
                                            self.id.clone(),
                                        )),
                                    )),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(24.))
                                    .flex()
                                    .items_center()
                                    .child(small_button(
                                        "✕",
                                        "editor-hotkey-clear".to_string(),
                                        cx,
                                        move |this, window, cx| {
                                            this.apply_field(|item| item.hotkey.clear(), cx);
                                            // 只管 settings 是不够的：输入框是独立的实体，
                                            // 不一起清掉的话拍完 ✕ 里头的字还挂着，
                                            // 下一次回车/失焦又会被写回 settings
                                            this.hotkey.update(cx, |input, cx| input.set_text("", cx));
                                            window.refresh();
                                            cx.notify();
                                        },
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(if valid && taken.is_none() {
                                theme::ui_text_tertiary()
                            } else {
                                theme::danger()
                            })
                            .child(hint),
                    )
            })
            // 局部快捷键：另一颗**独立**的键，只在满足条件的窗口里触发（AHK 的 #If）
            .child({
                let trigger = node.hotkey_local.trim().to_string();
                let when = node.hotkey_local_when.trim().to_string();
                let invalid = crate::hotkey::reason(&trigger);
                let valid = invalid.is_none();
                let taken = if trigger.is_empty() {
                    None
                } else {
                    // 局部键只有在「条件也一样」时才算撞（见 hotkey::same_scope）
                    crate::hotkey::taken_by(
                        &trigger,
                        Some(&self.id),
                        node.hotkey_local_sided,
                        &when,
                    )
                };
                let problem = crate::when::problem(&when);
                // 一行一条；注释和空行不算
                let count = when
                    .lines()
                    .filter(|line| {
                        let line = line.trim();
                        !line.is_empty() && !line.starts_with('#') && !line.starts_with("//")
                    })
                    .count();
                let hint = if trigger.is_empty() {
                    crate::i18n::t("留空 = 不配。填了就是第二颗键：只在下面这些条件满足时才开始抢")
                        .to_string()
                } else if let Some(problem) = invalid {
                    problem.to_string()
                } else if let Some(who) = taken.clone() {
                    if crate::i18n::is_english() {
                        format!("Already used by \"{who}\" - only the first one fires")
                    } else {
                        format!("这个键已经被「{who}」占用了：同一条只有先设置的生效")
                    }
                } else if when.is_empty() {
                    crate::i18n::t("还没写条件 → 和上面那颗一样到处生效（想做「只在 QQ 里」就点右边加条件）")
                        .to_string()
                } else if let Some(ref problem) = problem {
                    problem.clone()
                } else {
                    let first = when.lines().next().unwrap_or("").trim().to_string();
                    format!(
                        "{} · {} {}：{}",
                        crate::i18n::t("只在这些窗口里触发"),
                        count,
                        crate::i18n::t("条规则"),
                        first
                    )
                };
                let id = self.id.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(10.5))
                                    .text_color(theme::ui_text_tertiary())
                                    .child(crate::i18n::t("局部快捷键（只在某些窗口里生效）")),
                            )
                            .child(
                                div()
                                    .id("editor-when-open")
                                    .flex_none()
                                    .h(px(22.))
                                    .px(px(8.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(4.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(theme::border_strong())
                                    .bg(theme::surface_elevated())
                                    .text_size(px(10.5))
                                    .text_color(theme::ui_text_secondary())
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_hover()))
                                    .child("⊞")
                                    .child(crate::i18n::t("生效条件"))
                                    .on_click(move |_event, window, cx| {
                                        crate::when_ui::open(&id, cx);
                                        window.refresh();
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().child(text_box(&self.hotkey_local, 0.)))
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(24.))
                                    .flex()
                                    .items_center()
                                    .child(crate::keypad::open_button(
                                        "editor-local-pad",
                                        &self.hotkey_local,
                                        false,
                                        Some(crate::keypad::SidedTarget::ItemLocal(
                                            self.id.clone(),
                                        )),
                                    )),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(24.))
                                    .flex()
                                    .items_center()
                                    .child(small_button(
                                        "✕",
                                        "editor-local-clear".to_string(),
                                        cx,
                                        move |this, window, cx| {
                                            this.apply_field(
                                                |item| item.hotkey_local.clear(),
                                                cx,
                                            );
                                            // 同上：光清 settings 输入框里还留着旧字
                                            this.hotkey_local
                                                .update(cx, |input, cx| input.set_text("", cx));
                                            window.refresh();
                                            cx.notify();
                                        },
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(if valid && taken.is_none() && problem.is_none() {
                                theme::ui_text_tertiary()
                            } else {
                                theme::danger()
                            })
                            .child(hint),
                    )
            })
            // 热字符串：在别的程序里「打字」触发的第三种入口
            .child({
                let trigger = node.hotstring.trim().to_string();
                let now = node.hotstring_now;
                let hint = if trigger.is_empty() {
                    crate::i18n::t("留空 = 不参与。填上之后在别的程序里敲这串字、再敲空格 / 回车 / Tab 就执行这一项")
                        .to_string()
                } else if crate::i18n::is_english() {
                    if is_submenu && now {
                        format!("Typing \"{trigger}\" opens this branch right away (no space needed)")
                    } else if is_submenu {
                        format!("Typing \"{trigger}\" then space / Enter / Tab opens only this branch")
                    } else if now {
                        format!("Typing \"{trigger}\" runs the action right away (AHK's * option)")
                    } else {
                        format!("Typing \"{trigger}\" then space / Enter / Tab runs the action")
                    }
                } else if is_submenu && now {
                    format!("敲完「{trigger}」立刻弹出这一支（不用再敲空格）")
                } else if is_submenu {
                    format!("敲完「{trigger}」再敲空格 / 回车 / Tab，只弹出这一支")
                } else if now {
                    format!("敲完「{trigger}」立刻执行这一项的动作（AHK 的 * 选项，不用再敲空格）")
                } else {
                    format!("敲完「{trigger}」再敲空格 / 回车 / Tab，直接执行这一项的动作")
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("热字符串（敲一串字就执行）")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(text_box(&self.hotstring, 0.)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(24.))
                                    .flex()
                                    .items_center()
                                    .child(small_button(
                                        "✕",
                                        "editor-hotstring-clear".to_string(),
                                        cx,
                                        move |this, window, cx| {
                                            this.apply_field(|item| item.hotstring.clear(), cx);
                                            this.hotstring
                                                .update(cx, |input, cx| input.set_text("", cx));
                                            window.refresh();
                                            cx.notify();
                                        },
                                    )),
                            )
                            // AHK 的 `*`：不等结束符，一匹配上就执行
                            .child(
                                div()
                                    .id("editor-hotstring-now")
                                    .flex_none()
                                    .h(px(24.))
                                    .px(px(7.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(5.))
                                    .border_1()
                                    .cursor_pointer()
                                    .text_size(px(9.5))
                                    .when(now, |style| {
                                        style
                                            .bg(theme::accent_soft())
                                            .border_color(theme::accent())
                                            .text_color(theme::accent())
                                    })
                                    .when(!now, |style| {
                                        style
                                            .border_color(theme::border_strong())
                                            .text_color(theme::ui_text_tertiary())
                                            .hover(|style| style.bg(theme::surface_hover()))
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        let next = !now;
                                        this.apply_field(move |item| item.hotstring_now = next, cx);
                                        window.refresh();
                                        cx.notify();
                                    }))
                                    .child(crate::i18n::t("不等结束符")),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(hint),
                    )
            })
            // 在菜单里显示
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.5))
                            .text_color(theme::ui_text_secondary())
                            .child(crate::i18n::t("在菜单里显示")),
                    )
                    .child(
                        div()
                            .id("editor-enabled")
                            .cursor_pointer()
                            .on_click(cx.listener({
                                let id = self.id.clone();
                                move |_this, _: &ClickEvent, window, cx| {
                                    settings::update(cx, |settings| {
                                        if let Some(path) = settings.find_path(&id) {
                                            if let Some(item) = settings.node_mut(&path) {
                                                item.enabled = !item.enabled;
                                            }
                                        }
                                    });
                                    window.refresh();
                                    cx.notify();
                                }
                            }))
                            .child(switch(node.enabled)),
                    ),
            )
            // 图标位空着时，自动用「第一个运行程序动作」的 exe 图标
            .child(auto_icon_row(node, self.id.clone(), cx))
            // 运行程序时把选中内容接在命令末尾（只在普通项上出现）
            .when(!is_submenu, |column| {
                column.child(append_selection_row(node, self.id.clone(), cx))
            })
            // 适用选中内容：决定「弹出菜单时这一项出不出现」
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("适用选中内容")),
                    )
                    .child(context_chips(node, self.id.clone(), cx))
                    // 只有勾了「文件」才问后缀 —— 文字/文件夹没有后缀这回事
                    .when(wants_file(node), |block| {
                        block.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(theme::ui_text_tertiary())
                                        .child(crate::i18n::t("只对哪些后缀出现（留空 = 所有文件）")),
                                )
                                .child(ext_box(&self.ext, cx))
                                .child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(theme::ui_text_tertiary())
                                        .child(ext_hint(&node.file_exts)),
                                ),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(context_hint(node, is_root, is_submenu)),
                    ),
            )
            // 单独指定的普通项会浮到根菜单：浮上去放哪儿，让它自己说了算。
            // 根层的项也照样显示这一行（只是换成说明）—— 否则用户在根层项上
            // 怎么找都找不到这个功能，会以为没做。
            .when(
                node.context_mode == settings::ContextMode::Own && !is_submenu,
                |column| column.child(placement_section(node, is_root, self.id.clone(), cx)),
            )
            .child(div().h(px(1.)).w_full().bg(theme::border()))
            // 局部显示设置：优先级高于「外观 → 内容」里的全局开关
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("这一项的显示方式（优先于全局设置）")),
                    )
                    .child(visibility_row(
                        crate::i18n::t("图标位"),
                        node.show_badges,
                        "edit-badges",
                        self.id.clone(),
                        |item, value| item.show_badges = value,
                    ))
                    .child(visibility_row(
                        crate::i18n::t("快捷键提示"),
                        node.show_shortcuts,
                        "edit-shortcuts",
                        self.id.clone(),
                        |item, value| item.show_shortcuts = value,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme::ui_text_secondary())
                            .child(crate::i18n::t("类型")),
                    )
                    .child(
                        div()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(theme::badge_fill())
                            .text_size(px(10.))
                            .text_color(if is_submenu {
                                theme::accent()
                            } else {
                                theme::ui_text_tertiary()
                            })
                            .child(if is_submenu { crate::i18n::t("子菜单") } else { crate::i18n::t("普通项") }),
                    )
                    .child(
                        div()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(theme::badge_fill())
                            .text_size(px(10.))
                            .text_color(if node.context_mode == settings::ContextMode::Inherit {
                                theme::ui_text_tertiary()
                            } else {
                                theme::accent()
                            })
                            .child(node.context_label()),
                    )
                    // 类型切换就近放在「类型」这一行右端。
                    //
                    // 原来它是单独一行（左边一句长说明 + 右边按钮），而左栏是定宽的、
                    // 内容按 max-content 排：说明一长就把按钮顶出可视区，按钮看得见边框
                    // 却看不见字、也点不到。放到这一行就不会再被谁挤走。
                    //
                    // 两个方向都**不丢东西**：变子菜单时动作留着（只是不执行），
                    // 还原成普通项时子项收进 `parked_children`（看不见，再点回来还在）。
                    .child(div().flex_1())
                    .child({
                        let path = path.clone();
                        if is_submenu {
                            ghost_button(
                                crate::i18n::t("还原为菜单项"),
                                cx.listener(move |_this, _: &ClickEvent, window, cx| {
                                    let Some(path) = path.clone() else {
                                        return;
                                    };
                                    settings::update(cx, |settings| {
                                        settings.demote_submenu(&path);
                                    });
                                    window.refresh();
                                    cx.notify();
                                }),
                            )
                        } else {
                            ghost_button(
                                crate::i18n::t("添加子菜单"),
                                cx.listener(move |_this, _: &ClickEvent, window, cx| {
                                    let Some(path) = path.clone() else {
                                        return;
                                    };
                                    settings::update(cx, |settings| {
                                        settings.promote_to_submenu(&path);
                                    });
                                    window.refresh();
                                    cx.notify();
                                }),
                            )
                        }
                    }),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(if is_submenu {
                        if crate::i18n::is_english() {
                            format!(
                                "Expands only ({child_count} children); demoting folds them away"
                            )
                        } else {
                            format!("子菜单只展开、不执行动作（{child_count} 个子项）；还原时子项收起来，不丢")
                        }
                    } else {
                        crate::i18n::t("改成子菜单后已填的动作还留着；点错了也不会丢").to_string()
                    }),
            );

        div()
            .id("editor-basic-column")
            .w(px(LEFT_W))
            .flex_none()
            .relative()
            .flex()
            .flex_col()
            .p(px(10.))
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_sunken())
            .child(
                div()
                    .id("editor-basic-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.basic_scroll)
                    .child(rows),
            )
            .child(self.column_bar(ColumnBar::Basic, cx))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.bar_drag.is_some() {
                    this.drag_bar_to(f32::from(event.position.y), cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.bar_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.bar_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    /// 某一栏的滚动条（照着 `ScrollHandle` 的偏移手画）。
    ///
    /// gpui 的 `overflow_y_scroll` 只负责滚，不画条：窗口被拉小、内容超出时
    /// 用户根本看不出还能往下滚。这里画一根 4px 的细条，可以点滑轨、也可以拖。
    fn column_bar(&self, which: ColumnBar, cx: &mut Context<Self>) -> AnyElement {
        let handle = match which {
            ColumnBar::Basic => &self.basic_scroll,
            ColumnBar::Actions => &self.actions_scroll,
            ColumnBar::Picker => &self.picker_scroll,
        };
        let viewport = f32::from(handle.bounds().size.height);
        let max = f32::from(handle.max_offset().height);
        if std::env::var_os("GPUI_MENU_DEBUG_SCROLL").is_some() {
            eprintln!(
                "[scroll] {which:?} origin={:?} size={:?} max={max} offset={:?}",
                handle.bounds().origin,
                handle.bounds().size,
                handle.offset()
            );
        }
        if max <= 1.0 || viewport <= 1.0 {
            return div().into_any_element();
        }
        const PAD: f32 = 4.0;
        const BAR_W: f32 = 4.0;
        const HIT_W: f32 = 10.0;
        let track_h = (viewport - PAD * 2.0).max(1.0);
        let offset = (-f32::from(handle.offset().y)).clamp(0.0, max);
        let thumb_h = (viewport * viewport / (viewport + max))
            .clamp(24.0_f32.min(track_h), track_h);
        let top = offset / max * (track_h - thumb_h);
        let dragging = self.bar_drag.as_ref().map(|drag| drag.bar) == Some(which);

        div()
            .id(SharedString::from(match which {
                ColumnBar::Basic => "editor-bar-basic",
                ColumnBar::Actions => "editor-bar-actions",
                ColumnBar::Picker => "editor-bar-picker",
            }))
            .occlude()
            .absolute()
            .right(px(PAD))
            .top(px(PAD))
            .w(px(HIT_W))
            .h(px(track_h))
            .cursor_pointer()
            .child(
                div()
                    .absolute()
                    .right(px((HIT_W - BAR_W) / 2.0))
                    .top(px(top))
                    .w(px(BAR_W))
                    .h(px(thumb_h))
                    .rounded(px(BAR_W / 2.0))
                    .bg(if dragging {
                        theme::accent()
                    } else {
                        theme::scrollbar()
                    }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let handle = match which {
                        ColumnBar::Basic => this.basic_scroll.clone(),
                        ColumnBar::Actions => this.actions_scroll.clone(),
                        ColumnBar::Picker => this.picker_scroll.clone(),
                    };
                    let base = f32::from(handle.bounds().origin.y) + PAD;
                    let y = f32::from(event.position.y) - base;
                    let offset_now = -f32::from(handle.offset().y);
                    let thumb_top = offset_now / max * (track_h - thumb_h);
                    if y >= thumb_top && y <= thumb_top + thumb_h {
                        this.bar_drag = Some(BarDrag {
                            bar: which,
                            grab: y - thumb_top,
                            track: (track_h - thumb_h).max(1.0),
                            max,
                        });
                    } else {
                        // 点空白：让滑块中心对准点击处
                        let ratio = ((y - thumb_h / 2.0) / (track_h - thumb_h)).clamp(0.0, 1.0);
                        handle.set_offset(gpui::point(px(0.), px(-ratio * max)));
                        cx.notify();
                    }
                }),
            )
            // 按下去之后的 move / up 必须挂在滑轨自己身上：
            // `.occlude()` 挡住底下的列卡片之后，卡片那层的 move / up 就收不到事件了
            // （「菜单」的滚动条也是这么处理的）。
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.bar_drag.is_some() {
                    this.drag_bar_to(f32::from(event.position.y), cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.bar_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.bar_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    /// 拖着滚动条走（move / up 挂在整张卡片上，指针早就不在滑轨里了）。
    fn drag_bar_to(&mut self, y: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.bar_drag.as_ref().copied() else {
            return;
        };
        let handle = match drag.bar {
            ColumnBar::Basic => self.basic_scroll.clone(),
            ColumnBar::Actions => self.actions_scroll.clone(),
            ColumnBar::Picker => self.picker_scroll.clone(),
        };
        let base = f32::from(handle.bounds().origin.y) + 4.0;
        let local = y - base - drag.grab;
        let ratio = (local / drag.track).clamp(0.0, 1.0);
        handle.set_offset(gpui::point(px(0.), px(-ratio * drag.max)));
        cx.notify();
    }

    /// 右栏：普通项 = 动作列表；子菜单 = 子项列表。
    fn action_column(
        &self,
        node: &settings::ItemSetting,
        is_submenu: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut card = div()
            .id("editor-action-column")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(10.))
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_sunken());

        if is_submenu {
            // 子项列表和设置页「菜单项」是**同一个组件**（`tree_view::TreeView`）：
            // 拖动排序、加菜单项 / 子菜单 / 分隔线、点行编辑、显隐、删除、搜索筛选，全都在。
            // 之前这里只是「第 1 项 / 第 2 项」的只读列表，同一个子菜单从这边进来就什么都改不了。
            card = card.child(section_title(
                crate::i18n::t("子菜单"),
                crate::i18n::t("和设置里的「菜单项」一样：拖动排序、✎ 编辑、✕ 删除、加子项"),
            ));
            card = card.child(self.autofill_block(node, cx));
            return card
                .child(
                    div()
                        .id("editor-children-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(self.children_tree.clone()),
                )
                .into_any_element();
        }

        // ---- 普通项：动作列表 ----
        card = card.child(section_title(
            crate::i18n::t("动作 · 按顺序执行"),
            crate::i18n::t("每条执行完可设间隔毫秒，再走下一条"),
        ));

        let actions = node.actions.clone();
        let mut list = div().flex().flex_col().gap(px(8.));
        if actions.is_empty() {
            list = list.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "还没有动作：点下面的「添加动作」，或者把程序文件直接拖进这个窗口",
                    )),
            );
        }
        for (index, action) in actions.iter().enumerate() {
            list = list.child(self.action_block(index, action, actions.len(), cx));
        }

        card.child(
            div()
                .id("editor-actions-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.actions_scroll)
                .child(list),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(ghost_button(
                    crate::i18n::t("添加动作"),
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.add_action(cx);
                        window.refresh();
                    }),
                ))
                .child(ghost_button(
                    crate::i18n::t("▶ 全部测试"),
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.run_all_actions(cx);
                        window.refresh();
                    }),
                ))
                .child(div().flex_1())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(10.5))
                        .text_color(if self.test_msg.is_some() {
                            theme::accent()
                        } else {
                            theme::ui_text_tertiary()
                        })
                        .child(match &self.test_msg {
                            Some(msg) => msg.clone(),
                            None => crate::i18n::t("从上往下执行，第一条失败也会继续").to_string(),
                        }),
                ),
        )
        .child(self.column_bar(ColumnBar::Actions, cx))
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
            if this.bar_drag.is_some() {
                this.drag_bar_to(f32::from(event.position.y), cx);
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                if this.bar_drag.take().is_some() {
                    cx.notify();
                }
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                if this.bar_drag.take().is_some() {
                    cx.notify();
                }
            }),
        )
        .into_any_element()
    }

    /// 跑一条动作（测试用），结果写进 `note` 那行小字里。
    fn run_single_action(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(step) = settings::read(|settings| {
            settings
                .item_ref(&self.id)
                .and_then(|item| item.actions.get(index).cloned())
        }) else {
            return;
        };
        match crate::popup::test_run_step(cx, &step) {
            Ok(()) => self.test_msg = Some(crate::i18n::t("已执行这一条").to_string()),
            Err(err) => self.test_msg = Some(format!("执行失败：{err}")),
        }
        cx.notify();
    }

    /// 从上往下把这一项的动作全跑一遍。
    fn run_all_actions(&mut self, cx: &mut Context<Self>) {
        let steps = settings::read(|settings| {
            settings
                .item_ref(&self.id)
                .map(|item| item.actions.clone())
                .unwrap_or_default()
        });
        if steps.is_empty() {
            return;
        }
        let mut failed = 0usize;
        for step in &steps {
            if crate::popup::test_run_step(cx, step).is_err() {
                failed += 1;
            }
        }
        self.test_msg = Some(if crate::i18n::is_english() {
            format!("Ran {} action(s), {failed} failed", steps.len())
        } else {
            format!("跑了 {} 条，失败 {failed} 条", steps.len())
        });
        cx.notify();
    }

    /// 动作下面那行预览的**文本**（没有可预览的就返回 `None`）。
    ///
    /// 为什么是「文本」而不是「元素」：上一版返回一个 div 挂到动作块上，
    /// 实测那行死活不显示（`GPUI` 里 flex_col 的零高度子元素被吃掉了）。
    /// 改成拼进底下的提示行 —— 那一行本来就在渲染，一定看得见。
    fn action_preview_text(
        &self,
        index: usize,
        action: &settings::ActionStep,
        cx: &mut Context<Self>,
    ) -> Option<(String, gpui::Rgba)> {
        // 用**输入框里的实时值**：`action` 是上一次落盘的快照
        let live = self
            .rows
            .get(index)
            .map(|row| row.value.read(cx).text().trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| action.value.trim().to_string());
        let value = live.trim_matches('"').trim();
        if value.is_empty() {
            return None;
        }
        match action.kind {
            settings::ActionKind::Text => {
                let shown: String = crate::placeholders::expand(value).chars().take(120).collect();
                Some((
                    format!("{} {shown}", crate::i18n::t("实际发送：")),
                    theme::ui_text_secondary(),
                ))
            }
            settings::ActionKind::Run => {
                let bare = !value.contains('\\')
                    && !value.contains('/')
                    && !value.contains('{')
                    && !value.contains('%');
                if bare {
                    return Some(match crate::progindex::resolve(value) {
                        Some(path) => (
                            format!("{} {path}", crate::i18n::t("解析到")),
                            theme::accent(),
                        ),
                        None => (
                            crate::i18n::t("找不到这个文件（Everything 里没有 / 路径不对）").to_string(),
                            theme::danger(),
                        ),
                    });
                }
                if value.contains('{') || value.contains('%') {
                    return Some((
                        crate::i18n::t("含占位符：运行时才知道最终路径").to_string(),
                        theme::ui_text_tertiary(),
                    ));
                }
                Some(if std::path::Path::new(value).exists() {
                    (
                        crate::i18n::t("这个文件存在").to_string(),
                        theme::ui_text_secondary(),
                    )
                } else {
                    (
                        crate::i18n::t("这个路径不存在").to_string(),
                        theme::danger(),
                    )
                })
            }
            _ => None,
        }
    }

    /// 切到「会清空」的生成方式时，弹一个**居中的确认框**。
    ///
    /// 原来是卡片右侧内联一行红字，窗口没最大化时容易被挡住看不见（用户报过）；
    /// 改成和「运行参数」同一个遮罩层写法：压在最上面、点外面就是取消。
    fn autofill_confirm_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let mode = AUTOFILL_ARM.with(|slot| slot.get())?;
        let id = self.id.clone();
        let count = settings::read(|settings| {
            settings
                .find_path(&id)
                .and_then(|path| settings.node(&path))
                .map(|node| {
                    node.children
                        .iter()
                        .filter(|child| child.kind != settings::ItemKind::Separator)
                        .count()
                })
                .unwrap_or(0)
        });

        let id_ok = id.clone();
        let confirm = primary_button(
            crate::i18n::t("清空并切换"),
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                apply_autofill_mode(&id_ok, mode, cx);
                this.autofill_open = true;
                window.refresh();
                cx.notify();
            }),
        );
        let id_cancel = id.clone();
        let cancel = ghost_button(
            crate::i18n::t("算了"),
            cx.listener(move |_this, _: &ClickEvent, window, cx| {
                let _ = &id_cancel;
                AUTOFILL_ARM.with(|slot| slot.set(None));
                window.refresh();
                cx.notify();
            }),
        );

        Some(
            div()
                .absolute()
                .left_0()
                .top_0()
                .size_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x000000aa))
                // 点卡片外面 = 取消
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _: &MouseDownEvent, window, cx| {
                        AUTOFILL_ARM.with(|slot| slot.set(None));
                        window.refresh();
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w(px(360.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .p(px(14.))
                        .rounded(px(10.))
                        .bg(theme::surface_elevated())
                        .border_1()
                        .border_color(theme::border_strong())
                        // 卡片自己吃掉点击，别冒泡到上面那个「点外面取消」
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text_primary())
                                .child(if crate::i18n::is_english() {
                                    format!("Switch to 「{}」?", mode.label())
                                } else {
                                    format!("改成「{}」？", mode.label())
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .line_height(px(18.))
                                .text_color(theme::ui_text_secondary())
                                .child(if crate::i18n::is_english() {
                                    format!(
                                        "This clears the {count} menu item(s) currently in this submenu — they are not generated, so they will be gone for good."
                                    )
                                } else {
                                    format!(
                                        "这一支现在的 {count} 项菜单项会被清掉（它们不是文件夹生成的，删了找不回来）。"
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel)
                                .child(confirm),
                        ),
                )
                .into_any_element(),
        )
    }

    /// 一条动作的配置块。
    /// 子菜单的「从文件夹自动生成」：按文件夹 + 后缀把文件读成这一支的菜单项。
    ///
    /// 收起时只占一行 —— 大多数子菜单用不着它；展开才露出文件夹 / 后缀 /
    /// 写入方式 / 条数。真正去查的是 **Everything 的索引**（不是扫盘、也不递归扫目录），
    /// 所以点「立即生成」是毫秒级，几十个文件夹也不会卡。
    ///
    /// 只补缺不覆盖：同一支里已经有同名项就跳过，用户改过的动作不会被冲掉；
    /// 文件删了也不会反过来把项删掉（那是用户攒下来的东西）。
    fn autofill_block(&self, node: &settings::ItemSetting, cx: &mut Context<Self>) -> AnyElement {
        let spec = node.autofill.clone();
        let open = self.autofill_open;
        let connected = crate::everything::status().backend != crate::everything::Backend::None;
        let on = spec.enabled;
        let id = self.id.clone();

        let head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .id("editor-autofill-toggle")
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.autofill_open = !this.autofill_open;
                        window.refresh();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme::ui_text_tertiary())
                            .child(if open { "▼" } else { "▶" }),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme::ui_text_secondary())
                            .child(crate::i18n::t("从文件夹自动生成")),
                    )
                    .when(!open && on, |row| {
                        // 收起时也要看得出「这一支在自动生成」，不然会以为是空的
                        row.child(
                            div()
                                .px(px(5.))
                                .rounded(px(4.))
                                .bg(theme::accent_soft())
                                .text_size(px(9.))
                                .text_color(theme::accent())
                                .child(format!("{} 项", node.children.len())),
                        )
                    }),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(if on {
                        crate::i18n::t("已开启")
                    } else {
                        crate::i18n::t("已关闭")
                    }),
            )
            .child(
                div()
                    .id("editor-autofill-switch")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        let now = settings::read(|settings| {
                            settings
                                .find_path(&this.id)
                                .and_then(|path| settings.node(&path))
                                .map(|item| item.autofill.enabled)
                                .unwrap_or(false)
                        });
                        this.apply_field(move |item| item.autofill.enabled = !now, cx);
                        // 点开关就顺手展开：只翻开关看不到东西，会以为没生效
                        this.autofill_open = true;
                        this.autofill_msg = None;
                        window.refresh();
                        cx.notify();
                    }))
                    .child(switch(on)),
            );

        let card = div()
            .id("editor-autofill")
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(head);

        if !open {
            return card.into_any_element();
        }

        let hint_color = if connected {
            theme::ui_text_tertiary()
        } else {
            theme::danger()
        };
        let bottom = match &self.autofill_msg {
            Some(msg) => (msg.clone(), theme::accent()),
            None if connected => (
                crate::i18n::t("走 Everything 索引、不扫盘；同名项不会重复生成，删掉的文件也不会自动移走")
                    .to_string(),
                hint_color,
            ),
            None => (
                crate::i18n::t("Everything 还没连上：先开 Everything（「设置 → 行为」里能看到状态）")
                    .to_string(),
                hint_color,
            ),
        };

        card.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("文件夹（一行一个，可写 {exe_dir} / {env:USERPROFILE} 这类占位符）")),
                )
                // 多行框有时候点不进去（焦点没落上，Ctrl+V 就贴到别处去了），
                // 给一颗「粘贴路径」按钮：直接读剪贴板往里追加，绕开焦点问题
                .child(ghost_button(
                    crate::i18n::t("粘贴路径"),
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        let Some(text) = crate::win32::read_clipboard_text() else {
                            return;
                        };
                        let text = text.trim();
                        if text.is_empty() {
                            return;
                        }
                        this.apply_field(
                            move |item| {
                                if !item.autofill.dirs.trim().is_empty() {
                                    item.autofill.dirs.push('\n');
                                }
                                item.autofill.dirs.push_str(text);
                            },
                            cx,
                        );
                        window.refresh();
                        cx.notify();
                    }),
                )),
        )
        // 多行框外面套一层：和脚本动作那边一样的观感
        .child(
            div()
                .w_full()
                .h(px(64.))
                .rounded(px(5.))
                .bg(theme::surface_sunken())
                .border_1()
                .border_color(theme::border_strong())
                .p(px(4.))
                .child(self.autofill_dirs.clone()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(48.))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("后缀")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text_box(&self.autofill_exts, 0.)),
                )
                .child(
                    div()
                        .w(px(28.))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("最多")),
                )
                .child(
                    div()
                        .w(px(52.))
                        .flex_none()
                        .child(text_box(&self.autofill_limit, 0.)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(48.))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("范围")),
                )
                .child(segmented(
                    "editor-autofill-scope",
                    &[
                        (0, crate::i18n::t("仅当前文件夹")),
                        (1, crate::i18n::t("含子文件夹")),
                    ],
                    if spec.recurse { 1 } else { 0 },
                    {
                        let id = id.clone();
                        move |value: &u32, window, cx| {
                            let recurse = *value == 1;
                            let id = id.clone();
                            settings::update(cx, |settings| {
                                if let Some(path) = settings.find_path(&id) {
                                    if let Some(item) = settings.node_mut(&path) {
                                        item.autofill.recurse = recurse;
                                    }
                                }
                            });
                            window.refresh();
                        }
                    },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("默认只收这一层；子文件夹会一起收进来（文件多的文件夹会慢）")),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(48.))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("写入")),
                )
                .child(segmented(
                    "editor-autofill-mode",
                    &[
                        (0, crate::i18n::t("完整路径")),
                        (1, crate::i18n::t("只写文件名")),
                    ],
                    if spec.full_path { 0 } else { 1 },
                    {
                        let id = id.clone();
                        move |value: &u32, window, cx| {
                            let full = *value == 0;
                            let id = id.clone();
                            settings::update(cx, |settings| {
                                if let Some(path) = settings.find_path(&id) {
                                    if let Some(item) = settings.node_mut(&path) {
                                        item.autofill.full_path = full;
                                    }
                                }
                            });
                            window.refresh();
                        }
                    },
                ))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(48.))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("方式")),
                )
                .child(segmented(
                    "editor-autofill-way",
                    &[
                        (0, settings::AutoFillMode::Manual.label()),
                        (1, settings::AutoFillMode::Live.label()),
                        (2, settings::AutoFillMode::OnStart.label()),
                    ],
                    match spec.mode {
                        settings::AutoFillMode::Manual => 0,
                        settings::AutoFillMode::Live => 1,
                        settings::AutoFillMode::OnStart => 2,
                    },
                    {
                        let id = id.clone();
                        move |value: &u32, window, cx| {
                            let mode = match *value {
                                1 => settings::AutoFillMode::Live,
                                2 => settings::AutoFillMode::OnStart,
                                _ => settings::AutoFillMode::Manual,
                            };
                            let id = id.clone();
                            // 「会清空」的方式要二次确认：这一支里现在有东西的话，
                            // 先不动设置，只把窗口切到确认态（下面那行红字）
                            let current = settings::read(|settings| {
                                settings
                                    .find_path(&id)
                                    .and_then(|path| settings.node(&path))
                                    .map(|node| {
                                        (
                                            node.autofill.mode,
                                            node.children
                                                .iter()
                                                .filter(|c| c.kind != settings::ItemKind::Separator)
                                                .count(),
                                        )
                                    })
                                    .unwrap_or((settings::AutoFillMode::Manual, 0))
                            });
                            if mode.clears() && current.1 > 0 && current.0 != mode {
                                AUTOFILL_ARM.with(|slot| slot.set(Some(mode)));
                                // 这里 `cx` 是 `&mut App`（segmented 的回调签名），
                                // 只能刷窗口重绘；条目本身没变，不用 notify
                                window.refresh();
                                return;
                            }
                            apply_autofill_mode(&id, mode, cx);
                            window.refresh();
                        }
                    },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(spec.mode.hint()),
                ),
        )
                .child(div().flex_1())
                // 「立即生成」是手动模式专属：实时 / 仅启动时都是自动跑，
                // 这个按钮点了也没意义（而且会让人以为要手动点一下才生效）
                .when(spec.mode == settings::AutoFillMode::Manual, |row| {
                    row.child(ghost_button(
                    crate::i18n::t("立即生成"),
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        // 只认已经开着的分支：没开开关时按钮也能点，但会先把它打开 ——
                        // 否则「生成了却不出现」会让人莫名其妙
                        let on = settings::read(|settings| {
                            settings
                                .find_path(&this.id)
                                .and_then(|path| settings.node(&path))
                                .map(|item| item.autofill.enabled)
                                .unwrap_or(false)
                        });
                        if !on {
                            this.apply_field(|item| item.autofill.enabled = true, cx);
                        }
                        let (_, note) = settings::apply_autofill(cx);
                        this.autofill_msg = Some(note);
                        window.refresh();
                        cx.notify();
                    }),
                    ))
                })
        )
        .child(div().text_size(px(9.5)).text_color(bottom.1).child(bottom.0))
        .into_any_element()
    }

    fn action_block(
        &self,
        index: usize,
        action: &ActionStep,
        total: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(8.))
            .rounded(px(theme::RADIUS_ROW))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border());

        // 第一行：序号 + 类型胶囊 + 上移 / 下移 / 删除
        let mut kinds = div().flex().items_center().gap(px(2.));
        for kind in ActionKind::ALL {
            let selected = kind == action.kind;
            kinds = kinds.child(
                div()
                    .id(SharedString::from(format!("kind-{index}-{}", kind.label())))
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .when(selected, |style| {
                        style
                            .bg(theme::accent())
                            .text_color(theme::surface_elevated())
                    })
                    .when(!selected, |style| {
                        style
                            .bg(theme::track())
                            .text_color(theme::ui_text_secondary())
                            .hover(|s| s.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_action_kind(index, kind, cx);
                        window.refresh();
                    }))
                    .child(kind.label()),
            );
        }

        let mut head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .min_w(px(22.))
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .child(format!("{}", index + 1)),
            )
            .child(kinds)
            .child(div().flex_1());
        // 每条动作单独试一下：和菜单里执行同一段代码，占位符、程序解析都一样
        head = head.child(small_button(
            "▶",
            format!("test-{index}"),
            cx,
            move |this, window, cx| {
                this.run_single_action(index, cx);
                window.refresh();
            },
        ));
        if index > 0 {
            head = head.child(small_button(
                "↑",
                format!("up-{index}"),
                cx,
                move |this, window, cx| {
                    this.move_action(index, -1, cx);
                    window.refresh();
                },
            ));
        }
        if index + 1 < total {
            head = head.child(small_button(
                "↓",
                format!("down-{index}"),
                cx,
                move |this, window, cx| {
                    this.move_action(index, 1, cx);
                    window.refresh();
                },
            ));
        }
        head = head.child(small_button(
            "✕",
            format!("remove-{index}"),
            cx,
            move |this, window, cx| {
                this.remove_action(index, cx);
                window.refresh();
            },
        ));
        block = block.child(head);

        // 第二行：参数 + 间隔
        let param = if action.kind == ActionKind::Builtin {
            self.builtin_param_block(index, action, cx)
        } else if action.kind == ActionKind::Script {
            self.script_block(index, action, cx)
        } else if let Some(row) = self.rows.get(index) {
            let mut line = div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().flex_1().min_w_0().child(text_box(&row.value, 0.)));
            // 「发送按键」给一个键盘面板：Win 键、左右修饰键、小键盘只能点出来
            if action.kind == ActionKind::Keys {
                line = line.child(crate::keypad::open_button(
                    format!("action-pad-{index}"),
                    &row.value,
                    // 连按多个键用空格隔开，面板默认「追加」，不给它把前面写的冲掉
                    true,
                    None,
                ));
                // 多媒体键（音量 / 播放 / 浏览器 …）：键盘面板上画不出来，单开一片
                let media_open = self.media_open == Some(index);
                line = line.child(
                    div()
                        .id(SharedString::from(format!("action-media-{index}")))
                        .h(px(24.))
                        .px(px(9.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(4.))
                        .flex_none()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(if media_open {
                            theme::accent()
                        } else {
                            theme::border_strong()
                        })
                        .bg(if media_open {
                            theme::accent_soft()
                        } else {
                            theme::surface_elevated()
                        })
                        .text_size(px(11.))
                        .text_color(if media_open {
                            theme::accent()
                        } else {
                            theme::ui_text_secondary()
                        })
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .child("⏯")
                        .child(crate::i18n::t("多媒体键"))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.media_open = if this.media_open == Some(index) {
                                None
                            } else {
                                Some(index)
                            };
                            window.refresh();
                            cx.notify();
                        })),
                );
            }
            line.into_any_element()
        } else {
            div().into_any_element()
        };

        let delay_input = self
            .rows
            .get(index)
            .map(|row| {
                div()
                    .w(px(if crate::i18n::is_english() { 66. + 34. } else { 66. }))
                    .flex_none()
                    .child(text_box(&row.delay, 0.))
                    .into_any_element()
            })
            .unwrap_or_else(|| div().into_any_element());

        block = block.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(param)
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("之后等")),
                )
                .child(delay_input)
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child("ms"),
                ),
        );
        // 「发送按键」：底下再给一排常用键（音量 / 静音 / 播放 / 上一首 …），
        // 点「多媒体键」还能展开一整片浏览器键、启动键
        if action.kind == ActionKind::Keys {
            if let Some(row) = self.rows.get(index) {
                block = block.child(keys_quick_row(index, &row.value, cx));
                if self.media_open == Some(index) {
                    block = block.child(media_keys_panel(index, &row.value, cx));
                }
            }
        }
        // 「运行程序」的几个高级选项（管理员 / 已在运行时怎么处置 / 工作目录 / 窗口状态）：
        // 单独一个面板，动作块里只留一行摘要，不然这一行会被挤成一条缝
        if action.kind == ActionKind::Run {
            let options = crate::runopts::Options::from_params(&action.params);
            let summary = options.summary();
            block = block.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ghost_button(
                        crate::i18n::t("运行选项…"),
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_run_panel(index, cx);
                            window.refresh();
                        }),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(9.5))
                            .text_color(if summary.is_empty() {
                                theme::ui_text_tertiary()
                            } else {
                                theme::accent()
                            })
                            .child(if summary.is_empty() {
                                crate::i18n::t("管理员运行、已在运行时怎么办、工作目录、窗口状态").to_string()
                            } else {
                                summary
                            }),
                    ),
            );
        }
        // 占位符胶囊：点一下就把 {xxx} 追加到参数里（内置动作没有参数，不显示）
        if action.kind != ActionKind::Builtin {
            if let Some(row) = self.rows.get(index) {
                block = block.child(placeholder_row(&row.value, cx));
            }
        }
        // 预览：发送文本「展开后打出去的是什么」、运行程序「裸文件名解析到哪」
        let preview = self.action_preview_text(index, action, cx);
        block
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    // 「运行程序」不再显示那行说明：它上面的输入框、占位符胶囊已经把
                    // 用法说清楚了，再加一句「例如 notepad.exe…」纯属噪音。
                    // （下面那行「解析到 C://...」是实时结果，留着。）
                    .when(action.kind != ActionKind::Run, |row| {
                        row.child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme::ui_text_tertiary())
                                .child(action.kind.hint()),
                        )
                    })
                    .when(!action.is_ready(), |row| {
                        row.child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme::danger())
                                .child(crate::i18n::t("· 参数还没填")),
                        )
                    })
                    .children(preview.map(|(text, color)| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(9.5))
                            .text_color(color)
                            .child(format!("· {text}"))
                    })),
            )
            .into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let path = settings::read(|settings| settings.find_path(&self.id));
        let dirty = settings::current() != self.committed;
        div()
            .h(px(46.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(ghost_button(
                crate::i18n::t("删除这一项"),
                cx.listener(move |_this, _: &ClickEvent, window, cx| {
                    if let Some(path) = path.clone() {
                        settings::update(cx, |settings| settings.remove_at(&path));
                    }
                    window.remove_window();
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(10.5))
                    .text_color(if dirty {
                        theme::accent()
                    } else {
                        theme::ui_text_tertiary()
                    })
                    .child(if dirty {
                        crate::i18n::t("● 改动即时预览；取消会放弃未应用的改动")
                    } else {
                        crate::i18n::t("改动已应用到菜单")
                    }),
            )
            .child(ghost_button(
                crate::i18n::t("取消"),
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.cancel_changes(cx);
                    window.remove_window();
                }),
            ))
            .child(primary_button(
                crate::i18n::t("确定"),
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.apply_changes(cx);
                    window.remove_window();
                }),
            ))
            .child(ghost_button(
                crate::i18n::t("应用"),
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.apply_changes(cx);
                    window.refresh();
                }),
            ))
            .into_any_element()
    }
}

fn section_title(title: &'static str, hint: &'static str) -> AnyElement {
    let title = crate::i18n::t(title);
    let hint = crate::i18n::t(hint);
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child(title),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(10.))
                .text_color(theme::ui_text_tertiary())
                .child(hint),
        )
        .into_any_element()
}

/// 一行「跟随全局 / 显示 / 隐藏」三态开关（局部显示设置）。
fn visibility_row(
    label: &'static str,
    value: Option<bool>,
    seg_id: &'static str,
    item_id: String,
    set: fn(&mut settings::ItemSetting, Option<bool>),
) -> AnyElement {
    let current = match value {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    };
    let label = crate::i18n::t(label);
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .w(px(if crate::i18n::is_english() { 64. + 34. } else { 64. }))
                .flex_none()
                .text_size(px(10.5))
                .text_color(theme::ui_text_tertiary())
                .child(label),
        )
        .child(segmented(
            seg_id,
            &[(0, crate::i18n::t("跟随全局")), (1, crate::i18n::t("显示")), (2, crate::i18n::t("隐藏"))],
            current,
            move |value: &u32, window, cx| {
                let next = match *value {
                    1 => Some(true),
                    2 => Some(false),
                    _ => None,
                };
                let item_id = item_id.clone();
                settings::update(cx, |settings| {
                    if let Some(path) = settings.find_path(&item_id) {
                        if let Some(item) = settings.node_mut(&path) {
                            set(item, next);
                        }
                    }
                });
                window.refresh();
            },
        ))
        .into_any_element()
}

/// 编辑窗口里那个小预览方块画什么：自己有图标位就用自己的，
/// 否则和菜单里一样，自动拿「第一个运行程序动作」的 exe 图标。
fn preview_icon(node: &settings::ItemSetting) -> SharedString {
    if !node.badge.trim().is_empty() {
        return SharedString::from(node.badge.trim().to_string());
    }
    let auto = settings::read(|settings| settings.auto_icons);
    node.auto_badge(auto)
        .map(SharedString::from)
        .unwrap_or_else(|| SharedString::from(""))
}

/// 「自动图标」那一行：跟随全局 / 用程序图标 / 不用。
///
/// 图标位空着的时候，程序会去第一个「运行程序」动作里取那个 exe 的图标；
/// 这里能单独关掉（比如某个 exe 的图标太花，或者两项指向同一个程序、想换文字图标区分）。
fn auto_icon_row(
    node: &settings::ItemSetting,
    item_id: String,
    _cx: &mut Context<ItemEditor>,
) -> AnyElement {
    let current = match node.auto_icon {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    };
    let auto_icons = settings::read(|settings| settings.auto_icons);
    let hint = match node.auto_icon.unwrap_or(auto_icons) {
        true => match node.program_path() {
            Some(program) => {
                if crate::i18n::is_english() {
                    format!("Using the icon of {program}")
                } else {
                    format!("现在用的是 {program} 的图标")
                }
            }
            None => crate::i18n::t("这一项还没有「运行程序」动作，暂时没有图标可取").to_string(),
        },
        false => crate::i18n::t("不自动取图标（图标位留空就不显示）").to_string(),
    };
    // 「跟随全局」到底跟的是谁，得写清楚 —— 不然这个选项等于没说
    let follow = if auto_icons {
        crate::i18n::t("「跟随全局」= 跟「外观 → 内容 → 自动取程序图标」那个开关，现在是【开】")
    } else {
        crate::i18n::t("「跟随全局」= 跟「外观 → 内容 → 自动取程序图标」那个开关，现在是【关】")
    };
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(if crate::i18n::is_english() { 64. + 34. } else { 64. }))
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("自动图标")),
                )
                .child(segmented(
                    "edit-auto-icon",
                    &[(0, crate::i18n::t("跟随全局")), (1, crate::i18n::t("用程序图标")), (2, crate::i18n::t("不用"))],
                    current,
                    move |value: &u32, window, cx| {
                        let next = match *value {
                            1 => Some(true),
                            2 => Some(false),
                            _ => None,
                        };
                        let item_id = item_id.clone();
                        settings::update(cx, |settings| {
                            if let Some(path) = settings.find_path(&item_id) {
                                if let Some(item) = settings.node_mut(&path) {
                                    item.auto_icon = next;
                                }
                            }
                        });
                        window.refresh();
                    },
                )),
        )
        .child(
            div()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(follow),
        )
        .child(
            div()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(hint),
        )
        .into_any_element()
}

/// 「接上选中内容」那一行：跟随全局 / 开启 / 关闭。
///
/// 全局开关在「行为与占位符 → 选中内容」里（决定所有「运行程序」动作的默认值）；
/// 这里设了就以这里为准 —— 局部优先级高于全局，和「自动图标」那一行是同一套路。
fn append_selection_row(
    node: &settings::ItemSetting,
    item_id: String,
    _cx: &mut Context<ItemEditor>,
) -> AnyElement {
    let global = settings::read(|settings| settings.append_selection);
    let current = match node.append_selection {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    };
    let effective = node.append_selection.unwrap_or(global);
    let hint = if effective {
        crate::i18n::t("执行时是「程序 + 空格 + 选中内容」；文件路径带空格会自动加双引号")
    } else {
        crate::i18n::t("只执行程序本身，不拼选中内容")
    };
    let follow = if crate::i18n::is_english() {
        format!(
            "\"Follow global\" mirrors Behavior → Selection → Append selection, currently [{}]",
            if global { "on" } else { "off" }
        )
    } else {
        format!(
            "「跟随全局」= 跟「行为 → 选中内容 → 运行程序时接上选中内容」，现在是【{}】",
            if global { "开" } else { "关" }
        )
    };
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(if crate::i18n::is_english() { 64. + 34. } else { 64. }))
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("接上选中")),
                )
                .child(segmented(
                    "edit-append-selection",
                    &[(0, crate::i18n::t("跟随全局")), (1, crate::i18n::t("开启")), (2, crate::i18n::t("关闭"))],
                    current,
                    move |value: &u32, window, cx| {
                        let next = match *value {
                            1 => Some(true),
                            2 => Some(false),
                            _ => None,
                        };
                        let item_id = item_id.clone();
                        settings::update(cx, |settings| {
                            if let Some(path) = settings.find_path(&item_id) {
                                if let Some(item) = settings.node_mut(&path) {
                                    item.append_selection = next;
                                }
                            }
                        });
                        window.refresh();
                    },
                )),
        )
        .child(
            div()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(follow),
        )
        .child(
            div()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(hint),
        )
        .into_any_element()
}

/// 竖排字段：标题在上，控件在下。
/// 面板里那种「一行小标题」。
fn label_small(text: &'static str) -> AnyElement {
    div()
        .text_size(px(10.5))
        .text_color(theme::ui_text_tertiary())
        .child(crate::i18n::t(text))
        .into_any_element()
}

fn field(label: &'static str, control: AnyElement) -> AnyElement {
    let label = crate::i18n::t(label);
    div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme::ui_text_tertiary())
                .child(label),
        )
        .child(control)
        .into_any_element()
}

fn text_box(input: &Entity<TextInput>, width: f32) -> AnyElement {
    let mut boxed = div()
        .h(px(24.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(theme::surface_sunken())
        .border_1()
        .border_color(theme::border_strong())
        .child(input.clone());
    boxed = if width > 0.0 {
        boxed.w(px(width))
    } else {
        boxed.w_full()
    };
    boxed.into_any_element()
}

/// 一行占位符胶囊：点一下就把 `{名字}` 追加到参数末尾。
fn placeholder_row(input: &Entity<TextInput>, cx: &mut Context<ItemEditor>) -> AnyElement {
    let mut row = div().flex().items_center().gap(px(4.)).flex_wrap().child(
        div()
            .flex_none()
            .text_size(px(9.))
            .text_color(theme::ui_text_tertiary())
            .child(crate::i18n::t("占位符")),
    );
    for &name in crate::placeholders::QUICK {
        let input = input.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("ph-{name}")))
                .px(px(5.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(theme::track())
                .text_size(px(9.5))
                .text_color(theme::ui_text_secondary())
                .cursor_pointer()
                .hover(|style| {
                    style
                        .bg(theme::surface_hover())
                        .text_color(theme::text_primary())
                })
                .on_click(cx.listener(move |_this, _: &ClickEvent, _window, cx| {
                    let snippet = format!("{{{name}}}");
                    input.update(cx, |input, cx| input.insert_text(&snippet, cx));
                }))
                .child(format!("{{{name}}}")),
        );
    }
    row.child(
        div()
            .flex_none()
            .text_size(px(9.))
            .text_color(theme::ui_text_tertiary())
            .child(crate::i18n::t("全部见「设置 → 行为」")),
    )
    .into_any_element()
}

/// 把一颗键追加到「发送按键」那一行的末尾（多个用空格隔开，和面板的追加同一种写法）。
fn append_keys_token(input: &Entity<TextInput>, token: &str, cx: &mut Context<ItemEditor>) {
    let text = input.read(cx).text().to_string();
    let merged = if text.trim().is_empty() {
        token.to_string()
    } else {
        format!("{} {}", text.trim_end(), token)
    };
    input.update(cx, |input, cx| input.set_text(merged, cx));
}

/// 一颗「点一下就追加」的小胶囊。
fn keys_chip(
    id: String,
    label: String,
    input: &Entity<TextInput>,
    token: &'static str,
    cx: &mut Context<ItemEditor>,
) -> AnyElement {
    let input = input.clone();
    div()
        .id(SharedString::from(id))
        .h(px(20.))
        .px(px(7.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::track())
        .text_size(px(10.))
        .text_color(theme::ui_text_secondary())
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme::surface_hover())
                .text_color(theme::text_primary())
        })
        .on_click(cx.listener(move |_this, _: &ClickEvent, window, cx| {
            append_keys_token(&input, token, cx);
            window.refresh();
            cx.notify();
        }))
        .child(label)
        .into_any_element()
}

/// 「发送按键」底下那排常用键。
fn keys_quick_row(
    index: usize,
    input: &Entity<TextInput>,
    cx: &mut Context<ItemEditor>,
) -> AnyElement {
    let mut row = div().flex().items_center().gap(px(4.)).flex_wrap().child(
        div()
            .flex_none()
            .text_size(px(9.))
            .text_color(theme::ui_text_tertiary())
            .child(crate::i18n::t("常用键")),
    );
    for (label, token) in crate::mediakeys::QUICK {
        row = row.child(keys_chip(
            format!("keys-quick-{index}-{token}"),
            crate::i18n::t(label).to_string(),
            input,
            token,
            cx,
        ));
    }
    row.into_any_element()
}

/// 展开的「多媒体键」：按分组一片按钮（音量 / 播放 / 浏览器 / 启动 / 补充）。
fn media_keys_panel(
    index: usize,
    input: &Entity<TextInput>,
    cx: &mut Context<ItemEditor>,
) -> AnyElement {
    let mut panel = div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .p(px(7.))
        .rounded(px(6.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_elevated());
    for (group, keys) in crate::mediakeys::GROUPS {
        let mut row = div().flex().items_center().gap(px(4.)).flex_wrap().child(
            div()
                .w(px(52.))
                .flex_none()
                .text_size(px(9.))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t(group)),
        );
        for (label, token) in *keys {
            row = row.child(keys_chip(
                format!("keys-media-{index}-{token}"),
                crate::i18n::t(label).to_string(),
                input,
                token,
                cx,
            ));
        }
        panel = panel.child(row);
    }
    panel
        .child(
            div()
                .text_size(px(9.))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t(
                    "写进去的是 AHK 的键名（Volume_Up、Media_Play_Pause …），和键盘面板的写法混着用没事",
                )),
        )
        .into_any_element()
}

fn small_button<T: 'static>(
    glyph: &'static str,
    id: String,
    cx: &mut Context<T>,
    action: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .h(px(20.))
        .min_w(px(20.))
        .px(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .text_size(px(10.))
        .text_color(theme::ui_text_tertiary())
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme::surface_hover())
                .text_color(theme::text_primary())
        })
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            action(this, window, cx);
        }))
        .child(crate::i18n::t(glyph))
        .into_any_element()
}

/// 一颗「适用选中内容」的胶囊（选中的填强调色）。
fn context_chip<T: 'static>(
    label: &'static str,
    id: String,
    active: bool,
    cx: &mut Context<T>,
    action: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
) -> AnyElement {
    let label = crate::i18n::t(label);
    div()
        .id(SharedString::from(id))
        .h(px(20.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(10.))
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(if active {
            theme::accent_soft()
        } else {
            theme::track()
        })
        .text_size(px(10.))
        .text_color(if active {
            theme::accent()
        } else {
            theme::ui_text_secondary()
        })
        .cursor_pointer()
        .hover(|style| style.border_color(theme::border_strong()))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            action(this, window, cx);
        }))
        .child(label)
        .into_any_element()
}

/// 「跟随上级 / 文字 / 文件 / 图片 / 不参与」。
///
/// 前四颗是一组：`跟随上级` 是默认（继承上级子菜单的类型）；
/// 勾了类型就变成「单独指定」（从上级那一支里脱离，浮到根菜单去）；
/// `不参与` 是第五颗，独立开关。
fn context_chips(
    node: &settings::ItemSetting,
    item_id: String,
    cx: &mut Context<ItemEditor>,
) -> AnyElement {
    use settings::ContextMode;

    let mode = node.context_mode;
    let mut row = div().flex().items_center().gap(px(4.)).flex_wrap();
    // 前四颗合在一起画一个底，和「不参与」在视觉上分开
    let mut group = div().flex().items_center().gap(px(4.)).flex_wrap();
    group = group.child(context_chip(
        crate::i18n::t("跟随上级"),
        format!("ctx-inherit-{item_id}"),
        mode == ContextMode::Inherit,
        cx,
        {
            let item_id = item_id.clone();
            move |_this, window, cx| set_context_mode(&item_id, ContextMode::Inherit, window, cx)
        },
    ));
    for kind in crate::selection::SelectionKind::ALL {
        group = group.child(context_chip(
            kind.label(),
            format!("ctx-{}-{item_id}", kind.key()),
            mode == ContextMode::Own && node.contexts.contains(&kind),
            cx,
            {
                let item_id = item_id.clone();
                move |_this, window, cx| toggle_context_kind(&item_id, kind, window, cx)
            },
        ));
    }
    // 「不参与」跟大类挤同一行：它是否决项，不是类型。
    // 单独占一行的代价是整块高 30px —— 左栏矮的时候正好把它下面的
    // 「浮到根菜单以后放哪儿」挤出视口。
    group = group.child(context_chip(
        crate::i18n::t("不参与"),
        format!("ctx-never-{item_id}"),
        mode == ContextMode::Never,
        cx,
        {
            let item_id = item_id.clone();
            move |_this, window, cx| set_context_mode(&item_id, ContextMode::Never, window, cx)
        },
    ));
    row = row.child(group);
    row.into_any_element()
}

/// 这一项会不会「按文件后缀」筛选：只有单独指定了、并且勾了「文件」才问后缀。
fn wants_file(node: &settings::ItemSetting) -> bool {
    node.context_mode == settings::ContextMode::Own
        && node.contexts.contains(&crate::selection::SelectionKind::File)
}

/// 「浮到根菜单以后放哪儿」整段：子菜单里的项给四颗胶囊，根层的项给一句说明
/// （根层项本来就在根菜单第一层，没有「浮上来」这回事，但把入口藏起来会让人以为没做）。
fn placement_section(
    node: &settings::ItemSetting,
    is_root: bool,
    item_id: String,
    cx: &mut Context<ItemEditor>,
) -> AnyElement {
    let title = div()
        .text_size(px(10.5))
        .text_color(theme::ui_text_tertiary())
        .child(crate::i18n::t("浮到根菜单以后放哪儿"));
    let body = if is_root {
        div()
            .text_size(px(9.5))
            .text_color(theme::ui_text_tertiary())
            .child(crate::i18n::t("它已经在根菜单第一层，位置就是现在这儿；四颗胶囊只在「子菜单里的项单独指定类型、浮上来」时才有得选"))
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(placement_chips(node, item_id, cx))
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(node.float_placement.hint()),
            )
            .into_any_element()
    };
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(title)
        .child(body)
        .into_any_element()
}

/// 一类文件的后缀表输入框：类型名 + 输入框 + 「现在认几个」。
/// 后缀输入框整行（「勾了文件」才显示）。
fn ext_box(input: &Entity<TextInput>, cx: &mut Context<ItemEditor>) -> AnyElement {
    let text = input.read(cx).text();
    let count = crate::selection::parse_exts(&text).len();
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().flex_1().min_w_0().child(text_box(input, 0.)))
        .child(
            div()
                .min_w(px(if crate::i18n::is_english() { 98. } else { 64. }))
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(if count == 0 {
                    crate::i18n::t("所有文件").to_string()
                } else if crate::i18n::is_english() {
                    format!("{count} extensions")
                } else {
                    format!("认 {count} 种")
                }),
        )
        .into_any_element()
}

/// 后缀输入框下面那句说明：写对了就报个数，写错了就提醒。
fn ext_hint(text: &str) -> String {
    let list = crate::selection::parse_exts(text);
    if list.is_empty() {
        return crate::i18n::t("留空 = 任何文件都会出现").to_string();
    }
    if crate::i18n::is_english() {
        if list.len() <= 6 {
            format!("Only for .{}", list.join(" / ."))
        } else {
            format!("Only for .{}… and {} more", list[0], list.len())
        }
    } else if list.len() <= 6 {
        format!("只对 .{} 出现", list.join(" / ."))
    } else {
        format!("只对 .{}… 等 {} 种后缀出现", list[0], list.len())
    }
}

/// 「浮到根菜单以后放哪儿」的四颗胶囊。
fn placement_chips(
    node: &settings::ItemSetting,
    item_id: String,
    cx: &mut Context<ItemEditor>,
) -> AnyElement {
    let mut row = div().flex().items_center().gap(px(4.)).flex_wrap();
    for placement in settings::FloatPlacement::ALL {
        row = row.child(context_chip(
            placement.label(),
            format!("place-{}-{item_id}", placement.key()),
            node.float_placement == placement,
            cx,
            {
                let item_id = item_id.clone();
                move |_this, window, cx| set_float_placement(&item_id, placement, window, cx)
            },
        ));
    }
    row.into_any_element()
}

fn set_float_placement(
    item_id: &str,
    placement: settings::FloatPlacement,
    window: &mut Window,
    cx: &mut App,
) {
    let item_id = item_id.to_string();
    settings::update(cx, |settings| {
        if let Some(path) = settings.find_path(&item_id) {
            if let Some(item) = settings.node_mut(&path) {
                item.float_placement = placement;
            }
        }
    });
    window.refresh();
}

/// 胶囊下面那句说明：告诉用户这一项在筛选时到底会怎样。
fn context_hint(node: &settings::ItemSetting, is_root: bool, is_submenu: bool) -> &'static str {
    use settings::ContextMode;
    match node.context_mode {
        ContextMode::Inherit if is_root => {
            if is_submenu {
                crate::i18n::t("根菜单上的子菜单：选中了内容也不会自动出现，除非里面有项单独指定了类型")
            } else {
                crate::i18n::t("根菜单上的通用项：选中了内容时会藏起来（没选中时照常显示）")
            }
        }
        ContextMode::Inherit => {
            if is_submenu {
                crate::i18n::t("跟上级子菜单一样（上级命中时才出现）")
            } else {
                crate::i18n::t("跟上级子菜单一样 —— 上级命中时它就出现")
            }
        }
        ContextMode::Never => crate::i18n::t("就算上级命中了，有选中内容时这一项也不出现"),
        ContextMode::Own if is_submenu => crate::i18n::t("这一支归它管：选中这类内容时出现，里面的项默认跟着它"),
        // 根层的项：位置本来就在第一层，别让用户以为还要选什么
        ContextMode::Own if is_root => crate::i18n::t("单独指定：它已经在根菜单第一层，只在选中这类内容时出现"),
        ContextMode::Own => crate::i18n::t("单独指定：会浮到根菜单（原来那层不再重复），浮上去放哪儿在下面选"),
    }
}

fn set_context_mode(item_id: &str, mode: settings::ContextMode, window: &mut Window, cx: &mut App) {
    let item_id = item_id.to_string();
    settings::update(cx, |settings| {
        if let Some(path) = settings.find_path(&item_id) {
            if let Some(item) = settings.node_mut(&path) {
                item.context_mode = mode;
                // 类型只对「单独指定」有意义；切走就清掉，免得读回来又变成「单独指定」
                if mode != settings::ContextMode::Own {
                    item.contexts.clear();
                }
            }
        }
    });
    window.refresh();
}

/// 点类型胶囊：没在「单独指定」里就切过去并只勾这一个；再点一次取消。
fn toggle_context_kind(
    item_id: &str,
    kind: crate::selection::SelectionKind,
    window: &mut Window,
    cx: &mut App,
) {
    use settings::ContextMode;
    let item_id = item_id.to_string();
    settings::update(cx, |settings| {
        let Some(path) = settings.find_path(&item_id) else {
            return;
        };
        let Some(item) = settings.node_mut(&path) else {
            return;
        };
        if item.context_mode != ContextMode::Own {
            item.context_mode = ContextMode::Own;
            item.contexts = vec![kind];
            return;
        }
        match item.contexts.iter().position(|value| *value == kind) {
            Some(index) => {
                item.contexts.remove(index);
            }
            None => item.contexts.push(kind),
        }
        // 一个都不剩 = 不再单独指定，退回跟随上级
        if item.contexts.is_empty() {
            item.context_mode = ContextMode::Inherit;
        }
    });
    window.refresh();
}
