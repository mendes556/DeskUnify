//! Effective per-device policy, keyed by certificate identity rather than IP.
use input_event::Event;
use lan_mouse_ipc::Sharing;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
pub(crate) type Policies = Arc<RwLock<HashMap<String, Sharing>>>;
pub(crate) fn allows(sharing: Sharing, event: Event) -> bool {
    match event {
        Event::Pointer(_) => sharing.mouse,
        Event::Keyboard(_) => sharing.keyboard,
    }
}
pub(crate) fn policy(policies: &Policies, fingerprint: &str) -> Sharing {
    policies
        .read()
        .ok()
        .and_then(|p| p.get(fingerprint).copied())
        .unwrap_or({
            #[cfg(any(target_os = "macos", windows))]
            {
                Sharing::OFF
            }
            #[cfg(not(any(target_os = "macos", windows)))]
            {
                Sharing::default()
            }
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_and_unknown_peers_fail_closed() {
        let p = Policies::default();
        #[cfg(any(target_os = "macos", windows))]
        assert_eq!(policy(&p, "unknown"), Sharing::OFF);
        #[cfg(not(any(target_os = "macos", windows)))]
        assert_eq!(policy(&p, "unknown"), Sharing::default());
        assert_eq!(Sharing::default().intersect(Sharing::OFF), Sharing::OFF);
        let a = Sharing {
            keyboard: false,
            ..Default::default()
        };
        let b = Sharing {
            files: false,
            ..Default::default()
        };
        let c = a.intersect(b);
        assert!(c.mouse && c.clipboard);
        assert!(!c.keyboard && !c.files);
    }
}
