//! The feedback draft as the app holds it. One [`Feedback`] entity owns the
//! draft and its notices, runs the capture sequence against a [`Capturer`],
//! adds picked files, saves boxes as they change and draws the annotated
//! image on Done. Everything that reaches the world (the draft directory,
//! the capturer, the file picker, the next frame) comes in through
//! [`FeedbackOptions`], so tests replace all of it.
//!
//! Image work and file copies run off the UI thread. Work for an attachment
//! that is removed or discarded meanwhile is cancelled: its result is
//! dropped, and work already running on another thread writes nothing more
//! once it sees the cancel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use gpui_kit::{
    App, AppContext as _, Context, EventEmitter, PathPromptOptions, RenderImage, Task, Window,
};
use smithagents_capture::{CaptureError, Capturer, Frame, WindowTarget};

use super::annotation::Annotation;
use super::draft::{Attachment, AttachmentId, AttachmentKind, Draft, DraftStore, Loaded};
use super::files::{FileKind, Refusal};
use super::render::decode;
use super::text::{
    picker_failed_text, save_failed_text, DRAFT_FULL_TEXT, MAX_ATTACHMENTS, MAX_BOXES,
    MAX_IMAGE_BYTES, MAX_TOTAL_BYTES,
};
use super::work::{
    capture_error_text, copy_in, io_text, read_thumbnail, redraw, render_image, take_picture,
    Copied, Files, JobError, Picture,
};

/// Asks the user for files. Ok(None) is "cancelled"; Err carries the reason the picker failed.
pub type FilePicker =
    Rc<dyn Fn(&mut App) -> LocalBoxFuture<'static, Result<Option<Vec<PathBuf>>, String>>>;
/// Runs the callback at the start of the next frame (research B3). The default is `window.on_next_frame`.
pub type FrameHop = Rc<dyn Fn(&mut Window, Box<dyn FnOnce(&mut Window, &mut App)>)>;

/// The system's open-file dialog: files only, several, prompt "Add" (research B5). Used only by lib.rs.
///
/// Crate-private, so no test can open the dialog: tests pass their own
/// [`FilePicker`]. The dialog has no type filter; [`Feedback::add_files`]
/// checks each file.
pub(crate) fn native_file_picker() -> FilePicker {
    Rc::new(|cx: &mut App| {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add".into()),
        });
        async move {
            match picked.await {
                Ok(Ok(paths)) => Ok(paths),
                Ok(Err(e)) => Err(e.to_string()),
                // The dialog went away without an answer.
                Err(_) => Ok(None),
            }
        }
        .boxed_local()
    })
}

/// What a [`Feedback`] works with. The default reaches nothing: no files, no
/// capture, no dialog.
#[derive(Default)]
pub struct FeedbackOptions {
    /// Where the draft lives; `None` keeps the draft and its files in memory only.
    pub store: Option<DraftStore>,
    /// Takes the picture; `None` answers every capture with [`CaptureError::Unsupported`].
    pub capturer: Option<Arc<dyn Capturer>>,
    /// Asks for files to add; `None` answers "cancelled".
    pub picker: Option<FilePicker>,
    /// Waits for the next frame; `None` is `window.on_next_frame`.
    pub frame_hop: Option<FrameHop>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackEvent {
    /// Something to read in the Feedback pane (a capture that added nothing).
    RevealPane,
    /// Annotate this screenshot.
    OpenAnnotator(AttachmentId),
    /// The annotator is done (Done, or its screenshot is gone).
    CloseAnnotator,
}

/// Work running off the UI thread. Dropping it cancels it: the task stops,
/// and work already running elsewhere writes nothing more.
struct Work {
    cancel: Arc<AtomicBool>,
    _task: Task<()>,
}

impl Drop for Work {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Image work for one attachment: drawing its annotated image, or reading
/// its thumbnail.
struct Job {
    /// Done is waiting for this drawing to close the annotator.
    closes: bool,
    _work: Work,
}

/// The feedback draft, its notices and the work running on it.
pub struct Feedback {
    files: Files,
    capturer: Option<Arc<dyn Capturer>>,
    picker: Option<FilePicker>,
    frame_hop: FrameHop,
    draft: Draft,
    notices: Vec<String>,
    /// From `capture` until the picture is in the draft or refused.
    capturing: bool,
    capture: Option<Task<()>>,
    annotating: Option<AttachmentId>,
    /// The picked files being checked and copied.
    adding: Option<Work>,
    jobs: HashMap<AttachmentId, Job>,
    thumbnails: HashMap<AttachmentId, Arc<RenderImage>>,
    /// Thumbnails no longer shown, for the pane to release.
    retired: Vec<Arc<RenderImage>>,
}

impl EventEmitter<FeedbackEvent> for Feedback {}

impl Feedback {
    /// Loads the draft (its notices become [`Feedback::notices`]), then, off
    /// the UI thread, redraws every screenshot whose annotated image does not
    /// match its boxes and reads every thumbnail.
    pub fn new(options: FeedbackOptions, cx: &mut Context<Self>) -> Self {
        let FeedbackOptions {
            store,
            capturer,
            picker,
            frame_hop,
        } = options;
        let files = match store {
            Some(store) => Files::Disk(store),
            None => Files::Memory(Arc::default()),
        };
        let Loaded { draft, notices } = files.load();
        let mut this = Self {
            files,
            capturer,
            picker,
            frame_hop: frame_hop.unwrap_or_else(|| {
                Rc::new(|window: &mut Window, callback| window.on_next_frame(callback))
            }),
            draft,
            notices,
            capturing: false,
            capture: None,
            annotating: None,
            adding: None,
            jobs: HashMap::new(),
            thumbnails: HashMap::new(),
            retired: Vec::new(),
        };
        let starting: Vec<(AttachmentId, AttachmentKind, bool)> = this
            .draft
            .attachments
            .iter()
            .map(|a| (a.id.clone(), a.kind, a.rendered))
            .collect();
        for (id, kind, rendered) in starting {
            match kind {
                AttachmentKind::Video => {}
                AttachmentKind::Screenshot if !rendered => this.start_redraw(id, false, cx),
                AttachmentKind::Screenshot | AttachmentKind::Image => {
                    this.start_thumbnail_read(id, cx)
                }
            }
        }
        this
    }

    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    /// The draft's directory; `None` when the draft is kept in memory.
    pub fn store(&self) -> Option<&DraftStore> {
        match &self.files {
            Files::Disk(store) => Some(store),
            Files::Memory(_) => None,
        }
    }

    /// What the person should read, newest last. The next capture or pick
    /// that reports replaces them; a failed save adds to them.
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    pub fn capturing(&self) -> bool {
        self.capturing
    }

    /// Image work or a file copy is running ([`Feedback::capturing`] covers
    /// a capture).
    pub fn busy(&self) -> bool {
        self.adding.is_some() || !self.jobs.is_empty()
    }

    /// The screenshot the annotator shows.
    pub fn annotating(&self) -> Option<&AttachmentId> {
        self.annotating.as_ref()
    }

    /// Takes a picture of the window, two frames from now so that what
    /// `capture` changed on screen (the hold's ring) is drawn away first.
    /// Ignored while a capture runs or the annotator is open. A full draft
    /// says so without capturing. Success adds the screenshot and opens the
    /// annotator; a failure says why and reveals the pane.
    pub fn capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.capturing || self.annotating.is_some() {
            return;
        }
        if self.draft.attachments.len() >= MAX_ATTACHMENTS {
            self.notices = vec![DRAFT_FULL_TEXT.to_string()];
            cx.emit(FeedbackEvent::RevealPane);
            cx.notify();
            return;
        }
        tracing::debug!("feedback capture: waiting two frames");
        self.capturing = true;
        cx.notify();
        let this = cx.weak_entity();
        let hop = self.frame_hop.clone();
        let second_hop = hop.clone();
        hop(
            window,
            Box::new(move |window, _| {
                second_hop(
                    window,
                    Box::new(move |window, cx| {
                        let _ = this.update(cx, |this, cx| this.start_capture(window, cx));
                    }),
                );
            }),
        );
    }

    fn start_capture(&mut self, window: &Window, cx: &mut Context<Self>) {
        let bounds = window.bounds();
        let target = WindowTarget {
            pid: std::process::id(),
            frame: Frame {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
            },
            scale: window.scale_factor(),
        };
        tracing::debug!(?target, "feedback capture: taking the picture");
        let capturer = self.capturer.clone();
        let picture =
            cx.background_spawn(async move { take_picture(capturer.as_deref(), &target) });
        self.capture = Some(cx.spawn(async move |this, cx| {
            let picture = picture.await;
            let _ = this.update(cx, |this, cx| this.finish_capture(picture, cx));
        }));
    }

    fn finish_capture(&mut self, picture: Result<Picture, CaptureError>, cx: &mut Context<Self>) {
        self.capturing = false;
        self.capture = None;
        match self.add_screenshot(picture) {
            Ok(id) => {
                self.annotating = Some(id.clone());
                cx.emit(FeedbackEvent::OpenAnnotator(id));
            }
            Err(sentence) => {
                self.notices = vec![sentence];
                cx.emit(FeedbackEvent::RevealPane);
            }
        }
        cx.notify();
    }

    /// Adds the picture to the draft; `Err` is the sentence that says why not.
    fn add_screenshot(
        &mut self,
        picture: Result<Picture, CaptureError>,
    ) -> Result<AttachmentId, String> {
        let picture = picture.map_err(|e| {
            tracing::warn!(error = %e, "feedback capture failed");
            capture_error_text(&e)
        })?;
        // The draft may have filled up while the picture was taken.
        let usage = self.draft.usage();
        if usage.count >= MAX_ATTACHMENTS {
            return Err(DRAFT_FULL_TEXT.to_string());
        }
        let size = picture.png.len() as u64;
        if size > MAX_IMAGE_BYTES {
            return Err(Refusal::ImageTooLarge.sentence("The screenshot"));
        }
        if usage.bytes.saturating_add(size) > MAX_TOTAL_BYTES {
            return Err(Refusal::TotalTooLarge.sentence("The screenshot"));
        }
        let stored_name = self
            .draft
            .next_stored_name(AttachmentKind::Screenshot, FileKind::Png);
        self.files.write(&stored_name, &picture.png).map_err(|e| {
            tracing::warn!(error = %e, file = %stored_name, "could not write the screenshot");
            io_text(&e)
        })?;
        let id = AttachmentId::new();
        self.draft.attachments.push(Attachment {
            id: id.clone(),
            kind: AttachmentKind::Screenshot,
            original_name: stored_name.clone(),
            stored_name,
            size,
            width: Some(picture.width),
            height: Some(picture.height),
            boxes: Vec::new(),
            rendered: false,
        });
        self.notices.clear();
        self.save();
        Ok(id)
    }

    /// Asks the picker for files and adds them one by one, in order. Each
    /// refused file gets its own sentence; the files before and after it
    /// are still added. Ignored while an earlier pick is being added.
    pub fn add_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding.is_some() {
            return;
        }
        let picked = match &self.picker {
            Some(picker) => picker(cx),
            None => async { Ok(None) }.boxed_local(),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let paths = match picked.await {
                Ok(Some(paths)) => paths,
                other => {
                    let _ = this.update(cx, |this, cx| {
                        this.adding = None;
                        if let Err(e) = other {
                            tracing::warn!(error = %e, "the file picker failed");
                            this.notices = vec![picker_failed_text(&e)];
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            tracing::debug!(
                count = paths.len(),
                "adding picked files to the feedback draft"
            );
            let _ = this.update(cx, |this, cx| {
                this.notices.clear();
                cx.notify();
            });
            for path in paths {
                // What the draft holds now, files added earlier in this pick included.
                let Ok((files, usage, draft)) = this.update(cx, |this, _| {
                    (this.files.clone(), this.draft.usage(), this.draft.clone())
                }) else {
                    return;
                };
                let cancelled = cancelled.clone();
                let copied = cx
                    .background_spawn(
                        async move { copy_in(&files, &path, usage, &draft, &cancelled) },
                    )
                    .await;
                if this
                    .update(cx, |this, cx| this.finish_copy(copied, cx))
                    .is_err()
                {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.adding = None;
                cx.notify();
            });
        });
        self.adding = Some(Work {
            cancel,
            _task: task,
        });
        cx.notify();
    }

    fn finish_copy(&mut self, copied: Copied, cx: &mut Context<Self>) {
        match copied {
            Copied::Refused(sentence) => self.notices.push(sentence),
            Copied::Failed(sentence) => self.add_notice(sentence),
            Copied::Cancelled => {}
            Copied::Added {
                attachment,
                thumbnail,
                thumbnail_error,
            } => {
                // A capture may have landed while the file was copied.
                let usage = self.draft.usage();
                let late_refusal = if usage.count >= MAX_ATTACHMENTS {
                    Some(Refusal::TooMany)
                } else if usage.bytes.saturating_add(attachment.size) > MAX_TOTAL_BYTES {
                    Some(Refusal::TotalTooLarge)
                } else {
                    None
                };
                if let Some(refusal) = late_refusal {
                    self.files.remove(&attachment.stored_name);
                    self.notices
                        .push(refusal.sentence(&attachment.original_name));
                } else {
                    let id = attachment.id.clone();
                    self.draft.attachments.push(attachment);
                    if let Some(thumbnail) = thumbnail {
                        self.set_thumbnail(id, thumbnail);
                    }
                    if let Some(sentence) = thumbnail_error {
                        self.add_notice(sentence);
                    }
                    self.save();
                }
            }
        }
        cx.notify();
    }

    /// Removes an attachment and its files; its thumbnail is retired.
    pub fn remove(&mut self, id: &AttachmentId, cx: &mut Context<Self>) {
        let Some(index) = self.draft.attachments.iter().position(|a| &a.id == id) else {
            return;
        };
        self.jobs.remove(id);
        let removed = self.draft.attachments.remove(index);
        self.files.remove(&removed.stored_name);
        self.retire(id);
        self.save();
        if self.annotating.as_ref() == Some(id) {
            self.close_annotator(cx);
        }
        cx.notify();
    }

    /// Empties the draft and deletes its directory. Files still being added
    /// are dropped; every thumbnail is retired.
    pub fn discard_all(&mut self, cx: &mut Context<Self>) {
        self.adding = None;
        self.jobs.clear();
        self.retired
            .extend(self.thumbnails.drain().map(|(_, thumbnail)| thumbnail));
        self.draft = Draft::default();
        if let Err(e) = self.files.clear() {
            tracing::warn!(error = %e, "could not delete the feedback draft");
            self.add_notice(io_text(&e));
            // Whatever is left, the next start finds an empty draft.
            self.save();
        }
        self.close_annotator(cx);
        cx.notify();
    }

    /// Opens the annotator on a screenshot (not while capturing or already
    /// annotating).
    pub fn open_annotator(&mut self, id: &AttachmentId, cx: &mut Context<Self>) {
        if self.capturing || self.annotating.is_some() {
            return;
        }
        let is_screenshot = self
            .draft
            .get(id)
            .is_some_and(|a| a.kind == AttachmentKind::Screenshot);
        if !is_screenshot {
            return;
        }
        self.annotating = Some(id.clone());
        cx.emit(FeedbackEvent::OpenAnnotator(id.clone()));
        cx.notify();
    }

    /// Replaces a screenshot's boxes (at most [`MAX_BOXES`]) and saves; its
    /// annotated image no longer matches until Done draws it.
    pub fn set_boxes(
        &mut self,
        id: &AttachmentId,
        mut boxes: Vec<Annotation>,
        cx: &mut Context<Self>,
    ) {
        let Some(shot) = self
            .draft
            .attachments
            .iter_mut()
            .find(|a| &a.id == id && a.kind == AttachmentKind::Screenshot)
        else {
            return;
        };
        boxes.truncate(MAX_BOXES);
        shot.boxes = boxes;
        shot.rendered = false;
        self.save();
        cx.notify();
    }

    /// Done: draws the annotated image when it does not match the boxes,
    /// then closes the annotator.
    pub fn finish_annotating(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.annotating.clone() else {
            return;
        };
        if self.jobs.get(&id).is_some_and(|job| job.closes) {
            // Done was already pressed.
            return;
        }
        let drawn = self.draft.get(&id).is_none_or(|shot| shot.rendered);
        if drawn {
            self.close_annotator(cx);
        } else {
            self.start_redraw(id, true, cx);
        }
        cx.notify();
    }

    /// Removes the screenshot being annotated and closes the annotator.
    pub fn discard_annotated(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.annotating.clone() else {
            return;
        };
        self.remove(&id, cx);
        self.close_annotator(cx);
    }

    /// The attachment's thumbnail, once there is one.
    pub fn thumbnail(&self, id: &AttachmentId) -> Option<Arc<RenderImage>> {
        self.thumbnails.get(id).cloned()
    }

    /// The stored original (of a screenshot or an image), decoded off the UI
    /// thread; `None` for a video or a file that does not decode.
    pub fn load_image(
        &self,
        id: &AttachmentId,
        cx: &mut Context<Self>,
    ) -> Task<Option<Arc<RenderImage>>> {
        let Some(attachment) = self
            .draft
            .get(id)
            .filter(|a| a.kind != AttachmentKind::Video)
        else {
            return Task::ready(None);
        };
        let files = self.files.clone();
        let name = attachment.stored_name.clone();
        cx.background_spawn(async move {
            let decoded = files
                .read(&name, MAX_IMAGE_BYTES)
                .map_err(JobError::from)
                .and_then(|bytes| decode(&bytes).map_err(JobError::from));
            match decoded {
                Ok(image) => Some(render_image(image)),
                Err(e) => {
                    tracing::warn!(error = ?e, file = %name, "could not load a feedback image");
                    None
                }
            }
        })
    }

    /// Thumbnails no longer shown, each answered once; the pane releases them.
    pub fn take_retired_images(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.retired)
    }

    fn save(&mut self) {
        if let Err(e) = self.files.save(&self.draft) {
            tracing::warn!(error = %e, "could not save the feedback draft");
            self.add_notice(io_text(&e));
        }
    }

    /// Adds a notice unless it is already there.
    fn add_notice(&mut self, sentence: String) {
        if !self.notices.contains(&sentence) {
            self.notices.push(sentence);
        }
    }

    fn close_annotator(&mut self, cx: &mut Context<Self>) {
        if self.annotating.take().is_some() {
            cx.emit(FeedbackEvent::CloseAnnotator);
        }
    }

    fn set_thumbnail(&mut self, id: AttachmentId, thumbnail: Arc<RenderImage>) {
        if let Some(old) = self.thumbnails.insert(id, thumbnail) {
            self.retired.push(old);
        }
    }

    fn retire(&mut self, id: &AttachmentId) {
        if let Some(old) = self.thumbnails.remove(id) {
            self.retired.push(old);
        }
    }

    /// Draws the screenshot's annotated image and thumbnail off the UI
    /// thread, replacing (and so cancelling) other work for it.
    fn start_redraw(&mut self, id: AttachmentId, closes: bool, cx: &mut Context<Self>) {
        let Some(shot) = self.draft.get(&id) else {
            return;
        };
        let (stored_name, boxes) = (shot.stored_name.clone(), shot.boxes.clone());
        let files = self.files.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let drawn = boxes.clone();
        let drawing =
            cx.background_spawn(async move { redraw(&files, &stored_name, &boxes, &cancelled) });
        let key = id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = drawing.await;
            let _ = this.update(cx, |this, cx| this.finish_redraw(key, drawn, result, cx));
        });
        self.jobs.insert(
            id,
            Job {
                closes,
                _work: Work {
                    cancel,
                    _task: task,
                },
            },
        );
    }

    fn finish_redraw(
        &mut self,
        id: AttachmentId,
        drawn: Vec<Annotation>,
        result: Result<Arc<RenderImage>, JobError>,
        cx: &mut Context<Self>,
    ) {
        let closes = self.jobs.remove(&id).is_some_and(|job| job.closes);
        match result {
            Ok(thumbnail) => match self.draft.attachments.iter_mut().find(|a| a.id == id) {
                // Removed meanwhile (its work is dropped with it, so rarely here).
                None => {}
                Some(shot) if shot.boxes != drawn => {
                    // The boxes changed while drawing: draw them again.
                    self.start_redraw(id, closes, cx);
                    return;
                }
                Some(shot) => {
                    shot.rendered = true;
                    self.set_thumbnail(id, thumbnail);
                    self.save();
                }
            },
            Err(JobError::Cancelled) => {}
            Err(JobError::Failed(e)) => {
                tracing::warn!(error = %e, "could not draw the annotated screenshot");
                self.add_notice(save_failed_text(&e));
            }
        }
        if closes {
            self.close_annotator(cx);
        }
        cx.notify();
    }

    /// Reads the attachment's thumbnail file off the UI thread.
    fn start_thumbnail_read(&mut self, id: AttachmentId, cx: &mut Context<Self>) {
        let Some(attachment) = self.draft.get(&id) else {
            return;
        };
        let stored_name = attachment.stored_name.clone();
        let files = self.files.clone();
        let reading = cx.background_spawn(async move { read_thumbnail(&files, &stored_name) });
        let key = id.clone();
        let task = cx.spawn(async move |this, cx| {
            let thumbnail = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.jobs.remove(&key);
                if let Some(thumbnail) = thumbnail {
                    if this.draft.get(&key).is_some() {
                        this.set_thumbnail(key, thumbnail);
                    }
                }
                cx.notify();
            });
        });
        self.jobs.insert(
            id,
            Job {
                closes: false,
                _work: Work {
                    cancel: Arc::new(AtomicBool::new(false)),
                    _task: task,
                },
            },
        );
    }
}
