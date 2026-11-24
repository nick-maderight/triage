//! Triage's ompsdk sessions. The ompsdk daemon runs each one as an omp session in its triage pool.
//! Triage starts them, answers their questions and plan reviews, turns their replies into
//! cards, and keeps what the poll loop needs in `ompsdk-sessions.json` beside the database.

use super::{
    DEFAULT_SESSION, Launch, Start, TickReport, Tracker, brief_heading, dispatch_prompt, finish_cards,
    record_swipe, reply_turn, report_request, shell_quote, supersede_info_cards, trail, turn_card,
};
use crate::db::Db;
use crate::events::{self, Answer, AnswerReq, Card, CardOption, CardQuestion, CardStatus, Kind, Source, Star};
use crate::omp;
use crate::ompsdk::{self, Client, Elicitation, Turn};
use crate::tmux;
use crate::uploads::Upload;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Continuation prompts a reply without a status line earns before triage reports it anyway.
const MAX_CONTINUES: u32 = 5;
/// Polls a turn may read as running before its session counts as cut.
const RUNNING_TICKS: u32 = 30;
/// Gap before a session whose resume failed is tried again, in ms.
const RESUME_RETRY_MS: i64 = 30_000;
/// Gap before a failed connect or state read is tried again, in ms.
const RETRY_MS: i64 = 30_000;
/// The two answers a plan review takes, as `acp-agent.ts` spells them.
const PLAN_APPROVE: &str = "Approve and execute";
const PLAN_REFINE: &str = "Refine plan";
/// The prompt that resumes a session cut mid-turn.
const CUT_PROMPT: &str = "Continue where you stopped.";

/// What the ompsdk poll remembers between polls.
#[derive(Default)]
pub(super) struct Memo {
    /// Connection id of the last poll. A new id means a reconnect, which resumes every held session.
    connection: Option<u64>,
    retry_at_ms: i64,
    failing: bool,
    /// Polls each session's turn has read as still running.
    running: HashMap<String, u32>,
    /// Earliest time a cut session's resume is tried again, by session id.
    resume_at_ms: HashMap<String, i64>,
}

impl Tracker {
    /// Drive the ompsdk sessions triage holds and return the number of cards written.
    pub fn ompsdk_tick(&mut self, db: &Db, launch: &Launch) -> TickReport {
        TickReport {
            written: tick(&mut self.ompsdk, db, launch, events::now_ms()),
        }
    }
}

/// One tracked ompsdk session, as `ompsdk-sessions.json` keeps it.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Entry {
    sid: String,
    cwd: String,
    /// The brief path the session was launched with. A continuation names it.
    brief: String,
    title: String,
    /// What a plan-into launch approves its plan with.
    plan_into: Option<PlanInto>,
    /// Whether the first plan of a plan-into launch was approved without a card.
    auto_approved: bool,
    /// Continuation prompts sent since the session last got a brief.
    continues: u32,
    /// Whether the daemon holds the session for triage.
    loaded: bool,
    /// Whether a turn stalled, so the next poll resumes the session and nudges it.
    cut: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct PlanInto {
    model: Option<String>,
    thinking: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    sessions: Vec<Entry>,
}

/// Serializes the read-modify-write of the state file across the poll and the card actions.
static STATE_LOCK: Mutex<()> = Mutex::new(());

fn state_path(db: &Db) -> PathBuf {
    db.path().with_file_name("ompsdk-sessions.json")
}

fn load(db: &Db) -> Result<State, String> {
    match fs::read(state_path(db)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| format!("ompsdk state: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(error) => Err(format!("ompsdk state: {error}")),
    }
}

/// Apply `change` to the state under the lock, then write the state back in one rename.
fn edit<R>(db: &Db, change: impl FnOnce(&mut State) -> R) -> Result<R, String> {
    let _guard = STATE_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut state = load(db)?;
    let result = change(&mut state);
    let path = state_path(db);
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?;
    fs::write(&temp, bytes)
        .and_then(|()| fs::rename(&temp, &path))
        .map_err(|error| format!("ompsdk state: {error}"))?;
    Ok(result)
}

fn lookup(db: &Db, sid: &str) -> Option<Entry> {
    load(db).ok()?.sessions.into_iter().find(|entry| entry.sid == sid)
}

fn update(db: &Db, sid: &str, change: impl FnOnce(&mut Entry)) -> Result<(), String> {
    edit(db, |state| {
        if let Some(entry) = state.sessions.iter_mut().find(|entry| entry.sid == sid) {
            change(entry);
        }
    })
}

/// The socket the ompsdk engine reaches the daemon on.
fn socket(launch: &Launch) -> Result<&Path, String> {
    launch
        .ompsdk_socket
        .as_deref()
        .ok_or_else(|| "ompsdk engine has no daemon socket".to_owned())
}

/// The transcript omp keeps for session `sid`, which sits at `sessions/<cwd>/<stamp>_<sid>.jsonl`.
fn transcript_of(sid: &str) -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("HOME")?).join(".omp/agent/sessions");
    let suffix = format!("_{sid}.jsonl");
    fs::read_dir(root).ok()?.flatten().find_map(|dir| {
        fs::read_dir(dir.path())
            .ok()?
            .flatten()
            .map(|file| file.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(&suffix))
            })
    })
}

fn transcript_refs(sid: &str) -> Vec<String> {
    transcript_of(sid)
        .map(|path| path.display().to_string())
        .into_iter()
        .collect()
}

fn card_source(entry: &Entry) -> Source {
    let label = if entry.title.is_empty() {
        entry.sid.chars().take(8).collect()
    } else {
        entry.title.clone()
    };
    Source {
        uri: format!("{}{}", ompsdk::SESSION_URI_PREFIX, entry.sid),
        label,
        cwd: entry.cwd.clone(),
        pane: None,
        target: None,
    }
}

fn prompt_blocks(text: &str, image: Option<&Upload>) -> Value {
    let mut blocks = vec![json!({"type": "text", "text": text})];
    if let Some(image) = image {
        blocks.push(json!({"type": "image", "mimeType": image.media_type, "data": image.base64}));
    }
    Value::Array(blocks)
}

/// One question an ask elicitation carries, read from its schema the way `acp-agent.ts` writes it.
pub(super) struct Question {
    pub(super) key: String,
    pub(super) text: String,
    pub(super) multi: bool,
    /// Each option's label and description.
    pub(super) options: Vec<(String, Option<String>)>,
    /// The custom-text field, present when the dialog offers Other.
    pub(super) other: Option<String>,
}

pub(super) fn questions(schema: &Value) -> Vec<Question> {
    let Some(properties) = schema["properties"].as_object() else {
        return Vec::new();
    };
    let mut keys: Vec<&String> = properties.keys().filter(|key| !key.ends_with("__other")).collect();
    keys.sort_by_key(|key| (question_rank(key), key.to_string()));
    keys.into_iter()
        .map(|key| {
            let property = &properties[key];
            let multi_choices = property["items"]["anyOf"].as_array();
            let choices = multi_choices
                .or_else(|| property["oneOf"].as_array())
                .cloned()
                .unwrap_or_default();
            let other = format!("{key}__other");
            Question {
                key: key.clone(),
                text: property["title"].as_str().unwrap_or_default().to_owned(),
                multi: multi_choices.is_some(),
                options: choices
                    .iter()
                    .map(|choice| {
                        (
                            choice["const"].as_str().unwrap_or_default().to_owned(),
                            choice["description"].as_str().map(str::to_owned),
                        )
                    })
                    .collect(),
                other: properties.contains_key(&other).then_some(other),
            }
        })
        .collect()
}

/// Ask-tool questions sort by their `q<N>` number; form fields follow by name.
fn question_rank(key: &str) -> u32 {
    key.strip_prefix('q').and_then(|rest| rest.parse().ok()).unwrap_or(u32::MAX)
}

/// Whether an elicitation is a plan review, the select that offers Approve and execute.
fn is_plan_review(schema: &Value) -> bool {
    schema["properties"]["value"]["enum"]
        .as_array()
        .is_some_and(|choices| choices.iter().any(|choice| choice == PLAN_APPROVE))
}

fn card_id(sid: &str, elicitation: &Elicitation) -> String {
    let kind = if is_plan_review(&elicitation.schema) { "plan" } else { "ask" };
    format!("{kind}:ompsdk:{sid}:{}", elicitation.key())
}

fn question_options(question: &Question) -> Vec<CardOption> {
    question
        .options
        .iter()
        .map(|(label, description)| CardOption {
            label: label.clone(),
            description: description.clone(),
            ..CardOption::default()
        })
        .collect()
}

/// The card for one open elicitation, a plan review or a question.
fn prompt_card(entry: &Entry, elicitation: &Elicitation) -> Card {
    let refs = transcript_refs(&entry.sid);
    let tool_call_id = Some(elicitation.key());
    if is_plan_review(&elicitation.schema) {
        let name = if entry.title.is_empty() { "plan" } else { entry.title.as_str() };
        let body = plan_body(&entry.sid).unwrap_or_else(|| elicitation.message.clone());
        return Card {
            id: card_id(&entry.sid, elicitation),
            kind: Kind::Plan,
            priority: 1,
            created_ms: events::now_ms(),
            source: card_source(entry),
            title: format!("Plan ready: {name}"),
            star: Star::default(),
            options: vec![
                CardOption {
                    label: "Approve".to_owned(),
                    recommended: true,
                    ..CardOption::default()
                },
                CardOption {
                    label: "Refine".to_owned(),
                    ..CardOption::default()
                },
            ],
            answerable: true,
            allow_other: true,
            questions: Vec::new(),
            body: Some(omp::redact_credentials(&body)),
            refs,
            level: None,
            tool_call_id,
            report_due: false,
            engine: Some("ompsdk".to_owned()),
            status: Some(CardStatus::PlanReady),
        };
    }
    let asked = questions(&elicitation.schema);
    let first = asked.first();
    let title = first.map_or_else(|| elicitation.message.clone(), |question| question.text.clone());
    let multi_question = asked.len() > 1;
    Card {
        id: card_id(&entry.sid, elicitation),
        kind: Kind::Ask,
        priority: 1,
        created_ms: events::now_ms(),
        source: card_source(entry),
        title: title.clone(),
        star: Star {
            task: title,
            ..Star::default()
        },
        options: first.map(question_options).unwrap_or_default(),
        answerable: !asked.is_empty() && asked.iter().all(|question| !question.multi),
        allow_other: first.is_none_or(|question| question.other.is_some() || question.options.is_empty()),
        questions: if multi_question {
            asked
                .iter()
                .enumerate()
                .map(|(index, question)| CardQuestion {
                    label: format!("Question {}", index + 1),
                    question: question.text.clone(),
                    options: question_options(question),
                    allow_other: question.other.is_some() || question.options.is_empty(),
                    star: None,
                })
                .collect()
        } else {
            Vec::new()
        },
        body: None,
        refs,
        level: None,
        tool_call_id,
        report_due: false,
        engine: Some("ompsdk".to_owned()),
        status: Some(CardStatus::Waiting),
    }
}

/// The newest plan the session proposed, read from the plan file its transcript names.
fn plan_body(sid: &str) -> Option<String> {
    let path = transcript_of(sid)?;
    let plan = omp::proposes(&path).ok()?.pop()?;
    fs::read_to_string(plan.plan_path).ok()
}

/// The info card for a turn that ended. Its id names the turn by its transcript entry.
fn report_card(
    db: &Db,
    entry: &Entry,
    entry_id: &str,
    transcript: &str,
    text: String,
    report: Option<String>,
    failed: bool,
) -> Card {
    let key = format!("ompsdk:{}", entry.sid);
    let (star, investigation) = trail(db, &key, 0);
    let source = card_source(entry);
    let briefed_ms = db
        .pane_launch_since(&key, 0)
        .ok()
        .flatten()
        .map_or(0, |(_, swipe)| swipe.at_ms);
    let owed = !db.reported_since(&source.uri, briefed_ms).unwrap_or(false);
    let mut turn = reply_turn(text, report, investigation, owed);
    if failed {
        turn.report_due = false;
        turn.action.clear();
        turn.status = CardStatus::Incomplete;
        turn.level = "warn";
    }
    let mut card = turn_card(
        format!("done:ompsdk:{}:{entry_id}", entry.sid),
        source,
        vec![transcript.to_owned()],
        star,
        turn,
    );
    card.engine = Some("ompsdk".to_owned());
    card
}

fn tick(memo: &mut Memo, db: &Db, launch: &Launch, now: i64) -> usize {
    let Some(socket) = launch.ompsdk_socket.as_deref() else {
        return 0;
    };
    if now < memo.retry_at_ms {
        return 0;
    }
    let state = match load(db) {
        Ok(state) => state,
        Err(error) => {
            backoff(memo, now, &error);
            return 0;
        }
    };
    let held: Vec<String> = state
        .sessions
        .iter()
        .filter(|entry| entry.loaded || entry.cut)
        .map(|entry| entry.sid.clone())
        .collect();
    if held.is_empty() {
        return 0;
    }
    let client = match ompsdk::client(socket) {
        Ok(client) => client,
        Err(error) => {
            backoff(memo, now, &error);
            return 0;
        }
    };
    memo.failing = false;
    if memo.connection != Some(client.id) {
        memo.connection = Some(client.id);
        for sid in &held {
            resume_held(memo, db, &client, sid, now);
        }
        // Replayed requests and turn ends arrive after the resume replies. The next poll reads them.
        return 0;
    }
    held.iter()
        .map(|sid| tend(memo, db, &client, sid, now))
        .sum()
}

/// Wait `RETRY_MS` before the next poll. The error prints only on the first failure in a row.
fn backoff(memo: &mut Memo, now: i64, error: &str) {
    if !memo.failing {
        eprintln!("triage: ompsdk: {error}");
    }
    memo.failing = true;
    memo.retry_at_ms = now + RETRY_MS;
}

/// Reopen a session the daemon held before a reconnect. A failed resume cuts the session.
fn resume_held(memo: &mut Memo, db: &Db, client: &Client, sid: &str, now: i64) {
    let Some(entry) = lookup(db, sid) else {
        return;
    };
    if let Err(error) = client.resume(sid, &entry.cwd) {
        eprintln!("triage: ompsdk: resume {sid}: {error}");
        mark_cut(memo, db, sid, now);
    }
}

/// Mark a session cut. The next poll resumes it, after `RESUME_RETRY_MS` when a resume just failed.
fn mark_cut(memo: &mut Memo, db: &Db, sid: &str, now: i64) {
    let _ = update(db, sid, |entry| entry.cut = true);
    memo.resume_at_ms
        .insert(sid.to_owned(), now + RESUME_RETRY_MS);
}

/// Count a poll that found the turn still running. A turn that runs too long is cut.
fn stalled(memo: &mut Memo, db: &Db, sid: &str, now: i64) {
    let ticks = memo.running.entry(sid.to_owned()).or_default();
    *ticks += 1;
    if *ticks >= RUNNING_TICKS {
        memo.running.remove(sid);
        mark_cut(memo, db, sid, now);
    }
}

/// Advance one held session: answer or card its open elicitations, retire the cards of answered
/// ones, and settle its turn once the turn has ended. Returns the cards written.
fn tend(memo: &mut Memo, db: &Db, client: &Client, sid: &str, now: i64) -> usize {
    let Some(entry) = lookup(db, sid) else {
        return 0;
    };
    let open = client.elicitations(sid);
    let mut written = 0;
    for elicitation in &open {
        written += answer_or_card(db, client, &entry, elicitation);
    }
    finish_gone(db, &entry, &open, &mut written);
    if !open.is_empty() {
        // A session that waits on the user is not running out of time.
        memo.running.remove(sid);
        return written;
    }
    if entry.cut {
        if memo.resume_at_ms.get(sid).is_none_or(|due| now >= *due) {
            revive(memo, db, client, &entry, now);
        }
        return written;
    }
    if client.turn(sid) == Turn::Running {
        stalled(memo, db, sid, now);
        return written;
    }
    memo.running.remove(sid);
    written + settle(memo, db, client, &entry, now)
}

/// Approve a plan review that plan-into approves on its own, or write the card for an elicitation.
fn answer_or_card(db: &Db, client: &Client, entry: &Entry, elicitation: &Elicitation) -> usize {
    if matches!(db.card(&card_id(&entry.sid, elicitation)), Ok(Some(_))) {
        return 0;
    }
    if is_plan_review(&elicitation.schema)
        && !entry.auto_approved
        && let Some(plan_into) = &entry.plan_into
    {
        let executor = plan_into.model.as_deref().unwrap_or("@smol");
        let approved = approve(client, &entry.sid, elicitation, Some(executor), plan_into.thinking.as_deref());
        match approved {
            Ok(()) => {
                let _ = update(db, &entry.sid, |entry| entry.auto_approved = true);
                return 0;
            }
            // A plan that will not approve falls back to a card, so the user can answer it.
            Err(error) => eprintln!("triage: ompsdk: plan approval {}: {error}", entry.sid),
        }
    }
    match db.insert_card(&prompt_card(entry, elicitation)) {
        Ok(inserted) => usize::from(inserted),
        Err(error) => {
            eprintln!("triage: ompsdk: card for {}: {error}", entry.sid);
            0
        }
    }
}

/// Finish the open ask and plan cards of `entry` whose elicitation the session no longer waits on.
fn finish_gone(db: &Db, entry: &Entry, open: &[Elicitation], written: &mut usize) {
    let keys: HashSet<String> = open.iter().map(Elicitation::key).collect();
    let uri = card_source(entry).uri;
    let Ok(cards) = db.unresolved_cards() else {
        return;
    };
    let gone: Vec<Card> = cards
        .into_iter()
        .filter(|card| {
            card.engine.as_deref() == Some("ompsdk")
                && card.source.uri == uri
                && matches!(card.kind, Kind::Ask | Kind::Plan)
                && card.tool_call_id.as_ref().is_some_and(|key| !keys.contains(key))
        })
        .collect();
    finish_cards(db, gone, "answered elsewhere", written);
}

/// Resume a cut session and nudge it on. A session with no run behind it gets the cut prompt.
fn revive(memo: &mut Memo, db: &Db, client: &Client, entry: &Entry, now: i64) {
    let sid = entry.sid.as_str();
    let outcome = client.resume(sid, &entry.cwd).and_then(|running| {
        if running {
            Ok(())
        } else {
            client.prompt(sid, prompt_blocks(CUT_PROMPT, None))
        }
    });
    match outcome {
        Ok(()) => {
            let _ = update(db, sid, |entry| entry.cut = false);
            memo.running.remove(sid);
        }
        Err(error) => {
            eprintln!("triage: ompsdk: resume {sid}: {error}");
            memo.resume_at_ms.insert(sid.to_owned(), now + RESUME_RETRY_MS);
        }
    }
}

/// Read the transcript of a session whose turn ended, and act on how the turn ended.
fn settle(memo: &mut Memo, db: &Db, client: &Client, entry: &Entry, now: i64) -> usize {
    let sid = entry.sid.as_str();
    let Some(path) = transcript_of(sid) else {
        stalled(memo, db, sid, now);
        return 0;
    };
    let end = match omp::turn_end(&path) {
        Ok(Some(end)) => end,
        Ok(None) | Err(_) => omp::TurnEnd::Running,
    };
    let transcript = path.display().to_string();
    match end {
        omp::TurnEnd::Running => {
            stalled(memo, db, sid, now);
            0
        }
        omp::TurnEnd::Aborted => {
            retire(db, client, sid);
            0
        }
        omp::TurnEnd::Error { entry_id, message } => {
            let text = if message.trim().is_empty() {
                "Stopped with error".to_owned()
            } else {
                format!("Stopped with error: {message}")
            };
            let card = report_card(db, entry, &entry_id, &transcript, text, None, true);
            publish(db, client, entry, card)
        }
        omp::TurnEnd::Reply { entry_id, text, report, .. } => {
            let status = report.is_some() || omp::final_report(&text).is_some();
            if !status && entry.continues < MAX_CONTINUES {
                let prompt = format!(
                    "Continue the triage brief at {}. Finish every open part, then reply with the final report its format describes, starting with the ENVOY_STATUS line.",
                    entry.brief
                );
                match client.prompt(sid, prompt_blocks(&prompt, None)) {
                    Ok(()) => {
                        let _ = update(db, sid, |entry| entry.continues += 1);
                    }
                    Err(error) => eprintln!("triage: ompsdk: continue {sid}: {error}"),
                }
                return 0;
            }
            let card = report_card(db, entry, &entry_id, &transcript, text, report, false);
            publish(db, client, entry, card)
        }
    }
}

/// Write a turn's card, supersede the session's older info cards, and close the session. The
/// card insert is idempotent, so a close that fails retries the whole step on a later poll.
fn publish(db: &Db, client: &Client, entry: &Entry, card: Card) -> usize {
    let mut written = 0;
    let transcript = card.refs.first().map_or("", String::as_str).to_owned();
    supersede_info_cards(db, &card.id, &transcript, None, &mut written);
    if let Ok(true) = db.insert_card(&card) {
        written += 1;
    }
    retire(db, client, &entry.sid);
    written
}

/// Close a session the poll is done with and stop holding it.
fn retire(db: &Db, client: &Client, sid: &str) {
    match client.close(sid) {
        Ok(()) => {
            let _ = update(db, sid, |entry| entry.loaded = false);
        }
        Err(error) => eprintln!("triage: ompsdk: close {sid}: {error}"),
    }
}

/// Set the executor model and effort, then accept a plan review with Approve and execute.
fn approve(
    client: &Client,
    sid: &str,
    elicitation: &Elicitation,
    executor: Option<&str>,
    effort: Option<&str>,
) -> Result<(), String> {
    if executor.is_some() {
        configure(client, sid, executor, effort)?;
    }
    client.respond(
        sid,
        &elicitation.rpc_id,
        json!({"action": "accept", "content": {"value": PLAN_APPROVE}}),
    )
}

/// Set the model a selector names, then the thinking level. A selector-less launch sets only the level.
fn configure(client: &Client, sid: &str, selector: Option<&str>, thinking: Option<&str>) -> Result<(), String> {
    let Some(selector) = selector else {
        return match thinking {
            Some(level) => client.set_config(sid, "thinking", level),
            None => Ok(()),
        };
    };
    let resolved = ompsdk::resolve_model(selector, thinking, &client.options(sid))?;
    client.set_config(sid, "model", &resolved.model)?;
    match resolved.thinking {
        Some(level) => client.set_config(sid, "thinking", &level),
        None => Ok(()),
    }
}

/// The plan-into settings a start carries, which the session keeps to approve its plan with.
fn plan_into_of(start: Start) -> Option<PlanInto> {
    match start {
        Start::PlanInto(model, thinking) => Some(PlanInto {
            model: model.map(str::to_owned),
            thinking: thinking.map(str::to_owned),
        }),
        Start::Goal | Start::Plan => None,
    }
}

/// The first prompt of a session. A goal names the brief and asks for all of it, while a plan
/// reads the brief the way a dispatch does.
fn first_prompt(brief: &Path, start: Start) -> Result<String, String> {
    Ok(match start {
        Start::Goal => format!(
            "{}\n\nCarry out the triage brief saved at {}. Read the whole file before you start and again before you finish; it holds the full task and the report format.",
            brief_heading(brief, 120)?,
            brief.display()
        ),
        Start::Plan | Start::PlanInto(..) => dispatch_prompt(brief),
    })
}

/// Start an ompsdk session on `brief` and return its `ompsdk:<sid>` pane id.
pub(super) fn launch_new(
    db: &Db,
    launch: &Launch,
    cwd: &str,
    brief: &Path,
    start: Start,
    image: Option<&Upload>,
    title: &str,
) -> Result<String, String> {
    let client = ompsdk::client(socket(launch)?)?;
    let sid = client.new_session(cwd)?;
    let begun = begin(&client, &sid, launch, brief, start, image);
    if let Err(error) = begun {
        let _ = client.close(&sid);
        return Err(error);
    }
    let entry = Entry {
        sid: sid.clone(),
        cwd: cwd.to_owned(),
        brief: brief.display().to_string(),
        title: title.to_owned(),
        plan_into: plan_into_of(start),
        auto_approved: false,
        continues: 0,
        loaded: true,
        cut: false,
    };
    edit(db, |state| state.sessions.push(entry))?;
    Ok(format!("ompsdk:{sid}"))
}

/// Set a new session up for `start` and send its first prompt.
fn begin(
    client: &Client,
    sid: &str,
    launch: &Launch,
    brief: &Path,
    start: Start,
    image: Option<&Upload>,
) -> Result<(), String> {
    if matches!(start, Start::Plan | Start::PlanInto(..)) {
        client.set_config(sid, "mode", "plan")?;
    }
    configure(client, sid, launch.model.as_deref(), launch.thinking.as_deref())?;
    client.prompt(sid, prompt_blocks(&first_prompt(brief, start)?, image))
}

/// Give a brief to the card's ompsdk session. A session the daemon no longer holds is resumed first.
/// A card with no ompsdk session starts one.
pub(super) fn launch_brief(
    db: &Db,
    card: &Card,
    launch: &Launch,
    brief: &Path,
    start: Start,
    image: Option<&Upload>,
) -> Result<String, String> {
    let Some(sid) = ompsdk::session_id(card) else {
        return launch_new(db, launch, &card.source.cwd, brief, start, image, &card.title);
    };
    let entry = lookup(db, sid).ok_or("ompsdk session is no longer tracked")?;
    let client = ompsdk::client(socket(launch)?)?;
    let running = if entry.loaded {
        client.turn(sid) == Turn::Running
    } else {
        client.resume(sid, &entry.cwd)?
    };
    if matches!(start, Start::Plan | Start::PlanInto(..)) && !running {
        client.set_config(sid, "mode", "plan")?;
    }
    let text = dispatch_prompt(brief);
    let taken = running && client.steer(sid, &text)? == "injected";
    if !taken {
        client.prompt(sid, prompt_blocks(&text, image))?;
    }
    let plan_into = plan_into_of(start);
    update(db, sid, |entry| {
        entry.loaded = true;
        entry.cut = false;
        entry.continues = 0;
        entry.auto_approved = false;
        entry.plan_into = plan_into;
    })?;
    Ok(format!("ompsdk:{sid}"))
}

/// Answer an ompsdk question with the picked options or the typed text, one answer per question.
pub(super) fn answer_ask(db: &Db, card: &Card, sid: &str, req: &AnswerReq, launch: &Launch) -> Result<(), String> {
    let client = ompsdk::client(socket(launch)?)?;
    let key = card.tool_call_id.as_deref().ok_or("ask card has no question")?;
    let open = client
        .elicitation(sid, key)
        .ok_or("question is no longer pending")?;
    let asked = questions(&open.schema);
    let answers = if req.answers.is_empty() {
        vec![Answer {
            option: req.option,
            text: req.text.clone(),
        }]
    } else {
        req.answers.clone()
    };
    if answers.len() != asked.len() {
        return Err("ask answer needs one answer per question".to_owned());
    }
    let mut content = serde_json::Map::new();
    for (question, answer) in asked.iter().zip(&answers) {
        let text = answer.text.as_deref().map(str::trim).filter(|text| !text.is_empty());
        if let Some(text) = text {
            let slot = question.other.clone().unwrap_or_else(|| question.key.clone());
            content.insert(slot, Value::String(text.to_owned()));
            continue;
        }
        let (label, _) = answer
            .option
            .and_then(|index| question.options.get(index))
            .ok_or("ask answer needs an option or text")?;
        let value = if question.multi { json!([label]) } else { json!(label) };
        content.insert(question.key.clone(), value);
    }
    client.respond(sid, &open.rpc_id, json!({"action": "accept", "content": content}))?;
    let (option, text) = match answers.as_slice() {
        [only] => (only.option, only.text.clone()),
        _ => (None, None),
    };
    if record_swipe(db, &card.id, "done", option, text, req.note.clone(), Some("sent to ompsdk".to_owned())) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Answer an ompsdk plan review. Approve sets the executor first, so the plan runs on it. Refine
/// sends the feedback into the running turn, or as a new prompt when no turn takes it.
pub(super) fn answer_plan(
    db: &Db,
    card: &Card,
    sid: &str,
    req: &AnswerReq,
    choice: super::dsh_track::PlanChoice<'_>,
    launch: &Launch,
) -> Result<(), String> {
    let super::dsh_track::PlanChoice { option, refinement, executor, executor_effort } = choice;
    let client = ompsdk::client(socket(launch)?)?;
    let key = card.tool_call_id.as_deref().ok_or("plan card has no review")?;
    let open = client
        .elicitation(sid, key)
        .ok_or("plan review is not open")?;
    let text = if option == Some(0) {
        approve(&client, sid, &open, executor, executor_effort)?;
        executor.map(str::to_owned)
    } else {
        let feedback = refinement.ok_or("Refine requires nonempty text")?;
        client.respond(
            sid,
            &open.rpc_id,
            json!({"action": "accept", "content": {"value": PLAN_REFINE}}),
        )?;
        if client.steer(sid, feedback)? != "injected" {
            client.prompt(sid, prompt_blocks(feedback, None))?;
        }
        Some(feedback.to_owned())
    };
    if record_swipe(db, &card.id, "done", option, text, req.note.clone(), Some("sent to ompsdk".to_owned())) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Close an ompsdk session and resolve the cards it still has open.
pub(super) fn close(db: &Db, card: &Card, sid: &str, note: Option<&str>, launch: &Launch) -> Result<(), String> {
    let open = db.unresolved_cards().map_err(|error| error.to_string())?;
    if lookup(db, sid).is_some_and(|entry| entry.loaded) {
        ompsdk::client(socket(launch)?)?.close(sid)?;
    }
    let reason = format!("closed ompsdk:{sid}");
    if !record_swipe(
        db,
        &card.id,
        "done",
        None,
        Some("Close".to_owned()),
        note.map(str::to_owned),
        Some(reason.clone()),
    ) {
        return Err("could not record swipe".to_owned());
    }
    edit(db, |state| state.sessions.retain(|entry| entry.sid != sid))?;
    let others: Vec<Card> = open
        .into_iter()
        .filter(|other| other.id != card.id && other.source.uri == card.source.uri)
        .collect();
    let mut written = 0;
    finish_cards(db, others, &reason, &mut written);
    Ok(())
}

/// Ask an idle ompsdk session for its final report. A session still running, or waiting on an
/// answer, refuses the request.
pub(super) fn request_report(db: &Db, card: &Card, sid: &str, launch: &Launch) -> Result<(), String> {
    let entry = lookup(db, sid).ok_or("ompsdk session is no longer tracked")?;
    let client = ompsdk::client(socket(launch)?)?;
    if !entry.loaded && client.resume(sid, &entry.cwd)? {
        return Err("session is busy".to_owned());
    }
    if client.turn(sid) == Turn::Running || !client.elicitations(sid).is_empty() {
        return Err("session is busy".to_owned());
    }
    let request = report_request(db)?;
    client.prompt(sid, prompt_blocks(&request, None))?;
    update(db, sid, |entry| {
        entry.loaded = true;
        entry.cut = false;
    })?;
    let reason = format!("report requested ompsdk:{sid}");
    if record_swipe(
        db,
        &card.id,
        "done",
        None,
        Some("Request final report".to_owned()),
        None,
        Some(reason),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Hand a held ompsdk session to a tmux window that runs `omp --resume` on its transcript. A turn
/// that was still running resumes with the cut prompt, so the work goes on where it stopped.
pub(super) fn open_cli(db: &Db, card: &Card, sid: &str, launch: &Launch) -> Result<(), String> {
    let entry = lookup(db, sid).ok_or("ompsdk session is no longer tracked")?;
    let mut running = false;
    if entry.loaded {
        let client = ompsdk::client(socket(launch)?)?;
        running = client.turn(sid) == Turn::Running;
        client.close(sid)?;
    }
    if lease_held(sid)? {
        return Err("session is held by another omp".to_owned());
    }
    let path = transcript_of(sid).ok_or("transcript not found")?;
    let continue_arg = if running {
        format!(" {}", shell_quote(CUT_PROMPT))
    } else {
        String::new()
    };
    let command = format!(
        "omp --resume={}{continue_arg}",
        shell_quote(&path.display().to_string())
    );
    let session = launch.session.as_deref().unwrap_or(DEFAULT_SESSION);
    let pane = tmux::new_window(session, &entry.cwd, &command).map_err(|error| error.to_string())?;
    edit(db, |state| state.sessions.retain(|entry| entry.sid != sid))?;
    let reason = format!("opened in cli {pane}");
    let open = db.unresolved_cards().map_err(|error| error.to_string())?;
    let others: Vec<Card> = open
        .into_iter()
        .filter(|other| other.id != card.id && other.source.uri == card.source.uri)
        .collect();
    let mut written = 0;
    finish_cards(db, others, &reason, &mut written);
    if record_swipe(
        db,
        &card.id,
        "done",
        None,
        Some("Open in CLI".to_owned()),
        None,
        Some(reason),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Whether omp reports the session held by a process, so a second omp on it would clash.
fn lease_held(sid: &str) -> Result<bool, String> {
    let output = Command::new("omp")
        .args(["session", "lease", "--json", "--", sid])
        .output()
        .map_err(|error| format!("omp session lease: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "omp session lease: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let lease: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| format!("omp session lease: {error}"))?;
    Ok(held_by(&lease, sid))
}

/// Whether omp's lease report lists `sid` as held. An empty `held` list means nothing holds it.
pub(super) fn held_by(lease: &Value, sid: &str) -> bool {
    lease
        .get("held")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(sid)))
}
