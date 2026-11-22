//! 热字符串：在**任何程序**里敲一小串字，就执行绑定的菜单项。
//!
//! 就是 AutoHotkey 的 `::ftw::Free the whales`：敲 `ftw` 再敲一个结束字符
//! （空格 / Tab / 回车 / `.` `,` 这类），那三个字母被删掉、换成这一项要做的事。
//! 「要做的事」直接复用菜单项的动作链，所以热字符串既能当缩写展开
//! （动作用「发送文本」），也能用来启动程序、发送按键。
//!
//! ## 为什么是低层键盘钩子
//!
//! 热字符串要「看见」用户在别的程序里打的字，唯一可靠的办法是
//! `WH_KEYBOARD_LL`：它在按键送到目标窗口**之前**被调用，所以我们既能观察，
//! 也能在命中时把这个键吞掉（Tab / 回车当结束符时要吞，不然目标程序已经跳走了，
//! 退格删不回来）。
//!
//! 钩子回调在装了钩子的那个线程上跑，而且**每个按键都要走一趟**，
//! 所以这里的原则是：能少做就少做。回调里不碰文件、不查索引、不打日志，
//! 甚至连字符都不格式化 —— 只有真的命中才把活干完。
//!
//! ## 输入法
//!
//! 中文输入法开着的时候，字母其实进了输入法的组字缓冲区，目标程序一个字都没收到。
//! 早先的做法是「检测到中文模式就整段跳过」，代价是**只要输入法停在中文就再也
//! 触发不了热字符串**——而中文用户基本不会为了敲缩写去切英文。
//!
//! 现在改成按**拼音**匹配：钩子照样能看见那些字母键，把它们记进缓冲区去和短语比；
//! 命中之后发 N 个退格——删的是输入法组字缓冲里的拼音字母（一个退格删一个），
//! 删干净了再跑动作。两个额外的保护：
//!
//! - 组字期间按**数字**是在选候选词，字已经上屏了，这一轮缓冲直接作废；
//! - Tab / 回车在输入法里是「上屏 / 切候选」，照样整段跳过，不抢。
//!
//! 匹配**完全不看输入法状态**：只看你实际按了哪些键（`WH_KEYBOARD_LL` 拿到的是
//! 物理键，输入法再怎么组字也改不了它）。只有「删几个字符」这一步需要知道
//! 有没有组字 —— 数字键在组字区非空时是「选候选」，只上屏一个字。
//!
//! ## 提示
//!
//! 敲到第 N 个字符时在鼠标旁边弹一张小卡片，列出以这几个字开头的短语
//! （`huiz → 你好，世界`）。提示只管「看一眼」，命中与否和它无关。
//! 卡片自己会消失（`hint_ms`），消失之后先安静一会儿 —— 用户还在打同一个词，
//! 每敲一个字都弹一次太吵。等下一次敲结束字符（空格 / 回车 / Tab / 标点）
//! 才重新开始提示，这也是参考界面上那段蓝字说的行为。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use gpui::App;

use crate::settings::{self, ActionKind};
use crate::win32::{self, RawKey};

/// 一条从设置里编译出来的规则。
struct Rule {
    /// 原文（提示里显示这个，保留大小写）
    text: String,
    /// 比较用的键（不区分大小写时是小写）
    key: String,
    id: String,
    /// 菜单项的名称（提示卡片上要写「这是哪一项」）
    label: String,
    /// 提示第一行：展开成什么 / 执行什么
    expand: String,
    /// 提示第二行：启动的完整路径（没有就是空串）
    path: String,
    /// 一匹配就执行，不等结束字符（这一项自己的开关）
    now: bool,
    /// `:C:` —— 这一条区分大小写（AHK 默认不区分）
    case_sensitive: bool,
    /// `:*:` —— 不用结束字符，敲完最后一个字母就触发
    no_end_char: bool,
    /// `:?:` —— 词中间也触发（默认要求缓冲区从头就是这个短语）
    inside_word: bool,
    /// `:O:` —— 不把结束字符（空格 / 回车…）跟着打出去
    omit_end: bool,
    /// `:B0:` —— 不删掉已经敲进去的触发词
    no_backspace: bool,
}

/// AHK 的 `:选项:` 解析结果。
#[derive(Clone, Copy, Default, Debug)]
struct Flags {
    case_sensitive: bool,
    no_end_char: bool,
    inside_word: bool,
    omit_end: bool,
    no_backspace: bool,
}

/// 解析 AHK 的 `:选项:短语::` 写法。
///
/// 支持的选项（大小写都认）：`C` 区分大小写、`*` 不用结束字符、`?` 词中间也触发、
/// `O` 不把结束字符打出去、`B0` 不删除触发词。
/// `R` / `T` / `SI` / `SP` / `B` 接受但不改变行为 —— 我们本来就是直接注入文本，
/// 不经过剪贴板，也没有「发送模式」的差别。
fn parse_syntax(raw: &str) -> (Flags, String) {
    let mut flags = Flags::default();
    let text = raw.trim();
    let Some(rest) = text.strip_prefix(':') else {
        // 也接受 `ftw::` 这种只带结尾的写法
        return (flags, text.trim_end_matches("::").trim().to_string());
    };
    let Some((options, tail)) = rest.split_once(':') else {
        return (flags, text.to_string());
    };
    let upper = options.to_ascii_uppercase();
    for ch in upper.chars() {
        match ch {
            'C' => flags.case_sensitive = true,
            '*' => flags.no_end_char = true,
            '?' => flags.inside_word = true,
            'O' => flags.omit_end = true,
            _ => {}
        }
    }
    if upper.contains("B0") {
        flags.no_backspace = true;
    }
    (flags, tail.trim_end_matches("::").trim().to_string())
}

/// 一次同步编译出来的全部规则 + 全局参数。
///
/// 钩子线程每个按键读一次；换成 `Arc` 是为了读的时候不用一直占着锁
/// （设置界面在改设置时会写，两边不能互相等）。
struct Config {
    rules: Vec<Rule>,
    end: Vec<char>,
    case_sensitive: bool,
    hint_after: usize,
    hint_path_chars: usize,
    hide_hint: bool,
    /// 敲完最后一个字符就执行，不用再按空格 / 回车（AHK 的 `:*:`）
    immediate: bool,
    /// 输入法组字中也尝试匹配（默认关，见 `HotstringSettings::ime_match`）
    ime_match: bool,
}

static CONFIG: RwLock<Option<Arc<Config>>> = RwLock::new(None);
/// 用户到目前为止敲的字符（只在「一段词」里有效，命中 / 结束符 / 切窗口都会清空）
static TYPED: Mutex<String> = Mutex::new(String::new());
/// 提示的「这一轮」编号：旧定时器醒来发现号变了就不能收掉新提示
static HINT_GEN: AtomicU64 = AtomicU64::new(0);
/// 钩子装上了没有（只装一次；装失败下次还能再试）
static HOOK_READY: AtomicBool = AtomicBool::new(false);
/// 输入法是不是中文模式。
///
/// **由后台线程更新，钩子里只读这一个 atomic** —— 早先是每个按键在钩子线程里
/// 同步 `SendMessageTimeout` 问输入法窗口（还问两个窗口），打字快的时候每次
/// 按键都要等几毫秒到二十毫秒，输入明显发涩（搜狗尤其明显）。
static IME_NATIVE: AtomicBool = AtomicBool::new(false);
/// 最近一次按键的时刻（毫秒）：后台线程据此决定还要不要继续问
static LAST_KEY_MS: AtomicU64 = AtomicU64::new(0);
static IME_POLL_STARTED: AtomicBool = AtomicBool::new(false);
/// 这一段输入在**目标程序里已经占了多少个字符**（退格要删这么多次）。
///
/// 中文输入法下按键数和屏幕字符数对不上：`@ma1` 是四个键，屏幕上只有 `@吗` 两个字
/// （`1` 被输入法拿去选候选了）。所以不能按短语长度退格，要按这个数。
static SCREEN: Mutex<usize> = Mutex::new(0);
/// 组字区里**还没上屏**的字母数。
///
/// 中文输入法下 `win` 这三个字母躺在输入法的组字区里，目标程序一个字都还没收到，
/// 所以它们不占屏幕字符。但用户一按数字选候选，这几个字母就会**上屏成真字符** ——
/// 那一刻要把它们补记进 `SCREEN`，否则命中时退格数不够，
/// 屏幕上就会剩下半截触发词（`@win` 删不掉就是这么来的）。
static PENDING: Mutex<usize> = Mutex::new(0);
/// 上一次按键时的「输入目标」（前台窗口 + 焦点控件）：变了就清空缓冲
static LAST_TARGET: Mutex<(isize, isize)> = Mutex::new((0, 0));
/// 这一段词**开始打第一个键时**输入框里的光标位置。
///
/// 命中时再读一次光标，两次相减就是触发词真实上屏的字符数 —— 输入法把 `win`
/// 原样上屏是 3 个字、换成候选词可能是 1 个字，物理按键数两种情况下都一样，
/// 只有光标位移知道真相。问不到（浏览器 / 自绘控件）就退回按物理键推算。
static SEG_CARET: Mutex<Option<usize>> = Mutex::new(None);

/// 提示卡片上的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintLine {
    pub text: String,
    /// 第二行（路径）用小一号的灰字
    pub dim: bool,
    /// 点这一行要执行哪一项（空 = 这一行不可点，比如路径）
    pub id: String,
}

/// 钩子线程 → 界面线程的事件。
enum Event {
    /// 命中：跑这个菜单项。`replay` = 被吞掉的结束键，动作跑完补发。
    ///
    /// `cleanup` 是「输入法还在组字」时要补做的事：取消组字 + 退格这么多次。
    /// 这两步只能在 UI 线程做（`Imm*` 是线程绑定的），所以从钩子里交出来。
    Run {
        id: String,
        replay: Option<Replay>,
        cleanup: Option<usize>,
    },
    /// 提示内容（空 = 收起）
    Hint(Vec<HintLine>),
}

/// 被钩子吞掉的结束字符，动作跑完要补发出去。
///
/// 分两种是因为补发方式不一样：Tab / 回车要的是「跳焦点 / 换行」这个按键行为，
/// 按虚拟键发；空格 / 标点要的是那个字符，按字符发 —— 否则 `:` 会被发成 `;`
/// （虚拟键不带 Shift 状态）。
#[derive(Clone, Copy, Debug)]
pub enum Replay {
    Key(u16),
    Char(char),
}

impl Replay {
    pub fn send(self) {
        match self {
            Replay::Key(vk) => win32::send_virtual_key(vk),
            Replay::Char(ch) => {
                let _ = win32::send_text(&ch.to_string());
            }
        }
    }
}

static EVENTS: OnceLock<async_channel::Sender<Event>> = OnceLock::new();

/// 启动时装一次：起事件循环、按当前设置编译规则、必要时装钩子。
pub fn install(cx: &mut App) {
    let (tx, rx) = async_channel::unbounded::<Event>();
    let _ = EVENTS.set(tx);

    cx.spawn(async move |cx| {
        while let Ok(event) = rx.recv().await {
            match event {
                Event::Run {
                    id,
                    replay,
                    cleanup,
                } => {
                    let _ = cx.update(|app| {
                        crate::popup::run_hotstring(app, &id, replay, cleanup)
                    });
                }
                Event::Hint(lines) => {
                    let _ = cx.update(|app| crate::hint::show(app, lines));
                }
            }
        }
    })
    .detach();

    sync(cx);
}

/// 设置一变就调：重新编译规则。规则从「有」变「没有」时把钩子闲置下来
/// （钩子本身不卸 —— 卸钩子要跨线程握手，而闲置的回调只有一个原子读）。
pub fn sync(cx: &mut App) {
    let _ = cx;
    let (config, active) = rebuild();
    let hide_hint = config.hide_hint;
    if let Ok(mut slot) = CONFIG.write() {
        *slot = Some(Arc::new(config));
    }
    reset_typed();
    if !active || hide_hint {
        crate::hint::hide();
    }
    if active {
        ensure_hook();
    }
}

/// 从当前设置编译一份 `Config`；第二个返回值 = 现在有没有活要干。
fn rebuild() -> (Config, bool) {
    let cfg = settings::read(|settings| settings.hotstrings.clone());
    let phrases = settings::hotstring_phrases();

    let mut rules = Vec::with_capacity(phrases.len());
    for phrase in &phrases {
        let Some(compiled) =
            compile(&phrase.id, &phrase.text, &phrase.label, phrase.now, cfg.case_sensitive)
        else {
            continue;
        };
        rules.push(compiled);
    }
    // 长的优先：`ab` 和 `abcd` 都配了的时候，敲完 abcd 应该命中长的那个
    rules.sort_by(|a, b| b.key.chars().count().cmp(&a.key.chars().count()));

    let active = cfg.enabled && !rules.is_empty();
    let config = Config {
        rules,
        end: cfg.end_char_set(),
        case_sensitive: cfg.case_sensitive,
        hint_after: cfg.hint_after.clamp(1, 12) as usize,
        hint_path_chars: cfg.hint_path_chars as usize,
        hide_hint: cfg.hide_hint || !active,
        immediate: cfg.immediate,
        ime_match: cfg.ime_match,
    };
    (config, active)
}

/// 从菜单项里编译一条规则。
///
/// `None` 只在「短语是空的」时出现 —— 空短语会匹配一切，绝对不能进规则表。
fn compile(
    id: &str,
    text: &str,
    label: &str,
    now: bool,
    case_sensitive: bool,
) -> Option<Rule> {
    let (flags, text) = parse_syntax(text);
    let text = text.as_str();
    if text.is_empty() {
        return None;
    }
    let key = fold(text, case_sensitive || flags.case_sensitive);
    let (expand, path) = settings::read(|settings| {
        let Some(item) = settings.find_path(id).and_then(|path| settings.node(&path)) else {
            return (String::new(), String::new());
        };
        describe(item)
    });
    Some(Rule {
        text: text.to_string(),
        key,
        id: id.to_string(),
        label: label.to_string(),
        expand,
        path,
        now,
        case_sensitive: flags.case_sensitive,
        no_end_char: flags.no_end_char,
        inside_word: flags.inside_word,
        omit_end: flags.omit_end,
        no_backspace: flags.no_backspace,
    })
}

/// 提示里「这一项做什么」的简短说法 + 完整路径。
fn describe(item: &settings::ItemSetting) -> (String, String) {
    if item.is_submenu() {
        return (format!("菜单（{} 项）", item.children.len()), String::new());
    }
    let Some(step) = item
        .actions
        .iter()
        .find(|step| !step.value.trim().is_empty())
    else {
        return (String::new(), String::new());
    };
    let value = step.value.trim();
    match step.kind {
        ActionKind::Text => (clip(value, 46), String::new()),
        ActionKind::Run => {
            let program = first_token(value);
            let path = if value.contains('\\') || value.contains(':') {
                clip(value, 200)
            } else {
                String::new()
            };
            (clip(&program, 46), path)
        }
        ActionKind::Keys => (format!("发送按键 {value}"), String::new()),
        ActionKind::Url => (clip(value, 46), String::new()),
        ActionKind::Builtin => {
            let label = crate::actions::find(value)
                .map(|def| crate::i18n::t(def.name).to_string())
                .or_else(|| {
                    settings::BuiltinAction::from_key(value)
                        .map(|action| action.label().to_string())
                })
                .unwrap_or_else(|| value.to_string());
            (label, String::new())
        }
        ActionKind::Script => {
            let lines = value.lines().filter(|line| !line.trim().is_empty()).count();
            (format!("运行脚本（{lines} 行）"), String::new())
        }
    }
}

/// 一条命令里第一个 token（`"C:\a b\app.exe" --x` → `C:\a b\app.exe`）。
fn first_token(value: &str) -> String {
    let text = value.trim();
    if let Some(rest) = text.strip_prefix('"') {
        return rest.split('"').next().unwrap_or(rest).to_string();
    }
    text.split_whitespace().next().unwrap_or(text).to_string()
}

/// 按字符数截断（中文按 1 个算，路径里的 `…` 不占两格）。
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn config() -> Option<Arc<Config>> {
    CONFIG.read().ok()?.clone()
}

fn reset_typed() {
    if let Ok(mut typed) = TYPED.lock() {
        typed.clear();
    }
    if let Ok(mut screen) = SCREEN.lock() {
        *screen = 0;
    }
    if let Ok(mut pending) = PENDING.lock() {
        *pending = 0;
    }
    if let Ok(mut caret) = SEG_CARET.lock() {
        *caret = None;
    }
}

/// 取「屏幕上这一段输入到底占了多少个字符」，顺便说明刚才是不是还在组字。
///
/// 第二个返回值是「取之前组字区里有没有东西」：有的话命中后得先让输入法把这段
/// 丢掉（否则退格删的是候选窗里的拼音，屏幕上那几个字一个都掉不了）。
fn take_screen() -> (usize, bool) {
    // 组字区那几个字母还没上屏，取消组字就没了，不能算进要删的字符里
    let composing = PENDING
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending) > 0)
        .unwrap_or(false);
    let screen = SCREEN
        .lock()
        .map(|mut screen| std::mem::take(&mut *screen))
        .unwrap_or(0);
    (screen, composing)
}

fn add_screen(count: usize) {
    if let Ok(mut screen) = SCREEN.lock() {
        // 上限：一段「词」再怎么长也不会超过几十个字符，防止异常输入把数撑爆
        *screen = (*screen + count).min(64);
    }
}

fn add_pending(count: usize) {
    if let Ok(mut pending) = PENDING.lock() {
        *pending = (*pending + count).min(48);
    }
}

fn sub_pending() {
    if let Ok(mut pending) = PENDING.lock() {
        *pending = pending.saturating_sub(1);
    }
}

/// 组字区里那几个字母被「选候选」这一键顶掉了：拿掉记录，返回刚才有几个。
///
/// **不能按 1:1 补进屏幕字符数**：`win` 三个字母按一下 `1` 只上屏一个候选字，
/// 屏幕上多出来的是 1 个字符，不是 3 个。以前按 3 补，退格就多按两下，
/// 把触发词前面的正文一起删掉了 —— 这也是「删不干净 / 删过头」的真正原因。
fn commit_pending() -> usize {
    PENDING
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or(0)
}

fn sub_screen() {
    if let Ok(mut screen) = SCREEN.lock() {
        *screen = screen.saturating_sub(1);
    }
}

/// 进程启动起算的毫秒数（只为比较「最近有没有在打字」）。
fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 调试日志：`GPUI_MENU_DEBUG_HS=1` 时写到 exe 旁边的 `hotstring-debug.log`。
///
/// 为什么不 `eprintln`：这是个 windows 子系统程序，**没有控制台**，stderr 直接被
/// 丢掉 —— 以前的调试开关等于没开，用户按了也只有一片静默，所以一直定位不到
/// 「搜狗中文模式下 `@win111` 多打一个 1」到底卡在哪一步。
///
/// 每次启动重写（不追加）：一次实测就是一次干净的记录，不会越滚越大。
fn hs_log(line: &str) {
    use std::io::Write;
    if std::env::var_os("GPUI_MENU_DEBUG_HS").is_none() {
        return;
    }
    static LOG: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    let Some(file) = LOG.get_or_init(|| {
        let path = std::env::current_exe()
            .ok()?
            .parent()?
            .join("hotstring-debug.log");
        let mut file = std::fs::File::create(&path).ok()?;
        let _ = writeln!(file, "# 热字符串调试日志：物理按键 -> 缓冲区 -> 命中 -> 退格数");
        Some(std::sync::Mutex::new(file))
    }) else {
        return;
    };
    if let Ok(mut file) = file.lock() {
        let _ = writeln!(file, "{:>7}ms {line}", now_ms());
        let _ = file.flush();
    }
}

/// 后台轮询输入法状态（**钩子里绝不查**）。
///
/// 只在「最近 1.5 秒内有人按过键」时才真的去问输入法：不打字的时候一个系统调用
/// 都不发，CPU 是 0；打字时 60ms 一次的 `SendMessageTimeout` 落在自己的线程里，
/// 卡也卡不到输入上。
fn start_ime_poller() {
    if IME_POLL_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("ime-poll".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_millis(60));
            if now_ms().saturating_sub(LAST_KEY_MS.load(Ordering::Relaxed)) > 1500 {
                continue;
            }
            let value = win32::ime_composing();
            IME_NATIVE.store(value, Ordering::Relaxed);
        })
        .ok();
}

/// 装钩子（幂等）。
fn ensure_hook() {
    if HOOK_READY.load(Ordering::SeqCst) {
        return;
    }
    // 鼠标一按下（换窗口、点光标位置）就把缓冲清掉：不然下一串字会带着旧的几个
    // 字符去匹配，表现就是「同一个短语有时灵有时不灵」。
    // 点在我们自己的提示卡片上不算 —— 那一瞬间 `run_line` 还要读缓冲长度。
    let pointer = Box::new(|| {
        if !win32::cursor_over_self() {
            reset_typed();
        }
    });
    if win32::install_input_hooks(Box::new(on_key), pointer) {
        HOOK_READY.store(true, Ordering::SeqCst);
        // 输入法状态改由后台线程问（钩子里只读 atomic）：先问一次，别让第一次按键靠猜
        IME_NATIVE.store(win32::ime_composing(), Ordering::Relaxed);
        start_ime_poller();
        println!("[popup-menu] 热字符串：键盘钩子已就绪");
    } else {
        eprintln!("[popup-menu] 热字符串：键盘钩子装不上（功能暂时不可用）");
    }
}

/// 前台窗口的输入法是不是**正在组字**（缓存 120ms）。
///
/// 缓存只是省几次 `ImmGetContext`：太长了会坏在「刚按下 Shift 切换中英文」
/// 那一下 —— 用户切完立刻打字，拿到的还是上一轮的结果，于是这一次又不触发。
fn ime_composing() -> bool {
    IME_NATIVE.load(Ordering::Relaxed)
}

/// 这个窗口所属的程序在不在「禁用应用」名单里。
///
/// 查一次要 `OpenProcess`，不可能每个键都查 —— 按「前台窗口 + 设置版本」缓存：
/// 只有切了窗口、或者刚改过名单，才真的去问系统。
fn blocked_target(hwnd: isize) -> bool {
    static CACHE: Mutex<(isize, u64, bool)> = Mutex::new((0, 0, false));
    let revision = crate::settings::revision();
    if let Ok(cache) = CACHE.lock() {
        if cache.0 == hwnd && cache.1 == revision {
            return cache.2;
        }
    }
    let blocked = win32::process_name_of_window(hwnd)
        .map(|name| crate::settings::app_blocked(&name))
        .unwrap_or(false);
    if let Ok(mut cache) = CACHE.lock() {
        *cache = (hwnd, revision, blocked);
    }
    blocked
}

/// 钩子回调：**每个按键**都会进来。
fn on_key(key: RawKey) -> bool {
    // 后台线程靠它判断「现在有人打字」—— 一个 atomic store，不会阻塞输入
    LAST_KEY_MS.store(now_ms(), Ordering::Relaxed);
    // 抬起、以及各种组合键（Ctrl+? / Win+?）都不参与
    if !key.down || ctrl_or_win_down() {
        return false;
    }
    let Some(config) = config() else {
        return false;
    };
    if config.rules.is_empty() {
        return false;
    }
    // 前台换了窗口 / 换了输入框（切程序、点进另一个文本框）：上一段缓冲作废。
    //
    // 不做这一步，「点进新地方马上敲 @Win112」就会带着几个旧字符一起去匹配，
    // 于是同一个短语有时灵有时不灵 —— 这是最难查的一类「偶发失效」。
    let target = win32::input_target();
    let switched = LAST_TARGET
        .lock()
        .map(|mut last| {
            let switched = *last != target;
            *last = target;
            switched
        })
        .unwrap_or(false);
    if switched {
        reset_typed();
    }
    // 前台是我们自己的窗口（设置页在改短语、菜单开着）：一个字都别管，
    // 否则用户在「短语 key」输入框里打 ftw 就会被自己的规则替换掉。
    if win32::window_is_self(target.0) {
        reset_typed();
        return false;
    }
    // 「禁用应用」名单里的程序在前面：热字符串整段让路（全屏游戏里闪提示条最烦）
    if blocked_target(target.0) {
        reset_typed();
        return false;
    }

    const VK_BACK: u16 = 0x08;
    const VK_TAB: u16 = 0x09;
    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;

    match key.vk {
        VK_BACK => {
            // 中文模式下，退格删的可能是组字区里的拼音字母（不占屏幕字符）
            let last_is_letter = TYPED
                .lock()
                .map(|typed| {
                    typed
                        .chars()
                        .last()
                        .map(|ch| ch.is_ascii_alphabetic())
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if !(ime_composing() && last_is_letter) {
                sub_screen();
            } else {
                // 删掉的是组字区里的拼音字母：屏幕字符数不变
                sub_pending();
            }
            if let Ok(mut typed) = TYPED.lock() {
                typed.pop();
            }
            return false;
        }
        // Esc 和方向键这类：一段词打完了（或者被打断了），别留着半个前缀
        VK_ESCAPE | 0x25..=0x28 | 0x21 | 0x22 | 0x23 | 0x24 => {
            reset_typed();
            return false;
        }
        VK_TAB | VK_RETURN => {
            // 输入法正在组字时连 Tab/回车 都不碰：那是「上屏 / 切候选」，
            // 抢过来会把用户正在打的字吃掉
            if composing() {
                reset_typed();
                return false;
            }
            let hit = {
                let Ok(typed) = TYPED.lock() else {
                    return false;
                };
                match_rule(&config, &typed)
            };
            match hit {
                Some(rule) => {
                    fire_hit(rule, Some(Replay::Key(key.vk)));
                    return true;
                }
                None => {
                    reset_typed();
                    return false;
                }
            }
        }
        _ => {}
    }

    let Some(ch) = key_char(key) else {
        return false;
    };
    // 每一段词开头问一次输入法：中文模式下字母进的是组字缓冲区（屏幕上还看不见），
    // 英文模式下按键直接落到目标程序。段中间不重复问 —— 一次要等一个窗口消息，
    // 每个键都问输入会发涩。
    let fresh_segment = TYPED.lock().map(|typed| typed.is_empty()).unwrap_or(true);
    let ime = if fresh_segment {
        // 这一段词的第一个键：记下光标位置，命中时用它数「真上了几个字」
        let caret = win32::caret_index(win32::input_target().1);
        if let Ok(mut slot) = SEG_CARET.lock() {
            *slot = caret;
        }
        refresh_ime()
    } else {
        ime_composing()
    };
    let is_letter = ch.is_ascii_alphabetic();
    if std::env::var_os("GPUI_MENU_DEBUG_HS").is_some() {
        let (screen, pending, typed_now) = (
            SCREEN.lock().map(|v| *v).unwrap_or(0),
            PENDING.lock().map(|v| *v).unwrap_or(0),
            TYPED.lock().map(|v| v.clone()).unwrap_or_default(),
        );
        hs_log(&format!(
            "key vk=0x{:02x} ch={ch:?} ime={ime} 屏幕字符={screen} 组字中={pending} 缓冲={typed_now:?}",
            key.vk
        ));
    }

    let ended = ch == ' ' || config.end.contains(&ch);
    if ended {
        // 结束字符 = 一段词打完了。先看有没有命中，再决定要不要给提示
        let hit = {
            let Ok(typed) = TYPED.lock() else {
                return false;
            };
            match_rule(&config, &typed)
        };
        if let Some(rule) = hit.filter(|_| !composing() || config.ime_match) {
            // 结束字符也吞掉，动作跑完再补发。
            //
            // 不吞的话这一步会抢跑：钩子里注入的退格、动作线程注入的替换文本，
            // 都会排在这个空格**前面**落地，结果就是 `hello F ree the whales`
            // —— 用户敲的空格插进了替换文本中间。吞掉之后顺序完全由我们决定：
            // 先退格删掉触发词，再打替换文本，最后把这个结束字符补回去。
            fire_hit(rule, Some(Replay::Char(ch)));
            return true;
        }
        if let Ok(typed) = TYPED.lock() {
            if !typed.is_empty() {
                hs_log(&format!("结束符 {ch:?}：没有命中，清空缓冲 {typed:?}"));
            }
        }
        reset_typed();
        return false;
    }

    // 普通字符：记进缓冲，顺便看看要不要提示
    let prefix = {
        let Ok(mut typed) = TYPED.lock() else {
            return false;
        };
        typed.push(ch);
        // 只留尾巴：短语再长也就十几个字符，缓冲没必要无限涨
        trim_tail(&mut typed, 48);
        typed.clone()
    };
    // 提示**永远**先给：以前把这段放在「组字中不匹配」的闸门后面，中文输入法
    // 一开着就连提示框都看不见了，用户以为功能坏了（真报过）。
    if !config.hide_hint {
        let lines = hint_lines(&config, &prefix);
        if !lines.is_empty() {
            emit_hint(lines);
        }
    }
    // 中文输入法正在组字：默认**照样匹配**，命中后先按 Esc 让输入法丢掉组字再展开
    // （`fire_hit` 里走 `cleanup` 那条路，见 `run_hotstring`）。
    //
    // 以前这里直接 `return false`，等于「中文输入法一开，热字符串全哑」——
    // 用户实测「按了没反应、也不弹提示」就是它。想要 AHK 那种「组字中不动」的
    // 行为，可以在设置里关掉「输入法组字时也匹配」。
    if composing() && !config.ime_match {
        if ime && is_letter {
            add_pending(1);
        }
        return false;
    }
    // 敲完就执行（见 `auto_hit`）：这个键**还没送到目标程序**（钩子跑在它前面），
    // 所以直接吞掉它、退格删掉「已经落地的那些字符」就行。
    if let Some(rule) = auto_hit(&config, &prefix) {
        fire_hit(rule, None);
        return true;
    }
    // 这个键真的会落到目标程序里：记下它给屏幕加了几个字符
    if ime && is_letter {
        // 中文模式的字母进组字区：屏幕上还看不见
        add_pending(1);
    } else {
        // 其它情况屏幕都会多一个字：
        // - 英文模式下的任何键，直接落到目标程序；
        // - 中文模式下按数字 / 标点：组字区里那几个字母只会被换成**一个**候选字
        //   （`win` 按一下 `1` → 屏幕多一个「问」），所以先清掉 pending 再 +1，
        //   绝不能按字母个数补。
        let committed = commit_pending();
        // 兜底推算（问不到光标的程序）：组字那几个字母原样上屏，选候选的那一下
        // 本身不额外产生字符。真实情况里输入法也可能把 `hao` 换成「好」，
        // 所以这只是估算，能用光标量就以光标为准（见 `measured_screen`）。
        add_screen(committed.max(1).min(64));
    }
    false
}

/// 触发词真实上屏了几个字符：命中时读光标，减去这一段开头记下的位置。
///
/// 只接受「合理的位移」（0..=64）：读不到、退回 0、或者文本被换过（位置反而变小）
/// 都返回 `None`，让调用方用推算值。
fn measured_screen() -> Option<usize> {
    let start = SEG_CARET.lock().ok().and_then(|slot| *slot)?;
    let (_, focus) = win32::input_target();
    let now = win32::caret_index(focus)?;
    let delta = now.checked_sub(start)?;
    (delta <= 64).then_some(delta)
}

/// 命中之后统一的收尾：算清要删几个字符 → 退格 → 让界面去跑动作。
///
/// `composing` 说明命中那一刻输入法还在组字。这种情况不能在钩子线程里退格：
/// 组字区里的拼音挡在前面，退格删的是拼音，屏幕上的字一个都不会少
/// （这就是以前「只删掉三个 1、`@win` 还留着」的根源）。交给 UI 线程先让输入法
/// 丢掉这段组字，再按屏幕字符数退格。
fn fire_hit(rule: &Rule, replay: Option<Replay>) {
    let id = rule.id.clone();
    // `:O:` 不把结束字符跟着打出去；`:B0:` 不删已经敲进去的触发词
    let replay = if rule.omit_end { None } else { replay };
    if !crate::win32::is_elevated() && crate::win32::target_is_elevated() {
        hs_log("命中，但目标程序是**提权**运行的，本程序不是 —— UIPI 会丢掉我们注入的按键/文本");
    }
    let (screen, composing) = take_screen();
    // 光标位移是实测值，比按物理键推算准（输入法选候选会吃掉/合并字符）
    let measured = measured_screen();
    let mut screen = measured.unwrap_or(screen);
    // `:B0:` 一个字符都不删 —— 必须放在「实测值」**之后**，
    // 否则刚清成 0 又被实测值覆盖回去了（B0 之前就是这么失效的）
    if rule.no_backspace {
        screen = 0;
    }
    // `:?:` 只删**触发词本身**，前面的字母要留着（AHK：`xftw` → `xhello`）。
    // 默认（不带 `?`）整段缓冲都是触发词，删整段才对。组字时不算 —— 那时候
    // 屏幕上的字符数和敲的键数对不上，交给上面那套实测逻辑。
    if rule.inside_word && !composing {
        screen = rule.text.chars().count();
    }
    if std::env::var_os("GPUI_MENU_DEBUG_HS").is_some() {
        hs_log(&format!(
            "命中 id={id} 要退格={screen}（实测={measured:?} 推算={screen}）命中时还在组字={composing} 补发={:?}",
            replay.is_some()
        ));
    }
    reset_typed();
    emit_hint(Vec::new());
    if composing {
        send(Event::Run {
            id,
            replay,
            cleanup: Some(screen),
        });
    } else {
        win32::send_backspaces(screen);
        send(Event::Run {
            id,
            replay,
            cleanup: None,
        });
    }
}

/// Ctrl / Win 压着的时候不参与热字符串（那是快捷键，不是打字）。
fn ctrl_or_win_down() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        const VK_CONTROL: i32 = 0x11;
        const VK_LWIN: i32 = 0x5B;
        const VK_RWIN: i32 = 0x5C;
        unsafe {
            [VK_CONTROL, VK_LWIN, VK_RWIN]
                .iter()
                .any(|key| (GetAsyncKeyState(*key) as u16 & 0x8000) != 0)
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 只保留最后 `max` 个字符（按字符算，不按字节）。
fn trim_tail(text: &mut String, max: usize) {
    let count = text.chars().count();
    if count <= max {
        return;
    }
    let keep: String = text.chars().skip(count - max).collect();
    *text = keep;
}

/// 比较用的归一化：**全角转半角** + 按设置决定大小写。
///
/// 中文标点开着的时候，Shift+2 敲出来的可能是全角 `＠`，数字也可能是 `１` ；
/// 用户填短语时又常常用半角。不归一化就会出现「明明敲对了却不触发」。
fn fold(text: &str, case_sensitive: bool) -> String {
    text.chars()
        .map(|ch| {
            let half = match ch as u32 {
                0xFF01..=0xFF5E => char::from_u32(ch as u32 - 0xFEE0).unwrap_or(ch),
                0x3000 => ' ',
                _ => ch,
            };
            if case_sensitive {
                half
            } else {
                half.to_ascii_lowercase()
            }
        })
        .collect()
}

/// 缓冲区正好是这个短语时命中；返回那条规则（调用方自己取 id / 要删几个字符）。
fn match_rule<'a>(config: &'a Config, typed: &str) -> Option<&'a Rule> {
    if typed.is_empty() {
        return None;
    }
    // 逐条判断：`:C:` 的那几条按原样比，`:?:` 的那几条允许出现在词中间
    if let Some(rule) = config.rules.iter().find(|rule| {
        let hay = fold(typed, config.case_sensitive || rule.case_sensitive);
        if rule.inside_word {
            hay.ends_with(&rule.key)
        } else {
            hay == rule.key
        }
    }) {
        return Some(rule);
    }
    let hay = fold(typed, config.case_sensitive);
    // 要求**整个缓冲区**就是这个短语，而不是「以它结尾」。
    //
    // 缓冲区在每个结束字符（空格 / 句号 / Tab …）处都会清空，所以「缓冲区从头
    // 就是这个短语」等价于 AHK 的默认行为：短语前面必须是非字母数字（词边界），
    // 敲 `xftw ` 不会触发 `ftw`。AHK 里要「词中间也触发」得加 `?` 选项，
    // 这里没做那个开关 —— 按默认行为来。
    config.rules.iter().find(|rule| hay == rule.key)
}

/// 命中了、而且**不用再等结束字符**，就现在执行（AHK 的 `:*` / `*`）。
///
/// 三条里任一条成立就算：
///
/// - 这一项自己勾了「一匹配就执行」（`now`）；
/// - 全局「敲完就执行」开着（`immediate`，默认开）；
/// - 短语里带数字 / 符号（`@win111` 这种）。中文输入法下敲 `@` 一定是想要那个
///   符号本身，不存在「拼音打到一半」的可能，所以这类短语敲完最后一个字符就该响，
///   不用再补一个空格 —— 用户在这儿多按一下，看起来就是「要多打一个 1」。
fn auto_hit<'a>(config: &'a Config, typed: &str) -> Option<&'a Rule> {
    match_rule(config, typed).filter(|rule| {
        rule.now
            || rule.no_end_char
            || config.immediate
            || rule.text.chars().any(|ch| !ch.is_ascii_alphabetic())
    })
}

/// 输入法现在是不是**正在组字**（有一段拼音 / 笔画还没上屏）。
///
/// 用 `PENDING` 判断而不是再问一次输入法：`PENDING > 0` 本来就意味着
/// 「刚才那几个字母进了组字区」，同一个事实不需要问两遍。
fn composing() -> bool {
    PENDING.lock().map(|pending| *pending > 0).unwrap_or(false)
}

/// 段首刷新一次输入法状态（中文模式 / 英文模式）。
///
/// 400ms 内问过就复用缓存：`ime_composing()` 要发一个窗口消息（最多等 20ms），
/// 连打几段词的时候每段都问会让第一个键明显发钝。
fn refresh_ime() -> bool {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = now_ms();
    if now.saturating_sub(LAST.load(Ordering::Relaxed)) > 400 {
        let value = win32::ime_composing();
        IME_NATIVE.store(value, Ordering::Relaxed);
        LAST.store(now, Ordering::Relaxed);
        return value;
    }
    IME_NATIVE.load(Ordering::Relaxed)
}

/// 以当前输入开头的短语（提示用）。
fn hint_lines(config: &Config, typed: &str) -> Vec<HintLine> {
    if typed.chars().count() < config.hint_after {
        return Vec::new();
    }
    let hay = fold(typed, config.case_sensitive);
    let mut lines = Vec::new();
    for rule in &config.rules {
        if !rule.key.starts_with(&hay) {
            continue;
        }
        // 第一行：**短语**放最前面（用户敲的就是它，一眼对得上），
        // 然后是它属于哪个菜单项，最后是展开成什么。
        // 几条前缀相同的规则，靠后两段才分得清。
        let detail = if rule.expand.is_empty() {
            format!("{}  ·  {}", rule.text, rule.label)
        } else {
            format!("{}  ·  {}  →  {}", rule.text, rule.label, rule.expand)
        };
        lines.push(HintLine {
            text: clip(&detail, 72),
            dim: false,
            id: rule.id.clone(),
        });
        // 路径那行只在「第一行没写全路径」时才有意义：
        // `运行程序 C://Windows//notepad.exe` 的展开文本本身就是路径，再列一遍是重复。
        let path_worth_showing =
            !rule.path.is_empty() && !rule.expand.trim().starts_with(rule.path.trim());
        if config.hint_path_chars > 0 && path_worth_showing {
            lines.push(HintLine {
                text: clip(&rule.path, config.hint_path_chars),
                dim: true,
                id: String::new(),
            });
        }
        if lines.len() >= 6 {
            break;
        }
    }
    lines
}

fn emit_hint(lines: Vec<HintLine>) {
    HINT_GEN.fetch_add(1, Ordering::SeqCst);
    if lines.is_empty() {
        send(Event::Hint(Vec::new()));
        return;
    }
    send(Event::Hint(lines));
}

fn send(event: Event) {
    if let Some(tx) = EVENTS.get() {
        // 满了就丢：宁可少一条提示，也不能让钩子回调卡住
        let _ = tx.try_send(event);
    }
}

/// 提示卡片被点了一下：删掉已经敲进去的那些字符，然后执行这一项。
///
/// 只删「我们真的看见用户敲了的那几个字符」（缓冲区长度），而不是整条短语 ——
/// 提示是**前缀匹配**，用户可能才敲了 `jt` 就点了 `j@` 那一条。
pub fn run_line(id: &str) {
    // 删「屏幕上真的出现了的那几个字」，不是「敲了几个键」：
    // 中文模式下一条 `@ma1` 敲了 4 个键，屏幕上只有 `@吗` 两个字。
    let (length, composing) = take_screen();
    reset_typed();
    HINT_GEN.fetch_add(1, Ordering::SeqCst);
    crate::hint::hide();
    if length > 0 && !composing {
        win32::send_backspaces(length);
    }
    send(Event::Run {
        id: id.to_string(),
        replay: None,
        cleanup: composing.then_some(length),
    });
}

/// 提示窗口自己收起来了（超时 / 被替换）。
///
/// 只把编号往前推一格：这样「刚排上的那次收起」不会重复生效。
/// 提示本身不留「安静期」—— 再敲到匹配的字符就照常弹，
/// 不需要先按空格 / 回车这类结束字符把状态「解锁」。
pub fn on_hint_expired() {
    HINT_GEN.fetch_add(1, Ordering::SeqCst);
}

/// 把编号往前推一格：排着的「到点收起」全都作废（鼠标悬停、被新提示顶掉时用）。
pub fn bump_hint_generation() {
    HINT_GEN.fetch_add(1, Ordering::SeqCst);
}

/// 这一轮提示的编号（提示窗口用它判断自己是不是已经过期了）。
pub fn hint_generation() -> u64 {
    HINT_GEN.load(Ordering::SeqCst)
}

/// 调试用：直接跑一次「敲了这些字」的匹配（`GPUI_MENU_DEBUG_HOTSTRING=huiz`）。
pub fn debug_match(text: &str) {
    let (config, active) = rebuild();
    println!(
        "[hotstring] 共 {} 条规则（现在{}）",
        config.rules.len(),
        if active { "生效" } else { "不生效" }
    );
    for rule in &config.rules {
        println!(
            "[hotstring]   {:?} -> {} ({:?}) path={:?}",
            rule.text, rule.id, rule.expand, rule.path
        );
    }
    let lines = hint_lines(&config, text);
    println!("[hotstring] 提示（{text:?}）：{}", lines.len());
    for line in &lines {
        println!(
            "[hotstring]   {}{}",
            if line.dim { "  " } else { "" },
            line.text
        );
    }
    println!(
        "[hotstring] 命中 = {:?}",
        match_rule(&config, text).map(|rule| (rule.id.clone(), rule.text.chars().count()))
    );
}

/// 键码 → 字符（只认 ASCII 可见字符；中文靠输入法，走不到这里）。
fn key_char(key: RawKey) -> Option<char> {
    let shift = key.shift;
    let vk = key.vk;
    if (0x41..=0x5A).contains(&vk) {
        let lower = (b'a' + (vk - 0x41) as u8) as char;
        return Some(if shift {
            lower.to_ascii_uppercase()
        } else {
            lower
        });
    }
    if (0x30..=0x39).contains(&vk) {
        const SHIFTED: [char; 10] = [')', '!', '@', '#', '$', '%', '^', '&', '*', '('];
        let index = (vk - 0x30) as usize;
        return Some(if shift {
            SHIFTED[index]
        } else {
            (b'0' + index as u8) as char
        });
    }
    if (0x60..=0x69).contains(&vk) {
        let index = (vk - 0x60) as usize;
        return Some((b'0' + index as u8) as char);
    }
    let ch = match vk {
        0x20 => ' ',
        0xBA => {
            if shift {
                ':'
            } else {
                ';'
            }
        }
        0xBB => {
            if shift {
                '+'
            } else {
                '='
            }
        }
        0xBC => {
            if shift {
                '<'
            } else {
                ','
            }
        }
        0xBD => {
            if shift {
                '_'
            } else {
                '-'
            }
        }
        0xBE => {
            if shift {
                '>'
            } else {
                '.'
            }
        }
        0xBF => {
            if shift {
                '?'
            } else {
                '/'
            }
        }
        0xC0 => {
            if shift {
                '~'
            } else {
                '`'
            }
        }
        0xDB => {
            if shift {
                '{'
            } else {
                '['
            }
        }
        0xDC => {
            if shift {
                '|'
            } else {
                '\\'
            }
        }
        0xDD => {
            if shift {
                '}'
            } else {
                ']'
            }
        }
        0xDE => {
            if shift {
                '"'
            } else {
                '\''
            }
        }
        _ => return None,
    };
    Some(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ahk_option_syntax_is_parsed() {
        use super::parse_syntax;

        // 不带选项：原样就是短语
        let (flags, text) = parse_syntax("ftw");
        assert_eq!(text, "ftw");
        assert!(!flags.case_sensitive && !flags.no_end_char && !flags.inside_word);

        // `:C:BTW::` —— 区分大小写
        let (flags, text) = parse_syntax(":C:BTW::");
        assert_eq!(text, "BTW");
        assert!(flags.case_sensitive);

        // `:*:ftw::` —— 不用结束字符
        let (flags, text) = parse_syntax(":*:ftw::");
        assert_eq!(text, "ftw");
        assert!(flags.no_end_char);

        // 多个选项一起写，大小写都认
        let (flags, text) = parse_syntax(":c*o?b0:abc");
        assert_eq!(text, "abc");
        assert!(flags.case_sensitive);
        assert!(flags.no_end_char);
        assert!(flags.omit_end);
        assert!(flags.no_backspace);

        // `:?:` 词中间也触发；结尾的 `::` 可以省略
        let (flags, text) = parse_syntax(":?:btw");
        assert_eq!(text, "btw");
        assert!(flags.inside_word);

        // 只有收尾 `::` 的写法
        let (flags, text) = parse_syntax("ftw::");
        assert_eq!(text, "ftw");
        assert!(!flags.no_end_char);
    }

    fn config(rules: &[(&str, &str)], end: &str) -> Config {
        config_with(rules, end, &[])
    }

    /// `now` = 哪些短语是「一匹配就执行」的。
    fn config_with(rules: &[(&str, &str)], end: &str, now: &[&str]) -> Config {
        Config {
            rules: rules
                .iter()
                .map(|(text, id)| Rule {
                    text: text.to_string(),
                    key: fold(text, false),
                    id: id.to_string(),
                    label: id.to_string(),
                    expand: String::new(),
                    path: String::new(),
                    now: now.contains(text),
                    case_sensitive: false,
                    no_end_char: false,
                    inside_word: false,
                    omit_end: false,
                    no_backspace: false,
                })
                .collect(),
            end: end.chars().collect(),
            case_sensitive: false,
            hint_after: 3,
            hint_path_chars: 10,
            ime_match: false,
            hide_hint: false,
            immediate: false,
        }
    }

    /// 命中的是哪个条目 + 要删几个字符（单测里就这么比，省得给 `Rule` 加 Debug）。
    fn hit(cfg: &Config, typed: &str) -> Option<(String, usize)> {
        match_rule(cfg, typed).map(|rule| (rule.id.clone(), rule.text.chars().count()))
    }

    #[test]
    fn a_phrase_matches_the_whole_buffer() {
        let cfg = config(&[("ftw", "a")], "-.");
        assert_eq!(hit(&cfg, "ftw"), Some(("a".to_string(), 3)));
        assert_eq!(hit(&cfg, "ft"), None);
        // AHK 的默认行为：短语前面必须是词边界，`xftw` 不算命中
        // （缓冲区在空格处就清空了，剩下的 `xftw` 是一个整词）
        assert_eq!(hit(&cfg, "xftw"), None);
    }

    #[test]
    fn the_longest_phrase_wins() {
        let cfg = config(&[("abcd", "long"), ("ab", "short")], "-.");
        assert_eq!(hit(&cfg, "abcd"), Some(("long".to_string(), 4)));
        assert_eq!(hit(&cfg, "ab"), Some(("short".to_string(), 2)));
        assert_eq!(hit(&cfg, "abc"), None);
    }

    /// AHK 的 `*`：一匹配就执行，不用等空格 / 回车。
    ///
    /// 关键点是「只对开了这个选项的短语生效」—— 没开的还是要等结束字符，
    /// 否则用户敲到一半就被替换掉了。
    #[test]
    fn an_instant_phrase_fires_without_a_terminator() {
        let cfg = config_with(&[("j@", "mail"), ("ftw", "a")], "-.", &["j@"]);
        assert_eq!(
            auto_hit(&cfg, "j@").map(|rule| rule.id.clone()),
            Some("mail".to_string())
        );
        // `ftw` 没开「立刻执行」：匹配上了也不该在这里开火
        assert!(auto_hit(&cfg, "ftw").is_none());
        assert_eq!(hit(&cfg, "ftw"), Some(("a".to_string(), 3)));
        // 还没拼完当然不命中
        assert!(auto_hit(&cfg, "j").is_none());
    }

    /// 带数字/符号的短语永远「敲完就响」：中文输入法下用户为了一个 `@win111`
    /// 还要再多按一个空格，看起来就是「少了一个 1、要多打一个」。
    #[test]
    fn a_phrase_with_digits_or_symbols_fires_as_soon_as_it_is_typed() {
        let cfg = config(&[("@win111", "a"), ("ftw", "b")], "-.");
        assert_eq!(
            auto_hit(&cfg, "@win111").map(|rule| rule.id.clone()),
            Some("a".to_string())
        );
        // 纯字母短语没勾「立刻执行」时仍然等结束字符（避免拼音打到一半被替换）
        assert!(auto_hit(&cfg, "ftw").is_none());
    }

    /// 提示第一行必须是「短语 → 菜单项名」的顺序：敲字的人认的是自己打的缩写。
    #[test]
    fn hints_lead_with_the_phrase_then_the_menu_item() {
        let mut cfg = config(&[("hui", "a")], "-.");
        cfg.rules[0].label = "回邮件".to_string();
        cfg.rules[0].expand = "你好，世界".to_string();
        let lines = hint_lines(&cfg, "hui");
        assert_eq!(lines[0].text, "hui  ·  回邮件  →  你好，世界");
        // 这一行可以点（知道要执行哪一项）
        assert_eq!(lines[0].id, "a");
    }

    #[test]
    fn hints_start_at_the_configured_character_count() {
        let cfg = config(&[("huiz", "a")], "-.");
        assert!(hint_lines(&cfg, "hu").is_empty());
        assert_eq!(hint_lines(&cfg, "hui").len(), 1);
        assert!(hint_lines(&cfg, "zzz").is_empty());
    }

    #[test]
    fn keys_translate_to_characters() {
        let key = |vk, shift| key_char(RawKey { vk, down: true, shift });
        assert_eq!(key(0x41, false), Some('a'));
        assert_eq!(key(0x41, true), Some('A'));
        assert_eq!(key(0x31, true), Some('!'));
        assert_eq!(key(0xBE, true), Some('>'));
        assert_eq!(key(0x20, false), Some(' '));
        assert_eq!(key(0x25, false), None);
    }

    #[test]
    fn the_buffer_keeps_only_the_tail() {
        let mut text = String::new();
        for ch in "abcdefgh".chars() {
            text.push(ch);
        }
        trim_tail(&mut text, 4);
        assert_eq!(text, "efgh");
    }

    #[test]
    fn the_first_command_token_drops_arguments() {
        assert_eq!(first_token("\"C:\\a b\\app.exe\" --x"), "C:\\a b\\app.exe");
        assert_eq!(first_token("notepad.exe"), "notepad.exe");
    }
    #[test]
    fn full_width_input_still_matches_the_rule() {
        // 中文标点开着时 Shift+2 出的是全角 `＠`，数字也可能是全角
        let cfg = config_with(&[("@win111", "a")], " ", &["@win111"]);
        assert_eq!(match_rule(&cfg, "@win111").map(|rule| rule.id.as_str()), Some("a"));
        assert_eq!(
            match_rule(&cfg, "＠win１１１").map(|rule| rule.id.as_str()),
            Some("a"),
            "全角输入也要能触发"
        );
        assert!(match_rule(&cfg, "@win11").is_none());
    }

    /// `@win111` 在中文输入法下全打一遍，屏幕上到底有几个字。
    ///
    /// 搜狗这类输入法：`@` 直接上屏；`win` 进组字区；第一个 `1` 是「选第一个
    /// 候选」，只上屏**一个**字；后面两个 `1` 各自直接上屏。
    #[test]
    fn a_digit_that_picks_a_candidate_only_puts_one_character_on_screen() {
        reset_typed();
        add_screen(1); // `@`
        add_pending(3); // `win` 还在组字区，屏幕上没有
        // 第一个 `1`：选候选 —— 屏幕上多一个字，不是三个
        commit_pending();
        add_screen(1);
        // 第二个 `1`：直接上屏
        commit_pending();
        add_screen(1);
        let (screen, composing) = take_screen();
        assert_eq!(screen, 3, "@ + 一个候选字 + 一个 1");
        assert!(!composing, "取干净之后就不该还认为在组字");
        assert_eq!(take_screen(), (0, false), "取过一次就清零了");
    }

    /// 组字区里的字母不算屏幕字符：命中时先取消组字，它们就彻底没了。
    #[test]
    fn pending_letters_are_not_counted_as_screen_characters() {
        reset_typed();
        add_screen(1); // `@`
        add_pending(3); // `win`
        let (screen, composing) = take_screen();
        assert_eq!(screen, 1);
        assert!(composing, "命中那一刻还在组字，要先让输入法丢掉这一段");
    }
}
