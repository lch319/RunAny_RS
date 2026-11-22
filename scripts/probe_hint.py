"""看一眼热字符串提示卡片：写一份「提示常驻」的配置，敲一半短语，把卡片截下来。

用法：python scripts/probe_hint.py [caret|cursor]
截图落在 target/hint_card.bmp / .png。
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

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(ROOT, "target", "debug", "RunAny_RS.exe")
SETTINGS = os.path.join(ROOT, "target", "debug", "settings.json")

ANCHOR = sys.argv[1] if len(sys.argv) > 1 else "caret"


def make_settings():
    settings = {
        "theme_mode": "FollowSystem",
        "panel_width": 210.0,
        "row_height": 24.0,
        "popup_hotkey": "Ctrl+Alt+M",
        "hotstrings": {
            "enabled": True,
            "hide_hint": False,
            "hint_after": 1,
            "hint_path_chars": 40,
            "hint_ms": 60000,
            "hint_opacity": 100,
            "hint_x": 16,
            "hint_y": 20,
            "anchor": ANCHOR,
            "hint_clickable": True,
        },
        "next_id": 9,
        "items": [
            {
                "kind": "Item",
                "id": "hs-1",
                "label": "写邮件",
                "hotstring": "ftw",
                "actions": [{"kind": "Text", "value": "Free the whales", "delay_ms": 0}],
            },
            {
                "kind": "Item",
                "id": "hs-2",
                "label": "打开记事本",
                "hotstring": "ftx",
                "actions": [{"kind": "Run", "value": r"C:\Windows\notepad.exe", "delay_ms": 0}],
            },
            {
                "kind": "Item",
                "id": "hs-3",
                "label": "插日期",
                "hotstring": "ft",
                "actions": [{"kind": "Text", "value": "{date}", "delay_ms": 0}],
            },
        ],
    }
    with open(SETTINGS, "w", encoding="utf-8") as fh:
        json.dump(settings, fh, ensure_ascii=False, indent=2)


def hint_window():
    """提示卡片 = 没有标题、宽高都很小的 Zed::Window。"""
    u = ctypes.windll.user32
    found = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def enum(hwnd, _):
        if u.IsWindowVisible(hwnd):
            cls = ctypes.create_unicode_buffer(256)
            u.GetClassNameW(hwnd, cls, 256)
            title = ctypes.create_unicode_buffer(256)
            u.GetWindowTextW(hwnd, title, 256)
            if cls.value == "Zed::Window" and not title.value:
                rect = wintypes.RECT()
                u.GetWindowRect(hwnd, ctypes.byref(rect))
                w = rect.right - rect.left
                h = rect.bottom - rect.top
                if 100 < w < 520 and 20 < h < 300:
                    found.append((hwnd, rect.left, rect.top, w, h))
        return True

    u.EnumWindows(enum, 0)
    found.sort(key=lambda item: item[3] * item[4])
    return found[-1] if found else None


def main():
    if not os.path.exists(EXE):
        print("先 cargo build")
        return 1
    make_settings()
    subprocess.run(["taskkill", "/IM", "RunAny_RS.exe", "/F"], capture_output=True)
    time.sleep(0.4)

    log = open(os.path.join(ROOT, "target", "hint_probe.log"), "w", encoding="utf-8")
    app = subprocess.Popen([EXE], cwd=os.path.dirname(EXE), stdout=log, stderr=log)
    time.sleep(3.5)

    hwnd, edit = hs_window.start()
    if not hwnd:
        print("靶子窗口没起来")
        app.terminate()
        return 2
    hs_window.focus(hwnd, edit)
    time.sleep(0.4)
    hs_window.detach_ime(edit)
    time.sleep(0.3)

    print(f"锚点 = {ANCHOR}，敲两个字符看提示…")
    win_input.type_text("ft")
    time.sleep(1.2)

    card = hint_window()
    if not card:
        print("没找到提示卡片（是不是没弹出来？）")
        app.terminate()
        return 3
    print(f"提示卡片 {hex(card[0])} @({card[1]}, {card[2]}) {card[3]}x{card[4]}")
    subprocess.run(
        [sys.executable, os.path.join(ROOT, "scripts", "shot.py"), hex(card[0]),
         os.path.join(ROOT, "target", "hint_card.bmp")],
        capture_output=True,
    )
    from PIL import Image

    Image.open(os.path.join(ROOT, "target", "hint_card.bmp")).save(
        os.path.join(ROOT, "target", "hint_card.png")
    )
    print("截图: target/hint_card.png")
    app.terminate()
    return 0


if __name__ == "__main__":
    sys.exit(main())
