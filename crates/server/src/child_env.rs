//! What the processes the daemon starts (agents, git) inherit from it, and
//! how one is stopped together with what it started.

use std::time::Duration;

use tokio::process::Command;

/// The longest a process group that was asked to stop ([`GroupKiller::stop`])
/// gets before it is killed: enough for git to remove its lock files and for
/// an agent to stop what it started, short enough not to hold anything up.
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(2);
/// How often [`GroupKiller::stop`] looks whether the leader has ended.
const STOP_POLL: Duration = Duration::from_millis(20);

/// The variables that hold plugin secrets: `SMITHAGENTS_SECRET_<PLUGIN>__<KEY>`.
const PLUGIN_SECRET_PREFIX: &[u8] = b"SMITHAGENTS_SECRET_";

/// Removes every `SMITHAGENTS_SECRET_*` variable from `cmd`'s environment,
/// both those the daemon inherited and any set on `cmd` itself: an agent or
/// git never needs a plugin's secret. Other `SMITHAGENTS_*` variables, such
/// as `SMITHAGENTS_HOME`, stay.
pub(crate) fn remove_plugin_secrets(cmd: &mut Command) -> &mut Command {
    let inherited = std::env::vars_os().map(|(name, _)| name);
    let set_here = cmd.as_std().get_envs().map(|(name, _)| name.to_owned());
    let names: Vec<_> = inherited
        .chain(set_here)
        .filter(|name| name.as_encoded_bytes().starts_with(PLUGIN_SECRET_PREFIX))
        .collect();
    for name in names {
        cmd.env_remove(name);
    }
    cmd
}

/// Kills the process group led by the process it names when dropped, unless
/// disarmed first. The process must have been started as the leader of a
/// group of its own (`process_group(0)`).
///
/// Declare it after the child it guards, so it is dropped first: the group
/// is then killed while its leader is not yet reaped and the id cannot have
/// been given to another process. For the same reason, disarm it once the
/// leader has been waited for.
pub(crate) struct GroupKiller(Option<u32>);

impl GroupKiller {
    /// A guard for the group led by the process with this id (`None`: the
    /// process is already gone, and nothing is killed).
    pub(crate) fn of(leader: Option<u32>) -> Self {
        Self(leader)
    }

    /// Leaves the group alone from here on.
    pub(crate) fn disarm(&mut self) {
        self.0 = None;
    }

    /// Asks the group to stop (SIGTERM), then waits until its leader has
    /// ended, for at most [`STOP_GRACE`]. The leader is looked at without
    /// being reaped, so the group's id stays its own. The guard stays armed:
    /// what is left when it is dropped is killed. Nothing happens once it
    /// is disarmed.
    pub(crate) async fn stop(&self) {
        let Some(leader) = self.0 else {
            return;
        };
        signal_group(Some(leader), rustix::process::Signal::TERM);
        let deadline = tokio::time::Instant::now() + STOP_GRACE;
        while !has_ended(leader) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(STOP_POLL).await;
        }
    }

    /// Kills the group now, once.
    pub(crate) fn kill(&mut self) {
        kill_group(self.0.take());
    }
}

impl Drop for GroupKiller {
    fn drop(&mut self) {
        kill_group(self.0);
    }
}

/// Sends SIGKILL to every process of the group led by `leader`.
pub(crate) fn kill_group(leader: Option<u32>) {
    signal_group(leader, rustix::process::Signal::KILL);
}

/// Whether the child process with this id has ended, without reaping it.
/// One that cannot be looked at (not ours, or gone) counts as ended.
fn has_ended(leader: u32) -> bool {
    #[cfg(unix)]
    {
        use rustix::process::{waitid, Pid, WaitId, WaitIdOptions};
        let Some(pid) = i32::try_from(leader).ok().and_then(Pid::from_raw) else {
            return true;
        };
        let peek = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        !matches!(waitid(WaitId::Pid(pid), peek), Ok(None))
    }
    #[cfg(not(unix))]
    {
        let _ = leader;
        true
    }
}

fn signal_group(leader: Option<u32>, signal: rustix::process::Signal) {
    #[cfg(unix)]
    if let Some(pid) = leader
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        // The group may be gone already.
        let _ = rustix::process::kill_process_group(pid, signal);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn only_plugin_secret_variables_are_removed() {
        let mut cmd = Command::new("true");
        cmd.env("SMITHAGENTS_SECRET_SET__HERE", "x")
            .env("SMITHAGENTS_HOME", "/home")
            .env("OTHER", "y");
        remove_plugin_secrets(&mut cmd);
        let envs: HashMap<_, _> = cmd.as_std().get_envs().collect();
        let val = |k: &str| envs.get(OsStr::new(k)).copied();
        assert_eq!(val("SMITHAGENTS_SECRET_SET__HERE"), Some(None));
        assert_eq!(val("SMITHAGENTS_HOME"), Some(Some(OsStr::new("/home"))));
        assert_eq!(val("OTHER"), Some(Some(OsStr::new("y"))));
    }
}
