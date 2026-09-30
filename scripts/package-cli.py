#!/usr/bin/env python3
# DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
"""Package a GUI-free macOS executable and Terminal launchers. No identity is copied."""
import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", type=Path)
parser.add_argument("--arch", required=True, choices=["Intel", "AppleSilicon"])
parser.add_argument("--peer-ip")
parser.add_argument("--peer-fingerprint")
parser.add_argument("--position", choices=["left", "right", "top", "bottom"], default="right")
args = parser.parse_args()
if sys.platform != "darwin":
    parser.error("macOS packaging only")
if bool(args.peer_ip) != bool(args.peer_fingerprint):
    parser.error("peer IP and verified fingerprint must be provided together")
if args.peer_ip:
    import ipaddress
    import re
    ipaddress.ip_address(args.peer_ip)
    if not re.fullmatch(r"(?:[0-9a-fA-F]{2}:){31}[0-9a-fA-F]{2}", args.peer_fingerprint):
        parser.error("invalid SHA-256 fingerprint")
name = f"DeskUnify-CLI-macos-{args.arch}"
folder = Path("target") / name
folder.mkdir(parents=True, exist_ok=True)
binary = folder / "lan-mouse"
shutil.copy2(args.binary.resolve(), binary)
binary.chmod(0o755)
subprocess.run(["codesign", "--force", "--sign", "-", "--identifier", "dev.lanbridge.cli", str(binary)], check=True)
subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
files = [binary]


def launcher(name, body, pause=True):
    path = folder / name
    path.write_text('#!/bin/zsh\nset -eu\ntask_dir="${0:A:h}"\n' + body + ('\nread -r "?按回车关闭此窗口…"\n' if pause else "\n"))
    path.chmod(0o755)
    subprocess.run(["zsh", "-n", str(path)], check=True)
    files.append(path)


launcher("start.command", '\n"$task_dir/lan-mouse" cli permissions\nprint "后台将保持运行。另开终端控制它；Ctrl+C 可正常退出。"\nexec "$task_dir/lan-mouse" daemon', pause=False)
launcher("permissions.command", '"$task_dir/lan-mouse" cli permissions --request\nprint "在系统设置中授权 Terminal 的辅助功能和输入监控。授权后完全退出 Terminal，重新打开并再次检查。"')
launcher("status.command", '"$task_dir/lan-mouse" cli status\n"$task_dir/lan-mouse" cli fingerprint\n"$task_dir/lan-mouse" cli doctor || true')
launcher("scan.command", '"$task_dir/lan-mouse" cli scan --wait 5')
launcher("stop.command", '"$task_dir/lan-mouse" cli shutdown')
peer_note = ""
if args.peer_ip:
    launcher("pair-with-main.command", f'print "添加并授权主机 {args.peer_ip}，相对位置 {args.position}。"\nprint "指纹：{args.peer_fingerprint}"\n"$task_dir/lan-mouse" cli add-client --ips {args.peer_ip} --position {args.position} --fingerprint "{args.peer_fingerprint}"')
    peer_note = f"\n本包 pair-with-main.command 已填写主机 {args.peer_ip} 的公开指纹：\n{args.peer_fingerprint}\n先在主机 cli fingerprint 核对；双击后添加并授权主机，位置 {args.position}。重复执行会重复添加配置，请只运行一次。主机仍须授权本机的指纹。\n"
readme = folder / "使用说明.txt"
readme.write_text(f"""DeskUnify CLI — {args.arch}

无需安装 Rust。将整个文件夹复制到对应架构的 Mac，解压后执行。

1. 双击 permissions.command，在系统设置→隐私与安全性中允许 Terminal 的辅助功能和输入监控权限。
   首次请求辅助功能；授权并完全退出 Terminal 后再次请求输入监控。此脚本不会替你授权。
2. 双击 start.command，保持后台终端运行。若已有旧后台，先从旧程序正常退出。
3. 双击 status.command，查看原生后端、公开指纹和连接诊断。dummy 或 Disabled 不算真实键鼠就绪。
4. 双击 scan.command，发现其他运行 DeskUnify 的设备；允许局域网访问和 UDP 5353。
5. 在双方核对完整指纹后配对。另开终端，把本目录的 lan-mouse 拖入窗口，再输入命令，例如：
   /完整路径/lan-mouse cli pair --fingerprint "对端完整指纹" --position {args.position}
   扫描不可用时：
   /完整路径/lan-mouse cli add-client --ips 对端IP --position {args.position} --fingerprint "对端完整指纹"
{peer_note}
配置操作自动保存到 ~/.config/lan-mouse/config.toml。每台电脑单独生成自己的证书，不随包分发。
允许输入端口 UDP 4242；启用双向文本剪贴板时还需 TCP 4242，并在两端执行 cli clipboard true。
屏幕位置是对方相对本机的位置，左右相邻的两台应分别配置 right 和 left。
cli doctor --local 检查本机；cli doctor 还要求启用设备已连通，仍需实际跨屏测试。
cli pause / cli resume 暂停与恢复；cli release 返回本机；stop.command 正常停止后台。
紧急释放：同时按左侧 Ctrl + Shift + Option + Command。

Mac 的安全提示若阻止首次打开，可在 Finder 对启动脚本右键→打开，按系统提示确认。此包为本地临时签名，没有 Apple 公证。
项目许可证见 LICENSE，完整操作说明见 DOC.md。
""")
files.append(readme)
for name in ["LICENSE", "README.md", "DOC.md"]:
    output = folder / name
    shutil.copy2(name, output)
    files.append(output)
archive = folder.with_suffix(".zip")
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zipped:
    for path in files:
        zipped.write(path, arcname=f"{folder.name}/{path.name}")
print(archive.resolve())
