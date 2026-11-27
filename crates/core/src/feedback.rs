//! Feedback wire types: the submit request, its outcome, the targets the app
//! can pick, the stored setting and the origin an intent keeps.

use serde::{Deserialize, Serialize};

use crate::agent::Repo;
use crate::ids::{IntentId, RepoId};
use crate::intent::{Intent, IntentKind};

pub use crate::ids::SubmissionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    NewFeature,
    Enhancement,
    Bug,
}

impl FeedbackKind {
    /// The intent kind of the same name.
    pub fn intent_kind(self) -> IntentKind {
        match self {
            Self::NewFeature => IntentKind::NewFeature,
            Self::Enhancement => IntentKind::Enhancement,
            Self::Bug => IntentKind::Bug,
        }
    }
}

/// What Submit creates: an intent to work on now, or a GitHub issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendAs {
    Intent,
    GithubIssue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Screenshot,
    Image,
    Video,
}

/// Which parts an idea intent's agent prepares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaOptions {
    pub explainer: bool,
    pub diagrams: bool,
    pub demo_video: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackAttachment {
    pub kind: AttachmentKind,
    pub original_name: String,
    #[serde(default)]
    pub boxes: Vec<FeedbackBox>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackRequest {
    pub submission_id: SubmissionId,
    pub main_idea: String,
    pub kind: FeedbackKind,
    pub repo_id: RepoId,
    pub send_as: SendAs,
    #[serde(default)]
    pub idea: Option<IdeaOptions>,
    pub app_version: String,
    pub platform: String,
    #[serde(default)]
    pub attachments: Vec<FeedbackAttachment>,
    /// The user confirmed that the screenshots will be visible to anyone if
    /// the repository is public (asked before every GitHub issue to a
    /// repository that is or may be public). An older app sends nothing:
    /// not acknowledged.
    #[serde(default)]
    pub public_acknowledged: bool,
}

/// The step of a submission that failed after its files were committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackStep {
    CreateIntent,
    Push,
    CreateIssue,
    /// An idea intent's "Open pull request", after its push.
    OpenPullRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackFailure {
    pub step: FeedbackStep,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackOutcome {
    pub submission_id: SubmissionId,
    pub repo_id: RepoId,
    pub branch: String,
    pub commit: String,
    pub folder: String,
    pub pushed: bool,
    pub intent: Option<Intent>,
    pub issue_url: Option<String>,
    pub failed: Option<FeedbackFailure>,
}

/// What an idea intent's agent was asked to prepare, and what became of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdeaPackage {
    pub explainer: bool,
    pub diagrams: bool,
    pub demo_video: bool,
    pub pr_url: Option<String>,
    pub not_done: Option<String>,
    pub implementation_intent_id: Option<IntentId>,
    /// The commit "Open pull request" pushes as the idea branch: the agent's
    /// work, chosen before the first push, so a retry pushes the same one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_commit: Option<String>,
    /// Whether `pr_commit` is on `origin` (a retry does not push again).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pr_pushed: bool,
}

/// Where an intent created by Submit came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackOrigin {
    pub folder: String,
    pub branch: String,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idea: Option<IdeaPackage>,
}

/// The stored feedback setting (settings key `feedback`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackSettings {
    pub meta_repo_id: Option<RepoId>,
}

/// The repository feedback goes to: `stored` while a repository with that id
/// exists, else the only repository, else `None`.
pub fn effective_meta_repo(stored: Option<RepoId>, repos: &[Repo]) -> Option<RepoId> {
    if let Some(id) = stored {
        if repos.iter().any(|r| r.id == id) {
            return Some(id);
        }
    }
    match repos {
        [only] => Some(only.id),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReadiness {
    pub ready: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackTarget {
    pub repo_id: RepoId,
    pub has_github_remote: bool,
    pub is_public: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackTargets {
    pub repos: Vec<FeedbackTarget>,
    pub meta_repo_id: Option<RepoId>,
    pub gh: ToolReadiness,
    pub agent_browser: ToolReadiness,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Repo;
    use crate::ids::RepoId;
    use crate::intent::IntentKind;
    use chrono::Utc;
    use serde_json::json;

    fn repo() -> Repo {
        Repo {
            id: RepoId::new(),
            path: "/r".into(),
            default_branch: "main".into(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn the_request_has_the_documented_wire_shape() {
        let req = FeedbackRequest {
            submission_id: SubmissionId::new(),
            main_idea: "x".into(),
            kind: FeedbackKind::NewFeature,
            repo_id: RepoId::new(),
            send_as: SendAs::GithubIssue,
            idea: Some(IdeaOptions {
                explainer: true,
                diagrams: false,
                demo_video: true,
            }),
            app_version: "0.1.0".into(),
            platform: "macos".into(),
            attachments: vec![FeedbackAttachment {
                kind: AttachmentKind::Screenshot,
                original_name: "a.png".into(),
                boxes: vec![],
            }],
            public_acknowledged: true,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["send_as"], "github_issue");
        assert_eq!(v["kind"], "new_feature");
        assert_eq!(v["attachments"][0]["kind"], "screenshot");
        assert_eq!(serde_json::from_value::<FeedbackRequest>(v).unwrap(), req);

        let min = json!({"submission_id": SubmissionId::new(), "main_idea": "x", "kind": "bug",
            "repo_id": RepoId::new(), "send_as": "intent", "app_version": "0.1.0", "platform": "macos"});
        let got: FeedbackRequest = serde_json::from_value(min.clone()).unwrap();
        assert_eq!(got.idea, None);
        assert!(got.attachments.is_empty());
        assert_eq!(got.send_as, SendAs::Intent);
        assert!(!got.public_acknowledged, "an older app's request");

        let mut bad = min;
        bad["kind"] = json!("feature");
        assert!(serde_json::from_value::<FeedbackRequest>(bad).is_err());
    }

    #[test]
    fn feedback_kinds_map_to_intent_kinds() {
        assert_eq!(
            FeedbackKind::NewFeature.intent_kind(),
            IntentKind::NewFeature
        );
        assert_eq!(
            FeedbackKind::Enhancement.intent_kind(),
            IntentKind::Enhancement
        );
        assert_eq!(FeedbackKind::Bug.intent_kind(), IntentKind::Bug);
    }

    #[test]
    fn the_meta_repository_is_implied_only_for_a_single_repository() {
        let (a, b) = (repo(), repo());
        assert_eq!(
            effective_meta_repo(Some(a.id), &[a.clone(), b.clone()]),
            Some(a.id)
        );
        assert_eq!(
            effective_meta_repo(Some(RepoId::new()), &[a.clone(), b.clone()]),
            None
        );
        assert_eq!(
            effective_meta_repo(None, std::slice::from_ref(&a)),
            Some(a.id)
        );
        assert_eq!(
            effective_meta_repo(Some(RepoId::new()), std::slice::from_ref(&a)),
            Some(a.id)
        );
        assert_eq!(effective_meta_repo(None, &[a, b]), None);
        assert_eq!(effective_meta_repo(None, &[]), None);
    }

    #[test]
    fn the_submission_id_round_trips_as_a_string() {
        let id = SubmissionId::new();
        assert_eq!(id.to_string().parse::<SubmissionId>().unwrap(), id);
    }
}
