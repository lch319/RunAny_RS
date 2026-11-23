//! 一点点 Win32 胶水（仅 Windows 编译）。
//!
//! GPUI 0.2 没有暴露窗口的裸句柄，也不支持"移动窗口"和"圆角"，所以这里自己补：
//! 1. `zed_window_handles()` 枚举本进程的 GPUI 窗口（类名 `Zed::Window`），
//!    在创建窗口前后各取一次做差集，就拿到了新窗口的 HWND；
//! 2. `set_rounded_corners()` 让 DWM 按系统圆角裁窗口（Win11 起有效，
//!    这是唯一能拿到**真正抗锯齿透明圆角**的办法）；
//! 3. `move_window()` 用 SetWindowPos 挪窗口位置——`window.resize()` 只能改尺寸，
//!    不能改位置，而行对齐要求子菜单跟着父项换行。

/// 把逻辑像素坐标换成物理像素（SetWindowPos 用物理像素）。
pub fn px_to_physical(value: f32, scale: f32) -> i32 {
    (value * scale).round() as i32
}

/// 自己注入的按键都打上这个记号（`dwExtraInfo`）。
///
/// 热字符串的键盘钩子会看到**所有**按键，包括我们为了「删掉触发词 / 打出替换文本」
/// 注入的那些 —— 不打个记号，注入的字符会被当成用户输入记进缓冲区，
/// 于是替换文本自己又去匹配一遍短语（轻则提示乱跳，重则无限替换）。
const INJECT_MAGIC: usize = 0x5255_4E41; // "RUNA"

/// 键盘钩子里一条按键的原始信息（已经剥掉了 Win32 的结构体细节）。
#[derive(Clone, Copy, Debug)]
pub struct RawKey {
    /// 虚拟键码（`VK_BACK` 之类）
    pub vk: u16,
    /// 是不是按下（抬起的那些热字符串不关心）
    pub down: bool,
    /// Shift 压着没有（数字键上面那排符号要它才认得对）
    pub shift: bool,
}

/// 鼠标钩子里一条事件的原始信息（只看「按下」和滚轮，移动不往上报）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawMouse {
    /// 按下的是哪个键（`None` = 这条是滚轮）
    pub button: Option<MouseKind>,
    /// 滚轮方向：正 = 往上滚，负 = 往下滚
    pub wheel: i32,
}

/// 鼠标上能被当成「主键」按的那几个位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Left,
    Right,
    Middle,
    /// 侧键（靠近手掌那两个）
    X1,
    X2,
}

/// 剪贴板的一份快照：每种格式的原始字节。
///
/// 弹出菜单前要「注入一次 Ctrl+C」把选中内容拿出来，而 Ctrl+C 会覆盖用户的剪贴板；
/// 抓完必须按这份快照原样放回去，否则用户复制的东西就没了。
///
/// 另外它还兼任「再复制一次」的原料：点菜单第一项时把抓取那一刻的快照写回剪贴板，
/// 文件 / 图片 / 富文本就都是原样回去的，不用自己拼格式。
#[derive(Default)]
pub struct ClipboardBackup {
    /// 能读成字节的格式（内存块）；还原剪贴板就靠这一份
    entries: Vec<(u32, Vec<u8>)>,
    /// 剪贴板里出现过的**全部**格式号。
    /// 位图句柄那几种（CF_BITMAP 之类）读不了字节，但「有没有」本身有用 —— 判定类型要用。
    formats: Vec<u32>,
}

impl ClipboardBackup {
    /// 剪贴板里有这个格式吗（读不出字节的也算）。
    pub fn has_format(&self, format: u32) -> bool {
        format != 0 && self.formats.contains(&format)
    }

    /// 这个格式的原始字节（只有内存块格式才有）。
    pub fn bytes(&self, format: u32) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(id, _)| *id == format)
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// 手工拼一份快照（测试 `selection::detect` / 诊断用）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_raw(entries: Vec<(u32, Vec<u8>)>, formats: Vec<u32>) -> Self {
        Self { entries, formats }
    }
}

impl std::fmt::Debug for ClipboardBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 别把几 MB 的图片字节打到日志里
        write!(f, "ClipboardBackup({:?})", self.formats)
    }
}

// ---- 剪贴板格式号（winuser.h）----
// 放在模块级是因为 `selection::detect()` 要拿它们判断「这是什么类型」。

/// ANSI 文本
pub const CF_TEXT: u32 = 1;
/// 位图句柄（读不了字节，只能当「有没有图片」的旁证）
pub const CF_BITMAP: u32 = 2;
/// 设备无关位图（大多数程序复制图片时给这个）
pub const CF_DIB: u32 = 8;
/// 宽字符文本
pub const CF_UNICODETEXT: u32 = 13;
/// 文件列表（资源管理器里复制文件）
pub const CF_HDROP: u32 = 15;
/// 带 alpha 的 DIB
pub const CF_DIBV5: u32 = 17;

/// 查一个自定义格式号（`"PNG"` 这种没有固定编号，编号由系统发）。
/// 同一个名字在同一台机器上永远拿到同一个号，所以可以当场注册当场用。
pub fn registered_format(name: &str) -> u32 {
    #[cfg(windows)]
    {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW(wide.as_ptr()) }
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        0
    }
}

/// `CF_UNICODETEXT` 的字节 → 字符串（读到第一个 `\0` 为止）。
pub fn parse_wide_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// `CF_TEXT` 的字节 → 字符串（ANSI；按 UTF-8 解，解不了就 lossy）。
pub fn parse_ansi_text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

/// 本地时间（占位符 `{date}` / `{time}` 用）。
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

/// 解析 `CF_HDROP`：`DROPFILES` 头 + 一串以 NUL 结尾的路径（UTF-16 或 ANSI）。
///
/// 结构长这样（20 字节头，`pFiles` 是路径区的偏移）：
/// `pFiles(DWORD) pt(POINT) fNC(BOOL) fWide(BOOL)`。
/// 不用 `DragQueryFileW` 是为了少开一个 Shell 的 feature —— 这个格式简单到可以直接读。
pub fn parse_dropfiles(bytes: &[u8]) -> Vec<String> {
    if bytes.len() < 20 {
        return Vec::new();
    }
    let offset = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if offset < 20 || offset >= bytes.len() {
        return Vec::new();
    }
    let wide = i32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) != 0;
    let body = &bytes[offset..];
    let mut out: Vec<String> = Vec::new();
    if wide {
        let mut units: Vec<u16> = Vec::new();
        for chunk in body.chunks_exact(2) {
            let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
            if unit == 0 {
                if units.is_empty() {
                    break; // 两个 NUL = 结束
                }
                out.push(String::from_utf16_lossy(&units));
                units.clear();
            } else {
                units.push(unit);
            }
        }
    } else {
        let mut current: Vec<u8> = Vec::new();
        for byte in body {
            if *byte == 0 {
                if current.is_empty() {
                    break;
                }
                out.push(String::from_utf8_lossy(&current).to_string());
                current.clear();
            } else {
                current.push(*byte);
            }
        }
    }
    out
}

/// 从 `CF_DIB` 的 `BITMAPINFOHEADER` 里读宽高（只用来在菜单第一项上写个「1024×768」）。
pub fn parse_dib_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 12 {
        return None;
    }
    let header = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if header == 12 {
        // BITMAPCOREHEADER：宽高是 u16
        let w = u16::from_le_bytes([bytes[4], bytes[5]]) as u32;
        let h = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        return (w > 0 && h > 0).then_some((w, h));
    }
    let w = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let h = i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    // 高度为负 = 自上而下存的位图，取绝对值
    let (w, h) = (w.unsigned_abs(), h.unsigned_abs());
    (w > 0 && h > 0).then_some((w, h))
}

/// 从 PNG 的 `IHDR` 块里读宽高（浏览器「复制图片」常常只给 PNG，不给 DIB）。
pub fn parse_png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (w > 0 && h > 0).then_some((w, h))
}



#[cfg(windows)]
mod imp {
    #[link(name = "user32")]
    extern "system" {
        #[link_name = "GetDpiForWindow"]
        fn dpi_for_window(hwnd: HWND) -> u32;
    }


    #[link(name = "winhttp")]
    extern "system" {
        fn WinHttpOpen(
            agent: *const u16,
            access_type: u32,
            proxy: *const u16,
            bypass: *const u16,
            flags: u32,
        ) -> *mut std::ffi::c_void;
        fn WinHttpConnect(session: *mut std::ffi::c_void, server: *const u16, port: u16, reserved: u32)
            -> *mut std::ffi::c_void;
        fn WinHttpOpenRequest(
            connect: *mut std::ffi::c_void,
            verb: *const u16,
            object: *const u16,
            version: *const u16,
            referrer: *const u16,
            accept_types: *const *const u16,
            flags: u32,
        ) -> *mut std::ffi::c_void;
        fn WinHttpSendRequest(
            request: *mut std::ffi::c_void,
            headers: *const u16,
            headers_len: u32,
            optional: *mut std::ffi::c_void,
            optional_len: u32,
            total_len: u32,
            context: usize,
        ) -> i32;
        fn WinHttpReceiveResponse(request: *mut std::ffi::c_void, reserved: *mut std::ffi::c_void) -> i32;
        fn WinHttpQueryHeaders(
            request: *mut std::ffi::c_void,
            info_level: u32,
            name: *const u16,
            buffer: *mut std::ffi::c_void,
            buffer_len: *mut u32,
            index: *mut u32,
        ) -> i32;
        fn WinHttpReadData(
            request: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            to_read: u32,
            read: *mut u32,
        ) -> i32;
        fn WinHttpSetTimeouts(
            session: *mut std::ffi::c_void,
            resolve: i32,
            connect: i32,
            send: i32,
            receive: i32,
        ) -> i32;
        fn WinHttpCloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }

    use std::ffi::c_void;

    use super::{MouseKind, RawKey, RawMouse, INJECT_MAGIC};

    use windows_sys::Win32::Foundation::{
        BOOL, CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, SYSTEMTIME,
    };
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    use windows_sys::Win32::Graphics::Gdi::{
        ClientToScreen, CreateCompatibleDC, CreateRoundRectRgn, DeleteDC, DeleteObject, GetDIBits,
        GetMonitorInfoW, MonitorFromPoint, SetWindowRgn, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
        GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DestroyIcon, DispatchMessageW, EnumWindows, GetAncestor, GetClassNameW,
        GetClientRect, GetCursorPos, GetForegroundWindow, GetIconInfo, GetMessageW, GetTopWindow,
        GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, PostMessageW,
        SetForegroundWindow, SW_MINIMIZE, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWNORMAL,
        WM_CLOSE,
        SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, ShowWindow,
        ShowWindowAsync, TranslateMessage, UnhookWindowsHookEx, WindowFromPoint, GA_ROOT,
        GW_HWNDNEXT, GW_OWNER,
        GWL_EXSTYLE, HICON, HWND_TOP, HWND_TOPMOST, ICONINFO, KBDLLHOOKSTRUCT, LWA_ALPHA, MSG, MSLLHOOKSTRUCT,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_RESTORE,
        SW_SHOWNOACTIVATE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
        WS_EX_LAYERED,
    };
    use windows_sys::Win32::UI::Shell::{
        ExtractIconExW, ShellExecuteW, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
    };

    /// DWM_WINDOW_CORNER_PREFERENCE
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    /// DWMWCP_ROUND
    const DWMWCP_ROUND: u32 = 2;

    const CLASS_NAME: &str = "Zed::Window";

    /// 系统版本是不是 Windows 11（build ≥ 22000）。
    /// 用 `ntdll!RtlGetVersion`：`GetVersionEx` 会被兼容性 manifest 骗成 6.2。
    fn is_windows_11() -> bool {
        use std::sync::OnceLock;
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
        use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

        static CACHE: OnceLock<bool> = OnceLock::new();
        *CACHE.get_or_init(|| {
            type RtlGetVersion = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;
            unsafe {
                let module = GetModuleHandleA(c"ntdll.dll".as_ptr() as *const u8);
                if module.is_null() {
                    return false;
                }
                let Some(addr) = GetProcAddress(module, c"RtlGetVersion".as_ptr() as *const u8)
                else {
                    return false;
                };
                let rtl_get_version: RtlGetVersion = std::mem::transmute(addr);
                let mut info: OSVERSIONINFOW = std::mem::zeroed();
                info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
                if rtl_get_version(&mut info) != 0 {
                    return false;
                }
                info.dwBuildNumber >= 22000
            }
        })
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam as *mut Found);
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != out.pid {
            return 1; // continue
        }
        let mut buf = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        if len > 0 {
            let name = String::from_utf16_lossy(&buf[..len as usize]);
            if name == CLASS_NAME {
                out.handles.push(hwnd as isize);
            }
        }
        1
    }

    struct Found {
        pid: u32,
        handles: Vec<isize>,
    }

    /// 当前进程里所有 GPUI 窗口的 HWND。
    pub fn zed_window_handles() -> Vec<isize> {
        let mut found = Found {
            pid: std::process::id(),
            handles: Vec::new(),
        };
        unsafe {
            EnumWindows(Some(enum_proc), &mut found as *mut Found as LPARAM);
        }
        found.handles
    }

    /// 枚举系统里装了的全部字体族（`EnumFontFamiliesExW`，按 `DEFAULT_CHARSET` 走）。
    ///
    /// 字体枚举是 GDI 的事：拿屏幕 DC，把所有字体族过一遍，回调里收 `lfFaceName`。
    /// 以 `@` 开头的名字是「竖排」变体，过滤掉免得列表里出现两遍。
    pub fn font_families() -> Vec<String> {
        use windows_sys::Win32::Graphics::Gdi::{
            EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, LOGFONTW, TEXTMETRICW,
        };

        unsafe extern "system" fn collect(
            logfont: *const LOGFONTW,
            _metric: *const TEXTMETRICW,
            _font_type: u32,
            lparam: LPARAM,
        ) -> i32 {
            let out = &mut *(lparam as *mut Vec<String>);
            let face = &(*logfont).lfFaceName;
            let len = face
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(face.len());
            if len > 0 {
                let name = String::from_utf16_lossy(&face[..len]);
                if !name.starts_with('@') {
                    out.push(name);
                }
            }
            1 // 继续枚举
        }

        let mut names: Vec<String> = Vec::new();
        unsafe {
            let hdc = GetDC(std::ptr::null_mut());
            if hdc.is_null() {
                return names;
            }
            let mut logfont: LOGFONTW = std::mem::zeroed();
            logfont.lfCharSet = DEFAULT_CHARSET;
            EnumFontFamiliesExW(
                hdc,
                &logfont,
                Some(collect),
                &mut names as *mut Vec<String> as LPARAM,
                0,
            );
            ReleaseDC(std::ptr::null_mut(), hdc);
        }
        names
    }

    /// 把窗口抬到**置顶**层级的最上面（不改尺寸、不抢焦点）。
    ///
    /// 两件事都靠它解决：
    /// 1. 级联菜单的每一列都是独立窗口，如果只按顺序 ShowWindow，后显示的那一列
    ///    不一定压在前一列上面 —— 屏幕边缘把几列挤到同一个 x 时就会互相盖住。
    ///    摆完窗口后按层级从小到大各抬一次，第 N 列就一定在第 N-1 列上面。
    /// 2. 用 `HWND_TOPMOST` 而不是 `HWND_TOP`：`HWND_TOP` 只在**普通**窗口里
    ///    换位置，用户正在用的那个程序（尤其是最大化窗口）始终压在菜单上 ——
    ///    这就是「子菜单偶尔被其他窗口盖住 / 根本看不见」的原因。
    ///    菜单本身就是瞬时 UI，系统菜单也是置顶的，跟着来就对了。
    pub fn raise_window(hwnd: isize) {
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
    }

    /// 把窗口抬到**普通窗口**里的最前面，但**不激活**它。
    ///
    /// 用途：从设置窗口点「打开菜单试试」时，菜单窗口一抢前台，设置窗口就被
    /// Windows 顺手沉到别的程序下面去了（它会重新排 z 序）。弹出菜单本身是
    /// TOPMOST，仍然压在上面，所以这里只要把设置窗口抬回普通窗口的最前就行。
    pub fn raise_above(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    /// 抬到最前面**并**把前台交给它（关菜单后把焦点还给用户原来的窗口）。
    pub fn bring_to_front(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            SetForegroundWindow(hwnd as HWND);
        }
    }

    /// 只让 DWM 裁系统圆角（Win11 起有效；Win10 上什么都不做，窗口保持直角）。
    ///
    /// 设置窗口 / 编辑窗口用这个 —— Bolume 的窗口也是这么做的。**不设窗口区域**：
    /// `SetWindowRgn` 会把窗口形状改掉，`DefWindowProc` 的边框命中判定跟着变形，
    /// 拖边框改大小就时灵时不灵。宁可 Win10 上没圆角，也要保住原生缩放。
    pub fn native_round_corners(hwnd: isize) {
        if !is_windows_11() {
            return;
        }
        let value = DWMWCP_ROUND;
        unsafe {
            DwmSetWindowAttribute(
                hwnd as HWND,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &value as *const u32 as *const c_void,
                std::mem::size_of::<u32>() as u32,
            )
        };
    }

    /// 圆角：Win11 交给 DWM（平滑抗锯齿）；Win10 没有这个能力，
    /// 退回「窗口区域裁剪」（边缘是硬裁剪，但确实是圆角，且不会露出直角像素）。
    /// `scale` 用于把逻辑半径换成物理像素。
    pub fn set_rounded_corners(hwnd: isize, scale: f32) {
        set_rounded_corners_with(hwnd, scale, crate::layout::radius());
    }

    /// 同上，但半径由调用方给（菜单列的圆角跟设置项走）。
    pub fn set_rounded_corners_with(hwnd: isize, scale: f32, radius: f32) {
        if scale <= 0.0 {
            return;
        }
        let handle = hwnd as HWND;

        // Win11（build ≥ 22000）交给 DWM，能拿到抗锯齿的平滑圆角。
        // 注意不能靠 DwmSetWindowAttribute 的返回值判断：Win10 会返回 S_OK 但什么也不做。
        if is_windows_11() {
            native_round_corners(hwnd);
            return;
        }

        // 区域坐标是「窗口坐标」，要加上客户区相对窗口的偏移
        unsafe {
            let mut client = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetClientRect(handle, &mut client);
            let mut window = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetWindowRect(handle, &mut window);
            let mut origin = POINT { x: 0, y: 0 };
            ClientToScreen(handle, &mut origin);
            let inset_x = origin.x - window.left;
            let inset_y = origin.y - window.top;

            let radius = (radius * scale).round().max(0.0) as i32;
            let rgn = CreateRoundRectRgn(
                inset_x,
                inset_y,
                inset_x + client.right + 1,
                inset_y + client.bottom + 1,
                radius * 2,
                radius * 2,
            );
            // SetWindowRgn 会接管这个 region，不需要（也不能）DeleteObject
            SetWindowRgn(handle, rgn, 1);
        }
    }

    /// 只改窗口尺寸（按**客户区**算），**绝不激活**。
    ///
    /// 为什么不用 GPUI 的 `window.resize()`：它内部是
    /// `SetWindowPos(hwnd, None, .., SWP_NOMOVE)` —— **漏了 `SWP_NOACTIVATE`**，
    /// 于是每改一次尺寸就把这个窗口抢成前台。悬停提示用它会顺手把菜单的前台抢走，
    /// 菜单的 watchdog 一看「前台不是菜单」就当场把菜单收了（实测：提示一冒出来
    /// 菜单就整片消失）。尺寸变化 GPUI 自己会从 `WM_SIZE` 里知道，不用它代劳。
    pub fn resize_window(hwnd: isize, width: f32, height: f32, scale: f32) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            let hwnd = hwnd as HWND;
            // 客户区 → 窗口矩形要加回那圈非客户区（阴影 + 圆角边框），厚度按当前窗口量
            let mut window = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let mut client = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetWindowRect(hwnd, &mut window);
            GetClientRect(hwnd, &mut client);
            let extra_w = (window.right - window.left) - (client.right - client.left);
            let extra_h = (window.bottom - window.top) - (client.bottom - client.top);
            // **向上取整**：`round` 在 175% 这类小数缩放下会少算零点几个逻辑像素，
            // 客户区就比内容矮一丢丢 —— 最后一行文字被裁掉一截（悬停提示在 175%
            // 下「显示不全」就是这么来的）。宁可多一个物理像素，也不能切字。
            let physical_width = (width * scale).ceil() as i32;
            let physical_height = (height * scale).ceil() as i32;
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                physical_width + extra_w,
                physical_height + extra_h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    /// 把客户区做到**至少** `width × height` 逻辑像素。
    ///
    /// 不靠「按 DPI 换算一次就完事」：175% 这种小数缩放下取整、非客户区的圆角
    /// 阴影、窗口刚建好时的 DPI 切换，每一样都可能让它差几个像素 —— 差的那几个
    /// 像素正好把最后一行文字切掉（悬停提示「文字被裁一半、卡片尺寸不对」）。
    ///
    /// 做法：设一次 → **读回真实客户区** → 不够就按差值再设。最多三轮，收敛就停。
    pub fn fit_client_size(hwnd: isize, width: f32, height: f32, scale: f32) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            let hwnd = hwnd as HWND;
            // 客户区 → 窗口矩形要加回那圈非客户区，厚度按当前窗口量
            let mut window = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            let mut client = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetWindowRect(hwnd, &mut window);
            GetClientRect(hwnd, &mut client);
            let extra_w = (window.right - window.left) - (client.right - client.left);
            let extra_h = (window.bottom - window.top) - (client.bottom - client.top);

            let mut scale = window_scale(hwnd as isize).unwrap_or(scale).max(0.5);
            for _ in 0..3 {
                let want_w = (width * scale).ceil() as i32;
                let want_h = (height * scale).ceil() as i32;
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    want_w + extra_w,
                    want_h + extra_h,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let (got_w, got_h) = client_size(hwnd as isize).unwrap_or((0, 0));
                if got_w >= want_w && got_h >= want_h {
                    return;
                }
                // 窗口的 DPI 是摆上去之后才定下来的：重读一次再补
                scale = window_scale(hwnd as isize).unwrap_or(scale).max(0.5);
            }
        }
    }

    /// 客户区尺寸（物理像素）。判断「窗口是不是真的变成了我们要的高度」用。
    ///
    /// `window.resize()` 只是**请求**，系统那一侧可能因为窗口刚显示、正被
    /// `SetWindowPos` 抢着摆位置等原因把它吞掉。只看自己记的「上次请求的高度」
    /// 就会以为已经改好了，于是菜单卡在旧高度上：内容超出 → 多出一条滚动条和
    /// 底部的「还有更多」箭头。所以要能读回真实值来对账。
    pub fn client_size(hwnd: isize) -> Option<(i32, i32)> {
        if hwnd == 0 {
            return None;
        }
        unsafe {
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetClientRect(hwnd as HWND, &mut rect) == 0 {
                return None;
            }
            Some((rect.right - rect.left, rect.bottom - rect.top))
        }
    }

    /// 窗口矩形（物理像素：左/上/右/下）。调试定位用。
    pub fn window_rect(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
        if hwnd == 0 {
            return None;
        }
        unsafe {
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetWindowRect(hwnd as HWND, &mut rect) == 0 {
                return None;
            }
            Some((rect.left, rect.top, rect.right, rect.bottom))
        }
    }

    /// 光标所在显示器的「工作区」相对整块屏幕的四边内缩（物理像素：左/上/右/下）。
    ///
    /// 任务栏占掉的那一条就在里边（实测本机底部 40px）。菜单如果按整块屏幕夹高度，
    /// 底部 8px 的位置正好藏在任务栏后面，三角箭头就看不见了。
    pub fn work_area_insets() -> Option<(f32, f32, f32, f32)> {
        unsafe {
            let mut cursor = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut cursor) == 0 {
                return None;
            }
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            if monitor.is_null() {
                return None;
            }
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return None;
            }
            let (monitor_rect, work) = (info.rcMonitor, info.rcWork);
            Some((
                (work.left - monitor_rect.left) as f32,
                (work.top - monitor_rect.top) as f32,
                (monitor_rect.right - work.right) as f32,
                (monitor_rect.bottom - work.bottom) as f32,
            ))
        }
    }

    /// 让 Windows 自己接管一次「拖动窗口 / 拖边框」。
    ///
    /// 做法就是老式窗口常干的那招：`ReleaseCapture` + 告诉系统「按在标题栏上了」，
    /// 之后移动、贴边吸附（Aero Snap）、边框缩放全由系统做，比自己在
    /// `SetWindowPos` 里手搓稳得多。
    ///
    /// ## 为什么是 `PostMessage` 而不是 `SendMessage`
    ///
    /// 这里以前是 `SendMessage(WM_NCLBUTTONDOWN)`。那个调用是**同步**的：
    /// 系统会在本次消息处理内部直接进「拖动模态循环」，而这时候我们正站在
    /// GPUI 的事件回调里 —— `App` 那个 `RefCell` 还借着。
    ///
    /// 模态循环会给本窗口送 `WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD`
    /// （`SendMessageTimeout` → wndproc → `run_foreground_task`），GPUI 借机
    /// 把我们的异步任务（菜单的焦点看门狗、悬停提示的延时器）跑一遍；
    /// 那些任务里 `cx.update(...)` 要再 `borrow_mut` 一次 —— 于是
    /// **`RefCell already borrowed` → panic → abort**（0xc0000409，WER 里是 BEX64）。
    /// 表现就是「菜单开着的时候拖一下设置窗口，程序直接没了」。
    ///
    /// 改成 `PostMessage` 后，模态循环由消息泵在**顶层**开始，那时候没有任何借用；
    /// 循环期间被顺带跑掉的异步任务也能正常拿到 `App`。
    /// `SC_MOUSEMOVE` 表示「跟着鼠标走」，不需要真的按住键。
    ///
    /// `area` 现在只用来区分「移动」和「拖边框缩放」（`HT_CAPTION` 之外都当缩放）。
    pub fn start_system_drag(hwnd: isize, area: i32) {
        const WM_SYSCOMMAND: u32 = 0x0112;
        const SC_MOVE: usize = 0xF010;
        const SC_SIZE: usize = 0xF000;
        const SC_MOUSEMOVE: usize = 0x0002;
        let command = if area == super::HT_CAPTION {
            SC_MOVE
        } else {
            SC_SIZE
        };
        unsafe {
            ReleaseCapture();
            PostMessageW(
                hwnd as HWND,
                WM_SYSCOMMAND,
                command | SC_MOUSEMOVE,
                0,
            );
        }
    }

    /// 还原（只清掉「最大化」状态；尺寸由 Windows 自己还回去）。
    ///
    /// 必须用 `ShowWindowAsync`：`ShowWindow` 会把 `WM_SIZE` **同步**送达，
    /// 而我们此刻正在窗口过程里处理点击，gpui 那边的窗口还被借用着，
    /// 尺寸变更回调会被静默丢掉 —— 结果就是窗口小了、界面还按最大化时的尺寸排版。
    /// 异步版本把消息排进队列，等 gpui 处理完手上这帧再回来重排。
    pub fn restore_window(hwnd: isize) {
        unsafe {
            ShowWindowAsync(hwnd as HWND, SW_RESTORE);
        }
    }

    pub fn is_maximized(hwnd: isize) -> bool {
        unsafe { IsZoomed(hwnd as HWND) != 0 }
    }

    /// 当前前台窗口（关菜单时要还焦点给它）。
    pub fn foreground_window() -> isize {
        unsafe { GetForegroundWindow() as isize }
    }

    /// 本机 ANSI 代码页（国内就是 GBK）解成 UTF-8。
    ///
    /// 只给「读别人的文本文件」用：导入 `RunAny.ini` 时那份文件可能是 UTF-8，
    /// 也可能是 ANSI，先按 UTF-8 试，失败了再走这里。解不出来返回 `None`。
    pub fn decode_ansi(bytes: &[u8]) -> Option<String> {
        use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_ACP};
        if bytes.is_empty() {
            return Some(String::new());
        }
        unsafe {
            let len = MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), bytes.len() as i32, std::ptr::null_mut(), 0);
            if len <= 0 {
                return None;
            }
            let mut wide = vec![0u16; len as usize];
            let written =
                MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), bytes.len() as i32, wide.as_mut_ptr(), len);
            if written <= 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&wide[..written as usize]))
        }
    }

    /// 最近一个「属于别人」的前台窗口（后台线程每 150ms 更新一次）。
    static RECENT_FOREGROUND: std::sync::atomic::AtomicIsize =
        std::sync::atomic::AtomicIsize::new(0);

    fn last_foreign_foreground() -> isize {
        RECENT_FOREGROUND.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 窗口标题（打日志用；拿不到就是空串）。
    /// 这个窗口是不是「桌面」（Progman / WorkerW / 任务栏 SHELLDLL_DefView 这类）。
    ///
    /// 桌面上不可能有选中内容，抓取整条链路（等修饰键 → 发 Ctrl+C → 等剪贴板）
    /// 纯浪费时间 —— 按快捷键时如果前台就是桌面，直接跳过，菜单立刻出来。
    pub fn is_desktop_window(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let mut buf = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd as HWND, buf.as_mut_ptr(), buf.len() as i32) };
        if len <= 0 {
            return false;
        }
        matches!(
            String::from_utf16_lossy(&buf[..len as usize]).as_str(),
            "Progman" | "WorkerW" | "SHELLDLL_DefView" | "SysListView32"
        )
    }

    #[allow(dead_code)]
    pub fn window_title(hwnd: isize) -> String {
        if hwnd == 0 {
            return String::new();
        }
        unsafe {
            let hwnd = hwnd as HWND;
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return String::new();
            }
            let mut buffer = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
            String::from_utf16_lossy(&buffer[..n.max(0) as usize])
        }
    }

    /// 抓取选中内容该对着谁发复制键。
    ///
    /// 平常就是前台窗口。但前台经常是**我们自己的**窗口（用户刚在设置页里
    /// 配好快捷键就按下试一下，或者菜单刚收起来焦点还没回去）——
    /// 这时候要用「用户刚才待着的那个窗口」。找它的顺序：
    ///
    /// 1. 后台每 150ms 记一次的「最近一个**属于别人**的前台窗口」（`watch_foreground`），
    ///    这个最准：用户点我们的窗口之前，焦点确实在那儿；
    /// 2. 记的那个已经关掉了 → 沿 z 序往下找第一个别人的顶层窗口。
    ///    这只是兜底：z 序里有悬浮球、桌宠、任务栏工具这些常驻窗口，
    ///    它们常常压在你真正要用的窗口上面。
    ///
    /// 返回 `0` 表示「真的没地方可抓」。
    // 现在没有调用方了（前台是自己人时改成「干脆不抓」，见 `selection::capture`），
    // 留着是因为「从下面那个窗口抓」这件事本身还是有用的，别的地方可能再捡起来。
    #[allow(dead_code)]
    pub fn capture_target() -> isize {
        let ours = std::process::id();
        let foreground = foreground_window();
        if foreign_window(foreground, ours) {
            return foreground;
        }
        let remembered = last_foreign_foreground();
        if remembered != 0 && foreign_window(remembered, ours) {
            return remembered;
        }
        // 兜底：从 z 序最上面开始往下走
        let mut hwnd = unsafe { GetTopWindow(std::ptr::null_mut()) };
        let mut guard = 0;
        while !hwnd.is_null() && guard < 400 {
            if foreign_window(hwnd as isize, ours) {
                return hwnd as isize;
            }
            hwnd = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
            guard += 1;
        }
        0
    }

    /// 后台记「最近一个属于别人的前台窗口」，给 [`capture_target`] 用。
    ///
    /// 起一个每 150ms 采一次的线程（一个 `GetForegroundWindow` + 一个
    /// `GetWindowThreadProcessId`，几乎不占 CPU）。比沿 z 序猜可靠得多。
    pub fn watch_foreground() {
        use std::sync::atomic::Ordering;
        std::thread::spawn(|| loop {
            let hwnd = foreground_window();
            if hwnd != 0 && window_process_id(hwnd) != std::process::id() {
                RECENT_FOREGROUND.store(hwnd, Ordering::Relaxed);
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        });
    }

    /// 这个窗口属于**别人**、并且值得往里发 Ctrl+C 吗。
    fn foreign_window(hwnd: isize, ours: u32) -> bool {
        if hwnd == 0 || window_process_id(hwnd) == ours {
            return false;
        }
        unsafe {
            let hwnd = hwnd as HWND;
            if IsWindowVisible(hwnd) == 0 {
                return false;
            }
            // 桌面 / 任务栏 / 输入法候选框之类：没有标题的顶层窗口一般都不是编辑器
            if GetWindowTextLengthW(hwnd) == 0 {
                return false;
            }
            // 只要顶层窗口（子窗口发复制键没意义）
            GetWindow(hwnd, GW_OWNER).is_null() || GetAncestor(hwnd, GA_ROOT) == hwnd
        }
    }

    /// 等焦点落到 `hwnd` 上（最多 `max_ms`），返回是否等到了。
    pub fn wait_foreground(hwnd: isize, max_ms: u64) -> bool {
        if hwnd == 0 {
            return false;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(max_ms);
        loop {
            if foreground_window() == hwnd {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
    }

    /// 在**另一个线程**上跑一个文件对话框，等它返回。
    ///
    /// 为什么非得换线程：`GetOpenFileNameW` 会自己开一个模态消息循环，而它会
    /// 顺手把我们自己窗口的 Windows 消息也抽出来派发 —— 可这会儿调用方正握着
    /// `&mut App`（它是在点击回调里被调用的），GPUI 再想借用一次 `App` 就是
    /// 「already borrowed」panic，表现就是**点「选文件」偶尔闪退**。
    ///
    /// 换到独立线程之后，主线程只是阻塞等待（界面短暂不重绘，和普通模态框一样），
    /// 对话框的消息循环跑在自己的线程里，两边互不干扰。
    fn in_dialog_thread(run: impl FnOnce() -> Option<String> + Send + 'static) -> Option<String> {
        std::thread::spawn(run).join().ok().flatten()
    }

    /// 弹出系统的「打开文件」对话框，选一张图当菜单图标。
    ///
    /// 用 `GetOpenFileNameW`（comdlg32）而不是自己拉一个文件浏览器：
    /// 它会自己开模态消息循环，选完才返回，省得写一整个对话框。
    /// 过滤器只留图片，避免用户选到别的东西。
    pub fn pick_image_file() -> Option<String> {
        in_dialog_thread(pick_image_file_dialog)
    }

    /// 真正开对话框的那一半（只在独立线程里跑）。
    fn pick_image_file_dialog() -> Option<String> {
        use windows_sys::Win32::UI::Controls::Dialogs::{
            GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
            OFN_PATHMUSTEXIST,
            OPENFILENAMEW,
        };

        // 文件对话框的过滤器是 Windows 原生控件，在系统对话框里，只能按语言各写一份
        let filter: Vec<u16> = if crate::i18n::is_english() {
            "Images (*.svg;*.ico;*.png;*.bmp;*.jpg;*.webp)\0*.svg;*.ico;*.png;*.bmp;*.jpg;*.jpeg;*.webp\0All files (*.*)\0*.*\0\0"
        } else {
            "图片 (*.svg;*.ico;*.png;*.bmp;*.jpg;*.webp)\0*.svg;*.ico;*.png;*.bmp;*.jpg;*.jpeg;*.webp\0所有文件 (*.*)\0*.*\0\0"
        }
        .encode_utf16()
        .collect();
        let mut buffer = [0u16; 1024];
        let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY
            // 不勾这个：对话框会把进程的当前目录改到用户选的那个文件夹去，
            // 之后所有相对路径（图标、脚本）就全跟着跑偏
            | OFN_NOCHANGEDIR;
        let opened = unsafe { GetOpenFileNameW(&mut dialog) };
        if opened == 0 {
            return None; // 用户取消
        }
        let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
        (len > 0).then(|| String::from_utf16_lossy(&buffer[..len]))
    }

    /// 弹「打开文件」对话框，选一份 RunAny 原版的菜单配置（`RunAny.ini`）。
    pub fn pick_ini_file() -> Option<String> {
        in_dialog_thread(pick_ini_file_dialog)
    }

    /// 真正开对话框的那一半（只在独立线程里跑）。
    fn pick_ini_file_dialog() -> Option<String> {
        use windows_sys::Win32::UI::Controls::Dialogs::{
            GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
            OFN_PATHMUSTEXIST,
            OPENFILENAMEW,
        };

        let filter: Vec<u16> = if crate::i18n::is_english() {
            "RunAny menu (*.ini)\0*.ini\0All files (*.*)\0*.*\0\0"
        } else {
            "RunAny 菜单配置 (*.ini)\0*.ini\0所有文件 (*.*)\0*.*\0\0"
        }
        .encode_utf16()
        .collect();
        let mut buffer = [0u16; 1024];
        let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY
            // 不勾这个：对话框会把进程的当前目录改到用户选的那个文件夹去，
            // 之后所有相对路径（图标、脚本）就全跟着跑偏
            | OFN_NOCHANGEDIR;
        let opened = unsafe { GetOpenFileNameW(&mut dialog) };
        if opened == 0 {
            return None;
        }
        let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
        (len > 0).then(|| String::from_utf16_lossy(&buffer[..len]))
    }

    /// 弹「打开文件」对话框，选一个 AutoHotkey 脚本（`.ahk`）。
    pub fn pick_script_file() -> Option<String> {
        in_dialog_thread(pick_script_file_dialog)
    }

    /// 真正开对话框的那一半（只在独立线程里跑）。
    fn pick_script_file_dialog() -> Option<String> {
        use windows_sys::Win32::UI::Controls::Dialogs::{
            GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
            OFN_PATHMUSTEXIST,
            OPENFILENAMEW,
        };

        let filter: Vec<u16> = if crate::i18n::is_english() {
            "AutoHotkey script (*.ahk)\0*.ahk\0All files (*.*)\0*.*\0\0"
        } else {
            "AutoHotkey 脚本 (*.ahk)\0*.ahk\0所有文件 (*.*)\0*.*\0\0"
        }
        .encode_utf16()
        .collect();
        let mut buffer = [0u16; 1024];
        let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY
            // 不勾这个：对话框会把进程的当前目录改到用户选的那个文件夹去，
            // 之后所有相对路径（图标、脚本）就全跟着跑偏
            | OFN_NOCHANGEDIR;
        let opened = unsafe { GetOpenFileNameW(&mut dialog) };
        if opened == 0 {
            return None;
        }
        let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
        (len > 0).then(|| String::from_utf16_lossy(&buffer[..len]))
    }

    /// 弹「打开文件」对话框，选一个可执行文件（程序搜索页手动指定路径用）。
    pub fn pick_program_file() -> Option<String> {
        in_dialog_thread(pick_program_file_dialog)
    }

    /// 真正开对话框的那一半（只在独立线程里跑）。
    fn pick_program_file_dialog() -> Option<String> {
        use windows_sys::Win32::UI::Controls::Dialogs::{
            GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
            OFN_PATHMUSTEXIST,
            OPENFILENAMEW,
        };

        let filter: Vec<u16> = if crate::i18n::is_english() {
            "Programs (*.exe;*.bat;*.cmd;*.com)\0*.exe;*.bat;*.cmd;*.com\0All files (*.*)\0*.*\0\0"
        } else {
            "程序 (*.exe;*.bat;*.cmd;*.com)\0*.exe;*.bat;*.cmd;*.com\0所有文件 (*.*)\0*.*\0\0"
        }
        .encode_utf16()
        .collect();
        let mut buffer = [0u16; 1024];
        let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY
            // 不勾这个：对话框会把进程的当前目录改到用户选的那个文件夹去，
            // 之后所有相对路径（图标、脚本）就全跟着跑偏
            | OFN_NOCHANGEDIR;
        let opened = unsafe { GetOpenFileNameW(&mut dialog) };
        if opened == 0 {
            return None;
        }
        let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
        (len > 0).then(|| String::from_utf16_lossy(&buffer[..len]))
    }

    /// 把某个窗口拉到前台（切回用户原来那个应用）。
    pub fn set_foreground(hwnd: isize) {
        if hwnd != 0 {
            unsafe {
                SetForegroundWindow(hwnd as HWND);
            }
        }
    }

    /// 光标现在在屏幕上的物理像素坐标。
    pub fn cursor_position() -> (i32, i32) {
        unsafe {
            let mut point = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut point) == 0 {
                return (0, 0);
            }
            (point.x, point.y)
        }
    }

    /// 鼠标左键现在是不是按着（`GetAsyncKeyState` 的最高位）。
    ///
    /// 取色只能用轮询：没有全局鼠标钩子，也不值得为这个装一个。
    pub fn left_button_down() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        unsafe { (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0 }
    }

    /// 读屏幕上某个点的颜色（`0xRRGGBB`）。
    ///
    /// `GetDC(NULL)` 拿的是整个屏幕的 DC，`GetPixel` 返回的是**合成之后**的像素，
    /// 所以拿到的是用户眼睛看到的那个颜色。受保护内容 / 独占全屏下会失败，返回 `None`。
    pub fn screen_color_at(x: i32, y: i32) -> Option<u32> {
        use windows_sys::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
        unsafe {
            let hdc = GetDC(std::ptr::null_mut());
            if hdc.is_null() {
                return None;
            }
            let color = GetPixel(hdc, x, y);
            ReleaseDC(std::ptr::null_mut(), hdc);
            // CLR_INVALID = 0xFFFFFFFF
            if color == u32::MAX {
                return None;
            }
            // COLORREF 是 0x00BBGGRR，转成 0xRRGGBB
            Some(((color & 0xff) << 16) | (color & 0xff00) | ((color >> 16) & 0xff))
        }
    }

    /// 光标下面的颜色（取色按钮用）。
    pub fn screen_color_at_cursor() -> Option<u32> {
        let (x, y) = cursor_position();
        screen_color_at(x, y)
    }

    /// 藏起来（池里停放的窗口用）。
    pub fn hide_window(hwnd: isize) {
        unsafe {
            ShowWindow(hwnd as HWND, SW_HIDE);
        }
    }

    /// 把「暂时用不到的物理内存」还给系统（工作集裁剪）。
    ///
    /// 平台初始化时就已经建好了 D3D 设备，驱动页和交换链缓冲加起来四十多 MB
    /// 常驻着。菜单关掉、窗口池也回收之后，这些页全是「冷的」—— 收回去以后
    /// 任务管理器里的数字从七十多兆掉到几兆，下次弹出再从备用列表拉回来
    /// （实测只多 50ms 左右，比一直占着七十兆划算）。
    ///
    /// **只在彻底空闲时调**：启动阶段或者菜单开着的时候裁，会把正在用的
    /// 驱动页也赶出去，下一次弹出要多等好几百毫秒（实测 700ms）。
    pub fn trim_working_set() {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
        // -1 / -1 = EmptyWorkingSet 的等价写法（文档里给的标准用法）
        unsafe {
            SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        }
    }

    /// 显示出来但不抢焦点。
    pub fn show_window(hwnd: isize) {
        unsafe {
            ShowWindow(hwnd as HWND, SW_SHOWNOACTIVATE);
        }
    }

    /// 整个窗口的不透明度（0.0~1.0）。
    ///
    /// 做法是给它加 `WS_EX_LAYERED` + `SetLayeredWindowAttributes`：
    /// GPUI 照常往自己的 swapchain 里画，DWM 合成的时候按这个 alpha 一起淡出，
    /// 所以文字、图标、阴影是一起变淡的（不像改背景色那样只有底色变）。
    ///
    /// 1.0 时把 `WS_EX_LAYERED` 摘掉 —— 分层窗口在某些显卡驱动上会绕开优化路径，
    /// 没必要的开销就别留着。
    pub fn set_window_opacity(hwnd: isize, opacity: f32) {
        if hwnd == 0 {
            return;
        }
        let opacity = opacity.clamp(0.0, 1.0);
        unsafe {
            let hwnd = hwnd as HWND;
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            if opacity >= 0.999 {
                if style & WS_EX_LAYERED != 0 {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style & !WS_EX_LAYERED) as isize);
                }
                return;
            }
            if style & WS_EX_LAYERED == 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style | WS_EX_LAYERED) as isize);
            }
            let alpha = (opacity * 255.0).round().clamp(1.0, 255.0) as u8;
            SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
        }
    }

    // -----------------------------------------------------------------------
    // 从程序里抠图标
    // -----------------------------------------------------------------------

    /// 把 `.exe` / `.lnk` / `.ico` 的图标读成 RGBA 像素（`size` 只是给 shell 的提示）。
    ///
    /// 两步：
    /// 1. `ExtractIconExW` 拿 `HICON`（读不到就退到 `SHGetFileInfoW`，它对快捷方式、
    ///    文件夹、甚至没有内嵌图标的 exe 都有效）；
    /// 2. `GetIconInfo` 拆成「颜色位图 + 掩码位图」，`GetDIBits` 读成 32 位像素，
    ///    再用掩码补 alpha（老式 24 位图标没有 alpha 通道，全靠掩码说哪里透明）。
    pub fn icon_pixels(path: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut large: HICON = std::ptr::null_mut();
        let mut small: HICON = std::ptr::null_mut();
        let extracted =
            unsafe { ExtractIconExW(wide.as_ptr(), 0, &mut large, &mut small, 1) } as usize;
        let mut icon = if !large.is_null() { large } else { small };
        // 多出来的那个句柄要还回去
        if !large.is_null() && !small.is_null() {
            unsafe { DestroyIcon(small) };
            icon = large;
        }
        if extracted == 0 || icon.is_null() {
            let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                SHGetFileInfoW(
                    wide.as_ptr(),
                    0,
                    &mut info,
                    std::mem::size_of::<SHFILEINFOW>() as u32,
                    SHGFI_ICON | SHGFI_LARGEICON,
                )
            };
            if ok == 0 || info.hIcon.is_null() {
                return None;
            }
            icon = info.hIcon;
        }
        let pixels = icon_to_rgba(icon, size);
        unsafe { DestroyIcon(icon) };
        pixels
    }

    fn icon_to_rgba(icon: HICON, size: u32) -> Option<(u32, u32, Vec<u8>)> {
        unsafe {
            let mut info: ICONINFO = std::mem::zeroed();
            if GetIconInfo(icon, &mut info) == 0 {
                return None;
            }
            let mut result = None;
            if !info.hbmColor.is_null() {
                result = bitmap_to_rgba(info.hbmColor, size);
            }
            if result.is_none() && !info.hbmMask.is_null() {
                // 单色图标：掩码本身当灰度图用
                result = mask_to_rgba(info.hbmMask, size);
            }
            if !info.hbmColor.is_null() {
                DeleteObject(info.hbmColor as HGDIOBJ);
            }
            if !info.hbmMask.is_null() {
                DeleteObject(info.hbmMask as HGDIOBJ);
            }
            result
        }
    }

    /// 位图尺寸（拿不到就用调用方给的 `fallback`）。
    unsafe fn bitmap_size(hbm: HBITMAP, fallback: u32) -> (u32, u32) {
        use windows_sys::Win32::Graphics::Gdi::GetObjectW;
        let mut bitmap: BITMAP = std::mem::zeroed();
        let ok = GetObjectW(
            hbm as HGDIOBJ,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bitmap as *mut BITMAP as *mut c_void,
        );
        if ok == 0 || bitmap.bmWidth <= 0 || bitmap.bmHeight <= 0 {
            (fallback, fallback)
        } else {
            (bitmap.bmWidth as u32, bitmap.bmHeight as u32)
        }
    }

    /// 32 位位图 → RGBA；alpha 全 0 时用掩码补。
    unsafe fn bitmap_to_rgba(hbm: HBITMAP, fallback: u32) -> Option<(u32, u32, Vec<u8>)> {
        let (width, height) = bitmap_size(hbm, fallback);
        let mut pixels = read_dib(hbm, width, height, 32)?;
        // `GetDIBits` 给的是 BGRA：换成 RGBA，顺便看 alpha 有没有内容
        let mut has_alpha = false;
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            if pixel[3] != 0 {
                has_alpha = true;
            }
        }
        if !has_alpha {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }
        Some((width, height, pixels))
    }

    /// 1 位掩码位图 → 黑/白 RGBA（老式图标兜底）。
    unsafe fn mask_to_rgba(hbm: HBITMAP, fallback: u32) -> Option<(u32, u32, Vec<u8>)> {
        let (width, height) = bitmap_size(hbm, fallback);
        let bits = read_dib(hbm, width, height, 1)?;
        let stride = ((width as usize + 31) / 32) * 4;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let byte = bits.get(y * stride + x / 8).copied().unwrap_or(0);
                let set = (byte >> (7 - (x % 8))) & 1 == 1;
                let value = if set { 0 } else { 255 };
                let at = (y * width as usize + x) * 4;
                pixels[at] = value;
                pixels[at + 1] = value;
                pixels[at + 2] = value;
                pixels[at + 3] = 255;
            }
        }
        Some((width, height, pixels))
    }

    /// 用一块 32bpp 的 DIB 把位图读成像素（自顶向下，行不补齐）。
    unsafe fn read_dib(hbm: HBITMAP, width: u32, height: u32, bits: u16) -> Option<Vec<u8>> {
        if width == 0 || height == 0 {
            return None;
        }
        let bpp = bits as usize;
        let stride = if bpp == 1 {
            ((width as usize + 31) / 32) * 4
        } else {
            width as usize * 4
        };
        let mut buffer = vec![0u8; stride * height as usize];

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // 负数 = 自顶向下，省得自己翻转
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: bits,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };

        let hdc = CreateCompatibleDC(std::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }
        let lines = GetDIBits(
            hdc,
            hbm,
            0,
            height,
            buffer.as_mut_ptr() as *mut c_void,
            &mut info,
            DIB_RGB_COLORS,
        );
        DeleteDC(hdc);
        (lines != 0).then_some(buffer)
    }

    /// 注入一条「按键组合」序列，例如 `Ctrl+S` 或者 `Ctrl+A Delete`。
    ///
    /// 用 `SendInput` 而不是 `keybd_event`（后者已废弃，而且在 UAC 提权窗口上会被丢）。
    /// 语法：空格分隔多次按键，`+` 连接同一次按键里的修饰键与主键。
    pub fn send_keys(input: &str) -> Result<(), String> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};

        let mut events: Vec<INPUT> = Vec::new();
        for combo in input.split_whitespace() {
            let mut mods: Vec<u16> = Vec::new();
            let mut main: Option<u16> = None;
            for part in combo.split('+') {
                let token = part.trim();
                if token.is_empty() {
                    continue;
                }
                let lower = token.to_ascii_lowercase();
                // 左右分开的修饰键走各自的虚拟键，通用的还是 VK_CONTROL 那一套
                if let Some(vk) = modifier_key(&lower) {
                    if !mods.contains(&vk) {
                        mods.push(vk);
                    }
                    continue;
                }
                main = Some(virtual_key(&lower).ok_or_else(|| format!("认不出的按键 {token:?}"))?);
            }
            let Some(main) = main else {
                return Err(format!("{combo:?} 里没有主键"));
            };
            for key in &mods {
                events.push(key_input(*key, false));
            }
            events.push(key_input(main, false));
            events.push(key_input(main, true));
            for key in mods.iter().rev() {
                events.push(key_input(*key, true));
            }
        }

        if events.is_empty() {
            return Ok(());
        }
        let sent = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent as usize != events.len() {
            return Err(format!("SendInput 只送出 {sent}/{} 条", events.len()));
        }
        Ok(())
    }

    /// 注入一段文本（走 Unicode 输入，不碰剪贴板，中文也能打）。
    /// 「发送文本」但认识 `{Enter}` `{Tab}` `{Backspace}` 这类按键记号。
    ///
    /// 为什么需要：`send_text` 走的是 Unicode 注入，`{Enter}` 会**原样打出**
    /// 大括号；可用户写「发送文本」时十有八九想的是「打这段字，中间按一下回车」。
    ///
    /// 规则（和 AHK 的 Send 一致）：
    /// - `{Enter}` `{Tab}` `{Backspace}` `{Esc}` `{Left}` `{Space}` `{F5}`
    ///   `{Ctrl+S}` 这类 → 当成按键按一下
    /// - `{{` → 一个字面的 `{`；`}}` → 一个字面的 `}`
    /// - 认不出来的 `{xxx}` **原样保留**（宁可打出大括号，也不能把内容吃掉）
    /// 剪贴板全量快照（AHK 的 `ClipboardAll`）。
    pub struct ClipboardSnapshot {
        /// (格式, 原始字节) —— 只收 HGLOBAL 类格式
        pub blocks: Vec<(u32, Vec<u8>)>,
        /// CF_BITMAP 是 GDI 位图句柄，不是 HGLOBAL，单独复制一份留着
        pub bitmap: Option<isize>,
    }

    /// 把剪贴板里**所有格式**都存下来（图片、RTF、HTML、自定义格式都在内）。
    ///
    /// AHK 的 `ClipboardAll` 就是干这个的：临时用一下剪贴板，用完原样还回去，
    /// 不该把用户的图片/富文本弄没。
    ///
    /// 存不了的几类：延迟渲染格式（0x0080~0x00FF，句柄为空）、
    /// 增强图元（CF_ENHMETAFILE）、调色板（CF_PALETTE）、图元（CF_METAFILEPICT）——
    /// 它们不是普通内存块，强行拷会拷出错的东西，宁可跳过。
    pub fn clipboard_snapshot() -> Option<ClipboardSnapshot> {
        use windows_sys::Win32::System::DataExchange::{
            EnumClipboardFormats, GetClipboardData,
        };
        use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
        use windows_sys::Win32::UI::WindowsAndMessaging::{CopyImage, IMAGE_BITMAP};
        const CF_BITMAP: u32 = 2;
        const CF_METAFILEPICT: u32 = 3;
        const CF_PALETTE: u32 = 9;
        const CF_ENHMETAFILE: u32 = 14;
        unsafe {
            if !open_clipboard() {
                return None;
            }
            let mut blocks: Vec<(u32, Vec<u8>)> = Vec::new();
            let mut bitmap: Option<isize> = None;
            let mut format = 0u32;
            loop {
                format = EnumClipboardFormats(format);
                if format == 0 {
                    break;
                }
                if (0x0080..=0x00FF).contains(&format) {
                    continue; // 延迟渲染 / 私有格式，没有实体数据
                }
                let handle = GetClipboardData(format);
                if handle.is_null() {
                    continue;
                }
                if format == CF_BITMAP {
                    let copy = CopyImage(handle, IMAGE_BITMAP, 0, 0, 0);
                    if !copy.is_null() {
                        bitmap = Some(copy as isize);
                    }
                    continue;
                }
                if format == CF_METAFILEPICT || format == CF_PALETTE || format == CF_ENHMETAFILE {
                    continue;
                }
                let size = GlobalSize(handle);
                if size == 0 {
                    continue;
                }
                let pointer = GlobalLock(handle) as *const u8;
                if pointer.is_null() {
                    continue;
                }
                let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
                GlobalUnlock(handle);
                blocks.push((format, bytes));
            }
            CloseClipboard();
            Some(ClipboardSnapshot { blocks, bitmap })
        }
    }

    /// 把快照原样放回剪贴板（快照被消费：里面的 GDI 句柄交给剪贴板了）。
    pub fn clipboard_restore(snapshot: ClipboardSnapshot) {
        use windows_sys::Win32::System::Memory::{
            GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
        };
        const CF_BITMAP: u32 = 2;
        unsafe {
            if !open_clipboard() {
                return;
            }
            EmptyClipboard();
            for (format, bytes) in snapshot.blocks {
                let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
                if memory.is_null() {
                    continue;
                }
                let pointer = GlobalLock(memory) as *mut u8;
                if pointer.is_null() {
                    continue;
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
                GlobalUnlock(memory);
                SetClipboardData(format, memory as HANDLE);
            }
            if let Some(bitmap) = snapshot.bitmap {
                SetClipboardData(CF_BITMAP, bitmap as HANDLE);
            }
            CloseClipboard();
        }
    }

    /// 用剪贴板粘贴：写剪贴板 → `Ctrl+V` → 等它粘完 → **还原原来的剪贴板**。
    ///
    /// 为什么不直接用 `send_text`：终端、部分 Electron 应用（VS Code 终端、
    /// 某些聊天工具）不吃 Unicode 注入，但一定吃 `Ctrl+V`。
    ///
    /// 还原只做到「文本」这一层：原来剪贴板里是图片 / 文件的话还原不回去
    /// （只能清空），这一点在设置页的提示里写清楚了。
    pub fn paste_text(text: &str, wait_ms: u64) -> Result<(), String> {
        // 先整份备份（文本、图片、RTF、自定义格式全在），用完原样还回去
        let snapshot = clipboard_snapshot();
        // 写进去之后**读回核对**：剪贴板可能被别的程序锁着，写失败的话
        // 直接按 Ctrl+V 只会粘出一片空白（实测踩过）。写不成就退回直接注入。
        let mut written = false;
        for _ in 0..5 {
            write_clipboard_text(text);
            std::thread::sleep(std::time::Duration::from_millis(20));
            if read_clipboard_text().as_deref() == Some(text) {
                written = true;
                break;
            }
        }
        if !written {
            if let Some(snapshot) = snapshot {
                clipboard_restore(snapshot);
            }
            return send_text(text);
        }
        let sent = send_keys("Ctrl+V");
        // 给目标程序留出真正粘贴的时间（每条动作可以自己配），然后还原
        std::thread::sleep(std::time::Duration::from_millis(wait_ms.clamp(30, 3000)));
        match snapshot {
            Some(snapshot) => clipboard_restore(snapshot),
            // 备份失败（剪贴板被别人锁着）：至少别把自己的内容留在里面
            None => write_clipboard_text(""),
        }
        sent
    }

    pub fn send_text_with_keys(text: &str) -> Result<(), String> {
        let mut literal = String::new();
        let mut chars = text.chars().peekable();
        let mut flush = |literal: &mut String| -> Result<(), String> {
            if literal.is_empty() {
                return Ok(());
            }
            let chunk = std::mem::take(literal);
            send_text(&chunk)?;
            // 文本（`KEYEVENTF_UNICODE`）进目标窗口是**异步**的，按键事件却立刻
            // 处理：不等一下的话顺序会反过来 —— `第一{Enter}第二` 实测打出来是
            // 「回车、第一、第二」（回车跑到前面去了）。给它十几毫秒落地。
            std::thread::sleep(std::time::Duration::from_millis(18));
            Ok(())
        };
        while let Some(ch) = chars.next() {
            match ch {
                '{' => {
                    if chars.peek() == Some(&'{') {
                        chars.next();
                        literal.push('{');
                        continue;
                    }
                    let mut token = String::new();
                    let mut closed = false;
                    for next in chars.by_ref() {
                        if next == '}' {
                            closed = true;
                            break;
                        }
                        token.push(next);
                    }
                    if !closed {
                        // 没闭合：当普通字符
                        literal.push('{');
                        literal.push_str(&token);
                        continue;
                    }
                    if token.is_empty() {
                        literal.push_str("{}");
                        continue;
                    }
                    // **先把前面攒的文本送出去**，再按这个键 —— 顺序反了就会打成
                    // 「回车、第一、第二」（实测过）。
                    flush(&mut literal)?;
                    match send_keys(&token) {
                        Ok(()) => std::thread::sleep(std::time::Duration::from_millis(12)),
                        Err(_) => {
                            // 认不出来的记号：原样打出去
                            literal.push('{');
                            literal.push_str(&token);
                            literal.push('}');
                        }
                    }
                }
                '}' => {
                    if chars.peek() == Some(&'}') {
                        chars.next();
                    }
                    literal.push('}');
                }
                other => literal.push(other),
            }
        }
        flush(&mut literal)
    }

    pub fn send_text(text: &str) -> Result<(), String> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            KEYEVENTF_UNICODE,
        };

        let mut events: Vec<INPUT> = Vec::new();
        for unit in text.encode_utf16() {
            for up in [false, true] {
                let flags = if up {
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                } else {
                    KEYEVENTF_UNICODE
                };
                events.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: 0,
                            wScan: unit,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: INJECT_MAGIC,
                        },
                    },
                });
            }
        }
        if events.is_empty() {
            return Ok(());
        }
        let sent = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent as usize != events.len() {
            return Err(format!("SendInput 只送出 {sent}/{} 条", events.len()));
        }
        Ok(())
    }

    /// 虚拟键码（表里没有的返回 `None`）。
    pub(crate) fn virtual_key(name: &str) -> Option<u16> {
        let lower = name.to_ascii_lowercase();
        let mut chars = lower.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            if ch.is_ascii_alphabetic() {
                return Some(ch.to_ascii_uppercase() as u16);
            }
            if ch.is_ascii_digit() {
                return Some(ch as u16);
            }
        }
        if let Some(rest) = lower.strip_prefix('f') {
            if let Ok(number) = rest.parse::<u16>() {
                // F1–F24：前 12 个是 0x70 起，后 12 个接着往下排
                if (1..=24).contains(&number) {
                    return Some(0x70 + number - 1);
                }
            }
        }
        // 小键盘（键位表用的是 `Num7` / `NumAdd` 这种写法）
        if let Some(rest) = lower.strip_prefix("num") {
            let code = match rest {
                "0" => 0x60,
                "1" => 0x61,
                "2" => 0x62,
                "3" => 0x63,
                "4" => 0x64,
                "5" => 0x65,
                "6" => 0x66,
                "7" => 0x67,
                "8" => 0x68,
                "9" => 0x69,
                "*" | "mul" => 0x6A,
                "+" | "add" => 0x6B,
                "-" | "sub" => 0x6D,
                "." | "dot" => 0x6E,
                "/" | "div" => 0x6F,
                "enter" => 0x0D,
                "lock" => 0x90,
                _ => 0,
            };
            if code != 0 {
                return Some(code);
            }
        }
        let code = match lower.as_str() {
            // 多媒体键 / 浏览器键 / 启动键：名字和 AHK 一样（`Volume_Up`、
            // `Media_Play_Pause` …），从 AHK 脚本里抄过来的写法能直接用。
            "volume_mute" | "volumemute" => 0xAD,
            "volume_down" | "volumedown" => 0xAE,
            "volume_up" | "volumeup" => 0xAF,
            "media_next" | "medianext" | "nexttrack" => 0xB0,
            "media_prev" | "mediaprev" | "prevtrack" => 0xB1,
            "media_stop" | "mediastop" => 0xB2,
            "media_play_pause" | "mediaplaypause" | "playpause" => 0xB3,
            "launch_mail" | "launchmail" => 0xB4,
            "launch_media" | "launchmedia" => 0xB5,
            "launch_app1" | "launchapp1" => 0xB6,
            "launch_app2" | "launchapp2" => 0xB7,
            "browser_back" | "browserback" => 0xA6,
            "browser_forward" | "browserforward" => 0xA7,
            "browser_refresh" | "browserrefresh" => 0xA8,
            "browser_stop" | "browserstop" => 0xA9,
            "browser_search" | "browsersearch" => 0xAA,
            "browser_favorites" | "browserfavorites" => 0xAB,
            "browser_home" | "browserhome" => 0xAC,
            "sleep" => 0x5F,
            "enter" | "return" => 0x0D,
            "esc" | "escape" => 0x1B,
            "tab" => 0x09,
            "space" | "spacebar" => 0x20,
            "backspace" | "back" => 0x08,
            "delete" | "del" => 0x2E,
            "insert" | "ins" => 0x2D,
            "home" => 0x24,
            "end" => 0x23,
            "pageup" | "pgup" => 0x21,
            "pagedown" | "pgdn" => 0x22,
            "up" => 0x26,
            "down" => 0x28,
            "left" => 0x25,
            "right" => 0x27,
            "capslock" => 0x14,
            "numlock" => 0x90,
            "scrolllock" | "scr" => 0x91,
            "pause" => 0x13,
            "menu" | "apps" => 0x5D,
            "printscreen" | "prtsc" => 0x2C,
            "-" => 0xBD,
            "=" => 0xBB,
            "[" => 0xDB,
            "]" => 0xDD,
            "\\" => 0xDC,
            ";" => 0xBA,
            "'" => 0xDE,
            "," => 0xBC,
            "." => 0xBE,
            "/" => 0xBF,
            "`" => 0xC0,
            _ => return None,
        };
        Some(code)
    }

    /// 修饰键 → 虚拟键码（左右分开）。
    ///
    /// 发送按键是往系统里真注入，所以 `LCtrl` / `RCtrl` 能各管各的；
    /// 全局快捷键那边（`RegisterHotKey`）做不到，只能在注册时合成一个。
    fn modifier_key(lower: &str) -> Option<u16> {
        let code = match lower {
            "ctrl" | "control" => 0x11,
            "lctrl" | "leftctrl" => 0xA2,
            "rctrl" | "rightctrl" => 0xA3,
            "shift" => 0x10,
            "lshift" | "leftshift" => 0xA0,
            "rshift" | "rightshift" => 0xA1,
            "alt" => 0x12,
            "lalt" | "leftalt" => 0xA4,
            "ralt" | "rightalt" => 0xA5,
            "win" | "super" | "cmd" | "meta" | "lwin" => 0x5B,
            "rwin" => 0x5C,
            _ => return None,
        };
        Some(code)
    }

    /// 一次「按下 / 抬起」的键盘事件。
    fn key_input(vk: u16, up: bool) -> windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        };
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: INJECT_MAGIC,
                },
            },
        }
    }

    /// 只移动，不改尺寸、不改层级、不抢焦点。
    pub fn move_window(hwnd: isize, x: i32, y: i32) {
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    // -----------------------------------------------------------------------
    // 剪贴板（抓选中内容用）
    // -----------------------------------------------------------------------
    // 格式号定义在模块顶层（`super::CF_*`），`selection::detect()` 也要用。

    use super::{CF_BITMAP, CF_DIB, CF_DIBV5, CF_HDROP, CF_TEXT, CF_UNICODETEXT};

    /// 查一个自定义格式的名字（"PNG" 这类没有固定编号，编号由系统发）。
    fn registered_format(name: &str) -> u32 {
        super::registered_format(name)
    }

    /// 剪贴板里现在有哪些格式号（排查「类型认错了」用）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn clipboard_formats() -> Vec<u32> {
        let mut formats = Vec::new();
        unsafe {
            if !open_clipboard() {
                return formats;
            }
            let mut format = EnumClipboardFormats(0);
            while format != 0 {
                formats.push(format);
                format = EnumClipboardFormats(format);
            }
            CloseClipboard();
        }
        formats
    }

    /// 剪贴板里有没有「文件列表」。
    pub fn clipboard_has_files() -> bool {
        unsafe { IsClipboardFormatAvailable(CF_HDROP) != 0 }
    }

    /// 剪贴板里有没有图片（位图，或浏览器 / 截图工具塞的 PNG）。
    ///
    /// 注意顺序：`CF_DIB` 在「复制图片链接」时也可能出现，所以调用方要**先看文件列表**，
    /// 再看图片，最后才看文本 —— 见 `selection::capture()`。
    pub fn clipboard_has_image() -> bool {
        let formats = [CF_DIB, CF_DIBV5, CF_BITMAP];
        if formats
            .iter()
            .any(|format| unsafe { IsClipboardFormatAvailable(*format) != 0 })
        {
            return true;
        }
        matches!(registered_format("PNG"), id if id != 0
            && unsafe { IsClipboardFormatAvailable(id) != 0 })
    }

    /// 剪贴板里的文件路径（`CF_HDROP`）。
    pub fn read_clipboard_files() -> Vec<String> {
        let bytes = read_format_bytes(CF_HDROP);
        match bytes {
            Some(bytes) => super::parse_dropfiles(&bytes),
            None => Vec::new(),
        }
    }

    /// 剪贴板里那张图的宽高（DIB 读不到就试 PNG）。
    pub fn clipboard_image_size() -> Option<(u32, u32)> {
        if let Some(bytes) = read_format_bytes(CF_DIB).or_else(|| read_format_bytes(CF_DIBV5)) {
            if let Some(size) = super::parse_dib_size(&bytes) {
                return Some(size);
            }
        }
        let png = registered_format("PNG");
        if png != 0 {
            if let Some(bytes) = read_format_bytes(png) {
                return super::parse_png_size(&bytes);
            }
        }
        None
    }

    /// 单独打开剪贴板读一个格式的字节（`format_bytes` 要求外面已经 Open 过）。
    fn read_format_bytes(format: u32) -> Option<Vec<u8>> {
        unsafe {
            if !open_clipboard() {
                return None;
            }
            let bytes = format_bytes(format);
            CloseClipboard();
            bytes
        }
    }

    /// 打开剪贴板；被别的进程占着就等一下再试。
    ///
    /// 剪贴板是**全局独占**的，而 Explorer 复制文件之后还会在剪贴板里挂几个
    /// 「延迟渲染」的格式，这时候 `OpenClipboard` 失败一次很常见。
    /// 只试一次的话，抓到的内容就被误判成「没选中」，菜单也不筛选了。
    fn open_clipboard() -> bool {
        for attempt in 0..8 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(6));
            }
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return true;
            }
        }
        false
    }

    /// 剪贴板的「版本号」：任何一次写入都会让它 +1。
    ///
    /// 判断「Ctrl+C 到底有没有拷出东西」就看它变没变 —— 比轮询内容可靠得多。
    pub fn clipboard_sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    /// 这个格式的句柄是不是一块全局内存（能 GlobalLock 读字节）。
    ///
    /// 位图 / 元文件 / 调色板那几种是 GDI 句柄，硬读会崩，跳过。
    /// 常见应用复制的文本 / HTML / RTF / PNG 都是内存块，够用了。
    fn is_memory_format(format: u32) -> bool {
        !matches!(format, 2 | 3 | 9 | 14) && !(0x80..=0x8f).contains(&format)
    }

    fn format_bytes(format: u32) -> Option<Vec<u8>> {
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let size = GlobalSize(handle);
            if size == 0 {
                return None;
            }
            let ptr = GlobalLock(handle) as *const u8;
            if ptr.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
            GlobalUnlock(handle);
            Some(bytes)
        }
    }

    /// 把剪贴板里所有「内存块格式」都拷一份出来（调用方记得之后 `restore_clipboard`）。
    ///
    /// 顺带记下**全部**格式号：位图句柄那几种读不了字节，但「有没有」是判定类型的依据。
    pub fn snapshot_clipboard() -> Option<super::ClipboardBackup> {
        let mut entries = Vec::new();
        let mut formats = Vec::new();
        unsafe {
            if !open_clipboard() {
                return None;
            }
            let mut format = EnumClipboardFormats(0);
            while format != 0 {
                formats.push(format);
                if is_memory_format(format) {
                    if let Some(bytes) = format_bytes(format) {
                        entries.push((format, bytes));
                    }
                }
                format = EnumClipboardFormats(format);
            }
            CloseClipboard();
        }
        Some(super::ClipboardBackup { entries, formats })
    }

    /// 按快照把剪贴板放回去（先清空，再把每种格式原样写回）。
    pub fn restore_clipboard(backup: &super::ClipboardBackup) {
        unsafe {
            if !open_clipboard() {
                return;
            }
            EmptyClipboard();
            for (format, bytes) in &backup.entries {
                let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
                if memory.is_null() {
                    continue;
                }
                let ptr = GlobalLock(memory) as *mut u8;
                if ptr.is_null() {
                    continue;
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
                GlobalUnlock(memory);
                // SetClipboardData 成功后内存归系统所有，不能再自己释放
                SetClipboardData(*format, memory as HANDLE);
            }
            CloseClipboard();
        }
    }

    /// 剪贴板里的文本（优先 Unicode，没有就看 ANSI）。
    pub fn read_clipboard_text() -> Option<String> {
        unsafe {
            if !open_clipboard() {
                return None;
            }
            let mut text = None;
            let handle = GetClipboardData(CF_UNICODETEXT);
            if !handle.is_null() {
                let ptr = GlobalLock(handle);
                if !ptr.is_null() {
                    let units = ptr as *const u16;
                    let mut len = 0usize;
                    while *units.add(len) != 0 {
                        len += 1;
                    }
                    text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                        units, len,
                    )));
                    GlobalUnlock(handle);
                }
            }
            if text.is_none() {
                let handle = GetClipboardData(CF_TEXT);
                if !handle.is_null() {
                    let ptr = GlobalLock(handle);
                    if !ptr.is_null() {
                        let bytes = ptr as *const u8;
                        let mut len = 0usize;
                        while *bytes.add(len) != 0 {
                            len += 1;
                        }
                        text = Some(
                            String::from_utf8_lossy(std::slice::from_raw_parts(bytes, len))
                                .to_string(),
                        );
                        GlobalUnlock(handle);
                    }
                }
            }
            CloseClipboard();
            text
        }
    }

    /// 往剪贴板写一段文本（菜单第一项「复制选中内容」用它）。
    pub fn write_clipboard_text(text: &str) {
        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            if !open_clipboard() {
                return;
            }
            EmptyClipboard();
            let size = std::mem::size_of_val(units.as_slice());
            let memory = GlobalAlloc(GMEM_MOVEABLE, size);
            if !memory.is_null() {
                let ptr = GlobalLock(memory) as *mut u16;
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
                    GlobalUnlock(memory);
                    SetClipboardData(CF_UNICODETEXT, memory as HANDLE);
                }
            }
            CloseClipboard();
        }
    }

    /// 物理上还压着的修饰键：`(探测用的虚拟键, 注入 keyup 时用的虚拟键, 能不能顶开)`。
    ///
    /// 左右分开列，才能只给真正按着的那一个发 keyup（左右混着发虽然也能清状态，
    /// 但会让「另一侧真的按着」的键莫名其妙松开）。
    ///
    /// Win 键**不能**顶开：外壳程序看到一条 Win keyup 就会把开始菜单弹出来，
    /// 焦点、前台全被抢走，比抓不到选中内容还糟。所以 Win 只能等用户自己松手。
    const MODIFIER_KEYS: [(i32, u16, bool); 8] = [
        (0xA2, 0xA2, true),  // LCONTROL
        (0xA3, 0xA3, true),  // RCONTROL
        (0xA0, 0xA0, true),  // LSHIFT
        (0xA1, 0xA1, true),  // RSHIFT
        (0xA4, 0xA4, true),  // LMENU
        (0xA5, 0xA5, true),  // RMENU
        (0x5B, 0x5B, false), // LWIN
        (0x5C, 0x5C, false), // RWIN
    ];

    /// 现在有没有修饰键还压着。
    fn modifiers_down() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        MODIFIER_KEYS
            .iter()
            .any(|(probe, _, _)| unsafe { (GetAsyncKeyState(*probe) as u16 & 0x8000) != 0 })
    }

    /// 等用户把修饰键松开（最多 `max_ms` 毫秒）；返回是否已经全松开。
    ///
    /// 全局热键是 Ctrl+Alt+M：按下的那一瞬间 Alt 还压着，这时候注入 Ctrl+C 会变成
    /// Ctrl+Alt+C —— 目标程序根本不认。所以先等物理键放开再发。
    pub fn wait_modifiers_released(max_ms: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(max_ms);
        loop {
            if !modifiers_down() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
    }

    /// Win 键还按着吗（这种只能等，见 `MODIFIER_KEYS` 的说明）。
    pub fn win_key_down() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        MODIFIER_KEYS
            .iter()
            .filter(|(_, _, can_release)| !can_release)
            .any(|(probe, _, _)| unsafe { (GetAsyncKeyState(*probe) as u16 & 0x8000) != 0 })
    }

    /// 把还压着的 Ctrl / Alt / Shift「顶开」：给每一个按下的键注入一条 keyup。
    ///
    /// 用户按住热键不松手时（Ctrl+Alt+M 按着不放很常见），干等是等不完的 ——
    /// 等满预算之后还得在 Alt 压着的状态下发键，目标程序收到的是 Ctrl+Alt+C，
    /// 复制不了。注入 keyup 之后系统认为这些修饰键已经松开，随后的 Ctrl+C 才是
    /// 干净的三键；用户真正松手时那条 keyup 就是多余的，没有副作用。
    /// 只发 keyup、不发 keydown —— 不能替用户「按住」任何键。
    pub fn release_modifiers() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, SendInput, INPUT};
        let events: Vec<INPUT> = MODIFIER_KEYS
            .iter()
            .filter(|(probe, _, can_release)| {
                *can_release && unsafe { (GetAsyncKeyState(*probe) as u16 & 0x8000) != 0 }
            })
            .map(|(_, vk, _)| key_input(*vk, true))
            .collect();
        if events.is_empty() {
            return;
        }
        unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
        }
    }

    // -----------------------------------------------------------------------
    // 低层键盘钩子（热字符串用）
    // -----------------------------------------------------------------------

    /// 钩子回调：剥出按键信息 → 交给 sink → 原样放行（吞不吞由 sink 决定）。
    ///
    /// 这里**每个按键都会走一趟**，所以只做内存操作：谁来处理、处理成什么样
    /// 全在 `hotstring` / `hotkey` 里，这里连字符串都不碰。慢一毫秒用户就能感觉打字变粘。
    ///
    /// 顺序是定死的：**热键那一关先问**（它要抢在热字符串前面，
    /// 否则 `Ctrl+Alt+1` 这种组合会先被热字符串当成打字记进缓冲区），
    /// 它不要了才轮到热字符串。
    unsafe extern "system" fn keyboard_proc(code: i32, wparam: usize, lparam: isize) -> isize {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        const VK_SHIFT: i32 = 0x10;
        if code >= 0 {
            let info = &*(lparam as *const KBDLLHOOKSTRUCT);
            // 自己注入的（删触发词 / 打替换文本 / 发送按键）：直接放行，不记录也不拦。
            let mine = info.dwExtraInfo == INJECT_MAGIC;
            if !mine {
                let message = wparam as u32;
                let key = RawKey {
                    vk: info.vkCode as u16,
                    down: message == WM_KEYDOWN || message == WM_SYSKEYDOWN,
                    shift: (GetAsyncKeyState(VK_SHIFT) as u16 & 0x8000) != 0,
                };
                // 1) 全局热键：命中就吞掉，这也就是「覆盖别的程序的热键」的来源
                let taken = KEY_GATE
                    .lock()
                    .ok()
                    .and_then(|slot| slot.as_ref().map(|gate| gate(key)))
                    .unwrap_or(false);
                if taken {
                    return 1;
                }
                // 2) 热字符串
                let swallow = KEY_SINKS
                    .lock()
                    .map(|sinks| sinks.iter().any(|sink| sink(key)))
                    .unwrap_or(false);
                if swallow {
                    return 1;
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }

    /// 鼠标按下 / 滚轮：热字符串的缓冲区该清掉了（光标可能已经挪到别处）。
    ///
    /// 只装一份、只处理「按下」这几种消息；鼠标移动一律直接放行（那是每秒上百次的东西）。
    unsafe extern "system" fn mouse_proc(code: i32, wparam: usize, lparam: isize) -> isize {
        const WM_LBUTTONDOWN: u32 = 0x0201;
        const WM_RBUTTONDOWN: u32 = 0x0204;
        const WM_MBUTTONDOWN: u32 = 0x0207;
        const WM_MOUSEWHEEL: u32 = 0x020A;
        const WM_XBUTTONDOWN: u32 = 0x020B;
        if code >= 0 {
            let message = wparam as u32;
            if matches!(
                message,
                WM_LBUTTONDOWN
                    | WM_RBUTTONDOWN
                    | WM_MBUTTONDOWN
                    | WM_XBUTTONDOWN
                    | WM_MOUSEWHEEL
            ) {
                let info = &*(lparam as *const MSLLHOOKSTRUCT);
                let my_injection = info.dwExtraInfo == INJECT_MAGIC;
                // 滚轮的方向在 mouseData 的高 16 位（有符号）
                let wheel = if message == WM_MOUSEWHEEL {
                    ((info.mouseData >> 16) as u16 as i16) as i32
                } else {
                    0
                };
                let button = match message {
                    WM_LBUTTONDOWN => Some(MouseKind::Left),
                    WM_RBUTTONDOWN => Some(MouseKind::Right),
                    WM_MBUTTONDOWN => Some(MouseKind::Middle),
                    // 侧键：mouseData 的高 16 位 1 = X1，2 = X2
                    WM_XBUTTONDOWN => match (info.mouseData >> 16) as u16 {
                        1 => Some(MouseKind::X1),
                        2 => Some(MouseKind::X2),
                        _ => None,
                    },
                    _ => None,
                };
                // 全局热键可以先要（「鼠标侧键也当快捷键」就是靠这一关）
                if !my_injection {
                    let taken = MOUSE_GATE
                        .lock()
                        .ok()
                        .and_then(|slot| slot.as_ref().map(|gate| gate(RawMouse { button, wheel })))
                        .unwrap_or(false);
                    if taken {
                        return 1;
                    }
                }
                if button.is_some() {
                    // 「点了一下鼠标」的计数器：菜单靠它判断「点在菜单外面了」。
                    // 钩子比轮询可靠 —— 一次很快的点击（按下到抬起不到 60ms）
                    // 用 `GetAsyncKeyState` 是捞不到的。
                    CLICK_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if let Ok(slot) = POINTER_SINK.lock() {
                    if let Some(sink) = slot.as_ref() {
                        sink();
                    }
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }

    /// 装全局低层键盘 / 鼠标钩子（幂等：已经装过就直接返回 `true`）。
    ///
    /// 低层钩子的回调是在**装钩子的那个线程**上被调用的，所以这个线程必须一直跑
    /// 消息循环 —— 钩子装好之后线程就留在那条 `GetMessageW` 上，直到进程退出。
    pub fn ensure_input_hooks() -> bool {
        if HOOK_READY.load(std::sync::atomic::Ordering::SeqCst) {
            return true;
        }
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let spawned = std::thread::Builder::new()
            .name("runany-keyboard-hook".to_string())
            .spawn(move || unsafe {
                let hook =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), std::ptr::null_mut(), 0);
                if hook.is_null() {
                    let _ = tx.send(false);
                    return;
                }
                // 鼠标钩子装不上不影响键盘（只是「侧键当热键」这种用不了）
                let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), std::ptr::null_mut(), 0);
                HOOK_READY.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = tx.send(true);
                let mut message: MSG = std::mem::zeroed();
                while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                UnhookWindowsHookEx(hook);
                if !mouse.is_null() {
                    UnhookWindowsHookEx(mouse);
                }
            });
        if spawned.is_err() {
            return false;
        }
        rx.recv_timeout(std::time::Duration::from_millis(1500))
            .unwrap_or(false)
    }

    /// 热键那一关：返回 `true` = 这个键我要了（钩子吞掉它）。
    ///
    /// 单独一格（不是混在 `KEY_SINKS` 里）是因为它必须**第一个**问：
    /// 一旦热字符串先把 `Ctrl+Alt+1` 记进缓冲区，热键再吞就已经晚了。
    pub fn set_key_gate(gate: Box<dyn Fn(RawKey) -> bool + Send + Sync>) -> bool {
        if let Ok(mut slot) = KEY_GATE.lock() {
            *slot = Some(std::sync::Arc::from(gate));
        }
        ensure_input_hooks()
    }

    /// 鼠标那一关（侧键 / 中键 / 滚轮当热键）。
    pub fn set_mouse_gate(gate: Box<dyn Fn(RawMouse) -> bool + Send + Sync>) -> bool {
        if let Ok(mut slot) = MOUSE_GATE.lock() {
            *slot = Some(std::sync::Arc::from(gate));
        }
        ensure_input_hooks()
    }

    /// 再挂一条键盘回调（热字符串用；返回 `true` 表示这个键被吞了）。
    pub fn add_key_sink(sink: Box<dyn Fn(RawKey) -> bool + Send + Sync>) -> bool {
        if let Ok(mut sinks) = KEY_SINKS.lock() {
            sinks.push(std::sync::Arc::from(sink));
        }
        ensure_input_hooks()
    }

    /// 鼠标按下 / 滚轮时叫一下（热字符串拿它清缓冲）。
    pub fn add_pointer_sink(sink: Box<dyn Fn() + Send + Sync>) -> bool {
        if let Ok(mut slot) = POINTER_SINK.lock() {
            *slot = Some(std::sync::Arc::from(sink));
        }
        ensure_input_hooks()
    }

    /// 兼容旧调用（热字符串那一支）：键盘回调 + 鼠标回调一起挂。
    pub fn install_input_hooks(
        key: Box<dyn Fn(RawKey) -> bool + Send>,
        pointer: Box<dyn Fn() + Send>,
    ) -> bool {
        // 热字符串那一条回调是单线程造出来的 `Box<dyn Fn + Send>`：包一层锁转成
        // `Sync` 的（钩子线程是唯一调用方，这把锁实际永远不竞争）
        let key = std::sync::Mutex::new(key);
        let pointer = std::sync::Mutex::new(pointer);
        let added = add_key_sink(Box::new(move |raw| {
            key.lock().map(|f| f(raw)).unwrap_or(false)
        }));
        add_pointer_sink(Box::new(move || {
            if let Ok(f) = pointer.lock() {
                f();
            }
        }));
        added
    }

    /// 八颗修饰键**现在**是不是按着（左右分开）。
    ///
    /// 顺序：L/R Ctrl、L/R Shift、L/Alt、L/R Win。热键靠它区分「左 Alt+0」和
    /// 「右 Alt+0」—— `RegisterHotKey` 只有一颗 Ctrl，做不到这件事。
    pub fn modifier_state() -> [bool; 8] {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        const VKS: [i32; 8] = [0xA2, 0xA3, 0xA0, 0xA1, 0xA4, 0xA5, 0x5B, 0x5C];
        let mut out = [false; 8];
        for (slot, vk) in out.iter_mut().zip(VKS) {
            *slot = unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 };
        }
        out
    }

    /// NumLock 现在是开着吗（小键盘面板按它决定那一片键叫什么）。
    pub fn numlock_on() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_NUMLOCK};
        unsafe { (GetKeyState(VK_NUMLOCK as i32) & 1) != 0 }
    }

    /// 替用户按一下 NumLock（真的拨系统的状态，和小键盘上那颗键一样）。
    pub fn press_numlock() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::SendInput;
        const VK_NUMLOCK: u16 = 0x90;
        let events = [key_input(VK_NUMLOCK, false), key_input(VK_NUMLOCK, true)];
        unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT>() as i32,
            );
        }
    }

    /// 往当前焦点窗口连发 `count` 个退格（删掉已经打出去的触发词）。
    pub fn send_backspaces(count: usize) {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};
        const VK_BACK: u16 = 0x08;
        if count == 0 {
            return;
        }
        let mut events: Vec<INPUT> = Vec::with_capacity(count * 2);
        for _ in 0..count {
            events.push(key_input(VK_BACK, false));
            events.push(key_input(VK_BACK, true));
        }
        unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
        }
    }

    /// 补发一个虚拟键（热字符串吞掉 Tab / 回车之后要还给目标程序）。
    pub fn send_virtual_key(vk: u16) {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};
        let events = [key_input(vk, false), key_input(vk, true)];
        unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
        }
    }

    /// 前台线程里那个「文字光标」的屏幕矩形 `(left, top, right, bottom)`（物理像素）。
    ///
    /// 热字符串提示可以贴在插入点旁边而不是鼠标旁边 —— 打字的时候手在键盘上，
    /// 眼睛在光标那儿。`GetGUIThreadInfo` 是唯一不注入就能问到光标位置的办法；
    /// 目标程序不支持（控制台、部分自绘控件）时 `hwndCaret` 是 0，返回 `None`。
    pub fn caret_rect() -> Option<(i32, i32, i32, i32)> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO};
        let hwnd = foreground_window() as HWND;
        if hwnd.is_null() {
            return None;
        }
        unsafe {
            // 注意：**线程 id 是返回值**，out 参数拿到的是进程 id。
            // 把进程 id 当线程 id 传下去，`GetGUIThreadInfo` 只会返回 0，
            // 于是「贴插入点」永远悄悄退回鼠标 —— 这个 bug 就这么藏了很久。
            let thread = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
            if thread == 0 {
                return None;
            }
            let mut info: GUITHREADINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
            let ok = GetGUIThreadInfo(thread, &mut info);
            if std::env::var_os("GPUI_MENU_DEBUG_HINT").is_some() {
                eprintln!(
                    "[caret] err={} size={} fg={hwnd:?} thread={thread} ok={ok} caret={:?} focus={:?} rc=({}, {}, {}, {})",
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                    std::mem::size_of::<GUITHREADINFO>(),
                    info.hwndCaret,
                    info.hwndFocus,
                    info.rcCaret.left,
                    info.rcCaret.top,
                    info.rcCaret.right,
                    info.rcCaret.bottom
                );
            }
            if ok == 0 {
                return None;
            }
            if info.hwndCaret.is_null() {
                return None;
            }
            // rcCaret 是相对 hwndCaret 客户区的坐标，得加上窗口原点
            let mut origin = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetWindowRect(info.hwndCaret, &mut origin) == 0 {
                return None;
            }
            let caret = info.rcCaret;
            Some((
                origin.left + caret.left,
                origin.top + caret.top,
                origin.left + caret.right,
                origin.top + caret.bottom,
            ))
        }
    }

    /// 让窗口「鼠标穿透」：不接收任何点击、也不进 Alt+Tab。
    ///
    /// 热字符串的提示卡片就贴在鼠标旁边（偏移可以设成 0），
    /// 不穿透的话它会挡住用户在目标程序里的点击 —— 那比不提示还烦。
    pub fn make_click_through(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        set_click_through(hwnd, true);
    }

    /// 开/关鼠标穿透。
    ///
    /// 热字符串提示默认是穿透的（它贴在鼠标旁边，挡住点击很烦）；但用户也可以
    /// 让它「可以用鼠标点」—— 那就得把这个 ex-style 摘掉，不然点击会直接穿过卡片。
    /// `WS_EX_NOACTIVATE` 一直留着：点它也不抢前台，用户还在原来的程序里打字。
    pub fn set_click_through(hwnd: isize, on: bool) {
        if hwnd == 0 {
            return;
        }
        const WS_EX_TRANSPARENT: isize = 0x0000_0020;
        const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
        const WS_EX_NOACTIVATE: isize = 0x0800_0000;
        unsafe {
            let hwnd = hwnd as HWND;
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let base = style | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let style = if on {
                base | WS_EX_TRANSPARENT
            } else {
                base & !WS_EX_TRANSPARENT
            };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
        }
    }

    /// 热键那一关（第一个问；返回 `true` = 吞掉）
    static KEY_GATE: std::sync::Mutex<Option<std::sync::Arc<dyn Fn(RawKey) -> bool + Send + Sync>>> =
        std::sync::Mutex::new(None);
    /// 鼠标那一关（侧键 / 中键 / 滚轮当热键）
    static MOUSE_GATE: std::sync::Mutex<Option<std::sync::Arc<dyn Fn(RawMouse) -> bool + Send + Sync>>> =
        std::sync::Mutex::new(None);
    /// 其它键盘回调（热字符串）。可以有多个，任何一个说「要」就吞
    static KEY_SINKS: std::sync::Mutex<Vec<std::sync::Arc<dyn Fn(RawKey) -> bool + Send + Sync>>> =
        std::sync::Mutex::new(Vec::new());
    /// 钩子装好了没有（装一次就够）
    static HOOK_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    /// 鼠标按下 / 滚轮的回调（热字符串拿它清缓冲）
    static POINTER_SINK: std::sync::Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>> =
        std::sync::Mutex::new(None);

    /// 「鼠标被按下了几次」的计数器（钩子里 +1）。菜单的 watchdog 靠它发现
    /// 「用户点到菜单外面去了」—— 不管前台窗口有没有跟着变。
    static CLICK_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// 到目前为止一共按下了几次鼠标键（左右中，不含滚轮）。
    pub fn click_tick() -> u64 {
        CLICK_TICK.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 前台窗口的输入法现在是不是**正在用中文模式组字**。
    ///
    /// 只问「输入法开着吗」（`ImmGetOpenStatus`）远远不够：中文 Windows 上
    /// 输入法常年挂着，用户按 Shift 切到英文后按键是**直接落到目标程序**的，
    /// 这时候要是也跳过，热字符串就变成「有时能用、有时不能用」——
    /// 坏的恰好是「输入法开着、但没在组字」这最常见的一种状态。
    ///
    /// 真正的判据是转换模式里的 `IME_CMODE_NATIVE`：中文模式置位，英文模式清零。
    pub fn ime_composing() -> bool {
        // 输入法的上下文通常挂在**拿到焦点的那个控件**上（浏览器、编辑器都是这样），
        // 挂在顶层窗口上时问顶层也行 —— 两个都试一遍，先焦点后前台。
        const IME_CMODE_NATIVE: u32 = 0x0001;
        let (foreground, focus) = input_target();
        for hwnd in [focus, foreground] {
            if let Some(mode) = ime_conversion_mode(hwnd) {
                if std::env::var_os("GPUI_MENU_DEBUG_IME").is_some() {
                    eprintln!("[ime] hwnd={hwnd:x} mode=0x{mode:04x}");
                }
                return (mode & IME_CMODE_NATIVE) != 0;
            }
        }
        false
    }

    /// 这个窗口上的输入法现在是什么转换模式（`IME_CMODE_NATIVE` 置位 = 中文模式）。
    /// 问不出来（窗口没有输入法）时返回 `None`。
    ///
    /// 为什么不用 `ImmGetContext` + `ImmGetConversionStatus`：那套 API 是**线程绑定**的，
    /// 从别的线程（我们的键盘钩子线程）调用只会拿到空 context —— 于是「输入法在组字」
    /// 永远判不出来。`WM_IME_CONTROL` 走窗口消息，跨进程也能问到，AHK 也是这么干的。
    fn ime_conversion_mode(hwnd: isize) -> Option<u32> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK,
        };
        /// `WM_IME_CONTROL`：给输入法窗口发控制消息
        const WM_IME_CONTROL: u32 = 0x0283;
        /// 取转换模式（返回值里 `IME_CMODE_NATIVE` 置位 = 中文模式）
        const IMC_GETCONVERSIONMODE: usize = 0x0001;
        if hwnd == 0 {
            return None;
        }
        // 消息要发给**输入法窗口**而不是控件本身（AHK 也是先 ImmGetDefaultIMEWnd）
        let ime = unsafe { windows_sys::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd(hwnd as HWND) };
        if ime.is_null() {
            return None;
        }
        let mut result: usize = 0;
        // 20ms 上限 + ABORTIFHUNG：目标程序卡死时钩子也不能跟着卡
        let ok = unsafe {
            SendMessageTimeoutW(
                ime,
                WM_IME_CONTROL,
                IMC_GETCONVERSIONMODE,
                0,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                20,
                &mut result,
            )
        };
        (ok != 0).then_some(result as u32)
    }

    /// 取消这个窗口上正在进行的输入法组字。
    ///
    /// 为什么需要：搜狗这类输入法在「拼音打了一半、焦点被点走」时不会自己收场 ——
    /// 候选窗留在屏幕上，键盘也还被它攥着，这时候按退格既不删字也没反应，
    /// 用户会以为程序卡死。在焦点离开输入框的那一刻主动告诉输入法「这段不要了」，
    /// 它就会把组字缓冲和候选窗一起收掉。
    ///
    /// `Imm*` 这一套是**线程绑定**的，所以只能在 UI 线程调（失焦回调就是）；
    /// 从钩子线程调会拿到空 context，什么也不会发生。
    pub fn cancel_ime_composition(hwnd: isize) {
        use windows_sys::Win32::UI::Input::Ime::{
            ImmGetContext, ImmNotifyIME, ImmReleaseContext, CPS_CANCEL, NI_COMPOSITIONSTR,
        };
        use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        if hwnd == 0 {
            return;
        }
        unsafe {
            // `ImmGetContext` 只认**本线程**的输入上下文。目标程序在别的线程里，
            // 不 attach 过去拿到的是空 context —— 这一手以前悄悄失效，
            // 组字从来没被真正取消过，于是「先取消组字再退格」变成了「直接退格」，
            // 删的全是候选窗里的拼音，屏幕上的 `@win` 一个都没掉。
            let thread = GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut());
            let me = GetCurrentThreadId();
            let attached = thread != 0 && thread != me && AttachThreadInput(me, thread, 1) != 0;
            let ctx = ImmGetContext(hwnd as HWND);
            if !ctx.is_null() {
                // 没有组字时发这个也没副作用
                ImmNotifyIME(ctx, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                ImmReleaseContext(hwnd as HWND, ctx);
            }
            if attached {
                AttachThreadInput(me, thread, 0);
            }
        }
    }

    /// 组字区里现在有几个字符（0 = 没在组字）。
    ///
    /// 取消组字是**异步**的：`ImmNotifyIME(CPS_CANCEL)` 发出去之后输入法还要
    /// 转一圈才真把组字窗收掉。这段空档里注入的文本会被它当成拼音吃掉，
    /// 所以要能问「现在还在不在组字」。
    pub fn ime_composition_len(hwnd: isize) -> usize {
        use windows_sys::Win32::UI::Input::Ime::{
            ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, GCS_COMPSTR,
        };
        use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        if hwnd == 0 {
            return 0;
        }
        unsafe {
            let thread = GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut());
            let me = GetCurrentThreadId();
            let attached = thread != 0 && thread != me && AttachThreadInput(me, thread, 1) != 0;
            let ctx = ImmGetContext(hwnd as HWND);
            let mut len = 0usize;
            if !ctx.is_null() {
                // 返回字节数（宽字符）：负数表示出错
                let bytes = ImmGetCompositionStringW(ctx, GCS_COMPSTR, std::ptr::null_mut(), 0);
                if bytes > 0 {
                    len = (bytes as usize) / 2;
                }
                ImmReleaseContext(hwnd as HWND, ctx);
            }
            if attached {
                AttachThreadInput(me, thread, 0);
            }
            len
        }
    }

    /// 把输入法的**组字窗 / 候选窗**挪到窗口客户区的 `(x, y)`（物理像素）。
    ///
    /// 为什么要自己设：gpui 只在 `WM_IME_STARTCOMPOSITION` 时上报一次光标位置，
    /// 搜狗这类输入法在组字过程中会复用同一个窗口、不再重新询问，
    /// 于是候选窗停在「上一次开始组字的地方」—— 表现就是「不在输入框旁边」。
    /// 每次组字内容更新都重新摆一次，位置就始终跟着光标走。
    pub fn place_ime(hwnd: isize, x: i32, y: i32) {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::Input::Ime::{
            ImmGetContext, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow,
            CANDIDATEFORM, COMPOSITIONFORM, CFS_CANDIDATEPOS, CFS_POINT,
        };
        if hwnd == 0 {
            return;
        }
        unsafe {
            let ctx = ImmGetContext(hwnd as HWND);
            if ctx.is_null() {
                return;
            }
            let form = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT { x, y },
                rcArea: std::mem::zeroed(),
            };
            ImmSetCompositionWindow(ctx, &form);
            let candidate = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_CANDIDATEPOS,
                ptCurrentPos: POINT { x, y },
                rcArea: std::mem::zeroed(),
            };
            ImmSetCandidateWindow(ctx, &candidate);
            ImmReleaseContext(hwnd as HWND, ctx);
        }
    }

    /// 上面那个的 gpui 版：坐标是**逻辑像素**，内部按窗口缩放换成物理像素。
    ///
    /// `height` 是输入框高度：候选窗贴着光标下沿出来，所以要把 y 加上一行。
    pub fn place_ime_in(window: &gpui::Window, x: f32, y: f32, height: f32) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            let scale = window.scale_factor();
            place_ime(
                win32.hwnd.get(),
                (x * scale) as i32,
                ((y + height) * scale) as i32,
            );
        }
    }

    /// 取消 `window` 上正在进行的输入法组字（gpui 窗口 → 原生 HWND → 上面那个）。
    pub fn cancel_ime_composition_in(window: &gpui::Window) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        // `Window` 自己也有个 `window_handle()`（返回 gpui 的 AnyWindowHandle），
        // 会挡住 trait 里的同名方法，所以这里显式按 trait 调
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            cancel_ime_composition(win32.hwnd.get());
        }
    }

    /// 把窗口摆到指定位置和大小（半屏 / 居中这类内置动作要用）。
    pub fn set_window_bounds(hwnd: isize, x: i32, y: i32, width: i32, height: i32) {
        use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos;
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                std::ptr::null_mut(),
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// 最小化（`hide_window` 是 SW_HIDE，那是「藏起来」，不是最小化）。
    pub fn minimize_window(hwnd: isize) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindowAsync, SW_MINIMIZE};
        unsafe {
            ShowWindowAsync(hwnd as HWND, SW_MINIMIZE);
        }
    }

    /// 最大化 / 还原。
    /// 按 exe 文件名找进程号（不看窗口，托盘里活着的也算）。
    pub fn process_ids(exe: &str) -> Vec<u32> {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        let mut out = Vec::new();
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while ok {
            let len = entry
                .szExeFile
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case(exe) {
                out.push(entry.th32ProcessID);
            }
            ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        out
    }

    /// 这些进程的顶层可见窗口（托盘程序可能一个都没有）。
    pub fn top_windows(pids: &[u32]) -> Vec<isize> {
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let (pids, out) = &mut *(lparam as *mut (&[u32], Vec<isize>));
            if unsafe { IsWindowVisible(hwnd) } == 0 {
                return 1;
            }
            // 工具窗口（托盘图标的隐藏窗、输入法候选之类）不算
            let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            if style & 0x80 != 0 {
                return 1;
            }
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            if pids.contains(&pid) {
                out.push(hwnd as isize);
            }
            1
        }
        let mut pair: (&[u32], Vec<isize>) = (pids, Vec::new());
        unsafe {
            EnumWindows(Some(collect), &mut pair as *mut _ as LPARAM);
        }
        pair.1
    }

    /// 把窗口叫到前面；已经在前台就最小化（「激活 / 最小化窗口」那一个选项）。
    pub fn activate_or_minimize(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        let foreground = unsafe { GetForegroundWindow() as isize };
        if foreground == hwnd {
            unsafe { ShowWindowAsync(hwnd as HWND, SW_MINIMIZE) };
            return;
        }
        unsafe {
            if IsIconic(hwnd as HWND) != 0 {
                ShowWindowAsync(hwnd as HWND, SW_RESTORE);
            }
        }
        set_foreground(hwnd);
    }

    /// 好好地说一句「关掉」（发 WM_CLOSE，等于点了窗口右上角的 ✕）。
    pub fn close_window(hwnd: isize) {
        if hwnd != 0 {
            unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) };
        }
    }

    /// 起一个程序：可以带管理员权限 / 工作目录 / 窗口状态。
    ///
    /// 走 `ShellExecuteW`：`runas` 只有它能要，工作目录也只有它能一句话带过去。
    pub fn launch_as(
        command: &str,
        as_admin: bool,
        dir: Option<&str>,
        window: &str,
    ) -> Result<(), String> {
        let (exe, args) = split_command(command);
        if exe.is_empty() {
            return Ok(());
        }
        let wide = |text: &str| -> Vec<u16> {
            text.encode_utf16().chain(std::iter::once(0)).collect()
        };
        let verb = wide(if as_admin { "runas" } else { "open" });
        let file = wide(&exe);
        let params = wide(&args);
        let dir = dir.map(wide);
        let show = match window {
            "min" => SW_SHOWMINIMIZED,
            "max" => SW_SHOWMAXIMIZED,
            _ => SW_SHOWNORMAL,
        };
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params.as_ptr(),
                dir.as_ref()
                    .map(|text| text.as_ptr())
                    .unwrap_or(std::ptr::null()),
                show,
            )
        };
        // ShellExecuteW 的返回值 <= 32 都是错误码（不是句柄）
        if result as isize <= 32 {
            return Err(if crate::i18n::is_english() {
                if as_admin {
                    format!("Failed to start {exe} as administrator (declined the prompt?)")
                } else {
                    format!("Failed to start {exe}")
                }
            } else if as_admin {
                format!("以管理员身份启动失败（{exe}）：可能是点了「否」")
            } else {
                format!("启动失败（{exe}）")
            });
        }
        Ok(())
    }

    /// 把一条命令行拆成「程序 + 参数」（程序可能带引号）。
    pub fn split_command(command: &str) -> (String, String) {
        let trimmed = command.trim();
        if let Some(rest) = trimmed.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
            }
        }
        match trimmed.split_once(' ') {
            Some((exe, args)) => (exe.to_string(), args.trim().to_string()),
            None => (trimmed.to_string(), String::new()),
        }
    }

    pub fn toggle_maximize(hwnd: isize) -> Result<(), String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            ShowWindowAsync, SW_MAXIMIZE, SW_RESTORE,
        };
        if hwnd == 0 {
            return Err("没有前台窗口".into());
        }
        let command = if is_maximized(hwnd) { SW_RESTORE } else { SW_MAXIMIZE };
        unsafe {
            ShowWindowAsync(hwnd as HWND, command);
        }
        Ok(())
    }

    /// 切换「总在最前」。
    /// 按模式设置置顶：`on` / `off` / `toggle`。返回设置之后是不是置顶。
    pub fn set_topmost(hwnd: isize, mode: &str) -> Result<bool, String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongW, SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOSIZE,
        };
        const WS_EX_TOPMOST: i32 = 0x0000_0008;
        if hwnd == 0 {
            return Err("没有前台窗口".into());
        }
        unsafe {
            let style = GetWindowLongW(hwnd as HWND, GWL_EXSTYLE);
            let now = style & WS_EX_TOPMOST != 0;
            let want = match mode.trim().to_ascii_lowercase().as_str() {
                "on" => true,
                "off" => false,
                _ => !now,
            };
            SetWindowPos(
                hwnd as HWND,
                if want { HWND_TOPMOST } else { HWND_NOTOPMOST },
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            Ok(want)
        }
    }

    /// 把 `#RRGGBB` / `RRGGBB` 解析成 (r, g, b)。
    pub fn parse_hex_color(text: &str) -> Option<(u8, u8, u8)> {
        let hex = text.trim().trim_start_matches('#');
        if hex.len() != 6 {
            return None;
        }
        let value = u32::from_str_radix(hex, 16).ok()?;
        Some((
            ((value >> 16) & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            (value & 0xFF) as u8,
        ))
    }

    /// 给窗口设外边框的颜色和粗细（Windows 11 的 DWM 属性）。
    ///
    /// `color` 给 `None` = 去掉边框颜色（恢复系统默认）。粗细在 Win11 22H2+
    /// 才认，老系统上这一项会静默失败 —— 颜色能生效就够了。
    pub fn set_window_border(hwnd: isize, color: Option<(u8, u8, u8)>, thickness: u32) -> bool {
        if hwnd == 0 {
            return false;
        }
        const DWMWA_BORDER_COLOR: u32 = 34;
        const DWMWA_VISIBLE_FRAME_BORDER_THICKNESS: u32 = 37;
        const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
        unsafe {
            // COLORREF 是 0x00BBGGRR（注意和 #RRGGBB 的字节序相反）
            let value: u32 = match color {
                Some((r, g, b)) => (b as u32) << 16 | (g as u32) << 8 | r as u32,
                None => DWMWA_COLOR_NONE,
            };
            let ok = DwmSetWindowAttribute(
                hwnd as HWND,
                DWMWA_BORDER_COLOR,
                &value as *const u32 as *const _,
                4,
            ) == 0;
            if thickness > 0 {
                let width = thickness.clamp(1, 12);
                let _ = DwmSetWindowAttribute(
                    hwnd as HWND,
                    DWMWA_VISIBLE_FRAME_BORDER_THICKNESS,
                    &width as *const u32 as *const _,
                    4,
                );
            }
            ok
        }
    }

    pub fn toggle_topmost(hwnd: isize) -> Result<(), String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongW, SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE,
            SWP_NOSIZE,
        };
        const WS_EX_TOPMOST: i32 = 0x0000_0008;
        if hwnd == 0 {
            return Err("没有前台窗口".into());
        }
        unsafe {
            let style = GetWindowLongW(hwnd as HWND, GWL_EXSTYLE);
            let topmost = style & WS_EX_TOPMOST != 0;
            SetWindowPos(
                hwnd as HWND,
                if topmost { HWND_NOTOPMOST } else { HWND_TOPMOST },
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        Ok(())
    }

    /// 这个窗口所在显示器的工作区（不含任务栏）：`(x, y, 宽, 高)`，物理像素。
    /// 窗口的**真实**矩形（去掉 Win10 那圈隐形阴影边框）。
    ///
    /// `GetWindowRect` 在 Win10 上会把 DWM 的阴影算进去，四周各多出 7~8px ——
    /// 照着它摆窗口会越挪越小、对齐也差一点点。优先问 DWM 要「可见边框」。
    pub fn window_rect_exact(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
        use windows_sys::Win32::Graphics::Dwm::DwmGetWindowAttribute;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;
        if hwnd == 0 {
            return None;
        }
        const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
        unsafe {
            let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            let ok = DwmGetWindowAttribute(
                hwnd as HWND,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut rect as *mut RECT as *mut _,
                std::mem::size_of::<RECT>() as u32,
            ) == 0;
            if !ok || rect.right <= rect.left || rect.bottom <= rect.top {
                if GetWindowRect(hwnd as HWND, &mut rect) == 0 {
                    return None;
                }
            }
            Some((rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top))
        }
    }

    /// 移动 / 调整大小（`w`/`h` 传 0 表示不动那一维）。
    pub fn move_resize_window(hwnd: isize, x: i32, y: i32, width: i32, height: i32) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
        };
        if hwnd == 0 {
            return;
        }
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                std::ptr::null_mut(),
                x,
                y,
                width.max(0),
                height.max(0),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

/// 收集所有显示器的工作区（含任务栏以外区域），按系统枚举顺序。
fn monitor_areas() -> Vec<(i32, i32, i32, i32)> {
        use windows_sys::Win32::Foundation::{LPARAM, RECT, TRUE};
        use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, MONITORINFO};
        unsafe extern "system" fn each(
            monitor: *mut std::ffi::c_void,
            _dc: *mut std::ffi::c_void,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> windows_sys::Win32::Foundation::BOOL {
            let list = &mut *(data as *mut Vec<(i32, i32, i32, i32)>);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) != 0 {
                let work = info.rcWork;
                list.push((
                    work.left,
                    work.top,
                    work.right - work.left,
                    work.bottom - work.top,
                ));
            }
            TRUE
        }
        let mut list: Vec<(i32, i32, i32, i32)> = Vec::new();
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(each),
                &mut list as *mut Vec<(i32, i32, i32, i32)> as LPARAM,
            );
        }
        list
    }

    /// 显示器数量。
    pub fn monitor_count() -> usize {
        monitor_areas().len().max(1)
    }

    /// 第 `index`（从 1 开始）块屏的工作区：`(x, y, w, h)`。
    pub fn monitor_work_area(index: usize) -> Option<(i32, i32, i32, i32)> {
        monitor_areas().get(index.saturating_sub(1)).copied()
    }

    /// 结束某个进程。`force` = 强杀（不给它保存的机会）。
    pub fn kill_process(pid: u32, force: bool) -> bool {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        if pid == 0 {
            return false;
        }
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                return false;
            }
            let code = TerminateProcess(handle, if force { 1 } else { 0 });
            let _ = windows_sys::Win32::Foundation::CloseHandle(handle);
            code != 0
        }
    }

    /// 按「同一条路径的程序」收集所有相关进程（结束所有同名窗服用）。
    pub fn sibling_processes(path: &str) -> Vec<u32> {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId,
        };
        let mut pids: Vec<u32> = Vec::new();
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> windows_sys::Win32::Foundation::BOOL {
            let (path, pids) = &mut *(data as *mut (String, Vec<u32>));
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == 0 || pids.contains(&pid) {
                return 1;
            }
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return 1;
            }
            let mut buffer = vec![0u16; 1024];
            let mut size = buffer.len() as u32;
            if QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) != 0 {
                let text = String::from_utf16_lossy(&buffer[..size as usize]);
                if text.eq_ignore_ascii_case(path) {
                    pids.push(pid);
                }
            }
            let _ = windows_sys::Win32::Foundation::CloseHandle(handle);
            1
        }
        let mut data = (path.to_string(), pids);
        unsafe {
            EnumWindows(Some(each), &mut data as *mut (String, Vec<u32>) as LPARAM);
        }
        pids = data.1;
        pids
    }

    /// 用资源管理器打开某个文件所在的文件夹并选中它。
    pub fn open_file_location(path: &str) {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide = |text: &str| -> Vec<u16> {
            text.encode_utf16().chain(std::iter::once(0)).collect()
        };
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                wide("open").as_ptr(),
                wide("explorer.exe").as_ptr(),
                wide(&format!("/select,\"{path}\"")).as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }

    pub fn window_work_area(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        };
        if hwnd == 0 {
            return None;
        }
        unsafe {
            let monitor = MonitorFromWindow(hwnd as HWND, MONITOR_DEFAULTTONEAREST);
            if monitor.is_null() {
                return None;
            }
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return None;
            }
            let rect = info.rcWork;
            Some((
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
            ))
        }
    }

    /// 关掉显示器（只关屏，不动电源状态）。
    pub fn monitor_off() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SYSCOMMAND,
        };
        /// 显示器省电
        const SC_MONITORPOWER: usize = 0xF170;
        /// 2 = 关
        const MONITOR_OFF: usize = 2;
        let mut result: usize = 0;
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SYSCOMMAND,
                SC_MONITORPOWER,
                MONITOR_OFF as isize,
                SMTO_ABORTIFHUNG,
                200,
                &mut result,
            );
        }
    }

    /// 清空回收站（不弹确认、不显示进度、不出声）。
    pub fn empty_recycle_bin() -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::SHEmptyRecycleBinW;
        const SHERB_NOCONFIRMATION: u32 = 0x1;
        const SHERB_NOPROGRESSUI: u32 = 0x2;
        const SHERB_NOSOUND: u32 = 0x4;
        let code = unsafe {
            SHEmptyRecycleBinW(
                std::ptr::null_mut(),
                std::ptr::null(),
                SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
            )
        };
        // S_OK 或「本来就是空的」（0x8000FFFF）都算成功
        if code == 0 || code == 0x8000_FFFFu32 as i32 {
            Ok(())
        } else {
            Err(format!("清空回收站失败（0x{code:08x}）"))
        }
    }

    /// 注册表里 `.ahk` 的打开方式 —— 也就是 AutoHotkey 解释器路径。
    pub fn ahk_from_registry() -> Option<String> {
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_CLASSES_ROOT, RRF_RT_REG_SZ,
        };
        let key: Vec<u16> = "AutoHotkeyScript\\shell\\open\\command\0".encode_utf16().collect();
        let value: Vec<u16> = "\0".encode_utf16().collect();
        let mut buffer = vec![0u16; 1024];
        let mut size = (buffer.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CLASSES_ROOT,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        let text = String::from_utf16_lossy(&buffer);
        let text = text.trim_end_matches('\0').trim();
        // `"C://Program Files//AutoHotkey//v2//AutoHotkey64.exe" "%1"`
        let path = if let Some(rest) = text.strip_prefix('"') {
            rest.split('"').next()?.to_string()
        } else {
            text.split_whitespace().next()?.to_string()
        };
        (!path.is_empty()).then_some(path)
    }

    /// 现在的「输入目标」= 前台窗口 + 它里面拿着键盘焦点的那个控件。
    ///
    /// 热字符串的缓冲区跟着它走：目标一换就清空。返回值能直接比较，
    /// 调用方每次按键比一下就行（比自己拼字符串便宜得多）。
    pub fn input_target() -> (isize, isize) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO};
        let fg = foreground_window();
        if fg == 0 {
            return (0, 0);
        }
        let thread = unsafe { GetWindowThreadProcessId(fg as HWND, std::ptr::null_mut()) };
        if thread == 0 {
            return (fg, fg);
        }
        unsafe {
            let mut info: GUITHREADINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
            if GetGUIThreadInfo(thread, &mut info) == 0 || info.hwndFocus.is_null() {
                return (fg, fg);
            }
            // 有的程序（自绘控件、部分浏览器）报不出焦点控件，退回前台窗口本身
            (fg, info.hwndFocus as isize)
        }
    }

    /// 输入框里的光标字符位置（`EM_GETSEL` 的起点）。
    ///
    /// 热字符串用它来「量」触发词到底在屏幕上产生了几个字符：记下打第一个键时的
    /// 光标位置，命中时再读一次，差值就是**真实**上屏的字符数 —— 不管输入法把
    /// `win` 原样上屏（3 个字）还是换成了候选词（1 个字），这个数都准。
    ///
    /// 问不出来（自绘控件、浏览器、有选中内容、目标卡死）时返回 `None`，
    /// 调用方退回按物理键推算的老办法。
    pub fn caret_index(hwnd: isize) -> Option<usize> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK,
        };
        /// `EM_GETSEL`：返回值的低字 = 起点，高字 = 终点
        const EM_GETSEL: u32 = 0x00B0;
        if hwnd == 0 {
            return None;
        }
        let mut result: usize = 0;
        let ok = unsafe {
            SendMessageTimeoutW(
                hwnd as HWND,
                EM_GETSEL,
                0,
                0,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                20,
                &mut result,
            )
        };
        if ok == 0 {
            return None;
        }
        let start = result & 0xFFFF;
        let end = (result >> 16) & 0xFFFF;
        // 有选中内容时分不清光标在头还是尾，这种时候不猜
        if start != end {
            return None;
        }
        Some(start)
    }

    /// 某个窗口属于哪个进程（用来判断「前台是不是我们自己」）。
    pub fn window_process_id(hwnd: isize) -> u32 {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd as HWND, &mut pid) };
        pid
    }

    /// 窗口类名（`Chrome_WidgetWin_1`、`TXGuiFoundation`、`Button` 这些）。
    pub fn window_class(hwnd: isize) -> String {
        if hwnd == 0 {
            return String::new();
        }
        let mut buf = [0u16; 256];
        let len = unsafe { GetClassNameW(hwnd as HWND, buf.as_mut_ptr(), buf.len() as i32) };
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }

    /// 某个窗口的现场（exe / 路径 / 标题 / 类名 + 光标下控件类名）。
    ///
    /// 光标下控件那几项和「窗口自己」无关，永远是**此刻鼠标底下**的那个 ——
    /// 条件里写 `ctrl:Button` 指的是「按快捷键时鼠标停在哪」，不是「窗口的」。
    ///
    /// `mouse` = 还要不要顺带取「鼠标底下那扇窗口」的现场（条件里写了 `mouse.`
    /// 才需要）。取一次要开进程句柄、读标题，比取活动窗口贵，所以按需才做。
    fn context_of(hwnd: isize, mouse: bool) -> crate::when::Context {
        let ctrl = window_at_cursor();
        // 光标下那个控件属于哪扇窗口（`WindowFromPoint` 给的是最里层控件，
        // 往上找到顶层才知道它是不是活动窗口里的）
        let ctrl_root = unsafe { GetAncestor(ctrl as HWND, GA_ROOT) as isize };
        let ctrl_root = if ctrl_root != 0 { ctrl_root } else { ctrl };
        // `ctrl:` 只算「活动窗口里、光标底下那个控件」：鼠标停在别的窗口上时，
        // 活动窗口根本谈不上「光标下控件」，留空（要判「不在」就写 `!ctrl:…`）
        let active_ctrl = if hwnd != 0 && ctrl_root == hwnd {
            window_class(ctrl)
        } else {
            String::new()
        };
        let mut ctx = crate::when::Context {
            exe: process_name_of_window(hwnd).unwrap_or_default(),
            path: process_path_of_window(hwnd).unwrap_or_default(),
            title: window_title(hwnd),
            class: window_class(hwnd),
            ctrl_class: active_ctrl,
            ..Default::default()
        };
        if mouse {
            // `WindowFromPoint` 给的是最里层那个控件（按钮、输入框），条件的
            // 「标题 / 类名」要的是它所属的那扇顶层窗口
            let root = unsafe { GetAncestor(ctrl as HWND, GA_ROOT) as isize };
            let target = if root != 0 { root } else { ctrl };
            ctx.mouse_exe = process_name_of_window(target).unwrap_or_default();
            ctx.mouse_path = process_path_of_window(target).unwrap_or_default();
            ctx.mouse_title = window_title(target);
            ctx.mouse_class = window_class(target);
            // 鼠标那扇窗口里、光标所在控件（最里层那个就是，不用再往上找）
            ctx.mouse_ctrl_class = window_class(ctrl);
        }
        ctx
    }

    /// 某个**指定**窗口的现场（「十字」拖过去取窗口信息用）。
    ///
    /// 和 [`preview_context`] 的区别：那个是猜的（前台 / 最近一个别人的窗口），
    /// 这个是用户在屏幕上亲手点中的那一个 —— 目标窗口哪怕藏在后面也能取。
    pub fn context_of_window(hwnd: isize) -> crate::when::Context {
        if hwnd == 0 {
            return crate::when::Context::default();
        }
        // `WindowFromPoint` 给的是最里层那个控件（按钮、输入框），
        // 条件里要的「标题 / 类名」是它所属的那扇窗口 —— 往上找到顶层
        let root = unsafe { GetAncestor(hwnd as HWND, GA_ROOT) as isize };
        context_of(if root != 0 { root } else { hwnd }, true)
    }

    /// 判断「生效条件」那一刻的前台现场。
    ///
    /// 全是不带等待的查询（`GetForegroundWindow` / `QueryFullProcessImageNameW` /
    /// `GetClassNameW`），几十微秒就回来 —— 因为它是在**钩子**里同步调的，
    /// 慢一点点，用户按下的每一个键都要跟着等。
    ///
    /// `mouse` 由调用方按条件决定（`Conditions::needs_mouse()`）：没写 `mouse.`
    /// 就不去碰鼠标窗口，省掉一次开进程句柄 + 读标题。
    pub fn input_context(mouse: bool) -> crate::when::Context {
        context_of(foreground_window(), mouse)
    }

    /// 配置界面上「现在的前台是什么」用的现场。
    ///
    /// 和 [`input_context`] 的区别：用户正盯着我们自己的窗口写字，前台当然是
    /// 我们自己 —— 那没有意义。这时候退回到「最近一个属于别人的前台窗口」
    /// （后台每 150ms 记一次，见 [`watch_foreground`]），也就是用户刚才待着的
    /// 那个程序。配置生效条件时，**那个**才是他脑子里想的「前台」。
    pub fn preview_context() -> crate::when::Context {
        let fg = foreground_window();
        let ours = std::process::id();
        if fg != 0 && window_process_id(fg) != ours {
            return context_of(fg, true);
        }
        let remembered = last_foreign_foreground();
        if remembered != 0 {
            return context_of(remembered, true);
        }
        // 一个「别人的」窗口都没见过（刚启动就开了配置窗口）：宁可空着，
        // 也不要把我们自己的窗口当成前台 —— 那会让用户以为条件写错了
        crate::when::Context::default()
    }

    /// 某个窗口所属进程的 exe 完整路径（拿不到就是 `None`）。
    pub fn process_path_of_window(hwnd: isize) -> Option<String> {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        if hwnd == 0 {
            return None;
        }
        let pid = window_process_id(hwnd);
        if pid == 0 {
            return None;
        }
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut buffer = [0u16; 1024];
            let mut length = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length);
            let _ = CloseHandle(process);
            if ok == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buffer[..length as usize]))
        }
    }

    /// 某个窗口所属进程的 exe 文件名（小写，比如 `dnf.exe`）。
    ///
    /// 「禁用应用」名单按它比对：用户填的是 exe 名，不是完整路径 ——
    /// 换个安装目录也照样认得出来。
    pub fn process_name_of_window(hwnd: isize) -> Option<String> {
        let path = process_path_of_window(hwnd)?;
        path.rsplit(['\\', '/'])
            .next()
            .filter(|name| !name.is_empty())
            .map(|name| name.to_lowercase())
    }

    /// 前台窗口所属进程的 exe 名（小写）。
    pub fn foreground_process_name() -> Option<String> {
        let hwnd = unsafe { GetForegroundWindow() };
        process_name_of_window(hwnd as isize)
    }


    /// 屏幕坐标 (x, y) 底下的窗口是不是指定的那一扇（拖拽落点判断）。
    /// 结束掉**同名的其它实例**（同一个 exe），返回结束了几个。
    ///
    /// 单实例用：用户再点一次 exe / 快捷方式时，把老的那个关掉再接管 ——
    /// 否则托盘里会挂两个图标，热键也会互相抢。
    pub fn kill_other_instances() -> usize {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };

        let Ok(me) = std::env::current_exe() else {
            return 0;
        };
        let Some(name) = me.file_name().map(|name| name.to_string_lossy().to_string()) else {
            return 0;
        };
        let ours = std::process::id();
        let mut killed = 0usize;
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return 0;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snapshot, &mut entry);
            while ok != 0 {
                if entry.th32ProcessID != ours {
                    let end = entry
                        .szExeFile
                        .iter()
                        .position(|unit| *unit == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let other = String::from_utf16_lossy(&entry.szExeFile[..end]);
                    if other.eq_ignore_ascii_case(&name) {
                        let handle = OpenProcess(PROCESS_TERMINATE, 0, entry.th32ProcessID);
                        if !handle.is_null() {
                            if TerminateProcess(handle, 0) != 0 {
                                killed += 1;
                            }
                            let _ = CloseHandle(handle);
                        }
                    }
                }
                ok = Process32NextW(snapshot, &mut entry);
            }
            let _ = CloseHandle(snapshot);
        }
        killed
    }

    // ---- WinHTTP：检查更新 ----
    //
    // 不引第三方 HTTP 库：WinHTTP 是系统自带的，编译进 exe 的体积几乎为零，
    // 也不用担心又多一棵依赖树（这个项目对体积敏感）。

    /// 发一个 GET，返回 (状态码, 正文)。失败给一句能看懂的话。
    pub fn http_get(url: &str, timeout_ms: u32) -> Result<(u16, String), String> {
        let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err("只支持 http/https".to_string());
        };
        let (host, path) = match rest.split_once('/') {
            Some((host, path)) => (host, format!("/{path}")),
            None => (rest, "/".to_string()),
        };
        if host.is_empty() {
            return Err("URL 里没有主机名".to_string());
        }

        fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().chain(std::iter::once(0)).collect()
        }
        let agent_wide = wide("RunAny_RS/update-check");
        let host_wide = wide(host);
        let verb_wide = wide("GET");
        let path_wide = wide(&path);
        let headers_wide =
            wide("User-Agent: RunAny_RS\r\nAccept: application/vnd.github+json\r\n");

        unsafe {
            // 4 = WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY：走系统代理设置
            let session = WinHttpOpen(
                agent_wide.as_ptr(),
                4,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
            if session.is_null() {
                return Err("WinHTTP 初始化失败".to_string());
            }
            let timeout = timeout_ms.min(30_000) as i32;
            let _ = WinHttpSetTimeouts(session, timeout, timeout, timeout, timeout);

            let connect = WinHttpConnect(
                session,
                host_wide.as_ptr(),
                if secure { 443 } else { 80 },
                0,
            );
            if connect.is_null() {
                let _ = WinHttpCloseHandle(session);
                return Err("连不上 GitHub（网络不通？）".to_string());
            }

            let request = WinHttpOpenRequest(
                connect,
                verb_wide.as_ptr(),
                path_wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                if secure { 0x0080_0000 } else { 0 }, // WINHTTP_FLAG_SECURE
            );
            if request.is_null() {
                let _ = WinHttpCloseHandle(connect);
                let _ = WinHttpCloseHandle(session);
                return Err("建不起请求".to_string());
            }

            let sent = WinHttpSendRequest(
                request,
                headers_wide.as_ptr(),
                u32::MAX, // -1：头部按 NUL 结尾算
                std::ptr::null_mut(),
                0,
                0,
                0,
            );
            let ok = sent != 0 && WinHttpReceiveResponse(request, std::ptr::null_mut()) != 0;

            let mut status: u32 = 0;
            let mut len = std::mem::size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                request,
                19 | 0x2000_0000, // WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER
                std::ptr::null(),
                &mut status as *mut u32 as *mut _,
                &mut len,
                std::ptr::null_mut(),
            );

            let mut body: Vec<u8> = Vec::new();
            if ok {
                loop {
                    let mut buffer = vec![0u8; 8192];
                    let mut read: u32 = 0;
                    let code = WinHttpReadData(
                        request,
                        buffer.as_mut_ptr() as *mut _,
                        buffer.len() as u32,
                        &mut read,
                    );
                    if code == 0 || read == 0 {
                        break;
                    }
                    body.extend_from_slice(&buffer[..read as usize]);
                    if body.len() > 2 * 1024 * 1024 {
                        break; // 防止意外的超大响应
                    }
                }
            }

            let _ = WinHttpCloseHandle(request);
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);

            if !ok {
                return Err("请求失败（网络不通或被拦）".to_string());
            }
            Ok((
                status as u16,
                String::from_utf8_lossy(&body).to_string(),
            ))
        }
    }

    /// 光标位置（**物理**像素）。摆窗口用这个，别用逻辑坐标再乘回去 ——
    /// 175% 这种小数缩放下换算一次就够呛，换两次必错位。
    pub fn cursor_position_physical() -> Option<(i32, i32)> {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut point = POINT { x: 0, y: 0 };
        let ok = unsafe { GetCursorPos(&mut point) };
        (ok != 0).then_some((point.x, point.y))
    }

    /// 主屏尺寸（**物理**像素）。
    pub fn screen_size_physical() -> (i32, i32) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
        };
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
    }

    /// 这扇窗口**自己**的缩放比（96 DPI = 1.0）。
    ///
    /// 多显示器时各块屏的缩放可能不一样；提示窗口是在光标那块屏上建的，
    /// 拿主窗口的缩放去算尺寸就会错位、裁字。
    pub fn window_scale(hwnd: isize) -> Option<f32> {
        if hwnd == 0 {
            return None;
        }
        let dpi = unsafe { dpi_for_window(hwnd as HWND) };
        (dpi > 0).then(|| dpi as f32 / 96.0)
    }

    /// 开机以来经过的毫秒数（AHK 的 `A_TickCount` 同一个口径）。
    pub fn system_uptime_ms() -> u64 {
        use windows_sys::Win32::System::SystemInformation::GetTickCount64;
        unsafe { GetTickCount64() }
    }

    /// 当前进程是不是管理员权限（AHK 的 `A_IsAdmin`）。
    /// 光标底下那个输入框所属的进程是不是**提权**运行的。
    ///
    /// UIPI：非提权进程注入的按键/剪贴板操作会被系统直接丢弃，而且**不报错**。
    /// 表现就是「热字符串命中了、提示也弹了，但目标程序一个字都没变」——
    /// 目标程序以管理员运行时必现（记事本、某些编辑器、游戏启动器都是）。
    pub fn target_is_elevated() -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
        };
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let (top, _) = crate::win32::input_target();
        if top == 0 {
            return false;
        }
        unsafe {
            let mut pid = 0u32;
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                top as HWND,
                &mut pid,
            );
            if pid == 0 || pid == GetCurrentProcessId() {
                return false;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                // 打不开 = 权限比我们高，基本可以认定是提权的
                return true;
            }
            let mut token: HANDLE = std::ptr::null_mut();
            let mut elevated = false;
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) != 0 {
                let mut info = TOKEN_ELEVATION { TokenIsElevated: 0 };
                let mut size = 0u32;
                if GetTokenInformation(
                    token,
                    TokenElevation,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                    &mut size,
                ) != 0
                {
                    elevated = info.TokenIsElevated != 0;
                }
                let _ = CloseHandle(token);
            }
            let _ = CloseHandle(process);
            elevated
        }
    }

    pub fn is_elevated() -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut _,
                size,
                &mut size,
            );
            let _ = CloseHandle(token);
            ok != 0 && elevation.TokenIsElevated != 0
        }
    }

    /// 光标底下是哪个窗口（拖动拾取「禁用应用」时用；松手那一刻问一次）。
    /// 按「标题包含 / 类名 / 进程名」找一扇可见窗口（找不到给 None）。
    pub fn find_window(kind: &str, needle: &str) -> Option<isize> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowTextW, IsWindowVisible,
        };
        unsafe extern "system" fn each(
            hwnd: HWND,
            data: LPARAM,
        ) -> windows_sys::Win32::Foundation::BOOL {
            let (kind, needle, found) = &mut *(data as *mut (String, String, isize));
            if IsWindowVisible(hwnd) == 0 {
                return 1;
            }
            let matched = if kind == "title" {
                let mut buffer = vec![0u16; 512];
                let len = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
                let text = String::from_utf16_lossy(&buffer[..len.max(0) as usize]);
                !text.is_empty() && text.to_lowercase().contains(needle.as_str())
            } else if kind == "class" {
                let mut buffer = vec![0u16; 256];
                let len = GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
                let text = String::from_utf16_lossy(&buffer[..len.max(0) as usize]);
                text.to_lowercase().contains(needle.as_str())
            } else {
                process_path_of_window(hwnd as isize)
                    .map(|path| path.to_lowercase().contains(needle.as_str()))
                    .unwrap_or(false)
            };
            if matched {
                *found = hwnd as isize;
                return 0; // 找到就停
            }
            1
        }
        let needle_lower = needle.to_lowercase();
        let mut data = (kind.to_string(), needle_lower, 0isize);
        unsafe {
            EnumWindows(
                Some(each),
                &mut data as *mut (String, String, isize) as LPARAM,
            );
        }
        (data.2 != 0).then_some(data.2)
    }

    pub fn window_at_cursor() -> isize {
        let (x, y) = cursor_position();
        unsafe { WindowFromPoint(POINT { x, y }) as isize }
    }

    /// 「抓选中内容」该往哪个窗口发复制键。
    ///
    /// 平时就是前台窗口。但用**鼠标手势**触发快捷键时，手势工具往往自带一个
    /// 「分层 / 不抢激活 / 工具窗口」的画线浮层，热键按下的那一刻它就是前台 ——
    /// 往它身上发 Ctrl+C 什么也复制不到，用户看到的就是「手势触发抓不到，
    /// 手动按快捷键就正常」。
    ///
    /// 这种浮层一律当「不是真窗口」，改用**光标底下那个窗口**（`WindowFromPoint`
    /// 会跳过 `WS_EX_TRANSPARENT` 的穿透层），再取它所属的顶层窗口。
    pub fn selection_target() -> isize {
        const WS_EX_TRANSPARENT: isize = 0x0000_0020;
        const WS_EX_NOACTIVATE: isize = 0x0800_0000;
        let fg = foreground_window();
        let looks_like_overlay = |hwnd: isize| -> bool {
            if hwnd == 0 {
                return false;
            }
            let ex = unsafe { GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE) } as isize;
            let layered = ex & (WS_EX_LAYERED as isize) != 0;
            let transparent = ex & WS_EX_TRANSPARENT != 0;
            let no_activate = ex & WS_EX_NOACTIVATE != 0;
            (layered && transparent) || no_activate
        };
        if fg != 0 && !looks_like_overlay(fg) {
            return fg;
        }
        let under = window_at_cursor();
        if under == 0 {
            return fg;
        }
        let root = unsafe { GetAncestor(under as HWND, GA_ROOT) as isize };
        let candidate = if root != 0 { root } else { under };
        // 光标底下是桌面 / 本程序自己：那就还是老老实实用前台窗口
        if candidate == 0 || is_desktop_window(candidate) || window_is_self(candidate) {
            return fg;
        }
        candidate
    }

    /// 这个窗口是不是本程序自己的（设置窗口 / 编辑窗口 / 弹出菜单 / 提示卡片）。
    pub fn window_is_self(hwnd: isize) -> bool {
        hwnd != 0 && window_process_id(hwnd) == std::process::id()
    }

    /// 光标底下是不是本程序自己的窗口。
    ///
    /// 悬停提示要在「鼠标移出菜单」时自己收起来，而 GPUI 的 `on_hover(false)`
    /// 在鼠标直接离开窗口时并不可靠 —— 这里用系统给的「光标下是哪个窗口」兜底。
    pub fn cursor_over_self() -> bool {
        let (x, y) = cursor_position();
        unsafe {
            let hwnd = WindowFromPoint(POINT { x, y });
            window_is_self(hwnd as isize)
        }
    }

    /// 本地时间（占位符 `{date}` / `{time}` 用；不引入 chrono，直接问系统）。
    pub fn local_time() -> super::LocalTime {
        let mut time = SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        unsafe { GetLocalTime(&mut time) };
        super::LocalTime {
            year: time.wYear,
            month: time.wMonth,
            day: time.wDay,
            hour: time.wHour,
            minute: time.wMinute,
            second: time.wSecond,
        }
    }
}

#[cfg(windows)]
pub use imp::{
    clipboard_has_files, clipboard_has_image, clipboard_image_size,
    clipboard_sequence, cursor_position, font_families, foreground_window, hide_window,
    icon_pixels, is_desktop_window, is_maximized, left_button_down, local_time, move_window, native_round_corners,
    pick_image_file, pick_program_file, pick_ini_file, pick_script_file, decode_ansi, raise_above, raise_window, read_clipboard_files, read_clipboard_text,
    bring_to_front, restore_clipboard, client_size,
    restore_window, screen_color_at_cursor, send_keys, send_text, set_foreground,
    set_rounded_corners, set_window_opacity, show_window, snapshot_clipboard, start_system_drag,
    wait_foreground, wait_modifiers_released, release_modifiers, win_key_down,
    watch_foreground, window_process_id,
    process_name_of_window, input_context, preview_context, context_of_window,
    foreground_process_name, window_at_cursor, selection_target,
    window_rect_exact, move_resize_window, monitor_count, monitor_work_area, kill_process, find_window,
    process_path_of_window,
    sibling_processes, open_file_location,
    system_uptime_ms, is_elevated, target_is_elevated, kill_other_instances, http_get,
    window_scale,
    install_input_hooks, set_key_gate, set_mouse_gate, numlock_on, press_numlock,
    modifier_state, click_tick, send_backspaces, send_virtual_key,
    ime_composing, caret_rect,
    window_is_self, cursor_over_self, input_target, caret_index,
    set_window_bounds, minimize_window, toggle_maximize, toggle_topmost, window_work_area,
    process_ids, top_windows, activate_or_minimize, close_window, launch_as, split_command,
    monitor_off, empty_recycle_bin, ahk_from_registry,
    make_click_through, set_click_through,
    resize_window, fit_client_size, ime_composition_len, send_text_with_keys, paste_text,
    cursor_position_physical, screen_size_physical, set_topmost, set_window_border,
    parse_hex_color, clipboard_snapshot, clipboard_restore,
    work_area_insets, window_rect, write_clipboard_text, zed_window_handles,
    trim_working_set,
    cancel_ime_composition, cancel_ime_composition_in, place_ime_in,
};

/// 写法 → 虚拟键码（`Num7` / `PgUp` / `F13` 这些都在里面）。
#[cfg(windows)]
pub(crate) use imp::virtual_key;

#[cfg(not(windows))]
pub(crate) fn virtual_key(_name: &str) -> Option<u16> {
    None
}

/// 非客户区命中代码（和 winuser.h 里的 HTxxx 一致）。
pub const HT_CAPTION: i32 = 2;

// ---- 热字符串那套（其它平台全是空实现）----

#[cfg(not(windows))]
pub fn install_input_hooks(
    _key: Box<dyn Fn(RawKey) -> bool + Send>,
    _pointer: Box<dyn Fn() + Send>,
) -> bool {
    false
}

#[cfg(not(windows))]
pub fn ensure_input_hooks() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_key_gate(_gate: Box<dyn Fn(RawKey) -> bool + Send + Sync>) -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_mouse_gate(_gate: Box<dyn Fn(RawMouse) -> bool + Send + Sync>) -> bool {
    false
}

#[cfg(not(windows))]
pub fn add_key_sink(_sink: Box<dyn Fn(RawKey) -> bool + Send + Sync>) -> bool {
    false
}

#[cfg(not(windows))]
pub fn add_pointer_sink(_sink: Box<dyn Fn() + Send + Sync>) -> bool {
    false
}

#[cfg(not(windows))]
pub fn numlock_on() -> bool {
    false
}

#[cfg(not(windows))]
pub fn modifier_state() -> [bool; 8] {
    [false; 8]
}

#[cfg(not(windows))]
pub fn press_numlock() {}

#[cfg(not(windows))]
pub fn click_tick() -> u64 {
    0
}

#[cfg(not(windows))]
pub fn send_backspaces(_count: usize) {}

#[cfg(not(windows))]
pub fn send_virtual_key(_vk: u16) {}

#[cfg(not(windows))]
pub fn selection_target() -> isize {
    0
}

#[cfg(not(windows))]
pub fn ime_composing() -> bool {
    false
}

#[cfg(not(windows))]
pub fn pick_ini_file() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn caret_index(_hwnd: isize) -> Option<usize> {
    None
}

#[cfg(not(windows))]
pub fn set_window_bounds(_hwnd: isize, _x: i32, _y: i32, _width: i32, _height: i32) {}

#[cfg(not(windows))]
pub fn minimize_window(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn process_ids(_exe: &str) -> Vec<u32> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn top_windows(_pids: &[u32]) -> Vec<isize> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn activate_or_minimize(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn close_window(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn launch_as(
    _command: &str,
    _as_admin: bool,
    _dir: Option<&str>,
    _window: &str,
) -> Result<(), String> {
    Err("这个平台上还不支持".to_string())
}

#[cfg(not(windows))]
pub fn split_command(_command: &str) -> (String, String) {
    (String::new(), String::new())
}

#[cfg(not(windows))]
pub fn toggle_maximize(_hwnd: isize) -> Result<(), String> {
    Err("仅 Windows".into())
}

#[cfg(not(windows))]
pub fn toggle_topmost(_hwnd: isize) -> Result<(), String> {
    Err("仅 Windows".into())
}

#[cfg(not(windows))]
pub fn window_work_area(_hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    None
}

#[cfg(not(windows))]
pub fn monitor_off() {}

#[cfg(not(windows))]
pub fn empty_recycle_bin() -> Result<(), String> {
    Err("仅 Windows".into())
}

#[cfg(not(windows))]
pub fn ahk_from_registry() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn decode_ansi(_bytes: &[u8]) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn window_is_self(_hwnd: isize) -> bool {
    false
}

#[cfg(not(windows))]
pub fn cursor_over_self() -> bool {
    false
}

#[cfg(not(windows))]
pub fn input_target() -> (isize, isize) {
    (0, 0)
}

#[cfg(not(windows))]
pub fn caret_rect() -> Option<(i32, i32, i32, i32)> {
    None
}

#[cfg(not(windows))]
pub fn make_click_through(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn set_click_through(_hwnd: isize, _on: bool) {}

#[cfg(not(windows))]
pub fn resize_window(_hwnd: isize, _width: f32, _height: f32, _scale: f32) {}

#[cfg(not(windows))]
pub fn window_rect(_hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    None
}

#[cfg(not(windows))]
pub fn client_size(_hwnd: isize) -> Option<(i32, i32)> {
    None
}

#[cfg(not(windows))]
pub fn zed_window_handles() -> Vec<isize> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn font_families() -> Vec<String> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn raise_window(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn raise_above(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn bring_to_front(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn pick_image_file() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn pick_script_file() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn pick_program_file() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn set_rounded_corners(_hwnd: isize, _scale: f32) {}

#[cfg(not(windows))]
pub fn set_rounded_corners_with(_hwnd: isize, _scale: f32, _radius: f32) {}

#[cfg(not(windows))]
pub fn start_system_drag(_hwnd: isize, _area: i32) {}

#[cfg(not(windows))]
pub fn restore_window(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn native_round_corners(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn is_maximized(_hwnd: isize) -> bool {
    false
}

#[cfg(not(windows))]
pub fn move_window(_hwnd: isize, _x: i32, _y: i32) {}

#[cfg(not(windows))]
pub fn send_keys(_input: &str) -> Result<(), String> {
    Err("这个平台上还不支持发送按键".to_string())
}

#[cfg(not(windows))]
pub fn send_text(_text: &str) -> Result<(), String> {
    Err("这个平台上还不支持发送文本".to_string())
}

#[cfg(not(windows))]
pub fn foreground_window() -> isize {
    0
}

#[cfg(not(windows))]
pub fn capture_target() -> isize {
    0
}

#[cfg(not(windows))]
pub fn watch_foreground() {}

#[cfg(not(windows))]
pub fn is_desktop_window(_hwnd: isize) -> bool {
    false
}

#[cfg(not(windows))]
pub fn window_title(_hwnd: isize) -> String {
    String::new()
}

#[cfg(not(windows))]
pub fn wait_foreground(_hwnd: isize, _max_ms: u64) -> bool {
    false
}

#[cfg(not(windows))]
pub fn left_button_down() -> bool {
    false
}

#[cfg(not(windows))]
pub fn screen_color_at_cursor() -> Option<u32> {
    None
}

#[cfg(not(windows))]
pub fn set_foreground(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn hide_window(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn trim_working_set() {}

#[cfg(not(windows))]
pub fn set_window_opacity(_hwnd: isize, _opacity: f32) {}

#[cfg(not(windows))]
pub fn icon_pixels(_path: &str, _size: u32) -> Option<(u32, u32, Vec<u8>)> {
    None
}

#[cfg(not(windows))]
pub fn show_window(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn work_area_insets() -> Option<(f32, f32, f32, f32)> {
    None
}

#[cfg(not(windows))]
pub fn clipboard_sequence() -> u32 {
    0
}

#[cfg(not(windows))]
pub fn cursor_position() -> (i32, i32) {
    (0, 0)
}

#[cfg(not(windows))]
pub fn snapshot_clipboard() -> Option<ClipboardBackup> {
    None
}

#[cfg(not(windows))]
pub fn restore_clipboard(_backup: &ClipboardBackup) {}

#[cfg(not(windows))]
pub fn read_clipboard_text() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn clipboard_has_files() -> bool {
    false
}

#[cfg(not(windows))]
pub fn clipboard_has_image() -> bool {
    false
}

#[cfg(not(windows))]
pub fn read_clipboard_files() -> Vec<String> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn clipboard_image_size() -> Option<(u32, u32)> {
    None
}

#[cfg(not(windows))]
pub fn write_clipboard_text(_text: &str) {}

#[cfg(not(windows))]
pub fn wait_modifiers_released(_max_ms: u64) -> bool {
    true
}

#[cfg(not(windows))]
pub fn release_modifiers() {}

#[cfg(not(windows))]
pub fn win_key_down() -> bool {
    false
}

#[cfg(not(windows))]
pub fn window_process_id(_hwnd: isize) -> u32 {
    0
}

#[cfg(not(windows))]
pub fn process_name_of_window(_hwnd: isize) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn process_path_of_window(_hwnd: isize) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn window_class(_hwnd: isize) -> String {
    String::new()
}

/// 非 Windows 上没有「前台窗口」这回事，条件一律读不到（写空条件仍然等于「到处生效」）。
#[cfg(not(windows))]
pub fn input_context(_mouse: bool) -> crate::when::Context {
    crate::when::Context::default()
}

#[cfg(not(windows))]
pub fn preview_context() -> crate::when::Context {
    crate::when::Context::default()
}

#[cfg(not(windows))]
pub fn context_of_window(_hwnd: isize) -> crate::when::Context {
    crate::when::Context::default()
}

#[cfg(not(windows))]
pub fn foreground_process_name() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn window_at_cursor() -> isize {
    0
}

#[cfg(not(windows))]
pub fn cursor_position() -> (i32, i32) {
    (0, 0)
}



#[cfg(not(windows))]
pub fn system_uptime_ms() -> u64 {
    0
}

#[cfg(not(windows))]
pub fn kill_other_instances() -> usize {
    0
}

#[cfg(not(windows))]
pub fn http_get(_url: &str, _timeout_ms: u32) -> Result<(u16, String), String> {
    Err("只有 Windows 支持".to_string())
}

#[cfg(not(windows))]
pub fn window_scale(_hwnd: isize) -> Option<f32> {
    None
}

#[cfg(not(windows))]
pub fn fit_client_size(_hwnd: isize, _width: f32, _height: f32, _scale: f32) {}

#[cfg(not(windows))]
pub fn ime_composition_len(_hwnd: isize) -> usize {
    0
}

#[cfg(not(windows))]
pub fn send_text_with_keys(text: &str) -> Result<(), String> {
    Err(format!("只有 Windows 支持：{text}"))
}

#[cfg(not(windows))]
pub fn paste_text(_text: &str, _wait_ms: u64) -> Result<(), String> {
    Err("只有 Windows 支持".to_string())
}

#[cfg(not(windows))]
pub fn cursor_position_physical() -> Option<(i32, i32)> {
    None
}

#[cfg(not(windows))]
pub fn screen_size_physical() -> (i32, i32) {
    (1920, 1080)
}

#[cfg(not(windows))]
pub fn target_is_elevated() -> bool {
    false
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}


/// 非 Windows 平台用 SystemTime 折算一个「够用」的 UTC 时间。
#[cfg(not(windows))]
pub fn local_time() -> LocalTime {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = seconds / 86_400;
    let rest = seconds % 86_400;
    // 1970-01-01 起按 400 年一个周期折算民用日期
    let (year, month, day) = civil_from_days(days as i64);
    LocalTime {
        year: year as u16,
        month: month as u16,
        day: day as u16,
        hour: (rest / 3600) as u16,
        minute: ((rest % 3600) / 60) as u16,
        second: (rest % 60) as u16,
    }
}

/// 天数（1970-01-01 起）→ (年, 月, 日)。Howard Hinnant 的 `civil_from_days`。
#[cfg(not(windows))]
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m, d)
}

#[cfg(all(test, windows))]
mod tests {
    use super::imp::virtual_key;

    #[test]
    fn key_names_map_to_virtual_keys() {
        assert_eq!(virtual_key("a"), Some(0x41));
        assert_eq!(virtual_key("5"), Some(0x35));
        assert_eq!(virtual_key("F12"), Some(0x7B));
        assert_eq!(virtual_key("enter"), Some(0x0D));
        assert_eq!(virtual_key("esc"), Some(0x1B));
        assert_eq!(virtual_key("-"), Some(0xBD));
        assert_eq!(virtual_key("Ctrl"), None);
    }

    /// 造一份 `CF_HDROP`：20 字节头 + 双 NUL 结尾的路径串。
    fn dropfiles(paths: &[&str], wide: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&20u32.to_le_bytes()); // pFiles
        bytes.extend_from_slice(&0i32.to_le_bytes()); // pt.x
        bytes.extend_from_slice(&0i32.to_le_bytes()); // pt.y
        bytes.extend_from_slice(&0i32.to_le_bytes()); // fNC
        bytes.extend_from_slice(&(wide as i32).to_le_bytes()); // fWide
        for path in paths {
            if wide {
                for unit in path.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                }
                bytes.extend_from_slice(&0u16.to_le_bytes());
            } else {
                bytes.extend_from_slice(path.as_bytes());
                bytes.push(0);
            }
        }
        if wide {
            bytes.extend_from_slice(&0u16.to_le_bytes());
        } else {
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn dropfiles_are_parsed_in_both_encodings() {
        assert_eq!(
            super::parse_dropfiles(&dropfiles(&["C:\\a\\一.txt", "D:\\二.png"], true)),
            vec!["C:\\a\\一.txt".to_string(), "D:\\二.png".to_string()]
        );
        assert_eq!(
            super::parse_dropfiles(&dropfiles(&["C:\\plain.txt"], false)),
            vec!["C:\\plain.txt".to_string()]
        );
        // 结构不对的时候宁可返回空，也不要乱读
        assert!(super::parse_dropfiles(&[0, 1, 2]).is_empty());
        assert!(super::parse_dropfiles(&[0u8; 24]).is_empty());
    }

    #[test]
    fn dib_size_handles_negative_height() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&40u32.to_le_bytes()); // biSize
        bytes.extend_from_slice(&1024i32.to_le_bytes()); // biWidth
        bytes.extend_from_slice(&(-768i32).to_le_bytes()); // 自上而下
        assert_eq!(super::parse_dib_size(&bytes), Some((1024, 768)));

        // BITMAPCOREHEADER：宽高是 u16（整块头 12 字节）
        let mut core = Vec::new();
        core.extend_from_slice(&12u32.to_le_bytes());
        core.extend_from_slice(&320u16.to_le_bytes());
        core.extend_from_slice(&240u16.to_le_bytes());
        core.extend_from_slice(&1u16.to_le_bytes()); // bcPlanes
        core.extend_from_slice(&24u16.to_le_bytes()); // bcBitCount
        assert_eq!(super::parse_dib_size(&core), Some((320, 240)));
        assert_eq!(super::parse_dib_size(&[0, 1]), None);
    }

    #[test]
    fn png_size_comes_from_the_ihdr_chunk() {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&1920u32.to_be_bytes());
        bytes.extend_from_slice(&1080u32.to_be_bytes());
        assert_eq!(super::parse_png_size(&bytes), Some((1920, 1080)));
        assert_eq!(super::parse_png_size(b"not a png at all, really"), None);
    }

    #[test]
    #[ignore = "需要手动往剪贴板里放东西"]
    fn probe_clipboard() {
        println!("formats: {:?}", super::imp::clipboard_formats());
        println!("has_files: {}", super::clipboard_has_files());
        println!("files: {:?}", super::read_clipboard_files());
        println!("has_image: {}", super::clipboard_has_image());
        println!("image: {:?}", super::clipboard_image_size());
        println!("text: {:?}", super::read_clipboard_text());
    }

    #[test]
    fn clipboard_text_bytes_decode() {
        let wide: Vec<u8> = "你好"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert_eq!(super::parse_wide_text(&wide), "你好");
        assert_eq!(super::parse_ansi_text(b"hello\0rest"), "hello");
        assert_eq!(super::parse_wide_text(b""), "");
    }
}
