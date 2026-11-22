//! 菜单项的鼠标悬停提示。
//!
//! 鼠标在某一项上停够 `tooltip.delay_ms` 之后，在光标右下角弹一张小卡片。
//! 卡片上写什么由**模板**决定（见 `tip_template`），宽多少也归设置管。
//!
//! ## 为什么是一个独立窗口
//!
//! 提示要出现在菜单卡片**外面**（菜单列只有 200~360px 宽，写不下动作参数），
//! 而 GPUI 的绘制会被窗口边界裁掉，所以只能单开一个窗口。
//!
//! 这个窗口**一启动就建好藏起来**（和菜单窗口池一样）：`open_window` 一次
//! 要 300 多毫秒，那时候再建，提示就永远比鼠标慢半拍。
//!
//! ## 不能抢焦点
//!
//! 菜单靠「前台窗口还是不是自己」判断要不要收起（`popup.rs` 的 watchdog）。
//! 所以提示窗口一律用 `SWP_NOACTIVATE` 显示 —— 焦点始终留在菜单那一列上。
//!
//! ## 「悬停半天不出来」是怎么修的
//!
//! 以前用一个全局 `TICKET` 计数器防抖：每次悬停 `+1`，延迟醒来发现号变了就不弹。
//! 问题是「离开 A 行 → 进入 B 行」这两个事件**先后顺序不保证**：
//! 如果 B 的 enter 先到（排上号），A 的 leave 后到（又 `+1`），
//! B 那次等待就被自己前面那一行的 leave 作废了 —— 表现就是「有时弹、有时不弹」。
//!
//! 现在改成记「**待弹的是哪一项**」：只有同一项的 leave 才撤掉它自己的等待，
//! 别的行的 leave 一律不影响。另外给行加了 `on_mouse_move` 兜底：
//! 万一 enter 事件本身漏了（比如菜单刚好在光标底下弹出来），下一次鼠标移动会补上。

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, AnyWindowHandle, App, AppContext, Bounds, Context, IntoElement, Render, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};

use crate::settings;
use crate::theme;
use crate::tip_template::{self, TipAlign, TipContext, TipLine};
use crate::win32;

/// 内边距
const PAD: f32 = 9.0;
/// 标题行高
const TITLE_H: f32 = 21.0;
/// 正文字号 / 行高
const TEXT_SIZE: f32 = 11.5;
const LINE_H: f32 = 19.0;
/// 行与行之间的空隙
const LINE_GAP: f32 = 2.0;
/// 光标右下角的偏移
const OFFSET: (f32, f32) = (16.0, 20.0);
/// 卡片最窄多少（再窄就不好看了）
const MIN_W: f32 = 120.0;

thread_local! {
    /// 每行量出来的高度（渲染时按它给行高；长了会折行，所以不能写死）
    static HEIGHTS: std::cell::RefCell<Vec<f32>> = const { std::cell::RefCell::new(Vec::new()) };
}

thread_local! {
    /// 提示卡上现在要显示的几行（渲染时从这儿取）。
    static LINES: std::cell::RefCell<Vec<TipLine>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// 提示卡的根视图。内容全在 thread-local `LINES` 里 —— 只有一个提示窗口，
/// 没必要为它维护一个 Entity + 订阅，`window.refresh()` 一刷就重新读一遍。
struct Tooltip;

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // 卡片尺寸直接用「量出来的那个数」，**不用** `size_full()`。
        //
        // 原因：窗口是用 `SetWindowPos` 改的（不能用 `window.resize()`，见 `show()`），
        // 改完这一帧 gpui 手里还是旧 bounds —— 按 `size_full()` 排版就会拿一个
        // 一千多像素宽的旧尺寸去排，文本不折行、整张卡片被窗口裁掉。
        // 写死宽高之后，排版和窗口大小脱钩，第一帧就是对的。
        let (card_w, card_h) = HOST.with(|host| {
            let host = host.borrow();
            host.size
        });
        let mut card = div()
            .w(px(card_w))
            .h(px(card_h))
            .flex()
            .flex_col()
            .gap(px(LINE_GAP))
            .px(px(PAD))
            .py(px(PAD))
            .rounded(px(7.))
            .bg(theme::surface_elevated())
            .border_1()
            .border_color(theme::border_strong())
            .font_family(theme::font_family())
            .text_color(theme::text_primary())
            .overflow_hidden();

        let lines = LINES.with(|lines| lines.borrow().clone());
        let heights = HEIGHTS.with(|heights| heights.borrow().clone());
        for (index, line) in lines.iter().enumerate() {
            let title = line.title;
            // 行高来自测量（内容超过最大卡片宽度时会折行，行高就不是固定值了）
            let row_h = heights
                .get(index)
                .copied()
                .unwrap_or(if title { TITLE_H } else { LINE_H });
            // 对齐用 flex 的 justify 做（不是补空格）：比例字体里补空格对不齐
            let mut row = div()
                .w_full()
                .min_w_0()
                .h(px(row_h))
                .flex()
                .items_center()
                .flex_none()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_size(px(if title { 12.5 } else { TEXT_SIZE }))
                        .when(title, |style| style.font_weight(gpui::FontWeight::SEMIBOLD))
                        .text_color(if title {
                            theme::text_primary()
                        } else {
                            theme::text_secondary()
                        })
                        .child(line.text.clone()),
                );
            row = match line.align {
                TipAlign::Left | TipAlign::Custom => row.justify_start(),
                TipAlign::Center => row.justify_center(),
                TipAlign::Right => row.justify_end(),
            };
            card = card.child(row);
        }
        card
    }
}

struct Host {
    handle: Option<AnyWindowHandle>,
    hwnd: Option<isize>,
    visible: bool,
    /// 现在显示的是哪一项（同一个 id 不重复摆）
    shown_id: Option<String>,
    size: (f32, f32),
    scale: f32,
    building: bool,
}

thread_local! {
    static HOST: Rc<std::cell::RefCell<Host>> = Rc::new(std::cell::RefCell::new(Host {
        handle: None,
        hwnd: None,
        visible: false,
        shown_id: None,
        size: (288.0, 60.0),
        scale: 1.0,
        building: false,
    }));
}

// 等着弹的那一项（`id` + 这一次等待的号）。
thread_local! {
    static PENDING: std::cell::RefCell<Option<(String, u64)>> =
        const { std::cell::RefCell::new(None) };
}

/// 每次悬停换一个号；延迟醒来时号变了就说明这一项已经被换掉了。
static TICKET: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// 「光标还在不在菜单上」的轮询跑着没有（只有一个，别每个提示起一个）
    static WATCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 启动时把提示窗口建好藏起来（跟着窗口池后面跑，不抢启动时间）。
pub fn install(cx: &mut App) {
    if !cfg!(windows) {
        return;
    }
    let scale = crate::cursor::scale_factor(cx);
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(400))
            .await;
        let _ = cx.update(move |app| ensure_window(app, scale));
    })
    .detach();
}

fn ensure_window(cx: &mut App, scale: f32) -> bool {
    if HOST.with(|host| host.borrow().handle.is_some()) {
        return true;
    }
    if HOST.with(|host| host.borrow().building) {
        return false;
    }
    HOST.with(|host| host.borrow_mut().building = true);

    let width = tip_width();
    let bounds = Bounds::new(gpui::point(px(0.), px(0.)), gpui::size(px(width), px(60.)));
    let before = win32::zed_window_handles();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            window_decorations: Some(WindowDecorations::Client),
            kind: WindowKind::PopUp,
            focus: false,
            // 一开始就藏起来：还没内容的时候闪一下比不弹更难看
            show: false,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        |_window, cx| cx.new(|_cx| Tooltip),
    );
    HOST.with(|host| host.borrow_mut().building = false);
    let Ok(handle) = opened else {
        return false;
    };
    let hwnd = win32::zed_window_handles()
        .into_iter()
        .find(|candidate| !before.contains(candidate));
    if let Some(hwnd) = hwnd {
        win32::set_rounded_corners(hwnd, scale);
        // 先把窗口收成提示卡真正的大小：GPUI 建窗用的是 `CW_USEDEFAULT`，
        // 真实窗口是整块屏幕那么大（1424×744 的交换链光缓冲就 8 MB），
        // 常驻着白占内存。提示卡弹出前本来就要 resize，收小没有任何代价。
        win32::resize_window(hwnd, width, 60.0, scale);
        win32::hide_window(hwnd);
        // 和菜单窗口池一样：alpha=0 时先画一帧，把 GPUI 的渲染器点着，
        // 这样第一次悬停不用等那两三百毫秒（反正全透明，看不见）。
        win32::set_window_opacity(hwnd, 0.0);
        win32::show_window(hwnd);
        let _ = handle.update(cx, |_, window, _| window.refresh());
        win32::hide_window(hwnd);
        win32::set_window_opacity(hwnd, 1.0);
    }
    HOST.with(|host| {
        let mut host = host.borrow_mut();
        host.handle = Some(handle.into());
        host.hwnd = hwnd;
        host.scale = scale;
        host.size = (width, 60.0);
    });
    true
}

/// 提示卡的宽度（设置里可以拖）。
fn tip_width() -> f32 {
    let (min, max) = settings::TooltipSettings::limits_width();
    settings::read(|settings| settings.tooltip.width).clamp(min, max)
}

/// 鼠标停到某一项上：`delay` 毫秒后弹提示。
///
/// 同一项重复调用是**幂等**的（不会把已经开始的等待重新计时）。
pub fn schedule(cx: &mut App, id: &str) {
    let cfg = settings::read(|settings| settings.tooltip.clone());
    if !cfg.enabled || id.starts_with("__") {
        cancel(cx, id);
        return;
    }
    // 同一项已经显示着：不动它
    if HOST.with(|host| {
        let host = host.borrow();
        host.visible && host.shown_id.as_deref() == Some(id)
    }) {
        return;
    }
    // 显示着**别的**项：先收掉再排新的。
    //
    // 鼠标从 A 行滑到 B 行时，A 的 `on_hover(false)` 不一定先到（甚至可能根本不来），
    // 不在这里收一下就会留着 A 的提示卡在屏幕上，看着像「提示不消失」。
    if HOST.with(|host| host.borrow().visible) {
        hide();
    }
    // 已经在等这一项了：别重新计时（否则鼠标一动就永远等不到）
    if PENDING.with(|pending| {
        pending
            .borrow()
            .as_ref()
            .is_some_and(|(pending_id, _)| pending_id == id)
    }) {
        return;
    }

    let ticket = TICKET.fetch_add(1, Ordering::SeqCst) + 1;
    let id = id.to_string();
    PENDING.with(|pending| *pending.borrow_mut() = Some((id.clone(), ticket)));
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(cfg.delay_ms.max(60)))
            .await;
        let still_wanted = PENDING.with(|pending| {
            let mut pending = pending.borrow_mut();
            match pending.as_ref() {
                Some((pending_id, pending_ticket))
                    if pending_id == &id && *pending_ticket == ticket =>
                {
                    *pending = None;
                    true
                }
                _ => false,
            }
        });
        if !still_wanted {
            return;
        }
        let _ = cx.update(move |app| show(app, &id));
    })
    .detach();
}

/// 鼠标在这一行里动了，但没收到 enter 事件时的兜底：只在「什么都没在等」时补一次。
pub fn nudge(cx: &mut App, id: &str) {
    let idle = PENDING.with(|pending| pending.borrow().is_none())
        && !HOST.with(|host| host.borrow().visible);
    if idle {
        schedule(cx, id);
    }
}

/// 鼠标离开某一项：撤掉**它自己**那次等待，并把已经显示着的收起来。
pub fn cancel(cx: &mut App, id: &str) {
    let _ = cx;
    let hit = PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending
            .as_ref()
            .is_some_and(|(pending_id, _)| pending_id == id)
        {
            *pending = None;
            true
        } else {
            false
        }
    });
    if hit {
        TICKET.fetch_add(1, Ordering::SeqCst);
    }
    let showing = HOST.with(|host| host.borrow().shown_id.as_deref() == Some(id));
    if showing {
        hide();
    }
}

/// 直接把提示收起来（菜单关了 / 整个列的高亮没了的时候）。
pub fn hide_now() {
    TICKET.fetch_add(1, Ordering::SeqCst);
    PENDING.with(|pending| *pending.borrow_mut() = None);
    if HOST.with(|host| host.borrow().visible) {
        hide();
    }
}

fn hide() {
    HOST.with(|host| {
        let mut host = host.borrow_mut();
        if let Some(hwnd) = host.hwnd {
            win32::hide_window(hwnd);
        }
        host.visible = false;
        host.shown_id = None;
    });
}

/// 调试用：不走延时，直接把某一项的提示摆出来（看版式用）。
pub fn show_now(cx: &mut App, id: &str) {
    show(cx, id);
}

/// 量出「卡片要多宽、每行要多高」。
///
/// 宽度取最长那一行的自然宽度（夹在 `MIN_W` 和设置里的「最大卡片宽度」之间）；
/// 单行超过最大宽度时按 `wrap_width` 折行，所以行高由换行后的行数决定。
fn measure(
    cx: &mut App,
    handle: AnyWindowHandle,
    lines: &[TipLine],
    max_width: f32,
) -> (f32, Vec<f32>) {
    let wrap_w = (max_width - PAD * 2.0).max(60.0);
    let fallback = || {
        (
            max_width,
            lines
                .iter()
                .map(|line| if line.title { TITLE_H } else { LINE_H })
                .collect::<Vec<f32>>(),
        )
    };
    let measured = handle.update(cx, |_, window, _| {
        let text_system = window.text_system().clone();
        // `gpui::Font` 没有 `Default`，只能从 `font(family)` 起手再盖字段
        let base = gpui::font(theme::font_family());
        let mut widest: f32 = 0.0;
        let mut heights: Vec<f32> = Vec::with_capacity(lines.len());
        for line in lines {
            let size = px(if line.title { 12.5 } else { TEXT_SIZE });
            let line_h = px(if line.title { TITLE_H } else { LINE_H });
            let font = gpui::Font {
                weight: if line.title {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                },
                ..base.clone()
            };
            let run = gpui::TextRun {
                len: line.text.len(),
                font,
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = text_system.shape_text(
                line.text.clone().into(),
                size,
                &[run],
                Some(px(wrap_w)),
                None,
            );
            match shaped {
                Ok(shaped) => {
                    let mut width: f32 = 0.0;
                    let mut height: f32 = 0.0;
                    for shaped_line in &shaped {
                        width = width.max(f32::from(shaped_line.width()));
                        // 用真实字形高度（ascent + descent），而不是写死的行高：
                        // 11.5px 的中文（尤其中英混排）字形高度会超过 16px 的固定行高，
                        // 行高不够时文字在渲染端溢出、和下一行叠在一起（详见截图）。
                        let glyph_h = f32::from(shaped_line.ascent())
                            + f32::from(shaped_line.descent());
                        height += glyph_h.max(f32::from(line_h));
                    }
                    widest = widest.max(width);
                    heights.push(height.max(f32::from(line_h)));
                }
                Err(_) => heights.push(f32::from(line_h)),
            }
        }
        (widest, heights)
    });
    let Ok((widest, heights)) = measured else {
        return fallback();
    };
    let width = (widest + PAD * 2.0).clamp(MIN_W, max_width);
    (width, heights)
}

fn show(cx: &mut App, id: &str) {
    let (lines, cfg) = settings::read(|settings| {
        let cfg = settings.tooltip.clone();
        let Some(path) = settings.find_path(id) else {
            return (Vec::new(), cfg);
        };
        let Some(item) = settings.node(&path) else {
            return (Vec::new(), cfg);
        };
        let trail = settings.trail_labels(&path).join(" › ");
        let context = TipContext::of(item, &trail);
        let mut lines = tip_template::render(&cfg.template, &context, cfg.align);
        // 「运行程序」里只写了裸文件名的，把解析出来的完整路径补一行 ——
        // 菜单上看到的只是 `explorer.exe`，谁知道它到底会启动哪一个
        if let Some(full) = item.tooltip_path() {
            lines.push(crate::tip_template::TipLine {
                text: full,
                align: cfg.align,
                title: false,
            });
        }
        (lines, cfg)
    });
    if !cfg.enabled || lines.is_empty() {
        return;
    }

    let scale = HOST.with(|host| host.borrow().scale);
    if !ensure_window(cx, scale) {
        return;
    }

    let (handle, hwnd) = HOST.with(|host| {
        let host = host.borrow();
        (host.handle, host.hwnd)
    });
    let Some(handle) = handle else { return };

    // 宽度：先按「最长那一行」算自然宽度，再夹到最大卡片宽度以内。
    // 超过最大宽度的行会自动折行，所以高度也得跟着测量结果走（不能按行数拍脑袋）。
    let max_width = tip_width();
    let (width, heights) = measure(cx, handle, &lines, max_width);
    if std::env::var_os("GPUI_MENU_DEBUG_TIPLOG").is_some() {
        let bounds = handle
            .update(cx, |_, window, _| window.bounds())
            .unwrap_or_else(|_| Bounds::new(gpui::point(px(0.), px(0.)), gpui::size(px(0.), px(0.))));
        eprintln!(
            "[tooltip] max={max_width} got=({width}, {:?}) heights={:?} window={:?}",
            heights.iter().sum::<f32>(),
            heights,
            bounds
        );
        for line in &lines {
            eprintln!("  line({}) {:?}", line.text.len(), line.text);
        }
    }
    let height = PAD * 2.0
        + heights.iter().sum::<f32>()
        + LINE_GAP * lines.len().saturating_sub(1) as f32
        + 2.0;
    let (cursor_pos, screen, _) = crate::cursor::anchor(cx);
    let mut x = f32::from(cursor_pos.x) + OFFSET.0;
    let mut y = f32::from(cursor_pos.y) + OFFSET.1;
    // 右/下放不下就翻到左边/上面，别跑出屏幕
    let screen_right = f32::from(screen.origin.x) + f32::from(screen.size.width);
    let screen_bottom = f32::from(screen.origin.y) + f32::from(screen.size.height) - 8.0;
    if x + width > screen_right {
        x = (f32::from(cursor_pos.x) - width - 8.0).max(f32::from(screen.origin.x) + 4.0);
    }
    if y + height > screen_bottom {
        y = (screen_bottom - height).max(f32::from(screen.origin.y) + 4.0);
    }

    HOST.with(|host| {
        let mut host = host.borrow_mut();
        host.shown_id = Some(id.to_string());
        host.size = (width, height);
    });

    // 内容先塞进去，再摆窗口：顺序反了会看到上一次的旧内容
    LINES.with(|current| *current.borrow_mut() = lines);
    HEIGHTS.with(|current| *current.borrow_mut() = heights);
    let foreground = win32::foreground_window();
    if let Some(hwnd) = hwnd {
        // **不用 `window.resize()`**：它内部那次 `SetWindowPos` 没带
        // `SWP_NOACTIVATE`，会把提示自身抢成前台，菜单 watchdog 立刻判定
        // 「用户点走了」把整张菜单收掉（实测必现）。这里改用不激活的那版。
        // 用**这扇提示窗口**的实际 DPI（而不是主窗口的）：多屏不同缩放、
        // 或者系统是 175% 这种小数缩放时，拿错比例就会少算几个像素、最后一行被裁
        win32::fit_client_size(hwnd, width, height, scale);
        win32::move_window(
            hwnd,
            win32::px_to_physical(x - crate::layout::FRAME_INSET, scale),
            win32::px_to_physical(y, scale),
        );
        win32::show_window(hwnd);
        // 提示要压在菜单（TOPMOST）上面
        win32::raise_window(hwnd);
    }
    let _ = handle.update(cx, |_, window, _| window.refresh());
    // 兜底：万一还是有哪一步把前台抢过来了，立刻还给菜单 ——
    // 菜单一失焦，方向键就按不动了。
    if foreground != 0 && win32::foreground_window() != foreground {
        win32::set_foreground(foreground);
    }
    HOST.with(|host| host.borrow_mut().visible = true);
    watch_cursor(cx);
}

/// 光标离开菜单之后把提示收掉。
///
/// 为什么需要轮询：提示是**独立窗口**，鼠标从菜单直接移到桌面 / 别的程序时，
/// 菜单那一行收到的 `on_hover(false)` 并不保证送达（GPUI 只有在鼠标经过某个
/// 元素时才更新悬停状态，直接移出窗口就没人告诉它）。表现就是「提示一直挂着」。
/// 这里每 250ms 问一次系统「光标下还是我们自己的窗口吗」，不是就收。
fn watch_cursor(cx: &mut App) {
    if WATCHING.with(|flag| flag.replace(true)) {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            if !HOST.with(|host| host.borrow().visible) {
                break;
            }
            if win32::cursor_over_self() {
                continue;
            }
            let _ = cx.update(|_app: &mut App| hide());
            break;
        }
        WATCHING.with(|flag| flag.set(false));
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{ActionKind, ActionStep, ItemSetting};

    fn item() -> ItemSetting {
        let mut node = ItemSetting::item("a".to_string(), "记事本");
        node.actions = vec![ActionStep::new(ActionKind::Run, "notepad.exe")];
        node
    }

    fn render(node: &ItemSetting, cfg: &settings::TooltipSettings) -> Vec<TipLine> {
        let context = TipContext::of(node, "");
        tip_template::render(&cfg.template, &context, cfg.align)
    }

    #[test]
    fn the_label_always_comes_first() {
        let cfg = settings::TooltipSettings::default();
        let lines = render(&item(), &cfg);
        assert_eq!(lines[0].text, "记事本");
        assert!(lines[0].title);
    }

    #[test]
    fn a_bare_template_shows_only_the_label() {
        let cfg = settings::TooltipSettings {
            template: "{label}".to_string(),
            ..settings::TooltipSettings::default()
        };
        let lines = render(&item(), &cfg);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn actions_show_up_when_the_template_asks_for_them() {
        let cfg = settings::TooltipSettings::default();
        let lines = render(&item(), &cfg);
        assert!(lines.iter().any(|line| line.text.starts_with("1. ")));
    }

    #[test]
    fn the_old_flags_migrate_into_a_template() {
        let mut cfg = settings::TooltipSettings {
            template: String::new(),
            show_keys: false,
            show_note: false,
            show_meta: false,
            show_actions: false,
            ..settings::TooltipSettings::default()
        };
        assert!(cfg.migrate());
        assert_eq!(cfg.template, "{label}\n?{hidden}");
        // 再迁一次什么也不做
        assert!(!cfg.migrate());
    }
}
