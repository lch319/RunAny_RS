@echo off
rem ------------------------------------------------------------------
rem  热字符串调试启动器
rem
rem  双击本文件 = 带上 GPUI_MENU_DEBUG_HS=1 启动 RunAny_RS.exe，
rem  敲字时物理按键、缓冲区、命中、退格数会写进同目录的
rem  hotstring-debug.log（每次启动重写）。
rem
rem  注意：先退出正在运行的 RunAny（托盘图标右键 -> 退出），
rem  否则新进程会直接让位给老进程，日志不会更新。
rem ------------------------------------------------------------------
setlocal
cd /d "%~dp0"
set GPUI_MENU_DEBUG_HS=1
start "" "%~dp0RunAny_RS.exe"
