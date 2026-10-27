#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

//! Menu-bar helper for the LBP2900 driver.

mod updater;

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("lbp2900-app {} runs on macOS only (update source: {}).", updater::CURRENT, updater::REPO);
}
