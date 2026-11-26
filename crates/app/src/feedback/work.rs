//! The feedback work that needs no entity: the draft's files on disk or in
//! memory, and the jobs the [`Feedback`](super::state::Feedback) entity runs
//! off the UI thread (taking the picture, copying a picked file in, drawing
//! the annotated image, reading a thumbnail).

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use gpui_kit::RenderImage;
use image::{ImageError, RgbaImage};
use smithagents_capture::{checked, CaptureError, Capturer, WindowTarget, WRONG_SIZE_CAPTURE};

use super::annotation::Annotation;
use super::draft::{
    annotated_name, thumb_name, Attachment, AttachmentId, AttachmentKind, Draft, DraftStore, Loaded,
};
use super::files::{admit, Refusal, Usage};
use super::render::{annotate, decode, encode_png, image_from_screenshot, thumbnail};
use super::text::{
    capture_failed_text, save_failed_text, MAX_IMAGE_BYTES, MAX_VIDEO_BYTES, PERMISSION_TEXT,
    UNSUPPORTED_TEXT, WINDOW_NOT_FOUND_TEXT,
};

/// The sentence for a draft write or delete the disk refused.
pub(super) fn io_text(error: &io::Error) -> String {
    save_failed_text(&error.to_string())
}

/// The bytes of `path`, at most `limit + 1` of them: one more than allowed
/// tells a file that is too large apart from one at the limit.
pub(super) fn read_up_to(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Where the draft's files live: the store's directory, or memory.
#[derive(Clone)]
pub(super) enum Files {
    Disk(DraftStore),
    Memory(Arc<Mutex<HashMap<String, Arc<[u8]>>>>),
}

impl Files {
    fn memory(
        map: &Mutex<HashMap<String, Arc<[u8]>>>,
    ) -> MutexGuard<'_, HashMap<String, Arc<[u8]>>> {
        // A panic while holding the lock left a valid map behind.
        map.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn load(&self) -> Loaded {
        match self {
            Files::Disk(store) => store.load(),
            Files::Memory(_) => Loaded {
                draft: Draft::default(),
                notices: Vec::new(),
            },
        }
    }

    pub(super) fn save(&self, draft: &Draft) -> io::Result<()> {
        match self {
            Files::Disk(store) => store.save(draft),
            Files::Memory(_) => Ok(()),
        }
    }

    pub(super) fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        match self {
            Files::Disk(store) => store.write_file(name, bytes),
            Files::Memory(map) => {
                Self::memory(map).insert(name.to_string(), bytes.into());
                Ok(())
            }
        }
    }

    /// A stored file of at most `limit` bytes; a regular file only.
    pub(super) fn read(&self, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        let bytes = match self {
            Files::Disk(store) => {
                let path = store.path_of(name);
                if !fs::symlink_metadata(&path)?.file_type().is_file() {
                    return Err(io::Error::other("not a regular file"));
                }
                read_up_to(&path, limit)?
            }
            Files::Memory(map) => Self::memory(map)
                .get(name)
                .map(|bytes| bytes.to_vec())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?,
        };
        if bytes.len() as u64 > limit {
            return Err(io::Error::other("the file is too large"));
        }
        Ok(bytes)
    }

    /// The stored file and its annotated and thumbnail files.
    pub(super) fn remove(&self, stored_name: &str) {
        match self {
            Files::Disk(store) => store.remove_file(stored_name),
            Files::Memory(map) => {
                let mut map = Self::memory(map);
                map.remove(stored_name);
                map.remove(&annotated_name(stored_name));
                map.remove(&thumb_name(stored_name));
            }
        }
    }

    pub(super) fn clear(&self) -> io::Result<()> {
        match self {
            Files::Disk(store) => store.clear(),
            Files::Memory(map) => {
                Self::memory(map).clear();
                Ok(())
            }
        }
    }
}

/// Decoded pixels the views can draw (GPUI wants BGRA).
pub(super) fn render_image(mut rgba: RgbaImage) -> Arc<RenderImage> {
    // Whole pixels: an RgbaImage holds 4 bytes for each.
    let (pixels, _) = rgba.as_chunks_mut::<4>();
    for pixel in pixels {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))
}

#[derive(Debug)]
pub(super) enum JobError {
    Cancelled,
    Failed(String),
}

impl From<io::Error> for JobError {
    fn from(error: io::Error) -> Self {
        JobError::Failed(error.to_string())
    }
}

impl From<ImageError> for JobError {
    fn from(error: ImageError) -> Self {
        JobError::Failed(error.to_string())
    }
}

/// Draws `boxes` over the stored original, writes the annotated image and its
/// thumbnail, and answers the thumbnail.
pub(super) fn redraw(
    files: &Files,
    stored_name: &str,
    boxes: &[Annotation],
    cancel: &AtomicBool,
) -> Result<Arc<RenderImage>, JobError> {
    let original = decode(&files.read(stored_name, MAX_IMAGE_BYTES)?)?;
    let annotated = annotate(&original, boxes);
    let annotated_png = encode_png(&annotated)?;
    let small = thumbnail(&annotated);
    let small_png = encode_png(&small)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(JobError::Cancelled);
    }
    files.write(&annotated_name(stored_name), &annotated_png)?;
    files.write(&thumb_name(stored_name), &small_png)?;
    Ok(render_image(small))
}

/// A stored thumbnail, decoded; `None` when there is none that decodes.
pub(super) fn read_thumbnail(files: &Files, stored_name: &str) -> Option<Arc<RenderImage>> {
    let name = thumb_name(stored_name);
    let bytes = files.read(&name, MAX_IMAGE_BYTES).ok()?;
    match decode(&bytes) {
        Ok(image) => Some(render_image(image)),
        Err(e) => {
            tracing::warn!(error = %e, file = %name, "a feedback thumbnail does not decode");
            None
        }
    }
}

/// A capture, encoded.
pub(super) struct Picture {
    pub(super) png: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
}

/// Takes the picture and encodes it (off the UI thread).
pub(super) fn take_picture(
    capturer: Option<&dyn Capturer>,
    target: &WindowTarget,
) -> Result<Picture, CaptureError> {
    let capturer = capturer.ok_or(CaptureError::Unsupported)?;
    let shot = checked(capturer.capture_window(target)?)?;
    let (width, height) = (shot.width, shot.height);
    let image = image_from_screenshot(shot)
        .ok_or_else(|| CaptureError::Failed(WRONG_SIZE_CAPTURE.to_string()))?;
    let png = encode_png(&image).map_err(|e| CaptureError::Failed(e.to_string()))?;
    Ok(Picture { png, width, height })
}

pub(super) fn capture_error_text(error: &CaptureError) -> String {
    match error {
        CaptureError::PermissionDenied => PERMISSION_TEXT.to_string(),
        CaptureError::WindowNotFound => WINDOW_NOT_FOUND_TEXT.to_string(),
        CaptureError::Unsupported => UNSUPPORTED_TEXT.to_string(),
        CaptureError::Failed(message) => capture_failed_text(message),
    }
}

/// What became of one picked file.
pub(super) enum Copied {
    Added {
        attachment: Attachment,
        thumbnail: Option<Arc<RenderImage>>,
        /// The thumbnail file could not be written (the copy was).
        thumbnail_error: Option<String>,
    },
    /// The sentence naming the file and why it was not added.
    Refused(String),
    /// The copy could not be written: the sentence to show.
    Failed(String),
    Cancelled,
}

/// The first 12 bytes of `path` (fewer for a shorter file).
fn read_leading(path: &Path) -> io::Result<Vec<u8>> {
    let mut leading = Vec::with_capacity(12);
    fs::File::open(path)?.take(12).read_to_end(&mut leading)?;
    Ok(leading)
}

/// Checks one picked file against `usage` and copies it into the draft's
/// files under a name `draft` does not use yet (off the UI thread).
pub(super) fn copy_in(
    files: &Files,
    path: &Path,
    usage: Usage,
    draft: &Draft,
    cancel: &AtomicBool,
) -> Copied {
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned();
    let refused = |refusal: Refusal| Copied::Refused(refusal.sentence(&name));
    // A folder, a device or a pipe is not a file to read.
    let size = match fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return refused(Refusal::Unreadable),
    };
    let Ok(leading) = read_leading(path) else {
        return refused(Refusal::Unreadable);
    };
    let kind = match admit(&name, &leading, size, usage) {
        Ok(kind) => kind,
        Err(refusal) => return refused(refusal),
    };
    let limit = if kind.is_video() {
        MAX_VIDEO_BYTES
    } else {
        MAX_IMAGE_BYTES
    };
    let Ok(bytes) = read_up_to(path, limit) else {
        return refused(Refusal::Unreadable);
    };
    // The file may have changed since it was checked: check what was read.
    let leading = &bytes[..bytes.len().min(12)];
    let kind = match admit(&name, leading, bytes.len() as u64, usage) {
        Ok(read_kind) if read_kind == kind => kind,
        Ok(_) => return refused(Refusal::Contents),
        Err(refusal) => return refused(refusal),
    };
    let attachment_kind = if kind.is_video() {
        AttachmentKind::Video
    } else {
        AttachmentKind::Image
    };
    let stored_name = draft.next_stored_name(attachment_kind, kind);
    // A picture that does not decode is still added, without a thumbnail.
    let small = (!kind.is_video())
        .then(|| decode(&bytes))
        .and_then(|decoded| match decoded {
            Ok(image) => Some(thumbnail(&image)),
            Err(e) => {
                tracing::debug!(error = %e, file = %name, "no thumbnail for a picked file");
                None
            }
        });
    if cancel.load(Ordering::Relaxed) {
        return Copied::Cancelled;
    }
    if let Err(e) = files.write(&stored_name, &bytes) {
        tracing::warn!(error = %e, file = %stored_name, "could not copy a picked file");
        return Copied::Failed(io_text(&e));
    }
    let mut thumbnail_error = None;
    let thumbnail = small.map(|small| {
        let written = encode_png(&small)
            .map_err(|e| e.to_string())
            .and_then(|png| {
                files
                    .write(&thumb_name(&stored_name), &png)
                    .map_err(|e| e.to_string())
            });
        if let Err(e) = written {
            tracing::warn!(error = %e, file = %stored_name, "could not write a thumbnail");
            thumbnail_error = Some(save_failed_text(&e));
        }
        render_image(small)
    });
    Copied::Added {
        attachment: Attachment {
            id: AttachmentId::new(),
            kind: attachment_kind,
            original_name: name,
            stored_name,
            size: bytes.len() as u64,
            width: None,
            height: None,
            boxes: Vec::new(),
            rendered: false,
        },
        thumbnail,
        thumbnail_error,
    }
}
