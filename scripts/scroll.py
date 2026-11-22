"""在某个窗口上滚几下滚轮（看长页面用）。

用法：python scripts/scroll.py <hwnd> <notches>     正数向下，负数向上
"""
import ctypes
import sys
import time
from ctypes import wintypes

user32 = ctypes.windll.user32

hwnd = int(sys.argv[1], 0)
notches = int(sys.argv[2]) if len(sys.argv) > 2 else 5

rect = wintypes.RECT()
user32.GetWindowRect(hwnd, ctypes.byref(rect))
cx = (rect.left + rect.right) // 2
cy = (rect.top + rect.bottom) // 2
user32.SetCursorPos(cx, cy)
user32.SetForegroundWindow(hwnd)
time.sleep(0.2)

MOUSEEVENTF_WHEEL = 0x0800
for _ in range(abs(notches)):
    delta = -120 if notches > 0 else 120
    user32.mouse_event(MOUSEEVENTF_WHEEL, 0, 0, ctypes.c_uint(delta).value, 0)
    time.sleep(0.05)
print(f"滚了 {notches} 格 @({cx},{cy})")
