"""验证热字符串提示卡片的鼠标行为：

1. 鼠标停到卡片上 → **不消失**（用户可能正要点某一条）；
2. 鼠标移开 → **1 秒后**消失（不是按 hint_ms，也不是立刻）。

配置里把 `hint_ms` 设成 60 秒，这样卡片只可能是靠「移开后 1 秒」这条规则消失，
不会和「到点自动消失」混在一起。

用法：python scripts/probe_hint_leave.py
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
            "hint_path_chars": 0,
            "hint_ms": 60000,
            "hint_opacity": 100,
            "hint_x": 0,
            "hint_y": 0,
            "anchor": "cursor",
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
            }
        ],
    }
    with open(SETTINGS, "w", encoding="utf-8") as fh:
        json.dump(settings, fh, ensure_ascii=False, indent=2)


def hint_window():
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
        title = ctypes.create_unicode_buffer(64)
        u.GetWindowTextW(hwnd, title, 64)
        cls = ctypes.create_unicode_buffer(64)
        u.GetClassNameW(hwnd, cls, 64)
        if cls.value == "Zed::Window" and not title.value:
            rc = wintypes.RECT()
            u.GetWindowRect(hwnd, ctypes.byref(rc))
            w, h = rc.right - rc.left, rc.bottom - rc.top
            if 80 < w < 520 and 20 < h < 300:
                found.append((hwnd, rc.left, rc.top, w, h))
        return True

    u.EnumWindows(enum, 0)
    return found[-1] if found else None


def main():
    make_settings()
    subprocess.run(["taskkill", "/IM", "RunAny_RS.exe", "/F"], capture_output=True)
    time.sleep(0.5)
    app = subprocess.Popen([EXE], cwd=os.path.dirname(EXE))
    time.sleep(3.5)

    hwnd, edit = hs_window.start()
    hs_window.focus(hwnd, edit)
    time.sleep(0.4)

    # 鼠标停在一个已知位置，提示卡片就贴在那儿（偏移 0）
    u.SetCursorPos(500, 500)
    time.sleep(0.2)
    win_input.type_text("ft")
    time.sleep(0.8)

    card = hint_window()
    if not card:
        print("提示卡片没出来")
        app.kill()
        return 2
    print(f"卡片 @({card[1]}, {card[2]}) {card[3]}x{card[4]}")

    failures = 0
    # 1) 鼠标移到卡片上，等 2 秒，卡片必须还在
    u.SetCursorPos(card[1] + card[3] // 2, card[2] + card[4] // 2)
    time.sleep(0.15)
    u.SetCursorPos(card[1] + card[3] // 2 + 3, card[2] + card[4] // 2)
    time.sleep(2.0)
    still = hint_window() is not None
    if not still:
        failures += 1
    print(f"[{'通过' if still else '失败'}] 鼠标停在卡片上 2 秒：{'还在' if still else '消失了'}")

    # 2) 鼠标移开：0.4 秒时应当还在，1.4 秒时必须没了
    u.SetCursorPos(300, 800)
    time.sleep(0.4)
    early = hint_window() is not None
    time.sleep(1.1)
    late = hint_window() is None
    if not early:
        failures += 1
    if not late:
        failures += 1
    print(f"[{'通过' if early else '失败'}] 移开后 0.4 秒：{'还在' if early else '已经没了'}")
    print(f"[{'通过' if late else '失败'}] 移开后 1.5 秒：{'已经没了' if late else '还在'}")

    app.kill()
    print(f"\n{3 - failures}/3 通过")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
