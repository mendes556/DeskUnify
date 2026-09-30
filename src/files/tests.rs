// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use super::*;
use std::{collections::HashMap, fs};
use webrtc_dtls::crypto::Certificate;

fn certificate() -> Certificate {
    Certificate::generate_self_signed(["ignored".into()]).unwrap()
}
fn keys(cert: Option<&Certificate>) -> Authorized {
    let mut keys = HashMap::new();
    if let Some(cert) = cert {
        keys.insert(crypto::certificate_fingerprint(cert), "test".into());
    }
    Arc::new(RwLock::new(keys))
}
fn full() -> Pacer {
    Pacer {
        rate: Arc::new(AtomicU64::new(10_000 * 1048576)),
        task: None,
        next: Instant::now(),
    }
}

async fn ready<S: AsyncRead + Unpin>(stream: &mut S) -> Ready {
    loop {
        let ready: Ready = wire::read_json(stream).await.unwrap();
        if !ready.pending {
            return ready;
        }
    }
}
fn options() -> ReceiveOptions {
    ReceiveOptions {
        once: true,
        max_bytes: MAX_BYTES,
        allow_benchmark: true,
        json: false,
        clipboard: false,
    }
}

struct Pair {
    a: TlsConfig,
    a_keys: Authorized,
    b: TlsConfig,
    b_keys: Authorized,
    pin: String,
}
impl Pair {
    fn new(trust_client: bool, trust_server: bool) -> Self {
        let a = certificate();
        let b = certificate();
        let a_keys = keys(trust_server.then_some(&b));
        let b_keys = keys(trust_client.then_some(&a));
        Self {
            a: TlsConfig::with_alpn(&a, a_keys.clone(), ALPN).unwrap(),
            b: TlsConfig::with_alpn(&b, b_keys.clone(), ALPN).unwrap(),
            a_keys,
            b_keys,
            pin: crypto::certificate_fingerprint(&b),
        }
    }
    async fn start(
        &self,
        output: &Path,
        options: ReceiveOptions,
    ) -> (Target, JoinHandle<io::Result<Option<Report>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = Target {
            to: listener.local_addr().unwrap(),
            fingerprint: Some(self.pin.clone()),
            mode: Mode::Full,
            limit_mib: None,
        };
        let server = self.b.server.clone();
        let keys = self.b_keys.clone();
        let output = output.to_owned();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await?;
            receive_connection(
                socket,
                TlsAcceptor::from(server),
                keys,
                output,
                Arc::new(Mutex::new(())),
                options,
            )
            .await
        });
        (target, task)
    }
}

#[tokio::test]
async fn tls_transfers_directory_empty_files_and_32_byte_file_then_reuses_verified_files() {
    let pair = Pair::new(true, true);
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let root = source.path().join("中文");
    fs::create_dir_all(root.join("empty-dir")).unwrap();
    fs::write(root.join("empty-file"), []).unwrap();
    fs::write(root.join("small"), [7; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("small"), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(root.join("big"), vec![9; wire::CHUNK + 123]).unwrap();
    for attempt in 0..2 {
        let (target, server) = pair.start(output.path(), options()).await;
        let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
            .await
            .unwrap();
        let report = send_files(
            &mut stream,
            manifest::collect(vec![root.clone()]).unwrap(),
            full(),
            false,
        )
        .await
        .unwrap();
        let received = server.await.unwrap().unwrap().unwrap();
        assert_eq!(report.bytes, (wire::CHUNK + 155) as u64);
        assert_eq!(report.files, 3);
        assert_eq!(
            report.resumed_bytes,
            if attempt == 0 { 0 } else { report.bytes }
        );
        let destination = received.output.unwrap().join("中文");
        assert!(destination.join("empty-dir").is_dir());
        for name in ["empty-file", "small", "big"] {
            assert_eq!(
                fs::read(root.join(name)).unwrap(),
                fs::read(destination.join(name)).unwrap()
            );
            assert_eq!(
                fs::metadata(root.join(name)).unwrap().modified().unwrap(),
                fs::metadata(destination.join(name))
                    .unwrap()
                    .modified()
                    .unwrap()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                fs::metadata(destination.join("small"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o100
                    != 0
            );
        }
    }
}

#[tokio::test]
async fn interruption_resumes_only_complete_chunks_and_verifies_prefix() {
    let pair = Pair::new(true, true);
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = source.path().join("resume.bin");
    let bytes = vec![19; wire::CHUNK * 2 + 123];
    fs::write(&path, &bytes).unwrap();
    let manifest = manifest::collect(vec![path.clone()]).unwrap().manifest;
    let (target, server) = pair.start(output.path(), options()).await;
    let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
        .await
        .unwrap();
    wire::write_json(
        &mut stream,
        &Request::Files {
            manifest: manifest.clone(),
        },
    )
    .await
    .unwrap();
    let ready = ready(&mut stream).await;
    assert_eq!(ready.offsets[0].offset, 0);
    wire::write_packet(&mut stream, &[]).await.unwrap();
    wire::write_packet(&mut stream, &bytes[..wire::CHUNK])
        .await
        .unwrap();
    wire::write_all(&mut stream, &(wire::CHUNK as u32).to_be_bytes())
        .await
        .unwrap();
    wire::write_all(&mut stream, &bytes[..123]).await.unwrap();
    stream.flush().await.unwrap();
    drop(stream);
    assert!(server.await.unwrap().is_err());
    let (target, server) = pair.start(output.path(), options()).await;
    let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
        .await
        .unwrap();
    let report = send_files(
        &mut stream,
        manifest::collect(vec![path]).unwrap(),
        full(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.resumed_bytes, wire::CHUNK as u64);
    assert_eq!(
        fs::read(report.output.unwrap().join("resume.bin")).unwrap(),
        bytes
    );
    assert!(server.await.unwrap().unwrap().is_some());
}

#[tokio::test]
async fn wrong_digest_does_not_commit_and_retry_starts_clean() {
    let pair = Pair::new(true, true);
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = source.path().join("data");
    fs::write(&path, b"actual").unwrap();
    let manifest = manifest::collect(vec![path.clone()]).unwrap().manifest;
    let (target, server) = pair.start(output.path(), options()).await;
    let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
        .await
        .unwrap();
    wire::write_json(&mut stream, &Request::Files { manifest })
        .await
        .unwrap();
    let _ = ready(&mut stream).await;
    wire::write_packet(&mut stream, b"actual").await.unwrap();
    wire::write_packet(&mut stream, &[0; 32]).await.unwrap();
    stream.flush().await.unwrap();
    assert!(server.await.unwrap().is_err());
    assert!(fs::read_dir(output.path()).unwrap().all(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".partial")
    }));
    let (target, server) = pair.start(output.path(), options()).await;
    let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
        .await
        .unwrap();
    let report = send_files(
        &mut stream,
        manifest::collect(vec![path]).unwrap(),
        full(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.resumed_bytes, 0);
    assert!(server.await.unwrap().unwrap().is_some());
}

#[tokio::test]
async fn unpaired_client_server_and_wrong_pin_are_rejected() {
    for (client, server, wrong_pin) in [
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ] {
        let pair = Pair::new(client, server);
        let output = tempfile::tempdir().unwrap();
        let (mut target, task) = pair.start(output.path(), options()).await;
        if wrong_pin {
            target.fingerprint = Some(["00"; 32].join(":"));
        }
        let result = connect(&target, pair.a.client.clone(), &pair.a_keys).await;
        if let Ok(mut stream) = result {
            assert!(
                send_benchmark(&mut stream, 32, full(), false)
                    .await
                    .is_err()
            );
        }
        assert!(task.await.unwrap().is_err());
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn benchmark_requires_explicit_opt_in_and_never_creates_files() {
    let pair = Pair::new(true, true);
    let output = tempfile::tempdir().unwrap();
    for allow in [false, true] {
        let mut options = options();
        options.allow_benchmark = allow;
        let (target, task) = pair.start(output.path(), options).await;
        let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
            .await
            .unwrap();
        let result = send_benchmark(&mut stream, wire::CHUNK as u64 + 32, full(), false).await;
        assert_eq!(result.is_ok(), allow);
        assert_eq!(task.await.unwrap().is_ok(), allow);
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn corrupted_resume_prefix_fails_without_overwriting_completed_output() {
    let pair = Pair::new(true, true);
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = source.path().join("source");
    fs::write(&path, vec![1; wire::CHUNK * 2]).unwrap();
    let (target, task) = pair.start(output.path(), options()).await;
    let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
        .await
        .unwrap();
    let report = send_files(
        &mut stream,
        manifest::collect(vec![path.clone()]).unwrap(),
        full(),
        false,
    )
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    let completed = report.output.unwrap().join("source");
    fs::write(&completed, vec![2; wire::CHUNK * 2]).unwrap();
    let (target, task) = pair.start(output.path(), options()).await;
    let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
        .await
        .unwrap();
    assert!(
        send_files(
            &mut stream,
            manifest::collect(vec![path]).unwrap(),
            full(),
            false
        )
        .await
        .is_err()
    );
    drop(stream);
    assert!(task.await.unwrap().is_err());
    assert_eq!(fs::read(completed).unwrap(), vec![2; wire::CHUNK * 2]);
}

#[tokio::test]
async fn oversized_frame_is_rejected_before_reading_or_committing_payload() {
    let pair = Pair::new(true, true);
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = source.path().join("data");
    fs::write(&path, b"one").unwrap();
    let (target, task) = pair.start(output.path(), options()).await;
    let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
        .await
        .unwrap();
    wire::write_json(
        &mut stream,
        &Request::Files {
            manifest: manifest::collect(vec![path]).unwrap().manifest,
        },
    )
    .await
    .unwrap();
    let _ = ready(&mut stream).await;
    wire::write_all(&mut stream, &u32::MAX.to_be_bytes())
        .await
        .unwrap();
    stream.flush().await.unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(fs::read_dir(output.path()).unwrap().all(|p| {
        p.unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".partial")
    }));
}

#[tokio::test]
async fn source_changes_are_rejected_before_success_is_reported() {
    let pair = Pair::new(true, true);
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = source.path().join("data");
    fs::write(&path, b"old").unwrap();
    let collected = manifest::collect(vec![path.clone()]).unwrap();
    fs::write(path, b"new contents").unwrap();
    let (target, task) = pair.start(output.path(), options()).await;
    let mut stream = connect(&target, pair.a.client.clone(), &pair.a_keys)
        .await
        .unwrap();
    assert!(
        send_files(&mut stream, collected, full(), false)
            .await
            .is_err()
    );
    drop(stream);
    assert!(task.await.unwrap().is_err());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_copy_automatically_transfers_over_tls_and_received_urls_do_not_echo() {
    let pair = Pair::new(true, true);
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = source.path().join("自动复制 #%.txt");
    fs::write(&path, "原生复制 → TLS → 文件剪贴板").unwrap();
    let (target, server) = pair.start(output.path(), options()).await;
    let a = native::TestClipboard::new();
    let b = native::TestClipboard::new();
    let reader = a.clone();
    let receiver = b.clone();
    let client = pair.a.client.clone();
    let client_keys = pair.a_keys.clone();
    let source_name = path.file_name().unwrap().to_owned();
    let (sent, mut result) = tokio::sync::mpsc::unbounded_channel();
    let watcher_a = tokio::spawn(watch::changes(
        move || {
            let value = reader.snapshot();
            async move { value }
        },
        move |paths| {
            let target = target.clone();
            let client = client.clone();
            let keys = client_keys.clone();
            let receiver = receiver.clone();
            let source_name = source_name.clone();
            let sent = sent.clone();
            async move {
                let revision = receiver.snapshot()?.revision;
                let source = manifest::collect(paths)?;
                let mut stream = connect(&target, client, &keys).await?;
                let report = send_files(&mut stream, source, full(), false).await?;
                let path = report.output.unwrap().join(source_name);
                assert!(receiver.publish(vec![path.clone()], revision)?);
                let _ = sent.send(path);
                Ok(())
            }
        },
        false,
    ));
    let reader = b.clone();
    let (echo, mut echoes) = tokio::sync::mpsc::unbounded_channel();
    let watcher_b = tokio::spawn(watch::changes(
        move || {
            let value = reader.snapshot();
            async move { value }
        },
        move |paths| {
            let echo = echo.clone();
            async move {
                let _ = echo.send(paths);
                Ok(())
            }
        },
        false,
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.copy(&path);
    let received = timeout(Duration::from_secs(5), result.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(received).unwrap(), fs::read(&path).unwrap());
    assert!(b.snapshot().unwrap().received);
    tokio::time::sleep(Duration::from_millis(750)).await;
    assert!(
        echoes.try_recv().is_err(),
        "received files were automatically sent back"
    );
    assert!(server.await.unwrap().unwrap().unwrap().verified);
    watcher_a.abort();
    watcher_b.abort();
    let _ = watcher_a.await;
    let _ = watcher_b.await;
}
