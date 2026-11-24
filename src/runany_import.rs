//! 导入 RunAny 原版的菜单配置（`RunAny.ini`）。
//!
//! ## 原版的格式
//!
//! RunAny（AutoHotkey 那个）的 `RunAny.ini` 不是 ini，是一棵**缩进 + 前缀**混着写的树：
//!
//! ```text
//! -常用(&1)                     ← 一个连字符 = 第 1 层子菜单
//!     我的电脑(&Z)|explorer.exe    ← 名称|命令
//!     QQ	<!w|QQ.exe             ← 名称<Tab>热键|命令
//!     ||                         ← 单独一行竖线 = 另起一列
//! -工具(&2)
//!     --截图(&J)                 ← 两个连字符 = 第 2 层
//!         QQ截图	<^1|^!a::      ← 命令以 `::` 结尾 = 发送按键
//!         --
//! ```
//!
//! 层级只认**连字符的个数**（原版 `Menu_Read` 里 `menuLevel := StrLen(treeLevel)`），
//! 行首那串 Tab 是给人看的，解析时先按 AutoTrim 去掉。`;` 开头的整行是注释，
//! 原版直接 `continue`，这里一样跳过。
//!
//! ## 功能类型怎么对应
//!
//! 原版靠 `Get_Menu_Item_Mode` 看**命令结尾 / 开头**来判断这一项是什么，
//! 判断顺序不能乱（`;;` 必须先于 `;`、`:::` 必须先于 `::`）：
//!
//! | 原版写法 | 含义 | 本软件的动作 |
//! | --- | --- | --- |
//! | `文字;` | 粘贴输出这段文字 | 发送文本 |
//! | `文字;;` | 键盘输出这段文字 | 发送文本 |
//! | `^!a::` | 发送这串按键 | 发送按键 |
//! | `^!a:::` | 用 AHK 语法发送按键 | 发送按键 |
//! | `https://…` / `www.…` | 打开网页 | 打开网址 |
//! | `插件[函数](参数)` | 调用 RunAny 插件 | 原样留在「运行程序」里，加备注 |
//! | 其它 | 启动程序 / 打开文件夹 | 运行程序 |
//!
//! ## 名字里的小东西
//!
//! - `名称\t热键`：Tab 后面是这一项的触发快捷键（AHK 语法），转成本软件的
//!   `Ctrl+Alt+1` 那种写法；转不出来的（比如 `~F1`、`$^a`）就只在备注里留原文。
//! - `名称(&Z)`：`(&Z)` 是菜单助记符，转成图标位后面的加速键，名字里去掉这段。

use crate::hotkey;
use crate::settings::{ActionKind, ActionStep, ItemKind, ItemSetting};

/// 导入结果 + 几个数字（给界面回一句「导入了多少」）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// 普通菜单项（不含子菜单、分隔线）
    pub items: usize,
    /// 子菜单
    pub submenus: usize,
    /// 带触发快捷键的项
    pub hotkeys: usize,
    /// 有备注的项（插件函数这类没法一一对应的）
    pub notes: usize,
    /// 跳过的行（认不出来的写法）
    pub skipped: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Imported {
    pub items: Vec<ItemSetting>,
    pub stats: Stats,
}

/// 组树用的场地：`items[i]` 是节点本身，`kids[i]` 是它的子节点下标。
///
/// 以前是「先拍平成 (层级, 节点)，再递归组树」，但原版的层级不是树形下标 ——
/// 它是 `menuRootFn[层级]` 这种**查表**，一个单独一行的 `--` 会把「后面内容写进哪一层」
/// 重置到那一层。用下标递归根本表达不了往回跳，于是子菜单后面的项全掉进了上一个
/// 子菜单里（截图里红框那批「桌面壁纸 / 电流鼠标坐标」就是这么跑到「随机字符生成」里的）。
struct Arena {
    items: Vec<ItemSetting>,
    kids: Vec<Vec<usize>>,
}

impl Arena {
    fn new() -> Self {
        // 0 号是根容器，不画出来，只当所有顶层项的父节点
        Self {
            items: vec![ItemSetting::separator()],
            kids: vec![Vec::new()],
        }
    }

    fn push(&mut self, parent: usize, item: ItemSetting) -> usize {
        self.items.push(item);
        self.kids.push(Vec::new());
        let index = self.items.len() - 1;
        self.kids[parent].push(index);
        index
    }

    fn take(&mut self, index: usize) -> ItemSetting {
        let kids = std::mem::take(&mut self.kids[index]);
        let mut item = self.items[index].clone();
        item.children = kids.into_iter().map(|kid| self.take(kid)).collect();
        item
    }
}

/// 解析整份 `RunAny.ini`。
///
/// 结构照抄原版 `Menu_Read`：它维护一个 `menuRootFn[层级]`（第 N 层内容该写进哪个
/// 菜单），然后
///
/// * `-名称`（D 个连字符）：挂到 `menuRootFn[D]` 下，并让 `menuRootFn[D+1]` 指向它；
/// * **单独一行的 `--` / `---`（没有名称）**：往 `menuRootFn[D]` 里加一条分隔线，
///   并且**把内容层重置到 D** —— 这是原版最容易被漏掉的一条，也是「子菜单后面
///   的项跑到别的子菜单」的根源。作者用它表示「这一段结束了，回到上一级」；
/// * 内容行：写进当前层，跟行首 Tab 一个都无关（原版压根不看缩进）。
///
/// `|` / `||` 不是自己占一行：原版存进 `MenuBar`，**作用于下一个 add**，所以这里
/// 先记着，等下一个节点出现时插在它前面（父节点是那个节点的父）。
pub fn parse(text: &str) -> Imported {
    let mut out = Imported::default();
    let mut arena = Arena::new();
    // menu_at[层级] = 这一层内容该写进哪个节点。`menu_at[1]` 是根。
    let mut menu_at: Vec<usize> = vec![0, 0];
    // 内容行现在写进谁家
    let mut current = 0usize;
    let mut counter = 0usize;
    let mut pending_bar = false;

    for raw in text.lines() {
        let line = raw.trim_matches([' ', '\t', '\u{feff}']);
        if line.is_empty() || line.starts_with(';') {
            continue;
        }

        // ---- 结构行：`-` 开头 ----
        if line.starts_with('-') {
            let dashes = line.chars().take_while(|&ch| ch == '-').count();
            let rest = line[dashes..].trim();
            let level = dashes.max(1);
            let parent = level_node(&mut menu_at, level);

            if rest.is_empty() {
                // 纯连字符：分隔线，而且「后面的内容回到这一层」
                if pending_bar {
                    arena.push(parent, column_break());
                    pending_bar = false;
                }
                arena.push(parent, separator());
                menu_at.truncate(level + 1);
                current = parent;
                continue;
            }

            // 子菜单行也能带 `|关联`：`-文件(&G)|public file` 说的是「选中文件时
            // 才显示这一支」，不是名字的一部分。
            let (name_with_key, assoc) = match rest.split_once('|') {
                Some((name, assoc)) => (name.trim(), Some(assoc)),
                None => (rest, None),
            };
            // 子菜单名后面还能跟一个 Tab 热键：`-键谱	NumpadDiv` 是「按这个键
            // 直接把这一支弹出来」。
            let (name, key) = match name_with_key.split_once('\t') {
                Some((name, key)) => (name.trim(), key.trim()),
                None => (name_with_key, ""),
            };
            if pending_bar {
                arena.push(parent, column_break());
                pending_bar = false;
            }
            let (label, accel) = split_accel(name);
            let mut menu = ItemSetting::item("ra-menu".to_string(), label);
            menu.kind = ItemKind::Submenu;
            menu.accel = accel;
            if !key.is_empty() {
                menu.shortcut = key.to_string();
                menu.note = format!("原版按 {key} 直接呼出这一支");
                out.stats.notes += 1;
            }
            if let Some(assoc) = assoc {
                apply_assoc(&mut menu, name, assoc, &mut out.stats);
            }
            let index = arena.push(parent, menu);
            menu_at.truncate(level + 1);
            menu_at.push(index);
            current = index;
            out.stats.submenus += 1;
            continue;
        }

        // ---- 单独一行 `|` / `||`：下一个节点另起一列 ----
        if line == "|" || line == "||" {
            pending_bar = true;
            continue;
        }

        // ---- 内容行：`名称[Tab热键]|命令` ----
        let (name_part, command) = match line.split_once('|') {
            Some((name, command)) => (name.trim(), command.trim()),
            None => (line.trim(), line.trim()),
        };
        if command.is_empty() {
            out.stats.skipped += 1;
            continue;
        }
        let (raw_label, raw_hotkey) = match name_part.split_once('\t') {
            Some((label, key)) => (label.trim(), key.trim()),
            None => (name_part, ""),
        };
        let label = if raw_label.is_empty() {
            label_from_command(command)
        } else {
            raw_label.to_string()
        };
        let (label, accel) = split_accel(&label);

        if pending_bar {
            arena.push(current, column_break());
            pending_bar = false;
        }
        counter += 1;
        let mut item = ItemSetting::item(format!("ra{counter}"), label);
        item.accel = accel;

        // 快捷键：能转就转，转不出来只留个提示（不硬塞一个假的键）
        if !raw_hotkey.is_empty() {
            match ahk_hotkey(raw_hotkey) {
                Some(key) if hotkey::is_valid(&key) => {
                    item.hotkey = key.clone();
                    item.shortcut = key;
                    out.stats.hotkeys += 1;
                }
                _ => {
                    // 认不出来的键**不往「快捷键提示」里塞**：那一栏是给用户看
                    // 「这一项绑了什么键」的，塞一个按不动的字符串会误导人。
                    // 原样留在备注里，想用的人自己去设。
                    item.note = format!("原快捷键 {raw_hotkey}（未识别，没绑定）");
                    out.stats.notes += 1;
                }
            }
        }

        // 功能类型
        let plugin = is_plugin_call(command);
        if plugin {
            // 插件调用在 `classify` 里已经落到「发送文本」，这里只补备注和图标开关
            item.note = if item.note.is_empty() {
                PLUGIN_NOTE.to_string()
            } else {
                format!("{}；{PLUGIN_NOTE}", item.note)
            };
            // 插件函数不是磁盘上的程序，别让它去「程序搜索 / Everything」里找图标。
            item.auto_icon = Some(false);
            out.stats.notes += 1;
        }
        item.actions = vec![action_of(command)];
        arena.push(current, item);
        out.stats.items += 1;
    }

    let roots = std::mem::take(&mut arena.kids[0]);
    let mut items: Vec<ItemSetting> = roots.into_iter().map(|kid| arena.take(kid)).collect();
    // 空子菜单留着没用（原版也不显示）
    items = drop_empty_menus(items);
    dedupe_hotkeys(&mut items, &mut out.stats);
    out.items = items;
    out
}

/// `menu_at[level]`：这一层的内容该写进哪个节点。
///
/// 层级跳跃（`-工具` 后面直接跟 `----xxx`）时补一个最近的上级 —— 原版会拿那个
/// 层级上**残留的**旧菜单，宁可挂近处也不要挂到别的分支里去。
fn level_node(menu_at: &mut Vec<usize>, level: usize) -> usize {
    while menu_at.len() <= level {
        let last = *menu_at.last().unwrap_or(&0);
        menu_at.push(last);
    }
    menu_at[level]
}

/// 同一个快捷键在原版里可能绑了好几项（原版自己就是「后写的覆盖先写的」）。
/// 本软件一个键只能注册一次，重复的那几条留着会让「应用」时蹦出一串冲突提示 ——
/// 这里保留第一条，后面的清空快捷键并写一条备注，用户能在列表里一眼看见。
fn dedupe_hotkeys(items: &mut [ItemSetting], stats: &mut Stats) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    fn walk(items: &mut [ItemSetting], seen: &mut std::collections::HashSet<String>, stats: &mut Stats) {
        for item in items {
            if !item.hotkey.is_empty() {
                if !seen.insert(item.hotkey.clone()) {
                    let key = item.hotkey.clone();
                    item.hotkey.clear();
                    item.shortcut = key.clone();
                    item.note = if item.note.is_empty() {
                        format!("快捷键 {key} 和前面的项重复，已停用")
                    } else {
                        format!("{}；快捷键 {key} 重复，已停用", item.note)
                    };
                    stats.notes += 1;
                }
            }
            walk(&mut item.children, seen, stats);
        }
    }
    walk(items, &mut seen, stats);
}

/// 去掉没有任何子项的子菜单（递归到底）。
fn drop_empty_menus(items: Vec<ItemSetting>) -> Vec<ItemSetting> {
    items
        .into_iter()
        .filter_map(|mut item| {
            if item.kind == ItemKind::Submenu {
                item.children = drop_empty_menus(std::mem::take(&mut item.children));
                if item.children.is_empty() {
                    return None;
                }
            }
            Some(item)
        })
        .collect()
}

fn separator() -> ItemSetting {
    ItemSetting::separator()
}

/// 一个「多列符」条目（`|` / `||` 落到下一个节点前面）。
fn column_break() -> ItemSetting {
    let mut item = ItemSetting::separator();
    item.column_break = true;
    item
}

/// 省略了名称的菜单项（`|notepad.exe`）：拿命令里那个程序的文件名当名字。
fn label_from_command(command: &str) -> String {
    let token = command.trim().trim_start_matches('"');
    let token = token.split(['"', ' ']).next().unwrap_or(token);
    let name = token.rsplit(['\\', '/']).next().unwrap_or(token);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    if name.is_empty() {
        command.trim().to_string()
    } else {
        name.to_string()
    }
}

/// `常用(&1)` → (`常用`, `1`)；`[双alt]猫记` → 原样、没有加速键。
fn split_accel(label: &str) -> (String, String) {
    // 只认结尾的 `(&X)`：原版就是靠它做菜单助记符
    if let Some(start) = label.rfind("(&") {
        if label.ends_with(')') {
            let inner = &label[start + 2..label.len() - 1];
            // 助记符只认单字符（本软件的加速键就是一个字）；`(&10)` 这种两位数字
            // 表达不了，但名字里那截还是要摘掉，不然列表里会露出一串 `(&10)`
            if !inner.is_empty() && inner.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                let mut name = label[..start].to_string();
                while name.ends_with(' ') {
                    name.pop();
                }
                let accel = if inner.chars().count() == 1 {
                    inner.to_string()
                } else {
                    String::new()
                };
                return (name, accel);
            }
        }
    }
    (label.to_string(), String::new())
}

/// 子菜单后面的 `|public file` / `|mp4 mkv` 是原版的「关联」：选中这类内容时，
/// 原版会在光标旁**额外插入**这一支菜单，主菜单本身一项不少。
///
/// 本软件的「按选中内容筛选」是**替换式**的（有选中内容就只留匹配项），
/// 语义不一样：照搬成 `contexts` 会让菜单从十几支缩成一两支，用户看到的就是
/// 「导入后菜单项全不见了」。所以这里只把关联写成一条短备注，不动 `contexts`，
/// 菜单结构和原版主菜单保持一致；真想要筛选的人可以在编辑窗口里自己勾。
fn apply_assoc(menu: &mut ItemSetting, name: &str, assoc: &str, stats: &mut Stats) {
    use crate::selection::SelectionKind;
    use crate::settings::ContextMode;

    let bare_name = split_accel(name).0;
    let mut kinds: Vec<SelectionKind> = Vec::new();
    let mut extensions: Vec<String> = Vec::new();
    let mut extra: Vec<String> = Vec::new();

    for token in assoc.split_whitespace() {
        // `-软件(&8)|软件(&8)`：原版里这是「关联就是自己」的占位写法，等于没关联
        if token == name || split_accel(token).0 == bare_name {
            continue;
        }
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            // `public` = 原版「这条跟选中内容有关」。用户要求**不含文字**：
            // 只勾 文件 / 文件夹 / 图片（文字那类会跟划词菜单抢，太吵）
            "public" => {
                kinds = vec![
                    SelectionKind::File,
                    SelectionKind::Folder,
                    SelectionKind::Image,
                ]
            }
            "text" => {
                if !kinds.contains(&SelectionKind::Text) {
                    kinds.push(SelectionKind::Text);
                }
            }
            // `file` 本身不是后缀，只是「后面这些是文件后缀」的分隔词
            "file" => {
                if !kinds.contains(&SelectionKind::File) {
                    kinds.push(SelectionKind::File);
                }
            }
            // 按窗口过滤（`xxx.exe` / `#32770.class`）：本软件没有这个概念
            _ if lower.ends_with(".class") || lower.ends_with(".exe") => {}
            _ if is_extension(&lower) => extensions.push(lower),
            _ => extra.push(token.to_string()),
        }
    }

    if !extensions.is_empty() {
        if !kinds.contains(&SelectionKind::File) {
            kinds.push(SelectionKind::File);
        }
        // 后缀表用竖线隔开，和界面上那次「清除适用选中内容」用同一套格式
        menu.file_exts = extensions.join("|");
    }
    if !kinds.is_empty() {
        // 原版是按选中内容切换的菜单：只有这里是真的「只在这些情况下出现」
        menu.contexts = kinds.clone();
        menu.context_mode = ContextMode::Own;
    }

    let mut parts: Vec<String> = Vec::new();
    if !menu.file_exts.is_empty() {
        parts.push(format!("仅 {} 文件", menu.file_exts));
    }
    if !kinds.is_empty() && menu.file_exts.is_empty() {
        parts.push("按选中内容显示".to_string());
    }
    if !extra.is_empty() {
        parts.push(format!("原版关联：{}", extra.join(" ")));
    }
    if parts.is_empty() {
        return;
    }
    let note = parts.join(" · ");
    menu.note = if menu.note.is_empty() {
        note
    } else {
        format!("{}；{note}", menu.note)
    };
    stats.notes += 1;
}

/// 看起来像文件后缀的 token：纯字母数字、不含点、长度合理。
///
/// `xlsx` `ahk2` `txt` 都算；`(&3)`、`public`、`软件` 这些不算。
fn is_extension(token: &str) -> bool {
    if token.is_empty() || token.len() > 10 {
        return false;
    }
    if !token.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return false;
    }
    // 纯数字是加速键残留（`(&3)` 里那个 3），不是后缀
    token.chars().any(|ch| ch.is_ascii_alphabetic())
}

/// 原版插件调用导入后挂的备注（提醒功能还没接上）。
const PLUGIN_NOTE: &str =
    "原版插件调用：已放进内置动作「执行原版插件功能」的参数里，该动作还没实现，先原样存着";

/// 一眼看出这是「RunAny 插件函数」：`XiaoYao_plus[hotkeyStatus](...)`。
fn is_plugin_call(command: &str) -> bool {
    match (command.find('['), command.find('(')) {
        (Some(bracket), Some(paren)) => {
            bracket < paren && command[..bracket].chars().any(|ch| ch.is_alphabetic())
        }
        _ => false,
    }
}

/// 命令 → (动作类型, 参数)。
///
/// 判断顺序照抄原版 `Get_Menu_Item_Mode`（`;;` 先于 `;`、`:::` 先于 `::`），
/// 顺序错了会把「发一段带冒号的文字」认成「发按键」。
/// 动作库里「执行原版插件功能」那一条的 id（见 `crate::actions::BUILTINS`）。
const PLUGIN_ACTION: &str = "runany-plugin";

/// 一条原版命令 → 我们要存的动作。
///
/// 插件调用（`名字[函数](参数)`）不是程序、也不该是「发一段文字」，它是动作库里
/// 那一条内置动作，整串调用原样放进参数 `call` 里 —— 一个字符都不改
/// （函数名里的 `[` `]`、参数里的 `%` 都是它自己的语法）。
fn action_of(command: &str) -> ActionStep {
    if is_plugin_call(command) {
        let mut params = std::collections::BTreeMap::new();
        params.insert("call".to_string(), command.to_string());
        return ActionStep::with_params(ActionKind::Builtin, PLUGIN_ACTION, params);
    }
    let (kind, value) = classify(command);
    let mut step = ActionStep::new(kind, value);
    if kind == ActionKind::Text {
        // 原版：`文字;` 走「剪贴板粘贴」，`文字;;` 走「直接逐字发送」
        step.text_paste = Some(!command.ends_with(";;"));
    }
    step
}

fn classify(command: &str) -> (ActionKind, String) {
    // 短语：`文字;` / `文字;;`（结尾的分号本身不是内容）
    if command.len() > 2 && command.ends_with(";;") {
        return (ActionKind::Text, command[..command.len() - 2].to_string());
    }
    if command.len() > 1 && command.ends_with(';') {
        return (ActionKind::Text, command[..command.len() - 1].to_string());
    }
    // 按键：`^!a::` / `^!a:::`
    if command.len() > 3 && command.ends_with(":::") {
        let keys = ahk_keys(&command[..command.len() - 3]);
        return (ActionKind::Keys, keys);
    }
    if command.len() > 2 && command.ends_with("::") {
        let keys = ahk_keys(&command[..command.len() - 2]);
        return (ActionKind::Keys, keys);
    }
    // 网页
    let lower = command.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("www.")
        || lower.starts_with("ftp://")
    {
        return (ActionKind::Url, command.to_string());
    }
    // `xxx.exe https://…`：用指定浏览器打开网页
    if let Some((program, url)) = command.split_once(' ') {
        let p = program.to_ascii_lowercase();
        let u = url.trim().to_ascii_lowercase();
        if p.ends_with(".exe") && (u.starts_with("http") || u.starts_with("www.")) {
            return (ActionKind::Url, url.trim().to_string());
        }
    }
    // 原版插件调用（`XiaoYao_plus[hotkeyStatus](...)`）不是磁盘上的程序：
    // 放进「运行程序」会被当成程序名去解析（找不到 → 报错），还会去 Everything 里
    // 翻图标。原样落到「发送文本」里 —— 内容一个字不丢，也不会被误当成程序。
    if is_plugin_call(command) {
        return (ActionKind::Text, command.to_string());
    }
    (ActionKind::Run, command.to_string())
}

/// AHK 的按键写法 → 本软件的写法：`^!a` → `Ctrl+Alt+A`，多组用空格隔开。
///
/// 认不出来就原样返回（用户能在编辑窗口里手动改）。
fn ahk_keys(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for chunk in text.split_whitespace() {
        match ahk_hotkey(chunk) {
            Some(converted) => parts.push(converted),
            None => return text.trim().to_string(),
        }
    }
    if parts.is_empty() {
        text.trim().to_string()
    } else {
        parts.join(" ")
    }
}

/// 一个 AHK 组合键 → `Ctrl+Alt+A`。
///
/// AHK 的修饰符：`^` Ctrl、`!` Alt、`+` Shift、`#` Win；`<` / `>` 只是「左右」，
/// 本软件不区分左右，直接忽略。键名可以用 `{}` 包起来（`{F11}`、`{Enter}`）。
pub fn ahk_hotkey(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut win = false;
    let mut rest = text;
    loop {
        let mut chars = rest.chars();
        match chars.next() {
            Some('^') => ctrl = true,
            Some('!') => alt = true,
            Some('+') => shift = true,
            Some('#') => win = true,
            // 左右标记只影响修饰键，本软件不区分，跳过（`<^1` 就是 Ctrl+1）
            Some('<') | Some('>') => {}
            // `~` `$` `*` 这些选项在本软件里没有对应，跳过
            Some('~') | Some('$') | Some('*') => {}
            _ => break,
        }
        rest = &rest[1..];
    }
    let key = rest.trim().trim_matches(|ch| ch == '{' || ch == '}');
    if key.is_empty() {
        return None;
    }
    // 本软件要求至少带一个修饰键（不然等于抢走所有程序里的这个键）
    if !(ctrl || alt || shift || win) {
        return None;
    }
    let key = key_name(key)?;
    let mut parts: Vec<&str> = Vec::new();
    if ctrl {
        parts.push("Ctrl");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    if win {
        parts.push("Super");
    }
    parts.push(&key);
    Some(parts.join("+"))
}

/// AHK 的键名 → `global-hotkey` 认的名字（和 `hotkey::key_token` 那一套对齐）。
fn key_name(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    let name = match lower.as_str() {
        "enter" | "return" => "Enter",
        "esc" | "escape" => "Escape",
        "space" => "Space",
        "tab" => "Tab",
        "backspace" | "bs" => "Backspace",
        "delete" | "del" => "Delete",
        "insert" | "ins" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pgup" => "PageUp",
        "pgdn" => "PageDown",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "capslock" => "CapsLock",
        "numlock" => "NumLock",
        "scrolllock" => "ScrollLock",
        "printscreen" => "PrintScreen",
        "pause" | "break" => "Pause",
        "comma" => "Comma",
        "period" => "Period",
        "slash" => "Slash",
        "semicolon" => "Semicolon",
        "quote" => "Quote",
        "bracketleft" => "BracketLeft",
        "bracketright" => "BracketRight",
        "backslash" => "Backslash",
        "minus" => "Minus",
        "equal" => "Equal",
        "backquote" => "Backquote",
        _ => {
            // F1~F24、单个字母、单个数字原样往上交
            if let Some(number) = lower.strip_prefix('f') {
                if let Ok(n) = number.parse::<u8>() {
                    if (1..=24).contains(&n) {
                        return Some(format!("F{n}"));
                    }
                }
            }
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) if ch.is_ascii_alphanumeric() => {
                    return Some(ch.to_ascii_uppercase().to_string())
                }
                _ => return None,
            }
        }
    };
    Some(name.to_string())
}

/// 读一个 `RunAny.ini` 文件。
///
/// 编码按「UTF-8（带不带 BOM 都行）→ 本机 ANSI（GBK）」两步走：原版脚本用
/// AutoHotkey 的默认 `FileEncoding`，国内用户的这份文件十有八九是 UTF-8 BOM，
/// 但也有人存成 ANSI，直接 `read_to_string` 会整份失败。
pub fn read_ini(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("{path}: {err}"))?;
    if let Ok(text) = String::from_utf8(bytes.clone()) {
        return Ok(text.trim_start_matches('\u{feff}').to_string());
    }
    #[cfg(windows)]
    {
        if let Some(text) = crate::win32::decode_ansi(&bytes) {
            return Ok(text.trim_start_matches('\u{feff}').to_string());
        }
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// 解析 + 建一份新配置 + 立刻切过去。
///
/// 为什么单独建一份配置而不是塞进当前菜单：原版一份 `RunAny.ini` 动辄几百项，
/// 混进用户自己那份里既难找也难删；独立成 `menus/<名字>.runa` 之后，
/// 不满意在「配置」页删掉就行，原菜单一点没动。
pub fn import_text(
    cx: &mut gpui::App,
    text: &str,
    name_hint: &str,
) -> Result<(String, Stats), String> {
    let built = build_profile(text, name_hint)?;
    let (name, stats) = (built.name.clone(), built.stats);
    crate::settings::update(cx, |settings| {
        settings.adopt_profile_items(built.items, built.next_id);
        // 导入是「用户当下就要用」，不走「预览等你点应用」那一套
        settings.menu_profile = built.name;
        settings.pending_profile = None;
    });
    Ok((name, stats))
}

/// 一份「已经写好文件、还没装进当前设置」的导入结果。
pub struct Built {
    /// 新配置的名字（`menus/<名字>.runa` 已经写好了）
    pub name: String,
    pub items: Vec<ItemSetting>,
    pub next_id: u32,
    pub stats: Stats,
}

/// 解析 + 重新编号 + 写出一份新的 `.runa`（不碰当前设置）。
///
/// 和 `import_text` 拆开是为了能单独测：写文件这一步不需要 GPUI 的 `App`。
pub fn build_profile(text: &str, name_hint: &str) -> Result<Built, String> {
    let imported = parse(text);
    if imported.items.is_empty() {
        return Err(crate::i18n::t("这份文件里没读到菜单项").to_string());
    }
    let mut items = imported.items;
    let mut next = 1u32;
    renumber(&mut items, &mut next);

    let base = crate::menu_files::sanitize(name_hint).unwrap_or_else(|| "RunAny".to_string());
    let name = crate::menu_files::unique_name(&base);
    crate::menu_files::save(&name, &items, next).map_err(|err| err.to_string())?;
    Ok(Built {
        name,
        items,
        next_id: next,
        stats: imported.stats,
    })
}

/// 给整棵树换一批没人用过的新 id（`custom-1`、`custom-2`…）。
fn renumber(items: &mut [ItemSetting], next: &mut u32) {
    for item in items {
        if item.kind == ItemKind::Separator {
            continue;
        }
        item.id = format!("custom-{}", *next);
        *next += 1;
        renumber(&mut item.children, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ContextMode;

    #[test]
    fn assoc_becomes_contexts_and_extensions() {
        use crate::selection::SelectionKind;
        let text = "-编辑(&3)|xlsx ppt doc xls ini txt ahk file xlsm ahk2\n\t项|a.exe\n\
-划词(&10)|text\n\t项2|b.exe\n-文件(&G)|public\n\t项3|c.exe\n";
        let imported = parse(text);
        let menus: Vec<&ItemSetting> = imported.items.iter().collect();
        assert_eq!(menus.len(), 3);

        // 后缀用竖线隔开，`file` 这种分隔词要剔掉
        assert_eq!(menus[0].file_exts, "xlsx|ppt|doc|xls|ini|txt|ahk|xlsm|ahk2");
        assert!(menus[0].contexts.contains(&SelectionKind::File));

        // `text` → 文字
        assert_eq!(menus[1].contexts, vec![SelectionKind::Text]);

        // `public` → 文件 / 文件夹 / 图片（不含文字）
        assert_eq!(menus[2].contexts.len(), 3);
        assert!(!menus[2].contexts.contains(&SelectionKind::Text));
    }

    #[test]
    fn phrase_semicolons_pick_paste_mode() {
        let text = "账号1|lch319;\n账号2|lch319;;\n";
        let imported = parse(text);
        assert_eq!(imported.items.len(), 2);
        // 一个分号 = 剪贴板粘贴开；两个分号 = 关
        assert_eq!(imported.items[0].actions[0].text_paste, Some(true));
        assert_eq!(imported.items[0].actions[0].value, "lch319");
        assert_eq!(imported.items[1].actions[0].text_paste, Some(false));
        assert_eq!(imported.items[1].actions[0].value, "lch319");
    }

    #[test]
    fn unrecognized_hotkey_only_goes_to_note() {
        let text = "测试\tCtrl+Alt+Q|notepad.exe\n";
        let imported = parse(text);
        assert_eq!(imported.items.len(), 1);
        // 认不出来就不许塞进「快捷键提示」
        assert!(imported.items[0].shortcut.is_empty());
        assert!(imported.items[0].note.contains("Ctrl+Alt+Q"));
    }

    #[test]
    fn a_line_maps_to_a_menu_item_with_its_command() {
        let imported = parse("-常用(&1)\n\t我的电脑(&Z)|explorer.exe\n\t微信|WeChat.exe\n");
        assert_eq!(imported.items.len(), 1);
        let menu = &imported.items[0];
        assert_eq!(menu.label, "常用");
        assert_eq!(menu.accel, "1");
        assert_eq!(menu.children.len(), 2);
        assert_eq!(menu.children[0].label, "我的电脑");
        assert_eq!(menu.children[0].accel, "Z");
        assert_eq!(menu.children[0].actions[0].kind, ActionKind::Run);
        assert_eq!(menu.children[0].actions[0].value, "explorer.exe");
        assert_eq!(imported.stats.submenus, 1);
        assert_eq!(imported.stats.items, 2);
    }

    #[test]
    fn nested_submenus_follow_the_dash_count() {
        let text = "\
-常用
\t--截图
\t\tQQ截图|^!a::
\t\t---
\t\t截图2|SnippingTool.exe
\t--Snipaste
\t\tSnip截图|Snipaste.exe
-其它
\tNotepad|notepad.exe
";
        let imported = parse(text);
        assert_eq!(imported.items.len(), 2, "两个顶层子菜单");
        let common = &imported.items[0];
        assert_eq!(common.label, "常用");
        let shot = &common.children[0];
        assert_eq!(shot.label, "截图");
        // 三根连字符 = 第 3 层内容（「截图」自己的项），所以这些都在「截图」里
        assert_eq!(shot.children.len(), 3);
        assert_eq!(shot.children[0].label, "QQ截图");
        assert_eq!(shot.children[0].actions[0].kind, ActionKind::Keys);
        assert_eq!(shot.children[0].actions[0].value, "Ctrl+Alt+A");
        assert!(shot.children[1].kind == ItemKind::Separator);
        assert_eq!(shot.children[2].label, "截图2");
        // 两根连字符 = 回到第 2 层：「Snipaste」是「常用」下的第二支
        assert_eq!(common.children[1].label, "Snipaste");
        assert_eq!(imported.items[1].label, "其它");
        assert_eq!(imported.items[1].children[0].label, "Notepad");
    }

    #[test]
    fn a_tab_separated_hotkey_becomes_a_real_hotkey() {
        let imported = parse("\tQQ\t<!w|QQ.exe\n\t菜单\t^!+{F11}|searchRA()\n");
        assert_eq!(imported.items[0].hotkey, "Alt+W");
        assert_eq!(imported.items[1].hotkey, "Ctrl+Alt+Shift+F11");
        assert_eq!(imported.stats.hotkeys, 2);
    }

    #[test]
    fn commands_are_classified_like_the_original() {
        assert_eq!(classify("你好;").0, ActionKind::Text);
        assert_eq!(classify("你好;").1, "你好");
        assert_eq!(classify("你好;;").0, ActionKind::Text);
        assert_eq!(classify("你好;;").1, "你好");
        assert_eq!(classify("^!a::").0, ActionKind::Keys);
        assert_eq!(classify("^!a::").1, "Ctrl+Alt+A");
        assert_eq!(classify("https://gpui.rs").0, ActionKind::Url);
        assert_eq!(classify("www.baidu.com").0, ActionKind::Url);
        assert_eq!(classify("notepad.exe").0, ActionKind::Run);
        // 只写 `;` 结尾但长度不够的别误判
        assert_eq!(classify("a;").0, ActionKind::Text);
    }

    #[test]
    fn plugin_calls_become_the_plugin_builtin_action() {
        let imported = parse("\t开关|XiaoYao_plus[hotkeyStatus](%\"clash.exe\"%)\n");
        let item = &imported.items[0];
        // 插件调用落在**内置动作**里（动作库那一条），不是「运行程序 / 发送文本」
        assert_eq!(item.actions[0].kind, ActionKind::Builtin);
        assert_eq!(item.actions[0].value, "runany-plugin");
        assert_eq!(
            item.actions[0].param("call"),
            Some("XiaoYao_plus[hotkeyStatus](%\"clash.exe\"%)"),
            "整串调用要一个字不差地放进参数里"
        );
        assert!(item.note.contains("插件"), "插件函数要留一句备注: {:?}", item.note);
        assert_eq!(imported.stats.notes, 1);
    }

    /// 不是「名字[函数](参数)」那种写法的，仍旧按程序处理。
    #[test]
    fn only_real_plugin_calls_change_action_kind() {
        assert_eq!(classify("C:\\Tools\\a.exe").0, ActionKind::Run);
        assert_eq!(classify("截图[1]").0, ActionKind::Run, "没有括号的不算插件调用");
        assert_eq!(
            classify("XiaoYao_plus[hotkeyStatus]()").0,
            ActionKind::Text,
            "classify 只管普通命令；插件调用由 action_of 提前拦下"
        );
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let imported = parse(";注释\n\t;也是注释\n\n-常用\n\t项|a.exe\n");
        assert_eq!(imported.stats.items, 1);
        assert_eq!(imported.stats.skipped, 0);
    }

    /// 拿一份真实的 `RunAny.ini` 走一遍（开发时手工跑）：
    /// `RUNANY_INI=D:/RunAny/RunAny.ini cargo test parse_a_real -- --ignored --nocapture`
    #[test]
    #[ignore = "需要 RUNANY_INI 指向一份真实的 RunAny.ini"]
    fn parse_a_real_ini_file() {
        let Ok(path) = std::env::var("RUNANY_INI") else {
            return;
        };
        let text = read_ini(&path).expect("读不到文件");
        let imported = parse(&text);
        println!("{:?}", imported.stats);
        fn walk(items: &[ItemSetting], depth: usize, seen: &mut usize) {
            for item in items {
                if *seen < 70 {
                    println!(
                        "{}{} [{}] {}",
                        "  ".repeat(depth),
                        if item.label.is_empty() { "<空>" } else { &item.label },
                        item.actions
                            .first()
                            .map(|step| step.kind.label())
                            .unwrap_or("子菜单/分隔线"),
                        item.hotkey
                    );
                    *seen += 1;
                }
                walk(&item.children, depth + 1, seen);
            }
        }
        let mut seen = 0;
        walk(&imported.items, 0, &mut seen);
        assert!(!imported.items.is_empty());
    }

    /// 真实文件走到底：解析 → 写 `.runa` → 读回来，结构不能变形。
    /// `RUNANY_INI=D:/RunAny/RunAny.ini cargo test a_real_ini_becomes -- --ignored --nocapture`
    #[test]
    #[ignore = "会往 exe 同目录的 menus 里写文件，需要 RUNANY_INI"]
    fn a_real_ini_becomes_a_profile_file() {
        let Ok(path) = std::env::var("RUNANY_INI") else {
            return;
        };
        let text = read_ini(&path).expect("读不到文件");
        let built = build_profile(&text, "RunAny导入测试").expect("导入失败");
        assert!(built.stats.items > 1000, "{:?}", built.stats);

        let file = crate::menu_files::load(&built.name).expect("写出去的配置读不回来");
        assert_eq!(file.items.len(), built.items.len());
        assert!(file.next_id > 1);

        fn check(items: &[ItemSetting]) {
            for item in items {
                match item.kind {
                    ItemKind::Submenu => {
                        assert!(!item.children.is_empty(), "空子菜单不该留下: {}", item.label);
                        check(&item.children);
                    }
                    ItemKind::Item => {
                        assert!(!item.actions.is_empty(), "没有动作: {}", item.label);
                        assert!(!item.actions[0].value.is_empty(), "动作是空的: {}", item.label);
                    }
                    ItemKind::Separator => {}
                }
            }
        }
        check(&file.items);
        let _ = crate::menu_files::delete(&built.name);
        println!("ok: {} -> {}.{}", path, built.name, crate::menu_files::EXT);
    }

    /// 子菜单后面的 `|public file` 是「选中什么内容时显示这一支」，
    /// 不能当成名字的一部分。
    ///
    /// 现在就真的落到 `contexts` / `file_exts` 上了：用户要求
    /// `|xlsx ppt file` 这种写成后缀表，`|public` 选全部四类。
    #[test]
    fn a_submenu_association_becomes_a_short_note() {
        let imported = parse(
            "-文件(&G)|public file\n\t项|a.exe\n-图片|png jpg\n\t项2|b.exe\n-软件(&8)|软件(&8)\n\t项3|c.exe\n",
        );
        let file = &imported.items[0];
        assert_eq!(file.label, "文件", "关联不该留在名字里");
        assert_eq!(file.accel, "G");
        assert_eq!(file.context_mode, ContextMode::Own);
        assert!(file.contexts.contains(&crate::selection::SelectionKind::File));
        assert!(file.file_exts.is_empty(), "public 不是后缀");
        assert!(file.note.contains("选中内容"), "备注：{}", file.note);

        // `-图片|png jpg`：后缀表用竖线隔开
        let images = &imported.items[1];
        assert_eq!(images.file_exts, "png|jpg");
        assert!(images
            .contexts
            .contains(&crate::selection::SelectionKind::File));

        // `-软件(&8)|软件(&8)`：关联就是自己，等于没关联
        let software = &imported.items[2];
        assert!(software.contexts.is_empty());
        assert!(software.file_exts.is_empty());

        let pics = &imported.items[1];
        assert!(pics.note.contains("png"), "备注：{}", pics.note);
        assert!(!pics.note.contains("select"), "不该提窗口关联");
        assert!(pics.contexts.contains(&crate::selection::SelectionKind::File));

        // `|软件(&8)` 和名字一样：占位写法，等于没关联
        let software = &imported.items[2];
        assert!(software.note.is_empty(), "备注：{}", software.note);
    }

    #[test]
    fn a_two_digit_mnemonic_is_stripped_but_not_used_as_accel() {
        let imported = parse("-划词(&10)|text\n\t项|a.exe\n");
        assert_eq!(imported.items[0].label, "划词");
        assert!(imported.items[0].accel.is_empty(), "两位数的助记符当不了加速键");
    }

    /// 整棵菜单按「菜单呼出」的路径走一遍：每个节点都要算一次自动图标。
    /// 这一步以前会把九百多个插件函数当成程序名去查，一查就是十几秒。
    ///
    /// `RUNANY_INI=D:/RunAny/RunAny.ini cargo test a_real_menu_builds_fast -- --ignored --nocapture`
    #[test]
    #[ignore = "需要 RUNANY_INI，且会真的去解析程序路径"]
    fn a_real_menu_builds_fast() {
        let Ok(path) = std::env::var("RUNANY_INI") else {
            return;
        };
        let text = read_ini(&path).expect("读不到文件");
        let imported = parse(&text);

        fn walk(items: &[ItemSetting], seen: &mut usize) {
            for item in items {
                *seen += 1;
                // 菜单构建时 `auto_badge` 走的就是这一条
                let _ = item.program_path();
                walk(&item.children, seen);
            }
        }
        // 预热（第一次要建程序索引）
        let mut warm = 0;
        walk(&imported.items, &mut warm);

        let start = std::time::Instant::now();
        for _ in 0..3 {
            let mut seen = 0;
            walk(&imported.items, &mut seen);
        }
        let spent = start.elapsed() / 3;
        println!("{} 个节点，一轮图标解析 {:?}", warm, spent);
        assert!(
            spent.as_millis() < 150,
            "一轮要 {spent:?}，菜单呼出会明显卡"
        );
    }

    /// 「呼出菜单」那一步到底要多久：整棵树 → `Vec<MenuEntry>`（含图标路径解析）。
    ///
    /// `RUNANY_INI=D:/RunAny/RunAny.ini cargo test a_real_menu_opens_fast -- --ignored --nocapture`
    #[test]
    #[ignore = "需要 RUNANY_INI"]
    fn a_real_menu_opens_fast() {
        let Ok(path) = std::env::var("RUNANY_INI") else {
            return;
        };
        let text = read_ini(&path).expect("读不到文件");
        let built = parse(&text);
        let mut settings = crate::settings::MenuSettings::default();
        settings.adopt_profile_items(built.items, 1);
        // 预热：第一次要建程序索引 / 读图标缓存
        let _ = settings.menu_for(None);
        let _ = settings.menu_for(None);

        let rounds = 10;
        let start = std::time::Instant::now();
        for _ in 0..rounds {
            let entries = settings.menu_for(None);
            assert!(!entries.is_empty());
        }
        let spent = start.elapsed() / rounds;
        println!("呼出构建一轮 {spent:?}");
        assert!(spent.as_millis() < 30, "一轮 {spent:?}，呼出会顿");
    }

    /// 单独一行的连字符不只是分隔线：它还把**后面内容写进哪一层**重置到那一层。
    ///
    /// 这是原版 `Menu_Read` 里最容易被漏掉的一条（`menuLevel := StrLen(treeLevel)`
    /// 之后只有带名字的行才 `+=1`）。真实配置里到处都是
    /// `\t--` + 一堆 `\t项` 的写法，表示「这一段结束，回上一级」——
    /// 照「最近打开的子菜单」去算，整段就掉进上一个子菜单里了。
    #[test]
    fn a_bare_dash_run_sends_following_items_back_up() {
        let text = "\
-工具
\t--截图
\t\tQQ截图|^!a::
\t--
\t保存壁纸|a.exe
\t显示IP|b.exe
";
        let tool = &imported_tool(text);
        let labels: Vec<&str> = tool.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, vec!["截图", "", "保存壁纸", "显示IP"], "{labels:?}");
        assert_eq!(tool[0].children.len(), 1, "QQ截图 要留在「截图」里");
        assert!(tool[1].column_break == false && tool[1].kind == ItemKind::Separator);
    }

    /// `---` 在两层子菜单里：回到的是那一层，不是最外层。
    #[test]
    fn a_bare_dash_run_only_goes_up_one_level() {
        let text = "\
-工具
\t--截图
\t\t---PixPin
\t\t\tPP截图|a.exe
\t\t---
\t\tSnip截图|b.exe
";
        let tool = &imported_tool(text);
        let shot = &tool[0];
        assert_eq!(shot.label, "截图");
        let names: Vec<&str> = shot.children.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(names, vec!["PixPin", "", "Snip截图"], "{names:?}");
        assert_eq!(shot.children[0].children.len(), 1);
    }

    /// 「子菜单里既有子菜单、又有普通项」这一段走完整一点：项的归属一个都不能错。
    #[test]
    fn items_and_submenus_can_be_mixed_in_one_level() {
        let text = "\
-工具
\t--截图
\t\t---PixPin
\t\t\tPP截图|a.exe
\t\t---
\t\t第二个项|c.exe
\t\t---Snipaste
\t\t\tSnip截图|d.exe
\t--
\t直属项|e.exe
";
        let tool = &imported_tool(text);
        let shot = &tool[0];
        assert_eq!(shot.label, "截图");
        let names: Vec<&str> = shot.children.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(names, vec!["PixPin", "", "第二个项", "Snipaste"], "{names:?}");
        // `--` 既在「工具」里插一条分隔线，也把后面的项拉回「工具」
        assert!(tool[1].kind == ItemKind::Separator);
        assert_eq!(
            tool[2].label, "直属项",
            "`--` 之后要回到「工具」，不该留在「截图」里"
        );
    }

    fn imported_tool(text: &str) -> Vec<ItemSetting> {
        let imported = parse(text);
        imported
            .items
            .into_iter()
            .find(|item| item.label == "工具")
            .expect("没有工具这一支")
            .children
    }

    #[test]
    fn a_column_break_is_kept_as_a_break() {
        let imported = parse("-常用\n\t项|a.exe\n\t||\n\t项2|b.exe\n");
        let children = &imported.items[0].children;
        assert!(children[1].kind == ItemKind::Separator);
        assert!(children[1].column_break);
    }






}
