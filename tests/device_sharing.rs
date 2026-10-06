//! Two isolated dummy daemons. No OS input or system clipboard is touched:
//! the initiating peer disables text/files before the pair can become effective.
#![cfg(target_os = "macos")]
use lan_mouse_ipc::{FrontendEvent, FrontendRequest, Sharing, UiAction, UiSnapshot};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Daemon {
    child: Option<Child>,
    config: PathBuf,
    socket: PathBuf,
    log: PathBuf,
}
impl Daemon {
    fn start(&mut self) {
        let log = std::fs::File::create(&self.log).unwrap();
        self.child = Some(
            Command::new(env!("CARGO_BIN_EXE_lan-mouse"))
                .arg("--config")
                .arg(&self.config)
                .arg("daemon")
                .env("LAN_MOUSE_IPC_SOCKET", &self.socket)
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&self.socket).is_err() {
            assert!(
                Instant::now() < deadline,
                "daemon log: {}",
                std::fs::read_to_string(&self.log).unwrap()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn action(&self, action: UiAction) -> UiSnapshot {
        let mut stream = UnixStream::connect(&self.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&FrontendRequest::Ui {
                id: "test".into(),
                action
            })
            .unwrap()
        )
        .unwrap();
        for line in BufReader::new(stream).lines() {
            let event: FrontendEvent = serde_json::from_str(&line.unwrap()).unwrap();
            if let FrontendEvent::UiResult { id, result } = event {
                if id == "test" {
                    return result.unwrap();
                }
            }
        }
        panic!("daemon closed IPC")
    }
    fn wait(&self, condition: impl Fn(&UiSnapshot) -> bool) -> UiSnapshot {
        let end = Instant::now() + Duration::from_secs(12);
        loop {
            let s = self.action(UiAction::Snapshot);
            if condition(&s) {
                return s;
            }
            assert!(
                Instant::now() < end,
                "snapshot {s:?}; log {}",
                std::fs::read_to_string(&self.log).unwrap()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.action(UiAction::Shutdown);
            let _ = child.wait();
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
fn ports() -> (u16, u16) {
    let mut listeners = Vec::new();
    let mut ports = Vec::new();
    while ports.len() < 2 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        if p > 65533 {
            continue;
        }
        if let (Ok(a), Ok(b)) = (
            TcpListener::bind(("127.0.0.1", p + 1)),
            TcpListener::bind(("127.0.0.1", p + 2)),
        ) {
            listeners.extend([l, a, b]);
            ports.push(p);
        }
    }
    (ports[0], ports[1])
}
#[test]
fn one_sided_pairing_negotiates_policies_and_preserves_them_on_restart() {
    let temp = tempfile::tempdir().unwrap();
    let (ap, bp) = ports();
    let mut daemons = Vec::new();
    for (name, port) in [("a", ap), ("b", bp)] {
        let config = temp.path().join(format!("{name}.toml"));
        let cert = temp.path().join(format!("{name}.pem"));
        std::fs::write(&config,format!("port={port}\ndiscovery=false\ncapture_backend=\"dummy\"\nemulation_backend=\"dummy\"\ncert_path={}\n",serde_json::to_string(&cert.to_string_lossy()).unwrap())).unwrap();
        daemons.push(Daemon {
            child: None,
            config,
            socket: temp.path().join(format!("{name}.sock")),
            log: temp.path().join(format!("{name}.log")),
        });
    }

    daemons[1].start();
    let bfp = daemons[1].action(UiAction::Snapshot).fingerprint;
    let mut ac = std::fs::read_to_string(&daemons[0].config).unwrap();
    ac.push_str(&format!("\n[authorized_fingerprints]\n\"{bfp}\"=\"B\"\n\n[[clients]]\nfingerprint=\"{bfp}\"\nips=[\"127.0.0.1\"]\nport={bp}\nposition=\"right\"\nactivate_on_startup=true\nsharing={{mouse=false,keyboard=false,clipboard=false,files=false}}\n"));
    std::fs::write(&daemons[0].config, ac).unwrap();
    daemons[0].start();
    let afp = daemons[0].action(UiAction::Snapshot).fingerprint;
    let pending = daemons[1].wait(|s| !s.pair_requests.is_empty());
    assert_eq!(pending.pair_requests[0].fingerprint, afp);
    assert!(pending.clients.is_empty());
    assert!(pending.authorized.is_empty());
    daemons[1].action(UiAction::RejectPair {
        fingerprint: afp.clone(),
    });
    std::thread::sleep(Duration::from_millis(1300));
    let rejected = daemons[1].action(UiAction::Snapshot);
    assert!(
        rejected.clients.is_empty()
            && rejected.authorized.is_empty()
            && rejected.pair_requests.is_empty()
    );
    daemons[1].action(UiAction::Authorize {
        description: "A".into(),
        fingerprint: afp.clone(),
    });
    let paired = daemons[1].wait(|s| s.clients.len() == 1);
    assert_eq!(paired.clients.len(), 1);
    assert_eq!(
        paired.clients[0].1.fingerprint.as_deref(),
        Some(afp.as_str())
    );
    assert_eq!(paired.clients[0].1.pos, lan_mouse_ipc::Position::Left);
    assert_eq!(paired.clients[0].1.sharing, Sharing::default());
    assert!(paired.clients[0].2.active);
    daemons[0].wait(|s| s.clients[0].2.peer_sharing == Some(Sharing::default()));
    let a_policy = Sharing {
        keyboard: true,
        ..Sharing::OFF
    };
    daemons[0].action(UiAction::SetSharing {
        id: 0,
        sharing: a_policy,
    });
    daemons[1].wait(|s| s.clients[0].2.peer_sharing == Some(a_policy));
    let b_policy = Sharing {
        keyboard: false,
        ..Sharing::default()
    };
    daemons[1].action(UiAction::SetSharing {
        id: 0,
        sharing: b_policy,
    });
    daemons[0].wait(|s| s.clients[0].2.peer_sharing == Some(b_policy));
    daemons[0].action(UiAction::SetPaused { paused: true });
    daemons[1].wait(|s| s.clients[0].2.peer_sharing == Some(Sharing::OFF));
    daemons[0].action(UiAction::SetPaused { paused: false });
    daemons[1].wait(|s| s.clients[0].2.peer_sharing == Some(a_policy));
    daemons[1].action(UiAction::SetActive {
        id: 0,
        active: false,
    });
    daemons[0].wait(|s| s.clients[0].2.peer_sharing == Some(Sharing::OFF));
    daemons[1].stop();
    daemons[1].start();
    let saved = daemons[1].action(UiAction::Snapshot);
    assert_eq!(saved.clients.len(), 1);
    assert!(!saved.clients[0].2.active);
    assert_eq!(saved.clients[0].1.sharing, b_policy);
    assert_eq!(saved.clipboard_target, Some(0));
    daemons[1].action(UiAction::SetActive {
        id: 0,
        active: true,
    });
    daemons[0].wait(|s| s.clients[0].2.peer_sharing == Some(b_policy));
    // Repeated authenticated handshakes do not duplicate clients or reset preferences.
    assert_eq!(daemons[1].action(UiAction::Snapshot).clients.len(), 1);
    daemons[0].stop();
    daemons[0].start();
    let saved = daemons[0].action(UiAction::Snapshot);
    assert_eq!(saved.clients[0].1.sharing, a_policy);
    assert_eq!(saved.clipboard_target, Some(0));
    daemons[1].action(UiAction::Revoke {
        fingerprint: afp.clone(),
    });
    daemons[0].wait(|s| s.clients[0].2.peer_sharing.is_none());
    let revoked = daemons[1].action(UiAction::Snapshot);
    assert!(!revoked.authorized.contains_key(&afp));
    assert!(revoked.pair_requests.is_empty());
    assert_eq!(revoked.clients[0].1.sharing, b_policy);
    daemons[1].stop();
    daemons[0].wait(|s| s.clients[0].2.peer_sharing.is_none());
    daemons[0].stop();
}
