"""验证输入法开着的几种状态下，热字符串还能不能按预期触发。

中文 Windows 上输入法是常年挂着的。三件事分开测：

  A. **中文模式 + 纯字母短语**（`ftw`）→ 应当按拼音命中就展开。
     这是中文用户的实际场景：输入法停在中文，敲英文缩写。命中的那一刻拼音还在
     组字缓冲区里，那几个退格删的正是拼音字母。
  B. **中文模式 + 带数字的短语**（`@Win112`）→ 不展开，也不该乱删字。
     中文模式下数字键是「选第 N 个候选词」，抢过来会把用户上屏的汉字删掉，
     所以数字一律不进缓冲区（这比「能匹配」更重要）。
  C. **英文模式（输入法开着但没组字）**→ 应当正常展开。
     本机这个输入法不理 `ImmSetConversionStatus`，切不过去就跳过。

用法：python scripts/probe_ime.py
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

imm = ctypes.windll.imm32
u = ctypes.windll.user32

# 64 位下 HWND 必须按指针传，不然 ctypes 会把句柄截成 32 位，ImmGetContext 直接给 0
imm.ImmGetContext.restype = wintypes.HANDLE
imm.ImmGetContext.argtypes = [wintypes.HWND]
imm.ImmSetOpenStatus.argtypes = [wintypes.HANDLE, wintypes.BOOL]
imm.ImmGetOpenStatus.argtypes = [wintypes.HANDLE]
imm.ImmSetConversionStatus.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.DWORD]
imm.ImmGetConversionStatus.argtypes = [
    wintypes.HANDLE,
    ctypes.POINTER(wintypes.DWORD),
    ctypes.POINTER(wintypes.DWORD),
]
imm.ImmReleaseContext.argtypes = [wintypes.HWND, wintypes.HANDLE]

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(ROOT, "target", "debug", "RunAny_RS.exe")
SETTINGS = os.path.join(ROOT, "target", "debug", "settings.json")
# 菜单项的真身住在 menus/<名字>.runa；settings.json 里那份只是兜底快照，
# 启动时会被文件覆盖回来 —— 不一起换掉，写进去的规则根本不会生效。
MENUS = os.path.join(ROOT, "target", "debug", "menus")
PROFILE = "默认"

IME_CMODE_NATIVE = 0x0001
IME_CMODE_ALPHANUMERIC = 0x0000
VK_SPACE = 0x20

PHRASE = "@Win112"
EXPAND = "WIN112-OK"
# 纯字母短语：中文模式下真正能靠拼音匹配的那一类
LETTERS = "ftw"
LETTERS_EXPAND = "Free the whales"
# 「不等结束符」的那一条：敲完最后一个字母就该展开，全靠组字缓冲里那几个退格
NOW_LETTERS = "qqq"
NOW_EXPAND = "NOW-OK"


def make_settings():
    settings = {
        "theme_mode": "FollowSystem",
        "panel_width": 210.0,
        "row_height": 24.0,
        "popup_hotkey": "Ctrl+Alt+M",
        "hotstrings": {"enabled": True, "hide_hint": True, "hint_after": 1},
        "next_id": 9,
        "items": [
            {
                "kind": "Item",
                "id": "hs-ime",
                "label": "插 Win112",
                "hotstring": PHRASE,
                "actions": [{"kind": "Text", "value": EXPAND, "delay_ms": 0}],
            },
            {
                "kind": "Item",
                "id": "hs-ime-letters",
                "label": "展开 ftw",
                "hotstring": LETTERS,
                "actions": [{"kind": "Text", "value": LETTERS_EXPAND, "delay_ms": 0}],
            },
            {
                "kind": "Item",
                "id": "hs-ime-now",
                "label": "立刻展开 qqq",
                "hotstring": NOW_LETTERS,
                "hotstring_now": True,
                "actions": [{"kind": "Text", "value": NOW_EXPAND, "delay_ms": 0}],
            },
        ],
    }
    with open(SETTINGS, "w", encoding="utf-8") as fh:
        json.dump(settings, fh, ensure_ascii=False, indent=2)
    os.makedirs(MENUS, exist_ok=True)
    with open(os.path.join(MENUS, PROFILE + ".runa"), "w", encoding="utf-8") as fh:
        json.dump(
            {"name": PROFILE, "next_id": settings["next_id"], "items": settings["items"]},
            fh,
            ensure_ascii=False,
            indent=2,
        )


def set_mode(hwnd, native):
    """把输入法切到中文 / 英文模式，并保证它是「开着」的。返回真实状态说明。"""
    ctx = imm.ImmGetContext(hwnd)
    if not ctx:
        return "拿不到输入法上下文"
    imm.ImmSetOpenStatus(ctx, True)
    imm.ImmSetConversionStatus(ctx, IME_CMODE_NATIVE if native else IME_CMODE_ALPHANUMERIC, 0)
    conversion = wintypes.DWORD()
    sentence = wintypes.DWORD()
    imm.ImmGetConversionStatus(ctx, ctypes.byref(conversion), ctypes.byref(sentence))
    opened = bool(imm.ImmGetOpenStatus(ctx))
    imm.ImmReleaseContext(hwnd, ctx)
    return f"开={opened} cmode=0x{conversion.value:04x}"


def clear(edit):
    u.SendMessageW(edit, 0x000C, 0, "")


def main():
    # 备份真实配置，跑完（不管是成功还是异常）都还原回去
    backup_settings = SETTINGS + ".ime-backup"
    backup_menus = MENUS + ".ime-backup"
    had_settings = os.path.exists(SETTINGS)
    had_menus = os.path.isdir(MENUS)
    if had_settings:
        shutil.copy2(SETTINGS, backup_settings)
    if had_menus:
        if os.path.isdir(backup_menus):
            shutil.rmtree(backup_menus)
        shutil.copytree(MENUS, backup_menus)
    try:
        return run_cases()
    finally:
        if had_settings:
            shutil.copy2(backup_settings, SETTINGS)
            os.remove(backup_settings)
        if had_menus:
            shutil.rmtree(MENUS, ignore_errors=True)
            shutil.copytree(backup_menus, MENUS)
            shutil.rmtree(backup_menus, ignore_errors=True)


def run_cases():
    make_settings()
    subprocess.run(["taskkill", "/IM", "RunAny_RS.exe", "/F"], capture_output=True)
    time.sleep(0.4)
    app = subprocess.Popen([EXE], cwd=os.path.dirname(EXE))
    time.sleep(3.5)

    # 这次**不摘**输入法：要测的就是「它开着」
    hwnd, edit = hs_window.start(detach=False)
    hs_window.focus(hwnd, edit)
    time.sleep(0.4)

    failures = 0
    # (名字, 中文模式?, 短语, 期望结果)
    cases = [
        ("中文模式 + 纯字母（应当展开）", True, LETTERS, True, f"{LETTERS_EXPAND} "),
        ("中文模式 + 不等结束符（一敲完就展开）", True, NOW_LETTERS, False, NOW_EXPAND),
        ("中文模式 + 带数字（不该展开）", True, PHRASE, True, None),
        # 先试着切英文：有些输入法（本机这个就是）不理 ImmSetConversionStatus，
        # 那就别硬测「英文模式」，只把实测到的转换模式打出来。
        ("英文模式（应当展开）", False, PHRASE, True, f"{EXPAND} "),
    ]
    for name, native, phrase, trailing_space, expect in cases:
        clear(edit)
        hs_window.focus(hwnd, edit)
        time.sleep(0.2)
        ok = set_mode(edit, native)
        time.sleep(0.4)
        wants_native = "cmode=0x0001" in ok
        if native != wants_native:
            print(f"[跳过] {name}：本机输入法不认这个模式切换（{ok}）")
            continue
        win_input.type_text(phrase)
        if trailing_space:
            win_input.tap(VK_SPACE)
        time.sleep(1.0)
        got = hs_window.text_of(edit)
        # 期望 None 的那条只要求「没展开」：中文模式下输入法会把字母吃进
        # 组字缓冲区，EDIT 里落下来的字不一定齐，整个字符串比不了
        passed = (EXPAND not in got) if expect is None else got == expect
        if not passed:
            failures += 1
        print(f"[{'通过' if passed else '失败'}] {name}（{ok}）: {got!r}，期望 {expect!r}")

    app.kill()
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
