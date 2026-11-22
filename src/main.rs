#![windows_subsystem = "windows"]
//! Sidestep: a tray front-end for goodbyedpi with an honesty contract:
//!   no tray icon         => goodbyedpi is not running (kill-on-close job object)
//!   gray badge / "off"   => goodbyedpi is not running
//!   amber badge          => goodbyedpi is running, not (yet) verified to pass
//!   green badge / "on"   => goodbyedpi is running and the last check passed
//! The badge follows the real child process (polled every 500 ms), never a wish.

mod autostart;
mod config;
mod engine;
mod kit;
mod setup;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use config::Config;
use engine::Engine;
use kit::{checked, set_checked, set_if, Fonts, Glyphs};
use probe::Verdict;
use sidestep_core::{flags, probe};

const LOG_LINES: usize = 500;
const APP: &str = setup::APP_NAME;

pub enum Ev {
    Tick,
    Log(String),
    GdpiLine(u64, String),
    GdpiEof(u64),
    Probe { verdict: Verdict, lines: Vec<String> },
    /// (ttl, attempt index from 1, total attempts, flags)
    TuneStep(u8, usize, usize, String),
    TuneDone(Result<(u8, String), String>),
}

// ----------------------------------------------------------------- win32 bits

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attrs: *mut c_void, owner: i32, name: *const u16) -> *mut c_void;
    fn GetLastError() -> u32;
    fn GetLocalTime(st: *mut [u16; 8]);
}
#[link(name = "user32")]
extern "system" {
    fn GetSystemMetrics(index: i32) -> i32;
}

fn now_hms() -> String {
    let mut st = [0u16; 8];
    unsafe { GetLocalTime(&mut st) };
    format!("{:02}:{:02}:{:02}", st[4], st[5], st[6])
}

fn single_instance() -> bool {
    let name: Vec<u16> = "Local\\Sidestep.Instance\0".encode_utf16().collect();
    unsafe {
        let h = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
        !(h.is_null() || GetLastError() == 183 /* ERROR_ALREADY_EXISTS */)
    }
    // handle intentionally leaked: held until process exit
}

fn hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn ask_yes_no(parent: Option<&nwg::Window>, title: &str, text: &str) -> bool {
    let p = nwg::MessageParams { title, content: text, buttons: nwg::MessageButtons::YesNo, icons: nwg::MessageIcons::Question };
    let choice = match parent {
        Some(w) => nwg::modal_message(w, &p),
        None => nwg::message(&p),
    };
    choice == nwg::MessageChoice::Yes
}

// ----------------------------------------------------------------- state model

/// Result of the most recent block check for the *current* goodbyedpi process.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verify {
    Unknown,
    Passed,
    Blocked,
    Unreachable,
}

/// What the header / tray show.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Off,
    Starting,
    Checking,
    Tuning,
    On,
    Blocked,
    Unreachable,
}

// ----------------------------------------------------------------- UI layout

const R_ENGINE: usize = 0;
const R_PID: usize = 1;
const R_UPTIME: usize = 2;
const R_TTL: usize = 3;
const R_FLAGS: usize = 4;
const R_CHECK: usize = 5;
const R_COUNTS: usize = 6;
const ROWS: [(&str, &str); 7] = [
    ("Engine", "Whether goodbyedpi.exe, started by Sidestep, is alive right now. Checked twice a second."),
    ("PID", "Process ID of goodbyedpi.exe. You can find it in Task Manager > Details."),
    ("Uptime", "How long the current goodbyedpi process has run. Resets when flags change or a TTL search restarts it."),
    ("TTL in use", "Hops the decoy packet survives (--set-ttl). It must pass the campus firewall but expire before the server. TTL search measures it."),
    ("Active flags", "Exactly what goodbyedpi was started with."),
    ("Last check", "Last block check: a TLS hello to each test site through the bypass. Answer = passed; reset or silence = blocked."),
    ("Checks", "Checks this session that reached the sites, and checks the DPI blocked."),
];

/// Header band colours (COLORREF 0x00BBGGRR) and the headline colour on each.
const BAND_OFF: (u32, u32) = (0x00F5F3F1, 0x00574F49); // light grey, slate text
const BAND_BUSY: (u32, u32) = (0x00BFF3FF, 0x00004A9C); // light amber, dark amber text
const BAND_ON: (u32, u32) = (0x00D8F9D3, 0x003E8A2B); // light green, dark green text
const BAND_H: i32 = 80; // logical px; the tabs start just below

#[derive(Default)]
struct Ui {
    fonts: Fonts,
    g: Glyphs,
    app_icon: nwg::Icon,
    tray_on: nwg::Icon,
    tray_off: nwg::Icon,
    tray_busy: nwg::Icon,
    big_on: nwg::Icon,
    big_off: nwg::Icon,
    big_busy: nwg::Icon,
    tab_icons: nwg::ImageList,
    page_hooks: Vec<nwg::RawEventHandler>,
    band: Option<nwg::RawEventHandler>,
    band_colours: Rc<Cell<(u32, u32)>>,

    window: nwg::Window,
    tray: nwg::TrayNotification,
    tray_menu: nwg::Menu,
    mi_toggle: nwg::MenuItem,
    mi_open: nwg::MenuItem,
    mi_tune: nwg::MenuItem,
    mi_sep: nwg::MenuSeparator,
    mi_exit: nwg::MenuItem,
    notice: nwg::Notice,
    tips: nwg::Tooltip,

    // header
    img_state: nwg::ImageFrame,
    lbl_head: nwg::Label,
    lbl_sub: nwg::Label,
    btn_toggle: nwg::Button,
    pb_busy: nwg::ProgressBar,
    pb_steps: nwg::ProgressBar,

    tabs: nwg::TabsContainer,
    tab_status: nwg::Tab,
    tab_flags: nwg::Tab,
    tab_log: nwg::Tab,
    tab_settings: nwg::Tab,

    // status tab
    row_glyphs: Vec<nwg::ImageFrame>,
    row_names: Vec<nwg::Label>,
    row_values: Vec<nwg::Label>,
    img_check: nwg::ImageFrame,
    btn_check: nwg::Button,
    btn_tune: nwg::Button,
    btn_status_help: nwg::Button,

    // flags tab
    txt_flags: nwg::TextInput,
    btn_apply: nwg::Button,
    btn_defaults: nwg::Button,
    img_ref: nwg::ImageFrame,
    lbl_ref: nwg::Label,
    lst_flags: nwg::ListView,
    lbl_flag_detail: nwg::Label,
    btn_flag_toggle: nwg::Button,

    // log tab
    console_wrap: nwg::TextBox,
    console_nowrap: nwg::TextBox,
    chk_wrap: nwg::CheckBox,
    btn_clear_log: nwg::Button,

    // settings tab
    img_detect: nwg::ImageFrame,
    hdr_detect: nwg::Label,
    img_sites: nwg::ImageFrame,
    lbl_sites: nwg::Label,
    txt_sites: nwg::TextInput,
    chk_autotune: nwg::CheckBox,
    lbl_detect_hint: nwg::Label,
    img_startup: nwg::ImageFrame,
    hdr_startup: nwg::Label,
    chk_logon: nwg::CheckBox,
    chk_on_logon: nwg::CheckBox,
    img_install: nwg::ImageFrame,
    hdr_install: nwg::Label,
    txt_where: nwg::TextInput,
    btn_uninstall: nwg::Button,
    lbl_install_hint: nwg::Label,
}

fn build_ui(ui: &mut Ui) -> Result<(), nwg::NwgError> {
    use kit::{check, glyph_frame, icon_button, icon_from_ico, label, px};
    ui.fonts.build()?;
    ui.g = Glyphs::load();
    let tray_px = unsafe { GetSystemMetrics(49 /* SM_CXSMICON */) }.max(16) as u32;
    ui.app_icon = icon_from_ico(kit::APP_ICO, px(32));
    ui.tray_on = icon_from_ico(kit::ON_ICO, tray_px);
    ui.tray_off = icon_from_ico(kit::OFF_ICO, tray_px);
    ui.tray_busy = icon_from_ico(kit::BUSY_ICO, tray_px);
    ui.big_on = icon_from_ico(kit::ON_ICO, px(40));
    ui.big_off = icon_from_ico(kit::OFF_ICO, px(40));
    ui.big_busy = icon_from_ico(kit::BUSY_ICO, px(40));
    let (f, g) = (&ui.fonts, &ui.g);

    nwg::Window::builder()
        .size((560, 522))
        .position((360, 120))
        .title(APP)
        .icon(Some(&ui.app_icon))
        .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::MINIMIZE_BOX | nwg::WindowFlags::RESIZABLE)
        .build(&mut ui.window)?;
    let w = &ui.window;

    nwg::TrayNotification::builder()
        .parent(w)
        .icon(Some(&ui.tray_off))
        .tip(Some("Sidestep: off"))
        .build(&mut ui.tray)?;
    nwg::Menu::builder().popup(true).parent(w).build(&mut ui.tray_menu)?;
    nwg::MenuItem::builder().text("Bypass on").parent(&ui.tray_menu).build(&mut ui.mi_toggle)?;
    nwg::MenuItem::builder().text("Open Sidestep").parent(&ui.tray_menu).build(&mut ui.mi_open)?;
    nwg::MenuItem::builder().text("Find TTL now").parent(&ui.tray_menu).build(&mut ui.mi_tune)?;
    nwg::MenuSeparator::builder().parent(&ui.tray_menu).build(&mut ui.mi_sep)?;
    nwg::MenuItem::builder().text("Exit").parent(&ui.tray_menu).build(&mut ui.mi_exit)?;
    nwg::Notice::builder().parent(w).build(&mut ui.notice)?;

    // ---- header
    nwg::ImageFrame::builder().icon(Some(&ui.big_off)).position((20, 18)).size((40, 40)).parent(w).build(&mut ui.img_state)?;
    label(&mut ui.lbl_head, w, "Bypass is off", (74, 12), (330, 32), &f.head)?;
    label(&mut ui.lbl_sub, w, "Nothing is filtering.", (75, 46), (330, 20), &f.small)?;
    icon_button(&mut ui.btn_toggle, w, "Turn on", &g.power, (420, 16), (120, 46))?;
    ui.btn_toggle.set_font(Some(&f.bold));
    nwg::ProgressBar::builder()
        .flags(nwg::ProgressBarFlags::MARQUEE)
        .position((20, 72))
        .size((520, 5))
        .parent(w)
        .build(&mut ui.pb_busy)?;
    ui.pb_busy.set_marquee(true, 25);
    nwg::ProgressBar::builder().flags(nwg::ProgressBarFlags::empty()).position((20, 72)).size((520, 5)).parent(w).build(&mut ui.pb_steps)?;
    ui.band_colours.set(BAND_OFF);
    ui.band = Some(kit::state_band(&w.handle, BAND_H, ui.band_colours.clone(), &ui.lbl_head.handle)?);

    // ---- tabs (with glyphs)
    nwg::TabsContainer::builder().position((12, 84)).size((536, 426)).parent(w).build(&mut ui.tabs)?;
    nwg::ImageList::builder().size((px(16) as i32, px(16) as i32)).build(&mut ui.tab_icons)?;
    for icon in [&g.status, &g.flags, &g.log, &g.settings] {
        ui.tab_icons.add_icon(icon);
    }
    ui.tabs.set_image_list(Some(&ui.tab_icons));
    nwg::Tab::builder().text("Status").parent(&ui.tabs).build(&mut ui.tab_status)?;
    nwg::Tab::builder().text("Flags").parent(&ui.tabs).build(&mut ui.tab_flags)?;
    nwg::Tab::builder().text("Log").parent(&ui.tabs).build(&mut ui.tab_log)?;
    nwg::Tab::builder().text("Settings").parent(&ui.tabs).build(&mut ui.tab_settings)?;
    for (i, page) in [&ui.tab_status, &ui.tab_flags, &ui.tab_log, &ui.tab_settings].iter().enumerate() {
        page.set_image_index(Some(i as i32));
        ui.page_hooks.push(kit::white_statics(&page.handle, i)?);
    }

    // ---- Status
    let t = &ui.tab_status;
    let row_icons = [&g.engine, &g.pid, &g.clock, &g.ttl, &g.flag, &g.check, &g.chart];
    let mut y = 16;
    for (i, (name, _)) in ROWS.iter().enumerate() {
        let h = if i == R_FLAGS { 44 } else { 22 };
        let (mut gl, mut n, mut v) = (nwg::ImageFrame::default(), nwg::Label::default(), nwg::Label::default());
        glyph_frame(&mut gl, t, row_icons[i], (16, y + 2))?;
        label(&mut n, t, name, (42, y + 1), (104, 22), &f.small)?;
        let vx = if i == R_CHECK { 174 } else { 150 };
        label(&mut v, t, "-", (vx, y), (500 - vx, h), &f.normal)?;
        if i == R_CHECK {
            glyph_frame(&mut ui.img_check, t, &g.pending, (150, y + 2))?;
        }
        ui.row_glyphs.push(gl);
        ui.row_names.push(n);
        ui.row_values.push(v);
        y += h + 8;
    }
    icon_button(&mut ui.btn_check, t, "Check now", &g.check, (16, 304), (132, 34))?;
    icon_button(&mut ui.btn_tune, t, "Find TTL now", &g.tune, (156, 304), (152, 34))?;
    icon_button(&mut ui.btn_status_help, t, "Help", &g.help, (404, 304), (96, 34))?;

    // ---- Flags
    let t = &ui.tab_flags;
    nwg::TextInput::builder().position((16, 14)).size((484, 30)).font(Some(&f.mono)).parent(t).build(&mut ui.txt_flags)?;
    icon_button(&mut ui.btn_apply, t, "Apply", &g.apply, (16, 52), (100, 32))?;
    icon_button(&mut ui.btn_defaults, t, "Restore defaults", &g.undo, (124, 52), (170, 32))?;
    glyph_frame(&mut ui.img_ref, t, &g.help, (16, 101))?;
    label(&mut ui.lbl_ref, t, "All goodbyedpi flags. Select one to see what it does.", (40, 98), (460, 22), &f.bold)?;
    nwg::ListView::builder()
        .list_style(nwg::ListViewStyle::Detailed)
        .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT)
        .flags(
            nwg::ListViewFlags::VISIBLE
                | nwg::ListViewFlags::TAB_STOP
                | nwg::ListViewFlags::SINGLE_SELECTION
                | nwg::ListViewFlags::ALWAYS_SHOW_SELECTION,
        )
        .position((16, 124))
        .size((484, 170))
        .parent(t)
        .build(&mut ui.lst_flags)?;
    for (i, (title, width)) in [("Used", 46), ("Flag", 126), ("Group", 78), ("What it does", 212)].iter().enumerate() {
        ui.lst_flags.insert_column(nwg::InsertListViewColumn {
            index: Some(i as i32),
            fmt: None,
            width: Some(*width),
            text: Some(title.to_string()),
        });
    }
    for info in flags::ALL {
        let flag = match info.value {
            flags::Value::Required => format!("{} <value>", info.flag),
            flags::Value::Optional => format!("{} [value]", info.flag),
            flags::Value::None => info.flag.to_string(),
        };
        ui.lst_flags.insert_items_row(None, &[String::new(), flag, info.group.to_string(), info.text.to_string()]);
    }
    label(&mut ui.lbl_flag_detail, t, "", (16, 302), (360, 58), &f.small)?;
    icon_button(&mut ui.btn_flag_toggle, t, "Add", &g.plus, (384, 304), (116, 32))?;
    ui.btn_flag_toggle.set_enabled(false);

    // ---- Log
    let t = &ui.tab_log;
    for (tb, wrap) in [(&mut ui.console_wrap, true), (&mut ui.console_nowrap, false)] {
        let mut fl = nwg::TextBoxFlags::VSCROLL | nwg::TextBoxFlags::AUTOVSCROLL;
        if !wrap {
            fl |= nwg::TextBoxFlags::HSCROLL | nwg::TextBoxFlags::AUTOHSCROLL;
        }
        nwg::TextBox::builder().position((16, 14)).size((484, 296)).readonly(true).font(Some(&f.mono)).flags(fl).parent(t).build(tb)?;
        tb.set_limit(1_000_000);
    }
    check(&mut ui.chk_wrap, t, "Wrap long lines", (16, 322), (240, 26))?;
    icon_button(&mut ui.btn_clear_log, t, "Clear", &g.clear, (400, 318), (100, 32))?;

    // ---- Settings
    let t = &ui.tab_settings;
    glyph_frame(&mut ui.img_detect, t, &g.shield, (16, 15))?;
    label(&mut ui.hdr_detect, t, "Block detection", (40, 12), (460, 22), &f.bold)?;
    glyph_frame(&mut ui.img_sites, t, &g.globe, (40, 47))?;
    label(&mut ui.lbl_sites, t, "Test sites", (62, 44), (60, 22), &f.normal)?;
    nwg::TextInput::builder().position((124, 40)).size((376, 28)).parent(t).build(&mut ui.txt_sites)?;
    check(&mut ui.chk_autotune, t, "Search for a working TTL when a check is blocked", (124, 74), (376, 24))?;
    label(&mut ui.lbl_detect_hint, t, "", (124, 100), (376, 20), &f.small)?;

    glyph_frame(&mut ui.img_startup, t, &g.rocket, (16, 139))?;
    label(&mut ui.hdr_startup, t, "Startup", (40, 136), (460, 22), &f.bold)?;
    check(&mut ui.chk_logon, t, "Launch Sidestep at logon (tray icon always visible)", (40, 164), (460, 24))?;
    check(&mut ui.chk_on_logon, t, "...and turn the bypass on when it launches", (60, 190), (440, 24))?;

    glyph_frame(&mut ui.img_install, t, &g.folder, (16, 233))?;
    label(&mut ui.hdr_install, t, "Installation", (40, 230), (460, 22), &f.bold)?;
    nwg::TextInput::builder().position((40, 258)).size((322, 28)).readonly(true).parent(t).build(&mut ui.txt_where)?;
    icon_button(&mut ui.btn_uninstall, t, "Uninstall", &g.trash, (370, 257), (130, 30))?;
    label(&mut ui.lbl_install_hint, t, "", (40, 294), (460, 40), &f.small)?;

    // ---- tooltips
    let mut tips = nwg::Tooltip::builder();
    for (i, (_, tip)) in ROWS.iter().enumerate() {
        tips = tips.register(&ui.row_names[i], tip);
    }
    tips = tips
        .register(&ui.lbl_sites, "Sites used to detect a block. Plain names or full links, comma-separated.")
        .register(&ui.chk_autotune, "When a check is blocked, restart goodbyedpi with --set-ttl 1..12 until one passes, then save it.")
        .register(&ui.chk_logon, "Adds a 'Sidestep' task in Task Scheduler. It only starts the tray app.")
        .register(&ui.btn_check, "Send a TLS hello to each test site now (works while off too).")
        .register(&ui.btn_tune, "Find a working --set-ttl now (bypass must be on).")
        .register(&ui.lst_flags, "Double-click a flag to add or remove it.");
    tips.build(&mut ui.tips)?;

    // the space below each label's text: band colour in the header, white on the tab pages
    let mut id = 0;
    for l in [&ui.lbl_head, &ui.lbl_sub] {
        let c = ui.band_colours.clone();
        ui.page_hooks.push(kit::label_frame_fill(&l.handle, id, move || c.get().0)?);
        id += 1;
    }
    let page_labels = ui.row_names.iter().chain(ui.row_values.iter()).map(|l| &l.handle).chain([
        &ui.lbl_ref.handle,
        &ui.lbl_flag_detail.handle,
        &ui.hdr_detect.handle,
        &ui.lbl_sites.handle,
        &ui.lbl_detect_hint.handle,
        &ui.hdr_startup.handle,
        &ui.hdr_install.handle,
        &ui.lbl_install_hint.handle,
        &ui.txt_flags.handle,
        &ui.txt_sites.handle,
        &ui.txt_where.handle,
    ]);
    let mut hooks = Vec::new();
    for l in page_labels {
        hooks.push(kit::label_frame_fill(l, id, || 0x00FF_FFFF)?);
        id += 1;
    }
    ui.page_hooks.extend(hooks);
    Ok(())
}

// ----------------------------------------------------------------- state

struct St {
    cfg: Config,
    desired_on: bool,
    probing: bool,
    probe_manual: bool,
    tuning: bool,
    tune_step: (u8, usize, usize),
    verify: Verify,
    announce_pass: bool,
    last_check: Option<(String, Verdict)>,
    checks_ok: u32,
    checks_blocked: u32,
    next_check: Option<Instant>,
    log: VecDeque<String>,
    log_dirty: bool,
    flag_marks: Vec<bool>,
}

struct App {
    ui: Ui,
    st: RefCell<St>,
    engine: Arc<Mutex<Engine>>,
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    notice: nwg::NoticeSender,
    cancel_tune: RefCell<Arc<AtomicBool>>,
    gdpi: PathBuf,
    mode: setup::Mode,
    shown_view: Cell<Option<View>>,
    shown_check: Cell<Option<Option<Verdict>>>,
    shown_tip: RefCell<String>,
    /// client size the layout was last done for
    laid_out: Cell<(u32, u32)>,
    /// Preview builds only: render this view without a real engine.
    preview_view: Option<View>,
}

impl App {
    // ---------- logging
    fn log(&self, s: impl AsRef<str>) {
        let mut st = self.st.borrow_mut();
        for line in s.as_ref().lines() {
            st.log.push_back(format!("[{}] {}", now_hms(), line));
        }
        while st.log.len() > LOG_LINES {
            st.log.pop_front();
        }
        st.log_dirty = true;
    }

    fn console(&self) -> &nwg::TextBox {
        if self.st.borrow().cfg.log_wrap {
            &self.ui.console_wrap
        } else {
            &self.ui.console_nowrap
        }
    }

    fn flush_log(&self) {
        let text = {
            let mut st = self.st.borrow_mut();
            if !st.log_dirty {
                return;
            }
            st.log_dirty = false;
            st.log.iter().cloned().collect::<Vec<_>>().join("\r\n")
        };
        let c = self.console();
        c.set_text(&text);
        c.scroll_lastline();
    }

    fn apply_wrap(&self) {
        let wrap = self.st.borrow().cfg.log_wrap;
        self.ui.console_wrap.set_visible(wrap);
        self.ui.console_nowrap.set_visible(!wrap);
        self.st.borrow_mut().log_dirty = true;
        self.flush_log();
    }

    fn balloon(&self, title: &str, text: &str, warn: bool) {
        let flags = if warn { nwg::TrayNotificationFlags::WARNING_ICON } else { nwg::TrayNotificationFlags::INFO_ICON };
        self.ui.tray.show(text, Some(title), Some(flags), None);
    }

    fn info(&self, title: &str, text: &str) {
        nwg::modal_info_message(&self.ui.window, title, text);
    }

    // ---------- flags tab
    fn selected_flag(&self) -> Option<&'static flags::FlagInfo> {
        self.ui.lst_flags.selected_item().and_then(|i| flags::ALL.get(i))
    }

    /// Re-marks flags in use (✓) and updates the detail pane.
    fn refresh_flags_tab(&self) {
        let text = self.ui.txt_flags.text();
        let marks: Vec<bool> = flags::ALL.iter().map(|i| flags::uses(&text, i.flag)).collect();
        let old = std::mem::replace(&mut self.st.borrow_mut().flag_marks, marks.clone());
        for (row, &m) in marks.iter().enumerate() {
            if old.get(row) != Some(&m) {
                self.ui.lst_flags.update_item(
                    row,
                    nwg::InsertListViewItem { index: Some(row as i32), column_index: 0, text: Some(if m { "yes" } else { "" }.into()), image: None },
                );
            }
        }
        match self.selected_flag() {
            Some(info) => {
                let used = flags::uses(&text, info.flag);
                let value = match (info.value, info.example) {
                    (flags::Value::Required, ex) if !ex.is_empty() => format!(" (adds \"{} {ex}\"; edit the value in the box)", info.flag),
                    (flags::Value::Optional, _) => " (value optional)".to_string(),
                    _ => String::new(),
                };
                set_if(&self.ui.lbl_flag_detail, &format!("{} ({})\n{}{}", info.flag, info.group, info.text, value));
                self.ui.btn_flag_toggle.set_enabled(true);
                let (label, icon) = if used { ("Remove", &self.ui.g.minus) } else { ("Add", &self.ui.g.plus) };
                if kit::set_button_text(&self.ui.btn_flag_toggle, label) {
                    self.ui.btn_flag_toggle.set_icon(Some(icon));
                }
            }
            None => {
                set_if(&self.ui.lbl_flag_detail, "\"Used\" marks the flags in your line. Changes apply when you press Apply or Turn on.");
                self.ui.btn_flag_toggle.set_enabled(false);
            }
        }
    }

    fn toggle_selected_flag(&self) {
        let Some(info) = self.selected_flag() else { return };
        let text = self.ui.txt_flags.text();
        let (new, verb) =
            if flags::uses(&text, info.flag) { (flags::remove(&text, info.flag), "Removed") } else { (flags::add(&text, info.flag), "Added") };
        self.ui.txt_flags.set_text(&new);
        self.log(format!("{verb} {} in the flags box. Press Apply to use it.", info.flag));
        self.refresh_flags_tab();
        self.flush_log();
    }

    // ---------- on / off
    fn toggle(&self) {
        let running = self.engine.lock().unwrap().is_running();
        let desired = self.st.borrow().desired_on;
        if running || desired {
            self.turn_off("turned off by you");
        } else {
            self.turn_on();
        }
    }

    fn handle_strays(&self) {
        let ours = self.engine.lock().unwrap().pid();
        let strays: Vec<u32> = engine::find_goodbyedpi_pids().into_iter().filter(|p| Some(*p) != ours).collect();
        if strays.is_empty() {
            return;
        }
        self.log(format!(
            "WARNING: goodbyedpi.exe is running OUTSIDE Sidestep (PID {strays:?}). It filters traffic regardless of this toggle."
        ));
        self.flush_log();
        let text = format!(
            "goodbyedpi.exe (PID {strays:?}) was started by something else, for example a .cmd script or the old DPI Toggle.\n\n\
             It bypasses the DPI no matter what Sidestep shows, and two copies can conflict.\n\n\
             Terminate it now?"
        );
        if ask_yes_no(Some(&self.ui.window), "Another goodbyedpi is running", &text) {
            for pid in strays {
                let ok = engine::kill_pid(pid);
                self.log(format!("  PID {pid}: {}", if ok { "terminated" } else { "could not terminate" }));
            }
        } else {
            self.log("  left running (your choice).");
        }
    }

    fn start_engine(&self, flags_text: &str) -> bool {
        let args = flags::tokenize(flags_text);
        let res = self.engine.lock().unwrap().start(&self.gdpi, &args, &self.tx, self.notice);
        let mut st = self.st.borrow_mut();
        st.verify = Verify::Unknown;
        st.next_check = None;
        drop(st);
        match res {
            Ok(pid) => {
                self.log(format!("> goodbyedpi.exe {}   (PID {pid})", flags::join(&args)));
                self.st.borrow_mut().announce_pass = true;
                true
            }
            Err(e) => {
                self.log(format!("ERROR: {e}"));
                self.balloon("Sidestep could not start goodbyedpi", &e, true);
                false
            }
        }
    }

    fn turn_on(&self) {
        let text = self.ui.txt_flags.text().trim().to_string();
        {
            let mut st = self.st.borrow_mut();
            st.cfg.flags = text.clone();
            let _ = st.cfg.save();
        }
        self.handle_strays();
        let ok = self.start_engine(&text);
        self.st.borrow_mut().desired_on = ok;
        self.refresh();
    }

    fn cancel_tuner(&self) {
        self.cancel_tune.borrow().store(true, Ordering::SeqCst);
    }

    fn turn_off(&self, why: &str) {
        self.cancel_tuner();
        self.st.borrow_mut().desired_on = false;
        let msg = self.engine.lock().unwrap().stop();
        self.log(msg.unwrap_or_else(|| "goodbyedpi was not running".into()));
        let still = self.engine.lock().unwrap().is_running();
        self.log(if still { "ERROR: goodbyedpi still running after stop!".to_string() } else { format!("Off ({why}).") });
        self.st.borrow_mut().verify = Verify::Unknown;
        self.refresh();
    }

    fn apply_flags(&self) {
        let text = self.ui.txt_flags.text().trim().to_string();
        {
            let mut st = self.st.borrow_mut();
            st.cfg.flags = text.clone();
            let _ = st.cfg.save();
        }
        self.log(format!("Flags saved: {}", if text.is_empty() { "(none)" } else { &text }));
        let running = self.engine.lock().unwrap().is_running();
        if running {
            self.cancel_tuner();
            self.log("Restarting goodbyedpi with the new flags...");
            let ok = self.start_engine(&text);
            self.st.borrow_mut().desired_on = ok;
        }
        self.refresh();
    }

    // ---------- checks & TTL search
    fn sites(&self) -> Vec<String> {
        let text = self.ui.txt_sites.text();
        let mut st = self.st.borrow_mut();
        if st.cfg.sites != text {
            st.cfg.sites = text.clone();
            let _ = st.cfg.save();
        }
        probe::parse_sites(&text)
    }

    fn start_probe(&self, manual: bool) {
        let sites = self.sites();
        if sites.is_empty() {
            self.log("No valid test sites. Add one in Settings, for example epicgames.com.");
            return;
        }
        {
            let mut st = self.st.borrow_mut();
            if st.probing || st.tuning {
                return;
            }
            st.probing = true;
            st.probe_manual = manual;
        }
        self.log(format!("Checking {} ...", sites.join(", ")));
        let (tx, n) = (self.tx.clone(), self.notice);
        std::thread::spawn(move || {
            let (verdict, results) = probe::probe_all(&sites);
            let lines = results.iter().map(|r| format!("  {:<26} {:?}: {}", r.host, r.verdict, r.detail)).collect();
            let _ = tx.send(Ev::Probe { verdict, lines });
            n.notice();
        });
    }

    fn on_probe(&self, verdict: Verdict, lines: Vec<String>) {
        for l in &lines {
            self.log(l);
        }
        let running = self.engine.lock().unwrap().is_running();
        let (desired, autotune, announce) = {
            let mut st = self.st.borrow_mut();
            st.probing = false;
            st.probe_manual = false;
            st.next_check = Some(Instant::now() + Duration::from_secs(st.cfg.check_secs));
            let t = now_hms();
            match verdict {
                Verdict::Pass => {
                    st.checks_ok += 1;
                    st.last_check = Some((format!("passed at {t}"), verdict));
                }
                Verdict::Blocked => {
                    st.checks_blocked += 1;
                    st.last_check = Some((format!("blocked at {t}"), verdict));
                }
                Verdict::Unreachable => st.last_check = Some((format!("sites unreachable at {t}"), verdict)),
            }
            let announce = running && verdict == Verdict::Pass && st.announce_pass;
            if running {
                st.verify = match verdict {
                    Verdict::Pass => Verify::Passed,
                    Verdict::Blocked => Verify::Blocked,
                    Verdict::Unreachable => Verify::Unreachable,
                };
                st.announce_pass = false;
            }
            (st.desired_on, st.cfg.autotune, announce)
        };
        match verdict {
            Verdict::Pass => self.log(if running { "Result: passed. The DPI is not blocking." } else { "Result: passed (bypass is off)." }),
            Verdict::Unreachable => self.log("Result: sites unreachable at the TCP level. This is a network or IP problem, not the SNI filter, so no TTL search."),
            Verdict::Blocked if !running => self.log("Result: BLOCKED (bypass is off)."),
            Verdict::Blocked if desired && autotune => {
                self.log("Result: BLOCKED with the current flags. Starting TTL search.");
                self.start_tune(false);
            }
            Verdict::Blocked => self.log("Result: BLOCKED. TTL search is off: click 'Find TTL now' or change the flags."),
        }
        if announce && !self.ui.window.visible() {
            self.balloon(APP, "Bypass is on. The test sites pass.", false);
        }
        self.refresh();
    }

    fn start_tune(&self, manual: bool) {
        let running = self.engine.lock().unwrap().is_running();
        if !running {
            self.log("TTL search needs the bypass on first.");
            self.refresh();
            return;
        }
        let sites = self.sites();
        if sites.is_empty() {
            self.log("No valid test sites to tune against.");
            return;
        }
        let (base, min, max) = {
            let mut st = self.st.borrow_mut();
            if st.tuning || (manual && st.probing) {
                drop(st);
                self.log("A check or tune is already running.");
                return;
            }
            st.tuning = true;
            st.tune_step = (0, 0, 0);
            st.verify = Verify::Unknown;
            (st.cfg.flags.clone(), st.cfg.ttl_min, st.cfg.ttl_max)
        };
        let prev = flags::get_ttl(&base);
        let mut cands: Vec<u8> = (min..=max).collect();
        if let Some(p) = prev {
            // nearest to the last good value first; skip it if it just failed
            if !manual {
                cands.retain(|&x| x != p);
            }
            cands.sort_by_key(|&x| ((x as i16 - p as i16).abs(), x));
        }
        self.log(format!("TTL search: will try --set-ttl {cands:?} against {}", sites.join(", ")));

        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel_tune.borrow_mut() = cancel.clone();
        let (eng, exe, tx, n) = (self.engine.clone(), self.gdpi.clone(), self.tx.clone(), self.notice);
        std::thread::spawn(move || tune_worker(eng, exe, base, cands, sites, cancel, tx, n));
        self.refresh();
    }

    fn on_tune_done(&self, res: Result<(u8, String), String>) {
        let mut balloon = None;
        {
            let mut st = self.st.borrow_mut();
            st.tuning = false;
            st.next_check = Some(Instant::now() + Duration::from_secs(st.cfg.check_secs));
            match &res {
                Ok((_, f)) => {
                    st.cfg.flags = f.clone();
                    st.verify = Verify::Passed;
                    st.announce_pass = false;
                    let _ = st.cfg.save();
                }
                Err(e) if e == "cancelled" => st.verify = Verify::Unknown,
                Err(e) if e.contains("unreachable") => st.verify = Verify::Unreachable,
                Err(_) => st.verify = Verify::Blocked,
            }
        }
        match res {
            Ok((ttl, f)) => {
                self.ui.txt_flags.set_text(&f);
                self.refresh_flags_tab();
                self.log(format!("TTL search: TTL {ttl} passes. Saved flags: {f}"));
                balloon = Some((format!("TTL search found TTL {ttl}. Bypass is on."), false));
            }
            Err(e) => {
                self.log(format!("TTL search: {e}"));
                if e != "cancelled" {
                    balloon = Some((format!("TTL search: {e}"), true));
                }
            }
        }
        let running = self.engine.lock().unwrap().is_running();
        if !running && self.st.borrow().desired_on {
            self.st.borrow_mut().desired_on = false;
            self.log("Bypass is off (goodbyedpi is not running).");
        }
        if let Some((text, warn)) = balloon {
            self.balloon(APP, &text, warn);
        }
        self.refresh();
    }

    // ---------- periodic
    fn on_tick(&self) {
        let (exited, confirmed_now) = {
            let mut e = self.engine.lock().unwrap();
            let exited = e.poll();
            let mut c = false;
            if e.is_running() && !e.confirmed && e.started_at.map_or(false, |t| t.elapsed() >= Duration::from_millis(1500)) {
                e.confirmed = true;
                c = true;
            }
            (exited, c)
        };
        let tuning = self.st.borrow().tuning;
        if let Some(msg) = exited {
            self.log(&msg);
            if !tuning {
                {
                    let mut st = self.st.borrow_mut();
                    st.desired_on = false;
                    st.verify = Verify::Unknown;
                }
                self.log("Bypass is off. See goodbyedpi's output above for the reason.");
                self.balloon("goodbyedpi stopped", "goodbyedpi exited unexpectedly. Bypass is off.", true);
            }
        }
        if confirmed_now && !tuning {
            self.log("goodbyedpi is up. Checking whether the test sites pass...");
        }
        let due = {
            let st = self.st.borrow();
            st.desired_on
                && !st.probing
                && !st.tuning
                && (st.verify == Verify::Unknown || st.next_check.map_or(true, |t| Instant::now() >= t))
        };
        if due && self.engine.lock().unwrap().confirmed {
            self.start_probe(false);
        }
        self.refresh();
    }

    fn drain(&self) {
        let events: Vec<Ev> = self.rx.try_iter().collect();
        for ev in events {
            match ev {
                Ev::Tick => self.on_tick(),
                Ev::Log(s) => self.log(s),
                Ev::GdpiLine(gen, line) => {
                    let mut e = self.engine.lock().unwrap();
                    if gen == e.generation && line.contains("Filter activated") {
                        e.confirmed = true;
                    }
                    drop(e);
                    self.log(format!("  gdpi| {line}"));
                }
                Ev::GdpiEof(_) => {} // the tick's poll() reports the exit
                Ev::Probe { verdict, lines } => self.on_probe(verdict, lines),
                Ev::TuneStep(ttl, i, n, f) => {
                    {
                        let mut st = self.st.borrow_mut();
                        st.tune_step = (ttl, i, n);
                        st.verify = Verify::Unknown;
                    }
                    self.log(format!("TTL search {i}/{n}: trying TTL {ttl}: {f}"));
                }
                Ev::TuneDone(r) => self.on_tune_done(r),
            }
        }
        self.refresh();
    }

    // ---------- view
    fn view(&self, running: bool, confirmed: bool) -> View {
        if let Some(v) = self.preview_view {
            return v;
        }
        let st = self.st.borrow();
        if !running {
            View::Off
        } else if st.tuning {
            View::Tuning
        } else if !confirmed {
            View::Starting
        } else {
            match st.verify {
                Verify::Unknown => View::Checking,
                Verify::Passed => View::On,
                Verify::Blocked => View::Blocked,
                Verify::Unreachable => View::Unreachable,
            }
        }
    }

    fn refresh(&self) {
        let (running, confirmed, pid, uptime, active) = {
            let e = self.engine.lock().unwrap();
            (e.is_running(), e.confirmed, e.pid(), e.started_at.map(|t| t.elapsed()), e.active_flags.clone())
        };
        let view = self.view(running, confirmed);
        let shows_running = view != View::Off;
        let (desired, probing_manual, (ttl_try, step, steps), last_check, ok, blocked, autotune) = {
            let st = self.st.borrow();
            (
                st.desired_on,
                st.probing && st.probe_manual,
                st.tune_step,
                st.last_check.clone(),
                st.checks_ok,
                st.checks_blocked,
                st.cfg.autotune,
            )
        };
        let ui = &self.ui;
        let ttl = flags::get_ttl(&active);
        let up = uptime.map_or(String::new(), hms);

        // ---- header
        let (head, sub) = match view {
            View::Off => ("Bypass is off".to_string(), "Nothing is filtering. goodbyedpi is not running.".to_string()),
            View::Starting => ("Starting...".to_string(), "goodbyedpi launched. Opening its packet filter.".to_string()),
            View::Checking => ("Checking...".to_string(), "goodbyedpi is running. Testing whether the sites pass.".to_string()),
            View::Tuning => (
                "Searching for a TTL...".to_string(),
                if steps > 0 { format!("Trying TTL {ttl_try} ({step} of {steps}). Sites may not load yet.") } else { "Preparing...".into() },
            ),
            View::On => (
                "Bypass is on".to_string(),
                format!(
                    "Sites pass{}{}{}",
                    if up.is_empty() { String::new() } else { format!(", up {up}") },
                    ttl.map_or(String::new(), |t| format!(", TTL {t}")),
                    if probing_manual { ", checking..." } else { "" }
                ),
            ),
            View::Blocked => (
                "On, but blocked".to_string(),
                if autotune { "The last check was blocked; a TTL search will retry.".into() } else { "The last check was blocked. Try Find TTL now or other flags.".into() },
            ),
            View::Unreachable => ("On, sites unreachable".to_string(), "Can't reach the test sites at all (network, not the DPI).".to_string()),
        };
        set_if(&ui.lbl_head, &head);
        set_if(&ui.lbl_sub, &sub);
        let btn = if shows_running || desired { "Turn off" } else { "Turn on" };
        kit::set_button_text(&ui.btn_toggle, btn);

        // progress: marquee while we don't know yet, steps while tuning
        let busy = matches!(view, View::Starting | View::Checking) || (probing_manual && view != View::Tuning);
        ui.pb_busy.set_visible(busy);
        ui.pb_steps.set_visible(view == View::Tuning && steps > 0);
        if view == View::Tuning && steps > 0 {
            ui.pb_steps.set_range(0..steps as u32);
            ui.pb_steps.set_pos(step as u32);
        }

        // ---- badge (header + tray) follows the real process
        if self.shown_view.get() != Some(view) {
            let (big, small) = match view {
                View::Off => (&ui.big_off, &ui.tray_off),
                View::On => (&ui.big_on, &ui.tray_on),
                _ => (&ui.big_busy, &ui.tray_busy),
            };
            ui.img_state.set_icon(Some(big));
            ui.tray.set_icon(small);
            ui.band_colours.set(match view {
                View::Off => BAND_OFF,
                View::On => BAND_ON,
                _ => BAND_BUSY,
            });
            kit::redraw_band(&ui.window, BAND_H);
            self.shown_view.set(Some(view));
        }
        ui.mi_toggle.set_checked(shows_running);
        // one short line; the full flags live on the Status tab
        let tip = match view {
            View::Off => "Sidestep: off".to_string(),
            View::Starting => "Sidestep: starting".to_string(),
            View::Checking => "Sidestep: checking".to_string(),
            View::Tuning => "Sidestep: searching for a TTL".to_string(),
            View::On => format!("Sidestep: on{}", ttl.map_or(String::new(), |t| format!(", TTL {t}"))),
            View::Blocked => "Sidestep: on, but blocked".to_string(),
            View::Unreachable => "Sidestep: on, sites unreachable".to_string(),
        };
        if *self.shown_tip.borrow() != tip {
            ui.tray.set_tip(&tip);
            *self.shown_tip.borrow_mut() = tip;
        }

        // ---- status tab
        let v = &ui.row_values;
        set_if(&v[R_ENGINE], if running { "goodbyedpi running" } else { "not running" });
        set_if(&v[R_PID], &pid.map_or("-".into(), |p| p.to_string()));
        set_if(&v[R_UPTIME], &uptime.map_or("-".into(), hms));
        set_if(&v[R_TTL], &if running { ttl.map_or("none (no --set-ttl)".into(), |t| t.to_string()) } else { "-".into() });
        let flags_shown = if active.is_empty() { "(goodbyedpi defaults)".to_string() } else { active.clone() };
        set_if(&v[R_FLAGS], &if running { flags_shown } else { "-".into() });
        set_if(&v[R_CHECK], last_check.as_ref().map_or("not yet", |(s, _)| s.as_str()));
        set_if(&v[R_COUNTS], &format!("{ok} passed, {blocked} blocked"));
        let mark = last_check.as_ref().map(|(_, v)| *v);
        if self.shown_check.get() != Some(mark) {
            ui.img_check.set_icon(Some(match mark {
                Some(Verdict::Pass) => &ui.g.ok,
                Some(_) => &ui.g.bad,
                None => &ui.g.pending,
            }));
            self.shown_check.set(Some(mark));
        }
        self.flush_log();
    }

    /// Lays the window out for its current size. Everything is placed for the default 560 x 522
    /// client area in build_ui; here, controls on the right edge move with the extra width (dw)
    /// and controls at the bottom with the extra height (dh), and text boxes and lists stretch.
    fn layout(&self) {
        let ui = &self.ui;
        let (w, h) = ui.window.size();
        if self.laid_out.get() == (w, h) {
            return;
        }
        self.laid_out.set((w, h));
        kit::set_redraw(&ui.window, false);
        let (dw, dh) = ((w as i32 - 560).max(0), (h as i32 - 522).max(0));
        let sz = |x: i32, y: i32| (x.max(1) as u32, y.max(1) as u32);

        // header
        ui.btn_toggle.set_position(420 + dw, 16);
        let (x, y) = sz(330 + dw, 32);
        ui.lbl_head.set_size(x, y);
        let (x, y) = sz(330 + dw, 20);
        ui.lbl_sub.set_size(x, y);
        for pb in [&ui.pb_busy, &ui.pb_steps] {
            let (x, y) = sz(520 + dw, 5);
            pb.set_size(x, y);
        }
        let (x, y) = sz(536 + dw, 426 + dh);
        ui.tabs.set_size(x, y);

        // status
        for (i, v) in ui.row_values.iter().enumerate() {
            let vx = if i == R_CHECK { 174 } else { 150 };
            let (x, y) = sz(500 - vx + dw, if i == R_FLAGS { 44 } else { 22 });
            v.set_size(x, y);
        }

        // the action row sits at the bottom; Help on the right, like Clear, Add and Uninstall
        ui.btn_check.set_position(16, 304 + dh);
        ui.btn_tune.set_position(156, 304 + dh);
        ui.btn_status_help.set_position(404 + dw, 304 + dh);

        // flags
        let (x, y) = sz(484 + dw, 30);
        ui.txt_flags.set_size(x, y);
        let (x, y) = sz(460 + dw, 22);
        ui.lbl_ref.set_size(x, y);
        let (x, y) = sz(484 + dw, 170 + dh);
        ui.lst_flags.set_size(x, y);
        ui.lst_flags.set_column_width(3, kit::px((212 + dw) as u32) as isize);
        ui.lbl_flag_detail.set_position(16, 302 + dh);
        let (x, y) = sz(360 + dw, 58);
        ui.lbl_flag_detail.set_size(x, y);
        ui.btn_flag_toggle.set_position(384 + dw, 304 + dh);

        // log
        for tb in [&ui.console_wrap, &ui.console_nowrap] {
            let (x, y) = sz(484 + dw, 296 + dh);
            tb.set_size(x, y);
        }
        ui.chk_wrap.set_position(16, 322 + dh);
        ui.btn_clear_log.set_position(400 + dw, 318 + dh);

        // settings
        for l in [&ui.hdr_detect, &ui.hdr_startup, &ui.hdr_install] {
            let (x, y) = sz(460 + dw, 22);
            l.set_size(x, y);
        }
        let (x, y) = sz(376 + dw, 28);
        ui.txt_sites.set_size(x, y);
        let (x, y) = sz(376 + dw, 24);
        ui.chk_autotune.set_size(x, y);
        let (x, y) = sz(376 + dw, 20);
        ui.lbl_detect_hint.set_size(x, y);
        let (x, y) = sz(460 + dw, 24);
        ui.chk_logon.set_size(x, y);
        let (x, y) = sz(440 + dw, 24);
        ui.chk_on_logon.set_size(x, y);
        let (x, y) = sz(322 + dw, 28);
        ui.txt_where.set_size(x, y);
        ui.btn_uninstall.set_position(370 + dw, 257);
        let (x, y) = sz(460 + dw, 40);
        ui.lbl_install_hint.set_size(x, y);
        kit::set_redraw(&ui.window, true);
        kit::redraw_all(&ui.window);
    }

    fn show_window(&self) {
        self.ui.window.set_visible(true);
        self.ui.window.restore();
        self.ui.window.set_focus();
    }

    fn show_menu(&self) {
        let (x, y) = nwg::GlobalCursor::position();
        self.ui.tray_menu.popup(x, y);
    }

    fn exit(&self) {
        self.cancel_tuner();
        self.st.borrow_mut().desired_on = false;
        let _ = self.engine.lock().unwrap().stop();
        self.ui.tray.set_visibility(false);
        nwg::stop_thread_dispatch();
    }

    fn uninstall(&self) {
        let title = if self.mode.is_portable() { "Remove Sidestep data" } else { "Uninstall Sidestep" };
        if !ask_yes_no(Some(&self.ui.window), title, &setup::uninstall_summary(&self.mode)) {
            return;
        }
        self.cancel_tuner();
        self.st.borrow_mut().desired_on = false;
        let _ = self.engine.lock().unwrap().stop();
        self.refresh();
        let done = setup::uninstall(&self.mode);
        self.info("Sidestep removed", &format!("Done:\n- {}\n\nSidestep will now close.", done.join("\n- ")));
        setup::schedule_file_deletion(&self.mode);
        self.exit();
    }

    // ---------- autostart
    fn on_logon_changed(&self) {
        let want = checked(&self.ui.chk_logon);
        let res = if want { autostart::register(&self.mode.app_exe()) } else { autostart::unregister() };
        match res {
            Ok(()) if want => self.log("Launch at logon on: scheduled task 'Sidestep' starts the app at logon (see Task Scheduler)."),
            Ok(()) => self.log("Launch at logon off: scheduled task removed."),
            Err(e) => {
                self.log(format!("Launch at logon change failed: {e}"));
                self.info("Launch at logon", &format!("Could not change the logon task:\n{e}"));
                set_checked(&self.ui.chk_logon, autostart::is_registered());
            }
        }
        self.ui.chk_on_logon.set_enabled(autostart::is_registered());
        self.refresh();
    }

    // ---------- event routing
    fn on_button(&self, h: nwg::ControlHandle) {
        let ui = &self.ui;
        if h == ui.btn_toggle.handle {
            self.toggle();
        } else if h == ui.btn_apply.handle {
            self.apply_flags();
        } else if h == ui.btn_defaults.handle {
            ui.txt_flags.set_text(flags::DEFAULT_FLAGS);
            self.refresh_flags_tab();
            self.log("Default flags restored in the box (press Apply or Turn on to use them).");
            self.refresh();
        } else if h == ui.btn_flag_toggle.handle {
            self.toggle_selected_flag();
        } else if h == ui.btn_check.handle {
            self.start_probe(true);
            self.refresh();
        } else if h == ui.btn_tune.handle {
            self.start_tune(true);
        } else if h == ui.btn_clear_log.handle {
            {
                let mut st = self.st.borrow_mut();
                st.log.clear();
                st.log_dirty = true;
            }
            self.flush_log();
        } else if h == ui.chk_wrap.handle {
            {
                let mut st = self.st.borrow_mut();
                st.cfg.log_wrap = checked(&ui.chk_wrap);
                let _ = st.cfg.save();
            }
            self.apply_wrap();
        } else if h == ui.chk_autotune.handle {
            let mut st = self.st.borrow_mut();
            st.cfg.autotune = checked(&ui.chk_autotune);
            let _ = st.cfg.save();
        } else if h == ui.chk_logon.handle {
            self.on_logon_changed();
        } else if h == ui.chk_on_logon.handle {
            let mut st = self.st.borrow_mut();
            st.cfg.on_at_logon = checked(&ui.chk_on_logon);
            let _ = st.cfg.save();
        } else if h == ui.btn_uninstall.handle {
            self.uninstall();
        } else if h == ui.btn_status_help.handle {
            let (min, max, secs) = {
                let st = self.st.borrow();
                (st.cfg.ttl_min, st.cfg.ttl_max, st.cfg.check_secs)
            };
            let mut text: String = ROWS.iter().map(|(n, d)| format!("{n}\n    {d}\n\n")).collect();
            text.push_str(&format!(
                "Colours\n    Grey: off. Amber: running, but not confirmed to work yet. Green: on, and the last check passed.\n\n\
                 TTL search\n    Every {secs} s while the bypass is on, Sidestep runs a check. If it is blocked, goodbyedpi is \
                 restarted with --set-ttl {min} to {max}, nearest to the last working value first, until a check passes. \
                 The working flags are saved. If none pass, your previous flags are restored.\n\n\
                 Exiting\n    If Sidestep exits or crashes, goodbyedpi stops with it. No tray icon means no bypass."
            ));
            self.info("Help", &text);
        }
    }

    fn on_menu(&self, h: nwg::ControlHandle) {
        let ui = &self.ui;
        if h == ui.mi_toggle.handle {
            self.toggle();
        } else if h == ui.mi_open.handle {
            self.show_window();
        } else if h == ui.mi_tune.handle {
            self.start_tune(true);
        } else if h == ui.mi_exit.handle {
            self.exit();
        }
    }
}

// ----------------------------------------------------------------- tuner thread

#[allow(clippy::too_many_arguments)]
fn tune_worker(
    eng: Arc<Mutex<Engine>>,
    exe: PathBuf,
    base: String,
    cands: Vec<u8>,
    sites: Vec<String>,
    cancel: Arc<AtomicBool>,
    tx: Sender<Ev>,
    n: nwg::NoticeSender,
) {
    let send = |e: Ev| {
        let _ = tx.send(e);
        n.notice();
    };
    // Restart under the lock, checking `cancel` under the same lock: the UI sets
    // cancel *before* taking the lock to stop, so we can never resurrect goodbyedpi
    // after the user turned it off.
    let restart = |f: &str| -> Result<bool, String> {
        let mut g = eng.lock().unwrap();
        if cancel.load(Ordering::SeqCst) {
            return Ok(false);
        }
        g.start(&exe, &flags::tokenize(f), &tx, n).map(|_| true)
    };
    let cancelled = || cancel.load(Ordering::SeqCst);
    let restore = |why: String| {
        let msg = match restart(&base) {
            Ok(true) => format!("{why}; restored your previous flags"),
            Ok(false) => "cancelled".to_string(),
            Err(e) => format!("{why}; restoring previous flags FAILED: {e}"),
        };
        send(Ev::TuneDone(Err(msg)));
    };

    let total = cands.len();
    for (i, ttl) in cands.into_iter().enumerate() {
        if cancelled() {
            return send(Ev::TuneDone(Err("cancelled".into())));
        }
        let f = flags::with_ttl(&base, ttl);
        send(Ev::TuneStep(ttl, i + 1, total, f.clone()));
        match restart(&f) {
            Ok(false) => return send(Ev::TuneDone(Err("cancelled".into()))),
            Err(e) => {
                send(Ev::Log(format!("  could not start: {e}")));
                continue;
            }
            Ok(true) => {}
        }
        for _ in 0..15 {
            std::thread::sleep(Duration::from_millis(100));
            if cancelled() {
                return send(Ev::TuneDone(Err("cancelled".into())));
            }
        }
        {
            let mut g = eng.lock().unwrap();
            if let Some(m) = g.poll() {
                send(Ev::Log(format!("  {m}")));
            }
            if !g.is_running() {
                send(Ev::Log("  goodbyedpi did not stay up with these flags".into()));
                continue;
            }
            g.confirmed = true;
        }
        let (v, results) = probe::probe_all(&sites);
        if cancelled() {
            return send(Ev::TuneDone(Err("cancelled".into())));
        }
        for r in &results {
            send(Ev::Log(format!("  {:<26} {:?}: {}", r.host, r.verdict, r.detail)));
        }
        match v {
            Verdict::Pass => return send(Ev::TuneDone(Ok((ttl, f)))),
            Verdict::Unreachable => return restore("network unreachable during tuning (not a DPI block)".into()),
            Verdict::Blocked => {}
        }
    }
    restore("no TTL in range got past the DPI".into());
}

// ----------------------------------------------------------------- main

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |a: &str| args.iter().any(|x| x == a);
    let autostart_launch = has("--autostart");
    let uninstall_launch = has("--uninstall");

    // Preview build only (asInvoker manifest, never touches goodbyedpi or the
    // install location): renders the windows so the layout can be checked.
    // --preview-main <tab> [view]   view: 0 off 1 starting 2 checking 3 tuning 4 on 5 blocked
    #[cfg(feature = "preview")]
    let preview: Option<(usize, Option<View>)> = {
        if has("--preview-setup") {
            nwg::init().expect("init nwg");
            let _ = setup::run_setup(None);
            return;
        }
        args.iter().position(|a| a == "--preview-main").map(|i| {
            let tab = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let view = args.get(i + 2).and_then(|s| s.parse::<u8>().ok()).map(|v| match v {
                1 => View::Starting,
                2 => View::Checking,
                3 => View::Tuning,
                4 => View::On,
                5 => View::Blocked,
                _ => View::Off,
            });
            (tab, view)
        })
    };
    #[cfg(not(feature = "preview"))]
    let preview: Option<(usize, Option<View>)> = None;

    if preview.is_none() && !single_instance() {
        let _ = nwg::init();
        nwg::simple_message(
            APP,
            if uninstall_launch {
                "Sidestep is running. Exit it from its tray icon first (right-click it, then Exit), then uninstall again."
            } else {
                "Sidestep is already running. Look for its icon in the notification area of the taskbar."
            },
        );
        return;
    }
    nwg::init().expect("init nwg");

    let mut setup_notes = Vec::new();
    let mode = if preview.is_some() {
        setup::Mode::Portable(std::env::temp_dir().join("Sidestep-preview"))
    } else {
        // ---- where do our files live? Nothing is written before the user chooses.
        let detected = setup::detect();
        if uninstall_launch {
            // launched from Settings → Apps
            match detected {
                Ok(mode) => {
                    if ask_yes_no(None, "Uninstall Sidestep", &setup::uninstall_summary(&mode)) {
                        let done = setup::uninstall(&mode);
                        nwg::simple_message("Sidestep removed", &format!("Done:\n- {}", done.join("\n- ")));
                        setup::schedule_file_deletion(&mode);
                    }
                }
                Err(_) => {
                    nwg::simple_message(APP, "Sidestep is not set up on this PC. There is nothing to uninstall.");
                }
            }
            return;
        }
        match detected {
            Ok(m) => m,
            Err(prefill) => {
                if autostart_launch {
                    nwg::error_message(
                        APP,
                        "Sidestep was started at logon, but its files are missing.\n\
                         Open Sidestep.exe to set it up again (or untick 'Launch at logon').",
                    );
                    return;
                }
                match setup::run_setup(prefill) {
                    Some((m, notes)) => {
                        setup_notes = notes;
                        m
                    }
                    None => return, // cancelled: nothing written
                }
            }
        }
    };
    if preview.is_some() {
        let _ = std::fs::create_dir_all(mode.dir());
    }
    config::set_data_dir(mode.dir().to_path_buf());

    // Repair/update the engine files inside the approved folder only.
    let repaired = if preview.is_some() {
        Vec::new()
    } else {
        match setup::write_payload(mode.dir()) {
            Ok(r) => r,
            Err(e) => {
                nwg::error_message(APP, &format!("Could not prepare goodbyedpi:\n{e}"));
                return;
            }
        }
    };
    let gdpi = mode.dir().join("goodbyedpi.exe");

    let mut ui = Ui::default();
    if let Err(e) = build_ui(&mut ui) {
        nwg::error_message(APP, &format!("UI error: {e}"));
        return;
    }

    let cfg = Config::load();
    ui.txt_flags.set_text(&cfg.flags);
    ui.txt_sites.set_text(&cfg.sites);
    ui.lbl_detect_hint.set_text(&format!("Checks run every {} s while the bypass is on.", cfg.check_secs));
    set_checked(&ui.chk_autotune, cfg.autotune);
    set_checked(&ui.chk_on_logon, cfg.on_at_logon);
    set_checked(&ui.chk_wrap, cfg.log_wrap);
    let registered = preview.is_none() && autostart::is_registered();
    set_checked(&ui.chk_logon, registered);
    ui.chk_on_logon.set_enabled(registered);
    ui.txt_where.set_text(&kit::short_path(mode.dir()));
    if mode.is_portable() {
        ui.hdr_install.set_text("Portable mode");
        kit::set_button_text(&ui.btn_uninstall, "Remove data");
        ui.lbl_install_hint.set_text("Everything lives in this folder next to the exe. 'Remove data' deletes it and the logon task.");
    } else {
        ui.lbl_install_hint.set_text(
            "Uninstall removes Sidestep's files from this folder, the Start Menu shortcut, the logon task and the Settings > Apps entry.",
        );
    }
    let on_at_logon = cfg.on_at_logon;

    let (tx, rx) = channel();
    let notice = ui.notice.sender();
    let app = Rc::new(App {
        ui,
        st: RefCell::new(St {
            cfg,
            desired_on: false,
            probing: false,
            probe_manual: false,
            tuning: false,
            tune_step: (0, 0, 0),
            verify: Verify::Unknown,
            announce_pass: false,
            last_check: None,
            checks_ok: 0,
            checks_blocked: 0,
            next_check: None,
            log: VecDeque::new(),
            log_dirty: false,
            flag_marks: Vec::new(),
        }),
        engine: Arc::new(Mutex::new(Engine::default())),
        tx: tx.clone(),
        rx,
        notice,
        cancel_tune: RefCell::new(Arc::new(AtomicBool::new(false))),
        gdpi,
        mode,
        shown_view: Cell::new(None),
        shown_check: Cell::new(None),
        shown_tip: RefCell::new(String::new()),
        laid_out: Cell::new((0, 0)),
        preview_view: preview.and_then(|(_, v)| v),
    });

    let weak = Rc::downgrade(&app);
    let handler = nwg::full_bind_event_handler(&app.ui.window.handle, move |evt, data, handle| {
        use nwg::Event as E;
        let Some(app) = weak.upgrade() else { return };
        match evt {
            E::OnWindowClose if handle == app.ui.window.handle => {
                // closing the window only hides it; the tray icon stays = app still running
                if let nwg::EventData::OnWindowClose(d) = &data {
                    d.close(false);
                }
                app.ui.window.set_visible(false);
            }
            E::OnResize | E::OnWindowMaximize if handle == app.ui.window.handle => app.layout(),
            E::OnMinMaxInfo if handle == app.ui.window.handle => {
                // never smaller than the default layout
                if let nwg::EventData::OnMinMaxInfo(info) = &data {
                    info.set_min_size(576, 561);
                }
            }
            E::OnNotice if handle == app.ui.notice.handle => app.drain(),
            E::OnTextInput if handle == app.ui.txt_flags.handle => app.refresh_flags_tab(),
            E::OnListViewItemChanged if handle == app.ui.lst_flags.handle => app.refresh_flags_tab(),
            E::OnListViewDoubleClick if handle == app.ui.lst_flags.handle => app.toggle_selected_flag(),
            E::OnButtonClick => app.on_button(handle),
            E::OnMenuItemSelected => app.on_menu(handle),
            E::OnContextMenu if handle == app.ui.tray.handle => app.show_menu(),
            E::OnMousePress(nwg::MousePressEvent::MousePressLeftUp) if handle == app.ui.tray.handle => app.show_window(),
            _ => {}
        }
    });

    // 2 Hz heartbeat: liveness poll, start-up confirmation, scheduled checks, uptime.
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(500));
        if tx.send(Ev::Tick).is_err() {
            break;
        }
        notice.notice();
    });

    app.log(format!(
        "Sidestep {} ready ({} mode). goodbyedpi: {}",
        env!("CARGO_PKG_VERSION"),
        if app.mode.is_portable() { "portable" } else { "installed" },
        app.gdpi.display()
    ));
    for n in &setup_notes {
        app.log(n);
    }
    if !repaired.is_empty() {
        app.log(format!("Restored/updated in {}: {}", app.mode.dir().display(), repaired.join(", ")));
    }
    app.log("Nothing is filtering until you press Turn on.");
    app.apply_wrap();
    app.refresh_flags_tab();
    app.layout();

    if let Some((tab, _)) = preview {
        app.ui.tray.set_visibility(false);
        app.ui.tabs.set_selected_tab(tab);
        if tab == 1 {
            app.ui.lst_flags.select_item(3, true);
            app.refresh_flags_tab();
        }
        {
            let mut st = app.st.borrow_mut();
            st.tune_step = (3, 2, 11);
            st.last_check = Some(("passed at 05:40:28".into(), Verdict::Pass));
        }
        app.show_window();
        app.refresh();
        nwg::dispatch_thread_events();
        return;
    }

    if autostart_launch {
        app.log("Launched at logon.");
    } else {
        app.show_window();
    }
    app.refresh();
    app.handle_strays();
    if autostart_launch && on_at_logon {
        app.turn_on();
    }
    app.refresh();

    nwg::dispatch_thread_events();

    // Belt and braces: the job object would kill it anyway when we exit.
    let _ = app.engine.lock().unwrap().stop();
    nwg::unbind_event_handler(&handler);
}
