//! Shelling out to tmux. Owner: tmux worker. Pure wrappers, no policy here.
use std::fs;
use std::io;
use std::process::{Command, Output};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    /// `%12`
    pub id: String,
    pub title: String,
    /// `#{pane_current_command}` such as `omp` or `zsh`.
    pub command: String,
    /// `/dev/pts/18`
    pub tty: String,
    /// `session:window.pane` such as `bill:1.0`.
    pub target: String,
    pub session: String,
    pub triage_card_tag: Option<String>,
    pub cwd: String,
    /// Columns and rows of the pane, so a viewer can lay the screen out as tmux does.
    pub width: u16,
    pub height: u16,
    /// `#{pane_pid}`: the pane's first process, usually its shell.
    pub pid: u32,
}

impl Pane {
    /// The pty's ctime in Unix milliseconds, or `None` when the tty cannot be
    /// stat'ed. tmux reuses pane ids and the kernel reuses pts numbers after a
    /// server restart, so the pty's age tells a pane apart from an earlier one
    /// with the same id or tty.
    pub fn pty_born_ms(&self) -> Option<i64> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::metadata(&self.tty).ok()?;
            Some(metadata.ctime() * 1000 + metadata.ctime_nsec() / 1_000_000)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// The argv of the pane's foreground process, or `None` when it cannot be
    /// read. The kernel reports the foreground process group through the pty.
    pub fn foreground_argv(&self) -> Option<Vec<String>> {
        let stat = fs::read_to_string(format!("/proc/{}/stat", self.pid)).ok()?;
        let group = foreground_group(&stat)?;
        let cmdline = fs::read_to_string(format!("/proc/{group}/cmdline")).ok()?;
        Some(
            cmdline
                .split('\0')
                .filter(|piece| !piece.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }
}

/// The foreground process group id from a `/proc/<pid>/stat` line: field 8
/// (`tpgid`), counted after the last `)` because field 2, the command name, sits
/// in parentheses and may hold spaces.
fn foreground_group(stat: &str) -> Option<u32> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(5)?
        .parse()
        .ok()
}

/// `tmux list-panes -a` with a tab-separated format. Empty vec when no server runs.
pub fn list_panes() -> io::Result<Vec<Pane>> {
    let output = tmux_output(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{pane_title}\t#{pane_current_command}\t#{pane_tty}\t#{session_name}:#{window_index}.#{pane_index}\t#{pane_current_path}\t#{session_name}\t#{pane_width}\t#{pane_height}\t#{pane_pid}\t#{@triage_card_id}",
    ])?;
    if !output.status.success() {
        return if no_server(&output) {
            Ok(Vec::new())
        } else {
            Err(command_error(&output))
        };
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut panes = Vec::new();
    for line in stdout.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split('\t');
        let (Some(id), Some(title), Some(command), Some(tty), Some(target), Some(cwd), Some(session), Some(width), Some(height), Some(pid), Some(triage_card_tag)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid tmux pane row"));
        };
        let (Ok(width), Ok(height), Ok(pid), None) = (
            width.parse::<u16>(),
            height.parse::<u16>(),
            pid.parse::<u32>(),
            fields.next(),
        ) else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid tmux pane row"));
        };
        panes.push(Pane {
            id: id.to_owned(),
            title: title.to_owned(),
            command: command.to_owned(),
            tty: tty.to_owned(),
            target: target.to_owned(),
            session: session.to_owned(),
            cwd: cwd.to_owned(),
            width,
            height,
            pid,
            triage_card_tag: (!triage_card_tag.is_empty()).then(|| triage_card_tag.to_owned()),
        });
    }
    Ok(panes)
}

pub fn finding_pane(
    card_id: &str,
    option: usize,
    note: Option<&str>,
    mode: crate::cards::DispatchMode,
) -> io::Result<Option<String>> {
    let card_tag = card_tag(card_id);
    let expected_tag = finding_choice_tag(&card_tag, option, note, mode)?;
    for pane in list_panes()? {
        if let Some(tag) = pane.triage_card_tag.as_deref() {
            let suffix = tag.strip_prefix(card_tag.as_str());
            if tag == card_tag.as_str() || suffix.is_some_and(|suffix| suffix.starts_with(':')) {
                if tag != expected_tag.as_str() {
                    return Err(io::Error::other(
                        "finding pane was launched with a different answer",
                    ));
                }
                return Ok(Some(pane.id));
            }
        }
    }
    Ok(None)
}

pub fn tag_finding_pane(
    pane: &str,
    card_id: &str,
    option: usize,
    note: Option<&str>,
    mode: crate::cards::DispatchMode,
) -> io::Result<()> {
    let tag = finding_choice_tag(&card_tag(card_id), option, note, mode)?;
    tmux_run(&["set-option", "-p", "-t", pane, "@triage_card_id", &tag])?;
    Ok(())
}

fn finding_choice_tag(
    card_tag: &str,
    option: usize,
    note: Option<&str>,
    mode: crate::cards::DispatchMode,
) -> io::Result<String> {
    use std::fmt::Write as _;

    let mut tag = String::with_capacity(card_tag.len().saturating_add(80));
    tag.push_str(card_tag);
    write!(&mut tag, ":{option}:{}:", mode.swipe_text()).unwrap();
    if let Some(note) = note {
        write!(&mut tag, "s{:032x}", digest(note.as_bytes())).unwrap();
    } else {
        tag.push('n');
    }
    Ok(tag)
}

fn digest(bytes: &[u8]) -> u128 {
    let mut digest = 0x6c62272e07bb014262b821756295c58du128;
    for byte in bytes {
        digest ^= u128::from(*byte);
        digest = digest.wrapping_mul(0x0000000001000000000000000000013bu128);
    }
    digest
}

fn card_tag(card_id: &str) -> String {
    format!("finding:{:032x}", digest(card_id.as_bytes()))
}

/// `tmux capture-pane -p -J -t pane -S -lines`.
pub fn capture(pane: &str, lines: u32) -> io::Result<String> {
    let lines = format!("-{lines}");
    tmux_text(&["capture-pane", "-p", "-J", "-t", pane, "-S", &lines])
}

/// `tmux capture-pane -p -t pane`: the visible screen, one line per row as displayed.
pub fn screen(pane: &str) -> io::Result<String> {
    tmux_text(&["capture-pane", "-p", "-t", pane])
}

/// `tmux send-keys -t pane KEY...` with named keys such as `Down`, `Enter`.
pub fn send_keys(pane: &str, keys: &[&str]) -> io::Result<()> {
    let mut args = Vec::with_capacity(keys.len() + 3);
    args.extend(["send-keys", "-t", pane]);
    args.extend_from_slice(keys);
    tmux_run(&args)?;
    Ok(())
}

/// `tmux send-keys -t pane -l -- text` followed by `Enter`.
pub fn send_text(pane: &str, text: &str) -> io::Result<()> {
    send_literal(pane, text)?;
    tmux_run(&["send-keys", "-t", pane, "Enter"])?;
    Ok(())
}

/// Paste `text` as one bracketed paste with no `Enter`, so the focused form field keeps it verbatim.
pub fn paste_text(pane: &str, text: &str) -> io::Result<()> {
    send_literal(pane, &format!("\x1b[200~{text}\x1b[201~"))
}

/// `send-keys -l` in UTF-8-safe 8 KiB chunks.
fn send_literal(pane: &str, text: &str) -> io::Result<()> {
    const CHUNK_BYTES: usize = 8 * 1024;
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        tmux_run(&["send-keys", "-t", pane, "-l", "--", &text[start..end]])?;
        start = end;
    }
    Ok(())
}

/// `tmux new-window -t session -c cwd -P -F '#{pane_id}' cmd`. Returns the new pane id.
///
/// No `-n`: naming a window turns off tmux's `automatic-rename`, and the status bar
/// shows the live pane title only for auto-renamed windows, so a named window would
/// hide omp's session title forever behind the launch-time slug.
pub fn new_window(session: &str, cwd: &str, cmd: &str) -> io::Result<String> {
    let path = format!("PATH={}", std::env::var("PATH").map_err(io::Error::other)?);
    // The trailing colon makes this a session target. A bare name can prefix-match a window.
    let target = format!("{session}:");
    let pane = tmux_text(&[
        "new-window",
        "-e",
        &path,
        "-t",
        &target,
        "-c",
        cwd,
        "-P",
        "-F",
        "#{pane_id}",
        cmd,
    ])?;
    Ok(pane.trim().to_owned())
}

/// Switch the most recently active tmux client to `pane`: switch-client to its
/// session, then select-window and select-pane.
pub fn jump(pane: &str) -> io::Result<()> {
    let session = tmux_text(&["display", "-p", "-t", pane, "#S"])?;
    let session = session.trim_end_matches(|character| matches!(character, '\r' | '\n'));
    if session.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "tmux returned an empty session name"));
    }
    tmux_run(&["switch-client", "-t", session])?;
    tmux_run(&["select-window", "-t", pane])?;
    tmux_run(&["select-pane", "-t", pane])?;
    Ok(())
}

/// `tmux kill-pane -t pane`. tmux hangs up the harness running in it and closes the window when
/// this was its last pane.
pub fn kill_pane(pane: &str) -> io::Result<()> {
    tmux_run(&["kill-pane", "-t", pane]).map(|_| ())
}

/// Whether a tmux error means the target pane no longer exists: `can't find pane: %N` for a
/// dead pane id, or `no server running`, which means every pane is gone.
pub fn pane_gone(error: &io::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("can't find pane") || message.contains("no server running")
}

/// Names of every tmux session, in tmux's order. Empty when no server runs.
pub fn list_sessions() -> io::Result<Vec<String>> {
    let output = tmux_output(&["list-sessions", "-F", "#{session_name}"])?;
    if !output.status.success() {
        return if no_server(&output) { Ok(Vec::new()) } else { Err(command_error(&output)) };
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().filter(|s| !s.is_empty()).map(str::to_owned).collect())
}
fn tmux_output(args: &[&str]) -> io::Result<Output> {
    Command::new("tmux").args(args).output()
}

fn tmux_run(args: &[&str]) -> io::Result<Output> {
    let output = tmux_output(args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(command_error(&output))
    }
}

fn tmux_text(args: &[&str]) -> io::Result<String> {
    String::from_utf8(tmux_run(args)?.stdout)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn no_server(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr)
        .to_ascii_lowercase()
        .contains("no server running")
}

fn command_error(output: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    if message.is_empty() {
        io::Error::other(format!("tmux exited with {}", output.status))
    } else {
        io::Error::other(message.to_owned())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "triage-tmux-{name}-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn fake_tmux(&self, name: &str, script: &str) -> PathBuf {
            let directory = self.0.join(name);
            std::fs::create_dir(&directory).unwrap();
            let executable = directory.join("tmux");
            std::fs::write(&executable, script).unwrap();
            let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&executable, permissions).unwrap();
            directory
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn child_test(name: &str, path: &Path) -> Command {
        let full_name = format!("tmux::tests::{name}");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", full_name.as_str()]).env("PATH", path);
        command
    }

    #[test]
    fn foreground_group_is_counted_after_the_command_name() {
        assert_eq!(foreground_group("4242 (z sh) S 1 4242 4242 34816 5150 0"), Some(5150));
        assert_eq!(foreground_group("1 ((a) (b)) R 1 2 3 4 99 0"), Some(99));
        assert_eq!(foreground_group("4242 (zsh) S 1 4242 4242 34816 -1 0"), None);
        assert_eq!(foreground_group("(no-pid-field)"), None);
    }

    #[test]
    fn send_text_chunks_utf8_input_and_skips_enter_after_chunk_failure() {
        const CHUNK_BYTES: usize = 8 * 1024;
        const PREFIX: &str = "send-keys -t %1 -l -- ";
        const TMUX_SCRIPT: &str = r##"#!/bin/sh
call=0
if [ -f "$TRIAGE_TMUX_COUNT" ]; then
    IFS= read -r call < "$TRIAGE_TMUX_COUNT"
fi
call=$((call + 1))
printf '%s' "$call" > "$TRIAGE_TMUX_COUNT"
printf '%s\n' "$*" >> "$TRIAGE_TMUX_LOG"
all="$*"
if [ "${#all}" -gt 16000 ]; then
    echo 'command too long' >&2
    exit 1
fi
if [ "$call" -eq "${TRIAGE_TMUX_FAIL_CALL:-0}" ]; then
    exit 1
fi
"##;

        if std::env::var_os("TRIAGE_SEND_TEXT_CHILD").is_some() {
            let text = format!("{}🧠{}", "a".repeat(CHUNK_BYTES - 1), "b".repeat(CHUNK_BYTES));
            let result = send_text("%1", &text);
            if std::env::var_os("TRIAGE_TMUX_FAIL_CALL").is_some() {
                assert!(result.is_err());
            } else {
                result.unwrap();
            }
            return;
        }

        let scratch = Scratch::new("send-text");
        let text = format!("{}🧠{}", "a".repeat(CHUNK_BYTES - 1), "b".repeat(CHUNK_BYTES));

        for (name, fail_call) in [("success", None), ("failure", Some(2))] {
            let tmux_dir = scratch.fake_tmux(name, TMUX_SCRIPT);
            let log = tmux_dir.join("calls");
            std::fs::write(&log, "").unwrap();
            let mut command = child_test(
                "send_text_chunks_utf8_input_and_skips_enter_after_chunk_failure",
                &tmux_dir,
            );
            command
                .env("TRIAGE_SEND_TEXT_CHILD", "1")
                .env("TRIAGE_TMUX_LOG", &log)
                .env("TRIAGE_TMUX_COUNT", log.with_extension("count"))
                .env_remove("TRIAGE_TMUX_FAIL_CALL");
            if let Some(fail_call) = fail_call {
                command.env("TRIAGE_TMUX_FAIL_CALL", fail_call.to_string());
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );

            let calls = std::fs::read_to_string(log).unwrap();
            let calls = calls.lines().collect::<Vec<_>>();
            if fail_call.is_none() {
                assert_eq!(calls.len(), 4);
                let first = calls[0].strip_prefix(PREFIX).unwrap();
                let second = calls[1].strip_prefix(PREFIX).unwrap();
                let third = calls[2].strip_prefix(PREFIX).unwrap();
                assert_eq!(first.len(), CHUNK_BYTES - 1);
                assert!(second.starts_with("🧠"));
                assert_eq!(second.len(), CHUNK_BYTES);
                assert_eq!(third, "bbbb");
                assert_eq!(format!("{first}{second}{third}"), text);
                assert_eq!(calls[3], "send-keys -t %1 Enter");
            } else {
                assert_eq!(calls.len(), 2);
                assert!(calls[0].starts_with(PREFIX));
                assert!(calls[1].starts_with(PREFIX));
            }
        }
    }

    #[test]
    fn new_window_passes_daemon_path_to_tmux() {
        const TMUX_SCRIPT: &str = r##"#!/bin/sh
printf '%s\n' "$@" > "$TRIAGE_TMUX_LOG"
printf '%%12\n'
"##;

        if std::env::var_os("TRIAGE_NEW_WINDOW_CHILD").is_some() {
            let cwd = std::env::temp_dir();
            new_window("session", cwd.to_str().unwrap(), "omp").unwrap();
            return;
        }

        let scratch = Scratch::new("new-window");
        let tmux_dir = scratch.fake_tmux("bin", TMUX_SCRIPT);
        let log = scratch.0.join("calls");
        let output = child_test("new_window_passes_daemon_path_to_tmux", &tmux_dir)
            .env("TRIAGE_NEW_WINDOW_CHILD", "1")
            .env("TRIAGE_TMUX_LOG", &log)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let recorded = std::fs::read_to_string(log).unwrap();
        let mut args = recorded.lines();
        assert_eq!(args.next(), Some("new-window"));
        assert_eq!(args.next(), Some("-e"));
        let daemon_path = tmux_dir.to_string_lossy();
        let forwarded_path = format!("PATH={daemon_path}");
        assert_eq!(args.next(), Some(forwarded_path.as_str()));
        assert_eq!(args.next(), Some("-t"));
        // A bare name is a window target, so tmux prefix-matches a window such as
        // `emacsclient` for session `emacs` and fails because index 0 is in use.
        assert_eq!(args.next(), Some("session:"));
        // No `-n`: a named window loses tmux's `automatic-rename`, and the status bar
        // then hides omp's live session title behind the frozen name.
        assert!(
            !recorded.lines().any(|argument| argument == "-n"),
            "new_window must not name the window: {recorded}"
        );
        let cwd = std::env::temp_dir().to_str().unwrap().to_owned();
        assert_eq!(args.next(), Some("-c"));
        assert_eq!(args.next(), Some(cwd.as_str()));
        assert_eq!(args.next(), Some("-P"));
        assert_eq!(args.next(), Some("-F"));
        assert_eq!(args.next(), Some("#{pane_id}"));
        assert_eq!(args.next(), Some("omp"));
        assert_eq!(args.next(), None);
    }

    #[test]
    fn finding_pane_rejects_changed_option_note_or_mode() {
        const TMUX_SCRIPT: &str = r##"#!/bin/sh
case "$1" in
    set-option)
        for arg do tag=$arg; done
        printf '%s' "$tag" > "$TRIAGE_TMUX_TAG_FILE"
        ;;
    list-panes)
        tag=''
        if [ -f "$TRIAGE_TMUX_TAG_FILE" ]; then
            IFS= read -r tag < "$TRIAGE_TMUX_TAG_FILE"
        fi
        printf '%%1\tfinding\top\t/dev/pts/1\tsession:0.0\t/tmp\tsession\t120\t40\t0\t%s\n' "$tag"
        ;;
    *) exit 1 ;;
esac
"##;

        if std::env::var_os("TRIAGE_FINDING_PANE_CHILD").is_some() {
            let mode = crate::cards::DispatchMode::Act;
            tag_finding_pane("%1", "card", 2, Some("original note"), mode).unwrap();
            let same_choice = finding_pane("card", 2, Some("original note"), mode)
                .unwrap()
                .as_deref()
                == Some("%1");
            let option_rejected = finding_pane("card", 3, Some("original note"), mode).is_err();
            let note_rejected = finding_pane("card", 2, Some("changed note"), mode).is_err();
            let mode_rejected = finding_pane(
                "card",
                2,
                Some("original note"),
                crate::cards::DispatchMode::Plan,
            )
            .is_err();
            let results = format!("{same_choice}\n{option_rejected}\n{note_rejected}\n{mode_rejected}\n");
            let results_path =
                PathBuf::from(std::env::var_os("TRIAGE_FINDING_PANE_RESULT_FILE").unwrap());
            std::fs::write(results_path, results).unwrap();
            return;
        }

        let scratch = Scratch::new("finding-pane");
        let tmux_dir = scratch.fake_tmux("bin", TMUX_SCRIPT);
        let results_path = scratch.0.join("results");
        let output = child_test("finding_pane_rejects_changed_option_note_or_mode", &tmux_dir)
            .env("TRIAGE_FINDING_PANE_CHILD", "1")
            .env("TRIAGE_TMUX_TAG_FILE", scratch.0.join("tag"))
            .env("TRIAGE_FINDING_PANE_RESULT_FILE", &results_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let results = std::fs::read_to_string(results_path).unwrap();
        let mut results = results.lines();
        assert_eq!(results.next(), Some("true"));
        assert_eq!(results.next(), Some("true"), "changed option must be rejected");
        assert_eq!(results.next(), Some("true"), "changed note must be rejected");
        assert_eq!(results.next(), Some("true"), "changed mode must be rejected");
        assert_eq!(results.next(), None);
    }

    #[test]
    fn finding_choice_tag_bounds_long_notes_without_losing_identity() {
        let note_a = "a".repeat(32 * 1024);
        let note_b = "b".repeat(32 * 1024);
        let card_tag = card_tag("card");
        let tag_a = finding_choice_tag(&card_tag, 0, Some(&note_a), crate::cards::DispatchMode::Act).unwrap();
        let tag_b = finding_choice_tag(&card_tag, 0, Some(&note_b), crate::cards::DispatchMode::Act).unwrap();

        assert!(tag_a.len() < 128);
        assert!(tag_b.len() < 128);
        assert_ne!(tag_a, tag_b);
    }
    #[test]
    fn finding_choice_tag_distinguishes_mode() {
        let card_tag = card_tag("card");
        let act = finding_choice_tag(&card_tag, 0, None, crate::cards::DispatchMode::Act).unwrap();
        let plan = finding_choice_tag(&card_tag, 0, None, crate::cards::DispatchMode::Plan).unwrap();
        assert_ne!(act, plan);
    }

    #[test]
    fn finding_tag_bounds_large_card_id() {
        let card_id = "x".repeat(70 * 1024);
        let tag = finding_choice_tag(&card_tag(&card_id), 0, None, crate::cards::DispatchMode::Act).unwrap();
        assert!(tag.len() < 128);
    }
}
