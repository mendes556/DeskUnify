// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
//! LAN discovery advertises public identity only; announcements never grant trust.
use lan_mouse_ipc::DiscoveredDevice;
use mdns_sd::{DaemonEvent, Receiver, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    time::{Duration, Instant},
};

const SERVICE: &str = "_lanbridge._udp.local.";
const MAX_DEVICES: usize = 64;

pub(crate) struct Discovery {
    daemon: Option<ServiceDaemon>,
    events: Option<Receiver<ServiceEvent>>,
    monitor: Option<Receiver<DaemonEvent>>,
    fingerprint: String,
    name: String,
    identity: String,
    port: u16,
    enabled: bool,
    peers: BTreeMap<String, (DiscoveredDevice, Instant)>,
    pub error: Option<String>,
}

impl Discovery {
    pub fn new(fingerprint: String) -> Self {
        let identity = fingerprint.replace(':', "");
        let name = hostname::get()
            .ok()
            .and_then(|name| name.into_string().ok())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "DeskUnify".into());
        Self {
            daemon: None,
            events: None,
            monitor: None,
            fingerprint,
            name,
            identity,
            port: 0,
            enabled: false,
            peers: BTreeMap::new(),
            error: None,
        }
    }

    pub fn configure(&mut self, enabled: bool, port: u16) {
        if self.enabled == enabled && self.port == port {
            return;
        }
        self.enabled = enabled;
        self.port = port;
        self.error = None;
        if !enabled {
            self.stop();
            return;
        }
        let result = (|| {
            if self.daemon.is_none() {
                let daemon = ServiceDaemon::new()?;
                self.monitor = Some(daemon.monitor()?);
                self.daemon = Some(daemon);
            }
            let properties = [
                ("fingerprint", self.fingerprint.as_str()),
                ("name", self.name.as_str()),
                ("platform", std::env::consts::OS),
                ("version", "1"),
            ];
            let info = ServiceInfo::new(
                SERVICE,
                &self.identity[..24],
                &format!("lanbridge-{}.local.", &self.identity[..24]),
                "",
                port,
                &properties[..],
            )?
            .enable_addr_auto();
            self.daemon
                .as_ref()
                .expect("initialized daemon")
                .register(info)
        })();
        if let Err(error) = result {
            self.error = Some(format!("局域网发现不可用：{error}"));
        }
    }

    pub fn scan(&mut self) -> Result<(), String> {
        if !self.enabled {
            return Err("局域网发现已在配置中关闭；可手动添加设备".into());
        }
        let daemon = self.daemon.as_ref().ok_or_else(|| {
            self.error
                .clone()
                .unwrap_or_else(|| "局域网发现不可用".into())
        })?;
        if self.events.is_some() {
            let _ = daemon.stop_browse(SERVICE);
        }
        self.peers.clear();
        self.events = Some(
            daemon
                .browse(SERVICE)
                .map_err(|error| format!("无法扫描局域网：{error}"))?,
        );
        Ok(())
    }

    pub fn devices(&mut self) -> Vec<DiscoveredDevice> {
        if let Some(monitor) = &self.monitor {
            for _ in 0..256 {
                let Ok(event) = monitor.try_recv() else {
                    break;
                };
                if let DaemonEvent::Error(error) = event {
                    self.error = Some(format!("局域网发现报告错误：{error}"));
                }
            }
        }
        if let Some(events) = &self.events {
            for _ in 0..256 {
                let Ok(event) = events.try_recv() else {
                    break;
                };
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        if let Some(peer) = parse(&info, &self.fingerprint) {
                            let id = info.get_fullname().to_owned();
                            if self.peers.contains_key(&id) || self.peers.len() < MAX_DEVICES {
                                self.peers.insert(id, (peer, Instant::now()));
                            }
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, id) => {
                        self.peers.remove(&id);
                    }
                    _ => {}
                }
            }
        }
        self.peers
            .retain(|_, (_, seen)| seen.elapsed() < Duration::from_secs(120));
        self.peers
            .values()
            .map(|(device, _)| device.clone())
            .collect()
    }

    pub fn stop(&mut self) {
        self.events = None;
        self.monitor = None;
        self.peers.clear();
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.shutdown();
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop();
    }
}

fn parse(info: &ResolvedService, local_fingerprint: &str) -> Option<DiscoveredDevice> {
    if info.get_property_val_str("version") != Some("1") {
        return None;
    }
    let fingerprint = info
        .get_property_val_str("fingerprint")?
        .to_ascii_lowercase();
    if fingerprint == local_fingerprint
        || validate_fingerprint(&fingerprint).is_err()
        || info.get_port() == 0
    {
        return None;
    }
    let mut ips: Vec<_> = info
        .get_addresses_v4()
        .into_iter()
        .filter(|ip| !ip.is_unspecified() && !ip.is_multicast() && !ip.is_loopback())
        .map(IpAddr::V4)
        .collect();
    ips.sort();
    ips.truncate(16);
    if ips.is_empty() {
        return None;
    }
    let name = info
        .get_property_val_str("name")
        .filter(|name| !name.is_empty())
        .unwrap_or("DeskUnify");
    Some(DiscoveredDevice {
        name: name.chars().take(128).collect(),
        ips,
        port: info.get_port(),
        fingerprint,
        platform: info
            .get_property_val_str("platform")
            .unwrap_or("unknown")
            .chars()
            .take(32)
            .collect(),
    })
}

pub(crate) fn validate_fingerprint(fingerprint: &str) -> Result<(), String> {
    let bytes: Vec<_> = fingerprint.split(':').collect();
    if bytes.len() == 32
        && bytes
            .iter()
            .all(|byte| byte.len() == 2 && byte.bytes().all(|ch| ch.is_ascii_hexdigit()))
    {
        Ok(())
    } else {
        Err("证书指纹应为 32 组十六进制字节，以冒号分隔".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info(fp: &str, version: &str, ip: &str) -> ResolvedService {
        ServiceInfo::new(
            SERVICE,
            "test",
            "test.local.",
            ip,
            4242,
            &[
                ("fingerprint", fp),
                ("name", "<remote>"),
                ("version", version),
            ][..],
        )
        .unwrap()
        .as_resolved_service()
    }
    #[tokio::test]
    #[ignore = "requires a multicast-capable network interface"]
    async fn scan_resolves_a_peer_and_updates_its_port() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let mut local = Discovery::new(["aa"; 32].join(":"));
        local.daemon = Some(ServiceDaemon::new_with_port(port).unwrap());
        local.configure(true, 4242);
        let remote = ServiceDaemon::new_with_port(port).unwrap();
        let fingerprint = ["bb"; 32].join(":");
        let properties = [
            ("fingerprint", fingerprint.as_str()),
            ("version", "1"),
            ("name", "Discovery test"),
        ];
        let make_info = |port| {
            ServiceInfo::new(
                SERVICE,
                "lanbridge-discovery-test",
                "lanbridge-discovery-test.local.",
                "",
                port,
                &properties[..],
            )
            .unwrap()
            .enable_addr_auto()
        };
        remote.register(make_info(4243)).unwrap();
        local.scan().unwrap();
        let wait = async |local: &mut Discovery, expected| {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if local
                        .devices()
                        .iter()
                        .any(|p| p.fingerprint == fingerprint && p.port == expected)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
        };
        wait(&mut local, 4243).await;
        remote.register(make_info(4343)).unwrap();
        wait(&mut local, 4343).await;
        local.stop();
        remote.shutdown().unwrap().recv_async().await.unwrap();
    }

    #[test]
    fn advertisements_are_validated_and_never_authorize_devices() {
        let fp = ["ab"; 32].join(":");
        let peer = parse(&info(&fp, "1", "192.168.1.2"), "local").unwrap();
        assert_eq!(peer.name, "<remote>");
        assert!(parse(&info(&fp, "1", "192.168.1.2"), &fp).is_none());
        assert!(parse(&info("123456", "1", "192.168.1.2"), "local").is_none());
        assert!(parse(&info(&fp, "2", "192.168.1.2"), "local").is_none());
        assert!(parse(&info(&fp, "1", "127.0.0.1"), "local").is_none());
    }
}
