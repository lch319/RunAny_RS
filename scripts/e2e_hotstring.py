"""端到端验证热字符串：真的用键盘钩子在任何程序里把缩写换成文本。

做的事：
1. 备份 `target/debug/settings.json`，写一份只带一条热字符串的配置
   （`ftw` → 发送文本 `Free the whales`）；
2. 起 RunAny_RS（debug），等钩子就绪；
3. 起一个原生 Win32 靶子窗口（一个多行 EDIT）并抢到焦点；
4. 逐条跑场景：敲字、读回 EDIT 的内容、比对；
5. 还原配置、退出程序。

靶子窗口为什么不用记事本：「这台机器」上的记事本被换成了 Notepad4，
注入的按键进不去；还有常驻工具抢前台。自己建窗口最可控。

两个坑（都踩过，写在这里免得再踩）：
- `INPUT` 结构体在 64 位下必须是 40 字节，联合体里只放 `KEYBDINPUT` 会变成 32，
  `SendInput` 直接返回 0（err 87），一个键都发不出去；
- 这台机器默认开着中文输入法，字母按下会变成 `VK_PROCESSKEY` 被组字吃掉，
  靶子窗口要先 `ImmAssociateContext(edit, NULL)` 把输入法摘掉。

用法：python scripts/e2e_hotstring.py
"""

import ctypes
import json
import os
import shutil
import subprocess
import sys
import time
from ctypes import wintypes

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hs_window
import win_input

u = ctypes.windll.user32

VK_SPACE = 0x20
VK_TAB = 0x09
VK_BACK = 0x08
VK_RETURN = 0x0D
VK_OEM_PERIOD = 0xBE  # 主键盘上的 `.`（注意不是 0x2E，那是 VK_DELETE）

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# 默认测 debug 版；`RUNANY_EXE=dist\RunAny_RS.exe` 可以测打包后的压缩版
EXE = os.environ.get("RUNANY_EXE") or os.path.join(ROOT, "target", "debug", "RunAny_RS.exe")
if not os.path.isabs(EXE):
    EXE = os.path.join(ROOT, EXE)
# 便携版把 settings.json 放在 exe 旁边，跟着 exe 走
SETTINGS = os.path.join(os.path.dirname(EXE), "settings.json")
BACKUP = SETTINGS + ".e2e-backup"
# 菜单项现在住在 menus/<名字>.runa（settings.json 里那份只是兜底快照），
# 测试要连它一起换掉，否则程序启动时会被真实配置覆盖回来。
MENUS = os.path.join(os.path.dirname(EXE), "menus")
MENUS_BACKUP = MENUS + ".e2e-backup"
PROFILE = "默认"
LOG = os.path.join(ROOT, "target", "hs_e2e.log")

PHRASE = "ftw"
EXPAND = "Free the whales"
# 「一匹配就执行」的那一条（AHK 的 `*`）：敲完 j@ 立刻展开，不用空格
NOW_PHRASE = "j@"
NOW_EXPAND = "jsmith@somedomain.com"
# 用户报的那一类：带 Shift 符号（@ 是 Shift+2）+ 大小写混排
SHIFT_PHRASE = "@Win112"
SHIFT_EXPAND = "WIN112-OK"


def make_settings():
    settings = {
        "theme_mode": "FollowSystem",
        "panel_width": 210.0,
        "row_height": 24.0,
        "popup_hotkey": "Ctrl+Alt+M",
        "hotstrings": {
            "enabled": True,
            "hide_hint": True,
            "hint_after": 1,
            "hint_path_chars": 10,
            "hint_ms": 6000,
            "hint_opacity": 90,
            "hint_x": 16,
            "hint_y": 20,
        },
        "next_id": 9,
        "items": [
            {
                "kind": "Item",
                "id": "hs-test",
                "label": "展开 ftw",
                "hotstring": PHRASE,
                "actions": [{"kind": "Text", "value": EXPAND, "delay_ms": 0}],
            },
            {
                "kind": "Item",
                "id": "hs-shift",
                "label": "插 Win112",
                "hotstring": SHIFT_PHRASE,
                "actions": [{"kind": "Text", "value": SHIFT_EXPAND, "delay_ms": 0}],
            },
            {
                "kind": "Item",
                "id": "hs-now",
                "label": "插邮箱",
                "hotstring": NOW_PHRASE,
                "hotstring_now": True,
                "actions": [{"kind": "Text", "value": NOW_EXPAND, "delay_ms": 0}],
            },
        ],
    }
    with open(SETTINGS, "w", encoding="utf-8") as fh:
        json.dump(settings, fh, ensure_ascii=False, indent=2)
    # 菜单项的真身：写到 menus/默认.runa，不然启动时会被这份文件覆盖
    os.makedirs(MENUS, exist_ok=True)
    profile = {
        "name": PROFILE,
        "next_id": settings["next_id"],
        "items": settings["items"],
    }
    with open(os.path.join(MENUS, PROFILE + ".runa"), "w", encoding="utf-8") as fh:
        json.dump(profile, fh, ensure_ascii=False, indent=2)


def clear(edit):
    """把 EDIT 清空（直接 WM_SETTEXT，不经过键盘）。"""
    hs_window.u.SendMessageW(edit, 0x000C, 0, "")


def play(keys):
    """按脚本敲键。字符串按字符敲，整数当虚拟键，浮点数当停顿（秒）。"""
    for key in keys:
        if isinstance(key, float):
            time.sleep(key)
        elif isinstance(key, int):
            win_input.tap(key)
        else:
            win_input.type_text(key)


# (名字, 按键脚本, 期望的 EDIT 内容)
CASES = [
    ("空格结尾", ["hello ", PHRASE, VK_SPACE], f"hello {EXPAND} "),
    ("句号结尾", ["hello ", PHRASE, VK_OEM_PERIOD], f"hello {EXPAND}."),
    ("回车结尾", ["hi ", PHRASE, VK_RETURN], f"hi {EXPAND}\r\n"),
    ("敲错再退格", ["ftx", VK_BACK, "w", VK_SPACE], f"{EXPAND} "),
    ("没有结束字符不替换", [PHRASE], PHRASE),
    ("前缀不命中", ["xftw", VK_SPACE], "xftw "),
    # 两轮之间得等一下：展开是注入字符，人打得比注入还快的话两串字会在
    # 同一个光标处交织（AHK 同样如此，不是实现的问题）
    ("连敲两次", [PHRASE, VK_SPACE, 0.8, PHRASE, VK_SPACE], f"{EXPAND} {EXPAND} "),
    # AHK 的 `*`：Last 那个字符一打出来就执行，不用空格
    ("立刻执行", ["mail ", NOW_PHRASE], f"mail {NOW_EXPAND}"),
    # 立刻执行之后接着打字，光标应该正接在后面（中间留出注入的时间：
    # 人打得比注入还快的话两串字会在同一个光标处交织，AHK 也一样）
    ("立刻执行后继续打字", ["mail ", NOW_PHRASE, 1.0, " tail"], f"mail {NOW_EXPAND} tail"),
    # 只开了「立刻执行」的那一条不受影响：ftw 敲完不能自己展开
    ("立刻执行不影响别的短语", [PHRASE], PHRASE),
    # 带 Shift 符号 + 大小写混排的短语（@ 是 Shift+2）
    ("Shift 符号短语", ["x ", SHIFT_PHRASE, VK_SPACE], f"x {SHIFT_EXPAND} "),
    ("全小写也命中", ["x ", SHIFT_PHRASE.lower(), VK_SPACE], f"x {SHIFT_EXPAND} "),
    # 敲多了退格删掉多余的那个字符，剩下的正好是短语
    ("多敲一个退格删掉", ["x ", SHIFT_PHRASE, "z", VK_BACK, VK_SPACE],
     f"x {SHIFT_EXPAND} "),
    # 同一个短语连敲两次（缓冲区必须干干净净地重来）
    ("Shift 短语连敲两次", ["x ", SHIFT_PHRASE, VK_SPACE, 0.9, SHIFT_PHRASE, VK_SPACE],
     f"x {SHIFT_EXPAND} {SHIFT_EXPAND} "),
]


def main():
    if not os.path.exists(EXE):
        print("先 cargo build")
        return 1
    have_backup = os.path.exists(SETTINGS)
    if have_backup:
        shutil.copy2(SETTINGS, BACKUP)
    have_menus = os.path.isdir(MENUS)
    if have_menus:
        if os.path.isdir(MENUS_BACKUP):
            shutil.rmtree(MENUS_BACKUP)
        shutil.copytree(MENUS, MENUS_BACKUP)
    make_settings()

    subprocess.run(["taskkill", "/IM", "RunAny_RS.exe", "/F"], capture_output=True)
    time.sleep(0.4)

    log = open(LOG, "w", encoding="utf-8")
    app = subprocess.Popen([EXE], cwd=os.path.dirname(EXE), stdout=log, stderr=log)
    time.sleep(3.5)

    hwnd, edit = hs_window.start()
    if not hwnd or not edit:
        print("靶子窗口没起来")
        app.terminate()
        log.close()
        return 2

    hs_window.focus(hwnd, edit)
    time.sleep(0.3)
    # 重新激活窗口时系统会把输入法挂回来，敲之前再摘一次
    hs_window.detach_ime(edit)
    time.sleep(0.3)
    print(f"靶子窗口 {hs_window.window_class(hs_window.u.GetForegroundWindow())} {hex(hwnd)}\n")

    failures = 0
    for name, keys, expect in CASES:
        clear(edit)
        hs_window.focus(hwnd, edit)
        time.sleep(0.15)
        play(keys)
        time.sleep(0.9)
        got = hs_window.text_of(edit)
        ok = got == expect
        if not ok:
            failures += 1
        mark = "通过" if ok else "失败"
        print(f"[{mark}] {name}: 得到 {got!r}，期望 {expect!r}")

    # 用鼠标点一下（光标挪到别处）再敲：缓冲区必须已经清干净，
    # 不然会带着点之前那几个字符一起匹配 —— 这正是「时灵时不灵」的来源之一。
    clear(edit)
    hs_window.focus(hwnd, edit)
    time.sleep(0.15)
    win_input.type_text("x")
    rc = wintypes.RECT()
    u.GetWindowRect(edit, ctypes.byref(rc))
    u.SetCursorPos(rc.left + 320, rc.top + 80)
    time.sleep(0.15)
    u.mouse_event(0x0002, 0, 0, 0, 0)
    time.sleep(0.06)
    u.mouse_event(0x0004, 0, 0, 0, 0)
    time.sleep(0.35)
    win_input.type_text(SHIFT_PHRASE)
    win_input.tap(VK_SPACE)
    time.sleep(0.9)
    got = hs_window.text_of(edit)
    ok = SHIFT_EXPAND in got
    if not ok:
        failures += 1
    print(f"[{'通过' if ok else '失败'}] 点击后再敲: 得到 {got!r}，期望包含 {SHIFT_EXPAND!r}")

    app.terminate()
    log.close()
    time.sleep(0.3)
    if have_backup:
        shutil.move(BACKUP, SETTINGS)
    elif os.path.exists(SETTINGS):
        os.remove(SETTINGS)
    # menus 目录同样还原
    if os.path.isdir(MENUS_BACKUP):
        if os.path.isdir(MENUS):
            shutil.rmtree(MENUS)
        shutil.move(MENUS_BACKUP, MENUS)
    elif os.path.isdir(MENUS):
        shutil.rmtree(MENUS)

    total = len(CASES) + 1
    print(f"\n{total - failures}/{total} 通过")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
