//! 「实时生成 / 仅开启软件时」生成出来的菜单项，**只在内存里**。
//!
//! ## 为什么不写进配置
//!
//! 这两种方式的语义是「这一支的内容由文件夹说了算」。如果每次都把结果写进
//! `settings.json`，会连锁出三个毛病：
//!
//! 1. 呼出一次菜单就改一次配置 → 文件一直在变（用户开了 git / 网盘同步更明显）；
//! 2. 每次写入都 bump revision → 设置页那棵树跟着重建 → **行在闪**；
//! 3. 清空 + 重填产生的 id 每次都不同，选中项 / 展开状态全丢，看起来就是
//!    「呼出一次正常、再呼出一次子菜单里空了」的循环。
//!
//! 所以这里做一层「运行时叠加」：生成结果挂在 `子菜单 id → 项列表` 上，菜单构建
//! （`MenuSettings::entries_of`）时临时合并进去。配置里那一支永远是空的，
//! 手工加不了项，也不会被反复改写。
//!
//! 只影响**菜单怎么显示**；设置页照旧显示配置里的真实内容。

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::settings::ItemSetting;

/// 同一支多久之内不重复生成。
///
/// 「实时」说的是「用之前刷一次」，不是每一帧都刷：呼出菜单时会连着走几次
/// `entries_of`（量高度、算列宽、真正绘制），没有这个闸就会重复查索引。
/// 2 秒足够挡住这些重复调用，人眼看不出来。
const REGEN_TTL: Duration = Duration::from_secs(2);

struct Slot {
    items: Vec<ItemSetting>,
    at: Instant,
}

thread_local! {
    static SLOTS: RefCell<HashMap<String, Slot>> = RefCell::new(HashMap::new());
}

/// 有没有任何一支挂着运行时内容（`entries_of` 用它决定要不要走合并那条路）。
pub fn has_any() -> bool {
    SLOTS.with(|slots| !slots.borrow().is_empty())
}

/// 这一支现在挂着几项（设置页显示「已生成 N 项」用）。
pub fn count(id: &str) -> Option<usize> {
    SLOTS.with(|slots| slots.borrow().get(id).map(|slot| slot.items.len()))
}

/// 覆盖某一支的运行时内容。
pub fn set(id: &str, items: Vec<ItemSetting>) {
    SLOTS.with(|slots| {
        slots.borrow_mut().insert(
            id.to_string(),
            Slot {
                items,
                at: Instant::now(),
            },
        );
    });
}

/// 这一支刚生成过吗（TTL 内就不用再查一次索引）。
pub fn fresh(id: &str) -> bool {
    SLOTS.with(|slots| {
        slots
            .borrow()
            .get(id)
            .is_some_and(|slot| slot.at.elapsed() < REGEN_TTL)
    })
}

/// 清掉某一支（改了文件夹 / 后缀 / 关掉自动生成时用）。
pub fn clear(id: &str) {
    SLOTS.with(|slots| {
        slots.borrow_mut().remove(id);
    });
}

pub fn clear_all() {
    SLOTS.with(|slots| slots.borrow_mut().clear());
}

/// 把运行时内容合并进一个节点（只换子项，别的照旧）。
///
/// 返回 `None` 表示这一支没有运行时内容，调用方直接用原节点就行（省一次 clone）。
pub fn merge(item: &ItemSetting) -> Option<ItemSetting> {
    let items = SLOTS.with(|slots| slots.borrow().get(&item.id).map(|s| s.items.clone()))?;
    let mut node = item.clone();
    node.children = items;
    // 有内容就一定当子菜单显示（配置里这一支是空的）
    if !node.children.is_empty() {
        node.kind = crate::settings::ItemKind::Submenu;
    }
    Some(node)
}
