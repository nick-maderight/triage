//! omp harness knowledge: title states, transcript reading, dialog parsing.
//! Owner: tmux worker. Pure functions over strings and files, unit tested.

use crate::tmux;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneState {
    /// Title `π ` + spinner frame.
    Working,
    /// Title `π >`.
    Waiting,
    /// Title `π !`.
    Asking,
    /// Not an omp pane.
    Other,
}

/// Classify an omp pane by its title. Anything not starting with `π ` is Other.
pub fn classify(title: &str) -> PaneState {
    if title.starts_with("π !") {
        PaneState::Asking
    } else if title.starts_with("π >") {
        PaneState::Waiting
    } else if title.starts_with("π ") {
        PaneState::Working
    } else {
        PaneState::Other
    }
}

/// The transcript named by the pane's omp breadcrumb, `~/.omp/agent/terminal-sessions/`
/// `pts-N` line 2 for the pane's tty `/dev/pts/N`. The crumb names a session, but a
/// new tmux server's panes reuse old pts numbers, so a pane can read a crumb left
/// by an earlier pane's pty. The crumb counts only when it could be this pane's:
/// its mtime is at or after the pty's ctime, the pty's age is unknown, or the pane's
/// omp was started with `--resume` or `--restore` naming this session. Otherwise the
/// pane keeps no transcript until omp writes a fresh crumb.
pub fn transcript_path(pane: &tmux::Pane) -> Option<PathBuf> {
    let number = pane.tty.strip_prefix("/dev/pts/")?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let session_file = PathBuf::from(home)
        .join(".omp/agent/terminal-sessions")
        .join(format!("pts-{number}"));
    let contents = fs::read_to_string(&session_file).ok()?;
    let path = contents
        .lines()
        .nth(1)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)?;
    let crumb_ms = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&session_file)
                .ok()
                .map(|metadata| metadata.mtime() * 1000 + metadata.mtime_nsec() / 1_000_000)
        }
        #[cfg(not(unix))]
        {
            None
        }
    };
    let born = pane.pty_born_ms();
    (crumb_ms
        .zip(born)
        .is_none_or(|(crumb, born)| crumb >= born)
        || (pane.command == "omp"
            && pane
                .foreground_argv()
                .is_some_and(|argv| resumes(&argv, &path))))
    .then_some(path)
}

/// The session id a transcript's file name carries. It is the stem after the first `_`.
fn session_id(transcript: &Path) -> Option<&str> {
    transcript.file_stem()?.to_str()?.split_once('_').map(|(_, id)| id)
}

/// Whether argv starts omp on this transcript's session. `-r`, `--resume` and
/// `--restore` take the session as the next argument, `--resume=<v>` and
/// `--restore=<v>` carry it inline. A value names the session when it equals the
/// transcript path or is a prefix of the session id, the part of the file stem
/// after its first `_`. `-c`, `--continue` and a bare `--resume` do not name a
/// session: omp rewrites the crumb whenever it continues a different session than
/// the crumb names.
pub fn resumes(argv: &[String], transcript: &Path) -> bool {
    let session_id = session_id(transcript);
    let mut value_expected = false;
    for argument in argv.iter().skip(1) {
        if value_expected {
            value_expected = false;
            if !argument.is_empty()
                && (Path::new(argument) == transcript
                    || session_id.is_some_and(|id| id.starts_with(argument.as_str())))
            {
                return true;
            }
            continue;
        }
        let value = match argument.strip_prefix("--") {
            Some(rest) => match rest.split_once('=') {
                Some((flag, value)) => {
                    if flag != "resume" && flag != "restore" {
                        continue;
                    }
                    value
                }
                None => {
                    value_expected = rest == "resume" || rest == "restore";
                    continue;
                }
            },
            None => match argument.strip_prefix('-') {
                Some("r") => {
                    value_expected = true;
                    continue;
                }
                Some(_) => continue,
                None => continue,
            },
        };
        if !value.is_empty()
            && (Path::new(value) == transcript
                || session_id.is_some_and(|id| id.starts_with(value)))
        {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AskOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Question {
    pub id: String,
    pub question: String,
    pub options: Vec<AskOption>,
    #[serde(default)]
    pub recommended: Option<usize>,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub header: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskCall {
    pub tool_call_id: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Propose {
    pub tool_call_id: String,
    pub title: String,
    pub plan_path: PathBuf,
}

/// The last assistant `ask` tool call in the transcript that has no matching
/// toolResult yet. Transcript lines are JSON: `{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":..,"name":"ask","arguments":{"questions":[..]}}]}}`
/// and results are `{"type":"message","message":{"role":"toolResult","toolCallId":..}}`.
pub fn pending_ask(transcript: &std::path::Path) -> std::io::Result<Option<AskCall>> {
    let mut calls = Vec::new();
    let mut results = HashSet::new();
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let Some(content) = message.get("content").and_then(Value::as_array) else {
                    return true;
                };
                for item in content {
                    if item.get("type").and_then(Value::as_str) != Some("toolCall")
                        || item.get("name").and_then(Value::as_str) != Some("ask")
                    {
                        continue;
                    }
                    let (Some(id), Some(questions)) = (
                        item.get("id").and_then(Value::as_str),
                        item.get("arguments").and_then(|args| args.get("questions")),
                    ) else {
                        continue;
                    };
                    let Ok(questions) = serde_json::from_value::<Vec<Question>>(questions.clone()) else {
                        continue;
                    };
                    calls.push(AskCall {
                        tool_call_id: id.to_owned(),
                        questions,
                    });
                }
            }
            Some("toolResult") => {
                if let Some(id) = message.get("toolCallId").and_then(Value::as_str) {
                    results.insert(id.to_owned());
                }
            }
            _ => {}
        }
        true
    })?;
    Ok(calls
        .into_iter()
        .rev()
        .find(|call| !results.contains(&call.tool_call_id)))
}

/// Read plan proposals and matching results from an omp JSONL transcript.
pub fn proposes(transcript: &Path) -> io::Result<Vec<Propose>> {
    let mut calls = Vec::new();
    let mut results = HashMap::new();
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let Some(content) = message.get("content").and_then(Value::as_array) else {
                    return true;
                };
                for item in content {
                    if item.get("type").and_then(Value::as_str) != Some("toolCall")
                        || item.get("name").and_then(Value::as_str) != Some("write")
                        || item
                            .get("arguments")
                            .and_then(|args| args.get("path"))
                            .and_then(Value::as_str)
                            != Some("xd://propose")
                    {
                        continue;
                    }
                    let Some(id) = item.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let content = item
                        .get("arguments")
                        .and_then(|args| args.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    calls.push((id.to_owned(), content));
                }
            }
            Some("toolResult") => {
                if let Some(id) = message.get("toolCallId").and_then(Value::as_str) {
                    let inner = message
                        .get("details")
                        .and_then(|details| details.get("xdev"))
                        .and_then(|xdev| xdev.get("inner"));
                    results.insert(
                        id.to_owned(),
                        (
                            inner
                                .and_then(|inner| inner.get("title"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            inner
                                .and_then(|inner| inner.get("planFilePath"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        ),
                    );
                }
            }
            _ => {}
        }
        true
    })?;

    // omp stores a session's `local://` artifacts in the sibling directory named
    // like the transcript without `.jsonl`, so a plan file lives at
    // `<session dir>/local/<slug>-plan.md`.
    let session_directory = transcript.with_extension("");
    Ok(calls
        .into_iter()
        .map(|(tool_call_id, content)| {
            let result = results.get(&tool_call_id);
            let title = result
                .and_then(|(title, _)| title.as_deref())
                .unwrap_or(&content)
                .to_owned();
            let plan_path = result
                .and_then(|(_, plan_file_path)| plan_file_path.as_deref())
                .and_then(|plan_file_path| {
                    Path::new(plan_file_path.strip_prefix("local://").unwrap_or(plan_file_path))
                        .file_name()
                })
                .map(|basename| session_directory.join("local").join(basename))
                .unwrap_or_default();
            Propose {
                tool_call_id,
                title,
                plan_path,
            }
        })
        .collect())
}

/// Text of the last assistant message that has at least one text block, with the
/// entry id. Text blocks joined by blank lines.
pub fn last_assistant_text(transcript: &std::path::Path) -> std::io::Result<Option<(String, String)>> {
    let mut last = None;
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        if message.get("role").and_then(Value::as_str) == Some("assistant") {
            if let (Some(id), Some(text)) = (
                value.get("id").and_then(Value::as_str),
                text_blocks(message),
            ) {
                last = Some((id.to_owned(), text));
            }
        }
        true
    })?;
    Ok(last)
}

/// How the transcript's last assistant message ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    /// `stopReason` `stop` or `length` with non-blank text. `report` is the latest
    /// earlier assistant text since the last user message whose first line is a
    /// status line, set only when `text` has none.
    Reply { entry_id: String, text: String, report: Option<String> },
    /// `stopReason` `error`. `message` is `errorMessage`, empty when absent.
    Error { entry_id: String, message: String },
    /// `stopReason` `aborted`: an interrupt or a terminal tool ended the run.
    Aborted,
    /// `toolUse`, a missing or unknown stop reason, a stop without text, or a trailing
    /// delivered background-job result or supervised-process exit: the run is still going, or
    /// its last message is not on disk yet.
    Running,
}

/// Classify the transcript's last assistant message the way omp settles a run:
/// `error` is an error, `aborted` is idle, a `stop` with text is done. A trailing
/// delivered background-job result (`async-result`) or supervised-process exit
/// (`launch-completion`) wakes the run. It reads as `Running` until the resumed turn
/// writes its next message.
pub fn turn_end(transcript: &Path) -> io::Result<Option<TurnEnd>> {
    // (entry id, stopReason, errorMessage, non-blank text)
    let mut last: Option<(String, Option<String>, Option<String>, Option<String>)> = None;
    let mut report: Option<String> = None;
    let mut woken = false;
    scan_jsonl(transcript, |value| {
        if value.get("type").and_then(Value::as_str) == Some("custom_message") {
            if matches!(
                value.get("customType").and_then(Value::as_str),
                Some("async-result" | "launch-completion")
            ) {
                woken = true;
            }
            return true;
        }
        let Some(message) = message(&value) else {
            return true;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                report = None;
                woken = false;
            }
            Some("assistant") => {
                let Some(id) = value.get("id").and_then(Value::as_str) else {
                    return true;
                };
                let text = text_blocks(message).filter(|text| !text.trim().is_empty());
                if let Some(text) = text.as_deref().filter(|text| final_report(text).is_some()) {
                    report = Some(text.to_owned());
                }
                let field = |key| message.get(key).and_then(Value::as_str).map(str::to_owned);
                last = Some((id.to_owned(), field("stopReason"), field("errorMessage"), text));
                woken = false;
            }
            _ => {}
        }
        true
    })?;
    let Some((entry_id, stop, error, text)) = last else {
        return Ok(None);
    };
    Ok(Some(if woken {
        TurnEnd::Running
    } else {
        match (stop.as_deref(), text) {
            (Some("stop" | "length"), Some(text)) => TurnEnd::Reply {
                report: report.filter(|_| final_report(&text).is_none()),
                entry_id,
                text,
            },
            (Some("error"), _) => TurnEnd::Error {
                entry_id,
                message: error.unwrap_or_default(),
            },
            (Some("aborted"), _) => TurnEnd::Aborted,
            _ => TurnEnd::Running,
        }
    }))
}

/// A running supervised process holds a reply card for at most this long, counted from its start.
/// Past that, a keepalive loop or other process meant to outlive the run cannot hide replies for good.
const SERVICE_HOLD_MS: u64 = 2 * 60 * 60 * 1000;
/// How long after a supervised process exits its completion may still be on its way to
/// the session. omp appends it within seconds, whether the session is idle or mid-turn.
const COMPLETION_DELIVERY_MS: u64 = 60 * 1000;

/// Whether omp will start another turn for the session behind `transcript` when one of
/// its supervised processes (`bash` with `name`) exits. omp settles the run while such a
/// process runs, then reports the exit as a new turn. That covers a process the session
/// owns, started without a readiness gate and with completion events on, that either
/// runs (state `starting`, `running` or `restarting`, pid alive when known) and started
/// less than `SERVICE_HOLD_MS` ago, or exited less than `COMPLETION_DELIVERY_MS` ago with
/// its completion still pending. A process with a readiness gate is a server meant to
/// outlive the run and never counts.
pub fn service_wake_pending(transcript: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    // shortcut: omp moves this root to $XDG_STATE_HOME/omp/run/daemons once that omp
    // directory exists, which this host lacks. Read that root if this host migrates.
    service_wake_pending_in(&PathBuf::from(home).join(".omp/run/daemons"), transcript, SystemTime::now())
}

fn service_wake_pending_in(root: &Path, transcript: &Path, now: SystemTime) -> bool {
    let Some(owner) = session_id(transcript) else {
        return false;
    };
    let now = now.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    let read = |path: PathBuf| fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let Ok(scopes) = fs::read_dir(root) else {
        return false;
    };
    scopes
        .flatten()
        .filter_map(|scope| fs::read_dir(scope.path().join("daemons")).ok())
        .flatten()
        .flatten()
        .any(|service| {
            let dir = service.path();
            let Some(meta) = read(dir.join("meta.json")) else {
                return false;
            };
            let daemon = &meta["daemon"];
            if daemon["owner"].as_str() != Some(owner) || meta["completionEvents"].as_bool() != Some(true) {
                return false;
            }
            if read(dir.join("spec.json")).is_none_or(|spec| !spec["ready"].is_null()) {
                return false;
            }
            let since = |key: &str| daemon[key].as_u64().map(|at| now.saturating_sub(at));
            let running = matches!(daemon["state"].as_str(), Some("starting" | "running" | "restarting"))
                && since("startedAt").is_some_and(|age| age < SERVICE_HOLD_MS)
                && daemon["pid"].as_u64().is_none_or(|pid| Path::new("/proc").join(pid.to_string()).exists());
            let undelivered = since("exitedAt").is_some_and(|age| age < COMPLETION_DELIVERY_MS)
                && meta["pendingCompletions"].as_array().is_some_and(|pending| !pending.is_empty());
            running || undelivered
        })
}

/// Whether the transcript's latest goal state is an active goal. Its last `mode_change` has
/// mode `goal` and goal status `active`. omp's TUI then starts a continuation turn 800 ms
/// after each settled turn, unless a gate it keeps only in memory (no progress, composer
/// text) skips it.
pub fn goal_active(transcript: &Path) -> io::Result<bool> {
    let mut active = false;
    scan_jsonl(transcript, |value| {
        if value.get("type").and_then(Value::as_str) == Some("mode_change") {
            active = value.get("mode").and_then(Value::as_str) == Some("goal")
                && value.pointer("/data/goal/status").and_then(Value::as_str) == Some("active");
        }
        true
    })?;
    Ok(active)
}

/// Options the toolResult of `tool_call_id` recorded as selected, if present.
pub fn ask_result(transcript: &std::path::Path, tool_call_id: &str) -> std::io::Result<Option<Vec<String>>> {
    let mut selected = None;
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        if message.get("role").and_then(Value::as_str) == Some("toolResult")
            && message.get("toolCallId").and_then(Value::as_str) == Some(tool_call_id)
        {
            selected = message
                .get("details")
                .and_then(|details| details.get("selectedOptions"))
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                });
        }
        true
    })?;
    Ok(selected)
}

/// Parse omp's Ask dialog on a captured screen. Rows look like `│   ○ Red`,
/// chosen rows `│   ◉ Green`, and the cursor row `│ ❯ ○ Green (Recommended)`;
/// description rows are indented without a marker. A question revisited in the
/// pane still parses because chosen rows count too.
/// Returns (cursor index, total rows incl. Other).
pub fn dialog_cursor(screen: &str) -> Option<(usize, usize)> {
    let mut in_dialog = false;
    let mut closed = false;
    let mut cursor = None;
    let mut total = 0;
    for line in screen.lines() {
        if !in_dialog && line.contains("╭─ Ask") {
            in_dialog = true;
            closed = false;
            cursor = None;
            total = 0;
            continue;
        }
        if !in_dialog {
            continue;
        }
        if line.contains('╰') {
            in_dialog = false;
            closed = true;
            continue;
        }
        if line.contains('○') || line.contains('◉') {
            if line.contains('❯') {
                cursor = Some(total);
            }
            total += 1;
        }
    }
    if closed && total > 0 {
        cursor.map(|cursor| (cursor, total))
    } else {
        None
    }
}

/// Parse omp's Ask review tab on a captured screen. The last `╭─ Ask` box must
/// close with a `╰` line and show `Review answers` before its first `├` divider;
/// numbered rows `│ 1. f1: unanswered` after the divider contribute the text
/// after `<digits>. ` (here `f1: unanswered`). Any other tab parses to None.
pub fn review_answers(screen: &str) -> Option<Vec<String>> {
    let mut in_dialog = false;
    let mut review = false;
    let mut divider = false;
    let mut closed = false;
    let mut rows = Vec::new();
    for line in screen.lines() {
        if line.contains("╭─ Ask") {
            in_dialog = true;
            review = false;
            divider = false;
            closed = false;
            rows.clear();
            continue;
        }
        if !in_dialog {
            continue;
        }
        if line.contains('╰') {
            in_dialog = false;
            closed = true;
            continue;
        }
        if line.contains('├') {
            divider = true;
            continue;
        }
        let content = line
            .strip_prefix('│')
            .map(|rest| rest.trim_end().strip_suffix('│').unwrap_or(rest).trim())
            .unwrap_or_default();
        if !divider {
            review |= content == "Review answers";
        } else if let Some((number, rest)) = content.split_once(". ") {
            if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
                rows.push(rest.to_owned());
            }
        }
    }
    (review && closed).then_some(rows)
}

/// Detect the fullscreen Plan Review header in a captured screen.
pub fn plan_review_open(screen: &str) -> bool {
    screen.lines().any(|line| line.starts_with("╭─ Plan Review"))
}

/// Reads the visible role track and selected index from Plan Review.
pub fn plan_execution_models(screen: &str) -> Option<(Vec<String>, usize)> {
    if !screen.lines().any(|line| line.contains("←/→ model")) {
        return None;
    }
    let line = screen.lines().find(|line| line.contains("continue with"))?;
    let track = line.split_once('◂')?.1.split_once('▸')?.0
        .replace('', "◀")
        .replace('', "▶")
        .replace('', "┆");
    let (before, remaining) = track.split_once('◀')?;
    let (selected, after) = remaining.split_once('▶')?;
    let split_roles = |part: &str| {
        part.split('┆')
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let mut roles = split_roles(before);
    let selected_index = roles.len();
    let selected = selected.trim();
    if selected.is_empty() {
        return None;
    }
    roles.push(selected.to_owned());
    roles.extend(split_roles(after));
    Some((roles, selected_index))
}


/// Detects plan mode from the idle-screen status row only.
pub fn plan_mode_on(screen: &str) -> bool {
    screen.lines().any(|line| line.contains("🗺 Plan"))
}

/// A launch whose omp came up without a model: its TUI printed omp's `No model selected.` error
/// and its status row shows `⬢ no-model`. Both must show, because the status row alone also
/// appears while omp is still binding its model at startup.
pub fn no_model(screen: &str) -> bool {
    screen.lines().any(|line| line.contains("No model selected."))
        && screen.lines().any(|line| line.contains("⬢ no-model"))
}

/// Whether omp's own log for `pid` records the `SQLITE_BUSY` startup crash. omp writes each
/// run to `~/.omp/logs/omp.<date>.<pid>.log`, so the pane's pid names the file without the
/// date. A crash there reads `"code":"SQLITE_BUSY"` with `database is locked` as its message.
pub fn startup_busy(pid: u32) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    busy_log(&PathBuf::from(home).join(".omp/logs"), pid)
}

/// `startup_busy` over an explicit log directory.
pub fn busy_log(dir: &Path, pid: u32) -> bool {
    let suffix = format!(".{pid}.log");
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        if !name.starts_with("omp.") || !name.ends_with(&suffix) {
            return false;
        }
        fs::read_to_string(entry.path()).is_ok_and(|log| {
            log.contains("SQLITE_BUSY") || log.contains("database is locked")
        })
    })
}

/// Line indexes of the last two lines containing `───`, the rules around omp's editor.
fn editor_rules(screen: &str) -> Option<(usize, usize)> {
    let mut previous = None;
    let mut last = None;
    for (index, line) in screen.lines().enumerate() {
        if line.contains("───") {
            previous = last;
            last = Some(index);
        }
    }
    Some((previous?, last?))
}

/// True when omp's editor (between the last two `───` rules) holds nothing but hints.
pub fn editor_empty(screen: &str) -> bool {
    let Some((start, end)) = editor_rules(screen) else {
        return false;
    };
    screen
        .lines()
        .skip(start + 1)
        .take(end.saturating_sub(start + 1))
        .all(|line| line.split("←←").all(|part| part.trim().is_empty()))
}

/// True when omp's working row, the last non-blank line above the editor's top
/// rule, shows a background-job wait or an auto-retry, so the run has not stopped.
pub fn run_paused(screen: &str) -> bool {
    let Some((top, _)) = editor_rules(screen) else {
        return false;
    };
    let Some(row) = screen
        .lines()
        .take(top)
        .filter(|line| !line.trim().is_empty())
        .last()
    else {
        return false;
    };
    // Drop the `⎋` hint and any spinner frame before the label.
    let row = row
        .split_whitespace()
        .skip_while(|token| !token.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>()
        .join(" ");
    let counted_jobs = row.strip_prefix("Waiting for ").is_some_and(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with(" background jobs: ")
    });
    counted_jobs
        || [
            "Waiting for background work",
            "Waiting for background job ",
            "Retrying (",
            "Retrying · attempt ",
        ]
        .iter()
        .any(|prefix| row.starts_with(prefix))
}

/// Replace common secret shapes (AWS keys, bearer tokens, long hex/base64 runs) with `…`.
/// For a captured screen or a plan file, where paths and hashes matter less than a leak.
pub fn redact(text: &str) -> String {
    redact_with(text, true)
}

/// Replace only unambiguous credentials, AWS access keys and `Bearer` tokens, with `…`.
/// For card text that the user reads as a report, where a masked path or commit hash would
/// cost more than the long-run rule catches.
pub fn redact_credentials(text: &str) -> String {
    redact_with(text, false)
}

fn redact_with(text: &str, long_runs: bool) -> String {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if text[index..].starts_with("Bearer ") {
            let start = index + "Bearer ".len();
            let mut end = start;
            while end < bytes.len() && bearer_byte(bytes[end]) {
                end += 1;
            }
            if end > start {
                output.push_str("Bearer …");
                index = end;
                continue;
            }
        }

        if long_runs && secret_byte(bytes[index]) {
            let mut end = index;
            while end < bytes.len() && secret_byte(bytes[end]) {
                end += 1;
            }
            if end - index >= 32 {
                output.push('…');
                index = end;
                continue;
            }
        }

        if bytes.get(index..index + 4) == Some(b"AKIA".as_slice())
            && bytes.get(index + 4..index + 20).is_some_and(|suffix| {
                suffix.len() == 16
                    && suffix
                        .iter()
                        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            })
        {
            output.push('…');
            index += 20;
            continue;
        }

        let character = text[index..].chars().next().unwrap();
        output.push(character);
        index += character.len_utf8();
    }
    output
}

/// A reply that opens with a status line such as `ENVOY_STATUS: complete` is the
/// agent's final report. `complete` means the work was delivered and its full gate passed;
/// `agent-done` means the work finished with a step still open; `incomplete` means
/// the run could not do what its brief named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalStatus {
    Complete,
    AgentDone,
    Incomplete,
}

/// Splits a final report into its status and body. The first non-empty line must be a
/// `*_STATUS: complete|agent-done|incomplete` line. Markdown emphasis, code ticks or a
/// heading mark around it still count, because omp renders them away and the pane shows
/// a plain status line. Text after the status word on the same line starts the body.
pub fn final_report(text: &str) -> Option<(FinalStatus, &str)> {
    let markup = |c: char| c.is_whitespace() || matches!(c, '*' | '_' | '`' | '#');
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    let (key, value) = line.split_once(':')?;
    let key = key.trim_matches(markup);
    if !key.ends_with("_STATUS")
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    let value = value.trim_start_matches(markup);
    let word = &value[..value
        .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
        .unwrap_or(value.len())];
    let status = match word {
        "complete" => FinalStatus::Complete,
        "agent-done" => FinalStatus::AgentDone,
        "incomplete" => FinalStatus::Incomplete,
        _ => return None,
    };
    let rest = value[word.len()..]
        .trim_start_matches(|c: char| markup(c) || matches!(c, '.' | ',' | ';' | ':' | '-' | '–' | '—'));
    let start = text.find(line).unwrap_or_default() + line.len() - rest.len();
    Some((status, text[start..].trim()))
}

/// Splits the last labelled `Recommended action` paragraph from a report. List, quote,
/// heading and emphasis marks before the label still count. A bare label takes the next
/// paragraph. Returns the report without the label and paragraph, then the step; an empty
/// step means the label had no content or said `none`. Returns `None` when no valid label
/// exists.
pub fn recommended_action(report: &str) -> Option<(String, String)> {
    const LABEL: &str = "recommended action";
    let lead = |c: char| c.is_whitespace() || matches!(c, '*' | '_' | '#' | '>' | '-');
    let emphasis = |c: char| c.is_whitespace() || matches!(c, '*' | '_');
    let lines: Vec<&str> = report.lines().collect();
    let (start, inline) = lines.iter().enumerate().rev().find_map(|(index, line)| {
        let label = line.trim_start_matches(lead);
        if !label.get(..LABEL.len())?.eq_ignore_ascii_case(LABEL) {
            return None;
        }
        let rest = label[LABEL.len()..].trim_start_matches(emphasis);
        match rest.strip_prefix(':') {
            Some(text) => Some((index, text.trim_start_matches(emphasis).trim_end())),
            None if rest.is_empty() => Some((index, "")),
            None => None,
        }
    })?;
    let first = if inline.is_empty() {
        (start + 1..lines.len()).find(|&index| !lines[index].trim().is_empty()).unwrap_or(lines.len())
    } else {
        start + 1
    };
    let end = (first..lines.len())
        .find(|&index| lines[index].trim().is_empty())
        .unwrap_or(lines.len());
    let step = std::iter::once(inline)
        .chain(lines[first..end].iter().copied())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let word: String = step
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .chars()
        .take_while(|c| c.is_alphanumeric())
        .collect();
    let step = if step.trim().is_empty() || word.eq_ignore_ascii_case("none") {
        String::new()
    } else {
        step.trim().to_owned()
    };
    let before = lines[..start].join("\n");
    let after = lines[end..].join("\n");
    let rest = match (before.trim(), after.trim()) {
        (before, "") => before.to_owned(),
        ("", after) => after.to_owned(),
        (before, after) => format!("{before}\n\n{after}"),
    };
    Some((rest, step))
}

/// One-pass audit of a transcript whose omp process is gone: was the session
/// dispatched from a triage brief, and did its last reply carry a final report?
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DispatchAudit {
    pub dispatched: bool,
    pub reported: bool,
    pub entry_id: Option<String>,
    pub last_text: Option<String>,
}

/// A session is dispatched from triage when a user message carries a brief path under
/// `triage/briefs/`, or when a developer message starts with `Plan approved.` and carries
/// the `ENVOY_STATUS` contract (an approved triage plan running in a fresh session).
/// `reported` means some assistant reply after the latest such message opens with a
/// `*_STATUS` line (see [`final_report`]). `entry_id` and `last_text` are the last reply.
pub fn dispatch_audit(transcript: &Path) -> io::Result<DispatchAudit> {
    let mut audit = DispatchAudit::default();
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        let dispatch = match message.get("role").and_then(Value::as_str) {
            Some("user") => text_blocks(message).is_some_and(|text| text.contains("triage/briefs/")),
            Some("developer") => text_blocks(message).is_some_and(|text| {
                text.starts_with("Plan approved.") && text.contains("ENVOY_STATUS")
            }),
            Some("assistant") => {
                if let (Some(id), Some(text)) = (
                    value.get("id").and_then(Value::as_str),
                    text_blocks(message),
                ) {
                    if final_report(&text).is_some() {
                        audit.reported = true;
                    }
                    audit.entry_id = Some(id.to_owned());
                    audit.last_text = Some(text);
                }
                false
            }
            _ => false,
        };
        if dispatch {
            audit.dispatched = true;
            audit.reported = false;
        }
        true
    })?;
    Ok(audit)
}

fn scan_jsonl(path: &Path, mut visit: impl FnMut(Value) -> bool) -> io::Result<()> {
    let reader = BufReader::new(File::open(path)?);
    for line in reader.lines() {
        let line = line?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !visit(value) {
            break;
        }
    }
    Ok(())
}

fn message(value: &Value) -> Option<&Value> {
    if value.get("type")?.as_str()? != "message" {
        return None;
    }
    value.get("message")
}

fn text_blocks(message: &Value) -> Option<String> {
    let blocks = message.get("content")?.as_array()?;
    let texts = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    (!texts.is_empty()).then(|| texts.join("\n\n"))
}

fn secret_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn bearer_byte(byte: u8) -> bool {
    secret_byte(byte) || matches!(byte, b'.' | b'~')
}


#[cfg(test)]
mod tests {
    use super::*;

    fn transcript() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript_ask.jsonl")
    }

    #[test]
    fn busy_log_names_the_pid_and_the_crash() {
        let dir = std::env::temp_dir().join(format!("triage-omp-logs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let crash = r#"{"level":"error","pid":4242,"message":"Fatal error","err":{"name":"SQLiteError","message":"Database \"/home/user/.omp/agent/agent.db\": database is locked","code":"SQLITE_BUSY"}}"#;
        fs::write(dir.join("omp.2026-11-09.4242.log"), crash).unwrap();
        fs::write(dir.join("omp.2026-11-09.7.log"), r#"{"level":"warn"}"#).unwrap();
        fs::write(dir.join("other.4242.log"), crash).unwrap();

        assert!(busy_log(&dir, 4242));
        assert!(!busy_log(&dir, 7), "a log without the crash is not a startup death");
        assert!(!busy_log(&dir, 8), "another pid's log says nothing about this run");
        assert!(!busy_log(&dir.join("missing"), 4242));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn classifies_omp_title_prefixes() {
        assert_eq!(classify("π ⠋ 🧑‍💼 HR: Onboard…"), PaneState::Working);
        assert_eq!(classify("π > waiting"), PaneState::Waiting);
        assert_eq!(classify("π ! asking"), PaneState::Asking);
        assert_eq!(classify("zsh"), PaneState::Other);
    }

    #[test]
    fn parses_plan_execution_models_in_display_order() {
        assert_eq!(
            plan_execution_models(include_str!("../tests/fixtures/plan_review_overlay.txt")),
            Some((vec!["smol", "default", "slow"].into_iter().map(str::to_owned).collect(), 1))
        );
        assert_eq!(
            plan_execution_models("continue with ◂ smol ┆ default ◀ slow ▶ plan ┆ task ▸\n↑/↓ select · ⏎ confirm · ←/→ model"),
            Some((vec!["smol", "default", "slow", "plan", "task"].into_iter().map(str::to_owned).collect(), 2))
        );
        assert_eq!(
            plan_execution_models("continue with ◂ smol \u{e0b1} default \u{e0b2}slow\u{e0b0} plan ▸\n↑/↓ select · ⏎ confirm · ←/→ model"),
            Some((vec!["smol", "default", "slow", "plan"].into_iter().map(str::to_owned).collect(), 2))
        );
        assert_eq!(plan_execution_models("continue with ◂ smol ◀ default ▶ slow ▸"), None);
        assert_eq!(plan_execution_models("continue with ◂ smol ◀ default ▶ slow ▸\n↑/↓ scroll · ⏎ open"), None);
    }

    #[test]
    fn parses_ask_dialog_cursor() {
        assert_eq!(dialog_cursor(include_str!("../tests/fixtures/ask_dialog.txt")), Some((1, 4)));
    }

    #[test]
    fn parses_multi_question_dialog_cursor() {
        assert_eq!(
            dialog_cursor(include_str!("../tests/fixtures/ask_dialog_multi.txt")),
            Some((0, 3))
        );
    }

    #[test]
    fn dialog_cursor_counts_chosen_rows() {
        assert_eq!(dialog_cursor("╭─ Ask ─╮\n│ ❯ ◉ Green\n╰─\n"), Some((0, 1)));
    }

    #[test]
    fn review_answers_parses_answered_review() {
        assert_eq!(
            review_answers(include_str!("../tests/fixtures/ask_dialog_multi_review.txt")).unwrap(),
            ["f1: Formal (Recommended)", "f2: Yearly", "color: “Violet with spaces”"]
        );
    }

    #[test]
    fn review_answers_parses_unanswered_review() {
        assert_eq!(
            review_answers(include_str!("../tests/fixtures/ask_dialog_multi_review_empty.txt")).unwrap(),
            ["f1: unanswered", "f2: unanswered", "color: unanswered"]
        );
    }

    #[test]
    fn review_answers_rejects_question_tab() {
        assert!(review_answers(include_str!("../tests/fixtures/ask_dialog_multi.txt")).is_none());
    }

    #[test]
    fn detects_plan_review_overlay() {
        assert!(plan_review_open(include_str!("../tests/fixtures/plan_review_overlay.txt")));
        assert!(!plan_review_open(include_str!("../tests/fixtures/plan_mode_idle.txt")));
        assert!(!plan_review_open(include_str!("../tests/fixtures/ask_dialog.txt")));
    }

    #[test]
    fn detects_plan_mode_status() {
        assert!(plan_mode_on(include_str!("../tests/fixtures/plan_mode_idle.txt")));
        assert!(!plan_mode_on(include_str!("../tests/fixtures/ask_dialog.txt")));
        assert!(!plan_mode_on(include_str!("../tests/fixtures/plan_review_overlay.txt")));
        assert!(plan_mode_on("╭─ Plan Review ─╮\n⬢ Example Model · 🗺 Plan · ~/project"));
    }

    #[test]
    fn detects_no_model_launch() {
        assert!(no_model(include_str!("../tests/fixtures/no_model.txt")));
        assert!(!no_model(include_str!("../tests/fixtures/plan_mode_idle.txt")));
        assert!(!no_model(include_str!("../tests/fixtures/ask_dialog.txt")));
        assert!(!no_model("⬢ no-model · 🗑 ~/tmp\n"));
    }

    #[test]
    fn detects_paused_run_from_working_row() {
        assert!(run_paused(include_str!("../tests/fixtures/async_wait.txt")));
        assert!(run_paused("  ⎋ Retrying (1/3) in 4s…\n\n───\n\n───\n status"));
        assert!(run_paused("  ⎋ ⠋ Waiting for 2 background jobs: bg_1, bg_2\n───\n───\n"));
        assert!(!run_paused(include_str!("../tests/fixtures/plan_mode_idle.txt")));
        assert!(!run_paused(include_str!("../tests/fixtures/ask_dialog.txt")));
        assert!(!run_paused(include_str!("../tests/fixtures/plan_review_overlay.txt")));
        assert!(!run_paused(" Retrying the deploy fixed it.\n\n───\n\n───\n status"));
        assert!(!run_paused("  ⎋ Waiting for background job bg_1\n\n Done.\n───\n\n───\n status"));
        assert!(!run_paused("  ⎋ Waiting for background job bg_1\n"));
    }

    fn turn_end_of(lines: &[&str]) -> Option<TurnEnd> {
        let path = std::env::temp_dir().join(format!(
            "triage-omp-turn-end-{}-{}.jsonl",
            std::process::id(),
            lines.len()
        ));
        fs::write(&path, lines.join("\n")).unwrap();
        let end = turn_end(&path).unwrap();
        fs::remove_file(path).unwrap();
        end
    }

    #[test]
    fn turn_end_follows_omp_settled_status() {
        let user = r#"{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#;
        let report = r#"{"type":"message","id":"a1","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"ENVOY_STATUS: complete\n\nShipped."}]}}"#;
        let wake = r#"{"type":"custom_message","id":"c1","customType":"async-result","content":"done"}"#;
        let exit = r#"{"type":"custom_message","id":"c2","customType":"launch-completion","content":"Supervised process build exited with exit code 0."}"#;
        let reply = r#"{"type":"message","id":"a2","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"Background job finished."}]}}"#;
        let tool_use = r#"{"type":"message","id":"t1","message":{"role":"assistant","stopReason":"toolUse","content":[{"type":"text","text":"Dumping rows now."}]}}"#;
        let length = r#"{"type":"message","id":"l1","message":{"role":"assistant","stopReason":"length","content":[{"type":"text","text":"Cut off"}]}}"#;
        let aborted = r#"{"type":"message","id":"x1","message":{"role":"assistant","stopReason":"aborted","errorMessage":"Interrupted by user","content":[]}}"#;
        let error = r#"{"type":"message","id":"e1","message":{"role":"assistant","stopReason":"error","errorMessage":"Bad request","content":[]}}"#;
        let blank_stop = r#"{"type":"message","id":"b1","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"  \n"}]}}"#;
        let no_reason = r#"{"type":"message","id":"n1","message":{"role":"assistant","content":[{"type":"text","text":"Hello"}]}}"#;

        assert_eq!(turn_end_of(&[user]), None);
        assert_eq!(
            turn_end_of(&[user, report, wake, reply]),
            Some(TurnEnd::Reply {
                entry_id: "a2".into(),
                text: "Background job finished.".into(),
                report: Some("ENVOY_STATUS: complete\n\nShipped.".into()),
            })
        );
        assert_eq!(turn_end_of(&[user, report, wake]), Some(TurnEnd::Running));
        assert_eq!(turn_end_of(&[user, report, exit]), Some(TurnEnd::Running));
        assert_eq!(
            turn_end_of(&[report, wake, user, reply]),
            Some(TurnEnd::Reply {
                entry_id: "a2".into(),
                text: "Background job finished.".into(),
                report: None,
            })
        );
        assert_eq!(
            turn_end_of(&[user, report]),
            Some(TurnEnd::Reply {
                entry_id: "a1".into(),
                text: "ENVOY_STATUS: complete\n\nShipped.".into(),
                report: None,
            })
        );
        assert_eq!(
            turn_end_of(&[report, user, reply]),
            Some(TurnEnd::Reply {
                entry_id: "a2".into(),
                text: "Background job finished.".into(),
                report: None,
            })
        );
        assert_eq!(
            turn_end_of(&[user, length]),
            Some(TurnEnd::Reply { entry_id: "l1".into(), text: "Cut off".into(), report: None })
        );
        assert_eq!(
            turn_end_of(&[user, tool_use, error]),
            Some(TurnEnd::Error { entry_id: "e1".into(), message: "Bad request".into() })
        );
        assert_eq!(turn_end_of(&[user, tool_use, aborted]), Some(TurnEnd::Aborted));
        assert_eq!(turn_end_of(&[user, tool_use]), Some(TurnEnd::Running));
        assert_eq!(turn_end_of(&[user, blank_stop]), Some(TurnEnd::Running));
        assert_eq!(turn_end_of(&[user, no_reason]), Some(TurnEnd::Running));
    }

    #[test]
    fn resume_flags_name_the_breadcrumb_session() {
        let session = Path::new(
            "/sessions/-proj/2026-10-29T23-31-18-428Z_01a18a00-eb1c-7530-91cb-d6fd8ac4075d.jsonl",
        );
        let session_str = session.to_str().unwrap();
        let argv = |flags: &[&str]| {
            std::iter::once("omp")
                .chain(flags.iter().copied())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };

        assert!(resumes(
            &argv(&["--resume", session_str, "--profile", "default"]),
            session
        ));
        assert!(resumes(&argv(&[&format!("--resume={session_str}")]), session));
        assert!(resumes(&argv(&["-r", "01a18a00"]), session));
        assert!(resumes(&argv(&["--restore", session_str]), session));

        assert!(!resumes(&argv(&[]), session));
        assert!(!resumes(&argv(&["--model", "k3"]), session));
        assert!(!resumes(&argv(&["-c"]), session));
        assert!(!resumes(&argv(&["--continue"]), session));
        assert!(!resumes(&argv(&["--resume", "/other.jsonl"]), session));
        assert!(!resumes(&argv(&["-r"]), session));
        assert!(!resumes(&argv(&["--resume", "--profile", "default"]), session));
        assert!(!resumes(&argv(&["-r", "01a19"]), session));
        assert!(!resumes(&argv(&["--resume="]), session));
    }

    #[test]
    fn dialog_cursor_returns_latest_ask_panel() {
        let screen = concat!(
            "╭─ Ask ─╮\n",
            "│   ○ First\n",
            "│ ❯ ○ Selected\n",
            "╰─\n",
            "╭─ Ask ─╮\n",
            "│ ❯ ○ Latest\n",
            "│   ○ Other (type your own)\n",
            "╰─\n",
        );
        assert_eq!(dialog_cursor(screen), Some((0, 2)));
    }

    #[test]
    fn dialog_cursor_ignores_ask_marker_inside_dialog() {
        let screen = concat!(
            "╭─ Ask ─╮\n",
            "│   ○ First\n",
            "│ prompt text includes ╭─ Ask as ordinary text\n",
            "│ ❯ ○ Second\n",
            "│   ○ Other (type your own)\n",
            "╰─\n",
        );
        assert_eq!(dialog_cursor(screen), Some((1, 3)));
    }

    #[test]
    fn finds_pending_ask_and_completed_result() {
        let prefix = include_str!("../tests/fixtures/transcript_ask.jsonl")
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        let pending = std::env::temp_dir().join(format!("triage-omp-pending-{}.jsonl", std::process::id()));
        fs::write(&pending, prefix).unwrap();
        let call = pending_ask(&pending).unwrap().unwrap();
        assert_eq!(call.questions.len(), 1);
        assert_eq!(call.questions[0].options.len(), 3);
        assert_eq!(call.questions[0].recommended, Some(1));
        fs::remove_file(pending).unwrap();

        assert!(pending_ask(&transcript()).unwrap().is_none());
    }

    #[test]
    fn parses_plan_proposal_from_transcript() {
        let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcript_propose.jsonl");
        let proposals = proposes(&transcript).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "planverify-proof");
        assert!(proposals[0]
            .plan_path
            .ends_with("local/planverify-proof-plan.md"));
    }

    #[test]
    fn includes_in_flight_plan_proposal() {
        let path = std::env::temp_dir()
            .join(format!("triage-omp-propose-pending-{}.jsonl", std::process::id()));
        fs::write(
            &path,
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"pending","name":"write","arguments":{"path":"xd://propose"}}]}}"#,
        )
        .unwrap();

        let proposals = proposes(&path).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "");
        assert!(proposals[0].plan_path.as_os_str().is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn falls_back_to_write_content_when_proposal_title_is_missing() {
        let path = std::env::temp_dir()
            .join(format!("triage-omp-propose-fallback-{}.jsonl", std::process::id()));
        let transcript = concat!(
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"call","name":"write","arguments":{"path":"xd://propose","content":"fallback plan title"}}]}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"toolResult","toolCallId":"call","details":{"xdev":{"inner":{"planFilePath":"local://fallback-plan.md"}}}}}"#,
        );
        fs::write(&path, transcript).unwrap();

        let proposals = proposes(&path).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "fallback plan title");
        assert!(proposals[0].plan_path.ends_with("local/fallback-plan.md"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn reads_ask_result_and_assistant_text() {
        assert_eq!(ask_result(&transcript(), "call_ask_1").unwrap(), Some(vec!["Blue".to_owned()]));
        assert_eq!(last_assistant_text(&transcript()).unwrap().unwrap().1, "Blue");
    }

    #[test]
    fn checks_empty_editor_between_last_rules() {
        assert!(editor_empty("──────\n  ←←  \n──────"));
        assert!(!editor_empty("──────\ntext\n──────"));
        assert!(!editor_empty("no rules"));
    }

    #[test]
    fn redacts_secret_shapes() {
        let key = format!("{}{}", "AKIA", "1234567890ABCDEF");
        assert_eq!(redact(&format!("key {key}")), "key …");
        assert_eq!(redact("Bearer short-token"), "Bearer …");
        assert_eq!(redact(&"a".repeat(32)), "…");
    }

    #[test]
    fn final_report_needs_a_status_line() {
        assert_eq!(
            final_report("\nENVOY_STATUS: complete\nShipped the fix.\nTests pass."),
            Some((FinalStatus::Complete, "Shipped the fix.\nTests pass."))
        );
        assert_eq!(
            final_report("ENVOY_STATUS: agent-done\n\nPush still open."),
            Some((FinalStatus::AgentDone, "Push still open."))
        );
        assert_eq!(
            final_report("TRIAGE_STATUS: incomplete\nBlocked on access."),
            Some((FinalStatus::Incomplete, "Blocked on access."))
        );
        assert_eq!(final_report("ENVOY_STATUS: maybe\nx"), None);
        assert_eq!(final_report("Waiting on the build job."), None);
        assert_eq!(final_report("Note: ENVOY_STATUS: complete"), None);
    }

    #[test]
    fn final_report_accepts_status_without_report_body() {
        assert_eq!(
            final_report("ENVOY_STATUS: complete"),
            Some((FinalStatus::Complete, ""))
        );
        assert_eq!(
            final_report("TRIAGE_STATUS: incomplete\n \n"),
            Some((FinalStatus::Incomplete, ""))
        );
    }

    #[test]
    fn final_report_reads_markdown_wrapped_and_run_on_status_lines() {
        assert_eq!(
            final_report("**ENVOY_STATUS: complete**\n\nDraft created: \"Weekly fixes\" for the caller."),
            Some((FinalStatus::Complete, "Draft created: \"Weekly fixes\" for the caller."))
        );
        assert_eq!(
            final_report("`ENVOY_STATUS: agent-done`\nPush still open."),
            Some((FinalStatus::AgentDone, "Push still open."))
        );
        assert_eq!(
            final_report("**ENVOY_STATUS:** incomplete\nBlocked on access."),
            Some((FinalStatus::Incomplete, "Blocked on access."))
        );
        assert_eq!(
            final_report("ENVOY_STATUS: agent-done — open items are the push\nDetails."),
            Some((FinalStatus::AgentDone, "open items are the push\nDetails."))
        );
        assert_eq!(
            final_report("ENVOY_STATUS: complete. I wrote the line."),
            Some((FinalStatus::Complete, "I wrote the line."))
        );
        assert_eq!(final_report("**ENVOY_STATUS: complete**"), Some((FinalStatus::Complete, "")));
        assert_eq!(
            final_report("Both are finished (last reply `ENVOY_STATUS: complete`) — done."),
            None
        );
        assert_eq!(final_report("ENVOY_STATUS: completed\nx"), None);
    }

    #[test]
    fn recommended_action_splits_the_last_labelled_step_from_the_report() {
        assert_eq!(
            recommended_action(
                "Both lack the grant.\n\n**Recommended action:** Add both as readers.\n\nTime used: 9 seconds."
            ),
            Some((
                "Both lack the grant.\n\nTime used: 9 seconds.".to_owned(),
                "Add both as readers.".to_owned()
            ))
        );
        assert_eq!(
            recommended_action(
                "Found it.\n\n## Recommended action\n\nRotate the key\nthen restart the unit."
            ),
            Some(("Found it.".to_owned(), "Rotate the key\nthen restart the unit.".to_owned()))
        );
        assert_eq!(
            recommended_action("Nothing is wrong.\n\nRecommended action: none."),
            Some(("Nothing is wrong.".to_owned(), String::new()))
        );
        assert_eq!(
            recommended_action("Found it.\n\nRecommended action:"),
            Some(("Found it.".to_owned(), String::new()))
        );
        assert_eq!(recommended_action("Recommended actions differ by host."), None);
        assert_eq!(recommended_action("No step named."), None);
    }

    #[test]
    fn audit_detects_dispatch_and_report_state() {
        let path = std::env::temp_dir().join(format!("triage-omp-audit-{}.jsonl", std::process::id()));
        let brief = r#"{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"<file name=\"/home/x/.local/share/triage/briefs/triage-brief-a.md\">\ncarry it out"}]}}"#;
        let unreported = r#"{"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"text","text":"Work done, details inside."}]}}"#;
        let reported = r#"{"type":"message","id":"a2","message":{"role":"assistant","content":[{"type":"text","text":"ENVOY_STATUS: complete\n\nAll done."}]}}"#;

        fs::write(&path, format!("{brief}\n{unreported}\n")).unwrap();
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && !audit.reported);
        assert_eq!(audit.entry_id.as_deref(), Some("a1"));
        assert_eq!(audit.last_text.as_deref(), Some("Work done, details inside."));

        fs::write(&path, format!("{brief}\n{reported}\n")).unwrap();
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && audit.reported);

        fs::write(&path, format!("{unreported}\n")).unwrap();
        let audit = dispatch_audit(&path).unwrap();
        assert!(!audit.dispatched && !audit.reported);

        fs::write(&path, "\n").unwrap();
        assert_eq!(dispatch_audit(&path).unwrap(), DispatchAudit::default());

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn dispatch_audit_counts_plan_executions_and_reports_since_the_brief() {
        let path = std::env::temp_dir().join(format!("triage-omp-audit-plan-{}.jsonl", std::process::id()));
        let line = |id: &str, role: &str, text: &str| {
            serde_json::json!({"type": "message", "id": id, "message": {"role": role, "content": [{"type": "text", "text": text}]}}).to_string()
        };
        let write = |lines: &[String]| fs::write(&path, lines.join("\n") + "\n").unwrap();
        let brief = line("u1", "user", "Read /home/x/.local/share/triage/briefs/a.md and do it.");
        let report = line("a1", "assistant", "ENVOY_STATUS: complete\n\nDone.");

        write(&[
            line("d1", "developer", "Plan approved.\n\nEnd with ENVOY_STATUS: complete."),
            line("a1", "assistant", "Working on it."),
        ]);
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && !audit.reported);

        write(&[
            line("d1", "developer", "Plan approved.\n\nEnd with a report."),
            line("a1", "assistant", "Working on it."),
        ]);
        assert!(!dispatch_audit(&path).unwrap().dispatched);

        write(&[
            brief.clone(),
            report.clone(),
            line("u2", "user", "thanks"),
            line("a2", "assistant", "You're welcome."),
        ]);
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && audit.reported);
        assert_eq!(audit.last_text.as_deref(), Some("You're welcome."));

        write(&[brief.clone(), report, brief, line("a2", "assistant", "Started again.")]);
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && !audit.reported);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn service_wake_pending_reads_owned_job_processes() {
        let root = std::env::temp_dir().join(format!("triage-omp-services-{}", std::process::id()));
        let dir = root.join("0000000000000000/daemons/build");
        fs::create_dir_all(&dir).unwrap();
        let transcript = Path::new("/sessions/-tmp/2026-11-07T16-18-24-308Z_01a1b6cd-test.jsonl");
        let now = SystemTime::now();
        let ms = now.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let (pid, started) = (std::process::id(), ms - 60_000);
        let check = |spec: &Value, meta: &Value| {
            fs::write(dir.join("spec.json"), spec.to_string()).unwrap();
            fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
            service_wake_pending_in(&root, transcript, now)
        };
        let spec = serde_json::json!({"name": "build"});
        let base = serde_json::json!({
            "daemon": {"name": "build", "state": "running", "owner": "01a1b6cd-test", "pid": pid, "startedAt": started},
            "completionEvents": true,
            "pendingCompletions": []
        });
        assert!(check(&spec, &base));

        let ready = serde_json::json!({"name": "build", "ready": {"port": 8080}});
        assert!(!check(&ready, &base));
        let mut meta = base.clone();
        meta["daemon"]["owner"] = serde_json::json!("someone-else");
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["daemon"]["pid"] = serde_json::json!(999_999_999);
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["daemon"]["startedAt"] = serde_json::json!(ms - 3 * 60 * 60 * 1000);
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["completionEvents"] = serde_json::json!(false);
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["daemon"]["state"] = serde_json::json!("stopping");
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["daemon"]["state"] = serde_json::json!("starting");
        meta["daemon"].as_object_mut().unwrap().remove("pid");
        assert!(check(&spec, &meta));

        let mut exited = base.clone();
        exited["daemon"]["state"] = serde_json::json!("exited");
        exited["daemon"].as_object_mut().unwrap().remove("pid");
        exited["daemon"]["exitedAt"] = serde_json::json!(ms - 1000);
        exited["pendingCompletions"] = serde_json::json!([{"name": "build"}]);
        assert!(check(&spec, &exited));
        exited["daemon"]["exitedAt"] = serde_json::json!(ms - 120_000);
        assert!(!check(&spec, &exited));
        exited["daemon"]["exitedAt"] = serde_json::json!(ms - 1000);
        exited["pendingCompletions"] = serde_json::json!([]);
        assert!(!check(&spec, &exited));

        fs::remove_dir_all(&root).unwrap();
    }

    fn goal_active_of(lines: &[&str]) -> bool {
        let path = std::env::temp_dir().join(format!("triage-omp-goal-{}-{}.jsonl", std::process::id(), lines.len()));
        fs::write(&path, lines.join("\n")).unwrap();
        let active = goal_active(&path).unwrap();
        fs::remove_file(path).unwrap();
        active
    }

    #[test]
    fn goal_active_reads_latest_mode_change() {
        let user = r#"{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#;
        let active = r#"{"type":"mode_change","id":"m1","mode":"goal","data":{"goal":{"id":"g1","status":"active"}}}"#;
        let complete = r#"{"type":"mode_change","id":"m1","mode":"goal","data":{"goal":{"id":"g1","status":"complete"}}}"#;
        let none = r#"{"type":"mode_change","id":"m2","mode":"none"}"#;
        let paused = r#"{"type":"mode_change","id":"m3","mode":"goal_paused","data":{"goal":{"id":"g1","status":"paused"}}}"#;
        let budget = r#"{"type":"mode_change","id":"m4","mode":"goal","data":{"goal":{"id":"g1","status":"budget-limited"}}}"#;

        assert!(goal_active_of(&[active]));
        assert!(!goal_active_of(&[active, complete]));
        assert!(!goal_active_of(&[active, none]));
        assert!(!goal_active_of(&[paused]));
        assert!(!goal_active_of(&[budget]));
        assert!(!goal_active_of(&[user]));
    }
}
