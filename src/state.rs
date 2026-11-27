//! 菜单状态：菜单树 + 当前路径（每一层高亮的项）+ 每一列的滚动偏移。
//!
//! 视图（每一列一个窗口）只读它，事件处理完再调 `popup::sync()` 让窗口跟上。

use gpui::SharedString;

use crate::layout;
use crate::menu::{self, MenuEntry, MenuItem};
use crate::settings::{self, ActionStep};

pub struct MenuState {
    pub menu: Vec<MenuEntry>,
    /// 没被搜索框筛过的原始树（换搜索词时从这儿重新筛）
    base: Vec<MenuEntry>,
    /// 搜索框里的词（空串 = 没在搜）
    query: String,
    /// 从根到「当前高亮项」的索引路径；`path.len()` == 应显示的列数
    pub path: Vec<usize>,
    /// 每一列的行区域滚动偏移（像素），长度与 `path` 一致
    pub scroll: Vec<f32>,
    /// 构建这棵树时的设置版本（设置一变就重建）
    pub revision: u64,
}

impl MenuState {
    pub fn new(menu: Vec<MenuEntry>) -> Self {
        let mut state = Self {
            menu: menu.clone(),
            base: menu,
            query: String::new(),
            path: vec![0],
            scroll: vec![0.0],
            revision: settings::revision(),
        };
        state.normalize();
        state
    }

    /// 搜索框里的词。
    pub fn query(&self) -> &str {
        &self.query
    }

    /// 换搜索词：从原始树重新筛一遍，选中路径回到第一项。
    ///
    /// 返回这次有没有真的变（词没变 / 筛出来的结果一样就什么都不做）。
    pub fn set_query(&mut self, query: impl Into<String>) -> bool {
        let query: String = query.into();
        if query == self.query {
            return false;
        }
        self.query = query;
        self.apply_query();
        true
    }

    /// 按当前 `query` 重算 `menu`（`base` 不动）。
    fn apply_query(&mut self) {
        let needle = self.query.trim().to_lowercase();
        if needle.is_empty() {
            self.menu = self.base.clone();
        } else {
            self.menu = filter_entries(&self.base, &needle);
        }
        self.path.truncate(1);
        self.path[0] = 0;
        self.scroll.truncate(1);
        self.scroll[0] = 0.0;
        self.normalize();
    }

    /// 无条件重建菜单树。
    ///
    /// 每次弹出菜单都走这里：设置可能改过（看 revision 也行），但「抓到的选中内容」
    /// 是菜单第一项，它每次弹出都可能不一样，没法用 revision 表达。
    ///
    /// `filter` = 这次要不要按选中内容筛（点「显示全部菜单」重开时是 `false`）。
    /// 同 `rebuild`，但显式指定「这一份算不算完整菜单」。
    ///
    /// 不算完整（托盘菜单 / 只弹一支）时也会补一条「显示全部菜单」，
    /// 让任何「看不到全部」的形态都有出口。
    pub fn rebuild_with(&mut self, filter: bool, full: bool) {
        self.base = sample_menu_ex(filter, !full);
        self.menu = self.base.clone();
        self.revision = settings::revision();
        // 重新弹菜单时搜索框是空的，但状态里可能还留着上一轮搜过的词
        self.query.clear();
        self.reset();
    }

    /// 复用同一份菜单，只把选中路径和滚动位置复位（省掉每次重建几千条菜单）。
    pub fn reset(&mut self) {
        self.path.truncate(1);
        self.path[0] = 0;
        self.scroll.truncate(1);
        self.scroll[0] = 0.0;
        self.normalize();
    }

    /// 整棵树换成另一棵（快捷键「只弹某一支」用），顺便把选中路径复位到第一项。
    pub fn replace(&mut self, menu: Vec<MenuEntry>) {
        self.base = menu;
        self.menu = self.base.clone();
        self.revision = settings::revision();
        self.query.clear();
        self.reset();
    }

    pub fn entries(&self, level: usize) -> &[MenuEntry] {
        let mut list: &[MenuEntry] = &self.menu;
        for depth in 0..level {
            let Some(&idx) = self.path.get(depth) else {
                return &[];
            };
            let Some(item) = list.get(idx).and_then(|entry| entry.item()) else {
                return &[];
            };
            list = &item.children;
        }
        list
    }

    pub fn item_at(&self, level: usize, idx: usize) -> Option<&MenuItem> {
        self.entries(level).get(idx).and_then(|entry| entry.item())
    }

    pub fn has_children(&self, level: usize, idx: usize) -> bool {
        self.item_at(level, idx)
            .map(|item| item.has_children())
            .unwrap_or(false)
    }

    fn first_child(&self, level: usize, idx: usize) -> Option<usize> {
        self.item_at(level, idx)
            .and_then(|item| item.children.iter().position(|entry| entry.is_selectable()))
    }

    pub fn columns(&self) -> usize {
        self.path.len()
    }

    pub fn scroll_of(&self, level: usize) -> f32 {
        self.scroll.get(level).copied().unwrap_or(0.0)
    }

    pub fn set_scroll(&mut self, level: usize, value: f32) {
        if let Some(slot) = self.scroll.get_mut(level) {
            *slot = value;
        }
    }

    fn current_level(&self) -> usize {
        self.path.len().saturating_sub(1)
    }

    /// 修掉非法状态（越界、指向分隔线、父项没子菜单却展开了下一列）。
    fn normalize(&mut self) {
        if self.path.is_empty() {
            self.path.push(0);
        }
        self.scroll.resize(self.path.len(), 0.0);

        let mut level = 0;
        while level < self.path.len() {
            let count = self.entries(level).len();
            if count == 0 {
                self.path.truncate(level + 1);
                break;
            }
            let current = self.path[level];
            if current >= count || !self.entries(level)[current].is_selectable() {
                let fallback = self
                    .entries(level)
                    .iter()
                    .position(|entry| entry.is_selectable());
                match fallback {
                    Some(idx) => self.path[level] = idx,
                    None => {
                        self.path.truncate(level + 1);
                        break;
                    }
                }
            }
            if level + 1 < self.path.len() && !self.has_children(level, self.path[level]) {
                self.path.truncate(level + 1);
            }
            level += 1;
        }
        self.scroll.truncate(self.path.len());
        self.scroll.resize(self.path.len(), 0.0);
    }

    /// 把某一列滚动 `delta` 像素（滚轮 / 拖动滚动条 / 点击三角箭头都走这里）。
    /// 滚完如果高亮项跑出可视区，就把高亮挪到可视区里第一个可选项，
    /// 免得"看不见的项被 Enter 触发"。
    pub fn scroll_by(&mut self, level: usize, delta: f32, card_height: f32) -> bool {
        let max = layout::max_scroll(level, self.entries(level), card_height);
        let before = self.scroll_of(level);
        let next = (before + delta).clamp(0.0, max);
        if (next - before).abs() < 0.01 {
            return false;
        }
        self.set_scroll(level, next);
        self.pull_selection_into_view(level, card_height);
        true
    }

    /// 滚动位置变了以后调用：高亮项若已完全离开可视区，换成可视区里第一个可选项。
    fn pull_selection_into_view(&mut self, level: usize, card_height: f32) {
        let viewport = layout::row_viewport(level, card_height);
        if viewport <= 0.0 {
            return;
        }
        let scroll = self.scroll_of(level);
        let Some(&idx) = self.path.get(level) else {
            return;
        };
        let out_of_view = {
            let entries = self.entries(level);
            let Some(entry) = entries.get(idx) else {
                return;
            };
            let top = layout::row_offset(entries, idx);
            top < scroll - 0.5 || top + layout::entry_height(entry) > scroll + viewport + 0.5
        };
        if !out_of_view {
            return;
        }
        let target = {
            let entries = self.entries(level);
            // 这一行在哪一列里：可见区间要按那一列自己算（多列符切出来的列各排各的）
            let (_, start, end, _) = layout::group_of(entries, idx);
            let group = &entries[start..end];
            let (first, last) = layout::visible_rows_in(group, scroll, viewport);
            group[first..last]
                .iter()
                .position(|entry| entry.is_selectable())
                .map(|offset| start + first + offset)
        };
        if let Some(target) = target {
            self.path[level] = target;
        }
    }

    /// 把某一列滚到「让第 `idx` 行完整可见」。
    pub fn ensure_visible(&mut self, level: usize, card_height: f32) {
        let viewport = layout::row_viewport(level, card_height);
        if viewport <= 0.0 {
            return;
        }
        let Some(&idx) = self.path.get(level) else {
            return;
        };
        let (top, row_h, max_scroll) = {
            let entries = self.entries(level);
            let Some(entry) = entries.get(idx) else {
                return;
            };
            (
                layout::row_offset(entries, idx),
                layout::entry_height(entry),
                layout::max_scroll(level, entries, card_height),
            )
        };
        let mut scroll = self.scroll_of(level);
        if top < scroll {
            scroll = top;
        } else if top + row_h > scroll + viewport {
            scroll = top + row_h - viewport;
        }
        self.set_scroll(level, scroll.clamp(0.0, max_scroll));
    }

    /// 鼠标悬停到第 `level` 列的第 `idx` 项；返回是否有变化。
    /// 指针移到菜单**空白处**（没有落在任何一行上）时调：把比 `level` 更深的
    /// 展开列收起来。
    ///
    /// 多列菜单里，子菜单展开后是盖在右边列上的；光标挪到空白处如果不收，
    /// 用户就没法点被盖住的那几项。只收起「比当前列更深」的部分，当前列的选择
    /// 保持不动 —— 移开再移回来，位置不跳。
    pub fn collapse_below(&mut self, level: usize) -> bool {
        if self.path.len() <= level + 1 {
            return false;
        }
        self.path.truncate(level + 1);
        self.scroll.truncate(level + 1);
        true
    }

    pub fn hover(&mut self, level: usize, idx: usize) -> bool {
        let has_children = self.has_children(level, idx);
        let already = self.path.get(level) == Some(&idx);
        if already && (!has_children || self.path.len() > level + 1) {
            return false;
        }
        self.path.truncate(level + 1);
        self.scroll.truncate(level + 1);
        self.path[level] = idx;
        if has_children {
            if let Some(first) = self.first_child(level, idx) {
                self.path.push(first);
                self.scroll.push(0.0);
            }
        }
        true
    }

    /// 键盘上下移动当前列的高亮项；返回是否有变化（移动了，或因为翻页滚动了）。
    pub fn move_selection(&mut self, delta: isize, card_height: f32) -> bool {
        let level = self.current_level();
        let count = self.entries(level).len() as isize;
        if count == 0 {
            return false;
        }
        let before_scroll = self.scroll_of(level);
        let before_idx = self.path[level];
        let mut idx = before_idx as isize;
        loop {
            idx += delta;
            if idx < 0 || idx >= count {
                break; // 到边界：不动选中项，但仍把滚动位置校正一下
            }
            if self.entries(level)[idx as usize].is_selectable() {
                self.path[level] = idx as usize;
                break;
            }
        }
        self.ensure_visible(level, card_height);
        self.path[level] != before_idx || (self.scroll_of(level) - before_scroll).abs() > 0.01
    }

    /// 展开当前项的子菜单；返回是否有变化。
    pub fn expand(&mut self) -> bool {
        let level = self.current_level();
        let idx = self.path[level];
        if !self.has_children(level, idx) {
            return false;
        }
        match self.first_child(level, idx) {
            Some(first) => {
                self.path.push(first);
                self.scroll.push(0.0);
                true
            }
            None => false,
        }
    }

    /// 收回到上一列；返回是否有变化。
    pub fn collapse(&mut self) -> bool {
        if self.path.len() > 1 {
            self.path.pop();
            self.scroll.pop();
            true
        } else {
            false
        }
    }

    /// 按加速键选中一项：从根列到当前列挨个找，找到就把路径收到那一项上。
    ///
    /// 只在**已经打开的列**里找（和系统菜单一样，看不见的项不会被快捷键触发）。
    /// 返回是否有变化 —— 之后交给 `activate()` 决定是展开子菜单还是执行动作。
    pub fn select_by_accel(&mut self, key: &str) -> bool {
        let Some(target) = crate::settings::accel_char(key) else {
            return false;
        };
        for level in 0..self.path.len() {
            let hit = self.entries(level).iter().position(|entry| {
                entry
                    .item()
                    .and_then(|item| item.accel.as_deref())
                    .map(|accel| accel.eq_ignore_ascii_case(&target.to_string()))
                    .unwrap_or(false)
            });
            if let Some(idx) = hit {
                self.path.truncate(level + 1);
                self.scroll.truncate(level + 1);
                self.path[level] = idx;
                return true;
            }
        }
        false
    }

    /// 执行当前高亮项：有子菜单就展开，否则返回 (id, 文案, 动作) 交给业务。
    pub fn activate(&mut self) -> Option<(SharedString, SharedString, Vec<ActionStep>)> {
        let level = self.current_level();
        let idx = self.path[level];
        if self.has_children(level, idx) {
            self.expand();
            return None;
        }
        self.item_at(level, idx)
            .map(|item| (item.id.clone(), item.label.clone(), item.actions.clone()))
    }

    /// 点击某一项：父项展开、叶子直接执行。
    pub fn click(
        &mut self,
        level: usize,
        idx: usize,
    ) -> Option<(SharedString, SharedString, Vec<ActionStep>)> {
        self.path.truncate(level + 1);
        self.scroll.truncate(level + 1);
        self.path[level] = idx;
        if self.has_children(level, idx) {
            if let Some(first) = self.first_child(level, idx) {
                self.path.push(first);
                self.scroll.push(0.0);
            }
            return None;
        }
        self.item_at(level, idx)
            .map(|item| (item.id.clone(), item.label.clone(), item.actions.clone()))
    }
}

/// 当前要显示的菜单：由设置里的菜单项构建；
/// 设了环境变量 `GPUI_MENU_ITEMS=2000` 就生成对应条数的压测菜单（跳开设置）。
///
/// 「弹出前抓到的选中内容」会作为第一项插进来（点一下就是复制回剪贴板），
/// 同时它的**类型**（文字 / 文件 / 图片）决定菜单里留下哪些项（见 `settings::menu_for`）。
///
/// `filter = false` 时不按选中内容筛（「显示全部菜单」点下去就是重开一次这样的菜单），
/// 但第一项的「选中内容」还在 —— 用户只是想看全部，不想丢掉刚抓到的东西。
pub fn sample_menu(filter: bool) -> Vec<MenuEntry> {
    // 兼容老调用：只要筛过，就等于「不是完整菜单」
    sample_menu_ex(filter, filter)
}

/// `show_all` = 要不要在末尾/开头补一条「显示全部菜单」。
///
/// 以前只在「筛过」的时候补；现在**任何不是完整菜单的形态**都补
/// （托盘菜单、只弹一支……），保证用户总有出口能看到全部。
pub fn sample_menu_ex(filter: bool, show_all: bool) -> Vec<MenuEntry> {
    match std::env::var("GPUI_MENU_ITEMS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        Some(count) if count > 0 => menu::demo_menu(count),
        _ => {
            let context = crate::selection::current_context();
            let context = if filter { context } else { None };
            let mut filtered = context.is_some();
            let mut items = settings::menu_items_for(context.clone());
            // 有选中内容、但一条匹配的项都没有：把**整份菜单**摆出来，而不是
            // 弹一张只剩「复制」的空卡片。
            //
            // 以前这里塞一条「没有『文字』菜单项 · 去设置」的提示。问题是从 RunAny
            // 导入的菜单几乎全是「通用项」（通用项有选中内容时不显示），一选中文字
            // 整张菜单就空了 —— 用户要的是「原版那样全都在」，不是一张空菜单。
            if filtered && items.is_empty() {
                items = settings::menu_items_for(None);
                filtered = false;
            }
            let entry = crate::selection::menu_entry();
            if let Some(entry) = entry {
                // 抓到的内容和真正的菜单之间隔一根线，看得清楚哪条是「临时项」
                let with_separator = settings::read(|settings| settings.show_separators);
                items.insert(0, entry);
                if with_separator && items.len() > 1 {
                    items.insert(1, MenuEntry::Separator);
                }
            }
            // 「最近运行」：按设置决定要不要在筛过的菜单里也出现
            let allow_recent = !filtered || settings::read(|settings| settings.recent.in_filtered);
            if allow_recent {
                settings::append_recent(&mut items);
            }
            // 不是完整菜单就补一条「显示全部菜单」：位置、开不开都在设置里
            if filtered || show_all {
                settings::append_show_all(&mut items);
            }
            items
        }
    }
}

/// 按搜索词剪一棵菜单树（和「设置 → 菜单项」页那套搜索同一套规则：
/// 名称 / 备注 / 快捷键提示 / 加速键 / 图标 / 动作参数，都不分大小写）。
///
/// - 自己命中的项：整支留着（子菜单照旧，点进去还是全的）；
/// - 只有子孙命中的项：留着，但只带命中的那些子孙；
/// - 谁都没命中的：整支不要；分隔线在搜索时没有意义，一律跳过。
fn filter_entries(entries: &[MenuEntry], needle: &str) -> Vec<MenuEntry> {
    let mut out = Vec::new();
    for entry in entries {
        let MenuEntry::Item(item) = entry else {
            continue;
        };
        let self_hit = item_hits(item, needle);
        if self_hit {
            out.push(MenuEntry::Item(item.clone()));
            continue;
        }
        let children = filter_entries(&item.children, needle);
        if children.is_empty() {
            continue;
        }
        let mut kept = item.clone();
        kept.children = children;
        out.push(MenuEntry::Item(kept));
    }
    out
}

fn item_hits(item: &MenuItem, needle: &str) -> bool {
    let hit = |text: &str| !text.is_empty() && text.to_lowercase().contains(needle);
    hit(&item.label)
        || item.note.as_ref().map(|note| hit(note)).unwrap_or(false)
        || item
            .shortcut
            .as_ref()
            .map(|text| hit(text))
            .unwrap_or(false)
        || item.accel.as_ref().map(|text| hit(text)).unwrap_or(false)
        || item.badge.as_ref().map(|text| hit(text)).unwrap_or(false)
        || item
            .actions
            .iter()
            .any(|step| hit(&step.value) || step.kind.label().contains(needle))
}

pub fn example() -> MenuState {
    MenuState::new(sample_menu(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Vec<MenuEntry> {
        vec![
            MenuEntry::Item(
                MenuItem::new("edit", "编辑")
                    .submenu(vec![
                        MenuEntry::Item(MenuItem::new("copy", "复制").shortcut("Ctrl+C")),
                        MenuEntry::Item(MenuItem::new("paste", "粘贴").shortcut("Ctrl+V")),
                    ]),
            ),
            MenuEntry::Separator,
            MenuEntry::Item(MenuItem::new("settings", "打开设置").note("调外观")),
        ]
    }

    #[test]
    fn the_search_matches_the_label() {
        let hits = filter_entries(&tree(), "设置");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].item().unwrap().id.as_ref(), "settings");
    }

    #[test]
    fn a_parent_survives_when_only_a_child_matches() {
        let hits = filter_entries(&tree(), "ctrl+c");
        assert_eq!(hits.len(), 1);
        let children = &hits[0].item().unwrap().children;
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].item().unwrap().label.as_ref(), "复制");
    }

    #[test]
    fn a_matching_parent_keeps_the_whole_branch() {
        let hits = filter_entries(&tree(), "编辑");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].item().unwrap().children.len(), 2);
    }

    #[test]
    fn separators_and_misses_are_dropped() {
        assert!(filter_entries(&tree(), "不存在的词").is_empty());
    }

    #[test]
    fn the_note_is_searchable() {
        let hits = filter_entries(&tree(), "外观");
        assert_eq!(hits.len(), 1);
    }
}
