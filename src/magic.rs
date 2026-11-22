//! 一键直达：选中内容 + 一个快捷键 → 按规则表**依次**匹配，第一条命中就直接跑它
//! 绑定的菜单项 / 子菜单，**不弹菜单**。
//!
//! ## 为什么是「规则表」而不是「一条正则」
//!
//! 从网页上复制一段东西，它可能是网址、可能是一个颜色值、可能是一段 JSON、
//! 也可能是一堆文件路径 —— 全都甩给一条正则，写的人自己都看不懂。所以这里做成
//! 一张**有序的表**：每条规则选一个「怎么比」（正则 / 包含 / 通配 / 是网址 /
//! 是文件 / 是颜色 …），命中就跳到它绑的那个菜单项。
//!
//! | 想做的事 | 规则 |
//! | --- | --- |
//! | 选中的是网址 → 用浏览器打开 | `是网址` → 绑「浏览器打开」 |
//! | 选中的是颜色值 → 复制成 CSS 变量 | `是颜色` → 绑「转 CSS 变量」 |
//! | 选中的是 JSON → 格式化 | `是 JSON` → 绑「格式化 JSON」 |
//! | 选中的是 `*.mp4` 路径 → 转码 | `后缀 是 mp4\|mkv` → 绑「转码」 |
//! | 选中的是 `订单号: 123456` → 查单 | `正则 订单号:\s*(\d+)` → 绑「查订单」 |
//!
//! ## 顺序就是优先级
//!
//! 从上往下问，第一个说「是我」的就执行，后面的不再看。所以宽泛的规则（比如
//! `任意非空`）要放最后，不然它会把后面所有规则都挡掉。

use serde::{Deserialize, Serialize};

/// 一条规则怎么比。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// 正则（默认不区分大小写；想逐行匹配自己写 `(?m)`）
    Regex,
    /// 包含这段文字
    Contains,
    /// 以这段文字开头
    StartsWith,
    /// 以这段文字结尾
    EndsWith,
    /// 通配符：`*` 任意多个字符、`?` 一个字符（`*.mp4`、`订单-??-*`）
    Glob,
    /// 是一个网址（`http://` / `https://` / `ftp://` / `www.` 开头）
    Url,
    /// 像一条路径（`C:\` / `\\服务器\` / `/usr/` 开头）
    Path,
    /// 这个路径**存在，而且是文件**
    File,
    /// 这个路径**存在，而且是文件夹**
    Folder,
    /// 后缀在这份名单里（`mp4|mkv` 或 `mp4,mkv`）
    Ext,
    /// 颜色值：`#fff` / `#a1b2c3` / `rgb(1,2,3)`
    Color,
    /// 一个数字（允许千分位和百分号）
    Number,
    /// 一个邮箱地址
    Email,
    /// 能解析成 JSON
    Json,
    /// 一个 IP 地址（v4 / v6 都算）
    Ip,
    /// 一个 UUID
    Uuid,
    /// 只要非空就算命中（放在最后当兜底）
    Any,
}

impl Kind {
    pub const ALL: [Kind; 17] = [
        Kind::Regex,
        Kind::Contains,
        Kind::StartsWith,
        Kind::EndsWith,
        Kind::Glob,
        Kind::Url,
        Kind::Path,
        Kind::File,
        Kind::Folder,
        Kind::Ext,
        Kind::Color,
        Kind::Number,
        Kind::Email,
        Kind::Json,
        Kind::Ip,
        Kind::Uuid,
        Kind::Any,
    ];

    /// 存盘用的写法（和 serde 的 `snake_case` 一致）。
    ///
    /// 界面用的是 [`Kind::label`]；这两个是给「存盘 / 读盘 / 导入别人的规则表」用的。
    #[allow(dead_code)]
    pub fn key(self) -> &'static str {
        match self {
            Kind::Regex => "regex",
            Kind::Contains => "contains",
            Kind::StartsWith => "starts_with",
            Kind::EndsWith => "ends_with",
            Kind::Glob => "glob",
            Kind::Url => "url",
            Kind::Path => "path",
            Kind::File => "file",
            Kind::Folder => "folder",
            Kind::Ext => "ext",
            Kind::Color => "color",
            Kind::Number => "number",
            Kind::Email => "email",
            Kind::Json => "json",
            Kind::Ip => "ip",
            Kind::Uuid => "uuid",
            Kind::Any => "any",
        }
    }

    #[allow(dead_code)]
    pub fn from_key(key: &str) -> Option<Kind> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.key() == key.trim())
    }

    /// 界面上那个胶囊上写的字。
    pub fn label(self) -> &'static str {
        match self {
            Kind::Regex => "正则",
            Kind::Contains => "包含",
            Kind::StartsWith => "开头是",
            Kind::EndsWith => "结尾是",
            Kind::Glob => "通配",
            Kind::Url => "是网址",
            Kind::Path => "是路径",
            Kind::File => "是文件",
            Kind::Folder => "是文件夹",
            Kind::Ext => "后缀是",
            Kind::Color => "是颜色",
            Kind::Number => "是数字",
            Kind::Email => "是邮箱",
            Kind::Json => "是 JSON",
            Kind::Ip => "是 IP",
            Kind::Uuid => "是 UUID",
            Kind::Any => "非空就行",
        }
    }

    /// 表达式框里的占位提示（每种给个能直接抄的例子）。
    pub fn example(self) -> &'static str {
        match self {
            Kind::Regex => r"^订单号[:：]\s*(\d+)$",
            Kind::Contains => "github.com",
            Kind::StartsWith => "https://",
            Kind::EndsWith => ".mp4",
            Kind::Glob => "*.mp4",
            Kind::Url => "（不用填）",
            Kind::Path => "（不用填）",
            Kind::File => "（不用填）",
            Kind::Folder => "（不用填）",
            Kind::Ext => "mp4|mkv",
            Kind::Color => "（不用填）",
            Kind::Number => "（不用填）",
            Kind::Email => "（不用填）",
            Kind::Json => "（不用填）",
            Kind::Ip => "（不用填）",
            Kind::Uuid => "（不用填）",
            Kind::Any => "（不用填）",
        }
    }

    /// 这一类要填表达式吗（「是网址」这种不需要）。
    pub fn needs_pattern(self) -> bool {
        matches!(
            self,
            Kind::Regex
                | Kind::Contains
                | Kind::StartsWith
                | Kind::EndsWith
                | Kind::Glob
                | Kind::Ext
        )
    }
}

/// 一条规则。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub id: String,
    pub kind: Kind,
    /// 表达式（看 `Kind`；`是网址` 这类不需要）
    pub pattern: String,
    /// 命中之后跑谁：菜单项 / 子菜单的 id
    pub target: String,
    /// 给自己看的备注
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// 多行匹配：让 `.` 也能吃换行（正则加 `(?s)`）。
    ///
    /// 抓到的选中内容经常是好几行（多选的文件、一段邮件原文），
    /// 不开这个的话 `^...$` 只能落在其中某一行里。
    #[serde(default)]
    pub multiline: bool,
    pub enabled: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: Kind::Regex,
            pattern: String::new(),
            target: String::new(),
            note: String::new(),
            multiline: false,
            enabled: true,
        }
    }
}

impl Rule {
    /// 一行摘要（设置页列表里显示）。
    pub fn summary(&self) -> String {
        if self.kind.needs_pattern() {
            let pattern = self.pattern.trim();
            if pattern.is_empty() {
                self.kind.label().to_string()
            } else {
                format!("{} {}", self.kind.label(), pattern)
            }
        } else {
            self.kind.label().to_string()
        }
    }
}

/// 整个「一键直达」的配置。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MagicSettings {
    /// 总开关（关掉之后这一层完全不参与）
    pub enabled: bool,
    /// 专用全局快捷键。**留空 = 跟「弹出菜单」共用同一颗键** —— 按下先问规则，
    /// 一条都不中再照常弹菜单。填了就是一颗独立的键。
    pub hotkey: String,
    /// 这条快捷键要不要区分左右修饰键
    pub sided: bool,
    /// 一条都没命中时：`true` = 照常弹菜单，`false` = 什么都不做
    pub fallback_menu: bool,
    pub rules: Vec<Rule>,
}

impl Default for MagicSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: String::new(),
            sided: true,
            fallback_menu: true,
            rules: Vec::new(),
        }
    }
}

impl MagicSettings {
    // 「会参与的几条」原来靠 `active()` 数，那个汇总行已经从界面上删掉了
    // （规则表里每条自己带开关状态，再数一遍是重复），所以这里不再留这个方法。
}

impl Rule {
    /// 这条规则齐活了吗（能参与匹配）。
    ///
    /// 不看 `pattern` 的种类就把表达式忽略掉：用户从「正则」切成「是网址」时，
    /// 输入框里那些正则还留着，不能因此把规则判成「没填完」而悄悄失效。
    pub fn ready(&self) -> bool {
        self.enabled
            && !self.target.trim().is_empty()
            && (!self.kind.needs_pattern() || !self.pattern.trim().is_empty())
    }
}

// ---------------------------------------------------------------------------
// 匹配
// ---------------------------------------------------------------------------

/// 把通配符（`*` `?`）转成正则片段。
fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            _ => out.push_str(&regex::escape(&ch.to_string())),
        }
    }
    out
}

fn build(pattern: &str) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|err| {
            // 正则自己的报错信息很长，只留第一行就够看了
            let text = err.to_string();
            text.lines().next().unwrap_or("正则写错了").to_string()
        })
}

/// 这条规则的表达式能不能用（`Err` 里是给人看的原因）。
pub fn problem(rule: &Rule) -> Option<String> {
    if rule.kind.needs_pattern() && rule.pattern.trim().is_empty() {
        return Some(crate::i18n::t("这类规则要填表达式").to_string());
    }
    if rule.target.trim().is_empty() {
        return Some(crate::i18n::t("还没选命中之后跑哪一项").to_string());
    }
    if rule.kind == Kind::Regex {
        if let Err(err) = build(rule.pattern.trim()) {
            return Some(err);
        }
    }
    None
}

/// 这段文字命中这条规则吗。
pub fn hit(rule: &Rule, text: &str) -> bool {
    if !rule.ready() {
        return false;
    }
    let hay = text.trim();
    let pattern = rule.pattern.trim();
    match rule.kind {
        Kind::Any => !hay.is_empty(),
        Kind::Json => serde_json::from_str::<serde_json::Value>(hay).is_ok(),
        Kind::File => any_line(hay, |line| std::path::Path::new(line).is_file()),
        Kind::Folder => any_line(hay, |line| std::path::Path::new(line).is_dir()),
        Kind::Ext => {
            let wanted = crate::selection::parse_exts(pattern);
            if wanted.is_empty() {
                return false;
            }
            any_line(hay, |line| {
                crate::selection::ext_of(line)
                    .map(|ext| wanted.iter().any(|want| *want == ext))
                    .unwrap_or(false)
            })
        }
        Kind::Ip => hit_ip(hay),
        // 剩下的一律化成一条正则
        _ => {
            let source = match rule.kind {
                Kind::Regex => pattern.to_string(),
                Kind::Contains => regex::escape(pattern),
                Kind::StartsWith => format!("^{}", regex::escape(pattern)),
                Kind::EndsWith => format!("{}$", regex::escape(pattern)),
                Kind::Glob => format!("^{}$", glob_to_regex(pattern)),
                Kind::Url => r"^((https?|ftp|file)://|www\.)\S+$".to_string(),
                Kind::Path => r"^([a-zA-Z]:[\\/]|\\\\|~?/)[^\r\n]*$".to_string(),
                Kind::Color => {
                    r"^(#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})|rgba?\([^)]*\))$".to_string()
                }
                Kind::Number => r"^[+-]?[\d,]*\.?\d+([eE][+-]?\d+)?%?$".to_string(),
                Kind::Email => r"^[\w.+-]+@[\w-]+(\.[\w-]+)+$".to_string(),
                Kind::Uuid => {
                    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$".to_string()
                }
                _ => return false,
            };
            // 多行模式：`(?s)` 让 `.` 跨行；不加的话跨行内容只能靠 `[\s\S]`
            let source = if rule.multiline {
                format!("(?s){source}")
            } else {
                source
            };
            match build(&source) {
                Ok(regex) => regex.is_match(hay),
                Err(_) => false,
            }
        }
    }
}

/// 多行文本（多选文件）里只要有一行满足就算。
fn any_line(text: &str, f: impl Fn(&str) -> bool) -> bool {
    text.split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .any(|line| f(line.trim_matches('"')))
}

/// IP：v4 逐段查范围，v6 只看形状（够用了，不是写防火墙规则）。
fn hit_ip(text: &str) -> bool {
    let v4 = |text: &str| -> bool {
        let parts: Vec<&str> = text.split('.').collect();
        parts.len() == 4
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.len() <= 3
                    && part.chars().all(|c| c.is_ascii_digit())
                    && part.parse::<u16>().map(|n| n <= 255).unwrap_or(false)
            })
    };
    if v4(text) {
        return true;
    }
    // v6：至少两个冒号、只含十六进制和冒号（`::1` 这种也算）
    text.contains(':')
        && text.matches(':').count() >= 2
        && text
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
}

/// 按顺序问下去，第一个命中的就是答案。
pub fn first_hit<'a>(rules: &'a [Rule], text: &str) -> Option<&'a Rule> {
    rules
        .iter()
        .filter(|rule| rule.enabled)
        .find(|rule| hit(rule, text))
}

/// 一键直达是不是「蹭」弹出菜单那一颗键（自己没配）。
pub fn shares_popup_hotkey() -> bool {
    crate::settings::read(|settings| {
        settings.magic.enabled && settings.magic.hotkey.trim().is_empty()
    })
}

/// 新建一个「直达项」（隐藏层里的普通项 / 子菜单），返回它的 id。
///
/// 直达项是**独立**的：它不在菜单里出现，只被规则引用。做成真的菜单项
/// 才能白捡整套动作编辑器（运行程序、发送按键、子菜单…）。
pub fn create_target(cx: &mut gpui::App, submenu: bool) -> String {
    crate::settings::update(cx, |settings| settings.add_magic_target(submenu))
}

/// 把一批表达式加成规则（全部用同一个类型 + 同一个目标），返回新规则的 id。
///
/// 「多条规则一起写」就是这么落盘的：窗口里一行一个表达式，确定时一次追加。
pub fn add_rules(
    cx: &mut gpui::App,
    kind: Kind,
    patterns: &[String],
    target: &str,
    note: &str,
) -> Vec<String> {
    add_rules_with(cx, kind, patterns, target, note, false)
}

/// 同上，多一个「多行匹配」开关。
pub fn add_rules_with(
    cx: &mut gpui::App,
    kind: Kind,
    patterns: &[String],
    target: &str,
    note: &str,
    multiline: bool,
) -> Vec<String> {
    let target = target.trim().to_string();
    let note = note.trim().to_string();
    let mut patterns: Vec<String> = patterns
        .iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty() || !kind.needs_pattern())
        .collect();
    // 「是网址」「是文件」这类根本不用填表达式：表达式框空着的时候，
    // `text.lines()` 连一行都吐不出来，patterns 于是是空的 —— 以前这里直接
    // 当成「没填东西」返回空，导致选了这类类型、框里不写字就**存不进去**。
    // 给它补一个空表达式，规则表里保存成一条「看内容本身长什么样」的规则。
    if patterns.is_empty() && !kind.needs_pattern() {
        patterns.push(String::new());
    }
    if patterns.is_empty() || target.is_empty() {
        return Vec::new();
    }
    crate::settings::update(cx, move |settings| {
        let mut ids = Vec::new();
        // 「是网址」这类不用表达式：一批里也只加一条，免得点一次多出五条一样的
        let list: Vec<String> = if kind.needs_pattern() {
            patterns
        } else {
            vec![String::new()]
        };
        for pattern in list {
            let id = format!("magic-{}", settings.next_id);
            settings.next_id = settings.next_id.saturating_add(1);
            settings.magic.rules.push(Rule {
                id: id.clone(),
                kind,
                pattern,
                target: target.clone(),
                note: note.clone(),
                multiline,
                enabled: true,
            });
            ids.push(id);
        }
        ids
    })
}

/// 建一条规则，**顺带把它要跑的那个菜单项建出来**（一条规则配一个项）。
///
/// 以前是「先建项、再在规则窗口里从一堆已有项里挑一个」，用户得在两处之间来回跳；
/// 现在关系是固定的：新建规则 = 新建项，绑定写死在创建那一刻。
pub fn create_rule_with_item(
    cx: &mut gpui::App,
    kind: Kind,
    pattern: &str,
    note: &str,
    multiline: bool,
    submenu: bool,
    name: &str,
) -> Option<(String, String)> {
    let target = create_target(cx, submenu);
    let name = name.trim().to_string();
    if !name.is_empty() {
        let id = target.clone();
        crate::settings::update(cx, move |settings| {
            if let Some(path) = settings.find_path(&id) {
                if let Some(item) = settings.node_mut(&path) {
                    item.label = name;
                }
            }
        });
    }
    let ids = add_rules_with(cx, kind, &[pattern.to_string()], &target, note, multiline);
    ids.first().cloned().map(|rule| (rule, target))
}

/// 复制给别人用的那一坨：规则 + 它跑的那个菜单项（含动作、子项）。
#[derive(serde::Serialize, serde::Deserialize)]
struct Bundle {
    /// 格式标记，防止把别的 JSON 当成规则
    runany_magic_rules: u32,
    rules: Vec<BundledRule>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BundledRule {
    kind: Kind,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    multiline: bool,
    #[serde(default = "yes")]
    enabled: bool,
    /// 命中之后跑的那一项（连带子项、动作一起带走）
    #[serde(default)]
    item: Option<crate::settings::ItemSetting>,
}

fn yes() -> bool {
    true
}

/// 把「一键直达」的所有规则打包成 JSON（连菜单项一起，方便整个发给别人）。
pub fn export_bundle() -> String {
    let bundle = crate::settings::read(|settings| Bundle {
        runany_magic_rules: 1,
        rules: settings
            .magic
            .rules
            .iter()
            .map(|rule| BundledRule {
                kind: rule.kind,
                pattern: rule.pattern.clone(),
                note: rule.note.clone(),
                multiline: rule.multiline,
                enabled: rule.enabled,
                item: settings.item_ref(&rule.target).cloned(),
            })
            .collect(),
    });
    serde_json::to_string_pretty(&bundle).unwrap_or_default()
}

/// 只导出一条规则（同样带它跑的菜单项）。给每条规则上的「复制」按钮用。
pub fn export_rule(id: &str) -> String {
    let bundle = crate::settings::read(|settings| Bundle {
        runany_magic_rules: 1,
        rules: settings
            .magic
            .rules
            .iter()
            .filter(|rule| rule.id == id)
            .map(|rule| BundledRule {
                kind: rule.kind,
                pattern: rule.pattern.clone(),
                note: rule.note.clone(),
                multiline: rule.multiline,
                enabled: rule.enabled,
                item: settings.item_ref(&rule.target).cloned(),
            })
            .collect(),
    });
    serde_json::to_string_pretty(&bundle).unwrap_or_default()
}

/// 把一条规则挪到第 `to` 个位置（拖拽排序用；越界会自动夹住）。
pub fn move_rule_to(cx: &mut gpui::App, id: &str, to: usize) {
    let id = id.to_string();
    crate::settings::update(cx, move |settings| {
        let rules = &mut settings.magic.rules;
        let Some(from) = rules.iter().position(|rule| rule.id == id) else {
            return;
        };
        let to = to.min(rules.len().saturating_sub(1));
        if from == to {
            return;
        }
        let rule = rules.remove(from);
        rules.insert(to, rule);
    });
}

/// 从 JSON 追加规则（每条都重新建一个菜单项，id 全新）。返回加了几个。
pub fn import_bundle(cx: &mut gpui::App, json: &str) -> Result<usize, String> {
    let bundle: Bundle =
        serde_json::from_str(json).map_err(|err| format!("不是一键直达规则的格式：{err}"))?;
    if bundle.runany_magic_rules == 0 {
        return Err(crate::i18n::t("这份 JSON 不是一键直达规则").to_string());
    }
    let mut added = 0usize;
    crate::settings::update(cx, |settings| {
        let container = settings.magic_targets_path();
        for entry in bundle.rules {
            // 菜单项：有就整棵搬过来（换个新 id），没有就现建一个空的
            let target = match entry.item {
                Some(mut item) => {
                    fn reid(settings: &mut crate::settings::MenuSettings, node: &mut crate::settings::ItemSetting) {
                        node.id = settings.next_item_id();
                        for child in &mut node.children {
                            reid(settings, child);
                        }
                    }
                    reid(settings, &mut item);
                    let id = item.id.clone();
                    if let Some(list) = settings.sibling_list_mut(&container) {
                        list.push(item);
                    }
                    id
                }
                None => settings.add_magic_target(false),
            };
            let id = format!("magic-{}", settings.next_id);
            settings.next_id = settings.next_id.saturating_add(1);
            settings.magic.rules.push(Rule {
                id,
                kind: entry.kind,
                pattern: entry.pattern,
                target,
                note: entry.note,
                multiline: entry.multiline,
                enabled: entry.enabled,
            });
            added += 1;
        }
        settings.normalize_tree();
    });
    Ok(added)
}

/// 改一条规则（窗口里点「保存」时整体覆盖这三样，省得一个字段一个字段对）。
pub fn update_rule(
    cx: &mut gpui::App,
    id: &str,
    kind: Kind,
    pattern: &str,
    target: &str,
    note: &str,
) {
    update_rule_with(cx, id, kind, pattern, target, note, false);
}

/// 改一条规则（含「多行匹配」开关）。
pub fn update_rule_with(
    cx: &mut gpui::App,
    id: &str,
    kind: Kind,
    pattern: &str,
    target: &str,
    note: &str,
    multiline: bool,
) {
    let id = id.to_string();
    let pattern = pattern.trim().to_string();
    let target = target.trim().to_string();
    let note = note.trim().to_string();
    crate::settings::update(cx, move |settings| {
        if let Some(rule) = settings.magic.rules.iter_mut().find(|rule| rule.id == id) {
            rule.kind = kind;
            rule.pattern = pattern;
            rule.target = target;
            rule.note = note;
            rule.multiline = multiline;
        }
    });
}

/// 直达项现在叫什么（给界面显示）。
pub fn target_summary(id: &str) -> (String, bool) {
    crate::settings::read(|settings| match settings.item_ref(id) {
        Some(item) => (
            if item.label.trim().is_empty() {
                crate::i18n::t("未命名的直达项").to_string()
            } else {
                item.label.clone()
            },
            item.is_submenu(),
        ),
        None => (crate::i18n::t("直达项不见了").to_string(), false),
    })
}

/// 已经建好的直达项（给「选一个现成的」用）：`(id, 名字, 是不是子菜单)`。
pub fn existing_targets() -> Vec<(String, String, bool)> {
    crate::settings::read(|settings| {
        let Some(path) = settings.find_path(crate::settings::MAGIC_ID) else {
            return Vec::new();
        };
        settings
            .node(&path)
            .map(|node| {
                node.children
                    .iter()
                    .filter(|item| item.kind != crate::settings::ItemKind::Separator)
                    .map(|item| (item.id.clone(), item.label.clone(), item.is_submenu()))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// 规则绑的是子菜单吗（子菜单要「摊开」而不是直接执行）。
pub fn target_is_submenu(id: &str) -> bool {
    crate::settings::read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .map(|item| item.is_submenu())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: Kind, pattern: &str) -> Rule {
        Rule {
            kind,
            pattern: pattern.to_string(),
            target: "custom-1".to_string(),
            ..Rule::default()
        }
    }

    #[test]
    fn regex_matches_the_whole_selection() {
        let rule = rule(Kind::Regex, r"^订单号[:：]\s*(\d+)$");
        assert!(hit(&rule, "订单号: 123456"));
        assert!(hit(&rule, "  订单号：123456  "), "首尾空白要忽略");
        assert!(!hit(&rule, "订单号: abc"));
    }

    #[test]
    fn the_kind_table_round_trips() {
        for kind in Kind::ALL {
            assert_eq!(Kind::from_key(kind.key()), Some(kind), "{:?}", kind);
        }
    }

    #[test]
    fn the_builtin_kinds_recognise_their_own_things() {
        assert!(hit(&rule(Kind::Url, ""), "https://example.com/a?b=1"));
        // 反过来的几个典型误判
        assert!(!hit(&rule(Kind::Url, ""), "看这个 https://example.com"));
        assert!(!hit(&rule(Kind::Color, ""), "#12345"), "5 位不是颜色");
        assert!(hit(&rule(Kind::Color, ""), "#a1b2c3"));
        assert!(hit(&rule(Kind::Color, ""), "rgb(255, 0, 0)"));
        assert!(hit(&rule(Kind::Number, ""), "1,234.50"));
        assert!(hit(&rule(Kind::Number, ""), "12%"));
        assert!(!hit(&rule(Kind::Number, ""), "12 个"));
        assert!(hit(&rule(Kind::Email, ""), "a.b+c@mail.example.com"));
        assert!(hit(&rule(Kind::Json, ""), r#"{"a":[1,2]}"#));
        assert!(!hit(&rule(Kind::Json, ""), "{a:1}"));
        assert!(hit(
            &rule(Kind::Uuid, ""),
            "123e4567-e89b-12d3-a456-426614174000"
        ));
        assert!(!hit(&rule(Kind::Uuid, ""), "123e4567e89b12d3a456426614174000"));
    }

    #[test]
    fn ips_are_checked_segment_by_segment() {
        let ip = rule(Kind::Ip, "");
        assert!(hit(&ip, "192.168.1.1"));
        assert!(hit(&ip, "::1"));
        assert!(hit(&ip, "fe80::1"));
        assert!(!hit(&ip, "999.1.1.1"), "每段不能超过 255");
        assert!(!hit(&ip, "1.2.3"), "四段才叫 v4");
    }

    #[test]
    fn globs_are_anchored_and_escape_their_literals() {
        let glob = rule(Kind::Glob, "*.mp4");
        assert!(hit(&glob, "D:\\片\\a.mp4"));
        assert!(!hit(&glob, "D:\\片\\a.mp4 后面还有字"), "通配要对整串成立");
        let dotted = rule(Kind::Glob, "a.b");
        assert!(hit(&dotted, "a.b"));
        assert!(!hit(&dotted, "axb"), "`a.b` 里的点必须是字面量");
    }

    #[test]
    fn ext_lists_can_be_written_either_way() {
        for list in ["mp4|mkv", "mp4,MKV", ".mp4;.mkv"] {
            let ext = rule(Kind::Ext, list);
            assert!(hit(&ext, "D:\\片\\a.MP4"), "{list} 应该认 .mp4");
        }
        let ext = rule(Kind::Ext, "mp4");
        assert!(!hit(&ext, "D:\\片\\a.png"));
    }

    #[test]
    fn files_and_folders_ask_the_disk() {
        let exe = std::env::current_exe().expect("测试进程总有一个 exe");
        let file = rule(Kind::File, "");
        assert!(hit(&file, &exe.to_string_lossy()), "自己的 exe 得算是文件");
        assert!(!hit(&file, "Z:\\根本没有这个文件.txt"));
        let folder = rule(Kind::Folder, "");
        let parent = exe.parent().expect("exe 一定有父目录");
        assert!(hit(&folder, &parent.to_string_lossy()));
        assert!(!hit(&folder, &exe.to_string_lossy()), "文件不是文件夹");
    }

    #[test]
    fn order_decides_the_winner() {
        let rules = vec![
            rule(Kind::Url, ""),
            rule(Kind::Any, ""),
        ];
        let winner = first_hit(&rules, "https://a.b").expect("应该命中");
        assert_eq!(winner.kind, Kind::Url, "网址要赢过兜底的「非空」");
        // 关掉的规则不参与
        let mut off = rule(Kind::Url, "");
        off.enabled = false;
        let rules = vec![off, rule(Kind::Any, "")];
        assert_eq!(first_hit(&rules, "https://a.b").unwrap().kind, Kind::Any);
    }

    #[test]
    fn a_half_filled_rule_never_fires() {
        // 没填表达式
        assert!(!hit(&rule(Kind::Regex, "  "), "任意"));
        // 没选目标
        let no_target = Rule {
            kind: Kind::Any,
            target: String::new(),
            ..Rule::default()
        };
        assert!(!hit(&no_target, "任意"));
    }

    #[test]
    fn a_broken_regex_is_reported_not_panicking() {
        let broken = rule(Kind::Regex, "([a-z");
        assert!(!hit(&broken, "abc"));
        assert!(problem(&broken).is_some(), "要能告诉用户正则错了");
        assert!(problem(&rule(Kind::Regex, r"^\d+$")).is_none());
    }
}
