//! Keeps one DeckX per Windows user, so two copies never drive the deck at once. Both would
//! receive every key press and run each binding twice.

use std::{
    fs::{File, OpenOptions},
    os::windows::fs::OpenOptionsExt,
    sync::OnceLock,
};

const ERROR_SHARING_VIOLATION: i32 = 32;

static LOCK: OnceLock<File> = OnceLock::new();

/// Returns false when another DeckX is already running. Windows releases the lock when the
/// process exits, including after a crash.
pub fn claim() -> bool {
    let Ok(profile) = crate::profile::path() else {
        return true;
    };
    let Some(folder) = profile.parent() else {
        return true;
    };
    if std::fs::create_dir_all(folder).is_err() {
        return true;
    }
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(folder.join("instance.lock"));
    match lock {
        Ok(file) => {
            let _ = LOCK.set(file);
            true
        }
        Err(error) => error.raw_os_error() != Some(ERROR_SHARING_VIOLATION),
    }
}

/// Like `claim`, but keeps trying for `wait`. A DeckX that is closing holds the lock while it
/// blanks the deck, so a quick relaunch would otherwise hand off to a window about to close.
pub fn claim_within(wait: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + wait;
    loop {
        if claim() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Brings the running DeckX window to the front.
pub fn focus_running() {
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            FindWindowW, IsIconic, IsWindowVisible, SW_RESTORE, SW_SHOW, SetForegroundWindow,
            ShowWindow,
        },
        core::w,
    };
    // eframe windows use winit's default class name and the app name as their title.
    let Ok(window) = (unsafe { FindWindowW(w!("Window Class"), w!("DeckX")) }) else {
        return;
    };
    // Both calls only change the state of the window found above.
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        } else if !IsWindowVisible(window).as_bool() {
            let _ = ShowWindow(window, SW_SHOW);
        }
        let _ = SetForegroundWindow(window);
    }
}
