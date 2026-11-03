//! SQLite store. One table `events`, append-only. Cards and swipes are events too.
//! Owner: db worker. Keep signatures stable; other modules call these.

use crate::events::{Card, CloudEvent, Swipe};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use rusqlite::{OptionalExtension, params, types::Type};
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;

fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn from_json<T: DeserializeOwned>(value: String) -> Result<T> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
    })
}

#[cfg(all(test, unix))]
std::thread_local! {
    static SQLITE_FILE_MISSING_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(all(test, unix))]
fn run_sqlite_file_missing_hook() {
    SQLITE_FILE_MISSING_HOOK.with(|hook| {
        let hook = hook.borrow_mut().take();
        if let Some(hook) = hook {
            hook();
        }
    });
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let open_existing = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    };
    let file = match open_existing() {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(test)]
            run_sqlite_file_missing_hook();
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_existing()?
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(unix)]
fn restrict_sqlite_file_permissions(path: &Path) -> std::io::Result<()> {
    if path == Path::new(":memory:") || path.as_os_str().is_empty() {
        return Ok(());
    }

    let path_os = path.as_os_str();
    let mut file_path = std::ffi::OsString::with_capacity(path_os.len().saturating_add(4));
    for suffix in ["", "-wal", "-shm"] {
        file_path.clear();
        file_path.push(path_os);
        file_path.push(suffix);
        restrict_file_permissions(Path::new(&file_path))?;
    }
    Ok(())
}
fn latest_swipe(conn: &Connection, card_id: &str) -> Result<Option<Swipe>> {
    let data = conn
        .query_row(
            "SELECT data FROM events WHERE type = ?1 AND card_id = ?2 ORDER BY rowid DESC LIMIT 1",
            params![crate::events::T_SWIPE, card_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    data.map(from_json).transpose()
}

/// One card row by its card id, if stored.
fn card_on(conn: &Connection, id: &str) -> Result<Option<Card>> {
    conn.query_row(
        "SELECT data FROM events WHERE id = ?1",
        [format!("card:{id}")],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(from_json)
    .transpose()
}

/// Every card whose first ref is this transcript. Pane ids never match by pattern:
/// only exact `json_extract` equality, because a pane id such as `%50` is a prefix
/// of `%650` and a `LIKE` pattern would join both eras.
fn cards_by_refs(conn: &Connection, reference: &str) -> Result<Vec<Card>> {
    query_cards(
        conn,
        "SELECT data FROM events
         WHERE id LIKE 'card:%' AND json_extract(data, '$.refs[0]') = ?1",
        [reference],
    )
}

/// Cards of one pane created inside `[start, end]`. Card rows store `ts_ms = created_ms`.
fn cards_by_pane_window(conn: &Connection, pane: &str, start: i64, end: i64) -> Result<Vec<Card>> {
    query_cards(
        conn,
        "SELECT data FROM events
         WHERE id LIKE 'card:%'
           AND json_extract(data, '$.source.pane') = ?1 AND ts_ms BETWEEN ?2 AND ?3",
        params![pane, start, end],
    )
}

/// The newest cards of one producer source uri.
fn cards_by_source(conn: &Connection, uri: &str) -> Result<Vec<Card>> {
    query_cards(
        conn,
        "SELECT data FROM events
         WHERE id LIKE 'card:%' AND json_extract(data, '$.source.uri') = ?1
         ORDER BY ts_ms DESC LIMIT 50",
        params![uri],
    )
}

/// Every swipe of one card in arrival order, which is `at_ms` order.
fn swipes_of(conn: &Connection, card_id: &str) -> Result<Vec<Swipe>> {
    conn.prepare("SELECT data FROM events WHERE type = ?1 AND card_id = ?2 ORDER BY rowid")?
        .query_map(
            params![crate::events::T_SWIPE, card_id],
            |row| row.get::<_, String>(0),
        )?
        .map(|data| from_json(data?))
        .collect()
}

/// Card rows for a query with bind values; rows without a card body are skipped.
fn query_cards<P: rusqlite::Params>(conn: &Connection, sql: &str, params: P) -> Result<Vec<Card>> {
    conn.prepare(sql)?
        .query_map(params, |row| row.get::<_, String>(0))?
        .map(|data| from_json(data?))
        .collect()
}
fn push_subscription_row(row: &rusqlite::Row<'_>) -> Result<crate::push::Subscription> {
    Ok(crate::push::Subscription {
        endpoint: row.get(0)?,
        p256dh: row.get(1)?,
        auth: row.get(2)?,
        max_priority: row.get(3)?,
    })
}
pub(crate) fn cloud_event_key(source: &str, id: &str) -> String {
    format!("event:{}:{}:{}", source.len(), source, id)
}

pub struct Db {
    pub conn: Mutex<Connection>,
    path: PathBuf,
    answer_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

pub type Result<T, E = rusqlite::Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub enum InsertEventError {
    EmptyId,
    Database(rusqlite::Error),
}

impl From<rusqlite::Error> for InsertEventError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// How far around a pane's mentions a card still belongs to the same pane era.
/// Pane ids restart with each tmux server, so the window keeps an old era out.
// ponytail: era guard; widen to 7d if a same-era card ever goes missing
pub const PANE_WINDOW_MS: i64 = 48 * 60 * 60 * 1000;

/// One card of a context thread with every swipe it collected, oldest first.
pub struct ThreadItem {
    pub card: Card,
    pub swipes: Vec<Swipe>,
}

/// A card's whole story, oldest first, capped at the newest 100 cards.
pub struct Thread {
    pub items: Vec<ThreadItem>,
    pub truncated: usize,
}

/// The swipe reasons that name a pane, as hub-written `<verb> <pane>`.
const PANE_SWIPE_VERBS: [&str; 6] = ["launched", "launching", "closed", "investigating", "planning", "acting"];

/// The pane a swipe reason names, `launched %50` -> `%50`.
fn pane_of_reason(reason: &str) -> Option<&str> {
    let (verb, pane) = reason.split_once(' ')?;
    PANE_SWIPE_VERBS.contains(&verb).then_some(pane).filter(|pane| !pane.is_empty())
}

impl Db {
    /// Open or create the database, WAL mode, busy timeout, and schema.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        // Set file modes only before SQLite opens the files. Closing any other descriptor
        // for them afterwards would drop SQLite's POSIX locks (sqlite.org/howtocorrupt.html 2.2).
        #[cfg(unix)]
        restrict_sqlite_file_permissions(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;

        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                ts_ms INTEGER NOT NULL,
                source TEXT NOT NULL,
                type TEXT NOT NULL,
                card_id TEXT,
                priority INTEGER,
                data TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS events_card_id_idx ON events(card_id);
            CREATE INDEX IF NOT EXISTS events_type_ts_ms_idx ON events(type, ts_ms);
            CREATE TABLE IF NOT EXISTS push_subscriptions (
                endpoint TEXT PRIMARY KEY,
                p256dh TEXT NOT NULL,
                auth TEXT NOT NULL,
                max_priority INTEGER NOT NULL,
                created_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS push_vapid (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                pkcs8 BLOB NOT NULL
            );",
        )?;
        Ok(Db {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
            answer_locks: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Store an external CloudEvent and, when `card` is given, the derived card
    /// in the same transaction. Return false without writing when the
    /// `(source, id)` pair already exists.
    pub fn insert_event(
        &self,
        ev: &CloudEvent,
        card: Option<&Card>,
    ) -> Result<bool, InsertEventError> {
        if ev.id.is_empty() {
            return Err(InsertEventError::EmptyId);
        }
        let event_data = to_json(ev)?;
        let card_data = card.map(to_json).transpose()?;
        let event_key = cloud_event_key(&ev.source, &ev.id);
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO events(id, ts_ms, source, type, card_id, priority, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event_key,
                crate::events::now_ms(),
                ev.source,
                ev.r#type,
                card.map(|card| card.id.as_str()),
                ev.priority.map(i64::from),
                event_data,
            ],
        )?;
        if inserted == 0 {
            return Ok(false);
        }
        if let (Some(card), Some(data)) = (card, card_data) {
            tx.execute(
                "INSERT INTO events(id, ts_ms, source, type, card_id, priority, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    format!("card:{}", card.id),
                    card.created_ms,
                    card.source.uri,
                    card.card_event_type(),
                    card.id,
                    i64::from(card.priority),
                    data,
                ],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Store a hub-generated card (pane ask, pane done). Idempotent on `card.id`.
    /// Returns false when the card already exists.
    pub fn insert_card(&self, card: &Card) -> Result<bool> {
        let data = to_json(card)?;
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "INSERT OR IGNORE INTO events(id, ts_ms, source, type, card_id, priority, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                format!("card:{}", card.id),
                card.created_ms,
                card.source.uri,
                card.card_event_type(),
                card.id,
                i64::from(card.priority),
                data,
            ],
        )? != 0)
    }

    /// Record a swipe. Always appends.
    pub fn swipe(&self, s: &Swipe) -> Result<()> {
        let data = to_json(s)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events(id, ts_ms, source, type, card_id, priority, data)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![
                format!("swipe:{}", uuid::Uuid::new_v4()),
                s.at_ms,
                "triage",
                crate::events::T_SWIPE,
                s.card,
                data,
            ],
        )?;
        Ok(())
    }

    fn cards_at(
        &self,
        now_ms: i64,
        include_deferred: bool,
        include_deferred_pane_cards: bool,
    ) -> Result<Vec<Card>> {
        // Handled cards stay in SQLite; only open and deferred rows are read and parsed.
        let conn = self.conn.lock().unwrap();
        let mut cards = Vec::new();
        {
            let mut stmt = conn.prepare_cached(
                "SELECT cards.data, swipes.data
                 FROM events AS cards
                 LEFT JOIN (
                     SELECT card_id, MAX(rowid) AS latest_rowid
                     FROM events
                     WHERE type = ?1
                     GROUP BY card_id
                 ) AS latest ON latest.card_id = cards.card_id
                 LEFT JOIN events AS swipes ON swipes.rowid = latest.latest_rowid
                 WHERE cards.id LIKE 'card:%' AND cards.card_id IS NOT NULL
                   AND cards.type IN (?2, ?3, ?4, ?5)
                   AND (swipes.data IS NULL OR json_extract(swipes.data, '$.action') = 'later')
                 ORDER BY cards.rowid",
            )?;
            let rows = stmt.query_map(
                params![
                    crate::events::T_SWIPE,
                    crate::events::T_CARD_ASK,
                    crate::events::T_CARD_FINDING,
                    crate::events::T_CARD_INFO,
                    crate::events::T_CARD_PLAN,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )?;
            for row in rows {
                let (data, swipe_data) = row?;
                let card: Card = from_json(data)?;
                let swipe: Option<Swipe> = swipe_data.map(from_json).transpose()?;
                if swipe.as_ref().is_none_or(|swipe| {
                    swipe.action == "later"
                        && (include_deferred
                            || (include_deferred_pane_cards && matches!(card.kind, crate::events::Kind::Ask | crate::events::Kind::Plan))
                            || swipe.until_ms.is_some_and(|until| until <= now_ms))
                }) {
                    cards.push(card);
                }
            }
        }
        cards.sort_by_key(|card| (card.priority, card.created_ms));
        Ok(cards)
    }

    /// Cards with no swipe, or whose last swipe is `later` with `until_ms <= now_ms`.
    /// Ordered by priority ascending, then created_ms ascending.
    pub fn queue(&self, now_ms: i64) -> Result<Vec<Card>> {
        self.cards_at(now_ms, false, false)
    }

    /// A card and its latest swipe, if any.
    pub fn card(&self, id: &str) -> Result<Option<(Card, Option<Swipe>)>> {
        let conn = self.conn.lock().unwrap();
        let data = conn
            .query_row(
                "SELECT data FROM events WHERE id = ?1",
                [format!("card:{id}")],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(data) = data else { return Ok(None) };
        let card = from_json(data)?;
        let swipe = latest_swipe(&conn, id)?;
        Ok(Some((card, swipe)))
    }

    /// Every card whose id is `<prefix>:<pane>:<entry>`, oldest first, each with
    /// its latest swipe. Used to tell one reply's earlier cards from a new pane's.
    pub fn reply_cards(&self, prefix: &str, entry: &str) -> Result<Vec<(Card, Option<Swipe>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT card_id, data FROM events
             WHERE id GLOB ?1 AND substr(card_id, -(length(?2) + 1)) = ':' || ?2
             ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map(
                params![
                    format!("card:{prefix}:*"),
                    entry,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut cards = Vec::with_capacity(rows.len());
        for (id, data) in rows {
            cards.push((from_json(data)?, latest_swipe(&conn, &id)?));
        }
        Ok(cards)
    }

    /// The story around one card: the card itself, its transcript siblings, the cards
    /// of the panes it lived in (with the cards that launched those panes and every
    /// steering swipe they took), and for an ingested card the newest cards of the
    /// same producer. `None` when `id` names no stored card.
    pub fn thread(&self, id: &str) -> Result<Option<Thread>> {
        let conn = self.conn.lock().unwrap();
        let Some(anchor) = card_on(&conn, id)? else { return Ok(None) };
        let mut cards: HashMap<String, Card> = HashMap::from([(anchor.id.clone(), anchor.clone())]);
        // The copy's origin: a reload id is `reload:<original id>:<uuid>`.
        if let Some((original, _)) = anchor.id.strip_prefix("reload:").and_then(|rest| rest.rsplit_once(':')) {
            if let Some(card) = card_on(&conn, original)? {
                cards.entry(card.id.clone()).or_insert(card);
            }
        }
        // Transcript family first, then the pane families, to a fixed point: a report's
        // launcher joins through its pane, and that launcher's own swipes can name more panes.
        for _ in 0..3 {
            let mut changed = false;
            let report_refs: Vec<String> = cards
                .values()
                .filter(|card| card.id.starts_with("done:") || card.id.starts_with("exited:"))
                .filter_map(|card| card.refs.first().filter(|r| !r.is_empty()).cloned())
                .collect();
            for reference in report_refs {
                for card in cards_by_refs(&conn, &reference)? {
                    changed |= cards.insert(card.id.clone(), card).is_none();
                }
            }
            // Every swipe that names a pane, once per pass: those on included cards
            // widen the pane windows, and in-window ones pull their launcher cards in.
            let naming: Vec<(String, String, i64)> = conn
                .prepare(
                    "SELECT card_id, json_extract(data, '$.reason'), json_extract(data, '$.at_ms')
                     FROM events
                     WHERE type = ?1 AND json_extract(data, '$.reason') IS NOT NULL",
                )?
                .query_map(params![crate::events::T_SWIPE], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter_map(|(card_id, reason, at_ms)| {
                    pane_of_reason(&reason).map(|pane| (card_id, pane.to_owned(), at_ms))
                })
                .collect();
            let mut pane_mentions: HashMap<String, Vec<i64>> = HashMap::new();
            for card in cards.values() {
                if let Some(pane) = card.source.pane.as_deref().filter(|pane| !pane.is_empty()) {
                    pane_mentions.entry(pane.to_owned()).or_default().push(card.created_ms);
                }
            }
            for (card_id, pane, at_ms) in &naming {
                if cards.contains_key(card_id) {
                    pane_mentions.entry(pane.clone()).or_default().push(*at_ms);
                }
            }
            for (pane, mentions) in &pane_mentions {
                let start = mentions.iter().min().copied().unwrap_or_default() - PANE_WINDOW_MS;
                let end = mentions.iter().max().copied().unwrap_or_default() + PANE_WINDOW_MS;
                for card in cards_by_pane_window(&conn, pane, start, end)? {
                    changed |= cards.insert(card.id.clone(), card).is_none();
                }
                for card_id in naming
                    .iter()
                    .filter(|(_, named, at)| named == pane && (start..=end).contains(at))
                    .map(|(card_id, _, _)| card_id)
                {
                    if let Some(card) = card_on(&conn, card_id)? {
                        changed |= cards.insert(card.id.clone(), card).is_none();
                    }
                }
            }
            if !changed {
                break;
            }
        }
        // Producer family: an ingested card carries its producer's alert history.
        if !anchor.source.uri.is_empty() && !anchor.source.uri.starts_with("tmux://pane/") {
            for card in cards_by_source(&conn, &anchor.source.uri)? {
                cards.entry(card.id.clone()).or_insert(card);
            }
        }
        let mut ordered: Vec<Card> = cards.into_values().collect();
        ordered.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.id.cmp(&b.id)));
        let truncated = ordered.len().saturating_sub(100);
        if truncated > 0 {
            ordered.drain(..truncated);
        }
        let items = ordered
            .into_iter()
            .map(|card| {
                let swipes = swipes_of(&conn, &card.id)?;
                Ok(ThreadItem { card, swipes })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Thread { items, truncated }))
    }

    pub(crate) fn answer_lock(&self, card_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.answer_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(card_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(card_id.to_owned(), Arc::downgrade(&lock));
        lock
    }

    fn history_on(conn: &Connection, limit: usize) -> Result<Vec<(Card, Swipe)>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let records = {
            let mut stmt = conn.prepare(
                "SELECT c.data, s.data
                 FROM events AS s
                 JOIN events AS c ON c.id = 'card:' || s.card_id
                 WHERE s.type = ?1
                   AND c.type IN (?2, ?3, ?4, ?5)
                   AND s.rowid = (
                       SELECT MAX(newer.rowid) FROM events AS newer
                       WHERE newer.type = ?1 AND newer.card_id = s.card_id
                   )
                 ORDER BY s.rowid DESC LIMIT ?6",
            )?;
            stmt.query_map(
                params![
                    crate::events::T_SWIPE,
                    crate::events::T_CARD_ASK,
                    crate::events::T_CARD_FINDING,
                    crate::events::T_CARD_INFO,
                    crate::events::T_CARD_PLAN,
                    limit,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        records
            .into_iter()
            .map(|(card, swipe)| Ok((from_json(card)?, from_json(swipe)?)))
            .collect()
    }

    fn history_count_between_on(
        conn: &Connection,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<i64> {
        conn.query_row(
            "SELECT COUNT(*)
             FROM events AS s
             JOIN events AS c ON c.id = 'card:' || s.card_id
             WHERE s.type = ?1
               AND c.type IN (?2, ?3, ?4, ?5)
               AND s.rowid = (
                   SELECT MAX(newer.rowid) FROM events AS newer
                   WHERE newer.type = ?1 AND newer.card_id = s.card_id
               )
               AND s.ts_ms >= ?6 AND s.ts_ms < ?7",
            params![
                crate::events::T_SWIPE,
                crate::events::T_CARD_ASK,
                crate::events::T_CARD_FINDING,
                crate::events::T_CARD_INFO,
                crate::events::T_CARD_PLAN,
                start_ms,
                end_ms,
            ],
            |row| row.get(0),
        )
    }

    pub(crate) fn history_with_count_between(
        &self,
        limit: usize,
        count_range: Option<(i64, i64)>,
    ) -> Result<(Vec<(Card, Swipe)>, Option<i64>)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let cards = Self::history_on(&tx, limit)?;
        let count = count_range
            .map(|(start, end)| Self::history_count_between_on(&tx, start, end))
            .transpose()?;
        tx.commit()?;
        Ok((cards, count))
    }

    pub(crate) fn unresolved_cards_for_pane(&self, pane: &str) -> Result<Vec<Card>> {
        Ok(self
            .cards_at(crate::events::now_ms(), true, false)?
            .into_iter()
            .filter(|card| card.source.pane.as_deref() == Some(pane))
            .collect())
    }

    /// Every card with no swipe, or whose last swipe is `later`, queued or deferred.
    pub(crate) fn unresolved_cards(&self) -> Result<Vec<Card>> {
        self.cards_at(crate::events::now_ms(), true, false)
    }

    /// Ask and plan cards of `pane` that a pane exit closes: answering an ask needs its
    /// live dialog and approving a plan needs its review overlay. Info and finding cards
    /// outlive their pane.
    pub(crate) fn cards_for_exited_pane(&self, pane: &str) -> Result<Vec<Card>> {
        Ok(self
            .cards_at(crate::events::now_ms(), false, true)?
            .into_iter()
            .filter(|card| card.source.pane.as_deref() == Some(pane))
            .filter(|card| matches!(card.kind, crate::events::Kind::Ask | crate::events::Kind::Plan))
            .collect())
    }

    /// True when a swipe with reason `closed <pane>` was recorded at or after `since_ms`:
    /// the user closed that pane from one of its cards. The time bound keeps a close of an
    /// earlier tmux server's pane with the same id from matching.
    pub(crate) fn pane_closed_since(&self, pane: &str, since_ms: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE type = ?1 AND ts_ms >= ?2
             AND json_extract(data, '$.reason') = ?3)",
            params![crate::events::T_SWIPE, since_ms, format!("closed {pane}")],
            |row| row.get(0),
        )?)
    }

    /// The card whose answer last sent a brief into `pane` at or after `since_ms`, with that
    /// swipe. A finding answer or a dispatch records `launching <pane>` or `launched <pane>`,
    /// and an ask dispatch records `investigating`, `planning` or `acting <pane>`. The time
    /// bound keeps a launch into an earlier tmux server's pane with the same id from matching.
    pub(crate) fn pane_launch_since(&self, pane: &str, since_ms: i64) -> Result<Option<(Card, Swipe)>> {
        let conn = self.conn.lock().unwrap();
        let [launched, launching, investigating, planning, acting] =
            ["launched", "launching", "investigating", "planning", "acting"].map(|verb| format!("{verb} {pane}"));
        let found = conn
            .query_row(
                "SELECT card_id, data FROM events WHERE type = ?1 AND ts_ms >= ?2
                 AND json_extract(data, '$.reason') IN (?3, ?4, ?5, ?6, ?7)
                 ORDER BY ts_ms DESC, rowid DESC LIMIT 1",
                params![crate::events::T_SWIPE, since_ms, launched, launching, investigating, planning, acting],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((card_id, swipe)) = found else {
            return Ok(None);
        };
        let card = conn
            .query_row(
                "SELECT data FROM events WHERE id = ?1",
                [format!("card:{card_id}")],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(card) = card else {
            return Ok(None);
        };
        Ok(Some((from_json(card)?, from_json(swipe)?)))
    }

    pub(crate) fn unresolved_panes(&self) -> Result<HashSet<String>> {
        Ok(self
            .cards_at(crate::events::now_ms(), true, false)?
            .into_iter()
            .filter(|card| matches!(card.kind, crate::events::Kind::Ask | crate::events::Kind::Plan))
            .filter_map(|card| card.source.pane)
            .collect())
    }

    /// Monotonic change counter: number of events stored. Used as `version`.
    pub fn version(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT count(*) FROM events", [], |row| row.get(0))
    }

    /// The daemon's VAPID key: the stored one, or `candidate` stored now when none exists.
    pub fn vapid_key(&self, candidate: &[u8]) -> Result<Vec<u8>> {
        let conn = self.conn.lock().unwrap();
        conn.execute("INSERT OR IGNORE INTO push_vapid (id, pkcs8) VALUES (1, ?1)", [candidate])?;
        conn.query_row("SELECT pkcs8 FROM push_vapid WHERE id = 1", [], |row| row.get(0))
    }

    /// Stores a browser's subscription; subscribing again with the same endpoint replaces its keys and level.
    pub fn upsert_push_subscription(&self, subscription: &crate::push::Subscription) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO push_subscriptions (endpoint, p256dh, auth, max_priority, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(endpoint) DO UPDATE SET
                 p256dh = excluded.p256dh, auth = excluded.auth, max_priority = excluded.max_priority",
            params![
                subscription.endpoint,
                subscription.p256dh,
                subscription.auth,
                subscription.max_priority,
                crate::events::now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn push_subscription(&self, endpoint: &str) -> Result<Option<crate::push::Subscription>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT endpoint, p256dh, auth, max_priority FROM push_subscriptions WHERE endpoint = ?1",
                [endpoint],
                push_subscription_row,
            )
            .optional()
    }

    pub fn push_subscriptions(&self) -> Result<Vec<crate::push::Subscription>> {
        let conn = self.conn.lock().unwrap();
        let mut statement =
            conn.prepare("SELECT endpoint, p256dh, auth, max_priority FROM push_subscriptions ORDER BY created_ms")?;
        let rows = statement.query_map([], push_subscription_row)?;
        rows.collect()
    }

    pub fn delete_push_subscription(&self, endpoint: &str) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM push_subscriptions WHERE endpoint = ?1", [endpoint])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
