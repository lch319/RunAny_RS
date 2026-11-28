//! 独立的「快捷菜单」窗口：右键某个菜单项时弹的那种小列表。
//!
//! 为什么单独开一个窗口，而不是沿用主菜单那个：
//!
//! 主菜单是**单实例**的（一个 `MenuState` + 一组窗口），右键菜单以前是把它的
//! 内容换掉 —— 于是「置顶的菜单」会被右键菜单顶掉。用户要的是两个同时存在、
//! 互不干扰：置顶的那个留在原位，右键菜单另开一扇窗，不置顶、不抢前台，
//! 点自己外面就关自己。
//!
//! 这里的列表是**平的**（右键菜单本来也是平的），所以不需要级联、不需要
//! 窗口池，现开一扇窗就够。

use std::cell::RefCell;

use gpui::prelude::*;
use gpui::{
    div, px, size, App, Bounds, Context, IntoElement, MouseButton, Render, SharedString,
    Styled, Window, WindowBounds, WindowOptions,
};

use crate::menu::{MenuEntry, MenuItem};
use crate::theme;

const ROW_H: f32 = 26.0;
const PAD: f32 = 6.0;

thread_local! {
    static ENTRIES: RefCell<Vec<MenuItem>> = const { RefCell::new(Vec::new()) };
    static HOVER: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
    /// 这次弹出用的尺寸（渲染时要照着画，否则 GPUI 会按内容重算，宽度对不上）
    static SIZE: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((220.0, 120.0)) };
    /// 本窗口的 HWND：刚创建时可能还查不到，轮询里会再补
    static HWND: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static WINDOW: std::cell::Cell<Option<gpui::AnyWindowHandle>> = const { std::cell::Cell::new(None) };
}

/// 弹一个独立的快捷菜单（列表是平的）。已经在显示就先关掉旧的。
pub fn open(entries: Vec<MenuEntry>, cx: &mut App) {
    let items: Vec<MenuItem> = entries
        .into_iter()
        .filter_map(|entry| match entry {
            MenuEntry::Item(item) => Some(item),
            _ => None,
        })
        .collect();
    if std::env::var_os("GPUI_MENU_DEBUG_QUICK").is_some() {
        eprintln!("[quick] 打开：{} 条", items.len());
    }
    if items.is_empty() {
        return;
    }
    close(cx);

    let rows = items.len();
    let height = rows as f32 * ROW_H + PAD * 2.0;
    // 宽度按最长那条估：以前写死 220，长条目（「把选中的 1 个文件 / 文件夹加到这一项」）
    // 右边就被切掉了 —— 用户报的「右侧没显示完整」
    let widest = items
        .iter()
        .map(|item| {
            let text_w: f32 = item
                .label
                .chars()
                .map(|ch| if ch.is_ascii() { 7.0 } else { 12.5 })
                .sum();
            text_w + 14.0 + 8.0 + 20.0
        })
        .fold(0.0_f32, f32::max);
    let width = widest.clamp(180.0, 360.0).ceil();

    ENTRIES.with(|slot| *slot.borrow_mut() = items);
    SIZE.with(|slot| slot.set((width, height)));
    HOVER.with(|slot| slot.set(usize::MAX));

    let (cursor, screen, scale) = crate::cursor::anchor(cx);
    let mut x = f32::from(cursor.x);
    let mut y = f32::from(cursor.y);
    let screen_right = f32::from(screen.origin.x) + f32::from(screen.size.width);
    let screen_bottom = f32::from(screen.origin.y) + f32::from(screen.size.height);
    // 往右下弹，放不下就翻到左上
    if x + width > screen_right {
        x = (x - width).max(0.0);
    }
    if y + height > screen_bottom {
        y = (y - height).max(0.0);
    }

    let bounds = Bounds {
        origin: gpui::point(px(x), px(y)),
        size: size(px(width), px(height)),
    };
    let before = crate::win32::zed_window_handles();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            window_decorations: Some(gpui::WindowDecorations::Client),
            kind: gpui::WindowKind::PopUp,
            is_resizable: false,
            is_movable: false,
            window_min_size: Some(size(px(width), px(height))),
            focus: false,
            show: true,
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        |_window, cx| cx.new(QuickMenu::new),
    );
    let handle = match opened {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("[quick] 开窗失败：{err:?}");
            return;
        }
    };
    WINDOW.with(|slot| slot.set(Some(gpui::AnyWindowHandle::from(handle))));
    let _ = scale;
    // 用「创建前后差集」拿到这扇窗的 HWND（和菜单窗口池同一套办法）
    let hwnd = crate::win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        HWND.with(|slot| slot.set(hwnd));
        if crate::settings::read(|settings| settings.menu_no_focus) {
            crate::win32::set_no_activate(hwnd, true);
        }
        // 圆角是**窗口裁剪区域**实现的（不是 GPUI 的 rounded）：
        // 不设就成了直角方块，和主菜单放一起看着像另一个软件
        crate::win32::set_rounded_corners(hwnd, scale);
        // 主菜单本身是 TOPMOST，右键菜单不跟着置顶就会被它盖住（用户报的
        // 「右键菜单被置顶菜单挡住」）。这里也设 TOPMOST —— 但**不**代表「钉住」，
        // 它照样点外面就关。
        let _ = crate::win32::set_topmost(hwnd, "on");
    }

    // 点自己外面就关：这个窗口不抢前台，所以只能靠「鼠标点了几次 + 光标在哪」判断
    let task = cx.spawn(async move |cx| {
        // 先等这一下点击过去（右键那一刻的 click_tick 变化不能被当成「点走了」）
        cx.background_executor()
            .timer(std::time::Duration::from_millis(220))
            .await;
        // 现在窗口肯定已经画过一帧了：render 里会把 HWND 填进来
        let mut mine = HWND.with(|slot| slot.get());
        if mine == 0 {
            let want = ((width * scale).round() as i32, (height * scale).round() as i32);
            mine = crate::win32::zed_window_handles()
                .into_iter()
                .find(|hwnd| {
                    crate::win32::client_size(*hwnd).is_some_and(|(w, h)| {
                        (w - want.0).abs() <= 3 && (h - want.1).abs() <= 3
                    })
                })
                .unwrap_or(0);
            if mine != 0 {
                HWND.with(|slot| slot.set(mine));
            }
        }
        if mine == 0 {
            // 认不出自己那扇窗就没法判断「点外面」——留着只会变成关不掉的幽灵窗
            eprintln!("[quick] 认不出自己的窗口，直接关掉");
            let _ = cx.update(|cx| close(cx));
            return;
        }
        {
            crate::win32::set_rounded_corners(mine, scale);
            let _ = crate::win32::set_topmost(mine, "on");
            crate::win32::bring_to_front(mine);
        }
        let mut clicks = crate::win32::click_tick();
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(60))
                .await;
            let now = crate::win32::click_tick();
            if now != clicks {
                // 判「点外面」只认**本窗**：主菜单（置顶那个）也在我们的窗口里，
                // 用它来判断的话，点主菜单空白处就永远关不掉右键菜单
                let mine = HWND.with(|slot| slot.get());
                let outside = if mine != 0 {
                    !crate::win32::cursor_over_hwnd(mine)
                } else {
                    !crate::win32::cursor_over_self()
                };
                if outside {
                    let _ = cx.update(|cx| close(cx));
                    return;
                }
                clicks = now;
            }
        }
    });
    task.detach();
}

/// 把右键小窗重新抬到最前（主菜单展开子菜单时会被盖住）。
pub fn raise_if_open() {
    let hwnd = HWND.with(|slot| slot.get());
    if hwnd != 0 {
        let _ = crate::win32::set_topmost(hwnd, "on");
        crate::win32::bring_to_front(hwnd);
    }
}

/// 关掉（如果开着）。
pub fn close(cx: &mut App) {
    let handle = WINDOW.with(|slot| slot.replace(None));
    let hwnd = HWND.with(|slot| slot.replace(0));
    if let Some(handle) = handle {
        let removed = handle
            .update(cx, |_view, window, _cx| window.remove_window())
            .is_ok();
        if removed {
            return;
        }
    }
    // gpui 那边已经拉不动了（句柄过期/窗口被别的路径建过）：直接按 HWND 销毁，
    // 否则就留下一个「怎么点都关不掉」的窗口（用户报的就是这个）
    if hwnd != 0 {
        crate::win32::destroy_window(hwnd);
    }
}

pub struct QuickMenu;

impl QuickMenu {
    fn new(cx: &mut Context<Self>) -> Self {
        // 建好之后把 HWND 记下来（GPUI 不暴露裸句柄，用前后差集拿）
        let _ = cx;
        Self
    }
}

impl Render for QuickMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // HWND 直接从窗口对象拿（稳定），顺便把圆角 / 置顶 / 前台一次做完
        if let Some(hwnd) = crate::win32::hwnd_of(window) {
            if HWND.with(|slot| slot.get()) != hwnd {
                HWND.with(|slot| slot.set(hwnd));
                let scale = window.scale_factor();
                crate::win32::set_rounded_corners(hwnd, scale);
                if crate::settings::read(|settings| settings.menu_no_focus) {
                    crate::win32::set_no_activate(hwnd, true);
                }
                let _ = crate::win32::set_topmost(hwnd, "on");
                crate::win32::bring_to_front(hwnd);
            }
        }
        let items = ENTRIES.with(|slot| slot.borrow().clone());
        let (width, height) = SIZE.with(|slot| slot.get());
        let mut list = div()
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .p(px(PAD))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border())
            .shadow_sm()
            .rounded(px(8.));

        for (index, item) in items.into_iter().enumerate() {
            let hovered = HOVER.with(|slot| slot.get()) == index;
            let badge = item.badge.clone();
            let id = item.id.clone();
            let label = item.label.clone();
            let label_for_click = label.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("quick-{index}")))
                    .h(px(ROW_H))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(theme::text_primary())
                    .when(hovered, |style| style.bg(theme::row_selected()))
                    .on_hover(cx.listener(move |_this, hovered: &bool, _window, cx| {
                        HOVER.with(|slot| slot.set(if *hovered { index } else { usize::MAX }));
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |_this, _: &gpui::ClickEvent, _window, cx| {
                        // 先关窗、**下一帧再执行命令**：在窗口自己的点击回调里
                        // 把窗口销毁掉、紧接着又去开编辑器/跑动作，GPUI 会直接崩
                        // （用户报的「点条目就闪退」）
                        let id = id.clone();
                        let label = label_for_click.clone();
                        close(cx);
                        cx.defer(move |cx| crate::popup::run_entry(id, label, cx));
                    }))
                    .when_some(badge, |row, badge| {
                        row.child(
                            div()
                                .w(px(14.))
                                .flex_none()
                                .text_size(px(11.))
                                .text_color(theme::ui_text_tertiary())
                                .child(badge),
                        )
                    })
                    .child(label),
            );
        }

        list
    }
}
