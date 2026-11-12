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
    db.insert_card(&card("permission-check", 1, 1, "%1"))
        .unwrap();
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
        conn.query_row("SELECT count(*) FROM events", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
        return;
    }

    let directory =
        std::env::temp_dir().join(format!("triage-db-outside-{}", uuid::Uuid::new_v4()));
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

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        wal_kept,
        "an outside connection deleted the WAL of the open database"
    );
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
        report_due: false,
        engine: None,
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
fn thread_card(
    id: &str,
    kind: Kind,
    created_ms: i64,
    uri: &str,
    pane: Option<&str>,
    refs: &[&str],
) -> Card {
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
        report_due: false,
        engine: None,
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
    let finding = thread_card(
        "event:1:dagu://x:001",
        Kind::Finding,
        t_finding,
        "dagu://x",
        None,
        &["/tmp/producer.txt"],
    );
    let a1 = thread_card(
        "done:%5:a1",
        Kind::Info,
        t_a1,
        "tmux://pane/%5",
        Some("%5"),
        &["T1"],
    );
    let ask = thread_card(
        "ask:%5:q1",
        Kind::Ask,
        t_ask,
        "tmux://pane/%5",
        Some("%5"),
        &[],
    );
    let a2 = thread_card(
        "done:%5:a2",
        Kind::Info,
        t_a2,
        "tmux://pane/%5",
        Some("%5"),
        &["T1"],
    );
    let old = thread_card(
        "done:%5:old",
        Kind::Info,
        t_a2 - 5 * 86_400_000,
        "tmux://pane/%5",
        Some("%5"),
        &["T0"],
    );
    let unrelated = thread_card(
        "event:2:other://y:001",
        Kind::Info,
        t_a1,
        "other://y",
        Some("%9"),
        &[],
    );
    for card in [&finding, &a1, &ask, &a2, &old, &unrelated] {
        db.insert_card(card).unwrap();
    }
    db.swipe(&reason_swipe(&finding.id, "done", "launched %5", t_launch))
        .unwrap();
    db.swipe(&reason_swipe(&a1.id, "done", "superseded", t_a1 + 1))
        .unwrap();
    db.swipe(&swipe(&a2.id, "done", t_a2 + 1, None)).unwrap();

    let thread = db.thread(&a2.id).unwrap().expect("anchor threads");

    let ids: Vec<&str> = thread
        .items
        .iter()
        .map(|item| item.card.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "event:1:dagu://x:001",
            "done:%5:a1",
            "ask:%5:q1",
            "done:%5:a2"
        ]
    );
    assert_eq!(thread.truncated, 0);
    let by_id = |id: &str| thread.items.iter().find(|item| item.card.id == id).unwrap();
    assert_eq!(
        by_id(&finding.id)
            .swipes
            .iter()
            .map(|s| s.reason.as_deref())
            .collect::<Vec<_>>(),
        [Some("launched %5")],
    );
    assert_eq!(
        by_id(&a1.id)
            .swipes
            .iter()
            .map(|s| s.reason.as_deref())
            .collect::<Vec<_>>(),
        [Some("superseded")],
    );
    assert_eq!(
        by_id(&a2.id)
            .swipes
            .iter()
            .map(|s| s.action.as_str())
            .collect::<Vec<_>>(),
        ["done"]
    );
    assert_eq!(
        by_id(&a1.id).card.body.as_deref(),
        Some("Body of done:%5:a1")
    );
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

#[cfg(unix)]
#[test]
fn concurrent_sqlite_permission_setup_retries_file_creation() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Barrier};

    let directory =
        std::env::temp_dir().join(format!("triage-permissions-{}", uuid::Uuid::new_v4()));
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
    assert_eq!(
        cards
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
        ["high-priority", "older", "later-age"]
    );
}

#[test]
fn queue_reads_latest_swipes_with_one_query() {
    unsafe extern "C" fn count_statements(
        _: std::ffi::c_uint,
        context: *mut std::ffi::c_void,
        _: *mut std::ffi::c_void,
        _: *mut std::ffi::c_void,
    ) -> std::ffi::c_int {
        unsafe {
            *context.cast::<usize>() += 1;
        }
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
    let untracing =
        unsafe { rusqlite::ffi::sqlite3_trace_v2(conn.handle(), 0, None, std::ptr::null_mut()) };
    assert_eq!(untracing, rusqlite::ffi::SQLITE_OK);
    drop(conn);

    assert_eq!(
        cards
            .unwrap()
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
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
    assert_eq!(
        cards
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
        ["due"]
    );
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
    assert_eq!(
        history
            .iter()
            .map(|(card, _)| card.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
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
        cards
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
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
    use std::os::unix::fs::{PermissionsExt, symlink};

    let directory =
        std::env::temp_dir().join(format!("triage-db-symlink-{}", uuid::Uuid::new_v4()));
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
    use std::os::unix::fs::{PermissionsExt, symlink};

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
