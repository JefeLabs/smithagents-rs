//! On-disk layout of the daemon's state root.

use std::io;
use std::path::{Path, PathBuf};

/// The state root (`~/.smithagents` unless `SMITHAGENTS_HOME` is set) and
/// everything placed under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    /// `SMITHAGENTS_HOME` when set and non-empty, else `~/.smithagents`
    /// (relative `.smithagents` if no home directory can be found). The rule
    /// lives in `smithagents_core::daemon` so clients resolve the same home.
    pub fn from_env() -> Paths {
        Paths {
            home: smithagents_core::daemon::home_from_env(),
        }
    }

    #[cfg(test)]
    fn from_parts(override_home: Option<std::ffi::OsString>, user_home: Option<PathBuf>) -> Paths {
        Paths {
            home: smithagents_core::daemon::resolve_home(override_home, user_home),
        }
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

    /// Installed plugins live in `plugins_dir()/<id>/`.
    pub fn plugins_dir(&self) -> PathBuf {
        self.home.join("plugins")
    }

    /// Compiled WebAssembly components are cached here.
    pub fn wasm_cache_dir(&self) -> PathBuf {
        self.home.join("cache").join("wasm")
    }

    /// Creates the home with owner-only permissions (0700), tightening an
    /// existing one: it holds the chat transcripts and the token's lockfile.
    pub fn ensure_home(&self) -> io::Result<()> {
        ensure_private_dir(&self.home)
    }

    /// Creates the home (owner-only, see [`Paths::ensure_home`]), worktrees and
    /// database directories. The run directory is left to the daemon, which
    /// creates it with owner-only permissions.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        self.ensure_home()?;
        std::fs::create_dir_all(self.worktrees_dir())?;
        if let Some(parent) = self.db_path().parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}

/// Creates `dir` with mode 0700 (its missing parents with the default mode)
/// and sets 0700 on it if it already existed.
pub(crate) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    // The umask can only clear bits; this also tightens an existing directory.
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
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

    #[cfg(unix)]
    #[test]
    fn ensure_dirs_makes_home_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: tmp.path().join("new-home"),
        };
        paths.ensure_dirs().unwrap();
        assert_eq!(mode(&paths.home), 0o700);

        // An existing home that others can read is tightened.
        let paths = Paths {
            home: tmp.path().join("old-home"),
        };
        std::fs::create_dir(&paths.home).unwrap();
        std::fs::set_permissions(&paths.home, std::fs::Permissions::from_mode(0o755)).unwrap();
        paths.ensure_dirs().unwrap();
        assert_eq!(mode(&paths.home), 0o700);
    }
}
