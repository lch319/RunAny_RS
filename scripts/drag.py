"""拖动设置窗口标题栏（复现「菜单开着时移动设置窗口会崩」）。

用 ctypes 直接调 user32：EnumWindows 找到本进程的可见窗口，
取最宽的那个当设置窗口，然后按住它的标题栏拖一段。
"""
import ctypes
import subprocess
import sys
import time
from ctypes import wintypes

user32 = ctypes.windll.user32

EnumProc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)


def windows_of(pid):
    found = []

    def cb(hwnd, _):
        proc = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(proc))
        if proc.value == pid and user32.IsWindowVisible(hwnd):
            rect = wintypes.RECT()
            user32.GetWindowRect(hwnd, ctypes.byref(rect))
            buf = ctypes.create_unicode_buffer(256)
            user32.GetWindowTextW(hwnd, buf, 256)
            found.append((hwnd, rect.left, rect.top,
                          rect.right - rect.left, rect.bottom - rect.top, buf.value))
        return True

    user32.EnumWindows(EnumProc(cb), 0)
    return found


def alive():
    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq RunAny_RS.exe", "/NH"],
        capture_output=True, text=True, encoding="gbk", errors="replace")
    return "RunAny_RS" in out.stdout


pid = int(sys.argv[1]) if len(sys.argv) > 1 else None
if pid is None:
    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq RunAny_RS.exe", "/FO", "CSV", "/NH"],
        capture_output=True, text=True, encoding="gbk", errors="replace")
    pid = int(out.stdout.split(",")[1].strip('"'))

wins = windows_of(pid)
print(f"进程 {pid}，可见窗口 {len(wins)} 个：")
for w in wins:
    print(f"  h={w[0]} rect=({w[1]},{w[2]}) {w[3]}x{w[4]} title='{w[5]}'")

if not wins:
    sys.exit(1)
# 设置窗口（带标题的那个）；找不到就退回「最宽的那个」
settings = [w for w in wins if "设置" in w[5]]
hwnd, left, top, width, height, _ = (
    settings[0] if settings else max(wins, key=lambda w: w[3])
)
sx, sy = left + width // 2, top + 12
print(f"拖动点 ({sx}, {sy})")

user32.SetCursorPos(sx, sy)
time.sleep(0.25)
user32.mouse_event(0x0002, 0, 0, 0, 0)      # LEFTDOWN
time.sleep(0.15)
for i in range(1, 13):
    user32.SetCursorPos(sx + i * 6, sy + i * 2)
    time.sleep(0.06)
time.sleep(0.2)
user32.mouse_event(0x0004, 0, 0, 0, 0)      # LEFTUP
time.sleep(0.5)

print("进程还活着" if alive() else "进程没了（崩了）")
if alive():
    for w in windows_of(pid):
        print(f"  拖后 h={w[0]} rect=({w[1]},{w[2]}) {w[3]}x{w[4]} title='{w[5]}'")
