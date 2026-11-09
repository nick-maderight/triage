//! jcode panes run the `jcode` binary. Its session lives under `$JCODE_HOME`
//! (default `~/.jcode`). `client_sessions/<pid>` names the session id. The messages live in
//! `sessions/<sid>.json` and `<sid>.journal.jsonl`. `turn-status/` holds the turn-end crumb.

use crate::omp::{self, DispatchAudit, PaneState};
use crate::tmux;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// Sent into a session to start the run of an approved plan.
pub const PLAN_APPROVAL: &str =
    "Plan approved. Carry out the plan above end to end, then end with the final report it names.";

// shortcut: hand-copied from ~/.jcode/config.toml. Read the profiles there when models change often.
pub const MODELS: [&str; 11] = [
    "bedrock-claude/global.anthropic.claude-fable-5-1",
    "bedrock-claude/global.anthropic.claude-opus-5-5",
    "bedrock-claude/global.anthropic.claude-sonnet-5-5",
    "bedrock-claude/global.anthropic.claude-haiku-5-5",
    "bedrock-luna/global.openai.gpt-6-luna",
    "bedrock-openai/global.moonshotai.kimi-k3.1",
    "bedrock-openai/global.zai.glm-5.3",
    "bedrock-openai/global.xai.grok-4.7",
    "deepseek-api/deepseek-v4-pro",
    "deepseek-api/deepseek-v4-flash",
    "deepseek-api/deepseek-v4-flash-vision-exp",
];

/// `$JCODE_HOME`, else `$HOME/.jcode`.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("JCODE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".jcode")))
}

/// True for a jcode pane. An omp pane is never jcode and skips the `/proc` read.
pub fn is_jcode(pane: &tmux::Pane) -> bool {
    if pane.command == "omp" {
        return false;
    }
    basename(&pane.command).starts_with("jcode")
        || pane
            .foreground_argv()
            .and_then(|argv| argv.into_iter().next())
            .is_some_and(|argv0| basename(&argv0).contains("jcode"))
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Working while the title shows a turn (` · work`), Waiting otherwise.
pub fn classify(title: &str) -> PaneState {
    if title.contains(" · work") {
        PaneState::Working
    } else {
        PaneState::Waiting
    }
}

/// The session id from the client file of `pane`, read for the foreground process first.
pub fn session_id(pane: &tmux::Pane) -> Option<String> {
    session_in(&home()?, pane)
}

fn session_in(home: &Path, pane: &tmux::Pane) -> Option<String> {
    pane.foreground_pid()
        .into_iter()
        .chain(std::iter::once(pane.pid))
        .find_map(|pid| {
            let text =
                fs::read_to_string(home.join("client_sessions").join(pid.to_string())).ok()?;
            let id = text.trim();
            let valid = !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
            valid.then(|| id.to_owned())
        })
}

/// A message of a jcode session. `text` joins its text blocks with blank lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub text: String,
}

/// The messages of session `sid` under `$JCODE_HOME`.
pub fn messages(sid: &str) -> io::Result<Vec<Message>> {
    let home = home().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no jcode home"))?;
    read_messages(&home.join("sessions"), sid)
}

/// The session's snapshot file, which the tracker cites as a card's ref.
pub fn snapshot_path(sid: &str) -> Option<PathBuf> {
    Some(home()?.join("sessions").join(format!("{sid}.json")))
}

/// Modification times of a session's snapshot and journal. A new turn changes at least one.
pub fn file_times(sid: &str) -> (Option<SystemTime>, Option<SystemTime>) {
    let modified = |path: Option<PathBuf>| fs::metadata(path?).ok()?.modified().ok();
    let journal = home().map(|home| home.join("sessions").join(format!("{sid}.journal.jsonl")));
    (modified(snapshot_path(sid)), modified(journal))
}

/// The snapshot's messages, then the journal's appended ones. A repeated id replaces the
/// earlier message in place.
fn read_messages(sessions: &Path, sid: &str) -> io::Result<Vec<Message>> {
    let snapshot = fs::read_to_string(sessions.join(format!("{sid}.json")))?;
    let snapshot: Value = serde_json::from_str(&snapshot).map_err(io::Error::other)?;
    let journal = match fs::read_to_string(sessions.join(format!("{sid}.journal.jsonl"))) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut messages: Vec<Message> = Vec::new();
    let mut slots: HashMap<String, usize> = HashMap::new();
    let mut add = |value: &Value| {
        let Some(message) = parse_message(value) else {
            return;
        };
        match slots.get(&message.id) {
            Some(&slot) => messages[slot] = message,
            None => {
                slots.insert(message.id.clone(), messages.len());
                messages.push(message);
            }
        }
    };
    for value in snapshot
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        add(value);
    }
    for line in journal.lines() {
        // A torn line from a live append does not parse. The read after that write sees it.
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for value in entry
            .get("append_messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            add(value);
        }
    }
    Ok(messages)
}

fn parse_message(value: &Value) -> Option<Message> {
    let text = value
        .get("content")?
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(Message {
        id: value.get("id")?.as_str()?.to_owned(),
        role: value.get("role")?.as_str()?.to_owned(),
        text,
    })
}

/// The last assistant message with text.
pub fn last_assistant(messages: &[Message]) -> Option<&Message> {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && !message.text.trim().is_empty())
}

/// The body of a reply's `plan` fence and the title from its first `# ` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub title: String,
    pub body: String,
}

/// The first ```` ```plan ```` fence in `text`. The fence ends at a line that is exactly
/// ```` ``` ````, or at the end of the text.
pub fn plan(text: &str) -> Option<Plan> {
    let mut lines = text.lines();
    lines.find(|line| line.starts_with("```plan"))?;
    let body: Vec<&str> = lines.take_while(|line| *line != "```").collect();
    let title = body
        .iter()
        .find_map(|line| line.strip_prefix("# "))
        .map_or_else(|| "Plan".to_owned(), |title| title.trim().to_owned());
    Some(Plan {
        title,
        body: body.join("\n"),
    })
}

/// The turn-end crumb that `jcode-guard turn-end` writes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TurnStatus {
    pub status: String,
    #[serde(default)]
    pub error: String,
    pub ts_ms: i64,
}

/// The session's turn-end crumb, if one exists.
pub fn turn_status(sid: &str) -> Option<TurnStatus> {
    let text = fs::read_to_string(home()?.join("turn-status").join(format!("{sid}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Mirrors `dispatch_audit` in `omp.rs` over jcode messages. `jcode transcript` prefixes plain
/// text with `[transcription] `. The prefix is stripped before the brief path is looked for.
pub fn dispatch_audit(messages: &[Message]) -> DispatchAudit {
    let mut audit = DispatchAudit::default();
    for message in messages {
        match message.role.as_str() {
            "user" => {
                let text = message
                    .text
                    .strip_prefix("[transcription] ")
                    .unwrap_or(&message.text);
                if text.contains("triage/briefs/") {
                    audit.dispatched = true;
                    audit.reported = false;
                }
            }
            "assistant" if !message.text.trim().is_empty() => {
                if omp::final_report(&message.text).is_some() {
                    audit.reported = true;
                }
                audit.entry_id = Some(message.id.clone());
                audit.last_text = Some(message.text.clone());
            }
            _ => {}
        }
    }
    audit
}

/// Types `text` into the session's TUI through `jcode transcript`.
fn transcript(sid: &str, text: &str) -> Result<(), String> {
    let mut child = Command::new("jcode")
        .args([
            "--quiet",
            "--no-update",
            "transcript",
            "-S",
            sid,
            "--mode",
            "send",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Sends a plain message into the session. Text that starts with `/`, `!` or `$` would run
/// as a command inside the TUI. Such text is refused.
pub fn send_text(sid: &str, text: &str) -> Result<(), String> {
    if text.trim_start().starts_with(['/', '!', '$']) {
        return Err("text starting with / ! or $ is not sent to jcode as a message".to_owned());
    }
    transcript(sid, text)
}

/// Sends one of triage's own `/model` or `/plan` lines. The caller builds the line.
pub fn send_command(sid: &str, line: &str) -> Result<(), String> {
    transcript(sid, line)
}

/// The `/model` line for a `PROFILE/ID` selector. Bare names and `@role` selectors are refused.
pub fn selector_command(selector: &str) -> Result<String, String> {
    let plain = selector
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._:/@-".contains(&byte));
    match selector.split_once('/') {
        Some((profile, id))
            if plain && !profile.is_empty() && !id.is_empty() && !profile.starts_with('@') =>
        {
            Ok(format!("/model {profile}:{id}"))
        }
        _ => Err(format!("jcode selector must be PROFILE/ID, got {selector}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_sessions() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jcode/sessions")
    }

    fn pane(command: &str, pid: u32) -> tmux::Pane {
        tmux::Pane {
            id: "%12".to_owned(),
            title: String::new(),
            command: command.to_owned(),
            tty: "/dev/pts/12".to_owned(),
            target: "triage:1.0".to_owned(),
            session: "triage".to_owned(),
            triage_card_tag: None,
            cwd: "/tmp".to_owned(),
            width: 80,
            height: 24,
            pid,
        }
    }

    fn message(id: &str, role: &str, text: &str) -> Message {
        Message {
            id: id.to_owned(),
            role: role.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn classify_reads_the_turn_marker_in_the_title() {
        assert_eq!(
            classify("🦞 jcode/brook Lobster · work ~2s"),
            PaneState::Working
        );
        assert_eq!(
            classify("🦞 jcode/brook Lobster · last ~3s"),
            PaneState::Waiting
        );
    }

    #[test]
    fn omp_and_shell_panes_are_not_jcode() {
        let missing = 2_000_000_000;
        assert!(is_jcode(&pane("jcode", missing)));
        assert!(!is_jcode(&pane("omp", missing)));
        assert!(!is_jcode(&pane("zsh", missing)));
    }

    #[test]
    fn session_id_comes_from_the_client_file_and_must_be_a_plain_name() {
        let home = std::env::temp_dir().join(format!("triage-jcode-home-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(home.join("client_sessions")).unwrap();
        let file = home.join("client_sessions/2000000000");
        let jcode = pane("jcode", 2_000_000_000);
        fs::write(&file, "session_lobster_1\n").unwrap();
        assert_eq!(
            session_in(&home, &jcode).as_deref(),
            Some("session_lobster_1")
        );
        fs::write(&file, "../escape").unwrap();
        assert_eq!(session_in(&home, &jcode), None);
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn journal_appends_and_replaces_earlier_messages_by_id() {
        let messages = read_messages(&fixture_sessions(), "s1").unwrap();
        let ids: Vec<&str> = messages.iter().map(|message| message.id.as_str()).collect();
        assert_eq!(ids, ["m1", "m2", "m3", "m4"]);
        assert!(messages[2].text.starts_with("```plan"));
        assert_eq!(
            last_assistant(&messages).map(|message| message.id.as_str()),
            Some("m4")
        );
        let absent = read_messages(&fixture_sessions(), "absent").unwrap_err();
        assert_eq!(absent.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn plan_takes_the_first_fence_and_its_title() {
        let text = "Intro\n```plan\n# Ship the fix\n\n1. Edit.\n```\nAfter.";
        let found = plan(text).unwrap();
        assert_eq!(found.title, "Ship the fix");
        assert_eq!(found.body, "# Ship the fix\n\n1. Edit.");
        assert_eq!(plan("no fence here"), None);
    }

    #[test]
    fn dispatch_audit_finds_the_brief_in_prefixed_and_plan_prompts() {
        let messages = vec![
            message(
                "u1",
                "user",
                "[transcription] Read the brief at /home/u/triage/briefs/x.md and carry it out.",
            ),
            message("a1", "assistant", "Working."),
            message("a2", "assistant", "ENVOY_STATUS: complete\nDone."),
        ];
        assert_eq!(
            dispatch_audit(&messages),
            DispatchAudit {
                dispatched: true,
                reported: true,
                entry_id: Some("a2".to_owned()),
                last_text: Some("ENVOY_STATUS: complete\nDone.".to_owned()),
            }
        );
        let planning = [message(
            "u2",
            "user",
            "You are entering planning mode.\n\nGoal: Read /x/triage/briefs/y.md",
        )];
        assert!(dispatch_audit(&planning).dispatched);
        let unreported = dispatch_audit(&messages[..2]);
        assert!(unreported.dispatched && !unreported.reported);
    }

    #[test]
    fn send_text_refuses_commands_before_any_process_starts() {
        for text in ["/model x", "  !ls", "$HOME"] {
            assert!(send_text("s1", text).is_err(), "{text}");
        }
    }

    #[test]
    fn selector_command_turns_profile_and_id_into_a_model_line() {
        assert_eq!(
            selector_command("bedrock-claude/global.anthropic.claude-haiku-5-5").unwrap(),
            "/model bedrock-claude:global.anthropic.claude-haiku-5-5"
        );
        for bad in ["@kimi", "bedrock-claude/", "/global.x", "@role/x", "a b/c"] {
            assert!(selector_command(bad).is_err(), "{bad}");
        }
    }
}
