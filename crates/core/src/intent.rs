//! Intent domain types: a structured, reviewable unit of work distilled from
//! a chat or free text, plus the request/input shapes the API uses.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ids::{AgentId, IntentId, ItemId, QuestionId, RepoId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    NewFeature,
    Enhancement,
    Bug,
    Refactor,
    Maintenance,
    Task,
}

impl IntentKind {
    pub const ALL: [IntentKind; 6] = [
        IntentKind::NewFeature,
        IntentKind::Enhancement,
        IntentKind::Bug,
        IntentKind::Refactor,
        IntentKind::Maintenance,
        IntentKind::Task,
    ];

    /// The name this kind has on the wire (its serde name).
    pub fn wire_name(self) -> &'static str {
        match self {
            IntentKind::NewFeature => "new_feature",
            IntentKind::Enhancement => "enhancement",
            IntentKind::Bug => "bug",
            IntentKind::Refactor => "refactor",
            IntentKind::Maintenance => "maintenance",
            IntentKind::Task => "task",
        }
    }

    pub fn from_wire(name: &str) -> Option<IntentKind> {
        IntentKind::ALL.into_iter().find(|k| k.wire_name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentState {
    Draft,
    Complete,
    Queued,
    Sent,
    Resolved,
}

/// Where a draft is in the polish pipeline. `Failed.pass` is the pass to re-run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Phase {
    Polishing { pass: u8 },
    Questions,
    Review,
    Failed { pass: u8, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedSource {
    Chat {
        agent_id: AgentId,
        session_id: Option<String>,
        from_seq: Option<u64>,
        to_seq: Option<u64>,
    },
    Text,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seed {
    pub source: SeedSource,
    pub text: String,
    /// The kind chosen at create, kept by polish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<IntentKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Unanswered,
    Option { option_id: String },
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: QuestionId,
    pub text: String,
    pub options: Vec<QuestionOption>,
    pub answer: Answer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    Accepted,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub text: String,
    pub status: ItemStatus,
    pub edited: bool,
    /// Set only on depends-on items.
    pub intent_ref: Option<IntentId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Content {
    pub title: String,
    pub kind: IntentKind,
    pub current_behavior: String,
    pub questions: Vec<Question>,
    pub preconditions: Vec<Item>,
    pub criteria: Vec<Item>,
    pub stories: Vec<Item>,
    pub depends_on: Vec<Item>,
}

/// `(accepted-or-answered, total)` per section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Counters {
    pub questions: (u32, u32),
    pub preconditions: (u32, u32),
    pub criteria: (u32, u32),
    pub stories: (u32, u32),
    pub depends_on: (u32, u32),
}

fn item_counts(items: &[Item]) -> (u32, u32) {
    let accepted = items
        .iter()
        .filter(|i| i.status == ItemStatus::Accepted)
        .count();
    (accepted as u32, items.len() as u32)
}

fn pending(items: &[Item]) -> u32 {
    items
        .iter()
        .filter(|i| i.status == ItemStatus::Pending)
        .count() as u32
}

impl Content {
    pub fn counters(&self) -> Counters {
        let answered = self
            .questions
            .iter()
            .filter(|q| q.answer != Answer::Unanswered)
            .count();
        Counters {
            questions: (answered as u32, self.questions.len() as u32),
            preconditions: item_counts(&self.preconditions),
            criteria: item_counts(&self.criteria),
            stories: item_counts(&self.stories),
            depends_on: item_counts(&self.depends_on),
        }
    }

    /// Items still `Pending` across all item sections.
    pub fn pending_count(&self) -> u32 {
        pending(&self.preconditions)
            + pending(&self.criteria)
            + pending(&self.stories)
            + pending(&self.depends_on)
    }

    /// Questions still `Unanswered`.
    pub fn questions_open(&self) -> u32 {
        self.questions
            .iter()
            .filter(|q| q.answer == Answer::Unanswered)
            .count() as u32
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    pub id: IntentId,
    pub repo_id: RepoId,
    pub slug: String,
    pub state: IntentState,
    pub phase: Phase,
    pub seed: Seed,
    pub content: Content,
    pub agent_id: Option<AgentId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub queued_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// Set by the queue when a start attempt fails, cleared on success.
    pub last_start_error: Option<String>,
}

impl Intent {
    pub fn is_complete_content(&self) -> bool {
        self.phase == Phase::Review
            && self.content.pending_count() == 0
            && self.content.questions_open() == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionAuthor {
    Seed,
    Polish1,
    Polish2,
    Send,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentVersion {
    pub intent_id: IntentId,
    pub version: u32,
    pub author: VersionAuthor,
    pub content: Content,
    pub at: DateTime<Utc>,
}

/// Maximum slug length.
const SLUG_MAX: usize = 48;

/// Lowercase ASCII kebab-case. Anything that is not an ASCII letter or digit
/// collapses into a single `-`; the result is trimmed, at most 48 chars (cut
/// on a `-` boundary where possible), and `intent` when empty.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    if slug.len() > SLUG_MAX {
        // Cut exactly at the limit when that lands on a boundary, otherwise
        // back up to the last `-` inside the limit (hard cut if none).
        let at_boundary = slug.as_bytes()[SLUG_MAX] == b'-';
        slug.truncate(SLUG_MAX);
        if !at_boundary {
            if let Some(i) = slug.rfind('-') {
                slug.truncate(i);
            }
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "intent".to_string()
    } else {
        slug.to_string()
    }
}

/// Filter for listing intents.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IntentFilter {
    pub state: Option<IntentState>,
    pub repo_id: Option<RepoId>,
}

/// What to seed a new intent from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedInput {
    Chat {
        agent_id: AgentId,
        from_seq: Option<u64>,
        to_seq: Option<u64>,
    },
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateIntentRequest {
    pub repo_id: Option<RepoId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<IntentKind>,
    pub seed: SeedInput,
}

/// Partial update of an intent's content; `None` fields are left alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<IntentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_behavior: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<RepoId>,
}

/// Answer to a question. Wire: `{"option_id":".."}` or `{"skip":true}`.
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerInput {
    Option(String),
    Skip,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum AnswerWire {
    Option { option_id: String },
    Skip { skip: bool },
}

impl Serialize for AnswerInput {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            AnswerInput::Option(id) => AnswerWire::Option {
                option_id: id.clone(),
            },
            AnswerInput::Skip => AnswerWire::Skip { skip: true },
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for AnswerInput {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match AnswerWire::deserialize(d)? {
            AnswerWire::Option { option_id } => Ok(AnswerInput::Option(option_id)),
            AnswerWire::Skip { skip: true } => Ok(AnswerInput::Skip),
            AnswerWire::Skip { skip: false } => {
                Err(serde::de::Error::custom("`skip` must be true"))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Accept,
    Edit(String),
    Drop,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentDefaults {
    pub queue_limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AgentId, IntentId, ItemId, QuestionId, RepoId};

    fn item(status: ItemStatus) -> Item {
        Item {
            id: ItemId::new(),
            text: "x".into(),
            status,
            edited: false,
            intent_ref: None,
        }
    }

    fn question(answer: Answer) -> Question {
        Question {
            id: QuestionId::new(),
            text: "q".into(),
            options: vec![QuestionOption {
                id: "a".into(),
                label: "A".into(),
            }],
            answer,
        }
    }

    fn content() -> Content {
        Content {
            title: "T".into(),
            kind: IntentKind::Bug,
            current_behavior: String::new(),
            questions: vec![],
            preconditions: vec![],
            criteria: vec![],
            stories: vec![],
            depends_on: vec![],
        }
    }

    fn intent(phase: Phase, content: Content) -> Intent {
        let now = chrono::Utc::now();
        Intent {
            id: IntentId::new(),
            repo_id: RepoId::new(),
            slug: "t".into(),
            state: IntentState::Draft,
            phase,
            seed: Seed {
                source: SeedSource::Text,
                text: "s".into(),
                kind: None,
            },
            content,
            agent_id: None,
            created_at: now,
            updated_at: now,
            queued_at: None,
            sent_at: None,
            resolved_at: None,
            last_start_error: None,
        }
    }

    #[test]
    fn phase_json_shapes() {
        let failed = Phase::Failed {
            pass: 2,
            message: "boom".into(),
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert_eq!(json, r#"{"kind":"failed","pass":2,"message":"boom"}"#);
        assert_eq!(serde_json::from_str::<Phase>(&json).unwrap(), failed);
        assert_eq!(
            serde_json::to_string(&Phase::Polishing { pass: 1 }).unwrap(),
            r#"{"kind":"polishing","pass":1}"#
        );
        assert_eq!(
            serde_json::to_string(&Phase::Review).unwrap(),
            r#"{"kind":"review"}"#
        );
    }

    #[test]
    fn answer_json_shapes() {
        let a = Answer::Option {
            option_id: "a".into(),
        };
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, r#"{"option":{"option_id":"a"}}"#);
        assert_eq!(serde_json::from_str::<Answer>(&json).unwrap(), a);
        assert_eq!(
            serde_json::to_string(&Answer::Unanswered).unwrap(),
            r#""unanswered""#
        );
        assert_eq!(
            serde_json::to_string(&Answer::Skipped).unwrap(),
            r#""skipped""#
        );
    }

    #[test]
    fn counters_over_mixed_content() {
        let mut c = content();
        c.criteria = vec![
            item(ItemStatus::Accepted),
            item(ItemStatus::Accepted),
            item(ItemStatus::Dropped),
        ];
        c.questions = vec![
            question(Answer::Skipped),
            question(Answer::Unanswered),
            question(Answer::Option {
                option_id: "a".into(),
            }),
        ];
        let n = c.counters();
        assert_eq!(n.criteria, (2, 3));
        assert_eq!(n.questions, (2, 3));
        assert_eq!(n.stories, (0, 0));
        assert_eq!(c.pending_count(), 0);
        assert_eq!(c.questions_open(), 1);
    }

    #[test]
    fn is_complete_content_requires_review_and_all_decided() {
        let mut c = content();
        c.criteria = vec![item(ItemStatus::Pending)];
        c.questions = vec![question(Answer::Unanswered)];
        let mut i = intent(Phase::Review, c);
        assert!(!i.is_complete_content());
        i.content.criteria[0].status = ItemStatus::Accepted;
        assert!(!i.is_complete_content());
        i.content.questions[0].answer = Answer::Skipped;
        assert!(i.is_complete_content());
        i.phase = Phase::Questions;
        assert!(!i.is_complete_content());
    }

    #[test]
    fn kinds_have_stable_wire_names() {
        let names = [
            "new_feature",
            "enhancement",
            "bug",
            "refactor",
            "maintenance",
            "task",
        ];
        for (k, name) in IntentKind::ALL.into_iter().zip(names) {
            assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{name}\""));
            assert_eq!(k.wire_name(), name);
            assert_eq!(IntentKind::from_wire(k.wire_name()), Some(k));
        }
        assert_eq!(IntentKind::from_wire("feature"), None);
    }

    #[test]
    fn old_seed_and_request_json_still_load() {
        let seed: Seed = serde_json::from_str(r#"{"source":"text","text":"s"}"#).unwrap();
        assert_eq!(seed.kind, None);
        let json = serde_json::to_value(&seed).unwrap();
        assert!(json.get("kind").is_none());

        let req: CreateIntentRequest =
            serde_json::from_str(r#"{"repo_id":null,"seed":{"text":"hi"}}"#).unwrap();
        assert_eq!(req.kind, None);

        let req = CreateIntentRequest {
            repo_id: None,
            kind: Some(IntentKind::Refactor),
            seed: SeedInput::Text("hi".into()),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["kind"], "refactor");
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("Add dark-mode toggle!"), "add-dark-mode-toggle");
        assert_eq!(slugify("  "), "intent");
        assert_eq!(slugify("🎉🎉"), "intent");
        assert_eq!(slugify("A  B--C"), "a-b-c");
        assert_eq!(slugify("Café 2 été"), "caf-2-t");
        let long = slugify(&"word ".repeat(40));
        assert!(long.len() <= 48 && !long.ends_with('-'), "{long}");
        assert!(long.starts_with("word-word"));
        let one = slugify(&"x".repeat(200));
        assert_eq!(one.len(), 48);
    }

    #[test]
    fn request_and_input_wire_shapes() {
        let agent_id: AgentId = "6f1d3a52-8c4e-4b7a-9e21-0d5c7f3b9a10".parse().unwrap();
        let chat = SeedInput::Chat {
            agent_id,
            from_seq: Some(1),
            to_seq: None,
        };
        let json = serde_json::to_string(&chat).unwrap();
        assert_eq!(
            json,
            r#"{"chat":{"agent_id":"6f1d3a52-8c4e-4b7a-9e21-0d5c7f3b9a10","from_seq":1,"to_seq":null}}"#
        );
        assert_eq!(serde_json::from_str::<SeedInput>(&json).unwrap(), chat);
        let text = SeedInput::Text("hi".into());
        assert_eq!(serde_json::to_string(&text).unwrap(), r#"{"text":"hi"}"#);

        let opt = AnswerInput::Option("x".into());
        assert_eq!(serde_json::to_string(&opt).unwrap(), r#"{"option_id":"x"}"#);
        assert_eq!(
            serde_json::from_str::<AnswerInput>(r#"{"option_id":"x"}"#).unwrap(),
            opt
        );
        assert_eq!(
            serde_json::to_string(&AnswerInput::Skip).unwrap(),
            r#"{"skip":true}"#
        );
        assert_eq!(
            serde_json::from_str::<AnswerInput>(r#"{"skip":true}"#).unwrap(),
            AnswerInput::Skip
        );
        assert!(serde_json::from_str::<AnswerInput>(r#"{"skip":false}"#).is_err());
        assert!(serde_json::from_str::<AnswerInput>(r#"{}"#).is_err());

        assert_eq!(
            serde_json::to_string(&ReviewAction::Accept).unwrap(),
            r#""accept""#
        );
        assert_eq!(
            serde_json::to_string(&ReviewAction::Edit("t".into())).unwrap(),
            r#"{"edit":"t"}"#
        );
        assert_eq!(
            serde_json::to_string(&ReviewAction::Drop).unwrap(),
            r#""drop""#
        );

        let patch = Patch {
            title: Some("n".into()),
            ..Patch::default()
        };
        assert_eq!(serde_json::to_string(&patch).unwrap(), r#"{"title":"n"}"#);
        assert_eq!(serde_json::to_string(&Patch::default()).unwrap(), "{}");

        let req = CreateIntentRequest {
            repo_id: None,
            kind: None,
            seed: SeedInput::Text("hi".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            serde_json::from_str::<CreateIntentRequest>(&json).unwrap(),
            req
        );
        assert_eq!(IntentFilter::default().state, None);
        assert_eq!(
            serde_json::to_string(&AgentDefaults {
                queue_limit: Some(3)
            })
            .unwrap(),
            r#"{"queue_limit":3}"#
        );
    }
}
