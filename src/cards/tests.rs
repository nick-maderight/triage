use super::*;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;

const CHILD_TEST: &str = "TRIAGE_CARDS_CHILD_TEST";
const CHILD_ROOT: &str = "TRIAGE_CARDS_CHILD_ROOT";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("triage-cards-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn child_root(name: &str) -> Option<PathBuf> {
    (std::env::var(CHILD_TEST).ok().as_deref() == Some(name))
        .then(|| PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap()))
}

fn run_child(name: &str, root: &Path, home: Option<&Path>, path: Option<&Path>) {
    let temporary = root.join("tmp");
    fs::create_dir_all(&temporary).unwrap();

    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg(format!("cards::tests::{name}"))
        .env(CHILD_TEST, name)
        .env(CHILD_ROOT, root)
        .env("TMPDIR", temporary)
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_STATE_HOME");
    if let Some(home) = home {
        command
            .env("HOME", home)
            .env("XDG_DATA_HOME", root.join("xdg-data"))
            .env("XDG_CONFIG_HOME", root.join("xdg-config"))
            .env("XDG_CACHE_HOME", root.join("xdg-cache"))
            .env("XDG_STATE_HOME", root.join("xdg-state"));
    } else {
        command.env_remove("HOME");
    }
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child test {name} failed or ran no test\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn fake_tmux(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let directory = root.join("bin");
    fs::create_dir_all(&directory).unwrap();
    let executable = directory.join("tmux");
    fs::write(
            &executable,
            r##"#!/bin/sh
all="$*"
if [ "${#all}" -gt 16000 ]; then
    echo 'command too long' >&2
    exit 1
fi
printf '%s\n' "$*" >> "$HOME/tmux.log"
case "$1" in
list-panes)
    if [ -f "$HOME/tmux-missing-pane" ]; then exit 0; fi
    pane='%12'
    tty='/dev/pts/12'
    title='π ! asking'
    command='omp'
    if [ -f "$HOME/tmux-new-pane" ]; then
        IFS= read -r pane < "$HOME/tmux-new-pane"
        tty='/dev/pts/42'
        title='π > session'
    fi
    if [ -f "$HOME/tmux-working" ]; then title='π ⠋ session'; fi
    if [ -f "$HOME/tmux-waiting" ]; then title='π > session'; fi
    if [ -f "$HOME/tmux-title-gone" ]; then title='host: tmp'; fi
    if [ -f "$HOME/tmux-dead" ]; then
        title='host: tmp'
        command='zsh'
    fi
    if [ -f "$HOME/tmux-tty" ]; then
        IFS= read -r tty < "$HOME/tmux-tty"
    fi
    tag_file="$HOME/tmux-tag-$pane"
    tag=''
    if [ -f "$tag_file" ]; then IFS= read -r tag < "$tag_file"; fi
    if [ ! -f "$HOME/tmux-pane-12-gone" ]; then
        printf '%s\t%s\t%s\t%s\tsession:1.0\t%s\tsession\t80\t24\t0\t%s\n' "$pane" "$title" "$command" "$tty" "$HOME" "$tag"
    fi
    if [ -f "$HOME/tmux-second-pane" ]; then
        tag13=''
        if [ -f "$HOME/tmux-tag-%13" ]; then IFS= read -r tag13 < "$HOME/tmux-tag-%13"; fi
        tty13='/dev/pts/13'
        if [ -f "$HOME/tmux-tty-13" ]; then
            IFS= read -r tty13 < "$HOME/tmux-tty-13"
        fi
        printf '%%13\tπ > live\tomp\t%s\tsession:2.0\t%s\tsession\t80\t24\t0\t%s\n' "$tty13" "$HOME" "$tag13"
    fi
    ;;
capture-pane)
    if [ -d "$HOME/tmux-capture-seq" ]; then
        count=0
        if [ -f "$HOME/tmux-captures.log" ]; then count=$(/bin/wc -l < "$HOME/tmux-captures.log"); fi
        printf 'x\n' >> "$HOME/tmux-captures.log"
        if [ -f "$HOME/tmux-capture-seq/$count" ]; then /bin/cat "$HOME/tmux-capture-seq/$count"; else /bin/cat "$HOME/tmux-capture-seq/rest"; fi
        exit 0
    fi
    if [ -d "$HOME/tmux-screens" ]; then
        count=0
        if [ -f "$HOME/tmux-keys.log" ]; then count=$(/bin/wc -l < "$HOME/tmux-keys.log"); fi
        if [ -f "$HOME/tmux-screens/$count" ]; then /bin/cat "$HOME/tmux-screens/$count"; fi
        exit 0
    fi
    capture_answered=0
    if [ -f "$HOME/tmux-answered" ]; then capture_answered=1; fi
    if [ -f "$HOME/tmux-block-capture" ] && [ "$capture_answered" = 0 ]; then
        printf '%s\n' "$$" >> "$HOME/tmux-capture-entered"
        while [ ! -f "$HOME/tmux-release-capture" ] && [ -d "$HOME" ]; do /bin/sleep 0.01; done
    fi
    if [ -f "$HOME/tmux-plan-review" ]; then
        printf '╭─ Plan Review ─╮\n'
    elif [ -f "$HOME/tmux-plan-mode" ]; then
        printf '⬢ Example Model · ◉ high · 🗺 Plan\n'
        printf 'π > session\n'
        printf '───\n───\n'
    elif [ -f "$HOME/tmux-idle-screen" ]; then
        printf '──────\n  ←←  \n──────\n'
    elif [ "$capture_answered" = 1 ]; then
        printf 'complete\n'
    elif [ -f "$HOME/tmux-other-only" ]; then
        printf '╭─ Ask ─╮\n│ ❯ ○ Other\n╰─\n'
    else
        printf '╭─ Ask ─╮\n│ ❯ ○ Red\n╰─\n'
    fi
    ;;
send-keys)
    printf '%s\n' "$*" >> "$HOME/tmux-keys.log"
    case " $* " in *" Enter "*) : > "$HOME/tmux-answered" ;; esac
    case " $* " in *" M-P "*) if [ ! -f "$HOME/tmux-plan-toggle-fails" ]; then : > "$HOME/tmux-plan-mode"; fi ;; esac
    ;;
new-window)
    count=0
    if [ -f "$HOME/tmux-new-window-count" ]; then IFS= read -r count < "$HOME/tmux-new-window-count"; fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$HOME/tmux-new-window-count"
    printf '%%42\n' > "$HOME/tmux-new-pane"
    printf '%%42\n'
    ;;
set-option)
    if [ -f "$HOME/tmux-fail-first-tag" ] && [ ! -f "$HOME/tmux-tag-failed" ]; then
        : > "$HOME/tmux-tag-failed"
        exit 1
    fi
    printf '%s\n' "$6" > "$HOME/tmux-tag-$4"
    ;;
kill-pane)
    if [ -f "$HOME/tmux-fail-kill" ]; then exit 1; fi
    if [ -f "$HOME/tmux-gone-pane" ]; then printf 'can'\''t find pane: %s\n' "$3" >&2; exit 1; fi
    ;;
*) exit 0 ;;
esac
"##,
        )
        .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions).unwrap();
    directory
}

fn ask_call(call_id: &str, labels: &[&str]) -> String {
    let options = labels
        .iter()
        .map(|label| serde_json::json!({"label": label}))
        .collect::<Vec<_>>();
    serde_json::json!({
        "type": "message",
        "id": format!("assistant-{call_id}"),
        "message": {
            "role": "assistant",
            "content": [{
                "type": "toolCall",
                "id": call_id,
                "name": "ask",
                "arguments": {"questions": [{
                    "id": "choice",
                    "question": "Choose one",
                    "options": options
                }]}
            }]
        }
    })
    .to_string()
}

fn tool_result(call_id: &str, label: &str) -> String {
    serde_json::json!({
        "type": "message",
        "message": {
            "role": "toolResult",
            "toolCallId": call_id,
            "details": {"selectedOptions": [label]}
        }
    })
    .to_string()
}

fn map_transcript(root: &Path, tty: &str, transcript: &Path) {
    let sessions = root.join(".omp/agent/terminal-sessions");
    fs::create_dir_all(&sessions).unwrap();
    let name = Path::new(tty).file_name().unwrap();
    fs::write(
        sessions.join(format!("pts-{}", name.to_string_lossy())),
        format!("session\n{}\n", transcript.display()),
    )
    .unwrap();
}

fn write_transcript(root: &Path, tty: &str, transcript: &Path, lines: &[String]) {
    map_transcript(root, tty, transcript);
    fs::write(transcript, format!("{}\n", lines.join("\n"))).unwrap();
}

fn ensure_directory_length(path: &Path, minimum: usize) -> u64 {
    let mut entry = 0;
    loop {
        let length = fs::metadata(path).unwrap().len();
        if length >= minimum as u64 {
            return length;
        }
        fs::write(path.join(format!("entry-{entry}")), b"").unwrap();
        entry += 1;
    }
}

fn padded_json_line(line: &str, length: u64) -> Vec<u8> {
    let length = usize::try_from(length).unwrap();
    let mut bytes = line.as_bytes().to_vec();
    assert!(bytes.len() < length);
    bytes.resize(length - 1, b' ');
    bytes.push(b'\n');
    bytes
}

fn replace_directory_with_line(path: &Path, line: &str, length: u64) {
    fs::remove_dir_all(path).unwrap();
    fs::write(path, padded_json_line(line, length)).unwrap();
    assert_eq!(fs::metadata(path).unwrap().len(), length);
}

fn ask_card(pane: &str, call_id: &str) -> Card {
    Card {
        id: format!("ask:{pane}:{call_id}"),
        kind: Kind::Ask,
        priority: 2,
        created_ms: events::now_ms(),
        source: Source {
            uri: format!("pane:{pane}"),
            label: pane.to_owned(),
            pane: Some(pane.to_owned()),
            target: Some("session:1.0".to_owned()),
            ..Source::default()
        },
        title: "Question".to_owned(),
        star: Star::default(),
        options: ["Red", "Blue"]
            .into_iter()
            .map(|label| CardOption {
                label: label.to_owned(),
                ..CardOption::default()
            })
            .collect(),
        answerable: true,
        allow_other: true,
        questions: Vec::new(),
        body: None,
        refs: Vec::new(),
        level: None,
        tool_call_id: Some(call_id.to_owned()),
        report_due: false,
        images: Vec::new(),
        engine: None,
    }
}

fn finding_card(root: &Path) -> Card {
    Card {
        id: format!("finding-{}", uuid::Uuid::new_v4()),
        kind: Kind::Finding,
        priority: 1,
        created_ms: events::now_ms(),
        source: Source { cwd: root.display().to_string(), ..Source::default() },
        title: "Finding".to_owned(),
        star: Star::default(),
        options: ["Apply", "Defer"].into_iter().map(|label| CardOption { label: label.to_owned(), ..CardOption::default() }).collect(),
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: None,
        refs: Vec::new(),
        level: None,
        tool_call_id: None,
        report_due: false,
        images: Vec::new(),
        engine: None,
    }
}

fn plan_card(pane: &tmux::Pane, index: usize) -> Card {
    Card {
        id: format!("plan:{}:proposal-{index}", pane.id),
        kind: Kind::Plan,
        priority: 1,
        created_ms: events::now_ms(),
        source: pane_source(pane),
        title: format!("Plan ready: proposal-{index}"),
        star: Star::default(),
        options: vec![
            CardOption { label: "Approve".to_owned(), recommended: true, ..CardOption::default() },
            CardOption { label: "Refine".to_owned(), ..CardOption::default() },
        ],
        answerable: true,
        allow_other: true,
        questions: Vec::new(),
        body: Some("Plan body".to_owned()),
        refs: Vec::new(),
        level: None,
        tool_call_id: None,
        report_due: false,
        images: Vec::new(),
        engine: None,
    }
}

fn pane(root: &Path, id: &str, tty: &str, title: &str) -> tmux::Pane {
    tmux::Pane {
        id: id.to_owned(),
        title: title.to_owned(),
        command: "omp".to_owned(),
        tty: tty.to_owned(),
        target: "session:1.0".to_owned(),
        session: "session".to_owned(),
        triage_card_tag: None,
        cwd: root.display().to_string(),
        width: 80,
        height: 24,
        pid: 0,
    }
}

fn info_card(id: &str, pane: &str) -> Card {
    Card {
        id: id.to_owned(),
        kind: Kind::Info,
        priority: 1,
        created_ms: events::now_ms(),
        source: Source { uri: format!("pane:{pane}"), label: pane.to_owned(), pane: Some(pane.to_owned()), ..Source::default() },
        title: "Earlier result".to_owned(),
        star: Star::default(),
        options: Vec::new(),
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: Some("Earlier report".to_owned()),
        refs: Vec::new(),
        level: Some("info".to_owned()),
        tool_call_id: None,
        report_due: false,
        images: Vec::new(),
        engine: None,
    }
}

fn future_later(card: &str) -> Swipe {
    Swipe {
        card: card.to_owned(),
        action: "later".to_owned(),
        option: None,
        text: None,
        note: None,
        until_ms: Some(events::now_ms() + 86_400_000),
        reason: Some("deferred".to_owned()),
        at_ms: events::now_ms(),
    }
}

fn current_swipe(db: &Db, id: &str) -> Swipe {
    db.card(id).unwrap().unwrap().1.unwrap()
}

fn answer_req(option: usize, note: Option<&str>) -> AnswerReq {
    AnswerReq {
        option: Some(option),
        note: note.map(str::to_owned),
        ..AnswerReq::default()
    }
}

fn in_session(name: &str) -> Launch {
    Launch {
        session: Some(name.to_owned()),
        ..Launch::default()
    }
}

fn new_window_count(root: &Path) -> usize {
    fs::read_to_string(root.join("tmux.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("new-window "))
        .count()
}

fn tag_call_count(root: &Path) -> usize {
    fs::read_to_string(root.join("tmux.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("set-option "))
        .count()
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_for_lock_waiter<T>(lock: &Arc<Mutex<()>>, count: usize, completed: &mpsc::Receiver<T>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Arc::strong_count(lock) < count {
        assert!(
            completed.try_recv().is_err(),
            "worker completed before reaching the card lock"
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for card lock waiter"
        );
        thread::yield_now();
    }
}

fn install_swipe_failure(path: &Path, name: &str, action: &str) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            r#"CREATE TRIGGER {name} BEFORE INSERT ON events
                   WHEN NEW.type = 'swipe' AND NEW.data LIKE '%"action":"{action}"%'
                   BEGIN SELECT RAISE(ABORT, 'forced swipe failure'); END;"#
        ))
        .unwrap();
    connection
}

/// A finding with three questions. Stock disallows custom text.
#[cfg(unix)]
fn question_finding_card(root: &Path) -> Card {
    let option = |label: &str| CardOption { label: label.to_owned(), ..CardOption::default() };
    let question = |label: &str, options: Vec<CardOption>, allow_other: bool| CardQuestion {
        label: label.to_owned(),
        question: format!("{label}?"),
        options,
        allow_other,
    };
    Card {
        questions: vec![
            question("Supplier", vec![option("Do as recommended"), option("Skip")], true),
            question("Stock", vec![option("Investigate first"), option("Keep")], false),
            question("Note", vec![option("skip")], true),
        ],
        ..finding_card(root)
    }
}

#[cfg(unix)]
fn picked(option: usize) -> events::Answer {
    events::Answer { option: Some(option), ..events::Answer::default() }
}

#[cfg(unix)]
fn typed(text: &str) -> events::Answer {
    events::Answer { text: Some(text.to_owned()), ..events::Answer::default() }
}

#[cfg(unix)]
fn question_req(answers: Vec<events::Answer>, note: Option<&str>) -> AnswerReq {
    AnswerReq { answers, note: note.map(str::to_owned), ..AnswerReq::default() }
}

#[test]
fn finding_brief_includes_agent_done_status_contract() {
    let card = finding_card(Path::new("/tmp"));
    let option = CardOption {
        label: "Do as recommended".to_owned(),
        ..CardOption::default()
    };
    let brief = finding_brief(&card, &option, None, DispatchMode::Act);
    assert!(
        brief.contains("ENVOY_STATUS: complete")
            && brief.contains("ENVOY_STATUS: agent-done")
            && brief.contains("and names that step")
            && brief.contains("## Action\n")
            && brief.contains(ACT_INSTRUCTION)
    );
    assert!(brief.contains(&format!("{ENVOY_STATUS_CONTRACT}{REPORT_STEP}")));
}

// Regression: on 2026-10-31 the user picked "Investigate first" on a transcript-audit finding
// and the launched session shipped the finding's skill edit, because the brief carried the
// action, no investigate instruction, and a status contract that only counted shipped work.
#[test]
fn investigate_first_option_briefs_a_read_only_answer() {
    let card = finding_card(Path::new("/tmp"));
    let option = CardOption {
        label: "Investigate first".to_owned(),
        ..CardOption::default()
    };
    let brief = finding_brief(&card, &option, None, DispatchMode::Investigate);
    assert!(brief.contains("## Proposed action, not approved\n"));
    assert!(!brief.contains("## Action\n"));
    assert!(brief.contains(INVESTIGATE_INSTRUCTION));
    assert!(brief.contains(INVESTIGATE_STATUS_CONTRACT));
    assert!(!brief.contains("ENVOY_STATUS: agent-done"));
    assert!(brief.contains("`Recommended action:`"));
    assert!(brief.contains(&format!("{INVESTIGATE_STATUS_CONTRACT}{REPORT_STEP}")));
}
#[test]
fn finding_brief_plan_mode_includes_review_contract() {
    let card = finding_card(Path::new("/tmp"));
    let option = CardOption { label: "Do as recommended".to_owned(), ..CardOption::default() };
    let brief = finding_brief(&card, &option, None, DispatchMode::Plan);
    assert!(brief.contains("## Action\n"));
    assert!(brief.contains(PLAN_INSTRUCTION));
    assert!(brief.ends_with(&format!("{ENVOY_STATUS_CONTRACT}{REPORT_STEP}\n{PLAN_REPORT_STEP}\n")));
}
#[cfg(unix)]
#[test]
fn finding_answer_plan_mode_writes_reviewable_brief_and_config_overlay() {
    if let Some(root) = child_root("finding_answer_plan_mode_writes_reviewable_brief_and_config_overlay") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = finding_card(&root);
        db.insert_card(&card).unwrap();
        let result = answer(
            &db,
            &card,
            &AnswerReq { option: Some(0), mode: Some("plan".to_owned()), ..AnswerReq::default() },
            &in_session("session"),
        None,
        );
        let brief_path = fs::read_dir(root.join("briefs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("triage-brief-"))
            .unwrap();
        let overlay_path = fs::read_dir(root.join("briefs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("plan-mode-"))
            .unwrap();
        let brief = fs::read_to_string(&brief_path).unwrap();
        let launch = fs::read_to_string(root.join("tmux.log"))
            .unwrap()
            .lines()
            .find(|line| line.starts_with("new-window "))
            .unwrap()
            .to_owned();
        assert!(result.is_ok());
        assert!(brief.starts_with(&format!("# {}", card.title)));
        assert!(brief.contains(PLAN_INSTRUCTION));
        assert!(brief.contains(PLAN_REPORT_STEP));
        let heading = format!("# {}", card.title);
        assert!(launch.contains(&format!("--config '{}' '{}' @'{}'", overlay_path.display(), heading, brief_path.display())));
        assert_eq!(new_window_count(&root), 1);
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_answer_plan_mode_writes_reviewable_brief_and_config_overlay",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

// On 2026-10-31 a plan session approved with "Approve and execute" finished without a
// status line, so triage queued no card. The fresh session saw only the plan, and nothing
// had told the planner to copy the brief's status contract into it.
#[test]
fn plan_briefs_make_the_plan_repeat_the_status_contract_for_a_fresh_session() {
    let card = info_card("done:%12:plan", "%12");
    let dispatched = dispatch_brief(&card, DispatchMode::Plan, None, None);
    let composed = session_brief("Prepare a plan.", true, None);
    assert!(
        dispatched.ends_with(&format!(
            "{ENVOY_STATUS_CONTRACT}{REPORT_STEP}\n{PLAN_REPORT_STEP}\n"
        )),
        "{dispatched}"
    );
    assert!(
        composed.ends_with(&format!(
            "{ENVOY_STATUS_CONTRACT}{REPORT_STEP}\n{PLAN_REPORT_STEP}\n"
        )),
        "{composed}"
    );
    assert!(!dispatch_brief(&card, DispatchMode::Act, None, None).contains(PLAN_REPORT_STEP));
    assert!(!session_brief("Reply with pong.", false, None).contains(PLAN_REPORT_STEP));
    assert!(
        dispatch_brief(&card, DispatchMode::Act, None, None)
            .ends_with(&format!("{ENVOY_STATUS_CONTRACT}{REPORT_STEP}"))
    );
    assert!(
        session_brief("Reply with pong.", false, None)
            .ends_with(&format!("{ENVOY_STATUS_CONTRACT}{REPORT_STEP}"))
    );
}

#[test]
fn finding_briefs_use_private_unique_paths_for_long_ids_and_database_paths() {
    if let Some(root) =
        child_root("finding_briefs_use_private_unique_paths_for_long_ids_and_database_paths")
    {
        let configured = root.join("configured");
        fs::create_dir_all(&configured).unwrap();
        let configured_db = Db::open(&configured.join("triage.db")).unwrap();
        let short_id = format!("finding-{}", uuid::Uuid::new_v4());
        let short_card = Card {
            id: short_id.clone(),
            ..finding_card(&root)
        };
        let option = CardOption {
            label: "Apply".to_owned(),
            ..CardOption::default()
        };
        let first = write_brief(&configured_db, finding_brief(&short_card, &option, None, DispatchMode::Act));
        let second = write_brief(&configured_db, finding_brief(&short_card, &option, None, DispatchMode::Act));
        let expected_directory = configured.join("briefs");
        let first_path = first.as_ref().ok();
        let second_path = second.as_ref().ok();
        let unique = first_path
            .zip(second_path)
            .is_some_and(|(first, second)| first != second);
        let parent_matches = first_path
            .and_then(|path| path.parent())
            .is_some_and(|parent| parent == expected_directory.as_path());
        #[cfg(unix)]
        let modes_match = first_path.is_some_and(|path| {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&expected_directory)
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o777 == 0o700)
                && fs::metadata(path)
                    .is_ok_and(|metadata| metadata.permissions().mode() & 0o777 == 0o600)
        });
        #[cfg(not(unix))]
        let modes_match = true;

        let long_card = Card {
            id: format!("finding-{}", "x".repeat(300)),
            ..finding_card(&root)
        };
        let long_brief = write_brief(&configured_db, finding_brief(&long_card, &option, None, DispatchMode::Act));
        let long_id_uses_bounded_path = long_brief
            .as_ref()
            .is_ok_and(|path| path.parent() == Some(expected_directory.as_path()));

        let relative = root.join("relative");
        fs::create_dir_all(&relative).unwrap();
        std::env::set_current_dir(&relative).unwrap();
        let relative_db = Db::open(Path::new("triage.db")).unwrap();
        let relative_brief = write_brief(&relative_db, finding_brief(&short_card, &option, None, DispatchMode::Act));
        let expected_relative_parent = fs::canonicalize(&relative).unwrap().join("briefs");
        let relative_db_path_is_canonical = relative_brief.as_ref().is_ok_and(|path| {
            path.is_absolute() && path.parent() == Some(expected_relative_parent.as_path())
        });

        #[cfg(unix)]
        let symlink_target_untouched = {
            use std::os::unix::fs::symlink;
            let symlink_root = root.join("symlink-case");
            fs::create_dir_all(&symlink_root).unwrap();
            let symlink_db = Db::open(&symlink_root.join("triage.db")).unwrap();
            let sentinel = root.join("sentinel");
            fs::write(&sentinel, "unchanged").unwrap();
            let symlink_card = Card {
                id: format!("finding-{}", uuid::Uuid::new_v4()),
                ..finding_card(&root)
            };
            let legacy_path =
                std::env::temp_dir().join(format!("triage-brief-{}.md", symlink_card.id));
            symlink(&sentinel, &legacy_path).unwrap();
            symlink_db.insert_card(&symlink_card).unwrap();
            let result = answer(
                &symlink_db,
                &symlink_card,
                &answer_req(0, None),
                &in_session("session"),
            None,
            );
            let link_remains = fs::symlink_metadata(&legacy_path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink());
            let sentinel_untouched =
                fs::read_to_string(&sentinel).is_ok_and(|contents| contents == "unchanged");
            let _ = fs::remove_file(legacy_path);
            let swipe = symlink_db
                .card(&symlink_card.id)
                .unwrap()
                .and_then(|(_, swipe)| swipe);
            let launched =
                swipe.is_some_and(|swipe| swipe.reason.as_deref() == Some("launched %42"));
            result.is_ok() && link_remains && sentinel_untouched && launched
        };
        #[cfg(not(unix))]
        let symlink_target_untouched = true;

        assert!(
            symlink_target_untouched,
            "finding answer must preserve an existing temp-path symlink target"
        );
        assert!(
            long_id_uses_bounded_path,
            "card IDs must not become filename components"
        );
        assert!(
            parent_matches,
            "briefs must be stored beside the configured database"
        );
        assert!(
            relative_db_path_is_canonical,
            "relative database brief paths must be canonical"
        );
        assert!(unique, "each finding brief must have a fresh path");
        assert!(modes_match, "brief directory and file must be private");
        assert!(first.is_ok() && second.is_ok() && long_brief.is_ok() && relative_brief.is_ok());
        return;
    }

    let scratch = Scratch::new();
    #[cfg(unix)]
    {
        let bin = fake_tmux(scratch.path());
        run_child(
            "finding_briefs_use_private_unique_paths_for_long_ids_and_database_paths",
            scratch.path(),
            Some(scratch.path()),
            Some(&bin),
        );
    }
    #[cfg(not(unix))]
    run_child(
        "finding_briefs_use_private_unique_paths_for_long_ids_and_database_paths",
        scratch.path(),
        None,
        None,
    );
}

#[cfg(unix)]
#[test]
fn startup_reconciles_live_waiting_and_missing_future_asks_on_first_poll() {
    if let Some(root) =
        child_root("startup_reconciles_live_waiting_and_missing_future_asks_on_first_poll")
    {
        let live_id = "ask:%12:live-call";
        let missing_id = "ask:%14:missing-call";
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[tool_result("live-call", "Red")],
        );
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let live = ask_card("%12", "live-call");
        let missing = ask_card("%14", "missing-call");
        db.insert_card(&live).unwrap();
        db.insert_card(&missing).unwrap();
        db.swipe(&future_later(&live.id)).unwrap();
        db.swipe(&future_later(&missing.id)).unwrap();

        let mut tracker = Tracker::from_db(&db).unwrap();
        let written = tracker.tick(&db).written;
        let live_swipe = current_swipe(&db, live_id);
        let missing_swipe = current_swipe(&db, missing_id);
        let reconciled = written == 2
            && live_swipe.reason.as_deref() == Some("answered in pane")
            && live_swipe.option == Some(0)
            && missing_swipe.reason.as_deref() == Some("pane exited")
            && db.queue(events::now_ms()).unwrap().is_empty();
        assert!(reconciled);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "startup_reconciles_live_waiting_and_missing_future_asks_on_first_poll",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn pane_resolution_completes_future_deferred_ask() {
    if let Some(root) = child_root("pane_resolution_completes_future_deferred_ask") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[tool_result("deferred-call", "Red")],
        );
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let ask = ask_card("%12", "deferred-call");
        db.insert_card(&ask).unwrap();
        db.swipe(&future_later(&ask.id)).unwrap();
        let mut tracker = Tracker::default();
        tracker.states.insert("%12".to_owned(), PaneState::Asking);

        let written = tracker.tick(&db).written;
        let swipe = current_swipe(&db, &ask.id);
        let deferred_ask_completed = written == 1
            && swipe.action == "done"
            && swipe.reason.as_deref() == Some("answered in pane")
            && swipe.option == Some(0);
        assert!(deferred_ask_completed);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "pane_resolution_completes_future_deferred_ask",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn startup_retires_queued_ask_when_active_call_changed() {
    if let Some(root) = child_root("startup_retires_queued_ask_when_active_call_changed") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[ask_call("new-call", &["Red", "Blue"])],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let old = ask_card("%12", "old-call");
        db.insert_card(&old).unwrap();
        db.swipe(&future_later(&old.id)).unwrap();
        let mut tracker = Tracker::from_db(&db).unwrap();

        let written = tracker.tick(&db).written;
        let old_retired = current_swipe(&db, &old.id).reason.as_deref() == Some("answered in pane");
        let active = ask_card("%12", "new-call");
        let active_queued = db
            .queue(events::now_ms())
            .unwrap()
            .iter()
            .any(|card| card.id == active.id);
        assert!(written == 2 && old_retired && active_queued);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "startup_retires_queued_ask_when_active_call_changed",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn steady_asking_replaces_call_id_and_reuses_unchanged_transcript_parse() {
    if let Some(root) =
        child_root("steady_asking_replaces_call_id_and_reuses_unchanged_transcript_parse")
    {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[ask_call("call-one", &["Red", "Blue"])],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let first_poll = tracker.tick(&db).written;
        let old = ask_card("%12", "call-one");
        let initial_queued = db
            .queue(events::now_ms())
            .unwrap()
            .iter()
            .any(|card| card.id == old.id);
        let unchanged_poll = tracker.tick(&db).written;
        let parses_unchanged_once = unchanged_poll == 0 && tracker.pending_ask_reads == 1;

        let mut text = fs::read_to_string(&transcript).unwrap();
        text.push_str(&ask_call("call-two", &["Green", "Blue"]));
        text.push('\n');
        fs::write(&transcript, text).unwrap();
        let replacement_written = tracker.tick(&db).written;
        let old_swipe = db.card(&old.id).unwrap().unwrap().1;
        let old_retired = old_swipe
            .as_ref()
            .is_some_and(|swipe| swipe.reason.as_deref() == Some("answered in pane"));
        let active = ask_card("%12", "call-two");
        let active_queued = db
            .queue(events::now_ms())
            .unwrap()
            .iter()
            .any(|card| card.id == active.id);
        let replaced_call = replacement_written == 2 && old_retired && active_queued;
        assert!(first_poll == 1 && initial_queued && parses_unchanged_once && replaced_call);
        assert_eq!(tracker.pending_ask_reads, 2);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "steady_asking_replaces_call_id_and_reuses_unchanged_transcript_parse",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn stale_ask_answer_rejects_replaced_call_before_tracker_tick() {
    if let Some(root) = child_root("stale_ask_answer_rejects_replaced_call_before_tracker_tick") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[ask_call("new-call", &["Red", "Blue"])],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let stale = ask_card("%12", "old-call");
        db.insert_card(&stale).unwrap();

        let result = answer(&db, &stale, &answer_req(0, None), &Launch::default(), None);
        let stale_card_is_untouched = db.card(&stale.id).unwrap().unwrap().1.is_none();
        let no_key_sent = !root.join("tmux-keys.log").exists();
        let rejected_stale_call =
            matches!(result, Err(error) if error == "ask card no longer matches active dialog");
        assert!(rejected_stale_call && stale_card_is_untouched && no_key_sent);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "stale_ask_answer_rejects_replaced_call_before_tracker_tick",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn multi_select_ask_is_rejected_without_tmux_input() {
    if let Some(root) = child_root("multi_select_ask_is_rejected_without_tmux_input") {
        let transcript = root.join("transcript.jsonl");
        let multi_call = serde_json::json!({
            "type": "message",
            "id": "assistant-multi-call",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "multi-call",
                    "name": "ask",
                    "arguments": {"questions": [{
                        "id": "choice",
                        "question": "Choose all that apply",
                        "multi": true,
                        "options": [{"label": "Red"}, {"label": "Blue"}]
                    }]}
                }]
            }
        })
        .to_string();
        write_transcript(&root, "/dev/pts/12", &transcript, &[multi_call]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let card = db.queue(events::now_ms()).unwrap().into_iter().next();
        let unanswerable = card.as_ref().is_some_and(|card| !card.answerable);
        let answer_result = card
            .as_ref()
            .map(|card| answer(&db, card, &answer_req(0, None), &Launch::default(), None));
        let rejected = answer_result.as_ref().is_some_and(|result| {
            result.as_ref().err().map(String::as_str) == Some("ask card is not answerable")
        });
        let no_key_sent = !root.join("tmux-keys.log").exists();
        assert!(written == 1 && unanswerable && rejected && no_key_sent);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "multi_select_ask_is_rejected_without_tmux_input",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

const HTML_RADIO_FORM: &str = "<form><fieldset id='finding-001'><legend>Rotate the deploy key</legend><p>Task: Decide whether to rotate it now.</p><label><input type='radio' name='finding-001' value='do' checked> Do as recommended</label><label><input type='radio' name='finding-001' value='investigate'> Investigate first</label></fieldset></form>";
const HTML_TWO_FIELD_FORM: &str = "<form><fieldset><legend>Rotate the deploy key</legend><label><input type='radio' name='finding-001' value='do' checked> Do as recommended</label><label><input type='radio' name='finding-001' value='investigate'> Investigate first</label></fieldset><fieldset><legend>Reason</legend><label>Reason <input type='text' name='reason' value=''></label></fieldset></form>";
const HTML_SELECT_FORM: &str = "<form><fieldset><legend>Pick a tier</legend><label>Tier <select name='tier'><option>Basic</option><option>Pro</option></select></label></fieldset></form>";
const HTML_CHECKBOX_FORM: &str = "<form><fieldset><legend>Pick features</legend><label><input type='checkbox' name='f' value='a'> A</label><label><input type='checkbox' name='f' value='b'> B</label></fieldset></form>";

fn html_ask_call(html: &str) -> String {
    json!({
        "type": "message",
        "id": "assistant-html-call",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "toolCall",
                "id": "html-call",
                "name": "ask",
                "arguments": {"html": html}
            }]
        }
    })
    .to_string()
}

fn write_html_screens(root: &Path, screens: &[(&str, &str)]) {
    let directory = root.join("tmux-screens");
    fs::create_dir_all(&directory).unwrap();
    for (name, screen) in screens {
        fs::write(directory.join(name), screen).unwrap();
    }
}

fn queue_html_ask(root: &Path, html: &str) -> (Db, usize, Card) {
    let transcript = root.join("transcript.jsonl");
    write_transcript(root, "/dev/pts/12", &transcript, &[html_ask_call(html)]);
    let db = Db::open(&root.join("triage.db")).unwrap();
    let mut tracker = Tracker::default();
    let written = tracker.tick(&db).written;
    let card = db.queue(events::now_ms()).unwrap().into_iter().next().unwrap();
    (db, written, card)
}

#[cfg(unix)]
#[test]
fn html_radio_other_types_custom_text_then_submits_once() {
    if let Some(root) = child_root("html_radio_other_types_custom_text_then_submits_once") {
        write_html_screens(&root, &[("0", include_str!("../../tests/fixtures/ask_dialog.txt"))]);
        let (db, written, card) = queue_html_ask(&root, HTML_RADIO_FORM);
        let request = AnswerReq {
            text: Some("Rotate it Tuesday".to_owned()),
            ..AnswerReq::default()
        };
        let result = answer(&db, &card, &request, &Launch::default(), None);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        let swipe = db.card(&card.id).unwrap().unwrap().1;
        let expected = concat!(
            "send-keys -t %12 Down\n",
            "send-keys -t %12 BTab\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 C-u\n",
            "send-keys -t %12 C-k\n",
            "send-keys -t %12 -l -- \x1b[200~Rotate it Tuesday\x1b[201~\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 Enter\n",
        );
        let typed = result.is_ok()
            && written == 1
            && keys == expected
            && swipe.as_ref().is_some_and(|swipe| {
                swipe.action == "done"
                    && swipe.option.is_none()
                    && swipe.reason.as_deref() == Some("typed")
                    && swipe.text.as_deref() == Some("Rotate it Tuesday")
            });
        assert!(typed, "result: {result:?}\nswipe: {swipe:?}\nkeys:\n{keys}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "html_radio_other_types_custom_text_then_submits_once",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn html_two_fields_answer_in_form_order_then_submit_once() {
    if let Some(root) = child_root("html_two_fields_answer_in_form_order_then_submit_once") {
        write_html_screens(&root, &[("0", include_str!("../../tests/fixtures/ask_dialog.txt"))]);
        let (db, written, card) = queue_html_ask(&root, HTML_TWO_FIELD_FORM);
        let request = AnswerReq {
            answers: vec![
                events::Answer {
                    option: Some(1),
                    ..events::Answer::default()
                },
                events::Answer {
                    text: Some("Old key, rotated Tuesday".to_owned()),
                    ..events::Answer::default()
                },
            ],
            ..AnswerReq::default()
        };
        let result = answer(&db, &card, &request, &Launch::default(), None);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        let swipe = db.card(&card.id).unwrap().unwrap().1;
        let expected = concat!(
            "send-keys -t %12 Down\n",
            "send-keys -t %12 BTab\n",
            "send-keys -t %12 BTab\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 C-u\n",
            "send-keys -t %12 C-k\n",
            "send-keys -t %12 -l -- \x1b[200~Old key, rotated Tuesday\x1b[201~\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 Enter\n",
        );
        let typed_both = result.is_ok()
            && written == 1
            && keys == expected
            && swipe.as_ref().is_some_and(|swipe| {
                swipe.action == "done"
                    && swipe.option.is_none()
                    && swipe.reason.as_deref() == Some("typed")
                    && swipe.text.as_deref()
                        == Some("Rotate the deploy key: Investigate first; Reason: Old key, rotated Tuesday")
            });
        assert!(typed_both, "result: {result:?}\nswipe: {swipe:?}\nkeys:\n{keys}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "html_two_fields_answer_in_form_order_then_submit_once",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn html_select_rejects_custom_text_before_any_key() {
    if let Some(root) = child_root("html_select_rejects_custom_text_before_any_key") {
        let (db, written, card) = queue_html_ask(&root, HTML_SELECT_FORM);
        let request = AnswerReq {
            text: Some("Enterprise".to_owned()),
            ..AnswerReq::default()
        };
        let result = answer(&db, &card, &request, &Launch::default(), None);
        let rejected = result.as_ref().err().map(String::as_str) == Some("select does not accept Other");
        let no_key_sent = !root.join("tmux-keys.log").exists();
        let unanswered = db.card(&card.id).unwrap().unwrap().1.is_none();
        assert!(written == 1 && rejected && no_key_sent && unanswered, "result: {result:?}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "html_select_rejects_custom_text_before_any_key",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn html_checkbox_group_is_unanswerable_without_tmux_input() {
    if let Some(root) = child_root("html_checkbox_group_is_unanswerable_without_tmux_input") {
        let (db, written, card) = queue_html_ask(&root, HTML_CHECKBOX_FORM);
        let unanswerable = !card.answerable;
        let result = answer(&db, &card, &answer_req(0, None), &Launch::default(), None);
        let rejected = result.as_ref().err().map(String::as_str) == Some("ask card is not answerable");
        let no_key_sent = !root.join("tmux-keys.log").exists();
        assert!(written == 1 && unanswerable && rejected && no_key_sent, "result: {result:?}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "html_checkbox_group_is_unanswerable_without_tmux_input",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn html_ask_stays_queued_when_native_dialog_stays_open() {
    if let Some(root) = child_root("html_ask_stays_queued_when_native_dialog_stays_open") {
        let dialog = include_str!("../../tests/fixtures/ask_dialog.txt");
        write_html_screens(&root, &[("0", dialog), ("5", dialog)]);
        let (db, _, card) = queue_html_ask(&root, HTML_RADIO_FORM);
        let result = answer(&db, &card, &answer_req(1, None), &Launch::default(), None);
        let swipe = db.card(&card.id).unwrap().unwrap().1;
        let kept_queued = result.as_ref().err().map(String::as_str) == Some("dialog still open")
            && swipe.is_none();
        assert!(kept_queued, "result: {result:?}\nswipe: {swipe:?}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "html_ask_stays_queued_when_native_dialog_stays_open",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

fn landing_ask_call() -> String {
    serde_json::json!({
            "type": "message",
            "id": "assistant-landing-call",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "landing-call",
                    "name": "ask",
                    "arguments": {"questions": [
                        {"id": "f1", "question": "F1 — which tone should the hero headline use?",
                         "options": [{"label": "Formal"}, {"label": "Playful"}], "recommended": 0},
                        {"id": "f2", "question": "F2 — which billing period shows first?",
                         "options": [{"label": "Monthly"}, {"label": "Yearly"}], "recommended": 0},
                        {"id": "color", "question": "Which color should the button be?",
                         "options": [{"label": "Red"}, {"label": "Green"}, {"label": "Blue"}], "recommended": 1}
                    ]}
                }]
            }
        })
        .to_string()
}

fn write_landing_screens(root: &Path, review: &str) {
    let screens = root.join("tmux-screens");
    fs::create_dir_all(&screens).unwrap();
    let q1 = include_str!("../../tests/fixtures/ask_dialog_multi.txt");
    for (name, screen) in [
        ("0", q1),
        ("1", q1),
        (
            "2",
            include_str!("../../tests/fixtures/ask_dialog_multi_review_empty.txt"),
        ),
        ("3", q1),
        (
            "4",
            include_str!("../../tests/fixtures/ask_dialog_multi_q2.txt"),
        ),
        (
            "6",
            include_str!("../../tests/fixtures/ask_dialog_multi_q3.txt"),
        ),
        ("11", review),
    ] {
        fs::write(screens.join(name), screen).unwrap();
    }
}

fn landing_answers() -> Vec<events::Answer> {
    vec![
        events::Answer {
            option: Some(0),
            ..events::Answer::default()
        },
        events::Answer {
            option: Some(1),
            ..events::Answer::default()
        },
        events::Answer {
            text: Some("Violet  with   spaces".to_owned()),
            ..events::Answer::default()
        },
    ]
}

#[cfg(unix)]
#[test]
fn multi_question_ask_types_each_answer_and_submits_after_review() {
    if let Some(root) = child_root("multi_question_ask_types_each_answer_and_submits_after_review")
    {
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[landing_ask_call()]);
        write_landing_screens(
            &root,
            include_str!("../../tests/fixtures/ask_dialog_multi_review.txt"),
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let card = db
            .queue(events::now_ms())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let request = AnswerReq {
            answers: landing_answers(),
            ..AnswerReq::default()
        };
        let result = answer(&db, &card, &request, &Launch::default(), None);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        let swipe = db.card(&card.id).unwrap().unwrap().1;
        let expected = concat!(
            "send-keys -t %12 Up\n",
            "send-keys -t %12 BTab\n",
            "send-keys -t %12 Tab\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 -l -- Violet  with   spaces\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 Enter\n",
        );
        let typed_all_answers = result.is_ok()
            && written == 1
            && keys == expected
            && swipe.as_ref().is_some_and(|swipe| {
                swipe.action == "done"
                    && swipe.option.is_none()
                    && swipe.reason.as_deref() == Some("typed")
                    && swipe.text.as_deref()
                        == Some("f1: Formal; f2: Yearly; color: Violet  with   spaces")
            });
        assert!(
            typed_all_answers,
            "result: {result:?}\nswipe: {swipe:?}\nkeys:\n{keys}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "multi_question_ask_types_each_answer_and_submits_after_review",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn multi_question_ask_leaves_dialog_open_when_review_differs() {
    if let Some(root) = child_root("multi_question_ask_leaves_dialog_open_when_review_differs") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[landing_ask_call()]);
        let review = include_str!("../../tests/fixtures/ask_dialog_multi_review.txt")
            .replace("f2: Yearly", "f2: Monthly");
        write_landing_screens(&root, &review);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        let card = db
            .queue(events::now_ms())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let request = AnswerReq {
            answers: landing_answers(),
            ..AnswerReq::default()
        };
        let result = answer(&db, &card, &request, &Launch::default(), None);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap();
        let no_swipe = db.card(&card.id).unwrap().unwrap().1.is_none();
        let expected = concat!(
            "send-keys -t %12 Up\n",
            "send-keys -t %12 BTab\n",
            "send-keys -t %12 Tab\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 Down\n",
            "send-keys -t %12 Enter\n",
            "send-keys -t %12 -l -- Violet  with   spaces\n",
            "send-keys -t %12 Enter\n",
        );
        let rejected = result.as_ref().err().map(String::as_str)
            == Some("ask review does not match the answers");
        assert!(rejected && keys == expected && no_swipe, "keys:\n{keys}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "multi_question_ask_leaves_dialog_open_when_review_differs",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn first_seen_waiting_pane_still_reports_final_status() {
    if let Some(root) = child_root("first_seen_waiting_pane_still_reports_final_status") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let report = serde_json::json!({
            "type": "message",
            "id": "assistant-report",
            "message": {
                "role": "assistant",
                "stopReason": "stop",
                "content": [{"type": "text", "text": "ENVOY_STATUS: complete\n\nFound the cause."}]
            }
        })
        .to_string();
        write_transcript(&root, "/dev/pts/12", &transcript, &[report]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let first = tracker.tick(&db).written;
        let queued = db.queue(events::now_ms()).unwrap();
        let card = queued
            .iter()
            .find(|card| card.id == "done:%12:assistant-report");
        let second = tracker.tick(&db).written;
        let still_one = db
            .queue(events::now_ms())
            .unwrap()
            .iter()
            .filter(|card| card.id == "done:%12:assistant-report")
            .count();
        assert!(
            first == 1
                && card.is_some()
                && second == 0
                && still_one == 1
                && card.is_some_and(|card| card.title == "Done: Found the cause.")
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "first_seen_waiting_pane_still_reports_final_status",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

// Regression: until 2026-11-02 only peek text and plan bodies went through a redactor, so a
// reply that quoted a credential reached the inbox as written. Cards keep paths and hashes
// readable, so only the unambiguous shapes are masked.
#[cfg(unix)]
#[test]
fn reply_card_redacts_secrets_in_title_and_body() {
    if let Some(root) = child_root("reply_card_redacts_secrets_in_title_and_body") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        // Built from parts so the source holds nothing shaped like a credential.
        let access_key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        let token = format!("eyJ{}", "x".repeat(40));
        let hash = "0123456789abcdef".repeat(3);
        let path = "/home/example/Documents/projects/demo/worklog.md";
        let text = format!(
            "ENVOY_STATUS: complete\n\nRotated {access_key}.\n\nThe old header was Bearer {token}, the hash was {hash} and the log is {path}."
        );
        let report = serde_json::json!({
            "type": "message",
            "id": "assistant-report",
            "message": {
                "role": "assistant",
                "stopReason": "stop",
                "content": [{"type": "text", "text": text}]
            }
        })
        .to_string();
        write_transcript(&root, "/dev/pts/12", &transcript, &[report]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        let queued = db.queue(events::now_ms()).unwrap();
        let card = queued
            .iter()
            .find(|card| card.id == "done:%12:assistant-report")
            .expect("reply card");
        let body = card.body.as_deref().unwrap_or_default();
        assert_eq!(card.title, "Done: Rotated ….");
        assert!(!body.contains(&access_key) && !body.contains(&token), "{body}");
        assert!(body.contains("Bearer …,"), "{body}");
        assert!(body.contains(&hash) && body.contains(path), "{body}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "reply_card_redacts_secrets_in_title_and_body",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

// Regression: on 2026-11-01 the report card of a dispatched investigation
// named neither the finding the user had answered nor the answer.
#[cfg(unix)]
#[test]
fn dispatched_report_card_names_the_card_and_answer_that_launched_it() {
    if let Some(root) =
        child_root("dispatched_report_card_names_the_card_and_answer_that_launched_it")
    {
        let (_pty, tty) = new_pty();
        fs::write(root.join("tmux-tty"), format!("{tty}\n")).unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            &tty,
            &transcript,
            &[assistant_text_line(
                "assistant-report",
                "ENVOY_STATUS: complete\n\nGrant is excess.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut finding = finding_card(&root);
        finding.title = "The delivery team has an unexpected CI grant".to_owned();
        finding.options[0].label = "Investigate first".to_owned();
        db.insert_card(&finding).unwrap();
        db.swipe(&Swipe {
            card: finding.id.clone(),
            action: "done".to_owned(),
            option: Some(0),
            text: None,
            note: Some("so what should i do?".to_owned()),
            until_ms: None,
            reason: Some("launched %12".to_owned()),
            at_ms: events::now_ms(),
        })
        .unwrap();
        Tracker::default().tick(&db);
        let card = db
            .queue(events::now_ms())
            .unwrap()
            .into_iter()
            .find(|card| card.id == "done:%12:assistant-report");
        assert!(card.is_some_and(|card| {
            card.star.situation == "The delivery team has an unexpected CI grant"
                && card.star.task == "Investigate first · Note: so what should i do?"
        }));
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dispatched_report_card_names_the_card_and_answer_that_launched_it",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn report_trail_follows_this_panes_latest_launch_and_keeps_the_first_topic() {
    let scratch = Scratch::new();
    let db = Db::open(&scratch.path().join("triage.db")).unwrap();
    let (_pty, tty) = new_pty();
    let live = pane(scratch.path(), "%12", &tty, "π > session");
    let born = live.pty_born_ms().unwrap();
    let launch = |card: &str,
                  reason: &str,
                  at_ms: i64,
                  option: Option<usize>,
                  text: Option<&str>,
                  note: Option<&str>| {
        db.swipe(&Swipe {
            card: card.to_owned(),
            action: "done".to_owned(),
            option,
            text: text.map(str::to_owned),
            note: note.map(str::to_owned),
            until_ms: None,
            reason: Some(reason.to_owned()),
            at_ms,
        })
        .unwrap();
    };
    let trail = || {
        let (star, _) = pane_trail(&db, &live);
        (star.situation, star.task)
    };
    let investigation = || pane_trail(&db, &live).1;

    let mut earlier = finding_card(scratch.path());
    earlier.title = "An earlier tmux server's card".to_owned();
    db.insert_card(&earlier).unwrap();
    launch(
        &earlier.id,
        "launched %12",
        born - 60_000,
        Some(0),
        None,
        None,
    );
    assert_eq!(trail(), (String::new(), String::new()));

    let mut finding = finding_card(scratch.path());
    finding.title = "The delivery team has an unexpected CI grant".to_owned();
    db.insert_card(&finding).unwrap();
    launch(
        &finding.id,
        "launched %12",
        born + 1,
        Some(1),
        None,
        Some("so what should i do?"),
    );
    launch(&earlier.id, "launched %120", born + 2, Some(0), None, None);
    let topic = finding.title.clone();
    assert_eq!(
        trail(),
        (
            topic.clone(),
            "Defer · Note: so what should i do?".to_owned()
        )
    );

    let mut report = info_card("done:%12:r1", "%12");
    report.star = pane_trail(&db, &live).0;
    db.insert_card(&report).unwrap();
    launch(
        &report.id,
        "launched %12",
        born + 3,
        None,
        Some("Investigate"),
        None,
    );
    assert_eq!(trail(), (topic.clone(), "Investigate".to_owned()));
    assert!(
        investigation(),
        "an Investigate dispatch leaves its step awaiting approval"
    );

    let ask = ask_card("%12", "ask-call");
    db.insert_card(&ask).unwrap();
    launch(&ask.id, "acting %12", born + 4, None, Some("Act"), None);
    assert_eq!(trail(), (ask.title.clone(), "Act".to_owned()));
    assert!(!investigation(), "an Act launch owes no next step");
}

// On 2026-11-01 the %46 card reported an access-audit investigation whose answer ended
// "Proposed action not carried out", and the card offered only Acknowledge, so the
// user could not approve the next step from the inbox. The report's
// `Recommended action:` line must now move onto the card as its action.
#[cfg(unix)]
#[test]
fn investigation_report_card_carries_its_recommended_action() {
    if let Some(root) = child_root("investigation_report_card_carries_its_recommended_action") {
        let (_pty, tty) = new_pty();
        fs::write(root.join("tmux-tty"), format!("{tty}\n")).unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        // The user's draft sits in omp's editor, so triage leaves the pane alone and the
        // step stays on the card.
        fs::create_dir_all(root.join("tmux-screens")).unwrap();
        fs::write(root.join("tmux-screens/0"), "──────\n  hold on  \n──────\n").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            &tty,
            &transcript,
            &[assistant_text_line(
                "assistant-report",
                "ENVOY_STATUS: complete\n\nBoth users lack the grant.\n\n**Recommended action:** Add both as read collaborators.\n\nTime used: 9 seconds.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let finding = finding_card(&root);
        db.insert_card(&finding).unwrap();
        db.swipe(&Swipe {
            card: finding.id.clone(),
            action: "done".to_owned(),
            option: None,
            text: Some("Investigate".to_owned()),
            note: None,
            until_ms: None,
            reason: Some("launched %12".to_owned()),
            at_ms: events::now_ms(),
        })
        .unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let card = db
            .queue(events::now_ms())
            .unwrap()
            .into_iter()
            .find(|card| card.id == "done:%12:assistant-report");
        assert!(
            written == 1
                && card.is_some_and(|card| {
                    card.star.action == "Add both as read collaborators."
                        && card.body.as_deref()
                            == Some("Both users lack the grant.\n\nTime used: 9 seconds.")
                })
        );
        assert!(!root.join("tmux-keys.log").exists(), "the pane got keys");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "investigation_report_card_carries_its_recommended_action",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

// On 2026-11-08 the %304 investigation's step sat on its card and the user typed "do it" in
// the pane to move it, which dropped the brief's status contract. A final report whose pane
// still waits with an empty editor must get its step back there as a plan brief.
#[cfg(unix)]
#[test]
fn investigation_step_becomes_a_plan_in_its_waiting_pane() {
    if let Some(root) = child_root("investigation_step_becomes_a_plan_in_its_waiting_pane") {
        let (_pty, tty) = new_pty();
        fs::write(root.join("tmux-tty"), format!("{tty}\n")).unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-idle-screen"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            &tty,
            &transcript,
            &[assistant_text_line(
                "assistant-report",
                "ENVOY_STATUS: complete\n\nBoth users lack the grant.\n\n**Recommended action:** Add both as read collaborators.\n\nTime used: 9 seconds.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let finding = finding_card(&root);
        db.insert_card(&finding).unwrap();
        db.swipe(&Swipe {
            card: finding.id.clone(),
            action: "done".to_owned(),
            option: None,
            text: Some("Investigate".to_owned()),
            note: None,
            until_ms: None,
            reason: Some("launched %12".to_owned()),
            at_ms: events::now_ms(),
        })
        .unwrap();
        Tracker::default().tick(&db);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        let typed: Vec<&str> = keys
            .lines()
            .filter_map(|line| line.split_once("-l -- Read the brief at ").map(|(_, rest)| rest))
            .collect();
        assert_eq!(keys.lines().filter(|line| line.contains(" M-P")).count(), 1, "{keys}");
        assert_eq!(typed.len(), 1, "{keys}");
        let report = current_swipe(&db, "done:%12:assistant-report");
        assert_eq!(
            (report.text.as_deref(), report.reason.as_deref()),
            (Some("Plan"), Some("launched %12"))
        );
        let brief =
            fs::read_to_string(typed[0].strip_suffix(" and carry it out.").unwrap()).unwrap();
        assert!(brief.starts_with("# Plan: Done: Both users lack the grant.\n"), "{brief}");
        assert!(brief.contains("## Action\nAdd both as read collaborators.\n"), "{brief}");
        assert!(brief.contains(PLAN_INSTRUCTION), "{brief}");
        assert!(brief.ends_with(&format!("\n{PLAN_REPORT_STEP}\n")), "{brief}");
        assert_eq!(new_window_count(&root), 0);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "investigation_step_becomes_a_plan_in_its_waiting_pane",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

// omp refuses plan mode while a goal is active or paused, and in vibe mode. The step must
// then stay on its card for Plan recommended step, with no brief typed and no window opened.
#[cfg(unix)]
#[test]
fn investigation_step_stays_on_its_card_when_plan_mode_refuses() {
    if let Some(root) = child_root("investigation_step_stays_on_its_card_when_plan_mode_refuses") {
        let (_pty, tty) = new_pty();
        fs::write(root.join("tmux-tty"), format!("{tty}\n")).unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-idle-screen"), b"").unwrap();
        fs::write(root.join("tmux-plan-toggle-fails"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            &tty,
            &transcript,
            &[assistant_text_line(
                "assistant-report",
                "ENVOY_STATUS: complete\n\nBoth users lack the grant.\n\n**Recommended action:** Add both as read collaborators.\n\nTime used: 9 seconds.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let finding = finding_card(&root);
        db.insert_card(&finding).unwrap();
        db.swipe(&Swipe {
            card: finding.id.clone(),
            action: "done".to_owned(),
            option: None,
            text: Some("Investigate".to_owned()),
            note: None,
            until_ms: None,
            reason: Some("launched %12".to_owned()),
            at_ms: events::now_ms(),
        })
        .unwrap();
        Tracker::default().tick(&db);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        assert_eq!(keys.lines().filter(|line| line.contains(" M-P")).count(), 1, "{keys}");
        assert!(!keys.contains("-l --"), "{keys}");
        let (card, swipe) = db.card("done:%12:assistant-report").unwrap().unwrap();
        assert!(swipe.is_none());
        assert_eq!(card.star.action, "Add both as read collaborators.");
        assert_eq!(new_window_count(&root), 0);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "investigation_step_stays_on_its_card_when_plan_mode_refuses",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn act_complete_report_keeps_its_recommended_line_in_the_body() {
    if let Some(root) = child_root("act_complete_report_keeps_its_recommended_line_in_the_body") {
        let (_pty, tty) = new_pty();
        assert_eq!(
            reply_turn(
                "ENVOY_STATUS: complete\n\nShipped.\n\nRecommended action: Run the investigation follow-up.".to_owned(),
                None,
                true,
                true,
                Vec::new(),
            )
            .action,
            "Run the investigation follow-up."
        );
        fs::write(root.join("tmux-tty"), format!("{tty}\n")).unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::create_dir_all(root.join("tmux-screens")).unwrap();
        fs::write(root.join("tmux-screens/0"), "──────\n  hold on  \n──────\n").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            &tty,
            &transcript,
            &[assistant_text_line(
                "assistant-report",
                "ENVOY_STATUS: complete\n\nSkill updated.\n\nRecommended action: next cold-email task, read the field data first.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let finding = finding_card(&root);
        db.insert_card(&finding).unwrap();
        db.swipe(&Swipe {
            card: finding.id.clone(),
            action: "done".to_owned(),
            option: None,
            text: Some("Act".to_owned()),
            note: None,
            until_ms: None,
            reason: Some("launched %12".to_owned()),
            at_ms: events::now_ms(),
        })
        .unwrap();
        let written = Tracker::default().tick(&db).written;
        let card = db
            .queue(events::now_ms())
            .unwrap()
            .into_iter()
            .find(|card| card.id == "done:%12:assistant-report");
        assert!(
            written == 1
                && card.is_some_and(|card| {
                    card.star.action.is_empty()
                        && card.body.as_deref()
                            == Some("Skill updated.\n\nRecommended action: next cold-email task, read the field data first.")
                })
        );
        assert!(!root.join("tmux-keys.log").exists(), "the pane got keys");
        let none = reply_turn(
            "ENVOY_STATUS: complete\n\nShipped.\n\nRecommended action: none.".to_owned(),
            None,
            true,
            false,
            Vec::new(),
        );
        assert_eq!(none.body.as_deref(), Some("Shipped."));
        assert!(none.action.is_empty());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "act_complete_report_keeps_its_recommended_line_in_the_body",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}
#[cfg(unix)]
#[test]
fn agent_done_report_names_the_open_step() {
    if let Some(root) = child_root("agent_done_report_names_the_open_step") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let report = serde_json::json!({
                "type": "message",
                "id": "assistant-report",
                "message": {
                    "role": "assistant",
                    "stopReason": "stop",
                    "content": [{"type": "text", "text": "ENVOY_STATUS: agent-done\n\nPush still open, suite red.\n\nRecommended action: Push the release and rerun the suite."}]
                }
            })
            .to_string();
        write_transcript(&root, "/dev/pts/12", &transcript, &[report]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let queue = db.queue(events::now_ms()).unwrap();
        let card = queue
            .iter()
            .find(|card| card.id == "done:%12:assistant-report");
        assert!(
            written == 1
                && card.is_some_and(|card| {
                    card.title == "Agent done: Push still open, suite red."
                        && card.level.as_deref() == Some("warn")
                        && card.star.action == "Push the release and rerun the suite."
                        && card.body.as_deref() == Some("Push still open, suite red.")
                }),
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "agent_done_report_names_the_open_step",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn agent_done_report_that_closes_with_none_earns_a_done_card() {
    if let Some(root) = child_root("agent_done_report_that_closes_with_none_earns_a_done_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let report = serde_json::json!({
                "type": "message",
                "id": "assistant-report",
                "message": {
                    "role": "assistant",
                    "stopReason": "stop",
                    "content": [{"type": "text", "text": "ENVOY_STATUS: agent-done\n\nBlock key shipped and pushed.\n\nRecommended action: none."}]
                }
            })
            .to_string();
        write_transcript(&root, "/dev/pts/12", &transcript, &[report]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let queue = db.queue(events::now_ms()).unwrap();
        let card = queue
            .iter()
            .find(|card| card.id == "done:%12:assistant-report");
        assert!(
            written == 1
                && card.is_some_and(|card| {
                    card.title == "Done: Block key shipped and pushed."
                        && card.level.as_deref() == Some("info")
                        && card.star.action.is_empty()
                        && card.body.as_deref() == Some("Block key shipped and pushed.")
                }),
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "agent_done_report_that_closes_with_none_earns_a_done_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[test]
fn agent_done_report_closed_by_none_reads_done() {
    let turn = reply_turn(
        "ENVOY_STATUS: agent-done\n\nBlock key shipped and pushed.\n\nRecommended action: none.".to_owned(),
        None,
        true,
        false,
        Vec::new(),
    );
    assert_eq!(turn.title, "Done: Block key shipped and pushed.");
    assert_eq!(turn.level, "info");
    assert!(turn.action.is_empty(), "action: {}", turn.action);
    assert_eq!(turn.body.as_deref(), Some("Block key shipped and pushed."));
}

#[test]
fn agent_done_report_with_an_open_step_keeps_the_warning() {
    let turn = reply_turn(
        "ENVOY_STATUS: agent-done\n\nPush still open.\n\nRecommended action: Push the release.".to_owned(),
        None,
        true,
        false,
        Vec::new(),
    );
    assert_eq!(turn.title, "Agent done: Push still open.");
    assert_eq!(turn.level, "warn");
    assert_eq!(turn.action, "Push the release.");
}

#[test]
fn agent_done_report_without_a_recommended_action_keeps_the_warning() {
    let turn = reply_turn(
        "ENVOY_STATUS: agent-done\n\nSuite gate still red.".to_owned(),
        None,
        true,
        false,
        Vec::new(),
    );
    assert_eq!(turn.title, "Agent done: Suite gate still red.");
    assert_eq!(turn.level, "warn");
}

#[test]
fn carried_agent_done_report_takes_the_closing_step() {
    let turn = reply_turn(
        "Closing note.\n\nRecommended action: Publish the release.".to_owned(),
        Some("ENVOY_STATUS: agent-done\n\nWaited on the suite.\n\nRecommended action: none.".to_owned()),
        true,
        false,
        Vec::new(),
    );
    assert_eq!(turn.title, "Agent done: Waited on the suite.");
    assert_eq!(turn.level, "warn");
    assert_eq!(turn.action, "Publish the release.");
}

#[cfg(unix)]
#[test]
fn launched_reply_without_status_line_offers_report_request() {
    if let Some(root) = child_root("launched_reply_without_status_line_offers_report_request") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-keys.log"), b" ").unwrap();
        fs::create_dir_all(root.join("tmux-screens")).unwrap();
        fs::write(root.join("tmux-screens/0"), "──────\n  ←←  \n──────\n").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[assistant_text_line("assistant-report", "All done. Listing live.")]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut finding = finding_card(&root);
        finding.title = "The delivery team has an unexpected CI grant".to_owned();
        db.insert_card(&finding).unwrap();
        db.swipe(&Swipe { card: finding.id.clone(), action: "done".to_owned(), option: Some(0), text: None, note: None, until_ms: None, reason: Some("launched %12".to_owned()), at_ms: events::now_ms() }).unwrap();
        Tracker::default().tick(&db);
        let card = db.card("done:%12:assistant-report").unwrap().unwrap().0;
        assert!(card.report_due && card.star.situation == finding.title, "card: {card:?}");
        assert_eq!(request_report(&db, &card, &Launch::default()), Ok(()));
        let swipe = db.card(&card.id).unwrap().unwrap().1.unwrap();
        assert_eq!(swipe.text.as_deref(), Some("Request final report"));
        assert_eq!(swipe.reason.as_deref(), Some("report requested %12"));
        assert!(fs::read_to_string(root.join("tmux-keys.log")).unwrap().contains(REQUEST_REPORT_TEXT));
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("launched_reply_without_status_line_offers_report_request", scratch.path(), Some(scratch.path()), Some(&bin));
}
#[cfg(unix)]
#[test]
fn unlaunched_reply_does_not_offer_report_request() {
    if let Some(root) = child_root("unlaunched_reply_does_not_offer_report_request") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[assistant_text_line("assistant-report", "A plain reply.")]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        Tracker::default().tick(&db);
        let card = db.card("done:%12:assistant-report").unwrap().unwrap().0;
        assert!(!card.report_due);
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("unlaunched_reply_does_not_offer_report_request", scratch.path(), Some(scratch.path()), Some(&bin));
}
#[cfg(unix)]
#[test]
fn error_reply_does_not_offer_report_request() {
    if let Some(root) = child_root("error_reply_does_not_offer_report_request") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[assistant_entry("assistant-error", "error", None, Some("provider failed"))]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let finding = finding_card(&root);
        db.insert_card(&finding).unwrap();
        db.swipe(&Swipe { card: finding.id.clone(), action: "done".to_owned(), option: Some(0), text: None, note: None, until_ms: None, reason: Some("launched %12".to_owned()), at_ms: events::now_ms() }).unwrap();
        Tracker::default().tick(&db);
        assert!(!db.card("done:%12:assistant-error").unwrap().unwrap().0.report_due);
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("error_reply_does_not_offer_report_request", scratch.path(), Some(scratch.path()), Some(&bin));
}

#[cfg(unix)]
#[test]
fn waiting_pane_without_status_line_queues_an_info_card() {
    if let Some(root) = child_root("waiting_pane_without_status_line_queues_an_info_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let reply = serde_json::json!({
            "type": "message",
            "id": "assistant-report",
            "message": {
                "role": "assistant",
                "stopReason": "stop",
                "content": [{"type": "text", "text": "All done. Listing live.\n\nDetails."}]
            }
        })
        .to_string();
        write_transcript(&root, "/dev/pts/12", &transcript, &[reply]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let first = tracker.tick(&db).written;
        let queue = db.queue(events::now_ms()).unwrap();
        let card = queue
            .iter()
            .find(|card| card.id == "done:%12:assistant-report");
        let second = tracker.tick(&db).written;
        let still_one = db
            .queue(events::now_ms())
            .unwrap()
            .iter()
            .filter(|card| card.id == "done:%12:assistant-report")
            .count();
        assert!(
            first == 1
                && second == 0
                && still_one == 1
                && card.is_some_and(|card| {
                    card.title == "All done. Listing live."
                        && card.body.as_deref() == Some("All done. Listing live.\n\nDetails.")
                        && card.level.as_deref() == Some("info")
                        && card
                            .options
                            .iter()
                            .any(|option| option.label == "Acknowledge")
                }),
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "waiting_pane_without_status_line_queues_an_info_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn omp_reply_card_carries_its_turn_images() {
    if let Some(root) = child_root("omp_reply_card_carries_its_turn_images") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let hash = "a".repeat(64);
        let old = "b".repeat(64);
        let user = serde_json::json!({
            "type": "message",
            "id": "u1",
            "message": {"role": "user", "content": [{"type": "text", "text": "look"}]}
        })
        .to_string();
        let image = |hash: &str, path: &str| {
            serde_json::json!({
                "type": "message",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "read-1",
                    "content": [
                        {"type": "text", "text": "Read image file [image/png]"},
                        {"type": "image", "data": format!("blob:sha256:{hash}"), "mimeType": "image/png"}
                    ],
                    "details": {"meta": {"source": {"type": "path", "value": path}}}
                }
            })
            .to_string()
        };
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user.clone(),
                image(&old, "/tmp/old.png"),
                user,
                image(&hash, "/tmp/shots/new.png"),
                assistant_entry(
                    "assistant-report",
                    "stop",
                    Some("ENVOY_STATUS: complete\n\nShipped."),
                    None,
                ),
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        assert_eq!(Tracker::default().tick(&db).written, 1);
        let (card, _) = db.card("done:%12:assistant-report").unwrap().unwrap();
        assert_eq!(card.images.len(), 1, "{card:?}");
        assert_eq!(card.images[0].id, hash);
        assert_eq!(card.images[0].media_type, "image/png");
        assert_eq!(card.images[0].name.as_deref(), Some("new.png"));
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "omp_reply_card_carries_its_turn_images",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

fn assistant_entry(id: &str, stop: &str, text: Option<&str>, error: Option<&str>) -> String {
    let content = text
        .map(|text| vec![serde_json::json!({"type": "text", "text": text})])
        .unwrap_or_default();
    let mut message =
        serde_json::json!({"role": "assistant", "stopReason": stop, "content": content});
    if let Some(error) = error {
        message["errorMessage"] = error.into();
    }
    serde_json::json!({"type": "message", "id": id, "message": message}).to_string()
}

fn user_line(id: &str, text: &str) -> String {
    serde_json::json!({
        "type": "message",
        "id": id,
        "message": {"role": "user", "content": [{"type": "text", "text": text}]}
    })
    .to_string()
}

fn swipe_reason(db: &Db, id: &str) -> Option<String> {
    db.card(id)
        .unwrap()
        .and_then(|(_, swipe)| swipe)
        .and_then(|swipe| swipe.reason)
}

#[cfg(unix)]
#[test]
fn vanished_pane_closes_its_info_card() {
    if let Some(root) = child_root("vanished_pane_closes_its_info_card") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_line("u1", "Ship it."),
                assistant_entry(
                    "a1",
                    "stop",
                    Some("ENVOY_STATUS: complete\n\nShipped."),
                    None,
                ),
            ],
        );
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-second-pane"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        fs::write(root.join("tmux-pane-12-gone"), b"").unwrap();
        tracker.tick(&db);
        let queue = db.queue(events::now_ms()).unwrap();
        let reason = swipe_reason(&db, "done:%12:a1");
        assert!(
            queue.is_empty() && reason.as_deref() == Some("pane exited"),
            "queue: {queue:?} reason: {reason:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "vanished_pane_closes_its_info_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn tmux_server_gone_keeps_a_lone_pane_info_card() {
    if let Some(root) = child_root("tmux_server_gone_keeps_a_lone_pane_info_card") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_line("u1", "Ship it."),
                assistant_entry(
                    "a1",
                    "stop",
                    Some("ENVOY_STATUS: complete\n\nShipped."),
                    None,
                ),
            ],
        );
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        fs::write(root.join("tmux-missing-pane"), b"").unwrap();
        tracker.tick(&db);
        let queue = done_cards(&db);
        let swipe = db.card("done:%12:a1").unwrap().and_then(|(_, swipe)| swipe);
        assert!(
            queue
                .iter()
                .map(|card| card.id.as_str())
                .eq(["done:%12:a1"])
                && swipe.is_none(),
            "queue: {queue:?} swipe: {swipe:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "tmux_server_gone_keeps_a_lone_pane_info_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn mass_vanish_keeps_info_cards() {
    if let Some(root) = child_root("mass_vanish_keeps_info_cards") {
        write_transcript(
            &root,
            "/dev/pts/12",
            &root.join("transcript.jsonl"),
            &[assistant_text_line("a1", "Pane twelve is done.")],
        );
        write_transcript(
            &root,
            "/dev/pts/13",
            &root.join("transcript-13.jsonl"),
            &[assistant_text_line("a2", "Pane thirteen is done.")],
        );
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-second-pane"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        fs::write(root.join("tmux-missing-pane"), b"").unwrap();
        tracker.tick(&db);
        let queue = db.queue(events::now_ms()).unwrap();
        let ids = queue.iter().map(|card| card.id.as_str()).collect::<Vec<_>>();
        let swipes = [
            swipe_reason(&db, "done:%12:a1"),
            swipe_reason(&db, "done:%13:a2"),
        ];
        assert!(
            ids.len() == 2
                && ids.contains(&"done:%12:a1")
                && ids.contains(&"done:%13:a2")
                && swipes.iter().all(Option::is_none),
            "queue: {ids:?} swipes: {swipes:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "mass_vanish_keeps_info_cards",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn vanished_pane_queues_a_final_reply_it_never_saw_waiting() {
    if let Some(root) = child_root("vanished_pane_queues_a_final_reply_it_never_saw_waiting") {
        let transcript = root.join("transcript.jsonl");
        let prompt = user_line("u1", "Check the rows.");
        let narration = assistant_entry("t1", "toolUse", Some("Checking."), None);
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[prompt.clone(), narration.clone()],
        );
        fs::write(root.join("tmux-working"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                prompt,
                narration,
                assistant_entry("a2", "stop", Some("All done."), None),
            ],
        );
        fs::write(root.join("tmux-missing-pane"), b"").unwrap();
        tracker.tick(&db);
        let repeat = tracker.tick(&db).written;
        let queue = db.queue(events::now_ms()).unwrap();
        assert!(
            repeat == 0
                && queue.len() == 1
                && queue[0].id == "done:%12:a2"
                && queue[0].title == "All done."
                && queue[0].refs == [transcript.display().to_string()],
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "vanished_pane_queues_a_final_reply_it_never_saw_waiting",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn vanished_pane_that_died_mid_turn_raises_one_warning_card() {
    if let Some(root) = child_root("vanished_pane_that_died_mid_turn_raises_one_warning_card") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_line("u1", "Dump the rows."),
                assistant_entry("t1", "toolUse", Some("Dumping rows now.\n\nRecommended action: Restore the session."), None),
            ],
        );
        fs::write(root.join("tmux-working"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        fs::write(root.join("tmux-missing-pane"), b"").unwrap();
        tracker.tick(&db);
        let repeat = tracker.tick(&db).written;
        let queue = db.queue(events::now_ms()).unwrap();
        assert!(
            repeat == 0
                && queue.len() == 1
                && queue[0].id == "exited:%12:t1"
                && queue[0].title == "Ended mid-turn: Dumping rows now."
                && queue[0].level.as_deref() == Some("warn")
                && queue[0].body.as_deref() == Some("Dumping rows now.")
                && queue[0].star.action == "Restore the session."
                && queue[0].refs == [transcript.display().to_string()],
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "vanished_pane_that_died_mid_turn_raises_one_warning_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn vanished_dispatched_pane_without_report_replaces_its_reply_card() {
    if let Some(root) =
        child_root("vanished_dispatched_pane_without_report_replaces_its_reply_card")
    {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_brief_line(),
                assistant_text_line("a1", "Weekly audit is live, details follow."),
            ],
        );
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        fs::write(root.join("tmux-missing-pane"), b"").unwrap();
        tracker.tick(&db);
        let repeat = tracker.tick(&db).written;
        let queue = db.queue(events::now_ms()).unwrap();
        let replaced = swipe_reason(&db, "done:%12:a1");
        assert!(
            repeat == 0
                && queue.len() == 1
                && queue[0].id == "exited:%12:a1"
                && queue[0].title == "Unreported: Weekly audit is live, details follow."
                && queue[0].level.as_deref() == Some("warn")
                && replaced.as_deref() == Some("superseded"),
            "queue: {queue:?} replaced: {replaced:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "vanished_dispatched_pane_without_report_replaces_its_reply_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn pane_closed_from_its_card_is_not_audited() {
    if let Some(root) = child_root("pane_closed_from_its_card_is_not_audited") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_brief_line(),
                assistant_text_line("a1", "Weekly audit is live, details follow."),
            ],
        );
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        let (card, _) = db.card("done:%12:a1").unwrap().unwrap();
        let closed = close(&db, &card, None, &Launch::default());
        fs::write(root.join("tmux-missing-pane"), b"").unwrap();
        tracker.tick(&db);
        let queue = db.queue(events::now_ms()).unwrap();
        let exited = db.card("exited:%12:a1").unwrap();
        assert!(
            closed.is_ok() && queue.is_empty() && exited.is_none(),
            "closed: {closed:?} queue: {queue:?} exited: {exited:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "pane_closed_from_its_card_is_not_audited",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn info_cards_supersede_by_transcript_not_pane_id() {
    if let Some(root) = child_root("info_cards_supersede_by_transcript_not_pane_id") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_line("u1", "Go."),
                assistant_entry("a1", "stop", Some("Finished."), None),
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut other_session = info_card("done:%12:old", "%12");
        other_session.refs = vec!["/elsewhere/other.jsonl".to_owned()];
        let mut same_session = info_card("done:%7:a0", "%7");
        same_session.refs = vec![transcript.display().to_string()];
        db.insert_card(&other_session).unwrap();
        db.insert_card(&same_session).unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let mut tracker = Tracker::default();
        tracker.tick(&db);
        let queue = done_cards(&db);
        let mut ids = queue
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        let replaced = swipe_reason(&db, "done:%7:a0");
        assert!(
            ids == ["done:%12:a1", "done:%12:old"] && replaced.as_deref() == Some("superseded"),
            "queue: {ids:?} replaced: {replaced:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "info_cards_supersede_by_transcript_not_pane_id",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

fn done_cards(db: &Db) -> Vec<Card> {
    db.queue(events::now_ms())
        .unwrap()
        .into_iter()
        .filter(|card| card.id.starts_with("done:"))
        .collect()
}

#[cfg(unix)]
#[test]
fn interrupted_turn_queues_no_info_card() {
    if let Some(root) = child_root("interrupted_turn_queues_no_info_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                assistant_entry("t1", "toolUse", Some("Dumping rows now."), None),
                assistant_entry("a2", "aborted", None, Some("Interrupted by user")),
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written + tracker.tick(&db).written;
        let queue = done_cards(&db);
        assert!(
            written == 0
                && queue.is_empty()
                && tracker.states.get("%12") == Some(&PaneState::Waiting),
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "interrupted_turn_queues_no_info_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn info_card_waits_for_final_reply_on_disk() {
    if let Some(root) = child_root("info_card_waits_for_final_reply_on_disk") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let narration = assistant_entry("t1", "toolUse", Some("Dumping rows now."), None);
        write_transcript(&root, "/dev/pts/12", &transcript, &[narration.clone()]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let mid_turn = tracker.tick(&db).written;
        let mid_turn_queue = done_cards(&db);
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                narration,
                assistant_entry("a2", "stop", Some("All done."), None),
            ],
        );
        let stopped = tracker.tick(&db).written;
        let queue = done_cards(&db);
        assert!(
            mid_turn == 0
                && mid_turn_queue.is_empty()
                && stopped == 1
                && queue.len() == 1
                && queue[0].id == "done:%12:a2"
                && queue[0].title == "All done.",
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "info_card_waits_for_final_reply_on_disk",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn background_wait_defers_info_card() {
    if let Some(root) = child_root("background_wait_defers_info_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let screens = root.join("tmux-screens");
        fs::create_dir_all(&screens).unwrap();
        fs::write(
            screens.join("0"),
            include_str!("../../tests/fixtures/async_wait.txt"),
        )
        .unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[assistant_entry("a1", "stop", Some("started"), None)],
        );
        let append = |line: String| {
            let mut lines = fs::read_to_string(&transcript).unwrap();
            lines.push_str(&line);
            lines.push('\n');
            fs::write(&transcript, lines).unwrap();
        };
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.tick(&db).written, 0, "wait row shown");
        // omp drops the row as the job finishes, before the job's result reaches the transcript.
        fs::remove_file(screens.join("0")).unwrap();
        assert_eq!(tracker.tick(&db).written, 0, "row gone, transcript unchanged");
        append(
            serde_json::json!({
                "type": "custom_message",
                "id": "c1",
                "customType": "async-result",
                "content": "bg_1 finished"
            })
            .to_string(),
        );
        assert_eq!(tracker.tick(&db).written, 0, "job result delivered");
        append(assistant_entry("a2", "stop", Some("Background job finished."), None));
        assert_eq!(tracker.tick(&db).written, 1, "closing reply");
        assert_eq!(
            done_cards(&db).iter().map(|card| card.id.as_str()).collect::<Vec<_>>(),
            ["done:%12:a2"]
        );
        assert!(db.card("done:%12:a1").unwrap().is_none());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "background_wait_defers_info_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn error_stop_queues_error_card() {
    if let Some(root) = child_root("error_stop_queues_error_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let message = "The server had an error while processing your request.";
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                assistant_entry("t1", "toolUse", Some("Dumping rows now."), None),
                assistant_entry("e2", "error", None, Some(message)),
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let queue = done_cards(&db);
        assert!(
            written == 1
                && queue.len() == 1
                && queue[0].id == "done:%12:e2"
                && queue[0].title == format!("Stopped with error: {message}")
                && queue[0].body.as_deref() == Some(message)
                && queue[0].level.as_deref() == Some("error"),
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "error_stop_queues_error_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn report_survives_background_wake() {
    if let Some(root) = child_root("report_survives_background_wake") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let user = serde_json::json!({
            "type": "message",
            "id": "u1",
            "message": {"role": "user", "content": [{"type": "text", "text": "Ship it."}]}
        })
        .to_string();
        let wake = serde_json::json!({
            "type": "custom_message",
            "id": "c1",
            "customType": "async-result",
            "content": "bg_1 finished"
        })
        .to_string();
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user,
                assistant_entry(
                    "a1",
                    "stop",
                    Some("ENVOY_STATUS: complete\n\nShipped.\n\nRecommended action: Publish the release."),
                    None,
                ),
                wake,
                assistant_entry(
                    "a2",
                    "stop",
                    Some("Background job finished.\n\nRecommended action: Restart the worker."),
                    None,
                ),
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let queue = done_cards(&db);
        assert!(
            written == 1
                && queue.len() == 1
                && queue[0].id == "done:%12:a2"
                && queue[0].title == "Done: Shipped."
                && queue[0].body.as_deref()
                    == Some("Shipped.\n\nRecommended action: Publish the release.\n\n---\n\nBackground job finished.\n\nRecommended action: Restart the worker.")
                && queue[0].star.action.is_empty()
                && queue[0].level.as_deref() == Some("info"),
            "queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "report_survives_background_wake",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn wake_pending_defers_info_card() {
    if let Some(root) = child_root("wake_pending_defers_info_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let user = serde_json::json!({
            "type": "message",
            "id": "u1",
            "message": {"role": "user", "content": [{"type": "text", "text": "Ship it."}]}
        })
        .to_string();
        let wake = serde_json::json!({
            "type": "custom_message",
            "id": "c1",
            "customType": "async-result",
            "content": "bg_5 finished"
        })
        .to_string();
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user,
                assistant_entry("a1", "stop", Some("Ending turn to await bg_5."), None),
                wake,
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let queue = done_cards(&db);
        assert!(
            written == 0 && queue.is_empty(),
            "first tick: written {written}, queue: {queue:?}"
        );
        let mut lines = fs::read_to_string(&transcript).unwrap();
        lines.push_str(&assistant_entry(
            "a2",
            "stop",
            Some("Background job finished."),
            None,
        ));
        lines.push('\n');
        fs::write(&transcript, lines).unwrap();
        let written = tracker.tick(&db).written;
        let queue = done_cards(&db);
        assert!(
            written == 1
                && queue.len() == 1
                && queue[0].id == "done:%12:a2"
                && queue[0].title == "Background job finished."
                && queue[0].body.as_deref() == Some("Background job finished.")
                && queue[0].level.as_deref() == Some("info"),
            "second tick: queue: {queue:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "wake_pending_defers_info_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}
#[cfg(unix)]
#[test]
fn live_service_holds_reply_card() {
    if let Some(root) = child_root("live_service_holds_reply_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let session = "01a1b6cd-0000-7000-8000-000000000001";
        let transcript = root.join(format!("2026-11-07T16-18-24-308Z_{session}.jsonl"));
        write_transcript(&root, "/dev/pts/12", &transcript, &[
            user_line("u1", "Retry the build."),
            assistant_entry("a1", "stop", Some("Build still running; the service reports when it finishes."), None),
        ]);
        let service = root.join(".omp/run/daemons/0000000000000000/daemons/build");
        fs::create_dir_all(&service).unwrap();
        fs::write(service.join("spec.json"), r#"{"name":"build"}"#).unwrap();
        let meta = |state: &str, exited_at: Option<u64>, pending: bool| {
            let mut daemon = serde_json::json!({"name":"build","state":state,"owner":session,"pid":if state == "running" { serde_json::json!(std::process::id()) } else { serde_json::Value::Null },"startedAt":events::now_ms()});
            if let Some(exited_at) = exited_at { daemon["exitedAt"] = exited_at.into(); }
            serde_json::json!({"daemon":daemon,"completionEvents":true,"pendingCompletions":if pending { serde_json::json!([{"name":"build"}]) } else { serde_json::json!([]) }}).to_string()
        };
        fs::write(service.join("meta.json"), meta("running", None, false)).unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.tick(&db).written, 0);
        assert!(done_cards(&db).is_empty());

        fs::write(service.join("meta.json"), meta("exited", Some(events::now_ms() as u64), true)).unwrap();
        assert_eq!(tracker.tick(&db).written, 0);
        assert!(done_cards(&db).is_empty());

        let mut lines = fs::read_to_string(&transcript).unwrap();
        lines.push_str(&serde_json::json!({"type":"custom_message","id":"c1","customType":"launch-completion","content":"Supervised process build exited with exit code 0."}).to_string());
        lines.push('\n');
        fs::write(&transcript, lines).unwrap();
        fs::write(service.join("meta.json"), meta("exited", Some(events::now_ms() as u64), false)).unwrap();
        assert_eq!(tracker.tick(&db).written, 0);
        assert!(done_cards(&db).is_empty());

        let mut lines = fs::read_to_string(&transcript).unwrap();
        lines.push_str(&assistant_entry("a2", "stop", Some("Build finished green."), None));
        lines.push('\n');
        fs::write(&transcript, lines).unwrap();
        assert_eq!(tracker.tick(&db).written, 1);
        assert_eq!(done_cards(&db).iter().map(|card| card.id.as_str()).collect::<Vec<_>>(), ["done:%12:a2"]);
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("live_service_holds_reply_card", scratch.path(), Some(scratch.path()), Some(&bin));
}

#[cfg(unix)]
#[test]
fn active_goal_holds_reply_card_until_grace_ends() {
    if let Some(root) = child_root("active_goal_holds_reply_card_until_grace_ends") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[
            serde_json::json!({"type":"mode_change","id":"m1","mode":"goal","data":{"goal":{"id":"g1","status":"active"}}}).to_string(),
            user_line("u1", "Retry the gate."),
            assistant_entry("a1", "stop", Some("Gate attempt 1 still in flight."), None),
        ]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.tick(&db).written, 0);
        assert!(done_cards(&db).is_empty());
        let old = std::time::SystemTime::now().checked_sub(Duration::from_secs(60)).unwrap();
        fs::File::options().write(true).open(&transcript).unwrap().set_modified(old).unwrap();
        assert_eq!(tracker.tick(&db).written, 1);
        assert_eq!(done_cards(&db).iter().map(|card| card.id.as_str()).collect::<Vec<_>>(), ["done:%12:a1"]);
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("active_goal_holds_reply_card_until_grace_ends", scratch.path(), Some(scratch.path()), Some(&bin));
}

#[cfg(unix)]
#[test]
fn final_report_cards_despite_pending_turn() {
    if let Some(root) = child_root("final_report_cards_despite_pending_turn") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let session = "01a1b6cd-0000-7000-8000-000000000001";
        let transcript = root.join(format!("2026-11-07T16-18-24-308Z_{session}.jsonl"));
        let service = root.join(".omp/run/daemons/0000000000000000/daemons/build");
        fs::create_dir_all(&service).unwrap();
        fs::write(service.join("spec.json"), r#"{"name":"build"}"#).unwrap();
        fs::write(service.join("meta.json"), serde_json::json!({"daemon":{"name":"build","state":"running","owner":session,"pid":std::process::id(),"startedAt":events::now_ms()},"completionEvents":true,"pendingCompletions":[]}).to_string()).unwrap();
        write_transcript(&root, "/dev/pts/12", &transcript, &[
            serde_json::json!({"type":"mode_change","id":"m1","mode":"goal","data":{"goal":{"id":"g1","status":"active"}}}).to_string(),
            user_line("u1", "Ship it."),
            assistant_entry("a1", "stop", Some("ENVOY_STATUS: complete\n\nShipped; watcher still running."), None),
        ]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.tick(&db).written, 1);
        let queue = done_cards(&db);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].id, "done:%12:a1");
        assert_eq!(queue[0].title, "Done: Shipped; watcher still running.");
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("final_report_cards_despite_pending_turn", scratch.path(), Some(scratch.path()), Some(&bin));
}

#[cfg(unix)]
#[test]
fn torn_wait_row_holds_card() {
    if let Some(root) = child_root("torn_wait_row_holds_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let screens = root.join("tmux-capture-seq");
        fs::create_dir_all(&screens).unwrap();
        fs::write(screens.join("0"), "  ⎋ Waiting fo\n\n───\n\n───\n status\n").unwrap();
        fs::write(screens.join("rest"), include_str!("../../tests/fixtures/async_wait.txt")).unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[assistant_entry("a1", "stop", Some("Ending turn to await bg_5."), None)]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.tick(&db).written, 0);
        assert!(done_cards(&db).is_empty());
        fs::write(screens.join("rest"), "───\n\n───\n status\n").unwrap();
        assert_eq!(tracker.tick(&db).written, 0, "row gone, transcript unchanged");
        assert!(done_cards(&db).is_empty());
        // A run that settles without a wake reports after the grace expires.
        tracker.paused_info.get_mut("%12").unwrap().2 = Instant::now().checked_sub(WAIT_ROW_GRACE).unwrap();
        assert_eq!(tracker.tick(&db).written, 1);
        assert_eq!(done_cards(&db).iter().map(|card| card.id.as_str()).collect::<Vec<_>>(), ["done:%12:a1"]);
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("torn_wait_row_holds_card", scratch.path(), Some(scratch.path()), Some(&bin));
}

#[cfg(unix)]
#[test]
fn invalid_status_word_still_queues_a_card() {
    if let Some(root) = child_root("invalid_status_word_still_queues_a_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let reply = serde_json::json!({
            "type": "message",
            "id": "assistant-report",
            "message": {
                "role": "assistant",
                "stopReason": "stop",
                "content": [{"type": "text", "text": "**ENVOY_STATUS: done**\n\nWhat shipped"}]
            }
        })
        .to_string();
        write_transcript(&root, "/dev/pts/12", &transcript, &[reply]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let card = db
            .queue(events::now_ms())
            .unwrap()
            .into_iter()
            .find(|card| card.id == "done:%12:assistant-report");
        assert!(
            written == 1
                && card
                    .as_ref()
                    .is_some_and(|card| card.title == "**ENVOY_STATUS: done**"
                        && card.level.as_deref() == Some("info")),
            "queue: {card:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "invalid_status_word_still_queues_a_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn plain_reply_supersedes_older_queued_info_card() {
    if let Some(root) = child_root("plain_reply_supersedes_older_queued_info_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        let reply = serde_json::json!({
            "type": "message",
            "id": "assistant-report",
            "message": {
                "role": "assistant",
                "stopReason": "stop",
                "content": [{"type": "text", "text": "All done. Listing live.\n\nDetails."}]
            }
        })
        .to_string();
        write_transcript(&root, "/dev/pts/12", &transcript, &[reply]);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let older = info_card("done:%12:older", "%12");
        db.insert_card(&older).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let reason = current_swipe(&db, &older.id).reason;
        let queued = db
            .queue(events::now_ms())
            .unwrap()
            .iter()
            .filter(|card| card.id == "done:%12:assistant-report")
            .count();
        assert!(written == 2 && queued == 1 && reason.as_deref() == Some("superseded"));
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "plain_reply_supersedes_older_queued_info_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn manual_resume_closes_queued_report_cards() {
    if let Some(root) = child_root("manual_resume_closes_queued_report_cards") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_line("u1", "Ship it."),
                assistant_entry(
                    "a1",
                    "stop",
                    Some("ENVOY_STATUS: complete\n\nShipped."),
                    None,
                ),
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let queue = done_cards(&db);
        assert!(
            written == 1 && queue.len() == 1 && queue[0].id == "done:%12:a1",
            "first tick: written {written}, queue {queue:?}"
        );

        fs::remove_file(root.join("tmux-waiting")).unwrap();
        fs::write(root.join("tmux-working"), b"").unwrap();
        let written = tracker.tick(&db).written;
        let swipe = current_swipe(&db, "done:%12:a1");
        let queue = done_cards(&db);
        assert!(
            written == 1
                && queue.is_empty()
                && swipe.action == "done"
                && swipe.reason.as_deref() == Some("resumed in pane"),
            "second tick: written {written}, queue {queue:?}, swipe {swipe:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "manual_resume_closes_queued_report_cards",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn manual_resume_through_a_question_closes_report_cards() {
    if let Some(root) = child_root("manual_resume_through_a_question_closes_report_cards") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_line("u1", "Ship it."),
                assistant_entry(
                    "a1",
                    "stop",
                    Some("ENVOY_STATUS: complete\n\nShipped."),
                    None,
                ),
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.tick(&db).written, 1);

        fs::remove_file(root.join("tmux-waiting")).unwrap();
        let mut lines = fs::read_to_string(&transcript).unwrap();
        lines.push_str(&format!("{}\n", ask_call("call-2", &["Red", "Blue"])));
        fs::write(&transcript, lines).unwrap();
        let written = tracker.tick(&db).written;
        let report_swipe = current_swipe(&db, "done:%12:a1");
        let queue = db.queue(events::now_ms()).unwrap();
        assert!(
            written == 2
                && report_swipe.reason.as_deref() == Some("resumed in pane")
                && queue.iter().any(|card| card.id == "ask:%12:call-2"),
            "second tick: written {written}, queue {queue:?}, report swipe {report_swipe:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "manual_resume_through_a_question_closes_report_cards",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn resume_between_polls_supersedes_report_card() {
    if let Some(root) = child_root("resume_between_polls_supersedes_report_card") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_line("u1", "Ship it."),
                assistant_entry(
                    "a1",
                    "stop",
                    Some("ENVOY_STATUS: complete\n\nShipped."),
                    None,
                ),
            ],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.tick(&db).written, 1);

        let mut lines = fs::read_to_string(&transcript).unwrap();
        lines.push_str(&format!(
            "{}\n{}\n",
            user_line("u2", "Continue."),
            assistant_entry("a2", "stop", Some("All settled."), None)
        ));
        fs::write(&transcript, lines).unwrap();
        let written = tracker.tick(&db).written;
        let old_swipe = current_swipe(&db, "done:%12:a1");
        let queue = done_cards(&db);
        assert!(
            written == 2
                && old_swipe.reason.as_deref() == Some("superseded")
                && queue.len() == 1
                && queue[0].id == "done:%12:a2",
            "second tick: written {written}, queue {queue:?}, old swipe {old_swipe:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "resume_between_polls_supersedes_report_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn resume_keeps_cards_of_dead_pane_ids_and_other_panes() {
    if let Some(root) = child_root("resume_keeps_cards_of_dead_pane_ids_and_other_panes") {
        fs::write(root.join("tmux-working"), b"").unwrap();
        fs::write(root.join("tmux-second-pane"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let current = info_card("done:%12:a1", "%12");
        let dead = info_card("done:%3:old", "%3");
        let other = info_card("done:%13:other", "%13");
        db.insert_card(&current).unwrap();
        db.insert_card(&dead).unwrap();
        db.insert_card(&other).unwrap();
        let mut tracker = Tracker::default();
        tracker.states.insert("%12".to_owned(), PaneState::Waiting);

        let written = tracker.tick(&db).written;
        let current_swipe = current_swipe(&db, &current.id);
        let cards = db.queue(events::now_ms()).unwrap();
        assert!(
            written == 1
                && current_swipe.reason.as_deref() == Some("resumed in pane")
                && cards.iter().any(|card| card.id == dead.id)
                && cards.iter().any(|card| card.id == other.id),
            "written {written}, queue {cards:?}, current swipe {current_swipe:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "resume_keeps_cards_of_dead_pane_ids_and_other_panes",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}
#[test]
fn failed_ask_parse_retries_after_same_length_transcript_recovery() {
    if let Some(root) = child_root("failed_ask_parse_retries_after_same_length_transcript_recovery")
    {
        let transcript = root.join("transcript.jsonl");
        let valid = ask_call("recovered-call", &["Red", "Blue"]);
        fs::create_dir(&transcript).unwrap();
        map_transcript(&root, "/dev/pts/12", &transcript);
        ensure_directory_length(&transcript, valid.len() + 1);
        let pane = pane(&root, "%12", "/dev/pts/12", "π ! asking");
        let mut tracker = Tracker::default();

        tracker.refresh_ask_cache(&pane);
        assert!(!tracker.pending_ask_cache.contains_key(&pane.id));
        let failed_length = fs::metadata(&transcript).unwrap().len();
        replace_directory_with_line(&transcript, &valid, failed_length);
        assert_eq!(fs::metadata(&transcript).unwrap().len(), failed_length);
        tracker.refresh_ask_cache(&pane);
        let recovered = tracker
            .pending_ask_cache
            .get(&pane.id)
            .and_then(|cached| cached.call.as_ref())
            .map(|call| call.tool_call_id.as_str());
        assert_eq!(recovered, Some("recovered-call"));
        assert_eq!(tracker.pending_ask_reads, 2);
        return;
    }

    let scratch = Scratch::new();
    run_child(
        "failed_ask_parse_retries_after_same_length_transcript_recovery",
        scratch.path(),
        Some(scratch.path()),
        None,
    );
}

#[cfg(unix)]
#[test]
fn restored_ask_retries_after_transcript_read_failure() {
    if let Some(root) = child_root("restored_ask_retries_after_transcript_read_failure") {
        let transcript = root.join("transcript.jsonl");
        let result = tool_result("restored-call", "Red");
        fs::create_dir(&transcript).unwrap();
        map_transcript(&root, "/dev/pts/12", &transcript);
        ensure_directory_length(&transcript, result.len() + 1);
        let db = Db::open(&root.join("triage.db")).unwrap();
        let ask = ask_card("%12", "restored-call");
        db.insert_card(&ask).unwrap();
        let mut tracker = Tracker::from_db(&db).unwrap();

        let first_poll = tracker.tick(&db).written;
        let remained_queued = db.card(&ask.id).unwrap().unwrap().1.is_none();
        let length = fs::metadata(&transcript).unwrap().len();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        replace_directory_with_line(&transcript, &result, length);
        let recovery_poll = tracker.tick(&db).written;
        let swipe = db.card(&ask.id).unwrap().unwrap().1;
        let restored_ask_completed = first_poll == 0
            && remained_queued
            && recovery_poll == 1
            && swipe.as_ref().is_some_and(|swipe| {
                swipe.reason.as_deref() == Some("answered in pane") && swipe.option == Some(0)
            });
        assert!(restored_ask_completed);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "restored_ask_retries_after_transcript_read_failure",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn concurrent_ask_answers_send_one_key_sequence() {
    if let Some(root) = child_root("concurrent_ask_answers_send_one_key_sequence") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[ask_call("active-call", &["Red", "Blue"])],
        );
        fs::write(root.join("tmux-block-capture"), b"").unwrap();
        let db = Arc::new(Db::open(&root.join("triage.db")).unwrap());
        let card = ask_card("%12", "active-call");
        db.insert_card(&card).unwrap();
        // The pane lock is the outer lock now: the second answer queues on it, not on the card lock.
        let lock = pane_lock(&db, "%12");
        let baseline = Arc::strong_count(&lock);
        let (sender, receiver) = mpsc::channel();

        let first_db = Arc::clone(&db);
        let first_card = card.clone();
        let first_sender = sender.clone();
        let first = thread::spawn(move || {
            first_sender
                .send((
                    "first",
                    answer(
                        &first_db,
                        &first_card,
                        &answer_req(0, None),
                        &Launch::default(),
                    None,
                    ),
                ))
                .unwrap();
        });
        wait_for_file(&root.join("tmux-capture-entered"));

        let second_db = Arc::clone(&db);
        let second_card = card.clone();
        let second = thread::spawn(move || {
            sender
                .send((
                    "second",
                    answer(
                        &second_db,
                        &second_card,
                        &answer_req(0, None),
                        &Launch::default(),
                    None,
                    ),
                ))
                .unwrap();
        });
        wait_for_lock_waiter(&lock, baseline + 2, &receiver);
        fs::write(root.join("tmux-release-capture"), b"").unwrap();

        let first_result = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        let second_result = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        let (first_result, second_result) = if first_result.0 == "first" {
            (first_result.1, second_result.1)
        } else {
            (second_result.1, first_result.1)
        };
        let presses = fs::read_to_string(root.join("tmux-keys.log")).unwrap();
        let enter_count = presses
            .lines()
            .filter(|line| line.ends_with(" Enter"))
            .count();
        let competing_answer_rejected =
            second_result.as_ref().err().map(String::as_str) == Some("card is not queued");
        let one_key_sequence = enter_count == 1
            && first_result.is_ok()
            && competing_answer_rejected
            && current_swipe(&db, &card.id).reason.as_deref() == Some("typed");
        assert!(one_key_sequence);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "concurrent_ask_answers_send_one_key_sequence",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn other_answer_uses_full_card_option_count_for_navigation() {
    if let Some(root) = child_root("other_answer_uses_full_card_option_count_for_navigation") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[ask_call("active-call", &["Red", "Blue", "Green"])],
        );
        fs::write(root.join("tmux-other-only"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut card = ask_card("%12", "active-call");
        card.options.push(CardOption {
            label: "Green".to_owned(),
            ..CardOption::default()
        });
        db.insert_card(&card).unwrap();

        let request = AnswerReq {
            text: Some("Violet".to_owned()),
            ..AnswerReq::default()
        };
        answer(&db, &card, &request, &Launch::default(), None).unwrap();
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap();
        let down = keys.lines().filter(|line| line.ends_with(" Down")).count();
        let swipe = current_swipe(&db, &card.id);
        let other_selected_as_free_text = down == 3
            && swipe.action == "done"
            && swipe.option.is_none()
            && swipe.text.as_deref() == Some("Violet");
        assert!(other_selected_as_free_text);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "other_answer_uses_full_card_option_count_for_navigation",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}
#[test]
fn status_only_report_warns_and_preserves_info_ack() {
    if let Some(root) = child_root("status_only_report_warns_and_preserves_info_ack") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[serde_json::json!({
                "type": "message",
                "id": "assistant-entry",
                "message": {
                    "role": "assistant",
                    "stopReason": "stop",
                    "content": [{"type": "text", "text": "ENVOY_STATUS: complete"}]
                }
            })
            .to_string()],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let old = info_card("done:%12:older", "%12");
        db.insert_card(&old).unwrap();
        let ack = answer(
            &db,
            &old,
            &answer_req(0, Some("preserved API note")),
            &Launch::default(),
        None,
        );
        let pane = pane(&root, "%12", "/dev/pts/12", "π > session");
        let mut written = 0;
        let inserted = add_info_card(
            &db,
            &pane,
            std::slice::from_ref(&pane),
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut written,
        );
        let old_swipe = current_swipe(&db, &old.id);
        let warning = db.card("done:%12:assistant-entry").unwrap().unwrap();
        let warning_is_present = warning.0.title == "Missing final report"
            && warning.0.body.as_deref() == Some("Status received without a final report.")
            && warning.0.level.as_deref() == Some("warn")
            && warning.1.is_none();
        let warning_contract = ack.is_ok()
            && inserted
            && written == 1
            && old_swipe.note.as_deref() == Some("preserved API note")
            && warning_is_present;
        assert!(warning_contract);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "status_only_report_warns_and_preserves_info_ack",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn dispatch_launches_once_and_defers_asks_but_completes_info() {
    if let Some(root) = child_root("dispatch_launches_once_and_defers_asks_but_completes_info") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let info = info_card("done:%12:report", "%12");
        let ask = ask_card("%12", "ask-call");
        db.insert_card(&info).unwrap();
        db.insert_card(&ask).unwrap();
        let info_result = dispatch(
            &db,
            &info,
            DispatchMode::Investigate,
            Some("check the journal"),
            &in_session("session"),
            None,
            None,
        None,
        );
        let info_swipe = current_swipe(&db, &info.id);
        let repeat = dispatch(
            &db,
            &info,
            DispatchMode::Investigate,
            None,
            &in_session("session"),
            None,
            None,
        None,
        );
        let ask_result = dispatch(
            &db,
            &ask,
            DispatchMode::Investigate,
            None,
            &in_session("session"),
            None,
            None,
        None,
        );
        let ask_swipe = current_swipe(&db, &ask.id);
        let brief_dir = root.join("briefs");
        let briefs: Vec<String> = fs::read_dir(&brief_dir)
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        let info_brief = briefs
            .iter()
            .find(|brief| brief.contains("card.info"))
            .unwrap();
        let contract = info_result.is_ok()
            && info_swipe.action == "done"
            && info_swipe.reason.as_deref() == Some("launched %42")
            && info_swipe.text.as_deref() == Some("Investigate")
            && info_swipe.note.as_deref() == Some("check the journal")
            && repeat.is_err()
            && ask_result.is_ok()
            && ask_swipe.action == "later"
            && ask_swipe.reason.as_deref() == Some("investigating %42")
            && ask_swipe
                .until_ms
                .is_some_and(|until| until > events::now_ms())
            && info_brief.contains("# Investigate: ")
            && info_brief.contains("check the journal")
            && info_brief.contains(INVESTIGATE_STATUS_CONTRACT)
            && !info_brief.contains("ENVOY_STATUS: agent-done")
            && info_brief.contains("`Recommended action:`")
            && new_window_count(&root) == 2;
        assert!(contract);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dispatch_launches_once_and_defers_asks_but_completes_info",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn dispatch_reuses_waiting_source_pane_instead_of_new_window() {
    if let Some(root) = child_root("dispatch_reuses_waiting_source_pane_instead_of_new_window") {
        fs::write(root.join("tmux-waiting"), "").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let info = info_card("done:%12:report", "%12");
        db.insert_card(&info).unwrap();
        let result = dispatch(
            &db,
            &info,
            DispatchMode::Investigate,
            None,
            &in_session("session"),
            None,
            None,
        None,
        );
        let swipe = current_swipe(&db, &info.id);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        assert!(
            result.is_ok()
                && swipe.reason.as_deref() == Some("launched %12")
                && keys.contains("Read the brief at ")
                && new_window_count(&root) == 0
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dispatch_reuses_waiting_source_pane_instead_of_new_window",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_answer_opens_new_window_and_leaves_idle_dispatched_window_alone() {
    if let Some(root) =
        child_root("finding_answer_opens_new_window_and_leaves_idle_dispatched_window_alone")
    {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-idle-screen"), b"").unwrap();
        fs::write(root.join("tmux-tag-%12"), b"finding:old").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[assistant_text_line("a1", "ENVOY_STATUS: complete\n\nDone.")],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = finding_card(&root);
        db.insert_card(&card).unwrap();
        let result = answer(&db, &card, &answer_req(0, None), &Launch::default(), None);
        let swipe = current_swipe(&db, &card.id);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        let log = fs::read_to_string(root.join("tmux.log")).unwrap_or_default();
        let launch = log
            .lines()
            .find(|line| line.starts_with("new-window "))
            .map(str::to_owned);
        assert!(
            result.is_ok()
                && swipe.reason.as_deref() == Some("launched %42")
                && !keys.contains("Read the brief at ")
                && new_window_count(&root) == 1
                && launch
                    .as_deref()
                    .is_some_and(|line| line.contains(" -t triage: ")
                        && line.contains(" --goal ")
                        && !line.contains("@'"))
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_answer_opens_new_window_and_leaves_idle_dispatched_window_alone",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_answer_with_a_long_brief_launches_within_tmux_limit() {
    if let Some(root) = child_root("finding_answer_with_a_long_brief_launches_within_tmux_limit") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut card = finding_card(&root);
        card.star.situation = "situation ".repeat(3_000);
        db.insert_card(&card).unwrap();
        let result = answer(&db, &card, &answer_req(0, None), &Launch::default(), None);
        let log = fs::read_to_string(root.join("tmux.log")).unwrap_or_default();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(new_window_count(&root), 1);
        assert!(log.contains("Carry out the triage brief saved at "));
        assert!(!log.contains("situation situation"), "the brief body reached tmux");
        assert_eq!(current_swipe(&db, &card.id).reason.as_deref(), Some("launched %42"));
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_answer_with_a_long_brief_launches_within_tmux_limit",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn dispatch_uses_launch_settings_and_defaults_to_triage_session() {
    if let Some(root) = child_root("dispatch_uses_launch_settings_and_defaults_to_triage_session") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = info_card("done:%12:none", "%12");
        db.insert_card(&card).unwrap();
        let result = dispatch(
            &db,
            &card,
            DispatchMode::Investigate,
            None,
            &Launch::default(),
            None,
            None,
        None,
        );
        let configured = info_card("done:%12:set", "%12");
        db.insert_card(&configured).unwrap();
        let configured_result = dispatch(
            &db,
            &configured,
            DispatchMode::Investigate,
            None,
            &Launch {
                session: Some("work".to_owned()),
                model: Some("provider/example-model".to_owned()),
                thinking: Some("high".to_owned()),
                ..Launch::default()
            },
            None,
            None,
        None,
        );
        let log = fs::read_to_string(root.join("tmux.log")).unwrap_or_default();
        let launches: Vec<&str> = log
            .lines()
            .filter(|line| line.starts_with("new-window "))
            .collect();
        assert!(
                result.is_ok()
                    && configured_result.is_ok()
                    && launches.len() == 2
                    && launches[0].contains(" -t triage: ")
                    && launches[0].contains(" omp --goal '# Investigate: ")
                    && !launches[0].contains("@'")
                    && launches[1].contains(" -t work: ")
                    && launches[1].contains(" omp --model=provider/example-model --plan=provider/example-model --thinking=high --goal '# Investigate: ")
                    && !log.contains("list-clients")
            );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dispatch_uses_launch_settings_and_defaults_to_triage_session",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn dispatch_act_mode_brief_authorizes_action_and_records_act_swipe() {
    if let Some(root) =
        child_root("dispatch_act_mode_brief_authorizes_action_and_records_act_swipe")
    {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let act_card = info_card("done:%12:act", "%12");
        let look_card = info_card("done:%12:look", "%12");
        db.insert_card(&act_card).unwrap();
        db.insert_card(&look_card).unwrap();
        let act = dispatch(
            &db,
            &act_card,
            DispatchMode::Act,
            Some("ship it"),
            &in_session("session"),
            None,
            None,
        None,
        );
        let look = dispatch(
            &db,
            &look_card,
            DispatchMode::Investigate,
            None,
            &in_session("session"),
            None,
            None,
        None,
        );
        let swipe = current_swipe(&db, &act_card.id);
        let briefs: Vec<String> = fs::read_dir(root.join("briefs"))
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        let act_brief = briefs
            .iter()
            .find(|brief| brief.contains("# Act: "))
            .unwrap();
        let look_brief = briefs
            .iter()
            .find(|brief| brief.contains("# Investigate: "))
            .unwrap();
        assert!(
            act.is_ok()
                && look.is_ok()
                && act_brief.contains(ACT_INSTRUCTION)
                && act_brief.contains("ship it")
                && act_brief.contains("ENVOY_STATUS: complete")
                && act_brief.contains("ENVOY_STATUS: agent-done")
                && act_brief.contains("and names that step")
                && !look_brief.contains(ACT_INSTRUCTION)
                && swipe.action == "done"
                && swipe.text.as_deref() == Some("Act")
                && swipe.reason.as_deref() == Some("launched %42")
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dispatch_act_mode_brief_authorizes_action_and_records_act_swipe",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[test]
fn dispatch_mode_parse_accepts_known_modes_only() {
    assert_eq!(
        DispatchMode::parse("investigate"),
        Some(DispatchMode::Investigate)
    );
    assert_eq!(DispatchMode::parse("plan"), Some(DispatchMode::Plan));
    assert_eq!(DispatchMode::parse("act"), Some(DispatchMode::Act));
    assert_eq!(
        DispatchMode::parse("plan-into"),
        Some(DispatchMode::PlanInto)
    );
    assert_eq!(DispatchMode::parse("Act"), None);
    assert_eq!(DispatchMode::parse("fix"), None);
    assert_eq!(DispatchMode::parse(""), None);
}

#[cfg(unix)]
#[test]
fn dispatch_plan_mode_writes_reviewable_brief_and_config_overlay() {
    if let Some(root) = child_root("dispatch_plan_mode_writes_reviewable_brief_and_config_overlay")
    {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut card = info_card("done:%12:plan", "%12");
        card.source.cwd = root.display().to_string();
        db.insert_card(&card).unwrap();

        let result = dispatch(
            &db,
            &card,
            DispatchMode::Plan,
            Some("plan it"),
            &in_session("session"),
            None,
            None,
        None,
        );
        let mut brief_path = None;
        let mut overlay_path = None;
        for entry in fs::read_dir(root.join("briefs")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            if name.starts_with("triage-brief-") {
                brief_path = Some(path);
            } else if name.starts_with("plan-mode-") {
                overlay_path = Some(path);
            }
        }
        let brief_path = brief_path.unwrap();
        let overlay_path = overlay_path.unwrap();
        let brief = fs::read_to_string(&brief_path).unwrap();
        let overlay = fs::read_to_string(&overlay_path).unwrap();
        let launch = fs::read_to_string(root.join("tmux.log"))
            .unwrap()
            .lines()
            .find(|line| line.starts_with("new-window "))
            .unwrap()
            .to_owned();
        let swipe = current_swipe(&db, &card.id);

        assert!(result.is_ok());
        assert!(brief.starts_with("# Plan: Earlier result"));
        assert!(brief.contains(PLAN_INSTRUCTION));
        assert!(brief.contains(ENVOY_STATUS_CONTRACT));
        assert!(brief.contains("plan it"));
        assert_eq!(overlay, "plan:\n  defaultOnStartup: true\n");
        assert!(
            launch.contains(&format!(
                "--config '{}' '# Plan: Earlier result' @'{}'",
                overlay_path.display(),
                brief_path.display()
            )),
            "{launch}"
        );
        assert_eq!(swipe.reason.as_deref(), Some("launched %42"));
        assert_eq!(new_window_count(&root), 1);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dispatch_plan_mode_writes_reviewable_brief_and_config_overlay",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn plan_into_dispatch_opens_a_new_window_even_when_the_source_pane_waits() {
    if let Some(root) =
        child_root("plan_into_dispatch_opens_a_new_window_even_when_the_source_pane_waits")
    {
        let db = Db::open(&root.join("triage.db")).unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let mut card = info_card("done:%12:plan-into", "%12");
        card.source.cwd = root.display().to_string();
        db.insert_card(&card).unwrap();

        let mut launch = in_session("session");
        launch.model = Some("provider/planner".to_owned());
        let result = dispatch(
            &db,
            &card,
            DispatchMode::PlanInto,
            Some("plan it"),
            &launch,
            Some("provider/executor"),
            None,
        None,
        );
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        let brief = fs::read_dir(root.join("briefs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("triage-brief-")
            })
            .unwrap();
        let brief_text = fs::read_to_string(&brief).unwrap();
        let overlays = fs::read_dir(root.join("briefs"))
            .unwrap()
            .filter(|entry| {
                entry.as_ref().is_ok_and(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("plan-mode-")
                })
            })
            .count();
        let launch_line = fs::read_to_string(root.join("tmux.log"))
            .unwrap()
            .lines()
            .find(|line| line.starts_with("new-window "))
            .unwrap()
            .to_owned();
        let swipe = current_swipe(&db, &card.id);

        assert!(result.is_ok());
        assert!(!keys.contains("Read the brief at "), "{keys}");
        assert!(
                launch_line.contains(&format!(
                    "--model=provider/planner --plan=provider/planner --plan-yolo --plan-yolo-into=provider/executor '# Plan into: Earlier result' @'{}'",
                    brief.display()
                )),
                "{launch_line}"
            );
        assert_eq!(new_window_count(&root), 1);
        assert_eq!(overlays, 0);
        assert!(
            brief_text.starts_with("# Plan into: Earlier result"),
            "{brief_text}"
        );
        assert!(brief_text.contains(PLAN_INTO_INSTRUCTION));
        assert!(brief_text.contains(ENVOY_STATUS_CONTRACT));
        assert!(brief_text.contains("plan it"));
        assert!(brief_text.ends_with(&format!("\n{PLAN_REPORT_STEP}\n")));
        assert_eq!(swipe.text.as_deref(), Some("Plan into"));
        assert_eq!(swipe.reason.as_deref(), Some("launched %42"));
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "plan_into_dispatch_opens_a_new_window_even_when_the_source_pane_waits",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn plan_dispatch_toggles_waiting_pane_only_when_needed_and_rejects_toggle_failure() {
    if let Some(root) =
        child_root("plan_dispatch_toggles_waiting_pane_only_when_needed_and_rejects_toggle_failure")
    {
        let db = Db::open(&root.join("triage.db")).unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let off = info_card("done:%12:off", "%12");
        db.insert_card(&off).unwrap();
        let off_result = dispatch(
            &db,
            &off,
            DispatchMode::Plan,
            None,
            &in_session("session"),
            None,
            None,
        None,
        );

        let on = info_card("done:%12:on", "%12");
        db.insert_card(&on).unwrap();
        let on_result = dispatch(
            &db,
            &on,
            DispatchMode::Plan,
            None,
            &in_session("session"),
            None,
            None,
        None,
        );
        let toggles_after_on = fs::read_to_string(root.join("tmux-keys.log"))
            .unwrap()
            .lines()
            .filter(|line| line.contains(" M-P"))
            .count();

        fs::remove_file(root.join("tmux-plan-mode")).unwrap();
        fs::write(root.join("tmux-plan-toggle-fails"), b"").unwrap();
        let failed = info_card("done:%12:failed", "%12");
        db.insert_card(&failed).unwrap();
        let failure = dispatch(
            &db,
            &failed,
            DispatchMode::Plan,
            None,
            &in_session("session"),
            None,
            None,
        None,
        );
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap();
        let toggle_count = keys.lines().filter(|line| line.contains(" M-P")).count();
        let brief_count = keys.lines().filter(|line| line.contains("-l --")).count();

        assert!(off_result.is_ok() && on_result.is_ok());
        assert_eq!(toggles_after_on, 1);
        assert_eq!(toggle_count, 2);
        assert_eq!(brief_count, 2);
        assert_eq!(
            failure.as_ref().err().map(String::as_str),
            Some("plan mode did not turn on")
        );
        assert!(db.card(&failed.id).unwrap().unwrap().1.is_none());
        assert_eq!(new_window_count(&root), 0);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "plan_dispatch_toggles_waiting_pane_only_when_needed_and_rejects_toggle_failure",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn plan_answers_approve_refine_and_reject_invalid_refinements() {
    if let Some(root) = child_root("plan_answers_approve_refine_and_reject_invalid_refinements") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let pane = pane(&root, "%12", "/dev/pts/12", "π > session");
        fs::write(root.join("tmux-plan-review"), b"").unwrap();
        let approve = plan_card(&pane, 0);
        let empty = plan_card(&pane, 1);
        let unknown = plan_card(&pane, 2);
        let refine = plan_card(&pane, 3);
        let other = plan_card(&pane, 4);
        let refine_with_executor = plan_card(&pane, 5);
        for card in [&approve, &empty, &unknown, &refine, &other, &refine_with_executor] {
            db.insert_card(card).unwrap();
        }

        let empty_result = answer(
            &db,
            &empty,
            &AnswerReq {
                option: Some(1),
                text: Some("  ".to_owned()),
                ..AnswerReq::default()
            },
            &Launch::default(),
        None,
        );
        let unknown_result = answer(
            &db,
            &unknown,
            &AnswerReq {
                option: Some(2),
                ..AnswerReq::default()
            },
            &Launch::default(),
        None,
        );
        let approve_result = answer(&db, &approve, &answer_req(0, None), &Launch::default(), None);
        let refine_result = answer(
            &db,
            &refine,
            &AnswerReq {
                option: Some(1),
                text: Some("Add a rollback step".to_owned()),
                note: Some("Reviewed".to_owned()),
                ..AnswerReq::default()
            },
            &Launch::default(),
        None,
        );
        let executor_refine_result = answer(
            &db,
            &refine_with_executor,
            &AnswerReq {
                option: Some(1),
                text: Some("Use @smol".to_owned()),
                executor: Some("@smol".to_owned()),
                ..AnswerReq::default()
            },
            &Launch::default(),
        None,
        );
        let other_result = answer(
            &db,
            &other,
            &AnswerReq {
                text: Some("Keep the current owner".to_owned()),
                ..AnswerReq::default()
            },
            &Launch::default(),
        None,
        );
        let approve_swipe = current_swipe(&db, &approve.id);
        let refine_swipe = current_swipe(&db, &refine.id);
        let other_swipe = current_swipe(&db, &other.id);
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap();
        let escape = keys.find("send-keys -t %12 Esc").unwrap();
        let text = keys
            .find("send-keys -t %12 -l -- Add a rollback step")
            .unwrap();
        let next_enter = text + keys[text..].find("send-keys -t %12 Enter").unwrap();

        assert_eq!(
            empty_result.as_ref().err().map(String::as_str),
            Some("Refine requires nonempty text")
        );
        assert_eq!(
            unknown_result.as_ref().err().map(String::as_str),
            Some("unknown plan option")
        );
        assert_eq!(
            executor_refine_result.as_ref().err().map(String::as_str),
            Some("execution model requires Approve")
        );
        assert!(approve_result.is_ok() && refine_result.is_ok() && other_result.is_ok());
        assert!(db.card(&empty.id).unwrap().unwrap().1.is_none());
        assert!(db.card(&unknown.id).unwrap().unwrap().1.is_none());
        assert_eq!(approve_swipe.option, Some(0));
        assert_eq!(approve_swipe.reason.as_deref(), Some("typed"));
        assert_eq!(refine_swipe.option, Some(1));
        assert_eq!(refine_swipe.text.as_deref(), Some("Add a rollback step"));
        assert_eq!(refine_swipe.note.as_deref(), Some("Reviewed"));
        assert_eq!(other_swipe.option, None);
        assert!(escape < text && text < next_enter);
        assert_eq!(
            keys.lines()
                .filter(|line| *line == "send-keys -t %12 Escape")
                .count(),
            2
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "plan_answers_approve_refine_and_reject_invalid_refinements",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn plan_approval_selects_execution_model_before_enter() {
    if let Some(root) = child_root("plan_approval_selects_execution_model_before_enter") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let pane = pane(&root, "%12", "/dev/pts/12", "π > session");
        let screens = root.join("tmux-screens");
        fs::create_dir_all(&screens).unwrap();
        let review = |selected: &str| {
            let track = match selected {
                "smol" => "◀ smol ▶ default ┆ slow",
                "default" => "smol ◀ default ▶ slow",
                "slow" => "smol ┆ default ◀ slow ▶",
                _ => unreachable!(),
            };
            format!("╭─ Plan Review ─╮\n│ continue with ◂ {track} ▸\n│ ↑/↓ select · ⏎ confirm · ←/→ model\n")
        };
        for (index, selected) in ["default", "slow", "default", "default", "default", "default"].iter().enumerate() {
            fs::write(screens.join(index.to_string()), review(selected)).unwrap();
        }
        let selected = plan_card(&pane, 0);
        let blank = plan_card(&pane, 4);
        let effort = plan_card(&pane, 5);
        let unavailable = plan_card(&pane, 1);
        let default = plan_card(&pane, 2);
        let mismatch = plan_card(&pane, 3);
        db.insert_card(&blank).unwrap();
        for card in [&selected, &unavailable, &default, &mismatch, &effort] {
            db.insert_card(card).unwrap();
        }

        let selected_result = answer(
            &db,
            &selected,
            &AnswerReq { option: Some(0), executor: Some("@slow".to_owned()), ..AnswerReq::default() },
            &Launch::default(),
        None,
        );
        let unavailable_result = answer(
            &db,
            &unavailable,
            &AnswerReq { option: Some(0), executor: Some("@missing".to_owned()), ..AnswerReq::default() },
            &Launch::default(),
        None,
        );
        let default_result = answer(
            &db,
            &default,
            &AnswerReq { option: Some(0), executor: Some("@default".to_owned()), ..AnswerReq::default() },
            &Launch::default(),
        None,
        );
        let mismatch_result = answer(
            &db,
            &mismatch,
            &AnswerReq { option: Some(0), executor: Some("@slow".to_owned()), ..AnswerReq::default() },
            &Launch::default(),
        None,
        );
        let blank_result = answer(
            &db,
            &blank,
            &AnswerReq { option: Some(0), ..AnswerReq::default() },
            &Launch::default(),
        None,
        );
        let effort_result = answer(
            &db,
            &effort,
            &AnswerReq {
                option: Some(0),
                executor: Some("@slow".to_owned()),
                executor_thinking: Some("low".to_owned()),
                ..AnswerReq::default()
            },
            &Launch::default(),
        None,
        );
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap();
        let keys: Vec<_> = keys.lines().collect();

        assert!(selected_result.is_ok() && default_result.is_ok() && blank_result.is_ok());
        assert_eq!(
            unavailable_result.as_ref().err().map(String::as_str),
            Some("plan review does not offer @missing")
        );
        assert_eq!(
            mismatch_result.as_ref().err().map(String::as_str),
            Some("plan execution model is not @slow")
        );
        assert_eq!(
            effort_result.as_ref().err().map(String::as_str),
            Some("an omp plan review keeps the role's own thinking level")
        );
        assert_eq!(keys, [
            "send-keys -t %12 Right",
            "send-keys -t %12 Enter",
            "send-keys -t %12 Enter",
            "send-keys -t %12 Right",
            "send-keys -t %12 Enter",
        ]);
        assert_eq!(current_swipe(&db, &selected.id).text.as_deref(), Some("@slow"));
        assert_eq!(current_swipe(&db, &default.id).text.as_deref(), Some("@default"));
        assert!(current_swipe(&db, &blank.id).text.is_none());
        assert!(db.card(&unavailable.id).unwrap().unwrap().1.is_none());
        assert!(db.card(&mismatch.id).unwrap().unwrap().1.is_none());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "plan_approval_selects_execution_model_before_enter",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn close_kills_source_pane_and_failed_kill_keeps_card_queued() {
    if let Some(root) = child_root("close_kills_source_pane_and_failed_kill_keeps_card_queued") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let kept = info_card("done:%12:kept", "%12");
        let closed = info_card("done:%12:closed", "%12");
        db.insert_card(&kept).unwrap();
        db.insert_card(&closed).unwrap();
        fs::write(root.join("tmux-fail-kill"), "").unwrap();
        let failed = close(&db, &kept, None, &Launch::default());
        let kept_queued = db.card(&kept.id).unwrap().unwrap().1.is_none();
        fs::remove_file(root.join("tmux-fail-kill")).unwrap();
        let result = close(&db, &closed, Some("thanks"), &Launch::default());
        let swipe = current_swipe(&db, &closed.id);
        let repeat = close(&db, &closed, None, &Launch::default());
        let log = fs::read_to_string(root.join("tmux.log")).unwrap();
        assert!(failed.is_err() && kept_queued && result.is_ok() && repeat.is_err());
        assert_eq!(swipe.action, "done");
        assert_eq!(swipe.reason.as_deref(), Some("closed %12"));
        assert_eq!(swipe.note.as_deref(), Some("thanks"));
        assert_eq!(
            log.lines()
                .filter(|line| *line == "kill-pane -t %12")
                .count(),
            2
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "close_kills_source_pane_and_failed_kill_keeps_card_queued",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn tick_reads_unresolved_cards_once_for_all_panes() {
    unsafe extern "C" fn count_card_scans(
        _: std::ffi::c_uint,
        context: *mut std::ffi::c_void,
        _: *mut std::ffi::c_void,
        sql: *mut std::ffi::c_void,
    ) -> std::ffi::c_int {
        let sql = unsafe { std::ffi::CStr::from_ptr(sql.cast()) }.to_bytes();
        if sql
            .windows(b"latest_rowid".len())
            .any(|window| window == b"latest_rowid")
        {
            unsafe { *context.cast::<usize>() += 1 };
        }
        0
    }

    if let Some(root) = child_root("tick_reads_unresolved_cards_once_for_all_panes") {
        // Pane %12 is a dead shell and %13 a waiting omp pane, neither with cards.
        fs::write(root.join("tmux-dead"), b"").unwrap();
        fs::write(root.join("tmux-second-pane"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let mut scans = 0usize;
        let conn = db.conn.lock().unwrap();
        let traced = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                conn.handle(),
                rusqlite::ffi::SQLITE_TRACE_STMT,
                Some(count_card_scans),
                (&mut scans as *mut usize).cast(),
            )
        };
        drop(conn);
        tracker.tick(&db);
        let conn = db.conn.lock().unwrap();
        let untraced = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(conn.handle(), 0, None, std::ptr::null_mut())
        };
        drop(conn);
        assert_eq!(
            (traced, untraced),
            (rusqlite::ffi::SQLITE_OK, rusqlite::ffi::SQLITE_OK)
        );
        assert_eq!(
            scans, 1,
            "a tick scans unresolved cards once, not once per pane"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "tick_reads_unresolved_cards_once_for_all_panes",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn pane_actions_wait_for_the_action_in_flight() {
    if let Some(root) = child_root("pane_actions_wait_for_the_action_in_flight") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let acknowledged = info_card("done:%12:ack", "%12");
        let dispatched = info_card("done:%12:dispatch", "%12");
        let closed = info_card("done:%12:close", "%12");
        for card in [&acknowledged, &dispatched, &closed] {
            db.insert_card(card).unwrap();
        }
        let launch = in_session("session");
        let lock = pane_lock(&db, "%12");
        let guard = lock.lock().unwrap();
        thread::scope(|scope| {
            let actions = [
                scope.spawn(|| answer(&db, &acknowledged, &AnswerReq::default(), &launch, None)),
                scope.spawn(|| {
                    dispatch(
                        &db,
                        &dispatched,
                        DispatchMode::Investigate,
                        None,
                        &launch,
                        None,
                        None,
                    None,
                    )
                }),
                scope.spawn(|| close(&db, &closed, None, &launch)),
            ];
            thread::sleep(Duration::from_millis(300));
            let acknowledged_early = db.card(&acknowledged.id).unwrap().unwrap().1.is_some();
            let tmux_early = root.join("tmux.log").exists();
            drop(guard);
            let results = actions.map(|action| action.join().unwrap());
            assert!(
                !acknowledged_early && !tmux_early,
                "an action reached pane %12 while another held it"
            );
            assert_eq!(results, [Ok(()), Ok(()), Ok(())]);
        });
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "pane_actions_wait_for_the_action_in_flight",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn close_acknowledges_when_pane_is_already_gone() {
    if let Some(root) = child_root("close_acknowledges_when_pane_is_already_gone") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let gone = info_card("done:%12:gone", "%12");
        let mut paneless = info_card("done:%12:paneless", "%12");
        paneless.source.pane = None;
        db.insert_card(&gone).unwrap();
        db.insert_card(&paneless).unwrap();
        fs::write(root.join("tmux-gone-pane"), "").unwrap();
        let gone_result = close(&db, &gone, Some("noted"), &Launch::default());
        let paneless_result = close(&db, &paneless, None, &Launch::default());
        let gone_swipe = current_swipe(&db, &gone.id);
        let paneless_swipe = current_swipe(&db, &paneless.id);
        let queue = db.queue(events::now_ms()).unwrap();
        assert!(gone_result.is_ok() && paneless_result.is_ok() && queue.is_empty());
        assert_eq!(gone_swipe.action, "done");
        assert_eq!(gone_swipe.option, None);
        assert_eq!(gone_swipe.text, None);
        assert_eq!(gone_swipe.reason, None);
        assert_eq!(gone_swipe.note.as_deref(), Some("noted"));
        assert_eq!(paneless_swipe.action, "done");
        assert_eq!(paneless_swipe.reason, None);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "close_acknowledges_when_pane_is_already_gone",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn start_session_rejects_blank_prompt_and_launches_brief() {
    if let Some(root) = child_root("start_session_rejects_blank_prompt_and_launches_brief") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let blank = start_session(
            &db,
            "  \n",
            "/tmp",
            &in_session("session"),
            Start::Goal,
        None,
        );
        let launched = start_session(
            &db,
            "Reply with pong.",
            "/tmp",
            &in_session("session"),
            Start::Goal,
        None,
        );
        let brief = fs::read_dir(root.join("briefs"))
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .next()
            .unwrap();
        let contract = blank.is_err()
            && launched.as_deref() == Ok("%42")
            && brief.starts_with("Reply with pong.\n")
            && brief.contains("ENVOY_STATUS: complete")
            && brief.contains("ENVOY_STATUS: agent-done")
            && brief.contains("and names that step")
            && new_window_count(&root) == 1;
        assert!(contract);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "start_session_rejects_blank_prompt_and_launches_brief",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn start_session_plan_mode_uses_overlay_config_before_the_prompt_brief() {
    if let Some(root) =
        child_root("start_session_plan_mode_uses_overlay_config_before_the_prompt_brief")
    {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let result = start_session(
            &db,
            "Prepare a plan.",
            "/tmp",
            &in_session("session"),
            Start::Plan,
        None,
        );
        let mut brief_path = None;
        let mut overlay_path = None;
        for entry in fs::read_dir(root.join("briefs")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            if name.starts_with("triage-brief-") {
                brief_path = Some(path);
            } else if name.starts_with("plan-mode-") {
                overlay_path = Some(path);
            }
        }
        let brief_path = brief_path.unwrap();
        let overlay_path = overlay_path.unwrap();
        let brief = fs::read_to_string(&brief_path).unwrap();
        let launch = fs::read_to_string(root.join("tmux.log"))
            .unwrap()
            .lines()
            .find(|line| line.starts_with("new-window "))
            .unwrap()
            .to_owned();

        assert_eq!(result.as_deref(), Ok("%42"));
        assert!(brief.starts_with("Prepare a plan.\n"));
        assert_eq!(
            fs::read_to_string(&overlay_path).unwrap(),
            "plan:\n  defaultOnStartup: true\n"
        );
        assert!(
            launch.contains(&format!(
                "--config '{}' 'Prepare a plan.' @'{}'",
                overlay_path.display(),
                brief_path.display()
            )),
            "{launch}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "start_session_plan_mode_uses_overlay_config_before_the_prompt_brief",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[test]
fn chosen_model_and_thinking_reach_launch_and_bad_values_are_refused() {
    let chosen = launch_flags(Some("provider/example-model"), Some("high")).unwrap();
    assert_eq!(
        chosen,
        "--model=provider/example-model --plan=provider/example-model --thinking=high "
    );
    assert_eq!(launch_flags(Some(" "), Some("")).unwrap(), "");
    assert!(launch_flags(None, Some("turbo")).is_err());
    assert!(launch_flags(Some("x; rm -rf ~"), None).is_err());
}

#[test]
fn plan_into_command_hands_the_plan_to_the_execution_model() {
    let directory = std::env::temp_dir().join(format!("triage-plan-command-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let brief = directory.join("brief.md");
    fs::write(&brief, "# Ship the fix\n\nDo the work.\n").unwrap();
    let chosen = omp_command(
        &brief,
        Some("provider/planner"),
        Some("high"),
        Start::PlanInto(Some("provider/executor"), None),
        None,
    )
    .unwrap();
    assert_eq!(
        chosen,
        format!(
            "omp --model=provider/planner --plan=provider/planner --thinking=high --plan-yolo --plan-yolo-into=provider/executor '# Ship the fix' @'{}'",
            brief.display()
        )
    );
    let blank = omp_command(&brief, None, None, Start::PlanInto(Some(" "), None), None).unwrap();
    assert_eq!(blank, format!("omp --plan-yolo '# Ship the fix' @'{}'", brief.display()));
    let default_with_effort = omp_command(&brief, None, None, Start::PlanInto(None, Some("medium")), None).unwrap();
    assert_eq!(default_with_effort, format!("omp --plan-yolo --plan-yolo-into=@smol:medium '# Ship the fix' @'{}'", brief.display()));
    assert!(omp_command(&brief, None, None, Start::PlanInto(Some("x; rm -rf ~"), None), None).is_err());
    let leveled = omp_command(&brief, None, None, Start::PlanInto(Some("@smol"), Some("low")), None).unwrap();
    assert_eq!(leveled, format!("omp --plan-yolo --plan-yolo-into=@smol:low '# Ship the fix' @'{}'", brief.display()));
    let named = omp_command(&brief, None, None, Start::PlanInto(Some("provider/executor:max"), Some("low")), None).unwrap();
    assert_eq!(named, format!("omp --plan-yolo --plan-yolo-into=provider/executor:max '# Ship the fix' @'{}'", brief.display()));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn plan_command_prefixes_brief_heading() {
    let directory = std::env::temp_dir().join(format!("triage-plan-heading-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let brief = directory.join("brief.md");
    fs::write(&brief, "\n# Act: Ship it\n\nDo the work.\n").unwrap();

    let command = omp_command(&brief, None, None, Start::Plan, None).unwrap();

    assert!(command.contains("'# Act: Ship it' @'"), "{command}");
    assert!(command.ends_with(&format!("@'{}'", brief.display())), "{command}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn execution_effort_belongs_to_each_engine() {
    assert_eq!(executor_effort(Engine::Dsh, Some(" max ")), Ok(Some("max")));
    // dsh reports each model's own effort ids, so a model may offer medium or xhigh.
    assert_eq!(executor_effort(Engine::Dsh, Some("medium")), Ok(Some("medium")));
    assert_eq!(
        executor_effort(Engine::Dsh, Some("turbo")),
        Err("unknown execution effort turbo".to_owned())
    );
    assert_eq!(executor_effort(Engine::Omp, Some("medium")), Ok(Some("medium")));
    assert_eq!(
        executor_effort(Engine::Omp, Some("turbo")),
        Err("unknown execution thinking level turbo".to_owned())
    );
    assert_eq!(executor_effort(Engine::Omp, None), Ok(None));
    assert_eq!(executor_effort(Engine::Dsh, Some("  ")), Ok(None));
    assert_eq!(plan_into_choices(Engine::Omp, None, Some("medium")), Ok(()));
    assert_eq!(
        plan_into_choices(Engine::Dsh, None, Some("low")),
        Err("execution effort needs an execution model".to_owned())
    );
}

#[test]
fn start_parse_refuses_an_execution_effort_outside_plan_into() {
    assert_eq!(
        Start::parse(Some("plan"), None, Some("low")),
        Err("executor thinking needs mode plan-into".to_owned())
    );
    assert!(matches!(
        Start::parse(Some("plan-into"), Some("deepseek-flash"), Some("low")),
        Ok(Start::PlanInto(Some("deepseek-flash"), Some("low")))
    ));
}

#[test]
fn goal_command_names_the_brief_in_its_objective() {
    let directory =
        std::env::temp_dir().join(format!("triage-goal-command-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let brief = directory.join("brief.md");
    fs::write(&brief, "\n# Act: Ship it\n\nDo the work.\n").unwrap();
    let chosen = omp_command(
        &brief,
        Some("provider/example-model"),
        Some("high"),
        Start::Goal,
        None,
    )
    .unwrap();
    assert_eq!(
        chosen,
        format!(
            "omp --model=provider/example-model --plan=provider/example-model --thinking=high --goal '# Act: Ship it\n\nCarry out the triage brief saved at {}. Read the whole file before you start and again before you mark the goal complete; it holds the full task and the report format.'",
            brief.display()
        )
    );
    let blank = omp_command(&brief, Some(" "), Some(""), Start::Goal, None).unwrap();
    assert!(blank.starts_with("omp --goal '# Act: Ship it"), "{blank}");
    assert!(!blank.contains('@'));
    assert!(omp_command(&brief, None, Some("turbo"), Start::Goal, None).is_err());
    assert!(omp_command(&brief, Some("x; rm -rf ~"), None, Start::Goal, None).is_err());
    assert!(omp_command(&directory.join("missing.md"), None, None, Start::Goal, None).is_err());

    let heading = format!("# Act: {}", "Ship it ".repeat(50));
    fs::write(&brief, format!("{heading}\n\n{}\n", "BODY ".repeat(20_000))).unwrap();
    let long = omp_command(&brief, None, None, Start::Goal, None).unwrap();
    assert!(long.len() < 1024, "{} bytes", long.len());
    assert!(long.contains(&format!("saved at {}.", brief.display())), "{long}");
    assert!(long.contains('…') && !long.contains("BODY"), "{long}");
    fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn an_image_rides_the_omp_command() {
    let directory =
        std::env::temp_dir().join(format!("triage-image-command-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let brief = directory.join("brief.md");
    fs::write(&brief, "# Act: Ship it\n\nDo the work.\n").unwrap();
    let image_path = directory.join("shot.png");
    fs::write(&image_path, b"png").unwrap();
    let image = crate::uploads::Upload {
        path: image_path.clone(),
        base64: "iVBORw0KGgo=".to_owned(),
        media_type: "image/png".to_owned(),
        name: "shot.png".to_owned(),
    };
    let goal = omp_command(&brief, None, None, Start::Goal, Some(&image)).unwrap();
    assert!(goal.contains(&format!("Attached image: @{}.", image_path.display())), "{goal}");
    let plan = omp_command(&brief, None, None, Start::Plan, Some(&image)).unwrap();
    assert!(plan.ends_with(&format!("@'{}'", image_path.display())), "{plan}");
    let into = omp_command(&brief, None, None, Start::PlanInto(None, None), Some(&image)).unwrap();
    assert!(into.ends_with(&format!("@'{}'", image_path.display())), "{into}");
    let no_image = omp_command(&brief, None, None, Start::Goal, None).unwrap();
    assert!(!no_image.contains(&image_path.display().to_string()), "{no_image}");
    fs::remove_dir_all(&directory).unwrap();
}

#[cfg(unix)]
#[test]
fn dispatch_with_an_image_types_it_into_a_waiting_pane() {
    if let Some(root) = child_root("dispatch_with_an_image_types_it_into_a_waiting_pane") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = info_card("done:%12:report", "%12");
        db.insert_card(&card).unwrap();
        let image_path = root.join("shot.png");
        fs::write(&image_path, b"png").unwrap();
        let image = crate::uploads::Upload {
            path: fs::canonicalize(&image_path).unwrap(),
            base64: "iVBORw0KGgo=".to_owned(),
            media_type: "image/png".to_owned(),
            name: "shot.png".to_owned(),
        };
        dispatch(
            &db,
            &card,
            DispatchMode::Act,
            None,
            &in_session("session"),
            None,
            None,
            Some(&image),
        )
        .unwrap();
        let keys = fs::read_to_string(root.join("tmux-keys.log")).unwrap_or_default();
        assert!(
            keys.contains(&format!("@{}", image.path.display())),
            "the image reference reached the pane: {keys}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dispatch_with_an_image_types_it_into_a_waiting_pane",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

/// omp writes the `--goal` objective as the session's first user message. An exited
/// goal session that never reported must still be recognised as a triage dispatch, or
/// its Unreported warning card is silently lost.
#[test]
fn exited_goal_session_without_report_is_audited_as_a_dispatch() {
    let root = std::env::temp_dir().join(format!("triage-goal-audit-{}", uuid::Uuid::new_v4()));
    let briefs = root.join("triage").join("briefs");
    fs::create_dir_all(&briefs).unwrap();
    let brief = briefs.join("triage-brief-goal.md");
    fs::write(&brief, "# Act: It's done\n\nDo the work.\n").unwrap();
    let command = omp_command(&brief, None, None, Start::Goal, None).unwrap();
    let quoted = command
        .strip_prefix("omp --goal '")
        .unwrap()
        .strip_suffix('\'')
        .unwrap();
    let objective = quoted.replace("'\\''", "'");
    let user = serde_json::json!({"type": "message", "id": "u1", "message": {"role": "user", "content": [{"type": "text", "text": objective}]}});
    let reply = serde_json::json!({"type": "message", "id": "a1", "message": {"role": "assistant", "content": [{"type": "text", "text": "Work done, details inside."}]}});
    let transcript = root.join("transcript.jsonl");
    fs::write(&transcript, format!("{user}\n{reply}\n")).unwrap();
    let audit = omp::dispatch_audit(&transcript).unwrap();
    assert!(objective.starts_with("# Act: It's done"), "{objective}");
    assert!(audit.dispatched && !audit.reported, "{objective}");
    fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn tracker_ask_completion_preserves_api_note() {
    if let Some(root) = child_root("tracker_ask_completion_preserves_api_note") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[ask_call("ask-call", &["Red", "Blue"])],
        );
        fs::write(root.join("tmux-block-capture"), b"").unwrap();
        let db = Arc::new(Db::open(&root.join("triage.db")).unwrap());
        let ask = ask_card("%12", "ask-call");
        db.insert_card(&ask).unwrap();
        let pane = pane(&root, "%12", "/dev/pts/12", "π ! asking");
        let (answer_sender, answer_receiver) = mpsc::channel();
        let answer_db = Arc::clone(&db);
        let answer_card = ask.clone();
        let answering = thread::spawn(move || {
            answer_sender
                .send(answer(
                    &answer_db,
                    &answer_card,
                    &answer_req(0, Some("preserved API note")),
                    &Launch::default(),
                None,
                ))
                .unwrap();
        });
        wait_for_file(&root.join("tmux-capture-entered"));

        let (started_sender, started_receiver) = mpsc::channel();
        let (reached_sender, reached_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let (tracker_sender, tracker_receiver) = mpsc::channel();
        let tracker_db = Arc::clone(&db);
        let tracker_pane = pane.clone();
        let tracking = thread::spawn(move || {
            started_sender.send(()).unwrap();
            let mut written = 0;
            let succeeded =
                finish_asking_after_recheck(&tracker_db, &tracker_pane, None, &mut written, || {
                    reached_sender.send(()).unwrap();
                    release_receiver.recv().unwrap();
                });
            tracker_sender.send((succeeded, written)).unwrap();
        });
        started_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let _ = reached_receiver.recv_timeout(Duration::from_secs(1));
        fs::write(root.join("tmux-release-capture"), b"").unwrap();

        let answer_result = answer_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        answering.join().unwrap();
        let _ = release_sender.send(());
        let tracker_result = tracker_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        tracking.join().unwrap();
        let swipe = current_swipe(&db, &ask.id);
        let api_note_preserved = answer_result.is_ok()
            && swipe.note.as_deref() == Some("preserved API note")
            && swipe.reason.as_deref() == Some("typed")
            && tracker_result == (true, 0);
        assert!(api_note_preserved);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "tracker_ask_completion_preserves_api_note",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn tracker_creates_plan_card_and_reconciles_closed_and_superseded_proposals() {
    if let Some(root) =
        child_root("tracker_creates_plan_card_and_reconciles_closed_and_superseded_proposals")
    {
        let transcript = root.join("session.jsonl");
        let lines = include_str!("../../tests/fixtures/transcript_propose.jsonl")
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let local = root.join("session").join("local");
        fs::create_dir_all(&local).unwrap();
        let token = "x".repeat(40);
        fs::write(
            local.join("planverify-proof-plan.md"),
            format!("Deploy in order. Bearer {token}"),
        )
        .unwrap();
        write_transcript(&root, "/dev/pts/12", &transcript, &lines);
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-plan-review"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let first_tick = tracker.tick(&db);
        let first = db
            .card("plan:%12:toolu_bdrk_01F9sZDXkT5zNBmzjiCthZTN")
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(first_tick.written, 1);
        assert_eq!(first.title, "Plan ready: planverify-proof");
        assert_eq!(first.source.pane.as_deref(), Some("%12"));
        assert_eq!(first.options[0].label, "Approve");
        assert!(first.options[0].recommended);
        assert_eq!(first.options[1].label, "Refine");
        assert!(first.allow_other && first.answerable);
        assert!(first.body.as_deref().unwrap().contains("Bearer …"));
        assert!(!first.body.as_deref().unwrap().contains(token.as_str()));

        assert_eq!(tracker.tick(&db).written, 0);
        assert_eq!(tracker.proposal_reads, 1);
        let second_call = serde_json::json!({
                "type": "message",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "toolCall", "id": "plan-call-2", "name": "write", "arguments": {"path": "xd://propose", "content": "second plan"}}]
                }
            })
            .to_string();
        let second_result = serde_json::json!({
                "type": "message",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "plan-call-2",
                    "details": {"xdev": {"inner": {"title": "second-plan", "planFilePath": "local://second-plan.md"}}}
                }
            })
            .to_string();
        fs::write(local.join("second-plan.md"), "Second plan.").unwrap();
        let mut newer_lines = lines;
        newer_lines.push(second_call);
        newer_lines.push(second_result);
        write_transcript(&root, "/dev/pts/12", &transcript, &newer_lines);

        let newer_tick = tracker.tick(&db);
        let newer = db.card("plan:%12:plan-call-2").unwrap().unwrap().0;
        assert_eq!(newer_tick.written, 2);
        assert_eq!(newer.title, "Plan ready: second-plan");
        assert_eq!(
            current_swipe(&db, &first.id).reason.as_deref(),
            Some("superseded")
        );
        assert!(db.card(&newer.id).unwrap().unwrap().1.is_none());

        fs::remove_file(root.join("tmux-plan-review")).unwrap();
        fs::write(root.join("tmux-idle-screen"), b"").unwrap();
        let closed_tick = tracker.tick(&db);
        let closed = current_swipe(&db, &newer.id);
        assert_eq!(closed_tick.written, 1);
        assert_eq!(closed.reason.as_deref(), Some("answered in pane"));
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "tracker_creates_plan_card_and_reconciles_closed_and_superseded_proposals",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn tracker_creates_plan_card_when_review_draws_after_proposal() {
    if let Some(root) = child_root("tracker_creates_plan_card_when_review_draws_after_proposal") {
        let transcript = root.join("session.jsonl");
        let lines = include_str!("../../tests/fixtures/transcript_propose.jsonl")
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let local = root.join("session").join("local");
        fs::create_dir_all(&local).unwrap();
        fs::write(local.join("planverify-proof-plan.md"), "Deploy in order.").unwrap();
        write_transcript(&root, "/dev/pts/12", &transcript, &lines);
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-idle-screen"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let before = tracker.tick(&db);
        assert_eq!(before.written, 0);
        assert!(
            db.card("plan:%12:toolu_bdrk_01F9sZDXkT5zNBmzjiCthZTN")
                .unwrap()
                .is_none()
        );

        fs::remove_file(root.join("tmux-idle-screen")).unwrap();
        fs::write(root.join("tmux-plan-review"), b"").unwrap();
        let after = tracker.tick(&db);
        assert_eq!(after.written, 1);
        assert!(
            db.card("plan:%12:toolu_bdrk_01F9sZDXkT5zNBmzjiCthZTN")
                .unwrap()
                .unwrap()
                .1
                .is_none()
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "tracker_creates_plan_card_when_review_draws_after_proposal",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}
#[cfg(unix)]
#[test]
fn tracker_plan_card_ids_use_proposal_tool_call_id_across_transcripts() {
    let name = "tracker_plan_card_ids_use_proposal_tool_call_id_across_transcripts";
    if let Some(root) = child_root(name) {
        let first_transcript = root.join("first.jsonl");
        let second_transcript = root.join("second.jsonl");
        let first_local = root.join("first/local");
        let second_local = root.join("second/local");
        fs::create_dir_all(&first_local).unwrap();
        fs::create_dir_all(&second_local).unwrap();
        fs::write(first_local.join("first-plan.md"), "First plan.").unwrap();
        fs::write(second_local.join("second-plan.md"), "Second plan.").unwrap();
        let proposal_lines = |call_id: &str, title: &str, path: &str| {
            vec![
                serde_json::json!({
                    "type": "message",
                    "message": {
                        "role": "assistant",
                        "content": [{
                            "type": "toolCall",
                            "id": call_id,
                            "name": "write",
                            "arguments": {"path": "xd://propose", "content": title}
                        }]
                    }
                })
                .to_string(),
                serde_json::json!({
                    "type": "message",
                    "message": {
                        "role": "toolResult",
                        "toolCallId": call_id,
                        "details": {"xdev": {"inner": {
                            "title": title,
                            "planFilePath": format!("local://{path}")
                        }}}
                    }
                })
                .to_string(),
            ]
        };
        write_transcript(
            &root,
            "/dev/pts/12",
            &first_transcript,
            &proposal_lines("proposal-first", "first-plan", "first-plan.md"),
        );
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        fs::write(root.join("tmux-plan-review"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        assert_eq!(tracker.tick(&db).written, 1);
        let first_id = "plan:%12:proposal-first";
        assert_eq!(
            db.card(first_id).unwrap().unwrap().0.title,
            "Plan ready: first-plan"
        );

        write_transcript(
            &root,
            "/dev/pts/12",
            &second_transcript,
            &proposal_lines("proposal-second", "second-plan", "second-plan.md"),
        );
        assert_eq!(tracker.tick(&db).written, 2);
        let second_id = "plan:%12:proposal-second";
        let (second, swipe) = db.card(second_id).unwrap().unwrap();
        assert_eq!(second.title, "Plan ready: second-plan");
        assert!(swipe.is_none());
        assert_eq!(
            current_swipe(&db, first_id).reason.as_deref(),
            Some("superseded")
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "tracker_plan_card_ids_use_proposal_tool_call_id_across_transcripts",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn tracker_closes_deferred_plan_when_source_pane_disappears() {
    if let Some(root) = child_root("tracker_closes_deferred_plan_when_source_pane_disappears") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let pane = pane(&root, "%12", "/dev/pts/12", "π > session");
        let card = plan_card(&pane, 5);
        db.insert_card(&card).unwrap();
        db.swipe(&future_later(&card.id)).unwrap();
        fs::write(root.join("tmux-missing-pane"), b"").unwrap();
        let mut tracker = Tracker::default();
        tracker.states.insert(pane.id.clone(), PaneState::Waiting);

        let tick = tracker.tick(&db);
        let swipe = current_swipe(&db, &card.id);
        assert_eq!(tick.written, 1);
        assert_eq!(swipe.action, "done");
        assert_eq!(swipe.reason.as_deref(), Some("pane exited"));
        assert!(!tracker.states.contains_key(&pane.id));
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "tracker_closes_deferred_plan_when_source_pane_disappears",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_retry_reuses_tagged_pane_after_done_swipe_failure() {
    if let Some(root) = child_root("finding_retry_reuses_tagged_pane_after_done_swipe_failure") {
        let db_path = root.join("triage.db");
        let db = Db::open(&db_path).unwrap();
        let card = finding_card(&root);
        db.insert_card(&card).unwrap();
        let failure = install_swipe_failure(&db_path, "fail_done_swipe", "done");
        let request = answer_req(0, Some("review"));

        let first_result = answer(&db, &card, &request, &in_session("session"), None);
        let pending = current_swipe(&db, &card.id);
        let first_launch_count = new_window_count(&root);
        failure
            .execute_batch("DROP TRIGGER fail_done_swipe;")
            .unwrap();

        let retry_result = answer(&db, &card, &request, &in_session("session"), None);
        let completed = current_swipe(&db, &card.id);
        let retry_reused_pane = first_result.is_err()
            && pending.action == "later"
            && pending.reason.as_deref() == Some("launching %42")
            && first_launch_count == 1
            && retry_result.is_ok()
            && completed.action == "done"
            && completed.reason.as_deref() == Some("launched %42")
            && new_window_count(&root) == 1;
        assert!(retry_reused_pane);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_retry_reuses_tagged_pane_after_done_swipe_failure",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_retry_reuses_pending_pane_after_tag_failure_and_rejects_changed_choice() {
    if let Some(root) =
        child_root("finding_retry_reuses_pending_pane_after_tag_failure_and_rejects_changed_choice")
    {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = finding_card(&root);
        db.insert_card(&card).unwrap();
        fs::write(root.join("tmux-fail-first-tag"), b"").unwrap();
        let original = answer_req(0, Some("review"));

        let first_result = answer(&db, &card, &original, &in_session("session"), None);
        let pending_reason = current_swipe(&db, &card.id).reason;
        let changed_option = answer(
            &db,
            &card,
            &answer_req(1, Some("review")),
            &in_session("session"),
        None,
        );
        let changed_note = answer(
            &db,
            &card,
            &answer_req(0, Some("changed")),
            &in_session("session"),
        None,
        );
        let changed_mode = answer(
            &db,
            &card,
            &AnswerReq {
                option: Some(0),
                note: Some("review".to_owned()),
                mode: Some("investigate".to_owned()),
                ..AnswerReq::default()
            },
            &in_session("session"),
        None,
        );
        let launches_before_retry = new_window_count(&root);
        let retry_result = answer(&db, &card, &original, &in_session("session"), None);
        let completed = current_swipe(&db, &card.id);
        let different_choice = |result: &Result<(), String>| {
            result.as_ref().err().map(String::as_str)
                == Some("finding pane was launched with a different answer")
        };
        let retry_contract = first_result.is_err()
            && pending_reason.as_deref() == Some("launching %42")
            && different_choice(&changed_option)
            && different_choice(&changed_note)
            && different_choice(&changed_mode)
            && launches_before_retry == 1
            && retry_result.is_ok()
            && completed.action == "done"
            && completed.option == Some(0)
            && completed.note.as_deref() == Some("review")
            && completed.reason.as_deref() == Some("launched %42")
            && tag_call_count(&root) == 2
            && new_window_count(&root) == 1;
        assert!(retry_contract);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_retry_reuses_pending_pane_after_tag_failure_and_rejects_changed_choice",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_questions_skip_and_keep_record_done_without_a_launch() {
    if let Some(root) = child_root("finding_questions_skip_and_keep_record_done_without_a_launch") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = question_finding_card(&root);
        db.insert_card(&card).unwrap();
        let request = question_req(vec![picked(1), picked(1), picked(0)], Some("review"));
        let result = answer(&db, &card, &request, &in_session("session"), None);
        let swipe = current_swipe(&db, &card.id);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(swipe.action, "done");
        assert_eq!(swipe.option, None);
        assert_eq!(swipe.text.as_deref(), Some("1. Supplier: Skip; 2. Stock: Keep; 3. Note: skip"));
        assert_eq!(swipe.note.as_deref(), Some("review"));
        assert_eq!(swipe.reason.as_deref(), Some("answered"));
        assert_eq!(new_window_count(&root), 0);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_questions_skip_and_keep_record_done_without_a_launch",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_question_answers_are_validated_before_anything_is_recorded() {
    if let Some(root) = child_root("finding_question_answers_are_validated_before_anything_is_recorded") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = question_finding_card(&root);
        db.insert_card(&card).unwrap();
        let rejected = [
            (
                AnswerReq {
                    option: Some(0),
                    answers: vec![picked(0), picked(0), picked(0)],
                    ..AnswerReq::default()
                },
                "finding questions take answers, not a top-level option or text",
            ),
            (question_req(vec![picked(0)], None), "finding answer needs one answer per question"),
            (
                question_req(vec![picked(2), picked(0), picked(0)], None),
                "option is out of range or selects Other",
            ),
            (
                question_req(vec![picked(0), typed("custom"), picked(0)], None),
                "Stock does not take custom text",
            ),
            (
                question_req(vec![picked(0), picked(0), typed("   ")], None),
                "Other requires nonempty text",
            ),
            (
                AnswerReq {
                    mode: Some("investigate".to_owned()),
                    ..question_req(vec![picked(0), picked(0), picked(0)], None)
                },
                "finding questions run act, plan, or plan-into",
            ),
        ];
        for (request, message) in rejected {
            assert_eq!(answer(&db, &card, &request, &in_session("session"), None).unwrap_err(), message);
        }
        assert!(db.card(&card.id).unwrap().unwrap().1.is_none(), "a rejected answer leaves the card queued");
        assert_eq!(new_window_count(&root), 0);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_question_answers_are_validated_before_anything_is_recorded",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_questions_with_actions_launch_one_brief_with_every_decision_and_the_image() {
    if let Some(root) = child_root("finding_questions_with_actions_launch_one_brief_with_every_decision_and_the_image") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = question_finding_card(&root);
        db.insert_card(&card).unwrap();
        let image_path = root.join("shot.png");
        fs::write(&image_path, b"png").unwrap();
        let image = crate::uploads::Upload {
            path: fs::canonicalize(&image_path).unwrap(),
            base64: "iVBORw0KGgo=".to_owned(),
            media_type: "image/png".to_owned(),
            name: "shot.png".to_owned(),
        };
        let request = question_req(vec![picked(0), picked(0), typed("Keep the audit log")], Some("review all"));
        let result = answer(&db, &card, &request, &in_session("session"), Some(&image));
        let swipe = current_swipe(&db, &card.id);
        let briefs: Vec<PathBuf> = fs::read_dir(root.join("briefs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(swipe.action, "done");
        assert_eq!(swipe.option, None);
        assert_eq!(
            swipe.text.as_deref(),
            Some("Act: 1. Supplier: Do as recommended; 2. Stock: Investigate first; 3. Note: Other: Keep the audit log")
        );
        assert_eq!(swipe.note.as_deref(), Some("review all"));
        assert_eq!(swipe.reason.as_deref(), Some("launched %42"));
        assert_eq!(new_window_count(&root), 1);
        let log = fs::read_to_string(root.join("tmux.log")).unwrap_or_default();
        assert!(log.contains(&format!("Attached image: @{}", image.path.display())), "{log}");
        assert_eq!(briefs.len(), 1);
        let brief = fs::read_to_string(&briefs[0]).unwrap();
        assert!(brief.contains("### 1. Supplier\nSupplier?\nDecision: Do as recommended\nDirection: \n"), "{brief}");
        assert!(brief.contains("### 2. Stock\nStock?\nDecision: Investigate first\nDirection: \n"), "{brief}");
        assert!(brief.contains("### 3. Note\nNote?\nDecision: Other\nDirection: Keep the audit log\n"), "{brief}");
        assert!(brief.contains(FINDING_ANSWER_RULES) && brief.contains(ACT_INSTRUCTION), "{brief}");
        assert!(brief.contains("## Direction from the user\nreview all\n"), "{brief}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_questions_with_actions_launch_one_brief_with_every_decision_and_the_image",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_question_retry_reuses_the_launch_only_for_the_same_answers() {
    if let Some(root) = child_root("finding_question_retry_reuses_the_launch_only_for_the_same_answers") {
        fs::write(root.join("tmux-fail-first-tag"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let card = question_finding_card(&root);
        db.insert_card(&card).unwrap();
        let launch = in_session("session");
        let original = question_req(vec![picked(0), picked(0), typed("Keep the audit log")], Some("review all"));
        let other_decision = question_req(vec![picked(0), picked(1), typed("Keep the audit log")], Some("review all"));
        let other_note = question_req(vec![picked(0), picked(0), typed("Keep the audit log")], Some("changed"));
        let first = answer(&db, &card, &original, &launch, None);
        let pending = current_swipe(&db, &card.id);
        let changed_decision = answer(&db, &card, &other_decision, &launch, None);
        let changed_note = answer(&db, &card, &other_note, &launch, None);
        let launches_before_retry = new_window_count(&root);
        let retry = answer(&db, &card, &original, &launch, None);
        let completed = current_swipe(&db, &card.id);
        assert!(first.is_err(), "{first:?}");
        assert_eq!(pending.reason.as_deref(), Some("launching %42"));
        assert_eq!(changed_decision.unwrap_err(), "finding pane was launched with a different answer");
        assert_eq!(changed_note.unwrap_err(), "finding pane was launched with a different answer");
        assert_eq!(launches_before_retry, 1);
        assert!(retry.is_ok(), "{retry:?}");
        assert_eq!(completed.action, "done");
        assert_eq!(completed.option, None);
        assert_eq!(completed.reason.as_deref(), Some("launched %42"));
        assert_eq!(new_window_count(&root), 1);
        assert_eq!(tag_call_count(&root), 2);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_question_retry_reuses_the_launch_only_for_the_same_answers",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn finding_retry_tags_pane_when_pending_swipe_write_fails() {
    if let Some(root) = child_root("finding_retry_tags_pane_when_pending_swipe_write_fails") {
        let db_path = root.join("triage.db");
        let db = Db::open(&db_path).unwrap();
        let card = finding_card(&root);
        db.insert_card(&card).unwrap();
        let failure = install_swipe_failure(&db_path, "fail_pending_swipe", "later");
        let request = answer_req(0, Some("review"));

        let first_result = answer(&db, &card, &request, &in_session("session"), None);
        let no_pending_swipe = db.card(&card.id).unwrap().unwrap().1.is_none();
        let tagged_before_retry = root.join("tmux-tag-%42").exists();
        let first_launch_count = new_window_count(&root);
        failure
            .execute_batch("DROP TRIGGER fail_pending_swipe;")
            .unwrap();

        let retry_result = answer(&db, &card, &request, &in_session("session"), None);
        let completed = db.card(&card.id).unwrap().unwrap().1;
        let retry_reused_tagged_pane = first_result.is_err()
            && no_pending_swipe
            && tagged_before_retry
            && first_launch_count == 1
            && retry_result.is_ok()
            && completed.as_ref().is_some_and(|swipe| {
                swipe.action == "done" && swipe.reason.as_deref() == Some("launched %42")
            })
            && new_window_count(&root) == 1;
        assert!(retry_reused_tagged_pane);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "finding_retry_tags_pane_when_pending_swipe_write_fails",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

fn user_brief_line() -> String {
    serde_json::json!({
            "type": "message",
            "id": "u1",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text":
                    "<file name=\"/home/x/.local/share/triage/briefs/triage-brief-x.md\">\ncarry it out"}]
            }
        })
        .to_string()
}

fn assistant_text_line(id: &str, text: &str) -> String {
    serde_json::json!({
        "type": "message",
        "id": id,
        "message": {
            "role": "assistant",
            "stopReason": "stop",
            "content": [{"type": "text", "text": text}]
        }
    })
    .to_string()
}

#[cfg(unix)]
#[test]
fn interrupted_dispatched_pane_does_not_raise_unreported() {
    if let Some(root) = child_root("interrupted_dispatched_pane_does_not_raise_unreported") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[user_brief_line(), assistant_entry("a1", "aborted", None, None)]);
        fs::write(root.join("tmux-dead"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let written = Tracker::default().tick(&db).written;
        assert_eq!(written, 0);
        assert!(db.queue(events::now_ms()).unwrap().is_empty());
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("interrupted_dispatched_pane_does_not_raise_unreported", scratch.path(), Some(scratch.path()), Some(&bin));
}
#[cfg(unix)]
#[test]
fn stopped_dispatched_pane_still_raises_unreported() {
    if let Some(root) = child_root("stopped_dispatched_pane_still_raises_unreported") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(&root, "/dev/pts/12", &transcript, &[user_brief_line(), assistant_entry("a1", "stop", Some("Still incomplete."), None)]);
        fs::write(root.join("tmux-dead"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        Tracker::default().tick(&db);
        assert!(db.queue(events::now_ms()).unwrap().iter().any(|card| card.title == "Unreported: Still incomplete."));
        return;
    }
    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child("stopped_dispatched_pane_still_raises_unreported", scratch.path(), Some(scratch.path()), Some(&bin));
}

#[test]
fn dead_dispatched_pane_without_report_raises_one_warning_card() {
    if let Some(root) = child_root("dead_dispatched_pane_without_report_raises_one_warning_card") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_brief_line(),
                assistant_text_line("a1", "Weekly audit is live, details follow."),
            ],
        );
        fs::write(root.join("tmux-dead"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let first = tracker.tick(&db).written;
        let second = tracker.tick(&db).written;
        let queue = db.queue(events::now_ms()).unwrap();
        let card = queue.first();
        let raised = first == 1
            && second == 0
            && card.is_some_and(|card| {
                card.id == "exited:%12:a1"
                    && card.title == "Unreported: Weekly audit is live, details follow."
                    && card.level.as_deref() == Some("warn")
                    && card.body.as_deref() == Some("Weekly audit is live, details follow.")
                    && card.refs == [transcript.display().to_string()]
            });
        assert!(raised, "queue: {queue:?}");
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dead_dispatched_pane_without_report_raises_one_warning_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn dead_dispatched_pane_with_report_makes_no_card() {
    if let Some(root) = child_root("dead_dispatched_pane_with_report_makes_no_card") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_brief_line(),
                assistant_text_line("a1", "ENVOY_STATUS: complete\n\nAll done."),
            ],
        );
        fs::write(root.join("tmux-dead"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        assert!(written == 0 && db.queue(events::now_ms()).unwrap().is_empty());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dead_dispatched_pane_with_report_makes_no_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn dead_interactive_pane_makes_no_card() {
    if let Some(root) = child_root("dead_interactive_pane_makes_no_card") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[assistant_text_line("a1", "Just chatting with the user.")],
        );
        fs::write(root.join("tmux-dead"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        assert!(written == 0 && db.queue(events::now_ms()).unwrap().is_empty());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dead_interactive_pane_makes_no_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn omp_title_flicker_makes_no_exit_card() {
    if let Some(root) = child_root("omp_title_flicker_makes_no_exit_card") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_brief_line(),
                assistant_text_line("a1", "Still working, title blinked."),
            ],
        );
        fs::write(root.join("tmux-title-gone"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        assert!(written == 0 && db.queue(events::now_ms()).unwrap().is_empty());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "omp_title_flicker_makes_no_exit_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn dead_pane_whose_session_runs_elsewhere_makes_no_card() {
    if let Some(root) = child_root("dead_pane_whose_session_runs_elsewhere_makes_no_card") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_brief_line(),
                assistant_text_line("a1", "Resumed in another pane, still going."),
            ],
        );
        map_transcript(&root, "/dev/pts/13", &transcript);
        fs::write(root.join("tmux-dead"), b"").unwrap();
        fs::write(root.join("tmux-second-pane"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        let queued = db.queue(events::now_ms()).unwrap();
        let exited = queued
            .iter()
            .filter(|card| card.id.starts_with("exited:"))
            .count();
        // The live pane that resumed the session now earns a plain-reply
        // card; the dead pane itself still raises no exit card.
        assert!(written == 1 && queued.iter().any(|card| card.id == "done:%13:a1") && exited == 0);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "dead_pane_whose_session_runs_elsewhere_makes_no_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
fn new_pty() -> (fs::File, String) {
    use std::os::fd::RawFd;
    use std::os::unix::io::AsRawFd;

    let master = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ptmx")
        .unwrap();
    let mut number: RawFd = 0;
    let result = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGPTN, &mut number) };
    assert_eq!(result, 0, "TIOCGPTN failed");
    (master, format!("/dev/pts/{number}"))
}

#[cfg(unix)]
fn age_crumb(root: &Path, tty: &str) {
    let sessions = root.join(".omp/agent/terminal-sessions");
    let name = Path::new(tty).file_name().unwrap();
    let crumb = sessions.join(format!("pts-{}", name.to_string_lossy()));
    let old = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(60))
        .unwrap();
    fs::File::options()
        .write(true)
        .open(&crumb)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

#[cfg(unix)]
#[test]
fn restored_pane_skips_reply_handled_under_old_pane_id() {
    if let Some(root) = child_root("restored_pane_skips_reply_handled_under_old_pane_id") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[assistant_text_line(
                "a1",
                "Weekly audit is live, details follow.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let old = info_card("done:%7:a1", "%7");
        db.insert_card(&old).unwrap();
        assert!(record_swipe(
            &db,
            &old.id,
            "done",
            Some(0),
            None,
            None,
            None
        ));
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        assert!(
            written == 0 && db.card("done:%12:a1").unwrap().is_none(),
            "queue: {:?}",
            done_cards(&db)
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "restored_pane_skips_reply_handled_under_old_pane_id",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn restored_pane_requeues_reply_closed_only_by_pane_exit() {
    if let Some(root) = child_root("restored_pane_requeues_reply_closed_only_by_pane_exit") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[assistant_text_line(
                "a1",
                "Weekly audit is live, details follow.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let old = info_card("done:%7:a1", "%7");
        db.insert_card(&old).unwrap();
        assert!(record_swipe(
            &db,
            &old.id,
            "done",
            None,
            None,
            None,
            Some("pane exited".to_owned())
        ));
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        assert!(
            written == 1
                && db
                    .card("done:%12:a1")
                    .unwrap()
                    .is_some_and(|(_, swipe)| swipe.is_none()),
            "queue: {:?}",
            done_cards(&db)
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "restored_pane_requeues_reply_closed_only_by_pane_exit",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn restored_pane_takes_over_reply_queued_for_gone_pane() {
    if let Some(root) = child_root("restored_pane_takes_over_reply_queued_for_gone_pane") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[assistant_text_line(
                "a1",
                "Weekly audit is live, details follow.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let old = info_card("done:%7:a1", "%7");
        db.insert_card(&old).unwrap();
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        let old_swipe = current_swipe(&db, &old.id);
        let queue = done_cards(&db);
        assert!(
            written == 2
                && old_swipe.reason.as_deref() == Some("pane exited")
                && queue.len() == 1
                && queue[0].id == "done:%12:a1",
            "queue: {queue:?}, old swipe: {old_swipe:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "restored_pane_takes_over_reply_queued_for_gone_pane",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn reply_queued_for_live_pane_is_not_queued_twice() {
    if let Some(root) = child_root("reply_queued_for_live_pane_is_not_queued_twice") {
        let (_pty, tty13) = new_pty();
        fs::write(root.join("tmux-tty-13"), format!("{tty13}\n")).unwrap();
        let transcript13 = root.join("transcript-13.jsonl");
        write_transcript(
            &root,
            &tty13,
            &transcript13,
            &[assistant_text_line(
                "a1",
                "Queued on the live pane already.",
            )],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let prior = info_card("done:%13:a1", "%13");
        db.insert_card(&prior).unwrap();
        fs::write(root.join("tmux-second-pane"), b"").unwrap();
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[assistant_text_line(
                "a1",
                "Queued on the live pane already.",
            )],
        );
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        let queued = done_cards(&db);
        assert!(
            written == 0
                && db.card("done:%12:a1").unwrap().is_none()
                && queued.iter().any(|card| card.id == "done:%13:a1")
                && db
                    .card("done:%13:a1")
                    .unwrap()
                    .is_some_and(|(_, swipe)| swipe.is_none()),
            "queue: {queued:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "reply_queued_for_live_pane_is_not_queued_twice",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn reused_pane_id_closes_old_pane_card_as_exited() {
    if let Some(root) = child_root("reused_pane_id_closes_old_pane_card_as_exited") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let (_pty, tty12) = new_pty();
        fs::write(root.join("tmux-tty"), format!("{tty12}\n")).unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            &tty12,
            &transcript,
            &[assistant_text_line("a1", "The new pane's own reply.")],
        );
        fs::write(root.join("tmux-second-pane"), b"").unwrap();
        let transcript13 = root.join("transcript-13.jsonl");
        write_transcript(
            &root,
            "/dev/pts/13",
            &transcript13,
            &[assistant_text_line("e1", "Second pane reply.")],
        );
        let db = Db::open(&root.join("triage.db")).unwrap();
        let stale = Card {
            created_ms: 1,
            ..info_card("done:%12:e1", "%12")
        };
        db.insert_card(&stale).unwrap();
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        let stale_swipe = current_swipe(&db, &stale.id);
        let queued = done_cards(&db);
        assert!(
            written == 3
                && stale_swipe.reason.as_deref() == Some("pane exited")
                && queued.iter().any(|card| card.id == "done:%12:a1")
                && queued.iter().any(|card| card.id == "done:%13:e1"),
            "queue: {queued:?}, stale swipe: {stale_swipe:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "reused_pane_id_closes_old_pane_card_as_exited",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn card_of_old_pane_with_reused_id_moves_to_reply_pane() {
    if let Some(root) = child_root("card_of_old_pane_with_reused_id_moves_to_reply_pane") {
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[assistant_text_line("e1", "Pane twelve's reply.")],
        );
        let (_pty, tty13) = new_pty();
        fs::write(root.join("tmux-second-pane"), b"").unwrap();
        fs::write(root.join("tmux-tty-13"), format!("{tty13}\n")).unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let prior = Card {
            created_ms: 1,
            ..info_card("done:%13:e1", "%13")
        };
        db.insert_card(&prior).unwrap();
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        let prior_swipe = current_swipe(&db, &prior.id);
        let queued = done_cards(&db);
        assert!(
            written == 2
                && prior_swipe.reason.as_deref() == Some("pane exited")
                && queued.len() == 1
                && queued[0].id == "done:%12:e1",
            "queue: {queued:?}, prior swipe: {prior_swipe:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "card_of_old_pane_with_reused_id_moves_to_reply_pane",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn handled_exit_card_is_not_raised_again_under_new_pane_id() {
    if let Some(root) = child_root("handled_exit_card_is_not_raised_again_under_new_pane_id") {
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            "/dev/pts/12",
            &transcript,
            &[
                user_brief_line(),
                assistant_text_line("a1", "Unreported work."),
            ],
        );
        fs::write(root.join("tmux-dead"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let old = info_card("exited:%7:a1", "%7");
        db.insert_card(&old).unwrap();
        assert!(record_swipe(
            &db,
            &old.id,
            "done",
            Some(0),
            None,
            None,
            None
        ));
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        assert!(
            written == 0
                && db.card("exited:%12:a1").unwrap().is_none()
                && db.queue(events::now_ms()).unwrap().is_empty(),
            "queue: {:?}",
            db.queue(events::now_ms()).unwrap()
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "handled_exit_card_is_not_raised_again_under_new_pane_id",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn stale_breadcrumb_makes_no_card_until_omp_rewrites_it() {
    if let Some(root) = child_root("stale_breadcrumb_makes_no_card_until_omp_rewrites_it") {
        let (_pty, tty12) = new_pty();
        fs::write(root.join("tmux-tty"), format!("{tty12}\n")).unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            &tty12,
            &transcript,
            &[assistant_text_line(
                "a1",
                "Fresh reply on a reused pts number.",
            )],
        );
        age_crumb(&root, &tty12);
        fs::write(root.join("tmux-waiting"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let first = tracker.tick(&db).written;
        let first_cards = done_cards(&db);
        map_transcript(&root, &tty12, &transcript);
        let second = tracker.tick(&db).written;
        let queued = done_cards(&db);
        assert!(
            first == 0
                && first_cards.is_empty()
                && second == 1
                && queued.len() == 1
                && queued[0].id == "done:%12:a1",
            "first: {first}, queue: {queued:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "stale_breadcrumb_makes_no_card_until_omp_rewrites_it",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn stale_breadcrumb_on_shell_pane_raises_no_exit_card() {
    if let Some(root) = child_root("stale_breadcrumb_on_shell_pane_raises_no_exit_card") {
        let (_pty, tty12) = new_pty();
        fs::write(root.join("tmux-tty"), format!("{tty12}\n")).unwrap();
        let transcript = root.join("transcript.jsonl");
        write_transcript(
            &root,
            &tty12,
            &transcript,
            &[
                user_brief_line(),
                assistant_text_line("a1", "Unreported work."),
            ],
        );
        age_crumb(&root, &tty12);
        fs::write(root.join("tmux-dead"), b"").unwrap();
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();

        let written = tracker.tick(&db).written;
        assert!(
            written == 0 && db.queue(events::now_ms()).unwrap().is_empty(),
            "queue: {:?}",
            db.queue(events::now_ms()).unwrap()
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "stale_breadcrumb_on_shell_pane_raises_no_exit_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[test]
fn session_id_is_read_from_the_transcript_name() {
    assert_eq!(
        session_id(
            "/home/u/.omp/agent/sessions/-tmp/2026-11-08T22-33-32-271Z_01a1bd4b-9f6f-74e6-bf2d-8d1618a31655.jsonl"
        )
        .as_deref(),
        Some("01a1bd4b-9f6f-74e6-bf2d-8d1618a31655")
    );
    assert_eq!(session_id("/tmp/notes.txt"), None);
    assert_eq!(session_id("/tmp/2026-11-08T22-33-32-271Z_not-a-uuid.jsonl"), None);
}

#[cfg(unix)]
#[test]
fn restore_opens_a_window_running_omp_restore_and_resolves_the_card() {
    if let Some(root) =
        child_root("restore_opens_a_window_running_omp_restore_and_resolves_the_card")
    {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let transcript =
            root.join("2026-11-08T22-33-32-271Z_01a1bd4b-9f6f-74e6-bf2d-8d1618a31655.jsonl");
        fs::write(&transcript, b"").unwrap();
        let mut card = info_card("exited:%12:t1", "%12");
        card.refs = vec![transcript.display().to_string()];
        card.source.cwd = root.display().to_string();
        db.insert_card(&card).unwrap();
        let result = restore(&db, &card, &in_session("session"));
        let swipe = current_swipe(&db, &card.id);
        let log = fs::read_to_string(root.join("tmux.log")).unwrap_or_default();
        let launch = log
            .lines()
            .find(|line| line.starts_with("new-window "))
            .map(str::to_owned);

        let refused = info_card("done:%12:a1", "%12");
        db.insert_card(&refused).unwrap();
        let refusal = restore(&db, &refused, &in_session("session"));
        let mut broken = info_card("exited:%12:broken", "%12");
        broken.refs = vec![root.join("transcript.jsonl").display().to_string()];
        db.insert_card(&broken).unwrap();
        let no_id = restore(&db, &broken, &in_session("session"));

        assert!(
            result.is_ok()
                && swipe.text.as_deref() == Some("Restore run")
                && swipe.reason.as_deref() == Some("restored %42")
                && launch
                    .as_deref()
                    .is_some_and(|line| line.contains(" -t session: ")
                        && line.contains(&format!("-c {}", root.display()))
                        && line.contains("omp --restore '01a1bd4b-9f6f-74e6-bf2d-8d1618a31655'")),
            "result: {result:?}, swipe: {swipe:?}, launch: {launch:?}"
        );
        assert!(refusal.is_err() && db.card(&refused.id).unwrap().unwrap().1.is_none());
        assert!(no_id.is_err());
        assert_eq!(new_window_count(&root), 1);
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "restore_opens_a_window_running_omp_restore_and_resolves_the_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

const BRIDGE_SECRET: &str = "test-bridge-secret";
const NOW: i64 = 1_800_000_000_000;
const SID: &str = "session-7f3a9c10";

#[derive(Default)]
struct BridgeData {
    pid: u32,
    launch_id: String,
    syncing: bool,
    sessions: Vec<Value>,
    pending: Vec<Value>,
    states: HashMap<String, Value>,
    events: HashMap<String, Vec<Value>>,
    requests: usize,
    reads: Vec<String>,
    posts: Vec<(String, Value)>,
    // refuse_workspace answers a launch that names a workspace the way a host older
    // than workspaces does.
    refuse_workspace: bool,
    // plan_decisions says the host decides a plan the model wrote as text, the way a
    // host that carries the route does.
    plan_decisions: bool,
    // slow_launch_started is the session the host starts before its launch answer
    // fails. The next launch lists it and then answers the error, which is what a
    // loaded host does: the session exists, the caller gave up.
    slow_launch_started: Option<Value>,
    // launch_error is the error the next launch answers, when it answers one.
    launch_error: Option<String>,
}

impl BridgeData {
    fn route(&mut self, method: Method, uri: Uri, body: &[u8]) -> (StatusCode, Value) {
        self.requests += 1;
        let path = uri.path().to_owned();
        let sid = query_value(&uri, "sessionId");
        if method == Method::GET {
            self.reads.push(uri.to_string());
        }
        match (method.as_str(), path.as_str()) {
            ("GET", "/v1/sessions") => (
                StatusCode::OK,
                json!({"pid": self.pid, "items": self.sessions, "planDecisions": self.plan_decisions}),
            ),
            ("GET", "/v1/pending") if self.syncing => {
                (StatusCode::SERVICE_UNAVAILABLE, json!({"error": "syncing"}))
            }
            ("GET", "/v1/pending") => (StatusCode::OK, json!({"pid": self.pid, "items": self.pending})),
            ("GET", "/v1/state") if !self.listed(&sid) => {
                (StatusCode::NOT_FOUND, json!({"error": "session not found"}))
            }
            ("GET", "/v1/state") => {
                let state = self
                    .states
                    .get(&sid)
                    .cloned()
                    .unwrap_or_else(|| json!({"running": false, "pendingInput": []}));
                (StatusCode::OK, state)
            }
            ("GET", "/v1/events") => {
                let items = self.events.get(&sid).cloned().unwrap_or_default();
                (StatusCode::OK, json!({"items": items}))
            }
            ("POST", _) => {
                let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
                if path == "/v1/launch" && self.refuse_workspace && !value["workspace"].is_null() {
                    self.posts.push((path.clone(), value));
                    return (StatusCode::BAD_REQUEST, json!({"error": "cwd must be an absolute path"}));
                }
                if path == "/v1/launch" {
                    let started = self.slow_launch_started.take();
                    let sid = value["sessionId"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| {
                            started
                                .as_ref()
                                .and_then(|started| started["sessionId"].as_str())
                                .map(str::to_owned)
                        });
                    if let Some(started) = started {
                        self.sessions.push(started);
                    }
                    if let Some(error) = self.launch_error.take() {
                        self.posts.push((path, value));
                        return (
                            StatusCode::BAD_GATEWAY,
                            json!({"error": error, "sessionId": sid}),
                        );
                    }
                }
                let reply = if path == "/v1/launch" {
                    let launched = value["sessionId"].as_str().unwrap_or(&self.launch_id).to_owned();
                    json!({"sessionId": launched})
                } else {
                    json!({"ok": true})
                };
                self.posts.push((path, value));
                (StatusCode::OK, reply)
            }
            _ => (StatusCode::NOT_FOUND, json!({"error": "no route"})),
        }
    }

    fn listed(&self, sid: &str) -> bool {
        self.sessions.iter().any(|item| item["sessionId"].as_str() == Some(sid))
    }
}

fn query_value(uri: &Uri, key: &str) -> String {
    uri.query()
        .unwrap_or_default()
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_default()
        .to_owned()
}

async fn bridge_route(
    State(store): State<Arc<Mutex<BridgeData>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let bearer = format!("Bearer {BRIDGE_SECRET}");
    if headers.get(AUTHORIZATION).and_then(|value| value.to_str().ok()) != Some(bearer.as_str()) {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "unauthorized"}))).into_response();
    }
    let (status, reply) = store.lock().unwrap().route(method, uri, &body);
    (status, axum::Json(reply)).into_response()
}

struct FakeBridge {
    store: Arc<Mutex<BridgeData>>,
    api: String,
    host: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeBridge {
    fn start(root: &Path) -> Self {
        let store = Arc::new(Mutex::new(BridgeData {
            pid: 4242,
            launch_id: "session-new".to_owned(),
            ..BridgeData::default()
        }));
        let (address_tx, address_rx) = mpsc::channel();
        let (stop, stop_rx) = tokio::sync::oneshot::channel();
        let served = Arc::clone(&store);
        let thread = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    address_tx.send(listener.local_addr().unwrap()).unwrap();
                    let router = axum::Router::new().fallback(bridge_route).with_state(served);
                    axum::serve(listener, router)
                        .with_graceful_shutdown(async move {
                            let _ = stop_rx.await;
                        })
                        .await
                        .unwrap();
                });
        });
        let api = format!("http://{}", address_rx.recv().unwrap());
        let host = root.join("dsh-host.json");
        write_host(&host, &api, BRIDGE_SECRET);
        Self { store, api, host, stop: Some(stop), thread: Some(thread) }
    }

    fn host(&self) -> &Path {
        &self.host
    }

    fn launch(&self) -> Launch {
        Launch { engine: Engine::Dsh, dsh_host: Some(self.host.clone()), ..Launch::default() }
    }

    fn edit(&self, edit: impl FnOnce(&mut BridgeData)) {
        edit(&mut self.store.lock().unwrap());
    }

    fn posts(&self, path: &str) -> Vec<Value> {
        self.store
            .lock()
            .unwrap()
            .posts
            .iter()
            .filter(|(route, _)| route == path)
            .map(|(_, body)| body.clone())
            .collect()
    }

    fn reads(&self, prefix: &str) -> usize {
        self.store.lock().unwrap().reads.iter().filter(|read| read.starts_with(prefix)).count()
    }

    fn requests(&self) -> usize {
        self.store.lock().unwrap().requests
    }
}

impl Drop for FakeBridge {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn write_host(path: &Path, api: &str, secret: &str) {
    fs::write(path, json!({"api": api, "secret": secret, "pid": 4242}).to_string()).unwrap();
}

fn dsh_db(scratch: &Scratch) -> Db {
    Db::open(&scratch.path().join("triage.db")).unwrap()
}

fn dsh_tick(tracker: &mut Tracker, db: &Db, host: Option<&Path>, now: i64) -> usize {
    tracker.dsh_tick_at(db, host, now).written
}

fn session(cursor: i64) -> Value {
    json!({
        "sessionId": SID,
        "title": "Gate fix",
        "cwd": "/work/project",
        "running": false,
        "blank": false,
        "parentSessionId": null,
        "updatedAtMs": NOW - 60_000,
        "cursor": cursor,
        "goalPhase": null,
        "planActive": false,
    })
}

/// A session whose plan mode is still on. Its turn writes a plan and stops.
fn plan_session(cursor: i64) -> Value {
    let mut session = session(cursor);
    session["planActive"] = json!(true);
    session
}

fn reply_event(seq: i64, text: &str) -> Value {
    json!({"seq": seq, "type": "assistant/message", "text": text})
}

fn turn_end(seq: i64, reason: &str) -> Value {
    json!({"seq": seq, "type": "turn/end", "reason": reason})
}

fn image_event(seq: i64, id: &str, media: &str, name: &str) -> Value {
    json!({"seq": seq, "type": "tool/result", "images": [{
        "attachmentId": format!("sha256:{id}"),
        "mediaType": media,
        "bytes": 10,
        "width": 640,
        "height": 360,
        "name": name,
    }]})
}

fn question(id: &str, header: &str, text: &str, labels: &[&str], multi_select: bool) -> Value {
    let options: Vec<Value> = labels.iter().map(|label| json!({"label": label})).collect();
    json!({"id": id, "header": header, "question": text, "options": options, "multiSelect": multi_select})
}

fn ask(event: &str, questions: Vec<Value>) -> Value {
    json!({
        "sessionId": SID,
        "eventId": event,
        "callId": format!("call-{event}"),
        "kind": "question",
        "questions": questions,
    })
}

fn plan_review(event: &str, call: &str, detail: &str) -> Value {
    json!({
        "sessionId": SID,
        "eventId": event,
        "callId": call,
        "kind": "plan-review",
        "detail": detail,
        "questions": [question("plan-review", "Plan", "Approve this plan?", &["Approve", "Keep planning"], false)],
    })
}

fn dsh_card(db: &Db, id: &str, kind: Kind) -> Card {
    let card = Card {
        id: id.to_owned(),
        kind,
        priority: 1,
        created_ms: events::now_ms(),
        source: Source {
            uri: format!("dsh://session/{SID}"),
            label: "dsh Gate fix".to_owned(),
            cwd: "/work/project".to_owned(),
            pane: None,
            target: None,
        },
        title: "Gate fix is done".to_owned(),
        star: Star::default(),
        options: vec![CardOption { label: "Acknowledge".to_owned(), ..CardOption::default() }],
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: Some("Report".to_owned()),
        refs: vec![format!("dsh://session/{SID}")],
        level: Some("info".to_owned()),
        tool_call_id: None,
        report_due: true,
        images: Vec::new(),
        engine: Some("dsh".to_owned()),
    };
    db.insert_card(&card).unwrap();
    card
}

#[test]
fn completed_dsh_turn_queues_one_report_card_and_polls_on_a_cadence() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(4)];
        data.events.insert(
            SID.to_owned(),
            vec![
                reply_event(2, "Writing the file."),
                reply_event(3, "ENVOY_STATUS: complete\nGoal ok\n\nRecommended action: Run the autorules sweep."),
                turn_end(4, "completed"),
            ],
        );
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    let (card, swipe) = db.card(&format!("done:dsh:{SID}:4")).unwrap().unwrap();
    assert!(swipe.is_none());
    assert_eq!(card.engine.as_deref(), Some("dsh"));
    assert_eq!(card.source.uri, format!("dsh://session/{SID}"));
    assert_eq!(card.source.label, "dsh Gate fix");
    assert!(card.title.starts_with("Done:"), "{card:?}");
    assert!(card.star.action.is_empty());
    assert_eq!(card.body.as_deref(), Some("Goal ok\n\nRecommended action: Run the autorules sweep."));
    assert!(card.body.as_deref().is_some_and(|body| body.contains("Goal ok")), "{card:?}");
    assert_eq!(bridge.reads("/v1/events"), 1);
    let requests = bridge.requests();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 1_000), 0);
    assert_eq!(bridge.requests(), requests);
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 3_000), 0);
    assert_eq!(bridge.reads("/v1/events"), 1);
    assert_eq!(done_cards(&db).len(), 1);
}

#[test]
fn dsh_agent_done_report_closed_with_none_earns_a_done_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(4)];
        data.events.insert(
            SID.to_owned(),
            vec![
                reply_event(2, "Writing the file."),
                reply_event(3, "ENVOY_STATUS: agent-done\nBlock key shipped and pushed.\n\nRecommended action: none."),
                turn_end(4, "completed"),
            ],
        );
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    let (card, swipe) = db.card(&format!("done:dsh:{SID}:4")).unwrap().unwrap();
    assert!(swipe.is_none());
    assert_eq!(card.title, "Done: Block key shipped and pushed.");
    assert_eq!(card.level.as_deref(), Some("info"));
    assert!(card.star.action.is_empty(), "action: {}", card.star.action);
}

#[test]
fn dsh_report_card_carries_its_turn_images() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let hash = "c".repeat(64);
    let old = "d".repeat(64);
    bridge.edit(|data| {
        data.sessions = vec![session(5)];
        data.events.insert(
            SID.to_owned(),
            vec![
                image_event(1, &old, "image/png", "old.png"),
                turn_end(2, "completed"),
                reply_event(3, "Writing the file."),
                image_event(4, &hash, "image/webp", "shot.webp"),
                turn_end(5, "completed"),
            ],
        );
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 1);
    let (card, _) = db.card(&format!("done:dsh:{SID}:5")).unwrap().unwrap();
    assert_eq!(card.images.len(), 1, "{card:?}");
    assert_eq!(card.images[0].id, hash);
    assert_eq!(card.images[0].media_type, "image/webp");
    assert_eq!(card.images[0].name.as_deref(), Some("shot.webp"));
    assert_eq!(card.images[0].width, Some(640));
    assert_eq!(card.images[0].height, Some(360));
}

#[test]
fn plan_mode_turn_reports_the_plan_ready() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let plan = "# Keep autorules sessions out\n\n## Goal\n\nNo cards.";
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        let hash = "e".repeat(64);
        data.events.insert(
            SID.to_owned(),
            vec![
                reply_event(2, "Reading the tracker."),
                image_event(3, &hash, "image/png", "plan.png"),
                reply_event(4, plan),
                turn_end(5, "completed"),
            ],
        );
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    let (card, swipe) = db.card(&format!("done:dsh:{SID}:5")).unwrap().unwrap();
    assert!(swipe.is_none());
    assert_eq!(card.kind, Kind::Info);
    assert_eq!(card.title, "Plan ready: Keep autorules sessions out");
    assert_eq!(card.body.as_deref(), Some(plan));
    assert!(card.star.action.is_empty(), "{card:?}");
    assert!(!card.report_due, "{card:?}");
    assert_eq!(card.images.len(), 1, "{card:?}");
    assert_eq!(done_cards(&db).len(), 1);
}

#[test]
fn plan_mode_turn_with_a_status_line_does_not_read_done() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        data.events.insert(
            SID.to_owned(),
            vec![
                reply_event(3, "ENVOY_STATUS: complete\nPlanned the split. No code changed."),
                turn_end(4, "completed"),
            ],
        );
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 1);
    let (card, _) = db.card(&format!("done:dsh:{SID}:4")).unwrap().unwrap();
    assert_eq!(card.title, "Plan ready: Planned the split. No code changed.");
    assert!(!card.title.starts_with("Done:"), "{card:?}");
}

#[test]
fn plan_mode_turn_without_a_heading_takes_the_reply_line() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        data.events.insert(
            SID.to_owned(),
            vec![
                reply_event(3, "A plan without a title\n- step one"),
                turn_end(4, "completed"),
            ],
        );
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 1);
    let (card, _) = db.card(&format!("done:dsh:{SID}:4")).unwrap().unwrap();
    assert_eq!(card.title, "Plan ready: A plan without a title");
    assert_eq!(card.body.as_deref(), Some("A plan without a title\n- step one"));
}

#[test]
fn plan_mode_error_turn_keeps_its_stop_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        data.events.insert(
            SID.to_owned(),
            vec![
                json!({"seq": 4, "type": "turn/end", "reason": "error", "error": "400: bad request"}),
            ],
        );
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 1);
    let (card, _) = db.card(&format!("done:dsh:{SID}:4")).unwrap().unwrap();
    assert!(card.title.starts_with("Stopped with error"), "{card:?}");
    assert_eq!(card.level.as_deref(), Some("warn"));
}

/// A turn that ended in plan mode on a host that decides plans: the card the session waits on.
fn plan_decided_card(bridge: &FakeBridge, db: &Db) -> Card {
    bridge.edit(|data| data.plan_decisions = true);
    assert_eq!(dsh_tick(&mut Tracker::default(), db, Some(bridge.host()), NOW), 1);
    db.card(&format!("plan-ready:dsh:{SID}:4")).unwrap().expect("plan card").0
}

#[test]
fn a_host_that_decides_plans_turns_a_plan_mode_turn_into_a_plan_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let plan = "# Keep autorules sessions out\n\n## Goal\n\nNo cards.";
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        data.events.insert(
            SID.to_owned(),
            vec![
                reply_event(2, "Reading the tracker."),
                reply_event(3, plan),
                turn_end(4, "completed"),
            ],
        );
    });
    let card = plan_decided_card(&bridge, &db);
    assert_eq!(card.kind, Kind::Plan);
    assert_eq!(card.title, "Plan ready: Keep autorules sessions out");
    assert_eq!(card.body.as_deref(), Some(plan));
    let labels: Vec<&str> = card.options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels, ["Approve", "Refine"]);
    assert!(card.options[0].recommended && card.answerable && card.allow_other);
    assert!(card.tool_call_id.is_none() && !card.report_due, "{card:?}");
    assert_eq!(card.engine.as_deref(), Some("dsh"));
    assert_eq!(done_cards(&db).len(), 0, "a plan is not a finished report");
}

#[test]
fn the_plan_card_approve_takes_the_execution_model_and_leaves_plan_mode() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        data.events.insert(SID.to_owned(), vec![reply_event(3, "# Ship it\n\nDo it."), turn_end(4, "completed")]);
    });
    let card = plan_decided_card(&bridge, &db);
    answer(
        &db,
        &card,
        &AnswerReq {
            option: Some(0),
            executor: Some("deepseek-v4-pro".to_owned()),
            executor_thinking: Some("max".to_owned()),
            ..AnswerReq::default()
        },
        &bridge.launch(),
        None,
    )
    .unwrap();
    assert_eq!(
        bridge.posts("/v1/plan"),
        vec![json!({
            "sessionId": SID,
            "decision": "approve",
            "feedback": null,
            "executor": "deepseek-v4-pro",
            "effort": "max",
        })]
    );
    assert_eq!(swipe_reason(&db, &card.id).as_deref(), Some("sent to dsh"));
}

#[test]
fn the_plan_card_refine_sends_its_feedback_back_to_planning() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        data.events.insert(SID.to_owned(), vec![reply_event(3, "# Ship it\n\nDo it."), turn_end(4, "completed")]);
    });
    let card = plan_decided_card(&bridge, &db);
    answer(
        &db,
        &card,
        &AnswerReq { option: Some(1), text: Some("Add a rollback step.".to_owned()), ..AnswerReq::default() },
        &bridge.launch(),
        None,
    )
    .unwrap();
    assert_eq!(
        bridge.posts("/v1/plan"),
        vec![json!({
            "sessionId": SID,
            "decision": "refine",
            "feedback": "Add a rollback step.",
            "executor": null,
            "effort": null,
        })]
    );
}

#[test]
fn a_later_turn_of_the_session_supersedes_its_plan_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        data.events.insert(SID.to_owned(), vec![reply_event(3, "# Ship it"), turn_end(4, "completed")]);
    });
    let card = plan_decided_card(&bridge, &db);
    bridge.edit(|data| {
        data.sessions = vec![session(8)];
        data.events.get_mut(SID).unwrap().extend(vec![
            reply_event(7, "ENVOY_STATUS: complete\nShipped."),
            turn_end(8, "completed"),
        ]);
    });
    dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW + 3_000);
    assert_eq!(swipe_reason(&db, &card.id).as_deref(), Some("superseded"));
    assert!(db.card(&format!("done:dsh:{SID}:8")).unwrap().unwrap().1.is_none());
}

#[test]
fn a_plan_card_closes_when_its_session_leaves_the_host() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![plan_session(4)];
        data.events.insert(SID.to_owned(), vec![reply_event(3, "# Ship it"), turn_end(4, "completed")]);
    });
    let card = plan_decided_card(&bridge, &db);
    bridge.edit(|data| data.sessions.clear());
    dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW + 3_000);
    assert_eq!(swipe_reason(&db, &card.id).as_deref(), Some("session closed"));
}

#[test]
fn aborted_dsh_turn_queues_no_report_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(3)];
        data.events.insert(
            SID.to_owned(),
            vec![reply_event(2, "Stopped by the user."), turn_end(3, "aborted")],
        );
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 0);
    assert!(done_cards(&db).is_empty());
}

#[test]
fn active_dsh_goal_holds_its_report_until_the_grace_ends() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let mut goal = session(3);
    goal["goalPhase"] = json!("active");
    bridge.edit(|data| {
        data.sessions = vec![goal];
        data.events.insert(
            SID.to_owned(),
            vec![reply_event(2, "Gate attempt 1 still in flight."), turn_end(3, "completed")],
        );
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 0);
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 10_000), 0);
    assert!(done_cards(&db).is_empty());
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 31_000), 1);
    let ids: Vec<String> = done_cards(&db).into_iter().map(|card| card.id).collect();
    assert_eq!(ids, vec![format!("done:dsh:{SID}:3")]);
}

/// A tracker that leaves the named workspaces under `root` out of the inbox.
fn ignored_tracker(root: &Path, names: &[&str]) -> Tracker {
    let mut tracker = Tracker::default();
    tracker.set_dsh_ignore(crate::dsh::Ignore::new(
        root.to_path_buf(),
        names.iter().map(|name| (*name).to_owned()).collect(),
    ));
    tracker
}

/// A session in the dsh workspace `name` under `root`.
fn workspace_session(cursor: i64, root: &Path, name: &str) -> Value {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let mut session = session(cursor);
    session["cwd"] = json!(dir.display().to_string());
    session
}

#[test]
fn report_from_an_ignored_workspace_queues_no_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let root = scratch.path().join("workspaces");
    let finished = workspace_session(4, &root, "autorules");
    bridge.edit(|data| {
        data.sessions = vec![finished];
        data.events.insert(
            SID.to_owned(),
            vec![reply_event(3, "AUTORULES-VERDICT keep"), turn_end(4, "completed")],
        );
    });
    let mut tracker = ignored_tracker(&root, &["autorules"]);
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 0);
    assert!(done_cards(&db).is_empty());
    assert_eq!(bridge.reads("/v1/events"), 0, "an ignored session is never read");
}

#[test]
fn report_from_another_workspace_still_queues_a_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let root = scratch.path().join("workspaces");
    let finished = workspace_session(4, &root, "triage");
    bridge.edit(|data| {
        data.sessions = vec![finished];
        data.events.insert(
            SID.to_owned(),
            vec![reply_event(3, "ENVOY_STATUS: complete"), turn_end(4, "completed")],
        );
    });
    let mut tracker = ignored_tracker(&root, &["autorules"]);
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    let ids: Vec<String> = done_cards(&db).into_iter().map(|card| card.id).collect();
    assert_eq!(ids, vec![format!("done:dsh:{SID}:4")]);
}

#[test]
fn question_in_an_ignored_workspace_queues_no_ask_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let root = scratch.path().join("workspaces");
    let asking = workspace_session(0, &root, "autorules");
    bridge.edit(|data| {
        data.sessions = vec![asking];
        data.pending = vec![ask("ev-1", vec![question("q1", "Color", "Which color?", &["Red"], false)])];
    });
    let mut tracker = ignored_tracker(&root, &["autorules"]);
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 0);
    assert!(db.card(&format!("ask:dsh:{SID}:ev-1")).unwrap().is_none());
}

#[test]
fn empty_ignore_list_tracks_every_workspace() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let root = scratch.path().join("workspaces");
    let finished = workspace_session(4, &root, "autorules");
    bridge.edit(|data| {
        data.sessions = vec![finished];
        data.events.insert(
            SID.to_owned(),
            vec![reply_event(3, "AUTORULES-VERDICT keep"), turn_end(4, "completed")],
        );
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
}

#[test]
fn dsh_question_queues_an_ask_card_with_its_options() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![ask("ev-1", vec![question("q1", "Color", "Which color?", &["Red", "Blue"], false)])];
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 1);
    let card = db.card(&format!("ask:dsh:{SID}:ev-1")).unwrap().unwrap().0;
    assert_eq!(card.kind, Kind::Ask);
    assert_eq!(card.title, "Which color?");
    let labels: Vec<&str> = card.options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels, ["Red", "Blue"]);
    assert!(card.answerable && card.allow_other, "{card:?}");
    assert_eq!(card.tool_call_id.as_deref(), Some("ev-1"));
    assert_eq!(card.engine.as_deref(), Some("dsh"));
    assert!(card.questions.is_empty());
}

#[test]
fn dsh_multi_question_ask_lists_each_question_by_its_header() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![ask(
            "ev-2",
            vec![
                question("q1", "Color", "Which color?", &["Red"], false),
                question("q2", "Size", "Which size?", &["S", "L"], false),
            ],
        )];
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 1);
    let card = db.card(&format!("ask:dsh:{SID}:ev-2")).unwrap().unwrap().0;
    let labels: Vec<&str> = card.questions.iter().map(|asked| asked.label.as_str()).collect();
    assert_eq!(labels, ["Color", "Size"]);
    assert_eq!(card.questions[1].question, "Which size?");
    let sizes: Vec<&str> = card.questions[1].options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(sizes, ["S", "L"]);
}

#[test]
fn dsh_multi_select_question_is_not_answerable_from_the_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![ask("ev-3", vec![question("q1", "Tags", "Which tags?", &["a", "b"], true)])];
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 1);
    let card = db.card(&format!("ask:dsh:{SID}:ev-3")).unwrap().unwrap().0;
    assert!(!card.answerable);
}

#[test]
fn dsh_question_answered_in_the_web_ui_resolves_its_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![ask("ev-1", vec![question("q1", "Color", "Which color?", &["Red", "Blue"], false)])];
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    bridge.edit(|data| data.pending.clear());
    dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 3_000);
    assert_eq!(swipe_reason(&db, &format!("ask:dsh:{SID}:ev-1")).as_deref(), Some("answered in dsh"));
}

#[test]
fn dsh_host_restart_resolves_open_questions_as_restarted() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![ask("ev-1", vec![question("q1", "Color", "Which color?", &["Red", "Blue"], false)])];
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    bridge.edit(|data| {
        data.pid = 4343;
        data.pending.clear();
    });
    dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 3_000);
    assert_eq!(swipe_reason(&db, &format!("ask:dsh:{SID}:ev-1")).as_deref(), Some("dsh host restarted"));
}

#[test]
fn dsh_bridge_syncing_resolves_no_open_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![ask("ev-1", vec![question("q1", "Color", "Which color?", &["Red", "Blue"], false)])];
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    bridge.edit(|data| {
        data.syncing = true;
        data.pending.clear();
    });
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 3_000), 0);
    assert!(db.card(&format!("ask:dsh:{SID}:ev-1")).unwrap().unwrap().1.is_none());
}

#[test]
fn dsh_plan_review_queues_a_plan_card_with_its_detail_redacted() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let key = format!("{}{}", "AKIA", "1234567890ABCDEF");
    let detail = format!("# Ship the importer\n\nUpload with key {key} and retry.");
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![plan_review("ev-p1", "call-p1", &detail)];
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 1);
    let card = db.card(&format!("plan:dsh:{SID}:call-p1")).unwrap().unwrap().0;
    assert_eq!(card.kind, Kind::Plan);
    assert_eq!(card.title, "Plan ready: Ship the importer");
    let labels: Vec<&str> = card.options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels, ["Approve", "Refine"]);
    assert!(card.options[0].recommended && card.answerable && card.allow_other);
    assert_eq!(card.tool_call_id.as_deref(), Some("ev-p1"));
    let body = card.body.unwrap_or_default();
    assert!(body.contains("Upload with key") && !body.contains(&key), "{body}");
}

#[test]
fn newer_dsh_plan_review_supersedes_the_older_card() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![plan_review("ev-p1", "call-p1", "# Ship the importer")];
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    bridge.edit(|data| data.pending = vec![plan_review("ev-p2", "call-p2", "# Ship the importer v2")]);
    dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 3_000);
    assert_eq!(swipe_reason(&db, &format!("plan:dsh:{SID}:call-p1")).as_deref(), Some("superseded"));
    assert!(db.card(&format!("plan:dsh:{SID}:call-p2")).unwrap().unwrap().1.is_none());
}

#[test]
fn dsh_answer_ask_posts_the_picked_label_or_typed_text() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![
            ask("ev-1", vec![question("q1", "Color", "Which color?", &["Red", "Blue"], false)]),
            ask("ev-2", vec![question("q1", "Color", "Which color?", &["Red", "Blue"], false)]),
        ];
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(bridge.host()), NOW), 2);
    let picked = db.card(&format!("ask:dsh:{SID}:ev-1")).unwrap().unwrap().0;
    let typed = db.card(&format!("ask:dsh:{SID}:ev-2")).unwrap().unwrap().0;
    let launch = bridge.launch();
    answer(&db, &picked, &AnswerReq { option: Some(1), ..AnswerReq::default() }, &launch, None).unwrap();
    answer(&db, &typed, &AnswerReq { text: Some("Green".to_owned()), ..AnswerReq::default() }, &launch, None).unwrap();
    assert_eq!(
        bridge.posts("/v1/answer"),
        vec![
            json!({"eventId": "ev-1", "value": {"answers": [{"id": "q1", "selected": ["Blue"]}]}, "executor": null, "effort": null}),
            json!({"eventId": "ev-2", "value": {"answers": [{"id": "q1", "selected": [], "custom": "Green"}]}, "executor": null, "effort": null}),
        ]
    );
    assert_eq!(swipe_reason(&db, &picked.id).as_deref(), Some("sent to dsh"));
}

#[test]
fn dsh_plan_approve_takes_the_execution_model_and_refine_sends_feedback() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![plan_review("ev-p1", "call-p1", "# Ship the importer")];
    });
    let mut tracker = Tracker::default();
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW), 1);
    let first = db.card(&format!("plan:dsh:{SID}:call-p1")).unwrap().unwrap().0;
    let launch = bridge.launch();
    let refused = answer(
        &db,
        &first,
        &AnswerReq { option: Some(0), executor: Some("@smol".to_owned()), ..AnswerReq::default() },
        &launch,
    None,
    );
    assert_eq!(refused, Err("unknown dsh model @smol".to_owned()));
    assert!(bridge.posts("/v1/answer").is_empty());
    assert!(db.card(&first.id).unwrap().unwrap().1.is_none());
    answer(
        &db,
        &first,
        &AnswerReq {
            option: Some(0),
            executor: Some("deepseek-v4-pro".to_owned()),
            executor_thinking: Some("off".to_owned()),
            ..AnswerReq::default()
        },
        &launch,
    None,
    )
    .unwrap();
    assert_eq!(
        bridge.posts("/v1/answer"),
        vec![json!({"eventId": "ev-p1", "value": {"answers": [{"id": "plan-review", "selected": ["Approve"]}]}, "executor": "deepseek-v4-pro", "effort": "off"})]
    );
    bridge.edit(|data| data.pending = vec![plan_review("ev-p2", "call-p2", "# Ship the importer v2")]);
    assert_eq!(dsh_tick(&mut tracker, &db, Some(bridge.host()), NOW + 3_000), 1);
    let second = db.card(&format!("plan:dsh:{SID}:call-p2")).unwrap().unwrap().0;
    answer(
        &db,
        &second,
        &AnswerReq { option: Some(1), text: Some("Add a rollback step.".to_owned()), ..AnswerReq::default() },
        &launch,
    None,
    )
    .unwrap();
    assert_eq!(
        bridge.posts("/v1/answer")[1],
        json!({"eventId": "ev-p2", "value": {"answers": [{"id": "plan-review", "selected": ["Keep planning"], "custom": "Add a rollback step."}]}, "executor": null, "effort": null})
    );
}

#[test]
fn dsh_session_launch_posts_plan_into_with_its_model_effort_and_executor() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let launch = Launch {
        model: Some("deepseek-v4-pro".to_owned()),
        thinking: Some("high".to_owned()),
        ..bridge.launch()
    };
    let pane = start_session(
        &db,
        "Write the report\nwith detail",
        "/work/project",
        &launch,
        Start::PlanInto(Some("deepseek-flash"), Some("max")),
    None,
    );
    assert_eq!(pane, Ok("dsh:session-new".to_owned()));
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 1);
    let body = &launches[0];
    assert_eq!(body["cwd"], "/work/project");
    assert_eq!(body["title"], "Write the report");
    assert_eq!(body["model"], "deepseek-v4-pro");
    assert_eq!(body["effort"], "high");
    assert_eq!(body["mode"], "plan-into");
    assert_eq!(body["executor"], "deepseek-flash");
    assert_eq!(body["executorEffort"], "max");
    assert!(body.get("sessionId").is_none(), "{body}");
    assert!(
        body["prompt"].as_str().is_some_and(|prompt| prompt.starts_with("Read the brief at ") && prompt.ends_with(" and carry it out.")),
        "{body}"
    );
}

#[test]
fn dsh_goal_launch_without_a_model_leaves_the_host_default() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let pane = start_session(&db, "Ship it", "/work/project", &bridge.launch(), Start::Goal, None);
    assert_eq!(pane, Ok("dsh:session-new".to_owned()));
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["mode"], "goal");
    // No model sent: the bridge takes the host's catalog default, so a model
    // name that went stale in triage never reaches a selection.
    assert!(launches[0]["model"].is_null(), "{}", launches[0]);
    assert!(launches[0]["effort"].is_null());
    assert!(launches[0]["executor"].is_null());
}

#[test]
fn dsh_launch_adopts_the_session_a_timed_out_launch_started() {
    let scratch = Scratch::new();
    let bridge = FakeBridge::start(scratch.path());
    let client = dsh::connect(Some(bridge.host()), dsh::TICK_TIMEOUT).unwrap();
    let mut started = session(1);
    started["sessionId"] = json!("session-slow");
    started["title"] = json!("Ship it");
    bridge.edit(|data| data.sessions.push(started));
    // The host created the session, then took too long to answer. Reporting that as a
    // failure made a retry start the same brief a second time, so the launch adopts it.
    assert_eq!(
        dsh_track::adopt_started(
            &client,
            &HashSet::new(),
            "Ship it",
            "dsh bridge unreachable: timeout: global".to_owned(),
        ),
        Ok("dsh:session-slow".to_owned())
    );
    // The bridge's own error answer is final: it archived the session it created, so
    // adopting one would report a launch that never started.
    let reads = bridge.reads("/v1/sessions");
    assert_eq!(
        dsh_track::adopt_started(&client, &HashSet::new(), "Ship it", "workspace is invalid".to_owned()),
        Err("workspace is invalid".to_owned())
    );
    assert_eq!(bridge.reads("/v1/sessions"), reads);
}

#[test]
fn dsh_launch_reports_an_error_that_started_no_session() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.launch_error = Some("workspace is invalid".to_owned());
    });
    assert_eq!(
        start_session(&db, "Ship it", "/work/project", &bridge.launch(), Start::Goal, None),
        Err("workspace is invalid".to_owned())
    );
    assert_eq!(bridge.posts("/v1/launch").len(), 1);
}

#[test]
fn dsh_launch_archives_the_session_a_refused_launch_created() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        let mut started = session(1);
        started["sessionId"] = json!("session-failed");
        started["title"] = json!("Ship it");
        data.slow_launch_started = Some(started);
        data.launch_error = Some("session/model-unavailable: Select an available model".to_owned());
    });
    // The bridge names the session it created before the launch failed. Keeping it would
    // leave a duplicate behind the retry, so the failed launch archives it.
    assert_eq!(
        start_session(&db, "Ship it", "/work/project", &bridge.launch(), Start::Goal, None),
        Err("session/model-unavailable: Select an available model".to_owned())
    );
    let archived = bridge.posts("/v1/archive");
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0]["sessionId"], "session-failed");
}

#[test]
fn dsh_launch_without_a_host_file_says_dsh_is_not_running() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let launch = Launch { engine: Engine::Dsh, ..Launch::default() };
    assert_eq!(
        start_session(&db, "Ship it", "/work/project", &launch, Start::Goal, None),
        Err("dsh is not running".to_owned())
    );
}

#[test]
fn dsh_act_on_an_idle_session_prompts_it_with_the_brief() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| data.sessions = vec![session(4)]);
    let card = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    dispatch(&db, &card, DispatchMode::Act, None, &bridge.launch(), None, None, None).unwrap();
    let prompts = bridge.posts("/v1/prompt");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["sessionId"], SID);
    assert!(prompts[0]["text"].as_str().is_some_and(|text| text.starts_with("Read the brief at ")), "{prompts:?}");
    assert!(bridge.posts("/v1/launch").is_empty());
    assert_eq!(swipe_reason(&db, &card.id), Some(format!("launched dsh:{SID}")));
}

#[test]
fn dsh_dispatch_carries_the_image() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| data.sessions = vec![session(4)]);
    let image = crate::uploads::Upload {
        path: scratch.path().join("shot.png"),
        base64: "iVBORw0KGgo=".to_owned(),
        media_type: "image/png".to_owned(),
        name: "shot.png".to_owned(),
    };
    let card = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    dispatch(
        &db,
        &card,
        DispatchMode::Act,
        None,
        &bridge.launch(),
        None,
        None,
        Some(&image),
    )
    .unwrap();
    let prompts = bridge.posts("/v1/prompt");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["image"]["mediaType"], "image/png");
    assert_eq!(prompts[0]["image"]["data"], "iVBORw0KGgo=");
    assert_eq!(prompts[0]["image"]["name"], "shot.png");

    bridge.edit(|data| {
        data.states.insert(SID.to_owned(), json!({"running": true, "pendingInput": []}));
    });
    let card = dsh_card(&db, &format!("done:dsh:{SID}:5"), Kind::Info);
    dispatch(
        &db,
        &card,
        DispatchMode::Act,
        None,
        &bridge.launch(),
        None,
        None,
        Some(&image),
    )
    .unwrap();
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["image"]["mediaType"], "image/png");
    assert_eq!(launches[0]["image"]["data"], "iVBORw0KGgo=");
}

#[test]
fn dsh_plan_on_an_idle_session_relaunches_it_in_plan_mode() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| data.sessions = vec![session(4)]);
    let card = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    dispatch(&db, &card, DispatchMode::Plan, None, &bridge.launch(), None, None, None).unwrap();
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["sessionId"], SID);
    assert_eq!(launches[0]["mode"], "plan");
    assert!(bridge.posts("/v1/prompt").is_empty());
    assert_eq!(swipe_reason(&db, &card.id), Some(format!("launched dsh:{SID}")));
}

#[test]
fn dsh_plan_into_dispatch_starts_a_new_session() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| data.sessions = vec![session(4)]);
    let card = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    dispatch(&db, &card, DispatchMode::PlanInto, None, &bridge.launch(), Some("deepseek-flash"), None, None).unwrap();
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 1);
    assert!(launches[0].get("sessionId").is_none(), "{launches:?}");
    assert_eq!(launches[0]["mode"], "plan-into");
    assert_eq!(launches[0]["executor"], "deepseek-flash");
    assert!(bridge.posts("/v1/prompt").is_empty());
}

#[test]
fn dsh_dispatch_to_a_running_session_starts_a_new_goal_session() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(4)];
        data.states.insert(SID.to_owned(), json!({"running": true, "pendingInput": []}));
    });
    let card = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    dispatch(&db, &card, DispatchMode::Act, None, &bridge.launch(), None, None, None).unwrap();
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 1);
    assert!(launches[0].get("sessionId").is_none(), "{launches:?}");
    assert_eq!(launches[0]["mode"], "goal");
    assert!(bridge.posts("/v1/prompt").is_empty());
}

#[test]
fn dsh_close_archives_the_session_and_resolves_its_other_cards() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| data.sessions = vec![session(4)]);
    let done = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    let ask_card = dsh_card(&db, &format!("ask:dsh:{SID}:ev-1"), Kind::Ask);
    close(&db, &done, Some("thanks"), &bridge.launch()).unwrap();
    assert_eq!(bridge.posts("/v1/archive"), vec![json!({"sessionId": SID})]);
    assert!(bridge.posts("/v1/cancel").is_empty());
    assert_eq!(swipe_reason(&db, &done.id), Some(format!("closed dsh:{SID}")));
    assert_eq!(swipe_reason(&db, &ask_card.id), Some(format!("closed dsh:{SID}")));
}

#[test]
fn dsh_report_request_is_refused_while_the_session_runs() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(4)];
        data.states.insert(SID.to_owned(), json!({"running": true, "pendingInput": []}));
    });
    let card = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    assert_eq!(request_report(&db, &card, &bridge.launch()), Err("session is busy".to_owned()));
    assert!(bridge.posts("/v1/prompt").is_empty());
    assert!(db.card(&card.id).unwrap().unwrap().1.is_none());
}

#[test]
fn dsh_report_request_prompts_an_idle_session_for_its_final_report() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| data.sessions = vec![session(4)]);
    let card = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    assert_eq!(request_report(&db, &card, &bridge.launch()), Ok(()));
    let prompts = bridge.posts("/v1/prompt");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["sessionId"], SID);
    assert_eq!(prompts[0]["text"], REQUEST_REPORT_TEXT);
    assert_eq!(swipe_reason(&db, &card.id), Some(format!("report requested dsh:{SID}")));
}

#[test]
fn answer_finding_reuses_the_dsh_session_it_already_launched() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| data.sessions = vec![session(4)]);
    let finding = Card {
        id: "finding:dsh-gate".to_owned(),
        kind: Kind::Finding,
        priority: 2,
        created_ms: events::now_ms(),
        source: Source::default(),
        title: "Gate is red".to_owned(),
        star: Star::default(),
        options: vec![CardOption { label: "Act on it".to_owned(), ..CardOption::default() }],
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: None,
        refs: Vec::new(),
        level: None,
        tool_call_id: None,
        report_due: false,
        images: Vec::new(),
        engine: None,
    };
    db.insert_card(&finding).unwrap();
    db.swipe(&Swipe {
        card: finding.id.clone(),
        action: "later".to_owned(),
        option: Some(0),
        text: Some(DispatchMode::Act.swipe_text().to_owned()),
        note: None,
        until_ms: Some(events::now_ms()),
        reason: Some(format!("launching dsh:{SID}")),
        at_ms: events::now_ms(),
    })
    .unwrap();
    answer(
        &db,
        &finding,
        &AnswerReq { option: Some(0), mode: Some("act".to_owned()), ..AnswerReq::default() },
        &bridge.launch(),
    None,
    )
    .unwrap();
    assert!(bridge.posts("/v1/launch").is_empty());
    assert!(bridge.posts("/v1/prompt").is_empty());
    let swipe = current_swipe(&db, &finding.id);
    assert_eq!(swipe.action, "done");
    assert_eq!(swipe.reason, Some(format!("launched dsh:{SID}")));
}

#[test]
fn dsh_bridge_with_a_wrong_secret_creates_no_cards() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![ask("ev-1", vec![question("q1", "Color", "Which color?", &["Red"], false)])];
    });
    let wrong = scratch.path().join("wrong-host.json");
    write_host(&wrong, &bridge.api, "not-the-secret");
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, Some(wrong.as_path()), NOW), 0);
    assert_eq!(bridge.requests(), 0);
    assert!(db.card(&format!("ask:dsh:{SID}:ev-1")).unwrap().is_none());
}

#[test]
fn dsh_tick_without_a_host_file_reads_nothing() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions = vec![session(0)];
        data.pending = vec![ask("ev-1", vec![question("q1", "Color", "Which color?", &["Red"], false)])];
    });
    assert_eq!(dsh_tick(&mut Tracker::default(), &db, None, NOW), 0);
    assert_eq!(bridge.requests(), 0);
    assert!(db.card(&format!("ask:dsh:{SID}:ev-1")).unwrap().is_none());
}

#[test]
fn startup_delay_grows_with_each_attempt_within_its_jitter() {
    let command = "omp --model=@kimi --goal 'Reply DONE'";
    for (tries, base) in [(0u8, 1000u64), (1, 2000), (2, 4000), (3, 8000), (4, 8000)] {
        let delay = startup_delay(command, tries).as_millis() as u64;
        assert!((base..base + STARTUP_JITTER_MS + 1).contains(&delay), "attempt {tries}: {delay} ms");
    }
    assert_ne!(
        startup_delay(command, 0),
        startup_delay("omp --model=@sol --goal 'Reply DONE'", 0),
        "the jitter spreads launches"
    );
}

/// A watched launch: the pane `%12` runs `command` and has not written a session.
fn watched_launch(root: &Path, command: &str, tries: u8, seen: Instant) -> (Tracker, Db) {
    let db = Db::open(&root.join("triage.db")).unwrap();
    let mut tracker = Tracker::default();
    tracker.startup.insert(
        command.to_owned(),
        Startup {
            command: command.to_owned(),
            cwd: root.display().to_string(),
            session: "session".to_owned(),
            tries,
            seen,
            pane: Some(pane(root, "%12", "/dev/pts/12", "host: tmp")),
            pending: None,
            due: None,
        },
    );
    tracker.startup_panes.insert("%12".to_owned(), command.to_owned());
    (tracker, db)
}

/// omp's log for the watched pane's pid, holding the crash that makes a relaunch safe.
fn write_busy_log(root: &Path, body: &str) {
    fs::create_dir_all(root.join(".omp/logs")).unwrap();
    fs::write(root.join(".omp/logs/omp.2026-11-09.0.log"), body).unwrap();
}

#[cfg(unix)]
#[test]
fn startup_death_relaunches_after_its_backoff() {
    if let Some(root) = child_root("startup_death_relaunches_after_its_backoff") {
        let command = "omp --model=@kimi --goal 'Reply DONE'";
        fs::write(root.join("tmux-pane-12-gone"), b"").unwrap();
        write_busy_log(
            &root,
            r#"{"level":"error","message":"Fatal error","err":{"message":"database is locked","code":"SQLITE_BUSY"}}"#,
        );
        let (mut tracker, db) = watched_launch(&root, command, 0, Instant::now());

        let first = tracker.tick(&db).written;
        let scheduled = tracker
            .startup
            .get(command)
            .is_some_and(|entry| entry.due.is_some() && entry.tries == 1);
        assert_eq!(first, 0);
        assert!(scheduled, "the startup death waits out its backoff");
        assert!(!root.join("tmux-new-window-count").exists(), "no window before the backoff");

        thread::sleep(Duration::from_millis(1600));
        tracker.tick(&db);
        let windows = fs::read_to_string(root.join("tmux-new-window-count")).unwrap_or_default();
        assert_eq!(windows.trim(), "1", "one relaunch after the backoff");
        let log = fs::read_to_string(root.join("tmux.log")).unwrap();
        assert!(log.contains("new-window") && log.contains(command), "{log}");
        let relaunched = tracker.startup.get(command);
        assert!(relaunched.is_some_and(|entry| entry.pending.as_deref() == Some("%42")));
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "startup_death_relaunches_after_its_backoff",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn startup_death_without_a_busy_log_is_forgotten() {
    if let Some(root) = child_root("startup_death_without_a_busy_log_is_forgotten") {
        let command = "omp --model=@kimi --goal 'Reply DONE'";
        fs::write(root.join("tmux-pane-12-gone"), b"").unwrap();
        write_busy_log(&root, r#"{"level":"error","err":{"message":"No model selected."}}"#);
        let (mut tracker, db) = watched_launch(&root, command, 0, Instant::now());

        assert_eq!(tracker.tick(&db).written, 0);
        assert!(tracker.startup.get(command).is_none(), "another crash is not repeated");
        assert!(!root.join("tmux-new-window-count").exists());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "startup_death_without_a_busy_log_is_forgotten",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn startup_retries_stop_with_a_card() {
    if let Some(root) = child_root("startup_retries_stop_with_a_card") {
        let command = "omp --model=@kimi --goal 'Reply DONE'";
        fs::write(root.join("tmux-pane-12-gone"), b"").unwrap();
        write_busy_log(
            &root,
            r#"{"level":"error","message":"Fatal error","err":{"message":"database is locked","code":"SQLITE_BUSY"}}"#,
        );
        let (mut tracker, db) = watched_launch(&root, command, 4, Instant::now());

        let written = tracker.tick(&db).written;
        let queue = db.queue(events::now_ms()).unwrap();
        assert_eq!(written, 1);
        assert_eq!(queue.len(), 1);
        assert!(queue[0].id.starts_with("startup:%12:"), "{}", queue[0].id);
        assert_eq!(queue[0].level.as_deref(), Some("error"));
        assert!(queue[0].body.as_deref().is_some_and(|body| body.contains(command)));
        assert!(tracker.startup.get(command).is_none(), "no fifth attempt");
        assert!(!root.join("tmux-new-window-count").exists());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "startup_retries_stop_with_a_card",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

#[cfg(unix)]
#[test]
fn startup_launch_that_wrote_a_session_is_not_repeated() {
    if let Some(root) = child_root("startup_launch_that_wrote_a_session_is_not_repeated") {
        let command = "omp --model=@kimi --goal 'Reply DONE'";
        fs::write(root.join("tmux-pane-12-gone"), b"").unwrap();
        write_busy_log(
            &root,
            r#"{"level":"error","message":"Fatal error","err":{"message":"database is locked","code":"SQLITE_BUSY"}}"#,
        );
        let transcript = root.join("transcript.jsonl");
        map_transcript(&root, "/dev/pts/12", &transcript);
        fs::write(&transcript, "").unwrap();
        let (mut tracker, db) = watched_launch(&root, command, 0, Instant::now());

        assert_eq!(tracker.tick(&db).written, 0);
        assert!(tracker.startup.get(command).is_none(), "a session owns the exit");
        assert!(!root.join("tmux-new-window-count").exists());
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "startup_launch_that_wrote_a_session_is_not_repeated",
        scratch.path(),
        Some(scratch.path()),
        Some(&bin),
    );
}

fn brief_from_prompt(prompt: &Value) -> String {
    let prompt = prompt.as_str().expect("a launch prompt");
    let path = prompt
        .strip_prefix("Read the brief at ")
        .and_then(|rest| rest.strip_suffix(" and carry it out."))
        .unwrap_or_else(|| panic!("unexpected launch prompt {prompt}"));
    fs::read_to_string(path).unwrap()
}

#[test]
fn dsh_workspace_launch_posts_the_workspace_and_briefs_the_project_directory() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let launch = Launch { dsh_workspace: Some("triage".to_owned()), ..bridge.launch() };
    let pane = start_session(&db, "Ship it", "/work/project", &launch, Start::Goal, None);
    assert_eq!(pane, Ok("dsh:session-new".to_owned()));
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["workspace"], "triage");
    assert!(launches[0].get("cwd").is_none(), "{launches:?}");
    let brief = brief_from_prompt(&launches[0]["prompt"]);
    assert!(brief.starts_with("Work from `/work/project`."), "{brief}");
    assert!(brief.contains("`working_directory` tool"), "{brief}");
    assert!(brief.contains(ENVOY_STATUS_CONTRACT), "{brief}");
}

#[test]
fn dsh_workspace_launch_falls_back_to_the_project_directory_on_an_older_host() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| data.refuse_workspace = true);
    let launch = Launch { dsh_workspace: Some("triage".to_owned()), ..bridge.launch() };
    let pane = start_session(&db, "Ship it", "/work/project", &launch, Start::Goal, None);
    assert_eq!(pane, Ok("dsh:session-new".to_owned()));
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 2);
    assert_eq!(launches[0]["workspace"], "triage");
    assert!(launches[1]["workspace"].is_null(), "{launches:?}");
    assert_eq!(launches[1]["cwd"], "/work/project");
}

#[test]
fn dsh_workspace_dispatch_brief_names_the_working_directory() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    let launch = Launch { dsh_workspace: Some("triage".to_owned()), ..bridge.launch() };
    let card = dsh_card(&db, &format!("done:dsh:{SID}:4"), Kind::Info);
    dispatch(&db, &card, DispatchMode::Act, None, &launch, None, None, None).unwrap();
    let launches = bridge.posts("/v1/launch");
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["workspace"], "triage");
    let brief = brief_from_prompt(&launches[0]["prompt"]);
    assert!(brief.contains("## Working directory\n`/work/project`"), "{brief}");
    assert!(brief.contains("## References"), "{brief}");
    assert!(brief.find("## Working directory") < brief.find("## References"), "{brief}");
}

#[test]
fn dsh_plan_on_an_existing_session_archives_nothing_when_the_launch_fails() {
    let scratch = Scratch::new();
    let db = dsh_db(&scratch);
    let bridge = FakeBridge::start(scratch.path());
    bridge.edit(|data| {
        data.sessions.push(session(5));
        data.launch_error = Some("plan mode did not turn on".to_owned());
    });
    let card = dsh_card(&db, "plan-on-live", Kind::Info);
    let brief = scratch.path().join("brief.md");
    fs::write(&brief, "# Plan\n").unwrap();
    // The session was the card's before the launch. Failing to start a plan in it must
    // not archive the session the user already had.
    assert_eq!(
        super::dsh_track::launch_brief(&card, &bridge.launch(), &brief, Start::Plan, None),
        Err("plan mode did not turn on".to_owned())
    );
    assert!(bridge.posts("/v1/archive").is_empty());
}
