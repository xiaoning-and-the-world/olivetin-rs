"""aarch64-unknown-linux-musl 交叉编译的 zig cc 包装（Windows 宿主）。

相比旧版 zig-cc.py 修复两点：
1. rustc 1.99 在 Windows 上把链接参数写进 @响应文件，旧脚本只过滤 argv，
   导致 --fix-cortex-a53-843419 原样传给 lld 而链接失败；
   这里展开响应文件后再过滤，并把过滤后的参数写回响应文件传给 zig cc，
   同时规避 Windows 命令行 32767 字符上限。
2. cmd 会把 "-Wl,--fix-cortex-a53-843419" 按逗号拆成两个参数，两种形态都过滤。
"""
import os
import subprocess
import sys
import tempfile

# zig 可执行文件路径：优先读环境变量 ZIG，未设置则回退到 PATH 中的 zig。
ZIG = os.environ.get("ZIG", "zig")


def expand_response_files(raw):
    out = []
    for a in raw:
        if a.startswith("@") and os.path.isfile(a[1:]):
            with open(a[1:], "r", encoding="utf-8", errors="replace") as f:
                for line in f:
                    s = line.strip()
                    if not s:
                        continue
                    if len(s) >= 2 and s[0] == '"' and s[-1] == '"':
                        s = s[1:-1]
                    out.append(s)
        else:
            out.append(a)
    return out


args = expand_response_files(sys.argv[1:])
target_arg = "aarch64-linux-musl"
filtered = []
i = 0
while i < len(args):
    a = args[i]
    if a.startswith("--target="):
        triple = a.split("=", 1)[1]
        parts = triple.split("-")
        target_arg = (
            parts[0] + "-" + parts[2] + "-" + parts[3]
            if len(parts) == 4 and parts[1] == "unknown"
            else triple
        )
        i += 1
        continue
    if a == "-target" and i + 1 < len(args):
        i += 2
        continue
    if "fix-cortex-a53-843419" in a:
        i += 1
        continue
    if a in ("-Wl", "-Wl,"):
        i += 1
        continue
    # rustc 自带的 crt 与 zig 内置 musl crt 冲突
    if "self-contained" in a and ("crt" in a or "crtbegin" in a or "crtend" in a):
        i += 1
        continue
    filtered.append(a)
    i += 1

rsp = os.path.join(tempfile.gettempdir(), "olivetin-zig-link.rsp")
with open(rsp, "w", encoding="utf-8") as f:
    for a in filtered:
        if any(c.isspace() for c in a):
            f.write('"' + a.replace('"', '\\"') + '"\n')
        else:
            f.write(a + "\n")

sys.exit(subprocess.run([ZIG, "cc", "-target", target_arg, "@" + rsp]).returncode)
