// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use super::{Authorized, Pacer, Target, TlsConfig, connect, emit, manifest, native, send_files};
use std::{
    future::Future,
    io,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{
    task::JoinSet,
    time::{MissedTickBehavior, interval},
};

const DEBOUNCE: Duration = Duration::from_millis(350);

struct Changes {
    observed: native::Snapshot,
    pending: Option<native::Snapshot>,
    due: Instant,
    failures: u32,
}

impl Changes {
    fn new(observed: native::Snapshot) -> Self {
        Self {
            observed,
            pending: None,
            due: Instant::now(),
            failures: 0,
        }
    }
    fn observe(&mut self, snapshot: native::Snapshot, now: Instant) -> bool {
        if snapshot == self.observed {
            return false;
        }
        self.pending =
            (!snapshot.received && !snapshot.paths.is_empty()).then_some(snapshot.clone());
        self.observed = snapshot;
        self.due = now + DEBOUNCE;
        self.failures = 0;
        true
    }
    fn ready(&self, now: Instant) -> Option<Vec<PathBuf>> {
        (now >= self.due)
            .then(|| self.pending.as_ref().map(|p| p.paths.clone()))
            .flatten()
    }
    fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.due = now + Duration::from_secs((1u64 << self.failures.min(5)).min(30));
    }
}

pub(super) async fn run(
    target: Target,
    tls: TlsConfig,
    keys: Authorized,
    json: bool,
) -> io::Result<()> {
    changes(
        || async {
            tokio::task::spawn_blocking(native::read_snapshot)
                .await
                .map_err(io::Error::other)?
        },
        move |paths| {
            let target = target.clone();
            let tls = tls.clone();
            let keys = keys.clone();
            async move {
                let source = tokio::task::spawn_blocking(move || manifest::collect(paths))
                    .await
                    .map_err(io::Error::other)??;
                let mut stream = connect(&target, tls.client.clone(), &keys).await?;
                let pace = Pacer::new(&target, tls.client, keys).await;
                let report = send_files(&mut stream, source, pace, json).await?;
                emit(json, &serde_json::json!({"event":"auto_sent","files":report.files,"mib_per_second":report.mib_per_second}),
                    &format!("文件已自动送达并校验：{} 个文件 · {:.1} MiB/s", report.files, report.mib_per_second));
                Ok(())
            }
        },
        json,
    )
    .await
}

// Polling continues during transfer. A newer copy replaces pending work; remote
// writes carry a native marker, even when receiver and watcher are separate apps.
pub(super) async fn changes<R, RF, S, SF>(mut read: R, mut send: S, json: bool) -> io::Result<()>
where
    R: FnMut() -> RF,
    RF: Future<Output = io::Result<native::Snapshot>>,
    S: FnMut(Vec<PathBuf>) -> SF,
    SF: Future<Output = io::Result<()>> + Send + 'static,
{
    // Never transfer clipboard contents that predate the user's opt-in.
    let mut state = Changes::new(read().await?);
    emit(
        json,
        &serde_json::json!({"event":"watching"}),
        "自动文件复制已开启：重新复制文件即发送，完成后在对端粘贴",
    );
    let mut tick = interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut outgoing = JoinSet::new();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                match read().await {
                    Ok(snapshot) => {
                        if state.observe(snapshot, Instant::now()) {
                            outgoing.abort_all();
                            while outgoing.join_next().await.is_some() {}
                        }
                    }
                    Err(error) => { log::debug!("file clipboard unavailable: {error}"); continue; }
                }
                if outgoing.is_empty() {
                    if let Some(paths) = state.ready(Instant::now()) {
                        emit(json, &serde_json::json!({"event":"auto_sending","files":paths.len()}), "检测到新复制的文件，正在自动发送…");
                        outgoing.spawn(send(paths));
                    }
                }
            }
            Some(result) = outgoing.join_next() => {
                match result {
                    Ok(Ok(())) => {
                        state.pending = None;
                        emit(json, &serde_json::json!({"event":"watching"}), "发送完成，等待下一次复制");
                    }
                    result => {
                        let error = match result {
                            Ok(Err(error)) => error.to_string(),
                            Err(error) => error.to_string(),
                            _ => unreachable!(),
                        };
                        state.failed(Instant::now());
                        emit(json, &serde_json::json!({"event":"auto_retry","error":error}), &format!("自动发送失败，将重试：{error}"));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn copied(revision: u64, received: bool) -> native::Snapshot {
        native::Snapshot {
            revision,
            paths: vec![PathBuf::from("file")],
            received,
        }
    }
    #[test]
    fn startup_remote_echo_and_recopied_paths() {
        let now = Instant::now();
        let mut state = Changes::new(copied(1, false));
        assert!(state.ready(now + Duration::from_secs(1)).is_none());
        assert!(!state.observe(copied(1, false), now));
        assert!(state.observe(copied(2, false), now));
        assert!(state.ready(now).is_none());
        assert_eq!(
            state.ready(now + DEBOUNCE),
            Some(vec![PathBuf::from("file")])
        );
        state.failed(now);
        assert!(state.ready(now + Duration::from_secs(1)).is_none());
        assert!(state.ready(now + Duration::from_secs(2)).is_some());
        assert!(state.observe(copied(3, true), now));
        assert!(state.ready(now + Duration::from_secs(60)).is_none());
        assert!(state.observe(copied(4, false), now));
        assert!(state.ready(now + DEBOUNCE).is_some());
        assert!(state.observe(
            native::Snapshot {
                revision: 5,
                ..Default::default()
            },
            now
        ));
        assert!(state.ready(now + DEBOUNCE).is_none());
    }

    #[tokio::test]
    async fn new_copy_cancels_stale_transfer_and_remote_write_does_not_send() {
        use std::sync::{Arc, Mutex};
        let clipboard = Arc::new(Mutex::new(copied(1, false)));
        let reader = clipboard.clone();
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let watcher = tokio::spawn(changes(
            move || {
                let value = reader.lock().unwrap().clone();
                async move { Ok(value) }
            },
            move |paths| {
                let sent = sent.clone();
                async move {
                    let _ = sent.send(paths);
                    std::future::pending::<()>().await;
                    Ok(())
                }
            },
            false,
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(received.try_recv().is_err());
        *clipboard.lock().unwrap() = copied(2, false);
        tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        *clipboard.lock().unwrap() = copied(3, true);
        tokio::time::sleep(Duration::from_millis(750)).await;
        assert!(received.try_recv().is_err());
        *clipboard.lock().unwrap() = copied(4, false);
        tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        watcher.abort();
        assert!(watcher.await.unwrap_err().is_cancelled());
    }
}
