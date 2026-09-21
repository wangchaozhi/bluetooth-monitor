mod app;
mod ble;
mod capture;
mod codec;
mod plotting;
pub mod plugin;
mod profile;
mod protocol;
mod protocol_export;
mod protocol_preset;
mod replay;
mod workspace;

use app::BluetoothMonitorApp;
use ble::model::{BleCommand, BleEvent};
use eframe::egui;
use std::sync::mpsc;
use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
    init_tracing();

    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let shutdown_tx = command_tx.clone();
    let (event_tx, event_rx) = mpsc::channel();

    let worker_thread = std::thread::Builder::new()
        .name("bluetooth-monitor-runtime".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("bluetooth-monitor-worker")
                .build()
                .expect("failed to create Tokio runtime");

            let fatal_tx = event_tx.clone();
            if let Err(error) = runtime.block_on(ble::worker::run(command_rx, event_tx)) {
                let _ = fatal_tx.send(BleEvent::Fatal(format!("{error:#}")));
            }
        })
        .expect("failed to spawn Bluetooth worker thread");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Bluetooth Monitor")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([960.0, 640.0]),
        centered: true,
        ..Default::default()
    };

    let result = eframe::run_native(
        "Bluetooth Monitor",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(BluetoothMonitorApp::new(
                cc,
                command_tx,
                event_rx,
            )))
        }),
    );

    let _ = shutdown_tx.send(BleCommand::Shutdown);
    let _ = worker_thread.join();

    result
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("bluetooth_monitor=info,btleplug=warn"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init();
}
