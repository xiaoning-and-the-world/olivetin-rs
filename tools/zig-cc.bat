@echo off
REM Zig C 编译器包装脚本（Windows）。
REM 需要本机安装 zig 与 python。请按实际安装路径设置环境变量，或直接改下面两行：
REM   set "PYTHON=C:\path\to\python.exe"
REM   set "ZIG=C:\path\to\zig.exe"
REM 若未设置，则回退到 PATH 中的 python/zig。
setlocal
if not defined PYTHON set "PYTHON=python"

"%PYTHON%" "%~dp0zig-cc.py" %*
