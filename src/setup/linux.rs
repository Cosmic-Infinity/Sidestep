//! Linux installation, portable mode detection, payload extraction, and uninstallation.

use std::path::{Path, PathBuf};

pub const PAYLOAD: [(&str, &[u8]); 1] = [("ciadpi", include_bytes!("../../vendor/ciadpi"))];
pub const APP_NAME: &str = "Sidestep";
const APP_EXE: &str = "sidestep";
const PORTABLE_MARKER: &str = "PORTABLE.txt";
const PORTABLE_DIR: &str = "sidestep-data";
const OWN_FILES: [&str; 4] = [APP_EXE, "ciadpi", "config.ini", PORTABLE_MARKER];

#[derive(Clone)]
pub enum Mode {
    Installed(PathBuf),
    Portable(PathBuf),
}

impl Mode {
    pub fn dir(&self) -> &Path {
        match self {
            Mode::Installed(d) | Mode::Portable(d) => d,
        }
    }
    pub fn is_portable(&self) -> bool {
        matches!(self, Mode::Portable(_))
    }
    pub fn app_exe(&self) -> PathBuf {
        match self {
            Mode::Installed(_) => user_bin_dir().join(APP_EXE),
            Mode::Portable(_) => std::env::current_exe().unwrap_or_else(|_| PathBuf::from(APP_EXE)),
        }
    }
    pub fn engine_exe(&self) -> PathBuf {
        let local = self.dir().join("ciadpi");
        if local.is_file() {
            return local;
        }
        if let Some(sys) = find_system_ciadpi() {
            return sys;
        }
        local
    }
}

pub fn find_system_ciadpi() -> Option<PathBuf> {
    for p in ["/usr/bin/ciadpi", "/usr/local/bin/ciadpi", "/bin/ciadpi"] {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("ciadpi");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn xdg_data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join(".local/share")
        })
}

pub fn user_data_dir() -> PathBuf {
    xdg_data_home().join("sidestep")
}
pub fn user_apps_dir() -> PathBuf {
    xdg_data_home().join("applications")
}
pub fn user_icons_dir() -> PathBuf {
    xdg_data_home().join("icons/hicolor/scalable/apps")
}

pub fn user_bin_dir() -> PathBuf {
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".local/bin"))
        .unwrap_or_else(|_| PathBuf::from("/usr/local/bin"))
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn portable_dir() -> PathBuf {
    exe_dir().join(PORTABLE_DIR)
}

pub fn detect() -> Mode {
    let p = portable_dir();
    if p.join(PORTABLE_MARKER).is_file() {
        return Mode::Portable(p);
    }
    Mode::Installed(user_data_dir())
}

#[cfg(unix)]
fn set_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
}

pub fn write_payload(dir: &Path) -> Result<Vec<&'static str>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let dest = dir.join("ciadpi");
    if dest.is_file() {
        return Ok(vec![]);
    }
    // Prefer system binary; fall back to embedded bytes.
    let wrote = if let Some(sys) = find_system_ciadpi() {
        std::fs::copy(&sys, &dest).is_ok()
    } else {
        false
    };
    if !wrote {
        std::fs::write(&dest, PAYLOAD[0].1).map_err(|e| format!("writing ciadpi: {e}"))?;
    }
    set_executable(&dest);
    Ok(vec!["ciadpi"])
}

pub fn install(dir: &Path) -> Result<Mode, String> {
    write_payload(dir)?;

    let bin_dir = user_bin_dir();
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("creating {}: {e}", bin_dir.display()))?;
    let target = bin_dir.join(APP_EXE);

    if let Ok(me) = std::env::current_exe() {
        if std::fs::canonicalize(&me).ok() != std::fs::canonicalize(&target).ok() {
            std::fs::copy(&me, &target)
                .map_err(|e| format!("copying app to {}: {e}", target.display()))?;
            set_executable(&target);
        }
    }

    // Install icon
    let icon_dir = user_icons_dir();
    if std::fs::create_dir_all(&icon_dir).is_ok() {
        let icon_svg = include_bytes!("../../icons/svg/app.svg");
        let _ = std::fs::write(icon_dir.join("sidestep.svg"), icon_svg);
    }

    // Install .desktop entry
    let apps_dir = user_apps_dir();
    if std::fs::create_dir_all(&apps_dir).is_ok() {
        let desktop = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Sidestep\n\
             Comment=Autonomous DPI Bypass\n\
             Exec=\"{}\" daemon\n\
             Icon=sidestep\n\
             Terminal=false\n\
             Categories=Network;\n",
            target.display()
        );
        let _ = std::fs::write(apps_dir.join("sidestep.desktop"), desktop);
    }

    Ok(Mode::Installed(dir.to_path_buf()))
}

pub fn uninstall(mode: &Mode) -> Vec<String> {
    let mut done = Vec::new();
    let _ = crate::autostart::unregister();
    done.push("Autostart entry removed".into());

    let desktop = user_apps_dir().join("sidestep.desktop");
    if desktop.exists() && std::fs::remove_file(&desktop).is_ok() {
        done.push("Desktop launcher removed".into());
    }

    let bin = user_bin_dir().join(APP_EXE);
    if bin.exists() && std::fs::remove_file(&bin).is_ok() {
        done.push("Binary ~/.local/bin/sidestep removed".into());
    }

    let dir = mode.dir();
    for f in OWN_FILES {
        let p = dir.join(f);
        if p.exists() {
            let _ = std::fs::remove_file(&p);
        }
    }
    let _ = std::fs::remove_dir(dir);
    done.push(format!("Files in {} removed", dir.display()));
    done
}
