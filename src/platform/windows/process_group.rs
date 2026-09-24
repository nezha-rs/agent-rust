use std::{io, mem::size_of, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{OpenProcess, PROCESS_SET_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
    },
};

pub struct WindowsJob(HANDLE);

// A HANDLE remains valid when an async task moves between executor threads.
unsafe impl Send for WindowsJob {}

impl WindowsJob {
    pub fn new() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(error);
        }
        Ok(Self(handle))
    }

    pub fn assign(&self, pid: u32) -> io::Result<()> {
        let process = unsafe {
            OpenProcess(
                PROCESS_TERMINATE | PROCESS_SET_QUOTA | PROCESS_SET_INFORMATION,
                0,
                pid,
            )
        };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let assigned = unsafe { AssignProcessToJobObject(self.0, process) };
        let error = if assigned == 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        unsafe { CloseHandle(process) };
        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }

    pub fn terminate(&self) {
        unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
