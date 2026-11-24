//! On-disk layout of the daemon's state root.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

/// The state root (`~/.smithagents` unless `SMITHAGENTS_HOME` is set) and
/// everything placed under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    /// `SMITHAGENTS_HOME` when set and non-empty, else `~/.smithagents`
    /// (relative `.smithagents` if no home directory can be found).
    pub fn from_env() -> Paths {
        Self::from_parts(std::env::var_os("SMITHAGENTS_HOME"), std::env::home_dir())
    }

    fn from_parts(override_home: Option<OsString>, user_home: Option<PathBuf>) -> Paths {
        let home = match override_home {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => user_home.unwrap_or_default().join(".smithagents"),
        };
        Paths { home }
    }

    /// Holds the lockfile and instance lock; created with mode 0700 by the daemon.
    pub fn run_dir(&self) -> PathBuf {
        self.home.join("run")
    }

    pub fn db_path(&self) -> PathBuf {
        self.home.join("smithagents.db")
    }

    /// Agent worktrees live in `worktrees_dir()/<repo dir name>/`.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.home.join("worktrees")
    }

    /// Creates the home, worktrees and database directories. The run directory
    /// is left to the daemon, which creates it with owner-only permissions.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.home)?;
        std::fs::create_dir_all(self.worktrees_dir())?;
        if let Some(parent) = self.db_path().parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_home_resolution() {
        let user = Some(PathBuf::from("/users/me"));
        let default = Paths::from_parts(None, user.clone());
        assert_eq!(default.home, PathBuf::from("/users/me/.smithagents"));
        assert_eq!(
            Paths::from_parts(Some("".into()), user.clone()).home,
            default.home
        );
        let custom = Paths::from_parts(Some("/state".into()), user);
        assert_eq!(custom.home, PathBuf::from("/state"));
        assert_eq!(custom.run_dir(), PathBuf::from("/state/run"));
        assert_eq!(custom.db_path(), PathBuf::from("/state/smithagents.db"));
        assert_eq!(custom.worktrees_dir(), PathBuf::from("/state/worktrees"));
    }

    #[test]
    fn ensure_dirs_creates_home_worktrees_and_db_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: tmp.path().join("a").join("home"),
        };
        paths.ensure_dirs().unwrap();
        paths.ensure_dirs().unwrap(); // idempotent
        assert!(paths.home.is_dir());
        assert!(paths.worktrees_dir().is_dir());
        assert!(paths.db_path().parent().unwrap().is_dir());
    }
}
