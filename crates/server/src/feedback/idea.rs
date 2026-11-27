//! The idea package: a submission with `idea` set becomes an idea intent,
//! committed on `idea/<slug>`, whose agent prepares an explainer, diagrams
//! and a demo video in the issue folder instead of implementing anything.
//! The daemon fixes the intent's acceptance criteria from the ticked parts;
//! polish and review keep them (see [`crate::polish::apply_pass2`] and
//! [`crate::intents::IntentService::review`]).

use smithagents_core::feedback::{IdeaOptions, IdeaPackage};

pub const CRITERION_EXPLAINER: &str = "`explainer.md` exists in the issue folder and covers what, why, how it could work and open questions.";
pub const CRITERION_DIAGRAMS: &str = "`explainer.md` has at least one Mermaid diagram of the idea.";
pub const CRITERION_DEMO: &str = "`demo.mp4` (at most 50 MB) shows the bug or the idea in a browser, and `demo.md` lists the steps taken.";
pub const CRITERION_SCOPE: &str =
    "Only files inside the issue folder are changed, and nothing is implemented.";

/// The idea intent's fixed criteria: the explainer's, the diagrams' and the
/// demo video's, each when ticked, then the scope's (always).
pub fn fixed_criteria(options: IdeaOptions) -> Vec<String> {
    [
        (options.explainer, CRITERION_EXPLAINER),
        (options.diagrams, CRITERION_DIAGRAMS),
        (options.demo_video, CRITERION_DEMO),
        (true, CRITERION_SCOPE),
    ]
    .into_iter()
    .filter(|(ticked, _)| *ticked)
    .map(|(_, criterion)| criterion.to_owned())
    .collect()
}

/// What a new idea intent's origin records: the ticked parts, and nothing
/// done yet.
pub fn package(options: IdeaOptions) -> IdeaPackage {
    IdeaPackage {
        explainer: options.explainer,
        diagrams: options.diagrams,
        demo_video: options.demo_video,
        pr_url: None,
        not_done: None,
        implementation_intent_id: None,
    }
}
