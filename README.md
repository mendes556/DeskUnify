# DeskUnify · 桌联

Share your keyboard, mouse, clipboard and files across computers on the same local network.

在局域网内共享键鼠、剪贴板与文件，让多台电脑成为一个工作空间。

[![Core CI](https://github.com/mendes556/DeskUnify/actions/workflows/core.yml/badge.svg)](https://github.com/mendes556/DeskUnify/actions/workflows/core.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

DeskUnify is an early development fork of [Lan Mouse](https://github.com/feschber/lan-mouse). It adds a native egui control panel, LAN discovery and pairing management, text clipboard synchronization, and file transfer with automatic file-copy synchronization. The project previously used the working name **LAN Bridge**.

## Features

- Move the pointer across a configured screen edge; keyboard input follows it.
- Discover nearby peers with mDNS, verify their fingerprints, and manage paired devices.
- Native desktop UI with screen layout, diagnostics, permission guidance and multiple themes.
- Optional bidirectional plain-text clipboard synchronization over mutually authenticated TLS.
- Stream files and directories over a separate TLS connection, with SHA-256 verification and resumable transfers.
- Enable automatic file copying once: newly copied files transfer to one selected peer and become pasteable after receipt.
- CLI/daemon operation without the GUI; file commands work independently of input permissions.

## Status

**Alpha / development software.** Source builds and portable packaging are available; no official signed or notarized installer has been published.

- Apple Silicon and Intel macOS builds have been verified. macOS-to-macOS connections and bidirectional file transfers have been exercised on real devices.
- Windows code has passed a GNU cross-compilation check. Windows hardware, Windows-to-Windows and Windows-to-macOS interaction still need validation.
- Automated tests cover CLI transport, paired TLS, file integrity/resume, clipboard loop prevention and the GUI model. Automated native file-clipboard tests use private pasteboards; complete Finder/Explorer copy-and-paste interaction remains a manual acceptance item.
- Linux backends are inherited from Lan Mouse; the current DeskUnify clipboard and file features target macOS/Windows. GTK is optional and is not the DeskUnify desktop interface.
- File synchronization starts after copying, rather than intercepting the paste key. Wait for completion before pasting large files. Cross-machine cut, screen/video sharing and rich/image clipboard synchronization are not implemented.

## Build and run

Run commands from the repository root. A Rust toolchain supporting Edition 2024 is needed for workspace dependencies.

```sh
# Native egui desktop interface on macOS / Windows
cargo run --release -p lan-mouse --no-default-features --features egui --locked

# CLI-only core
cargo build --release -p lan-mouse --no-default-features --locked
./target/release/lan-mouse daemon
```

On Windows, use `./target/release/lan-mouse.exe`. The Rust crate and executable names remain `lan-mouse` for compatibility; the application name is DeskUnify.

### Portable desktop packages

The manually triggered [DeskUnify Packages workflow](https://github.com/mendes556/DeskUnify/actions/workflows/packages.yml) builds macOS ARM64, macOS Intel x86_64 and Windows x86_64 packages on native runners. Each artifact contains an application ZIP and its SHA-256 checksum; downloads are retained for 30 days. The ZIP includes the GUI/CLI, license, usage documentation and source commit metadata. macOS apps use ad-hoc signing without notarization; Windows executables are unsigned and built with the static MSVC runtime.

To package a native release build locally, use Python 3.11 or newer:

```sh
python3 scripts/package-release.py target/release/lan-mouse --platform macos-arm64
# Intel Mac: --platform macos-x86_64
# Windows: python scripts/package-release.py target/x86_64-pc-windows-msvc/release/lan-mouse.exe --platform windows-x86_64
```

On Windows, build with `--target x86_64-pc-windows-msvc` and `RUSTFLAGS="-C target-feature=+crt-static"`, as in the workflow. Outputs go to `target/packages/`. Download and extract the matching platform ZIP; on macOS move DeskUnify.app to Applications, and on Windows run DeskUnify.exe from the extracted folder. Exit an old instance before updating; existing pairing/configuration is retained.

In another terminal:

```sh
./target/release/lan-mouse cli status
./target/release/lan-mouse cli scan --wait 5
./target/release/lan-mouse cli fingerprint
./target/release/lan-mouse cli pair --fingerprint "VERIFIED_PEER_FINGERPRINT" --position right
```

Verify each fingerprint on its source machine and authorize both directions. macOS input capture/emulation requires the relevant Accessibility and Input Monitoring permissions; the GUI provides guidance. File transfer does not require keyboard/mouse permissions. The default ports are UDP 4242 for input, TCP 4242 for optional text clipboard synchronization, TCP 4243 for files, and UDP 5353 for mDNS.

### Files and automatic copy/paste

In the GUI, open **文件传输 / File transfer**, choose the peer IP and receive folder, and enable **开启自动复制粘贴** on each computer. Subsequent file copies transfer automatically; wait for completion and paste on the other computer. Keep both programs running.

CLI equivalent, with the existing paired configuration on each computer:

```sh
./target/release/lan-mouse files sync --to PEER_IP:4243 --output "$HOME/Downloads/DeskUnify"
```

Manual transfer remains available:

```sh
# Receiver
./target/release/lan-mouse files receive --output "$HOME/Downloads/DeskUnify"
# Sender
./target/release/lan-mouse files send --to PEER_IP:4243 --mode full "/path/to/file-or-directory"
```

Files are saved in independent batch directories. Cancelled transfers can resume by sending the same source selection again. Never distribute your device certificate/private key or copy another machine's identity.

## Documentation and development

- [使用说明与双机验收](DOC.md): CLI, pairing, permissions, desktop packaging and file-transfer details (Chinese).
- [Project architecture and scope](PROJECT.md).
- [Contribution guide](CONTRIBUTING.md).
- [Upstream README](README.upstream.md): retained Lan Mouse documentation; its installation/download links refer to the upstream project.

The configuration directory, certificates, macOS application IDs, mDNS service and wire protocol identifiers retain their existing names so a product rename does not require a pairing migration. See [compatibility notes](DOC.md#名称与兼容性).

## License and upstream attribution

DeskUnify is a modified version of **Lan Mouse**, based on commit `f1b96bdcc0294d285bc9050b7e146800a3ef8796`. Original upstream copyright and license notices are retained. DeskUnify changes include egui UI, discovery/pairing controls, clipboard/file synchronization, CLI diagnostics and deployment helpers, developed September–October 2026.

Distributed under **GPL-3.0-or-later**. See [LICENSE](LICENSE). Corresponding source and build/package scripts are included in this repository. Legacy upstream automation is archived in `docs/upstream/`; active DeskUnify CI is `.github/workflows/core.yml`.
