//! Linux ciadpi process supervision.
//! Child processes are started with PR_SET_PDEATHSIG=SIGTERM so the kernel
//! terminates them if Sidestep exits or crashes.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::Ev;

#[derive(Default)]
pub struct Engine {
    child: Option<Child>,
    /// Incremented on every start so stale reader-thread events can be ignored.
    pub generation: u64,
    pub started_at: Option<Instant>,
    pub active_flags: String,
    /// true once the engine has survived its start-up.
    pub confirmed: bool,
}

impl Engine {
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// Starts the DPI bypass engine (e.g. ciadpi) with `args`. Any previous instance is stopped first.
    pub fn start(&mut self, exe: &Path, args: &[String], tx: &Sender<Ev>) -> Result<u32, String> {
        self.stop();

        use std::os::unix::process::CommandExt;
        let exe_abs = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        let cur_dir = match exe_abs.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut cmd = Command::new(&exe_abs);
        cmd.args(args)
            .current_dir(cur_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Honesty guarantee:
        // prctl(PR_SET_PDEATHSIG, SIGTERM) ensures that if this Sidestep process
        // crashes or exits, the Linux kernel terminates the engine child immediately.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to launch {}: {e}", exe.display()))?;

        self.generation += 1;
        let gen = self.generation;
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, gen, tx.clone(), true);
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, gen, tx.clone(), false);
        }

        let pid = child.id();
        self.child = Some(child);
        self.started_at = Some(Instant::now());
        self.active_flags = crate::flags::join(args);
        self.confirmed = false;
        Ok(pid)
    }

    /// Terminates the child process and waits for it to exit.
    pub fn stop(&mut self) -> Option<String> {
        let mut child = self.child.take()?;
        let pid = child.id();
        let _ = child.kill();
        let status = child.wait();
        self.started_at = None;
        self.active_flags.clear();
        self.confirmed = false;
        Some(match status {
            Ok(_) => format!("bypass engine (PID {pid}) terminated"),
            Err(e) => format!("bypass engine (PID {pid}) kill issued, wait failed: {e}"),
        })
    }

    /// Non-blocking liveness check.
    pub fn poll(&mut self) -> Option<String> {
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                let pid = child.id();
                self.child = None;
                self.started_at = None;
                self.active_flags.clear();
                self.confirmed = false;
                Some(format!("bypass engine (PID {pid}) exited on its own: {status}"))
            }
            Ok(None) => None,
            Err(e) => Some(format!("could not query bypass engine status: {e}")),
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(stream: R, gen: u64, tx: Sender<Ev>, is_stdout: bool) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.split(b'\n') {
            let Ok(bytes) = line else { break };
            let text = String::from_utf8_lossy(&bytes).trim_end().to_string();
            if text.is_empty() {
                continue;
            }
            let _ = tx.send(Ev::GdpiLine(gen, text));
        }
        if is_stdout {
            let _ = tx.send(Ev::GdpiEof(gen));
        }
    });
}

/// Scans /proc to find any existing orphan or background instances of ciadpi or goodbyedpi.
pub fn find_goodbyedpi_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    let my_pid = std::process::id();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Ok(pid) = name.to_string_lossy().parse::<u32>() {
                if pid == my_pid {
                    continue;
                }
                if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
                    let comm = comm.trim();
                    if comm == "ciadpi" || comm == "goodbyedpi" {
                        pids.push(pid);
                    }
                }
            }
        }
    }
    pids
}

pub fn signal_pid(pid: u32, sig: libc::c_int) -> bool {
    unsafe { libc::kill(pid as i32, sig) == 0 }
}

pub fn kill_pid(pid: u32) -> bool {
    signal_pid(pid, libc::SIGKILL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn engine_lifecycle() {
        let mut eng = Engine::default();
        assert!(!eng.is_running());
        assert_eq!(eng.pid(), None);

        let (tx, _rx) = channel();
        // Launch a harmless process to verify supervisor mechanics
        let res = eng.start(Path::new("sleep"), &["5".into()], &tx);
        assert!(res.is_ok());
        assert!(eng.is_running());
        assert!(eng.pid().is_some());

        let stop_msg = eng.stop();
        assert!(stop_msg.is_some());
        assert!(!eng.is_running());
        assert_eq!(eng.pid(), None);
    }
}
