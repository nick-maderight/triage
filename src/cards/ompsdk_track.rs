//! Triage's ompsdk sessions. The ompsdk daemon runs each one as an omp session in its triage pool.
//! Triage starts them, answers their questions and plan reviews, turns their replies into
//! cards, and keeps what the poll loop needs in `ompsdk-sessions.json` beside the database.

use super::{
    DEFAULT_SESSION, Launch, Start, TickReport, brief_heading, dispatch_prompt, finish_cards, workdir,
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
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Continuation prompts a reply without a status line earns before triage reports it anyway.
const MAX_CONTINUES: u32 = 5;
/// Polls an ended turn may read as unfinished in its transcript before its session counts as cut.
/// A turn the daemon still runs is never cut, however long it takes.
const RUNNING_TICKS: u32 = 30;
/// Tries a plan-into auto-approval gets before its plan falls back to a card. With the waits
/// of `CALL_WAIT_MS` a busy pool has about ten minutes to take it.
const APPROVE_TRIES: u32 = 8;
/// First and longest wait after a failed connect or state read, in ms.
const CONNECT_WAIT_MS: (i64, i64) = (1_000, 60_000);
/// First and longest wait after a daemon call for one session failed, in ms.
const CALL_WAIT_MS: (i64, i64) = (5_000, 600_000);
/// First and longest wait before a session cut again soon after a revive gets its next revive, in ms.
const CUT_WAIT_MS: (i64, i64) = (60_000, 1_800_000);
/// Work between two cuts that clears the count of revives in a row, in ms.
const CUT_RESET_MS: i64 = 1_800_000;
/// Revives in a row a session gets before triage stops and reports it.
const MAX_CUTS: u32 = 5;
/// First and longest wait before a card action runs again after the daemon or its pool failed it, in ms.
const ACTION_WAIT_MS: (i64, i64) = if cfg!(test) { (5, 20) } else { (2_000, 60_000) };
/// How long a card action keeps trying. It outlasts the daemon's check for a wedged pool.
const ACTION_BUDGET: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 900 });
/// The answers a plan review takes, as `acp-agent.ts` spells them.
const PLAN_APPROVE: &str = "Approve and execute";
const PLAN_REFINE: &str = "Refine plan";
/// The prompt that resumes a session cut mid-turn.
const CUT_PROMPT: &str = "Continue where you stopped.";

/// The wait before try `tries` of a step that failed, in ms. It doubles from the first wait up
/// to the longest. Up to a quarter more is added at random, which keeps sessions that failed
/// together from trying again together.
pub(super) fn wait_ms((first, longest): (i64, i64), tries: u32) -> i64 {
    let wait = first.saturating_mul(1 << tries.saturating_sub(1).min(30)).min(longest);
    let random = std::collections::hash_map::RandomState::new().hash_one(tries);
    wait + (random % (wait as u64 / 4 + 1)) as i64
}

/// What the ompsdk poll remembers between polls. The server keeps it apart from the pane
/// tracker, so a call that waits on a busy pool never holds up the rest of the inbox poll.
#[derive(Default)]
pub struct Memo {
    /// Connection id of the last poll. A new id means a reconnect, which has bound no session yet.
    connection: Option<u64>,
    /// Whether the next poll asks the daemon which sessions it still holds.
    recheck: bool,
    /// Failed connects or state reads in a row, and the earliest next poll.
    failures: u32,
    retry_at_ms: i64,
    /// Polls each ended turn has read as unfinished.
    running: HashMap<String, u32>,
    /// Failed daemon calls in a row and the earliest next try, by session id. A session cut
    /// again soon after a revive waits here as well.
    waits: HashMap<String, (u32, i64)>,
    /// Failed plan-into auto-approvals and the earliest next try, by plan card id.
    approve_tries: HashMap<String, (u32, i64)>,
}

impl Memo {
    /// Drive the ompsdk sessions triage holds and return the number of cards written.
    pub fn tick(&mut self, db: &Db, launch: &Launch) -> TickReport {
        TickReport {
            written: tick(self, db, launch, events::now_ms()),
        }
    }
}

/// One tracked ompsdk session, as `ompsdk-sessions.json` keeps it. The file is what brings a
/// session back after triage, the daemon or its pool restarts.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Entry {
    sid: String,
    cwd: String,
    /// The brief the session works on. A continuation names it.
    brief: String,
    title: String,
    /// What a plan-into launch approves its plan with.
    plan_into: Option<PlanInto>,
    /// Continuation prompts sent since the session last got a brief.
    continues: u32,
    /// Whether the daemon holds the session for triage.
    loaded: bool,
    /// Whether the session's turn was cut. The poll reopens the session and sends the cut prompt.
    cut: bool,
    /// Whether a cut session waits for the user before the poll reopens it. A session cut
    /// while a question was open waits for the answer and costs the pool nothing until then.
    parked: bool,
    /// Whether the session still plans. omp keeps plan mode in memory only, and a revive sets it again.
    planning: bool,
    /// The model and effort an approved plan runs on. A revive sets them again.
    executor: Option<PlanInto>,
    /// The answers the user gave since the session's last new question. A revive repeats them,
    /// because a cut may have dropped them.
    answers: Vec<Said>,
    /// Revives in a row with less than `CUT_RESET_MS` of work between them, and when the last one was sent.
    cuts: u32,
    revived_ms: i64,
    /// Whether the daemon must still let the session go, and whether triage then forgets it.
    closing: bool,
    forget: bool,
}

impl Entry {
    /// Whether the poll has work for the session. A parked session waits for the user.
    fn polled(&self) -> bool {
        self.closing || (!self.parked && (self.loaded || self.cut))
    }
}

/// One answer the user gave.
#[derive(Clone, Serialize, Deserialize)]
struct Said {
    /// The elicitation the answer is for.
    key: String,
    /// The answer in words, as the cut prompt repeats it.
    text: String,
    /// The answer to send once its question is open on the connection again. None once it was sent.
    queued: Option<Queued>,
}

/// An answer given while its question was out of reach.
#[derive(Clone, Serialize, Deserialize)]
enum Queued {
    Ask(Vec<Answer>),
    Approve,
    Refine(String),
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
/// Counts the writes of the state file. The poll reads a session again when a card action wrote since its read.
static STATE_REV: AtomicU64 = AtomicU64::new(0);

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
    STATE_REV.fetch_add(1, Ordering::SeqCst);
    Ok(result)
}

fn lookup(db: &Db, sid: &str) -> Option<Entry> {
    load(db).ok()?.sessions.into_iter().find(|entry| entry.sid == sid)
}

/// Whether triage still tracks session `sid`.
pub(super) fn tracked(db: &Db, sid: &str) -> bool {
    lookup(db, sid).is_some()
}

fn update(db: &Db, sid: &str, change: impl FnOnce(&mut Entry)) -> Result<(), String> {
    edit(db, |state| {
        if let Some(entry) = state.sessions.iter_mut().find(|entry| entry.sid == sid) {
            change(entry);
        }
    })
}
fn rebuilt(card: &Card, sid: &str) -> Result<Entry, String> {
    Ok(Entry {
        sid: sid.to_owned(),
        cwd: workdir(&card.source.cwd)?,
        title: card.source.label.clone(),
        ..Entry::default()
    })
}

fn adopt(db: &Db, card: &Card, sid: &str) -> Result<Entry, String> {
    if let Some(entry) = lookup(db, sid) {
        return Ok(entry);
    }
    if lease_held(sid)? {
        return Err("session is held by another omp".to_owned());
    }
    let entry = rebuilt(card, sid)?;
    edit(db, |state| {
        if !state.sessions.iter().any(|known| known.sid == sid) {
            state.sessions.push(entry.clone());
        }
    })?;
    Ok(entry)
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

/// The key of the plan review a session's agent waits on. omp keeps an earlier review open
/// when the agent proposes again, and the daemon numbers its requests in the order they came.
fn live_review(open: &[Elicitation]) -> Option<String> {
    open.iter()
        .enumerate()
        .filter(|(_, elicitation)| is_plan_review(&elicitation.schema))
        .max_by_key(|(at, elicitation)| (request_number(&elicitation.key()), *at))
        .map(|(_, elicitation)| elicitation.key())
}

/// The number a request id ends in, or 0 when it ends in none.
fn request_number(key: &str) -> u64 {
    let digits = key.len() - key.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    key[key.len() - digits..].parse().unwrap_or(0)
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

pub(super) fn tick(memo: &mut Memo, db: &Db, launch: &Launch, now: i64) -> usize {
    let Some(socket) = launch.ompsdk_socket.as_deref() else {
        return 0;
    };
    if now < memo.retry_at_ms {
        return 0;
    }
    let revision = STATE_REV.load(Ordering::SeqCst);
    let state = match load(db) {
        Ok(state) => state,
        Err(error) => {
            backoff(memo, now, &error);
            return 0;
        }
    };
    let work: Vec<Entry> = state.sessions.into_iter().filter(Entry::polled).collect();
    if work.is_empty() {
        return 0;
    }
    let client = match ompsdk::client(socket) {
        Ok(client) => client,
        Err(error) => {
            backoff(memo, now, &error);
            return 0;
        }
    };
    let mut written = 0;
    if memo.connection != Some(client.id) || memo.recheck {
        let held = match client.held() {
            Ok(held) => held,
            Err(error) => {
                backoff(memo, now, &error);
                return 0;
            }
        };
        memo.connection = Some(client.id);
        memo.recheck = false;
        memo.failures = 0;
        // A restart of the daemon or its pool took these sessions and cut their turns.
        for entry in &work {
            if entry.loaded && !entry.closing && !held.contains(&entry.sid) {
                written += cut(memo, db, entry, now, false);
            }
        }
        return written;
    }
    memo.failures = 0;
    let mut called = false;
    for entry in &work {
        // A card action holds its session's lock, sometimes while it waits on the pool. The
        // poll leaves that session to the action and moves on.
        let lock = db.answer_lock(&format!("ompsdk:{}", entry.sid));
        let _guard = match lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => continue,
        };
        // A card action may have changed the session since the poll read the state.
        let changed;
        let entry = if STATE_REV.load(Ordering::SeqCst) == revision {
            entry
        } else {
            match lookup(db, &entry.sid) {
                Some(fresh) if fresh.polled() => {
                    changed = fresh;
                    &changed
                }
                _ => continue,
            }
        };
        written += step(memo, db, &client, entry, now, &mut called);
    }
    written
}

/// Wait before the next poll after a failed connect or state read. The wait doubles while the
/// failure lasts, and the error prints only on the first failure in a row.
fn backoff(memo: &mut Memo, now: i64, error: &str) {
    if memo.failures == 0 {
        eprintln!("triage: ompsdk: {error}");
    }
    memo.failures += 1;
    memo.retry_at_ms = now + wait_ms(CONNECT_WAIT_MS, memo.failures);
}

/// Wait before the session's next daemon call after one failed. The wait doubles while the
/// failure lasts. The next poll also asks the daemon which sessions it still holds, because a
/// failed call often means the pool restarted.
fn retry_later(memo: &mut Memo, sid: &str, now: i64, what: &str, error: &str) {
    let tries = memo.waits.get(sid).map_or(0, |(tries, _)| *tries) + 1;
    if tries == 1 {
        eprintln!("triage: ompsdk: {what} {sid}: {error}");
    }
    memo.waits.insert(sid.to_owned(), (tries, now + wait_ms(CALL_WAIT_MS, tries)));
    memo.recheck = true;
}

/// Advance one session. `called` is set once this poll made a daemon call that takes the
/// pool's time, and the sessions that need such a call then take turns, one a poll. A restart
/// that cuts every session at once never sends the pool a pile of resumes.
fn step(memo: &mut Memo, db: &Db, client: &Client, entry: &Entry, now: i64, called: &mut bool) -> usize {
    let sid = entry.sid.as_str();
    let due = !*called && memo.waits.get(sid).is_none_or(|(_, due)| now >= *due);
    if entry.closing {
        if !entry.loaded {
            released(memo, db, entry);
        } else if due {
            *called = true;
            release(memo, db, client, entry, now);
        }
        return 0;
    }
    if entry.parked {
        return 0;
    }
    if entry.cut || !client.bound(sid) {
        if !due {
            return 0;
        }
        // A session the daemon still holds binds at once. A cut one may come back from disk,
        // which takes seconds of the pool's time.
        *called = entry.cut;
        let written = reopen(memo, db, client, entry, now);
        *called |= memo.recheck;
        return written;
    }
    tend(memo, db, client, entry, now, called)
}

/// Mark a session's turn cut and set when the poll reopens it. `loaded` says whether the
/// daemon still holds the session. A session with a question open waits parked for its answer.
/// A session cut again soon after a revive waits longer each time, and stops with a card
/// after `MAX_CUTS` revives in a row.
fn cut(memo: &mut Memo, db: &Db, entry: &Entry, now: i64, loaded: bool) -> usize {
    let sid = entry.sid.as_str();
    memo.running.remove(sid);
    let cuts = if now - entry.revived_ms > CUT_RESET_MS { 0 } else { entry.cuts };
    if cuts >= MAX_CUTS {
        let reason = format!("its turn was cut {} times in a row", cuts + 1);
        return give_up(memo, db, entry, loaded, &reason);
    }
    let parked = has_question(db, entry);
    let _ = update(db, sid, |entry| {
        entry.cut = true;
        entry.loaded = loaded;
        entry.parked = parked;
        entry.cuts = cuts;
    });
    let wait = if cuts == 0 { 0 } else { wait_ms(CUT_WAIT_MS, cuts) };
    memo.waits.insert(sid.to_owned(), (0, now + wait));
    0
}

/// Whether the session has an ask or plan card the user has yet to answer.
fn has_question(db: &Db, entry: &Entry) -> bool {
    let uri = card_source(entry).uri;
    db.unresolved_cards().is_ok_and(|cards| {
        cards.iter().any(|card| {
            card.engine.as_deref() == Some("ompsdk")
                && card.source.uri == uri
                && matches!(card.kind, Kind::Ask | Kind::Plan)
        })
    })
}

/// Stop reviving a session and tell the user why. The session stays tracked and parked, and
/// a brief dispatched to the card continues it.
fn give_up(memo: &mut Memo, db: &Db, entry: &Entry, loaded: bool, reason: &str) -> usize {
    let sid = entry.sid.as_str();
    memo.waits.remove(sid);
    eprintln!("triage: ompsdk: stopped {sid}: {reason}");
    let transcript = transcript_of(sid).map_or_else(|| entry.brief.clone(), |path| path.display().to_string());
    let text = format!("Stopped: {reason}. Dispatch a brief to this card to continue the session.");
    let card = report_card(db, entry, &format!("stopped{now}", now = events::now_ms()), &transcript, text, None, true);
    let written = usize::from(matches!(db.insert_card(&card), Ok(true)));
    let _ = update(db, sid, |entry| {
        entry.cut = true;
        entry.parked = true;
        entry.cuts = 0;
        entry.loaded = loaded;
        entry.closing = loaded;
        entry.forget = false;
    });
    written
}

/// Bind a session to this connection. A session the daemon still held keeps its turn, and the
/// next poll tends it. A session whose turn was cut gets its mode and model back and then the
/// cut prompt, unless its turn runs on or had ended on its own.
fn reopen(memo: &mut Memo, db: &Db, client: &Client, entry: &Entry, now: i64) -> usize {
    let sid = entry.sid.as_str();
    let resumed = match client.resume(sid, &entry.cwd) {
        Ok(resumed) => resumed,
        Err(error) if ompsdk::transient(&error) => {
            retry_later(memo, sid, now, "resume", &error);
            return 0;
        }
        Err(error) => {
            let reason = format!("the session could not be reopened ({error})");
            return give_up(memo, db, entry, false, &reason);
        }
    };
    if !entry.cut {
        memo.waits.remove(sid);
        if resumed.held || resumed.running {
            return 0;
        }
        // The daemon read it back from disk. A restart cut its turn after the poll last asked.
        return cut(memo, db, entry, now, true);
    }
    let nudged = !resumed.running && !turn_over(sid);
    if nudged && let Err(error) = nudge(client, entry) {
        if ompsdk::transient(&error) {
            retry_later(memo, sid, now, "continue", &error);
            return 0;
        }
        let reason = format!("the session could not be continued ({error})");
        return give_up(memo, db, entry, true, &reason);
    }
    memo.waits.remove(sid);
    memo.running.remove(sid);
    let _ = update(db, sid, |entry| {
        entry.cut = false;
        entry.loaded = true;
        if nudged {
            entry.cuts += 1;
            entry.revived_ms = now;
            // The cut prompt carried every answer in words.
            for said in &mut entry.answers {
                said.queued = None;
            }
        }
    });
    0
}

/// Whether the session's last turn ended on its own, with a reply or an error, as its transcript tells.
fn turn_over(sid: &str) -> bool {
    transcript_of(sid)
        .and_then(|path| omp::turn_end(&path).ok().flatten())
        .is_some_and(|end| matches!(end, omp::TurnEnd::Reply { .. } | omp::TurnEnd::Error { .. }))
}

/// Put a cut session back in its mode and on its model, then send the cut prompt.
fn nudge(client: &Client, entry: &Entry) -> Result<(), String> {
    let sid = entry.sid.as_str();
    if entry.planning {
        client.set_config(sid, "mode", "plan")?;
    }
    if let Some(executor) = &entry.executor {
        configure(client, sid, executor.model.as_deref(), executor.thinking.as_deref())?;
    }
    client.prompt(sid, prompt_blocks(&cut_prompt(entry), None))
}

/// The prompt a cut session continues on. It names the brief, because the cut may have taken
/// the prompt that handed the brief over. It repeats the user's answers for the same reason.
fn cut_prompt(entry: &Entry) -> String {
    let mut prompt = format!("{CUT_PROMPT} Your last turn was cut short. Check what it left unfinished, including any job it had running.");
    if !entry.brief.is_empty() {
        prompt.push_str(&format!(" Your brief is at {}.", entry.brief));
    }
    if !entry.answers.is_empty() {
        prompt.push_str("\n\nThe user sent these answers before the cut. Use them in case they did not reach you.\n");
        for said in &entry.answers {
            prompt.push_str(&format!("- {}\n", said.text));
        }
    }
    prompt
}

/// Ask the daemon to let a session go. An error from omp itself means it holds nothing more.
fn release(memo: &mut Memo, db: &Db, client: &Client, entry: &Entry, now: i64) {
    match client.close(&entry.sid) {
        Err(error) if ompsdk::transient(&error) => retry_later(memo, &entry.sid, now, "close", &error),
        outcome => {
            if let Err(error) = outcome {
                eprintln!("triage: ompsdk: close {}: {error}", entry.sid);
            }
            released(memo, db, entry);
        }
    }
}

/// Stop holding a session the daemon let go, and forget one the user closed.
fn released(memo: &mut Memo, db: &Db, entry: &Entry) {
    let sid = entry.sid.as_str();
    memo.waits.remove(sid);
    memo.running.remove(sid);
    let _ = if entry.forget {
        edit(db, |state| state.sessions.retain(|entry| entry.sid != sid))
    } else {
        update(db, sid, |entry| {
            entry.loaded = false;
            entry.closing = false;
        })
    };
}

/// Count a poll that found an ended turn unfinished in its transcript. The last message can
/// reach the disk a moment late. A turn that stays unfinished is cut.
fn stalled(memo: &mut Memo, db: &Db, entry: &Entry, now: i64) -> usize {
    let ticks = memo.running.entry(entry.sid.clone()).or_default();
    *ticks += 1;
    if *ticks < RUNNING_TICKS {
        return 0;
    }
    cut(memo, db, entry, now, true)
}

/// Advance one bound session. Answers that waited for their question go out first. Every
/// other open elicitation gets an answer or a card, and the cards of answered ones are
/// finished. A turn that has ended is settled. Returns the cards written.
fn tend(memo: &mut Memo, db: &Db, client: &Client, entry: &Entry, now: i64, called: &mut bool) -> usize {
    let sid = entry.sid.as_str();
    let mut open = client.elicitations(sid);
    // Only the review the agent waits on gets an answer or a card. Approving it accepts the rest.
    let live = live_review(&open);
    open.retain(|elicitation| !is_plan_review(&elicitation.schema) || live.as_deref() == Some(elicitation.key().as_str()));
    let mut written = 0;
    for elicitation in &open {
        let key = elicitation.key();
        let queued = entry
            .answers
            .iter()
            .find(|said| said.key == key)
            .and_then(|said| said.queued.as_ref());
        let Some(queued) = queued else {
            written += answer_or_card(memo, db, client, entry, elicitation, now, called);
            continue;
        };
        if *called || memo.waits.get(sid).is_some_and(|(_, due)| now < *due) {
            continue;
        }
        // The user answered while the question was out of reach. It is open again.
        *called = true;
        match deliver(client, entry, elicitation, queued) {
            Err(error) if ompsdk::transient(&error) => retry_later(memo, sid, now, "answer", &error),
            outcome => {
                if let Err(error) = outcome {
                    eprintln!("triage: ompsdk: answer {sid}: {error}");
                }
                memo.waits.remove(sid);
                let _ = update(db, sid, |entry| {
                    for said in entry.answers.iter_mut().filter(|said| said.key == key) {
                        said.queued = None;
                    }
                });
            }
        }
    }
    finish_gone(db, entry, &open, &mut written);
    if !open.is_empty() || client.turn(sid) == Turn::Running {
        // A session that waits on the user or still runs its turn is not running out of time.
        memo.running.remove(sid);
        return written;
    }
    written + settle(memo, db, client, entry, now)
}

/// Send an answer that waited for its question to the open elicitation.
fn deliver(client: &Client, entry: &Entry, elicitation: &Elicitation, queued: &Queued) -> Result<(), String> {
    let sid = entry.sid.as_str();
    match queued {
        Queued::Ask(answers) => {
            let content = content_of(&elicitation.schema, answers)?;
            client.respond(sid, &elicitation.rpc_id, json!({"action": "accept", "content": content}))
        }
        Queued::Approve => {
            let executor = entry.executor.as_ref();
            approve(
                client,
                sid,
                elicitation,
                executor.and_then(|executor| executor.model.as_deref()),
                executor.and_then(|executor| executor.thinking.as_deref()),
            )
        }
        Queued::Refine(feedback) => refine(client, sid, elicitation, feedback),
    }
}

/// Approve a plan review of a plan-into session, or write the card for an elicitation. Triage
/// approves every plan of a plan-into session itself. The first runs on the model the launch
/// named, and a later one on the model the session already runs on. A failed approval tries
/// again on later polls, after waits that double. After `APPROVE_TRIES` the plan also gets a
/// card, which shows the failure, and the tries go on until one works or the user answers it.
fn answer_or_card(
    memo: &mut Memo,
    db: &Db,
    client: &Client,
    entry: &Entry,
    elicitation: &Elicitation,
    now: i64,
    called: &mut bool,
) -> usize {
    let id = card_id(&entry.sid, elicitation);
    let carded = matches!(db.card(&id), Ok(Some(_)));
    if is_plan_review(&elicitation.schema)
        && let Some(plan_into) = &entry.plan_into
    {
        let (tries, due) = memo.approve_tries.get(&id).copied().unwrap_or_default();
        if *called || now < due {
            return 0;
        }
        *called = true;
        let run = entry.executor.as_ref().unwrap_or(plan_into);
        let executor = run.model.as_deref().unwrap_or("@smol");
        let approved = approve(client, &entry.sid, elicitation, Some(executor), run.thinking.as_deref());
        match approved {
            Ok(()) => {
                memo.approve_tries.remove(&id);
                let executor = PlanInto {
                    model: Some(executor.to_owned()),
                    thinking: run.thinking.clone(),
                };
                let _ = update(db, &entry.sid, |entry| {
                    entry.planning = false;
                    entry.executor = Some(executor);
                });
                return 0;
            }
            Err(error) => {
                if tries == 0 {
                    eprintln!("triage: ompsdk: plan approval {}: {error}", entry.sid);
                }
                memo.approve_tries
                    .insert(id, (tries + 1, now + wait_ms(CALL_WAIT_MS, tries + 1)));
                if carded || tries + 1 < APPROVE_TRIES {
                    return 0;
                }
                // A plan that will not approve also gets a card for the user to answer.
            }
        }
    } else if carded {
        return 0;
    }
    // A new question means the session read the answers the user gave before it.
    if !entry.answers.is_empty() {
        let _ = update(db, &entry.sid, |entry| entry.answers.clear());
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

/// Read the transcript of a session whose turn ended, and act on how the turn ended.
fn settle(memo: &mut Memo, db: &Db, client: &Client, entry: &Entry, now: i64) -> usize {
    let sid = entry.sid.as_str();
    let Some(path) = transcript_of(sid) else {
        return stalled(memo, db, entry, now);
    };
    let end = match omp::turn_end(&path) {
        Ok(Some(end)) => end,
        Ok(None) | Err(_) => omp::TurnEnd::Running,
    };
    let transcript = path.display().to_string();
    match end {
        omp::TurnEnd::Running => stalled(memo, db, entry, now),
        // Triage never aborts a tracked turn. The daemon canceled a flood, or a restart cut the turn.
        omp::TurnEnd::Aborted => cut(memo, db, entry, now, true),
        omp::TurnEnd::Error { entry_id, message } => {
            let text = if message.trim().is_empty() {
                "Stopped with error".to_owned()
            } else {
                format!("Stopped with error: {message}")
            };
            let card = report_card(db, entry, &entry_id, &transcript, text, None, true);
            publish(memo, db, entry, card)
        }
        omp::TurnEnd::Reply { entry_id, text, report, .. } => {
            let status = report.is_some() || omp::final_report(&text).is_some();
            if !status && entry.continues < MAX_CONTINUES {
                let at = if entry.brief.is_empty() { String::new() } else { format!(" at {}", entry.brief) };
                let prompt = format!(
                    "Continue the triage brief{at}. Finish every open part, then reply with the final report its format describes, starting with the ENVOY_STATUS line."
                );
                memo.running.remove(sid);
                match client.prompt(sid, prompt_blocks(&prompt, None)) {
                    Ok(()) => {
                        let _ = update(db, sid, |entry| entry.continues += 1);
                    }
                    Err(error) => eprintln!("triage: ompsdk: continue {sid}: {error}"),
                }
                return 0;
            }
            let card = report_card(db, entry, &entry_id, &transcript, text, report, false);
            publish(memo, db, entry, card)
        }
    }
}

/// Write a turn's card, supersede the session's older info cards, and mark the session for
/// release. The poll then closes it in the daemon, and waits before each further try when a
/// close fails.
fn publish(memo: &mut Memo, db: &Db, entry: &Entry, card: Card) -> usize {
    let mut written = 0;
    let transcript = card.refs.first().map_or("", String::as_str).to_owned();
    supersede_info_cards(db, &card.id, &transcript, None, &mut written);
    if let Ok(true) = db.insert_card(&card) {
        written += 1;
    }
    memo.running.remove(&entry.sid);
    let _ = update(db, &entry.sid, |entry| {
        entry.closing = true;
        entry.forget = false;
        entry.cuts = 0;
        entry.executor = None;
        entry.answers.clear();
    });
    written
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
    let accept = json!({"action": "accept", "content": {"value": PLAN_APPROVE}});
    // omp keeps a review open when its agent proposes again. Each earlier one is accepted
    // first, so none stays open to hold the session's report back.
    let earlier = client.elicitations(sid);
    for review in earlier
        .iter()
        .filter(|open| is_plan_review(&open.schema) && open.rpc_id != elicitation.rpc_id)
    {
        client.respond(sid, &review.rpc_id, accept.clone())?;
    }
    client.respond(sid, &elicitation.rpc_id, accept)
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

/// Run a card action again after the daemon or its pool failed it. The waits between tries
/// double, and the action gives up after `ACTION_BUDGET`. An error from omp itself returns at once.
fn retrying<T>(mut action: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let deadline = Instant::now() + ACTION_BUDGET;
    let mut tries = 0;
    loop {
        match action() {
            Err(error) if ompsdk::transient(&error) && Instant::now() < deadline => {
                tries += 1;
                if tries == 1 {
                    eprintln!("triage: ompsdk: {error}; trying again");
                }
                thread::sleep(Duration::from_millis(wait_ms(ACTION_WAIT_MS, tries) as u64));
            }
            outcome => return outcome,
        }
    }
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
    retrying(|| {
        let client = ompsdk::client(socket(launch)?)?;
        let sid = client.new_session(cwd)?;
        let entry = Entry {
            sid: sid.clone(),
            cwd: cwd.to_owned(),
            brief: brief.display().to_string(),
            title: title.to_owned(),
            plan_into: plan_into_of(start),
            planning: matches!(start, Start::Plan | Start::PlanInto(..)),
            loaded: true,
            ..Entry::default()
        };
        // A session that started but is not tracked would run on with no poll to report it.
        let begun = begin(&client, &sid, launch, brief, start, image)
            .and_then(|()| edit(db, |state| state.sessions.push(entry)));
        if let Err(error) = begun {
            let _ = client.close(&sid);
            return Err(error);
        }
        Ok(format!("ompsdk:{sid}"))
    })
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

/// Give a brief to the card's session, rebuilding a missing state entry from its card.
pub(super) fn launch_brief(
    db: &Db,
    card: &Card,
    launch: &Launch,
    brief: &Path,
    start: Start,
    image: Option<&Upload>,
) -> Result<String, String> {
    let Some(sid) = ompsdk::session_id(card) else {
        return launch_new(db, launch, &workdir(&card.source.cwd)?, brief, start, image, &card.title);
    };
    let entry = adopt(db, card, sid)?;
    let planning = matches!(start, Start::Plan | Start::PlanInto(..));
    let text = dispatch_prompt(brief);
    let taken = retrying(|| {
        let client = ompsdk::client(socket(launch)?)?;
        let running = bind(db, &client, &entry)?;
        if planning && !running {
            client.set_config(sid, "mode", "plan")?;
        }
        let taken = running && client.steer(sid, &text)? == "injected";
        if !taken {
            client.prompt(sid, prompt_blocks(&text, image))?;
        }
        Ok(taken)
    })?;
    let plan_into = plan_into_of(start);
    update(db, sid, |entry| {
        entry.loaded = true;
        entry.cut = false;
        entry.parked = false;
        entry.closing = false;
        entry.continues = 0;
        entry.cuts = 0;
        entry.plan_into = plan_into;
        // A brief steered into a running turn leaves that turn's mode as it was.
        if !taken {
            entry.planning = planning;
        }
        entry.executor = None;
        entry.answers.clear();
        entry.brief = brief.display().to_string();
    })?;
    Ok(format!("ompsdk:{sid}"))
}

/// Whether a turn runs in the session. A session this connection has yet to bind is resumed
/// first, which also brings one back that the daemon no longer holds.
fn bind(db: &Db, client: &Client, entry: &Entry) -> Result<bool, String> {
    let sid = entry.sid.as_str();
    if client.bound(sid) {
        return Ok(client.turn(sid) == Turn::Running);
    }
    let resumed = client.resume(sid, &entry.cwd)?;
    // The daemon holds the session again. A later close must release it.
    update(db, sid, |entry| entry.loaded = true)?;
    Ok(resumed.running)
}

/// The question a card stands for, when this connection has it open.
fn open_question(launch: &Launch, sid: &str, key: &str) -> Option<(std::sync::Arc<Client>, Elicitation)> {
    let client = ompsdk::client(socket(launch).ok()?).ok()?;
    let open = client.elicitation(sid, key)?;
    Some((client, open))
}

/// Whether an answer reached the daemon. A failure of the daemon or its pool reads as not
/// sent, and the answer then waits in the session's entry. An error from omp itself is returned.
fn sent(outcome: Result<(), String>) -> Result<bool, String> {
    match outcome {
        Ok(()) => Ok(true),
        Err(error) if ompsdk::transient(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Keep an answer in the session's entry. One that was not sent also marks the session for
/// the poll, which sends it to its question once that is open again, or reopens a cut session
/// with the answer in the cut prompt.
fn remember(db: &Db, card: &Card, sid: &str, said: Said, change: impl FnOnce(&mut Entry)) -> Result<(), String> {
    adopt(db, card, sid)?;
    update(db, sid, |entry| {
        if said.queued.is_some() {
            entry.cut = true;
            entry.parked = false;
        }
        entry.answers.push(said);
        change(entry);
    })
}

/// The user's answers in words, one line per question, read from the card that asked.
fn spoken(card: &Card, answers: &[Answer]) -> Result<String, String> {
    if answers.len() != card.questions.len().max(1) {
        return Err("ask answer needs one answer per question".to_owned());
    }
    let lines: Result<Vec<String>, String> = answers
        .iter()
        .enumerate()
        .map(|(index, answer)| {
            let (question, options) = match card.questions.get(index) {
                Some(asked) => (asked.question.as_str(), &asked.options),
                None => (card.title.as_str(), &card.options),
            };
            let text = answer.text.as_deref().map(str::trim).filter(|text| !text.is_empty());
            let picked = answer.option.and_then(|index| options.get(index)).map(|option| option.label.as_str());
            let value = text.or(picked).ok_or("ask answer needs an option or text")?;
            Ok(format!("\"{question}\" was answered with: {value}"))
        })
        .collect();
    Ok(lines?.join("\n"))
}

/// The elicitation content for the user's answers, one field per question of the schema.
fn content_of(schema: &Value, answers: &[Answer]) -> Result<serde_json::Map<String, Value>, String> {
    let asked = questions(schema);
    if answers.len() != asked.len() {
        return Err("ask answer needs one answer per question".to_owned());
    }
    let mut content = serde_json::Map::new();
    for (question, answer) in asked.iter().zip(answers) {
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
    Ok(content)
}

/// Answer an ompsdk question with the picked options or the typed text, one answer per
/// question. An answer whose question is out of reach, after a restart or before the poll bound
/// the session again, is kept and returns at once. The poll sends it.
pub(super) fn answer_ask(db: &Db, card: &Card, sid: &str, req: &AnswerReq, launch: &Launch) -> Result<(), String> {
    let key = card.tool_call_id.as_deref().ok_or("ask card has no question")?;
    let answers = if req.answers.is_empty() {
        vec![Answer {
            option: req.option,
            text: req.text.clone(),
        }]
    } else {
        req.answers.clone()
    };
    let text = spoken(card, &answers)?;
    let delivered = match open_question(launch, sid, key) {
        Some((client, open)) => {
            let content = content_of(&open.schema, &answers)?;
            sent(client.respond(sid, &open.rpc_id, json!({"action": "accept", "content": content})))?
        }
        None => false,
    };
    let (option, typed) = match answers.as_slice() {
        [only] => (only.option, only.text.clone()),
        _ => (None, None),
    };
    let said = Said {
        key: key.to_owned(),
        text,
        queued: (!delivered).then_some(Queued::Ask(answers)),
    };
    remember(db, card, sid, said, |_| {})?;
    let reason = if delivered { "sent to ompsdk" } else { "queued for ompsdk" };
    if record_swipe(db, &card.id, "done", option, typed, req.note.clone(), Some(reason.to_owned())) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Answer a plan review with Refine. The feedback goes into the running turn, or into a new
/// prompt when no turn takes it.
fn refine(client: &Client, sid: &str, open: &Elicitation, feedback: &str) -> Result<(), String> {
    client.respond(
        sid,
        &open.rpc_id,
        json!({"action": "accept", "content": {"value": PLAN_REFINE}}),
    )?;
    if client.steer(sid, feedback)? != "injected" {
        client.prompt(sid, prompt_blocks(feedback, None))?;
    }
    Ok(())
}

/// Answer an ompsdk plan review. Approve sets the executor first. The plan then runs on it. A
/// review that is out of reach keeps the answer, as `answer_ask` does.
pub(super) fn answer_plan(
    db: &Db,
    card: &Card,
    sid: &str,
    req: &AnswerReq,
    choice: super::dsh_track::PlanChoice<'_>,
    launch: &Launch,
) -> Result<(), String> {
    let super::dsh_track::PlanChoice { option, refinement, executor, executor_effort } = choice;
    let key = card.tool_call_id.as_deref().ok_or("plan card has no review")?;
    let feedback = match option {
        Some(0) => None,
        _ => Some(refinement.ok_or("Refine requires nonempty text")?),
    };
    let delivered = match (open_question(launch, sid, key), feedback) {
        (Some((client, open)), None) => sent(approve(&client, sid, &open, executor, executor_effort))?,
        (Some((client, open)), Some(feedback)) => sent(refine(&client, sid, &open, feedback))?,
        (None, _) => false,
    };
    let said = match feedback {
        None => Said {
            key: key.to_owned(),
            text: "The plan is approved. Carry it out now.".to_owned(),
            queued: (!delivered).then_some(Queued::Approve),
        },
        Some(feedback) => Said {
            key: key.to_owned(),
            text: format!("Refine the plan: {feedback}"),
            queued: (!delivered).then(|| Queued::Refine(feedback.to_owned())),
        },
    };
    let ran_on = executor.map(|model| PlanInto {
        model: Some(model.to_owned()),
        thinking: executor_effort.map(str::to_owned),
    });
    remember(db, card, sid, said, |entry| {
        if feedback.is_none() {
            entry.planning = false;
            entry.executor = ran_on;
        }
    })?;
    let text = feedback.or(executor).map(str::to_owned);
    let reason = if delivered { "sent to ompsdk" } else { "queued for ompsdk" };
    if record_swipe(db, &card.id, "done", option, text, req.note.clone(), Some(reason.to_owned())) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Close an ompsdk session and resolve the cards it still has open. The close returns at
/// once. The poll tells the daemon, and waits before each further try when the pool does not answer.
pub(super) fn close(db: &Db, card: &Card, sid: &str, note: Option<&str>) -> Result<(), String> {
    let open = db.unresolved_cards().map_err(|error| error.to_string())?;
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
    edit(db, |state| {
        // A session the daemon no longer holds has nothing to release.
        state.sessions.retain(|entry| entry.sid != sid || entry.loaded);
        if let Some(entry) = state.sessions.iter_mut().find(|entry| entry.sid == sid) {
            entry.closing = true;
            entry.forget = true;
        }
    })?;
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
    let entry = adopt(db, card, sid)?;
    let request = report_request(db)?;
    retrying(|| {
        let client = ompsdk::client(socket(launch)?)?;
        if bind(db, &client, &entry)? || !client.elicitations(sid).is_empty() {
            return Err("session is busy".to_owned());
        }
        client.prompt(sid, prompt_blocks(&request, None))
    })?;
    update(db, sid, |entry| {
        entry.loaded = true;
        entry.cut = false;
        entry.parked = false;
        entry.closing = false;
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

/// Hand an ompsdk session to a tmux window that runs `omp --resume` on its transcript. A turn
/// that was still running, or that a restart cut, resumes with the cut prompt. The work then
/// goes on where it stopped.
pub(super) fn open_cli(db: &Db, card: &Card, sid: &str, launch: &Launch) -> Result<(), String> {
    let entry = match lookup(db, sid) {
        Some(entry) => entry,
        None => rebuilt(card, sid)?,
    };
    let mut running = entry.cut;
    if entry.loaded {
        running |= retrying(|| {
            let client = ompsdk::client(socket(launch)?)?;
            let running = bind(db, &client, &entry)?;
            client.close(sid)?;
            Ok(running)
        })?;
        // The daemon no longer holds it. A refusal below must not leave the poll resuming it.
        update(db, sid, |entry| {
            entry.loaded = false;
            entry.cut = false;
            entry.parked = false;
            entry.closing = false;
        })?;
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
