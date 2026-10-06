//! Automatic pairing is currently available on macOS and Windows.
pub(crate) struct Control;
pub(crate) enum Event {}
impl Control {
    pub async fn event(&mut self) -> Event {
        std::future::pending().await
    }
}
