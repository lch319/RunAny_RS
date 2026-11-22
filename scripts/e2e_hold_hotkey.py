"""端到端验证：按住热键的修饰键不放时，能不能抓到选中内容。

流程：
1. 起一个干净的 RunAny_RS（开 `GPUI_MENU_DEBUG_CAPTURE=1`，把抓取结果打到 stderr）；
2. 用记事本打开一个临时文件，选中全部文字；
3. **按住** Ctrl+Alt 不放，敲一下 M（触发全局热键），保持 1.2 秒再松手 ——
   这正是「手还没抬起来菜单就弹出来了」的场景；
4. 读日志，看有没有「抓到选中内容」。

用法（在项目根目录）：python scripts/e2e_hold_hotkey.py [exe路径]
默认用 target/debug/RunAny_RS.exe。
"""

import ctypes
import os
import subprocess
import sys
import tempfile
import time

user32 = ctypes.windll.user32
VK_CTRL, VK_ALT, VK_M, VK_A = 0x11, 0x12, 0x4D, 0x41
KEYUP = 2


def tap(vk: int) -> None:
    user32.keybd_event(vk, 0, 0, 0)
    time.sleep(0.02)
    user32.keybd_event(vk, 0, KEYUP, 0)


def main() -> int:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe = sys.argv[1] if len(sys.argv) > 1 else os.path.join(root, "target", "debug", "RunAny_RS.exe")
    if not os.path.isfile(exe):
        print(f"找不到 {exe}")
        return 2

    # 先把可能还在跑的实例收掉（全局热键同一个组合只能注册一次）
    subprocess.run(["taskkill", "/IM", "RunAny_RS.exe", "/F"], capture_output=True)
    time.sleep(0.4)

    sample = os.path.join(tempfile.gettempdir(), "runany_sel_test.txt")
    with open(sample, "w", encoding="utf-8") as handle:
        handle.write("HELLO-SELECTION-12345")

    log_path = os.path.join(tempfile.gettempdir(), "runany_capture.log")
    log = open(log_path, "w", encoding="utf-8", errors="replace")
    env = dict(os.environ, GPUI_MENU_DEBUG_CAPTURE="1")
    app = subprocess.Popen([exe], cwd=os.path.dirname(exe), stdout=log, stderr=subprocess.STDOUT, env=env)
    time.sleep(4)  # 等窗口池 / 索引起来

    notepad = subprocess.Popen(["notepad.exe", sample])
    time.sleep(2)
    # 记事本里全选（Ctrl+A）—— 选完什么都不按
    user32.keybd_event(VK_CTRL, 0, 0, 0)
    tap(VK_A)
    user32.keybd_event(VK_CTRL, 0, KEYUP, 0)
    time.sleep(0.5)

    # 关键：按住 Ctrl+Alt 不松手，敲一下 M 触发热键
    hold = float(os.environ.get("HOLD_SECONDS", "1.2"))
    user32.keybd_event(VK_CTRL, 0, 0, 0)
    user32.keybd_event(VK_ALT, 0, 0, 0)
    time.sleep(0.12)
    tap(VK_M)
    time.sleep(hold)  # 老代码就是在这里抓不到（按住 = Alt 压着 = Ctrl+Alt+C）
    user32.keybd_event(VK_CTRL, 0, KEYUP, 0)
    user32.keybd_event(VK_ALT, 0, KEYUP, 0)
    time.sleep(1.0)

    log.flush()
    app.terminate()
    notepad.terminate()
    time.sleep(0.3)

    with open(log_path, encoding="utf-8", errors="replace") as handle:
        text = handle.read()
    print(text)
    ok = "抓到选中内容" in text
    print("结果：" + ("按住修饰键也能抓到 ✅" if ok else "没抓到 ❌"))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
