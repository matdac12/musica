//! One process-wide kill-on-close job object. Children that call `join` (mpv
//! today, the yt-dlp downloads next) never outlive musica however it dies: the
//! job handle closes with the process and Windows kills the members. Search's
//! short-lived yt-dlp does not join.

use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::sync::Mutex;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// The job handle, created on first success and never closed: it must live
/// exactly as long as the process. A failed creation is not remembered, so the
/// next `join` tries again.
fn handle() -> Option<HANDLE> {
    static JOB: Mutex<Option<isize>> = Mutex::new(None);
    let mut slot = JOB.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        match unsafe { make() } {
            Ok(h) => *slot = Some(h.0 as isize),
            Err(e) => crate::log::log(format!("job object not created: {e}")),
        }
    }
    slot.map(|h| HANDLE(h as *mut _))
}

/// Put a freshly spawned child in the job. Best effort: a failure is only
/// logged, the child still runs.
pub fn join(child: &Child) {
    let Some(job) = handle() else {
        crate::log::log("job object unavailable: child not bound to musica");
        return;
    };
    if unsafe { AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())) }.is_err() {
        crate::log::log(format!("could not add pid {} to the job", child.id()));
    }
}

unsafe fn make() -> windows::core::Result<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(None, None)?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Err(e) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            let _ = CloseHandle(job);
            return Err(e);
        }
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use windows::Win32::System::JobObjects::IsProcessInJob;

    #[test]
    fn joined_child_is_in_the_shared_job() {
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let outside = child.id();
        join(&child);
        let mut inside = Default::default();
        unsafe {
            IsProcessInJob(HANDLE(child.as_raw_handle()), handle(), &mut inside).unwrap();
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(inside.as_bool(), "pid {outside} not in the job");
    }

    #[test]
    fn job_is_a_singleton() {
        assert_eq!(handle().map(|h| h.0), handle().map(|h| h.0));
        assert!(handle().is_some());
    }
}
