//! Checking an attached file by its name, its leading bytes and its size,
//! before anything is copied.

use super::text::{MAX_ATTACHMENTS, MAX_IMAGE_BYTES, MAX_TOTAL_BYTES, MAX_VIDEO_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Mp4,
}

impl FileKind {
    /// The extension a stored copy gets.
    pub fn extension(self) -> &'static str {
        match self {
            FileKind::Png => "png",
            FileKind::Jpeg => "jpg",
            FileKind::Gif => "gif",
            FileKind::Webp => "webp",
            FileKind::Mp4 => "mp4",
        }
    }

    pub fn is_video(self) -> bool {
        self == FileKind::Mp4
    }
}

/// The type the leading bytes say; needs at most the first 12.
pub fn sniff(leading: &[u8]) -> Option<FileKind> {
    if leading.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(FileKind::Png)
    } else if leading.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(FileKind::Jpeg)
    } else if leading.starts_with(b"GIF87a") || leading.starts_with(b"GIF89a") {
        Some(FileKind::Gif)
    } else if leading.get(..4) == Some(b"RIFF") && leading.get(8..12) == Some(b"WEBP") {
        Some(FileKind::Webp)
    } else if leading.get(4..8) == Some(b"ftyp") {
        Some(FileKind::Mp4)
    } else {
        None
    }
}

/// The type the file name's extension names, in any case; `jpeg` and `jpg`
/// are both [`FileKind::Jpeg`].
pub fn kind_from_name(file_name: &str) -> Option<FileKind> {
    let (_, extension) = file_name.rsplit_once('.')?;
    match extension.to_ascii_lowercase().as_str() {
        "png" => Some(FileKind::Png),
        "jpg" | "jpeg" => Some(FileKind::Jpeg),
        "gif" => Some(FileKind::Gif),
        "webp" => Some(FileKind::Webp),
        "mp4" => Some(FileKind::Mp4),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Extension,
    Contents,
    ImageTooLarge,
    VideoTooLarge,
    TotalTooLarge,
    TooMany,
    Unreadable,
}

impl Refusal {
    /// The sentence shown to the person, naming the file.
    pub fn sentence(self, file_name: &str) -> String {
        let reason = match self {
            Refusal::Extension => "only png, jpg, gif, webp and mp4 files can be attached.",
            Refusal::Contents => "its contents are not a png, jpg, gif, webp or mp4 file.",
            Refusal::ImageTooLarge => "an image can be at most 10 MB.",
            Refusal::VideoTooLarge => "a video can be at most 50 MB.",
            Refusal::TotalTooLarge => {
                "the draft would be larger than 100 MB to send (a screenshot counts twice)."
            }
            Refusal::TooMany => "a draft holds at most 10 attachments.",
            Refusal::Unreadable => "it could not be read.",
        };
        format!("{file_name} was not added: {reason}")
    }
}

/// What the draft already holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub count: usize,
    /// The bytes its attachments take in an upload, where a screenshot
    /// counts twice (`draft::upload_bytes`): what the total limit is about.
    pub bytes: u64,
}

/// Checks in this order: `TooMany`, `Extension`, `Contents` (no known type, or
/// not the extension's type), the size limit of its kind, `TotalTooLarge`.
pub fn admit(
    file_name: &str,
    leading: &[u8],
    size: u64,
    usage: Usage,
) -> Result<FileKind, Refusal> {
    if usage.count >= MAX_ATTACHMENTS {
        return Err(Refusal::TooMany);
    }
    let named = kind_from_name(file_name).ok_or(Refusal::Extension)?;
    if sniff(leading) != Some(named) {
        return Err(Refusal::Contents);
    }
    if named.is_video() {
        if size > MAX_VIDEO_BYTES {
            return Err(Refusal::VideoTooLarge);
        }
    } else if size > MAX_IMAGE_BYTES {
        return Err(Refusal::ImageTooLarge);
    }
    if usage.bytes.saturating_add(size) > MAX_TOTAL_BYTES {
        return Err(Refusal::TotalTooLarge);
    }
    Ok(named)
}
