// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use crate::config::Config;
use clap::Args;
use input_emulation::{EmulationError, InputEmulation, InputEmulationError};
use input_event::{BTN_LEFT, Event, KeyboardEvent, PointerEvent, scancode::Linux};
use std::time::Duration;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[command(group(clap::ArgGroup::new("events").args(["mouse", "keyboard", "scroll"]).required(true).multiple(true)))]
pub struct TestEmulationArgs {
    #[arg(long)]
    mouse: bool,
    /// Type one A key in the focused application
    #[arg(long)]
    keyboard: bool,
    #[arg(long)]
    scroll: bool,
    #[arg(long, default_value_t=5, value_parser=clap::value_parser!(u64).range(1..=300))]
    seconds: u64,
}

pub async fn run(config: Config, args: TestEmulationArgs) -> Result<(), InputEmulationError> {
    let backend = config.emulation_backend().map(|b| b.into());
    let mut emulation = InputEmulation::new(backend).await?;
    emulation.create(0).await;
    log::info!(
        "testing {} emulation for {} seconds",
        emulation.backend(),
        args.seconds
    );
    let result = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut index = 0;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                result = tokio::signal::ctrl_c() => { result.map_err(EmulationError::Io)?; break; }
                _ = tick.tick() => {
                    for event in events(&args, index) {
                        emulation.consume(event, 0).await?;
                    }
                    index += 1;
                }
            }
        }
        Ok::<(), EmulationError>(())
    }
    .await;
    // Cleanup also runs after a failed consume or Ctrl+C.
    emulation.terminate().await;
    result?;
    Ok(())
}

fn events(args: &TestEmulationArgs, index: usize) -> Vec<Event> {
    let mut events = Vec::new();
    if args.mouse {
        events.push(Event::Pointer(PointerEvent::Motion {
            time: 0,
            dx: if index % 20 < 10 { 1. } else { -1. },
            dy: 0.,
        }));
    }
    if args.keyboard && index == 0 {
        for state in [1, 0] {
            events.push(Event::Keyboard(KeyboardEvent::Key {
                time: 0,
                key: Linux::KeyA as u32,
                state,
            }));
        }
    }
    if args.mouse && index == 0 {
        for state in [1, 0] {
            events.push(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_LEFT,
                state,
            }));
        }
    }
    if args.scroll && index.is_multiple_of(10) {
        events.push(Event::Pointer(PointerEvent::AxisDiscrete120 {
            axis: 0,
            value: if index.is_multiple_of(20) { 120 } else { -120 },
        }));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyboard_flag_emits_only_one_balanced_key_pair() {
        let args = TestEmulationArgs {
            mouse: false,
            keyboard: true,
            scroll: false,
            seconds: 1,
        };
        assert!(matches!(
            events(&args, 0).as_slice(),
            [
                Event::Keyboard(KeyboardEvent::Key { state: 1, .. }),
                Event::Keyboard(KeyboardEvent::Key { state: 0, .. })
            ]
        ));
        assert!(events(&args, 1).is_empty());
    }
    #[test]
    fn scroll_flag_does_not_move_click_or_type() {
        let args = TestEmulationArgs {
            mouse: false,
            keyboard: false,
            scroll: true,
            seconds: 1,
        };
        assert!(matches!(
            events(&args, 0).as_slice(),
            [Event::Pointer(PointerEvent::AxisDiscrete120 { .. })]
        ));
    }
}
