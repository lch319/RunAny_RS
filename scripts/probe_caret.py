"""量一下「提示显示位置 = 光标插入处」到底贴到哪儿了。

做法：
1. 写一份 anchor=caret、偏移都设 0 的配置（这样卡片左上角就应该正好在插入点）；
2. 起 RunAny_RS，起一个原生 EDIT 靶子并点进去（插入点就在点的地方）；
3. 把鼠标挪到屏幕另一边（离插入点远一点，好区分是「贴鼠标」还是「贴插入点」）；
4. 逐字敲 ftw，等提示卡片出现；
5. 打印「插入点矩形」（GetGUIThreadInfo）和「提示窗口矩形」，看差多少像素。

用法：python scripts/probe_caret.py [caret|cursor]
"""

import ctypes
import json
import os
import subprocess
import sys
import time
from ctypes import wintypes

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hs_window
import win_input

u = ctypes.windll.user32

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(ROOT, "target", "debug", "RunAny_RS.exe")
SETTINGS = os.path.join(ROOT, "target", "debug", "settings.json")
ANCHOR = sys.argv[1] if len(sys.argv) > 1 else "caret"


class GUITHREADINFO(ctypes.Structure):
    _fields_ = [
        ("cbSize", wintypes.DWORD),
        ("flags", wintypes.DWORD),
        ("hwndActive", wintypes.HWND),
        ("hwndFocus", wintypes.HWND),
        ("hwndCapture", wintypes.HWND),
        ("hwndMenuOwner", wintypes.HWND),
        ("hwndMoveSize", wintypes.HWND),
        ("hwndCaret", wintypes.HWND),
        ("rcCaret", wintypes.RECT),
    ]


def write_settings():
    settings = {
        "theme_mode": "FollowSystem",
        "panel_width": 210.0,
        "row_height": 24.0,
        "popup_hotkey": "Ctrl+Alt+M",
        "hotstrings": {
            "enabled": True,
            "hide_hint": False,
            "hint_after": 1,
            "hint_path_chars": 0,
            "hint_ms": 60000,
            "hint_opacity": 100,
            "hint_x": 0,
            "hint_y": 0,
            "anchor": ANCHOR,
            "hint_clickable": False,
        },
        "next_id": 9,
        "items": [
            {
                "kind": "Item",
                "id": "hs-1",
                "label": "写邮件",
                "hotstring": "ftw",
                "actions": [{"kind": "Text", "value": "Free the whales", "delay_ms": 0}],
            }
        ],
    }
    with open(SETTINGS, "w", encoding="utf-8") as fh:
        json.dump(settings, fh, ensure_ascii=False, indent=2)


def caret_rect():
    """前台线程的插入点（屏幕物理像素）。"""
    fg = u.GetForegroundWindow()
    thread = u.GetWindowThreadProcessId(fg, None)
    info = GUITHREADINFO()
    info.cbSize = ctypes.sizeof(GUITHREADINFO)
    if not u.GetGUIThreadInfo(thread, ctypes.byref(info)):
        return None
    if not info.hwndCaret:
        return None
    origin = wintypes.RECT()
    u.GetWindowRect(info.hwndCaret, ctypes.byref(origin))
    rc = info.rcCaret
    return (
        origin.left + rc.left,
        origin.top + rc.top,
        origin.left + rc.right,
        origin.top + rc.bottom,
    )


def hint_window():
    """提示卡片 = 本进程里那张很小的可见 Zed::Window。"""
    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq RunAny_RS.exe", "/FO", "CSV", "/NH"],
        capture_output=True,
        text=True,
        encoding="gbk",
        errors="replace",
    )
    pid = int(out.stdout.split(",")[1].strip('"'))
    found = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def enum(hwnd, _):
        owner = wintypes.DWORD()
        u.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value != pid or not u.IsWindowVisible(hwnd):
            return True
        cls = ctypes.create_unicode_buffer(64)
        u.GetClassNameW(hwnd, cls, 64)
        if cls.value != "Zed::Window":
            return True
        rc = wintypes.RECT()
        u.GetWindowRect(hwnd, ctypes.byref(rc))
        w, h = rc.right - rc.left, rc.bottom - rc.top
        # 菜单卡片是 200+ 宽，提示卡片窄一点、矮一点
        if 60 < w < 500 and 20 < h < 300:
            found.append((hwnd, rc.left, rc.top, w, h))
        return True

    u.EnumWindows(enum, 0)
    return found[0] if found else None


def main():
    write_settings()
    subprocess.run(["taskkill", "/IM", "RunAny_RS.exe", "/F"], capture_output=True)
    time.sleep(0.5)
    app = subprocess.Popen([EXE], cwd=os.path.dirname(EXE))
    time.sleep(4.0)

    hwnd, edit = hs_window.start()
    time.sleep(0.4)
    print("target:", hs_window.focus(hwnd, edit))
    time.sleep(0.3)

    # 点一下 EDIT 中间偏左的位置，插入点就落在那儿
    rc = wintypes.RECT()
    u.GetWindowRect(edit, ctypes.byref(rc))
    u.SetCursorPos(rc.left + 60, rc.top + 40)
    time.sleep(0.15)
    u.mouse_event(0x0002, 0, 0, 0, 0)
    time.sleep(0.05)
    u.mouse_event(0x0004, 0, 0, 0, 0)
    time.sleep(0.3)

    # 鼠标挪到远处：这样「贴鼠标」和「贴插入点」的结果一眼能分开
    u.SetCursorPos(60, 900)
    time.sleep(0.2)
    print("cursor:", u.GetCursorPos.__name__ and None)

    for ch in "ftw":
        win_input.type_char(ch)
        time.sleep(0.18)
    time.sleep(0.6)

    caret = caret_rect()
    hint = hint_window()
    print("caret(物理):", caret)
    print("hint (物理):", hint)
    if caret and hint:
        _, left, top, w, h = hint
        print(f"hint 左上 - caret 右下 = ({left - caret[2]}, {top - caret[3]})")
        print(f"hint 左上 - caret 左上 = ({left - caret[0]}, {top - caret[1]})")

    app.kill()


if __name__ == "__main__":
    main()
