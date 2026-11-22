//! 悬停提示的「内容模板」。
//!
//! 一行一条，占位符写成 `{名字}`，其余字符原样保留：
//!
//! ```text
//! {label}
//! ?{kind} · {children} 项
//! ?触发键  {hotkey}
//! {note}
//! {actions}
//! ```
//!
//! 三条规则（都为了「没内容就别留一行空的」）：
//!
//! 1. 占位符取到的值是空串 → 替换成空，整行**只剩空白**时这一行就不画；
//! 2. 行首（允许前面有空格）写一个 `?` → 这一行**只要有任何一个占位符是空的就整行不画**，
//!    这样「触发键 {hotkey}」不会变成孤零零的「触发键」；
//! 3. `{left}` / `{center}` / `{right}` 写在哪一行，哪一行就单独按它对齐（写的位置会被吃掉）。
//!
//! 第一行按标题样式画（加粗、主色），其余行按正文样式。

use crate::settings::{ActionKind, ItemKind, ItemSetting};

/// 一行的对齐方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TipAlign {
    #[default]
    Left,
    Center,
    Right,
    /// 完全照模板里的空格显示，不做任何对齐
    Custom,
}

impl TipAlign {
    pub const ALL: [TipAlign; 4] = [
        TipAlign::Left,
        TipAlign::Center,
        TipAlign::Right,
        TipAlign::Custom,
    ];

    pub fn label(self) -> &'static str {
        crate::i18n::t(match self {
            TipAlign::Left => "靠左",
            TipAlign::Center => "居中",
            TipAlign::Right => "靠右",
            TipAlign::Custom => "自定义",
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            TipAlign::Left => "left",
            TipAlign::Center => "center",
            TipAlign::Right => "right",
            TipAlign::Custom => "custom",
        }
    }

}

/// 渲染好的一行。
#[derive(Clone, Debug, PartialEq)]
pub struct TipLine {
    pub text: String,
    pub align: TipAlign,
    /// 第一行（标题样式）
    pub title: bool,
}

/// 模板里能用的占位符（名字 + 用途）。设置界面的小圆角按钮就是照这张表画的。
pub const PLACEHOLDERS: &[(&str, &str)] = &[
    ("label", "名称"),
    ("kind", "类型：菜单项 / 子菜单 / 分隔线"),
    ("note", "备注"),
    ("accel", "加速键（那一个字母）"),
    ("shortcut", "快捷键提示"),
    ("hotkey", "触发键（真正注册的那个）"),
    ("icon", "图标位上的字符 / 文件"),
    ("action", "第一条动作"),
    ("actions", "全部动作，每条一行"),
    ("action_count", "动作条数"),
    ("children", "子菜单里有多少项"),
    ("hidden", "这一项被隐藏时显示「在菜单里隐藏」"),
    ("path", "在菜单里的层级：父 › 子"),
    ("id", "内部 id"),
];

/// 渲染用的「这一项有什么」。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TipContext {
    pub label: String,
    pub kind: String,
    pub note: String,
    pub accel: String,
    pub shortcut: String,
    pub hotkey: String,
    pub icon: String,
    /// (类型标签, 参数预览)
    pub actions: Vec<(String, String)>,
    pub children: usize,
    pub enabled: bool,
    pub path: String,
    pub id: String,
}

impl TipContext {
    /// 从一个菜单项凑出一份上下文。`path` 是它在菜单里的层级（父 › 子）。
    pub fn of(item: &ItemSetting, path: &str) -> Self {
        let label = if item.label.trim().is_empty() {
            crate::i18n::t("（未命名）").to_string()
        } else {
            item.label.trim().to_string()
        };
        let actions = item
            .actions
            .iter()
            .map(|step| {
                let value = preview_text(step.value.trim(), if step.kind == ActionKind::Run { 46 } else { 60 });
                (step.kind.label().to_string(), value)
            })
            .collect();
        Self {
            label,
            kind: crate::i18n::t(match item.kind {
                ItemKind::Item => "菜单项",
                ItemKind::Submenu => "子菜单",
                ItemKind::Separator => "分隔线",
            })
            .to_string(),
            note: item.note.trim().to_string(),
            accel: crate::settings::accel_char(&item.accel)
                .map(|ch| ch.to_string())
                .unwrap_or_default(),
            shortcut: item.shortcut.trim().to_string(),
            hotkey: item.hotkey.trim().to_string(),
            icon: item.badge.trim().to_string(),
            actions,
            children: item
                .children
                .iter()
                .filter(|child| child.kind != ItemKind::Separator)
                .count(),
            enabled: item.enabled,
            path: path.to_string(),
            id: item.id.clone(),
        }
    }

    /// 一个占位符的值。`None` = 不认识的占位符（原样留在提示里，方便发现自己写错了）。
    fn value(&self, name: &str) -> Option<String> {
        let text = match name {
            "label" | "name" => self.label.clone(),
            "kind" | "type" => self.kind.clone(),
            "note" => self.note.clone(),
            "accel" => self.accel.clone(),
            "shortcut" => self.shortcut.clone(),
            "hotkey" => self.hotkey.clone(),
            "icon" => self.icon.clone(),
            "action" => self
                .actions
                .first()
                .map(|(kind, preview)| join_action(kind, preview))
                .unwrap_or_default(),
            "actions" => self
                .actions
                .iter()
                .enumerate()
                .map(|(index, (kind, preview))| {
                    format!("{}. {}", index + 1, join_action(kind, preview))
                })
                .collect::<Vec<_>>()
                .join("\n"),
            "action_count" => {
                if self.actions.is_empty() {
                    String::new()
                } else {
                    self.actions.len().to_string()
                }
            }
            "children" => {
                if self.children == 0 {
                    String::new()
                } else {
                    self.children.to_string()
                }
            }
            "hidden" => {
                if self.enabled {
                    String::new()
                } else {
                    crate::i18n::t("在菜单里隐藏").to_string()
                }
            }
            "path" => self.path.clone(),
            "id" => self.id.clone(),
            _ => return None,
        };
        Some(text)
    }
}

fn join_action(kind: &str, preview: &str) -> String {
    if preview.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}  {preview}")
    }
}

/// 动作参数太长的截一段（提示卡一行放不下）。
pub fn preview_text(value: &str, max: usize) -> String {
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// 默认模板：把以前那几个开关的效果原样拼回来。
pub const DEFAULT_TEMPLATE: &str = "\
{label}
?{kind} · {children} 项
?加速键  {accel}
?触发键  {hotkey}
?快捷键  {shortcut}
?{hidden}
{note}
{actions}";

/// 按模板渲染出要画的几行。
pub fn render(template: &str, ctx: &TipContext, default_align: TipAlign) -> Vec<TipLine> {
    let template = if template.trim().is_empty() {
        DEFAULT_TEMPLATE
    } else {
        template
    };

    let mut lines: Vec<TipLine> = Vec::new();
    for raw in template.split('\n') {
        let mut text = raw.trim_end().to_string();

        // 行首的条件标记
        let trimmed = text.trim_start();
        let conditional = trimmed.starts_with('?');
        if conditional {
            let indent = text.len() - trimmed.len();
            text = format!("{}{}", " ".repeat(indent), &trimmed[1..]);
        }

        // 这一行自己的对齐
        let mut align = None;
        for (token, value) in [
            ("{left}", TipAlign::Left),
            ("{center}", TipAlign::Center),
            ("{right}", TipAlign::Right),
        ] {
            if text.contains(token) {
                align = Some(value);
                text = text.replace(token, "");
            }
        }

        let (filled, missing) = substitute(&text, ctx);
        if conditional && missing {
            continue;
        }
        for part in filled.split('\n') {
            let part = part.trim_end();
            if part.trim().is_empty() {
                continue;
            }
            lines.push(TipLine {
                text: part.to_string(),
                align: align.unwrap_or(default_align),
                title: lines.is_empty(),
            });
        }
    }
    lines
}

/// 换掉一行里的 `{占位符}`；返回值 (结果, 有没有占位符取到空值)。
fn substitute(line: &str, ctx: &TipContext) -> (String, bool) {
    let mut out = String::with_capacity(line.len());
    let mut missing = false;
    let mut rest = line;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return (out, missing);
        };
        let name = after[..end].trim();
        match ctx.value(name) {
            Some(value) => {
                if value.is_empty() {
                    missing = true;
                }
                out.push_str(&value);
            }
            None => {
                // 不认识的占位符：原样留着（写错了能一眼看出来）
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    (out, missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ActionStep;

    fn ctx() -> TipContext {
        let mut item = ItemSetting::item("a".to_string(), "记事本");
        item.note = "常用的".to_string();
        item.shortcut = "Ctrl+N".to_string();
        item.hotkey = "Ctrl+Alt+N".to_string();
        item.accel = "N".to_string();
        item.actions = vec![ActionStep::new(ActionKind::Run, "notepad.exe")];
        TipContext::of(&item, "")
    }

    #[test]
    fn placeholders_get_filled() {
        let lines = render("{label}\n?触发键  {hotkey}\n{note}", &ctx(), TipAlign::Left);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "记事本");
        assert!(lines[0].title);
        assert_eq!(lines[1].text, "触发键  Ctrl+Alt+N");
        assert_eq!(lines[2].text, "常用的");
    }

    #[test]
    fn a_question_mark_drops_the_whole_line() {
        // 加速键没填：这一行整行不画（不是画成孤零零的「加速键」）
        let mut context = ctx();
        context.accel.clear();
        let lines = render("{label}\n?加速键  {accel}\n?{kind} · {children} 项", &context, TipAlign::Left);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "记事本");
    }

    #[test]
    fn empty_placeholders_leave_no_blank_lines() {
        let mut context = ctx();
        context.note.clear();
        let lines = render("{label}\n{note}\n{shortcut}", &context, TipAlign::Left);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "Ctrl+N");
    }

    #[test]
    fn actions_take_one_line_each() {
        let mut context = ctx();
        context.actions.push(("打开网址".to_string(), "https://gpui.rs".to_string()));
        let lines = render("{label}\n{actions}", &context, TipAlign::Left);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].text.starts_with("1. 运行程序"));
        assert!(lines[2].text.starts_with("2. 打开网址"));
    }

    #[test]
    fn alignment_can_be_set_per_line() {
        let lines = render("{center}{label}\n{right}{hotkey}\n{note}", &ctx(), TipAlign::Left);
        assert_eq!(lines[0].align, TipAlign::Center);
        assert_eq!(lines[0].text, "记事本");
        assert_eq!(lines[1].align, TipAlign::Right);
        // 没写标记的行用全局对齐
        assert_eq!(lines[2].align, TipAlign::Left);
    }

    #[test]
    fn unknown_placeholders_stay_visible() {
        let lines = render("{label} {typo}", &ctx(), TipAlign::Left);
        assert_eq!(lines[0].text, "记事本 {typo}");
    }

    #[test]
    fn an_empty_template_falls_back_to_the_default() {
        let lines = render("   ", &ctx(), TipAlign::Left);
        assert!(!lines.is_empty());
        assert_eq!(lines[0].text, "记事本");
    }
}
