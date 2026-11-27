use std::io;

use anyhow::{Context, Result};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    },
    core::{PCWSTR, PWSTR},
};

/// A protected DACL that admits only the signed-in user and SYSTEM.
pub(super) struct PipeSecurity(PSECURITY_DESCRIPTOR);

// The descriptor is a LocalAlloc block that this value owns and never mutates.
unsafe impl Send for PipeSecurity {}

impl PipeSecurity {
    pub fn current_user() -> Result<Self> {
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})(A;;GA;;;SY)\0", user_sid()?)
            .encode_utf16()
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // `sddl` is null-terminated and Windows allocates the descriptor.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .context("Could not secure the DeckX MCP pipe")?;
        Ok(Self(descriptor))
    }

    pub fn create(&self, name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        };
        // Both the attributes and the descriptor outlive the call.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // The descriptor came from ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

fn user_sid() -> Result<String> {
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    let mut handle = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) }
        .context("Could not read the DeckX process token")?;
    let token = Token(handle);
    let mut length = 0;
    // The first call only reports the buffer size.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut length) };
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut length,
        )
    }
    .context("Could not read the signed-in Windows user")?;
    // The u64 buffer is aligned for TOKEN_USER, and Windows filled it above.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }
        .context("Could not format the Windows user SID")?;
    // Windows allocated `text` with LocalAlloc and null-terminated it.
    let sid = unsafe { text.to_string() };
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    Ok(sid?)
}
