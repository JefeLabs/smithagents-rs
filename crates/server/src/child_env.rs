//! What the processes the daemon starts (agents, git) inherit from it, and
//! how one is stopped together with what it started.

use tokio::process::Command;

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
    #[cfg(unix)]
    if let Some(pid) = leader
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        // The group may be gone already.
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
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
