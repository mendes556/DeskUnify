// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use crate::config::Config;
use clap::Args;
use futures::StreamExt;
use input_capture::{CaptureError, CaptureEvent, InputCapture, InputCaptureError, Position};
use input_event::{Event, KeyboardEvent, scancode::Linux};
use std::time::Duration;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TestCaptureArgs {
    #[arg(long, default_value_t=15, value_parser=clap::value_parser!(u64).range(1..=300))]
    seconds: u64,
}

pub async fn run(config: Config, args: TestCaptureArgs) -> Result<(), InputCaptureError> {
    let backend = config.capture_backend().map(|b| b.into());
    let mut capture = InputCapture::new(backend).await?;
    log::info!(
        "testing {} capture for {} seconds; Escape or Ctrl+C exits",
        capture.backend(),
        args.seconds
    );
    let result = async {
        for (id, position) in [Position::Left, Position::Right, Position::Top, Position::Bottom].into_iter().enumerate() {
            capture.create(id as u64, position).await?;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                result = tokio::signal::ctrl_c() => { result.map_err(CaptureError::Io)?; break; }
                event = capture.next() => {
                    let (client, event) = event.ok_or(CaptureError::EndOfStream)??;
                    log::info!("capture {client}: {event}");
                    if matches!(event, CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key { key, state: 1, .. })) if key == Linux::KeyEsc as u32) {
                        break;
                    }
                }
            }
        }
        Ok::<(), CaptureError>(())
    }.await;
    let released = capture.release().await;
    let terminated = capture.terminate().await;
    result?;
    released?;
    terminated?;
    Ok(())
}
