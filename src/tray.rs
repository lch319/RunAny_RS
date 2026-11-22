//! 系统托盘图标。
//!
//! 有了托盘之后程序可以「真正待在后台」：启动不弹主窗口、不闪黑框，
//! 左键点图标弹菜单、右键点图标弹托盘菜单（和弹出菜单同一套渲染 + 同一套菜单项）。
//!
//! ## 为什么单独一个线程
//!
//! `Shell_NotifyIcon` 要求有一个窗口去收回调消息，而收消息必须跑自己的消息循环
//! （`GetMessage` 是阻塞的）。GPUI 有自己的事件循环，塞进去会互相打架，所以这里
//! 单开一个线程：建一个**隐藏窗口** + 注册托盘图标 + 跑 `GetMessage` 循环。
//!
//! 跨线程只用两样东西：
//! - [`CONFIG`]（`Mutex`）：主线程把「要不要显示 / 用哪个图标 / 提示文字」写进去，
//!   再 `PostMessage(WM_APP_REFRESH)` 把线程叫醒；
//! - `async_channel`：线程把「左键 / 右键」丢回 GPUI，由 GPUI 决定弹什么。

use std::sync::{Mutex, OnceLock};

use gpui::App;

use crate::settings;

/// 托盘图标发给我们的事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    LeftClick,
    RightClick,
}

/// 托盘当前该长什么样（设置一变就推一份过去）。
#[derive(Clone, Debug, Default)]
pub(crate) struct TrayConfig {
    pub enabled: bool,
    /// `.ico` / `.png` / `.exe` 路径（空串 = 内置图标）
    pub icon: String,
    /// 鼠标悬停提示
    pub tip: String,
}

static CONFIG: Mutex<Option<TrayConfig>> = Mutex::new(None);
static SENDER: OnceLock<async_channel::Sender<TrayEvent>> = OnceLock::new();

/// 启动时调一次：起托盘线程 + 把点击事件接进 GPUI。
pub fn install(cx: &mut App) {
    let (tx, rx) = async_channel::bounded::<TrayEvent>(8);
    let _ = SENDER.set(tx.clone());
    // 先把配置写好再起线程：线程建完窗口会立刻按 CONFIG 摆一次，
    // 反过来（先起线程）会有一瞬间读到空配置，托盘图标就装不上了。
    refresh(cx);
    imp::spawn(tx);

    cx.spawn(async move |cx| {
        while let Ok(event) = rx.recv().await {
            let _ = cx.update(move |app| dispatch(event, app));
        }
    })
    .detach();
}

/// 设置改了就调一次（幂等：没变化也只是重设一遍图标）。
pub fn refresh(cx: &mut App) {
    let _ = cx;
    let config = settings::read(|settings| TrayConfig {
        enabled: settings.tray_enabled,
        icon: settings.tray_icon.clone(),
        tip: if crate::i18n::is_english() {
            format!(
                "Open menu ({}) · left click to open, right click for more",
                settings.popup_hotkey.trim()
            )
        } else {
            format!(
                "弹出菜单（{}）· 左键打开，右键更多",
                settings.popup_hotkey.trim()
            )
        },
    });
    imp::apply(config);
}

fn dispatch(event: TrayEvent, cx: &mut App) {
    match event {
        TrayEvent::LeftClick => match settings::read(|settings| settings.tray_left_click) {
            settings::TrayLeftClick::PopupMenu => crate::popup::open(cx),
            settings::TrayLeftClick::TrayMenu => crate::popup::open_tray(cx),
            settings::TrayLeftClick::Nothing => {}
        },
        TrayEvent::RightClick => crate::popup::open_tray(cx),
    }
}

// ---------------------------------------------------------------------------
// Windows 实现
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NOTIFYICONDATAW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, CreateWindowExW, DefWindowProcW, DestroyIcon, DispatchMessageW,
        GetMessageW, LoadImageW, PostMessageW, PostQuitMessage, RegisterClassW, TranslateMessage,
        HICON, ICONINFO, IMAGE_ICON, LR_LOADFROMFILE, MSG, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP,
        WNDCLASSW,
    };

    /// 托盘图标把鼠标事件发到这个消息上（`WM_APP + 1`）
    const WM_APP_TRAY: u32 = 0x8000 + 1;
    /// 主线程改了设置，往这个线程发一条「重新读配置」
    const WM_APP_REFRESH: u32 = 0x8000 + 2;
    const TRAY_UID: u32 = 1;

    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static ADDED: AtomicBool = AtomicBool::new(false);
    static CURRENT_ICON: AtomicIsize = AtomicIsize::new(0);

    /// 起线程：建隐藏窗口 + 注册托盘 + 跑消息循环。
    pub fn spawn(sender: async_channel::Sender<super::TrayEvent>) {
        let _ = sender; // 事件统一走 `super::SENDER`，这里只是保证 install 里那个 sender 有人持有
        std::thread::spawn(|| unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class_name: Vec<u16> = "GpuiPopupMenuTray\0".encode_utf16().collect();
            let class: WNDCLASSW = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            RegisterClassW(&class);
            // 普通顶层窗口，但不显示（只要它能收消息就够了）
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                eprintln!("[popup-menu] 托盘窗口创建失败");
                return;
            }
            WINDOW.store(hwnd as isize, Ordering::Relaxed);
            apply_now();

            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        });
    }

    /// 主线程：写配置 + 把线程叫醒。
    pub fn apply(config: super::TrayConfig) {
        if let Ok(mut slot) = super::CONFIG.lock() {
            *slot = Some(config);
        }
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if hwnd != 0 {
            unsafe { PostMessageW(hwnd as HWND, WM_APP_REFRESH, 0, 0) };
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_APP_TRAY => {
                let event = (lparam as u32) & 0xffff;
                let event = match event {
                    WM_LBUTTONUP => Some(super::TrayEvent::LeftClick),
                    WM_RBUTTONUP => Some(super::TrayEvent::RightClick),
                    _ => None,
                };
                if let (Some(event), Some(sender)) = (event, super::SENDER.get()) {
                    let _ = sender.send_blocking(event);
                }
                0
            }
            WM_APP_REFRESH => {
                apply_now();
                0
            }
            WM_DESTROY => {
                remove_icon();
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn remove_icon() {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if hwnd == 0 || !ADDED.swap(false, Ordering::Relaxed) {
            return;
        }
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd as HWND;
        data.uID = TRAY_UID;
        Shell_NotifyIconW(NIM_DELETE, &data);
    }

    /// 按 `CONFIG` 把托盘图标装 / 改 / 摘掉。
    unsafe fn apply_now() {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if hwnd == 0 {
            return;
        }
        let config = super::CONFIG
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .unwrap_or_default();

        if !config.enabled {
            if ADDED.swap(false, Ordering::Relaxed) {
                let mut data: NOTIFYICONDATAW = std::mem::zeroed();
                data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                data.hWnd = hwnd as HWND;
                data.uID = TRAY_UID;
                Shell_NotifyIconW(NIM_DELETE, &data);
            }
            return;
        }

        let icon = make_icon(&config.icon);
        let mut tip = [0u16; 128];
        for (index, unit) in config.tip.encode_utf16().take(127).enumerate() {
            tip[index] = unit;
        }

        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd as HWND;
        data.uID = TRAY_UID;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_APP_TRAY;
        data.hIcon = icon;
        data.szTip = tip;

        let was_added = ADDED.load(Ordering::Relaxed);
        let ok = Shell_NotifyIconW(if was_added { NIM_MODIFY } else { NIM_ADD }, &data);
        if ok == 0 {
            // 没装上（资源管理器还没准备好之类）：把图标句柄还掉，下次再试
            if !icon.is_null() {
                DestroyIcon(icon);
            }
            return;
        }
        ADDED.store(true, Ordering::Relaxed);
        let old = CURRENT_ICON.swap(icon as isize, Ordering::Relaxed);
        if old != 0 && old != icon as isize {
            DestroyIcon(old as HICON);
        }
    }

    /// 用配置里的图标；读不到（或者没配）就用内置画的那一个。
    unsafe fn make_icon(path: &str) -> HICON {
        if !path.trim().is_empty() {
            let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            let icon = LoadImageW(
                std::ptr::null_mut(),
                wide.as_ptr(),
                IMAGE_ICON,
                32,
                32,
                LR_LOADFROMFILE,
            );
            if !icon.is_null() {
                return icon as HICON;
            }
        }
        builtin_icon(32)
    }

    /// 内置图标：深色圆角方块 + 三道横线（「菜单」那个意思）。
    unsafe fn builtin_icon(size: u32) -> HICON {
        icon_from_rgba(size, &builtin_rgba(size))
    }

    /// 程序自带的图标：`assets/icon.ico`（`tools/make_icon.py` 生成的那份 RA 字母标）。
    static EMBEDDED_ICON: &[u8] = include_bytes!("../assets/icon.ico");

    pub(super) fn builtin_rgba(size: u32) -> Vec<u8> {
        // 先试自带图标；解不出来再退回原来那个「三道横线」的兜底画法
        if let Ok(image) = image::load_from_memory(EMBEDDED_ICON) {
            let rgba = image.to_rgba8();
            let scaled = image::imageops::resize(
                &rgba,
                size,
                size,
                image::imageops::FilterType::Lanczos3,
            );
            return scaled.into_raw();
        }
        bars_rgba(size)
    }

    /// 兜底图标：深色圆角方块 + 三道横线。
    pub(super) fn bars_rgba(size: u32) -> Vec<u8> {
        let side = size as f32;
        let radius = side * 0.22;
        // (y 起点, x 起点, x 终点) —— 都是相对边长的比例
        let bars = [(0.30_f32, 0.26_f32, 0.74_f32), (0.45, 0.26, 0.62), (0.60, 0.26, 0.74)];
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        for y in 0..size {
            for x in 0..size {
                let fx = x as f32 + 0.5;
                let fy = y as f32 + 0.5;
                if !inside_rounded(fx, fy, side, radius) {
                    continue;
                }
                let mut color = [30u8, 34, 45, 255];
                for (top, left, right) in bars {
                    let bar_top = top * side;
                    let bar_bottom = (top + 0.085) * side;
                    if fy >= bar_top && fy <= bar_bottom && fx >= left * side && fx <= right * side
                    {
                        color = [246, 248, 252, 255];
                    }
                }
                let at = ((y * size + x) * 4) as usize;
                pixels[at..at + 4].copy_from_slice(&color);
            }
        }
        pixels
    }

    /// 点 (x, y) 在不在圆角矩形里。
    fn inside_rounded(x: f32, y: f32, side: f32, radius: f32) -> bool {
        let (left, top, right, bottom) = (0.0, 0.0, side, side);
        if x < left || x > right || y < top || y > bottom {
            return false;
        }
        let cx = x.clamp(left + radius, right - radius);
        let cy = y.clamp(top + radius, bottom - radius);
        let (dx, dy) = (x - cx, y - cy);
        dx * dx + dy * dy <= radius * radius
    }

    /// RGBA 像素 → HICON（32bpp DIB + 全 0 掩码，alpha 全靠像素自己）。
    unsafe fn icon_from_rgba(size: u32, rgba: &[u8]) -> HICON {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            // 负数 = 自顶向下，省得自己翻转行
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let color = CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            return std::ptr::null_mut();
        }
        // DIB 要 BGRA
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, rgba.len());
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            dst[index * 4] = pixel[2];
            dst[index * 4 + 1] = pixel[1];
            dst[index * 4 + 2] = pixel[0];
            dst[index * 4 + 3] = pixel[3];
        }
        // 掩码全 0 = 全部不透明（透明度由颜色位图的 alpha 决定）
        let stride = ((size as usize + 31) / 32) * 4;
        let mask_bits = vec![0u8; stride * size as usize];
        let mask = CreateBitmap(
            size as i32,
            size as i32,
            1,
            1,
            mask_bits.as_ptr() as *const c_void,
        );
        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&icon_info);
        DeleteObject(color as HGDIOBJ);
        DeleteObject(mask as HGDIOBJ);
        icon
    }
}

// ---------------------------------------------------------------------------
// 其它平台：什么都不做（托盘目前只有 Windows 实现）
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
mod imp {
    pub fn spawn(_sender: async_channel::Sender<super::TrayEvent>) {}

    pub fn apply(_config: super::TrayConfig) {}
}

#[cfg(all(test, windows))]
mod tests {
    use crate::tray::imp;
    /// 内置图标必须真的来自 `assets/icon.ico`（解不出来时会退回「三道横线」的兜底画法，
    /// 那种情况在界面上根本看不出来是 bug，只能靠这个测试卡住）。
    #[test]
    fn the_builtin_icon_comes_from_the_bundled_ico() {
        let embedded = imp::builtin_rgba(32);
        let fallback = imp::bars_rgba(32);
        assert_ne!(
            embedded, fallback,
            "内置图标还是兜底的「三道横线」：assets/icon.ico 没解出来"
        );
        // 有效图标应该有不同程度的透明度和不止一种颜色（纯色说明画错了）
        let colors: std::collections::HashSet<[u8; 4]> = embedded
            .chunks_exact(4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect();
        assert!(colors.len() > 8, "内置图标只有 {} 种颜色，不像真图标", colors.len());
    }
}
