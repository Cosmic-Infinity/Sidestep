#![cfg_attr(windows, windows_subsystem = "windows")]

pub use sidestep_core::{flags, probe, Ev};

pub mod autostart;
pub mod config;
pub mod engine;
#[cfg(not(windows))]
pub mod gui;
#[cfg(windows)]
pub mod kit;
#[cfg(not(windows))]
pub mod proxy;
pub mod setup;

#[cfg(windows)]
mod win_main;

#[cfg(not(windows))]
mod linux_main;

fn main() {
    #[cfg(windows)]
    win_main::main();

    #[cfg(not(windows))]
    linux_main::main();
}
