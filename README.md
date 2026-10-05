# DeskUnify · 桌联

简体中文 | [English](README.en.md)

在局域网内共享键盘、鼠标、剪贴板与文件，让多台电脑成为一个工作空间。

[![核心检查](https://github.com/mendes556/DeskUnify/actions/workflows/core.yml/badge.svg)](https://github.com/mendes556/DeskUnify/actions/workflows/core.yml)
[![三平台打包](https://github.com/mendes556/DeskUnify/actions/workflows/packages.yml/badge.svg)](https://github.com/mendes556/DeskUnify/actions/workflows/packages.yml)
[![许可证：GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

DeskUnify 是基于 [Lan Mouse](https://github.com/feschber/lan-mouse) 开发的开源桌面共享工具，原工作名称为 **LAN Bridge**。项目新增了原生 egui 控制台、局域网发现与配对管理、文本剪贴板同步、文件传输和自动文件复制粘贴。

## 主要功能

- **跨屏键鼠**：鼠标越过配置的屏幕边缘后进入另一台电脑，键盘输入跟随鼠标。
- **扫描与配对**：通过 mDNS 发现同一局域网内的设备，核对完整指纹后授权连接。
- **原生控制台**：管理屏幕布局、共享配置、连接诊断与系统权限；支持多种主题，默认紫色渐变。
- **文本剪贴板**：可选的双向纯文本同步，使用双向认证 TLS，默认关闭。
- **文件传输**：流式发送文件和目录，支持 SHA-256 校验、中断续传与重复传输复用。
- **自动文件复制粘贴**：两端启用后，新复制的文件自动发送到选定设备，接收完成后可在对端粘贴。
- **CLI 与后台模式**：可独立使用命令行；文件传输不需要键鼠权限，也不要求键鼠后台运行。

## 下载与启动

已提供 **0.11.0 Alpha** 的三个便携版本，包含 GUI、CLI、使用说明、许可证和 SHA-256 校验文件：

- **macOS ARM64**：Apple Silicon，适用于 M 系列芯片。
- **macOS Intel x86_64**：适用于 Intel 芯片的 Mac。
- **Windows x86_64**：适用于 64 位 Windows。

打开[已通过验收的三平台构建](https://github.com/mendes556/DeskUnify/actions/runs/36758102378)，在页面底部的 **Artifacts（构建产物）** 中选择对应平台。下载通常需要登录 GitHub；先解压下载文件，再解压其中的程序 ZIP。

Mac 将 `DeskUnify.app` 放到「应用程序」后双击打开；Windows 完整解压后运行 `DeskUnify.exe`。更新前退出旧程序及后台，已有配置和配对信息继续沿用。

Mac 包使用临时签名，未经过 Apple 公证；Windows 包未签名，使用静态 MSVC 运行库。首次运行按系统提示确认，Mac 按界面引导允许辅助功能、输入监控和局域网访问。如果更新后权限开关已开但仍报未授权，请移除系统设置中的旧应用条目，再添加当前版本并重新授权，完全退出应用后重开。

构建产物保留 30 天。过期后可通过 [DeskUnify Packages 工作流](https://github.com/mendes556/DeskUnify/actions/workflows/packages.yml)重新生成；当前不会自动发布 GitHub Release。

## 当前状态

**项目处于 Alpha 阶段。** 便携版本已生成，正式签名、公证和安装器尚未完成。

- Apple Silicon 与 Intel macOS 已完成构建验证；macOS 双机连接和双向文件传输有真机验收记录。
- Windows x86_64 已通过原生 MSVC release 编译与 CLI 启动检查；Windows 实体键鼠、Windows ↔ Windows、Windows ↔ macOS 仍需真机验收。
- 自动测试覆盖 CLI 传输、双向认证 TLS、文件校验与续传、剪贴板防回传和 GUI 状态模型。原生文件剪贴板测试使用独立剪贴板，Finder / 资源管理器完整复制粘贴仍需人工验收。
- Linux 输入后端继承自 Lan Mouse；当前新增的剪贴板和文件功能面向 macOS / Windows。GTK 是保留的可选上游界面，日常桌面控制台使用 egui。
- 文件同步在复制后开始预传，大文件需等待完成再粘贴。跨机剪切、屏幕画面共享、图片和富文本剪贴板暂未实现。

## 从源码运行

在仓库根目录执行命令。需要支持 Edition 2024 的 Rust 工具链。

```sh
# 启动原生 egui 桌面界面（macOS / Windows）
cargo run --release -p lan-mouse --no-default-features --features egui --locked

# 只构建 CLI 核心并启动后台
cargo build --release -p lan-mouse --no-default-features --locked
./target/release/lan-mouse daemon
```

Windows 将可执行路径替换为 `./target/release/lan-mouse.exe`。为兼容已有配置和工具，Rust crate 与源码构建的可执行文件名称继续使用 `lan-mouse`，产品界面名称为 DeskUnify。

### 本地打包

先构建带 `egui` 功能的 release 程序，再使用 Python 3.11 或更新版本打包：

```sh
# Apple Silicon Mac
python3 scripts/package-release.py target/release/lan-mouse --platform macos-arm64

# Intel Mac
python3 scripts/package-release.py target/release/lan-mouse --platform macos-x86_64
```

Windows 使用 PowerShell 构建和打包：

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release -p lan-mouse --no-default-features --features egui --target x86_64-pc-windows-msvc --locked
python scripts/package-release.py target/x86_64-pc-windows-msvc/release/lan-mouse.exe --platform windows-x86_64
```

本地输出位于 `target/packages/`，包含程序 ZIP、校验文件、文档和源码提交信息，不包含设备配置、证书或私钥。

界面 Logo 和应用图标使用 `lan-mouse-egui/icons/` 中的资源。macOS 打包使用 `icon.icns`；Windows 的 egui 构建将 `icon.ico` 嵌入 EXE，需 Windows SDK 的资源编译器（原生 MSVC 构建环境已提供）。

## 扫描与配对

GUI 中扫描设备、核对对端完整指纹并配对，设置对方相对本机的屏幕位置。两台左右相邻的电脑应分别配置「右侧」与「左侧」。双方都需要授权对端身份。

CLI 在另一个终端中执行：

```sh
./target/release/lan-mouse cli status
./target/release/lan-mouse cli scan --wait 5
./target/release/lan-mouse cli fingerprint
./target/release/lan-mouse cli pair --fingerprint "对端完整指纹" --position right
```

把示例中的指纹替换为在对端核对过的完整指纹。macOS 键鼠采集和模拟需要系统授权；GUI 提供权限引导。文件传输不需要辅助功能或输入监控权限。

默认端口为：键鼠 **UDP 4242**，文本剪贴板 **TCP 4242**，文件 **TCP 4243**，mDNS 发现 **UDP 5353**。两端需位于同一局域网，并允许相应防火墙流量。

紧急返回本机：同时按左侧 **Ctrl + Shift + Alt/Option + Windows/Command**。

## 文件与自动复制粘贴

在 GUI 的「文件传输」页选择对端 IP 和接收目录，两台电脑分别开启「自动复制粘贴」。之后新复制的文件会自动传输，完成后在另一台电脑粘贴即可。两端程序需保持运行，大文件需等待传输完成。

CLI 等效命令如下，先完成双方身份授权，将示例 IP 换成对端地址：

```sh
./target/release/lan-mouse files sync --to 192.168.1.11:4243 --output "$HOME/Downloads/DeskUnify"
```

也可以手动传输：

```sh
# 接收端
./target/release/lan-mouse files receive --output "$HOME/Downloads/DeskUnify"

# 发送端，替换接收端地址与文件路径
./target/release/lan-mouse files send --to 192.168.1.11:4243 --mode full "/文件或目录的完整路径"
```

文件按批次保存到独立目录。中断后重新发送同一批文件可续传。每台电脑生成并使用自己的身份，不要分发私钥或复制另一台电脑的证书身份。

## 文档与参与开发

- [使用说明与双机验收](DOC.md)：CLI、配对、权限、桌面打包和文件传输详情。
- [项目架构与范围](PROJECT.md)。
- [贡献指南](CONTRIBUTING.md)。
- [上游 README](README.upstream.md)：保留的 Lan Mouse 文档，其中安装和下载链接指向上游项目。

配置目录、证书、macOS 应用标识、mDNS 服务和网络协议标识保留原有名称，避免改名引发配对迁移。详见[名称与兼容性](DOC.md#名称与兼容性)。

## 许可证与上游致谢

DeskUnify 基于 **Lan Mouse**，上游基线提交为 `f1b96bdcc0294d285bc9050b7e146800a3ef8796`，保留原版权与许可证声明。DeskUnify 在 2026 年 9–10 月新增了 egui 界面、发现与配对管理、剪贴板和文件同步、CLI 诊断及部署工具。

项目使用 **GPL-3.0-or-later**，详见 [LICENSE](LICENSE)。对应源码、构建和打包脚本均保存在本仓库。旧上游工作流归档在 `docs/upstream/`；当前核心检查与三平台打包分别使用 `.github/workflows/core.yml` 和 `.github/workflows/packages.yml`。
