//! System proxy integration for Linux (GNOME, KDE Plasma, systemd user environment, and XDG environment files).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn gsettings(args: &[&str]) -> bool {
    Command::new("gsettings").args(args).status().map(|s| s.success()).unwrap_or(false)
}

fn kwriteconfig(kw: &str, args: &[&str]) -> bool {
    Command::new(kw).args(args).status().map(|s| s.success()).unwrap_or(false)
}

/// Configures system proxy across all supported Linux desktop environments (GNOME, KDE Plasma, systemd, and env files).
pub fn set_socks_proxy(port: u16) -> bool {
    let mut any_ok = false;

    // 1. GNOME / Cinnamon / Budgie / Unity via gsettings
    if gsettings(&["set", "org.gnome.system.proxy.socks", "host", "127.0.0.1"])
        && gsettings(&["set", "org.gnome.system.proxy.socks", "port", &port.to_string()])
        && gsettings(&["set", "org.gnome.system.proxy", "mode", "manual"])
    {
        any_ok = true;
    }

    // 2. KDE Plasma 5 / 6 via kwriteconfig
    for kw in ["kwriteconfig6", "kwriteconfig5"] {
        if kwriteconfig(kw, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", "ProxyType", "1"]) {
            let _ = kwriteconfig(kw, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", "socksProxy", &format!("socks://127.0.0.1:{port}")]);
            let _ = Command::new("dbus-send").args(["--type=signal", "/KIO/Scheduler", "org.kde.KIO.Scheduler.reparseSlaveConfiguration", "string:''"]).status();
            any_ok = true;
            break;
        }
    }

    // 3. systemd user environment
    let proxy_url = format!("socks5h://127.0.0.1:{port}");
    let _ = Command::new("systemctl").args(["--user", "set-environment", &format!("all_proxy={proxy_url}"), &format!("ALL_PROXY={proxy_url}")]).status();

    // 4. Write exportable environment file for non-desktop / tiling window manager shell users
    let dir = proxy_env_dir();
    let _ = fs::create_dir_all(&dir);
    let env_file = dir.join("proxy.env");
    let content = format!("export all_proxy=\"socks5h://127.0.0.1:{port}\"\nexport ALL_PROXY=\"socks5h://127.0.0.1:{port}\"\n");
    let _ = fs::write(env_file, content);

    any_ok
}

/// Disables proxy across all supported Linux desktop environments, restoring direct connectivity.
pub fn clear_socks_proxy() -> bool {
    let mut any_ok = false;

    // 1. GNOME / Cinnamon / Budgie
    if gsettings(&["set", "org.gnome.system.proxy", "mode", "none"]) {
        any_ok = true;
    }

    // 2. KDE Plasma 5 / 6
    for kw in ["kwriteconfig6", "kwriteconfig5"] {
        if kwriteconfig(kw, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", "ProxyType", "0"]) {
            let _ = Command::new("dbus-send").args(["--type=signal", "/KIO/Scheduler", "org.kde.KIO.Scheduler.reparseSlaveConfiguration", "string:''"]).status();
            any_ok = true;
            break;
        }
    }

    // 3. systemd user environment
    let _ = Command::new("systemctl").args(["--user", "unset-environment", "all_proxy", "ALL_PROXY"]).status();

    // 4. Remove environment file
    let _ = fs::remove_file(proxy_env_dir().join("proxy.env"));

    any_ok
}

/// Checks whether a system proxy is currently configured.
pub fn is_socks_proxy_active() -> bool {
    if let Ok(out) = Command::new("gsettings").args(["get", "org.gnome.system.proxy", "mode"]).output() {
        if String::from_utf8_lossy(&out.stdout).contains("manual") { return true; }
    }

    for kr in ["kreadconfig6", "kreadconfig5"] {
        if let Ok(out) = Command::new(kr).args(["--file", "kioslaverc", "--group", "Proxy Settings", "--key", "ProxyType"]).output() {
            if String::from_utf8_lossy(&out.stdout).trim() == "1" { return true; }
        }
    }

    proxy_env_dir().join("proxy.env").is_file()
}

fn proxy_env_dir() -> PathBuf {
    crate::setup::user_data_dir()
}
