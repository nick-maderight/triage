//! Shared types. These are the contract between modules; change a signature
//! here only together with every caller.

use serde::{Deserialize, Serialize};

/// A CloudEvents 1.0 envelope as accepted by `POST /v1/ingest`.
/// `priority` is a CloudEvents extension attribute (1 high .. 5 low).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudEvent {
    pub specversion: String,
    pub id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub subject: Option<String>,
    pub time: Option<String>,
    #[serde(default, deserialize_with = "deserialize_priority")]
    pub priority: Option<u8>,
    #[serde(default)]
    pub data: serde_json::Value,
}

fn deserialize_priority<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let priority = Option::<u8>::deserialize(deserializer)?;
    if priority.is_some_and(|priority| !(1..=5).contains(&priority)) {
        return Err(serde::de::Error::custom("priority must be between 1 and 5"));
    }
    Ok(priority)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Ask,
    Finding,
    Info,
    Plan,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    pub uri: String,
    pub label: String,
    #[serde(default)]
    pub cwd: String,
    /// tmux pane id such as `%12`, when the source is a pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// Human tmux target such as `bill:1.0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Star {
    #[serde(default)]
    pub situation: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub result: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CardOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Finding only. Extra direction appended to the brief when this option is picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default)]
    pub recommended: bool,
}

/// What the inbox shows. Serialized as-is by `GET /v1/queue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub kind: Kind,
    pub priority: u8,
    pub created_ms: i64,
    pub source: Source,
    pub title: String,
    #[serde(default)]
    pub star: Star,
    #[serde(default)]
    pub options: Vec<CardOption>,
    /// False when the hub cannot type the answer (multi-select, pane gone).
    pub answerable: bool,
    /// Ask cards whose dialog offers "Other (type your own)".
    pub allow_other: bool,
    /// Ask cards with more than one question: every question, first included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<CardQuestion>,
    /// Info cards: the final reply text of an agent turn, or a report body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Ask cards: the omp tool call id, used to notice an answer given in the pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Info replies from launched panes without a status line can request a final report.
    #[serde(default)]
    pub report_due: bool,
    /// Harness of a session that is not omp, such as `dsh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
}

/// One question of a multi-question ask card.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CardQuestion {
    pub label: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<CardOption>,
}

/// The user's verdict on a card. Stored as an event of type `swipe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Swipe {
    pub card: String,
    /// `done`, `skipped`, or `later`.
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `later` only. Unix ms when the card returns to the queue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<i64>,
    /// Free-text reasons include `answered in pane` and `resumed in pane`.
    /// Other examples include `pane exited`, `superseded` or `launched %41`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at_ms: i64,
}

/// One answer of a multi-question ask: the chosen option index, or custom text.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Answer {
    pub option: Option<usize>,
    pub text: Option<String>,
}

/// Request body of `POST /v1/cards/{id}/answer`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnswerReq {
    pub option: Option<usize>,
    pub text: Option<String>,
    pub note: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub executor: Option<String>,
    /// Effort the execution model takes in a Plan into launch or approval. Empty keeps the
    /// planner's level, which is the session's launch effort for dsh.
    #[serde(default)]
    pub executor_thinking: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub answers: Vec<Answer>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Event type names used for internally generated events.
pub const T_CARD_ASK: &str = "card.ask";
pub const T_CARD_FINDING: &str = "card.finding";
pub const T_CARD_INFO: &str = "card.info";
pub const T_CARD_PLAN: &str = "card.plan";
pub const T_SWIPE: &str = "swipe";
pub const T_FINDING: &str = "triage.finding";
pub const T_INFO: &str = "triage.info";

impl Card {
    pub fn card_event_type(&self) -> &'static str {
        match self.kind {
            Kind::Ask => T_CARD_ASK,
            Kind::Finding => T_CARD_FINDING,
            Kind::Info => T_CARD_INFO,
            Kind::Plan => T_CARD_PLAN,
        }
    }
}
