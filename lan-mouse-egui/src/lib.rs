// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
mod app;
mod privacy;
mod transport;
use eframe::egui;
use thiserror::Error;

pub(crate) const APP_ICON_PNG: &[u8] = include_bytes!("../icons/icon.png");

#[derive(Debug, Error)]
pub enum EguiError {
    #[error("desktop UI: {0}")]
    Native(#[from] eframe::Error),
    #[error("daemon shutdown: {0}")]
    Shutdown(String),
}

pub fn run(owns_daemon: bool) -> Result<(), EguiError> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("DeskUnify")
        .with_app_id("dev.lanbridge.desktop")
        .with_inner_size([960.0, 640.0])
        .with_min_inner_size([800.0, 520.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(APP_ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "DeskUnify",
        options,
        Box::new(move |cc| Ok(Box::new(app::BridgeApp::new(cc, owns_daemon)?))),
    )?;
    Ok(())
}

pub async fn shutdown() -> Result<(), EguiError> {
    transport::execute(lan_mouse_ipc::UiAction::Shutdown)
        .await
        .map(|_| ())
        .map_err(EguiError::Shutdown)
}
