//! First-launch setup, install / portable modes, uninstall, and migration from
//! the old "DPI Toggle" install.
//!
//! Nothing is written to disk or registry until the user picks a mode on the
//! setup screen.
//!
//!   Installed: files in a user-chosen folder (default %LOCALAPPDATA%\Programs\Sidestep),
//!              HKCU\Software\Sidestep\InstallDir, a Settings -> Apps entry,
//!              optional Start Menu shortcut.
//!   Portable:  files only in "Sidestep-data" next to the exe (marked by PORTABLE.txt).
//!              No registry, no shortcut.

use std::cell::RefCell;
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use crate::kit;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The engine. Icons are embedded in the exe and never written to disk.
pub const PAYLOAD: [(&str, &[u8]); 3] = [
    (
        "goodbyedpi.exe",
        include_bytes!("../../vendor/goodbyedpi.exe"),
    ),
    (
        "WinDivert.dll",
        include_bytes!("../../vendor/WinDivert.dll"),
    ),
    (
        "WinDivert64.sys",
        include_bytes!("../../vendor/WinDivert64.sys"),
    ),
];
pub const APP_NAME: &str = "Sidestep";
const APP_EXE: &str = "Sidestep.exe";
const PORTABLE_MARKER: &str = "PORTABLE.txt";
const PORTABLE_DIR: &str = "Sidestep-data";
/// Every file this app may create in its folder. Uninstall deletes only these.
const OWN_FILES: [&str; 6] = [
    APP_EXE,
    "goodbyedpi.exe",
    "WinDivert.dll",
    "WinDivert64.sys",
    "config.ini",
    PORTABLE_MARKER,
];

const REG_APP: &str = "Software\\Sidestep";
const REG_UNINSTALL: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Sidestep";

// The app's previous name.
const LEGACY_REG_APP: &str = "Software\\DpiToggle";
const LEGACY_REG_UNINSTALL: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\DpiToggle";
const LEGACY_TASK: &str = "DpiToggle";
const LEGACY_LNK: &str = "DPI Toggle.lnk";
const LEGACY_FILES: [&str; 8] = [
    "DpiToggle.exe",
    "goodbyedpi.exe",
    "WinDivert.dll",
    "WinDivert64.sys",
    "icon_on.ico",
    "icon_off.ico",
    "config.ini",
    PORTABLE_MARKER,
];

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
    /// The exe that autostart / shortcuts should launch.
    pub fn app_exe(&self) -> PathBuf {
        match self {
            Mode::Installed(d) => d.join(APP_EXE),
            Mode::Portable(_) => std::env::current_exe().unwrap_or_else(|_| PathBuf::from(APP_EXE)),
        }
    }
}

// ----------------------------------------------------------------- registry

#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(
        k: isize,
        sub: *const u16,
        val: *const u16,
        flags: u32,
        ty: *mut u32,
        data: *mut c_void,
        len: *mut u32,
    ) -> i32;
    fn RegSetKeyValueW(
        k: isize,
        sub: *const u16,
        val: *const u16,
        ty: u32,
        data: *const c_void,
        len: u32,
    ) -> i32;
    fn RegDeleteTreeW(k: isize, sub: *const u16) -> i32;
}
const HKCU: isize = 0x8000_0001u32 as i32 as isize;
const RRF_RT_REG_SZ: u32 = 0x2;
const REG_SZ: u32 = 1;
const REG_DWORD: u32 = 4;

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn reg_get(sub: &str, name: &str) -> Option<String> {
    let (sub, name) = (w(sub), w(name));
    let mut len = 0u32;
    unsafe {
        if RegGetValueW(
            HKCU,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut len,
        ) != 0
        {
            return None;
        }
        let mut buf = vec![0u16; (len as usize / 2) + 1];
        if RegGetValueW(
            HKCU,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
        ) != 0
        {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

fn reg_set_sz(sub: &str, name: &str, val: &str) -> Result<(), String> {
    let v = w(val);
    let r = unsafe {
        RegSetKeyValueW(
            HKCU,
            w(sub).as_ptr(),
            w(name).as_ptr(),
            REG_SZ,
            v.as_ptr() as *const c_void,
            (v.len() * 2) as u32,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(format!("registry write {sub}\\{name} failed ({r})"))
    }
}

fn reg_set_dword(sub: &str, name: &str, val: u32) -> Result<(), String> {
    let r = unsafe {
        RegSetKeyValueW(
            HKCU,
            w(sub).as_ptr(),
            w(name).as_ptr(),
            REG_DWORD,
            &val as *const u32 as *const c_void,
            4,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(format!("registry write {sub}\\{name} failed ({r})"))
    }
}

fn reg_delete(sub: &str) {
    unsafe {
        RegDeleteTreeW(HKCU, w(sub).as_ptr());
    }
}

// ----------------------------------------------------------------- paths

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn portable_dir() -> PathBuf {
    exe_dir().join(PORTABLE_DIR)
}

pub fn default_install_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Sidestep"))
        .join("Programs")
        .join(APP_NAME)
}

fn start_menu(name: &str) -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| {
        PathBuf::from(a)
            .join("Microsoft\\Windows\\Start Menu\\Programs")
            .join(name)
    })
}

fn shortcut_path() -> Option<PathBuf> {
    start_menu("Sidestep.lnk")
}

/// Which mode this exe was set up in, if any. `Err(Some(dir))` = installed
/// before but files are missing (setup screen pre-fills that folder).
pub fn detect() -> Result<Mode, Option<PathBuf>> {
    let p = portable_dir();
    if p.join(PORTABLE_MARKER).is_file() {
        return Ok(Mode::Portable(p));
    }
    match reg_get(REG_APP, "InstallDir").map(PathBuf::from) {
        Some(d) if d.join("goodbyedpi.exe").is_file() => Ok(Mode::Installed(d)),
        Some(d) => Err(Some(d)),
        None => Err(None),
    }
}

/// Writes the engine files into `dir` where missing or different.
/// Returns the names of files that were (re)written.
pub fn write_payload(dir: &Path) -> Result<Vec<&'static str>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let mut written = Vec::new();
    for (name, data) in PAYLOAD {
        let p = dir.join(name);
        if std::fs::read(&p).map(|b| b != data).unwrap_or(true) {
            std::fs::write(&p, data).map_err(|e| {
                format!(
                    "writing {}: {e}\n(Is goodbyedpi from this folder still running?)",
                    p.display()
                )
            })?;
            written.push(name);
        }
    }
    Ok(written)
}

// ----------------------------------------------------------------- install / portable

fn install(dir: &Path, shortcut: bool) -> Result<Mode, String> {
    write_payload(dir)?;

    let target = dir.join(APP_EXE);
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let same = std::fs::canonicalize(&me).ok() == std::fs::canonicalize(&target).ok();
    if !same {
        std::fs::copy(&me, &target)
            .map_err(|e| format!("copying app to {}: {e}", target.display()))?;
    }

    let d = dir.display().to_string();
    let exe = target.display().to_string();
    reg_set_sz(REG_APP, "InstallDir", &d)?;
    reg_set_sz(REG_UNINSTALL, "DisplayName", APP_NAME)?;
    reg_set_sz(REG_UNINSTALL, "DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    reg_set_sz(REG_UNINSTALL, "Publisher", APP_NAME)?;
    reg_set_sz(REG_UNINSTALL, "InstallLocation", &d)?;
    reg_set_sz(REG_UNINSTALL, "DisplayIcon", &exe)?;
    reg_set_sz(
        REG_UNINSTALL,
        "UninstallString",
        &format!("\"{exe}\" --uninstall"),
    )?;
    reg_set_dword(REG_UNINSTALL, "NoModify", 1)?;
    reg_set_dword(REG_UNINSTALL, "NoRepair", 1)?;

    if shortcut {
        if let Some(lnk) = shortcut_path() {
            let ok = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:SS_LNK);\
                     $s.TargetPath=$env:SS_EXE;$s.WorkingDirectory=$env:SS_DIR;$s.Description='Sidestep';$s.Save()",
                ])
                .env("SS_LNK", &lnk)
                .env("SS_EXE", &target)
                .env("SS_DIR", dir)
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                return Err("installed, but creating the Start Menu shortcut failed".into());
            }
        }
    }
    Ok(Mode::Installed(dir.to_path_buf()))
}

fn make_portable() -> Result<Mode, String> {
    let dir = portable_dir();
    write_payload(&dir)?;
    std::fs::write(
        dir.join(PORTABLE_MARKER),
        "This folder belongs to Sidestep.exe (portable mode).\r\n\
         Delete this folder to remove all of its files. If 'Launch at logon' was enabled,\r\n\
         untick it in the app first (or delete the 'Sidestep' task in Task Scheduler).\r\n",
    )
    .map_err(|e| format!("writing portable marker: {e}"))?;
    Ok(Mode::Portable(dir))
}

// ----------------------------------------------------------------- old "DPI Toggle" install

pub fn legacy_install() -> Option<PathBuf> {
    reg_get(LEGACY_REG_APP, "InstallDir").map(PathBuf::from)
}

/// Carries the old settings over (if the new folder has none) and removes the
/// old install: its logon task, shortcut, registry entries and own files.
fn remove_legacy(old: &Path, new_dir: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    let (old_cfg, new_cfg) = (old.join("config.ini"), new_dir.join("config.ini"));
    if old_cfg.is_file() && !new_cfg.exists() && std::fs::copy(&old_cfg, &new_cfg).is_ok() {
        notes.push("Kept your settings and flags from DPI Toggle.".to_string());
    }
    let _ = crate::autostart::unregister_task(LEGACY_TASK);
    if let Some(lnk) = start_menu(LEGACY_LNK) {
        let _ = std::fs::remove_file(lnk);
    }
    reg_delete(LEGACY_REG_UNINSTALL);
    reg_delete(LEGACY_REG_APP);
    let mut stuck = Vec::new();
    for f in LEGACY_FILES {
        let p = old.join(f);
        if p.exists() && std::fs::remove_file(&p).is_err() {
            stuck.push(f);
        }
    }
    let _ = std::fs::remove_dir(old);
    if stuck.is_empty() {
        notes.push(format!(
            "Removed the old DPI Toggle install ({}).",
            kit::short_path(old)
        ));
    } else {
        notes.push(format!(
            "Removed DPI Toggle's task, shortcut and registry entries; could not delete {} in {} (still running?). Delete them by hand.",
            stuck.join(", "),
            kit::short_path(old)
        ));
    }
    notes
}

// ----------------------------------------------------------------- uninstall

/// Removes everything this app created. Only files in OWN_FILES are deleted,
/// so picking a shared folder (e.g. Desktop) as install dir can't wipe it.
pub fn uninstall(mode: &Mode) -> Vec<String> {
    let mut done = Vec::new();
    match crate::autostart::unregister() {
        Ok(()) => done.push("logon task removed (if it existed)".to_string()),
        Err(e) => done.push(format!("logon task: {e}")),
    }
    let _ = Command::new("sc")
        .args(["stop", "WinDivert"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();

    if let Mode::Installed(_) = mode {
        if let Some(lnk) = shortcut_path() {
            if lnk.exists() && std::fs::remove_file(&lnk).is_ok() {
                done.push("Start Menu shortcut removed".into());
            }
        }
        reg_delete(REG_UNINSTALL);
        reg_delete(REG_APP);
        done.push("registry entries removed (HKCU\\Software\\Sidestep, Settings > Apps)".into());
    }
    done.push(format!(
        "Sidestep's files in {} are deleted in the background as soon as Sidestep closes, after you \
         click OK. If WinDivert64.sys is still in use, it is left behind: delete it after a restart.",
        mode.dir().display()
    ));
    done
}

/// Call right before the process exits: deletes our own files (and the folder
/// if it ends up empty) as soon as this exe has exited and is no longer locked.
pub fn schedule_file_deletion(mode: &Mode) {
    let dir = mode.dir();
    let files: Vec<PathBuf> = OWN_FILES
        .iter()
        .map(|f| dir.join(f))
        .filter(|p| p.exists())
        .collect();
    let script = deletion_script(std::process::id(), &files, dir);
    let ok = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .is_ok();
    if !ok {
        nwg::error_message(
            APP_NAME,
            &format!(
                "Could not schedule file deletion. Delete {} manually.",
                dir.display()
            ),
        );
    }
}

/// A hidden PowerShell command that waits for process `pid` to exit (no timer, no network),
/// deletes `files` (retrying briefly while a file is still locked), then removes `dir` only if empty.
fn deletion_script(pid: u32, files: &[PathBuf], dir: &Path) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    let list = if files.is_empty() {
        "@()".to_string()
    } else {
        files.iter().map(|f| q(f)).collect::<Vec<_>>().join(",")
    };
    format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         Wait-Process -Id {pid} -Timeout 60; \
         foreach ($f in @({list})) {{ \
           for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $f); $i++) {{ \
             Remove-Item -LiteralPath $f -Force; \
             if (Test-Path -LiteralPath $f) {{ Start-Sleep -Milliseconds 250 }} }} }}; \
         $d = {dir}; \
         if ((Test-Path -LiteralPath $d) -and -not (Get-ChildItem -LiteralPath $d -Force)) {{ Remove-Item -LiteralPath $d -Force }}",
        dir = q(dir)
    )
}

pub fn uninstall_summary(mode: &Mode) -> String {
    let what = match mode {
        Mode::Installed(d) => format!(
            "- stop goodbyedpi and unload the WinDivert driver\n\
             - remove the logon task (if enabled)\n\
             - remove the Start Menu shortcut and the Settings > Apps entry\n\
             - delete Sidestep's files from:\n   {}\n   (only its own files; other files in that folder are left alone)",
            d.display()
        ),
        Mode::Portable(d) => format!(
            "- stop goodbyedpi and unload the WinDivert driver\n\
             - remove the logon task (if enabled)\n\
             - delete the data folder:\n   {}\n\
             Sidestep.exe itself is left where it is. Delete it yourself afterwards.",
            d.display()
        ),
    };
    format!("This will:\n{what}\n\nContinue?")
}

// ----------------------------------------------------------------- setup window

#[derive(Default)]
struct SetupUi {
    fonts: kit::Fonts,
    glyphs: kit::Glyphs,
    app_icon: nwg::Icon,
    window: nwg::Window,
    logo: nwg::ImageFrame,
    title: nwg::Label,
    subtitle: nwg::Label,
    rb_install: nwg::RadioButton,
    desc_install: nwg::Label,
    txt_dir: nwg::TextInput,
    btn_browse: nwg::Button,
    chk_shortcut: nwg::CheckBox,
    rb_portable: nwg::RadioButton,
    desc_portable: nwg::Label,
    txt_portable: nwg::TextInput,
    files_note: nwg::Label,
    chk_legacy: nwg::CheckBox,
    lbl_status: nwg::Label,
    btn_continue: nwg::Button,
    btn_cancel: nwg::Button,
    picker: nwg::FileDialog,
}

impl SetupUi {
    fn install_selected(&self) -> bool {
        self.rb_install.check_state() == nwg::RadioButtonState::Checked
    }
    fn sync_enabled(&self) {
        let i = self.install_selected();
        self.txt_dir.set_enabled(i);
        self.btn_browse.set_enabled(i);
        self.chk_shortcut.set_enabled(i);
        self.btn_continue
            .set_text(if i { "Install" } else { "Run portable" });
    }
}

/// Shows the setup screen. Returns the chosen and completed mode plus notes for
/// the log, or None if the user cancelled (in which case nothing was written).
#[allow(clippy::type_complexity)]
pub fn run_setup(prefill: Option<PathBuf>) -> Option<(Mode, Vec<String>)> {
    let legacy = legacy_install();
    let mut ui = SetupUi::default();
    if let Err(e) = build_setup(
        &mut ui,
        prefill.unwrap_or_else(default_install_dir),
        legacy.as_deref(),
    ) {
        nwg::error_message("Sidestep setup", &format!("UI error: {e}"));
        return None;
    }
    ui.sync_enabled();
    let ui = Rc::new(ui);
    let result: Rc<RefCell<Option<(Mode, Vec<String>)>>> = Rc::new(RefCell::new(None));

    let (wui, wres) = (Rc::downgrade(&ui), result.clone());
    let handler = nwg::full_bind_event_handler(&ui.window.handle, move |evt, _data, h| {
        let Some(ui) = wui.upgrade() else { return };
        match evt {
            nwg::Event::OnWindowClose if h == ui.window.handle => nwg::stop_thread_dispatch(),
            nwg::Event::OnButtonClick => {
                if h == ui.rb_install.handle || h == ui.rb_portable.handle {
                    ui.sync_enabled();
                } else if h == ui.btn_cancel.handle {
                    nwg::stop_thread_dispatch();
                } else if h == ui.btn_browse.handle {
                    if ui.picker.run(Some(&ui.window)) {
                        if let Ok(p) = ui.picker.get_selected_item() {
                            ui.txt_dir
                                .set_text(&PathBuf::from(p).join(APP_NAME).display().to_string());
                        }
                    }
                } else if h == ui.btn_continue.handle {
                    let r = if ui.install_selected() {
                        let d = PathBuf::from(ui.txt_dir.text().trim());
                        if !d.is_absolute() {
                            ui.lbl_status
                                .set_text("Enter a full folder path, e.g. C:\\Tools\\Sidestep");
                            return;
                        }
                        install(
                            &d,
                            ui.chk_shortcut.check_state() == nwg::CheckBoxState::Checked,
                        )
                    } else {
                        make_portable()
                    };
                    match r {
                        Ok(m) => {
                            let mut notes = Vec::new();
                            if let Some(old) = &legacy {
                                if ui.chk_legacy.check_state() == nwg::CheckBoxState::Checked {
                                    notes = remove_legacy(old, m.dir());
                                }
                            }
                            *wres.borrow_mut() = Some((m, notes));
                            nwg::stop_thread_dispatch();
                        }
                        Err(e) => ui.lbl_status.set_text(&format!("Failed: {e}")),
                    }
                }
            }
            _ => {}
        }
    });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    ui.window.set_visible(false);
    let r = result.borrow_mut().take();
    r
}

fn build_setup(ui: &mut SetupUi, dir: PathBuf, legacy: Option<&Path>) -> Result<(), nwg::NwgError> {
    use kit::{check, icon_button, label};
    ui.fonts.build()?;
    ui.glyphs = kit::Glyphs::load();
    ui.app_icon = kit::icon_from_ico(kit::APP_ICO, kit::px(40));
    let small_app = kit::icon_from_ico(kit::APP_ICO, kit::px(32));
    let f = &ui.fonts;
    let h = if legacy.is_some() { 534 } else { 506 };
    nwg::Window::builder()
        .size((540, h))
        .position((380, 150))
        .title("Sidestep Setup")
        .icon(Some(&small_app))
        .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::VISIBLE)
        .build(&mut ui.window)?;
    let w = &ui.window;

    nwg::ImageFrame::builder()
        .icon(Some(&ui.app_icon))
        .position((24, 20))
        .size((40, 40))
        .parent(w)
        .build(&mut ui.logo)?;
    label(
        &mut ui.title,
        w,
        "Set up Sidestep",
        (76, 16),
        (440, 32),
        &f.head,
    )?;
    label(
        &mut ui.subtitle,
        w,
        "Nothing has been written to this PC yet. Choose how you want to run it.",
        (77, 48),
        (440, 20),
        &f.small,
    )?;

    nwg::RadioButton::builder()
        .text("Install for this user")
        .font(Some(&f.bold))
        .flags(
            nwg::RadioButtonFlags::VISIBLE
                | nwg::RadioButtonFlags::GROUP
                | nwg::RadioButtonFlags::TAB_STOP,
        )
        .check_state(nwg::RadioButtonState::Checked)
        .position((24, 92))
        .size((492, 26))
        .parent(w)
        .build(&mut ui.rb_install)?;
    label(
        &mut ui.desc_install,
        w,
        "Copies the app into this folder and adds an entry in Settings > Apps so you can uninstall it.",
        (48, 120),
        (468, 20),
        &f.small,
    )?;
    nwg::TextInput::builder()
        .text(&dir.display().to_string())
        .position((48, 146))
        .size((362, 28))
        .parent(w)
        .build(&mut ui.txt_dir)?;
    icon_button(
        &mut ui.btn_browse,
        w,
        "Browse",
        &ui.glyphs.folder,
        (418, 145),
        (98, 30),
    )?;
    check(
        &mut ui.chk_shortcut,
        w,
        "Add a Start Menu shortcut",
        (48, 182),
        (468, 24),
    )?;
    ui.chk_shortcut.set_check_state(nwg::CheckBoxState::Checked);

    nwg::RadioButton::builder()
        .text("Portable")
        .font(Some(&f.bold))
        .flags(nwg::RadioButtonFlags::VISIBLE | nwg::RadioButtonFlags::TAB_STOP)
        .position((24, 226))
        .size((492, 26))
        .parent(w)
        .build(&mut ui.rb_portable)?;
    label(
        &mut ui.desc_portable,
        w,
        "Keeps everything in a folder next to this exe. No registry, no shortcut. Delete the folder to clean up.",
        (48, 254),
        (468, 38),
        &f.small,
    )?;
    nwg::TextInput::builder()
        .text(&kit::short_path(&portable_dir()))
        .readonly(true)
        .position((48, 296))
        .size((468, 28))
        .parent(w)
        .build(&mut ui.txt_portable)?;

    label(
        &mut ui.files_note,
        w,
        "Either way, the files are goodbyedpi.exe, WinDivert.dll, WinDivert64.sys (the packet driver, loaded only while the bypass is on) and config.ini. Launch at logon stays off until you enable it.",
        (24, 340),
        (492, 54),
        &f.small,
    )?;
    let mut y = 398;
    if let Some(old) = legacy {
        check(
            &mut ui.chk_legacy,
            w,
            &format!(
                "Remove the old DPI Toggle install ({}) and keep its flags",
                kit::short_path(old)
            ),
            (24, y),
            (492, 24),
        )?;
        ui.chk_legacy.set_check_state(nwg::CheckBoxState::Checked);
        y += 28;
    }
    label(&mut ui.lbl_status, w, "", (24, y), (492, 20), &f.small)?;
    y += 30;
    nwg::Button::builder()
        .text("Cancel")
        .position((300, y))
        .size((100, 34))
        .parent(w)
        .build(&mut ui.btn_cancel)?;
    nwg::Button::builder()
        .text("Install")
        .position((408, y))
        .size((108, 34))
        .parent(w)
        .build(&mut ui.btn_continue)?;
    ui.btn_continue.set_font(Some(&f.bold));

    nwg::FileDialog::builder()
        .title("Choose where to install Sidestep (a Sidestep subfolder is added)")
        .action(nwg::FileDialogAction::OpenDirectory)
        .build(&mut ui.picker)?;
    Ok(())
}
