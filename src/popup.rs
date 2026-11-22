//! 菜单会话 + 窗口池。
//!
//! ## 为什么要有「窗口池」
//!
//! GPUI 的 `App::open_window` 在 Windows 上会给新窗口新建一整套 DirectX 渲染器
//! （建管线 + 建 swapchain），实测一次 **约 380ms**。级联菜单每一级都要一个窗口，
//! 如果每次弹菜单都现建，手感和卡顿都会很难受。
//!
//! 所以这里启动时就把「每一层一个窗口」预创建好、`SW_HIDE` 藏起来。
//! 之后打开菜单只做「移动 + 改尺寸 + 显示」，实测 12ms 出菜单。
//!
//! ## 窗口位置
//!
//! - 第 0 列客户区左上角 = 光标位置（只有第一列都放不下时才夹）；
//! - 第 L 列 = 第 L-1 列里「被高亮那一行」的左上角（父列滚动偏移要算进去）；
//! - 右侧放不下就**翻到父列左边**（和系统菜单一样），不会跑出屏幕；
//! - 高度按「从列顶到屏幕底还剩多少」夹，夹出来的部分变成列内滚动（见 `column.rs`）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    point, px, size, AnyWindowHandle, App, AppContext, BorrowAppContext, Bounds, Entity, Global,
    Focusable, KeystrokeEvent, Pixels, Point, SharedString, Subscription, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};

use crate::column::ColumnView;
use crate::cursor;
use crate::host::AppStateHandle;
use crate::hotstring::Replay;
use crate::layout::{self, FRAME_INSET};
use crate::settings::{self, ActionKind, ActionStep, BuiltinAction};
use crate::state::MenuState;
use crate::win32;

/// 失焦（点到别的窗口）时是否自动收起
const CLOSE_ON_FOCUS_LOST: bool = true;
/// 显示/藏完窗口后多久之内不算「用户点走了」（藏窗口会引起系统级的焦点抖动）
const FOCUS_GRACE: Duration = Duration::from_millis(200);
/// 池里窗口的初始高度（真正显示时会 resize）
const INITIAL_HEIGHT: f32 = 180.0;
/// 能不能移动窗口（决定用「池化 + 移动」还是「每次重建」）
const CAN_MOVE_WINDOWS: bool = cfg!(windows);
/// 菜单关掉以后，静置多久就把窗口池收掉。
///
/// 一个窗口的交换链 + MSAA 中间纹理 + 图集都是按窗口分的 GPU 缓冲，
/// 菜单用完这些窗口只是藏起来，内存却一直占着 —— 而后台挂着的时候它们
/// 一点用都没有。25 秒是个折中：连开两三次菜单不会来回建，
/// 真正放下不用了就把几十兆还给系统（下次弹菜单现建一列 16 ms 量级）。
const IDLE_SHRINK: Duration = Duration::from_secs(25);

/// 打开耗时打点：`GPUI_MENU_TIMING=1` 时在控制台打一行各阶段毫秒数。
pub(crate) fn timing_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("GPUI_MENU_TIMING").is_some())
}

macro_rules! timing {
    ($t0:expr, $($arg:tt)*) => {
        if crate::popup::timing_enabled() {
            println!("[timing] +{:>4}ms  {}", $t0.elapsed().as_millis(), format!($($arg)*));
        }
    };
}
pub(crate) use timing;

struct Slot {
    handle: AnyWindowHandle,
    hwnd: Option<isize>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    on_screen: bool,
    /// 上一次把它画出来时菜单树是哪一版（`MenuState::revision`）。
    /// 内容换了但窗口尺寸恰好没变时（比如「显示全部菜单」换出来的树刚好一样高），
    /// 光看尺寸不会重画 —— 屏幕上就会留着旧内容。
    revision: u64,
}

struct Live {
    /// 第 0 列客户区左上角（逻辑像素，屏幕坐标）
    origin: Point<Pixels>,
    screen: Bounds<Pixels>,
    scale: f32,
    /// 打开菜单前的前台窗口，关闭时把焦点还给它
    previous_foreground: isize,
    /// 每一列当前的卡片高度（键盘翻页时算可见区间要用）
    heights: Vec<f32>,
    _keys: Subscription,
}

pub struct MenuHost {
    state: Entity<MenuState>,
    /// 菜单顶上那条搜索框（真的输入框，支持中文输入法 / 点击定位光标）
    pub(crate) search: Entity<crate::text_input::TextInput>,
    slots: Vec<Slot>,
    live: Option<Live>,
    /// 「保持显示」：开着的时候点到别的窗口也不收起（调外观时用）。
    ///
    /// 只存在内存里、不写盘 —— 它是个临时调试状态，不是配置。
    pinned: bool,
    /// 上一次「显示/隐藏窗口」的时间。
    ///
    /// 藏一个正拿着前台的窗口会让 Windows 自己去挑下一个前台（很可能是别的程序），
    /// 这一瞬间的失焦不是用户点走的，watchdog 得放过它。
    grace: Option<std::time::Instant>,
    /// 尺寸对账的重试次数：`window.resize()` 是排进 executor 的异步请求，偶尔会被
    /// 吞掉。读回真实尺寸还对不上就再 sync 一次，这里记着已经重试过几次。
    sync_retry: u8,
}

impl Global for MenuHost {}

impl MenuHost {
    /// 菜单现在是不是开着（设置窗口里的「预览」按钮要用）。
    pub fn is_open(&self) -> bool {
        self.live.is_some()
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned
    }
}

/// 菜单开着吗（设置界面读它决定按钮文案）。
pub fn is_open(cx: &App) -> bool {
    cx.try_global::<MenuHost>()
        .map(|host| host.is_open())
        .unwrap_or(false)
}

/// 「保持显示」当前是开还是关。
pub fn is_pinned(cx: &App) -> bool {
    cx.try_global::<MenuHost>()
        .map(|host| host.is_pinned())
        .unwrap_or(false)
}

/// 不透明度改了：立刻套到所有菜单窗口上（拖滑块时能当场看到变化）。
pub fn apply_opacity(cx: &mut App) {
    let opacity = layout::menu_opacity();
    let Some(host) = cx.try_global::<MenuHost>() else {
        return;
    };
    let handles: Vec<isize> = host.slots.iter().filter_map(|slot| slot.hwnd).collect();
    for hwnd in handles {
        win32::set_window_opacity(hwnd, opacity);
    }
}

/// 切换「保持显示」。开着菜单时切到「保持」会立刻重新盯一次焦点。
pub fn set_pinned(pinned: bool, cx: &mut App) {
    cx.update_global(|host: &mut MenuHost, _app: &mut App| {
        host.pinned = pinned;
    });
    if pinned {
        // 刚打开时可能已经因为失焦被收掉了，这里不主动开菜单，只保证下一次不看焦点
        return;
    }
    // 关掉「保持」后如果菜单还开着、而焦点已经不在菜单里，就按正常规则收起来
    if cx
        .try_global::<MenuHost>()
        .map(|host| host.live.is_some())
        .unwrap_or(false)
    {
        let foreground = win32::foreground_window();
        let inside = cx
            .try_global::<MenuHost>()
            .map(|host| {
                host.slots
                    .iter()
                    .filter(|slot| slot.on_screen)
                    .any(|slot| slot.hwnd == Some(foreground))
            })
            .unwrap_or(false);
        if !inside {
            close_all(cx);
        }
    }
}

// ---------------------------------------------------------------------------
// 初始化 / 预热
// ---------------------------------------------------------------------------

/// 启动时调用一次：建好状态实体，并把窗口池预热起来。
pub fn init(cx: &mut App) {
    let state = cx.new(|_| crate::state::example());
    // 搜索框：一个真正的输入框实体。打字（含中文输入法）由它接，
    // 它一变就把词写进 `MenuState::query` 重新筛菜单。
    let search = cx.new(|cx| {
        crate::text_input::TextInput::new("", crate::i18n::t("搜索菜单项…"), 11.0, cx)
    });
    {
        let state = state.clone();
        cx.observe(&search, move |input, cx| {
            let text = input.read(cx).text();
            let changed = state.update(cx, |state, cx| {
                if state.query() == text {
                    return false;
                }
                state.set_query(text);
                cx.notify();
                true
            });
            if changed {
                sync(cx);
            }
        })
        .detach();
    }
    cx.set_global(MenuHost {
        state,
        search,
        slots: Vec::new(),
        live: None,
        pinned: false,
        grace: None,
        sync_retry: 0,
    });
    // **不再预建菜单窗口**：一个空窗口就是 25~39 MB（swapchain + 图集 + 一堆
    // GPU 缓冲，GPUI 按窗口分资源），后台常驻时这是最大的一块开销 ——
    // 预建 3 列 136.6 MB 私有内存，一列都不建只有 58.8 MB。
    //
    // 不预建也照样秒开：真正贵的是「渲染管线第一次建」（两三百毫秒），而这一步
    // 已经被悬停提示窗口在启动 400ms 后那次全透明试画点着了（见 `tooltip`）。
    // 至于菜单列本身，现建一列 16 ms 量级，弹一次菜单的实测总耗时约 215 ms，
    // 其中绝大部分花在抢前台和抓选中内容上，跟建不建窗口没关系。
}

// ---------------------------------------------------------------------------
// 打开 / 关闭
// ---------------------------------------------------------------------------

/// 这次弹出的是哪一种菜单。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    /// 完整菜单（`filter` = 要不要按选中内容筛，`capture` = 弹出前要不要抓选中内容）
    All { filter: bool, capture: bool },
    /// 只弹某一个子菜单（菜单项 / 子菜单自己的触发键）
    Submenu(String),
    /// 托盘图标右键弹的菜单
    Tray,
    /// 右键某一个菜单项时的快捷操作
    ItemContext(String),
    /// 「把选中内容加到这一项后面」：第二问（完整路径 / 只填文件名）
    AddSelection(String),
}

/// 菜单原点：光标位置，按屏幕边界夹一下。
///
/// x 只按「第一列的实际宽度」夹（菜单要出现在鼠标处，展开时再向右/向左翻）；
/// y 至少留出一行的高度，剩下的交给 `column_targets` 按屏幕底部夹。
fn anchor_origin(cursor_pos: Point<Pixels>, screen: Bounds<Pixels>, first_width: f32) -> Point<Pixels> {
    let max_x = screen.origin.x + screen.size.width - px(first_width.clamp(80.0, 640.0));
    let min_height = layout::min_column_height(0);
    let max_y = screen.origin.y + screen.size.height - px(layout::SCREEN_MARGIN + min_height);
    point(
        cursor_pos.x.min(max_x).max(screen.origin.x),
        cursor_pos.y.min(max_y).max(screen.origin.y),
    )
}

/// 第一列（或某个菜单树根）的实际宽度：自动列宽时得量一遍才知道。
fn root_width(cx: &App, entries: &[crate::menu::MenuEntry]) -> f32 {
    let family = crate::theme::font_family();
    let system: &gpui::TextSystem = cx.text_system();
    let widths = layout::column_widths_with(entries, &|text, size| {
        layout::measure_text(system, &family, size, text)
    });
    layout::column_span(&widths)
}

pub fn toggle(cx: &mut App) {
    if cx.global::<MenuHost>().live.is_some() {
        close_all(cx);
        return;
    }
    // 「禁用应用」名单里的程序在前面：快捷键不该抢，直接不弹
    if crate::settings::foreground_blocked() {
        return;
    }
    open(cx);
}

pub fn open(cx: &mut App) {
    open_with(cx, Source::All { filter: true, capture: true });
}

/// 同一份菜单，但**不抓**选中内容（`popup_hotkey_noselect` 那一颗）。
///
/// 为什么要有它：抓取是一次「注入 Ctrl+C」，在终端里是中断、在全屏游戏里会卡一下、
/// 还会把剪贴板翻一遍。有些时候用户只想把菜单叫出来，压根不要 `{selection}`。
pub fn toggle_no_select(cx: &mut App) {
    if cx.global::<MenuHost>().live.is_some() {
        close_all(cx);
        return;
    }
    if crate::settings::foreground_blocked() {
        return;
    }
    open_with(cx, Source::All { filter: false, capture: false });
}

thread_local! {
    /// 这一次弹出**别再去抓一次**：内容已经在 `selection::current()` 里了。
    ///
    /// 一键直达先抓一遍用来匹配规则，没命中才弹菜单 —— 那时候再抓一次不光白花
    /// 半秒（注入 Ctrl+C + 等剪贴板），还会把用户的剪贴板翻来翻去。
    static KEEP_SELECTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 弹完整菜单，但**复用**刚抓到的那份选中内容（不再复制一次）。
///
/// 一键直达「一条规则都没命中」时用它兜底：用户按了键总得有点反应。
pub fn open_keeping_selection(cx: &mut App) {
    if cx.global::<MenuHost>().live.is_some() {
        return;
    }
    if crate::settings::foreground_blocked() {
        return;
    }
    KEEP_SELECTION.with(|flag| flag.set(true));
    open_with(cx, Source::All { filter: true, capture: false });
}

/// 只弹某一支子菜单，同样复用已经抓到的那份内容。
pub fn open_submenu_keeping_selection(cx: &mut App, id: &str) {
    if cx.global::<MenuHost>().live.is_some() {
        return;
    }
    if crate::settings::foreground_blocked() {
        return;
    }
    KEEP_SELECTION.with(|flag| flag.set(true));
    open_with(cx, Source::Submenu(id.to_string()));
}

/// 只弹某一个子菜单（菜单项 / 子菜单自己的快捷键用它）。
///
/// 树根就换成这个子菜单里的那几项 —— 按快捷键的人要的是「这一支的项」，
/// 直接摊开摆着，不用再移上去展开一次。尾巴上会有一条「显示全部菜单」。
pub fn open_submenu(cx: &mut App, id: &str) {
    open_with(cx, Source::Submenu(id.to_string()));
}

/// 调试用：菜单已经弹出来之后，按「名称路径」展开到某一层。
///
/// `GPUI_MENU_DEBUG_OPEN=工具,截图` → 第一列选中「工具」、第二列选中「截图」，
/// 正好能看到「多列父卡片 + 子菜单」这种最容易错位的组合。
pub fn open_path(cx: &mut App, labels: &[String]) {
    if labels.is_empty() {
        return;
    }
    let state = cx.global::<MenuHost>().state.clone();
    let found = state.update(cx, |state, cx| {
        let mut path: Vec<usize> = Vec::new();
        let mut list: &[crate::menu::MenuEntry] = &state.menu;
        for label in labels {
            let Some(index) = list.iter().position(|entry| {
                entry
                    .item()
                    .map(|item| item.label.as_str() == label.as_str())
                    .unwrap_or(false)
            }) else {
                return false;
            };
            path.push(index);
            list = list
                .get(index)
                .and_then(|entry| entry.item())
                .map(|item| item.children.as_slice())
                .unwrap_or(&[]);
        }
        if path.len() < 2 {
            return false;
        }
        state.path = path;
        state.scroll = vec![0.0; state.path.len()];
        cx.notify();
        true
    });
    if found {
        sync(cx);
    }
}

/// 托盘图标右键：弹托盘菜单（和弹出菜单同一套渲染，菜单项也是同一棵树里的）。
pub fn open_tray(cx: &mut App) {
    open_with(cx, Source::Tray);
}

/// 右键一个菜单项：弹「编辑 / 删除 / 复制名称」这几条。
///
/// 菜单开着的时候**不关再开**，只把树换掉（顺便把原点挪到光标处），
/// 否则会闪一下、还会把焦点还给别的程序。
pub fn open_item_context(cx: &mut App, id: &str) {
    let entries = crate::settings::item_context_menu(id);
    if entries.is_empty() {
        return;
    }
    if cx.global::<MenuHost>().live.is_some() {
        let (cursor_pos, screen, scale) = cursor::anchor(cx);
        let width = root_width(cx, &entries);
        let origin = anchor_origin(cursor_pos, screen, width);
        cx.update_global(|host: &mut MenuHost, _app: &mut App| {
            if let Some(live) = host.live.as_mut() {
                live.origin = origin;
                live.screen = screen;
                live.scale = scale;
            }
        });
        let state = cx.global::<MenuHost>().state.clone();
        state.update(cx, |state, cx| {
            state.replace(entries);
            cx.notify();
        });
        sync(cx);
        return;
    }
    open_with(cx, Source::ItemContext(id.to_string()));
}

fn open_with(cx: &mut App, source: Source) {
    if cx.global::<MenuHost>().live.is_some() {
        return;
    }
    let t0 = std::time::Instant::now();
    // 「跟随系统」模式下，每次弹出前问一次系统明暗
    settings::set_system_dark(matches!(
        cx.window_appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    ));
    timing!(t0, "开始（{source:?}）");

    // 1) 先抓「当前选中的内容」。
    //
    // 必须赶在打开任何自己的窗口之前：一旦我们的窗口拿到前台，注入的 Ctrl+C 就发到
    // 自己身上了。抓到的东西会成为菜单第一项，也是 `{selection}` 的值。
    //
    // 托盘 / 右键菜单**不抓**：那时候用户是在点我们的图标，往别的窗口发 Ctrl+C
    // 反而是打扰（还会把用户的剪贴板搅一遍）。
    let previous_foreground = win32::foreground_window();
    // 一键直达已经抓过一遍：这一次直接用那份内容
    let keep = KEEP_SELECTION.with(|flag| flag.replace(false));
    if matches!(source, Source::All { capture: true, .. } | Source::Submenu(_)) && !keep {
        crate::selection::capture();
        timing!(t0, "抓选中内容完成");
    } else if matches!(source, Source::All { capture: false, .. }) && !keep {
        // 「不抓选中内容」那一颗：把上一次抓到的清掉，否则 `{selection}` 里
        // 会躺着上一次的旧内容，用户以为这次也抓到了
        crate::selection::clear();
    }

    // 1.5) 「实时生成」的子菜单：在呼出这一刻按文件夹内容重建（会清空）。
    //
    // 放在建状态之前：清空 + 重填会改设置，`rebuild_with` 随后读到的就是新内容。
    // 只跑 Live 那些，手动/仅启动时的不碰（前者等用户点，后者启动时已经跑过）。
    match &source {
        Source::All { .. } | Source::Tray => {
            // 只放运行时缓存，不写配置：写配置会让每次呼出都改一次 settings.json
            // （文件一直变、设置页那棵树跟着重建 → 行在闪）
            let n = crate::settings::refresh_runtime(crate::settings::AutoFillMode::Live, None, false);
            if n > 0 {
                timing!(t0, "实时生成重建了 {n} 项");
            }
        }
        Source::Submenu(id) => {
            // 只弹这一支：强制刷（用户就是冲着它按的键）
            crate::settings::refresh_runtime(crate::settings::AutoFillMode::Live, Some(id), true);
        }
        _ => {}
    }

    // 2) 重建菜单状态（设置改过、抓到的内容也变了，所以要无条件重建）
    //
    // `Submenu` 是「只弹这一支」：按子菜单的触发快捷键，用户要的就是这一支里的项，
    // 直接摊开摆着 —— 不是整张菜单（还得重新找一次那个子菜单在哪），
    // 也不是「只弹一个名字、移到上面才展开」。
    // 代价是它「不是完整菜单」，所以尾巴上补一条「显示全部菜单」当出口。
    let scoped = match &source {
        Source::All { .. } => None,
        Source::Submenu(id) => Some(crate::settings::submenu_entries(id)),
        Source::Tray => Some(crate::settings::tray_menu()),
        Source::ItemContext(id) => Some(crate::settings::item_context_menu(id)),
        Source::AddSelection(id) => Some(add_selection_entries(id)),
    };
    let scope_kind = match &source {
        Source::Submenu(_) => "这一支",
        Source::Tray => "托盘菜单",
        Source::ItemContext(_) => "快捷操作",
        Source::AddSelection(_) => "加入选中内容",
        Source::All { .. } => "",
    };
    let filter = matches!(source, Source::All { filter: true, .. });
    let state = cx.global::<MenuHost>().state.clone();
    let empty = state.update(cx, |state, cx| {
        match scoped {
            Some(entries) => {
                if entries.is_empty() {
                    return true;
                }
                state.replace(entries);
            }
            None => {
                // 完整菜单：不补「显示全部菜单」（它就是全部）
                state.rebuild_with(filter, true);
                state.reset();
            }
        }
        cx.notify();
        state.entries(0).is_empty()
    });
    if empty {
        // 菜单项全被关掉 / 删光了：弹一张空卡片没意义，提示一句就算了
        if !scope_kind.is_empty() {
            println!("[popup-menu] {scope_kind}里没有可显示的项");
        } else {
            println!("[popup-menu] 菜单里没有可显示的项，去设置里加一条");
        }
        return;
    }
    timing!(t0, "重建菜单树完成");

    // 3) 算锚点（记住刚才那个前台窗口，关菜单时把焦点还给它）
    let (cursor_pos, screen, scale) = cursor::anchor(cx);
    // 第一列可能比设置的固定列宽宽/窄（自动列宽），x 要按它的实际宽度夹
    let first_width = {
        let snapshot = state.read(cx);
        root_width(cx, snapshot.entries(0))
    };
    let origin = anchor_origin(cursor_pos, screen, first_width);
    timing!(
        t0,
        "光标=({}, {}) 屏幕=({}, {} {}x{}) 原点=({}, {}) scale={scale}",
        f32::from(cursor_pos.x),
        f32::from(cursor_pos.y),
        f32::from(screen.origin.x),
        f32::from(screen.origin.y),
        f32::from(screen.size.width),
        f32::from(screen.size.height),
        f32::from(origin.x),
        f32::from(origin.y)
    );

    // 3) 键盘拦截（App 级，不依赖 FocusHandle）
    let keys = cx.intercept_keystrokes(|event, window, cx| on_key(event, window, cx));

    cx.update_global(|host: &mut MenuHost, _app: &mut App| {
        host.live = Some(Live {
            origin,
            screen,
            scale,
            previous_foreground,
            heights: Vec::new(),
            _keys: keys,
        });
    });

    // 4) 兜底：预热还没跑完就先补建
    let needed = layout::max_columns(&state.read(cx).menu);
    for level in cx.global::<MenuHost>().slots.len()..needed {
        create_slot(cx, state.clone(), level, scale, None);
    }
    timing!(t0, "窗口池就绪（需要 {needed} 列）");

    // 5) 摆窗口 + 抢焦点（键盘导航需要焦点在菜单窗口上）
    cx.activate(true);
    timing!(t0, "activate 完成");
    // 这里不要顺带抢前台：下面几步马上就做了，白抢一次要多花两百毫秒
    sync_with(cx, false);
    timing!(t0, "sync 完成");
    // 如果菜单是从我们自己的窗口（设置 / 编辑）里打开的：菜单抢了前台之后，
    // Windows 会把那个窗口顺便沉到别的程序底下。弹出前把它抬回普通窗口最前
    // （菜单本身是 TOPMOST，仍然压在上面），关掉菜单后它还得在原位。
    if previous_foreground != 0
        && win32::window_process_id(previous_foreground) == std::process::id()
    {
        win32::raise_above(previous_foreground);
    }
    timing!(t0, "摆窗口完成");
    // GPUI 侧的激活 + 系统侧兜底各来一次：热键刚按下时我们持有"最近输入"权限，
    // 这个时机 SetForegroundWindow 不会被系统拒绝。
    //
    // **千万别再调 `window.activate_window()`**：它会把 GPUI 记着的那套「初始
    // bounds」再 `SetWindowPos` 一遍（创建时的 0,0 / 预热高度），刚摆好的第一列
    // 就被拽回屏幕左上角、高度也变回预热那一版 —— 实测每次必现。
    // `cx.activate(true)` + `set_foreground` 已经够了：键盘事件照样进得来
    // （Esc 收一层、↑↓ 换行都是好的）。
    if let Some(hwnd) = cx
        .global::<MenuHost>()
        .slots
        .first()
        .and_then(|slot| slot.hwnd)
    {
        win32::set_foreground(hwnd);
    }
    // 收尾再钉一次位置：`set_foreground` 走的是系统消息，GPUI 那边可能还会补一帧
    // 布局（顺带把它记着的原点用上）。两次 SetWindowPos 一两毫秒，比「偶尔弹到
    // 屏幕左上角」强。
    reassert_positions(cx);
    timing!(t0, "钉位置完成");
    // 6) 搜索框：每次打开都清空，并把焦点直接给它 ——
    //    用户弹菜单就是为了打字，不该还得先点一下输入框（点不到就等于「输入不了」）。
    if layout::search_place().enabled() {
        let input = cx.global::<MenuHost>().search.clone();
        input.update(cx, |input, cx| input.set_text("", cx));
        if let Some(handle) = cx.global::<MenuHost>().slots.first().map(|slot| slot.handle) {
            let _ = handle.update(cx, |_, window, cx| {
                let focus = input.read(cx).focus_handle(cx);
                window.focus(&focus);
            });
        }
    }
    // 调试用：`GPUI_MENU_DEBUG_QUERY=新建` 弹出来就把这串字打进搜索框
    // （这台机器上注入按键会被吞，验「搜索 + 筛选 + 版式」只能这么来）
    if let Ok(text) = std::env::var("GPUI_MENU_DEBUG_QUERY") {
        let input = cx.global::<MenuHost>().search.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let _ = cx.update(|cx| {
                input.update(cx, |input, cx| input.set_text(text, cx));
            });
        })
        .detach();
    }
    spawn_watchdog(cx);
    spawn_selftest(cx);
    spawn_key_script(cx);
}

/// 按 `Slot` 里记着的目标位置再搬一遍（见 `open_with` 结尾的说明）。
fn reassert_positions(cx: &mut App) {
    let Some(scale) = cx
        .try_global::<MenuHost>()
        .and_then(|host| host.live.as_ref().map(|live| live.scale))
    else {
        return;
    };
    let targets: Vec<(isize, f32, f32)> = cx
        .global::<MenuHost>()
        .slots
        .iter()
        .filter(|slot| slot.on_screen)
        .filter_map(|slot| slot.hwnd.map(|hwnd| (hwnd, slot.x, slot.y)))
        .collect();
    for (hwnd, x, y) in targets {
        win32::move_window(
            hwnd,
            win32::px_to_physical(x - FRAME_INSET, scale),
            win32::px_to_physical(y, scale),
        );
    }
}

/// 自检用的一行状态：菜单还开着吗、当前几列、前台窗口是不是菜单里的某一列。
fn selftest_snapshot(cx: &mut App) -> String {
    let Some(host) = cx.try_global::<MenuHost>() else {
        return "（没有菜单宿主）".to_string();
    };
    let open = host.live.is_some();
    let path = host.state.read(cx).path.clone();
    let columns = path.len();
    let ours: Vec<isize> = host
        .slots
        .iter()
        .filter(|slot| slot.on_screen)
        .filter_map(|slot| slot.hwnd)
        .collect();
    let foreground = win32::foreground_window();
    format!(
        "开={open} 列={columns} 路径={path:?} 前台={foreground:#x}({}) 可见列={ours:?}",
        if ours.contains(&foreground) {
            "在菜单里"
        } else {
            "不在菜单里"
        }
    )
}

/// 调试用自检：`GPUI_MENU_SELFTEST=1` 时，菜单弹出后自动跑一遍「↓ → ← ←」。
///
/// 为什么不用真按键：这台机器上给 GPUI 窗口注入按键会被沙箱吞掉
/// （SendInput 返回成功，窗口收不到），所以改成自己调 `handle_key` ——
/// 走的是同一条代码路径，而这一步要验证的正是「藏掉一列窗口会不会把整个菜单连带收掉」。
fn spawn_selftest(cx: &mut App) {
    if std::env::var_os("GPUI_MENU_SELFTEST").is_none() {
        return;
    }
    cx.spawn(async move |cx| {
        // ↓×3 走到「编辑」→ → 展开二级 → ↓×3 走到「变量命名」→ → 展开三级
        // → ←×4 一路退回来（第四下已经在根层，绝不能关菜单）
        let script = [
            "down", "down", "down", "right", "down", "down", "down", "right", "left", "left",
            "left", "left",
        ];
        for (index, key) in script.iter().enumerate() {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = cx.update(|app| {
                let before = selftest_snapshot(app);
                handle_key(key, app);
                let after = selftest_snapshot(app);
                println!(
                    "[selftest] #{} {key}\n    之前 {before}\n    之后 {after}",
                    index + 1
                );
            });
        }
        println!("[selftest] 结束");
    })
    .detach();
}

/// 调试用：`GPUI_MENU_DEBUG_KEYS=down,down,enter` 按顺序敲这几下（每下隔 400ms）。
///
/// 和 `GPUI_MENU_SELFTEST` 一样直接走 `handle_key` —— 这台机器上往 GPUI 窗口
/// `SendInput` 注入的按键会被吞掉（返回成功但窗口收不到），而这里要验的是
/// 「点了这一项之后菜单变成什么样」，键本身怎么进来的不重要。
///
/// 只跑一次：`__show_all__` 会重新走一遍 `open_with`，不设防就会无限重放。
static KEY_SCRIPT_PLAYED: AtomicBool = AtomicBool::new(false);

fn spawn_key_script(cx: &mut App) {
    let Ok(script) = std::env::var("GPUI_MENU_DEBUG_KEYS") else {
        return;
    };
    let keys: Vec<String> = script
        .split(',')
        .map(|key| key.trim().to_lowercase())
        .filter(|key| !key.is_empty())
        .collect();
    if keys.is_empty() || KEY_SCRIPT_PLAYED.swap(true, Ordering::SeqCst) {
        return;
    }
    cx.spawn(async move |cx| {
        for key in keys {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = cx.update(|app| {
                handle_key(&key, app);
                println!("[keys] {key} → {}", selftest_snapshot(app));
            });
        }
    })
    .detach();
}

/// 执行了一个菜单项：关菜单 → 按顺序跑动作 → 更新主窗口状态。
///
/// 顺序很重要：先关菜单，焦点才会还给用户原来那个窗口；
/// 「发送按键 / 发送文本」这类动作打的正是那个窗口。
pub fn finish(id: SharedString, label: SharedString, actions: Vec<ActionStep>, cx: &mut App) {
    // 右键菜单里那几条不是普通动作：id 前缀认出来交给专门的处理器
    if let Some(rest) = id.strip_prefix("__ctx:") {
        close_all(cx);
        run_context_action(rest, cx);
        return;
    }
    // 「显示全部菜单」：**原地**换成不筛的完整菜单。
    //
    // 以前是「关掉再重新弹一次」：窗口被收起来又摆出来，尺寸/内容偶尔来不及跟上，
    // 表现就是「点了以后菜单还是原来那么小，下半截不见了」。原地换树只走一次
    // `sync()`（该 resize 的 resize、该重画的现画），不闪、尺寸也不会错。
    // 顺手保住「刚抓到的选中内容」——它还在树里，只是这次不按它筛。
    if id.as_ref() == "__show_all__" {
        let state = cx.global::<MenuHost>().state.clone();
        state.update(cx, |state, cx| {
            state.rebuild_with(false, true);
            state.reset();
            if std::env::var_os("GPUI_MENU_DEBUG_TIPLOG").is_some() {
                eprintln!(
                    "[showall] rebuild -> {} 项",
                    state.menu.len()
                );
            }
            cx.notify();
        });
        // 搜索框里的词也要跟着清掉：否则「显示全部菜单」换成完整菜单之后，
        // 输入框里还留着上一轮搜的词，菜单内容却是全的，看着像「搜了但没生效」。
        let input = cx.global::<MenuHost>().search.clone();
        input.update(cx, |input, cx| input.set_text("", cx));
        // 上一轮算出来的「可视高度」作废：菜单内容整套换了，键盘翻页要按新高度算。
        cx.update_global(|host: &mut MenuHost, _app: &mut App| {
            if let Some(live) = host.live.as_mut() {
                live.heights.clear();
            }
        });
        // 一次 sync 就够。尺寸没真正落到窗口上的情况（`window.resize()` 是异步的），
        // 由 `sync_with` 末尾的「尺寸对账」统一兜：它会延后读回真实客户区、对不上再补。
        // 这里再单独排一次定时 sync 属于同一件事做两遍，而且那次 sync 还会因为
        // `slot.height` 已经更新而跳过 resize —— 恰恰是修不好高度的那条路。
        sync(cx);
        return;
    }
    println!("[popup-menu] 触发菜单项: {label} ({id})");
    // 「最近运行」：执行过的项记一笔（关掉开关就不记）
    crate::settings::record_recent(cx, id.as_ref());
    let state = cx.global::<AppStateHandle>().0.clone();
    state.update(cx, |state, cx| {
        state.last_action = Some(label);
        state.hits += 1;
        cx.notify();
    });
    close_all(cx);
    let append = crate::settings::appends_selection(&id);
    run_actions(actions, append, None, false, cx);
}

/// 菜单项右键菜单里那几条（`__ctx:` 后面那半截）。
fn run_context_action(rest: &str, cx: &mut App) {
    let (command, id) = match rest.split_once(':') {
        Some((command, id)) => (command, id),
        None => (rest, ""),
    };
    match command {
        "edit" => crate::item_editor::open(id, cx),
        // 「在这一项后面新建…」：插成它的兄弟（和列表上方那排按钮同一套规则）
        "add" | "addsub" | "addsep" | "addcol" => {
            let kind = command;
            settings::update(cx, |settings| {
                let Some(path) = settings.find_path(id) else {
                    return;
                };
                let container = path[..path.len().saturating_sub(1)].to_vec();
                let after = Some(path.as_slice());
                match kind {
                    "add" => settings.add_item_in(&container, after),
                    "addsub" => settings.add_submenu_in(&container, after),
                    "addsep" => settings.add_separator_in(&container, after),
                    _ => settings.add_column_break_in(&container, after),
                };
            });
        }
        "settings" => crate::settings_ui::open(cx),
        "copy" => {
            let label = crate::settings::read(|settings| {
                settings
                    .find_path(id)
                    .and_then(|path| settings.node(&path))
                    .map(|item| item.label.clone())
                    .unwrap_or_default()
            });
            if !label.is_empty() {
                win32::write_clipboard_text(&label);
            }
        }
        "toggle" => {
            settings::update(cx, |settings| {
                if let Some(path) = settings.find_path(id) {
                    if let Some(item) = settings.node_mut(&path) {
                        item.enabled = !item.enabled;
                    }
                }
            });
        }
        "icon" => {
            settings::update(cx, |settings| {
                if let Some(path) = settings.find_path(id) {
                    if let Some(item) = settings.node_mut(&path) {
                        if item.badge.trim().is_empty() {
                            if let Some(program) = item.program_path() {
                                item.badge = program;
                            }
                        } else {
                            item.badge.clear();
                        }
                    }
                }
            });
        }
        "delete" => {
            settings::update(cx, |settings| {
                if let Some(path) = settings.find_path(id) {
                    settings.remove_at(&path);
                }
            });
        }
        // 复制成「可粘贴的菜单项」：和列表里 Ctrl+C 同一份格式
        "copyitem" => {
            let payload = settings::read(|settings| {
                settings
                    .find_path(id)
                    .and_then(|path| settings.node(&path))
                    .and_then(|item| settings::clipboard_payload(std::slice::from_ref(item)))
            });
            if let Some(text) = payload {
                win32::write_clipboard_text(&text);
            }
        }
        // 原地复制一份，插在这一项下面
        "duplicate" => {
            settings::update(cx, |settings| {
                let Some(path) = settings.find_path(id) else {
                    return;
                };
                let Some(item) = settings.node(&path).cloned() else {
                    return;
                };
                let container = path[..path.len().saturating_sub(1)].to_vec();
                settings.paste_items(&container, Some(&path), vec![item]);
            });
        }
        // 粘贴到这一项下面（从系统剪贴板读，格式同 Ctrl+C）
        "paste" => {
            let Some(text) = win32::read_clipboard_text() else {
                return;
            };
            let Some(items) = settings::items_from_clipboard(&text) else {
                return;
            };
            settings::update(cx, |settings| {
                let Some(path) = settings.find_path(id) else {
                    settings.paste_items(&[], None, items);
                    return;
                };
                let container = path[..path.len().saturating_sub(1)].to_vec();
                settings.paste_items(&container, Some(&path), items);
            });
        }
        // 把当前选中的东西变成菜单项，插在这一项后面。
        //
        // 选中是**文件**时要问一句写完整路径还是裸文件名（和从资源管理器拖进设置页
        // 的菜单项一模一样）：`运行程序` 那一栏本来就走 PATH，写 `notepad.exe` 才
        // 跟着 PATH 走；写死 C:\Windows\notepad.exe 换了机器就废。
        // 选中是**文字**时没得选，直接当一项加进去。
        "addsel" => {
            let Some(selection) = crate::selection::current() else {
                return;
            };
            if selection.files.is_empty() {
                add_selection_items(id, true, cx);
            } else {
                // 老行为是每次都问一句；设置里可以定死成「总是完整路径 / 总是裸文件名」，
                // 天天往菜单里加文件的人就不用每次点两下了
                match crate::settings::read(|settings| settings.add_selection_path) {
                    crate::selection::AddSelectionPath::Ask => {
                        open_with(cx, Source::AddSelection(id.to_string()));
                    }
                    crate::selection::AddSelectionPath::Full => {
                        add_selection_items(id, true, cx)
                    }
                    crate::selection::AddSelectionPath::Bare => {
                        add_selection_items(id, false, cx)
                    }
                }
            }
        }
        "addselfull" => add_selection_items(id, true, cx),
        "addselbare" => add_selection_items(id, false, cx),
        other => eprintln!("[popup-menu] 不认识的快捷操作 {other:?}"),
    }
}

/// 「把选中内容加到这一项后面」的第二问：完整路径还是只填文件名。
fn add_selection_entries(id: &str) -> Vec<crate::menu::MenuEntry> {
    vec![
        crate::settings::ctx_entry(
            format!("__ctx:addselfull:{id}"),
            crate::i18n::t("写完整路径").to_string(),
            "📄",
        ),
        crate::settings::ctx_entry(
            format!("__ctx:addselbare:{id}"),
            crate::i18n::t("只填文件名（跟着 PATH 找）").to_string(),
            "✎",
        ),
    ]
}

/// 真的加：一批文件（或一段文字）变成菜单项，插在 `target` 后面。
///
/// 落点和设置页里拖文件进来完全一致（`add_program_items` + 「插在选中项后面」），
/// 所以两边的结果长得一模一样 —— 名字取文件名去后缀、文件夹一律完整路径。
fn add_selection_items(target: &str, full_path: bool, cx: &mut App) {
    let Some(selection) = crate::selection::current() else {
        return;
    };
    let mut specs: Vec<(String, String)> = Vec::new();
    for path in &selection.files {
        let path = std::path::Path::new(path);
        let label = crate::tree_view::drop_label(path);
        // 和拖进设置页同一个口径：一律套双引号（带空格的路径才不会散架）
        let program = if full_path || path.is_dir() {
            crate::settings::MenuSettings::quote_path(&path.display().to_string())
        } else {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            crate::settings::MenuSettings::quote_path(&name)
        };
        specs.push((label, program));
    }
    if specs.is_empty() {
        // 文字：整段当「运行程序」，名字取第一行头几十个字
        let text = selection.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let label: String = text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(24)
            .collect();
        let label = if label.trim().is_empty() {
            crate::i18n::t("选中内容").to_string()
        } else {
            label.trim().to_string()
        };
        specs.push((label, text));
    }
    let target = target.to_string();
    let shown = target.clone();
    let ids = settings::update(cx, move |settings| {
        let Some(path) = settings.find_path(&target) else {
            return Vec::new();
        };
        let container: Vec<usize> = path[..path.len().saturating_sub(1)].to_vec();
        settings.add_program_items(&container, Some(&path), &specs)
    });
    println!("[popup-menu] 把选中内容加到 {shown} 后面：{} 项", ids.len());
}

/// 调试用：不等点菜单，直接跑某一项的动作（`GPUI_MENU_DEBUG_RUN=<条目 id>`）。
///
/// 用来验证「运行程序只填文件名」有没有真补成完整路径 —— 补成了才会真的启动。
pub fn run_item_now(cx: &mut App, id: &str) {
    let actions = settings::read(|settings| {
        let path = settings.find_path(id)?;
        settings.node(&path).map(|item| item.actions.clone())
    });
    match actions {
        Some(actions) if !actions.is_empty() => run_actions(actions, false, None, false, cx),
        _ => eprintln!("[popup-menu] 条目 {id:?} 没有动作可跑"),
    }
}

/// 热字符串命中：跑这一项的动作，跑完把被吞掉的结束键补出去。
///
/// 和点菜单的区别：
/// - **不关菜单**（本来就没开）；
/// - **不接选中内容**（打字场景没有选中内容，接了等于凭空往命令里塞东西）；
/// - 第一条动作**不等那 80ms**：触发词刚从屏幕上删掉，晚一点出字用户就看得出卡；
/// - 动作全跑完要补发结束键（Tab / 回车被钩子吞掉了，得还给目标程序）。
pub fn run_hotstring(
    cx: &mut App,
    id: &str,
    replay: Option<Replay>,
    ime_cleanup: Option<usize>,
) {
    // 中文输入法还压着组字（拼音在候选窗里没上屏）：先让输入法把这段丢掉，
    // 再按「屏幕上已提交的字符数」退格。这两步必须在这里做 —— `Imm*` 是
    // 线程绑定的，钩子线程调了也没用。
    if let Some(count) = ime_cleanup {
        // 输入法的上下文挂在**拿到焦点的那个控件**上（浏览器、编辑器都是这样），
        // 只在顶层窗口上取消是取消不掉的
        let target = crate::win32::input_target();
        crate::win32::cancel_ime_composition(if target.1 != 0 { target.1 } else { target.0 });
        // 输入法收组字窗是异步的，稍微等一下再退格，否则退格会被它当成删拼音
        std::thread::sleep(std::time::Duration::from_millis(25));
        crate::win32::send_backspaces(count);
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    // 绑在子菜单上：先把被吞掉的结束字符还给目标程序，再弹那一支
    if settings::item_is_submenu(id) {
        if let Some(replay) = replay {
            replay.send();
        }
        open_submenu(cx, id);
        return;
    }
    let actions = settings::read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .map(|item| item.actions.clone())
            .unwrap_or_default()
    });
    if actions.is_empty() {
        // 没动作（比如只是个占位项）：至少别把用户敲的那个结束字符吞掉
        if let Some(replay) = replay {
            replay.send();
        }
        return;
    }
    run_actions(actions, false, replay, true, cx);
}

/// 按顺序执行多条动作，每条之间按 `delay_ms` 等待。
///
/// 用 `cx.spawn` 跑成一条异步链，界面不会被 `sleep` 卡住；
/// `Keys` / `Text` 前面再多等一点，等系统把前台窗口切回去。
///
/// `replay` 是热字符串场景专用的：动作全跑完以后把被钩子吞掉的结束字符
/// （空格 / 标点 / Tab / 回车）补发出去，否则用户在目标程序里按的那一下就像没按。
/// `eager` = 第一条动作不等那 80ms（热字符串要求「立刻出字」）。
fn run_actions(
    actions: Vec<ActionStep>,
    append_selection: bool,
    replay: Option<Replay>,
    eager: bool,
    cx: &mut App,
) {
    if actions.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        for (index, step) in actions.iter().enumerate() {
            if step.kind.needs_focus_back() && !(eager && index == 0) {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(80))
                    .await;
            }
            let mut failure = None;
            let _ = cx.update(|app| {
                if let Err(err) = execute_step(app, step, append_selection) {
                    failure = Some(err);
                }
            });
            if let Some(err) = failure {
                eprintln!(
                    "[popup-menu] 第 {} 条动作失败（{}）: {err}",
                    index + 1,
                    step.summary()
                );
            }
            if step.delay_ms > 0 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(step.delay_ms as u64))
                    .await;
            }
        }
        if let Some(replay) = replay {
            replay.send();
        }
    })
    .detach();
}

/// 执行单条动作。
/// 单独跑一条动作（编辑窗口里的「测试」按钮用）。
///
/// 和菜单里执行走的是同一段代码：占位符展开、程序解析、按键发送全一致，
/// 差别只在于不抓选中内容（`append_selection = false`）。
pub fn test_run_step(cx: &mut App, step: &ActionStep) -> Result<(), String> {
    execute_step(cx, step, false)
}

fn execute_step(cx: &mut App, step: &ActionStep, append_selection: bool) -> Result<(), String> {
    // 占位符在**执行的这一刻**才替换：`{clipboard}` 这类要取实时值。
    // 内置动作的值是动作 key，替换了反而认不出来，所以它不参与。
    // 脚本是**代码**：`{...}` 在 AHK 里是对象字面量 / 热键语法，不能整套替换；
    // 但 RunAny 的 `%getZz%` 必须认（用户从 RunAny 导进来的脚本里到处是它）。
    let value = if step.kind == ActionKind::Builtin {
        step.value.trim().to_string()
    } else if step.kind == ActionKind::Script {
        crate::placeholders::expand_runany(step.value.trim())
    } else {
        crate::placeholders::expand(step.value.trim())
    };
    let value = value.as_str();
    match step.kind {
        ActionKind::Run => {
            if value.is_empty() {
                return Ok(());
            }
            // 「运行程序 + 接上选中内容」：`code ` + `"D:\a b.txt"` 这样拼
            let command = match append_selection.then(crate::selection::append_arg).flatten() {
                Some(argument) => format!("{value} {argument}"),
                None => value.to_string(),
            };
            // 只填了文件名（没有 `\` 没有 `:`）就去本地索引里补成完整路径：
            // `cmd` 自己只会按 PATH 找，装进 Program Files 的那些它找不到。
            let command = crate::progindex::resolve_command(&command);
            // 管理员 / 已在运行的处置 / 工作目录 / 窗口状态都在 `runopts` 里分叉，
            // 没有这些参数时还是走原来那条「交给 shell 起一个」的路
            crate::runopts::execute(&command, &step.params, |line| {
                spawn_command(line).map_err(|err| err.to_string())
            })
        }
        ActionKind::Url => {
            if value.is_empty() {
                return Ok(());
            }
            spawn_command(&format!("start \"\" {value}")).map_err(|err| err.to_string())
        }
        ActionKind::Keys => {
            if value.is_empty() {
                return Ok(());
            }
            win32::send_keys(value)
        }
        ActionKind::Text => {
            if value.is_empty() {
                return Ok(());
            }
            win32::send_text(value)
        }
        ActionKind::Builtin => {
            // 老配置里的 9 个内置动作（open-settings / quit …）还要能跑
            if let Some(action) = BuiltinAction::from_key(value) {
                run_builtin_action(action, cx);
                return Ok(());
            }
            crate::actions::execute(value, &step.params)
        }
        ActionKind::Script => {
            let lang = crate::actions::ScriptLang::from_key(step.param("lang").unwrap_or("v1"));
            crate::actions::run_script(value, lang)
        }
    }
}

/// 内置动作：都是本程序自己的事，所以直接在这里分发。
pub fn run_builtin_action(action: BuiltinAction, cx: &mut App) {
    match action {
        BuiltinAction::OpenSettings => crate::settings_ui::open(cx),
        BuiltinAction::OpenConfigDir => {
            if let Some(path) = settings::config_path_display().and_then(|path| {
                std::path::Path::new(&path)
                    .parent()
                    .map(|p| p.to_path_buf())
            }) {
                let _ = spawn_shell(&["/C", "start", "", &path.display().to_string()]);
            }
        }
        // 「关于」是设置窗口里的一页：没开就开、开着就翻过去
        BuiltinAction::About => crate::settings_ui::open_about(cx),
        BuiltinAction::ReloadMenu => {
            // 设置已经在内存里，菜单树按 revision 重建；这里只把窗口池收一下再预热
            settings::update(cx, |_| {});
            close_all(cx);
        }
        // 托盘菜单里的「打开完整菜单」：菜单这会儿已经收起来了，重开一份
        BuiltinAction::OpenMenu => open(cx),
        BuiltinAction::Restart => {
            // 先把菜单收掉、把焦点还给用户原来的窗口，再拉一个新的自己。
            // 用 `cmd` 中转 + 等一秒：直接 `spawn` 的话，旧进程还没退干净，
            // 新进程可能和它抢同一个托盘图标 / 全局热键。
            close_all(cx);
            restart_self();
            cx.quit();
        }
        BuiltinAction::Quit => cx.quit(),
        BuiltinAction::CloseMenu => close_all(cx),
        BuiltinAction::CopySelection => {
            // 菜单第一项：把弹出前抓到的选中内容写回剪贴板
            // （用抓取那一刻的剪贴板快照，所以文件 / 图片也是原样回去的）
            crate::selection::copy_back();
        }
    }
}

/// 退出后把自己重新拉起来（等一秒，让旧进程把托盘图标、热键还回去）。
///
/// 用 `ping` 当 `sleep`：`timeout` 在 `CREATE_NO_WINDOW` 下会报「输入重定向不受支持」。
fn restart_self() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let command = format!("ping -n 2 127.0.0.1 >nul & start \"\" \"{}\"", exe.display());
    let _ = spawn_command(&command);
}

/// 起一个后台进程，走 shell（Windows 用 `cmd`，其它平台用 `sh`）。
#[cfg(windows)]
/// 让 `cmd /C` 原样吃掉一整串命令。
///
/// 这里必须用 `raw_arg`（不是 `arg`）：路径里有空格时要靠 `"..."` 让 cmd 认出
/// 程序名，而普通的 `arg` 会再套一层引号转义，cmd 拿到的就成了带引号的
/// **名字**（实测报「'"C:\...\a.exe"' 不是内部或外部命令」）。
fn spawn_command(command: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW：别为一个菜单项闪一下黑窗口
    std::process::Command::new("cmd")
        .arg("/C")
        .raw_arg(command)
        .creation_flags(0x0800_0000)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn spawn_command(command: &str) -> std::io::Result<()> {
    std::process::Command::new("sh")
        .args(["-c", command])
        .spawn()
        .map(|_| ())
}

fn spawn_shell(args: &[&str]) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW：别为一个菜单项闪一下黑窗口
    std::process::Command::new("cmd")
        .args(args)
        .creation_flags(0x0800_0000)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn spawn_shell(args: &[&str]) -> std::io::Result<()> {
    let command = args.last().copied().unwrap_or("");
    std::process::Command::new("sh")
        .args(["-c", command])
        .spawn()
        .map(|_| ())
}

/// 调试用：直接把菜单换成「完整菜单」（`GPUI_MENU_DEBUG_SHOWALL=1`）。
///
/// 走的就是「显示全部菜单」那一条代码路径 —— 手上没有选中内容时，
/// 正常途径弹出来的本来就是完整菜单，压根没有那条入口可点。
pub fn show_all_now(cx: &mut App) {
    if cx.global::<MenuHost>().live.is_none() {
        return;
    }
    finish(
        SharedString::from("__show_all__"),
        SharedString::from(crate::i18n::t("显示全部菜单")),
        Vec::new(),
        cx,
    );
}

pub fn close_all(cx: &mut App) {
    // 悬停提示跟着菜单一起收（它是独立窗口，不收就一直挂在那儿）
    crate::tooltip::hide_now();
    let Some(live) = cx.global_mut::<MenuHost>().live.take() else {
        return;
    };
    // 先把焦点还给用户原来的窗口，再把菜单挪走，
    // 否则焦点会留在一个看不见的窗口上。
    //
    // 用 `bring_to_front` 而不是只 `set_foreground`：菜单是 TOPMOST、又抢过前台，
    // 光交还前台不一定能把那个窗口从别的程序下面捞出来（点「打开菜单试试」之后
    // 设置窗口跑到别的窗口底下，就是这个原因）。
    if live.previous_foreground != 0 {
        win32::bring_to_front(live.previous_foreground);
    }
    drop(live); // _keys 随之解绑

    if CAN_MOVE_WINDOWS {
        let mut slots = std::mem::take(&mut cx.global_mut::<MenuHost>().slots);
        for slot in slots.iter_mut() {
            park(slot);
        }
        cx.global_mut::<MenuHost>().slots = slots;
        schedule_idle_shrink(cx);
    } else {
        let old = std::mem::take(&mut cx.global_mut::<MenuHost>().slots);
        for slot in old {
            slot.handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
    }
}

// ---------------------------------------------------------------------------
// 空闲回收
// ---------------------------------------------------------------------------

/// 关掉菜单 `IDLE_SHRINK` 之后如果还没再弹过，就把池里的窗口全收掉。
///
/// 只在「菜单已经关掉」时动手：键盘导航、看门狗那些都要求窗口还在。
/// 等待期间又弹了一次菜单就什么都不做 —— 那一轮关掉时自己会排下一次。
fn schedule_idle_shrink(cx: &mut App) {
    if cx.global::<MenuHost>().slots.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(IDLE_SHRINK).await;
        let _ = cx.update(|app| {
            if app.global::<MenuHost>().live.is_some() {
                return;
            }
            let slots = std::mem::take(&mut app.global_mut::<MenuHost>().slots);
            if slots.is_empty() {
                return;
            }
            let count = slots.len();
            for slot in slots {
                slot.handle
                    .update(app, |_, window, _| window.remove_window())
                    .ok();
            }
            // 窗口没了，句柄也撤了；剩下的驱动页 / 图集缓冲全是冷的，收回去。
            // （实测 73 MB → 几 MB，下一次弹出的代价约 50ms。）
            win32::trim_working_set();
            println!("[popup-menu] 窗口池已回收（{count} 列），内存还给系统");
        });
    })
    .detach();
}

// ---------------------------------------------------------------------------
// 窗口几何（纯函数，有单测）
// ---------------------------------------------------------------------------

/// 计算每一列的 (x, y, height, width)。
///
/// `steps[level]` = (整列完整高度, 相对父列「行区域顶部」的 y 偏移, 卡片宽度,
/// **子菜单从父列左边多少像素处起（父项那一列的右边缘）**,
/// **翻到左边时从父列左边多少像素处起（父项那一列的左边缘）**）。
///
/// 横向：默认贴着父项那一列的右边缘展开（跟系统自带右键菜单一样，鼠标往右
/// 一挪就进去了，盖住右边并排的列也无所谓）；右边确实放不下才整条翻到左边，
/// 翻过去贴的是**父项那一列的左边缘**，不是整张卡片的左边缘。
///
/// 竖向两条规则：
///
/// 1. **首行先对齐父项那一行**（顶边 = 父行顶边 - 行区留白）。
/// 2. **下方放不下就整张往上挪**，挪到刚好贴住屏幕底 —— 也就是「往上长」。
///    整张一律不缩，只有比整个屏幕还高时才截到屏幕高、内部滚动。
///
/// 以前试过「顶边死钉在父项那一行、下方剩多少就切成多高、剩下的滚动」，
/// 结果是父项在屏幕中上部时，一列长菜单被切成半截还带滚动条；用户要的是
/// Windows 自带右键菜单那种手感：宁可顶边离开父项，也要整张摊开。
fn column_targets(
    origin: (f32, f32),
    screen: (f32, f32, f32, f32),
    steps: &[(f32, f32, f32, f32, f32)],
) -> Vec<(f32, f32, f32, f32)> {
    let (screen_left, screen_top, screen_w, screen_h) = screen;
    let screen_right = screen_left + screen_w;
    let screen_bottom = screen_top + screen_h - layout::SCREEN_MARGIN;
    let min_height = layout::min_column_height(0);

    let mut targets = Vec::with_capacity(steps.len());
    let (mut prev_x, mut prev_y) = origin;
    // 上一级「子菜单该从哪儿起」：右边界和左边界各一份，都由 `sync` 按
    // 「父项落在多列卡片里的哪一段」算好递进来
    let mut prev_right = steps.first().map(|step| step.3).unwrap_or(0.0);
    let mut prev_left = steps.first().map(|step| step.4).unwrap_or(0.0);
    // 一旦某级翻到左边，后面各级都继续往左，避免来回横跳
    let mut going_left = false;
    // 屏幕可用的整块高度：子菜单比它还高才需要滚动
    let screen_avail = screen_bottom - screen_top;

    for (level, &(full_height, offset, width, child_right, child_left)) in steps.iter().enumerate()
    {
        if level == 0 {
            // 第一列贴着光标；太高就往上顶，把屏幕高度用满（剩下的滚动）
            let x = prev_x.min(screen_right - width).max(screen_left);
            let height = layout::clamp_column_height(0, full_height, screen_avail);
            let mut y = prev_y;
            if y + height > screen_bottom {
                y = screen_bottom - height;
            }
            y = y.min(screen_bottom - min_height).max(screen_top);
            targets.push((x, y, height, width));
            prev_x = x;
            prev_y = y;
            prev_right = child_right;
            prev_left = child_left;
            continue;
        }

        // 横向：先试右边（贴父项那一列的右边缘）；右边放不下就翻到左边，
        // 贴父项那一列的**左边缘**（不是整张卡片的左边缘）
        if !going_left && prev_x + prev_right + width > screen_right {
            going_left = true;
        }
        let mut x = if going_left {
            prev_x + prev_left - width
        } else {
            prev_x + prev_right
        };
        // 翻到左边还是放不下（子菜单比父项左边那点空间还宽）：贴屏幕左边，
        // 至少保证整列都在屏幕里
        x = x.min(screen_right - width).max(screen_left);

        // 父项那一行的顶边（屏幕坐标）—— 子菜单要「对齐」的就是它
        let anchor_y = prev_y + offset;
        // 首行对齐：卡片顶边 = 父行顶边 - 行区上方的留白
        let aligned_top = anchor_y - layout::row_top(level);

        // 高度：**整张一律不缩**（只有比整个屏幕还高才截到屏幕高，剩下的滚动）。
        //
        // 位置：原版 RunAny 的手感 ——
        //   · 光标在上面、下方还有地方：**子菜单第一项跟父项那一行齐平**（往下长）；
        //   · 光标贴着屏幕下面、下方放不下：**子菜单最后一项跟父项那一行齐平**
        //     （往上长）。视线不用离开父项，也不用去够屏幕底部。
        // 两种对齐都以「父项那一行」为基准，所以翻上翻下的时候菜单不会跳来跳去。
        let height = full_height.min(screen_avail);
        let mut y = aligned_top;
        if y + height > screen_bottom {
            // 往上长：让**最后一行**压在父项那一行上 ——
            // 卡片底边 = 父项行底 + 行区下方留白，倒推卡片顶边。
            y = anchor_y + layout::row_height() + layout::row_bottom_edge(level) - height;
        }
        // 上边也不能顶出屏幕（父项很靠下、子菜单又比屏幕高时）
        y = y.max(screen_top);

        targets.push((x, y, height, width));
        prev_x = x;
        prev_y = y;
        prev_right = child_right;
        prev_left = child_left;
    }
    targets
}

// ---------------------------------------------------------------------------
// 同步窗口
// ---------------------------------------------------------------------------

/// 把菜单状态拍成 `column_targets` 要的那串 `steps`。
///
/// 每列五元组：完整高度、**父项那一行相对父卡片顶边的 y 偏移**、卡片宽度、
/// 子菜单往右展开时的起点 x、翻到左边时的起点 x。
///
/// 这里最容易错的是第二项：算父行位置必须用**父项所在那一层**的条目。
/// 以前拿的是子菜单自己的条目 `entries`，多列卡片里父项在第 2 段时，
/// 索引落到子菜单里一个毫不相干的行上 —— 子菜单整张往下挪「索引×行高」，
/// 用户看到的就是「右列的子菜单跑到屏幕下半截去了」。
fn column_steps(
    snapshot: &MenuState,
    measure: &dyn Fn(&str, f32) -> f32,
) -> Vec<(f32, f32, f32, f32, f32)> {
    (0..snapshot.columns())
        .map(|level| {
            let entries = snapshot.entries(level);
            // 至少要有「一行」那么高：搜索词一条都没命中时行数是 0，
            // 不兜这一下卡片会缩成只有搜索框那么一条缝，
            // 连「没有匹配的项」那句提示都塞不进去。
            let full = layout::column_height(level, entries).max(layout::min_column_height(level));
            let offset = if level == 0 {
                0.0
            } else {
                let parent = snapshot.entries(level - 1);
                let index = snapshot.path.get(level - 1).copied().unwrap_or(0);
                layout::row_top(level - 1) + layout::row_offset(parent, index)
                    - snapshot.scroll_of(level - 1)
            };
            // 多列符切出来的列并排在同一张卡片里，每段各按自己的内容算宽
            let widths = layout::column_widths_with(entries, measure);
            let width = layout::column_span(&widths);
            // 子菜单就贴在**父项自己那一列**的右边缘展开 —— 跟 Windows 自带的
            // 右键菜单一样：名字旁边一格就是子菜单，鼠标往右一挪就进去了。
            //
            // 多列卡片里，父项在第 2 段、子菜单就从第 2 段右边出来，会盖住
            // 第 3 段。以前贴的是整张卡片的右边缘：看着不盖东西，但子菜单
            // 和名字之间隔着一整列不相干的内容，眼睛要跨过去才找得到。
            //
            // 想「不盖住右边几列」可以在设置里选「贴卡片」，那时从头到尾
            // 都贴整张卡片的右边缘（= `width`）。
            let hugs = settings::read(|settings| settings.submenu_anchor).hugs_name();
            let (child_right, child_left) = match snapshot.path.get(level) {
                Some(&idx) if hugs => (
                    layout::item_column_right_edge(entries, idx, &widths),
                    layout::item_column_left_edge(entries, idx, &widths),
                ),
                // 贴卡片：往右就从整张卡片右边出来，往左也从整张卡片左边出来
                _ => (width, 0.0),
            };
            (full, offset, width, child_right, child_left)
        })
        .collect()
}

pub fn sync(cx: &mut App) {
    sync_with(cx, true);
}

/// `steal_focus` = 藏/显了一列之后要不要顺手把前台抢回菜单。
///
/// 打开菜单那一刻**不要**（`open_with` 自己马上就会 activate + set_foreground），
/// 否则会白付一次 `SetForegroundWindow`：调用方不是前台进程时，系统会把这次调用
/// 拖到两百毫秒上下才返回 —— 一秒都没到，但「按下去就该立刻出来」的手感全毁在这儿。
fn sync_with(cx: &mut App, steal_focus: bool) {
    let Some((state, origin, screen, scale)) = cx.try_global::<MenuHost>().and_then(|host| {
        host.live
            .as_ref()
            .map(|live| (host.state.clone(), live.origin, live.screen, live.scale))
    }) else {
        return;
    };

    let t_sync = std::time::Instant::now();
    // 这一轮要画的菜单树是哪一版（尺寸没变、内容变了也要重画）
    let revision = state.read(cx).revision;
    // 每列：完整高度 + 相对父列行区域顶部的偏移 + 卡片宽度 + 子菜单的起止 x
    let steps = {
        let snapshot = state.read(cx);
        // 自动列宽要按真实字体量文字，量不准就会截字/留白过多
        let family = crate::theme::font_family();
        let system: &gpui::TextSystem = cx.text_system();
        column_steps(&snapshot, &|text, size| {
            layout::measure_text(system, &family, size, text)
        })
    };

    let targets = column_targets(
        (f32::from(origin.x), f32::from(origin.y)),
        (
            f32::from(screen.origin.x),
            f32::from(screen.origin.y),
            f32::from(screen.size.width),
            f32::from(screen.size.height),
        ),
        &steps,
    );

    if timing_enabled() {
        println!("[timing]     steps {}ms", t_sync.elapsed().as_millis());
        println!(
            "[timing]     origin=({}, {}) screen=({}, {}, {}, {}) scale={scale} steps={steps:?} targets={targets:?}",
            f32::from(origin.x),
            f32::from(origin.y),
            f32::from(screen.origin.x),
            f32::from(screen.origin.y),
            f32::from(screen.size.width),
            f32::from(screen.size.height)
        );
    }
    // 键盘翻页要用「当前列的可视高度」
    cx.update_global(|host: &mut MenuHost, _app: &mut App| {
        if let Some(live) = host.live.as_mut() {
            live.heights = targets.iter().map(|&(_, _, height, _)| height).collect();
        }
    });

    if CAN_MOVE_WINDOWS {
        // 槽位不够就补建（正常情况下一列一列现建，16ms 量级）
        //
        // **建的时候就把目标位置/尺寸给它**：以前先建在 (0,0) 再搬过去，中间有一帧
        // 窗口是「GPUI 自己那套初始 bounds」——x 还是 -8（FRAME_INSET），GPUI 判定
        // 出屏了，直接换成「屏幕居中 1440×753」显示出来。用户看到的就是展开子菜单
        // 那一瞬闪一下大白块（也是「卡」的一部分）。
        let mut created_any = false;
        while cx.global::<MenuHost>().slots.len() < targets.len() {
            let level = cx.global::<MenuHost>().slots.len();
            let before = level;
            let at = targets.get(level).copied();
            create_slot(cx, state.clone(), level, scale, at);
            if cx.global::<MenuHost>().slots.len() == before {
                break; // 建失败，别死循环
            }
            // 新窗口一出来，前台可能被系统抖一下 —— 和「收/开一列」一样要放过失焦
            created_any = true;
        }

        let mut slots = std::mem::take(&mut cx.global_mut::<MenuHost>().slots);
        let opened = targets.len().min(slots.len());
        if timing_enabled() {
            println!("[timing]     取 slots {}ms", t_sync.elapsed().as_millis());
        }

        // 将要被藏起来的那几列里，如果有一列正拿着前台，必须在**藏之前**把前台交给
        // 还开着的最上面那一列。顺序反了就来不及：`SW_HIDE` 一执行，Windows 立刻
        // 自己挑下一个前台（多半是别的程序），这时本进程已经没资格再
        // `SetForegroundWindow`，菜单就会被 watchdog 当成「用户点走了」收掉。
        let foreground = win32::foreground_window();
        if foreground != 0
            && slots
                .iter()
                .skip(opened)
                .any(|slot| slot.on_screen && slot.hwnd == Some(foreground))
        {
            let next = slots[..opened]
                .iter()
                .rev()
                .find(|slot| slot.on_screen)
                .and_then(|slot| slot.hwnd);
            if let Some(next) = next {
                win32::set_foreground(next);
            }
        }

        let mut churn = created_any;
        // 这一轮有没有动过尺寸：动了才需要延后读回真实尺寸对账
        let mut resized_any = false;
        let t_place = std::time::Instant::now();
        for (level, slot) in slots.iter_mut().enumerate() {
            match targets.get(level) {
                Some(&(x, y, height, width)) => {
                    if !slot.on_screen {
                        churn = true;
                    }
                    if place(cx, slot, x, y, width, height, scale, revision) {
                        resized_any = true;
                    }
                }
                None => {
                    if slot.on_screen {
                        churn = true;
                    }
                    park(slot);
                }
            }
        }
        if timing_enabled() {
            println!("[timing]     place×{opened} 花了 {}ms", t_place.elapsed().as_millis());
        }
        // 层级：按列顺序各抬一次。屏幕不够宽时几列会被夹到同一个 x，
        // 不抬的话后显示的那一列可能压在前一列下面（看着就是「子菜单没出来」）。
        // 抬到置顶层，顺带保证菜单压在用户当前那个程序上面。
        let t_raise = std::time::Instant::now();
        for slot in slots.iter().filter(|slot| slot.on_screen) {
            if let Some(hwnd) = slot.hwnd {
                let t_one = std::time::Instant::now();
                win32::raise_window(hwnd);
                if timing_enabled() {
                    println!("[timing]     raise 一列 {}ms", t_one.elapsed().as_millis());
                }
            }
        }
        if timing_enabled() {
            println!("[timing]     raise 共 {}ms", t_raise.elapsed().as_millis());
        }
        cx.global_mut::<MenuHost>().slots = slots;

        if timing_enabled() {
            println!("[timing]     sync 总计 {}ms", t_sync.elapsed().as_millis());
        }

        if timing_enabled() {
            let handles: Vec<String> = cx
                .global::<MenuHost>()
                .slots
                .iter()
                .enumerate()
                .map(|(level, slot)| {
                    format!(
                        "#{level} hwnd={:?} on={} at=({}, {}) {}x{}",
                        slot.hwnd, slot.on_screen, slot.x, slot.y, slot.width, slot.height
                    )
                })
                .collect();
            println!("[timing]     slots: {}", handles.join(" | "));
        }

        if churn {
            cx.update_global(|host: &mut MenuHost, _app: &mut App| {
                host.grace = Some(std::time::Instant::now());
            });
            if steal_focus {
                // 收起一列时前台可能已经掉出去了，趁还开着的时候拉回来。
                keep_focus_inside(cx);
            }
        }

        // ---- 尺寸对账 ----
        //
        // `window.resize()` 只是往 executor 里排了个 `SetWindowPos`，不是立即生效：
        // 这一轮刚排下去时读回来的客户区还是旧值，偶尔还会被别的 `SetWindowPos`
        // 顶掉。所以延后一点点读真实客户区，还对不上就再 sync 一次（最多几次），
        // 保证窗口一定收敛到算出来的高度。
        //
        // 少了这一步，「显示全部菜单」把内容从几行换成几十行时，窗口可能停在
        // 原来那个高度：内容超出视口 → 多出一条滚动条 + 底部「还有更多」的箭头，
        // 而且因为 `slot.height` 已经记成新值，后面的 sync 全都不会再补尺寸。
        if resized_any {
            let retry = cx.update_global(|host: &mut MenuHost, _app: &mut App| {
                if host.sync_retry < 4 {
                    host.sync_retry += 1;
                    true
                } else {
                    false
                }
            });
            if retry {
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    let _ = cx.update(|app| sync_with(app, false));
                })
                .detach();
            }
        } else {
            cx.update_global(|host: &mut MenuHost, _app: &mut App| host.sync_retry = 0);
        }
    } else {
        // 非 Windows：没有移动窗口的能力，只能每次重建
        let old = std::mem::take(&mut cx.global_mut::<MenuHost>().slots);
        for slot in old {
            slot.handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
        let mut slots = Vec::new();
        for (level, &(x, y, height, width)) in targets.iter().enumerate() {
            if create_slot(cx, state.clone(), level, scale, Some((x, y, width, height))).is_some() {
                if let Some(slot) = cx.global_mut::<MenuHost>().slots.pop() {
                    slots.push(slot);
                }
            }
        }
        cx.global_mut::<MenuHost>().slots = slots;
    }
}

/// 把一列摆到 `(x, y)`、高 `height`。
///
/// **顺序是有讲究的：显示 → 改尺寸 → 最后搬位置。**
///
/// 以前是「改尺寸 → 搬位置 → 显示」，结果第一列老是回到屏幕左上角、高度也不对：
/// GPUI 的 `window_bounds` 不是创建时就落到系统窗口上的，而是**第一次显示的那一帧**
/// 才应用（实测创建后窗口其实是默认的 1440x759）。所以：
///
/// - 改尺寸要排在显示**之后** —— 排在前面会被「应用初始 bounds」这一步盖掉，
///   表现就是「菜单窗口在自己该在的位置上，但只有半截高、下半截是空白」；
/// - 搬位置要排在**最后** —— GPUI 应用尺寸时会用它自己记的窗口原点重新
///   `SetWindowPos`，我们先前设的位置会被顺手拽回初始原点（(0,0) 那一套）。
fn place(
    cx: &mut App,
    slot: &mut Slot,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    scale: f32,
    revision: u64,
) -> bool {
    // 列宽（多列时是一整张卡片的总宽）、列高都可能被设置改，两者都要跟着 resize。
    //
    // **判定依据是系统里的真实客户区，不是自己记的 `slot.height`。**
    // GPUI 的 `window.resize()` 只是往 executor 排了个 `SetWindowPos`（异步），
    // 窗口刚显示、正被搬位置、或一轮里连着改几次时都可能没落到窗口上；
    // 而 `slot.height` 在请求的一瞬间就被改成新值，之后每一轮 sync 都以为
    // 「尺寸已经对了」不再补 —— 结果就是菜单卡在旧高度：内容超出，多出一条
    // 滚动条和底部「还有更多」的向下箭头。点「显示全部菜单」长不大也是这个。
    let resized = match slot.hwnd.and_then(win32::client_size) {
        Some((client_w, client_h)) => {
            let client_w = client_w as f32 / scale;
            let client_h = client_h as f32 / scale;
            (client_w - width).abs() > 0.5 || (client_h - height).abs() > 1.0
        }
        // 还读不到（窗口没建好）：退回自己记的值，聊胜于无
        None => (slot.height - height).abs() > 0.5 || (slot.width - width).abs() > 0.5,
    };
    // 尺寸没变也要重画的情况：菜单树换了一版（「显示全部菜单」/ 搜索筛过 / 重建）
    let stale = slot.revision != revision;
    let moved = (slot.x - x).abs() > 0.5 || (slot.y - y).abs() > 0.5;
    let was_hidden = !slot.on_screen;
    if let Some(hwnd) = slot.hwnd {
        // 1) 从隐藏里出来这一路，**先把几何同步落到位、再显示**：
        //
        //    GPUI 建窗走 `CreateWindowExW` + `CW_USEDEFAULT`，我们给的 bounds 并不会
        //    立刻落到系统窗口上 —— 真实窗口一开始是「整块屏幕居中」（实测 1440×753）。
        //    以前是「显示 → 排队 resize → 搬位置」，中间那一两帧就是一块盖满屏幕的
        //    白板：鼠标扫过子菜单时「闪一下」就是它，也是「卡」的一半。
        //
        //    现在的顺序是：alpha=0 → 原地改尺寸/搬位置 → 显示 → 刷一帧 → 恢复 alpha。
        //    任何还没画好的帧都是全透明的，用户看到的第一个像素就是正确尺寸的菜单。
        // **只有还藏着的窗口才能用同步 `SetWindowPos` 改尺寸。**
        //
        // 已经显示出来的窗口走这条路会踩到 GPUI 的一个坑：系统那边尺寸变了，
        // 但它自己记的 `viewport` 还停在旧值（实测客户区 373×623、viewport 仍是
        // 348×521），于是整列按**旧高度**排版 —— 卡片在旧高度那里被切一刀，
        // 切面以下那一截永远不画，露出来的就是壁纸。也就是用户看到的
        // 「子菜单展开后显示不全、下面一大块空白」。
        //
        // 可见窗口一律交给 `window.resize()`：它排进 executor 之后由 GPUI 自己
        // `SetWindowPos`，WM_SIZE 走的是它认得的那条路，viewport 会跟着改。
        if was_hidden {
            win32::fit_client_size(hwnd, width, height, scale);
        }
        if was_hidden {
            win32::set_window_opacity(hwnd, 0.0);
            win32::move_window(
                hwnd,
                win32::px_to_physical(x - FRAME_INSET, scale),
                win32::px_to_physical(y, scale),
            );
            // 不抢焦点地显示出来（此时 alpha 还是 0）
            win32::show_window(hwnd);
        }
        // 2) 尺寸改了 / 刚从隐藏状态出来：内容可能是旧的那一版（GPUI 的 resize 是
        //    「改窗口 + 等下一帧重新布局」，这时候如果不主动刷一帧，用户看到的就是
        //    「窗口变大了但里面的菜单还是原来那么短」）。这里同步刷一帧，让新尺寸
        //    和新内容一次到位。顺便清掉上一轮遗留的「拖滚动条」状态。
        //
        //    窗口尺寸本身已经在第 1 步用同步 `SetWindowPos` 落好了；这里的
        //    `window.resize()` 是让 GPUI 自己的布局状态跟上（它按自己记的 bounds 排版）。
        if resized || was_hidden {
            slot.handle
                .update(cx, |_, window, _| {
                    window.resize(size(px(width), px(height)))
                })
                .ok();
            slot.height = height;
            slot.width = width;
        }
        if was_hidden || resized || stale {
            slot.revision = revision;
            let t_refresh = std::time::Instant::now();
            slot.handle
                .update(cx, |view, window, cx| {
                    if let Ok(entity) = view.downcast::<ColumnView>() {
                        entity.update(cx, |view, cx| {
                            view.cancel_drag();
                            cx.notify();
                        });
                    }
                    window.refresh();
                })
                .ok();
            if timing_enabled() {
                println!(
                    "[timing]     refresh(hidden={was_hidden} resized={resized}) {}ms",
                    t_refresh.elapsed().as_millis()
                );
            }
            // 隔一帧再画一次（「闪现旧帧」的收尾，见 `settle_frame`）：
            // 窗口刚改过尺寸 / 刚显示出来时，DWM 会先把**上一帧按新尺寸拉伸**着顶着，
            // 要等下一帧合成上来才换掉。只画一次的话，用户看到的就是
            // 「列表被切掉一截、下面一片空白」，而且不动鼠标就一直是那样。
            // 顺带在这一帧之后才把 alpha 恢复成正常值（隐藏→显示的路径），
            // 保证用户看见的第一个像素就是干净的成品。
            settle_frame(cx, slot.handle, hwnd, was_hidden, width, height, scale);
        }
        // 搬位置**永远放最后**，而且跟「这一轮改没改尺寸」无关：
        //
        // GPUI 改尺寸时用它自己记的原点重新 `SetWindowPos`，先搬会被拽回去；
        // 但反过来「改了尺寸就跳过搬位置」更糟 —— 从「工具」挪到「窗口」时子菜单
        // 高度变了、位置也该往下走 24px，结果 y 一直停在上一个父项那一行：
        // 用户看到的「子菜单跟父项没对齐、跑到上面去了」就是这个。
        // （`window.resize()` 带的是 `SWP_NOMOVE`，所以搬完不会被它推翻。）
        if moved && !was_hidden {
            win32::move_window(
                hwnd,
                win32::px_to_physical(x - FRAME_INSET, scale),
                win32::px_to_physical(y, scale),
            );
        }
    }
    if timing_enabled() {
        println!(
            "[timing]     place @({x}, {y}) {}x{height} moved={moved} hidden={was_hidden} resized={resized} rect={:?}",
            width,
            slot.hwnd.and_then(win32::window_rect)
        );
    }
    slot.x = x;
    slot.y = y;
    slot.on_screen = true;
    // 这一轮动过尺寸（或刚从隐藏里出来，GPUI 会先套用它自己记的初始 bounds）：
    // 调用方要延后读回真实尺寸对账，不对就再 sync 一次。
    resized || was_hidden
}

/// 改完尺寸/刚显示出来后，隔几帧再画，顺便把「还没真正露脸」的窗口点亮。
///
/// 要补帧的理由有两条（都是实测撞出来的）：
///
/// 1. 窗口是 `WS_EX_LAYERED`（不透明度那一套），DWM 合成时用的是「上一次抓到的
///    位图」。改尺寸的那一瞬间它手里只有上一帧，于是会先把那张旧图按新尺寸拉伸着
///    顶着 —— 看到的就是「菜单被切掉一截、下面一片空白」。
/// 2. 可见窗口改尺寸走的是 `window.resize()`（异步 `SetWindowPos`），它落地之前
///    画出来的那一帧还是旧尺寸的排版；而尺寸变化本身又未必再触发一次重画。
///
/// 所以 ~16ms 补一帧（顺便兜底校尺寸），~48ms 再补一帧（等异步 resize 落地后的
/// 排版）。`was_hidden` 的窗口在 16ms 那帧之后才恢复 alpha：点亮之后到新位图合成
/// 上来之间，露出来的可能正是那张拉伸的旧帧。
fn settle_frame(
    cx: &mut App,
    handle: AnyWindowHandle,
    hwnd: isize,
    was_hidden: bool,
    width: f32,
    height: f32,
    scale: f32,
) {
    let draw = move |cx: &mut gpui::AsyncApp, resize: bool| {
        let _ = cx.update(|app| {
            let _ = handle.update(app, |_, window, _| {
                if resize {
                    // 可见窗口只能让 GPUI 自己去改：同步 `SetWindowPos` 会让它的
                    // viewport 停在旧值，内容按旧高度排版（见 `place` 里的说明）。
                    window.resize(size(px(width), px(height)));
                }
                window.refresh();
            });
        });
    };
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(16))
            .await;
        let mismatch = win32::client_size(hwnd).is_some_and(|(client_w, client_h)| {
            (client_w as f32 / scale - width).abs() > 0.5
                || (client_h as f32 / scale - height).abs() > 1.0
        });
        draw(cx, mismatch);
        if was_hidden {
            win32::set_window_opacity(hwnd, layout::menu_opacity());
        }
        // 第二帧：等异步 resize 落地，让内容按新高度重新排一次
        cx.background_executor()
            .timer(Duration::from_millis(32))
            .await;
        draw(cx, false);
    })
    .detach();
}

fn park(slot: &mut Slot) {    if !slot.on_screen {
        return;
    }
    if let Some(hwnd) = slot.hwnd {
        win32::hide_window(hwnd);
    }
    slot.on_screen = false;
}

/// 建一个列窗口。`at` 为 None 时建在屏幕内的小位置再立刻隐藏（预热）。
fn create_slot(
    cx: &mut App,
    state: Entity<MenuState>,
    level: usize,
    scale: f32,
    at: Option<(f32, f32, f32, f32)>,
) -> Option<()> {
    // 注意：不能给"屏幕外"的初始 bounds——GPUI 会把整块窗口判定为无效，
    // 换成显示器默认尺寸（实测 1440×753）并**居中显示出来**。
    // 而 `x - FRAME_INSET` 在 x=0 时正好是负的：所以兜底位置从 FRAME_INSET 起。
    let (x, y, width, height) = at.unwrap_or((
        FRAME_INSET,
        0.0,
        layout::panel_width(),
        INITIAL_HEIGHT,
    ));
    let bounds = Bounds::new(
        point(px(x - FRAME_INSET), px(y)),
        size(px(width), px(height)),
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        window_decorations: Some(WindowDecorations::Client),
        kind: WindowKind::PopUp,
        focus: false,
        // **一律藏起来建**：我们给的 bounds 这时并没有落到系统窗口上（GPUI 用
        // `CW_USEDEFAULT` 建窗，真实窗口是整块屏幕那么大），建出来就显示的话
        // 必然先闪一帧大白板。窗口交给 `place()` 显示 —— 它会先把几何同步落好、
        // 画好第一帧，再把 alpha 恢复成正常值。
        show: false,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };

    // 「创建前后差集」拿到新窗口的 HWND（GPUI 不暴露裸句柄）
    let before = win32::zed_window_handles();
    let handle = cx
        .open_window(options, |_window, cx| {
            cx.new(|cx| ColumnView::new(state, level, scale, cx))
        })
        .ok()?;
    let hwnd = win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        handle
            .update(cx, |view, _window, cx| {
                view.attach_window(hwnd);
                cx.notify();
            })
            .ok();
        win32::set_rounded_corners(hwnd, scale);
        // GPUI 建窗走 `CW_USEDEFAULT`，我们给的 bounds 这时还没落到窗口上 ——
        // 真实窗口是「整块屏幕」那么大（一张 1424×744 的交换链 + 缓冲 ≈ 8 MB）。
        // 趁它还没显示、还没画过，先收成目标尺寸（预热路径就只剩一个小方块）。
        win32::fit_client_size(hwnd, width, height, scale);
        // 不透明度不在这里管：预热路径下面自己会设，正常路径由 `place()` 负责
        // （显示前 alpha=0 → 画好第一帧 → 再恢复），任何中间帧都不会露给用户。
        if at.is_none() {
            // 预热：**在整个窗口 alpha=0 的情况下**先显示一次、刷一帧，再把窗口藏起来。
            //
            // 为什么非要多这一步：GPUI 的渲染器（管线 + swapchain）是「第一次画」的
            // 时候才真正建起来的，那一下要两三百毫秒。如果一直藏着不画，这笔账就会
            // 记在**第一次弹菜单**上 —— 菜单看着就像卡了一下。
            // 而这条预热路径 alpha 是 0，用户什么都看不见，等于免费把渲染器点着了。
            win32::set_window_opacity(hwnd, 0.0);
            win32::show_window(hwnd);
            handle
                .update(cx, |_, window, _| window.refresh())
                .ok();
            win32::hide_window(hwnd);
            // 藏好之后把不透明度设回去，下次真正显示时就是对的
            win32::set_window_opacity(hwnd, layout::menu_opacity());
        }
    }

    cx.update_global(|host: &mut MenuHost, _app: &mut App| {
        host.slots.push(Slot {
            handle: handle.into(),
            hwnd,
            x,
            y,
            width: at.map(|(_, _, w, _)| w).unwrap_or_else(layout::panel_width),
            height,
            // 建的时候就摆好了（at 有值）→ 它已经在屏幕上，不用再走一遍「显示」
            on_screen: at.is_some(),
            revision: 0,
        });
    });
    Some(())
}

// ---------------------------------------------------------------------------
// 键盘
// ---------------------------------------------------------------------------

/// 一个按键对菜单做了什么。
///
/// 抽成枚举 + 纯函数，方向键的语义（尤其是「在根层按 ← 不能关菜单」）就能靠
/// 单测钉住，不用真的开一堆窗口去点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOutcome {
    /// 没变化（例如已经在根层还按 ←）
    Idle,
    /// 选中项 / 列数变了，需要重摆窗口
    Changed,
    /// 执行当前项
    Activate,
    /// 收起整个菜单
    Close,
}

fn moved(changed: bool) -> KeyOutcome {
    if changed {
        KeyOutcome::Changed
    } else {
        KeyOutcome::Idle
    }
}

/// 把 `key` 应用到菜单状态上，返回后续该做什么。
///
/// 语义对齐系统菜单：↑↓ 在同列里换行（跳过不可选项、到边界停住并滚动），
/// → 进子菜单，← 退一层，Esc 退一层、已经在根层才关菜单。
/// **← 永远不会关菜单** —— 之前「按两下左方向键菜单就没了」就是这里漏了。
/// 单个数字/字母 = 加速键，直接选到对应的那一项（有子菜单就展开，否则执行）。
fn apply_key(state: &mut MenuState, key: &str, card_height: f32) -> KeyOutcome {
    match key {
        "down" | "arrowdown" => moved(state.move_selection(1, card_height)),
        "up" | "arrowup" => moved(state.move_selection(-1, card_height)),
        "right" | "arrowright" => moved(state.expand()),
        "left" | "arrowleft" => moved(state.collapse()),
        "enter" | "return" => KeyOutcome::Activate,
        "escape" | "esc" => {
            if state.collapse() {
                KeyOutcome::Changed
            } else {
                KeyOutcome::Close
            }
        }
        other => {
            // 加速键：选中之后交给 activate() 决定展开还是执行
            if state.select_by_accel(other) {
                KeyOutcome::Activate
            } else {
                KeyOutcome::Idle
            }
        }
    }
}

fn on_key(event: &KeystrokeEvent, window: &mut gpui::Window, cx: &mut App) {
    let keystroke = &event.keystroke;
    // Ctrl / Alt 组合留给全局热键和系统，别在菜单里吃掉
    if keystroke.modifiers.control || keystroke.modifiers.alt {
        return;
    }
    let key = keystroke.key.as_str();

    // 搜索框真的拿到焦点时，字符 / 退格都归它自己（中文输入法也走它），
    // 这边只留下「菜单导航」那几个键 —— 否则同一键会被处理两遍。
    let focused = layout::search_place().enabled()
        && cx
            .try_global::<MenuHost>()
            .map(|host| {
                let handle = host.search.read(cx).focus_handle(cx);
                handle.is_focused(window)
            })
            .unwrap_or(false);
    if focused {
        let input = cx.global::<MenuHost>().search.clone();
        match key {
            // 方向键 / 翻页：菜单导航（选中项移动），搜索框保持焦点
            "up" | "down" | "pageup" | "pagedown" => handle_key(key, cx),
            // 回车：搜索框聚焦时**绝不**执行菜单项。
            //
            // 中文输入法下，回车是用来「确认候选词」的 —— 组字还没上屏时按回车，
            // 如果把菜单项执行了（或把菜单关了），就是用户报的那个 bug。
            // 输入法组字期间（有候选），回车本该由 IME 自己吃掉确认候选；
            // 即便组字已经上屏、marked_range 已经清空，这时的回车也只该「结束搜索」，
            // 不该误伤菜单。所以这里一律不执行，交给输入框 / IME 去消化。
            "enter" | "return" => {
                // 正在组字：让 IME 收下这次回车（什么都不做，事件继续传播给输入框）
                let composing = input.read(cx).is_composing();
                if !composing {
                    // 没在组字：回车 = 确认搜索。把焦点交回菜单本身，
                    // 之后方向键继续导航、再回车才是执行（和没有搜索框时一致）。
                    // 这里先不动选中项，只让「回车」不再误执行。
                }
            }
            // 退格 / 删除：删搜索框里光标前一个字符
            "backspace" | "delete" => {
                input.update(cx, |input, cx| input.delete_backward(cx));
            }
            "escape" => {
                let some_word = !cx
                    .global::<MenuHost>()
                    .state
                    .read(cx)
                    .query()
                    .trim()
                    .is_empty();
                if some_word {
                    // 有词先清词（菜单不关）：清输入框，它一空 query 也跟着空
                    input.update(cx, |input, cx| input.set_text("", cx));
                } else {
                    handle_key("escape", cx);
                }
            }
            // 其它键（可打印字符等）：直接写进搜索框。
            //
            // 正常路径是 WM_CHAR → input_handler → TextInput，但菜单窗口是 PopUp、
            // 又经常在抢前台，那条路径偶尔到不了。这里在拦截器里兜底，
            // 只要能识别出「打出来的那个字符」就喂给搜索框，保证一定能输入。
            _ => {
                if let Some(ch) = typed_char(key) {
                    input.update(cx, |input, cx| input.insert_text(&ch.to_string(), cx));
                }
            }
        }
        return;
    }
    handle_key(key, cx);
}

/// `on_key` 的实体部分（自检脚本也走这里，保证测的就是真实路径）。
fn handle_key(key: &str, cx: &mut App) {
    let Some((state, heights)) = cx.try_global::<MenuHost>().and_then(|host| {
        host.live
            .as_ref()
            .map(|live| (host.state.clone(), live.heights.clone()))
    }) else {
        return;
    };

    // 搜索框开着的时候，可打印字符先进搜索框（不进加速键那条路）
    if layout::search_place().enabled() {
        if let Some(handled) = search_key(&state, key, cx) {
            let _ = handled;
            return;
        }
    }

    let level = state.read(cx).columns().saturating_sub(1);
    // 当前列的可视高度（还没 sync 过就先用一个保守值）
    let card_height = heights
        .get(level)
        .copied()
        .unwrap_or(layout::min_column_height(level));

    let outcome = state.update(cx, |state, cx| {
        let outcome = apply_key(state, key, card_height);
        if outcome == KeyOutcome::Changed {
            cx.notify();
        }
        outcome
    });

    match outcome {
        KeyOutcome::Close => close_all(cx),
        KeyOutcome::Activate => match state.update(cx, |state, _cx| state.activate()) {
            Some((id, label, action)) => finish(id, label, action, cx),
            None => sync(cx),
        },
        // 键盘移动后列高可能变（子菜单展开/收起），sync 会重新摆窗口
        KeyOutcome::Changed => sync(cx),
        KeyOutcome::Idle => {}
    }
}

/// 搜索框吃掉的键：普通字符 / 空格 / 退格 / Esc（先清词）。
///
/// 返回 `Some(())` = 这个键归搜索框了，别再去当加速键；
/// `None` = 不归它（方向键、Enter、菜单没开搜索框时的普通键）。
fn search_key(state: &Entity<MenuState>, key: &str, cx: &mut App) -> Option<()> {
    match key {
        "backspace" | "delete" => {
            let changed = state.update(cx, |state, cx| {
                let mut query = state.query().to_string();
                if !query.pop().is_some() {
                    return false;
                }
                state.set_query(query);
                cx.notify();
                true
            });
            if changed {
                sync(cx);
            }
            Some(())
        }
        "escape" => {
            // 有词先清词（菜单不关）；没词就让上层去收菜单
            let non_empty = state.read(cx).query().trim().to_string();
            if non_empty.is_empty() {
                return None;
            }
            state.update(cx, |state, cx| {
                state.set_query(String::new());
                cx.notify();
            });
            sync(cx);
            Some(())
        }
        other => {
            let ch = typed_char(other)?;
            state.update(cx, |state, cx| {
                let mut query = state.query().to_string();
                query.push(ch);
                state.set_query(query);
                cx.notify();
            });
            sync(cx);
            Some(())
        }
    }
}

/// 一个按键对应的「打出来的那个字符」。
///
/// GPUI 给的是按键名：字母/数字就是它自己，空格是 `space`，
/// 其余（`up` / `enter` / `f5` 这类）都不是能打出来的字符。
fn typed_char(key: &str) -> Option<char> {
    if key == "space" {
        return Some(' ');
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if !ch.is_control() && ch.is_ascii() => Some(ch),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 失焦关闭
// ---------------------------------------------------------------------------

/// 把焦点留在菜单里。
///
/// 场景：用鼠标点进第 2 列、再按 ← 收起 —— 第 2 列的窗口被 `SW_HIDE` 藏起来时，
/// 它正好是前台窗口，Windows 会把前台交给本进程的另一个窗口（或者干脆变成 0，
/// 更糟的是交给别的程序）。后两种都会让 watchdog 觉得「用户跑去点别的窗口了」，
/// 于是整个菜单被收掉 —— 这就是「按两下 ← 菜单自己关了」。
///
/// `sync()` 里已经尽量在藏之前交棒；这里再兜一次底：只要前台不在菜单里，
/// 就把它拉回还开着的最上面那一列。
fn keep_focus_inside(cx: &mut App) {
    let (foreground, ours, top) = {
        let Some(host) = cx.try_global::<MenuHost>() else {
            return;
        };
        let ours: Vec<isize> = host
            .slots
            .iter()
            .filter(|slot| slot.on_screen)
            .filter_map(|slot| slot.hwnd)
            .collect();
        let top = host
            .slots
            .iter()
            .rev()
            .find(|slot| slot.on_screen)
            .and_then(|slot| slot.hwnd);
        (win32::foreground_window(), ours, top)
    };
    let Some(top) = top else {
        return;
    };
    // 前台本来就在菜单的某一列上：别去动它，免得每次悬停都换一次前台。
    if ours.contains(&foreground) {
        return;
    }
    win32::set_foreground(top);
}

/// 当前前台窗口 + 开着的菜单窗口列表 + 上一次窗口扰动的时间 + 是否「保持显示」
/// （异步上下文里用）。
fn focus_snapshot(
    cx: &mut gpui::AsyncApp,
) -> Option<(isize, Vec<isize>, Option<std::time::Instant>, bool)> {
    cx.update(|app| {
        let host = app.try_global::<MenuHost>()?;
        Some((
            win32::foreground_window(),
            host.slots
                .iter()
                .filter(|slot| slot.on_screen)
                .filter_map(|slot| slot.hwnd)
                .collect(),
            host.grace,
            host.pinned,
        ))
    })
    .ok()
    .flatten()
}

fn spawn_watchdog(cx: &mut App) {
    if !CLOSE_ON_FOCUS_LOST {
        return;
    }
    cx.spawn(async move |cx| {
        // 先等某一列真的拿到前台（最多 2 秒）。**拿不到也别放弃**：菜单是
        // TOPMOST + `SW_SHOWNOACTIVATE` 显示的，系统偶尔就是不给前台
        // （`SetForegroundWindow` 被拒），这时候「失焦」这条路整个失效 ——
        // 用户点菜单外面，菜单一动不动。所以这里记下开菜单那一刻的前台，
        // 之后靠「前台换了个人」和「鼠标按下的计数」两条路兜住。
        let mut activated = false;
        let mut start_fg = None;
        for _ in 0..20 {
            let Some((foreground, ours, _, pinned)) = focus_snapshot(cx) else {
                return;
            };
            if pinned {
                return;
            }
            if ours.contains(&foreground) {
                activated = true;
                break;
            }
            if start_fg.is_none() && foreground != 0 {
                start_fg = Some(foreground);
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        }

        // 钩子记的「鼠标被按了几下」。低层鼠标钩子比 60ms 轮询可靠：
        // 一下很快的点击（按住不到 60ms）用 `GetAsyncKeyState` 是捞不到的。
        let mut clicks = crate::win32::click_tick();
        let mut misses = 0u8;
        loop {
            let Some((foreground, ours, grace, pinned)) = focus_snapshot(cx) else {
                return;
            };
            // 刚显示/藏过窗口：这几百毫秒里的失焦不算「用户点走了」
            let settled = grace.map_or(true, |at| at.elapsed() >= FOCUS_GRACE);
            // 「保持显示」开着：焦点跑哪儿都不收菜单（调外观时就是要点别处）
            if pinned {
                return;
            }
            if !settled {
                clicks = crate::win32::click_tick();
                misses = 0;
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                continue;
            }

            // 1) 点在菜单外面：最可靠的一条 —— 又按了一下鼠标，而光标不在我们
            //    任何一个窗口上（菜单列、提示卡、设置窗口都算「里面」）。
            //    不依赖前台有没有变，所以「菜单根本没抢到前台」时照样能收。
            let now_clicks = crate::win32::click_tick();
            if now_clicks != clicks && !crate::win32::cursor_over_self() {
                let _ = cx.update(|app| close_all(app));
                return;
            }
            clicks = now_clicks;

            // 2) 失焦：抢到过前台又丢了，或者压根没抢到、但前台已经换了个人
            let away = foreground != 0 && !ours.contains(&foreground);
            let left = away && (activated || start_fg.is_some_and(|start| start != foreground));
            if left {
                misses += 1;
                if misses >= 2 {
                    let _ = cx.update(|app| close_all(app));
                    return;
                }
            } else {
                misses = 0;
            }
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{apply_key, column_steps, column_targets, KeyOutcome};
    use crate::layout::{self, SCREEN_MARGIN};
    use crate::menu::{item, sep, MenuEntry, MenuItem};
    use crate::state::MenuState;

    const SCREEN: (f32, f32, f32, f32) = (0.0, 0.0, 1920.0, 1040.0);
    /// 测试里给的「列可视高度」，够高所以不会触发滚动
    const CARD: f32 = 400.0;

    /// 三列：高度 212 / 143 / 164，子列相对父列行区顶部的偏移都是 106。
    /// 最后两个字段是「子菜单从父列左边多少像素处起」：往右时是父项那一列的
    /// 右边缘，翻到左边时是它那一列的左边缘。单列时 = 这一列的宽度 / 0。
    fn steps() -> Vec<(f32, f32, f32, f32, f32)> {
        let w = layout::panel_width();
        vec![
            (212.0, 0.0, w, w, 0.0),
            (143.0, 106.0, w, w, 0.0),
            (164.0, 106.0, w, w, 0.0),
        ]
    }

    #[test]
    fn normal_cascade_goes_right() {
        let t = column_targets((100.0, 100.0), SCREEN, &steps());
        assert_eq!(t.len(), 3);
        assert_eq!((t[0].0, t[0].1, t[0].2), (100.0, 100.0, 212.0));
        // 子菜单的第一行和父项那一行齐平：卡片顶边 = 父行顶边 - 行区上方的留白
        let aligned = |anchor: f32, level: usize| anchor - layout::row_top(level);
        assert_eq!(
            (t[1].0, t[1].1, t[1].2),
            (100.0 + layout::panel_width(), aligned(206.0, 1), 143.0)
        );
        assert_eq!(
            (t[2].0, t[2].1, t[2].2),
            (
                100.0 + 2.0 * layout::panel_width(),
                aligned(aligned(206.0, 1) + 106.0, 2),
                164.0
            )
        );
        // 第一行确实落在父项那一行上（差一像素以内）
        let parent_row_top = 100.0 + 106.0;
        assert!((t[1].1 + layout::row_top(1) - parent_row_top).abs() < 0.5);
    }

    /// 多列菜单：子菜单从**父项那一列**的右边缘出来（跟系统右键菜单一样，贴着名字），
    /// 盖住右边并排的列也无所谓。
    #[test]
    fn a_submenu_opens_right_beside_the_item() {
        let w = layout::panel_width();
        let span = w * 2.0 + layout::COLUMN_GAP; // 两列并排的卡片
        // 父项在第 1 段（多列卡片里的左边那一列）：子菜单从第 1 段右边起
        let steps = vec![(300.0, 0.0, span, w, 0.0), (200.0, 40.0, w, w, 0.0)];
        let t = column_targets((100.0, 100.0), SCREEN, &steps);
        assert_eq!(t[1].0, 100.0 + w);
        assert!(t[1].0 < t[0].0 + t[0].2, "子菜单要贴在名字旁边，不跨过整张卡片");
    }

    /// 翻到左边时贴的是**父项那一列的左边缘**：父项在卡片第 2 段时，子菜单
    /// 的右边就在第 2 段的左边，不是跑到整张卡片外面去。
    #[test]
    fn flipping_left_hugs_the_items_own_column() {
        let w = 200.0;
        let gap = layout::COLUMN_GAP;
        let span = w * 3.0 + gap * 2.0;
        // 父项落在第 3 段：右边缘 = 2*(w+gap)+w，左边缘 = 2*(w+gap)
        let steps = vec![
            (300.0, 0.0, span, 2.0 * (w + gap) + w, 2.0 * (w + gap)),
            (200.0, 40.0, w, w, 0.0),
        ];
        // 贴着屏幕右边开，右边肯定放不下 → 翻左
        let t = column_targets((1920.0 - span - 40.0, 100.0), SCREEN, &steps);
        assert_eq!(t[1].0 + w, t[0].0 + 2.0 * (w + gap));
        assert!(t[1].0 + w < t[0].0 + span, "翻左也在卡片里面，不能贴整张卡片的左边");
    }

    #[test]
    fn near_right_edge_flips_left() {
        let origin_x = 1900.0f32.min(1920.0 - layout::panel_width());
        let t = column_targets((origin_x, 100.0), SCREEN, &steps());
        assert_eq!(t[0].0, 1920.0 - layout::panel_width());
        assert_eq!(t[1].0, 1920.0 - 2.0 * layout::panel_width());
        assert_eq!(t[2].0, 1920.0 - 3.0 * layout::panel_width());
        for (x, _, _, _) in &t {
            assert!(*x >= 0.0 && *x + layout::panel_width() <= 1920.0);
        }
    }

    #[test]
    fn tall_column_fills_screen_then_scrolls() {
        // 一列 1000 条：优先把屏幕高度用满，从屏幕顶部开始，剩下的靠滚动
        let w = layout::panel_width();
        let t = column_targets((100.0, 600.0), SCREEN, &[(30000.0, 0.0, w, w, 0.0)]);
        let (_, y, h, _) = t[0];
        assert_eq!(y, 0.0);
        assert_eq!(h, 1040.0 - SCREEN_MARGIN);
    }

    #[test]
    fn short_column_stays_at_cursor() {
        // 内容比屏幕矮：就贴着光标，不上移也不用滚动
        let w = layout::panel_width();
        let t = column_targets((100.0, 600.0), SCREEN, &[(212.0, 0.0, w, w, 0.0)]);
        assert_eq!((t[0].1, t[0].2), (600.0, 212.0));
    }

    /// 首行和父项那一行齐平（能对齐就对齐）。
    #[test]
    fn a_submenu_starts_level_with_its_row() {
        let w = layout::panel_width();
        let anchor = 300.0; // 父项那一行的顶边
        let steps = vec![(400.0, 0.0, w, w, 0.0), (200.0, anchor - 100.0, w, w, 0.0)];
        let t = column_targets((100.0, 100.0), SCREEN, &steps);
        let (_, y, h, _) = t[1];
        assert!((y + layout::row_top(1) - anchor).abs() < 0.5, "顶边对齐父项那一行");
        assert_eq!(h, 200.0);
    }

    /// 下方放不下就**整张往上挪**（往上长）：最后一行跟父项那一行齐平，
    /// 高度一个像素都不缩。这是原版 RunAny 的手感。
    #[test]
    fn a_long_submenu_grows_up_wards_instead_of_scrolling() {
        let w = layout::panel_width();
        let steps = vec![(400.0, 0.0, w, w, 0.0), (400.0, 700.0, w, w, 0.0)];
        let t = column_targets((100.0, 100.0), SCREEN, &steps);
        let (_, y, h, _) = t[1];
        assert_eq!(h, 400.0, "整张 400 都得留着，不能切成半截");
        // 首行对齐的话，底边 800+400 会超到屏幕外 —— 所以应该往上长
        assert!(y < 100.0 + 700.0 - layout::row_top(1), "不是首行对齐");
        // 父项那一行的底边 = 100（卡片顶）+ 700（偏移）+ 一行
        let row_bottom = 100.0 + 700.0 + layout::row_height();
        let card_bottom = y + h - layout::row_bottom_edge(1);
        assert!(
            (card_bottom - row_bottom).abs() < 0.5,
            "最后一行要跟父项齐平：底边 {card_bottom} vs 父项底边 {row_bottom}"
        );
        assert!(y > 0.0, "是往上挪，不是贴顶铺满");
    }

    /// 比屏幕还高：这才允许截到屏幕高、内部滚动，并且贴顶铺满。
    #[test]
    fn only_a_submenu_taller_than_the_screen_scrolls() {
        let w = layout::panel_width();
        let steps = vec![(400.0, 0.0, w, w, 0.0), (5000.0, 300.0, w, w, 0.0)];
        let t = column_targets((100.0, 100.0), SCREEN, &steps);
        let (_, y, h, _) = t[1];
        assert_eq!(y, 0.0);
        assert_eq!(h, 1040.0 - SCREEN_MARGIN);
    }

    /// 父项贴着屏幕底：整张还是 300，往上长到「最后一行跟父项齐平」，
    /// 而不是硬贴屏幕底（那样最后一项会跑到父项下面去）。
    #[test]
    fn a_submenu_at_the_bottom_slides_up_whole() {
        let w = layout::panel_width();
        let steps = vec![(400.0, 0.0, w, w, 0.0), (300.0, 900.0, w, w, 0.0)];
        let t = column_targets((100.0, 100.0), SCREEN, &steps);
        let (_, y, h, _) = t[1];
        assert_eq!(h, 300.0);
        let row_bottom = 100.0 + 900.0 + layout::row_height();
        let card_bottom = y + h - layout::row_bottom_edge(1);
        assert!(
            (card_bottom - row_bottom).abs() < 0.5,
            "最后一行跟父项齐平：{card_bottom} vs {row_bottom}"
        );
        assert!(y + h <= 1040.0, "整张还是得在屏幕里");
    }

    /// 右列（多列卡片里的第 2 段）的子菜单，必须对齐**父项自己那一行**。
    ///
    /// 以前这里拿子菜单的条目去数父项的索引：父项在根列表里是第 6 项、
    /// 子菜单只有 2 项时，偏移就成了「6 行」，子菜单整张往下掉一大截
    /// （用户报的「双列菜单右列子菜单展开位置还是不对」）。
    #[test]
    fn a_submenu_of_the_second_column_starts_at_its_own_row() {
        let menu = vec![
            item("a", "A"),
            item("b", "B"),
            item("c", "C"),
            MenuEntry::ColumnBreak,
            item("d", "D"),
            MenuEntry::Item(
                MenuItem::new("e", "E").submenu(vec![item("e1", "E1"), item("e2", "E2")]),
            ),
        ];
        let mut state = MenuState::new(menu);
        // 选中第 2 段第 2 项（根列表索引 5），展开它的子菜单
        state.path = vec![5, 0];
        state.scroll = vec![0.0, 0.0];
        let steps = column_steps(&state, &layout::estimate_width);
        assert_eq!(steps.len(), 2);
        // 父项那一行相对父卡片顶边 = 行区顶部 + 1 行（它上面只有 D）
        let want = layout::row_top(0) + layout::row_height();
        assert!(
            (steps[1].1 - want).abs() < 0.5,
            "offset 要按父项那一行算：{} vs {want}",
            steps[1].1
        );
        // 摆成窗口坐标后，子菜单第一行正好压在父项那一行上
        let targets = column_targets((100.0, 100.0), SCREEN, &steps);
        let parent_row_y = 100.0 + want;
        assert!((targets[1].1 + layout::row_top(1) - parent_row_y).abs() < 0.5);
    }

    // -- 键盘语义 ---------------------------------------------------------

    /// 新建 / 最近打开(有子菜单) / ─── / 退出
    fn tree() -> Vec<MenuEntry> {
        vec![
            item("new", "新建文件"),
            MenuEntry::Item(
                MenuItem::new("recent", "最近打开")
                    .submenu(vec![item("a", "打开 A"), item("b", "打开 B")]),
            ),
            sep(),
            item("quit", "退出"),
        ]
    }

    fn state() -> MenuState {
        MenuState::new(tree())
    }

    #[test]
    fn left_at_root_never_closes_the_menu() {
        let mut state = state();
        assert_eq!(apply_key(&mut state, "left", CARD), KeyOutcome::Idle);
        assert_eq!(apply_key(&mut state, "left", CARD), KeyOutcome::Idle);
        assert_eq!(apply_key(&mut state, "arrowleft", CARD), KeyOutcome::Idle);
        assert_eq!(state.columns(), 1);
    }

    #[test]
    fn right_then_left_twice_only_leaves_the_submenu() {
        // 用户报的那个：→ 进子菜单，再按两下 ← 菜单整个关掉
        let mut state = state();
        assert_eq!(apply_key(&mut state, "down", CARD), KeyOutcome::Changed);
        assert_eq!(apply_key(&mut state, "right", CARD), KeyOutcome::Changed);
        assert_eq!(state.columns(), 2);
        assert_eq!(apply_key(&mut state, "left", CARD), KeyOutcome::Changed);
        assert_eq!(state.columns(), 1);
        // 第二下、第三下都停在根层，不能关
        assert_eq!(apply_key(&mut state, "left", CARD), KeyOutcome::Idle);
        assert_eq!(apply_key(&mut state, "left", CARD), KeyOutcome::Idle);
        assert_eq!(state.columns(), 1);
    }

    #[test]
    fn escape_goes_up_one_level_then_closes() {
        let mut state = state();
        apply_key(&mut state, "down", CARD);
        apply_key(&mut state, "right", CARD);
        assert_eq!(apply_key(&mut state, "escape", CARD), KeyOutcome::Changed);
        assert_eq!(state.columns(), 1);
        assert_eq!(apply_key(&mut state, "escape", CARD), KeyOutcome::Close);
    }

    #[test]
    fn arrows_skip_separators_and_stop_at_the_edges() {
        let mut state = state();
        // 新建文件 → 最近打开
        assert_eq!(apply_key(&mut state, "down", CARD), KeyOutcome::Changed);
        assert_eq!(state.path[0], 1);
        // 最近打开 →（跳过分隔线）退出
        assert_eq!(apply_key(&mut state, "down", CARD), KeyOutcome::Changed);
        assert_eq!(state.path[0], 3);
        // 到底是停住，不是循环
        assert_eq!(apply_key(&mut state, "down", CARD), KeyOutcome::Idle);
        assert_eq!(state.path[0], 3);
        assert_eq!(apply_key(&mut state, "up", CARD), KeyOutcome::Changed);
        assert_eq!(state.path[0], 1);
    }

    #[test]
    fn right_without_children_is_a_no_op() {
        let mut state = state();
        // 「新建文件」没有子菜单：按 → 什么都不该发生（更不能关菜单）
        assert_eq!(apply_key(&mut state, "right", CARD), KeyOutcome::Idle);
        assert_eq!(state.columns(), 1);
    }

    #[test]
    fn enter_on_a_parent_expands_instead_of_closing() {
        let mut state = state();
        apply_key(&mut state, "down", CARD);
        assert_eq!(apply_key(&mut state, "enter", CARD), KeyOutcome::Activate);
        // activate() 对父项是「展开」，返回 None，菜单留在原地
        assert!(state.activate().is_none());
        assert_eq!(state.columns(), 2);
    }

    /// 带加速键的菜单：N=新建 Q=退出 R=最近打开（有子菜单）
    fn accel_tree() -> Vec<MenuEntry> {
        vec![
            MenuEntry::Item(MenuItem::new("new", "新建文件").accel("N")),
            MenuEntry::Item(MenuItem::new("recent", "最近打开").accel("R").submenu(vec![
                MenuEntry::Item(MenuItem::new("a", "打开 A").accel("A")),
                MenuEntry::Item(MenuItem::new("b", "打开 B")),
            ])),
            MenuEntry::Item(MenuItem::new("quit", "退出").accel("Q")),
        ]
    }

    #[test]
    fn accelerator_selects_and_fires_the_item() {
        let mut state = MenuState::new(accel_tree());
        // 小写也认，选中之后是 Activate（叶子 = 执行）
        assert_eq!(apply_key(&mut state, "q", CARD), KeyOutcome::Activate);
        assert_eq!(state.path, vec![2]);
        let fired = state.activate().expect("叶子项应该返回动作");
        assert_eq!(fired.0.as_ref(), "quit");
    }

    #[test]
    fn accelerator_on_a_parent_expands_the_submenu() {
        let mut state = MenuState::new(accel_tree());
        assert_eq!(apply_key(&mut state, "r", CARD), KeyOutcome::Activate);
        // activate() 对父项只是展开
        assert!(state.activate().is_none());
        assert_eq!(state.columns(), 2);
        // 展开之后，子项的加速键也能用
        assert_eq!(apply_key(&mut state, "a", CARD), KeyOutcome::Activate);
        assert_eq!(state.path, vec![1, 0]);
    }

    #[test]
    fn unknown_accelerator_changes_nothing() {
        let mut state = MenuState::new(accel_tree());
        for key in ["z", "1", "space", "f5"] {
            assert_eq!(apply_key(&mut state, key, CARD), KeyOutcome::Idle);
        }
        assert_eq!(state.path, vec![0]);
    }
}
