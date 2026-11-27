//! Local-only, current-user named pipes behind the unchanged control protocol.

use crate::platform::windows::{owned, sid_name, wide, Security};
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::Path;
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

pub fn lock(file: &std::fs::File) -> io::Result<()> {
    let mut operation: OVERLAPPED = unsafe { std::mem::zeroed() };
    if unsafe { LockFileEx(file.as_raw_handle(), LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, &mut operation) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub struct Stream {
    handle: OwnedHandle,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl Stream {
    fn new(handle: OwnedHandle) -> Self {
        Self { handle, read_timeout: Some(super::REPLY_TIMEOUT), write_timeout: Some(super::REPLY_TIMEOUT) }
    }

    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let endpoint = std::fs::read_to_string(path)?;
        let prefix = format!(r"\\.\pipe\kittybrowser-{}-", sid_name()?);
        let suffix = endpoint
            .strip_prefix(&prefix)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid kb control endpoint"))?;
        if suffix.len() != 32 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid kb control endpoint"));
        }
        let name = wide(endpoint);
        for _ in 0..2 {
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            match owned(handle) {
                Ok(handle) => {
                    let mut pid = 0;
                    if unsafe { GetNamedPipeServerProcessId(handle.as_raw_handle(), &mut pid) } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let process = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
                    if crate::platform::windows::process_sid(process.as_raw_handle())?
                        != crate::platform::windows::user_sid()?
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "the control pipe belongs to another user",
                        ));
                    }
                    return Ok(Self::new(handle));
                }
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    if unsafe { WaitNamedPipeW(name.as_ptr(), 1000) } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "kb's control pipe is busy"))
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            handle: self.handle.try_clone()?,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
        })
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout = timeout;
        Ok(())
    }

    fn operation(
        &self,
        mut operation: impl FnMut(*mut OVERLAPPED) -> windows_sys::core::BOOL,
        timeout: Option<Duration>,
    ) -> io::Result<usize> {
        let event = owned(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) })?;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.as_raw_handle();
        if operation(&mut overlapped) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
            let wait = timeout.map_or(INFINITE, |timeout| timeout.as_millis().min(u32::MAX as u128 - 1) as u32);
            let ready = unsafe { WaitForSingleObject(event.as_raw_handle(), wait) };
            if ready != WAIT_OBJECT_0 {
                let error = if ready == WAIT_TIMEOUT {
                    io::Error::new(io::ErrorKind::TimedOut, "kb's control pipe did not answer in time")
                } else {
                    io::Error::last_os_error()
                };
                // The OVERLAPPED and data must stay alive until cancellation finishes.
                unsafe {
                    CancelIoEx(self.handle.as_raw_handle(), &overlapped);
                    let mut count = 0;
                    GetOverlappedResult(self.handle.as_raw_handle(), &overlapped, &mut count, 1);
                }
                return Err(error);
            }
        }
        let mut count = 0;
        if unsafe { GetOverlappedResult(self.handle.as_raw_handle(), &overlapped, &mut count, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(count as usize)
    }
}

impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        match self.operation(
            |operation| unsafe {
                ReadFile(
                    self.handle.as_raw_handle(),
                    bytes.as_mut_ptr(),
                    bytes.len().min(u32::MAX as usize) as u32,
                    std::ptr::null_mut(),
                    operation,
                )
            },
            self.read_timeout,
        ) {
            Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => Ok(0),
            result => result,
        }
    }
}

impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.operation(
            |operation| unsafe {
                WriteFile(
                    self.handle.as_raw_handle(),
                    bytes.as_ptr(),
                    bytes.len().min(u32::MAX as usize) as u32,
                    std::ptr::null_mut(),
                    operation,
                )
            },
            self.write_timeout,
        )
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct Listener {
    name: String,
    pending: Option<OwnedHandle>,
}

fn create(name: &str, first: bool) -> io::Result<OwnedHandle> {
    let security = Security::new()?;
    owned(unsafe {
        CreateNamedPipeW(
            wide(name).as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 },
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            &security.attributes(),
        )
    })
}

impl Listener {
    pub fn bind(_path: &Path) -> io::Result<Self> {
        let name = format!(r"\\.\pipe\kittybrowser-{}-{}", sid_name()?, crate::platform::windows::random_name()?);
        let pending = Some(create(&name, true)?);
        Ok(Self { name, pending })
    }

    pub fn incoming(self) -> Self {
        self
    }

    pub fn endpoint(&self) -> &str {
        &self.name
    }
}

impl Iterator for Listener {
    type Item = io::Result<Stream>;

    fn next(&mut self) -> Option<Self::Item> {
        let handle = self.pending.take()?;
        let stream = Stream::new(handle);
        let result =
            stream.operation(|operation| unsafe { ConnectNamedPipe(stream.handle.as_raw_handle(), operation) }, None);
        // Keep an available instance before handing this connection to a thread.
        match create(&self.name, false) {
            Ok(handle) => self.pending = Some(handle),
            Err(e) => return Some(Err(e)),
        }
        // An early client needs no OVERLAPPED completion: it is connected already.
        Some(match result {
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => Ok(stream),
            result => result.map(|_| stream),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipes_deliver_fragmented_messages_and_enforce_read_deadlines() {
        let folder = crate::files::private_temp_dir("kb-pipe-test-").unwrap();
        let marker = folder.join("kb.sock");
        let listener = Listener::bind(&marker).unwrap();
        crate::files::write_new(&marker, listener.endpoint().as_bytes(), 0o600).unwrap();
        let mut client = Stream::connect(&marker).unwrap();
        let server = std::thread::spawn(move || {
            let mut listener = listener.incoming();
            let mut stream = listener.next().unwrap().unwrap();
            let mut message = [0u8; 5];
            stream.read_exact(&mut message).unwrap();
            assert_eq!(&message, b"hello");
            stream.write_all(b"wo").unwrap();
            stream.write_all(b"rld").unwrap();
            std::thread::sleep(Duration::from_millis(150));
        });
        client.write_all(b"he").unwrap();
        client.write_all(b"llo").unwrap();
        let mut reply = [0u8; 5];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"world");
        client.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        assert_eq!(client.read(&mut reply).unwrap_err().kind(), io::ErrorKind::TimedOut);
        server.join().unwrap();
        drop(client);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn anonymous_clients_cannot_open_control_pipes() {
        let folder = crate::files::private_temp_dir("kb-pipe-acl-test-").unwrap();
        let listener = Listener::bind(&folder.join("kb.sock")).unwrap();
        let name = listener.endpoint().to_owned();
        let error = std::thread::spawn(move || {
            use windows_sys::Win32::Security::*;
            assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
            let result = owned(unsafe {
                CreateFileW(
                    wide(name).as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            });
            assert_ne!(unsafe { RevertToSelf() }, 0);
            result.unwrap_err()
        })
        .join()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        drop(listener);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
