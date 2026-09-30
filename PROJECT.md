# DeskUnify 项目说明

DeskUnify（桌联）是一款局域网内的键鼠、剪贴板与文件共享工具，原工作名称为 LAN Bridge。公开仓库：https://github.com/mendes556/DeskUnify。

## 范围与基础

- 基于 [Lan Mouse](https://github.com/feschber/lan-mouse)，上游基线 `f1b96bdcc0294d285bc9050b7e146800a3ef8796`，保留版权声明与 GPL-3.0-or-later 许可证。
- 目标组合为 macOS ↔ macOS、Windows ↔ Windows、Windows ↔ macOS。每轮采用一个实体输入来源；多套键鼠同时竞争控制权仍待设计。
- 复用 Rust 输入捕获/模拟和 DTLS 连接；默认运行 CLI/daemon，egui 为显式可选界面。
- 保留 crate / 二进制名称 `lan-mouse`、配置目录、证书、应用标识及现有协议，产品界面与打包名称使用 DeskUnify。

## 模块

- `input-capture` / `input-emulation` / `input-event`：平台输入与统一事件。
- `lan-mouse-proto` / `lan-mouse-ipc`：键鼠网络及本机控制协议。
- `src/discovery.rs`：mDNS 发布和扫描；公告不能替代对端指纹核对。
- `src/clipboard`：纯文本同步、并发冲突处理、原生剪贴板线程与双向认证 TLS。
- `src/files`：独立 CLI、目录流式传输、续传、校验、原生文件剪贴板与自动监听。
- `lan-mouse-cli`：后台控制、配置保存、权限和连接诊断。
- `lan-mouse-egui`：原生控制台、主题、布局、权限引导及文件任务。通过子进程复用文件 CLI。
- `lan-mouse-gtk`：保留的上游可选前端。

## 协议与运行

- 输入为 DTLS/UDP，默认 4242；文本为 TLS/TCP，同端口号。
- 文件为独立 TLS/TCP，默认 4243，ALPN `lan-bridge-files/2`；IPC 为版本 6。
- 发现为 `_lanbridge._udp.local.` / UDP 5353。授权与证书必须在双方完成，不分发私钥。
- 文件自动模式只向明确选定的设备发送启用后新复制的文件，接收完成后写入原生文件剪贴板；来源标记防回传。新复制取消旧发送，离线重试；接收期间有新剪贴板内容则保留它。
- GUI 或文件命令都不能绕过系统权限。输入权限失败不代表文件功能不可使用。

## 当前验证状态

- Apple Silicon / Intel macOS 构建、macOS 双机发现和连接、双向文件直传已验证。
- 本次 Wi-Fi 128 MiB 文件双向约 21 MiB/s；1001 个小文件约 0.39 秒，逐文件核对 SHA-256。结果受链路和磁盘条件影响。
- 核心 35 项测试通过、1 项组播忽略；egui 18 项通过。隔离 CLI 测试覆盖真实 TLS、203 个文件、恢复 2 MiB 和完整批次零数据重传。
- 两个系统私有剪贴板联合真实 TLS 验收覆盖自动发送、原生文件 URL 发布与无回传，不修改用户剪贴板。
- Windows GNU 交叉编译检查通过；Windows 真机、Finder/Explorer 完整文件粘贴、实体输入各项、断网/休眠恢复及多屏缩放仍需完整验收。
- 全功能 GTK 检查需要额外系统依赖；常用 DeskUnify 核心/egui 路径不依赖 GTK。

## 开发与发布

从仓库根目录运行命令，详见 README、DOC 与 CONTRIBUTING。首次开源以 Alpha 源码为主，安装器签名/公证和正式二进制发行待完成。公开文档使用示例地址，不包含私人调试记录、机器证书或账号密码。本地验收详情可保存在被忽略的 `PROJECT.local.md` 中，继续本机开发时可按需读取。

后续优先级：跨平台真机验收 → 输入与文件粘贴完整交互 → 稳定发行。按粘贴键触发按需文件下载、跨机剪切、图片/富文本剪贴板和多来源控制仍在范围外。
