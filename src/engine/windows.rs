//! Windows GoodbyeDPI process management via Win32 Job Objects.

use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use std::time::Instant;

use crate::Ev;

pub type Notifier = nwg::NoticeSender;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;

#[repr(C)]
#[derive(Default)]
struct BasicLimit {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Default)]
struct IoCounters {
    _c: [u64; 6],
}

#[repr(C)]
#[derive(Default)]
struct ExtendedLimit {
    basic: BasicLimit,
    io: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> *mut c_void;
    fn SetInformationJobObject(job: *mut c_void, class: i32, info: *mut c_void, len: u32) -> i32;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
}

/// Process-lifetime job handle (stored as usize so it is Send/Sync). Never closed
/// explicitly: the OS closes it when this process ends, killing the children.
static JOB: OnceLock<usize> = OnceLock::new();

fn job() -> Result<*mut c_void, String> {
    let h = *JOB.get_or_init(|| unsafe {
        let h = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if h.is_null() {
            return 0;
        }
        let mut info = ExtendedLimit::default();
        info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            h,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<ExtendedLimit>() as u32,
        );
        if ok == 0 {
            return 0;
        }
        h as usize
    });
    if h == 0 {
        Err("could not create kill-on-close job object".into())
    } else {
        Ok(h as *mut c_void)
    }
}

#[derive(Default)]
pub struct Engine {
    child: Option<Child>,
    /// Incremented on every start so stale reader-thread events can be ignored.
    pub generation: u64,
    pub started_at: Option<Instant>,
    pub active_flags: String,
    /// true once goodbyedpi has survived its start-up (or printed "Filter activated").
    pub confirmed: bool,
}

impl Engine {
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// Starts goodbyedpi with `args`. Any previous instance is stopped first.
    pub fn start(
        &mut self,
        exe: &Path,
        args: &[String],
        tx: &Sender<Ev>,
        notice: Notifier,
    ) -> Result<u32, String> {
        self.stop();
        let job = job()?;

        let exe_abs = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        let cur_dir = match exe_abs.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut child = Command::new(&exe_abs)
            .args(args)
            .current_dir(cur_dir)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to launch {}: {e}", exe.display()))?;

        let ok = unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) };
        if ok == 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "could not attach goodbyedpi to the kill-on-close job; refused to run it \
                        unsupervised"
                    .into(),
            );
        }

        self.generation += 1;
        let gen = self.generation;
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, gen, tx.clone(), notice, true);
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, gen, tx.clone(), notice, false);
        }

        let pid = child.id();
        self.child = Some(child);
        self.started_at = Some(Instant::now());
        self.active_flags = crate::flags::join(args);
        self.confirmed = false;
        Ok(pid)
    }

    /// Terminates goodbyedpi and waits for it to be gone. Returns a log line if
    /// something was actually stopped.
    pub fn stop(&mut self) -> Option<String> {
        let mut child = self.child.take()?;
        let pid = child.id();
        let _ = child.kill();
        let status = child.wait();
        self.started_at = None;
        self.active_flags.clear();
        self.confirmed = false;
        Some(match status {
            Ok(_) => format!("goodbyedpi (PID {pid}) terminated"),
            Err(e) => format!("goodbyedpi (PID {pid}) kill issued, wait failed: {e}"),
        })
    }

    /// Non-blocking liveness check. Returns Some(description) if the child had
    /// exited on its own since the last poll.
    pub fn poll(&mut self) -> Option<String> {
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                let pid = child.id();
                self.child = None;
                self.started_at = None;
                self.active_flags.clear();
                self.confirmed = false;
                Some(format!(
                    "goodbyedpi (PID {pid}) exited on its own: {status}"
                ))
            }
            Ok(None) => None,
            Err(e) => Some(format!("could not query goodbyedpi status: {e}")),
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    stream: R,
    gen: u64,
    tx: Sender<Ev>,
    notice: Notifier,
    is_stdout: bool,
) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.split(b'\n') {
            let Ok(bytes) = line else { break };
            let text = String::from_utf8_lossy(&bytes).trim_end().to_string();
            if text.is_empty() {
                continue;
            }
            let _ = tx.send(Ev::GdpiLine(gen, text));
            notice.notice();
        }
        if is_stdout {
            let _ = tx.send(Ev::GdpiEof(gen));
            notice.notice();
        }
    });
}

/// PIDs of goodbyedpi.exe processes on this machine (any owner).
pub fn find_goodbyedpi_pids() -> Vec<u32> {
    let out = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq goodbyedpi.exe", "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let Ok(out) = out else { return vec![] };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split("\",\"").collect();
            if cols.len() > 1
                && cols[0]
                    .trim_matches('"')
                    .eq_ignore_ascii_case("goodbyedpi.exe")
            {
                cols[1].trim_matches('"').parse().ok()
            } else {
                None
            }
        })
        .collect()
}

pub fn kill_pid(pid: u32) -> bool {
    Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
