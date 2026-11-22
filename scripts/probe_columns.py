"""看多列菜单里「子菜单从哪儿展开」。

多列菜单是一张大卡片，子菜单该贴**父项所在那一列**的右边缘（可以盖住右边几列），
而不是贴整张卡片的最右边。这个脚本直接把每一列的窗口矩形打印出来，
看 x 就知道了（不用截图）。

用法：python scripts/probe_columns.py [第几项有子菜单]
"""

import ctypes
import json
import os
import subprocess
import sys
import time
from ctypes import wintypes

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import win_input

u = ctypes.windll.user32

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(ROOT, "target", "debug", "RunAny_RS.exe")
SETTINGS = os.path.join(ROOT, "target", "debug", "settings.json")
SUBMENU_AT = int(sys.argv[1]) if len(sys.argv) > 1 else 2

VK_DOWN = 0x28
VK_RIGHT = 0x27


def make_settings():
    items = []
    for i in range(1, 17):
        node = {
            "kind": "Item",
            "id": f"c{i}",
            "label": f"第 {i} 项",
            "actions": [{"kind": "Text", "value": "x", "delay_ms": 0}],
        }
        if i == SUBMENU_AT:
            node["children"] = [
                {
                    "kind": "Item",
                    "id": f"c{i}-1",
                    "label": "子项一",
                    "actions": [{"kind": "Text", "value": "y", "delay_ms": 0}],
                },
                {
                    "kind": "Item",
                    "id": f"c{i}-2",
                    "label": "子项二",
                    "actions": [{"kind": "Text", "value": "z", "delay_ms": 0}],
                },
            ]
        items.append(node)
        if i == 8:
            items.append(
                {
                    "kind": "Separator",
                    "id": "cb1",
                    "column_break": True,
                }
            )
    settings = {
        "theme_mode": "FollowSystem",
        "panel_width": 210.0,
        "row_height": 24.0,
        "popup_hotkey": "Ctrl+Alt+M",
        "show_separators": False,
        "next_id": 99,
        "items": items,
    }
    with open(SETTINGS, "w", encoding="utf-8") as fh:
        json.dump(settings, fh, ensure_ascii=False, indent=2)


def windows():
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
        rc = wintypes.RECT()
        u.GetWindowRect(hwnd, ctypes.byref(rc))
        w, h = rc.right - rc.left, rc.bottom - rc.top
        if cls.value == "Zed::Window" and w > 80 and h > 40:
            found.append((rc.left, rc.top, w, h))
        return True

    u.EnumWindows(enum, 0)
    return sorted(found)


def main():
    make_settings()
    subprocess.run(["taskkill", "/IM", "RunAny_RS.exe", "/F"], capture_output=True)
    time.sleep(0.5)
    app = subprocess.Popen([EXE], cwd=os.path.dirname(EXE))
    time.sleep(4.0)

    u.SetCursorPos(600, 400)
    time.sleep(0.2)
    for vk in (0x11, 0x12):
        u.keybd_event(vk, 0, 0, 0)
        time.sleep(0.02)
    time.sleep(0.05)
    u.keybd_event(0x4D, 0, 0, 0)
    time.sleep(0.05)
    u.keybd_event(0x4D, 0, 2, 0)
    time.sleep(0.06)
    for vk in (0x12, 0x11):
        u.keybd_event(vk, 0, 2, 0)
        time.sleep(0.02)
    time.sleep(0.8)
    print("打开菜单：", windows())

    for _ in range(SUBMENU_AT - 1):
        win_input.tap(VK_DOWN)
        time.sleep(0.08)
    win_input.tap(VK_RIGHT)
    time.sleep(0.8)
    cols = windows()
    print("展开子菜单：", cols)
    if len(cols) >= 2:
        parent, child = cols[0], cols[1]
        print(f"父卡片 x={parent[0]} 宽={parent[2]}；子卡片 x={child[0]}")
        print(f"「贴着第一列的右边缘」应当是 x≈{parent[0] + 210}；贴在整张卡片右边则是 x≈{parent[0] + parent[2]}")

    app.kill()


if __name__ == "__main__":
    main()
