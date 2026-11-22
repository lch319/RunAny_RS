//! 菜单项配置文件：每套菜单存在 `menus/<名字>.runa` 里，可以随时换一套用。
//!
//! 为什么要把「菜单项」从 `settings.json` 里拆出来：
//! 配色、快捷键、热字符串这些是**这台机器**的偏好，换一套菜单项不该跟着变；
//! 反过来，别人给的一份菜单配置（`.runa`）拷进来就能直接切换着用，
//! 自己的菜单也不会被覆盖。所以两边分开放，各改各的。
//!
//! 文件格式就是 JSON（`name` / `next_id` / `items`），用独有的后缀 `.runa`
//! 只是为了「一眼能认出来这是菜单配置」，顺便避免和普通 json 撞名。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::settings::ItemSetting;

/// 配置文件后缀（runany 的菜单配置）。
pub const EXT: &str = "runa";
/// 第一次启动时自动建的那份配置叫这个。
pub const DEFAULT_NAME: &str = "默认";

/// `.runa` 文件的正文。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileFile {
    /// 显示用的名字（文件名可能被系统改过，这里存一份原始的）
    pub name: String,
    /// 新加菜单项的 id 序号
    pub next_id: u32,
    pub items: Vec<ItemSetting>,
}

/// `exe 同目录/menus`。
pub fn dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("menus"))
}

pub fn path_of(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    Some(dir()?.join(format!("{name}.{EXT}")))
}

/// 目录里现有的配置名（不含后缀），按名字排序。
pub fn list() -> Vec<String> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some(EXT) {
                return None;
            }
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_string())
        })
        .collect();
    // 「默认」永远排第一个，其余按名字排（中文按码位，够用了）
    names.sort_by(|a, b| {
        let a_default = a == DEFAULT_NAME;
        let b_default = b == DEFAULT_NAME;
        b_default.cmp(&a_default).then_with(|| a.cmp(b))
    });
    names
}

/// 把用户输入的名字洗成一个安全的文件名。
///
/// Windows 文件名里不能有 `\ / : * ? " < > |`，末尾还不能是点或空格；
/// 洗不出来（全是空白 / 非法字符）就返回 `None`，调用方给个提示。
pub fn sanitize(name: &str) -> Option<String> {
    let cleaned: String = name
        .trim()
        .chars()
        .filter(|ch| !matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .filter(|ch| !ch.is_control())
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// 名字被占用了就在后面加序号：`默认 副本` → `默认 副本 2` → `默认 副本 3`。
pub fn unique_name(base: &str) -> String {
    let base = sanitize(base).unwrap_or_else(|| DEFAULT_NAME.to_string());
    let existing = list();
    if !existing.iter().any(|name| name == &base) {
        return base;
    }
    for index in 2..10_000 {
        let candidate = format!("{base} {index}");
        if !existing.iter().any(|name| name == &candidate) {
            return candidate;
        }
    }
    base
}

/// 写一份配置（目录不存在就建）。
///
/// 托盘右键菜单**不会**写进这里：它属于这台机器（存 `settings.json`），
/// 换一套菜单配置不该让托盘图标的右键菜单跟着变。
pub fn save(name: &str, items: &[ItemSetting], next_id: u32) -> std::io::Result<()> {
    let Some(dir) = dir() else {
        return Err(std::io::Error::other(crate::i18n::t(crate::i18n::t("找不到 exe 目录"))));
    };
    std::fs::create_dir_all(&dir)?;
    let items: Vec<ItemSetting> = items
        .iter()
        .filter(|item| item.id != crate::settings::TRAY_ID)
        .cloned()
        .collect();
    let file = ProfileFile {
        name: name.to_string(),
        next_id,
        items,
    };
    let text = serde_json::to_string_pretty(&file)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    std::fs::write(dir.join(format!("{name}.{EXT}")), text)
}

/// 读一份配置。文件不在 / 读坏了都返回 `None`（调用方保持当前内容）。
pub fn load(name: &str) -> Option<ProfileFile> {
    let path = path_of(name)?;
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<ProfileFile>(&text) {
        Ok(file) => Some(file),
        Err(err) => {
            eprintln!("[popup-menu] 菜单配置 {name}.{EXT} 解析失败：{err}");
            None
        }
    }
}

/// 直接删文件。
pub fn delete(name: &str) -> std::io::Result<()> {
    let Some(path) = path_of(name) else {
        return Err(std::io::Error::other(crate::i18n::t(crate::i18n::t(crate::i18n::t("名字不合法")))));
    };
    std::fs::remove_file(path)
}

/// 改名（文件先挪，内容里的 `name` 下次保存时自动跟上）。
pub fn rename(old: &str, new: &str) -> std::io::Result<()> {
    let (Some(from), Some(to)) = (path_of(old), path_of(new)) else {
        return Err(std::io::Error::other(crate::i18n::t(crate::i18n::t("名字不合法"))));
    };
    std::fs::rename(from, to)
}
