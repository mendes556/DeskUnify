// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
mod native;
mod state;
pub(crate) mod tls;
mod wire;

use native::NativeClipboard;
use state::{Revision, State, Update, node};
use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tls::{ALPN, Authorized, TlsConfig};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::{MissedTickBehavior, interval, timeout},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use webrtc_dtls::crypto::Certificate;

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 16;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Settings {
    pub enabled: bool,
    pub port: u16,
    pub peers: Vec<SocketAddr>,
    pub fingerprint: Option<String>,
}

pub(crate) struct ClipboardSync {
    settings: watch::Sender<Settings>,
    inactive: watch::Receiver<bool>,
    task: JoinHandle<()>,
}

struct Incoming {
    cert: Vec<u8>,
    update: Update,
    reply: oneshot::Sender<bool>,
}

trait ClipboardStore: Sync {
    fn read(&self) -> impl std::future::Future<Output = io::Result<Option<String>>> + Send;
    fn write(&self, text: String) -> impl std::future::Future<Output = io::Result<()>> + Send;
}

impl ClipboardStore for NativeClipboard {
    async fn read(&self) -> io::Result<Option<String>> {
        NativeClipboard::read(self).await
    }
    async fn write(&self, text: String) -> io::Result<()> {
        NativeClipboard::write(self, text).await
    }
}

impl ClipboardSync {
    pub fn new(cert: Certificate, authorized: Authorized) -> Self {
        let (settings, changes) = watch::channel(Settings::default());
        let (inactive_tx, inactive) = watch::channel(true);
        let task = tokio::task::spawn_local(run(cert, authorized, changes, inactive_tx));
        Self {
            settings,
            inactive,
            task,
        }
    }

    pub fn configure(&self, mut settings: Settings) {
        settings.peers.sort();
        settings.peers.dedup();
        self.settings.send_if_modified(|current| {
            if *current == settings {
                false
            } else {
                *current = settings;
                true
            }
        });
    }

    pub async fn wait_until_inactive(&mut self) -> Result<(), String> {
        timeout(Duration::from_secs(5), async {
            while !*self.inactive.borrow_and_update() {
                self.inactive
                    .changed()
                    .await
                    .map_err(|_| "剪贴板任务已停止".to_owned())?;
            }
            Ok(())
        })
        .await
        .map_err(|_| "剪贴板暂停未确认".to_owned())?
    }

    pub async fn terminate(&mut self) {
        self.configure(Settings::default());
        // Closing the sender lets the session clean up its native worker.
        let (replacement, _) = watch::channel(Settings::default());
        self.settings = replacement;
        if let Err(error) = (&mut self.task).await {
            log::warn!("clipboard shutdown: {error}");
        }
    }
}

async fn run(
    cert: Certificate,
    authorized: Authorized,
    mut settings: watch::Receiver<Settings>,
    inactive: watch::Sender<bool>,
) {
    let config = match TlsConfig::new(&cert, authorized.clone()) {
        Ok(config) => config,
        Err(error) => {
            log::error!("clipboard TLS configuration: {error}");
            return;
        }
    };
    let origin = node(cert.certificate[0].as_ref());
    loop {
        let current = settings.borrow_and_update().clone();
        if !current.enabled {
            if settings.changed().await.is_err() {
                break;
            }
            continue;
        }
        match TcpListener::bind(("0.0.0.0", current.port)).await {
            Ok(listener) => {
                if !settings.borrow().enabled {
                    continue;
                }
                log::info!("text clipboard sync listening on TCP port {}", current.port);
                inactive.send_replace(false);
                if let Err(error) =
                    session(listener, origin, &config, authorized.clone(), &mut settings).await
                {
                    log::warn!("clipboard session: {error}");
                }
                inactive.send_replace(true);
            }
            Err(error) => log::warn!("clipboard TCP port {}: {error}", current.port),
        }
        tokio::select! {
            result = settings.changed() => if result.is_err() { break; },
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
        if settings.has_changed().is_err() {
            break;
        }
    }
}

async fn session(
    listener: TcpListener,
    origin: state::Node,
    config: &TlsConfig,
    authorized: Authorized,
    settings: &mut watch::Receiver<Settings>,
) -> io::Result<()> {
    let clipboard = NativeClipboard::new()?;
    let result =
        session_with_clipboard(listener, origin, config, authorized, settings, &clipboard).await;
    clipboard.terminate().await;
    result
}

async fn session_with_clipboard(
    listener: TcpListener,
    origin: state::Node,
    config: &TlsConfig,
    authorized: Authorized,
    settings: &mut watch::Receiver<Settings>,
    clipboard: &impl ClipboardStore,
) -> io::Result<()> {
    let port = settings.borrow().port;
    // Seeding avoids ordinary restarts reusing a device's old revisions.
    // Thereafter this is a Lamport clock, advanced by every observed update.
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut state = State::new(origin, seed);
    let mut latest: Option<Arc<Update>> = None;
    let mut delivered = HashMap::<SocketAddr, Revision>::new();
    let mut sending = HashSet::new();
    let mut outgoing = JoinSet::new();
    let mut connections = JoinSet::new();
    let (incoming, mut received) = mpsc::channel::<Incoming>(MAX_CONNECTIONS);
    let mut poll = interval(Duration::from_millis(300));
    let mut retry = interval(Duration::from_secs(1));
    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    retry.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            changed = settings.changed() => {
                if changed.is_err() { break; }
                let current = settings.borrow_and_update().clone();
                if !current.enabled || current.port != port { break; }
                // A target change must not replay a previous copy to a new recipient.
                latest=None;
                // Removing a destination also cancels any transfer in flight.
                outgoing.abort_all();
                while outgoing.join_next().await.is_some() {}
                sending.clear();
                delivered.retain(|peer, _| current.peers.contains(peer));
            }
            _ = poll.tick() => {
                match clipboard.read().await {
                    Ok(text) => observe(&mut state, &mut latest, text),
                    Err(error) => log::debug!("clipboard read unavailable: {error}"),
                }
            }
            Some(message) = received.recv() => {
                let result = apply_incoming(clipboard, &mut state, &mut latest, &authorized, &message).await;
                if let Err(error) = &result { log::debug!("clipboard update not applied: {error}"); }
                let _ = message.reply.send(result.is_ok());
            }
            _ = retry.tick() => {
                if let Some(update) = &latest {
                    let peers = settings.borrow().peers.clone();
                    // Bound both concurrent connections and their 1 MiB payloads.
                    for peer in peers {
                        if sending.len() >= MAX_CONNECTIONS { break; }
                        if delivered.get(&peer) == Some(&update.revision) || !sending.insert(peer) { continue; }
                        let update = update.clone();
                        let client = config.client.clone();
                        let keys = authorized.clone();
                        let fingerprint=settings.borrow().fingerprint.clone();
                        outgoing.spawn(async move {
                            let result = timeout(TRANSFER_TIMEOUT, send_update_pinned(peer, client, keys, &update, fingerprint.as_deref())).await;
                            (peer, update.revision, matches!(result, Ok(Ok(()))))
                        });
                    }
                }
            }
            Some(result) = outgoing.join_next() => match result {
                Ok((peer, revision, success)) => {
                    sending.remove(&peer);
                    if success { delivered.insert(peer, revision); }
                    else { log::debug!("clipboard delivery to {peer} failed; will retry latest text"); }
                }
                Err(error) => log::warn!("clipboard sender task: {error}"),
            },
            connection = listener.accept() => match connection {
                Ok((stream, peer)) if connections.len() < MAX_CONNECTIONS => {
                    let acceptor = TlsAcceptor::from(config.server.clone());
                    let incoming = incoming.clone();
                    let keys = authorized.clone();
                    connections.spawn(async move {
                        match timeout(TRANSFER_TIMEOUT, receive_update(stream, acceptor, keys, incoming)).await {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => log::debug!("clipboard connection {peer}: {error}"),
                            Err(_) => log::debug!("clipboard connection {peer} timed out"),
                        }
                    });
                }
                Ok(_) => {},
                Err(error) => { log::warn!("clipboard accept: {error}"); break; }
            },
            Some(_) = connections.join_next() => {}
        }
    }
    outgoing.abort_all();
    connections.abort_all();
    while outgoing.join_next().await.is_some() {}
    while connections.join_next().await.is_some() {}
    Ok(())
}

async fn apply_incoming(
    clipboard: &impl ClipboardStore,
    state: &mut State,
    latest: &mut Option<Arc<Update>>,
    authorized: &Authorized,
    message: &Incoming,
) -> io::Result<()> {
    if !tls::authorized(authorized, &message.cert) {
        return Err(wire::invalid("clipboard peer authorization was removed"));
    }
    // Read once more before overwriting, so a local copy since the last poll
    // enters the same conflict ordering as the arriving remote update.
    observe(state, latest, clipboard.read().await?);
    if !tls::authorized(authorized, &message.cert) {
        return Err(wire::invalid("clipboard peer authorization was removed"));
    }
    if state.accepts(&message.update) {
        clipboard.write(message.update.text.clone()).await?;
        state.applied(&message.update);
        *latest = None;
    }
    Ok(())
}

fn observe(state: &mut State, latest: &mut Option<Arc<Update>>, text: Option<String>) {
    if let Some(update) = state.observe(text) {
        *latest = Some(Arc::new(update));
    } else if latest
        .as_ref()
        .is_some_and(|update| !state.is_current(update))
    {
        *latest = None;
    }
}

#[cfg(test)]
async fn send_update(
    peer: SocketAddr,
    config: Arc<rustls::ClientConfig>,
    authorized: Authorized,
    update: &Update,
) -> io::Result<()> {
    send_update_pinned(peer, config, authorized, update, None).await
}

async fn send_update_pinned(
    peer: SocketAddr,
    config: Arc<rustls::ClientConfig>,
    authorized: Authorized,
    update: &Update,
    fingerprint: Option<&str>,
) -> io::Result<()> {
    let stream = TcpStream::connect(peer).await?;
    let name =
        rustls::pki_types::ServerName::try_from("lan-bridge.invalid").map_err(io::Error::other)?;
    let mut stream = TlsConnector::from(config).connect(name, stream).await?;
    let (_, connection) = stream.get_ref();
    let cert = connection
        .peer_certificates()
        .and_then(|certs| certs.first())
        .ok_or_else(|| wire::invalid("missing clipboard server identity"))?;
    if connection.alpn_protocol() != Some(ALPN) || !tls::authorized(&authorized, cert.as_ref()) {
        return Err(wire::invalid(
            "unpaired clipboard server or unsupported protocol",
        ));
    }
    if fingerprint.is_some_and(|fp| crate::crypto::generate_fingerprint(cert.as_ref()) != fp) {
        return Err(wire::invalid("clipboard target identity changed"));
    }
    wire::write_update(&mut stream, update).await?;
    if stream.read_u8().await? != 1 {
        return Err(wire::invalid("clipboard peer could not apply update"));
    }
    Ok(())
}

async fn receive_update(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    authorized: Authorized,
    incoming: mpsc::Sender<Incoming>,
) -> io::Result<()> {
    let mut stream = acceptor.accept(stream).await?;
    let (_, connection) = stream.get_ref();
    let cert = connection
        .peer_certificates()
        .and_then(|certs| certs.first())
        .ok_or_else(|| wire::invalid("missing clipboard client identity"))?
        .as_ref()
        .to_vec();
    if connection.alpn_protocol() != Some(ALPN) || !tls::authorized(&authorized, &cert) {
        return Err(wire::invalid(
            "unpaired clipboard client or unsupported protocol",
        ));
    }
    let update = wire::read_update(&mut stream, node(&cert)).await?;
    let (reply, response) = oneshot::channel();
    incoming
        .send(Incoming {
            cert,
            update,
            reply,
        })
        .await
        .map_err(|_| wire::invalid("clipboard receiver stopped"))?;
    let accepted = response
        .await
        .map_err(|_| wire::invalid("clipboard receiver stopped"))?;
    stream.write_u8(u8::from(accepted)).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct FakeClipboard {
        text: Mutex<Option<String>>,
        fail_write: AtomicBool,
        writes: AtomicUsize,
        reads: AtomicUsize,
    }

    impl ClipboardStore for FakeClipboard {
        async fn read(&self) -> io::Result<Option<String>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.text.lock().unwrap().clone())
        }
        async fn write(&self, text: String) -> io::Result<()> {
            if self.fail_write.load(Ordering::Relaxed) {
                return Err(io::Error::other("clipboard occupied"));
            }
            *self.text.lock().unwrap() = Some(text);
            self.writes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn certificate() -> Certificate {
        Certificate::generate_self_signed(["ignored".to_owned()]).unwrap()
    }

    fn keys(peer: Option<&Certificate>) -> Authorized {
        let mut keys = HashMap::new();
        if let Some(peer) = peer {
            keys.insert(
                crate::crypto::certificate_fingerprint(peer),
                "test peer".into(),
            );
        }
        Arc::new(RwLock::new(keys))
    }

    async fn exchange(
        trust_client: bool,
        trust_server: bool,
        spoof_origin: bool,
    ) -> (bool, Option<Update>) {
        let a = certificate();
        let b = certificate();
        let a_keys = keys(trust_server.then_some(&b));
        let b_keys = keys(trust_client.then_some(&a));
        let client = TlsConfig::new(&a, a_keys.clone()).unwrap();
        let server = TlsConfig::new(&b, b_keys.clone()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (incoming, mut received) = mpsc::channel::<Incoming>(1);
        let reader = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            timeout(
                TRANSFER_TIMEOUT,
                receive_update(stream, TlsAcceptor::from(server.server), b_keys, incoming),
            )
            .await
        });
        let actor = tokio::spawn(async move {
            let message = received.recv().await?;
            let _ = message.reply.send(true);
            Some(message.update)
        });
        let update = Update {
            revision: Revision {
                counter: 1,
                origin: if spoof_origin {
                    [0; 32]
                } else {
                    node(a.certificate[0].as_ref())
                },
            },
            text: "中文\nEnglish 😀".into(),
        };
        let result = timeout(
            TRANSFER_TIMEOUT,
            send_update(addr, client.client, a_keys, &update),
        )
        .await;
        let _ = reader.await.unwrap();
        (matches!(result, Ok(Ok(()))), actor.await.unwrap())
    }

    #[tokio::test]
    async fn mutually_paired_tls_transfers_unicode_text() {
        let (success, received) = exchange(true, true, false).await;
        assert!(success);
        assert_eq!(received.unwrap().text, "中文\nEnglish 😀");
    }

    #[tokio::test]
    async fn unpaired_client_cannot_deliver_text() {
        let (success, received) = exchange(false, true, false).await;
        assert!(!success);
        assert!(received.is_none());
    }

    #[tokio::test]
    async fn unpaired_server_does_not_receive_text() {
        let (success, received) = exchange(true, false, false).await;
        assert!(!success);
        assert!(received.is_none());
    }

    #[tokio::test]
    async fn paired_sender_cannot_forge_update_origin() {
        let (success, received) = exchange(true, true, true).await;
        assert!(!success);
        assert!(received.is_none());
    }

    #[tokio::test]
    async fn write_failure_retries_and_revocation_blocks_queued_update() {
        let peer = certificate();
        let authorized = keys(Some(&peer));
        let clipboard = FakeClipboard::default();
        clipboard.fail_write.store(true, Ordering::Relaxed);
        let mut state = State::new([1; 32], 0);
        let mut latest = None;
        let (reply, _) = oneshot::channel();
        let message = Incoming {
            cert: peer.certificate[0].as_ref().to_vec(),
            update: Update {
                revision: Revision {
                    counter: 1,
                    origin: node(peer.certificate[0].as_ref()),
                },
                text: "retry".into(),
            },
            reply,
        };
        assert!(
            apply_incoming(&clipboard, &mut state, &mut latest, &authorized, &message)
                .await
                .is_err()
        );
        clipboard.fail_write.store(false, Ordering::Relaxed);
        apply_incoming(&clipboard, &mut state, &mut latest, &authorized, &message)
            .await
            .unwrap();
        assert!(state.observe(Some("retry".into())).is_none());
        assert!(latest.is_none());
        assert_eq!(clipboard.writes.load(Ordering::Relaxed), 1);
        authorized.write().unwrap().clear();
        assert!(
            apply_incoming(&clipboard, &mut state, &mut latest, &authorized, &message)
                .await
                .is_err()
        );
        assert_eq!(clipboard.writes.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn local_copy_since_last_poll_wins_over_older_remote_update() {
        let peer = certificate();
        let authorized = keys(Some(&peer));
        let clipboard = FakeClipboard::default();
        let mut state = State::new([1; 32], 10);
        state.observe(None);
        *clipboard.text.lock().unwrap() = Some("local copy".into());
        let mut latest = None;
        let (reply, _) = oneshot::channel();
        let message = Incoming {
            cert: peer.certificate[0].as_ref().to_vec(),
            update: Update {
                revision: Revision {
                    counter: 1,
                    origin: node(peer.certificate[0].as_ref()),
                },
                text: "older".into(),
            },
            reply,
        };
        apply_incoming(&clipboard, &mut state, &mut latest, &authorized, &message)
            .await
            .unwrap();
        assert_eq!(latest.unwrap().text, "local copy");
        assert_eq!(clipboard.writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn nontext_change_discards_pending_text() {
        let mut state = State::new([1; 32], 0);
        let mut latest = None;
        observe(&mut state, &mut latest, None);
        observe(&mut state, &mut latest, Some("pending".into()));
        assert!(latest.is_some());
        observe(&mut state, &mut latest, None);
        assert!(latest.is_none());
    }

    async fn until(mut condition: impl FnMut() -> bool) {
        timeout(Duration::from_secs(8), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn two_sessions_sync_both_directions_retry_offline_and_do_not_loop() {
        let a = certificate();
        let b = certificate();
        let a_keys = keys(Some(&b));
        let b_keys = keys(Some(&a));
        let a_config = TlsConfig::new(&a, a_keys.clone()).unwrap();
        let b_config = TlsConfig::new(&b, b_keys.clone()).unwrap();
        let a_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a_addr = a_listener.local_addr().unwrap();
        let b_addr = b_listener.local_addr().unwrap();
        drop(b_listener);
        let (a_settings, mut a_changes) = watch::channel(Settings {
            fingerprint: None,
            enabled: true,
            port: a_addr.port(),
            peers: vec![b_addr],
        });
        let (b_settings, mut b_changes) = watch::channel(Settings {
            fingerprint: None,
            enabled: true,
            port: b_addr.port(),
            peers: vec![a_addr],
        });
        let a_clipboard = Arc::new(FakeClipboard::default());
        let b_clipboard = Arc::new(FakeClipboard::default());
        let a_store = a_clipboard.clone();
        let a_node = node(a.certificate[0].as_ref());
        let a_task = tokio::spawn(async move {
            session_with_clipboard(
                a_listener,
                a_node,
                &a_config,
                a_keys,
                &mut a_changes,
                a_store.as_ref(),
            )
            .await
        });
        until(|| a_clipboard.reads.load(Ordering::Relaxed) > 0).await;
        *a_clipboard.text.lock().unwrap() = Some("offline copy 中文😀".into());
        // A first connects to an offline peer; later retries its latest text.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let b_listener = TcpListener::bind(b_addr).await.unwrap();
        let b_store = b_clipboard.clone();
        let b_node = node(b.certificate[0].as_ref());
        let b_task = tokio::spawn(async move {
            session_with_clipboard(
                b_listener,
                b_node,
                &b_config,
                b_keys,
                &mut b_changes,
                b_store.as_ref(),
            )
            .await
        });
        until(|| b_clipboard.text.lock().unwrap().as_deref() == Some("offline copy 中文😀")).await;
        *b_clipboard.text.lock().unwrap() = Some("return\ncopy".into());
        until(|| a_clipboard.text.lock().unwrap().as_deref() == Some("return\ncopy")).await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(a_clipboard.writes.load(Ordering::Relaxed), 1);
        assert_eq!(b_clipboard.writes.load(Ordering::Relaxed), 1);
        drop(a_settings);
        drop(b_settings);
        a_task.await.unwrap().unwrap();
        b_task.await.unwrap().unwrap();
    }
}
