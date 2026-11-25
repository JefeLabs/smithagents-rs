//! Daemon discovery: the lockfile that tells clients how to reach it.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    pub version: String,
}

/// Prints `token: "<redacted>"`, so logging or a panic message never shows
/// the token.
impl fmt::Debug for DaemonInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonInfo")
            .field("pid", &self.pid)
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .field("version", &self.version)
            .finish()
    }
}

/// The SmithAgents home (the daemon's state root) from the environment:
/// `SMITHAGENTS_HOME` when set and non-empty, else `~/.smithagents`. The one
/// rule shared by the daemon and its clients.
pub fn home_from_env() -> PathBuf {
    resolve_home(std::env::var_os("SMITHAGENTS_HOME"), std::env::home_dir())
}

/// [`home_from_env`] with its inputs given: `override_home` when non-empty,
/// else `user_home/.smithagents` (a relative `.smithagents` when no user home
/// is known).
pub fn resolve_home(override_home: Option<OsString>, user_home: Option<PathBuf>) -> PathBuf {
    match override_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => user_home.unwrap_or_default().join(".smithagents"),
    }
}

/// Location of the daemon lockfile: `<home>/run/daemon.json`.
pub fn lockfile_path(home: &Path) -> PathBuf {
    home.join("run").join("daemon.json")
}

/// Reads the lockfile; `Ok(None)` when it does not exist.
pub fn read_lockfile(home: &Path) -> io::Result<Option<DaemonInfo>> {
    match std::fs::read_to_string(lockfile_path(home)) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_redacts_the_token() {
        let info = DaemonInfo {
            pid: 42,
            port: 7777,
            token: "s3cr3t-t0ken-value".into(),
            version: "0.1.0".into(),
        };
        let shown = format!("{info:?}");
        assert!(!shown.contains("s3cr3t-t0ken-value"), "{shown}");
        assert!(shown.contains(r#"token: "<redacted>""#), "{shown}");
        assert!(
            shown.contains("pid: 42") && shown.contains("port: 7777"),
            "{shown}"
        );
        assert!(shown.contains(r#"version: "0.1.0""#), "{shown}");
        let pretty = format!("{info:#?}");
        assert!(!pretty.contains("s3cr3t-t0ken-value"), "{pretty}");
    }

    #[test]
    fn home_resolution() {
        let user = Some(PathBuf::from("/users/me"));
        assert_eq!(
            resolve_home(None, user.clone()),
            PathBuf::from("/users/me/.smithagents")
        );
        assert_eq!(
            resolve_home(Some("".into()), user.clone()),
            PathBuf::from("/users/me/.smithagents"),
            "an empty override is ignored"
        );
        assert_eq!(
            resolve_home(Some("/state".into()), user),
            PathBuf::from("/state")
        );
        assert_eq!(resolve_home(None, None), PathBuf::from(".smithagents"));
    }

    #[test]
    fn read_lockfile_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_lockfile(dir.path()).unwrap(), None);
    }

    #[test]
    fn read_lockfile_parses_present_file() {
        let dir = tempfile::tempdir().unwrap();
        let info = DaemonInfo {
            pid: 42,
            port: 7777,
            token: "tok".into(),
            version: "0.1.0".into(),
        };
        let path = lockfile_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&info).unwrap()).unwrap();
        assert_eq!(read_lockfile(dir.path()).unwrap(), Some(info));
        assert_eq!(path, dir.path().join("run").join("daemon.json"));
    }
}
