//! Daemon-owned clipboard transfers. Recipient is captured with each copy;
//! changing the control target never redirects a transfer already in progress.
use super::*;
use tokio::sync::watch;
#[derive(Clone, Default, PartialEq)]
pub(crate) struct ManagedSettings {
    pub port: u16,
    pub output: PathBuf,
    pub target: Option<(SocketAddr, String)>,
    pub enabled: bool,
}
pub(crate) struct Managed {
    inactive: watch::Receiver<bool>,
    settings: watch::Sender<ManagedSettings>,
    error: watch::Receiver<Option<String>>,
    task: JoinHandle<()>,
}
impl Managed {
    pub fn new(cert: webrtc_dtls::crypto::Certificate, keys: Authorized) -> Self {
        let (settings, rx) = watch::channel(ManagedSettings::default());
        let (error, err) = watch::channel(None);
        let (inactive_tx, inactive) = watch::channel(true);
        let task = tokio::task::spawn_local(run(cert, keys, rx, error, inactive_tx));
        Self {
            inactive,
            settings,
            error: err,
            task,
        }
    }
    pub fn configure(&self, value: ManagedSettings) {
        self.settings.send_if_modified(|old| {
            if *old == value {
                false
            } else {
                *old = value;
                true
            }
        });
    }
    pub async fn wait_until_inactive(&mut self) -> Result<(), String> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !*self.inactive.borrow_and_update() {
                self.inactive
                    .changed()
                    .await
                    .map_err(|_| "文件后台已停止".to_owned())?;
            }
            Ok(())
        })
        .await
        .map_err(|_| "文件暂停未确认".to_owned())?
    }
    pub fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }
    pub async fn terminate(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}
async fn run(
    cert: webrtc_dtls::crypto::Certificate,
    keys: Authorized,
    settings: watch::Receiver<ManagedSettings>,
    error: watch::Sender<Option<String>>,
    inactive: watch::Sender<bool>,
) {
    run_with_reader(cert, keys, settings, error, inactive, || async {
        tokio::task::spawn_blocking(native::read_snapshot)
            .await
            .map_err(io::Error::other)?
    })
    .await;
}
async fn run_with_reader<R, RF>(
    cert: webrtc_dtls::crypto::Certificate,
    keys: Authorized,
    mut settings: watch::Receiver<ManagedSettings>,
    error: watch::Sender<Option<String>>,
    inactive: watch::Sender<bool>,
    mut read: R,
) where
    R: FnMut() -> RF,
    RF: std::future::Future<Output = io::Result<native::Snapshot>>,
{
    let tls = match TlsConfig::with_alpn(&cert, keys.clone(), ALPN) {
        Ok(t) => t,
        Err(e) => {
            error.send_replace(Some(e.to_string()));
            return;
        }
    };
    let mut receiver = JoinSet::new();
    let mut outgoing = JoinSet::new();
    let mut receiving = None;
    let mut sending: Option<String> = None;
    let mut state = None;
    let mut pending_target = None;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let current = settings.borrow().clone();
        let receive = if current.enabled {
            Some((current.port, current.output.clone()))
        } else {
            None
        };
        if receiving != receive {
            receiver.abort_all();
            while receiver.join_next().await.is_some() {}
            receiving = receive.clone();
            if let Some((port, output)) = receive {
                let tls = tls.clone();
                let keys = keys.clone();
                receiver.spawn(async move {
                    receive_managed(
                        None,
                        tls,
                        keys,
                        output,
                        SocketAddr::from(([0, 0, 0, 0], port)),
                        ReceiveOptions {
                            once: false,
                            max_bytes: MAX_BYTES,
                            allow_benchmark: false,
                            json: false,
                            clipboard: true,
                        },
                    )
                    .await
                });
            }
        }
        if sending
            .as_ref()
            .is_some_and(|fp| !current.enabled || !keys.read().is_ok_and(|k| k.contains_key(fp)))
        {
            outgoing.abort_all();
            while outgoing.join_next().await.is_some() {}
            sending = None;
            state = None;
            pending_target = None;
        }
        inactive.send_replace(!current.enabled && outgoing.is_empty() && receiver.is_empty());
        if !current.enabled {
            state = None;
            pending_target = None;
        }
        tokio::select! {
            changed=settings.changed()=>{if changed.is_err(){break;}},
            _=tick.tick(),if current.enabled=>{
                let snapshot=match read().await {Ok(s)=>s,Err(e)=>{log::debug!("file clipboard: {e}");continue;}};
                let changes=state.get_or_insert_with(||super::watch::Changes::new(snapshot.clone()));
                if changes.observe(snapshot,Instant::now()) {
                    outgoing.abort_all();while outgoing.join_next().await.is_some(){} sending=None;
                    pending_target=current.target.clone();
                    // Copies made with no valid target are not sent later to another device.
                    if pending_target.is_none(){changes.pending=None;}
                }
                if outgoing.is_empty() {
                    if let (Some(paths),Some((to,fingerprint)))=(changes.ready(Instant::now()),pending_target.clone()) {
                        if !keys.read().is_ok_and(|k|k.contains_key(&fingerprint)){changes.pending=None;continue;}
                        sending=Some(fingerprint.clone());let tls=tls.clone();let keys=keys.clone();
                        outgoing.spawn(async move {
                            let source=tokio::task::spawn_blocking(move||manifest::collect(paths)).await.map_err(io::Error::other)??;
                            let target=Target{to,fingerprint:Some(fingerprint),mode:Mode::Full,limit_mib:None};
                            let mut stream=connect(&target,tls.client.clone(),&keys).await?;
                            send_files(&mut stream,source,Pacer::new(&target,tls.client,keys).await,false).await.map(|_|())
                        });
                    }
                }
            },
            Some(result)=outgoing.join_next()=>{
                sending=None;
                match result {Ok(Ok(()))=>{if let Some(s)=&mut state{s.pending=None;}error.send_replace(None);},result=>{
                    if let Some(s)=&mut state{s.failed(Instant::now());}
                    error.send_replace(Some(format!("自动文件发送失败，将重试：{result:?}")));
                }}
            },
            Some(result)=receiver.join_next()=>{
                error.send_replace(Some(format!("文件接收服务：{result:?}")));
                // Retry after a delay; a busy port must not spin the daemon.
                tokio::time::sleep(Duration::from_secs(2)).await;receiving=None;
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn target_switch_keeps_inflight_recipient_and_pause_or_policy_revocation_cancels() {
        let a = webrtc_dtls::crypto::Certificate::generate_self_signed(["A".into()]).unwrap();
        let b = webrtc_dtls::crypto::Certificate::generate_self_signed(["B".into()]).unwrap();
        let c = webrtc_dtls::crypto::Certificate::generate_self_signed(["C".into()]).unwrap();
        let afp = crypto::certificate_fingerprint(&a);
        let bfp = crypto::certificate_fingerprint(&b);
        let cfp = crypto::certificate_fingerprint(&c);
        let keys = Authorized::default();
        keys.write().unwrap().insert(bfp.clone(), "B".into());
        keys.write().unwrap().insert(cfp.clone(), "C".into());
        let server = TlsConfig::with_alpn(
            &b,
            Arc::new(RwLock::new(std::collections::HashMap::from([(
                afp,
                "A".into(),
            )]))),
            ALPN,
        )
        .unwrap();
        let b_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let baddr = b_listener.local_addr().unwrap();
        let c_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let caddr = c_listener.local_addr().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.bin");
        std::fs::write(&path, b"a real collected file").unwrap();
        let settings = ManagedSettings {
            port: 0,
            output: temp.path().join("received"),
            target: Some((baddr, bfp.clone())),
            enabled: true,
        };
        let (tx, rx) = watch::channel(settings.clone());
        let (error, _) = watch::channel(None);
        let (inactive, mut paused) = watch::channel(false);
        let clipboard = Arc::new(StdMutex::new(native::Snapshot::default()));
        let reader = clipboard.clone();
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let task = tokio::spawn(run_with_reader(
            a,
            keys.clone(),
            rx,
            error,
            inactive,
            move || {
                let s = reader.lock().unwrap().clone();
                count.fetch_add(1, Ordering::Relaxed);
                async move { Ok(s) }
            },
        ));
        while reads.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for (revision, pause) in [(1, true), (2, false)] {
            *clipboard.lock().unwrap() = native::Snapshot {
                revision,
                paths: vec![path.clone()],
                received: false,
            };
            let (socket, _) = timeout(Duration::from_secs(3), b_listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut stream = TlsAcceptor::from(server.server.clone())
                .accept(socket)
                .await
                .unwrap();
            assert!(matches!(
                wire::read_json::<_, Request>(&mut stream).await.unwrap(),
                Request::Files { .. }
            ));
            let mut changed = settings.clone();
            changed.target = Some((caddr, cfp.clone()));
            tx.send_replace(changed.clone());
            // No Ready response yet: the transfer remains open at B while target becomes C.
            assert!(
                timeout(Duration::from_millis(400), stream.read_u8())
                    .await
                    .is_err()
            );
            if pause {
                changed.enabled = false;
                tx.send_replace(changed);
            } else {
                keys.write().unwrap().remove(&bfp);
            }
            assert!(
                timeout(Duration::from_secs(2), stream.read_u8())
                    .await
                    .unwrap()
                    .is_err()
            );
            assert!(
                timeout(Duration::from_millis(400), c_listener.accept())
                    .await
                    .is_err()
            );
            if pause {
                timeout(Duration::from_secs(2), async {
                    while !*paused.borrow_and_update() {
                        paused.changed().await.unwrap();
                    }
                })
                .await
                .unwrap();
                tx.send_replace(settings.clone());
                // Resume takes a baseline; the prior copy must not be retransmitted.
                tokio::time::sleep(Duration::from_millis(350)).await;
            }
        }
        task.abort();
        let _ = task.await;
    }
}
