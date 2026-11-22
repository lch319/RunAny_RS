//! 菜单数据模型 + 示例菜单。
//!
//! 自定义菜单只需要改 `example_menu()` 这一个函数，其余代码无需改动。

use gpui::SharedString;

/// 一个菜单条目：普通项或分隔线。
#[derive(Clone)]
pub enum MenuEntry {
    Item(MenuItem),
    Separator,
    /// 多列符：这一层从这里另起一列（同一张卡片里并排放几列）
    ColumnBreak,
}

impl MenuEntry {
    /// 是否可以被键盘/鼠标选中（分隔线、多列符不可选）。
    pub fn is_selectable(&self) -> bool {
        matches!(self, MenuEntry::Item(_))
    }

    pub fn item(&self) -> Option<&MenuItem> {
        match self {
            MenuEntry::Item(item) => Some(item),
            MenuEntry::Separator | MenuEntry::ColumnBreak => None,
        }
    }
}

/// 菜单项。用 builder 风格构造，链式调用即可。
#[derive(Clone)]
pub struct MenuItem {
    /// 稳定标识，选中时回传给业务逻辑（设置窗口里可以动态改，所以不是 `&'static str`）
    pub id: SharedString,
    /// 显示文本
    pub label: SharedString,
    /// 左侧小图标位：一个字符 / emoji，或者一个图片文件名（`.svg` / `.ico` / `.png` …）
    pub badge: Option<SharedString>,
    /// 图标位后面的「数字 / 字母加速键」：菜单打开时按这个键直接触发
    pub accel: Option<SharedString>,
    /// 右侧快捷键提示（纯展示，不参与按键匹配）
    pub shortcut: Option<SharedString>,
    /// 备注：显示在名称右侧的灰色小字（给菜单项写点说明）
    pub note: Option<SharedString>,
    /// 子菜单；不为空时该项右侧显示箭头，并可以级联展开
    pub children: Vec<MenuEntry>,
    /// 点击后要依次执行的动作（由设置里的菜单项带过来；子菜单为空）
    pub actions: Vec<crate::settings::ActionStep>,
}

impl MenuItem {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        // 菜单项名称是存盘的 `String`，用 `t_dynamic`：表里有的（默认模板那几条）
        // 跟着界面语言走，用户自己起的名字原样显示
        let label: SharedString = label.into();
        let label: SharedString = crate::i18n::t_dynamic(label.as_ref()).to_string().into();
        Self {
            id: id.into(),
            label,
            badge: None,
            accel: None,
            shortcut: None,
            note: None,
            children: Vec::new(),
            actions: Vec::new(),
        }
    }

    /// 备注（名称右边的灰色小字）。
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// 右侧的快捷键提示文案，例如 `Ctrl+N`。
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// 左侧的小图标位：一个字符（例如 "+"、"★"），或一个图片文件路径。
    pub fn badge(mut self, badge: impl Into<SharedString>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// 图标位后面的加速键（一个数字或字母）。
    pub fn accel(mut self, accel: impl Into<SharedString>) -> Self {
        self.accel = Some(accel.into());
        self
    }

    /// 挂一个子菜单。
    pub fn submenu(mut self, children: Vec<MenuEntry>) -> Self {
        self.children = children;
        self
    }

    pub fn has_children(&self) -> bool {
        !self.children.is_empty()
    }
}

/// 分隔线的简写。
pub fn sep() -> MenuEntry {
    MenuEntry::Separator
}

/// 菜单项简写。
pub fn item(id: impl Into<SharedString>, label: impl Into<SharedString>) -> MenuEntry {
    MenuEntry::Item(MenuItem::new(id, label))
}

/// 压测用菜单：生成 `count` 条（每 12 条一个分隔线，每 17 条给一个带子菜单的项）。
/// 用环境变量 `GPUI_MENU_ITEMS=2000` 触发，见 `state::sample_menu()`。
pub fn demo_menu(count: usize) -> Vec<MenuEntry> {
    let mut entries = Vec::with_capacity(count + count / 12);
    for i in 0..count {
        if i > 0 && i % 12 == 0 {
            entries.push(sep());
        }
        if i % 17 == 0 {
            entries.push(MenuEntry::Item(
                MenuItem::new(format!("demo-sub-{i}"), format!("第 {i} 项 · 有子菜单"))
                    .shortcut("Ctrl+Shift+F")
                    .submenu(vec![
                        item("sub-a", "子项 A"),
                        item("sub-b", "子项 B"),
                        sep(),
                        item("sub-c", "子项 C"),
                    ]),
            ));
        } else {
            entries.push(MenuEntry::Item(
                MenuItem::new(format!("demo-{i}"), format!("第 {i} 项 · 示例菜单项"))
                    .shortcut("Ctrl+Alt+Shift+P"),
            ));
        }
    }
    entries
}
