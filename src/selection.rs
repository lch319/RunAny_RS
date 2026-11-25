//! 弹出菜单前，把用户「当前选中的内容」抓过来，并且认出它是**文字 / 文件 / 图片**。
//!
//! ## 怎么抓
//!
//! 没有通用的 Windows API 能直接读任意程序的选区，唯一可行的路子是模拟一次复制：
//!
//! 1. 先等热键的修饰键松开（Ctrl+Alt+M 按下的瞬间 Alt 还压着，
//!    这时候注入 Ctrl+C 会变成 Ctrl+Alt+C，目标程序不认）。用户按住热键不松手时
//!    不能干等 —— 超时就把还按着的 Ctrl / Alt / Shift 用注入的 keyup 顶开
//!    （Win 键例外，顶开它会弹出开始菜单，只能多等一会儿）；
//! 2. 备份整份剪贴板（每种格式的原始字节，含文本 / HTML / RTF / 图片）；
//! 3. 给原来的前台窗口发一次复制键（默认 `Ctrl+C`），轮询剪贴板序号等它变；
//! 4. 读走内容，**再把剪贴板按备份原样写回去** —— 不污染用户的剪贴板；
//! 5. 抓到的东西留在本模块里：菜单第一项显示它、点一下复制它，
//!    动作参数里的 `{selection}` 也取它。
//!
//! ## 怎么认出类型
//!
//! 复制出来的东西是什么，看剪贴板里有哪些格式就知道（`detect()`）：
//!
//! | 剪贴板里有什么 | 判定 |
//! | --- | --- |
//! | `CF_HDROP`（文件列表） | **文件 / 文件夹**；再看后缀加一层细分 |
//! | `CF_DIB` / `CF_DIBV5` / `CF_BITMAP` / 注册格式 `PNG` | **图片** |
//! | `CF_UNICODETEXT` / `CF_TEXT` | **文字** |
//!
//! 顺序就是优先级：在资源管理器里复制一张 `.png` 时剪贴板里也有文本（路径），
//! 但我们要的是「那个文件」而不是那段路径。
//!
//! 类型只有四种：文字 / 文件 / 文件夹 / 图片（位图）。
//! 文件**不再按后缀细分成视频·音频·文档…**，「这个菜单只对 mp4、mkv 出现」
//! 由菜单项自己的 `file_exts` 说了算（留空 = 所有文件），
//! 多选时怎么算命中由 `MultiFileRule` 决定。
//!
//! 抓不到（目标程序没有选中内容、剪贴板没变）就当作「没选中」，
//! 此时菜单不做任何过滤 —— 和以前一样全显示。
//!
//! ## 注意
//!
//! 终端里 Ctrl+C 是「中断当前进程」。在意这一点的用户可以在
//! 「设置 → 行为」里把抓取按键改成 `Ctrl+Insert`（大多数 Windows 程序也认）。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::menu::{MenuEntry, MenuItem};
use crate::settings::{self, ActionKind, ActionStep, BuiltinAction};
use crate::win32;
use crate::win32::ClipboardBackup;

/// 轮询剪贴板的间隔（等它变化 / 等它写完都按这个节拍）
///
/// 4ms 看着激进，但实际就是 `GetClipboardSequenceNumber`（读一个内核计数器），
/// 一次几十纳秒。原来是 15ms —— 光轮询粒度就给每次弹出多加了十几毫秒。
const POLL_INTERVAL: Duration = Duration::from_millis(4);
/// 检测到剪贴板变化之后，还要再等它「连续这么久序号不再变」才算写完。
///
/// 复制文件时 Explorer 是**分几次**往剪贴板写格式的（文本路径先到、文件列表后到），
/// 序号每写一次都会 +1。要是一看到序号变了就立刻快照，很可能只截到「文本」、
/// 漏掉「文件列表」，把选中的文件误判成文字。等它稳定一小段再快照就稳了。
///
/// 原来 80ms —— 太保守了：Explorer 两次写之间只隔一两毫秒，30ms 足够，
/// 而这一段是**每次弹出都要付**的固定成本。
const STABLE_MS: Duration = Duration::from_millis(30);
/// 菜单第一项里最多显示多少个字符
const PREVIEW_CHARS: usize = 46;

/// 选中内容的类型。菜单项可以声明「我只在选中 X 的时候出现」。
///
/// 只有四种，够用且好理解：文字 / 文件 / 文件夹 / 图片（剪贴板里直接是位图那种，
/// 不是图片文件）。**文件后缀不再细分类型** —— 「这个菜单只对 mp4、mkv 出现」
/// 这种需求由每个菜单项自己的 `file_exts` 表达（留空 = 所有文件都算），
/// 见 `settings::ItemSetting::file_exts`。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionKind {
    Text,
    File,
    Folder,
    /// 剪贴板里是位图（截图、或者从网页上复制一张图），没有对应文件路径
    Image,
}

impl SelectionKind {
    pub const ALL: [SelectionKind; 4] = [
        SelectionKind::Text,
        SelectionKind::File,
        SelectionKind::Folder,
        SelectionKind::Image,
    ];

    /// 存进 JSON 的名字（`settings.json` 里看到的就是它）。
    pub fn key(self) -> &'static str {
        match self {
            SelectionKind::Text => "text",
            SelectionKind::File => "file",
            SelectionKind::Folder => "folder",
            SelectionKind::Image => "image",
        }
    }

    /// 界面上的中文名。
    pub fn label(self) -> &'static str {
        match self {
            SelectionKind::Text => crate::i18n::t("文字"),
            SelectionKind::File => crate::i18n::t("文件"),
            SelectionKind::Folder => crate::i18n::t("文件夹"),
            SelectionKind::Image => crate::i18n::t("图片"),
        }
    }

    /// 老配置里的 `image` / `video` / `audio` / `document` / `code` / `archive`
    /// 都算「文件」（那时候它们是文件的后缀细分）。认不出来就是 `None`。
    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "text" => Some(SelectionKind::Text),
            "file" => Some(SelectionKind::File),
            "folder" => Some(SelectionKind::Folder),
            "image" => Some(SelectionKind::Image),
            "video" | "audio" | "document" | "code" | "archive" => Some(SelectionKind::File),
            _ => None,
        }
    }
}

/// 多选文件时，菜单项要求的那串后缀怎么算命中。
///
/// 单选时两种规则结果一样，区别只在多选：一 mp4 一 png 这种混选，
/// 默认（`AllSame`）要求**每个**文件都命中，不会因为顺序不同闪出不相干的菜单。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MultiFileRule {
    /// 每个文件的后缀都得在列表里才算命中
    #[default]
    AllSame,
    /// 只看第一个文件的后缀
    First,
}

impl MultiFileRule {
    pub fn label(self) -> &'static str {
        match self {
            MultiFileRule::AllSame => crate::i18n::t("全部都得命中"),
            MultiFileRule::First => crate::i18n::t("看第一个文件"),
        }
    }
}

/// 「把选中的文件加到菜单里」时，路径写成什么样。
///
/// 以前这里每次都弹一问「完整路径还是裸文件名」，一天点十次就烦了；
/// 现在可以一次定死，`Ask` 保持老行为。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AddSelectionPath {
    /// 每次都问（默认）
    #[default]
    Ask,
    /// 直接写完整路径
    Full,
    /// 只写文件名（交给 PATH 去找）
    Bare,
}

impl AddSelectionPath {
    pub const ALL: [AddSelectionPath; 3] =
        [AddSelectionPath::Ask, AddSelectionPath::Full, AddSelectionPath::Bare];

    pub fn label(self) -> &'static str {
        match self {
            AddSelectionPath::Ask => crate::i18n::t("每次都问"),
            AddSelectionPath::Full => crate::i18n::t("总是完整路径"),
            AddSelectionPath::Bare => crate::i18n::t("总是裸文件名"),
        }
    }

    pub fn index(self) -> u32 {
        match self {
            AddSelectionPath::Ask => 0,
            AddSelectionPath::Full => 1,
            AddSelectionPath::Bare => 2,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            1 => AddSelectionPath::Full,
            2 => AddSelectionPath::Bare,
            _ => AddSelectionPath::Ask,
        }
    }
}


// ---------------------------------------------------------------------------
// 后缀：只做「认后缀 / 比后缀」，不再把文件分成视频·音频·文档…那些类型
// ---------------------------------------------------------------------------

/// 把用户写的 `mp4|mkv, .AVI  mov` 解析成干净的后缀列表。
///
/// 分隔符认 `|` / `,` / `，` / `;` / `；` / 空白，大小写不敏感，前导的 `.` 会去掉，
/// 重复的只留一个。这样「`mp4|mkv`」「`.mp4, .mkv`」「mp4 mkv」三种写法都能用。
pub fn parse_exts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in text.split(['|', ',', '，', ';', '；', ' ', '\t', '\n', '\r']) {
        let ext = piece.trim().trim_start_matches('.').to_ascii_lowercase();
        if ext.is_empty() || out.iter().any(|seen| *seen == ext) {
            continue;
        }
        out.push(ext);
    }
    out
}

/// 一个路径的后缀（小写、不带点）。没有后缀就是 `None`。
pub fn ext_of(path: &str) -> Option<String> {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// 这几个文件的后缀（只算文件，文件夹没有后缀）。
#[cfg(test)]
pub fn exts_of(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !std::path::Path::new(path.as_str()).is_dir())
        .filter_map(|path| ext_of(path))
        .collect()
}

/// 这批文件满不满足「后缀必须在 `list` 里」。
///
/// `MultiFileRule::AllSame` 要求**每个**文件都命中（`1.mp4 + 1.png` 不算 mp4 菜单）；
/// `First` 只看第一个。列表为空 = 不限制，永远算命中（调用方一般会先挡掉这种情况）。
pub fn exts_hit(paths: &[String], list: &[String], rule: MultiFileRule) -> bool {
    if list.is_empty() {
        return true;
    }
    let each: Vec<Option<String>> = paths
        .iter()
        .filter(|path| !std::path::Path::new(path.as_str()).is_dir())
        .map(|path| ext_of(path))
        .collect();
    if each.is_empty() {
        return false; // 只有文件夹：没有后缀可比
    }
    match rule {
        MultiFileRule::First => each
            .first()
            .and_then(|ext| ext.as_ref())
            .is_some_and(|ext| list.contains(ext)),
        MultiFileRule::AllSame => each
            .iter()
            .all(|ext| ext.as_ref().is_some_and(|ext| list.contains(ext))),
    }
}

/// 一批路径 → 类型集合（文件夹不算文件；纯文件夹只算 `Folder`）。
///
/// `paths` 为空（有 `CF_HDROP` 但路径没解析出来）时按「文件」算 ——
/// 类型判定不该依赖能不能解析出路径。
pub fn kinds_of(paths: &[String]) -> Vec<SelectionKind> {
    if paths.is_empty() {
        return vec![SelectionKind::File];
    }
    let mut files = 0usize;
    let mut folders = 0usize;
    for path in paths {
        if std::path::Path::new(path.as_str()).is_dir() {
            folders += 1;
        } else {
            files += 1;
        }
    }
    // 混选时也算「文件」：列表里确实有文件，用户多半是冲着文件来的
    let mut kinds = Vec::new();
    if files > 0 || folders == 0 {
        kinds.push(SelectionKind::File);
    }
    if folders > 0 {
        kinds.push(SelectionKind::Folder);
    }
    kinds
}

/// 「这次选中的是什么」——筛选菜单时唯一需要的信息。
///
/// 以前这里是 `Vec<SelectionKind>`，加了「每个菜单项自己写后缀」之后还得带上文件路径，
/// 所以收成一个结构体，省得每个函数都多两个参数。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub kinds: Vec<SelectionKind>,
    /// 选中的文件路径（判断后缀用）
    pub files: Vec<String>,
}

impl Context {
    pub fn new(kinds: Vec<SelectionKind>, files: Vec<String>) -> Self {
        Self { kinds, files }
    }

    /// 从一次抓取结果建（`None` = 没抓到，调用方一般直接显示完整菜单）。
    pub fn of(selection: Option<&Selection>) -> Option<Self> {
        selection.map(|selection| Self::new(selection.kinds.clone(), selection.files.clone()))
    }

    pub fn has(&self, kind: SelectionKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// 这几个文件后缀满不满足条件（空列表 = 不限）。
    pub fn exts_hit(&self, list: &[String], rule: MultiFileRule) -> bool {
        exts_hit(&self.files, list, rule)
    }
}

impl serde::Serialize for SelectionKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.key())
    }
}

/// 这一次抓到的选中内容。
#[derive(Debug, Default)]
pub struct Selection {
    /// 它是哪几种类型（图片文件同时是文件和图片）
    pub kinds: Vec<SelectionKind>,
    /// 文字内容的原文；文件选择时是「路径逐个换行」
    pub text: String,
    /// `CF_HDROP` 里的文件路径
    pub files: Vec<String>,
    /// 图片宽高（读不到就是 None）
    pub image: Option<(u32, u32)>,
    /// 抓取成功那一刻的剪贴板快照 —— 点第一项时原样放回去，就等于「再复制一次」
    pub snapshot: Option<ClipboardBackup>,
}

impl Selection {
    pub fn has(&self, kind: SelectionKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// 「文字 / 文件 / 图片 / 图片文件」
    pub fn kind_label(&self) -> String {
        let labels: Vec<&str> = self.kinds.iter().map(|kind| kind.label()).collect();
        labels.join(" ")
    }

    /// 给动作参数用的那个字符串（`{selection}`）。
    ///
    /// - 文字 → 原文
    /// - 文件 → 路径用空格连起来（带空格的路径自己加引号，直接丢给命令行也安全）
    /// - 纯图片（没有路径）→ 空串
    pub fn placeholder_text(&self) -> String {
        if self.has(SelectionKind::Text) {
            return self.text.clone();
        }
        if self.files.is_empty() {
            return String::new();
        }
        self.files
            .iter()
            .map(|path| {
                if path.contains(' ') {
                    format!("\"{path}\"")
                } else {
                    path.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 菜单第一项长什么样：(图标位, 文字)。
    pub fn headline(&self) -> (&'static str, String) {
        if self.has(SelectionKind::Text) {
            return ("✂", preview(&self.text));
        }
        if !self.files.is_empty() {
            let badge = if self.has(SelectionKind::Folder) && !self.has(SelectionKind::File) {
                "📁"
            } else {
                "📄"
            };
            let noun = if self.has(SelectionKind::Folder) && !self.has(SelectionKind::File) {
                "个文件夹"
            } else {
                "个文件"
            };
            let label = if self.files.len() == 1 {
                file_name(&self.files[0]).to_string()
            } else {
                format!(
                    "{} {noun} · {}",
                    self.files.len(),
                    file_name(&self.files[0])
                )
            };
            // 多选时把第一个名字截短，免得整行都是路径
            return (badge, truncated(&label, PREVIEW_CHARS));
        }
        // 剪贴板里确实有文件列表、但路径解析不出来（少见）：类型照样是「文件」
        if self.has(SelectionKind::File) {
            return ("📄", "文件".to_string());
        }
        if self.has(SelectionKind::Folder) {
            return ("📁", "文件夹".to_string());
        }
        match self.image {
            Some((w, h)) => ("🖼", format!("图片 {w}×{h}")),
            None => ("🖼", "图片".to_string()),
        }
    }
}

thread_local! {
    /// 这一次弹出时抓到的选中内容（下次弹出会重新抓/清空）
    static CURRENT: RefCell<Option<Rc<Selection>>> = const { RefCell::new(None) };
}

/// 当前抓到的选中内容（动作里的 `{selection}` 用它）。`Rc` 是为了不复制图片快照。
pub fn current() -> Option<Rc<Selection>> {
    CURRENT.with(|slot| slot.borrow().clone())
}

/// 当前选中内容的「筛选依据」；没选中就是 `None`。
pub fn current_context() -> Option<Context> {
    let selection = current();
    Context::of(selection.as_deref())
}

fn set(selection: Option<Rc<Selection>>) {
    CURRENT.with(|slot| *slot.borrow_mut() = selection);
}

/// 清空（关菜单 / 关掉抓取功能时调）。
pub fn clear() {
    set(None);
}

/// 单行预览：换行、制表符压成空格，太长就省略。
fn preview(text: &str) -> String {
    let mut flat = String::new();
    let mut last_space = false;
    for ch in text.chars() {
        let ch = if ch.is_whitespace() { ' ' } else { ch };
        if ch == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        flat.push(ch);
        if flat.chars().count() > PREVIEW_CHARS {
            break;
        }
    }
    let flat = flat.trim().to_string();
    truncated(&flat, PREVIEW_CHARS)
}

fn truncated(text: &str, limit: usize) -> String {
    if text.chars().count() > limit {
        let cut: String = text.chars().take(limit).collect();
        format!("{}…", cut.trim_end())
    } else {
        text.to_string()
    }
}

/// 路径的最后一段（`C:\a\b\报告.pdf` → `报告.pdf`）。
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// 菜单里的第一项：显示抓到的内容，点一下把它复制回剪贴板。
pub fn menu_entry() -> Option<MenuEntry> {
    let selection = current()?;
    let (badge, label) = selection.headline();
    if label.is_empty() {
        return None;
    }
    let mut item = MenuItem::new(SELECTION_ID, label).badge(badge);
    item.shortcut = Some("复制".into());
    item.actions = vec![ActionStep::new(
        ActionKind::Builtin,
        BuiltinAction::CopySelection.key(),
    )];
    Some(MenuEntry::Item(item))
}

/// 选中内容里的第一个文件 / 文件夹路径（内置动作「打开路径」留空时用它）。
pub fn first_path() -> Option<String> {
    current()?.files.first().cloned()
}

/// 把抓到的内容再放回剪贴板（点第一项时调）。
///
/// 用抓取那一刻的快照，所以文件、图片、富文本都是**原样**回去的；
/// 没有快照（调试种子）才退回写纯文本。
pub fn copy_back() {
    let Some(selection) = current() else {
        return;
    };
    if let Some(snapshot) = &selection.snapshot {
        win32::restore_clipboard(snapshot);
        return;
    }
    let text = selection.placeholder_text();
    if !text.is_empty() {
        win32::write_clipboard_text(&text);
    }
}

/// 菜单第一项的稳定 id（业务侧可以据此判断「点的是不是它」）。
pub const SELECTION_ID: &str = "__selection__";

/// 「接在命令末尾」的那段参数（菜单项勾了「接上选中内容」时用）。
///
/// - 选中的是文件 / 文件夹：逐个路径拼上，**带空格的自动加双引号**
///   （`D:\a b\1.txt` → `"D:\a b\1.txt"`），这样直接交给 `cmd` 也不会被拆成两个参数；
/// - 选中的是文字：原样接上（换行压成空格，免得一条命令变成两条）。
pub fn append_arg() -> Option<String> {
    let selection = current()?;
    if !selection.files.is_empty() {
        let args: Vec<String> = selection
            .files
            .iter()
            .map(|path| quote_path(path))
            .collect();
        return Some(args.join(" "));
    }
    let text = selection.text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.replace(['\r', '\n'], " "))
}

/// 路径带空格就加双引号（已经带了的不重复加）。
fn quote_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('"') || !path.contains(' ') {
        path.to_string()
    } else {
        format!("\"{path}\"")
    }
}

/// 剪贴板里「这一眼能看到的证据」。
///
/// 关键点是**优先从快照里看**：快照是在同一次 `OpenClipboard` 里读出来的，
/// 而剪贴板是全局独占的 —— Explorer 复制文件之后还会挂几个延迟渲染的格式，
/// 这时候再单独 Open 一次很容易失败。失败就意味着把「一批文件」误判成「没选中」，
/// 菜单也就不筛选了（就是「文件筛选没效果」那个 bug）。
struct Probe {
    has_files: bool,
    files: Vec<String>,
    has_image: bool,
    image: Option<(u32, u32)>,
    text: String,
}

impl Probe {
    fn from_backup(backup: &ClipboardBackup) -> Self {
        let png = win32::registered_format("PNG");
        let has_image = backup.has_format(win32::CF_DIB)
            || backup.has_format(win32::CF_DIBV5)
            || backup.has_format(win32::CF_BITMAP)
            || (png != 0 && backup.has_format(png));
        let image = backup
            .bytes(win32::CF_DIB)
            .and_then(win32::parse_dib_size)
            .or_else(|| {
                backup
                    .bytes(win32::CF_DIBV5)
                    .and_then(win32::parse_dib_size)
            })
            .or_else(|| {
                (png != 0)
                    .then(|| backup.bytes(png))
                    .flatten()
                    .and_then(win32::parse_png_size)
            });
        let text = backup
            .bytes(win32::CF_UNICODETEXT)
            .map(win32::parse_wide_text)
            .filter(|text| !text.is_empty())
            .or_else(|| backup.bytes(win32::CF_TEXT).map(win32::parse_ansi_text))
            .unwrap_or_default();
        Self {
            has_files: backup.has_format(win32::CF_HDROP),
            files: backup
                .bytes(win32::CF_HDROP)
                .map(win32::parse_dropfiles)
                .unwrap_or_default(),
            has_image,
            image,
            text,
        }
    }

    /// 没有快照时的退路：现读一遍剪贴板。
    fn live() -> Self {
        let has_files = win32::clipboard_has_files();
        let files = if has_files {
            win32::read_clipboard_files()
        } else {
            Vec::new()
        };
        let has_image = win32::clipboard_has_image();
        Self {
            has_files,
            files,
            has_image,
            image: if has_image {
                win32::clipboard_image_size()
            } else {
                None
            },
            text: win32::read_clipboard_text().unwrap_or_default(),
        }
    }
}

/// 看剪贴板里有哪些格式，判断复制出来的是什么。
///
/// 文件优先于图片、图片优先于文本 —— 理由见模块文档。
/// 注意 `CF_HDROP` 只要**存在**就算文件（哪怕路径解析不出来）：
/// 判定类型不该依赖能不能解析出路径。
fn detect(backup: Option<ClipboardBackup>) -> Option<Selection> {
    let mut probe = match &backup {
        Some(backup) => Probe::from_backup(backup),
        None => Probe::live(),
    };
    // CF_HDROP 常常是「延迟渲染」的：快照里看得到格式号，但那一刻还拿不到路径。
    // 类型不受影响（有格式就算文件），但 `{selection}` 需要有路径，所以补读一次。
    if probe.has_files && probe.files.is_empty() {
        probe.files = win32::read_clipboard_files();
    }
    if probe.has_files {
        let kinds = kinds_of(&probe.files);
        let text = probe.files.join("\r\n");
        return Some(Selection {
            kinds,
            text,
            files: probe.files,
            image: None,
            snapshot: backup,
        });
    }
    if probe.has_image {
        return Some(Selection {
            kinds: vec![SelectionKind::Image],
            text: String::new(),
            files: Vec::new(),
            image: probe.image,
            snapshot: backup,
        });
    }
    let text = probe.text.trim_end_matches(['\r', '\n']).to_string();
    if text.trim().is_empty() {
        return None;
    }
    Some(Selection {
        kinds: vec![SelectionKind::Text],
        text,
        files: Vec::new(),
        image: None,
        snapshot: backup,
    })
}

/// 抓取失败的原因只打一行日志：没抓到时 `{selection}` 会是空串，
/// 用户最好能在控制台（或日志）里看到「为什么空」。
fn note_failure(reason: &str) {
    eprintln!("[popup-menu] 没抓到选中内容：{reason}");
}

/// 抓一次选中内容；返回抓到的东西。
///
/// 整个过程在主线程上同步跑（最多几百毫秒），因为菜单得等它拿到东西才能摆出来。
thread_local! {
    /// 上一次抓取还在跑（正在发复制键 / 等剪贴板）。
    static CAPTURING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// 上一次「只抓一次」是什么时候（见 `capture_trying`）
    static LAST_QUICK: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

pub fn capture() -> Option<Rc<Selection>> {
    // 弹菜单用：最多试 3 次（焦点刚切过来、程序还在忙的时候，多发两次确实能抓到）
    capture_trying(3)
}

/// 只发**一次**复制键的抓取。
///
/// 给「用快捷键直接执行菜单项」这类场景用：那里要的只是命令后面那个路径，
/// 抓不到顶多少一个参数；而重试意味着往目标程序里连发 3 次 `Ctrl+C`
/// ——在终端里那就是三次中断，用户看到的「按一下发了三次 Ctrl+C」就是这么来的。
pub fn capture_quick() -> Option<Rc<Selection>> {
    capture_trying(1)
}

fn capture_trying(attempts: usize) -> Option<Rc<Selection>> {
    // 同一次触发里别重复发复制键：快捷键路径先抓了一次（没抓到），
    // 紧接着 `execute_step` 又要追加选中内容——不拦一下就会再发一次。
    // 800ms 内只认第一次的结果（抓到就沿用，没抓到就认了）。
    if attempts == 1 {
        let fresh = LAST_QUICK.with(|slot| {
            slot.get()
                .map(|at: std::time::Instant| at.elapsed() < Duration::from_millis(800))
                .unwrap_or(false)
        });
        if fresh {
            return current();
        }
        LAST_QUICK.with(|slot| slot.set(Some(std::time::Instant::now())));
    }
    // 连按快捷键时，上一次可能还卡在「等修饰键 → 发 Ctrl+C → 等剪贴板」里
    // （最长几百毫秒）。再进来一次会往同一个窗口重发复制、再等一轮，几次一叠
    // 就是「连续按会卡住」。这里直接不重入：沿用上一次抓到的内容。
    if CAPTURING.with(|flag| flag.get()) {
        return current();
    }
    CAPTURING.with(|flag| flag.set(true));
    struct CaptureGuard;
    impl Drop for CaptureGuard {
        fn drop(&mut self) {
            CAPTURING.with(|flag| flag.set(false));
        }
    }
    let _capture_guard = CaptureGuard;

    // 调试用：`GPUI_MENU_DEBUG_SELECTION=一段文字` 直接当抓到了这段
    // （这台机器上没法真去别的窗口里选中文字，看版式时用）。
    // 再加 `GPUI_MENU_DEBUG_SELECTION_KIND=file|folder|image` 可以伪装成文件 / 文件夹 / 图片
    // （老写法 `video` / `audio` / `document` … 现在都按「文件」算）。
    if let Ok(seeded) = std::env::var("GPUI_MENU_DEBUG_SELECTION") {
        if !seeded.is_empty() {
            let kind = std::env::var("GPUI_MENU_DEBUG_SELECTION_KIND").unwrap_or_default();
            let selection = match kind.trim() {
                "" => Selection {
                    kinds: vec![SelectionKind::Text],
                    text: seeded.clone(),
                    files: Vec::new(),
                    image: None,
                    snapshot: None,
                },
                _ => {
                    let files: Vec<String> = seeded.split(';').map(|s| s.to_string()).collect();
                    let mut kinds = kinds_of(&files);
                    // 调试时路径可能不存在（`is_dir()` 是 false），这里按类型名硬补一个，
                    // 不然想看的「文件夹菜单」永远出不来
                    if let Some(forced) = SelectionKind::from_key(kind.trim()) {
                        if forced == SelectionKind::Image {
                            return Some(Rc::new(Selection {
                                kinds: vec![SelectionKind::Image],
                                text: String::new(),
                                files: Vec::new(),
                                image: Some((1024, 768)),
                                snapshot: None,
                            }));
                        }
                        if !kinds.contains(&forced) {
                            kinds.push(forced);
                        }
                    }
                    Selection {
                        kinds,
                        text: files.join("\r\n"),
                        files,
                        image: None,
                        snapshot: None,
                    }
                }
            };
            let selection = Rc::new(selection);
            set(Some(selection.clone()));
            return Some(selection);
        }
    }
    let enabled = settings::read(|settings| settings.selection_enabled);
    if !enabled {
        clear();
        return None;
    }
    // 真正的前台窗口（判断「是不是我们自己的 / 桌面」必须用它）
    let foreground = win32::foreground_window();
    // 真正要发复制键的那个窗口：鼠标手势触发时，前台是手势工具的浮层，
    // 得换成光标底下那个真窗口（见 `win32::selection_target`）。
    let target = win32::selection_target();
    if std::env::var_os("GPUI_MENU_DEBUG_CAPTURE").is_some() && target != foreground {
        eprintln!(
            "[popup-menu] 前台是浮层（{:x}），改用光标底下的窗口 {:x} 收复制键",
            foreground, target
        );
    }
    // 桌面上不可能有选中内容：整条抓取链路（等修饰键 → 发复制键 → 等剪贴板）
    // 全是白等，直接返回，菜单立刻就能弹。
    if win32::is_desktop_window(foreground) {
        clear();
        return None;
    }
    // 根本没有前台窗口（刚开机、锁屏回来、当前会话没有可见窗口）：没有地方能「选中
    // 东西」，发复制键也没人接 —— 整条链路只会白等。直接放行，菜单立刻弹。
    if foreground == 0 || target == 0 {
        clear();
        return None;
    }
    if win32::window_process_id(foreground) == std::process::id() {
        // 前台是我们自己的窗口（设置页 / 编辑窗口 / 菜单）：**不抓**。
        //
        // 以前的做法是「把焦点还给下面那个窗口，再往那儿发 Ctrl+C」—— 复制是复制到了，
        // 但代价是把用户正在用的设置窗口丢到后面去：按一下快捷键，设置窗口就失去焦点、
        // 跑到别的程序底下（用户报的就是这个）。设置窗口里本来也没有「选中内容」可言，
        // 与其偷焦点去猜，不如老老实实不抓。
        eprintln!("[popup-menu] 前台是本程序自己的窗口，不抓选中内容（免把设置窗口顶到后面）");
        clear();
        return None;
    }

    let keys = settings::read(|settings| settings.selection_copy_keys.clone());
    let keys = if keys.trim().is_empty() {
        "Ctrl+C".to_string()
    } else {
        keys
    };

    // 备份剪贴板：抓完要原样放回去（只备份一次，重试之间复用）
    let t_cap = std::time::Instant::now();
    let mut backup = win32::snapshot_clipboard();
    crate::popup::timing!(t_cap, "  备份剪贴板");
    // 等待时长：按前台程序取（慢程序单独配长一点），没配过才用全局
    let process = win32::process_path_of_window(target)
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    let wait_ms = settings::read(|settings| settings.selection_wait_for(&process));
    let wait_ms = wait_ms.clamp(30, 8000);
    if std::env::var_os("GPUI_MENU_DEBUG_CAPTURE").is_some() {
        eprintln!("[selection] 前台进程 {process:?} 的等待预算 {wait_ms}ms");
    }

    // 最多试 3 次。为什么要重试：
    // - 快捷键刚按下时，目标窗口可能还没拿到前台 / 还没处理完按键抬起，
    //   第一发 Ctrl+C 会被它当成「组合键的一部分」吞掉；
    // - 目标程序忙的时候剪贴板要过一会儿才写。
    //
    // 但**抓取本身封顶 200ms**（等修饰键的时间不算在里面，见下面）：
    // 抓取是「菜单弹出来之前」唯一还能省的活，以前按 `selection_wait_ms`（默认 500）算，
    // 加上每轮还原剪贴板，桌面上没选中任何东西时要 1.1 秒菜单才出来 ——
    // 这才是「感觉变慢」的真凶。正常的复制几十毫秒就写完了，200ms 够用；
    // 真抓不到就快速放行。
    let attempts = attempts.max(1);
    // 以前这里还压了一个 `CAP_MS = 200` 的上限：用户把「复制后最多等待」
    // 填成 1000ms 也只会等 200ms —— 填了等于没填（用户报的就是这个）。
    // 现在直接按设置值走；默认 500ms，抓取最快的正常情况几十毫秒就返回了，
    // 只有真抓不到才会用满。
    let mut deadline = std::time::Instant::now() + Duration::from_millis(wait_ms);
    let mut selection = None;
    for attempt in 0..attempts {
        // 焦点可能已经跑掉了（比如第一次发键把目标窗口激活过去之后又弹了别的东西）
        if win32::foreground_window() != target {
            win32::set_foreground(target);
            if !win32::wait_foreground(target, 300) {
                note_failure("抓取前焦点不在目标窗口，也没能拉回来");
                continue;
            }
            std::thread::sleep(Duration::from_millis(if attempt == 0 { 24 } else { 40 }));
        }

        // 修饰键还压着的时候注入 Ctrl+C，目标程序收到的是 Ctrl+Alt+C —— 复制不了。
        // 所以先等它松开；但**只给很短的时间**：用户按住热键不松手（Ctrl+Alt+M
        // 按着不放太常见了）时，干等就是在烧预算，等完了还得在 Alt 压着时发键。
        // 超时就把还按着的修饰键用 keyup「顶开」—— 效果和用户松手一样，
        // 这样按住热键也能正常抓到选中内容。
        let t_mod = std::time::Instant::now();
        // 只抓一次的场景（快捷键直接执行）多等一会儿：那一次机会更不能浪费
        let mod_wait = if attempts == 1 { 180 } else if attempt == 0 { 80 } else { 40 };
        if !win32::wait_modifiers_released(mod_wait) {
            win32::release_modifiers();
            // Win 键不能顶开（开始菜单会被弹出来，焦点全跑掉），只能多等一会儿
            if win32::win_key_down() {
                win32::wait_modifiers_released(600);
            }
            // 给目标窗口几毫秒消化这几条 keyup，不然它的键盘状态里 Ctrl 还是按着的
            std::thread::sleep(Duration::from_millis(if attempt == 0 { 12 } else { 8 }));
        }
        // 等修饰键花的时间不算进抓取预算：用户按住热键，不该把等剪贴板的钱挤没了
        deadline = deadline + t_mod.elapsed();
        crate::popup::timing!(t_cap, "  第{}次：修饰键处理完", attempt + 1);

        // 修饰键刚松开时，目标程序的键盘状态可能还没清干净（它还在等 keyup 的处理），
        // 这时候注入的 Ctrl+C 会被当成「组合键的一部分」丢掉 —— 这是「时而抓到
        // 时而抓不到」最常见的一种。等一小会儿再发，比多发一次便宜得多。
        std::thread::sleep(Duration::from_millis(if attempts == 1 { 18 } else { 8 }));
        let before = win32::clipboard_sequence();
        if let Err(err) = win32::send_keys(&keys) {
            note_failure(&format!("发不出 {keys}：{err}"));
            break;
        }
        crate::popup::timing!(t_cap, "  第{}次：复制键已发出", attempt + 1);

        // 余下的预算分给这一轮：首轮多给一点，重试轮少给一点
        let left = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_millis() as u64;
        // 第一轮给「剩余预算的一大半」：真慢的程序（Office / 大文件）写完剪贴板
        // 常常要 300ms 以上，只给 130ms 就会误判成失败、白白多发一次复制键
        let budget = if attempt + 1 == attempts {
            left.max(20)
        } else {
            (left * 3 / 4).max(40)
        };
        if wait_for_clipboard(before, budget) {
            crate::popup::timing!(t_cap, "  第{}次：剪贴板变了", attempt + 1);
            let seq = win32::clipboard_sequence();
            selection = detect(win32::snapshot_clipboard());
            // 有些程序是「先占位、再写数据」：序号变了但内容还没写完，这时
            // detect 会给 None。再读几次（**不重发按键**），比多发一次 Ctrl+C
            // 便宜得多，也不会在终端里造成中断。
            let mut reread = 0;
            while selection.is_none() && reread < 8 {
                std::thread::sleep(Duration::from_millis(25));
                selection = detect(win32::snapshot_clipboard());
                reread += 1;
            }
            // 只有「真把剪贴板换掉了」才需要还原，而且丢给后台线程去还原：
            // 备份里如果是一张大图，写回去要几十上百毫秒，没必要挡着菜单弹出来。
            if let Some(backup) = backup.take() {
                restore_later(backup, seq);
            }
            break;
        }
        note_failure(&format!(
            "第 {} 次没抓到：{budget}ms 内剪贴板没变化（目标进程 {}）",
            attempt + 1,
            win32::window_process_id(target)
        ));
        // 这里**不还原剪贴板**了：序号没变就说明剪贴板根本没被写过（我们只读不写），
        // 每轮都还原一次纯属白花时间 —— 以前一次弹出里最多会白烧掉 0.5 秒。
        // 「上一次写进去的内容被当成这一次的变化」也不会发生：每一轮的
        // `before` 都是现取的序号。
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let selection = selection
        .filter(|selection| !selection.kinds.is_empty())
        .map(Rc::new);
    // 调试用：`GPUI_MENU_DEBUG_CAPTURE=1` 把这次抓到什么打出来
    // （按住热键那个问题就是靠它确认「注入的复制键到底有没有生效」）
    if std::env::var_os("GPUI_MENU_DEBUG_CAPTURE").is_some() {
        match &selection {
            Some(found) => eprintln!(
                "[popup-menu] 抓到选中内容：{:?}（{} 个字，{} 个文件，用时 {}ms）",
                preview(&found.text),
                found.text.chars().count(),
                found.files.len(),
                t_cap.elapsed().as_millis()
            ),
            None => eprintln!(
                "[popup-menu] 这次没抓到选中内容（用时 {}ms）",
                t_cap.elapsed().as_millis()
            ),
        }
    }
    set(selection.clone());
    selection
}

/// 把剪贴板放回抓取之前的样子（后台线程）。
///
/// 先睡 30ms 让目标程序把 `WM_CLIPBOARDUPDATE` 处理完，再核对一次序号：
/// 还是我们复制完那一刻的序号才写回去 —— 否则说明这期间用户 / 别的程序
/// 往剪贴板里放了新东西，这时候还原反而把人家刚复制的内容冲掉了。
fn restore_later(backup: ClipboardBackup, expected_seq: u32) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        if win32::clipboard_sequence() == expected_seq {
            win32::restore_clipboard(&backup);
        }
    });
}

/// 等剪贴板序号变化并且「写完」（连续 `STABLE_MS` 毫秒没再动）。
///
/// 返回是否等到了变化；超时返回 `false`。
fn wait_for_clipboard(before: u32, wait_ms: u64) -> bool {
    let t0 = std::time::Instant::now();
    let deadline = t0 + Duration::from_millis(wait_ms);
    let mut changed = false;
    let mut last_seq = before;
    let mut stable_at: Option<std::time::Instant> = None;
    loop {
        let seq = win32::clipboard_sequence();
        if seq != before {
            changed = true;
        }
        if seq != last_seq {
            // 序号又动了一下：内容还在写，重新计时稳定期
            last_seq = seq;
            stable_at = Some(std::time::Instant::now());
        }
        if changed {
            if let Some(at) = stable_at {
                if at.elapsed() >= STABLE_MS {
                    return true;
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            if crate::popup::timing_enabled() {
                println!(
                    "[timing]       (等待 {}ms 预算，实际 {}ms)",
                    wait_ms,
                    t0.elapsed().as_millis()
                );
            }
            return changed;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detect, ext_of, exts_hit, exts_of, file_name, kinds_of, parse_exts, preview, Context,
        MultiFileRule, Selection, SelectionKind,
    };
    use crate::win32::ClipboardBackup;

    #[test]
    fn preview_flattens_newlines() {
        assert_eq!(preview("第一行\n第二行"), "第一行 第二行");
        assert_eq!(preview("  前后空白  "), "前后空白");
    }

    #[test]
    fn long_text_is_truncated_with_an_ellipsis() {
        let text = "字".repeat(80);
        let shown = preview(&text);
        assert!(shown.ends_with('…'));
        assert!(shown.chars().count() <= 47);
    }

    #[test]
    fn file_names_come_from_the_last_segment() {
        assert_eq!(file_name(r"C:\Users\me\报告.pdf"), "报告.pdf");
        assert_eq!(file_name("/home/me/a.png"), "a.png");
        assert_eq!(file_name("a.txt"), "a.txt");
    }

    #[test]
    fn file_selection_renders_paths_for_placeholders() {
        let selection = Selection {
            kinds: vec![SelectionKind::File],
            text: r"C:\a b\一.txt".to_string(),
            files: vec![r"C:\a b\一.txt".to_string(), r"C:\二.txt".to_string()],
            image: None,
            snapshot: None,
        };
        // 带空格的路径自己加引号，命令行直接用
        assert_eq!(
            selection.placeholder_text(),
            "\"C:\\a b\\一.txt\" C:\\二.txt"
        );
        let (badge, label) = selection.headline();
        assert_eq!(badge, "📄");
        assert_eq!(label, "2 个文件 · 一.txt");
        assert_eq!(selection.kind_label(), "文件");
    }

    #[test]
    fn an_image_file_is_just_a_file() {
        // 后缀不再变出额外类型：.png 就是「文件」，要不要单独给它一个菜单
        // 由菜单项自己的 `file_exts` 决定
        let selection = Selection {
            kinds: vec![SelectionKind::File],
            files: vec![r"C:\x\图.png".to_string()],
            ..Selection::default()
        };
        assert!(selection.has(SelectionKind::File));
        assert!(!selection.has(SelectionKind::Image));
        assert_eq!(selection.headline().0, "📄");
        assert_eq!(selection.kind_label(), "文件");
    }

    #[test]
    fn legacy_subtype_keys_fall_back_to_file() {
        // 老配置里写的是 video / audio / document / code / archive
        for key in ["video", "audio", "document", "code", "archive"] {
            assert_eq!(SelectionKind::from_key(key), Some(SelectionKind::File));
        }
        assert_eq!(SelectionKind::from_key("Image"), Some(SelectionKind::Image));
    }

    #[test]
    fn plain_image_has_no_placeholder_text() {
        let selection = Selection {
            kinds: vec![SelectionKind::Image],
            image: Some((1920, 1080)),
            ..Selection::default()
        };
        assert_eq!(selection.placeholder_text(), "");
        assert_eq!(selection.headline(), ("🖼", "图片 1920×1080".to_string()));
    }

    #[test]
    fn kind_keys_round_trip() {
        for kind in SelectionKind::ALL {
            assert_eq!(SelectionKind::from_key(kind.key()), Some(kind));
        }
        assert_eq!(SelectionKind::from_key("nope"), None);
    }

    // ---- detect()：从剪贴板快照认类型（纯函数，不碰真剪贴板）----

    fn hdrop(paths: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&20u32.to_le_bytes()); // pFiles 偏移
        bytes.extend_from_slice(&0u64.to_le_bytes()); // pt（POINT）
        bytes.extend_from_slice(&0u32.to_le_bytes()); // fNC = 0
        bytes.extend_from_slice(&0u32.to_le_bytes()); // fWide = 0（ANSI）
        for path in paths {
            bytes.extend_from_slice(path.as_bytes());
            bytes.push(0);
        }
        bytes.push(0);
        bytes
    }

    fn wide(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(|unit| unit.to_le_bytes())
            .collect()
    }

    fn dib(w: i32, h: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&40u32.to_le_bytes()); // biSize
        bytes.extend_from_slice(&w.to_le_bytes());
        bytes.extend_from_slice(&h.to_le_bytes());
        bytes
    }

    #[test]
    fn detect_reads_files_before_text() {
        // 复制一批 .png：剪贴板里同时有文件列表和（路径）文本 —— 必须是「文件」
        let backup = ClipboardBackup::from_raw(
            vec![
                (crate::win32::CF_HDROP, hdrop(&[r"C:\a\图.png"])),
                (crate::win32::CF_UNICODETEXT, wide(r"C:\a\图.png")),
            ],
            vec![crate::win32::CF_HDROP, crate::win32::CF_UNICODETEXT],
        );
        let selection = detect(Some(backup)).unwrap();
        assert_eq!(selection.kinds, vec![SelectionKind::File]);
        assert_eq!(selection.files, vec![r"C:\a\图.png".to_string()]);
    }

    #[test]
    fn detect_treats_plain_image_as_image_only() {
        let backup = ClipboardBackup::from_raw(
            vec![(crate::win32::CF_DIB, dib(640, 480))],
            vec![crate::win32::CF_DIB],
        );
        let selection = detect(Some(backup)).unwrap();
        assert_eq!(selection.kinds, vec![SelectionKind::Image]);
        assert_eq!(selection.image, Some((640, 480)));
        assert_eq!(selection.placeholder_text(), "");
    }

    #[test]
    fn detect_reads_text_when_there_is_nothing_else() {
        let backup = ClipboardBackup::from_raw(
            vec![(crate::win32::CF_UNICODETEXT, wide("选中的一句话"))],
            vec![crate::win32::CF_UNICODETEXT],
        );
        let selection = detect(Some(backup)).unwrap();
        assert_eq!(selection.kinds, vec![SelectionKind::Text]);
        assert_eq!(selection.text, "选中的一句话");
    }

    #[test]
    fn detect_keeps_file_type_even_if_paths_fail_to_parse() {
        // 只有 CF_HDROP 的格式号、读不到字节：类型照样是「文件」
        let backup = ClipboardBackup::from_raw(Vec::new(), vec![crate::win32::CF_HDROP]);
        let selection = detect(Some(backup)).unwrap();
        assert_eq!(selection.kinds, vec![SelectionKind::File]);
        assert_eq!(selection.headline().0, "📄");
    }

    // ---- 后缀 ----

    #[test]
    fn typed_extensions_are_parsed_leniently() {
        assert_eq!(parse_exts("mp4|mkv"), vec!["mp4", "mkv"]);
        // 点号、逗号、中文逗号、空格、大小写都认
        assert_eq!(parse_exts(" .MP4, mkv，avi "), vec!["mp4", "mkv", "avi"]);
        // 空段和重复项丢掉
        assert_eq!(parse_exts("mp4||MP4|"), vec!["mp4"]);
        assert_eq!(parse_exts("   "), Vec::<String>::new());
        assert_eq!(parse_exts("|"), Vec::<String>::new());
    }

    #[test]
    fn extension_comes_from_the_file_name_only() {
        assert_eq!(ext_of(r"C:\a\片子.MKV"), Some("mkv".to_string()));
        assert_eq!(ext_of("/home/me/photo.jpeg"), Some("jpeg".to_string()));
        // 目录名里的点不算
        assert_eq!(ext_of(r"C:\a.png\inner"), None);
        assert_eq!(ext_of("no-extension"), None);
        // 隐藏文件（.gitignore）不算后缀
        assert_eq!(ext_of(".gitignore"), None);
    }

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn exts_of_only_looks_at_files() {
        let dir = std::env::current_dir().unwrap().display().to_string();
        assert_eq!(
            exts_of(&paths(&[r"C:\a\A.MP4", &dir])),
            vec!["mp4".to_string()]
        );
    }

    #[test]
    fn all_same_requires_every_file_to_hit() {
        let list = vec!["mp4".to_string(), "mkv".to_string()];
        // 两种都认的视频格式：混选也算命中
        assert!(exts_hit(
            &paths(&["a.mp4", "b.mkv"]),
            &list,
            MultiFileRule::AllSame
        ));
        // 混进一张图片：默认就不算「视频菜单」
        assert!(!exts_hit(
            &paths(&["a.mp4", "b.png"]),
            &list,
            MultiFileRule::AllSame
        ));
        // 只看第一个文件的话仍然算
        assert!(exts_hit(
            &paths(&["a.mp4", "b.png"]),
            &list,
            MultiFileRule::First
        ));
    }

    #[test]
    fn an_empty_list_means_every_file() {
        assert!(exts_hit(
            &paths(&["a.png"]),
            &[],
            MultiFileRule::AllSame
        ));
        // 只有文件夹时没有后缀可比：给了列表就不命中
        let dir = std::env::current_dir().unwrap().display().to_string();
        assert!(!exts_hit(
            &paths(&[&dir]),
            &["mp4".to_string()],
            MultiFileRule::AllSame
        ));
    }

    #[test]
    fn folders_are_their_own_kind_and_do_not_count_as_files() {
        // 用当前目录当「一个真实存在的文件夹」（测试里不能凭空造路径）
        let dir = std::env::current_dir().unwrap().display().to_string();
        assert_eq!(kinds_of(&[dir.clone()]), vec![SelectionKind::Folder]);
        // 文件和文件夹混选：列表里有文件，就带上「文件」
        assert_eq!(
            kinds_of(&[dir, r"C:\a\x.mp4".to_string()]),
            vec![SelectionKind::File, SelectionKind::Folder]
        );
        // 路径解析不出来时宁可当文件
        assert_eq!(kinds_of(&[]), vec![SelectionKind::File]);
    }

    #[test]
    fn context_carries_the_files_for_suffix_checks() {
        let context = Context::new(
            vec![SelectionKind::File],
            paths(&[r"C:\a\片子.mkv"]),
        );
        assert!(context.has(SelectionKind::File));
        assert!(!context.has(SelectionKind::Text));
        assert!(context.exts_hit(&["mkv".to_string()], MultiFileRule::AllSame));
        assert!(!context.exts_hit(&["mp4".to_string()], MultiFileRule::AllSame));
    }

    #[test]
    fn appended_arguments_quote_paths_with_spaces() {
        let selection = Selection {
            kinds: vec![SelectionKind::File],
            text: String::new(),
            files: vec![r"D:\a b\1.txt".to_string(), r"D:\plain.txt".to_string()],
            image: None,
            snapshot: None,
        };
        super::set(Some(std::rc::Rc::new(selection)));
        assert_eq!(
            super::append_arg().as_deref(),
            Some(r#""D:\a b\1.txt" D:\plain.txt"#)
        );

        // 选中的是文字：原样接上，换行压成空格
        let selection = Selection {
            kinds: vec![SelectionKind::Text],
            text: "hello\nworld".to_string(),
            files: Vec::new(),
            image: None,
            snapshot: None,
        };
        super::set(Some(std::rc::Rc::new(selection)));
        assert_eq!(super::append_arg().as_deref(), Some("hello world"));

        // 什么都没有 → 不接
        super::clear();
        assert_eq!(super::append_arg(), None);
    }

    #[test]
    fn quotes_are_not_added_twice() {
        assert_eq!(super::quote_path(r#""D:\a b\c.txt""#), r#""D:\a b\c.txt""#);
        assert_eq!(super::quote_path(r"D:\a\b.txt"), r"D:\a\b.txt");
        assert_eq!(super::quote_path(" D:\\a b.txt "), r#""D:\a b.txt""#);
    }
}
