// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
mod privacy;
use clap::{Args, Parser, Subcommand, ValueEnum};
use futures::{Stream, StreamExt};
use lan_mouse_ipc::{
    ClientConfig, ClientHandle, ConnectionError, DiscoveredDevice, FrontendEvent, FrontendRequest,
    IPC_VERSION, IpcError, Position, UiAction, UiSnapshot, connect_async,
};
use serde::Serialize;
use std::{
    io::{self, Write},
    net::IpAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("无法连接本机后台：{0}。请先在另一终端运行 lan-mouse daemon")]
    ServiceNotRunning(#[from] ConnectionError),
    #[error("后台通信失败：{0}")]
    Ipc(#[from] IpcError),
    #[error("后台在确认操作之前断开连接")]
    ServiceDisconnected,
    #[error("后台未在时限内确认；操作可能已生效，请用 cli status 核对")]
    Timeout,
    #[error("后台操作失败：{0}")]
    ServiceError(String),
    #[error(
        "CLI/后台 IPC 版本不一致：CLI {expected}，后台 {actual}。请先退出旧后台，再启动同版本 daemon"
    )]
    Version { expected: u32, actual: u32 },
    #[error("{0}")]
    Invalid(String),
    #[error("键鼠共享尚未就绪，请按 doctor 的诊断处理")]
    NotReady,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Output(#[from] io::Error),
}

#[derive(Parser, Clone, Debug, PartialEq, Eq)]
#[command(
    name = "lan-mouse-cli",
    about = "DeskUnify CLI：扫描、配对、配置和键鼠共享控制"
)]
pub struct CliArgs {
    /// Output machine-readable JSON
    #[arg(long, global = true)]
    json: bool,
    /// IPC confirmation timeout in seconds
    #[arg(long, global=true, default_value_t=10, value_parser=clap::value_parser!(u64).range(1..=120))]
    timeout: u64,
    #[command(subcommand)]
    command: CliSubcommand,
}
#[derive(Args, Clone, Debug, PartialEq, Eq)]
struct Client {
    #[arg(long)]
    hostname: Option<String>,
    #[arg(long, default_value_t = 4242)]
    port: u16,
    #[arg(long, num_args=1.., value_delimiter=',')]
    ips: Vec<IpAddr>,
    #[arg(long, alias = "pos", default_value = "right")]
    position: Position,
    /// Peer SHA-256 certificate fingerprint; verify on peer before authorizing
    #[arg(long)]
    fingerprint: Option<String>,
    #[arg(long)]
    enter_hook: Option<String>,
    #[arg(long)]
    leave_hook: Option<String>,
}
#[derive(Clone, Copy, ValueEnum, Debug, PartialEq, Eq)]
enum PermissionPane {
    Accessibility,
    InputMonitoring,
    LocalNetwork,
}
#[derive(Clone, Subcommand, Debug, PartialEq, Eq)]
enum CliSubcommand {
    /// Print local public certificate fingerprint
    Fingerprint,
    /// Print real backend, connection and configuration status
    Status,
    /// Check native input readiness; --local skips peer connectivity check
    Doctor {
        #[arg(long)]
        local: bool,
    },
    /// Discover other DeskUnify computers via mDNS (not arbitrary LAN hosts)
    Scan {
        #[arg(long,default_value_t=3,value_parser=clap::value_parser!(u64).range(1..=30))]
        wait: u64,
    },
    /// Add and authorize a discovered peer after checking its fingerprint
    Pair {
        #[arg(long)]
        fingerprint: String,
        #[arg(long, default_value = "right")]
        position: Position,
        #[arg(long,default_value_t=3,value_parser=clap::value_parser!(u64).range(1..=30))]
        wait: u64,
    },
    /// Add, activate and persist a manually configured peer
    AddClient(Client),
    RemoveClient {
        id: ClientHandle,
    },
    Activate {
        id: ClientHandle,
    },
    Deactivate {
        id: ClientHandle,
    },
    List,
    SetHost {
        id: ClientHandle,
        host: Option<String>,
    },
    SetPort {
        id: ClientHandle,
        port: u16,
    },
    SetPosition {
        id: ClientHandle,
        pos: Position,
    },
    SetIps {
        id: ClientHandle,
        #[arg(num_args=0..,value_delimiter=',')]
        ips: Vec<IpAddr>,
    },
    /// Set or clear entry/exit hooks (omitted hooks are cleared)
    SetHooks {
        id: ClientHandle,
        #[arg(long)]
        enter_hook: Option<String>,
        #[arg(long)]
        leave_hook: Option<String>,
    },
    /// Retry both native backends and wait for actual readiness
    #[command(alias = "enable-capture", alias = "enable-emulation")]
    RetryBackends {
        #[arg(long,default_value_t=3,value_parser=clap::value_parser!(u64).range(1..=30))]
        wait: u64,
    },
    #[command(alias = "authorize")]
    AuthorizeKey {
        description: String,
        sha256_fingerprint: String,
    },
    #[command(alias = "revoke")]
    RemoveAuthorizedKey {
        sha256_fingerprint: String,
    },
    /// List authorized peer fingerprints
    Authorized,
    /// Show settings or change and persist selected settings
    Settings {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        clipboard: Option<bool>,
    },
    /// Enable or disable automatic plain-text clipboard synchronization
    Clipboard {
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
    /// Pause input send/receive and clipboard synchronization
    Pause,
    Resume,
    /// Release outgoing capture and return control to this computer
    Release,
    SaveConfig,
    /// Gracefully stop the daemon and release its input state
    Shutdown,
    /// macOS permission preflight, explicit request or settings shortcut
    Permissions {
        #[arg(long, conflicts_with = "open")]
        request: bool,
        #[arg(long)]
        open: Option<PermissionPane>,
    },
}

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);
async fn request(action: UiAction, timeout: Duration) -> Result<UiSnapshot, CliError> {
    let id = format!(
        "cli-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    tokio::time::timeout(timeout, async {
        let (mut events, mut requests) =
            connect_async(Some(Duration::from_secs(2).min(timeout))).await?;
        requests
            .request(FrontendRequest::Ui {
                id: id.clone(),
                action,
            })
            .await?;
        read_result(&mut events, &id).await
    })
    .await
    .map_err(|_| CliError::Timeout)?
}
async fn read_result(
    events: &mut (impl Stream<Item = Result<FrontendEvent, IpcError>> + Unpin),
    id: &str,
) -> Result<UiSnapshot, CliError> {
    while let Some(event) = events.next().await {
        if let FrontendEvent::UiResult {
            id: response_id,
            result,
        } = event?
        {
            if response_id == id {
                let snapshot = result.map_err(CliError::ServiceError)?;
                if snapshot.protocol_version != IPC_VERSION {
                    return Err(CliError::Version {
                        expected: IPC_VERSION,
                        actual: snapshot.protocol_version,
                    });
                }
                return Ok(snapshot);
            }
        }
    }
    Err(CliError::ServiceDisconnected)
}
fn json<T: Serialize>(value: &T) -> Result<(), CliError> {
    writeln!(
        io::stdout().lock(),
        "{}",
        serde_json::to_string_pretty(value)?
    )?;
    Ok(())
}
fn text(value: impl std::fmt::Display) -> Result<(), CliError> {
    writeln!(io::stdout().lock(), "{value}")?;
    Ok(())
}
fn display_name(config: &ClientConfig) -> String {
    clean(config.hostname.as_deref().unwrap_or("未命名"))
}
fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}
fn native_ready(s: &UiSnapshot) -> bool {
    s.native_ready()
}
fn status(s: &UiSnapshot) -> Result<(), CliError> {
    text(format!(
        "平台 {} · IPC {} · 监听端口 {} · {}",
        s.platform,
        s.protocol_version,
        s.port,
        if s.paused { "已暂停" } else { "未暂停" }
    ))?;
    for (label, state, backend, error) in [
        ("输入采集", s.capture, &s.capture_backend, &s.capture_error),
        (
            "输入模拟",
            s.emulation,
            &s.emulation_backend,
            &s.emulation_error,
        ),
    ] {
        text(format!(
            "{label}: {state:?} · {}",
            backend.as_deref().unwrap_or("尚未就绪")
        ))?;
        if backend.as_deref() == Some("dummy") {
            text("  测试虚拟后端，不能共享真实键鼠")?;
        }
        if let Some(error) = error {
            text(format!("  {error}"))?;
        }
    }
    text(format!(
        "设备 {} · 已连接 {} · 授权 {} · 剪贴板 {}",
        s.clients.len(),
        s.clients.iter().filter(|(_, _, s)| s.alive).count(),
        s.authorized.len(),
        s.clipboard
    ))?;
    text(format!("释放快捷键: {}", s.release_bind.join(" + ")))?;
    text(format!("配置文件: {}", s.config_path))?;
    if let Some(error) = &s.discovery_error {
        text(format!("发现错误: {error}"))?;
    }
    Ok(())
}
fn list(s: &UiSnapshot) -> Result<(), CliError> {
    if s.clients.is_empty() {
        text("未配置设备")?;
    }
    for (id, c, state) in &s.clients {
        text(format!(
            "id {id}: {}:{} ({}) active={} connected={} ips={:?}",
            display_name(c),
            c.port,
            c.pos,
            state.active,
            state.alive,
            c.fix_ips
        ))?;
    }
    Ok(())
}
async fn scan(wait: u64, timeout: Duration) -> Result<UiSnapshot, CliError> {
    let mut snapshot = request(UiAction::Scan, timeout).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
        snapshot = request(UiAction::Snapshot, timeout).await?;
        if let Some(error) = &snapshot.discovery_error {
            return Err(CliError::ServiceError(error.clone()));
        }
    }
    snapshot
        .discovered
        .sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    Ok(snapshot)
}
fn peer_action(peer: &DiscoveredDevice, position: Position) -> UiAction {
    let hostname = (!peer.name.is_empty()
        && peer.name.len() <= 253
        && !peer
            .name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control()))
    .then(|| peer.name.clone());
    UiAction::AddClient {
        hostname,
        ips: peer.ips.clone(),
        port: peer.port,
        position,
        fingerprint: Some(peer.fingerprint.clone()),
        enter_hook: None,
        leave_hook: None,
    }
}
fn doctor(s: &UiSnapshot, local: bool) -> Result<(), CliError> {
    if !native_ready(s) {
        text(if s.platform == "macos" {
            "原生键鼠后端尚未就绪。请允许启动 CLI 的终端 App 的辅助功能和输入监控权限，完全退出后台后重新启动；可用 cli permissions 查看/请求权限。"
        } else {
            "原生键鼠后端尚未就绪。请按上方后端错误处理，再运行 cli retry-backends 或重启后台。"
        })?;
        return Err(CliError::NotReady);
    }
    if s.paused {
        text("当前已暂停，请运行 cli resume")?;
        return Err(CliError::NotReady);
    }
    if !local && !s.clients.iter().any(|(_, _, s)| s.active && s.alive) {
        text("没有已连接的启用设备。两端运行 daemon，核对双方指纹并相互授权后再检查。")?;
        return Err(CliError::NotReady);
    }
    text(if local {
        "本机原生键鼠后端就绪；这不代表双机输入已实测通过"
    } else {
        "原生键鼠后端和设备连接已就绪；请跨边缘实测键盘、鼠标、滚轮和紧急释放"
    })
}

pub async fn run(args: CliArgs) -> Result<(), CliError> {
    if let CliSubcommand::Permissions {
        request: prompt,
        open,
    } = args.command
    {
        if let Some(pane) = open {
            let pane = match pane {
                PermissionPane::Accessibility => privacy::PrivacyPane::Accessibility,
                PermissionPane::InputMonitoring => privacy::PrivacyPane::InputMonitoring,
                PermissionPane::LocalNetwork => privacy::PrivacyPane::LocalNetwork,
            };
            privacy::open(pane).map_err(CliError::Invalid)?;
        }
        let s = if prompt {
            privacy::request().map_err(CliError::Invalid)?
        } else {
            privacy::status()
        };
        if args.json {
            json(&s)?;
        } else if s.supported {
            text(format!(
                "辅助功能={} 输入监控={} 输入控制={}",
                s.accessibility, s.input_monitoring, s.input_control
            ))?;
            text(
                "CLI 模式请授权启动它的终端 App（如 Terminal/iTerm），授权后重启后台。首次请求辅助功能，重启后再次请求输入监控/输入控制。",
            )?;
        } else {
            text("此系统不使用 macOS TCC 权限；请检查本机原生后端与防火墙，运行 cli doctor。")?;
        }
        return Ok(());
    }
    let timeout = Duration::from_secs(args.timeout);
    // Check schema before mutating an older daemon.
    let snapshot = request(UiAction::Snapshot, timeout).await?;
    let action = match args.command {
        CliSubcommand::Fingerprint => {
            if args.json {
                json(&snapshot.fingerprint)?;
            } else {
                text(&snapshot.fingerprint)?;
            }
            return Ok(());
        }
        CliSubcommand::Status => {
            if args.json {
                json(&snapshot)?;
            } else {
                status(&snapshot)?;
            }
            return Ok(());
        }
        CliSubcommand::Doctor { local } => {
            if args.json {
                json(&snapshot)?;
                if !native_ready(&snapshot)
                    || snapshot.paused
                    || (!local && !snapshot.clients.iter().any(|(_, _, s)| s.active && s.alive))
                {
                    return Err(CliError::NotReady);
                }
                return Ok(());
            }
            status(&snapshot)?;
            return doctor(&snapshot, local);
        }
        CliSubcommand::Scan { wait } => {
            let snapshot = scan(wait, timeout).await?;
            if args.json {
                json(&snapshot.discovered)?;
            } else {
                for peer in &snapshot.discovered {
                    text(format!(
                        "{} · {} · {:?}:{}\n  fingerprint {}",
                        clean(&peer.name),
                        clean(&peer.platform),
                        peer.ips,
                        peer.port,
                        peer.fingerprint
                    ))?;
                }
                if snapshot.discovered.is_empty() {
                    text(
                        "未发现其他 DeskUnify。已过滤本机；另一台电脑须运行同版本 daemon，允许 mDNS UDP 5353 和本地网络访问。",
                    )?;
                }
            }
            return Ok(());
        }
        CliSubcommand::Pair {
            fingerprint,
            position,
            wait,
        } => {
            let discovered = scan(wait, timeout).await?;
            let peer=discovered.discovered.iter().find(|p|p.fingerprint.eq_ignore_ascii_case(fingerprint.trim())).ok_or_else(||CliError::Invalid("扫描未找到此指纹。核对对端 daemon 和局域网；也可用 add-client --ips ... --fingerprint ... 手工添加".into()))?;
            if discovered.clients.iter().any(|(_, c, _)| {
                c.port == peer.port && c.fix_ips.iter().any(|ip| peer.ips.contains(ip))
            }) {
                return Err(CliError::Invalid(
                    "此设备已有配置，请用 authorize-key 和 set-position 更新，避免重复添加".into(),
                ));
            }
            peer_action(peer, position)
        }
        CliSubcommand::AddClient(client) => UiAction::AddClient {
            hostname: client.hostname,
            ips: client.ips,
            port: client.port,
            position: client.position,
            fingerprint: client.fingerprint,
            enter_hook: client.enter_hook,
            leave_hook: client.leave_hook,
        },
        CliSubcommand::RemoveClient { id } => UiAction::RemoveClient { id },
        CliSubcommand::Activate { id } => UiAction::SetActive { id, active: true },
        CliSubcommand::Deactivate { id } => UiAction::SetActive { id, active: false },
        CliSubcommand::List => {
            if args.json {
                json(&snapshot.clients)?;
            } else {
                list(&snapshot)?;
            }
            return Ok(());
        }
        CliSubcommand::SetHost { id, host } => UiAction::SetHostname { id, hostname: host },
        CliSubcommand::SetPort { id, port } => UiAction::SetClientPort { id, port },
        CliSubcommand::SetIps { id, ips } => UiAction::SetClientIps { id, ips },
        CliSubcommand::SetPosition { id, pos } => UiAction::SetPosition { id, position: pos },
        CliSubcommand::SetHooks {
            id,
            enter_hook,
            leave_hook,
        } => UiAction::SetHooks {
            id,
            enter_hook,
            leave_hook,
        },
        CliSubcommand::RetryBackends { wait } => {
            request(UiAction::RetryBackends, timeout).await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
            // Re-enable is asynchronous; wait at least one poll for fresh backend results.
            let snapshot = loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let snapshot = request(UiAction::Snapshot, timeout).await?;
                if native_ready(&snapshot) || tokio::time::Instant::now() >= deadline {
                    break snapshot;
                }
            };
            if args.json {
                json(&snapshot)?;
            } else {
                status(&snapshot)?;
            }
            return if native_ready(&snapshot) {
                Ok(())
            } else {
                Err(CliError::NotReady)
            };
        }
        CliSubcommand::AuthorizeKey {
            description,
            sha256_fingerprint,
        } => UiAction::Authorize {
            description,
            fingerprint: sha256_fingerprint,
        },
        CliSubcommand::RemoveAuthorizedKey { sha256_fingerprint } => UiAction::Revoke {
            fingerprint: sha256_fingerprint,
        },
        CliSubcommand::Authorized => {
            if args.json {
                json(&snapshot.authorized)?;
            } else {
                let mut peers: Vec<_> = snapshot.authorized.iter().collect();
                peers.sort();
                for (fp, name) in peers {
                    text(format!("{}: {fp}", clean(name)))?;
                }
            }
            return Ok(());
        }
        CliSubcommand::Settings { port, clipboard } => {
            if port.is_none() && clipboard.is_none() {
                return output(&snapshot, args.json);
            }
            UiAction::SetSettings {
                port: port.unwrap_or(snapshot.port),
                clipboard: clipboard.unwrap_or(snapshot.clipboard),
            }
        }
        CliSubcommand::Clipboard { enabled } => UiAction::SetSettings {
            port: snapshot.port,
            clipboard: enabled,
        },
        CliSubcommand::Pause => UiAction::SetPaused { paused: true },
        CliSubcommand::Resume => UiAction::SetPaused { paused: false },
        CliSubcommand::Release => UiAction::Release,
        CliSubcommand::SaveConfig => UiAction::SaveConfig,
        CliSubcommand::Shutdown => UiAction::Shutdown,
        CliSubcommand::Permissions { .. } => unreachable!(),
    };
    let snapshot = request(action, timeout).await?;
    output(&snapshot, args.json)
}
fn output(snapshot: &UiSnapshot, as_json: bool) -> Result<(), CliError> {
    if as_json {
        json(snapshot)
    } else {
        text("后台已确认操作")?;
        list(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use lan_mouse_ipc::Status;
    fn snapshot() -> UiSnapshot {
        UiSnapshot {
            protocol_version: IPC_VERSION,
            clients: vec![],
            fingerprint: "aa:bb".into(),
            authorized: Default::default(),
            port: 4242,
            clipboard: false,
            clipboard_supported: true,
            paused: false,
            active_client: None,
            capture: Status::Disabled,
            capture_backend: None,
            emulation_backend: None,
            capture_error: None,
            emulation_error: None,
            emulation: Status::Disabled,
            release_bind: vec![],
            platform: "test".into(),
            config_path: String::new(),
            connection_attempts: vec![],
            discovered: vec![],
            discovery_error: None,
        }
    }
    #[tokio::test]
    async fn correlated_response_ignores_other_clients_and_events() {
        let mut events = stream::iter([
            Ok(FrontendEvent::Enumerate(vec![])),
            Ok(FrontendEvent::UiResult {
                id: "other".into(),
                result: Err("unrelated".into()),
            }),
            Ok(FrontendEvent::UiResult {
                id: "mine".into(),
                result: Ok(snapshot()),
            }),
        ]);
        assert_eq!(
            read_result(&mut events, "mine").await.unwrap().fingerprint,
            "aa:bb"
        );
    }
    #[tokio::test]
    async fn disconnect_is_not_success() {
        assert!(matches!(
            read_result(&mut stream::empty(), "mine").await,
            Err(CliError::ServiceDisconnected)
        ));
    }
    #[tokio::test]
    async fn backend_rejection_is_not_success() {
        let mut events = stream::iter([Ok(FrontendEvent::UiResult {
            id: "mine".into(),
            result: Err("save failed".into()),
        })]);
        assert!(
            matches!(read_result(&mut events,"mine").await,Err(CliError::ServiceError(e)) if e=="save failed")
        );
    }
    #[tokio::test]
    async fn old_schema_is_rejected() {
        let mut s = snapshot();
        s.protocol_version -= 1;
        let mut events = stream::iter([Ok(FrontendEvent::UiResult {
            id: "mine".into(),
            result: Ok(s),
        })]);
        assert!(matches!(
            read_result(&mut events, "mine").await,
            Err(CliError::Version { .. })
        ));
    }
    #[test]
    fn dummy_cannot_pass_doctor() {
        let mut s = snapshot();
        s.capture = Status::Enabled;
        s.emulation = Status::Enabled;
        s.capture_backend = Some("dummy".into());
        s.emulation_backend = Some("dummy".into());
        assert!(!native_ready(&s));
    }
    #[test]
    fn clipboard_requires_an_explicit_boolean_value() {
        let args = CliArgs::try_parse_from(["cli", "clipboard", "false"]).unwrap();
        assert!(matches!(
            args.command,
            CliSubcommand::Clipboard { enabled: false }
        ));
        assert!(CliArgs::try_parse_from(["cli", "clipboard"]).is_err());
    }

    #[test]
    fn add_client_parses_multiple_ips_and_position() {
        let args = CliArgs::try_parse_from([
            "cli",
            "add-client",
            "--ips",
            "192.168.1.2,192.168.1.3",
            "--position",
            "left",
        ])
        .unwrap();
        assert!(
            matches!(args.command,CliSubcommand::AddClient(c) if c.ips.len()==2&&c.position==Position::Left)
        );
    }
    #[test]
    fn mdns_display_names_with_spaces_are_not_used_as_hostnames() {
        let p = DiscoveredDevice {
            name: "My Mac".into(),
            ips: vec!["192.168.1.2".parse().unwrap()],
            port: 4242,
            fingerprint: "aa".into(),
            platform: "macos".into(),
        };
        assert!(matches!(
            peer_action(&p, Position::Right),
            UiAction::AddClient { hostname: None, .. }
        ));
    }
}
