"""验证提示卡片的「悬停不消失 + 点一条就执行」。

流程：
1. 写一份 hint_ms=1500 的配置（1.5 秒就该自己消失）；
2. 起程序 + 靶子窗口，敲 `ftw` 让提示弹出来；
3. 立刻把鼠标移到卡片**第一行**上，等 3.5 秒（远超 1.5 秒）→ 卡片应该还在；
4. 点一下那一行 → 靶子窗口里应该出现那一条的展开文本，触发词被删掉。

用法：python scripts/probe_hint_click.py
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
u = ctypes.windll.user32


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
            "hint_ms": 1500,
            "hint_opacity": 100,
            "hint_x": 16,
            "hint_y": 20,
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
            },
            {
                "kind": "Item",
                "id": "hs-2",
                "label": "插日期",
                "hotstring": "ft",
                "actions": [{"kind": "Text", "value": "DATE-HERE", "delay_ms": 0}],
            },
        ],
    }
    with open(SETTINGS, "w", encoding="utf-8") as fh:
        json.dump(settings, fh, ensure_ascii=False, indent=2)


def hint_window():
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


def click(x, y):
    u.SetCursorPos(x, y)
    time.sleep(0.25)
    u.mouse_event(0x0002, 0, 0, 0, 0)
    time.sleep(0.06)
    u.mouse_event(0x0004, 0, 0, 0, 0)
    time.sleep(0.6)


def main():
    if not os.path.exists(EXE):
        print("先 cargo build")
        return 1
    make_settings()
    subprocess.run(["taskkill", "/IM", "RunAny_RS.exe", "/F"], capture_output=True)
    time.sleep(0.4)

    log = open(os.path.join(ROOT, "target", "hint_click.log"), "w", encoding="utf-8")
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

    win_input.type_text("ftw")
    time.sleep(0.8)
    card = hint_window()
    if not card:
        print("没有任何提示卡片")
        app.terminate()
        return 3
    card_hwnd, left, top, width, height = card
    print(f"卡片 {hex(card_hwnd)} @({left},{top}) {width}x{height}")

    # 停到第一行（卡片内边距 8px + 行高一半）
    hover_x, hover_y = left + width // 2, top + 8 + 8
    u.SetCursorPos(hover_x, hover_y)
    print("鼠标停在第一行，等 3.5 秒（hint_ms 只有 1.5 秒）…")
    time.sleep(3.5)

    still = hint_window()
    if still and still[0] == card_hwnd:
        print("[通过] 悬停期间没有消失")
    else:
        print("[失败] 卡片在悬停期间消失了")

    print("点一下第一行…")
    click(hover_x, hover_y)
    time.sleep(0.9)
    got = hs_window.text_of(edit)
    expect = "Free the whales"
    if got == expect:
        print(f"[通过] 点完内容 = {got!r}")
    else:
        print(f"[失败] 点完内容 = {got!r}，期望 {expect!r}")

    # 焦点有没有被卡片抢走：再敲一个字，应该落进同一个 EDIT
    win_input.type_text("Z")
    time.sleep(0.5)
    after = hs_window.text_of(edit)
    print(f"再敲一个 Z：{after!r}（应该以 Z 结尾）")

    app.terminate()
    log.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
