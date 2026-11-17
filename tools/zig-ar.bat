@echo off
REM Zig 归档器（ar）包装脚本（Windows）。
REM 需要本机安装 zig。请按实际安装路径设置环境变量，或直接改下面一行：
REM   set "ZIG=C:\path\to\zig.exe"
REM 若未设置，则回退到 PATH 中的 zig。
setlocal
if not defined ZIG set "ZIG=zig"

"%ZIG%" ar %*
