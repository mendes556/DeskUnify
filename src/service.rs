// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
#[cfg(any(target_os = "macos", windows))]
use crate::clipboard::{ClipboardSync, Settings as ClipboardSettings};
use crate::{
    capture::{Capture, CaptureType, ICaptureEvent},
    client::ClientManager,
    config::{Config, ConfigClient},
    connect::LanMouseConnection,
    crypto,
    dns::{DnsEvent, DnsResolver},
    emulation::{Emulation, EmulationEvent},
    listen::{LanMouseListener, ListenerCreationError},
};
use futures::StreamExt;
use lan_mouse_ipc::{
    AsyncFrontendListener, ClientHandle, FrontendEvent, FrontendRequest, IpcError,
    IpcListenerCreationError, Position, Status,
};
use log;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::{process::Command, signal, sync::Notify};
mod ui;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    IpcListen(#[from] IpcListenerCreationError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ListenError(#[from] ListenerCreationError),
    #[error("failed to load certificate: `{0}`")]
    Certificate(#[from] crypto::Error),
}

pub struct Service {
    policies: crate::sharing::Policies,
    control: crate::control::Control,
    #[cfg(any(target_os = "macos", windows))]
    files: crate::files::Managed,
    #[cfg(any(target_os = "macos", windows))]
    clipboard_keys: crate::clipboard::tls::Authorized,
    #[cfg(any(target_os = "macos", windows))]
    file_keys: crate::clipboard::tls::Authorized,
    #[cfg(any(target_os = "macos", windows))]
    pair_requests: HashMap<
        String,
        (
            lan_mouse_ipc::PairRequest,
            lan_mouse_ipc::Sharing,
            tokio::sync::oneshot::Sender<crate::control::Reply>,
        ),
    >,
    rejected_pairs: HashSet<String>,
    peer_seen: HashMap<String, std::time::Instant>,
    clipboard_target: Option<String>,
    ui: ui::UiState,
    discovery: crate::discovery::Discovery,
    #[cfg(any(target_os = "macos", windows))]
    clipboard: ClipboardSync,
    /// configuration
    config: Config,
    /// input capture
    capture: Capture,
    /// input emulation
    emulation: Emulation,
    /// dns resolver
    resolver: DnsResolver,
    /// frontend listener
    frontend_listener: AsyncFrontendListener,
    /// authorized public key sha256 fingerprints
    authorized_keys: Arc<RwLock<HashMap<String, String>>>,
    /// (outgoing) client information
    client_manager: ClientManager,
    /// current port
    port: u16,
    /// the public key fingerprint for (D)TLS
    public_key_fingerprint: String,
    /// notify for pending frontend events
    frontend_event_pending: Notify,
    /// frontend events queued for sending
    pending_frontend_events: VecDeque<FrontendEvent>,
    /// status of input capture (enabled / disabled)
    capture_status: Status,
    /// status of input emulation (enabled / disabled)
    emulation_status: Status,
    /// keep track of registered connections to avoid duplicate barriers
    incoming_conns: HashSet<SocketAddr>,
    /// map from capture handle to connection info
    incoming_conn_info: HashMap<ClientHandle, Incoming>,
    next_trigger_handle: u64,
}

#[derive(Debug)]
struct Incoming {
    fingerprint: String,
    addr: SocketAddr,
    pos: Position,
}

impl Service {
    pub async fn new(config: Config) -> Result<Self, ServiceError> {
        #[cfg(any(target_os = "macos", windows))]
        if config.port() == 0
            || config.port() > 65533
            || (cfg!(windows) && (5250..=5252).contains(&config.port()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "共享端口必须为 1–65533，Windows 还需避开 5250–5252",
            )
            .into());
        }
        let client_manager = ClientManager::default();
        for client in config.clients() {
            client_manager.add_with_config(client);
        }

        // load certificate
        let cert = crypto::load_or_generate_key_and_cert(config.cert_path())?;
        let public_key_fingerprint = crypto::certificate_fingerprint(&cert);

        // create frontend communication adapter, exit if already running
        let frontend_listener = AsyncFrontendListener::new().await?;

        let authorized_keys = Arc::new(RwLock::new(config.authorized_fingerprints()));
        // listener + connection
        let listener =
            LanMouseListener::new(config.port(), cert.clone(), authorized_keys.clone()).await?;
        let policies = crate::sharing::Policies::default();
        let conn = LanMouseConnection::new(cert.clone(), client_manager.clone(), policies.clone());
        #[cfg(any(target_os = "macos", windows))]
        let clipboard_keys = crate::clipboard::tls::Authorized::default();
        #[cfg(any(target_os = "macos", windows))]
        let file_keys = crate::clipboard::tls::Authorized::default();
        #[cfg(any(target_os = "macos", windows))]
        let clipboard = ClipboardSync::new(cert.clone(), clipboard_keys.clone());
        #[cfg(any(target_os = "macos", windows))]
        let control = crate::control::Control::new(cert.clone(), authorized_keys.clone());
        #[cfg(not(any(target_os = "macos", windows)))]
        let control = crate::control::Control;
        #[cfg(any(target_os = "macos", windows))]
        let files = crate::files::Managed::new(cert.clone(), file_keys.clone());

        // input capture + emulation
        let capture_backend = config.capture_backend().map(|b| b.into());
        let capture = Capture::new(capture_backend, conn, config.release_bind());
        let emulation_backend = config.emulation_backend().map(|b| b.into());
        let emulation = Emulation::new(emulation_backend, listener, policies.clone());

        // create dns resolver
        let resolver = DnsResolver::new()?;

        let port = config.port();
        let initial_target = {
            let clients = client_manager.clients();
            let mut fps = clients
                .iter()
                .filter(|(_, s)| s.active)
                .filter_map(|(c, _)| c.fingerprint.clone());
            let first = fps.next();
            if fps.next().is_none() { first } else { None }
        };
        let service = Self {
            policies,
            rejected_pairs: Default::default(),
            peer_seen: Default::default(),
            clipboard_target: config.clipboard_target().or(initial_target),
            control,
            #[cfg(any(target_os = "macos", windows))]
            files,
            #[cfg(any(target_os = "macos", windows))]
            file_keys,
            #[cfg(any(target_os = "macos", windows))]
            clipboard_keys,
            #[cfg(any(target_os = "macos", windows))]
            pair_requests: Default::default(),
            ui: Default::default(),
            discovery: crate::discovery::Discovery::new(public_key_fingerprint.clone()),
            #[cfg(any(target_os = "macos", windows))]
            clipboard,
            config,
            capture,
            emulation,
            frontend_listener,
            resolver,
            authorized_keys,
            public_key_fingerprint,
            client_manager,
            frontend_event_pending: Default::default(),
            port,
            pending_frontend_events: Default::default(),
            capture_status: Default::default(),
            emulation_status: Default::default(),
            incoming_conn_info: Default::default(),
            incoming_conns: Default::default(),
            next_trigger_handle: 0,
        };
        Ok(service)
    }

    pub async fn run(&mut self) -> Result<(), ServiceError> {
        let active = self.client_manager.active_clients();
        for handle in active.iter() {
            // small hack: `activate_client()` checks, if the client
            // is already active in client_manager and does not create a
            // capture barrier in that case so we have to deactivate it first
            self.client_manager.deactivate_client(*handle);
        }

        for handle in active {
            self.activate_client(handle);
        }

        let mut sharing_tick = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            self.reconcile_identities();
            self.configure_sharing();
            self.discovery
                .configure(self.config.discovery_enabled(), self.port);
            #[cfg(any(target_os = "macos", windows))]
            self.configure_clipboard();
            tokio::select! {
                _ = sharing_tick.tick() => {},
                event = control_event(&mut self.control) => self.handle_control_dispatch(event).await,
                request = self.frontend_listener.next() => self.handle_frontend_request(request).await,
                _ = self.frontend_event_pending.notified() => self.handle_frontend_pending().await,
                event = self.emulation.event() => self.handle_emulation_event(event),
                event = self.capture.event() => self.handle_capture_event(event),
                event = self.resolver.event() => self.handle_resolver_event(event),
                _ = self.config.changed() => self.handle_config_change(),
                r = signal::ctrl_c() => break r.expect("failed to wait for CTRL+C"),
            }
            if self.ui.shutdown {
                self.handle_frontend_pending().await;
                break;
            }
        }

        log::info!("terminating service ...");
        self.discovery.stop();
        log::debug!("terminating capture ...");
        self.capture.terminate().await;
        log::debug!("terminating emulation ...");
        self.emulation.terminate().await;
        log::debug!("terminating dns resolver ...");
        self.resolver.terminate().await;
        #[cfg(any(target_os = "macos", windows))]
        {
            self.clipboard.terminate().await;
            self.control.terminate().await;
            self.files.terminate().await;
        }

        Ok(())
    }

    fn reconcile_identities(&mut self) {
        let discovered = self.discovery.devices();
        let authorized = self.authorized_keys.read().expect("lock").clone();
        let mut changed = false;
        for (id, mut c, s) in self.client_manager.get_client_states() {
            if c.fingerprint.is_some() {
                continue;
            }
            let matches = discovered
                .iter()
                .filter(|peer| {
                    peer.port == c.port
                        && authorized.contains_key(&peer.fingerprint)
                        && peer.ips.iter().any(|ip| s.ips.contains(ip))
                })
                .collect::<Vec<_>>();
            if matches.len() == 1 {
                c.fingerprint = Some(matches[0].fingerprint.clone());
                self.client_manager.set_config(id, c);
                changed = true;
            }
        }
        if changed {
            if self.clipboard_target.is_none() {
                let mut peers = self
                    .client_manager
                    .clients()
                    .into_iter()
                    .filter(|(_, s)| s.active)
                    .filter_map(|(c, _)| c.fingerprint);
                let first = peers.next();
                if peers.next().is_none() {
                    self.clipboard_target = first;
                }
            }
            self.save_config();
        }
    }
    #[cfg(any(target_os = "macos", windows))]
    fn peer_note(&self, active: bool) -> Option<String> {
        if self.ui.paused {
            Some("对端已暂停共享".into())
        } else if !active {
            Some("对端已停用此设备".into())
        } else {
            self.ui
                .emulation_error
                .as_ref()
                .map(|error| format!("对端输入模拟尚未就绪：{error}"))
        }
    }
    fn configure_sharing(&self) {
        let mut policies = HashMap::new();
        let authorized = self.authorized_keys.read().expect("lock");
        for (_, config, state) in self.client_manager.get_client_states() {
            if let Some(fp) = config.fingerprint {
                #[cfg(any(target_os = "macos", windows))]
                let live = self
                    .peer_seen
                    .get(&fp)
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(4));
                #[cfg(not(any(target_os = "macos", windows)))]
                let live = true;
                #[cfg(any(target_os = "macos", windows))]
                let remote = state.peer_sharing.unwrap_or(lan_mouse_ipc::Sharing::OFF);
                #[cfg(not(any(target_os = "macos", windows)))]
                let remote = lan_mouse_ipc::Sharing::default();
                let effective =
                    if state.active && !self.ui.paused && live && authorized.contains_key(&fp) {
                        config.sharing.intersect(remote)
                    } else {
                        lan_mouse_ipc::Sharing::OFF
                    };
                policies.insert(fp, effective);
            }
        }
        *self.policies.write().expect("lock") = policies;
        #[cfg(any(target_os = "macos", windows))]
        {
            let peers = self
                .client_manager
                .get_client_states()
                .into_iter()
                .filter_map(|(id, c, s)| {
                    let fp = c.fingerprint?;
                    if !authorized.contains_key(&fp) {
                        return None;
                    }
                    let ip = s
                        .peer_addr
                        .or(s.active_addr)
                        .map(|a| a.ip())
                        .or_else(|| c.fix_ips.first().copied())
                        .or_else(|| s.dns_ips.first().copied())?;
                    Some(crate::control::Peer {
                        id,
                        fingerprint: fp,
                        addr: SocketAddr::new(ip, c.port),
                        alternates: s
                            .ips
                            .iter()
                            .filter(|other| **other != ip)
                            .map(|ip| SocketAddr::new(*ip, c.port))
                            .collect(),
                        position: c.pos,
                        note: self.peer_note(s.active),
                        sharing: if s.active && !self.ui.paused {
                            c.sharing
                        } else {
                            lan_mouse_ipc::Sharing::OFF
                        },
                    })
                })
                .collect();
            self.control.configure(crate::control::Settings {
                port: self.port,
                peers,
            });
            let policies = self.policies.read().expect("lock");
            *self.clipboard_keys.write().expect("lock") = authorized
                .iter()
                .filter(|(fp, _)| policies.get(*fp).is_some_and(|p| p.clipboard))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            *self.file_keys.write().expect("lock") = authorized
                .iter()
                .filter(|(fp, _)| policies.get(*fp).is_some_and(|p| p.files))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            drop(policies);
            self.configure_clipboard();
            let target = self.target_client().and_then(|(_, c, s)| {
                let fp = c.fingerprint?;
                if !self.file_keys.read().expect("lock").contains_key(&fp) {
                    return None;
                }
                let ip = s
                    .peer_addr
                    .or(s.active_addr)
                    .map(|a| a.ip())
                    .or_else(|| c.fix_ips.first().copied())?;
                Some((SocketAddr::new(ip, c.port.checked_add(1)?), fp))
            });
            self.files.configure(crate::files::ManagedSettings {
                port: self.port.saturating_add(1),
                output: self.config.file_directory(),
                target,
                enabled: !self.ui.paused && !self.file_keys.read().expect("lock").is_empty(),
            });
        }
    }
    fn target_client(
        &self,
    ) -> Option<(
        ClientHandle,
        lan_mouse_ipc::ClientConfig,
        lan_mouse_ipc::ClientState,
    )> {
        let fp = self.clipboard_target.as_deref()?;
        let id = self.client_manager.by_fingerprint(fp)?;
        let (c, s) = self.client_manager.get_state(id)?;
        Some((id, c, s))
    }
    #[cfg(any(target_os = "macos", windows))]
    fn configure_clipboard(&self) {
        let peers = self
            .target_client()
            .into_iter()
            .filter(|(_, c, _)| {
                c.fingerprint
                    .as_ref()
                    .is_some_and(|fp| self.clipboard_keys.read().expect("lock").contains_key(fp))
            })
            .flat_map(|(_, c, s)| {
                let ip = s
                    .peer_addr
                    .or(s.active_addr)
                    .map(|a| a.ip())
                    .or_else(|| c.fix_ips.first().copied());
                ip.map(|ip| SocketAddr::new(ip, c.port))
            })
            .collect();
        let fingerprint = self.target_client().and_then(|(_, c, _)| c.fingerprint);
        self.clipboard.configure(ClipboardSettings {
            fingerprint,
            enabled: !self.ui.paused && !self.clipboard_keys.read().expect("lock").is_empty(),
            port: self.port,
            peers,
        });
    }
    async fn handle_control_dispatch(&mut self, event: crate::control::Event) {
        #[cfg(any(target_os = "macos", windows))]
        self.handle_control_event(event).await;
        #[cfg(not(any(target_os = "macos", windows)))]
        let _ = event;
    }
    #[cfg(any(target_os = "macos", windows))]
    async fn handle_control_event(&mut self, event: crate::control::Event) {
        use crate::control::{Event, Reply};
        match event {
            Event::Request {
                request,
                sharing,
                note,
                reply,
            } => {
                let fp = request.fingerprint.clone();
                if self.rejected_pairs.contains(&fp)
                    || (self.client_manager.by_fingerprint(&fp).is_some()
                        && !self.authorized_keys.read().expect("lock").contains_key(&fp))
                {
                    let _ = reply.send(Reply::rejected("对端拒绝配对，请由对端重新授权"));
                } else if self.authorized_keys.read().expect("lock").contains_key(&fp) {
                    let id = self.ensure_reverse_client(&request);
                    if let Some((c, mut s)) = self.client_manager.get_state(id) {
                        s.peer_sharing = Some(sharing);
                        s.peer_note = note;
                        s.sharing_error = None;
                        self.client_manager.set_state(id, s.clone());
                        self.peer_seen.insert(fp, std::time::Instant::now());
                        let mut response = Reply::accepted(if s.active && !self.ui.paused {
                            c.sharing
                        } else {
                            lan_mouse_ipc::Sharing::OFF
                        });
                        response.note = self.peer_note(s.active);
                        let _ = reply.send(response);
                    }
                } else if self.pair_requests.len() < 16 && !self.pair_requests.contains_key(&fp) {
                    self.pair_requests.insert(fp, (request, sharing, reply));
                } else {
                    let _ = reply.send(Reply::rejected("已有待确认请求，请在对端确认配对"));
                }
            }
            Event::Updated {
                id,
                fingerprint,
                result,
            } => {
                if let Some((c, mut s)) = self
                    .client_manager
                    .get_state(id)
                    .filter(|(c, _)| c.fingerprint.as_deref() == Some(&fingerprint))
                {
                    match result {
                        Ok(response) => {
                            s.peer_sharing = Some(response.sharing);
                            s.peer_note = response.note;
                            s.peer_addr = response.verified_addr;
                            s.sharing_error = None;
                            self.peer_seen
                                .insert(fingerprint, std::time::Instant::now());
                        }
                        Err(error) => {
                            s.peer_sharing = None;
                            s.sharing_error = Some(error);
                            self.peer_seen.remove(&fingerprint);
                        }
                    }
                    self.client_manager.set_state(id, s);
                    let _ = c;
                }
            }
            Event::Error(error) => log::warn!("pairing service: {error}"),
        }
        self.pair_requests
            .retain(|_, (_, _, reply)| !reply.is_closed());
        self.configure_sharing();
    }
    #[cfg(any(target_os = "macos", windows))]
    fn ensure_reverse_client(&mut self, request: &lan_mouse_ipc::PairRequest) -> ClientHandle {
        if let Some(id) = self.client_manager.by_fingerprint(&request.fingerprint) {
            if let Some((mut c, _)) = self.client_manager.get_state(id) {
                // Preserve policy, enabled state and user layout; refresh the authenticated address.
                if !c.fix_ips.contains(&request.ip) {
                    c.fix_ips = vec![request.ip];
                    self.client_manager.set_config(id, c);
                    self.client_manager.set_fix_ips(id, vec![request.ip]);
                    self.save_config();
                }
            }
            return id;
        }
        // Bind a legacy IP entry once, only after certificate authorization.
        let legacy = self
            .client_manager
            .get_client_states()
            .into_iter()
            .find(|(_, c, _)| {
                c.fingerprint.is_none() && c.fix_ips.contains(&request.ip) && c.port == request.port
            });
        if let Some((id, mut c, _)) = legacy {
            c.fingerprint = Some(request.fingerprint.clone());
            self.client_manager.set_config(id, c);
            self.save_config();
            return id;
        }
        let desired = request.position.opposite();
        let position = [
            desired,
            Position::Left,
            Position::Right,
            Position::Top,
            Position::Bottom,
        ]
        .into_iter()
        .find(|p| self.client_manager.client_at(*p).is_none());
        let id = self.client_manager.add_with_config(ConfigClient {
            fingerprint: Some(request.fingerprint.clone()),
            sharing: Default::default(),
            ips: HashSet::from([request.ip]),
            hostname: None,
            port: request.port,
            pos: position.unwrap_or(desired),
            active: false,
            enter_hook: None,
            leave_hook: None,
        });
        if position.is_some() {
            self.activate_client(id);
        }
        if self.clipboard_target.is_none() {
            self.clipboard_target = Some(request.fingerprint.clone());
        }
        self.save_config();
        self.broadcast_client(id);
        id
    }
    #[cfg(any(target_os = "macos", windows))]
    fn approve_pair(&mut self, fp: &str) {
        self.rejected_pairs.remove(fp);
        if let Some((request, sharing, reply)) = self.pair_requests.remove(fp) {
            let id = self.ensure_reverse_client(&request);
            let (c, mut s) = self.client_manager.get_state(id).expect("paired client");
            s.peer_sharing = Some(sharing);
            self.client_manager.set_state(id, s.clone());
            self.peer_seen
                .insert(fp.to_owned(), std::time::Instant::now());
            let _ = reply.send(crate::control::Reply::accepted(
                if s.active && !self.ui.paused {
                    c.sharing
                } else {
                    lan_mouse_ipc::Sharing::OFF
                },
            ));
        }
    }

    async fn handle_frontend_request(
        &mut self,
        request: Option<Result<FrontendRequest, IpcError>>,
    ) {
        let request = match request.expect("frontend listener closed") {
            Ok(r) => r,
            Err(e) => return log::error!("error receiving request: {e}"),
        };
        match request {
            FrontendRequest::Ui { id, action } => {
                let result = self.handle_ui_action(action).await;
                self.notify_frontend(FrontendEvent::UiResult { id, result });
            }
            FrontendRequest::Activate(handle, active) => {
                self.set_client_active(handle, active);
                self.save_config();
            }
            FrontendRequest::AuthorizeKey(desc, fp) => {
                self.add_authorized_key(desc, fp);
                self.save_config();
            }
            FrontendRequest::ChangePort(port) => self.change_port(port),
            FrontendRequest::Create => {
                self.add_client();
                self.save_config();
            }
            FrontendRequest::Delete(handle) => {
                self.remove_client(handle);
                self.save_config();
            }
            FrontendRequest::EnableCapture => self.capture.reenable(),
            FrontendRequest::EnableEmulation => self.emulation.reenable(),
            FrontendRequest::Enumerate() => self.enumerate(),
            FrontendRequest::UpdateFixIps(handle, fix_ips) => {
                self.update_fix_ips(handle, fix_ips);
                self.save_config();
            }
            FrontendRequest::UpdateHostname(handle, host) => {
                self.update_hostname(handle, host);
                self.save_config();
            }
            FrontendRequest::UpdatePort(handle, port) => {
                self.update_port(handle, port);
                self.save_config();
            }
            FrontendRequest::UpdatePosition(handle, pos) => {
                self.update_pos(handle, pos);
                self.save_config();
            }
            FrontendRequest::ResolveDns(handle) => self.resolve(handle),
            FrontendRequest::Sync => self.sync_frontend(),
            FrontendRequest::RemoveAuthorizedKey(key) => {
                self.remove_authorized_key(key);
                self.save_config();
            }
            FrontendRequest::UpdateEnterHook(handle, enter_hook) => {
                self.update_enter_hook(handle, enter_hook)
            }
            FrontendRequest::UpdateLeaveHook(handle, leave_hook) => {
                self.update_leave_hook(handle, leave_hook)
            }
            FrontendRequest::SaveConfiguration => self.save_config(),
        }
    }

    fn save_config(&mut self) {
        if let Err(e) = self.persist_config() {
            log::warn!("failed to write config: {e}");
        }
    }

    fn persist_config(&mut self) -> Result<(), io::Error> {
        let clients = self.client_manager.clients();
        let clients = clients
            .into_iter()
            .map(|(c, s)| ConfigClient {
                fingerprint: c.fingerprint,
                sharing: c.sharing,
                ips: HashSet::from_iter(c.fix_ips),
                hostname: c.hostname,
                port: c.port,
                pos: c.pos,
                active: s.active,
                enter_hook: c.cmd,
                leave_hook: c.leave_cmd,
            })
            .collect();
        self.config
            .set_clipboard_target(self.clipboard_target.clone());
        self.config.set_clients(clients);
        let authorized_keys = self.authorized_keys.read().expect("lock").clone();
        self.config.set_authorized_keys(authorized_keys);
        self.config.write_back()
    }

    fn handle_config_change(&mut self) {
        for h in self.client_manager.registered_clients() {
            self.remove_client(h);
        }
        for c in self.config.clients() {
            let handle = self.client_manager.add_with_config(c);
            log::info!("added client {handle}");
            let (c, s) = self.client_manager.get_state(handle).unwrap();
            if s.active {
                self.client_manager.deactivate_client(handle);
                self.activate_client(handle);
            }
            self.notify_frontend(FrontendEvent::Created(handle, c, s));
        }
        let release_bind = self.config.release_bind();
        self.capture.set_release_bind(release_bind);
        let authorized_keys = self.config.authorized_fingerprints();
        self.authorized_keys
            .write()
            .unwrap()
            .clone_from(&authorized_keys);
        self.sync_frontend();
    }

    async fn handle_frontend_pending(&mut self) {
        while let Some(event) = self.pending_frontend_events.pop_front() {
            self.frontend_listener.broadcast(event).await;
        }
    }

    fn handle_emulation_event(&mut self, event: EmulationEvent) {
        match event {
            EmulationEvent::ConnectionAttempt { fingerprint } => {
                if !self.ui.attempts.contains(&fingerprint) {
                    self.ui.attempts.push_front(fingerprint.clone());
                    self.ui.attempts.truncate(16);
                }
                self.notify_frontend(FrontendEvent::ConnectionAttempt { fingerprint });
            }
            EmulationEvent::Entered {
                addr,
                pos,
                fingerprint,
            } => {
                self.clipboard_target = Some(fingerprint.clone());
                self.save_config();
                // check if already registered
                if !self.incoming_conns.contains(&addr) {
                    self.add_incoming(addr, pos, fingerprint.clone());
                    self.notify_frontend(FrontendEvent::DeviceEntered {
                        fingerprint,
                        addr,
                        pos,
                    });
                } else {
                    self.update_incoming(addr, pos, fingerprint);
                }
            }
            EmulationEvent::Disconnected { addr } => {
                if let Some(addr) = self.remove_incoming(addr) {
                    self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
                }
            }
            EmulationEvent::PortChanged(port) => match port {
                Ok(port) => {
                    self.port = port;
                    self.notify_frontend(FrontendEvent::PortChanged(port, None));
                }
                Err(e) => self
                    .notify_frontend(FrontendEvent::PortChanged(self.port, Some(format!("{e}")))),
            },
            EmulationEvent::EmulationDisabled => {
                self.ui.emulation_backend = None;
                self.emulation_status = Status::Disabled;
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
            }
            EmulationEvent::EmulationFailed(error) => {
                self.ui.emulation_error = Some(error);
            }
            EmulationEvent::EmulationEnabled(backend) => {
                self.ui.emulation_backend = Some(backend);
                self.ui.emulation_error = None;
                self.emulation_status = Status::Enabled;
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
            }
            EmulationEvent::ReleaseNotify => self.capture.release(),
            EmulationEvent::Connected { addr, fingerprint } => {
                self.notify_frontend(FrontendEvent::DeviceConnected { addr, fingerprint });
            }
            EmulationEvent::PeerHello { addr, commit } => {
                // Map the peer's source addr back to its client handle
                // and stamp the commit. Skip if we don't have an
                // outgoing client configured for this peer (incoming-
                // only setup) — there's nowhere to display the version
                // in that case anyway.
                if let Some(handle) = self.client_manager.get_client(addr) {
                    self.client_manager.set_peer_commit(handle, Some(commit));
                    self.broadcast_client(handle);
                }
            }
        }
    }

    fn handle_capture_event(&mut self, event: ICaptureEvent) {
        match event {
            ICaptureEvent::CaptureBegin(handle) => {
                // we entered the capture zone for an incoming connection
                // => notify it that its capture should be released
                if let Some(incoming) = self.incoming_conn_info.get(&handle) {
                    self.emulation.send_leave_event(incoming.addr);
                }
            }
            ICaptureEvent::CaptureDisabled => {
                self.ui.capture_backend = None;
                self.capture_status = Status::Disabled;
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
            }
            ICaptureEvent::CaptureFailed(error) => {
                self.ui.capture_error = Some(error);
            }
            ICaptureEvent::CaptureEnabled(backend) => {
                self.ui.capture_backend = Some(backend);
                self.ui.capture_error = None;
                self.capture_status = Status::Enabled;
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
            }
            ICaptureEvent::ClientEntered(handle) => {
                if !self.ui.paused {
                    self.ui.active_client = Some(handle);
                    self.clipboard_target = self
                        .client_manager
                        .get_state(handle)
                        .and_then(|(c, _)| c.fingerprint);
                    self.save_config();
                }
                log::info!("entering client {handle} ...");
                self.spawn_hook_command(handle, HookKind::Enter);
            }
            ICaptureEvent::ClientLeft(handle) => {
                if self.ui.active_client == Some(handle) {
                    self.ui.active_client = None;
                }
                log::info!("leaving client {handle} ...");
                self.spawn_hook_command(handle, HookKind::Leave);
            }
        }
    }

    fn handle_resolver_event(&mut self, event: DnsEvent) {
        let handle = match event {
            DnsEvent::Resolving(handle) => {
                self.client_manager.set_resolving(handle, true);
                handle
            }
            DnsEvent::Resolved(handle, hostname, ips) => {
                self.client_manager.set_resolving(handle, false);
                if let Err(e) = &ips {
                    log::warn!("could not resolve {hostname}: {e}");
                }
                let ips = ips.unwrap_or_default();
                self.client_manager.set_dns_ips(handle, ips);
                handle
            }
        };
        self.broadcast_client(handle);
    }

    fn resolve(&self, handle: ClientHandle) {
        if let Some(hostname) = self.client_manager.get_hostname(handle) {
            self.resolver.resolve(handle, hostname);
        }
    }

    fn sync_frontend(&mut self) {
        self.enumerate();
        self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
        self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
        self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
        self.notify_frontend(FrontendEvent::PublicKeyFingerprint(
            self.public_key_fingerprint.clone(),
        ));
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    const ENTER_HANDLE_BEGIN: u64 = u64::MAX / 2 + 1;

    fn add_incoming(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) {
        let handle = Self::ENTER_HANDLE_BEGIN + self.next_trigger_handle;
        self.next_trigger_handle += 1;
        self.capture.create(handle, pos, CaptureType::EnterOnly);
        self.incoming_conns.insert(addr);
        self.incoming_conn_info.insert(
            handle,
            Incoming {
                fingerprint,
                addr,
                pos,
            },
        );
    }

    fn update_incoming(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) {
        let incoming = self
            .incoming_conn_info
            .iter_mut()
            .find(|(_, i)| i.addr == addr)
            .map(|(_, i)| i)
            .expect("no such client");
        let mut changed = false;
        if incoming.fingerprint != fingerprint {
            incoming.fingerprint = fingerprint.clone();
            changed = true;
        }
        if incoming.pos != pos {
            incoming.pos = pos;
            changed = true;
        }
        if changed {
            self.remove_incoming(addr);
            self.add_incoming(addr, pos, fingerprint.clone());
            self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
            self.notify_frontend(FrontendEvent::DeviceEntered {
                fingerprint,
                addr,
                pos,
            });
        }
    }

    fn remove_incoming(&mut self, addr: SocketAddr) -> Option<SocketAddr> {
        let handle = self
            .incoming_conn_info
            .iter()
            .find(|(_, incoming)| incoming.addr == addr)
            .map(|(k, _)| *k)?;
        self.capture.destroy(handle);
        self.incoming_conns.remove(&addr);
        self.incoming_conn_info
            .remove(&handle)
            .map(|incoming| incoming.addr)
    }

    fn notify_frontend(&mut self, event: FrontendEvent) {
        self.pending_frontend_events.push_back(event);
        self.frontend_event_pending.notify_one();
    }

    fn add_authorized_key(&mut self, desc: String, fp: String) {
        self.ui.attempts.retain(|attempt| attempt != &fp);
        self.authorized_keys.write().expect("lock").insert(fp, desc);
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn remove_authorized_key(&mut self, fp: String) {
        self.rejected_pairs.insert(fp.clone());
        #[cfg(any(target_os = "macos", windows))]
        if let Some((_, _, reply)) = self.pair_requests.remove(&fp) {
            let _ = reply.send(crate::control::Reply::rejected("设备授权已撤销"));
        }
        self.authorized_keys.write().expect("lock").remove(&fp);
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn enumerate(&mut self) {
        let clients = self.client_manager.get_client_states();
        self.notify_frontend(FrontendEvent::Enumerate(clients));
    }

    fn add_client(&mut self) {
        let handle = self.client_manager.add_client();
        log::info!("added client {handle}");
        let (c, s) = self.client_manager.get_state(handle).unwrap();
        self.notify_frontend(FrontendEvent::Created(handle, c, s));
    }

    fn set_client_active(&mut self, handle: ClientHandle, active: bool) {
        if active {
            self.activate_client(handle);
        } else {
            self.deactivate_client(handle);
        }
    }

    fn deactivate_client(&mut self, handle: ClientHandle) {
        log::debug!("deactivating client {handle}");
        if self.client_manager.deactivate_client(handle) {
            self.capture.destroy(handle);
            self.broadcast_client(handle);
            log::info!("deactivated client {handle}");
        }
    }

    fn activate_client(&mut self, handle: ClientHandle) {
        log::debug!("activating client {handle}");

        /* resolve dns on activate */
        self.resolve(handle);

        /* deactivate potential other client at this position */
        let Some(pos) = self.client_manager.get_pos(handle) else {
            return;
        };

        if let Some(other) = self.client_manager.client_at(pos) {
            if other != handle {
                self.deactivate_client(other);
            }
        }

        /* activate the client */
        if self.client_manager.activate_client(handle) {
            /* notify capture and frontends */
            self.capture.create(handle, pos, CaptureType::Default);
            self.broadcast_client(handle);
            log::info!("activated client {handle} ({pos})");
        }
    }

    fn change_port(&mut self, port: u16) {
        if self.port != port {
            self.emulation.request_port_change(port);
        } else {
            self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
        }
    }

    fn remove_client(&mut self, handle: ClientHandle) {
        if self
            .client_manager
            .remove_client(handle)
            .map(|(_, s)| s.active)
            .unwrap_or(false)
        {
            self.capture.destroy(handle);
        }
        self.notify_frontend(FrontendEvent::Deleted(handle));
    }

    fn update_fix_ips(&mut self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        self.client_manager.set_fix_ips(handle, fix_ips);
        self.broadcast_client(handle);
    }

    fn update_hostname(&mut self, handle: ClientHandle, hostname: Option<String>) {
        log::info!("hostname changed: {hostname:?}");
        if self.client_manager.set_hostname(handle, hostname.clone()) {
            self.resolve(handle);
        }
        self.broadcast_client(handle);
    }

    fn update_port(&mut self, handle: ClientHandle, port: u16) {
        self.client_manager.set_port(handle, port);
        self.broadcast_client(handle);
    }

    fn update_pos(&mut self, handle: ClientHandle, pos: Position) {
        // update state in event input emulator & input capture
        let active = self
            .client_manager
            .get_state(handle)
            .is_some_and(|(_, state)| state.active);
        if self.client_manager.set_pos(handle, pos) && active {
            self.deactivate_client(handle);
            self.activate_client(handle);
        }
        self.broadcast_client(handle);
    }

    fn update_enter_hook(&mut self, handle: ClientHandle, enter_hook: Option<String>) {
        self.client_manager.set_enter_hook(handle, enter_hook);
        self.broadcast_client(handle);
    }

    fn update_leave_hook(&mut self, handle: ClientHandle, leave_hook: Option<String>) {
        self.client_manager.set_leave_hook(handle, leave_hook);
        self.broadcast_client(handle);
    }

    fn broadcast_client(&mut self, handle: ClientHandle) {
        let event = self
            .client_manager
            .get_state(handle)
            .map(|(c, s)| FrontendEvent::State(handle, c, s))
            .unwrap_or(FrontendEvent::NoSuchClient(handle));
        self.notify_frontend(event);
    }

    fn spawn_hook_command(&self, handle: ClientHandle, kind: HookKind) {
        let cmd = match kind {
            HookKind::Enter => self.client_manager.get_enter_cmd(handle),
            HookKind::Leave => self.client_manager.get_leave_cmd(handle),
        };
        let Some(cmd) = cmd else { return };
        tokio::task::spawn_local(async move {
            log::info!("spawning {kind} hook for client {handle}");
            let mut child = match Command::new("sh").arg("-c").arg(cmd.as_str()).spawn() {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("could not execute {kind} hook for client {handle}: {e}");
                    return;
                }
            };
            match child.wait().await {
                Ok(s) => {
                    if s.success() {
                        log::info!("{kind} hook for client {handle} ({cmd}) exited successfully");
                    } else {
                        log::warn!("{kind} hook for client {handle} ({cmd}) exited with {s}");
                    }
                }
                Err(e) => log::warn!("{kind} hook for client {handle} ({cmd}): {e}"),
            }
        });
    }
}

#[derive(Clone, Copy, Debug)]
enum HookKind {
    Enter,
    Leave,
}

impl std::fmt::Display for HookKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookKind::Enter => f.write_str("enter"),
            HookKind::Leave => f.write_str("leave"),
        }
    }
}

async fn control_event(control: &mut crate::control::Control) -> crate::control::Event {
    control.event().await
}
