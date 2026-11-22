//! 菜单树的**树形编辑器**，三处共用：
//!
//! - 设置 → 菜单项（根层，跳过托盘那一支）
//! - 设置 → 托盘（`__tray__` 那一支的子项）
//! - 编辑窗口右侧（某个子菜单的子项）
//!
//! 之前这三处是三套代码：设置页那套能拖动排序、能加子菜单、能点行编辑；
//! 编辑窗口右侧只有「第 1 项 / 第 2 项」的只读列表 —— 同一个子菜单，
//! 从设置页进去能做的事，从编辑窗口进去就做不了，很别扭。
//!
//! 所以这里把它做成一个独立组件（`Entity<TreeView>`），谁要用就 `Scope` 指定
//! 「编哪一段」，其余全一样：拖动排序（行中段放进去 / 上下沿前后插）、点行选中、
//! ✎ 编辑、显隐开关、删除、展开收起、搜索、快捷筛选。
//!
//! ## 路径
//!
//! 行里存的 `path` 始终是**相对根层**的下标路径（`settings` 里所有树操作都吃这个），
//! `Scope` 只决定「从哪一段开始铺」以及「新项往哪个容器里放」。
//! 这样 `move_node` / `node_mut` / `find_path` 这些现成函数一行都不用改。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    canvas, div, px, AnyElement, Bounds, ClickEvent, Context, Div, Entity, ExternalPaths,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString,
    Subscription, Window,
};

use crate::settings::{self, ItemKind};
use crate::text_input::TextInput;
use crate::theme;

/// 这一次编的是菜单树的哪一段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// 根层（自动跳过托盘那一支 —— 它在「托盘」页单独编）
    Root,
    /// 托盘图标的右键菜单
    Tray,
    /// 某一支子菜单的子项
    Submenu(String),
}

impl Scope {
    fn same(&self, other: &Scope) -> bool {
        self == other
    }

    /// 拼元素 id 用的**跨帧稳定**标识。
    ///
    /// 同一页里可能同时挂着两棵树（设置页一棵、「编辑菜单项」窗口里还有一棵
    /// 子菜单的树），而 gpui 的交互状态是按 `ElementId` 挂的 —— 两棵树都叫
    /// `ghost-粘贴` 的话，点哪一棵都只有第一棵收到点击，另一棵看着像坏了一样。
    fn key(&self) -> String {
        match self {
            Scope::Root => "root".to_string(),
            Scope::Tray => "tray".to_string(),
            Scope::Submenu(id) => format!("sub-{id}"),
        }
    }

    /// 这一段的容器路径（`[]` = 根）。找不到（托盘没建 / 子菜单被删了）就是 `None`。
    fn container(&self, settings: &settings::MenuSettings) -> Option<Vec<usize>> {
        match self {
            Scope::Root => Some(Vec::new()),
            Scope::Tray => settings.find_path(settings::TRAY_ID),
            Scope::Submenu(id) => settings.find_path(id),
        }
    }
}

/// 快捷筛选：和搜索框叠加生效（两个条件都得满足）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickFilter {
    All,
    /// 配了**真的**触发快捷键（`item.hotkey`；只有提示 `shortcut` 的不算）
    Hotkey,
    /// 配了**局部生效**的快捷键（只在某些窗口里才响应）
    LocalHotkey,
    /// 配了热字符串（在别的程序里敲一串字就执行这一项）
    Hotstring,
    /// 配了「只在选中某些内容时才出现」（单独指定了类型，或写了后缀限制）
    Contextual,
    /// 写了备注
    Note,
    /// 子菜单（有子项）
    Submenu,
    /// 在菜单里被关掉了的
    Hidden,
}

impl QuickFilter {
    const ALL: [QuickFilter; 8] = [
        QuickFilter::All,
        QuickFilter::Hotkey,
        QuickFilter::LocalHotkey,
        QuickFilter::Hotstring,
        QuickFilter::Contextual,
        QuickFilter::Note,
        QuickFilter::Submenu,
        QuickFilter::Hidden,
    ];

    pub fn label(self) -> &'static str {
        crate::i18n::t(match self {
            QuickFilter::All => "全部",
            QuickFilter::Hotkey => "有快捷键",
            QuickFilter::LocalHotkey => "局部快捷键",
            QuickFilter::Hotstring => "有热字符串",
            QuickFilter::Contextual => "有适用选中内容",
            QuickFilter::Note => "有备注",
            QuickFilter::Submenu => "子菜单",
            QuickFilter::Hidden => "已隐藏",
        })
    }

    /// 这一项自己满不满足（不算后代）。
    fn hits(self, item: &settings::ItemSetting) -> bool {
        match self {
            QuickFilter::All => true,
            // 只算「真的注册了」的那种：`shortcut` 是给人看的提示，
            // 菜单项没配触发键时光筛它出来等于筛了个寂寞（按了也没反应）
            QuickFilter::Hotkey => !item.hotkey.trim().is_empty(),
            QuickFilter::LocalHotkey => !item.hotkey_local.trim().is_empty(),
            QuickFilter::Hotstring => !item.hotstring.trim().is_empty(),
            // 「单独指定了类型」或「写了后缀」都算有适用内容 —— 后者多半是
            // 顺手填的（跟着上级继承下来也算），所以两种都收
            QuickFilter::Contextual => {
                item.context_mode == settings::ContextMode::Own || !item.contexts.is_empty()
            }
            QuickFilter::Note => !item.note.trim().is_empty(),
            QuickFilter::Submenu => item.has_children(),
            QuickFilter::Hidden => !item.enabled,
        }
    }
}

/// 按下但还没挪动（够阈值才算拖动，否则就是一次点击）。
struct Press {
    path: Vec<usize>,
    start: gpui::Point<Pixels>,
}

/// 按下之后移动多少像素才算「开始拖动」（不是「点一下」）。
///
/// 系统默认的拖拽门槛是 4px，但这个列表里点选是主操作：手一抖就进拖动态，
/// 整行变成半透明的灰色，用户以为坏了。10px 是「明显在拖」和「手抖」的分界。
const DRAG_SLOP: f32 = 10.0;

struct DragItem {
    from: Vec<usize>,
    hover: Option<settings::DropTarget>,
}

/// 从资源管理器拖进来、还没决定「运行程序」那一栏写什么的一批文件。
///
/// 不能直接按完整路径建菜单项：`运行程序` 这一栏本来就能写裸文件名（走 PATH），
/// 拖进来的人可能是想要 `notepad.exe` 而不是 `C://Windows//notepad.exe`。
/// 所以先停在这儿问一句，选完再落盘。
struct PendingDrop {
    paths: Vec<std::path::PathBuf>,
    /// 落在哪儿：选中项后面 / 容器末尾
    after: Option<Vec<usize>>,
}

/// 展开后的一个列表行。
struct Row {
    path: Vec<usize>,
    depth: usize,
    id: String,
    label: String,
    note: String,
    badge: String,
    accel: String,
    hotkey: String,
    /// 局部生效的触发键（只在某些窗口里才响应）；空 = 没配
    hotkey_local: String,
    /// 局部键的生效条件（一行摘要，鼠标悬停时看得到大概）
    local_when: String,
    /// 热字符串（敲这串字就执行这一项）；空 = 没配
    hotstring: String,
    /// 「适用选中内容」：只在选中某类东西时出现的那一条规则；空 = 没单独指定
    context: String,
    /// 这一行是「多列符」而不是普通分隔线
    column_break: bool,
    action_label: String,
    visibility: String,
    enabled: bool,
    kind: ItemKind,
    has_children: bool,
    children_count: usize,
}

pub struct TreeView {
    scope: Scope,
    /// 搜索框（名称 / 备注 / 快捷键 / 动作参数都认）
    search: Entity<TextInput>,
    query: String,
    filter: QuickFilter,
    expanded: HashSet<String>,
    /// 主选中（最后点的那一行）：插新项、显示「选中了谁」都以它为准
    selected_id: Option<String>,
    /// 多选集合（总是包含主选中）。Ctrl 点选 / Shift 连选都往这里放
    selected: HashSet<String>,
    /// Shift 连选的锚点（上一次单选 / 点选的项）
    anchor: Option<String>,
    /// 列表的键盘焦点（Ctrl+C / Ctrl+V / Delete / Ctrl+A）
    focus: gpui::FocusHandle,
    press: Option<Press>,
    drag: Option<DragItem>,
    row_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// 列表区域的位置（判断「点到行没有」用：点工具条 / 批量栏不算点空白）
    list_bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// 拖进来的文件等着选「完整路径 / 只填文件名」
    pending_drop: Option<PendingDrop>,
    /// 复制 / 粘贴之后的一句回话（几秒后自己消失）
    note: Option<(String, std::time::Instant)>,
    _subs: Vec<Subscription>,
}

/// 调试用：`GPUI_MENU_DEBUG_EXPAND=工具,截图` 打开设置页时先把这几支展开。
///
/// 这台机器上模拟点击不可靠（坐标系随版式漂），要看「导入之后第 N 层长什么样」
/// 只能这么来。按**名称**匹配，不写死 id。
fn expand_on_startup(expanded: &mut HashSet<String>) {
    let Ok(list) = std::env::var("GPUI_MENU_DEBUG_EXPAND") else {
        return;
    };
    let wanted: Vec<String> = list
        .split(',')
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    if wanted.is_empty() {
        return;
    }
    fn walk(items: &[settings::ItemSetting], wanted: &[String], out: &mut HashSet<String>) {
        for item in items {
            if item.has_children() && wanted.iter().any(|name| *name == item.label) {
                out.insert(item.id.clone());
            }
            walk(&item.children, wanted, out);
        }
    }
    settings::read(|settings| walk(&settings.items, &wanted, expanded));
}

impl TreeView {
    pub fn new(scope: Scope, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            TextInput::new("", crate::i18n::t("搜索名称 / 备注 / 快捷键 / 动作参数"), 11.5, cx)
        });
        let mut subs = Vec::new();
        subs.push(cx.observe(&search, |this: &mut Self, input, cx| {
            this.query = input.read(cx).text();
            cx.notify();
        }));
        let mut expanded = HashSet::new();
        expand_on_startup(&mut expanded);
        Self {
            scope,
            focus: cx.focus_handle(),
            search,
            query: String::new(),
            filter: QuickFilter::All,
            expanded,
            selected_id: None,
            selected: HashSet::new(),
            anchor: None,
            press: None,
            drag: None,
            row_bounds: Rc::new(RefCell::new(HashMap::new())),
            list_bounds: Rc::new(RefCell::new(None)),
            pending_drop: None,
            note: None,
            _subs: subs,
        }
    }

    /// 配置被整份换掉之后清一下（恢复默认菜单 / 恢复默认设置）。
    ///
    /// 展开和选中都按 id 记，换了整棵树之后这些 id 全都不存在了，
    /// 不清的话下一帧 `prune` 也会清 —— 但拖动 / 按下这些中间状态得当场丢掉。
    pub fn reset_state(&mut self, cx: &mut Context<Self>) {
        // 搜索词也要清：换配置 / 导入之后它还留着的话，列表只剩上次搜到的那几行，
        // 看起来就是「菜单项全没了」。
        self.query.clear();
        self.search.update(cx, |input, cx| input.set_text("", cx));
        self.expanded.clear();
        self.selected_id = None;
        self.selected.clear();
        self.anchor = None;
        self.press = None;
        self.drag = None;
        self.pending_drop = None;
        cx.notify();
    }

    /// 快捷筛选切到「有快捷键」（热键设置页那颗按钮跳过来时用）。
    pub fn filter_hotkeys(&mut self, cx: &mut Context<Self>) {
        self.filter = QuickFilter::Hotkey;
        self.query.clear();
        self.search.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    /// 快捷筛选切到「局部快捷键」（热键设置页那颗按钮跳过来时用）。
    pub fn filter_local_hotkeys(&mut self, cx: &mut Context<Self>) {
        self.filter = QuickFilter::LocalHotkey;
        self.query.clear();
        self.search.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    /// 快捷筛选切到「有热字符串」（热字符串设置页那颗按钮跳过来时用）。
    pub fn filter_hotstrings(&mut self, cx: &mut Context<Self>) {
        self.filter = QuickFilter::Hotstring;
        self.query.clear();
        self.search.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    /// 换一段来编（编辑窗口改成了另一支时用；不清搜索词）。
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope.same(&scope) {
            return;
        }
        self.scope = scope;
        self.expanded.clear();
        self.selected_id = None;
        self.selected.clear();
        self.anchor = None;
        self.press = None;
        self.drag = None;
        cx.notify();
    }

    /// 丢掉已经不存在的展开项 / 选中项（删了项、换了配置之后）。
    fn prune(&mut self, cx: &mut Context<Self>) {        let alive: HashSet<String> = settings::read(|settings| {
            let mut out = HashSet::new();
            fn walk(items: &[settings::ItemSetting], out: &mut HashSet<String>) {
                for item in items {
                    out.insert(item.id.clone());
                    walk(&item.children, out);
                }
            }
            walk(&settings.items, &mut out);
            out
        });
        self.expanded.retain(|id| alive.contains(id));
        self.selected.retain(|id| alive.contains(id));
        if let Some(id) = &self.anchor {
            if !alive.contains(id) {
                self.anchor = None;
            }
        }
        if let Some(id) = &self.selected_id {
            if !alive.contains(id) {
                self.selected_id = None;
            }
        }
        // 主选中没了就从多选里随便挑一个当主选中（批量操作还要用它）
        if self.selected_id.is_none() {
            if let Some(id) = self.selected.iter().next().cloned() {
                self.selected_id = Some(id);
            }
        }
        let _ = cx;
    }

    // ---- 数据 ----

    /// 这一次要铺的那一层（Root 是 `settings.items`，其余是某个节点的子项）。
    ///
    /// 根层**不过滤托盘**：`path` 是树里的真实下标，所有树操作（拖拽 / 删除 / 编辑）
    /// 都拿它去 `settings.node(path)`。以前这里先 filter 掉托盘再造 path，
    /// 于是「过滤后的第 0 项」其实是树里的第 1 项 —— 只要托盘不在最后
    /// （换配置、删光了再加），点一行改的就是另一行、拖到最后一行的项会消失、
    /// 甚至把托盘那一支当成普通菜单项拖出来。托盘只在铺行时跳过（`walk`）。
    fn root_items(&self) -> Vec<settings::ItemSetting> {
        settings::read(|settings| match self.scope.container(settings) {
            Some(path) if !path.is_empty() => settings
                .node(&path)
                .map(|node| node.children.clone())
                .unwrap_or_default(),
            _ => settings.items.clone(),
        })
    }

    fn base_path(&self) -> Vec<usize> {
        settings::read(|settings| self.scope.container(settings).unwrap_or_default())
    }

    fn filtering(&self) -> bool {
        self.filter != QuickFilter::All || !self.query.trim().is_empty()
    }

    /// 铺平成行。
    ///
    /// 没筛选时按 `expanded` 决定铺到多深；筛选时**忽略展开状态**（匹配到就铺出来），
    /// 并且把命中项的祖先也留着 —— 否则匹配结果孤零零的，看不出它在哪一层。
    fn rows(&self) -> Vec<Row> {
        let items = self.root_items();
        let base = self.base_path();
        let mut out = Vec::new();
        if self.filtering() {
            let needle = self.query.trim().to_lowercase();
            let mut path = base.clone();
            self.walk_filtered(&items, 0, &mut path, &needle, &mut out);
        } else {
            let mut path = base.clone();
            self.walk(&items, 0, &mut path, &mut out);
        }
        out
    }

    fn walk(
        &self,
        items: &[settings::ItemSetting],
        depth: usize,
        path: &mut Vec<usize>,
        out: &mut Vec<Row>,
    ) {
        for (index, item) in items.iter().enumerate() {
            path.push(index);
            // 托盘 / 一键直达那两支有自己的页，根层这里不铺（但下标仍按真树算）
            if depth == 0 && settings::is_hidden_item(&item.id) {
                path.pop();
                continue;
            }
            out.push(make_row(item, path, depth));
            if item.has_children() && self.expanded.contains(&item.id) {
                self.walk(&item.children, depth + 1, path, out);
            }
            path.pop();
        }
    }

    /// 筛选模式：返回「这一支里有没有命中」，有就把它和命中后代铺出来。
    fn walk_filtered(
        &self,
        items: &[settings::ItemSetting],
        depth: usize,
        path: &mut Vec<usize>,
        needle: &str,
        out: &mut Vec<Row>,
    ) -> bool {
        let mut any = false;
        for (index, item) in items.iter().enumerate() {
            path.push(index);
            if depth == 0 && settings::is_hidden_item(&item.id) {
                path.pop();
                continue;
            }
            let mut child_rows: Vec<Row> = Vec::new();
            let child_hit = self.walk_filtered(&item.children, depth + 1, path, needle, &mut child_rows);
            let self_hit = item_hits(item, self.filter, needle);
            if self_hit || child_hit {
                any = true;
                // 分隔线在被筛的时候没有意义（它没有名字，永远不命中）
                if item.kind != ItemKind::Separator {
                    out.push(make_row(item, path, depth));
                }
                out.extend(child_rows);
            }
            path.pop();
        }
        any
    }

    // ---- 拖拽 ----

    fn drop_target(
        &self,
        y: f32,
        from: &[usize],
        rows: &[Row],
    ) -> Option<settings::DropTarget> {
        let kinds: HashMap<String, ItemKind> =
            rows.iter().map(|row| (path_key(&row.path), row.kind)).collect();
        let bounds = self.row_bounds.borrow();
        let mut hit: Option<(f32, Vec<usize>, f32)> = None;
        for (key, rect) in bounds.iter() {
            let top = f32::from(rect.origin.y);
            let height = f32::from(rect.size.height).max(1.0);
            if y < top || y > top + height {
                continue;
            }
            let path = parse_key(key);
            if path == from || path.starts_with(from) {
                continue;
            }
            let ratio = ((y - top) / height).clamp(0.0, 1.0);
            let distance = (ratio - 0.5).abs();
            if hit.as_ref().map(|(d, _, _)| distance < *d).unwrap_or(true) {
                hit = Some((distance, path, ratio));
            }
        }
        let Some((_, path, ratio)) = hit else {
            // 落在所有行**下面**的空白区：挪到这一层最后（拖到列表末尾最自然的手势）。
            // 行与行之间那几个像素的空隙不算（那里松手应该是「哪儿也不去」）。
            let bottom = bounds
                .values()
                .map(|rect| f32::from(rect.origin.y) + f32::from(rect.size.height))
                .fold(f32::MIN, f32::max);
            return (y > bottom).then(|| self.tail_target(rows, from)).flatten();
        };
        let is_separator = kinds
            .get(&path_key(&path))
            .map(|kind| *kind == ItemKind::Separator)
            .unwrap_or(false);
        if is_separator {
            return Some(if ratio < 0.5 {
                settings::DropTarget::Before(path)
            } else {
                settings::DropTarget::After(path)
            });
        }
        if ratio < 0.28 {
            Some(settings::DropTarget::Before(path))
        } else if ratio > 0.72 {
            Some(settings::DropTarget::After(path))
        } else {
            Some(settings::DropTarget::Into(path))
        }
    }

    /// 「这一层的最后」是哪儿：铺出来的最后一行（不含正在拖的那一支）。
    fn tail_target(&self, rows: &[Row], from: &[usize]) -> Option<settings::DropTarget> {
        let base = self.base_path();
        let last = rows.iter().rev().find(|row| {
            row.depth == base.len() && row.path != from && !row.path.starts_with(from)
        })?;
        Some(settings::DropTarget::After(last.path.clone()))
    }

    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if let Some(target) = drag.hover {
            if let settings::DropTarget::Into(path) = &target {
                if let Some(id) =
                    settings::read(|settings| settings.node(path).map(|node| node.id.clone()))
                {
                    self.expanded.insert(id);
                }
            }
            settings::update(cx, |settings| settings.move_node(&drag.from, &target));
        }
        self.prune(cx);
        cx.notify();
    }

    fn toggle_expanded(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_string());
        }
        cx.notify();
    }

    // ---- 多选 ----

    /// 只选这一行。
    fn select_only(&mut self, id: &str) {
        self.selected.clear();
        self.selected.insert(id.to_string());
        self.selected_id = Some(id.to_string());
    }

    /// 右键某一行：先让它成为当前项（已经在多选里就不动选择，方便批量处理），
    /// 再弹那一套快捷操作（复制 / 粘贴 / 复制一份 / 隐藏 / 删除 …）。
    fn context_row(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.selected.contains(id) {
            self.select_only(id);
        }
        crate::popup::open_item_context(cx, id);
        cx.notify();
    }

    /// 一次点击的选择语义：Shift 连选、Ctrl 加减选、直接点单选。
    fn select_click(&mut self, id: &str, modifiers: gpui::Modifiers) {
        let toggle = modifiers.control || modifiers.platform;
        if modifiers.shift {
            match self.anchor.clone() {
                Some(anchor) => {
                    let ids = self.ids_between(&anchor, id);
                    self.selected.extend(ids);
                    self.selected_id = Some(id.to_string());
                }
                None => {
                    self.select_only(id);
                    self.anchor = Some(id.to_string());
                }
            }
        } else if toggle {
            if self.selected.contains(id) {
                self.selected.remove(id);
                if self.selected_id.as_deref() == Some(id) {
                    self.selected_id = self.selected.iter().next().cloned();
                }
            } else {
                self.selected.insert(id.to_string());
                self.selected_id = Some(id.to_string());
            }
            self.anchor = Some(id.to_string());
        } else {
            self.select_only(id);
            self.anchor = Some(id.to_string());
        }
    }

    /// `a` 到 `b` 之间（按列表顺序，含两端）的所有行 id。
    fn ids_between(&self, a: &str, b: &str) -> Vec<String> {
        let rows = self.rows();
        let (Some(ia), Some(ib)) = (
            rows.iter().position(|row| row.id == a),
            rows.iter().position(|row| row.id == b),
        ) else {
            return vec![b.to_string()];
        };
        let (lo, hi) = if ia <= ib { (ia, ib) } else { (ib, ia) };
        rows[lo..=hi].iter().map(|row| row.id.clone()).collect()
    }

    /// 选中的 id（按列表顺序；祖先也被选中时不再单独算它 —— 复制/删除都会连子树一起处理）。
    fn selected_in_order(&self) -> Vec<String> {
        let rows = self.rows();
        let chosen: Vec<&Row> = rows
            .iter()
            .filter(|row| self.selected.contains(&row.id))
            .collect();
        chosen
            .iter()
            .filter(|row| {
                !chosen
                    .iter()
                    .any(|other| other.path.len() < row.path.len() && row.path.starts_with(&other.path))
            })
            .map(|row| row.id.clone())
            .collect()
    }

    /// 批量删除（子项跟着父项一起走，不会漏）。
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let ids = self.selected_in_order();
        if ids.is_empty() {
            return;
        }
        settings::update(cx, |settings| {
            for id in &ids {
                if let Some(path) = settings.find_path(id) {
                    settings.remove_at(&path);
                }
            }
        });
        self.selected.clear();
        self.selected_id = None;
        self.anchor = None;
        self.prune(cx);
        cx.notify();
    }

    /// 批量复制：把整棵子树写成一段 JSON 放进剪贴板。
    ///
    /// 用**文本**而不是内部剪贴板：这样直接粘到聊天窗口就能发给别人，
    /// 别人再粘回这里就还原（连子项、动作、热字符串一起）。
    fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let ids = self.selected_in_order();
        if ids.is_empty() {
            self.note = Some((
                crate::i18n::t("先选中菜单项再复制").to_string(),
                std::time::Instant::now(),
            ));
            cx.notify();
            return;
        }
        let items: Vec<settings::ItemSetting> = settings::read(|settings| {
            ids.iter()
                .filter_map(|id| settings.item_by_id(id).cloned())
                .collect()
        });
        let count = items.len();
        let payload = PastePayload { marker: 1, items };
        match serde_json::to_string(&payload) {
            Ok(text) => {
                // 两种写法都做一次：GPUI 的剪贴板给程序内部用，Win32 的那次直接把
                // 文本落到系统剪贴板。只写其中一种的话，另一种读法就读不到 ——
                // 表现就是「复制成功了，点粘贴却没反应」。
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                crate::win32::write_clipboard_text(&text);
                self.note = Some((
                    format!(
                        "{} {count} {}",
                        crate::i18n::t("已复制"),
                        crate::i18n::t("项 · 到目标位置按 Ctrl+V 粘贴（换菜单配置后也能粘）")
                    ),
                    std::time::Instant::now(),
                ));
            }
            Err(err) => {
                self.note = Some((
                    format!("{}: {err}", crate::i18n::t("复制失败")),
                    std::time::Instant::now(),
                ));
            }
        }
        cx.notify();
    }

    /// 从剪贴板粘进来（粘在选中项后面；没选中就放末尾）。
    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        if self.scope_is_live() {
            self.note = Some((
                crate::i18n::t("这一支开着「实时生成」，加进来的项会在下次呼出时被清掉，已拦下").to_string(),
                std::time::Instant::now(),
            ));
            cx.notify();
            return;
        }
        let missing = || {
            (
                crate::i18n::t("剪贴板里没有菜单项：先选中要复制的项，按 Ctrl+C").to_string(),
                std::time::Instant::now(),
            )
        };
        // 读剪贴板优先走 Win32：GPUI 的 `read_from_clipboard` 在 Windows 上并不
        // 总能拿到内容（窗口刚拿到焦点、或者剪贴板是刚写进去的时候），而菜单里
        // 那个「粘贴到这一项下面」一直用 Win32 读，从来没出过问题 —— 设置页这颗
        // 「粘贴」按钮点了没反应，就是读法不一样造成的。
        let text = crate::win32::read_clipboard_text()
            .or_else(|| cx.read_from_clipboard().and_then(|item| item.text()));
        let Some(text) = text else {
            self.note = Some(missing());
            cx.notify();
            return;
        };
        let Some(payload) = parse_paste(&text) else {
            self.note = Some(missing());
            cx.notify();
            return;
        };
        if payload.items.is_empty() {
            self.note = Some(missing());
            cx.notify();
            return;
        }
        let count = payload.items.len();
        let container = self.container_of(cx);
        let after = self
            .selected_id
            .as_ref()
            .and_then(|id| settings::read(|settings| settings.find_path(id)));
        let at_end = after.is_none();
        let ids = settings::update(cx, |settings| {
            settings.paste_items(&container, after.as_deref(), payload.items)
        });
        if ids.len() == 1 {
            self.select_only(&ids[0]);
        } else {
            self.selected = ids.iter().cloned().collect();
            self.selected_id = ids.last().cloned();
        }
        self.anchor = self.selected_id.clone();
        self.note = Some((
            format!(
                "{} {count} {}",
                crate::i18n::t("已粘贴"),
                crate::i18n::t(if at_end {
                    "项（没选中，加在菜单末尾；id 已重新编号）"
                } else {
                    "项（id 已重新编号，不会和现有的撞）"
                })
            ),
            std::time::Instant::now(),
        ));
        cx.notify();
    }

    /// 全选当前铺出来的行。
    fn select_all_rows(&mut self, cx: &mut Context<Self>) {
        self.selected = self.rows().iter().map(|row| row.id.clone()).collect();
        self.selected_id = self.selected.iter().next().cloned();
        cx.notify();
    }

    /// 指针底下是哪一行（None = 点在空白处）。
    fn row_at(&self, point: gpui::Point<Pixels>) -> Option<Vec<usize>> {
        let bounds = self.row_bounds.borrow();
        bounds.iter().find_map(|(key, rect)| {
            (point.x >= rect.origin.x
                && point.x <= rect.origin.x + rect.size.width
                && point.y >= rect.origin.y
                && point.y <= rect.origin.y + rect.size.height)
                .then(|| parse_key(key))
        })
    }

    /// 这一段是不是「实时生成」的子菜单 —— 是的话不允许手动加任何项
    /// （加了也会在下一次呼出菜单时被清掉，不如当场拦住并说明原因）。
    fn scope_is_live(&self) -> bool {
        match &self.scope {
            Scope::Submenu(id) => settings::autofill_is_live(id),
            _ => false,
        }
    }

    /// 容器路径（新项往这里放）。
    fn container_of(&self, cx: &Context<Self>) -> Vec<usize> {
        let _ = cx;
        self.base_path()
    }

    // ---- 从资源管理器拖文件进来 ----

    /// 收到一批路径：先停下来问「运行程序那一栏填完整路径还是只填文件名」。
    fn accept_paths(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        if self.scope_is_live() {
            self.note = Some((
                crate::i18n::t("这一支开着「实时生成」，拖进来的项会在下次呼出时被清掉，已拦下").to_string(),
                std::time::Instant::now(),
            ));
            cx.notify();
            return;
        }
        // 只认真的存在的；顺便去重（同一次拖拽里偶尔会重复给同一条）
        let mut seen = HashSet::new();
        let paths: Vec<std::path::PathBuf> = paths
            .into_iter()
            .filter(|path| path.exists() && seen.insert(path.clone()))
            .collect();
        if paths.is_empty() {
            return;
        }
        // 落点在选中项后面（和「添加菜单项」的规则一致）
        let after = self
            .selected_id
            .as_ref()
            .and_then(|id| settings::read(|settings| settings.find_path(id)));
        self.pending_drop = Some(PendingDrop { paths, after });
        cx.notify();
    }

    /// 选完了：真的建菜单项。
    fn apply_pending_drop(&mut self, full_path: bool, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_drop.take() else {
            return;
        };
        let container = self.container_of(cx);
        let mut specs: Vec<(String, String)> = Vec::new();
        for path in &pending.paths {
            let label = drop_label(path);
            // 文件夹没有「裸名字」这回事（PATH 里只有可执行文件），一律完整路径
            // 一律套双引号：路径/文件名里带空格时不套会被 shell 拆成两个参数
            let program = if full_path || path.is_dir() {
                settings::MenuSettings::quote_path(&path.display().to_string())
            } else {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                settings::MenuSettings::quote_path(&name)
            };
            specs.push((label, program));
        }
        let ids = settings::update(cx, |settings| {
            settings.add_program_items(&container, pending.after.as_deref(), &specs)
        });
        if let Some(id) = ids.last() {
            self.selected_id = Some(id.clone());
        }
        cx.notify();
    }
}

/// 复制到剪贴板的那段 JSON（`runany_menu_items` 是格式标记，防止把别的 JSON 当菜单项）。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PastePayload {
    #[serde(default = "paste_marker", rename = "runany_menu_items")]
    marker: u32,
    items: Vec<settings::ItemSetting>,
}

fn paste_marker() -> u32 {
    1
}

/// 解析剪贴板：既认 `{"runany_menu_items":1,"items":[...]}`，也认单个菜单项对象。
fn parse_paste(text: &str) -> Option<PastePayload> {
    if let Ok(payload) = serde_json::from_str::<PastePayload>(text) {
        return Some(payload);
    }
    serde_json::from_str::<settings::ItemSetting>(text)
        .ok()
        .map(|item| PastePayload {
            marker: 1,
            items: vec![item],
        })
}

/// 拖进来的文件叫什么：文件名去掉后缀（`QQ.exe` → `QQ`），文件夹就用文件夹名。
pub(crate) fn drop_label(path: &std::path::Path) -> String {
    if path.is_dir() {
        return path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
    }
    path.file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn make_row(item: &settings::ItemSetting, path: &[usize], depth: usize) -> Row {
    // 图标位：自己填了就用自己那个；空着就按「第一个运行程序动作」自动抠一个 ——
    // 和菜单里显示的一致，否则设置页里看着「用了程序图标却没显示」。
    let badge = {
        let raw = item.badge.trim();
        if !raw.is_empty() {
            raw.to_string()
        } else {
            settings::read(|settings| item.auto_badge(settings.auto_icons)).unwrap_or_default()
        }
    };
    Row {
        path: path.to_vec(),
        depth,
        id: item.id.clone(),
        label: if item.column_break {
            crate::i18n::t("多列符（从这里另起一列）").to_string()
        } else if item.kind == ItemKind::Separator {
            crate::i18n::t("分隔线").to_string()
        } else if item.label.trim().is_empty() {
            crate::i18n::t("（未命名）").to_string()
        } else {
            // 菜单项名字是存在 `*.runa` 里的原文。默认模板那几条（「打开配置目录」
            // 「退出」…）跟着界面语言走，用户自己起的名字表里没有、原样显示。
            crate::i18n::t_dynamic(&item.label).to_string()
        },
        note: item.note.trim().to_string(),
        badge,
        accel: item.accel.clone(),
        hotkey: item.hotkey.trim().to_string(),
        hotkey_local: item.hotkey_local.trim().to_string(),
        local_when: item
            .hotkey_local_when
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("//"))
            .collect::<Vec<_>>()
            .join(" / "),
        hotstring: item.hotstring.trim().to_string(),
        // 只显示「真的单独指定过」的：跟随上级 / 没勾类型都是常态，摆出来只是噪音。
        // 勾了「文件」的还要把后缀带上，不然 `mp4|mkv` 这种限制在列表里看不见。
        context: match item.context_mode {
            settings::ContextMode::Own if !item.contexts.is_empty() => {
                let mut text = item
                    .contexts
                    .iter()
                    .map(|kind| kind.label())
                    .collect::<Vec<_>>()
                    .join("/");
                let exts = item.file_exts.trim();
                if !exts.is_empty() && item.contexts.contains(&crate::selection::SelectionKind::File) {
                    text.push_str(" · ");
                    text.push_str(exts);
                }
                text
            }
            settings::ContextMode::Never => crate::i18n::t("不参与筛选").to_string(),
            _ => String::new(),
        },
        column_break: item.kind == ItemKind::Separator && item.column_break,
        action_label: if item.kind == ItemKind::Submenu {
            crate::i18n::t("子菜单").to_string()
        } else {
            match item.actions.len() {
                0 => String::new(),
                1 => item.actions[0].kind.label().to_string(),
                count if crate::i18n::is_english() => format!("{count} actions"),
                count => format!("{count} 个动作"),
            }
        },
        visibility: {
            let mut parts: Vec<&str> = Vec::new();
            if item.show_badges == Some(false) {
                parts.push(crate::i18n::t("无图标位"));
            }
            if item.show_shortcuts == Some(false) {
                parts.push(crate::i18n::t("无快捷键"));
            }
            parts.join(" · ")
        },
        enabled: item.enabled,
        kind: item.kind,
        has_children: item.has_children(),
        // 数「能点进去的东西」：子菜单也算一项。以前只数普通项，
        // 于是「工具」这种全是二级子菜单的显示「0 项」，看着像空的。
        children_count: item
            .children
            .iter()
            .filter(|child| child.kind != ItemKind::Separator)
            .count(),
    }
}

/// 一项满不满足「快捷筛选 + 搜索词」。
fn item_hits(item: &settings::ItemSetting, filter: QuickFilter, needle: &str) -> bool {
    if !filter.hits(item) {
        return false;
    }
    if needle.is_empty() {
        return true;
    }
    if text_hits(&item.label, needle)
        || text_hits(&item.note, needle)
        || text_hits(&item.shortcut, needle)
        || text_hits(&item.hotkey, needle)
        || text_hits(&item.hotkey_local, needle)
        || text_hits(&item.hotkey_local_when, needle)
        || text_hits(&item.badge, needle)
    {
        return true;
    }
    item.actions.iter().any(|step| {
        text_hits(&step.value, needle) || text_hits(step.kind.label(), needle)
    })
}

fn text_hits(haystack: &str, needle: &str) -> bool {
    !needle.is_empty() && haystack.to_lowercase().contains(needle)
}

fn path_key(path: &[usize]) -> String {
    path.iter()
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join("-")
}

fn parse_key(key: &str) -> Vec<usize> {
    key.split('-')
        .filter_map(|part| part.parse().ok())
        .collect()
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

impl gpui::Render for TreeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prune(cx);
        // 行的位置每帧重算：老位置留着会让拖拽命中「已经不在那儿的行」
        // （删项 / 折叠 / 换配置之后，鼠标下面那块区域其实是旧行）。
        self.row_bounds.borrow_mut().clear();
        let rows = self.rows();
        let container = self.container_of(cx);

        let selected = self
            .selected_id
            .as_ref()
            .filter(|id| rows.iter().any(|row| &row.id == *id));
        let selected_label = selected.and_then(|id| {
            rows.iter()
                .find(|row| &row.id == id)
                .map(|row| row.label.clone())
        });
        let after: Option<Vec<usize>> = selected.and_then(|id| {
            settings::read(|settings| settings.find_path(id))
        });

        // ---- 工具条 ----
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .flex_wrap()
            .px(px(6.))
            .py(px(4.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex_1()
                    .min_w(px(140.))
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(theme::surface())
                    .border_1()
                    .border_color(theme::border_strong())
                    .child(self.search.clone()),
            )
            .child(
                div()
                    .id("tree-search-clear")
                    .size(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .text_size(px(10.))
                    .text_color(theme::ui_text_tertiary())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.search.update(cx, |input, cx| {
                            input.set_text("", cx);
                        });
                        window.refresh();
                        cx.notify();
                    }))
                    .child("✕"),
            )
            .when(!self.scope_is_live(), |bar| {
                bar.child(add_button("添加菜单项", container.clone(), after.clone(), AddKind::Item, cx))
                    .child(add_button("添加子菜单", container.clone(), after.clone(), AddKind::Submenu, cx))
                    .child(add_button("添加分隔线", container.clone(), after.clone(), AddKind::Separator, cx))
                    .child(add_button("添加多列符", container.clone(), after.clone(), AddKind::ColumnBreak, cx))
            })
            .when(self.scope_is_live(), |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t(
                            "这一支开着「实时生成」：菜单项由文件夹决定，不能手动添加（要改请在子菜单里改方式）",
                        )),
                )
            });

        // ---- 快捷筛选 ----
        let mut chips = div().flex().items_center().gap(px(4.)).flex_wrap().px(px(6.));
        for filter in QuickFilter::ALL {
            let active = self.filter == filter;
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("tree-chip-{}", filter.label())))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(9.))
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .when(active, |style| {
                        style
                            .bg(theme::accent_soft())
                            .text_color(theme::accent())
                            .border_1()
                            .border_color(theme::accent())
                    })
                    .when(!active, |style| {
                        style
                            .bg(theme::surface())
                            .text_color(theme::ui_text_secondary())
                            .border_1()
                            .border_color(theme::border())
                            .hover(|style| style.bg(theme::surface_hover()))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.filter = filter;
                        cx.notify();
                    }))
                    .child(filter.label()),
            );
        }
        if self.filtering() {
            chips = chips.child(
                div()
                    .flex_1()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(if crate::i18n::is_english() {
                        format!("{} matching", rows.len())
                    } else {
                        format!("匹配 {} 项", rows.len())
                    }),
            );
        }

        // ---- 批量操作栏 ----
        //
        // 常驻：一行没选的时候也得能按「粘贴」（粘到菜单末尾），
        // 所以这条栏不跟着选中与否一起消失，只把按钮置灰。
        let batch_bar: AnyElement = {
            let scope_key = self.scope.key();
            let count = self.selected.len();
            let hint = if crate::i18n::is_english() {
                if count > 1 {
                    format!("{count} selected - Ctrl+C copy, Ctrl+V paste, Del delete")
                } else if count == 1 {
                    "1 selected - Ctrl+C copy, Ctrl+V paste".to_string()
                } else {
                    "Ctrl+V or Paste also adds to the end".to_string()
                }
            } else if count > 1 {
                format!("已选 {count} 项 · Ctrl+C 复制、Ctrl+V 粘贴、Del 删除")
            } else if count == 1 {
                "已选 1 项 · Ctrl+C 复制、Ctrl+V 粘贴".to_string()
            } else {
                "Ctrl+V 或「粘贴」同样加到末尾".to_string()
            };
            let mut bar = div()
                .px(px(6.))
                .pb(px(4.))
                // 这一栏里的按钮自己吃掉按下事件：别让它冒到卡片上变成「点空白取消选择」
                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                    cx.stop_propagation();
                })
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(10.5))
                        .text_color(if count == 0 {
                            theme::ui_text_tertiary()
                        } else {
                            theme::accent()
                        })
                        .child(hint),
                )
                .child(crate::settings_ui::ghost_button_tagged_if(
                    format!("{}-copy", scope_key),
                    "复制",
                    count > 0,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.copy_selected(cx);
                        window.refresh();
                    }),
                ))
                .child(crate::settings_ui::ghost_button_tagged(
                    format!("{}-paste", scope_key),
                    "粘贴",
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.paste_clipboard(cx);
                        window.refresh();
                    }),
                ))
                .child(crate::settings_ui::ghost_button_tagged_if(
                    format!("{}-delete", scope_key),
                    "删除",
                    count > 0,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.delete_selected(cx);
                        window.refresh();
                    }),
                ));
            if count > 0 {
                bar = bar.child(crate::settings_ui::ghost_button_tagged(
                    format!("{}-clear", scope_key),
                    "取消选择",
                    cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.selected.clear();
                        this.selected_id = None;
                        this.anchor = None;
                        cx.notify();
                    }),
                ));
            }
            bar.into_any_element()
        };

        // 复制 / 粘贴之后的一句回话（4 秒后自己消失）
        let note_line: Option<AnyElement> = self
            .note
            .as_ref()
            .filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(4))
            .map(|(text, _)| {
                div()
                    .px(px(6.))
                    .pb(px(4.))
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_secondary())
                    .child(text.clone())
                    .into_any_element()
            });

        // 新项插哪儿
        let position_hint = div()
            .px(px(6.))
            .pb(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(crate::settings_ui::dot(selected.is_some()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(if selected.is_some() {
                        theme::accent()
                    } else {
                        theme::ui_text_tertiary()
                    })
                    .child(match &selected_label {
                        Some(label) if crate::i18n::is_english() => {
                            format!("New rows go after \"{label}\"")
                        }
                        Some(label) => format!("新项插在「{label}」后面"),
                        None => {
                            crate::i18n::t("还没选中项：新项加到末尾（点一下行就能选中）")
                                .to_string()
                        }
                    }),
            );

        // ---- 拖入确认条 ----
        //
        // 拖文件进列表时先冒出一条带两个按钮的提示：`运行程序` 那一栏到底填
        // 完整路径还是裸文件名。默认按钮给「完整路径」（最不容易出错）。
        let drop_bar = self.pending_drop.as_ref().map(|pending| {
            let count = pending.paths.len();
            let names: Vec<String> = pending
                .paths
                .iter()
                .take(3)
                .map(|path| drop_label(path))
                .collect();
            let mut title = names.join("、");
            if count > names.len() {
                title.push_str(" 等");
            }
            let headline = if crate::i18n::is_english() {
                format!("Dropped {count}: {title} — what goes in \"Run program\"?")
            } else {
                format!("拖进来 {count} 项：{title}\n「运行程序」那一栏填什么？")
            };
            // 居中弹窗，而不是列表顶上那条横幅：插入位置可能滚到很下面，
            // 顶上那条提示根本看不到（用户报过）。
            div()
                .absolute()
                .left_0()
                .top_0()
                .size_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x000000aa))
                .child(
                    div()
                        .w(px(420.))
                        .p(px(14.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(theme::accent())
                        .bg(theme::surface_elevated())
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(20.))
                                .text_color(theme::text_primary())
                                .child(headline),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .line_height(px(16.))
                                .text_color(theme::ui_text_tertiary())
                                .child(crate::i18n::t(
                                    "完整路径最省事；只填文件名的话跟着 PATH 走，换了机器可能找不到。文件夹一律用完整路径。",
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(crate::settings_ui::ghost_button(
                                    crate::i18n::t("取消"),
                                    cx.listener(|this, _: &ClickEvent, _window, cx| {
                                        this.pending_drop = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(crate::settings_ui::ghost_button(
                                    crate::i18n::t("只填文件名"),
                                    cx.listener(|this, _: &ClickEvent, _window, cx| {
                                        this.apply_pending_drop(false, cx);
                                    }),
                                ))
                                .child(crate::settings_ui::primary_button(
                                    crate::i18n::t("完整路径"),
                                    cx.listener(|this, _: &ClickEvent, _window, cx| {
                                        this.apply_pending_drop(true, cx);
                                    }),
                                )),
                        ),
                )
        });

        let mut list = div().flex().flex_col();
        if rows.is_empty() {
            list = list.child(
                div()
                    .px(px(9.))
                    .py(px(14.))
                    .text_size(px(11.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(crate::i18n::t(if self.filtering() {
                        "没有匹配的菜单项（换个词，或者点「全部」）"
                    } else if self.scope == Scope::Tray {
                        "托盘菜单还是空的，点上面的「添加菜单项」"
                    } else {
                        "还没有菜单项，点上面的「添加菜单项」"
                    })),
            );
        }
        for row in &rows {
            list = list.child(self.row(row, cx));
        }
        let list_bounds = self.list_bounds.clone();
        // `flex_1` 很重要：不加的话列表区高度只有「行的总高」，最后一行下面那块空白
        // 不属于列表 —— 在那儿按下去不会取消选择，看起来就是「点空白没反应」。
        let list_area = div()
            .relative()
            .flex_1()
            .min_h(px(40.))
            .w_full()
            .child(list)
            .child(
                canvas(
                    |_, _, _| {},
                    move |rect, _, _, _| {
                        *list_bounds.borrow_mut() = Some(rect);
                    },
                )
                .absolute()
                .inset_0(),
            );

        let mut card = div()
            .w_full()
            .min_w_0()
            .relative()
            .flex()
            .flex_col()
            .gap(px(4.))
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_sunken())
            .p(px(7.))
            // 从资源管理器拖文件进来：松手时这里收到路径
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _window, cx| {
                let modifiers = event.keystroke.modifiers;
                let key = event.keystroke.key.as_str();
                if modifiers.control || modifiers.platform {
                    match key {
                        "c" => {
                            this.copy_selected(cx);
                            cx.stop_propagation();
                        }
                        "v" => {
                            this.paste_clipboard(cx);
                            cx.stop_propagation();
                        }
                        "a" => {
                            this.select_all_rows(cx);
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                } else if key == "delete" || key == "backspace" {
                    if !this.selected.is_empty() {
                        this.delete_selected(cx);
                        cx.stop_propagation();
                    }
                } else if key == "escape" {
                    if !this.selected.is_empty() {
                        this.selected.clear();
                        this.selected_id = None;
                        this.anchor = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }
            }))
            // 点列表空白处（最后一行下面的那块）＝取消选择。
            // 只挂一「清选择」，不抢焦点：工具条里的搜索框也在这一张卡片里。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    // 只有点在**列表区域内部**的空白处才算「取消选择」。
                    // 以前是整张卡片都算 —— 点上面的工具条（复制 / 粘贴 / 删除、
                    // 搜索框）会把选中项先清掉，于是那几个按钮点了「没反应」：
                    // 按下时选中没了，抬起时按钮拿不到要操作的对象。
                    let inside_list = this
                        .list_bounds
                        .borrow()
                        .map(|bounds| bounds.contains(&event.position))
                        .unwrap_or(false);
                    if inside_list
                        && this.row_at(event.position).is_none()
                        && !(event.modifiers.control || event.modifiers.platform)
                    {
                        this.selected.clear();
                        this.selected_id = None;
                        this.anchor = None;
                        cx.notify();
                    }
                }),
            )
            .on_drop::<ExternalPaths>(cx.listener(
                |this, paths: &ExternalPaths, _window, cx| {
                    this.accept_paths(paths.paths().to_vec(), cx);
                },
            ))
            .child(toolbar);
        card = card.child(chips).child(batch_bar);
        if let Some(line) = note_line {
            card = card.child(line);
        }
        card = card
            .child(position_hint)
            // 拖动时指针早就离开原元素了，move / up 挂在整个列表上
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                // 鼠标键已经松了，但我们没收到那一下 up（手指抖一下、up 落在别的
                // 窗口上、或者被别的元素吞了）：按下 / 拖动这两个中间状态会一直挂着，
                // 那一行就永远显示成「半透明、正在拖动」的灰色。这里借下一次
                // move 把它们放掉。
                if event.pressed_button.is_none() && (this.press.is_some() || this.drag.is_some()) {
                    this.press = None;
                    this.drag = None;
                    cx.notify();
                }
                if let Some(press) = &this.press {
                    let dx = f32::from(event.position.x) - f32::from(press.start.x);
                    let dy = f32::from(event.position.y) - f32::from(press.start.y);
                    // 以前是 5px：点一下手稍微抖一点就算「开始拖动」，整行变成半透明的
                    // 灰色，看着像坏了。菜单项列表里「点中」比「拖动」常用得多，
                    // 门槛放到 10px，真要拖的人照样一拖就起来。
                    if dx * dx + dy * dy > DRAG_SLOP * DRAG_SLOP {
                        let from = press.path.clone();
                        this.press = None;
                        this.drag = Some(DragItem { from, hover: None });
                    }
                }
                if let Some(from) = this.drag.as_ref().map(|drag| drag.from.clone()) {
                    let rows = this.rows();
                    let hover = this.drop_target(f32::from(event.position.y), &from, &rows);
                    if let Some(drag) = this.drag.as_mut() {
                        drag.hover = hover;
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.drag.is_some() {
                        this.finish_drag(cx);
                    } else {
                        this.press = None;
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.drag.is_some() {
                        this.finish_drag(cx);
                    } else {
                        this.press = None;
                    }
                }),
            )
            .child(list_area)
            // 拖入确认挂在最后：和列表同级、画在它上面（否则会被列表盖住）
            .children(drop_bar);
        card
    }
}

#[derive(Clone, Copy)]
enum AddKind {
    Item,
    Submenu,
    Separator,
    /// 多列符：和分隔线同一套插入逻辑，只是多一个标记
    ColumnBreak,
}

fn add_button(
    label: &'static str,
    container: Vec<usize>,
    after: Option<Vec<usize>>,
    kind: AddKind,
    cx: &mut Context<TreeView>,
) -> AnyElement {
    let listener = cx.listener(move |this, _: &ClickEvent, window, cx| {
        let id = settings::update(cx, |settings| match kind {
            AddKind::Item => settings.add_item_in(&container, after.as_deref()),
            AddKind::Submenu => settings.add_submenu_in(&container, after.as_deref()),
            AddKind::Separator => settings.add_separator_in(&container, after.as_deref()),
            AddKind::ColumnBreak => settings.add_column_break_in(&container, after.as_deref()),
        });
        if matches!(kind, AddKind::Submenu) {
            // 顺手展开，省得刚建出来看不见里面的占位子项
            this.expanded.insert(id.clone());
        }
        this.selected_id = Some(id);
        window.refresh();
        cx.notify();
    });
    div()
        .id(SharedString::from(format!("tree-add-{label}")))
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .text_size(px(11.))
        .bg(theme::surface_elevated())
        .border_1()
        .border_color(theme::border_strong())
        .text_color(theme::text_primary())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(listener)
        .child(crate::i18n::t(label))
        .into_any_element()
}

impl TreeView {
    fn row(&self, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let path = row.path.clone();
        let key = path_key(&path);
        let indent = 4.0 + row.depth as f32 * 16.0;
        let selected = self.selected.contains(&row.id);
        let dragging = self
            .drag
            .as_ref()
            .map(|drag| drag.from == path)
            .unwrap_or(false);
        let drop = self
            .drag
            .as_ref()
            .and_then(|drag| drag.hover.clone())
            .filter(|target| target.anchor() == path.as_slice());
        let drop_into = matches!(drop, Some(settings::DropTarget::Into(_)));
        let drop_before = matches!(drop, Some(settings::DropTarget::Before(_)));
        let drop_after = matches!(drop, Some(settings::DropTarget::After(_)));

        // 分隔线：一行虚线 + 删除
        if row.kind == ItemKind::Separator {
            let separator_row = div()
                .id(SharedString::from(format!("sep-{key}")))
                .h(px(22.))
                .pl(px(indent + 20.0))
                .pr(px(6.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(5.))
                .cursor_pointer()
                .when(selected, |style| style.bg(theme::accent_soft()))
                .on_click(cx.listener({
                    let id = row.id.clone();
                    move |this, _: &ClickEvent, _window, cx| {
                        this.selected_id = Some(id.clone());
                        cx.notify();
                    }
                }))
                .child(if row.column_break {
                    // 多列符：一根竖线 + 说明，和横着的分隔线一眼能分清
                    div()
                        .flex_1()
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().w(px(1.)).h_full().bg(theme::accent()))
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme::accent())
                                .child(crate::i18n::t("多列符：从这里另起一列")),
                        )
                        .into_any_element()
                } else {
                    div().flex_1().h(px(1.)).bg(theme::separator()).into_any_element()
                })
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(theme::ui_text_tertiary())
                        .child(crate::i18n::t(if row.column_break { "多列" } else { "分隔线" })),
                )
                .child(remove_button(path, cx));
            return measured_row(
                key.clone(),
                self.row_bounds.clone(),
                div().w_full().min_w_0().child(separator_row),
            );
        }

        let mut leading = div().flex().items_center().gap(px(4.)).flex_none();
        leading = leading.child(if row.has_children {
            let expanded = self.expanded.contains(&row.id) || self.filtering();
            div()
                .id(SharedString::from(format!("chev-{key}")))
                // 展开箭头：以前是 10.5px 的灰三角，藏在行首几乎看不见。
                // 放大到 15px、加粗、用主色，展开时再给个底色。
                .size(px(15.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .when(expanded, |style| style.bg(theme::track()))
                .text_size(px(14.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(if expanded {
                    theme::accent()
                } else {
                    theme::text_primary()
                })
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener({
                    let id = row.id.clone();
                    move |this, _: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.toggle_expanded(&id, cx);
                    }
                }))
                .child(if expanded { "▾" } else { "▸" })
                .into_any_element()
        } else {
            div().size(px(14.)).flex_none().into_any_element()
        });

        let badge = SharedString::from(row.badge.clone());
        let badge_image = crate::icon::is_image(&row.badge);
        leading = leading.child(
            div()
                .size(px(18.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .when(!badge_image, |style| {
                    style.rounded(px(4.)).bg(theme::badge_fill())
                })
                .child(crate::icon::render(
                    &badge,
                    18.0,
                    theme::ui_text_secondary(),
                )),
        );

        if let Some(accel) = settings::accel_char(&row.accel) {
            leading = leading.child(
                div()
                    .flex_none()
                    .w(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_secondary())
                    .child(accel.to_string()),
            );
        }

        let mut content = div()
            .id(SharedString::from(format!("item-{key}")))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(5.))
            .py(px(4.))
            .rounded(px(5.))
            .when(selected, |style| style.bg(theme::accent_soft()))
            // 整行都能选中：以前只有名称那一块挂了点击，点图标、点右边的灰字、
            // 点行尾的空白全都没反应，得瞄准才点得中。行内的按钮（展开 / 开关 /
            // 编辑 / 删除）照旧各干各的，它们会把点击拦在里面。
            .on_click(cx.listener({
                let id = row.id.clone();
                move |this, event: &ClickEvent, window, cx| {
                    // 双击 = 直接打开编辑窗口（和右键「编辑这一项…」同一个入口）。
                    // 单击还是只选一下 —— 拖动排序、框选都靠它。
                    if event.click_count() >= 2 {
                        this.select_only(&id);
                        window.focus(&this.focus);
                        crate::item_editor::open(&id, cx);
                        cx.notify();
                        return;
                    }
                    // Shift 连选 / Ctrl 加减选 / 直接点单选
                    this.select_click(&id, event.modifiers());
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let path = path.clone();
                    move |this, event: &MouseDownEvent, window, cx| {
                        // 点行就把键盘焦点收到列表上：不这么做，单选之后按 Ctrl+C
                        // 没有任何元素接得住这个键，看起来就是「复制没反应」
                        window.focus(&this.focus);
                        // 按住行体就能拖动排序（超过 DRAG_SLOP 才算拖动，否则还是「点一下」）。
                        // 曾经把拖动挪到左边一根把手上，好给「框选」腾地方 —— 结果整个
                        // 列表都不能拖了，用户直接报「菜单项不能拖动排序」。框选已经撤掉，
                        // 这里恢复成整行可拖。
                        this.press = Some(Press {
                            path: path.clone(),
                            start: event.position,
                        });
                        cx.notify();
                    }
                }),
            )
            // 右键：复制 / 粘贴 / 复制一份 / 隐藏 / 删除（和菜单里右键同一套命令）
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let id = row.id.clone();
                    move |this, _: &MouseDownEvent, _window, cx| {
                        this.context_row(&id, cx);
                    }
                }),
            );
        if drop_into {
            content = content
                .rounded(px(6.))
                .bg(theme::accent_soft())
                .border_1()
                .border_color(theme::accent());
        }
        if drop_before {
            content = content.border_t_2().border_color(theme::accent());
        }
        if drop_after {
            content = content.border_b_2().border_color(theme::accent());
        }

        let mut trailing = div().flex_none().flex().items_center().gap(px(8.));
        for (show, text) in [
            (!row.action_label.is_empty(), row.action_label.clone()),
            (!row.visibility.is_empty(), row.visibility.clone()),
        ] {
            if show {
                trailing = trailing.child(pill(text, theme::ui_text_tertiary()));
            }
        }
        // 空子菜单：明确告诉用户这一支现在有什么，不然它看起来和普通项一样。
        // 开着自动生成的那几支，内容在运行时（不写配置），这里显示生成了几项。
        if row.kind == ItemKind::Submenu && row.children_count == 0 {
            let auto = settings::read(|settings| {
                settings
                    .item_ref(&row.id)
                    .map(|item| (item.autofill.enabled, item.autofill.mode))
                    .unwrap_or((false, settings::AutoFillMode::Manual))
            });
            if auto.0 {
                let made = crate::autofill_runtime::count(&row.id).unwrap_or(0);
                let text = format!("{} · {made} {}", auto.1.label(), crate::i18n::t("项"));
                trailing = trailing.child(pill(text, theme::accent()));
            } else {
                trailing = trailing.child(pill(
                    crate::i18n::t("还没有菜单项").to_string(),
                    theme::danger(),
                ));
            }
        }
        if row.has_children {
            trailing = trailing.child(pill(
                if crate::i18n::is_english() {
                    format!("{} items", row.children_count)
                } else {
                    format!("{} 项", row.children_count)
                },
                theme::ui_text_tertiary(),
            ));
        }
        if !row.hotkey.is_empty() {
            trailing = trailing.child(
                div()
                    .flex_none()
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(theme::accent_soft())
                    .text_size(px(9.5))
                    .text_color(theme::accent())
                    .child(row.hotkey.clone()),
            );
        }
        // 局部生效的快捷键：和上面那颗全局键长得不一样 —— 一眼能看出「这颗只在
        // 某些窗口里按得动」；条件用小灰字接在后面。
        if !row.hotkey_local.is_empty() {
            let mut chip = div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(3.))
                .px(px(5.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(theme::badge_fill())
                .border_1()
                .border_color(theme::accent())
                .text_size(px(9.5))
                .text_color(theme::accent())
                .child("◐")
                .child(row.hotkey_local.clone());
            // 条件接在后面（截短）：不用点进去就知道「什么时候才按得动」
            if !row.local_when.is_empty() {
                chip = chip.child(
                    div()
                        .max_w(px(110.))
                        .truncate()
                        .text_color(theme::ui_text_tertiary())
                        .child(row.local_when.clone()),
                );
            }
            trailing = trailing.child(chip);
        }
        // 适用选中内容：只在「选中了这类东西」时才出现的那一条
        if !row.context.is_empty() {
            trailing = trailing.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(theme::accent_soft())
                    .text_size(px(9.5))
                    .text_color(theme::accent())
                    .child("◉")
                    .child(row.context.clone()),
            );
        }
        // 热字符串：敲这串字就能执行这一项（比快捷键更值得摆在列表上）
        if !row.hotstring.is_empty() {
            trailing = trailing.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(theme::badge_fill())
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_secondary())
                    .child("⌨")
                    .child(row.hotstring.clone()),
            );
        }
        trailing = trailing.child(
            div()
                .id(SharedString::from(format!("toggle-{key}")))
                .flex_none()
                .cursor_pointer()
                .on_click(cx.listener({
                    let path = path.clone();
                    move |_this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        settings::update(cx, |settings| {
                            if let Some(item) = settings.node_mut(&path) {
                                item.enabled = !item.enabled;
                            }
                        });
                        window.refresh();
                        cx.notify();
                    }
                }))
                .child(crate::settings_ui::switch(row.enabled)),
        );
        trailing = trailing.child(
            div()
                .id(SharedString::from(format!("edit-{key}")))
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .text_size(px(10.))
                .text_color(theme::ui_text_tertiary())
                .cursor_pointer()
                .hover(|style| {
                    style
                        .bg(theme::surface_hover())
                        .text_color(theme::text_primary())
                })
                .on_click(cx.listener({
                    let id = row.id.clone();
                    move |this, _: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.selected_id = Some(id.clone());
                        crate::item_editor::open(&id, cx);
                    }
                }))
                .child(
                    gpui::svg()
                        .path("icons/edit.svg")
                        .size(px(12.))
                        .flex_none()
                        .text_color(theme::ui_text_secondary()),
                ),
        );
        trailing = trailing.child(remove_button(path.clone(), cx));

        // 名称 + 备注（备注是灰色小字，最多占 40% 行宽，优先保证名称完整）
        let mut label_row = div()
            .id(SharedString::from(format!("label-{key}")))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer();
        label_row = label_row.child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(12.5))
                .text_color(if row.enabled {
                    theme::text_primary()
                } else {
                    theme::ui_text_tertiary()
                })
                .hover(|style| style.text_color(theme::accent()))
                .child(row.label.clone()),
        );
        if !row.note.is_empty() {
            label_row = label_row.child(
                div()
                    .flex_none()
                    .max_w(px(160.))
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(row.note.clone()),
            );
        }

        content = content.child(leading).child(label_row).child(trailing);

        measured_row(
            key,
            self.row_bounds.clone(),
            div()
                .w_full()
                .min_w_0()
                .pl(px(indent))
                .opacity(if dragging { 0.45 } else { 1.0 })
                .child(content),
        )
    }
}

fn pill(text: String, color: gpui::Rgba) -> AnyElement {
    div()
        .flex_none()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(theme::badge_fill())
        .text_size(px(9.))
        .text_color(color)
        .child(text)
        .into_any_element()
}

fn remove_button(path: Vec<usize>, cx: &mut Context<TreeView>) -> AnyElement {
    div()
        .id(SharedString::from(format!("rm-{}", path_key(&path))))
        .size(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .text_size(px(10.))
        .text_color(theme::ui_text_tertiary())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()).text_color(theme::danger()))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            cx.stop_propagation();
            settings::update(cx, |settings| settings.remove_at(&path));
            this.prune(cx);
            window.refresh();
            cx.notify();
        }))
        .child("✕")
        .into_any_element()
}

/// 把行的实际位置记下来（拖拽落点要按 y 换算，得知道每一行在屏幕上的位置）。
fn measured_row(
    key: String,
    bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    inner: Div,
) -> AnyElement {
    div()
        .w_full()
        .min_w_0()
        .relative()
        .child(inner)
        .child(
            canvas(
                |_, _, _| {},
                move |rect, _, _, _| {
                    bounds.borrow_mut().insert(key.clone(), rect);
                },
            )
            .absolute()
            .left_0()
            .top_0()
            .size_full(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> settings::ItemSetting {
        settings::ItemSetting::item(format!("id-{label}"), label)
    }

    #[test]
    fn the_search_matches_label_note_and_action_parameters() {
        let mut node = item("Notepad");
        node.note = "记得改端口".into();
        node.actions = vec![settings::ActionStep::new(
            settings::ActionKind::Run,
            "C:/Windows/notepad.exe",
        )];
        assert!(item_hits(&node, QuickFilter::All, "notepad"));
        assert!(item_hits(&node, QuickFilter::All, "端口"));
        assert!(item_hits(&node, QuickFilter::All, "windows"));
        assert!(!item_hits(&node, QuickFilter::All, "计算器"));
        // 空搜索词 = 不过滤
        assert!(item_hits(&node, QuickFilter::All, ""));
    }

    #[test]
    fn quick_filters_only_keep_what_they_say() {
        let plain = item("普通");
        let mut hot = item("有快捷键");
        hot.hotkey = "Ctrl+Alt+1".into();
        // 只写了「快捷键提示」的：菜单里会显示 Ctrl+N，但按下去没反应，
        // 所以「有快捷键」这一档不能把它算进来
        let mut hint_only = item("只有提示");
        hint_only.shortcut = "Ctrl+N".into();
        let mut noted = item("有备注");
        noted.note = "说明".into();
        let mut hidden = item("隐藏的");
        hidden.enabled = false;
        let mut parent = item("父项");
        parent.children = vec![item("子项")];
        parent.kind = ItemKind::Submenu;

        assert!(item_hits(&plain, QuickFilter::All, ""));
        assert!(!item_hits(&plain, QuickFilter::Hotkey, ""));
        assert!(item_hits(&hot, QuickFilter::Hotkey, ""));
        assert!(
            !item_hits(&hint_only, QuickFilter::Hotkey, ""),
            "只有提示、没配触发键的项不该出现在「有快捷键」里"
        );
        assert!(item_hits(&noted, QuickFilter::Note, ""));
        assert!(!item_hits(&plain, QuickFilter::Note, ""));
        assert!(item_hits(&hidden, QuickFilter::Hidden, ""));
        assert!(!item_hits(&plain, QuickFilter::Hidden, ""));
        assert!(item_hits(&parent, QuickFilter::Submenu, ""));
        assert!(!item_hits(&plain, QuickFilter::Submenu, ""));
    }

    #[test]
    fn the_filter_and_the_search_have_to_agree() {
        let mut node = item("计算器");
        node.note = "顺手".into();
        // 快捷筛选先过一道：没备注的项，搜「计算器」不会出现在「有备注」里
        let mut plain = item("计算器");
        plain.note = String::new();
        assert!(!item_hits(&plain, QuickFilter::Note, "计算器"));
        assert!(item_hits(&plain, QuickFilter::All, "计算器"));
        // 两个条件都满足才算命中（筛选 + 搜索词）
        assert!(item_hits(&node, QuickFilter::Note, "顺手"));
        assert!(!item_hits(&node, QuickFilter::Note, "浏览器"));
    }
}
