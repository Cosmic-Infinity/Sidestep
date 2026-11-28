//! Linux Native GUI front-end using eframe (egui) and ksni (FreeDesktop System Tray).
//! Faithfully matches the Windows 4-tab interface, colors, icons, and honesty contract.

pub mod app;
pub mod icons;
pub mod ipc;
pub mod state;
pub mod tray;
pub mod views;

pub use app::SidestepApp;
pub use ipc::send_instance_cmd;

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::Config;
use crate::setup::Mode;
use state::AppState;

/// Main GUI entrypoint.
pub fn run_gui(mode: Mode, cfg: Config, exe: PathBuf) {
    let (cmd_tx, cmd_rx) = channel();
    let state = Arc::new(Mutex::new(AppState::new(mode, cfg)));

    // Check single-instance
    if let Some(resp) = ipc::spawn_instance_listener(state.clone(), cmd_tx.clone()) {
        println!("Another Sidestep instance is already running ({resp}). Focused existing window.");
        return;
    }

    // Spawn supervisor background thread
    state::Supervisor::spawn(state.clone(), cmd_rx, exe);

    // Spawn system tray service in background thread so D-Bus initialization never delays the GUI
    let tray_state = state.clone();
    let tray_tx = cmd_tx.clone();
    std::thread::Builder::new()
        .name("sidestep-tray".into())
        .spawn(move || {
            use ksni::blocking::TrayMethods;
            let tray = tray::SidestepTray::new(tray_state.clone(), tray_tx);
            match tray.assume_sni_available(true).spawn() {
                Ok(handle) => {
                    if let Ok(mut st) = tray_state.lock() {
                        st.has_tray = true;
                    }
                    let mut last_view = None;
                    loop {
                        std::thread::sleep(Duration::from_millis(100));
                        let view = match tray_state.lock() {
                            Ok(st) => {
                                if st.should_exit {
                                    break;
                                }
                                st.view
                            }
                            Err(_) => break,
                        };
                        if Some(view) != last_view {
                            last_view = Some(view);
                            handle.update(|_| {});
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[WARN] Failed to spawn FreeDesktop system tray: {e:?}");
                }
            }
        })
        .ok();

    // Set application window icon from app.ico
    let mut viewport_builder = eframe::egui::ViewportBuilder::default()
        .with_inner_size([600.0, 560.0])
        .with_min_inner_size([540.0, 500.0])
        .with_title("Sidestep")
        .with_visible(true)
        .with_resizable(true)
        .with_decorations(true)
        .with_close_button(true);

    if let Ok(img) = image::load_from_memory(icons::APP_ICO) {
        let rgba = img
            .resize_exact(48, 48, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        viewport_builder = viewport_builder.with_icon(Arc::new(eframe::egui::IconData {
            rgba: rgba.into_raw(),
            width: 48,
            height: 48,
        }));
    }

    let options = eframe::NativeOptions {
        viewport: viewport_builder,
        ..Default::default()
    };

    let app_state = state.clone();
    let app_tx = cmd_tx;
    let res = eframe::run_native(
        "Sidestep",
        options,
        Box::new(move |_cc| Ok(Box::new(SidestepApp::new(app_state, app_tx)))),
    );
    if let Err(e) = res {
        eprintln!("[ERROR] eframe::run_native failed: {e}");
    }
}
