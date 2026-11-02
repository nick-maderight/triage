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

fn legacy_cloud_event(data: &str) -> Option<CloudEvent> {
    let mut value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let object = value.as_object_mut()?;
    if object
        .remove("priority")
        .is_some_and(|priority| serde_json::from_value::<Option<u8>>(priority).is_err())
    {
        return None;
    }
    serde_json::from_value(value).ok()
}

fn rekey_card_data(
    data: String,
    old_id: &str,
    new_id: &str,
    source: &str,
    kind: &str,
) -> Result<Option<String>> {
    let mut value: serde_json::Value = from_json(data)?;
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    if object.get("id").and_then(serde_json::Value::as_str) != Some(old_id)
        || object.get("kind").and_then(serde_json::Value::as_str) != Some(kind)
        || object
            .get("source")
            .and_then(|source| source.get("uri"))
            .and_then(serde_json::Value::as_str)
            != Some(source)
    {
        return Ok(None);
    }
    object.insert("id".into(), serde_json::Value::String(new_id.to_owned()));
    Ok(Some(value.to_string()))
}

fn rekey_json_reference(
    data: String,
    field: &str,
    old_id: &str,
    new_id: &str,
) -> Result<Option<String>> {
    let mut value: serde_json::Value = from_json(data)?;
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    if object.get(field).and_then(serde_json::Value::as_str) != Some(old_id) {
        return Ok(None);
    }
    object.insert(field.into(), serde_json::Value::String(new_id.to_owned()));
    Ok(Some(value.to_string()))
}

fn migrate_legacy_cloud_event_cards(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TEMP TABLE legacy_card_migration (
            rowid INTEGER PRIMARY KEY,
            old_card_id TEXT NOT NULL UNIQUE,
            new_card_id TEXT NOT NULL,
            ts_ms INTEGER NOT NULL,
            source TEXT NOT NULL,
            type TEXT NOT NULL,
            priority INTEGER,
            data TEXT NOT NULL
        );
        CREATE TEMP TABLE legacy_swipe_migration (
            rowid INTEGER PRIMARY KEY,
            id TEXT NOT NULL,
            ts_ms INTEGER NOT NULL,
            source TEXT NOT NULL,
            type TEXT NOT NULL,
            card_id TEXT NOT NULL,
            priority INTEGER,
            data TEXT NOT NULL
        );",
    )?;
    {
        let mut stmt = tx.prepare(
            "SELECT cards.rowid, cards.ts_ms, cards.source, cards.type,
                    cards.priority, cards.data, links.old_card_id,
                    links.new_card_id, links.card_type, links.source
             FROM events AS cards
             JOIN legacy_cloud_event_card_candidates AS links
               ON cards.id = 'card:' || links.old_card_id
              AND cards.card_id = links.old_card_id
              AND cards.type = links.card_type
             ORDER BY cards.rowid",
        )?;
        let mut rows = stmt.query([])?;
        let mut insert = tx.prepare(
            "INSERT INTO legacy_card_migration
             (rowid, old_card_id, new_card_id, ts_ms, source, type, priority, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        while let Some(row) = rows.next()? {
            let rowid = row.get::<_, i64>(0)?;
            let ts_ms = row.get::<_, i64>(1)?;
            let source = row.get::<_, String>(2)?;
            let card_type = row.get::<_, String>(3)?;
            let priority = row.get::<_, Option<i64>>(4)?;
            let data = row.get::<_, String>(5)?;
            let old_card_id = row.get::<_, String>(6)?;
            let new_card_id = row.get::<_, String>(7)?;
            let event_source = row.get::<_, String>(9)?;
            if source != event_source {
                continue;
            }
            let kind = match card_type.as_str() {
                crate::events::T_CARD_FINDING => "finding",
                crate::events::T_CARD_INFO => "info",
                _ => continue,
            };
            let Some(data) = rekey_card_data(
                data,
                &old_card_id,
                &new_card_id,
                &event_source,
                kind,
            )? else {
                continue;
            };
            insert.execute(params![
                rowid,
                old_card_id,
                new_card_id,
                ts_ms,
                source,
                card_type,
                priority,
                data,
            ])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "SELECT swipes.rowid, swipes.id, swipes.ts_ms, swipes.source,
                    swipes.type, swipes.priority, swipes.data,
                    cards.old_card_id, cards.new_card_id
             FROM events AS swipes
             JOIN legacy_card_migration AS cards
               ON swipes.card_id = cards.old_card_id
             WHERE swipes.type = ?1
             ORDER BY swipes.rowid",
        )?;
        let mut rows = stmt.query(params![crate::events::T_SWIPE])?;
        let mut insert = tx.prepare(
            "INSERT INTO legacy_swipe_migration
             (rowid, id, ts_ms, source, type, card_id, priority, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        while let Some(row) = rows.next()? {
            let rowid = row.get::<_, i64>(0)?;
            let id = row.get::<_, String>(1)?;
            let ts_ms = row.get::<_, i64>(2)?;
            let source = row.get::<_, String>(3)?;
            let event_type = row.get::<_, String>(4)?;
            let priority = row.get::<_, Option<i64>>(5)?;
            let data = row.get::<_, String>(6)?;
            let old_card_id = row.get::<_, String>(7)?;
            let new_card_id = row.get::<_, String>(8)?;
            let Some(data) = rekey_json_reference(data, "card", &old_card_id, &new_card_id)? else {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "legacy swipe card does not match its card_id",
                    )),
                ));
            };
            insert.execute(params![
                rowid,
                id,
                ts_ms,
                source,
                event_type,
                new_card_id,
                priority,
                data,
            ])?;
        }
    }
    tx.execute(
        "UPDATE events
         SET card_id = (
             SELECT cards.new_card_id
             FROM legacy_cloud_event_card_candidates AS links
             JOIN legacy_card_migration AS cards
               ON cards.old_card_id = links.old_card_id
             WHERE links.event_rowid = events.rowid
         )
         WHERE rowid IN (
             SELECT links.event_rowid
             FROM legacy_cloud_event_card_candidates AS links
             JOIN legacy_card_migration AS cards
               ON cards.old_card_id = links.old_card_id
         )",
        [],
    )?;
    tx.execute(
        "DELETE FROM events WHERE rowid IN (SELECT rowid FROM legacy_card_migration)",
        [],
    )?;
    tx.execute(
        "DELETE FROM events WHERE rowid IN (SELECT rowid FROM legacy_swipe_migration)",
        [],
    )?;
    tx.execute(
        "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
         SELECT rowid, 'card:' || new_card_id, ts_ms, source, type,
                new_card_id, priority, data
         FROM legacy_card_migration ORDER BY rowid",
        [],
    )?;
    tx.execute(
        "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
         SELECT rowid, id, ts_ms, source, type, card_id, priority, data
         FROM legacy_swipe_migration ORDER BY rowid",
        [],
    )?;
    tx.execute_batch(
        "DROP TABLE legacy_swipe_migration;
         DROP TABLE legacy_card_migration;",
    )?;
    Ok(())
}

fn migrate_cloud_event_keys(conn: &mut Connection) -> Result<()> {
    if conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))? >= 2 {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version = tx.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    if version >= 2 {
        tx.commit()?;
        return Ok(());
    }
    tx.execute_batch(
        "CREATE TEMP TABLE legacy_cloud_event_migration (
            rowid INTEGER PRIMARY KEY,
            new_id TEXT NOT NULL,
            ts_ms INTEGER NOT NULL,
            source TEXT NOT NULL,
            type TEXT NOT NULL,
            card_id TEXT,
            priority INTEGER,
            data TEXT NOT NULL
        );
        CREATE TEMP TABLE legacy_cloud_event_card_candidates (
            event_rowid INTEGER PRIMARY KEY,
            old_card_id TEXT NOT NULL UNIQUE,
            new_card_id TEXT NOT NULL,
            card_type TEXT NOT NULL,
            source TEXT NOT NULL
        );",
    )?;
    {
        let mut stmt = tx.prepare(
            "SELECT rowid, ts_ms, source, type, card_id, priority, data FROM events",
        )?;
        let mut rows = stmt.query([])?;
        let mut insert_events = tx.prepare(
            "INSERT INTO legacy_cloud_event_migration
             (rowid, new_id, ts_ms, source, type, card_id, priority, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut insert_cards = tx.prepare(
            "INSERT INTO legacy_cloud_event_card_candidates
             (event_rowid, old_card_id, new_card_id, card_type, source)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        while let Some(row) = rows.next()? {
            let rowid = row.get::<_, i64>(0)?;
            let ts_ms = row.get::<_, i64>(1)?;
            let source = row.get::<_, String>(2)?;
            let event_type = row.get::<_, String>(3)?;
            let card_id = row.get::<_, Option<String>>(4)?;
            let priority = row.get::<_, Option<i64>>(5)?;
            let data = row.get::<_, String>(6)?;
            let Some(event) = legacy_cloud_event(&data) else {
                continue;
            };
            let new_id = cloud_event_key(&event.source, &event.id);
            if version < 1 {
                insert_events.execute(params![
                    rowid,
                    new_id,
                    ts_ms,
                    source,
                    event_type,
                    card_id.as_deref(),
                    priority,
                    data,
                ])?;
            }
            let card_type = match (event.r#type.as_str(), event_type.as_str()) {
                (crate::events::T_FINDING, crate::events::T_FINDING) => {
                    Some(crate::events::T_CARD_FINDING)
                }
                (crate::events::T_INFO, crate::events::T_INFO) => {
                    Some(crate::events::T_CARD_INFO)
                }
                _ => None,
            };
            if let (Some(card_id), Some(card_type)) = (card_id.as_deref(), card_type) {
                if card_id != new_id {
                    insert_cards.execute(params![
                        rowid,
                        card_id,
                        new_id,
                        card_type,
                        event.source,
                    ])?;
                }
            }
        }
    }
    if version < 1 {
        tx.execute(
            "DELETE FROM events WHERE rowid IN (SELECT rowid FROM legacy_cloud_event_migration)",
            [],
        )?;
        {
            let mut stmt = tx.prepare(
                "SELECT rowid, new_id, ts_ms, source, type, card_id, priority, data
                 FROM legacy_cloud_event_migration ORDER BY rowid",
            )?;
            let mut rows = stmt.query([])?;
            let mut insert = tx.prepare(
                "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            while let Some(row) = rows.next()? {
                insert.execute(params![
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, String>(7)?,
                ])?;
            }
        }
    }
    migrate_legacy_cloud_event_cards(&tx)?;
    tx.execute_batch(
        "DROP TABLE legacy_cloud_event_card_candidates;
         DROP TABLE legacy_cloud_event_migration;",
    )?;
    tx.pragma_update(None, "user_version", 2)?;
    tx.commit()?;
    Ok(())
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
    /// Open or create the database, WAL mode, busy timeout, schema migrated.
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

        let mut conn = Connection::open_with_flags(
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
        migrate_cloud_event_keys(&mut conn)?;
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
mod tests {
    use super::*;
    use crate::events::{Kind, Source, Star};

    fn db() -> Db {
        let path = std::env::temp_dir().join(format!("triage-db-{}.sqlite", uuid::Uuid::new_v4()));
        Db::open(&path).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn open_restricts_existing_database_and_wal_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("triage-db-permissions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("triage.sqlite");
        let db = Db::open(&path).unwrap();
        db.insert_card(&card("permission-check", 1, 1, "%1")).unwrap();
        let file_paths = ["", "-wal", "-shm"].map(|suffix| {
            let mut file_path = path.as_os_str().to_os_string();
            file_path.push(suffix);
            PathBuf::from(file_path)
        });
        for file_path in &file_paths {
            assert!(file_path.is_file());
            assert_eq!(
                std::fs::metadata(file_path).unwrap().permissions().mode() & 0o777,
                0o600,
                "{}",
                file_path.display()
            );
            std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o666)).unwrap();
        }
        let reopened = Db::open(&path).unwrap();
        for file_path in &file_paths {
            assert_eq!(
                std::fs::metadata(file_path).unwrap().permissions().mode() & 0o777,
                0o600,
                "{}",
                file_path.display()
            );
        }
        drop(reopened);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn outside_connection_closing_keeps_the_open_database_wal() {
        use std::os::unix::fs::PermissionsExt;

        const OUTSIDE_DB: &str = "TRIAGE_OUTSIDE_CONNECTION_TEST_DB";
        const TEST_NAME: &str = "db::tests::outside_connection_closing_keeps_the_open_database_wal";

        if let Some(path) = std::env::var_os(OUTSIDE_DB) {
            let conn = Connection::open(PathBuf::from(path)).unwrap();
            conn.query_row("SELECT count(*) FROM events", [], |row| row.get::<_, i64>(0))
                .unwrap();
            return;
        }

        let directory = std::env::temp_dir().join(format!("triage-db-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("triage.sqlite");
        // A restarted daemon opens a database that is already in WAL mode, and only then does
        // SQLite hold its locks inside Db::open. Create the file first, then open it again.
        drop(Db::open(&path).unwrap());
        let db = Db::open(&path).unwrap();
        db.insert_card(&card("before-outside", 1, 1, "%1")).unwrap();

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(OUTSIDE_DB, &path)
            .output()
            .unwrap();
        let mut wal = path.as_os_str().to_os_string();
        wal.push("-wal");
        let wal_kept = Path::new(&wal).is_file();
        drop(db);
        std::fs::remove_dir_all(&directory).unwrap();

        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(wal_kept, "an outside connection deleted the WAL of the open database");
    }

    fn card(id: &str, priority: u8, created_ms: i64, pane: &str) -> Card {
        Card {
            id: id.into(),
            kind: Kind::Info,
            priority,
            created_ms,
            source: Source {
                uri: format!("pane:{pane}"),
                label: pane.into(),
                pane: Some(pane.into()),
                ..Source::default()
            },
            title: id.into(),
            star: Star::default(),
            options: Vec::new(),
            answerable: false,
            allow_other: false,
            questions: Vec::new(),
            body: None,
            refs: Vec::new(),
            level: None,
            tool_call_id: None,
        }
    }

    fn swipe(card: &str, action: &str, at_ms: i64, until_ms: Option<i64>) -> Swipe {
        Swipe {
            card: card.into(),
            action: action.into(),
            option: None,
            text: None,
            note: None,
            until_ms,
            reason: None,
            at_ms,
        }
    }

    /// A thread test card: any kind, with a source uri, an optional pane and refs.
    fn thread_card(id: &str, kind: Kind, created_ms: i64, uri: &str, pane: Option<&str>, refs: &[&str]) -> Card {
        Card {
            id: id.into(),
            kind,
            priority: 3,
            created_ms,
            source: Source {
                uri: uri.into(),
                label: uri.into(),
                pane: pane.map(|pane| pane.to_owned()),
                ..Source::default()
            },
            title: id.into(),
            star: Star::default(),
            options: Vec::new(),
            answerable: false,
            allow_other: false,
            questions: Vec::new(),
            body: Some(format!("Body of {id}")),
            refs: refs.iter().map(|r| r.to_string()).collect(),
            level: None,
            tool_call_id: None,
        }
    }

    fn reason_swipe(card: &str, action: &str, reason: &str, at_ms: i64) -> Swipe {
        Swipe {
            reason: Some(reason.into()),
            ..swipe(card, action, at_ms, None)
        }
    }

    #[test]
    fn thread_of_unknown_id_is_none() {
        let db = db();
        assert!(db.thread("done:%9:zzz").unwrap().is_none());
    }

    #[test]
    fn thread_joins_launch_reports_steering_and_acks() {
        let db = db();
        // One session's story: a finding dispatches pane %5 an hour before the anchor
        // report. The superseded report is two days old, so only the transcript joins
        // it; a five-day-old card of the same pane and an unrelated card stay out.
        let t_a2 = 1_700_000_000_000;
        let (t_a1, t_ask) = (t_a2 - 216_000_000, t_a2 - 1_800_000);
        let t_finding = t_a1 - 7_200_000;
        let t_launch = t_a2 - 3_600_000;
        let finding = thread_card("event:1:dagu://x:001", Kind::Finding, t_finding, "dagu://x", None, &["/tmp/producer.txt"]);
        let a1 = thread_card("done:%5:a1", Kind::Info, t_a1, "tmux://pane/%5", Some("%5"), &["T1"]);
        let ask = thread_card("ask:%5:q1", Kind::Ask, t_ask, "tmux://pane/%5", Some("%5"), &[]);
        let a2 = thread_card("done:%5:a2", Kind::Info, t_a2, "tmux://pane/%5", Some("%5"), &["T1"]);
        let old = thread_card("done:%5:old", Kind::Info, t_a2 - 5 * 86_400_000, "tmux://pane/%5", Some("%5"), &["T0"]);
        let unrelated = thread_card("event:2:other://y:001", Kind::Info, t_a1, "other://y", Some("%9"), &[]);
        for card in [&finding, &a1, &ask, &a2, &old, &unrelated] {
            db.insert_card(card).unwrap();
        }
        db.swipe(&reason_swipe(&finding.id, "done", "launched %5", t_launch)).unwrap();
        db.swipe(&reason_swipe(&a1.id, "done", "superseded", t_a1 + 1)).unwrap();
        db.swipe(&swipe(&a2.id, "done", t_a2 + 1, None)).unwrap();

        let thread = db.thread(&a2.id).unwrap().expect("anchor threads");

        let ids: Vec<&str> = thread.items.iter().map(|item| item.card.id.as_str()).collect();
        assert_eq!(ids, ["event:1:dagu://x:001", "done:%5:a1", "ask:%5:q1", "done:%5:a2"]);
        assert_eq!(thread.truncated, 0);
        let by_id = |id: &str| thread.items.iter().find(|item| item.card.id == id).unwrap();
        assert_eq!(
            by_id(&finding.id).swipes.iter().map(|s| s.reason.as_deref()).collect::<Vec<_>>(),
            [Some("launched %5")],
        );
        assert_eq!(
            by_id(&a1.id).swipes.iter().map(|s| s.reason.as_deref()).collect::<Vec<_>>(),
            [Some("superseded")],
        );
        assert_eq!(by_id(&a2.id).swipes.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(), ["done"]);
        assert_eq!(by_id(&a1.id).card.body.as_deref(), Some("Body of done:%5:a1"));
    }

    fn event(id: &str) -> CloudEvent {
        CloudEvent {
            specversion: "1.0".into(),
            id: id.into(),
            source: "test".into(),
            r#type: "test.event".into(),
            subject: None,
            time: None,
            priority: Some(3),
            data: serde_json::json!({"ok": true}),
        }
    }

    #[test]
    fn migration_rekeys_legacy_events_with_out_of_range_priorities() {
        let path = std::env::temp_dir().join(format!("triage-db-migration-{}.sqlite", uuid::Uuid::new_v4()));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE events (
                    id TEXT PRIMARY KEY,
                    ts_ms INTEGER NOT NULL,
                    source TEXT NOT NULL,
                    type TEXT NOT NULL,
                    card_id TEXT,
                    priority INTEGER,
                    data TEXT NOT NULL
                )",
            )
            .unwrap();
        for (id, priority) in [("zero", 0), ("six", 6)] {
            let data = serde_json::json!({
                "specversion": "1.0",
                "id": id,
                "source": "test",
                "type": "test.event",
                "priority": priority,
                "data": {}
            });
            connection
                .execute(
                    "INSERT INTO events(id, ts_ms, source, type, card_id, priority, data)
                     VALUES (?1, 1, 'test', 'test.event', NULL, ?2, ?3)",
                    params![id, priority, data.to_string()],
                )
                .unwrap();
        }
        drop(connection);

        let db = Db::open(&path).unwrap();
        let connection = db.conn.lock().unwrap();
        for id in ["zero", "six"] {
            let expected = cloud_event_key("test", id);
            let migrated_id: Option<String> = connection
                .query_row(
                    "SELECT id FROM events WHERE id = ?1",
                    params![expected.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            assert_eq!(migrated_id.as_deref(), Some(expected.as_str()));
        }
        drop(connection);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn legacy_card_links_migrate_through_target_chains_for_versions_zero_and_one() {
        let incoming_id = cloud_event_key("urn:x", "y");
        let mut first_event = event(&incoming_id);
        first_event.source = "urn:a".into();
        first_event.r#type = crate::events::T_INFO.into();
        first_event.data = serde_json::json!({"title": "legacy first", "opaque": {"keep": true}});
        let first_target = cloud_event_key(&first_event.source, &first_event.id);
        let mut second_event = event(&first_target);
        second_event.source = "urn:c".into();
        second_event.r#type = crate::events::T_INFO.into();
        second_event.data = serde_json::json!({"title": "legacy chain"});
        let second_target = cloud_event_key(&second_event.source, &second_event.id);
        let events = [first_event, second_event];
        let targets = [first_target, second_target];

        for version in [0, 1] {
            let mut conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE events (
                    id TEXT PRIMARY KEY,
                    ts_ms INTEGER NOT NULL,
                    source TEXT NOT NULL,
                    type TEXT NOT NULL,
                    card_id TEXT,
                    priority INTEGER,
                    data TEXT NOT NULL
                )",
            )
            .unwrap();
            for (index, event) in events.iter().enumerate() {
                let card_id = event.id.as_str();
                let event_key = cloud_event_key(&event.source, &event.id);
                let storage_id: &str = if version == 0 { card_id } else { event_key.as_str() };
                let event_rowid = index as i64 + 1;
                let mut legacy_card = card(card_id, 3, 20 + index as i64, "%8");
                legacy_card.source.uri = event.source.clone();
                legacy_card.source.label = event.source.clone();
                legacy_card.source.pane = None;
                legacy_card.title = format!("legacy-{index}");
                conn.execute(
                    "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        event_rowid,
                        storage_id,
                        10 + event_rowid,
                        event.source,
                        event.r#type,
                        card_id,
                        event.priority.map(i64::from),
                        to_json(event).unwrap(),
                    ],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        event_rowid + 2,
                        format!("card:{card_id}"),
                        legacy_card.created_ms,
                        legacy_card.source.uri,
                        legacy_card.card_event_type(),
                        card_id,
                        i64::from(legacy_card.priority),
                        to_json(&legacy_card).unwrap(),
                    ],
                )
                .unwrap();
                let action = if index == 0 { "skipped" } else { "later" };
                let mut legacy_swipe = swipe(card_id, action, 30 + index as i64, Some(1));
                legacy_swipe.note = Some(format!("note-{index}"));
                legacy_swipe.reason = Some(format!("reason-{index}"));
                conn.execute(
                    "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                     VALUES (?1, ?2, ?3, 'triage', ?4, ?5, NULL, ?6)",
                    params![
                        event_rowid + 4,
                        format!("swipe:legacy-{index}"),
                        legacy_swipe.at_ms,
                        crate::events::T_SWIPE,
                        card_id,
                        to_json(&legacy_swipe).unwrap(),
                    ],
                )
                .unwrap();
            }
            let mut pane_ask = card("pane-ask", 1, 40, "%8");
            pane_ask.kind = Kind::Ask;
            let pane_ask_data = to_json(&pane_ask).unwrap();
            conn.execute(
                "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                 VALUES (7, 'card:pane-ask', ?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    pane_ask.created_ms,
                    pane_ask.source.uri,
                    pane_ask.card_event_type(),
                    pane_ask.id,
                    i64::from(pane_ask.priority),
                    pane_ask_data,
                ],
            )
            .unwrap();
            let mut pane_done = card("pane-done", 1, 41, "%8");
            pane_done.body = Some("Done".into());
            let pane_done_data = to_json(&pane_done).unwrap();
            conn.execute(
                "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                 VALUES (8, 'card:pane-done', ?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    pane_done.created_ms,
                    pane_done.source.uri,
                    pane_done.card_event_type(),
                    pane_done.id,
                    i64::from(pane_done.priority),
                    pane_done_data,
                ],
            )
            .unwrap();
            let mut notice = event("custom-notice");
            notice.source = "watcher://custom".into();
            notice.r#type = "custom.notice".into();
            notice.data = serde_json::json!({"detail": "unchanged"});
            let notice_key = cloud_event_key(&notice.source, &notice.id);
            let notice_storage_id = if version == 0 {
                notice.id.as_str()
            } else {
                notice_key.as_str()
            };
            conn.execute(
                "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                 VALUES (9, ?1, 50, ?2, ?3, NULL, ?4, ?5)",
                params![
                    notice_storage_id,
                    notice.source,
                    notice.r#type,
                    notice.priority.map(i64::from),
                    to_json(&notice).unwrap(),
                ],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();

            migrate_cloud_event_keys(&mut conn).unwrap();

            assert_eq!(
                conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                2
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                9
            );
            for (index, (event, target)) in events.iter().zip(&targets).enumerate() {
                let event_rowid = index as i64 + 1;
                let (stored_id, event_ts_ms, card_id, source, event_type, priority, payload): (
                    String,
                    i64,
                    Option<String>,
                    String,
                    String,
                    Option<i64>,
                    String,
                ) = conn
                    .query_row(
                        "SELECT id, ts_ms, card_id, source, type, priority, data FROM events WHERE rowid = ?1",
                        params![event_rowid],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                            ))
                        },
                    )
                    .unwrap();
                assert_eq!(event_ts_ms, 10 + event_rowid);
                assert_eq!(stored_id, *target);
                assert_eq!(card_id.as_deref(), Some(target.as_str()));
                assert_eq!(source, event.source);
                assert_eq!(event_type, event.r#type);
                assert_eq!(priority, event.priority.map(i64::from));
                assert_eq!(payload, to_json(event).unwrap());

                let card_key = format!("card:{target}");
                let card_rowid: i64 = conn
                    .query_row(
                        "SELECT rowid FROM events WHERE id = ?1",
                        params![card_key],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(card_rowid, event_rowid + 2);
                let (card_ts_ms, card_source, card_type, card_priority, card_data): (
                    i64,
                    String,
                    String,
                    Option<i64>,
                    String,
                ) = conn
                    .query_row(
                        "SELECT ts_ms, source, type, priority, data FROM events WHERE rowid = ?1",
                        params![card_rowid],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                    )
                    .unwrap();
                assert_eq!(card_ts_ms, 20 + index as i64);
                assert_eq!(card_source, event.source);
                assert_eq!(card_type, crate::events::T_CARD_INFO);
                assert_eq!(card_priority, Some(3));
                let migrated_card: Card = from_json(card_data).unwrap();
                assert_eq!(migrated_card.id, *target);
                assert_eq!(migrated_card.title, format!("legacy-{index}"));
                assert_eq!(migrated_card.source.uri, event.source);
                let stored_card_id: Option<String> = conn
                    .query_row(
                        "SELECT card_id FROM events WHERE rowid = ?1",
                        params![card_rowid],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(stored_card_id.as_deref(), Some(target.as_str()));

                let migrated_swipe = latest_swipe(&conn, target).unwrap().unwrap();
                assert_eq!(migrated_swipe.card, *target);
                assert_eq!(migrated_swipe.action, if index == 0 { "skipped" } else { "later" });
                assert_eq!(migrated_swipe.note.as_deref(), Some(if index == 0 { "note-0" } else { "note-1" }));
                assert_eq!(migrated_swipe.reason.as_deref(), Some(if index == 0 { "reason-0" } else { "reason-1" }));
                assert_eq!(migrated_swipe.at_ms, 30 + index as i64);
                let (
                    swipe_rowid,
                    swipe_ts_ms,
                    swipe_source,
                    swipe_type,
                    swipe_card_id,
                    swipe_priority,
                ): (i64, i64, String, String, Option<String>, Option<i64>) = conn
                    .query_row(
                        "SELECT rowid, ts_ms, source, type, card_id, priority FROM events WHERE id = ?1",
                        params![format!("swipe:legacy-{index}")],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                            ))
                        },
                    )
                    .unwrap();
                assert_eq!(swipe_rowid, event_rowid + 4);
                assert_eq!(swipe_ts_ms, 30 + index as i64);
                assert_eq!(swipe_source, "triage");
                assert_eq!(swipe_type, crate::events::T_SWIPE);
                assert_eq!(swipe_card_id.as_deref(), Some(target.as_str()));
                assert_eq!(swipe_priority, None);
            }
            for (rowid, card_id, data) in [
                (7, "pane-ask", pane_ask_data),
                (8, "pane-done", pane_done_data),
            ] {
                let (stored_id, stored_card_id, stored_data): (String, Option<String>, String) =
                    conn.query_row(
                        "SELECT id, card_id, data FROM events WHERE rowid = ?1",
                        params![rowid],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap();
                assert_eq!(stored_id, format!("card:{card_id}"));
                assert_eq!(stored_card_id.as_deref(), Some(card_id));
                assert_eq!(stored_data, data);
            }
            let (notice_id, notice_card_id, notice_payload): (
                String,
                Option<String>,
                String,
            ) = conn
                .query_row(
                    "SELECT id, card_id, data FROM events WHERE rowid = 9",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(notice_id, notice_key);
            assert_eq!(notice_card_id, None);
            assert_eq!(notice_payload, to_json(&notice).unwrap());
            let old_collision_card: Option<i64> = conn
                .query_row(
                    "SELECT rowid FROM events WHERE id = ?1",
                    params![format!("card:{incoming_id}")],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            assert_eq!(old_collision_card, None);
        }
    }

    #[test]
    fn concurrent_openers_do_not_rekey_migrated_events_twice() {
        use std::ffi::{c_int, c_uint, c_void};
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        use std::time::Duration;

        struct MigrationGate {
            reached: mpsc::Sender<()>,
            resume: mpsc::Receiver<()>,
            paused: AtomicBool,
        }

        unsafe extern "C" fn pause_after_version_check(
            event_code: c_uint,
            context: *mut c_void,
            _: *mut c_void,
            _: *mut c_void,
        ) -> c_int {
            if event_code == rusqlite::ffi::SQLITE_TRACE_PROFILE {
                let gate = unsafe { &*context.cast::<MigrationGate>() };
                if !gate.paused.swap(true, Ordering::SeqCst) {
                    let _ = gate.reached.send(());
                    let _ = gate.resume.recv();
                }
            }
            0
        }

        let database = db();
        let path = database.path().to_path_buf();
        let event = event("legacy-event");
        {
            let conn = database.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO events(id, ts_ms, source, type, card_id, priority, data)
                 VALUES (?1, 1, ?2, ?3, NULL, 3, ?4)",
                params![
                    event.id.as_str(),
                    event.source.as_str(),
                    event.r#type.as_str(),
                    to_json(&event).unwrap()
                ],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 0).unwrap();
        }
        drop(database);

        let mut first = Connection::open(&path).unwrap();
        let mut second = Connection::open(&path).unwrap();
        first.busy_timeout(Duration::from_secs(5)).unwrap();
        second.busy_timeout(Duration::from_secs(5)).unwrap();
        let (reached_tx, reached_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let gate = Box::new(MigrationGate {
            reached: reached_tx,
            resume: resume_rx,
            paused: AtomicBool::new(false),
        });
        let gate_ptr = Box::into_raw(gate);
        let status = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                first.handle(),
                rusqlite::ffi::SQLITE_TRACE_PROFILE,
                Some(pause_after_version_check),
                gate_ptr.cast(),
            )
        };
        assert_eq!(status, rusqlite::ffi::SQLITE_OK);

        let worker = std::thread::spawn(move || {
            let result = migrate_cloud_event_keys(&mut first);
            unsafe {
                rusqlite::ffi::sqlite3_trace_v2(
                    first.handle(),
                    0,
                    None,
                    std::ptr::null_mut(),
                );
            }
            result
        });
        let reached = reached_rx.recv_timeout(Duration::from_secs(5)).is_ok();
        let second_result = migrate_cloud_event_keys(&mut second);
        let _ = resume_tx.send(());
        let first_result = worker.join().unwrap();
        unsafe {
            drop(Box::from_raw(gate_ptr));
        }
        second_result.unwrap();
        first_result.unwrap();
        drop(second);

        let conn = Connection::open(&path).unwrap();
        let migrated_id = conn
            .query_row("SELECT id FROM events", [], |row| row.get::<_, String>(0))
            .unwrap();
        drop(conn);
        std::fs::remove_file(path).unwrap();
        assert!(reached);
        assert_eq!(migrated_id, cloud_event_key(&event.source, &event.id));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn legacy_cloud_event_migration_does_not_accumulate_payloads() {
        const MODE: &str = "TRIAGE_BUGHUNT_BUG014_MIGRATION_TEST_MODE";
        const DATABASE: &str = "TRIAGE_BUGHUNT_BUG014_MIGRATION_TEST_DB";
        const TEST_NAME: &str = "db::tests::legacy_cloud_event_migration_does_not_accumulate_payloads";

        if let Ok(mode) = std::env::var(MODE) {
            match mode.as_str() {
                "measure" => {
                    let size = std::fs::read_to_string("/proc/self/status")
                        .unwrap()
                        .lines()
                        .find_map(|line| line.strip_prefix("VmSize:"))
                        .and_then(|value| value.split_whitespace().next())
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap();
                    println!("{size}");
                }
                "migrate" => {
                    let path = PathBuf::from(std::env::var_os(DATABASE).unwrap());
                    let mut conn = Connection::open(path).unwrap();
                    conn.execute_batch("PRAGMA temp_store = FILE").unwrap();
                    migrate_cloud_event_keys(&mut conn).unwrap();
                }
                _ => panic!("invalid migration memory test mode"),
            }
            return;
        }

        use std::process::Command;

        const ROWS: usize = 96;
        const PAYLOAD_BYTES: usize = 1024 * 1024;
        const ADDITIONAL_LIMIT_KIB: u64 = 24 * 1024;

        let executable = std::env::current_exe().unwrap();
        let measurement = Command::new(&executable)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(MODE, "measure")
            .output()
            .unwrap();
        assert!(measurement.status.success());
        let baseline_kib = String::from_utf8_lossy(&measurement.stdout)
            .lines()
            .find_map(|line| line.parse::<u64>().ok())
            .unwrap();
        assert!((ROWS * PAYLOAD_BYTES) as u64 > ADDITIONAL_LIMIT_KIB * 1024);

        let directory = std::env::temp_dir().join(format!(
            "triage-db-migration-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("triage.sqlite");
        {
            let mut connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE events (
                        id TEXT PRIMARY KEY,
                        ts_ms INTEGER NOT NULL,
                        source TEXT NOT NULL,
                        type TEXT NOT NULL,
                        card_id TEXT,
                        priority INTEGER,
                        data TEXT NOT NULL
                    )",
                )
                .unwrap();
            let tx = connection.transaction().unwrap();
            let payload = "x".repeat(PAYLOAD_BYTES);
            for index in 0..ROWS {
                let id = format!("legacy-{index}");
                let data = serde_json::json!({
                    "specversion": "1.0",
                    "id": id.as_str(),
                    "source": "test",
                    "type": "test.event",
                    "priority": 3,
                    "data": {"payload": payload.as_str()}
                })
                .to_string();
                tx.execute(
                    "INSERT INTO events(id, ts_ms, source, type, card_id, priority, data)
                     VALUES (?1, ?2, 'test', 'test.event', NULL, 3, ?3)",
                    params![id.as_str(), index as i64, data],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }

        let memory_limit = (baseline_kib + ADDITIONAL_LIMIT_KIB).to_string();
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg("ulimit -v \"$1\" && ulimit -c 0 && exec \"$2\" --exact \"$3\" --nocapture")
            .arg("sh")
            .arg(&memory_limit)
            .arg(&executable)
            .arg(TEST_NAME)
            .env(MODE, "migrate")
            .env(DATABASE, &path)
            .env("SQLITE_TMPDIR", &directory)
            .env("TMPDIR", &directory)
            .output()
            .unwrap();
        let success = output.status.success();
        let migrated = if success {
            let conn = Connection::open(&path).unwrap();
            (
                conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                conn.query_row("SELECT COUNT(*) FROM events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
                conn.query_row("SELECT id FROM events ORDER BY rowid LIMIT 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            )
        } else {
            (0, 0, String::new())
        };
        std::fs::remove_dir_all(&directory).unwrap();

        assert!(
            success,
            "migration process failed: {:?} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(migrated.0, 2);
        assert_eq!(migrated.1, ROWS as i64);
        assert_eq!(migrated.2, cloud_event_key("test", "legacy-0"));
    }


    #[cfg(unix)]
    #[test]
    fn concurrent_sqlite_permission_setup_retries_file_creation() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, Barrier};

        let directory = std::env::temp_dir().join(format!(
            "triage-permissions-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("database.sqlite");
        let barrier = Arc::new(Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let first_path = &path;
            let first_barrier = Arc::clone(&barrier);
            let first = scope.spawn(move || {
                SQLITE_FILE_MISSING_HOOK.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        first_barrier.wait();
                    }));
                });
                restrict_file_permissions(first_path)
            });
            let second_path = &path;
            let second_barrier = Arc::clone(&barrier);
            let second = scope.spawn(move || {
                SQLITE_FILE_MISSING_HOOK.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        second_barrier.wait();
                    }));
                });
                restrict_file_permissions(second_path)
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        for result in results {
            result.unwrap();
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn event_id_deduplicates_external_event_and_card_atomically() {
        let db = db();
        let card = card("derived", 2, 5, "%1");
        assert!(db.insert_event(&event("event-1"), Some(&card)).unwrap());
        assert!(!db.insert_event(&event("event-1"), Some(&card)).unwrap());
        assert_eq!(db.version().unwrap(), 2);
        assert!(db.card("derived").unwrap().is_some());
    }

    #[test]
    fn same_event_id_from_different_sources_creates_distinct_cards() {
        let db = db();
        let first = event("shared-event");
        let first_card = card(&cloud_event_key(&first.source, &first.id), 2, 5, "%1");
        let mut second = event("shared-event");
        second.source = "other".into();
        let second_card = card(&cloud_event_key(&second.source, &second.id), 2, 6, "%2");

        assert!(db.insert_event(&first, Some(&first_card)).unwrap());
        assert!(db.insert_event(&second, Some(&second_card)).unwrap());
        assert_eq!(db.version().unwrap(), 4);
        assert!(db.card(&first_card.id).unwrap().is_some());
        assert!(db.card(&second_card.id).unwrap().is_some());
    }

    #[test]
    fn external_event_ids_cannot_collide_with_card_records() {
        let db = db();
        let event = event("card:target");
        let card = card("target", 2, 5, "%1");

        let inserted = db.insert_event(&event, Some(&card));
        assert!(
            matches!(&inserted, Ok(true)),
            "external event IDs must not collide with card records: {inserted:?}"
        );
        assert_eq!(db.version().unwrap(), 2);
        assert!(db.card(&card.id).unwrap().is_some());
    }

    #[test]
    fn queue_sorts_by_priority_then_age() {
        let db = db();
        db.insert_card(&card("later-age", 2, 20, "%1")).unwrap();
        db.insert_card(&card("high-priority", 1, 30, "%1")).unwrap();
        db.insert_card(&card("older", 2, 10, "%1")).unwrap();
        let cards = db.queue(100).unwrap();
        assert_eq!(cards.iter().map(|card| card.id.as_str()).collect::<Vec<_>>(), ["high-priority", "older", "later-age"]);
    }

    #[test]
    fn queue_reads_latest_swipes_with_one_query() {
        unsafe extern "C" fn count_statements(
            _: std::ffi::c_uint,
            context: *mut std::ffi::c_void,
            _: *mut std::ffi::c_void,
            _: *mut std::ffi::c_void,
        ) -> std::ffi::c_int {
            unsafe { *context.cast::<usize>() += 1; }
            0
        }

        let db = db();
        db.insert_card(&card("due", 2, 1, "%1")).unwrap();
        db.insert_card(&card("not-due", 2, 2, "%1")).unwrap();
        db.insert_card(&card("skipped", 2, 3, "%1")).unwrap();
        db.swipe(&swipe("due", "later", 10, Some(50))).unwrap();
        db.swipe(&swipe("not-due", "later", 11, Some(150))).unwrap();
        db.swipe(&swipe("skipped", "skipped", 12, None)).unwrap();

        let mut statements = 0usize;
        let conn = db.conn.lock().unwrap();
        let tracing = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                conn.handle(),
                rusqlite::ffi::SQLITE_TRACE_STMT,
                Some(count_statements),
                (&mut statements as *mut usize).cast(),
            )
        };
        assert_eq!(tracing, rusqlite::ffi::SQLITE_OK);
        drop(conn);

        let cards = db.queue(100);
        let conn = db.conn.lock().unwrap();
        let untracing = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                conn.handle(),
                0,
                None,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(untracing, rusqlite::ffi::SQLITE_OK);
        drop(conn);

        assert_eq!(
            cards.unwrap().iter().map(|card| card.id.as_str()).collect::<Vec<_>>(),
            ["due"]
        );
        assert_eq!(statements, 1);
    }

    #[test]
    fn queue_ignores_rows_without_card_id() {
        let db = db();
        db.insert_card(&card("expected", 1, 1, "%1")).unwrap();
        let orphan_data = to_json(&card("orphan", 2, 2, "%2")).unwrap();
        db.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO events(id, ts_ms, source, type, card_id, priority, data)
                 VALUES ('card:orphan', 2, 'test', ?1, NULL, 2, ?2)",
                params![crate::events::T_CARD_INFO, orphan_data],
            )
            .unwrap();
        assert_eq!(
            db.queue(100)
                .unwrap()
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["expected"]
        );
    }

    #[test]
    fn due_later_returns_and_skipped_stays_out_of_queue() {
        let db = db();
        db.insert_card(&card("due", 2, 1, "%1")).unwrap();
        db.insert_card(&card("not-due", 2, 2, "%1")).unwrap();
        db.insert_card(&card("skipped", 2, 3, "%1")).unwrap();
        db.swipe(&swipe("due", "later", 10, Some(50))).unwrap();
        db.swipe(&swipe("not-due", "later", 11, Some(150))).unwrap();
        db.swipe(&swipe("skipped", "skipped", 12, None)).unwrap();
        let cards = db.queue(100).unwrap();
        assert_eq!(cards.iter().map(|card| card.id.as_str()).collect::<Vec<_>>(), ["due"]);
    }

    #[test]
    fn card_returns_latest_swipe() {
        let db = db();
        db.insert_card(&card("one", 1, 1, "%1")).unwrap();
        db.swipe(&swipe("one", "later", 10, Some(20))).unwrap();
        db.swipe(&swipe("one", "done", 11, None)).unwrap();
        let (_, swipe) = db.card("one").unwrap().unwrap();
        assert_eq!(swipe.unwrap().action, "done");
    }

    #[test]
    fn history_orders_latest_swipes_newest_first() {
        let db = db();
        db.insert_card(&card("first", 1, 1, "%1")).unwrap();
        db.insert_card(&card("second", 1, 2, "%1")).unwrap();
        db.swipe(&swipe("first", "done", 10, None)).unwrap();
        db.swipe(&swipe("second", "skipped", 11, None)).unwrap();
        db.swipe(&swipe("first", "done", 12, None)).unwrap();
        let (history, count) = db.history_with_count_between(10, None).unwrap();
        assert_eq!(count, None);
        assert_eq!(history.iter().map(|(card, _)| card.id.as_str()).collect::<Vec<_>>(), ["first", "second"]);
    }

    #[test]
    fn plan_card_is_visible_in_queue_and_history_count() {
        let db = db();
        let mut plan = card("plan", 1, 10, "%1");
        plan.kind = Kind::Plan;
        db.insert_card(&plan).unwrap();
        assert_eq!(db.queue(100).unwrap()[0].id, "plan");

        db.swipe(&swipe(&plan.id, "done", 20, None)).unwrap();
        let (history, count) = db.history_with_count_between(10, Some((20, 21))).unwrap();
        assert_eq!(history[0].0.id, "plan");
        assert_eq!(count, Some(1));
    }

    #[test]
    fn unresolved_panes_include_future_deferred_ask_and_plan_cards() {
        let db = db();
        let mut open = card("open-ask", 1, 1, "%1");
        open.kind = Kind::Ask;
        let mut deferred = card("deferred-ask", 1, 2, "%2");
        deferred.kind = Kind::Ask;
        let mut deferred_plan = card("deferred-plan", 1, 5, "%5");
        deferred_plan.kind = Kind::Plan;
        let deferred_info = card("deferred-info", 1, 3, "%3");
        let mut resolved = card("resolved-ask", 1, 4, "%4");
        resolved.kind = Kind::Ask;

        for card in [&open, &deferred, &deferred_info, &deferred_plan, &resolved] {
            db.insert_card(card).unwrap();
        }
        for card in [&deferred, &deferred_info, &deferred_plan] {
            db.swipe(&swipe(&card.id, "later", 10, Some(i64::MAX)))
                .unwrap();
        }
        db.swipe(&swipe(&resolved.id, "done", 11, None)).unwrap();

        assert_eq!(
            db.unresolved_panes().unwrap(),
            std::collections::HashSet::from(["%1".to_owned(), "%2".to_owned(), "%5".to_owned()])
        );
    }

    #[test]
    fn unresolved_cards_for_pane_include_future_deferred_cards() {
        let db = db();
        let open = card("open", 1, 1, "%1");
        let mut deferred_ask = card("deferred-ask", 1, 2, "%1");
        deferred_ask.kind = Kind::Ask;
        let mut deferred_finding = card("deferred-finding", 1, 3, "%1");
        deferred_finding.kind = Kind::Finding;
        let resolved = card("resolved", 1, 4, "%1");

        for card in [&open, &deferred_ask, &deferred_finding, &resolved] {
            db.insert_card(card).unwrap();
        }
        for card in [&deferred_ask, &deferred_finding] {
            db.swipe(&swipe(&card.id, "later", 10, Some(i64::MAX)))
                .unwrap();
        }
        db.swipe(&swipe(&resolved.id, "done", 11, None)).unwrap();

        assert_eq!(
            db.unresolved_cards_for_pane("%1")
                .unwrap()
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["open", "deferred-ask", "deferred-finding"]
        );
    }

    #[test]
    fn exited_pane_recovery_closes_only_queued_and_deferred_ask_and_plan_cards() {
        let db = db();
        let mut open = card("open", 1, 0, "%1");
        open.kind = Kind::Ask;
        db.insert_card(&open).unwrap();
        let open_info = card("open-info", 1, 0, "%1");
        db.insert_card(&open_info).unwrap();
        let mut ask = card("deferred-ask", 1, 1, "%1");
        ask.kind = Kind::Ask;
        let mut finding = card("deferred-finding", 1, 2, "%1");
        finding.kind = Kind::Finding;
        let info = card("deferred-info", 1, 3, "%1");
        let mut plan = card("deferred-plan", 1, 5, "%1");
        plan.kind = Kind::Plan;
        for card in [&ask, &finding, &info, &plan] {
            db.insert_card(card).unwrap();
            db.swipe(&swipe(&card.id, "later", 10, Some(i64::MAX)))
                .unwrap();
        }

        let mut due_finding = card("due-finding", 1, 4, "%1");
        due_finding.kind = Kind::Finding;
        db.insert_card(&due_finding).unwrap();
        db.swipe(&swipe(&due_finding.id, "later", 12, Some(0)))
            .unwrap();

        let cards = db.cards_for_exited_pane("%1").unwrap();
        assert_eq!(
            cards.iter().map(|card| card.id.as_str()).collect::<Vec<_>>(),
            ["open", "deferred-ask", "deferred-plan"]
        );
    }

    #[test]
    fn pane_closed_since_matches_only_recent_closes_of_that_pane() {
        let db = db();
        let mut closed = swipe("done:%12:a1", "done", 1_000, None);
        closed.reason = Some("closed %12".into());
        db.swipe(&closed).unwrap();

        assert!(db.pane_closed_since("%12", 999).unwrap());
        assert!(db.pane_closed_since("%12", 1_000).unwrap());
        assert!(!db.pane_closed_since("%12", 1_001).unwrap());
        assert!(!db.pane_closed_since("%1", 0).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_database_file() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = std::env::temp_dir().join(format!(
            "triage-db-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let database = directory.join("db.sqlite");
        let target = directory.join("target.sqlite");
        std::fs::write(&target, "unchanged").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&target, &database).unwrap();

        assert!(Db::open(&database).is_err());
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_sqlite_sidecars_without_touching_targets() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        for suffix in ["-wal", "-shm"] {
            let directory = std::env::temp_dir().join(format!(
                "triage-db-sidecar-symlink-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&directory).unwrap();
            let database = directory.join("db.sqlite");
            let target = directory.join("target");
            std::fs::write(&target, "unchanged").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
            let mut sidecar = database.as_os_str().to_os_string();
            sidecar.push(suffix);
            symlink(&target, Path::new(&sidecar)).unwrap();

            assert!(Db::open(&database).is_err());
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o644
            );
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
            std::fs::remove_dir_all(&directory).unwrap();
        }
    }

    #[test]
    fn queue_reads_skip_handled_cards_without_parsing_them() {
        let db = db();
        let mut open = card("open", 3, 10, "%1");
        open.kind = Kind::Ask;
        db.insert_card(&open).unwrap();
        // A handled row that no longer parses must not break the queue: handled rows are never read.
        db.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO events(id, ts_ms, source, type, card_id, priority, data)
                 VALUES ('card:handled', 5, 'pane:%1', ?1, 'handled', 3, '{\"id\":\"handled\"}')",
                params![crate::events::T_CARD_INFO],
            )
            .unwrap();
        db.swipe(&swipe("handled", "done", 20, None)).unwrap();
        let ids = |cards: Vec<Card>| cards.into_iter().map(|card| card.id).collect::<Vec<_>>();

        assert_eq!(ids(db.queue(100).unwrap()), ["open"]);
        assert_eq!(ids(db.unresolved_cards_for_pane("%1").unwrap()), ["open"]);
        assert_eq!(ids(db.cards_for_exited_pane("%1").unwrap()), ["open"]);
    }

}
