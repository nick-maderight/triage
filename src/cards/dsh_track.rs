//! Bridge poll and card actions for dsh sessions.

use super::{
    GOAL_CONTINUATION_GRACE, Launch, REQUEST_REPORT_TEXT, Start, TickReport, Tracker, finish_cards,
    record_swipe, reply_turn, supersede_info_cards, trail, truncate_chars, turn_card,
};
use crate::db::Db;
use crate::dsh;
use crate::events::{self, Answer, AnswerReq, Card, CardOption, CardQuestion, Kind, Source, Star};
use crate::omp;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

/// Gap between bridge polls after a good poll, in ms.
const POLL_MS: i64 = 3_000;
/// Gap between bridge polls after a failed poll, in ms.
const BACKOFF_MS: i64 = 30_000;
/// Event reads one poll may spend on report cards.
const REPORT_READS: usize = 8;
/// Sessions idle for longer than this, in ms, are watched only while they run.
const WATCH_MS: i64 = 24 * 60 * 60 * 1000;

/// What the bridge poll remembers between polls.
#[derive(Default)]
pub(super) struct Memo {
    /// Host pid of the last good poll. A new pid means the host restarted.
    pid: Option<u32>,
    retry_at_ms: i64,
    failing: bool,
    sessions: HashMap<String, SessionMemo>,
}

/// What one session's last read left behind.
#[derive(Default)]
struct SessionMemo {
    /// Session cursor at the last event read.
    cursor: i64,
    /// Seq of the newest turn end that already has its card or never gets one.
    reported_seq: i64,
    idle_since_ms: Option<i64>,
    /// A goal inside its continuation grace. The next poll reads it again.
    waiting_grace: bool,
}

impl Tracker {
    /// Poll the dsh bridge and return the number of cards written.
    pub fn dsh_tick(&mut self, db: &Db, host_file: Option<&Path>) -> TickReport {
        self.dsh_tick_at(db, host_file, events::now_ms())
    }

    /// `dsh_tick` at `now`, in ms since the epoch.
    pub(super) fn dsh_tick_at(
        &mut self,
        db: &Db,
        host_file: Option<&Path>,
        now: i64,
    ) -> TickReport {
        let mut written = 0;
        let Some(host) = host_file
            .filter(|_| now >= self.dsh.retry_at_ms)
            .and_then(dsh::Host::load)
        else {
            return TickReport { written };
        };
        let client = dsh::Client::new(&host, dsh::TICK_TIMEOUT);
        let listed = client.sessions().and_then(|(pid, sessions)| {
            client
                .pending()
                .map(|(_, pending)| (pid, sessions, pending))
        });
        let (pid, sessions, pending) = match listed {
            Ok(listed) => listed,
            Err(error) => {
                backoff(&mut self.dsh, now, &error);
                return TickReport { written };
            }
        };
        self.dsh.failing = false;
        self.dsh.retry_at_ms = now + POLL_MS;
        let answered: HashSet<&str> = pending.iter().map(|item| item.event_id.as_str()).collect();
        let gone = |card: &Card| {
            card.tool_call_id
                .as_deref()
                .is_none_or(|id| !answered.contains(id))
        };
        if self.dsh.pid.is_some_and(|old| old != pid) {
            if !finish_prompts(db, "dsh host restarted", gone, &mut written) {
                return TickReport { written };
            }
            self.dsh.sessions.clear();
        }
        self.dsh.pid = Some(pid);
        pending_cards(db, &client, &sessions, &pending, &mut written);
        finish_prompts(db, "answered in dsh", gone, &mut written);
        let waiting: HashSet<&str> = pending
            .iter()
            .map(|item| item.session_id.as_str())
            .collect();
        if let Err(error) = report_sessions(
            &mut self.dsh,
            db,
            &client,
            &sessions,
            &waiting,
            now,
            &mut written,
        ) {
            backoff(&mut self.dsh, now, &error);
        }
        TickReport { written }
    }
}

/// Wait `BACKOFF_MS` before the next poll. The error prints only on the first failure in a row.
fn backoff(memo: &mut Memo, now: i64, error: &str) {
    if !memo.failing {
        eprintln!("triage: dsh bridge: {error}");
    }
    memo.failing = true;
    memo.retry_at_ms = now + BACKOFF_MS;
}

/// Queue a card for each pending dsh question or plan review that has none yet.
fn pending_cards(
    db: &Db,
    client: &dsh::Client,
    sessions: &[dsh::Session],
    pending: &[dsh::Pending],
    written: &mut usize,
) {
    for item in pending {
        if matches!(db.card(&prompt_id(item)), Ok(Some(_))) {
            continue;
        }
        let session = sessions
            .iter()
            .find(|session| session.session_id == item.session_id);
        let Some(card) = prompt_card(client, session, item) else {
            continue;
        };
        let Ok(inserted) = db.insert_card(&card) else {
            continue;
        };
        *written += usize::from(inserted);
        if inserted && card.kind == Kind::Plan {
            supersede_plans(db, &card, written);
        }
    }
}

/// Supersede the other open plan reviews of the card's session.
fn supersede_plans(db: &Db, card: &Card, written: &mut usize) {
    let Ok(open) = db.unresolved_cards() else {
        return;
    };
    let older = open
        .into_iter()
        .filter(|other| {
            other.kind == Kind::Plan && other.id != card.id && other.source.uri == card.source.uri
        })
        .collect();
    finish_cards(db, older, "superseded", written);
}

/// The card id of a pending item. A plan review keys on its call, a question on its event.
fn prompt_id(item: &dsh::Pending) -> String {
    if item.kind == "plan-review" {
        let call = if item.call_id.is_empty() {
            &item.event_id
        } else {
            &item.call_id
        };
        format!("plan:dsh:{}:{call}", item.session_id)
    } else {
        format!("ask:dsh:{}:{}", item.session_id, item.event_id)
    }
}

/// The card for a pending dsh question or plan review.
fn prompt_card(
    client: &dsh::Client,
    session: Option<&dsh::Session>,
    item: &dsh::Pending,
) -> Option<Card> {
    let source = dsh_source(&item.session_id, session);
    let refs = vec![source.uri.clone()];
    if item.kind == "plan-review" {
        let heading = item
            .detail
            .as_deref()
            .and_then(|detail| detail.lines().find_map(|line| line.strip_prefix("# ")))
            .map(str::trim)
            .filter(|heading| !heading.is_empty());
        let name = heading.map_or_else(|| dsh_name(&item.session_id, session), str::to_owned);
        return Some(Card {
            id: prompt_id(item),
            kind: Kind::Plan,
            priority: 1,
            created_ms: events::now_ms(),
            source,
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
            body: item.detail.as_deref().map(omp::redact),
            refs,
            level: None,
            tool_call_id: Some(item.event_id.clone()),
            report_due: false,
            engine: Some("dsh".to_owned()),
        });
    }
    let question = item.questions.first()?;
    let situation = client
        .events(&item.session_id, 50)
        .ok()
        .and_then(|recent| last_reply(&recent).map(|text| truncate_chars(omp::redact(text), 1200)))
        .unwrap_or_default();
    Some(Card {
        id: prompt_id(item),
        kind: Kind::Ask,
        priority: 1,
        created_ms: events::now_ms(),
        source,
        title: question.question.clone(),
        star: Star {
            situation,
            task: question.question.clone(),
            ..Star::default()
        },
        options: question_options(question),
        answerable: item.questions.iter().all(|asked| !asked.multi_select),
        allow_other: true,
        questions: if item.questions.len() > 1 {
            card_questions(item)
        } else {
            Vec::new()
        },
        body: None,
        refs,
        level: None,
        tool_call_id: Some(item.event_id.clone()),
        report_due: false,
        engine: Some("dsh".to_owned()),
    })
}

/// Each question of a multi-question ask, named by its header. The id names it when there is no
/// header.
fn card_questions(item: &dsh::Pending) -> Vec<CardQuestion> {
    item.questions
        .iter()
        .map(|question| CardQuestion {
            label: question
                .header
                .as_deref()
                .map(str::trim)
                .filter(|header| !header.is_empty())
                .unwrap_or(&question.id)
                .to_owned(),
            question: question.question.clone(),
            options: question_options(question),
        })
        .collect()
}

fn question_options(question: &dsh::Question) -> Vec<CardOption> {
    question
        .options
        .iter()
        .map(|option| CardOption {
            label: option.label.clone(),
            description: option.description.clone(),
            ..CardOption::default()
        })
        .collect()
}

/// The newest assistant text in `recent` that is not blank. `recent` is oldest first.
fn last_reply(recent: &[dsh::Event]) -> Option<&str> {
    recent
        .iter()
        .rev()
        .filter(|event| event.kind == "assistant/message")
        .find_map(|event| event.text.as_deref().filter(|text| !text.trim().is_empty()))
}

/// Resolve the open dsh questions and plan reviews that `stale` selects. Returns false after a
/// failed read or write.
fn finish_prompts(
    db: &Db,
    reason: &str,
    stale: impl Fn(&Card) -> bool,
    written: &mut usize,
) -> bool {
    let Ok(open) = db.unresolved_cards() else {
        return false;
    };
    let cards = open
        .into_iter()
        .filter(|card| {
            (card.id.starts_with("ask:dsh:") || card.id.starts_with("plan:dsh:")) && stale(card)
        })
        .collect();
    finish_cards(db, cards, reason, written)
}

/// Queue a report card for the newest finished turn of each idle dsh session. A session is
/// read only when its cursor moved. A poll reads at most `REPORT_READS` sessions. An event
/// read that fails stops the poll.
fn report_sessions(
    memo: &mut Memo,
    db: &Db,
    client: &dsh::Client,
    sessions: &[dsh::Session],
    waiting: &HashSet<&str>,
    now: i64,
    written: &mut usize,
) -> Result<(), String> {
    let mut reads = REPORT_READS;
    for session in sessions.iter().filter(|session| {
        !session.blank
            && session.parent_session_id.is_none()
            && (session.running || now - session.updated_at_ms <= WATCH_MS)
    }) {
        let entry = memo.sessions.entry(session.session_id.clone()).or_default();
        if session.running || waiting.contains(session.session_id.as_str()) {
            entry.idle_since_ms = None;
            continue;
        }
        let idle_since = *entry.idle_since_ms.get_or_insert(now);
        if session.cursor == entry.cursor && !entry.waiting_grace {
            continue;
        }
        let idle = Duration::from_millis(u64::try_from(now - idle_since).unwrap_or_default());
        if session.goal_phase.as_deref() == Some("active") && idle < GOAL_CONTINUATION_GRACE {
            entry.waiting_grace = true;
            continue;
        }
        if reads == 0 {
            break;
        }
        reads -= 1;
        let recent = client.events(&session.session_id, 200)?;
        entry.cursor = session.cursor;
        entry.waiting_grace = false;
        let reported = entry.reported_seq;
        let Some(end) = recent
            .iter()
            .rev()
            .find(|event| event.kind == "turn/end" && event.seq > reported)
        else {
            continue;
        };
        if matches!(
            end.reason.as_deref(),
            Some("aborted" | "interrupted" | "forked")
        ) {
            entry.reported_seq = end.seq;
            continue;
        }
        let card = report_card(db, session, &recent, end);
        if matches!(db.card(&card.id), Ok(Some(_))) {
            entry.reported_seq = end.seq;
            continue;
        }
        if !supersede_info_cards(db, &card.id, &card.refs[0], None, written) {
            continue;
        }
        if let Ok(inserted) = db.insert_card(&card) {
            *written += usize::from(inserted);
            entry.reported_seq = end.seq;
        }
    }
    Ok(())
}

/// The info card for the turn that ended at `end`. Its body is the newest reply with a status
/// line in that turn, else the newest reply.
fn report_card(db: &Db, session: &dsh::Session, recent: &[dsh::Event], end: &dsh::Event) -> Card {
    let sid = session.session_id.as_str();
    let turn_start = recent
        .iter()
        .filter(|event| event.kind == "turn/end" && event.seq < end.seq)
        .map(|event| event.seq)
        .max()
        .unwrap_or_default();
    let replies: Vec<(i64, &str)> = recent
        .iter()
        .filter(|event| event.kind == "assistant/message" && event.seq < end.seq)
        .filter_map(|event| {
            let text = event
                .text
                .as_deref()
                .filter(|text| !text.trim().is_empty())?;
            Some((event.seq, text))
        })
        .collect();
    let status = replies.iter().rev().find(|(seq, text)| {
        *seq > turn_start && text.lines().any(|line| line.starts_with("ENVOY_STATUS:"))
    });
    let reply = status
        .or(replies.last())
        .map_or_else(String::new, |&(_, text)| text.to_owned());
    let reason = end.reason.as_deref().unwrap_or("completed");
    let mut parts = Vec::new();
    if reason == "error" {
        parts.push(
            match end
                .error
                .as_deref()
                .map(str::trim)
                .filter(|error| !error.is_empty())
            {
                Some(error) => format!("Stopped with error: {error}"),
                None => "Stopped with error".to_owned(),
            },
        );
    }
    if !reply.is_empty() {
        parts.push(reply);
    }
    let text = if parts.is_empty() {
        format!("Turn ended: {reason}")
    } else {
        parts.join("\n\n")
    };
    let (star, investigation) = trail(db, &format!("dsh:{sid}"), 0);
    let mut turn = reply_turn(text, None, !star.situation.is_empty(), investigation);
    if reason == "error" {
        turn.report_due = false;
        turn.action.clear();
    }
    if matches!(reason, "error" | "blocked" | "max-tokens") {
        turn.level = "warn";
    }
    let source = dsh_source(sid, Some(session));
    let refs = vec![source.uri.clone()];
    let mut card = turn_card(
        format!("done:dsh:{sid}:{}", end.seq),
        source,
        refs,
        star,
        turn,
    );
    card.engine = Some("dsh".to_owned());
    card
}

/// The name a dsh session shows on its cards. That is its title, else the first 8 characters of
/// its id after the `session-` prefix.
fn dsh_name(sid: &str, session: Option<&dsh::Session>) -> String {
    session
        .and_then(|session| session.title.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map_or_else(
            || {
                sid.strip_prefix("session-")
                    .unwrap_or(sid)
                    .chars()
                    .take(8)
                    .collect::<String>()
            },
            str::to_owned,
        )
}

/// The card source of a dsh session. Its uri keys the poll and the card actions.
fn dsh_source(sid: &str, session: Option<&dsh::Session>) -> Source {
    Source {
        uri: format!("dsh://session/{sid}"),
        label: format!("dsh {}", dsh_name(sid, session)),
        cwd: session
            .and_then(|session| session.cwd.clone())
            .unwrap_or_default(),
        pane: None,
        target: None,
    }
}

/// The pending item that answers `card`. It sits in `sid` and has the card's tool call id as its
/// event id.
fn pending_item(
    client: &dsh::Client,
    sid: &str,
    card: &Card,
) -> Result<Option<dsh::Pending>, String> {
    let (_, pending) = client.pending()?;
    Ok(pending.into_iter().find(|item| {
        item.session_id == sid && card.tool_call_id.as_deref() == Some(item.event_id.as_str())
    }))
}

/// One question's entry in an ask answer. It is the picked option's label, or the typed text.
fn answer_entry(question: &dsh::Question, answer: &Answer) -> Result<Value, String> {
    let text = answer
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    match (answer.option, text) {
        (Some(index), _) => {
            let option = question
                .options
                .get(index)
                .ok_or_else(|| "option is out of range".to_owned())?;
            Ok(json!({"id": question.id, "selected": [option.label]}))
        }
        (None, Some(text)) => Ok(json!({"id": question.id, "selected": [], "custom": text})),
        (None, None) => Err("answer needs an option or text".to_owned()),
    }
}

/// The prompt a dsh session gets for a brief file.
fn brief_prompt(brief: &Path) -> String {
    format!("Read the brief at {} and carry it out.", brief.display())
}

/// Working directory for a dsh launch of `card`. The card's own, else the home directory.
fn card_cwd(card: &Card) -> String {
    if card.source.cwd.is_empty() {
        std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())
    } else {
        card.source.cwd.clone()
    }
}

/// Answer a dsh question with the picked option or the typed text. Multi-question asks take one
/// answer per question.
pub(super) fn answer_ask(
    db: &Db,
    card: &Card,
    sid: &str,
    req: &AnswerReq,
    launch: &Launch,
) -> Result<(), String> {
    let client = dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)?;
    let item = pending_item(&client, sid, card)?
        .ok_or_else(|| "question is no longer pending".to_owned())?;
    let answers = if req.answers.is_empty() {
        vec![Answer {
            option: req.option,
            text: req.text.clone(),
        }]
    } else {
        req.answers.clone()
    };
    if answers.len() != item.questions.len() {
        return Err("ask answer needs one answer per question".to_owned());
    }
    let entries = item
        .questions
        .iter()
        .zip(&answers)
        .map(|(question, answer)| answer_entry(question, answer))
        .collect::<Result<Vec<Value>, String>>()?;
    client.answer(&item.event_id, &json!({"answers": entries}), None)?;
    let (option, text) = match answers.as_slice() {
        [only] => (only.option, only.text.clone()),
        _ => (None, None),
    };
    if record_swipe(
        db,
        &card.id,
        "done",
        option,
        text,
        req.note.clone(),
        Some("sent to dsh".to_owned()),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Approve a dsh plan review with its execution model, or send a refinement back to planning.
pub(super) fn answer_plan(
    db: &Db,
    card: &Card,
    req: &AnswerReq,
    option: Option<usize>,
    refinement: Option<&str>,
    executor: Option<&str>,
    launch: &Launch,
) -> Result<(), String> {
    let sid = dsh::session_id(card).ok_or_else(|| "card is not a dsh session".to_owned())?;
    let client = dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)?;
    let item =
        pending_item(&client, sid, card)?.ok_or_else(|| "plan review is not open".to_owned())?;
    let question = item
        .questions
        .first()
        .ok_or_else(|| "plan review has no question".to_owned())?;
    let (value, model, text) = if option == Some(0) {
        (
            json!({"answers": [{"id": question.id, "selected": ["Approve"]}]}),
            executor,
            executor,
        )
    } else {
        let refinement = refinement.ok_or_else(|| "Refine requires nonempty text".to_owned())?;
        (
            json!({
                "answers": [{
                    "id": question.id,
                    "selected": ["Keep planning"],
                    "custom": refinement,
                }]
            }),
            None,
            Some(refinement),
        )
    };
    client.answer(&item.event_id, &value, model)?;
    if record_swipe(
        db,
        &card.id,
        "done",
        option,
        text.map(str::to_owned),
        req.note.clone(),
        Some("sent to dsh".to_owned()),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Cancel a dsh session, then resolve the card and the session's other open cards.
pub(super) fn close(
    db: &Db,
    card: &Card,
    sid: &str,
    note: Option<&str>,
    launch: &Launch,
) -> Result<(), String> {
    let reason = format!("closed dsh:{sid}");
    let open = db.unresolved_cards().map_err(|error| error.to_string())?;
    dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)?.cancel(sid)?;
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
    let others = open
        .into_iter()
        .filter(|other| other.id != card.id && other.source.uri == card.source.uri)
        .collect();
    let mut written = 0;
    finish_cards(db, others, &reason, &mut written);
    Ok(())
}

/// Ask an idle dsh session for its final report and resolve the card.
pub(super) fn request_report(
    db: &Db,
    card: &Card,
    sid: &str,
    launch: &Launch,
) -> Result<(), String> {
    let client = dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)?;
    let state = client.state(sid)?;
    if state.running || !state.pending_input.is_empty() {
        return Err("session is busy".to_owned());
    }
    client.prompt(sid, REQUEST_REPORT_TEXT)?;
    if record_swipe(
        db,
        &card.id,
        "done",
        None,
        Some("Request final report".to_owned()),
        None,
        Some(format!("report requested dsh:{sid}")),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Give the brief to the card's dsh session when it is idle, else start a session for it. A
/// plan-into dispatch starts a new session. The bridge arms auto-approval only at launch.
pub(super) fn launch_brief(
    card: &Card,
    launch: &Launch,
    brief: &Path,
    start: Start,
) -> Result<String, String> {
    if let Some(sid) = dsh::session_id(card).filter(|_| !matches!(start, Start::PlanInto(_))) {
        let client = dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)?;
        if client
            .state(sid)
            .is_ok_and(|state| !state.running && state.pending_input.is_empty())
        {
            if start == Start::Plan {
                let body = json!({
                    "sessionId": sid,
                    "cwd": card_cwd(card),
                    "prompt": brief_prompt(brief),
                    "mode": "plan",
                });
                return client
                    .launch(&body)
                    .map(|launched| format!("dsh:{launched}"));
            }
            client.prompt(sid, &brief_prompt(brief))?;
            return Ok(format!("dsh:{sid}"));
        }
    }
    launch_new(
        launch,
        &card_cwd(card),
        brief,
        &truncate_chars(card.title.clone(), 60),
        start,
    )
}

/// Start a dsh session on `brief` and return its `dsh:<sid>` pane id.
pub(super) fn launch_new(
    launch: &Launch,
    cwd: &str,
    brief: &Path,
    title: &str,
    start: Start,
) -> Result<String, String> {
    let client = dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)?;
    let (mode, executor) = match start {
        Start::Goal => ("goal", None),
        Start::Plan => ("plan", None),
        Start::PlanInto(executor) => ("plan-into", executor),
    };
    let mut body = json!({
        "prompt": brief_prompt(brief),
        "title": title,
        "model": launch.model.as_deref().unwrap_or(dsh::DEFAULT_MODEL),
        "effort": launch.thinking,
        "mode": mode,
        "executor": executor,
    });
    if let Some(workspace) = launch.dsh_workspace.as_deref() {
        body["workspace"] = json!(workspace);
    } else {
        body["cwd"] = json!(cwd);
    }
    match client.launch(&body) {
        Ok(sid) => Ok(format!("dsh:{sid}")),
        // A host older than workspaces rejects the field before it starts anything, so the
        // same brief starts in a session of its own directory instead of failing.
        Err(error) if launch.dsh_workspace.is_some() && error.contains("cwd") => {
            body.as_object_mut().map(|fields| fields.remove("workspace"));
            body["cwd"] = json!(cwd);
            client.launch(&body).map(|sid| format!("dsh:{sid}"))
        }
        Err(error) => Err(error),
    }
}
