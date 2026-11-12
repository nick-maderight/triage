use super::{AppState, card_from_event};
use crate::db::Db;
use crate::events::{CloudEvent, Kind, Swipe};
use axum::http::StatusCode;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn host_without_port_matches_configured_host_port() {
    let host = "localhost".parse::<axum::http::uri::Authority>().unwrap();
    let mismatched_port = "localhost:3001"
        .parse::<axum::http::uri::Authority>()
        .unwrap();

    assert!(super::valid_host(&host, Some(3000), &[]));
    assert!(!super::valid_host(&mismatched_port, Some(3000), &[]));
}

#[test]
fn extra_allowed_host_is_accepted_only_when_configured() {
    let configured = ["inbox.example.test".to_owned()];
    let host = "inbox.example.test:7340"
        .parse::<axum::http::uri::Authority>()
        .unwrap();
    let evil = "evil.example:7340"
        .parse::<axum::http::uri::Authority>()
        .unwrap();
    let localhost = "localhost:7340"
        .parse::<axum::http::uri::Authority>()
        .unwrap();

    assert!(super::valid_host(&host, None, &configured));
    assert!(!super::valid_host(&host, None, &[]));
    assert!(!super::valid_host(&evil, None, &configured));
    assert!(super::valid_host(&localhost, None, &[]));
}

#[test]
fn roles_from_config_parses_role_names_as_role_selectors() {
    let config = r#"{"key":"modelRoles","value":{"smol":"provider/b","default":"provider/a","plan":"provider/c"},"type":"record","description":""}"#;
    let roles = super::roles_from_config(config);
    let entries: Vec<(&str, &str)> = roles
        .iter()
        .map(|role| (role.selector.as_str(), role.name.as_str()))
        .collect();
    assert_eq!(
        entries,
        [
            ("@default", "default role (provider/a)"),
            ("@plan", "plan role (provider/c)"),
            ("@smol", "smol role (provider/b)")
        ]
    );
    assert!(super::roles_from_config(r#"{"key":"modelRoles","value":null}"#).is_empty());
    assert!(super::roles_from_config("not json").is_empty());
}

#[test]
fn string_list_from_config_parses_value_arrays() {
    let config = r#"{"key":"enabledModels","value":["provider/a","provider/b"],"type":"list","description":""}"#;
    assert_eq!(super::string_list_from_config(config).unwrap(), ["provider/a", "provider/b"]);
    assert!(super::string_list_from_config(r#"{"value":null}"#).unwrap().is_empty());
    assert!(super::string_list_from_config("not json").is_none());
}

#[test]
fn execution_models_follow_cycle_order_and_name_missing_roles() {
    let models = super::roles_from_config(
        r#"{"key":"modelRoles","value":{"smol":"provider/b","default":"provider/a","plan":"provider/c"}}"#,
    );
    let cycle = ["smol", "default", "missing", "plan"].map(str::to_owned);
    let execution = super::execution_models(&models, &cycle);
    let entries: Vec<(&str, &str)> = execution
        .iter()
        .map(|model| (model.selector.as_str(), model.name.as_str()))
        .collect();
    assert_eq!(
        entries,
        [
            ("@smol", "smol role (provider/b)"),
            ("@default", "default role (provider/a)"),
            ("@missing", "missing role"),
            ("@plan", "plan role (provider/c)")
        ]
    );
}

#[test]
fn parse_catalog_keeps_only_enabled_selectors() {
    let catalog = r#"{"models":[
            {"selector":"provider/a","name":"Alpha"},
            {"selector":"provider/b","name":""},
            {"selector":"provider/c","name":"Gamma"}
        ]}"#;
    let enabled = vec!["provider/a".to_owned(), "provider/b".to_owned()];
    let models = super::parse_catalog(catalog, &enabled).unwrap();
    let entries: Vec<(&str, &str)> = models.iter().map(|m| (m.selector.as_str(), m.name.as_str())).collect();
    assert_eq!(entries, [("provider/a", "Alpha"), ("provider/b", "provider/b")]);
    assert_eq!(super::parse_catalog(catalog, &[]).unwrap().len(), 3);
    assert!(super::parse_catalog("not json", &enabled).is_none());
}

fn event_with_id(event_type: &str, source: &str, id: &str, data: serde_json::Value) -> CloudEvent {
    CloudEvent {
        specversion: "1.0".into(),
        id: id.into(),
        source: source.into(),
        r#type: event_type.into(),
        subject: None,
        time: None,
        priority: None,
        data,
    }
}

fn event(event_type: &str, source: &str, data: serde_json::Value) -> CloudEvent {
    event_with_id(event_type, source, "test-event", data)
}

fn test_plan_card(pane: &str) -> crate::events::Card {
    crate::events::Card {
        id: format!("plan-test-{}", pane.trim_start_matches('%')),
        kind: Kind::Plan,
        priority: 1,
        created_ms: crate::events::now_ms(),
        source: crate::events::Source {
            uri: format!("pane:{pane}"),
            label: pane.to_owned(),
            cwd: "/tmp".to_owned(),
            pane: Some(pane.to_owned()),
            ..crate::events::Source::default()
        },
        title: "Plan ready: test".to_owned(),
        star: crate::events::Star::default(),
        options: vec![
            crate::events::CardOption {
                label: "Approve".to_owned(),
                recommended: true,
                ..crate::events::CardOption::default()
            },
            crate::events::CardOption {
                label: "Refine".to_owned(),
                ..crate::events::CardOption::default()
            },
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

fn state_with(db: Db, launch: crate::cards::Launch) -> Arc<AppState> {
    let push = crate::push::Push::load(&db, &[]).unwrap();
    Arc::new(AppState {
        db,
        bus: tokio::sync::broadcast::channel(32).0,
        tracker: Mutex::new(crate::cards::Tracker::default()),
        launch,
        push,
        allowed_hosts: Vec::new(),
        media: crate::media::Roots::default(),
    })
}

fn state(db: Db, launch_session: Option<&str>) -> Arc<AppState> {
    state_with(
        db,
        crate::cards::Launch {
            session: launch_session.map(str::to_owned),
            ..crate::cards::Launch::default()
        },
    )
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn run_with_fake_tmux_child(test_name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    if std::env::var_os("TRIAGE_BUGHUNT_1792294661_FAKE_TMUX_CHILD").is_some() {
        return false;
    }
    let directory = TestDir::new("triage-fake-tmux");
    let bin = directory.0.join("bin");
    let home = directory.0.join("home");
    let config = directory.0.join("xdg-config");
    let data = directory.0.join("xdg-data");
    let cache = directory.0.join("xdg-cache");
    let state_dir = directory.0.join("xdg-state");
    for path in [&bin, &home, &config, &data, &cache, &state_dir] {
        std::fs::create_dir_all(path).unwrap();
    }
    let tmux = bin.join("tmux");
    std::fs::write(
            &tmux,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$TRIAGE_BUGHUNT_1792294661_TMUX_LOG\"\nprintf '%s\\n' \"$*\" >> \"$TRIAGE_BUGHUNT_1792294661_TMUX_LOG.args\"\nif [ \"$1\" = \"list-panes\" ]; then\n  arrivals=\"$TRIAGE_BUGHUNT_1792294661_TMUX_LOG.arrivals\"\n  release=\"$TRIAGE_BUGHUNT_1792294661_TMUX_LOG.release\"\n  # Hold only the first list-panes call, so the first answer blocks inside its card's answer lock.\n  if [ -p \"$arrivals\" ] && [ -p \"$release\" ]; then\n    counter=\"$TRIAGE_BUGHUNT_1792294661_TMUX_LOG.gate\"\n    seen=0\n    if [ -f \"$counter\" ]; then IFS= read -r seen < \"$counter\"; fi\n    seen=$((seen + 1))\n    printf '%s\\n' \"$seen\" > \"$counter\"\n    if [ \"$seen\" = 1 ]; then\n      printf x > \"$arrivals\"\n      IFS= read -r _ < \"$release\"\n    fi\n  fi\n  exit 0\nfi\nif [ \"$1\" = \"new-window\" ]; then printf '%%42\\n'; exit 0; fi\nexit 0\n",
        )
        .unwrap();
    let mut permissions = std::fs::metadata(&tmux).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&tmux, permissions).unwrap();
    let log = directory.0.join("tmux.log");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env("TRIAGE_BUGHUNT_1792294661_FAKE_TMUX_CHILD", "1")
        .env("TRIAGE_BUGHUNT_1792294661_TMUX_LOG", &log)
        .env("TMPDIR", &directory.0)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_STATE_HOME", &state_dir)
        .env("PATH", &bin)
        .env_remove("TMUX")
        .output()
        .unwrap();
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{transcript}");
    assert!(
        transcript.contains("1 passed"),
        "child ran no test: {transcript}"
    );
    true
}

#[cfg(unix)]
#[tokio::test]
async fn new_session_prefers_request_then_daemon_launch_settings() {
    if run_with_fake_tmux_child(
        "server::tests::new_session_prefers_request_then_daemon_launch_settings",
    ) {
        return;
    }
    let directory = TestDir::new("triage-new-session-target");
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let daemon_state = state_with(
        db,
        crate::cards::Launch {
            session: Some("daemon-default".to_owned()),
            model: Some("provider/daemon-model".to_owned()),
            thinking: Some("low".to_owned()),
            ..crate::cards::Launch::default()
        },
    );
    let mut args = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args.push(".args");
    let last_launch = || {
        std::fs::read_to_string(&args)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("new-window"))
            .last()
            .unwrap()
            .to_owned()
    };
    let request = super::SessionRequest {
        prompt: "Reply pong.".to_owned(),
        cwd: Some("/tmp".to_owned()),
        session: None,
        model: None,
        thinking: None,
        mode: None,
        executor: None,
        executor_thinking: None,
        engine: None,
        image: None,
    };
    let response = super::start_session(
        axum::extract::State(daemon_state.clone()),
        axum::Json(request),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let launch = last_launch();
    assert!(launch.contains("-t daemon-default:") && launch.contains("--model=provider/daemon-model --plan=provider/daemon-model --thinking=low --goal 'Reply pong."), "{launch}");

    let request = super::SessionRequest {
        prompt: "Reply pong.".to_owned(),
        cwd: Some("/tmp".to_owned()),
        session: Some("bill".to_owned()),
        model: Some("provider/chosen".to_owned()),
        thinking: Some("high".to_owned()),
        mode: None,
        executor: None,
        executor_thinking: None,
        engine: None,
        image: None,
    };
    let response =
        super::start_session(axum::extract::State(daemon_state), axum::Json(request)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let launch = last_launch();
    assert!(
        launch.contains("-t bill:")
            && launch.contains(
                "--model=provider/chosen --plan=provider/chosen --thinking=high --goal 'Reply pong."
            ),
        "{launch}"
    );

    let second_db = Db::open(&directory.0.join("second.sqlite")).unwrap();
    let bare = state(second_db, None);
    let request = super::SessionRequest {
        prompt: "Reply pong.".to_owned(),
        cwd: Some("/tmp".to_owned()),
        session: None,
        model: None,
        thinking: None,
        mode: None,
        executor: None,
        executor_thinking: None,
        engine: None,
        image: None,
    };
    let response = super::start_session(axum::extract::State(bare), axum::Json(request)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let launch = last_launch();
    assert!(
        launch.contains("-t triage:") && launch.contains(" omp --goal 'Reply pong."),
        "{launch}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn new_session_plan_mode_launches_with_config_overlay() {
    if run_with_fake_tmux_child("server::tests::new_session_plan_mode_launches_with_config_overlay")
    {
        return;
    }
    let directory = TestDir::new("triage-session-plan");
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let state = state(db, None);
    let defaulted: super::SessionRequest =
        serde_json::from_value(serde_json::json!({"prompt": "Default plan"})).unwrap();
    assert_eq!(defaulted.mode, None);
    let request = super::SessionRequest {
        prompt: "Prepare a plan.".to_owned(),
        cwd: Some("/tmp".to_owned()),
        session: Some("bill".to_owned()),
        model: None,
        thinking: None,
        mode: Some("plan".to_owned()),
        executor: None,
        executor_thinking: None,
        engine: None,
        image: None,
    };
    let response = super::start_session(axum::extract::State(state), axum::Json(request)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut args = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args.push(".args");
    let launch = std::fs::read_to_string(&args)
        .unwrap()
        .lines()
        .find(|line| line.starts_with("new-window"))
        .unwrap()
        .to_owned();
    let overlay = std::fs::read_dir(directory.0.join("briefs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("plan-mode-")
        })
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&overlay).unwrap(),
        "plan:\n  defaultOnStartup: true\n"
    );
    let brief = std::fs::read_dir(directory.0.join("briefs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("triage-brief-")
        })
        .unwrap();
    assert!(
        launch.contains(&format!(
            "--config '{}' 'Prepare a plan.' @'{}'",
            overlay.display(),
            brief.display()
        )),
        "{launch}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn new_plan_session_keeps_daemon_default_model_in_plan_mode() {
    if run_with_fake_tmux_child(
        "server::tests::new_plan_session_keeps_daemon_default_model_in_plan_mode",
    ) {
        return;
    }
    let directory = TestDir::new("triage-plan-session-model");
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let state = state_with(
        db,
        crate::cards::Launch {
            session: Some("daemon-default".to_owned()),
            model: Some("provider/daemon-model".to_owned()),
            thinking: Some("max".to_owned()),
            ..crate::cards::Launch::default()
        },
    );
    // The New session sheet leaves model and thinking blank, so the daemon defaults apply.
    let request = super::SessionRequest {
        prompt: "Prepare a plan.".to_owned(),
        cwd: Some("/tmp".to_owned()),
        session: None,
        model: None,
        thinking: None,
        mode: Some("plan".to_owned()),
        executor: None,
        executor_thinking: None,
        engine: None,
        image: None,
    };
    let response = super::start_session(axum::extract::State(state), axum::Json(request)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut args = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args.push(".args");
    let launch = std::fs::read_to_string(&args)
        .unwrap()
        .lines()
        .find(|line| line.starts_with("new-window"))
        .unwrap()
        .to_owned();
    // Without --plan, omp's plan mode moves the session onto its own plan role model.
    assert!(
        launch.contains(
            " --model=provider/daemon-model --plan=provider/daemon-model --thinking=max 'Prepare a plan.' @'",
        ),
        "{launch}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn new_session_plan_into_applies_effort_to_default_execution_model() {
    if run_with_fake_tmux_child(
        "server::tests::new_session_plan_into_applies_effort_to_default_execution_model",
    ) {
        return;
    }
    let directory = TestDir::new("triage-session-plan-into");
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let state = state_with(
        db,
        crate::cards::Launch {
            session: Some("daemon-default".to_owned()),
            model: Some("provider/daemon-model".to_owned()),
            thinking: Some("low".to_owned()),
            ..crate::cards::Launch::default()
        },
    );
    let request = super::SessionRequest {
        prompt: "Prepare a plan.".to_owned(),
        cwd: Some("/tmp".to_owned()),
        session: None,
        model: None,
        thinking: None,
        mode: Some("plan-into".to_owned()),
        executor: None,
        executor_thinking: Some("medium".to_owned()),
        engine: None,
        image: None,
    };
    let response = super::start_session(axum::extract::State(state), axum::Json(request)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut args = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args.push(".args");
    let launch = std::fs::read_to_string(&args)
        .unwrap()
        .lines()
        .find(|line| line.starts_with("new-window"))
        .unwrap()
        .to_owned();
    assert!(
        launch.contains(
            " --model=provider/daemon-model --plan=provider/daemon-model --thinking=low --plan-yolo --plan-yolo-into=@smol:medium 'Prepare a plan.' @'"
        ),
            "{launch}"
        );
    assert!(!launch.contains("--config"), "{launch}");
    assert!(!launch.contains("--goal"), "{launch}");
}

#[cfg(unix)]
#[test]
fn an_answer_request_takes_an_image_beside_its_answer() {
    let request: super::AnswerRequest = serde_json::from_str(
        r#"{"option":1,"note":"look","image":{"media_type":"image/png","data":"abc","name":"shot.png"}}"#,
    )
    .unwrap();
    assert_eq!(request.answer.option, Some(1));
    assert_eq!(request.answer.note.as_deref(), Some("look"));
    let image = request.image.unwrap();
    assert_eq!(image.media_type, "image/png");
    assert_eq!(image.data, "abc");
    assert_eq!(image.name.as_deref(), Some("shot.png"));
}

#[tokio::test]
async fn new_session_stores_an_image_and_hands_it_to_omp() {
    if run_with_fake_tmux_child("server::tests::new_session_stores_an_image_and_hands_it_to_omp") {
        return;
    }
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    use base64::Engine;
    let directory = TestDir::new("triage-session-image");
    let daemon_state = state(Db::open(&directory.0.join("triage.sqlite")).unwrap(), None);
    let request = super::SessionRequest {
        prompt: "Describe this.".to_owned(),
        cwd: Some("/tmp".to_owned()),
        session: None,
        engine: None,
        model: None,
        thinking: None,
        mode: Some("goal".to_owned()),
        executor: None,
        executor_thinking: None,
        image: Some(super::ImageRequest {
            media_type: "image/png".to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(PNG),
            name: Some("shot.png".to_owned()),
        }),
    };
    let response = super::start_session(axum::extract::State(daemon_state), axum::Json(request)).await;
    assert_eq!(response.status(), StatusCode::OK);

    let uploads = std::fs::read_dir(directory.0.join("uploads"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(uploads.len(), 1);
    assert_eq!(std::fs::read(&uploads[0]).unwrap(), PNG);
    let mut args = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args.push(".args");
    let launch = std::fs::read_to_string(&args).unwrap();
    assert!(launch.lines().any(|line| line.starts_with("new-window")), "{launch}");
    assert!(launch.contains(&format!("@{}", uploads[0].display())), "{launch}");

    let bad = super::SessionRequest {
        prompt: "Describe this.".to_owned(),
        cwd: Some("/tmp".to_owned()),
        session: None,
        engine: None,
        model: None,
        thinking: None,
        mode: Some("goal".to_owned()),
        executor: None,
        executor_thinking: None,
        image: Some(super::ImageRequest {
            media_type: "image/png".to_owned(),
            data: "not base64".to_owned(),
            name: None,
        }),
    };
    let bad_state = state(Db::open(&directory.0.join("second.sqlite")).unwrap(), None);
    let rejected =
        super::start_session(axum::extract::State(bad_state), axum::Json(bad)).await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn new_session_rejects_plan_into_effort_the_engine_cannot_take() {
    let directory = TestDir::new("triage-session-effort-validation");
    let state = state_with(
        Db::open(&directory.0.join("triage.sqlite")).unwrap(),
        crate::cards::Launch::default(),
    );
    let request = |engine: &str, executor: Option<&str>, effort: Option<&str>| {
        axum::Json(super::SessionRequest {
            prompt: "Prepare a plan.".to_owned(),
            cwd: Some("/tmp".to_owned()),
            session: None,
            engine: Some(engine.to_owned()),
            model: None,
            thinking: None,
            mode: Some("plan-into".to_owned()),
            executor: executor.map(str::to_owned),
            executor_thinking: effort.map(str::to_owned),
            image: None,
        })
    };
    for (engine, executor, effort) in [
        ("dsh", Some("deepseek-flash"), Some("medium")),
        ("dsh", None, Some("low")),
        ("omp", Some("provider/executor"), Some("sideways")),
    ] {
        let response = super::start_session(
            axum::extract::State(state.clone()),
            request(engine, executor, effort),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{engine} {effort:?}");
    }
}

struct HttpResponse {
    status: StatusCode,
    content_type: Option<String>,
    cache_control: Option<String>,
    etag: Option<String>,
    content_encoding: Option<String>,
    body: serde_json::Value,
    raw_body: Vec<u8>,
}

async fn start_http_server(
    state: Arc<AppState>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    start_http_server_with_router(super::router_with_notify(state, false, None)).await
}

async fn start_http_server_with_prewrite_gate(
    state: Arc<AppState>,
    gate: Arc<super::PreWriteGate>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    start_http_server_with_router(super::router_with_prewrite_gate(state, gate)).await
}

async fn start_http_server_with_router(
    router: axum::Router,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (address, server)
}
fn prewrite_gate(
    card_id: &str,
) -> (
    Arc<super::PreWriteGate>,
    tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    let (arrived, arrivals) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new(super::PreWriteGate {
        card_id: card_id.to_owned(),
        arrived,
        permits: Arc::new((Mutex::new(0), std::sync::Condvar::new())),
    });
    (gate, arrivals)
}

async fn wait_for_gate_arrival(
    arrivals: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    timeout: Duration,
) -> bool {
    matches!(
        tokio::time::timeout(timeout, arrivals.recv()).await,
        Ok(Some(()))
    )
}

#[cfg(unix)]
fn create_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(
        result,
        0,
        "could not create FIFO: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
fn fifo_arrivals(path: &Path) -> tokio::sync::mpsc::UnboundedReceiver<()> {
    use std::io::Read;

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    std::thread::spawn(move || {
        let mut byte = [0; 1];
        loop {
            match fifo.read_exact(&mut byte) {
                Ok(()) => {
                    if sender.send(()).is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    });
    receiver
}

async fn http_request(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: &str,
) -> HttpResponse {
    http_request_with_headers(
        address,
        method,
        path,
        body,
        "localhost",
        Some("application/json"),
        None,
    )
    .await
}

async fn http_request_with_headers(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    host: &str,
    content_type: Option<&str>,
    origin: Option<&str>,
) -> HttpResponse {
    let content_type = content_type
        .map(|value| format!("Content-Type: {value}\r\n"))
        .unwrap_or_default();
    let origin = origin
        .map(|value| format!("Origin: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{content_type}{origin}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, request.as_bytes())
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut response)
            .await
            .unwrap();
        response
    })
    .await
    .expect("HTTP request timed out");
    send_raw_response(response).await
}

async fn send_raw(address: std::net::SocketAddr, request: String) -> HttpResponse {
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, request.as_bytes())
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut response)
            .await
            .unwrap();
        response
    })
    .await
    .expect("HTTP request timed out");
    send_raw_response(response).await
}

async fn send_raw_response(response: Vec<u8>) -> HttpResponse {
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let headers = String::from_utf8_lossy(&response[..split]);
    let status = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let raw_body = response[split + 4..].to_vec();
    let header = |name: &str| {
        headers.lines().find_map(|line| {
            let (header, value) = line.split_once(':')?;
            header
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    HttpResponse {
        status: StatusCode::from_u16(status).unwrap(),
        content_type: header("content-type"),
        cache_control: header("cache-control"),
        etag: header("etag"),
        content_encoding: header("content-encoding"),
        body: serde_json::from_slice(&raw_body).unwrap_or_default(),
        raw_body,
    }
}

fn spawn_request(
    address: std::net::SocketAddr,
    method: &'static str,
    path: String,
    body: String,
) -> tokio::task::JoinHandle<HttpResponse> {
    tokio::spawn(async move { http_request(address, method, &path, &body).await })
}

fn assert_one_success_one_conflict(first: &HttpResponse, second: &HttpResponse) {
    let statuses = [first.status, second.status];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );
}

#[test]
fn finding_event_becomes_recommended_finding_card() {
    let card = card_from_event(&event(
            "triage.finding",
            "file:///workspace/src/main.rs",
            serde_json::json!({"title": "Review this", "star": {"task": "Review"}, "options": [{"label": "Fix"}]}),
        ))
        .unwrap();
    assert_eq!(card.kind, Kind::Finding);
    assert_eq!(card.priority, 2);
    assert_eq!(card.title, "Review this");
    assert_eq!(card.source.label, "main.rs");
    assert!(card.options[0].recommended);
    assert!(card.answerable);
    assert!(card.allow_other);
}

#[test]
fn info_event_moves_recommended_action_out_of_body() {
    let card = card_from_event(&event(
        "triage.info",
        "urn:test:report-action",
        serde_json::json!({
            "title": "Report complete",
            "body": "Report complete.\n\nRecommended action: Publish the release."
        }),
    ))
    .unwrap();
    assert_eq!(card.body.as_deref(), Some("Report complete."));
    assert_eq!(card.star.action, "Publish the release.");
    assert!(!card.body.as_deref().unwrap().contains("Recommended action"));
}

#[test]
fn unknown_event_makes_no_card() {
    let data = serde_json::json!({"detail": "unexpected"});
    assert!(card_from_event(&event("custom.notice", "tool://watcher", data)).is_none());
}

#[test]
fn finding_questions_become_card_questions_with_custom_text_allowed_by_default() {
    let card = card_from_event(&event(
        crate::events::T_FINDING,
        "urn:test:finding-questions",
        serde_json::json!({
            "title": "Two decisions",
            "questions": [
                {"label": "Supplier", "question": "Delete the supplier?", "options": [
                    {"label": "Do as recommended", "recommended": true},
                    {"label": "Skip"}
                ]},
                {"label": "Stock", "question": "Disable the stock?", "allow_other": false, "options": [
                    {"label": "Keep"}
                ]}
            ]
        }),
    ))
    .unwrap();
    assert_eq!(card.questions.len(), 2);
    assert_eq!(card.questions[0].label, "Supplier");
    assert_eq!(card.questions[0].question, "Delete the supplier?");
    assert_eq!(card.questions[0].options[0].label, "Do as recommended");
    assert!(card.questions[0].options[0].recommended);
    assert!(card.questions[0].allow_other);
    assert!(!card.questions[1].allow_other);
}
#[test]
fn finding_question_keeps_its_own_star_and_a_malformed_one_makes_no_card() {
    let data = |star: serde_json::Value| {
        serde_json::json!({
            "title": "Two fixes",
            "questions": [
                {"label": "Rotate the key", "question": "Title: Rotate the key", "star": star},
                {"label": "Supplier", "question": "Delete the supplier?"}
            ]
        })
    };
    let card = card_from_event(&event(
        crate::events::T_FINDING,
        "urn:test:finding-question-star",
        data(serde_json::json!({"situation": "The key is 120 days old.", "action": "Run rotate.sh."})),
    ))
    .unwrap();
    let queued = serde_json::to_value(&card).unwrap();
    assert_eq!(queued["questions"][0]["star"]["situation"], "The key is 120 days old.");
    assert_eq!(queued["questions"][0]["star"]["action"], "Run rotate.sh.");
    assert!(queued["questions"][1].get("star").is_none());
    assert!(
        card_from_event(&event(
            crate::events::T_FINDING,
            "urn:test:finding-question-star-broken",
            data(serde_json::json!("not an object")),
        ))
        .is_none()
    );
}
#[test]
fn finding_questions_are_optional_and_malformed_ones_make_no_card() {
    let plain = card_from_event(&event(
        crate::events::T_FINDING,
        "urn:test:finding-plain",
        serde_json::json!({"title": "Plain"}),
    ))
    .unwrap();
    assert!(plain.questions.is_empty());
    for questions in [
        serde_json::json!("not a list"),
        serde_json::json!([{"label": 3, "question": "Label is not text"}]),
    ] {
        let data = serde_json::json!({"title": "Broken", "questions": questions});
        assert!(card_from_event(&event(crate::events::T_FINDING, "urn:test:finding-broken", data)).is_none());
    }
    let info = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:info-questions",
        serde_json::json!({"title": "Report", "questions": "not a list"}),
    ))
    .unwrap();
    assert!(info.questions.is_empty());
}

#[tokio::test]
async fn serves_pwa_assets() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (address, server) = start_http_server(state).await;

    let worker = http_request(address, "GET", "/sw.js", "").await;
    assert_eq!(worker.status, StatusCode::OK);
    assert_eq!(
        worker.content_type.as_deref(),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(worker.cache_control.as_deref(), Some("no-cache"));

    let manifest = http_request(address, "GET", "/manifest.webmanifest", "").await;
    assert_eq!(manifest.status, StatusCode::OK);
    assert_eq!(
        manifest.content_type.as_deref(),
        Some("application/manifest+json")
    );
    assert_eq!(manifest.body["display"], "standalone");

    let icon = http_request(address, "GET", "/apple-touch-icon.png", "").await;
    assert_eq!(icon.status, StatusCode::OK);
    assert_eq!(icon.content_type.as_deref(), Some("image/png"));
    assert!(
        icon.raw_body
            .starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'])
    );
    server.abort();
}

#[tokio::test]
async fn serves_a_card_image_from_the_store() {
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let directory = TestDir::new("triage-image-route");
    let blobs = directory.0.join("blobs");
    std::fs::create_dir_all(&blobs).unwrap();
    let hash = "a".repeat(64);
    std::fs::write(blobs.join(&hash), PNG).unwrap();
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let mut card = test_plan_card("%9");
    card.id = "done:test:1".to_owned();
    card.kind = Kind::Info;
    card.images = vec![crate::events::CardImage {
        id: hash,
        media_type: "image/png".to_owned(),
        ..crate::events::CardImage::default()
    }];
    db.insert_card(&card).unwrap();
    let push = crate::push::Push::load(&db, &[]).unwrap();
    let state = Arc::new(AppState {
        db,
        bus: tokio::sync::broadcast::channel(8).0,
        tracker: Mutex::new(crate::cards::Tracker::default()),
        launch: crate::cards::Launch::default(),
        push,
        allowed_hosts: Vec::new(),
        media: crate::media::Roots {
            omp_blobs: blobs,
            dsh_objects: directory.0.join("objects"),
        },
    });
    let (address, server) = start_http_server(state).await;

    let card_image = http_request(address, "GET", "/v1/cards/done:test:1/image/0", "").await;
    assert_eq!(card_image.status, StatusCode::OK);
    assert_eq!(card_image.content_type.as_deref(), Some("image/png"));
    assert_eq!(card_image.cache_control.as_deref(), Some("private, max-age=31536000, immutable"));
    assert_eq!(card_image.raw_body, PNG);

    for path in [
        "/v1/cards/done:test:1/image/1",
        "/v1/cards/done:test:2/image/0",
    ] {
        let missing = http_request(address, "GET", path, "").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{path}");
    }
    server.abort();
}

#[tokio::test]
async fn history_rows_leave_card_bodies_out() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:history-slim",
        serde_json::json!({"title": "Slim me", "body": "A long final report"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    db.swipe(&Swipe {
        card: card.id.clone(),
        action: "done".into(),
        option: Some(0),
        text: None,
        note: None,
        until_ms: None,
        reason: None,
        at_ms: 1,
    })
    .unwrap();
    let (address, server) = start_http_server(state(db, None)).await;

    let history = http_request(address, "GET", "/v1/history?limit=50", "").await;
    let row = &history.body["cards"][0];
    assert_eq!(row["card"]["title"], "Slim me");
    assert_eq!(
        row["card"]["options"],
        serde_json::json!([{"label": "Acknowledge"}])
    );
    assert_eq!(row["swipe"]["option"], 0);
    assert!(
        row["card"].get("body").is_none(),
        "history rows carry no card body"
    );
    server.abort();
}

#[tokio::test]
async fn context_route_returns_thread_and_404_for_unknown_cards() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let anchor = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:context-thread",
        serde_json::json!({"title": "Thread anchor", "body": "The full report body"}),
    ))
    .unwrap();
    db.insert_card(&anchor).unwrap();
    db.swipe(&Swipe {
        card: anchor.id.clone(),
        action: "done".into(),
        option: None,
        text: None,
        note: Some("so what should i do?".into()),
        until_ms: None,
        reason: None,
        at_ms: 2,
    })
    .unwrap();
    let (address, server) = start_http_server(state(db, None)).await;

    let unknown = http_request(address, "GET", "/v1/cards/missing/context", "").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["error"], "card not found");

    // Real card ids carry `/` and `%`; the route only reaches the handler percent-encoded.
    let path = format!(
        "/v1/cards/{}/context",
        anchor
            .id
            .bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' =>
                    (byte as char).to_string(),
                _ => format!("%{byte:02X}"),
            })
            .collect::<String>()
    );
    let thread = http_request(address, "GET", &path, "").await;
    assert_eq!(thread.status, StatusCode::OK);
    assert_eq!(thread.body["anchor"], anchor.id);
    assert_eq!(thread.body["truncated"], 0);
    let item = &thread.body["items"][0];
    assert_eq!(item["card"]["title"], "Thread anchor");
    // The whole point of the thread: bodies and every swipe ride along.
    assert_eq!(item["card"]["body"], "The full report body");
    assert_eq!(item["swipes"][0]["note"], "so what should i do?");
    server.abort();
}

#[tokio::test]
async fn shell_files_revalidate_and_compress() {
    let (address, server) =
        start_http_server(state(Db::open(Path::new(":memory:")).unwrap(), None)).await;
    let get = |extra: &str| {
        format!("GET /app.js HTTP/1.1\r\nHost: localhost\r\n{extra}Connection: close\r\n\r\n")
    };

    let plain = send_raw(address, get("")).await;
    let etag = plain.etag.clone().expect("app.js carries an ETag");
    let revalidated = send_raw(address, get(&format!("If-None-Match: {etag}\r\n"))).await;
    let compressed = send_raw(address, get("Accept-Encoding: gzip\r\n")).await;

    assert_eq!(plain.status, StatusCode::OK);
    assert!(etag.starts_with("W/\""), "{etag}");
    assert_eq!(plain.cache_control.as_deref(), Some("no-cache"));
    assert_eq!(plain.content_encoding, None);
    assert_eq!(revalidated.status, StatusCode::NOT_MODIFIED);
    assert!(revalidated.raw_body.is_empty());
    assert_eq!(compressed.content_encoding.as_deref(), Some("gzip"));
    let queue = http_request(address, "GET", "/v1/queue", "").await;
    assert_eq!(queue.body["shell"], etag.as_str(), "the queue names the shell the daemon serves");
    server.abort();
}

#[tokio::test]
async fn ingest_rejects_empty_event_id() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let mut event = event("custom.notice", "urn:test:empty-id", serde_json::json!({}));
    event.id.clear();
    let (address, server) = start_http_server(state.clone()).await;
    let response = http_request(
        address,
        "POST",
        "/v1/ingest",
        &serde_json::to_string(&event).unwrap(),
    )
    .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert_eq!(response.body["error"], "event ID must not be empty");
    assert_eq!(state.db.version().unwrap(), 0);
    server.abort();
}

#[tokio::test]
async fn ingest_rejects_malformed_finding_questions_and_stores_unknown_events() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (address, server) = start_http_server(state.clone()).await;
    let malformed = event(
        crate::events::T_FINDING,
        "urn:test:malformed-questions",
        serde_json::json!({"title": "Broken", "questions": "not a list"}),
    );
    let rejected = http_request(address, "POST", "/v1/ingest", &serde_json::to_string(&malformed).unwrap()).await;
    assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
    assert_eq!(rejected.body["error"], "invalid finding questions");
    assert_eq!(state.db.version().unwrap(), 0);
    let unknown = event("custom.notice", "urn:test:unknown-questions", serde_json::json!({"questions": "not a list"}));
    let stored = http_request(address, "POST", "/v1/ingest", &serde_json::to_string(&unknown).unwrap()).await;
    assert_eq!(stored.status, StatusCode::ACCEPTED);
    assert_eq!(stored.body["stored"], true);
    assert!(state.db.queue(crate::events::now_ms()).unwrap().is_empty());
    server.abort();
}

/// The RFC 8291 receiver's public key and auth secret.
const UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

type PushHit = (String, axum::http::HeaderMap, Vec<u8>);

/// A push service at `{base}{id}` that records every delivery and answers `status`.
async fn stub_push_service(
    status: StatusCode,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<PushHit>) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let router = axum::Router::new().route(
        "/push/{id}",
        axum::routing::post(
            move |axum::extract::Path(id): axum::extract::Path<String>,
                  headers: axum::http::HeaderMap,
                  body: axum::body::Bytes| {
                let sender = sender.clone();
                async move {
                    let _ = sender.send((id, headers, body.to_vec()));
                    status
                }
            },
        ),
    );
    let (address, _server) = start_http_server_with_router(router).await;
    (format!("http://{address}/push/"), receiver)
}

fn subscription(endpoint: String, max_priority: u8) -> crate::push::Subscription {
    crate::push::Subscription {
        endpoint,
        p256dh: UA_PUBLIC.to_owned(),
        auth: AUTH.to_owned(),
        max_priority,
    }
}

#[tokio::test]
async fn push_routes_store_update_and_forget_a_subscription() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (address, server) = start_http_server(state.clone()).await;
    let lookup = "/v1/push?endpoint=https%3A%2F%2Fpush.example.test%2Fabc";
    for level in [2, 1] {
        let body = serde_json::json!({
            "endpoint": "https://push.example.test/abc",
            "keys": {"p256dh": UA_PUBLIC, "auth": AUTH},
            "max_priority": level,
        });
        let response = http_request(address, "POST", "/v1/push/subscribe", &body.to_string()).await;
        assert_eq!(response.status, StatusCode::OK, "{body}");
    }

    let stored = http_request(address, "GET", lookup, "").await;
    assert_eq!(
        stored.body["max_priority"], 1,
        "subscribing again must replace the level"
    );
    assert_eq!(stored.body["key"].as_str().unwrap().len(), 87);
    assert!(http_request(address, "GET", "/v1/push", "").await.body["max_priority"].is_null());

    let forgotten = http_request(
        address,
        "POST",
        "/v1/push/unsubscribe",
        r#"{"endpoint":"https://push.example.test/abc"}"#,
    )
    .await;
    assert_eq!(forgotten.status, StatusCode::OK);
    assert!(http_request(address, "GET", lookup, "").await.body["max_priority"].is_null());
    server.abort();
}

#[tokio::test]
async fn push_subscribe_rejects_what_a_browser_would_never_send() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (address, server) = start_http_server(state.clone()).await;
    let https = "https://push.example.test/abc";
    let cases = [
        (
            "http://push.example.test/abc",
            UA_PUBLIC,
            AUTH,
            2,
            "endpoint must be an https URL",
        ),
        (
            https,
            AUTH,
            AUTH,
            2,
            "p256dh must be an uncompressed P-256 public key",
        ),
        (https, UA_PUBLIC, UA_PUBLIC, 2, "auth must be 16 bytes"),
        (https, UA_PUBLIC, AUTH, 0, "max_priority must be 1 to 5"),
        (https, UA_PUBLIC, AUTH, 6, "max_priority must be 1 to 5"),
    ];
    for (endpoint, p256dh, auth, level, error) in cases {
        let body = serde_json::json!({"endpoint": endpoint, "keys": {"p256dh": p256dh, "auth": auth}, "max_priority": level});
        let response = http_request(address, "POST", "/v1/push/subscribe", &body.to_string()).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(response.body["error"], error, "{body}");
    }
    assert!(state.db.push_subscriptions().unwrap().is_empty());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingest_pushes_a_card_to_devices_whose_level_admits_it() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (base, mut pushes) = stub_push_service(StatusCode::CREATED).await;
    // Inserted directly, because the route accepts only https endpoints.
    state
        .db
        .upsert_push_subscription(&subscription(format!("{base}important"), 2))
        .unwrap();
    state
        .db
        .upsert_push_subscription(&subscription(format!("{base}urgent"), 1))
        .unwrap();
    let (address, server) = start_http_server(state.clone()).await;

    let response = http_request(
            address,
            "POST",
            "/v1/ingest",
            r#"{"specversion":"1.0","id":"push-check","source":"urn:test:push","type":"triage.finding","data":{"title":"Push check"}}"#,
        )
        .await;
    assert_eq!(response.status, StatusCode::ACCEPTED);
    let queued = state.db.queue(crate::events::now_ms()).unwrap();
    assert_eq!((queued[0].kind, queued[0].priority), (Kind::Finding, 2));

    let (id, headers, body) = tokio::time::timeout(Duration::from_secs(5), pushes.recv())
        .await
        .expect("the important device got no push")
        .unwrap();
    assert_eq!(id, "important");
    assert_eq!(headers["content-encoding"], "aes128gcm");
    assert_eq!(headers["ttl"], "86400");
    assert_eq!(headers["urgency"], "normal");
    let authorization = headers["authorization"].to_str().unwrap();
    assert!(authorization.starts_with("vapid t="), "{authorization}");
    assert!(
        authorization.ends_with(&format!(", k={}", state.push.public_key())),
        "{authorization}"
    );
    // Record size 4096, then the 65-byte sender key, after the 16-byte salt.
    assert_eq!(body[16..21], [0, 0, 16, 0, 65]);
    assert!(
        tokio::time::timeout(Duration::from_millis(500), pushes.recv())
            .await
            .is_err(),
        "the urgent-only device must not hear a priority-2 card"
    );
    server.abort();
}

#[tokio::test]
async fn push_test_drops_a_subscription_the_service_forgot() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (base, _pushes) = stub_push_service(StatusCode::GONE).await;
    let endpoint = format!("{base}forgotten");
    state
        .db
        .upsert_push_subscription(&subscription(endpoint.clone(), 2))
        .unwrap();
    let (address, server) = start_http_server(state.clone()).await;
    let body = serde_json::json!({"endpoint": endpoint}).to_string();

    assert_eq!(
        http_request(address, "POST", "/v1/push/test", &body)
            .await
            .status,
        StatusCode::GONE
    );
    assert!(state.db.push_subscription(&endpoint).unwrap().is_none());
    assert_eq!(
        http_request(address, "POST", "/v1/push/test", &body)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    server.abort();
}

#[tokio::test]
async fn same_event_id_from_different_sources_stores_and_looks_up_distinct_cards() {
    let first_event = event_with_id(
        crate::events::T_INFO,
        "source://first",
        "shared-id",
        serde_json::json!({"title": "First"}),
    );
    let second_event = event_with_id(
        crate::events::T_INFO,
        "source://second",
        "shared-id",
        serde_json::json!({"title": "Second"}),
    );
    let first_card = card_from_event(&first_event).unwrap();
    let second_card = card_from_event(&second_event).unwrap();
    assert_ne!(first_card.id, second_card.id);
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (address, server) = start_http_server(state.clone()).await;
    for event in [&first_event, &second_event] {
        let response = http_request(
            address,
            "POST",
            "/v1/ingest",
            &serde_json::to_string(event).unwrap(),
        )
        .await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        assert_eq!(response.body["stored"], true);
    }
    let queue = http_request(address, "GET", "/v1/queue", "").await;
    assert_eq!(queue.status, StatusCode::OK);
    assert_eq!(queue.body["cards"].as_array().unwrap().len(), 2);
    state
        .db
        .swipe(&Swipe {
            card: first_card.id.clone(),
            action: "skipped".into(),
            option: None,
            text: None,
            note: None,
            until_ms: None,
            reason: None,
            at_ms: crate::events::now_ms(),
        })
        .unwrap();
    assert_eq!(
        state
            .db
            .card(&first_card.id)
            .unwrap()
            .unwrap()
            .1
            .unwrap()
            .action,
        "skipped"
    );
    assert!(state.db.card(&second_card.id).unwrap().unwrap().1.is_none());
    server.abort();
}
#[tokio::test]
async fn duplicate_queued_finding_folds_without_queueing() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (address, server) = start_http_server(state.clone()).await;
    let post = |id: &str, title: &str| {
        let event = event_with_id(
            crate::events::T_FINDING,
            "jobs://runner1",
            id,
            serde_json::json!({"title": title}),
        );
        async move {
            http_request(address, "POST", "/v1/ingest", &serde_json::to_string(&event).unwrap()).await
        }
    };

    let first = post("first", "Alert flush failed").await;
    let duplicate = post("duplicate", "Alert flush failed").await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    assert_eq!(first.body["stored"], true);
    assert_eq!(duplicate.status, StatusCode::ACCEPTED);
    assert_eq!(duplicate.body["stored"], true);
    let first_id = card_from_event(&event_with_id(
        crate::events::T_FINDING,
        "jobs://runner1",
        "first",
        serde_json::json!({"title": "Alert flush failed"}),
    )).unwrap().id;
    let duplicate_id = card_from_event(&event_with_id(
        crate::events::T_FINDING,
        "jobs://runner1",
        "duplicate",
        serde_json::json!({"title": "Alert flush failed"}),
    )).unwrap().id;
    let queue = state.db.queue(crate::events::now_ms()).unwrap();
    let swipe = state.db.card(&duplicate_id).unwrap().unwrap().1.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].id, first_id);
    assert_eq!(swipe.action, "done");
    assert_eq!(swipe.reason.as_deref(), Some(format!("duplicate of {first_id}").as_str()));

    assert_eq!(post("different", "Another alert").await.body["stored"], true);
    assert_eq!(state.db.queue(crate::events::now_ms()).unwrap().len(), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folded_duplicate_sends_no_push() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let (base, mut pushes) = stub_push_service(StatusCode::CREATED).await;
    state.db.upsert_push_subscription(&subscription(format!("{base}duplicate"), 2)).unwrap();
    let (address, server) = start_http_server(state.clone()).await;
    let event = |id| event_with_id(
        crate::events::T_FINDING,
        "jobs://runner1",
        id,
        serde_json::json!({"title": "Alert flush failed"}),
    );
    for id in ["first", "duplicate"] {
        let response = http_request(
            address,
            "POST",
            "/v1/ingest",
            &serde_json::to_string(&event(id)).unwrap(),
        ).await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), pushes.recv()).await.unwrap().unwrap().0,
        "duplicate"
    );
    assert!(tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await.is_err());
    server.abort();
}
#[tokio::test]
async fn report_route_handles_unknown_and_paneless_cards() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let card = card_from_event(&event(crate::events::T_INFO, "urn:test:report", serde_json::json!({"title": "Report"}))).unwrap();
    state.db.insert_card(&card).unwrap();
    let (address, server) = start_http_server(state.clone()).await;
    let unknown = http_request(address, "POST", "/v1/cards/missing/report", "{}").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let response = http_request(address, "POST", &format!("/v1/cards/{}/report", card.id), "{}").await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.body["error"], "card has no pane");
    assert!(state.db.card(&card.id).unwrap().unwrap().1.is_none());
    server.abort();
}


#[tokio::test]
async fn report_route_rejects_handled_card() {
    let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
    let card = card_from_event(&event(crate::events::T_INFO, "urn:test:report-handled", serde_json::json!({"title": "Report"}))).unwrap();
    state.db.insert_card(&card).unwrap();
    state.db.swipe(&Swipe { card: card.id.clone(), action: "done".to_owned(), option: None, text: None, note: None, until_ms: None, reason: None, at_ms: crate::events::now_ms() }).unwrap();
    let (address, server) = start_http_server(state.clone()).await;
    let response = http_request(address, "POST", &format!("/v1/cards/{}/report", card.id), "{}").await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.body["error"], "card is not queued");
    server.abort();
}
#[tokio::test]
async fn failed_action_is_not_reported_as_already_handled() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let finding = card_from_event(&event(
        crate::events::T_FINDING,
        "urn:test:failed-action",
        serde_json::json!({"title": "Launch fails", "options": [{"label": "Fix"}]}),
    ))
    .unwrap();
    db.insert_card(&finding).unwrap();
    let id = finding.id;
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    let response = http_request(
        address,
        "POST",
        &format!("/v1/cards/{id}/answer"),
        r#"{"option":null,"text":null,"note":null}"#,
    )
    .await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.body["error"], "finding answer needs an option");
    assert!(state.db.card(&id).unwrap().unwrap().1.is_none());
    server.abort();
}

#[tokio::test]
async fn dispatch_rejects_unknown_mode_before_touching_the_card() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:dispatch",
        serde_json::json!({"title": "Dispatch"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    let id = card.id;
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    let path = format!("/v1/cards/{id}/dispatch");
    let unknown = http_request(address, "POST", &path, r#"{"mode":"fix"}"#).await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown.body["error"], "unknown mode");
    assert!(state.db.card(&id).unwrap().unwrap().1.is_none());
    let missing = http_request(
        address,
        "POST",
        "/v1/cards/missing/dispatch",
        r#"{"mode":"act"}"#,
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    server.abort();
}
#[tokio::test]
async fn read_endpoints_reject_hostile_host() {
    let (address, server) =
        start_http_server(state(Db::open(Path::new(":memory:")).unwrap(), None)).await;
    for path in [
        "/v1/queue",
        "/v1/history",
        "/v1/cards/x/context",
        "/v1/panes/1/peek",
    ] {
        let response =
            http_request_with_headers(address, "GET", path, "", "evil.example", None, None).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN, "{path}");
    }
    server.abort();
}
#[tokio::test]
async fn optional_body_posts_require_valid_host_origin_and_json_content_type() {
    let (address, server) =
        start_http_server(state(Db::open(Path::new(":memory:")).unwrap(), None)).await;
    let path = "/v1/cards/missing/dispatch";
    for (host, content_type, origin, expected) in [
        (
            "localhost",
            Some("application/json"),
            Some("https://evil.example"),
            StatusCode::FORBIDDEN,
        ),
        (
            "evil.example",
            Some("application/json"),
            None,
            StatusCode::FORBIDDEN,
        ),
        ("localhost", None, None, StatusCode::UNSUPPORTED_MEDIA_TYPE),
        (
            "localhost",
            Some("text/plain"),
            None,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
    ] {
        let response =
            http_request_with_headers(address, "POST", path, "", host, content_type, origin).await;
        assert_eq!(response.status, expected);
    }
    let accepted = http_request_with_headers(
        address,
        "POST",
        path,
        "",
        "localhost",
        Some("application/json; charset=utf-8"),
        Some("http://localhost"),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::BAD_REQUEST);
    server.abort();
}

#[tokio::test]
async fn plan_refinement_requires_nonempty_text_or_note_and_rejects_unknown_option() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = test_plan_card("%42");
    db.insert_card(&card).unwrap();
    let id = card.id.clone();
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    let path = format!("/v1/cards/{id}/answer");
    let empty = http_request(address, "POST", &path, r#"{"option":1,"text":"  "}"#).await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(empty.body["error"], "Refine requires nonempty text");
    let empty_note = http_request(address, "POST", &path, r#"{"option":1,"note":"  "}"#).await;
    assert_eq!(empty_note.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(empty_note.body["error"], "Refine requires nonempty text");
    let unknown = http_request(address, "POST", &path, r#"{"option":2}"#).await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(unknown.body["error"], "unknown plan option");
    assert!(state.db.card(&id).unwrap().unwrap().1.is_none());
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn plan_answer_route_relays_approve_and_refine() {
    if run_with_fake_tmux_child("server::tests::plan_answer_route_relays_approve_and_refine") {
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let tmux = PathBuf::from(std::env::var_os("PATH").unwrap()).join("tmux");
    std::fs::write(
        &tmux,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$TRIAGE_BUGHUNT_1792294661_TMUX_LOG.args"
pane=
for arg in "$@"; do
    case "$arg" in %*) pane=$arg ;; esac
done
state="$TRIAGE_BUGHUNT_1792294661_TMUX_LOG$pane"
case "$1" in
capture-pane)
    if [ -f "$state.picked" ]; then
        printf '⬢ Example Model · ◉ high · 🗺 Plan\n'
        exit 0
    fi
    printf '╭─ Plan Review ─╮\n'
    printf '├────────────────╮\n'
    printf '│ Plan mode - next step │\n'
    printf '│ continue with  ◂  smol  ◀ default ▶  slow  ▸ │\n'
    printf '│   ↳ Example Model │\n'
    if [ "$pane" = "%43" ]; then
        printf '│   Approve and execute │\n'
        printf '│   Approve and compact context │\n'
        printf '│   Approve and keep context (~8k / 1m) │\n'
        printf '│ ❯ Refine plan │\n'
    else
        printf '│ ❯ Approve and execute │\n'
        printf '│   Approve and compact context │\n'
        printf '│   Approve and keep context (~8k / 1m) │\n'
        printf '│   Refine plan │\n'
    fi
    printf '│   Save and quit │\n'
    printf '├────────────────╮\n'
    printf '│ ↑/↓ select · ⏎ confirm · ←/→ model · ⎋ cancel │\n'
    printf '╰────────────────╯\n'
    ;;
send-keys)
    case " $* " in *" Enter "*) : > "$state.picked" ;; esac
    ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&tmux).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&tmux, permissions).unwrap();

    let db = Db::open(Path::new(":memory:")).unwrap();
    let approve = test_plan_card("%42");
    let refine = test_plan_card("%43");
    db.insert_card(&approve).unwrap();
    db.insert_card(&refine).unwrap();
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    let approved = http_request(
        address,
        "POST",
        &format!("/v1/cards/{}/answer", approve.id),
        r#"{"option":0}"#,
    )
    .await;
    let refined = http_request(
        address,
        "POST",
        &format!("/v1/cards/{}/answer", refine.id),
        r#"{"option":1,"text":"Add rollback step"}"#,
    )
    .await;
    let mut args = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args.push(".args");
    let log = std::fs::read_to_string(args).unwrap();
    let approve_swipe = state.db.card(&approve.id).unwrap().unwrap().1.unwrap();
    let refine_swipe = state.db.card(&refine.id).unwrap().unwrap().1.unwrap();

    assert_eq!(approved.status, StatusCode::OK);
    assert_eq!(refined.status, StatusCode::OK);
    assert!(log.lines().any(|line| line == "send-keys -t %42 Enter"));
    assert!(!log.contains("Escape"), "{log}");
    let pick = log.find("send-keys -t %43 Enter").unwrap();
    let note = log
        .find("send-keys -t %43 -l -- Add rollback step")
        .unwrap();
    let submit = note + log[note..].find("send-keys -t %43 Enter").unwrap();
    assert!(pick < note && note < submit, "{log}");
    assert_eq!(approve_swipe.option, Some(0));
    assert_eq!(refine_swipe.option, Some(1));
    assert_eq!(refine_swipe.text.as_deref(), Some("Add rollback step"));
    server.abort();
}

#[tokio::test]
async fn plan_cards_appear_in_queue_counts_and_history() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let queued = test_plan_card("%42");
    let completed = test_plan_card("%43");
    db.insert_card(&queued).unwrap();
    db.insert_card(&completed).unwrap();
    let state = state(db, None);
    state
        .db
        .swipe(&Swipe {
            card: completed.id.clone(),
            action: "done".to_owned(),
            option: Some(0),
            text: None,
            note: None,
            until_ms: None,
            reason: Some("typed".to_owned()),
            at_ms: crate::events::now_ms(),
        })
        .unwrap();
    let (address, server) = start_http_server(state).await;

    let queue = http_request(address, "GET", "/v1/queue", "").await;
    assert_eq!(queue.body["counts"]["plan"], 1);
    assert!(
        queue.body["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|card| { card["id"].as_str() == Some(queued.id.as_str()) })
    );
    let history = http_request(address, "GET", "/v1/history?limit=50", "").await;
    assert!(
        history.body["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["card"]["id"].as_str() == Some(completed.id.as_str())
                    && entry["card"]["kind"].as_str() == Some("plan")
            })
    );
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn plan_dispatch_reports_mode_toggle_failure_as_422() {
    if run_with_fake_tmux_child("server::tests::plan_dispatch_reports_mode_toggle_failure_as_422") {
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let tmux = PathBuf::from(std::env::var_os("PATH").unwrap()).join("tmux");
    std::fs::write(
            &tmux,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$TRIAGE_BUGHUNT_1792294661_TMUX_LOG.args"
case "$1" in
list-panes) printf '%%42\tπ > session\tomp\t/dev/pts/42\tsession:1.0\t/tmp\tsession\t80\t24\t0\t\n' ;;
capture-pane) printf 'Plan mode is off\n' ;;
esac
"#,
        )
        .unwrap();
    let mut permissions = std::fs::metadata(&tmux).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&tmux, permissions).unwrap();

    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = test_plan_card("%42");
    db.insert_card(&card).unwrap();
    let id = card.id.clone();
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    let response = http_request(
        address,
        "POST",
        &format!("/v1/cards/{id}/dispatch"),
        r#"{"mode":"plan"}"#,
    )
    .await;
    let mut args = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args.push(".args");
    let log = std::fs::read_to_string(args).unwrap();
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        response.body["error"]
            .as_str()
            .unwrap()
            .contains("plan mode")
    );
    assert!(log.lines().any(|line| line == "send-keys -t %42 M-P"));
    assert!(state.db.card(&id).unwrap().unwrap().1.is_none());
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn plan_into_dispatch_plans_with_the_requested_model() {
    if run_with_fake_tmux_child("server::tests::plan_into_dispatch_plans_with_the_requested_model")
    {
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let tmux = PathBuf::from(std::env::var_os("PATH").unwrap()).join("tmux");
    std::fs::write(
        &tmux,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$TRIAGE_BUGHUNT_1792294661_TMUX_LOG.args"
case "$1" in
list-panes) printf '' ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&tmux).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&tmux, permissions).unwrap();

    let directory = TestDir::new("triage-plan-into-chosen");
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:plan-into-model",
        serde_json::json!({"title": "Plan into with the requested model"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    let state = state_with(
        db,
        crate::cards::Launch {
            session: None,
            model: Some("provider/daemon-model".to_owned()),
            thinking: None,
            ..crate::cards::Launch::default()
        },
    );
    let (address, server) = start_http_server(state).await;
    let response = http_request(
        address,
        "POST",
        &format!("/v1/cards/{}/dispatch", card.id),
        r#"{"mode":"plan-into","model":"provider/chosen","executor":"provider/executor","executor_thinking":"high"}"#,
    )
    .await;
    let mut args = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args.push(".args");
    let launch = std::fs::read_to_string(&args)
        .unwrap()
        .lines()
        .find(|line| line.starts_with("new-window"))
        .unwrap()
        .to_owned();
    assert_eq!(response.status, StatusCode::OK);
    assert!(
        launch.contains("--model=provider/chosen --plan=provider/chosen"),
        "{launch}"
    );
    assert!(
        launch.contains("--plan-yolo --plan-yolo-into=provider/executor:high '# Plan into: Plan into with the requested model' @'"),
        "{launch}"
    );
    assert!(!launch.contains("provider/daemon-model"), "{launch}");
    server.abort();
}

#[tokio::test]
async fn dispatch_rejects_executor_outside_plan_into_and_bad_launch_choices() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:dispatch-models",
        serde_json::json!({"title": "Dispatch"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    let id = card.id;
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    let path = format!("/v1/cards/{id}/dispatch");
    let outside = http_request(address, "POST", &path, r#"{"mode":"act","executor":"provider/x"}"#).await;
    assert_eq!(outside.status, StatusCode::BAD_REQUEST);
    assert_eq!(outside.body["error"], "executor needs mode plan-into");
    let bad = http_request(address, "POST", &path, r#"{"mode":"plan-into","executor":"x; rm -rf ~"}"#).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.body["error"], "invalid model x; rm -rf ~");
    let thinking = http_request(address, "POST", &path, r#"{"mode":"act","thinking":"sideways"}"#).await;
    assert_eq!(thinking.status, StatusCode::BAD_REQUEST);
    assert_eq!(thinking.body["error"], "unknown thinking level sideways");
    let effort_outside =
        http_request(address, "POST", &path, r#"{"mode":"act","executor_thinking":"low"}"#).await;
    assert_eq!(effort_outside.status, StatusCode::BAD_REQUEST);
    assert_eq!(effort_outside.body["error"], "executor thinking needs mode plan-into");
    let dsh_effort_alone = http_request(
        address,
        "POST",
        &path,
        r#"{"mode":"plan-into","engine":"dsh","executor_thinking":"low"}"#,
    )
    .await;
    assert_eq!(dsh_effort_alone.status, StatusCode::BAD_REQUEST);
    assert_eq!(dsh_effort_alone.body["error"], "execution effort needs an execution model");
    let bad_effort = http_request(
        address,
        "POST",
        &path,
        r#"{"mode":"plan-into","executor":"provider/executor","executor_thinking":"sideways"}"#,
    )
    .await;
    assert_eq!(bad_effort.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_effort.body["error"], "unknown execution thinking level sideways");
    assert!(state.db.card(&id).unwrap().unwrap().1.is_none());
    server.abort();
}
#[cfg(unix)]
#[tokio::test]
async fn finding_answer_launches_with_requested_mode_model_and_thinking() {
    if run_with_fake_tmux_child("server::tests::finding_answer_launches_with_requested_mode_model_and_thinking") {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let tmux = PathBuf::from(std::env::var_os("PATH").unwrap()).join("tmux");
    std::fs::write(
        &tmux,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$TRIAGE_BUGHUNT_1792294661_TMUX_LOG.args"
case "$1" in list-panes) printf '' ;; esac
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&tmux).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&tmux, permissions).unwrap();
    let directory = TestDir::new("triage-finding-launch-choices");
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_FINDING,
        "urn:test:finding-launch-choices",
        serde_json::json!({"title": "Finding launch choices", "options": [{"label": "Do as recommended"}]}),
    )).unwrap();
    db.insert_card(&card).unwrap();
    let state = state_with(db, crate::cards::Launch { session: Some("triage".to_owned()), model: None, thinking: None, ..crate::cards::Launch::default() });
    let (address, server) = start_http_server(state.clone()).await;
    let path = format!("/v1/cards/{}/answer", card.id);
    let response = http_request(address, "POST", &path,
        r#"{"option":0,"mode":"act","model":"provider/chosen","thinking":"high"}"#).await;
    let mut args_path = std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap();
    args_path.push(".args");
    let launch = std::fs::read_to_string(&args_path).unwrap();
    assert_eq!(response.status, StatusCode::OK);
    assert!(launch.contains("--model=provider/chosen --plan=provider/chosen --thinking=high --goal"), "{launch}");
    assert!(state.db.card(&card.id).unwrap().unwrap().1.is_some());
    server.abort();
}

#[tokio::test]
async fn finding_answer_rejects_unknown_mode_and_thinking_level() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(crate::events::T_FINDING, "urn:test:finding-validation", serde_json::json!({"title": "Finding"}))).unwrap();
    db.insert_card(&card).unwrap();
    let id = card.id;
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    let path = format!("/v1/cards/{id}/answer");
    let mode = http_request(address, "POST", &path, r#"{"option":0,"mode":"fix"}"#).await;
    assert_eq!(mode.status, StatusCode::BAD_REQUEST);
    assert_eq!(mode.body["error"], "unknown mode");
    let thinking = http_request(address, "POST", &path, r#"{"option":0,"thinking":"sideways"}"#).await;
    assert_eq!(thinking.status, StatusCode::BAD_REQUEST);
    assert_eq!(thinking.body["error"], "unknown thinking level sideways");
    assert!(state.db.card(&id).unwrap().unwrap().1.is_none());
    server.abort();
}


#[tokio::test]
async fn new_session_rejects_unknown_mode_and_executor_outside_plan_into() {
    let directory = TestDir::new("triage-session-mode-rejects");
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let state = state(db, None);
    let (address, server) = start_http_server(state).await;
    let unknown = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","mode":"yolo"}"#,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown.body["error"], "unknown mode");
    let executor = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","mode":"goal","executor":"provider/x"}"#,
    )
    .await;
    assert_eq!(executor.status, StatusCode::BAD_REQUEST);
    assert_eq!(executor.body["error"], "executor needs mode plan-into");
    let bad = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","mode":"plan-into","executor":"x; rm -rf ~"}"#,
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let stale = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","plan":true}"#,
    )
    .await;
    assert_eq!(stale.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        stale.body["error"].as_str().is_some_and(|error| error.contains("unknown field `plan`")),
        "{:?}",
        stale.body
    );
    assert!(!directory.0.join("briefs").exists());
    server.abort();
}

#[tokio::test]
async fn skip_and_later_reject_missing_or_nonqueued_cards() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let skipped = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:skip",
        serde_json::json!({"title": "Skip"}),
    ))
    .unwrap();
    let deferred = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:later",
        serde_json::json!({"title": "Later"}),
    ))
    .unwrap();
    db.insert_card(&skipped).unwrap();
    db.insert_card(&deferred).unwrap();
    let skipped_id = skipped.id;
    let deferred_id = deferred.id;
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    for (path, body) in [
        ("/v1/cards/missing/skip".to_owned(), String::new()),
        (
            "/v1/cards/missing/later".to_owned(),
            r#"{"minutes":1}"#.to_owned(),
        ),
    ] {
        assert_eq!(
            http_request(address, "POST", &path, &body).await.status,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        http_request(address, "POST", &format!("/v1/cards/{skipped_id}/skip"), "")
            .await
            .status,
        StatusCode::OK,
    );
    let skipped_later = http_request(
        address,
        "POST",
        &format!("/v1/cards/{skipped_id}/later"),
        r#"{"minutes":1}"#,
    )
    .await;
    assert_eq!(skipped_later.status, StatusCode::CONFLICT);
    assert_eq!(skipped_later.body["error"], "card is not queued");
    assert_eq!(
        http_request(
            address,
            "POST",
            &format!("/v1/cards/{deferred_id}/later"),
            r#"{"minutes":60}"#,
        )
        .await
        .status,
        StatusCode::OK,
    );
    let deferred_swipe = state.db.card(&deferred_id).unwrap().unwrap().1.unwrap();
    let deferred_skip = http_request(
        address,
        "POST",
        &format!("/v1/cards/{deferred_id}/skip"),
        "",
    )
    .await;
    assert_eq!(deferred_skip.status, StatusCode::CONFLICT);
    assert_eq!(deferred_skip.body["error"], "card is not queued");
    let current_swipe = state.db.card(&deferred_id).unwrap().unwrap().1.unwrap();
    assert_eq!(current_swipe.action, "later");
    assert_eq!(current_swipe.until_ms, deferred_swipe.until_ms);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_skip_and_later_only_accept_one_action() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:skip-later-race",
        serde_json::json!({"title": "Concurrent actions"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    let id = card.id;
    let state = state(db, None);
    let (gate, mut arrivals) = prewrite_gate(&id);
    let (address, server) = start_http_server_with_prewrite_gate(state.clone(), gate.clone()).await;
    let before = state.db.version().unwrap();
    let skip = spawn_request(
        address,
        "POST",
        format!("/v1/cards/{id}/skip"),
        String::new(),
    );
    assert!(wait_for_gate_arrival(&mut arrivals, Duration::from_secs(5)).await);
    let later = spawn_request(
        address,
        "POST",
        format!("/v1/cards/{id}/later"),
        r#"{"minutes":1}"#.into(),
    );
    let _ = wait_for_gate_arrival(&mut arrivals, Duration::from_secs(1)).await;
    gate.release(2);
    let (skip, later) = tokio::join!(skip, later);
    let skip = skip.unwrap();
    let later = later.unwrap();
    assert_one_success_one_conflict(&skip, &later);
    assert_eq!(state.db.version().unwrap(), before + 1);
    let swipe = state.db.card(&id).unwrap().unwrap().1.unwrap();
    assert!(swipe.action == "skipped" || swipe.action == "later");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_info_ack_and_skip_only_write_once() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:info-skip-race",
        serde_json::json!({"title": "Acknowledge"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    let id = card.id;
    let state = state(db, None);
    let (gate, mut arrivals) = prewrite_gate(&id);
    let (address, server) = start_http_server_with_prewrite_gate(state.clone(), gate.clone()).await;
    let before = state.db.version().unwrap();
    let skip = spawn_request(
        address,
        "POST",
        format!("/v1/cards/{id}/skip"),
        String::new(),
    );
    assert!(wait_for_gate_arrival(&mut arrivals, Duration::from_secs(5)).await);
    let mut answer = spawn_request(
        address,
        "POST",
        format!("/v1/cards/{id}/answer"),
        r#"{"option":0}"#.into(),
    );
    let early_answer = tokio::time::timeout(Duration::from_secs(1), &mut answer).await;
    gate.release(1);
    let answer = match early_answer {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => panic!("answer route failed: {error}"),
        Err(_) => answer.await.unwrap(),
    };
    let skip = skip.await.unwrap();
    assert_one_success_one_conflict(&answer, &skip);
    assert_eq!(state.db.version().unwrap(), before + 1);
    let swipe = state.db.card(&id).unwrap().unwrap().1.unwrap();
    assert!(swipe.action == "done" || swipe.action == "skipped");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_info_ack_and_later_only_write_once() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:info-later-race",
        serde_json::json!({"title": "Acknowledge"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    let id = card.id;
    let state = state(db, None);
    let (gate, mut arrivals) = prewrite_gate(&id);
    let (address, server) = start_http_server_with_prewrite_gate(state.clone(), gate.clone()).await;
    let before = state.db.version().unwrap();
    let later = spawn_request(
        address,
        "POST",
        format!("/v1/cards/{id}/later"),
        r#"{"minutes":1}"#.into(),
    );
    assert!(wait_for_gate_arrival(&mut arrivals, Duration::from_secs(5)).await);
    let mut answer = spawn_request(
        address,
        "POST",
        format!("/v1/cards/{id}/answer"),
        r#"{"option":0}"#.into(),
    );
    let early_answer = tokio::time::timeout(Duration::from_secs(1), &mut answer).await;
    gate.release(1);
    let answer = match early_answer {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => panic!("answer route failed: {error}"),
        Err(_) => answer.await.unwrap(),
    };
    let later = later.await.unwrap();
    assert_one_success_one_conflict(&answer, &later);
    assert_eq!(state.db.version().unwrap(), before + 1);
    let swipe = state.db.card(&id).unwrap().unwrap().1.unwrap();
    assert!(swipe.action == "done" || swipe.action == "later");
    server.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_finding_answers_launch_one_window_and_conflict() {
    if run_with_fake_tmux_child(
        "server::tests::concurrent_finding_answers_launch_one_window_and_conflict",
    ) {
        return;
    }
    let directory = TestDir::new("triage-finding-answer");
    let database = directory.0.join("triage.sqlite");
    let db = Db::open(&database).unwrap();
    let card = card_from_event(&event(
        crate::events::T_FINDING,
        "urn:test:finding-race",
        serde_json::json!({"title": "Review", "options": [{"label": "Fix"}]}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    let id = card.id;
    let state = state(db, Some("triage-test"));
    let log = PathBuf::from(std::env::var_os("TRIAGE_BUGHUNT_1792294661_TMUX_LOG").unwrap());
    let mut arrivals_name = log.as_os_str().to_os_string();
    arrivals_name.push(".arrivals");
    let arrivals_path = PathBuf::from(arrivals_name);
    let mut release_name = log.as_os_str().to_os_string();
    release_name.push(".release");
    let release_path = PathBuf::from(release_name);
    create_fifo(&arrivals_path);
    create_fifo(&release_path);
    let mut arrivals = fifo_arrivals(&arrivals_path);
    let release = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&release_path)
        .unwrap();
    let before = state.db.version().unwrap();
    let (address, server) = start_http_server(state.clone()).await;
    let path = format!("/v1/cards/{id}/answer");
    let body = r#"{"option":0,"note":"race"}"#.to_owned();
    let first = spawn_request(address, "POST", path.clone(), body.clone());
    assert!(wait_for_gate_arrival(&mut arrivals, Duration::from_secs(5)).await);
    let second = spawn_request(address, "POST", path, body);
    let _ = wait_for_gate_arrival(&mut arrivals, Duration::from_secs(1)).await;
    drop(release);
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert_one_success_one_conflict(&first, &second);
    assert_eq!(state.db.version().unwrap(), before + 2);
    let swipe = state.db.card(&id).unwrap().unwrap().1.unwrap();
    assert_eq!(swipe.action, "done");
    assert_eq!(swipe.reason.as_deref(), Some("launched %42"));
    let calls = std::fs::read_to_string(log).unwrap();
    assert_eq!(
        calls.lines().filter(|call| *call == "new-window").count(),
        1
    );
    server.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn poll_broadcasts_when_deferred_card_becomes_due() {
    if run_with_fake_tmux_child("server::tests::poll_broadcasts_when_deferred_card_becomes_due") {
        return;
    }
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:snooze-expiry",
        serde_json::json!({"title": "Due later"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    let id = card.id.clone();
    db.swipe(&Swipe {
        card: id.clone(),
        action: "later".into(),
        option: None,
        text: None,
        note: None,
        until_ms: Some(crate::events::now_ms() + 1_500),
        reason: None,
        at_ms: crate::events::now_ms(),
    })
    .unwrap();
    let (bus, mut updates) = tokio::sync::broadcast::channel(16);
    let push = crate::push::Push::load(&db, &[]).unwrap();
    let state = Arc::new(AppState {
        db,
        bus,
        tracker: Mutex::new(crate::cards::Tracker::default()),
        launch: crate::cards::Launch::default(),
        push,
        allowed_hosts: Vec::new(),
        media: crate::media::Roots::default(),
    });
    let server = tokio::spawn(super::serve(
        state.clone(),
        "127.0.0.1:0".parse().unwrap(),
        false,
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(500), updates.recv())
            .await
            .is_err(),
        "no queue update should arrive before snooze expiry",
    );
    let update = tokio::time::timeout(Duration::from_secs(5), updates.recv()).await;
    server.abort();
    assert!(matches!(update, Ok(Ok(_))));
    assert!(
        state
            .db
            .queue(crate::events::now_ms())
            .unwrap()
            .iter()
            .any(|card| card.id == id)
    );
}

#[tokio::test]
async fn history_limit_is_capped_at_500() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    for index in 0..501 {
        let card = card_from_event(&event(
            crate::events::T_INFO,
            &format!("urn:test:history-{index}"),
            serde_json::json!({"title": "History"}),
        ))
        .unwrap();
        db.insert_card(&card).unwrap();
        db.swipe(&Swipe {
            card: card.id,
            action: "done".into(),
            option: None,
            text: None,
            note: None,
            until_ms: None,
            reason: None,
            at_ms: index,
        })
        .unwrap();
    }
    let (address, server) = start_http_server(state(db, None)).await;
    let response = http_request(
        address,
        "GET",
        &format!("/v1/history?limit={}", usize::MAX),
        "",
    )
    .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["cards"].as_array().unwrap().len(), 500);
    server.abort();
}

#[tokio::test]
async fn reload_returns_handled_card_to_queue_as_a_fresh_copy() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:reload",
        serde_json::json!({"title": "Reload me"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    db.swipe(&Swipe {
        card: card.id.clone(),
        action: "done".into(),
        option: None,
        text: None,
        note: None,
        until_ms: None,
        reason: None,
        at_ms: 1,
    })
    .unwrap();
    let original_id = card.id;
    let (address, server) = start_http_server(state(db, None)).await;
    let path = format!("/v1/cards/{original_id}/reload");

    let first = http_request(address, "POST", &path, "").await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.body["ok"], true);
    let copy_id = first.body["card"].as_str().unwrap().to_owned();
    assert!(copy_id.starts_with(&format!("reload:{original_id}:")));

    let queue = http_request(address, "GET", "/v1/queue", "").await;
    let queued = queue.body["cards"].as_array().unwrap();
    let copy = queued
        .iter()
        .find(|card| card["id"] == copy_id)
        .expect("copy is queued");
    assert_eq!(copy["title"], "Reload me");
    assert_eq!(copy["priority"], card.priority);
    assert!(queued.iter().all(|card| card["id"] != original_id.as_str()));

    let history = http_request(address, "GET", "/v1/history", "").await;
    assert!(
        history.body["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["card"]["id"] == original_id.as_str())
    );

    let second = http_request(address, "POST", &path, "").await;
    assert_eq!(second.status, StatusCode::CONFLICT);
    assert_eq!(second.body["error"], "card is already queued");

    let skip = http_request(address, "POST", &format!("/v1/cards/{copy_id}/skip"), "").await;
    assert_eq!(skip.status, StatusCode::OK);
    let third = http_request(address, "POST", &path, "").await;
    assert_eq!(third.status, StatusCode::OK);
    assert_ne!(third.body["card"].as_str().unwrap(), copy_id);

    let unknown = http_request(address, "POST", "/v1/cards/missing/reload", "").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["error"], "card not found");
    server.abort();
}

struct HistoryTraceGate {
    reached: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
    fired: std::sync::atomic::AtomicBool,
}

static HISTORY_TRACE_GATE: std::sync::LazyLock<Mutex<Option<Arc<HistoryTraceGate>>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

fn history_trace_slot() -> &'static Mutex<Option<Arc<HistoryTraceGate>>> {
    &HISTORY_TRACE_GATE
}

fn pause_after_history_rows(event: rusqlite::trace::TraceEvent<'_>) {
    let rusqlite::trace::TraceEvent::Profile(statement, _) = event else {
        return;
    };
    if !statement.sql().starts_with("SELECT c.data, s.data") {
        return;
    }
    let gate = history_trace_slot().lock().unwrap().clone();
    let Some(gate) = gate else {
        return;
    };
    if gate.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(reached) = gate.reached.lock().unwrap().take() {
        let _ = reached.send(());
    }
    let _ = gate.resume.lock().unwrap().recv();
}

struct HistoryTraceGuard<'a> {
    db: &'a Db,
    resume: std::sync::mpsc::SyncSender<()>,
}

impl Drop for HistoryTraceGuard<'_> {
    fn drop(&mut self) {
        let _ = self.resume.send(());
        *history_trace_slot().lock().unwrap() = None;
        self.db
            .conn
            .lock()
            .unwrap()
            .trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_count_and_rows_use_one_snapshot() {
    let directory = TestDir::new("triage-history-snapshot");
    let database = directory.0.join("triage.sqlite");
    let db = Db::open(&database).unwrap();
    let card = card_from_event(&event(
        crate::events::T_INFO,
        "urn:test:history-snapshot",
        serde_json::json!({"title": "Snapshot"}),
    ))
    .unwrap();
    db.insert_card(&card).unwrap();
    db.swipe(&Swipe {
        card: card.id.clone(),
        action: "done".into(),
        option: None,
        text: None,
        note: None,
        until_ms: None,
        reason: None,
        at_ms: 900,
    })
    .unwrap();
    let writer = Db::open(db.path()).unwrap();
    let state = state(db, None);
    let (address, server) = start_http_server(state.clone()).await;
    let (reached_sender, reached_receiver) = tokio::sync::oneshot::channel();
    let (resume_sender, resume_receiver) = std::sync::mpsc::sync_channel(1);
    *history_trace_slot().lock().unwrap() = Some(Arc::new(HistoryTraceGate {
        reached: Mutex::new(Some(reached_sender)),
        resume: Mutex::new(resume_receiver),
        fired: std::sync::atomic::AtomicBool::new(false),
    }));
    state.db.conn.lock().unwrap().trace_v2(
        rusqlite::trace::TraceEventCodes::SQLITE_TRACE_PROFILE,
        Some(pause_after_history_rows),
    );
    let trace_guard = HistoryTraceGuard {
        db: &state.db,
        resume: resume_sender.clone(),
    };
    let request = tokio::spawn(http_request(
        address,
        "GET",
        "/v1/history?limit=50&today_start_ms=1000&today_end_ms=2000",
        "",
    ));
    tokio::time::timeout(Duration::from_secs(5), reached_receiver)
        .await
        .expect("history rows SELECT did not reach trace seam")
        .unwrap();
    writer
        .swipe(&Swipe {
            card: card.id,
            action: "done".into(),
            option: None,
            text: None,
            note: None,
            until_ms: None,
            reason: None,
            at_ms: 1_100,
        })
        .unwrap();
    resume_sender.send(()).unwrap();
    let response = request.await.unwrap();
    drop(trace_guard);
    assert_eq!(response.status, StatusCode::OK);
    let body = response.body;
    assert_eq!(body["cards"][0]["swipe"]["at_ms"], 900);
    assert_eq!(body["today_count"], 0);
    server.abort();
}

#[tokio::test]
async fn models_answers_immediately_while_the_first_load_runs() {
    let _load_in_flight = super::MODELS.write().unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        super::models(
            axum::extract::State(state(Db::open(Path::new(":memory:")).unwrap(), None)),
            axum::extract::Query(super::ModelsQuery { engine: None }),
        ),
    )
    .await
    .expect("models endpoint must answer while the catalog load runs");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["models"], serde_json::json!([]));
    assert_eq!(parsed["execution_models"], serde_json::json!([]));
}

#[cfg(unix)]
#[test]
fn models_reload_only_when_omp_config_changes() {
    if run_with_fake_tmux_child("server::tests::models_reload_only_when_omp_config_changes") {
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let agent = TestDir::new("triage-omp-agent");
    let catalog = agent.0.join("catalog.json");
    let config = agent.0.join("config.yml");
    // The child's PATH holds only the fake bin directory, so the fake omp calls cat by absolute path.
    let omp = PathBuf::from(std::env::var_os("PATH").unwrap()).join("omp");
    std::fs::write(
        &omp,
        format!(
            r#"#!/bin/sh
case "$*" in
"models --json") /bin/cat '{catalog}' ;;
"config get "*) printf '%s\n' '{{"value":null}}' ;;
*) exit 1 ;;
esac
"#,
            catalog = catalog.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&omp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&config, "modelRoles: {}\n").unwrap();
    let serve_catalog = |selector: &str| {
        std::fs::write(
            &catalog,
            format!(r#"{{"models":[{{"selector":"{selector}","name":"Model"}}]}}"#),
        )
        .unwrap();
    };
    let snapshot = || -> Vec<String> {
        super::MODELS.read().unwrap().models.iter().map(|model| model.selector.clone()).collect()
    };
    let mut loaded = None;

    serve_catalog("provider/old");
    assert!(super::refresh_models(Some(agent.0.as_path()), &mut loaded));
    assert_eq!(snapshot(), ["provider/old"]);

    serve_catalog("provider/new");
    assert!(
        !super::refresh_models(Some(agent.0.as_path()), &mut loaded),
        "an unchanged omp config must not rerun the slow catalog load"
    );
    assert_eq!(snapshot(), ["provider/old"]);

    std::fs::File::options()
        .write(true)
        .open(&config)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(10))
        .unwrap();
    assert!(
        super::refresh_models(Some(agent.0.as_path()), &mut loaded),
        "a changed omp config must reload the catalog"
    );
    assert_eq!(snapshot(), ["provider/new"]);
}
#[cfg(unix)]
#[test]
fn models_retries_failed_loads_without_another_config_change() {
    if run_with_fake_tmux_child("server::tests::models_retries_failed_loads_without_another_config_change") {
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let agent = TestDir::new("triage-omp-retry");
    let catalog = agent.0.join("catalog.json");
    let fail_models = agent.0.join("fail-models");
    let fail_cycle = agent.0.join("fail-cycle");
    let config = agent.0.join("config.yml");
    let omp = PathBuf::from(std::env::var_os("PATH").unwrap()).join("omp");
    std::fs::write(
        &omp,
        format!(
            r#"#!/bin/sh
case "$*" in
"models --json")
  if [ -f '{fail_models}' ]; then exit 1; fi
  /bin/cat '{catalog}'
  ;;
"config get modelRoles --json") printf '%s\n' '{{"value":{{"smol":"provider/b","default":"provider/a"}}}}' ;;
"config get enabledModels --json") printf '%s\n' '{{"value":[]}}' ;;
"config get cycleOrder --json")
  if [ -f '{fail_cycle}' ]; then printf '%s\n' 'not json'; else printf '%s\n' '{{"value":["smol","default"]}}'; fi
  ;;
*) exit 1 ;;
esac
"#,
            catalog = catalog.display(),
            fail_models = fail_models.display(),
            fail_cycle = fail_cycle.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&omp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&config, "modelRoles: {}\n").unwrap();
    std::fs::write(
        &catalog,
        r#"{"models":[{"selector":"provider/recovered","name":"Recovered model"}]}"#,
    )
    .unwrap();

    let snapshot = || {
        let catalog = super::MODELS.read().unwrap();
        (
            catalog.models.iter().map(|model| model.selector.clone()).collect::<Vec<_>>(),
            catalog.execution_models.iter().map(|model| model.selector.clone()).collect::<Vec<_>>(),
        )
    };
    let mut loaded = None;

    std::fs::write(&fail_models, "fail").unwrap();
    std::fs::write(&fail_cycle, "fail").unwrap();
    assert!(super::refresh_models(Some(agent.0.as_path()), &mut loaded));
    assert!(loaded.is_none());
    assert_eq!(snapshot(), (vec!["@default".to_owned(), "@smol".to_owned()], Vec::<String>::new()));

    std::fs::remove_file(&fail_models).unwrap();
    std::fs::remove_file(&fail_cycle).unwrap();
    assert!(super::refresh_models(Some(agent.0.as_path()), &mut loaded));
    assert_eq!(
        snapshot(),
        (
            vec!["@default".to_owned(), "@smol".to_owned(), "provider/recovered".to_owned()],
            vec!["@smol".to_owned(), "@default".to_owned()]
        )
    );

    std::fs::File::options()
        .write(true)
        .open(&config)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(10))
        .unwrap();
    std::fs::write(&fail_cycle, "fail").unwrap();
    assert!(super::refresh_models(Some(agent.0.as_path()), &mut loaded));
    assert!(snapshot().1.is_empty());
    std::fs::remove_file(&fail_cycle).unwrap();
    assert!(super::refresh_models(Some(agent.0.as_path()), &mut loaded));
    assert_eq!(snapshot().1, ["@smol", "@default"]);

    std::fs::write(
        &catalog,
        r#"{"models":[{"selector":"provider/not-reloaded","name":"Stale by design"}]}"#,
    )
    .unwrap();
    assert!(!super::refresh_models(Some(agent.0.as_path()), &mut loaded));
    assert!(snapshot().0.contains(&"provider/recovered".to_owned()));
}
#[test]
fn pane_directories_list_each_directory_once_most_shared_first() {
    assert_eq!(
        super::pane_directories([
            "/srv/b", "/srv/a", "", "/srv/c", "/srv/c", "/srv/b", "/srv/d", "/srv/c"
        ]),
        ["/srv/c", "/srv/b", "/srv/a", "/srv/d"]
    );
}

/// A host file for a bridge that answers one model catalog and nothing else.
async fn dsh_catalog_host(catalog: serde_json::Value) -> (TestDir, PathBuf) {
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(move || {
            let catalog = catalog.clone();
            async move { axum::Json(catalog) }
        }),
    );
    let (address, _server) = start_http_server_with_router(app).await;
    let dir = TestDir::new("triage-dsh-catalog");
    let host = dir.0.join("dsh-host.json");
    std::fs::write(
        &host,
        serde_json::json!({"api": format!("http://{address}"), "secret": "s3cret", "pid": 1})
            .to_string(),
    )
    .unwrap();
    (dir, host)
}

fn dsh_models_state(db: Db, host: PathBuf, model: Option<&str>) -> Arc<AppState> {
    state_with(
        db,
        crate::cards::Launch {
            engine: crate::cards::Engine::Dsh,
            dsh_host: Some(host),
            model: model.map(str::to_owned),
            ..crate::cards::Launch::default()
        },
    )
}

#[tokio::test]
async fn dsh_models_follow_the_host_catalog() {
    let (_dir, host) = dsh_catalog_host(serde_json::json!({
        "default": "opencode-go/deepseek-v4.1-flash",
        "models": [
            {"provider": "opencode-go", "model": "deepseek-v4.1-flash",
             "selector": "opencode-go/deepseek-v4.1-flash", "name": "DeepSeek V4.1 Flash",
             "efforts": ["off", "low", "high", "max"]},
            {"provider": "opencode-go", "model": "beam-free",
             "selector": "opencode-go/beam-free", "name": "Beam Free", "efforts": []}
        ]
    }))
    .await;
    let db = Db::open(Path::new(":memory:")).unwrap();
    let response = super::models(
        axum::extract::State(dsh_models_state(db, host, None)),
        axum::extract::Query(super::ModelsQuery { engine: Some("dsh".to_owned()) }),
    )
    .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["engine"], "dsh");
    assert_eq!(parsed["default"], "opencode-go/deepseek-v4.1-flash");
    assert_eq!(parsed["thinking"], serde_json::json!(["off", "low", "high", "max"]));
    let selectors: Vec<&str> = parsed["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["selector"].as_str().unwrap())
        .collect();
    assert_eq!(selectors, ["opencode-go/deepseek-v4.1-flash", "opencode-go/beam-free"]);
    let names: Vec<&str> = parsed["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["DeepSeek V4.1 Flash", "Beam Free"]);
    // The efforts ride along, so a picker never offers a level dsh would refuse.
    assert_eq!(
        parsed["models"][0]["efforts"],
        serde_json::json!(["off", "low", "high", "max"])
    );
    assert_eq!(parsed["models"][1]["efforts"], serde_json::json!([]));
}

#[tokio::test]
async fn dsh_models_keep_a_daemon_model_the_host_offers() {
    let (_dir, host) = dsh_catalog_host(serde_json::json!({
        "default": "opencode-go/deepseek-v4.1-flash",
        "models": [
            {"provider": "opencode-go", "model": "deepseek-v4.1-flash",
             "selector": "opencode-go/deepseek-v4.1-flash", "name": "DeepSeek V4.1 Flash"},
            {"provider": "opencode-go", "model": "beam-free",
             "selector": "opencode-go/beam-free", "name": "Beam Free"}
        ]
    }))
    .await;
    let db = Db::open(Path::new(":memory:")).unwrap();
    let response = super::models(
        axum::extract::State(dsh_models_state(db, host, Some("opencode-go/beam-free"))),
        axum::extract::Query(super::ModelsQuery { engine: Some("dsh".to_owned()) }),
    )
    .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["default"], "opencode-go/beam-free");
}

#[tokio::test]
async fn dsh_models_drop_a_daemon_model_the_host_no_longer_offers() {
    let (_dir, host) = dsh_catalog_host(serde_json::json!({
        "default": "opencode-go/deepseek-v4.1-flash",
        "models": [
            {"provider": "opencode-go", "model": "deepseek-v4.1-flash",
             "selector": "opencode-go/deepseek-v4.1-flash", "name": "DeepSeek V4.1 Flash"}
        ]
    }))
    .await;
    let db = Db::open(Path::new(":memory:")).unwrap();
    let response = super::models(
        axum::extract::State(dsh_models_state(db, host, Some("deepseek-official/deepseek-flash"))),
        axum::extract::Query(super::ModelsQuery { engine: Some("dsh".to_owned()) }),
    )
    .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["default"], "opencode-go/deepseek-v4.1-flash");
}

#[tokio::test]
async fn dsh_models_are_empty_without_a_host() {
    let dir = TestDir::new("triage-dsh-nohost");
    let db = Db::open(Path::new(":memory:")).unwrap();
    let response = super::models(
        axum::extract::State(dsh_models_state(db, dir.0.join("missing.json"), None)),
        axum::extract::Query(super::ModelsQuery { engine: Some("dsh".to_owned()) }),
    )
    .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["models"], serde_json::json!([]));
    assert!(parsed["default"].is_null());
    assert_eq!(parsed["thinking"], serde_json::json!(["off", "low", "high", "max"]));
}

#[tokio::test]
async fn models_refuse_an_unknown_engine() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let response = super::models(
        axum::extract::State(state(db, None)),
        axum::extract::Query(super::ModelsQuery { engine: Some("bogus".to_owned()) }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn engine_switch_drops_the_daemon_model_and_keeps_a_requested_thinking_level() {
    let mut launch = crate::cards::Launch {
        model: Some("@smol".to_owned()),
        thinking: Some("low".to_owned()),
        ..crate::cards::Launch::default()
    };
    super::apply_launch_choices(&mut launch, Some("dsh"), None, Some("high")).unwrap();
    assert_eq!(launch.engine, crate::cards::Engine::Dsh);
    assert_eq!(launch.model, None);
    assert_eq!(launch.thinking.as_deref(), Some("high"));
}

#[test]
fn engine_switch_refuses_an_unknown_engine() {
    let mut launch = crate::cards::Launch::default();
    assert_eq!(
        super::apply_launch_choices(&mut launch, Some("jcode"), None, None),
        Err("unknown engine jcode".to_owned())
    );
    assert_eq!(launch.engine, crate::cards::Engine::Omp);
}

#[tokio::test]
async fn dsh_session_refuses_an_omp_model_selector() {
    let directory = TestDir::new("triage-dsh-session-model");
    let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
    let (address, server) = start_http_server(state(db, None)).await;
    let response = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","engine":"dsh","model":"@smol"}"#,
    )
    .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert_eq!(response.body["error"], "unknown dsh model @smol");
    assert!(!directory.0.join("briefs").exists());
    server.abort();
}

/// A dsh bridge that holds every launch for `delay` and then answers `reply`. It lists no
/// sessions, so a tracker tick against it is harmless.
async fn slow_launch_bridge(
    delay: std::time::Duration,
    reply: (StatusCode, serde_json::Value),
) -> (TestDir, PathBuf, Arc<std::sync::atomic::AtomicUsize>) {
    let launches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&launches);
    let app = axum::Router::new()
        .route(
            "/v1/sessions",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"pid": 4242, "items": [], "planDecisions": true}))
            }),
        )
        .route(
            "/v1/pending",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"pid": 4242, "items": []})) }),
        )
        .route(
            "/v1/launch",
            axum::routing::post(move || {
                let reply = reply.clone();
                let counted = Arc::clone(&counted);
                async move {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    (reply.0, axum::Json(reply.1))
                }
            }),
        );
    let (address, _server) = start_http_server_with_router(app).await;
    let dir = TestDir::new("triage-slow-launch");
    let host = dir.0.join("dsh-host.json");
    std::fs::write(
        &host,
        serde_json::json!({"api": format!("http://{address}"), "secret": "s3cret", "pid": 4242})
            .to_string(),
    )
    .unwrap();
    (dir, host, launches)
}

fn dsh_launch_state(db: Db, host: PathBuf) -> Arc<AppState> {
    state_with(
        db,
        crate::cards::Launch {
            engine: crate::cards::Engine::Dsh,
            dsh_host: Some(host),
            ..crate::cards::Launch::default()
        },
    )
}

#[tokio::test]
async fn new_session_answers_starting_before_a_slow_launch_finishes() {
    let (dir, host, launches) = slow_launch_bridge(
        std::time::Duration::from_secs(5),
        (StatusCode::OK, serde_json::json!({"sessionId": "session-slow"})),
    )
    .await;
    let db = Db::open(&dir.0.join("triage.sqlite")).unwrap();
    let state = dsh_launch_state(db, host);
    let (address, server) = start_http_server(Arc::clone(&state)).await;
    let started = std::time::Instant::now();
    let response = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","engine":"dsh"}"#,
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(response.status, StatusCode::ACCEPTED);
    assert_eq!(response.body["ok"], true);
    assert_eq!(response.body["starting"], true);
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "Start waited for the launch: {elapsed:?}"
    );
    assert_eq!(launches.load(std::sync::atomic::Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn new_session_answers_the_pane_a_fast_launch_returns() {
    let (dir, host, _launches) = slow_launch_bridge(
        std::time::Duration::ZERO,
        (StatusCode::OK, serde_json::json!({"sessionId": "session-fast"})),
    )
    .await;
    let db = Db::open(&dir.0.join("triage.sqlite")).unwrap();
    let state = dsh_launch_state(db, host);
    let (address, server) = start_http_server(Arc::clone(&state)).await;
    let response = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","engine":"dsh"}"#,
    )
    .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["pane"], "dsh:session-fast");
    server.abort();
}

#[tokio::test]
async fn a_launch_that_fails_after_start_lands_in_the_inbox() {
    let (dir, host, _launches) = slow_launch_bridge(
        std::time::Duration::from_secs(3),
        (
            StatusCode::BAD_GATEWAY,
            serde_json::json!({"error": "dsh bridge unreachable: connection reset by peer"}),
        ),
    )
    .await;
    let db = Db::open(&dir.0.join("triage.sqlite")).unwrap();
    let state = dsh_launch_state(db, host);
    let (address, server) = start_http_server(Arc::clone(&state)).await;
    let response = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","engine":"dsh"}"#,
    )
    .await;
    assert_eq!(response.status, StatusCode::ACCEPTED);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let queued = state.db.queue(crate::events::now_ms()).unwrap();
        if let Some(card) = queued.iter().find(|card| card.title == "Could not start the session") {
            let body = card.body.as_deref().unwrap_or_default();
            assert!(body.contains("connection reset by peer"), "{body}");
            assert!(body.contains("Reply pong."), "{body}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no launch failure card in the inbox"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    server.abort();
}

#[tokio::test]
async fn new_session_refuses_a_bad_mode_before_it_waits_for_the_launch() {
    let db = Db::open(&TestDir::new("triage-session-bad-mode").0.join("triage.sqlite")).unwrap();
    let state = state(db, None);
    let (address, server) = start_http_server(Arc::clone(&state)).await;
    let response = http_request(
        address,
        "POST",
        "/v1/sessions",
        r#"{"prompt":"Reply pong.","engine":"dsh","mode":"plan-into","executor_thinking":"max"}"#,
    )
    .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert_eq!(response.body["error"], "execution effort needs an execution model");
    server.abort();
}

#[tokio::test]
async fn dsh_send_refuses_a_command_prefix_before_reaching_the_bridge() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let launch = crate::cards::Launch { engine: crate::cards::Engine::Dsh, ..crate::cards::Launch::default() };
    let (address, server) = start_http_server(state_with(db, launch)).await;
    let response = http_request(address, "POST", "/v1/dsh/x/send", r#"{"text":"/goal x"}"#).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.body["error"], "text starts with a blocked command prefix");
    server.abort();
}

#[tokio::test]
async fn dsh_open_without_a_host_says_dsh_is_not_running() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let launch = crate::cards::Launch { engine: crate::cards::Engine::Dsh, ..crate::cards::Launch::default() };
    let (address, server) = start_http_server(state_with(db, launch)).await;
    let response = http_request(address, "POST", "/v1/dsh/open", "{}").await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.body["error"], "dsh is not running");
    server.abort();
}
