"""把某个窗口（或整屏）截成 BMP，方便肉眼看版式。

用法：
    python scripts/shot.py                 # 整屏
    python scripts/shot.py <hwnd> out.bmp  # 指定窗口
"""
import ctypes
import sys
from ctypes import wintypes

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD),
        ("biWidth", wintypes.LONG),
        ("biHeight", wintypes.LONG),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", wintypes.LONG),
        ("biYPelsPerMeter", wintypes.LONG),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wintypes.DWORD * 3)]


def capture(x, y, w, h, path):
    screen = user32.GetDC(0)
    mem = gdi32.CreateCompatibleDC(screen)
    bmp = gdi32.CreateCompatibleBitmap(screen, w, h)
    gdi32.SelectObject(mem, bmp)
    # CAPTUREBLT 让分层窗口（我们的菜单窗口设了 alpha）也能截进来
    gdi32.BitBlt(mem, 0, 0, w, h, screen, x, y, 0x00CC0020 | 0x40000000)

    info = BITMAPINFO()
    info.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    info.bmiHeader.biWidth = w
    info.bmiHeader.biHeight = -h  # 负数 = 从上往下
    info.bmiHeader.biPlanes = 1
    info.bmiHeader.biBitCount = 32
    info.bmiHeader.biCompression = 0  # BI_RGB
    buf = ctypes.create_string_buffer(w * h * 4)
    gdi32.GetDIBits(mem, bmp, 0, h, buf, ctypes.byref(info), 0)

    header = b"BM" + (54 + len(buf)).to_bytes(4, "little") + b"\0\0\0\0" + (54).to_bytes(4, "little")
    header += (40).to_bytes(4, "little") + w.to_bytes(4, "little", signed=True)
    header += (-h).to_bytes(4, "little", signed=True) + (1).to_bytes(2, "little")
    header += (32).to_bytes(2, "little") + (0).to_bytes(4, "little")
    header += (w * h * 4).to_bytes(4, "little") + b"\0" * 16
    with open(path, "wb") as f:
        f.write(header)
        f.write(buf.raw)

    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(0, screen)


def find_window(needle):
    """在本进程里找标题含 `needle` 的可见窗口（还要最像「最前面那个」）。"""
    import subprocess

    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq RunAny_RS.exe", "/FO", "CSV", "/NH"],
        capture_output=True, text=True, encoding="gbk", errors="replace")
    try:
        pid = int(out.stdout.split(",")[1].strip('"'))
    except (IndexError, ValueError):
        return None

    found = []
    EnumProc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def cb(hwnd, _):
        owner = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value != pid or not user32.IsWindowVisible(hwnd):
            return True
        buf = ctypes.create_unicode_buffer(256)
        user32.GetWindowTextW(hwnd, buf, 256)
        if needle and needle not in buf.value:
            return True
        rect = wintypes.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        found.append((hwnd, rect.left, rect.top,
                      rect.right - rect.left, rect.bottom - rect.top))
        return True

    user32.EnumWindows(EnumProc(cb), 0)
    if not found:
        return None
    return max(found, key=lambda item: item[3])


def main():
    if len(sys.argv) >= 2 and sys.argv[1].startswith("title:"):
        # 按标题找本进程的窗口（找不到就退回整屏）
        needle = sys.argv[1][len("title:"):]
        path = sys.argv[2] if len(sys.argv) > 2 else "shot.bmp"
        target = find_window(needle)
        if target is None:
            print("没找到窗口，截整屏")
            x, y = 0, 0
            w = user32.GetSystemMetrics(78)
            h = user32.GetSystemMetrics(79)
        else:
            hwnd, x, y, w, h = target
    elif len(sys.argv) >= 2 and sys.argv[1] not in ("-", "0", "screen"):
        hwnd = int(sys.argv[1], 0)
        rect = wintypes.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        path = sys.argv[2] if len(sys.argv) > 2 else "shot.bmp"
        x, y = rect.left, rect.top
        w, h = rect.right - rect.left, rect.bottom - rect.top
    else:
        path = sys.argv[2] if len(sys.argv) > 2 else "shot.bmp"
        # 整屏
        x = user32.GetSystemMetrics(76)
        y = user32.GetSystemMetrics(77)
        w = user32.GetSystemMetrics(78)
        h = user32.GetSystemMetrics(79)
    # 把所有窗口提到前台之后再截，免得被别的程序挡着
    user32.SetForegroundWindow(user32.GetTopWindow(0))
    capture(x, y, w, h, path)
    print(f"{path} {w}x{h} @({x},{y})")


main()
