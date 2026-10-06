// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use super::*;
use crate::discovery::validate_fingerprint;
use lan_mouse_ipc::{IPC_VERSION, UiAction, UiSnapshot};
use std::time::Duration;

#[derive(Default)]
pub(super) struct UiState {
    pub paused: bool,
    pub capture_backend: Option<String>,
    pub emulation_backend: Option<String>,
    pub capture_error: Option<String>,
    pub emulation_error: Option<String>,
    pub active_client: Option<ClientHandle>,
    pub attempts: VecDeque<String>,
    pub shutdown: bool,
}

impl Service {
    fn ui_snapshot(&mut self) -> UiSnapshot {
        UiSnapshot {
            #[cfg(any(target_os = "macos", windows))]
            pair_requests: self
                .pair_requests
                .values()
                .filter(|(_, _, reply)| !reply.is_closed())
                .map(|(r, _, _)| r.clone())
                .collect(),
            #[cfg(not(any(target_os = "macos", windows)))]
            pair_requests: vec![],
            clipboard_target: self.target_client().map(|(id, _, _)| id),
            file_directory: self.config.file_directory().display().to_string(),
            #[cfg(any(target_os = "macos", windows))]
            files_error: self.files.error(),
            #[cfg(not(any(target_os = "macos", windows)))]
            files_error: Some("自动文件剪贴板仅支持 macOS/Windows".into()),
            protocol_version: IPC_VERSION,
            clients: self
                .client_manager
                .get_client_states()
                .into_iter()
                .map(|(id, c, mut s)| {
                    #[cfg(any(target_os = "macos", windows))]
                    if let Some(fp) = &c.fingerprint {
                        if s.peer_sharing.is_some()
                            && self
                                .peer_seen
                                .get(fp)
                                .is_none_or(|t| t.elapsed() >= Duration::from_secs(4))
                        {
                            s.peer_sharing = None;
                            s.sharing_error = Some("连接已中断，等待对端后台".into());
                        }
                    }
                    (id, c, s)
                })
                .collect(),
            fingerprint: self.public_key_fingerprint.clone(),
            authorized: self.authorized_keys.read().expect("lock").clone(),
            port: self.port,
            clipboard: self
                .client_manager
                .clients()
                .iter()
                .any(|(c, s)| s.active && c.sharing.clipboard),
            clipboard_supported: cfg!(any(target_os = "macos", windows)),
            paused: self.ui.paused,
            active_client: self.ui.active_client,
            capture: self.capture_status,
            capture_backend: self.ui.capture_backend.clone(),
            emulation_backend: self.ui.emulation_backend.clone(),
            capture_error: self.ui.capture_error.clone(),
            emulation_error: self.ui.emulation_error.clone(),
            emulation: self.emulation_status,
            release_bind: self
                .config
                .release_bind()
                .iter()
                .map(|key| format!("{key:?}"))
                .collect(),
            platform: std::env::consts::OS.into(),
            config_path: self.config.config_path().display().to_string(),
            connection_attempts: self.ui.attempts.iter().cloned().collect(),
            discovered: self.discovery.devices(),
            discovery_error: self.discovery.error.clone(),
        }
    }

    pub(super) async fn handle_ui_action(
        &mut self,
        action: UiAction,
    ) -> Result<UiSnapshot, String> {
        validate(&action)?;
        let persist = !matches!(
            action,
            UiAction::Snapshot
                | UiAction::SetPaused { .. }
                | UiAction::Release
                | UiAction::RetryBackends
                | UiAction::Scan
                | UiAction::Shutdown
        );
        match action {
            UiAction::Snapshot | UiAction::SaveConfig => {}
            UiAction::AddClient {
                hostname,
                ips,
                port,
                position,
                fingerprint,
                enter_hook,
                leave_hook,
            } => {
                let description = hostname.clone().unwrap_or_else(|| {
                    ips.first()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "远端设备".into())
                });
                let fingerprint = fingerprint.map(|s| s.to_ascii_lowercase());
                let existing = fingerprint
                    .as_ref()
                    .and_then(|fp| self.client_manager.by_fingerprint(fp));
                let handle = if let Some(id) = existing {
                    self.update_fix_ips(id, ips);
                    self.update_port(id, port);
                    id
                } else {
                    self.client_manager.add_with_config(ConfigClient {
                        fingerprint: fingerprint.clone().map(|s| s.to_ascii_lowercase()),
                        sharing: Default::default(),
                        hostname,
                        ips: ips.into_iter().collect(),
                        port,
                        pos: position,
                        active: false,
                        enter_hook,
                        leave_hook,
                    })
                };
                let (config, state) = self.client_manager.get_state(handle).expect("new client");
                self.notify_frontend(FrontendEvent::Created(handle, config, state));
                self.activate_client(handle);
                if let Some(fingerprint) = fingerprint {
                    self.add_authorized_key(description, fingerprint.to_ascii_lowercase());
                }
                if self.clipboard_target.is_none() {
                    self.clipboard_target = self
                        .client_manager
                        .get_state(handle)
                        .and_then(|(c, _)| c.fingerprint);
                }
                self.broadcast_client(handle);
            }
            UiAction::UpdateClient {
                id,
                hostname,
                ips,
                port,
                position,
                active,
            } => {
                self.require_client(id)?;
                self.deactivate_client(id);
                self.update_hostname(id, hostname);
                self.update_fix_ips(id, ips);
                self.update_port(id, port);
                self.update_pos(id, position);
                self.set_client_active(id, active);
            }
            UiAction::RemoveClient { id } => {
                self.require_client(id)?;
                self.release_device(id).await?;
                self.remove_client(id);
            }
            UiAction::SetHostname { id, hostname } => {
                self.require_client(id)?;
                let (config, _) = self.client_manager.get_state(id).expect("checked client");
                validate_client(&hostname, &config.fix_ips, config.port)?;
                self.update_hostname(id, hostname);
            }
            UiAction::SetClientPort { id, port } => {
                self.require_client(id)?;
                validate_port(port)?;
                self.update_port(id, port);
            }
            UiAction::SetClientIps { id, ips } => {
                self.require_client(id)?;
                let (config, _) = self.client_manager.get_state(id).expect("checked client");
                validate_client(&config.hostname, &ips, config.port)?;
                self.update_fix_ips(id, ips);
            }
            UiAction::SetPosition { id, position } => {
                self.require_client(id)?;
                let (config, state) = self.client_manager.get_state(id).expect("checked client");
                if let Some(other) = self
                    .client_manager
                    .client_at(position)
                    .filter(|other| *other != id && state.active)
                {
                    self.deactivate_client(id);
                    self.deactivate_client(other);
                    self.update_pos(other, config.pos);
                    self.activate_client(other);
                    self.update_pos(id, position);
                    self.activate_client(id);
                } else {
                    self.update_pos(id, position);
                }
            }
            UiAction::SetSharing { id, sharing } => {
                self.require_client(id)?;
                self.release_device(id).await?;
                let (mut c, _) = self.client_manager.get_state(id).expect("checked client");
                c.sharing = sharing;
                self.client_manager.set_config(id, c);
                self.configure_sharing();
            }
            UiAction::RejectPair { fingerprint } => {
                self.rejected_pairs.insert(fingerprint.clone());
                #[cfg(any(target_os = "macos", windows))]
                if let Some((_, _, reply)) = self.pair_requests.remove(&fingerprint) {
                    let _ = reply.send(crate::control::Reply::rejected("对端拒绝了配对请求"));
                }
                let _ = fingerprint;
            }
            UiAction::SetFileDirectory { directory } => {
                let path = std::path::PathBuf::from(directory);
                if !path.is_absolute() {
                    return Err("接收目录必须是绝对路径".into());
                }
                self.config.set_file_directory(path);
            }
            UiAction::SetActive { id, active } => {
                self.release_device(id).await?;
                self.require_client(id)?;
                self.set_client_active(id, active);
            }
            UiAction::Authorize {
                description,
                fingerprint,
            } => {
                let fp = fingerprint.to_ascii_lowercase();
                self.add_authorized_key(description, fp.clone());
                #[cfg(any(target_os = "macos", windows))]
                self.approve_pair(&fp);
            }
            UiAction::Revoke { fingerprint } => {
                self.remove_authorized_key(fingerprint.to_ascii_lowercase())
            }
            UiAction::SetSettings { port, clipboard } => {
                if clipboard && !cfg!(any(target_os = "macos", windows)) {
                    return Err("当前平台不支持文本剪贴板同步".into());
                }
                if port != self.port {
                    self.emulation.request_port_change(port);
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            let event = self.emulation.event().await;
                            let result = match &event {
                                EmulationEvent::PortChanged(result) => {
                                    Some(result.as_ref().map(|_| ()).map_err(ToString::to_string))
                                }
                                _ => None,
                            };
                            self.handle_emulation_event(event);
                            if let Some(result) = result {
                                return result;
                            }
                        }
                    })
                    .await
                    .map_err(|_| "监听端口更改未确认，请刷新状态".to_owned())??;
                }
                // Compatibility command: an explicit change applies the text switch to each device.
                let current = self
                    .client_manager
                    .clients()
                    .iter()
                    .any(|(c, s)| s.active && c.sharing.clipboard);
                if current != clipboard {
                    for (id, mut c, _) in self.client_manager.get_client_states() {
                        c.sharing.clipboard = clipboard;
                        self.client_manager.set_config(id, c);
                    }
                }
                self.config.set_ui_settings(port, clipboard);
            }
            UiAction::SetPaused { paused } => {
                // Enter a fail-closed paused state before touching either backend.
                // Resume is published only when both tasks confirm completion.
                self.ui.paused = true;
                #[cfg(any(target_os = "macos", windows))]
                self.configure_sharing();
                #[cfg(any(target_os = "macos", windows))]
                let clipboard_result = self.clipboard.wait_until_inactive().await;
                #[cfg(any(target_os = "macos", windows))]
                let files_result = self.files.wait_until_inactive().await;
                let capture_result = self.capture.set_paused(true).await;
                let emulation_result = self.emulation.set_paused(true).await;
                capture_result?;
                emulation_result?;
                #[cfg(any(target_os = "macos", windows))]
                clipboard_result?;
                #[cfg(any(target_os = "macos", windows))]
                files_result?;
                if !paused {
                    self.emulation.set_paused(false).await?;
                    if let Err(error) = self.capture.set_paused(false).await {
                        let _ = self.emulation.set_paused(true).await;
                        let _ = self.capture.set_paused(true).await;
                        return Err(error);
                    }
                }
                self.ui.paused = paused;
                self.ui.active_client = None;
            }
            UiAction::Release => {
                self.capture.release_confirmed().await?;
                self.ui.active_client = None;
            }
            UiAction::SetHooks {
                id,
                enter_hook,
                leave_hook,
            } => {
                self.require_client(id)?;
                self.update_enter_hook(id, enter_hook);
                self.update_leave_hook(id, leave_hook);
            }
            UiAction::RetryBackends => {
                self.capture.reenable();
                self.emulation.reenable();
            }
            UiAction::Scan => {
                self.discovery
                    .configure(self.config.discovery_enabled(), self.port);
                self.discovery.scan()?;
            }
            UiAction::Shutdown => {
                self.ui.paused = true;
                #[cfg(any(target_os = "macos", windows))]
                {
                    self.configure_sharing();
                    self.clipboard.wait_until_inactive().await?;
                    self.files.wait_until_inactive().await?;
                }
                self.capture.release_confirmed().await?;
                self.emulation.set_paused(true).await?;
                self.ui.shutdown = true;
            }
        }
        if persist {
            self.persist_config()
                .map_err(|error| format!("操作已应用，但配置保存失败：{error}"))?;
        }
        self.configure_sharing();
        Ok(self.ui_snapshot())
    }

    async fn release_device(&self, id: ClientHandle) -> Result<(), String> {
        self.capture.release_client(id).await?;
        if let Some(fp) = self
            .client_manager
            .get_state(id)
            .and_then(|(c, _)| c.fingerprint)
        {
            self.emulation.release_peer(fp).await?;
        }
        Ok(())
    }
    fn require_client(&self, id: ClientHandle) -> Result<(), String> {
        self.client_manager
            .get_state(id)
            .map(|_| ())
            .ok_or_else(|| "设备已被移除，请刷新列表".into())
    }
}

fn validate(action: &UiAction) -> Result<(), String> {
    match action {
        UiAction::AddClient {
            hostname,
            ips,
            port,
            fingerprint,
            ..
        } => {
            validate_client(hostname, ips, *port)?;
            if let Some(fingerprint) = fingerprint {
                validate_fingerprint(fingerprint)?;
            }
        }
        UiAction::UpdateClient {
            hostname,
            ips,
            port,
            ..
        } => validate_client(hostname, ips, *port)?,
        UiAction::Authorize {
            description,
            fingerprint,
        } => {
            if description.trim().is_empty() || description.len() > 128 {
                return Err("请填写设备授权名称（最多 128 字节）".into());
            }
            validate_fingerprint(fingerprint)?;
        }
        UiAction::Revoke { fingerprint } => validate_fingerprint(fingerprint)?,
        UiAction::SetSettings { port, .. } => validate_port(*port)?,
        _ => {}
    }
    Ok(())
}

fn validate_port(port: u16) -> Result<(), String> {
    if port == 0 || port > 65533 || (cfg!(windows) && (5250..=5252).contains(&port)) {
        Err("监听端口无效或与本机服务端口冲突".into())
    } else {
        Ok(())
    }
}

fn validate_client(hostname: &Option<String>, ips: &[IpAddr], port: u16) -> Result<(), String> {
    validate_port(port)?;
    if hostname.as_ref().is_some_and(|name| {
        name.trim().is_empty() || name.len() > 253 || name.chars().any(char::is_whitespace)
    }) {
        return Err("主机名不能为空、含空格或超过 253 字节".into());
    }
    if ips.is_empty() && hostname.is_none() {
        return Err("请填写对端 IP 地址或主机名".into());
    }
    if ips.len() > 16
        || ips
            .iter()
            .any(|ip| !ip.is_ipv4() || ip.is_unspecified() || ip.is_multicast())
    {
        return Err("请填写有效的 IPv4 地址（最多 16 个）".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::validate_fingerprint;
    #[test]
    fn validates_pairing_before_mutating_configuration() {
        assert!(validate_fingerprint(&format!("{}ab", "ab:".repeat(31))).is_ok());
        assert!(validate_fingerprint("123456").is_err());
        assert!(validate_client(&None, &[], 4242).is_err());
        assert!(validate_client(&None, &["0.0.0.0".parse().unwrap()], 4242).is_err());
        assert!(validate_client(&None, &["192.168.1.2".parse().unwrap()], 4242).is_ok());
        assert!(validate_port(0).is_err());
    }
}
