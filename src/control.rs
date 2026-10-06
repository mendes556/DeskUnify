//! TLS control protocol v1. Unpaired certificates can only ask for approval;
//! possession of the private key is verified, and the UI approves the actual TLS identity.
use crate::{
    clipboard::tls::{Authorized, TlsConfig},
    crypto,
};
use lan_mouse_ipc::{ClientHandle, PairRequest, Position, Sharing};
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use webrtc_dtls::crypto::Certificate;
const ALPN: &[u8] = b"deskunify-control/1";
const VERSION: u32 = 1;
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Peer {
    pub id: ClientHandle,
    pub fingerprint: String,
    pub addr: SocketAddr,
    pub alternates: Vec<SocketAddr>,
    pub position: Position,
    pub sharing: Sharing,
    pub note: Option<String>,
}
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Settings {
    pub port: u16,
    pub peers: Vec<Peer>,
}
#[derive(Serialize, Deserialize)]
struct Request {
    version: u32,
    port: u16,
    position: Position,
    sharing: Sharing,
    #[serde(default)]
    note: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Reply {
    pub version: u32,
    pub sharing: Sharing,
    pub error: Option<String>,
    pub note: Option<String>,
    #[serde(skip)]
    pub verified_addr: Option<SocketAddr>,
}
impl Reply {
    pub fn accepted(sharing: Sharing) -> Self {
        Self {
            version: VERSION,
            sharing,
            error: None,
            note: None,
            verified_addr: None,
        }
    }
    pub fn rejected(error: &str) -> Self {
        Self {
            version: VERSION,
            sharing: Sharing::OFF,
            error: Some(error.into()),
            note: None,
            verified_addr: None,
        }
    }
}
pub(crate) enum Event {
    Request {
        request: PairRequest,
        sharing: Sharing,
        note: Option<String>,
        reply: oneshot::Sender<Reply>,
    },
    Updated {
        id: ClientHandle,
        fingerprint: String,
        result: Result<Reply, String>,
    },
    Error(String),
}
pub(crate) struct Control {
    settings: watch::Sender<Settings>,
    events: mpsc::Receiver<Event>,
    task: JoinHandle<()>,
}
impl Control {
    pub fn new(cert: Certificate, keys: Authorized) -> Self {
        let (settings, rx) = watch::channel(Settings::default());
        let (tx, events) = mpsc::channel(32);
        let task = tokio::task::spawn_local(run(cert, keys, rx, tx));
        Self {
            settings,
            events,
            task,
        }
    }
    pub fn configure(&self, value: Settings) {
        self.settings.send_if_modified(|old| {
            if *old == value {
                false
            } else {
                *old = value;
                true
            }
        });
    }
    pub async fn event(&mut self) -> Event {
        self.events
            .recv()
            .await
            .unwrap_or_else(|| Event::Error("配对服务已停止".into()))
    }
    pub async fn terminate(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}
async fn frame_write<S: AsyncWrite + Unpin, T: Serialize>(s: &mut S, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > 4096 {
        return Err(io::Error::other("control message too large"));
    }
    s.write_u32(bytes.len() as u32).await?;
    s.write_all(&bytes).await?;
    s.flush().await
}
async fn frame_read<S: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    s: &mut S,
) -> io::Result<T> {
    let n = s.read_u32().await?;
    if n == 0 || n > 4096 {
        return Err(io::Error::other("invalid control message length"));
    }
    let mut bytes = vec![0; n as usize];
    s.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
async fn accept(
    socket: TcpStream,
    ip: IpAddr,
    tls: TlsConfig,
    tx: mpsc::Sender<Event>,
) -> io::Result<()> {
    let mut stream = tokio::time::timeout(
        Duration::from_secs(5),
        TlsAcceptor::from(tls.server).accept(socket),
    )
    .await
    .map_err(io::Error::other)??;
    let conn = stream.get_ref().1;
    if conn.alpn_protocol() != Some(ALPN) {
        return Err(io::Error::other("unsupported control protocol"));
    }
    let fingerprint = conn
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| crypto::generate_fingerprint(c.as_ref()))
        .ok_or_else(|| io::Error::other("missing peer certificate"))?;
    let request: Request = tokio::time::timeout(Duration::from_secs(5), frame_read(&mut stream))
        .await
        .map_err(io::Error::other)??;
    if request.version != VERSION || request.port == 0 || request.port > 65533 {
        return Err(io::Error::other("unsupported version or port"));
    }
    let (reply, response) = oneshot::channel();
    tx.send(Event::Request {
        request: PairRequest {
            fingerprint,
            ip,
            port: request.port,
            position: request.position,
            name: ip.to_string(),
        },
        sharing: request.sharing,
        note: request.note,
        reply,
    })
    .await
    .map_err(io::Error::other)?;
    let response = tokio::time::timeout(Duration::from_secs(60), response)
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    frame_write(&mut stream, &response).await
}
async fn exchange(tls: TlsConfig, peer: Peer, port: u16) -> io::Result<Reply> {
    let control_port = peer
        .addr
        .port()
        .checked_add(2)
        .ok_or_else(|| io::Error::other("control port overflow"))?;
    let mut connected = None;
    let mut last_error = io::Error::other("no reachable control address");
    for addr in std::iter::once(peer.addr).chain(peer.alternates.iter().copied()) {
        match tokio::time::timeout(
            Duration::from_secs(2),
            TcpStream::connect(SocketAddr::new(addr.ip(), control_port)),
        )
        .await
        {
            Ok(Ok(socket)) => {
                connected = Some(socket);
                break;
            }
            Ok(Err(error)) => last_error = error,
            Err(error) => last_error = io::Error::other(error),
        }
    }
    let socket = connected.ok_or(last_error)?;
    let verified_addr = SocketAddr::new(socket.peer_addr()?.ip(), peer.addr.port());
    socket.set_nodelay(true)?;
    let mut stream = TlsConnector::from(tls.client)
        .connect(
            ServerName::try_from("deskunify.local").unwrap().to_owned(),
            socket,
        )
        .await?;
    let conn = stream.get_ref().1;
    let fingerprint = conn
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| crypto::generate_fingerprint(c.as_ref()));
    if conn.alpn_protocol() != Some(ALPN) || fingerprint.as_deref() != Some(&peer.fingerprint) {
        return Err(io::Error::other("peer identity mismatch"));
    }
    frame_write(
        &mut stream,
        &Request {
            version: VERSION,
            port,
            position: peer.position,
            sharing: peer.sharing,
            note: peer.note,
        },
    )
    .await?;
    let mut response: Reply = frame_read(&mut stream).await?;
    if response.version != VERSION {
        return Err(io::Error::other("unsupported control version"));
    }
    if let Some(error) = &response.error {
        return Err(io::Error::other(error.clone()));
    }
    response.verified_addr = Some(verified_addr);
    Ok(response)
}
async fn run(
    cert: Certificate,
    keys: Authorized,
    mut settings: watch::Receiver<Settings>,
    tx: mpsc::Sender<Event>,
) {
    let tls = match TlsConfig::control(&cert, keys, ALPN) {
        Ok(tls) => tls,
        Err(e) => {
            let _ = tx.send(Event::Error(e.to_string())).await;
            return;
        }
    };
    let mut tasks = JoinSet::new();
    let mut polls = JoinSet::new();
    let mut busy = std::collections::HashSet::new();
    let mut listener = None;
    let mut port = 0;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let current = settings.borrow().clone();
        if current.port != port || listener.is_none() {
            port = current.port;
            listener = if let Some(p) = port.checked_add(2).filter(|_| port != 0) {
                match TcpListener::bind(("0.0.0.0", p)).await {
                    Ok(l) => Some(l),
                    Err(e) => {
                        let _ = tx.send(Event::Error(format!("配对端口 {p}: {e}"))).await;
                        None
                    }
                }
            } else {
                None
            };
        }
        tokio::select! {
            changed = settings.changed() => { if changed.is_err() { break; } },
            _ = tick.tick() => {
                for peer in current.peers {
                    if busy.insert(peer.id) {
                        let tls = tls.clone(); let port = current.port;
                        polls.spawn(async move { let id=peer.id; let fingerprint=peer.fingerprint.clone();
                            let result = tokio::time::timeout(Duration::from_secs(65),exchange(tls,peer,port)).await.map_err(|e|e.to_string()).and_then(|r|r.map_err(|e|e.to_string()));
                            (id,fingerprint,result) });
                    }
                }
            },
            connection = async { match &listener { Some(l) => l.accept().await, None => std::future::pending().await } } => {
                if let Ok((socket, addr)) = connection { if tasks.len() < 16 { let tls=tls.clone(); let tx=tx.clone(); tasks.spawn(accept(socket,addr.ip(),tls,tx)); } }
            },
            Some(_) = tasks.join_next() => {},
            Some(result) = polls.join_next() => { if let Ok((id,fingerprint,result)) = result { busy.remove(&id); let _=tx.send(Event::Updated{id,fingerprint,result}).await; } },
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bounded_frames_reject_untrusted_lengths() {
        let (mut a, mut b) = tokio::io::duplex(32);
        a.write_u32(4097).await.unwrap();
        assert!(frame_read::<_, Request>(&mut b).await.is_err());
    }
    #[tokio::test]
    async fn unknown_certificate_requires_explicit_control_approval_and_server_pin() {
        use std::{
            collections::HashMap,
            sync::{Arc, RwLock},
        };
        let a = Certificate::generate_self_signed(["A".into()]).unwrap();
        let b = Certificate::generate_self_signed(["B".into()]).unwrap();
        let afp = crypto::certificate_fingerprint(&a);
        let bfp = crypto::certificate_fingerprint(&b);
        let client = TlsConfig::control(
            &a,
            Arc::new(RwLock::new(HashMap::from([(bfp.clone(), "B".into())]))),
            ALPN,
        )
        .unwrap();
        let server = TlsConfig::control(&b, Authorized::default(), ALPN).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let receiver = tokio::spawn(async move {
            let (socket, peer) = listener.accept().await.unwrap();
            accept(socket, peer.ip(), server, tx).await
        });
        let peer = Peer {
            id: 0,
            fingerprint: bfp.clone(),
            addr: SocketAddr::new("127.0.0.2".parse().unwrap(), addr.port() - 2),
            alternates: vec![SocketAddr::new(addr.ip(), addr.port() - 2)],
            position: Position::Right,
            sharing: Sharing::OFF,
            note: None,
        };
        let sender = tokio::spawn(exchange(client.clone(), peer.clone(), 4242));
        match rx.recv().await.unwrap() {
            Event::Request {
                request,
                sharing,
                reply,
                ..
            } => {
                assert_eq!(request.fingerprint, afp);
                assert_eq!(request.port, 4242);
                assert_eq!(request.position, Position::Right);
                assert_eq!(sharing, Sharing::OFF);
                reply.send(Reply::rejected("denied")).unwrap();
            }
            _ => panic!("unexpected event"),
        }
        assert!(
            sender
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("denied")
        );
        receiver.await.unwrap().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = TlsConfig::control(&b, Authorized::default(), ALPN).unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let receiver = tokio::spawn(async move {
            let (socket, peer) = listener.accept().await.unwrap();
            accept(socket, peer.ip(), server, tx).await
        });
        let mut wrong = peer;
        wrong.addr = SocketAddr::new(addr.ip(), addr.port() - 2);
        wrong.fingerprint = afp;
        assert!(
            exchange(client, wrong, 4242)
                .await
                .unwrap_err()
                .to_string()
                .contains("identity mismatch")
        );
        assert!(rx.try_recv().is_err());
        receiver.abort();
        let _ = receiver.await;
    }
}
