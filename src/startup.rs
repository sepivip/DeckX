//! Per-user Windows sign-in registration, independent of the saved workspace.

use std::{
    ffi::OsStr,
    io,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr::{null, null_mut},
};

use anyhow::{Result, ensure};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
    System::Registry::{
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
    },
};

pub(crate) const ARG: &str = "--startup";
const RUN: Entry<'static> = Entry {
    key: r"Software\Microsoft\Windows\CurrentVersion\Run",
    name: "DeckX",
};
// Windows Run accepts at most 260 UTF-16 code units plus the terminator.
const COMMAND_LIMIT: usize = 260;

#[derive(Default)]
pub(crate) struct State {
    pub enabled: bool,
    pub error: Option<String>,
}

impl State {
    pub fn load(update_path: bool) -> Self {
        let mut state = Self::default();
        state.refresh(update_path);
        state
    }

    pub fn refresh(&mut self, update_path: bool) {
        match RUN.read() {
            Ok(command) => {
                self.enabled = command.is_some();
                self.error = command.filter(|_| update_path).and_then(|previous| {
                    std::env::current_exe()
                        .map_err(anyhow::Error::from)
                        .and_then(|path| RUN.retarget(&previous, &path))
                        .err()
                        .map(|error| format!("Could not update Windows startup: {error:#}"))
                });
            }
            Err(error) => self.error = Some(format!("Could not read Windows startup: {error:#}")),
        }
    }

    pub fn set(&mut self, enabled: bool) {
        let result = if enabled {
            std::env::current_exe()
                .map_err(anyhow::Error::from)
                .and_then(|path| command(&path))
                .and_then(|command| RUN.write(&command))
        } else {
            RUN.remove()
        };
        match result {
            Ok(()) => {
                self.enabled = enabled;
                self.error = None;
            }
            Err(error) => self.error = Some(format!("Could not change Windows startup: {error:#}")),
        }
    }
}

struct Entry<'a> {
    key: &'a str,
    name: &'a str,
}

impl Entry<'_> {
    fn read(&self) -> Result<Option<Vec<u16>>> {
        let key = wide(self.key);
        let name = wide(self.name);
        let mut value = [0_u16; COMMAND_LIMIT + 1];
        let mut bytes = size_of_val(&value) as u32;
        // The buffers are valid for the duration of this call.
        let result = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                value.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if missing(result) {
            return Ok(None);
        }
        check(result)?;
        let words = bytes as usize / size_of::<u16>();
        Ok((words > 1).then(|| value[..words].to_vec()))
    }

    /// Follow the newly opened executable when an existing entry was set by DeckX.
    fn retarget(&self, previous: &[u16], path: &Path) -> Result<()> {
        let suffix = wide(&format!("\" {ARG}"));
        if previous.starts_with(&[u16::from(b'"')]) && previous.ends_with(&suffix) {
            let next = command(path)?;
            if next != previous {
                self.write(&next)?;
            }
        }
        Ok(())
    }

    fn write(&self, command: &[u16]) -> Result<()> {
        let path = wide(self.key);
        let name = wide(self.name);
        let mut key = null_mut();
        // The names and command are terminated wide strings and the returned key is closed below.
        unsafe {
            check(RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                null(),
                &mut key,
                null_mut(),
            ))?;
            let result = RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                size_of_val(command) as u32,
            );
            RegCloseKey(key);
            check(result)
        }
    }

    fn remove(&self) -> Result<()> {
        let key = wide(self.key);
        let name = wide(self.name);
        // Both names are valid terminated wide strings.
        let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        if missing(result) {
            Ok(())
        } else {
            check(result)
        }
    }
}

fn command(path: &Path) -> Result<Vec<u16>> {
    ensure!(
        path.is_absolute(),
        "The DeckX executable path must be absolute"
    );
    let mut command = vec![u16::from(b'"')];
    for word in path.as_os_str().encode_wide() {
        ensure!(
            word != 0 && word != u16::from(b'"'),
            "Invalid DeckX executable path"
        );
        command.push(word);
    }
    command.extend(wide(&format!("\" {ARG}")));
    ensure!(
        command.len() <= COMMAND_LIMIT + 1,
        "The DeckX path is too long for Windows startup"
    );
    Ok(command)
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain([0]).collect()
}

fn missing(code: WIN32_ERROR) -> bool {
    matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
}

fn check(code: WIN32_ERROR) -> Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::RegDeleteKeyW;

    struct TestKey(String);

    impl Drop for TestKey {
        fn drop(&mut self) {
            // Each test creates its own subkey outside Windows Run.
            unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide(&self.0).as_ptr()) };
        }
    }

    #[test]
    fn startup_registration_can_be_written_moved_and_removed() -> Result<()> {
        let key = TestKey(format!(
            r"Software\DeckX-startup-test-{}",
            uuid::Uuid::new_v4()
        ));
        let entry = Entry {
            key: &key.0,
            name: RUN.name,
        };
        assert!(entry.read()?.is_none());
        entry.remove()?;

        let old = command(Path::new(r"E:\My DeckX\0.5.4\deckx.exe"))?;
        assert_eq!(old, wide("\"E:\\My DeckX\\0.5.4\\deckx.exe\" --startup"));
        entry.write(&old)?;
        assert_eq!(entry.read()?, Some(old.clone()));

        let next = Path::new(r"E:\My DeckX\0.5.5\deckx.exe");
        entry.retarget(&old, next)?;
        assert_eq!(entry.read()?, Some(command(next)?));

        let manual = wide("\"E:\\Other DeckX\\deckx.exe\" --connect --grok-mode");
        entry.write(&manual)?;
        entry.retarget(&manual, next)?;
        assert_eq!(entry.read()?, Some(manual));

        entry.remove()?;
        entry.remove()?;
        assert!(entry.read()?.is_none());
        Ok(())
    }

    #[test]
    fn startup_commands_fit_the_windows_run_limit() {
        let dir = "x".repeat(COMMAND_LIMIT);
        assert!(command(Path::new(&format!(r"C:\{dir}\deckx.exe"))).is_err());
        assert!(command(Path::new("deckx.exe")).is_err());
    }
}
