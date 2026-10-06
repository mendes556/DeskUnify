use super::error::{EmulationError, WindowsEmulationCreationError};
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
    scancode,
};

use async_trait::async_trait;
use std::ops::BitOrAssign;
use std::time::Duration;
use tokio::task::AbortHandle;
use windows::Win32::Foundation::{ERROR_SUCCESS, GetLastError, SetLastError};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT_0, KEYEVENTF_EXTENDEDKEY, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{XBUTTON1, XBUTTON2};

use super::{Emulation, EmulationHandle};

const DEFAULT_REPEAT_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_REPEAT_INTERVAL: Duration = Duration::from_millis(32);

pub(crate) struct WindowsEmulation {
    repeat_task: Option<AbortHandle>,
    pending_releases: Vec<PendingRelease>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingRelease {
    Key(u32),
    Button(u32),
}

impl PendingRelease {
    fn from_event(event: &Event) -> Option<Self> {
        match event {
            Event::Keyboard(KeyboardEvent::Key { key, state: 0, .. }) => Some(Self::Key(*key)),
            Event::Pointer(PointerEvent::Button {
                button, state: 0, ..
            }) => Some(Self::Button(*button)),
            _ => None,
        }
    }

    fn send(self) -> Result<(), EmulationError> {
        match self {
            Self::Key(key) => key_event(key, 0),
            Self::Button(button) => mouse_button(button, 0),
        }
    }
}

fn flush_releases(
    pending: &mut Vec<PendingRelease>,
    mut send: impl FnMut(PendingRelease) -> Result<(), EmulationError>,
) -> Result<(), EmulationError> {
    while let Some(&release) = pending.last() {
        send(release)?;
        pending.pop();
    }
    Ok(())
}

impl WindowsEmulation {
    pub(crate) fn new() -> Result<Self, WindowsEmulationCreationError> {
        Ok(Self {
            repeat_task: None,
            pending_releases: Vec::new(),
        })
    }
}

#[async_trait]
impl Emulation for WindowsEmulation {
    async fn consume(&mut self, event: Event, _: EmulationHandle) -> Result<(), EmulationError> {
        let release = PendingRelease::from_event(&event);
        let result = flush_releases(&mut self.pending_releases, PendingRelease::send)
            .and_then(|()| self.consume_event(event));
        if result.is_err() {
            self.kill_repeat_task();
            // A blocked key/button up must still be delivered when input is permitted again.
            // Never queue presses or motion for replay into a different foreground window.
            if let Some(release) =
                release.filter(|release| !self.pending_releases.contains(release))
            {
                self.pending_releases.push(release);
            }
        }
        result
    }

    async fn create(&mut self, _handle: EmulationHandle) {}

    async fn destroy(&mut self, _handle: EmulationHandle) {
        self.kill_repeat_task();
        if let Err(error) = flush_releases(&mut self.pending_releases, PendingRelease::send) {
            log::warn!("releasing input on disconnect: {error}");
        }
    }

    async fn terminate(&mut self) {
        self.destroy(0).await;
    }
}

impl WindowsEmulation {
    fn consume_event(&mut self, event: Event) -> Result<(), EmulationError> {
        match event {
            Event::Pointer(pointer_event) => match pointer_event {
                PointerEvent::Motion { time: _, dx, dy } => rel_mouse(dx as i32, dy as i32),
                PointerEvent::Button {
                    time: _,
                    button,
                    state,
                } => mouse_button(button, state),
                PointerEvent::Axis {
                    time: _,
                    axis,
                    value,
                } => scroll(axis, value as i32),
                PointerEvent::AxisDiscrete120 { axis, value } => scroll(axis, value),
            },
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    self.kill_repeat_task();
                    key_event(key, state)?;
                    if state == 1 {
                        self.spawn_repeat_task(key);
                    }
                    Ok(())
                }
                KeyboardEvent::Modifiers { .. } => Ok(()),
            },
        }
    }

    fn spawn_repeat_task(&mut self, key: u32) {
        // there can only be one repeating key and it's
        // always the last to be pressed
        self.kill_repeat_task();
        let repeat_task = tokio::task::spawn_local(async move {
            tokio::time::sleep(DEFAULT_REPEAT_DELAY).await;
            loop {
                if let Err(error) = key_event(key, 1) {
                    log::warn!("stopping key repeat: {error}");
                    break;
                }
                tokio::time::sleep(DEFAULT_REPEAT_INTERVAL).await;
            }
        });
        self.repeat_task = Some(repeat_task.abort_handle());
    }
    fn kill_repeat_task(&mut self) {
        if let Some(task) = self.repeat_task.take() {
            task.abort();
        }
    }
}

fn send_input_safe(input: INPUT) -> Result<(), EmulationError> {
    send_input_with(input, |inputs, size| unsafe { SendInput(inputs, size) })
}

fn send_input_with(
    input: INPUT,
    send: impl FnOnce(&[INPUT], i32) -> u32,
) -> Result<(), EmulationError> {
    // UIPI can reject input without setting an error. Never retry synchronously:
    // this backend shares the daemon's single-threaded async runtime.
    unsafe { SetLastError(ERROR_SUCCESS) };
    if send(&[input], std::mem::size_of::<INPUT>() as i32) == 1 {
        Ok(())
    } else {
        Err(EmulationError::WindowsInputBlocked {
            code: unsafe { GetLastError() }.0,
        })
    }
}

fn send_mouse_input(mi: MOUSEINPUT) -> Result<(), EmulationError> {
    send_input_safe(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi },
    })
}

fn send_keyboard_input(ki: KEYBDINPUT) -> Result<(), EmulationError> {
    send_input_safe(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    })
}
fn rel_mouse(dx: i32, dy: i32) -> Result<(), EmulationError> {
    let mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi)
}

fn mouse_button(button: u32, state: u32) -> Result<(), EmulationError> {
    let dw_flags = match state {
        0 => match button {
            BTN_LEFT => MOUSEEVENTF_LEFTUP,
            BTN_RIGHT => MOUSEEVENTF_RIGHTUP,
            BTN_MIDDLE => MOUSEEVENTF_MIDDLEUP,
            BTN_BACK => MOUSEEVENTF_XUP,
            BTN_FORWARD => MOUSEEVENTF_XUP,
            _ => return Ok(()),
        },
        1 => match button {
            BTN_LEFT => MOUSEEVENTF_LEFTDOWN,
            BTN_RIGHT => MOUSEEVENTF_RIGHTDOWN,
            BTN_MIDDLE => MOUSEEVENTF_MIDDLEDOWN,
            BTN_BACK => MOUSEEVENTF_XDOWN,
            BTN_FORWARD => MOUSEEVENTF_XDOWN,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    let mouse_data = match button {
        BTN_BACK => XBUTTON1 as u32,
        BTN_FORWARD => XBUTTON2 as u32,
        _ => 0,
    };
    let mi = MOUSEINPUT {
        dx: 0,
        dy: 0, // no movement
        mouseData: mouse_data,
        dwFlags: dw_flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi)
}

fn scroll(axis: u8, value: i32) -> Result<(), EmulationError> {
    let event_type = match axis {
        0 => MOUSEEVENTF_WHEEL,
        1 => MOUSEEVENTF_HWHEEL,
        _ => return Ok(()),
    };
    let mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: -value as u32,
        dwFlags: event_type,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi)
}

fn key_event(key: u32, state: u8) -> Result<(), EmulationError> {
    let scancode = match linux_keycode_to_windows_scancode(key) {
        Some(code) => code,
        None => return Ok(()),
    };
    let extended = scancode > 0xff;
    let scancode = scancode & 0xff;
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags.bitor_assign(KEYEVENTF_EXTENDEDKEY);
    }
    if state == 0 {
        flags.bitor_assign(KEYEVENTF_KEYUP);
    }
    let ki = KEYBDINPUT {
        wVk: Default::default(),
        wScan: scancode,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_keyboard_input(ki)
}

fn linux_keycode_to_windows_scancode(linux_keycode: u32) -> Option<u16> {
    let linux_scancode = match scancode::Linux::try_from(linux_keycode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("unknown keycode: {linux_keycode}");
            return None;
        }
    };
    log::trace!("linux code: {linux_scancode:?}");
    let windows_scancode = match scancode::Windows::try_from(linux_scancode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("failed to translate linux code into windows scancode: {linux_scancode:?}");
            return None;
        }
    };
    log::trace!("windows code: {windows_scancode:?}");
    Some(windows_scancode as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_input_is_attempted_once_even_without_a_win32_error() {
        let mut attempts = 0;
        let result = send_input_with(INPUT::default(), |_, _| {
            attempts += 1;
            0
        });
        assert_eq!(attempts, 1);
        assert!(matches!(
            result,
            Err(EmulationError::WindowsInputBlocked { code: 0 })
        ));
    }

    #[test]
    fn invalid_native_input_returns_an_error_without_retrying() {
        // Invalid structure size: SendInput must reject this without injecting events.
        let result = send_input_with(INPUT::default(), |inputs, _| unsafe {
            SendInput(inputs, 0)
        });
        assert!(matches!(
            result,
            Err(EmulationError::WindowsInputBlocked { .. })
        ));
    }

    #[test]
    fn accepted_input_uses_the_native_structure_size() {
        assert!(
            send_input_with(INPUT::default(), |inputs, size| {
                assert_eq!(inputs.len(), 1);
                assert_eq!(size, std::mem::size_of::<INPUT>() as i32);
                1
            })
            .is_ok()
        );
    }

    #[test]
    fn blocked_releases_are_delivered_before_input_resumes() {
        let mut pending = vec![
            PendingRelease::Key(scancode::Linux::KeyLeftCtrl as u32),
            PendingRelease::Button(BTN_LEFT),
        ];
        let result = flush_releases(&mut pending, |_| {
            Err(EmulationError::WindowsInputBlocked { code: 0 })
        });
        assert!(result.is_err());
        assert_eq!(pending.len(), 2);

        let mut delivered = Vec::new();
        flush_releases(&mut pending, |release| {
            delivered.push(release);
            Ok(())
        })
        .unwrap();
        assert!(pending.is_empty());
        assert!(delivered.contains(&PendingRelease::Key(scancode::Linux::KeyLeftCtrl as u32)));
        assert!(delivered.contains(&PendingRelease::Button(BTN_LEFT)));
        let press = Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key: scancode::Linux::KeyA as u32,
            state: 1,
        });
        assert!(PendingRelease::from_event(&press).is_none());
    }

    #[tokio::test]
    async fn destroying_or_terminating_emulation_cancels_key_repeat() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut emulation = WindowsEmulation::new().unwrap();
                for terminate in [false, true] {
                    emulation.spawn_repeat_task(scancode::Linux::KeyA as u32);
                    let task = emulation.repeat_task.as_ref().unwrap().clone();
                    if terminate {
                        emulation.terminate().await;
                    } else {
                        emulation.destroy(0).await;
                    }
                    tokio::task::yield_now().await;
                    assert!(emulation.repeat_task.is_none());
                    assert!(task.is_finished());
                }
            })
            .await;
    }
}
