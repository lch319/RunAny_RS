//! 配置备份：把 `settings.json` 和 `menus/*.runa` 整份复制到 `backups/<时间>/`。
//!
//! 为什么是「复制目录」而不是打包成一个文件：
//! 备份完用户多半要**看一眼 / 拷到别处**（换电脑、发给同事）。目录形式直接
//! 双击就能翻，不需要额外的解包工具；真出事了把文件拷回去就是恢复。
//!
//! 恢复前一定会先备份当前状态（`before-restore-<时间>`）——
//! 恢复错了一份还能再回来，这是「可逆」的最低要求。

use std::path::{Path, PathBuf};

use crate::settings;

/// `exe 同目录/backups`。
pub fn dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("backups"))
}

/// 一份备份的摘要（设置页的列表里显示这个）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// 目录名（也是恢复时用的 key）
    pub name: String,
    /// 里面有几个文件（settings.json 算一个）
    pub files: usize,
    /// 这批文件最后写的时间（显示用，`YYYY-MM-DD HH:MM`）
    pub at: String,
}

fn config_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(path) = settings::config_path() {
        if path.exists() {
            files.push(path);
        }
    }
    if let Some(path) = settings::fixes_path() {
        if path.exists() {
            files.push(path);
        }
    }
    if let Some(menus) = crate::menu_files::dir() {
        if let Ok(entries) = std::fs::read_dir(&menus) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some(crate::menu_files::EXT) {
                    files.push(path);
                }
            }
        }
    }
    files
}

/// 把当前配置复制到 `backups/<name>/`，返回实际用的目录名和文件数。
///
/// 同一秒里点两次「立即备份」会撞名，撞了就加 `-2`、`-3` ——
/// 直接覆盖会把上一份悄悄吃掉。
fn copy_into(name: &str) -> Result<(String, usize), String> {
    let root = dir().ok_or_else(|| crate::i18n::t("找不到 exe 目录").to_string())?;
    let mut name = name.to_string();
    let mut attempt = 2;
    while root.join(&name).exists() {
        name = format!("{name}-{attempt}");
        attempt += 1;
    }
    let target = root.join(&name);
    std::fs::create_dir_all(target.join("menus")).map_err(|err| err.to_string())?;
    let mut count = 0;
    for file in config_files() {
        let into = if file.file_name().and_then(|name| name.to_str()) == Some("settings.json") {
            target.join("settings.json")
        } else if file.extension().and_then(|ext| ext.to_str()) == Some("json") {
            // 与设置一起存的小文件（`program-fixes.json` 之类）保留原名
            target.join(
                file.file_name()
                    .map(|name| name.to_owned())
                    .unwrap_or_default(),
            )
        } else {
            target.join("menus").join(
                file.file_name()
                    .map(|name| name.to_owned())
                    .unwrap_or_default(),
            )
        };
        std::fs::copy(&file, &into).map_err(|err| format!("{}: {err}", file.display()))?;
        count += 1;
    }
    Ok((name, count))
}

/// 现在做一份备份，返回目录名。
pub fn create() -> Result<String, String> {
    Ok(copy_into(&timestamp())?.0)
}

/// 恢复之前的安全备份（名字带前缀，列表里和普通备份分得开）。
fn create_before_restore() -> Result<String, String> {
    let name = format!("before-restore-{}", timestamp());
    Ok(copy_into(&name)?.0)
}

/// 现有的备份（新的在前）。
pub fn list() -> Vec<Entry> {
    let Some(root) = dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            let files = count_files(&path);
            if files == 0 {
                return None;
            }
            Some(Entry {
                at: pretty_name(&name),
                name,
                files,
            })
        })
        .collect();
    // 按时间戳倒序 = 新的在前。不能直接比目录名：`before-restore-…` 的
    // 前缀是字母，字典序会压在所有日期前面，看上去像「最新的一份」。
    out.sort_by(|a, b| stamp(&b.name).cmp(stamp(&a.name)));
    out
}

/// 目录名里的时间戳部分（去掉 `before-restore-` 前缀和撞名后缀）。
fn stamp(name: &str) -> &str {
    name.trim_start_matches("before-restore-")
}

fn count_files(path: &Path) -> usize {
    let mut count = 0;
    if path.join("settings.json").exists() {
        count += 1;
    }
    if path.join("program-fixes.json").exists() {
        count += 1;
    }
    if let Ok(entries) = std::fs::read_dir(path.join("menus")) {
        count += entries
            .flatten()
            .filter(|entry| {
                entry.path().extension().and_then(|ext| ext.to_str())
                    == Some(crate::menu_files::EXT)
            })
            .count();
    }
    count
}

/// 用某一份备份覆盖当前配置（先自动备份当前状态）。
pub fn restore(name: &str) -> Result<String, String> {
    let root = dir().ok_or_else(|| crate::i18n::t("找不到 exe 目录").to_string())?;
    let source = root.join(name);
    if !source.is_dir() {
        return Err(crate::i18n::t("这份备份已经不在了").to_string());
    }
    let safety = create_before_restore()?;

    let settings_file = source.join("settings.json");
    if settings_file.exists() {
        let target = settings::config_path()
            .ok_or_else(|| crate::i18n::t("找不到 exe 目录").to_string())?;
        std::fs::copy(&settings_file, &target).map_err(|err| err.to_string())?;
    }
    // 「文件名」页那张表：备份里没有就保持现状
    let fixes = source.join("program-fixes.json");
    if fixes.exists() {
        if let Some(target) = settings::fixes_path() {
            std::fs::copy(&fixes, &target).map_err(|err| err.to_string())?;
        }
    }
    if let Some(menus) = crate::menu_files::dir() {
        std::fs::create_dir_all(&menus).map_err(|err| err.to_string())?;
        if let Ok(entries) = std::fs::read_dir(source.join("menus")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some(crate::menu_files::EXT) {
                    continue;
                }
                if let Some(file) = path.file_name() {
                    std::fs::copy(&path, menus.join(file)).map_err(|err| err.to_string())?;
                }
            }
        }
    }
    Ok(safety)
}

/// 打开备份目录（没有就建一个）。
pub fn open_dir() -> Result<(), String> {
    let root = dir().ok_or_else(|| crate::i18n::t("找不到 exe 目录").to_string())?;
    std::fs::create_dir_all(&root).map_err(|err| err.to_string())?;
    std::process::Command::new("explorer")
        .arg(root.as_os_str())
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// `2026-11-18_17-30-05`（本地时间；目录名可直接排序）。
fn timestamp() -> String {
    let now = crate::win32::local_time();
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        now.year, now.month, now.day, now.hour, now.minute, now.second
    )
}

/// `2026-11-18_17-30-05` → `2026-11-18 17:30`（去掉秒；恢复前的安全备份带前缀）。
fn pretty_name(name: &str) -> String {
    let trimmed = name.trim_start_matches("before-restore-");
    let mut parts = trimmed.splitn(2, '_');
    let date = parts.next().unwrap_or(trimmed);
    let time = parts.next().unwrap_or("");
    // `17-30-05` → `17:30`
    let short: String = time.chars().take(5).collect::<String>().replace('-', ":");
    if short.is_empty() {
        date.to_string()
    } else {
        format!("{date} {short}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_sort_newest_first() {
        // 目录名就是时间戳，字典序 == 时间序
        let mut names = vec!["2026-11-18_09-00-00", "2026-11-18_17-30-05"];
        names.sort_by(|a, b| b.cmp(a));
        assert_eq!(names[0], "2026-11-18_17-30-05");
    }

    #[test]
    fn safety_backups_still_sort_by_their_timestamp() {
        // `before-restore-` 前缀不能让它冒充「最新的一份」
        let mut names = vec![
            "before-restore-2026-11-18_09-00-00",
            "2026-11-18_17-30-05",
            "2026-11-17_08-00-00",
        ];
        names.sort_by(|a, b| stamp(b).cmp(stamp(a)));
        assert_eq!(
            names,
            vec![
                "2026-11-18_17-30-05",
                "before-restore-2026-11-18_09-00-00",
                "2026-11-17_08-00-00",
            ]
        );
    }

    #[test]
    fn pretty_name_drops_the_seconds() {
        assert_eq!(pretty_name("2026-11-18_17-30-05"), "2026-11-18 17:30");
        assert_eq!(
            pretty_name("before-restore-2026-11-18_09-05-01"),
            "2026-11-18 09:05"
        );
    }
}
