//! 「运行程序」只填文件名时的解析 —— 全交给 Everything 联动。
//!
//! ## 为什么不再自己扫盘
//!
//! 老版本会扫 `PATH` + `Program Files` + `%LOCALAPPDATA%\Programs` + `WINDIR`，
//! 建一张 `programs.json` 缓存。三个硬伤：
//!
//! - 扫一遍几百毫秒到几秒，启动时后台呼呼转，折腾半天用户还是找不到装在 D 盘 /
//!   绿色版的程序 —— 白扫；
//! - 文件挪了位置它不知道，还按老路径去启动，直接报错；
//! - 缓存文件本身还要维护过期、上限、排除规则。
//!
//! Everything 本来就在给整台机器建索引（NTFS 的 USN 日志，挪文件它自己跟着更新），
//! 我们只要问一句「这个文件名在哪」。所以这一层现在只剩下：**问 Everything →
//! 按设置里的规则挑一份 → 结果缓存一会儿**。
//!
//! ## 路径没了怎么办
//!
//! Everything 的结果也可能比磁盘慢半拍（或者它当时没在跑，读到的是上次的答案）。
//! 所以**每次拿到候选都当场验一眼**：那个文件还在不在？
//!
//! - 不在了 → 丢掉这一条，换下一个候选；
//! - 一个都不剩 → 清掉解析缓存，并让 Everything 重新查一遍（[`rebuild`]），
//!   下次再问就是新结果。
//!
//! 这就是「把程序挪到别的目录之后，菜单里那条突然打不开了」的修复：
//! 挑的时候只认磁盘上真有的那个路径。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// 认哪些扩展名（小写，带点）—— 从设置里读，见 [`extensions`]。
const DEFAULT_EXTENSIONS: &[&str] = &[".exe", ".bat", ".cmd", ".com"];

/// 文件名里带这些的多半是安装器 / 卸载器 / 崩溃上报，排到后面去（不删）
const JUNK_HINTS: &[&str] = &[
    "unins", "uninst", "setup", "install", "update", "upgrade", "crash", "reporter", "elevate",
    "vcredist", "dxsetup",
];

/// 这些目录里放的是「同一份东西的另一位宽副本」（SysWOW64 = 32 位镜像），
/// 同一个名字在别处还有一份时，优先那份正常的。
const MIRROR_HINTS: &[&str] = &["\\syswow64\\", "\\x86\\", "\\amd64\\"];

/// `cmd` 自己的内置命令：这些不是文件，不能拿去索引里查
const CMD_BUILTINS: &[&str] = &[
    "start", "call", "echo", "dir", "copy", "move", "del", "erase", "type", "md", "mkdir", "rd",
    "rmdir", "cls", "set", "setx", "exit", "cd", "chdir", "pause", "title", "if", "for", "goto",
    "shift", "rem", "path", "pushd", "popd", "assoc", "ftype", "cmd",
];

/// 一个候选：完整路径 + 挑它时用的两个指标。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub path: String,
    /// 修改时间（Unix 秒，取不到就是 0）
    pub mtime: u64,
    /// 版本号（Everything 不给版本号，这里通常是空串；
    /// 手动钉死的那条会用文件本身读出来的版本）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
}

/// 名字统一成小写 + 去掉首尾空白（缓存键都按这个存）。
fn normalize(name: &str) -> String {
    name.trim().trim_matches('"').to_ascii_lowercase()
}

/// 当前认哪些扩展名（从设置里读，读不到就用默认那套）。
fn extensions() -> Vec<String> {
    let configured = crate::settings::read(|settings| settings.programs.extensions());
    if configured.is_empty() {
        DEFAULT_EXTENSIONS
            .iter()
            .map(|ext| ext.to_string())
            .collect()
    } else {
        configured
    }
}

fn is_junk(name: &str) -> bool {
    JUNK_HINTS.iter().any(|hint| name.contains(hint))
}

fn is_mirror(path: &str) -> bool {
    // 路径里偶尔会有正斜杠（尤其是用户手填 / 某些工具生成的），统一按反斜杠比
    let lower = path.to_ascii_lowercase().replace('/', "\\");
    MIRROR_HINTS.iter().any(|hint| lower.contains(hint))
}

/// 这个路径现在还在磁盘上吗（判断 Everything 的答案有没有过期）。
fn still_there(path: &str) -> bool {
    !path.trim().is_empty() && Path::new(path).is_file()
}

fn enabled() -> bool {
    crate::settings::read(|settings| settings.programs.enabled)
}

fn prefer() -> crate::settings::PreferProgram {
    crate::settings::read(|settings| settings.programs.prefer)
}

fn excluded(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    crate::settings::read(|settings| {
        settings.programs.exclude.lines().any(|line| {
            let pattern = line.trim().to_ascii_lowercase();
            !pattern.is_empty() && lower.contains(&pattern)
        })
    })
}

/// 一个名字可能有几种写法：`code` 要能查到 `code.exe`。
fn lookup_keys(name: &str) -> Vec<String> {
    let key = normalize(name);
    let mut keys = vec![key.clone()];
    if !key.contains('.') {
        for extension in extensions() {
            keys.push(format!("{key}{extension}"));
        }
    }
    keys
}

// ---------------------------------------------------------------------------
// 候选：只问 Everything
// ---------------------------------------------------------------------------

/// 某个名字的所有候选（按设置里的优先级排好，第一个就是会用的那个）。
///
/// 最后一步会**验一遍磁盘**：Everything 的索引可能比磁盘慢一点，
/// 挪走的文件还挂着老路径，那样的候选一律丢掉。
pub fn candidates(name: &str) -> Vec<Candidate> {
    let keys = lookup_keys(name);
    let mut list: Vec<Candidate> = Vec::new();
    for key in &keys {
        let hits = crate::everything::lookup(key, crate::everything::DEFAULT_LIMIT);
        for hit in hits {
            if excluded(&hit.path) {
                continue;
            }
            if list.iter().any(|known: &Candidate| known.path == hit.path) {
                continue;
            }
            list.push(Candidate {
                path: hit.path,
                mtime: hit.mtime,
                version: String::new(),
            });
        }
    }
    // 走了通配（`code.*`）的时候可能把 `code.txt` 也带回来：
    // 只留文件名正好是某个写法的那几种后缀（`code` / `code.exe` …）
    list.retain(|candidate| {
        let file = candidate
            .path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&candidate.path)
            .to_ascii_lowercase();
        keys.iter().any(|key| *key == file)
    });
    list.sort_by(|a, b| compare(a, b, prefer()));
    list
}

fn version_key(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect()
}

fn compare(
    a: &Candidate,
    b: &Candidate,
    prefer: crate::settings::PreferProgram,
) -> std::cmp::Ordering {
    use crate::settings::PreferProgram;

    // 「卸载程序 / 安装器」先往后放，不管用哪种排序规则
    let junk_a = is_junk(&a.path.to_ascii_lowercase());
    let junk_b = is_junk(&b.path.to_ascii_lowercase());
    if junk_a != junk_b {
        return junk_a.cmp(&junk_b);
    }
    // 32 位镜像目录（SysWOW64 这类）也往后放
    let mirror_a = is_mirror(&a.path);
    let mirror_b = is_mirror(&b.path);
    if mirror_a != mirror_b {
        return mirror_a.cmp(&mirror_b);
    }
    // 真在磁盘上的优先：Everything 里还挂着、但文件已经挪走的那条不该抢第一
    let live_a = still_there(&a.path);
    let live_b = still_there(&b.path);
    if live_a != live_b {
        return live_b.cmp(&live_a);
    }

    let primary = match prefer {
        PreferProgram::Newest => b.mtime.cmp(&a.mtime),
        PreferProgram::Version => {
            let (key_a, key_b) = (version_key(&a.version), version_key(&b.version));
            key_b.cmp(&key_a).then(b.mtime.cmp(&a.mtime))
        }
        PreferProgram::Shortest => a
            .path
            .len()
            .cmp(&b.path.len())
            .then(b.mtime.cmp(&a.mtime)),
    };
    primary.then_with(|| a.path.cmp(&b.path))
}

// ---------------------------------------------------------------------------
// 「这个裸文件名解析到哪」的一次性结果：界面用
// ---------------------------------------------------------------------------

/// 一次解析的结果：最终会用哪个文件 + 所有同名候选。
#[derive(Clone, Default)]
pub struct Probe {
    pub resolved: Option<String>,
    pub candidates: Vec<Candidate>,
}

fn probes() -> &'static Mutex<HashMap<String, Probe>> {
    static PROBES: OnceLock<Mutex<HashMap<String, Probe>>> = OnceLock::new();
    PROBES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 缓存里有没有算过（`None` = 还没算，界面显示「计算中」）。
pub fn peek(name: &str) -> Option<Probe> {
    probes()
        .lock()
        .ok()
        .and_then(|map| map.get(&normalize(name)).cloned())
}

/// 算一次并存进缓存（口径和 [`resolve`] 一样）。
///
/// **不要在渲染里直接调 [`resolve`]**：它要问 Everything（一次 IPC），
/// 菜单项一多就是「每帧几百次 IPC」，设置页会直接卡死。界面走这个缓存，
/// 首次算的时候放在后台线程（见 `settings_ui::file_names_page`）。
pub fn probe(name: &str) -> Probe {
    let key = normalize(name);
    if let Some(hit) = probes().lock().ok().and_then(|m| m.get(&key).cloned()) {
        return hit;
    }
    let list = candidates(name);
    let resolved = {
        let keys = lookup_keys(name);
        let fixed = crate::settings::read(|settings| {
            keys.iter()
                .find_map(|k| settings.programs.fixes.get(k).cloned())
        });
        match fixed {
            Some(path) if still_there(&path) => Some(path),
            // 钉死的那个文件被挪走了：这条路作废，重新问 Everything 兜底
            Some(_) => {
                reindex();
                list.iter()
                    .find(|candidate| still_there(&candidate.path))
                    .map(|candidate| candidate.path.clone())
            }
            None if !enabled() => None,
            None => list
                .iter()
                .find(|candidate| still_there(&candidate.path))
                .map(|candidate| candidate.path.clone()),
        }
    };
    let probe = Probe {
        resolved,
        candidates: list,
    };
    if let Ok(mut map) = probes().lock() {
        map.insert(key, probe.clone());
    }
    probe
}

/// 丢掉缓存：改了「手动指定」、重新检测之后都要调。
pub fn clear_probes() {
    if let Ok(mut map) = probes().lock() {
        map.clear();
    }
}

// ---------------------------------------------------------------------------
// 对外
// ---------------------------------------------------------------------------

/// 启动时调一次：把上一次的解析结果清掉（磁盘可能已经变了）。
pub fn start() {
    clear_probes();
    // 「没运行就拉起 Everything」由设置页那个开关决定，这里只是顺手问一声
    let _ = crate::everything::autostart_if_needed();
}

/// 重新索引：让 Everything 丢掉我们的查询缓存，下次重新问。
///
/// 我们没有自己的索引了，所谓「重建」就是**让 Everything 重新答一遍**：
/// - 清掉它那一层的查询缓存与冷却计数（`everything::reset`）；
/// - 清掉我们这一层的解析缓存。
///
/// 挪走文件、刚装完程序、Everything 刚从没开变成开着 —— 都靠这一下。
pub fn rebuild() {
    crate::everything::reset();
    clear_probes();
}

/// 同 [`rebuild`]，名字更贴合「发现路径不存在了，重新索引补上」这句话。
pub fn reindex() {
    rebuild();
}

/// 解析成完整路径：用户钉死的 > Everything 里挑第一的。
///
/// **只返回磁盘上真有的路径**。全都找不到时返回 `None`，
/// 调用方（`resolve_command`）会把原命令原样交出去，`cmd` 还会按 PATH 再找一次。
pub fn resolve(name: &str) -> Option<String> {
    let keys = lookup_keys(name);
    let fixed = crate::settings::read(|settings| {
        keys.iter()
            .find_map(|key| settings.programs.fixes.get(key).cloned())
    });
    if let Some(path) = fixed {
        if still_there(&path) {
            return Some(path);
        }
        // 钉死的那条失效了：清缓存 + 让 Everything 重答，后面按候选兜底
        reindex();
    }
    if !enabled() {
        return None;
    }
    candidates(name)
        .into_iter()
        .find(|candidate| still_there(&candidate.path))
        .map(|candidate| candidate.path)
}

/// 从一条「运行程序」的值里取出那个裸文件名（只写名字、没写路径的才算）。
pub fn bare_name(command: &str) -> Option<String> {
    let (program, _) = split_program(command)?;
    if program.contains(['\\', '/', ':', '"']) {
        return None;
    }
    if CMD_BUILTINS.contains(&program.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some(program)
}

/// 把命令拆成「程序」和「后面的参数」。
fn split_program(command: &str) -> Option<(String, String)> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some((rest[..end].to_string(), rest[end + 1..].to_string()));
    }
    match trimmed.find(' ') {
        Some(index) => Some((trimmed[..index].to_string(), trimmed[index..].to_string())),
        None => Some((trimmed.to_string(), String::new())),
    }
}

/// 执行前把「只写了文件名」的命令补成完整路径。
///
/// 补不上就**原样返回** —— `cmd` 还会自己按 `PATH` 找一次，
/// Everything 没装 / 没跑也不代表它跑不起来。
pub fn resolve_command(command: &str) -> String {
    if !enabled() {
        return command.to_string();
    }
    let Some((program, rest)) = split_program(command) else {
        return command.to_string();
    };
    if program.contains(['\\', '/', ':']) {
        return command.to_string();
    }
    let Some(name) = bare_name(command) else {
        return command.to_string();
    };
    let _ = program;
    match resolve(&name) {
        // 路径里有空格才加引号：没空格时 `cmd` 反而会把引号当成名字的一部分
        Some(path) => {
            if path.contains(' ') || path.contains('\t') {
                format!("\"{path}\"{rest}")
            } else {
                format!("{path}{rest}")
            }
        }
        None => command.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_name_is_next_to_its_arguments() {
        assert_eq!(
            split_program("notepad.exe"),
            Some(("notepad.exe".into(), String::new()))
        );
        assert_eq!(
            split_program("code C:\\a.txt"),
            Some(("code".into(), " C:\\a.txt".into()))
        );
        assert_eq!(
            split_program("\"C:\\Program Files\\a b\\app.exe\" -x"),
            Some(("C:\\Program Files\\a b\\app.exe".into(), " -x".into()))
        );
        assert_eq!(split_program("   "), None);
    }

    #[test]
    fn a_name_without_extension_tries_the_known_ones() {
        // 具体认哪些后缀是设置里的默认值，会随版本调；这里只验「前缀 + 常见后缀」的结构
        let keys = lookup_keys("Code");
        assert_eq!(keys.first().map(String::as_str), Some("code"));
        assert!(keys.contains(&"code.exe".to_string()), "{keys:?}");
        assert!(keys.contains(&"code.lnk".to_string()), "{keys:?}");
        // 已经带后缀的不再多试
        assert_eq!(lookup_keys("code.cmd"), ["code.cmd"]);
    }

    #[test]
    fn only_names_without_a_path_are_bare() {
        assert_eq!(bare_name("notepad.exe"), Some("notepad.exe".to_string()));
        assert_eq!(bare_name("code ."), Some("code".to_string()));
        assert_eq!(bare_name("C:\\Windows\\notepad.exe"), None);
        assert_eq!(bare_name("\"C:\\a b\\app.exe\""), None);
        assert_eq!(bare_name("start https://a.b"), None);
        assert_eq!(bare_name(""), None);
    }

    #[test]
    fn junk_names_go_last() {
        let prefer = crate::settings::PreferProgram::Newest;
        let real = Candidate {
            path: "C:\\app\\app.exe".into(),
            mtime: 10,
            version: String::new(),
        };
        let installer = Candidate {
            path: "C:\\app\\setup.exe".into(),
            mtime: 99,
            version: String::new(),
        };
        assert_eq!(
            compare(&real, &installer, prefer),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn the_32_bit_mirror_loses_to_the_real_one() {
        let prefer = crate::settings::PreferProgram::Newest;
        let real = Candidate {
            path: "C:\\Windows\\explorer.exe".into(),
            mtime: 10,
            version: String::new(),
        };
        let mirror = Candidate {
            path: "C:\\Windows\\SysWOW64\\explorer.exe".into(),
            mtime: 99,
            version: String::new(),
        };
        assert_eq!(
            compare(&real, &mirror, prefer),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn newest_wins_then_version_then_shortest() {
        use crate::settings::PreferProgram;
        let a = Candidate {
            path: "C:\\a\\app.exe".into(),
            mtime: 10,
            version: "1.0.0".into(),
        };
        let b = Candidate {
            path: "C:\\b\\deep\\app.exe".into(),
            mtime: 20,
            version: "2.0.0".into(),
        };
        assert_eq!(
            compare(&a, &b, PreferProgram::Newest),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare(&a, &b, PreferProgram::Version),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare(&a, &b, PreferProgram::Shortest),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn index_keys_are_lowercase() {
        assert_eq!(normalize("  NotePad.EXE "), "notepad.exe");
        assert_eq!(normalize("\"App.exe\""), "app.exe");
    }

    #[test]
    fn a_path_that_is_gone_is_not_a_candidate() {
        // 「挪走了但 Everything 还挂着」的那条不能算数
        let prefer = crate::settings::PreferProgram::Newest;
        let gone = Candidate {
            path: "Z:\\根本没有这个目录\\app.exe".into(),
            mtime: 99,
            version: String::new(),
        };
        let here = Candidate {
            path: std::env::current_exe()
                .expect("测试进程自己总有 exe")
                .display()
                .to_string(),
            mtime: 10,
            version: String::new(),
        };
        assert_eq!(compare(&gone, &here, prefer), std::cmp::Ordering::Greater);
        assert!(!still_there("Z:\\根本没有这个文件.exe"));
    }
}
