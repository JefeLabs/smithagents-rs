//! The feedback draft: `draft.json` (version 1) and the draft's files under
//! `<home>/feedback/draft/`. Everything read from that directory is treated
//! as untrusted: a stored name never leaves the directory, and a damaged
//! draft is set aside instead of failing (only the newest set-aside draft is
//! kept). A draft that cannot be set aside is never written over, and a link
//! in the directory's place is never read, written or swept through. Files
//! the draft does not name are swept away. The draft's pending submission,
//! once there is one, is `submission.json` next to `draft.json`.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::annotation::Annotation;
use super::files::{FileKind, Usage};
use super::text::{
    removed_attachments_text, save_failed_text, BROKEN_DRAFT_TEXT, DRAFT_FOLDER_NOT_A_FOLDER,
    DRAFT_NOT_SET_ASIDE, MAX_ATTACHMENTS, MAX_BOXES,
};

pub const DRAFT_VERSION: u32 = 1;

const DRAFT_FILE: &str = "draft.json";
/// The draft's pending submission (see `state::Pending`).
pub const SUBMISSION_FILE: &str = "submission.json";
/// A set-aside draft is the whole directory renamed to this and a number.
const SET_ASIDE_PREFIX: &str = "draft.broken-";
/// A real draft is a few KB; anything bigger than this is not ours.
const MAX_DRAFT_JSON_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachmentId(String);

impl AttachmentId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AttachmentId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Screenshot,
    Image,
    Video,
}

impl AttachmentKind {
    fn prefix(self) -> &'static str {
        match self {
            AttachmentKind::Screenshot => "screenshot",
            AttachmentKind::Image => "image",
            AttachmentKind::Video => "clip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub kind: AttachmentKind,
    pub original_name: String,
    pub stored_name: String,
    pub size: u64,
    /// Screenshots only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boxes: Vec<Annotation>,
    /// Whether the annotated image matches the boxes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub rendered: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub version: u32,
    #[serde(default)]
    pub main_idea: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub attachments: Vec<Attachment>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            version: DRAFT_VERSION,
            main_idea: String::new(),
            kind: None,
            attachments: Vec::new(),
        }
    }
}

impl Draft {
    /// No attachments (a main idea may still be there: see [`Draft::is_blank`]).
    pub fn is_empty(&self) -> bool {
        self.attachments.is_empty()
    }

    /// Nothing worth keeping: no attachments and no main idea. Only a blank
    /// draft takes its directory, `draft.json` included, away.
    pub fn is_blank(&self) -> bool {
        self.attachments.is_empty() && self.main_idea.trim().is_empty()
    }

    pub fn usage(&self) -> Usage {
        Usage {
            count: self.attachments.len(),
            bytes: self
                .attachments
                .iter()
                .fold(0u64, |sum, a| sum.saturating_add(a.size)),
        }
    }

    pub fn get(&self, id: &AttachmentId) -> Option<&Attachment> {
        self.attachments.iter().find(|a| &a.id == id)
    }

    pub fn screenshot_count(&self) -> usize {
        self.screenshots().count()
    }

    /// 1-based position among the draft's screenshots.
    pub fn screenshot_number(&self, id: &AttachmentId) -> Option<usize> {
        self.screenshots().position(|a| &a.id == id).map(|i| i + 1)
    }

    /// `<prefix>-<n>.<ext>`, `n` one more than the highest number of that
    /// prefix in the draft.
    pub fn next_stored_name(&self, kind: AttachmentKind, file: FileKind) -> String {
        let head = format!("{}-", kind.prefix());
        let highest = self
            .attachments
            .iter()
            .filter_map(|a| {
                let rest = a.stored_name.strip_prefix(&head)?;
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                digits.parse::<u64>().ok()
            })
            .max()
            .unwrap_or(0);
        format!("{head}{}.{}", highest.saturating_add(1), file.extension())
    }

    fn screenshots(&self) -> impl Iterator<Item = &Attachment> {
        self.attachments
            .iter()
            .filter(|a| a.kind == AttachmentKind::Screenshot)
    }

    /// Every file name the draft uses: `draft.json`, `submission.json` and
    /// each attachment's stored, annotated and thumbnail files.
    pub fn file_names(&self) -> HashSet<String> {
        let mut names = HashSet::from([DRAFT_FILE.to_string(), SUBMISSION_FILE.to_string()]);
        for a in &self.attachments {
            names.insert(a.stored_name.clone());
            names.insert(annotated_name(&a.stored_name));
            names.insert(thumb_name(&a.stored_name));
        }
        names
    }
}

fn with_suffix(stored_name: &str, suffix: &str) -> String {
    let stem = Path::new(stored_name).file_stem().map_or_else(
        || stored_name.to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    format!("{stem}.{suffix}.png")
}

/// `screenshot-1.png` -> `screenshot-1.annotated.png`.
pub fn annotated_name(stored_name: &str) -> String {
    with_suffix(stored_name, "annotated")
}

/// `image-2.jpg` -> `image-2.thumb.png`.
pub fn thumb_name(stored_name: &str) -> String {
    with_suffix(stored_name, "thumb")
}

/// A plain file name: not empty, no separators, no leading dot, no NUL, and
/// exactly its own `Path::file_name`.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name != DRAFT_FILE
        && name != SUBMISSION_FILE
        && !name.contains(['/', '\\', '\0'])
        && Path::new(name).file_name().and_then(|n| n.to_str()) == Some(name)
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// A temporary file of an atomic write (`.<name>.<…>.tmp`).
fn is_temp_name(name: &OsString) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"))
}

/// The error every write answers while a damaged draft that could not be set
/// aside is in the draft's place.
fn stuck_error() -> io::Error {
    io::Error::other(DRAFT_NOT_SET_ASIDE)
}

#[derive(Debug, Clone)]
pub struct DraftStore {
    home: PathBuf,
    /// The last load found a damaged draft and could not set it aside: until
    /// a later load succeeds, nothing is written into or deleted from the
    /// draft directory. Shared by every clone.
    stuck: Arc<AtomicBool>,
}

pub struct Loaded {
    pub draft: Draft,
    pub notices: Vec<String>,
}

impl DraftStore {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            stuck: Arc::default(),
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.home.join("feedback").join("draft")
    }

    /// The path of a stored file. A name that is not a plain file name
    /// answers a path that does not exist, never one outside the directory.
    pub fn path_of(&self, stored_name: &str) -> PathBuf {
        if is_plain_name(stored_name) {
            self.dir().join(stored_name)
        } else {
            self.dir().join(".invalid-name")
        }
    }

    /// The draft directory, to write or delete in: refused while a damaged
    /// draft that could not be set aside is in its place, and when a link
    /// (or a file) is in its place, which is never written or swept through.
    /// A directory that is not there yet is fine: a write creates it.
    fn writable_dir(&self) -> io::Result<PathBuf> {
        if self.stuck.load(Ordering::SeqCst) {
            return Err(stuck_error());
        }
        let dir = self.dir();
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_dir() => Ok(dir),
            Ok(_) => Err(io::Error::other(DRAFT_FOLDER_NOT_A_FOLDER)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(dir),
            Err(e) => Err(e),
        }
    }

    /// Never fails: a draft that cannot be read is set aside and an empty one
    /// answered with a notice. A draft that cannot be set aside stays where
    /// it is, and the store refuses every write until a later load (the
    /// notice says the draft cannot be saved); so does a link in the draft
    /// directory's place, which is not read. Attachments whose files are
    /// gone are dropped first, then all but the first [`MAX_ATTACHMENTS`];
    /// then, once the cleaned draft is saved (or needed no saving), the
    /// files it does not name are swept away.
    pub fn load(&self) -> Loaded {
        self.stuck.store(false, Ordering::SeqCst);
        if fs::symlink_metadata(self.dir()).is_ok_and(|meta| !meta.file_type().is_dir()) {
            tracing::warn!("the feedback draft directory is not a folder; it is left alone");
            return Loaded {
                draft: Draft::default(),
                notices: vec![save_failed_text(DRAFT_FOLDER_NOT_A_FOLDER)],
            };
        }
        let json_path = self.dir().join(DRAFT_FILE);
        match fs::symlink_metadata(&json_path) {
            Ok(_) => {}
            Err(e) => {
                let draft = Draft::default();
                // Only a draft that is surely not there leaves files to sweep.
                if e.kind() == io::ErrorKind::NotFound {
                    self.sweep_files(&draft, true);
                }
                return Loaded {
                    draft,
                    notices: Vec::new(),
                };
            }
        }
        let Some(mut draft) = self.read_draft(&json_path) else {
            let notice = match self.set_aside() {
                Ok(()) => BROKEN_DRAFT_TEXT.to_string(),
                Err(e) => {
                    tracing::warn!(error = %e, "could not set aside the broken feedback draft");
                    self.stuck.store(true, Ordering::SeqCst);
                    save_failed_text(DRAFT_NOT_SET_ASIDE)
                }
            };
            return Loaded {
                draft: Draft::default(),
                notices: vec![notice],
            };
        };

        let before = draft.clone();
        let mut kept = Vec::with_capacity(draft.attachments.len().min(MAX_ATTACHMENTS));
        let mut missing = 0usize;
        for mut a in std::mem::take(&mut draft.attachments) {
            if !is_plain_name(&a.stored_name) || !is_regular_file(&self.path_of(&a.stored_name)) {
                missing += 1;
                continue;
            }
            if a.kind == AttachmentKind::Screenshot {
                let cleaned: Vec<Annotation> = a
                    .boxes
                    .iter()
                    .filter_map(|b| {
                        b.rect.sanitized().map(|rect| Annotation {
                            rect,
                            comment: b.comment.clone(),
                        })
                    })
                    .take(MAX_BOXES)
                    .collect();
                if cleaned != a.boxes {
                    a.boxes = cleaned;
                    a.rendered = false;
                }
                let annotated = self.path_of(&annotated_name(&a.stored_name));
                if a.rendered && !is_regular_file(&annotated) {
                    a.rendered = false;
                }
            }
            kept.push(a);
        }
        let over_limit = kept.len().saturating_sub(MAX_ATTACHMENTS);
        kept.truncate(MAX_ATTACHMENTS);
        draft.attachments = kept;
        let notices = removed_attachments_text(missing, over_limit)
            .into_iter()
            .collect();
        let saved = draft == before
            || match self.save(&draft) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "could not save the cleaned feedback draft");
                    false
                }
            };
        // The draft on disk still names what was dropped: nothing to sweep.
        if saved {
            // Nothing else writes yet: a temp file now is left from a crash.
            self.sweep_files(&draft, true);
        }
        Loaded { draft, notices }
    }

    /// Deletes the regular files in the draft directory that `draft` does
    /// not name ([`Draft::file_names`]); a blank draft ([`Draft::is_blank`])
    /// takes the directory with it. Temp files are kept: a write may be
    /// under way. Never follows a link, never enters a folder, never leaves
    /// the directory; does nothing while the store refuses writes or a link
    /// is in the directory's place. Errors are logged.
    pub fn sweep(&self, draft: &Draft) {
        self.sweep_files(draft, false);
    }

    fn sweep_files(&self, draft: &Draft, with_temps: bool) {
        let dir = match self.writable_dir() {
            Ok(dir) => dir,
            Err(e) => {
                tracing::debug!(error = %e, "the feedback draft is not swept");
                return;
            }
        };
        if draft.is_blank() {
            if let Err(e) = self.clear() {
                tracing::warn!(error = %e, "could not delete the blank feedback draft");
            }
            return;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(error = %e, "could not list the feedback draft");
                }
                return;
            }
        };
        let named: HashSet<OsString> = draft.file_names().into_iter().map(Into::into).collect();
        for entry in entries.flatten() {
            let name = entry.file_name();
            // The entry itself: a link is never followed, a folder never entered.
            let regular = entry.file_type().is_ok_and(|kind| kind.is_file());
            if !regular || named.contains(&name) || (!with_temps && is_temp_name(&name)) {
                continue;
            }
            match fs::remove_file(dir.join(&name)) {
                Ok(()) => {
                    tracing::debug!(file = ?name, "swept a feedback file the draft does not name")
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(error = %e, file = ?name, "could not sweep a feedback file")
                }
            }
        }
    }

    fn read_draft(&self, json_path: &Path) -> Option<Draft> {
        let meta = fs::symlink_metadata(json_path).ok()?;
        if !meta.file_type().is_file() || meta.len() > MAX_DRAFT_JSON_BYTES {
            return None;
        }
        let mut text = String::new();
        fs::File::open(json_path)
            .ok()?
            .take(MAX_DRAFT_JSON_BYTES + 1)
            .read_to_string(&mut text)
            .ok()?;
        let draft: Draft = serde_json::from_str(&text).ok()?;
        (draft.version == DRAFT_VERSION).then_some(draft)
    }

    /// Renames the whole draft directory to `draft.broken-<unix seconds>`
    /// (`-2`, `-3` when taken), then deletes the older set-aside drafts: they
    /// hold screenshots of the window, and only the newest is kept.
    fn set_aside(&self) -> io::Result<()> {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let parent = self.home.join("feedback");
        for n in 1u32..=1000 {
            let name = if n == 1 {
                format!("{SET_ASIDE_PREFIX}{secs}")
            } else {
                format!("{SET_ASIDE_PREFIX}{secs}-{n}")
            };
            let target = parent.join(name);
            if fs::symlink_metadata(&target).is_ok() {
                continue;
            }
            fs::rename(self.dir(), &target)?;
            Self::drop_older_set_asides(&parent, &target);
            return Ok(());
        }
        Err(io::Error::other("no free name to set the draft aside"))
    }

    /// Deletes every set-aside draft in `parent` but `newest`. Only real
    /// folders named like one: a link or a file is left alone.
    fn drop_older_set_asides(parent: &Path, newest: &Path) {
        let Ok(entries) = fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let set_aside = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(SET_ASIDE_PREFIX));
            let folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if !set_aside || !folder || path == newest {
                continue;
            }
            if let Err(e) = fs::remove_dir_all(&path) {
                tracing::warn!(error = %e, "could not delete an older set-aside feedback draft");
            }
        }
    }

    /// Atomic: the old `draft.json` stays readable when this fails.
    pub fn save(&self, draft: &Draft) -> io::Result<()> {
        let dir = self.writable_dir()?;
        let json = serde_json::to_vec_pretty(draft).map_err(io::Error::other)?;
        crate::atomic_file::write_private(&dir, DRAFT_FILE, &json)
    }

    /// Atomic. A name that is not a plain file name is refused.
    pub fn write_file(&self, stored_name: &str, bytes: &[u8]) -> io::Result<()> {
        let dir = self.writable_dir()?;
        if !is_plain_name(stored_name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a plain file name",
            ));
        }
        crate::atomic_file::write_private(&dir, stored_name, bytes)
    }

    /// Removes the file and its annotated and thumbnail files; errors are
    /// logged, a missing file is not an error.
    pub fn remove_file(&self, stored_name: &str) {
        if !is_plain_name(stored_name) {
            return;
        }
        self.remove_named(&[
            stored_name.to_string(),
            annotated_name(stored_name),
            thumb_name(stored_name),
        ]);
    }

    /// Removes these files of the draft directory (a name that is not a
    /// plain file name is skipped); errors are logged, a missing file is not
    /// an error. Nothing while the store refuses writes or a link is in the
    /// directory's place.
    pub fn remove_named(&self, names: &[String]) {
        let Ok(dir) = self.writable_dir() else {
            return;
        };
        for name in names.iter().filter(|name| is_plain_name(name)) {
            if let Err(e) = fs::remove_file(dir.join(name)) {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(error = %e, file = %name, "could not remove a feedback file");
                }
            }
        }
    }

    /// `submission.json`'s bytes; `None` when there is none, or it is not a
    /// regular file of a sensible size.
    pub fn read_submission(&self) -> Option<Vec<u8>> {
        let path = self.dir().join(SUBMISSION_FILE);
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.file_type().is_file() || meta.len() > MAX_DRAFT_JSON_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .ok()?
            .take(MAX_DRAFT_JSON_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    }

    /// Atomic, like [`DraftStore::save`].
    pub fn write_submission(&self, bytes: &[u8]) -> io::Result<()> {
        let dir = self.writable_dir()?;
        crate::atomic_file::write_private(&dir, SUBMISSION_FILE, bytes)
    }

    /// Removes `submission.json`; a missing one is not an error.
    pub fn remove_submission(&self) -> io::Result<()> {
        let dir = self.writable_dir()?;
        match fs::remove_file(dir.join(SUBMISSION_FILE)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Removes the draft directory; refused when a link (or a file) is in
    /// its place.
    pub fn clear(&self) -> io::Result<()> {
        let dir = self.writable_dir()?;
        match fs::remove_dir_all(dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}
