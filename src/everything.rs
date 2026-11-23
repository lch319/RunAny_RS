//! Everything 联动：把「裸文件名」查成完整路径。
//!
//! ## 为什么不再自己建全盘索引
//!
//! 上一版做过一份自己的全盘文件索引（遍历所有固定盘、把每个文件的 名字→路径 存进
//! 内存再落到 `fileindex.json`）。实测不可接受：
//!
//! - 几百万个文件 → 常驻几十上百 MB 内存，进程启动就背着；
//! - 首次扫描要遍历整盘，CPU / 磁盘 IO 抖好几分钟（用户开着别的程序时尤其明显）；
//! - `fileindex.json` 动辄几十 MB，写一次要几秒；
//! - 「实时更新」只能靠定时重扫，等于把上面这些开销重复付。
//!
//! 也评估过 Arke（`crates/indexd`）的做法：读 NTFS MFT + USN 日志，确实又快又省。
//! 但那是一个**独立的 Windows 后台服务**：mft.rs 1111 行、usn.rs 913 行、
//! server.rs 732 行、install.rs 602 行（装服务）、proto/client 900 多行，一共
//! 5000 多行，还要管员权限（读卷需要 SeManageVolumePrivilege）、要装服务、要
//! 走命名管道 IPC。为了「按快捷键弹个菜单」的便携单 exe 背一套索引服务，不划算。
//!
//! **Everything 本身就是这件事的成熟实现**（同样读 MFT + USN），而且很多人机器上
//! 已经常年开着。所以这里只做一个薄封装：借它的查询结果，自己不存任何索引。
//! 查询一次几毫秒，进程内存零增长，Everything 没装 / 没运行 / 查询失败都不影响
//! 本软件正常使用（调用方会退回自己的可执行文件小索引）。
//!
//! ## 两条后端
//!
//! 1. **SDK（首选）**：动态加载 `Everything64.dll` / `Everything32.dll`（不用在编译期
//!    链接，也不占分发体积）。DLL 里导出的是 `Everything_SetSearchW` 这一套 C API。
//! 2. **es.exe（兜底）**：Everything 官方的命令行工具，输出一行一个完整路径。
//!
//! 两条都不可用就返回空，绝不影响主流程。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 一条命中结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// 完整路径
    pub path: String,
    /// 修改时间（Unix 秒；拿不到就是 0）
    pub mtime: u64,
}

/// 一次查询最多取多少条（同名文件再多也没必要全拿）
pub const DEFAULT_LIMIT: usize = 30;

/// 查询结果的缓存时长：设置页每帧都会问一次「这个名字解析到哪」，
/// 不缓存就会每帧起一次查询。
const CACHE_TTL: Duration = Duration::from_secs(30);
/// 缓存最多留多少条
const CACHE_CAP: usize = 256;
/// 连续失败几次就冷却一会儿，别每次执行动作都去撞
const FAIL_COOLDOWN_THRESHOLD: u32 = 2;
const COOLDOWN: Duration = Duration::from_secs(20);

/// 后端类型（给设置页显示）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    /// Everything SDK（DLL）
    Sdk,
    /// Everything 命令行 es.exe
    Es,
    /// 都不可用
    None,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Backend::Sdk => "Everything SDK",
            Backend::Es => "es.exe",
            Backend::None => "未连接",
        }
    }
}

/// 给设置页看的一行状态。
#[derive(Clone, Debug)]
pub struct Status {
    pub backend: Backend,
    /// 后端从哪儿来的（DLL / es.exe 的完整路径），没有就是空
    pub source: String,
    /// 最近一次查询耗时（毫秒）
    pub last_ms: u128,
    /// 最近一次失败原因
    pub last_error: Option<String>,
    /// 当前是不是在冷却（刚连续失败过）
    pub cooling: bool,
}

struct Inner {
    /// 缓存：查询词 → (结果, 时间)
    cache: HashMap<String, (Vec<Hit>, Instant)>,
    failures: u32,
    cooldown_until: Option<Instant>,
    last_ms: u128,
    last_error: Option<String>,
}

fn inner() -> &'static Mutex<Inner> {
    static INNER: OnceLock<Mutex<Inner>> = OnceLock::new();
    INNER.get_or_init(|| {
        Mutex::new(Inner {
            cache: HashMap::new(),
            failures: 0,
            cooldown_until: None,
            last_ms: 0,
            last_error: None,
        })
    })
}

// ---------------------------------------------------------------------------
// 对外接口
// ---------------------------------------------------------------------------

/// 按文件名精确查（只返回文件名完全相同的那些完整路径）。
///
/// 名字里带不带 `.exe` 都行：不带扩展名时按 `名字.*` 查，再按「主名相同」收一遍，
/// 所以 `notepad` 能查到 `notepad.exe`，但不会把 `notepad2.exe` 带出来。
/// 排查用日志：写到 exe 同目录的 `prog-debug.log`（GUI 程序没有控制台，
/// `eprintln` 谁也看不见）。设环境变量 `GPUI_MENU_DEBUG_PROG=1` 才写。
fn prog_log(line: &str) {
    use std::io::Write;
    if std::env::var_os("GPUI_MENU_DEBUG_PROG").is_none() {
        return;
    }
    static LOG: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    let Some(file) = LOG.get_or_init(|| {
        let path = std::env::current_exe().ok()?.parent()?.join("prog-debug.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(std::sync::Mutex::new(file))
    }) else {
        return;
    };
    if let Ok(mut file) = file.lock() {
        let _ = writeln!(file, "{line}");
    }
}

pub fn lookup(name: &str, limit: usize) -> Vec<Hit> {
    // 用 const 拼引号：这个文件里手写 `\"` 太容易出错，干脆不写转义
    const Q: char = '"';
    let name = name.trim();
    if name.is_empty() {
        return Vec::new();
    }
    let lower = name.to_ascii_lowercase();
    let has_ext = lower.contains('.');
    let stem = lower.split('.').next().unwrap_or(&lower).to_string();

    // `file:` 前缀是关键：Everything 默认开着「匹配路径」，光加引号的整名
    // 会被拿去匹配**整个路径** → 0 条命中。用户截图里 Everything 自己搜的是
    // `ext:exe` + 引号整名，能中，说明它按文件名比。这里统一用 `file:` 强制
    // 按文件名匹配，再不行就退回通配写法，最后都按完整文件名过滤。
    let mut attempts: Vec<String> = Vec::new();
    if has_ext {
        attempts.push(format!("file:{Q}{name}{Q}"));
        attempts.push(format!("file:{name}"));
        attempts.push(format!("*{name}"));
    } else {
        attempts.push(format!("file:{Q}{name}.*{Q}"));
        attempts.push(format!("file:{name}.*"));
        attempts.push(format!("*{name}.*"));
        attempts.push(stem.clone());
    }

    let mut found: Vec<Hit> = Vec::new();
    for attempt in &attempts {
        let hits = query(attempt, limit.max(60));
        prog_log(&format!("lookup({name:?}) 试 {attempt:?} -> {} 条", hits.len()));
        if !hits.is_empty() {
            found = hits;
            break;
        }
    }

    found.retain(|hit| {
        let file = file_name_of(&hit.path).to_ascii_lowercase();
        if has_ext {
            file == lower
        } else {
            let file_stem = file.split('.').next().unwrap_or(&file);
            file == lower || file_stem == lower
        }
    });
    found.truncate(limit);
    if !found.is_empty() {
        prog_log(&format!(
            "   保留：{:?}",
            found.iter().take(3).map(|hit| hit.path.as_str()).collect::<Vec<_>>()
        ));
    }
    found
}

/// 按词模糊搜（文件名里包含这个词的都算）。
///
/// 目前菜单用不到（「运行程序」只填裸文件名，走 `lookup` 就够），
/// 留给之后那个「搜整台电脑文件」的搜索框。
#[allow(dead_code)]
pub fn search(keyword: &str, limit: usize) -> Vec<Hit> {
    const Q: char = '"';
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return Vec::new();
    }
    let hits = query(&format!("file:{Q}{keyword}{Q}"), limit);
    if !hits.is_empty() {
        return hits;
    }
    // 带扩展名查不到就退回主名（同 `lookup` 的理由）
    if let Some(stem) = keyword.split('.').next().filter(|stem| !stem.is_empty()) {
        let hits = query(&format!("file:{Q}{stem}.*{Q}"), limit);
        if !hits.is_empty() {
            return hits;
        }
    }
    hits
}

/// 按「文件夹 + 后缀」列文件（子菜单的「自动生成」用）。
///
/// 写法是 Everything 自己的语法：`path:"D:\影片\" ext:mp4;mkv` ——
/// 走索引，不扫盘。后端没连上 / 查询失败都返回空，调用方按「这一轮没东西可加」处理。
pub fn files_in(dir: &str, exts: &[String], limit: usize, recurse: bool) -> Vec<Hit> {
    let dir = dir.trim().trim_matches('"');
    if dir.is_empty() {
        return Vec::new();
    }
    // 统一成反斜杠、去掉尾部的 `\`：**查询里不能带**，否则结尾就成了
    // `path:"D:/影片/"`，Everything 会把 `\"` 当成转义，整条语法就废了。
    let mut norm = dir.replace('/', "\\");
    while norm.ends_with('\\') {
        norm.pop();
    }
    if norm.is_empty() {
        return Vec::new();
    }
    // 只看这一层：直接读目录就行（比查索引还准，也不用管索引有没有建好）
    if !recurse {
        let mut found = local_hits(&norm, exts);
        found.sort_by_key(|hit| hit.path.to_ascii_lowercase());
        found.truncate(limit.max(1));
        return found;
    }
    // 过滤用的前缀**要带** `\`：这样 `D:/影片` 不会把 `D:/影片2/a.mp4` 也算进来
    let prefix = format!("{norm}\\").to_ascii_lowercase();
    let pattern = if exts.is_empty() {
        format!("path:\"{norm}\"")
    } else {
        format!("path:\"{norm}\" ext:{}", exts.join(";"))
    };
    let mut found = query(&pattern, limit.max(50));
    found.retain(|hit| hit.path.replace('/', "\\").to_ascii_lowercase().starts_with(&prefix));

    // 兜底：Everything 的索引会漏东西（刚建好的索引、隐藏项、某些 `.lnk`），
    // 用户报过「桌面 5 个 lnk 只生成 1 个」。这里再拿本地目录直接读一遍补上 ——
    // 只是「列出这个文件夹」，不是自己建索引，开销就是一次目录读取。
    //
    // 只在 Everything 确实连上了的时候补：全盘搜索这种能力没有它是不成立的，
    // 但「这个文件夹里有什么」本地本来就知道。
    if backend() != Backend::None {
        for hit in local_hits(&norm, exts) {
            if !found
                .iter()
                .any(|seen| seen.path.eq_ignore_ascii_case(&hit.path))
            {
                found.push(hit);
            }
        }
    }

    found.sort_by_key(|hit| hit.path.to_ascii_lowercase());
    // 去重（同一个文件可能被 EV 用不同大小写给两遍）
    found.dedup_by(|a, b| a.path.eq_ignore_ascii_case(&b.path));
    found.truncate(limit.max(1));
    found
}

/// 直接读目录（不递归）：拿到这个文件夹里符合后缀的文件。
///
/// 只读得到「本地/可访问」的路径；网络路径慢、虚拟目录读不了 —— 那些读不到的
/// 一律返回空，交给 Everything 的结果兜着。
fn local_hits(dir: &str, exts: &[String]) -> Vec<Hit> {
    let want_ext = |name: &str| -> bool {
        if exts.is_empty() {
            return true;
        }
        match std::path::Path::new(name).extension() {
            Some(ext) => {
                let ext = ext.to_string_lossy().to_ascii_lowercase();
                exts.iter().any(|want| want == &ext)
            }
            None => false,
        }
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_file = entry.file_type().map(|kind| kind.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !want_ext(&name) {
            continue;
        }
        out.push(Hit {
            path: path.display().to_string(),
            mtime: 0,
        });
    }
    out
}

/// 当前状态（设置页显示用）。
pub fn status() -> Status {
    let backend = backend();
    let source = backend_source();
    let guard = inner().lock().unwrap_or_else(|e| e.into_inner());
    Status {
        backend,
        source,
        last_ms: guard.last_ms,
        last_error: guard.last_error.clone(),
        cooling: guard
            .cooldown_until
            .is_some_and(|until| until > Instant::now()),
    }
}

/// 清掉缓存 / 冷却计数（设置页的「重新检测」按钮用）。
///
/// 场景：用户刚才没开 Everything，查询失败进了冷却；现在打开了 Everything，
/// 点一下就能立刻重试，不用等冷却时间过去。
pub fn reset() {
    let mut guard = inner().lock().unwrap_or_else(|e| e.into_inner());
    guard.cache.clear();
    guard.failures = 0;
    guard.cooldown_until = None;
    guard.last_error = None;
    guard.last_ms = 0;
}

/// 改了 Everything 路径 / 刚扫描到目录：把「找过哪些 DLL、es.exe」的判断作废，
/// 下次查询重新探一遍。
pub fn reload() {
    #[cfg(windows)]
    win::reload();
    reset();
}

/// Everything.exe 在哪儿（用户指定 > 程序目录下的 `Everything\` > 安装目录）。
pub fn exe_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        win::exe_path()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Everything 现在是不是在运行（按进程名看，不用问它的 IPC）。
pub fn is_running() -> bool {
    #[cfg(windows)]
    {
        win::is_running()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 设置里勾了「没运行时随本程序启动」：它没在跑就拉起来。
///
/// 返回「这一次是不是真的启动了它」。启动失败（路径不对 / 没权限）就返回 false，
/// 调用方不用管 —— Everything 没起来只是查询失败，不影响菜单本身。
pub fn autostart_if_needed() -> bool {
    if !crate::settings::read(|settings| settings.everything.autostart) {
        return false;
    }
    if is_running() {
        return false;
    }
    #[cfg(windows)]
    {
        win::start()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 扫描「程序目录/Everything」：便携版把整个文件夹拷过来，点一下就能接上。
///
/// 返回找到的 `Everything.exe`（找不到就是 `None`）。DLL / es.exe 在同一个目录里，
/// 后端探测会顺着 `exe` 的目录去找。
pub fn scan_program_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    scan_dir(exe.parent()?)
}

/// [`scan_program_dir`] 的本体：给一个「程序目录」，看它下面有没有 `Everything\`。
///
/// 单独拆出来是为了能测 —— 直接测 `scan_program_dir` 得先造一个程序目录，
/// 而 `current_exe()` 在测试里指向的是 cargo 的临时 exe。
fn scan_dir(program_dir: &std::path::Path) -> Option<PathBuf> {
    let dir = program_dir.join("Everything");
    if !dir.is_dir() {
        return None;
    }
    for name in ["Everything.exe", "Everything64.exe", "Everything32.exe"] {
        let path = dir.join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    // 只有 DLL / es.exe 的（解压出来只放了这几个文件）：整个目录也能用
    for name in ["Everything64.dll", "Everything32.dll", "es.exe"] {
        if dir.join(name).is_file() {
            return Some(dir);
        }
    }
    None
}

/// 清掉老版本留下的全盘索引缓存（`fileindex.json` 可能几十 MB）。
///
/// 只在 exe 旁边找，删得掉就删，删不掉（被占用/没权限）也不管 —— 不能让清理动作
/// 挡住启动。
pub fn cleanup_legacy_index() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    for name in ["fileindex.json", "fileindex.json.tmp"] {
        let path = dir.join(name);
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => println!("[everything] 清掉旧的全盘索引缓存: {}", path.display()),
                Err(err) => println!(
                    "[everything] 旧的全盘索引缓存删不掉（{}），忽略: {}",
                    err,
                    path.display()
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 查询（带缓存 / 失败冷却）
// ---------------------------------------------------------------------------

fn query(text: &str, limit: usize) -> Vec<Hit> {
    // 缓存键要带上 limit：同一个查询先按 50 条查过，再按 500 条来要，
    // 直接命中旧缓存就只拿到 50 条（「明明有 5 个文件却只给 1 个」也有这个原因）
    let key = format!("{text}\u{0}{limit}");
    let text = key.as_str();
    // 1) 缓存
    {
        let mut guard = inner().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((hit, at)) = guard.cache.get(text) {
            if at.elapsed() < CACHE_TTL {
                return hit.clone();
            }
        }
        guard.cache.remove(text);
        if guard
            .cooldown_until
            .is_some_and(|until| until > Instant::now())
        {
            return Vec::new();
        }
    }

    // 2) 真去查
    let started = Instant::now();
    let result = query_impl(text, limit);
    let elapsed = started.elapsed();

    let mut guard = inner().lock().unwrap_or_else(|e| e.into_inner());
    guard.last_ms = elapsed.as_millis();
    match result {
        Ok(found) => {
            guard.failures = 0;
            guard.cooldown_until = None;
            guard.last_error = None;
            if guard.cache.len() >= CACHE_CAP {
                guard.cache.clear();
            }
            guard.cache.insert(text.to_string(), (found.clone(), Instant::now()));
            found
        }
        Err(err) => {
            guard.failures = guard.failures.saturating_add(1);
            guard.last_error = Some(err);
            if guard.failures >= FAIL_COOLDOWN_THRESHOLD {
                // 连续失败（Everything 没运行 / DLL 是坏的）：先冷一会儿。
                // 冷却期内的调用直接返回空，不让「执行菜单动作」被拖慢。
                guard.cooldown_until = Some(Instant::now() + COOLDOWN);
            }
            Vec::new()
        }
    }
}

fn query_impl(text: &str, limit: usize) -> Result<Vec<Hit>, String> {
    #[cfg(windows)]
    {
        if let Some(paths) = win::sdk_query(text, limit)? {
            return Ok(paths);
        }
        win::es_query(text, limit)
    }
    #[cfg(not(windows))]
    {
        let _ = (text, limit);
        Err("Everything 只在 Windows 上可用".to_string())
    }
}

fn backend() -> Backend {
    #[cfg(windows)]
    {
        if win::sdk_loaded() {
            return Backend::Sdk;
        }
        if win::es_path().is_some() {
            return Backend::Es;
        }
    }
    Backend::None
}

fn backend_source() -> String {
    #[cfg(windows)]
    {
        if let Some(path) = win::sdk_source() {
            return path;
        }
        if let Some(path) = win::es_path() {
            return path.display().to_string();
        }
    }
    String::new()
}

/// 取一段路径里的文件名。
fn file_name_of(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

// ---------------------------------------------------------------------------
// Windows 实现
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex, OnceLock};

    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    use super::Hit;

    /// `EVERYTHING_REQUEST_FULL_PATH_AND_FILE_NAME`
    const REQUEST_FULL_PATH: u32 = 0x0000_0004;
    /// `EVERYTHING_REQUEST_DATE_MODIFIED`（注意不是 0x8 —— 那是 EXTENSION）
    const REQUEST_DATE_MODIFIED: u32 = 0x0000_0040;
    /// 起子进程不要弹控制台黑框
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    type SetSearchW = unsafe extern "system" fn(*const u16);
    type SetMax = unsafe extern "system" fn(u32);
    type SetRequestFlags = unsafe extern "system" fn(u32);
    type SetMatchPath = unsafe extern "system" fn(i32);
    type SetMatchCase = unsafe extern "system" fn(i32);
    type SetRegex = unsafe extern "system" fn(i32);
    type QueryW = unsafe extern "system" fn(i32) -> i32;
    type GetNumResults = unsafe extern "system" fn() -> u32;
    type GetFullPathW = unsafe extern "system" fn(u32, *mut u16, u32) -> u32;
    type GetDateModified = unsafe extern "system" fn(u32, *mut FILETIME) -> i32;
    type GetLastError = unsafe extern "system" fn() -> u32;

    pub struct Sdk {
        /// 模块基址。只是拿住它别被卸载（查询靠下面那堆函数指针）。
        /// DLL 一旦 LoadLibrary 就会陪到进程结束，卸载反而会让指针失效。
        pub _lib: usize,
        pub source: String,
        set_search: SetSearchW,
        set_max: SetMax,
        set_request_flags: SetRequestFlags,
        set_match_path: SetMatchPath,
        set_match_case: SetMatchCase,
        set_regex: SetRegex,
        query: QueryW,
        get_num_results: GetNumResults,
        get_full_path: GetFullPathW,
        get_date_modified: GetDateModified,
        get_last_error: GetLastError,
    }

    // SAFETY: 句柄本身只是个模块基址；SDK 的查询状态是「进程全局一份」，
    // 所有调用都会先拿到 `SDK_LOCK` 串行化，不会并发进 SDK。
    unsafe impl Send for Sdk {}
    unsafe impl Sync for Sdk {}

    /// SDK 的全局状态是每个进程一份，并发调用会互相踩，所以查询要串行。
    static SDK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 「探测过什么」的缓存。`tried = false` 就是「还没探 / 刚 reload，下次要用再探」。
    ///
    /// 之所以要能作废：用户在设置里改了 Everything 路径、或者刚扫描到程序目录下的
    /// `Everything\` 文件夹，得让后端重新找一遍 DLL / es.exe（以前是 `OnceLock`，
    /// 探过一次就定死了，改了路径也不生效）。
    struct Probe<T> {
        tried: bool,
        value: Option<T>,
    }

    impl<T> Default for Probe<T> {
        fn default() -> Self {
            Self {
                tried: false,
                value: None,
            }
        }
    }

    static SDK: OnceLock<Mutex<Probe<Arc<Sdk>>>> = OnceLock::new();
    static ES: OnceLock<Mutex<Probe<PathBuf>>> = OnceLock::new();

    fn sdk() -> Option<Arc<Sdk>> {
        let slot = SDK.get_or_init(|| Mutex::new(Probe::default()));
        let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.tried {
            guard.tried = true;
            guard.value = load_sdk().map(Arc::new);
        }
        guard.value.clone()
    }

    /// 路径改了 / 刚扫描到目录：下次查询重新找 DLL 和 es.exe。
    pub fn reload() {
        if let Some(slot) = SDK.get() {
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Probe::default();
        }
        if let Some(slot) = ES.get() {
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Probe::default();
        }
    }

    pub fn sdk_loaded() -> bool {
        sdk().is_some()
    }

    pub fn sdk_source() -> Option<String> {
        sdk().map(|sdk| sdk.source.clone())
    }

    /// 用户在设置里填的路径（可能是 exe，也可能是文件夹）。
    fn configured() -> Option<PathBuf> {
        let raw = crate::settings::read(|settings| settings.everything.exe.clone());
        let raw = raw.trim().trim_matches('"');
        if raw.is_empty() {
            return None;
        }
        Some(PathBuf::from(raw))
    }

    /// 用户指定的文件夹（填的是 exe 就取它的上一级）。
    fn configured_dir() -> Option<PathBuf> {
        let path = configured()?;
        if path.is_dir() {
            return Some(path);
        }
        path.parent().map(|dir| dir.to_path_buf())
    }

    /// 程序目录下的 `Everything\`（便携版把整个文件夹拷过来）。
    fn bundled_dir() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("Everything"))
    }

    /// `Everything.exe` 可能出现的位置，按优先级排。
    fn exe_candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(path) = configured() {
            let is_exe = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
            out.push(if is_exe {
                path
            } else {
                path.join("Everything.exe")
            });
        }
        if let Some(dir) = bundled_dir() {
            out.push(dir.join("Everything.exe"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                out.push(dir.join("Everything.exe"));
            }
        }
        for base in [
            r"C:\Program Files\Everything",
            r"C:\Program Files (x86)\Everything",
        ] {
            out.push(PathBuf::from(base).join("Everything.exe"));
        }
        out
    }

    /// 找到的 `Everything.exe`（找不到就是 None）。
    pub fn exe_path() -> Option<PathBuf> {
        exe_candidates().into_iter().find(|path| path.is_file())
    }

    /// 把 Everything 拉起来。它是 GUI 程序，直接 spawn，不经过 cmd（不闪黑框）。
    pub fn start() -> bool {
        let Some(exe) = exe_path() else {
            return false;
        };
        Command::new(&exe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }

    /// 进程快照里有没有 `Everything.exe`（不看窗口，托盘里活着也算在运行）。
    pub fn is_running() -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };

        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while ok {
            let len = entry
                .szExeFile
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case("Everything.exe") {
                found = true;
                break;
            }
            ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        found
    }

    /// 可能放着 `Everything64.dll` 的地方：用户指定的目录 / 程序目录下的
    /// `Everything\` / exe 同目录 / Everything 安装目录 / 系统 PATH。
    fn dll_candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        let dirs: Vec<PathBuf> = configured_dir()
            .into_iter()
            .chain(bundled_dir())
            .chain(
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf())),
            )
            .collect();
        for dir in dirs {
            out.push(dir.join("Everything64.dll"));
            out.push(dir.join("Everything32.dll"));
        }
        // Everything 默认安装位置
        for base in [
            r"C:\Program Files\Everything",
            r"C:\Program Files (x86)\Everything",
        ] {
            let dir = PathBuf::from(base);
            out.push(dir.join("Everything64.dll"));
            out.push(dir.join("Everything32.dll"));
        }
        // 让系统按 PATH / system32 自己找
        out.push(PathBuf::from("Everything64.dll"));
        out.push(PathBuf::from("Everything32.dll"));
        out
    }

    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn proc_addr(lib: *mut std::ffi::c_void, name: &str) -> Option<usize> {
        let mut bytes = Vec::with_capacity(name.len() + 1);
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        let addr = unsafe { GetProcAddress(lib, bytes.as_ptr()) };
        addr.map(|f| f as usize)
    }

    fn load_sdk() -> Option<Sdk> {
        for candidate in dll_candidates() {
            let path: Vec<u16> = candidate
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let lib = unsafe { LoadLibraryW(path.as_ptr()) };
            if lib.is_null() {
                continue;
            }
            // 全部符号都在才认：缺一个就说明不是我们认识的 SDK 版本
            let Some(set_search) = proc_addr(lib, "Everything_SetSearchW") else {
                continue;
            };
            let Some(set_max) = proc_addr(lib, "Everything_SetMax") else {
                continue;
            };
            let Some(set_request_flags) = proc_addr(lib, "Everything_SetRequestFlags") else {
                continue;
            };
            let Some(set_match_path) = proc_addr(lib, "Everything_SetMatchPath") else {
                continue;
            };
            let Some(set_match_case) = proc_addr(lib, "Everything_SetMatchCase") else {
                continue;
            };
            let Some(set_regex) = proc_addr(lib, "Everything_SetRegex") else {
                continue;
            };
            let Some(query) = proc_addr(lib, "Everything_QueryW") else {
                continue;
            };
            let Some(get_num_results) = proc_addr(lib, "Everything_GetNumResults") else {
                continue;
            };
            let Some(get_full_path) = proc_addr(lib, "Everything_GetResultFullPathNameW") else {
                continue;
            };
            let Some(get_date_modified) = proc_addr(lib, "Everything_GetResultDateModified") else {
                continue;
            };
            let Some(get_last_error) = proc_addr(lib, "Everything_GetLastError") else {
                continue;
            };
            unsafe {
                return Some(Sdk {
                    _lib: lib as usize,
                    source: candidate.display().to_string(),
                    set_search: std::mem::transmute::<usize, SetSearchW>(set_search),
                    set_max: std::mem::transmute::<usize, SetMax>(set_max),
                    set_request_flags: std::mem::transmute::<usize, SetRequestFlags>(
                        set_request_flags,
                    ),
                    set_match_path: std::mem::transmute::<usize, SetMatchPath>(set_match_path),
                    set_match_case: std::mem::transmute::<usize, SetMatchCase>(set_match_case),
                    set_regex: std::mem::transmute::<usize, SetRegex>(set_regex),
                    query: std::mem::transmute::<usize, QueryW>(query),
                    get_num_results: std::mem::transmute::<usize, GetNumResults>(get_num_results),
                    get_full_path: std::mem::transmute::<usize, GetFullPathW>(get_full_path),
                    get_date_modified: std::mem::transmute::<usize, GetDateModified>(
                        get_date_modified,
                    ),
                    get_last_error: std::mem::transmute::<usize, GetLastError>(get_last_error),
                });
            }
        }
        None
    }

    /// 用 SDK 查一次。`Ok(None)` = SDK 不可用（让调用方去试 es.exe）。
    pub fn sdk_query(text: &str, limit: usize) -> Result<Option<Vec<Hit>>, String> {
        let Some(sdk) = sdk() else {
            return Ok(None);
        };
        let _guard = SDK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let query_wide = wide(text);
        unsafe {
            // 每次查询前把这几项显式设一遍：Everything 自己的设置里可能改过默认值，
            // 不覆盖就会出现「用户开了正则/整词匹配，我们查什么都查不到」。
            (sdk.set_search)(query_wide.as_ptr());
            (sdk.set_match_case)(0);
            (sdk.set_match_path)(0);
            (sdk.set_regex)(0);
            (sdk.set_request_flags)(REQUEST_FULL_PATH | REQUEST_DATE_MODIFIED);
            (sdk.set_max)(limit.clamp(1, 500) as u32);
            if (sdk.query)(1) == 0 {
                let code = (sdk.get_last_error)();
                return Err(match code {
                    // EVERYTHING_ERROR_IPC = 2：没找到 Everything 的窗口 —— 最常见，
                    // 就是「没装 / 没运行」，不算异常，给个能看懂的说明就行。
                    2 => {
                        // 设置里勾了「随本程序启动」就顺手拉起来：用户下一次查
                        // （多半是几秒后）就能用上，不用自己再去开一遍。
                        let _ = super::autostart_if_needed();
                        crate::i18n::t("Everything 没有运行（找不到 IPC 窗口）").to_string()
                    }
                    1 => crate::i18n::t("Everything 内存不足").to_string(),
                    3 => crate::i18n::t("Everything 正在重建数据库").to_string(),
                    4 => crate::i18n::t("Everything 不支持这个请求").to_string(),
                    8 => crate::i18n::t("Everything 查询超时").to_string(),
                    other => {
                        if crate::i18n::is_english() {
                            format!("Everything query failed (error {other})")
                        } else {
                            format!("Everything 查询失败（错误码 {other}）")
                        }
                    }
                });
            }
            let count = (sdk.get_num_results)().min(limit as u32);
            let mut out = Vec::with_capacity(count as usize);
            let mut buf = vec![0u16; 1024];
            for index in 0..count {
                let written = (sdk.get_full_path)(index, buf.as_mut_ptr(), buf.len() as u32);
                if written == 0 {
                    continue;
                }
                let len = (written as usize).min(buf.len().saturating_sub(1));
                let path = String::from_utf16_lossy(&buf[..len]);
                if path.is_empty() {
                    continue;
                }
                let mut modified = FILETIME {
                    dwLowDateTime: 0,
                    dwHighDateTime: 0,
                };
                let mtime = if (sdk.get_date_modified)(index, &mut modified) != 0 {
                    filetime_secs(&modified)
                } else {
                    0
                };
                out.push(Hit { path, mtime });
            }
            Ok(Some(out))
        }
    }

    /// FILETIME（1601 年起的 100ns）→ Unix 秒。读不到就是 0。
    fn filetime_secs(time: &FILETIME) -> u64 {
        let ticks = ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
        if ticks == 0 {
            return 0;
        }
        // 1601-01-01 → 1970-01-01 之间有 11644473600 秒
        (ticks / 10_000_000).saturating_sub(11_644_473_600)
    }

    /// `es.exe` 在哪儿（用户指定的目录 / 程序目录下的 `Everything\` / exe 同目录 /
    /// Everything 安装目录 / PATH）。
    pub fn es_path() -> Option<PathBuf> {
        let slot = ES.get_or_init(|| Mutex::new(Probe::default()));
        let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.tried {
            guard.tried = true;
            guard.value = find_es();
        }
        guard.value.clone()
    }

    fn find_es() -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(dir) = configured_dir() {
            candidates.push(dir.join("es.exe"));
        }
        if let Some(dir) = bundled_dir() {
            candidates.push(dir.join("es.exe"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("es.exe"));
            }
        }
        for base in [
            r"C:\Program Files\Everything",
            r"C:\Program Files (x86)\Everything",
        ] {
            candidates.push(PathBuf::from(base).join("es.exe"));
        }
        for path in candidates {
            if path.is_file() {
                return Some(path);
            }
        }
        // PATH 里有就直接交给系统解析（`Command::new("es")`）
        let found = Command::new("es")
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        if found.is_ok() {
            return Some(PathBuf::from("es"));
        }
        None
    }

    /// 用 es.exe 查一次。输出是「一行一个完整路径」。
    pub fn es_query(text: &str, limit: usize) -> Result<Vec<Hit>, String> {
        let Some(es) = es_path() else {
            return Err(crate::i18n::t("没有找到 Everything SDK（Everything64.dll）或 es.exe").to_string());
        };
        let output = Command::new(&es)
            .arg("-n")
            .arg(limit.clamp(1, 500).to_string())
            .arg(text)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|err| {
                if crate::i18n::is_english() {
                    format!("es.exe failed to start: {err}")
                } else {
                    format!("es.exe 启动失败：{err}")
                }
            })?;
        if !output.status.success() {
            // 和 SDK 那条一样：没在运行就看设置要不要顺手拉起来
            let _ = super::autostart_if_needed();
            let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if err.is_empty() {
                crate::i18n::t("es.exe 返回失败（Everything 没在运行？）").to_string()
            } else {
                format!("es.exe：{err}")
            });
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(|path| Hit {
                path: path.to_string(),
                // es 默认不带修改时间，就不去多要格式了（排序会退回按路径长度/时间 0）
                mtime: 0,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个「程序目录」，下面按需放 `Everything\` 里的文件。
    fn program_dir(tag: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join("runany-everything-test").join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in files {
            let path = dir.join("Everything").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        }
        dir
    }

    #[test]
    fn a_bundled_everything_folder_is_found_next_to_the_program() {
        let dir = program_dir("with-exe", &["Everything.exe", "Everything64.dll"]);
        let found = scan_dir(&dir).expect("应该扫到 Everything.exe");
        assert_eq!(found.file_name().unwrap(), "Everything.exe");
        assert_eq!(found.parent().unwrap(), dir.join("Everything"));
    }

    #[test]
    fn a_folder_with_only_the_dll_still_counts() {
        let dir = program_dir("only-dll", &["Everything64.dll"]);
        // 没有 exe 但有 DLL：返回整个目录（DLL 就从那儿加载）
        assert_eq!(scan_dir(&dir), Some(dir.join("Everything")));
    }

    #[test]
    fn no_everything_folder_means_nothing_found() {
        let dir = program_dir("empty", &[]);
        assert_eq!(scan_dir(&dir), None);
    }
}
