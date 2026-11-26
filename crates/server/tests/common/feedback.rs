//! Fake `gh` and `agent-browser` for the feedback tests: the scripts in
//! `tests/bin`, copied into a test's own directory, which is the only place a
//! [`ToolFinder`] from [`FakeTools::finder`] looks. Nothing installed on the
//! machine is found or run.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use smithagents_plugin_host::ToolFinder;

const FAKE_GH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bin/fake-gh");
const FAKE_AGENT_BROWSER: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bin/fake-agent-browser");

/// One test's fakes. `bin` holds `gh` and/or `agent-browser`; `dir` is the
/// fake `gh`'s control directory (what `FAKE_GH_DIR` names; the fake falls
/// back to `<bin>/../gh`, which is `dir`, when the variable is unset).
pub struct FakeTools {
    pub bin: PathBuf,
    pub dir: PathBuf,
}

impl FakeTools {
    /// Copies the chosen fakes into `<root>/bin` (mode 0755) and creates
    /// `<root>/gh`. Called again with the same root, it adds fakes and keeps
    /// what the control directory holds.
    pub fn new(root: &Path, gh: bool, agent_browser: bool) -> Self {
        let (bin, dir) = (root.join("bin"), root.join("gh"));
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&dir).unwrap();
        for (wanted, source, name) in [
            (gh, FAKE_GH, "gh"),
            (agent_browser, FAKE_AGENT_BROWSER, "agent-browser"),
        ] {
            if wanted {
                let to = bin.join(name);
                fs::copy(source, &to).unwrap();
                fs::set_permissions(&to, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        FakeTools { bin, dir }
    }

    /// A finder that searches `bin` only.
    pub fn finder(&self) -> ToolFinder {
        ToolFinder::new(vec![self.bin.clone()])
    }

    /// Every run of the fake `gh` so far, in order: its arguments joined by
    /// spaces.
    pub fn calls(&self) -> Vec<String> {
        self.read("calls.log")
            .map(|log| log.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Writes the control file `file` (`fail`, `hang`, `signed-out`,
    /// `visibility`, `label`, `existing`).
    pub fn set(&self, file: &str, text: &str) {
        fs::write(self.dir.join(file), text).unwrap();
    }

    /// A file of the control directory (a control file, `calls.log`,
    /// `env.log`, `issue-body.md`, `pr-body.md`), if it exists.
    pub fn read(&self, file: &str) -> Option<String> {
        fs::read_to_string(self.dir.join(file)).ok()
    }
}
