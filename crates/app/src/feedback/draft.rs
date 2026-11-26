//! The feedback draft: `draft.json` (version 1) and the draft's files under
//! `<home>/feedback/draft/`. Everything read from that directory is treated
//! as untrusted: a stored name never leaves the directory, and a damaged
//! draft is set aside instead of failing.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::annotation::Annotation;
use super::files::{FileKind, Usage};
use super::text::{missing_files_text, BROKEN_DRAFT_TEXT, MAX_ATTACHMENTS, MAX_BOXES};

pub const DRAFT_VERSION: u32 = 1;

const DRAFT_FILE: &str = "draft.json";
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
    pub fn is_empty(&self) -> bool {
        self.attachments.is_empty()
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
        && !name.contains(['/', '\\', '\0'])
        && Path::new(name).file_name().and_then(|n| n.to_str()) == Some(name)
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

#[derive(Debug, Clone)]
pub struct DraftStore {
    home: PathBuf,
}

pub struct Loaded {
    pub draft: Draft,
    pub notices: Vec<String>,
}

impl DraftStore {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
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

    /// Never fails: a draft that cannot be read is set aside and an empty one
    /// answered with a notice.
    pub fn load(&self) -> Loaded {
        let json_path = self.dir().join(DRAFT_FILE);
        if fs::symlink_metadata(&json_path).is_err() {
            return Loaded {
                draft: Draft::default(),
                notices: Vec::new(),
            };
        }
        let Some(mut draft) = self.read_draft(&json_path) else {
            self.set_aside();
            return Loaded {
                draft: Draft::default(),
                notices: vec![BROKEN_DRAFT_TEXT.to_string()],
            };
        };

        let before = draft.clone();
        let total = draft.attachments.len();
        draft.attachments.truncate(MAX_ATTACHMENTS);
        let mut kept = Vec::with_capacity(draft.attachments.len());
        let mut dropped = 0usize;
        for mut a in std::mem::take(&mut draft.attachments) {
            if !is_plain_name(&a.stored_name) || !is_regular_file(&self.path_of(&a.stored_name)) {
                dropped += 1;
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
        draft.attachments = kept;
        let mut notices = Vec::new();
        if dropped > 0 {
            notices.push(missing_files_text(dropped));
        }
        if draft != before || total > MAX_ATTACHMENTS {
            if let Err(e) = self.save(&draft) {
                tracing::warn!(error = %e, "could not save the cleaned feedback draft");
            }
        }
        Loaded { draft, notices }
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
    /// (`-2`, `-3` when taken).
    fn set_aside(&self) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let parent = self.home.join("feedback");
        for n in 1u32..=1000 {
            let name = if n == 1 {
                format!("draft.broken-{secs}")
            } else {
                format!("draft.broken-{secs}-{n}")
            };
            let target = parent.join(name);
            if fs::symlink_metadata(&target).is_ok() {
                continue;
            }
            if let Err(e) = fs::rename(self.dir(), &target) {
                tracing::warn!(error = %e, "could not set aside the broken feedback draft");
            }
            return;
        }
        tracing::warn!("no free name to set aside the broken feedback draft");
    }

    /// Atomic: the old `draft.json` stays readable when this fails.
    pub fn save(&self, draft: &Draft) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(draft).map_err(io::Error::other)?;
        crate::atomic_file::write_private(&self.dir(), DRAFT_FILE, &json)
    }

    /// Atomic. A name that is not a plain file name is refused.
    pub fn write_file(&self, stored_name: &str, bytes: &[u8]) -> io::Result<()> {
        if !is_plain_name(stored_name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a plain file name",
            ));
        }
        crate::atomic_file::write_private(&self.dir(), stored_name, bytes)
    }

    /// Removes the file and its annotated and thumbnail files; errors are
    /// logged, a missing file is not an error.
    pub fn remove_file(&self, stored_name: &str) {
        if !is_plain_name(stored_name) {
            return;
        }
        for name in [
            stored_name.to_string(),
            annotated_name(stored_name),
            thumb_name(stored_name),
        ] {
            if let Err(e) = fs::remove_file(self.path_of(&name)) {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(error = %e, file = %name, "could not remove a feedback file");
                }
            }
        }
    }

    /// Removes the draft directory.
    pub fn clear(&self) -> io::Result<()> {
        match fs::remove_dir_all(self.dir()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}
