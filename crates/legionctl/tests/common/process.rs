//! The one way a test starts a process that runs beside it (an operator, a daemon, a private bus, a stand-in, an
//! agent, a VM): a guard that kills the process's whole group when it is dropped, also when the test panics. Test
//! processes that stayed after hung or killed runs held the user's state (todo-123).

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

/// A process in a group of its own, killed with its group on drop.
pub struct Kill(Child);

impl Kill {
    /// Start `cmd` as [`own`] makes it.
    pub fn spawn(cmd: &mut Command) -> Kill {
        Kill(own(cmd).spawn().unwrap())
    }

    /// Kill the process group and wait for the process.
    pub fn stop(&mut self) {
        use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
        let pid = Pid::from_child(&self.0);
        // the group id is the process id, which the system can give again once the process was reaped (by a test's
        // own wait): then there is no group of ours to kill
        let reaped = waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .is_err();
        if !reaped {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        let _ = self.0.wait();
    }
}

/// `cmd` starts a new process group (the guard kills the group: the process and what it started), and its process
/// gets SIGKILL when the thread that started it ends, so a test binary killed with SIGKILL leaves nothing.
// shortcut: the parent-death signal reaches only the direct child; what it started stays when the test binary is
// killed with SIGKILL (the drop does not run). The stray check (`make stray-tests`) finds those.
pub fn own(cmd: &mut Command) -> &mut Command {
    let parent = rustix::process::getpid();
    // the agent session of the shell that runs the tests is not the test's: only one that the test set is kept
    for var in [
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
        "LEGIONCTL_SESSION",
        "LEGIONCTL_TOKEN",
    ] {
        if !cmd.get_envs().any(|(k, _)| k == var) {
            cmd.env_remove(var);
        }
    }
    cmd.process_group(0);
    // SAFETY: the closure makes only async-signal-safe system calls (prctl, getppid).
    unsafe {
        cmd.pre_exec(move || {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))?;
            // the parent ended before the prctl: no signal comes, so stop here
            if rustix::process::getppid() != Some(parent) {
                return Err(std::io::Error::other("the test process ended"));
            }
            Ok(())
        })
    }
}

impl std::ops::Deref for Kill {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Kill {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Kill {
    fn drop(&mut self) {
        self.stop();
    }
}
