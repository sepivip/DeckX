use std::{
    path::Path,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND, LPARAM},
    System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    },
    UI::WindowsAndMessaging::{
        EnumChildWindows, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
    },
};

pub fn app_key(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).ok();
    let name = resolved.as_deref().unwrap_or(path).to_string_lossy();
    name.strip_prefix(r"\\?\").unwrap_or(&name).to_lowercase()
}

pub struct Foreground {
    last_poll: Option<Instant>,
    last_process: Option<u32>,
}

impl Foreground {
    pub fn new() -> Self {
        Self {
            last_poll: None,
            last_process: None,
        }
    }

    /// Returns a new foreground app key, or an empty key for an app we cannot resolve.
    /// Focusing DeckX itself keeps the current deck layout.
    pub fn changed_app(&mut self) -> Option<String> {
        if self
            .last_poll
            .is_some_and(|poll| poll.elapsed() < Duration::from_millis(250))
        {
            return None;
        }
        self.last_poll = Some(Instant::now());
        let mut process = 0;
        // Both calls read window metadata. The PID pointer refers to local storage.
        let window = unsafe { GetForegroundWindow() };
        if !window.is_null() {
            unsafe { GetWindowThreadProcessId(window, &mut process) };
            // Store apps run inside ApplicationFrameHost. Use the app it hosts instead.
            if is_frame_host(window) {
                process = hosted_process(window, process).unwrap_or(process);
            }
        }
        if self.last_process.replace(process) == Some(process)
            || process == std::process::id()
            || process == 0
        {
            return None;
        }
        Some(
            process_path(process)
                .map(|path| app_key(Path::new(&path)))
                .unwrap_or_default(),
        )
    }
}

fn is_frame_host(window: HWND) -> bool {
    let mut class = [0u16; 64];
    // The buffer length is passed with the buffer, and the call writes at most that much.
    let length = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
    usize::try_from(length)
        .is_ok_and(|length| String::from_utf16_lossy(&class[..length]) == "ApplicationFrameWindow")
}

/// Returns the process of the app inside a Store app frame.
fn hosted_process(frame: HWND, host: u32) -> Option<u32> {
    extern "system" fn visit(child: HWND, state: LPARAM) -> i32 {
        // `state` points to the `(host, hosted)` pair below, which outlives the enumeration.
        let (host, hosted) = unsafe { &mut *(state as *mut (u32, u32)) };
        let mut process = 0;
        unsafe { GetWindowThreadProcessId(child, &mut process) };
        if process != 0 && process != *host {
            *hosted = process;
            return 0;
        }
        1
    }
    let mut state = (host, 0u32);
    // The callback only runs during this call.
    unsafe { EnumChildWindows(frame, Some(visit), &mut state as *mut (u32, u32) as LPARAM) };
    (state.1 != 0).then_some(state.1)
}

fn process_path(process: u32) -> Option<std::ffi::OsString> {
    use std::os::windows::ffi::OsStringExt;
    // The process handle is opened for metadata and closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process);
        if handle.is_null() {
            return None;
        }
        let mut path = vec![0u16; 32_768];
        let mut length = path.len() as u32;
        let result = QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut length);
        CloseHandle(handle);
        (result != 0).then(|| std::ffi::OsString::from_wide(&path[..length as usize]))
    }
}
