// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
//! Opt-in, mutually authenticated file transfer, separate from input capture.
mod disk;
mod manifest;
mod native;
#[cfg(test)]
mod tests;
mod watch;
mod wire;

use crate::{
    clipboard::tls::{self, Authorized, TlsConfig},
    config::Config,
    crypto,
};
use clap::{Args, Subcommand, ValueEnum};
use manifest::{Manifest, Source, invalid};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

const ALPN: &[u8] = b"lan-bridge-files/2";
const MAX_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

pub(crate) fn has_file_clipboard() -> io::Result<bool> {
    native::has_files()
}

#[derive(Debug, Error)]
pub enum FileError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Certificate(#[from] crypto::Error),
    #[error(transparent)]
    Tls(#[from] rustls::Error),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct FileArgs {
    /// Print newline-delimited JSON progress and results
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: FileCommand,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
struct Target {
    /// Destination IP:port (file port defaults to 4243 on receiver)
    #[arg(long)]
    to: SocketAddr,
    /// Additionally pin the destination to this authorized fingerprint
    #[arg(long)]
    fingerprint: Option<String>,
    /// Auto reduces bulk traffic when probe latency rises; full does not pace
    #[arg(long, value_enum, default_value = "auto")]
    mode: Mode,
    /// Optional hard upload limit, in MiB/s
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=10000))]
    limit_mib: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Mode {
    Auto,
    Full,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
enum FileCommand {
    /// Print/create this computer's certificate identity, without starting input
    Identity,
    /// Explicitly allow paired devices to write verified files into this directory
    Receive {
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "0.0.0.0:4243")]
        listen: SocketAddr,
        /// Stop after one successful file transfer (failed attempts keep listening)
        #[arg(long)]
        once: bool,
        #[arg(long, default_value_t = MAX_BYTES)]
        max_bytes: u64,
        /// Allow memory-only network benchmarks from paired devices
        #[arg(long)]
        allow_benchmark: bool,
        /// Put received local files on the clipboard for Finder/Explorer paste
        #[arg(long)]
        clipboard: bool,
    },
    /// Stream files/directories; rerun the same command to resume interrupted work
    Send {
        #[command(flatten)]
        target: Target,
        /// Send files copied in Finder/Explorer instead of explicit paths
        #[arg(long, conflicts_with = "paths")]
        clipboard: bool,
        #[arg(required_unless_present = "clipboard", num_args = 1..)]
        paths: Vec<PathBuf>,
    },
    /// Automatically send newly copied files to one peer and receive its files
    Sync {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "0.0.0.0:4243")]
        listen: SocketAddr,
        #[arg(long, default_value_t = MAX_BYTES)]
        max_bytes: u64,
    },
    /// Measure encrypted network throughput; no files are created on the receiver
    Benchmark {
        #[command(flatten)]
        target: Target,
        #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u64).range(1..=4096))]
        size_mib: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Request {
    Files { manifest: Manifest },
    Benchmark { bytes: u64 },
    Probe,
}

#[derive(Clone, Serialize, Deserialize)]
struct Resume {
    offset: u64,
    hash: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Ready {
    pending: bool,
    offsets: Vec<Resume>,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Report {
    event: String,
    bytes: u64,
    network_bytes: u64,
    resumed_bytes: u64,
    files: usize,
    seconds: f64,
    mib_per_second: f64,
    output: Option<PathBuf>,
    verified: bool,
}

impl Report {
    fn new(
        bytes: u64,
        resumed: u64,
        files: usize,
        start: Instant,
        output: Option<PathBuf>,
    ) -> Self {
        let seconds = start.elapsed().as_secs_f64().max(0.000001);
        Self {
            event: "completed".into(),
            bytes,
            network_bytes: bytes - resumed,
            resumed_bytes: resumed,
            files,
            seconds,
            mib_per_second: (bytes - resumed) as f64 / 1048576.0 / seconds,
            output,
            verified: true,
        }
    }
}

fn emit(json: bool, value: &impl Serialize, text: &str) {
    if json {
        match serde_json::to_string(value) {
            Ok(value) => println!("{value}"),
            Err(error) => log::warn!("file progress: {error}"),
        }
    } else {
        eprintln!("{text}");
    }
}

fn complete(json: bool, report: &Report) {
    emit(
        json,
        report,
        &format!(
            "已完成并校验：{} 个文件，{:.2} MiB，新增传输 {:.2} MiB/s，续传 {:.2} MiB{}",
            report.files,
            report.bytes as f64 / 1048576.0,
            report.mib_per_second,
            report.resumed_bytes as f64 / 1048576.0,
            report
                .output
                .as_ref()
                .map(|p| format!("\n保存至：{}", p.display()))
                .unwrap_or_default()
        ),
    );
}

struct Progress {
    start: Instant,
    last: Instant,
    bytes: u64,
    total: u64,
    json: bool,
}
impl Progress {
    fn new(total: u64, json: bool) -> Self {
        Self {
            start: Instant::now(),
            last: Instant::now(),
            bytes: 0,
            total,
            json,
        }
    }
    fn advance(&mut self, bytes: u64) {
        self.bytes += bytes;
        if self.last.elapsed() >= Duration::from_millis(500) {
            let rate = self.bytes as f64 / 1048576.0 / self.start.elapsed().as_secs_f64();
            emit(
                self.json,
                &serde_json::json!({"event":"progress", "network_bytes":self.bytes,
                "network_total":self.total, "mib_per_second":rate}),
                &format!(
                    "传输 {:.1}/{:.1} MiB · {:.1} MiB/s",
                    self.bytes as f64 / 1048576.0,
                    self.total as f64 / 1048576.0,
                    rate
                ),
            );
            self.last = Instant::now();
        }
    }
}

pub async fn run(mut config: Config, args: FileArgs) -> Result<(), FileError> {
    let cert = crypto::load_or_generate_key_and_cert(config.cert_path())?;
    if matches!(args.command, FileCommand::Identity) {
        let fingerprint = crypto::certificate_fingerprint(&cert);
        emit(
            args.json,
            &serde_json::json!({"fingerprint":fingerprint}),
            &fingerprint,
        );
        return Ok(());
    }
    let keys = Arc::new(RwLock::new(config.authorized_fingerprints()));
    let tls = TlsConfig::with_alpn(&cert, keys.clone(), ALPN)?;
    let operation = async {
        match args.command {
            FileCommand::Identity => unreachable!(),
            FileCommand::Receive {
                output,
                listen,
                once,
                max_bytes,
                allow_benchmark,
                clipboard,
            } => {
                receive_service(
                    &mut config,
                    tls,
                    keys,
                    output,
                    listen,
                    ReceiveOptions {
                        once,
                        max_bytes,
                        allow_benchmark,
                        json: args.json,
                        clipboard,
                    },
                )
                .await
            }
            FileCommand::Send {
                target,
                paths,
                clipboard,
            } => {
                let paths = if clipboard {
                    tokio::task::spawn_blocking(native::read_paths)
                        .await
                        .map_err(io::Error::other)??
                } else {
                    paths
                };
                let source = tokio::task::spawn_blocking(move || manifest::collect(paths))
                    .await
                    .map_err(io::Error::other)??;
                let mut stream = connect(&target, tls.client.clone(), &keys).await?;
                let pace = Pacer::new(&target, tls.client, keys).await;
                let report = send_files(&mut stream, source, pace, args.json).await?;
                complete(args.json, &report);
                Ok(())
            }
            FileCommand::Sync {
                target,
                output,
                listen,
                max_bytes,
            } => {
                let watcher = watch::run(target, tls.clone(), keys.clone(), args.json);
                let receiver = receive_service(
                    &mut config,
                    tls,
                    keys,
                    output,
                    listen,
                    ReceiveOptions {
                        once: false,
                        max_bytes,
                        allow_benchmark: false,
                        json: args.json,
                        clipboard: true,
                    },
                );
                tokio::select! {
                    result = watcher => result,
                    result = receiver => result,
                }
            }
            FileCommand::Benchmark { target, size_mib } => {
                let mut stream = connect(&target, tls.client.clone(), &keys).await?;
                let pace = Pacer::new(&target, tls.client, keys).await;
                let report =
                    send_benchmark(&mut stream, size_mib * 1048576, pace, args.json).await?;
                complete(args.json, &report);
                Ok(())
            }
        }
    };
    tokio::select! {
        result = operation => result.map_err(FileError::Io),
        result = tokio::signal::ctrl_c() => {
            result?;
            Err(io::Error::new(io::ErrorKind::Interrupted, "传输已取消；重新发送同一批文件可续传").into())
        }
    }
}

async fn connect(
    target: &Target,
    config: Arc<rustls::ClientConfig>,
    keys: &Authorized,
) -> io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    timeout(Duration::from_secs(10), async {
        let socket = TcpStream::connect(target.to).await?;
        socket.set_nodelay(true)?;
        let name = rustls::pki_types::ServerName::try_from("lan-bridge.invalid")
            .map_err(io::Error::other)?;
        let stream = TlsConnector::from(config).connect(name, socket).await?;
        let connection = stream.get_ref().1;
        let cert = connection
            .peer_certificates()
            .and_then(|c| c.first())
            .ok_or_else(|| invalid("缺少文件接收端身份"))?;
        let fingerprint = crypto::generate_fingerprint(cert);
        if connection.alpn_protocol() != Some(ALPN)
            || !tls::authorized(keys, cert)
            || target
                .fingerprint
                .as_ref()
                .is_some_and(|pin| pin.to_lowercase() != fingerprint)
        {
            return Err(invalid("文件接收端指纹不符或未授权"));
        }
        log::debug!("file peer {} · {fingerprint}", target.to);
        Ok(stream)
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "文件接收端连接超时；请先开启 files receive",
        )
    })?
}

struct Pacer {
    rate: Arc<AtomicU64>,
    task: Option<JoinHandle<()>>,
    next: Instant,
}
impl Drop for Pacer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Pacer {
    async fn new(target: &Target, config: Arc<rustls::ClientConfig>, keys: Authorized) -> Self {
        let cap = target
            .limit_mib
            .map(|n| n * 1048576)
            .unwrap_or(10_000 * 1048576);
        let rate = Arc::new(AtomicU64::new(
            if target.mode == Mode::Full && target.limit_mib.is_none() {
                0
            } else if target.mode == Mode::Auto {
                cap.min(64 * 1048576)
            } else {
                cap
            },
        ));
        let task = if target.mode == Mode::Auto {
            let target = target.clone();
            let rate = rate.clone();
            Some(tokio::spawn(async move {
                // Probe traffic has its own connection and bypasses the disk queue.
                let Ok(mut probe) = connect(&target, config, &keys).await else {
                    return;
                };
                if wire::write_json(&mut probe, &Request::Probe).await.is_err() {
                    return;
                }
                let mut baseline = Duration::from_secs(60);
                for _ in 0..3 {
                    let start = Instant::now();
                    if ping(&mut probe).await.is_err() {
                        return;
                    }
                    baseline = baseline.min(start.elapsed());
                }
                loop {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    let start = Instant::now();
                    if ping(&mut probe).await.is_err() {
                        return;
                    }
                    let current = rate.load(Ordering::Relaxed);
                    let congested =
                        start.elapsed() > (baseline * 3).max(baseline + Duration::from_millis(10));
                    let next = if congested {
                        current * 4 / 5
                    } else {
                        current + current / 10
                    };
                    rate.store(next.clamp(1048576, cap), Ordering::Relaxed);
                }
            }))
        } else {
            None
        };
        Self {
            rate,
            task,
            next: Instant::now(),
        }
    }
    async fn wait(&mut self, bytes: usize) {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        let now = Instant::now();
        self.next = self.next.max(now) + Duration::from_secs_f64(bytes as f64 / rate as f64);
        // Accumulate tiny files into a short burst instead of a timer per file.
        let burst = Duration::from_millis(20);
        if self.next > now + burst {
            tokio::time::sleep_until((self.next - burst).into()).await;
        }
    }
}

async fn ping<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> io::Result<()> {
    wire::write_all(stream, &[1]).await?;
    stream.flush().await?;
    let mut reply = [0];
    wire::read_exact(stream, &mut reply).await?;
    if reply != [1] {
        return Err(invalid("测速探测响应非法"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ReceiveOptions {
    once: bool,
    max_bytes: u64,
    allow_benchmark: bool,
    json: bool,
    clipboard: bool,
}

async fn receive_service(
    config: &mut Config,
    tls: TlsConfig,
    keys: Authorized,
    output: PathBuf,
    listen: SocketAddr,
    options: ReceiveOptions,
) -> io::Result<()> {
    tokio::fs::create_dir_all(&output).await?;
    let output = tokio::fs::canonicalize(output).await?;
    let listener = TcpListener::bind(listen).await?;
    emit(
        options.json,
        &serde_json::json!({"event":"listening","address":listener.local_addr()?,"output":output}),
        &format!(
            "文件接收已开启：{} → {} · 仅接受已配对设备 · Ctrl+C 停止",
            listener.local_addr()?,
            output.display()
        ),
    );
    let mut connections = JoinSet::new();
    let disk = Arc::new(Mutex::new(()));
    loop {
        tokio::select! {
            connection = listener.accept() => {
                let (stream, peer) = connection?;
                if connections.len() >= 8 { continue; }
                let acceptor = TlsAcceptor::from(tls.server.clone());
                let keys = keys.clone();
                let output = output.clone();
                let disk = disk.clone();
                connections.spawn(async move {
                    let result = receive_connection(stream, acceptor, keys, output, disk, options).await;
                    if let Err(error) = &result { log::warn!("file connection {peer}: {error}"); }
                    result
                });
            },
            Some(result) = connections.join_next() => {
                if let Ok(Ok(Some(report))) = result {
                    complete(options.json, &report);
                    if options.once && report.output.is_some() { return Ok(()); }
                }
            },
            changed = config.changed() => {
                changed.map_err(io::Error::other)?;
                if config.read_from_disk()? {
                    *keys.write().map_err(|_| invalid("授权锁不可用"))? = config.authorized_fingerprints();
                }
            }
        }
    }
}

async fn receive_connection(
    socket: TcpStream,
    acceptor: TlsAcceptor,
    keys: Authorized,
    output: PathBuf,
    disk: Arc<Mutex<()>>,
    options: ReceiveOptions,
) -> io::Result<Option<Report>> {
    socket.set_nodelay(true)?;
    let mut stream = timeout(Duration::from_secs(10), acceptor.accept(socket))
        .await
        .map_err(|_| invalid("文件 TLS 握手超时"))??;
    let connection = stream.get_ref().1;
    let cert = connection
        .peer_certificates()
        .and_then(|c| c.first())
        .ok_or_else(|| invalid("缺少文件发送端身份"))?
        .as_ref()
        .to_vec();
    if connection.alpn_protocol() != Some(ALPN) || !tls::authorized(&keys, &cert) {
        return Err(invalid("文件发送端未授权或协议不兼容"));
    }
    let request: Request = wire::read_json(&mut stream).await?;
    match request {
        Request::Probe => loop {
            let mut byte = [0];
            wire::read_exact(&mut stream, &mut byte).await?;
            check_authorized(&keys, &cert)?;
            if byte != [1] {
                return Err(invalid("探测请求非法"));
            }
            wire::write_all(&mut stream, &[1]).await?;
            stream.flush().await?;
        },
        Request::Files { manifest } => {
            let _guard = timeout(wire::IDLE, disk.lock())
                .await
                .map_err(|_| invalid("接收任务忙，请稍后重试"))?;
            let start = Instant::now();
            let clipboard_revision = if options.clipboard {
                tokio::task::spawn_blocking(native::read_snapshot)
                    .await
                    .map_err(io::Error::other)?
                    .map(|snapshot| snapshot.revision)
                    .ok()
            } else {
                None
            };
            let peer = crypto::generate_fingerprint(&cert);
            let prepared = prepare(&output, &manifest, &peer, options.max_bytes);
            tokio::pin!(prepared);
            let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
            let prepared = loop {
                tokio::select! {
                    result = &mut prepared => break result,
                    _ = heartbeat.tick() => wire::write_json(&mut stream, &Ready { pending: true, offsets: vec![], error: None }).await?,
                }
            };
            let mut destination = match prepared {
                Ok(destination) => destination,
                Err(error) => {
                    wire::write_json(
                        &mut stream,
                        &Ready {
                            pending: false,
                            offsets: vec![],
                            error: Some(error.to_string()),
                        },
                    )
                    .await?;
                    return Err(error);
                }
            };
            wire::write_json(
                &mut stream,
                &Ready {
                    pending: false,
                    offsets: destination.offsets.clone(),
                    error: None,
                },
            )
            .await?;
            let report = receive_files(
                &mut stream,
                &manifest,
                &mut destination,
                &keys,
                &cert,
                start,
                options.json,
            )
            .await?;
            if options.clipboard {
                let output = report
                    .output
                    .clone()
                    .ok_or_else(|| invalid("没有已接收文件"))?;
                let paths = manifest
                    .entries
                    .iter()
                    .filter(|e| !e.path.contains('/'))
                    .map(|e| output.join(&e.path))
                    .collect();
                // Files are already committed. Clipboard failure must not cause
                // the sender to believe its verified transfer was lost.
                let result = if let Some(revision) = clipboard_revision {
                    tokio::task::spawn_blocking(move || native::write_if_unchanged(paths, revision))
                        .await
                        .map_err(io::Error::other)?
                } else {
                    Err(invalid("接收开始时无法读取剪贴板；请从保存目录复制文件"))
                };
                match result {
                    Ok(true) => emit(
                        options.json,
                        &serde_json::json!({"event":"clipboard_ready"}),
                        "文件已放入剪贴板，可以粘贴",
                    ),
                    Ok(false) => emit(
                        options.json,
                        &serde_json::json!({"event":"clipboard_skipped"}),
                        "文件已保存；保留传输期间新复制的剪贴板内容",
                    ),
                    Err(error) => emit(
                        options.json,
                        &serde_json::json!({"event":"clipboard_error","error":error.to_string()}),
                        &format!("文件已保存，写入文件剪贴板失败：{error}"),
                    ),
                }
            }
            wire::write_json(&mut stream, &report).await?;
            Ok(Some(report))
        }
        Request::Benchmark { bytes } => {
            if !options.allow_benchmark || bytes > 4096 * 1048576 || bytes == 0 {
                wire::write_json(
                    &mut stream,
                    &Ready {
                        pending: false,
                        offsets: vec![],
                        error: Some("接收端未开启测速或测速大小非法".into()),
                    },
                )
                .await?;
                return Err(invalid("测速被拒绝"));
            }
            let _guard = timeout(wire::IDLE, disk.lock())
                .await
                .map_err(|_| invalid("接收任务忙"))?;
            wire::write_json(
                &mut stream,
                &Ready {
                    pending: false,
                    offsets: vec![],
                    error: None,
                },
            )
            .await?;
            let start = Instant::now();
            let mut left = bytes;
            let mut buffer = vec![0; wire::CHUNK];
            let mut hash = Sha256::new();
            while left > 0 {
                check_authorized(&keys, &cert)?;
                let length = left.min(wire::CHUNK as u64) as usize;
                wire::read_exact(&mut stream, &mut buffer[..length]).await?;
                hash.update(&buffer[..length]);
                left -= length as u64;
            }
            verify_digest(&mut stream, hash).await?;
            let report = Report::new(bytes, 0, 0, start, None);
            wire::write_json(&mut stream, &report).await?;
            Ok(Some(report))
        }
    }
}

fn check_authorized(keys: &Authorized, cert: &[u8]) -> io::Result<()> {
    if !tls::authorized(keys, cert) {
        return Err(invalid("发送端授权已撤销"));
    }
    Ok(())
}

struct Destination {
    stage: PathBuf,
    final_path: PathBuf,
    paths: Vec<PathBuf>,
    offsets: Vec<Resume>,
    committed: bool,
    fresh: Vec<bool>,
    hashes: Vec<Sha256>,
}

async fn hash_prefix(file: &mut File, length: u64) -> io::Result<Sha256> {
    // Callers pass newly opened files: an empty prefix needs no seek/buffer.
    if length == 0 {
        return Ok(Sha256::new());
    }
    file.seek(io::SeekFrom::Start(0)).await?;
    let mut left = length;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; wire::CHUNK];
    while left > 0 {
        let size = left.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..size]).await?;
        hash.update(&buffer[..size]);
        left -= size as u64;
    }
    Ok(hash)
}

async fn prepare(
    output: &Path,
    manifest: &Manifest,
    peer: &str,
    max_bytes: u64,
) -> io::Result<Destination> {
    manifest.validate(max_bytes.min(MAX_BYTES))?;
    let id = manifest.id(peer)?;
    let final_path = output.join(format!("transfer-{}", &id[..24]));
    let stage = output.join(format!(".lan-bridge-{id}.partial"));
    let committed = tokio::fs::try_exists(&final_path).await?;
    let base = if committed {
        final_path.clone()
    } else {
        let output = output.to_owned();
        let name = format!(".lan-bridge-{id}.partial");
        tokio::task::spawn_blocking(move || manifest::directory(&output, &name))
            .await
            .map_err(io::Error::other)??
    };
    let metadata = tokio::fs::symlink_metadata(&base).await?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("接收目录被替换"));
    }
    let mut destination = Destination {
        stage,
        final_path,
        paths: vec![],
        offsets: vec![],
        committed,
        fresh: vec![],
        hashes: vec![],
    };
    let mut prepared_dirs = HashSet::from([String::new()]);
    for entry in &manifest.entries {
        let path = base.join(&entry.path);
        let dir = if entry.size.is_none() {
            entry.path.as_str()
        } else {
            entry.path.rsplit_once('/').map(|p| p.0).unwrap_or("")
        };
        if prepared_dirs.insert(dir.to_owned()) {
            let root = base.clone();
            let dir = dir.to_owned();
            if committed {
                let metadata = tokio::fs::symlink_metadata(root.join(&dir)).await?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(invalid("已完成目录被替换"));
                }
            } else {
                tokio::task::spawn_blocking(move || manifest::directory(&root, &dir))
                    .await
                    .map_err(io::Error::other)??;
            }
        }
        let mut resume = Resume {
            offset: 0,
            hash: Sha256::digest([]).into(),
        };
        let mut fresh = false;
        let mut hash = Sha256::new();
        if let Some(size) = entry.size {
            match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                    return Err(invalid("接收文件被替换为链接或特殊文件"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound && !committed => {
                    fresh = true;
                }
                Err(error) => return Err(error),
            }
            if !fresh {
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(!committed)
                    .open(&path)
                    .await?;
                let length = file.metadata().await?.len();
                if length > size || (committed && length != size) {
                    return Err(invalid("续传文件大小与清单不符"));
                }
                // Partial writes after cancellation are discarded at a chunk boundary.
                resume.offset = if length == size {
                    size
                } else {
                    length / wire::CHUNK as u64 * wire::CHUNK as u64
                };
                if !committed {
                    file.set_len(resume.offset).await?;
                }
                hash = hash_prefix(&mut file, resume.offset).await?;
                resume.hash = hash.clone().finalize().into();
            }
        }
        destination.paths.push(path);
        destination.offsets.push(resume);
        destination.fresh.push(fresh);
        destination.hashes.push(hash);
    }
    Ok(destination)
}

async fn verify_digest<S: AsyncRead + Unpin>(stream: &mut S, hash: Sha256) -> io::Result<()> {
    let mut expected = [0; 32];
    wire::read_exact(stream, &mut expected).await?;
    let actual: [u8; 32] = hash.finalize().into();
    if expected != actual {
        return Err(invalid("文件 SHA-256 校验失败；未提交接收文件"));
    }
    Ok(())
}

async fn receive_files<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    manifest: &Manifest,
    destination: &mut Destination,
    keys: &Authorized,
    cert: &[u8],
    start: Instant,
    json: bool,
) -> io::Result<Report> {
    let bytes = manifest.validate(MAX_BYTES)?;
    let resumed = destination.offsets.iter().map(|r| r.offset).sum();
    let mut progress = Progress::new(bytes - resumed, json);
    let mut buffer = vec![0; wire::CHUNK];
    let mut files = 0;
    let mut flushes = JoinSet::new();
    for (index, ((entry, path), resume)) in manifest
        .entries
        .iter()
        .zip(&destination.paths)
        .zip(&destination.offsets)
        .enumerate()
    {
        let Some(size) = entry.size else {
            continue;
        };
        check_authorized(keys, cert)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(!destination.committed)
            .create_new(destination.fresh[index])
            .open(path)
            .await?;
        file.seek(io::SeekFrom::Start(resume.offset)).await?;
        let mut hash = destination.hashes[index].clone();
        let mut offset = resume.offset;
        while offset < size {
            check_authorized(keys, cert)?;
            let length = wire::packet_length(stream).await?;
            if length == 0 {
                continue;
            }
            if length > wire::CHUNK || length as u64 > size - offset {
                return Err(invalid("文件数据块长度超出范围"));
            }
            wire::read_exact(stream, &mut buffer[..length]).await?;
            if let Err(error) = file.write_all(&buffer[..length]).await {
                file.set_len(offset).await?;
                return Err(error);
            }
            hash.update(&buffer[..length]);
            offset += length as u64;
            progress.advance(length as u64);
        }
        let digest_length = loop {
            let length = wire::packet_length(stream).await?;
            if length != 0 {
                break length;
            }
        };
        if digest_length != 32 {
            return Err(invalid("文件校验帧长度非法"));
        }
        if let Err(error) = verify_digest(stream, hash).await {
            if !destination.committed {
                file.set_len(0).await?;
            }
            return Err(error);
        }
        if !destination.committed {
            let modified = entry.modified_ns;
            let executable = entry.executable;
            flushes.spawn(async move {
                let file = file.into_std().await;
                tokio::task::spawn_blocking(move || {
                    file.set_times(
                        std::fs::FileTimes::new()
                            .set_modified(std::time::UNIX_EPOCH + Duration::from_nanos(modified)),
                    )?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(std::fs::Permissions::from_mode(if executable {
                            0o700
                        } else {
                            0o600
                        }))?;
                    }
                    #[cfg(not(unix))]
                    let _ = executable;
                    disk::sync_file(&file)
                })
                .await
                .map_err(io::Error::other)?
            });
            if flushes.len() >= 8 {
                flushes
                    .join_next()
                    .await
                    .ok_or_else(|| invalid("文件落盘任务丢失"))?
                    .map_err(io::Error::other)??;
            }
        }
        files += 1;
    }
    while let Some(result) = flushes.join_next().await {
        result.map_err(io::Error::other)??;
    }
    check_authorized(keys, cert)?;
    if !destination.committed {
        let directories = manifest
            .entries
            .iter()
            .filter(|e| e.size.is_none())
            .map(|e| destination.stage.join(&e.path))
            .collect::<Vec<_>>();
        let stage = destination.stage.clone();
        let output = destination
            .final_path
            .parent()
            .ok_or_else(|| invalid("缺少接收父目录"))?
            .to_owned();
        tokio::task::spawn_blocking(move || disk::finish_batch(&stage, &directories, &output))
            .await
            .map_err(io::Error::other)??;
        if tokio::fs::try_exists(&destination.final_path).await? {
            return Err(invalid("目标已存在，未覆盖"));
        }
        tokio::fs::rename(&destination.stage, &destination.final_path).await?;
        let parent = destination
            .final_path
            .parent()
            .ok_or_else(|| invalid("缺少接收父目录"))?
            .to_owned();
        tokio::task::spawn_blocking(move || disk::finish_batch(&parent, &[], &parent))
            .await
            .map_err(io::Error::other)??;
    }
    Ok(Report::new(
        bytes,
        resumed,
        files,
        start,
        Some(destination.final_path.clone()),
    ))
}

async fn send_files<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    source: Source,
    mut pace: Pacer,
    json: bool,
) -> io::Result<Report> {
    let bytes = source.manifest.validate(MAX_BYTES)?;
    let start = Instant::now();
    wire::write_json(
        stream,
        &Request::Files {
            manifest: source.manifest.clone(),
        },
    )
    .await?;
    let ready: Ready = loop {
        let ready: Ready = wire::read_json(stream).await?;
        if !ready.pending {
            break ready;
        }
        emit(
            json,
            &serde_json::json!({"event":"preparing"}),
            "接收端正在检查续传内容…",
        );
    };
    if let Some(error) = ready.error {
        return Err(invalid(error));
    }
    if ready.offsets.len() != source.manifest.entries.len() {
        return Err(invalid("续传清单长度不符"));
    }
    let mut resumed = 0;
    for (entry, resume) in source.manifest.entries.iter().zip(&ready.offsets) {
        if resume.offset > entry.size.unwrap_or(0) {
            return Err(invalid("续传偏移超出文件范围"));
        }
        resumed += resume.offset;
    }
    let mut progress = Progress::new(bytes - resumed, json);
    let (sender, mut chunks) = mpsc::channel::<io::Result<Chunk>>(8);
    // Eight chunks bound memory while filesystem reads/hash overlap TLS writes.
    let producer = tokio::spawn(async move {
        let result = produce_files(source, ready.offsets, &sender).await;
        if let Err(error) = result {
            let _ = sender.send(Err(error)).await;
        }
    });
    let mut producer = AbortTask(producer);
    while let Some(chunk) = chunks.recv().await {
        let (chunk, data) = match chunk? {
            Chunk::Data(bytes) => (bytes, true),
            Chunk::Digest(bytes) => (bytes, false),
            Chunk::Heartbeat => (vec![], false),
        };
        if data {
            pace.wait(chunk.len()).await;
            progress.advance(chunk.len() as u64);
        }
        wire::write_packet(stream, &chunk).await?;
    }
    (&mut producer.0).await.map_err(io::Error::other)?;
    timeout(wire::IDLE, stream.flush())
        .await
        .map_err(|_| invalid("文件发送刷新超时"))??;
    let mut report: Report = wire::read_json(stream).await?;
    if !report.verified || report.bytes != bytes || report.resumed_bytes != resumed {
        return Err(invalid("接收确认与发送清单不符"));
    }
    report.seconds = start.elapsed().as_secs_f64().max(0.000001);
    report.mib_per_second = (bytes - resumed) as f64 / 1048576.0 / report.seconds;
    Ok(report)
}

struct AbortTask(JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Chunk {
    Heartbeat,
    Data(Vec<u8>),
    Digest(Vec<u8>),
}

async fn produce_files(
    source: Source,
    offsets: Vec<Resume>,
    sender: &mpsc::Sender<io::Result<Chunk>>,
) -> io::Result<()> {
    for ((entry, path), resume) in source
        .manifest
        .entries
        .iter()
        .zip(&source.paths)
        .zip(offsets)
    {
        let Some(size) = entry.size else {
            continue;
        };
        let metadata = tokio::fs::symlink_metadata(path).await?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != size
            || manifest::modified(&metadata)? != entry.modified_ns
        {
            return Err(invalid("源文件在传输前发生变化"));
        }
        let mut file = File::open(path).await?;
        let mut hash = Sha256::new();
        let mut prefix_left = resume.offset;
        let mut prefix_buffer = vec![0; if prefix_left > 0 { wire::CHUNK } else { 0 }];
        let mut last_heartbeat = Instant::now();
        while prefix_left > 0 {
            let length = prefix_left.min(wire::CHUNK as u64) as usize;
            file.read_exact(&mut prefix_buffer[..length]).await?;
            hash.update(&prefix_buffer[..length]);
            prefix_left -= length as u64;
            if last_heartbeat.elapsed() >= Duration::from_secs(2) {
                if sender.send(Ok(Chunk::Heartbeat)).await.is_err() {
                    return Ok(());
                }
                last_heartbeat = Instant::now();
            }
        }
        let prefix: [u8; 32] = hash.clone().finalize().into();
        if prefix != resume.hash {
            return Err(invalid("续传前缀校验失败；清理对应 .partial 目录后重传"));
        }
        let mut left = size - resume.offset;
        while left > 0 {
            let length = left.min(wire::CHUNK as u64) as usize;
            let mut buffer = vec![0; length];
            file.read_exact(&mut buffer).await?;
            hash.update(&buffer);
            left -= length as u64;
            if sender.send(Ok(Chunk::Data(buffer))).await.is_err() {
                return Ok(());
            }
        }
        let metadata = file.metadata().await?;
        if metadata.len() != size || manifest::modified(&metadata)? != entry.modified_ns {
            return Err(invalid("源文件在传输中发生变化；未确认成功"));
        }
        if sender
            .send(Ok(Chunk::Digest(hash.finalize().to_vec())))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
    Ok(())
}

async fn send_benchmark<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    bytes: u64,
    mut pace: Pacer,
    json: bool,
) -> io::Result<Report> {
    wire::write_json(stream, &Request::Benchmark { bytes }).await?;
    let ready: Ready = wire::read_json(stream).await?;
    if let Some(error) = ready.error {
        return Err(invalid(error));
    }
    let start = Instant::now();
    let mut progress = Progress::new(bytes, json);
    let mut buffer = vec![0; wire::CHUNK];
    // This channel never compresses data, but use varied bytes for realistic fixtures.
    for (i, byte) in buffer.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(131) >> 7) as u8;
    }
    let mut hash = Sha256::new();
    let mut left = bytes;
    while left > 0 {
        let length = left.min(wire::CHUNK as u64) as usize;
        pace.wait(length).await;
        wire::write_all(stream, &buffer[..length]).await?;
        hash.update(&buffer[..length]);
        left -= length as u64;
        progress.advance(length as u64);
    }
    wire::write_all(stream, &hash.finalize()).await?;
    stream.flush().await?;
    let mut report: Report = wire::read_json(stream).await?;
    if !report.verified || report.bytes != bytes {
        return Err(invalid("测速确认非法"));
    }
    report.seconds = start.elapsed().as_secs_f64().max(0.000001);
    report.mib_per_second = bytes as f64 / 1048576.0 / report.seconds;
    Ok(report)
}
