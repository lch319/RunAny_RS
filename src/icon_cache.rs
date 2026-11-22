//! 菜单图标的**磁盘**缓存：`exe 同目录/Icon/` 下放抠好的 32px PNG。
//!
//! ## 为什么要有它
//!
//! 菜单项只写了程序名（`WeChat.exe`）时，想画出图标得先知道程序在哪：
//! 走 `PATH` → 本地小索引 → Everything，一次几十毫秒。一千多项、每次呼出都问一遍，
//! 菜单就得等好几秒。原来的缓存只在内存里（`icon.rs` 里那几个 `thread_local`），
//! 换个线程、重启一次就全丢。
//!
//! 这里把结论落到磁盘上：
//!
//! * `Icon/icons.tsv` —— 一行一条 `动作原文<Tab>png 文件名`（`-` = 确认过没有图标），
//!   追加写，不重写整个文件，一千多项也只有几十 KB；
//! * `Icon/<哈希>.png` —— 从 exe / lnk 里抠出来的 32×32 图标。
//!
//! 之后每次呼出只剩「查表 + 量一下文件还在不在」，跟程序搜索彻底无关。
//!
//! 删掉整个 `Icon` 目录就等于「全部重来」，升级了某个程序想换图标也这么干。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// 图标目录名（exe 同目录下）。
pub const FOLDER: &str = "Icon";
/// 索引文件名。
const INDEX: &str = "icons.tsv";
thread_local! {
    /// 一次菜单构建里会对同一个程序问很多遍（每一项都问），这里挡一手。
    static SEEN: std::cell::RefCell<HashMap<String, Option<String>>> =
        std::cell::RefCell::new(HashMap::new());
}

fn dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.join(FOLDER);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).ok()?;
        }
        Some(dir)
    })
    .clone()
}

/// 索引：动作原文 → png 文件名（或 `-`）。
fn index() -> &'static Mutex<HashMap<String, String>> {
    static INDEX_MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    INDEX_MAP.get_or_init(|| Mutex::new(read_index()))
}

fn read_index() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(path) = dir().map(|dir| dir.join(INDEX)) else {
        return map;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return map;
    };
    for line in text.lines() {
        let Some((key, file)) = line.split_once('\t') else {
            continue;
        };
        if key.is_empty() || file.is_empty() {
            continue;
        }
        map.insert(key.to_string(), file.to_string());
    }
    map
}

/// 追加一行（不重写整个文件 —— 一千多项重写一次就是几十毫秒）。
fn remember(key: &str, file: &str) {
    index().lock().unwrap().insert(key.to_string(), file.to_string());
    let Some(path) = dir().map(|dir| dir.join(INDEX)) else {
        return;
    };
    use std::io::Write;
    if let Ok(mut handle) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        // 同一项反复写同一条也无所谓：读的时候后写的覆盖先写的
        let _ = writeln!(handle, "{key}\t{file}");
    }
}

/// 动作原文当索引用。换行 / 连续空白在菜单项里没有意义，先压平再说。
fn key_of(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 给一条「运行程序」动作找一个能直接画的图标位。
///
/// 命中磁盘缓存就是一次查表 + 一次 `exists`；没命中才去解析程序、抠图标、
/// 存成 png 并记进索引。
///
/// **只记「有图标」的**，不记「查过没有」：本地程序索引第一次可能还没建好，
/// 这时候问「WeChat.exe 在哪」会答不上来，记成「没有」就永远画不出图标了。
pub fn badge_for(value: &str) -> Option<String> {
    let key = key_of(value);
    if key.is_empty() {
        return None;
    }
    if let Some(hit) = SEEN.with(|seen| seen.borrow().get(&key).cloned()) {
        return hit;
    }
    let found = match lookup(&key) {
        Some(icon) => icon,
        None => build(&key),
    };
    SEEN.with(|seen| seen.borrow_mut().insert(key, found.clone()));
    found
}

/// `Some(None)` = 查过，这条路没有图标；`None` = 索引里没有，得现算。
fn lookup(key: &str) -> Option<Option<String>> {
    let file = index().lock().unwrap().get(key)?.clone();
    let path = dir()?.join(&file);
    // png 被删了（用户清了 Icon 目录）就当没缓存，重新抠一张
    Some(path.exists().then(|| path.to_string_lossy().into_owned()))
}

fn build(key: &str) -> Option<String> {
    // 插件函数、网址、占位符这里会直接 None（`icon::program_of` 里那串
    // 一眼不是程序名的字符）；真的程序名才去 PATH / 程序索引里查。
    let program = crate::icon::program_of(key)?;
    let dir = dir()?;
    let file = format!("{:016x}.png", fnv1a(&program));
    let path = dir.join(&file);
    if !path.exists() {
        let (width, height, rgba) = crate::win32::icon_pixels(&program, 32)?;
        let buffer = image::RgbaImage::from_raw(width, height, rgba)?;
        // 抠不出来（没有图标的 dll / 被删的程序）就走下面的失败分支
        buffer.save(&path).ok()?;
    }
    remember(key, &file);
    Some(path.to_string_lossy().into_owned())
}

/// 程序路径 → 文件名。改路径就换文件，改名没事。
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 后台把一整棵树里「运行程序」的图标都抠一遍。
///
/// 调用方（导入之后 / 启动之后）放到线程里跑：第一次要解析几百个程序名，
/// 在主线程上做就是「点了导入卡住两秒」。跑完之后再呼出菜单就只剩查表了。
///
/// 名字解析现在走 Everything（一次 IPC，几毫秒到几十毫秒），所以这里不再等
/// 「本地索引扫完」—— 已经没有那个东西了。
pub fn warm(items: &[crate::settings::ItemSetting], auto_icons: bool) {
    fn walk(items: &[crate::settings::ItemSetting], auto_icons: bool) {
        for item in items {
            if item.badge.trim().is_empty() && item.auto_icon.unwrap_or(auto_icons) {
                if let Some(step) = item
                    .actions
                    .iter()
                    .find(|step| step.kind == crate::settings::ActionKind::Run)
                {
                    let _ = badge_for(&step.value);
                }
            }
            walk(&item.children, auto_icons);
        }
    }
    let started = std::time::Instant::now();
    walk(items, auto_icons);
    if std::env::var_os("GPUI_MENU_DEBUG_ICONLOG").is_some() {
        println!("[icon] warm 用 {:?}（裸文件名交给 Everything 解析）", started.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_call_is_answered_from_memory() {
        let value = "XiaoYao_plus[RA_plus](%getZz%,,23)";
        assert_eq!(badge_for(value), None);
        // 第二次必须走内存缓存：这时候再去解析一遍就是白等
        let start = std::time::Instant::now();
        for _ in 0..200 {
            assert_eq!(badge_for(value), None);
        }
        assert!(start.elapsed().as_millis() < 50, "{:?}", start.elapsed());
        // 但没有「没有图标」这种结论落到磁盘上：程序索引没建好时问什么都是 None，
        // 那个结论不能当真
        let key = key_of(value);
        assert!(index().lock().unwrap().get(&key).is_none());
    }

    #[test]
    fn a_real_program_gets_a_png() {
        let exe = std::env::current_exe().expect("测试进程自己总有 exe");
        let badge = badge_for(&exe.to_string_lossy());
        // 非 Windows 上没有抠图标这一步，只要求「不炸」
        if cfg!(windows) {
            let badge = badge.expect("自己这个 exe 应该有图标");
            assert!(badge.ends_with(".png"), "{badge}");
            assert!(std::path::Path::new(&badge).exists());
        }
    }

    #[test]
    fn the_key_squeezes_whitespace() {
        assert_eq!(key_of("  a.exe   --flag "), "a.exe --flag");
    }
}
