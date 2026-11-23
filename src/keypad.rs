//! 按键选择面板：点着键盘往输入框里填快捷键。
//!
//! ## 为什么不是「按一下真实组合键就录进去」
//!
//! 原来那种录入只有一个全局键盘拦截器，能拿到的信息是 gpui 的 `Keystroke`：
//! 它**没有 Win 键**（按下去系统先拿去开开始菜单了），左右修饰键也**合成了一个**
//! `Ctrl` / `Shift` / `Alt`。于是「Win+E」「左 Ctrl+Shift+1」这类就永远录不进去。
//!
//! 改成在窗口里画一张键盘：每个键点一下就点亮，修饰键可以点两颗（左右分开），
//! 最后点「完成」写进输入框。信息全在手里，不再受按键派发的限制。
//!
//! ## 现在是一扇自己的窗
//!
//! 以前这张键盘画在设置窗口里面（浮层），窗口只有 900px 宽，键盘挤得像玩具，
//! 字小到看不清。现在它是**独立的一扇可缩放窗口**：拉大窗口、按「＋」放大键帽，
//! 都能让字变大，不再受宿主窗口的尺寸限制。
//!
//! ## 小键盘按 NumLock 分两套名字
//!
//! 和 AHK 一样：NumLock 开着时 `Num7` 是「7」，关着时同一个物理键是 `Home`。
//! 面板上那段键会按当前 NumLock 状态显示主 / 副标签，点 NUM 那颗就是真的拨一下
//! 系统的 NumLock（和小键盘上那颗键完全一样）。
//!
//! ## 写进设置的写法
//!
//! | 类别 | 写法 | 例子 |
//! | --- | --- | --- |
//! | 修饰键 | `Ctrl` `Alt` `Shift` `Win`，左右分开就加前缀 | `LCtrl` `RShift` `LWin` |
//! | 字母 / 数字 / 符号 | 原样 | `A` `7` `-` `[` |
//! | 功能键 | 和 AHK 一样的名字 | `F13` `PgUp` `PrintScreen` |
//! | 小键盘 | `Num` 开头；NumLock 关着时是导航键名 | `Num7` `NumAdd` `Home` |
//! | 鼠标 | AHK 的叫法 | `LButton` `MButton` `XButton1` `WheelUp` |
//!
//! ## 「区分左右修饰键」跟着**这一条快捷键**走
//!
//! 面板右上角有个开关，拨的是**正在编辑的那一条**的设置（不是全局的）：
//!
//! - 开着：写进去的是 `LCtrl` / `RAlt` 这种，按错边不触发；
//! - 关掉：写进去的是 `Ctrl` / `Alt`，左右哪颗按下去都算。
//!
//! 键盘**始终画全两套**（左右各一颗，键帽上直接把 `LCTRL` / `RCTRL` 写出来）——
//! 以前是「关掉开关右边那几颗就消失」，面板跟着变来变去，看不清自己到底点了啥。
//! 现在布局不动，只有「写出来的写法」和「亮的范围」跟着开关变：关掉时点哪一颗
//! 都记成 `Ctrl`，两颗一起亮。

use std::sync::OnceLock;

use gpui::prelude::*;
use gpui::{
    div, px, size, AnyElement, AnyWindowHandle, App, AppContext, Bounds, ClickEvent, Context,
    Entity, Global, IntoElement, SharedString, Window, WindowBackgroundAppearance, WindowBounds,
    WindowOptions,
};

use crate::text_input::TextInput;
use crate::theme;

/// 一个字母键的边长
const U: f32 = 30.0;
/// 键与键之间的缝
const GAP: f32 = 3.0;
/// 卡片内边距
const PAD: f32 = 12.0;
/// 键帽字号
const KEY_FONT: f32 = 10.5;

const WIDTH: f32 = 960.0;
const HEIGHT: f32 = 500.0;
const MIN_WIDTH: f32 = 700.0;
const MIN_HEIGHT: f32 = 390.0;

// ---------------------------------------------------------------------------
// 键位
// ---------------------------------------------------------------------------

/// 键盘上的一个键。
#[derive(Clone, Debug)]
pub struct Key {
    /// 键帽上写的字
    label: String,
    /// 写进设置里的写法（`LCtrl` / `F13` / `NumAdd`）
    token: String,
    /// 宽度，单位是「一个字母键」
    w: f32,
    /// 是修饰键：点一下是「加上 / 去掉」，不当主键
    modifier: bool,
    /// 小键盘那种「一个键两套名字」：NumLock 关着时显示的标签 / 写法
    alt: Option<(String, String)>,
}

fn k(label: &str, token: &str) -> Key {
    Key {
        label: label.to_string(),
        token: token.to_string(),
        w: 1.0,
        modifier: false,
        alt: None,
    }
}

fn kw(label: &str, token: &str, w: f32) -> Key {
    Key { w, ..k(label, token) }
}

/// 空位（键盘上那种没有键的格子，用来对齐）。
fn sp(w: f32) -> Key {
    Key {
        label: String::new(),
        token: String::new(),
        w,
        modifier: false,
        alt: None,
    }
}

/// NumLock 关着时同一个物理键是另一颗键（`Num7` → `Home`）。
fn num(label_on: &str, token_on: &str, label_off: &str, token_off: &str) -> Key {
    Key {
        alt: Some((label_off.to_string(), token_off.to_string())),
        ..k(label_on, token_on)
    }
}

/// 修饰键：常驻在「按住不放」那一侧，点一下加进来、再点一下去掉。
fn m(label: &str, token: &str, w: f32) -> Key {
    Key {
        modifier: true,
        ..kw(label, token, w)
    }
}

/// 一块键盘（主键区 / 导航区 / 小键盘各一块，纵向对齐）。
struct Block {
    rows: Vec<Vec<Key>>,
    /// 网格化：这一块里所有键同宽（导航区 / 小键盘必须这样，否则
    /// 「PRTSC」和「INS」宽窄不一，四列全歪——看起来就是「布局凌乱」）
    uniform: bool,
}

fn layout() -> &'static Vec<Block> {
    static LAYOUT: OnceLock<Vec<Block>> = OnceLock::new();
    LAYOUT.get_or_init(build)
}

fn build() -> Vec<Block> {
    vec![
        Block {
            rows: main_rows(),
            uniform: false,
        },
        Block {
            rows: nav_rows(),
            uniform: true,
        },
        Block {
            rows: numpad_rows(),
            uniform: true,
        },
    ]
}

/// 网格块里一格的宽度：这块里最宽的那个键（标签长的也能塞下）。
fn cell_width(block: &Block, zoom: f32) -> f32 {
    let widest = block
        .rows
        .iter()
        .flatten()
        // 空格位不算：`sp(4.0)` 是「占四格」，不是「一个字有四格宽」
        .filter(|item| !item.token.is_empty())
        .map(|item| key_width(item, zoom))
        .fold(0.0_f32, f32::max);
    // 兜底：一块里全是空格位时也得有个格子宽
    if widest <= 0.0 {
        U * zoom
    } else {
        widest
    }
}

/// 这个键实际占多宽（网格块里所有键对齐到同一格）。
fn width_in(block: &Block, item: &Key, zoom: f32) -> f32 {
    if block.uniform {
        item.w * cell_width(block, zoom)
    } else {
        key_width(item, zoom)
    }
}

/// 主键区：7 行（两排 F 键 + 5 行），宽度都以最下面那排为准。
///
/// 左右修饰键**一直画全**，键帽上直接写 `LCTRL` / `RCTRL` —— 位置加名字，
/// 一眼就知道点的是哪一颗。右上角那个「区分左右」只影响**写出来的写法**
/// （`LCtrl` 还是 `Ctrl`）和「亮几颗」，不改布局。
fn main_rows() -> Vec<Vec<Key>> {
    let mut rows: Vec<Vec<Key>> = Vec::new();

    // F 排（ESC + F1–F12）
    let mut row = vec![kw("ESC", "Esc", 1.2)];
    row.extend((1..=12).map(|n| k(&format!("F{n}"), &format!("F{n}"))));
    rows.push(row);

    // F13–F24：第二排，对齐在 F1 下面（ESC 那格留空）
    let mut row = vec![sp(1.2)];
    row.extend((13..=24).map(|n| k(&format!("F{n}"), &format!("F{n}"))));
    rows.push(row);

    // 数字行
    let mut row = vec![k("`", "`")];
    for n in 1..=9 {
        row.push(k(&n.to_string(), &n.to_string()));
    }
    row.push(k("0", "0"));
    row.push(k("-", "-"));
    row.push(k("=", "="));
    row.push(kw("BSPC", "Backspace", 1.9));
    rows.push(row);

    // QWERTY
    let mut row = vec![kw("TAB", "Tab", 1.5)];
    row.extend("QWERTYUIOP".chars().map(|c| k(&c.to_string(), &c.to_string())));
    row.push(k("[", "["));
    row.push(k("]", "]"));
    row.push(kw("\\", "\\", 1.5));
    rows.push(row);

    // ASDF
    let mut row = vec![kw("CAPS", "CapsLock", 1.7)];
    row.extend("ASDFGHJKL".chars().map(|c| k(&c.to_string(), &c.to_string())));
    row.push(k(";", ";"));
    row.push(k("'", "'"));
    row.push(kw("ENTER", "Enter", 2.1));
    rows.push(row);

    // ZXCV
    let mut row = vec![m("LSHIFT", "LShift", 2.3)];
    row.extend("ZXCVBNM".chars().map(|c| k(&c.to_string(), &c.to_string())));
    row.push(k(",", ","));
    row.push(k(".", "."));
    row.push(k("/", "/"));
    row.push(m("RSHIFT", "RShift", 2.3));
    rows.push(row);

    // 最下面那排修饰键
    rows.push(vec![
        m("LCTRL", "LCtrl", 1.6),
        m("LWIN", "LWin", 1.3),
        m("LALT", "LAlt", 1.3),
        kw("SPACE", "Space", 4.2),
        m("RALT", "RAlt", 1.3),
        m("RWIN", "RWin", 1.3),
        kw("MENU", "Menu", 1.3),
        m("RCTRL", "RCtrl", 1.6),
    ]);

    rows
}

/// 导航区 + 方向键（3 列，和主键区一样 6 行高）。
fn nav_rows() -> Vec<Vec<Key>> {
    vec![
        vec![k("PrtSc", "PrintScreen"), k("Scr", "ScrollLock"), k("Pause", "Pause")],
        vec![k("Ins", "Insert"), k("Home", "Home"), k("PgUp", "PageUp")],
        vec![k("Del", "Delete"), k("End", "End"), k("PgDn", "PageDown")],
        // 方向键（倒 T 字）
        vec![sp(1.0), k("↑", "Up"), sp(1.0)],
        vec![k("←", "Left"), k("↓", "Down"), k("→", "Right")],
        vec![sp(3.0)],
    ]
}

/// 小键盘（4 列，6 行；和主键区一样高）。
fn numpad_rows() -> Vec<Vec<Key>> {
    vec![
        vec![sp(4.0)],
        vec![
            k("Num", "NumLock"),
            k("/", "NumDiv"),
            k("*", "NumMul"),
            k("-", "NumSub"),
        ],
        vec![
            num("7", "Num7", "Home", "Home"),
            num("8", "Num8", "↑", "Up"),
            num("9", "Num9", "PgUp", "PageUp"),
            k("+", "NumAdd"),
        ],
        vec![
            num("4", "Num4", "←", "Left"),
            num("5", "Num5", "Clear", "Clear"),
            num("6", "Num6", "→", "Right"),
            k("Ent", "NumEnter"),
        ],
        vec![
            num("1", "Num1", "End", "End"),
            num("2", "Num2", "↓", "Down"),
            num("3", "Num3", "PgDn", "PageDown"),
            sp(1.0),
        ],
        vec![num("0", "Num0", "Ins", "Insert"), num(".", "NumDot", "Del", "Delete"), sp(2.0)],
    ]
}

/// 鼠标键：一排单独的按钮（不当修饰键，只当主键）。
fn mouse_keys() -> Vec<Key> {
    vec![
        kw("左键", "LButton", 1.4),
        kw("右键", "RButton", 1.4),
        kw("中键", "MButton", 1.4),
        kw("侧键1", "XButton1", 1.5),
        kw("侧键2", "XButton2", 1.5),
        kw("滚轮上", "WheelUp", 1.6),
        kw("滚轮下", "WheelDown", 1.8),
    ]
}

/// 鼠标那排（只要一份，找键帽文字时要一起查）。
fn mouse_layout() -> &'static Vec<Key> {
    static MOUSE: OnceLock<Vec<Key>> = OnceLock::new();
    MOUSE.get_or_init(mouse_keys)
}

/// 所有的键（找键帽文字 / 反查写法用）。
fn all_keys() -> impl Iterator<Item = &'static Key> {
    layout()
        .iter()
        .flat_map(|block| block.rows.iter())
        .flatten()
        .chain(mouse_layout().iter())
}

/// `LCtrl` → `Ctrl`（关掉「区分左右」时写这个）。
///
/// 只认四族修饰键：`LButton` / `RAlt` 之外的东西一个字都不能动 ——
/// 鼠标左键叫 `LButton`，把开头的 L 砍掉就成了另一颗键。
fn generic(token: &str) -> &str {
    let mut chars = token.chars();
    if matches!(chars.next(), Some('L') | Some('R')) {
        let rest = &token[1..];
        if matches!(rest, "Ctrl" | "Shift" | "Alt" | "Win") {
            return rest;
        }
    }
    token
}

/// 一个键实际要多宽：`w` 个字母宽，但窄键塞不下 `SHIFT` / `PRTSC` 这种长标签时
/// 按字数放宽（否则字会挤到键外面去，或者折成两行）。
fn key_width(item: &Key, zoom: f32) -> f32 {
    let font = KEY_FONT * zoom;
    let chars = item
        .label
        .chars()
        .count()
        .max(item.alt.as_ref().map(|(label, _)| label.chars().count()).unwrap_or(0));
    // 0.68 是这套字体的「平均字宽 / 字号」实测值，再留 10px 边距：
    // 宁可宽一点点，也不要让 `MENU` 折成 `MEN` + `U` 两行
    let text = chars as f32 * (font * 0.68);
    (item.w * U * zoom).max(text + 10.0)
}

fn label_of(token: &str) -> String {
    for key in all_keys() {
        if key.token == token {
            return key.label.clone();
        }
        if let Some((label, alt_token)) = &key.alt {
            if alt_token == token {
                return label.clone();
            }
        }
    }
    token.to_string()
}

// ---------------------------------------------------------------------------
// 写法 ⇄ 键
// ---------------------------------------------------------------------------

/// 修饰键的别名 → 标准写法。
fn modifier_token(part: &str) -> Option<&'static str> {
    let token = match part.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => "Ctrl",
        "lctrl" | "lcontrol" | "leftctrl" | "leftcontrol" => "LCtrl",
        "rctrl" | "rcontrol" | "rightctrl" | "rightcontrol" => "RCtrl",
        "alt" => "Alt",
        "lalt" | "leftalt" => "LAlt",
        "ralt" | "rightalt" => "RAlt",
        "shift" => "Shift",
        "lshift" | "leftshift" => "LShift",
        "rshift" | "rightshift" => "RShift",
        "win" | "super" | "cmd" | "meta" | "windows" => "Win",
        "lwin" | "leftwin" => "LWin",
        "rwin" | "rightwin" => "RWin",
        _ => return None,
    };
    Some(token)
}

/// 主键的别名（别处写过的叫法都认）→ 键位表里的写法。
fn main_token(part: &str) -> Option<String> {
    static ALIASES: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();
    let table = ALIASES.get_or_init(|| {
        let mut map = std::collections::HashMap::new();
        for key in all_keys() {
            if !key.token.is_empty() {
                map.insert(key.token.to_ascii_lowercase(), key.token.clone());
            }
            if let Some((_, alt)) = &key.alt {
                map.insert(alt.to_ascii_lowercase(), alt.clone());
            }
        }
        let alias = [
            ("escape", "Esc"),
            ("pageup", "PageUp"),
            ("pagedown", "PageDown"),
            ("printscreen", "PrintScreen"),
            ("prtsc", "PrintScreen"),
            ("del", "Delete"),
            ("ins", "Insert"),
            ("return", "Enter"),
            ("back", "Backspace"),
            ("capslock", "CapsLock"),
            ("numlock", "NumLock"),
            ("scrolllock", "ScrollLock"),
            ("clear", "Clear"),
            ("numpadenter", "NumEnter"),
            ("numadd", "NumAdd"),
            ("numpadadd", "NumAdd"),
            ("numplus", "NumAdd"),
            ("numsub", "NumSub"),
            ("numpadsubtract", "NumSub"),
            ("nummul", "NumMul"),
            ("numpadmultiply", "NumMul"),
            ("numdiv", "NumDiv"),
            ("numpaddivide", "NumDiv"),
            ("numpaddecimal", "NumDot"),
            ("numdecimal", "NumDot"),
            ("numpadclear", "Clear"),
            // 鼠标
            ("leftbutton", "LButton"),
            ("rightbutton", "RButton"),
            ("middlebutton", "MButton"),
            ("x1button", "XButton1"),
            ("x2button", "XButton2"),
            ("wheelu", "WheelUp"),
            ("wheeld", "WheelDown"),
        ];
        for (from, to) in alias {
            map.insert(from.to_string(), to.to_string());
        }
        for n in 0..=9 {
            map.insert(format!("numpad{n}"), format!("Num{n}"));
            map.insert(format!("num{n}"), format!("Num{n}"));
        }
        map
    });
    table.get(&part.trim().to_ascii_lowercase()).cloned()
}

/// 把输入框里现有的写法拆成「修饰键 + 主键」（拆不出来就留空）。
///
/// 多组（`Ctrl+A Delete` 这种序列）不猜：面板只管一组，剩下的用户自己接着点。
fn seed(text: &str) -> (Vec<String>, Vec<String>) {
    let mut combos = text.split_whitespace();
    let Some(first) = combos.next() else {
        return (Vec::new(), Vec::new());
    };
    if combos.next().is_some() {
        return (Vec::new(), Vec::new());
    }
    let mut mods: Vec<String> = Vec::new();
    // 主键可以有好几颗：`CapsLock+1`、`LButton+WheelUp` 这种在 Windows 上是
    // 合法组合（前者 CapsLock 本身就是一颗普通键），读回来时一颗都不能丢，
    // 否则「打开面板 → 点完成」会把组合改短
    let mut mains: Vec<String> = Vec::new();
    for part in first.split('+') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(token) = modifier_token(part) {
            if !mods.iter().any(|seen| seen == token) {
                mods.push(token.to_string());
            }
        } else if let Some(token) = main_token(part) {
            if !mains.iter().any(|seen| seen == &token) {
                mains.push(token);
            }
        } else {
            return (Vec::new(), Vec::new());
        }
    }
    (mods, mains)
}

/// 修饰键在前、主键在后拼成设置里的写法（没有主键 = 还没选完）。
///
/// 主键可以有好几颗（面板里连着点 A、B、C 就是三颗）：
/// - 「发送按键」那种序列：每颗各自带着修饰键，之间用空格隔开 → `Ctrl+A B`
/// - 快捷键只能有一颗（点第二颗就换掉第一颗，不会拼出 `A+B` 这种不存在的组合）
fn compose(mods: &[String], mains: &[String], append: bool) -> Option<String> {
    if mains.is_empty() {
        return None;
    }
    let mut combos: Vec<String> = Vec::new();
    for main in mains {
        let mut parts = mods.to_vec();
        parts.push(main.clone());
        combos.push(parts.join("+"));
    }
    if append {
        Some(combos.join(" "))
    } else {
        // 快捷键：几颗主键拼在同一个组合里（修饰键在最前）
        let mut parts = mods.to_vec();
        parts.extend(mains.iter().cloned());
        Some(parts.join("+"))
    }
}

/// 写进输入框的最终文本：追加模式接在后面（「发送按键」的序列），否则整条替换。
fn merge_text(existing: &str, combo: &str, append: bool) -> String {
    if !append {
        return combo.to_string();
    }
    let existing = existing.trim_end();
    if existing.is_empty() {
        combo.to_string()
    } else {
        format!("{existing} {combo}")
    }
}

// ---------------------------------------------------------------------------
// 窗口
// ---------------------------------------------------------------------------

thread_local! {
    /// 调试钩子只自动弹一次（`GPUI_MENU_DEBUG_KEYPAD`）
    static DEBUG_SHOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn debug_shot_taken() -> bool {
    DEBUG_SHOT.with(|flag| flag.get())
}

pub fn debug_mark_shot() {
    DEBUG_SHOT.with(|flag| flag.set(true));
}

pub struct KeyPadHandle(pub std::cell::Cell<Option<AnyWindowHandle>>);

impl Global for KeyPadHandle {}

pub fn install(cx: &mut App) {
    cx.set_global(KeyPadHandle(std::cell::Cell::new(None)));
}

fn handle(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<KeyPadHandle>().and_then(|slot| slot.0.get())
}

/// 面板右上角那个「区分左右修饰键」开关拨的是**哪一条**快捷键的设置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidedTarget {
    /// 弹出菜单那一颗
    Popup,
    /// 「不抓选中内容」那一颗
    Noselect,
    /// 「一键直达」那一颗
    Magic,
    /// 某个菜单项的触发键
    Item(String),
    /// 某个菜单项的**局部**触发键
    ItemLocal(String),
}

impl SidedTarget {
    fn get(&self) -> bool {
        crate::settings::read(|settings| match self {
            SidedTarget::Popup => settings.popup_hotkey_sided,
            SidedTarget::Noselect => settings.popup_hotkey_noselect_sided,
            SidedTarget::Magic => settings.magic.sided,
            SidedTarget::Item(id) => settings
                .find_path(id)
                .and_then(|path| settings.node(&path))
                .map(|item| item.hotkey_sided)
                .unwrap_or(true),
            SidedTarget::ItemLocal(id) => settings
                .find_path(id)
                .and_then(|path| settings.node(&path))
                .map(|item| item.hotkey_local_sided)
                .unwrap_or(true),
        })
    }

    fn set(&self, value: bool, cx: &mut gpui::App) {
        let target = self.clone();
        crate::settings::update(cx, move |settings| match target {
            SidedTarget::Popup => settings.popup_hotkey_sided = value,
            SidedTarget::Noselect => settings.popup_hotkey_noselect_sided = value,
            SidedTarget::Magic => settings.magic.sided = value,
            SidedTarget::Item(id) => {
                if let Some(path) = settings.find_path(&id) {
                    if let Some(item) = settings.node_mut(&path) {
                        item.hotkey_sided = value;
                    }
                }
            }
            SidedTarget::ItemLocal(id) => {
                if let Some(path) = settings.find_path(&id) {
                    if let Some(item) = settings.node_mut(&path) {
                        item.hotkey_local_sided = value;
                    }
                }
            }
        });
    }
}

/// 打开（或替换）按键面板窗口。
///
/// `sided` 是「正在编辑哪一条快捷键」：`None` = 这次不是配快捷键（比如「发送按键」
/// 那种序列），右上角就不显示那个开关。
pub fn open(target: Entity<TextInput>, append: bool, sided: Option<SidedTarget>, cx: &mut App) {
    // 同一时间只留一扇：改另一个输入框时直接换掉旧的
    close(cx);
    let (mods, mains) = seed(&target.read(cx).text());
    let numlock = crate::win32::numlock_on();
    let sided_on = sided.as_ref().map(|target| target.get()).unwrap_or(true);
    let mut width = WIDTH;
    let mut height = HEIGHT;
    if let Some(display) = cx.primary_display() {
        let screen = display.bounds();
        width = width.min(f32::from(screen.size.width) - 60.0);
        height = height.min(f32::from(screen.size.height) - 60.0);
    }
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            kind: gpui::WindowKind::Normal,
            is_resizable: true,
            is_movable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
            focus: true,
            show: true,
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        move |_window, cx| {
            cx.new(|cx| KeyPadView {
                target,
                append,
                mods,
                mains,
                numlock,
                sided: sided_on,
                sided_target: sided,
                media_open: false,
                zoom: 1.0,
                _subs: Vec::new(),
                focus: cx.focus_handle(),
            })
        },
    );
    if let Ok(opened) = opened {
        let any: AnyWindowHandle = opened.into();
        if let Some(slot) = cx.try_global::<KeyPadHandle>() {
            slot.0.set(Some(any));
        }
    }
}

/// 面板开着吗。
pub fn any_open(cx: &mut App) -> bool {
    let Some(handle) = handle(cx) else {
        return false;
    };
    handle.update(cx, |_, _, _| {}).is_ok()
}

/// 关掉面板窗口。
pub fn close(cx: &mut App) {
    let Some(slot) = cx.try_global::<KeyPadHandle>() else {
        return;
    };
    let Some(handle) = slot.0.take() else {
        return;
    };
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

/// 「键盘」小按钮：点一下弹出按键面板。
///
/// `id` 要是这个窗口里唯一的（同一个窗口可能有好几个快捷键输入框）；
/// `sided` 见 [`SidedTarget`]（「发送按键」那种序列传 `None`）。
pub fn open_button(
    id: impl Into<SharedString>,
    target: &Entity<TextInput>,
    append: bool,
    sided: Option<SidedTarget>,
) -> AnyElement {
    let target = target.clone();
    div()
        .id(id.into())
        .h(px(24.))
        .px(px(9.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .rounded(px(6.))
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::surface_elevated())
        .text_size(px(11.))
        .text_color(theme::ui_text_secondary())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .child("⌨")
        .child(crate::i18n::t("键盘"))
        .on_click(move |_event, window, cx| {
            open(target.clone(), append, sided.clone(), cx);
            window.refresh();
        })
        .into_any_element()
}

pub struct KeyPadView {
    target: Entity<TextInput>,
    /// 追加到现有内容后面（「发送按键」的序列用），否则整条替换
    append: bool,
    mods: Vec<String>,
    /// 选中的主键。追加模式（「发送按键」）可以连着点多颗，拼成一段序列；
    /// 快捷键模式只留最后一颗 —— 一个组合里不可能有两个主键
    mains: Vec<String>,
    /// 键帽缩放（1.0 起步；窗口还能自己拉大）
    zoom: f32,
    /// 面板里显示的 NumLock 状态（按 NUM 那颗真去拨系统状态）
    numlock: bool,
    /// 这一条快捷键区分左右修饰键吗（面板右上角的开关，直接写回设置）
    sided: bool,
    /// 开关拨的是谁；`None` = 这不是配快捷键（没有开关）
    sided_target: Option<SidedTarget>,
    /// 键盘上没画的那一片（音量 / 播放 / 浏览器）是不是展开着
    media_open: bool,
    _subs: Vec<gpui::Subscription>,
    focus: gpui::FocusHandle,
}

impl KeyPadView {
    fn press(&mut self, token: &str, modifier: bool) {
        if modifier {
            // 关掉「区分左右」时，左右两颗算同一颗：点 RCTRL 记成 `Ctrl`，
            // 再点 LCTRL 就把它取消掉（而不是两个都亮）
            let token = if self.sided { token } else { generic(token) };
            match self
                .mods
                .iter()
                .position(|seen| seen == token || (!self.sided && generic(seen) == token))
            {
                Some(index) => {
                    self.mods.remove(index);
                }
                None => self.mods.push(token.to_string()),
            }
        } else if let Some(index) = self.mains.iter().position(|seen| seen == token) {
            // 再点一次取消这颗
            self.mains.remove(index);
        } else {
            // 主键可以连着点好几颗：追加模式拼成一段序列（空格隔开），
            // 快捷键模式拼进同一个组合（`CapsLock+1`、`LButton+WheelUp`）——
            // 这两种写法 Windows 都认，没必要限制成「只能一颗」
            self.mains.push(token.to_string());
        }
    }

    fn commit(&mut self, window: &mut Window, cx: &mut App) {
        let Some(text) = compose(&self.mods, &self.mains, self.append) else {
            return;
        };
        let existing = self.target.read(cx).text().to_string();
        let text = merge_text(&existing, &text, self.append);
        self.target
            .update(cx, |input, cx| input.set_text(text, cx));
        window.remove_window();
    }

    fn clear(&mut self) {
        self.mods.clear();
        self.mains.clear();
    }

    /// 键盘底下那几行说明（原来在设置页「匹配规则」那块）。
    fn help_block(&self) -> AnyElement {
        let sided = self.sided;
        let sided_text = if self.sided_target.is_none() && !self.append {
            crate::i18n::t("这一条不分左右").to_string()
        } else if sided {
            if self.sided_target.is_none() {
                crate::i18n::t(
                    "区分左右：开着 —— 序列里写的是 LCtrl / RCtrl，注入时左右各管各的（这是这条序列自己的开关，不用存）",
                )
                .to_string()
            } else {
                crate::i18n::t(
                    "区分左右：开着 —— 点 LCTRL 写的是 LCtrl、点 RCTRL 写的是 RCtrl，按错边不触发。点右上角那颗可以关掉",
                )
                .to_string()
            }
        } else {
            crate::i18n::t(
                "区分左右：关着 —— 键盘还是全画着，但点哪一颗都写成 Ctrl / Alt / Shift / Win（左右两颗一起亮），按哪边都算。再点一下右上角恢复",
            )
            .to_string()
        };
        let lines = vec![
            sided_text,
            crate::i18n::t(
                "怎么做到覆盖别的程序：快捷键走的是底层键盘钩子（和热字符串同一条），按键先到我们这儿、再问别的程序 —— 所以 Win+数字、鼠标侧键这类被系统 / 别的软件占着的组合也能抢过来",
            )
            .to_string(),
            crate::i18n::t(
                "鼠标键：左键、右键、中键要配着 Ctrl / Alt / Shift / Win 用（单按会把全系统的点击都吞掉）；侧键和滚轮可以单用。小键盘按 NumLock 分两套名字",
            )
            .to_string(),
        ];
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .mt(px(10.))
            .max_w(px(820.)); 
        for line in lines {
            block = block.child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .child(line),
            );
        }
        block.into_any_element()
    }

    /// 多媒体键那一片：键盘上根本没画这些键（音量 / 播放 / 浏览器 / 启动），
    /// 单开一块按钮，点一下和点普通键一样 —— 都算「选中主键」，再点「完成」写回去。
    fn media_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = div()
            .id("kp-media")
            .w_full()
            .max_w(px(820.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(8.))
            .mb(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_elevated());
        for (group, keys) in crate::mediakeys::GROUPS {
            let mut row = div().flex().items_center().gap(px(4.)).flex_wrap().child(
                div()
                    .w(px(52.))
                    .flex_none()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(*group)),
            );
            for (label, token) in *keys {
                let lit = self.mains.iter().any(|seen| seen == *token);
                let token_owned = (*token).to_string();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("kp-media-{token}")))
                        .h(px(22.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .text_size(px(10.5))
                        .cursor_pointer()
                        .bg(if lit {
                            theme::accent()
                        } else {
                            theme::track()
                        })
                        .text_color(if lit {
                            theme::surface_elevated()
                        } else {
                            theme::text_primary()
                        })
                        .hover(|style| style.bg(theme::surface_hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.press(&token_owned, false);
                            window.refresh();
                            cx.notify();
                        }))
                        .child(crate::i18n::t(*label).to_string()),
                );
            }
            panel = panel.child(row);
        }
        panel
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "和普通键一样：选好点右下角「完成」；要组合就先点修饰键（比如 Ctrl+音量加），写进去的是 AHK 的键名",
                    )),
            )
            .into_any_element()
    }
}

impl Render for KeyPadView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom = self.zoom;
        let u = U * zoom;
        let gap = GAP * zoom;
        let font = KEY_FONT * zoom;
        let pad = PAD * zoom;

        // ---- 顶部：标题 + 缩放 + NumLock + 关闭
        let mut header = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(14.))
            .py(px(8.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(crate::i18n::t("按键选择")),
            );

        // 区分左右修饰键：拨的是**这一条**快捷键的设置（不是全局）。
        // 拨完面板立刻重排：左右各一颗 ↔ 每族只剩一颗。
        //
        // 「发送按键」那种序列没有「存哪儿」这一说（序列本身就是文本），
        // 所以它的开关只活在这扇窗口里，照样能点出 LCtrl / RCtrl。
        if self.sided_target.is_some() || self.append {
            let sided = self.sided;
            header = header.child(
                div()
                    .id("kp-sided")
                    .h(px(24.))
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .rounded(px(6.))
                    .text_size(px(11.))
                    .cursor_pointer()
                    .bg(if sided {
                        theme::accent_soft()
                    } else {
                        theme::track()
                    })
                    .text_color(if sided {
                        theme::accent()
                    } else {
                        theme::ui_text_secondary()
                    })
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.sided = !this.sided;
                        // 写回设置：钩子那张表会跟着重建
                        if let Some(target) = this.sided_target.clone() {
                            target.set(this.sided, cx);
                        }
                        // 关掉「区分左右」之后 `RCtrl` 这种写法就不合法了，
                        // 顺手换成本族的通用写法（`Ctrl`）
                        if !this.sided {
                            this.mods = this
                                .mods
                                .iter()
                                .map(|token| generic(token).to_string())
                                .collect();
                            this.mods.sort();
                            this.mods.dedup();
                        }
                        window.refresh();
                        cx.notify();
                    }))
                    .child(crate::i18n::t("左右分开"))
                    .child(if sided { "开" } else { "关" }),
            );
        }

        // NumLock 状态：点一下真的拨系统的 NumLock（小键盘那一片的名字跟着变）
        // 多媒体键：键盘上没画这些键，单开一片按钮。点一下和点普通键一样算「选中主键」
        header = header.child(
            div()
                .id("kp-media-toggle")
                .h(px(24.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(6.))
                .text_size(px(11.))
                .cursor_pointer()
                .bg(if self.media_open {
                    theme::accent_soft()
                } else {
                    theme::track()
                })
                .text_color(if self.media_open {
                    theme::accent()
                } else {
                    theme::ui_text_secondary()
                })
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.media_open = !this.media_open;
                    window.refresh();
                    cx.notify();
                }))
                .child(crate::i18n::t("多媒体键")),
        );

        let numlock_on = self.numlock;
        header = header.child(
            div()
                .id("kp-numlock")
                .h(px(24.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(6.))
                .text_size(px(11.))
                .cursor_pointer()
                .bg(if numlock_on {
                    theme::accent_soft()
                } else {
                    theme::track()
                })
                .text_color(if numlock_on {
                    theme::accent()
                } else {
                    theme::ui_text_secondary()
                })
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    crate::win32::press_numlock();
                    this.numlock = crate::win32::numlock_on();
                    window.refresh();
                    cx.notify();
                }))
                .child("NUM")
                .child(if numlock_on { "开" } else { "关" }),
        );

        for (label, delta) in [("−", -0.1_f32), ("＋", 0.1_f32)] {
            header = header.child(
                div()
                    .id(SharedString::from(format!("kp-zoom-{label}")))
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border_strong())
                    .bg(theme::surface_elevated())
                    .text_size(px(12.))
                    .text_color(theme::ui_text_secondary())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.zoom = (this.zoom + delta).clamp(0.8, 1.8);
                        window.refresh();
                        cx.notify();
                    }))
                    .child(label),
            );
        }
        header = header.child(
            div()
                .h(px(24.))
                .px(px(6.))
                .flex()
                .items_center()
                .text_size(px(10.5))
                .text_color(theme::ui_text_tertiary())
                .child(format!("{:.0}%", zoom * 100.0)),
        );
        header = header.child(
            div()
                .id("kp-close")
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_size(px(12.))
                .text_color(theme::ui_text_tertiary())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .child("✕")
                .on_click(cx.listener(|_this, _: &ClickEvent, window, _cx| {
                    window.remove_window();
                })),
        );

        // ---- 预览
        let mut parts: Vec<(String, bool)> = self
            .mods
            .iter()
            .map(|token| (token.clone(), true))
            .collect();
        for main in self.mains.clone() {
            parts.push((main, false));
        }
        let mut preview = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(4.))
            .px(px(14.))
            .py(px(6.))
            .min_h(px(32.));
        if parts.is_empty() {
            preview = preview.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t("先点上修饰键（可多选），再点一个主键")),
            );
        } else {
            for (index, (token, is_mod)) in parts.iter().enumerate() {
                if index > 0 {
                    preview = preview.child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme::ui_text_tertiary())
                            .child("+"),
                    );
                }
                preview = preview.child(
                    div()
                        .flex_none()
                        .h(px(22.))
                        .px(px(7.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .bg(if *is_mod {
                            theme::accent_soft()
                        } else {
                            theme::accent()
                        })
                        .text_color(if *is_mod {
                            theme::accent()
                        } else {
                            theme::surface_elevated()
                        })
                        .text_size(px(11.5))
                        .child(label_of(token)),
                );
            }
        }

        // ---- 键盘
        let mut key_id = 0usize;
        let key = |item: &Key, index: usize, width: f32, cx: &mut Context<Self>| -> AnyElement {
            if item.token.is_empty() {
                return div()
                    .flex_none()
                    .w(px(width))
                    .h(px(u))
                    .into_any_element();
            }
            // 小键盘那一片：按 NumLock 挑这一颗现在到底是谁
            let (label, token) = match &item.alt {
                Some((off_label, off_token)) if !numlock_on => (off_label.clone(), off_token.clone()),
                _ => (item.label.clone(), item.token.clone()),
            };
            let sub = item.alt.as_ref().map(|(off_label, _)| {
                if numlock_on {
                    off_label.clone()
                } else {
                    item.label.clone()
                }
            });
            let modifier = item.modifier;
            let lit = if modifier {
                // 存的是「通用写法」（`Ctrl`，关掉左右分开 / 输入框里本来就这么写）时，
                // LCTRL / RCTRL 两颗一起亮 —— 「按哪颗都算」这件事得让人看得见
                self.mods.iter().any(|seen| {
                    *seen == item.token
                        || (generic(seen) == seen.as_str() && generic(&item.token) == seen)
                })
            } else {
                self.mains.iter().any(|seen| seen.as_str() == token.as_str())
            };
            let token_owned = token.clone();
            let mut element = div()
                .id(SharedString::from(format!("kp-key-{index}-{token}")))
                .flex_none()
                .w(px(width))
                .h(px(u))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .border_1()
                .text_size(px(font))
                // 键帽上的字**绝不折行**：`MENU` 折成 `MEN`+`U` 会顶到下一行去，
                // 看着就像这颗键没对齐
                .whitespace_nowrap()
                .overflow_hidden()
                .cursor_pointer()
                .child(label);
            if let Some(sub) = sub {
                element = element.child(
                    div()
                        .text_size(px(font * 0.72))
                        .text_color(if lit {
                            theme::surface_elevated()
                        } else {
                            theme::ui_text_tertiary()
                        })
                        .child(sub),
                );
            }
            if lit {
                element = element
                    .bg(theme::accent())
                    .border_color(theme::accent())
                    .text_color(theme::text_on_accent());
            } else if modifier {
                element = element
                    .bg(theme::track())
                    .border_color(theme::border())
                    .text_color(theme::text_primary())
                    .hover(|style| style.bg(theme::surface_hover()));
            } else {
                element = element
                    .bg(theme::surface_elevated())
                    .border_color(theme::border())
                    .text_color(theme::text_primary())
                    .hover(|style| style.bg(theme::surface_hover()));
            }
            element
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.press(&token_owned, modifier);
                    window.refresh();
                    cx.notify();
                }))
                .into_any_element()
        };

        let mut board = div().flex().items_start().gap(px(gap * 3.0));
        for block in layout().iter() {
            let mut column = div().flex().flex_col().gap(px(gap));
            let cell = cell_width(block, zoom);
            for row in block.rows.iter() {
                let mut line = div().flex().items_center().gap(px(gap)).h(px(u));
                for item in row.iter() {
                    key_id += 1;
                    line = line.child(key(item, key_id, width_in(block, item, zoom), cx));
                }
                column = column.child(line);
            }
            // 网格块：宽度固定成「列数 × 格宽」，不然每条行按自己的键算宽，会左右漂
            if block.uniform {
                let columns = block.rows.iter().map(|row| row.len()).max().unwrap_or(0);
                column = column.w(px(columns as f32 * cell + gap * (columns.saturating_sub(1)) as f32));
            }
            board = board.child(column);
        }

        // 鼠标那排：和键盘同宽，单独一块
        let mut mouse_row = div().flex().items_center().gap(px(gap)).mt(px(gap * 3.0));
        for item in mouse_layout().iter() {
            key_id += 1;
            mouse_row = mouse_row.child(key(item, key_id, key_width(item, zoom), cx));
        }

        let board_area = div()
            .id("kp-board-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .overflow_x_scroll()
            .p(px(pad))
            .flex()
            .flex_col()
            .items_center()
            .child(if self.media_open {
                self.media_panel(cx)
            } else {
                div().into_any_element()
            })
            .child(board)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .mt(px(10.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::ui_text_tertiary())
                            .child(crate::i18n::t("鼠标键：")),
                    )
                    .child(mouse_row),
            )
            // 「匹配规则」那一块说明从设置页搬过来了：写法和限制放在按键旁边，
            // 边点边看，比在另一页翻说明强
            .child(self.help_block());

        // ---- 底部
        let has_main = !self.mains.is_empty();
        let mut done = div()
            .id("kp-done")
            .h(px(28.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .text_size(px(12.))
            .child(crate::i18n::t(if self.append { "追加" } else { "完成" }));
        if has_main {
            done = done
                .bg(theme::accent())
                .text_color(theme::text_on_accent())
                .cursor_pointer()
                .hover(|style| style.opacity(0.9))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.commit(window, cx);
                }));
        } else {
            done = done
                .bg(theme::track())
                .text_color(theme::ui_text_tertiary());
        }

        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(46.))
            .px(px(14.))
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(
                        "修饰键可以点两颗（左右各一颗），再点一次取消；关掉「左右分开」之后点哪一颗都写成通用的那个写法，两颗一起亮",
                    )),
            )
            .child(
                div()
                    .id("kp-clear")
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border_strong())
                    .bg(theme::surface_elevated())
                    .text_size(px(12.))
                    .text_color(theme::ui_text_secondary())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .child(crate::i18n::t("清空"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.clear();
                        window.refresh();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("kp-cancel")
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border_strong())
                    .bg(theme::surface_elevated())
                    .text_size(px(12.))
                    .text_color(theme::ui_text_secondary())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .child(crate::i18n::t("取消"))
                    .on_click(cx.listener(|_this, _: &ClickEvent, window, _cx| {
                        window.remove_window();
                    })),
            )
            .child(done);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::surface())
            .text_color(theme::text_primary())
            .child(header)
            .child(preview)
            .child(board_area)
            .child(footer)
            .track_focus(&self.focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_reads_a_plain_combo() {
        let (mods, mains) = seed("Ctrl+Alt+M");
        assert_eq!(mods, vec!["Ctrl", "Alt"]);
        assert_eq!(mains, vec!["M"]);
    }

    #[test]
    fn seed_understands_left_and_right_modifiers() {
        let (mods, mains) = seed("LCtrl+RShift+1");
        assert_eq!(mods, vec!["LCtrl", "RShift"]);
        assert_eq!(mains, vec!["1"]);
    }

    #[test]
    fn seed_keeps_single_keys_and_symbols() {
        assert_eq!(seed("Enter").1, vec!["Enter"]);
        assert_eq!(seed("-").1, vec!["-"]);
        assert_eq!(seed("NumEnter").1, vec!["NumEnter"]);
        assert_eq!(seed("PageDown").1, vec!["PageDown"]);
        // 鼠标键也要能读回面板里（`XButton1` → 面板上的写法）
        assert_eq!(seed("LButton").1, vec!["LButton"]);
        assert_eq!(seed("WheelUp").1, vec!["WheelUp"]);
    }

    #[test]
    fn seed_gives_up_on_sequences() {
        // 多组是「连按几个键」，面板只管一组
        assert_eq!(
            seed("Ctrl+A Delete"),
            (Vec::<String>::new(), Vec::<String>::new())
        );
        assert!(seed("").1.is_empty());
    }

    #[test]
    fn hotkey_keeps_every_pressed_key() {
        // 用户要的那种：CapsLock+1、鼠标左键+滚轮上
        let none: Vec<String> = Vec::new();
        let caps = vec!["CapsLock".to_string(), "1".to_string()];
        assert_eq!(compose(&none, &caps, false).as_deref(), Some("CapsLock+1"));
        let mouse = vec!["LButton".to_string(), "WheelUp".to_string()];
        assert_eq!(compose(&none, &mouse, false).as_deref(), Some("LButton+WheelUp"));
        // 读回面板也要一颗不丢
        let (_, back) = seed("CapsLock+1");
        assert_eq!(back, vec!["CapsLock", "1"]);
        let (_, back) = seed("LButton+WheelUp");
        assert_eq!(back, vec!["LButton", "WheelUp"]);
    }

    #[test]
    fn compose_puts_modifiers_first() {
        let mods = vec!["LCtrl".to_string(), "RShift".to_string()];
        assert_eq!(
            compose(&mods, &["1".to_string()], false).as_deref(),
            Some("LCtrl+RShift+1")
        );
        assert_eq!(compose(&mods, &[], false), None, "没点主键就不算拼完");
    }

    #[test]
    fn several_plain_keys_make_a_sequence_in_append_mode() {
        // 「发送按键」：不加修饰键也能连点几颗，每颗各自带修饰键，之间用空格
        let none: Vec<String> = Vec::new();
        let keys = vec!["A".to_string(), "B".to_string(), "Enter".to_string()];
        assert_eq!(
            compose(&none, &keys, true).as_deref(),
            Some("A B Enter"),
            "连点三颗 = 一段序列"
        );
        let mods = vec!["LCtrl".to_string()];
        assert_eq!(
            compose(&mods, &keys, true).as_deref(),
            Some("LCtrl+A LCtrl+B LCtrl+Enter")
        );
        // 快捷键模式：几颗主键拼进同一个组合（Windows 上有 `CapsLock+1` 这种写法）
        assert_eq!(compose(&none, &keys, false).as_deref(), Some("A+B+Enter"));
    }

    #[test]
    fn merge_text_appends_for_send_keys_and_replaces_for_hotkeys() {
        // 发送按键：接在后面，多组用空格隔开
        assert_eq!(merge_text("Ctrl+A", "Delete", true), "Ctrl+A Delete");
        assert_eq!(merge_text("   ", "Enter", true), "Enter");
        // 快捷键：整条替换
        assert_eq!(merge_text("Ctrl+Alt+M", "LCtrl+RShift+1", false), "LCtrl+RShift+1");
    }

    #[test]
    fn every_token_round_trips() {
        // 键帽上的写法必须能被 `seed` 读回来，否则「打开面板→点完成」会换掉内容
        for key in all_keys() {
            if key.token.is_empty() {
                continue;
            }
            if key.modifier {
                assert_eq!(modifier_token(&key.token), Some(key.token.as_str()));
            } else {
                assert_eq!(
                    main_token(&key.token).as_deref(),
                    Some(key.token.as_str()),
                    "{} 读不回来",
                    key.token
                );
            }
            if let Some((_, alt)) = &key.alt {
                assert_eq!(
                    main_token(alt).as_deref(),
                    Some(alt.as_str()),
                    "NumLock 关着时的 {} 读不回来",
                    alt
                );
            }
        }
    }

    #[test]
    fn numpad_has_two_names_for_the_same_physical_key() {
        // NumLock 关着的那套名字得能对上 AHK 的表（Numpad7 = Home）
        let key = numpad_rows()
            .into_iter()
            .flatten()
            .find(|key| key.token == "Num7")
            .expect("小键盘上有 7");
        let (label, token) = key.alt.clone().expect("7 也有第二个名字");
        assert_eq!((label.as_str(), token.as_str()), ("Home", "Home"));
    }
}
