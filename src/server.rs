//! HTTP server for the decision inbox.
//! Routes are documented in README.md under HTTP routes.
//! Embedded assets use a shared ETag. Accepted requests get gzip responses, except fonts, images and SSE.
//! Every write broadcasts on `bus` after it commits.

use crate::cards::Tracker;
use crate::db::Db;
use axum::{extract::State, response::IntoResponse};
use tower_http::compression::{predicate::NotForContentType, CompressionLayer, DefaultPredicate, Predicate};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub struct AppState {
    pub db: Db,
    pub bus: broadcast::Sender<i64>,
    pub tracker: Mutex<Tracker>,
    /// Where and how triage launches omp sessions, from `triage serve` flags.
    pub launch: crate::cards::Launch,
    /// Host names accepted besides loopback, from `serve --allow-host`.
    pub allowed_hosts: Vec<String>,
    /// Roots of the session stores that keep card images.
    pub media: crate::media::Roots,
}

#[cfg(test)]
struct PreWriteGate {
    card_id: String,
    arrived: tokio::sync::mpsc::UnboundedSender<()>,
    permits: Arc<(Mutex<usize>, std::sync::Condvar)>,
}

#[cfg(test)]
impl PreWriteGate {
    fn pause(&self, card_id: &str) {
        if self.card_id != card_id {
            return;
        }
        let _ = self.arrived.send(());
        let (permits, ready) = &*self.permits;
        let mut permits = permits.lock().unwrap();
        while *permits == 0 {
            permits = ready.wait(permits).unwrap();
        }
        *permits -= 1;
    }

    fn release(&self, count: usize) {
        let (permits, ready) = &*self.permits;
        *permits.lock().unwrap() += count;
        ready.notify_all();
    }
}


#[derive(Clone)]
struct HostPolicy {
    port: Option<u16>,
    hosts: Arc<[String]>,
}

fn valid_host(authority: &axum::http::uri::Authority, port: Option<u16>, hosts: &[String]) -> bool {
    let host = authority.host().trim_start_matches('[').trim_end_matches(']').trim_end_matches('.');
    let allowed = !authority.as_str().contains('@')
        && (host.eq_ignore_ascii_case("localhost")
            || host.eq_ignore_ascii_case("127.0.0.1")
            || host.eq_ignore_ascii_case("::1")
            || hosts.iter().any(|allowed| host.eq_ignore_ascii_case(allowed.trim_end_matches('.'))));
    allowed && port.is_none_or(|port| authority.port_u16().is_none_or(|authority_port| authority_port == port))
}

fn same_authority(left: &axum::http::uri::Authority, right: &axum::http::uri::Authority) -> bool {
    let left_host = left.host().trim_start_matches('[').trim_end_matches(']');
    let right_host = right.host().trim_start_matches('[').trim_end_matches(']');
    left_host.eq_ignore_ascii_case(right_host) && left.port_u16() == right.port_u16()
}

async fn validate_host_origin(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let policy = request.extensions().get::<HostPolicy>().cloned().expect("host policy layer is required");
    let host = request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok());
    let Some(host) = host.filter(|host| valid_host(host, policy.port, &policy.hosts)) else {
        return error_response(axum::http::StatusCode::FORBIDDEN, "invalid host");
    };

    let method = request.method();
    if method != axum::http::Method::GET
        && method != axum::http::Method::HEAD
        && method != axum::http::Method::OPTIONS
    {
        if let Some(origin) = request.headers().get(axum::http::header::ORIGIN) {
            let matches_host = origin
                .to_str()
                .ok()
                .and_then(|origin| origin.parse::<axum::http::Uri>().ok())
                .filter(|origin| matches!(origin.scheme_str(), Some("http" | "https")))
                .and_then(|origin| origin.authority().cloned())
                .is_some_and(|origin| same_authority(&origin, &host));
            if !matches_host {
                return error_response(axum::http::StatusCode::FORBIDDEN, "invalid origin");
            }
        }

        let optional_json = method == axum::http::Method::POST
            && ["/dispatch", "/close", "/later"].iter().any(|suffix| {
                let path = request.uri().path();
                path.starts_with("/v1/cards/") && path.ends_with(suffix)
            });
        if optional_json {
            let is_json = request
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"));
            if !is_json {
                return error_response(axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE, "expected application/json");
            }
        }
    }

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    failure_body(method, path, next.run(request).await).await
}

/// Logs a failed write and returns its error as JSON. axum's extractor rejections are plain text,
/// which the inbox cannot show. Request bodies are not logged.
async fn failure_body(
    method: axum::http::Method,
    path: String,
    response: axum::response::Response,
) -> axum::response::Response {
    let status = response.status();
    if method == axum::http::Method::GET || method == axum::http::Method::HEAD || status.as_u16() < 400 {
        return response;
    }
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap_or_default();
    let json = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    if json {
        let text = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|body| body["error"].as_str().map(str::to_owned))
            .unwrap_or_default();
        eprintln!("triage: {method} {path} -> {} {text}", status.as_u16());
        return axum::response::Response::from_parts(parts, axum::body::Body::from(bytes));
    }
    let text = String::from_utf8_lossy(&bytes).trim().to_owned();
    eprintln!("triage: {method} {path} -> {} {text}", status.as_u16());
    error_response(status, &text)
}


fn router(state: Arc<AppState>, port: Option<u16>) -> axum::Router {
    let policy = HostPolicy { port, hosts: state.allowed_hosts.clone().into() };
    axum::Router::new()
        .route("/", axum::routing::get(shell_asset))
        .route("/style.css", axum::routing::get(shell_asset))
        .route("/app.js", axum::routing::get(shell_asset))
        .route("/marked.js", axum::routing::get(shell_asset))
        .route("/sw.js", axum::routing::get(shell_asset))
        .route("/manifest.webmanifest", axum::routing::get(shell_asset))
        .route("/apple-touch-icon.png", axum::routing::get(apple_touch_icon))
        .route("/fonts/Geist-Variable.woff2", axum::routing::get(font_sans))
        .route("/fonts/GeistMono-Variable.woff2", axum::routing::get(font_mono))
        .route("/v1/queue", axum::routing::get(queue))
        .route("/v1/stream", axum::routing::get(stream))
        .route("/v1/ingest", axum::routing::post(ingest))
        .route("/v1/cards/{id}/context", axum::routing::get(context))
        .route("/v1/cards/{id}/answer", axum::routing::post(answer))
        .route("/v1/cards/{id}/skip", axum::routing::post(skip))
        .route("/v1/cards/{id}/later", axum::routing::post(later))
        .route("/v1/cards/{id}/dispatch", axum::routing::post(dispatch))
        .route("/v1/cards/{id}/report", axum::routing::post(request_report))
        .route("/v1/cards/{id}/restore", axum::routing::post(restore))
        .route("/v1/cards/{id}/close", axum::routing::post(close))
        .route("/v1/cards/{id}/reload", axum::routing::post(reload))
        .route("/v1/cards/{id}/image/{index}", axum::routing::get(card_image))
        .route("/v1/sessions", axum::routing::post(start_session))
        .route("/v1/tmux-sessions", axum::routing::get(tmux_sessions))
        .route("/v1/models", axum::routing::get(models))
        .route("/v1/history", axum::routing::get(history))
        .route("/v1/dictation", axum::routing::get(dictation_settings).post(save_dictation))
        .route("/v1/panes/{pane}/peek", axum::routing::get(peek))
        .route("/v1/panes/{pane}/send", axum::routing::post(send))
        .route("/v1/panes/{pane}/jump", axum::routing::post(jump))
        .route("/v1/dsh/open", axum::routing::post(dsh_open))
        .route("/v1/dsh/{id}/peek", axum::routing::get(dsh_peek))
        .route("/v1/dsh/{id}/send", axum::routing::post(dsh_send))
        // A prompt image rides the request as base64; everything else stays far below this.
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .with_state(state)
        .layer(axum::middleware::from_fn(validate_host_origin))
        .layer(axum::Extension(policy))
        // Fonts are compressed already; DefaultPredicate also skips images, bodies under 32 bytes and SSE.
        .layer(CompressionLayer::new().compress_when(DefaultPredicate::new().and(NotForContentType::const_new("font/"))))
}
#[cfg(test)]
fn router_with_prewrite_gate(state: Arc<AppState>, gate: Arc<PreWriteGate>) -> axum::Router {
    let policy = HostPolicy { port: None, hosts: state.allowed_hosts.clone().into() };
    axum::Router::new()
        .route("/v1/cards/{id}/answer", axum::routing::post(answer))
        .route("/v1/cards/{id}/skip", axum::routing::post(skip_with_prewrite_gate))
        .route("/v1/cards/{id}/later", axum::routing::post(later_with_prewrite_gate))
        .layer(axum::Extension(gate))
        .with_state(state)
        .layer(axum::middleware::from_fn(validate_host_origin))
        .layer(axum::Extension(policy))
}

const INDEX_HTML: &str = include_str!("../ui/index.html");
const STYLE_CSS: &str = include_str!("../ui/style.css");
const APP_JS: &str = include_str!("../ui/app.js");
const MARKED_JS: &str = include_str!("../ui/marked.js");
const SW_JS: &str = include_str!("../ui/sw.js");
const MANIFEST: &str = include_str!("../ui/manifest.webmanifest");

/// One validator for the whole shell: any change to one file changes every tag, so a deploy
/// revalidates everything while an unchanged load gets 304s. `GET /v1/queue` names it as
/// `shell`, so an open inbox can tell that the daemon now serves another build.
static SHELL_ETAG: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    (INDEX_HTML, STYLE_CSS, APP_JS, MARKED_JS, SW_JS, MANIFEST).hash(&mut hasher);
    format!("W/\"{:016x}\"", hasher.finish())
});

fn shell_file(
    headers: &axum::http::HeaderMap,
    content_type: &'static str,
    body: &'static str,
) -> axum::response::Response {
    use axum::http::header;
    let etag = SHELL_ETAG.as_str();
    let fresh = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|tag| tag.trim() == "*" || tag.trim() == etag);
    let validators = [(header::ETAG, etag), (header::CACHE_CONTROL, "no-cache")];
    if fresh {
        (axum::http::StatusCode::NOT_MODIFIED, validators).into_response()
    } else {
        (validators, [(header::CONTENT_TYPE, content_type)], body).into_response()
    }
}

async fn shell_asset(uri: axum::http::Uri, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (content_type, body) = match uri.path() {
        "/" => ("text/html; charset=utf-8", INDEX_HTML),
        "/style.css" => ("text/css; charset=utf-8", STYLE_CSS),
        "/app.js" => ("text/javascript; charset=utf-8", APP_JS),
        "/marked.js" => ("text/javascript; charset=utf-8", MARKED_JS),
        "/sw.js" => ("text/javascript; charset=utf-8", SW_JS),
        "/manifest.webmanifest" => ("application/manifest+json", MANIFEST),
        _ => return error_response(axum::http::StatusCode::NOT_FOUND, "not found"),
    };
    shell_file(&headers, content_type, body)
}

async fn apple_touch_icon() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        &include_bytes!("../ui/icon-180.png")[..],
    )
}

// Geist is licensed under the SIL Open Font License (ui/fonts/OFL.txt). The files never change in place,
// so browsers may keep them for a year.
fn font(bytes: &'static [u8]) -> impl axum::response::IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "font/woff2"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
}

async fn font_sans() -> impl axum::response::IntoResponse {
    font(include_bytes!("../ui/fonts/Geist-Variable.woff2"))
}

async fn font_mono() -> impl axum::response::IntoResponse {
    font(include_bytes!("../ui/fonts/GeistMono-Variable.woff2"))
}

#[derive(serde::Deserialize)]
struct HistoryQuery {
    #[serde(default = "default_history_limit")]
    limit: usize,
    #[serde(default)]
    today_start_ms: Option<i64>,
    #[serde(default)]
    today_end_ms: Option<i64>,
}

const MAX_HISTORY_LIMIT: usize = 500;

fn default_history_limit() -> usize {
    50
}

#[derive(serde::Deserialize)]
struct ModelsQuery {
    #[serde(default)]
    engine: Option<String>,
}

#[derive(serde::Deserialize)]
struct DictationReq {
    dictionary: String,
    phrases: String,
}

async fn queue(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let now = crate::events::now_ms();
    let result = tokio::task::spawn_blocking(move || {
        Ok::<_, rusqlite::Error>((state.db.queue(now)?, state.db.version()?))
    })
    .await;
    match result {
        Ok(Ok((cards, version))) => {
            let mut counts = serde_json::Map::new();
            counts.insert("ask".into(), serde_json::json!(cards.iter().filter(|card| card.kind == crate::events::Kind::Ask).count()));
            counts.insert("finding".into(), serde_json::json!(cards.iter().filter(|card| card.kind == crate::events::Kind::Finding).count()));
            counts.insert("info".into(), serde_json::json!(cards.iter().filter(|card| card.kind == crate::events::Kind::Info).count()));
            counts.insert("plan".into(), serde_json::json!(cards.iter().filter(|card| card.kind == crate::events::Kind::Plan).count()));
            axum::Json(serde_json::json!({"cards": cards, "counts": counts, "version": version, "shell": SHELL_ETAG.as_str()})).into_response()
        }
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn stream(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    use tokio_stream::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let events = BroadcastStream::new(state.bus.subscribe()).filter_map(|message| {
        message.ok().map(|version| {
            Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default()
                .event("queue")
                .data(serde_json::json!({"version": version}).to_string()))
        })
    });
    axum::response::sse::Sse::new(events).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn ingest(
    State(state): State<Arc<AppState>>,
    axum::Json(event): axum::Json<crate::events::CloudEvent>,
) -> axum::response::Response {
    let card = card_from_event(&event);
    // Only malformed questions leave a finding without a card.
    if card.is_none() && event.r#type == crate::events::T_FINDING {
        return error_response(axum::http::StatusCode::BAD_REQUEST, "invalid finding questions");
    }
    let db = Arc::clone(&state);
    let stored = tokio::task::spawn_blocking(move || {
        let fold_lock = db.db.answer_lock(&format!("fold:{}", event.source));
        let _fold_guard = fold_lock.lock().unwrap();
        let fold_target = card.as_ref().filter(|card| card.kind == crate::events::Kind::Finding)
            .and_then(|card| db.db.queue(crate::events::now_ms()).ok()?.into_iter().find(|queued| {
                queued.kind == crate::events::Kind::Finding
                    && queued.id != card.id
                    && queued.source.uri == card.source.uri
                    && queued.title == card.title
            }));
        let stored = db.db.insert_event(&event, card.as_ref())?;
        if stored {
            if let Some(target) = &fold_target {
                let card_id = card.as_ref().expect("fold target requires card").id.clone();
                db.db.swipe(&crate::events::Swipe {
                    card: card_id,
                    action: "done".to_owned(),
                    option: None,
                    text: None,
                    note: None,
                    until_ms: None,
                    reason: Some(format!("duplicate of {}", target.id)),
                    at_ms: crate::events::now_ms(),
                }).map_err(crate::db::InsertEventError::Database)?;
            }
        }
        Ok::<_, crate::db::InsertEventError>(stored)
    }).await;
    match stored {
        Ok(Ok(stored)) => {
            if stored {
                broadcast(&state).await;
            }
            (axum::http::StatusCode::ACCEPTED, axum::Json(serde_json::json!({"ok": true, "stored": stored}))).into_response()
        }
        Ok(Err(crate::db::InsertEventError::EmptyId)) => {
            error_response(axum::http::StatusCode::BAD_REQUEST, "event ID must not be empty")
        }
        Ok(Err(crate::db::InsertEventError::Database(error))) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn answer(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(request): axum::Json<AnswerRequest>,
) -> axum::response::Response {
    let answer = request.answer;
    if answer.mode.as_deref().is_some_and(|mode| crate::cards::DispatchMode::parse(mode).is_none()) {
        return error_response(axum::http::StatusCode::BAD_REQUEST, "unknown mode");
    }
    let mut launch = state.launch.clone();
    if let Err(error) = apply_launch_choices(
        &mut launch,
        answer.engine.as_deref(),
        answer.model.as_deref(),
        answer.thinking.as_deref(),
    ) {
        return error_response(axum::http::StatusCode::BAD_REQUEST, &error);
    }
    if answer.mode.as_deref() == Some("plan-into") {
        if let Err(error) = crate::cards::plan_into_choices(
            launch.engine,
            answer.executor.as_deref(),
            answer.executor_thinking.as_deref(),
        ) {
            return error_response(axum::http::StatusCode::BAD_REQUEST, &error);
        }
    }
    let image = match store_image(state.db.path(), request.image).await {
        Ok(image) => image,
        Err(response) => return response,
    };
    card_action(state, id, move |db, card| {
        crate::cards::answer(db, &card, &answer, &launch, image.as_ref())
    })
    .await
}

/// A new engine drops the daemon's model and thinking level, which belong to the old engine.
fn apply_launch_choices(
    launch: &mut crate::cards::Launch,
    engine: Option<&str>,
    model: Option<&str>,
    thinking: Option<&str>,
) -> Result<(), String> {
    if let Some(engine) = engine.map(str::trim).filter(|engine| !engine.is_empty()) {
        let engine = crate::cards::Engine::parse(engine).ok_or_else(|| format!("unknown engine {engine}"))?;
        if engine != launch.engine {
            launch.engine = engine;
            launch.model = None;
            launch.thinking = None;
        }
    }
    if let Some(model) = crate::cards::engine_selector(launch.engine, model)? {
        launch.model = Some(model.to_owned());
    }
    if let Some(thinking) = thinking.map(str::trim).filter(|thinking| !thinking.is_empty()) {
        if !crate::cards::THINKING_LEVELS.contains(&thinking) {
            return Err(format!("unknown thinking level {thinking}"));
        }
        launch.thinking = Some(thinking.to_owned());
    }
    Ok(())
}

async fn card_action<F>(state: Arc<AppState>, id: String, action: F) -> axum::response::Response
where
    F: FnOnce(&Db, crate::events::Card) -> Result<(), String> + Send + 'static,
{
    let lookup_state = Arc::clone(&state);
    let found = tokio::task::spawn_blocking(move || lookup_state.db.card(&id)).await;
    let (card, swipe) = match found {
        Ok(Ok(Some((card, swipe)))) => (card, swipe),
        Ok(Ok(None)) => return error_response(axum::http::StatusCode::NOT_FOUND, "card not found"),
        Ok(Err(error)) => return internal_error(error),
        Err(error) => return internal_error(error),
    };
    if !crate::cards::queued_for_answer(swipe.as_ref()) {
        return error_response(axum::http::StatusCode::CONFLICT, "card is not queued");
    }
    let action_state = Arc::clone(&state);
    match tokio::task::spawn_blocking(move || action(&action_state.db, card)).await {
        Ok(Ok(())) => {
            broadcast(&state).await;
            axum::Json(serde_json::json!({"ok": true})).into_response()
        }
        Ok(Err(error)) => action_error_response(&error),
        Err(error) => internal_error(error),
    }
}

/// 409 means the card is already handled, and the inbox says so. Any other failure, such as a
/// tmux launch error or a closed dialog, is 422 with the reason, so the inbox shows it.
fn action_error_response(error: &str) -> axum::response::Response {
    let status = if error == "card is not queued" {
        axum::http::StatusCode::CONFLICT
    } else {
        axum::http::StatusCode::UNPROCESSABLE_ENTITY
    };
    error_response(status, error)
}

#[derive(Debug, Default, serde::Deserialize)]
struct CardNoteRequest {
    #[serde(default)]
    note: Option<String>,
}

/// An image a prompt carries, encoded as the inbox holds it. The daemon saves one copy under
/// `uploads/` and hands the engines that copy: omp as an `@` reference, dsh as the same bytes.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageRequest {
    media_type: String,
    data: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct AnswerRequest {
    #[serde(flatten)]
    answer: crate::events::AnswerReq,
    #[serde(default)]
    image: Option<ImageRequest>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct DispatchRequest {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    engine: Option<String>,
    /// Execution model for a plan-into dispatch, and the effort it takes.
    #[serde(default)]
    executor: Option<String>,
    #[serde(default)]
    executor_thinking: Option<String>,
    #[serde(default)]
    image: Option<ImageRequest>,
}

async fn dispatch(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    request: Option<axum::Json<DispatchRequest>>,
) -> axum::response::Response {
    let request = request.map(|request| request.0).unwrap_or_default();
    let mode = match request.mode.as_deref() {
        None => crate::cards::DispatchMode::Investigate,
        Some(mode) => match crate::cards::DispatchMode::parse(mode) {
            Some(mode) => mode,
            None => return error_response(axum::http::StatusCode::BAD_REQUEST, "unknown mode"),
        },
    };
    let plan_into = mode == crate::cards::DispatchMode::PlanInto;
    if !plan_into && request.executor.is_some() {
        return error_response(axum::http::StatusCode::BAD_REQUEST, "executor needs mode plan-into");
    }
    if !plan_into && request.executor_thinking.is_some() {
        return error_response(
            axum::http::StatusCode::BAD_REQUEST,
            "executor thinking needs mode plan-into",
        );
    }
    let mut launch = state.launch.clone();
    if let Err(error) = apply_launch_choices(
        &mut launch,
        request.engine.as_deref(),
        request.model.as_deref(),
        request.thinking.as_deref(),
    ) {
        return error_response(axum::http::StatusCode::BAD_REQUEST, &error);
    }
    if plan_into {
        if let Err(error) = crate::cards::plan_into_choices(
            launch.engine,
            request.executor.as_deref(),
            request.executor_thinking.as_deref(),
        ) {
            return error_response(axum::http::StatusCode::BAD_REQUEST, &error);
        }
    }
    let note = request.note.filter(|note| !note.trim().is_empty());
    let executor = request.executor;
    let executor_thinking = request.executor_thinking;
    let image = match store_image(state.db.path(), request.image).await {
        Ok(image) => image,
        Err(response) => return response,
    };
    card_action(state, id, move |db, card| {
        crate::cards::dispatch(
            db,
            &card,
            mode,
            note.as_deref(),
            &launch,
            executor.as_deref(),
            executor_thinking.as_deref(),
            image.as_ref(),
        )
    })
    .await
}

/// Save a request's image under the daemon's `uploads/`, or the 400 to answer with.
async fn store_image(
    db_file: &std::path::Path,
    image: Option<ImageRequest>,
) -> Result<Option<crate::uploads::Upload>, axum::response::Response> {
    let Some(image) = image else {
        return Ok(None);
    };
    let db_file = db_file.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        crate::uploads::store(&db_file, &image.media_type, &image.data, image.name.as_deref())
    })
    .await
    {
        Ok(Ok(upload)) => Ok(Some(upload)),
        Ok(Err(error)) => Err(error_response(axum::http::StatusCode::BAD_REQUEST, &error)),
        Err(error) => Err(internal_error(error)),
    }
}

async fn close(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    request: Option<axum::Json<CardNoteRequest>>,
) -> axum::response::Response {
    let note = request.and_then(|request| request.0.note).filter(|note| !note.trim().is_empty());
    let launch = state.launch.clone();
    card_action(state, id, move |db, card| crate::cards::close(db, &card, note.as_deref(), &launch)).await
}
async fn request_report(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    let launch = state.launch.clone();
    card_action(state, id, move |db, card| crate::cards::request_report(db, &card, &launch)).await
}

async fn restore(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    let launch = state.launch.clone();
    card_action(state, id, move |db, card| crate::cards::restore(db, &card, &launch)).await
}

/// Puts a fresh copy of a handled card back in the queue. The original and its swipes stay as they are.
async fn reload(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    let work_state = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        let lock = work_state.db.answer_lock(&id);
        let _guard = lock.lock().unwrap();
        let Some((original, swipe)) = work_state.db.card(&id)? else {
            return Ok::<_, rusqlite::Error>(Err((axum::http::StatusCode::NOT_FOUND, "card not found")));
        };
        if swipe.as_ref().is_none_or(|swipe| swipe.action == "later") {
            return Ok(Err((axum::http::StatusCode::CONFLICT, "card is already queued")));
        }
        let copy_prefix = format!("reload:{}:", original.id);
        let now_ms = crate::events::now_ms();
        let copy_queued = work_state
            .db
            .queue(now_ms)?
            .iter()
            .any(|card| card.id == original.id || card.id.starts_with(&copy_prefix));
        if copy_queued {
            return Ok(Err((axum::http::StatusCode::CONFLICT, "card is already queued")));
        }
        let mut copy = original;
        copy.id = format!("{copy_prefix}{}", uuid::Uuid::new_v4());
        copy.created_ms = now_ms;
        work_state.db.insert_card(&copy)?;
        Ok(Ok(copy.id))
    })
    .await;
    match result {
        Ok(Ok(Ok(card))) => {
            broadcast(&state).await;
            axum::Json(serde_json::json!({"ok": true, "card": card})).into_response()
        }
        Ok(Ok(Err((status, message)))) => error_response(status, message),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    prompt: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    executor: Option<String>,
    /// Effort the Plan into execution model takes, which only a dsh plan can name.
    #[serde(default)]
    executor_thinking: Option<String>,
    #[serde(default)]
    image: Option<ImageRequest>,
}

/// A model entry for the shared pickers, named by its omp selector.
#[derive(Clone, serde::Serialize)]
struct Model {
    selector: String,
    name: String,
}

#[derive(Clone, Default)]
struct ModelCatalog {
    models: Vec<Model>,
    execution_models: Vec<Model>,
}

/// One nonblocking snapshot holds omp's general and Plan Review model choices.
static MODELS: std::sync::RwLock<ModelCatalog> = std::sync::RwLock::new(ModelCatalog {
    models: Vec::new(),
    execution_models: Vec::new(),
});

/// The files in omp's agent directory that hold its roles, enabledModels whitelist and custom models.
const OMP_CONFIG_FILES: [&str; 2] = ["config.yml", "models.yml"];

/// How often the catalog thread checks omp's config files for a change.
const MODELS_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// The modification times of [`OMP_CONFIG_FILES`] in order; a missing file reads as `None`.
type ConfigStamp = Vec<Option<std::time::SystemTime>>;

/// Starts the catalog thread when first forced; forcing only spawns, it never waits. The thread
/// loads at once, then retries after a failed catalog query.
static MODELS_LOAD: std::sync::LazyLock<()> = std::sync::LazyLock::new(|| {
    let _ = std::thread::Builder::new()
        .name("triage-models".to_owned())
        .spawn(|| {
            let mut loaded = None;
            loop {
                let dir = omp_agent_dir();
                refresh_models(dir.as_deref(), &mut loaded);
                std::thread::sleep(MODELS_CHECK);
            }
        });
});

/// omp's agent directory as `omp config path` prints it, which follows profiles and `PI_CODING_AGENT_DIR`.
fn omp_agent_dir() -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("omp").args(["config", "path"]).output().ok()?;
    let dir = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !dir.is_empty()).then(|| dir.into())
}


fn config_stamp(dir: &std::path::Path) -> ConfigStamp {
    OMP_CONFIG_FILES
        .iter()
        .map(|file| std::fs::metadata(dir.join(file)).and_then(|meta| meta.modified()).ok())
        .collect()
}

/// Loads the catalog when no successful snapshot exists or omp's config changes. Failed queries leave the stamp unset, so the next 30-second check retries. Successful empty lists are cached.
fn refresh_models(dir: Option<&std::path::Path>, loaded: &mut Option<ConfigStamp>) -> bool {
    let stamp = dir.map(config_stamp).unwrap_or_default();
    if loaded.as_ref() == Some(&stamp) {
        return false;
    }
    let (models, models_ok) = load_models();
    let (execution_models, execution_models_ok) = load_execution_models(&models);
    if let Ok(mut slot) = MODELS.write() {
        *slot = ModelCatalog { models, execution_models };
    }
    if models_ok && execution_models_ok {
        *loaded = Some(stamp);
    }
    true
}

/// Parses `omp config get modelRoles --json` output into `@role` picker entries; a missing or
/// null `value`, or invalid JSON, yields none. The entry name carries the role's model so the
/// picker can tell roles apart: `smol role (bedrock-mantle/openai.gpt-6-luna:medium)`.
fn roles_from_config(stdout: &str) -> Vec<Model> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).unwrap_or_default();
    let mut roles: Vec<(String, String)> = parsed["value"]
        .as_object()
        .map(|roles| {
            roles
                .iter()
                .map(|(role, model)| {
                    let model = model.as_str().unwrap_or_default();
                    (role.clone(), format!("{role} role ({model})"))
                })
                .collect()
        })
        .unwrap_or_default();
    roles.sort();
    roles
        .into_iter()
        .map(|(role, name)| Model { selector: format!("@{role}"), name })
        .collect()
}

/// The configured omp model roles; a failed query contributes nothing and the picker keeps the models.
fn load_roles() -> Vec<Model> {
    let output = std::process::Command::new("omp").args(["config", "get", "modelRoles", "--json"]).output();
    let Some(output) = output.ok().filter(|o| o.status.success()) else { return Vec::new() };
    roles_from_config(&String::from_utf8_lossy(&output.stdout))
}

/// Parses string-array values printed by `omp config get`.
/// Parses string-array values printed by `omp config get`; invalid JSON is `None`.
fn string_list_from_config(stdout: &str) -> Option<Vec<String>> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).ok()?;
    Some(
        parsed["value"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect(),
    )
}

/// The configured enabledModels whitelist; a failed query yields an empty list, which filters nothing.
fn load_enabled() -> Vec<String> {
    let output = std::process::Command::new("omp").args(["config", "get", "enabledModels", "--json"]).output();
    let Some(output) = output.ok().filter(|o| o.status.success()) else { return Vec::new() };
    string_list_from_config(&String::from_utf8_lossy(&output.stdout)).unwrap_or_default()
}

/// Builds role selectors in Plan Review's configured order.
fn execution_models(models: &[Model], cycle: &[String]) -> Vec<Model> {
    cycle
        .iter()
        .map(|role| {
            let selector = format!("@{role}");
            models
                .iter()
                .find(|model| model.selector == selector)
                .cloned()
                .unwrap_or_else(|| Model { selector, name: format!("{role} role") })
        })
        .collect()
}

/// Loads model roles in the configured Plan Review order.
fn load_execution_models(models: &[Model]) -> (Vec<Model>, bool) {
    let output = std::process::Command::new("omp").args(["config", "get", "cycleOrder", "--json"]).output();
    let Some(output) = output.ok().filter(|o| o.status.success()) else { return (Vec::new(), false) };
    let Some(cycle) = string_list_from_config(&String::from_utf8_lossy(&output.stdout)) else {
        return (Vec::new(), false);
    };
    (execution_models(models, &cycle), true)
}

/// Parses `omp models --json` output. The catalog carries every bundled model, valid or not —
/// provider `amazon-bedrock` lists the bare `openai.gpt-6-luna`, which Bedrock rejects with
/// HTTP 400 because the model runs only through an inference profile (`global.`/`us.` prefix) —
/// so a configured enabledModels whitelist keeps the picker to models that actually run. An empty
/// whitelist (omp's default, everything allowed) leaves the catalog untouched.
fn parse_catalog(stdout: &str, enabled: &[String]) -> Option<Vec<Model>> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).ok()?;
    Some(
        parsed["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let selector = m["selector"].as_str()?;
                if !enabled.is_empty() && !enabled.iter().any(|allowed| allowed == selector) {
                    return None;
                }
                let name = m["name"].as_str().filter(|name| !name.trim().is_empty()).unwrap_or(selector);
                Some(Model { selector: selector.to_owned(), name: name.to_owned() })
            })
            .collect(),
    )
}

fn load_models() -> (Vec<Model>, bool) {
    let roles = load_roles();
    let enabled = load_enabled();
    let output = std::process::Command::new("omp").args(["models", "--json"]).output();
    let Some(output) = output.ok().filter(|o| o.status.success()) else { return (roles, false) };
    let Some(catalog) = parse_catalog(&String::from_utf8_lossy(&output.stdout), &enabled) else {
        return (roles, false);
    };
    (roles.into_iter().chain(catalog).collect(), true)
}

async fn models(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<ModelsQuery>,
) -> axum::response::Response {
    let engine = match query.engine.as_deref().map(str::trim).filter(|engine| !engine.is_empty()) {
        Some(name) => match crate::cards::Engine::parse(name) {
            Some(engine) => engine,
            None => return error_response(axum::http::StatusCode::BAD_REQUEST, &format!("unknown engine {name}")),
        },
        None => state.launch.engine,
    };
    let daemon = state.launch.engine == engine;
    if engine == crate::cards::Engine::Dsh {
        // dsh's own catalog decides the list: its settings add and remove model
        // routes, and a pair triage remembered would be rejected at launch.
        let host = state.launch.dsh_host.clone();
        let catalog = tokio::task::spawn_blocking(move || crate::dsh::catalog(host.as_deref()))
            .await
            .ok()
            .flatten();
        // Each entry carries the efforts that model accepts, so a picker never
        // offers a level dsh would refuse.
        let models: Vec<serde_json::Value> = catalog
            .iter()
            .flat_map(|catalog| catalog.models.iter())
            .map(|model| {
                serde_json::json!({
                    "selector": model.selector,
                    "name": model.name.clone().unwrap_or_else(|| model.selector.clone()),
                    "efforts": model.efforts,
                })
            })
            .collect();
        // The daemon's own dsh model wins when the host still offers it.
        let daemon_model = state.launch.model.as_deref().filter(|model| {
            catalog.as_ref().is_some_and(|catalog| catalog.offers(model))
        });
        let default = match (daemon, daemon_model) {
            (true, Some(model)) => Some(model.to_owned()),
            _ => catalog.as_ref().and_then(|catalog| catalog.default_selector().map(str::to_owned)),
        };
        let default_thinking = if daemon { state.launch.thinking.clone() } else { None };
        return axum::Json(serde_json::json!({
            "engine": engine.name(),
            "models": models,
            "execution_models": models,
            "default": default,
            "thinking": crate::dsh::THINKING,
            "default_thinking": default_thinking,
        }))
        .into_response();
    }
    // Never waits for the load. Before the first snapshot, both lists are empty.
    let catalog = MODELS.try_read().map(|slot| slot.clone()).unwrap_or_default();
    let mut body = serde_json::json!({
        "engine": engine.name(),
        "models": catalog.models,
        "execution_models": catalog.execution_models,
        "thinking": crate::cards::THINKING_LEVELS,
    });
    if daemon {
        body["default"] = serde_json::json!(state.launch.model);
        body["default_thinking"] = serde_json::json!(state.launch.thinking);
    }
    axum::Json(body).into_response()
}

async fn tmux_sessions(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let listing = tokio::task::spawn_blocking(|| {
        crate::tmux::list_sessions().map(|sessions| {
            // Directories only suggest, so a failed pane listing leaves them empty.
            let directories = crate::tmux::list_panes()
                .map(|panes| pane_directories(panes.iter().map(|pane| pane.cwd.as_str())))
                .unwrap_or_default();
            (sessions, directories)
        })
    });
    match listing.await {
        Ok(Ok((sessions, directories))) => axum::Json(serde_json::json!({
            "sessions": sessions,
            "default": state.launch.session.as_deref().unwrap_or(crate::cards::DEFAULT_SESSION),
            "directories": directories,
        }))
        .into_response(),
        Ok(Err(error)) => error_response(axum::http::StatusCode::BAD_GATEWAY, &error.to_string()),
        Err(error) => internal_error(error),
    }
}

/// Distinct pane working directories, those most panes share first and ties in path order,
/// so the New session sheet can offer the places sessions already run.
fn pane_directories<'a>(cwds: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for cwd in cwds.into_iter().filter(|cwd| !cwd.is_empty()) {
        *counts.entry(cwd).or_default() += 1;
    }
    let mut directories: Vec<_> = counts.into_iter().collect();
    directories.sort_by(|a, b| b.1.cmp(&a.1));
    directories.into_iter().map(|(cwd, _)| cwd.to_owned()).collect()
}

async fn start_session(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<SessionRequest>,
) -> axum::response::Response {
    let cwd = request
        .cwd
        .filter(|cwd| !cwd.trim().is_empty())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/".to_owned());
    let mut launch = state.launch.clone();
    if let Some(session) = request.session.filter(|session| !session.trim().is_empty()) {
        launch.session = Some(session);
    }
    if let Err(error) = apply_launch_choices(
        &mut launch,
        request.engine.as_deref(),
        request.model.as_deref(),
        request.thinking.as_deref(),
    ) {
        return error_response(axum::http::StatusCode::BAD_REQUEST, &error);
    }
    // Reject a bad mode now; the worker parses it again, because `Start` borrows the request.
    if let Err(error) = crate::cards::Start::parse(
        request.mode.as_deref(),
        request.executor.as_deref(),
        request.executor_thinking.as_deref(),
    ) {
        return error_response(axum::http::StatusCode::BAD_REQUEST, &error);
    }
    // The launch outlives the answer: a slow dsh host takes tens of seconds, and the inbox
    // must not wait on it. A worker sends its outcome once; the handler waits LAUNCH_GRACE
    // for it, and a watcher turns a later failure into a card.
    let (tx, mut rx) = tokio::sync::watch::channel::<Option<Result<String, String>>>(None);
    let action_state = Arc::clone(&state);
    let prompt = request.prompt.clone();
    tokio::task::spawn_blocking(move || {
        let outcome = (|| {
            let start = crate::cards::Start::parse(
                request.mode.as_deref(),
                request.executor.as_deref(),
                request.executor_thinking.as_deref(),
            )?;
            let image = match request.image.as_ref() {
                Some(image) => Some(crate::uploads::store(
                    &action_state.db.path(),
                    &image.media_type,
                    &image.data,
                    image.name.as_deref(),
                )?),
                None => None,
            };
            crate::cards::start_session(
                &action_state.db,
                &request.prompt,
                &cwd,
                &launch,
                start,
                image.as_ref(),
            )
        })();
        let _ = tx.send(Some(outcome));
    });
    if tokio::time::timeout(LAUNCH_GRACE, rx.changed()).await.is_ok() {
        return match rx.borrow_and_update().clone() {
            Some(Ok(pane)) => axum::Json(serde_json::json!({"ok": true, "pane": pane})).into_response(),
            Some(Err(error)) => error_response(axum::http::StatusCode::BAD_REQUEST, &error),
            None => internal_error("launch answered nothing"),
        };
    }
    let watcher = Arc::clone(&state);
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let outcome = { rx.borrow_and_update().clone() };
            if let Some(Err(error)) = outcome {
                report_failed_launch(&watcher, &prompt, &error).await;
                break;
            }
        }
    });
    (axum::http::StatusCode::ACCEPTED, axum::Json(serde_json::json!({"ok": true, "starting": true}))).into_response()
}

/// How long Start waits for the engine before it answers "still starting". Shorter than a slow
/// launch, longer than a healthy one, which answers in well under a second.
const LAUNCH_GRACE: std::time::Duration = std::time::Duration::from_millis(2000);

/// Tell the inbox a launch it had already accepted failed, so a background failure is never
/// silent. The card carries the engine's error and the prompt that was not started.
async fn report_failed_launch(state: &Arc<AppState>, prompt: &str, error: &str) {
    eprintln!("triage: launch failed: {error}");
    let event = crate::events::CloudEvent {
        specversion: "1.0".to_owned(),
        id: format!("launch-failed:{}", uuid::Uuid::new_v4()),
        source: "triage://launch".to_owned(),
        r#type: crate::events::T_INFO.to_owned(),
        subject: Some("Could not start the session".to_owned()),
        time: None,
        priority: Some(2),
        data: serde_json::json!({
            "title": "Could not start the session",
            "body": format!("{error}\n\n{prompt}"),
            "level": "warn",
        }),
    };
    let card = card_from_event(&event);
    let stored_card = card.clone();
    let db = Arc::clone(state);
    let stored = tokio::task::spawn_blocking(move || db.db.insert_event(&event, stored_card.as_ref())).await;
    if !matches!(stored, Ok(Ok(true))) {
        return;
    }
    broadcast(state).await;
}

async fn skip(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    #[cfg(test)]
    let result = write_swipe(&state, id, "skipped", None, None).await;
    #[cfg(not(test))]
    let result = write_swipe(&state, id, "skipped", None).await;
    swipe_response(result)
}

#[cfg(test)]
async fn skip_with_prewrite_gate(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Extension(gate): axum::Extension<Arc<PreWriteGate>>,
) -> axum::response::Response {
    let result = write_swipe(&state, id, "skipped", None, Some(gate)).await;
    swipe_response(result)
}
#[derive(serde::Deserialize)]
struct LaterRequest {
    #[serde(default)]
    minutes: Option<i64>,
}

fn later_until(request: Option<axum::Json<LaterRequest>>) -> Result<i64, axum::response::Response> {
    let minutes = request.and_then(|request| request.0.minutes).unwrap_or(60);
    if minutes < 0 {
        return Err(error_response(axum::http::StatusCode::BAD_REQUEST, "invalid minutes"));
    }
    minutes
        .checked_mul(60_000)
        .and_then(|delay| crate::events::now_ms().checked_add(delay))
        .ok_or_else(|| error_response(axum::http::StatusCode::BAD_REQUEST, "invalid minutes"))
}

async fn later(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    request: Option<axum::Json<LaterRequest>>,
) -> axum::response::Response {
    let until_ms = match later_until(request) {
        Ok(until_ms) => until_ms,
        Err(error) => return error,
    };
    #[cfg(test)]
    let result = write_swipe(&state, id, "later", Some(until_ms), None).await;
    #[cfg(not(test))]
    let result = write_swipe(&state, id, "later", Some(until_ms)).await;
    swipe_response(result)
}

#[cfg(test)]
async fn later_with_prewrite_gate(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Extension(gate): axum::Extension<Arc<PreWriteGate>>,
    request: Option<axum::Json<LaterRequest>>,
) -> axum::response::Response {
    let until_ms = match later_until(request) {
        Ok(until_ms) => until_ms,
        Err(error) => return error,
    };
    let result = write_swipe(&state, id, "later", Some(until_ms), Some(gate)).await;
    swipe_response(result)
}

fn swipe_response(result: Result<(), axum::response::Response>) -> axum::response::Response {
    result.map_or_else(|error| error, |_| axum::Json(serde_json::json!({"ok": true})).into_response())
}

async fn write_swipe(
    state: &Arc<AppState>,
    id: String,
    action: &str,
    until_ms: Option<i64>,
    #[cfg(test)] prewrite_gate: Option<Arc<PreWriteGate>>,
) -> Result<(), axum::response::Response> {
    let write_state = Arc::clone(state);
    let action = action.to_owned();
    let result = tokio::task::spawn_blocking(move || {
        let lock = write_state.db.answer_lock(&id);
        let _guard = lock.lock().unwrap();
        let Some((_, swipe)) = write_state.db.card(&id)? else {
            return Ok::<_, rusqlite::Error>(Err((axum::http::StatusCode::NOT_FOUND, "card not found")));
        };
        let queued = crate::cards::queued_for_answer(swipe.as_ref());
        if !queued {
            return Ok(Err((axum::http::StatusCode::CONFLICT, "card is not queued")));
        }
        #[cfg(test)]
        if let Some(gate) = &prewrite_gate {
            gate.pause(&id);
        }
        let swipe = crate::events::Swipe {
            card: id,
            action,
            option: None,
            text: None,
            note: None,
            until_ms,
            reason: None,
            at_ms: crate::events::now_ms(),
        };
        write_state.db.swipe(&swipe)?;
        Ok(Ok(()))
    })
    .await;
    match result {
        Ok(Ok(Ok(()))) => {
            broadcast(state).await;
            Ok(())
        }
        Ok(Ok(Err((status, message)))) => Err(error_response(status, message)),
        Ok(Err(error)) => Err(internal_error(error)),
        Err(error) => Err(internal_error(error)),
    }
}

/// Serve one image a card's turn read, from the store that keeps it. The card names the bytes;
/// the request only picks the card and the index, so no path ever comes from the client.
async fn card_image(
    State(state): State<Arc<AppState>>,
    axum::extract::Path((id, index)): axum::extract::Path<(String, usize)>,
) -> axum::response::Response {
    use axum::http::header;
    let found = tokio::task::spawn_blocking(move || {
        state
            .db
            .card(&id)
            .ok()
            .flatten()
            .and_then(|(card, _)| crate::media::image(&state.media, &card, index))
    })
    .await;
    let Ok(Some((media_type, bytes))) = found else {
        return error_response(axum::http::StatusCode::NOT_FOUND, "image not found");
    };
    (
        [
            (header::CONTENT_TYPE, media_type),
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable".to_owned(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        bytes,
    )
        .into_response()
}

async fn history(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> axum::response::Response {
    let result = tokio::task::spawn_blocking(move || {
        state.db.history_with_count_between(
            query.limit.min(MAX_HISTORY_LIMIT),
            query.today_start_ms.zip(query.today_end_ms),
        )
    })
    .await;
    match result {
        Ok(Ok((cards, today_count))) => {
            let cards: Vec<_> = cards
                .into_iter()
                // The history list shows a title, a source and an option label; bodies stay here.
                .map(|(card, swipe)| {
                    let options: Vec<_> =
                        card.options.iter().map(|option| serde_json::json!({"label": option.label})).collect();
                    serde_json::json!({
                        "card": {"id": card.id, "kind": card.kind, "title": card.title, "source": card.source, "options": options},
                        "swipe": swipe,
                    })
                })
                .collect();
            let mut response = serde_json::Map::new();
            response.insert("cards".into(), serde_json::json!(cards));
            if let Some(today_count) = today_count {
                response.insert("today_count".into(), serde_json::json!(today_count));
            }
            axum::Json(response).into_response()
        }
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

/// A card's context thread. Unlike history rows, items carry the full card, body
/// included: the thread exists to show the whole story.
async fn context(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    let result = tokio::task::spawn_blocking({
        let id = id.clone();
        move || state.db.thread(&id)
    })
    .await;
    match result {
        Ok(Ok(Some(thread))) => {
            let items: Vec<_> = thread
                .items
                .into_iter()
                .map(|item| serde_json::json!({"card": item.card, "swipes": item.swipes}))
                .collect();
            axum::Json(serde_json::json!({
                "anchor": id,
                "items": items,
                "truncated": thread.truncated,
            }))
            .into_response()
        }
        Ok(Ok(None)) => error_response(axum::http::StatusCode::NOT_FOUND, "card not found"),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn dictation_settings(State(state): State<Arc<AppState>>) -> axum::response::Response {
    match tokio::task::spawn_blocking(move || state.db.dictation()).await {
        Ok(Ok((dictionary, phrases))) => {
            axum::Json(serde_json::json!({"dictionary": dictionary, "phrases": phrases})).into_response()
        }
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn save_dictation(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<DictationReq>,
) -> axum::response::Response {
    match tokio::task::spawn_blocking(move || state.db.set_dictation(&request.dictionary, &request.phrases)).await {
        Ok(Ok(())) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

fn pane_error_response(
    error: &str,
    fallback: axum::http::StatusCode,
) -> axum::response::Response {
    let status = if error == "pane not found" || error == "session not found" {
        axum::http::StatusCode::NOT_FOUND
    } else {
        fallback
    };
    error_response(status, error)
}

async fn peek(
    axum::extract::Path(pane): axum::extract::Path<String>,
) -> axum::response::Response {
    match tokio::task::spawn_blocking(move || crate::cards::peek(&pane)).await {
        Ok(Ok(peek)) => axum::Json(serde_json::json!({
            "text": peek.text,
            "state": pane_state_name(peek.state),
            "width": peek.width,
            "height": peek.height,
        }))
        .into_response(),
        Ok(Err(error)) => pane_error_response(&error, axum::http::StatusCode::BAD_REQUEST),
        Err(error) => internal_error(error),
    }
}

#[derive(serde::Deserialize)]
struct SendRequest {
    text: String,
}

async fn send(
    axum::extract::Path(pane): axum::extract::Path<String>,
    axum::Json(request): axum::Json<SendRequest>,
) -> axum::response::Response {
    match tokio::task::spawn_blocking(move || crate::cards::send(&pane, &request.text)).await {
        Ok(Ok(())) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Ok(Err(error)) => pane_error_response(&error, axum::http::StatusCode::CONFLICT),
        Err(error) => internal_error(error),
    }
}

async fn dsh_open(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let host = state.launch.dsh_host.clone();
    match tokio::task::spawn_blocking(move || {
        crate::dsh::connect(host.as_deref(), crate::dsh::ACTION_TIMEOUT)?.open()
    })
    .await
    {
        Ok(Ok(())) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Ok(Err(error)) => pane_error_response(&error, axum::http::StatusCode::CONFLICT),
        Err(error) => internal_error(error),
    }
}

/// Peek view of a dsh session that shows its state and then its recent events as text.
async fn dsh_peek(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    let host = state.launch.dsh_host.clone();
    match tokio::task::spawn_blocking(move || -> Result<(crate::dsh::State, Vec<crate::dsh::Event>), String> {
        let client = crate::dsh::connect(host.as_deref(), crate::dsh::ACTION_TIMEOUT)?;
        let session = client.state(&id)?;
        Ok((session, client.events(&id, 80)?))
    })
    .await
    {
        Ok(Ok((session, events))) => {
            let text = crate::omp::redact(&crate::dsh::peek_text(&events));
            let label = if !session.pending_input.is_empty() {
                "asking"
            } else if session.running {
                "working"
            } else {
                "waiting"
            };
            let width = text.lines().map(|line| line.chars().count()).max().unwrap_or(0).clamp(1, 200);
            let height = text.lines().count().max(1);
            axum::Json(serde_json::json!({"text": text, "state": label, "width": width, "height": height}))
                .into_response()
        }
        Ok(Err(error)) => pane_error_response(&error, axum::http::StatusCode::CONFLICT),
        Err(error) => internal_error(error),
    }
}

async fn dsh_send(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(request): axum::Json<SendRequest>,
) -> axum::response::Response {
    let host = state.launch.dsh_host.clone();
    match tokio::task::spawn_blocking(move || {
        if crate::cards::blocked_command_prefix(&request.text) {
            return Err("text starts with a blocked command prefix".to_owned());
        }
        let client = crate::dsh::connect(host.as_deref(), crate::dsh::ACTION_TIMEOUT)?;
        let session = client.state(&id)?;
        if session.running || !session.pending_input.is_empty() {
            return Err("session is not waiting".to_owned());
        }
        client.prompt(&id, &request.text, None)
    })
    .await
    {
        Ok(Ok(())) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Ok(Err(error)) => pane_error_response(&error, axum::http::StatusCode::CONFLICT),
        Err(error) => internal_error(error),
    }
}

async fn jump(
    axum::extract::Path(pane): axum::extract::Path<String>,
) -> axum::response::Response {
    let lookup_pane = pane.clone();
    match tokio::task::spawn_blocking(move || {
        crate::tmux::list_panes()
            .map(|panes| panes.iter().any(|candidate| candidate.id == lookup_pane))
    })
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            return error_response(axum::http::StatusCode::NOT_FOUND, "pane not found");
        }
        Ok(Err(error)) => {
            return internal_error(error);
        }
        Err(error) => return internal_error(error),
    }
    match tokio::task::spawn_blocking(move || crate::tmux::jump(&pane)).await {
        Ok(Ok(())) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Ok(Err(error)) => {
            let message = error.to_string();
            if message.starts_with("can't find pane:") {
                error_response(axum::http::StatusCode::NOT_FOUND, "pane not found")
            } else {
                error_response(axum::http::StatusCode::BAD_REQUEST, &message)
            }
        }
        Err(error) => internal_error(error),
    }
}

fn pane_state_name(state: crate::omp::PaneState) -> &'static str {
    match state {
        crate::omp::PaneState::Asking => "asking",
        crate::omp::PaneState::Waiting => "waiting",
        crate::omp::PaneState::Working => "working",
        crate::omp::PaneState::Other => "other",
    }
}

async fn broadcast(state: &Arc<AppState>) {
    let version_state = Arc::clone(state);
    if let Ok(Ok(version)) = tokio::task::spawn_blocking(move || version_state.db.version()).await {
        let _ = state.bus.send(version);
    }
}

fn internal_error(error: impl std::fmt::Display) -> axum::response::Response {
    error_response(axum::http::StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
}

fn error_response(status: axum::http::StatusCode, error: &str) -> axum::response::Response {
    (status, axum::Json(serde_json::json!({"error": error}))).into_response()
}

fn string_field(data: &serde_json::Value, key: &str) -> Option<String> {
    data.get(key).and_then(serde_json::Value::as_str).map(str::to_owned)
}

fn source_for_event(ev: &crate::events::CloudEvent) -> crate::events::Source {
    let label = string_field(&ev.data, "label").unwrap_or_else(|| {
        ev.source
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&ev.source)
            .to_owned()
    });
    crate::events::Source {
        uri: ev.source.clone(),
        label,
        cwd: string_field(&ev.data, "cwd").filter(|cwd| !cwd.is_empty()).unwrap_or_else(|| std::env::var("HOME").unwrap_or_default()),
        pane: None,
        target: None,
    }
}

fn card_from_event(ev: &crate::events::CloudEvent) -> Option<crate::events::Card> {
    use crate::events::{Card, CardOption, Kind, Star};

    let source = source_for_event(ev);
    let data = &ev.data;
    // A finding's malformed questions reject the event. Other events and absent fields have none.
    let questions = match data.get("questions") {
        Some(value) if ev.r#type == crate::events::T_FINDING => {
            serde_json::from_value::<Vec<crate::events::CardQuestion>>(value.clone()).ok()?
        }
        _ => Vec::new(),
    };
    let (kind, priority, title, star, options, answerable, allow_other, body, refs, level) =
        if ev.r#type == crate::events::T_FINDING {
            let mut options = data
                .get("options")
                .and_then(|value| serde_json::from_value::<Vec<CardOption>>(value.clone()).ok())
                .filter(|options| !options.is_empty())
                .unwrap_or_else(|| vec![CardOption {
                    label: "Do as recommended".into(),
                    recommended: true,
                    ..CardOption::default()
                }]);
            if !options.iter().any(|option| option.recommended) {
                if let Some(first) = options.first_mut() {
                    first.recommended = true;
                }
            }
            (
                Kind::Finding,
                ev.priority.unwrap_or(2),
                string_field(data, "title").unwrap_or_else(|| ev.subject.clone().unwrap_or_default()),
                data.get("star")
                    .and_then(|value| serde_json::from_value::<Star>(value.clone()).ok())
                    .unwrap_or_default(),
                options,
                true,
                true,
                None,
                data.get("refs")
                    .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
                    .unwrap_or_default(),
                None,
            )
        } else if ev.r#type == crate::events::T_INFO {
            let body = data.get("body").map(|value| {
                value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())
            });
            let (body, action) = match body {
                Some(body) => match crate::omp::recommended_action(&body) {
                    Some((rest, action)) => (Some(rest), action),
                    None => (Some(body), String::new()),
                },
                None => (None, String::new()),
            };
            (
                Kind::Info,
                ev.priority.unwrap_or(3),
                string_field(data, "title").unwrap_or_else(|| source.label.clone()),
                Star { action, ..Star::default() },
                vec![CardOption { label: "Acknowledge".into(), recommended: true, ..CardOption::default() }],
                true,
                false,
                body,
                Vec::new(),
                string_field(data, "level"),
            )
        } else {
            return None;
        };

    Some(Card {
        id: crate::db::cloud_event_key(&ev.source, &ev.id),
        kind,
        priority,
        created_ms: crate::events::now_ms(),
        source,
        title,
        star,
        options,
        answerable,
        allow_other,
        questions,
        body,
        refs,
        level,
        tool_call_id: None,
        report_due: false,
        images: Vec::new(),
        engine: None,
    })
}

/// Bind and serve forever. Also spawns the pane tracker loop.
pub async fn serve(
    state: Arc<AppState>,
    addr: std::net::SocketAddr,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    std::sync::LazyLock::force(&MODELS_LOAD);
    let poll_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        // A slow machine must coalesce missed polls, not burst catch-up tmux forks once it catches up.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_poll_ms = crate::events::now_ms();
        loop {
            interval.tick().await;
            let now_ms = crate::events::now_ms();
            let state = Arc::clone(&poll_state);
            let result = tokio::task::spawn_blocking(move || {
                let before = state.db.queue(last_poll_ms)?;
                let mut tracker = state.tracker.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let written = tracker.tick(&state.db).written
                    + tracker.dsh_tick(&state.db, state.launch.dsh_host.as_deref()).written;
                let after = state.db.queue(now_ms)?;
                let previous: std::collections::HashSet<_> = before.iter().map(|card| card.id.as_str()).collect();
                let added = after.iter().any(|card| !previous.contains(card.id.as_str()));
                let version = if written > 0 || added { Some(state.db.version()?) } else { None };
                Ok::<_, rusqlite::Error>(version)
            })
            .await;
            if let Ok(Ok(version)) = result {
                last_poll_ms = now_ms;
                if let Some(version) = version {
                    let _ = poll_state.bus.send(version);
                }
            }
        }
    });
    let port = listener.local_addr()?.port();
    axum::serve(listener, router(state, Some(port))).await
}

#[cfg(test)]
mod tests;
