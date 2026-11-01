//! HTTP server. Owner: server worker. axum 0.8.
//!
//! Routes (same origin):
//! - `GET  /`                         index.html (embedded from ../ui)
//! - `GET  /style.css`, `GET /app.js`, `GET /marked.js`  embedded assets; the shell files share one ETag and `no-cache`; marked is the MIT Markdown tokenizer (ui/marked.LICENSE.md)
//! - `GET  /sw.js`                     service worker
//! - `GET  /manifest.webmanifest`      web app manifest
//! - `GET  /apple-touch-icon.png`      iOS home screen icon
//! - `GET  /fonts/Geist-Variable.woff2`, `GET /fonts/GeistMono-Variable.woff2`  bundled fonts, cached for a year
//!
//! Responses are gzipped when the client accepts it, except fonts, images and the SSE stream.
//! - `GET  /v1/queue`                 `{cards:[Card], counts:{ask,finding,info,plan}, version}`
//! - `GET  /v1/stream`                SSE; event `queue`, data `{"version":n}` on every change
//! - `POST /v1/ingest`                CloudEvent body -> 202 `{ok:true, stored:bool}`;
//!                                    `triage.finding` and `triage.info` become cards,
//!                                    any other type is stored without a card.
//! - `POST /v1/cards/{id}/answer`     AnswerReq -> `{ok:true}`, 409 `{error}` when already handled, 422 `{error}` when the action failed
//! - `POST /v1/cards/{id}/skip`       -> swipe skipped
//! - `POST /v1/cards/{id}/later`      `{minutes}` -> swipe later
//! - `POST /v1/cards/{id}/close`      `{note?}` acknowledges the card and kills its tmux pane; a card with no pane, or whose pane tmux no longer finds, is acknowledged like a plain ack, and any other kill failure is 422 with the card left queued
//! - `POST /v1/cards/{id}/dispatch` `{mode?: "investigate"|"act"|"plan"|"plan-into", note?, model?, executor?}` launches an omp session with a card brief. The mode defaults to investigate, unknown modes return 400, and ask cards defer while other cards complete. `model` and `executor` belong to `plan-into`, which plans with `model`, default the daemon's `--model`, and executes with `executor`, default omp's `smol` role, in a new window; either field with another mode, or a selector with shell characters, returns 400.
//! - `POST /v1/cards/{id}/reload`      -> `{ok:true, card}` puts a fresh copy of a handled card back in the queue, 404 `{error}` when unknown, 409 `{error}` when the card or a copy of it is already queued
//! - `GET  /v1/tmux-sessions`          `{sessions:[name], default, directories:[path]}` tmux sessions a new session can launch into; `default` is the daemon's `--session` or `triage`; `directories` are the distinct pane working directories, those most panes share first
//! - `GET  /v1/models`                 `{models:[{selector, name}], default, thinking:[level], default_thinking}` from `omp models --json`, loaded once in a background thread and filtered to the configured enabledModels so the picker offers only models that run; each `@role` name carries the role's model. The endpoint answers immediately with empty models while the first load runs. `default` and `default_thinking` are the daemon's `--model` and `--thinking`, or null when omp's own defaults apply
//! - `POST /v1/sessions` `{prompt, cwd?, session?, model?, thinking?, mode?, executor?}` launches an omp session from free text. Unset session, model and thinking fall back to the daemon's launch flags. `mode` is `goal` (default), `plan` or `plan-into`; `executor` belongs to `plan-into`, and an unknown body field returns 422.
//! - `GET  /v1/history?limit=50`      `{cards:[{card:{id,kind,title,source,options:[{label}]}, swipe}]}`, limit capped at 500
//! - `GET  /v1/cards/{id}/context`   `{anchor, items:[{card, swipes}], truncated}` the card's context thread: its transcript siblings, the panes it lived in, the launchers and steering swipes of those panes, and for an ingested card the newest 50 cards of its source, oldest first, capped at the newest 100
//! - `GET  /v1/push?endpoint=URL`     `{key, max_priority}` the daemon's VAPID public key (base64url) and the level stored for that push endpoint, null when it is not subscribed
//! - `POST /v1/push/subscribe`        `{endpoint, keys:{p256dh, auth}, max_priority}` (`PushSubscription.toJSON()` plus a level 1–5) -> `{ok:true}`, 400 `{error}` when a field is invalid; subscribing again updates the keys and level
//! - `POST /v1/push/unsubscribe`      `{endpoint}` -> `{ok:true}`, also when the endpoint was not subscribed
//! - `POST /v1/push/test`             `{endpoint}` sends a test notification -> `{ok:true}`, 404 when not subscribed, 410 when the push service dropped the subscription (the row is deleted), 502 `{error}` when the push service refused it
//! - `GET  /v1/panes/{pane}/peek`     `{text, state, width, height}`  state in asking|waiting|working|other
//! - `POST /v1/panes/{pane}/send`     `{text}`
//! - `POST /v1/panes/{pane}/jump`
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
    /// Web Push sender with the daemon's VAPID key and one HTTP agent for every push service.
    pub push: crate::push::Push,
    /// Host names accepted besides loopback, from `serve --allow-host`.
    pub allowed_hosts: Vec<String>,
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

    next.run(request).await
}

pub fn router(state: Arc<AppState>) -> axum::Router {
    router_with_notify(state, false, None)
}

fn router_with_notify(state: Arc<AppState>, notify: bool, port: Option<u16>) -> axum::Router {
    let policy = HostPolicy { port, hosts: state.allowed_hosts.clone().into() };
    axum::Router::new()
        .route("/", axum::routing::get(index))
        .route("/style.css", axum::routing::get(style))
        .route("/app.js", axum::routing::get(script))
        .route("/marked.js", axum::routing::get(markdown_parser))
        .route("/sw.js", axum::routing::get(service_worker))
        .route("/manifest.webmanifest", axum::routing::get(manifest))
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
        .route("/v1/cards/{id}/close", axum::routing::post(close))
        .route("/v1/cards/{id}/reload", axum::routing::post(reload))
        .route("/v1/sessions", axum::routing::post(start_session))
        .route("/v1/tmux-sessions", axum::routing::get(tmux_sessions))
        .route("/v1/models", axum::routing::get(models))
        .route("/v1/history", axum::routing::get(history))
        .route("/v1/push", axum::routing::get(push_settings))
        .route("/v1/push/subscribe", axum::routing::post(push_subscribe))
        .route("/v1/push/unsubscribe", axum::routing::post(push_unsubscribe))
        .route("/v1/push/test", axum::routing::post(push_test))
        .route("/v1/panes/{pane}/peek", axum::routing::get(peek))
        .route("/v1/panes/{pane}/send", axum::routing::post(send))
        .route("/v1/panes/{pane}/jump", axum::routing::post(jump))
        .layer(axum::Extension(notify))
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
/// revalidates everything while an unchanged load gets 304s.
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

async fn index(headers: axum::http::HeaderMap) -> axum::response::Response {
    shell_file(&headers, "text/html; charset=utf-8", INDEX_HTML)
}

async fn style(headers: axum::http::HeaderMap) -> axum::response::Response {
    shell_file(&headers, "text/css; charset=utf-8", STYLE_CSS)
}

async fn script(headers: axum::http::HeaderMap) -> axum::response::Response {
    shell_file(&headers, "text/javascript; charset=utf-8", APP_JS)
}

async fn markdown_parser(headers: axum::http::HeaderMap) -> axum::response::Response {
    shell_file(&headers, "text/javascript; charset=utf-8", MARKED_JS)
}

async fn service_worker(headers: axum::http::HeaderMap) -> axum::response::Response {
    shell_file(&headers, "text/javascript; charset=utf-8", SW_JS)
}

async fn manifest(headers: axum::http::HeaderMap) -> axum::response::Response {
    shell_file(&headers, "application/manifest+json", MANIFEST)
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
struct PushQuery {
    endpoint: Option<String>,
}

#[derive(serde::Deserialize)]
struct PushKeys {
    p256dh: String,
    auth: String,
}

/// `PushSubscription.toJSON()` plus the level; serde ignores `expirationTime`.
#[derive(serde::Deserialize)]
struct PushSubscribeReq {
    endpoint: String,
    keys: PushKeys,
    max_priority: u8,
}

#[derive(serde::Deserialize)]
struct PushEndpointReq {
    endpoint: String,
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
            axum::Json(serde_json::json!({"cards": cards, "counts": counts, "version": version})).into_response()
        }
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn stream(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    use futures::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let events = BroadcastStream::new(state.bus.subscribe()).filter_map(|message| async move {
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
    axum::Extension(notify): axum::Extension<bool>,
    axum::Json(event): axum::Json<crate::events::CloudEvent>,
) -> axum::response::Response {
    let card = card_from_event(&event);
    let notification_title = if notify { card.as_ref().map(|card| card.title.clone()) } else { None };
    let pushed = card.clone();
    let db = Arc::clone(&state);
    let stored = tokio::task::spawn_blocking(move || db.db.insert_event(&event, card.as_ref())).await;
    match stored {
        Ok(Ok(stored)) => {
            if stored {
                broadcast(&state).await;
                if let Some(title) = notification_title {
                    notify_card(title);
                }
                push_cards(&state, pushed.into_iter().collect());
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
    axum::Json(request): axum::Json<crate::events::AnswerReq>,
) -> axum::response::Response {
    let lookup_state = Arc::clone(&state);
    let lookup_id = id.clone();
    let found = tokio::task::spawn_blocking(move || lookup_state.db.card(&lookup_id)).await;
    let (card, swipe) = match found {
        Ok(Ok(Some(card))) => card,
        Ok(Ok(None)) => return error_response(axum::http::StatusCode::NOT_FOUND, "card not found"),
        Ok(Err(error)) => return internal_error(error),
        Err(error) => return internal_error(error),
    };
    let queued = crate::cards::queued_for_answer(swipe.as_ref());
    if !queued {
        return error_response(axum::http::StatusCode::CONFLICT, "card is not queued");
    }

    let launch = state.launch.clone();
    let action_state = Arc::clone(&state);
    match tokio::task::spawn_blocking(move || {
        crate::cards::answer(&action_state.db, &card, &request, &launch)
    })
    .await
    {
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

#[derive(Debug, Default, serde::Deserialize)]
struct DispatchRequest {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    note: Option<String>,
    /// Planner of a plan-into dispatch.
    #[serde(default)]
    model: Option<String>,
    /// Its execution model.
    #[serde(default)]
    executor: Option<String>,
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
    if !plan_into && (request.model.is_some() || request.executor.is_some()) {
        return error_response(axum::http::StatusCode::BAD_REQUEST, "model and executor need mode plan-into");
    }
    for selector in [request.model.as_deref(), request.executor.as_deref()] {
        if let Err(error) = crate::cards::model_selector(selector) {
            return error_response(axum::http::StatusCode::BAD_REQUEST, &error);
        }
    }
    let lookup_state = Arc::clone(&state);
    let lookup_id = id.clone();
    let found = tokio::task::spawn_blocking(move || lookup_state.db.card(&lookup_id)).await;
    let (card, swipe) = match found {
        Ok(Ok(Some(card))) => card,
        Ok(Ok(None)) => return error_response(axum::http::StatusCode::NOT_FOUND, "card not found"),
        Ok(Err(error)) => return internal_error(error),
        Err(error) => return internal_error(error),
    };
    if !crate::cards::queued_for_answer(swipe.as_ref()) {
        return error_response(axum::http::StatusCode::CONFLICT, "card is not queued");
    }
    let note = request.note.filter(|note| !note.trim().is_empty());
    let mut launch = state.launch.clone();
    if let Some(model) = request.model.filter(|model| !model.trim().is_empty()) {
        launch.model = Some(model);
    }
    let executor = request.executor;
    let action_state = Arc::clone(&state);
    match tokio::task::spawn_blocking(move || {
        crate::cards::dispatch(&action_state.db, &card, mode, note.as_deref(), &launch, executor.as_deref())
    })
    .await
    {
        Ok(Ok(())) => {
            broadcast(&state).await;
            axum::Json(serde_json::json!({"ok": true})).into_response()
        }
        Ok(Err(error)) => action_error_response(&error),
        Err(error) => internal_error(error),
    }
}

async fn close(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    request: Option<axum::Json<CardNoteRequest>>,
) -> axum::response::Response {
    let lookup_state = Arc::clone(&state);
    let found = tokio::task::spawn_blocking(move || lookup_state.db.card(&id)).await;
    let card = match found {
        Ok(Ok(Some((card, _)))) => card,
        Ok(Ok(None)) => return error_response(axum::http::StatusCode::NOT_FOUND, "card not found"),
        Ok(Err(error)) => return internal_error(error),
        Err(error) => return internal_error(error),
    };
    let note = request.and_then(|request| request.0.note).filter(|note| !note.trim().is_empty());
    let action_state = Arc::clone(&state);
    match tokio::task::spawn_blocking(move || crate::cards::close(&action_state.db, &card, note.as_deref())).await {
        Ok(Ok(())) => {
            broadcast(&state).await;
            axum::Json(serde_json::json!({"ok": true})).into_response()
        }
        Ok(Err(error)) => action_error_response(&error),
        Err(error) => internal_error(error),
    }
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
    model: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    executor: Option<String>,
}

/// A model the New session sheet offers, with the selector omp takes and the name it shows.
#[derive(Clone, serde::Serialize)]
struct Model {
    selector: String,
    name: String,
}

/// omp's model catalog plus the configured roles. `omp models --json` takes about twenty seconds,
/// so one background thread loads it at startup and the endpoint answers from the snapshot it has;
/// the roles come from `omp config get modelRoles --json` in the same load.
static MODELS: std::sync::RwLock<Vec<Model>> = std::sync::RwLock::new(Vec::new());

/// Starts the one catalog load thread when first forced; forcing only spawns, it never waits.
static MODELS_LOAD: std::sync::LazyLock<()> = std::sync::LazyLock::new(|| {
    let _ = std::thread::Builder::new()
        .name("triage-models".to_owned())
        .spawn(|| {
            let models = load_models();
            if let Ok(mut slot) = MODELS.write() {
                *slot = models;
            }
        });
});

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

/// Parses `omp config get enabledModels --json` output into selector strings.
fn enabled_from_config(stdout: &str) -> Vec<String> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).unwrap_or_default();
    parsed["value"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|selector| selector.as_str().map(str::to_owned))
        .collect()
}

/// The configured enabledModels whitelist; a failed query yields an empty list, which filters nothing.
fn load_enabled() -> Vec<String> {
    let output = std::process::Command::new("omp").args(["config", "get", "enabledModels", "--json"]).output();
    let Some(output) = output.ok().filter(|o| o.status.success()) else { return Vec::new() };
    enabled_from_config(&String::from_utf8_lossy(&output.stdout))
}

/// Parses `omp models --json` output. The catalog carries every bundled model, valid or not —
/// provider `amazon-bedrock` lists the bare `openai.gpt-6-luna`, which Bedrock rejects with
/// HTTP 400 because the model runs only through an inference profile (`global.`/`us.` prefix) —
/// so a configured enabledModels whitelist keeps the picker to models that actually run. An empty
/// whitelist (omp's default, everything allowed) leaves the catalog untouched.
fn parse_catalog(stdout: &str, enabled: &[String]) -> Vec<Model> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).unwrap_or_default();
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
        .collect()
}

fn load_models() -> Vec<Model> {
    let roles = load_roles();
    let enabled = load_enabled();
    let output = std::process::Command::new("omp").args(["models", "--json"]).output();
    let Some(output) = output.ok().filter(|o| o.status.success()) else { return roles };
    let catalog = parse_catalog(&String::from_utf8_lossy(&output.stdout), &enabled);
    roles.into_iter().chain(catalog).collect()
}

async fn models(State(state): State<Arc<AppState>>) -> axum::response::Response {
    // Never waits for the load: an in-flight load answers with an empty list, and a failed command
    // leaves the list with whatever did load until restart; the UI still accepts typed selectors.
    // ponytail: the load runs once per process and is never refreshed; restart to pick up models or roles.
    let models = MODELS.try_read().map(|slot| slot.clone()).unwrap_or_default();
    axum::Json(serde_json::json!({
        "models": models,
        "default": state.launch.model,
        "thinking": crate::cards::THINKING_LEVELS,
        "default_thinking": state.launch.thinking,
    }))
    .into_response()
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
    let session = request
        .session
        .filter(|session| !session.trim().is_empty())
        .or_else(|| state.launch.session.clone())
        .unwrap_or_else(|| crate::cards::DEFAULT_SESSION.to_owned());
    let model = request
        .model
        .filter(|model| !model.trim().is_empty())
        .or_else(|| state.launch.model.clone());
    let thinking = request
        .thinking
        .filter(|thinking| !thinking.trim().is_empty())
        .or_else(|| state.launch.thinking.clone());
    let action_state = Arc::clone(&state);
    match tokio::task::spawn_blocking(move || {
        let start = crate::cards::Start::parse(request.mode.as_deref(), request.executor.as_deref())?;
        crate::cards::start_session(
            &action_state.db,
            &request.prompt,
            &cwd,
            Some(&session),
            model.as_deref(),
            thinking.as_deref(),
            start,
        )
    })
    .await
    {
        Ok(Ok(pane)) => axum::Json(serde_json::json!({"ok": true, "pane": pane})).into_response(),
        Ok(Err(error)) => error_response(axum::http::StatusCode::BAD_REQUEST, &error),
        Err(error) => internal_error(error),
    }
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

async fn push_settings(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<PushQuery>,
) -> axum::response::Response {
    let lookup = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || match query.endpoint {
        Some(endpoint) => lookup.db.push_subscription(&endpoint),
        None => Ok(None),
    })
    .await;
    match result {
        Ok(Ok(subscription)) => axum::Json(serde_json::json!({
            "key": state.push.public_key(),
            "max_priority": subscription.map(|subscription| subscription.max_priority),
        }))
        .into_response(),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn push_subscribe(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<PushSubscribeReq>,
) -> axum::response::Response {
    let subscription = match crate::push::Subscription::checked(
        request.endpoint,
        request.keys.p256dh,
        request.keys.auth,
        request.max_priority,
    ) {
        Ok(subscription) => subscription,
        Err(message) => return error_response(axum::http::StatusCode::BAD_REQUEST, message),
    };
    match tokio::task::spawn_blocking(move || state.db.upsert_push_subscription(&subscription)).await {
        Ok(Ok(())) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn push_unsubscribe(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<PushEndpointReq>,
) -> axum::response::Response {
    match tokio::task::spawn_blocking(move || state.db.delete_push_subscription(&request.endpoint)).await {
        Ok(Ok(())) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn push_test(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<PushEndpointReq>,
) -> axum::response::Response {
    let result = tokio::task::spawn_blocking(move || -> rusqlite::Result<Option<crate::push::Sent>> {
        let Some(subscription) = state.db.push_subscription(&request.endpoint)? else {
            return Ok(None);
        };
        let sent = state.push.send(&subscription, crate::push::TEST_PAYLOAD, "high");
        if matches!(sent, crate::push::Sent::Gone) {
            state.db.delete_push_subscription(&subscription.endpoint)?;
        }
        Ok(Some(sent))
    })
    .await;
    match result {
        Ok(Ok(Some(crate::push::Sent::Delivered))) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Ok(Ok(Some(crate::push::Sent::Gone))) => error_response(
            axum::http::StatusCode::GONE,
            "the push service dropped this subscription; turn notifications on again",
        ),
        Ok(Ok(Some(crate::push::Sent::Failed(error)))) => error_response(axum::http::StatusCode::BAD_GATEWAY, &error),
        Ok(Ok(None)) => error_response(axum::http::StatusCode::NOT_FOUND, "this device is not subscribed"),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

fn pane_error_response(
    error: &str,
    fallback: axum::http::StatusCode,
) -> axum::response::Response {
    let status = if error == "pane not found" {
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

fn notify_card(title: String) {
    tokio::spawn(async move {
        let _ = tokio::process::Command::new("notify-send")
            .arg("triage")
            .arg(title)
            .status()
            .await;
    });
}

/// Pushes new cards to subscribed devices on a blocking thread, since ureq blocks.
fn push_cards(state: &Arc<AppState>, cards: Vec<crate::events::Card>) {
    if cards.is_empty() {
        return;
    }
    let state = Arc::clone(state);
    // Devices get their pushes one after another. Send them in parallel only if one slow push service ever holds up the rest.
    tokio::task::spawn_blocking(move || {
        for card in &cards {
            state.push.notify(&state.db, card);
        }
    });
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
            (
                Kind::Info,
                ev.priority.unwrap_or(3),
                string_field(data, "title").unwrap_or_else(|| source.label.clone()),
                Star::default(),
                vec![CardOption { label: "Acknowledge".into(), recommended: true, ..CardOption::default() }],
                true,
                false,
                data.get("body").map(|value| value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())),
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
        questions: Vec::new(),
        body,
        refs,
        level,
        tool_call_id: None,
    })
}

/// Bind and serve forever. Also spawns the pane tracker loop and queue notifications.
pub async fn serve(
    state: Arc<AppState>,
    addr: std::net::SocketAddr,
    notify: bool,
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
                let written = tracker.tick(&state.db).written;
                let after = state.db.queue(now_ms)?;
                let previous: std::collections::HashSet<_> = before.iter().map(|card| card.id.as_str()).collect();
                let added = after.into_iter().filter(|card| !previous.contains(card.id.as_str())).collect::<Vec<_>>();
                let version = if written > 0 || !added.is_empty() { Some(state.db.version()?) } else { None };
                Ok::<_, rusqlite::Error>((version, added))
            })
            .await;
            if let Ok(Ok((version, added))) = result {
                last_poll_ms = now_ms;
                if let Some(version) = version {
                    let _ = poll_state.bus.send(version);
                }
                if notify {
                    for card in &added {
                        notify_card(card.title.clone());
                    }
                }
                push_cards(&poll_state, added);
            }
        }
    });
    let port = listener.local_addr()?.port();
    axum::serve(listener, router_with_notify(state, notify, Some(port))).await
}

#[cfg(test)]
mod tests {
    use super::{card_from_event, AppState};
    use crate::db::Db;
    use crate::events::{CloudEvent, Kind, Swipe};
    use axum::http::StatusCode;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn host_without_port_matches_configured_host_port() {
        let host = "localhost".parse::<axum::http::uri::Authority>().unwrap();
        let mismatched_port = "localhost:3001".parse::<axum::http::uri::Authority>().unwrap();

        assert!(super::valid_host(&host, Some(3000), &[]));
        assert!(!super::valid_host(&mismatched_port, Some(3000), &[]));
    }

    #[test]
    fn extra_allowed_host_is_accepted_only_when_configured() {
        let configured = ["inbox.example.test".to_owned()];
        let host = "inbox.example.test:7340".parse::<axum::http::uri::Authority>().unwrap();
        let evil = "evil.example:7340".parse::<axum::http::uri::Authority>().unwrap();
        let localhost = "localhost:7340".parse::<axum::http::uri::Authority>().unwrap();

        assert!(super::valid_host(&host, None, &configured));
        assert!(!super::valid_host(&host, None, &[]));
        assert!(!super::valid_host(&evil, None, &configured));
        assert!(super::valid_host(&localhost, None, &[]));
    }

    #[test]
    fn roles_from_config_parses_role_names_as_role_selectors() {
        let config = r#"{"key":"modelRoles","value":{"smol":"provider/b","default":"provider/a","plan":"provider/c"},"type":"record","description":""}"#;
        let roles = super::roles_from_config(config);
        let entries: Vec<(&str, &str)> =
            roles.iter().map(|role| (role.selector.as_str(), role.name.as_str())).collect();
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
    fn enabled_from_config_parses_selector_strings() {
        let config = r#"{"key":"enabledModels","value":["provider/a","provider/b"],"type":"list","description":""}"#;
        assert_eq!(super::enabled_from_config(config), ["provider/a", "provider/b"]);
        assert!(super::enabled_from_config(r#"{"key":"enabledModels","value":null}"#).is_empty());
        assert!(super::enabled_from_config("not json").is_empty());
    }

    #[test]
    fn parse_catalog_keeps_only_enabled_selectors() {
        let catalog = r#"{"models":[
            {"selector":"provider/a","name":"Alpha"},
            {"selector":"provider/b","name":""},
            {"selector":"provider/c","name":"Gamma"}
        ]}"#;
        let enabled = vec!["provider/a".to_owned(), "provider/b".to_owned()];
        let models = super::parse_catalog(catalog, &enabled);
        let entries: Vec<(&str, &str)> =
            models.iter().map(|m| (m.selector.as_str(), m.name.as_str())).collect();
        assert_eq!(entries, [("provider/a", "Alpha"), ("provider/b", "provider/b")]);
        assert_eq!(super::parse_catalog(catalog, &[]).len(), 3);
        assert!(super::parse_catalog("not json", &enabled).is_empty());
    }

    fn event_with_id(
        event_type: &str,
        source: &str,
        id: &str,
        data: serde_json::Value,
    ) -> CloudEvent {
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
        })
    }

    fn state(db: Db, launch_session: Option<&str>) -> Arc<AppState> {
        state_with(db, crate::cards::Launch { session: launch_session.map(str::to_owned), ..crate::cards::Launch::default() })
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
        assert!(transcript.contains("1 passed"), "child ran no test: {transcript}");
        true
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_session_prefers_request_then_daemon_launch_settings() {
        if run_with_fake_tmux_child("server::tests::new_session_prefers_request_then_daemon_launch_settings") {
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
        };
        let response = super::start_session(axum::extract::State(daemon_state.clone()), axum::Json(request)).await;
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
        };
        let response = super::start_session(axum::extract::State(daemon_state), axum::Json(request)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let launch = last_launch();
        assert!(launch.contains("-t bill:") && launch.contains("--model=provider/chosen --plan=provider/chosen --thinking=high --goal 'Reply pong."), "{launch}");

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
        };
        let response = super::start_session(axum::extract::State(bare), axum::Json(request)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let launch = last_launch();
        assert!(launch.contains("-t triage:") && launch.contains(" omp --goal 'Reply pong."), "{launch}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_session_plan_mode_launches_with_config_overlay() {
        if run_with_fake_tmux_child("server::tests::new_session_plan_mode_launches_with_config_overlay") {
            return;
        }
        let directory = TestDir::new("triage-session-plan");
        let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
        let state = state(db, None);
        let defaulted: super::SessionRequest = serde_json::from_value(serde_json::json!({"prompt": "Default plan"})).unwrap();
        assert_eq!(defaulted.mode, None);
        let request = super::SessionRequest {
            prompt: "Prepare a plan.".to_owned(),
            cwd: Some("/tmp".to_owned()),
            session: Some("bill".to_owned()),
            model: None,
            thinking: None,
            mode: Some("plan".to_owned()),
            executor: None,
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
            .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("plan-mode-"))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&overlay).unwrap(), "plan:\n  defaultOnStartup: true\n");
        let brief = std::fs::read_dir(directory.0.join("briefs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("triage-brief-"))
            .unwrap();
        assert!(launch.contains(&format!("--config '{}' @'{}'", overlay.display(), brief.display())), "{launch}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_plan_session_keeps_daemon_default_model_in_plan_mode() {
        if run_with_fake_tmux_child("server::tests::new_plan_session_keeps_daemon_default_model_in_plan_mode") {
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
            launch.contains(" --model=provider/daemon-model --plan=provider/daemon-model --thinking=max @'"),
            "{launch}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_session_plan_into_launches_plan_yolo_with_the_execution_model() {
        if run_with_fake_tmux_child("server::tests::new_session_plan_into_launches_plan_yolo_with_the_execution_model") {
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
            },
        );
        let request = super::SessionRequest {
            prompt: "Prepare a plan.".to_owned(),
            cwd: Some("/tmp".to_owned()),
            session: None,
            model: None,
            thinking: None,
            mode: Some("plan-into".to_owned()),
            executor: Some("provider/executor".to_owned()),
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
            launch.contains(" --model=provider/daemon-model --plan=provider/daemon-model --thinking=low --plan-yolo --plan-yolo-into=provider/executor @'"),
            "{launch}"
        );
        assert!(!launch.contains("--config"), "{launch}");
        assert!(!launch.contains("--goal"), "{launch}");
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

    async fn start_http_server(state: Arc<AppState>) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        start_http_server_with_router(super::router(state)).await
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
    ) -> (Arc<super::PreWriteGate>, tokio::sync::mpsc::UnboundedReceiver<()>) {
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
        matches!(tokio::time::timeout(timeout, arrivals.recv()).await, Ok(Some(())))
    }

    #[cfg(unix)]
    fn create_fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;

        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
        assert_eq!(result, 0, "could not create FIFO: {}", std::io::Error::last_os_error());
    }

    #[cfg(unix)]
    fn fifo_arrivals(path: &Path) -> tokio::sync::mpsc::UnboundedReceiver<()> {
        use std::io::Read;

        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut fifo = std::fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
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
        http_request_with_headers(address, method, path, body, "localhost", Some("application/json"), None).await
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
        let content_type = content_type.map(|value| format!("Content-Type: {value}\r\n")).unwrap_or_default();
        let origin = origin.map(|value| format!("Origin: {value}\r\n")).unwrap_or_default();
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
                header.eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
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
        assert_eq!(statuses.iter().filter(|status| **status == StatusCode::OK).count(), 1);
        assert_eq!(statuses.iter().filter(|status| **status == StatusCode::CONFLICT).count(), 1);
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
    fn unknown_event_makes_no_card() {
        let data = serde_json::json!({"detail": "unexpected"});
        assert!(card_from_event(&event("custom.notice", "tool://watcher", data)).is_none());
    }

    #[tokio::test]
    async fn serves_pwa_assets() {
        let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
        let (address, server) = start_http_server(state).await;

        let worker = http_request(address, "GET", "/sw.js", "").await;
        assert_eq!(worker.status, StatusCode::OK);
        assert_eq!(worker.content_type.as_deref(), Some("text/javascript; charset=utf-8"));
        assert_eq!(worker.cache_control.as_deref(), Some("no-cache"));

        let manifest = http_request(address, "GET", "/manifest.webmanifest", "").await;
        assert_eq!(manifest.status, StatusCode::OK);
        assert_eq!(manifest.content_type.as_deref(), Some("application/manifest+json"));
        assert_eq!(manifest.body["display"], "standalone");

        let icon = http_request(address, "GET", "/apple-touch-icon.png", "").await;
        assert_eq!(icon.status, StatusCode::OK);
        assert_eq!(icon.content_type.as_deref(), Some("image/png"));
        assert!(icon.raw_body.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']));
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
        assert_eq!(row["card"]["options"], serde_json::json!([{"label": "Acknowledge"}]));
        assert_eq!(row["swipe"]["option"], 0);
        assert!(row["card"].get("body").is_none(), "history rows carry no card body");
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
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
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
        let (address, server) = start_http_server(state(Db::open(Path::new(":memory:")).unwrap(), None)).await;
        let get = |extra: &str| format!("GET /app.js HTTP/1.1\r\nHost: localhost\r\n{extra}Connection: close\r\n\r\n");

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

    /// The RFC 8291 receiver's public key and auth secret.
    const UA_PUBLIC: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

    type PushHit = (String, axum::http::HeaderMap, Vec<u8>);

    /// A push service at `{base}{id}` that records every delivery and answers `status`.
    async fn stub_push_service(status: StatusCode) -> (String, tokio::sync::mpsc::UnboundedReceiver<PushHit>) {
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
        crate::push::Subscription { endpoint, p256dh: UA_PUBLIC.to_owned(), auth: AUTH.to_owned(), max_priority }
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
        assert_eq!(stored.body["max_priority"], 1, "subscribing again must replace the level");
        assert_eq!(stored.body["key"].as_str().unwrap().len(), 87);
        assert!(http_request(address, "GET", "/v1/push", "").await.body["max_priority"].is_null());

        let forgotten =
            http_request(address, "POST", "/v1/push/unsubscribe", r#"{"endpoint":"https://push.example.test/abc"}"#).await;
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
            ("http://push.example.test/abc", UA_PUBLIC, AUTH, 2, "endpoint must be an https URL"),
            (https, AUTH, AUTH, 2, "p256dh must be an uncompressed P-256 public key"),
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
        state.db.upsert_push_subscription(&subscription(format!("{base}important"), 2)).unwrap();
        state.db.upsert_push_subscription(&subscription(format!("{base}urgent"), 1)).unwrap();
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
        assert!(authorization.ends_with(&format!(", k={}", state.push.public_key())), "{authorization}");
        // Record size 4096, then the 65-byte sender key, after the 16-byte salt.
        assert_eq!(body[16..21], [0, 0, 16, 0, 65]);
        assert!(
            tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await.is_err(),
            "the urgent-only device must not hear a priority-2 card"
        );
        server.abort();
    }

    #[tokio::test]
    async fn push_test_drops_a_subscription_the_service_forgot() {
        let state = state(Db::open(Path::new(":memory:")).unwrap(), None);
        let (base, _pushes) = stub_push_service(StatusCode::GONE).await;
        let endpoint = format!("{base}forgotten");
        state.db.upsert_push_subscription(&subscription(endpoint.clone(), 2)).unwrap();
        let (address, server) = start_http_server(state.clone()).await;
        let body = serde_json::json!({"endpoint": endpoint}).to_string();

        assert_eq!(http_request(address, "POST", "/v1/push/test", &body).await.status, StatusCode::GONE);
        assert!(state.db.push_subscription(&endpoint).unwrap().is_none());
        assert_eq!(http_request(address, "POST", "/v1/push/test", &body).await.status, StatusCode::NOT_FOUND);
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
        assert_eq!(state.db.card(&first_card.id).unwrap().unwrap().1.unwrap().action, "skipped");
        assert!(state.db.card(&second_card.id).unwrap().unwrap().1.is_none());
        server.abort();
    }
    #[tokio::test]
    async fn legacy_card_key_collision_does_not_block_ingest() {
        let directory = TestDir::new("triage-legacy-card-collision");
        let database = directory.0.join("triage.sqlite");
        let collision_id = crate::db::cloud_event_key("urn:x", "y");
        let legacy_event = event_with_id(
            crate::events::T_INFO,
            "urn:legacy",
            &collision_id,
            serde_json::json!({"title": "Legacy"}),
        );
        let legacy_card_id = crate::db::cloud_event_key(&legacy_event.source, &legacy_event.id);
        let mut legacy_card = card_from_event(&legacy_event).unwrap();
        legacy_card.id = legacy_event.id.clone();
        legacy_card.created_ms = 10;
        let legacy_swipe = Swipe {
            card: legacy_event.id.clone(),
            action: "later".into(),
            option: None,
            text: None,
            note: Some("preserve this note".into()),
            until_ms: Some(1),
            reason: Some("legacy defer".into()),
            at_ms: 11,
        };
        {
            let connection = rusqlite::Connection::open(&database).unwrap();
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
                    );
                    PRAGMA user_version = 1;",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                     VALUES (1, ?1, 1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        legacy_card_id,
                        legacy_event.source,
                        legacy_event.r#type,
                        legacy_event.id,
                        legacy_event.priority.map(i64::from),
                        serde_json::to_string(&legacy_event).unwrap(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                     VALUES (2, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        format!("card:{}", legacy_card.id),
                        legacy_card.created_ms,
                        legacy_card.source.uri,
                        legacy_card.card_event_type(),
                        legacy_card.id,
                        i64::from(legacy_card.priority),
                        serde_json::to_string(&legacy_card).unwrap(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO events(rowid, id, ts_ms, source, type, card_id, priority, data)
                     VALUES (3, 'swipe:legacy', ?1, 'triage', ?2, ?3, NULL, ?4)",
                    rusqlite::params![
                        legacy_swipe.at_ms,
                        crate::events::T_SWIPE,
                        legacy_swipe.card,
                        serde_json::to_string(&legacy_swipe).unwrap(),
                    ],
                )
                .unwrap();
        }
        let db = Db::open(&database).unwrap();
        let state = state(db, None);
        let (address, server) = start_http_server(state.clone()).await;
        let incoming = event_with_id(
            crate::events::T_INFO,
            "urn:x",
            "y",
            serde_json::json!({"title": "Incoming"}),
        );
        let response = http_request(
            address,
            "POST",
            "/v1/ingest",
            &serde_json::to_string(&incoming).unwrap(),
        )
        .await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        assert_eq!(response.body["stored"], true);

        let old_card = state.db.card(&legacy_card_id).unwrap().unwrap();
        let incoming_card = state.db.card(&collision_id).unwrap().unwrap();
        assert_ne!(old_card.0.id, incoming_card.0.id);
        assert_eq!(old_card.0.title, "Legacy");
        assert_eq!(old_card.1.as_ref().unwrap().card, legacy_card_id);
        assert_eq!(old_card.1.as_ref().unwrap().action, "later");
        assert_eq!(old_card.1.as_ref().unwrap().note.as_deref(), Some("preserve this note"));

        let first_version = state.db.version().unwrap();
        let duplicate = http_request(
            address,
            "POST",
            "/v1/ingest",
            &serde_json::to_string(&incoming).unwrap(),
        )
        .await;
        assert_eq!(duplicate.status, StatusCode::ACCEPTED);
        assert_eq!(duplicate.body["stored"], false);
        assert_eq!(state.db.version().unwrap(), first_version);

        let queue = http_request(address, "GET", "/v1/queue", "").await;
        assert!(queue.body["cards"].as_array().unwrap().iter().any(|card| {
            card["id"].as_str() == Some(legacy_card_id.as_str())
        }));
        assert!(queue.body["cards"].as_array().unwrap().iter().any(|card| {
            card["id"].as_str() == Some(collision_id.as_str())
        }));
        let history = http_request(address, "GET", "/v1/history?limit=50", "").await;
        assert_eq!(history.status, StatusCode::OK);
        let legacy_history = history.body["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["card"]["id"].as_str() == Some(legacy_card_id.as_str()))
            .unwrap();
        assert_eq!(legacy_history["swipe"]["card"], legacy_card_id);
        assert_eq!(legacy_history["swipe"]["action"], "later");
        assert_eq!(legacy_history["swipe"]["note"], "preserve this note");
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
        let missing = http_request(address, "POST", "/v1/cards/missing/dispatch", r#"{"mode":"act"}"#).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        server.abort();
    }
    #[tokio::test]
    async fn read_endpoints_reject_hostile_host() {
        let (address, server) = start_http_server(state(Db::open(Path::new(":memory:")).unwrap(), None)).await;
        for path in ["/v1/queue", "/v1/history", "/v1/cards/x/context", "/v1/panes/1/peek"] {
            let response = http_request_with_headers(address, "GET", path, "", "evil.example", None, None).await;
            assert_eq!(response.status, StatusCode::FORBIDDEN, "{path}");
        }
        server.abort();
    }
    #[tokio::test]
    async fn optional_body_posts_require_valid_host_origin_and_json_content_type() {
        let (address, server) = start_http_server(state(Db::open(Path::new(":memory:")).unwrap(), None)).await;
        let path = "/v1/cards/missing/dispatch";
        for (host, content_type, origin, expected) in [
            ("localhost", Some("application/json"), Some("https://evil.example"), StatusCode::FORBIDDEN),
            ("evil.example", Some("application/json"), None, StatusCode::FORBIDDEN),
            ("localhost", None, None, StatusCode::UNSUPPORTED_MEDIA_TYPE),
            ("localhost", Some("text/plain"), None, StatusCode::UNSUPPORTED_MEDIA_TYPE),
        ] {
            let response = http_request_with_headers(address, "POST", path, "", host, content_type, origin).await;
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
        ).await;
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
case "$1" in
capture-pane) printf '╭─ Plan Review ─╮\n' ;;
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
        let escape = log.find("send-keys -t %43 Esc").unwrap();
        let note = log.find("send-keys -t %43 -l -- Add rollback step").unwrap();
        let enter = note + log[note..].find("send-keys -t %43 Enter").unwrap();
        assert!(escape < note && note < enter);
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
        assert!(queue.body["cards"].as_array().unwrap().iter().any(|card| {
            card["id"].as_str() == Some(queued.id.as_str())
        }));
        let history = http_request(address, "GET", "/v1/history?limit=50", "").await;
        assert!(history.body["cards"].as_array().unwrap().iter().any(|entry| {
            entry["card"]["id"].as_str() == Some(completed.id.as_str())
                && entry["card"]["kind"].as_str() == Some("plan")
        }));
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
        assert!(response.body["error"].as_str().unwrap().contains("plan mode"));
        assert!(log.lines().any(|line| line == "send-keys -t %42 M-P"));
        assert!(state.db.card(&id).unwrap().unwrap().1.is_none());
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plan_into_dispatch_plans_with_the_requested_model() {
        if run_with_fake_tmux_child("server::tests::plan_into_dispatch_plans_with_the_requested_model") {
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
            },
        );
        let (address, server) = start_http_server(state).await;
        let response = http_request(
            address,
            "POST",
            &format!("/v1/cards/{}/dispatch", card.id),
            r#"{"mode":"plan-into","model":"provider/chosen","executor":"provider/executor"}"#,
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
        assert!(launch.contains("--model=provider/chosen --plan=provider/chosen"), "{launch}");
        assert!(launch.contains("--plan-yolo --plan-yolo-into=provider/executor @'"), "{launch}");
        assert!(!launch.contains("provider/daemon-model"), "{launch}");
        server.abort();
    }

    #[tokio::test]
    async fn dispatch_rejects_models_outside_plan_into_and_bad_selectors_before_touching_the_card() {
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
        assert_eq!(outside.body["error"], "model and executor need mode plan-into");
        let model = http_request(address, "POST", &path, r#"{"model":"provider/x"}"#).await;
        assert_eq!(model.status, StatusCode::BAD_REQUEST);
        assert_eq!(model.body["error"], "model and executor need mode plan-into");
        let bad = http_request(address, "POST", &path, r#"{"mode":"plan-into","executor":"x; rm -rf ~"}"#).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert_eq!(bad.body["error"], "invalid model x; rm -rf ~");
        assert!(state.db.card(&id).unwrap().unwrap().1.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn new_session_rejects_unknown_mode_and_executor_outside_plan_into() {
        let directory = TestDir::new("triage-session-mode-rejects");
        let db = Db::open(&directory.0.join("triage.sqlite")).unwrap();
        let state = state(db, None);
        let (address, server) = start_http_server(state).await;
        let unknown = http_request(address, "POST", "/v1/sessions", r#"{"prompt":"Reply pong.","mode":"yolo"}"#).await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
        assert_eq!(unknown.body["error"], "unknown mode");
        let executor = http_request(address, "POST", "/v1/sessions", r#"{"prompt":"Reply pong.","mode":"goal","executor":"provider/x"}"#).await;
        assert_eq!(executor.status, StatusCode::BAD_REQUEST);
        assert_eq!(executor.body["error"], "executor needs mode plan-into");
        let bad = http_request(address, "POST", "/v1/sessions", r#"{"prompt":"Reply pong.","mode":"plan-into","executor":"x; rm -rf ~"}"#).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        let stale = http_request(address, "POST", "/v1/sessions", r#"{"prompt":"Reply pong.","plan":true}"#).await;
        assert_eq!(stale.status, StatusCode::UNPROCESSABLE_ENTITY);
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
            ("/v1/cards/missing/later".to_owned(), r#"{"minutes":1}"#.to_owned()),
        ] {
            assert_eq!(http_request(address, "POST", &path, &body).await.status, StatusCode::NOT_FOUND);
        }
        assert_eq!(
            http_request(address, "POST", &format!("/v1/cards/{skipped_id}/skip"), "").await.status,
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
        let skip = spawn_request(address, "POST", format!("/v1/cards/{id}/skip"), String::new());
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
        let skip = spawn_request(address, "POST", format!("/v1/cards/{id}/skip"), String::new());
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
        if run_with_fake_tmux_child("server::tests::concurrent_finding_answers_launch_one_window_and_conflict") {
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
        assert_eq!(calls.lines().filter(|call| *call == "new-window").count(), 1);
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
        });
        let server = tokio::spawn(super::serve(state.clone(), "127.0.0.1:0".parse().unwrap(), false));
        assert!(
            tokio::time::timeout(Duration::from_millis(500), updates.recv()).await.is_err(),
            "no queue update should arrive before snooze expiry",
        );
        let update = tokio::time::timeout(Duration::from_secs(5), updates.recv()).await;
        server.abort();
        assert!(matches!(update, Ok(Ok(_))));
        assert!(state.db.queue(crate::events::now_ms()).unwrap().iter().any(|card| card.id == id));
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
        let copy = queued.iter().find(|card| card["id"] == copy_id).expect("copy is queued");
        assert_eq!(copy["title"], "Reload me");
        assert_eq!(copy["priority"], card.priority);
        assert!(queued.iter().all(|card| card["id"] != original_id.as_str()));

        let history = http_request(address, "GET", "/v1/history", "").await;
        assert!(history.body["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["card"]["id"] == original_id.as_str()));

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
        let response = tokio::time::timeout(Duration::from_secs(1), super::models(axum::extract::State(state(Db::open(Path::new(":memory:")).unwrap(), None))))
            .await
            .expect("models endpoint must answer while the catalog load runs");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["models"], serde_json::json!([]), "an in-flight load must answer with an empty list");
    }
    #[test]
    fn pane_directories_list_each_directory_once_most_shared_first() {
        assert_eq!(
            super::pane_directories(["/srv/b", "/srv/a", "", "/srv/c", "/srv/c", "/srv/b", "/srv/d", "/srv/c"]),
            ["/srv/c", "/srv/b", "/srv/a", "/srv/d"]
        );
    }
}
