//! What the processes the daemon starts (agents, git) inherit from it.

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
