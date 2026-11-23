//! 置顶窗口的彩色边框（**Windows 10 也能用**）。
//!
//! Win11 有 `DWMWA_BORDER_COLOR` 可以一句话描边；Win10 没有。所以这里自己画：
//! 给目标窗口压一扇「只剩一圈」的透明窗口，后台线程每 25ms 让它跟着目标走。
//!
//! 思路和 GhostSnap.ahk 的 `win_SetWindowTop_xy` + `UpdateWindowBorder` 一致：
//!
//! - 覆盖窗口：`WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`
//!   （点得透、不抢焦点、不进任务栏）
//! - 形状：`SetWindowRgn` = 外框矩形 **减去** 内框矩形，只剩一圈，
//!   中间被挖空，填充色只留在边上
//! - 跟随：25ms 一轮，目标最小化 / 藏起来 / 关掉了就跟着隐藏或销毁
//!
//! 所有窗口操作都在同一个线程里做（窗口属于创建它的线程），外面只发命令。

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, CreateSolidBrush, DeleteObject, FillRect, SetWindowRgn, HBRUSH, HDC,
    RGN_DIFF,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetWindowRect,
    IsIconic, IsWindow, IsWindowVisible, PeekMessageW, RegisterClassW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, TranslateMessage, MSG, PM_REMOVE, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, WM_ERASEBKGND, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

const CLASS_NAME: &str = "RunAnyBorderWnd";
const LWA_ALPHA: u32 = 0x0000_0002;

enum Command {
    Set {
        target: isize,
        color: (u8, u8, u8),
        thickness: u32,
        alpha: u8,
    },
    Clear {
        target: isize,
    },
}

struct Overlay {
    hwnd: HWND,
    brush: HBRUSH,
    color: u32,
    thickness: u32,
    /// 上一次设过形状的尺寸：没变就不重建 region
    shaped: (i32, i32),
}

static SENDER: OnceLock<Sender<Command>> = OnceLock::new();

fn sender() -> &'static Sender<Command> {
    SENDER.get_or_init(|| {
        let (tx, rx) = channel::<Command>();
        let _ = std::thread::Builder::new()
            .name("runany-border".to_string())
            .spawn(move || run(rx));
        tx
    })
}

/// 给某个窗口加彩色边框（已经有了就更新颜色 / 粗细 / 透明度）。
pub fn set(target: isize, color: (u8, u8, u8), thickness: u32, alpha: u8) {
    if target == 0 {
        return;
    }
    let _ = sender().send(Command::Set {
        target,
        color,
        thickness: thickness.clamp(1, 24),
        alpha: alpha.clamp(20, 255),
    });
}

/// 去掉某个窗口的边框（取消置顶时调）。
pub fn clear(target: isize) {
    if target == 0 {
        return;
    }
    let _ = sender().send(Command::Clear { target });
}

fn colorref(color: (u8, u8, u8)) -> u32 {
    // COLORREF 是 0x00BBGGRR
    (color.2 as u32) << 16 | (color.1 as u32) << 8 | color.0 as u32
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

thread_local! {
    /// 覆盖窗口句柄 → 画刷（只在创建它的那个线程里访问）
    static BRUSHES: std::cell::RefCell<HashMap<isize, HBRUSH>> =
        std::cell::RefCell::new(HashMap::new());
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_ERASEBKGND {
        let brush = BRUSHES.with(|map| map.borrow().get(&(hwnd as isize)).copied());
        if let Some(brush) = brush {
            let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetClientRect(hwnd, &mut rect);
            FillRect(wparam as HDC, &rect, brush);
            return 1;
        }
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

fn run(rx: Receiver<Command>) {
    unsafe {
        let class = wide(CLASS_NAME);
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = GetModuleHandleW(std::ptr::null());
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
    }

    let mut overlays: HashMap<isize, Overlay> = HashMap::new();

    loop {
        while let Ok(command) = rx.try_recv() {
            match command {
                Command::Set { target, color, thickness, alpha } => {
                    let wanted = colorref(color);
                    match overlays.get_mut(&target) {
                        Some(overlay) => {
                            if overlay.color != wanted {
                                unsafe {
                                    DeleteObject(overlay.brush as _);
                                    let brush = CreateSolidBrush(wanted);
                                    BRUSHES.with(|map| {
                                        map.borrow_mut().insert(overlay.hwnd as isize, brush);
                                    });
                                    overlay.brush = brush;
                                }
                                overlay.color = wanted;
                                overlay.shaped = (0, 0);
                            }
                            if overlay.thickness != thickness {
                                overlay.thickness = thickness;
                                overlay.shaped = (0, 0);
                            }
                            unsafe {
                                SetLayeredWindowAttributes(overlay.hwnd, 0, alpha, LWA_ALPHA);
                            }
                        }
                        None => {
                            let hwnd = unsafe {
                                CreateWindowExW(
                                    WS_EX_LAYERED
                                        | WS_EX_TRANSPARENT
                                        | WS_EX_NOACTIVATE
                                        | WS_EX_TOOLWINDOW,
                                    wide(CLASS_NAME).as_ptr(),
                                    wide("").as_ptr(),
                                    WS_POPUP,
                                    0,
                                    0,
                                    0,
                                    0,
                                    std::ptr::null_mut(),
                                    std::ptr::null_mut(),
                                    GetModuleHandleW(std::ptr::null()),
                                    std::ptr::null_mut(),
                                )
                            };
                            if hwnd.is_null() {
                                continue;
                            }
                            let brush = unsafe { CreateSolidBrush(wanted) };
                            BRUSHES.with(|map| {
                                map.borrow_mut().insert(hwnd as isize, brush);
                            });
                            unsafe {
                                SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
                            }
                            overlays.insert(
                                target,
                                Overlay {
                                    hwnd,
                                    brush,
                                    color: wanted,
                                    thickness,
                                    shaped: (0, 0),
                                },
                            );
                        }
                    }
                }
                Command::Clear { target } => {
                    if let Some(overlay) = overlays.remove(&target) {
                        BRUSHES.with(|map| {
                            map.borrow_mut().remove(&(overlay.hwnd as isize));
                        });
                        unsafe {
                            DestroyWindow(overlay.hwnd);
                            DeleteObject(overlay.brush as _);
                        }
                    }
                }
            }
        }

        let targets: Vec<isize> = overlays.keys().copied().collect();
        for target in targets {
            let Some(overlay) = overlays.get_mut(&target) else {
                continue;
            };
            unsafe {
                if IsWindow(target as HWND) == 0
                    || IsWindowVisible(target as HWND) == 0
                    || IsIconic(target as HWND) != 0
                {
                    ShowWindow(overlay.hwnd, SW_HIDE);
                    continue;
                }
                let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                if GetWindowRect(target as HWND, &mut rect) == 0 {
                    ShowWindow(overlay.hwnd, SW_HIDE);
                    continue;
                }
                let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
                if width <= 0 || height <= 0 {
                    ShowWindow(overlay.hwnd, SW_HIDE);
                    continue;
                }
                SetWindowPos(
                    overlay.hwnd,
                    std::ptr::null_mut(),
                    rect.left,
                    rect.top,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                if overlay.shaped != (width, height) {
                    let thickness = overlay.thickness as i32;
                    let outer = CreateRectRgn(0, 0, width, height);
                    let inner = CreateRectRgn(
                        thickness,
                        thickness,
                        width - thickness,
                        height - thickness,
                    );
                    CombineRgn(outer, outer, inner, RGN_DIFF);
                    DeleteObject(inner as _);
                    SetWindowRgn(overlay.hwnd, outer, 1);
                    overlay.shaped = (width, height);
                }
                ShowWindow(overlay.hwnd, SW_SHOWNOACTIVATE);
            }
        }

        unsafe {
            let mut message: MSG = std::mem::zeroed();
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
