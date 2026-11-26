//! A submission's upload: its limits, the checks of the request part, file
//! types told by their leading bytes, and the parts staged on disk as they
//! stream in.
//!
//! The body is `multipart/form-data`: the part `request` (JSON) first, then
//! `file-<i>` for every attachment and `annotated-<i>` for every screenshot
//! (`i` is the attachment's 1-based position). Nothing a client sends is a
//! path: each file is written under the name of its part, which is one of the
//! names the daemon expects, and the part's own file name is never read.
//! Sizes are counted while the parts stream, and a part is abandoned as soon
//! as it passes its limit. Every sentence names the attachment by number.

use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use axum::extract::multipart::{Field, Multipart, MultipartError};
use axum::http::StatusCode;
use smithagents_core::feedback::{
    AttachmentKind, FeedbackBox, FeedbackRequest, SendAs, SubmissionId,
};
use tokio::io::AsyncWriteExt;

use super::issue::{title_of, FileKind, MAIN_IDEA_MAX_CHARS};
use super::FeedbackError;
use crate::paths::ensure_private_real_dir;
use crate::plugins::clean_message;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;
/// The most boxes one screenshot can have.
pub const MAX_BOXES: usize = 20;
/// The longest comment of a box, in characters.
pub const COMMENT_MAX_CHARS: usize = 500;
/// The longest app version or platform.
const FIELD_MAX_CHARS: usize = 32;
/// The most characters of an unexpected part's name a sentence shows.
const PART_NAME_SHOWN: usize = 40;
/// How many leading bytes tell a file's type.
const SNIFF_BYTES: usize = 12;
/// A box may pass the right or bottom edge by this much (rounding).
const EDGE: f32 = 1.0001;
const REQUEST_PART: &str = "request";
/// The staged parts' directory inside a submission's scratch directory.
const UPLOAD_DIR: &str = "upload";
const NOT_MULTIPART: &str = "the upload is not valid multipart form data";

/// What one submission may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadLimits {
    pub attachments: usize,
    /// A screenshot, its annotated image, or an image.
    pub image_bytes: u64,
    pub video_bytes: u64,
    /// Every file part together.
    pub total_bytes: u64,
    /// The `request` part.
    pub request_bytes: u64,
}

impl Default for UploadLimits {
    /// The design's: 10 attachments, 10 MB per image, 50 MB per video,
    /// 100 MB in all (binary megabytes), and 256 KiB of request.
    fn default() -> Self {
        UploadLimits {
            attachments: 10,
            image_bytes: 10 * MIB,
            video_bytes: 50 * MIB,
            total_bytes: 100 * MIB,
            request_bytes: 256 * KIB,
        }
    }
}

impl UploadLimits {
    /// The route's body limit: every file part, and 1 MiB for the request
    /// part and the boundaries.
    pub fn body_limit(&self) -> usize {
        usize::try_from(self.total_bytes.saturating_add(MIB)).unwrap_or(usize::MAX)
    }
}

/// The type of a file from its leading bytes: PNG, JPEG, GIF (87a or 89a),
/// WebP (`RIFF` … `WEBP`) or MP4 (`ftyp` at offset 4).
pub fn sniff(leading: &[u8]) -> Option<FileKind> {
    if leading.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(FileKind::Png)
    } else if leading.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(FileKind::Jpeg)
    } else if leading.starts_with(b"GIF87a") || leading.starts_with(b"GIF89a") {
        Some(FileKind::Gif)
    } else if leading.len() >= 12 && leading.starts_with(b"RIFF") && &leading[8..12] == b"WEBP" {
        Some(FileKind::Webp)
    } else if leading.len() >= 8 && &leading[4..8] == b"ftyp" {
        Some(FileKind::Mp4)
    } else {
        None
    }
}

fn invalid(message: impl Into<String>) -> FeedbackError {
    FeedbackError::Invalid(message.into())
}

/// Checks everything the request says before any file is read; returns the
/// title.
pub fn check_request(
    request: &FeedbackRequest,
    limits: &UploadLimits,
) -> Result<String, FeedbackError> {
    let title = title_of(&request.main_idea).ok_or_else(|| invalid("the main idea is empty"))?;
    if request.main_idea.chars().count() > MAIN_IDEA_MAX_CHARS {
        return Err(invalid(format!(
            "the main idea must be at most {MAIN_IDEA_MAX_CHARS} characters"
        )));
    }
    if request.attachments.len() > limits.attachments {
        return Err(invalid(format!(
            "at most {} attachments can be sent",
            limits.attachments
        )));
    }
    for (at, attachment) in request.attachments.iter().enumerate() {
        let i = at + 1;
        let boxes = &attachment.boxes;
        if attachment.kind != AttachmentKind::Screenshot && !boxes.is_empty() {
            return Err(invalid(format!(
                "attachment {i}: only a screenshot can have boxes"
            )));
        }
        if boxes.len() > MAX_BOXES {
            return Err(invalid(format!(
                "attachment {i} has more than {MAX_BOXES} boxes"
            )));
        }
        for b in boxes {
            if !inside_the_image(b) {
                return Err(invalid(format!(
                    "attachment {i}: a box is outside the image"
                )));
            }
            if b.comment.chars().count() > COMMENT_MAX_CHARS {
                return Err(invalid(format!(
                    "attachment {i}: a comment is longer than {COMMENT_MAX_CHARS} characters"
                )));
            }
        }
    }
    if !is_plain_field(&request.app_version) {
        return Err(invalid("the app version is not valid"));
    }
    if !is_plain_field(&request.platform) {
        return Err(invalid("the platform is not valid"));
    }
    if let Some(idea) = request.idea {
        if request.send_as != SendAs::Intent {
            return Err(invalid(
                "an idea package needs an intent, not a GitHub issue",
            ));
        }
        if !(idea.explainer || idea.diagrams || idea.demo_video) {
            return Err(invalid("choose at least one part of the idea package"));
        }
    }
    Ok(title)
}

/// Every field finite and in 0..=1, and the box inside the image.
fn inside_the_image(b: &FeedbackBox) -> bool {
    let fields = [b.x, b.y, b.w, b.h];
    fields
        .iter()
        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        && b.x + b.w <= EDGE
        && b.y + b.h <= EDGE
}

/// `[A-Za-z0-9._+-]{1,32}`.
fn is_plain_field(text: &str) -> bool {
    (1..=FIELD_MAX_CHARS).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

/// `{n} MB` for a whole number of MiB, else `{n} bytes`.
fn size_text(bytes: u64) -> String {
    if bytes > 0 && bytes.is_multiple_of(MIB) {
        format!("{} MB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// `{n} KiB` for a whole number of KiB, else `{n} bytes`.
fn kib_text(bytes: u64) -> String {
    if bytes > 0 && bytes.is_multiple_of(KIB) {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// A submission whose parts are all on disk.
pub struct Staged {
    pub request: FeedbackRequest,
    pub title: String,
    /// In attachment order, each attachment's file before its annotated one.
    pub files: Vec<StagedFile>,
    pub dir: StagingDir,
}

/// One file part on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    /// The attachment's 1-based position, as in the part's name.
    pub attachment: usize,
    /// The `annotated-<i>` part (a screenshot's annotated image).
    pub annotated: bool,
    pub kind: FileKind,
    pub path: PathBuf,
    pub size: u64,
}

/// A submission's scratch directory, `<root>/<submission id>-<random>`, with
/// the staged parts in `upload/`. Owner-only; removed with everything in it
/// when dropped (the request ended, failed or was dropped).
#[derive(Debug)]
pub struct StagingDir {
    path: PathBuf,
}

impl StagingDir {
    /// Creates a new one directly under `root` (made owner-only; a symbolic
    /// link or another user's directory is refused).
    fn create(root: &Path, id: SubmissionId) -> io::Result<StagingDir> {
        ensure_private_real_dir(root)?;
        let path = root.join(format!("{id}-{}", uuid::Uuid::new_v4().simple()));
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&path)?;
        let dir = StagingDir { path };
        builder.create(dir.upload_dir())?;
        Ok(dir)
    }

    /// The scratch directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn upload_dir(&self) -> PathBuf {
        self.path.join(UPLOAD_DIR)
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                tracing::warn!("could not remove a feedback upload directory: {e}");
            }
            _ => {}
        }
    }
}

/// A part the request calls for.
struct Expected {
    name: String,
    attachment: usize,
    annotated: bool,
    kind: AttachmentKind,
    received: bool,
}

fn expected_parts(request: &FeedbackRequest) -> Vec<Expected> {
    let mut parts = Vec::new();
    for (at, a) in request.attachments.iter().enumerate() {
        let i = at + 1;
        parts.push(Expected {
            name: format!("file-{i}"),
            attachment: i,
            annotated: false,
            kind: a.kind,
            received: false,
        });
        if a.kind == AttachmentKind::Screenshot {
            parts.push(Expected {
                name: format!("annotated-{i}"),
                attachment: i,
                annotated: true,
                kind: a.kind,
                received: false,
            });
        }
    }
    parts
}

/// Reads the parts in order: the request (checked before any file is read),
/// then each file part, written to `<root>/<submission id>-<random>/upload/`
/// as it arrives. On any failure the directory is removed again.
pub async fn stage(
    mut multipart: Multipart,
    root: &Path,
    limits: &UploadLimits,
) -> Result<Staged, FeedbackError> {
    let json = {
        let mut field = match multipart
            .next_field()
            .await
            .map_err(|e| upload_error(&e, limits))?
        {
            Some(field) if field.name() == Some(REQUEST_PART) => field,
            _ => return Err(invalid("the request part must come first")),
        };
        read_request(&mut field, limits).await?
    };
    let request: FeedbackRequest = serde_json::from_slice(&json)
        .map_err(|_| invalid("the request part is not a valid feedback request"))?;
    let title = check_request(&request, limits)?;

    let dir = StagingDir::create(root, request.submission_id)?;
    let upload = dir.upload_dir();
    let mut expected = expected_parts(&request);
    let mut files = Vec::with_capacity(expected.len());
    let mut total = 0u64;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| upload_error(&e, limits))?
    {
        let name = field.name().unwrap_or_default();
        if name == REQUEST_PART {
            return Err(invalid("part request was sent twice"));
        }
        let Some(part) = expected.iter_mut().find(|p| p.name == name) else {
            return Err(invalid(format!("unexpected part {}", shown_name(name))));
        };
        if part.received {
            return Err(invalid(format!("part {} was sent twice", part.name)));
        }
        part.received = true;
        let file = receive(&mut field, part, &upload, &mut total, limits).await?;
        files.push(file);
    }
    if let Some(missing) = expected.iter().find(|p| !p.received) {
        return Err(invalid(format!(
            "attachment {} was not sent",
            missing.attachment
        )));
    }
    files.sort_by_key(|f| (f.attachment, f.annotated));
    Ok(Staged {
        request,
        title,
        files,
        dir,
    })
}

/// A part's name as a sentence may show it: cleaned, at most 40 characters.
fn shown_name(name: &str) -> String {
    let head: String = name.chars().take(4 * PART_NAME_SHOWN).collect();
    clean_message(&head).chars().take(PART_NAME_SHOWN).collect()
}

/// A failure of the multipart stream: a body over the route's limit is the
/// submission's size sentence; anything else is not multipart.
fn upload_error(e: &MultipartError, limits: &UploadLimits) -> FeedbackError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        too_large_in_all(limits)
    } else {
        invalid(NOT_MULTIPART)
    }
}

/// A body that is not `multipart/form-data` (or not readable as it).
pub fn not_multipart() -> FeedbackError {
    invalid(NOT_MULTIPART)
}

fn too_large_in_all(limits: &UploadLimits) -> FeedbackError {
    FeedbackError::TooLarge(format!(
        "the submission is larger than {}",
        size_text(limits.total_bytes)
    ))
}

/// The request part's bytes, refused as soon as they pass `request_bytes`.
async fn read_request(
    field: &mut Field<'_>,
    limits: &UploadLimits,
) -> Result<Vec<u8>, FeedbackError> {
    let mut json = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|e| upload_error(&e, limits))? {
        if (json.len() + chunk.len()) as u64 > limits.request_bytes {
            return Err(FeedbackError::TooLarge(format!(
                "the request part is larger than {}",
                kib_text(limits.request_bytes)
            )));
        }
        json.extend_from_slice(&chunk);
    }
    Ok(json)
}

/// Streams one file part to `<upload>/<part name>`, counting its size and
/// the submission's while it arrives, and checking its type once its first
/// bytes are in.
async fn receive(
    field: &mut Field<'_>,
    part: &Expected,
    upload: &Path,
    total: &mut u64,
    limits: &UploadLimits,
) -> Result<StagedFile, FeedbackError> {
    let limit = match part.kind {
        AttachmentKind::Video => limits.video_bytes,
        AttachmentKind::Screenshot | AttachmentKind::Image => limits.image_bytes,
    };
    let path = upload.join(&part.name);
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .await?;
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    let mut kind = None;
    let mut size = 0u64;
    while let Some(chunk) = field.chunk().await.map_err(|e| upload_error(&e, limits))? {
        size += chunk.len() as u64;
        *total += chunk.len() as u64;
        if size > limit {
            return Err(FeedbackError::TooLarge(format!(
                "attachment {} is larger than {}",
                part.attachment,
                size_text(limit)
            )));
        }
        if *total > limits.total_bytes {
            return Err(too_large_in_all(limits));
        }
        if kind.is_none() {
            let wanted = SNIFF_BYTES - head.len();
            head.extend_from_slice(&chunk[..wanted.min(chunk.len())]);
            if head.len() == SNIFF_BYTES {
                kind = Some(file_kind(part, &head)?);
            }
        }
        file.write_all(&chunk).await?;
    }
    let kind = match kind {
        Some(kind) => kind,
        None => file_kind(part, &head)?,
    };
    file.flush().await?;
    Ok(StagedFile {
        attachment: part.attachment,
        annotated: part.annotated,
        kind,
        path,
        size,
    })
}

/// The part's type when it is one its attachment may have: a screenshot's
/// two files are PNG, an image PNG, JPEG, GIF or WebP, a video MP4.
fn file_kind(part: &Expected, leading: &[u8]) -> Result<FileKind, FeedbackError> {
    let found = sniff(leading);
    let fits = match part.kind {
        AttachmentKind::Screenshot => found == Some(FileKind::Png),
        AttachmentKind::Image => matches!(
            found,
            Some(FileKind::Png | FileKind::Jpeg | FileKind::Gif | FileKind::Webp)
        ),
        AttachmentKind::Video => found == Some(FileKind::Mp4),
    };
    match found {
        Some(kind) if fits => Ok(kind),
        _ if part.kind == AttachmentKind::Screenshot => Err(FeedbackError::UnsupportedFile(
            format!("screenshot {} is not a png file", part.attachment),
        )),
        _ => Err(FeedbackError::UnsupportedFile(format!(
            "attachment {} is not a png, jpg, gif, webp or mp4 file",
            part.attachment
        ))),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::FromRequest;
    use axum::http::Request;
    use smithagents_core::feedback::{FeedbackAttachment, FeedbackKind, IdeaOptions};
    use smithagents_core::ids::RepoId;

    use super::*;

    #[test]
    fn default_limits_are_the_designs() {
        let limits = UploadLimits::default();
        assert_eq!(limits.attachments, 10);
        assert_eq!(limits.image_bytes, 10 * 1024 * 1024);
        assert_eq!(limits.video_bytes, 50 * 1024 * 1024);
        assert_eq!(limits.total_bytes, 100 * 1024 * 1024);
        assert_eq!(limits.request_bytes, 256 * 1024);
        assert_eq!(limits.body_limit(), 100 * 1024 * 1024 + 1024 * 1024);
    }

    #[test]
    fn file_types_come_from_their_leading_bytes() {
        let cases: [(&[u8], Option<FileKind>); 10] = [
            (b"\x89PNG\r\n\x1a\n\0\0\0\r", Some(FileKind::Png)),
            (b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01", Some(FileKind::Jpeg)),
            (b"GIF87a\x01\0\x01\0\0\0", Some(FileKind::Gif)),
            (b"GIF89a\x01\0\x01\0\0\0", Some(FileKind::Gif)),
            (b"RIFF\x24\0\0\0WEBP", Some(FileKind::Webp)),
            (b"\0\0\0\x18ftypmp42", Some(FileKind::Mp4)),
            (b"RIFF\x24\0\0\0WAVE", None),
            (b"\x89PNG\r\n\x1a", None),
            (b"plain text!!", None),
            (b"", None),
        ];
        for (bytes, kind) in cases {
            assert_eq!(sniff(bytes), kind, "{bytes:?}");
        }
    }

    #[test]
    fn sizes_are_printed_from_the_limit() {
        assert_eq!(size_text(10 * MIB), "10 MB");
        assert_eq!(size_text(1024), "1024 bytes");
        assert_eq!(size_text(MIB + 1), "1048577 bytes");
        assert_eq!(kib_text(256 * KIB), "256 KiB");
        assert_eq!(kib_text(1000), "1000 bytes");
    }

    fn request() -> FeedbackRequest {
        FeedbackRequest {
            submission_id: SubmissionId::new(),
            main_idea: "Fix the zoom".into(),
            kind: FeedbackKind::Bug,
            repo_id: RepoId::new(),
            send_as: SendAs::Intent,
            idea: None,
            app_version: "0.1.0+dev".into(),
            platform: "macos-14_arm".into(),
            attachments: vec![],
        }
    }

    fn shot(boxes: Vec<FeedbackBox>) -> FeedbackAttachment {
        FeedbackAttachment {
            kind: AttachmentKind::Screenshot,
            original_name: "a.png".into(),
            boxes,
        }
    }

    fn a_box(x: f32, y: f32, w: f32, h: f32) -> FeedbackBox {
        FeedbackBox {
            x,
            y,
            w,
            h,
            comment: String::new(),
        }
    }

    fn refusal(r: &FeedbackRequest) -> String {
        match check_request(r, &UploadLimits::default()) {
            Err(FeedbackError::Invalid(message)) => message,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_valid_request_gives_its_title() {
        let mut r = request();
        r.main_idea = "## Fix the zoom\n\nThe page is too big.".into();
        r.attachments = vec![shot(vec![a_box(0.5, 0.5, 0.5, 0.50005)])];
        assert_eq!(
            check_request(&r, &UploadLimits::default()).unwrap(),
            "Fix the zoom"
        );
    }

    #[test]
    fn boxes_must_lie_inside_the_image() {
        for b in [
            a_box(-0.1, 0.0, 0.1, 0.1),
            a_box(0.0, 1.5, 0.1, 0.1),
            a_box(f32::NAN, 0.0, 0.1, 0.1),
            a_box(0.0, 0.0, f32::INFINITY, 0.1),
            a_box(0.9, 0.0, 0.2, 0.1),
            a_box(0.0, 0.9, 0.1, 0.1002),
        ] {
            let mut r = request();
            r.attachments = vec![shot(vec![b.clone()])];
            assert_eq!(
                refusal(&r),
                "attachment 1: a box is outside the image",
                "{b:?}"
            );
        }
    }

    #[test]
    fn fields_and_the_idea_package_are_checked() {
        for platform in ["", "a b", "x;rm", &"a".repeat(33), "é"] {
            let mut r = request();
            r.platform = platform.into();
            assert_eq!(refusal(&r), "the platform is not valid", "{platform:?}");
        }
        let mut r = request();
        r.app_version = "1.0\n".into();
        assert_eq!(refusal(&r), "the app version is not valid");
        let mut r = request();
        r.idea = Some(IdeaOptions {
            explainer: false,
            diagrams: false,
            demo_video: false,
        });
        assert_eq!(refusal(&r), "choose at least one part of the idea package");
        r.send_as = SendAs::GithubIssue;
        assert_eq!(
            refusal(&r),
            "an idea package needs an intent, not a GitHub issue"
        );
    }

    async fn upload(body: Vec<u8>) -> Multipart {
        let req = Request::builder()
            .header("content-type", "multipart/form-data; boundary=b0undary")
            .body(Body::from(body))
            .unwrap();
        Multipart::from_request(req, &()).await.unwrap()
    }

    fn body(request: &FeedbackRequest, files: &[(&str, &[u8])]) -> Vec<u8> {
        let json = serde_json::to_vec(request).unwrap();
        let mut out = Vec::new();
        let parts = std::iter::once((REQUEST_PART, json.as_slice())).chain(files.iter().copied());
        for (name, data) in parts {
            out.extend_from_slice(
                format!("--b0undary\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                    .as_bytes(),
            );
            out.extend_from_slice(data);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"--b0undary--\r\n");
        out
    }

    #[tokio::test]
    async fn staging_dirs_are_private_unique_and_removed_when_dropped() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tmp").join("feedback");
        let mut r = request();
        r.attachments = vec![shot(vec![])];
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRmore bytes";
        let files: [(&str, &[u8]); 2] = [("annotated-1", png), ("file-1", png)];
        let limits = UploadLimits::default();

        let a = stage(upload(body(&r, &files)).await, &root, &limits)
            .await
            .unwrap();
        let b = stage(upload(body(&r, &files)).await, &root, &limits)
            .await
            .unwrap();

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        for staged in [&a, &b] {
            let dir = staged.dir.path();
            assert_eq!(dir.parent(), Some(root.as_path()));
            let name = dir.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with(&format!("{}-", r.submission_id)), "{name}");
            assert_eq!(mode(dir), 0o700);
            let names: Vec<(usize, bool)> = staged
                .files
                .iter()
                .map(|f| (f.attachment, f.annotated))
                .collect();
            assert_eq!(names, [(1, false), (1, true)]);
            for f in &staged.files {
                assert_eq!(f.path.parent(), Some(dir.join("upload").as_path()));
                assert_eq!(std::fs::read(&f.path).unwrap(), png);
                assert_eq!((f.kind, f.size), (FileKind::Png, png.len() as u64));
                assert_eq!(mode(&f.path), 0o600);
            }
        }
        assert_ne!(a.dir.path(), b.dir.path());
        let (a_dir, b_dir) = (a.dir.path().to_path_buf(), b.dir.path().to_path_buf());
        drop(a);
        assert!(!a_dir.exists());
        assert!(b_dir.exists());
        drop(b);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_failed_stage_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("feedback");
        let mut r = request();
        r.attachments = vec![shot(vec![])];
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRmore bytes";
        let files: [(&str, &[u8]); 1] = [("file-1", png)];
        let failed = stage(
            upload(body(&r, &files)).await,
            &root,
            &UploadLimits::default(),
        )
        .await;
        assert!(
            matches!(&failed, Err(FeedbackError::Invalid(m)) if m == "attachment 1 was not sent"),
            "{:?}",
            failed.err()
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
}
