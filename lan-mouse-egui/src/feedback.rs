// DeskUnify changes, 2026-10-14; GPL-3.0-or-later.
//! User-facing connection feedback, derived from confirmed snapshots only.
use lan_mouse_ipc::{ClientConfig, ClientState, ConnectionIssueKind, ConnectionState, UiSnapshot};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Neutral,
    Success,
    Warning,
    Error,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Connecting,
    Waiting,
    Connected,
    Partial,
    Failed,
    Paused,
    Disabled,
    NeedsTrust,
    Offline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Remedy {
    Retry,
    Edit,
    Trust,
    LocalPermissions,
    RemotePermissions,
    RemoteSettings,
    Resume,
    Enable,
    Files,
    Diagnostics,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Feature {
    Mouse,
    Keyboard,
    Clipboard,
    Files,
}
impl Feature {
    pub const ALL: [Self; 4] = [Self::Mouse, Self::Keyboard, Self::Clipboard, Self::Files];
    pub fn label(self) -> &'static str {
        match self {
            Self::Mouse => "鼠标",
            Self::Keyboard => "键盘",
            Self::Clipboard => "文字复制粘贴",
            Self::Files => "文件复制粘贴",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Capability {
    pub feature: Feature,
    pub enabled: bool,
    pub available: bool,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeviceFeedback {
    pub phase: Phase,
    pub tone: Tone,
    pub summary: String,
    pub guidance: String,
    pub details: Vec<String>,
    pub remedies: Vec<Remedy>,
    pub capabilities: Vec<Capability>,
}
pub(super) fn connected(snapshot: &UiSnapshot, state: &ClientState) -> bool {
    snapshot.control_connected(state)
}
fn native_pairing(snapshot: &UiSnapshot) -> bool {
    matches!(snapshot.platform.as_str(), "macos" | "windows")
}
fn preference(config: &ClientConfig, feature: Feature) -> bool {
    match feature {
        Feature::Mouse => config.sharing.mouse,
        Feature::Keyboard => config.sharing.keyboard,
        Feature::Clipboard => config.sharing.clipboard,
        Feature::Files => config.sharing.files,
    }
}
fn peer_preference(state: &ClientState, feature: Feature) -> bool {
    state.peer_sharing.is_some_and(|p| match feature {
        Feature::Mouse => p.mouse,
        Feature::Keyboard => p.keyboard,
        Feature::Clipboard => p.clipboard,
        Feature::Files => p.files,
    })
}
pub(super) fn device(
    snapshot: &UiSnapshot,
    config: &ClientConfig,
    state: &ClientState,
    online: bool,
) -> DeviceFeedback {
    let mut result = DeviceFeedback {
        phase: Phase::Connecting,
        tone: Tone::Neutral,
        summary: "正在连接".into(),
        guidance: "正在联系对端程序，请稍候。".into(),
        details: Vec::new(),
        remedies: Vec::new(),
        capabilities: Vec::new(),
    };
    if let Some(error) = &state.sharing_error {
        result.details.push(error.clone());
    }
    if let Some(note) = &state.peer_note {
        result.details.push(note.clone());
    }
    if !online {
        result.phase = Phase::Offline;
        result.tone = Tone::Error;
        result.summary = "本机后台离线".into();
        result.guidance = "当前为最后一次状态。请重新启动本机后台后再操作。".into();
        result.remedies.push(Remedy::Diagnostics);
    } else if !state.active {
        result.phase = Phase::Disabled;
        result.summary = "设备已停用".into();
        result.guidance = "此设备四项共享均已停止，原偏好保留。".into();
        result.remedies.push(Remedy::Enable);
    } else if snapshot.paused {
        result.phase = Phase::Paused;
        result.summary = "共享已暂停".into();
        result.guidance = "恢复后继续使用每台设备保存的共享设置。".into();
        result.remedies.push(Remedy::Resume);
    } else if native_pairing(snapshot)
        && config
            .fingerprint
            .as_ref()
            .is_none_or(|fp| !snapshot.authorized.contains_key(fp))
    {
        result.phase = Phase::NeedsTrust;
        result.tone = Tone::Warning;
        result.summary = "尚未完成配对授权".into();
        result.guidance = "请核对对端完整指纹并添加授权。首次连接还需对端确认允许。".into();
        result.remedies.extend([Remedy::Trust, Remedy::Edit]);
    } else if let Some(issue) = &snapshot.control_error {
        result.phase = Phase::Failed;
        result.tone = Tone::Error;
        result.summary = issue.summary().into();
        result.guidance = issue.guidance().into();
        result.details.push(issue.detail.clone());
        result.remedies.push(Remedy::Diagnostics);
    } else if let Some(ConnectionState::Failed(issue)) = &state.connection {
        result.phase = if issue.kind == ConnectionIssueKind::ConfirmationPending {
            Phase::Waiting
        } else {
            Phase::Failed
        };
        result.tone = if result.phase == Phase::Waiting {
            Tone::Neutral
        } else {
            Tone::Error
        };
        result.summary = issue.summary().into();
        result.guidance = issue.guidance().into();
        if !result.details.contains(&issue.detail) {
            result.details.push(issue.detail.clone());
        }
        result.remedies.push(match issue.kind {
            ConnectionIssueKind::IdentityMismatch => Remedy::Trust,
            ConnectionIssueKind::ProtocolMismatch | ConnectionIssueKind::LocalService => {
                Remedy::Diagnostics
            }
            _ => Remedy::Retry,
        });
        if matches!(
            issue.kind,
            ConnectionIssueKind::Refused
                | ConnectionIssueKind::Timeout
                | ConnectionIssueKind::Unreachable
        ) {
            result.remedies.push(Remedy::Edit);
        }
    } else if state.sharing_error.is_some() {
        result.phase = Phase::Failed;
        result.tone = Tone::Error;
        result.summary = "连接或配对尚未就绪".into();
        result.guidance = "请查看技术详情，检查对端程序、网络与授权后重试。".into();
        result.remedies.push(Remedy::Retry);
    } else if connected(snapshot, state) {
        result.phase = Phase::Connected;
        result.tone = Tone::Success;
        result.summary = "连接已建立".into();
        result.guidance = "已完成设备身份与共享设置协商；请跨屏实测各项功能。".into();
    } else if matches!(
        state.connection,
        Some(ConnectionState::AwaitingConfirmation)
    ) {
        result.phase = Phase::Waiting;
        result.summary = "等待对端确认".into();
        result.guidance =
            "请求已发送；首次连接请在对端点击「允许配对」。已有配对时正在等待对端回复共享状态。"
                .into();
        result.remedies.push(Remedy::Retry);
    } else if state.resolving
        || (config.fix_ips.is_empty() && state.ips.is_empty() && state.peer_addr.is_none())
    {
        result.summary = "正在解析设备地址".into();
        result.guidance = "尚未得到可连接地址，请检查主机名，或在编辑设备中填写固定 IP。".into();
        result.remedies.push(Remedy::Edit);
    } else if !native_pairing(snapshot) {
        result.guidance = "将鼠标移到配置的屏幕边缘，发起输入连接。".into();
        result.remedies.push(Remedy::Diagnostics);
    }
    let ready = result.phase == Phase::Connected;
    for feature in Feature::ALL {
        let enabled = preference(config, feature);
        let mut available = false;
        let reason = if !enabled {
            "本机已关闭".into()
        } else if !ready {
            result.summary.clone()
        } else if native_pairing(snapshot) && !peer_preference(state, feature) {
            result.remedies.push(Remedy::RemoteSettings);
            state
                .peer_note
                .clone()
                .unwrap_or_else(|| "对端已关闭此功能".into())
        } else if matches!(feature, Feature::Mouse | Feature::Keyboard) && !snapshot.native_ready()
        {
            result.remedies.push(
                if [&snapshot.capture_error, &snapshot.emulation_error]
                    .into_iter()
                    .flatten()
                    .any(|e| e.to_ascii_lowercase().contains("permission"))
                {
                    Remedy::LocalPermissions
                } else {
                    Remedy::Diagnostics
                },
            );
            "本机原生键鼠后端未就绪".into()
        } else if matches!(feature, Feature::Mouse | Feature::Keyboard)
            && state
                .peer_note
                .as_deref()
                .is_some_and(|n| n.contains("输入模拟尚未就绪"))
        {
            result.remedies.push(Remedy::RemotePermissions);
            "对端输入模拟未就绪，请在对端处理权限或后端错误".into()
        } else if matches!(feature, Feature::Clipboard | Feature::Files)
            && !snapshot.clipboard_supported
        {
            "当前平台未提供此共享能力".into()
        } else if feature == Feature::Files && snapshot.files_error.is_some() {
            result.remedies.push(Remedy::Files);
            "本机文件服务报告错误，请查看文件页详情".into()
        } else {
            available = true;
            "双方已开启 · 就绪".into()
        };
        result.capabilities.push(Capability {
            feature,
            enabled,
            available,
            reason,
        });
    }
    if ready {
        let blocked = result
            .capabilities
            .iter()
            .filter(|f| f.enabled && !f.available)
            .collect::<Vec<_>>();
        if !blocked.is_empty() {
            result.phase = Phase::Partial;
            result.tone = Tone::Warning;
            result.summary = "已连接，部分共享不可用".into();
            result.guidance = blocked
                .iter()
                .map(|f| format!("{}：{}", f.feature.label(), f.reason))
                .collect::<Vec<_>>()
                .join("；");
        } else if result.capabilities.iter().all(|f| !f.enabled) {
            result.summary = "已连接，四项共享均已关闭".into();
        }
    }
    let mut unique = Vec::new();
    for remedy in result.remedies {
        if !unique.contains(&remedy) {
            unique.push(remedy);
        }
    }
    result.remedies = unique;
    result
}

#[derive(Clone, Debug)]
pub(super) struct Notice {
    pub id: Option<u64>,
    pub tone: Tone,
    pub title: String,
    pub body: String,
}
#[derive(Default)]
pub(super) struct Tracker {
    known: HashMap<String, DeviceFeedback>,
    initialized: bool,
    native_ready: bool,
}
impl Tracker {
    pub fn update(&mut self, snapshot: &UiSnapshot) -> Vec<Notice> {
        let mut notices = Vec::new();
        let readiness_changed = self.initialized && self.native_ready != snapshot.native_ready();
        if readiness_changed {
            let ready = snapshot.native_ready();
            notices.push(Notice {
                id: None,
                tone: if ready { Tone::Success } else { Tone::Warning },
                title: if ready {
                    "本机原生键鼠后端已就绪"
                } else {
                    "本机键鼠共享未就绪"
                }
                .into(),
                body: if ready {
                    "权限与后端检查通过，请跨屏实测。"
                } else {
                    "请处理本机权限或后端错误；文字与文件共享独立运行。"
                }
                .into(),
            });
        }
        let mut current = HashMap::new();
        for (id, config, state) in &snapshot.clients {
            let key = config
                .fingerprint
                .clone()
                .unwrap_or_else(|| format!("client:{id}"));
            let feedback = device(snapshot, config, state, true);
            let old = self.known.get(&key);
            // Ignore raw socket error churn: compare user-facing causes, not technical details.
            let changed = old.is_none_or(|old| {
                old.phase != feedback.phase
                    || old.summary != feedback.summary
                    || old.guidance != feedback.guidance
            });
            if self.initialized
                && changed
                && matches!(
                    feedback.phase,
                    Phase::Connected | Phase::Partial | Phase::Failed | Phase::NeedsTrust
                )
                && !(readiness_changed
                    && old
                        .is_some_and(|old| matches!(old.phase, Phase::Connected | Phase::Partial))
                    && matches!(feedback.phase, Phase::Connected | Phase::Partial))
            {
                let name = config
                    .hostname
                    .as_deref()
                    .map(str::to_owned)
                    .or_else(|| config.fix_ips.first().map(ToString::to_string))
                    .unwrap_or_else(|| "远端设备".into());
                notices.push(Notice {
                    id: Some(*id),
                    tone: feedback.tone,
                    title: format!("{name}：{}", feedback.summary),
                    body: feedback.guidance.clone(),
                });
            }
            current.insert(key, feedback);
        }
        self.known = current;
        self.initialized = true;
        self.native_ready = snapshot.native_ready();
        notices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lan_mouse_ipc::{ConnectionIssue, Sharing, Status};
    fn fixture() -> UiSnapshot {
        let mut s = crate::app::tests::snapshot();
        s.platform = "macos".into();
        s.capture = Status::Enabled;
        s.emulation = Status::Enabled;
        s.capture_backend = Some("macos".into());
        s.emulation_backend = Some("macos".into());
        s.clients[0].1.fingerprint = Some("peer-fingerprint".into());
        s.authorized
            .insert("peer-fingerprint".into(), "peer".into());
        s.clients[0].2.connection = Some(ConnectionState::Connected);
        s.clients[0].2.peer_sharing = Some(Sharing::default());
        s
    }
    fn report(s: &UiSnapshot) -> DeviceFeedback {
        device(s, &s.clients[0].1, &s.clients[0].2, true)
    }
    #[test]
    fn configuration_ack_and_input_alive_do_not_claim_control_connection() {
        let mut s = fixture();
        s.clients[0].2.connection = None;
        s.clients[0].2.peer_sharing = None;
        s.clients[0].2.alive = true;
        assert_ne!(report(&s).phase, Phase::Connected);
        s.clients[0].2.connection = Some(ConnectionState::AwaitingConfirmation);
        assert_eq!(report(&s).phase, Phase::Waiting);
    }
    #[test]
    fn authenticated_control_connects_before_pointer_enters_peer() {
        let s = fixture();
        assert!(!s.clients[0].2.alive);
        assert_eq!(report(&s).phase, Phase::Connected);
        assert!(report(&s).capabilities.iter().all(|f| f.available));
    }
    #[test]
    fn doctor_rejects_stale_input_channel_after_control_failure() {
        let mut s = fixture();
        s.clients[0].2.alive = true;
        assert!(s.doctor_ready(false));
        s.clients[0].2.connection = Some(ConnectionState::Failed(ConnectionIssue::new(
            ConnectionIssueKind::Interrupted,
            "closed",
        )));
        assert!(!s.doctor_ready(false));
        assert!(s.doctor_ready(true));
    }
    #[test]
    fn missing_local_permission_keeps_text_and_files_ready() {
        let mut s = fixture();
        s.capture = Status::Disabled;
        s.capture_error = Some("accessibility permission is required".into());
        let f = report(&s);
        assert_eq!(f.phase, Phase::Partial);
        assert!(f.remedies.contains(&Remedy::LocalPermissions));
        assert!(f.capabilities[2].available && f.capabilities[3].available);
        assert!(!f.capabilities[0].available && !f.capabilities[1].available);
    }
    #[test]
    fn remote_input_error_is_attributed_to_remote_machine() {
        let mut s = fixture();
        s.clients[0].2.peer_note = Some("对端输入模拟尚未就绪：permission required".into());
        let f = report(&s);
        assert!(f.remedies.contains(&Remedy::RemotePermissions));
        assert!(!f.remedies.contains(&Remedy::LocalPermissions));
        assert!(f.capabilities[2].available);
    }
    #[test]
    fn remote_switch_off_and_local_preference_are_distinct() {
        let mut s = fixture();
        s.clients[0].1.sharing.keyboard = false;
        s.clients[0].2.peer_sharing.as_mut().unwrap().clipboard = false;
        let f = report(&s);
        assert_eq!(f.capabilities[1].reason, "本机已关闭");
        assert_eq!(f.capabilities[2].reason, "对端已关闭此功能");
        assert!(f.capabilities[3].available);
    }
    #[test]
    fn pause_disable_revoke_and_offline_never_display_available() {
        let mut s = fixture();
        s.paused = true;
        assert_eq!(report(&s).phase, Phase::Paused);
        assert!(report(&s).capabilities.iter().all(|f| !f.available));
        s.paused = false;
        s.clients[0].2.active = false;
        assert_eq!(report(&s).phase, Phase::Disabled);
        s.clients[0].2.active = true;
        s.authorized.clear();
        assert_eq!(report(&s).phase, Phase::NeedsTrust);
        assert_eq!(
            device(&s, &s.clients[0].1, &s.clients[0].2, false).phase,
            Phase::Offline
        );
    }
    #[test]
    fn timeout_does_not_claim_firewall_is_the_cause() {
        let mut s = fixture();
        s.clients[0].2.connection = Some(ConnectionState::Failed(ConnectionIssue::new(
            ConnectionIssueKind::Timeout,
            "elapsed",
        )));
        let f = report(&s);
        assert_eq!(f.phase, Phase::Failed);
        assert!(f.guidance.contains("不能确定"));
        assert!(f.remedies.contains(&Remedy::Retry));
    }
    #[test]
    fn notifications_are_once_per_cause_and_reappear_on_recovery() {
        let mut s = fixture();
        s.clients[0].2.connection = Some(ConnectionState::Connecting);
        s.clients[0].2.peer_sharing = None;
        let mut tracker = Tracker::default();
        assert!(tracker.update(&s).is_empty());
        s.clients[0].2.connection = Some(ConnectionState::Connected);
        s.clients[0].2.peer_sharing = Some(Sharing::default());
        assert_eq!(tracker.update(&s).len(), 1);
        assert!(tracker.update(&s).is_empty());
        s.clients[0].2.connection = Some(ConnectionState::Failed(ConnectionIssue::new(
            ConnectionIssueKind::Refused,
            "errno 61",
        )));
        assert_eq!(tracker.update(&s).len(), 1);
        s.clients[0].2.connection = Some(ConnectionState::Failed(ConnectionIssue::new(
            ConnectionIssueKind::Refused,
            "errno 10061",
        )));
        assert!(tracker.update(&s).is_empty());
        s.clients[0].2.connection = Some(ConnectionState::Connected);
        assert_eq!(tracker.update(&s).len(), 1);
    }
    #[test]
    fn file_service_failure_is_partial_readiness_and_linux_keeps_input_flow() {
        let mut s = fixture();
        s.files_error = Some("port in use".into());
        let f = report(&s);
        assert!(!f.capabilities[3].available);
        assert!(f.capabilities[0].available);
        s.platform = "linux".into();
        s.clients[0].1.fingerprint = None;
        s.clients[0].2.connection = None;
        s.clients[0].2.alive = true;
        assert!(connected(&s, &s.clients[0].2));
    }
}
