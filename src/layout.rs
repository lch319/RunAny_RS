//! 尺寸与测量（纯函数，有单测）。
//!
//! 列宽 / 行高 / 圆角来自用户设置（`settings`），其余是固定令牌。
//! 之所以用函数而不是 `const`：设置窗口里能拖，改完立刻要生效。

use crate::menu::MenuEntry;
use crate::settings;

/// 卡片描边宽度：窗口尺寸比内容多 2px
/// 卡片描边的粗细（0 = 设置里关掉了边框）。上下留白全都由它推出来，
/// 所以关掉边框时行区域会自己往上收 1px，不会留白。
pub fn chrome() -> f32 {
    if settings::read(|settings| settings.menu_border) {
        1.0
    } else {
        0.0
    }
}

/// 行区域上下内边距
pub fn row_pad() -> f32 {
    6.0
}

/// 行区域相对卡片顶边的偏移（描边 + 内边距）
pub fn row_area_top() -> f32 {
    chrome() + row_pad()
}

/// 列底离屏幕边缘的留白（夹高度时用）
pub const SCREEN_MARGIN: f32 = 8.0;
/// 顶部/底部「还有更多」三角箭头的高度
pub const ARROW_H: f32 = 12.0;
/// 滚动条宽度
pub const SCROLLBAR_W: f32 = 4.0;
/// 滚动条的鼠标热区宽度（比看起来宽，方便拖）
pub const SCROLLBAR_HIT_W: f32 = 12.0;
/// 滚动条最小长度
pub const MIN_THUMB: f32 = 24.0;

/// Windows 无边框窗口的非客户区宽度（DWM 用来画阴影/圆角）。
/// 96 DPI 下是 8 个**逻辑**像素，随 DPI 变化但换算回逻辑像素后恒为 8。
pub const FRAME_INSET: f32 = 8.0;

/// 一列（一张卡片）的宽度
pub fn panel_width() -> f32 {
    settings::read(|settings| settings.panel_width)
}

/// 列宽模式：固定值 / 按内容自动
pub fn width_mode() -> settings::PanelWidthMode {
    settings::read(|settings| settings.panel_width_mode)
}

/// 自动列宽的上限
pub fn max_panel_width() -> f32 {
    settings::read(|settings| settings.panel_max_width)
}

/// 自动列宽的下限：再窄图标位 + 两三个字就挤没了
pub const AUTO_MIN_PANEL_W: f32 = 120.0;

/// 一行的左右留白（`mx(4) + px(8)`，两边各 12）
const ROW_INSET: f32 = 24.0;
/// 行内元素之间的间距
const ROW_GAP: f32 = 8.0;
/// 名称和加速键之间那道窄间距
const ACCEL_GAP: f32 = 3.0;

/// 单行高度
pub fn row_height() -> f32 {
    settings::read(|settings| settings.row_height)
}

/// 卡片圆角半径；`win32.rs` 的窗口裁剪半径必须和它一致
pub fn radius() -> f32 {
    settings::read(|settings| settings.radius)
}

/// 图标位（左侧小图标）的边长
pub fn badge_size() -> f32 {
    settings::read(|settings| settings.badge_size)
}

/// 菜单的不透明度（`win32::set_window_opacity` 用它）。
pub fn menu_opacity() -> f32 {
    settings::read(|settings| settings.opacity)
}

/// 分隔线占位高度（1px 线 + 上下留白）
pub fn separator_height() -> f32 {
    9.0
}

/// 同一张卡片里两列之间的留白（多列符切出来的列）
pub const COLUMN_GAP: f32 = 6.0;

pub fn entry_height(entry: &MenuEntry) -> f32 {
    match entry {
        // 多列符不占行：它的作用是「这里断开另起一列」
        MenuEntry::ColumnBreak => 0.0,
        MenuEntry::Separator => separator_height(),
        MenuEntry::Item(_) => row_height(),
    }
}

/// 这一层按多列符切成几段，返回每段在 `entries` 里的区间 `[start, end)`。
///
/// 没有多列符时就是一段 `[(0, len)]`；全被切空时也保证返回一段空的，
/// 调用方不用到处判空。
pub fn groups(entries: &[MenuEntry]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (idx, entry) in entries.iter().enumerate() {
        if matches!(entry, MenuEntry::ColumnBreak) {
            // 连着两个多列符（中间什么都没有）会切出空段，跳过
            if idx > start {
                out.push((start, idx));
            }
            start = idx + 1;
        }
    }
    if entries.len() > start {
        out.push((start, entries.len()));
    }
    if out.is_empty() {
        out.push((0, 0));
    }
    out
}

/// 这一层并排几列。
pub fn column_count(entries: &[MenuEntry]) -> usize {
    groups(entries).len()
}

/// 这一层每一段（多列符切出来的列）各自的宽度。
///
/// 固定模式：全都一样宽（设置里那个值），和以前完全一致。
/// 自动模式：按每段里最长的菜单项算，夹在 `[AUTO_MIN_PANEL_W, 最大列宽]` 之间 ——
/// 短的子菜单不再跟着最长的那个一起变宽，长名字也不会把卡片拉出屏幕。
#[cfg_attr(not(test), allow(dead_code))]
pub fn column_widths(entries: &[MenuEntry]) -> Vec<f32> {
    match width_mode() {
        settings::PanelWidthMode::Fixed => {
            vec![panel_width(); column_count(entries)]
        }
        settings::PanelWidthMode::Auto => column_widths_with(entries, &estimate_width),
    }
}

/// 同上，但文字宽度由调用方给（菜单窗口里用真实文本系统量，最准）。
pub fn column_widths_with(
    entries: &[MenuEntry],
    measure: &dyn Fn(&str, f32) -> f32,
) -> Vec<f32> {
    if width_mode() == settings::PanelWidthMode::Fixed {
        return vec![panel_width(); column_count(entries)];
    }
    let (min, _) = settings::MenuSettings::limits_panel_max_width();
    let low = AUTO_MIN_PANEL_W.max(min.min(panel_width()));
    let high = max_panel_width().max(low);
    auto_column_widths(entries, measure, low, high)
}

/// 自动列宽的纯计算部分（不读设置，方便单测）。
pub fn auto_column_widths(
    entries: &[MenuEntry],
    measure: &dyn Fn(&str, f32) -> f32,
    low: f32,
    high: f32,
) -> Vec<f32> {
    groups(entries)
        .iter()
        .map(|&(start, end)| {
            let widest = entries[start..end]
                .iter()
                .filter_map(|entry| match entry {
                    MenuEntry::Item(item) => Some(row_natural_width(item, measure)),
                    // 分隔线 / 多列符不撑宽度（它们本身就是「这里断开」）
                    _ => None,
                })
                .fold(0.0_f32, f32::max);
            widest.clamp(low, high)
        })
        .collect()
}

/// 一行至少要多宽才不挤：图标位 + 加速键 + 名称 + 快捷键 + 子菜单箭头。
fn row_natural_width(item: &crate::menu::MenuItem, measure: &dyn Fn(&str, f32) -> f32) -> f32 {
    let scale = settings::font_scale();
    let mut width = ROW_INSET;
    if item.badge.is_some() {
        width += badge_size() + ROW_GAP;
    }
    if item.accel.is_some() {
        // 加速键是固定宽的方块（见 column.rs 的 `accel_w`）
        width += 10.5 * scale + 4.0 + ACCEL_GAP;
    }
    width += measure(&item.label, 13.0 * scale);
    if let Some(shortcut) = &item.shortcut {
        width += ROW_GAP + measure(shortcut, 11.5 * scale);
    }
    if item.has_children() {
        width += ROW_GAP + measure("›", 13.0 * scale);
    }
    // 还要留滚动条热区：滚起来的时候最后一列会扣掉这 12px（见 column.rs 的 `stack_w`），
    // 不留的话长名字会正好被截掉一两个字。不滚的时候多这 12px 也看不出来。
    width + SCROLLBAR_HIT_W
}

/// 没有文本系统时（单测、设置页里的估算）的近似宽度。
///
/// 按字符类别给经验系数，再乘字号：中日韩 / 全角 = 1 em，大写和数字略窄，
/// 小写和标点更窄。中文字体大多是等宽的方块字，这个近似误差很小；
/// 真正决定菜单宽度的是 `popup` 里那套用文本系统实测的口径。
pub fn estimate_width(text: &str, size: f32) -> f32 {
    let mut em = 0.0_f32;
    for ch in text.chars() {
        em += match ch {
            '\u{4e00}'..='\u{9fff}' | '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ffef}' => 1.0,
            'M' | 'W' => 0.82,
            'i' | 'l' | 'j' | 't' | 'f' | 'I' | '.' | ',' | '\'' | '!' | '|' => 0.32,
            'A'..='Z' => 0.66,
            'm' | 'w' | '@' => 0.72,
            '0'..='9' => 0.56,
            ' ' => 0.3,
            _ => 0.52,
        };
    }
    em * size
}

/// 用真实文本系统量一段文字的宽度（结果按「字号 + 文字」缓存在线程本地）。
///
/// 菜单每开一次、每按一次方向键都会重算列宽；不缓存的话一千多项每帧都要
/// 排版一遍。缓存没有上限，但菜单里的文案基本是有限的几百条，够用了。
pub fn measure_text(
    system: &gpui::TextSystem,
    family: &gpui::SharedString,
    size: f32,
    text: &str,
) -> f32 {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<(u32, String), f32>> = RefCell::new(HashMap::new());
    }
    let key = ((size * 16.0).round() as u32, text.to_string());
    if let Some(hit) = CACHE.with(|cache| cache.borrow().get(&key).copied()) {
        return hit;
    }
    // `TextSystem` 上能直接用的只有逐字 `advance`（`layout_line` / `shape_line`
    // 在 `WindowTextSystem` 上，摆窗口的时候手里没有窗口）。逐字累加没有 kerning，
    // 但界面文案都是短标签，差不到半个像素。
    let font_id = system.resolve_font(&gpui::font(family.clone()));
    let mut width = 0.0_f32;
    for ch in text.chars() {
        let advance = system
            .advance(font_id, gpui::px(size), ch)
            .map(|size| f32::from(size.width))
            .unwrap_or(0.0);
        width += if advance > 0.0 {
            advance
        } else {
            // 这个字没量出来：按类别估一个
            let mut buf = [0_u8; 4];
            estimate_width(ch.encode_utf8(&mut buf), size)
        };
    }
    // 量不出来（空串 / 字体没装）就退回近似值，别让卡片缩成一条缝
    let width = if width > 0.0 {
        width
    } else {
        estimate_width(text, size)
    };
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 20_000 {
            cache.clear();
        }
        cache.insert(key, width);
    });
    width
}

/// 这一层卡片的总宽度（几列并排 = 各段宽度之和 + 列间留白）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn panel_span(entries: &[MenuEntry]) -> f32 {
    column_span(&column_widths(entries))
}

/// 一段宽度数组拼成一张卡片的总宽
pub fn column_span(widths: &[f32]) -> f32 {
    let sum: f32 = widths.iter().sum();
    sum + (widths.len().saturating_sub(1)) as f32 * COLUMN_GAP
}

/// 第 `idx` 行右边那一刀在卡片里是第几个像素 —— 子菜单就从这个 x 起。
///
/// 单列卡片里它就是卡片宽度（子菜单贴着卡片右边出来）；多列卡片里它是
/// 「这一行所在那一段」的右边缘，也就是说子菜单紧挨着名字，会盖住右边几段
/// （Windows 自带菜单就是这个手感：鼠标往右一挪就进子菜单，不用跨过别的列）。
pub fn item_column_right_edge(entries: &[MenuEntry], idx: usize, widths: &[f32]) -> f32 {
    if widths.is_empty() {
        return 0.0;
    }
    let (group, ..) = group_of(entries, idx);
    let group = group.min(widths.len() - 1);
    let span: f32 = widths[..=group].iter().sum();
    span + group as f32 * COLUMN_GAP
}

/// 第 `idx` 行左边那一刀在卡片里是第几个像素 —— 子菜单翻到左边时从这儿起。
///
/// 和 [`item_column_right_edge`] 对称：单列卡片里是 0（子菜单贴着卡片左边，
/// 正好是父项的左边）；多列卡片里是「这一行所在那一段」的左边缘，
/// 这样翻到左边的子菜单也是紧挨着父项那一列，而不是隔着好几列跑到卡片外面。
pub fn item_column_left_edge(entries: &[MenuEntry], idx: usize, widths: &[f32]) -> f32 {
    if widths.is_empty() {
        return 0.0;
    }
    let (group, ..) = group_of(entries, idx);
    let group = group.min(widths.len() - 1);
    let span: f32 = widths[..group].iter().sum();
    span + group as f32 * COLUMN_GAP
}

/// 第 `idx` 行落在哪一段：返回 `(段号, 段内的起止区间, 段内序号)`。
pub fn group_of(entries: &[MenuEntry], idx: usize) -> (usize, usize, usize, usize) {
    for (group, &(start, end)) in groups(entries).iter().enumerate() {
        if idx < end {
            return (group, start, end, idx.saturating_sub(start));
        }
    }
    let end = entries.len();
    (0, 0, end, idx.min(end))
}

/// 所有行的总高度（不含内边距），单列时的口径。
pub fn inner_height(entries: &[MenuEntry]) -> f32 {
    entries.iter().map(entry_height).sum()
}

/// 这一层要占的高度：多列并排时取**最高的那一列**。
pub fn content_height(entries: &[MenuEntry]) -> f32 {
    groups(entries)
        .iter()
        .map(|&(start, end)| inner_height(&entries[start..end]))
        .fold(0.0, f32::max)
}

/// 第 `idx` 行相对「它所在那一列的顶部」的 y 偏移。
pub fn row_offset(entries: &[MenuEntry], idx: usize) -> f32 {
    let (_, start, end, _) = group_of(entries, idx);
    inner_height(&entries[start..idx.min(end)])
}

/// 搜索框的高度（含它和行之间那条缝）
pub const SEARCH_H: f32 = 26.0;
/// 搜索框和第一行之间的空隙
pub const SEARCH_GAP: f32 = 5.0;

/// 菜单里那个搜索框摆在哪儿。
pub fn search_place() -> settings::SearchPlace {
    settings::read(|settings| settings.menu_search)
}

/// 第 `level` 列顶部的额外留白（只有第一列会被搜索框占掉一条）。
thread_local! {
    /// 当前这个弹出窗口是不是「正经菜单」。
    ///
    /// 右键某一项、托盘右键、第二问这些也是弹出窗口，但**不该**带搜索框和快捷栏
    static FULL_MENU: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// 标记当前弹出窗口的种类（`open_with` 里按 `Source` 设）。
pub fn set_full_menu(full: bool) {
    FULL_MENU.with(|slot| slot.set(full));
}

fn is_full_menu() -> bool {
    FULL_MENU.with(|slot| slot.get())
}

/// 快捷栏（Tab 栏）的厚度：占位模式下它会**吃掉**一条边。
pub const TAB_H: f32 = 26.0;

/// 快捷栏贴在哪条边（没开 / 不是第一列 → `None`）。
fn tab_edge(level: usize) -> Option<crate::settings::TabPosition> {
    if level != 0 || !is_full_menu() {
        return None;
    }
    crate::settings::read(|settings| {
        if settings.tab_bar.enabled {
            Some(settings.tab_bar.position)
        } else {
            None
        }
    })
}

/// 顶部要多留的厚度（快捷栏在顶部时）。
pub fn tab_top_h(level: usize) -> f32 {
    match tab_edge(level) {
        Some(crate::settings::TabPosition::Top) => TAB_H,
        _ => 0.0,
    }
}

/// 底部要多留的厚度。
pub fn tab_bottom_h(level: usize) -> f32 {
    match tab_edge(level) {
        Some(crate::settings::TabPosition::Bottom) => TAB_H,
        _ => 0.0,
    }
}

/// 左侧 / 右侧要占的宽度。
pub fn tab_side_w(level: usize) -> f32 {
    match tab_edge(level) {
        Some(crate::settings::TabPosition::Left) | Some(crate::settings::TabPosition::Right) => TAB_H,
        _ => 0.0,
    }
}

pub fn header_h(level: usize) -> f32 {
    let search = if level == 0 && search_place() == settings::SearchPlace::Top {
        SEARCH_H + SEARCH_GAP
    } else {
        0.0
    };
    search + tab_top_h(level)
}

/// 第 `level` 列底部的额外留白（搜索框放底部时）。
pub fn footer_h(level: usize) -> f32 {
    let search = if level == 0 && search_place() == settings::SearchPlace::Bottom {
        SEARCH_H + SEARCH_GAP
    } else {
        0.0
    };
    search + tab_bottom_h(level)
}

/// 第 `level` 列里第一行的顶边（相对卡片顶边）。
pub fn row_top(level: usize) -> f32 {
    row_area_top() + header_h(level)
}

/// 第 `level` 列里行区域的下边界（相对卡片顶边）。
pub fn row_bottom_edge(level: usize) -> f32 {
    row_area_top() + footer_h(level)
}

/// 搜索框在第 `level` 列里的 y（相对卡片顶边）；没开搜索框就是 None。
pub fn search_top(level: usize) -> Option<f32> {
    if level != 0 || !is_full_menu() {
        return None;
    }
    match search_place() {
        settings::SearchPlace::Off => None,
        // 快捷栏占了顶部就在它下面
        settings::SearchPlace::Top => Some(chrome() + row_pad() - 2.0 + tab_top_h(level)),
        settings::SearchPlace::Bottom => None,
    }
}

/// 搜索框底边到卡片底边的距离（放底部时用）。
pub fn search_bottom(level: usize) -> Option<f32> {
    if level != 0 || !is_full_menu() {
        return None;
    }
    match search_place() {
        settings::SearchPlace::Off => None,
        settings::SearchPlace::Bottom => Some(chrome() + row_pad() - 2.0 + tab_bottom_h(level)),
        settings::SearchPlace::Top => None,
    }
}

/// 卡片完整高度（不夹屏幕）：所有行 + 上下留白 + 描边。
/// 多列并排时按最高的那一列算。
pub fn column_height(level: usize, entries: &[MenuEntry]) -> f32 {
    content_height(entries) + row_top(level) + row_bottom_edge(level)
}

/// 给定卡片高度，第 `level` 列里能显示的行区域高度。
pub fn row_viewport(level: usize, card_height: f32) -> f32 {
    (card_height - row_top(level) - row_bottom_edge(level)).max(0.0)
}

/// 卡片最少要有这么高（至少放得下一行 + 搜索框）。
pub fn min_column_height(level: usize) -> f32 {
    row_top(level) + row_bottom_edge(level) + row_height()
}

/// 按屏幕可用高度夹一下卡片高度（至少放得下一行）。
pub fn clamp_column_height(level: usize, full_height: f32, available: f32) -> f32 {
    full_height.min(available.max(min_column_height(level)))
}

/// 内容滚动上限（多列时按最高的那一列算）。
pub fn max_scroll(level: usize, entries: &[MenuEntry], card_height: f32) -> f32 {
    (content_height(entries) - row_viewport(level, card_height)).max(0.0)
}

/// 给定滚动偏移和可视高度，算出要渲染的行区间 `[first, last)`。
/// 这就是"虚拟化"的核心：上千条也只需要画这十几行。
///
/// 传进来的是**一列**（多列时是其中一段），所以下标是段内下标。
pub fn visible_rows_in(entries: &[MenuEntry], scroll: f32, viewport: f32) -> (usize, usize) {
    if entries.is_empty() || viewport <= 0.0 {
        return (0, 0);
    }
    let mut acc = 0.0;
    let mut first = 0;
    while first < entries.len() && acc + entry_height(&entries[first]) <= scroll {
        acc += entry_height(&entries[first]);
        first += 1;
    }
    let mut last = first;
    let bottom = scroll + viewport;
    while last < entries.len() && acc < bottom {
        acc += entry_height(&entries[last]);
        last += 1;
    }
    (first, last)
}

/// 滚动条滑块：返回 (相对行区域顶部的 top, 高度)。不需要滚动时返回 None。
pub fn scroll_thumb(
    viewport: f32,
    content: f32,
    scroll: f32,
    max_scroll: f32,
) -> Option<(f32, f32)> {
    if max_scroll <= 0.5 || content <= 0.0 || viewport <= 0.0 {
        return None;
    }
    let thumb_h = (viewport * (viewport / content)).clamp(MIN_THUMB.min(viewport), viewport);
    let track = viewport - thumb_h;
    let ratio = (scroll / max_scroll).clamp(0.0, 1.0);
    Some((track * ratio, thumb_h))
}

/// 这棵菜单最多会展开出多少列（用于预热窗口池、夹 x）。
pub fn max_columns(menu: &[MenuEntry]) -> usize {
    fn walk(entries: &[MenuEntry], level: usize, deepest: &mut usize) {
        *deepest = (*deepest).max(level + 1);
        for entry in entries {
            if let MenuEntry::Item(item) = entry {
                if !item.children.is_empty() {
                    walk(&item.children, level + 1, deepest);
                }
            }
        }
    }
    let mut deepest = 1;
    walk(menu, 0, &mut deepest);
    deepest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::{sep, MenuItem};

    fn flat(n: usize) -> Vec<MenuEntry> {
        (0..n)
            .map(|i| MenuEntry::Item(MenuItem::new("x", format!("第 {i} 项"))))
            .collect()
    }

    #[test]
    fn visible_rows_only_covers_viewport() {
        let entries = flat(1000);
        // 视口 300，未滚动 -> 只画能放下的那几行（行高归设置管，别写死）
        let per = (300.0 / row_height()).ceil() as usize;
        assert_eq!(visible_rows_in(&entries, 0.0, 300.0), (0, per));
        // 滚到 150 -> 从第 floor(150/行高) 行开始，仍然 per 行
        let first = (150.0 / row_height()).floor() as usize;
        assert_eq!(visible_rows_in(&entries, 150.0, 300.0), (first, first + per));
        // 滚到最底（卡片高 = 视口 + 上下留白，跟有没有搜索框无关）
        let max = max_scroll(0, &entries, 300.0 + row_top(0) + row_bottom_edge(0));
        let (first, last) = visible_rows_in(&entries, max, 300.0);
        assert_eq!(last, 1000);
        assert!(last - first <= per + 1);
    }

    #[test]
    fn separators_count_in_heights() {
        let entries = vec![
            MenuEntry::Item(MenuItem::new("a", "a")),
            sep(),
            MenuEntry::Item(MenuItem::new("b", "b")),
        ];
        assert_eq!(
            inner_height(&entries),
            row_height() * 2.0 + separator_height()
        );
        assert_eq!(row_offset(&entries, 2), row_height() + separator_height());
    }

    #[test]
    fn scroll_thumb_tracks_position() {
        // 视口 300，内容 3000 -> 滑块长度 30，滚到一半(1350/2700) -> top = (300-30)/2
        let (top, h) = scroll_thumb(300.0, 3000.0, 1350.0, 2700.0).unwrap();
        assert!((h - 30.0).abs() < 0.01);
        assert!((top - 135.0).abs() < 0.01);
        // 不需要滚动就没有滑块
        assert!(scroll_thumb(300.0, 300.0, 0.0, 0.0).is_none());
        // 内容极长时滑块不小于 MIN_THUMB
        let (_, h) = scroll_thumb(300.0, 100000.0, 0.0, 99700.0).unwrap();
        assert!((h - MIN_THUMB).abs() < 0.01);
    }

    #[test]
    fn tall_content_is_clamped_to_screen() {
        let entries = flat(1000);
        let full = column_height(0, &entries);
        // 1000 项无论如何都远超一屏（别写成具体像素，行高改了就得跟着改）
        assert!(full > 20_000.0);
        assert_eq!(clamp_column_height(0, full, 800.0), 800.0);
        assert_eq!(
            max_scroll(0, &entries, 800.0),
            inner_height(&entries) - row_viewport(0, 800.0)
        );
    }

    /// 搜索框只是把行区域往下（或往上）挪一条，不该改变「总高 = 行高之和 + 上下留白」这个关系。
    #[test]
    fn column_height_accounts_for_header_and_footer() {
        let entries = flat(3);
        for level in 0..3 {
            let expect = inner_height(&entries) + row_top(level) + row_bottom_edge(level);
            assert_eq!(column_height(level, &entries), expect);
            // 视口 + 上下留白 = 卡片高，一条都不能算丢
            assert_eq!(
                row_viewport(level, column_height(level, &entries)),
                inner_height(&entries)
            );
        }
        // 子菜单那几列永远不带搜索框
        assert_eq!(header_h(1), 0.0);
        assert_eq!(footer_h(1), 0.0);
        assert_eq!(row_top(1), row_area_top());
        assert_eq!(search_top(1), None);
        assert_eq!(search_bottom(1), None);
    }

    /// 多列符把一层切成几段：段区间、每段自己的 y 偏移、卡片总宽都跟着变。
    #[test]
    fn a_column_break_splits_the_level() {
        let entries = vec![
            MenuEntry::Item(MenuItem::new("a", "a")),
            MenuEntry::Item(MenuItem::new("b", "b")),
            MenuEntry::ColumnBreak,
            MenuEntry::Item(MenuItem::new("c", "c")),
        ];
        // 多列符自己不占行，也不能被选中
        assert_eq!(entry_height(&MenuEntry::ColumnBreak), 0.0);
        assert!(!MenuEntry::ColumnBreak.is_selectable());
        assert_eq!(groups(&entries), vec![(0, 2), (3, 4)]);
        assert_eq!(column_count(&entries), 2);
        // 卡片宽 = 各段宽度之和 + 段间留白（列宽多少由设置决定，这里只验口径）
        assert_eq!(panel_span(&entries), column_span(&column_widths(&entries)));
        assert_eq!(
            column_span(&[panel_width(), panel_width()]),
            panel_width() * 2.0 + COLUMN_GAP
        );
        // 每列都从自己那一列的顶上排起（不是从整层的顶上累加）
        assert_eq!(row_offset(&entries, 0), 0.0);
        assert_eq!(row_offset(&entries, 1), row_height());
        assert_eq!(row_offset(&entries, 3), 0.0);
        // 高度取最高的那一列（2 行 vs 1 行）
        assert_eq!(content_height(&entries), row_height() * 2.0);
        assert_eq!(
            column_height(0, &entries),
            content_height(&entries) + row_top(0) + row_bottom_edge(0)
        );
        // 只有一列时和单列口径完全一致
        let single = flat(3);
        assert_eq!(content_height(&single), inner_height(&single));
        // 固定列宽下总宽 = 几列面板宽 + 列间留白
        let fixed = vec![panel_width(); column_count(&entries)];
        assert_eq!(column_span(&fixed), panel_width() * 2.0 + COLUMN_GAP);
    }

    /// 子菜单的起点：父项在哪一段，就从那一段的右边缘展开（多列时会盖住右边几段）。
    #[test]
    fn the_submenu_starts_at_the_right_edge_of_the_item_column() {
        let entries = vec![
            MenuEntry::Item(MenuItem::new("a", "a")),
            MenuEntry::Item(MenuItem::new("b", "b")),
            MenuEntry::ColumnBreak,
            MenuEntry::Item(MenuItem::new("c", "c")),
            MenuEntry::ColumnBreak,
            MenuEntry::Item(MenuItem::new("d", "d")),
        ];
        let widths = vec![100.0, 140.0, 120.0];
        // 第 0 段里的行（0 / 1）：贴着第 1 段的右边缘
        assert_eq!(item_column_right_edge(&entries, 0, &widths), 100.0);
        assert_eq!(item_column_right_edge(&entries, 1, &widths), 100.0);
        // 第 2 段里的行（3）：第 1 段 + 间距 + 第 2 段
        assert_eq!(
            item_column_right_edge(&entries, 3, &widths),
            100.0 + COLUMN_GAP + 140.0
        );
        // 最后一段：等于整张卡片的宽度
        assert_eq!(item_column_right_edge(&entries, 5, &widths), column_span(&widths));
        // 单列卡片：子菜单就是贴着卡片右边（和以前一样）
        let single = flat(3);
        assert_eq!(
            item_column_right_edge(&single, 1, &column_widths(&single)),
            panel_span(&single)
        );
    }

    /// 翻到左边的子菜单要贴**父项那一列的左边缘**（不是整张卡片的左边）。
    #[test]
    fn the_submenu_left_edge_follows_the_items_column() {
        let entries = vec![
            MenuEntry::Item(MenuItem::new("a", "a")),
            MenuEntry::ColumnBreak,
            MenuEntry::Item(MenuItem::new("b", "b")),
            MenuEntry::ColumnBreak,
            MenuEntry::Item(MenuItem::new("c", "c")),
        ];
        let widths = vec![100.0, 140.0, 120.0];
        // 第 1 段里的行：从 0 起（卡片左边）
        assert_eq!(item_column_left_edge(&entries, 0, &widths), 0.0);
        // 第 2 段：第 1 段宽度 + 列间距
        assert_eq!(
            item_column_left_edge(&entries, 2, &widths),
            100.0 + COLUMN_GAP
        );
        // 第 3 段：前两段 + 两个间距
        assert_eq!(
            item_column_left_edge(&entries, 4, &widths),
            100.0 + 140.0 + 2.0 * COLUMN_GAP
        );
        // 和右边缘的关系：右边缘 = 左边缘 + 这一段自己的宽度
        assert_eq!(
            item_column_right_edge(&entries, 4, &widths),
            item_column_left_edge(&entries, 4, &widths) + 120.0
        );
        // 单列卡片：左边缘就是 0
        let single = flat(3);
        assert_eq!(
            item_column_left_edge(&single, 1, &column_widths(&single)),
            0.0
        );
    }

    /// 自动列宽：每一段按自己的内容算宽，短的不跟着长的一起变宽。
    #[test]
    fn auto_width_fits_each_column_to_its_own_items() {
        let short = MenuEntry::Item(MenuItem::new("a", "存"));
        let long = MenuEntry::Item(MenuItem::new("b", "保存桌面壁纸到图片文件夹"));
        let entries = vec![
            short.clone(),
            MenuEntry::ColumnBreak,
            long.clone(),
        ];
        let widths = auto_column_widths(&entries, &estimate_width, 120.0, 400.0);
        assert_eq!(widths.len(), 2);
        assert!(
            widths[1] > widths[0],
            "长名字那一列要更宽：{widths:?}"
        );
        // 单列里只有一项时按它自己算；相同内容的两列一样宽
        let same = auto_column_widths(&[long.clone(), MenuEntry::ColumnBreak, long], &estimate_width, 120.0, 400.0);
        assert_eq!(same[0], same[1]);
        // 超长名字被「最大列宽」截住，不会把菜单拉出屏幕
        let huge = MenuEntry::Item(MenuItem::new("c", "超级长的菜单项名字".repeat(20)));
        let capped = auto_column_widths(&[huge], &estimate_width, 120.0, 300.0);
        assert_eq!(capped[0], 300.0);
        // 极短的名字也不会窄到放不下图标位
        let tiny = auto_column_widths(&[short], &estimate_width, 120.0, 400.0);
        assert_eq!(tiny[0], 120.0);
    }

    /// 名称长度估算：中文按方块字、英文按比例，别把两者算成一样宽。
    #[test]
    fn estimate_width_knows_cjk_from_latin() {
        let cjk = estimate_width("保存文件", 13.0);
        let latin = estimate_width("Save", 13.0);
        assert!(cjk > latin, "{cjk} vs {latin}");
        assert!(estimate_width("iiii", 13.0) < estimate_width("WWWW", 13.0));
    }

    /// 开头 / 结尾 / 连着两个多列符都会切出空段，`groups` 不能吐空区间。
    #[test]
    fn empty_segments_are_skipped() {
        let entries = vec![
            MenuEntry::ColumnBreak,
            MenuEntry::Item(MenuItem::new("a", "a")),
            MenuEntry::ColumnBreak,
            MenuEntry::ColumnBreak,
        ];
        assert_eq!(groups(&entries), vec![(1, 2)]);
        // 极端情况：整层只有一个多列符 —— 也得给出一段，别让调用方拿到空 vec
        assert_eq!(groups(&[MenuEntry::ColumnBreak]), vec![(0, 0)]);
    }

    /// 一条命中都没有时，卡片也必须留得下一行（放「没有匹配的项」）。
    #[test]
    fn empty_column_still_fits_one_row() {
        let min = min_column_height(0);
        assert!(min >= row_top(0) + row_height());
        // `popup::sync` 就是这么兜的：内容高度和「最少一行」取大的那个
        assert_eq!(column_height(0, &[]).max(min), min);
    }
}
