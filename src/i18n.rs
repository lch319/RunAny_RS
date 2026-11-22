//! 界面语言：中文是原文，英文查一张表。
//!
//! 做法是「中文当 key」：调用方把中文原文传进来，英文模式下去表里找对应的英文，
//! 表里没有就原样返回中文。这样加新界面时不需要先登记，先把中文写好，
//! 想翻译的时候再往表里补一条 —— 没翻译的部分也不会显示成空白或 key 名。
//!
//! 为什么是 `HashMap` 而不是一个大 `match`：界面文案现在有几百条，
//! 而设置页每帧都要重算几十上百个标签。`match` 是线性比较，几百条乘上几百次
//! 调用就是每帧十几万次字符串比较；`HashMap` 是一次哈希。表只建一次
//! （`OnceLock`），中文模式下连表都不进。

use std::collections::HashMap;
use std::sync::OnceLock;


/// 当前是不是英文界面。
///
/// 用一个 `AtomicBool` 而不是每次去读设置：`t()` 每帧要调几百次，
/// 读设置是「线程本地 + `RefCell` 借用」，比一次原子读贵得多；
/// 更要紧的是**默认菜单模板**（`settings::default_items()`）里也要用 `t()`，
/// 而它正好是在设置快照初始化过程中被调用的 —— 那一刻再回去读设置会重入死锁。
static ENGLISH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 设置里的语言一变就同步过来（`settings::install` / `settings::update` 里调）。
pub fn set_english(on: bool) {
    ENGLISH.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// 把一句界面文案换成本地化后的样子（未收录的原样返回）。
pub fn t(text: &'static str) -> &'static str {
    if is_english() {
        english(text)
    } else {
        text
    }
}

fn english(text: &'static str) -> &'static str {
    table().get(text).copied().unwrap_or(text)
}

/// 运行时才知道的文案（菜单项名称是存在 `*.runa` 里的 `String`）。
///
/// 只有**表里有的**才换：自己写进菜单的名字表里没有，原样显示。
/// 这样「默认模板那几条」跟着语言走，用户自己起的名字不会被乱改。
pub fn t_dynamic(text: &str) -> &str {
    if !is_english() {
        return text;
    }
    table().get(text).copied().unwrap_or(text)
}

/// 中英两版直接给（不进表）。
///
/// 给「同一个中文字面量在不同位置要不同译文」的地方用：比如「菜单项」在「类型」
/// 那个位置是 `Item`，而左侧导航里的页面名要的是 `Menu Items` —— 表里一个 key
/// 只能挂一个译文，这种就得自己挑。
pub fn either(zh: &'static str, en: &'static str) -> &'static str {
    if is_english() { en } else { zh }
}

/// 现在是不是英文界面。动态拼接的文案（format! 里的数字 + 量词）用得上，
/// 这类句子没法整句查表，只能自己按语言分叉写两遍。
pub fn is_english() -> bool {
    ENGLISH.load(std::sync::atomic::Ordering::Relaxed)
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m: HashMap<&'static str, &'static str> = HashMap::new();
        m.insert("列宽方式", "Width mode");
        m.insert("固定", "Fixed");
        m.insert("按内容", "Fit content");
        m.insert("最大列宽", "Max width");
        m.insert("「固定」所有列一样宽；「按内容」每一列各自按最长的菜单项算（不超过最大列宽），子菜单不会因为名字短而空一大块", "Fixed keeps every column the same width. Fit content sizes each column to its longest item (up to Max width), so short submenus don't leave a big empty area.");
        m.insert("字符 / emoji，或 .svg .ico .png 路径。选文件、把图片直接拖到这整行（输入框或右边的方块）都行：图片会复制进程序目录的 Icon/badges，配置里只记相对路径，整个文件夹拷走图标也不断", "A character / emoji, or a .svg .ico .png path. Use Pick file, or just drop an image anywhere on this row (the text box or the square): it is copied into Icon/badges next to the program and only a relative path is saved, so icons survive moving the folder.");
        m.insert("设置", "Settings");
        m.insert("确定", "OK");
        m.insert("取消", "Cancel");
        m.insert("应用", "Apply");
        m.insert("撤销", "Revert");
        m.insert("重做", "Redo");
        m.insert("删除", "Delete");
        m.insert("添加", "Add");
        m.insert("关闭菜单", "Close menu");
        m.insert("收起", "Close");
        m.insert("选文件", "Pick file");
        m.insert("选程序…", "Pick app…");
        m.insert("打开文件对话框…", "Browse…");
        m.insert("重新扫描", "Rescan");
        m.insert("重新检测", "Check again");
        m.insert("打开菜单", "Open menu");
        m.insert("打开菜单试试", "Open the menu");
        m.insert("打开配置目录", "Open config folder");
        m.insert("恢复自动", "Back to auto");
        m.insert("恢复默认模板", "Reset template");
        m.insert("恢复默认菜单", "Reset menu");
        m.insert("恢复默认设置", "Reset all settings");
        m.insert("再点一次确认", "Click again to confirm");
        m.insert("再点确认", "Confirm");
        m.insert("点外面关闭", "Click outside to close");
        m.insert("点外面取消", "Click outside to cancel");
        m.insert("当前", "Current");
        m.insert("条", "entries");
        m.insert("个字符", "chars");
        m.insert("像素", "px");
        m.insert("毫秒", "ms");
        m.insert("刚刚", "just now");
        m.insert("标准", "Standard");
        m.insert("通用", "General");
        m.insert("大", "Large");
        m.insert("小", "Small");
        m.insert("浅", "Light");
        m.insert("深", "Dark");
        m.insert("外观", "Appearance");
        m.insert("常规", "General");
        m.insert("菜单项", "Item");
        m.insert("托盘", "Tray");
        m.insert("索引搜索", "Search Index");
        m.insert("文件名", "File Names");
        m.insert("热字符串", "Hotstrings");
        m.insert("行为", "Behavior");
        m.insert("关于", "About");
        m.insert("菜单外观", "Menu Appearance");
        m.insert("托盘图标", "Tray icon");
        m.insert("行为与占位符", "Behavior & Placeholders");
        m.insert("配色、尺寸与显示项，改完立刻生效", "Colors, sizes and visible parts — applied instantly");
        m.insert("搜索筛选、拖左侧把手排序、拖行体框选，点 ✎ 改文案 / 动作 / 快捷键 / 热字符串", "Search; drag the handle on the left to reorder; drag a row to rubber-band select; click ✎ to edit label / action / shortcut / hotstring");
        m.insert("托盘图标本身 + 它右键弹出的菜单（和菜单项同一套编辑方式）", "The tray icon itself plus its right-click menu");
        m.insert("「运行程序」只填文件名时自动补成完整路径；全盘查名字走 Everything", "Auto-resolve bare program names to full paths; global name lookup via Everything");
        m.insert("菜单里用到的裸文件名，各自解析到哪、同名有几份", "Bare file names used by the menu, and where each one resolves");
        m.insert("敲一串字就执行菜单项（AHK 的热字符串）；短语本身在「菜单项」页给每一项配", "Type a phrase to run a menu item (AHK hotstrings)");
        m.insert("弹出前抓取选中内容；动作参数里的内置变量", "Capture the selection before popping up; built-in variables");
        m.insert("快捷键与操作说明", "Shortcuts and controls");
        m.insert("开机自启、界面语言、让路的程序、最近运行", "Startup, language, programs to step aside for, recently used");
        m.insert("开机自启、启动权限、界面语言", "Start with Windows, elevation, language");
        m.insert("预览", "Preview");
        m.insert("主题配色", "Theme");
        m.insert("主题模式", "Theme mode");
        m.insert("配色", "Colors");
        m.insert("尺寸", "Size");
        m.insert("内容", "Content");
        m.insert("内容模板", "Content template");
        m.insert("字体", "Font");
        m.insert("字号", "Size");
        m.insert("行高", "Row height");
        m.insert("列宽", "Width");
        m.insert("圆角", "Corner radius");
        m.insert("透明度", "Opacity");
        m.insert("不透明度", "Opacity");
        m.insert("强调色", "Accent");
        m.insert("背景色", "Background");
        m.insert("选中项", "Selected");
        m.insert("菜单提示", "Menu hints");
        m.insert("设置页提示", "Settings hints");
        m.insert("加速键", "Accel key");
        m.insert("浅色主题", "Light theme");
        m.insert("深色主题", "Dark theme");
        m.insert("浅色", "Light");
        m.insert("深色", "Dark");
        m.insert("跟随系统", "Follow system");
        m.insert("固定浅色", "Always light");
        m.insert("固定深色", "Always dark");
        m.insert("系统默认", "System default");
        m.insert("系统为浅色时使用", "Used while the system is light");
        m.insert("系统为深色时使用", "Used while the system is dark");
        m.insert("系统当前是浅色，跟随系统时用下面「浅色」这一套", "System is currently light; \"Follow system\" uses the light set below");
        m.insert("系统当前是深色，跟随系统时用下面「深色」这一套", "System is currently dark; \"Follow system\" uses the dark set below");
        m.insert("整个菜单一起变淡（文字、图标、边框都跟着），不是只改底色", "Fades the whole menu (text, icons, borders), not just the background");
        m.insert("点色块即时预览；＋ 打开颜色面板（点色块 / 手填实时预览，按「确定」才算数，点外面取消）。「边框」是菜单卡片那一圈描边，不要就在「尺寸」里把「显示边框」关掉", "Swatches preview instantly; ＋ opens the picker (preview live, click OK to keep). Border is the 1px outline of the menu card - turn it off with Show border under Size");
        m.insert("点一下整套换掉（浅色 / 深色各一份，跟随系统时两个模式都好看）；下面还能继续细调", "One click swaps the whole set (light + dark); fine-tune below");
        m.insert("拖动即时生效，可以开着菜单对照调", "Drag to preview live against the open menu");
        m.insert("菜单里显示哪些东西", "What the menu shows");
        m.insert("显示图标位", "Show icon slot");
        m.insert("显示快捷键提示", "Show shortcut hints");
        m.insert("显示分隔线", "Show separators");
        m.insert("关掉后会跳过所有分隔线", "Skips every separator when off");
        m.insert("自动取程序图标", "Auto program icons");
        m.insert("图标位空着时，用第一个「运行程序」动作里那个 exe 的图标（单项可以在编辑窗口里关掉）", "When the icon slot is empty, use the exe icon from the first Run action");
        m.insert("图标大小", "Icon size");
        m.insert("菜单项左侧的小方块", "The small square on the left of each row");
        m.insert("菜单项右侧的 Ctrl+N 之类", "The Ctrl+N style hint on the right");
        m.insert("菜单里的文字整体缩放", "Scales all menu text");
        m.insert("菜单和设置界面用同一款字体，选完立刻生效", "The menu and settings share one font, applied instantly");
        m.insert("点开列出系统里装了的全部字体", "Lists every installed font");
        m.insert("悬停提示", "Hover Tooltip");
        m.insert("显示悬停提示", "Show hover tooltip");
        m.insert("鼠标在菜单项上停一会儿，在光标旁边弹一张小卡片", "Hover a row for a moment to pop a small card next to the cursor");
        m.insert("停多久弹出来", "Delay");
        m.insert("最大卡片宽度", "Max card width");
        m.insert("只是「看一眼」用的，不影响点击；关掉就完全不弹", "Read-only, never affects clicks; turn off to hide entirely");
        m.insert("到点自己消失（鼠标停在上面时不会消失）", "Auto-hides (stays while hovered)");
        m.insert("鼠标移上去不消失；列了好几条的时候，点哪条就直接执行哪条", "Stays while hovered; with several matches, click one to run it");
        m.insert("菜单里的搜索框", "Search box in the menu");
        m.insert("第一列顶上或底上放一条：打字就筛菜单项 / 子菜单（和「菜单项」页那套搜索一样）", "Puts a search row in the first column: typing filters items like the Menu Items page");
        m.insert("「显示全部菜单」入口", "\"Show Full Menu\" Entry");
        m.insert("不是完整菜单就补一条", "Add when the menu isn't complete");
        m.insert("筛过 / 托盘菜单 / 快捷键只弹一支 —— 这些都算「不完整」，都会出现这条入口", "Filtered / tray / single-branch menus all count as incomplete and get this entry");
        m.insert("放在哪儿", "Placement");
        m.insert("顶部（第一项）或者底部（最后一项）", "Top (first row) or bottom (last row)");
        m.insert("最近运行", "Recently Used");
        m.insert("启用最近运行", "Enable recently used");
        m.insert("执行过的菜单项会记一笔；关掉就不记录、也不在菜单里显示", "Records each run; off means no tracking and nothing shown");
        m.insert("把刚用过的几项提到菜单顶部 / 底部；完整菜单和筛过的菜单里都生效", "Puts recently used items at the top / bottom, in full and filtered menus alike");
        m.insert("排在哪一头", "Placement");
        m.insert("顶部：一开菜单就是它，回车最快；底部：不打断原来的第一项", "Top: first thing you see; Bottom: keeps the original first row");
        m.insert("最多显示几条", "Max entries");
        m.insert("只显示这么多个最近项；记录本身留 40 条，改小改大都还在", "Only this many are shown; 40 are kept, so raising it later is fine");
        m.insert("按选中内容筛选菜单", "Filter menu by selection");
        m.insert("按选中内容切换菜单", "Filter Menu by Selection");
        m.insert("按选中内容筛过的菜单里也显示", "Also show in filtered menus");
        m.insert("抓到文字 / 文件后菜单只剩适用的几项，最近运行照样排在前面", "Keeps recent items on top even when the menu is reduced by the selection");
        m.insert("快捷键直接呼出的子菜单里也显示", "Also show in hotkey submenus");
        m.insert("某一项配了「只弹这一支」的快捷键时，那一支的菜单里也带最近运行", "Submenus opened straight from a hotkey also carry recent items");
        m.insert("禁用应用", "Blocked Apps");
        m.insert("启用「禁用应用」名单", "Enable blocked apps");
        m.insert("这些程序的窗口在前面时，快捷键和热字符串提示全部让路；托盘图标手动点开面板照旧可用", "While these apps are in front, hotkeys and hints step aside; the tray icon still works");
        m.insert("典型是全屏游戏 / 剪辑软件：快捷键被抢会中断操作，提示条闪一下更烦", "Typical: full-screen games and editors — stolen hotkeys break the flow");
        m.insert("名单还是空的：拖一次上面的条，或者用「选程序…」挑一个", "List is empty: drag the bar above onto a window, or use \"Pick app…\"");
        m.insert("⣿  按住这里，拖到目标窗口上", "⣿  Hold here and drag onto a target window");
        m.insert("松开鼠标就加进名单…", "Release to add it…");
        m.insert("启动", "Startup");
        m.insert("开机自动启动", "Start with Windows");
        m.insert("登录 Windows 后自动在托盘运行（关掉会把注册表和任务计划里的都清掉）", "Runs in the tray after sign-in (turning off clears both the Run key and the task)");
        m.insert("选择开机自启方式", "Startup method");
        m.insert("普通开机启动", "At boot");
        m.insert("用户登录时启动", "At sign-in");
        m.insert("任务计划在「用户登录」时拉起，比注册表方式快一两秒，还能静默提权", "The scheduled task fires at sign-in: a second or two faster, and can elevate silently");
        m.insert("启动时以管理员运行", "Run as administrator");
        m.insert("以最高权限启动；任务计划方式静默提权，普通方式需要 UAC 确认", "Starts elevated; silently via the task, with a UAC prompt via the Run key");
        m.insert("系统里的状态", "System state");
        m.insert("真实查出来的；和上面的开关对不上，说明被别的程序改过", "Queried from Windows; a mismatch means something else changed it");
        m.insert("注册表 Run 已写", "Run key set");
        m.insert("注册表 Run 未写", "Run key absent");
        m.insert("任务计划已建", "Task created");
        m.insert("任务计划未建", "Task absent");
        m.insert("语言", "Language");
        m.insert("界面语言：切换后设置窗口和菜单里的文字马上跟着变", "Interface language — the window and the menu follow immediately");
        m.insert("托盘右键菜单", "Tray Context Menu");
        m.insert("显示托盘图标", "Show tray icon");
        m.insert("关掉之后程序就只能在后台跑，靠全局快捷键唤出", "With this off the app is background-only; reach it via the global hotkey");
        m.insert("左键点图标", "Left click");
        m.insert("右键永远是这份托盘菜单", "Right click always opens this tray menu");
        m.insert("现在用内置图标（RA 字母标）", "Using the built-in icon (RA mark)");
        m.insert("用这个文件里的图标", "Using the icon in this file");
        m.insert("常驻在系统托盘里：左键弹菜单、右键弹下面这份托盘菜单", "Lives in the tray: left click pops the menu, right click this tray menu");
        m.insert("和「菜单项」页一模一样：搜索、筛选、拖动排序、增删改；改完右键托盘图标就能看到", "Same editor as Menu Items: search, filter, drag, add / remove");
        m.insert("选中内容", "Selected Content");
        m.insert("弹出前抓取选中内容", "Capture selection before popup");
        m.insert("选中文字后按热键：先向原窗口发一次复制键把选中内容抓过来，抓完立刻把剪贴板原样还原", "On popup, send the copy key to grab the selection, then restore the clipboard");
        m.insert("抓取按键", "Copy keys");
        m.insert("终端里 Ctrl+C 是「中断」，这种情况改成 Ctrl+Insert", "Ctrl+C interrupts in terminals; use Ctrl+Insert there");
        m.insert("复制后最多等待", "Max wait");
        m.insert("发完复制键后，等剪贴板把内容写全的最长时间；经常抓不到 / 文件被认成文字就调大", "How long to wait for the clipboard; raise it if captures are flaky");
        m.insert("关掉之后无论选中什么，都显示完整菜单", "With this off, the full menu always shows");
        m.insert("给菜单项标上「适用选中内容」，弹出时按抓到的类型只显示用得上的那些", "Tag items with a selection type; the menu keeps only matching ones");
        m.insert("复制了文字、代码、一句话", "Copied text, code, a sentence");
        m.insert("文件", "File");
        m.insert("资源管理器里复制的任何文件", "Any file copied in Explorer");
        m.insert("文件夹", "Folder");
        m.insert("选中了一个 / 多个文件夹（文件夹不算文件）", "One or more folders picked (a folder is not a file)");
        m.insert("图片", "Image");
        m.insert("剪贴板里直接是位图（截图、从网页复制一张图），不是图片文件 —— 图片文件按「文件」算", "The clipboard holds a bitmap (a screenshot, an image copied from a page), not an image file — image files count as files");
        m.insert("多个文件的后缀命中", "Multi-file extensions");
        m.insert("每个文件的后缀都得在菜单项要求的那串里（一 mp4 + 一 png 就不算 mp4 菜单）", "Every file must match (mp4 + png does not count as an mp4 menu)");
        m.insert("只看第一个文件的后缀（顺序会影响结果）", "Only the first file's extension (order matters)");
        m.insert("菜单项的「适用选中内容」有三种：跟随上级（默认）、单独指定、不参与", "Three modes: inherit (default), own types, or opt out");
        m.insert("单独指定的普通项会提到根菜单（放最前面 / 按原顺序 / 子菜单前后，每项自己定；在编辑窗口里选）；只有一个子菜单配了筛选时它会被摊平，配了两个以上就各自保留；子菜单只剩一个子菜单时外壳会被塌掉", "Items with their own types are promoted to the root; a single matching submenu is flattened, two or more are kept");
        m.insert("「只对哪些后缀出现」写在编辑窗口里：勾了「文件」之后才会出现那个输入框，多个后缀用 | 隔开（mp4|mkv），留空 = 任何文件都出现", "Extensions are set in the editor: separate with | (mp4|mkv), blank means any file");
        m.insert("没标类型的根层项是「通用项」—— 选中内容时会一起隐藏；没选中任何东西时永远显示完整菜单", "Untyped root items are generic: hidden while something is selected");
        m.insert("占位符", "Placeholders");
        m.insert("在「菜单项 → 编辑 → 动作」里还有一排胶囊，点一下就把变量插进参数", "Chips in the action editor insert these variables for you");
        m.insert("一行一条：路径里包含这段文字就不收录", "One per line: paths containing it are skipped");
        m.insert("排除规则（路径里包含这段文字就不收录）", "Exclude rules");
        m.insert("认哪些后缀名（可执行文件不止 .exe）", "Extensions");
        m.insert("认哪些后缀，; 分隔，如 exe;bat;cmd;msi", "Separate extensions with ;");
        m.insert("多个后缀用 ; 隔开，例如 exe;bat;cmd;msi;msp;msu;ps1;scr;vbs。改完点「重新扫描」生效。", "Separate with ; e.g. exe;bat;cmd;msi;ps1 — rescan to apply");
        m.insert("程序索引", "Program Index");
        m.insert("扫 PATH、Program Files、LocalAppData\\Programs，给每个可执行文件记下它的完整路径", "Scans PATH, Program Files and LocalAppData\\Programs for every executable");
        m.insert("索引是空的", "Index is empty");
        m.insert("还没建过索引", "No index yet");
        m.insert("怎么挑", "How to Pick");
        m.insert("同名多份时用哪个", "Which copy wins");
        m.insert("同一个文件名在磁盘上有好几份时，按这里的规则选一份；认哪些后缀也在这里定", "Chooses one copy when several share a name; extensions are set here too");
        m.insert("查名字", "Name Lookup");
        m.insert("输入文件名，看它解析到哪几个路径…", "Type a file name to see where it resolves");
        m.insert("看看某个文件名在磁盘上有几份，各在哪", "See how many copies exist and where");
        m.insert("在上面输入文件名（不用带路径），这里会列出索引里所有同名的完整路径。", "Type a bare file name above to list every matching path");
        m.insert("索引里没有这个名字。名字里带不带 .exe 都能查；也可以重新扫描一次。", "Not in the index (with or without .exe); try rescanning");
        m.insert("索引里没有这个文件的记录，可以直接打开文件对话框去挑。", "No record for this file; browse for it instead");
        m.insert("菜单里用到的文件名", "File Names in Use");
        m.insert("现在都配了哪些", "What the menu uses");
        m.insert("现在菜单里没有「只填文件名」的动作。在编辑窗口里把程序写成 notepad.exe 这种就会出现。", "No Run action uses a bare file name yet (write notepad.exe in the editor)");
        m.insert("只写了名字的动作，执行时会解析成完整路径；不对就点「选文件」挑一个，同名会先全部列出来", "Bare names resolve at run time; pick a file if the guess is wrong");
        m.insert("没找到，动作会交给 cmd 自己按 PATH 找", "Not found — cmd will look it up on PATH itself");
        m.insert("本地索引里没有的名字，交给 Everything 在全盘查完整路径", "Names missing locally go to Everything for a global lookup");
        m.insert("没有连上 Everything", "Not connected to Everything");
        m.insert("把 Everything64.dll 放到本程序旁边（或正常安装 Everything）就能自动连上；没有它也不影响使用。", "Drop Everything64.dll next to this app (or install Everything normally) and it connects by itself; everything still works without it.");
        m.insert("启用热字符串", "Enable hotstrings");
        m.insert("关掉就撤掉那个键盘钩子；一条短语都没配的时候也等于没开（零开销）", "Removes the keyboard hook; no phrases means no work at all");
        m.insert("在任何程序里敲一串字 → 执行绑定的菜单项（和「点击」「快捷键」并列的第三种触发方式）", "Type a phrase anywhere to run a menu item — the third trigger, next to click and hotkey");
        m.insert("一匹配就执行（不用结束符）", "Run on match (no end char)");
        m.insert("中文输入法也匹配", "Match under Chinese IME");
        m.insert("短语在哪配", "Where to set phrases");
        m.insert("短语是「菜单项」的触发方式之一，在「菜单项」页点 ✎ 就能配", "A phrase is one trigger of a menu item; click ✎ on the Menu Items page");
        m.insert("筛出有热字符串的项", "Filter items with phrases");
        m.insert("提示", "Hint");
        m.insert("隐藏热字符串提示", "Hide hotstring hint");
        m.insert("只在后台替换，不弹那张小卡片", "Replaces in place without popping the card");
        m.insert("敲到第几个字符冒出来、长什么样、停多久、多透明、贴在哪儿", "When it appears, how it looks, how long and where");
        m.insert("按几个字符出现提示", "Hint after");
        m.insert("打满这么多个字符开始提示；敲得比这少就当普通打字，什么都别弹", "Hints start after this many characters");
        m.insert("提示显示位置", "Hint position");
        m.insert("跟着鼠标，还是贴着正在输入的那个光标（插入点）", "Next to the mouse or next to the caret");
        m.insert("跟着鼠标时相对鼠标、跟着光标时相对插入点；允许负数", "Relative to the mouse or the caret; negatives allowed");
        m.insert("负数往上放；贴着光标时 -6 左右刚好不压住字", "Negative moves up; about -6 keeps it off the text");
        m.insert("提示相对坐标 X", "Hint offset X");
        m.insert("提示相对坐标 Y", "Hint offset Y");
        m.insert("提示显示时长", "Hint duration");
        m.insert("提示显示透明度", "Hint opacity");
        m.insert("越透明越不挡视线；调到 100% 就是实心卡片", "More transparency blocks less; 100% is solid");
        m.insert("提示启动路径最长字数", "Max path chars");
        m.insert("提示里那行完整路径最多显示几个字，0 = 不显示路径", "Max characters of the path line; 0 hides it");
        m.insert("提示可以用鼠标点", "Clickable hint");
        m.insert("结束字符", "End characters");
        m.insert("空格 / 回车 / Tab 永远算打完了；这里填的符号也算（AHK 的 EndChars）", "Space / Enter / Tab always end a phrase; these symbols do too");
        m.insert("区分大小写", "Case sensitive");
        m.insert("关掉就跟 AHK 一样：FtW 和 ftw 算同一个短语", "Off matches AHK: FtW and ftw are the same phrase");
        m.insert("全局快捷键", "Global Hotkey");
        m.insert("弹出菜单", "Popup menu");
        m.insert("在任何应用里按下都会在鼠标处弹出", "Pops the menu at the cursor from any app");
        m.insert("在任何程序里按下都生效", "Works in any program");
        m.insert("点「录入」直接按组合键，或者手打；Ctrl / Alt / Shift / Win 至少带一个", "Click Record and press the combo, or type it; needs Ctrl / Alt / Shift / Win");
        m.insert("改完立刻重新注册，重启程序也记得住", "Re-registered instantly, and remembered");
        m.insert("在「行为」页可以改成别的组合", "Change it on the Behavior page");
        m.insert("菜单项快捷键", "Item hotkeys");
        m.insert("编辑窗口里的「触发快捷键」：普通项按下就直接执行，子菜单按下只弹这一支", "The trigger hotkey in the editor: runs the item, or pops just that submenu");
        m.insert("菜单项 / 子菜单也能有触发键（编辑窗口里的「触发快捷键」）：普通项按下就直接执行，子菜单按下只弹这一支", "Items and submenus can have trigger hotkeys (set in the editor)");
        m.insert("操作", "Controls");
        m.insert("鼠标和键盘都能用", "Mouse and keyboard both work");
        m.insert("在当前列里移动高亮，会自动跟滚", "Moves the highlight, scrolling as needed");
        m.insert("展开子菜单 / 返回上一级", "Expand a submenu / go back");
        m.insert("执行菜单项（有子菜单则展开）", "Run the item (or expand it)");
        m.insert("先收子菜单，再按一次关闭", "Collapses, then closes");
        m.insert("滚轮 / 拖动 / 点箭头", "Wheel / drag / arrows");
        m.insert("长菜单的三种滚动方式", "Three ways to scroll a long menu");
        m.insert("点击空白处", "Click empty space");
        m.insert("关闭菜单，焦点还给原来的窗口", "Closes and returns focus");
        m.insert("运行程序时接上选中内容（默认）", "Append selection to Run actions (default)");
        m.insert("所有「运行程序」动作的默认值：命令末尾补一个空格再拼上选中内容；菜单项里可以单独设成开启 / 关闭，优先级高于这里", "Default for Run actions: a space plus the selection; per-item settings win");
        m.insert("抓不到（原窗口没有选中内容）就什么都不加，菜单和平时一样", "Nothing is appended when there is no selection");
        m.insert("当前配置", "Current Config");
        m.insert("改动已经自动保存", "Changes are saved automatically");
        m.insert("菜单配置", "Profile");
        m.insert("设置文件", "Settings file");
        m.insert("菜单配置文件就在 menus 文件夹里，拷走一个 .runa 就是一套菜单", "Menu profiles live in menus/; copy one .runa to share a whole menu");
        m.insert("其他", "Other");
        m.insert("目录里还没有菜单配置文件", "No menu profile in menus/ yet");
        m.insert("换一套只动菜单项，配色 / 快捷键不跟着变", "Switching changes menu items only; colors and hotkeys stay");
        m.insert("目录", "Folder");
        m.insert("建副本", "Duplicate");
        m.insert("改名", "Rename");
        m.insert("新名字", "New name");
        m.insert("副本名字", "Copy name");
        m.insert("给这份配置起个名字", "Name this profile");
        m.insert("只恢复这一份菜单（托盘那一支会重新补回来）；配色和尺寸不动", "Resets just this profile (the tray branch comes back); colors stay");
        m.insert("取色", "Pick");
        m.insert("拖色块选色 · 色号可直接编辑", "Drag to pick · hex is editable");
        m.insert("不是颜色", "not a color");
        m.insert("对齐方式", "Align");
        m.insert("整行靠左", "Whole row left");
        m.insert("整行居中", "Whole row centered");
        m.insert("整行靠右", "Whole row right");
        m.insert("一个大框，换行就是下一条；行首加 ? = 这一行里有空占位符就整行不显示", "One line per entry; prefix ? to hide the line when a placeholder is empty");
        m.insert("行首的 ? 是条件行：这一行里的占位符只要有一个没内容，整行就不显示（省得留下空行）", "A leading ? makes the line conditional");
        m.insert("上面点一下就插到光标处。每个占位符代表：", "Click to insert at the caret. Each means:");
        m.insert("· 下面有每个占位符的说明", "· every placeholder is explained below");
        m.insert("保持显示", "Keep open");
        m.insert("菜单出现在鼠标位置；开着「保持显示」时点别的窗口也不会收起，方便边调边看", "The menu opens at the cursor; \"Keep open\" lets you tweak while it stays");
        m.insert("↺ 自动", "↺ Auto");
        m.insert("字（0 隐藏）", "chars (0 hides)");
        m.insert(".ico / .png 文件路径（留空用内置图标）", ".ico / .png path (blank = built-in icon)");
        m.insert("DNF.exe 或完整路径", "DNF.exe or a full path");
        m.insert("RunAny_RS · 设置", "RunAny_RS · Settings");
        m.insert("v0.2.0 · 便携版", "v0.2.0 · Portable");
        m.insert("● 有未应用的改动（菜单里已经能预览到）", "● Unapplied changes (previewed in the menu)");
        m.insert("✓ 已应用到菜单 · 没有待应用的改动", "✓ Applied · nothing pending");
        m.insert("只要这次弹的不是完整菜单，就补一条入口，点它原地换成完整菜单", "Adds one entry whenever the menu isn't complete; clicking it swaps in the full menu");
        m.insert("文字", "Text");
        m.insert("用这个", "Use this");
        m.insert("程序", "Program");
        m.insert("程序搜索", "Program Search");
        m.insert("索引", "Index");
        m.insert("编辑", "Edit");
        m.insert("（无可用目录）", "(no writable folder)");
        m.insert("（这一项用当前模板什么都不显示）", "(renders nothing with this template)");
        m.insert("不参与", "Opt out");
        m.insert("跟随上级", "Inherit");
        m.insert("默认跟随全局", "Follow global by default");
        m.insert("（没选类型）", "(no type)");
        m.insert("不要", "Off");
        m.insert("顶部", "Top");
        m.insert("底部", "Bottom");
        m.insert("什么都不做", "Do nothing");
        m.insert("弹出完整菜单", "Open the full menu");
        m.insert("弹出托盘菜单", "Open the tray menu");
        m.insert("例如 notepad.exe，或一条完整的 shell 命令", "e.g. notepad.exe or a full shell command");
        m.insert("例如 https://gpui.rs，或某个文件路径", "e.g. https://gpui.rs or a file path");
        m.insert("例如 Ctrl+S，连按多个用空格隔开：Ctrl+A Delete", "e.g. Ctrl+S; separate sequences with a space");
        m.insert("直接当键盘输入打出去，支持中文", "Typed out as keystrokes; Chinese supported");
        m.insert("从下面的内置列表里选一个", "Pick one from the list below");
        m.insert("运行程序", "Run");
        m.insert("打开网址", "Open URL");
        m.insert("发送按键", "Send keys");
        m.insert("发送文本", "Send text");
        m.insert("内置动作", "Built-in");
        m.insert("光标插入处", "At the caret");
        m.insert("鼠标旁边", "Next to the mouse");
        m.insert("最前面", "Very top");
        m.insert("按原顺序", "Original order");
        m.insert("子菜单前", "Before submenus");
        m.insert("子菜单后", "After submenus");
        m.insert("修改时间最新", "Newest first");
        m.insert("版本号最高", "Highest version");
        m.insert("路径最短", "Shortest path");
        m.insert("打开设置窗口", "Open settings");
        m.insert("打开关于", "Open About");
        m.insert("重新加载菜单", "Reload menu");
        m.insert("重启软件", "Restart app");
        m.insert("退出程序", "Quit");
        m.insert("只关闭菜单", "Just close the menu");
        m.insert("复制选中内容", "Copy selection");
        m.insert("打开完整菜单", "Show full menu");
        m.insert("显示全部菜单", "Show full menu");
        m.insert("完整菜单", "Full menu");
        m.insert("复制名称", "Copy name");
        m.insert("编辑这一项…", "Edit this item…");
        m.insert("在菜单里隐藏", "Hidden in menu");
        m.insert("重新显示", "Show again");
        m.insert("用程序图标当图标", "Use program icon");
        m.insert("清空图标位", "Clear icon slot");
        m.insert("删除这一项", "Delete this item");
        m.insert("（未命名）", "(untitled)");
        m.insert("（未选择）", "(none)");
        m.insert("打开文件…", "Open file…");
        m.insert("打开项目主页", "Open project page");
        m.insert("设置…", "Settings…");
        m.insert("退出", "Quit");
        m.insert("切换主题", "Switch theme");
        m.insert("搜索", "Search");
        m.insert("复制", "Copy");
        m.insert("粘贴", "Paste");
        m.insert("剪切", "Cut");
        m.insert("放大", "Zoom in");
        m.insert("缩小", "Zoom out");
        m.insert("缩放", "Zoom");
        m.insert("视图", "View");
        m.insert("Everything 联动", "Everything Integration");
        m.insert("填 exe 名或完整路径都行；填一个目录（如 D:\\Games\\DNF）会把里面这一层的 exe 全部加进去", "An exe name or full path; point at a folder to add every exe inside it");
        m.insert("中文", "Chinese");
        m.insert("全部", "All");
        m.insert("分隔线", "Separator");
        m.insert("多列", "Column");
        m.insert("多列符（从这里另起一列）", "Column break");
        m.insert("多列符：从这里另起一列", "Column break: start a new column here");
        m.insert("子菜单", "Submenu");
        m.insert("子项", "Child");
        m.insert("已隐藏", "Hidden");
        m.insert("隐藏的", "Hidden");
        m.insert("搜索名称 / 备注 / 快捷键 / 动作参数", "Search name / note / hotkey / action");
        m.insert("无图标位", "No icon");
        m.insert("无快捷键", "No shortcut");
        m.insert("普通", "Plain");
        m.insert("有备注", "With note");
        m.insert("有快捷键", "With hotkey");
        m.insert("有热字符串", "With hotstring");
        m.insert("有适用选中内容", "Fits a selection");
        m.insert("每次都问", "Ask every time");
        m.insert("总是完整路径", "Always full path");
        m.insert("总是裸文件名", "Always bare name");
        m.insert("把选中文件加到菜单里时", "When adding selected files to the menu");
        m.insert("每次弹一句问「完整路径还是裸文件名」", "Ask full path or bare name each time");
        m.insert("直接写完整路径（换机器 / 换安装目录就失效）", "Write the full path (breaks when moved to another machine)");
        m.insert("只写文件名，运行时交给 PATH 去找（notepad.exe 这种）", "Write only the file name and let PATH find it (like notepad.exe)");
        m.insert("按条件填（不用记语法）", "Fill in by condition (no syntax to remember)");
        m.insert("比什么", "What to compare");
        m.insert("怎么比", "How to compare");
        m.insert("包含", "Contains");
        m.insert("等于", "Equals");
        m.insert("不是", "Is not");
        m.insert("程序", "Program");
        m.insert("窗口类名", "Window class");
        m.insert("光标下控件", "Control under cursor");
        m.insert("鼠标下", "Under mouse");
        m.insert("类名", "Class");
        m.insert("路径", "Path");
        m.insert("标题", "Title");
        m.insert("导入 ini", "Import .ini");
        m.insert("方式", "Mode");
        m.insert("手动生成", "Manual");
        m.insert("实时生成", "Live");
        m.insert("仅开启软件时", "On app start");
        m.insert(
            "点「立即生成」才更新；只补缺，不覆盖同名项",
            "Only when you press Generate; fills the gaps and never overwrites existing items",
        );
        m.insert(
            "每次呼出菜单前重建这一支：会清空它下面现有的项，且不能再手动添加",
            "Rebuilt right before the menu opens: clears this submenu and blocks adding items by hand",
        );
        m.insert(
            "软件启动时重建一次：会清空它下面现有的项，之后不再变",
            "Rebuilt once at startup: clears this submenu, then stays put",
        );
        m.insert("还没有菜单项", "No items yet");
        m.insert("清空并切换", "Clear and switch");
        m.insert(
            "这一支开着「实时生成」：菜单项由文件夹决定，不能手动添加（要改请在子菜单里改方式）",
            "This submenu is in Live mode: its items come from the folder and cannot be added by hand (change the mode inside the submenu)",
        );
        m.insert(
            "这一支开着「实时生成」，加进来的项会在下次呼出时被清掉，已拦下",
            "This submenu is in Live mode — pasted items would be wiped on the next open, so it was blocked",
        );
        m.insert(
            "这一支开着「实时生成」，拖进来的项会在下次呼出时被清掉，已拦下",
            "This submenu is in Live mode — dropped items would be wiped on the next open, so it was blocked",
        );
        m.insert("执行原版插件功能", "Run a RunAny plugin function");
        m.insert(
            "原版 RunAny 的插件调用，例如 XiaoYao_plus[hotkeyStatus](...)；功能还没接上，先原样存着",
            "A RunAny plugin call such as XiaoYao_plus[hotkeyStatus](...) — not wired up yet, kept verbatim",
        );
        m.insert("局部快捷键", "Hotkeys with conditions");
        m.insert("筛选出局部快捷键的项", "Filter items with conditions");
        m.insert(
            "带生效条件的触发键，这一页看汇总",
            "Trigger keys that only fire in certain windows — summarized here",
        );
        m.insert(
            "只在特定窗口里才生效的触发键：每一条都写着生效条件（exe: / title: / mouse.exe: …），条件不满足时不吞键，那个组合在别的程序里还是它本来的意思",
            "Trigger keys that only fire in certain windows: each one carries a condition (exe: / title: / mouse.exe: …). When the condition fails the key is not swallowed, so it keeps its normal meaning in other apps",
        );
        m.insert(
            "鼠标下的窗口（条件里写 mouse.）",
            "Window under the mouse (write mouse. in the condition)",
        );
        m.insert(
            "前缀前加 mouse. 就改成看「鼠标底下那扇窗口」：mouse.exe: / mouse.title: / mouse.class: / mouse.path:。不写就都看活动窗口（拿到焦点的那个）；ctrl: 只能看活动窗口",
            "Put mouse. before the prefix to look at the window under the mouse instead: mouse.exe: / mouse.title: / mouse.class: / mouse.path:. Without it every condition looks at the active window (the one with focus); ctrl: always means the active window",
        );
        m.insert(
            "光标下控件只能看活动窗口，不能再加 mouse.",
            "The control under the cursor belongs to the active window, so it cannot take mouse.",
        );
        m.insert("＋ 加一条", "+ Add line");
        m.insert("收起", "Collapse");
        m.insert("⧉ 复制", "⧉ Copy");
        m.insert("填一个值，例如 QQ.exe / 群聊 / Button", "Enter a value, e.g. QQ.exe / chat / Button");
        m.insert("✎ 配动作", "✎ Edit action");
        m.insert("没被任何规则用的直达项（删规则时留下的）：", "Direct items no rule uses (left over from deleted rules):");
        m.insert("建完直接开动作编辑器，然后在上面的规则里选它；子菜单适合「命中后摊开给人挑」（比如一组截图工具）", "Creating one opens its action editor — then pick it in a rule above. Submenus suit \"hit, then let me pick\" (a set of screenshot tools, say)");
        m.insert("顺序就是优先级；每行第二行就是这条规则跑的那个直达项，点「配动作」改它", "Order is priority; the second line of each rule is the direct item it runs — use \"Edit action\" to change it");
        m.insert("没有匹配的菜单项（换个词，或者点「全部」）", "No matching items (try another word, or All)");
        m.insert("添加分隔线", "Add separator");
        m.insert("添加多列符", "Add column break");
        m.insert("添加子菜单", "Add submenu");
        m.insert("添加菜单项", "Add item");
        m.insert("父项", "Parent");
        m.insert("说明", "Note");
        m.insert("还没有菜单项，点上面的「添加菜单项」", "No items yet — use \"Add item\" above");
        m.insert("托盘菜单还是空的，点上面的「添加菜单项」", "Tray menu is empty — use \"Add item\" above");
        m.insert("还没选中项：新项加到末尾（点一下行标签即可选中）", "Nothing selected: new rows go last (click a row to select)");
        m.insert("AHK 的 * 选项：敲完最后一个字立刻执行，不用再补空格 / 回车。单项也可以自己开", "AHK's * option: runs as soon as the last character is typed, no space needed");
        m.insert("中文模式下按拼音匹配：命中就删掉输入法里那段拼音再执行；关掉就完全不管中文输入法", "Matches pinyin while the IME is in Chinese mode, then deletes that pinyin; off ignores the IME");
        m.insert("本程序不再自己建全盘索引：几百万个文件的索引要常驻几十 MB 内存、还得定时重扫。全盘按文件名找路径改成问 Everything（一次几毫秒，本程序不存任何索引）。没装 / 没运行 Everything 时自动退回上面那份可执行文件小索引，功能不受影响。", "This app no longer builds a whole-disk index: an index of millions of files costs tens of MB of RAM and has to be re-scanned on a timer. Looking up a path by file name now asks Everything instead (a few ms per query, nothing is stored here). Without Everything it falls back to the small executable index above, and nothing breaks.");
        m.insert("同名的那几份里，挑最近改过的那个", "Among same-name copies, take the most recently modified one");
        m.insert("读文件本身的版本号（PE 资源），挑大的；读不到就比时间", "Reads the version resource inside the file and takes the larger one; falls back to mtime");
        m.insert("路径越短越靠前，一般就是最外层那个", "Shorter paths rank higher, which is usually the outermost copy");
        m.insert("?{kind} · {children} 项", "?{kind} · {children} items");
        m.insert("?加速键  {accel}", "?Accel  {accel}");
        m.insert("?快捷键  {shortcut}", "?Shortcut  {shortcut}");
        m.insert("?触发键  {hotkey}", "?Trigger  {hotkey}");
        m.insert("Everything 不支持这个请求", "Everything does not support this request");
        m.insert("Everything 内存不足", "Everything ran out of memory");
        m.insert("Everything 查询超时", "Everything query timed out");
        m.insert("Everything 正在重建数据库", "Everything is rebuilding its database");
        m.insert("Everything 没有运行（找不到 IPC 窗口）", "Everything is not running (no IPC window found)");
        m.insert("es.exe 返回失败（Everything 没在运行？）", "es.exe failed (is Everything running?)");
        m.insert("mp4|mkv（留空 = 所有文件）", "mp4|mkv (empty = all files)");
        m.insert("· 参数还没填", "· no arguments yet");
        m.insert("↑ ↓ 移动 · → 展开子菜单 · ← 返回上级 · Enter 执行 · Esc 或点击空白处关闭", "↑ ↓ move · → open submenu · ← back · Enter run · Esc or click outside to close");
        m.insert("● 改动即时预览；取消会放弃未应用的改动", "● Changes preview live; Cancel discards anything not applied");
        m.insert("「跟随全局」= 跟「外观 → 内容 → 自动取程序图标」那个开关，现在是【关】", "\"Follow global\" mirrors Appearance → Content → Auto program icon, currently [off]");
        m.insert("「跟随全局」= 跟「外观 → 内容 → 自动取程序图标」那个开关，现在是【开】", "\"Follow global\" mirrors Appearance → Content → Auto program icon, currently [on]");
        m.insert("一个字母或数字", "One letter or digit");
        m.insert("一行一条；{label} 这类是占位符，下面有说明", "One per line; things like {label} are placeholders, explained below");
        m.insert("不用", "None");
        m.insert("不等结束符", "No end char needed");
        m.insert("不管在哪个程序里按下，都会先抓选中内容、再只弹这一支", "Works in any app: grabs the selection first, then opens only this branch");
        m.insert("不管在哪个程序里按下，都会先抓选中内容、再直接执行动作", "Works in any app: grabs the selection first, then runs the action");
        m.insert("不自动取图标（图标位留空就不显示）", "No automatic icon (an empty badge shows nothing)");
        m.insert("之后等", "then wait");
        m.insert("例如 Ctrl+Alt+1", "e.g. Ctrl+Alt+1");
        m.insert("例如 Ctrl+N", "e.g. Ctrl+N");
        m.insert("全选", "Select all");
        m.insert("全部见「设置 → 行为」", "See Settings → Behavior for the full list");
        m.insert("全部都得命中", "All must match");
        m.insert("关闭", "Off");
        m.insert("写法不合法：修饰键 + 一个键，例如 Ctrl+Alt+1", "Invalid: a modifier plus one key, e.g. Ctrl+Alt+1");
        m.insert("动作 · 按顺序执行", "Actions · run in order");
        m.insert("单独指定：会浮到根菜单（原来那层不再重复），浮上去放哪儿在下面选", "Explicit: floats up to the root menu (not repeated in its old place); pick where below");
        m.insert("单独指定：它已经在根菜单第一层，只在选中这类内容时出现", "Explicit: already at the root level, shown only for this kind of selection");
        m.insert("只对哪些后缀出现（留空 = 所有文件）", "Only for these extensions (empty = all files)");
        m.insert("只影响显示，不涉及动作", "Display only, actions are unaffected");
        m.insert("只执行程序本身，不拼选中内容", "Runs the program only, without the selection");
        m.insert("名字不合法", "Invalid name");
        m.insert("名字不能为空", "Name cannot be empty");
        m.insert("名称", "Name");
        m.insert("和设置里的「菜单项」一样：拖动排序、✎ 编辑、✕ 删除、加子项", "Same as Menu Items in Settings: drag to sort, ✎ edit, ✕ delete, add children");
        m.insert("图标位", "Badge");
        m.insert("在菜单里显示", "Shown in the menu");
        m.insert("基本信息", "Basics");
        m.insert("备注", "Note");
        m.insert("字符 / emoji，或 .svg .ico .png 路径", "A character / emoji, or a .svg .ico .png path");
        m.insert("字符 / 图片", "Character / image");
        m.insert("它已经在根菜单第一层，位置就是现在这儿；四颗胶囊只在「子菜单里的项单独指定类型、浮上来」时才有得选", "Already at the root level and stays here; the four pills only matter when an item inside a submenu is set explicitly and floats up");
        m.insert("就算上级命中了，有选中内容时这一项也不出现", "Even if its parent matches, this item is hidden when something is selected");
        m.insert("开启", "On");
        m.insert("录入", "Record");
        m.insert("快捷键提示", "Shortcut hint");
        m.insert("所有文件", "All files");
        m.insert("打开设置", "Open Settings");
        m.insert("执行时是「程序 + 空格 + 选中内容」；文件路径带空格会自动加双引号", "Runs as program + space + selection; paths with spaces are quoted");
        m.insert("找不到 exe 目录", "Cannot locate the exe folder");
        m.insert("按下要用的组合键（Esc 取消，Backspace 清空）", "Press the combo you want (Esc cancels, Backspace clears)");
        m.insert("按组合键…", "Press keys…");
        m.insert("接上选中", "Append selection");
        m.insert("搜索菜单项…", "Search items…");
        m.insert("改动已应用到菜单", "Changes applied");
        m.insert("敲什么就触发，例如 ftw", "The text that triggers it, e.g. ftw");
        m.insert("显示", "Shown");
        m.insert("普通项", "Plain item");
        m.insert("普通项才会执行右边的动作", "Only plain items run the actions on the right");
        m.insert("最近执行：（还没有选择过菜单项）", "Last ran: (nothing picked yet)");
        m.insert("未命名", "Untitled");
        m.insert("根菜单上的子菜单：选中了内容也不会自动出现，除非里面有项单独指定了类型", "A submenu at the root level does not appear for a selection unless one of its items sets a type explicitly");
        m.insert("根菜单上的通用项：选中了内容时会藏起来（没选中时照常显示）", "A generic item at the root: hidden when something is selected, shown when nothing is");
        m.insert("每条执行完可设间隔毫秒，再走下一条", "Each step can wait a number of milliseconds before the next one");
        m.insert("没有匹配的项", "No matching items");
        m.insert("没有找到 Everything SDK（Everything64.dll）或 es.exe", "Neither the Everything SDK (Everything64.dll) nor es.exe was found");
        m.insert("浮到根菜单以后放哪儿", "Where it goes after floating to the root");
        m.insert("添加动作", "Add action");
        m.insert("清空", "Clear");
        m.insert("热字符串（敲一串字就执行）", "Hotstring (runs when you type a phrase)");
        m.insert("用程序图标", "Program icon");
        m.insert("留空 = 不参与。填上之后在别的程序里敲这串字、再敲空格 / 回车 / Tab 就执行这一项", "Empty = off. Once set, typing this phrase anywhere then space / Enter / Tab runs the item");
        m.insert("留空 = 任何文件都会出现", "Empty = shown for any file");
        m.insert("留空 = 只能从菜单里点；点「录入」按一下组合键，或者自己填（例如 Ctrl+Alt+1）", "Empty = menu only; click Record and press a combo, or type one (e.g. Ctrl+Alt+1)");
        m.insert("看第一个文件", "Look at the first file");
        m.insert("类型", "Type");
        m.insert("给自己看的备注（显示在菜单项右侧）", "A note for yourself (shown to the right of the item)");
        m.insert("编辑菜单项", "Edit item");
        m.insert("自动图标", "Automatic icon");
        m.insert("菜单项文案", "Item text");
        m.insert("触发快捷键", "Trigger hotkey");
        m.insert("跟上级子菜单一样 —— 上级命中时它就出现", "Same as its parent submenu — appears when the parent matches");
        m.insert("跟上级子菜单一样（上级命中时才出现）", "Same as the parent submenu (appears when the parent matches)");
        m.insert("跟随全局", "Global");
        m.insert("还原为菜单项", "Back to a plain item");
        m.insert("这一支只出现在托盘图标的右键菜单里，不会进弹出菜单；里面的子项就是托盘菜单的内容", "This branch only shows in the tray icon's context menu, never in the popup; its children are the tray menu");
        m.insert("这一支归它管：选中这类内容时出现，里面的项默认跟着它", "This branch owns that kind: it appears for such selections and its items follow it by default");
        m.insert("这一项的显示方式（优先于全局设置）", "How this item shows up (overrides the global setting)");
        m.insert("这一项还没有「运行程序」动作，暂时没有图标可取", "This item has no \"Run program\" action yet, so there is no icon to take");
        m.insert("这个菜单项已经被删掉了", "This item has been deleted");
        m.insert("适用选中内容", "Applies to selection");
        m.insert("隐藏", "Hidden");
        m.insert("默认蓝", "Default Blue");
        m.insert("白底 + 标准蓝", "White base + standard blue");
        m.insert("石墨灰", "Graphite");
        m.insert("耐看的中性灰，不抢眼", "Understated neutral grey");
        m.insert("暖纸", "Warm Paper");
        m.insert("米色纸感 + 琥珀强调", "Creamy paper with an amber accent");
        m.insert("靛青", "Indigo");
        m.insert("冷色调蓝灰，偏工作风", "Cool blue-grey, work-like");
        m.insert("森绿", "Forest");
        m.insert("清爽的墨绿 + 青绿", "Fresh deep green with teal");
        m.insert("玫红", "Rose");
        m.insert("柔和的粉底 + 玫红强调", "Soft pink base with a rose accent");
        m.insert("深夜紫", "Midnight Purple");
        m.insert("偏紫的深色，夜里不刺眼", "Deep purple that is easy at night");
        m.insert("珊瑚", "Coral");
        m.insert("暖橘红，比玫红更活泼", "Warm orange-red, livelier than rose");
        m.insert("类型：菜单项 / 子菜单 / 分隔线", "Type: item / submenu / separator");
        m.insert("加速键（那一个字母）", "Accel key (the single letter)");
        m.insert("触发键（真正注册的那个）", "Trigger (the key actually registered)");
        m.insert("图标位上的字符 / 文件", "Badge character / file");
        m.insert("第一条动作", "First action");
        m.insert("全部动作，每条一行", "All actions, one per line");
        m.insert("动作条数", "Number of actions");
        m.insert("子菜单里有多少项", "How many children the submenu has");
        m.insert("这一项被隐藏时显示「在菜单里隐藏」", "Shows \"hidden in menu\" when the item is hidden");
        m.insert("在菜单里的层级：父 › 子", "Depth in the menu: parent › child");
        m.insert("内部 id", "Internal id");
        m.insert("菜单项页", "Menu Items");
        m.insert("靠左", "Left");
        m.insert("居中", "Center");
        m.insert("靠右", "Right");
        m.insert("自定义", "Custom");
        m.insert("弹出菜单前抓到的选中内容（文字原文 / 文件路径）；也可以写成 %getZz%", "The selection grabbed before the menu opens (raw text / file paths); %getZz% works too");
        m.insert("选中内容的类型：文字 / 文件 / 文件夹 / 图片（多个用空格隔开）", "Kind of selection: text / file / folder / image (space-separated)");
        m.insert("选中的文件个数（文字和图片是 0）", "Number of selected files (0 for text and images)");
        m.insert("点击时剪贴板里的文本", "Clipboard text at click time");
        m.insert("本程序所在目录", "Folder of this app");
        m.insert("本程序完整路径", "Full path of this app");
        m.insert("settings.json 的完整路径", "Full path of settings.json");
        m.insert("今天，如 2026-11-14", "Today, e.g. 2026-11-14");
        m.insert("现在，如 18:15:27", "Now, e.g. 18:15:27");
        m.insert("年，如 2026", "Year, e.g. 2026");
        m.insert("月，如 11", "Month, e.g. 11");
        m.insert("日，如 14", "Day, e.g. 14");
        m.insert("鼠标屏幕坐标 X", "Cursor X on screen");
        m.insert("鼠标屏幕坐标 Y", "Cursor Y on screen");
        m.insert("把文件名解析成完整路径，如 {path: QQ.exe}（后面必须跟 : 和文件名）", "Resolves a file name to a full path, e.g. {path: QQ.exe} (must be followed by : and the name)");
        m.insert("抓到的内容成为菜单第一项（点它就是复制），动作参数里也能用 {selection}", "The grabbed content becomes the first item (clicking it copies); actions can also use {selection}");
        m.insert("整张卡片的默认对齐；某一行想单独对齐，行首插 {left} / {center} / {right}", "Default alignment for the whole card; to align one line, start it with {left} / {center} / {right}");
        m.insert("动作参数里写 {名字}，点菜单项执行时才替换成实际值", "Write {name} in the arguments; it is replaced with the real value when the item runs");
        m.insert("新建文件", "New file");
        m.insert("最近打开", "Recent");
        m.insert("清空最近记录", "Clear recent list");
        m.insert("变量命名", "Variable naming");
        m.insert("显示侧边栏", "Show sidebar");
        m.insert("恢复默认", "Reset to defaults");
        m.insert("插入签名", "Insert signature");
        m.insert("新建菜单项", "New item");
        m.insert("新建子菜单", "New submenu");
        m.insert("新建子项", "New child");
        m.insert("新建子项（改个名字，或者删掉）", "New child (rename it or delete it)");
        m.insert("1 驼峰命名（camelCase）", "1 camelCase");
        m.insert("2 帕斯卡命名（PascalCase）", "2 PascalCase");
        m.insert("3 下划线命名（snake_case）", "3 snake_case");
        m.insert("4 短横线命名（kebab-case）", "4 kebab-case");
        m.insert("5 常量命名（SCREAMING_SNAKE_CASE）", "5 SCREAMING_SNAKE_CASE");
        m.insert("子项留在右边；还原时收起来，不会丢", "Children stay on the right; demoting folds them away, nothing lost");
        m.insert("已填的动作会留着，点错了也不怕", "Actions you filled in are kept - safe to try");
        m.insert("从上往下执行，第一条失败也会继续", "Runs top to bottom; a failing step does not stop the rest");
        m.insert("不参与筛选", "Excluded");
        m.insert("还没有动作：点下面的「添加动作」，或者把程序文件直接拖进这个窗口", "No actions yet: click \"Add action\" below, or drag a program file into this window");
        m.insert("完整路径", "Full path");
        m.insert("只填文件名", "File name only");
        m.insert("完整路径最省事；只填文件名的话跟着 PATH 走，换了机器可能找不到。文件夹一律用完整路径。", "Full path is safest; a bare file name is looked up on PATH and may not exist on another machine. Folders always use the full path.");
        m.insert("改成子菜单后已填的动作还留着；点错了也不会丢", "Actions you filled in are kept when it becomes a submenu");
        m.insert("取消选择", "Deselect");
        m.insert("立即备份", "Back up now");
        m.insert("打开备份目录", "Open backup folder");
        m.insert("恢复", "Restore");
        m.insert("配置备份", "Backup");
        m.insert("已备份到", "Backed up to");
        m.insert("备份失败", "Backup failed");
        m.insert("恢复失败", "Restore failed");
        m.insert("这份备份已经不在了", "That backup is gone");
        m.insert("备份里含 settings.json 和整个 menus 文件夹", "A backup contains settings.json and the whole menus folder");
        m.insert("还没有备份：点「立即备份」，之后随时能恢复到这一刻", "No backups yet - click Back up now to save this moment");
        m.insert("文件写回去了，但读不回来（备份可能坏了）", "Files were written back but could not be read (backup may be corrupt)");
        m.insert("换电脑 / 手滑删了菜单，都能一键回到某一刻", "Move machines or undo an accidental delete - jump back to a snapshot");
        m.insert("配置", "Config");
        m.insert("配置与备份", "Config & Backup");
        m.insert("换菜单配置、备份与恢复、配置文件在哪", "Switch menu config, backup & restore, where the files live");
        m.insert("换一份菜单配置只是预览，点「应用 / 确定」才写盘", "Switching a menu config is just a preview - it is written only when you press Apply / OK");
        m.insert("这一份菜单现在有多少项、多宽多高", "How many items this menu has, and how wide/tall it is");
        m.insert("已切到", "Switched to");
        m.insert("点「应用 / 确定」才写入文件，点「取消」则什么都不留", "Apply / OK writes it to disk, Cancel leaves nothing behind");
        m.insert("先选中菜单项再复制", "Select a menu item first, then copy");
        m.insert("已复制", "Copied");
        m.insert("项 · 到目标位置按 Ctrl+V 粘贴（换菜单配置后也能粘）", "item(s) - press Ctrl+V at the target (works across menu configs too)");
        m.insert("复制失败", "Copy failed");
        m.insert("剪贴板里没有菜单项：先选中要复制的项，按 Ctrl+C", "No menu items on the clipboard - select what you want and press Ctrl+C first");
        m.insert("已粘贴", "Pasted");
        m.insert("项（id 已重新编号，不会和现有的撞）", "item(s), renumbered so their ids cannot clash");
        m.insert("项（没选中，加在菜单末尾；id 已重新编号）", "item(s), added at the end of the menu (ids renumbered)");
        m.insert("从 RunAny 导入", "Import from RunAny");
        m.insert("读原版 RunAny.ini：菜单项 / 子菜单 / 分隔线 / 快捷键 / 动作类型一一对应，单独存成一份新配置", "Reads an original RunAny.ini - items, submenus, separators, hotkeys and action types map one to one, saved as its own new config");
        m.insert("选择 RunAny.ini", "Choose RunAny.ini");
        m.insert("注释行会跳过；插件函数（名字[函数](参数)）原样保留并加一条备注", "Comment lines are skipped; plugin calls (name[func](args)) are kept as-is with a note");
        m.insert("这份文件里没读到菜单项", "No menu items found in this file");
        m.insert("匹配只看你实际按了哪些键，不看输入法在不在组字；执行前先让输入法丢掉没上屏的拼音，再按屏幕上真实的字数退格", "Matching only looks at the keys you actually pressed, never at the IME state; before running, the unfinished pinyin is dropped and backspaces match the characters really on screen");
        m.insert("中文输入法", "IME (Chinese input)");
        m.insert("原版关联：选中内容时显示", "Original association: shown when something is selected");
        m.insert("已导入 {} 项（{} 个子菜单、{} 个快捷键），新建配置「{profile}」并切过去了。原版的 `|public` 这类关联只记成备注，不参与筛选（否则菜单会缺项）", "Imported {} items ({} submenus, {} hotkeys) into a new config \"{profile}\" and switched to it. Original `|public` associations are kept as notes only, not as filters (otherwise items would go missing)");
        m.insert("显示边框", "Show border");
        m.insert("边框", "Border");
        m.insert("菜单卡片外面那圈 1px 细线。关掉就是一块纯色，靠底色和阴影分边界（颜色在下面「配色」里调，浅色 / 深色各一份）", "The 1px outline around the menu card. Turned off, the card is a flat block and only colour and shadow separate it (the colour lives under Colours, one per light and dark theme)");
        m.insert("子菜单展开", "Submenus open");
        m.insert("智能", "Smart");
        m.insert("贴名字", "Beside name");
        m.insert("贴卡片", "Beside card");
        m.insert("「贴名字」子菜单紧挨着父项那一列的边缘，第一行尽量和父项那一行齐平；下方放不下就整张往上挪，绝不切成半截（默认，手感最像原版）；「贴卡片」从整张卡片右边出来，不压任何一列，但离父项远", "Beside name keeps the submenu against the parent column's edge and its first row level with the parent row; if it does not fit below it slides up whole instead of being cut (default, closest to the original feel). Beside card opens it past the whole card so no column is covered, but it sits farther from the item.");
        m.insert("动作库", "Actions");
        m.insert("内置动作的目录：搜一搜、看参数，点「添加到菜单」就能用；AHK 脚本也在这里配", "The built-in action catalogue: search, inspect parameters, add one to a menu; AHK scripts live here too");
        m.insert("运行脚本", "Run script");
        m.insert("换一个…", "Change…");
        m.insert("添加到菜单", "Add to menu");
        m.insert("共 {n} 条", "{n} actions");
        m.insert("搜动作：音量、窗口、剪贴板、脚本…", "Search actions: volume, window, clipboard, script…");
        m.insert("没找到对应的动作", "No matching action");
        m.insert("已把「{name}」加到当前配置末尾，去「菜单项」里改名 / 换图标 / 调参数", "Added {name} to the end of this profile - rename it, change its icon or tweak its parameters under Menu Items");
        m.insert("脚本解释器", "Script interpreter");
        m.insert("「运行脚本」类型的动作用它跑 AHK 代码；一般不用管，自动找得到（程序目录 / 注册表 / 安装目录）", "Used to run AutoHotkey code for Run script actions. Usually nothing to do - it is found automatically (program folder, registry, install dir)");
        m.insert("没找到，装一个或手动指定", "Not found - install it or point to it manually");
        m.insert("解释器在「设置 → 动作库」里指定，留空就自动找", "The interpreter is set in Settings > Actions; leave it empty to auto-detect");
        m.insert("从动作库里挑一个；带参数的可以在这里改", "Pick one from the action library; parameters can be edited here");
        m.insert("AutoHotkey v1 / v2 代码，直接在这里写（也可以先粘过来）", "AutoHotkey v1 / v2 code, written right here (paste works too)");
        m.insert("搜一搜、点「添加到菜单」就能用；带参数的动作（音量、亮度、剪贴板变形…）会先填好默认值，参数在 菜单项 → 动作 里改", "Search, then click Add to menu. Actions with parameters (volume, brightness, clipboard transforms) come with defaults; edit them under Menu Items > Actions");
        m.insert("动作库页", "Actions");
        m.insert("一共 {n} 条内置动作", "{n} built-in actions");
        m.insert("音量", "volume");
        m.insert("自动", "Auto");
        m.insert("选择…", "Choose…");
        m.insert("菜单", "Menu");
        m.insert("菜单与程序", "Menu & apps");
        m.insert("声音与屏幕", "Sound & screen");
        m.insert("输入与文本", "Input & text");
        m.insert("系统", "System");
        m.insert("窗口", "Window");
        m.insert("媒体", "Media");
        m.insert("剪贴板", "Clipboard");
        m.insert("输入", "Input");
        m.insert("网络", "Network");
        m.insert("脚本", "Script");
        m.insert("{desc} · 参数：{names}", "{desc} · params: {names}");
        m.insert("收起预览", "Hide preview");
        m.insert("预览参数", "Preview parameters");
        m.insert("预览只是试调，不写进配置；加到菜单后去「菜单项 → 动作」里改", "The preview is just a sandbox - nothing is written. Edit the real values under Menu Items > Actions after adding it.");
        m.insert("计算中…", "Working…");
        m.insert("已复制占位符「{name}」", "Copied {name}");
        m.insert("已添加「{name}」到菜单末尾 —— 去「菜单项」里就能看到它", "Added {name} to the end of the menu - find it under Menu Items");
        m.insert("已复制「{name}」的菜单项 JSON —— 粘进 items 或菜单项列表都认", "Copied the menu item JSON for {name} - paste it into items or back into the list");
        m.insert("复制这一项（Ctrl+V 可粘贴）", "Copy item (paste with Ctrl+V)");
        m.insert("复制一份", "Duplicate");
        m.insert("粘贴到这一项下面", "Paste below this item");
        m.insert("开", "On");
        m.insert("关", "Off");
        m.insert("（空）", "(empty)");
        m.insert("常用", "Common");
        m.insert("搜索筛选、按住一行就能拖动排序，Ctrl / Shift 点选多项，点 ✎ 改文案 / 动作 / 快捷键 / 热字符串", "Search; hold a row to drag it into place, Ctrl / Shift-click for multi-select, click ✎ to edit label / action / shortcut / hotstring");
        m.insert("还没这个文件夹：点右边建一个，把解释器和 .ahk 放进去", "Folder missing - create it and drop the interpreter + .ahk files in");
        m.insert("已认出解释器", "Interpreter found:");
        m.insert("个脚本", "scripts");
        m.insert("空的：放 AutoHotkey.exe 和 .ahk 进来", "Empty: put AutoHotkey.exe and .ahk files here");
        m.insert("只有脚本，缺个 AutoHotkey.exe：", "Scripts but no AutoHotkey.exe:");
        m.insert("建好并打开", "Create and open");
        m.insert("打开文件夹", "Open folder");
        m.insert("选 .ahk 文件…", "Pick .ahk file...");
        m.insert("框里只写文件名也行：Plugins\\ahkv1 / ahkv2 里的脚本能直接按名字跑；脚本里的 %getZz% 会换成选中内容", "A bare file name works too: scripts in Plugins\\ahkv1 / ahkv2 run by name; %getZz% inside a script becomes the selection");
        m.insert("「运行脚本」动作用的解释器。放进 Plugins\\ahkv1 / ahkv2 就自动认；.ahk 也放这儿，菜单里只写文件名就行", "Interpreter for \"Run script\" actions. Drop it into Plugins\\ahkv1 / ahkv2 and it is found automatically; put .ahk files there too and a bare file name is enough");
        m.insert(r"Everything.exe 的完整路径；留空自动找", r"Full path to Everything.exe; leave empty to auto-detect");
        m.insert("没运行时随本程序启动", "Start it together with this app when it is not running");
        m.insert("勾上以后，本程序启动时（以及查询发现它没在跑时）顺手把 Everything 拉起来；路径就是上面那个，留空会自动找", "When on, this app launches Everything on startup (and when a query finds it is not running). Path is the one above; empty means auto-detect");
        m.insert("扫描程序目录", "Scan program folder");
        m.insert("程序路径", "Program path");
        m.insert("已接上程序目录里的 Everything", "Linked to Everything in the program folder");
        m.insert("程序目录下没有 Everything 文件夹：把整个文件夹改名成 Everything 放进去再点这里", "No Everything folder next to the program: rename the folder to Everything, drop it there and click again");
        m.insert("把 Everything 整个文件夹放到程序目录、或在下面填 Everything.exe 的路径就能连上（DLL / es.exe 从同一个目录里找）；没有它也不影响使用。", "Drop the whole Everything folder next to the program, or fill in the path to Everything.exe below (the DLL / es.exe are looked up in the same folder). Without it everything still works.");
        m.insert("找到 Everything.exe，但还缺 Everything64.dll / es.exe（放在同一个目录里就行）", "Found Everything.exe but the Everything64.dll / es.exe is missing (put it in the same folder)");
        // 一键直达 / 生效条件（第 4.8、4.9 节）
        m.insert("一键直达", "Magic");
        m.insert("选中内容 + 一个快捷键 → 按规则表从上往下问，第一条命中就直接跑它，不弹菜单", "Selected text + one hotkey: rules are tried top to bottom and the first hit runs right away, without opening the menu");
        m.insert("启用一键直达", "Enable Magic");
        m.insert("关掉之后这颗快捷键不注册，选中内容也不会被动", "When off, the hotkey is not registered and selected text is left alone");
        m.insert("没命中时照常弹菜单", "Fall back to the menu");
        m.insert("规则一条都不命中 → 弹普通菜单（关掉 = 什么都不做，只当直达用）", "No rule matched - open the normal menu (off = do nothing, use it as direct-only)");
        m.insert("规则表（从上往下）", "Rules (top to bottom)");
        m.insert("顺序就是优先级；点类型胶囊换一种比法", "Order is priority; click the kind chip to try another way");
        m.insert("加一条规则", "Add rule");
        m.insert("规则表里的一行", "A rule row");
        m.insert("试一试", "Try it");
        m.insert("拿一段文字走一遍匹配，不动设置", "Run one sample through the rules - settings are untouched");
        m.insert("能玩的花样", "More ideas");
        m.insert("不用写正则", "No regex needed");
        m.insert("绑到子菜单", "Bind to a submenu");
        m.insert("多条兜底", "Catch-all last");
        m.insert("会参与的", "Active rules");
        m.insert("没找到这个菜单项", "No such menu item");
        m.insert("命中就跑它", "runs this");
        m.insert("还没填：写菜单项的名字", "empty: type a menu item name");
        m.insert("菜单项的名字，例如 查订单", "Menu item name, e.g. Check order");
        m.insert("粘贴一段文字，看看它命中哪条规则", "Paste some text to see which rule hits");
        m.insert("生效条件", "Conditions");
        m.insert("加条件", "Add");
        m.insert("怎么写", "How to write");
        m.insert("现在的前台窗口", "Foreground window");
        m.insert("刷新", "Refresh");
        m.insert("应用", "Apply");
        m.insert("清空", "Clear");
        m.insert("插入", "Insert");
        m.insert("这条快捷键只在满足条件的窗口里生效", "This hotkey only fires in matching windows");
        m.insert("不满足条件时不吞键：那个组合在别的程序里还是它原来的意思", "When the conditions do not match the key is not swallowed - it keeps its normal meaning elsewhere");
        m.insert("还没写条件 —— 这条快捷键在所有程序里都生效", "No conditions yet - this hotkey works in every app");
        m.insert("条件为空 = 到处都生效", "Empty = everywhere");
        m.insert("有行写错了：这些行不会被触发（改对了才生效）", "Some lines are invalid and will never fire (fix them to enable)");
        m.insert("当前窗口满足条件 → 这个键在这儿会触发", "Current window matches - the key fires here");
        m.insert("当前窗口不满足条件 → 这个键在这儿还是原来的意思", "Current window does not match - the key keeps its normal meaning here");
        m.insert("怎么比", "How it matches");
        m.insert("匹配内容", "What to match");
        m.insert("例子", "Example");
        m.insert("备注", "Note");
        m.insert("保存", "Save");
        m.insert("改", "Edit");
        m.insert("建", "Create");
        m.insert("算了", "Never mind");
        m.insert("子菜单", "Submenu");
        m.insert("菜单项", "Item");
        m.insert("子菜单（摊开）", "Submenu (expands)");
        m.insert("菜单项（直接执行）", "Item (runs)");
        m.insert("子菜单（命中时摊开这一支）", "Submenu (opens just this branch)");
        m.insert("菜单项（命中时直接执行）", "Item (runs directly)");
        m.insert("✎ 配动作", "✎ Actions");
        m.insert("＋ 加规则", "＋ Add rule");
        m.insert("＋ 新建直达项", "＋ New direct item");
        m.insert("＋ 新建直达子菜单", "＋ New direct submenu");
        m.insert("未命名的直达项", "Unnamed direct item");
        m.insert("给自己看的备注（显示在规则行上）", "Note for yourself (shown on the rule row)");
        m.insert("起个名字，例如：用浏览器打开", "Give it a name, e.g. Open in browser");
        m.insert("先选一个「命中之后跑」的直达项", "Pick a direct item to run first");
        m.insert("这种比法要填表达式：一行一条，写一条也行", "This kind needs a pattern: one per line, one is enough");
        m.insert("只要选中内容不是空的就算命中", "Any non-empty selection matches");
        m.insert("看形状：http(s):// 、ftp:// 、www. 开头", "By shape: starts with http(s):// , ftp:// or www.");
        m.insert("看形状：#abc / #aabbcc / rgb(...)", "By shape: #abc / #aabbcc / rgb(...)");
        m.insert("能被解析成 JSON 就算命中", "Matches when it parses as JSON");
        m.insert("这个路径存在、而且是文件", "The path exists and is a file");
        m.insert("这个路径存在、而且是文件夹", "The path exists and is a folder");
        m.insert("看表达式：一行一条，每行生成一条规则", "By pattern: one per line, each line becomes a rule");
        m.insert("看形状，不用填表达式", "By shape, no pattern needed");
        m.insert("命中之后跑", "Then run");
        m.insert("直达项不在菜单里露头，只为规则存在；「✎ 配动作」配它到底干什么", "Direct items never show up in the menu - they exist for rules only. Use ✎ Actions to say what they do.");
        m.insert("还没有直达项。点下面的「+ 新建」，它不在菜单里出现，只给规则用", "No direct items yet. Use + New below: they never appear in the menu, only rules use them.");
        m.insert("选中内容跟这条规则对上了，就直接跑它绑的直达项，不弹菜单", "When the selection matches this rule, its direct item runs right away - no menu.");
        m.insert("规则表从上往下问，第一条命中的就执行 —— 宽泛的规则（比如「非空就行」）要放最后", "Rules are asked top-down and the first hit wins - keep broad ones (like any non-empty) last.");
        m.insert("一行一条：每行都是同一个类型、同一个目标的一条规则；改一条时多出来的行排它后面", "One per line: each line becomes a rule with the same kind and target. When editing, extra lines go after the one you opened.");
        m.insert("这种比法不用填表达式（表达式留着也不会被用）", "This kind needs no pattern (leftovers are ignored).");
        m.insert("想写一批：一行一条，保存时一次全加上（顺序就是它们的优先级）", "Write a batch: one per line, all added on save (that order is their priority).");
        m.insert("还没有规则。点「＋ 加规则」：类型从下拉里挑一种，表达式一行一条，目标是下面那些直达项", "No rules yet. Use ＋ Add rule: pick a kind from the list, one pattern per line, and target one of the direct items below.");
        m.insert("在一个窗口里写：类型下拉选、表达式一行一条、一次能加好几条 —— 从上往下问，第一条命中的就执行", "Write it in one window: kind from a list, one pattern per line, several rules at once - first hit wins.");
        m.insert("还没有直达项。它就是「命中之后要干什么」—— 新建之后开动作编辑器配（运行程序 / 发送按键 / 打开网址都行），配好再让规则指着它", "No direct items yet. That is the \"what happens on a hit\": create one, configure its actions (run a program / send keys / open a URL), then point a rule at it.");
        m.insert("建完直接开动作编辑器；子菜单适合「命中后摊开给人挑」（比如一组截图工具）", "Creating one opens the action editor; a submenu is good when a hit should offer a choice (a set of screenshot tools, say).");
        m.insert("直达项（不在菜单里出现）", "Direct items (never in the menu)");
        m.insert("规则命中之后跑的就是它们：改名字、配动作都在这儿", "What rules run on a hit: rename and configure them here");
        m.insert("裸文件名交给 Everything 查", "Bare file names are resolved by Everything");
        m.insert("没连上 Everything：只有 PATH 里的程序和写了完整路径的能跑", "Everything is not connected: only programs on PATH or full paths will run");
        m.insert("刚刚连续失败，先冷却一会儿再问", "Just failed a couple of times - cooling down before retrying");
        m.insert("已连接：还没查过东西，第一次用的时候会慢一下", "Connected: nothing queried yet, the first lookup may take a moment");
        m.insert("不再自己扫盘：装了 Everything（或把 es.exe 放进程序目录）就能全盘解析；挪过位置的文件会当场作废并重新索引", "No local scanning any more: install Everything (or drop es.exe next to the program) to resolve anything on the disk. Files that moved are rejected on the spot and re-indexed.");
        m.insert("留空 = 跟「弹出菜单」共用同一颗键：按下先逐条问规则，一条都不中再照常弹菜单。想单独配一颗再填这里", "Empty = share the popup menu hotkey: rules are asked first, and the menu pops when nothing matches. Fill this in to use a separate key.");
        m.insert("现在跟「弹出菜单」共用同一颗；单独一颗会更好记，但也更容易和别的键撞", "Currently sharing the popup hotkey; a separate key is easier to remember but easier to collide with.");
        m.insert("这条局部快捷键只在满足条件的窗口里生效", "This local hotkey only fires in matching windows");
        m.insert("◎ 十字", "◎ Crosshair");
        m.insert("松开即取…", "Release to pick…");
        m.insert("还没读到别的窗口：按住上面的「◎ 十字」拖到目标程序上松手，或者去目标程序里点一下再回来点「刷新」", "Nothing read yet: drag the ◎ Crosshair above onto the target window and release, or click that window and come back to Refresh");
        m.insert("「◎ 十字」按住拖到任意窗口上松手，取的就是那一扇；什么都不拖时，显示的是「最近一个别的程序的窗口」", "Drag ◎ Crosshair onto any window and release to read that one; otherwise this shows the last foreign foreground window");
        m.insert("写完整路径", "Full path");
        m.insert("只填文件名（跟着 PATH 找）", "File name only (found via PATH)");
        m.insert("把选中内容加到这一项后面…", "Add the selection after this item…");
        m.insert("把选中内容加到这一项后面", "Add the selection after this item");
        m.insert("选中内容", "Selection");
        m
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_is_translated() {
        for (zh, en) in table().iter() {
            assert!(!en.trim().is_empty(), "{zh} 的译文是空的");
            assert_ne!(zh, en, "{zh} 的译文跟原文一样");
        }
    }

    #[test]
    fn unknown_text_falls_back_to_itself() {
        assert_eq!(english("这条肯定没收录"), "这条肯定没收录");
    }
}
