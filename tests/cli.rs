use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("triage-cli-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).expect("create test directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("make directory private");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct RunningChild(Child);

impl Drop for RunningChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unused_loopback_addr() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback port");
    listener.local_addr().expect("read loopback address")
}

fn http_get(addr: SocketAddr, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(200))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn response_body(response: &str) -> &str {
    response.split_once("\r\n\r\n").expect("HTTP response body").1
}

fn wait_for_response(child: &mut Child, addr: SocketAddr, path: &str) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("triage exited before serving {path} with {status}"));
        }
        match http_get(addr, path) {
            Ok(response) => return Ok(response),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(format!("triage did not serve {path}: {error}")),
        }
    }
}

fn insert_unresolved_ask(path: &Path) {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("create private test database");
    drop(file);
    let connection = rusqlite::Connection::open(path).expect("open test database");
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
            );",
        )
        .expect("create event table");
    let card_id = "ask:%42:restart-call";
    let card = serde_json::json!({
        "id": card_id,
        "kind": "ask",
        "priority": 1,
        "created_ms": 1,
        "source": {
            "uri": "tmux://pane/%42",
            "label": "restart pane",
            "cwd": "",
            "pane": "%42",
            "target": "test:0.0"
        },
        "title": "Persisted question",
        "star": {},
        "options": [],
        "answerable": false,
        "allow_other": false,
        "refs": [],
        "tool_call_id": "restart-call"
    });
    connection
        .execute(
            "INSERT INTO events(id, ts_ms, source, type, card_id, priority, data)
             VALUES (?1, 1, ?2, 'card.ask', ?3, 1, ?4)",
            rusqlite::params![
                format!("card:{card_id}"),
                "tmux://pane/%42",
                card_id,
                card.to_string()
            ],
        )
        .expect("insert persisted Ask card");
}

fn fake_tmux_dir(temp: &TestDir) -> PathBuf {
    let tmux_dir = temp.0.join("bin");
    fs::create_dir(&tmux_dir).expect("create fake tmux directory");
    let fake_tmux = tmux_dir.join("tmux");
    fs::write(&fake_tmux, "#!/bin/sh\nexit 0\n").expect("write fake tmux");
    fs::set_permissions(&fake_tmux, fs::Permissions::from_mode(0o700))
        .expect("make fake tmux executable");
    tmux_dir
}

fn spawn_daemon(temp: &TestDir, tmux_dir: &Path, db_path: &Path, addr: SocketAddr) -> RunningChild {
    let child = Command::new(env!("CARGO_BIN_EXE_triage"))
        .args(["serve", "--db"])
        .arg(db_path)
        .args(["--listen"])
        .arg(addr.to_string())
        .env_clear()
        .env("PATH", tmux_dir)
        .env("TMPDIR", &temp.0)
        .spawn()
        .expect("start isolated triage daemon");
    RunningChild(child)
}

#[test]
fn serve_with_explicit_db_without_home() {
    let temp = TestDir::new();
    let db_path = temp.0.join("triage.db");
    let tmux_dir = fake_tmux_dir(&temp);
    let addr = unused_loopback_addr();
    let mut child = spawn_daemon(&temp, &tmux_dir, &db_path, addr);

    let response = wait_for_response(&mut child.0, addr, "/v1/queue");
    assert!(response.is_ok(), "triage did not serve the explicit database: {response:?}");
    let response = response.unwrap();
    let queue: serde_json::Value = serde_json::from_str(response_body(&response)).expect("queue JSON");
    assert!(queue["cards"].as_array().is_some_and(Vec::is_empty));
    assert!(db_path.is_file());
}

#[test]
fn serve_reconciles_missing_ask_on_startup() {
    let temp = TestDir::new();
    let db_path = temp.0.join("triage.db");
    insert_unresolved_ask(&db_path);
    let tmux_dir = fake_tmux_dir(&temp);
    let addr = unused_loopback_addr();
    let mut child = spawn_daemon(&temp, &tmux_dir, &db_path, addr);
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        let response = wait_for_response(&mut child.0, addr, "/v1/queue");
        assert!(response.is_ok(), "triage did not start with persisted state: {response:?}");
        let response = response.unwrap();
        let queue: serde_json::Value = serde_json::from_str(response_body(&response)).expect("queue JSON");
        if queue["cards"].as_array().is_some_and(Vec::is_empty) {
            break;
        }
        assert!(Instant::now() < deadline, "persisted Ask remained queued after startup");
        thread::sleep(Duration::from_millis(20));
    }

    let history = wait_for_response(&mut child.0, addr, "/v1/history?limit=50");
    assert!(history.is_ok(), "triage did not serve history: {history:?}");
    let history = history.unwrap();
    let history: serde_json::Value = serde_json::from_str(response_body(&history)).expect("history JSON");
    assert_eq!(history["cards"][0]["swipe"]["reason"].as_str(), Some("pane exited"));
}
