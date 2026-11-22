//! 触发快捷键的「生效条件」—— AHK 里的 `#If`。
//!
//! ## 为什么需要它
//!
//! 一个 `Ctrl+Alt+F`，在资源管理器里想「打开这个文件夹」，在编辑器里想「格式化」，
//! 在 QQ 里想「发出去」。以前只能把键让给其中一个，剩下的去菜单里点。
//! 有了生效条件，同一个组合可以在不同窗口里干不同的事 —— 而且**不满足条件时
//! 连键都不吞**，那个组合在别的程序里还是它本来的意思。
//!
//! ## 怎么写
//!
//! 一行一条，**行与行之间是「或」**（满足任意一行就生效）：
//!
//! ```text
//! Explorer.exe
//! title:另一个窗口
//! ```
//!
//! 一行里可以写多个条件：用 `&&` 连是「且」，用逗号`,`连是「或」。
//!
//! ```text
//! QQ.exe && title:群聊
//! ```
//!
//! 每个条件可以带前缀指定拿什么去比：
//!
//! | 前缀 | 比什么 | 例子 |
//! | --- | --- | --- |
//! | `exe:` | 活动窗口进程的 exe 名 | `exe:QQ.exe` |
//! | `path:` | 活动窗口进程的完整路径 | `path:*\\Tencent\\*` |
//! | `title:` | 活动窗口标题 | `title:^聊天` |
//! | `class:` | 活动窗口类名 | `class:Chrome_WidgetWin_1` |
//! | `ctrl:` | **活动窗口里、光标底下那个控件的类名** | `ctrl:Button` |
//! | 不写 | exe / 标题 / 类名 / 光标下控件，**哪个命中算哪个** | `QQ` |
//!
//! ### 活动窗口 vs 鼠标下窗口
//!
//! 上面那些默认看的都是**活动窗口**（按快捷键那一刻拿到焦点的那个）。想改成看
//! **鼠标指针底下那扇窗口**，在前面加 `mouse.`：
//!
//! ```text
//! mouse.exe:notepad.exe
//! mouse.title:*草稿*
//! exe:explorer.exe && mouse.class:DirectUIHWND
//! ```
//!
//! 典型场景：右 Alt 那颗键经常这样用 —— 鼠标停在哪扇窗口上就作用于哪扇，
//! 哪怕当前焦点在别的程序里。
//!
//! `ctrl:`（光标下控件）两边都有，只是含义跟着窗口走：
//!
//! - `ctrl:Button` —— 鼠标在**活动窗口**里时，它底下那个控件的类名；
//!   鼠标跑到别的窗口上时这一格是空的（那时候「活动窗口的光标下控件」不成立）
//! - `mouse.ctrl:Button` —— 鼠标底下那扇窗口里、光标所在控件的类名，不管焦点在哪
//!
//! 值的写法：默认「包含」，`*` `?` 是通配，`=x` 是全等，`/re/` 是正则。
//! 前面加 `!` 取反：`!exe:explorer.exe`。
//!
//! ## 判断在哪儿做
//!
//! 在**钩子里**当场做（`GetForegroundWindow` + `QueryFullProcessImageNameW` +
//! `GetClassNameW`，都是几十微秒的内存/内核调用）。必须这样：钩子要同步决定
//! 「吞不吞这个键」，不能先送到界面线程问一圈。

use regex::Regex;

/// 拿什么去比。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// 前台进程 exe 名
    Exe,
    /// 前台进程完整路径
    Path,
    /// 窗口标题
    Title,
    /// 窗口类名
    Class,
    /// 鼠标光标下那个控件的类名
    CtrlClass,
    /// 不写前缀：上面几个里任意一个命中就算
    Any,
}

/// 一个比较器。
#[derive(Debug)]
enum Matcher {
    /// 包含 / 通配（都编译成正则，统一处理大小写）
    Pattern(Regex),
    /// `=x`：整串相等
    Exact(String),
}

impl Matcher {
    fn test(&self, value: &str) -> bool {
        match self {
            Matcher::Pattern(regex) => regex.is_match(value),
            Matcher::Exact(want) => value.eq_ignore_ascii_case(want),
        }
    }
}

/// 条件看的是哪扇窗口。
///
/// 默认是**活动窗口**（拿到焦点的那个）；写了 `mouse.` 前缀就看**鼠标底下**那扇。
/// 两者不是一回事：焦点在编辑器里、鼠标却停在任务栏 / 另一个窗口上，是很常见的。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Target {
    /// 活动（前台）窗口 —— 不写前缀时的默认
    #[default]
    Active,
    /// 鼠标指针底下那扇窗口（条件里写 `mouse.`）
    Mouse,
}

/// 一个条件（`[!][mouse.]前缀:值`）。
#[derive(Debug)]
struct Cond {
    field: Field,
    /// 看哪扇窗口
    target: Target,
    negate: bool,
    matcher: Matcher,
}

impl Cond {
    fn test(&self, ctx: &Context) -> bool {
        // 先按来源挑出这一份现场：`mouse.` 的条件看鼠标下窗口，其余看活动窗口
        let (exe, path, title, class, ctrl) = match self.target {
            Target::Active => (
                &ctx.exe,
                &ctx.path,
                &ctx.title,
                &ctx.class,
                &ctx.ctrl_class,
            ),
            Target::Mouse => (
                &ctx.mouse_exe,
                &ctx.mouse_path,
                &ctx.mouse_title,
                &ctx.mouse_class,
                &ctx.mouse_ctrl_class,
            ),
        };
        let hit = match self.field {
            Field::Exe => self.matcher.test(exe),
            Field::Path => self.matcher.test(path),
            Field::Title => self.matcher.test(title),
            Field::Class => self.matcher.test(class),
            Field::CtrlClass => self.matcher.test(ctrl),
            // 裸值：几个都试一遍，有一个中就算中（「QQ」既能对上 QQ.exe，也能对上标题、
            // 路径、类名、光标下控件 —— 用户不用先想清楚自己写的是哪一类）
            Field::Any => {
                self.matcher.test(exe)
                    || self.matcher.test(path)
                    || self.matcher.test(title)
                    || self.matcher.test(class)
                    || self.matcher.test(ctrl)
            }
        };
        hit != self.negate
    }
}

/// 一行规则：`或(且(或(条件)))`。
///
/// - 逗号连的是一组「或」
/// - `&&` 连的是几组「且」
#[derive(Debug)]
struct Line {
    /// 外层是「且」，内层是「或」
    groups: Vec<Vec<Cond>>,
}

impl Line {
    fn test(&self, ctx: &Context) -> bool {
        self.groups
            .iter()
            .all(|group| group.iter().any(|cond| cond.test(ctx)))
    }
}

/// 编译好的一份条件（空 = 没有条件，任何程序里都生效）。
#[derive(Debug, Default)]
pub struct Conditions {
    lines: Vec<Line>,
    /// 编译时遇到的问题（写错的行），界面拿它标红
    problems: Vec<String>,
}

impl Conditions {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// 现在这个前台窗口满足条件吗。
    ///
    /// 一条条件都没写 = 永远满足；写了但一行都没编译成功 = **永远不满足**
    /// （写错了就别悄悄触发，界面上会红字提示）。
    pub fn matches(&self, ctx: &Context) -> bool {
        if self.lines.is_empty() {
            return self.problems.is_empty();
        }
        self.lines.iter().any(|line| line.test(ctx))
    }

    /// 条件里有没有 `mouse.` —— 有才需要去查鼠标下那扇窗口。
    ///
    /// 钩子里每按一个键都要判一次，而取鼠标窗口得付一次 `WindowFromPoint` +
    /// 进程名/标题查询（后者要开进程句柄）。没写 `mouse.` 就别付这个钱。
    pub fn needs_mouse(&self) -> bool {
        self.lines
            .iter()
            .any(|line| line.groups.iter().flatten().any(|c| c.target == Target::Mouse))
    }
}

/// 前台窗口的现场（判断条件时读一次）。
///
/// 前面五个字段是**活动窗口**的（也是 `ctrl_class` 唯一能挂的地方）；
/// `mouse_*` 那四个是**鼠标底下那扇窗口**的，只有条件里写了 `mouse.` 才需要填
/// （见 [`Conditions::needs_mouse`]），没填时是空串。
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// 进程 exe 名（`qq.exe`）
    pub exe: String,
    /// 完整路径
    pub path: String,
    pub title: String,
    /// 前台窗口类名
    pub class: String,
    /// 鼠标光标下那个控件的类名（焦点在网页 / 自绘控件里时特别有用）
    pub ctrl_class: String,
    /// 鼠标底下那扇窗口的进程 exe 名
    pub mouse_exe: String,
    /// 鼠标底下那扇窗口的进程完整路径
    pub mouse_path: String,
    /// 鼠标底下那扇窗口的标题
    pub mouse_title: String,
    /// 鼠标底下那扇窗口的类名
    pub mouse_class: String,
    /// 鼠标底下那扇窗口里、光标所在控件的类名（`mouse.ctrl:`）
    pub mouse_ctrl_class: String,
}

/// 解析一份条件文本（多行）。
pub fn compile(text: &str) -> Conditions {
    let mut out = Conditions::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        match parse_line(line) {
            Ok(line) => out.lines.push(line),
            Err(reason) => out.problems.push(format!(
                "{} {}: {reason}",
                crate::i18n::t("第"),
                index + 1
            )),
        }
    }
    out
}

/// 这一条能不能用（设置界面标红用）。
pub fn problem(text: &str) -> Option<String> {
    compile(text).problems.into_iter().next()
}

/// 一行规则现在的状态（条件编辑器里那一排「哪条命中了」的小胶囊）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineStatus {
    /// 在原文里是第几行（从 1 起 —— 和用户自己数的行号一致）
    pub line: usize,
    /// 原样的那一行
    pub source: String,
    /// 现在这个窗口命中了吗（写错的行永远是 `false`）
    pub hit: bool,
    /// 这一行写错了
    pub problem: Option<String>,
}

/// 逐行给出「现在这个窗口命中哪一条」。
///
/// 配置的时候光看「整体成不成立」不够：写了五行，到底哪一行在管用？
/// 这里一行一行试，界面就能把命中的那几行点亮。
pub fn statuses(text: &str, ctx: &Context) -> Vec<LineStatus> {
    let mut out = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        match parse_line(line) {
            Ok(parsed) => out.push(LineStatus {
                line: index + 1,
                source: line.to_string(),
                hit: parsed.test(ctx),
                problem: None,
            }),
            Err(reason) => out.push(LineStatus {
                line: index + 1,
                source: line.to_string(),
                hit: false,
                problem: Some(reason),
            }),
        }
    }
    out
}

fn parse_line(line: &str) -> Result<Line, String> {
    let mut groups = Vec::new();
    // 分隔符只在「引号 / 正则外面」才算分隔符：窗口标题里逗号很常见
    // （「文档1 - Word, 已保存」），按 `,` 切开就把一条条件变成两条互相「或」的
    // 条件 —— 那结果永远成立，用户会以为条件根本没管用。
    for and_part in split_outside(line, &["&&"]) {
        let and_part = and_part.trim();
        if and_part.is_empty() {
            continue;
        }
        let mut group = Vec::new();
        for or_part in split_outside(and_part, &[",", "，"]) {
            let or_part = or_part.trim();
            if or_part.is_empty() {
                continue;
            }
            group.push(parse_cond(or_part)?);
        }
        if !group.is_empty() {
            groups.push(group);
        }
    }
    if groups.is_empty() {
        return Err(crate::i18n::t("这行没写条件").to_string());
    }
    Ok(Line { groups })
}

/// 按分隔符切，但跳过 `"…"` 和 `/正则/` 里面的内容。
fn split_outside(text: &str, seps: &[&str]) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote = false;
    let mut regex = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if quote {
            current.push(ch);
            if ch == '"' {
                quote = false;
            }
            index += 1;
            continue;
        }
        if regex {
            current.push(ch);
            if ch == '/' {
                regex = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            quote = true;
            current.push(ch);
            index += 1;
            continue;
        }
        // 只有「一段的开头」那个 `/` 才可能是正则的开头（`C:/Tools` 里的不算）
        if ch == '/' && current.trim().is_empty() {
            regex = true;
            current.push(ch);
            index += 1;
            continue;
        }
        let rest: String = chars[index..].iter().collect();
        if let Some(sep) = seps.iter().find(|sep| rest.starts_with(**sep)) {
            out.push(std::mem::take(&mut current));
            index += sep.chars().count();
            continue;
        }
        current.push(ch);
        index += 1;
    }
    out.push(current);
    out
}


fn parse_cond(text: &str) -> Result<Cond, String> {
    let (negate, rest) = match text.strip_prefix('!') {
        Some(rest) => (true, rest.trim()),
        None => (false, text),
    };
    if rest.is_empty() {
        return Err(crate::i18n::t("`!` 后面没写东西").to_string());
    }
    // 前缀（`exe:` 这种）。注意 `C:\…` 里也有冒号 —— 只有**已知前缀**才算，
    // 而且前缀里不能有反斜杠，所以路径不会被误拆。
    let (field, target, value) = split_prefix(rest);
    let value = value.trim();
    if value.is_empty() {
        return Err(crate::i18n::t("条件后面没写值").to_string());
    }
    Ok(Cond {
        field,
        target,
        negate,
        matcher: matcher(value)?,
    })
}

/// 先剥「看哪扇窗口」的 `mouse.` / `鼠标.`，再认字段前缀。
fn split_prefix(text: &str) -> (Field, Target, &str) {
    let lower = text.to_ascii_lowercase();
    for prefix in ["mouse.", "鼠标."] {
        if lower.starts_with(prefix) {
            let (field, value) = split_field(&text[prefix.len()..]);
            return (field, Target::Mouse, value);
        }
    }
    let (field, value) = split_field(text);
    (field, Target::Active, value)
}

fn split_field(text: &str) -> (Field, &str) {
    let Some((head, tail)) = text.split_once(':') else {
        return (bare_field(text), text);
    };
    let head = head.trim().to_ascii_lowercase();
    let field = match head.as_str() {
        "exe" | "app" | "process" | "程序" | "进程" => Field::Exe,
        "path" | "file" | "路径" => Field::Path,
        "title" | "标题" | "窗口" => Field::Title,
        "class" | "类名" => Field::Class,
        "ctrl" | "ctrlclass" | "控件" => Field::CtrlClass,
        _ => return (bare_field(text), text),
    };
    (field, tail)
}

/// 没写前缀时：`xxx.exe` 一眼就是进程名，其余按「哪个命中算哪个」。
fn bare_field(text: &str) -> Field {
    let lower = text.trim().to_ascii_lowercase();
    if lower.ends_with(".exe") {
        Field::Exe
    } else {
        Field::Any
    }
}

fn matcher(value: &str) -> Result<Matcher, String> {
    let value = value.trim();
    // `"…"`：原样比（里面的逗号、`&&`、`*` 都不当语法）。
    // 「插入当前标题」写出来的就是这种形式 —— 标题长什么样都不能被当成规则。
    if let Some(inner) = quoted(value) {
        return same_as(&regex::escape(inner));
    }
    if let Some(exact) = value.strip_prefix('=') {
        return Ok(Matcher::Exact(exact.trim().to_string()));
    }
    let source = if value.len() > 2 && value.starts_with('/') && value.ends_with('/') {
        value[1..value.len() - 1].to_string()
    } else if value.contains('*') || value.contains('?') {
        format!("^{}$", glob_to_regex(value))
    } else {
        regex::escape(value)
    };
    same_as(&source)
}

/// `"…"` 里那一段（本身不是引号包着的就返回 `None`）。
fn quoted(text: &str) -> Option<&str> {
    let len = text.len();
    (len >= 2 && text.starts_with('"') && text.ends_with('"')).then(|| &text[1..len - 1])
}

fn same_as(source: &str) -> Result<Matcher, String> {
    Regex::new(&format!("(?i){source}"))
        .map(Matcher::Pattern)
        .map_err(|err| {
            let text = err.to_string();
            text.lines().next().unwrap_or("正则写错了").to_string()
        })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(exe: &str, title: &str, class: &str, ctrl: &str) -> Context {
        Context {
            exe: exe.to_string(),
            path: format!("C:\\Program Files\\{exe}"),
            title: title.to_string(),
            class: class.to_string(),
            ctrl_class: ctrl.to_string(),
            ..Default::default()
        }
    }

    /// 活动窗口是 QQ，鼠标却停在记事本上 —— 用来区分两套条件。
    fn qq_with_mouse_on_notepad() -> Context {
        let mut ctx = qq();
        ctx.mouse_exe = "notepad.exe".to_string();
        ctx.mouse_path = "C:\\Windows\\notepad.exe".to_string();
        ctx.mouse_title = "未命名 - 记事本".to_string();
        ctx.mouse_class = "Notepad".to_string();
        ctx
    }

    fn qq() -> Context {
        ctx("qq.exe", "聊天 - 张三", "TXGuiFoundation", "Edit")
    }

    #[test]
    fn no_conditions_means_everywhere() {
        let conditions = compile("");
        assert!(conditions.is_empty());
        assert!(conditions.matches(&qq()));
        // 只有注释也等于没写
        assert!(compile("# 只在 QQ 里\n").is_empty());
        assert!(compile("# 只在 QQ 里\n").matches(&qq()));
    }

    #[test]
    fn a_bare_exe_name_matches_the_process() {
        let conditions = compile("QQ.exe");
        assert!(conditions.matches(&qq()));
        assert!(!conditions.matches(&ctx("wechat.exe", "微信", "WeChatMainWndForPC", "")));
    }

    #[test]
    fn a_bare_word_matches_any_field() {
        let conditions = compile("张三");
        assert!(conditions.matches(&qq()), "标题里有「张三」");
        let conditions = compile("TXGui");
        assert!(conditions.matches(&qq()), "类名里有 TXGui");
        let conditions = compile("Button");
        assert!(
            conditions.matches(&ctx("x.exe", "", "", "Button")),
            "光标下控件也算"
        );
    }

    #[test]
    fn lines_are_or_and_ampersand_is_and() {
        // 两行 = 或
        let conditions = compile("QQ.exe\nwechat.exe");
        assert!(conditions.matches(&qq()));
        assert!(conditions.matches(&ctx("wechat.exe", "微信", "", "")));
        assert!(!conditions.matches(&ctx("chrome.exe", "网页", "", "")));

        // 一行 && = 且
        let conditions = compile("QQ.exe && title:聊天");
        assert!(conditions.matches(&qq()));
        assert!(!conditions.matches(&ctx("qq.exe", "登录", "", "")));

        // 逗号 = 或（同「且」里）
        let conditions = compile("title:聊天,title:微信");
        assert!(conditions.matches(&qq()));
    }

    #[test]
    fn bang_negates_one_condition() {
        let conditions = compile("!exe:explorer.exe");
        assert!(conditions.matches(&qq()));
        assert!(!conditions.matches(&ctx("explorer.exe", "此电脑", "", "")));
    }

    #[test]
    fn wildcards_and_exact_values() {
        let mut tencent = ctx("qq.exe", "", "", "");
        tencent.path = "C:\\Program Files\\Tencent\\QQNT\\qq.exe".to_string();
        let conditions = compile("path:*\\Tencent\\*");
        assert!(conditions.matches(&tencent));
        assert!(!conditions.matches(&qq()), "path 只比完整路径");
        // `=` 是全等，不是包含
        let conditions = compile("=qq.exe");
        assert!(conditions.matches(&qq()));
        assert!(!conditions.matches(&ctx("qq.exe.bak", "", "", "")));
    }

    #[test]
    fn a_regex_can_be_written_between_slashes() {
        let conditions = compile("/^\\d+$/");
        assert!(conditions.matches(&ctx("123", "", "", "")));
        assert!(!conditions.matches(&qq()));
    }

    #[test]
    fn a_broken_rule_never_fires_and_says_why() {
        let conditions = compile("/[a-z/");
        assert!(!conditions.matches(&qq()), "写错了就别悄悄触发");
        assert_eq!(conditions.problems().len(), 1);
        assert!(conditions.problems()[0].contains('1'), "要指出第几行");
    }

    #[test]
    fn colons_inside_paths_are_not_prefixes() {
        // `C:\…` 里的冒号前面不是已知前缀，整串按「任意字段包含」比
        let conditions = compile("C:\\Tools");
        let mut tool = ctx("a.exe", "", "", "");
        tool.path = "C:\\Tools\\a.exe".to_string();
        assert!(conditions.matches(&tool), "路径里有 C:\\Tools");
        assert!(!conditions.matches(&qq()));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let conditions = compile("\n  \n# 说明\nQQ.exe\n// 另一行说明\n");
        assert_eq!(conditions.lines.len(), 1);
        assert!(conditions.problems().is_empty());
    }

    #[test]
    fn commas_inside_quotes_are_not_separators() {
        // 窗口标题里逗号很常见；引号里的是文字，不是「或」
        let word = ctx("winword.exe", "文档1 - Word, 已保存", "OpusApp", "");
        let conditions = compile("title:\"文档1 - Word, 已保存\"");
        assert!(conditions.matches(&word));
        assert!(!conditions.matches(&qq()));
    }

    #[test]
    fn quoted_values_are_plain_text_not_patterns() {
        // `*` 在引号里就是星号，不再当通配
        let conditions = compile("title:\"a*b\"");
        assert!(conditions.matches(&ctx("x.exe", "a*b", "", "")));
        assert!(!conditions.matches(&ctx("x.exe", "axxb", "", "")));
        // `&&` 同理：一整条条件，不是两条
        let conditions = compile("title:\"A && B\"");
        assert!(conditions.matches(&ctx("x.exe", "A && B", "", "")));
        assert!(!conditions.matches(&ctx("x.exe", "A", "", "")));
    }

    #[test]
    fn statuses_say_which_line_hits() {
        let list = statuses("QQ.exe\nwechat.exe\n/[a-z/", &qq());
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].line, 1);
        assert!(list[0].hit, "第 1 行 QQ.exe 命中");
        assert!(!list[1].hit, "第 2 行 wechat.exe 不命中");
        assert_eq!(list[2].line, 3);
        assert!(list[2].problem.is_some(), "第 3 行写错了");
        // 注释行不算一条
        assert_eq!(statuses("# 说明\nQQ.exe", &qq()).len(), 1);
    }

    // ---- 活动窗口 vs 鼠标下窗口 ----

    #[test]
    fn plain_prefix_looks_at_the_active_window_only() {
        let both = qq_with_mouse_on_notepad();
        // 不带 `mouse.` 的条件只认活动窗口：鼠标停在记事本上，也不该被记事本命中
        assert!(compile("exe:qq.exe").matches(&both));
        assert!(!compile("exe:notepad.exe").matches(&both));
    }

    #[test]
    fn mouse_prefix_looks_at_the_window_under_the_cursor() {
        let both = qq_with_mouse_on_notepad();
        assert!(compile("mouse.exe:notepad.exe").matches(&both));
        assert!(!compile("mouse.exe:qq.exe").matches(&both));
        // 四个字段都认
        assert!(compile("mouse.title:*记事本*").matches(&both));
        assert!(compile("mouse.class:=Notepad").matches(&both));
        assert!(compile("mouse.path:C:\\Windows\\*").matches(&both));
        // 两种来源能混着写：活动窗口是 QQ 且鼠标停在记事本上
        assert!(compile("exe:qq.exe && mouse.exe:notepad.exe").matches(&both));
        assert!(!compile("exe:qq.exe && mouse.exe:qq.exe").matches(&both));
        // 中文前缀也认
        assert!(compile("鼠标.exe:notepad.exe").matches(&both));
    }

    #[test]
    fn ctrl_class_looks_at_each_side_of_the_pair() {
        // 两边都能写：`ctrl:` 是活动窗口的，`mouse.ctrl:` 是鼠标下窗口的
        let mut both = qq_with_mouse_on_notepad();
        both.ctrl_class = "Edit".to_string();
        both.mouse_ctrl_class = "Button".to_string();
        assert!(compile("ctrl:Edit").matches(&both));
        assert!(!compile("ctrl:Button").matches(&both), "活动窗口那格是 Edit");
        assert!(compile("mouse.ctrl:Button").matches(&both));
        assert!(!compile("mouse.ctrl:Edit").matches(&both), "鼠标窗口那格是 Button");
        // 两种能同时写：焦点在 QQ 的输入框里，鼠标停在记事本的按钮上
        assert!(compile("ctrl:Edit && mouse.ctrl:Button").matches(&both));
        // 不再当写错处理
        assert!(problem("mouse.ctrl:Button").is_none());
    }

    #[test]
    fn needs_mouse_only_when_a_condition_asks_for_it() {
        assert!(!compile("exe:qq.exe\ntitle:聊天").needs_mouse());
        assert!(compile("exe:qq.exe\nmouse.title:记事本").needs_mouse());
        assert!(compile("mouse.class:Notepad").needs_mouse());
        // 鼠标下窗口里的「光标下控件」同样要先取现场
        assert!(compile("mouse.ctrl:Button").needs_mouse());
    }

    #[test]
    fn mouse_conditions_do_not_match_when_the_facts_are_missing() {
        // 没取鼠标现场时（空串）不该命中，免得误触发
        assert!(!compile("mouse.exe:notepad.exe").matches(&qq()));
        // 取反则成立
        assert!(compile("!mouse.exe:notepad.exe").matches(&qq()));
    }
}
