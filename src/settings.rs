//! 菜单设置：主题、尺寸、菜单项（含动作），落盘到 exe 同目录的 `settings.json`（纯便携）。
//!
//! 设置只有一份真相（`MenuSettings`），三处读它：
//! 1. `layout` / `theme` 读尺寸与配色（无 cx，走线程本地 `CURRENT`）；
//! 2. `state::sample_menu()` 读菜单项，弹出时现场构建菜单树；
//! 3. 设置窗口 / 条目编辑窗口读它、改它（`update()` 改完自动重绘 + 落盘）。
//!
//! 放在线程本地而不是 `Global`：布局/取色函数到处都在调，每次都 `cx.global()` 拿不到
//! （它们是纯函数，没有 cx）。GPUI 的渲染全在主线程，线程本地足够安全。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use gpui::{App, AppContext, Entity, Global};
use serde::{Deserialize, Serialize};

use crate::menu::{MenuEntry, MenuItem};
use crate::selection::{Context, MultiFileRule, SelectionKind};
use crate::theme::{self, ThemeColors};
use crate::tip_template::TipAlign;

/// 默认的弹出菜单快捷键。
pub const DEFAULT_POPUP_HOTKEY: &str = "Ctrl+Alt+M";

/// 「托盘右键菜单」在菜单树里的固定 id。
///
/// 它就是**根层的一个普通子菜单**，只是弹出菜单时会被跳过 —— 这样它能用现成的
/// 树形编辑器（拖拽、增删、动作、图标）来改，而不用为托盘再写一套配置结构。
pub const TRAY_ID: &str = "__tray__";

/// 一键直达的「直达项」寄居的那一支（**不出现在菜单里**）。
///
/// 规则要绑的目标必须是「真的菜单项」，才能白捡整套动作编辑器 / 子菜单 / 图标；
/// 但它又不该混进用户自己的菜单。所以给它一个隐藏根层：菜单构建、树编辑器、
/// 「按名字找菜单项」全都跳过它 —— 用户只在「一键直达」页里看得见里面的东西。
pub const MAGIC_ID: &str = "__magic__";

/// 这两支「寄居」在内存树的根层，但它们不是菜单的一部分：
/// 托盘右键菜单（`TRAY_ID`）和 一键直达的直达项（`MAGIC_ID`）。
fn is_hidden_branch(id: &str) -> bool {
    id == TRAY_ID || id == MAGIC_ID
}

/// 某个 id 是不是那两支隐藏分支（界面判断「这是特殊项」用）。
pub fn is_hidden_item(id: &str) -> bool {
    is_hidden_branch(id)
}

/// 一条热字符串（引擎编译规则、提示卡片显示名称都用它）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotstringPhrase {
    /// 挂在哪个菜单项上
    pub id: String,
    /// 那个菜单项的文案
    pub label: String,
    /// 要敲的那串字
    pub text: String,
    /// 一匹配就执行、不等结束字符（AHK 的 `*`）
    pub now: bool,
}

/// 收集热字符串（递归，跳过托盘那一支和禁用项）。
fn collect_hotstrings(items: &[ItemSetting], out: &mut Vec<HotstringPhrase>) {
    for item in items {
        if item.id == TRAY_ID || item.kind == ItemKind::Separator || !item.enabled {
            continue;
        }
        let text = item.hotstring.trim();
        if !text.is_empty() {
            out.push(HotstringPhrase {
                id: item.id.clone(),
                label: item.label.clone(),
                text: text.to_string(),
                now: item.hotstring_now,
            });
        }
        collect_hotstrings(&item.children, out);
    }
}

/// 托盘图标左键点一下做什么。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrayLeftClick {
    /// 弹出完整菜单（默认）
    #[default]
    PopupMenu,
    /// 弹出托盘右键菜单
    TrayMenu,
    /// 什么都不做
    Nothing,
}

impl TrayLeftClick {
    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            TrayLeftClick::PopupMenu => "弹出完整菜单",
            TrayLeftClick::TrayMenu => "弹出托盘菜单",
            TrayLeftClick::Nothing => "什么都不做",
        })
    }
}

/// 「显示全部菜单」那一项放顶部还是底部。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgePlacement {
    Top,
    #[default]
    Bottom,
}

impl EdgePlacement {
    pub fn label(self) -> &'static str {
        crate::i18n::t(match self {
            EdgePlacement::Top => "顶部",
            EdgePlacement::Bottom => "底部",
        })
    }
}

/// 弹出菜单里那个搜索框摆在哪儿（`Off` = 不放）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SearchPlace {
    /// 不放搜索框（菜单和以前一样）
    #[default]
    Off,
    /// 第一列顶上一条
    Top,
    /// 第一列底上一条
    Bottom,
}

impl SearchPlace {
    pub const ALL: [SearchPlace; 3] = [SearchPlace::Off, SearchPlace::Top, SearchPlace::Bottom];

    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            SearchPlace::Off => "不要",
            SearchPlace::Top => "顶部",
            SearchPlace::Bottom => "底部",
        })
    }

    pub fn enabled(self) -> bool {
        self != SearchPlace::Off
    }
}

/// 列宽怎么来。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PanelWidthMode {
    /// 所有列都用「列宽」那个固定值
    Fixed,
    /// 按这一列里最长的菜单项算，超过「最大列宽」就截住
    #[default]
    Auto,
}

impl PanelWidthMode {
    pub const ALL: [PanelWidthMode; 2] = [PanelWidthMode::Fixed, PanelWidthMode::Auto];

    pub fn label(self) -> &'static str {
        crate::i18n::t(match self {
            PanelWidthMode::Fixed => "固定",
            PanelWidthMode::Auto => "按内容",
        })
    }
}

/// 子菜单从哪条边展开。
///
/// 这里有个真实的取舍：贴在父项名字旁边最好找，但多列卡片里会**盖住右边那几列**
/// —— 被盖住的行鼠标点不到、也悬停不到（针穿过不去，Windows 只把鼠标消息发给
/// 最上面那个窗口）。所以默认「智能」：单列时贴名字，多列时贴整张卡片的右边缘。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubmenuAnchor {
    /// 旧配置里的「智能」（多列时贴卡片）。现在已经并进「贴名字」：
    /// 一直保留这个变体只是为了让老配置还能正常读进来，不再出现在设置页里。
    Smart,
    /// 永远贴父项那一列的右边缘（默认：子菜单紧挨着名字，手感跟原版一致）
    #[default]
    Name,
    /// 永远贴整张卡片的右边缘
    Card,
}

impl SubmenuAnchor {
    pub const ALL: [SubmenuAnchor; 2] = [SubmenuAnchor::Name, SubmenuAnchor::Card];

    pub fn label(self) -> &'static str {
        crate::i18n::t(match self {
            SubmenuAnchor::Smart => "智能",
            SubmenuAnchor::Name => "贴名字",
            SubmenuAnchor::Card => "贴卡片",
        })
    }

    /// 要不要贴名字（父项那一列的边缘）。
    ///
    /// 只有「贴卡片」是 false。老的「智能」也走贴名字 —— 用户实测多列贴卡片时
    /// 子菜单离父项隔着一整列，根本对不上，明确要求改回去。
    pub fn hugs_name(self) -> bool {
        !matches!(self, SubmenuAnchor::Card)
    }
}

/// 读 `contexts`：认不出来的名字直接丢掉，不让一个手改错的值把整份配置顶掉。
/// 老配置里的 `video` / `audio` / `document` / `code` / `archive` 都会读成「文件」，
/// 所以这里还要去个重（那几个可能同时出现）。
fn de_contexts<'de, D>(deserializer: D) -> Result<Vec<SelectionKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<String>::deserialize(deserializer)?;
    let mut out: Vec<SelectionKind> = Vec::new();
    for key in &raw {
        if let Some(kind) = SelectionKind::from_key(key) {
            if !out.contains(&kind) {
                out.push(kind);
            }
        }
    }
    Ok(out)
}

/// 主题模式：跟随系统 / 固定浅色 / 固定深色。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    FollowSystem,
    Light,
    Dark,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            ThemeMode::FollowSystem => "跟随系统",
            ThemeMode::Light => "固定浅色",
            ThemeMode::Dark => "固定深色",
        })
    }
}

/// 枚举不出来时用的兜底字体（非 Windows 平台 / GDI 调用失败）。
const FONT_FALLBACK: [&str; 5] = [
    "Microsoft YaHei UI",
    "SimSun",
    "SimHei",
    "Consolas",
    "Segoe UI",
];

/// 系统里装了的全部字体名（排序去重，枚举一次后缓存）。
///
/// 「系统默认」（空串）不在这里，界面列表自己补在最前面。
pub fn available_fonts() -> &'static [String] {
    static FONTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        let mut list = crate::win32::font_families();
        if list.is_empty() {
            list = FONT_FALLBACK.iter().map(|name| name.to_string()).collect();
        }
        list.sort_by_key(|name| name.to_lowercase());
        list.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        list
    })
}

/// 菜单字体名（空串用系统 UI 字体）。
pub fn font_family_name() -> String {
    read(|settings| settings.font_family.clone())
}

/// 菜单项类型。
///
/// - `Item`：可以点、可以配动作；
/// - `Submenu`：只负责“展开一层”，不配动作（只有名称 / 图标 / 快捷键提示）；
/// - `Separator`：分隔线。
///
/// `Item` 和 `Submenu` 的边界不靠人记：任何一项有了子项就自动变 `Submenu`，
/// 子项被搬空了就自动退回 `Item`（见 `normalize`），拖拽怎么折腾都不会出现
/// “既想执行动作、又想展开子菜单”的矛盾状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    #[default]
    Item,
    Submenu,
    Separator,
}

/// 一条动作的类型。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionKind {
    /// 跑一条命令（交给 shell 执行）
    #[default]
    Run,
    /// 打开网址 / 文件
    Url,
    /// 发送按键（`Ctrl+S`，多个用空格分隔）
    Keys,
    /// 发送一段文本（走 Unicode 输入，不碰剪贴板）
    Text,
    /// 内置动作（打开设置、退出…；目录见 `crate::actions`）
    Builtin,
    /// 跑一段 AutoHotkey 脚本（v1 / v2 由参数 `lang` 决定）
    Script,
}

impl ActionKind {
    pub const ALL: [ActionKind; 6] = [
        ActionKind::Run,
        ActionKind::Url,
        ActionKind::Keys,
        ActionKind::Text,
        ActionKind::Builtin,
        ActionKind::Script,
    ];

    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            ActionKind::Run => "运行程序",
            ActionKind::Url => "打开网址",
            ActionKind::Keys => "发送按键",
            ActionKind::Text => "发送文本",
            ActionKind::Builtin => "内置动作",
            ActionKind::Script => "运行脚本",
        })
    }

    pub fn hint(self) -> &'static str {
        crate::i18n::t(
        match self {
            ActionKind::Run => {
                "例如 notepad.exe，或一条完整的 shell 命令；支持占位符：{selection} / {env:TEMP} / {ahkvar:A_YYYY}"
            }
            ActionKind::Url => "例如 https://gpui.rs，或某个文件路径",
            ActionKind::Keys => "例如 Ctrl+S，连按多个用空格隔开：Ctrl+A Delete",
            ActionKind::Text => "直接当键盘输入打出去，支持中文和占位符（{selection} / {clipboard} / {ahkvar:A_Now}）；要按回车 / Tab / 退格就写 {Enter} / {Tab} / {Backspace}，打大括号本身写 {{ 和 }}",
            ActionKind::Builtin => "从动作库里挑一个；带参数的可以在这里改",
            ActionKind::Script => "AutoHotkey v1 / v2 代码，直接在这里写（也可以先粘过来）",
        })
    }

    /// 这条动作需不需要先等焦点回到原来的窗口（按键 / 文本）。
    pub fn needs_focus_back(self) -> bool {
        matches!(self, ActionKind::Keys | ActionKind::Text)
    }
}

/// 内置动作：不需要用户填参数，选一下就行。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BuiltinAction {
    #[default]
    OpenSettings,
    OpenConfigDir,
    /// 设置窗口的「关于」页（快捷键 / 版本 / 配置路径都写在那儿）
    About,
    ReloadMenu,
    Quit,
    /// 退出再自己拉起来（改了托盘图标、热键这类「只有启动时才读」的设置后不用手动开）
    Restart,
    CloseMenu,
    /// 把「弹出菜单前抓到的选中内容」复制回剪贴板（菜单第一项用它）
    CopySelection,
    /// 弹出完整菜单（托盘右键菜单里那一项用它）
    OpenMenu,
}

impl BuiltinAction {
    pub const ALL: [BuiltinAction; 9] = [
        BuiltinAction::OpenSettings,
        BuiltinAction::OpenConfigDir,
        BuiltinAction::About,
        BuiltinAction::ReloadMenu,
        BuiltinAction::OpenMenu,
        BuiltinAction::Restart,
        BuiltinAction::Quit,
        BuiltinAction::CloseMenu,
        BuiltinAction::CopySelection,
    ];

    pub fn key(self) -> &'static str {
        match self {
            BuiltinAction::OpenSettings => "open-settings",
            BuiltinAction::OpenConfigDir => "open-config-dir",
            BuiltinAction::About => "about",
            BuiltinAction::ReloadMenu => "reload-menu",
            BuiltinAction::OpenMenu => "open-menu",
            BuiltinAction::Restart => "restart",
            BuiltinAction::Quit => "quit",
            BuiltinAction::CloseMenu => "close-menu",
            BuiltinAction::CopySelection => "copy-selection",
        }
    }

    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            BuiltinAction::OpenSettings => "打开设置窗口",
            BuiltinAction::OpenConfigDir => "打开配置目录",
            BuiltinAction::About => "打开关于",
            BuiltinAction::ReloadMenu => "重新加载菜单",
            BuiltinAction::OpenMenu => "弹出完整菜单",
            BuiltinAction::Restart => "重启软件",
            BuiltinAction::Quit => "退出程序",
            BuiltinAction::CloseMenu => "只关闭菜单",
            BuiltinAction::CopySelection => "复制选中内容",
        })
    }

    pub fn from_key(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.key() == value)
    }
}

/// 一条动作：类型 + 参数 + **执行完这条之后等多少毫秒**再走下一条。
///
/// 多条动作按数组顺序依次执行，`delay_ms` 挂在“这条之后”，
/// 所以「复制 → 等 200ms → 粘贴」就是两条动作、前一条 delay=200。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionStep {
    pub kind: ActionKind,
    pub value: String,
    pub delay_ms: u32,
    /// 内置动作 / 脚本的参数：`键 -> 值`（键见 `crate::actions` 里那条动作的 `params`）。
    ///
    /// 单独一个字段而不是塞进 `value`：参数是「可有可无、可增可减」的，
    /// 拼进字符串以后改一个参数就得解析一次，加参数还得保证旧值不串位。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub params: std::collections::BTreeMap<String, String>,
    /// 「发送文本」：认不认 `{Enter}` `{Tab}` 这类按键记号（`None` = 认）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_escapes: Option<bool>,
    /// 「发送文本」：用剪贴板粘贴（`None` = 直接注入）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_paste: Option<bool>,
}

impl Default for ActionStep {
    fn default() -> Self {
        Self {
            kind: ActionKind::Run,
            value: String::new(),
            delay_ms: 0,
            params: std::collections::BTreeMap::new(),
            text_escapes: None,
            text_paste: None,
        }
    }
}

impl ActionStep {
    pub fn new(kind: ActionKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
            delay_ms: 0,
            params: std::collections::BTreeMap::new(),
            text_escapes: None,
            text_paste: None,
        }
    }

    /// 带参数的一条动作（动作库里「添加到菜单」用）。
    pub fn with_params(
        kind: ActionKind,
        value: impl Into<String>,
        params: std::collections::BTreeMap<String, String>,
    ) -> Self {
        Self {
            kind,
            value: value.into(),
            delay_ms: 0,
            params,
            text_escapes: None,
            text_paste: None,
        }
    }

    /// 某个参数的值（没有就用默认值）。
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }

    pub fn delay(mut self, delay_ms: u32) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    /// 参数是不是填全了（内置动作看值能不能认出来）。
    pub fn is_ready(&self) -> bool {
        match self.kind {
            ActionKind::Builtin => crate::actions::is_known(&self.value),
            _ => !self.value.trim().is_empty(),
        }
    }

    /// 列表里显示的短标签，例如 `运行程序 · notepad.exe`。
    pub fn summary(&self) -> String {
        if self.kind == ActionKind::Builtin {
            let label = crate::actions::find(&self.value)
                .map(|def| crate::i18n::t(def.name).to_string())
                .or_else(|| {
                    BuiltinAction::from_key(&self.value).map(|action| action.label().to_string())
                })
                .unwrap_or_else(|| crate::i18n::t("（未选择）").to_string());
            // 带参数的把参数也带上，列表里一眼能看出「调大 2 下」和「调小 2 下」
            let detail: Vec<String> = self
                .params
                .iter()
                .filter(|(_, value)| !value.trim().is_empty())
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            return if detail.is_empty() {
                format!("{} · {}", self.kind.label(), label)
            } else {
                format!("{} · {}（{}）", self.kind.label(), label, detail.join(" "))
            };
        }
        let value = self.value.trim();
        if value.is_empty() {
            self.kind.label().to_string()
        } else {
            format!("{} · {}", self.kind.label(), value)
        }
    }
}

/// 旧版本（单条动作）的 JSON，只用于迁移。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct LegacyAction {
    kind: LegacyKind,
    value: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
enum LegacyKind {
    #[default]
    None,
    Command,
    Url,
}

/// 一个菜单项（或子菜单）在「按选中内容筛选」里算什么类型。
///
/// 三态是这次需求的核心：子菜单的筛选是**全局**的（这一支都归它管），
/// 菜单项默认**跟随**上级；只有「单独指定」的才是局部覆盖，会浮到根菜单去。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextMode {
    /// 跟随上一级。根层没有上一级 = 通用项（有选中内容时隐藏）
    #[default]
    Inherit,
    /// 自己指定（看 `contexts`）
    Own,
    /// 不参与筛选：就算上级命中了，有选中内容时这一项也不出现
    Never,
}

impl ContextMode {
    pub fn is_inherit(&self) -> bool {
        *self == ContextMode::Inherit
    }
}

/// 「运行程序」动作里那几个附加参数（都放在 `ActionStep::params` 里）。
///
/// 单独一个模块是因为三处都要用同一套 key 和文案：编辑窗口的「运行选项」面板、
/// 动作摘要那一行、以及真正执行时的分叉（`crate::runopts`）。
pub mod run_opts {
    /// 以管理员身份运行（`"1"`）
    pub const RUNAS: &str = "runas";
    /// 程序已经在跑的时候怎么办（见 [`OnRun`]）
    pub const ONRUN: &str = "onrun";
    /// `onrun = keys` 时要发的那串键
    pub const ONRUN_KEYS: &str = "onrun_keys";
    /// 追加到命令行后面的参数（和「程序」分开填，改的时候不用动那串路径）
    pub const ARGS: &str = "args";
    /// 工作文件夹
    pub const WORKDIR: &str = "workdir";
    /// 窗口状态（见 [`WindowState`]）
    pub const WINDOW: &str = "window";

    /// 程序已经在运行时怎么办。
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum OnRun {
        /// 保存之前的逻辑：再起一个（就是原来那样）
        #[default]
        Again,
        /// 激活窗口；已经在前台就最小化
        Toggle,
        /// 关掉它
        Close,
        /// 激活窗口，然后发一串按键（比如 `Ctrl+S`）
        Keys,
    }

    impl OnRun {
        pub const ALL: [OnRun; 4] = [OnRun::Again, OnRun::Toggle, OnRun::Close, OnRun::Keys];

        pub fn key(self) -> &'static str {
            match self {
                OnRun::Again => "again",
                OnRun::Toggle => "toggle",
                OnRun::Close => "close",
                OnRun::Keys => "keys",
            }
        }

        pub fn from_key(text: &str) -> OnRun {
            match text.trim().to_ascii_lowercase().as_str() {
                "toggle" | "activate" => OnRun::Toggle,
                "close" | "kill" => OnRun::Close,
                "keys" | "send" => OnRun::Keys,
                _ => OnRun::Again,
            }
        }

        pub fn label(self) -> &'static str {
            crate::i18n::t(match self {
                OnRun::Again => "再启动一次（默认）",
                OnRun::Toggle => "激活 / 最小化窗口",
                OnRun::Close => "关掉它",
                OnRun::Keys => "激活后发送按键",
            })
        }
    }

    /// 启动时的窗口状态。
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum WindowState {
        #[default]
        Normal,
        Minimized,
        Maximized,
    }

    impl WindowState {
        pub const ALL: [WindowState; 3] = [
            WindowState::Normal,
            WindowState::Minimized,
            WindowState::Maximized,
        ];

        pub fn key(self) -> &'static str {
            match self {
                WindowState::Normal => "normal",
                WindowState::Minimized => "min",
                WindowState::Maximized => "max",
            }
        }

        pub fn from_key(text: &str) -> WindowState {
            match text.trim().to_ascii_lowercase().as_str() {
                "min" | "minimized" => WindowState::Minimized,
                "max" | "maximized" => WindowState::Maximized,
                _ => WindowState::Normal,
            }
        }

        pub fn label(self) -> &'static str {
            crate::i18n::t(match self {
                WindowState::Normal => "正常",
                WindowState::Minimized => "最小化",
                WindowState::Maximized => "最大化",
            })
        }
    }
}


/// 「单独指定」的普通项会从原来那一层浮到根菜单，浮上去以后放哪儿。
///
/// 只有「在子菜单里、又单独指定了类型」的普通项才会浮动（根层的项本来就在根层），
/// 所以这个选项只在那时才有意义。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FloatPlacement {
    /// 根菜单最前面（最早的行为）
    #[default]
    Top,
    /// 按它在整棵菜单树里的原始顺序插回去
    Order,
    /// 放在所有子菜单前面（没有子菜单就等同于最前面）
    BeforeSubmenus,
    /// 放在所有子菜单后面（没有子菜单就等同于最后面）
    AfterSubmenus,
}

impl FloatPlacement {
    pub const ALL: [FloatPlacement; 4] = [
        FloatPlacement::Top,
        FloatPlacement::Order,
        FloatPlacement::BeforeSubmenus,
        FloatPlacement::AfterSubmenus,
    ];

    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            FloatPlacement::Top => "最前面",
            FloatPlacement::Order => "按原顺序",
            FloatPlacement::BeforeSubmenus => "子菜单前",
            FloatPlacement::AfterSubmenus => "子菜单后",
        })
    }

    /// 界面元素 id 用（不跟着中文文案变）。
    pub fn key(self) -> &'static str {
        match self {
            FloatPlacement::Top => "top",
            FloatPlacement::Order => "order",
            FloatPlacement::BeforeSubmenus => "before-submenus",
            FloatPlacement::AfterSubmenus => "after-submenus",
        }
    }

    pub fn hint(self) -> &'static str {
        crate::i18n::t(
        match self {
            FloatPlacement::Top => "浮到根菜单最前面",
            FloatPlacement::Order => "记住它原来在树里的位置，插回那一带",
            FloatPlacement::BeforeSubmenus => "排在根菜单所有子菜单之前",
            FloatPlacement::AfterSubmenus => "排在根菜单所有子菜单之后",
        })
    }

    /// serde 用：默认值不写进 JSON。
    pub fn is_top(&self) -> bool {
        *self == FloatPlacement::Top
    }
}

/// 一条可编辑的菜单项。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemSetting {    pub kind: ItemKind,
    pub id: String,
    pub label: String,
    /// 左侧小图标位：一个字符 / emoji，或一个图片文件（`.svg` / `.ico` / `.png` …）
    pub badge: String,
    /// 图标位后面的加速键（一个数字或字母，空串 = 不显示也不响应）
    pub accel: String,
    /// 右侧快捷键提示（只是展示）
    pub shortcut: String,
    /// 真正的触发快捷键（`Ctrl+Alt+1` 这种组合）：在**任何程序里**按下就直接执行 /
    /// 呼出这一支，空串表示不注册。和 `shortcut` 分开，因为提示文字常常只是
    /// 「Ctrl+N」这种给用户看的说明，不能真去抢系统的 Ctrl+N。
    pub hotkey: String,
    /// 这个触发键要不要**区分左右修饰键**：开着时 `RAlt+0` 只认右边那颗 Alt。
    ///
    /// 放在这一项自己身上、而不是一个全局开关：弹菜单那一颗通常希望左右都能按，
    /// 某个项又可能只想认左边 —— 一刀切永远有一半人不满意。
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub hotkey_sided: bool,
    /// **局部生效**的触发键：只在满足 `hotkey_local_when` 的窗口里才起作用。
    ///
    /// 和上面那个 `hotkey`（全局）是**两颗独立**的键：一个菜单项可以既有一个
    /// 到处都能按的全局键，又有一个只在某个程序里能按的局部键（AHK 的 `#If`）。
    /// 空串 = 没配。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hotkey_local: String,
    /// 局部键要不要区分左右修饰键
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub hotkey_local_sided: bool,
    /// 局部键的**生效条件**：一行一条，空串 = 到处生效（那它就等于第二颗全局键）。
    ///
    /// 写法见 `when.rs`：`QQ.exe` / `title:聊天` / `!exe:explorer.exe` / `ctrl:Button`，
    /// 行与行之间是「或」，行内用 `&&` 是「且」、用逗号是「或」。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hotkey_local_when: String,
    /// 老版本的「触发键 + 生效条件」二合一写法。读盘时在 `migrate()` 里搬进
    /// `hotkey_local` / `hotkey_local_when`，所以这里只读不写。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hotkey_when: String,
    /// 热字符串：在任何程序里敲完这串字（再敲一个结束字符）就执行这一项。
    ///
    /// 和 `hotkey` 是两套入口：`hotkey` 是组合键（Ctrl+Alt+1），这个是**直接打字**。
    /// 空串 = 没配。子菜单也能配（敲完弹那一支）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hotstring: String,
    /// 这一项的热字符串「一匹配就执行，不等结束字符」（AHK 的 `*`）。
    ///
    /// **默认开**：敲完最后一个字母立刻执行，不用再补空格 / 回车。
    /// 全局那个开关已经删了（设置页不再有），只留这一处，免得两个开关打架。
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub hotstring_now: bool,
    /// 这一条是「多列符」而不是普通分隔线：菜单在这里另起一列。
    ///
    /// `kind` 仍然是 `Separator` —— 它俩在「不执行动作、不可选中、不进动作列表」
    /// 这些规矩上完全一样，只差显示和排版，单开一个 `ItemKind` 反而要改二十多处判断。
    #[serde(default, skip_serializing_if = "is_false")]
    pub column_break: bool,
    /// 图标位显不显示：`None` = 跟随全局设置，`Some` = 这一项自己定
    pub show_badges: Option<bool>,
    /// 快捷键提示显不显示：`None` = 跟随全局设置
    pub show_shortcuts: Option<bool>,
    pub enabled: bool,
    /// 依次执行的动作（只有 `Item` 有；`Submenu` 一定是空的）
    pub actions: Vec<ActionStep>,
    /// 「只在选中了这些内容时出现」—— 配 `context_mode = Own` 时才有意义
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "de_contexts"
    )]
    pub contexts: Vec<SelectionKind>,
    /// 只对**这些后缀**的文件出现（`mp4|mkv` 这种写法）。
    ///
    /// 空串 = 所有文件都算。只有 `contexts` 里勾了「文件」才有意义，
    /// 而且会**继承给子项**（子菜单写了，里面的项默认也按它来）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_exts: String,
    /// 备注：给自己看的说明，显示在菜单项右侧（灰色小字）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// 运行程序时，把「这次抓到的选中内容」接在命令末尾（前面自动补一个空格，
    /// 文件路径带空格会加双引号）。
    ///
    /// `None` = 跟随全局开关（`MenuSettings::append_selection`），
    /// `Some` = 这一项自己定 —— 局部优先级高于全局。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append_selection: Option<bool>,
    /// 图标位空着时，要不要按「第一个动作里那个程序」自动取图标：`None` = 跟随全局
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_icon: Option<bool>,
    /// 这一项怎么参与「按选中内容筛选」：跟随上级 / 单独指定 / 不参与
    #[serde(default, skip_serializing_if = "ContextMode::is_inherit")]
    pub context_mode: ContextMode,
    /// 单独指定的普通项浮到根菜单以后放哪儿
    #[serde(default, skip_serializing_if = "FloatPlacement::is_top")]
    pub float_placement: FloatPlacement,
    /// 子菜单
    pub children: Vec<ItemSetting>,
    /// 「还原为菜单项」时暂存下来的子项。
    ///
    /// 直接在 `children` 里留着是不行的：`normalize()` 只看「有没有子项」来定
    /// `kind`，留着就永远降不成普通项。所以挪到这个看不见的抽屉里 —— 不参与菜单、
    /// 不参与 `has_children()`，等用户再点「添加子菜单」时原样放回去。
    /// 这样点错了也不会把攒了半天的子项弄丢。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_children: Vec<ItemSetting>,
    /// 子菜单的「自动生成」：从指定文件夹按后缀读文件，变成这一支的菜单项
    #[serde(default, skip_serializing_if = "AutoFillSpec::is_empty")]
    pub autofill: AutoFillSpec,
    /// 老版本的单条动作：读进来之后在 `migrate()` 里转成 `actions`
    #[serde(rename = "action", skip_serializing)]
    legacy_action: Option<LegacyAction>,
}

/// 子菜单的「自动生成」：从几个文件夹里按后缀把文件读成菜单项。
///
/// 走 **Everything 的索引**（不扫盘，几万个文件也是毫秒级），所以 Everything
/// 没连上时这一支就什么也不生成（界面上会说明「要先连上 Everything」）。
///
/// 规则：只**补缺**，不覆盖 —— 子菜单里已经有同名的项就跳过，
/// 用户改过的动作不会被下一次生成冲掉；文件删了也不会把项删掉（那是用户的东西）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoFillSpec {
    /// 开关（关掉就不生成，也不动这一支）
    pub enabled: bool,
    /// 什么时候生成（见 [`AutoFillMode`]；老配置没有这个字段 = 手动）
    #[serde(default)]
    pub mode: AutoFillMode,
    /// 文件夹：一行一个（也认 `;` / `,` 分隔）
    pub dirs: String,
    /// 后缀：`mp4|mkv` / `mp4 mkv` / `mp4,mkv` 都认（不带点也行）
    pub exts: String,
    /// 写完整路径（`false` = 只写文件名，交给 Everything / PATH 去解析）
    pub full_path: bool,
    /// 要不要连子文件夹里的文件一起收（默认不要：只收当前文件夹这一层）
    #[serde(default)]
    pub recurse: bool,
    /// 一次最多生成多少条（护栏：别把几千个文件一口气塞进菜单）
    pub limit: u32,
}

/// 「自动生成」什么时候跑。
///
/// 三个模式的区别只有一个：**要不要清空这一支现有的项**。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoFillMode {
    /// 手动：点「立即生成」才跑。只补缺，同名（含去后缀同名）的跳过，
    /// 用户自己加过的项、改过的动作都不会被动到。
    #[default]
    Manual,
    /// 实时：这一支**要显示之前**重建一次（呼出菜单时、或展开这一支时），
    /// 先清空这一支现有的项再按文件夹内容重来。
    ///
    /// 注意不是「盯着文件夹轮询」：Windows 上没有便宜的目录变更通知能跨盘可靠用，
    /// 而且真盯着反而会拖慢——文件一改就重建整个菜单树。实际做法是「用之前刷一次」。
    Live,
    /// 仅开启软件时：软件启动时重建一次，同样会清空。运行期间不再变。
    OnStart,
}

impl AutoFillMode {
    pub const ALL: [AutoFillMode; 3] =
        [AutoFillMode::Manual, AutoFillMode::Live, AutoFillMode::OnStart];

    pub fn label(self) -> &'static str {
        crate::i18n::t(match self {
            AutoFillMode::Manual => "手动生成",
            AutoFillMode::Live => "实时生成",
            AutoFillMode::OnStart => "仅开启软件时",
        })
    }

    pub fn hint(self) -> &'static str {
        crate::i18n::t(match self {
            AutoFillMode::Manual => "点「立即生成」才更新；只补缺，不覆盖同名项",
            AutoFillMode::Live => "每次呼出菜单前重建这一支：会清空它下面现有的项，且不能再手动添加",
            AutoFillMode::OnStart => "软件启动时重建一次：会清空它下面现有的项，之后不再变",
        })
    }

    /// 这个模式会不会把这一支清空重来。
    pub fn clears(self) -> bool {
        matches!(self, AutoFillMode::Live | AutoFillMode::OnStart)
    }
}

impl AutoFillSpec {
    pub fn is_empty(&self) -> bool {
        !self.enabled && self.dirs.trim().is_empty() && self.exts.trim().is_empty()
    }

    pub fn limits_limit() -> (u32, u32) {
        (1, 5000)
    }

    /// 「后缀」拆成干净的列表（去掉点、统一小写、去重）。
    pub fn ext_list(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for part in split_ext_list(&self.exts) {
            let ext = part.trim().trim_start_matches('.').to_ascii_lowercase();
            if !ext.is_empty() && !out.contains(&ext) {
                out.push(ext);
            }
        }
        out
    }

    /// 「文件夹」拆成列表。
    pub fn dir_list(&self) -> Vec<String> {
        split_autofill_list(&self.dirs)
            .into_iter()
            .map(|dir| dir.trim().trim_matches('"').trim_end_matches('\\').to_string())
            .filter(|dir| !dir.is_empty())
            .collect()
    }
}

/// `dirs` / `exts` 的分隔符：换行、逗号、分号、竖线、制表、顿号都算。
fn split_autofill_list(text: &str) -> Vec<String> {
    text.split(|ch: char| {
        ch == '\n' || ch == '\r' || ch == ',' || ch == ';' || ch == '|' || ch == '\t' || ch == '、'
    })
    .map(|part| part.trim().to_string())
    .filter(|part| !part.is_empty())
    .collect()
}

/// 后缀的分隔符比文件夹多一个**空格**：`mp4 mkv` 也认（老写法），现在推荐 `mp4|mkv`。
///
/// 文件夹不能按空格切 —— `D://My Files` 会被切成两半。
fn split_ext_list(text: &str) -> Vec<String> {
    text.split(|ch: char| {
        ch == '\n'
            || ch == '\r'
            || ch == ','
            || ch == ';'
            || ch == '|'
            || ch == '\t'
            || ch == '、'
            || ch == ' '
    })
    .map(|part| part.trim().to_string())
    .filter(|part| !part.is_empty())
    .collect()
}

impl Default for ItemSetting {
    fn default() -> Self {
        Self {
            kind: ItemKind::Item,
            id: String::new(),
            label: String::new(),
            badge: String::new(),
            accel: String::new(),
            shortcut: String::new(),
            hotkey: String::new(),
            hotkey_sided: true,
            hotkey_local: String::new(),
            hotkey_local_sided: true,
            hotkey_local_when: String::new(),
            hotkey_when: String::new(),
            hotstring: String::new(),
            // 默认「不等结束符」：敲完最后一个字母立刻执行（AHK 的 `:*:`）。
            // 想回到「必须再按空格/回车」的，在编辑窗口里把这个按钮关掉。
            hotstring_now: true,
            column_break: false,
            show_badges: None,
            show_shortcuts: None,
            enabled: true,
            actions: Vec::new(),
            contexts: Vec::new(),
            file_exts: String::new(),
            note: String::new(),
            append_selection: None,
            auto_icon: None,
            context_mode: ContextMode::Inherit,
            float_placement: FloatPlacement::Top,
            children: Vec::new(),
            parked_children: Vec::new(),
            autofill: AutoFillSpec::default(),
            legacy_action: None,
        }
    }
}

impl ItemSetting {
    pub fn item(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            ..Self::default()
        }
    }

    pub fn separator() -> Self {
        Self {
            kind: ItemKind::Separator,
            ..Self::item(next_separator_id(), "")
        }
    }

    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = badge.into();
        self
    }

    /// 图标位后面的加速键（只取第一个数字/字母）。
    pub fn accel(mut self, accel: impl Into<String>) -> Self {
        self.accel = accel.into();
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = shortcut.into();
        self
    }

    /// 单条动作（写默认菜单时最常用）。
    pub fn action(mut self, kind: ActionKind, value: impl Into<String>) -> Self {
        self.actions = vec![ActionStep::new(kind, value)];
        self
    }

    /// 多条动作（按顺序执行）。
    pub fn steps(mut self, steps: Vec<ActionStep>) -> Self {
        self.actions = steps;
        self
    }

    pub fn children(mut self, children: Vec<ItemSetting>) -> Self {
        self.children = children;
        self.kind = ItemKind::Submenu;
        self
    }

    pub fn has_children(&self) -> bool {
        !self.children.is_empty()
    }

    /// 这一项写的后缀列表（`mp4|mkv` → `["mp4", "mkv"]`；空 = 不限）。
    pub fn ext_list(&self) -> Vec<String> {
        crate::selection::parse_exts(&self.file_exts)
    }

    /// 第一个「运行程序」动作里那个程序文件（有就返回它的路径）。
    ///
    /// 只认第一种写法：`notepad.exe` / `"C:\Program Files\App\app.exe"` 这种
    /// 开头一个可执行文件；`code .` 会取到 `code`（不是路径，取不到图标就算了）。
    pub fn program_path(&self) -> Option<String> {
        self.actions
            .iter()
            .find(|step| step.kind == ActionKind::Run)
            .and_then(|step| crate::icon::program_of(&step.value))
    }

    /// 图标位空着时，按第一个动作的程序自动补一个（`None` = 这一项不要自动图标）。
    ///
    /// 返回的是**能直接画的**东西：`Icon/` 里那张 png（磁盘缓存，一次查表）。
    ///
    /// 以前这里每条都把程序路径解析一遍（`PATH` → 本地索引 → Everything），
    /// 一千多项、每呼出一次就要问上千次「这个程序在哪」。现在解析只发生在
    /// 建缓存的时候（启动 / 导入后的后台线程），菜单里只剩查表。
    ///
    /// 缓存还没建好（刚启动那几秒）就暂时不给图标 —— 宁可少画几个图标，
    /// 也不要让「呼出菜单」等它现算。
    pub fn auto_badge(&self, global_auto: bool) -> Option<String> {
        if !self.badge.trim().is_empty() || !self.auto_icon.unwrap_or(global_auto) {
            return None;
        }
        let value = self
            .actions
            .iter()
            .find(|step| step.kind == ActionKind::Run)
            .map(|step| step.value.as_str())?;
        crate::icon_cache::badge_for(value)
    }

    /// 界面上那句说明：「跟随上级」/「文字 / 图片」/「不参与」。
    pub fn context_label(&self) -> String {
        match self.context_mode {
            ContextMode::Inherit => crate::i18n::t("跟随上级").to_string(),
            ContextMode::Never => crate::i18n::t("不参与").to_string(),
            ContextMode::Own if self.contexts.is_empty() => {
                crate::i18n::t("（没选类型）").to_string()
            }
            ContextMode::Own => self
                .contexts
                .iter()
                .map(|kind| kind.label())
                .collect::<Vec<_>>()
                .join(" / "),
        }
    }

    pub fn is_submenu(&self) -> bool {
        self.kind == ItemKind::Submenu
    }

    /// 普通分隔线（不是「多列符」）。
    pub fn is_plain_separator(&self) -> bool {
        self.kind == ItemKind::Separator && !self.column_break
    }

    /// 老 JSON 的单条动作 → 多条动作；顺便把 kind 归一。
    fn migrate(&mut self) {
        if let Some(legacy) = self.legacy_action.take() {
            if self.actions.is_empty() {
                match legacy.kind {
                    LegacyKind::None => {}
                    LegacyKind::Command => self
                        .actions
                        .push(ActionStep::new(ActionKind::Run, legacy.value)),
                    LegacyKind::Url => self
                        .actions
                        .push(ActionStep::new(ActionKind::Url, legacy.value)),
                }
            }
        }
        self.normalize();
        // 老配置只有 `contexts`、没有 `context_mode`：那说明作者是「单独指定了类型」
        if self.context_mode == ContextMode::Inherit && !self.contexts.is_empty() {
            self.context_mode = ContextMode::Own;
        }
        // 老版本只有一颗「带条件的触发键」（`hotkey` + `hotkey_when`）。现在拆成
        // 「全局键」「局部键」两颗：带条件的那一颗搬到局部去，全局留空 ——
        // 用户原来的行为（只在某些窗口里生效）一点都没变，还多出空位再加一颗全局键。
        if !self.hotkey_when.trim().is_empty() && self.hotkey_local.trim().is_empty() {
            self.hotkey_local = std::mem::take(&mut self.hotkey);
            self.hotkey_local_when = std::mem::take(&mut self.hotkey_when);
            self.hotkey_local_sided = self.hotkey_sided;
        }
        // 设了触发快捷键、又没单独写「快捷键提示」的：提示就默认显示这个组合
        if self.shortcut.trim().is_empty() {
            let shown = if !self.hotkey.trim().is_empty() {
                self.hotkey.trim()
            } else {
                self.hotkey_local.trim()
            };
            if !shown.is_empty() {
                self.shortcut = shown.to_string();
            }
        }
        // 老配置只有「接上选中内容」这一个开关（bool，默认 false）。新语义里
        // `Some(false)` 是「这一项强制关掉」，会把后面的全局开关焊死 ——
        // 老的 `false` 其实就是「没配过」，统一收成「跟随全局」。
        if self.append_selection == Some(false) {
            self.append_selection = None;
        }
        for child in &mut self.children {
            child.migrate();
        }
    }

    /// 子菜单不配动作；有子项就是子菜单，没子项就退回普通项。
    fn normalize(&mut self) {
        if self.kind == ItemKind::Separator {
            self.children.clear();
            self.actions.clear();
            return;
        }
        if !self.children.is_empty() {
            self.kind = ItemKind::Submenu;
            // 动作**不删**：子菜单只是「不执行动作」，动作本身留着。
            // 用户把类型点来点去、或者先把动作填好再改成子菜单，切回来时东西还在。
            // 菜单构建那一步本来就按 kind 走，子菜单不会去读这些动作。
        }
        // 空子菜单**保持子菜单**：它可能是「自动生成」还没跑、或者刚建好还没加项。
        // 以前这里会把空的子菜单降成普通项 —— 自动生成的项被清空（比如文件夹里的
        // 文件删光了）之后，子菜单莫名其妙变成一个点不动的普通项，就是这个造成的。
        // 要还原成普通项请走显式的 `demote_submenu()`（右键「还原为菜单项」）。
    }

    /// 悬停提示里要补的那一行：这一项「运行程序」写的是裸文件名时，
    /// 解析出来的完整路径（没有就返回 `None`）。
    pub fn tooltip_path(&self) -> Option<String> {
        let value = self
            .actions
            .iter()
            .find(|step| step.kind == ActionKind::Run)?
            .value
            .trim()
            .trim_matches('"')
            .to_string();
        if value.is_empty()
            || value.contains('\\')
            || value.contains('/')
            || value.contains('{')
            || value.contains('%')
        {
            return None;
        }
        crate::progindex::resolve(&value).map(|path| format!("→ {path}"))
    }

    /// 变成菜单树里的一个节点（分隔线 / 普通项 / 子菜单）。
    ///
    /// `show_badges` / `show_shortcuts` 传的是**全局**设置；
    /// 单项自己设了（`Some`）就按自己的来，优先级高于全局。
    fn to_entry(&self, show_badges: bool, show_shortcuts: bool, auto_icons: bool) -> MenuEntry {
        if self.kind == ItemKind::Separator {
            if self.column_break {
                return MenuEntry::ColumnBreak;
            }
            return MenuEntry::Separator;
        }
        let badges = self.show_badges.unwrap_or(show_badges);
        let shortcuts = self.show_shortcuts.unwrap_or(show_shortcuts);
        let label = self.label.trim();
        let mut item = MenuItem::new(
            self.id.clone(),
            if label.is_empty() {
                crate::i18n::t("（未命名）").to_string()
            } else {
                label.to_string()
            },
        );
        if badges {
            if !self.badge.trim().is_empty() {
                item = item.badge(self.badge.trim().to_string());
            } else if let Some(program) = self.auto_badge(auto_icons) {
                // 图标位空着：拿第一个「运行程序」动作的 exe 图标顶上
                item = item.badge(program);
            }
        }
        // 备注：右侧灰色小字，和快捷键提示并排
        if !self.note.trim().is_empty() {
            item = item.note(self.note.trim().to_string());
        }
        // 加速键不进「显示图标位」那个开关：它是键盘入口，不是一个装饰
        if let Some(accel) = accel_char(&self.accel) {
            item = item.accel(accel.to_string());
        }
        // 右边那一列文字：设了真实快捷键就显示它（那个才是按下去会生效的），
        // 否则显示纯提示用的 `shortcut`
        let trigger = self.hotkey.trim();
        if !trigger.is_empty() {
            if shortcuts {
                item = item.shortcut(trigger.to_string());
            }
        } else if shortcuts && !self.shortcut.trim().is_empty() {
            item = item.shortcut(self.shortcut.trim().to_string());
        }
        let children: Vec<MenuEntry> = self
            .children
            .iter()
            .filter(|child| child.enabled)
            .map(|child| child.to_entry(show_badges, show_shortcuts, auto_icons))
            .collect();
        if !children.is_empty() {
            item = item.submenu(children);
        }
        // 子菜单不执行动作，只有普通项才带动作
        if self.kind == ItemKind::Item {
            item.actions = self.actions.clone();
        }
        MenuEntry::Item(item)
    }
}

/// 拖拽落点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropTarget {
    /// 插到某节点前面（和它同一层）
    Before(Vec<usize>),
    /// 插到某节点后面（和它同一层）
    After(Vec<usize>),
    /// 变成某节点的子项（放到它的最后）
    Into(Vec<usize>),
}

impl DropTarget {
    /// 落点参照的那个节点的路径。
    pub fn anchor(&self) -> &[usize] {
        match self {
            DropTarget::Before(path) | DropTarget::After(path) | DropTarget::Into(path) => path,
        }
    }
}

/// serde：`false` 的布尔字段不写进 JSON（老配置文件里没这些字段，读的时候默认 false）。
fn is_false(value: &bool) -> bool {
    !*value
}

/// serde：`true` 的布尔字段不写进 JSON（默认就是 `true` 的那些）。
fn is_true(value: &bool) -> bool {
    *value
}

/// 分隔线的 id：不能重名（`find_path` 按 id 找，重名会一次点亮一排）。
fn next_separator_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(1);
    format!("sep-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// 所有可调项。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MenuSettings {
    pub theme_mode: ThemeMode,
    pub light: ThemeColors,
    pub dark: ThemeColors,
    /// 一列的宽度
    pub panel_width: f32,
    /// 列宽怎么来：固定值 / 按这一列的内容自动算
    #[serde(default)]
    pub panel_width_mode: PanelWidthMode,
    /// 自动列宽时的上限（再长就截住，免得一个长名字把菜单拉出屏幕）
    #[serde(default = "default_max_panel_width")]
    pub panel_max_width: f32,
    /// 单行高度
    pub row_height: f32,
    /// 卡片圆角
    pub radius: f32,
    /// 菜单卡片外面那一圈 1px 描边（关掉就是纯色块，靠底色对比分边界）
    #[serde(default = "default_true")]
    pub menu_border: bool,

    /// AutoHotkey v1 解释器路径（空 = 自动找）
    #[serde(default)]
    pub ahk_v1: String,
    /// AutoHotkey v2 解释器路径（空 = 自动找）
    #[serde(default)]
    pub ahk_v2: String,

    /// 子菜单从哪儿展开（见 [`SubmenuAnchor`]）
    #[serde(default)]
    pub submenu_anchor: SubmenuAnchor,
    /// 图标位（左侧小图标）的边长
    pub badge_size: f32,
    /// 字号倍数
    pub font_scale: f32,
    /// 菜单字体名称（空串 = 系统默认 UI 字体）
    pub font_family: String,
    pub show_shortcuts: bool,
    pub show_badges: bool,
    pub show_separators: bool,
    /// 弹出菜单前先抓一次「当前选中的内容」（注入复制键，抓完还原剪贴板）
    pub selection_enabled: bool,
    /// 抓取时注入的复制按键（终端里 Ctrl+C 是「中断」，可以改成 Ctrl+Insert）
    pub selection_copy_keys: String,
    /// 发完复制键后，最多等多少毫秒让剪贴板把内容写全（慢了就调大，单位 ms）
    pub selection_wait_ms: u64,
    /// 有选中内容时，菜单只显示「适用这种内容」的项（关掉就永远全显示）
    pub context_filter: bool,
    /// 「运行程序」动作默认接上选中内容（菜单项里可以单独设成开启 / 关闭，
    /// 局部优先级高于这里）
    #[serde(default)]
    pub append_selection: bool,
    /// 多选文件时，「这几项要求的后缀」怎么算命中
    pub multi_file_rule: MultiFileRule,
    /// 「把选中的文件加到菜单里」时路径写成什么样（每次问 / 总是完整 / 总是裸名）
    #[serde(default)]
    pub add_selection_path: crate::selection::AddSelectionPath,
    /// 弹出菜单的全局快捷键（`Ctrl+Alt+M`；改完立刻重新注册）
    pub popup_hotkey: String,
    /// 同一份菜单，但**不抓**选中内容的那个快捷键（空 = 不注册）。
    ///
    /// 有些场景「先复制一次选中内容」本身就是打扰：正在全屏游戏里、正在拖选一段
    /// 长得要命的日志、或者剪贴板里正放着别的东西。按这一键就只是把菜单摆出来，
    /// `{selection}` 是空的。
    #[serde(default)]
    pub popup_hotkey_noselect: String,
    /// 弹出菜单那一键要不要区分左右修饰键
    #[serde(default = "default_true")]
    pub popup_hotkey_sided: bool,
    /// 「不抓选中内容」那一键要不要区分左右修饰键
    #[serde(default = "default_true")]
    pub popup_hotkey_noselect_sided: bool,
    /// 一键直达：选中内容 + 一个快捷键 → 按规则表匹配 → 命中就直接跑那一项
    #[serde(default)]
    pub magic: crate::magic::MagicSettings,
    /// 菜单不透明度（1.0 = 完全不透明）。整个菜单窗口一起变淡，文字也跟着淡。
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// 图标位空着时，自动按「第一个动作里的程序」取图标
    #[serde(default = "default_true")]
    pub auto_icons: bool,
    /// 在系统托盘里放一个图标（关掉就彻底没有托盘）
    #[serde(default = "default_true")]
    pub tray_enabled: bool,
    /// 左键点托盘图标做什么
    pub tray_left_click: TrayLeftClick,
    /// 托盘图标：`.ico` / `.png` / `.exe` 路径，空串 = 内置图标
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tray_icon: String,
    /// 按选中内容筛选之后，加一条「显示全部菜单」用来一键看全量
    #[serde(default = "default_true")]
    pub show_all_entry: bool,
    /// 那条「显示全部菜单」放顶部还是底部
    pub show_all_placement: EdgePlacement,
    /// 弹出菜单里要不要带一个搜索框（筛选菜单项 / 子菜单），摆在顶部还是底部
    #[serde(default)]
    pub menu_search: SearchPlace,
    /// 菜单项的鼠标悬停提示（显示什么、开不开）
    #[serde(default)]
    pub tooltip: TooltipSettings,
    /// 「运行程序」只填文件名时的自动补全（本地可执行文件索引）
    #[serde(default)]
    pub programs: ProgramIndexSettings,
    /// Everything 联动：程序在哪儿、要不要随本程序启动
    #[serde(default)]
    pub everything: EverythingSettings,
    /// 热字符串：敲一串字就执行菜单项（AHK 的 hotstring）
    #[serde(default)]
    pub hotstrings: HotstringSettings,
    /// 「最近运行」：把刚用过的几项提到菜单顶部 / 底部
    #[serde(default)]
    pub recent: RecentSettings,
    /// 开机自启（注册表 / 任务计划）
    #[serde(default)]
    pub autostart: AutostartSettings,
    /// 「禁用应用」名单
    #[serde(default)]
    pub block: BlockSettings,
    /// 界面语言
    #[serde(default)]
    pub language: Language,
    /// 进「关于」页时自动检查一次更新
    #[serde(default = "default_true")]
    pub check_updates: bool,
    /// 最近运行的记录（新的在前，最多留几十条；显示时再按 limit 截）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_items: Vec<RecentTrack>,
    /// 当前用的菜单配置文件（`menus/<名字>.runa`，不含后缀；空 = 默认那份）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub menu_profile: String,
    /// 正在**预览**、还没点「应用 / 确定」的那份菜单配置。
    ///
    /// 切配置只把 `.runa` 读进内存显示，不写盘；用户点「应用 / 确定」才把
    /// `menu_profile` 改成它并落盘，点「取消」就把树换回原来那份。
    /// 这样「切一下看看」不会顺手把文件改掉。
    #[serde(skip)]
    pub pending_profile: Option<String>,
    /// 新加菜单项的 id 序号
    pub next_id: u32,
    /// 菜单项（**内存里也包含托盘那一支**，`TRAY_ID`）。
    ///
    /// 托盘为什么还留在这儿：树编辑器（拖拽 / 增删 / 动作 / 图标）只认一棵树，
    /// 分两个数组就得把每个操作写两遍。但**写盘时会把它拆出去**（见 `save`）：
    /// `menus/*.runa` 里没有托盘，`settings.json` 里单独一段 `tray_menu`。
    /// 这样换一套菜单配置，托盘右键菜单不动 —— 它属于这台机器，不属于某套菜单。
    ///
    /// 写盘时只要 `menus/<名字>.runa` 写成功了，这份就不会再塞进 `settings.json`
    /// （`save` 里清空）：两千条菜单项存两份要一兆多，纯属浪费。
    #[serde(default = "default_items")]
    pub items: Vec<ItemSetting>,
    /// 托盘右键菜单（`items` 里那一支的副本，只用于落盘 / 读盘）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tray_menu: Option<ItemSetting>,
}

/// 鼠标停在菜单项上一会儿弹出来的小提示。
///
/// 只是「看一眼」用的：弹菜单时鼠标下面压着的是名字，想确认这一项到底执行什么，
/// 或者它绑了哪个键，就不用点进去看设置。
///
/// 内容由一个模板决定（`template`，见 `tip_template`），
/// `show_*` 那几个老开关只在「老配置迁移」时用一次 —— 它们拼出默认模板。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TooltipSettings {
    /// 总开关
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 卡片宽度（逻辑像素）
    #[serde(default = "default_tip_width")]
    pub width: f32,
    /// 全局对齐方式（每行还能用 `{left}` / `{center}` / `{right}` 单独覆盖）
    #[serde(default)]
    pub align: TipAlign,
    /// 内容模板：一行一条，占位符写成 `{名字}`
    #[serde(default)]
    pub template: String,
    /// 显示动作（运行什么程序 / 发什么键 / 内置动作）
    #[serde(default = "default_true")]
    pub show_actions: bool,
    /// 显示快捷键：触发键 / 快捷键提示 / 加速键
    #[serde(default = "default_true")]
    pub show_keys: bool,
    /// 显示备注
    #[serde(default = "default_true")]
    pub show_note: bool,
    /// 显示类型信息（子菜单有几项 / 当前是隐藏的）
    #[serde(default = "default_true")]
    pub show_meta: bool,
    /// 鼠标停多久才出来（毫秒）
    #[serde(default = "default_tooltip_delay")]
    pub delay_ms: u64,
}

fn default_tooltip_delay() -> u64 {
    450
}

fn default_tip_width() -> f32 {
    400.0
}

impl Default for TooltipSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            width: default_tip_width(),
            align: TipAlign::default(),
            template: crate::tip_template::DEFAULT_TEMPLATE.to_string(),
            show_actions: true,
            show_keys: true,
            show_note: true,
            show_meta: true,
            delay_ms: default_tooltip_delay(),
        }
    }
}

impl TooltipSettings {
    pub fn limits_delay() -> (f32, f32) {
        (120.0, 1500.0)
    }

    pub fn limits_width() -> (f32, f32) {
        (180.0, 560.0)
    }

    /// 老配置（只有 `show_*` 开关、没有 `template`）迁移成模板。
    ///
    /// 迁移一次就写回盘上，之后用户在界面上改模板说了算。
    pub fn migrate(&mut self) -> bool {
        if !self.template.trim().is_empty() {
            return false;
        }
        let mut lines = vec!["{label}".to_string()];
        if self.show_meta {
            lines.push(crate::i18n::t("?{kind} · {children} 项").to_string());
        }
        if self.show_keys {
            lines.push(crate::i18n::t("?加速键  {accel}").to_string());
            lines.push(crate::i18n::t("?触发键  {hotkey}").to_string());
            lines.push(crate::i18n::t("?快捷键  {shortcut}").to_string());
        }
        lines.push("?{hidden}".to_string());
        if self.show_note {
            lines.push("{note}".to_string());
        }
        if self.show_actions {
            lines.push("{actions}".to_string());
        }
        self.template = lines.join("\n");
        self.width = self.width.clamp(
            TooltipSettings::limits_width().0,
            TooltipSettings::limits_width().1,
        );
        true
    }
}

fn default_true() -> bool {
    true
}

/// 同名多份可执行文件时，先看哪个指标。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreferProgram {
    /// 修改时间最新
    #[default]
    Newest,
    /// 路径里认出来的版本号最高（认不出就比修改时间）
    Version,
    /// 路径最短（通常就是最外层那个）
    Shortest,
}

impl PreferProgram {
    pub const ALL: [PreferProgram; 3] = [Self::Newest, Self::Version, Self::Shortest];

    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            Self::Newest => "修改时间最新",
            Self::Version => "版本号最高",
            Self::Shortest => "路径最短",
        })
    }

    pub fn hint(self) -> &'static str {
        crate::i18n::t(
        match self {
            Self::Newest => "同名的那几份里，挑最近改过的那个",
            Self::Version => "读文件本身的版本号（PE 资源），挑大的；读不到就比时间",
            Self::Shortest => "路径越短越靠前，一般就是最外层那个",
        })
    }
}

/// Everything 联动（比文件名、找完整路径）。
///
/// 后端是「借 Everything 的索引」，所以这里只管**它在哪儿、要不要顺手拉起来**：
/// - 便携版把 `Everything\` 整个文件夹放在本程序旁边就能用（`Everything.exe` /
///   `Everything64.dll` / `es.exe` 都在里面）；
/// - 装在别处（绿色版、改过安装位置）就用 `exe` 手动指一下；
/// - Everything 没运行时，可以勾上 `autostart`，本程序启动时顺手把它拉起来。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EverythingSettings {
    /// `Everything.exe`（或它所在的文件夹）的路径；空 = 自动找
    pub exe: String,
    /// Everything 没在运行时，随本程序一起启动
    pub autostart: bool,
}

impl Default for EverythingSettings {
    fn default() -> Self {
        Self {
            exe: String::new(),
            autostart: false,
        }
    }
}

/// 「运行程序」只填文件名时的自动补全。
///
/// 索引本身是 `progindex` 那套（扫 PATH / Program Files / LocalAppData\\Programs），
/// 这里只放「用户能改的那部分」：优先级、排除规则、手动钉死的路径、认哪些后缀。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgramIndexSettings {
    /// 总开关（关掉就完全不查索引，交回 `cmd` 自己按 PATH 找）
    pub enabled: bool,
    /// 同名多份时先看什么
    pub prefer: PreferProgram,
    /// 排除规则：路径里包含这些文字就不要（一行一条）
    pub exclude: String,
    /// 认哪些后缀名（小写、不带点，`;` 分隔）：只索引这些扩展名的文件
    pub extensions: String,
    /// 手动指定：小写文件名 → 完整路径
    ///
    /// **不写进 `settings.json`**：它是一条条试出来的缓存，条目多起来能把这个
    /// 文件撑到几 MB。单独落在 `program-fixes.json`（见 [`fixes_path`]）。
    /// 这里仍然保留反序列化，只为把老版本写在 `settings.json` 里的那份搬出来。
    #[serde(default, skip_serializing)]
    pub fixes: std::collections::BTreeMap<String, String>,
}

impl Default for ProgramIndexSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            prefer: PreferProgram::default(),
            exclude: String::new(),
            extensions: "bat;cmd;exe;msi;msp;msu;ps1;scr;vbs;lnk".to_string(),
            fixes: std::collections::BTreeMap::new(),
        }
    }
}

impl ProgramIndexSettings {
    /// 解析「后缀名」配置成 `[".exe", ".bat", ...]`（小写、带点、去重）。
    pub fn extensions(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.extensions
            .split([';', ',', '，', '、'])
            .map(|part| part.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|part| !part.is_empty())
            .filter(|part| part.chars().all(|ch| ch.is_ascii_alphanumeric()))
            .map(|part| format!(".{part}"))
            .filter(|ext| seen.insert(ext.clone()))
            .collect()
    }
}

/// 热字符串：在任何程序里敲一小串字，自动执行绑定的菜单项。
///
/// 就是 AutoHotkey 的 `::ftw::Free the whales`：敲 `ftw` 再敲一个结束字符
/// （空格 / Tab / 回车 / `.` `,` 这类），那三个字母被删掉、换成这一项的动作。
///
/// 这一份只管**全局**那部分（开不开、提示怎么弹）；每条短语本身挂在
/// `ItemSetting::hotstring` 上 —— 热字符串和「点击」「触发快捷键」是同一个菜单项的
/// 三种触发方式，不该另开一套短语库。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotstringSettings {
    /// 总开关（关掉就撤掉键盘钩子，一个键都不看）
    pub enabled: bool,
    /// 只在后台替换，不弹提示小窗
    pub hide_hint: bool,
    /// 敲到第几个字符开始提示（默认 3）
    pub hint_after: u32,
    /// 提示里「启动路径」最多显示几个字（0 = 不显示路径）
    pub hint_path_chars: u32,
    /// 提示停留多久自动消失（毫秒）
    pub hint_ms: u64,
    /// 提示的不透明度（百分比 20~100）
    pub hint_opacity: u32,
    /// 提示相对鼠标坐标的偏移（可为负）
    pub hint_x: i32,
    pub hint_y: i32,
    /// 除了空格 / Tab / 回车，还有哪些字符算「打完了」（AHK 的 EndChars）
    pub end_chars: String,
    /// 提示贴在哪儿：鼠标旁边 / 光标插入处
    pub anchor: HintAnchor,
    /// 区分大小写（默认不区分，和 AHK 一样）
    pub case_sensitive: bool,
    /// 提示卡片上可以点：鼠标悬上去不消失，点哪条就直接执行哪一条
    pub hint_clickable: bool,
    /// 输入法**组字中**也尝试匹配（默认开）
    ///
    /// 关着 = AHK 的行为：中文输入法正在组字时不触发、也不抢键。
    /// 开着 = 命中后先按一下 Esc 让输入法自己丢掉组字，再展开。
    ///
    /// 为什么是「按 Esc」而不是 `ImmNotifyIME(CPS_CANCEL)`：跨进程调 Imm*
    /// 拿到的 context 读不到组字（实测 `ImmGetCompositionString` 一直返回 0），
    /// 取消于是时灵时不灵。Esc 走正常输入通道，输入法一定认。
    ///
    /// **默认开**：中文输入法组字中也能用（命中后按 Esc 取消组字再展开）。
    /// 想恢复 AHK 那种「组字中一个键都不抢」的行为就关掉它。
    #[serde(default = "default_true")]
    pub ime_match: bool,
}

/// 热字符串提示卡片贴在哪儿。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HintAnchor {
    /// 鼠标指针旁边（默认；偏移量相对鼠标）
    #[default]
    Cursor,
    /// 光标插入处（文字光标右下角；偏移量相对那个点）
    Caret,
}

impl HintAnchor {
    pub const ALL: [HintAnchor; 2] = [HintAnchor::Cursor, HintAnchor::Caret];

    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            HintAnchor::Cursor => "鼠标旁边",
            HintAnchor::Caret => "光标插入处",
        })
    }
}

impl Default for HotstringSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hide_hint: false,
            hint_after: 3,
            hint_path_chars: 10,
            hint_ms: 6000,
            hint_opacity: 90,
            hint_x: 0,
            hint_y: 0,
            // AHK 的默认 EndChars
            end_chars: "-()[]{}:;'\"/\\,.?!".to_string(),
            anchor: HintAnchor::default(),
            // 默认「敲完就执行」：AHK 那种非得再按一个空格才响的行为在中文输入法下
            // 会让人多按一个键（候选词已经把字符吃掉了），实际体验就是「不灵」。
            case_sensitive: false,
            hint_clickable: true,
            ime_match: true,
        }
    }
}

impl HotstringSettings {
    pub fn limits_hint_after() -> (u32, u32) {
        (1, 12)
    }
    pub fn limits_path_chars() -> (u32, u32) {
        (0, 80)
    }
    pub fn limits_hint_ms() -> (u64, u64) {
        (500, 60_000)
    }
    pub fn limits_opacity() -> (u32, u32) {
        (20, 100)
    }
    pub fn limits_offset() -> (i32, i32) {
        (-2000, 2000)
    }

    /// 结束字符（不含空格 / Tab / 回车，那三个永远算）。
    pub fn end_char_set(&self) -> Vec<char> {
        self.end_chars.chars().filter(|ch| !ch.is_whitespace()).collect()
    }
}

/// 「最近运行」：把刚用过的几项提到菜单顶部 / 底部，省得每次翻子菜单。
///
/// 记录的是**菜单项的 id + 时间戳**（`RecentTrack`），不是整份菜单项：
/// 菜单项改了名字 / 动作，最近列表自动跟着变；项被删了就自然从列表里消失。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentSettings {
    /// 总开关（关掉就不记录、也不显示）
    pub enabled: bool,
    /// 排在菜单顶部还是底部
    pub placement: EdgePlacement,
    /// 最多显示几条
    pub limit: u32,
    /// 「按选中内容筛过」的菜单里也显示
    pub in_filtered: bool,
    /// 快捷键直接呼出的子菜单里也显示
    pub in_submenu: bool,
    /// 托盘菜单里点的那几项也记（默认不记：托盘菜单常常是临时翻一下）
    pub record_tray: bool,
}

impl Default for RecentSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            placement: EdgePlacement::Top,
            limit: 5,
            in_filtered: true,
            in_submenu: false,
            record_tray: false,
        }
    }
}

impl RecentSettings {
    pub fn limits_limit() -> (u32, u32) {
        (1, 20)
    }
}

/// 「禁用应用」名单：这些程序在前面时，快捷键 / 滚轮 / 提示条全部让路。
///
/// 典型场景是全屏游戏（DNF、CS 之类）：快捷键被抢会中断操作，
/// 提示条闪一下更烦。名单按 **exe 文件名**匹配（不区分大小写），
/// 换安装目录也照样认得出来。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlockSettings {
    pub enabled: bool,
    /// exe 名（`dnf.exe`）或完整路径；比对时只看文件名
    pub apps: Vec<String>,
}

impl Default for BlockSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            apps: Vec::new(),
        }
    }
}

impl BlockSettings {
    /// 把用户输入洗成「exe 名」：完整路径取最后一段，补上 `.exe`，转小写。
    pub fn normalize(entry: &str) -> Option<String> {
        let entry = entry.trim().trim_matches('"');
        if entry.is_empty() {
            return None;
        }
        let name = entry
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(entry)
            .trim()
            .to_lowercase();
        if name.is_empty() {
            return None;
        }
        if name.contains('.') {
            Some(name)
        } else {
            Some(format!("{name}.exe"))
        }
    }
}

/// 前台这个程序是不是被禁用了（名单为空 / 没开 → 永远 `false`）。
///
/// 只在「要弹东西之前」调（弹菜单、热字符串提示），不是每帧都查。
pub fn foreground_blocked() -> bool {
    let (enabled, apps) = read(|settings| {
        (settings.block.enabled, settings.block.apps.clone())
    });
    if !enabled || apps.is_empty() {
        return false;
    }
    let Some(name) = crate::win32::foreground_process_name() else {
        return false;
    };
    apps.iter()
        .filter_map(|entry| BlockSettings::normalize(entry))
        .any(|blocked| blocked == name)
}

/// 某个 exe 名在不在名单里（热字符串钩子按「当前输入目标」查，不走前台窗口）。
pub fn app_blocked(name: &str) -> bool {
    let (enabled, apps) = read(|settings| {
        (settings.block.enabled, settings.block.apps.clone())
    });
    if !enabled || apps.is_empty() {
        return false;
    }
    let name = name.to_lowercase();
    apps.iter()
        .filter_map(|entry| BlockSettings::normalize(entry))
        .any(|blocked| blocked == name)
}

/// 添加一条（重复 / 洗不出来就不加），返回有没有真的加上。
pub fn add_blocked_app(cx: &mut App, entry: &str) -> bool {
    let Some(name) = BlockSettings::normalize(entry) else {
        return false;
    };
    let exists = read(|settings| {
        settings
            .block
            .apps
            .iter()
            .filter_map(|known| BlockSettings::normalize(known))
            .any(|known| known == name)
    });
    if exists {
        return false;
    }
    update(cx, |settings| {
        settings.block.apps.push(name);
        // 加第一条的时候顺手把总开关打开，不然用户加了半天发现没生效
        settings.block.enabled = true;
    });
    true
}

/// 界面语言。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    #[default]
    Chinese,
    English,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::Chinese, Language::English];

    pub fn label(self) -> &'static str {
        crate::i18n::t(
        match self {
            Language::Chinese => "中文",
            Language::English => "English",
        })
    }
}

/// 开机自启的设置。
///
/// 系统里真正的状态由 `crate::autostart` 维护（注册表 Run 键 / 任务计划），
/// 这里的字段只记「用户想要什么」，启动和改开关时按它去对齐。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutostartSettings {
    /// 登录后自动在托盘运行
    pub enabled: bool,
    /// `true` = 用户登录时启动（任务计划程序）；`false` = 普通开机启动（注册表 Run）
    pub use_task: bool,
    /// 以管理员权限运行（只有任务计划方式能静默提权）
    pub as_admin: bool,
}

impl Default for AutostartSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            use_task: false,
            as_admin: false,
        }
    }
}

/// 一条「最近运行」记录。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentTrack {
    pub id: String,
    /// Unix 秒；只用来排序（越大越近）
    pub at: u64,
}

fn default_opacity() -> f32 {
    1.0
}

/// 自动列宽的上限。老配置里没有这个字段，读出来是 0 —— 用默认值兜住。
fn default_max_panel_width() -> f32 {
    360.0
}
/// 打包在 exe 里的默认外观 / 快捷键（`assets/defaults/settings.json`）。
const DEFAULT_SETTINGS: &str = include_str!("../assets/defaults/settings.json");

/// 打包在 exe 里的默认菜单（`assets/defaults/menu.runa`）。
const DEFAULT_MENU: &str = include_str!("../assets/defaults/menu.runa");

/// 解析打包的默认菜单；解析不出来就返回 `None`（调用方退回代码里写死的那套）。
fn bundled_menu() -> Option<(Vec<ItemSetting>, u32)> {
    let file: crate::menu_files::ProfileFile = serde_json::from_str(DEFAULT_MENU).ok()?;
    if file.items.is_empty() {
        return None;
    }
    Some((file.items, file.next_id.max(1)))
}

impl MenuSettings {
    /// 代码里写死的那套默认（打包的默认文件坏了时兜底用）。
    fn default_from_code() -> Self {
        Self {
            theme_mode: ThemeMode::FollowSystem,
            light: theme::default_light(),
            dark: theme::default_dark(),
            panel_width: 210.0,
            panel_width_mode: PanelWidthMode::default(),
            panel_max_width: default_max_panel_width(),
            row_height: 24.0,
            radius: 8.0,
            menu_border: true,
            ahk_v1: String::new(),
            ahk_v2: String::new(),
            submenu_anchor: SubmenuAnchor::default(),
            badge_size: 18.0,
            font_scale: 1.0,
            font_family: String::new(),
            show_shortcuts: true,
            show_badges: true,
            show_separators: true,
            selection_enabled: true,
            selection_copy_keys: "Ctrl+C".to_string(),
            selection_wait_ms: 500,
            context_filter: true,
            append_selection: true,
            multi_file_rule: MultiFileRule::AllSame,
            add_selection_path: crate::selection::AddSelectionPath::default(),
            popup_hotkey: DEFAULT_POPUP_HOTKEY.to_string(),
            popup_hotkey_noselect: String::new(),
            popup_hotkey_sided: true,
            popup_hotkey_noselect_sided: true,
            magic: crate::magic::MagicSettings::default(),
            opacity: default_opacity(),
            auto_icons: true,
            tray_enabled: true,
            tray_left_click: TrayLeftClick::default(),
            tray_icon: String::new(),
            show_all_entry: true,
            show_all_placement: EdgePlacement::default(),
            menu_search: SearchPlace::default(),
            tooltip: TooltipSettings::default(),
            programs: ProgramIndexSettings::default(),
            everything: EverythingSettings::default(),
            hotstrings: HotstringSettings::default(),
            recent: RecentSettings::default(),
            autostart: AutostartSettings::default(),
            block: BlockSettings::default(),
            check_updates: true,
            language: Language::default(),
            recent_items: Vec::new(),
            menu_profile: String::new(),
            pending_profile: None,
            next_id: 1,
            items: default_items(),
            tray_menu: None,
        }
    }
}

impl Default for MenuSettings {
    /// **只**是代码里写死的那套（用作兜底、也是单元测试的基线）。
    ///
    /// 打包的默认配置不在这里读：解析那份 54KB 的菜单会在测试线程的小栈上爆栈，
    /// 而且 `Default` 应该是「最省事、最不会失败」的那个值。
    /// 真正首次运行时用的是 [`bundled_default`]（见 `initial`）。
    fn default() -> Self {
        MenuSettings::default_from_code()
    }
}

/// 首次运行时用的那份默认：打包在 exe 里的 `assets/defaults/settings.json` +
/// `assets/defaults/menu.runa`。解析失败就退回 [`MenuSettings::default`]。
pub fn bundled_default() -> MenuSettings {
    let mut from_code = MenuSettings::default_from_code();
    if let Some((items, next_id)) = bundled_menu() {
        from_code.items = items;
        from_code.next_id = next_id;
    }
    let Ok(mut bundled) = serde_json::from_str::<MenuSettings>(DEFAULT_SETTINGS) else {
        return from_code;
    };
    bundled.items = from_code.items;
    bundled.next_id = from_code.next_id;
    // 托盘菜单**用打包那份里的**（作者的托盘也是配过的）；只有那份没有才用代码兜底
    if bundled.tray_menu.is_none() {
        bundled.tray_menu = from_code.tray_menu;
    }
    bundled.menu_profile = String::new();
    bundled.pending_profile = None;
    bundled.recent_items = Vec::new();
    bundled.tray_icon = String::new();
    bundled
}

impl MenuSettings {
    pub fn limits_panel_width() -> (f32, f32) {
        (120.0, 640.0)
    }
    /// 自动列宽的上下限：至少放得下一个图标 + 两三个字，最多到 640。
    pub fn limits_panel_max_width() -> (f32, f32) {
        (160.0, 640.0)
    }
    pub fn limits_row_height() -> (f32, f32) {
        (16.0, 96.0)
    }
    pub fn limits_radius() -> (f32, f32) {
        (0.0, 16.0)
    }
    pub fn limits_badge_size() -> (f32, f32) {
        (12.0, 28.0)
    }
    pub fn limits_opacity() -> (f32, f32) {
        (0.35, 1.0)
    }

    /// 托盘那一支（只读）。它平时就躺在 `items` 里，写盘时才拆出去。
    pub fn tray_root(&self) -> Option<&ItemSetting> {
        self.items.iter().find(|item| item.id == TRAY_ID)
    }

    /// 落盘用的那份副本：托盘那一支从 `items` 挪进 `tray_menu`，
    /// `items` 里只剩这套菜单（`.runa` 和 `settings.json` 都是这么分的）。
    fn split_tray(&self) -> MenuSettings {
        let mut file = self.clone();
        // 内存里没有托盘那一支（正常不会）就保留已经读进来的那份，别写成空
        if let Some(tray) = self.tray_root().cloned() {
            file.tray_menu = Some(tray);
        }
        file.items.retain(|item| item.id != TRAY_ID);
        file
    }

    /// 把托盘那一支放回 `items` 末尾（内存里永远是「一棵树」）。
    ///
    /// 两个来源：老配置在 `items` 里，新配置在 `tray_menu` 里 —— 谁有就用谁，
    /// 都没有就补一份默认的（别让用户点托盘发现右键菜单空了）。
    fn adopt_tray(&mut self) {
        let inline = self.items.iter().position(|item| item.id == TRAY_ID);
        let mut tray = match inline {
            Some(pos) => self.items.remove(pos),
            None => self.tray_menu.take().unwrap_or_else(tray_branch),
        };
        tray.kind = ItemKind::Submenu;
        self.items.push(tray);
        self.reassign_tray_ids();
    }

    /// 换一套菜单项，但**托盘那一支不动**（它属于这台机器，不属于某套菜单）。
    ///
    /// `next_id` 也重新对一遍：别人分享来的配置里 `next_id` 可能很小，
    /// 直接用会让新加的项和老项撞 id（撞了 `find_path` 只找到第一个，
    /// 改一条动两条 —— 这类 bug 最难查）。
    pub fn adopt_profile_items(&mut self, items: Vec<ItemSetting>, next_id: u32) {
        let tray = self
            .items
            .iter()
            .position(|item| item.id == TRAY_ID)
            .map(|pos| self.items.remove(pos))
            .or_else(|| self.tray_menu.take())
            .unwrap_or_else(tray_branch);
        self.items = items;
        // 老 `.runa` 里可能带着托盘那一支：丢掉它，托盘以本机为准
        self.items.retain(|item| item.id != TRAY_ID);
        self.next_id = next_id.max(1);
        self.bump_next_id();
        self.ensure_unique_ids();
        self.items.push(tray);
        self.reassign_tray_ids();
        self.normalize_tree();
    }

    /// `next_id` 至少要比现有 `custom-N` 里最大的那个大一号。
    fn bump_next_id(&mut self) {
        fn walk(items: &[ItemSetting], max: &mut u32) {
            for item in items {
                if let Some(rest) = item.id.strip_prefix("custom-") {
                    if let Ok(number) = rest.parse::<u32>() {
                        *max = (*max).max(number);
                    }
                }
                walk(&item.children, max);
            }
        }
        let mut max = self.next_id;
        walk(&self.items, &mut max);
        self.next_id = max + 1;
    }

    /// 托盘子项和菜单项撞 id 就换一个。
    ///
    /// 什么时候会撞：换个菜单配置之后，两边各自从自己的 `next_id` 编号。
    /// 撞了之后点托盘里那一项，改的却是菜单里另一条 —— 必须修。
    fn reassign_tray_ids(&mut self) {
        let Some(pos) = self.items.iter().position(|item| item.id == TRAY_ID) else {
            return;
        };
        fn collect(items: &[ItemSetting], used: &mut std::collections::HashSet<String>) {
            for item in items {
                used.insert(item.id.clone());
                collect(&item.children, used);
            }
        }
        let mut used = std::collections::HashSet::new();
        for (index, item) in self.items.iter().enumerate() {
            if index != pos {
                collect(std::slice::from_ref(item), &mut used);
            }
        }
        let mut tray = self.items.remove(pos);
        fn fix(items: &mut [ItemSetting], used: &mut std::collections::HashSet<String>, next: &mut u32) {
            for item in items.iter_mut() {
                if !used.insert(item.id.clone()) {
                    loop {
                        let id = format!("custom-{}", *next);
                        *next += 1;
                        if used.insert(id.clone()) {
                            item.id = id;
                            break;
                        }
                    }
                }
                fix(&mut item.children, used, next);
            }
        }
        fix(&mut tray.children, &mut used, &mut self.next_id);
        self.items.push(tray);
    }

    /// 所有配了热字符串的节点（按树的顺序，托盘那一支不算）。
    pub fn hotstring_phrases(&self) -> Vec<HotstringPhrase> {
        let mut out = Vec::new();
        collect_hotstrings(&self.items, &mut out);
        out
    }

    /// 当前生效的那一套颜色（跟随系统时按系统明暗取）。
    pub fn active_colors(&self) -> &ThemeColors {
        match self.theme_mode {
            ThemeMode::Light => &self.light,
            ThemeMode::Dark => &self.dark,
            ThemeMode::FollowSystem => {
                if system_is_dark() {
                    &self.dark
                } else {
                    &self.light
                }
            }
        }
    }

    pub fn colors_mut(&mut self, dark: bool) -> &mut ThemeColors {
        if dark {
            &mut self.dark
        } else {
            &mut self.light
        }
    }

    /// 按当前设置构建菜单树。
    /// 构建菜单树。`kinds = Some(..)` 表示「这次选中了这种内容」，
    /// 这时只保留声明了对应类型的项，并把命中的子菜单**摊平**到根层。
    ///
    /// 摊平的理由：一个「文字」子菜单就是给文字准备的菜单，用户掏出菜单是为了直接点，
    /// 不该再多展开一次。多个子菜单都命中时按原顺序拼起来。
    ///
    /// 分隔线：根层自己的分隔线在这种模式下没意义（项的集合都不一样了），丢掉；
    /// 被摊平的子菜单内部的分隔线保留，因为那是作者用来分组的。
    pub fn menu_for(&self, context: Option<&Context>) -> Vec<MenuEntry> {
        let items = match context {
            Some(context) => self.items_for(context),
            None => self.items.clone(),
        };
        self.entries_of(&items)
    }

    /// 托盘图标右键弹的那一菜单（就是树里 `TRAY_ID` 那个子菜单的子项）。
    ///
    /// 风格 / 图标 / 动作 / 快捷键提示全都和弹出菜单同源，因为它本来就是同一棵树的一部分。
    pub fn tray_menu(&self) -> Vec<MenuEntry> {
        let Some(node) = self.items.iter().find(|item| item.id == TRAY_ID) else {
            return Vec::new();
        };
        if !node.enabled {
            return Vec::new();
        }
        self.entries_of(&node.children)
    }

    /// 某个子菜单里的那几项（**只**这一支，不带外层）。
    ///
    /// 给「子菜单自己的触发快捷键」用：按下就应该是「这一支的项，直接摊开摆着」，
    /// 不是整张菜单、也不是只弹一个等着鼠标移上去的名字。
    pub fn submenu_menu(&self, id: &str) -> Vec<MenuEntry> {
        let Some(path) = self.find_path(id) else {
            return Vec::new();
        };
        let Some(node) = self.node(&path) else {
            return Vec::new();
        };
        if !node.enabled {
            return Vec::new();
        }
        self.entries_of(&node.children)
    }

    /// 一组菜单项 → 菜单条目（过滤掉禁用的 / 关掉的分隔线 / 那两支隐藏分支）。
    fn entries_of(&self, items: &[ItemSetting]) -> Vec<MenuEntry> {
        // 「实时 / 仅启动时」生成的那些项不写配置，运行时按 id 挂上来：
        // 这里先合并一层，后面的过滤 / 构建全都按合并后的内容走
        // （空子菜单的隐藏判断也要看合并后的子项，所以必须在过滤之前）。
        let merged: Vec<ItemSetting>;
        let items: &[ItemSetting] = if crate::autofill_runtime::has_any() {
            merged = items
                .iter()
                .map(|item| crate::autofill_runtime::merge(item).unwrap_or_else(|| item.clone()))
                .collect();
            &merged
        } else {
            items
        };
        let mut entries: Vec<MenuEntry> = items
            .iter()
            .filter(|item| !is_hidden_branch(&item.id))
            .filter(|item| item.enabled)
            // 空子菜单不出现：「自动生成」还没跑、或者文件被删光了的时候，
            // 点进去空空如也最难受 —— 干脆不显示，加了项自然就出来了
            .filter(|item| {
                !(item.kind == ItemKind::Submenu
                    && !item.children.iter().any(|child| child.enabled))
            })
            // 「不显示分隔线」只藏装饰用的横线，多列符是排版，留着
            .filter(|item| self.show_separators || !item.is_plain_separator())
            .map(|item| item.to_entry(self.show_badges, self.show_shortcuts, self.auto_icons))
            // 连续分隔线只留一根（关掉中间项之后很容易出现）
            .fold(Vec::new(), |mut acc: Vec<MenuEntry>, entry| {
                let is_sep = matches!(entry, MenuEntry::Separator);
                if is_sep && matches!(acc.last(), Some(MenuEntry::Separator) | None) {
                    return acc;
                }
                acc.push(entry);
                acc
            });
        // 末尾吊着一根分隔线不好看
        while matches!(entries.last(), Some(MenuEntry::Separator)) {
            entries.pop();
        }
        // 多列符：开头 / 结尾 / 连着两根都没有意义（放出来就是一条空列），
        // 顺手收掉，省得作者在树里删了半天还留一条看不见的空列。
        while matches!(entries.first(), Some(MenuEntry::ColumnBreak)) {
            entries.remove(0);
        }
        while matches!(entries.last(), Some(MenuEntry::ColumnBreak)) {
            entries.pop();
        }
        entries.dedup_by(|a, b| {
            matches!(a, MenuEntry::ColumnBreak) && matches!(b, MenuEntry::ColumnBreak)
        });
        entries
    }

    /// 按选中内容的类型筛一遍根层，并决定要不要摊平。
    ///
    /// 规则（和「设置 → 行为」里写的一致）：
    /// - 类型是**继承**的：子菜单标了类型，里面的项默认跟着它（`ContextMode::Inherit`）；
    /// - 菜单项自己标了类型 = 局部覆盖 → 它**浮到根菜单**去，原来那一层就不再重复显示；
    ///   浮上去放哪儿由它自己的 `float_placement` 定（最前面 / 按原顺序 / 子菜单前后）；
    /// - 标了「不参与」的项，就算上级命中也不出现；
    /// - 根层没有上级可跟 → 通用项，有选中内容时隐藏；
    /// - **只有一个**子菜单配了这个筛选（并且筛完还剩东西）时，把它摊平到根层
    ///   （少点一次）；配了两个以上就不摊平，各自留成一个子菜单（否则菜单会糊成一片）。
    ///   根层还有「单独指定的普通项」浮上来时也不摊 —— 两种来源混在一层里分不清。
    ///
    /// 后缀限制（`file_exts`）只在「这一项要求文件」时生效，并且**跟着继承走**：
    /// 子菜单写了 `mp4|mkv`，里面的项默认也只对这两种出现；项自己再写就覆盖它。
    fn items_for(&self, context: &Context) -> Vec<ItemSetting> {
        // 1) 按树剪一遍：留下「自己命中」或「后代里还有东西」的节点，
        //    顺手把「单独指定」的普通项摘出来（它们要去根菜单）
        let rule = self.multi_file_rule;
        let mut floating: Vec<ItemSetting> = Vec::new();
        let mut roots: Vec<ItemSetting> = Vec::new();
        // 「自己配了筛选、并且剪完还剩东西」的子菜单（摊平只看这一份）
        let mut targets: Vec<String> = Vec::new();
        for item in &self.items {
            // 托盘 / 一键直达那两支只在自己的页面里出现，别混进弹出菜单
            if is_hidden_branch(&item.id) {
                continue;
            }
            if let Some(node) = prune_item(
                item,
                &Effective::Generic,
                context,
                rule,
                0,
                &mut floating,
                &mut targets,
            ) {
                roots.push(node);
            }
        }

        // 2) 摊平：只有「唯一一个配了筛选的子菜单」、而且**没有**单独指定的普通项时才做。
        //
        // 有一条浮动项时就不摊了：浮动项本来就排在根层，再把某一支摊平，
        // 两种来源的项混在一层里，用户分不清哪个是「整支」哪个是「单独一条」。
        if std::env::var_os("GPUI_MENU_DEBUG_PRUNE").is_some() {
            eprintln!(
                "[prune] targets={:?} floating={} roots={}",
                targets,
                floating.len(),
                roots.len()
            );
        }
        // 只在「同一支」上配了筛选时才摊：用户可能先在父级配、后来又在子级配，
        // 两个目标落在同一条祖先链上 —— 那就按**最深的那一个**摊，沿途的祖先壳
        // 全部化掉（`dissolve_shells`），根层直接看到最深那一支的子项。
        //
        // 互不为祖先的两支各配各的，不摊（摊了菜单会糊成一片）。
        //
        // 曾经的错误做法：这里限制成「只能有一个目标」，结果父级也配了筛选时
        // 一级壳虽然被 `collapse_pure_shells` 化掉，二级却还剩成一个子菜单，
        // 用户看到的就是「摊是摊了，可还得再点一下，跟没摊一样」。
        let flatten_to = if floating.is_empty() {
            pick_flatten_target(&targets, &self.items)
        } else {
            None
        };
        if let Some(target) = flatten_to {
            roots = flatten_submenu(roots, &target);
            // 摊完根层只剩一只子菜单 —— 它这会儿纯粹是「装着命中项的外壳」
            // （用户那种「后缀 → 影音」的配置就是这样：剪完只剩影音）。
            // 再往下摊一层，呼出菜单直接就是这一支的动作，不用为它多点一次。
            // 根层还有别的项时不动：那样会把人家正经的分组也拆掉。
            if let [only] = roots.as_slice() {
                if is_usable_submenu(only) {
                    roots = only.children.clone();
                }
            }
        }

        // 3) 塌缩「纯外壳」：剪完某子菜单里只剩一个子菜单子项时，把它直接提到
        //    上一级，别让用户点开 A 还得再点一次 B（B 仍是子菜单，不摊它的子项）
        roots = collapse_pure_shells(roots);

        // 4) 浮上来的项按各自指定的位置插回根层
        let order = self.preorder_index();
        merge_floating(roots, floating, &order)
    }

    /// 整棵树的前序遍历序号（id → 序号）。浮动项「按原顺序」插回根层时拿它当尺子。
    fn preorder_index(&self) -> std::collections::HashMap<String, usize> {
        fn walk(
            items: &[ItemSetting],
            next: &mut usize,
            out: &mut std::collections::HashMap<String, usize>,
        ) {
            for item in items {
                out.insert(item.id.clone(), *next);
                *next += 1;
                walk(&item.children, next, out);
            }
        }
        let mut out = std::collections::HashMap::new();
        let mut next = 0;
        walk(&self.items, &mut next, &mut out);
        out
    }

    /// 「只弹这一支」用的菜单项：把 `id` 这个子菜单单独拿出来。
    ///
    /// 先按这次选中内容筛一遍（里面的项一样要过滤），筛空了就退回不过滤的原样 ——
    /// 用户按快捷键就是要看这一支，弹个空菜单没有意义。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn submenu_entry_for(
        &self,
        id: &str,
        context: Option<&Context>,
    ) -> Option<MenuEntry> {
        fn find<'a>(items: &'a [ItemSetting], id: &str) -> Option<&'a ItemSetting> {
            for item in items {
                if item.id == id {
                    return Some(item);
                }
                if let Some(hit) = find(&item.children, id) {
                    return Some(hit);
                }
            }
            None
        }
        let filtered = context.map(|context| self.items_for(context));
        let node = filtered
            .as_deref()
            .and_then(|items| find(items, id))
            .filter(|node| is_usable_submenu(node))
            // 筛没了就退回不过滤的原样：按快捷键就是要看这一支
            .or_else(|| find(&self.items, id).filter(|node| node.is_submenu()))?;
        Some(node.to_entry(
            self.show_badges,
            self.show_shortcuts,
            self.auto_icons,
        ))
    }

    // ---- 树操作（路径 = 从根到该节点的下标序列）----

    pub fn node(&self, path: &[usize]) -> Option<&ItemSetting> {
        let (first, rest) = path.split_first()?;
        let mut node = self.items.get(*first)?;
        for index in rest {
            node = node.children.get(*index)?;
        }
        Some(node)
    }

    /// 从根到 `path` 各级的文案（**不含**最后那个自己）。
    ///
    /// 悬停提示里的 `{path}` 用它：「开发工具 › 编辑」比孤零零一个「编辑」好认。
    pub fn trail_labels(&self, path: &[usize]) -> Vec<String> {
        let mut labels = Vec::new();
        for depth in 1..path.len() {
            if let Some(node) = self.node(&path[..depth]) {
                let label = node.label.trim();
                labels.push(if label.is_empty() {
                    crate::i18n::t("（未命名）").to_string()
                } else {
                    label.to_string()
                });
            }
        }
        labels
    }

    pub fn sibling_list_mut(&mut self, parent: &[usize]) -> Option<&mut Vec<ItemSetting>> {
        if parent.is_empty() {
            return Some(&mut self.items);
        }
        let (first, rest) = parent.split_first()?;
        let mut node = self.items.get_mut(*first)?;
        for index in rest {
            node = node.children.get_mut(*index)?;
        }
        Some(&mut node.children)
    }

    pub fn node_mut(&mut self, path: &[usize]) -> Option<&mut ItemSetting> {
        let (last, parent) = path.split_last()?;
        self.sibling_list_mut(parent)?.get_mut(*last)
    }

    /// 按 id 找路径（编辑窗口开着时树可能变了，所以用 id 而不是下标）。
    pub fn find_path(&self, id: &str) -> Option<Vec<usize>> {
        fn walk(items: &[ItemSetting], id: &str, path: &mut Vec<usize>) -> bool {
            for (index, item) in items.iter().enumerate() {
                path.push(index);
                if item.id == id || walk(&item.children, id, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
        let mut path = Vec::new();
        if walk(&self.items, id, &mut path) {
            Some(path)
        } else {
            None
        }
    }

    /// 新建子项的 id。
    pub fn next_item_id(&mut self) -> String {
        // 跳过已经被占用的 id：默认菜单里托盘那一支就有 `custom-1`，
        // 而 `next_id` 的初值是 1 —— 不查的话新建的项会和它撞名，
        // 之后 `find_path` 找到的是**旧的**那一个，改一条动两条。
        loop {
            let id = format!("custom-{}", self.next_id);
            self.next_id += 1;
            if self.find_path(&id).is_none() {
                return id;
            }
        }
    }

    /// `insert_sibling` 的「指定容器」版：`after` 不在 `container` 这一层就补到容器末尾。
    ///
    /// 树编辑器现在三处共用（根 / 托盘那一支 / 某个子菜单的子项），所以「新项加到哪儿」
    /// 不能只认根层 —— 否则在托盘页点「添加菜单项」会插到弹出菜单的根部去。
    fn insert_sibling_in(
        &mut self,
        container: &[usize],
        after: Option<&[usize]>,
        node: ItemSetting,
    ) -> String {
        let id = node.id.clone();
        if let Some(path) = after.filter(|path| !path.is_empty()) {
            if let Some((last, parent)) = path.split_last() {
                // `after` 只要在容器里面（含容器本身这一层）就贴着它插；
                // 不在容器里就说明选中项是别的分支的，退回「加到容器末尾」。
                if parent.starts_with(container) {
                    if let Some(list) = self.sibling_list_mut(parent) {
                        let at = (*last + 1).min(list.len());
                        list.insert(at, node);
                        return id;
                    }
                }
            }
        }
        if let Some(list) = self.sibling_list_mut(container) {
            // 根层末尾其实躺着托盘那一支（它只在这份内存树里，不参与弹出菜单）。
            // 直接 push 会插到它后面 —— 文件顺序上就变成「托盘前面的项全在 Root 页，
            // 新项排在托盘之后」，看着没差，但「最后一项」的判断会跟着错。
            // 所以根层一律插到托盘**之前**。
            let at = if container.is_empty() {
                list.iter()
                    .position(|item| item.id == TRAY_ID)
                    .unwrap_or(list.len())
            } else {
                list.len()
            };
            list.insert(at, node);
        }
        id
    }

    /// 加一个普通菜单项（插在选中项后面；没选中就加到末尾）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_item(&mut self, after: Option<&[usize]>) -> String {
        self.add_item_in(&[], after)
    }

    /// 往「`container` 这一层的选中项后面」加一个普通菜单项。
    pub fn add_item_in(&mut self, container: &[usize], after: Option<&[usize]>) -> String {
        let id = self.next_item_id();
        let mut node = ItemSetting::item(id.clone(), crate::i18n::t("新建菜单项"));
        // 新建出来最常见的就是「运行一个程序」：先摆一条空的运行程序动作，
        // 打开编辑窗口就能直接填路径，省掉「添加动作 → 选类型」两步。
        // 空值在真正执行时会被跳过（`execute_step` 里 `if value.is_empty()`），
        // 所以忘了填也不会误跑什么。
        node.actions = vec![ActionStep::new(ActionKind::Run, String::new())];
        self.insert_sibling_in(container, after, node)
    }

    /// 把动作库里的一条动作做成**可独立粘贴**的菜单项（不进配置，只给剪贴板用）。
    ///
    /// 序列化出来的就是 `.runa` 里 `items` 数组的一个元素；`paste_items` 的
    /// `parse_paste` 也认单个对象，所以粘进编辑器、粘回菜单项列表都行。
    pub fn action_item(
        action_id: &str,
        name: &str,
        params: std::collections::BTreeMap<String, String>,
    ) -> ItemSetting {
        // id 从动作 id 派生（`sys.volume` → `act-sys-volume`），只是为了在 JSON 里好认；
        // 真正粘进菜单时 `paste_items` 会重新编号。
        let id = format!("act-{}", action_id.replace('.', "-"));
        let mut node = ItemSetting::item(id, name);
        node.actions = vec![ActionStep::with_params(
            ActionKind::Builtin,
            action_id.to_string(),
            params,
        )];
        node
    }

    /// 把一段（从剪贴板粘来的）菜单项插进 `container`，返回顶层新 id。
    ///
    /// id 全部重新分配：别处复制来的配置里 id 和本地撞了会「改一条动两条」。
    /// 子项、动作、热字符串、适用内容都原样保留 —— 这份东西是拿去分享的。
    pub fn paste_items(
        &mut self,
        container: &[usize],
        after: Option<&[usize]>,
        items: Vec<ItemSetting>,
    ) -> Vec<String> {
        let mut ids = Vec::new();
        let mut cursor: Option<Vec<usize>> = after.map(|path| path.to_vec());
        for mut item in items {
            let id = self.renumber(&mut item);
            self.insert_sibling_in(container, cursor.as_deref(), item);
            cursor = self.find_path(&id);
            ids.push(id);
        }
        ids
    }

    /// 给整棵子树换一批没人用过的新 id（粘贴进来的项用）。
    fn renumber(&mut self, item: &mut ItemSetting) -> String {
        let id = self.next_item_id();
        item.id = id.clone();
        for child in &mut item.children {
            self.renumber(child);
        }
        for child in &mut item.parked_children {
            self.renumber(child);
        }
        id
    }

    /// 一次加好几条「运行程序」菜单项（把文件拖进列表时用）。
    ///
    /// `specs` 是 `(菜单项名称, 运行程序参数)`，按给的顺序插在 `after` 后面 ——
    /// 一条条调 `add_item_in` 也行，但那样每加一条都要重新按 id 找一次路径，
    /// 拖十个文件就是十次全树查找，还会因为 `normalize_tree` 反复重排。
    pub fn add_program_items(
        &mut self,
        container: &[usize],
        after: Option<&[usize]>,
        specs: &[(String, String)],
    ) -> Vec<String> {
        let mut ids = Vec::new();
        // 每插一条，下一条就贴在它后面，保证顺序和拖进来的顺序一致
        let mut cursor: Option<Vec<usize>> = after.map(|path| path.to_vec());
        for (label, program) in specs {
            let id = self.next_item_id();
            let mut node = ItemSetting::item(id.clone(), label.clone());
            node.actions = vec![ActionStep::new(ActionKind::Run, program.clone())];
            self.insert_sibling_in(container, cursor.as_deref(), node);
            // 找到刚插进去的位置，当成下一条的锚点
            cursor = self.find_path(&id);
            ids.push(id);
        }
        self.normalize_tree();
        ids
    }

    /// 给「运行程序」那一栏的值套上双引号（已经套过就原样返回）。
    ///
    /// 为什么一律加：路径里有空格（`C://Program Files\...`、`我的 文件`）时，
    /// 不加引号会被 shell 按空格拆成两个参数，程序根本起不来。裸文件名同理 ——
    /// `my tool.exe` 不加引号也认不出来。加了引号对没有空格的路径没有任何坏处。
    pub fn quote_path(value: &str) -> String {
        let value = value.trim();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            return value.to_string();
        }
        format!("\"{value}\"")
    }

    /// 一键直达的「直达项」放在哪一层：隐藏根层 `__magic__`（不在菜单里出现）。
    ///
    /// 老配置里没有这一支，第一次要用的时候当场建出来。
    pub fn magic_targets_path(&mut self) -> Vec<usize> {
        if let Some(path) = self.find_path(MAGIC_ID) {
            return path;
        }
        let mut node = ItemSetting::item(MAGIC_ID, "一键直达（不在菜单里）");
        node.kind = ItemKind::Submenu;
        self.items.push(node);
        self.find_path(MAGIC_ID).unwrap_or_default()
    }

    /// 新建一个「直达项」（普通项 / 子菜单），返回它的 id。
    ///
    /// 它就是一棵普通的菜单项 —— 动作、图标、子菜单全是现成那套；只是挂在隐藏层里，
    /// 所以菜单里看不到它，只有绑了它的规则会用到。
    pub fn add_magic_target(&mut self, submenu: bool) -> String {
        let container = self.magic_targets_path();
        let id = self.next_item_id();
        let mut node = ItemSetting::item(
            id.clone(),
            crate::i18n::t(if submenu { "新建直达子菜单" } else { "新建直达项" }),
        );
        if submenu {
            // 同上：空着就行，用户自己加
            node.kind = ItemKind::Submenu;
        } else {
            node.actions = vec![ActionStep::new(ActionKind::Run, String::new())];
        }
        if let Some(list) = self.sibling_list_mut(&container) {
            list.push(node);
        }
        self.normalize_tree();
        id
    }

    /// 直接查一个节点（按 id）。
    pub fn item_ref(&self, id: &str) -> Option<&ItemSetting> {
        self.find_path(id).and_then(|path| self.node(&path))
    }

    /// 往 `container` 这一层加一个子菜单。
    pub fn add_submenu_in(&mut self, container: &[usize], after: Option<&[usize]>) -> String {
        let id = self.next_item_id();
        let mut node = ItemSetting::item(id.clone(), "新建子菜单");
        node.kind = ItemKind::Submenu;
        // 不再塞「新建子项（改个名字，或者删掉）」占位：空子菜单是允许的，
        // 界面上会给提示，菜单里则自动隐藏（加了项才出现）
        self.insert_sibling_in(container, after, node)
    }

    /// 往 `container` 这一层加一条分隔线。
    pub fn add_separator_in(&mut self, container: &[usize], after: Option<&[usize]>) -> String {
        let id = self.next_item_id();
        let node = ItemSetting {
            kind: ItemKind::Separator,
            ..ItemSetting::item(id.clone(), "")
        };
        self.insert_sibling_in(container, after, node)
    }

    /// 往 `container` 这一层加一个「多列符」：从这里另起一列。
    pub fn add_column_break_in(&mut self, container: &[usize], after: Option<&[usize]>) -> String {
        let id = self.next_item_id();
        let node = ItemSetting {
            kind: ItemKind::Separator,
            column_break: true,
            ..ItemSetting::item(id.clone(), "")
        };
        self.insert_sibling_in(container, after, node)
    }

    /// 在 `parent` 这一组的末尾加一个子项（父项没子菜单就会变成子菜单）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_child(&mut self, parent: &[usize]) {
        let id = self.next_item_id();
        if let Some(list) = self.sibling_list_mut(parent) {
            list.push(ItemSetting::item(id, "新建子项"));
        }
        self.normalize_tree();
    }

    /// 加一条分隔线（同样插在选中项后面）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_separator(&mut self, after: Option<&[usize]>) -> String {
        self.add_separator_in(&[], after)
    }

    pub fn remove_at(&mut self, path: &[usize]) {
        let Some((last, parent)) = path.split_last() else {
            return;
        };
        if let Some(list) = self.sibling_list_mut(parent) {
            if *last < list.len() {
                list.remove(*last);
            }
        }
        self.normalize_tree();
    }

    /// 取出某个节点（连同它的子树）。
    fn take_node(&mut self, path: &[usize]) -> Option<ItemSetting> {
        let (last, parent) = path.split_last()?;
        let list = self.sibling_list_mut(parent)?;
        if *last >= list.len() {
            return None;
        }
        Some(list.remove(*last))
    }

    /// 把摘下来的节点塞回原位（拖拽中途失败时用，保证「拖了没落成也不会丢」）。
    fn put_back(&mut self, parent: Vec<usize>, index: usize, node: ItemSetting) {
        if let Some(list) = self.sibling_list_mut(&parent) {
            let at = index.min(list.len());
            list.insert(at, node);
        } else {
            self.items.push(node);
        }
        self.normalize_tree();
    }

    /// 整棵树跑一遍 `normalize`（有子项 = 子菜单，没子项 = 普通项）。
    pub fn normalize_tree(&mut self) {
        fn walk(items: &mut [ItemSetting]) {
            for item in items.iter_mut() {
                item.normalize();
                // 图标位存的是程序目录里的绝对路径：改成相对的，整个目录搬走也不断
                if let Some(relative) = crate::icon::make_relative(&item.badge) {
                    item.badge = relative;
                }
                walk(&mut item.children);
            }
        }
        walk(&mut self.items);
        // 托盘那一支**永远是子菜单**：子项被删空也留着外壳。
        // 不留的话 `normalize` 会把它降成普通项，托盘右键菜单就整个消失了 ——
        // 用户在「托盘」页删光了子项，再去点托盘图标会以为程序坏了。
        if let Some(tray) = self.items.iter_mut().find(|item| item.id == TRAY_ID) {
            tray.kind = ItemKind::Submenu;
        }
    }

    /// 读盘之后调一次：迁移旧 JSON + 归一 kind + 把重复的 id 改成唯一。
    pub fn migrate(&mut self) {
        // 托盘先归位（老格式在 items 里，新格式在 tray_menu 里），下面统一迁移
        self.adopt_tray();
        for item in &mut self.items {
            item.migrate();
        }
        self.ensure_unique_ids();
        self.normalize_tree();
        // 悬停提示：老版本只有「显示动作 / 快捷键 / 备注 / 类型」四个开关，
        // 现在是一份模板 —— 把它们拼成模板，之后界面里改模板说了算
        self.tooltip.migrate();
        self.opacity = self.opacity.clamp(
            Self::limits_opacity().0,
            Self::limits_opacity().1,
        );
        // 列宽 / 行高范围后来调大了，老值可能落在新范围之外，夹一下
        self.panel_width = self
            .panel_width
            .clamp(Self::limits_panel_width().0, Self::limits_panel_width().1);
        // 老配置里没有这个字段（读出来是 0）：直接给默认值，别夹成 160
        if !self.panel_max_width.is_finite() || self.panel_max_width <= 0.0 {
            self.panel_max_width = default_max_panel_width();
        }
        self.panel_max_width = self.panel_max_width.clamp(
            Self::limits_panel_max_width().0,
            Self::limits_panel_max_width().1,
        );
        self.row_height = self
            .row_height
            .clamp(Self::limits_row_height().0, Self::limits_row_height().1);
    }

    /// 同一棵树里 id 必须唯一。
    ///
    /// 选中、插在谁后面、删除、拖拽全是按 id 找路径的（`find_path` 返回第一个匹配），
    /// 老版本的分隔线全都叫 `sep`，结果点一条就把整列分隔线一起点亮、
    /// 新项也总是插到第一条分隔线后面。发现重名就发一个新 id。
    pub fn ensure_unique_ids(&mut self) {
        fn walk(items: &mut [ItemSetting], seen: &mut std::collections::HashSet<String>) {
            for item in items.iter_mut() {
                if item.id.trim().is_empty() || !seen.insert(item.id.clone()) {
                    item.id = format!("fix-{}", next_separator_id());
                    seen.insert(item.id.clone());
                }
                walk(&mut item.children, seen);
            }
        }
        let mut seen = std::collections::HashSet::new();
        walk(&mut self.items, &mut seen);
    }

    /// 拖拽：把 `from` 这一个节点挪到 `target` 指定的位置（可以跨层）。
    ///
    /// 谁能当落点、落点前后怎么算，都在这里定：
    /// - 不能挪进自己或自己的子孙里；
    /// - 落点是「谁的子项」时，那个节点自动变成子菜单（`normalize` 负责）；
    /// - 原父级被子项搬空后自动退回普通项。
    pub fn move_node(&mut self, from: &[usize], target: &DropTarget) -> bool {
        // 分隔线不能当爹（它没有子项，`normalize` 会把塞进去的东西清掉）：
        // 拖到它「里面」= 插在它后面
        let target = match target {
            DropTarget::Into(path)
                if self
                    .node(path)
                    .map(|node| node.kind == ItemKind::Separator)
                    .unwrap_or(false) =>
            {
                DropTarget::After(path.clone())
            }
            other => other.clone(),
        };
        let target = &target;
        let Some((from_index, from_parent)) =
            from.split_last().map(|(last, p)| (*last, p.to_vec()))
        else {
            return false;
        };
        let (dest_parent, dest_index) = match target {
            DropTarget::Before(path) => {
                let Some((last, parent)) = path.split_last() else {
                    return false;
                };
                (parent.to_vec(), *last)
            }
            DropTarget::After(path) => {
                let Some((last, parent)) = path.split_last() else {
                    return false;
                };
                (parent.to_vec(), last + 1)
            }
            DropTarget::Into(path) => (path.clone(), usize::MAX),
        };
        // 托盘那一支不算「这套菜单」的一部分：它只是和菜单项挤在同一份内存树里。
        // 拖着拖着掉进托盘（或者从托盘拖出来）都不是用户想要的，这里双向夹住 ——
        // 落在哪边就留在哪边，绝不跨过去。
        let tray_path = self.find_path(TRAY_ID);
        let owner = |path: &[usize], tray: &Option<Vec<usize>>| -> bool {
            match tray {
                Some(tray) => path.starts_with(tray),
                None => false,
            }
        };
        if from == tray_path.as_deref().unwrap_or(&[]) {
            // 托盘这一支本身不能拖
            return false;
        }
        let from_in_tray = owner(from, &tray_path);
        let (mut dest_parent, mut dest_index) = (dest_parent, dest_index);
        if from_in_tray != owner(&dest_parent, &tray_path) {
            dest_parent = if from_in_tray {
                tray_path.clone().unwrap_or_default()
            } else {
                Vec::new()
            };
            dest_index = usize::MAX;
        }
        // 目标不能是自己、也不能在自己的子树里
        if dest_parent.starts_with(from) {
            return false;
        }
        // 目标父级必须存在（用 id 记下来：取走节点后下标会变）
        let dest_parent_id = if dest_parent.is_empty() {
            None
        } else {
            match self.node(&dest_parent) {
                Some(node) => Some(node.id.clone()),
                None => return false,
            }
        };

        let Some(node) = self.take_node(from) else {
            return false;
        };

        // 取走之后，原父级可能空了 → 降级
        if !from_parent.is_empty() {
            let emptied = self
                .sibling_list_mut(&from_parent)
                .map(|list| list.is_empty())
                .unwrap_or(false);
            if emptied {
                if let Some(parent_node) = self.node_mut(&from_parent) {
                    parent_node.kind = ItemKind::Item;
                }
            }
        }

        // 目标父级重新按 id 定位（取走节点可能让下标整体前移）
        let dest_parent = match &dest_parent_id {
            None => Vec::new(),
            Some(id) => match self.find_path(id) {
                Some(path) => path,
                None => {
                    // 节点已经从树上摘下来了：**必须放回去**，
                    // 否则一次失败的拖拽就把用户这一项弄丢了（「拖到某处会消失」）。
                    self.put_back(from_parent.clone(), from_index, node);
                    return false;
                }
            },
        };

        let mut index = dest_index;
        if dest_parent == from_parent && dest_index != usize::MAX && from_index < index {
            index -= 1;
        }

        let Some(list) = self.sibling_list_mut(&dest_parent) else {
            self.put_back(from_parent.clone(), from_index, node);
            return false;
        };
        let at = if index == usize::MAX {
            list.len()
        } else {
            index.min(list.len())
        };
        list.insert(at, node);

        // 落点的父级变成子菜单、原父级要不要降级，都交给 normalize
        self.normalize_tree();
        true
    }

    /// 把子菜单**还原成普通菜单项**：子项收进 `parked_children`（看不见，但没丢）。
    ///
    /// 以前是把子项拍平成同级兄弟。问题在于「点一下看看会怎样」的代价太大：
    /// 原来那一支被拆开了，想还原得一个个拖回去。现在只是「收起来」——
    /// 再点「添加子菜单」，原样放回来。
    pub fn demote_submenu(&mut self, path: &[usize]) -> bool {
        let Some(node) = self.node_mut(path) else {
            return false;
        };
        if node.kind != ItemKind::Submenu && node.children.is_empty() {
            return false;
        }
        let children: Vec<ItemSetting> = node.children.drain(..).collect();
        // 之前已经收着的排在后面：用户可能是「收起来 → 又加了新的 → 再收起来」
        node.parked_children.extend(children);
        node.kind = ItemKind::Item;
        self.normalize_tree();
        true
    }

    /// 把普通菜单项**升级成子菜单**：先把收着的子项放回来，没有就建一个空的子项。
    ///
    /// 动作不清空（`normalize` 也不会再清）—— 用户可能只是好奇点一下，改回
    /// 「还原为菜单项」时动作还在。
    pub fn promote_to_submenu(&mut self, path: &[usize]) -> bool {
        let Some(node) = self.node_mut(path) else {
            return false;
        };
        if node.kind == ItemKind::Separator {
            return false;
        }
        node.kind = ItemKind::Submenu;
        if node.children.is_empty() {
            // 之前收起来的子项原样放回来；一个都没有就**让它空着**（不再硬塞一个
            // 「新建子项」占位 —— 用户想加什么自己加，空着的时候界面会提示、
            // 菜单里也会自动藏起来）
            let restored: Vec<ItemSetting> = node.parked_children.drain(..).collect();
            node.children = restored;
        }
        self.normalize_tree();
        true
    }

    pub fn reset_items(&mut self) {
        // 只重置菜单那一部分，托盘右键菜单留着（它不是「菜单配置」的一部分）
        let tray = self
            .items
            .iter()
            .position(|item| item.id == TRAY_ID)
            .map(|pos| self.items.remove(pos))
            .or_else(|| self.tray_menu.take())
            .unwrap_or_else(tray_branch);
        // 「还原默认菜单」还原成**打包在 exe 里的那套**（`assets/defaults/menu.runa`），
        // 不是代码里写死的示例菜单 —— 用户点这个按钮就是想回到「刚装好时那样」。
        let (mut items, next_id) = match bundled_menu() {
            Some((items, next_id)) => (items, next_id),
            None => (default_items(), 1),
        };
        items.retain(|item| item.id != TRAY_ID);
        items.push(tray);
        self.items = items;
        self.next_id = next_id;
        self.bump_next_id();
        self.reassign_tray_ids();
        self.normalize_tree();
    }

    pub fn reset_all(&mut self) {
        *self = Self::default();
    }
}

/// 从一个字符串里取出可用的加速键：只认**一个** ASCII 字母或数字，统一成大写。
///
/// 中文/符号一律不认（那些当不了快捷键），返回 `None` 就等于「没设」。
pub fn accel_char(text: &str) -> Option<char> {
    let first = text.trim().chars().next()?;
    first
        .is_ascii_alphanumeric()
        .then(|| first.to_ascii_uppercase())
}

/// 内置默认菜单：三级级联 + 几种动作示例。
///
/// 顶层里 `最近打开 / 编辑 / 视图` 是子菜单（不配动作），其余是普通项。
pub fn default_items() -> Vec<ItemSetting> {
    vec![
        ItemSetting::item("new-file", crate::i18n::t("新建文件"))
            .accel("N")
            .shortcut("Ctrl+N")
            .action(ActionKind::Run, "notepad.exe"),
        ItemSetting::item("open-file", crate::i18n::t("打开文件…"))
            .accel("O")
            .shortcut("Ctrl+O")
            .action(ActionKind::Run, "explorer ."),
        ItemSetting::item("recent", crate::i18n::t("最近打开"))
            .accel("R")
            .children(vec![
                ItemSetting::item("recent-main", "main.rs"),
                ItemSetting::item("recent-design", "菜单组件设计稿.md"),
                ItemSetting::item("recent-cargo", "Cargo.toml"),
                ItemSetting::separator(),
                ItemSetting::item("recent-clear", crate::i18n::t("清空最近记录")),
            ]),
        ItemSetting::separator(),
        ItemSetting::item("edit", crate::i18n::t("编辑")).accel("E").children(vec![
            ItemSetting::item("copy", crate::i18n::t("复制")).action(ActionKind::Keys, "Ctrl+C"),
            ItemSetting::item("paste", crate::i18n::t("粘贴")).action(ActionKind::Keys, "Ctrl+V"),
            ItemSetting::item("cut", crate::i18n::t("剪切")).action(ActionKind::Keys, "Ctrl+X"),
            ItemSetting::separator(),
            ItemSetting::item("naming", crate::i18n::t("变量命名")).children(vec![
                ItemSetting::item("naming-camel", crate::i18n::t("1 驼峰命名（camelCase）")),
                ItemSetting::item("naming-pascal", crate::i18n::t("2 帕斯卡命名（PascalCase）")),
                ItemSetting::item("naming-snake", crate::i18n::t("3 下划线命名（snake_case）")),
                ItemSetting::item("naming-kebab", crate::i18n::t("4 短横线命名（kebab-case）")),
                ItemSetting::item("naming-screaming", crate::i18n::t("5 常量命名（SCREAMING_SNAKE_CASE）")),
            ]),
        ]),
        ItemSetting::item("view", crate::i18n::t("视图")).accel("V").children(vec![
            ItemSetting::item("sidebar", crate::i18n::t("显示侧边栏")).shortcut("Ctrl+B"),
            ItemSetting::item("theme", crate::i18n::t("切换主题")),
            ItemSetting::separator(),
            ItemSetting::item("zoom", crate::i18n::t("缩放")).children(vec![
                ItemSetting::item("zoom-in", crate::i18n::t("放大")).shortcut("Ctrl+="),
                ItemSetting::item("zoom-out", crate::i18n::t("缩小")).shortcut("Ctrl+-"),
                ItemSetting::item("zoom-reset", crate::i18n::t("恢复默认")).shortcut("Ctrl+0"),
            ]),
        ]),
        ItemSetting::separator(),
        // 多条动作的示例：先打一行签名，等 150ms，再回车
        ItemSetting::item("signature", crate::i18n::t("插入签名"))
            .accel("S")
            .steps(vec![
                ActionStep::new(ActionKind::Text, "—— 来自 RunAny_RS").delay(150),
                ActionStep::new(ActionKind::Keys, "Enter"),
            ]),
        ItemSetting::item("docs", crate::i18n::t("打开项目主页"))
            .accel("H")
            .action(ActionKind::Url, "https://gpui.rs")
            .shortcut("Ctrl+D"),
        ItemSetting::item("settings", crate::i18n::t("打开设置"))
            .accel("P")
            .action(ActionKind::Builtin, BuiltinAction::OpenSettings.key()),
        ItemSetting::item("quit", crate::i18n::t("退出"))
            .accel("Q")
            .shortcut("Ctrl+Q")
            .action(ActionKind::Builtin, BuiltinAction::Quit.key()),
        // 托盘图标右键弹的菜单：树里的一个普通子菜单，弹出菜单时会被跳过
        tray_branch(),
    ]
}

/// 托盘右键菜单的默认内容（也是新建 / 恢复默认时的那一份）。
pub fn tray_branch() -> ItemSetting {
    ItemSetting::item(TRAY_ID, crate::i18n::t("托盘右键菜单"))
        .children(vec![
            ItemSetting::item("tray-settings", crate::i18n::t("设置…"))
                .accel("P")
                .action(ActionKind::Builtin, BuiltinAction::OpenSettings.key()),
            ItemSetting::item("custom-1", crate::i18n::t("打开配置目录"))
                .accel("J")
                .action(ActionKind::Builtin, "open-config-dir"),
            ItemSetting::item("tray-popup", crate::i18n::t("打开完整菜单"))
                .accel("M")
                .badge("☰")
                .action(ActionKind::Builtin, "open-menu"),
            ItemSetting::item("tray-quit", crate::i18n::t("退出"))
                .accel("Q")
                .action(ActionKind::Builtin, BuiltinAction::Quit.key()),
        ])
}

// ---------------------------------------------------------------------------
// 线程本地快照
// ---------------------------------------------------------------------------

thread_local! {
    static CURRENT: RefCell<MenuSettings> = RefCell::new(MenuSettings::default());
    /// 每改一次设置 +1：菜单树只在版本变了才重建（弹一次不重建几千条）
    static REVISION: Cell<u64> = const { Cell::new(0) };
    /// 最近一次问到的系统明暗（跟随系统模式用）
    static SYSTEM_DARK: Cell<bool> = const { Cell::new(false) };
}

/// 当前设置的版本号。
pub fn revision() -> u64 {
    REVISION.with(|slot| slot.get())
}

fn bump_revision() {
    REVISION.with(|slot| slot.set(slot.get().wrapping_add(1)));
}

pub fn system_is_dark() -> bool {
    SYSTEM_DARK.with(|slot| slot.get())
}

/// 系统切了明暗：跟着重算配色。
pub fn set_system_dark(dark: bool) {
    if SYSTEM_DARK.with(|slot| slot.get()) == dark {
        return;
    }
    SYSTEM_DARK.with(|slot| slot.set(dark));
    apply_palette_of_current();
}

fn apply_palette_of_current() {
    let palette = CURRENT.with(|slot| theme::Palette::build(slot.borrow().active_colors()));
    theme::set_palette(palette);
}

/// 当前设置（克隆；菜单构建用，弹一次才调一次）。
pub fn current() -> MenuSettings {
    CURRENT.with(|slot| slot.borrow().clone())
}

/// 不克隆整个结构，只读一个字段。
pub fn read<T>(f: impl FnOnce(&MenuSettings) -> T) -> T {
    CURRENT.with(|slot| f(&slot.borrow()))
}

// ---------------------------------------------------------------------------
// 按选中内容筛选：一趟树剪 + 有条件的摊平
// ---------------------------------------------------------------------------

/// 一个节点在「按选中内容筛选」时实际生效的条件。
///
/// 关键在**继承**：子菜单标了「文字」，它里面的项就都跟着是「文字」，
/// 不用一个个去标；后缀限制（`file_exts`）也一样继承 —— 自己写了就盖掉上级的。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Effective {
    /// 一路继承到根都没指定 = 通用项（有选中内容时隐藏）
    Generic,
    /// 不参与筛选（整支都不出现）
    Never,
    /// 这些类型里任意一个命中就显示，另外还可能要满足后缀限制
    Kinds {
        kinds: Vec<SelectionKind>,
        /// 空的 = 不限后缀
        exts: Vec<String>,
    },
}

impl Effective {
    fn of(item: &ItemSetting, parent: &Effective) -> Effective {
        match item.context_mode {
            ContextMode::Inherit => parent.clone(),
            ContextMode::Never => Effective::Never,
            // 「单独指定」但一个类型都没勾：等于谁都不命中
            ContextMode::Own if item.contexts.is_empty() => Effective::Kinds {
                kinds: Vec::new(),
                exts: Vec::new(),
            },
            ContextMode::Own => Effective::Kinds {
                kinds: item.contexts.clone(),
                exts: item.ext_list(),
            },
        }
    }

    fn hits(&self, context: &Context, rule: MultiFileRule) -> bool {
        match self {
            // 通用项在有选中内容时不出现；「不参与」永远不出现
            Effective::Kinds { kinds, exts } => {
                kinds.iter().any(|kind| context.has(*kind))
                    // 后缀只对「要求文件」的项有意义；不限文件时写了也不生效
                    && (exts.is_empty()
                        || !kinds.contains(&SelectionKind::File)
                        || context.exts_hit(exts, rule))
            }
            Effective::Generic | Effective::Never => false,
        }
    }
}

/// 剪一枝子树：留下「自己命中」或「后代里还有能显示的」节点。
///
/// `floating` 收集「单独指定了类型」的普通项 —— 它们要浮到根菜单，
/// 所以在原来的位置返回 `None`（少一次重复显示）。
///
/// `filtered` 收集「自己配了这个筛选、而且**剪完还活着**」的子菜单：要不要摊平
/// 只看这一份。以前是另跑一趟 `filter_submenus` 数「Own 且命中」的子菜单，
/// 完全不看它里面还剩不剩东西 —— 一个配了筛选却全军覆没（被后缀挡掉 / 子项全禁用）
/// 的子菜单白白算成第二个，唯一活下来那一支就摊不平，用户还得点一层。
fn prune_item(
    item: &ItemSetting,
    parent: &Effective,
    context: &Context,
    rule: MultiFileRule,
    depth: usize,
    floating: &mut Vec<ItemSetting>,
    filtered: &mut Vec<String>,
) -> Option<ItemSetting> {
    if item.kind == ItemKind::Separator {
        return Some(item.clone());
    }
    if !item.enabled {
        return None;
    }
    // 「不参与」会挡住整支：下面的项就算自己指定了类型也不再例外
    if matches!(parent, Effective::Never) {
        return None;
    }
    let effective = Effective::of(item, parent);
    if item.is_submenu() {
        let mut children = Vec::new();
        for child in &item.children {
            if let Some(node) =
                prune_item(child, &effective, context, rule, depth + 1, floating, filtered)
            {
                children.push(node);
            }
        }
        // 一个能用的子项都不剩，这个子菜单也别留着（空壳点进去没东西）
        if !children
            .iter()
            .any(|child| child.kind != ItemKind::Separator)
        {
            return None;
        }
        if item.context_mode == ContextMode::Own {
            filtered.push(item.id.clone());
        }
        let mut node = item.clone();
        node.children = children;
        return Some(node);
    }
    if !effective.hits(context, rule) {
        return None;
    }
    if depth > 0 && item.context_mode == ContextMode::Own {
        floating.push(item.clone());
        return None;
    }
    Some(item.clone())
}

fn contains_id(items: &[ItemSetting], id: &str) -> bool {
    items
        .iter()
        .any(|item| item.id == id || contains_id(&item.children, id))
}

/// 剪完之后这一支还「点得进去吗」：是子菜单、并且至少剩一个不是分隔线的子项。
#[cfg_attr(not(test), allow(dead_code))]
fn is_usable_submenu(node: &ItemSetting) -> bool {
    node.is_submenu()
        && node
            .children
            .iter()
            .any(|child| child.kind != ItemKind::Separator)
}

/// 塌缩「纯外壳」子菜单。
///
/// 剪枝之后，一个子菜单可能只剩一个子菜单子项（其余都被筛掉了）。这时它只是
/// 「装着里面那个子菜单的外壳」—— 用户点开 A 还得再点一次 B，多此一举。
/// 所以把里层的子菜单提到 A 的位置、A 直接消失（里层仍是子菜单，不摊它的子项）。
///
/// 例：一级子菜单「文字」里有个二级子菜单「翻译」，两个都配了「文字」；
/// 剪完「文字」里只剩「翻译」，这里就把「翻译」提到根层，菜单只显示「翻译」一项。
fn collapse_pure_shells(items: Vec<ItemSetting>) -> Vec<ItemSetting> {
    let mut out = Vec::new();
    for item in items {
        if !item.is_submenu() {
            out.push(item);
            continue;
        }
        let mut node = item;
        node.children = collapse_pure_shells(std::mem::take(&mut node.children));
        // 去掉分隔线后，恰只剩一个子菜单（没有任何普通项）→ 纯外壳
        let non_sep: Vec<&ItemSetting> = node
            .children
            .iter()
            .filter(|child| child.kind != ItemKind::Separator)
            .collect();
        if non_sep.len() == 1 && non_sep[0].is_submenu() {
            out.push(non_sep[0].clone());
        } else {
            out.push(node);
        }
    }
    out
}

/// 把 `target` 这个子菜单的子项整块提到当前层。
/// 把 `target` 这一支摊到根层。
///
/// 做法：把 target 的**直接子项**在原地提上来（target 自己消失），
/// 再把它沿途那些「只为装它才存在」的祖先壳一起化掉。
/// 所以不管 target 埋在第 2 层还是第 5 层，摊完根层就只剩它的子项 ——
/// 不会出现「摊是摊了，外面还套着一级父项」的样子（那是上一版只掉一层壳的毛病）。
fn flatten_submenu(items: Vec<ItemSetting>, target: &str) -> Vec<ItemSetting> {
    let mut promoted: Vec<String> = Vec::new();
    let tree = promote_children(items, target, &mut promoted);
    let set: std::collections::HashSet<String> = promoted.into_iter().collect();
    dissolve_shells(tree, &set)
}

/// 把 target 换成它自己的子项（递归找到为止），顺手记下被提上来的 id。
fn promote_children(
    items: Vec<ItemSetting>,
    target: &str,
    promoted: &mut Vec<String>,
) -> Vec<ItemSetting> {
    let mut out = Vec::new();
    for item in items {
        if item.id == target {
            for child in &item.children {
                if child.kind != ItemKind::Separator {
                    promoted.push(child.id.clone());
                }
            }
            out.extend(item.children);
            continue;
        }
        let mut node = item;
        if node.is_submenu() && contains_id(&node.children, target) {
            node.children = promote_children(std::mem::take(&mut node.children), target, promoted);
        }
        out.push(node);
    }
    out
}

/// 化掉「纯外壳」子菜单：子项全是刚从 target 提上来的那几个 → 这层壳没别的内容，
/// 直接把子项再往上提（递归，所以「后缀 → 影音 → 动作」两层壳会一起掉干净）。
fn dissolve_shells(
    items: Vec<ItemSetting>,
    promoted: &std::collections::HashSet<String>,
) -> Vec<ItemSetting> {
    let mut out = Vec::new();
    for item in items {
        if !item.is_submenu() {
            out.push(item);
            continue;
        }
        let mut node = item;
        node.children = dissolve_shells(std::mem::take(&mut node.children), promoted);
        let mut kept = 0usize;
        let mut only_promoted = true;
        for child in &node.children {
            if child.kind == ItemKind::Separator {
                continue;
            }
            kept += 1;
            if !promoted.contains(&child.id) {
                only_promoted = false;
            }
        }
        if kept > 0 && only_promoted {
            out.extend(node.children);
            continue;
        }
        out.push(node);
    }
    out
}

/// 该摊哪一支：从 `targets` 里选最深的那个，前提是其余目标都在它的祖先链上
/// （用户先在父级配了筛选、后来又给子级配 —— 同一支，按最深的摊）。
/// 互不为祖先的两支各配各的 → 返回 None，谁都不摊（摊了会糊成一片）。
fn pick_flatten_target(targets: &[String], items: &[ItemSetting]) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for target in targets {
        let mut path = Vec::new();
        if !path_to(items, target, &mut path) {
            continue;
        }
        let depth = path.len();
        if best.as_ref().map(|(seen, _)| depth > *seen).unwrap_or(true) {
            best = Some((depth, target.clone()));
        }
    }
    let (_, chosen) = best?;
    let mut chain = Vec::new();
    if !path_to(items, &chosen, &mut chain) {
        return None;
    }
    if targets.iter().all(|target| chain.contains(target)) {
        Some(chosen)
    } else {
        None
    }
}

/// 从根到 `target` 的 id 链（含 target 自己）；找不到返回 false。
fn path_to(items: &[ItemSetting], target: &str, path: &mut Vec<String>) -> bool {
    for item in items {
        path.push(item.id.clone());
        if item.id == target {
            return true;
        }
        if item.is_submenu() && path_to(&item.children, target, path) {
            return true;
        }
        path.pop();
    }
    false
}

/// 把浮上来的项按各自指定的位置插回根层。
///
/// `Top` 的先整批塞到最前面（保持在树里的先后顺序），其余的按 `float_placement`
/// 一条条插 —— 「按原顺序」用前序遍历序号当尺子，所以它最终会落回原来那一带。
fn merge_floating(
    roots: Vec<ItemSetting>,
    floating: Vec<ItemSetting>,
    order: &std::collections::HashMap<String, usize>,
) -> Vec<ItemSetting> {
    if floating.is_empty() {
        return roots;
    }
    let (top, rest): (Vec<ItemSetting>, Vec<ItemSetting>) = floating
        .into_iter()
        .partition(|item| item.float_placement == FloatPlacement::Top);
    let mut out = top;
    out.extend(roots);
    for item in rest {
        let at = match item.float_placement {
            FloatPlacement::Top => 0,
            FloatPlacement::BeforeSubmenus => out
                .iter()
                .position(|node| node.is_submenu())
                .unwrap_or(0),
            FloatPlacement::AfterSubmenus => out
                .iter()
                .rposition(|node| node.is_submenu())
                .map(|index| index + 1)
                .unwrap_or(out.len()),
            FloatPlacement::Order => {
                let mine = order.get(&item.id).copied().unwrap_or(usize::MAX);
                out.iter()
                    .position(|node| order.get(&node.id).copied().unwrap_or(usize::MAX) > mine)
                    .unwrap_or(out.len())
            }
        };
        out.insert(at.min(out.len()), item);
    }
    out
}

/// 把某个子菜单整支做成一棵「只有它」的菜单（快捷键呼出用）。
/// 一个条目的 (文案, 动作)：快捷键直接执行时用。
pub fn item_actions(id: &str) -> Option<(String, Vec<ActionStep>)> {
    read(|settings| {
        settings.find_path(id).and_then(|path| {
            settings
                .node(&path)
                .map(|item| (item.label.clone(), item.actions.clone()))
        })
    })
}

/// 热字符串的全局设置。
pub fn hotstring_settings() -> HotstringSettings {
    read(|settings| settings.hotstrings.clone())
}

/// 现在配了哪些热字符串。
pub fn hotstring_phrases() -> Vec<HotstringPhrase> {
    read(|settings| settings.hotstring_phrases())
}

/// 某一项是不是子菜单（热字符串命中子菜单时只弹那一支）。
pub fn item_is_submenu(id: &str) -> bool {
    read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .map(|item| item.is_submenu())
            .unwrap_or(false)
    })
}

/// 字号缩放（设置里的「字体缩放」，1.0 = 原样）。
pub fn font_scale() -> f32 {
    read(|settings| settings.font_scale)
}

/// 按「这次选中了什么」筛过的菜单树。
///
/// `context = None`（没选中任何东西 / 关掉了筛选）时就是全量菜单。
pub fn menu_items_for(context: Option<Context>) -> Vec<MenuEntry> {
    read(|settings| {
        let context = if settings.context_filter {
            context
        } else {
            None
        };
        settings.menu_for(context.as_ref())
    })
}

/// 托盘图标右键弹的菜单。
pub fn tray_menu() -> Vec<MenuEntry> {
    let mut entries = read(|settings| settings.tray_menu());
    // 托盘菜单也不是「完整菜单」，给它留一个出口
    append_show_all(&mut entries);
    entries
}

/// 某一个子菜单里的那几项（快捷键呼出用），顺便补一条「显示全部菜单」。
pub fn submenu_entries(id: &str) -> Vec<MenuEntry> {
    let mut entries = read(|settings| settings.submenu_menu(id));
    if entries.is_empty() {
        return entries;
    }
    // 快捷键直接呼出的子菜单里要不要也带「最近运行」（设置里可关）
    if read(|settings| settings.recent.in_submenu) {
        append_recent(&mut entries);
    }
    // 只弹一支当然不是「完整菜单」：留个出口回全量
    append_show_all(&mut entries);
    entries
}

/// 往一份「不是完整菜单」的菜单里补一条 `≡ 显示全部菜单`（位置和开关都在设置里）。
///
/// 调用方保证：只给**不完整**的菜单补（筛过的 / 托盘 / 只弹一支）。
pub fn append_show_all(entries: &mut Vec<MenuEntry>) {
    let (enabled, placement, separators) = read(|settings| {
        (
            settings.show_all_entry,
            settings.show_all_placement,
            settings.show_separators,
        )
    });
    if !enabled {
        return;
    }
    // 已经有「打开完整菜单」这类入口就别再加一条 —— 托盘菜单自带一个，
    // 两条并排看着像重复。
    // 中英两种写法都认：菜单项文案是生成时按当时的语言写死的，之后切了语言也不会改，
    // 只比当前语言的写法会在切语言后多出一条重复入口。
    let already = entries.iter().any(|entry| match entry {
        MenuEntry::Item(item) => {
            item.id == "__show_all__"
                || item.label.contains("完整菜单")
                || item.label.to_ascii_lowercase().contains("full menu")
        }
        MenuEntry::Separator | MenuEntry::ColumnBreak => false,
    });
    if already {
        return;
    }
    let mut all = MenuItem::new("__show_all__", crate::i18n::t("显示全部菜单"));
    all.badge = Some("≡".into());
    match placement {
        EdgePlacement::Top => {
            entries.insert(0, MenuEntry::Item(all));
            if separators && entries.len() > 1 {
                entries.insert(1, MenuEntry::Separator);
            }
        }
        EdgePlacement::Bottom => {
            if separators && !entries.is_empty() {
                entries.push(MenuEntry::Separator);
            }
            entries.push(MenuEntry::Item(all));
        }
    }
}

// ---------------------------------------------------------------------------
// 菜单项配置文件（menus/*.runa）

impl MenuSettings {
    /// 当前配置名（空 = 默认那份）。
    pub fn effective_profile(&self) -> String {
        let name = self.menu_profile.trim();
        if name.is_empty() {
            crate::menu_files::DEFAULT_NAME.to_string()
        } else {
            name.to_string()
        }
    }

    /// 界面上该显示哪一份：切了还没确认时显示正在预览的那份。
    pub fn shown_profile(&self) -> String {
        match &self.pending_profile {
            Some(name) => name.clone(),
            None => self.effective_profile(),
        }
    }
}

/// 换一套菜单项配置：读那套的 `.runa`，替换内存里的菜单树。
///
/// 配色 / 快捷键 / 热字符串这些**不动** —— 它们属于这台机器，不属于某套菜单。
pub fn switch_profile(cx: &mut App, name: &str) -> Result<(), String> {
    let Some(name) = crate::menu_files::sanitize(name) else {
        return Err(crate::i18n::t(crate::i18n::t("名字不能为空")).to_string());
    };
    let Some(file) = crate::menu_files::load(&name) else {
        return Err(if crate::i18n::is_english() {
            format!("cannot read {name}.{}", crate::menu_files::EXT)
        } else {
            format!("读不到 {name}.{}", crate::menu_files::EXT)
        });
    };
    // 换配置本身不是「改菜单项」，别记成一步可撤销的改动
    without_undo(|| {
        update(cx, |settings| {
            // 托盘那一支不跟着走：换配置不该改掉右键图标里的菜单
            settings.adopt_profile_items(file.items, file.next_id);
            // 只是预览：`menu_profile` 一个字都不动，落盘等「应用 / 确定」
            let committed = settings.effective_profile();
            settings.pending_profile = if committed == name {
                None
            } else {
                Some(name.clone())
            };
        });
    });
    Ok(())
}

/// 把「正在预览的那份配置」正式认下来（按「应用 / 确定」时调）。
///
/// 返回是否真的提交了东西。
pub fn commit_pending_profile(cx: &mut App) -> bool {
    let Some(name) = read(|settings| settings.pending_profile.clone()) else {
        return false;
    };
    update(cx, |settings| {
        settings.menu_profile = name;
        settings.pending_profile = None;
    });
    true
}

/// 放弃这次切换：把菜单树换回原来那份（按「取消 / 撤销」时调）。
pub fn cancel_pending_profile(cx: &mut App) -> bool {
    let Some(_preview) = read(|settings| settings.pending_profile.clone()) else {
        return false;
    };
    let committed = read(|settings| settings.effective_profile());
    let file = crate::menu_files::load(&committed);
    without_undo(|| {
        update(cx, |settings| {
            match file {
                // 托盘不动，只换菜单那一部分
                Some(file) => settings.adopt_profile_items(file.items, file.next_id),
                None => settings.reset_items(),
            }
            settings.pending_profile = None;
        });
    });
    true
}

/// 把当前这份复制成新的一份，然后切过去（原配置一个字都不动）。
pub fn duplicate_profile(cx: &mut App, new_name: &str) -> Result<String, String> {
    let Some(base) = crate::menu_files::sanitize(new_name) else {
        return Err(crate::i18n::t("名字不能为空").to_string());
    };
    let name = crate::menu_files::unique_name(&base);
    let (items, next_id) = read(|settings| (settings.items.clone(), settings.next_id));
    crate::menu_files::save(&name, &items, next_id).map_err(|err| err.to_string())?;
    update(cx, |settings| {
        settings.menu_profile = name.clone();
    });
    Ok(name)
}

/// 新建一份**空的**配置（一个菜单项都没有），然后切过去。
///
/// 和「建副本」的区别：副本是照着现在这份抄一份，这里是张白纸 ——
/// 想从头攒一套菜单的人不用先删掉一堆用不上的项。
pub fn create_profile(cx: &mut App, new_name: &str) -> Result<String, String> {
    let Some(base) = crate::menu_files::sanitize(new_name) else {
        return Err(crate::i18n::t("名字不能为空").to_string());
    };
    let name = crate::menu_files::unique_name(&base);
    crate::menu_files::save(&name, &[], 1).map_err(|err| err.to_string())?;
    without_undo(|| {
        update(cx, |settings| {
            // 托盘那一支留着：它属于这台机器，不属于某套菜单
            settings.items.retain(|item| item.id == TRAY_ID);
            settings.next_id = 1;
            settings.bump_next_id();
            settings.reassign_tray_ids();
            settings.normalize_tree();
            settings.menu_profile = name.clone();
            // 新配置就是「已经切过去」的那份，不留在预览状态
            settings.pending_profile = None;
        });
    });
    Ok(name)
}

/// 给当前这份改名（文件名 + 设置里的名字一起改）。
pub fn rename_profile(cx: &mut App, new_name: &str) -> Result<(), String> {
    let Some(base) = crate::menu_files::sanitize(new_name) else {
        return Err(crate::i18n::t("名字不能为空").to_string());
    };
    let old = read(|settings| settings.effective_profile());
    if base == old {
        return Ok(());
    }
    let name = crate::menu_files::unique_name(&base);
    crate::menu_files::rename(&old, &name).map_err(|err| err.to_string())?;
    update(cx, |settings| settings.menu_profile = name);
    Ok(())
}

/// 删掉「已提交的那份」之后，换哪一份顶上：优先「默认」，其次目录里第一份。
///
/// 抽出来是因为这块最容易写错：目录里一份都不剩时还得给个名字（调用方会
/// 因为读不到文件走「恢复出厂菜单」），不能返回空串把 `menu_profile` 弄成脏值。
fn fallback_after_delete(remaining: &[String]) -> String {
    remaining
        .iter()
        .find(|name| name.as_str() == crate::menu_files::DEFAULT_NAME)
        .or_else(|| remaining.first())
        .cloned()
        .unwrap_or_else(|| crate::menu_files::DEFAULT_NAME.to_string())
}

/// 删一份配置。
///
/// `name` 是**界面上显示的那份**（可能是切换后还没应用、正在预览的那份）。
/// 这里有个坑：删掉的如果只是「正在预览」的那份，`menu_profile` 里记的还是
/// 原来那份，所以不能拿「当前提交的那份」去判断要不要收尾 ——
/// 否则删掉 A 而界面显示 B 时，B 的文件被删了、菜单树却还停在 A，
/// 看起来就是「删的是另一个配置」。
///
/// 删完的收尾分三种：
/// - 删的是预览中的那份：丢预览，把菜单树换回已提交的那份；
/// - 删的是已提交的那份：换一份还在的（优先「默认」），顺手清掉指向它的预览；
/// - 删的是别的：什么都不动。
pub fn delete_profile(cx: &mut App, name: &str) -> Result<(), String> {
    let (current, pending) = read(|settings| (settings.effective_profile(), settings.pending_profile.clone()));
    // 最后一份不给删：删完这个目录就空了，用户连「切回去」都没得切
    if crate::menu_files::list().len() <= 1 {
        return Err(crate::i18n::either(
            "这是最后一份菜单配置，先建个副本再删",
            "This is the last menu config - make a copy before deleting it",
        )
        .to_string());
    }
    crate::menu_files::delete(name).map_err(|err| err.to_string())?;

    let deleted_current = name == current;
    let deleted_pending = pending.as_deref() == Some(name);
    if !deleted_current && !deleted_pending {
        return Ok(());
    }

    // 已提交的那份被删了：换一份还在的顶上
    let fallback = if deleted_current {
        Some(fallback_after_delete(&crate::menu_files::list()))
    } else {
        None
    };
    let reload = if deleted_pending {
        // 预览的那份没了：菜单树换回「提交的那份」（它自己也刚被删就换成 fallback）
        Some(
            fallback
                .clone()
                .unwrap_or_else(|| read(|settings| settings.effective_profile())),
        )
    } else {
        None
    };
    without_undo(|| {
        update(cx, |settings| {
            if let Some(fallback) = fallback.clone() {
                settings.menu_profile = fallback;
            }
            if deleted_pending {
                settings.pending_profile = None;
            }
            // 预览正好落在「刚顶上来的那份」上：那已经不算预览了，
            // 留着会让「应用」一直亮着、撤销还会去读同一份
            if settings.pending_profile.as_deref() == Some(settings.menu_profile.as_str()) {
                settings.pending_profile = None;
            }
        });
    });
    if let Some(target) = reload {
        match crate::menu_files::load(&target) {
            // 托盘那一支不跟着走：换配置不该改掉右键图标里的菜单
            Some(file) => without_undo(|| {
                update(cx, |settings| {
                    settings.adopt_profile_items(file.items, file.next_id);
                })
            }),
            None => without_undo(|| update(cx, |settings| settings.reset_items())),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 最近运行

/// 归一化到根层之外还要保留的那条：先把「截断」定死，别让它随显示条数变。
const RECENT_KEEP: usize = 40;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

impl MenuSettings {
    /// 按 id 取菜单项（可能在子菜单深处）。
    pub fn item_by_id(&self, id: &str) -> Option<&ItemSetting> {
        let path = self.find_path(id)?;
        self.node(&path)
    }

    /// 这一项是不是在托盘那一支里面（托盘右键菜单里点出来的就是这些）。
    pub fn in_tray_branch(&self, id: &str) -> bool {
        let Some(path) = self.find_path(id) else {
            return false;
        };
        let Some(&root) = path.first() else {
            return false;
        };
        self.items
            .get(root)
            .map(|item| item.id == TRAY_ID)
            .unwrap_or(false)
    }
}

/// 记录一次「运行了某个菜单项」。菜单项一执行就调，最近列表跟着刷新。
///
/// 只记真正带动作的项（子菜单 / 分隔线不算「运行过」）；
/// `__` 开头的内部项（显示全部 / 右键菜单项）也不记。
pub fn record_recent(cx: &mut App, id: &str) {
    if id.is_empty() || id.starts_with("__") {
        return;
    }
    let (enabled, valid) = read(|settings| {
        let valid = settings
            .item_by_id(id)
            .map(|item| item.kind == ItemKind::Item && !item.actions.is_empty())
            .unwrap_or(false);
        // 托盘菜单（托盘图标右键弹出的那份）里点的项：默认不记，
        // 免得临时翻一下托盘就把最近列表顶掉。设置里可以打开。
        let record = settings.recent.record_tray || !settings.in_tray_branch(id);
        (settings.recent.enabled && record, valid)
    });
    if !enabled || !valid {
        return;
    }
    let at = now_secs();
    update(cx, |settings| {
        settings.recent_items.retain(|track| track.id != id);
        settings
            .recent_items
            .insert(0, RecentTrack { id: id.to_string(), at });
        settings.recent_items.truncate(RECENT_KEEP);
    });
}

/// 「自动生成」的一轮结果：每个子菜单该补哪些项（纯数据，能扔后台线程算）。
pub struct AutofillPlan {
    /// 子菜单 id → [(菜单文案, 动作里写的值)]
    pub entries: Vec<(String, Vec<(String, String)>)>,
    /// 这一轮有没有真查到东西 —— 说明「Everything 没连上」用
    pub used_backend: bool,
    /// 有几个子菜单开了开关（用来区分「没配」和「配了但没东西」）
    pub jobs: usize,
}

impl AutofillPlan {
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            used_backend: false,
            jobs: 0,
        }
    }
}

/// 摘出「哪些子菜单开了自动生成、各自要什么」。
///
/// 只有它能碰 `read()`（线程本地），所以放主线程；查索引的部分在
/// `autofill_plan` 里，那一段可以扔到后台线程去跑。
pub fn autofill_jobs() -> Vec<(String, AutoFillSpec)> {
    autofill_jobs_where(&|_| true)
}

/// 只挑出符合条件的那些子菜单（按模式筛：启动时 / 实时 / 手动）。
pub fn autofill_jobs_where(keep: &dyn Fn(&AutoFillSpec) -> bool) -> Vec<(String, AutoFillSpec)> {
    read(|settings| {
        let mut jobs = Vec::new();
        fn walk(
            items: &[ItemSetting],
            keep: &dyn Fn(&AutoFillSpec) -> bool,
            jobs: &mut Vec<(String, AutoFillSpec)>,
        ) {
            for item in items {
                if item.is_submenu() && item.autofill.enabled && keep(&item.autofill) {
                    jobs.push((item.id.clone(), item.autofill.clone()));
                }
                walk(&item.children, keep, jobs);
            }
        }
        walk(&settings.items, keep, &mut jobs);
        jobs
    })
}

/// 这一个子菜单开着「实时生成」吗（界面要据此禁掉手动加项）。
pub fn autofill_is_live(id: &str) -> bool {
    read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .map(|node| node.autofill.enabled && node.autofill.mode == AutoFillMode::Live)
            .unwrap_or(false)
    })
}

/// 子菜单的「自动生成」：按文件夹 + 后缀把文件读成这一支的菜单项。
///
/// 走 **Everything 的索引**（不扫盘，几万个文件也是毫秒级）。只**补缺**不覆盖：
/// 同一支里已经有同名项就跳过（用户改过的动作不会被下一次生成冲掉），
/// 文件删了也不会反过来把项删掉。
///
/// 这一半只查不算写：不碰 App、不碰线程本地状态，所以能放到后台线程，
/// 界面不会因为查索引卡一下。
pub fn autofill_plan(jobs: &[(String, AutoFillSpec)]) -> AutofillPlan {
    let mut plan = AutofillPlan {
        entries: Vec::new(),
        used_backend: false,
        jobs: jobs.len(),
    };
    if jobs.is_empty() {
        return plan;
    }
    let mut used_backend = false;
    let mut entries: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for (id, spec) in jobs {
        // 文件夹可以写占位符（`{exe_dir}\影片`、`{env:USERPROFILE}\Desktop` 这种）；
        // 展开之后再查，否则 `path:"{exe_dir}"` 这种字符串什么也匹配不到
        let dirs: Vec<String> = spec
            .dir_list()
            .into_iter()
            .map(|dir| crate::placeholders::expand(&dir))
            .collect();
        let exts = spec.ext_list();
        if dirs.is_empty() {
            continue;
        }
        let mut files: Vec<(String, String)> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        // 被「最多 N 项」截断的文件夹（有就末尾补一条「打开这个文件夹」的出口）
        let mut truncated_dir: Option<String> = None;
        // 上限留空（0）= 用默认的 500，**不是 1**。以前 0 被 clamp 成 1，
        // 结果「文件夹里 5 个 lnk 只生成 1 个」—— 就是这个 off-by-default 造成的。
        let per_dir = if spec.limit == 0 {
            500
        } else {
            spec.limit.clamp(1, 5000) as usize
        };
        for dir in &dirs {
            // 多要一条：正好多出来就说明被截断了
            let mut hits = crate::everything::files_in(dir, &exts, per_dir + 1, spec.recurse);
            if hits.len() > per_dir {
                hits.truncate(per_dir);
                truncated_dir = Some(dir.clone());
            }
            if !hits.is_empty() {
                used_backend = true;
            }
            for hit in hits {
                let name = base_name(&hit.path);
                if name.is_empty() {
                    continue;
                }
                let stem = stem_of(name);
                let key = stem.to_ascii_lowercase();
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                // 一律套双引号：`我的 视频.mp4` 这种带空格的，不套会被拆成两个参数
                let value = if spec.full_path {
                    MenuSettings::quote_path(&hit.path)
                } else {
                    MenuSettings::quote_path(name)
                };
                files.push((stem.to_string(), value));
            }
        }
        if files.is_empty() {
            continue;
        }
        // 按名字排序：生成出来的菜单是「音乐 / 影片」这种字母序，翻起来有规律
        files.sort_by_key(|(label, _)| label.to_ascii_lowercase());
        // 文件太多：别把几千条塞进菜单，末尾给一个「打开这个文件夹」的出口
        if let Some(dir) = truncated_dir {
            files.push((
                crate::i18n::t("还有更多没列出…（打开这个文件夹）").to_string(),
                format!("explorer \"{dir}\""),
            ));
        }
        entries.push((id.clone(), files));
    }
    plan.entries = entries;
    plan.used_backend = used_backend;
    plan
}

/// 把算好的项写回配置：缺哪条补哪条（插在这一支最前面，名字有序）。返回新加了几条。
pub fn apply_autofill_plan(cx: &mut App, plan: &AutofillPlan, clear: bool) -> usize {
    if plan.entries.is_empty() {
        return 0;
    }
    let mut added = 0usize;
    update(cx, |settings| {
        for (sub_id, files) in plan.entries.iter() {
            let Some(path) = settings.find_path(sub_id) else {
                continue;
            };
            // 已有项的「文案」和「去掉后缀的文案」都算占位：
            // 用户手写的「影片」和生成的「影片.mp4」不该重复出现
            let existing: Vec<(String, String)> = settings
                .node(&path)
                .map(|node| {
                    node.children
                        .iter()
                        .map(|child| {
                            let label = child.label.trim().to_ascii_lowercase();
                            let stem = stem_of(&label).to_ascii_lowercase();
                            (label, stem)
                        })
                        .collect::<Vec<(String, String)>>()
                })
                .unwrap_or_default();
            let known: Vec<String> = existing
                .iter()
                .flat_map(|(label, stem)| [label.clone(), stem.clone()])
                .collect();
            let mut fresh: Vec<ItemSetting> = Vec::new();
            let mut fresh_names: Vec<String> = Vec::new();
            for (label, value) in files.iter() {
                let key = label.to_ascii_lowercase();
                if known.contains(&key) || fresh_names.contains(&key) {
                    continue;
                }
                fresh_names.push(key);
                let id = settings.next_item_id();
                let mut node = ItemSetting::item(id, label.clone());
                node.actions = vec![ActionStep::new(ActionKind::Run, value.clone())];
                fresh.push(node);
            }
            // 「实时 / 仅启动时」是重建：先清空这一支现有的项。
            // 这两个模式下界面本来就不允许手动加项，所以清掉的一定是上一次生成的。
            if clear {
                if let Some(node) = settings.node_mut(&path) {
                    node.children.clear();
                }
            }
            if fresh.is_empty() {
                continue;
            }
            added += fresh.len();
            if let Some(node) = settings.node_mut(&path) {
                // 新项排在这一支最前面：后面跟着的是用户自己加的，不会被冲散
                fresh.extend(std::mem::take(&mut node.children));
                node.children = fresh;
            }
        }
    });
    added
}

/// 跑这一项之前，需不需要先「抓一次选中内容」。
///
/// 抓一次要发 Ctrl+C 再等剪贴板（最长半秒），而「发送按键 / 发送文本 / 打开网址」
/// 这些根本用不到选中内容 —— 连按几次快捷键，几次抓取就叠起来，看起来像卡死。
/// 只有这两种情况才抓：这一项要「接上选中内容」，或动作里写了 `{selection}`。
pub fn item_needs_selection(id: &str) -> bool {
    read(|settings| {
        let Some(item) = settings.item_by_id(id) else {
            // 找不到项就别省这一步（宁可慢一点，也不要少传东西）
            return true;
        };
        needs_selection(item, settings.append_selection)
    })
}

/// 纯逻辑版（方便单测）：这一项跑之前要不要先抓一次选中内容。
pub fn needs_selection(item: &ItemSetting, global_append: bool) -> bool {
    // 1) 动作里明确写了 `{selection}` / `%getZz%`：必须抓
    if item
        .actions
        .iter()
        .any(|step| crate::placeholders::mentions_selection(&step.value))
    {
        return true;
    }
    // 2) 只有「运行程序」会把选中内容的路径追加到命令行后面，
    //    所以「接上选中内容」这个开关只在有运行程序时才有意义。
    //
    //    以前这里写成「开关开着就抓」——而全局默认就是开着的，于是**每一项**
    //    按下都白抓一次（发 Ctrl+C + 等剪贴板，最长半秒）。长按快捷键时几次
    //    抓取叠起来，用户看到的就是「按下去要半天才重复一次」，而且每重复一次
    //    都往目标窗口里发了一次 Ctrl+C。
    let has_run = item.actions.iter().any(|step| step.kind == ActionKind::Run);
    has_run && item.append_selection.unwrap_or(global_append)
}

/// 某个子菜单的自动生成配置（没开就是 `None`）。
pub fn autofill_spec_of(id: &str) -> Option<AutoFillSpec> {
    read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .filter(|node| node.autofill.enabled)
            .map(|node| node.autofill.clone())
    })
}

/// 把一份算好的「生成结果」装进运行时缓存（**不写配置**）。返回装了几项。
///
/// 给启动那一轮用：它在后台线程里算，算完回到界面线程调用这个。
pub fn install_runtime_plan(plan: &AutofillPlan) -> usize {
    install_runtime(plan)
}

fn install_runtime(plan: &AutofillPlan) -> usize {
    let mut total = 0usize;
    for (sub_id, files) in plan.entries.iter() {
        let mut items = Vec::with_capacity(files.len());
        for (label, value) in files {
            // id 在进程内唯一即可（不落盘，不用怕和配置里的撞）
            let id = format!("af{}", NEXT_RUNTIME_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            let mut node = ItemSetting::item(id, label.clone());
            node.actions = vec![ActionStep::new(ActionKind::Run, value.clone())];
            items.push(node);
        }
        total += items.len();
        crate::autofill_runtime::set(sub_id, items);
    }
    total
}

static NEXT_RUNTIME_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// 按模式重新生成一批（实时 / 仅启动时）：结果只放运行时缓存。
///
/// `only` 限定某一支（`Submenu` 来源那一支）；`force` 跳过 2 秒的节流
/// （手动「立即生成」和启动那一轮用）。
pub fn refresh_runtime(mode: AutoFillMode, only: Option<&str>, force: bool) -> usize {
    let jobs: Vec<(String, AutoFillSpec)> = autofill_jobs_where(&|spec| spec.mode == mode)
        .into_iter()
        .filter(|(id, _)| match only { Some(want) => want == id, None => true })
        .filter(|(id, _)| force || !crate::autofill_runtime::fresh(id))
        .collect();
    if jobs.is_empty() {
        return 0;
    }
    let plan = autofill_plan(&jobs);
    let total = install_runtime(&plan);
    // 这一轮没查到东西的支（文件夹空了 / 被删了）要把旧的清掉，否则菜单里还挂着上次的
    for (id, _) in &jobs {
        if !plan.entries.iter().any(|(sub, _)| sub == id) {
            crate::autofill_runtime::clear(id);
        }
    }
    total
}

/// 启动那一轮：算完（可能在后台线程）再装进来。
pub fn refresh_runtime_on_start() -> usize {
    let jobs = autofill_jobs_where(&|spec| spec.mode == AutoFillMode::OnStart);
    if jobs.is_empty() {
        return 0;
    }
    let plan = autofill_plan(&jobs);
    let total = install_runtime(&plan);
    for (id, _) in &jobs {
        if !plan.entries.iter().any(|(sub, _)| sub == id) {
            crate::autofill_runtime::clear(id);
        }
    }
    total
}

/// 生成完给界面的那句话（没东西可加时要说清楚是「没连上」还是「都齐了」）。
pub fn autofill_note(added: usize, plan: &AutofillPlan) -> String {
    if added > 0 {
        return if crate::i18n::is_english() {
            format!("Added {added} item(s) from Everything's index")
        } else {
            format!("从 Everything 索引补进来 {added} 项")
        };
    }
    if plan.jobs == 0 {
        return String::new();
    }
    if !plan.used_backend {
        return crate::i18n::t(
            "没查到文件：先确认 Everything 正在运行、后端已连上（设置 → 行为 里能看到「未连接」）",
        )
        .to_string();
    }
    crate::i18n::t("没有新东西可加：同名项已经在了，或者文件都已被删掉").to_string()
}

/// 一键跑完：查 + 写 + 说明。编辑窗口的「立即生成」按钮走这个。
pub fn apply_autofill(cx: &mut App) -> (usize, String) {
    let jobs = autofill_jobs();
    let plan = autofill_plan(&jobs);
    let added = apply_autofill_plan(cx, &plan, false);
    let note = autofill_note(added, &plan);
    (added, note)
}

/// 路径里最后那一段（文件名）。
fn base_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// 去掉最后一段扩展名（`测试.视频.mp4` → `测试.视频`）。
fn stem_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(at) if at > 0 => &name[..at],
        _ => name,
    }
}

/// 把「最近运行」的几项插到菜单顶部 / 底部。
///
/// 以前这里会跳过「根层本来就看得见」的项，想法是「别重复」；结果是**完整菜单里
/// 几乎永远看不到最近运行** —— 常用的项往往就配在根层，全被跳过了，
/// 只有「按选中内容筛过」的菜单（根层项被筛掉了）才会露出来。
/// 现在改成无条件显示：最近运行的定位就是**快捷入口**，和原位那一条并存是正常的
/// （和「开始菜单的最近使用」一个道理），顺带还省了「翻进子菜单找」这一步。
pub fn append_recent(entries: &mut Vec<MenuEntry>) {
    let (list, placement, separators) = read(|settings| {
        let mut list: Vec<MenuEntry> = Vec::new();
        if settings.recent.enabled {
            let limit = settings.recent.limit.clamp(1, 20) as usize;
            let mut seen: Vec<String> = Vec::new();
            for track in settings.recent_items.iter() {
                if list.len() >= limit {
                    break;
                }
                let Some(item) = settings.item_by_id(&track.id) else {
                    continue; // 项被删了：自然从最近列表里消失
                };
                if !item.enabled || item.kind != ItemKind::Item || item.actions.is_empty() {
                    continue;
                }
                // 同一个 id 只放一条（记录本身已经去重，这里防的是改过 id 的老配置）
                if seen.iter().any(|id| id == &track.id) {
                    continue;
                }
                seen.push(track.id.clone());
                list.push(item.to_entry(
                    settings.show_badges,
                    settings.show_shortcuts,
                    settings.auto_icons,
                ));
            }
        }
        (list, settings.recent.placement, settings.show_separators)
    });
    if list.is_empty() {
        return;
    }
    let count = list.len();
    match placement {
        EdgePlacement::Top => {
            for entry in list.into_iter().rev() {
                entries.insert(0, entry);
            }
            if separators && entries.len() > count {
                entries.insert(count, MenuEntry::Separator);
            }
        }
        EdgePlacement::Bottom => {
            if separators && !entries.is_empty() {
                entries.push(MenuEntry::Separator);
            }
            entries.extend(list);
        }
    }
}

// ---------------------------------------------------------------------------
// 菜单项右键快捷操作
// ---------------------------------------------------------------------------

/// 剪贴板里那份「菜单项」的格式：`{"runany_menu_items":1,"items":[...]}`。
///
/// 只写一个对象也行（粘进 `.runa` 的 `items` 数组就是那个形状），
/// 读的时候两种都认 —— 这样「动作库里复制一条」既能粘回列表，也能粘进配置文件。
pub fn clipboard_payload(items: &[ItemSetting]) -> Option<String> {
    #[derive(Serialize)]
    struct Payload<'a> {
        runany_menu_items: u32,
        items: &'a [ItemSetting],
    }
    serde_json::to_string(&Payload {
        runany_menu_items: 1,
        items,
    })
    .ok()
}

/// 从剪贴板文本里解析出菜单项（认整个 payload，也认单个对象）。
pub fn items_from_clipboard(text: &str) -> Option<Vec<ItemSetting>> {
    #[derive(Deserialize)]
    struct Payload {
        #[serde(default = "clipboard_marker")]
        #[allow(dead_code)]
        runany_menu_items: u32,
        items: Vec<ItemSetting>,
    }
    fn clipboard_marker() -> u32 {
        1
    }
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(payload) = serde_json::from_str::<Payload>(text) {
        if !payload.items.is_empty() {
            return Some(payload.items);
        }
    }
    serde_json::from_str::<ItemSetting>(text)
        .ok()
        .map(|item| vec![item])
}

/// 右键点菜单项时弹的那几条（id 带 `__ctx:` 前缀，点下去由 `popup::finish` 分发）。
pub fn item_context_menu(id: &str) -> Vec<MenuEntry> {
    let Some(item) = read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path).cloned())
    }) else {
        return Vec::new();
    };
    let mut entries = vec![
        ctx_entry(
            format!("__ctx:edit:{id}"),
            crate::i18n::t("编辑这一项…").to_string(),
            "✎",
        ),
        ctx_entry(
            format!("__ctx:copy:{id}"),
            crate::i18n::t("复制名称").to_string(),
            "⧉",
        ),
        ctx_entry(
            format!("__ctx:copyitem:{id}"),
            crate::i18n::t("复制这一项（Ctrl+V 可粘贴）").to_string(),
            "▣",
        ),
        ctx_entry(
            format!("__ctx:duplicate:{id}"),
            crate::i18n::t("复制一份").to_string(),
            "＋",
        ),
        ctx_entry(
            format!("__ctx:paste:{id}"),
            crate::i18n::t("粘贴到这一项下面").to_string(),
            "↓",
        ),
    ];
    // 「把选中内容加到这一项后面」：只在这次弹出**真抓到了**东西时才有意义。
    //
    // 选中文件时后面还要问一句「完整路径 / 只填文件名」（那就是菜单里的第二问），
    // 所以这里挂一个省略号，让人知道点了还有下文。
    if let Some(selection) = crate::selection::current() {
        let label = if selection.files.is_empty() {
            crate::i18n::t("把选中内容加到这一项后面…").to_string()
        } else if crate::i18n::is_english() {
            format!("Add the {} selected file(s) after this one…", selection.files.len())
        } else {
            format!(
                "把选中的 {} 个文件 / 文件夹加到这一项后面…",
                selection.files.len()
            )
        };
        entries.push(ctx_entry(format!("__ctx:addsel:{id}"), label, "＋"));
    }
    if item.kind != ItemKind::Separator {
        entries.push(ctx_entry(
            format!("__ctx:toggle:{id}"),
            if item.enabled {
                crate::i18n::t("在菜单里隐藏").to_string()
            } else {
                crate::i18n::t("重新显示").to_string()
            },
            if item.enabled { "◌" } else { "◉" },
        ));
        if item.program_path().is_some() {
            entries.push(ctx_entry(
                format!("__ctx:icon:{id}"),
                if item.badge.trim().is_empty() {
                    crate::i18n::t("用程序图标当图标").to_string()
                } else {
                    crate::i18n::t("清空图标位").to_string()
                },
                "▣",
            ));
        }    }
    entries.push(MenuEntry::Separator);
    entries.push(ctx_entry(
        "__ctx:settings".to_string(),
        crate::i18n::t("打开设置").to_string(),
        "⚙",
    ));
    if item.kind != ItemKind::Separator {
        entries.push(ctx_entry(
            format!("__ctx:delete:{id}"),
            crate::i18n::t("删除这一项").to_string(),
            "✕",
        ));
    }
    // 右键菜单最底下：和列表上方那排按钮同一套「新建」（插在这一项后面当兄弟）
    entries.push(MenuEntry::Separator);
    entries.push(ctx_entry(
        format!("__ctx:add:{id}"),
        crate::i18n::t("添加菜单项").to_string(),
        "＋",
    ));
    entries.push(ctx_entry(
        format!("__ctx:addsub:{id}"),
        crate::i18n::t("添加子菜单").to_string(),
        "▸",
    ));
    entries.push(ctx_entry(
        format!("__ctx:addsep:{id}"),
        crate::i18n::t("添加分隔线").to_string(),
        "─",
    ));
    entries.push(ctx_entry(
        format!("__ctx:addcol:{id}"),
        crate::i18n::t("添加多列符").to_string(),
        "┃",
    ));
    entries
}

pub(crate) fn ctx_entry(id: String, label: String, badge: &str) -> MenuEntry {
    MenuEntry::Item(MenuItem::new(id, label).badge(badge.to_string()))
}

/// 这一项的动作要不要接上「当前选中内容」。
///
/// 三级：项自己设了（`Some`）就按自己的来，否则跟全局开关走 —— 局部优先。
/// 找不到这一项时也只按全局开关算（不留死值）。
pub fn appends_selection(id: &str) -> bool {
    read(|settings| {
        settings
            .find_path(id)
            .and_then(|path| settings.node(&path))
            .and_then(|item| item.append_selection)
            .unwrap_or(settings.append_selection)
    })
}

/// 这一项是不是托盘那一支（界面上要给出不同说明）。
pub fn is_tray(id: &str) -> bool {
    id == TRAY_ID
}

/// 这一项是不是**在**托盘那一支里面（托盘图标右键菜单里点出来的就是这些）。
///
/// 和 [`is_tray`] 不同：那个只认托盘那一项本身，这个认它整棵子树。
/// 「最近运行」用它判断「这一笔是不是托盘菜单里点的」。
pub fn in_tray_branch(id: &str) -> bool {
    read(|settings| settings.in_tray_branch(id))
}

// ---------------------------------------------------------------------------
// 读 / 写 / 落盘
// ---------------------------------------------------------------------------

/// 把设置塞进全局状态（启动时调一次）。
pub fn install(cx: &mut App) {
    SYSTEM_DARK.with(|slot| {
        slot.set(matches!(
            cx.window_appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        ))
    });
    let mut loaded = load().unwrap_or_default();
    // 老配置里的单条 action → 多条 actions；顺便统一 kind
    loaded.migrate();
    // 开机自启：按设置把系统状态对齐一次（exe 挪过位置也会自动更新）
    let autostart = loaded.autostart.clone();
    if let Err(err) =
        crate::autostart::apply(autostart.enabled, autostart.use_task, autostart.as_admin)
    {
        eprintln!("[popup-menu] 对齐开机自启失败：{err}");
    }
    crate::i18n::set_english(loaded.language == Language::English);
    CURRENT.with(|slot| *slot.borrow_mut() = loaded.clone());
    apply_palette_of_current();
    bump_revision();
    let store = cx.new(|_| SettingsStore { value: loaded });
    cx.set_global(SettingsHandle(store));
}

/// 设置窗口观察的实体。改完 `cx.notify()` 让界面重绘。
pub struct SettingsStore {
    pub value: MenuSettings,
}

impl Global for SettingsHandle {}

pub struct SettingsHandle(pub Entity<SettingsStore>);

pub fn store(cx: &App) -> Option<Entity<SettingsStore>> {
    cx.try_global::<SettingsHandle>()
        .map(|handle| handle.0.clone())
}

/// 菜单项撤销栈里的一格（一次改动之前的菜单树）。
pub struct ItemUndo {
    /// 改动发生在哪一份菜单配置里 —— 撤销时如果不在这一份，先切回去。
    profile: String,
    items: Vec<ItemSetting>,
    next_id: u32,
}

thread_local! {
    static ITEM_UNDO: RefCell<Vec<ItemUndo>> = const { RefCell::new(Vec::new()) };
    static ITEM_REDO: RefCell<Vec<ItemUndo>> = const { RefCell::new(Vec::new()) };
    /// 撤销 / 重做自己也会走 `update`，别把「撤销」这一步又记成一次可撤销的改动
    static SUPPRESS_UNDO: Cell<bool> = const { Cell::new(false) };
}

/// 撤销栈上限：60 步够回头了，再多占内存（每一步是一整棵菜单树）。
const ITEM_UNDO_LIMIT: usize = 60;

fn snapshot_items(settings: &MenuSettings) -> ItemUndo {
    ItemUndo {
        profile: settings.effective_profile(),
        items: settings.items.clone(),
        next_id: settings.next_id,
    }
}

fn push_undo(entry: ItemUndo) {
    let profile = entry.profile.clone();
    ITEM_UNDO.with(|stack| {
        let mut stack = stack.borrow_mut();
        stack.push(entry);
        // 超上限时先丢这一份配置里最早的那一步，别把别的配置的栈挤掉
        let mut seen = 0usize;
        let mut drop_at = None;
        for (index, item) in stack.iter().enumerate() {
            if item.profile == profile {
                seen += 1;
                if seen > ITEM_UNDO_LIMIT && drop_at.is_none() {
                    drop_at = Some(index);
                }
            }
        }
        if let Some(index) = drop_at {
            stack.remove(index);
        }
    });
    // 新的改动 = 这一份配置里之前撤销掉的那些不该再重做回来了
    ITEM_REDO.with(|stack| stack.borrow_mut().retain(|item| item.profile != profile));
}

/// 从栈里取某一份配置最近的那一步（别的配置的格子原封不动留在栈里）。
fn take_last_for(stack: &mut Vec<ItemUndo>, profile: &str) -> Option<ItemUndo> {
    let index = stack.iter().rposition(|item| item.profile == profile)?;
    Some(stack.remove(index))
}

/// 撤销最近一次菜单项改动（拖拽 / 删除 / 新增 / 粘贴…）。
///
/// **只撤当前这份配置的**：在配置 A 改了几步、切到 B 按 Ctrl+Z，
/// 撤的是 B 里的改动，A 一个字都不动；B 里没改过就什么也不发生
/// （不会偷偷把配置切回 A —— 那种"按一下撤销，配置跑了"最吓人）。
pub fn undo_items(cx: &mut App) -> bool {
    let profile = read(|settings| settings.effective_profile());
    let Some(entry) = ITEM_UNDO.with(|stack| take_last_for(&mut stack.borrow_mut(), &profile))
    else {
        return false;
    };
    let current = read(snapshot_items);
    ITEM_REDO.with(|stack| stack.borrow_mut().push(current));
    apply_undo(cx, entry);
    true
}

/// 重做刚撤销掉的那一步（同样只在同一份配置里）。
pub fn redo_items(cx: &mut App) -> bool {
    let profile = read(|settings| settings.effective_profile());
    let Some(entry) = ITEM_REDO.with(|stack| take_last_for(&mut stack.borrow_mut(), &profile))
    else {
        return false;
    };
    let current = read(snapshot_items);
    ITEM_UNDO.with(|stack| stack.borrow_mut().push(current));
    apply_undo(cx, entry);
    true
}

fn apply_undo(cx: &mut App, entry: ItemUndo) {
    SUPPRESS_UNDO.with(|flag| flag.set(true));
    update(cx, |settings| {
        // 正常走不到：取格子时就按配置筛过了。真撞上了也不换配置 ——
        // 把别人那份菜单树塞进当前这份比"撤销没反应"糟糕得多。
        if settings.effective_profile() == entry.profile {
            settings.adopt_profile_items(entry.items, entry.next_id);
        }
    });
    SUPPRESS_UNDO.with(|flag| flag.set(false));
}

/// 这段时间里的改动不算「菜单项编辑」（换配置、删配置用）。
fn without_undo<R>(f: impl FnOnce() -> R) -> R {
    SUPPRESS_UNDO.with(|flag| flag.set(true));
    let out = f();
    SUPPRESS_UNDO.with(|flag| flag.set(false));
    out
}

/// 改设置：更新实体（触发重绘）+ 刷新线程本地快照 + 重算配色 + 落盘。
pub fn update<R>(cx: &mut App, f: impl FnOnce(&mut MenuSettings) -> R) -> R {
    let Some(store) = store(cx) else {
        return f(&mut MenuSettings::default());
    };
    let (result, snapshot, before) = store.update(cx, |store, cx| {
        let before = store.value.clone();
        let result = f(&mut store.value);
        cx.notify();
        (result, store.value.clone(), before)
    });
    // 菜单树变了就记一步（撤销用）。配色 / 滑条那些不动菜单树的改动不记。
    if !SUPPRESS_UNDO.with(|flag| flag.get())
        && (before.items != snapshot.items || before.next_id != snapshot.next_id)
    {
        push_undo(ItemUndo {
            profile: before.effective_profile(),
            items: before.items,
            next_id: before.next_id,
        });
    }
    crate::i18n::set_english(snapshot.language == Language::English);
    CURRENT.with(|slot| *slot.borrow_mut() = snapshot.clone());
    apply_palette_of_current();
    bump_revision();
    save(&snapshot);
    // 快捷键可能刚被改过：对齐一下系统注册表（幂等，没变就一个系统调用都不发）
    crate::hotkey::sync(cx);
    // 热字符串的短语 / 提示参数可能刚被改过：重新编译规则
    crate::hotstring::sync(cx);
    // 托盘图标 / 提示文字可能刚被改过
    crate::tray::refresh(cx);
    // 不透明度可能刚被拖过：直接套到开着的菜单窗口上
    crate::popup::apply_opacity(cx);
    // 尺寸/配色变了，所有窗口都要跟着重画
    cx.refresh_windows();
    result
}

/// 设置文件的完整路径（`settings.json`：配色 / 快捷键 / 热字符串这些偏好）。
pub fn config_path_display() -> Option<String> {
    file_path().map(|path| path.display().to_string())
}

/// 设置文件的路径（备份 / 恢复要按它复制）。
pub fn config_path() -> Option<PathBuf> {
    file_path()
}

/// 「文件名」页那张「名字 → 完整路径」的表单独存这儿。
///
/// 它本质是缓存：条目跟菜单里用到的裸文件名一样多，塞进 `settings.json`
/// 会让那个文件从几 KB 涨到几 MB，改个配色都要重写一遍。
pub fn fixes_path() -> Option<PathBuf> {
    Some(file_path()?.with_file_name("program-fixes.json"))
}

/// 读单独存的 fixes 表（文件不存在 / 坏了 = 空表）。
fn load_fixes() -> std::collections::BTreeMap<String, String> {
    fixes_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_fixes(fixes: &std::collections::BTreeMap<String, String>) {
    let Some(path) = fixes_path() else {
        return;
    };
    let Ok(text) = serde_json::to_string_pretty(fixes) else {
        return;
    };
    // 没变就别写：`save` 会被每次改设置触发
    if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
        return;
    }
    let _ = std::fs::write(path, text);
}

/// 从磁盘重新读一遍设置（恢复备份之后用）。读不出来就保持现状 ——
/// 恢复了一份坏文件时不能把内存里好好的设置也清掉。
pub fn reload(cx: &mut App) -> bool {
    let Some(path) = file_path() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(mut loaded) = serde_json::from_str::<MenuSettings>(&text) else {
        return false;
    };
    loaded.migrate();
    crate::i18n::set_english(loaded.language == Language::English);
    CURRENT.with(|slot| *slot.borrow_mut() = loaded.clone());
    apply_palette_of_current();
    bump_revision();
    if let Some(store) = store(cx) {
        store.update(cx, |store, cx| {
            store.value = loaded;
            cx.notify();
        });
    }
    crate::hotkey::sync(cx);
    crate::hotstring::sync(cx);
    crate::tray::refresh(cx);
    cx.refresh_windows();
    true
}

/// 放配置的目录（`settings.json` 和 `menus/` 都在这一层）。
pub fn config_dir_display() -> Option<String> {
    file_path()?.parent().map(|dir| dir.display().to_string())
}

fn file_path() -> Option<PathBuf> {
    // 纯便携：配置就跟 exe 放一起，整个文件夹拷走就能搬家
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("settings.json"))
}

/// 老版本的配置位置（`%APPDATA%\gpui-popup-menu\settings.json`）：
/// 只在「便携文件还不存在」时读一次，读到就顺手写到 exe 旁边。
fn legacy_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("gpui-popup-menu").join("settings.json"))
}

fn load() -> Option<MenuSettings> {
    let mut found: Option<MenuSettings> = None;
    if let Some(path) = file_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str(&text) {
                Ok(settings) => found = Some(settings),
                // 读不动就报一声，否则用户只会看到「设置全丢了」却不知道为什么
                Err(err) => eprintln!(
                    "[popup-menu] {} 解析失败（将使用默认设置）：{err}",
                    path.display()
                ),
            }
        }
    }
    if found.is_none() {
        // 便携文件还没有 → 试试老位置，搬过来再继续
        if let Some(text) = legacy_path().and_then(|path| std::fs::read_to_string(path).ok()) {
            if let Ok(settings) = serde_json::from_str::<MenuSettings>(&text) {
                save(&settings);
                found = Some(settings);
            }
        }
    }
    let Some(mut settings) = found else {
        // 全新环境（典型场景：单文件 exe 刚被拷到一个空目录）：
        //
        // 不能只把默认值放在内存里就完事 —— 那样 `menus/` 目录根本不会被建出来，
        // 用户在文件管理器里看到的还是光秃秃一个 exe，想改菜单都找不到文件。
        // 这里直接把默认菜单落成 `menus/默认.runa`（`hydrate_profile` 里会
        // `create_dir_all`），再顺手写一份 `settings.json`，从此以后文件就在那儿了。
        //
        // 用 `bundled_default()`：打包进 exe 的那套默认（作者调好的外观 / 快捷键 / 菜单），
        // 不是代码里写死的那套示例。
        let mut settings = bundled_default();
        hydrate_profile(&mut settings);
        save(&settings);
        return Some(settings);
    };
    // 老版本的 `settings.json` 里存着整份菜单（还有「文件名」那张表）：
    // 读过一次就让它瘦下来 —— 不然用户会一直对着一个几 MB 的 json 文件，
    // 直到哪天改了个设置才被改写。
    let was_fat = !settings.items.is_empty();
    hydrate_profile(&mut settings);
    adopt_fixes(&mut settings);
    if was_fat {
        save(&settings);
    }
    Some(settings)
}

/// 把「老配置里那份 fixes」搬到 `program-fixes.json`，之后只认新文件。
///
/// 老版本把这张表写在 `settings.json` 的 `programs.fixes` 里：第一次读到就
/// 合并进新文件（新文件优先），顺手把 `settings.json` 重写一遍 —— 少了这一大坨，
/// 那个文件立刻瘦回去。
fn adopt_fixes(settings: &mut MenuSettings) {
    let stored = load_fixes();
    let legacy = std::mem::take(&mut settings.programs.fixes);
    if legacy.is_empty() {
        settings.programs.fixes = stored;
        return;
    }
    let mut merged = stored;
    for (key, value) in legacy {
        merged.entry(key).or_insert(value);
    }
    settings.programs.fixes = merged.clone();
    save_fixes(&merged);
    // 老 `settings.json` 里还留着这份大表：重写一次把它挤掉。
    // （`save` 里 `fixes` 是 `skip_serializing`，写出来就没有了）
    save(settings);
}

/// 菜单项的真身在 `menus/<名字>.runa`；`settings.json` 里那份只是兜底快照。
///
/// 读的时候顺序是：先按 `menu_profile` 找 `.runa`，找到了就用它覆盖内存里的 items；
/// 找不到（第一次升级到多配置 / 用户手动删了文件）就把当前 items 写出去 ——
/// 这样老用户升级上来不会突然发现「菜单空了」。
fn hydrate_profile(settings: &mut MenuSettings) {
    let profile = settings.effective_profile();
    match crate::menu_files::load(&profile) {
        Some(file) => {
            if !file.items.is_empty() || settings.items.is_empty() {
                settings.adopt_profile_items(file.items, file.next_id);
            }
        }
        None => {
            // 还没这份文件：把现有菜单（可能是老配置里的）落成第一份。
            // `items` 空有两种来路：老配置压根没这个字段（serde 会给默认菜单，
            // 不会空），或者 `menus/` 被删了而 `settings.json` 是精简版 ——
            // 后者就当新装，给一份默认菜单，别让用户对着空菜单发愣。
            let source = if settings.items.is_empty() {
                default_items()
            } else {
                settings.items.clone()
            };
            let items: Vec<ItemSetting> = source
                .iter()
                .filter(|item| item.id != TRAY_ID)
                .cloned()
                .collect();
            let _ = crate::menu_files::save(&profile, &items, settings.next_id);
        }
    }
    settings.menu_profile = profile;
}

fn save(settings: &MenuSettings) {
    // 菜单项单独落到 `menus/*.runa`：换一套菜单不影响配色 / 快捷键这些。
    // 托盘那一支**不写进去**：它是这台机器的（右键图标）菜单，不属于某套菜单。
    //
    // 切了配置但还没点「应用 / 确定」时**一个字节都不写**：否则用户只是点开
    // 下拉看一眼，`menus/` 里那两份文件就都被改写了。
    let mut menu_saved = false;
    if settings.pending_profile.is_none() {
        let profile = settings.effective_profile();
        let profile_items: Vec<ItemSetting> = settings
            .items
            .iter()
            .filter(|item| item.id != TRAY_ID)
            .cloned()
            .collect();
        menu_saved = crate::menu_files::save(&profile, &profile_items, settings.next_id).is_ok();
    }

    // 「文件名」页那张表单独存，不跟着 settings.json 一起变大
    save_fixes(&settings.programs.fixes);

    // settings.json：托盘单独一段
    let mut file = settings.split_tray();
    // 菜单已经落到 `menus/*.runa` 了，就别在 settings.json 里再存一份：
    // 两千条菜单项写两遍要一兆多，而改配色 / 改快捷键每次都要重写这个文件。
    // 只有真正写不进去（只读目录之类）时才留这份兜底。
    if menu_saved {
        file.items.clear();
    }

    let Ok(text) = serde_json::to_string_pretty(&file) else {
        return;
    };
    if let Some(path) = file_path() {
        if std::fs::write(&path, &text).is_ok() {
            return;
        }
    }
    // 程序装在只读目录（Program Files 之类）时退回用户目录，至少别把设置丢掉
    if let Some(path) = legacy_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use crate::selection::Context;

    /// 测试里只关心「选中了哪几类」，后缀一律给空（= 不限）。
    fn ctx(kinds: &[SelectionKind]) -> Context {
        Context::new(kinds.to_vec(), Vec::new())
    }

    use super::*;

    fn collect_ids(items: &[ItemSetting], out: &mut Vec<String>) {
        for item in items {
            out.push(item.id.clone());
            collect_ids(&item.children, out);
        }
    }

    /// 「删除」必须删界面上下拉框里显示的那份 —— 换配置只是预览，
    /// `menu_profile` 还没变。以前拿 `effective_profile()` 去删，就是
    /// 「看到的是 B，删掉的却是 A」。
    #[test]
    fn delete_targets_the_profile_shown_in_the_dropdown() {
        let mut settings = MenuSettings::default();
        settings.menu_profile = "A".to_string();
        settings.pending_profile = Some("B".to_string());
        assert_eq!(settings.shown_profile(), "B");
        assert_eq!(settings.effective_profile(), "A");
    }

    #[test]
    fn fallback_prefers_default_then_first_remaining() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            fallback_after_delete(&names(&["工具", "默认"])),
            "默认",
            "有「默认」就回默认那份"
        );
        assert_eq!(
            fallback_after_delete(&names(&["工具", "游戏"])),
            "工具",
            "没有「默认」就用目录里第一份"
        );
        assert_eq!(
            fallback_after_delete(&[]),
            "默认",
            "一份都不剩也得给个合法名字"
        );
    }

    #[test]
    fn default_menu_keeps_the_three_level_cascade() {
        let settings = MenuSettings::default();
        let menu = settings.menu_for(None);
        assert!(menu.len() >= 6);
        let edit = menu
            .iter()
            .find_map(|entry| entry.item().filter(|item| item.label == "编辑"))
            .expect("要有「编辑」这一项");
        assert!(edit.has_children());
    }

    /// 造一个「文字 / 文件 / 图片 各一个子菜单 + 一条通用项」的配置。
    fn context_menu() -> MenuSettings {
        fn with(mut item: ItemSetting, contexts: &[SelectionKind]) -> ItemSetting {
            item.contexts = contexts.to_vec();
            item.context_mode = ContextMode::Own;
            item
        }
        let mut settings = MenuSettings::default();
        settings.items = vec![
            with(
                ItemSetting::item("t", "文字子菜单").children(vec![
                    ItemSetting::item("t1", "翻译"),
                    ItemSetting::separator(),
                    ItemSetting::item("t2", "搜索"),
                ]),
                &[SelectionKind::Text],
            ),
            with(
                ItemSetting::item("f", "文件子菜单")
                    .children(vec![ItemSetting::item("f1", "用记事本打开")]),
                &[SelectionKind::File],
            ),
            with(
                ItemSetting::item("i", "图片子菜单")
                    .children(vec![ItemSetting::item("i1", "压缩图片")]),
                &[SelectionKind::Image],
            ),
            ItemSetting::item("any", "打开设置"),
        ];
        settings
    }

    fn labels(menu: &[MenuEntry]) -> Vec<String> {
        menu.iter()
            .filter_map(|entry| entry.item().map(|item| item.label.to_string()))
            .collect()
    }

    #[test]
    fn selecting_text_flattens_only_the_text_submenu() {
        let settings = context_menu();
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        // 子菜单这一层被摊掉了，直接是它里面的项
        assert_eq!(labels(&menu), vec!["翻译", "搜索"]);
    }

    #[test]
    fn common_items_are_hidden_once_something_is_selected() {
        let settings = context_menu();
        // 选中文字时，「打开设置」这种没声明类型的通用项一起藏起来
        assert!(!labels(&settings.menu_for(Some(&ctx(&[SelectionKind::Text]))))
            .contains(&"打开设置".to_string()));
        // 没选中内容时它照常出现
        assert!(labels(&settings.menu_for(None)).contains(&"打开设置".to_string()));
    }

    #[test]
    fn image_files_light_up_both_the_file_and_image_submenus() {
        let settings = context_menu();
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::File, SelectionKind::Image])));
        // 文件和图片两个子菜单都命中 → 不摊平，各自留成一个子菜单
        assert_eq!(labels(&menu), vec!["文件子菜单", "图片子菜单"]);
    }

    #[test]
    fn a_single_matching_submenu_is_flattened() {
        // 只有「文字」这一个子菜单配了筛选 → 摊平
        let settings = context_menu();
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["翻译", "搜索"]);
    }

    #[test]
    fn two_matching_submenus_are_kept_as_submenus() {
        // 文字 + 文件两个子菜单都标了「文字」→ 有两个命中，就不摊平
        let mut settings = context_menu();
        // 把「文件子菜单」也标成文字
        settings.items[1].contexts = vec![SelectionKind::Text];
        settings.items[1].context_mode = ContextMode::Own;
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["文字子菜单", "文件子菜单"]);
    }

    /// 「只有一支活着」才摊平 —— 数的是**剪完还剩东西**的子菜单，不是「标了筛选」的。
    ///
    /// 这条以前是反的：另跑一趟 `filter_submenus` 数「Own 且这次命中」的子菜单，
    /// 完全不看它里面剩不剩东西。于是「配了筛选但全军覆没」的那一支被算成第二个，
    /// 唯一活下来的一支摊不平，用户还得再点一层。
    #[test]
    fn a_submenu_that_emptied_out_does_not_block_flattening() {
        let mut settings = context_menu();
        // 第二个子菜单也标成「文字」，但它里面的项全被关掉了
        settings.items[1].contexts = vec![SelectionKind::Text];
        settings.items[1].context_mode = ContextMode::Own;
        for child in &mut settings.items[1].children {
            child.enabled = false;
        }
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["翻译", "搜索"]);
    }

    /// 后缀限制把一支剪空了，同样不该挡住另一支摊平。
    #[test]
    fn a_submenu_emptied_by_the_extension_rule_does_not_block_flattening() {
        let mut settings = context_menu();
        // 两支都只对文件生效，但第二支限死 mp4（只改这两支，别把「打开设置」也标成文件）
        for item in settings.items.iter_mut().take(2) {
            item.contexts = vec![SelectionKind::File];
            item.context_mode = ContextMode::Own;
        }
        settings.items[1].file_exts = "mp4|mkv".to_string();
        settings.items[0].children.push(ItemSetting::item("t3", "重命名"));
        let context = Context::new(
            vec![SelectionKind::File],
            vec!["D:////片////a.txt".to_string()],
        );
        let menu = settings.menu_for(Some(&context));
        // 只有 txt 子菜单活着 → 直接摊开它
        assert_eq!(labels(&menu), vec!["翻译", "搜索", "重命名"]);
    }

    #[test]
    fn a_nested_submenu_flattens_all_the_way_down() {
        // 一级「文字」里有个二级「翻译」，两个都标了「文字」：
        // 按**最深**的那支摊，沿途的壳（文字 → 翻译）全部化掉，
        // 根层直接看到「中→英」——不该还留一个「翻译」让人再点一下
        let mut b = ItemSetting::item("b", "翻译").children(vec![ItemSetting::item("b1", "中→英")]);
        b.contexts = vec![SelectionKind::Text];
        b.context_mode = ContextMode::Own;
        let mut a = ItemSetting::item("a", "文字").children(vec![b]);
        a.contexts = vec![SelectionKind::Text];
        a.context_mode = ContextMode::Own;
        let mut settings = MenuSettings::default();
        settings.items = vec![a];

        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["中→英"], "父项一级和二级都不该露出来");
        assert!(!menu[0].item().unwrap().has_children());
    }

    #[test]
    fn a_shell_with_both_a_submenu_and_an_item_is_flattened() {
        // 外壳里除了子菜单还有普通项：普通项跟着一起摊到根层，
        // 不能因为「不是纯外壳」就把一级父项整个留着（那看起来跟没摊一样）
        let mut b = ItemSetting::item("b", "翻译").children(vec![ItemSetting::item("b1", "中→英")]);
        b.contexts = vec![SelectionKind::Text];
        b.context_mode = ContextMode::Own;
        let mut a = ItemSetting::item("a", "文字").children(vec![
            b,
            ItemSetting::item("c", "搜索"), // 继承「文字」，也命中
        ]);
        a.contexts = vec![SelectionKind::Text];
        a.context_mode = ContextMode::Own;
        let mut settings = MenuSettings::default();
        settings.items = vec![a];

        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["中→英", "搜索"]);
        assert!(!menu[0].item().unwrap().has_children());
    }

    #[test]
    fn a_local_item_is_promoted_to_the_root() {
        // 子菜单里的普通项单独标了「文字」→ 提到根菜单，原来那层不再重复
        let mut settings = context_menu();
        settings.items[0].children[0].contexts = vec![SelectionKind::Text];
        settings.items[0].children[0].context_mode = ContextMode::Own;
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        // **不摊平**：根层上既有被摊开的子项、又有单独浮上来的项，两种来源混在一起
        // 分不清谁是谁 —— 那就把子菜单的名字留着。浮动项按它自己的「浮动位置」排
        // （默认 `Top`，所以「翻译」在最前面）
        assert_eq!(labels(&menu), vec!["翻译", "文字子菜单"]);
        assert_eq!(menu[0].item().unwrap().id.to_string(), "t1");
        // 「翻译」已经浮到根层，子菜单里只剩「搜索」
        let submenu = menu[1].item().expect("第二项是子菜单");
        assert!(submenu.has_children());
        assert_eq!(labels(&submenu.children), vec!["搜索"]);
    }

    /// 根层 = [子菜单 A, 普通项 B, 子菜单 C, 普通项 D]，A 里有个要浮上来的 x。
    fn placement_menu(placement: FloatPlacement) -> MenuSettings {
        let mut x = ItemSetting::item("x", "浮动项");
        x.contexts = vec![SelectionKind::Text];
        x.context_mode = ContextMode::Own;
        x.float_placement = placement;
        let mut a = ItemSetting::item("a", "A").children(vec![
            ItemSetting::item("a1", "A1"),
            x,
        ]);
        a.contexts = vec![SelectionKind::Text];
        a.context_mode = ContextMode::Own;
        let mut c = ItemSetting::item("c", "C").children(vec![ItemSetting::item("c1", "C1")]);
        c.contexts = vec![SelectionKind::Text];
        c.context_mode = ContextMode::Own;
        let mut items = vec![
            a,
            ItemSetting::item("b", "B"),
            c,
            ItemSetting::item("d", "D"),
        ];
        for item in items.iter_mut() {
            if item.id == "b" || item.id == "d" {
                item.contexts = vec![SelectionKind::Text];
                item.context_mode = ContextMode::Own;
            }
        }
        let mut settings = MenuSettings::default();
        settings.items = items;
        settings
    }

    #[test]
    fn a_floating_item_can_choose_where_it_lands() {
        let kinds = [SelectionKind::Text];
        let with = |placement: FloatPlacement| labels(&placement_menu(placement).menu_for(Some(&ctx(&kinds))));

        // 默认：最前面
        assert_eq!(with(FloatPlacement::Top), vec!["浮动项", "A", "B", "C", "D"]);
        // 子菜单前面：根层第一个子菜单之前（这里就是最前面）
        assert_eq!(
            with(FloatPlacement::BeforeSubmenus),
            vec!["浮动项", "A", "B", "C", "D"]
        );
        // 子菜单后面：最后一个子菜单（C）之后，落在 D 前面
        assert_eq!(
            with(FloatPlacement::AfterSubmenus),
            vec!["A", "B", "C", "浮动项", "D"]
        );
        // 按原顺序：它原本在 A 里面（排在 A1 之后），所以跟回 A 后面
        assert_eq!(
            with(FloatPlacement::Order),
            vec!["A", "浮动项", "B", "C", "D"]
        );
    }

    #[test]
    fn the_tray_branch_is_a_root_submenu_but_never_shows_in_the_popup() {
        let settings = MenuSettings::default();
        assert!(
            settings.items.iter().any(|item| item.id == TRAY_ID),
            "默认配置里应该有托盘那一支"
        );
        // 它不会作为普通项出现在弹出菜单里
        let menu = settings.menu_for(None);
        assert!(!menu
            .iter()
            .filter_map(|entry| entry.item())
            .any(|item| item.id == TRAY_ID));
        assert!(!menu
            .iter()
            .filter_map(|entry| entry.item())
            .any(|item| item.label == "托盘右键菜单"));

        // 托盘右键用的就是它的子项
        let tray = settings.tray_menu();
        let labels: Vec<&str> = tray
            .iter()
            .filter_map(|entry| entry.item())
            .map(|item| item.label.as_ref())
            .collect();
        assert!(labels.contains(&"设置…"), "{labels:?}");
        assert!(labels.contains(&"退出"), "{labels:?}");
    }

    #[test]
    fn the_tray_branch_is_added_to_old_configs() {
        let mut settings = MenuSettings::default();
        settings.items.retain(|item| item.id != TRAY_ID);
        assert!(settings.tray_menu().is_empty());
        settings.migrate();
        assert!(!settings.tray_menu().is_empty(), "迁移要把它补回来");
    }

    #[test]
    fn a_profile_switch_is_only_a_preview_until_committed() {
        let mut settings = MenuSettings::default();
        settings.menu_profile = "默认".into();
        // 点了下拉里另一份：只是预览
        settings.pending_profile = Some("第二套".into());
        assert_eq!(settings.shown_profile(), "第二套", "界面显示正在预览的那份");
        assert_eq!(
            settings.effective_profile(),
            "默认",
            "没点应用之前，落盘还写原来那份"
        );
        // 点了「应用 / 确定」
        settings.menu_profile = settings.pending_profile.take().unwrap();
        assert_eq!(settings.effective_profile(), "第二套");
        assert_eq!(settings.shown_profile(), "第二套");
    }

    #[test]
    fn the_tray_menu_survives_a_profile_switch() {
        let mut settings = MenuSettings::default();
        if let Some(tray) = settings.items.iter_mut().find(|item| item.id == TRAY_ID) {
            tray.children[0].label = "只属于这台机器".into();
        }
        // 「别人分享的」那份配置里没有托盘
        let mut incoming = MenuSettings::default();
        incoming.items.retain(|item| item.id != TRAY_ID);
        settings.adopt_profile_items(incoming.items, incoming.next_id);

        let tray = settings.tray_root().expect("换配置不能把托盘弄丢");
        assert_eq!(tray.children[0].label, "只属于这台机器");
        assert_eq!(
            settings.items.iter().filter(|item| item.id == TRAY_ID).count(),
            1,
            "托盘只能有一份"
        );
    }

    #[test]
    fn the_tray_is_written_to_settings_json_not_to_the_profile() {
        let settings = MenuSettings::default();
        // 直接走 `save()` 用的那套拆分，别在这里抄一遍（抄的会跟实现跑偏）
        let file = settings.split_tray();

        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("tray_menu"));
        // `items` 里不能再出现托盘（它只在 `tray_menu` 那一段）
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let items = parsed["items"].as_array().expect("items 是数组");
        assert!(
            !items.iter().any(|item| item["id"] == TRAY_ID),
            "托盘不该混在菜单项里"
        );

        let mut back: MenuSettings = serde_json::from_str(&json).unwrap();
        back.migrate();
        assert_eq!(
            back.tray_root().map(|tray| tray.id.as_str()),
            Some(TRAY_ID)
        );
        // 别的菜单项一个没少
        assert_eq!(
            back.items.iter().filter(|item| item.id != TRAY_ID).count(),
            settings.items.iter().filter(|item| item.id != TRAY_ID).count()
        );
    }

    /// 老配置里没有 `everything` 这一节（升级上来的用户）：要能正常解析，
    /// 而且默认是「不自动启动、路径留空让程序自己找」。
    #[test]
    fn old_settings_without_the_everything_section_still_load() {
        let json = r#"{"panel_width": 320.0, "row_height": 26.0}"#;
        let settings: MenuSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.everything, EverythingSettings::default());
        assert!(!settings.everything.autostart);
        assert!(settings.everything.exe.is_empty());
    }

    /// 填过路径、勾过「随本程序启动」之后要原样存回来（重启还认得那个 exe）。
    #[test]
    fn the_everything_path_survives_a_save_and_load() {
        let mut settings = MenuSettings::default();
        settings.everything.exe = r"D:\RunAny\Everything\Everything.exe".into();
        settings.everything.autostart = true;

        let json = serde_json::to_string(&settings).unwrap();
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.everything.exe,
            r"D:\RunAny\Everything\Everything.exe"
        );
        assert!(back.everything.autostart);
    }

    #[test]
    fn pasted_items_get_fresh_unique_ids() {
        let mut settings = MenuSettings::default();
        let mut incoming = MenuSettings::default();
        incoming.items.retain(|item| item.id != TRAY_ID);
        let count = incoming.items.len();
        let ids = settings.paste_items(&[], None, incoming.items);
        assert_eq!(ids.len(), count);

        let mut seen = std::collections::HashSet::new();
        fn walk(items: &[ItemSetting], seen: &mut std::collections::HashSet<String>) -> bool {
            items
                .iter()
                .all(|item| seen.insert(item.id.clone()) && walk(&item.children, seen))
        }
        assert!(walk(&settings.items, &mut seen), "粘贴后 id 必须唯一");
    }

    #[test]
    fn a_refused_move_puts_the_node_back() {
        let mut settings = MenuSettings::default();
        let before = settings.items.len();
        let submenu = settings
            .items
            .iter()
            .position(|item| !item.children.is_empty())
            .expect("默认菜单里有子菜单");
        // 拖到自己身上：必须拒绝，而且不能把这一项弄丢（以前会消失）
        let moved = settings.move_node(&[submenu], &DropTarget::Into(vec![submenu]));
        assert!(!moved);
        assert_eq!(settings.items.len(), before);
    }

    #[test]
    fn undo_steps_stay_inside_their_own_profile() {
        let step = |profile: &str, next_id: u32| ItemUndo {
            profile: profile.to_string(),
            items: Vec::new(),
            next_id,
        };
        let mut stack = vec![step("A", 1), step("B", 2), step("A", 3)];
        // 在 B 里撤销：只拿到 B 那一步，A 的两步原样留着
        assert_eq!(take_last_for(&mut stack, "B").map(|s| s.next_id), Some(2));
        assert_eq!(stack.len(), 2, "A 的格子不该被一起拿走");
        // B 里没有更多可撤的了 —— 不会跑去撤 A，更不会把配置切回 A
        assert!(take_last_for(&mut stack, "B").is_none());
        // A 里按后进先出取
        assert_eq!(take_last_for(&mut stack, "A").map(|s| s.next_id), Some(3));
        assert_eq!(take_last_for(&mut stack, "A").map(|s| s.next_id), Some(1));
        assert!(stack.is_empty());
    }

    #[test]
    fn a_new_item_starts_with_a_run_action() {
        let mut settings = MenuSettings::default();
        let id = settings.add_item_in(&[], None);
        let item = settings.item_by_id(&id).expect("新项要能找回来");
        assert_eq!(item.actions.len(), 1);
        assert_eq!(item.actions[0].kind, ActionKind::Run);
        assert!(item.actions[0].value.is_empty(), "先留空，等用户填路径");
    }

    #[test]
    fn an_item_can_carry_a_note() {
        let mut item = ItemSetting::item("a", "A");
        assert!(item.note.is_empty());
        item.note = "记得改端口".to_string();
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("记得改端口"), "{json}");
        let back: ItemSetting = serde_json::from_str(&json).unwrap();
        assert_eq!(back.note, "记得改端口");

        // 没写备注就不写进 JSON
        let plain = serde_json::to_string(&ItemSetting::item("b", "B")).unwrap();
        assert!(!plain.contains("note"), "{plain}");
    }

    #[test]
    fn append_selection_follows_global_until_it_is_pinned() {
        let item = ItemSetting::item("a", "A");
        assert_eq!(item.append_selection, None, "默认跟随全局");
        assert_eq!(item.auto_icon, None);
        let json = serde_json::to_string(&item).unwrap();
        assert!(!json.contains("append_selection"), "{json}");
        assert!(!json.contains("auto_icon"), "{json}");

        // 单独定了才写进去，读回来还是那个值
        let mut item = ItemSetting::item("a", "A");
        item.append_selection = Some(true);
        item.auto_icon = Some(false);
        let json = serde_json::to_string(&item).unwrap();
        let back: ItemSetting = serde_json::from_str(&json).unwrap();
        assert_eq!(back.append_selection, Some(true));
        assert_eq!(back.auto_icon, Some(false));

        // 老配置里存的是 `false`（当时只有一个勾选框）：收成「跟随全局」，
        // 否则全局开关永远推不动这一项
        let mut old: ItemSetting =
            serde_json::from_str(r#"{"id":"a","label":"A","append_selection":false}"#).unwrap();
        old.migrate();
        assert_eq!(old.append_selection, None);
    }

    #[test]
    fn the_context_menu_offers_edit_delete_and_settings() {
        let entries = super::item_context_menu("new-file");
        let ids: Vec<String> = entries
            .iter()
            .filter_map(|entry| entry.item())
            .map(|item| item.id.to_string())
            .collect();
        assert!(ids.contains(&"__ctx:edit:new-file".to_string()), "{ids:?}");
        assert!(ids.contains(&"__ctx:delete:new-file".to_string()), "{ids:?}");
        assert!(ids.contains(&"__ctx:copy:new-file".to_string()), "{ids:?}");
        assert!(ids.contains(&"__ctx:settings".to_string()), "{ids:?}");

        // 找不到的 id 就没有菜单（右键点到不存在的项时不会有反应）
        assert!(super::item_context_menu("nope").is_empty());
    }

    /// 「文件名」页那张「名字 → 路径」的表不再塞进 `settings.json`：
    /// 老配置里的还能读（迁移），但写出去就没有了。
    #[test]
    fn program_fixes_live_outside_settings_json() {
        let mut settings = MenuSettings::default();
        settings
            .programs
            .fixes
            .insert("foo.exe".into(), "C:////tools////foo.exe".into());
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("foo.exe"), "手动指定的路径不该写进 settings.json");

        // 老配置（还写在 programs.fixes 里）照样能读出来，交给迁移逻辑搬走
        let legacy = r#"{"programs":{"fixes":{"foo.exe":"C:////tools////foo.exe"}}}"#;
        let back: MenuSettings = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            back.programs.fixes.get("foo.exe").map(String::as_str),
            Some("C:////tools////foo.exe")
        );
    }

    #[test]
    fn file_exts_are_saved_and_omitted_when_empty() {        let mut settings = MenuSettings::default();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("file_exts"), "没填就不用存：{json}");

        settings.items[0].file_exts = "mp4|mkv".to_string();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"file_exts\""), "{json}");
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items[0].file_exts, "mp4|mkv");
        // 后缀按用户写的原样存（输入框要能原样回显）
        assert_eq!(back.items[0].ext_list(), vec!["mp4", "mkv"]);

        // 老配置没有这个字段也能读（serde default）
        let legacy = r#"{"panel_width": 280.0}"#;
        let back: MenuSettings = serde_json::from_str(legacy).unwrap();
        assert!(back.items[0].file_exts.is_empty());
    }

    /// 一个「视频文件」子菜单：子菜单自己写了后缀，里面的项默认跟着；
    /// 另有一项自己「单独指定」且不限后缀，任何文件都该出现。
    fn ext_menu(rule: MultiFileRule) -> MenuSettings {
        let play = ItemSetting::item("play", "播放");
        let convert = ItemSetting::item("convert", "转成 mp4");

        let mut submenu =
            ItemSetting::item("videos", "视频文件").children(vec![play, convert]);
        submenu.kind = ItemKind::Submenu;
        submenu.contexts = vec![SelectionKind::File];
        submenu.context_mode = ContextMode::Own;
        submenu.file_exts = "mp4|mkv".to_string();

        let mut any = ItemSetting::item("any", "发送到…");
        any.contexts = vec![SelectionKind::File];
        any.context_mode = ContextMode::Own;

        let mut settings = MenuSettings::default();
        settings.items = vec![submenu, any];
        settings.multi_file_rule = rule;
        settings
    }

    fn files(list: &[&str]) -> Context {
        Context::new(
            vec![SelectionKind::File],
            list.iter().map(|path| path.to_string()).collect(),
        )
    }

    #[test]
    fn an_item_with_suffixes_only_shows_up_for_those_suffixes() {
        let settings = ext_menu(MultiFileRule::AllSame);
        // 选中 .mkv：子菜单里的项（继承 mp4|mkv）和「发送到…」都在
        let video = labels(&settings.menu_for(Some(&files(&[r"C:/a/x.mkv"]))));
        assert!(video.contains(&"播放".to_string()), "{video:?}");
        assert!(video.contains(&"发送到…".to_string()), "{video:?}");
        // 选中 .png：要求后缀的整支都不出现，只剩不限后缀的「发送到…」
        let image = labels(&settings.menu_for(Some(&files(&[r"C:/a/x.png"]))));
        assert_eq!(image, vec!["发送到…"]);
        // 后缀写法大小写不敏感
        let upper = labels(&settings.menu_for(Some(&files(&[r"C:/a/X.MKV"]))));
        assert!(upper.contains(&"播放".to_string()), "{upper:?}");
    }

    #[test]
    fn a_child_inherits_the_suffixes_of_its_submenu() {
        let settings = ext_menu(MultiFileRule::AllSame);
        // 子项自己没写后缀，跟着子菜单的 mp4|mkv 走
        let shown = labels(&settings.menu_for(Some(&files(&[r"C:/a/x.png"]))));
        assert!(!shown.contains(&"播放".to_string()), "{shown:?}");
        assert!(!shown.contains(&"转成 mp4".to_string()), "{shown:?}");
    }

    #[test]
    fn selecting_a_folder_never_matches_suffix_rules() {
        let settings = ext_menu(MultiFileRule::AllSame);
        let folder = std::env::current_dir().unwrap().display().to_string();
        let context = Context::new(vec![SelectionKind::Folder], vec![folder]);
        assert!(settings.menu_for(Some(&context)).is_empty());
    }

    #[test]
    fn placement_is_only_written_out_when_it_is_not_the_default() {
        let mut settings = MenuSettings::default();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("float_placement"), "默认位置不用存：{json}");
        settings.items[0].float_placement = FloatPlacement::AfterSubmenus;
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            json.contains("\"float_placement\":\"after-submenus\""),
            "{json}"
        );
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items[0].float_placement, FloatPlacement::AfterSubmenus);
    }

    /// 动作库「复制」出来的那段 JSON：既是 `.runa` 的 items 元素，也能直接粘回来。
    #[test]
    #[test]
    fn plugin_call_action_round_trips_through_json() {
        // 插件调用是「内置动作 runany-plugin + 参数 call」，要能原样存盘再读回来
        let mut params = std::collections::BTreeMap::new();
        params.insert(
            "call".to_string(),
            "XiaoYao_plus[hotkeyStatus](%\"clash.exe\"%)".to_string(),
        );
        let step = ActionStep::with_params(ActionKind::Builtin, "runany-plugin", params);
        let json = serde_json::to_string(&step).unwrap();
        assert!(json.contains("runany-plugin"), "动作 id 要写进 JSON: {json}");
        let back: ActionStep = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, ActionKind::Builtin);
        assert_eq!(
            back.param("call"),
            Some("XiaoYao_plus[hotkeyStatus](%\"clash.exe\"%)")
        );
        // 老配置里没有这个 param 也不该炸
        let old: ActionStep = serde_json::from_str(r#"{"kind":"Text","value":"abc"}"#).unwrap();
        assert_eq!(old.kind, ActionKind::Text);
    }

    fn an_action_item_copies_and_pastes_back() {
        let def = crate::actions::find("sys.volume").expect("动作库里得有调节音量");
        let node = MenuSettings::action_item(def.id, def.name, crate::actions::defaults(def));
        let pretty = serde_json::to_string_pretty(&node).unwrap();
        assert!(pretty.contains("\"Builtin\""), "{pretty}");

        let single = items_from_clipboard(&pretty).expect("单个菜单项对象也要认");
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].actions[0].value, "sys.volume");
        assert!(!single[0].actions[0].params.is_empty(), "参数要跟着一起走");

        let payload = clipboard_payload(std::slice::from_ref(&node)).unwrap();
        assert_eq!(items_from_clipboard(&payload).expect("整份 payload 也要认").len(), 1);
        assert!(items_from_clipboard("随便一句话").is_none());
    }

    /// 拖动**永远不能**把菜单项挪进托盘那一支，也不能把托盘拖出来。
    #[test]
    fn dragging_never_crosses_into_the_tray() {
        let mut settings = MenuSettings::default();
        let tray = settings.find_path(TRAY_ID).expect("托盘那一支在树里");
        let tray_children = settings.tray_root().map(|t| t.children.len()).unwrap_or(0);
        let moved = settings.items[0].id.clone();

        // 拖进托盘里
        assert!(settings.move_node(&[0], &DropTarget::Into(tray.clone())));
        assert!(
            settings.items.iter().any(|item| item.id == moved),
            "不该被塞进托盘：{moved}"
        );
        assert_eq!(
            settings.tray_root().map(|t| t.children.len()).unwrap_or(0),
            tray_children,
            "托盘里的东西不能变"
        );

        // 托盘本身也不能被拖走
        let tray = settings.find_path(TRAY_ID).expect("托盘还在");
        assert!(!settings.move_node(&tray, &DropTarget::After(vec![0])));
    }

    /// 多列符：JSON 里就是一条 `Separator` 加个标记，存得住、读得回。
    #[test]
    fn a_column_break_round_trips() {
        let mut settings = MenuSettings::default();
        let id = settings.add_column_break_in(&[], None);
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"column_break\":true"), "应该写进 JSON");
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        let node = back.find_path(&id).and_then(|path| back.node(&path)).unwrap();
        assert!(node.column_break);
        assert!(!node.is_plain_separator());
        // 普通分隔线不会被写成 column_break
        let plain = MenuSettings::default();
        assert!(!serde_json::to_string(&plain).unwrap().contains("column_break"));
    }

    /// 多列符切列；但它不该被「不显示分隔线」那个开关顺手藏掉，
    /// 也不该在开头 / 结尾 / 连续重复时留下一列空的。
    #[test]
    fn column_breaks_survive_the_separator_filter() {
        let mut settings = MenuSettings::default();
        settings.show_separators = false;
        settings.items = vec![
            ItemSetting {
                column_break: true,
                kind: ItemKind::Separator,
                ..ItemSetting::item("cb0", "")
            },
            ItemSetting::item("a", "A"),
            ItemSetting {
                kind: ItemKind::Separator,
                ..ItemSetting::item("sep", "")
            },
            ItemSetting {
                column_break: true,
                kind: ItemKind::Separator,
                ..ItemSetting::item("cb1", "")
            },
            ItemSetting::item("b", "B"),
            ItemSetting {
                column_break: true,
                kind: ItemKind::Separator,
                ..ItemSetting::item("cb2", "")
            },
        ];
        let menu = settings.menu_for(None);
        // 开头和结尾的多列符被收掉，普通分隔线按开关藏掉，中间那根留着
        assert_eq!(labels(&menu), vec!["A", "B"]);
        let breaks = menu
            .iter()
            .filter(|entry| matches!(entry, MenuEntry::ColumnBreak))
            .count();
        assert_eq!(breaks, 1);
    }

    /// 「不等结束符」现在只有**单项**一个开关（全局那个已经从设置页删掉了）。
    #[test]
    fn hotstring_phrases_carry_the_instant_flag() {
        let mut settings = MenuSettings::default();
        settings.items = vec![
            ItemSetting {
                hotstring: "j@".to_string(),
                hotstring_now: true,
                ..ItemSetting::item("mail", "写邮件")
            },
            ItemSetting {
                hotstring: "ftw".to_string(),
                hotstring_now: false,
                ..ItemSetting::item("ftw", "Free the whales")
            },
        ];
        let phrases = settings.hotstring_phrases();
        assert_eq!(phrases.len(), 2);
        assert!(phrases[0].now);
        assert_eq!(phrases[0].label, "写邮件");
        assert!(!phrases[1].now);
    }

    /// 新建的菜单项默认「不等结束符」是**开**的。
    #[test]
    fn new_items_default_to_no_end_char() {
        let item = ItemSetting::item("x", "随便");
        assert!(item.hotstring_now);
    }

    #[test]
    fn hotkeys_round_trip_and_default_to_empty() {
        let mut settings = MenuSettings::default();
        assert_eq!(settings.popup_hotkey, "Ctrl+Alt+M");
        assert!(settings.items.iter().all(|item| item.hotkey.is_empty()));
        settings.items[0].hotkey = "Ctrl+Alt+1".to_string();
        let json = serde_json::to_string(&settings).unwrap();
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items[0].hotkey, "Ctrl+Alt+1");
        assert_eq!(back.popup_hotkey, "Ctrl+Alt+M");
    }

    #[test]
    fn a_submenu_can_be_built_on_its_own() {
        let settings = MenuSettings::default();
        let menu: Vec<MenuEntry> = settings
            .submenu_entry_for("edit", None)
            .into_iter()
            .collect();
        assert_eq!(labels(&menu), vec!["编辑"]);
        assert!(menu[0].item().unwrap().has_children());
        // 不存在的 id / 普通项都不该给出一个「只有它」的菜单
        assert!(settings.submenu_entry_for("nope", None).is_none());
        assert!(settings.submenu_entry_for("new-file", None).is_none());
    }

    #[test]
    fn an_item_marked_never_is_dropped_even_if_its_parent_matches() {
        let mut settings = context_menu();
        settings.items[0].children[0].context_mode = ContextMode::Never;
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["搜索"]);
    }

    #[test]
    fn children_inherit_the_parents_kind_by_default() {
        // 子菜单标了「文字」，里面的项没标类型 → 默认跟它，摊平后照常出现
        let settings = context_menu();
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["翻译", "搜索"]);
    }

    #[test]
    fn filtering_never_leaves_stray_separators() {
        let settings = context_menu();
        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert!(!matches!(menu.first(), Some(MenuEntry::Separator)));
        assert!(!matches!(menu.last(), Some(MenuEntry::Separator)));
        // 中间那根是子菜单自己的分组线，要留着
        assert!(menu
            .iter()
            .any(|entry| matches!(entry, MenuEntry::Separator)));
    }

    #[test]
    fn contexts_survive_a_round_trip_and_unknown_keys_are_dropped() {
        let settings = context_menu();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"text\""), "类型要按 key 存：{json}");
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items[0].contexts, vec![SelectionKind::Text]);
        assert_eq!(back.items[3].contexts, Vec::new());

        // 手改错 / 未来版本的未知类型：只丢掉那一项，不能让整份配置读不出来
        let dirty = r#"{
            "items": [
                { "id": "a", "label": "A", "contexts": ["text", "hologram"] }
            ]
        }"#;
        let parsed: MenuSettings = serde_json::from_str(dirty).unwrap();
        assert_eq!(parsed.items[0].contexts, vec![SelectionKind::Text]);

        // 老配置里的文件细分类型（video / audio / …）现在都按「文件」读，
        // 而且重复的会被去掉
        let fresh = r#"{
            "items": [ { "id": "a", "label": "A", "contexts": ["video", "audio", "folder"] } ]
        }"#;
        let parsed: MenuSettings = serde_json::from_str(fresh).unwrap();
        assert_eq!(
            parsed.items[0].contexts,
            vec![SelectionKind::File, SelectionKind::Folder]
        );
    }

    #[test]
    fn disabled_items_and_separators_can_be_hidden() {
        let mut settings = MenuSettings::default();
        settings.items[0].enabled = false;
        assert!(settings.menu_for(None).iter().all(|entry| entry
            .item()
            .map(|item| item.label != "新建文件")
            .unwrap_or(true)));

        settings.show_separators = false;
        assert!(settings
            .menu_for(None)
            .iter()
            .all(|entry| !matches!(entry, MenuEntry::Separator)));
    }

    #[test]
    fn neighbours_never_are_two_separators() {
        let mut settings = MenuSettings::default();
        settings.items[0].enabled = false;
        let menu = settings.menu_for(None);
        let mut last_was_separator = false;
        for entry in &menu {
            let is_separator = matches!(entry, MenuEntry::Separator);
            assert!(!(is_separator && last_was_separator), "出现了连续分隔线");
            last_was_separator = is_separator;
        }
    }

    #[test]
    fn tree_paths_find_and_move_nodes() {
        let mut settings = MenuSettings::default();
        let edit = settings.find_path("edit").expect("找得到编辑");
        assert_eq!(&settings.items[edit[0]].label, "编辑");
        let copy = settings.find_path("copy").expect("找得到复制");
        assert_eq!(copy.len(), 2);

        // 拖拽：把「编辑」挪到根层最前面，子项要跟着走
        assert!(settings.move_node(&edit, &DropTarget::Before(vec![0])));
        assert_eq!(settings.items[0].label, "编辑");
        let mut ids = Vec::new();
        collect_ids(&settings.items, &mut ids);
        assert!(ids.contains(&"copy".to_string()), "子项不能丢");
    }

    #[test]
    fn add_child_creates_a_submenu() {
        let mut settings = MenuSettings::default();
        let quit = settings.find_path("quit").expect("找得到退出");
        assert!(!settings.items[quit[0]].has_children());
        settings.add_child(&quit);
        assert!(settings.items[quit[0]].has_children());
    }

    #[test]
    fn removing_a_node_keeps_the_rest_intact() {
        let mut settings = MenuSettings::default();
        let before = settings.items.len();
        let copy = settings.find_path("copy").expect("找得到复制");
        settings.remove_at(&copy);
        assert!(settings.find_path("copy").is_none());
        assert_eq!(settings.items.len(), before, "删子项不该动根层");
    }

    #[test]
    fn font_family_survives_a_round_trip() {
        let mut settings = MenuSettings::default();
        assert_eq!(settings.font_family, "");
        assert!(!available_fonts().is_empty(), "字体列表不能是空的");
        settings.font_family = "SimSun".to_string();
        let json = serde_json::to_string(&settings).unwrap();
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.font_family, "SimSun");
        assert_eq!(back, settings);
    }

    #[test]
    fn new_nodes_are_inserted_after_the_selected_one() {
        let mut settings = MenuSettings::default();
        // 选中根层第 0 项 → 新项应该插在它后面（而不是末尾）
        let first = vec![0];
        let first_label = settings.items[0].label.clone();
        let id = settings.add_item(Some(&first));
        assert_eq!(settings.items[1].id, id);
        assert_eq!(settings.items[0].label, first_label);

        // 选中子项 → 新项插进同一个子菜单里
        let copy = settings.find_path("copy").expect("找得到复制");
        let parent_len = settings.node(&copy[..1]).unwrap().children.len();
        let child_id = settings.add_item(Some(&copy));
        let parent = settings.find_path("edit").unwrap();
        assert_eq!(settings.items[parent[0]].children.len(), parent_len + 1);
        assert_eq!(settings.items[parent[0]].children[1].id, child_id);

        // 没选中 → 加到末尾
        let before = settings.items.len();
        settings.add_separator(None);
        assert_eq!(settings.items.len(), before + 1);
    }

    #[test]
    fn separators_have_unique_ids() {
        // 分隔线的 id 必须互不相同：设置界面按 id 找路径，
        // 重名会让「选中一条 = 点亮一排」，新项也会插到错的位置。
        let settings = MenuSettings::default();
        let mut ids = Vec::new();
        collect_ids(&settings.items, &mut ids);
        let separators = settings
            .items
            .iter()
            .filter(|item| item.kind == ItemKind::Separator)
            .count();
        assert!(separators >= 2, "默认菜单里本来就有好几条分隔线");
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "id 有重复：{ids:?}");
    }

    #[test]
    fn a_new_item_lands_right_after_the_selected_separator() {
        // 选中第 2 条分隔线（不是第一条）→ 新项必须插在它后面
        let mut settings = MenuSettings::default();
        let separators: Vec<usize> = settings
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.kind == ItemKind::Separator)
            .map(|(index, _)| index)
            .collect();
        assert!(separators.len() >= 2);
        let (first, second) = (separators[0], separators[1]);
        let first_next = settings.items[first + 1].id.clone();
        let id = settings.add_item(Some(&[second]));
        assert_eq!(settings.items[second + 1].id, id);
        assert_eq!(
            settings.items[first + 1].id,
            first_next,
            "第一条分隔线后面不该被动到"
        );
    }

    #[test]
    fn duplicate_ids_are_repaired_on_load() {
        // 老配置里所有分隔线都叫 "sep" —— 读进来要能自己修好
        let mut settings = MenuSettings::default();
        settings.ensure_unique_ids();
        let mut raw = serde_json::to_value(&settings).unwrap();
        let items = raw["items"].as_array_mut().unwrap();
        let mut renamed = 0;
        for item in items.iter_mut() {
            if item["kind"] == serde_json::json!("Separator") {
                item["id"] = serde_json::json!("sep");
                renamed += 1;
            }
        }
        assert!(renamed >= 2);
        let mut back: MenuSettings = serde_json::from_value(raw).unwrap();
        back.migrate();
        let mut ids = Vec::new();
        collect_ids(&back.items, &mut ids);
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "迁移后仍重名：{ids:?}");
    }

    #[test]
    fn dropping_into_a_separator_means_dropping_after_it() {
        let mut settings = MenuSettings::default();
        let separator = settings
            .items
            .iter()
            .position(|item| item.kind == ItemKind::Separator)
            .expect("默认菜单有分隔线");
        let quit = settings.find_path("quit").expect("找得到退出");
        // 「拖进分隔线里」不能把这一项吞掉，应该落成「插在它后面」
        assert!(settings.move_node(&quit, &DropTarget::Into(vec![separator])));
        let mut ids = Vec::new();
        collect_ids(&settings.items, &mut ids);
        assert!(ids.contains(&"quit".to_string()), "不能把拖的项弄丢");
        assert_eq!(settings.items[separator + 1].id, "quit");
        assert!(settings.items[separator].children.is_empty());
    }

    #[test]
    fn per_item_visibility_overrides_the_global_toggle() {
        let mut settings = MenuSettings::default();
        settings.show_badges = true;
        settings.show_shortcuts = true;
        // 「新建文件」默认带图标位 + 快捷键
        let entry = settings.menu_for(None);
        let first = entry[0].item().unwrap();
        assert!(first.badge.is_some());
        assert!(first.shortcut.is_some());

        // 单项关掉图标位、但全局还开着
        settings.items[0].show_badges = Some(false);
        let entry = settings.menu_for(None);
        let first = entry[0].item().unwrap();
        assert!(first.badge.is_none(), "单项设置要盖过全局");

        // 全局关了，单项打开
        settings.show_shortcuts = false;
        settings.items[1].show_shortcuts = Some(true);
        let entry = settings.menu_for(None);
        assert!(entry[1].item().unwrap().shortcut.is_some());
        assert!(entry[0].item().unwrap().shortcut.is_none());
    }

    #[test]
    fn action_values_survive_a_round_trip() {
        let settings = MenuSettings::default();
        let json = serde_json::to_string(&settings).unwrap();
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        let new_file = back
            .items
            .iter()
            .find(|item| item.id == "new-file")
            .unwrap();
        assert_eq!(new_file.actions[0].kind, ActionKind::Run);
        assert_eq!(new_file.actions[0].value, "notepad.exe");

        // 多条动作 + 间隔也要原样回来
        let signature = back
            .items
            .iter()
            .find(|item| item.id == "signature")
            .unwrap();
        assert_eq!(signature.actions.len(), 2);
        assert_eq!(signature.actions[0].delay_ms, 150);
        assert_eq!(signature.actions[1].kind, ActionKind::Keys);
    }

    #[test]
    fn old_single_action_json_migrates_into_the_list() {
        let json = r#"{
            "items": [
                { "id": "old", "label": "老的", "action": { "kind": "Command", "value": "notepad.exe" } }
            ]
        }"#;
        let mut settings: MenuSettings = serde_json::from_str(json).unwrap();
        settings.migrate();
        let old = &settings.items[0];
        assert_eq!(old.actions.len(), 1);
        assert_eq!(old.actions[0].kind, ActionKind::Run);
        assert_eq!(old.actions[0].value, "notepad.exe");
    }

    #[test]
    fn having_children_turns_a_row_into_a_submenu_and_keeps_its_actions() {
        let mut settings = MenuSettings::default();
        let quit = settings.find_path("quit").unwrap();
        assert_eq!(settings.items[quit[0]].kind, ItemKind::Item);
        let actions = settings.items[quit[0]].actions.len();
        assert!(actions > 0);

        settings.add_child(&quit);
        let quit = settings.find_path("quit").unwrap();
        assert_eq!(settings.items[quit[0]].kind, ItemKind::Submenu);
        // 动作留着（只是不执行）：用户把类型点来点去，或者先填动作再改成子菜单，
        // 切回普通项时东西还得在
        assert_eq!(settings.items[quit[0]].actions.len(), actions);
        // 但子菜单真的不执行动作：菜单里这一项是「展开」而不是「跑动作」
        let menu = settings.menu_for(None);
        assert!(labels(&menu).contains(&"退出".to_string()));
    }

    #[test]
    fn dragging_a_row_into_another_row_nests_it() {
        let mut settings = MenuSettings::default();
        let quit = settings.find_path("quit").expect("找得到退出");
        let settings_path = settings.find_path("settings").expect("找得到打开设置");
        // 把「退出」拖进「打开设置」里
        assert!(settings.move_node(&quit, &DropTarget::Into(settings_path.clone())));
        let parent = settings.find_path("settings").unwrap();
        assert_eq!(settings.items[parent[0]].kind, ItemKind::Submenu);
        assert!(
            settings.items[parent[0]]
                .children
                .iter()
                .any(|child| child.id == "quit"),
            "退出应该成为它的子项"
        );
        assert!(settings.find_path("quit").unwrap().len() == 2);
    }

    #[test]
    fn dragging_the_last_child_out_demotes_the_submenu() {
        let mut settings = MenuSettings::default();
        let recent = settings.find_path("recent").unwrap();
        let count = settings.items[recent[0]].children.len();
        let edit = settings.find_path("edit").unwrap();
        // 把「最近打开」的子项一个个搬进「编辑」里
        for _ in 0..count {
            let child = vec![recent[0], 0];
            assert!(settings.move_node(&child, &DropTarget::Into(edit.clone())));
        }
        let recent = settings.find_path("recent").unwrap();
        assert_eq!(
            settings.items[recent[0]].kind,
            ItemKind::Item,
            "子项搬空了就该退回普通项"
        );
    }

    #[test]
    fn a_node_cannot_be_dropped_into_itself() {
        let mut settings = MenuSettings::default();
        let edit = settings.find_path("edit").unwrap();
        let copy = settings.find_path("copy").unwrap();
        assert!(!settings.move_node(&edit, &DropTarget::Into(copy.clone())));
        assert!(!settings.move_node(&edit, &DropTarget::Into(edit.clone())));
    }

    #[test]
    fn demoting_a_submenu_parks_its_children_instead_of_losing_them() {
        // 用户报过的坑：把普通项变成子菜单后，没有路走回普通项。
        // 现在两个方向都能来回切，而且**切的时候不丢东西** —— 误点一下不会
        // 把攒了半天的子项拆散。
        let mut settings = MenuSettings::default();
        let quit = settings.find_path("quit").unwrap();
        settings.add_child(&quit);
        let quit = settings.find_path("quit").unwrap();
        assert_eq!(settings.items[quit[0]].kind, ItemKind::Submenu);
        let child_id = settings.items[quit[0]].children[0].id.clone();
        let actions_before = settings.items[quit[0]].actions.len();

        assert!(settings.demote_submenu(&quit));
        let quit = settings.find_path("quit").unwrap();
        assert_eq!(settings.items[quit[0]].kind, ItemKind::Item);
        assert!(settings.items[quit[0]].children.is_empty(), "子项不该留在树里");
        assert_eq!(
            settings.items[quit[0]].parked_children.len(),
            1,
            "子项进了抽屉，没丢"
        );
        assert_eq!(
            settings.items[quit[0]].actions.len(),
            actions_before,
            "动作也不该被清掉"
        );
        // 子项真的不在树里了（不会被当成同级菜单项显示出来）
        assert!(settings.find_path(&child_id).is_none());

        // 再点回「添加子菜单」：抽屉里的东西原样回来，id 都不变
        let quit = settings.find_path("quit").unwrap();
        assert!(settings.promote_to_submenu(&quit));
        let quit = settings.find_path("quit").unwrap();
        assert_eq!(settings.items[quit[0]].kind, ItemKind::Submenu);
        assert_eq!(settings.find_path(&child_id), Some(vec![quit[0], 0]));
        assert!(settings.items[quit[0]].parked_children.is_empty());
    }

    #[test]
    fn demoting_an_item_without_children_does_nothing() {
        let mut settings = MenuSettings::default();
        let quit = settings.find_path("quit").unwrap();
        // 本来就是普通项：没有可收的子项，直接说「没做事」
        assert!(!settings.demote_submenu(&quit));
        let quit = settings.find_path("quit").unwrap();
        assert_eq!(settings.items[quit[0]].kind, ItemKind::Item);
        // 动作不能被顺手清掉
        assert!(!settings.items[quit[0]].actions.is_empty());
        assert!(settings.items[quit[0]].children.is_empty());
    }

    #[test]
    fn promoting_a_plain_item_to_a_submenu_refills_the_parked_children() {
        let mut settings = MenuSettings::default();
        let quit = settings.find_path("quit").unwrap();
        assert!(settings.promote_to_submenu(&quit));
        let quit = settings.find_path("quit").unwrap();
        assert_eq!(settings.items[quit[0]].kind, ItemKind::Submenu);
        // 没有抽屉内容时**就让它空着**（不再塞「新建子项」占位）：
        // 空子菜单在菜单里是隐藏的，界面会给「还没有菜单项」的提示
        assert!(settings.items[quit[0]].children.is_empty());
        // 空子菜单仍然是子菜单 —— 以前 normalize 会把它降级成普通项
        assert_eq!(settings.items[quit[0]].kind, ItemKind::Submenu);
        // 动作照样留着
        assert!(!settings.items[quit[0]].actions.is_empty());
    }

    #[test]
    fn only_run_actions_or_selection_placeholders_need_a_capture() {
        // 全局「接上选中内容」默认是开的，但它对「发送按键」没有任何意义：
        // 抓一次要发 Ctrl+C + 等半秒剪贴板，长按快捷键时就是灾难
        let keys = ItemSetting::item("k", "音量+").action(ActionKind::Keys, "Volume_Up");
        assert!(!needs_selection(&keys, true));
        assert!(!needs_selection(&keys, false));

        // 运行程序：开关开着就要抓（要把选中文件的路径拼到命令行后面）
        let run = ItemSetting::item("r", "记事本").action(ActionKind::Run, "notepad.exe");
        assert!(needs_selection(&run, true));
        // 单项关掉了就不抓
        let mut run_off = run.clone();
        run_off.append_selection = Some(false);
        assert!(!needs_selection(&run_off, true));

        // 写了 {selection} 的文本动作：必须抓
        let text = ItemSetting::item("t", "发出去").action(ActionKind::Text, "选中了：{selection}");
        assert!(needs_selection(&text, false));
    }

    #[test]
    fn resetting_restores_the_bundled_default_menu() {
        // 「还原默认菜单」要回到打包的那套（assets/defaults/menu.runa），
        // 不是代码里写死的示例菜单 —— 后者只有 4 个顶层项
        let mut settings = MenuSettings::default();
        settings.reset_items();
        let labels: Vec<&str> = settings.items.iter().map(|item| item.label.as_str()).collect();
        assert!(
            labels.contains(&"常用"),
            "还原后没回到打包的默认菜单，实际顶层：{labels:?}"
        );
    }

    #[test]
    fn selection_settings_round_trip() {
        let mut settings = MenuSettings::default();
        assert!(settings.selection_enabled, "默认开启抓取选中内容");
        assert_eq!(settings.selection_copy_keys, "Ctrl+C");
        assert_eq!(settings.selection_wait_ms, 500);
        settings.selection_enabled = false;
        settings.selection_copy_keys = "Ctrl+Insert".to_string();
        settings.selection_wait_ms = 1200;
        let json = serde_json::to_string(&settings).unwrap();
        let back: MenuSettings = serde_json::from_str(&json).unwrap();
        assert!(!back.selection_enabled);
        assert_eq!(back.selection_copy_keys, "Ctrl+Insert");
        assert_eq!(back.selection_wait_ms, 1200);
    }

    #[test]
    fn old_config_without_selection_fields_gets_the_defaults() {
        // 老 settings.json 里没有这几个字段：读进来必须补默认值，不能报错
        let json = r#"{ "items": [ { "id": "a", "label": "A" } ] }"#;
        let settings: MenuSettings = serde_json::from_str(json).unwrap();
        assert!(settings.selection_enabled);
        assert_eq!(settings.selection_copy_keys, "Ctrl+C");
        assert_eq!(settings.selection_wait_ms, 500, "复制后等待默认 500ms");
        // 容器级 #[serde(default)] 拿的是 MenuSettings::default()，不是 bool 的 false
        assert!(settings.context_filter, "按选中内容筛选默认开着");
        assert!(settings.items[0].contexts.is_empty(), "老项都是通用项");
    }

    // ---- 层级相关的回归用例（用户报过：「二级配筛选，摊了但父项还露着」）----

    /// 一级只标类型（继承）、二级才转成 Own：照样按二级摊到叶子。
    #[test]
    fn filter_only_on_the_second_level_flattens_to_its_children() {
        let mut b = ItemSetting::item("b", "翻译").children(vec![ItemSetting::item("b1", "中→英")]);
        b.contexts = vec![SelectionKind::Text];
        b.context_mode = ContextMode::Own;
        let mut a = ItemSetting::item("a", "文字").children(vec![b]);
        a.contexts = vec![SelectionKind::Text]; // 继承：没有转成 Own
        let mut settings = MenuSettings::default();
        settings.items = vec![a];

        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["中→英"]);
    }

    /// 三层都配了筛选：一路摊到最里层，中间的壳一个都不留。
    #[test]
    fn three_configured_levels_flatten_to_the_innermost() {
        let mut c = ItemSetting::item("c", "中英").children(vec![ItemSetting::item("c1", "中→英")]);
        c.contexts = vec![SelectionKind::Text];
        c.context_mode = ContextMode::Own;
        let mut b = ItemSetting::item("b", "翻译").children(vec![c]);
        b.contexts = vec![SelectionKind::Text];
        b.context_mode = ContextMode::Own;
        let mut a = ItemSetting::item("a", "文字").children(vec![b]);
        a.contexts = vec![SelectionKind::Text];
        a.context_mode = ContextMode::Own;
        let mut settings = MenuSettings::default();
        settings.items = vec![a];

        let menu = settings.menu_for(Some(&ctx(&[SelectionKind::Text])));
        assert_eq!(labels(&menu), vec!["中→英"], "「文字」「翻译」「中英」三层壳都该化掉");
    }
}
