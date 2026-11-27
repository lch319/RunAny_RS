//! 动作参数里的内置占位符。
//!
//! 写动作时经常要拼「拿选中内容去搜一下」这种参数，所以参数文本里支持一批变量，
//! **点击菜单项、真正执行的那一刻**才替换成具体值（`{clipboard}` 这类要取实时值）。
//!
//! 语法就是 `{名字}`，不认识的原样保留，方便写 PowerShell / 正则里的大括号。
//!
//! 另外认得 RunAny 原版的老写法 `%getZz%`（不分大小写），它和 `{selection}`
//! 是同一个东西 —— 从 RunAny 导进来的动作里到处是它，用户也不必改写法。
//!
//! 换行问题：`{selection}` / `{clipboard}` 里可能有多行文本，直接塞进命令行会断，
//! 所以替换时把换行压成空格（和「发送文本」动作的处理保持一致）。

use crate::selection;
use crate::settings;
use crate::win32;

/// 可用变量：(名字, 说明)。设置页和编辑窗口都从这里取，不会再写第二份。
pub const VARS: &[(&str, &str)] = &[
    (
        "selection",
        "弹出菜单前抓到的选中内容（文字原文 / 文件路径）；也可以写成 %getZz%",
    ),
    (
        "selection_kind",
        "选中内容的类型：文字 / 文件 / 文件夹 / 图片（多个用空格隔开）",
    ),
    ("selection_count", "选中的文件个数（文字和图片是 0）"),
    ("clipboard", "点击时剪贴板里的文本"),
    ("exe_dir", "本程序所在目录"),
    ("exe_path", "本程序完整路径"),
    ("config", "settings.json 的完整路径"),
    ("date", "今天，如 2026-11-14"),
    ("time", "现在，如 18:15:27"),
    ("year", "年，如 2026"),
    ("month", "月，如 11"),
    ("day", "日，如 14"),
    ("cursor_x", "鼠标屏幕坐标 X"),
    ("cursor_y", "鼠标屏幕坐标 Y"),
    (
        "path",
        "把文件名解析成完整路径，如 {path: QQ.exe}（后面必须跟 : 和文件名）",
    ),
    (
        "env:NAME",
        "系统环境变量，如 {env:USERPROFILE}、{env:TEMP}；可给默认值 {env:XXX|没有时用这个}",
    ),
    (
        "rust:名字",
        "本程序自己算的值：pid / version / arch / now_unix / now_ms / tick / cpus / user / host / exe_stem / profile / menu_items",
    ),
    (
        "ahkvar:名字",
        "AutoHotkey v2 的内置变量（常用的一部分）：A_YYYY / A_MM / A_DD / A_Hour / A_Min / A_Sec / A_Now / A_NowUTC / A_Now / A_WDay / A_YDay / A_DDD / A_DDDD / A_TickCount / A_ScriptDir / A_ScriptName / A_Desktop / A_MyDocuments / A_Temp / A_WinDir / A_ProgramFiles / A_AppData / A_UserName / A_ComputerName / A_OSVersion / A_Language / A_IsAdmin / A_Clipboard / A_Space / A_Tab / A_LineEnding",
    ),
];

/// 编辑窗口里一排胶囊点着插入的「常用几个」。
pub const QUICK: &[&str] = &[
    "selection",
    "selection_count",
    "clipboard",
    "exe_dir",
    "date",
];

/// 把参数里的占位符换成实际值。
///
/// 先换 RunAny 老写法 `%getZz%`，再换 `{名字}` —— 两种写法可以混着用。
/// 这段文本用到了「选中内容」吗（`{selection}` 系列，或 RunAny 的 `%getZz%`）。
///
/// 用来决定「按快捷键跑这一项之前，要不要先发一次 Ctrl+C 去抓选中内容」。
/// `{clipboard}` 不算：它读的是现成的剪贴板，抓一次反而会把用户的东西盖掉。
pub fn mentions_selection(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("{selection") || lower.contains("%getzz%")
}

pub fn expand(input: &str) -> String {
    let input = expand_runany(input);
    expand_braces(&input)
}

/// 只换 `%getZz%`（不分大小写）→ 选中内容。
///
/// **脚本动作走这一条**：AHK 代码里的 `{` 是对象字面量 / 热键语法，
/// 不能整套占位符替换，但 RunAny 的 `%getZz%` 必须认。
pub fn expand_runany(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        // 只往后看一小段：`%...%` 里的名字很短，别把一整段代码当名字
        let close = tail
            .char_indices()
            .take(40)
            .find(|(_, ch)| *ch == '%')
            .map(|(index, _)| index);
        match close {
            Some(close) if tail[..close].trim().eq_ignore_ascii_case("getZz") => {
                out.push_str(&value("selection").unwrap_or_default());
                rest = &tail[close + 1..];
            }
            // 不是我们认识的变量：`%` 原样留着，继续往后找
            _ => {
                out.push('%');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `{名字}` 那一套（不认识的原样保留）。
fn expand_braces(input: &str) -> String {
    if !input.contains('{') {
        return input.to_string();
    }
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '{' {
            // 只往后面看一小段，避免把一整段 JSON 当占位符名
            let end = chars[index + 1..]
                .iter()
                .take(80)
                .position(|ch| *ch == '}')
                .map(|offset| index + 1 + offset);
            if let Some(end) = end {
                let name: String = chars[index + 1..end].iter().collect();
                if let Some(value) = value(&name) {
                    out.push_str(&value);
                    index = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// 单个变量的值；名字不认识返回 `None`（占位符原样保留）。
pub fn value(name: &str) -> Option<String> {
    let text = match name.trim() {
        "selection" => {
            // 菜单「置顶」着放了很久、或者是直接按快捷键执行某一项时，
            // 打开那一刻抓到的内容可能已经没了（或者根本没抓过）。
            // 这里补一次现场抓取，别让「用指定网址搜选中内容」发出一个空搜索。
            //
            // 另外：用户开了「执行前重新抓取」时，**无条件**重抓一次 ——
            // 置顶菜单放了一整天，期间又选了别的东西，他要的是后来选的那个。
            let current = if crate::settings::read(|settings| settings.recapture_before_run) {
                selection::capture_quick()
            } else {
                selection::current()
            };
            let current = if current.is_none() {
                selection::capture_quick()
            } else {
                current
            };
            current
                .map(|selection| selection.placeholder_text())
                .unwrap_or_default()
        }
        "selection_kind" => selection::current()
            .map(|selection| selection.kind_label())
            .unwrap_or_default(),
        "selection_count" => selection::current()
            .map(|selection| selection.files.len().to_string())
            .unwrap_or_else(|| "0".to_string()),
        "clipboard" => win32::read_clipboard_text().unwrap_or_default(),
        "exe_dir" => exe_dir(),
        "exe_path" => std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        "config" => settings::config_path_display().unwrap_or_default(),
        "date" => {
            let time = win32::local_time();
            format!("{:04}-{:02}-{:02}", time.year, time.month, time.day)
        }
        "time" => {
            let time = win32::local_time();
            format!("{:02}:{:02}:{:02}", time.hour, time.minute, time.second)
        }
        "year" => format!("{:04}", win32::local_time().year),
        "month" => format!("{:02}", win32::local_time().month),
        "day" => format!("{:02}", win32::local_time().day),
        "cursor_x" => win32::cursor_position().0.to_string(),
        "cursor_y" => win32::cursor_position().1.to_string(),
        // `{env:NAME}` / `{env:NAME|默认值}` —— 系统环境变量
        other if other.starts_with("env:") => {
            let rest = other["env:".len()..].trim();
            let (name, fallback) = match rest.split_once('|') {
                Some((name, fallback)) => (name.trim(), Some(fallback)),
                None => (rest, None),
            };
            if name.is_empty() {
                return None;
            }
            match std::env::var(name) {
                Ok(value) if !value.is_empty() => value,
                _ => fallback.unwrap_or_default().to_string(),
            }
        }
        // `{rust:名字}` —— 本程序自己算的几个值（用户不用懂 Rust，给的是现成的）
        other if other.starts_with("rust:") => rust_var(other["rust:".len()..].trim())?,
        // `{ahkvar:A_YYYY}`（也认 `{ahk:A_YYYY}`）—— AutoHotkey v2 的内置变量
        other if other.starts_with("ahkvar:") || other.starts_with("ahk:") => {
            let name = other.split_once(':').map(|(_, name)| name.trim()).unwrap_or("");
            ahk_var(name)?
        }
        // `{path: QQ.exe}` —— 把文件名解析成完整路径
        other if other.starts_with("path:") => {
            let file = other["path:".len()..].trim();
            if file.is_empty() {
                return None;
            }
            crate::progindex::resolve(file).unwrap_or_default()
        }
        _ => return None,
    };
    Some(flatten(text))
}

/// `{rust:...}` 支持的那些（都是现成的、不依赖用户懂 Rust）。
fn rust_var(name: &str) -> Option<String> {
    let value = match name {
        "pid" => std::process::id().to_string(),
        "version" => env!("CARGO_PKG_VERSION").to_string(),
        "arch" => std::env::consts::ARCH.to_string(),
        "os" => std::env::consts::OS.to_string(),
        "now_unix" => now_unix().to_string(),
        "now_ms" => (now_unix() * 1000).to_string(),
        // 开机以来的毫秒数（和 AHK 的 A_TickCount 同一个口径）
        "tick" => crate::win32::system_uptime_ms().to_string(),
        "cpus" => std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .to_string(),
        "user" => std::env::var("USERNAME").unwrap_or_default(),
        "host" => std::env::var("COMPUTERNAME").unwrap_or_default(),
        "exe_stem" => std::env::current_exe()
            .ok()
            .and_then(|path| path.file_stem().map(|stem| stem.to_string_lossy().to_string()))
            .unwrap_or_default(),
        "profile" => settings::read(|settings| settings.shown_profile()),
        "menu_items" => settings::read(|settings| {
            fn count(items: &[crate::settings::ItemSetting]) -> usize {
                items
                    .iter()
                    .map(|item| 1 + count(&item.children))
                    .sum()
            }
            count(&settings.items).to_string()
        }),
        _ => return None,
    };
    Some(flatten(value))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 星期几（1 = 周日 … 7 = 周六，和 AHK 的 `A_WDay` 一致）。
fn weekday(year: u16, month: u16, day: u16) -> u16 {
    // 基姆拉尔森公式（够用，不需要引入日期库）
    let (mut y, m) = (year as i32, month as i32);
    if m < 3 {
        y -= 1;
    }
    let k = y % 100;
    let j = y / 100;
    let h = (day as i32 + (13 * (m + 1)) / 5 + k + k / 4 + j / 4 + 5 * j) % 7;
    // h: 0 = 周六 … 6 = 周五 → 换成 1 = 周日 … 7 = 周六
    (((h + 6) % 7) + 1) as u16
}

/// `{ahkvar:...}`：把 AHK v2 的常用内置变量算出来。
///
/// 只覆盖**能算准的**那些（时间、路径、用户名、系统版本……）；`A_CaretX` 这种
/// 需要实时读别的窗口状态的没做 —— 认不出来就原样保留，用户一眼能看出没展开。
fn ahk_var(name: &str) -> Option<String> {
    let time = crate::win32::local_time();
    let upper = name.to_ascii_uppercase();
    let value = match upper.as_str() {
        "A_YYYY" => format!("{:04}", time.year),
        "A_MM" => format!("{:02}", time.month),
        "A_DD" => format!("{:02}", time.day),
        "A_HOUR" => format!("{:02}", time.hour),
        "A_MIN" => format!("{:02}", time.minute),
        "A_SEC" => format!("{:02}", time.second),
        // AHK 的 `A_Now` 格式：YYYYMMDDHH24MISS
        "A_NOW" => format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}",
            time.year, time.month, time.day, time.hour, time.minute, time.second
        ),
        "A_NOWUTC" => {
            let unix = now_unix();
            // 直接按 UTC 拆成年月日时：省一个时区库
            let (y, mo, d, h, mi, se) = utc_parts(unix);
            format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{se:02}")
        }
        "A_MDAY" => format!("{:02}", time.day),
        "A_WDAY" => weekday(time.year, time.month, time.day).to_string(),
        "A_YDAY" => yday(time.year, time.month, time.day).to_string(),
        "A_DDD" => match weekday(time.year, time.month, time.day) {
            1 => "Sun",
            2 => "Mon",
            3 => "Tue",
            4 => "Wed",
            5 => "Thu",
            6 => "Fri",
            _ => "Sat",
        }
        .to_string(),
        "A_DDDD" => match weekday(time.year, time.month, time.day) {
            1 => "Sunday",
            2 => "Monday",
            3 => "Tuesday",
            4 => "Wednesday",
            5 => "Thursday",
            6 => "Friday",
            _ => "Saturday",
        }
        .to_string(),
        "A_TICKCOUNT" => crate::win32::system_uptime_ms().to_string(),
        "A_SCRIPTDIR" => exe_dir(),
        "A_SCRIPTFULLPATH" | "A_SCRIPTNAME" => {
            let path = std::env::current_exe().ok()?;
            if upper == "A_SCRIPTNAME" {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default()
            } else {
                path.display().to_string()
            }
        }
        "A_DESKTOP" => user_dir("Desktop"),
        "A_MYDOCUMENTS" => user_dir("Documents"),
        "A_TEMP" => std::env::var("TEMP").unwrap_or_default(),
        "A_WINDIR" => std::env::var("SystemRoot").unwrap_or_default(),
        "A_PROGRAMFILES" => std::env::var("ProgramFiles").unwrap_or_default(),
        "A_APPDATA" => std::env::var("APPDATA").unwrap_or_default(),
        "A_USERNAME" => std::env::var("USERNAME").unwrap_or_default(),
        "A_COMPUTERNAME" => std::env::var("COMPUTERNAME").unwrap_or_default(),
        "A_OSVERSION" => std::env::var("OS").unwrap_or_else(|_| "Windows".to_string()),
        "A_LANGUAGE" => std::env::var("LANG").unwrap_or_default(),
        "A_ISADMIN" => crate::win32::is_elevated().to_string(),
        "A_CLIPBOARD" => crate::win32::read_clipboard_text().unwrap_or_default(),
        "A_SPACE" => " ".to_string(),
        "A_TAB" => "\t".to_string(),
        "A_LINEENDING" | "A_LINEEND" => "\r\n".to_string(),
        "A_AHKVERSION" => env!("CARGO_PKG_VERSION").to_string(),
        _ => return None,
    };
    Some(flatten(value))
}

fn user_dir(name: &str) -> String {
    std::env::var("USERPROFILE")
        .map(|base| format!("{base}\\{name}"))
        .unwrap_or_default()
}

/// 一年里的第几天（1 起）。
fn yday(year: u16, month: u16, day: u16) -> u16 {
    const DAYS: [u16; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let mut total = day;
    for (index, days) in DAYS.iter().enumerate() {
        if index as u16 + 1 < month {
            total += days + if index == 1 && leap { 1 } else { 0 };
        }
    }
    total
}

/// Unix 秒 → (年, 月, 日, 时, 分, 秒)，UTC。
fn utc_parts(unix: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (unix / 86_400) as i64;
    let secs = (unix % 86_400) as u32;
    // 1970-01-01 起的天数 → 年月日（civil_from_days 的常用写法）
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (
        year as i32,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
    )
}

/// 多行文本压成一行：占位符替换出来的东西大多要进命令行 / 单行输入框。
fn flatten(text: String) -> String {
    if !text.contains(['\n', '\r']) {
        return text;
    }
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

/// 程序所在目录（`exe_dir`）。
fn exe_dir() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.display().to_string()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{expand, expand_runany};

    #[test]
    fn unknown_placeholders_stay_as_they_are() {
        assert_eq!(expand("echo {not-a-var}"), "echo {not-a-var}");
        assert_eq!(expand("{}"), "{}");
        assert_eq!(expand("{ 中文 }"), "{ 中文 }");
    }

    /// RunAny 原版写法：`%getZz%` 和 `{selection}` 一个意思（不分大小写）。
    #[test]
    fn the_runany_selection_macro_is_the_same_as_selection() {
        for input in ["%getZz%", "%GETZZ%", "%getzz%"] {
            let out = expand(input);
            assert!(!out.contains('%'), "{input} 应该被认出来：{out:?}");
            assert_eq!(out, expand("{selection}"));
        }
        // 和普通文本混在一起时只换那一段
        let mixed = expand("cmd /c echo %getZz% > out.txt");
        assert!(!mixed.contains("getZz"));
        assert!(mixed.starts_with("cmd /c echo ") && mixed.ends_with(" > out.txt"));
    }

    /// 脚本只换 `%getZz%`：AHK 的大括号（v2 对象字面量）必须原样留着。
    #[test]
    fn script_expansion_touches_only_the_selection_macro() {
        let code = "Send \"{Enter}\"\nRun %getZz%\n";
        let out = expand_runany(code);
        assert!(out.contains("{Enter}"), "脚本里的大括号不能动：{out:?}");
        assert!(!out.contains("getZz"));
        // AHK 自己的 %变量% 不动
        assert_eq!(expand_runany("MsgBox %A_ScriptDir%"), "MsgBox %A_ScriptDir%");
        // 孤零零一个 % 也不能被吃掉
        assert_eq!(expand_runany("echo 100%"), "echo 100%");
    }

    #[test]
    fn text_without_placeholders_is_untouched() {
        assert_eq!(expand("notepad.exe"), "notepad.exe");
    }

    #[test]
    fn known_placeholders_are_replaced() {
        let dir = expand("{exe_dir}");
        assert!(!dir.contains('{'), "占位符应该被替换掉");
        let date = expand("{date}");
        assert_eq!(date.len(), 10, "日期应该是 YYYY-MM-DD：{date}");
        // 多个占位符 + 普通文本混在一起
        let mixed = expand("cd {exe_dir} && echo {year}");
        assert!(mixed.starts_with("cd "));
        assert!(!mixed.contains("{exe_dir}"));
        assert!(!mixed.contains("{year}"));
    }

    #[test]
    fn path_placeholder_keeps_its_form_when_unresolvable() {
        // 没有索引时，`{path:...}` 解析不到，但占位符形式本身该被识别、不该报错。
        // （resolve 返回空串时 expand 仍应替换，避免把 `{path:...}` 原样留在参数里）
        let empty = expand("{path: notepad.exe}");
        assert!(!empty.contains('{'), "path 占位符应该被替换（解析不到则为空）");
    }
}
