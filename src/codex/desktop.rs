use std::{
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    ptr::null_mut,
};

use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
    Storage::Packaging::Appx::{GetPackagePathByFullName, GetPackagesByPackageFamily},
};

pub fn find_in_desktop<T>(mut read: impl FnMut(&Path) -> Option<T>) -> Option<T> {
    let family: Vec<u16> = "OpenAI.Codex_2p2nqsd0c76g0\0".encode_utf16().collect();
    let (mut count, mut length) = (0, 0);
    // First ask Windows for the size of the package name buffers.
    let result = unsafe {
        GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            null_mut(),
            &mut length,
            null_mut(),
        )
    };
    if result != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    let mut names = vec![null_mut(); count as usize];
    let mut buffer = vec![0; length as usize];
    // Windows writes name pointers into `names` and the strings into `buffer`.
    let result = unsafe {
        GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            names.as_mut_ptr(),
            &mut length,
            buffer.as_mut_ptr(),
        )
    };
    if result != ERROR_SUCCESS {
        return None;
    }
    names[..count as usize].iter().find_map(|&name| {
        let mut path_length = 0;
        // Each name is a null-terminated string in `buffer`, which is still alive.
        let result = unsafe { GetPackagePathByFullName(name, &mut path_length, null_mut()) };
        if result != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
        let mut path = vec![0; path_length as usize];
        let result = unsafe { GetPackagePathByFullName(name, &mut path_length, path.as_mut_ptr()) };
        if result != ERROR_SUCCESS || path_length == 0 {
            return None;
        }
        let root = PathBuf::from(OsString::from_wide(&path[..path_length as usize - 1]));
        read(&root)
    })
}
