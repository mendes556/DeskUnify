#!/usr/bin/env python3
# DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
"""Package a native egui release binary, documentation and source provenance."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tomllib
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", type=Path)
parser.add_argument("--platform", required=True, choices=["macos-arm64", "macos-x86_64", "windows-x86_64"])
parser.add_argument("--output-dir", type=Path, default=Path("target/packages"))
args = parser.parse_args()
binary = args.binary.resolve()
version = tomllib.loads(Path("Cargo.toml").read_text())["package"]["version"]
commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
name = f"DeskUnify-{version}-{args.platform}"
folder = args.output_dir.resolve() / name
folder.mkdir(parents=True, exist_ok=False)

if args.platform.startswith("macos-"):
    if sys.platform != "darwin":
        parser.error("macOS app packaging requires macOS")
    arch = "arm64" if args.platform == "macos-arm64" else "x86_64"
    actual = subprocess.check_output(["lipo", "-archs", str(binary)], text=True).strip()
    if actual != arch:
        parser.error(f"expected {arch}, got {actual}")
    subprocess.run([sys.executable, "scripts/package-desktop.py", str(binary), "--output", str(folder / "DeskUnify.app")], check=True)
    executable = folder / "DeskUnify.app/Contents/MacOS/lan-mouse"
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(folder / "DeskUnify.app")], check=True)
    launch = "解压后将 DeskUnify.app 放到‘应用程序’，双击启动。CLI 位于 DeskUnify.app/Contents/MacOS/lan-mouse。\n首次打开按 macOS 安全提示确认；这是临时签名包，未经过 Apple 公证。\n在系统设置→隐私与安全性中允许 DeskUnify 的辅助功能、输入监控和局域网访问，然后按界面提示重新检测或重启。"
else:
    if sys.platform != "win32":
        parser.error("Windows release packaging requires Windows")
    with binary.open("rb") as source:
        if source.read(2) != b"MZ":
            parser.error("not a Windows executable")
        source.seek(0x3C)
        source.seek(struct.unpack("<I", source.read(4))[0])
        if source.read(4) != b"PE\0\0" or struct.unpack("<H", source.read(2))[0] != 0x8664:
            parser.error("expected a Windows x86_64 PE executable")
    executable = folder / "DeskUnify.exe"
    shutil.copy2(binary, executable)
    launch = "完整解压文件夹，双击 DeskUnify.exe 启动 GUI。相同程序支持 CLI，例如 .\\DeskUnify.exe cli status。\n这是未签名的便携包；首次启动按 Windows 安全提示确认，并允许专用网络防火墙访问。普通权限启动；如需控制以管理员身份运行的程序，两端的权限级别需匹配。"

for doc in ["LICENSE", "README.md", "DOC.md"]:
    shutil.copy2(doc, folder / doc)
(folder / "使用说明.txt").write_text(
    f"DeskUnify {version} — {args.platform}\n\n{launch}\n\n"
    "两台电脑使用同一版本。扫描设备，双方核对完整指纹并配对，设置屏幕相对位置。\n"
    "文本剪贴板默认关闭，按需要启用。文件页选择对端和接收目录，两端开启自动复制粘贴后，新复制的文件自动传输；大文件等待完成再粘贴。\n"
    "默认端口：键鼠 UDP 4242，文本 TCP 4242，文件 TCP 4243，发现 UDP 5353。\n"
    "紧急返回本机：同时按左侧 Ctrl、Shift、Alt/Option、Windows/Command。\n"
    "这是 Alpha 版本。macOS 双机有实测记录；Windows 原生构建与自动测试不代表实体键鼠和资源管理器粘贴已经验收。\n"
    "包不包含设备配置和私钥；每台机器首次运行生成自己的身份。更新前先退出旧程序。\n"
    f"源码：https://github.com/mendes556/DeskUnify/tree/{commit}\n"
    "许可证：GPL-3.0-or-later；基于 Lan Mouse，版权声明见 LICENSE 与 README.md。\n",
    encoding="utf-8",
)
manifest = {
    "product": "DeskUnify", "version": version, "platform": args.platform,
    "source_commit": commit, "features": ["egui"],
    "executable": str(executable.relative_to(folder)).replace("\\", "/"),
    "executable_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
    "signing": "ad-hoc, not notarized" if sys.platform == "darwin" else "unsigned, static CRT",
}
(folder / "build-info.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
archive = folder.parent / f"{folder.name}.zip"
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
    for path in sorted(folder.rglob("*")):
        if path.is_file():
            output.write(path, path.relative_to(folder.parent))
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix(".zip.sha256").write_text(f"{checksum}  {archive.name}\n", encoding="utf-8")
print(archive)
