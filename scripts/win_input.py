"""按键注入的共用小工具（SendInput）。

踩过的坑：`INPUT` 在 64 位下必须是 40 字节。如果联合体里只放 `KEYBDINPUT`
（24 字节），整个 `INPUT` 只有 32 字节，`SendInput` 会认为参数非法、返回 0
并置 `GetLastError() == 87`，**一个键都不会发出去**——外面看起来就像
「钩子把按键吃了」。所以联合体里必须同时放 `MOUSEINPUT`，让大小对上 40。
"""

import ctypes
import time
from ctypes import wintypes

u = ctypes.windll.user32
k32 = ctypes.windll.kernel32

KEYUP = 0x0002
INPUT_KEYBOARD = 1
INPUT_MOUSE = 0
VK_SPACE = 0x20

ULONG_PTR = ctypes.c_ulonglong if ctypes.sizeof(ctypes.c_void_p) == 8 else ctypes.c_ulong


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wintypes.LONG),
        ("dy", wintypes.LONG),
        ("mouseData", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ULONG_PTR),
    ]


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ULONG_PTR),
    ]


class HARDWAREINPUT(ctypes.Structure):
    _fields_ = [
        ("uMsg", wintypes.DWORD),
        ("wParamL", wintypes.WORD),
        ("wParamH", wintypes.WORD),
    ]


class _INPUTUNION(ctypes.Union):
    _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT), ("hi", HARDWAREINPUT)]


class INPUT(ctypes.Structure):
    _anonymous_ = ("u",)
    _fields_ = [("type", wintypes.DWORD), ("u", _INPUTUNION)]


assert ctypes.sizeof(INPUT) == (40 if ctypes.sizeof(ctypes.c_void_p) == 8 else 28), ctypes.sizeof(INPUT)


def send_key(vk, up=False):
    inp = INPUT(type=INPUT_KEYBOARD)
    inp.ki = KEYBDINPUT(wVk=vk, wScan=0, dwFlags=KEYUP if up else 0, time=0, dwExtraInfo=0)
    if u.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT)) != 1:
        raise OSError(f"SendInput 失败 (err={k32.GetLastError()})")
    return True


def tap(vk, hold=0.03, gap=0.035):
    send_key(vk, False)
    time.sleep(hold)
    send_key(vk, True)
    time.sleep(gap)


# 美式布局上「字符 → (虚拟键, 要不要 Shift)」。`ord(ch.upper())` 只对字母数字成立，
# `@` 是 Shift+2、`:` 是 Shift+`;`，照 ord 发出去什么都不是。
SHIFTED = {
    "!": ("1", True), "@": ("2", True), "#": ("3", True), "$": ("4", True),
    "%": ("5", True), "^": ("6", True), "&": ("7", True), "*": ("8", True),
    "(": ("9", True), ")": ("0", True),
    "_": ("-", True), "+": ("=", True), "{": ("[", True), "}": ("]", True),
    "|": ("\\", True), ":": (";", True), '"': ("'", True), "<": (",", True),
    ">": (".", True), "?": ("/", True), "~": ("`", True),
    "-": ("-", False), "=": ("=", False), "[": ("[", False), "]": ("]", False),
    "\\": ("\\", False), ";": (";", False), "'": ("'", False), ",": (",", False),
    ".": (".", False), "/": ("/", False), "`": ("`", False),
}
PUNCT_VK = {
    "-": 0xBD, "=": 0xBB, "[": 0xDB, "]": 0xDD, "\\": 0xDC, ";": 0xBA, "'": 0xDE,
    ",": 0xBC, ".": 0xBE, "/": 0xBF, "`": 0xC0,
}


def type_char(ch):
    """敲一个字符（按住 Shift 发大写 / 上面的符号）。"""
    if ch == " ":
        tap(VK_SPACE)
        return
    base = ch
    shift = False
    if ch in SHIFTED:
        base, shift = SHIFTED[ch]
    elif ch.isalpha():
        shift = ch.isupper()
        base = ch.upper()
    if shift:
        send_key(0x10, False)  # VK_SHIFT
        time.sleep(0.01)
    if base.isdigit():
        tap(0x30 + int(base))
    elif base.isalpha():
        tap(ord(base.upper()))
    else:
        tap(PUNCT_VK.get(base, 0xBE))
    if shift:
        time.sleep(0.01)
        send_key(0x10, True)


def type_text(text):
    for ch in text:
        type_char(ch)
