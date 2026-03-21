#[cfg(windows)]
mod imp {
    use std::io;
    use std::mem::size_of;
    use std::ptr;

    use anyhow::{Context, Result};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    #[derive(Debug)]
    pub struct ProcessJobObject {
        handle: HANDLE,
    }

    unsafe impl Send for ProcessJobObject {}
    unsafe impl Sync for ProcessJobObject {}

    impl ProcessJobObject {
        pub fn new() -> Result<Self> {
            let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error()).context("failed to create job object");
            }

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let success = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };

            if success == 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    CloseHandle(handle);
                }
                return Err(error).context("failed to configure job object");
            }

            Ok(Self { handle })
        }

        pub fn assign_pid(&self, pid: u32) -> Result<()> {
            let process_handle =
                unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
            if process_handle.is_null() {
                return Err(io::Error::last_os_error())
                    .with_context(|| format!("failed to open process {pid} for job assignment"));
            }

            let success = unsafe { AssignProcessToJobObject(self.handle, process_handle) };
            let assign_result = if success == 0 {
                Err(io::Error::last_os_error())
                    .with_context(|| format!("failed to assign process {pid} to job object"))
            } else {
                Ok(())
            };

            unsafe {
                CloseHandle(process_handle);
            }

            assign_result
        }

        pub fn terminate_all_processes(&self) -> Result<()> {
            let success = unsafe { TerminateJobObject(self.handle, 1) };
            if success == 0 {
                return Err(io::Error::last_os_error())
                    .context("failed to terminate processes in job object");
            }

            Ok(())
        }
    }

    impl Drop for ProcessJobObject {
        fn drop(&mut self) {
            if self.handle.is_null() {
                return;
            }

            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::{bail, Result};

    #[derive(Debug)]
    pub struct ProcessJobObject;

    impl ProcessJobObject {
        pub fn new() -> Result<Self> {
            bail!("Windows job objects are only supported on Windows")
        }

        pub fn assign_pid(&self, _pid: u32) -> Result<()> {
            bail!("Windows job objects are only supported on Windows")
        }

        pub fn terminate_all_processes(&self) -> Result<()> {
            bail!("Windows job objects are only supported on Windows")
        }
    }
}

pub use imp::ProcessJobObject;
