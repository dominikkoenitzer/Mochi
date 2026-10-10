//! Mochi-Setup.exe installs Mochi for the current user on a double click and
//! removes it again from Apps & features. No admin rights, no console window.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod plan;
#[cfg(windows)]
mod setup;
#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    std::process::exit(setup::run());
}
