//! 视觉主题：由「底色 + 强调色 + 三项覆盖」派生出整套令牌。
//!
//! 设计参照 Bolume：打底只有深 / 浅两种「模板」，具体长什么样由用户选的颜色决定 ——
//! 选了浅底，文字和描边自动翻成深色；强调色换了，选中态、滑块、开关全跟着走。
//!
//! 用户可配的只有 6 个值（`ThemeColors`），其余令牌都是算出来的：
//! - 底色 `surface`：菜单卡片 / 窗口底
//! - 强调色 `accent`
//! - 选中项背景 `selected`（留空 = 强调色淡底）
//! - 文字 `text`（留空 = 按底色明暗自动取）
//! - 菜单提示文字 `muted`（留空 = 文字色的半透明）
//! - 设置页提示文字 `muted_ui`（留空 = 跟菜单提示文字一致）

use std::cell::RefCell;

use gpui::{px, rgba, Pixels, Rgba, SharedString};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 用户配置
// ---------------------------------------------------------------------------

/// 一套配色的用户输入。空串 = 自动派生。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    pub surface: String,
    pub accent: String,
    pub selected: String,
    pub text: String,
    /// 菜单项名称后面那个加速键字母（留空 = 跟文字色一致）
    pub accel: String,
    /// 菜单里的提示文字（快捷键、说明）
    pub muted: String,
    /// 设置 / 编辑窗口里的提示文字（和菜单分开调）
    pub muted_ui: String,
    /// 卡片描边（留空 = 按底色自动派生一档）
    pub border: String,
}

impl ThemeColors {
    pub fn new(surface: &str, accent: &str) -> Self {
        Self {
            surface: surface.into(),
            accent: accent.into(),
            ..Self::default()
        }
    }

    /// 自动派生出来的值（给界面上「自动」按钮回显用）。
    pub fn effective(&self, field: Field) -> String {
        let palette = Palette::build(self);
        let color = match field {
            Field::Surface => palette.surface,
            Field::Accent => palette.accent,
            Field::Selected => palette.row_selected,
            Field::Text => palette.text_primary,
            Field::Accel => palette.accel_text,
            Field::Muted => palette.text_secondary,
            Field::MutedUi => palette.ui_secondary,
            Field::Border => palette.border,
        };
        to_hex(color >> 8)
    }

    pub fn slot(&self, field: Field) -> &str {
        match field {
            Field::Surface => &self.surface,
            Field::Accent => &self.accent,
            Field::Selected => &self.selected,
            Field::Text => &self.text,
            Field::Accel => &self.accel,
            Field::Muted => &self.muted,
            Field::MutedUi => &self.muted_ui,
            Field::Border => &self.border,
        }
    }

    pub fn slot_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Surface => &mut self.surface,
            Field::Accent => &mut self.accent,
            Field::Selected => &mut self.selected,
            Field::Text => &mut self.text,
            Field::Accel => &mut self.accel,
            Field::Muted => &mut self.muted,
            Field::MutedUi => &mut self.muted_ui,
            Field::Border => &mut self.border,
        }
    }
}

/// 可配置的 8 个色位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Surface,
    Accent,
    Selected,
    Text,
    /// 加速键字母（默认跟文字色一致）
    Accel,
    Muted,
    MutedUi,
    /// 卡片描边（默认按底色自动派生；菜单卡片那一圈的粗细另由设置里的开关控制）
    Border,
}

/// 浅色 / 深色两套的默认值。
pub fn default_light() -> ThemeColors {
    ThemeColors::new("#ffffff", "#2f6feb")
}

pub fn default_dark() -> ThemeColors {
    ThemeColors::new("#181b21", "#6e9bff")
}

// ---------------------------------------------------------------------------
// 派生出来的调色板
// ---------------------------------------------------------------------------

/// 一套算好的配色。颜色统一 `0xRRGGBBAA`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// 用户选的背景色：菜单卡片 / 内嵌块就是它
    pub surface: u32,
    /// 窗口底（比背景色再暗一档，卡片才浮得起来）
    pub window: u32,
    pub surface_sunken: u32,
    pub surface_elevated: u32,
    pub surface_hover: u32,
    pub border: u32,
    pub border_strong: u32,
    pub accent: u32,
    pub accent_soft: u32,
    pub accent_glow: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub text_tertiary: u32,
    /// 加速键字母（默认 = 文字色）
    pub accel_text: u32,
    /// 加速键下面那道细线（同色，压低透明度）
    pub accel_underline: u32,
    /// 设置 / 编辑窗口里的次级文字（可以和菜单分开）
    pub ui_secondary: u32,
    /// 设置 / 编辑窗口里的三级文字
    pub ui_tertiary: u32,
    pub track: u32,
    /// 菜单里「当前高亮行」的底色
    pub row_selected: u32,
    pub separator: u32,
    pub badge_fill: u32,
    pub scrollbar: u32,
    pub danger: u32,
    /// 这套配色算不算深色
    pub dark: bool,
}

impl Palette {
    /// 由用户输入的 5 个色值算出整套令牌。
    pub fn build(colors: &ThemeColors) -> Self {
        let surface = parse_hex(&colors.surface).unwrap_or(0xffffff);
        // 底色是浅是深，决定文字和线条往哪个方向翻
        let dark = !is_light(surface);
        let text = parse_hex(&colors.text).unwrap_or(if dark { 0xe9ecf2 } else { 0x1f2329 });
        let accent = parse_hex(&colors.accent).unwrap_or(0x2f6feb);
        let muted = parse_hex(&colors.muted);
        // 设置页提示文字：没单独设就跟着菜单那一份
        let muted_ui = parse_hex(&colors.muted_ui);
        // 加速键：没单独设就跟文字色一致
        let accel_rgb = parse_hex(&colors.accel).unwrap_or(text);

        // 三档层次：窗口底 < 侧栏/分组 < 卡片（卡片就是用户选的色）
        let window = tint(surface, if dark { -0.07 } else { -0.045 });
        let sunken = tint(surface, if dark { -0.035 } else { -0.02 });

        let secondary = muted.map(opaque).unwrap_or(with_alpha(text, 0xa8));
        let tertiary = muted
            .map(|color| with_alpha(mix(color, surface, 0.3), 0xc8))
            .unwrap_or(with_alpha(text, 0x6b));
        let ui_secondary = muted_ui.map(opaque).unwrap_or(secondary);
        let ui_tertiary = muted_ui
            .map(|color| with_alpha(mix(color, surface, 0.3), 0xc8))
            .unwrap_or(tertiary);

        // 描边：用户没单独指定就按底色与文字色混出来（深色底混亮、浅色底混暗）
        let border_custom = parse_hex(&colors.border);
        let border = border_custom
            .map(opaque)
            .unwrap_or_else(|| opaque(mix(surface, text, if dark { 0.17 } else { 0.11 })));
        let border_strong = match border_custom {
            Some(color) => opaque(mix(color, text, 0.4)),
            None => opaque(mix(surface, text, if dark { 0.26 } else { 0.18 })),
        };

        Self {
            surface: opaque(surface),
            window: opaque(window),
            surface_sunken: opaque(sunken),
            surface_elevated: opaque(surface),
            surface_hover: with_alpha(text, if dark { 0x12 } else { 0x0a }),
            border,
            border_strong,
            accent: opaque(accent),
            accent_soft: with_alpha(accent, 0x2e),
            accent_glow: with_alpha(accent, 0x59),
            text_primary: opaque(text),
            text_secondary: secondary,
            text_tertiary: tertiary,
            accel_text: opaque(accel_rgb),
            accel_underline: with_alpha(accel_rgb, 0x66),
            ui_secondary,
            ui_tertiary,
            track: with_alpha(text, if dark { 0x1f } else { 0x14 }),
            row_selected: match parse_hex(&colors.selected) {
                Some(color) => opaque(color),
                None => with_alpha(accent, if dark { 0x2e } else { 0x24 }),
            },
            separator: opaque(mix(surface, text, if dark { 0.12 } else { 0.08 })),
            badge_fill: with_alpha(text, if dark { 0x12 } else { 0x0c }),
            scrollbar: with_alpha(text, 0x33),
            danger: if dark { 0xff7b72ff } else { 0xc62d33ff },
            dark,
        }
    }
}

// ---------------------------------------------------------------------------
// 取色（界面里只调这些）
// ---------------------------------------------------------------------------

thread_local! {
    static CURRENT: RefCell<Palette> = RefCell::new(Palette::build(&default_light()));
}

pub fn palette() -> Palette {
    CURRENT.with(|slot| *slot.borrow())
}

pub fn set_palette(next: Palette) {
    CURRENT.with(|slot| *slot.borrow_mut() = next);
}

macro_rules! token {
    ($name:ident, $field:ident) => {
        pub fn $name() -> Rgba {
            rgba(palette().$field)
        }
    };
}

token!(surface, surface);
token!(surface_sunken, surface_sunken);
token!(surface_elevated, surface_elevated);
token!(surface_hover, surface_hover);
token!(border, border);
token!(border_strong, border_strong);
token!(accent, accent);
token!(accent_soft, accent_soft);
token!(accent_glow, accent_glow);
token!(text_primary, text_primary);
token!(text_secondary, text_secondary);
token!(text_tertiary, text_tertiary);
token!(accel_text, accel_text);
token!(accel_underline, accel_underline);
token!(ui_text_secondary, ui_secondary);
token!(ui_text_tertiary, ui_tertiary);
token!(track, track);
token!(row_selected, row_selected);
token!(separator, separator);
token!(badge_fill, badge_fill);
token!(scrollbar, scrollbar);
token!(danger, danger);

/// 压在强调色上的字（按钮、选中的色块）。
///
/// 以前几处图省事写成 `theme::surface()` —— 浅色主题下看着是白的，**暗色主题下
/// 就是深灰压在蓝底上，几乎看不见**。统一走这个函数，两套主题都是浅色字。
pub fn text_on_accent() -> Rgba {
    rgba(0xFFFFFFF2)
}

/// 窗口根底色：比「菜单背景色」再暗一档，卡片才浮得起来。
pub fn surface_window() -> Rgba {
    rgba(palette().window)
}

/// 字号：所有文字尺寸都过这一层，改「字号」时菜单整体跟着缩放。
pub fn fs(size: f32) -> Pixels {
    px(size * crate::settings::font_scale())
}

/// 菜单字体：设置里选的字体族，没选就是系统的 UI 字体。
pub fn font_family() -> SharedString {
    let name = crate::settings::font_family_name();
    if name.trim().is_empty() {
        ".SystemUIFont".into()
    } else {
        name.into()
    }
}

// 圆角令牌（菜单卡片的半径由设置项覆盖，其余固定）
pub const RADIUS_WINDOW: f32 = 12.0;
pub const RADIUS_CARD: f32 = 12.0;
pub const RADIUS_ROW: f32 = 8.0;
pub const RADIUS_SMALL: f32 = 6.0;

// ---------------------------------------------------------------------------
// 候选色板（界面上的色块行）
// ---------------------------------------------------------------------------

/// 浅色底色候选。
pub const SURFACES_LIGHT: [&str; 6] = [
    "#ffffff", "#f7f8fa", "#f6f2ec", "#eef4f8", "#fbf2f6", "#f2f6f0",
];

/// 深色底色候选。
pub const SURFACES_DARK: [&str; 6] = [
    "#101114", "#181b21", "#1d2129", "#141a2a", "#121a17", "#221d1a",
];

/// 强调色候选（蓝 / 靛 / 紫 / 青 / 绿 / 黄 / 橙 / 红 / 粉 / 灰）。
pub const ACCENTS: [&str; 12] = [
    "#2f6feb", "#5b8cff", "#6e9bff", "#7c5cd6", "#a855c9", "#00b8d9", "#12b886", "#e8a33d",
    "#f2711c", "#e0628f", "#e5484d", "#8a93a8",
];

/// 「选中项背景」候选：前 7 个是浅色淡底，最后一个留给深色主题。
pub const SELECTED_TINTS: [&str; 8] = [
    "#eaf1ff", "#e8f0fe", "#f0ebff", "#e6f7f1", "#fff4e0", "#ffe9ef", "#dfe4ec", "#2a3350",
];

// ---------------------------------------------------------------------------
// 现成主题（一键应用）
// ---------------------------------------------------------------------------

/// 一套现成的配色。
///
/// 只给「底色 + 强调色」两项 —— 其余 5 个色位（选中项 / 文字 / 加速键 / 两种提示文字）
/// 全都靠 `Palette::build` 派生，所以点一下就能得到一整套协调的颜色，
/// 而不是把一堆手工写死的十六进制色塞进去（那样换个底色就全崩了）。
///
/// 每套都有**浅色 + 深色**两份：跟随系统时，两个模式下都是对的。
pub struct ThemePreset {
    pub name: &'static str,
    /// 一句话说明它长什么样
    pub hint: &'static str,
    /// (底色, 强调色)
    pub light: (&'static str, &'static str),
    pub dark: (&'static str, &'static str),
}

pub const PRESETS: &[ThemePreset] = &[
    ThemePreset {
        name: "默认蓝",
        hint: "白底 + 标准蓝",
        light: ("#ffffff", "#2f6feb"),
        dark: ("#181b21", "#6e9bff"),
    },
    ThemePreset {
        name: "石墨灰",
        hint: "耐看的中性灰，不抢眼",
        light: ("#f7f8fa", "#4b5563"),
        dark: ("#101114", "#9aa4b2"),
    },
    ThemePreset {
        name: "暖纸",
        hint: "米色纸感 + 琥珀强调",
        light: ("#f6f2ec", "#b45309"),
        dark: ("#221d1a", "#e8a33d"),
    },
    ThemePreset {
        name: "靛青",
        hint: "冷色调蓝灰，偏工作风",
        light: ("#eef4f8", "#2563eb"),
        dark: ("#141a2a", "#5b8cff"),
    },
    ThemePreset {
        name: "森绿",
        hint: "清爽的墨绿 + 青绿",
        light: ("#f2f6f0", "#0f766e"),
        dark: ("#121a17", "#12b886"),
    },
    ThemePreset {
        name: "玫红",
        hint: "柔和的粉底 + 玫红强调",
        light: ("#fbf2f6", "#be185d"),
        dark: ("#1d1520", "#e0628f"),
    },
    ThemePreset {
        name: "深夜紫",
        hint: "偏紫的深色，夜里不刺眼",
        light: ("#f4f2fb", "#6d28d9"),
        dark: ("#151223", "#a855c9"),
    },
    // 第 8 套：凑满 4×2，不然最后一行少一个，看着别扭
    ThemePreset {
        name: "珊瑚",
        hint: "暖橘红，比玫红更活泼",
        light: ("#fdf5f2", "#e2503a"),
        dark: ("#1d1512", "#ff8b70"),
    },
];

impl ThemePreset {
    /// 应用到一份设置上：浅色 / 深色两套一起换（当前模式不变）。
    ///
    /// 顺带把「选中项 / 提示文字」这些覆盖项清掉 —— 那多半是上一套主题留下来的，
    /// 留着会让新主题的派生结果不协调。
    pub fn apply(&self, light: &mut ThemeColors, dark: &mut ThemeColors) {
        *light = ThemeColors::new(self.light.0, self.light.1);
        *dark = ThemeColors::new(self.dark.0, self.dark.1);
    }

    /// 当前这套是不是就是这个预设（界面上打个勾）。
    pub fn matches(&self, light: &ThemeColors, dark: &ThemeColors) -> bool {
        let same = |colors: &ThemeColors, pair: (&str, &str)| {
            colors.surface.eq_ignore_ascii_case(pair.0)
                && colors.accent.eq_ignore_ascii_case(pair.1)
                && colors.selected.is_empty()
                && colors.text.is_empty()
                && colors.accel.is_empty()
                && colors.muted.is_empty()
                && colors.muted_ui.is_empty()
        };
        same(light, self.light) && same(dark, self.dark)
    }
}


// ---------------------------------------------------------------------------
// 颜色工具
// ---------------------------------------------------------------------------

/// `#rrggbb` / `rrggbb` → `0xRRGGBB`。
pub fn parse_hex(text: &str) -> Option<u32> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// `0xRRGGBB` → `#rrggbb`。
pub fn to_hex(color: u32) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (color >> 16) & 0xff,
        (color >> 8) & 0xff,
        color & 0xff
    )
}

/// 这个颜色算不算浅色（感知亮度）。
pub fn is_light(color: u32) -> bool {
    let r = ((color >> 16) & 0xff) as f32;
    let g = ((color >> 8) & 0xff) as f32;
    let b = (color & 0xff) as f32;
    (0.299 * r + 0.587 * g + 0.114 * b) / 255.0 > 0.55
}

/// `0xRRGGBB` → `0xRRGGBBFF`
fn opaque(rgb: u32) -> u32 {
    ((rgb & 0x00ff_ffff) << 8) | 0xff
}

/// 给 24 位色挂 alpha。
fn with_alpha(rgb: u32, alpha: u32) -> u32 {
    ((rgb & 0x00ff_ffff) << 8) | alpha.min(0xff)
}

/// `a` 往 `b` 揉 `t`（0..1），出入都是 24 位。
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let mut out = 0;
    for shift in [16, 8, 0] {
        let from = ((a >> shift) & 0xff) as f32;
        let to = ((b >> shift) & 0xff) as f32;
        let value = (from + (to - from) * t).round().clamp(0.0, 255.0) as u32;
        out |= value << shift;
    }
    out
}

/// 提亮（正）/ 压暗（负）。
fn tint(color: u32, amount: f32) -> u32 {
    if amount >= 0.0 {
        mix(color, 0xffffff, amount)
    } else {
        mix(color, 0x000000, -amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_surface_gets_dark_text() {
        let palette = Palette::build(&ThemeColors::new("#ffffff", "#2f6feb"));
        assert!(!palette.dark);
        assert!(!is_light(palette.text_primary), "浅底要配深色文字");
        // 窗口底、侧栏、卡片要三档层次，否则糊成一片
        assert_ne!(palette.surface, palette.surface_sunken);
        assert_ne!(palette.surface, palette.window);
        // 菜单卡片就是用户选的那个色
        assert_eq!(palette.surface_elevated, palette.surface);
    }

    #[test]
    fn dark_surface_gets_light_text() {
        let palette = Palette::build(&ThemeColors::new("#181b21", "#6e9bff"));
        assert!(palette.dark);
        assert!(is_light(palette.text_primary), "深底要配浅色文字");
        assert_ne!(palette.surface, palette.window);
    }

    /// 边框颜色：不填就按底色派生；填了就原样用，强调版再往文字色混一点。
    #[test]
    fn a_custom_border_colour_wins_over_the_derived_one() {
        let mut colors = ThemeColors::new("#ffffff", "#2f6feb");
        let derived = Palette::build(&colors).border;
        colors.border = "#ff0000".into();
        let palette = Palette::build(&colors);
        assert_eq!(palette.border, 0xff0000ff);
        assert_ne!(palette.border, derived);
        assert_ne!(palette.border_strong, palette.border, "强调版要再深一点");
        // 留空 = 回到派生值（界面上按「↺」就是这个效果）
        colors.border.clear();
        assert_eq!(Palette::build(&colors).border, derived);
    }

    #[test]
    fn rgba_and_rgb_agree_on_channel_order() {
        let palette = Palette::build(&ThemeColors::new("#ffffff", "#2f6feb"));
        assert_eq!(rgba(palette.accent), gpui::rgb(0x2f6feb));
        assert_eq!(rgba(palette.surface), gpui::rgb(0xffffff));
        assert_eq!(rgba(palette.text_primary), gpui::rgb(0x1f2329));
    }

    #[test]
    fn overrides_win_over_derivation() {
        let mut colors = ThemeColors::new("#ffffff", "#2f6feb");
        colors.text = "#102030".into();
        colors.selected = "#ffe4e6".into();
        colors.muted = "#8899aa".into();
        let palette = Palette::build(&colors);
        assert_eq!(palette.text_primary, opaque(0x102030));
        assert_eq!(palette.row_selected, opaque(0xffe4e6));
        assert_eq!(palette.text_secondary, opaque(0x8899aa));
    }

    #[test]
    fn empty_colors_still_produce_a_usable_palette() {
        let palette = Palette::build(&ThemeColors::default());
        assert_eq!(palette.surface & 0xff, 0xff);
        assert_eq!(palette.accent & 0xff, 0xff);
        assert_ne!(palette.row_selected, 0);
        assert_eq!(palette.effective_text(), "#1f2329".to_string());
    }

    #[test]
    fn settings_hint_color_can_be_set_apart_from_the_menu_one() {
        // 没单独设：设置页提示文字跟菜单提示文字一致
        let colors = ThemeColors::new("#ffffff", "#2f6feb");
        let palette = Palette::build(&colors);
        assert_eq!(palette.ui_secondary, palette.text_secondary);
        assert_eq!(palette.ui_tertiary, palette.text_tertiary);

        // 分别设：互不影响
        let mut colors = colors;
        colors.muted = "#111111".into();
        colors.muted_ui = "#999999".into();
        let palette = Palette::build(&colors);
        assert_eq!(palette.text_secondary, opaque(0x111111));
        assert_eq!(palette.ui_secondary, opaque(0x999999));
    }

    #[test]
    fn accel_color_follows_the_text_color_by_default() {
        // 默认：加速键跟文字一个颜色
        let colors = ThemeColors::new("#ffffff", "#2f6feb");
        let palette = Palette::build(&colors);
        assert_eq!(palette.accel_text, palette.text_primary);

        // 单独设了就用它自己，文字色不受影响
        let mut colors = colors;
        colors.accel = "#c62d33".into();
        let palette = Palette::build(&colors);
        assert_eq!(palette.accel_text, opaque(0xc62d33));
        assert_ne!(palette.text_primary, palette.accel_text);
    }

    #[test]
    fn hex_round_trip_and_validation() {
        assert_eq!(parse_hex("#2f6feb"), Some(0x2f6feb));
        assert_eq!(parse_hex("2f6feb"), Some(0x2f6feb));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("nope"), None);
        assert_eq!(to_hex(0x2f6feb), "#2f6feb");
    }

    #[test]
    fn every_preset_produces_a_readable_palette() {
        for preset in PRESETS {
            for (surface, accent) in [preset.light, preset.dark] {
                assert_eq!(parse_hex(surface), Some(parse_hex(surface).unwrap()), "{}", preset.name);
                let colors = ThemeColors::new(surface, accent);
                let palette = Palette::build(&colors);
                // 文字和底色的明暗必须相反，否则一片糊
                assert_ne!(
                    is_light(palette.text_primary),
                    is_light(palette.surface),
                    "{} 的 {} 配 {} 对比度反了",
                    preset.name,
                    surface,
                    accent
                );
                assert_ne!(palette.row_selected, palette.surface, "{} 的选中态看不出来", preset.name);
            }
        }
    }

    #[test]
    fn applying_a_preset_clears_the_old_overrides() {
        let mut light = ThemeColors::new("#ffffff", "#2f6feb");
        light.selected = "#ffe4e6".into();
        light.text = "#102030".into();
        let mut dark = ThemeColors::new("#181b21", "#6e9bff");

        let preset = &PRESETS[2];
        preset.apply(&mut light, &mut dark);
        // 覆盖项被清掉：新主题全靠派生，不会带着上一套的残留
        assert!(light.selected.is_empty());
        assert!(light.text.is_empty());
        assert_eq!(light.surface, preset.light.0);
        assert_eq!(dark.accent, preset.dark.1);

        // 应用之后就该认出「当前就是这一套」
        assert!(preset.matches(&light, &dark));
        assert!(!PRESETS[0].matches(&light, &dark));
    }
}

#[cfg(test)]
impl Palette {
    fn effective_text(&self) -> String {
        to_hex(self.text_primary >> 8)
    }
}
