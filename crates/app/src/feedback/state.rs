//! The feedback draft as the app holds it. One [`Feedback`] entity owns the
//! draft and its notices, runs the capture sequence against a [`Capturer`],
//! adds picked files, saves boxes as they change and draws the annotated
//! image on Done. Everything that reaches the world (the draft directory,
//! the capturer, the file picker, the next frame) comes in through
//! [`FeedbackOptions`], so tests replace all of it.
//!
//! Image work and file copies run off the UI thread. Work for an attachment
//! that is removed or discarded meanwhile is cancelled: its result is
//! dropped, work already running on another thread writes nothing more once
//! it sees the cancel, and deletes what it wrote when the cancel came while
//! it wrote. Files the draft does not name are swept away after a removal.
//!
//! Submitting: the draft gets one submission id ([`Pending`], kept in
//! `submission.json`), used for every send and retry of it until it
//! succeeds, is started over or discarded. Before a request leaves, the
//! target it is sent for is written there ([`Feedback::mark_sent`]); from
//! then on, and until an answer shows that nothing exists under the id,
//! the draft is [`Feedback::locked`]: it stays as it was sent (the daemon
//! may already have committed it), so nothing is added, removed, annotated
//! or retyped, and a capture or a file that lands meanwhile is refused.
//! Only a success deletes the draft and its files.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::{Either, LocalBoxFuture};
use futures::FutureExt as _;
use gpui_kit::{
    App, AppContext as _, Context, EventEmitter, PathPromptOptions, RenderImage, Task, Window,
};
use serde::{Deserialize, Serialize};
use smithagents_capture::{CaptureError, Capturer, Frame, WindowTarget};
use smithagents_client::FeedbackFile;
use smithagents_core::feedback::{
    FeedbackAttachment, FeedbackBox, FeedbackKind, FeedbackOutcome, IdeaOptions, SendAs,
    SubmissionId,
};
use smithagents_core::ids::RepoId;

use super::annotation::Annotation;
use super::draft::{
    annotated_name, Attachment, AttachmentId, AttachmentKind, Draft, DraftStore, Loaded,
};
use super::files::{FileKind, Refusal};
use super::render::decode;
use super::submit_vm::{wire_kind, KINDS};
use super::text::{
    capture_failed_text, more_files_text, more_notices_text, picker_failed_text,
    CAPTURE_FRAMES_MISSED, CAPTURE_FRAMES_TIMEOUT, CAPTURE_PICTURE_TIMEOUT, CAPTURE_TOO_SLOW,
    DRAFT_FULL_TEXT, MAX_ATTACHMENTS, MAX_BOXES, MAX_COMMENT_CHARS, MAX_IMAGE_BYTES, MAX_NOTICES,
    MAX_TOTAL_BYTES, SUBMISSION_LOCKED_TEXT,
};
use super::work::{
    capture_error_text, copy_in, io_text, read_thumbnail, redraw, render_image, take_picture,
    Copied, Files, JobError, Picture,
};

/// How long a submission (an upload of up to 100 MB, a commit, a push and
/// `gh`) or a retry may go unanswered before the Submit screen gives up on
/// it.
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(600);

/// The draft's submission, until it succeeds, is started over or discarded:
/// its id, sent with every send and retry of the draft (the daemon makes one
/// commit, intent or issue per id), and what became of it. Kept in
/// `<draft dir>/submission.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub submission_id: SubmissionId,
    /// The daemon's answer when a step after the commit failed.
    #[serde(default)]
    pub outcome: Option<FeedbackOutcome>,
    /// Where the submission went: written before its request leaves, and
    /// kept while something may exist under the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent: Option<SentTo>,
    /// The last request went out and no answer came (a timeout, a lost
    /// connection, a quit while sending): the daemon may still be making it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub awaiting: bool,
}

/// The repository and the kind of result a submission was sent for, and the
/// idea package's parts when it was sent as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentTo {
    pub repo_id: RepoId,
    pub send_as: SendAs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idea: Option<IdeaOptions>,
}

/// A `submission.json` as this build reads it. One it cannot read in full
/// (another version of the app wrote it, say) still gives its id, and where
/// it was sent when that part reads: the id is what keeps a re-send from
/// being made a second time, and the daemon knows what became of it. Sent
/// somewhere with no readable answer, it awaits one.
fn read_pending(bytes: &[u8]) -> Option<Pending> {
    match serde_json::from_slice::<Pending>(bytes) {
        Ok(pending) => return Some(pending),
        Err(e) => {
            tracing::warn!(error = %e, "the pending feedback submission could not be read in full");
        }
    }
    let json: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let submission_id = serde_json::from_value(json.get("submission_id")?.clone()).ok()?;
    let sent: Option<SentTo> = json
        .get("sent")
        .and_then(|sent| serde_json::from_value(sent.clone()).ok());
    Some(Pending {
        submission_id,
        outcome: None,
        sent,
        awaiting: sent.is_some(),
    })
}

/// A box as the daemon takes it.
fn wire_box(annotation: &Annotation) -> FeedbackBox {
    let rect = annotation.rect;
    FeedbackBox {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
        comment: annotation.comment.clone(),
    }
}

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
    /// How long the picture itself may take; `None` is
    /// [`CAPTURE_PICTURE_TIMEOUT`]. (The test clock runs every ready job
    /// before it moves, so a test sets a short limit instead of a slow
    /// capturer.)
    pub picture_timeout: Option<Duration>,
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
        // Ordered with the file system: the UI thread deletes after this, a
        // job checks it after its writes.
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Image work for one attachment: drawing its annotated image, or reading
/// its thumbnail.
struct Job {
    /// A drawing (it writes files), not a thumbnail read.
    draws: bool,
    /// Done is waiting for this drawing to close the annotator.
    closes: bool,
    _work: Work,
}

/// A box as `draft.json` loads it back: inside the picture (an invalid one
/// is dropped) with a comment of at most [`MAX_COMMENT_CHARS`].
fn sanitized(annotation: Annotation) -> Option<Annotation> {
    let rect = annotation.rect.sanitized()?;
    let mut comment = annotation.comment;
    if let Some((cut, _)) = comment.char_indices().nth(MAX_COMMENT_CHARS) {
        comment.truncate(cut);
    }
    Some(Annotation { rect, comment })
}

/// The feedback draft, its notices and the work running on it.
pub struct Feedback {
    files: Files,
    capturer: Option<Arc<dyn Capturer>>,
    picker: Option<FilePicker>,
    frame_hop: FrameHop,
    picture_timeout: Duration,
    draft: Draft,
    /// At most [`MAX_NOTICES`], then one line counting `hidden_notices`.
    notices: Vec<String>,
    hidden_notices: usize,
    /// From `capture` until the picture is in the draft or refused.
    capturing: bool,
    /// Which capture the frame hops and the fallback timer belong to: a
    /// late hop of a capture that gave up takes no picture.
    capture_generation: u64,
    /// The fallback timer while waiting for the frames, then the picture.
    capture: Option<Task<()>>,
    annotating: Option<AttachmentId>,
    /// The picked files being checked and copied.
    adding: Option<Work>,
    /// A removal during a pick left the sweep for the pick's end: the pick's
    /// own files are not in the draft until each copy is done.
    sweep_after_pick: bool,
    jobs: HashMap<AttachmentId, Job>,
    thumbnails: HashMap<AttachmentId, Arc<RenderImage>>,
    /// Thumbnails no longer shown, for the pane to release.
    retired: Vec<Arc<RenderImage>>,
    /// The draft's submission, from `submission.json`.
    pending: Option<Pending>,
    /// A submission or a retry of the draft is under way.
    submitting: bool,
    /// The last success sentence, until the draft changes.
    result: Option<String>,
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
            picture_timeout,
        } = options;
        let files = match store {
            Some(store) => Files::Disk(store),
            None => Files::Memory(Arc::default()),
        };
        let Loaded { draft, notices } = files.load();
        let pending = files
            .read_submission()
            .and_then(|bytes| read_pending(&bytes));
        let mut this = Self {
            files,
            capturer,
            picker,
            frame_hop: frame_hop.unwrap_or_else(|| {
                Rc::new(|window: &mut Window, callback| window.on_next_frame(callback))
            }),
            picture_timeout: picture_timeout.unwrap_or(CAPTURE_PICTURE_TIMEOUT),
            draft,
            notices: Vec::new(),
            hidden_notices: 0,
            capturing: false,
            capture_generation: 0,
            capture: None,
            annotating: None,
            adding: None,
            sweep_after_pick: false,
            jobs: HashMap::new(),
            thumbnails: HashMap::new(),
            retired: Vec::new(),
            pending,
            submitting: false,
            result: None,
        };
        this.set_notices(notices);
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

    /// What the person should read, newest last: at most [`MAX_NOTICES`],
    /// then one line counting the rest. The next capture or pick that
    /// reports replaces them; a failed save adds to them.
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
    /// annotator; a failure says why and reveals the pane. When the two
    /// frames do not come within [`CAPTURE_FRAMES_TIMEOUT`] (a window that
    /// draws nothing), or the picture within [`CAPTURE_PICTURE_TIMEOUT`]
    /// ([`FeedbackOptions::picture_timeout`]), the capture gives up the same
    /// way, so Capture never stays disabled.
    pub fn capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.capturing || self.annotating.is_some() {
            return;
        }
        if self.refused_while_locked(cx) {
            return;
        }
        if self.draft.attachments.len() >= MAX_ATTACHMENTS {
            self.set_notices(vec![DRAFT_FULL_TEXT.to_string()]);
            cx.emit(FeedbackEvent::RevealPane);
            cx.notify();
            return;
        }
        tracing::debug!("feedback capture: waiting two frames");
        self.capturing = true;
        self.capture_generation += 1;
        let generation = self.capture_generation;
        let timer = cx.background_executor().timer(CAPTURE_FRAMES_TIMEOUT);
        self.capture = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| this.frames_missed(generation, cx));
        }));
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
                        let _ =
                            this.update(cx, |this, cx| this.start_capture(generation, window, cx));
                    }),
                );
            }),
        );
    }

    /// The fallback timer: the frames never came.
    fn frames_missed(&mut self, generation: u64, cx: &mut Context<Self>) {
        if !self.capturing || generation != self.capture_generation {
            return;
        }
        tracing::warn!(
            after = ?CAPTURE_FRAMES_TIMEOUT,
            "feedback capture: the window drew no frames; giving up"
        );
        self.capturing = false;
        self.capture = None;
        self.set_notices(vec![capture_failed_text(CAPTURE_FRAMES_MISSED)]);
        cx.emit(FeedbackEvent::RevealPane);
        cx.notify();
    }

    fn start_capture(&mut self, generation: u64, window: &Window, cx: &mut Context<Self>) {
        if !self.capturing || generation != self.capture_generation {
            tracing::debug!("feedback capture: a late frame of a capture that gave up");
            return;
        }
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
        let started = cx.background_executor().now();
        let timeout = cx.background_executor().timer(self.picture_timeout);
        // Replaces (and so stops) the frames' fallback timer. Whichever comes
        // first, the picture or the timeout; a picture that hangs is left to
        // its thread, and its result is never used.
        self.capture = Some(cx.spawn(async move |this, cx| {
            let picture = match futures::future::select(picture, timeout).await {
                Either::Left((picture, _)) => Some(picture),
                Either::Right(_) => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_capture(generation, started, picture, cx)
            });
        }));
    }

    /// The picture, or `None` when the picture's time limit passed first. A
    /// picture that comes at or after the limit is dropped too: nothing of
    /// it is written (the file is written only here, on the UI thread).
    fn finish_capture(
        &mut self,
        generation: u64,
        started: Instant,
        picture: Option<Result<Picture, CaptureError>>,
        cx: &mut Context<Self>,
    ) {
        if !self.capturing || generation != self.capture_generation {
            return;
        }
        self.capturing = false;
        self.capture = None;
        let waited = cx
            .background_executor()
            .now()
            .saturating_duration_since(started);
        let picture = match picture {
            Some(picture) if waited < self.picture_timeout => picture,
            _ => {
                tracing::warn!(
                    after = ?self.picture_timeout,
                    "feedback capture: the picture took too long; giving up"
                );
                Err(CaptureError::Failed(CAPTURE_TOO_SLOW.to_string()))
            }
        };
        match self.add_screenshot(picture) {
            Ok(id) => {
                self.annotating = Some(id.clone());
                cx.emit(FeedbackEvent::OpenAnnotator(id));
            }
            Err(sentence) => {
                self.set_notices(vec![sentence]);
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
        // The draft was sent while the picture was taken: what a success
        // deletes must be only what was sent.
        if self.locked() {
            return Err(SUBMISSION_LOCKED_TEXT.to_string());
        }
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
        self.set_notices(Vec::new());
        self.save();
        Ok(id)
    }

    /// Asks the picker for files and adds them one by one, in order. Each
    /// refused file gets its own sentence; the files before and after it
    /// are still added. Once the draft is full the pick stops, with one
    /// sentence counting the files not looked at. Ignored while an earlier
    /// pick is being added.
    pub fn add_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding.is_some() || self.refused_while_locked(cx) {
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
                            this.set_notices(vec![picker_failed_text(&e)]);
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
                this.set_notices(Vec::new());
                cx.notify();
            });
            let mut paths = paths.into_iter();
            while let Some(path) = paths.next() {
                // What the draft holds now, files added earlier in this pick
                // included; nothing once it is full.
                let Ok(next) = this.update(cx, |this, cx| {
                    if this.locked() {
                        this.add_notice(SUBMISSION_LOCKED_TEXT.to_string());
                        cx.notify();
                        return Err(());
                    }
                    Ok((this.draft.attachments.len() < MAX_ATTACHMENTS)
                        .then(|| (this.files.clone(), this.draft.usage(), this.draft.clone())))
                }) else {
                    return;
                };
                // Sent meanwhile: the rest of the pick is not added.
                let Ok(next) = next else {
                    break;
                };
                let Some((files, usage, draft)) = next else {
                    let left = 1 + paths.len();
                    let _ = this.update(cx, |this, _| this.push_notice(more_files_text(left)));
                    break;
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
                if std::mem::take(&mut this.sweep_after_pick) {
                    this.files.sweep(&this.draft);
                }
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
            Copied::Refused(sentence) => self.push_notice(sentence),
            Copied::Failed(sentence) => self.add_notice(sentence),
            Copied::Cancelled => {}
            Copied::Added {
                attachment,
                thumbnail,
                thumbnail_error,
            } => {
                if self.locked() {
                    // Sent while the file was copied: its copy goes.
                    self.files.remove(&attachment.stored_name);
                    self.add_notice(SUBMISSION_LOCKED_TEXT.to_string());
                    cx.notify();
                    return;
                }
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
                    self.push_notice(refusal.sentence(&attachment.original_name));
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

    /// Removes an attachment; its thumbnail is retired and its work
    /// cancelled. The draft is saved first, then its files are deleted and
    /// the files the draft does not name are swept away; when the save
    /// fails the files stay, since the draft on disk still names them.
    pub fn remove(&mut self, id: &AttachmentId, cx: &mut Context<Self>) {
        if self.locked() {
            return;
        }
        let Some(index) = self.draft.attachments.iter().position(|a| &a.id == id) else {
            return;
        };
        self.jobs.remove(id);
        let removed = self.draft.attachments.remove(index);
        self.retire(id);
        if self.save() {
            self.files.remove(&removed.stored_name);
            if self.adding.is_some() {
                // A copy may have written a file the draft does not name yet.
                self.sweep_after_pick = true;
            } else {
                self.files.sweep(&self.draft);
            }
        }
        if self.annotating.as_ref() == Some(id) {
            self.close_annotator(cx);
        }
        cx.notify();
    }

    /// Empties the draft and deletes its directory, the pending submission
    /// included. Files still being added are dropped; every thumbnail is
    /// retired. Not while a submission is under way.
    pub fn discard_all(&mut self, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        self.result = None;
        self.pending = None;
        self.adding = None;
        self.sweep_after_pick = false;
        self.jobs.clear();
        self.retired
            .extend(self.thumbnails.drain().map(|(_, thumbnail)| thumbnail));
        self.draft = Draft::default();
        if let Err(e) = self.files.clear() {
            tracing::warn!(error = %e, "could not delete the feedback draft");
            self.add_notice(io_text(&e));
            // Whatever is left, the next start finds an empty draft with no
            // submission.
            self.save();
            if let Err(e) = self.files.remove_submission() {
                tracing::warn!(error = %e, "could not delete the pending feedback submission");
            }
        }
        self.close_annotator(cx);
        cx.notify();
    }

    /// Opens the annotator on a screenshot (not while capturing or already
    /// annotating).
    pub fn open_annotator(&mut self, id: &AttachmentId, cx: &mut Context<Self>) {
        if self.capturing || self.annotating.is_some() || self.locked() {
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

    /// Replaces a screenshot's boxes and saves; its annotated image no
    /// longer matches until Done draws it. Each box is kept as `draft.json`
    /// loads it back: one outside the picture is clamped or dropped, a
    /// comment is cut at [`MAX_COMMENT_CHARS`], and at most [`MAX_BOXES`].
    pub fn set_boxes(&mut self, id: &AttachmentId, boxes: Vec<Annotation>, cx: &mut Context<Self>) {
        if self.locked() {
            return;
        }
        let Some(shot) = self
            .draft
            .attachments
            .iter_mut()
            .find(|a| &a.id == id && a.kind == AttachmentKind::Screenshot)
        else {
            return;
        };
        shot.boxes = boxes
            .into_iter()
            .filter_map(sanitized)
            .take(MAX_BOXES)
            .collect();
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
        if let Some(job) = self.jobs.get_mut(&id).filter(|job| job.draws) {
            // Done was already pressed, or a drawing from the start runs:
            // it closes the annotator when it is done (and draws again when
            // the boxes changed meanwhile). Replacing it would cancel it
            // while it may be writing.
            job.closes = true;
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
                .map_err(|e| e.to_string())
                .and_then(|bytes| decode(&bytes).map_err(|e| e.to_string()));
            match decoded {
                Ok(image) => Some(render_image(image)),
                Err(e) => {
                    tracing::warn!(error = %e, file = %name, "could not load a feedback image");
                    None
                }
            }
        })
    }

    // ---- Submitting ------------------------------------------------------

    /// Keeps the main idea (the first line is the title). Not while locked.
    pub fn set_main_idea(&mut self, text: String, cx: &mut Context<Self>) {
        if self.locked() || self.draft.main_idea == text {
            return;
        }
        self.draft.main_idea = text;
        self.save();
        cx.notify();
    }

    /// Keeps the kind, by its wire name. Not while locked.
    pub fn set_kind(&mut self, kind: FeedbackKind, cx: &mut Context<Self>) {
        let name = kind.intent_kind().wire_name();
        if self.locked() || self.draft.kind.as_deref() == Some(name) {
            return;
        }
        self.draft.kind = Some(name.to_owned());
        self.save();
        cx.notify();
    }

    /// The draft's kind; a new feature when none (or an unknown one) is kept.
    pub fn kind(&self) -> FeedbackKind {
        let name = self.draft.kind.as_deref();
        KINDS
            .into_iter()
            .find(|kind| Some(kind.intent_kind().wire_name()) == name)
            .unwrap_or(FeedbackKind::NewFeature)
    }

    pub fn pending(&self) -> Option<&Pending> {
        self.pending.as_ref()
    }

    /// A submission is under way ([`Feedback::set_submitting`]).
    pub fn submitting(&self) -> bool {
        self.submitting
    }

    /// The draft stays as it was sent: a submission is under way, or
    /// something may exist under the pending one's id. Nothing can be
    /// added, removed, annotated or retyped then.
    pub fn locked(&self) -> bool {
        self.submitting || self.pending.as_ref().is_some_and(|p| p.sent.is_some())
    }

    /// Marks a submission (or a retry) as under way, or over.
    pub fn set_submitting(&mut self, submitting: bool, cx: &mut Context<Self>) {
        if self.submitting != submitting {
            self.submitting = submitting;
            cx.notify();
        }
    }

    /// The pending submission's id, or a new one, written to
    /// `submission.json` (a failed write is said; the id still holds for
    /// this run).
    pub fn begin_submission(&mut self, cx: &mut Context<Self>) -> SubmissionId {
        if let Some(pending) = &self.pending {
            return pending.submission_id;
        }
        let id = SubmissionId::new();
        tracing::debug!(submission_id = %id, "a new feedback submission");
        self.pending = Some(Pending {
            submission_id: id,
            outcome: None,
            sent: None,
            awaiting: false,
        });
        self.save_pending();
        cx.notify();
        id
    }

    /// The pending submission's request is about to leave for `to`: written
    /// first, so a quit or a lost connection meanwhile leaves the draft
    /// locked. It is awaiting an answer until one comes.
    ///
    /// Whether it is written. When it is not (a failure is said), the
    /// request must not leave, and the draft is as it was before.
    #[must_use = "a submission that is not written down must not be sent"]
    pub fn mark_sent(&mut self, to: SentTo, cx: &mut Context<Self>) -> bool {
        let before = self.pending.as_ref().map(|p| (p.sent, p.awaiting));
        if self.set_sent(Some(to), true, cx) {
            return true;
        }
        if let (Some(pending), Some((sent, awaiting))) = (self.pending.as_mut(), before) {
            pending.sent = sent;
            pending.awaiting = awaiting;
        }
        cx.notify();
        false
    }

    /// The daemon answered the pending submission (and something may exist
    /// under its id): still locked, no longer awaiting.
    pub fn mark_answered(&mut self, cx: &mut Context<Self>) {
        let sent = self.pending.as_ref().and_then(|p| p.sent);
        self.set_sent(sent, false, cx);
    }

    /// Where the pending submission was sent, and whether it awaits an
    /// answer, as they were or are again: `None` when nothing exists under
    /// its id (the draft can change again, under the same id).
    ///
    /// Whether `submission.json` says so now: `false` when there is no
    /// pending submission or the file could not be written (which is said).
    pub fn set_sent(
        &mut self,
        sent: Option<SentTo>,
        awaiting: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pending) = self.pending.as_mut() else {
            return false;
        };
        let awaiting = awaiting && sent.is_some();
        if pending.sent == sent && pending.awaiting == awaiting {
            return true;
        }
        pending.sent = sent;
        pending.awaiting = awaiting;
        let written = self.save_pending();
        cx.notify();
        written
    }

    /// Keeps the daemon's answer of a submission whose step failed: sent to
    /// the repository the daemon names, answered.
    pub fn record_outcome(&mut self, outcome: FeedbackOutcome, cx: &mut Context<Self>) {
        let Some(pending) = self
            .pending
            .as_mut()
            .filter(|pending| pending.submission_id == outcome.submission_id)
        else {
            tracing::warn!(submission_id = %outcome.submission_id, "an outcome of another submission");
            return;
        };
        if let Some(sent) = pending.sent.as_mut() {
            sent.repo_id = outcome.repo_id;
        }
        pending.awaiting = false;
        pending.outcome = Some(outcome);
        self.save_pending();
        cx.notify();
    }

    /// Drops the pending submission (what it committed stays where it is):
    /// the next send is a new one, and the draft can change again.
    pub fn forget_submission(&mut self, cx: &mut Context<Self>) {
        if self.pending.take().is_none() {
            return;
        }
        if let Err(e) = self.files.remove_submission() {
            tracing::warn!(error = %e, "could not delete the pending feedback submission");
            self.add_notice(io_text(&e));
        }
        cx.notify();
    }

    /// What the daemon is sent: each attachment as the request lists it,
    /// and its files as parts (`file-<i>`, and `annotated-<i>` for a
    /// screenshot, `i` its 1-based position). `None` while a screenshot's
    /// annotated picture does not match its boxes, and for a draft kept in
    /// memory only (there are no files to send).
    pub fn upload(&self) -> Option<(Vec<FeedbackAttachment>, Vec<FeedbackFile>)> {
        let store = self.store()?;
        let mut attachments = Vec::with_capacity(self.draft.attachments.len());
        let mut files = Vec::new();
        for (at, attachment) in self.draft.attachments.iter().enumerate() {
            let number = at + 1;
            let screenshot = attachment.kind == AttachmentKind::Screenshot;
            if screenshot && !attachment.rendered {
                return None;
            }
            attachments.push(FeedbackAttachment {
                kind: wire_kind(attachment.kind),
                original_name: attachment.original_name.clone(),
                boxes: attachment.boxes.iter().map(wire_box).collect(),
            });
            files.push(FeedbackFile {
                part: format!("file-{number}"),
                path: store.path_of(&attachment.stored_name),
            });
            if screenshot {
                files.push(FeedbackFile {
                    part: format!("annotated-{number}"),
                    path: store.path_of(&annotated_name(&attachment.stored_name)),
                });
            }
        }
        Some((attachments, files))
    }

    /// The last success sentence; the next change to the draft clears it.
    pub fn result(&self) -> Option<&str> {
        self.result.as_deref()
    }

    /// The daemon confirmed the submission: the draft, its files and the
    /// pending submission go, and `result` is what the pane says.
    pub fn finish_submission(&mut self, result: String, cx: &mut Context<Self>) {
        self.submitting = false;
        self.discard_all(cx);
        self.result = Some(result);
        cx.notify();
    }

    /// Whether a change is refused because the draft is locked; if so, the
    /// pane says why.
    fn refused_while_locked(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.locked() {
            return false;
        }
        self.set_notices(vec![SUBMISSION_LOCKED_TEXT.to_string()]);
        cx.emit(FeedbackEvent::RevealPane);
        cx.notify();
        true
    }

    /// Writes `submission.json`; whether it was written (a failure is said).
    fn save_pending(&mut self) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let written = serde_json::to_vec_pretty(pending)
            .map_err(std::io::Error::other)
            .and_then(|json| self.files.write_submission(&json));
        match written {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "could not save the pending feedback submission");
                self.add_notice(io_text(&e));
                false
            }
        }
    }

    /// Thumbnails no longer shown, each answered once; the pane releases them.
    pub fn take_retired_images(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.retired)
    }

    /// Saves the draft; whether it was saved (a failure is said). The draft
    /// changed, so the last success sentence goes.
    fn save(&mut self) -> bool {
        self.result = None;
        match self.files.save(&self.draft) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "could not save the feedback draft");
                self.add_notice(io_text(&e));
                false
            }
        }
    }

    /// Replaces the notices.
    fn set_notices(&mut self, notices: Vec<String>) {
        self.notices.clear();
        self.hidden_notices = 0;
        for sentence in notices {
            self.push_notice(sentence);
        }
    }

    /// Adds a notice; past [`MAX_NOTICES`] the last line counts the rest.
    fn push_notice(&mut self, sentence: String) {
        if self.hidden_notices == 0 && self.notices.len() < MAX_NOTICES {
            self.notices.push(sentence);
            return;
        }
        self.hidden_notices += 1;
        let line = more_notices_text(self.hidden_notices);
        if self.hidden_notices == 1 {
            self.notices.push(line);
        } else if let Some(last) = self.notices.last_mut() {
            *last = line;
        }
    }

    /// Adds a notice unless it is already there.
    fn add_notice(&mut self, sentence: String) {
        if !self.notices.contains(&sentence) {
            self.push_notice(sentence);
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
                draws: true,
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
            Err(error) => {
                if let Some(sentence) = error.sentence() {
                    tracing::warn!(?error, "could not draw the annotated screenshot");
                    self.add_notice(sentence);
                }
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
                draws: false,
                closes: false,
                _work: Work {
                    cancel: Arc::new(AtomicBool::new(false)),
                    _task: task,
                },
            },
        );
    }
}
