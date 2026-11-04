mod app;
mod autostart;
mod single_instance;
mod tray;

use palisade_core::{AppConfig, ConnEvent, Session};
use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // One GUI at a time: a new start replaces a stale one.
    let _instance = single_instance::claim();

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<ConnEvent>();
    let session = match Session::spawn(event_tx) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to start VPN session worker: {e}");
            std::process::exit(1);
        }
    };

    let config = AppConfig::load();
    let app = app::GpConnect::new(config, session, event_rx);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Palisade")
            .with_inner_size([460.0, 560.0])
            .with_min_inner_size([400.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native("Palisade", options, Box::new(|_cc| Ok(Box::new(app))))
}
