//! One process-wide kill-on-close job object. Children that call `join` (mpv
//! today, the yt-dlp downloads next) never outlive musica however it dies: the
//! job handle closes with the process and Windows kills the members. Search's
//! short-lived yt-dlp does not join.

use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::sync::OnceLock;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// The job handle, created on first use and never closed: it must live exactly
/// as long as the process. `None` if the job could not be made.
fn handle() -> Option<HANDLE> {
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    JOB.get_or_init(|| unsafe { make().map(|h| h.0 as isize) })
        .map(|h| HANDLE(h as *mut _))
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

unsafe fn make() -> Option<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        Some(job)
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
