//! Private Windows files and directories, protected by a current-user DACL.

use crate::platform::windows::{owned, random_name, wide, Security};
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;

pub fn private_dir(dir: &Path) -> Result<(), String> {
    private_shared_dir(dir)
}

pub fn private_shared_dir(dir: &Path) -> Result<(), String> {
    make_private(dir).map_err(|e| format!("making {} private: {e}", dir.display()))
}

fn make_private(path: &Path) -> io::Result<()> {
    let security = Security::new()?;
    let name = wide(path);
    if unsafe { CreateDirectoryW(name.as_ptr(), &security.attributes()) } == 0
        && io::Error::last_os_error().raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32)
    {
        return Err(io::Error::last_os_error());
    }
    let handle = owned(unsafe {
        CreateFileW(
            name.as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    })?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private folders must be directories, not reparse points",
        ));
    }
    security.protect(handle.as_raw_handle())
}

pub fn private_temp_dir(prefix: &str) -> io::Result<PathBuf> {
    let security = Security::new()?;
    for _ in 0..10 {
        let path = std::env::temp_dir().join(format!("{prefix}{}", random_name()?));
        if unsafe { CreateDirectoryW(wide(&path).as_ptr(), &security.attributes()) } != 0 {
            return Ok(path);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "couldn't create a private temporary directory"))
}

pub fn write_new(path: &Path, bytes: &[u8], _mode: u32) -> io::Result<std::fs::File> {
    let security = Security::new()?;
    let handle = owned(unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            &security.attributes(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    })?;
    let mut file = std::fs::File::from(handle);
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_files_are_created_exclusively() {
        let folder = private_temp_dir("kb-private-test-").unwrap();
        private_dir(&folder).unwrap();
        let path = folder.join("private");
        write_new(&path, b"kept", 0o600).unwrap();
        assert_eq!(write_new(&path, b"replaced", 0o600).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(path).unwrap(), b"kept");
        assert!(private_shared_dir(&folder.join("private")).is_err());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn anonymous_readers_cannot_open_private_files() {
        let folder = private_temp_dir("kb-acl-test-").unwrap();
        let path = folder.join("private");
        write_new(&path, b"private", 0o600).unwrap();
        let error = std::thread::spawn(move || {
            use windows_sys::Win32::Security::*;
            use windows_sys::Win32::System::Threading::GetCurrentThread;
            assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
            let result = std::fs::File::open(path);
            assert_ne!(unsafe { RevertToSelf() }, 0);
            result.unwrap_err()
        })
        .join()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
