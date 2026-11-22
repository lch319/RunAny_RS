//! 「运行程序」动作的高级行为：管理员权限、已在运行时怎么处理、工作目录、窗口状态。
//!
//! 这几项都挂在 `ActionStep::params` 上（key 见 [`crate::settings::run_opts`]），
//! 所以老配置读进来一个字都不用改：没有这些参数就是「原样再启动一次」。
//!
//! ## 「已经在运行」是怎么判断的
//!
//! 按**exe 文件名**去进程快照里找（`Discord.exe` 这种），不看路径 ——
//! 用户从菜单里点的是哪个副本并不重要，同名进程活着就算在跑。
//! 找到之后再从这些进程的顶层窗口里挑一个来激活 / 关闭；
//! 完全没有窗口的（托盘程序）就只能「再启动一次」了，不然点了没反应。

use std::collections::BTreeMap;

use crate::settings::run_opts::{self, OnRun, WindowState};

/// 一条「运行程序」动作解析好的选项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub as_admin: bool,
    /// 追加到命令后面的参数
    pub args: String,
    pub on_run: OnRun,
    pub on_run_keys: String,
    pub workdir: Option<String>,
    pub window: WindowState,
}

impl Options {
    pub fn from_params(params: &BTreeMap<String, String>) -> Options {
        let get = |key: &str| {
            params
                .get(key)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Options {
            as_admin: matches!(params.get(run_opts::RUNAS).map(String::as_str), Some("1" | "true" | "on")),
            args: get(run_opts::ARGS).unwrap_or_default(),
            on_run: OnRun::from_key(params.get(run_opts::ONRUN).map(String::as_str).unwrap_or("")),
            on_run_keys: get(run_opts::ONRUN_KEYS).unwrap_or_default(),
            workdir: get(run_opts::WORKDIR),
            window: WindowState::from_key(
                params.get(run_opts::WINDOW).map(String::as_str).unwrap_or(""),
            ),
        }
    }

    /// 全是默认值时不用写进配置（省得每条动作都拖一串空参数）。
    pub fn is_default(&self) -> bool {
        !self.as_admin
            && self.args.trim().is_empty()
            && self.on_run == OnRun::Again
            && self.on_run_keys.is_empty()
            && self.workdir.is_none()
            && self.window == WindowState::Normal
    }

    /// 全清回「原样再启动一次」（面板上的「恢复默认」用）。
    pub fn reset(&mut self) {
        *self = Options {
            as_admin: false,
            args: String::new(),
            on_run: OnRun::Again,
            on_run_keys: String::new(),
            workdir: None,
            window: WindowState::Normal,
        };
    }

    /// 写回 `params`（默认值就把对应的 key 删掉）。
    pub fn write(&self, params: &mut BTreeMap<String, String>) {
        let mut set = |key: &str, value: Option<String>| match value {
            Some(value) => {
                params.insert(key.to_string(), value);
            }
            None => {
                params.remove(key);
            }
        };
        set(run_opts::RUNAS, self.as_admin.then(|| "1".to_string()));
        set(
            run_opts::ARGS,
            (!self.args.trim().is_empty()).then(|| self.args.trim().to_string()),
        );
        set(
            run_opts::ONRUN,
            (self.on_run != OnRun::Again).then(|| self.on_run.key().to_string()),
        );
        set(
            run_opts::ONRUN_KEYS,
            (self.on_run == OnRun::Keys)
                .then(|| self.on_run_keys.trim().to_string())
                .filter(|text| !text.is_empty()),
        );
        set(
            run_opts::WORKDIR,
            self.workdir
                .as_ref()
                .map(|dir| dir.trim().to_string())
                .filter(|dir| !dir.is_empty()),
        );
        set(
            run_opts::WINDOW,
            (self.window != WindowState::Normal).then(|| self.window.key().to_string()),
        );
    }

    /// 一行摘要（动作块底下那行小字）。
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.args.trim().is_empty() {
            parts.push(self.args.trim().to_string());
        }
        if self.as_admin {
            parts.push(crate::i18n::t("管理员").to_string());
        }
        if let Some(dir) = &self.workdir {
            if crate::i18n::is_english() {
                parts.push(format!("in {dir}"));
            } else {
                parts.push(format!("在 {dir} 里"));
            }
        }
        if self.window != WindowState::Normal {
            parts.push(self.window.label().to_string());
        }
        if self.on_run != OnRun::Again {
            parts.push(if crate::i18n::is_english() {
                format!("if running: {}", self.on_run.key())
            } else {
                format!("已在运行：{}", self.on_run.label())
            });
        }
        parts.join(" · ")
    }
}

/// 命令行里那个程序的文件名（`C:\Tools\a.exe -x` → `a.exe`）。
pub fn exe_name(command: &str) -> Option<String> {
    let (program, _) = crate::win32::split_command(command);
    let name = program.rsplit(['\\', '/']).next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// 执行一条「运行程序」动作。
///
/// `spawn_normal` 是原来的启动路径（走 shell、`CREATE_NO_WINDOW`）——
/// 没有要求管理员 / 工作目录 / 窗口状态时就还用它，`explorer .` 这类
/// shell 写法才不会突然打不开。
pub fn execute(
    command: &str,
    params: &BTreeMap<String, String>,
    spawn_normal: impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    let options = Options::from_params(params);
    if command.trim().is_empty() {
        return Ok(());
    }
    // 「参数」单独填的那一份接在程序后面
    let command = if options.args.trim().is_empty() {
        command.to_string()
    } else {
        format!("{} {}", command.trim(), options.args.trim())
    };
    let command = command.as_str();

    // 1) 已经在跑？按 exe 名字找
    if options.on_run != OnRun::Again {
        if let Some(name) = exe_name(command) {
            let pids = crate::win32::process_ids(&name);
            if !pids.is_empty() {
                let windows = crate::win32::top_windows(&pids);
                match options.on_run {
                    OnRun::Toggle => {
                        if let Some(hwnd) = windows.first() {
                            crate::win32::activate_or_minimize(*hwnd);
                            return Ok(());
                        }
                        // 没有窗口（托盘程序）：退化成再启动一次
                    }
                    OnRun::Close => {
                        if windows.is_empty() {
                            return Err(if crate::i18n::is_english() {
                                format!("{name} is running but has no window to close")
                            } else {
                                format!("{name} 在跑，但找不到可关闭的窗口")
                            });
                        }
                        for hwnd in windows {
                            crate::win32::close_window(hwnd);
                        }
                        return Ok(());
                    }
                    OnRun::Keys => {
                        let Some(hwnd) = windows.first() else {
                            return Err(if crate::i18n::is_english() {
                                format!("{name} is running but has no window to focus")
                            } else {
                                format!("{name} 在跑，但找不到可激活的窗口")
                            });
                        };
                        if options.on_run_keys.trim().is_empty() {
                            return Ok(());
                        }
                        // 先把它叫到前面，等窗口真的拿到焦点再按键
                        crate::win32::activate_or_minimize(*hwnd);
                        let keys = options.on_run_keys.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(220));
                            let _ = crate::win32::send_keys(&keys);
                        });
                        return Ok(());
                    }
                    OnRun::Again => {}
                }
            }
        }
    }

    // 2) 正常启动
    if options.as_admin || options.workdir.is_some() || options.window != WindowState::Normal {
        return crate::win32::launch_as(
            command,
            options.as_admin,
            options.workdir.as_deref(),
            options.window.key(),
        );
    }
    spawn_normal(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn empty_params_mean_the_old_behaviour() {
        let options = Options::from_params(&BTreeMap::new());
        assert!(!options.as_admin);
        assert_eq!(options.on_run, OnRun::Again);
        assert_eq!(options.window, WindowState::Normal);
        assert!(options.is_default());
    }

    #[test]
    fn round_trips_through_params() {
        let wanted = Options {
            as_admin: true,
            args: "-x 1".into(),
            on_run: OnRun::Keys,
            on_run_keys: "Ctrl+S".into(),
            workdir: Some("D:\\work".into()),
            window: WindowState::Minimized,
        };
        let mut map = BTreeMap::new();
        wanted.write(&mut map);
        assert_eq!(Options::from_params(&map), wanted);

        // 改回默认：参数要跟着删干净，不能留一串空值在配置里
        let mut map = map;
        Options::from_params(&BTreeMap::new()).write(&mut map);
        assert!(map.is_empty(), "默认值不该留在参数表里");
    }

    #[test]
    fn only_keeps_the_keys_of_the_chosen_mode() {
        let options = Options {
            args: String::new(),
            on_run: OnRun::Toggle,
            on_run_keys: "Ctrl+S".into(),
            ..Options::from_params(&BTreeMap::new())
        };
        let mut map = BTreeMap::new();
        options.write(&mut map);
        assert!(!map.contains_key(run_opts::ONRUN_KEYS), "不是「发按键」就不该存按键");
    }

    #[test]
    fn exe_name_takes_the_last_component() {
        assert_eq!(exe_name("C:\\Tools\\a.exe -x").as_deref(), Some("a.exe"));
        assert_eq!(
            exe_name("\"C:\\Program Files\\B\\b.exe\" --fast").as_deref(),
            Some("b.exe")
        );
        assert_eq!(exe_name("notepad.exe").as_deref(), Some("notepad.exe"));
        assert_eq!(exe_name("   ").as_deref(), None);
    }

    #[test]
    fn default_options_still_go_through_the_old_launcher() {
        let seen = std::cell::RefCell::new(Vec::new());
        execute("notepad.exe", &BTreeMap::new(), |line| {
            seen.borrow_mut().push(line.to_string());
            Ok(())
        })
        .expect("默认路径不该失败");
        assert_eq!(*seen.borrow(), vec!["notepad.exe".to_string()]);
    }

    #[test]
    fn args_are_stuck_onto_the_command() {
        let seen = std::cell::RefCell::new(Vec::new());
        let map = params(&[(run_opts::ARGS, "--fast  -x")]);
        execute("tool.exe", &map, |line| {
            seen.borrow_mut().push(line.to_string());
            Ok(())
        })
        .expect("接参数不该失败");
        assert_eq!(*seen.borrow(), vec!["tool.exe --fast  -x".to_string()]);
    }

    #[test]
    fn an_empty_command_does_nothing() {
        let called = std::cell::Cell::new(false);
        execute("   ", &BTreeMap::new(), |_| {
            called.set(true);
            Ok(())
        })
        .expect("空命令不算错");
        assert!(!called.get(), "空命令不该去启动任何东西");
    }

    #[test]
    fn reset_goes_back_to_the_defaults() {
        let mut options = Options {
            as_admin: true,
            args: "-x".into(),
            on_run: OnRun::Close,
            on_run_keys: "Ctrl+S".into(),
            workdir: Some("D:\\work".into()),
            window: WindowState::Maximized,
        };
        options.reset();
        assert!(options.is_default(), "恢复默认之后要跟没配过一样");
    }
}
