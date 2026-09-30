// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use input_event::{Event, KeyboardEvent, PointerEvent};

pub use self::error::{EmulationCreationError, EmulationError, InputEmulationError};

#[cfg(windows)]
mod windows;

#[cfg(x11)]
mod x11;

#[cfg(wlroots)]
mod wlroots;

#[cfg(rdp)]
mod xdg_desktop_portal;

#[cfg(libei)]
mod libei;

#[cfg(target_os = "macos")]
mod macos;

/// Explicit test backend; also a fallback on platforms without native input.
mod dummy;
mod error;

pub type EmulationHandle = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(wlroots)]
    Wlroots,
    #[cfg(libei)]
    Libei,
    #[cfg(rdp)]
    Xdp,
    #[cfg(x11)]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(wlroots)]
            Backend::Wlroots => write!(f, "wlroots"),
            #[cfg(libei)]
            Backend::Libei => write!(f, "libei"),
            #[cfg(rdp)]
            Backend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(x11)]
            Backend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            Backend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            Backend::MacOs => write!(f, "macos"),
            Backend::Dummy => write!(f, "dummy"),
        }
    }
}

pub struct InputEmulation {
    backend: Backend,
    emulation: Box<dyn Emulation>,
    handles: HashSet<EmulationHandle>,
    pressed_keys: HashMap<EmulationHandle, HashSet<u32>>,
    pressed_buttons: HashMap<EmulationHandle, HashSet<u32>>,
}

impl InputEmulation {
    async fn with_backend(backend: Backend) -> Result<InputEmulation, EmulationCreationError> {
        let emulation: Box<dyn Emulation> = match backend {
            #[cfg(wlroots)]
            Backend::Wlroots => Box::new(wlroots::WlrootsEmulation::new()?),
            #[cfg(libei)]
            Backend::Libei => Box::new(libei::LibeiEmulation::new().await?),
            #[cfg(x11)]
            Backend::X11 => Box::new(x11::X11Emulation::new()?),
            #[cfg(rdp)]
            Backend::Xdp => Box::new(xdg_desktop_portal::DesktopPortalEmulation::new().await?),
            #[cfg(windows)]
            Backend::Windows => Box::new(windows::WindowsEmulation::new()?),
            #[cfg(target_os = "macos")]
            Backend::MacOs => Box::new(macos::MacOSEmulation::new()?),
            Backend::Dummy => Box::new(dummy::DummyEmulation::new()),
        };
        Ok(Self {
            backend,
            emulation,
            handles: HashSet::new(),
            pressed_keys: HashMap::new(),
            pressed_buttons: HashMap::new(),
        })
    }

    /// The backend actually selected, including an explicit dummy fallback.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub async fn new(backend: Option<Backend>) -> Result<InputEmulation, EmulationCreationError> {
        if let Some(backend) = backend {
            let b = Self::with_backend(backend).await;
            if b.is_ok() {
                log::info!("using emulation backend: {backend}");
            }
            return b;
        }

        let mut last_error = None;
        for backend in [
            #[cfg(wlroots)]
            Backend::Wlroots,
            #[cfg(libei)]
            Backend::Libei,
            #[cfg(rdp)]
            Backend::Xdp,
            #[cfg(x11)]
            Backend::X11,
            #[cfg(windows)]
            Backend::Windows,
            #[cfg(target_os = "macos")]
            Backend::MacOs,
            // Native permission failures must remain visible to the frontend.
            #[cfg(not(any(target_os = "macos", windows)))]
            Backend::Dummy,
        ] {
            match Self::with_backend(backend).await {
                Ok(b) => {
                    log::info!("using emulation backend: {backend}");
                    return Ok(b);
                }
                Err(e) if e.cancelled_by_user() => return Err(e),
                Err(e) => {
                    log::warn!("{backend} input emulation backend unavailable: {e}");
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or(EmulationCreationError::NoAvailableBackend))
    }

    pub async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        match event {
            Event::Keyboard(KeyboardEvent::Key { key, state, .. }) => {
                // prevent double pressed / released keys
                if self.update_pressed_keys(handle, key, state) {
                    self.emulation.consume(event, handle).await?;
                }
                Ok(())
            }
            Event::Pointer(PointerEvent::Button { button, state, .. }) => {
                let Some(buttons) = self.pressed_buttons.get_mut(&handle) else {
                    return Ok(());
                };
                let changed = if state == 0 {
                    buttons.remove(&button)
                } else {
                    buttons.insert(button)
                };
                if changed {
                    self.emulation.consume(event, handle).await?;
                }
                Ok(())
            }
            _ => self.emulation.consume(event, handle).await,
        }
    }

    pub async fn create(&mut self, handle: EmulationHandle) -> bool {
        if self.handles.insert(handle) {
            self.pressed_keys.insert(handle, HashSet::new());
            self.pressed_buttons.insert(handle, HashSet::new());
            self.emulation.create(handle).await;
            true
        } else {
            false
        }
    }

    pub async fn destroy(&mut self, handle: EmulationHandle) {
        let _ = self.release_keys(handle).await;
        if self.handles.remove(&handle) {
            self.pressed_keys.remove(&handle);
            self.pressed_buttons.remove(&handle);
            self.emulation.destroy(handle).await
        }
    }

    pub async fn terminate(&mut self) {
        for handle in self.handles.iter().cloned().collect::<Vec<_>>() {
            self.destroy(handle).await
        }
        self.emulation.terminate().await
    }

    pub async fn release_keys(&mut self, handle: EmulationHandle) -> Result<(), EmulationError> {
        if let Some(keys) = self.pressed_keys.get_mut(&handle) {
            let keys = keys.drain().collect::<Vec<_>>();
            for key in keys {
                let event = Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key,
                    state: 0,
                });
                self.emulation.consume(event, handle).await?;
                if let Ok(key) = input_event::scancode::Linux::try_from(key) {
                    log::warn!("releasing stuck key: {key:?}");
                }
            }
        }

        if let Some(buttons) = self.pressed_buttons.get_mut(&handle) {
            for button in buttons.drain().collect::<Vec<_>>() {
                self.emulation
                    .consume(
                        Event::Pointer(PointerEvent::Button {
                            time: 0,
                            button,
                            state: 0,
                        }),
                        handle,
                    )
                    .await?;
            }
        }
        let event = Event::Keyboard(KeyboardEvent::Modifiers {
            depressed: 0,
            latched: 0,
            locked: 0,
            group: 0,
        });
        self.emulation.consume(event, handle).await?;
        Ok(())
    }

    pub fn has_pressed_keys(&self, handle: EmulationHandle) -> bool {
        self.pressed_keys
            .get(&handle)
            .is_some_and(|p| !p.is_empty())
    }

    /// update the pressed_keys for the given handle
    /// returns whether the event should be processed
    fn update_pressed_keys(&mut self, handle: EmulationHandle, key: u32, state: u8) -> bool {
        let Some(pressed_keys) = self.pressed_keys.get_mut(&handle) else {
            return false;
        };

        if state == 0 {
            // currently pressed => can release
            pressed_keys.remove(&key)
        } else {
            // currently not pressed => can press
            pressed_keys.insert(key)
        }
    }
}

#[async_trait]
trait Emulation: Send {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError>;
    async fn create(&mut self, handle: EmulationHandle);
    async fn destroy(&mut self, handle: EmulationHandle);
    async fn terminate(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, PartialEq)]
    enum Recorded {
        Input(EmulationHandle, Event),
        Destroy(EmulationHandle),
        Terminate,
    }

    struct RecordingEmulation(Arc<Mutex<Vec<Recorded>>>);

    #[async_trait]
    impl Emulation for RecordingEmulation {
        async fn consume(
            &mut self,
            event: Event,
            handle: EmulationHandle,
        ) -> Result<(), EmulationError> {
            self.0.lock().unwrap().push(Recorded::Input(handle, event));
            Ok(())
        }

        async fn create(&mut self, _: EmulationHandle) {}

        async fn destroy(&mut self, handle: EmulationHandle) {
            self.0.lock().unwrap().push(Recorded::Destroy(handle));
        }

        async fn terminate(&mut self) {
            self.0.lock().unwrap().push(Recorded::Terminate);
        }
    }

    fn key_event(key: u32, state: u8) -> Event {
        Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key,
            state,
        })
    }

    #[cfg(any(target_os = "macos", windows))]
    #[tokio::test]
    async fn automatic_native_selection_does_not_hide_failure_with_dummy() {
        // Native constructors only preflight permissions; no input is replayed.
        match InputEmulation::new(None).await {
            Ok(mut emulation) => {
                assert_ne!(emulation.backend(), Backend::Dummy);
                emulation.terminate().await;
            }
            #[cfg(target_os = "macos")]
            Err(EmulationCreationError::MacOs(_)) => {}
            #[cfg(windows)]
            Err(EmulationCreationError::Windows(_)) => {}
            Err(error) => panic!("native backend error was lost: {error}"),
        }
        let mut dummy = InputEmulation::new(Some(Backend::Dummy)).await.unwrap();
        assert_eq!(dummy.backend(), Backend::Dummy);
        dummy.terminate().await;
    }

    #[tokio::test]
    async fn disconnect_and_termination_release_keys_before_destroying_handles() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut emulation = InputEmulation {
            backend: Backend::Dummy,
            emulation: Box::new(RecordingEmulation(recorded.clone())),
            handles: HashSet::new(),
            pressed_keys: HashMap::new(),
            pressed_buttons: HashMap::new(),
        };
        let ctrl = input_event::scancode::Linux::KeyLeftCtrl as u32;
        let shift = input_event::scancode::Linux::KeyLeftShift as u32;
        emulation.create(1).await;
        emulation.create(2).await;
        emulation.consume(key_event(ctrl, 1), 1).await.unwrap();
        let mouse = |state| {
            Event::Pointer(PointerEvent::Button {
                time: 0,
                button: input_event::BTN_LEFT,
                state,
            })
        };
        emulation.consume(mouse(1), 1).await.unwrap();
        emulation.consume(key_event(shift, 1), 2).await.unwrap();
        recorded.lock().unwrap().clear();

        // One disconnect must release only that client's keys.
        emulation.destroy(1).await;
        assert!(!emulation.has_pressed_keys(1));
        assert!(emulation.has_pressed_keys(2));
        emulation.terminate().await;
        assert!(!emulation.has_pressed_keys(2));
        assert!(emulation.handles.is_empty());

        let reset_modifiers = Event::Keyboard(KeyboardEvent::Modifiers {
            depressed: 0,
            latched: 0,
            locked: 0,
            group: 0,
        });
        assert_eq!(
            *recorded.lock().unwrap(),
            vec![
                Recorded::Input(1, key_event(ctrl, 0)),
                Recorded::Input(1, mouse(0)),
                Recorded::Input(1, reset_modifiers),
                Recorded::Destroy(1),
                Recorded::Input(2, key_event(shift, 0)),
                Recorded::Input(2, reset_modifiers),
                Recorded::Destroy(2),
                Recorded::Terminate,
            ]
        );
    }
}
