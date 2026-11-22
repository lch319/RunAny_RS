//! 计算菜单弹出的锚点：光标全局坐标 + 所在屏幕范围（逻辑像素）。

use gpui::{point, px, App, Bounds, Pixels, Point};

/// 兜底屏幕范围（拿不到显示器信息时使用）
fn fallback_bounds() -> Bounds<Pixels> {
    Bounds::new(point(px(0.0), px(0.0)), gpui::size(px(1280.0), px(800.0)))
}

/// 返回 (光标位置, 屏幕范围, 缩放比例)。位置/范围都是 GPUI 的逻辑像素。
///
/// 这里返回的是**客户区**该放的位置；Windows 上窗口还有一圈 8px 非客户区，
/// 换算成窗口矩形的位置是 `popup.rs` 里做的（减 `layout::FRAME_INSET`）。
pub fn anchor(cx: &mut App) -> (Point<Pixels>, Bounds<Pixels>, f32) {
    // 借任意一个已有窗口来拿「缩放比例」和「显示器范围」：
    // PlatformDisplay 只暴露 bounds()，缩放比例从 Window::scale_factor() 取。
    let mut display: Option<Bounds<Pixels>> = None;
    let mut scale = 1.0_f32;

    for handle in cx.windows() {
        let info = handle.update(cx, |_, window, cx| {
            (
                window.scale_factor(),
                window.display(cx).map(|display| display.bounds()),
            )
        });
        if let Ok((window_scale, bounds)) = info {
            scale = window_scale;
            display = bounds;
            break;
        }
    }

    let display = display.unwrap_or_else(fallback_bounds);
    // 扣掉任务栏：GPUI 的 display.bounds() 是整块屏幕，含任务栏那一条。
    // 不扣的话菜单最下面一截（含底部三角箭头）会藏在任务栏后面。
    let display = match crate::win32::work_area_insets() {
        Some((left, top, right, bottom)) if scale > 0.0 => {
            let (left, top) = (left / scale, top / scale);
            let (right, bottom) = (right / scale, bottom / scale);
            Bounds::new(
                point(display.origin.x + px(left), display.origin.y + px(top)),
                gpui::size(
                    px(f32::from(display.size.width) - left - right),
                    px(f32::from(display.size.height) - top - bottom),
                ),
            )
        }
        _ => display,
    };

    let position = match platform_cursor() {
        // 物理像素 -> 逻辑像素
        Some((x, y)) => point(px(x / scale), px(y / scale)),
        // 拿不到光标（非 Windows）就居中显示
        None => point(
            display.origin.x + display.size.width / 2.0,
            display.origin.y + display.size.height / 2.0,
        ),
    };

    (position, display, scale)
}

/// 只取缩放比例（预热窗口时用）。
pub fn scale_factor(cx: &mut App) -> f32 {
    anchor(cx).2
}

/// 光标全局坐标（物理像素）。其它平台返回 None，走「屏幕居中」兜底。
#[cfg(windows)]
fn platform_cursor() -> Option<(f32, f32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: GetCursorPos 只写我们提供的栈上 POINT，失败返回 0。
    let ok = unsafe { GetCursorPos(&mut point) };
    if ok != 0 {
        Some((point.x as f32, point.y as f32))
    } else {
        None
    }
}

#[cfg(not(windows))]
fn platform_cursor() -> Option<(f32, f32)> {
    None
}
