//! Bridge poll and card actions for dsh sessions.

use super::{
    GOAL_CONTINUATION_GRACE, Launch, REQUEST_REPORT_TEXT, Start, TickReport, Tracker, finish_cards,
    headline_of, record_swipe, reply_turn, supersede_info_cards, trail, truncate_chars, turn_card,
};
use crate::db::Db;
use crate::dsh;
use crate::events::{self, Answer, AnswerReq, Card, CardOption, CardQuestion, Kind, Source, Star};
use crate::omp;
use crate::uploads::Upload;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Gap between bridge polls after a good poll, in ms.
const POLL_MS: i64 = 3_000;
/// Gap between bridge polls after a failed poll, in ms.
const BACKOFF_MS: i64 = 30_000;
/// Event reads one poll may spend on report cards.
const REPORT_READS: usize = 8;
/// Sessions idle for longer than this, in ms, are watched only while they run.
const WATCH_MS: i64 = 24 * 60 * 60 * 1000;
/// How long a launch that errored keeps looking for the session it may have started.
const ADOPT_WINDOW: Duration = Duration::from_secs(30);
/// Gap between those looks.
const ADOPT_POLL: Duration = Duration::from_millis(500);
/// How long Close waits for dsh to answer the archive. dsh finishes an archive it was asked for
/// even after the caller gives up, so a slow answer resolves the card while the archive finishes.
const ARCHIVE_GRACE: Duration = Duration::from_secs(2);
/// Caps the archive request. A failure after the grace has no caller left and is logged.
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(120);

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
        // The cheap read first: `session/list` computes a projection per session and can take
        // ten seconds, so only this one tells a dead host from a busy one.
        let pending = match client.pending() {
            Ok((_, pending)) => pending,
            Err(error) => {
                backoff(&mut self.dsh, now, &error);
                return TickReport { written };
            }
        };
        let (pid, sessions, plan_decisions) = match client.sessions() {
            Ok(listed) => listed,
            // A slow or failed list skips this tick, not the next thirty seconds of cards.
            Err(error) => {
                list_failed(&mut self.dsh, now, &error);
                return TickReport { written };
            }
        };
        self.dsh.failing = false;
        self.dsh.retry_at_ms = now + POLL_MS;
        // A session of an ignored workspace belongs to an automated producer, so it earns no
        // ask card and no report card.
        let ignore = &self.dsh_ignore;
        let ignored: HashSet<String> = sessions
            .iter()
            .filter(|session| ignore.skips(session))
            .map(|session| session.session_id.clone())
            .collect();
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
        // A plan card waits on its session: one that left the host, archived or deleted, can no
        // longer be approved and would otherwise sit in the inbox forever.
        let live: HashSet<&str> = sessions.iter().map(|session| session.session_id.as_str()).collect();
        if !finish_gone_plan_ready(db, &live, &mut written) {
            return TickReport { written };
        }
        pending_cards(db, &client, &sessions, &pending, &ignored, &mut written);
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
            &ignored,
            plan_decisions,
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

/// Retry on the next poll after a session list that failed or timed out. `session/list`
/// projects every session, so a busy host stalls it for seconds while the bridge stays up.
fn list_failed(memo: &mut Memo, now: i64, error: &str) {
    if !memo.failing {
        eprintln!("triage: dsh bridge: session list: {error}");
    }
    memo.failing = true;
    memo.retry_at_ms = now + POLL_MS;
}

/// Queue a card for each pending dsh question or plan review that has none yet.
fn pending_cards(
    db: &Db,
    client: &dsh::Client,
    sessions: &[dsh::Session],
    pending: &[dsh::Pending],
    ignored: &HashSet<String>,
    written: &mut usize,
) {
    for item in pending {
        if ignored.contains(&item.session_id) {
            continue;
        }
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
        let heading = item.detail.as_deref().and_then(plan_heading);
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
            allow_other: true,
            star: None,
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

/// The title a plan is known by. That is its first top-level Markdown heading, the form dsh
/// asks a presented plan to start with.
fn plan_heading(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|heading| !heading.is_empty())
}

/// The card title of a turn that ended while plan mode was still on. The plan names the card;
/// a turn without one falls back to the reply's first line.
fn plan_ready_title(name: &str) -> String {
    match name.trim() {
        "" => "Plan ready".to_owned(),
        name => format!("Plan ready: {name}"),
    }
}

/// The newest message of a turn that opens with a title, which is the plan dsh left in the
/// transcript.
fn plan_message<'a>(replies: &[(i64, &'a str)], turn_start: i64) -> Option<&'a str> {
    replies
        .iter()
        .rev()
        .filter(|(seq, _)| *seq > turn_start)
        .map(|&(_, text)| text)
        .find(|text| plan_heading(text).is_some())
}

/// What that plan is called: its heading, else the report's first line, so a plan written
/// without one still names the card.
fn plan_name(plan: Option<&str>, turn: &super::Turn) -> String {
    match plan {
        Some(plan) => plan_heading(plan).map_or_else(|| headline_of(plan), str::to_owned),
        None => headline_of(turn.body.as_deref().unwrap_or_default()),
    }
}

/// The plan card of a turn that ended in plan mode. Its Approve leaves plan mode and starts the
/// work, and its Refine sends the feedback back for more planning.
fn plan_card(
    session: &dsh::Session,
    end: &dsh::Event,
    name: &str,
    plan: Option<&str>,
    report: Option<String>,
) -> Card {
    let sid = session.session_id.as_str();
    let source = dsh_source(sid, Some(session));
    let body = plan.map_or_else(|| report.unwrap_or_default(), omp::redact_credentials);
    Card {
        id: plan_ready_id(sid, end.seq),
        kind: Kind::Plan,
        priority: 1,
        created_ms: events::now_ms(),
        refs: vec![source.uri.clone()],
        source,
        title: plan_ready_title(name),
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
        body: Some(body),
        level: None,
        tool_call_id: None,
        report_due: false,
        engine: Some("dsh".to_owned()),
    }
}

/// The card id of the plan a session wrote without a review. It keys on the turn end, so a
/// later plan of the same session is its own card.
fn plan_ready_id(sid: &str, seq: i64) -> String {
    format!("plan-ready:dsh:{sid}:{seq}")
}

/// Whether `card` is the plan a session wrote without a review.
fn is_plan_ready(card: &Card) -> bool {
    plan_ready_session(card).is_some()
}

/// The session a plan-ready card belongs to.
fn plan_ready_session(card: &Card) -> Option<&str> {
    card.id
        .strip_prefix("plan-ready:dsh:")
        .and_then(|rest| rest.rsplit_once(':'))
        .map(|(sid, _)| sid)
}

/// Close the queued plan cards whose session the host no longer lists. False when a read or
/// write failed, so the caller tries again on its next poll.
fn finish_gone_plan_ready(db: &Db, live: &HashSet<&str>, written: &mut usize) -> bool {
    let Ok(open) = db.unresolved_cards() else {
        return false;
    };
    let cards = open
        .into_iter()
        .filter(|card| plan_ready_session(card).is_some_and(|sid| !live.contains(sid)))
        .collect();
    finish_cards(db, cards, "session closed", written)
}

/// Close the queued plan cards of the session a later turn moved past. False when a read or
/// write failed, so the caller tries again on its next poll.
fn finish_plan_ready(db: &Db, session: &str, written: &mut usize) -> bool {
    let Ok(open) = db.unresolved_cards() else {
        return false;
    };
    let cards = open
        .into_iter()
        .filter(|card| plan_ready_session(card) == Some(session))
        .collect();
    finish_cards(db, cards, "superseded", written)
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

/// Queue a report card for the newest finished turn of each idle dsh session. A turn that wrote
/// no reply of its own earns no card unless it failed. A session is read only when its cursor
/// moved. A poll reads at most `REPORT_READS` sessions. An event read that fails stops the poll.
fn report_sessions(
    memo: &mut Memo,
    db: &Db,
    client: &dsh::Client,
    sessions: &[dsh::Session],
    waiting: &HashSet<&str>,
    ignored: &HashSet<String>,
    plan_decisions: bool,
    now: i64,
    written: &mut usize,
) -> Result<(), String> {
    let mut reads = REPORT_READS;
    for session in sessions.iter().filter(|session| {
        !ignored.contains(&session.session_id)
            && !session.blank
            && session.parent_session_id.is_none()
            && (session.running || now - session.updated_at_ms <= WATCH_MS)
    }) {
        let entry = memo.sessions.entry(session.session_id.clone()).or_default();
        // A completed goal can leave running set after its last turn. The cursor check below rejects earlier rounds.
        if (session.running && session.goal_phase.as_deref() != Some("complete"))
            || waiting.contains(session.session_id.as_str())
        {
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
        let reported = entry.reported_seq;
        let Some(end) = recent
            .iter()
            .rev()
            .find(|event| event.kind == "turn/end" && event.seq > reported)
        else {
            entry.cursor = session.cursor;
            entry.waiting_grace = false;
            continue;
        };
        if session.running && session.cursor != end.seq {
            continue;
        }
        entry.cursor = session.cursor;
        entry.waiting_grace = false;
        if matches!(
            end.reason.as_deref(),
            Some("aborted" | "interrupted" | "forked")
        ) {
            entry.reported_seq = end.seq;
            continue;
        }
        let Some(card) = report_card(db, session, &recent, end, plan_decisions) else {
            entry.reported_seq = end.seq;
            continue;
        };
        if matches!(db.card(&card.id), Ok(Some(_))) {
            entry.reported_seq = end.seq;
            continue;
        }
        if !supersede_info_cards(db, &card.id, &card.refs[0], None, written) {
            continue;
        }
        // A turn after a plan card means the plan was carried out, and a newer plan is the card
        // that matters, so the older plan card is spent either way.
        if !finish_plan_ready(db, &session.session_id, written) {
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
/// line in that turn, else the newest reply. `None` when the turn wrote no reply of its own and
/// did not fail: dsh ends such a turn after an archive, and carding it would repeat the reply
/// the user just closed.
fn report_card(
    db: &Db,
    session: &dsh::Session,
    recent: &[dsh::Event],
    end: &dsh::Event,
    plan_decisions: bool,
) -> Option<Card> {
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
    let replied_now = replies.iter().any(|(seq, _)| *seq > turn_start);
    if !replied_now && reason != "error" {
        return None;
    }
    let replied = !reply.trim().is_empty();
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
    if replied {
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
    // Plan mode means no work ran. The turn wrote a plan and left it for the user to approve.
    // A report it ends with is not a finished result. The card says what waits instead.
    let plan = plan_message(&replies, turn_start);
    if session.plan_active && reason == "completed" && replied {
        let name = plan_name(plan, &turn);
        // A host that decides plans turns the turn into the plan card whose Approve leaves
        // plan mode. A host older than that route can only report, so the plan stays a report
        // that names what waits for you.
        if plan_decisions {
            return Some(plan_card(session, end, &name, plan, turn.body));
        }
        turn.title = plan_ready_title(&name);
        if let Some(plan) = plan {
            turn.body = Some(omp::redact_credentials(plan));
        }
        turn.action.clear();
        turn.report_due = false;
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
    Some(card)
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

/// The prompt a new dsh session gets. It opens with the task's name, which gives dsh's
/// session titler the work itself instead of a brief path. dsh names a session from its
/// first `user` message. A plan launch records this text as one. An empty `name` gives the
/// plain brief prompt.
fn launch_prompt(brief: &Path, name: &str) -> String {
    let name = title_words(name, 120, 240);
    if name.is_empty() {
        return brief_prompt(brief);
    }
    format!("{name}: Read the brief at {} and carry it out.", brief.display())
}

/// One line of a title. A card title cannot break the launch prompt, which the bridge
/// accepts only as one line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` cut to at most `chars` characters and `bytes` bytes on a word boundary. A single
/// overlong word is cut at a character boundary, never inside one. It carries no ellipsis,
/// because this is a name and not a clipped preview.
fn title_words(text: &str, chars: usize, bytes: usize) -> String {
    let text = one_line(text);
    let fits = |candidate: &str| candidate.chars().count() <= chars && candidate.len() <= bytes;
    if fits(&text) {
        return text;
    }
    let mut cut = String::new();
    for word in text.split(' ') {
        let candidate = if cut.is_empty() { word.to_owned() } else { format!("{cut} {word}") };
        if fits(&candidate) {
            cut = candidate;
            continue;
        }
        if cut.is_empty() {
            cut = word.chars().take(chars).collect();
            while !cut.is_empty() && cut.len() > bytes {
                cut.pop();
            }
        }
        break;
    }
    cut
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
    client.answer(&item.event_id, &json!({"answers": entries}), None, None)?;
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
    executor_effort: Option<&str>,
    launch: &Launch,
) -> Result<(), String> {
    let sid = dsh::session_id(card).ok_or_else(|| "card is not a dsh session".to_owned())?;
    let client = dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)?;
    // A plan written without a review waits on no pending item: the host leaves plan mode and
    // starts the work, or keeps planning with the feedback.
    if is_plan_ready(card) {
        return decide_plan(
            db,
            card,
            &client,
            sid,
            req,
            option,
            refinement,
            executor,
            executor_effort,
        );
    }
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
    client.answer(&item.event_id, &value, model, executor_effort)?;
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
/// Approve or refine a plan the session wrote as text. Approve leaves plan mode with the
/// execution model and starts the plan; Refine keeps planning with the feedback.
fn decide_plan(
    db: &Db,
    card: &Card,
    client: &dsh::Client,
    sid: &str,
    req: &AnswerReq,
    option: Option<usize>,
    refinement: Option<&str>,
    executor: Option<&str>,
    executor_effort: Option<&str>,
) -> Result<(), String> {
    let (decision, feedback, text) = if option == Some(0) {
        ("approve", None, executor.map(str::to_owned))
    } else {
        let feedback = refinement.ok_or_else(|| "Refine requires nonempty text".to_owned())?;
        ("refine", Some(feedback), Some(feedback.to_owned()))
    };
    client.decide_plan(sid, decision, feedback, executor, executor_effort)?;
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

/// Asks dsh to archive `sid` and waits at most ARCHIVE_GRACE for the answer. A failure inside the
/// grace is returned, so the card stays queued. A slower answer lets the card resolve, and a failure
/// after that is logged, because the worker has no caller left to return it to.
fn archive_within_grace(launch: &Launch, sid: &str) -> Result<(), String> {
    let host = launch.dsh_host.clone();
    let owned = sid.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = dsh::connect(host.as_deref(), ARCHIVE_TIMEOUT).and_then(|client| client.archive(&owned));
        if let Err(std::sync::mpsc::SendError(Err(error))) = tx.send(result) {
            eprintln!("triage: dsh archive of {owned} failed after the card closed: {error}");
        }
    });
    match rx.recv_timeout(ARCHIVE_GRACE) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(()),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("dsh archive stopped".to_owned()),
    }
}

/// Archive a dsh session and stop its activity before resolving its open cards.
pub(super) fn close(
    db: &Db,
    card: &Card,
    sid: &str,
    note: Option<&str>,
    launch: &Launch,
) -> Result<(), String> {
    let reason = format!("closed dsh:{sid}");
    let open = db.unresolved_cards().map_err(|error| error.to_string())?;
    archive_within_grace(launch, sid)?;
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
    client.prompt(sid, REQUEST_REPORT_TEXT, None)?;
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

/// Give the brief to the card's dsh session, and start a session only when that one cannot take
/// it. A goal brief is queued in the session whatever it is doing, and a plan brief relaunches
/// it in plan mode while it is idle. A session the host no longer lists, or a prompt the bridge
/// refuses, starts a session of its own.
pub(super) fn launch_brief(
    card: &Card,
    launch: &Launch,
    brief: &Path,
    start: Start,
    image: Option<&Upload>,
) -> Result<String, String> {
    if let Some(sid) = dsh::session_id(card) {
        let client = dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)?;
        if let Ok(state) = client.state(sid) {
            match start {
                // A goal brief is queued in the card's session even while it runs or waits for
                // an answer, because dsh queues a prompt for the session's next turn. The work
                // then continues where the card came from instead of in a second session.
                Start::Goal => {
                    match client.prompt(sid, &brief_prompt(brief), image.map(Upload::dsh_image)) {
                        Ok(()) => return Ok(format!("dsh:{sid}")),
                        // A refused prompt was not taken, so the brief starts a session instead.
                        Err(error) if prompt_refused(&error) => {}
                        Err(error) => return Err(error),
                    }
                }
                // A plan sets the session's permission and plan mode with slash commands, which
                // take effect between turns. A busy session cannot take one, so its brief
                // starts a session of its own instead.
                Start::Plan | Start::PlanInto(..) if !state.running && state.pending_input.is_empty() => {
                    let (mode, executor, executor_effort) = match start {
                        Start::PlanInto(executor, effort) => ("plan-into", executor, effort),
                        _ => ("plan", None, None),
                    };
                    let mut body = json!({
                        "sessionId": sid,
                        "cwd": card_cwd(card),
                        "prompt": brief_prompt(brief),
                        "mode": mode,
                        "executor": executor,
                        "executorEffort": executor_effort,
                    });
                    if let Some(image) = image {
                        body["image"] = image.dsh_image();
                    }
                    return client.launch(&body).map(|launched| format!("dsh:{launched}"));
                }
                Start::Plan | Start::PlanInto(..) => {}
            }
        }
    }
    launch_new(launch, &card_cwd(card), brief, &card.title, start, image)
}

/// Whether the bridge refused a prompt without taking it. A timeout or an unreachable host is
/// not a refusal: the bridge may have queued the prompt before its answer went missing, and a
/// second session on the same brief would do the work twice.
fn prompt_refused(error: &str) -> bool {
    error.contains("session is busy") || error.contains("session not found")
}

/// Start a dsh session on `brief` and return its `dsh:<sid>` pane id.
///
/// A launch that errors may still have started its session: the bridge finishes a launch
/// after the caller gives up, and a host that is only slow created the session first. The
/// session the launch started is adopted rather than reported as a failure, because the
/// retry would otherwise start a second session on the same brief.
pub(super) fn launch_new(
    launch: &Launch,
    cwd: &str,
    brief: &Path,
    task: &str,
    start: Start,
    image: Option<&Upload>,
) -> Result<String, String> {
    let client = dsh::connect(launch.dsh_host.as_deref(), dsh::LAUNCH_TIMEOUT)?;
    let name = one_line(task);
    let (mode, executor, executor_effort) = match start {
        Start::Goal => ("goal", None, None),
        Start::Plan => ("plan", None, None),
        Start::PlanInto(executor, effort) => ("plan-into", executor, effort),
    };
    let mut body = json!({
        "prompt": launch_prompt(brief, &name),
        // No model leaves the bridge its host's default from the dsh catalog.
        "model": launch.model.as_deref(),
        "effort": launch.thinking,
        "mode": mode,
        "executor": executor,
        "executorEffort": executor_effort,
    });
    // dsh names a session from its first `user` message. A plan launch records the brief
    // prompt as one. Since a goal launch only creates a goal, whose rounds are not user
    // messages, dsh has nothing to name it from. Only a goal launch keeps this title.
    let pinned = match start {
        Start::Goal => title_words(&name, 60, 80),
        _ => String::new(),
    };
    if !pinned.is_empty() {
        body["title"] = json!(pinned);
    }
    if let Some(workspace) = launch.dsh_workspace.as_deref() {
        body["workspace"] = json!(workspace);
    } else {
        body["cwd"] = json!(cwd);
    }
    if let Some(image) = image {
        body["image"] = image.dsh_image();
    }
    // The session this launch creates is newer than this, so adoption never needs a list of
    // the sessions that were already there, which costs a slow `session/list` round trip.
    let started_at = crate::events::now_ms();
    let pinned = (!pinned.is_empty()).then_some(pinned.as_str());
    let dir = launch_dir(launch, cwd);
    match client.launch(&body) {
        Ok(sid) => Ok(format!("dsh:{sid}")),
        // A host older than workspaces rejects the field before it starts anything, so the
        // same brief starts in a session of its own directory instead of failing.
        Err(error) if launch.dsh_workspace.is_some() && error.contains("cwd") => {
            if let Some(fields) = body.as_object_mut() {
                fields.remove("workspace");
            }
            body["cwd"] = json!(cwd);
            match client.launch(&body) {
                Ok(sid) => Ok(format!("dsh:{sid}")),
                Err(error) => adopt_started(&client, started_at, pinned, Path::new(cwd), error),
            }
        }
        Err(error) => adopt_started(&client, started_at, pinned, &dir, error),
    }
}

/// The directory a new session starts in. That is the workspace's own directory when the
/// launch joins a workspace, else the project directory.
pub(super) fn launch_dir(launch: &Launch, cwd: &str) -> PathBuf {
    match (launch.dsh_workspaces.as_deref(), launch.dsh_workspace.as_deref()) {
        (Some(root), Some(name)) => root.join(name),
        _ => PathBuf::from(cwd),
    }
}

/// The session a launch that timed out started, or the launch's own error. The bridge
/// keeps working on a launch after its caller gives up, so a timeout is not a failure until
/// the host has had a while to list the session. Any other error is the bridge's own
/// answer, and it archives the session it created before answering.
pub(super) fn adopt_started(
    client: &dsh::Client,
    started_at: i64,
    pinned: Option<&str>,
    dir: &Path,
    error: String,
) -> Result<String, String> {
    if !error.contains("timeout") {
        return Err(error);
    }
    let deadline = Instant::now() + ADOPT_WINDOW;
    loop {
        if let Ok((_, sessions, _)) = client.sessions() {
            // A session created by this launch is newer than it. A second of slack covers a
            // clock that lands just under the recorded start. It starts in the launch's
            // directory. A launch that named a title shows that title too. Only one session
            // may answer, because guessing would hand the card someone else's work.
            let mut matches = sessions.iter().filter(|session| {
                session.updated_at_ms >= started_at - 1000
                    && session.cwd.as_deref().is_none_or(|cwd| dsh::same_path(Path::new(cwd), dir))
                    && pinned.is_none_or(|title| session.title.as_deref() == Some(title))
            });
            let found = matches.next();
            if found.is_some() && matches.next().is_none()
                && let Some(session) = found
            {
                return Ok(format!("dsh:{}", session.session_id));
            }
        }
        if Instant::now() >= deadline {
            return Err(error);
        }
        std::thread::sleep(ADOPT_POLL);
    }
}
