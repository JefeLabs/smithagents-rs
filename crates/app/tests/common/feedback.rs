//! Shared setup for the feedback tests: a temporary home holding the draft,
//! a fake capturer, a file picker that answers what the test put in
//! [`FeedbackSetup::picked`], and frame hops the test runs by hand
//! ([`ManualHops`]). Nothing here reaches the screen, a dialog or the real
//! home.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use futures::FutureExt as _;
use gpui_kit::base::Root;
use gpui_kit::{App, AppContext as _, Entity, TestAppContext, Window, WindowHandle};
use smithagents_app::feedback::draft::DraftStore;
use smithagents_app::feedback::render::encode_png;
use smithagents_app::feedback::state::{
    Feedback, FeedbackEvent, FeedbackOptions, FilePicker, FrameHop,
};
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_capture::{test_pattern, FakeCapturer};

use super::ui::open_shell_options;

type Hop = Box<dyn FnOnce(&mut Window, &mut App)>;

/// Frame hops queued instead of waiting for a frame: the test runs each one
/// with [`ManualHops::run_next`].
#[derive(Default)]
pub struct ManualHops {
    queue: RefCell<VecDeque<Hop>>,
}

impl ManualHops {
    /// How many hops wait to run.
    pub fn pending(&self) -> usize {
        self.queue.borrow().len()
    }

    /// Runs the oldest queued hop.
    pub fn run_next(&self, window: &mut Window, cx: &mut App) {
        let hop = self
            .queue
            .borrow_mut()
            .pop_front()
            .expect("a frame hop is queued");
        hop(window, cx);
    }
}

pub struct FeedbackSetup {
    pub home: tempfile::TempDir,
    pub capturer: Arc<FakeCapturer>,
    /// What the picker answers next (cancelled at first).
    pub picked: Rc<RefCell<Result<Option<Vec<PathBuf>>, String>>>,
    pub hops: Rc<ManualHops>,
}

impl FeedbackSetup {
    /// An empty home; the capturer answers a 2400x1512 test pattern.
    pub fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            capturer: Arc::new(FakeCapturer::new(Ok(test_pattern(2400, 1512)))),
            picked: Rc::new(RefCell::new(Ok(None))),
            hops: Rc::new(ManualHops::default()),
        }
    }

    /// Shell options whose feedback uses this setup's home, fake capturer,
    /// picker and hops; everything else is the default (offline, in memory).
    pub fn options(&self) -> ShellOptions {
        let picked = self.picked.clone();
        let picker: FilePicker = Rc::new(move |_: &mut App| {
            let answer = picked.borrow().clone();
            async move { answer }.boxed_local()
        });
        let hops = self.hops.clone();
        let frame_hop: FrameHop = Rc::new(move |_: &mut Window, hop: Hop| {
            hops.queue.borrow_mut().push_back(hop);
        });
        ShellOptions {
            feedback: Some(FeedbackOptions {
                store: Some(self.store()),
                capturer: Some(self.capturer.clone()),
                picker: Some(picker),
                frame_hop: Some(frame_hop),
            }),
            ..Default::default()
        }
    }

    /// The store on this setup's home.
    pub fn store(&self) -> DraftStore {
        DraftStore::new(self.home.path())
    }

    /// The draft directory.
    pub fn draft_dir(&self) -> PathBuf {
        self.store().dir()
    }

    /// Opens the shell on this setup in a 1200x800 window.
    pub fn open(
        &self,
        cx: &mut TestAppContext,
    ) -> (WindowHandle<Root>, Entity<Shell>, Entity<Feedback>) {
        let (window, shell) = open_shell_options(cx, self.options(), (1200., 800.));
        cx.run_until_parked();
        let feedback = cx.read(|cx| shell.read(cx).feedback().clone());
        (window, shell, feedback)
    }

    /// Writes `bytes` as `name` in a scratch directory next to the home's
    /// draft (`<home>/picked/<name>`, subdirectories allowed) and answers its
    /// path.
    pub fn picked_file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.home.path().join("picked").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    /// The picker's next answer: these files.
    pub fn pick(&self, paths: Vec<PathBuf>) {
        *self.picked.borrow_mut() = Ok(Some(paths));
    }
}

/// Runs `f` on the feedback entity inside the window, then parks.
pub fn act<R>(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    feedback: &Entity<Feedback>,
    f: impl FnOnce(&mut Feedback, &mut Window, &mut gpui_kit::Context<Feedback>) -> R,
) -> R {
    let result = cx
        .update_window(window.into(), |_, window, cx| {
            feedback.update(cx, |feedback, cx| f(feedback, window, cx))
        })
        .unwrap();
    cx.run_until_parked();
    result
}

/// Runs the oldest queued frame hop inside the window, then parks.
pub fn run_hop(cx: &mut TestAppContext, window: WindowHandle<Root>, setup: &FeedbackSetup) {
    cx.update_window(window.into(), |_, window, cx| {
        setup.hops.run_next(window, cx)
    })
    .unwrap();
    cx.run_until_parked();
}

/// `Feedback::capture`, then both frame hops, then parks: a whole capture
/// without the hold.
pub fn capture_now(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    feedback: &Entity<Feedback>,
    setup: &FeedbackSetup,
) {
    act(cx, window, feedback, |f, window, cx| f.capture(window, cx));
    run_hop(cx, window, setup);
    run_hop(cx, window, setup);
}

/// Every event `feedback` emits from now on, collected as they come.
pub fn record_events(
    cx: &mut TestAppContext,
    feedback: &Entity<Feedback>,
) -> Rc<RefCell<Vec<FeedbackEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update(|cx| {
        cx.subscribe(feedback, move |_, event: &FeedbackEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    seen
}

/// A real PNG of `width` x `height` (a test pattern).
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let shot = test_pattern(width, height);
    let image = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba).unwrap();
    encode_png(&image).unwrap()
}

/// The names of the files directly in `dir` (none when it does not exist).
pub fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
