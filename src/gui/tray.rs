//! Linux FreeDesktop System Tray indicator (StatusNotifierItem via ksni).

use ksni::menu::*;
use ksni::{Category, Icon, Status, ToolTip, Tray};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use super::icons::{ico_to_ksni_icon, APP_ICO, BUSY_ICO, OFF_ICO, ON_ICO};
use super::state::{AppState, UiCmd, View};

fn icon_name_for(view: View) -> &'static str {
    match view {
        View::On => "sidestep-on",
        View::Starting | View::Checking | View::Tuning => "sidestep-busy",
        View::Off => "sidestep-off",
        _ => "sidestep",
    }
}

pub struct SidestepTray {
    state: Arc<Mutex<AppState>>,
    cmd_tx: Sender<UiCmd>,
    icon_theme_dir: String,
    icon_off: Icon,
    icon_on: Icon,
    icon_busy: Icon,
}

impl SidestepTray {
    pub fn new(state: Arc<Mutex<AppState>>, cmd_tx: Sender<UiCmd>) -> Self {
        let icon_off = ico_to_ksni_icon(OFF_ICO, 32);
        let icon_on = ico_to_ksni_icon(ON_ICO, 32);
        let icon_busy = ico_to_ksni_icon(BUSY_ICO, 32);

        // Ensure icon theme directory exists on disk for desktop shells that read icons by theme name
        let icon_dir = dirs_icon_cache();
        if let Some(ref d) = icon_dir {
            let _ = fs::create_dir_all(d);
            save_png_if_missing(&d.join("sidestep-off.png"), OFF_ICO);
            save_png_if_missing(&d.join("sidestep-on.png"), ON_ICO);
            save_png_if_missing(&d.join("sidestep-busy.png"), BUSY_ICO);
            save_png_if_missing(&d.join("sidestep.png"), APP_ICO);
        }

        let icon_theme_dir = icon_dir
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        Self {
            state,
            cmd_tx,
            icon_theme_dir,
            icon_off,
            icon_on,
            icon_busy,
        }
    }
}

fn dirs_icon_cache() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("sidestep/icons"))
}

fn save_png_if_missing(dest: &Path, ico_bytes: &[u8]) {
    if !dest.exists() {
        if let Ok(img) = image::load_from_memory(ico_bytes) {
            let _ = img
                .resize_exact(32, 32, image::imageops::FilterType::Lanczos3)
                .save(dest);
        }
    }
}

impl Tray for SidestepTray {
    fn id(&self) -> String {
        "sidestep".into()
    }

    fn title(&self) -> String {
        "Sidestep".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_theme_path(&self) -> String {
        self.icon_theme_dir.clone()
    }

    fn icon_name(&self) -> String {
        let st = self.state.lock().unwrap();
        icon_name_for(st.view).into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let st = self.state.lock().unwrap();
        let icon = match st.view {
            View::On => &self.icon_on,
            View::Off => &self.icon_off,
            _ => &self.icon_busy,
        };
        vec![icon.clone()]
    }

    fn attention_icon_name(&self) -> String {
        let st = self.state.lock().unwrap();
        icon_name_for(st.view).into()
    }

    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        self.icon_pixmap()
    }

    fn tool_tip(&self) -> ToolTip {
        let st = self.state.lock().unwrap();
        let sub = match st.view {
            View::On => "Bypass is ON (verified)",
            View::Off => "Bypass is OFF",
            View::Starting => "Starting bypass engine...",
            View::Checking => "Checking test sites...",
            View::Tuning => "Tuning TTL for network...",
            View::Blocked => "Bypass is ON, but DPI is blocking",
            View::Unreachable => "Test sites unreachable",
        };
        let icon_name = icon_name_for(st.view);
        let icon_pixmap = match st.view {
            View::On => self.icon_on.clone(),
            View::Off => self.icon_off.clone(),
            _ => self.icon_busy.clone(),
        };

        ToolTip {
            title: "Sidestep".into(),
            description: sub.into(),
            icon_name: icon_name.into(),
            icon_pixmap: vec![icon_pixmap],
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.cmd_tx.send(UiCmd::ShowWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let (is_running, cmd_tx) = {
            let st = self.state.lock().unwrap();
            (
                st.engine_pid.is_some() || st.view != View::Off,
                self.cmd_tx.clone(),
            )
        };

        let tx_open = cmd_tx.clone();
        let tx_toggle = cmd_tx.clone();
        let tx_tune = cmd_tx.clone();
        let tx_quit = cmd_tx;

        let toggle_label = if is_running {
            "Bypass off"
        } else {
            "Bypass on"
        };

        vec![
            StandardItem {
                label: toggle_label.into(),
                activate: Box::new(move |_| {
                    let _ = tx_toggle.send(UiCmd::Toggle);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open Sidestep".into(),
                activate: Box::new(move |_| {
                    let _ = tx_open.send(UiCmd::ShowWindow);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Find TTL now".into(),
                activate: Box::new(move |_| {
                    let _ = tx_tune.send(UiCmd::TuneNow);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Exit".into(),
                activate: Box::new(move |_| {
                    let _ = tx_quit.send(UiCmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
