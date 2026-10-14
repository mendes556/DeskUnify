// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use std::{
    collections::{HashMap, HashSet},
    env::VarError,
    fmt::Display,
    io,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};
use thiserror::Error;

#[cfg(unix)]
use std::{
    env,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

mod connect;
mod connect_async;
mod listen;

pub use connect::{FrontendEventReader, FrontendRequestWriter, connect, try_connect};
pub use connect_async::{AsyncFrontendEventReader, AsyncFrontendRequestWriter, connect_async};
pub use listen::AsyncFrontendListener;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error(transparent)]
    SocketPath(#[from] SocketPathError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("connection timed out")]
    Timeout,
}

#[derive(Debug, Error)]
pub enum IpcListenerCreationError {
    #[error("could not determine socket-path: `{0}`")]
    SocketPath(#[from] SocketPathError),
    #[error("service already running!")]
    AlreadyRunning,
    #[error("failed to bind lan-mouse socket: `{0}`")]
    Bind(io::Error),
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io error occured: `{0}`")]
    Io(#[from] io::Error),
    #[error("invalid json: `{0}`")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Listen(#[from] IpcListenerCreationError),
}

pub const DEFAULT_PORT: u16 = 4242;
/// Local IPC schema version. UI and daemon must use matching builds.
pub const IPC_VERSION: u32 = 8;

#[derive(Debug, Default, Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

impl Position {
    pub fn opposite(&self) -> Self {
        match self {
            Position::Left => Position::Right,
            Position::Right => Position::Left,
            Position::Top => Position::Bottom,
            Position::Bottom => Position::Top,
        }
    }
}

#[derive(Debug, Error)]
#[error("not a valid position: {pos}")]
pub struct PositionParseError {
    pos: String,
}

impl FromStr for Position {
    type Err = PositionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            _ => Err(PositionParseError { pos: s.into() }),
        }
    }
}

impl Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Position::Left => "left",
                Position::Right => "right",
                Position::Top => "top",
                Position::Bottom => "bottom",
            }
        )
    }
}

impl TryFrom<&str> for Position {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "left" => Ok(Position::Left),
            "right" => Ok(Position::Right),
            "top" => Ok(Position::Top),
            "bottom" => Ok(Position::Bottom),
            _ => Err(()),
        }
    }
}

/// Persistent preferences; effective sharing is the intersection of both peers.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharing {
    pub mouse: bool,
    pub keyboard: bool,
    pub clipboard: bool,
    pub files: bool,
}
impl Default for Sharing {
    fn default() -> Self {
        Self {
            mouse: true,
            keyboard: true,
            clipboard: true,
            files: true,
        }
    }
}
impl Sharing {
    pub const OFF: Self = Self {
        mouse: false,
        keyboard: false,
        clipboard: false,
        files: false,
    };
    pub fn intersect(self, other: Self) -> Self {
        Self {
            mouse: self.mouse && other.mouse,
            keyboard: self.keyboard && other.keyboard,
            clipboard: self.clipboard && other.clipboard,
            files: self.files && other.files,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    pub fingerprint: String,
    pub ip: IpAddr,
    pub port: u16,
    pub position: Position,
    pub name: String,
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub sharing: Sharing,
    /// hostname of this client
    pub hostname: Option<String>,
    /// fix ips, determined by the user
    pub fix_ips: Vec<IpAddr>,
    /// both active_addr and addrs can be None / empty so port needs to be stored seperately
    pub port: u16,
    /// position of a client on screen
    pub pos: Position,
    /// enter hook
    pub cmd: Option<String>,
    /// leave hook
    pub leave_cmd: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            fingerprint: None,
            sharing: Sharing::default(),
            port: DEFAULT_PORT,
            hostname: Default::default(),
            fix_ips: Default::default(),
            pos: Default::default(),
            cmd: None,
            leave_cmd: None,
        }
    }
}

pub type ClientHandle = u64;

/// The authenticated control connection is separate from physical input readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "issue", rename_all = "snake_case")]
pub enum ConnectionState {
    Connecting,
    AwaitingConfirmation,
    Connected,
    Failed(ConnectionIssue),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionIssueKind {
    Timeout,
    ConfirmationTimeout,
    ConfirmationPending,
    Refused,
    Unreachable,
    IdentityMismatch,
    ProtocolMismatch,
    Rejected,
    Interrupted,
    LocalService,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{detail}")]
pub struct ConnectionIssue {
    pub kind: ConnectionIssueKind,
    pub detail: String,
}

impl ConnectionIssue {
    pub fn new(kind: ConnectionIssueKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub fn summary(&self) -> &'static str {
        match self.kind {
            ConnectionIssueKind::Timeout => "未收到对端响应，连接超时",
            ConnectionIssueKind::ConfirmationTimeout => "等待对端确认超时",
            ConnectionIssueKind::ConfirmationPending => "等待对端确认配对",
            ConnectionIssueKind::Refused => "对端拒绝了网络连接",
            ConnectionIssueKind::Unreachable => "无法到达对端网络地址",
            ConnectionIssueKind::IdentityMismatch => "对端证书身份校验失败",
            ConnectionIssueKind::ProtocolMismatch => "两端连接协议不兼容",
            ConnectionIssueKind::Rejected => "对端未允许此次配对",
            ConnectionIssueKind::Interrupted => "与对端的连接已中断",
            ConnectionIssueKind::LocalService => "本机配对服务未就绪",
            ConnectionIssueKind::Other => "连接未能完成",
        }
    }

    pub fn guidance(&self) -> &'static str {
        match self.kind {
            ConnectionIssueKind::Timeout | ConnectionIssueKind::Unreachable => {
                "请检查对端是否启动 DeskUnify、IP 是否正确、两台设备的网络和防火墙；当前不能确定是哪一项导致。"
            }
            ConnectionIssueKind::ConfirmationTimeout | ConnectionIssueKind::ConfirmationPending => {
                "请求已发出；首次配对请在对端点击允许，然后重试。也请确认对端程序仍在运行。"
            }
            ConnectionIssueKind::Refused => {
                "请确认对端后台已启动且端口正确；防火墙拒绝连接也可能产生此结果。"
            }
            ConnectionIssueKind::IdentityMismatch => {
                "请在对端查看当前完整指纹并重新核对配对信息；证书更新可能需要重新授权。"
            }
            ConnectionIssueKind::ProtocolMismatch => "请将两台设备更新到兼容版本，再重新连接。",
            ConnectionIssueKind::Rejected => "请在对端查看配对或设备授权，确认允许这台电脑后重试。",
            ConnectionIssueKind::Interrupted => "请确认对端未退出或休眠、网络仍连通，然后重试。",
            ConnectionIssueKind::LocalService => {
                "请查看下方技术详情，检查端口占用或证书错误；处理后重新启动本机后台。"
            }
            ConnectionIssueKind::Other => "请查看技术详情，并检查两端程序、网络和配对状态后重试。",
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClientState {
    #[serde(default)]
    pub connection: Option<ConnectionState>,
    #[serde(default)]
    pub peer_sharing: Option<Sharing>,
    #[serde(default)]
    pub peer_addr: Option<SocketAddr>,
    #[serde(default)]
    pub peer_note: Option<String>,
    #[serde(default)]
    pub sharing_error: Option<String>,
    /// events should be sent to and received from the client
    pub active: bool,
    /// `active` address of the client, used to send data to.
    /// This should generally be the socket address where data
    /// was last received from.
    pub active_addr: Option<SocketAddr>,
    /// tracks whether or not the client is available for emulation
    pub alive: bool,
    /// ips from dns
    pub dns_ips: Vec<IpAddr>,
    /// all ip addresses associated with a particular client
    /// e.g. Laptops usually have at least an ethernet and a wifi port
    /// which have different ip addresses
    pub ips: HashSet<IpAddr>,
    /// client has pressed keys
    pub has_pressed_keys: bool,
    /// dns resolving in progress
    pub resolving: bool,
    /// Peer's build short commit hash from the [`Hello`] proto
    /// event. `None` means we haven't received a Hello yet — either
    /// the connection is fresh, or the peer is on an older build
    /// that predates the Hello event. The frontend uses this to
    /// soft-warn on version mismatch.
    pub peer_commit: Option<[u8; 8]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendEvent {
    /// A correlated desktop UI operation, confirmed by the daemon.
    UiResult {
        id: String,
        result: Result<UiSnapshot, String>,
    },
    /// a client was created
    Created(ClientHandle, ClientConfig, ClientState),
    /// no such client
    NoSuchClient(ClientHandle),
    /// state changed
    State(ClientHandle, ClientConfig, ClientState),
    /// the client was deleted
    Deleted(ClientHandle),
    /// new port, reason of failure (if failed)
    PortChanged(u16, Option<String>),
    /// list of all clients, used for initial state synchronization
    Enumerate(Vec<(ClientHandle, ClientConfig, ClientState)>),
    /// an error occured
    Error(String),
    /// capture status
    CaptureStatus(Status),
    /// emulation status
    EmulationStatus(Status),
    /// authorized public key fingerprints have been updated
    AuthorizedUpdated(HashMap<String, String>),
    /// public key fingerprint of this device
    PublicKeyFingerprint(String),
    /// new device connected
    DeviceConnected {
        addr: SocketAddr,
        fingerprint: String,
    },
    /// incoming device entered the screen
    DeviceEntered {
        fingerprint: String,
        addr: SocketAddr,
        pos: Position,
    },
    /// incoming disconnected
    IncomingDisconnected(SocketAddr),
    /// failed connection attempt (approval for fingerprint required)
    ConnectionAttempt { fingerprint: String },
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub enum FrontendRequest {
    Ui {
        id: String,
        action: UiAction,
    },
    /// activate/deactivate client
    Activate(ClientHandle, bool),
    /// add a new client
    Create,
    /// change the listen port (recreate udp listener)
    ChangePort(u16),
    /// remove a client
    Delete(ClientHandle),
    /// request an enumeration of all clients
    Enumerate(),
    /// resolve dns
    ResolveDns(ClientHandle),
    /// update hostname
    UpdateHostname(ClientHandle, Option<String>),
    /// update port
    UpdatePort(ClientHandle, u16),
    /// update position
    UpdatePosition(ClientHandle, Position),
    /// update fix-ips
    UpdateFixIps(ClientHandle, Vec<IpAddr>),
    /// request reenabling input capture
    EnableCapture,
    /// request reenabling input emulation
    EnableEmulation,
    /// synchronize all state
    Sync,
    /// authorize fingerprint (description, fingerprint)
    AuthorizeKey(String, String),
    /// remove fingerprint (fingerprint)
    RemoveAuthorizedKey(String),
    /// change the hook command
    UpdateEnterHook(u64, Option<String>),
    /// change the leave hook command
    UpdateLeaveHook(u64, Option<String>),
    /// save config file
    SaveConfiguration,
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiAction {
    Snapshot,
    AddClient {
        hostname: Option<String>,
        ips: Vec<IpAddr>,
        port: u16,
        position: Position,
        fingerprint: Option<String>,
        #[serde(default)]
        enter_hook: Option<String>,
        #[serde(default)]
        leave_hook: Option<String>,
    },
    UpdateClient {
        id: ClientHandle,
        hostname: Option<String>,
        ips: Vec<IpAddr>,
        port: u16,
        position: Position,
        active: bool,
    },
    RemoveClient {
        id: ClientHandle,
    },
    SetHostname {
        id: ClientHandle,
        hostname: Option<String>,
    },
    SetClientPort {
        id: ClientHandle,
        port: u16,
    },
    SetClientIps {
        id: ClientHandle,
        ips: Vec<IpAddr>,
    },
    SetPosition {
        id: ClientHandle,
        position: Position,
    },
    SetSharing {
        id: ClientHandle,
        sharing: Sharing,
    },
    RejectPair {
        fingerprint: String,
    },
    SetFileDirectory {
        directory: String,
    },
    SetActive {
        id: ClientHandle,
        active: bool,
    },
    RetryClient {
        id: ClientHandle,
    },
    Authorize {
        description: String,
        fingerprint: String,
    },
    Revoke {
        fingerprint: String,
    },
    SetSettings {
        port: u16,
        clipboard: bool,
    },
    SetPaused {
        paused: bool,
    },
    Release,
    RetryBackends,
    SetHooks {
        id: ClientHandle,
        enter_hook: Option<String>,
        leave_hook: Option<String>,
    },
    SaveConfig,
    Scan,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub name: String,
    pub ips: Vec<IpAddr>,
    pub port: u16,
    pub fingerprint: String,
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSnapshot {
    #[serde(default)]
    pub control_error: Option<ConnectionIssue>,
    #[serde(default)]
    pub pair_requests: Vec<PairRequest>,
    #[serde(default)]
    pub clipboard_target: Option<ClientHandle>,
    #[serde(default)]
    pub file_directory: String,
    #[serde(default)]
    pub files_error: Option<String>,
    pub protocol_version: u32,
    pub clients: Vec<(ClientHandle, ClientConfig, ClientState)>,
    pub fingerprint: String,
    pub authorized: HashMap<String, String>,
    pub port: u16,
    pub clipboard: bool,
    pub clipboard_supported: bool,
    pub paused: bool,
    pub active_client: Option<ClientHandle>,
    pub capture: Status,
    pub capture_backend: Option<String>,
    pub emulation_backend: Option<String>,
    pub capture_error: Option<String>,
    pub emulation_error: Option<String>,
    pub emulation: Status,
    pub release_bind: Vec<String>,
    pub platform: String,
    pub config_path: String,
    pub connection_attempts: Vec<String>,
    pub discovered: Vec<DiscoveredDevice>,
    pub discovery_error: Option<String>,
}

/// A virtual or unidentified backend cannot share physical input.
pub fn native_backend_ready(status: Status, backend: Option<&str>) -> bool {
    status == Status::Enabled && backend.is_some_and(|name| name != "dummy")
}

impl UiSnapshot {
    pub fn control_connected(&self, state: &ClientState) -> bool {
        if matches!(self.platform.as_str(), "macos" | "windows") {
            matches!(state.connection, Some(ConnectionState::Connected))
                && state.peer_sharing.is_some()
                && state.sharing_error.is_none()
        } else {
            state.alive
        }
    }
    pub fn native_ready(&self) -> bool {
        native_backend_ready(self.capture, self.capture_backend.as_deref())
            && native_backend_ready(self.emulation, self.emulation_backend.as_deref())
    }

    /// Connection readiness still requires a physical cross-screen test.
    pub fn doctor_ready(&self, local: bool) -> bool {
        self.native_ready()
            && !self.paused
            && (local
                || self.clients.iter().any(|(_, _, state)| {
                    state.active && state.alive && self.control_connected(state)
                }))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Disabled,
    Enabled,
}

impl From<Status> for bool {
    fn from(status: Status) -> Self {
        match status {
            Status::Enabled => true,
            Status::Disabled => false,
        }
    }
}

#[cfg(unix)]
const LAN_MOUSE_SOCKET_NAME: &str = "lan-mouse-socket.sock";

#[derive(Debug, Error)]
pub enum SocketPathError {
    #[error("could not determine $XDG_RUNTIME_DIR: `{0}`")]
    XdgRuntimeDirNotFound(VarError),
    #[error("could not determine $HOME: `{0}`")]
    HomeDirNotFound(VarError),
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn default_socket_path() -> Result<PathBuf, SocketPathError> {
    if let Some(path) = env::var_os("LAN_MOUSE_IPC_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let xdg_runtime_dir =
        env::var("XDG_RUNTIME_DIR").map_err(SocketPathError::XdgRuntimeDirNotFound)?;
    Ok(Path::new(xdg_runtime_dir.as_str()).join(LAN_MOUSE_SOCKET_NAME))
}

#[cfg(all(unix, target_os = "macos"))]
pub fn default_socket_path() -> Result<PathBuf, SocketPathError> {
    if let Some(path) = env::var_os("LAN_MOUSE_IPC_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let home = env::var("HOME").map_err(SocketPathError::HomeDirNotFound)?;
    Ok(Path::new(home.as_str())
        .join("Library")
        .join("Caches")
        .join(LAN_MOUSE_SOCKET_NAME))
}

/// Check if a lan-mouse service is already running by probing the IPC socket.
#[cfg(unix)]
pub fn is_service_running() -> bool {
    let Ok(socket_path) = default_socket_path() else {
        return false;
    };
    std::os::unix::net::UnixStream::connect(socket_path).is_ok()
}

/// Check if a lan-mouse service is already running by probing the IPC socket.
#[cfg(windows)]
pub fn is_service_running() -> bool {
    std::net::TcpStream::connect("127.0.0.1:5252").is_ok()
}
