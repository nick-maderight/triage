//! Pane tracker and card actions. Owner: tmux worker (after tmux.rs and omp.rs).
//! Binds pane snapshots to the store and executes the user's answers.

use crate::db::Db;
use crate::dsh;
use crate::events::{self, AnswerReq, Card, CardOption, CardQuestion, Kind, Source, Star, Swipe};
use crate::omp::{self, PaneState};
use crate::tmux;
use crate::uploads::Upload;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

mod dsh_track;

/// Remembers each pane's last state so only transitions produce cards.
#[derive(Default)]
pub struct Tracker {
    pub states: HashMap<String, PaneState>,
    pending_ask_ids: HashMap<String, String>,
    pending_ask_cache: HashMap<String, CachedAsk>,
    pending_proposals: HashMap<String, u32>,
    proposal_cache: HashMap<String, CachedProposals>,
    pending_panes: HashSet<String>,
    exit_scans: HashMap<String, CachedExit>,
    /// Pane id -> transcript path and length at the last info-card check that read
    /// the run as still going.
    stalled_info: HashMap<String, (PathBuf, u64)>,
    /// Pane id -> the reply an info-card check held because omp will start the next turn itself.
    held_info: HashMap<String, HeldReply>,
    /// Pane id -> transcript path and length when a capture last showed omp's wait or
    /// retry row, and when it did.
    paused_info: HashMap<String, (PathBuf, u64, Instant)>,
    /// Pane id -> last snapshot and transcript of every pane where omp itself runs, so a
    /// pane that disappears can still have its session audited.
    seen: HashMap<String, (tmux::Pane, PathBuf)>,
    /// Panes whose gone-pane close ran but whose session audit has not yet succeeded.
    /// A retried audit must not sweep the information cards its first run raised.
    audit_pending: HashSet<String>,
    /// Launch commands already relaunched once after a no-model start. A repeat start of the same
    /// command raises a card instead (see `recover_no_model_launch`).
    relaunched: HashSet<String>,
    /// Launches seen running omp with no session yet, keyed by their shell command (see
    /// `watch_startup`). A window that dies inside `STARTUP_WINDOW` with omp's SQLITE_BUSY
    /// startup crash is relaunched after a backoff (see `recover_dead_startup`).
    startup: HashMap<String, Startup>,
    /// Pane id -> the launch command its window runs, so a watched pane is claimed once.
    startup_panes: HashMap<String, String>,
    /// The host pid, retry time and per-session change marks of the dsh bridge poll.
    dsh: dsh_track::Memo,
    /// The dsh workspaces whose sessions triage does not track.
    dsh_ignore: crate::dsh::Ignore,
    #[cfg(test)]
    pending_ask_reads: usize,
    #[cfg(test)]
    proposal_reads: usize,
}

struct CachedAsk {
    path: PathBuf,
    length: u64,
    call: Option<omp::AskCall>,
}

struct CachedProposals {
    path: PathBuf,
    length: u64,
    proposals: Vec<omp::Propose>,
}

/// One audit of a pane whose omp process is gone: the transcript it ran and its
/// length, so an unchanged file is not parsed again on later polls.
struct CachedExit {
    path: PathBuf,
    length: u64,
}

/// A launch seen running omp before its session existed. `command` is the shell command a
/// relaunch repeats, `pane` the window running the current attempt (its snapshot keeps the pid
/// whose omp log names the crash), and `pending` the window a relaunch just opened, which the
/// next poll claims. `seen` is when the current attempt's window first appeared.
struct Startup {
    command: String,
    cwd: String,
    session: String,
    tries: u8,
    seen: Instant,
    pane: Option<tmux::Pane>,
    pending: Option<String>,
    due: Option<Instant>,
}

/// How long after a launch window first appears its disappearance still counts as a startup
/// death. Load can stretch omp's SQLite busy timeouts into tens of seconds before it gives up.
const STARTUP_WINDOW: Duration = Duration::from_secs(60);

/// Delays before each relaunch of a launch that died at startup. A launch that dies again after
/// the last one raises a card instead of retrying forever.
const STARTUP_BACKOFF: [u64; 4] = [1, 2, 4, 8];

/// Spread added to each delay so launches retrying together do not open windows in lockstep.
const STARTUP_JITTER_MS: u64 = 400;

/// A finished reply whose card waits because omp starts the session's next turn itself.
/// It keeps the transcript version the reply was read from, and whether the goal was
/// active then. Later polls recheck only the hold while the file stays unchanged.
struct HeldReply {
    path: PathBuf,
    length: u64,
    goal: bool,
}

/// Outcome of one tick: how many events were written. The caller broadcasts a
/// queue change when it is above zero.
pub struct TickReport {
    pub written: usize,
}

impl Tracker {
    pub fn from_db(db: &Db) -> crate::db::Result<Self> {
        let pending_panes = db.unresolved_panes()?;
        Ok(Self {
            states: HashMap::new(),
            pending_ask_ids: HashMap::new(),
            pending_ask_cache: HashMap::new(),
            pending_proposals: HashMap::new(),
            proposal_cache: HashMap::new(),
            pending_panes,
            exit_scans: HashMap::new(),
            stalled_info: HashMap::new(),
            held_info: HashMap::new(),
            paused_info: HashMap::new(),
            seen: HashMap::new(),
            audit_pending: HashSet::new(),
            relaunched: HashSet::new(),
            startup: HashMap::new(),
            startup_panes: HashMap::new(),
            dsh: dsh_track::Memo::default(),
            dsh_ignore: crate::dsh::Ignore::default(),
            #[cfg(test)]
            pending_ask_reads: 0,
            #[cfg(test)]
            proposal_reads: 0,
        })
    }

    /// Leave the sessions of the ignored dsh workspaces out of the inbox.
    pub fn set_dsh_ignore(&mut self, ignore: crate::dsh::Ignore) {
        self.dsh_ignore = ignore;
    }

    fn refresh_ask_cache(&mut self, pane: &tmux::Pane) {
        let Some(path) = omp::transcript_path(pane) else {
            self.pending_ask_cache.remove(&pane.id);
            return;
        };
        let Ok(length) = fs::metadata(&path).map(|metadata| metadata.len()) else {
            self.pending_ask_cache.remove(&pane.id);
            return;
        };
        let unchanged = self
            .pending_ask_cache
            .get(&pane.id)
            .is_some_and(|cached| cached.path == path && cached.length == length);
        if unchanged {
            return;
        }
        #[cfg(test)]
        {
            self.pending_ask_reads += 1;
        }
        let Ok(call) = omp::pending_ask(&path) else {
            self.pending_ask_cache.remove(&pane.id);
            return;
        };
        self.pending_ask_cache.insert(
            pane.id.clone(),
            CachedAsk { path, length, call },
        );
    }

    fn refresh_proposal_cache(&mut self, pane: &tmux::Pane) -> bool {
        let Some(path) = omp::transcript_path(pane) else {
            self.proposal_cache.remove(&pane.id);
            return false;
        };
        let Ok(length) = fs::metadata(&path).map(|metadata| metadata.len()) else {
            self.proposal_cache.remove(&pane.id);
            return false;
        };
        if self
            .proposal_cache
            .get(&pane.id)
            .is_some_and(|cached| cached.path == path && cached.length == length)
        {
            return false;
        }
        #[cfg(test)]
        {
            self.proposal_reads += 1;
        }
        let Ok(proposals) = omp::proposes(&path) else {
            self.proposal_cache.remove(&pane.id);
            return false;
        };
        let previous = self
            .proposal_cache
            .get(&pane.id)
            .filter(|cached| cached.path == path)
            .and_then(|cached| cached.proposals.last());
        let gained = proposals
            .last()
            .is_some_and(|proposal| previous.is_none_or(|previous| proposal != previous));
        self.proposal_cache.insert(
            pane.id.clone(),
            CachedProposals {
                path,
                length,
                proposals,
            },
        );
        gained
    }

    /// A pane whose omp process is gone while its shell survives. Audit its session
    /// once per transcript version (see [`audit_ended_session`]). `seen_live` means the
    /// tracker read this transcript while omp ran in the pane. A breadcrumb counts only
    /// for its own pane, and a reply that the user already dealt with under an earlier pane
    /// id keeps its verdict across a restart. False means retry next tick.
    #[allow(clippy::too_many_arguments)]
    fn audit_exited_pane(
        &mut self,
        db: &Db,
        pane: &tmux::Pane,
        panes: &[tmux::Pane],
        path: PathBuf,
        length: u64,
        seen_live: bool,
        written: &mut usize,
    ) -> bool {
        if self
            .exit_scans
            .get(&pane.id)
            .is_some_and(|cached| cached.path == path && cached.length == length)
        {
            return true;
        }
        if !audit_ended_session(db, pane, panes, &path, seen_live, written) {
            return false;
        }
        self.exit_scans.insert(pane.id.clone(), CachedExit { path, length });
        true
    }

    /// Audit the session of a pane that disappeared, once, from the transcript the pane
    /// last showed while omp ran in it. A pane closed from one of its cards adds nothing.
    /// False means retry next tick.
    fn audit_gone_pane(
        &self,
        db: &Db,
        pane: &str,
        panes: &[tmux::Pane],
        live: &HashSet<PathBuf>,
        written: &mut usize,
    ) -> bool {
        let Some((snapshot, path)) = self.seen.get(pane) else {
            return true;
        };
        if live.contains(path) {
            return true;
        }
        // `close` holds the pane lock from `kill-pane` until its swipe is recorded.
        let pane_mutex = pane_lock(db, pane);
        let _pane_guard = pane_mutex.lock().unwrap();
        match db.pane_closed_since(pane, events::now_ms() - 60_000) {
            Ok(true) => true,
            Ok(false) => audit_ended_session(db, snapshot, panes, path, true, written),
            Err(_) => false,
        }
    }

    /// One poll. `tmux::list_panes`, classify each omp pane, and on a transition:
    /// - `-> Asking`: read `omp::pending_ask`; insert an ask card
    ///   (id `ask:<pane>:<tool_call_id>`, answerable unless a question is multi-select,
    ///   allow_other true, options from the call, situation = last assistant text).
    /// - `Asking -> anything`: swipe every queued ask card of the pane as done,
    ///   reason `answered in pane` plus the selection from `omp::ask_result` when present.
    /// - `Working -> Waiting`: insert an info card (id `done:<pane>:<assistant entry id>`)
    ///   only when the run stopped, read from the transcript's last assistant message
    ///   (`omp::turn_end`), and swipe older queued info cards of the same transcript as done,
    ///   reason `superseded`. omp shows `π >` for every settled run and for a run that
    ///   only waits on background jobs, so the title alone cannot tell a stop. A `stop`
    ///   reply earns a card; an `error` stop earns a `Stopped with error` card with level
    ///   `error`; an `aborted` turn (an interrupt) earns none. A mid-turn message, a
    ///   working row that shows a background-job wait or auto-retry (`omp::run_paused`,
    ///   read on three captures because omp redraws the row on every spinner tick), or a
    ///   reply followed by a delivered background-job result (`async-result`) or
    ///   supervised-process exit (`launch-completion`) means the run continues. Check
    ///   again on later polls. omp drops the wait row when a job finishes, seconds before
    ///   its result reaches the transcript. A reply last seen with that row waits
    ///   `WAIT_ROW_GRACE` while the transcript stays unchanged. Replies without a status
    ///   line also wait when omp starts the next turn itself. This occurs while the session
    ///   owns a supervised process that reports its exit (`omp::service_wake_pending`), or
    ///   for `GOAL_CONTINUATION_GRACE` after a transcript update during an active goal
    ///   (`omp::goal_active`). Later polls check those holds until the transcript changes.
    ///   When the reply is
    ///   a final report (opens with a `*_STATUS: complete|agent-done|incomplete` line, see
    ///   `omp::final_report`), the card body is the report; a report written before a
    ///   background wait carries over, followed by the closing reply. A reply without a
    ///   status line is titled with its first non-empty line and has the reply as body.
    ///   A pane first seen Waiting gets the same check, so a short run whose Working
    ///   window fell between polls still reports; the card id makes the check idempotent.
    ///   A new final report from an investigation whose step moved under Action waits on
    ///   its card: the tracker never types a plan into a finished session.
    ///   A reply keeps one card across pane ids: when tmux restarts and omp resumes under
    ///   a new pane id, an already-handled reply stays handled. A card closed because its
    ///   pane exited may return. A pane's omp breadcrumb counts only when it was written
    ///   after the pty appeared or the pane's omp started with `--resume` or `--restore`.
    /// - A Waiting to Working or Asking transition records `done` on unresolved info cards
    ///   from that pane instance with reason `resumed in pane`.
    /// - A pane that stays Waiting checks its transcript again after it grows.
    ///   The new reply replaces the older card.
    /// - omp gone from a pane whose shell survives (title and `pane_current_command` no
    ///   longer omp): the session is audited once per transcript version with the
    ///   outcomes of `audit_ended_session`. A pane omp was never seen running in reads
    ///   its breadcrumb and can raise only `Unreported:`.
    /// - When a pane disappears, its asks and plans close `pane exited`. Its session is
    ///   audited once from the transcript it last showed while omp ran, with the outcomes
    ///   of `audit_ended_session`, except when one of its cards closed the pane. A pane
    ///   that vanishes alone, such as a closed window, also closes its queued and deferred
    ///   information cards. When several panes vanish in one tick, or the tmux server dies,
    ///   information cards stay queued. Unread reports survive a tmux server death. Finding
    ///   cards are never swept.
    /// - Plan proposals from the transcript add a card only while the review overlay is open.
    ///   The tracker completes it after the overlay closes or supersedes it when a newer proposal appears.
    /// Non-omp panes are ignored.
    pub fn tick(&mut self, db: &Db) -> TickReport {
        let Ok(panes) = tmux::list_panes() else {
            return TickReport { written: 0 };
        };
        let present = panes
            .iter()
            .map(|pane| pane.id.as_str())
            .collect::<HashSet<_>>();
        // One scan per tick names the panes that can have plan cards to finish.
        let unresolved_panes = db.unresolved_panes().ok();
        let mut written = 0;
        let mut failed_restored_panes = HashSet::new();
        let mut exited: Vec<(&tmux::Pane, Option<PathBuf>, bool)> = Vec::new();
        let mut live_transcripts = HashSet::new();
        self.watch_startup(&panes);
        self.recover_dead_startup(&present, db, &mut written);
        self.fire_startup_retries();
        for pane in &panes {
            let state = omp::classify(&pane.title);
            let previous = self.states.get(&pane.id).copied();
            if state == PaneState::Asking {
                self.refresh_ask_cache(pane);
            } else {
                self.pending_ask_cache.remove(&pane.id);
            }
            let proposal_changed = state != PaneState::Other && self.refresh_proposal_cache(pane);
            let ask = self
                .pending_ask_cache
                .get(&pane.id)
                .and_then(|cached| cached.call.as_ref().map(|call| (&cached.path, call)));
            if previous.is_none()
                && state != PaneState::Asking
                && self.pending_panes.contains(&pane.id)
                && !finish_asking(db, pane, None, &mut written)
            {
                failed_restored_panes.insert(pane.id.clone());
                continue;
            }
            let ask_changed = ask.is_some_and(|(_, call)| {
                self.pending_ask_ids.get(&pane.id) != Some(&call.tool_call_id)
            });
            if state == PaneState::Other {
                if previous == Some(PaneState::Asking)
                    && !finish_asking(db, pane, None, &mut written)
                {
                    continue;
                }
                if !finish_closed_plan_cards(db, unresolved_panes.as_ref(), pane, &mut written)
                    && self.pending_panes.contains(&pane.id)
                {
                    failed_restored_panes.insert(pane.id.clone());
                }
                self.pending_proposals.remove(&pane.id);
                self.proposal_cache.remove(&pane.id);
                if pane.command != "omp" {
                    let (path, seen_live) = match self.seen.get(&pane.id) {
                        Some((_, path)) => (Some(path.clone()), true),
                        None => (omp::transcript_path(pane), false),
                    };
                    exited.push((pane, path, seen_live));
                }
                self.states.remove(&pane.id);
                self.stalled_info.remove(&pane.id);
                self.held_info.remove(&pane.id);
                self.paused_info.remove(&pane.id);
                self.pending_ask_ids.remove(&pane.id);
                continue;
            }
            if pane.command == "omp" {
                if let Some(path) = omp::transcript_path(pane) {
                    if previous != Some(state)
                        || self.seen.get(&pane.id).is_none_or(|(_, seen)| *seen != path)
                    {
                        self.seen.insert(pane.id.clone(), (pane.clone(), path.clone()));
                    }
                    live_transcripts.insert(path);
                }
            }
            if pane.command == "omp"
                && state == PaneState::Waiting
                && recover_no_model_launch(db, pane, &mut self.relaunched, &mut written)
            {
                self.states.remove(&pane.id);
                continue;
            }
            if proposal_changed {
                self.pending_proposals.insert(pane.id.clone(), 0);
            }
            if let Some(tries) = self.pending_proposals.get_mut(&pane.id) {
                let handled = self
                    .proposal_cache
                    .get(&pane.id)
                    .and_then(|cached| cached.proposals.last())
                    .is_some_and(|proposal| add_plan_card(db, pane, proposal, &mut written));
                if handled {
                    self.pending_proposals.remove(&pane.id);
                } else {
                    *tries += 1;
                    if *tries >= 10 {
                        self.pending_proposals.remove(&pane.id);
                    }
                }
            } else {
                finish_closed_plan_cards(db, unresolved_panes.as_ref(), pane, &mut written);
            }

            if previous.is_none() && state == PaneState::Asking {
                if let Some((_, call)) = ask {
                    if !finish_asking(db, pane, Some(&call.tool_call_id), &mut written) {
                        if self.pending_panes.contains(&pane.id) {
                            failed_restored_panes.insert(pane.id.clone());
                        }
                        continue;
                    }
                }
            }
            if previous != Some(state) || ask_changed {
                let succeeded = match (previous, state) {
                    (_, PaneState::Asking) => {
                        if previous == Some(PaneState::Asking)
                            && !finish_asking(db, pane, None, &mut written)
                        {
                            false
                        } else {
                            let resumed = if previous == Some(PaneState::Waiting) {
                                finish_resumed_cards(db, pane, &mut written)
                            } else {
                                true
                            };
                            resumed && match ask {
                                Some((path, call)) => {
                                    add_ask_card(db, pane, path, call, &mut written)
                                }
                                None => false,
                            }
                        }
                    }
                    (Some(PaneState::Asking), _) => finish_asking(db, pane, None, &mut written),
                    (Some(PaneState::Waiting), PaneState::Working) => {
                        finish_resumed_cards(db, pane, &mut written)
                    }
                    (Some(PaneState::Working) | None, PaneState::Waiting) => {
                        add_info_card(
                            db,
                            pane,
                            &panes,
                            &mut self.stalled_info,
                            &mut self.held_info,
                            &mut self.paused_info,
                            &mut written,
                        )
                    }
                    _ => true,
                };
                if !succeeded {
                    if self.pending_panes.contains(&pane.id) {
                        failed_restored_panes.insert(pane.id.clone());
                    }
                    continue;
                }
            }
            if state == PaneState::Waiting && previous == Some(PaneState::Waiting) {
                add_info_card(
                    db,
                    pane,
                    &panes,
                    &mut self.stalled_info,
                    &mut self.held_info,
                    &mut self.paused_info,
                    &mut written,
                );
            }
            if state == PaneState::Asking && (previous != Some(state) || ask_changed) {
                if let Some((_, call)) = ask {
                    self.pending_ask_ids
                        .insert(pane.id.clone(), call.tool_call_id.clone());
                }
            } else if state != PaneState::Asking {
                self.pending_ask_ids.remove(&pane.id);
            }
            self.states.insert(pane.id.clone(), state);
        }
        self.pending_panes.retain(|pane| {
            !present.contains(pane.as_str()) || failed_restored_panes.contains(pane)
        });

        let gone = self
            .states
            .keys()
            .chain(self.pending_panes.iter())
            .chain(self.stalled_info.keys())
            .chain(self.held_info.keys())
            .chain(self.paused_info.keys())
            .chain(self.seen.keys())
            .filter(|pane| !present.contains(pane.as_str()))
            .cloned()
            .collect::<HashSet<_>>();
        // One pane gone while tmux still lists panes means its window was closed by hand.
        // An empty listing means the tmux server died. Unread reports must survive that.
        let individual = gone.len() == 1 && !panes.is_empty();
        for pane in gone {
            self.pending_ask_cache.remove(&pane);
            self.stalled_info.remove(&pane);
            self.held_info.remove(&pane);
            self.paused_info.remove(&pane);
            self.pending_proposals.remove(&pane);
            self.proposal_cache.remove(&pane);
            // Information cards are swept once per vanish. A retried audit skips the sweep,
            // which keeps the card its first run raised.
            let close_info = individual && !self.audit_pending.contains(&pane);
            if finish_gone(db, &pane, close_info, &mut written) {
                self.audit_pending.insert(pane.clone());
                self.states.remove(&pane);
                self.pending_ask_ids.remove(&pane);
                self.pending_panes.remove(&pane);
                if self.audit_gone_pane(db, &pane, &panes, &live_transcripts, &mut written) {
                    self.seen.remove(&pane);
                    self.audit_pending.remove(&pane);
                }
            }
        }
        for (pane, path, seen_live) in exited {
            let Some(path) = path else {
                self.seen.remove(&pane.id);
                continue;
            };
            if live_transcripts.contains(&path) {
                self.seen.remove(&pane.id);
                continue;
            }
            if let Some(length) = fs::metadata(&path).ok().map(|metadata| metadata.len()) {
                if self.audit_exited_pane(db, pane, &panes, path, length, seen_live, &mut written) {
                    self.seen.remove(&pane.id);
                }
            } else {
                self.seen.remove(&pane.id);
            }
        }
        TickReport { written }
    }
}


/// Startup launches: omp can die before it writes a session, which used to leave nothing behind.
impl Tracker {
    /// Remember every pane that runs omp with no session yet. A triage launch is `omp` with flags,
    /// so a bare `omp` stays the operator's own launch. A window a relaunch opened is claimed here,
    /// which is where its pid arrives for the log check in `recover_dead_startup`.
    fn watch_startup(&mut self, panes: &[tmux::Pane]) {
        let now = Instant::now();
        for pane in panes {
            if pane.command != "omp" {
                continue;
            }
            if omp::transcript_path(pane).is_some_and(|path| path.exists()) {
                if let Some(command) = self.startup_panes.remove(&pane.id) {
                    self.startup.remove(&command);
                }
                continue;
            }
            if let Some(command) = self
                .startup
                .iter()
                .find(|(_, entry)| entry.pending.as_deref() == Some(pane.id.as_str()))
                .map(|(command, _)| command.clone())
            {
                let entry = self.startup.get_mut(&command).expect("found just above");
                entry.pane = Some(pane.clone());
                entry.pending = None;
                entry.seen = now;
                self.startup_panes.insert(pane.id.clone(), command);
                continue;
            }
            if self.startup_panes.contains_key(&pane.id) {
                continue;
            }
            let Some(command) = launch_command(pane) else {
                continue;
            };
            self.startup.insert(
                command.clone(),
                Startup {
                    command: command.clone(),
                    cwd: pane.cwd.clone(),
                    session: pane.session.clone(),
                    tries: 0,
                    seen: now,
                    pane: Some(pane.clone()),
                    pending: None,
                    due: None,
                },
            );
            self.startup_panes.insert(pane.id.clone(), command);
        }
    }

    /// Settle watched launches whose window vanished before a session existed. Only omp's own
    /// SQLITE_BUSY log entry makes a launch safe to repeat, so a launch that failed for another
    /// reason keeps the old silent behaviour. Relaunches follow `STARTUP_BACKOFF`; one that dies
    /// again after the last backoff raises a card.
    fn recover_dead_startup(&mut self, present: &HashSet<&str>, db: &Db, written: &mut usize) {
        let now = Instant::now();
        let settled: Vec<String> = self
            .startup
            .iter()
            .filter(|(_, entry)| match (&entry.pane, &entry.pending) {
                (Some(pane), _) => !present.contains(pane.id.as_str()),
                (None, Some(pending)) => !present.contains(pending.as_str()),
                (None, None) => false,
            })
            .map(|(command, _)| command.clone())
            .collect();
        for command in settled {
            let Some(mut entry) = self.startup.remove(&command) else {
                continue;
            };
            if let Some(pending) = entry.pending.take() {
                eprintln!(
                    "triage: relaunched window {pending} for {} never appeared",
                    truncate_chars(command, 80)
                );
                continue;
            }
            let Some(pane) = entry.pane.take() else {
                continue;
            };
            self.startup_panes.remove(&pane.id);
            if omp::transcript_path(&pane).is_some_and(|path| path.exists()) {
                continue;
            }
            let young = now.saturating_duration_since(entry.seen) < STARTUP_WINDOW;
            if !young || self.relaunched.contains(&command) || !omp::startup_busy(pane.pid) {
                continue;
            }
            if usize::from(entry.tries) >= STARTUP_BACKOFF.len() {
                eprintln!(
                    "triage: {} died at startup {} times; raising a card",
                    pane.id, entry.tries
                );
                queue_startup_card(db, &pane, &entry, written);
                continue;
            }
            let delay = startup_delay(&command, entry.tries);
            entry.tries += 1;
            entry.seen = now;
            entry.due = Some(now + delay);
            eprintln!(
                "triage: {} died at startup with SQLITE_BUSY; relaunching in {} ms",
                pane.id,
                delay.as_millis()
            );
            self.startup.insert(command, entry);
        }
        self.startup.retain(|_, entry| {
            if entry.due.is_some() {
                return true;
            }
            let window = now.saturating_duration_since(entry.seen) < STARTUP_WINDOW;
            match (&entry.pane, &entry.pending) {
                (Some(pane), _) => window && present.contains(pane.id.as_str()),
                (None, Some(pending)) => window && present.contains(pending.as_str()),
                (None, None) => false,
            }
        });
    }

    /// Open the replacement window for every relaunch whose backoff has elapsed. The new pane is
    /// claimed by `watch_startup` on the next poll, which reads the pid its omp log is named by.
    fn fire_startup_retries(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .startup
            .iter()
            .filter(|(_, entry)| entry.due.is_some_and(|due| due <= now))
            .map(|(command, _)| command.clone())
            .collect();
        for command in due {
            let Some(mut entry) = self.startup.remove(&command) else {
                continue;
            };
            entry.due = None;
            entry.seen = now;
            match tmux::new_window(&entry.session, &entry.cwd, &entry.command) {
                Ok(pane) => {
                    eprintln!("triage: relaunched {} as {pane}", truncate_chars(command.clone(), 80));
                    entry.pending = Some(pane);
                    self.startup.insert(command, entry);
                }
                Err(error) => eprintln!("triage: relaunch of {command} failed: {error}"),
            }
        }
    }
}

/// Card actions for a pane run one at a time. Lock order is pane, then card. The tracker takes
/// a pane lock before any card lock only to audit a gone pane.
fn pane_lock(db: &Db, pane: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    db.answer_lock(&format!("pane:{pane}"))
}

/// The lock a card action takes first, keyed by the card's dsh session or else its pane.
fn target_lock(db: &Db, card: &Card) -> Option<std::sync::Arc<std::sync::Mutex<()>>> {
    match dsh::session_id(card) {
        Some(sid) => Some(db.answer_lock(&format!("dsh:{sid}"))),
        None => card.source.pane.as_deref().map(|pane| pane_lock(db, pane)),
    }
}

/// Execute an answer for a queued card.
/// - ask: re-capture the pane, require `omp::classify(title) == Asking` and a parsed
///   dialog cursor; move with Down/Up to `option` (or to the last row for `text`,
///   then type the text), press Enter, re-capture and require the dialog gone.
///   Record the swipe with reason `typed`. Any mismatch returns Err and writes nothing.
/// - info: swipe done.
/// - A plan answer requires a queued card and an open review overlay. It sends Enter for approval, or confirms the overlay's own `Refine plan` row and then sends the note as literal text, and stores the choice in the swipe.
/// `launch` names the harness, tmux session, model and thinking level a launched session uses.
pub fn answer(
    db: &Db,
    card: &Card,
    req: &AnswerReq,
    launch: &Launch,
    image: Option<&Upload>,
) -> Result<(), String> {
    let target_mutex = target_lock(db, card);
    let _target_guard = target_mutex.as_ref().map(|mutex| mutex.lock().unwrap());
    match card.kind {
        Kind::Ask => answer_ask(db, card, req, launch),
        Kind::Finding => answer_finding(db, card, req, launch, image),
        Kind::Plan => answer_plan(db, card, req, launch),
        Kind::Info => {
            let answer_lock = db.answer_lock(&card.id);
            let _answer_guard = answer_lock.lock().unwrap();
            let swipe = db
                .card(&card.id)
                .map_err(|error| error.to_string())?
                .and_then(|(_, swipe)| swipe);
            if !queued_for_answer(swipe.as_ref()) {
                return Err("card is not queued".to_owned());
            }
            if record_swipe(
                db,
                &card.id,
                "done",
                req.option,
                req.text.clone(),
                req.note.clone(),
                None,
            ) {
                Ok(())
            } else {
                Err("could not record swipe".to_owned())
            }
        }
    }
}

/// How a new omp window starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start<'a> {
    /// omp goal mode with an objective that names the brief file.
    Goal,
    /// Read-only plan mode through a per-run config overlay; the plan comes back as a card.
    Plan,
    /// `--plan-yolo`. omp approves the plan itself and hands it to this execution model,
    /// or to its `smol` role when unset. The effort names the execution model's own level:
    /// omp appends it to the selector, and dsh applies it at approval.
    PlanInto(Option<&'a str>, Option<&'a str>),
    /// Carries out a plan the session already holds. Full permissions, no second review: a
    /// dsh session still in plan mode leaves it through the plan decision, because a prompt
    /// in plan mode is planned again instead of carried out.
    Implement,
}

impl<'a> Start<'a> {
    /// Read a `/v1/sessions` mode. Only `plan-into` takes an execution model and its effort.
    pub fn parse(
        mode: Option<&str>,
        executor: Option<&'a str>,
        executor_thinking: Option<&'a str>,
    ) -> Result<Self, String> {
        let start = match mode.unwrap_or("goal") {
            "goal" => Self::Goal,
            "plan" => Self::Plan,
            "plan-into" => return Ok(Self::PlanInto(executor, executor_thinking)),
            _ => return Err("unknown mode".to_owned()),
        };
        if executor.is_some() {
            return Err("executor needs mode plan-into".to_owned());
        }
        if executor_thinking.is_some() {
            return Err("executor thinking needs mode plan-into".to_owned());
        }
        Ok(start)
    }
}

/// A dispatch can investigate, act, implement a plan it already holds, plan for review, or plan
/// into an execution model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    Investigate,
    Plan,
    Act,
    PlanInto,
    /// Carry out the plan the card holds. The design is settled, so the session moves forward
    /// instead of planning again.
    Implement,
}

impl DispatchMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "investigate" => Some(Self::Investigate),
            "act" => Some(Self::Act),
            "plan" => Some(Self::Plan),
            "plan-into" => Some(Self::PlanInto),
            "implement" => Some(Self::Implement),
            _ => None,
        }
    }

    /// Swipe text and brief title word.
    pub fn swipe_text(&self) -> &'static str {
        match self {
            Self::Investigate => "Investigate",
            Self::Act => "Act",
            Self::Plan => "Plan",
            Self::PlanInto => "Plan into",
            Self::Implement => "Implement",
        }
    }
}

/// Launch an omp session for any card and resolve the card.
///
/// An ask stays queued for an hour with reason `investigating %N`, `planning %N`, or `acting %N`
/// while its dialog remains open. Findings and info cards finish with reason `launched %N`.
/// The session's final report comes back as an info card through the tracker. `executor` is the
/// execution model a plan-into dispatch hands its approved plan to, omp's `smol` role when
/// unset, and `executor_thinking` is the effort it takes; the server refuses both with any
/// other mode. `new_window` starts the brief in a session of its own instead of the card's
/// waiting source session.
#[allow(clippy::too_many_arguments)]
pub fn dispatch(
    db: &Db,
    card: &Card,
    mode: DispatchMode,
    note: Option<&str>,
    launch: &Launch,
    executor: Option<&str>,
    executor_thinking: Option<&str>,
    image: Option<&Upload>,
    new_window: bool,
) -> Result<(), String> {
    let target_mutex = target_lock(db, card);
    let _target_guard = target_mutex.as_ref().map(|mutex| mutex.lock().unwrap());
    let answer_lock = db.answer_lock(&card.id);
    let _answer_guard = answer_lock.lock().unwrap();
    let swipe = db
        .card(&card.id)
        .map_err(|error| error.to_string())?
        .and_then(|(_, swipe)| swipe);
    if !queued_for_answer(swipe.as_ref()) {
        return Err("card is not queued".to_owned());
    }
    let path = write_brief(db, dispatch_brief(card, mode, note, workspace_workdir(launch, &card_workdir(card))))?;
    let start = match mode {
        DispatchMode::Plan => Start::Plan,
        DispatchMode::PlanInto => Start::PlanInto(executor, executor_thinking),
        DispatchMode::Implement => Start::Implement,
        DispatchMode::Investigate | DispatchMode::Act => Start::Goal,
    };
    let pane = launch_brief(card, launch, &path, start, image, new_window)?;
    let recorded = if card.kind == Kind::Ask {
        let doing = match mode {
            DispatchMode::Investigate => "investigating",
            DispatchMode::Plan | DispatchMode::PlanInto => "planning",
            DispatchMode::Implement => "implementing",
            DispatchMode::Act => "acting",
        };
        db.swipe(&Swipe {
            card: card.id.clone(),
            action: "later".to_owned(),
            option: None,
            text: Some(mode.swipe_text().to_owned()),
            note: note.map(str::to_owned),
            until_ms: Some(events::now_ms() + 60 * 60 * 1000),
            reason: Some(format!("{doing} {pane}")),
            at_ms: events::now_ms(),
        })
        .is_ok()
    } else {
        record_swipe(
            db,
            &card.id,
            "done",
            None,
            Some(mode.swipe_text().to_owned()),
            note.map(str::to_owned),
            Some(format!("launched {pane}")),
        )
    };
    if recorded {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Acknowledge a card and close the tmux pane it came from, which exits the harness in it and
/// closes the window when the pane was its only one. The pane goes first, so a failed kill
/// leaves the card queued. Reason `closed %N`. A card with no pane, or whose pane tmux no
/// longer finds, is acknowledged exactly like a plain ack, because there is nothing to close.
/// A dsh card cancels its session instead and resolves that session's other cards with it.
pub fn close(db: &Db, card: &Card, note: Option<&str>, launch: &Launch) -> Result<(), String> {
    let target_mutex = target_lock(db, card);
    let _target_guard = target_mutex.as_ref().map(|mutex| mutex.lock().unwrap());
    let answer_lock = db.answer_lock(&card.id);
    let _answer_guard = answer_lock.lock().unwrap();
    let swipe = db
        .card(&card.id)
        .map_err(|error| error.to_string())?
        .and_then(|(_, swipe)| swipe);
    if !queued_for_answer(swipe.as_ref()) {
        return Err("card is not queued".to_owned());
    }
    if let Some(sid) = dsh::session_id(card) {
        return dsh_track::close(db, card, sid, note, launch);
    }
    let reason = match card.source.pane.as_deref() {
        None => None,
        Some(pane) => match tmux::kill_pane(pane) {
            Ok(()) => Some(format!("closed {pane}")),
            Err(error) if tmux::pane_gone(&error) => None,
            Err(error) => return Err(error.to_string()),
        },
    };
    if record_swipe(
        db,
        &card.id,
        "done",
        None,
        reason.is_some().then(|| "Close".to_owned()),
        note.map(str::to_owned),
        reason,
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}
const REQUEST_REPORT_TEXT: &str = "Send the final report your brief requires now: first line `ENVOY_STATUS: complete`, `ENVOY_STATUS: agent-done` or `ENVOY_STATUS: incomplete`, then the four report lines your brief describes.";

/// Type the request for the final report into the card's waiting pane and resolve the card.
/// A dsh card asks its session instead. Only an idle session takes the request.
pub fn request_report(db: &Db, card: &Card, launch: &Launch) -> Result<(), String> {
    if dsh::session_id(card).is_none() && card.source.pane.is_none() {
        return Err("card has no pane".to_owned());
    }
    let target_mutex = target_lock(db, card);
    let _target_guard = target_mutex.as_ref().map(|mutex| mutex.lock().unwrap());
    let answer_lock = db.answer_lock(&card.id);
    let _answer_guard = answer_lock.lock().unwrap();
    let swipe = db.card(&card.id).map_err(|error| error.to_string())?.and_then(|(_, swipe)| swipe);
    if !card.report_due {
        return Err("final report was not requested for this card".to_owned());
    }
    if !queued_for_answer(swipe.as_ref()) {
        return Err("card is not queued".to_owned());
    }
    if let Some(sid) = dsh::session_id(card) {
        return dsh_track::request_report(db, card, sid, launch);
    }
    let pane = card.source.pane.as_deref().ok_or_else(|| "card has no pane".to_owned())?;
    let panes = tmux::list_panes().map_err(|error| error.to_string())?;
    if !panes.iter().any(|candidate| candidate.id == pane && omp::classify(&candidate.title) == PaneState::Waiting) {
        return Err("pane is not waiting".to_owned());
    }
    send(pane, REQUEST_REPORT_TEXT)?;
    if record_swipe(db, &card.id, "done", None, Some("Request final report".to_owned()), None, Some(format!("report requested {pane}"))) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Open a new window that resumes an ended session with `omp --restore`. A crashed or
/// killed run then continues with its todos and context instead of a fresh brief. The
/// window opens in the daemon's launch session at the pane's own cwd. The restored run's
/// next card supersedes this one. Reason `restored <pane>`.
pub fn restore(db: &Db, card: &Card, launch: &Launch) -> Result<(), String> {
    let answer_lock = db.answer_lock(&card.id);
    let _answer_guard = answer_lock.lock().unwrap();
    let swipe = db.card(&card.id).map_err(|error| error.to_string())?.and_then(|(_, swipe)| swipe);
    if !restorable(card) {
        return Err("card is not an ended-session card".to_owned());
    }
    if !queued_for_answer(swipe.as_ref()) {
        return Err("card is not queued".to_owned());
    }
    let transcript = card.refs.first().ok_or_else(|| "card has no transcript".to_owned())?;
    let id = session_id(transcript).ok_or_else(|| "no session id in transcript name".to_owned())?;
    let session = launch.session.as_deref().unwrap_or(DEFAULT_SESSION);
    let command = format!("omp --restore {}", shell_quote(&id));
    let pane = tmux::new_window(session, &card.source.cwd, &command).map_err(|error| error.to_string())?;
    if record_swipe(db, &card.id, "done", None, Some("Restore run".to_owned()), None, Some(format!("restored {pane}"))) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// An `exited:` card (Unreported or Ended mid-turn, including a reloaded copy) whose
/// transcript names a session omp can restore.
fn restorable(card: &Card) -> bool {
    (card.id.starts_with("exited:") || card.id.starts_with("reload:exited:"))
        && card.kind == Kind::Info
        && card.refs.first().is_some_and(|path| session_id(path).is_some())
}

/// The session id omp names a transcript file by, after the timestamp:
/// `<iso timestamp>_<session id>.jsonl`.
fn session_id(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?.strip_suffix(".jsonl")?;
    let (_, id) = name.rsplit_once('_')?;
    uuid::Uuid::parse_str(id).ok().map(|_| id.to_owned())
}

/// Launch a session from free text. Returns the new omp pane id, or `dsh:<sid>` for a dsh session.
/// The session's asks and final report reach the inbox through the tracker like any other pane.
/// A plan launch starts in read-only plan mode and a plan-into launch hands its plan to the
/// execution model without review. Every other session starts in goal mode with an objective
/// that names the brief.
pub fn start_session(
    db: &Db,
    prompt: &str,
    cwd: &str,
    launch: &Launch,
    start: Start,
    image: Option<&Upload>,
) -> Result<String, String> {
    if prompt.trim().is_empty() {
        return Err("prompt is empty".to_owned());
    }
    if launch.engine == Engine::Dsh {
        engine_flags(Engine::Dsh, launch.model.as_deref(), launch.thinking.as_deref())?;
        if let Start::PlanInto(executor, effort) = start {
            plan_into_choices(Engine::Dsh, executor, effort)?;
        }
        let path = write_brief(db, session_brief(prompt, plans_for_review(start), workspace_workdir(launch, cwd)))?;
        let title = prompt.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim();
        return dsh_track::launch_new(launch, cwd, &path, title, start, image);
    }
    // Validate before writing the brief so a bad choice leaves nothing behind.
    launch_flags(launch.model.as_deref(), launch.thinking.as_deref())?;
    if let Start::PlanInto(executor, effort) = start {
        plan_into_choices(Engine::Omp, executor, effort)?;
    }
    let path = write_brief(db, session_brief(prompt, plans_for_review(start), None))?;
    let session = launch.session.as_deref().unwrap_or(DEFAULT_SESSION);
    let command = omp_command(
        &path,
        launch.model.as_deref(),
        launch.thinking.as_deref(),
        start,
        image,
    )?;
    tmux::new_window(session, cwd, &command).map_err(|error| error.to_string())
}

/// Whether a launch has a plan to review, which adds the plan report section to its brief. An
/// implement launch carries out a plan that is already settled, so it is not one.
fn plans_for_review(start: Start) -> bool {
    matches!(start, Start::Plan | Start::PlanInto(..))
}

/// The prompt a session gets for a brief. Every dispatch reads the brief; an implement dispatch
/// says outright that the plan is approved and the work starts, so the harness prompts the next
/// step as implement instead of asking for the brief again.
fn dispatch_prompt(brief: &Path, start: Start) -> String {
    if start == Start::Implement {
        format!(
            "Implement the plan in the brief at {}: carry out every step and report the result.",
            brief.display()
        )
    } else {
        format!("Read the brief at {} and carry it out.", brief.display())
    }
}

/// Give the brief to the card's own pane while that pane is an omp pane waiting for input,
/// so the work continues in the session that already has the context. Otherwise open a new
/// window in the daemon's launch session; a window another launch left behind is never
/// taken. A new window starts in omp goal mode with an objective that names the brief, except
/// a plan dispatch, which starts in read-only plan mode, and a plan-into dispatch, which
/// runs `--plan-yolo` and hands the approved plan to the execution model. A waiting source
/// pane keeps its own modes and receives the brief as typed text, except that a plan-into
/// dispatch always opens a new window, because only launch flags turn plan-yolo on, and a
/// caller that asks for `new_window` starts its own session instead.
fn launch_brief(
    card: &Card,
    launch: &Launch,
    brief: &Path,
    start: Start,
    image: Option<&Upload>,
    new_window: bool,
) -> Result<String, String> {
    if launch.engine == Engine::Dsh {
        return dsh_track::launch_brief(card, launch, brief, start, image, new_window);
    }
    // Only launch flags turn plan-yolo on, so a Plan into dispatch never reuses a running pane.
    let source =
        card.source.pane.as_deref().filter(|_| !new_window && !matches!(start, Start::PlanInto(..)));
    if let Some(pane) = source {
        let waiting = tmux::list_panes()
            .map_err(|error| error.to_string())?
            .into_iter()
            .any(|candidate| {
                candidate.id == pane && omp::classify(&candidate.title) == PaneState::Waiting
            });
        if waiting {
            type_brief(pane, brief, start, image)?;
            return Ok(pane.to_owned());
        }
    }
    let session = launch.session.as_deref().unwrap_or(DEFAULT_SESSION);
    let command = omp_command(
        brief,
        launch.model.as_deref(),
        launch.thinking.as_deref(),
        start,
        image,
    )?;
    tmux::new_window(session, &card.source.cwd, &command).map_err(|error| error.to_string())
}

/// Type a brief into a waiting omp pane, turning plan mode on first for a plan brief. The
/// image rides the message as an `@` reference, which omp turns into a native attachment.
fn type_brief(
    pane: &str,
    brief: &Path,
    start: Start,
    image: Option<&Upload>,
) -> Result<(), String> {
    if start == Start::Plan {
        ensure_plan_mode(pane)?;
    }
    let mut text = dispatch_prompt(brief, start);
    if let Some(image) = image {
        text.push_str(&format!(
            " The attached image: @{}.",
            image.path.display()
        ));
    }
    tmux::send_text(pane, &text).map_err(|error| error.to_string())
}

fn ensure_plan_mode(pane: &str) -> Result<(), String> {
    let plan_mode_on = || {
        tmux::screen(pane)
            .map(|screen| omp::plan_mode_on(&screen))
            .map_err(|error| format!("plan mode screen capture failed: {error}"))
    };
    if plan_mode_on()? {
        return Ok(());
    }
    tmux::send_keys(pane, &["M-P"]).map_err(|error| format!("plan mode toggle failed: {error}"))?;
    // omp draws the Plan row only after it resolves the plan file and the plan model, so a
    // capture right after the toggle can miss it; poll for up to two seconds.
    for _ in 0..20 {
        if plan_mode_on()? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("plan mode did not turn on".to_owned())
}

pub(crate) fn queued_for_answer(swipe: Option<&Swipe>) -> bool {
    swipe.is_none_or(|swipe| {
        swipe.action == "later" && swipe.until_ms.is_some_and(|until| until <= events::now_ms())
    })
}


/// What the peek panel shows: the redacted visible screen laid out as tmux shows it.
pub struct Peek {
    pub text: String,
    pub state: PaneState,
    pub width: u16,
    pub height: u16,
}

pub fn peek(pane: &str) -> Result<Peek, String> {
    let panes = tmux::list_panes().map_err(|error| error.to_string())?;
    let target = panes
        .iter()
        .find(|candidate| candidate.id == pane)
        .ok_or_else(|| "pane not found".to_owned())?;
    let text = tmux::screen(pane).map_err(|error| error.to_string())?;
    Ok(Peek {
        text: omp::redact(&text),
        state: omp::classify(&target.title),
        width: target.width,
        height: target.height,
    })
}

/// Type `text` into a waiting omp pane. Refuses when not Waiting, when the editor
/// holds text, or when text starts with `/` (other than `/skill:`), `!` or `$`.
pub fn send(pane: &str, text: &str) -> Result<(), String> {
    if blocked_command_prefix(text) {
        return Err("text starts with a blocked command prefix".to_owned());
    }
    let panes = tmux::list_panes().map_err(|error| error.to_string())?;
    let target = panes
        .iter()
        .find(|candidate| candidate.id == pane)
        .ok_or_else(|| "pane not found".to_owned())?;
    if omp::classify(&target.title) != PaneState::Waiting {
        return Err("pane is not waiting".to_owned());
    }
    let screen = tmux::capture(pane, 60).map_err(|error| error.to_string())?;
    if !omp::editor_empty(&screen) {
        return Err("editor is not empty".to_owned());
    }
    tmux::send_text(pane, text).map_err(|error| error.to_string())
}

/// Whether the harness would run `text` as a command. Commands start with `/` (except `/skill:`), `!` or `$`.
pub(crate) fn blocked_command_prefix(text: &str) -> bool {
    (text.starts_with('/') && !text.starts_with("/skill:")) || text.starts_with('!') || text.starts_with('$')
}

/// Whether question `index` of an ask offers custom text. Legacy asks always do.
fn ask_allows_other(call: &omp::AskCall, index: usize) -> bool {
    call.form
        .as_ref()
        .and_then(|form| form.controls.get(index))
        .is_none_or(omp::HtmlControl::allows_other)
}

fn add_ask_card(
    db: &Db,
    pane: &tmux::Pane,
    path: &std::path::Path,
    call: &omp::AskCall,
    written: &mut usize,
) -> bool {
    let Some(question) = call.questions.first() else {
        return false;
    };
    let situation = match &call.form {
        Some(form) => truncate_chars(omp::redact_credentials(&form.context), 1200),
        None => omp::last_assistant_text(path)
            .ok()
            .flatten()
            .map(|(_, text)| truncate_chars(omp::redact_credentials(&text), 1200))
            .unwrap_or_default(),
    };
    // An HTML ask titles its card by its first fieldset legend.
    let title = match question.header.as_deref().map(str::trim).filter(|header| !header.is_empty()) {
        Some(header) if call.form.is_some() => header.to_owned(),
        _ => question.question.clone(),
    };
    let allow_other: Vec<bool> = (0..call.questions.len())
        .map(|index| ask_allows_other(call, index))
        .collect();
    let unsupported = call.form.as_ref().is_some_and(|form| {
        form.controls
            .iter()
            .any(|control| matches!(control, omp::HtmlControl::Unsupported))
    });
    let card = Card {
        id: format!("ask:{}:{}", pane.id, call.tool_call_id),
        kind: Kind::Ask,
        priority: 1,
        created_ms: events::now_ms(),
        source: pane_source(pane),
        title,
        star: Star {
            situation,
            task: question.question.clone(),
            ..Star::default()
        },
        options: card_options(question),
        answerable: !unsupported && call.questions.iter().all(|question| !question.multi),
        allow_other: allow_other.iter().any(|allowed| *allowed),
        questions: (call.questions.len() > 1)
            .then(|| {
                call.questions
                    .iter()
                    .zip(&allow_other)
                    .map(|(question, allowed)| CardQuestion {
                        label: question
                            .header
                            .as_deref()
                            .map(str::trim)
                            .filter(|header| !header.is_empty())
                            .unwrap_or(&question.id)
                            .to_owned(),
                        question: question.question.clone(),
                        options: card_options(question),
                        allow_other: *allowed,
                        star: None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        body: None,
        refs: Vec::new(),
        level: None,
        tool_call_id: Some(call.tool_call_id.clone()),
        report_due: false,
        engine: None,
    };
    match db.insert_card(&card) {
        Ok(inserted) => {
            *written += usize::from(inserted);
            true
        }
        Err(_) => false,
    }
}

fn card_options(question: &omp::Question) -> Vec<CardOption> {
    question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| CardOption {
            label: option.label.clone(),
            description: option.description.clone(),
            direction: None,
            recommended: question.recommended == Some(index),
        })
        .collect()
}

/// How long a reply without a status line waits for an active goal's continuation, counted
/// from the transcript's last change. omp's TUI starts that turn 800 ms after a settled one
/// unless a gate it keeps only in memory skips it.
const GOAL_CONTINUATION_GRACE: Duration = Duration::from_secs(30);

/// How long a reply last seen with omp's wait or retry row stays held once the row is
/// gone. omp drops the row as a background job finishes before its result reaches the
/// transcript. Panes stayed waiting up to 20 s in that gap.
const WAIT_ROW_GRACE: Duration = Duration::from_secs(30);

/// Queue an info card after a pane that turned Waiting finishes its run.
/// The wait-row hold delays replies after a clean capture if an earlier one saw the row.
/// It applies only to the same transcript version and `WAIT_ROW_GRACE` window.
/// `paused` records the last sighting of the row. A delivered result or process exit means
/// the run continues. Replies without status lines also wait for goal or process continuations.
/// `held` stores that reply's transcript version. Later polls check the hold only.
fn add_info_card(
    db: &Db,
    pane: &tmux::Pane,
    panes: &[tmux::Pane],
    stalled: &mut HashMap<String, (PathBuf, u64)>,
    held: &mut HashMap<String, HeldReply>,
    paused: &mut HashMap<String, (PathBuf, u64, Instant)>,
    written: &mut usize,
) -> bool {
    let Some(path) = omp::transcript_path(pane) else {
        return false;
    };
    let Ok(metadata) = fs::metadata(&path) else {
        return false;
    };
    let length = metadata.len();
    if stalled
        .get(&pane.id)
        .is_some_and(|(stalled_path, stalled_length)| *stalled_path == path && *stalled_length == length)
    {
        return false;
    }
    let fresh = metadata
        .modified()
        .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age < GOAL_CONTINUATION_GRACE));
    if held.get(&pane.id).is_some_and(|reply| {
        reply.path == path && reply.length == length && ((reply.goal && fresh) || omp::service_wake_pending(&path))
    }) {
        return false;
    }
    held.remove(&pane.id);
    let Ok(screen) = tmux::screen(&pane.id) else {
        return false;
    };
    if omp::run_paused(&screen) {
        paused.insert(pane.id.clone(), (path, length, Instant::now()));
        return false;
    }
    // shortcut: an entry between the row vanishing and job result releases the hold early.
    // Key on reply id if one appears in card history.
    if paused.get(&pane.id).is_some_and(|(seen_path, seen_length, seen)| {
        *seen_path == path && *seen_length == length && seen.elapsed() < WAIT_ROW_GRACE
    }) {
        return false;
    }
    let end = match omp::turn_end(&path) {
        Ok(Some(omp::TurnEnd::Running)) => {
            stalled.insert(pane.id.clone(), (path, length));
            return false;
        }
        Ok(end) => end,
        Err(_) => return false,
    };
    if let Some(omp::TurnEnd::Reply { text, .. }) = &end {
        // omp starts the next turn itself. Only a final report may card before that turn.
        if omp::final_report(text).is_none() {
            let goal = fresh && omp::goal_active(&path).unwrap_or(false);
            if goal || omp::service_wake_pending(&path) {
                held.insert(pane.id.clone(), HeldReply { path, length, goal });
                return false;
            }
        }
    }
    // omp rewrites its working row on every spinner tick. One capture can miss a background-job
    // wait or retry. Two more captures must agree before a card goes out.
    if matches!(end, Some(omp::TurnEnd::Reply { .. } | omp::TurnEnd::Error { .. })) {
        for _ in 0..2 {
            let Ok(screen) = tmux::screen(&pane.id) else {
                return false;
            };
            if omp::run_paused(&screen) {
                paused.insert(pane.id.clone(), (path.clone(), length, Instant::now()));
                return false;
            }
        }
    }
    let succeeded = match end {
        None => true,
        Some(end) => queue_turn_card(db, pane, panes, &path, end, written),
    };
    if succeeded {
        stalled.insert(pane.id.clone(), (path, length));
    }
    succeeded
}

/// The shell command behind a pane running a triage-style omp launch: the pane's foreground argv,
/// quoted, when it is `omp` with at least one flag. `None` for a bare `omp`, which is the
/// operator's own launch, and for an argv that cannot be read.
fn launch_command(pane: &tmux::Pane) -> Option<String> {
    let argv = pane.foreground_argv()?;
    if argv.first().map(String::as_str) != Some("omp") || argv.len() < 2 {
        return None;
    }
    Some(argv.iter().map(|arg| shell_quote(arg)).collect::<Vec<_>>().join(" "))
}

/// The wait before relaunch number `tries` (zero-based) of `command`: that attempt's fixed step
/// plus jitter spread by the command, so several launches retrying together do not open their
/// windows in lockstep.
fn startup_delay(command: &str, tries: u8) -> Duration {
    use std::hash::{Hash, Hasher};
    let base = STARTUP_BACKOFF.get(usize::from(tries)).copied().unwrap_or(8);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    command.hash(&mut hasher);
    tries.hash(&mut hasher);
    Duration::from_millis(base * 1000 + hasher.finish() % (STARTUP_JITTER_MS + 1))
}

/// One error card for a launch that kept dying at startup: id `startup:<pane>:<ms>`, the command
/// in the body so it can be started by hand, and level error.
fn queue_startup_card(db: &Db, pane: &tmux::Pane, entry: &Startup, written: &mut usize) -> bool {
    let card = Card {
        id: format!("startup:{}:{}", pane.id, events::now_ms()),
        kind: Kind::Info,
        priority: 2,
        created_ms: events::now_ms(),
        source: pane_source(pane),
        title: "Launch kept dying at startup: database is locked".to_owned(),
        star: Star::default(),
        options: vec![CardOption {
            label: "Acknowledge".to_owned(),
            description: None,
            direction: None,
            recommended: false,
        }],
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: Some(format!(
            "omp exited before it wrote a session {} times in a row, each time with SQLITE_BUSY on ~/.omp/agent/agent.db. The host was too busy to serve the database. Start the command by hand when the load drops:\n\n{}",
            usize::from(entry.tries) + 1, entry.command
        )),
        refs: Vec::new(),
        level: Some("error".to_owned()),
        tool_call_id: None,
        report_due: false,
        engine: None,
    };
    match db.insert_card(&card) {
        Ok(inserted) => {
            *written += usize::from(inserted);
            true
        }
        Err(_) => false,
    }
}

/// A triage-launched omp that came up without a model (see `omp::no_model`) wrote no session, so
/// it has nothing to lose. Relaunch its argv once in the same session and cwd, then kill the dead
/// pane. A second no-model start of the same command raises an error card and leaves its pane
/// alone. Returns true when it acted.
fn recover_no_model_launch(db: &Db, pane: &tmux::Pane, relaunched: &mut HashSet<String>, written: &mut usize) -> bool {
    if omp::transcript_path(pane).is_some_and(|path| path.exists()) {
        return false;
    }
    let Ok(screen) = tmux::screen(&pane.id) else {
        return false;
    };
    if !omp::no_model(&screen) {
        return false;
    }
    let Some(command) = launch_command(pane) else {
        return false;
    };
    if relaunched.insert(command.clone()) {
        return match tmux::new_window(&pane.session, &pane.cwd, &command) {
            Ok(new_pane) => {
                eprintln!("triage: {} started without a model; relaunched as {new_pane}", pane.id);
                let _ = tmux::kill_pane(&pane.id);
                true
            }
            Err(error) => {
                eprintln!("triage: relaunch of {} failed: {error}", pane.id);
                relaunched.remove(&command);
                false
            }
        };
    }
    let card = Card {
        id: format!("nomodel:{}:{}", pane.id, pane.pty_born_ms().unwrap_or_default()),
        kind: Kind::Info,
        priority: 2,
        created_ms: events::now_ms(),
        source: pane_source(pane),
        title: "Launch failed twice: No model selected".to_owned(),
        star: Star::default(),
        options: vec![CardOption {
            label: "Acknowledge".to_owned(),
            description: None,
            direction: None,
            recommended: false,
        }],
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: Some(format!(
            "omp came up without a model twice, so it could not load its model configuration. The cause seen so far is host load locking ~/.omp/agent/agent.db. Start the command by hand when the load drops:\n\n{command}"
        )),
        refs: Vec::new(),
        level: Some("error".to_owned()),
        tool_call_id: None,
        report_due: false,
        engine: None,
    };
    match db.insert_card(&card) {
        Ok(inserted) => {
            *written += usize::from(inserted);
            true
        }
        Err(_) => false,
    }
}

/// Queue the card a live pane gets when its run ends as `end`: a reply card, a
/// `Stopped with error` card, or nothing for an interrupt or a run still going.
fn queue_turn_card(
    db: &Db,
    pane: &tmux::Pane,
    panes: &[tmux::Pane],
    path: &Path,
    end: omp::TurnEnd,
    written: &mut usize,
) -> bool {
    let (trail, investigation) = pane_trail(db, pane);
    let (entry_id, turn) = match end {
        omp::TurnEnd::Aborted | omp::TurnEnd::Running => return true,
        omp::TurnEnd::Error { entry_id, message } => {
            let message = omp::redact_credentials(&message);
            let headline = headline_of(&message);
            let title = if headline.is_empty() {
                "Stopped with error".to_owned()
            } else {
                format!("Stopped with error: {headline}")
            };
            let body = (!message.trim().is_empty()).then_some(message);
            let turn = Turn {
                title,
                body,
                level: "error",
                action: String::new(),
                report_due: false,
            };
            (entry_id, turn)
        }
        omp::TurnEnd::Reply { entry_id, text, report } => (
            entry_id,
            reply_turn(
                text,
                report,
                !trail.situation.is_empty(),
                investigation,
            ),
        ),
    };
    let card = turn_card(
        format!("done:{}:{entry_id}", pane.id),
        pane_source(pane),
        vec![path.display().to_string()],
        trail,
        turn,
    );
    let succeeded = supersede_info_cards(db, &card.id, &card.refs[0], Some(pane), written);
    match reply_has_card(db, panes, "done", &entry_id, &card.id, written) {
        None => return false,
        Some(true) => return succeeded,
        Some(false) => {}
    }
    match db.insert_card(&card) {
        Ok(inserted) => *written += usize::from(inserted),
        Err(_) => return false,
    }
    succeeded
}

/// The first non-blank line of `source`, cut to 80 characters.
fn headline_of(source: &str) -> String {
    truncate_chars(
        source.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim().to_owned(),
        80,
    )
}

/// What a finished turn puts on its info card, before the launch trail joins it.
struct Turn {
    title: String,
    body: Option<String>,
    level: &'static str,
    action: String,
    report_due: bool,
}

/// The info card for a finished turn. Its launch trail becomes the star. Its one option is
/// Acknowledge.
fn turn_card(id: String, source: Source, refs: Vec<String>, trail: Star, turn: Turn) -> Card {
    Card {
        id,
        kind: Kind::Info,
        priority: 3,
        created_ms: events::now_ms(),
        source,
        title: turn.title,
        star: Star { action: turn.action, ..trail },
        options: vec![CardOption {
            label: "Acknowledge".to_owned(),
            description: None,
            direction: None,
            recommended: false,
        }],
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: turn.body,
        refs,
        level: Some(turn.level.to_owned()),
        tool_call_id: None,
        report_due: turn.report_due,
        engine: None,
    }
}

/// The card parts for a reply. `report` is the latest status-line report earlier in the run,
/// which a closing reply follows.
fn reply_turn(
    text: String,
    report: Option<String>,
    has_trail: bool,
    investigation: bool,
) -> Turn {
    let text = omp::redact_credentials(&text);
    let report = report.map(|report| omp::redact_credentials(&report));
    // A status-line report written before a background wait still names the
    // run's outcome; the closing reply follows it in the body.
    let carried = report.filter(|_| omp::final_report(&text).is_none());
    let source = carried.as_deref().unwrap_or(&text);
    let reported = omp::final_report(source);
    let withhold_action = !investigation && matches!(reported, Some((omp::FinalStatus::Complete, _)));
    let missing_report = reported.as_ref().is_some_and(|(_, report)| report.is_empty());
    // A reply without a status line still ends a turn that waits for the user,
    // so it earns the same card titled with its first non-empty line.
    let headline = match reported {
        None => headline_of(source),
        Some((_, report)) if report.is_empty() => "Missing final report".to_owned(),
        Some((_, report)) => headline_of(report),
    };
    let body = match reported {
        None => source.to_owned(),
        Some((_, _)) if missing_report => "Status received without a final report.".to_owned(),
        Some((_, report)) => report.to_owned(),
    };
    let (mut body, mut action) = match omp::recommended_action(&body)
        .filter(|(_, step)| !withhold_action || step.is_empty())
    {
        Some((rest, step)) => (rest, step),
        None => (body, String::new()),
    };
    if carried.is_some() {
        match omp::recommended_action(&text).filter(|(_, step)| !withhold_action || step.is_empty()) {
            Some((closing, closing_action)) => {
                body.push_str("\n\n---\n\n");
                body.push_str(&closing);
                if !closing_action.is_empty() {
                    if !action.is_empty() {
                        action.push_str("\n\n");
                    }
                    action.push_str(&closing_action);
                }
            }
            None => {
                body.push_str("\n\n---\n\n");
                body.push_str(&text);
            }
        }
    }
    // An agent-done report that closes with `Recommended action: none` names no step for the
    // user to pick up. Nothing is open, so the card reports the work as done. A report with no
    // recommended-action line keeps the warning, because it never named its open step.
    let closed_report = matches!(reported, Some((omp::FinalStatus::AgentDone, _)))
        && action.is_empty()
        && (omp::recommended_action(source).is_some()
            || (carried.is_some() && omp::recommended_action(&text).is_some()));
    let title = match reported {
        None => headline,
        Some((_, _)) if missing_report => headline,
        Some((omp::FinalStatus::Complete, _)) => format!("Done: {headline}"),
        Some((omp::FinalStatus::AgentDone, _)) if closed_report => format!("Done: {headline}"),
        Some((omp::FinalStatus::AgentDone, _)) => format!("Agent done: {headline}"),
        Some((omp::FinalStatus::Incomplete, _)) => format!("Incomplete: {headline}"),
    };
    let level = match reported {
        None => "info",
        Some((omp::FinalStatus::Complete, _)) if !missing_report => "info",
        Some((omp::FinalStatus::AgentDone, _)) if closed_report && !missing_report => "info",
        Some(_) => "warn",
    };
    Turn { title, body: Some(body), level, action, report_due: reported.is_none() && has_trail }
}

/// Where a launched session's work came from. The situation is the title of the card whose
/// answer last sent a brief into this pane, and the task is that answer with its note. A
/// report the user answers again keeps the first card's title, so a chain of follow-ups stays on
/// one topic. Only launches since the pane's pty appeared count, because tmux reuses pane ids
/// after a server restart.
fn pane_trail(db: &Db, pane: &tmux::Pane) -> (Star, bool) {
    pane.pty_born_ms().map_or((Star::default(), false), |born| trail(db, &pane.id, born))
}

/// The launch trail of `key`, a pane id or `dsh:<sid>`, from launches at or after `since` ms.
fn trail(db: &Db, key: &str, since: i64) -> (Star, bool) {
    let Some((origin, swipe)) = db.pane_launch_since(key, since).ok().flatten() else {
        return (Star::default(), false);
    };
    let choice = swipe.text.or_else(|| {
        swipe
            .option
            .and_then(|index| origin.options.get(index))
            .map(|option| option.label.clone())
    });
    // A read-only launch asked the session for an answer, so its report's recommended
    // step is the one thing still waiting on the user. Any other launch sent the
    // session to carry the work out, so its report owes nothing.
    let investigation = choice
        .as_deref()
        .is_some_and(|label| option_mode(&CardOption { label: label.to_owned(), ..CardOption::default() })
            == DispatchMode::Investigate);
    let note = swipe
        .note
        .as_deref()
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(|note| format!("Note: {note}"));
    let situation = if origin.kind == Kind::Info && !origin.star.situation.is_empty() {
        origin.star.situation
    } else {
        origin.title
    };
    (
        Star {
            situation,
            task: choice.into_iter().chain(note).collect::<Vec<_>>().join(" · "),
            ..Star::default()
        },
        investigation,
    )
}

/// The prefix and entry of a reply card id: `done:%3:a1` and `done:%12:a1` name one
/// reply under two pane ids, which `reply_has_card` judges instead of a supersede.
fn reply_key(id: &str) -> Option<(&str, &str)> {
    let (prefix, rest) = id.split_once(':')?;
    let (_, entry) = rest.rsplit_once(':')?;
    Some((prefix, entry))
}

/// Swipe `done`, reason `superseded`, every queued info card other than `keep` whose
/// first ref is `transcript`, and every queued info card of `pane` with no ref
/// (queued before info cards carried their transcript). Pane ids restart at `%0` with
/// each tmux server and cards outlive their pane, so the transcript, not the pane id,
/// names the session a card came from. A card of the same reply under another pane id
/// is left to `reply_has_card`. A card an earlier pane with this id raised before the
/// pane's pty appeared closes as `pane exited`. With no `pane` (a dsh session), only transcript
/// matches count. False means a failed read or write.
fn supersede_info_cards(
    db: &Db,
    keep: &str,
    transcript: &str,
    pane: Option<&tmux::Pane>,
    written: &mut usize,
) -> bool {
    let Ok(open) = db.queue(events::now_ms()) else {
        return false;
    };
    let mut succeeded = true;
    let born = pane.and_then(tmux::Pane::pty_born_ms);
    for older in open.iter().filter(|older| {
        older.kind == Kind::Info
            && older.id != keep
            && reply_key(&older.id).is_none_or(|key| Some(key) != reply_key(keep))
            && match older.refs.first() {
                Some(first) => first == transcript,
                None => pane.is_some_and(|pane| older.source.pane.as_deref() == Some(pane.id.as_str())),
            }
    }) {
        let answer_lock = db.answer_lock(&older.id);
        let _answer_guard = answer_lock.lock().unwrap();
        let Ok(Some((_, swipe))) = db.card(&older.id) else {
            succeeded = false;
            continue;
        };
        if !queued_for_answer(swipe.as_ref()) {
            continue;
        }
        let same_pane = pane.is_some_and(|pane| older.source.pane.as_deref() == Some(pane.id.as_str()));
        let reason = if same_pane && born.is_some_and(|born| older.created_ms < born) {
            "pane exited"
        } else {
            "superseded"
        };
        if record_swipe(db, &older.id, "done", None, None, None, Some(reason.to_owned())) {
            *written += 1;
        } else {
            succeeded = false;
        }
    }
    succeeded
}

/// What a session whose omp process is gone still owes the inbox. A dispatched session
/// with no report since its latest brief raises `Unreported:`. When `seen_live` (the
/// tracker read this transcript while omp ran in the pane), a final reply or error queues
/// the card a live pane would get, and a run cut off mid-turn raises `Ended mid-turn:`.
/// An interrupt, a session with no reply, or a transcript known only from its breadcrumb
/// adds nothing more. Card-id dedup keeps repeat audits harmless. False means retry.
fn audit_ended_session(
    db: &Db,
    pane: &tmux::Pane,
    panes: &[tmux::Pane],
    path: &Path,
    seen_live: bool,
    written: &mut usize,
) -> bool {
    let audit = match omp::dispatch_audit(path) {
        Ok(audit) => audit,
        // A deleted transcript has nothing left to audit.
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    if audit.dispatched && !audit.reported {
        if matches!(omp::turn_end(path), Ok(Some(omp::TurnEnd::Aborted))) {
            return true;
        }
        return queue_ended_card(db, pane, panes, path, "Unreported", audit.entry_id, audit.last_text, written);
    }
    if !seen_live {
        return true;
    }
    match omp::turn_end(path) {
        Err(_) => false,
        Ok(None | Some(omp::TurnEnd::Aborted)) => true,
        Ok(Some(omp::TurnEnd::Running)) => {
            queue_ended_card(db, pane, panes, path, "Ended mid-turn", audit.entry_id, audit.last_text, written)
        }
        Ok(Some(end)) => queue_turn_card(db, pane, panes, path, end, written),
    }
}

/// One warning card for an ended session: id `exited:<pane>:<entry or ->`, title
/// `<prefix>: <first non-blank report line, cut to 80>` or `<prefix>: no reply recorded`,
/// the stripped report as body, and the transcript as ref. It supersedes the reply card
/// that holds the same text.
#[allow(clippy::too_many_arguments)]
fn queue_ended_card(
    db: &Db,
    pane: &tmux::Pane,
    panes: &[tmux::Pane],
    path: &Path,
    prefix: &str,
    entry: Option<String>,
    text: Option<String>,
    written: &mut usize,
) -> bool {
    let entry = entry.unwrap_or_else(|| "-".to_owned());
    let (text, action) = match text.map(|text| omp::redact_credentials(&text)) {
        Some(text) => match omp::recommended_action(&text) {
            Some((body, action)) => (Some(body), action),
            None => (Some(text), String::new()),
        },
        None => (None, String::new()),
    };
    let headline = text
        .as_deref()
        .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
        .map(|line| truncate_chars(line.to_owned(), 80))
        .unwrap_or_else(|| "no reply recorded".to_owned());
    let body = text.unwrap_or_else(|| "The session ended without an assistant reply.".to_owned());
    let card = Card {
        id: format!("exited:{}:{entry}", pane.id),
        kind: Kind::Info,
        priority: 3,
        created_ms: events::now_ms(),
        source: pane_source(pane),
        title: format!("{prefix}: {headline}"),
        star: Star { action, ..Star::default() },
        options: vec![CardOption {
            label: "Acknowledge".to_owned(),
            description: None,
            direction: None,
            recommended: false,
        }],
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: Some(body),
        refs: vec![path.display().to_string()],
        level: Some("warn".to_owned()),
        tool_call_id: None,
        report_due: false,
        engine: None,
    };
    if entry != "-" {
        match reply_has_card(db, panes, "exited", &entry, &card.id, written) {
            None => return false,
            Some(true) => return true,
            Some(false) => {}
        }
    }
    if !supersede_info_cards(db, &card.id, &card.refs[0], Some(pane), written) {
        return false;
    }
    match db.insert_card(&card) {
        Ok(inserted) => {
            *written += usize::from(inserted);
            true
        }
        Err(_) => false,
    }
}

fn add_plan_card(
    db: &Db,
    pane: &tmux::Pane,
    proposal: &omp::Propose,
    written: &mut usize,
) -> bool {
    let screen = match tmux::screen(&pane.id) {
        Ok(screen) => screen,
        Err(_) => return false,
    };
    if !omp::plan_review_open(&screen) {
        return false;
    }
    let body = match fs::read_to_string(&proposal.plan_path) {
        Ok(body) => omp::redact(&body),
        Err(_) => return false,
    };
    let source = pane_source(pane);
    let title = if proposal.title.trim().is_empty() {
        source.label.clone()
    } else {
        proposal.title.clone()
    };
    let card = Card {
        id: format!("plan:{}:{}", pane.id, proposal.tool_call_id),
        kind: Kind::Plan,
        priority: 1,
        created_ms: events::now_ms(),
        source,
        title: format!("Plan ready: {title}"),
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
        refs: Vec::new(),
        level: None,
        tool_call_id: None,
        report_due: false,
        engine: None,
    };
    match db.insert_card(&card) {
        Ok(inserted) => *written += usize::from(inserted),
        Err(_) => return false,
    }
    let Ok(cards) = db.unresolved_cards_for_pane(&pane.id) else {
        return false;
    };
    finish_cards(
        db,
        cards
            .into_iter()
            .filter(|older| older.kind == Kind::Plan && older.id != card.id)
            .collect(),
        "superseded",
        written,
    )
}

fn finish_closed_plan_cards(
    db: &Db,
    unresolved_panes: Option<&HashSet<String>>,
    pane: &tmux::Pane,
    written: &mut usize,
) -> bool {
    if unresolved_panes.is_some_and(|panes| !panes.contains(&pane.id)) {
        return true;
    }
    let Ok(cards) = db.unresolved_cards_for_pane(&pane.id) else {
        return false;
    };
    let cards = cards
        .into_iter()
        .filter(|card| card.kind == Kind::Plan)
        .collect::<Vec<_>>();
    if cards.is_empty() {
        return true;
    }
    let screen = match tmux::screen(&pane.id) {
        Ok(screen) => screen,
        Err(_) => return false,
    };
    if omp::plan_review_open(&screen) {
        return true;
    }
    finish_cards(db, cards, "answered in pane", written)
}

/// Swipe each card `done` with `reason` unless it was already handled. Cards still queued
/// or deferred count; a card another writer resolved meanwhile is left alone. Returns false
/// when a read or write failed, so the caller retries on its next tick.
fn finish_cards(db: &Db, cards: Vec<Card>, reason: &str, written: &mut usize) -> bool {
    let mut succeeded = true;
    for card in cards {
        let answer_lock = db.answer_lock(&card.id);
        let _answer_guard = answer_lock.lock().unwrap();
        let Ok(Some((_, swipe))) = db.card(&card.id) else {
            succeeded = false;
            continue;
        };
        if swipe
            .as_ref()
            .is_some_and(|swipe| swipe.action != "later")
        {
            continue;
        }
        if record_swipe(
            db,
            &card.id,
            "done",
            None,
            None,
            None,
            Some(reason.to_owned()),
        ) {
            *written += 1;
        } else {
            succeeded = false;
        }
    }
    succeeded
}

fn finish_asking(
    db: &Db,
    pane: &tmux::Pane,
    except_call_id: Option<&str>,
    written: &mut usize,
) -> bool {
    finish_asking_after_recheck(db, pane, except_call_id, written, || {})
}

fn finish_asking_after_recheck(
    db: &Db,
    pane: &tmux::Pane,
    except_call_id: Option<&str>,
    written: &mut usize,
    mut after_recheck: impl FnMut(),
) -> bool {
    let Ok(cards) = db.unresolved_cards_for_pane(&pane.id) else {
        return false;
    };
    let transcript = omp::transcript_path(pane);
    let mut succeeded = true;
    for card in cards.into_iter().filter(|card| {
        card.kind == Kind::Ask
            && except_call_id.map_or(true, |id| card.tool_call_id.as_deref() != Some(id))
    }) {
        let answer_lock = db.answer_lock(&card.id);
        let _answer_guard = answer_lock.lock().unwrap();
        let Ok(Some((_, swipe))) = db.card(&card.id) else {
            succeeded = false;
            continue;
        };
        if swipe
            .as_ref()
            .is_some_and(|swipe| swipe.action != "later")
        {
            continue;
        }
        after_recheck();
        let selected = card.tool_call_id.as_deref().and_then(|id| {
            let path = transcript.as_ref()?;
            omp::ask_result(path, id).ok().flatten()
        });
        let (option, text) = match selected.filter(|options| !options.is_empty()) {
            Some(options) => {
                let option = options.first().and_then(|label| {
                    card.options
                        .iter()
                        .position(|option| option.label.as_str() == label.as_str())
                });
                (option, Some(options.join(", ")))
            }
            None => (None, None),
        };
        if record_swipe(
            db,
            &card.id,
            "done",
            option,
            text,
            None,
            Some("answered in pane".to_owned()),
        ) {
            *written += 1;
        } else {
            succeeded = false;
        }
    }
    succeeded
}

/// Mark unresolved info cards from this pane instance as done when its session resumes.
/// Return false when database access fails so the caller can retry.
fn finish_resumed_cards(db: &Db, pane: &tmux::Pane, written: &mut usize) -> bool {
    let Ok(cards) = db.unresolved_cards() else {
        return false;
    };
    let cards = cards
        .into_iter()
        .filter(|card| card.kind == Kind::Info && raised_by_live_pane(card, std::slice::from_ref(pane)))
        .collect();
    finish_cards(db, cards, "resumed in pane", written)
}

/// Ask and plan cards of a gone pane close `pane exited`, because their answers need the
/// live pane. With `close_info`, its queued and deferred information cards close too.
fn finish_gone(db: &Db, pane: &str, close_info: bool, written: &mut usize) -> bool {
    let Ok(mut cards) = db.cards_for_exited_pane(pane) else {
        return false;
    };
    if close_info {
        let Ok(open) = db.unresolved_cards_for_pane(pane) else {
            return false;
        };
        cards.extend(open.into_iter().filter(|card| matches!(card.kind, Kind::Info)));
    }
    finish_cards(db, cards, "pane exited", written)
}

/// Whether a listed pane still holds the card's reply: same pane id, and the card
/// was raised no earlier than that pane's pty appeared.
fn raised_by_live_pane(card: &Card, panes: &[tmux::Pane]) -> bool {
    panes.iter().any(|pane| {
        Some(pane.id.as_str()) == card.source.pane.as_deref()
            && pane
                .pty_born_ms()
                .is_none_or(|born| card.created_ms >= born)
    })
}

/// Whether the reply named by `prefix` and `entry` already has a card, looking
/// past the pane id a tmux restart renumbers. Every earlier card of the reply is
/// judged by its latest swipe: unswiped on a live pane means the reply is on
/// screen, `pane exited` does not count, and any other swipe is the user's verdict
/// or the system's supersede, so the reply was dealt with. A dead pane's queued
/// card is closed as `pane exited` on the way. `None` means a store error or a
/// failed swipe, and the caller keeps its retry.
fn reply_has_card(
    db: &Db,
    panes: &[tmux::Pane],
    prefix: &str,
    entry: &str,
    card_id: &str,
    written: &mut usize,
) -> Option<bool> {
    let priors = db.reply_cards(prefix, entry).ok()?;
    let mut has_card = false;
    for (prior, swipe) in priors {
        if prior.id == card_id {
            continue;
        }
        match swipe.as_ref().map(|swipe| (swipe.action.as_str(), swipe.reason.as_deref())) {
            None => {
                if raised_by_live_pane(&prior, panes) {
                    has_card = true;
                } else {
                    let answer_lock = db.answer_lock(&prior.id);
                    let _answer_guard = answer_lock.lock().unwrap();
                    let Ok(Some((_, swipe))) = db.card(&prior.id) else {
                        return None;
                    };
                    if swipe.is_none() {
                        if !record_swipe(
                            db,
                            &prior.id,
                            "done",
                            None,
                            None,
                            None,
                            Some("pane exited".to_owned()),
                        ) {
                            return None;
                        }
                        *written += 1;
                    } else {
                        // A verdict landed while this pass waited for the lock; it counts.
                        has_card = true;
                    }
                }
            }
            Some((_, Some("pane exited"))) => {}
            Some(_) => has_card = true,
        }
    }
    Some(has_card)
}

fn answer_ask(db: &Db, card: &Card, req: &AnswerReq, launch: &Launch) -> Result<(), String> {
    let answer_lock = db.answer_lock(&card.id);
    let _answer_guard = answer_lock.lock().unwrap();
    let swipe = db
        .card(&card.id)
        .map_err(|error| error.to_string())?
        .and_then(|(_, swipe)| swipe);
    if !queued_for_answer(swipe.as_ref()) {
        return Err("card is not queued".to_owned());
    }
    if !card.answerable {
        return Err("ask card is not answerable".to_owned());
    }
    if let Some(sid) = dsh::session_id(card) {
        return dsh_track::answer_ask(db, card, sid, req, launch);
    }
    let pane = card
        .source
        .pane
        .as_deref()
        .ok_or_else(|| "ask card has no pane".to_owned())?;
    let panes = tmux::list_panes().map_err(|error| error.to_string())?;
    let target = panes
        .iter()
        .find(|candidate| candidate.id == pane)
        .ok_or_else(|| "pane not found".to_owned())?;
    if omp::classify(&target.title) != PaneState::Asking {
        return Err("pane is not asking".to_owned());
    }
    let path = omp::transcript_path(target)
        .ok_or_else(|| "ask transcript not found".to_owned())?;
    let active_call = omp::pending_ask(&path)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "ask dialog not found".to_owned())?;
    if card.tool_call_id.as_deref() != Some(active_call.tool_call_id.as_str()) {
        return Err("ask card no longer matches active dialog".to_owned());
    }
    if let Some(form) = &active_call.form {
        return answer_html_form(db, card, req, pane, form);
    }
    if !card.questions.is_empty() {
        return answer_questions(db, card, req, pane);
    }
    let screen = tmux::capture(pane, 60).map_err(|error| error.to_string())?;
    let cursor = omp::dialog_cursor(&screen)
        .ok_or_else(|| "ask dialog not found".to_owned())?
        .0;
    let row = answer_row(card.options.len(), req.option, req.text.as_deref())?;
    type_row(pane, cursor, row, req.text.as_deref())?;
    thread::sleep(DIALOG_SETTLE);
    if tmux::capture(pane, 60)
        .map_err(|error| error.to_string())?
        .contains("╭─ Ask")
    {
        return Err("dialog still open".to_owned());
    }
    if record_swipe(
        db,
        &card.id,
        "done",
        (row < card.options.len()).then_some(row),
        req.text.clone(),
        req.note.clone(),
        Some("typed".to_owned()),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Time omp's dialog needs to redraw after a key press before a capture reads it.
const DIALOG_SETTLE: Duration = Duration::from_millis(400);

/// Map one answer to the dialog row to press: an option index, or the Other row
/// (one past the last option) for custom text. Text must be nonempty after
/// trimming, because omp drops a whitespace-only custom answer.
fn answer_row(options: usize, option: Option<usize>, text: Option<&str>) -> Result<usize, String> {
    let other = options;
    match (option, text) {
        (Some(index), Some(text)) => {
            if index != other || text.trim().is_empty() {
                return Err("text requires the Other option and nonempty text".to_owned());
            }
            Ok(index)
        }
        (Some(index), None) if index < other => Ok(index),
        (Some(_), None) => return Err("option is out of range or selects Other".to_owned()),
        (None, Some(text)) if !text.trim().is_empty() => Ok(other),
        (None, Some(_)) => return Err("Other requires nonempty text".to_owned()),
        (None, None) => return Err("ask answer needs an option or text".to_owned()),
    }
}

/// Move the dialog cursor from `cursor` to `row`, press Enter, and for custom
/// text settle, then type it (which submits the prompt with its own Enter).
fn type_row(pane: &str, cursor: usize, row: usize, text: Option<&str>) -> Result<(), String> {
    let (key, moves) = if cursor < row {
        ("Down", row - cursor)
    } else {
        ("Up", cursor - row)
    };
    for _ in 0..moves {
        tmux::send_keys(pane, &[key]).map_err(|error| error.to_string())?;
    }
    tmux::send_keys(pane, &["Enter"]).map_err(|error| error.to_string())?;
    if let Some(text) = text {
        thread::sleep(DIALOG_SETTLE);
        tmux::send_text(pane, text).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// One key press, or text pasted into the focused field, while answering an HTML ask.
#[derive(Debug, Clone, Copy)]
enum FormStep<'a> {
    Key(&'static str),
    Paste(&'a str),
}

/// Keys that answer one HTML control. `row` is its validated option row; `row == options` is the
/// custom-text row and `text` is that text. Every key starts from the control's first stop.
fn html_steps<'a>(
    control: &omp::HtmlControl,
    options: usize,
    row: usize,
    text: Option<&'a str>,
) -> Result<Vec<FormStep<'a>>, String> {
    use FormStep::{Key, Paste};
    let custom = row == options;
    let mut steps = Vec::new();
    match *control {
        omp::HtmlControl::Radio { enabled, other, .. } if custom => {
            if !other {
                return Err("question does not accept Other".to_owned());
            }
            steps.extend(std::iter::repeat_n(Key("Down"), enabled));
            steps.extend([Key("C-u"), Key("C-k"), Paste(text.unwrap_or_default()), Key("Enter")]);
        }
        omp::HtmlControl::Radio { leave_unselected: true, .. } if row == 0 => steps.push(Key("Tab")),
        omp::HtmlControl::Radio { leave_unselected, .. } => {
            steps.extend(std::iter::repeat_n(Key("Down"), row - usize::from(leave_unselected)));
            steps.push(Key("Enter"));
        }
        omp::HtmlControl::Select { .. } if custom => {
            return Err("select does not accept Other".to_owned());
        }
        omp::HtmlControl::Select { leave_unselected: true, .. } if row == 0 => steps.push(Key("Enter")),
        omp::HtmlControl::Select { enabled, leave_unselected } => {
            steps.push(Key("Space"));
            steps.extend(std::iter::repeat_n(Key("Up"), enabled));
            steps.extend(std::iter::repeat_n(Key("Down"), row - usize::from(leave_unselected)));
            steps.extend([Key("Enter"), Key("Enter")]);
        }
        omp::HtmlControl::Checkbox if custom => {
            return Err("checkbox does not accept Other".to_owned());
        }
        omp::HtmlControl::Checkbox => {
            if row == 1 {
                steps.push(Key("Space"));
            }
            steps.push(Key("Enter"));
        }
        omp::HtmlControl::Text { multiline, initial_lines } if custom => {
            if multiline {
                steps.extend(std::iter::repeat_n(Key("C-u"), 2 * initial_lines));
                steps.push(Key("C-a"));
                steps.extend(std::iter::repeat_n(Key("C-k"), 2 * initial_lines));
            } else {
                steps.extend([Key("C-u"), Key("C-k")]);
            }
            steps.extend([Paste(text.unwrap_or_default()), Key("Enter")]);
        }
        omp::HtmlControl::Text { .. } => steps.push(Key("Enter")),
        omp::HtmlControl::Unsupported => {
            return Err("question is read-only; answer in the pane".to_owned());
        }
    }
    Ok(steps)
}

/// Answer an HTML ask (`ask.arguments.html`) field by field. Validate every answer before the first
/// key. Then Down releases the composer guard, one BTab per field returns to the first field, each
/// field's keys run with a settle, and Enter submits. A dialog still open leaves the card queued.
fn answer_html_form(
    db: &Db,
    card: &Card,
    req: &AnswerReq,
    pane: &str,
    form: &omp::HtmlAskForm,
) -> Result<(), String> {
    let (counts, answers): (Vec<usize>, Vec<events::Answer>) = if card.questions.is_empty() {
        (
            vec![card.options.len()],
            vec![events::Answer {
                option: req.option,
                text: req.text.clone(),
            }],
        )
    } else {
        (
            card.questions.iter().map(|question| question.options.len()).collect(),
            req.answers.clone(),
        )
    };
    if counts.len() != form.controls.len() || answers.len() != form.controls.len() {
        return Err("ask answer needs one answer per question".to_owned());
    }
    let mut rows = Vec::with_capacity(answers.len());
    let mut fields = Vec::with_capacity(answers.len());
    for ((control, count), answer) in form.controls.iter().zip(&counts).zip(&answers) {
        let row = answer_row(*count, answer.option, answer.text.as_deref())?;
        fields.push(html_steps(control, *count, row, answer.text.as_deref())?);
        rows.push(row);
    }
    let screen = tmux::capture(pane, 60).map_err(|error| error.to_string())?;
    if !screen.contains("╭─ Ask") {
        return Err("ask dialog not found".to_owned());
    }
    tmux::send_keys(pane, &["Down"]).map_err(|error| error.to_string())?;
    for _ in &form.controls {
        tmux::send_keys(pane, &["BTab"]).map_err(|error| error.to_string())?;
    }
    thread::sleep(DIALOG_SETTLE);
    for steps in fields {
        for step in steps {
            let sent = match step {
                FormStep::Key(key) => tmux::send_keys(pane, &[key]),
                FormStep::Paste(text) => tmux::paste_text(pane, text),
            };
            sent.map_err(|error| error.to_string())?;
        }
        thread::sleep(DIALOG_SETTLE);
    }
    tmux::send_keys(pane, &["Enter"]).map_err(|error| error.to_string())?;
    thread::sleep(DIALOG_SETTLE);
    if tmux::capture(pane, 60)
        .map_err(|error| error.to_string())?
        .contains("╭─ Ask")
    {
        return Err("dialog still open".to_owned());
    }
    let (option, text) = if card.questions.is_empty() {
        ((rows[0] < counts[0]).then_some(rows[0]), req.text.clone())
    } else {
        let summary = card
            .questions
            .iter()
            .zip(&answers)
            .map(|(question, answer)| {
                let shown = answer
                    .option
                    .and_then(|index| question.options.get(index))
                    .map(|option| option.label.clone())
                    .or_else(|| answer.text.clone())
                    .unwrap_or_default();
                format!("{}: {}", question.label, shown)
            })
            .collect::<Vec<_>>()
            .join("; ");
        (None, Some(summary))
    };
    if record_swipe(
        db,
        &card.id,
        "done",
        option,
        text,
        req.note.clone(),
        Some("typed".to_owned()),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Answer a multi-question ask card in its pane: walk to the review tab, enter
/// every question from its first tab, confirm the review, and submit. Any
/// mismatch sends no Enter on the review and leaves the card queued.
fn answer_questions(db: &Db, card: &Card, req: &AnswerReq, pane: &str) -> Result<(), String> {
    if req.answers.len() != card.questions.len() {
        return Err("ask answer needs one answer per question".to_owned());
    }
    let rows: Vec<usize> = card
        .questions
        .iter()
        .zip(&req.answers)
        .map(|(question, answer)| answer_row(question.options.len(), answer.option, answer.text.as_deref()))
        .collect::<Result<_, _>>()?;
    let screen = tmux::capture(pane, 60).map_err(|error| error.to_string())?;
    if omp::dialog_cursor(&screen).is_none() && omp::review_answers(&screen).is_none() {
        return Err("ask dialog not found".to_owned());
    }
    // A prompt editor holding text swallows every key until Up or Down; release
    // that guard before walking tabs. A moved cursor is harmless: every step
    // captures the screen again.
    tmux::send_keys(pane, &["Up"]).map_err(|error| error.to_string())?;
    thread::sleep(DIALOG_SETTLE);
    if !tmux::capture(pane, 60)
        .map_err(|error| error.to_string())?
        .contains("╭─ Ask")
    {
        return Err("ask dialog not found".to_owned());
    }
    let mut review = None;
    for _ in 0..card.questions.len() {
        tmux::send_keys(pane, &["BTab"]).map_err(|error| error.to_string())?;
        thread::sleep(DIALOG_SETTLE);
        let screen = tmux::capture(pane, 60).map_err(|error| error.to_string())?;
        if let Some(rows) = omp::review_answers(&screen) {
            review = Some(rows);
            break;
        }
        if !screen.contains("╭─ Ask") {
            return Err("ask dialog not found".to_owned());
        }
    }
    review.ok_or_else(|| "ask review tab not found".to_owned())?;
    tmux::send_keys(pane, &["Tab"]).map_err(|error| error.to_string())?;
    for (index, (question, answer)) in
        card.questions.iter().zip(&req.answers).enumerate()
    {
        thread::sleep(DIALOG_SETTLE);
        let screen = tmux::capture(pane, 60).map_err(|error| error.to_string())?;
        let cursor = omp::dialog_cursor(&screen)
            .filter(|(_, total)| *total == question.options.len() + 1)
            .map(|(cursor, _)| cursor)
            .ok_or_else(|| format!("ask dialog does not show question {}", index + 1))?;
        type_row(pane, cursor, rows[index], answer.text.as_deref())?;
    }
    thread::sleep(DIALOG_SETTLE);
    let screen = tmux::capture(pane, 60).map_err(|error| error.to_string())?;
    if !omp::review_answers(&screen)
        .is_some_and(|shown| review_shows(&shown, card, &req.answers))
    {
        return Err("ask review does not match the answers".to_owned());
    }
    tmux::send_keys(pane, &["Enter"]).map_err(|error| error.to_string())?;
    thread::sleep(DIALOG_SETTLE);
    if tmux::capture(pane, 60)
        .map_err(|error| error.to_string())?
        .contains("╭─ Ask")
    {
        return Err("dialog still open".to_owned());
    }
    let text = card
        .questions
        .iter()
        .zip(&req.answers)
        .map(|(question, answer)| {
            let answer = answer
                .option
                .and_then(|index| question.options.get(index))
                .map(|option| option.label.clone())
                .or_else(|| answer.text.clone())
                .unwrap_or_default();
            format!("{}: {}", question.label, answer)
        })
        .collect::<Vec<_>>()
        .join("; ");
    if record_swipe(
        db,
        &card.id,
        "done",
        None,
        Some(text),
        req.note.clone(),
        Some("typed".to_owned()),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// True when the review rows the pane shows match one answer per question:
/// `label: value` rows where value is the option label (with or without
/// `(Recommended)`) or `“text”` with whitespace collapsed; a row truncated
/// with `…` matches when the visible part prefixes an accepted value.
fn review_shows(rows: &[String], card: &Card, answers: &[events::Answer]) -> bool {
    rows.len() == card.questions.len()
        && card
            .questions
            .iter()
            .zip(answers)
            .zip(rows)
            .all(|((question, answer), row)| {
                let value = row
                    .strip_prefix(&format!("{}: ", question.label))
                    .or_else(|| row.split_once(": ").map(|(_, rest)| rest));
                let Some(value) = value else {
                    return false;
                };
                let mut accepted = answer
                    .option
                    .and_then(|index| question.options.get(index))
                    .map(|option| vec![option.label.clone(), format!("{} (Recommended)", option.label)])
                    .unwrap_or_default();
                if let Some(text) = answer.text.as_deref().filter(|text| !text.trim().is_empty()) {
                    accepted.push(format!(
                        "“{}”",
                        text.split_whitespace().collect::<Vec<_>>().join(" ")
                    ));
                }
                accepted.iter().any(|form| {
                    value == form
                        || value.strip_suffix('…').is_some_and(|prefix| {
                            !prefix.is_empty() && form.starts_with(prefix)
                        })
                })
            })
}

fn answer_plan(db: &Db, card: &Card, req: &AnswerReq, launch: &Launch) -> Result<(), String> {
    let answer_lock = db.answer_lock(&card.id);
    let _answer_guard = answer_lock.lock().unwrap();
    let swipe = db
        .card(&card.id)
        .map_err(|error| error.to_string())?
        .and_then(|(_, swipe)| swipe);
    if !queued_for_answer(swipe.as_ref()) {
        return Err("card is not queued".to_owned());
    }
    if !card.answerable {
        return Err("plan card is not answerable".to_owned());
    }
    let dsh_card = dsh::session_id(card).is_some();
    let engine = if dsh_card { Engine::Dsh } else { Engine::Omp };
    // omp's Plan Review slider lists roles only, so its execution model keeps the role's level.
    if !dsh_card && req.executor_thinking.is_some() {
        return Err("an omp plan review keeps the role's own thinking level".to_owned());
    }
    let executor = match engine {
        Engine::Dsh => dsh_selector(req.executor.as_deref())?,
        Engine::Omp => model_selector(req.executor.as_deref())?,
    };
    // An effort belongs to the executor model, which must be named with it.
    let executor_effort = executor_effort(engine, req.executor_thinking.as_deref())?;
    if executor_effort.is_some() && executor.is_none() {
        return Err("execution effort needs an execution model".to_owned());
    }
    let refinement = req
        .text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| req.note.as_deref().filter(|text| !text.trim().is_empty()))
        .map(str::trim);
    let (option, refinement) = match req.option {
        Some(0) if req.text.as_deref().is_none_or(str::is_empty) => (Some(0), None),
        Some(0) => return Err("Approve does not accept text".to_owned()),
        Some(1) if executor.is_none() => (Some(1), Some(refinement.ok_or_else(|| "Refine requires nonempty text".to_owned())?)),
        Some(1) => return Err("execution model requires Approve".to_owned()),
        None if executor.is_none() => (None, Some(refinement.ok_or_else(|| "Refine requires nonempty text".to_owned())?)),
        None => return Err("execution model requires Approve".to_owned()),
        Some(_) => return Err("unknown plan option".to_owned()),
    };
    if dsh_card {
        return dsh_track::answer_plan(
            db, card, req, option, refinement, executor, executor_effort, launch,
        );
    }
    let pane = card
        .source
        .pane
        .as_deref()
        .ok_or_else(|| "plan card has no pane".to_owned())?;
    let mut screen = tmux::screen(pane).map_err(|error| error.to_string())?;
    if !omp::plan_review_open(&screen) {
        return Err("plan review is not open".to_owned());
    }
    let recorded = if option == Some(0) {
        let role = if let Some(executor) = executor {
            let role = executor
                .strip_prefix('@')
                .filter(|role| !role.is_empty())
                .ok_or_else(|| "plan executor must be an omp @role".to_owned())?;
            let (roles, selected) = omp::plan_execution_models(&screen)
                .ok_or_else(|| "plan review has no execution model selector".to_owned())?;
            let target = roles
                .iter()
                .position(|candidate| candidate == role)
                .ok_or_else(|| format!("plan review does not offer {executor}"))?;
            if target != selected {
                let key = if target < selected { "Left" } else { "Right" };
                for _ in 0..target.abs_diff(selected) {
                    tmux::send_keys(pane, &[key]).map_err(|error| error.to_string())?;
                }
                thread::sleep(DIALOG_SETTLE);
                screen = tmux::screen(pane).map_err(|error| error.to_string())?;
                let (shown_roles, shown) = omp::plan_execution_models(&screen)
                    .filter(|_| omp::plan_review_open(&screen))
                    .ok_or_else(|| "plan review has no execution model selector".to_owned())?;
                if shown_roles.get(shown).map(String::as_str) != Some(role) {
                    return Err(format!("plan execution model is not {executor}"));
                }
            }
            Some(executor.to_owned())
        } else {
            None
        };
        tmux::send_keys(pane, &["Enter"]).map_err(|error| error.to_string())?;
        record_swipe(db, &card.id, "done", option, role, req.note.clone(), Some("typed".to_owned()))
    } else {
        refine_plan(pane, refinement.unwrap())?;
        record_swipe(
            db,
            &card.id,
            "done",
            option,
            refinement.map(str::to_owned),
            req.note.clone(),
            Some("typed".to_owned()),
        )
    };
    if recorded {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// Move Plan Review's `❯` onto `label` and confirm it. Every move is read back, because omp
/// skips a disabled row, so counting presses cannot land on the row. Confirms only when the
/// marked row is `label`, so a Refine can never submit another choice such as Approve.
fn confirm_plan_option(pane: &str, label: &str) -> Result<(), String> {
    for _ in 0..10 {
        let screen = tmux::screen(pane).map_err(|error| error.to_string())?;
        if !omp::plan_review_open(&screen) {
            return Err("plan review is not open".to_owned());
        }
        let (rows, selected) = omp::plan_review_options(&screen)
            .ok_or_else(|| "plan review has no options".to_owned())?;
        let target = rows
            .iter()
            .position(|row| row == label)
            .ok_or_else(|| format!("plan review does not offer {label}"))?;
        if target == selected {
            tmux::send_keys(pane, &["Enter"]).map_err(|error| error.to_string())?;
            return Ok(());
        }
        let key = if target > selected { "Down" } else { "Up" };
        tmux::send_keys(pane, &[key]).map_err(|error| error.to_string())?;
        thread::sleep(DIALOG_SETTLE);
    }
    Err(format!("plan review did not select {label}"))
}

/// Refine a plan: confirm Plan Review's own Refine row, wait for the overlay to close while plan
/// mode stays on, then type the feedback into the prompt. Escape is not a refine: it cancels the
/// review, and the keystrokes that follow can reach the overlay's highlighted row and approve the
/// plan instead of refining it.
fn refine_plan(pane: &str, refinement: &str) -> Result<(), String> {
    confirm_plan_option(pane, "Refine plan")?;
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(100));
        let screen = tmux::screen(pane).map_err(|error| error.to_string())?;
        if omp::plan_review_open(&screen) || !omp::plan_mode_on(&screen) {
            continue;
        }
        return tmux::send_text(pane, refinement).map_err(|error| error.to_string());
    }
    let screen = tmux::screen(pane).map_err(|error| error.to_string())?;
    if omp::plan_review_open(&screen) {
        return Err("plan review did not close".to_owned());
    }
    Err("plan review approved the plan instead of refining".to_owned())
}

fn answer_finding(
    db: &Db,
    card: &Card,
    req: &AnswerReq,
    launch: &Launch,
    image: Option<&Upload>,
) -> Result<(), String> {
    let answer_lock = db.answer_lock(&card.id);
    let _answer_guard = answer_lock.lock().unwrap();
    let swipe = db
        .card(&card.id)
        .map_err(|error| error.to_string())?
        .and_then(|(_, swipe)| swipe);
    if !queued_for_answer(swipe.as_ref()) {
        return Err("card is not queued".to_owned());
    }
    if !card.questions.is_empty() {
        return answer_finding_questions(db, card, req, launch, image, swipe.as_ref());
    }
    let index = req
        .option
        .ok_or_else(|| "finding answer needs an option".to_owned())?;
    let option = card
        .options
        .get(index)
        .ok_or_else(|| "finding option is out of range".to_owned())?;
    let chosen = req.mode.as_deref().and_then(DispatchMode::parse);
    let mode = chosen.unwrap_or_else(|| option_mode(option));
    let pane = finding_pane_for_answer(
        db,
        card,
        launch,
        image,
        swipe.as_ref(),
        req,
        (index, req.note.as_deref(), mode),
        || finding_brief(card, option, req.note.as_deref(), mode),
    )?;
    if record_swipe(
        db,
        &card.id,
        "done",
        Some(index),
        chosen.map(|mode| mode.swipe_text().to_owned()),
        req.note.clone(),
        Some(format!("launched {pane}")),
    ) {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// The pane a finding answer runs in: the pane already tagged with this answer, else a launch
/// an earlier attempt left pending, else a new session. A new session is recorded as pending
/// before it is tagged, so a retry after a failed tag finds it.
#[allow(clippy::too_many_arguments)]
fn finding_pane_for_answer(
    db: &Db,
    card: &Card,
    launch: &Launch,
    image: Option<&Upload>,
    swipe: Option<&Swipe>,
    req: &AnswerReq,
    (option, note, mode): (usize, Option<&str>, DispatchMode),
    brief: impl FnOnce() -> String,
) -> Result<String, String> {
    // dsh sessions have no tmux pane to find or tag.
    let is_dsh = launch.engine == Engine::Dsh;
    let tagged_pane = if is_dsh {
        None
    } else {
        tmux::finding_pane(&card.id, option, note, mode).map_err(|error| error.to_string())?
    };
    let pending_pane = if tagged_pane.is_none() {
        recover_pending_finding_pane(launch, swipe, option, note, mode)?
    } else {
        None
    };
    let start = match mode {
        DispatchMode::Plan => Start::Plan,
        DispatchMode::PlanInto => {
            Start::PlanInto(req.executor.as_deref(), req.executor_thinking.as_deref())
        }
        DispatchMode::Implement => Start::Implement,
        DispatchMode::Investigate | DispatchMode::Act => Start::Goal,
    };
    let (pane, needs_tag) = if let Some(pane) = tagged_pane {
        (pane, false)
    } else if let Some(pane) = pending_pane {
        (pane, true)
    } else {
        let path = write_brief(db, brief())?;
        let pane = launch_brief(card, launch, &path, start, image, false)?;
        let pending = record_pending_finding_pane(db, &card.id, option, note, &pane, mode);
        if !is_dsh {
            tmux::tag_finding_pane(&pane, &card.id, option, note, mode)
                .map_err(|error| error.to_string())?;
        }
        pending?;
        (pane, false)
    };
    if needs_tag && !is_dsh {
        tmux::tag_finding_pane(&pane, &card.id, option, note, mode)
            .map_err(|error| error.to_string())?;
    }
    Ok(pane)
}

/// Answer a finding with native questions. One session takes every decision in its brief.
/// When every decision is Skip or Keep, the finding is recorded done without a launch.
fn answer_finding_questions(
    db: &Db,
    card: &Card,
    req: &AnswerReq,
    launch: &Launch,
    image: Option<&Upload>,
    swipe: Option<&Swipe>,
) -> Result<(), String> {
    if req.option.is_some() || req.text.is_some() {
        return Err("finding questions take answers, not a top-level option or text".to_owned());
    }
    if req.answers.len() != card.questions.len() {
        return Err("finding answer needs one answer per question".to_owned());
    }
    let mode = match req.mode.as_deref() {
        None => DispatchMode::Act,
        Some(value) => match DispatchMode::parse(value) {
            Some(DispatchMode::Investigate) | None => {
                return Err("finding questions run act, implement, plan, or plan-into".to_owned());
            }
            Some(mode) => mode,
        },
    };
    let decisions = card
        .questions
        .iter()
        .zip(&req.answers)
        .map(|(question, answer)| {
            finding_decision(question, answer).map(|decision| (question, decision))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let summary = decisions
        .iter()
        .enumerate()
        .map(|(index, (question, decision))| {
            format!("{}. {}: {}", index + 1, question.label, decision.label())
        })
        .collect::<Vec<_>>()
        .join("; ");
    let recorded = if decisions.iter().all(|(_, decision)| decision.takes_no_action()) {
        record_swipe(
            db,
            &card.id,
            "done",
            None,
            Some(summary),
            req.note.clone(),
            Some("answered".to_owned()),
        )
    } else {
        // The identity names every answer and the note, so a retry must repeat both exactly.
        let identity = format!("{summary}\n{}", req.note.as_deref().unwrap_or_default());
        // One past the last option, so no real option matches the tag.
        let pane = finding_pane_for_answer(
            db,
            card,
            launch,
            image,
            swipe,
            req,
            (card.options.len(), Some(identity.as_str()), mode),
            || finding_answers_brief(card, &decisions, req.note.as_deref(), mode),
        )?;
        record_swipe(
            db,
            &card.id,
            "done",
            None,
            Some(format!("{}: {summary}", mode.swipe_text())),
            req.note.clone(),
            Some(format!("launched {pane}")),
        )
    };
    if recorded {
        Ok(())
    } else {
        Err("could not record swipe".to_owned())
    }
}

/// What one answer chose: one of the question's options, or the custom text for Other.
enum FindingDecision<'a> {
    Authored(&'a CardOption),
    Other(&'a str),
}

impl FindingDecision<'_> {
    /// The decision as the summary names it.
    fn label(&self) -> String {
        match self {
            Self::Authored(option) => option.label.clone(),
            Self::Other(text) => format!("Other: {text}"),
        }
    }

    /// Skip and Keep leave the finding untouched.
    fn takes_no_action(&self) -> bool {
        match self {
            Self::Authored(option) => {
                option.label.eq_ignore_ascii_case("skip") || option.label.eq_ignore_ascii_case("keep")
            }
            Self::Other(_) => false,
        }
    }
}

/// Check one answer against its question and resolve it to the decision it names.
fn finding_decision<'a>(
    question: &'a CardQuestion,
    answer: &'a events::Answer,
) -> Result<FindingDecision<'a>, String> {
    let row = answer_row(question.options.len(), answer.option, answer.text.as_deref())?;
    if let Some(option) = question.options.get(row) {
        return Ok(FindingDecision::Authored(option));
    }
    if !question.allow_other {
        return Err(format!("{} does not take custom text", question.label));
    }
    Ok(FindingDecision::Other(answer.text.as_deref().unwrap_or_default().trim()))
}

const FINDING_ANSWER_RULES: &str = "Honor every decision independently. Do as recommended executes that finding's action. Other is Nick's direction for that finding. Investigate first is read-only for that finding: confirm its claims and report evidence, changing nothing for it. Skip and Keep take no action. Do not ask these questions again. Finish investigations before accepted changes, then report the outcome and evidence for every finding.";

/// The brief for a finding answered question by question: each question's full text with its
/// decision, then the rules for all of them, then the mode's usual sections.
fn finding_answers_brief(
    card: &Card,
    decisions: &[(&CardQuestion, FindingDecision<'_>)],
    note: Option<&str>,
    mode: DispatchMode,
) -> String {
    let mut brief = format!(
        "# {}\n\n## Situation\n{}\n\n## Task\n{}\n\n## {}\n{}\n\n## Result\n{}\n\n## Finding decisions\n",
        card.title,
        card.star.situation,
        card.star.task,
        action_heading(mode),
        card.star.action,
        card.star.result,
    );
    for (number, (question, decision)) in decisions.iter().enumerate() {
        let (chosen, direction) = match decision {
            FindingDecision::Authored(option) => (option.label.as_str(), ""),
            FindingDecision::Other(text) => ("Other", *text),
        };
        brief.push_str(&format!(
            "\n### {}. {}\n{}\nDecision: {chosen}\nDirection: {direction}\n",
            number + 1,
            question.label,
            question.question,
        ));
    }
    brief.push_str(&format!(
        "\n## Direction from the user\n{}\n\n## References\n",
        note.unwrap_or_default()
    ));
    push_refs(&mut brief, card);
    brief.push_str(&format!("\n## Rules for these decisions\n{FINDING_ANSWER_RULES}\n"));
    push_instructions(&mut brief, mode);
    brief.push_str(REPORT_STEP);
    if matches!(mode, DispatchMode::Plan | DispatchMode::PlanInto) {
        brief.push_str(&format!("\n{PLAN_REPORT_STEP}\n"));
    }
    brief
}

fn recover_pending_finding_pane(
    launch: &Launch,
    swipe: Option<&Swipe>,
    option: usize,
    note: Option<&str>,
    mode: DispatchMode,
) -> Result<Option<String>, String> {
    let Some(swipe) = swipe.filter(|swipe| swipe.action == "later") else {
        return Ok(None);
    };
    let Some(pane) = swipe
        .reason
        .as_deref()
        .and_then(|reason| reason.strip_prefix("launching "))
    else {
        return Ok(None);
    };
    // With the bridge unreachable, the session is unconfirmed and the finding launches anew.
    let listed = match pane.strip_prefix("dsh:") {
        Some(sid) => dsh::connect(launch.dsh_host.as_deref(), dsh::ACTION_TIMEOUT)
            .and_then(|client| client.sessions())
            .is_ok_and(|(_, sessions, _)| sessions.iter().any(|session| session.session_id == sid)),
        None => {
            let panes = tmux::list_panes().map_err(|error| error.to_string())?;
            panes.iter().any(|candidate| candidate.id == pane)
        }
    };
    if !listed {
        return Ok(None);
    }
    if swipe.option != Some(option)
        || swipe.note.as_deref() != note
        || swipe.text.as_deref() != Some(mode.swipe_text())
    {
        return Err("finding pane was launched with a different answer".to_owned());
    }
    Ok(Some(pane.to_owned()))
}

fn record_pending_finding_pane(
    db: &Db,
    card: &str,
    option: usize,
    note: Option<&str>,
    pane: &str,
    mode: DispatchMode,
) -> Result<(), String> {
    let now = events::now_ms();
    db.swipe(&Swipe {
        card: card.to_owned(),
        action: "later".to_owned(),
        option: Some(option),
        text: Some(mode.swipe_text().to_owned()),
        note: note.map(|note| note.to_owned()),
        until_ms: Some(now),
        reason: Some(format!("launching {pane}")),
        at_ms: now,
    })
    .map_err(|error| error.to_string())
}

fn pane_source(pane: &tmux::Pane) -> Source {
    let window = pane
        .target
        .rsplit_once('.')
        .map_or(pane.target.as_str(), |(window, _)| window);
    // Title is "π <state glyph> <session title>"; keep only the session title.
    let title = pane
        .title
        .strip_prefix("π ")
        .and_then(|rest| rest.split_once(' ').map(|(_, t)| t))
        .unwrap_or(&pane.title);
    Source {
        uri: format!("tmux://pane/{}", pane.id),
        label: format!("{window} {title}"),
        cwd: pane.cwd.clone(),
        pane: Some(pane.id.clone()),
        target: Some(pane.target.clone()),
    }
}

fn record_swipe(
    db: &Db,
    card: &str,
    action: &str,
    option: Option<usize>,
    text: Option<String>,
    note: Option<String>,
    reason: Option<String>,
) -> bool {
    db.swipe(&Swipe {
        card: card.to_owned(),
        action: action.to_owned(),
        option,
        text,
        note,
        until_ms: None,
        reason,
        at_ms: events::now_ms(),
    })
    .is_ok()
}

fn truncate_chars(mut text: String, limit: usize) -> String {
    if let Some((index, _)) = text.char_indices().nth(limit) {
        text.truncate(index);
        text.push('…');
    }
    text
}
fn brief_heading(brief: &Path, limit: usize) -> Result<String, String> {
    let text = fs::read_to_string(brief).map_err(|error| format!("could not read brief: {error}"))?;
    let heading = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    Ok(truncate_chars(heading.to_owned(), limit))
}

/// A finding option whose label opens with "Investigate" asks for an answer, not a change.
/// Every other option carries out the card's action.
fn option_mode(option: &CardOption) -> DispatchMode {
    if option.label.trim_start().to_lowercase().starts_with("investigate") {
        DispatchMode::Investigate
    } else {
        DispatchMode::Act
    }
}

/// An investigation reads the producer's action as a proposal, so the heading says so.
fn action_heading(mode: DispatchMode) -> &'static str {
    match mode {
        DispatchMode::Investigate => "Proposed action, not approved",
        DispatchMode::Act | DispatchMode::Plan | DispatchMode::PlanInto | DispatchMode::Implement => {
            "Action"
        }
    }
}

fn push_refs(brief: &mut String, card: &Card) {
    if card.refs.is_empty() {
        brief.push_str("- None\n");
    } else {
        for reference in &card.refs {
            brief.push_str("- ");
            brief.push_str(reference);
            brief.push('\n');
        }
    }
}

fn push_instructions(brief: &mut String, mode: DispatchMode) {
    brief.push_str("\n## Instructions\n");
    brief.push_str(match mode {
        DispatchMode::Investigate => INVESTIGATE_INSTRUCTION,
        DispatchMode::Act => ACT_INSTRUCTION,
        DispatchMode::Plan => PLAN_INSTRUCTION,
        DispatchMode::PlanInto => PLAN_INTO_INSTRUCTION,
        DispatchMode::Implement => IMPLEMENT_INSTRUCTION,
    });
    brief.push_str("\n\n");
    brief.push_str(match mode {
        DispatchMode::Investigate => INVESTIGATE_STATUS_CONTRACT,
        DispatchMode::Act
        | DispatchMode::Plan
        | DispatchMode::PlanInto
        | DispatchMode::Implement => ENVOY_STATUS_CONTRACT,
    });
}

fn finding_brief(card: &Card, option: &CardOption, note: Option<&str>, mode: DispatchMode) -> String {
    let mut brief = format!(
        "# {}\n\n## Situation\n{}\n\n## Task\n{}\n\n## {}\n{}\n\n## Result\n{}\n\n## Chosen option\n{}\n\n## Direction\n{}\n\n## Direction from the user\n{}\n\n## References\n",
        card.title,
        card.star.situation,
        card.star.task,
        action_heading(mode),
        card.star.action,
        card.star.result,
        option.label,
        option.direction.as_deref().unwrap_or_default(),
        note.unwrap_or_default(),
    );
    push_refs(&mut brief, card);
    push_instructions(&mut brief, mode);
    brief.push_str(REPORT_STEP);
    if matches!(mode, DispatchMode::Plan | DispatchMode::PlanInto) {
        brief.push_str(&format!("\n{PLAN_REPORT_STEP}\n"));
    }
    brief
}

const ENVOY_STATUS_CONTRACT: &str = "When finished, end with a final reply whose first line is `ENVOY_STATUS: complete`, `ENVOY_STATUS: agent-done` or `ENVOY_STATUS: incomplete`. `complete` means the work shipped through the project's normal pipeline, pushed or deployed as the task calls for, and the full test suite passed. Work that is done but left its push, deploy or full test gate open reports `agent-done` and names that step. A run that could not use the method its brief named reports `incomplete`.";

const INVESTIGATE_STATUS_CONTRACT: &str = "When finished, end with a final reply whose first line is `ENVOY_STATUS: complete` or `ENVOY_STATUS: incomplete`. `complete` means the answer is reported with its evidence and nothing changed beyond what the direction from the user asked for. `incomplete` means the question could not be answered, and the reply names what blocked it.";

/// How every brief's final reply reads after its status line. Each session finishes its
/// allowed work before reporting. The first line gives the outcome for the card title; the
/// last names only the change the user must approve. Without this shape, sessions list checks.
const REPORT_STEP: &str = " Before you report, finish every step your instructions allow, such as the checks, commit, push or deploy the task calls for, and never hand back a step they allow. Then report in caveman style, four short lines at most: the answer or outcome first, then the evidence that settles it, then what changed or `Changed nothing`, and last a line that starts `Recommended action:` and names the one change your instructions did not allow and the user must approve, complete enough to plan from as written. Work waiting on a later event, steps only the user can take, advice for future work, standing rules, and steps already finished are no step; write `Recommended action: none` and name any step only the user can take in the evidence line. Leave out how you checked.\n";

const INVESTIGATE_INSTRUCTION: &str = "Investigate what this card reports and answer it. Test its premise first. Confirm each claim against the transcripts, live systems, files and logs it names, and say plainly where the evidence contradicts the card. The proposed action is the producer's suggestion and the user has not approved it, so do not carry it out. Change nothing unless the direction from the user asks for a change, and then make only that change.";
const ACT_INSTRUCTION: &str = "Take the action this card calls for, end to end. Investigate first when the right change is not obvious, then carry it out in the live systems, files and logs it names: make the fix, run the checks, and ship it through the project's normal pipeline. The direction above narrows or overrides this. Report what you changed and the evidence it works.";
const PLAN_INSTRUCTION: &str = "Start in plan mode. Explore read-only, draft a decision-complete plan, and propose it. The plan arrives in the triage inbox for approval. After approval, carry out the work end to end.";
const PLAN_INTO_INSTRUCTION: &str = "Start in plan mode. Explore read-only, draft a decision-complete plan, and propose it. The harness approves the plan on its own and hands it to the execution model, which carries out the work end to end.";
const IMPLEMENT_INSTRUCTION: &str = "The plan in this card is approved, so carry it out end to end. Confirm that its steps still hold, then make the changes it lists, run its checks, and ship them through the project's normal pipeline. Do not write a new plan, propose one for review, or ask for approval again: the design is settled, so move forward. The direction above narrows or overrides this. Report what you changed and the evidence it works.";
/// Plan approval hands the work to a model that may see only the plan. omp's default
/// "Approve and execute" starts a fresh session, and Plan into switches to the execution
/// model, so the plan has to carry the status contract.
const PLAN_REPORT_STEP: &str = "Approval hands the plan to a model that may see only the plan. End the plan with a section headed \"Final report\" that repeats the previous paragraph word for word.";

/// The directory a dsh workspace session must work from. A workspace session starts in the
/// workspace directory, so its brief names the project directory and the tool that moves it.
fn workspace_workdir(launch: &Launch, cwd: &str) -> Option<String> {
    (launch.engine == Engine::Dsh && launch.dsh_workspace.is_some() && !cwd.is_empty())
        .then(|| cwd.to_owned())
}

fn card_workdir(card: &Card) -> String {
    if card.source.cwd.is_empty() {
        std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())
    } else {
        card.source.cwd.clone()
    }
}

fn workdir_note(cwd: &str) -> String {
    format!(
        "Work from `{cwd}`. This session starts in its dsh workspace directory, so call the `working_directory` tool with `cd` set to that path before you start.\n\n"
    )
}

fn workdir_section(cwd: &str) -> String {
    format!(
        "## Working directory\n`{cwd}`\n\nThis session starts in its dsh workspace directory. Call the `working_directory` tool with `cd` set to that path before you start.\n\n"
    )
}

fn session_brief(prompt: &str, plan: bool, workdir: Option<String>) -> String {
    let mut brief = String::new();
    if let Some(cwd) = workdir.as_deref() {
        brief.push_str(&workdir_note(cwd));
    }
    brief.push_str(&format!(
        "{}\n\n{ENVOY_STATUS_CONTRACT}{REPORT_STEP}",
        prompt.trim()
    ));
    if plan {
        brief.push_str(&format!("\n{PLAN_REPORT_STEP}\n"));
    }
    brief
}

fn dispatch_brief(card: &Card, mode: DispatchMode, note: Option<&str>, workdir: Option<String>) -> String {
    let mut brief = format!(
        "# {}: {}\n\n## Card\n- Kind: {}\n- Source: {} ({})\n- Priority: {}\n\n## Situation\n{}\n\n## Task\n{}\n\n## {}\n{}\n\n## Result\n{}\n\n## Body\n{}\n\n## Direction from the user\n{}\n\n{}\n## References\n",
        mode.swipe_text(),
        card.title,
        card.card_event_type(),
        card.source.label,
        card.source.uri,
        card.priority,
        card.star.situation,
        card.star.task,
        action_heading(mode),
        card.star.action,
        card.star.result,
        card.body.as_deref().unwrap_or_default(),
        note.unwrap_or_default(),
        workdir.as_deref().map(workdir_section).unwrap_or_default(),
    );
    push_refs(&mut brief, card);
    push_instructions(&mut brief, mode);
    brief.push_str(REPORT_STEP);
    if matches!(mode, DispatchMode::Plan | DispatchMode::PlanInto) {
        brief.push_str(&format!("\n{PLAN_REPORT_STEP}\n"));
    }
    brief
}

fn write_brief(db: &Db, brief: String) -> Result<PathBuf, String> {
    let directory = db.path().with_file_name("briefs");
    if let Some(parent) = directory.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
            if !metadata.file_type().is_dir() {
                return Err(error.to_string());
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let path = directory.join(format!("triage-brief-{}.md", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
    file.write_all(brief.as_bytes())
        .map_err(|error| error.to_string())?;
    fs::canonicalize(path).map_err(|error| error.to_string())
}

fn write_plan_overlay(brief: &Path) -> Result<PathBuf, String> {
    let directory = brief
        .parent()
        .ok_or_else(|| "brief has no parent directory".to_owned())?;
    let path = directory.join(format!("plan-mode-{}.yml", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
    file.write_all(b"plan:\n  defaultOnStartup: true\n")
        .map_err(|error| error.to_string())?;
    fs::canonicalize(path).map_err(|error| error.to_string())
}

/// Thinking levels `omp --thinking` accepts.
pub const THINKING_LEVELS: &[&str] = &["auto", "off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// tmux session new windows open in when neither the request nor `serve --session` names one.
pub const DEFAULT_SESSION: &str = "triage";

/// The harness a launched session runs in, either omp in a tmux window or dsh through its web host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Engine {
    #[default]
    Omp,
    Dsh,
}

impl Engine {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "omp" => Some(Self::Omp),
            "dsh" => Some(Self::Dsh),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Omp => "omp",
            Self::Dsh => "dsh",
        }
    }
}

/// Where and how triage launches sessions, from `triage serve` flags. Unset omp fields fall
/// back to `DEFAULT_SESSION` and to omp's own configured model and thinking level. `dsh_host`
/// is the host file the dsh bridge is read from. `dsh_workspace` names the dsh workspace every
/// new dsh session joins, so the dsh web UI shows triage's sessions in one group, and
/// `dsh_workspaces` is the directory that holds one directory per workspace name.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    pub session: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub engine: Engine,
    pub dsh_host: Option<PathBuf>,
    pub dsh_workspace: Option<String>,
    pub dsh_workspaces: Option<PathBuf>,
}

/// Trim an omp model selector and refuse shell characters. A blank selector means unset.
pub fn model_selector(model: Option<&str>) -> Result<Option<&str>, String> {
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    if let Some(model) = model {
        if !model.chars().all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c)) {
            return Err(format!("invalid model {model}"));
        }
    }
    Ok(model)
}

/// Validate the launch choices and render the `--model`, `--plan` and `--thinking` flags.
/// omp moves a session onto its `plan` role model whenever plan mode turns on, so the chosen
/// model fills that role too and a plan session stays on it. An unset model or thinking level
/// leaves the choice to omp's configuration.
pub fn launch_flags(model: Option<&str>, thinking: Option<&str>) -> Result<String, String> {
    let model = model_selector(model)?;
    let thinking = thinking_level(thinking)?;
    Ok(format!(
        "{}{}",
        model.map(|model| format!("--model={model} --plan={model} ")).unwrap_or_default(),
        thinking
            .map(|thinking| format!("--thinking={thinking} "))
            .unwrap_or_default(),
    ))
}

/// Trim a thinking level and check it against `THINKING_LEVELS`. A blank level means unset.
fn thinking_level(thinking: Option<&str>) -> Result<Option<&str>, String> {
    let thinking = thinking.map(str::trim).filter(|t| !t.is_empty());
    if let Some(thinking) = thinking {
        if !THINKING_LEVELS.contains(&thinking) {
            return Err(format!("unknown thinking level {thinking}"));
        }
    }
    Ok(thinking)
}

/// Trim a dsh model selector. A blank selector means unset. A selector is one
/// token, "provider/model" or a bare model id. Whether the host offers it is the
/// bridge's check, because dsh's settings decide the catalog and a copy here
/// would go stale.
pub fn dsh_selector(model: Option<&str>) -> Result<Option<&str>, String> {
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return Ok(None);
    };
    // A dsh selector is "provider/model" or a bare model id. An omp role (@smol)
    // and anything with whitespace or control characters can never name one.
    if model.starts_with('@')
        || model.len() > 200
        || model.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("unknown dsh model {model}"));
    }
    Ok(Some(model))
}

/// Trim and check `model` for `engine`, as an omp selector or a dsh model.
pub fn engine_selector(engine: Engine, model: Option<&str>) -> Result<Option<&str>, String> {
    match engine {
        Engine::Omp => model_selector(model),
        Engine::Dsh => dsh_selector(model),
    }
}

/// Trim the execution model's effort. dsh takes one of its effort levels, which it applies to
/// the executor model at approval. omp takes one of its thinking levels, which the launch
/// appends to the execution model selector.
pub fn executor_effort(engine: Engine, effort: Option<&str>) -> Result<Option<&str>, String> {
    let Some(effort) = effort.map(str::trim).filter(|effort| !effort.is_empty()) else {
        return Ok(None);
    };
    match engine {
        // dsh reports each model's effort ids, which are the shared level names.
        Engine::Dsh if THINKING_LEVELS.contains(&effort) => Ok(Some(effort)),
        Engine::Dsh => Err(format!("unknown execution effort {effort}")),
        Engine::Omp if THINKING_LEVELS.contains(&effort) => Ok(Some(effort)),
        Engine::Omp => Err(format!("unknown execution thinking level {effort}")),
    }
}

/// Check a plan-into launch's execution model and effort. dsh requires a named executor for effort. omp uses its default smol role.
pub(crate) fn plan_into_choices(
    engine: Engine,
    executor: Option<&str>,
    effort: Option<&str>,
) -> Result<(), String> {
    engine_selector(engine, executor)?;
    let effort = executor_effort(engine, effort)?;
    if engine == Engine::Dsh && effort.is_some() && executor.is_none() {
        return Err("execution effort needs an execution model".to_owned());
    }
    Ok(())
}

/// omp's execution model selector with `effort` as its thinking level. A selector that names
/// its own level keeps it, because omp takes the last `:<level>` suffix.
fn omp_execution_model(executor: Option<&str>, effort: Option<&str>) -> Result<Option<String>, String> {
    let executor = match (model_selector(executor)?, effort) {
        (None, None) => return Ok(None),
        (None, Some(_)) => "@smol",
        (Some(executor), _) => executor,
    };
    let named = executor
        .rsplit_once(':')
        .is_some_and(|(_, level)| THINKING_LEVELS.contains(&level));
    let selector = match effort {
        Some(effort) if !named => format!("{executor}:{effort}"),
        _ => executor.to_owned(),
    };
    model_selector(Some(&selector))?;
    Ok(Some(selector))
}

/// Check the launch model and thinking level for `engine` without rendering omp flags.
pub fn engine_flags(engine: Engine, model: Option<&str>, thinking: Option<&str>) -> Result<(), String> {
    match engine {
        Engine::Omp => launch_flags(model, thinking).map(|_| ()),
        Engine::Dsh => {
            dsh_selector(model)?;
            thinking_level(thinking).map(|_| ())
        }
    }
}

/// Build the omp launch line for a brief. Goal mode starts omp with an objective made of the
/// brief's first line and the brief's path, so the session keeps working toward the brief
/// between turns until it reports done, and the line stays far below the 16 KiB tmux takes
/// in one command whatever the brief's size. omp records the objective as the session's
/// first user message, and `omp::dispatch_audit` recognises a triage dispatch by the path.
/// Plan mode feeds the brief file as the positional prompt behind a per-run config overlay
/// that turns read-only plan mode on. Plan into feeds the brief the same way behind
/// `--plan-yolo`, so omp approves the first plan on its own and switches the session to the
/// execution model.
fn omp_command(
    brief: &Path,
    model: Option<&str>,
    thinking: Option<&str>,
    start: Start,
    image: Option<&Upload>,
) -> Result<String, String> {
    let flags = launch_flags(model, thinking)?;
    // A goal's objective is text, so the image rides it as an `@` mention; plan messages
    // carry it as one more `@` file beside the brief.
    let mention = image
        .map(|image| format!(" Attached image: @{}.", image.path.display()))
        .unwrap_or_default();
    let file = image
        .map(|image| format!(" @{}", shell_quote(&image.path.to_string_lossy())))
        .unwrap_or_default();
    match start {
        Start::Goal => {
            let text = fs::read_to_string(brief)
                .map_err(|error| format!("could not read brief: {error}"))?;
            let heading = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
            let objective = format!(
                "{}\n\nCarry out the triage brief saved at {}. Read the whole file before you start and again before you mark the goal complete; it holds the full task and the report format.{mention}",
                truncate_chars(heading.to_owned(), 120),
                brief.display()
            );
            Ok(format!("omp {flags}--goal {}", shell_quote(&objective)))
        }
        Start::Plan => {
            let overlay = write_plan_overlay(brief)?;
            let heading = brief_heading(brief, 120)?;
            let title_arg = format!("{} ", shell_quote(&heading));
            Ok(format!(
                "omp --config {} {flags}{title_arg}@{}{file}",
                shell_quote(&overlay.to_string_lossy()),
                shell_quote(&brief.to_string_lossy())
            ))
        }
        // An implement dispatch runs like a goal, at full permission, with a brief that
        // carries out the plan instead of a prompt that asks for one.
        Start::Implement => {
            let text = fs::read_to_string(brief)
                .map_err(|error| format!("could not read brief: {error}"))?;
            let heading = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
            let objective = format!(
                "{}\n\nCarry out the triage brief saved at {}. Read the whole file before you start and again before you mark the goal complete; the plan in it is approved and settled, so implement it rather than planning again.{mention}",
                truncate_chars(heading.to_owned(), 120),
                brief.display()
            );
            Ok(format!("omp {flags}--goal {}", shell_quote(&objective)))
        }
        Start::PlanInto(executor, effort) => {
            let into = omp_execution_model(executor, executor_effort(Engine::Omp, effort)?)?
                .map(|executor| format!("--plan-yolo-into={executor} "))
                .unwrap_or_default();
            let heading = brief_heading(brief, 120)?;
            let title_arg = format!("{} ", shell_quote(&heading));
            Ok(format!(
                "omp {flags}--plan-yolo {into}{title_arg}@{}{file}",
                shell_quote(&brief.to_string_lossy())
            ))
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests;
