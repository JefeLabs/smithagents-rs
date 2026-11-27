//! The idea package's boxes on the Submit screen, under "An intent to work
//! on now" and only while it is chosen: "Prepare an idea PR for teammates"
//! and, while that is ticked, its parts, indented ("Demo video",
//! "Diagrams", "Explainer doc"). Ticking it ticks "Explainer doc". "Demo
//! video" needs `agent-browser` ready (until the targets load it is
//! disabled, with no sentence). With no part ticked the summary asks for
//! one and Create is disabled.
//!
//! The boxes are part of the form, not of the draft: they start unticked
//! whenever the screen opens. Once a submission is sent, they show what was
//! sent and stay as they were while the draft is locked, like the rest of
//! the form; Start over keeps them ticked as sent.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::label::Label;
use gpui_kit::component::{v_flex, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, Context, IntoElement, ParentElement as _, Styled as _};
use smithagents_core::feedback::{IdeaOptions, SendAs};

use super::super::{FeedbackPane, Paint};
use crate::feedback::submit_vm::{demo_help, IDEA_START};
use crate::feedback::text::{
    IDEA_DEMO_LABEL, IDEA_DIAGRAMS_LABEL, IDEA_EXPLAINER_LABEL, IDEA_LABEL,
};

/// Element id of "Prepare an idea PR for teammates".
pub const IDEA_CHECK_ID: &str = "feedback-idea";
pub const IDEA_EXPLAINER_ID: &str = "feedback-idea-explainer";
pub const IDEA_DIAGRAMS_ID: &str = "feedback-idea-diagrams";
pub const IDEA_DEMO_ID: &str = "feedback-idea-demo";
/// Element id of what "Demo video" needs, or why it cannot be chosen.
pub const IDEA_DEMO_HELP_ID: &str = "feedback-idea-demo-help";

/// One part of the idea package.
#[derive(Debug, Clone, Copy)]
enum Part {
    Demo,
    Diagrams,
    Explainer,
}

impl FeedbackPane {
    /// Whether the daemon said `agent-browser` is ready; `None` while the
    /// targets load.
    fn agent_browser_ready(&self) -> Option<bool> {
        Some(self.submit.targets.as_ref()?.agent_browser.ready)
    }

    /// The idea package Create sends: the one sent, while something may
    /// exist under the submission's id; else, for an intent, the boxes as
    /// ticked (a demo video only while `agent-browser` is ready). `None`
    /// for an issue, and while "Prepare an idea PR for teammates" is not
    /// ticked.
    pub(super) fn chosen_idea(&self, send_as: SendAs, cx: &App) -> Option<IdeaOptions> {
        if let Some(sent) = self.sent_to(cx) {
            return sent.idea;
        }
        if send_as != SendAs::Intent {
            return None;
        }
        let demo = self.agent_browser_ready() == Some(true);
        self.submit.package.map(|parts| IdeaOptions {
            demo_video: parts.demo_video && demo,
            ..parts
        })
    }

    /// Whether the boxes can change now: no command under way and nothing
    /// sent under the submission's id.
    fn package_editable(&self, cx: &App) -> bool {
        self.submit.busy.is_none() && !self.feedback.read(cx).locked()
    }

    fn tick_idea(&mut self, ticked: bool, cx: &mut Context<Self>) {
        if self.package_editable(cx) {
            self.submit.package = ticked.then_some(IDEA_START);
            cx.notify();
        }
    }

    fn tick_part(&mut self, part: Part, ticked: bool, cx: &mut Context<Self>) {
        if !self.package_editable(cx) {
            return;
        }
        let Some(parts) = self.submit.package.as_mut() else {
            return;
        };
        match part {
            Part::Demo => parts.demo_video = ticked,
            Part::Diagrams => parts.diagrams = ticked,
            Part::Explainer => parts.explainer = ticked,
        }
        cx.notify();
    }

    /// The boxes, as `idea` has them (see [`Self::chosen_idea`]), indented
    /// under the intent's radio. `frozen`: a command is under way or the
    /// draft is locked.
    pub(super) fn idea_boxes(
        &self,
        idea: Option<IdeaOptions>,
        frozen: bool,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let ready = self.agent_browser_ready();
        let part =
            |id: &'static str, label: &'static str, part: Part, ticked: bool| {
                Checkbox::new(id)
                    .label(label)
                    .small()
                    .checked(ticked)
                    .on_click(cx.listener(move |this, ticked: &bool, _, cx| {
                        this.tick_part(part, *ticked, cx)
                    }))
            };
        v_flex()
            .w_full()
            .pl_6()
            .gap_1p5()
            .child(
                Checkbox::new(IDEA_CHECK_ID)
                    .label(IDEA_LABEL)
                    .small()
                    .checked(idea.is_some())
                    .disabled(frozen)
                    .on_click(
                        cx.listener(|this, ticked: &bool, _, cx| this.tick_idea(*ticked, cx)),
                    ),
            )
            .when_some(idea, |this, idea| {
                this.child(
                    v_flex()
                        .w_full()
                        .pl_6()
                        .gap_1p5()
                        .child(
                            part(IDEA_DEMO_ID, IDEA_DEMO_LABEL, Part::Demo, idea.demo_video)
                                .disabled(frozen || ready != Some(true)),
                        )
                        .when_some(ready, |this, ready| {
                            this.child(
                                Label::new(demo_help(ready))
                                    .id(IDEA_DEMO_HELP_ID)
                                    .text_xs()
                                    .text_color(paint.muted_foreground),
                            )
                        })
                        .child(
                            part(
                                IDEA_DIAGRAMS_ID,
                                IDEA_DIAGRAMS_LABEL,
                                Part::Diagrams,
                                idea.diagrams,
                            )
                            .disabled(frozen),
                        )
                        .child(
                            part(
                                IDEA_EXPLAINER_ID,
                                IDEA_EXPLAINER_LABEL,
                                Part::Explainer,
                                idea.explainer,
                            )
                            .disabled(frozen),
                        ),
                )
            })
    }
}
