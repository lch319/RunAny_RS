//! 开机自启：注册表 `Run` 键 / 任务计划程序两条路。
//!
//! 为什么用 `reg.exe` / `schtasks.exe` 而不是直接调 Win32：
//! 注册表要额外开 `windows-sys` 的 Registry feature，任务计划要 COM 一大套接口；
//! 两个命令行工具在所有 Windows 版本上行为一致，出错时给的提示也是现成的，
//! 这点开销（进程启动一次，几十毫秒）只在用户点开关时才付。
//!
//! 「启动时以管理员运行」只有**任务计划**这条路能静默做到（`/RL HIGHEST`）；
//! 注册表 `Run` 键启动的进程继承普通权限，要提权只能弹 UAC —— 所以界面上
//! 打开「管理员运行」会自动把自启方式切成任务计划。

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Command, Output};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 注册表 Run 键下的值名。
const RUN_VALUE: &str = "RunAny_RS";
/// 任务计划里的任务名。
pub const TASK_NAME: &str = "RunAny_RS";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn run_tool(program: &str, args: &[&str]) -> Option<Output> {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.output().ok()
}

fn run_ok(program: &str, args: &[&str]) -> Result<(), String> {
    match run_tool(program, args) {
        Some(output) if output.status.success() => Ok(()),
        Some(output) => {
            let text = String::from_utf8_lossy(&output.stderr);
            let text = text.trim();
            Err(if text.is_empty() {
                format!("{program} 退出码 {:?}", output.status.code())
            } else {
                text.to_string()
            })
        }
        None => Err(format!("启动不了 {program}")),
    }
}

/// 注册表里那个值现在指向哪个 exe（`None` = 没注册）。
pub fn run_value() -> Option<String> {
    let output = run_tool("reg", &["query", RUN_KEY, "/v", RUN_VALUE])?;
    if !output.status.success() {
        return None;
    }
    // 输出形如：`    RunAny_RS    REG_SZ    C:\...\RunAny_RS.exe`
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with(RUN_VALUE) {
            continue;
        }
        let mut parts = line.split_whitespace();
        parts.next();
        parts.next();
        let rest: Vec<&str> = parts.collect();
        if rest.is_empty() {
            return Some(String::new());
        }
        return Some(rest.join(" "));
    }
    Some(String::new())
}

/// 写 / 删注册表 `Run` 键。
pub fn set_run(enabled: bool, exe: &str) -> Result<(), String> {
    if enabled {
        run_ok(
            "reg",
            &[
                "add",
                RUN_KEY,
                "/v",
                RUN_VALUE,
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{exe}\""),
                "/f",
            ],
        )
    } else {
        // 本来就没有：`reg delete` 会报错，这里当成功
        let _ = run_tool("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
        Ok(())
    }
}

/// 任务计划里有没有这个任务。
pub fn task_registered() -> bool {
    run_tool("schtasks", &["/Query", "/TN", TASK_NAME])
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// 建（或重建）登录时启动的任务计划。
///
/// `/RL HIGHEST` = 以最高权限运行（静默提权，不弹 UAC）；
/// 不给的时候用 `/RL LIMITED`，行为跟注册表那条一样。
pub fn set_task(enabled: bool, exe: &str, as_admin: bool) -> Result<(), String> {
    if !enabled {
        let _ = run_tool("schtasks", &["/Delete", "/TN", TASK_NAME, "/F"]);
        return Ok(());
    }
    let level = if as_admin { "HIGHEST" } else { "LIMITED" };
    run_ok(
        "schtasks",
        &[
            "/Create",
            "/TN",
            TASK_NAME,
            "/TR",
            &format!("\"{exe}\""),
            "/SC",
            "ONLOGON",
            "/RL",
            level,
            "/F",
        ],
    )
}

/// 现在的自启状态（给界面显示用）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// 注册表 Run 键指向的 exe（空串 = 有值但读不出路径，`None` = 没注册）
    pub run: Option<String>,
    pub task: bool,
}

pub fn status() -> Status {
    Status {
        run: run_value(),
        task: task_registered(),
    }
}

/// 缓存一份状态：`status()` 要起一两个命令行进程，不能在每帧渲染里调。
static CACHED: std::sync::Mutex<Option<Status>> = std::sync::Mutex::new(None);

/// 读缓存（第一次调用时查一次）。
pub fn cached_status() -> Status {
    if let Ok(cache) = CACHED.lock() {
        if let Some(status) = cache.as_ref() {
            return status.clone();
        }
    }
    let fresh = status();
    if let Ok(mut cache) = CACHED.lock() {
        *cache = Some(fresh.clone());
    }
    fresh
}

/// 丢掉缓存，下次 `cached_status()` 重新查（改完开关时调）。
pub fn refresh_status() {
    if let Ok(mut cache) = CACHED.lock() {
        *cache = None;
    }
}

/// 按设置把系统状态对齐（幂等：已经对上了就只是几次查询）。
///
/// 启动时调一次，用户改开关时调一次 —— 这样 exe 被挪了位置也会自动更新。
///
/// 关掉的时候**先查再删**：没注册过就一个命令都不跑。否则每次开机启动都会
/// 起两个命令行进程去删两个本来就不存在的东西，既慢又容易在受限环境里报错。
pub fn apply(enabled: bool, use_task: bool, as_admin: bool) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|err| format!("读不到自己的路径：{err}"))?
        .display()
        .to_string();
    if !enabled {
        if run_value().is_some() {
            set_run(false, &exe)?;
        }
        if task_registered() {
            set_task(false, &exe, false)?;
        }
        return Ok(());
    }
    if use_task {
        // 任务计划这条路：Run 键上别留一份，不然登录时会起两个
        if run_value().is_some() {
            set_run(false, &exe)?;
        }
        set_task(true, &exe, as_admin)?;
    } else {
        if task_registered() {
            set_task(false, &exe, false)?;
        }
        set_run(true, &exe)?;
    }
    Ok(())
}
