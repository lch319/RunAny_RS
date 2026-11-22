//! 菜单项的图标位：一个字符 / emoji，或者一个图片文件。
//!
//! 判定很简单 —— 看字符串的扩展名：
//!
//! | 写法 | 画成什么 |
//! | --- | --- |
//! | `+` / `★` / `🔍` | 文字（原样画出来，颜色跟主题） |
//! | `C:\icons\save.svg` | 矢量图，由 gpui 的 svg 渲染器画（**单色**，`text_color` 给它着色） |
//! | `C:\icons\save.ico` / `.png` / … | 位图，解码成 `RenderImage` 交给 gpui，保留原色 |
//!
//! 位图解码走 `image` crate（gpui 自己的 `ImageFormat` 里没有 ico）。
//! 解码结果按「路径 + 修改时间」缓存在线程本地，菜单每帧都要画，
//! 不能每帧都去读盘解码。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::prelude::*;
use gpui::{div, img, px, svg, AnyElement, Hsla, ObjectFit, RenderImage, SharedString};

/// 能当图标用的图片扩展名。
const IMAGE_EXTENSIONS: [&str; 7] = ["svg", "ico", "png", "jpg", "jpeg", "bmp", "webp"];

/// 这些扩展名不是图片，但能**从里面把图标抠出来**（Windows 的 shell 图标）。
const SHELL_EXTENSIONS: [&str; 6] = ["exe", "lnk", "dll", "com", "bat", "cmd"];

/// 这个字符串是不是一个图片文件名（而不是要画成文字的字形）。
pub fn is_image(text: &str) -> bool {
    extension(text).is_some()
}

/// 这个字符串是不是「能从程序里抠图标」的路径。
fn is_shell_icon(text: &str) -> bool {
    extension(text)
        .map(|ext| SHELL_EXTENSIONS.contains(&ext.as_str()))
        .unwrap_or(false)
}

/// 从一条「运行程序」动作里取程序文件（有就返回它的完整路径）。
///
/// 认这几种写法：
/// - `notepad.exe` —— 到 `PATH` 里找，找不到再问「程序搜索」（本地小索引 → Everything）；
/// - `QQ` —— 没写后缀也行，同样问「程序搜索」（它自己会试 `.exe` 等后缀）；
/// - `"C:\Program Files\App\app.exe" --flag` —— 引号里那一整段；
/// - `C:\Apps\app.exe` —— 第一个空格前的那个 token；
/// - `{path: QQ.exe}` —— 占位符里的那个名字（执行时也是它）。
///
/// 拿不到就返回 `None`（比如 `{selection}` 开头、或者文件根本不存在）。
pub fn program_of(value: &str) -> Option<String> {
    // 菜单每次重绘、设置页每一帧都会对同一批动作重复问一遍。`resolve_in_path`
    // 要遍历整个 PATH（几十个目录 × 四个后缀），一千多项就是上万次 stat ——
    // 这里按原始字符串缓存结果（命中 / 没命中都缓存），60 秒后重算，
    // 免得刚装上的程序一直显示不出图标。
    const TTL: Duration = Duration::from_secs(60);
    thread_local! {
        static PROGRAM_CACHE: RefCell<HashMap<String, (Option<String>, Instant)>> =
            RefCell::new(HashMap::new());
    }
    let hit = PROGRAM_CACHE.with(|cache| {
        cache
            .borrow()
            .get(value)
            .filter(|(_, at)| at.elapsed() < TTL)
            .map(|(path, _)| path.clone())
    });
    if let Some(path) = hit {
        return path;
    }
    let found = program_of_uncached(value);
    PROGRAM_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        // 菜单项再多也就几千条，超过就整体清一次（比 LRU 省事，也不会一直涨）
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(value.to_string(), (found.clone(), Instant::now()));
    });
    found
}

fn program_of_uncached(value: &str) -> Option<String> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }
    // `{path: QQ.exe}`：程序名藏在这个占位符里
    let text = if let Some(inner) = text.strip_prefix("{path:") {
        inner.split('}').next().unwrap_or(inner).trim()
    } else if text.starts_with('{') {
        // 别的占位符开头（`{selection}` / `{clipboard}`）：没有固定的程序名
        return None;
    } else {
        text
    };
    if text.is_empty() {
        return None;
    }
    let token = if let Some(rest) = text.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        text.split_whitespace().next()?
    };
    let token = token.trim();
    if token.len() < 2 {
        return None;
    }
    // 一眼就不是程序名的：RunAny 插件调用 `XiaoYao_plus[func](args)`、AHK 片段、
    // 带占位符 / 变量的参数（`%A_ScriptDir%`、`$clip`、`` `n ``）。
    //
    // 这一条是给导入的菜单兜底的：一千多项里九百多项都是插件函数，全都不带路径。
    // 以前它们会一路走到「只写了个名字 → 问程序搜索 / Everything」，**每一项一次
    // 全盘查询**，菜单呼出要等十几秒，设置页每帧重算更是直接卡死。
    if token
        .chars()
        .any(|ch| "[](){}%$`'!<>|;,*?".contains(ch))
    {
        return None;
    }
    let path = Path::new(token);
    // 有后缀就必须是「能抠图标」的那种：`a.txt`、`https://…` 都不算程序
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if !SHELL_EXTENSIONS.contains(&ext.as_str()) {
            return None;
        }
    }
    if path.is_file() {
        return Some(path.to_string_lossy().into_owned());
    }
    // 写的是一整条路径却不存在：没别的可解析
    if token.contains(['\\', '/', ':']) {
        return None;
    }
    // 只写了个名字（`notepad.exe` / `QQ`）：先按 PATH / System32 找，
    // 再交给「程序搜索」—— 装在 Program Files 里的那些（QQ.exe、WeChat.exe…）
    // 只靠 PATH 是找不到的，而图标位就在等这个路径。
    resolve_in_path(token).or_else(|| resolve_by_name(token))
}

/// 只写了文件名时，问「程序搜索」（本地小索引 → Everything）要完整路径。
///
/// 结果缓存 60 秒：图标位每次重绘都会问一遍，真去查 Everything 就太亏了。
/// 查不到也缓存 —— 不然每帧都去全盘问一次「有没有这个程序」。
fn resolve_by_name(name: &str) -> Option<String> {
    const TTL: Duration = Duration::from_secs(60);
    let key = name.to_ascii_lowercase();
    let hit = NAME_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache
            .get(&key)
            .filter(|(_, at)| at.elapsed() < TTL)
            .map(|(path, _)| path.clone())
    });
    if let Some(path) = hit {
        return path;
    }
    // 只收「真能抠出图标」的那种文件：`code`（VS Code 那个 `bin\code` shell 脚本）
    // 这类没有后缀的东西也会被索引命中，但它没有图标可抠 —— 收下来只会让图标位
    // 退化成「把一整条路径当文字画」。所以先按原顺序找「第一份能用的」：
    // 用户钉死的映射优先，其次才是索引里的候选。
    let found = crate::progindex::resolve(name)
        .filter(|path| icon_source(path))
        .or_else(|| {
            crate::progindex::candidates(name)
                .into_iter()
                .map(|candidate| candidate.path)
                .find(|path| icon_source(path))
        });
    NAME_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .insert(key, (found.clone(), Instant::now()))
    });
    found
}

/// 这个路径能不能当图标来源：文件真的在，后缀是 `exe` / `lnk` / `bat` 那几种。
fn icon_source(path: &str) -> bool {
    is_shell_icon(path) && Path::new(path).is_file()
}

/// 在 `PATH`（外加 System32 / windir）里找一个可执行文件。
///
/// 只写名字（`notepad`）时也补一遍常见后缀 —— `PATH` 里能命中的不只是 `.exe`，
/// 还有 VS Code 的 `bin\code` 这种没有后缀的 shell 脚本，那种不能当图标来源。
fn resolve_in_path(name: &str) -> Option<String> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    for key in ["SystemRoot", "windir"] {
        if let Some(root) = std::env::var_os(key) {
            let root = std::path::PathBuf::from(root);
            dirs.push(root.join("System32"));
            dirs.push(root);
        }
    }
    let mut files: Vec<String> = Vec::new();
    if Path::new(name).extension().is_some() {
        files.push(name.to_string());
    }
    for ext in ["exe", "com", "bat", "cmd"] {
        files.push(format!("{name}.{ext}"));
    }
    for dir in dirs {
        for file in &files {
            let candidate = dir.join(file);
            if candidate.is_file() && icon_source(&candidate.to_string_lossy()) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

fn extension(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // 太短的不用看（"a.b" 这种），带换行的也不是路径
    if trimmed.len() < 5 || trimmed.contains('\n') {
        return None;
    }
    let ext = Path::new(trimmed)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    let known = IMAGE_EXTENSIONS.contains(&ext.as_str()) || SHELL_EXTENSIONS.contains(&ext.as_str());
    known.then_some(ext)
}

/// 位图 / 矢量图文件（`image` crate 能读、或者 gpui 的 svg 渲染器能画）。
fn is_raster(text: &str) -> bool {
    extension(text)
        .map(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str()))
        .unwrap_or(false)
}

fn is_svg(text: &str) -> bool {
    extension(text).as_deref() == Some("svg")
}

/// 程序所在目录（`Icon/` 之类跟着它走）。
pub fn app_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.to_path_buf())
}

/// 图标路径的解析：**相对路径按 exe 同目录算**。
///
/// 存进菜单配置里的路径要是相对的（`Icon\badges\xxx.png`），整个程序目录拷到
/// 另一台机器上图标才不会断。绝对的照旧、纯字形（`★`）原样返回。
pub fn resolve(raw: &str) -> String {
    let text = raw.trim();
    if text.is_empty() || !(text.contains('\\') || text.contains('/')) {
        return text.to_string();
    }
    let path = Path::new(text);
    if path.is_absolute() {
        return text.to_string();
    }
    let Some(dir) = app_dir() else {
        return text.to_string();
    };
    let joined = dir.join(path);
    let text = joined.to_string_lossy().into_owned();
    // 相对路径统一用 `/` 存（换机器也认），拼出来之后换回系统分隔符
    if cfg!(windows) {
        text.replace('/', "\\")
    } else {
        text
    }
}

/// 反方向：绝对路径落在程序目录里面的，改成相对路径。
///
/// 老配置里存的是 `C://...//RunAny_RS//Icon//x.png` 这种；用户把整个目录拷到
/// 另一台电脑，盘符/用户名一变就全断了。读盘时顺手改写成相对路径。
pub fn make_relative(path: &str) -> Option<String> {
    let text = path.trim();
    let file = Path::new(text);
    if text.is_empty() || !file.is_absolute() {
        return None;
    }
    let dir = app_dir()?;
    let rest = file.strip_prefix(&dir).ok()?;
    let rest: Vec<String> = rest
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!rest.is_empty()).then(|| rest.join("/"))
}

/// 把用户挑的 / 拖进来的图标文件复制进 `exe 同目录/Icon/badges/`，
/// 返回**相对路径**（写进菜单配置里的就是这个）。
///
/// 复制不过去（只读介质、没权限、源文件已经没了）就返回 `None`，
/// 调用方退回原来的绝对路径 —— 至少这一台机器上还能用。
pub fn import_badge(source: &str) -> Option<String> {
    let source = resolve(source);
    let from = Path::new(&source);
    if !from.is_file() {
        return None;
    }
    let ext = from
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let dir = app_dir()?.join("Icon").join("badges");
    std::fs::create_dir_all(&dir).ok()?;
    // 文件名 = 原名 + 内容指纹：同名不同图的两个图标不会互相覆盖，
    // 同一张图重复导入也只是多一份同样的文件（内容一样，无所谓）
    let stem: String = from
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("icon")
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '_' })
        .take(40)
        .collect();
    let meta = std::fs::metadata(from).ok()?;
    let stamp = meta.len()
        ^ meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs())
            .unwrap_or(0);
    let name = format!("{stem}-{stamp:08x}.{ext}");
    let to = dir.join(&name);
    if !to.is_file() {
        std::fs::copy(from, &to).ok()?;
    }
    // 相对路径统一用正斜杠：跨机器、跨盘符都不会被误读
    Some(format!("Icon/badges/{name}"))
}

/// 画一个图标位。`size` 是方框边长，`color` 给纯文字和 svg 用。
pub fn render(text: &SharedString, size: f32, color: impl Into<Hsla>) -> AnyElement {
    let color = color.into();
    let raw = text.to_string();
    // 相对路径（`Icon/badges/..`）按 exe 目录解析后才能读盘 / 交给 svg 渲染器
    let raw = if is_image(&raw) { resolve(&raw) } else { raw };
    if is_svg(&raw) {
        return div()
            .size(px(size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(svg().path(raw).w(px(size)).h(px(size)).text_color(color))
            .into_any_element();
    }
    let bitmap = if is_shell_icon(&raw) {
        shell_icon(&raw)
    } else if is_raster(&raw) {
        raster(&raw)
    } else {
        None
    };
    if let Some(image) = bitmap {
        return div()
            .size(px(size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(
                img(image)
                    .w(px(size))
                    .h(px(size))
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element();
    }
    if is_image(&raw) {
        // 读不出来就当文字画个问号，至少能看出这里配了个图标
        return text_fallback("?", size, color);
    }
    // 一条路径但没能当成图标来源（老配置里存进来的 `…\bin\code` 这种）：
    // 画问号，别把一整条路径当文字塞进图标位
    if raw.contains('\\') || raw.contains(":/") {
        return text_fallback("?", size, color);
    }
    text_fallback(&raw, size, color)
}

fn text_fallback(text: &str, size: f32, color: Hsla) -> AnyElement {
    div()
        .text_size(px(size * 0.58))
        .text_color(color)
        .child(text.to_string())
        .into_any_element()
}

// ---------------------------------------------------------------------------
// 位图缓存
// ---------------------------------------------------------------------------

struct Cached {
    modified: Option<SystemTime>,
    image: Option<Arc<RenderImage>>,
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Cached>> = RefCell::new(HashMap::new());
    /// 「只写了文件名」→ 完整路径（键是小写名字，值是路径 + 查询时刻）
    static NAME_CACHE: RefCell<HashMap<String, (Option<String>, Instant)>> =
        RefCell::new(HashMap::new());
}

/// 读一张位图（带缓存）。读不到 / 解不出来返回 `None`。
fn raster(path: &str) -> Option<Arc<RenderImage>> {
    let file = PathBuf::from(path);
    let modified = std::fs::metadata(&file)
        .ok()
        .and_then(|meta| meta.modified().ok());

    if let Some(hit) = CACHE.with(|cache| {
        let cache = cache.borrow();
        cache
            .get(path)
            .and_then(|entry| (entry.modified == modified).then(|| entry.image.clone()))
    }) {
        return hit;
    }

    let image = std::fs::read(&file)
        .ok()
        .and_then(|bytes| image::load_from_memory(&bytes).ok())
        // gpui 的图片管线吃的是 BGRA（看 `elements/img.rs` 里解码完就 swap(0, 2)），
        // `image` crate 解出来是 RGBA，这里自己换一次，不然红色图标会变成蓝色
        .map(|decoded| {
            let mut buffer = decoded.to_rgba8();
            for pixel in buffer.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
        });

    CACHE.with(|cache| {
        cache.borrow_mut().insert(
            path.to_string(),
            Cached {
                modified,
                image: image.clone(),
            },
        );
    });
    image
}

/// 从 `.exe` / `.lnk` / `.ico` 里把图标抠出来（带缓存）。
///
/// 走的是 `win32::icon_pixels`（`ExtractIconExW` + `GetDIBits`，见那边的注释），
/// 非 Windows 上直接返回 `None`，图标位退化成「?」。
fn shell_icon(path: &str) -> Option<Arc<RenderImage>> {
    let modified = std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok());

    if let Some(hit) = CACHE.with(|cache| {
        let cache = cache.borrow();
        cache
            .get(path)
            .and_then(|entry| (entry.modified == modified).then(|| entry.image.clone()))
    }) {
        return hit;
    }

    let image = crate::win32::icon_pixels(path, 32).and_then(|(width, height, mut rgba)| {
        let buffer = image::RgbaImage::from_raw(width, height, {
            // 和 `raster()` 一样：gpui 的图片管线吃 BGRA
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            rgba
        })?;
        Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
    });

    CACHE.with(|cache| {
        cache.borrow_mut().insert(
            path.to_string(),
            Cached {
                modified,
                image: image.clone(),
            },
        );
    });
    image
}

#[cfg(test)]
mod tests {
    use super::{extension, is_image, is_svg, program_of};

    #[test]
    fn plain_text_is_not_an_image() {
        for text in ["+", "★", "一点", "🔍", "", "Ctrl+N", "a.b"] {
            assert!(!is_image(text), "{text:?} 不该被当成图片");
        }
    }

    #[test]
    fn image_paths_are_recognised() {
        assert!(is_image(r"C:\icons\save.svg"));
        assert!(is_image("icons/save.ICO"));
        assert!(is_image("/tmp/a.PNG"));
        assert!(is_image(r"C:\my icons\a.svg"), "带空格的路径也要认");
        assert!(is_svg(r"C:\icons\save.svg"));
        assert!(!is_svg(r"C:\icons\save.ico"));
    }

    #[test]
    fn extension_is_lowercased() {
        assert_eq!(extension("a.SVG").as_deref(), Some("svg"));
        assert_eq!(extension("a.txt"), None);
    }

    #[test]
    fn program_paths_are_recognised() {
        // 只认「运行程序」那种写法，且文件得真的在
        let exe = std::env::current_exe().expect("测试进程自己总有 exe");
        let exe = exe.to_string_lossy().into_owned();
        assert_eq!(program_of(&exe).as_deref(), Some(exe.as_str()));
        // 带参数、带引号
        assert_eq!(
            program_of(&format!("\"{exe}\" --flag")).as_deref(),
            Some(exe.as_str())
        );
        assert_eq!(
            program_of(&format!("{exe} .")).as_deref(),
            Some(exe.as_str())
        );
        // 不是程序的都不认
        for value in ["", "{selection}", "a.txt", "https://gpui.rs"] {
            assert_eq!(program_of(value), None, "{value:?} 不该被当成程序");
        }
        // 只写名字的（`notepad.exe`）会到 PATH / System32 里找
        #[cfg(windows)]
        assert!(
            program_of("notepad.exe").is_some(),
            "notepad.exe 应该能在系统目录里找到"
        );
    }

    #[test]
    fn a_path_placeholder_is_an_icon_source() {
        let exe = std::env::current_exe()
            .expect("测试进程自己总有 exe")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            program_of(&format!("{{path: {exe}}}")).as_deref(),
            Some(exe.as_str())
        );
    }

    #[test]
    fn an_exe_counts_as_an_icon_source() {
        assert!(is_image(r"C:\Windows\notepad.exe"));
        assert!(is_image(r"C:\Apps\a.lnk"));
    }
}

#[cfg(test)]
mod perf_tests {
    use super::*;

    /// 导入的菜单里九百多项是 `XiaoYao_plus[...]` 这种插件调用。
    /// 它们以前每个都会走到「问程序搜索 / Everything」，一千多项就是上千次全盘查询 ——
    /// 菜单呼出要等十几秒。现在这类一眼不是程序的 token 直接返回 `None`。
    ///
    /// `cargo test plugin_calls_do_not_trigger_a_program_search -- --nocapture`
    #[test]
    fn plugin_calls_do_not_trigger_a_program_search() {
        let cases = [
            "XiaoYao_plus[RA_plus](%getZz%,,22)",
            "huiZz_Window[win_size_zz](320,180)",
            "runany[DynaRun](FileRecycleEmpty)",
            "XiaoYao_plus[xy_RegExReplace](%getZz%,^(?!\\/\\*)[\\s\\S]*)",
            "%ComSpec% /s /k pushd \"%getZz%\"",
        ];
        let start = Instant::now();
        for value in cases {
            assert_eq!(program_of(value), None, "{value} 不该被当成程序");
        }
        let spent = start.elapsed();
        println!("{} 条插件调用判定耗时 {:?}", cases.len(), spent);
        // 真的去查程序搜索的话，每一条都要几十毫秒
        assert!(spent.as_millis() < 50, "判定太慢：{spent:?}");
    }

    /// 同一批动作反复问（菜单每帧重绘、设置页每行都算一次图标）要走缓存。
    #[test]
    fn repeated_lookups_are_cached() {
        let value = "XiaoYao_plus[RA_plus](%getZz%,,22)";
        let first = program_of(value);
        let start = Instant::now();
        for _ in 0..2000 {
            assert_eq!(program_of(value), first);
        }
        let spent = start.elapsed();
        println!("2000 次重复查询 {:?}", spent);
        assert!(spent.as_millis() < 100, "缓存没生效：{spent:?}");
    }

    /// 图标配的是相对路径：跟着 exe 目录走，整个文件夹拷到别的电脑也能用。
    #[test]
    fn a_relative_badge_path_resolves_next_to_the_exe() {
        let dir = app_dir().expect("测试进程总有 exe 目录");
        assert_eq!(
            resolve("Icon/badges/save.png"),
            dir.join("Icon").join("badges").join("save.png").to_string_lossy()
        );
        // 绝对的、纯字形的、裸文件名的都不动
        assert_eq!(resolve("C:////icons////save.png"), "C:////icons////save.png");
        assert_eq!(resolve("★"), "★");
        assert_eq!(resolve("notepad.exe"), "notepad.exe");
    }

    /// 选文件 / 拖图片进来：复制到 `Icon/badges/`，配置里存相对路径。
    #[test]
    fn importing_a_badge_copies_it_and_stores_a_relative_path() {
        let source = std::env::temp_dir().join("runany_badge_import_test.png");
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([200, 30, 40, 255]));
        image.save(&source).expect("造一张测试图");
        let stored = import_badge(&source.to_string_lossy()).expect("应该能收进 Icon/badges");
        assert!(stored.starts_with("Icon/badges/"), "{stored}");
        assert!(!stored.contains(':'), "不能存绝对路径：{stored}");
        let absolute = resolve(&stored);
        assert!(Path::new(&absolute).is_file(), "复制的文件应该在：{absolute}");
        // 收的都是图片，exe 之类的不会被当成图标收进来
        assert_eq!(import_badge("notepad.exe"), None);
        let _ = std::fs::remove_file(&absolute);
        let _ = std::fs::remove_file(&source);
    }

    /// 程序目录里的绝对图标路径会被改写成相对路径（搬电脑不断）。
    #[test]
    fn an_absolute_icon_inside_the_app_dir_becomes_relative() {
        let dir = app_dir().expect("测试进程总有 exe 目录");
        let absolute = dir.join("Icon").join("badges").join("x.png");
        assert_eq!(
            make_relative(&absolute.to_string_lossy()).as_deref(),
            Some("Icon/badges/x.png")
        );
        assert_eq!(make_relative("C:////other////x.png"), None);
        assert_eq!(make_relative("Icon/badges/x.png"), None);
    }
}
