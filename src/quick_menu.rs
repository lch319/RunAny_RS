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
        // 圆角裁剪区域：这里再补一次（按客户区尺寸认出自己那扇窗，
        // 开窗那一刻拿到的句柄有时还没量出尺寸，设了也不生效）
        let want = (width as i32, height as i32);
        let mut mine = HWND.with(|slot| slot.get());
        if mine == 0 {
            // 按客户区尺寸认出自己那扇窗（开窗那一刻的差集有时是空的）
            mine = crate::win32::zed_window_handles()
                .into_iter()
                .find(|hwnd| crate::win32::client_size(*hwnd) == Some(want))
                .unwrap_or(0);
            if mine != 0 {
                HWND.with(|slot| slot.set(mine));
            }
        }
        if mine != 0 {
            crate::win32::set_rounded_corners(mine, scale);
            let _ = crate::win32::set_topmost(mine, "on");
            crate::win32::bring_to_front(mine);
        }
        // 先把基线等稳：右键那一下的点击计数会在这几十毫秒里变，
        // 立刻开始判「点外面」就会把它当成用户点走了（表现为「一闪就关」）
        cx.background_executor()
            .timer(std::time::Duration::from_millis(220))
            .await;
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

/// 关掉（如果开着）。
pub fn close(cx: &mut App) {
    let handle = WINDOW.with(|slot| slot.replace(None));
    if let Some(handle) = handle {
        let _ = handle.update(cx, |_view, window, _cx| window.remove_window());
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                        close(cx);
                        crate::popup::run_entry(id.clone(), label_for_click.clone(), cx);
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
