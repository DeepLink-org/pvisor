//! No runner executes before it belongs to a kill-on-close Job.
use super::ipc::Handle;
use anyhow::ensure;
use std::{io, os::windows::io::AsRawHandle, process::Child, ptr};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    },
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    },
    System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
};
pub(super) fn own_and_resume(child: &mut Child) -> anyhow::Result<Handle> {
    let result = (|| {
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        ensure!(
            !raw.is_null(),
            "create runner Job: {}",
            io::Error::last_os_error()
        );
        let job = Handle(raw);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        ensure!(
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } != 0,
            "configure runner Job"
        );
        ensure!(
            unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle().cast()) } != 0,
            "assign suspended runner to Job: {}",
            io::Error::last_os_error()
        );
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        ensure!(
            raw != INVALID_HANDLE_VALUE,
            "snapshot suspended runner thread"
        );
        let snapshot = Handle(raw);
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut present = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
        while present {
            if entry.th32OwnerProcessID == child.id() {
                let raw = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                ensure!(!raw.is_null(), "open suspended runner thread");
                let thread = Handle(raw);
                ensure!(
                    unsafe { ResumeThread(thread.0) } != u32::MAX,
                    "resume owned runner"
                );
                return Ok(job);
            }
            present = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
        }
        anyhow::bail!("runner has no suspended primary thread")
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
