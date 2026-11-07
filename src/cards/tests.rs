use super::*;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

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
    elif [ -f "$HOME/tmux-idle-screen" ]; then
        printf '───\n───\n'
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
    }
}

fn finding_card(root: &Path) -> Card {
    Card {
        id: format!("finding-{}", uuid::Uuid::new_v4()),
        kind: Kind::Finding,
        priority: 1,
        created_ms: events::now_ms(),
        source: Source {
            cwd: root.display().to_string(),
            ..Source::default()
        },
        title: "Finding".to_owned(),
        star: Star::default(),
        options: ["Apply", "Defer"]
            .into_iter()
            .map(|label| CardOption {
                label: label.to_owned(),
                ..CardOption::default()
            })
            .collect(),
        answerable: true,
        allow_other: false,
        questions: Vec::new(),
        body: None,
        refs: Vec::new(),
        level: None,
        tool_call_id: None,
    }
}

fn info_card(id: &str, pane: &str) -> Card {
    Card {
        id: id.to_owned(),
        kind: Kind::Info,
        priority: 1,
        created_ms: events::now_ms(),
        source: Source {
            uri: format!("pane:{pane}"),
            label: pane.to_owned(),
            pane: Some(pane.to_owned()),
            ..Source::default()
        },
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
        body: Some("Plan body".to_owned()),
        refs: Vec::new(),
        level: None,
        tool_call_id: None,
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

#[test]
fn finding_brief_includes_agent_done_status_contract() {
    let card = finding_card(Path::new("/tmp"));
    let option = CardOption {
        label: "Do as recommended".to_owned(),
        ..CardOption::default()
    };
    let brief = finding_brief(&card, &option, None);
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
    let brief = finding_brief(&card, &option, None);
    assert!(brief.contains("## Proposed action, not approved\n"));
    assert!(!brief.contains("## Action\n"));
    assert!(brief.contains(INVESTIGATE_INSTRUCTION));
    assert!(brief.contains(INVESTIGATE_STATUS_CONTRACT));
    assert!(!brief.contains("ENVOY_STATUS: agent-done"));
    assert!(brief.contains("`Recommended action:`"));
    assert!(brief.contains(&format!("{INVESTIGATE_STATUS_CONTRACT}{REPORT_STEP}")));
}

// On 2026-10-31 a plan session approved with "Approve and execute" finished without a
// status line, so triage queued no card. The fresh session saw only the plan, and nothing
// had told the planner to copy the brief's status contract into it.
#[test]
fn plan_briefs_make_the_plan_repeat_the_status_contract_for_a_fresh_session() {
    let card = info_card("done:%12:plan", "%12");
    let dispatched = dispatch_brief(&card, DispatchMode::Plan, None);
    let composed = session_brief("Prepare a plan.", true);
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
    assert!(!dispatch_brief(&card, DispatchMode::Act, None).contains(PLAN_REPORT_STEP));
    assert!(!session_brief("Reply with pong.", false).contains(PLAN_REPORT_STEP));
    assert!(
        dispatch_brief(&card, DispatchMode::Act, None)
            .ends_with(&format!("{ENVOY_STATUS_CONTRACT}{REPORT_STEP}"))
    );
    assert!(
        session_brief("Reply with pong.", false)
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
        let first = write_brief(&configured_db, finding_brief(&short_card, &option, None));
        let second = write_brief(&configured_db, finding_brief(&short_card, &option, None));
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
        let long_brief = write_brief(&configured_db, finding_brief(&long_card, &option, None));
        let long_id_uses_bounded_path = long_brief
            .as_ref()
            .is_ok_and(|path| path.parent() == Some(expected_directory.as_path()));

        let relative = root.join("relative");
        fs::create_dir_all(&relative).unwrap();
        std::env::set_current_dir(&relative).unwrap();
        let relative_db = Db::open(Path::new("triage.db")).unwrap();
        let relative_brief = write_brief(&relative_db, finding_brief(&short_card, &option, None));
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

        let result = answer(&db, &stale, &answer_req(0, None), &Launch::default());
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
            .map(|card| answer(&db, card, &answer_req(0, None), &Launch::default()));
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
        let result = answer(&db, &card, &request, &Launch::default());
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
        let result = answer(&db, &card, &request, &Launch::default());
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

// Regression: on 2026-11-01 an act dispatch reported "complete" but left a
// standing rule as its `Recommended action:` line, and the card offered
// "Do as recommended" for advice no session could carry out. A launch that
// carried the work out owes no next step, so its line stays in the body.
#[cfg(unix)]
#[test]
fn act_report_keeps_its_recommended_line_in_the_body() {
    if let Some(root) = child_root("act_report_keeps_its_recommended_line_in_the_body") {
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
        let mut tracker = Tracker::default();
        let written = tracker.tick(&db).written;
        let card = db
            .queue(events::now_ms())
            .unwrap()
            .into_iter()
            .find(|card| card.id == "done:%12:assistant-report");
        let card = card.as_ref().and_then(|card| {
            card.body
                .as_deref()
                .map(|body| (card.star.action.is_empty(), body))
        });
        assert!(
            written == 1
                && card.is_some_and(|(no_action, body)| {
                    no_action
                        && body.contains(
                            "Recommended action: next cold-email task, read the field data first.",
                        )
                }),
            "card: {card:?}"
        );
        return;
    }

    let scratch = Scratch::new();
    let bin = fake_tmux(scratch.path());
    run_child(
        "act_report_keeps_its_recommended_line_in_the_body",
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
                    "content": [{"type": "text", "text": "ENVOY_STATUS: agent-done\n\nPush still open, suite red."}]
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
                assistant_entry("t1", "toolUse", Some("Dumping rows now."), None),
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
        let closed = close(&db, &card, None);
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
        let db = Db::open(&root.join("triage.db")).unwrap();
        let mut tracker = Tracker::default();
        let waiting = tracker.tick(&db).written;
        let waiting_queue = done_cards(&db);
        fs::remove_file(screens.join("0")).unwrap();
        let finished = tracker.tick(&db).written;
        let queue = done_cards(&db);
        assert!(
            waiting == 0
                && waiting_queue.is_empty()
                && finished == 1
                && queue.len() == 1
                && queue[0].id == "done:%12:a1",
            "queue: {queue:?}"
        );
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
                    Some("ENVOY_STATUS: complete\n\nShipped."),
                    None,
                ),
                wake,
                assistant_entry("a2", "stop", Some("Background job finished."), None),
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
                && queue[0].body.as_deref() == Some("Shipped.\n\n---\n\nBackground job finished.")
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
        answer(&db, &card, &request, &Launch::default()).unwrap();
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
        );
        let pane = pane(&root, "%12", "/dev/pts/12", "π > session");
        let mut written = 0;
        let inserted = add_info_card(
            &db,
            &pane,
            std::slice::from_ref(&pane),
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
        );
        let info_swipe = current_swipe(&db, &info.id);
        let repeat = dispatch(
            &db,
            &info,
            DispatchMode::Investigate,
            None,
            &in_session("session"),
            None,
        );
        let ask_result = dispatch(
            &db,
            &ask,
            DispatchMode::Investigate,
            None,
            &in_session("session"),
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
        let result = answer(&db, &card, &answer_req(0, None), &Launch::default());
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
            },
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
        );
        let look = dispatch(
            &db,
            &look_card,
            DispatchMode::Investigate,
            None,
            &in_session("session"),
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
                "--config '{}' @'{}'",
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
                    "--model=provider/planner --plan=provider/planner --plan-yolo --plan-yolo-into=provider/executor @'{}'",
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
        for card in [&approve, &empty, &unknown, &refine, &other] {
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
        );
        let unknown_result = answer(
            &db,
            &unknown,
            &AnswerReq {
                option: Some(2),
                ..AnswerReq::default()
            },
            &Launch::default(),
        );
        let approve_result = answer(&db, &approve, &answer_req(0, None), &Launch::default());
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
        );
        let other_result = answer(
            &db,
            &other,
            &AnswerReq {
                text: Some("Keep the current owner".to_owned()),
                ..AnswerReq::default()
            },
            &Launch::default(),
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
fn close_kills_source_pane_and_failed_kill_keeps_card_queued() {
    if let Some(root) = child_root("close_kills_source_pane_and_failed_kill_keeps_card_queued") {
        let db = Db::open(&root.join("triage.db")).unwrap();
        let kept = info_card("done:%12:kept", "%12");
        let closed = info_card("done:%12:closed", "%12");
        db.insert_card(&kept).unwrap();
        db.insert_card(&closed).unwrap();
        fs::write(root.join("tmux-fail-kill"), "").unwrap();
        let failed = close(&db, &kept, None);
        let kept_queued = db.card(&kept.id).unwrap().unwrap().1.is_none();
        fs::remove_file(root.join("tmux-fail-kill")).unwrap();
        let result = close(&db, &closed, Some("thanks"));
        let swipe = current_swipe(&db, &closed.id);
        let repeat = close(&db, &closed, None);
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
                scope.spawn(|| answer(&db, &acknowledged, &AnswerReq::default(), &launch)),
                scope.spawn(|| {
                    dispatch(
                        &db,
                        &dispatched,
                        DispatchMode::Investigate,
                        None,
                        &launch,
                        None,
                    )
                }),
                scope.spawn(|| close(&db, &closed, None)),
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
        let gone_result = close(&db, &gone, Some("noted"));
        let paneless_result = close(&db, &paneless, None);
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
            Some("session"),
            None,
            None,
            Start::Goal,
        );
        let launched = start_session(
            &db,
            "Reply with pong.",
            "/tmp",
            Some("session"),
            None,
            None,
            Start::Goal,
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
            Some("session"),
            None,
            None,
            Start::Plan,
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
                "--config '{}' @'{}'",
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
    let brief = Path::new("/tmp/b.md");
    let chosen = omp_command(
        brief,
        Some("provider/planner"),
        Some("high"),
        Start::PlanInto(Some("provider/executor")),
    )
    .unwrap();
    assert_eq!(
        chosen,
        "omp --model=provider/planner --plan=provider/planner --thinking=high --plan-yolo --plan-yolo-into=provider/executor @'/tmp/b.md'"
    );
    let blank = omp_command(brief, None, None, Start::PlanInto(Some(" "))).unwrap();
    assert_eq!(blank, "omp --plan-yolo @'/tmp/b.md'");
    assert!(omp_command(brief, None, None, Start::PlanInto(Some("x; rm -rf ~"))).is_err());
}

#[test]
fn goal_command_makes_the_brief_the_objective() {
    let directory =
        std::env::temp_dir().join(format!("triage-goal-command-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let brief = directory.join("brief.md");
    fs::write(&brief, "# Act: Ship it\n\nDo the work.\n").unwrap();
    let chosen = omp_command(
        &brief,
        Some("provider/example-model"),
        Some("high"),
        Start::Goal,
    )
    .unwrap();
    assert_eq!(
        chosen,
        format!(
            "omp --model=provider/example-model --plan=provider/example-model --thinking=high --goal '# Act: Ship it\n\nDo the work.\n\nThe brief is saved at {}.'",
            brief.display()
        )
    );
    let blank = omp_command(&brief, Some(" "), Some(""), Start::Goal).unwrap();
    assert!(blank.starts_with("omp --goal '# Act: Ship it"), "{blank}");
    assert!(!blank.contains('@'));
    assert!(omp_command(&brief, None, Some("turbo"), Start::Goal).is_err());
    assert!(omp_command(&brief, Some("x; rm -rf ~"), None, Start::Goal).is_err());
    assert!(omp_command(&directory.join("missing.md"), None, None, Start::Goal).is_err());
    fs::remove_dir_all(&directory).unwrap();
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
    let command = omp_command(&brief, None, None, Start::Goal).unwrap();
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

        let first_result = answer(&db, &card, &request, &in_session("session"));
        let pending = current_swipe(&db, &card.id);
        let first_launch_count = new_window_count(&root);
        failure
            .execute_batch("DROP TRIGGER fail_done_swipe;")
            .unwrap();

        let retry_result = answer(&db, &card, &request, &in_session("session"));
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

        let first_result = answer(&db, &card, &original, &in_session("session"));
        let pending_reason = current_swipe(&db, &card.id).reason;
        let changed_option = answer(
            &db,
            &card,
            &answer_req(1, Some("review")),
            &in_session("session"),
        );
        let changed_note = answer(
            &db,
            &card,
            &answer_req(0, Some("changed")),
            &in_session("session"),
        );
        let launches_before_retry = new_window_count(&root);
        let retry_result = answer(&db, &card, &original, &in_session("session"));
        let completed = current_swipe(&db, &card.id);
        let different_choice = |result: &Result<(), String>| {
            result.as_ref().err().map(String::as_str)
                == Some("finding pane was launched with a different answer")
        };
        let retry_contract = first_result.is_err()
            && pending_reason.as_deref() == Some("launching %42")
            && different_choice(&changed_option)
            && different_choice(&changed_note)
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
fn finding_retry_tags_pane_when_pending_swipe_write_fails() {
    if let Some(root) = child_root("finding_retry_tags_pane_when_pending_swipe_write_fails") {
        let db_path = root.join("triage.db");
        let db = Db::open(&db_path).unwrap();
        let card = finding_card(&root);
        db.insert_card(&card).unwrap();
        let failure = install_swipe_failure(&db_path, "fail_pending_swipe", "later");
        let request = answer_req(0, Some("review"));

        let first_result = answer(&db, &card, &request, &in_session("session"));
        let no_pending_swipe = db.card(&card.id).unwrap().unwrap().1.is_none();
        let tagged_before_retry = root.join("tmux-tag-%42").exists();
        let first_launch_count = new_window_count(&root);
        failure
            .execute_batch("DROP TRIGGER fail_pending_swipe;")
            .unwrap();

        let retry_result = answer(&db, &card, &request, &in_session("session"));
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
