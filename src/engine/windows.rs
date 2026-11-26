use crate::platform::windows::{owned, wide};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::{Command, ExitStatus};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;

pub fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((owned(read)?, owned(write)?))
}

pub struct Process {
    handle: OwnedHandle,
    _job: Option<OwnedHandle>,
}

impl Process {
    pub fn handle(&self) -> io::Result<OwnedHandle> {
        self.handle.try_clone()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => self.wait().map(Some),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if unsafe { WaitForSingleObject(self.handle.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ExitStatus::from_raw(code))
    }

    pub fn kill(&mut self) -> io::Result<()> {
        if unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn argument(value: &std::ffi::OsStr, out: &mut Vec<u16>) {
    out.push(b'"' as u16);
    let mut slashes = 0;
    for unit in value.encode_wide() {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(b'\\' as u16, if unit == b'"' as u16 { slashes * 2 + 1 } else { slashes }));
        slashes = 0;
        out.push(unit);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
}

pub fn spawn(command: &Command, pipes: [&OwnedHandle; 2]) -> io::Result<Process> {
    let job = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    spawn_with(command, &pipes.map(|pipe| pipe.as_raw_handle()), None, Some(job))
}

pub fn background(command: &Command) -> io::Result<(Process, std::fs::File, std::fs::File)> {
    let input: OwnedHandle = std::fs::File::open("NUL")?.into();
    let (output, child_output) = pipe()?;
    let (error, child_error) = pipe()?;
    let handles = [input.as_raw_handle(), child_output.as_raw_handle(), child_error.as_raw_handle()];
    let child = spawn_with(command, &handles, Some(handles), None)?;
    Ok((child, output.into(), error.into()))
}

fn spawn_with(
    command: &Command,
    handles: &[HANDLE],
    stdio: Option<[HANDLE; 3]>,
    job: Option<OwnedHandle>,
) -> io::Result<Process> {
    let mut size = 0;
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
    let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let attributes = storage.as_mut_ptr().cast();
    if unsafe { InitializeProcThreadAttributeList(attributes, 1, 0, &mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        for &handle in handles {
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        if unsafe {
            UpdateProcThreadAttribute(
                attributes,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast_mut().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
        startup.lpAttributeList = attributes;
        if let Some([input, output, error]) = stdio {
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = input;
            startup.StartupInfo.hStdOutput = output;
            startup.StartupInfo.hStdError = error;
        }
        let mut environment: Vec<_> = std::env::vars_os().collect();
        for (name, value) in command.get_envs() {
            environment.retain(|(key, _)| !key.to_string_lossy().eq_ignore_ascii_case(&name.to_string_lossy()));
            if let Some(value) = value {
                environment.push((name.to_owned(), value.to_owned()));
            }
        }
        environment.sort_by_key(|(key, _)| key.to_string_lossy().to_uppercase());
        let mut block = Vec::new();
        for (name, value) in environment {
            block.extend(name.encode_wide());
            block.push(b'=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
        block.push(0);
        let directory = command.get_current_dir().map(wide);
        let mut line = Vec::new();
        argument(command.get_program(), &mut line);
        for arg in command.get_args() {
            line.push(b' ' as u16);
            argument(arg, &mut line);
        }
        line.push(0);
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            CreateProcessW(
                wide(command.get_program()).as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                block.as_ptr().cast(),
                directory.as_ref().map_or(std::ptr::null(), |path| path.as_ptr()),
                &startup.StartupInfo,
                &mut process,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let handle = owned(process.hProcess)?;
        let thread = owned(process.hThread)?;
        if let Some(job) = &job {
            if unsafe { AssignProcessToJobObject(job.as_raw_handle(), handle.as_raw_handle()) } == 0 {
                let error = io::Error::last_os_error();
                unsafe { TerminateProcess(handle.as_raw_handle(), 1) };
                return Err(error);
            }
        }
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            let error = io::Error::last_os_error();
            unsafe { TerminateProcess(handle.as_raw_handle(), 1) };
            return Err(error);
        }
        Ok(handle)
    })();
    unsafe { DeleteProcThreadAttributeList(attributes) };
    for &handle in handles {
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
    result.map(|handle| Process { handle, _job: job })
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_arguments_keep_quotes_and_trailing_slashes() {
        let mut out = Vec::new();
        super::argument(std::ffi::OsStr::new("a b\\\"c\\"), &mut out);
        assert_eq!(String::from_utf16(&out).unwrap(), "\"a b\\\\\\\"c\\\\\"");
    }
}
