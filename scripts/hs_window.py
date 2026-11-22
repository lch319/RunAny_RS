"""热字符串的靶子窗口：一个最小化的原生 Win32 窗口 + 多行 EDIT。

为什么不用记事本 / WinForms：
- 这台机器上的记事本是 Notepad4（第三方），注入的按键不一定进得去；
- 还有常驻工具会抢前台、劫持 Win+R；
- PowerShell 起 WinForms 在这台机器上直接报「不能对 Null 值表达式调用方法」。

原生窗口只有一个 EDIT 控件，`GetWindowTextW` 就能读内容，最可控。

用法（被 e2e_hotstring.py 调用，也可以单独跑）：
    python scripts/hs_window.py            # 起窗口，每 500ms 打印一次内容
"""

import ctypes
import sys
import threading
import time
from ctypes import wintypes

u = ctypes.windll.user32
k32 = ctypes.windll.kernel32
imm = ctypes.windll.imm32

TITLE = "HS_TEST_WINDOW"
CLASS_NAME = "HsTestWindowClass"

WS_OVERLAPPEDWINDOW = 0x00CF0000
WS_VISIBLE = 0x10000000
WS_CHILD = 0x40000000
WS_VSCROLL = 0x00200000
ES_MULTILINE = 0x0004
ES_AUTOVSCROLL = 0x0040
ES_WANTRETURN = 0x1000
SW_SHOW = 5
WM_DESTROY = 0x0002
WM_GETTEXT = 0x000D
WM_GETTEXTLENGTH = 0x000E

LRESULT = ctypes.c_ssize_t
WNDPROC = ctypes.WINFUNCTYPE(LRESULT, wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM)


class WNDCLASSW(ctypes.Structure):
    _fields_ = [
        ("style", wintypes.UINT),
        ("lpfnWndProc", WNDPROC),
        ("cbClsExtra", ctypes.c_int),
        ("cbWndExtra", ctypes.c_int),
        ("hInstance", wintypes.HINSTANCE),
        ("hIcon", wintypes.HICON),
        ("hCursor", wintypes.HANDLE),
        ("hbrBackground", wintypes.HBRUSH),
        ("lpszMenuName", wintypes.LPCWSTR),
        ("lpszClassName", wintypes.LPCWSTR),
    ]


class MSG(ctypes.Structure):
    _fields_ = [
        ("hwnd", wintypes.HWND),
        ("message", wintypes.UINT),
        ("wParam", wintypes.WPARAM),
        ("lParam", wintypes.LPARAM),
        ("time", wintypes.DWORD),
        ("pt", wintypes.POINT),
    ]


u.DefWindowProcW.restype = LRESULT
u.DefWindowProcW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
u.CreateWindowExW.restype = wintypes.HWND
u.SendMessageW.restype = LRESULT

_keep = []  # 别让回调被 GC 掉


def start(detach=True):
    """建窗口 + 起消息循环（跑在新线程里），返回 (hwnd, edit)。

    `detach=False` 时保留输入法（`probe_ime.py` 要测「输入法开着但切到英文」那种情况）。
    """
    ready = threading.Event()
    holder = {}

    @WNDPROC
    def proc(hwnd, message, wparam, lparam):
        if message == WM_DESTROY:
            u.PostQuitMessage(0)
            return 0
        return u.DefWindowProcW(hwnd, message, wparam, lparam)

    _keep.append(proc)

    def run():
        instance = k32.GetModuleHandleW(None)
        wc = WNDCLASSW()
        wc.style = 0
        wc.lpfnWndProc = proc
        wc.cbClsExtra = 0
        wc.cbWndExtra = 0
        wc.hInstance = instance
        wc.hIcon = None
        wc.hCursor = u.LoadCursorW(None, 32512)  # IDC_ARROW
        wc.hbrBackground = 16  # COLOR_WINDOW + 1
        wc.lpszMenuName = None
        wc.lpszClassName = CLASS_NAME
        u.RegisterClassW(ctypes.byref(wc))

        hwnd = u.CreateWindowExW(
            0,
            CLASS_NAME,
            TITLE,
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            200,
            200,
            560,
            260,
            None,
            None,
            instance,
            None,
        )
        edit = u.CreateWindowExW(
            0,
            "EDIT",
            "",
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN,
            10,
            10,
            520,
            200,
            hwnd,
            None,
            instance,
            None,
        )
        u.ShowWindow(hwnd, SW_SHOW)
        u.UpdateWindow(hwnd)
        # 焦点必须在建窗口的这个线程里设，否则 SetFocus 会失败（跨线程不允许），
        # 敲进去的字符就全落到对话框本体上、EDIT 里一个字都没有。
        u.SetForegroundWindow(hwnd)
        u.SetFocus(edit)
        # 这台机器默认开着中文输入法：字母按下会变成 VK_PROCESSKEY(0xE5) 被输入法
        # 组字吃掉，EDIT 里留不下字。把输入法从控件上摘掉，按键才会原样进 EDIT。
        if detach:
            detach_ime(edit)
        holder["hwnd"] = hwnd
        holder["edit"] = edit
        ready.set()

        message = MSG()
        while u.GetMessageW(ctypes.byref(message), None, 0, 0) > 0:
            u.TranslateMessage(ctypes.byref(message))
            u.DispatchMessageW(ctypes.byref(message))

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    ready.wait(5.0)
    return holder.get("hwnd"), holder.get("edit")


def text_of(edit):
    length = u.SendMessageW(edit, WM_GETTEXTLENGTH, 0, 0)
    buf = ctypes.create_unicode_buffer(int(length) + 2)
    u.SendMessageW(edit, WM_GETTEXT, length + 1, buf)
    return buf.value


def focus(hwnd, edit):
    """从头线程把焦点抢过来（跨线程 SetFocus 要先用 AttachThreadInput 挂上去）。"""
    foreground = u.GetForegroundWindow()
    other = u.GetWindowThreadProcessId(foreground, None)
    me = k32.GetCurrentThreadId()
    attached = False
    if other and other != me:
        attached = bool(u.AttachThreadInput(me, other, True))
    u.AllowSetForegroundWindow(-1)
    u.SetForegroundWindow(hwnd)
    u.SetFocus(edit)
    if attached:
        u.AttachThreadInput(me, other, False)
    return window_class(u.GetForegroundWindow()), u.GetFocus() == edit


def detach_ime(hwnd):
    """把输入法从窗口上摘掉（`ImmAssociateContext(hwnd, NULL)`），并关掉打开状态。

    不摘的话，默认中文输入法会把字母变成 `WM_KEYDOWN` + `VK_PROCESSKEY`，
    字符进组字缓冲区、`EDIT` 里什么都留不下。
    """
    try:
        imm.ImmAssociateContext(hwnd, None)
    except Exception:
        pass
    try:
        context = imm.ImmGetContext(hwnd)
        if context:
            imm.ImmSetOpenStatus(context, False)
            imm.ImmReleaseContext(hwnd, context)
    except Exception:
        pass


def window_class(hwnd):
    buf = ctypes.create_unicode_buffer(256)
    u.GetClassNameW(hwnd, buf, 256)
    return buf.value


if __name__ == "__main__":
    hwnd, edit = start()
    print("hwnd:", hex(hwnd or 0), "edit:", hex(edit or 0))
    try:
        while True:
            print(repr(text_of(edit)))
            time.sleep(0.5)
    except KeyboardInterrupt:
        sys.exit(0)
