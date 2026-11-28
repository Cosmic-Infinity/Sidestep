//! Pure, side-effect-free parts of DPI Toggle (testable without elevation).
pub mod engine;
pub mod flags;
pub mod probe;

#[cfg(not(windows))]
pub use engine::Engine;
#[cfg(windows)]
pub use engine::{Engine, Notifier};

#[derive(Clone, Debug)]
pub enum Ev {
    Tick,
    Log(String),
    GdpiLine(u64, String),
    GdpiEof(u64),
    Probe { verdict: probe::Verdict, lines: Vec<String> },
    TuneStep(u8, usize, usize, String),
    TuneDone(Result<(u8, String), String>),
}
