use std::{path::Path, process::Stdio};

use process_wrap::tokio::{CommandWrap, KillOnDrop};
use tokio::process::Command;

pub(crate) fn command(path: &Path, configure: impl FnOnce(&mut Command)) -> CommandWrap {
    let mut command = CommandWrap::with_new(path, |command| {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        kill_when_core_exits(command);
        configure(command);
    });
    command.wrap(KillOnDrop);
    #[cfg(windows)]
    {
        use process_wrap::tokio::{CreationFlags, JobObject};
        use windows::Win32::System::Threading::DETACHED_PROCESS;

        // JobObject preserves creation flags registered through CreationFlags.
        // The workers communicate through redirected handles and need no console.
        command.wrap(CreationFlags(DETACHED_PROCESS));
        command.wrap(JobObject);
    }
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    command
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn kill_when_core_exits(command: &mut Command) {
    use rustix::{
        io::Errno,
        process::{Signal, getpid, getppid, set_parent_process_death_signal},
    };

    let parent = getpid();
    // SAFETY: The post-fork hook only calls prctl and getppid and returns raw
    // errno on failure. It performs no allocation or locking. Core spawns
    // from its main thread or runtime threads that live until Core shuts down.
    unsafe {
        command.pre_exec(move || {
            set_parent_process_death_signal(Some(Signal::KILL))?;
            if getppid() != Some(parent) {
                return Err(Errno::SRCH.into());
            }
            Ok(())
        });
    }
}
