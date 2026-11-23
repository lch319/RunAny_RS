//! 「自动规则」：启动后按间隔检测环境条件，命中就执行某个菜单项。
//!
//! 对应 RunCtrl 那种「规则组」：一组条件（与 / 或）+ 循环次数 + 间隔秒数。
//! 变量都从本机取（开机时长、WiFi 名、电脑名、IP、时间……），不依赖网络。

use std::sync::OnceLock;

/// 变量表：`(存盘用的键, 界面上显示的名字, 说明)`
pub static VARIABLES: &[(&str, &str, &str)] = &[
    ("uptime", "开机时长(秒)", "系统起来多少秒了"),
    ("computer", "电脑名", ""),
    ("user", "用户名", ""),
    ("online", "联网状态", "已联网 / 未联网"),
    ("lan_ip", "内网IP", "第一块网卡的 IPv4"),
    ("wifi", "WiFi名", "当前连着的 SSID（没连给空）"),
    ("os", "系统版本", "比如 10.0.19045"),
    ("is64", "系统64位", "是 / 否"),
    ("time", "本地时间", "HH:MM:SS"),
    ("hour", "时", "0~23"),
    ("minute", "分", "0~59"),
    ("second", "秒", "0~59"),
    ("day", "日", "1~31"),
    ("month", "月", "1~12"),
    ("year", "年", "四位"),
    ("weekday", "星期", "1~7（1 = 星期一）"),
    ("running", "进程运行状态", "值填 exe 名；条件写「相等」= 正在运行，「不等」= 没运行"),
];

/// 比较方式：`(存盘键, 界面名)`
pub static OPS: &[(&str, &str)] = &[
    ("eq", "相等"),
    ("ne", "不等"),
    ("contains", "包含"),
    ("not_contains", "不包含"),
    ("gt", "大于"),
    ("lt", "小于"),
];

/// 这个变量该用什么方式填。
pub enum InputKind {
    /// 自由填
    Text,
    /// 数字
    Number,
    /// 从几个固定值里挑
    Choice(&'static [&'static str]),
}

pub fn input_kind(key: &str) -> InputKind {
    match key {
        "online" => InputKind::Choice(&["已联网", "未联网"]),
        "is64" => InputKind::Choice(&["是", "否"]),
        "uptime" | "hour" | "minute" | "second" | "day" | "month" | "year" | "weekday" => {
            InputKind::Number
        }
        _ => InputKind::Text,
    }
}

/// 这个变量「现在是多少」，界面上挂在输入框下面当例子。
pub fn current_example(key: &str) -> String {
    match key {
        "running" => "填 exe 名，比如 QQ.exe".to_string(),
        "wifi" => {
            let ssid = probe("wifi", "");
            if ssid.is_empty() {
                "当前没连 WiFi".to_string()
            } else {
                format!("当前：{ssid}")
            }
        }
        "lan_ip" => {
            let ip = probe("lan_ip", "");
            if ip.is_empty() {
                "当前取不到内网 IP".to_string()
            } else {
                format!("当前：{ip}")
            }
        }
        _ => {
            let value = probe(key, "");
            if value.is_empty() {
                String::new()
            } else {
                format!("当前：{value}")
            }
        }
    }
}

pub fn variable_label(key: &str) -> &'static str {
    VARIABLES
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, label, _)| *label)
        .unwrap_or("（未选择）")
}

pub fn op_label(key: &str) -> &'static str {
    OPS.iter()
        .find(|(k, _)| *k == key)
        .map(|(_, label)| *label)
        .unwrap_or("相等")
}

/// 取一个变量的当前值。`arg` 只有「进程运行状态」用得上（填 exe 名）。
pub fn probe(key: &str, arg: &str) -> String {
    match key {
        "uptime" => format!("{}", uptime_secs()),
        "computer" => std::env::var("COMPUTERNAME").unwrap_or_default(),
        "user" => std::env::var("USERNAME").unwrap_or_default(),
        "online" => {
            if online() {
                "已联网".to_string()
            } else {
                "未联网".to_string()
            }
        }
        "lan_ip" => lan_ip().unwrap_or_default(),
        "wifi" => wifi_ssid().unwrap_or_default(),
        "os" => os_version(),
        "is64" => {
            if cfg!(target_pointer_width = "64") {
                "是".to_string()
            } else {
                "否".to_string()
            }
        }
        "time" | "hour" | "minute" | "second" | "day" | "month" | "year" | "weekday" => {
            let now = local_now();
            match key {
                "time" => format!("{:02}:{:02}:{:02}", now.hour, now.minute, now.second),
                "hour" => format!("{}", now.hour),
                "minute" => format!("{}", now.minute),
                "second" => format!("{}", now.second),
                "day" => format!("{}", now.day),
                "month" => format!("{}", now.month),
                "year" => format!("{}", now.year),
                // AHK 的 A_WDay 是 1=周日；这里用 ISO：1=周一 … 7=周日
                _ => format!("{}", if now.weekday == 0 { 7 } else { now.weekday }),
            }
        }
        // 「进程运行状态」比较的是状态本身，值里填的是 exe 名
        "running" => {
            let exe = arg.trim();
            if exe.is_empty() {
                return String::new();
            }
            if process_running(exe) {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        _ => String::new(),
    }
}

/// 一条条件是否成立。
///
/// 「进程运行状态」单独处理：条件值里填 exe 名，比较的却是「在不在跑」，
/// 所以 `eq` = 正在运行、`ne` = 没在跑。
pub fn condition_holds(var: &str, op: &str, value: &str) -> bool {
    if var == "running" {
        let running = probe("running", value) == "1";
        return match op {
            "ne" | "not_contains" => !running,
            _ => running,
        };
    }
    let actual = probe(var, value);
    compare(&actual, op, value)
}

fn compare(actual: &str, op: &str, expected: &str) -> bool {
    let (a, e) = (actual.trim(), expected.trim());
    match op {
        "eq" => a.eq_ignore_ascii_case(e),
        "ne" => !a.eq_ignore_ascii_case(e),
        "contains" => a.to_lowercase().contains(&e.to_lowercase()),
        "not_contains" => !a.to_lowercase().contains(&e.to_lowercase()),
        "gt" | "lt" => {
            let (Ok(a), Ok(e)) = (a.parse::<f64>(), e.parse::<f64>()) else {
                return false;
            };
            if op == "gt" {
                a > e
            } else {
                a < e
            }
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 取值
// ---------------------------------------------------------------------------

fn uptime_secs() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    unsafe { GetTickCount64() / 1000 }
}

fn online() -> bool {
    // windows-sys 这个 feature 没开，自己声明（wininet.dll）
    #[link(name = "wininet")]
    extern "system" {
        fn InternetGetConnectedState(flags: *mut u32, reserved: u32) -> i32;
    }
    const LAN: u32 = 0x2;
    const MODEM: u32 = 0x1;
    unsafe {
        let mut flags = 0u32;
        if InternetGetConnectedState(&mut flags, 0) == 0 {
            return false;
        }
        flags & (LAN | MODEM) != 0
    }
}

/// 内网 IP：拿一个 UDP socket「连」一下外网地址（不发包），读本端地址。
fn lan_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}

/// 当前 WiFi 的 SSID：`netsh wlan show interfaces` 里挑一行 `SSID`。
///
/// 不用 `wlanapi`：那套 API 要开句柄、枚举接口、解 XML 一样的结构，代码量是这的十倍。
/// 这里只在规则求值时跑一次，开销可以接受（每轮最多一次，且结果缓存 5 秒）。
fn wifi_ssid() -> Option<String> {
    static CACHE: OnceLock<std::sync::Mutex<Option<(String, std::time::Instant)>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some((value, at)) = guard.as_ref() {
            if at.elapsed() < std::time::Duration::from_secs(5) {
                return if value.is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
            }
        }
    }

    let output = std::process::Command::new("netsh")
        .args(["wlan", "show", "interfaces"])
        .creation_flags_no_window()
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let mut ssid = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        // 只认 `SSID` 那一行（BSSID 也会匹配，所以要求冒号前正好是 SSID）
        if let Some(rest) = trimmed.strip_prefix("SSID") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix(':') {
                ssid = value.trim().to_string();
                break;
            }
        }
    }
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((ssid.clone(), std::time::Instant::now()));
    }
    if ssid.is_empty() {
        None
    } else {
        Some(ssid)
    }
}

fn os_version() -> String {
    // 用 RtlGetVersion：`GetVersionEx` 在 Win8.1 之后会对未声明兼容的进程撒谎
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }
    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }
    let mut info = OsVersionInfo {
        size: std::mem::size_of::<OsVersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    unsafe {
        RtlGetVersion(&mut info);
    }
    format!("{}.{}.{}", info.major, info.minor, info.build)
}

/// 拆开的本地时间（`weekday`：0 = 周日）
struct Now {
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    weekday: u16,
}

fn local_now() -> Now {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut time: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut time) };
    Now {
        year: time.wYear,
        month: time.wMonth,
        day: time.wDay,
        hour: time.wHour,
        minute: time.wMinute,
        second: time.wSecond,
        weekday: time.wDayOfWeek,
    }
}

/// 某个 exe 名（可带 .exe，也可不带）现在有没有在跑。
pub fn process_running(exe: &str) -> bool {
    let needle = exe.trim().to_lowercase();
    if needle.is_empty() {
        return false;
    }
    let needle = if needle.ends_with(".exe") {
        needle
    } else {
        format!("{needle}.exe")
    };
    crate::win32::running_process_names()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&needle))
}

/// 让 `Command` 不弹黑框（Windows 上的 `netsh` 调用）。
trait NoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

impl NoWindow for std::process::Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x0800_0000);
        }
        self
    }
}

// ---------------------------------------------------------------------------
// 调度
// ---------------------------------------------------------------------------

use crate::settings;

/// 一条规则的条件是否成立。
pub fn evaluate(rule: &settings::AutoRule) -> bool {
    if rule.conditions.is_empty() {
        return false;
    }
    if rule.any {
        rule.conditions
            .iter()
            .any(|cond| condition_holds(&cond.var, &cond.op, &cond.value))
    } else {
        rule.conditions
            .iter()
            .all(|cond| condition_holds(&cond.var, &cond.op, &cond.value))
    }
}

/// 启动时调：每条规则按「轮次 × 间隔」自己跑，命中就执行菜单项。
///
/// 用 `cx.spawn` 挂在界面上：程序一退出，这些等待自然跟着结束，
/// 不用额外管线程回收。
pub fn start(cx: &mut gpui::App) {
    let (enabled, rules) = settings::read(|s| (s.auto_enabled, s.auto_rules.clone()));
    if !enabled || rules.is_empty() {
        return;
    }
    for rule in rules.into_iter().filter(|rule| {
        rule.enabled && !rule.target.trim().is_empty() && !rule.conditions.is_empty()
    }) {
        let rounds = rule.rounds.max(1);
        let interval = rule.interval_secs.max(1) as u64;
        let label = if rule.name.trim().is_empty() {
            rule.target.clone()
        } else {
            rule.name.clone()
        };
        cx.spawn(async move |cx| {
            for round in 0..rounds {
                if round > 0 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(interval))
                        .await;
                }
                if evaluate(&rule) {
                    let _ = cx.update(|app| fire(app, &rule.target, &label));
                    break;
                }
            }
        })
        .detach();
    }
}

/// 命中：把这条规则自己的菜单项按顺序跑一遍，并按设置弹一条提示。
fn fire(cx: &mut gpui::App, target: &str, label: &str) {
    // 规则的菜单项挂在根层的隐藏容器里（`__rule__<规则id>`）
    let (actions, rule_name) = settings::read(|settings| {
        let rule = settings
            .auto_rules
            .iter()
            .find(|rule| rule.target == target || rule.name == label || rule.id == target);
        match rule {
            Some(rule) => {
                let mut actions = Vec::new();
                for item in settings.rule_items(&rule.id).iter().filter(|item| item.enabled) {
                    actions.extend(item.actions.iter().cloned());
                }
                (actions, rule.name.clone())
            }
            None => (Vec::new(), label.to_string()),
        }
    });

    if !actions.is_empty() {
        if settings::read(|settings| settings.auto_notice) {
            crate::hint::show(
                cx,
                vec![crate::hotstring::HintLine {
                    text: format!(
                        "自动规则「{}」已执行",
                        if rule_name.trim().is_empty() { label } else { &rule_name }
                    ),
                    dim: true,
                    id: String::new(),
                }],
            );
        }
        crate::popup::run_actions_now(cx, actions);
        return;
    }

    // 老写法兜底：target 指向主菜单里的一项
    let resolved = settings::read(|settings| {
        if settings.item_by_id(target).is_some() {
            Some(target.to_string())
        } else {
            settings
                .items
                .iter()
                .find(|item| item.label == target)
                .map(|item| item.id.clone())
        }
    });
    let Some(id) = resolved else {
        crate::hint::show(
            cx,
            vec![crate::hotstring::HintLine {
                text: format!("自动规则「{label}」还没配要执行的菜单项"),
                dim: false,
                id: String::new(),
            }],
        );
        return;
    };
    if settings::read(|settings| settings.auto_notice) {
        crate::hint::show(
            cx,
            vec![crate::hotstring::HintLine {
                text: format!("自动规则「{label}」已执行"),
                dim: true,
                id: String::new(),
            }],
        );
    }
    crate::popup::run_item_now(cx, &id);
}

/// 一行摘要，给规则卡片下面那行小字用。
pub fn summarize(rule: &settings::AutoRule) -> String {
    if rule.conditions.is_empty() {
        return "还没有条件".to_string();
    }
    let joiner = if rule.any { " 或 " } else { " 且 " };
    let parts: Vec<String> = rule
        .conditions
        .iter()
        .map(|cond| {
            let value = if cond.var == "running" {
                format!("{} {}", cond.value, if cond.op == "ne" { "没在跑" } else { "在跑" })
            } else {
                format!("{} {}", op_label(&cond.op), cond.value)
            };
            format!("{} {}", variable_label(&cond.var), value)
        })
        .collect();
    parts.join(joiner)
}

// ---------------------------------------------------------------------------
// 文本格式（设置页里直接编辑的就是这段文本）
// ---------------------------------------------------------------------------

/// 规则列表 → 一段文本（每块之间用 `---` 隔开）。
///
/// 界面已经改成可视化卡片了，这两个函数留着做「导入 / 导出规则」用。
#[allow(dead_code)]
pub fn rules_to_text(rules: &[settings::AutoRule]) -> String {
    let mut out = String::new();
    for (index, rule) in rules.iter().enumerate() {
        if index > 0 {
            out.push_str("---\n");
        }
        if !rule.name.trim().is_empty() {
            out.push_str(&format!("名称: {}\n", rule.name));
        }
        if !rule.target.trim().is_empty() {
            out.push_str(&format!("执行: {}\n", rule.target));
        }
        out.push_str(&format!("轮次: {}\n间隔: {}\n", rule.rounds.max(1), rule.interval_secs));
        out.push_str(if rule.any { "组合: 或\n" } else { "组合: 与\n" });
        out.push_str("条件:\n");
        for cond in &rule.conditions {
            out.push_str(&format!("  {} {} {}\n", cond.var, cond.op, cond.value));
        }
    }
    out
}

/// 反过来：文本 → 规则列表。认不出的行直接跳过（宁可少一条，也别整段丢掉）。
#[allow(dead_code)]
pub fn text_to_rules(text: &str) -> Vec<settings::AutoRule> {
    let mut rules: Vec<settings::AutoRule> = Vec::new();
    let mut current = settings::AutoRule::default();
    let mut has_any = false;

    let mut flush = |rule: &mut settings::AutoRule, has: &mut bool, out: &mut Vec<settings::AutoRule>| {
        if *has {
            if rule.id.trim().is_empty() {
                rule.id = format!("auto-{}", out.len() + 1);
            }
            out.push(rule.clone());
        }
        *rule = settings::AutoRule::default();
        *has = false;
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "---" {
            flush(&mut current, &mut has_any, &mut rules);
            continue;
        }
        // `键: 值`
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            let value = value.trim().to_string();
            match key {
                "名称" | "名字" => {
                    current.name = value;
                    has_any = true;
                }
                "执行" | "菜单项" => {
                    current.target = value;
                    has_any = true;
                }
                "轮次" | "次数" => {
                    current.rounds = value.parse().unwrap_or(1).max(1);
                    has_any = true;
                }
                "间隔" => {
                    current.interval_secs = value.parse().unwrap_or(10);
                    has_any = true;
                }
                "组合" => {
                    current.any = value.contains('或');
                    has_any = true;
                }
                "条件" => has_any = true,
                _ => {}
            }
            continue;
        }
        // 条件行：`变量 比较 值`
        let mut parts = trimmed.splitn(3, char::is_whitespace);
        let (Some(var), Some(op)) = (parts.next(), parts.next()) else {
            continue;
        };
        let value = parts.next().unwrap_or("").trim().to_string();
        if !VARIABLES.iter().any(|(key, _, _)| *key == var) {
            continue;
        }
        let op = if OPS.iter().any(|(key, _)| *key == op) {
            op.to_string()
        } else {
            "eq".to_string()
        };
        current.conditions.push(settings::AutoCondition {
            var: var.to_string(),
            op,
            value,
        });
        has_any = true;
    }
    flush(&mut current, &mut has_any, &mut rules);
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_are_sane() {
        assert!(!probe("computer", "").is_empty());
        assert!(!probe("user", "").is_empty());
        assert!(!probe("uptime", "").is_empty());
        assert!(probe("is64", "") == "是" || probe("is64", "") == "否");
        assert!(!probe("time", "").is_empty());
    }

    #[test]
    fn text_round_trip() {
        let text = "名称: A\n执行: B\n轮次: 3\n间隔: 5\n组合: 或\n条件:\n  wifi eq X\n  uptime gt 60\n";
        let rules = text_to_rules(text);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "A");
        assert_eq!(rules[0].target, "B");
        assert_eq!(rules[0].rounds, 3);
        assert_eq!(rules[0].interval_secs, 5);
        assert!(rules[0].any);
        assert_eq!(rules[0].conditions.len(), 2);
        let back = rules_to_text(&rules);
        assert!(back.contains("wifi eq X"));
        // 再解析一遍结果要一样（幂等）
        let again = text_to_rules(&back);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].conditions, rules[0].conditions);
    }

    #[test]
    fn conditions_compare() {
        assert!(condition_holds("uptime", "gt", "0"));
        assert!(condition_holds("online", "ne", "不可能的状态"));
        assert!(!condition_holds("uptime", "lt", "0"));
    }
}
