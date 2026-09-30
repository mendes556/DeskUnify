// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use std::f64::consts::PI;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use input_event::{BTN_LEFT, Event, KeyboardEvent, PointerEvent, scancode::Linux};
use tokio::time::{self, Instant, Interval};

use super::{Capture, CaptureError, CaptureEvent, Position};

pub struct DummyInputCapture {
    start: Option<Instant>,
    interval: Interval,
    offset: (i32, i32),
    left_enabled: bool,
    test_events: bool,
    test_index: usize,
}

impl DummyInputCapture {
    pub fn new() -> Self {
        Self {
            start: None,
            interval: time::interval(Duration::from_millis(
                if std::env::var("LAN_MOUSE_DUMMY_TEST_EVENTS").as_deref() == Ok("1") {
                    100
                } else {
                    1
                },
            )),
            offset: (0, 0),
            left_enabled: false,
            test_events: std::env::var("LAN_MOUSE_DUMMY_TEST_EVENTS").as_deref() == Ok("1"),
            test_index: 0,
        }
    }
}

impl Default for DummyInputCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Capture for DummyInputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        if pos == Position::Left {
            self.left_enabled = true;
            self.start = None;
            self.test_index = 0;
        }
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        if pos == Position::Left {
            self.left_enabled = false;
        }
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        self.start = None;
        self.test_index = 0;
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

const FREQUENCY_HZ: f64 = 1.0;
const RADIUS: f64 = 100.0;

impl Stream for DummyInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let current = ready!(self.interval.poll_tick(cx));
        if !self.left_enabled {
            // Register the next tick even when there is no configured barrier.
            // Otherwise adding a client later could leave this stream asleep.
            let _ = self.interval.poll_tick(cx);
            return Poll::Pending;
        }
        let event = match self.start {
            None => {
                self.start.replace(current);
                CaptureEvent::Begin
            }
            Some(_) if self.test_events => {
                let event = test_event(self.test_index);
                self.test_index += 1;
                CaptureEvent::Input(event)
            }
            Some(start) => {
                let elapsed = start.elapsed();
                let elapsed_sec_f64 = elapsed.as_secs_f64();
                let second_fraction = elapsed_sec_f64 - elapsed_sec_f64 as u64 as f64;
                let radians = second_fraction * 2. * PI * FREQUENCY_HZ;
                let offset = (radians.cos() * RADIUS * 2., (radians * 2.).sin() * RADIUS);
                let offset = (offset.0 as i32, offset.1 as i32);
                let relative_motion = (offset.0 - self.offset.0, offset.1 - self.offset.1);
                self.offset = offset;
                let (dx, dy) = (relative_motion.0 as f64, relative_motion.1 as f64);
                CaptureEvent::Input(input_event::Event::Pointer(PointerEvent::Motion {
                    time: 0,
                    dx,
                    dy,
                }))
            }
        };
        Poll::Ready(Some(Ok((Position::Left, event))))
    }
}

// Opt-in fixtures exercise serialization and transport using dummy emulation.
// Every held key/button has a matching release within a single cycle.
fn test_event(index: usize) -> Event {
    let key = |key: Linux, state| {
        Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key: key as u32,
            state,
        })
    };
    match index % 9 {
        0 => Event::Pointer(PointerEvent::Motion {
            time: 0,
            dx: 2.,
            dy: -1.,
        }),
        1 => key(Linux::KeyLeftCtrl, 1),
        2 => key(Linux::KeyA, 1),
        3 => key(Linux::KeyA, 0),
        4 => key(Linux::KeyLeftCtrl, 0),
        5 | 6 => Event::Pointer(PointerEvent::Button {
            time: 0,
            button: BTN_LEFT,
            state: u32::from(index % 9 == 5),
        }),
        7 => Event::Pointer(PointerEvent::Axis {
            time: 0,
            axis: 0,
            value: -1.,
        }),
        _ => Event::Pointer(PointerEvent::AxisDiscrete120 {
            axis: 1,
            value: 120,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn adding_and_reactivating_left_client_emit_begin() {
        let mut capture = DummyInputCapture::new();
        capture.create(Position::Left).await.unwrap();
        assert_eq!(
            capture.next().await.unwrap().unwrap().1,
            CaptureEvent::Begin
        );
        capture.destroy(Position::Left).await.unwrap();
        capture.create(Position::Left).await.unwrap();
        assert_eq!(
            capture.next().await.unwrap().unwrap().1,
            CaptureEvent::Begin
        );
    }
}
