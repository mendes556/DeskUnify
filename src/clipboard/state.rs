// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use sha2::{Digest, Sha256};

pub(super) const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub(super) type Node = [u8; 32];

pub(super) fn node(cert: &[u8]) -> Node {
    Sha256::digest(cert).into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct Revision {
    pub counter: u64,
    pub origin: Node,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Update {
    pub revision: Revision,
    pub text: String,
}

pub(super) struct State {
    origin: Node,
    counter: u64,
    current: Option<Revision>,
    observed: Option<String>,
    initialized: bool,
}

impl State {
    pub fn new(origin: Node, counter: u64) -> Self {
        Self {
            origin,
            counter,
            current: None,
            observed: None,
            initialized: false,
        }
    }

    /// The startup clipboard is a baseline, never an unsolicited transfer.
    pub fn observe(&mut self, text: Option<String>) -> Option<Update> {
        if !self.initialized {
            self.initialized = true;
            self.observed = text;
            return None;
        }
        if self.observed == text {
            return None;
        }
        self.observed = text;
        let text = self.observed.as_ref()?;
        if text.len() > MAX_TEXT_BYTES {
            return None;
        }
        self.counter = self.counter.checked_add(1)?;
        let revision = Revision {
            counter: self.counter,
            origin: self.origin,
        };
        self.current = Some(revision);
        Some(Update {
            revision,
            text: text.clone(),
        })
    }

    pub fn accepts(&mut self, update: &Update) -> bool {
        self.counter = self.counter.max(update.revision.counter);
        self.current
            .is_none_or(|revision| update.revision > revision)
    }

    /// Commit only after the OS write succeeds, so failed writes can retry.
    pub fn applied(&mut self, update: &Update) {
        self.current = Some(update.revision);
        self.observed = Some(update.text.clone());
        self.initialized = true;
    }

    pub fn is_current(&self, update: &Update) -> bool {
        self.current == Some(update.revision) && self.observed.as_ref() == Some(&update.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_write_does_not_loop_and_local_copy_advances_clock() {
        let mut state = State::new([1; 32], 0);
        assert!(state.observe(Some("startup".into())).is_none());
        let remote = Update {
            revision: Revision {
                counter: 10,
                origin: [2; 32],
            },
            text: "中文\nhello 😀".into(),
        };
        assert!(state.accepts(&remote));
        state.applied(&remote);
        assert!(state.observe(Some(remote.text.clone())).is_none());
        assert!(!state.accepts(&remote));
        let local = state.observe(Some("next".into())).unwrap();
        assert_eq!(local.revision.counter, 11);
        assert!(!state.accepts(&remote));
    }

    #[test]
    fn simultaneous_copies_converge_in_either_delivery_order() {
        let mut a = State::new([1; 32], 0);
        let mut b = State::new([2; 32], 0);
        a.observe(None);
        b.observe(None);
        let from_a = a.observe(Some("a".into())).unwrap();
        let from_b = b.observe(Some("b".into())).unwrap();
        assert!(a.accepts(&from_b));
        a.applied(&from_b);
        assert!(!b.accepts(&from_a));
        assert!(!a.accepts(&from_a));
        assert_eq!(a.current, b.current);
        assert_eq!(a.observed, b.observed);
    }

    #[test]
    fn nontext_and_oversized_text_are_not_sent() {
        let mut state = State::new([1; 32], 0);
        state.observe(None);
        assert!(
            state
                .observe(Some("x".repeat(MAX_TEXT_BYTES + 1)))
                .is_none()
        );
        assert!(state.observe(None).is_none());
        assert!(state.observe(Some(String::new())).is_some());
    }

    #[test]
    fn failed_write_can_retry_same_revision() {
        let mut state = State::new([1; 32], 0);
        let remote = Update {
            revision: Revision {
                counter: 1,
                origin: [2; 32],
            },
            text: "retry".into(),
        };
        assert!(state.accepts(&remote));
        assert!(state.accepts(&remote));
        state.applied(&remote);
        assert!(!state.accepts(&remote));
    }
}
