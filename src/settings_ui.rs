//! 设置窗口：侧栏 + 页面，仿 Bolume 的设置界面。
//!
//! 版式分层（和 Bolume 一样）：
//!
//! ```text
//! 窗口（surface）
//! ├ 顶栏（可拖动 + 最小化 / 最大化 / 关闭）
//! └ 主体
//!   ├ 侧栏（surface_sunken，导航 + 版本）
//!   └ 内容（滚动）
//!     └ 分组卡（surface_sunken + 细边框 + 标题行）
//!       └ 行（px 9 / py 4，悬停 surface_hover）
//! ```
//!
//! 十一个页面（见 `Page::ALL`）：外观、菜单项、动作库、常规、托盘、程序、文件名、热字符串、行为、配置、关于。
//!
//! - 「程序」：本地可执行文件小索引 + 同名怎么挑 + Everything 联动状态；
//! - 「文件名」：菜单里用到的裸文件名各自解析到哪、同名有几份（点「选文件」先展开候选）。
//!
//! 全盘索引（`fileindex.rs` / 原来的「设置 → 索引」页）已删除：它要遍历整盘、常驻几十 MB，
//! 和「极低资源占用」冲突。全盘找路径改由 Everything 代劳，见 `src/everything.rs`。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    canvas, div, linear_color_stop, linear_gradient, px, rgba, size, AnyElement, AnyWindowHandle,
    App, AppContext, Bounds, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight,
    Global, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render,
    ScrollHandle, SharedString, Subscription, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions,
};

use crate::chrome::{self, Chrome};
use crate::popup;
use crate::selection::MultiFileRule;
use crate::settings::{
    self, EdgePlacement, ItemKind, ItemSetting, MenuSettings, PanelWidthMode, SearchPlace,
    SubmenuAnchor, ThemeMode, TrayLeftClick,
};
use crate::text_input::TextInput;
use crate::theme::{self, Field, ThemeColors};

const DEFAULT_W: f32 = 900.0;
const DEFAULT_H: f32 = 680.0;
const SIDEBAR_W: f32 = 184.0;
/// 滚动区左右内边距
const PAD_X: f32 = 18.0;
/// 给右侧滚动条留的宽
const SCROLLBAR_GUTTER: f32 = 10.0;
/// 滚动条看着多粗 / 鼠标热区多宽
const BAR_W: f32 = 4.0;
const BAR_HIT_W: f32 = 10.0;
/// 滑块最短长度
const MIN_BAR_THUMB: f32 = 28.0;
const NAV_H: f32 = 32.0;

// ---------------------------------------------------------------------------
// 打开窗口
// ---------------------------------------------------------------------------

pub struct SettingsWindowHandle(pub AnyWindowHandle);

impl Global for SettingsWindowHandle {}

/// 设置窗口里那个 View 的 Entity（「打开关于」要把已经开着的窗口翻到「关于」页）。
pub struct SettingsViewHandle(pub gpui::Entity<SettingsView>);

impl Global for SettingsViewHandle {}

thread_local! {
    /// 「改完没点确定就关窗」的兜底快照（= 打开窗口 / 上次应用时的状态）。
    ///
    /// 右上角的 ✕ 走我们自己的点击回调，直接回滚；
    /// 但 Alt+F4、任务栏「关闭窗口」走的是系统 `WM_CLOSE`，gpui 的 Windows 后端
    /// 没有「即将关闭」的钩子，收不到这次点击 —— 只能等窗口真的没了再退回去。
    static CLOSE_GUARD: RefCell<Option<MenuSettings>> = const { RefCell::new(None) };
}

/// 记下回滚点（打开窗口 / 应用 / 取消时调）。
fn arm_close_guard(snapshot: &MenuSettings) {
    CLOSE_GUARD.with(|slot| *slot.borrow_mut() = Some(snapshot.clone()));
}

/// 设置窗口还活着吗（`on_window_closed` 里用来判断关掉的是不是它）。
pub fn window_alive(cx: &mut App) -> bool {
    let Some(handle) = cx
        .try_global::<SettingsWindowHandle>()
        .map(|handle| handle.0)
    else {
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

/// 图标缓存补齐了，让两棵树各重画一次。
///
/// `icon_cache::warm` 在后台线程跑完之后调：不重画的话，设置页里那些
/// 「还没建好缓存时空着的图标位」要等下一次鼠标移动才补上。
pub fn repaint_trees(cx: &mut App) {
    let Some(view) = cx
        .try_global::<SettingsViewHandle>()
        .map(|handle| handle.0.clone())
    else {
        return;
    };
    let _ = view.update(cx, |view, cx| {
        view.items_tree.update(cx, |_, cx| cx.notify());
        view.tray_tree.update(cx, |_, cx| cx.notify());
    });
}

/// 打开（或聚焦）设置窗口。
pub fn open(cx: &mut App) {
    open_at(cx, None);
}

/// 打开设置窗口、并直接落到某一页（「打开关于」这个内置动作用）。
pub fn open_about(cx: &mut App) {
    open_at(cx, Some("about"));
}

/// 打开（或聚焦）设置窗口，可以顺带指定落到哪一页。
fn open_at(cx: &mut App, page: Option<&str>) {
    if let Some(handle) = cx
        .try_global::<SettingsWindowHandle>()
        .map(|handle| handle.0)
    {
        let alive = handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok();
        if alive {
            // 窗口已经开着：直接翻到要的那一页（否则「打开关于」只会把
            // 一个停在「外观」页的设置窗口拽到前台，等于什么都没发生）
            if let Some(page) = page.and_then(page_from_key) {
                if let Some(view) = cx.try_global::<SettingsViewHandle>() {
                    let view = view.0.clone();
                    let _ = view.update(cx, |view, cx| {
                        view.page = page;
                        cx.notify();
                    });
                }
            }
            return;
        }
    }
    // 新开窗口：让 `SettingsView::new` 知道该落在哪一页（不污染用户的环境变量）
    REQUESTED_PAGE.with(|slot| *slot.borrow_mut() = page.map(str::to_string));
    let bounds = Bounds::centered(None, size(px(DEFAULT_W), px(DEFAULT_H)), cx);
    let hwnd_cell: Rc<Cell<Option<isize>>> = Rc::new(Cell::new(None));
    // 窗口里那个 View 的 Entity：留着，才好把「已经开着的窗口」翻到别的页
    let view_cell: Rc<Cell<Option<gpui::Entity<SettingsView>>>> = Rc::new(Cell::new(None));
    let before = crate::win32::zed_window_handles();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // 自绘标题栏（Bolume 同款）：`appears_transparent` 让 gpui 铺满客户区，
            // `is_resizable` 让窗口带上 WS_THICKFRAME —— 四边四角都能拖，全是系统行为。
            titlebar: Some(chrome::titlebar(crate::i18n::t("RunAny_RS · 设置"))),
            window_decorations: Some(WindowDecorations::Client),
            kind: WindowKind::Normal,
            is_resizable: true,
            is_movable: true,
            // 自绘顶栏上有最小化按钮，gpui 靠这个标志决定要不要给 WS_MINIMIZEBOX
            is_minimizable: true,
            window_min_size: Some(size(px(700.), px(480.))),
            focus: true,
            show: true,
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        {
            let hwnd_cell = hwnd_cell.clone();
            let view_cell = view_cell.clone();
            move |_window, cx| {
                let view = cx.new(|cx| SettingsView::new(hwnd_cell, cx));
                view_cell.set(Some(view.clone()));
                view
            }
        },
    );

    let Ok(handle) = opened else {
        return;
    };
    cx.set_global(SettingsWindowHandle(handle.into()));
    if let Some(view) = view_cell.take() {
        cx.set_global(SettingsViewHandle(view));
    }

    // Win11 交给 DWM 裁系统圆角；Win10 保持直角（不再用 SetWindowRgn，
    // 区域裁剪会连带改掉边框命中区，拖边框就不可靠了）
    let hwnd = crate::win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        hwnd_cell.set(Some(hwnd));
        chrome::apply(hwnd);
    }
}

// 自绘标题栏 / 拖动 / 最大化都收在 `chrome` 里，两个窗口共用同一套。
// ---------------------------------------------------------------------------
// 页面
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// 页面
// ---------------------------------------------------------------------------

// 「关于」这类内置动作要打开设置窗口的某一页：窗口还没建时先记在这儿。
thread_local! {
    static REQUESTED_PAGE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// 页面 key（调试环境变量和内置动作共用一套写法）。
fn page_from_key(key: &str) -> Option<Page> {
    match key.trim().to_ascii_lowercase().as_str() {
        "appearance" | "外观" | "" => Some(Page::Appearance),
        "general" | "常规" | "通用" => Some(Page::General),
        "items" | "菜单项" => Some(Page::Items),
        "actions" | "动作" | "动作库" => Some(Page::Actions),
        "tray" | "托盘" => Some(Page::Tray),
        "behavior" | "行为" => Some(Page::Behavior),
        "programs" | "程序" | "程序搜索" | "索引搜索" | "索引" => Some(Page::Programs),
        "filenames" | "文件名" | "file" | "文件" => Some(Page::FileNames),
        "hotstring" | "hotstrings" | "热字符串" => Some(Page::Hotstrings),
        "hotkeys" | "hotkey" | "热键" | "快捷键" => Some(Page::Hotkeys),
        "magic" | "一键直达" | "直达" | "正则直达" => Some(Page::Magic),
        "auto" | "自动规则" | "自动" | "规则" => Some(Page::Auto),
        "config" | "配置" | "配置备份" | "备份" => Some(Page::Config),
        "about" | "关于" => Some(Page::About),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    General,
    Items,
    Actions,
    Tray,
    Programs,
    FileNames,
    Hotstrings,
    Hotkeys,
    Magic,
    Auto,
    Behavior,
    Config,
    About,
}

impl Page {
    const ALL: [Page; 14] = [
        // 常规排最前：换机器、改开机启动这些是刚装完就要碰的
        Page::General,
        Page::Items,
        Page::Actions,
        Page::Appearance,
        Page::Tray,
        Page::Programs,
        Page::FileNames,
        Page::Hotstrings,
        Page::Hotkeys,
        Page::Magic,
        Page::Auto,
        Page::Behavior,
        Page::Config,
        Page::About,
    ];

    fn title(self) -> &'static str {
        match self {
            // 页面名要「菜单项」，而「菜单项」这个字面量在别处指的是**类型**
            // （英文该是 `Item`）。一个 key 挂一个译文，所以这里直接给两版。
            Page::Items => crate::i18n::either("菜单项", "Menu Items"),
            _ => crate::i18n::t(self.title_key()),
        }
    }

    fn title_key(self) -> &'static str {
        match self {
            Page::Appearance => "菜单外观",
            Page::General => "常规",
            Page::Items => "菜单项页",
            Page::Actions => "动作库",
            Page::Tray => "托盘图标",
            Page::Programs => "索引搜索",
            Page::FileNames => "文件名",
            Page::Hotstrings => "热字符串",
            Page::Hotkeys => "热键",
            Page::Magic => "一键直达",
            Page::Auto => "自动规则",
            Page::Auto => "自动规则",
            Page::Behavior => "行为与占位符",
            Page::Config => "配置与备份",
            Page::About => "关于",
        }
    }

    fn nav(self) -> &'static str {
        match self {
            Page::Items => crate::i18n::either("菜单项", "Menu Items"),
            _ => crate::i18n::t(self.nav_key()),
        }
    }

    fn nav_key(self) -> &'static str {
        match self {
            Page::Appearance => "外观",
            Page::General => "常规",
            Page::Items => "菜单项页",
            Page::Actions => "动作库",
            Page::Tray => "托盘",
            Page::Programs => "索引搜索",
            Page::FileNames => "文件名",
            Page::Hotstrings => "热字符串",
            Page::Hotkeys => "热键",
            Page::Magic => "一键直达",
            Page::Auto => "自动规则",
            Page::Behavior => "行为",
            Page::Config => "配置",
            Page::About => "关于",
        }
    }

    fn subtitle(self) -> &'static str {
        crate::i18n::t(self.subtitle_key())
    }

    fn subtitle_key(self) -> &'static str {
        match self {
            Page::Appearance => "配色、尺寸与显示项，改完立刻生效",
            Page::General => "开机自启、界面语言、让路的程序、最近运行",
            Page::Items => "搜索筛选、按住一行就能拖动排序，Ctrl / Shift 点选多项；点行尾的编辑图标改文案 / 动作 / 快捷键 / 热字符串",
            Page::Actions => "内置动作的目录：搜一搜、看参数，点「添加到菜单」就能用；AHK 脚本也在这里配",
            Page::Tray => "托盘图标本身 + 它右键弹出的菜单（和菜单项同一套编辑方式）",
            Page::Programs => "「运行程序」只填文件名时自动补成完整路径；全盘查名字走 Everything",
            Page::FileNames => "菜单里用到的裸文件名，各自解析到哪、同名有几份",
            Page::Hotstrings => "敲一串字就执行菜单项（AHK 的热字符串）；短语本身在「菜单项」页给每一项配",
            Page::Auto => "启动后按间隔检测环境条件（WiFi 名、开机时长、IP、时间……），命中就自动执行菜单项，不用按任何键",
            Page::Hotkeys => "弹出菜单的全局快捷键、不抓选中内容的那一颗；菜单项自己的触发键从这里找",
            Page::Magic => "选中内容 + 一个快捷键 → 按规则表从上往下问，第一条命中就直接跑它，不弹菜单",
            Page::Behavior => "弹出前抓取选中内容；动作参数里的内置变量",
            Page::Config => "换菜单配置、备份与恢复、配置文件在哪",
            Page::About => "快捷键与操作说明",
        }
    }
}

thread_local! {
    /// 调试钩子只弹一次（`GPUI_MENU_DEBUG_KEYPAD`）
    static DEBUG_KEYPAD_SHOT: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
}

/// 「恢复默认」这类危险按钮的二次确认：第一次点只把按钮变成「再点一次确认」。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResetKind {
    /// 只恢复菜单项
    Items,
    /// 恢复全部设置
    All,
    /// 删掉当前这份菜单配置文件
    Profile,
}

/// 「文件名」页那列裸文件名怎么排。
///
/// 两把尺子：按**裸文件名**（也就是菜单里写的那个名字）或者按**解析出来的完整路径**。
/// 按路径排的时候，没解析到的（解析结果为 `None`）统一沉到最后 ——
/// 用户点这一下多半就是想先把「没找到路径」的那些挑出来看。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProgSort {
    NameAsc,
    NameDesc,
    PathAsc,
    PathDesc,
}

impl ProgSort {
    const ALL: [ProgSort; 4] = [
        ProgSort::NameAsc,
        ProgSort::NameDesc,
        ProgSort::PathAsc,
        ProgSort::PathDesc,
    ];

    fn label(self) -> &'static str {
        crate::i18n::t(match self {
            ProgSort::NameAsc => "名字 A→Z",
            ProgSort::NameDesc => "名字 Z→A",
            ProgSort::PathAsc => "路径 A→Z",
            ProgSort::PathDesc => "路径 Z→A",
        })
    }

    fn by_path(self) -> bool {
        matches!(self, ProgSort::PathAsc | ProgSort::PathDesc)
    }

    fn descending(self) -> bool {
        matches!(self, ProgSort::NameDesc | ProgSort::PathDesc)
    }
}

/// 「文件名」页排序：按裸名字或按解析出来的完整路径。
///
/// 按路径排时，**没解析到路径的一律沉到最后**（正序倒序都一样）：
/// 点「路径 Z→A」的人通常是想把「没找到」的那些先聚在一起看清，
/// 让它们混在中间反而更难找。
fn sort_program_names(sort: ProgSort, a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let fold = |text: &str| text.to_lowercase();
    if !sort.by_path() {
        return if sort.descending() {
            fold(b).cmp(&fold(a))
        } else {
            fold(a).cmp(&fold(b))
        };
    }
    let resolved = |name: &str| crate::progindex::peek(name).and_then(|probe| probe.resolved);
    match (resolved(a), resolved(b)) {
        (Some(left), Some(right)) => {
            if sort.descending() {
                fold(&right).cmp(&fold(&left))
            } else {
                fold(&left).cmp(&fold(&right))
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => fold(a).cmp(&fold(b)),
    }
}


/// 菜单项页顶部「配置文件」那一行正在做什么。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileEditKind {
    /// 给当前配置改名
    Rename,
    /// 拿当前配置复制一份新的
    Copy,
    /// 新建一份空的（一个菜单项都没有）
    Create,
}

/// 配置文件行的内联输入框（改名 / 建副本时那一行会临时变成输入框）。
struct ProfileEdit {
    kind: ProfileEditKind,
    input: Entity<TextInput>,
}

/// 按设置把开机自启的**系统状态**对齐（注册表 Run / 任务计划）。
///
/// 只在用户动这几个开关时调：`apply` 是幂等的，但每次要起一两个命令行进程，
/// 别塞进通用的 `update()` 里跟每次改配色一起跑。
fn apply_autostart(cx: &mut App) {
    let (enabled, use_task, as_admin) = settings::read(|settings| {
        (
            settings.autostart.enabled,
            settings.autostart.use_task,
            settings.autostart.as_admin,
        )
    });
    if let Err(err) = crate::autostart::apply(enabled, use_task, as_admin) {
        eprintln!("[popup-menu] 设置开机自启失败：{err}");
    }
    // 界面上「系统里的状态」那一栏跟着变
    crate::autostart::refresh_status();
    let _ = cx;
}

/// 状态小圆点 + 一句文字（「已注册」/「未注册」）。
fn status_chip(label: &'static str, on: bool) -> AnyElement {
    let label = crate::i18n::t(label);
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .size(px(6.))
                .rounded(px(3.))
                .bg(if on { theme::accent() } else { theme::track() }),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(if on {
                    theme::text_primary()
                } else {
                    theme::ui_text_tertiary()
                })
                .child(label),
        )
        .into_any_element()
}

/// 二次确认的有效期：超过这么久没再点，按钮自动恢复原样（免得一直亮着被误触）。
const CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// 正在拖的滑条。
struct Slide {
    min: f32,
    max: f32,
    step: f32,
    apply: fn(&mut MenuSettings, f32),
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// 颜色面板里正在拖的区域。
#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerDrag {
    /// 饱和度 / 明度方块
    Sv,
    /// 色相条
    Hue,
}

/// 点 ＋ 弹出的颜色面板：调色板（饱和度/明度方块 + 色相条）+ 十六进制 + 取色。
///
/// 交互是「实时预览 + 显式确定」：拖色块 / 手填 / 取色都会立刻刷到界面上（菜单能看见），
/// 但只有按「确定」才算数；点面板外面（或 Esc / ✕）一律回滚到打开面板时的颜色。
struct ColorPicker {
    dark: bool,
    field: Field,
    input: Entity<TextInput>,
    /// 点 ＋ 时鼠标在窗口里的位置，面板贴着它弹
    anchor: gpui::Point<Pixels>,
    /// 打开面板时这个色位的原值（取消时回滚到它；空串 = 自动派生）
    original: String,
    /// 面板自己的 HSV 状态：拖色块改它，手填 / 取色也会同步回来
    hue: f32,
    sat: f32,
    val: f32,
    drag: Option<PickerDrag>,
    /// 方块和色相条的矩形（拖拽换算用，canvas 每帧写进来）
    sv_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    hue_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _sub: Subscription,
}

/// 字体下拉框（开着的锚点位置）。
struct FontPicker {
    anchor: gpui::Point<Pixels>,
}

pub struct SettingsView {
    _store: Entity<settings::SettingsStore>,
    _sub: Subscription,
    page: Page,
    scroll: ScrollHandle,
    /// 正在拖右侧滚动条：`(按下时的鼠标 y, 按下时的滚动偏移)`，单位都是逻辑像素
    bar_drag: Option<(f32, f32)>,
    /// 菜单项页那棵树（搜索 / 筛选 / 拖动 / 增删改都在它自己身上）
    items_tree: Entity<crate::tree_view::TreeView>,
    /// 托盘页那棵树（就是 `__tray__` 那一支，和菜单项页同一套编辑器）
    tray_tree: Entity<crate::tree_view::TreeView>,
    sliding: Option<Slide>,
    /// 每条滑条在窗口里的矩形。**按需插**：新加一条滑条不用再记着改这个表
    /// （以前漏了 `opacity`，结果透明度那条怎么点都拖不动）。
    slider_bounds: RefCell<HashMap<&'static str, Rc<Cell<Option<Bounds<Pixels>>>>>>,
    color_picker: Option<ColorPicker>,
    /// 字体下拉框：开着的时候记一下弹出的位置
    font_picker: Option<FontPicker>,
    /// 菜单配置下拉框（点名字那一栏弹出来的那份列表）
    profile_picker: Option<FontPicker>,
    profile_scroll: ScrollHandle,
    /// 字体列表自己的滚动位置（系统字体可能有几百个）
    font_scroll: ScrollHandle,
    /// 「弹出前抓取选中内容」的复制按键输入框
    selection_keys: Entity<TextInput>,
    /// 「复制后最多等待」输入框（毫秒）
    selection_wait: Entity<TextInput>,
    /// 「按程序覆盖等待时长」每一行的输入框（键是 `proc|0` / `ms|0`）
    wait_inputs: std::collections::HashMap<String, Entity<TextInput>>,
    wait_subs: Vec<Subscription>,
    /// 弹出菜单的全局快捷键
    popup_hotkey: Entity<TextInput>,
    /// 同一份菜单、但不抓选中内容的那一颗（留空 = 不注册）
    popup_noselect: Entity<TextInput>,
    /// 「一键直达」的专用快捷键
    magic_hotkey: Entity<TextInput>,
    /// 「一键直达」页：拿一段文字试规则（只影响显示，不写设置）
    magic_test: Entity<TextInput>,
    _magic_subs: Vec<Subscription>,
    /// 托盘图标文件路径
    tray_icon: Entity<TextInput>,
    /// 悬停提示的内容模板：一个大编辑框，一行一条
    tip_text: Entity<crate::text_area::TextArea>,
    /// 上面那个编辑框的订阅（要留住，掉了就不跟着改了）
    _tip_sub: Subscription,
    /// 常规页：「打开文件夹的程序」输入框（第三方文件管理器）
    folder_opener_input: Entity<TextInput>,
    /// 自动规则页：拖动中的规则 id / 悬停到的落点下标
    auto_drag: Option<String>,
    auto_drop: Option<usize>,
    /// 动作库页：搜动作名的输入框
    action_query: Entity<TextInput>,
    /// 搜索框的订阅（不留住就不刷新了）
    _action_sub: Subscription,
    /// 「打开文件夹的程序」输入框的订阅（存着才不会被丢掉）
    _opener_sub: Subscription,
    /// 动作库页当前选中的分类（None = 全部）
    action_category: Option<crate::actions::Category>,
    /// 刚加进菜单的那条动作名（页面上给个反馈）
    action_added: Option<String>,
    /// 屏幕下方那一条短提示（「已添加」「已复制」这种），2.8 秒后自己消失
    toast: Option<(String, std::time::Instant)>,
    /// 动作库页：正在展开参数预览的那条动作 id
    action_preview: Option<String>,
    /// 预览里的参数值（只影响预览，不动配置）
    preview_params: std::collections::BTreeMap<String, String>,
    /// 「文件名」页正在后台算裸文件名的解析结果（算完一个刷一次）
    probe_task: bool,
    /// 程序搜索页：查一个名字（列出所有同名候选）
    prog_query: Entity<TextInput>,
    /// 程序搜索页的排除规则（一行一条）
    prog_exclude: Entity<crate::text_area::TextArea>,
    /// 程序搜索页：认哪些后缀名（; 分隔）
    prog_extensions: Entity<TextInput>,
    _prog_subs: Vec<Subscription>,
    /// Everything 联动：手动指定的 Everything.exe / 文件夹路径
    ev_exe: Entity<TextInput>,
    _ev_subs: Vec<Subscription>,
    /// 「文件名」页里，哪些名字展开了同名候选列表（点「选文件」展开）
    expanded_names: std::collections::HashSet<String>,
    /// 「文件名」页那一列的排序方式（按名字 / 按路径，正序 / 倒序）
    prog_sort: ProgSort,
    /// 「文件名」页：只过滤「菜单里用到的裸文件名」那一列
    prog_name_filter: Entity<TextInput>,
    /// 上面那个输入框的订阅（要留住，掉了就不跟着改了）
    _prog_name_sub: Subscription,
    /// 热字符串页的几个数字输入框（敲完立刻写回设置）
    hs_after: Entity<TextInput>,
    hs_path_chars: Entity<TextInput>,
    hs_ms: Entity<TextInput>,
    hs_offset_x: Entity<TextInput>,
    hs_offset_y: Entity<TextInput>,
    /// 结束字符（除了空格 / Tab / 回车，还有哪些符号算「打完了」）
    hs_end_chars: Entity<TextInput>,
    /// 热字符串页那几个输入框的订阅（要留住，掉了就不跟着改了）
    _hs_subs: Vec<Subscription>,
    /// 菜单项页「最近运行最多几条」
    recent_limit: Entity<TextInput>,
    _recent_subs: Vec<Subscription>,
    /// 「禁用应用」那一行的手动输入框
    block_input: Entity<TextInput>,
    /// 正在「拖到目标窗口上」拾取（等鼠标松开）
    picking: bool,
    /// 配置文件行：改名 / 建副本时临时出现的输入框
    profile_edit: Option<ProfileEdit>,
    /// 「删除这份配置」第一次点击的时间（3 秒内再点才真删）
    confirm_delete_profile: Option<std::time::Instant>,
    /// 已经在盯着后台扫描了（免得每帧都 spawn 一个轮询）
    /// 「恢复默认菜单」第一次点击的时间：3 秒内再点一次才真做
    confirm_reset_items: Option<std::time::Instant>,
    /// 「恢复默认设置」同上
    confirm_reset_all: Option<std::time::Instant>,
    /// 备份列表（进「关于」页时读一次；做了备份 / 恢复之后置空重读）
    backups: Vec<crate::backup::Entry>,
    backups_loaded: bool,
    /// 上一次备份 / 恢复的结果（就地显示一行字，不弹窗）
    backup_note: String,
    /// 「恢复这一份」第一次点击的时间（3 秒内再点同一份才真恢复）
    confirm_restore: Option<(String, std::time::Instant)>,
    /// 打开窗口 / 上次「应用」时的配置，取消就回滚到它
    committed: MenuSettings,
    /// 调试用：还没自动弹过调色板（`GPUI_MENU_DEBUG_PICKER=1`）
    debug_picker_pending: bool,
    /// 窗口外壳：HWND、拖动记号
    chrome: Chrome,
    focus: FocusHandle,
}

thread_local! {
    /// 规则表拖拽：按下的那条规则 + 起点（`dragging` 表示已经超过阈值）
    static RULE_DRAG: std::cell::RefCell<Option<(String, gpui::Point<gpui::Pixels>, bool)>> =
        const { std::cell::RefCell::new(None) };
    /// 每一行规则的位置范围（按 id），拖拽时用来算落点
    static RULE_BOUNDS: std::cell::RefCell<Vec<(String, gpui::Bounds<gpui::Pixels>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl SettingsView {
    fn new(hwnd: Rc<Cell<Option<isize>>>, cx: &mut Context<Self>) -> Self {
        let store = settings::store(cx).expect("设置还没安装");
        let store_sub = cx.observe(&store, |_this, _store, cx| cx.notify());
        let committed = settings::current();
        // 关窗兜底（Alt+F4 之类）用的回滚点
        arm_close_guard(&committed);

        // 调试用：`GPUI_MENU_DEBUG_PREVIEW=sys.volume` 进动作库页就把这条的参数预览摊开
        // （这台机器上模拟点击不可靠，看版式只能从代码里直接设状态）
        let debug_preview = std::env::var("GPUI_MENU_DEBUG_PREVIEW")
            .ok()
            .filter(|value| !value.is_empty());
        let preview_params = debug_preview
            .as_deref()
            .and_then(crate::actions::find)
            .map(crate::actions::defaults)
            .unwrap_or_default();

        // 「抓取选中内容」的复制按键（改一下立刻生效，下次弹菜单就用新的）
        let keys = settings::read(|settings| settings.selection_copy_keys.clone());
        let selection_keys = cx.new(|cx| TextInput::new(keys, "Ctrl+C", 11.5, cx));
        let keys_sub = {
            let input = selection_keys.clone();
            cx.observe(&input, move |_this, input, cx| {
                let text = input.read(cx).text();
                if settings::read(|settings| settings.selection_copy_keys.clone()) == text {
                    return;
                }
                settings::update(cx, |settings| settings.selection_copy_keys = text);
            })
        };
        // 「复制后最多等待」：毫秒数，输个非法值就不写回（保持上一个有效值）
        let wait_text = settings::read(|settings| settings.selection_wait_ms.to_string());
        let selection_wait = cx.new(|cx| TextInput::new(wait_text, "500", 11.5, cx));
        let wait_sub = {
            let input = selection_wait.clone();
            cx.observe(&input, move |_this, input, cx| {
                let text = input.read(cx).text();
                let Some(ms) = text.trim().parse::<u64>().ok() else {
                    return;
                };
                let ms = ms.clamp(60, 5000);
                if settings::read(|settings| settings.selection_wait_ms) == ms {
                    return;
                }
                settings::update(cx, |settings| settings.selection_wait_ms = ms);
            })
        };
        // 弹出菜单的全局快捷键：只有能解析的写法才写回设置（写错了输入框里可以继续改，
        // 系统那边保留上一个能用的键）
        let hotkey_text = settings::read(|settings| settings.popup_hotkey.clone());
        let popup_hotkey = cx.new(|cx| TextInput::new(hotkey_text, "Ctrl+Alt+M", 11.5, cx));
        let hotkey_sub = {
            let input = popup_hotkey.clone();
            cx.observe(&input, move |_this, input, cx| {
                let text = input.read(cx).text();
                if !crate::hotkey::is_valid(&text) {
                    return;
                }
                if settings::read(|settings| settings.popup_hotkey.clone()) == text {
                    return;
                }
                settings::update(cx, |settings| settings.popup_hotkey = text);
            })
        };
        // 「不抓选中内容」的那一颗：留空 = 不注册。允许空串写回，所以不用校验
        let noselect_text = settings::read(|settings| settings.popup_hotkey_noselect.clone());
        let popup_noselect = cx.new(|cx| {
            TextInput::new(
                noselect_text,
                crate::i18n::t("例如 Ctrl+Alt+N"),
                11.5,
                cx,
            )
        });
        let noselect_sub = {
            let input = popup_noselect.clone();
            cx.observe(&input, move |_this, input, cx| {
                let text = input.read(cx).text();
                if !text.trim().is_empty() && !crate::hotkey::is_valid(&text) {
                    return;
                }
                if settings::read(|settings| settings.popup_hotkey_noselect.clone()) == text {
                    return;
                }
                settings::update(cx, |settings| settings.popup_hotkey_noselect = text);
            })
        };
        // ---- 一键直达：专用快捷键 + 试一试的输入框 ----
        let magic_hotkey_text = settings::read(|settings| settings.magic.hotkey.clone());
        let magic_hotkey = cx.new(|cx| {
            TextInput::new(
                magic_hotkey_text,
                crate::i18n::t("例如 Ctrl+Alt+X"),
                11.5,
                cx,
            )
        });
        let magic_hotkey_sub = {
            let input = magic_hotkey.clone();
            cx.observe(&input, move |_this, input, cx| {
                let text = input.read(cx).text();
                // 留空 = 不注册；写错了就留上一颗能用的键
                if !text.trim().is_empty() && !crate::hotkey::is_valid(&text) {
                    return;
                }
                if settings::read(|settings| settings.magic.hotkey.clone()) == text {
                    return;
                }
                settings::update(cx, |settings| settings.magic.hotkey = text);
            })
        };
        let magic_test = cx.new(|cx| {
            TextInput::new(
                "",
                crate::i18n::t("粘贴一段文字，看看它命中哪条规则"),
                11.5,
                cx,
            )
        });
        let magic_test_sub = {
            let input = magic_test.clone();
            cx.observe(&input, move |_this, _input, cx| cx.notify())
        };
        // 托盘图标文件路径（留空 = 内置那个深色方块）
        let icon_text = settings::read(|settings| settings.tray_icon.clone());
        let tray_icon = cx.new(|cx| TextInput::new(icon_text, crate::i18n::t(".ico / .png 文件路径（留空用内置图标）"), 11.5, cx));
        let icon_sub = {
            let input = tray_icon.clone();
            cx.observe(&input, move |_this, input, cx| {
                let text = input.read(cx).text();
                if settings::read(|settings| settings.tray_icon.clone()) == text {
                    return;
                }
                settings::update(cx, |settings| settings.tray_icon = text);
            })
        };
        // 输入框右键菜单：全局状态一变就重绘（浮层画在窗口根上）
        let menu_sub = crate::text_menu::store(cx)
            .map(|menu| cx.observe(&menu, |_this, _menu, cx| cx.notify()));
        let mut subs = vec![store_sub, keys_sub, wait_sub, hotkey_sub, noselect_sub, icon_sub];
        if let Some(menu_sub) = menu_sub {
            subs.push(menu_sub);
        }
        let sub = subs
            .into_iter()
            .reduce(Subscription::join)
            .expect("至少有一个订阅");

        let items_tree = cx.new(|cx| crate::tree_view::TreeView::new(crate::tree_view::Scope::Root, cx));
        let tray_tree = cx.new(|cx| crate::tree_view::TreeView::new(crate::tree_view::Scope::Tray, cx));

        // 悬停提示的模板：一个大编辑框，换行 = 下一条
        let template = settings::read(|settings| settings.tooltip.template.clone());
        let tip_text = cx.new(|cx| {
            crate::text_area::TextArea::new(
                template,
                crate::i18n::t("一行一条；{label} 这类是占位符，下面有说明"),
                11.5,
                cx,
            )
        });
        let tip_sub = cx.observe(&tip_text, |this, _input, cx| this.tip_changed(cx));

        // 动作库页：搜动作的输入框
        let action_query = cx.new(|cx| {
            crate::text_input::TextInput::new(
                "",
                crate::i18n::t("搜动作：音量、窗口、剪贴板、脚本…"),
                11.5,
                cx,
            )
        });
        let action_sub = cx.observe(&action_query, |_this, _input, cx| cx.notify());
        let folder_opener_input = cx.new(|cx| {
            TextInput::new(
                settings::read(|settings| settings.folder_opener.clone()),
                "留空 = 资源管理器；例：dopusrt.exe /acmd go \"path={path}\"",
                11.5,
                cx,
            )
        });
        let opener_sub = cx.observe(&folder_opener_input, |_this, input, cx| {
            let text = input.read(cx).text();
            settings::update(cx, |settings| settings.folder_opener = text);
        });

        // 程序搜索页：查名字的输入框 + 排除规则的编辑框
        let prog_query = cx.new(|cx| {
            crate::text_input::TextInput::new(
                "",
                crate::i18n::t("输入文件名，看它解析到哪几个路径…"),
                11.5,
                cx,
            )
        });
        // 「文件名」页：只过滤「菜单里用到的裸文件名」那一列（按名字或解析出的路径匹配）
        let prog_name_filter = cx.new(|cx| {
            crate::text_input::TextInput::new(
                "",
                crate::i18n::t("在菜单用到的文件名里找…（名字或路径都认）"),
                11.0,
                cx,
            )
        });
        let prog_name_sub = cx.observe(&prog_name_filter, |_this, _input, cx| cx.notify());
        let exclude = settings::read(|settings| settings.programs.exclude.clone());        let prog_exclude = cx.new(|cx| {
            crate::text_area::TextArea::new(
                exclude,
                crate::i18n::t("一行一条：路径里包含这段文字就不收录"),
                11.0,
                cx,
            )
                .with_min_rows(3)
        });
        let extensions = settings::read(|settings| settings.programs.extensions.clone());
        let prog_extensions = cx.new(|cx| {
            crate::text_input::TextInput::new(
                extensions,
                crate::i18n::t("认哪些后缀，; 分隔，如 exe;bat;cmd;msi"),
                11.5,
                cx,
            )
        });
        let prog_subs = vec![
            cx.observe(&prog_query, |_this, _input, cx| cx.notify()),            cx.observe(&prog_exclude, |this, _input, cx| {
                let text = this.prog_exclude_text(cx);
                settings::update(cx, |settings| settings.programs.exclude = text);
                cx.notify();
            }),
            cx.observe(&prog_extensions, |this, _input, cx| {
                let text = this.prog_extensions_text(cx);
                settings::update(cx, |settings| settings.programs.extensions = text);
                cx.notify();
            }),
        ];

        // Everything 的路径输入框：改完立刻让后端重新找一遍（不用重启）
        let ev_exe_text = settings::read(|settings| settings.everything.exe.clone());
        let ev_exe = cx.new(|cx| {
            TextInput::new(
                ev_exe_text,
                crate::i18n::t(r"Everything.exe 的完整路径；留空自动找"),
                11.5,
                cx,
            )
        });
        let ev_subs = vec![cx.observe(&ev_exe, |_this, input, cx| {
            let text = input.read(cx).text();
            if settings::read(|settings| settings.everything.exe.clone()) == text {
                return;
            }
            settings::update(cx, |settings| settings.everything.exe = text);
            // 路径变了：DLL / es.exe 的探测结果作废（否则还是旧目录）
            crate::everything::reload();
            cx.notify();
        })];

        let hs = settings::read(|settings| settings.hotstrings.clone());
        let hs_after = cx.new(|cx| TextInput::new(hs.hint_after.to_string(), "3", 11.5, cx));
        let hs_path_chars =
            cx.new(|cx| TextInput::new(hs.hint_path_chars.to_string(), "10", 11.5, cx));
        let hs_ms = cx.new(|cx| TextInput::new(hs.hint_ms.to_string(), "6000", 11.5, cx));
        let hs_offset_x = cx.new(|cx| TextInput::new(hs.hint_x.to_string(), "0", 11.5, cx));
        let hs_offset_y = cx.new(|cx| TextInput::new(hs.hint_y.to_string(), "0", 11.5, cx));
        let hs_end_chars =
            cx.new(|cx| TextInput::new(hs.end_chars.clone(), "-()[]{}:;'\"/\\,.?!", 11.5, cx));
        let (after_lo, after_hi) = settings::HotstringSettings::limits_hint_after();
        let (path_lo, path_hi) = settings::HotstringSettings::limits_path_chars();
        let (ms_lo, ms_hi) = settings::HotstringSettings::limits_hint_ms();
        let (off_lo, off_hi) = settings::HotstringSettings::limits_offset();
        let hs_subs = vec![
            bind_number(&hs_after, cx, |s| s.hotstrings.hint_after as i64, |s, v| {
                s.hotstrings.hint_after = v as u32
            }, after_lo as i64, after_hi as i64),
            bind_number(&hs_path_chars, cx, |s| s.hotstrings.hint_path_chars as i64, |s, v| {
                s.hotstrings.hint_path_chars = v as u32
            }, path_lo as i64, path_hi as i64),
            bind_number(&hs_ms, cx, |s| s.hotstrings.hint_ms as i64, |s, v| {
                s.hotstrings.hint_ms = v as u64
            }, ms_lo as i64, ms_hi as i64),
            bind_number(&hs_offset_x, cx, |s| s.hotstrings.hint_x as i64, |s, v| {
                s.hotstrings.hint_x = v as i32
            }, off_lo as i64, off_hi as i64),
            bind_number(&hs_offset_y, cx, |s| s.hotstrings.hint_y as i64, |s, v| {
                s.hotstrings.hint_y = v as i32
            }, off_lo as i64, off_hi as i64),
            {
                let input = hs_end_chars.clone();
                cx.observe(&input, move |_this, input, cx| {
                    let text = input.read(cx).text();
                    if settings::read(|s| s.hotstrings.end_chars.clone()) == text {
                        return;
                    }
                    settings::update(cx, |s| s.hotstrings.end_chars = text);
                })
            },
        ];

        // 「最近运行」最多几条
        let recent = settings::read(|settings| settings.recent.clone());
        let recent_limit = cx.new(|cx| TextInput::new(recent.limit.to_string(), "5", 11.5, cx));
        let (recent_lo, recent_hi) = settings::RecentSettings::limits_limit();
        let recent_subs = vec![bind_number(
            &recent_limit,
            cx,
            |s| s.recent.limit as i64,
            |s, v| s.recent.limit = v.clamp(1, 20) as u32,
            recent_lo as i64,
            recent_hi as i64,
        )];

        let block_input =
            cx.new(|cx| TextInput::new(String::new(), crate::i18n::t("DNF.exe 或完整路径"), 11.5, cx));

        Self {
            _store: store,
            _sub: sub,
            // 调试用：`GPUI_MENU_DEBUG_PAGE=behavior` 直接落到某一页（省得手点）；
            // 「打开关于」这类内置动作走 `REQUESTED_PAGE`（不污染用户的环境变量）
            page: REQUESTED_PAGE
                .with(|slot| slot.borrow_mut().take())
                .as_deref()
                .and_then(page_from_key)
                .or_else(|| {
                    std::env::var("GPUI_MENU_DEBUG_PAGE")
                        .ok()
                        .and_then(|key| page_from_key(&key))
                })
                .unwrap_or(Page::Appearance),
            scroll: ScrollHandle::new(),
            bar_drag: None,
            items_tree,
            tray_tree,
            sliding: None,
            slider_bounds: RefCell::new(HashMap::new()),
            color_picker: None,
            font_picker: None,
            profile_picker: None,
            profile_scroll: ScrollHandle::new(),
            font_scroll: ScrollHandle::new(),
            selection_keys,
            selection_wait,
            wait_inputs: std::collections::HashMap::new(),
            wait_subs: Vec::new(),
            popup_hotkey,
            popup_noselect,
            magic_hotkey,
            magic_test,
            _magic_subs: vec![magic_hotkey_sub, magic_test_sub],
            tray_icon,
            tip_text,
            _tip_sub: tip_sub,
            action_query,
            auto_drag: None,
            auto_drop: None,
            action_category: None,
            action_added: None,
            toast: None,
            action_preview: debug_preview.clone(),
            preview_params,
            probe_task: false,
            _action_sub: action_sub,
            folder_opener_input,
            _opener_sub: opener_sub,
            prog_query,
            prog_exclude,
            prog_extensions,
            _prog_subs: prog_subs,
            ev_exe,
            _ev_subs: ev_subs,
            expanded_names: std::collections::HashSet::new(),
            prog_sort: ProgSort::NameAsc,
            prog_name_filter,
            _prog_name_sub: prog_name_sub,
            hs_after,
            hs_path_chars,
            hs_ms,
            hs_offset_x,
            hs_offset_y,
            hs_end_chars,
            _hs_subs: hs_subs,
            recent_limit,
            _recent_subs: recent_subs,
            block_input,
            picking: false,
            profile_edit: None,
            confirm_delete_profile: None,
            confirm_reset_items: None,
            confirm_reset_all: None,
            backups: Vec::new(),
            backups_loaded: false,
            backup_note: String::new(),
            confirm_restore: None,
            committed,
            debug_picker_pending: std::env::var_os("GPUI_MENU_DEBUG_PICKER").is_some(),
            chrome: Chrome::new(hwnd),
            focus: cx.focus_handle(),
        }
    }

    /// 危险按钮的两步确认：第一次点只是「上膛」，`CONFIRM_WINDOW` 内再点才真做。
    ///
    /// 上膛期间按钮文案会变成「再点一次确认」，超过时间自动撤回 ——
    /// 「恢复默认」一点就把用户辛苦排的菜单清掉，这种按钮不该一下就打出去。
    /// 按程序覆盖的那几行输入框（懒创建，键 `proc|下标` / `ms|下标`）。
    fn wait_field(
        &mut self,
        key: String,
        initial: String,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> Entity<TextInput> {
        if let Some(entity) = self.wait_inputs.get(&key) {
            return entity.clone();
        }
        let entity = cx.new(|cx| TextInput::new(initial, hint, 11.5, cx));
        let seen: std::rc::Rc<std::cell::Cell<Option<String>>> =
            std::rc::Rc::new(std::cell::Cell::new(None));
        let key_for_observer = key.clone();
        let sub = cx.observe(&entity, move |_view, input, cx| {
            let text = input.read(cx).text();
            if seen.replace(Some(text.clone())).as_deref() == Some(text.as_str()) {
                return;
            }
            let mut parts = key_for_observer.split('|');
            let field = parts.next().unwrap_or_default().to_string();
            let index = parts.next().and_then(|value| value.parse::<usize>().ok());
            settings::update(cx, move |settings| {
                let Some(rule) = index.and_then(|i| settings.selection_wait_rules.get_mut(i))
                else {
                    return;
                };
                match field.as_str() {
                    "proc" => rule.process = text.trim().to_string(),
                    "ms" => rule.wait_ms = text.trim().parse().unwrap_or(0),
                    _ => {}
                }
            });
        });
        self.wait_subs.push(sub);
        self.wait_inputs.insert(key, entity.clone());
        entity
    }

    fn confirmed(&mut self, which: ResetKind, cx: &mut Context<Self>) -> bool {
        let now = std::time::Instant::now();
        let armed = self.armed_at(which);
        let fire = armed.is_some_and(|at| now.duration_since(at) < CONFIRM_WINDOW);
        self.set_armed(which, (!fire).then_some(now));
        if !fire {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(CONFIRM_WINDOW).await;
                let _ = this.update(cx, |this, cx| {
                    // 只有「还是那一次上膛」才撤回：期间真做了的话这里是 None
                    if this.armed_at(which) == Some(now) {
                        this.set_armed(which, None);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        fire
    }

    fn armed_at(&self, which: ResetKind) -> Option<std::time::Instant> {
        match which {
            ResetKind::Items => self.confirm_reset_items,
            ResetKind::All => self.confirm_reset_all,
            ResetKind::Profile => self.confirm_delete_profile,
        }
    }

    fn set_armed(&mut self, which: ResetKind, value: Option<std::time::Instant>) {
        match which {
            ResetKind::Items => self.confirm_reset_items = value,
            ResetKind::All => self.confirm_reset_all = value,
            ResetKind::Profile => self.confirm_delete_profile = value,
        }
    }

    fn is_armed(&self, which: ResetKind) -> bool {
        self.armed_at(which)
            .is_some_and(|at| at.elapsed() < CONFIRM_WINDOW)
    }

    // ---- 底部「取消 / 确定 / 应用」----

    /// 「应用」：把切了一半的菜单配置认下来（真正写盘），然后刷新所有窗口，
    /// 并把回滚点推到当前状态。
    fn apply_changes(&mut self, cx: &mut Context<Self>) {
        settings::commit_pending_profile(cx);
        settings::update(cx, |_| {});
        self.committed = settings::current();
        arm_close_guard(&self.committed);
        cx.notify();
    }

    /// 取消：把配置整体还原到打开窗口（或上次应用）时的快照。
    /// 只是预览了另一份菜单配置的话，这一下就换回原来那份。
    fn cancel_changes(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.committed.clone();
        settings::cancel_pending_profile(cx);
        settings::update(cx, |settings| *settings = snapshot);
        arm_close_guard(&self.committed);
        // 模板可能被整体换回去了：输入框跟着重来一遍
        self.sync_tip_text(cx);
        cx.notify();
    }

    // ---- 悬停提示的模板编辑器 ----

    /// 编辑框里的整份模板。
    fn tip_value(&self, cx: &Context<Self>) -> String {
        self.tip_text.read(cx).text()
    }

    /// 把整份模板写回设置（内容没变就不写，免得和输入框自己打架）。
    fn tip_write(&mut self, cx: &mut Context<Self>) {
        let text = self.tip_value(cx);
        if settings::read(|settings| settings.tooltip.template.clone()) == text {
            return;
        }
        settings::update(cx, |settings| settings.tooltip.template = text);
    }

    /// 编辑框内容变了：写回设置 + 重画（下面的预览要跟着变）。
    fn tip_changed(&mut self, cx: &mut Context<Self>) {
        self.tip_write(cx);
        cx.notify();
    }

    /// 设置里的模板和编辑框里的内容对不上了（撤销 / 恢复默认之后）就把编辑框换掉。
    fn sync_tip_text(&mut self, cx: &mut Context<Self>) {
        let template = settings::read(|settings| settings.tooltip.template.clone());
        if template == self.tip_value(cx) {
            return;
        }
        self.tip_text
            .update(cx, |input, cx| input.set_text(template, cx));
    }

    /// 重置成默认模板。
    fn tip_reset(&mut self, cx: &mut Context<Self>) {
        let template = crate::tip_template::DEFAULT_TEMPLATE.to_string();
        self.tip_text
            .update(cx, |input, cx| input.set_text(template, cx));
        self.tip_write(cx);
        cx.notify();
    }

    /// 把 `{占位符}` 插到编辑框里的光标处。
    fn tip_insert(&mut self, name: &str, cx: &mut Context<Self>) {
        let token = format!("{{{name}}}");
        self.tip_text
            .update(cx, |input, cx| input.insert_text(&token, cx));
        self.tip_write(cx);
        cx.notify();
    }

    fn action_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let dirty = settings::current() != self.committed;
        // 切了菜单配置但没确认：说清「现在是预览，还没写盘」，别让人以为已经保存了
        let pending = settings::read(|settings| settings.pending_profile.clone());
        let message = match (&pending, dirty) {
            (Some(name), _) => format!(
                "● {}「{name}」—— {}",
                crate::i18n::t("已切到"),
                crate::i18n::t("点「应用 / 确定」才写入文件，点「取消」则什么都不留")
            ),
            (None, true) => crate::i18n::t("● 有未应用的改动（菜单里已经能预览到）").to_string(),
            (None, false) => crate::i18n::t("✓ 已应用到菜单 · 没有待应用的改动").to_string(),
        };
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
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(if dirty {
                        theme::accent()
                    } else {
                        theme::ui_text_tertiary()
                    })
                    .child(message),
            )
            // 撤销：回到上次「应用 / 确定」时的状态，不关窗口
            .child(ghost_button_if(
                "撤销",
                dirty,
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.cancel_changes(cx);
                    window.refresh();
                }),
            ))
            .child(ghost_button(
                "取消",
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.cancel_changes(cx);
                    window.remove_window();
                }),
            ))
            .child(primary_button(
                "确定",
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.apply_changes(cx);
                    window.remove_window();
                }),
            ))
            // 应用：没有改动就没得应用，按钮置灰
            .child(primary_button_if(
                "应用",
                dirty,
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.apply_changes(cx);
                    window.refresh();
                }),
            ))
            .into_any_element()
    }

    // ---- 滑条 ----

    /// 取这条滑条的「位置格子」；第一次用到就建一个。这样加新滑条不用同步改任何表。
    fn slider_cell(&self, id: &'static str) -> Rc<Cell<Option<Bounds<Pixels>>>> {
        self.slider_bounds
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| Rc::new(Cell::new(None)))
            .clone()
    }

    fn slide_with(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(slide) = self.sliding.as_ref() else {
            return;
        };
        let Some(bounds) = slide.bounds.get() else {
            return;
        };
        let width = f32::from(bounds.size.width).max(1.0);
        let ratio = ((x - f32::from(bounds.origin.x)) / width).clamp(0.0, 1.0);
        let raw = slide.min + ratio * (slide.max - slide.min);
        let step = slide.step.max(0.01);
        let value = ((raw / step).round() * step).clamp(slide.min, slide.max);
        let apply = slide.apply;
        settings::update(cx, |settings| apply(settings, value));
    }

    fn end_slide(&mut self, cx: &mut Context<Self>) {
        if self.sliding.take().is_some() {
            cx.notify();
        }
    }

    // ---- 颜色 ----

    fn set_color(&mut self, dark: bool, field: Field, value: String, cx: &mut App) {
        settings::update(cx, |settings| {
            *settings.colors_mut(dark).slot_mut(field) = value;
        });
    }

    /// 关掉颜色面板。`revert = true` 时把颜色退回打开面板前的值（点外面 / Esc / ✕）。
    fn close_color_picker(&mut self, revert: bool, cx: &mut Context<Self>) {
        let Some(picker) = self.color_picker.take() else {
            return;
        };
        if revert {
            self.set_color(picker.dark, picker.field, picker.original, cx);
        }
        cx.notify();
    }

    fn open_color_picker(
        &mut self,
        dark: bool,
        field: Field,
        current: String,
        anchor: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 已经开着一个面板：先把它撤回去（换色位也算「点了别处」）
        self.close_color_picker(true, cx);
        self.font_picker = None;
        let placeholder = match field {
            Field::Surface => "#ffffff",
            Field::Accent => "#2f6feb",
            Field::Selected => "#eaf1ff",
            Field::Text => "#1f2329",
            Field::Accel => "#1f2329",
            Field::Muted | Field::MutedUi => "#8a9099",
            Field::Border => "#d8dce3",
        };
        // 原值要留一份：面板上点点点都是「预览」，取消时按它回滚
        let original = settings::read(|settings| {
            let colors = if dark {
                &settings.dark
            } else {
                &settings.light
            };
            colors.slot(field).to_string()
        });
        // 调色板的初始位置按当前颜色反算；色值是空串（自动派生）就按派生出来的那个算
        let (hue, sat, val) = hex_hsv(&current).unwrap_or((210.0, 0.5, 0.9));
        let input = cx.new(|cx| TextInput::new(current, placeholder, 12.0, cx));
        let sub = cx.observe(&input, move |this, input, cx| {
            let text = input.read(cx).text();
            // 手填也要让调色板的方块 / 色相条跟着动
            if let Some((h, s, v)) = hex_hsv(&text) {
                this.set_color(dark, field, text, cx);
                if let Some(picker) = this.color_picker.as_mut() {
                    picker.hue = h;
                    picker.sat = s;
                    picker.val = v;
                }
                cx.notify();
            }
        });
        window.focus(&input.read(cx).focus_handle(cx));
        self.color_picker = Some(ColorPicker {
            dark,
            field,
            input,
            anchor,
            original,
            hue,
            sat,
            val,
            drag: None,
            sv_bounds: Rc::new(Cell::new(None)),
            hue_bounds: Rc::new(Cell::new(None)),
            _sub: sub,
        });
        cx.notify();
    }

    // ---- 调色板交互 ----

    /// 拖 / 点调色板：把鼠标位置换算成 (饱和度, 明度) 或色相，写进设置并同步色号输入框。
    fn picker_drag_to(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let (dark, field, hex, input) = {
            let Some(picker) = self.color_picker.as_mut() else {
                return;
            };
            let Some(area) = picker.drag else {
                return;
            };
            match area {
                PickerDrag::Sv => {
                    let Some(bounds) = picker.sv_bounds.get() else {
                        return;
                    };
                    let width = f32::from(bounds.size.width).max(1.0);
                    let height = f32::from(bounds.size.height).max(1.0);
                    picker.sat = ((f32::from(position.x) - f32::from(bounds.origin.x)) / width)
                        .clamp(0.0, 1.0);
                    picker.val = 1.0
                        - ((f32::from(position.y) - f32::from(bounds.origin.y)) / height)
                            .clamp(0.0, 1.0);
                }
                PickerDrag::Hue => {
                    let Some(bounds) = picker.hue_bounds.get() else {
                        return;
                    };
                    let width = f32::from(bounds.size.width).max(1.0);
                    let ratio = ((f32::from(position.x) - f32::from(bounds.origin.x)) / width)
                        .clamp(0.0, 1.0);
                    // 用 359.9 而不是 360：色相条最右端不会绕回红色
                    picker.hue = ratio * 359.9;
                }
            }
            let hex = hsv_hex(picker.hue, picker.sat, picker.val);
            (picker.dark, picker.field, hex, picker.input.clone())
        };
        // 色号跟着动（拖动时也能看到具体值）
        push_hex_to_input(&input, hex.clone(), cx);
        self.set_color(dark, field, hex, cx);
        cx.notify();
    }

    fn end_picker_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(picker) = self.color_picker.as_mut() {
            if picker.drag.take().is_some() {
                cx.notify();
            }
        }
    }

    /// 取色：点一下按钮，然后去屏幕上随便点一处，把那个像素的颜色拿回来。
    ///
    /// 只能轮询「左键有没有按下」—— 没有全局鼠标钩子，也不该为这个装一个。
    /// 先用 250ms 让「点按钮」这一下松手，否则读到的就是按钮自己的颜色。
    fn start_eyedropper(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let tick = |cx: &mut gpui::AsyncApp, ms: u64| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(ms))
            };
            // 先等「点按钮」这一下松手（最多 1 秒）
            tick(cx, 250).await;
            for _ in 0..50 {
                if !crate::win32::left_button_down() {
                    break;
                }
                tick(cx, 20).await;
            }
            // 之后 30 秒内的第一次左键点击 = 取色
            for _ in 0..1500 {
                tick(cx, 20).await;
                if !crate::win32::left_button_down() {
                    continue;
                }
                let Some(rgb) = crate::win32::screen_color_at_cursor() else {
                    break;
                };
                let _ = this.update(cx, |this, cx| this.apply_picked_color(rgb, cx));
                break;
            }
        })
        .detach();
    }

    /// 取色拿到的颜色：写进设置，调色板和色号输入框一起跟上。
    fn apply_picked_color(&mut self, rgb: u32, cx: &mut Context<Self>) {
        let Some((dark, field, input)) = self
            .color_picker
            .as_ref()
            .map(|picker| (picker.dark, picker.field, picker.input.clone()))
        else {
            return;
        };
        let hex = theme::to_hex(rgb);
        push_hex_to_input(&input, hex.clone(), cx);
        if let Some(picker) = self.color_picker.as_mut() {
            if let Some((h, s, v)) = hex_hsv(&hex) {
                picker.hue = h;
                picker.sat = s;
                picker.val = v;
            }
        }
        self.set_color(dark, field, hex, cx);
        cx.notify();
    }

    /// 打开中的浮层：一层「点哪儿都算取消」的遮罩 + 面板本身。
    /// 遮罩要盖满整个窗口（`.occlude()` 吃掉点击），面板画在它后面所以压在遮罩上面。
    /// 弹一条短提示（「已添加」「已复制」这种）。
    ///
    /// 之前这类反馈只是一行小灰字挂在列表标题旁边，用户根本看不见，
    /// 点完「添加到菜单」以为没反应 —— 这里改成屏幕下方居中、带底色的条，
    /// 2.8 秒自己消失，点一下也能立刻关掉。
    fn toast(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let at = std::time::Instant::now();
        self.toast = Some((text.into(), at));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(2800))
                .await;
            let _ = this.update(cx, |this, cx| {
                // 期间又弹了新的一条：别把新的那条也清掉
                if this.toast.as_ref().map(|(_, t)| *t) == Some(at) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn toast_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (text, _) = self.toast.as_ref()?;
        Some(
            div()
                .id("settings-toast")
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(58.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("settings-toast-pill")
                        .max_w(px(560.))
                        .px(px(14.))
                        .py(px(8.))
                        .rounded(px(9.))
                        .bg(theme::text_primary())
                        .text_color(theme::surface_window())
                        .text_size(px(11.5))
                        .cursor_pointer()
                        .shadow_lg()
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.toast = None;
                            cx.notify();
                        }))
                        .child(text.clone()),
                )
                .into_any_element(),
        )
    }

    fn floating_layers(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut layers = Vec::new();
        if let Some(toast) = self.toast_layer(cx) {
            layers.push(toast);
        }
        if self.color_picker.is_some() {
            layers.push(self.picker_overlay(cx));
        }
        if self.font_picker.is_some() {
            layers.push(self.font_overlay(cx));
        }
        if self.profile_picker.is_some() {
            layers.push(self.profile_overlay(cx));
        }
        if let Some(panel) = self.color_picker_panel(window, cx) {
            layers.push(panel);
        }
        if let Some(panel) = self.font_picker_panel(window, cx) {
            layers.push(panel);
        }
        if let Some(panel) = self.profile_picker_panel(window, cx) {
            layers.push(panel);
        }
        // 按键面板：自带遮罩，盖在别的浮层上面
        // 输入框右键菜单：盖在最上面
        layers.extend(crate::text_menu::layer(window, cx));
        layers
    }

    fn picker_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("color-picker-overlay")
            .occlude()
            .absolute()
            .left_0()
            .top_0()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.close_color_picker(true, cx);
                }),
            )
            .into_any_element()
    }

    // ---- 字体下拉 ----

    fn open_font_picker(&mut self, anchor: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.close_color_picker(true, cx);
        self.font_picker = Some(FontPicker { anchor });
        // 打开时滚到当前字体那一条（列表第 0 条是「系统默认」）
        let index = settings::read(|settings| {
            settings::available_fonts()
                .iter()
                .position(|name| *name == settings.font_family)
        });
        self.font_scroll
            .scroll_to_item(index.map(|i| i + 1).unwrap_or(0));
        cx.notify();
    }

    fn font_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("font-picker-overlay")
            .occlude()
            .absolute()
            .left_0()
            .top_0()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.font_picker = None;
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// 下拉框本身（点一下展开字体面板）。
    fn font_dropdown(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> AnyElement {
        let open = self.font_picker.is_some();
        let name = if settings.font_family.trim().is_empty() {
            crate::i18n::t("系统默认").to_string()
        } else {
            settings.font_family.clone()
        };
        div()
            .id("font-dropdown")
            .w(px(196.))
            .h(px(28.))
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(7.))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(if open {
                theme::accent()
            } else {
                theme::border_strong()
            })
            .cursor_pointer()
            .hover(|style| style.border_color(theme::accent()))
            .on_click(cx.listener(|this, event: &ClickEvent, _window, cx| {
                if this.font_picker.is_some() {
                    this.font_picker = None;
                    cx.notify();
                    return;
                }
                this.open_font_picker(event.position(), cx);
            }))
            // 下拉框里也用选中的字体显示自己，所见即所得
            .when(!settings.font_family.trim().is_empty(), |style| {
                style.font_family(settings.font_family.clone())
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(theme::text_primary())
                    .child(name),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(9.))
                    .text_color(theme::ui_text_tertiary())
                    .child("▾"),
            )
            .into_any_element()
    }

    /// 点 ＋ 之后的浮动颜色面板：调色板（饱和度/明度方块 + 色相条）+ 十六进制 + 取色。
    fn color_picker_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.color_picker.as_ref()?;
        let (dark, field) = (picker.dark, picker.field);
        let viewport = window.viewport_size();
        const PANEL_W: f32 = 236.0;
        const PANEL_H: f32 = 286.0;
        const SV_W: f32 = 218.0;
        const SV_H: f32 = 132.0;
        let x = f32::from(picker.anchor.x) - 60.0;
        let y = f32::from(picker.anchor.y) + 14.0;
        let x = x.clamp(8.0, (f32::from(viewport.width) - PANEL_W - 8.0).max(8.0));
        let y = y.clamp(8.0, (f32::from(viewport.height) - PANEL_H - 8.0).max(8.0));

        let field_label = crate::i18n::t(match field {
            Field::Surface => "背景色",
            Field::Accent => "强调色",
            Field::Selected => "选中项",
            Field::Text => "文字",
            Field::Accel => "加速键",
            Field::Muted => "菜单提示",
            Field::MutedUi => "设置页提示",
            Field::Border => "边框",
        });

        // 调色板的两个区域：饱和度/明度方块 + 色相条
        let hue_rgb = theme::parse_hex(&hsv_hex(picker.hue, 1.0, 1.0)).unwrap_or(0);
        let dot_x = (SV_W * picker.sat - 6.0).clamp(0.0, SV_W - 12.0);
        let dot_y = (SV_H * (1.0 - picker.val) - 6.0).clamp(0.0, SV_H - 12.0);

        let sv_bounds = picker.sv_bounds.clone();
        let sv = div()
            .id("color-sv")
            .relative()
            .w(px(SV_W))
            .h(px(SV_H))
            .flex_none()
            .rounded(px(6.))
            .overflow_hidden()
            .cursor_pointer()
            // 底色 = 当前色相的纯色，上面盖两层渐变就是标准的 S/V 方块
            .bg(rgba(hue_rgb << 8 | 0xff))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full()
                    .bg(linear_gradient(
                        90.0,
                        linear_color_stop(rgba(0xffffffff), 0.0),
                        linear_color_stop(rgba(0xffffff00), 1.0),
                    )),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full()
                    .bg(linear_gradient(
                        180.0,
                        linear_color_stop(rgba(0x00000000), 0.0),
                        linear_color_stop(rgba(0x000000ff), 1.0),
                    )),
            )
            .child(
                div()
                    .absolute()
                    .left(px(dot_x))
                    .top(px(dot_y))
                    .size(px(12.))
                    .rounded(px(6.))
                    .border_2()
                    .border_color(rgba(0xffffffff))
                    .shadow_sm(),
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, _, _| sv_bounds.set(Some(bounds)),
                )
                .absolute()
                .left_0()
                .top_0()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    if let Some(picker) = this.color_picker.as_mut() {
                        picker.drag = Some(PickerDrag::Sv);
                    }
                    this.picker_drag_to(event.position, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.picker_drag_to(event.position, cx);
                } else {
                    this.end_picker_drag(cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| this.end_picker_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| this.end_picker_drag(cx)),
            );

        // 色相条：36 段纯色拼出来（gpui 的渐变只支持两个 stop，拼段更准）
        let hue_bounds = picker.hue_bounds.clone();
        let mut strip = div()
            .id("color-hue")
            .relative()
            .w(px(SV_W))
            .h(px(13.))
            .flex_none()
            .flex()
            .rounded(px(6.))
            .overflow_hidden()
            .cursor_pointer();
        for step in 0..36 {
            let rgb = theme::parse_hex(&hsv_hex(step as f32 * 10.0, 1.0, 1.0)).unwrap_or(0);
            strip = strip.child(div().flex_1().h_full().bg(rgba(rgb << 8 | 0xff)));
        }
        let marker_x = ((SV_W - 5.0) * (picker.hue / 359.9)).clamp(0.0, SV_W - 5.0);
        strip = strip
            .child(
                div()
                    .absolute()
                    .left(px(marker_x))
                    .top(px(-1.))
                    .w(px(5.))
                    .h(px(15.))
                    .rounded(px(2.5))
                    .bg(rgba(0xffffffff))
                    .border_1()
                    .border_color(rgba(0x00000040)),
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, _, _| hue_bounds.set(Some(bounds)),
                )
                .absolute()
                .left_0()
                .top_0()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    if let Some(picker) = this.color_picker.as_mut() {
                        picker.drag = Some(PickerDrag::Hue);
                    }
                    this.picker_drag_to(event.position, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.picker_drag_to(event.position, cx);
                } else {
                    this.end_picker_drag(cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| this.end_picker_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| this.end_picker_drag(cx)),
            );

        // 色号 + 取色
        let hex_row = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(theme::surface_sunken())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(picker.input.clone()),
            )
            .child(
                div()
                    .id("color-eyedrop")
                    .h(px(28.))
                    .px(px(10.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(theme::surface_hover())
                    .border_1()
                    .border_color(theme::border_strong())
                    .text_size(px(11.5))
                    .text_color(theme::text_primary())
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::accent()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.start_eyedropper(cx);
                    }))
                    .child(crate::i18n::t("取色")),
            );

        let panel = div()
            .id("color-picker")
            .occlude()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(PANEL_W))
            .flex()
            .flex_col()
            .gap(px(7.))
            .p(px(9.))
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::accent())
            .bg(theme::surface_elevated())
            .shadow_sm()
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
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_primary())
                            .child(format!(
                                "{} · {}",
                                crate::i18n::t(if dark { "深色" } else { "浅色" }),
                                field_label
                            )),
                    )
                    .child(
                        div()
                            .id("color-picker-close")
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.close_color_picker(true, cx);
                            }))
                            .child("✕"),
                    ),
            )
            .child(sv)
            .child(strip)
            .child(hex_row)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("拖色块选色 · 色号可直接编辑")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("点外面取消")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id("color-auto")
                            .h(px(24.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .cursor_pointer()
                            .hover(|style| {
                                style
                                    .bg(theme::surface_hover())
                                    .text_color(theme::text_primary())
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                // 清掉覆盖 = 回到自动派生（这是一次明确的操作，直接生效）
                                this.set_color(dark, field, String::new(), cx);
                                this.color_picker = None;
                                window.refresh();
                                cx.notify();
                            }))
                            .child(crate::i18n::t("↺ 自动")),
                    )
                    .child(div().flex_1())
                    .child(primary_button(
                        "确定",
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            // 预览期间已经写进设置了，这里只要关面板（不回滚）
                            this.close_color_picker(false, cx);
                            window.refresh();
                        }),
                    )),
            )
            .into_any_element();
        Some(panel)
    }

    /// 字体下拉面板：列出系统里装了的全部字体，选中即生效。
    fn font_picker_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.font_picker.as_ref()?;
        let viewport = window.viewport_size();
        let panel_w = 236.0_f32;
        let panel_h = 300.0_f32;
        let x = (f32::from(picker.anchor.x) - panel_w + 30.0)
            .clamp(8.0, (f32::from(viewport.width) - panel_w - 8.0).max(8.0));
        let y = (f32::from(picker.anchor.y) + 24.0)
            .clamp(8.0, (f32::from(viewport.height) - panel_h - 8.0).max(8.0));

        let current = settings::read(|settings| settings.font_family.clone());
        let mut list = div().flex().flex_col();
        // 第 0 条固定是「系统默认」
        let mut options: Vec<String> = vec![String::new()];
        options.extend(settings::available_fonts().iter().cloned());

        for (index, name) in options.into_iter().enumerate() {
            let selected = name == current;
            let picked = name.clone();
            let label = if name.is_empty() {
                crate::i18n::t("系统默认").to_string()
            } else {
                name.clone()
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("font-opt-{index}")))
                    .h(px(24.))
                    .flex_none()
                    .px(px(7.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(selected, |style| {
                        style.bg(theme::accent_soft()).text_color(theme::accent())
                    })
                    .when(!selected, |style| {
                        style
                            .text_color(theme::text_primary())
                            .hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let name = picked.clone();
                        settings::update(cx, |settings| settings.font_family = name);
                        this.font_picker = None;
                        window.refresh();
                        cx.notify();
                    }))
                    // 每一条都用它自己的字体画，选之前就能看见长什么样
                    .when(!name.is_empty(), |style| style.font_family(name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5))
                            .child(label),
                    )
                    .when(selected, |style| style.child("✓")),
            );
        }

        let panel = div()
            .id("font-picker")
            .occlude()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(panel_w))
            .h(px(panel_h))
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(6.))
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::accent())
            .bg(theme::surface_elevated())
            .shadow_sm()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(4.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(if crate::i18n::is_english() {
                                format!("{} installed fonts", settings::available_fonts().len())
                            } else {
                                format!("已安装字体 · {} 款", settings::available_fonts().len())
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("点外面关闭")),
                    ),
            )
            .child(
                div()
                    .id("font-picker-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.font_scroll)
                    .child(list),
            )
            .into_any_element();
        Some(panel)
    }

    // ---- 菜单配置下拉 ----

    fn profile_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("profile-picker-overlay")
            .occlude()
            .absolute()
            .left_0()
            .top_0()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.profile_picker = None;
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// 点「菜单配置」那一栏弹出来的列表。
    ///
    /// 以前是 ◀ 名字 ▶ 两个箭头一个个翻：有三五份配置时就得很没效率地来回点，
    /// 也看不出到底有几套。换成下拉之后一眼看全，点哪套切哪套。
    fn profile_picker_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.profile_picker.as_ref()?;
        let viewport = window.viewport_size();
        let names = crate::menu_files::list();
        let rows = names.len().max(1) as f32;
        let panel_w = 232.0_f32;
        let panel_h = (34.0 + rows * 26.0).clamp(74.0, 320.0);
        let x = f32::from(picker.anchor.x) - 40.0;
        let y = f32::from(picker.anchor.y) + 16.0;
        let x = x.clamp(8.0, (f32::from(viewport.width) - panel_w - 8.0).max(8.0));
        let y = y.clamp(8.0, (f32::from(viewport.height) - panel_h - 8.0).max(8.0));

        let current = settings::read(|settings| settings.shown_profile());
        let mut list = div().flex().flex_col().gap(px(1.));
        if names.is_empty() {
            list = list.child(
                div()
                    .px(px(7.))
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("目录里还没有菜单配置文件")),
            );
        }
        for (index, name) in names.into_iter().enumerate() {
            let selected = name == current;
            let picked = name.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("profile-opt-{index}")))
                    .h(px(25.))
                    .flex_none()
                    .px(px(7.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(selected, |style| {
                        style.bg(theme::accent_soft()).text_color(theme::accent())
                    })
                    .when(!selected, |style| {
                        style
                            .text_color(theme::text_primary())
                            .hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let name = picked.clone();
                        if let Err(err) = settings::switch_profile(cx, &name) {
                            eprintln!("[popup-menu] 切换配置失败：{err}");
                        }
                        this.profile_picker = None;
                        this.items_tree.update(cx, |tree, cx| tree.reset_state(cx));
                        this.tray_tree.update(cx, |tree, cx| tree.reset_state(cx));
                        window.refresh();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(10.))
                            .text_size(px(10.))
                            .child(if selected { "✓" } else { "" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5))
                            .child(name),
                    )
                    .when(selected, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(px(9.5))
                                .text_color(theme::ui_text_tertiary())
                                .child(crate::i18n::t("当前")),
                        )
                    }),
            );
        }

        Some(
            div()
                .id("profile-picker")
                .occlude()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(panel_w))
                .h(px(panel_h))
                .flex()
                .flex_col()
                .gap(px(3.))
                .p(px(6.))
                .rounded(px(theme::RADIUS_CARD))
                .border_1()
                .border_color(theme::accent())
                .bg(theme::surface_elevated())
                .shadow_sm()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(4.))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(10.))
                                .text_color(theme::ui_text_tertiary())
                                .child(crate::i18n::t("换一套只动菜单项，配色 / 快捷键不跟着变")),
                        )
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(theme::ui_text_tertiary())
                                .child(crate::i18n::t("点外面关闭")),
                        ),
                )
                .child(
                    div()
                        .id("profile-picker-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.profile_scroll)
                        .child(list),
                )
                .into_any_element(),
        )
    }

}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 调试用：`GPUI_MENU_DEBUG_SCROLL=520` 进页面就滚到这个位置。
        // 截图脚本在这个环境里发不进真实的滚轮消息，只能从代码里直接设。
        // 从外面拖进来的网址 / 文本：钩子那边已经记下了，这里取走变成菜单项。
        // 只在「菜单项」页且编辑窗口没开的时候加（编辑窗口有自己的落点逻辑）。
        if let Ok(value) = std::env::var("GPUI_MENU_DEBUG_SCROLL") {
            if let Ok(value) = value.parse::<f32>() {
                self.scroll.set_offset(gpui::point(px(0.), px(-value)));
            }
        }
        // 调试用：`GPUI_MENU_DEBUG_KEYPAD=1` 进热键页就把按键面板弹出来
        if std::env::var_os("GPUI_MENU_DEBUG_KEYPAD").is_some()
            && self.page == Page::Hotkeys
            && !crate::keypad::any_open(cx)
            && !DEBUG_KEYPAD_SHOT.with(|slot| *slot.borrow())
        {
            DEBUG_KEYPAD_SHOT.with(|slot| *slot.borrow_mut() = true);
            // 不能在 render 里直接开窗口（gpui 会崩）：等这一帧过去再开
            let target = self.popup_hotkey.clone();
            cx.spawn(async move |_this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let _ = cx.update(|app| crate::keypad::open(target, false, None, app));
            })
            .detach();
        }
        // 调试用：`GPUI_MENU_DEBUG_PICKER=1` 时进来就弹出「背景色」的调色板
        if self.debug_picker_pending {
            self.debug_picker_pending = false;
            let current = settings::read(|settings| settings.light.effective(Field::Surface));
            self.open_color_picker(
                false,
                Field::Surface,
                current,
                gpui::point(px(90.), px(220.)),
                window,
                cx,
            );
        }

        // 跟随系统时，窗口外观一变就重算配色
        settings::set_system_dark(matches!(
            window.appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        ));

        // 换了菜单配置：以切过去那一刻为新基准。
        // 不这么做的话，「有未应用的改动」会一直亮着，点「撤销」还会把
        // 上一份配置的菜单树塞进这一份（切配置本身不是一种「改动」）。
        //
        // 但**正在预览、还没确认**的时候不能重设基准：那一下会把「有未应用的改动」
        // 直接抹掉，「应用」也就永远是灰的 —— 切配置必须先点应用才落盘。
        if settings::read(|settings| settings.pending_profile.is_none()) {
            let profile_now = settings::read(|settings| settings.effective_profile());
            if profile_now != self.committed.effective_profile() {
                self.committed = settings::current();
            }
        }

        let settings = settings::current();
        let page = self.page;
        let body = match page {
            Page::Appearance => self.appearance_page(&settings, cx),
            Page::General => self.general_page(&settings, cx),
            Page::Items => self.items_page(&settings, cx),
            Page::Actions => self.actions_page(&settings, cx),
            Page::Tray => self.tray_page(&settings, cx),
            Page::Hotkeys => self.hotkeys_page(&settings, cx),
            Page::Magic => self.magic_page(&settings, cx),
            Page::Behavior => self.behavior_page(&settings, cx),
            Page::Programs => self.programs_page(&settings, cx),
            Page::FileNames => self.file_names_page(&settings, cx),
            Page::Hotstrings => self.hotstrings_page(&settings, cx),
            Page::Auto => self.auto_page(&settings, cx),
            Page::Config => self.config_page(&settings, cx),
            Page::About => self.about_page(&settings, cx),
        };

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::surface_window())
            .text_color(theme::text_primary())
            // 设置界面也用菜单里选的那款字体，风格统一
            .font_family(theme::font_family())
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    let modifiers = event.keystroke.modifiers;
                    if (modifiers.control || modifiers.platform)
                        && event.keystroke.key.eq_ignore_ascii_case("z")
                    {
                        // 菜单项的撤销 / 重做（输入框里的 Ctrl+Z 走它自己那套，
                        // 命中时会 stop_propagation，到不了这里）
                        if modifiers.shift {
                            settings::redo_items(cx);
                        } else {
                            settings::undo_items(cx);
                        }
                        cx.notify();
                        return;
                    }
                    if (modifiers.control || modifiers.platform)
                        && event.keystroke.key.eq_ignore_ascii_case("y")
                    {
                        settings::redo_items(cx);
                        cx.notify();
                        return;
                    }
                    match event.keystroke.key.as_str() {
                        // Esc：先关输入框的右键菜单，再管颜色面板 / 字体下拉
                        "escape" | "esc" => {
                            if crate::keypad::any_open(cx) {
                                crate::keypad::close(cx);
                                window.refresh();
                                return;
                            }
                            if crate::text_menu::any_open(cx) {
                                crate::text_menu::close(cx);
                                return;
                            }
                            this.close_color_picker(true, cx);
                            this.font_picker = None;
                            this.profile_picker = None;
                            cx.notify();
                        }
                        // Enter：颜色面板算「确定」（手填完按回车很顺手）
                        "enter" | "return" if this.color_picker.is_some() => {
                            this.close_color_picker(false, cx);
                            cx.notify();
                        }
                        _ => {}
                    }
                }),
            )
            .child(self.header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.))
                                    .px(px(18.))
                                    .pt(px(14.))
                                    .pb(px(10.))
                                    .child(
                                        div()
                                            .text_size(px(17.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme::text_primary())
                                            .child(page.title()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(theme::ui_text_tertiary())
                                            .child(page.subtitle()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .relative()
                                    .flex()
                                    .child(self.scroll_area(body, window, cx))
                                    .child(self.scrollbar(cx)),
                            ),
                    ),
            )
            .child(self.action_bar(cx))
            .children(self.floating_layers(window, cx))
            .into_any_element()
    }
}

impl SettingsView {
    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let maximized = self.chrome.maximized();
        let chrome = self.chrome.clone();
        let chrome_max = self.chrome.clone();

        div()
            .id("settings-header")
            .h(px(42.))
            .flex_none()
            .flex()
            .items_center()
            .px(px(6.))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::surface())
            // 整条标题栏都能拖；双击 = 最大化 / 还原（chrome::drag_area 里处理的）
            .child(
                chrome::drag_area("settings-titlebar", &chrome)
                    .flex_1()
                    .min_w_0()
                    .child(chrome::title_row(true, crate::i18n::t("设置"))),
            )
            .child(chrome::caption_button(
                "—",
                "settings-min",
                |_event, window, _cx| {
                    window.minimize_window();
                },
            ))
            .child(chrome::caption_button(
                if maximized { "❐" } else { "□" },
                "settings-max",
                move |_event, window, _cx| chrome_max.toggle_maximize(window),
            ))
            // ✕ = 取消：有未应用的改动就先回滚再关窗（跟「取消」按钮一样），
            // 不能因为点的是右上角那一下就当成保存
            .child(chrome::caption_button_action(
                "✕",
                "settings-close",
                cx,
                |this, _: &ClickEvent, window, cx| {
                    this.cancel_changes(cx);
                    window.remove_window();
                },
            ))
            .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.page;
        let mut nav = div().flex().flex_col().gap(px(3.));
        for page in Page::ALL {
            let active = page == current;
            nav = nav.child(
                div()
                    .id(SharedString::from(format!("settings-nav-{}", page.title())))
                    .h(px(NAV_H))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .px(px(10.))
                    .rounded(px(theme::RADIUS_ROW))
                    .cursor_pointer()
                    .when(active, |style| style.bg(theme::accent_soft()))
                    .when(!active, |style| {
                        style.hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if this.page == page {
                            return;
                        }
                        this.page = page;
                        this.scroll.set_offset(gpui::point(px(0.), px(0.)));
                        window.refresh();
                        cx.notify();
                    }))
                    .child(dot(active))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(if active {
                                theme::text_primary()
                            } else {
                                theme::ui_text_secondary()
                            })
                            .child(page.nav()),
                    ),
            );
        }

        div()
            .w(px(SIDEBAR_W))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme::border())
            .bg(theme::surface_sunken())
            .p(px(8.))
            .child(nav)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .px(px(8.))
                    .py(px(4.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme::ui_text_secondary())
                            .child("RunAny_RS"),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            // 用编译时的真实版本号：以前是写死的 "v0.2.0"，
                            // 换了 exe 也看不出来，排查问题白费半天
                            .child(format!(
                                "v{} · {}",
                                env!("CARGO_PKG_VERSION"),
                                crate::i18n::t("便携版")
                            )),
                    ),
            )
            .into_any_element()
    }

    /// 自定义细滚动条：gpui 的 `overflow_y_scroll` 自己不画条，得照着偏移量手画。
    ///
    /// 两个坑：
    /// 1. `offset().y` 是**负数**（往下滚 → offset 越来越小），位置得用 `-offset`，
    ///    否则算出来是负的、被 clamp 到 0，滑块永远钉在最上面（「摆设」就是这么来的）；
    /// 2. 得自己挂鼠标事件，不然拖不动 —— 点轨道 = 跳到那里，按住滑块 = 跟着走。
    fn scrollbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let viewport = f32::from(self.scroll.bounds().size.height);
        let max = f32::from(self.scroll.max_offset().height);
        if max <= 1.0 || viewport <= 1.0 {
            return div().into_any_element();
        }
        const PAD: f32 = 4.0;
        let track_h = (viewport - PAD * 2.0).max(1.0);
        let offset = (-f32::from(self.scroll.offset().y)).clamp(0.0, max);
        let thumb_h = (viewport * viewport / (viewport + max)).clamp(MIN_BAR_THUMB.min(track_h), track_h);
        let top = offset / max * (track_h - thumb_h);

        // 轨道：点空白处直接跳过去；按住滑块就是拖
        let dragging = self.bar_drag.is_some();
        div()
            .id("settings-scrollbar")
            .occlude()
            .absolute()
            .right(px(PAD))
            .top(px(PAD))
            .w(px(BAR_HIT_W))
            .h(px(track_h))
            .cursor_pointer()
            .child(
                div()
                    .absolute()
                    .right(px(0.))
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
                    let base = f32::from(this.scroll.bounds().origin.y) + PAD;
                    let y = f32::from(event.position.y) - base;
                    let offset_now = -f32::from(this.scroll.offset().y);
                    let thumb_top = offset_now / max * (track_h - thumb_h);
                    if y >= thumb_top && y <= thumb_top + thumb_h {
                        // 按在滑块上：记下抓点，之后按位移走
                        this.bar_drag = Some((y, offset_now));
                    } else {
                        // 按在空白处：滚一屏，让滑块中心对准点击处
                        let ratio = ((y - thumb_h / 2.0) / (track_h - thumb_h)).clamp(0.0, 1.0);
                        this.scroll.set_offset(gpui::point(px(0.), px(-ratio * max)));
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                if let Some((grab_y, start)) = this.bar_drag {
                    let y = f32::from(event.position.y)
                        - (f32::from(this.scroll.bounds().origin.y) + PAD);
                    let span = (track_h - thumb_h).max(1.0);
                    let target = (start + (y - grab_y) / span * max).clamp(0.0, max);
                    this.scroll.set_offset(gpui::point(px(0.), px(-target)));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    if this.bar_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    if this.bar_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    fn scroll_area(
        &self,
        body: Vec<AnyElement>,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 每个 section 都必须 `flex_none`：`flex_col` 的子项默认可收缩，
        // 内容比视口高的时候它们会被一起压扁 —— 表现就是「窗口没最大化时后面的
        // 几块直接不见了，最大化才冒出来」。加上 flex_none 才是「超出就滚动」。
        let body: Vec<AnyElement> = body
            .into_iter()
            .map(|element| div().flex_none().w_full().min_w_0().child(element).into_any_element())
            .collect();
        // 内容的宽度**必须**是确定值（算出来的），不能靠 `w_full()` / 拉伸去推：
        // taffy 量「内容总高」时会用不确定宽度量一遍文本，长一点的段落会被量成
        // 好几十行，卡片底部就多出一大段空白（行为页「按选中内容切换菜单」实测多 1300px）。
        // 给它一个明确的 px 宽度后，卡片里的 `%` / `flex_1` 都能算出真实宽度。
        let inner_w = (f32::from(window.bounds().size.width) - SIDEBAR_W - PAD_X * 2.0
            - SCROLLBAR_GUTTER)
            .max(240.0);
        div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(PAD_X))
            .pt(px(2.))
            .flex()
            .flex_col()
            .child(
                div()
                    .w(px(inner_w))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .children(body),
            )
            // 拖滑条时指针早已离开原元素，所以 move / up 挂在整个滚动区上
            // （菜单项的拖动排序在 `tree_view.rs` 里自己处理，它有自己的滚动边界）
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.sliding.is_some() {
                    this.slide_with(f32::from(event.position.x), cx);
                }
                // 拖着滚动条时指针可能滑出那条细缝，这里兜一下
                if this.bar_drag.is_some() {
                    this.drag_bar_to(f32::from(event.position.y), cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.end_slide(cx);
                    this.bar_drag = None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.end_slide(cx);
                    this.bar_drag = None;
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// 拖动滚动条：按鼠标 y 反算偏移（和 `scrollbar()` 里那套几何一致）。
    fn drag_bar_to(&mut self, y: f32, cx: &mut Context<Self>) {
        let Some((grab_y, start)) = self.bar_drag else {
            return;
        };
        let viewport = f32::from(self.scroll.bounds().size.height);
        let max = f32::from(self.scroll.max_offset().height);
        if max <= 1.0 || viewport <= 1.0 {
            return;
        }
        const PAD: f32 = 4.0;
        let track_h = (viewport - PAD * 2.0).max(1.0);
        let thumb_h = (viewport * viewport / (viewport + max))
            .clamp(MIN_BAR_THUMB.min(track_h), track_h);
        let local = y - (f32::from(self.scroll.bounds().origin.y) + PAD);
        let span = (track_h - thumb_h).max(1.0);
        let target = (start + (local - grab_y) / span * max).clamp(0.0, max);
        self.scroll.set_offset(gpui::point(px(0.), px(-target)));
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // 外观页
    // -----------------------------------------------------------------------

    fn appearance_page(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mode_row = row_with_control(
            "主题模式",
            if settings::system_is_dark() {
                "系统当前是深色，跟随系统时用下面「深色」这一套"
            } else {
                "系统当前是浅色，跟随系统时用下面「浅色」这一套"
            },
            segmented(
                "theme-mode",
                &[(0, "跟随系统"), (1, "固定浅色"), (2, "固定深色")],
                match settings.theme_mode {
                    ThemeMode::FollowSystem => 0,
                    ThemeMode::Light => 1,
                    ThemeMode::Dark => 2,
                },
                cx.listener(|_this, value: &u32, window, cx| {
                    let mode = match value {
                        1 => ThemeMode::Light,
                        2 => ThemeMode::Dark,
                        _ => ThemeMode::FollowSystem,
                    };
                    settings::update(cx, |settings| settings.theme_mode = mode);
                    window.refresh();
                    cx.notify();
                }),
            ),
        );

        // 预览放在最上面：调外观时最常用的就是「开着菜单改一下、看一眼」
        let menu_open = popup::is_open(cx);
        let pinned = popup::is_pinned(cx);
        let preview = section(
            "预览",
            "菜单出现在鼠标位置；开着「保持显示」时点别的窗口也不会收起，方便边调边看",
            vec![div()
                .px(px(9.))
                .py(px(6.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(primary_button(
                    if menu_open {
                        "关闭菜单"
                    } else {
                        "打开菜单试试"
                    },
                    cx.listener(|_this, _: &ClickEvent, _window, cx| {
                        popup::toggle(cx);
                    }),
                ))
                // 保持显示：开着的时候 watchdog 不再因为失焦收起菜单
                .child(
                    div()
                        .id("pin-menu-hold")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(theme::RADIUS_ROW))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .on_click(cx.listener(|_this, _: &ClickEvent, window, cx| {
                            let next = !popup::is_pinned(cx);
                            popup::set_pinned(next, cx);
                            window.refresh();
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme::text_primary())
                                .child(crate::i18n::t("保持显示")),
                        )
                        .child(switch(pinned)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(if crate::i18n::is_english() {
                            format!(
                                "Hotkey {} pops up in any app",
                                settings.popup_hotkey.trim()
                            )
                        } else {
                            format!(
                                "快捷键 {} 在任何应用里都能唤出",
                                settings.popup_hotkey.trim()
                            )
                        }),
                )
                .into_any_element()],
        );

        vec![
            preview,
            section(
                "主题配色",
                "点一下整套换掉（浅色 / 深色各一份，跟随系统时两个模式都好看）；下面还能继续细调",
                vec![div()
                    .px(px(6.))
                    .pb(px(4.))
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(
                        theme::PRESETS
                            .iter()
                            .enumerate()
                            .map(|(index, preset)| {
                                preset_card(preset, preset.matches(&settings.light, &settings.dark), index, cx)
                            })
                            .collect::<Vec<_>>(),
                    )
                    .into_any_element()],
            ),
            section(
                "配色",
                "点色块即时预览；＋ 打开颜色面板（点色块 / 手填实时预览，按「确定」才算数，点外面取消）。「边框」是菜单卡片那一圈描边，不要就在「尺寸」里把「显示边框」关掉",
                vec![
                    mode_row,
                    div()
                        .px(px(6.))
                        .pb(px(4.))
                        .flex()
                        .gap(px(10.))
                        .child(self.color_block(false, &settings.light, cx))
                        .child(self.color_block(true, &settings.dark, cx))
                        .into_any_element(),
                ],
            ),
            section(
                "尺寸",
                "拖动即时生效，可以开着菜单对照调",
                {
                    let mut rows: Vec<AnyElement> = vec![row_with_control(
                        "列宽方式",
                        "「固定」所有列一样宽；「按内容」每一列各自按最长的菜单项算（不超过最大列宽），子菜单不会因为名字短而空一大块",
                        segmented(
                            "panel-width-mode",
                            &width_mode_options(),
                            PanelWidthMode::ALL
                                .iter()
                                .position(|mode| *mode == settings.panel_width_mode)
                                .unwrap_or(0) as u32,
                            cx.listener(|_this, value: &u32, window, cx| {
                                let mode = PanelWidthMode::ALL
                                    .get(*value as usize)
                                    .copied()
                                    .unwrap_or_default();
                                settings::update(cx, |settings| {
                                    settings.panel_width_mode = mode
                                });
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    )];
                    rows.push(row_with_control(
                        "子菜单展开",
                        "「贴名字」子菜单紧挨着父项那一列的边缘，第一行尽量和父项那一行齐平；下方放不下就整张往上挪，绝不切成半截（默认，手感最像原版）；「贴卡片」从整张卡片右边出来，不压任何一列，但离父项远",
                        segmented(
                            "submenu-anchor",
                            &submenu_anchor_options(),
                            SubmenuAnchor::ALL
                                .iter()
                                .position(|mode| *mode == settings.submenu_anchor)
                                .unwrap_or(0) as u32,
                            cx.listener(|_this, value: &u32, window, cx| {
                                let mode = SubmenuAnchor::ALL
                                    .get(*value as usize)
                                    .copied()
                                    .unwrap_or_default();
                                settings::update(cx, |settings| settings.submenu_anchor = mode);
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ));
                    match settings.panel_width_mode {
                        PanelWidthMode::Fixed => rows.push(self.slider(
                            "panel-width",
                            "列宽",
                            settings.panel_width,
                            MenuSettings::limits_panel_width().0,
                            MenuSettings::limits_panel_width().1,
                            2.0,
                            "px",
                            |settings, value| settings.panel_width = value,
                            cx,
                        )),
                        PanelWidthMode::Auto => rows.push(self.slider(
                            "panel-max-width",
                            "最大列宽",
                            settings.panel_max_width,
                            MenuSettings::limits_panel_max_width().0,
                            MenuSettings::limits_panel_max_width().1,
                            2.0,
                            "px",
                            |settings, value| settings.panel_max_width = value,
                            cx,
                        )),
                    }
                    rows.push(self.slider(
                        "row-height",
                        "行高",
                        settings.row_height,
                        MenuSettings::limits_row_height().0,
                        MenuSettings::limits_row_height().1,
                        1.0,
                        "px",
                        |settings, value| settings.row_height = value,
                        cx,
                    ));
                    rows.push(self.slider(
                        "blank-rest",
                        "空白停留收起",
                        settings.blank_rest_ms as f32,
                        80.0,
                        1500.0,
                        20.0,
                        "ms",
                        |settings, value| settings.blank_rest_ms = value as u32,
                        cx,
                    ));
                    rows.push(self.slider(
                        "radius",
                        "圆角",
                        settings.radius,
                        MenuSettings::limits_radius().0,
                        MenuSettings::limits_radius().1,
                        1.0,
                        "px",
                        |settings, value| settings.radius = value,
                        cx,
                    ));
                    rows.push(toggle_row(
                        "显示边框",
                        "菜单卡片外面那圈 1px 细线。关掉就是一块纯色，靠底色和阴影分边界（颜色在下面「配色」里调，浅色 / 深色各一份）",
                        settings.menu_border,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.menu_border = !settings.menu_border
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ));
                    rows.push(self.slider(
                        "badge-size",
                        "图标大小",
                        settings.badge_size,
                        MenuSettings::limits_badge_size().0,
                        MenuSettings::limits_badge_size().1,
                        1.0,
                        "px",
                        |settings, value| settings.badge_size = value,
                        cx,
                    ));
                    rows
                },
            ),
            section(
                "透明度",
                "整个菜单一起变淡（文字、图标、边框都跟着），不是只改底色",
                vec![self.slider(
                    "opacity",
                    "不透明度",
                    settings.opacity * 100.0,
                    35.0,
                    100.0,
                    1.0,
                    "%",
                    |settings, value| {
                        settings.opacity =
                            (value / 100.0).clamp(MenuSettings::limits_opacity().0, 1.0)
                    },
                    cx,
                )],
            ),
            section(
                "字体",
                "菜单和设置界面用同一款字体，选完立刻生效",
                vec![
                    row_with_control(
                        "字体",
                        "点开列出系统里装了的全部字体",
                        self.font_dropdown(settings, cx),
                    ),
                    row_with_control(
                        "字号",
                        "菜单里的文字整体缩放",
                        segmented(
                            "font-scale",
                            &[(90, "小"), (100, "标准"), (112, "大")],
                            (settings.font_scale * 100.0).round() as u32,
                            cx.listener(|_this, value: &u32, window, cx| {
                                settings::update(cx, |settings| {
                                    settings.font_scale = *value as f32 / 100.0;
                                });
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                ],
            ),
            section(
                "内容",
                "菜单里显示哪些东西",
                vec![
                    toggle_row(
                        "显示快捷键提示",
                        "菜单项右侧的 Ctrl+N 之类",
                        settings.show_shortcuts,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.show_shortcuts = !settings.show_shortcuts
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    toggle_row(
                        "显示图标位",
                        "菜单项左侧的小方块",
                        settings.show_badges,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.show_badges = !settings.show_badges
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    toggle_row(
                        "显示分隔线",
                        "关掉后会跳过所有分隔线",
                        settings.show_separators,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.show_separators = !settings.show_separators
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    toggle_row(
                        "自动取程序图标",
                        "图标位空着时，用第一个「运行程序」动作里那个 exe 的图标（单项可以在编辑窗口里关掉）",
                        settings.auto_icons,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.auto_icons = !settings.auto_icons
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    row_with_control(
                        "菜单里的搜索框",
                        "第一列顶上或底上放一条：打字就筛菜单项 / 子菜单（和「菜单项」页那套搜索一样）",
                        segmented(
                            "menu-search",
                            &search_place_options(),
                            SearchPlace::ALL
                                .iter()
                                .position(|place| *place == settings.menu_search)
                                .unwrap_or(0) as u32,
                            cx.listener(|_this, value: &u32, window, cx| {
                                let place = SearchPlace::ALL
                                    .get(*value as usize)
                                    .copied()
                                    .unwrap_or_default();
                                settings::update(cx, |settings| settings.menu_search = place);
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                ],
            ),
            // 悬停提示：鼠标在菜单项上停一会儿，弹一张小卡片说明它是什么
            {
                let tip = settings.tooltip.clone();
                let (min_w, max_w) = crate::settings::TooltipSettings::limits_width();
                let mut rows: Vec<AnyElement> = vec![
                    toggle_row(
                        "显示悬停提示",
                        "鼠标在菜单项上停一会儿，在光标旁边弹一张小卡片",
                        tip.enabled,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.tooltip.enabled = !settings.tooltip.enabled
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    self.slider(
                        "tip-width",
                        "最大卡片宽度",
                        tip.width,
                        min_w,
                        max_w,
                        4.0,
                        "px",
                        |settings, value| settings.tooltip.width = value,
                        cx,
                    ),
                ];
                let (min, max) = crate::settings::TooltipSettings::limits_delay();
                rows.push(self.slider(
                    "tooltip-delay",
                    "停多久弹出来",
                    tip.delay_ms as f32,
                    min,
                    max,
                    10.0,
                    "ms",
                    |settings, value| settings.tooltip.delay_ms = value as u64,
                    cx,
                ));
                // 对齐方式：全局默认，单行还能用 {left} / {center} / {right} 覆盖
                rows.push(row_with_control(
                    "对齐方式",
                    "整张卡片的默认对齐；某一行想单独对齐，行首插 {left} / {center} / {right}",
                    segmented(
                        "tip-align",
                        &align_options(),
                        crate::tip_template::TipAlign::ALL
                            .iter()
                            .position(|align| *align == tip.align)
                            .unwrap_or(0) as u32,
                        cx.listener(|_this, value: &u32, window, cx| {
                            let align = crate::tip_template::TipAlign::ALL
                                .get(*value as usize)
                                .copied()
                                .unwrap_or_default();
                            settings::update(cx, |settings| settings.tooltip.align = align);
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                ));
                rows.push(self.tip_editor(settings, cx));
                section(
                    "悬停提示",
                    "只是「看一眼」用的，不影响点击；关掉就完全不弹",
                    rows,
                )
            },
        ]
    }

    /// 一套配色的编辑块（浅色 / 深色各一块，左右并排）。
    fn color_block(&self, dark: bool, colors: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        let rows = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(self.color_row(dark, Field::Surface, "背景色", colors, cx))
            .child(self.color_row(dark, Field::Accent, "强调色", colors, cx))
            .child(self.color_row(dark, Field::Selected, "选中项", colors, cx))
            .child(self.color_row(dark, Field::Text, "文字", colors, cx))
            .child(self.color_row(dark, Field::Accel, "加速键", colors, cx))
            .child(self.color_row(dark, Field::Muted, "菜单提示", colors, cx))
            .child(self.color_row(dark, Field::MutedUi, "设置页提示", colors, cx))
            .child(self.color_row(dark, Field::Border, "边框", colors, cx));

        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(7.))
            .rounded(px(theme::RADIUS_ROW))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(2.))
                    .child(dot(false))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::ui_text_secondary())
                            .child(crate::i18n::t(if dark { "深色主题" } else { "浅色主题" })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .truncate()
                            .child(crate::i18n::t(if dark {
                                "系统为深色时使用"
                            } else {
                                "系统为浅色时使用"
                            })),
                    ),
            )
            .child(rows)
            .into_any_element()
    }

    fn color_row(
        &self,
        dark: bool,
        field: Field,
        label: &'static str,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = colors.slot(field).to_string();
        let fallback = colors.effective(field);
        let is_custom = !current.trim().is_empty();

        let mut swatches = div().flex().items_center().gap(px(3.)).flex_wrap();
        for color in candidates(dark, field) {
            let selected = is_custom && normalize(&current) == normalize(color);
            let value = color.to_string();
            swatches = swatches.child(
                div()
                    // id 必须带 dark：浅色/深色两段用同一份 label，不带就撞成同一个元素，
                    // 后面那一段（深色）的色块和 + 号会全都点不动
                    .id(SharedString::from(format!("sw-{dark}-{label}-{color}")))
                    .size(px(15.))
                    .flex_none()
                    .rounded(px(4.))
                    .bg(rgba(theme::parse_hex(color).unwrap_or(0) << 8 | 0xff))
                    .border_1()
                    .border_color(if selected {
                        theme::accent()
                    } else {
                        theme::border_strong()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_color(dark, field, value.clone(), cx);
                        window.refresh();
                        cx.notify();
                    })),
            );
        }

        let mut row = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    // 英文标签比中文长（"Background" 一行放不下），列宽跟着语言走
                    .w(px(if crate::i18n::is_english() { 78. } else { 64. }))
                    .flex_none()
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_secondary())
                    .child(crate::i18n::t(label)),
            )
            // 当前生效的颜色（没设过时就是自动值）
            .child(
                div()
                    .size(px(16.))
                    .flex_none()
                    .rounded(px(4.))
                    .bg(rgba(theme::parse_hex(&fallback).unwrap_or(0) << 8 | 0xff))
                    .border_1()
                    .border_color(if is_custom {
                        theme::accent()
                    } else {
                        theme::border_strong()
                    }),
            )
            .child(swatches);

        // ＋：手填十六进制
        row = row.child(
            div()
                .id(SharedString::from(format!("plus-{dark}-{label}")))
                .size(px(16.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(theme::surface_sunken())
                .border_1()
                .border_color(theme::border_strong())
                .text_size(px(10.))
                .text_color(theme::ui_text_tertiary())
                .cursor_pointer()
                .hover(|style| style.text_color(theme::text_primary()))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    // 已经开着同一个面板：再点一次 = 收起（等于取消）
                    if this
                        .color_picker
                        .as_ref()
                        .map(|picker| picker.dark == dark && picker.field == field)
                        .unwrap_or(false)
                    {
                        this.close_color_picker(true, cx);
                        return;
                    }
                    let colors = settings::read(|settings| {
                        if dark {
                            settings.dark.clone()
                        } else {
                            settings.light.clone()
                        }
                    });
                    let text = if colors.slot(field).trim().is_empty() {
                        colors.effective(field)
                    } else {
                        colors.slot(field).to_string()
                    };
                    this.open_color_picker(dark, field, text, event.position(), window, cx);
                }))
                .child("+"),
        );

        // ↺：清掉覆盖，回到自动派生
        if is_custom {
            row = row.child(
                div()
                    .id(SharedString::from(format!("reset-{label}")))
                    .size(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::text_primary()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_color(dark, field, String::new(), cx);
                        window.refresh();
                        cx.notify();
                    }))
                    .child("↺"),
            );
        }

        row.into_any_element()
    }

    // -----------------------------------------------------------------------
    // 菜单项页
    // -----------------------------------------------------------------------

    /// 开始「拖到目标窗口上」的拾取：等鼠标松开，取光标下那个窗口的 exe。
    ///
    /// 为什么不用 GPUI 的 `on_mouse_up`：拖出设置窗口之后那个事件就收不到了，
    /// 表现为「拖过去松手，什么都没发生」。这里改成每 60ms 问一次系统「左键还按着吗」，
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
                // 松手了：光标底下那个窗口属于谁
                let name = {
                    let hwnd = crate::win32::window_at_cursor();
                    // 松开在自己窗口上（只是点了一下）不算
                    if hwnd == 0 || crate::win32::window_is_self(hwnd) {
                        None
                    } else {
                        crate::win32::process_name_of_window(hwnd)
                    }
                };
                let _ = this.update(cx, |this, cx| {
                    this.picking = false;
                    if let Some(name) = name {
                        crate::settings::add_blocked_app(cx, &name);
                    }
                    cx.notify();
                });
                return;
            }
        })
        .detach();
    }

    /// 「禁用应用」区块：拖动拾取 + 手动输入 + 名单。
    fn block_section(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> AnyElement {
        let cfg = &settings.block;
        let mut rows: Vec<AnyElement> = Vec::new();

        // 拖动条
        rows.push(
            div()
                .id("block-pick")
                .h(px(30.))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_size(px(11.5))
                .cursor_pointer()
                .when(self.picking, |bar| {
                    bar.bg(theme::accent())
                        .text_color(theme::text_on_accent())
                })
                .when(!self.picking, |bar| {
                    bar.bg(theme::track())
                        .text_color(theme::ui_text_secondary())
                        .hover(|style| style.bg(theme::surface_hover()))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.begin_pick(cx);
                        window.refresh();
                        cx.notify();
                    }),
                )
                .child(crate::i18n::t(if self.picking {
                    "松开鼠标就加进名单…"
                } else {
                    "⣿  按住这里，拖到目标窗口上"
                }))
                .into_any_element(),
        );

        // 手动输入 + 添加 + 选程序
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
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
                        .child(self.block_input.clone()),
                )
                .child(ghost_button(
                    "添加",
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        let text = this.block_input.read(cx).text();
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            return;
                        }
                        // 填的是目录：把这一层的 exe 全加进去
                        let path = std::path::Path::new(&text);
                        if path.is_dir() {
                            if let Ok(entries) = std::fs::read_dir(path) {
                                for entry in entries.flatten() {
                                    let path = entry.path();
                                    if path.extension().and_then(|ext| ext.to_str()).map(|ext| {
                                        ext.eq_ignore_ascii_case("exe")
                                    }) != Some(true)
                                    {
                                        continue;
                                    }
                                    if let Some(name) =
                                        path.file_name().and_then(|name| name.to_str())
                                    {
                                        crate::settings::add_blocked_app(cx, name);
                                    }
                                }
                            }
                        } else {
                            crate::settings::add_blocked_app(cx, &text);
                        }
                        this.block_input
                            .update(cx, |input, cx| input.set_text("", cx));
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .child(ghost_button(
                    "选程序…",
                    cx.listener(|_this, _: &ClickEvent, window, cx| {
                        if let Some(path) = crate::win32::pick_program_file() {
                            crate::settings::add_blocked_app(cx, &path);
                        }
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .into_any_element(),
        );

        rows.push(
            div()
                .px(px(9.))
                .text_size(px(10.5))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t(
                    "填 exe 名或完整路径都行；填一个目录（如 D:\\Games\\DNF）会把里面这一层的 exe 全部加进去",
                ))
                .into_any_element(),
        );

        // 名单
        if cfg.apps.is_empty() {
            rows.push(
                div()
                    .px(px(9.))
                    .py(px(4.))
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("名单还是空的：拖一次上面的条，或者用「选程序…」挑一个"))
                    .into_any_element(),
            );
        } else {
            for (index, app) in cfg.apps.iter().enumerate() {
                let name = app.clone();
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(24.))
                        .px(px(9.))
                        .rounded(px(5.))
                        .hover(|style| style.bg(theme::surface_hover()))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_size(px(11.5))
                                .text_color(theme::text_primary())
                                .child(app.clone()),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("block-del-{index}")))
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .text_size(px(10.))
                                .text_color(theme::ui_text_tertiary())
                                .cursor_pointer()
                                .hover(|style| {
                                    style
                                        .bg(theme::track())
                                        .text_color(theme::text_primary())
                                })
                                .on_click(cx.listener(move |_this, _: &ClickEvent, window, cx| {
                                    settings::update(cx, |settings| {
                                        settings.block.apps.retain(|known| known != &name);
                                    });
                                    window.refresh();
                                    cx.notify();
                                }))
                                .child("✕"),
                        )
                        .into_any_element(),
                );
            }
        }

        section(
            "禁用应用",
            "这些程序的窗口在前面时，快捷键和热字符串提示全部让路；托盘图标手动点开面板照旧可用",
            {
                let mut all: Vec<AnyElement> = vec![toggle_row(
                    "启用「禁用应用」名单",
                    "典型是全屏游戏 / 剪辑软件：快捷键被抢会中断操作，提示条闪一下更烦",
                    cfg.enabled,
                    cx.listener(|_this, _: &ClickEvent, window, cx| {
                        settings::update(cx, |settings| {
                            settings.block.enabled = !settings.block.enabled;
                        });
                        window.refresh();
                        cx.notify();
                    }),
                )];
                all.extend(rows);
                all
            },
        )
    }

    /// 菜单项页顶部那一行「配置文件」：左右换一套、建副本、改名、删除。
    ///
    /// 换一套只动菜单项（`menus/*.runa`），配色 / 快捷键 / 热字符串一概不跟着变。
    fn profile_row(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> AnyElement {
        let current = settings.shown_profile();
        let names = crate::menu_files::list();
        let count = names.len();

        // 改名 / 建副本：这一行临时变成「输入框 + 确定 / 取消」
        if let Some(edit) = &self.profile_edit {
            let kind = edit.kind;
            let submit = cx.listener(move |this, _: &ClickEvent, window, cx| {
                let Some(edit) = this.profile_edit.take() else {
                    return;
                };
                let text = edit.input.read(cx).text();
                let result = match edit.kind {
                    ProfileEditKind::Rename => settings::rename_profile(cx, &text),
                    ProfileEditKind::Copy => settings::duplicate_profile(cx, &text).map(|_| ()),
                    ProfileEditKind::Create => settings::create_profile(cx, &text).map(|_| ()),
                };
                if let Err(err) = result {
                    eprintln!("[popup-menu] 配置操作失败：{err}");
                }
                // 换了配置文件，树编辑器要按新的 items 重画
                this.items_tree.update(cx, |tree, cx| tree.reset_state(cx));
                window.refresh();
                cx.notify();
            });
            let cancel = cx.listener(|this, _: &ClickEvent, window, cx| {
                this.profile_edit = None;
                window.refresh();
                cx.notify();
            });
            return div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(9.))
                .py(px(5.))
                .rounded(px(theme::RADIUS_ROW))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::ui_text_secondary())
                        .child(crate::i18n::t(match kind {
                            ProfileEditKind::Rename => "新名字",
                            ProfileEditKind::Copy => "副本名字",
                            ProfileEditKind::Create => "新菜单名字",
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(24.))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .bg(theme::surface_sunken())
                        .border_1()
                        .border_color(theme::accent())
                        .child(edit.input.clone()),
                )
                .child(primary_button("确定", submit))
                .child(ghost_button("取消", cancel))
                .into_any_element();
        }

        let open = self.profile_picker.is_some();
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(9.))
            .py(px(5.))
            .rounded(px(theme::RADIUS_ROW))
            .bg(theme::surface_sunken())
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::ui_text_secondary())
                    .child(crate::i18n::t("菜单配置")),
            )
            // 下拉框本身：点一下把 menus/ 里所有 .runa 列出来
            .child(
                div()
                    .id("profile-dropdown")
                    .h(px(26.))
                    .min_w(px(118.))
                    .max_w(px(200.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(6.))
                    .bg(theme::surface_elevated())
                    .border_1()
                    .border_color(if open {
                        theme::accent()
                    } else {
                        theme::border_strong()
                    })
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::accent()))
                    .on_click(cx.listener(|this, event: &ClickEvent, _window, cx| {
                        if this.profile_picker.is_some() {
                            this.profile_picker = None;
                        } else {
                            this.profile_picker = Some(FontPicker {
                                anchor: event.position(),
                            });
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(theme::text_primary())
                            .child(current.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(9.))
                            .text_color(theme::ui_text_tertiary())
                            .child("▾"),
                    ),
            )
            .child(
                // 新建：一份空配置（一个菜单项都没有），从头搭一套
                ghost_button(
                    crate::i18n::t("新建"),
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        let input = this.start_profile_edit(ProfileEditKind::Create, cx);
                        this.profile_edit = Some(ProfileEdit {
                            kind: ProfileEditKind::Create,
                            input,
                        });
                        window.refresh();
                        cx.notify();
                    }),
                ),
            )
            .child(
                ghost_button(
                    crate::i18n::t("建副本"),
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        let input = this.start_profile_edit(ProfileEditKind::Copy, cx);
                        this.profile_edit = Some(ProfileEdit {
                            kind: ProfileEditKind::Copy,
                            input,
                        });
                        window.refresh();
                        cx.notify();
                    }),
                ),
            )
            .child(
                ghost_button(
                    crate::i18n::t("改名"),
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        let input = this.start_profile_edit(ProfileEditKind::Rename, cx);
                        this.profile_edit = Some(ProfileEdit {
                            kind: ProfileEditKind::Rename,
                            input,
                        });
                        window.refresh();
                        cx.notify();
                    }),
                ),
            );

        // 删：只有一份时置灰（先建副本再删，不然删完没东西可切）
        let deletable = count > 1;
        let armed = self.is_armed(ResetKind::Profile);
        row = row.child(ghost_button_if(
            if armed { crate::i18n::t("再点确认") } else { crate::i18n::t("删除") },
            deletable,
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                if !this.confirmed(ResetKind::Profile, cx) {
                    window.refresh();
                    cx.notify();
                    return;
                }
                // 删的是「界面上下拉框里显示的那份」—— 它可能是刚点开、还没应用
                // 的预览。拿 `effective_profile()`（已提交的那份）来删的话，
                // 用户看到的是 B、删掉的却是 A（换配置只是预览，不点应用不落盘）。
                let name = settings::read(|s| s.shown_profile());
                if let Err(err) = settings::delete_profile(cx, &name) {
                    eprintln!("[popup-menu] 删除配置失败：{err}");
                    this.toast(err, cx);
                }
                this.items_tree.update(cx, |tree, cx| tree.reset_state(cx));
                window.refresh();
                cx.notify();
            }),
        ));
        // 从旧机器搬过来的人就站在这一页：原版 RunAny.ini 直接在这儿导，
        // 不用先跑去「配置」页找那个入口（导进来会新建一份菜单配置并切过去）
        row = row.child(ghost_button_tagged(
            "menu-import-ini",
            "导入 ini",
            cx.listener(|this, _: &ClickEvent, window, cx| {
                this.import_runany_ini(window, cx);
            }),
        ));
        // 上膛之后把「要删哪一份」写在旁边：下拉框里显示的可能只是预览，
        // 光写「再点确认」很容易删错（用户以为删的是眼前这份）
        let delete_target = settings::read(|settings| settings.shown_profile());
        row = row.child(
            div()
                .flex_1()
                .text_size(px(10.5))
                .text_color(if armed {
                    theme::danger()
                } else {
                    theme::ui_text_tertiary()
                })
                .child(if armed {
                    if crate::i18n::is_english() {
                        format!("click again to delete \"{delete_target}\"")
                    } else {
                        format!("再点一次就删掉「{delete_target}」")
                    }
                } else if crate::i18n::is_english() {
                    // 英文要单复数：`1 profile(s)` 这种写法看着像没翻完
                    if count == 1 {
                        "1 profile".to_string()
                    } else {
                        format!("{count} profiles")
                    }
                } else {
                    format!("{count} 份")
                }),
        );
        // 别人给的配置直接扔进这个目录就能切过去用
        row = row.child(ghost_button(
            crate::i18n::t("目录"),
            cx.listener(|_this, _: &ClickEvent, _window, _cx| {
                if let Some(dir) = crate::menu_files::dir() {
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer").arg(dir).spawn();
                }
            }),
        ));
        row.into_any_element()
    }

    /// 配置文件改名 / 建副本时那个输入框（种子内容：改名前是原名，副本是「原名 副本」）。
    fn start_profile_edit(&self, kind: ProfileEditKind, cx: &mut Context<Self>) -> Entity<TextInput> {
        let current = settings::read(|settings| settings.shown_profile());
        let seed = match kind {
            ProfileEditKind::Rename => current,
            ProfileEditKind::Copy => {
                if crate::i18n::is_english() {
                    format!("{current} copy")
                } else {
                    format!("{current} 副本")
                }
            }
            // 新菜单：给个不重复的种子名，直接回车也能用
            ProfileEditKind::Create => {
                let count = crate::menu_files::list().len() + 1;
                if crate::i18n::is_english() {
                    format!("Menu {count}")
                } else {
                    format!("菜单 {count}")
                }
            }
        };
        cx.new(|cx| TextInput::new(seed, crate::i18n::t("给这份配置起个名字"), 12.0, cx))
    }

    // ---------------------------------------------------------------- 动作库

    fn actions_page(&mut self, _settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let query = self.action_query.read(cx).text();
        let selected = self.action_category;
        let hits: Vec<&'static crate::actions::BuiltinDef> = crate::actions::search(&query)
            .into_iter()
            .filter(|def| selected.map_or(true, |category| def.category == category))
            .collect();

        // ---- 搜索框
        let search = div()
            .w_full()
            .min_w_0()
            .px(px(9.))
            .child(
                div()
                    .w_full()
                    .h(px(26.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(theme::surface())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(self.action_query.clone()),
            )
            .into_any_element();

        // ---- 分类胶囊
        let mut chips = div().flex().flex_wrap().items_center().gap(px(4.)).px(px(9.));
        let all_selected = selected.is_none();
        chips = chips.child(self.category_chip(
            crate::i18n::t("全部"),
            None,
            all_selected,
            crate::actions::search("").len(),
            cx,
        ));
        for category in crate::actions::Category::ALL {
            let count = crate::actions::BUILTINS
                .iter()
                .filter(|def| def.category == category)
                .count();
            chips = chips.child(self.category_chip(
                category.label(),
                Some(category),
                selected == Some(category),
                count,
                cx,
            ));
        }

        let mut head_note = div()
            .px(px(9.))
            .pb(px(2.))
            .text_size(px(10.))
            .text_color(theme::ui_text_tertiary())
            .child(
                crate::i18n::t("共 {n} 条")
                    .replace("{n}", &hits.len().to_string()),
            );
        if let Some(name) = &self.action_added {
            head_note = head_note.child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::accent())
                    .child(
                        crate::i18n::t("已把「{name}」加到当前配置末尾，去「菜单项」里改名 / 换图标 / 调参数")
                            .replace("{name}", name),
                    ),
            );
        }

        // ---- 列表
        let mut list = div().flex().flex_col().gap(px(1.)).px(px(7.));
        if hits.is_empty() {
            list = list.child(
                div()
                    .p(px(10.))
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("没找到对应的动作")),
            );
        }
        let mut last_category: Option<crate::actions::Category> = None;
        for def in hits {
            if selected.is_none() && last_category != Some(def.category) {
                last_category = Some(def.category);
                list = list.child(
                    div()
                        .pt(px(6.))
                        .px(px(4.))
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(def.category.label()),
                );
            }
            list = list.child(self.action_row(def, cx));
        }

        let browser = section(
            "动作库",
            "搜一搜、点「添加到菜单」就能用；带参数的动作（音量、亮度、剪贴板变形…）会先填好默认值，参数在 菜单项 → 动作 里改",
            vec![
                search,
                chips.into_any_element(),
                head_note.into_any_element(),
                list.into_any_element(),
            ],
        );

        // ---- 脚本解释器
        let mut interpreter_rows: Vec<AnyElement> = Vec::new();
        for lang in crate::actions::ScriptLang::ALL {
            let found = crate::actions::ahk_path(lang);
            let configured = settings::read(|settings| match lang {
                crate::actions::ScriptLang::V1 => settings.ahk_v1.clone(),
                crate::actions::ScriptLang::V2 => settings.ahk_v2.clone(),
            });
            let state = match &found {
                Some(path) => div()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_secondary())
                    .child(path.display().to_string()),
                None => div()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("没找到，装一个或手动指定")),
            };
            let mut row = div()
                .id(SharedString::from(format!("ahk-row-{}", lang.key())))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(9.))
                .py(px(3.))
                .child(
                    div()
                        .w(px(118.))
                        .flex_none()
                        .text_size(px(11.))
                        .child(lang.label()),
                )
                .child(div().flex_1().min_w_0().truncate().child(state));
            row = row.child(ghost_button_tagged(
                format!("ahk-pick-{}", lang.key()),
                crate::i18n::t("选择…"),
                cx.listener(move |_this, _: &ClickEvent, _window, cx| {
                    if let Some(path) = crate::win32::pick_program_file() {
                        settings::update(cx, |settings| match lang {
                            crate::actions::ScriptLang::V1 => settings.ahk_v1 = path,
                            crate::actions::ScriptLang::V2 => settings.ahk_v2 = path,
                        });
                    }
                }),
            ));
            if !configured.trim().is_empty() {
                row = row.child(ghost_button_tagged(
                    format!("ahk-auto-{}", lang.key()),
                    crate::i18n::t("自动"),
                    cx.listener(move |_this, _: &ClickEvent, _window, cx| {
                        settings::update(cx, |settings| match lang {
                            crate::actions::ScriptLang::V1 => settings.ahk_v1.clear(),
                            crate::actions::ScriptLang::V2 => settings.ahk_v2.clear(),
                        });
                    }),
                ));
            }
            interpreter_rows.push(row.into_any_element());

            // 程序目录下的 Plugins\ahkv1 / Plugins\ahkv2：解释器和脚本都能扔进去
            let dir = crate::actions::plugin_dir(lang);
            let scripts = crate::actions::plugin_scripts(lang).len();
            let in_plugin = crate::actions::plugin_interpreter(lang);
            let exists = dir.as_ref().map(|dir| dir.is_dir()).unwrap_or(false);
            let detail = if !exists {
                crate::i18n::t("还没这个文件夹：点右边建一个，把解释器和 .ahk 放进去").to_string()
            } else {
                match (&in_plugin, scripts) {
                    (Some(path), 0) => {
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_default();
                        format!("{} {name}", crate::i18n::t("已认出解释器"))
                    }
                    (Some(path), count) => {
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_default();
                        format!(
                            "{} {name} · {count} {}",
                            crate::i18n::t("已认出解释器"),
                            crate::i18n::t("个脚本")
                        )
                    }
                    (None, 0) => crate::i18n::t("空的：放 AutoHotkey.exe 和 .ahk 进来").to_string(),
                    (None, count) => {
                        format!("{} {count}", crate::i18n::t("只有脚本，缺个 AutoHotkey.exe："))
                    }
                }
            };
            let dir_label = format!(
                "Plugins{}{}",
                std::path::MAIN_SEPARATOR,
                lang.dir_name()
            );
            let open_dir = crate::actions::plugin_dir(lang);
            let mut plugin_row = div()
                .id(SharedString::from(format!("ahk-plugins-{}", lang.key())))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(9.))
                .py(px(2.))
                .child(
                    div()
                        .w(px(118.))
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_secondary())
                        .child(dir_label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(detail),
                );
            plugin_row = plugin_row.child(ghost_button_tagged(
                format!("plug-{}", lang.key()),
                if exists { "打开文件夹" } else { "建好并打开" },
                cx.listener(move |_this, _: &ClickEvent, _window, _cx| {
                    if let Some(dir) = open_dir.as_ref() {
                        let _ = std::fs::create_dir_all(dir);
                        // 统一入口：配了第三方文件管理器就用它
                        crate::win32::open_folder(&dir.display().to_string());
                    }
                }),
            ));
            interpreter_rows.push(plugin_row.into_any_element());
        }

        let interpreters = section(
            "脚本解释器",
            "「运行脚本」动作用的解释器。放进 Plugins\\ahkv1 / ahkv2 就自动认；.ahk 也放这儿，菜单里只写文件名就行",
            interpreter_rows,
        );

        // 解释器放在最上面：它是「先决条件」，筛完动作才发现没装解释器最烦
        vec![interpreters, browser]
    }

    fn category_chip(
        &self,
        label: &'static str,
        category: Option<crate::actions::Category>,
        selected: bool,
        count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(if let Some(category) = category {
                format!("actions-cat-{}", category.label())
            } else {
                "actions-cat-all".to_string()
            }))
            .px(px(8.))
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
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.action_category = category;
                cx.notify();
            }))
            .child(format!("{label} {count}"))
            .into_any_element()
    }

    fn action_row(
        &self,
        def: &'static crate::actions::BuiltinDef,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.action_preview.as_deref() == Some(def.id);
        let value_of = |param: &'static crate::actions::ParamDef| -> String {
            let raw = if expanded {
                self.preview_params
                    .get(param.key)
                    .map(String::as_str)
                    .unwrap_or(param.default)
            } else {
                param.default
            };
            crate::actions::display_value(param, raw)
        };

        let detail = if def.params.is_empty() {
            crate::i18n::t(def.desc).to_string()
        } else {
            let bits: Vec<String> = def
                .params
                .iter()
                .map(|param| {
                    format!("{} {}", crate::i18n::t(param.label), value_of(param))
                })
                .collect();
            crate::i18n::t("{desc} · 参数：{names}")
                .replace("{desc}", crate::i18n::t(def.desc))
                .replace("{names}", &bits.join(" · "))
        };

        let mut text_col = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t(def.name)),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(detail),
            );

        // 带参数的动作：把「哪一个参数 = 什么值」摊成一排小胶囊，
        // 不用点进去就知道它能调什么（以前只有「参数：按几下 / 方向」这种名字）。
        if !def.params.is_empty() {
            let mut chips = div().flex().flex_wrap().items_center().gap(px(3.)).pt(px(1.));
            for param in def.params {
                chips = chips.child(
                    div()
                        .px(px(5.))
                        .rounded(px(3.))
                        .bg(theme::track())
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(format!(
                            "{} {}",
                            crate::i18n::t(param.label),
                            value_of(param)
                        )),
                );
            }
            text_col = text_col.child(chips);
        }

        let mut buttons = div().flex_none().flex().items_center().gap(px(4.));
        if !def.params.is_empty() {
            buttons = buttons.child(ghost_button(
                if expanded {
                    crate::i18n::t("收起预览")
                } else {
                    crate::i18n::t("预览参数")
                },
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if this.action_preview.as_deref() == Some(def.id) {
                        this.action_preview = None;
                    } else {
                        this.action_preview = Some(def.id.to_string());
                        this.preview_params = crate::actions::defaults(def);
                    }
                    cx.notify();
                }),
            ));
        }
        buttons = buttons
            .child(ghost_button(
                crate::i18n::t("复制"),
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.copy_action_item(def, cx);
                }),
            ))
            .child(ghost_button(
                crate::i18n::t("添加到菜单"),
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.add_action_to_menu(def, cx);
                    cx.notify();
                }),
            ));

        let header = div()
            .id(SharedString::from(format!("act-row-{}", def.id)))
            .w_full()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(4.))
            .py(px(4.))
            .rounded(px(4.))
            .hover(|style| style.bg(theme::surface_hover()))
            .child(text_col)
            .child(buttons);

        if !expanded {
            return header.into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .pb(px(5.))
            .child(header)
            .child(self.action_preview_block(def, cx))
            .into_any_element()
    }

    /// 参数预览面板：真的能调，但只作用在预览上（不动任何配置）。
    fn action_preview_block(
        &self,
        def: &'static crate::actions::BuiltinDef,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows = div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .ml(px(9.))
            .mr(px(9.))
            .p(px(7.))
            .rounded(px(5.))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border_strong());

        for param in def.params {
            let raw = self
                .preview_params
                .get(param.key)
                .cloned()
                .unwrap_or_else(|| param.default.to_string());
            let mut line = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(84.))
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_secondary())
                        .child(crate::i18n::t(param.label)),
                )
                .child(self.action_preview_control(def, param, &raw, cx));
            if !param.hint.is_empty() {
                line = line.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t(param.hint)),
                );
            }
            rows = rows.child(line);
        }

        rows.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .pt(px(2.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("预览只是试调，不写进配置；加到菜单后去「菜单项 → 动作」里改")),
                )
                .child(ghost_button(
                    crate::i18n::t("恢复默认"),
                    cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.preview_params = crate::actions::defaults(def);
                        cx.notify();
                    }),
                )),
        )
        .into_any_element()
    }

    fn action_preview_control(
        &self,
        def: &'static crate::actions::BuiltinDef,
        param: &'static crate::actions::ParamDef,
        raw: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = param.key;
        let set = move |this: &mut Self, value: String| {
            this.preview_params.insert(key.to_string(), value);
        };
        match &param.kind {
            crate::actions::ParamKind::Int {
                min,
                max,
                step,
                unit: _,
            } => {
                let current = raw.parse::<i32>().unwrap_or(param.default.parse().unwrap_or(0));
                let (min, max, step) = (*min, *max, *step);
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .child(ghost_button(
                        "−",
                        cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            let now = this
                                .preview_params
                                .get(param.key)
                                .and_then(|v| v.parse::<i32>().ok())
                                .unwrap_or(current);
                            set(this, (now - step).max(min).to_string());
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .min_w(px(46.))
                            .text_size(px(10.5))
                            .text_color(theme::text_primary())
                            .child(crate::actions::display_value(param, &current.to_string())),
                    )
                    .child(ghost_button(
                        "+",
                        cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            let now = this
                                .preview_params
                                .get(param.key)
                                .and_then(|v| v.parse::<i32>().ok())
                                .unwrap_or(current);
                            set(this, (now + step).min(max).to_string());
                            cx.notify();
                        }),
                    ))
                    .into_any_element()
            }
            crate::actions::ParamKind::Choice(options) => {
                let mut row = div().flex().flex_wrap().items_center().gap(px(3.));
                for (value, label) in options.iter() {
                    let value = value.to_string();
                    let selected = value == raw || (raw.is_empty() && value == param.default);
                    row = row.child(
                        div()
                            .id(SharedString::from(format!(
                                "pv-{}-{}",
                                def.id, value
                            )))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .text_size(px(10.))
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
                                set(this, value.clone());
                                cx.notify();
                            }))
                            .child(crate::i18n::t(label)),
                    );
                }
                row.into_any_element()
            }
            crate::actions::ParamKind::Bool => {
                let on = matches!(raw, "true" | "1" | "on" | "yes");
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .child(
                        div()
                            .id(SharedString::from(format!("pv-bool-{}", def.id)))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .text_size(px(10.))
                            .cursor_pointer()
                            .bg(if on { theme::accent_soft() } else { theme::track() })
                            .text_color(if on {
                                theme::accent()
                            } else {
                                theme::ui_text_secondary()
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                let on = matches!(
                                    this.preview_params
                                        .get(param.key)
                                        .map(String::as_str)
                                        .unwrap_or(param.default),
                                    "true" | "1" | "on" | "yes"
                                );
                                set(this, if on { "false" } else { "true" }.to_string());
                                cx.notify();
                            }))
                            .child(crate::actions::display_value(param, raw)),
                    )
                    .into_any_element()
            }
            crate::actions::ParamKind::Text => div()
                .flex_none()
                .max_w(px(260.))
                .truncate()
                .text_size(px(10.5))
                .text_color(theme::ui_text_tertiary())
                .child(crate::actions::display_value(param, raw))
                .into_any_element(),
        }
    }

    /// 把动作库里的一条动作加到当前配置末尾。
    fn add_action_to_menu(
        &mut self,
        def: &'static crate::actions::BuiltinDef,
        cx: &mut Context<Self>,
    ) {
        let name = crate::i18n::t(def.name).to_string();
        let id = def.id.to_string();
        let params = crate::actions::defaults(def);
        settings::update(cx, |settings| {
            let added = settings.add_item_in(&[], None);
            if let Some(path) = settings.find_path(&added) {
                if let Some(node) = settings.node_mut(&path) {
                    node.label = name.clone();
                    node.actions = vec![crate::settings::ActionStep::with_params(
                        crate::settings::ActionKind::Builtin,
                        id,
                        params,
                    )];
                }
            }
        });
        self.action_added = Some(name.clone());
        self.items_tree.update(cx, |tree, cx| tree.reset_state(cx));
        let message = crate::i18n::t("已添加「{name}」到菜单末尾 —— 去「菜单项」里就能看到它")
            .replace("{name}", &name);
        self.toast(message, cx);
        cx.notify();
    }

    /// 把一条动作复制成**菜单项 JSON**（`.runa` 里 `items` 的一个元素）。
    ///
    /// 粘进菜单项列表（Ctrl+V）也行 —— `parse_paste` 认单个对象；
    /// 粘进配置文件也行 —— 就是同一段 JSON。
    fn copy_action_item(
        &mut self,
        def: &'static crate::actions::BuiltinDef,
        cx: &mut Context<Self>,
    ) {
        let name = crate::i18n::t(def.name).to_string();
        let params = crate::actions::defaults(def);
        let node = crate::settings::MenuSettings::action_item(def.id, &name, params);
        match serde_json::to_string_pretty(&node) {
            Ok(text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                let message = crate::i18n::t("已复制「{name}」的菜单项 JSON —— 粘进 items 或菜单项列表都认")
                    .replace("{name}", &name);
                self.toast(message, cx);
            }
            Err(err) => {
                let message = format!("{}: {err}", crate::i18n::t("复制失败"));
                self.toast(message, cx);
            }
        }
    }

    fn items_page(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            self.profile_row(settings, cx),
            self.items_tree.clone().into_any_element(),
            // 「恢复默认菜单」留在这一页的底部，因为它要走两步确认
            // （树编辑器里那个工具条是三个地方共用的，不好只给根层加确认逻辑）
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(3.))
                .child(ghost_button(
                    if self.is_armed(ResetKind::Items) {
                        "再点一次确认"
                    } else {
                        "恢复默认菜单"
                    },
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        // 第一次点只是上膛：改名 + 3 秒内再点才真恢复
                        if this.confirmed(ResetKind::Items, cx) {
                            settings::update(cx, |settings| settings.reset_items());
                            this.items_tree.update(cx, |tree, cx| tree.reset_state(cx));
                        }
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("只恢复这一份菜单（托盘那一支会重新补回来）；配色和尺寸不动")),
                )
                .into_any_element(),
        ]
    }

    // -----------------------------------------------------------------------
    // 托盘页：图标本身 + 它右键弹出的菜单
    // -----------------------------------------------------------------------

    fn tray_page(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            section(
                "托盘图标",
                "常驻在系统托盘里：左键弹菜单、右键弹下面这份托盘菜单",
                vec![
                    toggle_row(
                        "显示托盘图标",
                        "关掉之后程序就只能在后台跑，靠全局快捷键唤出",
                        settings.tray_enabled,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.tray_enabled = !settings.tray_enabled;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    row_with_control(
                        "左键点图标",
                        "右键永远是这份托盘菜单",
                        segmented(
                            "tray-left",
                            &[
                                (0, TrayLeftClick::PopupMenu.label()),
                                (1, TrayLeftClick::TrayMenu.label()),
                                (2, TrayLeftClick::Nothing.label()),
                            ],
                            match settings.tray_left_click {
                                TrayLeftClick::PopupMenu => 0,
                                TrayLeftClick::TrayMenu => 1,
                                TrayLeftClick::Nothing => 2,
                            },
                            cx.listener(|_this, value: &u32, window, cx| {
                                let next = match *value {
                                    1 => TrayLeftClick::TrayMenu,
                                    2 => TrayLeftClick::Nothing,
                                    _ => TrayLeftClick::PopupMenu,
                                };
                                settings::update(cx, |settings| {
                                    settings.tray_left_click = next;
                                });
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                    row_with_control(
                        "托盘图标",
                        if settings.tray_icon.trim().is_empty() {
                            "现在用内置图标（RA 字母标）"
                        } else {
                            "用这个文件里的图标"
                        },
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .w(px(170.))
                                    .h(px(24.))
                                    .px(px(6.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(6.))
                                    .bg(theme::surface())
                                    .border_1()
                                    .border_color(theme::border_strong())
                                    .child(self.tray_icon.clone()),
                            )
                            .child(ghost_button(
                                "选文件",
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    let Some(path) = crate::win32::pick_image_file() else {
                                        return;
                                    };
                                    this.tray_icon.update(cx, |input, cx| {
                                        input.set_text(path, cx);
                                    });
                                    window.refresh();
                                    cx.notify();
                                }),
                            ))
                            .child(ghost_button(
                                "✕",
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.tray_icon.update(cx, |input, cx| {
                                        input.set_text("", cx);
                                    });
                                    window.refresh();
                                    cx.notify();
                                }),
                            ))
                            .into_any_element(),
                    ),
                ],
            ),
            section(
                "托盘右键菜单",
                "和「菜单项」页一模一样：搜索、筛选、拖动排序、增删改；改完右键托盘图标就能看到",
                vec![self.tray_tree.clone().into_any_element()],
            ),
        ]
    }


    // -----------------------------------------------------------------------
    // 行为页：选中内容抓取 + 占位符
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // 常规页：开机自启 / 界面语言 / 让路的程序 / 最近运行
    // -----------------------------------------------------------------------
    //
    // 这几块原先分散在「行为」和「菜单项」两页：它们跟「菜单里有哪些项」无关，
    // 而是「这个程序在这台机器上怎么跑」。收拢到一页之后，菜单项页只剩编辑菜单，
    // 行为页只剩「弹出前后做什么」。

    fn general_page(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let recent = &settings.recent;
        vec![
            section(
                "打开文件夹",
                "留空就用资源管理器。填了就用它打开：占位符 {path} 会换成那个文件夹的路径",
                vec![row_with_control(
                    "用指定程序打开文件夹",
                    "所有「打开文件夹 / 打开文件所在位置 / 建好并打开」都走这里。例：dopusrt.exe /acmd go \"path={path}\"",
                    div()
                        .w(px(360.))
                        .flex_none()
                        .h(px(22.))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .bg(theme::surface_sunken())
                        .border_1()
                        .border_color(theme::border_strong())
                        .child(self.folder_opener_input.clone())
                        .into_any_element(),
                )],
            ),
            section(
                "启动",
                "开机自启、启动权限、界面语言",
                vec![
                    toggle_row(
                        "开机自动启动",
                        "登录 Windows 后自动在托盘运行（关掉会把注册表和任务计划里的都清掉）",
                        settings.autostart.enabled,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.autostart.enabled = !settings.autostart.enabled;
                            });
                            apply_autostart(cx);
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    row_with_control(
                        "选择开机自启方式",
                        "任务计划在「用户登录」时拉起，比注册表方式快一两秒，还能静默提权",
                        segmented(
                            "autostart-mode",
                            &[(0, "普通开机启动"), (1, "用户登录时启动")],
                            if settings.autostart.use_task { 1 } else { 0 },
                            cx.listener(|_this, value: &u32, window, cx| {
                                let use_task = *value == 1;
                                settings::update(cx, |settings| {
                                    settings.autostart.use_task = use_task;
                                    // 注册表方式做不到静默提权，切回来时把「管理员」关掉
                                    if !use_task {
                                        settings.autostart.as_admin = false;
                                    }
                                });
                                apply_autostart(cx);
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                    toggle_row(
                        "启动时以管理员运行",
                        "以最高权限启动；任务计划方式静默提权，普通方式需要 UAC 确认",
                        settings.autostart.as_admin,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.autostart.as_admin = !settings.autostart.as_admin;
                                // 打开管理员必然要走任务计划，这里顺手切过去
                                if settings.autostart.as_admin {
                                    settings.autostart.use_task = true;
                                }
                            });
                            apply_autostart(cx);
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    row_with_control(
                        "语言",
                        "界面语言：切换后设置窗口和菜单里的文字马上跟着变",
                        segmented(
                            "language",
                            &[
                                (0, settings::Language::ALL[0].label()),
                                (1, settings::Language::ALL[1].label()),
                            ],
                            if settings.language == settings::Language::English {
                                1
                            } else {
                                0
                            },
                            cx.listener(|_this, value: &u32, window, cx| {
                                let language = if *value == 1 {
                                    settings::Language::English
                                } else {
                                    settings::Language::Chinese
                                };
                                settings::update(cx, |settings| settings.language = language);
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                    row_with_control(
                        "系统里的状态",
                        "真实查出来的；和上面的开关对不上，说明被别的程序改过",
                        {
                            let state = crate::autostart::cached_status();
                            let run_on = state.run.is_some();
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .child(status_chip(
                                    if run_on {
                                        "注册表 Run 已写"
                                    } else {
                                        "注册表 Run 未写"
                                    },
                                    run_on,
                                ))
                                .child(status_chip(
                                    if state.task {
                                        "任务计划已建"
                                    } else {
                                        "任务计划未建"
                                    },
                                    state.task,
                                ))
                                .into_any_element()
                        },
                    ),
                ],
            ),
            section(
                "最近运行",
                "把刚用过的几项提到菜单顶部 / 底部；完整菜单和筛过的菜单里都生效",
                vec![
                    toggle_row(
                        "启用最近运行",
                        "执行过的菜单项会记一笔；关掉就不记录、也不在菜单里显示",
                        recent.enabled,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.recent.enabled = !settings.recent.enabled;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    row_with_control(
                        "排在哪一头",
                        "顶部：一开菜单就是它，回车最快；底部：不打断原来的第一项",
                        segmented(
                            "recent-placement",
                            &[
                                (0, settings::EdgePlacement::Top.label()),
                                (1, settings::EdgePlacement::Bottom.label()),
                            ],
                            if recent.placement == settings::EdgePlacement::Top {
                                0
                            } else {
                                1
                            },
                            cx.listener(|_this, value: &u32, window, cx| {
                                let placement = if *value == 0 {
                                    settings::EdgePlacement::Top
                                } else {
                                    settings::EdgePlacement::Bottom
                                };
                                settings::update(cx, |settings| {
                                    settings.recent.placement = placement;
                                });
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                    row_with_control(
                        "最多显示几条",
                        "只显示这么多个最近项；记录本身留 40 条，改小改大都还在",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(number_box(&self.recent_limit, 58.))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(theme::ui_text_tertiary())
                                    .child(crate::i18n::t("条")),
                            )
                            .into_any_element(),
                    ),
                    toggle_row(
                        "按选中内容筛过的菜单里也显示",
                        "抓到文字 / 文件后菜单只剩适用的几项，最近运行照样排在前面",
                        recent.in_filtered,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.recent.in_filtered = !settings.recent.in_filtered;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    toggle_row(
                        "记录托盘菜单里点的项",
                        "托盘图标右键菜单里点的那几项也记进最近列表；关掉就只记正常弹出的菜单",
                        recent.record_tray,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.recent.record_tray = !settings.recent.record_tray;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    toggle_row(
                        "快捷键直接呼出的子菜单里也显示",
                        "某一项配了「只弹这一支」的快捷键时，那一支的菜单里也带最近运行",
                        recent.in_submenu,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.recent.in_submenu = !settings.recent.in_submenu;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                ],
            ),
            self.block_section(settings, cx),
        ]
    }

    fn behavior_page(&mut self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut vars: Vec<AnyElement> = crate::placeholders::VARS
            .iter()
            .map(|(name, hint)| var_row(name, hint, cx))
            .collect();
        vars.push(
            div()
                .px(px(9.))
                .py(px(5.))
                .text_size(px(10.5))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t("在「菜单项 → 编辑 → 动作」里还有一排胶囊，点一下就把变量插进参数"))
                .into_any_element(),
        );

        let page = vec![
            section(
                "选中内容",
                "选中文字后按热键：先向原窗口发一次复制键把选中内容抓过来，抓完立刻把剪贴板原样还原",
                vec![
                    toggle_row(
                        "弹出前抓取选中内容",
                        "抓到的内容成为菜单第一项（点它就是复制），动作参数里也能用 {selection}",
                        settings.selection_enabled,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.selection_enabled = !settings.selection_enabled;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    row_with_control(
                        "抓取按键",
                        "终端里 Ctrl+C 是「中断」，这种情况改成 Ctrl+Insert",
                        div()
                            .w(px(196.))
                            .h(px(24.))
                            .px(px(6.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .bg(theme::surface_sunken())
                            .border_1()
                            .border_color(theme::border_strong())
                            .child(self.selection_keys.clone())
                            .into_any_element(),
                    ),
                    row_with_control(
                        "复制后最多等待",
                        "发完复制键后，等剪贴板把内容写全的最长时间；经常抓不到 / 文件被认成文字就调大",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .w(px(64.))
                                    .h(px(24.))
                                    .px(px(6.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(6.))
                                    .bg(theme::surface_sunken())
                                    .border_1()
                                    .border_color(theme::border_strong())
                                    .child(self.selection_wait.clone())
                                    .into_any_element(),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(theme::ui_text_tertiary())
                                    .child(crate::i18n::t("毫秒")),
                            )
                            .into_any_element(),
                    ),
                    {
                        // 按程序覆盖等待时长：慢程序单独放宽，别的还是秒弹
                        let rules = settings.selection_wait_rules.clone();
                        let mut rows: Vec<AnyElement> = Vec::new();
                        rows.push(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::ui_text_tertiary())
                                .child(crate::i18n::t(
                                    "按程序覆盖：慢程序（Office、QQ 之类）单独放宽，没列出的走上面的全局值",
                                ))
                                .into_any_element(),
                        );
                        for (index, rule) in rules.iter().enumerate() {
                            let proc_input = self.wait_field(
                                format!("proc|{index}"),
                                rule.process.clone(),
                                "qq.exe",
                                cx,
                            );
                            let ms_input = self.wait_field(
                                format!("ms|{index}"),
                                if rule.wait_ms == 0 {
                                    String::new()
                                } else {
                                    rule.wait_ms.to_string()
                                },
                                "500",
                                cx,
                            );
                            let mut row = div()
                                .id(SharedString::from(format!("wait-rule-{index}")))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .w(px(160.))
                                        .h(px(24.))
                                        .px(px(6.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.))
                                        .bg(theme::surface_sunken())
                                        .border_1()
                                        .border_color(theme::border_strong())
                                        .child(proc_input),
                                )
                                .child(
                                    div()
                                        .w(px(64.))
                                        .h(px(24.))
                                        .px(px(6.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.))
                                        .bg(theme::surface_sunken())
                                        .border_1()
                                        .border_color(theme::border_strong())
                                        .child(ms_input),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(theme::ui_text_tertiary())
                                        .child(crate::i18n::t("毫秒")),
                                );
                            row = row.child(ghost_button_tagged(
                                format!("wait-del-{index}"),
                                crate::i18n::t("删除"),
                                cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    settings::update(cx, |settings| {
                                        if index < settings.selection_wait_rules.len() {
                                            settings.selection_wait_rules.remove(index);
                                        }
                                    });
                                    this.wait_inputs.clear();
                                    window.refresh();
                                    cx.notify();
                                }),
                            ));
                            rows.push(row.into_any_element());
                        }
                        rows.push(
                            div()
                                .child(ghost_button_tagged(
                                    "wait-add",
                                    crate::i18n::t("添加程序"),
                                    cx.listener(|this, _: &ClickEvent, window, cx| {
                                        settings::update(cx, |settings| {
                                            settings.selection_wait_rules.push(
                                                settings::SelectionWaitRule {
                                                    process: String::new(),
                                                    wait_ms: 500,
                                                },
                                            );
                                        });
                                        this.wait_inputs.clear();
                                        window.refresh();
                                        cx.notify();
                                    }),
                                ))
                                .into_any_element(),
                        );
                        row_with_control(
                            "按程序设置等待时长",
                            "填进程名（qq.exe / qq 都认）+ 毫秒；留空的毫秒按全局值算",
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .child(div().flex().flex_col().gap(px(4.)).children(rows))
                                .into_any_element(),
                        )
                    },
                    toggle_row(
                        "运行程序时接上选中内容（默认）",
                        "所有「运行程序」动作的默认值：命令末尾补一个空格再拼上选中内容；菜单项里可以单独设成开启 / 关闭，优先级高于这里",
                        settings.append_selection,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.append_selection = !settings.append_selection;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    div()
                        .px(px(9.))
                        .pb(px(5.))
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("抓不到（原窗口没有选中内容）就什么都不加，菜单和平时一样"))
                        .into_any_element(),
                ],
            ),
            section(
                "按选中内容切换菜单",
                "给菜单项标上「适用选中内容」，弹出时按抓到的类型只显示用得上的那些",
                vec![
                    toggle_row(
                        "按选中内容筛选菜单",
                        "关掉之后无论选中什么，都显示完整菜单",
                        settings.context_filter,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.context_filter = !settings.context_filter;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    info_row("文字", crate::i18n::t("复制了文字、代码、一句话")),
                    info_row("文件", crate::i18n::t("资源管理器里复制的任何文件")),
                    info_row("文件夹", crate::i18n::t("选中了一个 / 多个文件夹（文件夹不算文件）")),
                    info_row(
                        "图片",
                        crate::i18n::t(
                            "剪贴板里直接是位图（截图、从网页复制一张图），不是图片文件 —— 图片文件按「文件」算",
                        ),
                    ),
                    row_with_control(
                        "多个文件的后缀命中",
                        if settings.multi_file_rule == MultiFileRule::AllSame {
                            "每个文件的后缀都得在菜单项要求的那串里（一 mp4 + 一 png 就不算 mp4 菜单）"
                        } else {
                            "只看第一个文件的后缀（顺序会影响结果）"
                        },
                        segmented(
                            "multi-file-rule",
                            &[
                                (0, MultiFileRule::AllSame.label()),
                                (1, MultiFileRule::First.label()),
                            ],
                            if settings.multi_file_rule == MultiFileRule::AllSame {
                                0
                            } else {
                                1
                            },
                            cx.listener(|_this, value: &u32, window, cx| {
                                let rule = if *value == 0 {
                                    MultiFileRule::AllSame
                                } else {
                                    MultiFileRule::First
                                };
                                settings::update(cx, |settings| settings.multi_file_rule = rule);
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                    row_with_control(
                        "把选中文件加到菜单里时",
                        match settings.add_selection_path {
                            crate::selection::AddSelectionPath::Ask => {
                                "每次弹一句问「完整路径还是裸文件名」"
                            }
                            crate::selection::AddSelectionPath::Full => {
                                "直接写完整路径（换机器 / 换安装目录就失效）"
                            }
                            crate::selection::AddSelectionPath::Bare => {
                                "只写文件名，运行时交给 PATH 去找（notepad.exe 这种）"
                            }
                        },
                        segmented(
                            "add-selection-path",
                            &[
                                (0, crate::selection::AddSelectionPath::Ask.label()),
                                (1, crate::selection::AddSelectionPath::Full.label()),
                                (2, crate::selection::AddSelectionPath::Bare.label()),
                            ],
                            settings.add_selection_path.index(),
                            cx.listener(|_this, value: &u32, window, cx| {
                                let mode = crate::selection::AddSelectionPath::from_index(*value);
                                settings::update(cx, |settings| {
                                    settings.add_selection_path = mode
                                });
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                    note_block(&[
                        "菜单项的「适用选中内容」有三种：跟随上级（默认）、单独指定、不参与",
                        "单独指定的普通项会提到根菜单（放最前面 / 按原顺序 / 子菜单前后，每项自己定；在编辑窗口里选）；只有一个子菜单配了筛选时它会被摊平，配了两个以上就各自保留；子菜单只剩一个子菜单时外壳会被塌掉",
                        "「只对哪些后缀出现」写在编辑窗口里：勾了「文件」之后才会出现那个输入框，多个后缀用 | 隔开（mp4|mkv），留空 = 任何文件都出现",
                        "没标类型的根层项是「通用项」—— 选中内容时会一起隐藏；没选中任何东西时永远显示完整菜单",
                    ]),
                ],
            ),
            section(
                "「显示全部菜单」入口",
                "只要这次弹的不是完整菜单，就补一条入口，点它原地换成完整菜单",
                vec![
                    toggle_row(
                        "不是完整菜单就补一条",
                        "筛过 / 托盘菜单 / 快捷键只弹一支 —— 这些都算「不完整」，都会出现这条入口",
                        settings.show_all_entry,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.show_all_entry = !settings.show_all_entry;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    row_with_control(
                        "放在哪儿",
                        "顶部（第一项）或者底部（最后一项）",
                        segmented(
                            "show-all-placement",
                            &[
                                (0, EdgePlacement::Top.label()),
                                (1, EdgePlacement::Bottom.label()),
                            ],
                            if settings.show_all_placement == EdgePlacement::Top {
                                0
                            } else {
                                1
                            },
                            cx.listener(|_this, value: &u32, window, cx| {
                                let next = if *value == 0 {
                                    EdgePlacement::Top
                                } else {
                                    EdgePlacement::Bottom
                                };
                                settings::update(cx, |settings| {
                                    settings.show_all_placement = next;
                                });
                                window.refresh();
                                cx.notify();
                            }),
                        ),
                    ),
                ],
            ),
            section(
                "占位符",
                "动作参数里写 {名字}，点菜单项执行时才替换成实际值",
                vars,
            ),
        ];
        page
    }

    // -----------------------------------------------------------------------
    // 热键页：弹出菜单的那几颗键 + 菜单项自己的触发键入口
    // -----------------------------------------------------------------------

    fn hotkeys_page(&self, _settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // 弹出菜单（抓选中内容）
        let hotkey_text = self.popup_hotkey.read(cx).text();
        let hotkey_problem = crate::hotkey::reason(&hotkey_text);
        let hotkey_ok = hotkey_problem.is_none();
        // 这个写法是不是已经被别处占了（自己不算）
        let popup_taken = if hotkey_text.trim().is_empty() {
            None
        } else {
            let sided = settings::read(|settings| settings.popup_hotkey_sided);
            crate::hotkey::taken_by(&hotkey_text, Some(crate::hotkey::POPUP_ID), sided, "")
        };
        let hotkey_note = if let Some(who) = popup_taken.clone() {
            if crate::i18n::is_english() {
                format!("Already used by \"{who}\" - the same combo only fires once")
            } else {
                format!("这个键已经被「{who}」占了：同一条只有先设置的那个生效")
            }
        } else if let Some(problem) = hotkey_problem {
            problem.to_string()
        } else {
            crate::i18n::t("改完立刻生效，重启程序也记得住").to_string()
        };

        // 不抓选中内容的那一颗：留空 = 不注册，所以空串不算错
        let noselect_text = self.popup_noselect.read(cx).text();
        let noselect_problem = crate::hotkey::reason(&noselect_text);
        let noselect_ok = noselect_text.trim().is_empty() || noselect_problem.is_none();
        let noselect_taken = if noselect_text.trim().is_empty() {
            None
        } else {
            let sided = settings::read(|settings| settings.popup_hotkey_noselect_sided);
            crate::hotkey::taken_by(
                &noselect_text,
                Some(crate::hotkey::POPUP_NOSELECT_ID),
                sided,
                "",
            )
        };
        let noselect_note = if noselect_text.trim().is_empty() {
            crate::i18n::t("留空 = 不注册。配上之后，这一键弹出来的菜单不会去复制你正在选的东西").to_string()
        } else if let Some(who) = noselect_taken.clone() {
            if crate::i18n::is_english() {
                format!("Already used by \"{who}\" - the same combo only fires once")
            } else {
                format!("这个键已经被「{who}」占了：同一条只有先设置的那个生效")
            }
        } else if noselect_ok {
            crate::i18n::t("这些场景用它更合适：终端（Ctrl+C 是中断）、全屏游戏、剪贴板里正放着要用的东西").to_string()
        } else if let Some(problem) = noselect_problem {
            problem.to_string()
        } else {
            crate::i18n::t("写法：修饰键 + 一个键，例如 Ctrl+Alt+N").to_string()
        };

        let hotkey_box = |input: &Entity<TextInput>, ok: bool| -> AnyElement {
            div()
                .w(px(150.))
                .h(px(24.))
                .px(px(6.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .bg(theme::surface_sunken())
                .border_1()
                .border_color(if ok {
                    theme::border_strong()
                } else {
                    theme::danger()
                })
                .child(input.clone())
                .into_any_element()
        };

        let mut popup_rows: Vec<AnyElement> = vec![
            row_with_control(
                "弹出菜单",
                "选中内容会被先复制过来，成为菜单第一项；点「键盘」可以直接点键拼组合",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(hotkey_box(&self.popup_hotkey, hotkey_ok))
                    .child(crate::keypad::open_button(
                        "pad-popup-hotkey",
                        &self.popup_hotkey,
                        false,
                        Some(crate::keypad::SidedTarget::Popup),
                    ))
                    .into_any_element(),
            ),
            div()
                .px(px(9.))
                .pb(px(6.))
                .text_size(px(10.5))
                .text_color(if hotkey_ok && popup_taken.is_none() {
                    theme::ui_text_tertiary()
                } else {
                    theme::danger()
                })
                .child(hotkey_note)
                .into_any_element(),
            row_with_control(
                "弹出菜单（不抓选中内容）",
                "同一份菜单，省掉那一次复制：终端里不会中断、全屏游戏里不卡一下；{selection} 是空的",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(hotkey_box(&self.popup_noselect, noselect_ok))
                    .child(crate::keypad::open_button(
                        "pad-popup-noselect",
                        &self.popup_noselect,
                        false,
                        Some(crate::keypad::SidedTarget::Noselect),
                    ))
                    .into_any_element(),
            ),
            div()
                .px(px(9.))
                .pb(px(6.))
                .text_size(px(10.5))
                .text_color(if noselect_ok && noselect_taken.is_none() {
                    theme::ui_text_tertiary()
                } else {
                    theme::danger()
                })
                .child(noselect_note)
                .into_any_element(),
        ];
        for problem in crate::hotkey::problems() {
            popup_rows.push(
                div()
                    .px(px(9.))
                    .pb(px(3.))
                    .text_size(px(10.5))
                    .text_color(theme::danger())
                    .child(format!("· {problem}"))
                    .into_any_element(),
            );
        }

        // 菜单项自己的触发键：只在这儿给个入口 + 计数，配的时候去「菜单项」页
        let (with_hotkey, with_shortcut) = settings::read(|settings| {
            let mut real = 0;
            let mut hint = 0;
            fn walk(items: &[ItemSetting], real: &mut usize, hint: &mut usize) {
                for item in items {
                    if !item.hotkey.trim().is_empty() {
                        *real += 1;
                    }
                    if !item.shortcut.trim().is_empty() {
                        *hint += 1;
                    }
                    walk(&item.children, real, hint);
                }
            }
            walk(&settings.items, &mut real, &mut hint);
            (real, hint)
        });

        let item_rows: Vec<AnyElement> = vec![
            row_with_control(
                "有触发快捷键的菜单项",
                "普通项按下就直接执行；子菜单按下只弹这一支。配置入口在「菜单项」页的 ✎ 里",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(if crate::i18n::is_english() {
                                format!("{with_hotkey} items")
                            } else {
                                format!("现在 {with_hotkey} 条")
                            }),
                    )
                    .child(ghost_button_if(
                        "筛选出有快捷键的项",
                        true,
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.page = Page::Items;
                            this.items_tree.update(cx, |tree, cx| {
                                tree.filter_hotkeys(cx);
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ))
                    .into_any_element(),
            ),
            info_row(
                "快捷键提示",
                if crate::i18n::is_english() {
                    format!("{with_shortcut} items also show a hint like \"Ctrl+Shift+1\" - that one is display-only")
                } else {
                    format!("另有 {with_shortcut} 条只配了「快捷键提示」（只在菜单里显示，不注册到系统）")
                },
            ),
        ];

        // ---- 局部快捷键 ----
        // 原来这里有一块「匹配规则 / 区分左右 / 鼠标键」的说明，已经删掉：
        // 「区分左右修饰键」是每条快捷键自己的开关（在键盘面板里拨），写法说明
        // 也在面板底下，这一页再列一遍只是噪音。
        //
        // 换成汇总：有几条局部快捷键（带生效条件的），点一下跳去「菜单项」页
        // 并按「局部快捷键」筛出来 —— 以前这一页压根找不到这些项。
        let local = settings::read(|settings| {
            let mut count = 0usize;
            fn walk(items: &[ItemSetting], count: &mut usize) {
                for item in items {
                    if !item.hotkey_local.trim().is_empty()
                        || !item.hotkey_local_when.trim().is_empty()
                    {
                        *count += 1;
                    }
                    walk(&item.children, count);
                }
            }
            walk(&settings.items, &mut count);
            count
        });
        let local_rows: Vec<AnyElement> = vec![row_with_control(
            "局部快捷键",
            "只在特定窗口里才生效的触发键：每一条都写着生效条件（exe: / title: / mouse.exe: …），条件不满足时不吞键，那个组合在别的程序里还是它本来的意思",
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(if crate::i18n::is_english() {
                            format!("{local} items")
                        } else {
                            format!("现在 {local} 条")
                        }),
                )
                .child(ghost_button_if(
                    "筛选出局部快捷键的项",
                    true,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.page = Page::Items;
                        this.items_tree.update(cx, |tree, cx| {
                            tree.filter_local_hotkeys(cx);
                        });
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .into_any_element(),
        )];

        // ---- 冲突：同一个键被两处用 ----
        let clashes = crate::hotkey::conflicts();
        let mut clash_rows: Vec<AnyElement> = Vec::new();
        if clashes.is_empty() {
            clash_rows.push(
                div()
                    .px(px(9.))
                    .py(px(5.))
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("现在没有重复的快捷键"))
                    .into_any_element(),
            );
        }
        for (text, names) in clashes {
            clash_rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(9.))
                    .py(px(4.))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme::danger())
                            .child(text),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_secondary())
                            .child(names.join(" · ")),
                    )
                    .into_any_element(),
            );
        }

        vec![
            section(
                "弹出菜单",
                "在任何程序里按下都生效，菜单出现在鼠标处",
                popup_rows,
            ),
            section("菜单项自己的键", "点进去就能改，改完立刻生效", item_rows),
            section(
                "局部快捷键",
                "带生效条件的触发键，这一页看汇总",
                local_rows,
            ),
            section(
                "重复的快捷键",
                "同一个组合键只能有一处生效，先设置的赢",
                clash_rows,
            ),
            section(
                "关于写法",
                "「键盘」面板点出来的组合就按这个口径存",
                vec![
                    info_row(
                        "修饰键",
                        crate::i18n::t(
                            "Ctrl / Alt / Shift / Win；左右分开写 LCtrl、RShift、LWin、RWin（「键盘」面板里点左边那颗就是 L-）",
                        ),
                    ),
                    info_row(
                        "写错了会怎样",
                        crate::i18n::t(
                            "填了不合法的写法不会覆盖上一颗能用的键（上面会红字提示），方便你慢慢改",
                        ),
                    ),
                ],
            ),
        ]
    }

    // -----------------------------------------------------------------------
    // 一键直达页
    // -----------------------------------------------------------------------

    /// 规则表和设置对齐：条数 / 顺序变了就重建输入框（按 id 对齐，拖排也不串行）。
    /// 新建一个直达项（`submenu` = 子菜单），建完直接开它的动作编辑器。
    /// 删掉一个直达项 —— 顺手把指着它的规则一起删掉。
    ///
    /// 留着规则只会得到一条永远「目标不见了」的红字；用户删项的本意是
    /// 「这一路玩法我不要了」，规则跟着走才符合预期。
    fn delete_magic_target(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        settings::update(cx, move |settings| {
            settings.magic.rules.retain(|rule| rule.target != id);
            if let Some(path) = settings.find_path(&id) {
                if let Some(parent) = path.split_last().map(|(_, head)| head.to_vec()) {
                    if let Some(node) = settings.node_mut(&parent) {
                        node.children.retain(|item| item.id != id);
                    }
                }
            }
        });
        cx.notify();
    }

    /// 改一条规则 = 打开规则窗口（类型下拉、多行表达式、选直达项都在那儿）。
    fn edit_magic_rule(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        crate::magic_ui::open(id, cx);
        cx.notify();
    }

    /// 上移 / 下移一条规则（顺序就是优先级）。
    fn move_magic_rule(&mut self, id: &str, up: bool, cx: &mut Context<Self>) {
        let id = id.to_string();
        settings::update(cx, move |settings| {
            let rules = &mut settings.magic.rules;
            let Some(index) = rules.iter().position(|rule| rule.id == id) else {
                return;
            };
            let to = if up {
                index.checked_sub(1)
            } else if index + 1 < rules.len() {
                Some(index + 1)
            } else {
                None
            };
            if let Some(to) = to {
                rules.swap(index, to);
            }
        });
        cx.notify();
    }

    fn delete_magic_rule(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        settings::update(cx, move |settings| {
            settings.magic.rules.retain(|rule| rule.id != id);
        });
        cx.notify();
    }

    fn magic_page(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let magic = &settings.magic;

        // ---- 开关 + 快捷键 ----
        let hotkey = self.magic_hotkey.read(cx).text();
        let hotkey_problem = crate::hotkey::reason(&hotkey);
        let hotkey_ok = hotkey.trim().is_empty() || hotkey_problem.is_none();
        let taken = if hotkey.trim().is_empty() {
            None
        } else {
            crate::hotkey::taken_by(&hotkey, Some(crate::hotkey::MAGIC_ID), magic.sided, "")
        };
        // 留空不是「没配」，是「蹭弹出菜单那一颗」—— 说得明白点，别让人以为还得再填一颗
        let shared = hotkey.trim().is_empty();
        let hotkey_note = if shared {
            crate::i18n::t(
                "留空 = 跟「弹出菜单」共用同一颗键：按下先逐条问规则，一条都不中再照常弹菜单。想单独配一颗再填这里",
            )
            .to_string()
        } else if let Some(who) = taken.clone() {
            if crate::i18n::is_english() {
                format!("Already used by \"{who}\" - the same combo only fires once")
            } else {
                format!("这个键已经被「{who}」占了：同一条只有先设置的那个生效")
            }
        } else if let Some(problem) = hotkey_problem {
            problem.to_string()
        } else {
            crate::i18n::t("选中内容 → 按下这一颗 → 命中就直接执行，不弹菜单").to_string()
        };

        let toggle = |id: &'static str,
                      title: &'static str,
                      hint: &'static str,
                      value: bool,
                      cx: &mut Context<Self>,
                      flip: fn(&mut MenuSettings)| {
            div()
                .id(SharedString::from(id))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(9.))
                .py(px(4.))
                .rounded(px(theme::RADIUS_ROW))
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(move |_this, _: &ClickEvent, window, cx| {
                    settings::update(cx, |settings| flip(settings));
                    window.refresh();
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(124.))
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(theme::text_primary())
                        .child(crate::i18n::t(title)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t(hint)),
                )
                .child(switch(value))
                .into_any_element()
        };

        let basic_rows: Vec<AnyElement> = vec![
            toggle(
                "magic-enabled",
                "启用一键直达",
                "关掉之后这颗快捷键不注册，选中内容也不会被动",
                magic.enabled,
                cx,
                |settings| settings.magic.enabled = !settings.magic.enabled,
            ),
            row_with_control(
                "快捷键",
                if shared {
                    "现在跟「弹出菜单」共用同一颗；单独一颗会更好记，但也更容易和别的键撞"
                } else {
                    "和弹出菜单那颗是两个键；选中内容会被先复制过来再逐条问规则"
                },
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .w(px(150.))
                            .h(px(24.))
                            .px(px(6.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .bg(theme::surface_sunken())
                            .border_1()
                            .border_color(if hotkey_ok && taken.is_none() {
                                theme::border_strong()
                            } else {
                                theme::danger()
                            })
                            .child(self.magic_hotkey.clone()),
                    )
                    .child(crate::keypad::open_button(
                        "pad-magic-hotkey",
                        &self.magic_hotkey,
                        false,
                        Some(crate::keypad::SidedTarget::Magic),
                    ))
                    .into_any_element(),
            ),
            div()
                .px(px(9.))
                .pb(px(6.))
                .text_size(px(10.5))
                .text_color(if hotkey_ok && taken.is_none() {
                    theme::ui_text_tertiary()
                } else {
                    theme::danger()
                })
                .child(hotkey_note)
                .into_any_element(),
            toggle(
                "magic-fallback",
                "没命中时照常弹菜单",
                "规则一条都不命中 → 弹普通菜单（关掉 = 什么都不做，只当直达用）",
                magic.fallback_menu,
                cx,
                |settings| settings.magic.fallback_menu = !settings.magic.fallback_menu,
            ),
        ];

        // ---- 规则表 ----
        let mut rule_rows: Vec<AnyElement> = Vec::new();
        if magic.rules.is_empty() {
            rule_rows.push(
                div()
                    .px(px(9.))
                    .py(px(6.))
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "还没有规则。点「＋ 加规则」：类型从下拉里挑一种，表达式一行一条；目标在同一个窗口里选，没有合适的就在那儿直接新建个菜单项",
                    ))
                    .into_any_element(),
            );
        }
        // 每一帧重记行位置（拖拽落点靠它）
        RULE_BOUNDS.with(|slot| slot.borrow_mut().clear());
        for (index, rule) in magic.rules.iter().enumerate() {
            rule_rows.push(self.magic_rule_row(index, rule, cx));
        }
        rule_rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(9.))
                .py(px(4.))
                .child(ghost_button_tagged(
                    "magic-add-rule",
                    "＋ 加规则",
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.edit_magic_rule(None, cx);
                        window.refresh();
                    }),
                ))
                // 复制 / 粘贴整份规则：规则和它跑的菜单项一起走，直接发给人就能用
                .child(ghost_button_tagged(
                    "magic-copy-rules",
                    "复制全部",
                    cx.listener(|_this, _: &ClickEvent, window, cx| {
                        let json = crate::magic::export_bundle();
                        crate::win32::write_clipboard_text(&json);
                        println!("[popup-menu] 已把一键直达规则复制到剪贴板（{}字节）", json.len());
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .child(ghost_button_tagged(
                    "magic-paste-rules",
                    "粘贴",
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        let Some(text) = crate::win32::read_clipboard_text() else {
                            return;
                        };
                        match crate::magic::import_bundle(cx, &text) {
                            Ok(n) => {
                                println!("[popup-menu] 粘进来 {n} 条一键直达规则");
                                this.items_tree.update(cx, |tree, cx| tree.reset_state(cx));
                            }
                            Err(err) => eprintln!("[popup-menu] 粘贴规则失败：{err}"),
                        }
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t(
                            "在一个窗口里写：类型下拉选、表达式一行一条、一次能加好几条 —— 从上往下问，第一条命中的就执行",
                        )),
                )
                .into_any_element(),
        );
        // 「会参与的 1 条 / 共 1 条」这一行删掉了：上面规则表里每条右端就带着开关和
        // 表达式状态，再加一句计数纯属重复，还容易让人以为「没参与」是没保存。
        //
        // ---- 直达项 ----
        //
        // 「＋ 新建直达项 / ＋ 新建直达子菜单」这两颗按钮删掉了：规则窗口的「目标」
        // 那一栏本来就能边写规则边新建（同一行还带「✎ 配动作」直接开动作编辑器），
        // 两条入口干的是同一件事，摆在页面底部只会让人在两处之间来回跳。
        let targets = crate::magic::existing_targets();
        // 规则删了、直达项还留着：列在最后，免得变成看不见的孤儿
        let orphans: Vec<&(String, String, bool)> = targets
            .iter()
            .filter(|(id, _, _)| !magic.rules.iter().any(|rule| &rule.target == id))
            .collect();
        if !orphans.is_empty() {
            rule_rows.push(
                div()
                    .px(px(9.))
                    .pt(px(6.))
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("没被任何规则用的直达项（删规则时留下的）："))
                    .into_any_element(),
            );
            for (id, label, submenu) in orphans {
                rule_rows.push(self.magic_target_row(id, label, *submenu, 0, cx));
            }
        }

        // ---- 试一试 ----
        let sample = self.magic_test.read(cx).text();
        let hit = crate::magic::first_hit(&magic.rules, &sample);
        let verdict = if sample.trim().is_empty() {
            crate::i18n::t("在上面粘一段文字，就能看到它会命中哪一条（不写设置）").to_string()
        } else if let Some(rule) = hit {
            let target = crate::magic::target_summary(&rule.target).0;
            let by = if rule.note.trim().is_empty() {
                rule.summary()
            } else {
                format!("{}（{}）", rule.summary(), rule.note)
            };
            if crate::i18n::is_english() {
                format!("Hit [{by}] -> {target}")
            } else {
                format!("命中「{by}」→ {target}")
            }
        } else if magic.fallback_menu {
            crate::i18n::t("一条都没命中 → 照常弹菜单").to_string()
        } else {
            crate::i18n::t("一条都没命中 → 什么都不做").to_string()
        };
        let test_rows: Vec<AnyElement> = vec![
            div()
                .w_full()
                .min_w_0()
                .px(px(9.))
                .child(
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
                        .child(self.magic_test.clone()),
                )
                .into_any_element(),
            div()
                .px(px(9.))
                .py(px(5.))
                .text_size(px(10.5))
                .text_color(if hit.is_some() {
                    theme::accent()
                } else {
                    theme::ui_text_tertiary()
                })
                .child(verdict)
                .into_any_element(),
        ];

        vec![
            section(
                "一键直达",
                "选中内容 + 一个快捷键，按规则表逐条问，命中就直接跑",
                basic_rows,
            ),
            section(
                "规则表（从上往下）",
                "顺序就是优先级；按住任意一行上下拖可以调整顺序（↑ ↓ 也行）；每行第二行就是这条规则跑的那个直达项",
                vec![div()
                    .flex()
                    .flex_col()
                    .children(rule_rows)
                    // 拖拽排序：按下时行自己记了「拖的是谁」，这里只用管移动和松手
                    .on_mouse_move(cx.listener(
                        |_this, event: &MouseMoveEvent, _window, cx| {
                            let dx_dy = RULE_DRAG.with(|slot| {
                                let mut guard = slot.borrow_mut();
                                match guard.as_mut() {
                                    Some((_, start, dragging)) => {
                                        let dx = f32::from(event.position.x)
                                            - f32::from(start.x);
                                        let dy = f32::from(event.position.y)
                                            - f32::from(start.y);
                                        if !*dragging && dx * dx + dy * dy > 64.0 {
                                            *dragging = true;
                                            return Some((true, false));
                                        }
                                        Some((false, *dragging))
                                    }
                                    None => None,
                                }
                            });
                            if let Some((started, dragging)) = dx_dy {
                                if started || dragging {
                                    cx.notify();
                                }
                            }
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                            let state = RULE_DRAG.with(|slot| slot.borrow_mut().take());
                            if let Some((id, _, true)) = state {
                                let to = this.magic_drop_index(f32::from(event.position.y));
                                crate::magic::move_rule_to(cx, &id, to);
                                cx.notify();
                            }
                        }),
                    )
                    .into_any_element()],
            ),
            section("试一试", "拿一段文字走一遍匹配，不动设置", test_rows),
            section(
                "能玩的花样",
                "不止正则：网站、路径、颜色、JSON、IP 都能认",
                vec![
                    note_row(
                        "不用写正则",
                        crate::i18n::t(
                            "类型选「是网址」「是颜色」「是 JSON」「是文件」这类，表达式都不用填 —— 命中就看内容本身长什么样",
                        )
                        .to_string(),
                    ),
                    note_row(
                        "绑到子菜单",
                        crate::i18n::t(
                            "目标选一个直达子菜单：命中时只弹那一支，不再从整个菜单里找",
                        )
                        .to_string(),
                    ),
                    note_row(
                        "多条兜底",
                        crate::i18n::t(
                            "最后放一条「非空就行」：认不出来的东西也照样一步到位",
                        )
                        .to_string(),
                    ),
                ],
            ),
        ]
    }

    /// 规则表里的一行 —— 只读摘要，改就点「改」开规则窗口。
    fn magic_rule_row(
        &self,
        index: usize,
        rule: &crate::magic::Rule,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = rule.id.clone();
        let problem = crate::magic::problem(rule);
        let (target_label, target_sub) = crate::magic::target_summary(&rule.target);
        let target_id = rule.target.clone();
        let enabled = rule.enabled;
        let summary = rule.summary();

        // 小方块按钮（↑ ↓ ✕ 这类）
        let small = |glyph: &'static str,
                     cx: &mut Context<Self>,
                     action: fn(&mut Self, &str, &mut Context<Self>)| {
            let id = id.clone();
            div()
                .id(SharedString::from(format!("magic-{glyph}-{id}")))
                .flex_none()
                .size(px(20.))
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
                .child(glyph)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    action(this, &id, cx);
                    window.refresh();
                }))
                .into_any_element()
        };

        let enabled_toggle = {
            let id = id.clone();
            div()
                .id(SharedString::from(format!("magic-on-{id}")))
                .flex_none()
                .flex()
                .items_center()
                .cursor_pointer()
                .on_click(cx.listener(move |_this, _: &ClickEvent, window, cx| {
                    let id = id.clone();
                    settings::update(cx, |settings| {
                        if let Some(rule) =
                            settings.magic.rules.iter_mut().find(|rule| rule.id == id)
                        {
                            rule.enabled = !rule.enabled;
                        }
                    });
                    window.refresh();
                    cx.notify();
                }))
                .child(switch(enabled))
        };

        let edit = {
            let id = id.clone();
            div()
                .id(SharedString::from(format!("magic-edit-{id}")))
                .flex_none()
                .h(px(20.))
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .text_color(theme::ui_text_tertiary())
                .cursor_pointer()
                .hover(|style| {
                    style
                        .bg(theme::surface_hover())
                        .text_color(theme::text_primary())
                })
                .child(
                    gpui::svg()
                        .path("icons/edit.svg")
                        .size(px(12.))
                        .flex_none()
                        .text_color(theme::ui_text_secondary()),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.edit_magic_rule(Some(&id), cx);
                    window.refresh();
                }))
        };

        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(3.))
            .p(px(6.))
            .rounded(px(theme::RADIUS_ROW))
            .bg(if enabled {
                theme::surface()
            } else {
                theme::surface_sunken()
            })
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .w(px(16.))
                            .flex_none()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(format!("{}", index + 1)),
                    )
                    // 类型：这里只是个「是什么比法」的标签，改在窗口里用下拉改
                    .child(
                        div()
                            .flex_none()
                            .h(px(22.))
                            .px(px(7.))
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .bg(theme::accent_soft())
                            .text_size(px(10.))
                            .text_color(theme::accent())
                            .child(crate::i18n::t(rule.kind.label())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(if enabled {
                                theme::text_primary()
                            } else {
                                theme::ui_text_tertiary()
                            })
                            .child(if rule.note.trim().is_empty() {
                                summary
                            } else {
                                format!("{summary}　·　{}", rule.note.trim())
                            }),
                    )
                    .child(edit)
                    .child(enabled_toggle)
                    .child(small("↑", cx, |this, id, cx| {
                        this.move_magic_rule(id, true, cx)
                    }))
                    .child(small("↓", cx, |this, id, cx| {
                        this.move_magic_rule(id, false, cx)
                    }))
                    .child(
                        div()
                            .id(SharedString::from(format!("magic-copy-{id}")))
                            .flex_none()
                            .size(px(20.))
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
                            .child("⧉")
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |_this, _: &ClickEvent, window, cx| {
                                    // 只复制这一条（连它跑的菜单项一起），粘到别处就是新的一套
                                    let json = crate::magic::export_rule(&id);
                                    crate::win32::write_clipboard_text(&json);
                                    window.refresh();
                                    cx.notify();
                                }
                            })),
                    )
                    .child(small("✕", cx, |this, id, cx| {
                        this.delete_magic_rule(id, cx)
                    })),
            )
            .child(
                // 第二行就是「这条规则绑的那个直达项」—— 以前它单开一块，得来回对着看；
                // 现在紧贴在规则下面，一眼就知道哪条规则跑哪个动作
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .w(px(16.))
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child("→"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(280.))
                                    .truncate()
                                    .text_size(px(10.5))
                                    .text_color(if problem.is_some() {
                                        theme::danger()
                                    } else {
                                        theme::ui_text_secondary()
                                    })
                                    .child(target_label),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(9.))
                                    .text_color(theme::ui_text_tertiary())
                                    .child(crate::i18n::t(if target_sub {
                                        "子菜单（摊开）"
                                    } else {
                                        "菜单项（直接执行）"
                                    })),
                            ),
                    )
                    .child(if target_id.trim().is_empty() {
                        div().flex_none().into_any_element()
                    } else {
                        let open_id = target_id.clone();
                        ghost_button_tagged(
                            format!("rule-target-edit-{id}"),
                            "✎ 配动作",
                            cx.listener(move |_this, _: &ClickEvent, _window, cx| {
                                crate::item_editor::open(&open_id, cx);
                            }),
                        )
                    }),
            )
            .children(problem.map(|problem| {
                div()
                    .pl(px(22.))
                    .text_size(px(9.5))
                    .text_color(theme::danger())
                    .child(problem)
            }))
            // 拖拽排序：记下这一行的位置，按下时记住拖的是谁
            .child(
                canvas(
                    |_, _, _| {},
                    {
                        let id = id.clone();
                        move |rect, _, _, _| {
                            RULE_BOUNDS.with(|slot| slot.borrow_mut().push((id.clone(), rect)));
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = id.clone();
                    move |_this, event: &MouseDownEvent, _window, cx| {
                        RULE_DRAG.with(|slot| {
                            *slot.borrow_mut() = Some((id.clone(), event.position, false));
                        });
                        // 别让下面的行内按钮和「点空白清选中」抢这一下
                        cx.stop_propagation();
                    }
                }),
            )
            .into_any_element()
    }

    /// 拖拽过程中的落点：鼠标在哪一行上就插到哪一行前面。
    fn magic_drop_index(&self, y: f32) -> usize {
        RULE_BOUNDS.with(|slot| {
            let bounds = slot.borrow();
            for (index, (_, rect)) in bounds.iter().enumerate() {
                if y < f32::from(rect.origin.y) + f32::from(rect.size.height) / 2.0 {
                    return index;
                }
            }
            bounds.len()
        })
    }

    /// 直达项那一行：名字 + 类型 + 被几条规则用着 + 配动作 / 删除。
    fn magic_target_row(
        &self,
        id: &str,
        label: &str,
        submenu: bool,
        used: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if label.trim().is_empty() {
            crate::i18n::t("未命名的直达项").to_string()
        } else {
            label.to_string()
        };
        let edit_id = id.to_string();
        let delete_id = id.to_string();
        div()
            .w_full()
            .min_w_0()
            .h(px(30.))
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(theme::RADIUS_ROW))
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(theme::text_primary())
                    .child(label),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(18.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(theme::accent_soft())
                    .text_size(px(9.))
                    .text_color(theme::accent())
                    .child(crate::i18n::t(if submenu { "子菜单" } else { "菜单项" })),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(9.5))
                    .text_color(if used == 0 {
                        theme::ui_text_tertiary()
                    } else {
                        theme::ui_text_secondary()
                    })
                    .child(if used == 0 {
                        crate::i18n::t("还没有规则用它").to_string()
                    } else if crate::i18n::is_english() {
                        format!("{used} rule(s)")
                    } else {
                        format!("{used} 条规则用它")
                    }),
            )
            .child(ghost_button_tagged(
                format!("target-edit-{edit_id}"),
                "✎ 配动作",
                cx.listener(move |_this, _: &ClickEvent, _window, cx| {
                    crate::item_editor::open(&edit_id, cx);
                }),
            ))
            .child(
                div()
                    .id(SharedString::from(format!("magic-target-del-{delete_id}")))
                    .flex_none()
                    .size(px(22.))
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
                            .text_color(theme::danger())
                    })
                    .child("✕")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.delete_magic_target(&delete_id, cx);
                        window.refresh();
                    })),
            )
            .into_any_element()
    }

    // -----------------------------------------------------------------------
    // 程序搜索页    // -----------------------------------------------------------------------
    // 程序搜索页
    // -----------------------------------------------------------------------

    /// 排除规则编辑框里的文本。
    fn prog_exclude_text(&self, cx: &Context<Self>) -> String {
        self.prog_exclude.read(cx).text()
    }

    fn prog_extensions_text(&self, cx: &Context<Self>) -> String {
        self.prog_extensions.read(cx).text()
    }

    /// 后台把没算过的裸文件名一个个解析掉（每算完一个刷一次界面）。
    ///
    /// Everything 的查询是 IPC，一次几十毫秒；以前在渲染里每帧每行算两遍，
    /// 裸文件名一多，这一页就整个卡住。现在页面上只读缓存。
    fn ensure_probes(&mut self, names: &[String], cx: &mut Context<Self>) {
        let missing: Vec<String> = names
            .iter()
            .filter(|name| crate::progindex::peek(name).is_none())
            .cloned()
            .collect();
        if missing.is_empty() || self.probe_task {
            return;
        }
        self.probe_task = true;
        cx.spawn(async move |this, cx| {
            for name in missing {
                cx.background_executor()
                    .spawn(async move {
                        crate::progindex::probe(&name);
                    })
                    .await;
                let _ = this.update(cx, |_this, cx| cx.notify());
            }
            let _ = this.update(cx, |this, cx| {
                this.probe_task = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// 「运行程序」动作里所有「只写了文件名」的项（去重、保序）。
    fn bare_program_names(settings: &MenuSettings) -> Vec<String> {
        fn walk(items: &[ItemSetting], out: &mut Vec<String>) {
            for item in items {
                for step in &item.actions {
                    if step.kind != crate::settings::ActionKind::Run {
                        continue;
                    }
                    if let Some(name) = crate::progindex::bare_name(&step.value) {
                        if !out.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
                            out.push(name);
                        }
                    }
                }
                walk(&item.children, out);
            }
        }
        let mut names = Vec::new();
        walk(&settings.items, &mut names);
        // 手动钉死过、但菜单里已经不用的名字也列出来（不然用户没法清掉它）
        for name in settings.programs.fixes.keys() {
            if !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
                names.push(name.clone());
            }
        }
        names
    }

    /// 程序搜索页。
    ///
    /// 三块：索引状态（+ 重扫）、挑哪一份的规则（+ 排除规则）、
    /// 「菜单里现在用到哪些裸文件名 / 各自解析到哪」——最后那块才是用户真要看的。
    fn programs_page(&mut self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cfg = &settings.programs;
        // 我们自己不建索引了：裸文件名全交给 Everything（它本来就在给整台机器建）。
        // 这一块是「那个联动现在通不通」的体检报告。
        let ev = crate::everything::status();
        let ev_note = if ev.backend == crate::everything::Backend::None {
            crate::i18n::t("没连上 Everything：只有 PATH 里的程序和写了完整路径的能跑").to_string()
        } else if ev.cooling {
            crate::i18n::t("刚刚连续失败，先冷却一会儿再问").to_string()
        } else if let Some(error) = &ev.last_error {
            error.clone()
        } else if ev.last_ms == 0 {
            crate::i18n::t("已连接：还没查过东西，第一次用的时候会慢一下").to_string()
        } else if crate::i18n::is_english() {
            format!("Connected · last query {} ms", ev.last_ms)
        } else {
            format!("已连接 · 上次查了 {} 毫秒", ev.last_ms)
        };
        let status_row = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(9.))
            .py(px(6.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme::text_primary())
                            .child(crate::i18n::t("裸文件名交给 Everything 查")),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(if ev.backend == crate::everything::Backend::None {
                                theme::danger()
                            } else {
                                theme::ui_text_tertiary()
                            })
                            .child(ev_note),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(if ev.source.is_empty() {
                                crate::i18n::t("不再自己扫盘：装了 Everything（或把 es.exe 放进程序目录）就能全盘解析；挪过位置的文件会当场作废并重新索引").to_string()
                            } else {
                                format!("{} · {}", ev.backend.label(), ev.source)
                            }),
                    ),
            )
            .child(ghost_button_if(
                crate::i18n::t("重新检测"),
                true,
                cx.listener(|_this, _: &ClickEvent, window, cx| {
                    crate::everything::reload();
                    crate::progindex::clear_probes();
                    window.refresh();
                    cx.notify();
                }),
            ));

        // ---- 挑哪一份 ----
        let mut prefer_row = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(9.))
            .child(
                div()
                    .w(px(152.))
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t("同名多份时用哪个")),
            );
        for option in crate::settings::PreferProgram::ALL {
            let active = cfg.prefer == option;
            prefer_row = prefer_row.child(
                div()
                    .id(SharedString::from(format!("prefer-{}", option.label())))
                    .flex_none()
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(5.))
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .when(active, |style| {
                        style.bg(theme::accent_soft()).text_color(theme::accent())
                    })
                    .when(!active, |style| {
                        style
                            .bg(theme::track())
                            .text_color(theme::ui_text_secondary())
                            .hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |_this, _: &ClickEvent, window, cx| {
                        settings::update(cx, |settings| settings.programs.prefer = option);
                        window.refresh();
                        cx.notify();
                    }))
                    .child(option.label()),
            );
        }
        let prefer_hint = div()
            .w_full()
            .px(px(9.))
            .text_size(px(10.))
            .text_color(theme::ui_text_tertiary())
            .child(cfg.prefer.hint());

        // ---- 排除规则 ----
        let exclude_row = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(9.))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t("排除规则（路径里包含这段文字就不收录）")),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .p(px(6.))
                    .rounded(px(6.))
                    .bg(theme::surface())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(div().w_full().child(self.prog_exclude.clone())),
            );

        // ---- 认哪些后缀名 ----
        let extensions_row = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(9.))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t("认哪些后缀名（可执行文件不止 .exe）")),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(theme::surface())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(div().w_full().child(self.prog_extensions.clone())),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("多个后缀用 ; 隔开，例如 exe;bat;cmd;msi;msp;msu;ps1;scr;vbs。改完点「重新扫描」生效。")),
            );

        // ---- Everything 联动 ----
        //
        // 这一块取代了原来的「设置 → 索引」页（自建全盘索引）：那份索引要遍历整盘、
        // 常驻几十 MB、还得定时重扫，和「极低资源占用」冲突。全盘找同名文件改问
        // Everything —— 它本来就在读 NTFS 的 MFT + USN 日志。
        let ev = crate::everything::status();
        let ev_connected = ev.backend != crate::everything::Backend::None;
        let ev_ready = ev_connected && !ev.cooling;
        let ev_title = if !ev_connected {
            crate::i18n::t("没有连上 Everything").to_string()
        } else if ev.cooling {
            if crate::i18n::is_english() {
                format!("{} · last query failed, paused", ev.backend.label())
            } else {
                format!("{}（最近查询失败，暂时停用）", ev.backend.label())
            }
        } else if crate::i18n::is_english() {
            format!("Connected: {}", ev.backend.label())
        } else {
            format!("已连接：{}", ev.backend.label())
        };
        let ev_detail = if !ev_connected {
            // 找到了 exe 但还没有 DLL / es.exe：把位置说出来，用户一眼就知道该往哪儿放
            match crate::everything::exe_path() {
                Some(path) => format!(
                    "{}：{}",
                    crate::i18n::t("找到 Everything.exe，但还缺 Everything64.dll / es.exe（放在同一个目录里就行）"),
                    path.display()
                ),
                None => crate::i18n::t("把 Everything 整个文件夹放到程序目录、或在下面填 Everything.exe 的路径就能连上（DLL / es.exe 从同一个目录里找）；没有它也不影响使用。").to_string(),
            }
        } else if let Some(err) = &ev.last_error {
            if crate::i18n::is_english() {
                format!("{} · last failure: {err}", ev.source)
            } else {
                format!("{} · 最近一次失败：{err}", ev.source)
            }
        } else if ev.last_ms > 0 {
            if crate::i18n::is_english() {
                format!("{} · last query {} ms", ev.source, ev.last_ms)
            } else {
                format!("{} · 上次查询 {} ms", ev.source, ev.last_ms)
            }
        } else {
            ev.source.clone()
        };
        let everything_row = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(9.))
            .py(px(6.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(if ev_ready {
                                theme::accent()
                            } else {
                                theme::text_primary()
                            })
                            .child(ev_title),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(ev_detail),
                    ),
            )
            .child(ghost_button(crate::i18n::t("重新检测"), cx.listener(
                |this, _: &ClickEvent, window, cx| {
                    // 清掉缓存和「冷却」，让人可以手动把 Everything 打开后再试一次
                    crate::everything::reset();
                    let _ = this;
                    window.refresh();
                    cx.notify();
                },
            )));
        let everything_hint = div()
            .w_full()
            .px(px(9.))
            .text_size(px(10.))
            .text_color(theme::ui_text_tertiary())
            .child(crate::i18n::t(
                "本程序不再自己建全盘索引：几百万个文件的索引要常驻几十 MB 内存、还得定时重扫。全盘按文件名找路径改成问 Everything（一次几毫秒，本程序不存任何索引）。没装 / 没运行 Everything 时自动退回上面那份可执行文件小索引，功能不受影响。",
            ));

        // ---- Everything.exe 在哪儿：手动填 / 选 / 扫描程序目录 ----
        //
        // 便携版最常见的用法是「把 Everything 整个文件夹拷到本程序旁边」，
        // 所以给一颗「扫描程序目录」：点一下就直接接上，不用手打路径。
        let ev_path_row = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(9.))
            .child(
                div()
                    .w(px(96.))
                    .flex_none()
                    .text_size(px(11.5))
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t("程序路径")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(theme::surface())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(div().w_full().child(self.ev_exe.clone())),
            )
            .child(ghost_button_tagged(
                "ev-pick",
                crate::i18n::t("选择…"),
                cx.listener(|_this, _: &ClickEvent, window, cx| {
                    if let Some(path) = crate::win32::pick_program_file() {
                        settings::update(cx, |settings| settings.everything.exe = path);
                        crate::everything::reload();
                        window.refresh();
                        cx.notify();
                    }
                }),
            ))
            .child(ghost_button_tagged(
                "ev-scan",
                crate::i18n::t("扫描程序目录"),
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    match crate::everything::scan_program_dir() {
                        Some(path) => {
                            let text = path.display().to_string();
                            settings::update(cx, |settings| {
                                settings.everything.exe = text.clone()
                            });
                            crate::everything::reload();
                            this.ev_exe
                                .update(cx, |input, cx| input.set_text(text, cx));
                            this.toast(crate::i18n::t("已接上程序目录里的 Everything"), cx);
                        }
                        None => {
                            this.toast(
                                crate::i18n::t(
                                    "程序目录下没有 Everything 文件夹：把整个文件夹改名成 Everything 放进去再点这里",
                                ),
                                cx,
                            );
                        }
                    }
                    window.refresh();
                    cx.notify();
                }),
            ));
        let ev_auto_row = toggle_row(
            "没运行时随本程序启动",
            "勾上以后，本程序启动时（以及查询发现它没在跑时）顺手把 Everything 拉起来；路径就是上面那个，留空会自动找",
            settings.everything.autostart,
            cx.listener(|_this, _: &ClickEvent, window, cx| {
                let next = settings::read(|settings| !settings.everything.autostart);
                settings::update(cx, |settings| settings.everything.autostart = next);
                // 刚勾上就试一次：用户不用等下次启动
                if next {
                    let _ = crate::everything::autostart_if_needed();
                }
                window.refresh();
                cx.notify();
            }),
        );

        vec![
            section(
                "程序索引",
                "扫 PATH、Program Files、LocalAppData\\Programs，给每个可执行文件记下它的完整路径",
                vec![status_row.into_any_element()],
            ),
            section(
                "怎么挑",
                "同一个文件名在磁盘上有好几份时，按这里的规则选一份；认哪些后缀也在这里定",
                vec![
                    prefer_row.into_any_element(),
                    prefer_hint.into_any_element(),
                    exclude_row.into_any_element(),
                    extensions_row.into_any_element(),
                ],
            ),
            section(
                "Everything 联动",
                "本地索引里没有的名字，交给 Everything 在全盘查完整路径",
                vec![
                    everything_row.into_any_element(),
                    ev_path_row.into_any_element(),
                    ev_auto_row,
                    everything_hint.into_any_element(),
                ],
            ),
        ]
    }

    /// 「文件名」页：菜单里现在用到哪些裸文件名、各自解析到哪、同名有几份。
    ///
    /// 两块：
    /// 1. 菜单里用到的裸文件名 —— 每一行点「选文件」就**先展开所有同名候选**（点「用这个」钉住），
    ///    实在没有想要的，再点「打开文件对话框…」去系统里挑；
    /// 2. 查名字 —— 输入框里随便打一个文件名，列出索引里所有同名的完整路径。
    fn file_names_page(&mut self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cfg = &settings.programs;

        // ---- 菜单里用到的裸文件名 ----
        let all_names = Self::bare_program_names(settings);
        // 「这个名字解析到哪个文件」可能要去问 Everything（一次 IPC）。
        // 以前是**每帧每行算两遍**，菜单项一多就整页卡死；现在只在缓存里读，
        // 没算过的排到后台线程，算好一个刷一下（行上先显示「计算中…」）。
        self.ensure_probes(&all_names, cx);

        // 过滤 + 排序：名字或解析出的路径命中都算（找「没找到路径」的那些最常用）
        let needle = self.prog_name_filter.read(cx).text().trim().to_lowercase();
        let mut names: Vec<String> = all_names
            .iter()
            .filter(|name| {
                if needle.is_empty() {
                    return true;
                }
                if name.to_lowercase().contains(&needle) {
                    return true;
                }
                crate::progindex::peek(name)
                    .and_then(|probe| probe.resolved)
                    .map(|path| path.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let sort = self.prog_sort;
        names.sort_by(|a, b| sort_program_names(sort, a, b));
        let shown = names.len();
        let total = all_names.len();

        // ---- 工具条：过滤框 + 四个排序 + 计数
        let mut toolbar = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(9.))
            .py(px(4.))
            .child(
                div()
                    .w(px(220.))
                    .flex_none()
                    .h(px(22.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(theme::surface_sunken())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(self.prog_name_filter.clone()),
            );
        for option in ProgSort::ALL {
            let selected = option == sort;
            toolbar = toolbar.child(
                div()
                    .id(SharedString::from(format!("prog-sort-{}", option.label())))
                    .flex_none()
                    .h(px(22.))
                    .px(px(7.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .text_size(px(10.))
                    .cursor_pointer()
                    .when(selected, |style| {
                        style
                            .bg(theme::accent_soft())
                            .text_color(theme::accent())
                    })
                    .when(!selected, |style| {
                        style
                            .bg(theme::track())
                            .text_color(theme::ui_text_secondary())
                            .hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.prog_sort = option;
                        window.refresh();
                        cx.notify();
                    }))
                    .child(option.label()),
            );
        }
        toolbar = toolbar.child(div().flex_1()).child(
            div()
                .flex_none()
                .text_size(px(10.))
                .text_color(theme::ui_text_tertiary())
                .child(if crate::i18n::is_english() {
                    format!("{shown}/{total} names")
                } else {
                    format!("{shown} / {total} 个名字")
                }),
        );

        let mut list = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .py(px(2.));
        if all_names.is_empty() {
            list = list.child(
                div()
                    .px(px(9.))
                    .py(px(4.))
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("现在菜单里没有「只填文件名」的动作。在编辑窗口里把程序写成 notepad.exe 这种就会出现。")),
            );
        } else if names.is_empty() {
            list = list.child(
                div()
                    .px(px(9.))
                    .py(px(4.))
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("没有匹配的名字：清掉上面的过滤词，或者按路径排序看看哪些没解析到")),
            );
        }
        for name in &names {
            let key = name.to_ascii_lowercase();
            let fixed = cfg.fixes.get(&key).cloned();
            let probe = crate::progindex::peek(name).unwrap_or_default();
            let resolved = probe.resolved.clone();
            let candidates = probe.candidates.clone();
            let pending = crate::progindex::peek(name).is_none();
            let expanded = self.expanded_names.contains(&key);
            let (text, color) = match (&resolved, &fixed) {
                _ if pending => (
                    crate::i18n::t("计算中…").to_string(),
                    theme::ui_text_tertiary(),
                ),
                (Some(path), Some(_)) => (path.clone(), theme::accent()),
                (Some(path), None) => (path.clone(), theme::ui_text_secondary()),
                (None, Some(path)) => (
                    if crate::i18n::is_english() {
                        format!("pinned file is gone: {path}")
                    } else {
                        format!("指定的文件不在了：{path}")
                    },
                    theme::ui_text_tertiary(),
                ),
                (None, None) => (
                    crate::i18n::t("没找到，动作会交给 cmd 自己按 PATH 找").to_string(),
                    theme::ui_text_tertiary(),
                ),
            };
            let mut row = div()
                .id(SharedString::from(format!("prog-row-{key}")))
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(9.))
                .h(px(26.))
                .child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(theme::text_primary())
                        .child(name.clone()),
                )
                .child(
                    div()
                        .w(px(10.))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child("→"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(10.5))
                        .text_color(color)
                        .child(text),
                );
            if candidates.len() > 1 {
                row = row.child(
                    div()
                        .flex_none()
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(if crate::i18n::is_english() {
                            format!("{} copies", candidates.len())
                        } else {
                            format!("{} 份同名", candidates.len())
                        }),
                );
            }
            // 「选文件」= 展开同名候选列表（不是直接弹系统对话框）
            let toggle_key = key.clone();
            row = row.child(ghost_button(
                crate::i18n::t(if expanded { "收起" } else { "选文件" }),
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if this.expanded_names.contains(&toggle_key) {
                        this.expanded_names.remove(&toggle_key);
                    } else {
                        this.expanded_names.insert(toggle_key.clone());
                    }
                    window.refresh();
                    cx.notify();
                }),
            ));
            if fixed.is_some() {
                let name_owned = name.clone();
                row = row.child(ghost_button(
                    crate::i18n::t("恢复自动"),
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.fix_program(&name_owned, None, cx);
                        window.refresh();
                        cx.notify();
                    }),
                ));
            }
            list = list.child(row);

            // 展开后：把所有同名候选都列出来，各带「用这个」；最后给一个文件对话框兜底
            if expanded {
                let mut candidate_list = div()
                    .id(SharedString::from(format!("prog-cands-{key}")))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .pl(px(24.));
                if candidates.is_empty() {
                    candidate_list = candidate_list.child(
                        div()
                            .px(px(9.))
                            .py(px(3.))
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("索引里没有这个文件的记录，可以直接打开文件对话框去挑。")),
                    );
                }
                for (index, candidate) in candidates.iter().enumerate() {
                    let candidate_path = candidate.path.clone();
                    let name_owned2 = name.clone();
                    let key_owned = key.clone();
                    let mut crow = div()
                        .id(SharedString::from(format!("prog-cand-{key}-{index}")))
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(9.))
                        .h(px(22.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(10.))
                                .text_color(theme::ui_text_secondary())
                                .child(candidate_path.clone()),
                        );
                    if !candidate.version.is_empty() {
                        crow = crow.child(
                            div()
                                .flex_none()
                                .text_size(px(9.))
                                .text_color(theme::ui_text_tertiary())
                                .child(format!("v{}", candidate.version)),
                        );
                    }
                    if candidate.mtime > 0 {
                        crow = crow.child(
                            div()
                                .flex_none()
                                .text_size(px(9.))
                                .text_color(theme::ui_text_tertiary())
                                .child(format_stamp(candidate.mtime)),
                        );
                    }
                    crow = crow.child(ghost_button(
                        crate::i18n::t("用这个"),
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.fix_program(&name_owned2, Some(candidate_path.clone()), cx);
                            this.expanded_names.remove(&key_owned);
                            window.refresh();
                            cx.notify();
                        }),
                    ));
                    candidate_list = candidate_list.child(crow);
                }
                // 兜底：打开系统文件对话框
                let name_owned3 = name.clone();
                candidate_list = candidate_list.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(9.))
                        .h(px(24.))
                        .child(ghost_button(
                            crate::i18n::t("打开文件对话框…"),
                            cx.listener(move |this, _: &ClickEvent, window, cx| {
                                let Some(path) = crate::win32::pick_program_file() else {
                                    return;
                                };
                                this.fix_program(&name_owned3, Some(path), cx);
                                this.expanded_names.remove(&key);
                                window.refresh();
                                cx.notify();
                            }),
                        )),
                );
                list = list.child(candidate_list);
            }
        }

        // ---- 查一个名字：列出所有候选，可以直接选一个钉住 ----
        let query = self.prog_query.read(cx).text();
        let query = query.trim().to_ascii_lowercase();
        let mut results = div().w_full().min_w_0().flex().flex_col().gap(px(2.));
        if query.is_empty() {
            results = results.child(
                div()
                    .px(px(9.))
                    .py(px(4.))
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("在上面输入文件名（不用带路径），这里会列出索引里所有同名的完整路径。")),
            );
        } else {
            let found = crate::progindex::candidates(&query);
            if found.is_empty() {
                results = results.child(
                    div()
                        .px(px(9.))
                        .py(px(4.))
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t("索引里没有这个名字。名字里带不带 .exe 都能查；也可以重新扫描一次。")),
                );
            }
            for (index, candidate) in found.iter().enumerate() {
                let used = index == 0 && cfg.fixes.get(&query).is_none();
                let candidate_path = candidate.path.clone();
                let name_owned = query.clone();
                let mut row = div()
                    .id(SharedString::from(format!("prog-hit-{index}")))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(9.))
                    .h(px(24.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(if used {
                                theme::accent()
                            } else {
                                theme::ui_text_secondary()
                            })
                            .child(candidate_path.clone()),
                    );
                if !candidate.version.is_empty() {
                    row = row.child(
                        div()
                            .flex_none()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(format!("v{}", candidate.version)),
                    );
                }
                if candidate.mtime > 0 {
                    row = row.child(
                        div()
                            .flex_none()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(format_stamp(candidate.mtime)),
                    );
                }
                let name_owned2 = name_owned.clone();
                row = row.child(ghost_button(
                    crate::i18n::t("用这个"),
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.fix_program(&name_owned2, Some(candidate_path.clone()), cx);
                        window.refresh();
                        cx.notify();
                    }),
                ));
                results = results.child(row);
            }
        }

        vec![
            section(
                "菜单里用到的文件名",
                "只写了名字的动作，执行时会解析成完整路径；不对就点「选文件」挑一个，同名会先全部列出来",
                vec![
                    toolbar.into_any_element(),
                    list.into_any_element(),
                ],
            ),
            section(
                "查名字",
                "看看某个文件名在磁盘上有几份，各在哪",
                vec![
                    div()
                        .w_full()
                        .min_w_0()
                        .px(px(9.))
                        .child(
                            div()
                                .w_full()
                                .h(px(24.))
                                .px(px(6.))
                                .flex()
                                .items_center()
                                .rounded(px(6.))
                                .bg(theme::surface())
                                .border_1()
                                .border_color(theme::border_strong())
                                .child(self.prog_query.clone()),
                        )
                        .into_any_element(),
                    results.into_any_element(),
                ],
            ),
        ]
    }


    /// 手动钉死 / 取消钉死某个名字用哪个路径。
    fn fix_program(&mut self, name: &str, path: Option<String>, cx: &mut Context<Self>) {
        // 「手动指定」变了：这一页的解析结果全作废，重新算
        crate::progindex::clear_probes();
        let key = name.trim().to_ascii_lowercase();
        settings::update(cx, |settings| match path {
            Some(path) => {
                settings.programs.fixes.insert(key, path);
            }
            None => {
                settings.programs.fixes.remove(&key);
            }
        });
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // 关于页
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // 热字符串
    // -----------------------------------------------------------------------

    /// 「热字符串」页：敲一串字就执行菜单项（AHK 的 hotstring）。
    ///
    /// 这里只管**全局参数**：开不开、等不等结束符、提示长什么样、贴在哪儿。
    /// 「哪串字绑哪一项」不在这儿列表 —— 热字符串和「点击」「快捷键」一样，
    /// 只是某一个菜单项的一个属性，在那一项的编辑窗口里配才不容易迷路；
    /// 想看"现在都配了哪些"，去「菜单项」页点那颗「有热字符串」筛选。
    /// 自动规则页：卡片列表 + 内联编辑（可视化，不用背变量键名）。
    fn auto_page(&mut self, _settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (enabled, notice, rules) = settings::read(|settings| {
            (
                settings.auto_enabled,
                settings.auto_notice,
                settings.auto_rules.clone(),
            )
        });

        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(toggle_row(
            "启用自动规则",
            "启动时按下面的顺序逐条检测；命中就执行，关掉之后一条都不跑",
            enabled,
            cx.listener(|_this, _: &ClickEvent, window, cx| {
                settings::update(cx, |settings| {
                    settings.auto_enabled = !settings.auto_enabled;
                });
                window.refresh();
                cx.notify();
            }),
        ));
        rows.push(toggle_row(
            "执行时弹提示",
            "命中并执行时在鼠标旁提示一下，知道是规则干的而不是手滑",
            notice,
            cx.listener(|_this, _: &ClickEvent, window, cx| {
                settings::update(cx, |settings| {
                    settings.auto_notice = !settings.auto_notice;
                });
                window.refresh();
                cx.notify();
            }),
        ));

        // ---- 规则列表
        let mut list = div()
            .relative()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(6.));
        if rules.is_empty() {
            list = list.child(
                div()
                    .w_full()
                    .py(px(18.))
                    .flex()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("还没有规则，点下面的「新增规则」开始")),
            );
        }
        let total = rules.len();
        for (index, rule) in rules.iter().enumerate() {
            list = list.child(self.auto_card(index, rule, total, cx));
        }

        // 拖动松手：把规则挪到悬停的位置
        let list = list
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                let Some(id) = this.auto_drag.clone() else {
                    return;
                };
                if let Some(to) = this.auto_drop {
                    settings::update(cx, |settings| settings.move_auto_rule(&id, to));
                }
                this.auto_drag = None;
                this.auto_drop = None;
                window.refresh();
                cx.notify();
            }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    if this.auto_drag.is_some() {
                        this.auto_drag = None;
                        this.auto_drop = None;
                        window.refresh();
                        cx.notify();
                    }
                }),
            );

        let mut body = div().w_full().min_w_0().flex().flex_col().gap(px(8.));
        body = body.child(list);
        body = body.child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(primary_button(
                    crate::i18n::t("新增规则"),
                    cx.listener(|_this, _: &ClickEvent, window, cx| {
                        let id = settings::update(cx, |settings| settings.add_auto_rule());
                        crate::rule_editor::open(&id, cx);
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
                            "按住卡片左边的 ⠿ 可以上下拖动排序；条件行里点变量名就能换变量",
                        )),
                ),
        );
        rows.push(section("规则", "", vec![body.into_any_element()]));
        rows
    }

    /// 一张规则卡片（含内联编辑面板）。
    fn auto_card(
        &mut self,
        index: usize,
        rule: &settings::AutoRule,
        total: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = rule.id.clone();
        let id_grip = id.clone();
        let id_on = id.clone();
        let id_toggle = id.clone();
        let id_edit = id.clone();
        let id_copy = id.clone();
        let id_del = id.clone();
        let editing = false;
        let dragging = self.auto_drag.as_deref() == Some(id.as_str());
        let drop_here = self.auto_drop == Some(index) && self.auto_drag.is_some();

        let mut card = div()
            .id(SharedString::from(format!("auto-card-{id}")))
            .w_full()
            .min_w_0()
            .rounded(px(theme::RADIUS_SMALL))
            .border_1()
            .border_color(if drop_here {
                theme::accent()
            } else if editing {
                theme::accent_soft()
            } else {
                theme::border()
            })
            .bg(if editing {
                theme::surface_hover()
            } else {
                theme::surface_sunken()
            })
            .px(px(8.))
            .py(px(7.))
            .flex()
            .flex_col()
            .gap(px(6.));

        // 拖动时把鼠标所在的那张卡记成落点
        if self.auto_drag.is_some() {
            let index_now = index;
            card = card.on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _w, cx| {
                if this.auto_drop != Some(index_now) {
                    this.auto_drop = Some(index_now);
                    cx.notify();
                }
            }));
        }

        let mut head = div().flex().items_center().gap(px(8.)).w_full().min_w_0();

        // 拖动把手
        head = head.child(
            div()
                .id(SharedString::from(format!("auto-grip-{id}")))
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
                        this.auto_drag = Some(id_grip.clone());
                        this.auto_drop = Some(index);
                        window.refresh();
                        cx.notify();
                    }),
                ),
        );

        // 启用勾
        head = head.child(
            div()
                .id(SharedString::from(format!("auto-on-{id}")))
                .flex_none()
                .size(px(15.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .border_1()
                .border_color(if rule.enabled {
                    theme::accent()
                } else {
                    theme::border_strong()
                })
                .bg(if rule.enabled {
                    theme::accent()
                } else {
                    theme::surface()
                })
                .cursor_pointer()
                .text_size(px(9.))
                .text_color(theme::text_on_accent())
                .child(if rule.enabled { "✓" } else { "" })
                .on_click(cx.listener(move |_this, _: &ClickEvent, window, cx| {
                    let key = id_on.clone();
                    settings::update(cx, |settings| {
                        if let Some(rule) = settings.auto_rule_mut(&key) {
                            rule.enabled = !rule.enabled;
                        }
                    });
                    window.refresh();
                    cx.notify();
                })),
        );

        // 名称 + 摘要
        let name = if rule.name.trim().is_empty() {
            crate::i18n::t("（未命名）").to_string()
        } else {
            rule.name.clone()
        };
        head = head.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(theme::text_primary())
                        .child(name),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::autorule::summarize(rule)),
                ),
        );

        // 统计 + 操作
        head = head.child(
            div()
                .flex_none()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(if rule.rounds == 0 || rule.interval_secs == 0 {
                    "只在启动时检测一次".to_string()
                } else {
                    format!("{} 轮 × {}s", rule.rounds, rule.interval_secs)
                }),
        );
        // 和「菜单项」页一样的图标按钮：✎ 编辑 / ⧉ 复制 / 👁 启用禁用 / ✕ 删除
        head = head.child(icon_action(
            format!("auto-edit-{id}"),
            "icons/edit.svg",
            crate::i18n::t("编辑"),
            cx.listener(move |_this, _: &ClickEvent, window, cx| {
                crate::rule_editor::open(&id_edit, cx);
                window.refresh();
            }),
        ));
        head = head.child(icon_action(
            format!("auto-copy-{id}"),
            "icons/copy.svg",
            crate::i18n::t("复制"),
            cx.listener(move |_this, _: &ClickEvent, window, cx| {
                let key = id_copy.clone();
                let copy = settings::update(cx, |settings| settings.duplicate_auto_rule(&key));
                if let Some(copy) = copy {
                    crate::rule_editor::open(&copy, cx);
                }
                window.refresh();
                cx.notify();
            }),
        ));
        head = head.child(icon_action(
            format!("auto-toggle-{id}"),
            if rule.enabled {
                "icons/eye.svg"
            } else {
                "icons/eye-off.svg"
            },
            if rule.enabled {
                crate::i18n::t("禁用这条规则")
            } else {
                crate::i18n::t("启用这条规则")
            },
            cx.listener(move |_this, _: &ClickEvent, window, cx| {
                let key = id_toggle.clone();
                settings::update(cx, |settings| {
                    if let Some(rule) = settings.auto_rule_mut(&key) {
                        rule.enabled = !rule.enabled;
                    }
                });
                window.refresh();
                cx.notify();
            }),
        ));
        head = head.child(icon_action(
            format!("auto-del-{id}"),
            "icons/trash.svg",
            crate::i18n::t("删除"),
            cx.listener(move |_this, _: &ClickEvent, window, cx| {
                let key = id_del.clone();
                settings::update(cx, |settings| settings.remove_auto_rule(&key));
                window.refresh();
                cx.notify();
            }),
        ));

        let _ = total;
        card = card.child(head);
        card.into_any_element()
    }

    fn hotstrings_page(
        &mut self,
        settings: &MenuSettings,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let cfg = &settings.hotstrings;
        let count = settings.hotstring_phrases().len();

        // ---- 提示参数 ----
        let opacity = cfg.hint_opacity.clamp(20, 100) as f32;
        let (op_lo, op_hi) = settings::HotstringSettings::limits_opacity();
        let hint_rows = vec![
            toggle_row(
                "隐藏热字符串提示",
                "只在后台替换，不弹那张小卡片",
                cfg.hide_hint,
                cx.listener(|_this, _: &ClickEvent, window, cx| {
                    settings::update(cx, |settings| {
                        settings.hotstrings.hide_hint = !settings.hotstrings.hide_hint;
                    });
                    window.refresh();
                    cx.notify();
                }),
            ),
            row_with_control(
                "提示显示位置",
                "跟着鼠标，还是贴着正在输入的那个光标（插入点）",
                segmented(
                    "hs-anchor",
                    &anchor_options(),
                    settings::HintAnchor::ALL
                        .iter()
                        .position(|anchor| *anchor == cfg.anchor)
                        .unwrap_or(0) as u32,
                    cx.listener(|_this, value: &u32, window, cx| {
                        let anchor = settings::HintAnchor::ALL
                            .get(*value as usize)
                            .copied()
                            .unwrap_or_default();
                        settings::update(cx, |settings| settings.hotstrings.anchor = anchor);
                        window.refresh();
                        cx.notify();
                    }),
                ),
            ),
            toggle_row(
                "提示可以用鼠标点",
                "鼠标移上去不消失；列了好几条的时候，点哪条就直接执行哪条",
                cfg.hint_clickable,
                cx.listener(|_this, _: &ClickEvent, window, cx| {
                    settings::update(cx, |settings| {
                        settings.hotstrings.hint_clickable = !settings.hotstrings.hint_clickable;
                    });
                    window.refresh();
                    cx.notify();
                }),
            ),
            row_with_control(
                "按几个字符出现提示",
                "打满这么多个字符开始提示；敲得比这少就当普通打字，什么都别弹",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(number_box(&self.hs_after, 58.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("个字符")),
                    )
                    .into_any_element(),
            ),
            row_with_control(
                "提示启动路径最长字数",
                "提示里那行完整路径最多显示几个字，0 = 不显示路径",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(number_box(&self.hs_path_chars, 58.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("字（0 隐藏）")),
                    )
                    .into_any_element(),
            ),
            row_with_control(
                "提示显示时长",
                "到点自己消失（鼠标停在上面时不会消失）",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(number_box(&self.hs_ms, 70.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("毫秒")),
                    )
                    .into_any_element(),
            ),
            row_with_control(
                "提示显示透明度",
                "越透明越不挡视线；调到 100% 就是实心卡片",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(self.slider(
                        "hs-opacity",
                        "",
                        opacity,
                        op_lo as f32,
                        op_hi as f32,
                        5.0,
                        "%",
                        set_hint_opacity,
                        cx,
                    ))
                    .into_any_element(),
            ),
            row_with_control(
                "提示相对坐标 X",
                "跟着鼠标时相对鼠标、跟着光标时相对插入点；允许负数",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(number_box(&self.hs_offset_x, 58.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("像素")),
                    )
                    .into_any_element(),
            ),
            row_with_control(
                "提示相对坐标 Y",
                "负数往上放；贴着光标时 -6 左右刚好不压住字",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(number_box(&self.hs_offset_y, 58.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("像素")),
                    )
                    .into_any_element(),
            ),
            row_with_control(
                "结束字符",
                "空格 / 回车 / Tab 永远算打完了；这里填的符号也算（AHK 的 EndChars）",
                div()
                    .w(px(196.))
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(theme::surface_sunken())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(self.hs_end_chars.clone())
                    .into_any_element(),
            ),
            toggle_row(
                "区分大小写",
                "关掉就跟 AHK 一样：FtW 和 ftw 算同一个短语",
                cfg.case_sensitive,
                cx.listener(|_this, _: &ClickEvent, window, cx| {
                    settings::update(cx, |settings| {
                        settings.hotstrings.case_sensitive = !settings.hotstrings.case_sensitive;
                    });
                    window.refresh();
                    cx.notify();
                }),
            ),
            // —— 权限提示：不解释的话，「提示弹了但没反应」根本查不出来
            section(
                "权限",
                if crate::win32::is_elevated() {
                    "本程序已以管理员身份运行，可以往任何程序里注入按键"
                } else {
                    "本程序不是管理员：往**管理员权限**的程序（记事本、部分编辑器）里注入按键会被系统丢掉，表现是「提示弹了但没反应」"
                },
                if crate::win32::is_elevated() {
                    Vec::new()
                } else {
                    vec![info_row(
                        "怎么办",
                        "右键 RunAny_RS.exe → 以管理员身份运行；或用非管理员的程序（浏览器地址栏）验证热字符串本身是好的",
                    )]
                },
            ),
            toggle_row(
                "输入法组字时也匹配（实验）",
                "关着＝和 AHK 一样：中文输入法正在组字时一个键都不抢（推荐）。开着＝先让输入法丢掉组字再展开，部分输入法可能把展开内容吸进候选",
                cfg.ime_match,
                cx.listener(|_this, _: &ClickEvent, window, cx| {
                    settings::update(cx, |settings| {
                        settings.hotstrings.ime_match = !settings.hotstrings.ime_match;
                    });
                    window.refresh();
                    cx.notify();
                }),
            ),
        ];

        vec![
            section(
                "热字符串",
                "在任何程序里敲一串字 → 执行绑定的菜单项（和「点击」「快捷键」并列的第三种触发方式）",
                vec![
                    toggle_row(
                        "启用热字符串",
                        "关掉就撤掉那个键盘钩子；一条短语都没配的时候也等于没开（零开销）",
                        cfg.enabled,
                        cx.listener(|_this, _: &ClickEvent, window, cx| {
                            settings::update(cx, |settings| {
                                settings.hotstrings.enabled = !settings.hotstrings.enabled;
                            });
                            window.refresh();
                            cx.notify();
                        }),
                    ),
                    info_row(
                        "中文输入法",
                        crate::i18n::t(
                            "匹配只看你实际按了哪些键，不看输入法在不在组字；执行前先让输入法丢掉没上屏的拼音，再按屏幕上真实的字数退格",
                        ),
                    ),
                    row_with_control(
                        "短语在哪配",
                        "短语是「菜单项」的触发方式之一，在「菜单项」页点 ✎ 就能配",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(theme::ui_text_tertiary())
                                    .child(if crate::i18n::is_english() {
                                        format!("{count} configured")
                                    } else {
                                        format!("现在 {count} 条")
                                    }),
                            )
                            .child(ghost_button_if(
                                "筛出有热字符串的项",
                                true,
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.page = Page::Items;
                                    this.items_tree.update(cx, |tree, cx| {
                                        tree.filter_hotstrings(cx);
                                    });
                                    window.refresh();
                                    cx.notify();
                                }),
                            ))
                            .into_any_element(),
                    ),
                ],
            ),
            section(
                "提示",
                "敲到第几个字符冒出来、长什么样、停多久、多透明、贴在哪儿",
                hint_rows,
            ),
        ]
    }

    fn about_page(&mut self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // 项目仓库：点一下用默认浏览器打开，地址本身也摊在界面上（方便手抄/复制）
        let repo = "https://github.com/lch319/RunAny_RS";
        let repo_row = div()
            .id("about-repo")
            .px(px(9.))
            .py(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click({
                let repo = repo.to_string();
                move |_event, _window, cx: &mut gpui::App| {
                    let step = settings::ActionStep::new(
                        settings::ActionKind::Url,
                        repo.clone(),
                    );
                    let _ = crate::popup::test_run_step(cx, &step);
                }
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(theme::accent())
                    .child("github.com/lch319/RunAny_RS"),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("点击用浏览器打开 · 问题反馈 / 更新都在这里")),
            )
            .into_any_element();

        // ---- 检查更新 ----
        let update_state = crate::update::state();
        // 进这一页就顺手查一次（设置里可以关掉）
        if settings.check_updates && matches!(update_state, crate::update::State::Idle) {
            crate::update::check(cx, false);
        }
        // 正在检查时每 400ms 自己刷一帧：网络请求在后台跑，回来时没有别的
        // 东西会通知这一页重绘，不主动轮询的话会一直停在「正在检查…」
        if matches!(update_state, crate::update::State::Checking) {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(400))
                    .await;
                let _ = this.update(cx, |_this, cx| cx.notify());
            })
            .detach();
        }
        let (status_text, status_color) = match &update_state {
            crate::update::State::Idle => ("还没检查过".to_string(), theme::ui_text_tertiary()),
            crate::update::State::Checking => ("正在检查…".to_string(), theme::ui_text_tertiary()),
            crate::update::State::UpToDate => (
                format!("已是最新（v{}）", crate::update::current()),
                theme::ui_text_tertiary(),
            ),
            crate::update::State::Newer { version, .. } => (
                format!("发现新版本 {version}，点右边去下载"),
                theme::accent(),
            ),
            crate::update::State::Failed(err) => (err.clone(), theme::danger()),
        };
        let mut update_rows = vec![
            div()
                .id("about-auto-update")
                .px(px(9.))
                .py(px(7.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(|_this, _: &ClickEvent, window, cx| {
                    let next = settings::read(|settings| !settings.check_updates);
                    settings::update(cx, move |settings| settings.check_updates = next);
                    window.refresh();
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(11.5))
                        .text_color(theme::text_primary())
                        .child(crate::i18n::t("自动检查更新（进这一页时查一次）")),
                )
                .child(switch(settings.check_updates))
                .into_any_element(),
            div()
                .px(px(9.))
                .py(px(7.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(ghost_button_tagged(
                    "about-check-update",
                    "检查更新",
                    cx.listener(|_this, _: &ClickEvent, window, cx| {
                        crate::update::check(cx, true);
                        window.refresh();
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(10.5))
                        .text_color(status_color)
                        .child(status_text),
                )
                .into_any_element(),
        ];
        if let crate::update::State::Newer { url, .. } = &update_state {
            let url = url.clone();
            update_rows.push(
                div()
                    .id("about-download")
                    .px(px(9.))
                    .py(px(7.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(move |_event, _window, cx: &mut gpui::App| {
                        let step = settings::ActionStep::new(
                            settings::ActionKind::Url,
                            url.clone(),
                        );
                        let _ = crate::popup::test_run_step(cx, &step);
                    })
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme::accent())
                            .child(crate::i18n::t("打开下载页")),
                    )
                    .into_any_element(),
            );
        }

        vec![
            section(
                "关于",
                "RunAny_RS · MIT · Rust + GPUI",
                vec![
                    repo_row,
                    info_row("版本", env!("CARGO_PKG_VERSION")),
                    info_row("许可", "MIT（可自由使用 / 修改 / 再发布）"),
                ],
            ),
            section("更新", "问 GitHub 上的最新 release", update_rows),
            section(
                "全局快捷键",
                "在任何应用里按下都会在鼠标处弹出",
                vec![div()
                    .px(px(9.))
                    .py(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            // 设置里怎么写就怎么切：Ctrl+Alt+M → 三个键帽
                            .children(
                                settings
                                    .popup_hotkey
                                    .split('+')
                                    .map(str::trim)
                                    .filter(|part| !part.is_empty())
                                    .map(|part| key_cap_dynamic(SharedString::from(part.to_string()))),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("在「热键」页可以改成别的组合")),
                    )
                    .into_any_element()],
            ),
            section(
                "操作",
                "鼠标和键盘都能用",
                vec![
                    info_row("↑ / ↓", crate::i18n::t("在当前列里移动高亮，会自动跟滚")),
                    info_row("→ / ←", crate::i18n::t("展开子菜单 / 返回上一级")),
                    info_row("Enter", crate::i18n::t("执行菜单项（有子菜单则展开）")),
                    info_row("Esc", crate::i18n::t("先收子菜单，再按一次关闭")),
                    info_row("滚轮 / 拖动 / 点箭头", crate::i18n::t("长菜单的三种滚动方式")),
                    info_row("点击空白处", crate::i18n::t("关闭菜单，焦点还给原来的窗口")),
                    info_row(
                        "菜单项快捷键",
                        crate::i18n::t(
                            "编辑窗口里的「触发快捷键」：普通项按下就直接执行，子菜单按下只弹这一支",
                        ),
                    ),
                ],
            ),
        ]
    }

    /// 「配置」页：换菜单配置、备份恢复、配置文件在哪。
    fn config_page(&mut self, settings: &MenuSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            section(
                "当前配置",
                "换一份菜单配置只是预览，点「应用 / 确定」才写盘",
                vec![
                    info_row(
                        "设置文件",
                        settings::config_path_display()
                            .unwrap_or_else(|| crate::i18n::t("（无可用目录）").into()),
                    ),
                    info_row(
                        "菜单配置",
                        // 只显示「menus/名字.runa」这一小段：完整路径又长又没用，
                        // 要真去找文件就点下面那个「打开配置目录」。
                        format!(
                            "{} · menus{}{}.runa",
                            settings.shown_profile(),
                            std::path::MAIN_SEPARATOR,
                            settings.shown_profile()
                        ),
                    ),
                    div()
                        .px(px(9.))
                        .py(px(4.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ghost_button(
                            "打开配置目录",
                            cx.listener(|_this, _: &ClickEvent, _window, _cx| {
                                if let Some(dir) = settings::config_dir_display() {
                                    let dir = std::path::PathBuf::from(dir);
                                    let _ = std::fs::create_dir_all(dir.join("menus"));
                                    let _ = std::process::Command::new("explorer").arg(dir).spawn();
                                }
                            }),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(10.5))
                                .text_color(theme::ui_text_tertiary())
                                .child(crate::i18n::t("菜单配置文件就在 menus 文件夹里，拷走一个 .runa 就是一套菜单")),
                        )
                        .into_any_element(),
                ],
            ),
            section(
                "从 RunAny 导入",
                "读原版 RunAny.ini：菜单项 / 子菜单 / 分隔线 / 快捷键 / 动作类型一一对应，单独存成一份新配置",
                vec![div()
                    .px(px(9.))
                    .py(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ghost_button(
                        "选择 RunAny.ini",
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.import_runany_ini(window, cx);
                        }),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t(
                                "注释行会跳过；插件函数（名字[函数](参数)）原样保留并加一条备注",
                            )),
                    )
                    .into_any_element()],
            ),
            self.backup_section(cx),
            section(
                "其他",
                "",
                vec![div()
                    .px(px(9.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ghost_button(
                        "打开菜单",
                        cx.listener(|_this, _: &ClickEvent, _window, cx| {
                            popup::open(cx);
                        }),
                    ))
                    .child(ghost_button(
                        if self.is_armed(ResetKind::All) {
                            "再点一次确认"
                        } else {
                            "恢复默认设置"
                        },
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            // 这一下可能把配色 / 尺寸 / 菜单全清掉，得点两次
                            if this.confirmed(ResetKind::All, cx) {
                                settings::update(cx, |settings| settings.reset_all());
                                this.items_tree.update(cx, |tree, cx| tree.reset_state(cx));
                                this.tray_tree.update(cx, |tree, cx| tree.reset_state(cx));
                                this.committed = settings::current();
                                arm_close_guard(&this.committed);
                                // 模板也一起回默认了
                                this.sync_tip_text(cx);
                            }
                            window.refresh();
                            cx.notify();
                        }),
                    ))
                    .into_any_element()],
            ),
        ]
    }

    /// 「配置备份」：把 settings.json + menus 一起存一份，出事了点一下就回来。
    /// 选一个原版 RunAny.ini 导入成一份新配置。
    ///
    /// 两个入口共用：「配置」页那块「从 RunAny 导入」，和「菜单项」页工具条上的
    /// 「导入 ini」按钮 —— 从旧机器搬过来的人就在这一页，不该先跑去配置页找。
    fn import_runany_ini(&mut self, window: &mut Window, cx: &mut Context<Self>) {
                            // 选文件是模态的（GetOpenFileNameW 自己开消息循环），
                            // 选完回来才继续，所以这里可以直接同步读、同步导入
                            let Some(path) = crate::win32::pick_ini_file() else {
                                return;
                            };
                            let hint = std::path::Path::new(&path)
                                .file_stem()
                                .and_then(|stem| stem.to_str())
                                .unwrap_or("RunAny")
                                .to_string();
                            let result = crate::runany_import::read_ini(&path)
                                .and_then(|text| crate::runany_import::import_text(cx, &text, &hint));
                            match result {
                                Ok((profile, stats)) => {
                                    self.backup_note = if crate::i18n::is_english() {
                                        format!(
                                            "Imported {} items ({} submenus, {} hotkeys) into new config \"{profile}\"",
                                            stats.items, stats.submenus, stats.hotkeys
                                        )
                                    } else {
                                        format!(
                                            "已导入 {} 项（{} 个子菜单、{} 个快捷键），新建配置「{profile}」并切过去了。原版的 `|public` 这类关联只记成备注，不参与筛选（否则菜单会缺项）",
                                            stats.items, stats.submenus, stats.hotkeys
                                        )
                                    };
                                    // 整棵树换了：选中 / 展开 / 拖动状态全按 id 记，必须清掉
                                    self.items_tree.update(cx, |tree, cx| tree.reset_state(cx));
                                    self.tray_tree.update(cx, |tree, cx| tree.reset_state(cx));
                                    // 一千多项的图标在后台抠好存进 `Icon/`：等用户第一次
                                    // 呼出菜单时就不用再挨个去查「这个程序在哪」了
                                    {
                                        let (items, auto_icons) = settings::read(|settings| {
                                            (settings.items.clone(), settings.auto_icons)
                                        });
                                        cx.spawn(async move |_view, cx| {
                                            cx.background_executor()
                                                .spawn(async move {
                                                    crate::icon_cache::warm(&items, auto_icons);
                                                })
                                                .await;
                                            let _ = cx.update(repaint_trees);
                                        })
                                        .detach();
                                    }
                                    self.committed = settings::current();
                                    arm_close_guard(&self.committed);
                                }
                                Err(err) => self.backup_note = err,
                            }
                            window.refresh();
                            cx.notify();
        window.refresh();
        cx.notify();
    }

    fn backup_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.backups_loaded {
            self.backups = crate::backup::list();
            self.backups_loaded = true;
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            div()
                .px(px(9.))
                .py(px(4.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(ghost_button(
                    "立即备份",
                    cx.listener(|this, _: &ClickEvent, _window, cx| {
                        match crate::backup::create() {
                            Ok(name) => {
                                this.backup_note =
                                    format!("{} backups/{name}", crate::i18n::t("已备份到"))
                            }
                            Err(err) => {
                                this.backup_note = format!("{}: {err}", crate::i18n::t("备份失败"))
                            }
                        }
                        this.backups_loaded = false;
                        cx.notify();
                    }),
                ))
                .child(ghost_button(
                    "打开备份目录",
                    cx.listener(|this, _: &ClickEvent, _window, cx| {
                        if let Err(err) = crate::backup::open_dir() {
                            this.backup_note = err;
                        }
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(if self.backup_note.is_empty() {
                            crate::i18n::t("备份里含 settings.json 和整个 menus 文件夹").to_string()
                        } else {
                            self.backup_note.clone()
                        }),
                )
                .into_any_element(),
        );

        if self.backups.is_empty() {
            rows.push(
                div()
                    .px(px(9.))
                    .py(px(2.))
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "还没有备份：点「立即备份」，之后随时能恢复到这一刻",
                    ))
                    .into_any_element(),
            );
        } else {
            for entry in self.backups.iter().take(6).cloned() {
                let armed = self
                    .confirm_restore
                    .as_ref()
                    .is_some_and(|(name, at)| name == &entry.name && at.elapsed() < CONFIRM_WINDOW);
                let name = entry.name.clone();
                rows.push(
                    div()
                        .id(SharedString::from(format!("backup-row-{name}")))
                        .px(px(9.))
                        .py(px(3.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.))
                                .child(if crate::i18n::is_english() {
                                    format!("{}  ·  {} files", entry.at, entry.files)
                                } else {
                                    format!("{}  ·  {} 个文件", entry.at, entry.files)
                                }),
                        )
                        .child(ghost_button(
                            if armed { "再点一次确认" } else { "恢复" },
                            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                let armed = this.confirm_restore.as_ref().is_some_and(|(n, at)| {
                                    n == &name && at.elapsed() < CONFIRM_WINDOW
                                });
                                if !armed {
                                    this.confirm_restore =
                                        Some((name.clone(), std::time::Instant::now()));
                                    cx.notify();
                                    return;
                                }
                                match crate::backup::restore(&name) {
                                    Ok(safety) => {
                                        if settings::reload(cx) {
                                            this.backup_note = if crate::i18n::is_english() {
                                                format!("Restored. Previous state saved as backups/{safety}")
                                            } else {
                                                format!("已恢复；恢复前的状态存到了 backups/{safety}")
                                            };
                                            // 整棵树都换了：UI 里的展开 / 选中 / 回滚点一起刷新
                                            this.committed = settings::current();
                                            this.items_tree
                                                .update(cx, |tree, cx| tree.reset_state(cx));
                                            this.tray_tree
                                                .update(cx, |tree, cx| tree.reset_state(cx));
                                            this.sync_tip_text(cx);
                                        } else {
                                            this.backup_note = crate::i18n::t(
                                                "文件写回去了，但读不回来（备份可能坏了）",
                                            )
                                            .to_string();
                                        }
                                    }
                                    Err(err) => {
                                        this.backup_note = format!("{}: {err}", crate::i18n::t("恢复失败"))
                                    }
                                }
                                this.confirm_restore = None;
                                this.backups_loaded = false;
                                cx.notify();
                            }),
                        ))
                        .into_any_element(),
                );
            }
        }

        section(
            "配置备份",
            "换电脑 / 手滑删了菜单，都能一键回到某一刻",
            rows,
        )
    }

    /// 悬停提示的模板编辑器：一行一条 → 占位符胶囊 → 实时预览。
    fn tip_editor(&self, settings: &MenuSettings, cx: &mut Context<Self>) -> AnyElement {
        let tip = &settings.tooltip;
        let (min_w, max_w) = crate::settings::TooltipSettings::limits_width();
        let width = tip.width.clamp(min_w, max_w);

        // 标题行：说明 + 「恢复默认模板」
        let header = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(9.))
            .pt(px(6.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::text_primary())
                    .child(crate::i18n::t("内容模板")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("一个大框，换行就是下一条；行首加 ? = 这一行里有空占位符就整行不显示")),
            )
            .child(ghost_button(
                "恢复默认模板",
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.tip_reset(cx);
                    window.refresh();
                }),
            ));

        // 编辑区：一个大的多行编辑框。点一下就能打字，占位符胶囊插在光标处。
        let editor = div()
            .w_full()
            .min_w_0()
            .p(px(6.))
            .rounded(px(6.))
            .bg(theme::surface_sunken())
            .border_1()
            .border_color(theme::border_strong())
            .child(div().w_full().child(self.tip_text.clone()));

        // 占位符胶囊
        let mut chips = div().flex().items_center().gap(px(3.)).flex_wrap().px(px(9.));
        chips = chips.child(
            div()
                .flex_none()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t("占位符")),
        );
        for (name, hint) in crate::tip_template::PLACEHOLDERS {
            let name = *name;
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("tip-token-{name}")))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(theme::track())
                    .text_size(px(9.5))
                    .text_color(theme::text_secondary())
                    .cursor_pointer()
                    .hover(|style| {
                        style
                            .bg(theme::accent_soft())
                            .text_color(theme::accent())
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.tip_insert(name, cx);
                        window.refresh();
                    }))
                    .child(format!("{{{name}}}")),
            );
            let _ = hint;
        }
        chips = chips.child(
            div()
                .flex_none()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t("· 下面有每个占位符的说明")),
        );
        // 对齐标记也是「能插的东西」，单独三个
        for align in [
            crate::tip_template::TipAlign::Left,
            crate::tip_template::TipAlign::Center,
            crate::tip_template::TipAlign::Right,
        ] {
            let token = align.key();
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("tip-token-{token}")))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(theme::track())
                    .text_size(px(9.5))
                    .text_color(theme::accent())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::accent_soft()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.tip_insert(token, cx);
                        window.refresh();
                    }))
                    .child(format!("{{{token}}}")),
            );
        }

        // 每个占位符代表什么（用户问过「这些都代表啥」，直接摆出来）
        let mut legend = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .gap(px(3.))
            .px(px(9.))
            .pt(px(2.))
            .child(
                div()
                    .w_full()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("上面点一下就插到光标处。每个占位符代表：")),
            );
        let entries: Vec<(String, &'static str, bool)> = crate::tip_template::PLACEHOLDERS
            .iter()
            .map(|(name, hint)| (format!("{{{name}}}"), crate::i18n::t(*hint), false))
            .chain([
                ("{left}".to_string(), crate::i18n::t("整行靠左"), true),
                ("{center}".to_string(), crate::i18n::t("整行居中"), true),
                ("{right}".to_string(), crate::i18n::t("整行靠右"), true),
            ])
            .collect();
        for (token, hint, is_align) in entries {
            legend = legend.child(
                div()
                    .w(px(150.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .text_size(px(9.5))
                    .child(
                        div()
                            .flex_none()
                            .text_color(if is_align {
                                theme::accent()
                            } else {
                                theme::text_secondary()
                            })
                            .child(token),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::ui_text_tertiary())
                            .child(hint),
                    ),
            );
        }
        legend = legend.child(
            div()
                .w_full()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .child(crate::i18n::t(
                    "行首的 ? 是条件行：这一行里的占位符只要有一个没内容，整行就不显示（省得留下空行）",
                )),
        );

        // 实时预览：按当前模板 + 一个真实菜单项渲染，宽度也跟着滑块走
        let context = tip_preview_context(settings);
        let preview_lines = crate::tip_template::render(&tip.template, &context, tip.align);
        let mut preview = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(9.))
            .w(px(width))
            .rounded(px(7.))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border_strong());
        if preview_lines.is_empty() {
            preview = preview.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("（这一项用当前模板什么都不显示）")),
            );
        }
        for line in &preview_lines {
            let title = line.title;
            let mut row = div().w_full().flex().items_center().child(
                div()
                    .min_w_0()
                    .text_size(px(if title { 12.5 } else { 11.5 }))
                    .when(title, |style| style.font_weight(FontWeight::SEMIBOLD))
                    .text_color(if title {
                        theme::text_primary()
                    } else {
                        theme::text_secondary()
                    })
                    .child(line.text.clone()),
            );
            row = match line.align {
                crate::tip_template::TipAlign::Left | crate::tip_template::TipAlign::Custom => {
                    row.justify_start()
                }
                crate::tip_template::TipAlign::Center => row.justify_center(),
                crate::tip_template::TipAlign::Right => row.justify_end(),
            };
            preview = preview.child(row);
        }

        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.))
            .pb(px(2.))
            .child(header)
            .child(div().px(px(9.)).child(editor))
            .child(chips)
            .child(legend)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(9.))
                    .pt(px(2.))
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("预览")),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(format!("· {}", context.label)),
                    ),
            )
            .child(div().px(px(9.)).pb(px(4.)).child(preview))
            .into_any_element()
    }

    // -----------------------------------------------------------------------
    // 通用控件
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn slider(
        &self,
        id: &'static str,
        title: &'static str,
        value: f32,
        min: f32,
        max: f32,
        step: f32,
        unit: &'static str,
        apply: fn(&mut MenuSettings, f32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const TRACK: f32 = 168.0;
        let ratio = ((value - min) / (max - min)).clamp(0.0, 1.0);
        let knob_x = (TRACK * ratio - 6.0).clamp(0.0, TRACK - 12.0);
        let cell = self.slider_cell(id);

        let mut track = div()
            .id(SharedString::from(format!("slider-{id}")))
            .relative()
            .w(px(TRACK))
            .h(px(20.))
            .flex_none()
            .cursor_pointer()
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(8.))
                    .w(px(TRACK))
                    .h(px(4.))
                    .rounded(px(2.))
                    .bg(theme::track()),
            )
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(8.))
                    .w(px(TRACK * ratio))
                    .h(px(4.))
                    .rounded(px(2.))
                    .bg(theme::accent()),
            )
            .child(
                div()
                    .absolute()
                    .left(px(knob_x))
                    .top(px(4.))
                    .size(px(12.))
                    .rounded(px(6.))
                    .bg(theme::surface_elevated())
                    .border_2()
                    .border_color(theme::accent()),
            );

        {
            let measure = cell.clone();
            let cell = cell.clone();
            track = track
                .child(
                    canvas(|_, _, _| {}, move |bounds, _, _, _| cell.set(Some(bounds)))
                        .absolute()
                        .left_0()
                        .top_0()
                        .size_full(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                        this.sliding = Some(Slide {
                            min,
                            max,
                            step,
                            apply,
                            bounds: measure.clone(),
                        });
                        this.slide_with(f32::from(event.position.x), cx);
                    }),
                )
                // 拖动时指针早就跑出这条轨道了，滚动区上那份 `on_mouse_move` 才是主力；
                // 这里再挂一份是**兜底**：万一指针压在轨道里但滚动区没收到（比如窗口
                // 边界夹住、或滚动区被别的元素挡住），靠这份也能继续跟手。
                .on_mouse_move(
                    cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                        if this.sliding.is_some() {
                            this.slide_with(f32::from(event.position.x), cx);
                        }
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                        this.end_slide(cx);
                    }),
                );
        }

        row_with_control(
            title,
            "",
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(track)
                .child(
                    div()
                        .w(px(52.))
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(theme::ui_text_secondary())
                        .child(format!("{value:.0}{unit}")),
                )
                .into_any_element(),
        )
    }
}

/// 把色号写进调色板那个输入框。
///
/// 必须**延到下一帧**再做：`TextInput::set_text` 里的 `notify` 会同步回调本视图
/// 注册的观察者，而调用点（拖色块 / 取色）正处在 `&mut self` 的更新里 ——
/// 观察者再借一次 self 就是重复借用，debug 下报 borrow 错误，release 下直接崩。
fn push_hex_to_input(input: &Entity<TextInput>, hex: String, cx: &mut App) {
    let input = input.clone();
    cx.defer(move |cx| {
        let _ = input.update(cx, |input, cx| input.set_text(hex, cx));
    });
}

/// HSV → `#rrggbb`（色板用，不依赖额外的颜色库）。
fn hsv_hex(h: f32, s: f32, v: f32) -> String {
    let c = v * s;
    let hp = (h % 360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |t: f32| ((t + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    let rgb = (byte(r) << 16) | (byte(g) << 8) | byte(b);
    theme::to_hex(rgb)
}

/// `#rrggbb` → (色相 0..360, 饱和度 0..1, 明度 0..1)。
fn hex_hsv(hex: &str) -> Option<(f32, f32, f32)> {
    let rgb = theme::parse_hex(hex)?;
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let hue = if hue < 0.0 { hue + 360.0 } else { hue };
    let sat = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    Some((hue, sat, max))
}

fn normalize(hex: &str) -> String {
    let text = hex.trim().trim_start_matches('#').to_ascii_lowercase();
    format!("#{text}")
}

fn candidates(dark: bool, field: Field) -> Vec<&'static str> {
    match field {
        Field::Surface => {
            if dark {
                theme::SURFACES_DARK.to_vec()
            } else {
                theme::SURFACES_LIGHT.to_vec()
            }
        }
        Field::Accent => theme::ACCENTS[..8].to_vec(),
        Field::Selected => theme::SELECTED_TINTS[..6].to_vec(),
        Field::Text => {
            if dark {
                vec!["#e9ecf2", "#ffffff", "#cfd6e4", "#aeb7c8"]
            } else {
                vec!["#1f2329", "#111318", "#3b4252", "#4b5563"]
            }
        }
        // 加速键：默认跟文字色，也可以挑个更跳的颜色把它区分出来
        Field::Accel => {
            if dark {
                vec!["#e9ecf2", "#8fb4ff", "#7ee2c4", "#ffd479", "#ff9f9a"]
            } else {
                vec!["#1f2329", "#2f6feb", "#0f9d76", "#b7791f", "#c62d33"]
            }
        }
        Field::Muted | Field::MutedUi => {
            if dark {
                vec!["#9aa4b2", "#8a93a8", "#b6becd", "#77808f"]
            } else {
                vec!["#6b7280", "#8a9099", "#9aa4b2", "#c9ced6"]
            }
        }
        // 边框：默认那档最不抢眼，往下依次更明显（也有人喜欢强调色描边）
        Field::Border => {
            if dark {
                vec!["#2f3642", "#3d4553", "#4e5768", "#6e9bff"]
            } else {
                vec!["#e8eaee", "#d8dce3", "#c2c8d2", "#2f6feb"]
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 无状态小组件
// ---------------------------------------------------------------------------


/// 一小段说明文字（一行 = 一条）。
///
/// 每条都包一层 `flex().flex_1().min_w_0()`：外层容器在 `scroll_area` 里已经被
/// 写死成确定的宽度，这里才能算出真实的行数。以前直接给文本 `w_full()`，
/// taffy 量「内容总高」时按不确定宽度量，长段落被量成几十行 ——
/// 卡片底部就多出一大段空白（行为页「按选中内容切换菜单」实测多 1300px）。
pub(crate) fn note_block(lines: &[&'static str]) -> AnyElement {
    let mut block = div()
        .min_w_0()
        .px(px(9.))
        .py(px(5.))
        .flex()
        .flex_col()
        .gap(px(3.));
    for line in lines {
        block = block.child(
            div().min_w_0().flex().child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(*line)),
            ),
        );
    }
    block.into_any_element()
}

pub(crate) fn section(
    title: &'static str,
    hint: &'static str,
    children: Vec<AnyElement>,
) -> AnyElement {
    let title = crate::i18n::t(title);
    let hint = crate::i18n::t(hint);
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(4.))
        .rounded(px(theme::RADIUS_CARD))
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_sunken())
        .p(px(7.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(3.))
                .pb(px(2.))
                .child(dot(false))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::ui_text_secondary())
                        .child(title),
                )
                .when(!hint.is_empty(), |header| {
                    header.child(
                        div()
                            .flex_1()
                            .text_size(px(10.))
                            .text_color(theme::ui_text_tertiary())
                            .truncate()
                            .child(hint),
                    )
                }),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .p(px(2.))
                .children(children),
        )
        .into_any_element()
}

/// 「菜单里的搜索框」分段按钮的选项。
fn submenu_anchor_options() -> Vec<(u32, &'static str)> {
    SubmenuAnchor::ALL
        .iter()
        .enumerate()
        .map(|(index, mode)| (index as u32, mode.label()))
        .collect()
}

fn width_mode_options() -> Vec<(u32, &'static str)> {
    PanelWidthMode::ALL
        .iter()
        .enumerate()
        .map(|(index, mode)| (index as u32, mode.label()))
        .collect()
}

fn search_place_options() -> Vec<(u32, &'static str)> {
    SearchPlace::ALL
        .iter()
        .enumerate()
        .map(|(index, place)| (index as u32, place.label()))
        .collect()
}

/// 「提示显示位置」分段按钮的选项。
fn anchor_options() -> Vec<(u32, &'static str)> {
    settings::HintAnchor::ALL
        .iter()
        .enumerate()
        .map(|(index, anchor)| (index as u32, anchor.label()))
        .collect()
}

/// 「对齐方式」分段按钮的选项（文案和枚举放在一处，别两边各写一份）。
fn align_options() -> Vec<(u32, &'static str)> {
    crate::tip_template::TipAlign::ALL
        .iter()
        .enumerate()
        .map(|(index, align)| (index as u32, align.label()))
        .collect()
}

/// 预览用的样例：挑一个「有备注、有动作」的真实菜单项，都没有就用内置的假数据。
fn tip_preview_context(settings: &MenuSettings) -> crate::tip_template::TipContext {
    fn walk<'a>(
        items: &'a [ItemSetting],
        trail: &str,
        best: &mut Option<(&'a ItemSetting, String, u32)>,
    ) {
        for item in items {
            if item.kind == ItemKind::Separator {
                continue;
            }
            let score = (item.actions.len().min(1) as u32) * 2
                + (!item.note.is_empty()) as u32
                + (!item.hotkey.is_empty()) as u32;
            if best.as_ref().map(|(_, _, best_score)| score > *best_score).unwrap_or(true) {
                *best = Some((item, trail.to_string(), score));
            }
            if !item.children.is_empty() {
                let label = item.label.trim();
                let next = if trail.is_empty() {
                    label.to_string()
                } else {
                    format!("{trail} › {label}")
                };
                walk(&item.children, &next, best);
            }
        }
    }

    let mut best = None;
    walk(&settings.items, "", &mut best);
    if let Some((item, trail, score)) = best {
        if score > 0 {
            return crate::tip_template::TipContext::of(item, &trail);
        }
    }

    // 一个能填满各项的假条目：用户还没配好玩意时预览也不至于空着
    let mut sample = ItemSetting::item("preview".to_string(), "记事本");
    sample.note = "常用的一个，随手记点东西".to_string();
    sample.accel = "N".to_string();
    sample.shortcut = "Ctrl+N".to_string();
    sample.hotkey = "Ctrl+Alt+N".to_string();
    sample.badge = "📝".to_string();
    sample.actions = vec![
        settings::ActionStep::new(settings::ActionKind::Run, "notepad.exe"),
        settings::ActionStep::new(settings::ActionKind::Keys, "Ctrl+S"),
    ];
    crate::tip_template::TipContext::of(&sample, "编辑")
}

/// 一行：左边标题（+ 说明），右边控件。
/// 一行占位符说明：`{名字}` + 用途。
fn var_row(
    name: &str,
    hint: &'static str,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    let hint = crate::i18n::t(hint);
    let token = format!("{{{name}}}");
    let copy_token = token.clone();
    let copy_message = crate::i18n::t("已复制占位符「{name}」").replace("{name}", &token);
    div()
        // 每行一颗「复制」：id 里带上占位符名，否则整页几十颗同名按钮
        // 会被 gpui 当成同一个交互元素，只有第一颗点得动
        .id(SharedString::from(format!("var-row-{name}")))
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(9.))
        .py(px(3.))
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .text_size(px(11.))
                .text_color(theme::accent())
                .child(token),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.))
                .text_color(theme::ui_text_tertiary())
                .child(hint.to_string()),
        )
        // 复制：写进剪贴板，方便粘到别的地方（动作参数、记事本、别的软件）
        .child(ghost_button(
            crate::i18n::t("复制"),
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_token.clone()));
                this.toast(copy_message.clone(), cx);
            }),
        ))
        .into_any_element()
}

/// 一套现成主题的卡片：名字 + 说明 + 两组色点（浅色底 / 强调 / 选中，深色同样三个）。
///
/// 点一下 = 整套换掉（浅色 + 深色两份一起）。当前正好是这一套时打勾并描边。
fn preset_card(
    preset: &theme::ThemePreset,
    active: bool,
    index: usize,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    let swatch = |surface: &str, accent: &str| {
        let palette = theme::Palette::build(&theme::ThemeColors::new(surface, accent));
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                div()
                    .size(px(10.))
                    .rounded(px(3.))
                    .bg(gpui::rgba(palette.surface))
                    .border_1()
                    .border_color(theme::border()),
            )
            .child(
                div()
                    .size(px(10.))
                    .rounded(px(3.))
                    .bg(gpui::rgba(palette.accent)),
            )
            .child(
                div()
                    .size(px(10.))
                    .rounded(px(3.))
                    .bg(gpui::rgba(palette.row_selected)),
            )
    };

    div()
        .id(SharedString::from(format!("preset-{index}")))
        .w(px(132.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(7.))
        .rounded(px(theme::RADIUS_ROW))
        .cursor_pointer()
        .bg(if active {
            theme::accent_soft()
        } else {
            theme::surface()
        })
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::border()
        })
        .hover(|style| style.border_color(theme::border_strong()))
        .on_click(cx.listener(move |_this, _: &ClickEvent, window, cx| {
            let preset = &theme::PRESETS[index];
            settings::update(cx, |settings| {
                preset.apply(&mut settings.light, &mut settings.dark);
            });
            window.refresh();
            cx.notify();
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(theme::text_primary())
                        .child(crate::i18n::t(preset.name)),
                )
                .when(active, |row| {
                    row.child(
                        div()
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(theme::accent())
                            .child("✓"),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(9.5))
                .text_color(theme::ui_text_tertiary())
                .truncate()
                .child(crate::i18n::t(preset.hint)),
        )
        .child(swatch(preset.light.0, preset.light.1))
        .child(swatch(preset.dark.0, preset.dark.1))
        .into_any_element()
}

/// 提示卡片的透明度滑条（`slider` 要函数指针，不能收闭包）。
fn set_hint_opacity(settings: &mut MenuSettings, value: f32) {
    let (lo, hi) = settings::HotstringSettings::limits_opacity();
    settings.hotstrings.hint_opacity = value.round().clamp(lo as f32, hi as f32) as u32;
}

/// 数字输入框 → 设置里的一个字段：夹到 `[min, max]`，非法输入不写回
/// （用户正在把 `6000` 改成 `60`，中间会经过空串，这时候别把设置写成 0）。
fn bind_number(
    input: &Entity<TextInput>,
    cx: &mut Context<SettingsView>,
    get: fn(&MenuSettings) -> i64,
    set: fn(&mut MenuSettings, i64),
    min: i64,
    max: i64,
) -> Subscription {
    let input = input.clone();
    cx.observe(&input, move |_this, input, cx| {
        let text = input.read(cx).text();
        let Ok(value) = text.trim().parse::<i64>() else {
            return;
        };
        let value = value.clamp(min, max);
        if get(&settings::current()) == value {
            return;
        }
        settings::update(cx, |settings| set(settings, value));
    })
}

/// 数字输入框外面那层「小方框」（宽高、圆角、边框统一）。
pub(crate) fn number_box(input: &Entity<TextInput>, width: f32) -> AnyElement {
    div()
        .w(px(width))
        .h(px(24.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(theme::surface_sunken())
        .border_1()
        .border_color(theme::border_strong())
        .child(input.clone())
        .into_any_element()
}

pub(crate) fn row_with_control(
    title: &'static str,
    hint: &'static str,
    control: AnyElement,
) -> AnyElement {
    let title = crate::i18n::t(title);
    let hint = crate::i18n::t(hint);
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(9.))
        .py(px(4.))
        .rounded(px(theme::RADIUS_ROW))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme::text_primary())
                        .child(title),
                )
                .when(!hint.is_empty(), |column| {
                    column.child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(hint),
                    )
                }),
        )
        .child(control)
        .into_any_element()
}

fn toggle_row(
    title: &'static str,
    hint: &'static str,
    value: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let title = crate::i18n::t(title);
    let hint = crate::i18n::t(hint);
    div()
        .id(SharedString::from(format!("toggle-row-{title}")))
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(9.))
        .py(px(5.))
        .rounded(px(theme::RADIUS_ROW))
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(on_click)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme::text_primary())
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(hint),
                ),
        )
        .child(switch(value))
        .into_any_element()
}

/// 行尾的小图标按钮（✎ / ⧉ / 👁 / ✕）。
///
/// 和「菜单项」页的行内按钮一个味道：紧凑、悬停才亮。`ElementId` 带业务 id，
/// 不然列表里几十颗会撞成同一个元素（点击只认第一颗）。
fn icon_action(
    id: String,
    icon: &'static str,
    _tip: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    // 排版/配色照抄菜单项页的行内按钮（20px 方块、10px、三级字色、悬停提亮）
    div()
        .id(SharedString::from(format!("icon-{id}")))
        .flex_none()
        .size(px(20.))
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
        .child(
            gpui::svg()
                .path(icon)
                .size(px(12.))
                .flex_none()
                .text_color(theme::ui_text_secondary()),
        )
        .on_click(move |event, window, cx| on_click(event, window, cx))
        .into_any_element()
}

pub(crate) fn segmented(
    id: &'static str,
    options: &[(u32, &'static str)],
    value: u32,
    on_pick: impl Fn(&u32, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_pick = Rc::new(on_pick);
    let mut group = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(theme::RADIUS_SMALL))
        .bg(theme::track());
    for (option_value, label) in options {
        let label = crate::i18n::t(label);
        let selected = *option_value == value;
        let option_value = *option_value;
        let on_pick = on_pick.clone();
        group = group.child(
            div()
                .id(SharedString::from(format!("seg-{id}-{option_value}")))
                .px(px(10.))
                .py(px(2.))
                .rounded(px(4.))
                .text_size(px(11.))
                .cursor_pointer()
                .when(selected, |style| {
                    style
                        .bg(theme::accent())
                        .text_color(theme::text_on_accent())
                })
                .when(!selected, |style| {
                    style.text_color(theme::ui_text_secondary())
                })
                .on_click(move |_, window, cx| on_pick(&option_value, window, cx))
                .child(label.to_string()),
        );
    }
    group.into_any_element()
}

/// 贴边缩放热区不用了：窗口带 `WS_THICKFRAME`，四边四角由系统直接接管
/// （见 `chrome` 模块顶部的说明）。
pub(crate) fn primary_button(
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    primary_button_if(label, true, on_click)
}

/// 主按钮。`enabled = false` 时置灰、不响应点击（「应用」在没有改动时就是这样）。
pub(crate) fn primary_button_if(
    label: &'static str,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let label = crate::i18n::t(label);
    let mut button = div()
        .id(SharedString::from(format!("primary-{label}")))
        .h(px(30.))
        .px(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .text_size(px(12.5))
        .child(label);
    if enabled {
        button = button
            .bg(theme::accent())
            .text_color(theme::text_on_accent())
            .cursor_pointer()
            .hover(|style| style.opacity(0.88))
            .on_click(on_click);
    } else {
        button = button
            .bg(theme::track())
            .text_color(theme::ui_text_tertiary())
            .border_1()
            .border_color(theme::border());
    }
    button.into_any_element()
}

/// 把「Unix 秒」说成「多久以前」——不碰时区，也不依赖额外依赖。
fn format_stamp(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(secs);
    // 相对时间没法整句查表（分钟 / 小时 / 天是三个量词），按语言分叉
    if crate::i18n::is_english() {
        const MINUTE: u64 = 60;
        const HOUR: u64 = 60 * MINUTE;
        const DAY: u64 = 24 * HOUR;
        return match delta {
            0..=90 => "just now".to_string(),
            91..=3600 => format!("{} min ago", delta / MINUTE),
            3601..=86_400 => format!("{} h ago", delta / HOUR),
            _ => format!("{} d ago", delta / DAY),
        };
    }
    match delta {
        0..=90 => "刚刚".to_string(),
        91..=3600 => format!("{} 分钟前", delta / 60),
        3601..=86_400 => format!("{} 小时前", delta / 3600),
        _ => format!("{} 天前", delta / 86_400),
    }
}

pub(crate) fn ghost_button(
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    ghost_button_if(label, true, on_click)
}

/// 次按钮，`id` 里带一个 `tag`。
///
/// gpui 把交互状态挂在 `ElementId` 上，而 `ElementId` 是「文案」拼出来的 ——
/// 同一页里几十颗「复制」「添加到菜单」就会撞成同一个元素：鼠标按下时被**第一颗**
/// 的监听器吃掉 `pending_mouse_down`，于是后面的按钮全都点不动。
/// 列表里 / 循环里的按钮一律用这个带 tag 的版本（tag 用行的 id、序号这类
/// **跨帧稳定**的值；别用递增计数器，那样 id 每帧都变，点击反而永远不成立）。
pub(crate) fn ghost_button_tagged(
    tag: impl AsRef<str>,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    ghost_button_tagged_if(tag, label, true, on_click)
}

/// 带 tag 的次按钮，`enabled = false` 时置灰、不响应点击。
pub(crate) fn ghost_button_tagged_if(
    tag: impl AsRef<str>,
    label: &'static str,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let tag = tag.as_ref().to_string();
    ghost_button_with(
        format!("ghost-{tag}-{}", crate::i18n::t(label)),
        label,
        enabled,
        on_click,
    )
}

/// 次按钮。`enabled = false` 时置灰、不响应点击（「撤销」在没有改动时就是这样）。
pub(crate) fn ghost_button_if(
    label: &'static str,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    ghost_button_with(
        format!("ghost-{}", crate::i18n::t(label)),
        label,
        enabled,
        on_click,
    )
}

fn ghost_button_with(
    id: String,
    label: &'static str,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let label = crate::i18n::t(label);
    let mut button = div()
        .id(SharedString::from(id))
        .h(px(28.))
        // 英文按钮文案比中文长，`flex_none` 免得被同一行的其他元素挤掉半个字母
        // （「Reset menu」曾经显示成「Rest menu」）
        .flex_none()
        .px(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .text_size(px(11.5))
        .child(label);
    if enabled {
        button = button
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border_strong())
            .text_color(theme::text_primary())
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(on_click);
    } else {
        button = button
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .text_color(theme::ui_text_tertiary());
    }
    button.into_any_element()
}

/// 键帽（文案可以是运行时拼出来的，比如用户自己设的快捷键）。
pub(crate) fn key_cap_dynamic(label: SharedString) -> AnyElement {
    div()
        .min_w(px(24.))
        .h(px(22.))
        .px(px(7.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .bg(theme::surface_elevated())
        .border_1()
        .border_color(theme::border_strong())
        .text_size(px(11.))
        .text_color(theme::text_primary())
        .child(label)
        .into_any_element()
}

pub(crate) fn info_row(label: &'static str, value: impl Into<String>) -> AnyElement {
    let label = crate::i18n::t(label);
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(9.))
        .py(px(4.))
        .child(
            div()
                .w(px(124.))
                .flex_none()
                .text_size(px(11.5))
                .text_color(theme::ui_text_secondary())
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .text_color(theme::ui_text_tertiary())
                .child(value.into()),
        )
        .into_any_element()
}

/// 说明行：标签在上、正文在下（正文自己折行，长句子不会被截成一条横线）。
pub(crate) fn note_row(label: &'static str, value: String) -> AnyElement {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(9.))
        .py(px(5.))
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme::ui_text_secondary())
                .child(crate::i18n::t(label)),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_size(px(11.))
                .text_color(theme::ui_text_tertiary())
                .child(value),
        )
        .into_any_element()
}

pub(crate) fn dot(active: bool) -> AnyElement {
    div()
        .size(px(6.))
        .flex_none()
        .rounded(px(3.))
        .bg(if active {
            theme::accent()
        } else {
            theme::ui_text_tertiary()
        })
        .into_any_element()
}

pub(crate) fn switch(value: bool) -> AnyElement {
    div()
        .w(px(34.))
        .h(px(20.))
        .flex_none()
        .rounded(px(10.))
        .bg(if value {
            theme::accent()
        } else {
            theme::track()
        })
        .relative()
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(px(if value { 16.0 } else { 2.0 }))
                .size(px(16.))
                .rounded(px(8.))
                .bg(rgba(0xffffffff)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{hex_hsv, hsv_hex};

    #[test]
    fn hsv_conversion_matches_known_colors() {
        assert_eq!(hsv_hex(0.0, 0.0, 1.0), "#ffffff");
        assert_eq!(hsv_hex(0.0, 0.0, 0.0), "#000000");
        assert_eq!(hsv_hex(0.0, 1.0, 1.0), "#ff0000");
        assert_eq!(hsv_hex(120.0, 1.0, 1.0), "#00ff00");
        assert_eq!(hsv_hex(240.0, 1.0, 1.0), "#0000ff");
    }

    #[test]
    fn hex_round_trips_through_hsv() {
        // 调色板靠 hex → HSV 反算初始位置，所以这两者必须互为逆运算
        for color in [
            "#2f6feb", "#e8eef6", "#22c55e", "#ff8800", "#123456", "#ffffff", "#000000",
        ] {
            let (h, s, v) = hex_hsv(color).expect("能解析");
            assert_eq!(hsv_hex(h, s, v), color, "{color} 转一圈回不来了");
        }
    }

    #[test]
    fn gray_has_no_hue() {
        let (h, s, _v) = hex_hsv("#808080").unwrap();
        assert_eq!(h, 0.0);
        assert_eq!(s, 0.0);
        assert!(hex_hsv("不是颜色").is_none());
    }
}
