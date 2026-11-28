//! Owns the DPI bypass engine child process (GoodbyeDPI on Windows, ByeDPI/ciadpi on Linux).
//!
//! Honesty guarantees:
//! * Windows: child is placed in a Job Object with KILL_ON_JOB_CLOSE.
//! * Linux: child is spawned with prctl(PR_SET_PDEATHSIG, SIGTERM) so the kernel
//!   kills the child if this app exits or crashes.
//! * In both cases, if the supervision cannot be established, start() fails.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod linux;
#[cfg(unix)]
pub use linux::*;
