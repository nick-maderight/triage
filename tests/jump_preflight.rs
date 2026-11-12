use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("triage-jump-preflight-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Response {
    status: u16,
    body: String,
}

#[cfg(unix)]
fn install_tmux(directory: &TestDir, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = directory.0.join(name);
    std::fs::create_dir_all(&bin).unwrap();
    let tmux = bin.join("tmux");
    std::fs::write(&tmux, script).unwrap();
    let mut permissions = std::fs::metadata(&tmux).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(tmux, permissions).unwrap();
    bin
}

#[cfg(unix)]
fn isolate_server(command: &mut Command, directory: &TestDir, path: &Path) {
    let home = directory.0.join("home");
    let config = directory.0.join("xdg-config");
    let data = directory.0.join("xdg-data");
    let cache = directory.0.join("xdg-cache");
    let state_dir = directory.0.join("xdg-state");
    for path in [&home, &config, &data, &cache, &state_dir] {
         std::fs::create_dir_all(path).unwrap();
    }
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", data)
        .env("XDG_CACHE_HOME", cache)
        .env("XDG_STATE_HOME", state_dir)
        .env("TMPDIR", &directory.0)
        .env("PATH", path)
        .env_remove("TMUX");
}

fn server_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}

#[cfg(unix)]
fn start_server(address: SocketAddr, database: &Path, directory: &TestDir, path: &Path) -> Server {
    let address = address.to_string();
    let database = database.to_str().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_triage"));
    command
        .args(["serve", "--listen", &address, "--db", database])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    isolate_server(&mut command, directory, path);
    Server(command.spawn().unwrap())
}

fn request(address: SocketAddr, method: &str, path: &str, body: &str) -> std::io::Result<Response> {
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("invalid HTTP response"))?;
    let headers = String::from_utf8_lossy(&response[..split]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| std::io::Error::other("invalid HTTP status"))?;
    Ok(Response {
        status,
        body: String::from_utf8_lossy(&response[split + 4..]).into_owned(),
    })
}

#[cfg(unix)]
fn request_when_ready(
    address: SocketAddr,
    server: &mut Server,
    method: &str,
    path: &str,
    body: &str,
) -> Response {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match request(address, method, path, body) {
            Ok(response) => return response,
            Err(_) if Instant::now() < deadline && server.0.try_wait().unwrap().is_none() => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("server did not answer {method} {path}: {error}"),
        }
    }
}

#[cfg(unix)]
fn assert_error(response: Response, status: u16, message: &str) {
    assert_eq!(response.status, status);
    let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["error"], message);
}

#[cfg(unix)]
#[test]
fn pane_routes_return_404_for_missing_panes_and_preserve_other_statuses() {
    let directory = TestDir::new();
    let empty_list = install_tmux(
        &directory,
        "empty-list",
        "#!/bin/sh\nif [ \"$1\" = \"list-panes\" ]; then exit 0; fi\nprintf '%s\\n' 'tmux command failed' >&2\nexit 1\n",
    );
    let address = server_address();
    let database = directory.0.join("triage.db");
    let mut server = start_server(address, &database, &directory, &empty_list);
    for (method, path, body) in [
        ("GET", "/v1/panes/missing/peek", ""),
        ("POST", "/v1/panes/missing/send", r#"{"text":"hello"}"#),
        ("POST", "/v1/panes/missing/jump", ""),
    ] {
        assert_error(
            request_when_ready(address, &mut server, method, path, body),
            404,
            "pane not found",
        );
    }

    drop(server);
    let empty_path = directory.0.join("empty-path");
    std::fs::create_dir_all(&empty_path).unwrap();
    let mut server = start_server(address, &database, &directory, &empty_path);
    for (method, path, body, expected) in [
        ("GET", "/v1/panes/missing/peek", "", 400),
        ("POST", "/v1/panes/missing/send", r#"{"text":"hello"}"#, 409),
    ] {
        assert_eq!(
            request_when_ready(address, &mut server, method, path, body).status,
            expected,
            "{method} {path}",
        );
    }
}

#[cfg(unix)]
#[test]
fn jump_returns_500_when_pane_listing_fails() {
    let directory = TestDir::new();
    let empty_path = directory.0.join("empty-path");
    std::fs::create_dir_all(&empty_path).unwrap();
    let address = server_address();
    let database = directory.0.join("triage.db");
    let mut server = start_server(address, &database, &directory, &empty_path);
    assert_eq!(
        request_when_ready(address, &mut server, "POST", "/v1/panes/%2512/jump", "").status,
        500,
    );
}

#[cfg(unix)]
#[test]
fn jump_returns_404_when_pane_disappears_after_preflight() {
    const PANE: &str = "%12\tpane\tbash\t/dev/pts/1\t0:0.0\t/tmp\ttest\t80\t24\t0\t\n";
    let directory = TestDir::new();
    let path = install_tmux(
        &directory,
        "vanishing-pane",
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"list-panes\" ]; then printf '%s' '{PANE}'; exit 0; fi\nif [ \"$1\" = \"display\" ]; then printf '%s\\n' \"can't find pane: %12\" >&2; exit 1; fi\nexit 0\n"
        ),
    );
    let address = server_address();
    let database = directory.0.join("triage.db");
    let mut server = start_server(address, &database, &directory, &path);
    assert_error(
        request_when_ready(address, &mut server, "POST", "/v1/panes/%2512/jump", ""),
        404,
        "pane not found",
    );
}

#[cfg(unix)]
#[test]
fn jump_keeps_bad_request_for_other_tmux_failures() {
    const PANE: &str = "%12\tpane\tbash\t/dev/pts/1\t0:0.0\t/tmp\ttest\t80\t24\t0\t\n";
    let directory = TestDir::new();
    let path = install_tmux(
        &directory,
        "failed-jump",
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"list-panes\" ]; then printf '%s' '{PANE}'; exit 0; fi\nprintf '%s\\n' 'tmux command failed' >&2\nexit 1\n"
        ),
    );
    let address = server_address();
    let database = directory.0.join("triage.db");
    let mut server = start_server(address, &database, &directory, &path);
    assert_error(
        request_when_ready(address, &mut server, "POST", "/v1/panes/%2512/jump", ""),
        400,
        "tmux command failed",
    );
}
