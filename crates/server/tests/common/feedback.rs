//! Fake `gh` and `agent-browser` for the feedback tests: the scripts in
//! `tests/bin`, copied into a test's own directory, which is the only place a
//! [`ToolFinder`] from [`FakeTools::finder`] looks. Nothing installed on the
//! machine is found or run. Also the pieces of a feedback upload: multipart
//! bodies, file contents and the request part.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use smithagents_core::ids::{RepoId, SubmissionId};
use smithagents_plugin_host::ToolFinder;

/// The boundary of every body [`multipart`] makes; no file the tests send
/// holds it.
pub const BOUNDARY: &str = "smithagents-test-boundary-7d1f0c";

/// A `multipart/form-data` body of `parts` (name, contents) in order, and its
/// content type. Every part but `request` carries its own name as its file
/// name.
pub fn multipart(parts: &[(&str, &[u8])]) -> (String, Vec<u8>) {
    let named: Vec<(&str, Option<&str>, &[u8])> = parts
        .iter()
        .map(|(name, data)| (*name, (*name != "request").then_some(*name), *data))
        .collect();
    multipart_named(&named)
}

/// [`multipart`] with each part's file name given (`None`: no file name).
pub fn multipart_named(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    for (name, file_name, data) in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let disposition = match file_name {
            Some(f) => {
                format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n")
            }
            None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n"),
        };
        body.extend_from_slice(disposition.as_bytes());
        let content_type = if *name == "request" {
            "application/json"
        } else {
            "application/octet-stream"
        };
        body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

/// `n` bytes from `seed` onward, never holding the boundary.
fn filler(n: usize, seed: u8) -> impl Iterator<Item = u8> {
    (0..n).map(move |i| seed.wrapping_add((i % 251) as u8))
}

/// A file of `bytes` bytes (at least 16) that starts like a PNG.
pub fn png(bytes: usize) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    let rest = bytes.saturating_sub(out.len());
    out.extend(filler(rest, 3));
    out
}

/// A small file that starts like a JPEG.
pub fn jpeg() -> Vec<u8> {
    let mut out = b"\xff\xd8\xff\xe0\0\x10JFIF\0".to_vec();
    out.extend(filler(200, 7));
    out
}

/// A file of `bytes` bytes (at least 16) that starts like an MP4.
pub fn mp4(bytes: usize) -> Vec<u8> {
    let mut out = b"\0\0\0\x18ftypmp42\0\0\0\0".to_vec();
    let rest = bytes.saturating_sub(out.len());
    out.extend(filler(rest, 11));
    out
}

/// A request part for `repo` with no attachments: main idea "Fix the
/// zoom\n\nThe page is too big.", kind `bug`, a new submission id.
pub fn request_json(repo: RepoId, send_as: &str) -> Value {
    json!({
        "submission_id": SubmissionId::new(),
        "main_idea": "Fix the zoom\n\nThe page is too big.",
        "kind": "bug",
        "repo_id": repo,
        "send_as": send_as,
        "app_version": "0.1.0",
        "platform": "macos",
        "attachments": [],
    })
}

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

    /// Writes the control file `file` (`fail`, `hang`, `wait`, `go`,
    /// `signed-out`, `visibility`, `visibility-<name>`, `label`, `existing`,
    /// `existing-head`, `issue-url`).
    pub fn set(&self, file: &str, text: &str) {
        fs::write(self.dir.join(file), text).unwrap();
    }

    /// A file of the control directory (a control file, `calls.log`,
    /// `env.log`, `cwd.log`, `hang.pid`, `wait.pid`, `issue-body.md`,
    /// `pr-body.md`), if it exists.
    pub fn read(&self, file: &str) -> Option<String> {
        fs::read_to_string(self.dir.join(file)).ok()
    }
}
