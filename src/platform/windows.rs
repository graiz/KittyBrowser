use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Threading::*;

pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

pub fn known_folder(id: &windows_sys::core::GUID) -> io::Result<PathBuf> {
    let mut pointer = std::ptr::null_mut();
    let result =
        unsafe { windows_sys::Win32::UI::Shell::SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut pointer) };
    if result < 0 {
        return Err(io::Error::other(format!("Known Folder lookup failed ({result:#x})")));
    }
    let path = unsafe {
        let len = (0..).take_while(|&n| *pointer.add(n) != 0).count();
        let value = std::ffi::OsString::from_wide(std::slice::from_raw_parts(pointer, len));
        windows_sys::Win32::System::Com::CoTaskMemFree(pointer.cast());
        PathBuf::from(value)
    };
    Ok(path)
}

pub fn user_sid() -> io::Result<Vec<u8>> {
    process_sid(unsafe { GetCurrentProcess() })
}

pub fn process_sid(process: HANDLE) -> io::Result<Vec<u8>> {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(token)?;
    let mut len = 0;
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, std::ptr::null_mut(), 0, &mut len) };
    // TOKEN_USER includes a pointer, so the backing storage must be aligned.
    let mut buffer = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, buffer.as_mut_ptr().cast(), len, &mut len) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let len = unsafe { GetLengthSid(sid) };
    let mut copy = vec![0u8; len as usize];
    if unsafe { CopySid(len, copy.as_mut_ptr().cast(), sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(copy)
}

pub fn sid_name() -> io::Result<String> {
    let sid = user_sid()?;
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid.as_ptr().cast_mut().cast(), &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let name = unsafe {
        let len = (0..).take_while(|&n| *text.add(n) != 0).count();
        let name = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        name
    };
    Ok(name)
}

pub struct Security {
    descriptor: PSECURITY_DESCRIPTOR,
}

impl Security {
    pub fn new() -> io::Result<Self> {
        let sddl = wide(format!("D:P(A;OICI;FA;;;{})", sid_name()?));
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { descriptor })
    }

    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        }
    }

    pub fn protect(&self, handle: HANDLE) -> io::Result<()> {
        let sid = user_sid()?;
        let mut owner = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let ours = unsafe { EqualSid(owner, sid.as_ptr().cast_mut().cast()) != 0 };
        unsafe { LocalFree(descriptor) };
        if !ours {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the folder belongs to another user"));
        }
        let (mut present, mut defaulted, mut acl) = (0, 0, std::ptr::null_mut());
        if unsafe { GetSecurityDescriptorDacl(self.descriptor, &mut present, &mut acl, &mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        unsafe { LocalFree(self.descriptor) };
    }
}

pub fn random_name() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    let result = unsafe {
        windows_sys::Win32::Security::Cryptography::BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            windows_sys::Win32::Security::Cryptography::BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if result < 0 {
        return Err(io::Error::other(format!("generating a private filename failed ({result:#x})")));
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn file_id(file: &std::fs::File) -> io::Result<(u32, u64)> {
    use windows_sys::Win32::Storage::FileSystem::*;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok((info.dwVolumeSerialNumber, (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)))
}

pub fn editor(editor: &str, path: &std::path::Path) -> io::Result<std::process::Child> {
    let mut count = 0;
    let args = unsafe { windows_sys::Win32::UI::Shell::CommandLineToArgvW(wide(editor).as_ptr(), &mut count) };
    if args.is_null() {
        return Err(io::Error::last_os_error());
    }
    let words: Vec<_> = unsafe {
        std::slice::from_raw_parts(args, count as usize)
            .iter()
            .map(|&word| {
                let len = (0..).take_while(|&n| *word.add(n) != 0).count();
                std::ffi::OsString::from_wide(std::slice::from_raw_parts(word, len))
            })
            .collect()
    };
    unsafe { LocalFree(args.cast()) };
    let program = words.first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "EDITOR needs a program"))?;
    std::process::Command::new(program).args(&words[1..]).arg(path).spawn()
}
