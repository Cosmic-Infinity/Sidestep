//! Shared application state and background supervisor for the Linux GUI.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::engine::{self, Engine};
use crate::flags;
use crate::probe::{self, SiteResult, Verdict};
use crate::proxy;
use crate::setup::Mode;
use crate::Ev;

pub const MAX_LOG_LINES: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verify {
    Unknown,
    Passed,
    Blocked,
    Unreachable,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Off,
    Starting,
    Checking,
    Tuning,
    On,
    Blocked,
    Unreachable,
}

pub enum UiCmd {
    Toggle,
    CheckNow,
    TuneNow,
    ApplyFlags(String),
    SaveConfig,
    Quit,
    ShowWindow,
}

pub struct AppState {
    pub config: Config,
    pub mode: Mode,
    pub view: View,
    pub verify: Verify,
    pub engine_pid: Option<u32>,
    pub started_at: Option<Instant>,
    pub active_flags: String,
    pub current_ttl: Option<u8>,
    pub last_check_detail: String,
    pub last_check_results: Vec<SiteResult>,
    pub checks_passed: usize,
    pub checks_blocked: usize,
    pub log_lines: VecDeque<String>,
    pub log_wrap: bool,
    pub probing: bool,
    pub tuning: bool,
    pub tuning_step: Option<u8>,
    pub window_visible: bool,
    pub has_tray: bool,
    pub should_exit: bool,
}

impl AppState {
    pub fn new(mode: Mode, config: Config) -> Self {
        let ttl = flags::get_ttl(&config.flags);
        let active = config.flags.clone();
        let log_wrap = config.log_wrap;
        Self {
            config,
            mode,
            view: View::Off,
            verify: Verify::Unknown,
            engine_pid: None,
            started_at: None,
            active_flags: active,
            current_ttl: ttl,
            last_check_detail: "No check run yet this session".into(),
            last_check_results: Vec::new(),
            checks_passed: 0,
            checks_blocked: 0,
            log_lines: VecDeque::with_capacity(MAX_LOG_LINES),
            log_wrap,
            probing: false,
            tuning: false,
            tuning_step: None,
            window_visible: true,
            has_tray: false,
            should_exit: false,
        }
    }

    pub fn add_log(&mut self, text: impl Into<String>) {
        if self.log_lines.len() >= MAX_LOG_LINES {
            self.log_lines.pop_front();
        }
        let now = now_hms();
        self.log_lines.push_back(format!("[{now}] {}", text.into()));
    }

    pub fn uptime_str(&self) -> String {
        match self.started_at {
            Some(t) => {
                let d = t.elapsed();
                let s = d.as_secs();
                format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
            }
            None => "none".into(),
        }
    }
}

pub fn is_debug() -> bool {
    std::env::var("SIDESTEP_DEBUG")
        .map(|v| v != "0")
        .unwrap_or(false)
        || std::env::args().any(|a| a == "--debug" || a == "-v" || a == "--verbose")
}

pub fn now_hms() -> String {
    let now = std::time::SystemTime::now();
    let d = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sec = d % 60;
    let min = (d / 60) % 60;
    let hr = (d / 3600) % 24;
    format!("{hr:02}:{min:02}:{sec:02}")
}

pub struct Supervisor {
    state: Arc<Mutex<AppState>>,
    cmd_rx: Receiver<UiCmd>,
    exe: PathBuf,
}

impl Supervisor {
    pub fn spawn(
        state: Arc<Mutex<AppState>>,
        cmd_rx: Receiver<UiCmd>,
        exe: PathBuf,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("sidestep-supervisor".into())
            .spawn(move || {
                let mut sup = Supervisor { state, cmd_rx, exe };
                sup.run();
            })
            .expect("failed to spawn supervisor thread")
    }

    fn run(&mut self) {
        let (tx, rx) = channel();
        let mut eng = Engine::default();
        let mut next_check = Instant::now() + Duration::from_secs(999999);

        // Clean up any stale engine instances from past runs
        for pid in engine::find_goodbyedpi_pids() {
            let _ = engine::kill_pid(pid);
        }
        proxy::clear_socks_proxy();

        {
            let mut st = self.state.lock().unwrap();
            st.add_log(format!(
                "Sidestep {} ready. Engine binary: {}",
                env!("CARGO_PKG_VERSION"),
                self.exe.display()
            ));
            st.add_log("Nothing is filtering until you press Turn on.");
        }

        while !self.state.lock().unwrap().should_exit {
            // 1. Process any incoming commands
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                match cmd {
                    UiCmd::Toggle => {
                        let is_running = eng.is_running();
                        if is_running {
                            eng.stop();
                            proxy::clear_socks_proxy();
                            let mut st = self.state.lock().unwrap();
                            st.view = View::Off;
                            st.verify = Verify::Unknown;
                            st.engine_pid = None;
                            st.started_at = None;
                            st.probing = false;
                            st.tuning = false;
                            st.add_log("Bypass is off. System proxy restored to Direct.");
                        } else {
                            self.start_engine(&mut eng, &tx);
                            next_check = Instant::now() + Duration::from_millis(500);
                        }
                    }
                    UiCmd::CheckNow => {
                        if eng.is_running() {
                            self.run_check(&eng);
                            let secs = self.state.lock().unwrap().config.check_secs;
                            next_check = Instant::now() + Duration::from_secs(secs);
                        }
                    }
                    UiCmd::TuneNow => {
                        self.run_tune(&mut eng, &tx);
                        let secs = self.state.lock().unwrap().config.check_secs;
                        next_check = Instant::now() + Duration::from_secs(secs);
                    }
                    UiCmd::ApplyFlags(new_flags) => {
                        let mut st = self.state.lock().unwrap();
                        st.config.flags = new_flags.clone();
                        st.active_flags = new_flags.clone();
                        st.current_ttl = flags::get_ttl(&new_flags);
                        let _ = st.config.save();
                        st.add_log(format!("Updated flags: {new_flags}"));
                        drop(st);

                        if eng.is_running() {
                            self.start_engine(&mut eng, &tx);
                            next_check = Instant::now() + Duration::from_millis(500);
                        }
                    }
                    UiCmd::SaveConfig => {
                        let st = self.state.lock().unwrap();
                        let _ = st.config.save();
                    }
                    UiCmd::ShowWindow => {
                        let mut st = self.state.lock().unwrap();
                        st.window_visible = true;
                    }
                    UiCmd::Quit => {
                        let mut st = self.state.lock().unwrap();
                        st.should_exit = true;
                        break;
                    }
                }
            }

            // 2. Drain any output lines from child process
            while let Ok(ev) = rx.try_recv() {
                if let Ev::GdpiLine(_, line) = ev {
                    if is_debug() {
                        eprintln!("[DEBUG/ciadpi] {line}");
                    }
                    let mut st = self.state.lock().unwrap();
                    st.add_log(format!("ciadpi: {line}"));
                }
            }

            // 3. Poll engine liveness
            if let Some(msg) = eng.poll() {
                proxy::clear_socks_proxy();
                let mut st = self.state.lock().unwrap();
                st.view = View::Off;
                st.verify = Verify::Unknown;
                st.engine_pid = None;
                st.started_at = None;
                st.probing = false;
                st.tuning = false;
                st.add_log(format!("Bypass stopped: {msg}"));
            }

            // 4. Periodic health check
            if eng.is_running() && Instant::now() >= next_check {
                self.run_check(&eng);
                let (autotune, blocked, secs) = {
                    let st = self.state.lock().unwrap();
                    (
                        st.config.autotune,
                        st.verify == Verify::Blocked,
                        st.config.check_secs,
                    )
                };
                if blocked && autotune {
                    self.run_tune(&mut eng, &tx);
                }
                next_check = Instant::now() + Duration::from_secs(secs);
            }

            std::thread::sleep(Duration::from_millis(100));
        }

        eng.stop();
        proxy::clear_socks_proxy();
    }

    fn start_engine(&self, eng: &mut Engine, tx: &Sender<Ev>) {
        let (flags, port) = {
            let mut st = self.state.lock().unwrap();
            st.view = View::Starting;
            st.add_log("Starting bypass engine...");
            (
                st.config.flags.clone(),
                flags::parse_proxy_port(&st.config.flags),
            )
        };

        let effective_flags = if is_debug() && !flags::uses(&flags, "-x") {
            format!("{flags} -x 1")
        } else {
            flags.clone()
        };
        let args = flags::tokenize(&effective_flags);
        if is_debug() {
            eprintln!(
                "[DEBUG/engine] Spawning: {} with args: {:?}",
                self.exe.display(),
                args
            );
        }
        match eng.start(&self.exe, &args, tx) {
            Ok(pid) => {
                proxy::set_socks_proxy(port);
                let mut st = self.state.lock().unwrap();
                st.engine_pid = Some(pid);
                st.started_at = Some(Instant::now());
                st.view = View::Checking;
                st.add_log(format!(
                    "ciadpi started (PID {pid}). System proxy set to 127.0.0.1:{port}."
                ));
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                st.view = View::Off;
                st.add_log(format!("Failed to start bypass engine: {e}"));
            }
        }
    }

    fn run_check(&self, eng: &Engine) {
        if !eng.is_running() {
            return;
        }
        let (sites, port) = {
            let mut st = self.state.lock().unwrap();
            st.probing = true;
            st.view = View::Checking;
            (
                probe::parse_sites(&st.config.sites),
                flags::parse_proxy_port(&st.config.flags),
            )
        };

        let proxy: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let (verdict, results) = probe::probe_all_via(&sites, Some(proxy));

        if is_debug() {
            eprintln!(
                "[DEBUG/probe] Probe verdict: {:?} across {} sites",
                verdict,
                sites.len()
            );
            for r in &results {
                eprintln!(
                    "  [DEBUG/probe] {:24} [{:?}] {}",
                    r.host, r.verdict, r.detail
                );
            }
        }

        let mut st = self.state.lock().unwrap();
        st.probing = false;
        st.last_check_results = results.clone();

        match verdict {
            Verdict::Pass => {
                st.verify = Verify::Passed;
                st.view = View::On;
                st.checks_passed += 1;
                st.last_check_detail = "All sites passed".into();
                st.add_log("Check: all test sites passed through bypass.");
            }
            Verdict::Blocked => {
                st.verify = Verify::Blocked;
                st.view = View::Blocked;
                st.checks_blocked += 1;
                let reasons: Vec<String> = results
                    .iter()
                    .filter(|r| r.verdict != Verdict::Pass)
                    .map(|r| format!("{}: {}", r.host, r.detail))
                    .collect();
                st.last_check_detail = reasons.join("; ");
                let log_msg = format!("Check: DPI blocked sites: {}", st.last_check_detail);
                st.add_log(log_msg);
            }
            Verdict::Unreachable => {
                st.verify = Verify::Unreachable;
                st.view = View::Unreachable;
                st.last_check_detail = "Network unreachable".into();
                st.add_log("Check: test sites unreachable (network issue, not DPI).");
            }
        }
    }

    fn run_tune(&self, eng: &mut Engine, tx: &Sender<Ev>) {
        let (sites, port, min_ttl, max_ttl, base_flags) = {
            let mut st = self.state.lock().unwrap();
            st.tuning = true;
            st.view = View::Tuning;
            st.add_log("Starting autonomous TTL tuning...");
            (
                probe::parse_sites(&st.config.sites),
                flags::parse_proxy_port(&st.config.flags),
                st.config.ttl_min,
                st.config.ttl_max,
                st.config.flags.clone(),
            )
        };

        let proxy: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let mut found_ttl = None;

        if is_debug() {
            eprintln!("[DEBUG/tune] Starting TTL tuning (range {min_ttl}..={max_ttl})");
        }

        for ttl in min_ttl..=max_ttl {
            {
                let mut st = self.state.lock().unwrap();
                st.tuning_step = Some(ttl);
                st.add_log(format!("Testing TTL {ttl}..."));
            }

            let candidate_flags = flags::with_ttl(&base_flags, ttl);
            let args = flags::tokenize(&candidate_flags);
            if eng.start(&self.exe, &args, tx).is_ok() {
                std::thread::sleep(Duration::from_millis(500));
                let (v, results) = probe::probe_all_via(&sites, Some(proxy));
                if v == Verdict::Pass {
                    found_ttl = Some((ttl, candidate_flags));
                    break;
                } else {
                    let mut st = self.state.lock().unwrap();
                    let blocked_detail: Vec<String> = results
                        .iter()
                        .filter(|r| r.verdict != Verdict::Pass)
                        .map(|r| format!("{}: {}", r.host, r.detail))
                        .collect();
                    st.add_log(format!("TTL {ttl} blocked ({})", blocked_detail.join(", ")));
                }
            }
        }

        let mut st = self.state.lock().unwrap();
        st.tuning = false;
        st.tuning_step = None;

        match found_ttl {
            Some((ttl, new_flags)) => {
                st.config.flags = new_flags.clone();
                st.active_flags = new_flags.clone();
                st.current_ttl = Some(ttl);
                st.view = View::On;
                st.verify = Verify::Passed;
                let _ = st.config.save();
                st.add_log(format!(
                    "Tuning succeeded! Optimal TTL {ttl} found and saved to config."
                ));
            }
            None => {
                // Restore base flags
                let args = flags::tokenize(&base_flags);
                let _ = eng.start(&self.exe, &args, tx);
                st.view = View::Blocked;
                st.verify = Verify::Blocked;
                st.add_log(format!(
                    "Tuning finished: No working TTL found in range {min_ttl}-{max_ttl}."
                ));
            }
        }
    }
}
