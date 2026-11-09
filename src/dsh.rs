//! Client for the dsh bridge that dsh-host serves on loopback behind a bearer secret.

use crate::events::Card;
use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Check a workspace name against the bridge's rule: one lowercase path segment, which
/// keeps every session in the directory the name points at. A blank name means unset.
pub fn workspace_name(name: Option<&str>) -> Result<Option<&str>, String> {
    let name = name.map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = name {
        let allowed = !name.is_empty()
            && name.len() <= 64
            && name != "."
            && name != ".."
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c));
        if !allowed {
            return Err(format!("invalid dsh workspace {name}"));
        }
    }
    Ok(name)
}

/// The models dsh offers, as (selector, display name).
pub const MODELS: [(&str, &str); 2] = [
    ("deepseek-flash", "DeepSeek-V4.1-Flash"),
    ("deepseek-v4-pro", "DeepSeek-V4-Pro"),
];
/// The effort levels dsh shows.
pub const THINKING: [&str; 4] = ["off", "low", "high", "max"];
/// The model a dsh launch uses when none is chosen.
pub const DEFAULT_MODEL: &str = "deepseek-flash";
/// Caps a background poll, which runs under the tracker lock.
pub const TICK_TIMEOUT: Duration = Duration::from_secs(3);
/// Caps a card action or launch, which a person waits on.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// The bridge address and secret dsh-host publishes. Never log them.
#[derive(Deserialize)]
pub struct Host {
    pub api: String,
    pub secret: String,
}

/// `DSH_HOST_FILE`, else `~/.dsh/run/host.json`.
pub fn host_file() -> Option<PathBuf> {
    match std::env::var_os("DSH_HOST_FILE") {
        Some(path) if !path.is_empty() => Some(path.into()),
        _ => Some(PathBuf::from(std::env::var_os("HOME")?).join(".dsh/run/host.json")),
    }
}

impl Host {
    pub fn load(path: &Path) -> Option<Host> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

/// Client for the bridge named by the host file, or the reason there is none.
pub fn connect(host_file: Option<&Path>, timeout: Duration) -> Result<Client, String> {
    let host = host_file
        .and_then(Host::load)
        .ok_or_else(|| "dsh is not running".to_owned())?;
    Ok(Client::new(&host, timeout))
}

/// One session from the bridge. `cursor` advances with every logged event.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub blank: bool,
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub updated_at_ms: i64,
    #[serde(default)]
    pub cursor: i64,
    pub goal_phase: Option<String>,
}

/// Live state of a session. Non-empty pending input means a question or plan review waits.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub pending_input: Vec<String>,
}

/// A question or plan review that waits for an answer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub session_id: String,
    pub event_id: String,
    #[serde(default)]
    pub call_id: String,
    pub kind: String,
    #[serde(default)]
    pub questions: Vec<Question>,
    pub detail: Option<String>,
}

/// One question from a dsh waterfall.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: Option<String>,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// A message, tool call or turn end that the bridge keeps for a session.
#[derive(Deserialize)]
pub struct Event {
    pub seq: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub name: Option<String>,
    pub command: Option<String>,
}

/// Bridge connection. Errors carry the bridge's text, never the secret.
pub struct Client {
    agent: ureq::Agent,
    api: String,
    bearer: String,
}

#[derive(Deserialize)]
struct Listed<T> {
    #[serde(default)]
    pid: u32,
    items: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Launched {
    session_id: String,
}

impl Client {
    pub fn new(host: &Host, timeout: Duration) -> Client {
        Client {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(timeout))
                .build()
                .into(),
            api: host.api.clone(),
            bearer: format!("Bearer {}", host.secret),
        }
    }

    /// The bridge's host pid and the sessions it lists.
    pub fn sessions(&self) -> Result<(u32, Vec<Session>), String> {
        let listed: Listed<Session> = self.get("/v1/sessions", &[])?;
        Ok((listed.pid, listed.items))
    }

    /// The host pid and the questions and plan reviews that wait for an answer.
    pub fn pending(&self) -> Result<(u32, Vec<Pending>), String> {
        let listed: Listed<Pending> = self.get("/v1/pending", &[])?;
        Ok((listed.pid, listed.items))
    }

    pub fn state(&self, session_id: &str) -> Result<State, String> {
        self.get("/v1/state", &[("sessionId", session_id)])
    }

    /// The newest `limit` events of a session, oldest first.
    pub fn events(&self, session_id: &str, limit: usize) -> Result<Vec<Event>, String> {
        let limit = limit.to_string();
        let listed: Listed<Event> = self.get(
            "/v1/events",
            &[("sessionId", session_id), ("limit", &limit)],
        )?;
        Ok(listed.items)
    }

    /// Start a session from a `/v1/launch` body and return its id.
    pub fn launch(&self, body: &Value) -> Result<String, String> {
        let launched: Launched = self.post("/v1/launch", body)?;
        Ok(launched.session_id)
    }

    pub fn prompt(&self, session_id: &str, text: &str) -> Result<(), String> {
        self.post::<IgnoredAny>(
            "/v1/prompt",
            &json!({"sessionId": session_id, "text": text}),
        )?;
        Ok(())
    }

    pub fn cancel(&self, session_id: &str) -> Result<(), String> {
        self.post::<IgnoredAny>("/v1/cancel", &json!({"sessionId": session_id}))?;
        Ok(())
    }

    /// Send the answer value for a pending item. `executor` rides with an Approve.
    pub fn answer(
        &self,
        event_id: &str,
        value: &Value,
        executor: Option<&str>,
    ) -> Result<(), String> {
        let body = json!({"eventId": event_id, "value": value, "executor": executor});
        self.post::<IgnoredAny>("/v1/answer", &body)?;
        Ok(())
    }

    /// Open the dsh web UI on the desktop.
    pub fn open(&self) -> Result<(), String> {
        self.post::<IgnoredAny>("/v1/open", &json!({}))?;
        Ok(())
    }

    fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T, String> {
        let mut request = self
            .agent
            .get(format!("{}{path}", self.api))
            .header("Authorization", self.bearer.as_str());
        for (key, value) in query {
            request = request.query(*key, *value);
        }
        finish(request.call())
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T, String> {
        let request = self
            .agent
            .post(format!("{}{path}", self.api))
            .header("Authorization", self.bearer.as_str());
        finish(request.send_json(body))
    }
}

/// Reads the reply, or the bridge's `error` text, or the status.
fn finish<T: DeserializeOwned>(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, String> {
    let mut response = response.map_err(|error| format!("dsh bridge unreachable: {error}"))?;
    let status = response.status();
    if status.is_success() {
        return response
            .body_mut()
            .read_json()
            .map_err(|error| format!("dsh bridge sent an unreadable reply: {error}"));
    }
    let error = response
        .body_mut()
        .read_json::<Value>()
        .ok()
        .and_then(|body| body["error"].as_str().map(str::to_owned));
    Err(error.unwrap_or_else(|| format!("dsh bridge returned {}", status.as_u16())))
}

/// The dsh session behind a card, if the card came from dsh.
pub fn session_id(card: &Card) -> Option<&str> {
    if card.engine.as_deref() != Some("dsh") {
        return None;
    }
    card.source.uri.strip_prefix("dsh://session/")
}

/// A session's recent events as plain text, keeping the newest 120 lines.
pub fn peek_text(events: &[Event]) -> String {
    let mut lines = Vec::new();
    for event in events {
        match event.kind.as_str() {
            "assistant/message" => {
                lines.extend(
                    event
                        .text
                        .as_deref()
                        .unwrap_or_default()
                        .lines()
                        .map(str::to_owned),
                );
            }
            "tool/call" => {
                let name = event.name.as_deref().unwrap_or_default();
                lines.push(match event.command.as_deref() {
                    Some(command) => format!("› {name} {command}"),
                    None => format!("› {name}"),
                });
            }
            "turn/end" => {
                lines.push(format!(
                    "— turn ended: {}",
                    event.reason.as_deref().unwrap_or_default()
                ));
            }
            _ => {}
        }
    }
    lines[lines.len().saturating_sub(120)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str) -> Event {
        Event {
            seq: 0,
            kind: kind.to_owned(),
            text: None,
            reason: None,
            error: None,
            name: None,
            command: None,
        }
    }

    #[test]
    fn workspace_names_are_one_lowercase_path_segment() {
        assert_eq!(workspace_name(Some("triage")), Ok(Some("triage")));
        assert_eq!(workspace_name(Some(" autorules ")), Ok(Some("autorules")));
        assert_eq!(workspace_name(Some("")), Ok(None));
        let long = "x".repeat(65);
        for name in ["Triage", "a/b", "..", ".", "a b", &long] {
            assert!(workspace_name(Some(name)).is_err(), "{name}");
        }
    }

    #[test]
    fn peek_keeps_the_newest_120_lines_and_names_tool_calls() {
        let message = Event {
            text: Some("one\ntwo".to_owned()),
            ..event("assistant/message")
        };
        let tool = Event {
            name: Some("bash".to_owned()),
            command: Some("ls".to_owned()),
            ..event("tool/call")
        };
        let end = Event {
            reason: Some("completed".to_owned()),
            ..event("turn/end")
        };
        assert_eq!(
            peek_text(&[message, tool, end]),
            "one\ntwo\n› bash ls\n— turn ended: completed"
        );
        let many: Vec<Event> = (0..130)
            .map(|n| Event {
                text: Some(format!("line {n}")),
                ..event("assistant/message")
            })
            .collect();
        let text = peek_text(&many);
        assert_eq!(text.lines().count(), 120);
        assert!(text.starts_with("line 10\n"));
    }
}
