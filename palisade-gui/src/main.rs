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
            .with_app_id("palisade")
            .with_inner_size([460.0, 560.0])
            .with_min_inner_size([400.0, 480.0])
            .with_icon(load_app_icon()),
        ..Default::default()
    };

    eframe::run_native("Palisade", options, Box::new(|_cc| Ok(Box::new(app))))
}

/// Decode the app icon (PNG) into egui's RGBA IconData.
fn load_app_icon() -> egui::IconData {
    const PNG: &[u8] = include_bytes!("../../assets/palisade-icon.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(PNG));
    let Ok(mut reader) = decoder.read_info() else {
        return egui::IconData::default();
    };
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let Ok(info) = reader.next_frame(&mut buf) else {
        return egui::IconData::default();
    };
    let rgba = buf[..info.buffer_size()].to_vec();
    let w = info.width as usize;
    let h = info.height as usize;
    // egui wants a square icon; letterbox to 256x256 preserving aspect ratio.
    let size = 256usize;
    let scale = (size as f32 / w as f32).min(size as f32 / h as f32);
    let nw = (w as f32 * scale).round() as usize;
    let nh = (h as f32 * scale).round() as usize;
    let ox = (size - nw) / 2;
    let oy = (size - nh) / 2;
    let mut out = vec![0u8; size * size * 4];
    for y in 0..nh {
        for x in 0..nw {
            let sx = (x as f32 / scale).round().clamp(0.0, (w - 1) as f32) as usize;
            let sy = (y as f32 / scale).round().clamp(0.0, (h - 1) as f32) as usize;
            let si = (sy * w + sx) * 4;
            let di = ((y + oy) * size + (x + ox)) * 4;
            out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
        }
    }
    egui::IconData {
        rgba: out,
        width: size as u32,
        height: size as u32,
    }
}
