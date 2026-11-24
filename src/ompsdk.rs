//! Client for the ompsdk daemon's ACP socket. One connection per process carries every ompsdk
//! session triage holds. A reader thread routes the daemon's replies, agent requests and turn
//! ends into the state each session's calls and tracker read.

use crate::cards::THINKING_LEVELS;
use crate::events::Card;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a call waits for its reply.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// The pool the triage sessions run in. `pools.toml` names it.
const POOL: &str = "triage";

/// The process-wide connection and the socket it reaches.
static CLIENT: Mutex<Option<(PathBuf, Arc<Client>)>> = Mutex::new(None);
static CLIENTS_MADE: AtomicU64 = AtomicU64::new(0);

/// The live connection to `socket`, opened on first use and reopened once its reader ended.
pub fn client(socket: &Path) -> Result<Arc<Client>, String> {
    let mut slot = CLIENT.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some((path, client)) = slot.as_ref()
        && path == socket
        && client.link.open.load(Ordering::SeqCst)
    {
        return Ok(Arc::clone(client));
    }
    let client = Arc::new(Client::connect(socket)?);
    *slot = Some((socket.to_path_buf(), Arc::clone(&client)));
    Ok(client)
}

/// The URI prefix of a card whose source is an ompsdk session.
pub const SESSION_URI_PREFIX: &str = "ompsdk://session/";

/// The ompsdk session a card belongs to, from its `ompsdk://session/<sid>` source.
pub fn session_id(card: &Card) -> Option<&str> {
    card.source.uri.strip_prefix(SESSION_URI_PREFIX)
}

/// The socket ompsdk serves on: `$XDG_RUNTIME_DIR/ompsdk/sock`, or the same path under the user's runtime directory when that variable is unset.
pub fn default_socket() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("ompsdk/sock"),
        None => PathBuf::from(format!("/run/user/{}/ompsdk/sock", unsafe { libc::getuid() })),
    }
}

/// One connection to the daemon.
pub struct Client {
    /// Distinguishes connections, so a caller sees a reconnect.
    pub id: u64,
    link: Arc<Link>,
}

/// State shared with the reader thread.
struct Link {
    writer: Mutex<UnixStream>,
    prefix: String,
    next: AtomicU64,
    open: AtomicBool,
    pending: Mutex<HashMap<String, Waiting>>,
    live: Mutex<HashMap<String, Session>>,
}

/// What a reply to one of our requests completes.
enum Waiting {
    Call(mpsc::Sender<Result<Value, String>>),
    /// A prompt. Its reply ends the session's turn.
    Prompt(String),
    /// A `session/new` whose caller timed out. The daemon still binds its session to this
    /// connection. Triage closes that session when the reply arrives.
    Abandoned,
}

/// What the connection knows about one session.
#[derive(Default, Clone)]
struct Session {
    turn: Turn,
    options: Value,
    elicitations: Vec<Elicitation>,
}

/// Where a session's turn stands, as the daemon reports it.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub enum Turn {
    /// The connection has seen no turn for this session.
    #[default]
    Idle,
    Running,
    /// The turn ended. Holds the stop reason the daemon gave, or `resumed` when a resume found no run.
    Ended(String),
}

/// A question or plan review the agent waits on.
#[derive(Clone, Debug)]
pub struct Elicitation {
    pub rpc_id: Value,
    pub message: String,
    pub schema: Value,
}

impl Elicitation {
    /// The request id as a string, which names the card the elicitation became.
    pub fn key(&self) -> String {
        match &self.rpc_id {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    }
}

impl Client {
    fn connect(socket: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(socket)
            .map_err(|error| format!("could not reach ompsdk at {}: {error}", socket.display()))?;
        let reader = stream.try_clone().map_err(|error| error.to_string())?;
        let link = Arc::new(Link {
            writer: Mutex::new(stream),
            prefix: format!("t{:08x}", boot_id()),
            next: AtomicU64::new(0),
            open: AtomicBool::new(true),
            pending: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
        });
        let reading = Arc::clone(&link);
        std::thread::spawn(move || read_loop(&reading, reader));
        let client = Self {
            id: CLIENTS_MADE.fetch_add(1, Ordering::SeqCst),
            link,
        };
        client.call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {"elicitation": {"form": {}}},
                "clientInfo": {"name": "triage", "version": env!("CARGO_PKG_VERSION")},
                "_meta": {"ompsdk": {"pool": POOL, "lifetime": "durable"}},
            }),
        )?;
        Ok(client)
    }

    /// Start a session in `cwd` and return its id.
    pub fn new_session(&self, cwd: &str) -> Result<String, String> {
        let result = self.call("session/new", json!({"cwd": cwd, "mcpServers": []}))?;
        let sid = result["sessionId"]
            .as_str()
            .ok_or("session/new gave no sessionId")?
            .to_owned();
        self.store_options(&sid, &result);
        Ok(sid)
    }

    /// Reopen a session the daemon no longer holds. Returns whether a turn is still running in it.
    pub fn resume(&self, sid: &str, cwd: &str) -> Result<bool, String> {
        let result = self.call(
            "session/resume",
            json!({"sessionId": sid, "cwd": cwd, "mcpServers": []}),
        )?;
        self.store_options(sid, &result);
        let running = result["_meta"]["ompsdk"]["running"].as_bool().unwrap_or(false);
        self.with(sid, |session| {
            // A turn end that arrived before this reply is already recorded, and the reply must not undo it.
            if !matches!(session.turn, Turn::Ended(_)) {
                session.turn = if running { Turn::Running } else { Turn::Ended("resumed".to_owned()) };
            }
        });
        Ok(running)
    }

    /// Set one config option, such as `mode`, `model` or `thinking`, to `value`.
    pub fn set_config(&self, sid: &str, config_id: &str, value: &str) -> Result<(), String> {
        let result = self.call(
            "session/set_config_option",
            json!({"sessionId": sid, "configId": config_id, "value": value}),
        )?;
        self.store_options(sid, &result);
        Ok(())
    }

    /// Send a prompt without waiting for its turn. The reply ends the turn when it arrives.
    pub fn prompt(&self, sid: &str, blocks: Value) -> Result<(), String> {
        self.with(sid, |session| session.turn = Turn::Running);
        let id = self.link.register(Waiting::Prompt(sid.to_owned()));
        self.link
            .send(&json!({"jsonrpc": "2.0", "id": id, "method": "session/prompt", "params": {"sessionId": sid, "prompt": blocks}}))
            .inspect_err(|_| self.link.forget(&id))
    }

    /// Put text into the running turn. Returns the daemon's outcome; anything but `injected`
    /// means no turn took it.
    pub fn steer(&self, sid: &str, text: &str) -> Result<String, String> {
        let result = self.call(
            "_session/steering",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": text}]}),
        )?;
        Ok(result["outcome"].as_str().unwrap_or_default().to_owned())
    }

    /// Dispose of a session. Its transcript stays on disk.
    pub fn close(&self, sid: &str) -> Result<(), String> {
        self.call("session/close", json!({"sessionId": sid}))?;
        self.link
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(sid);
        Ok(())
    }

    /// Answer an agent request and drop it from the session's pending list.
    pub fn respond(&self, sid: &str, rpc_id: &Value, result: Value) -> Result<(), String> {
        self.link
            .send(&json!({"jsonrpc": "2.0", "id": rpc_id, "result": result}))?;
        self.with(sid, |session| {
            session.elicitations.retain(|open| &open.rpc_id != rpc_id);
        });
        Ok(())
    }

    /// The session's turn as the connection last saw it.
    pub fn turn(&self, sid: &str) -> Turn {
        self.snapshot(sid).turn
    }

    /// The elicitations the agent is waiting on in the session.
    pub fn elicitations(&self, sid: &str) -> Vec<Elicitation> {
        self.snapshot(sid).elicitations
    }

    /// One pending elicitation by its key.
    pub fn elicitation(&self, sid: &str, key: &str) -> Option<Elicitation> {
        self.elicitations(sid)
            .into_iter()
            .find(|open| open.key() == key)
    }

    /// The session's config options as the daemon last reported them. Empty until a reply carried them.
    pub fn options(&self, sid: &str) -> Value {
        self.snapshot(sid).options
    }

    fn snapshot(&self, sid: &str) -> Session {
        self.link
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(sid)
            .cloned()
            .unwrap_or_default()
    }

    fn with(&self, sid: &str, change: impl FnOnce(&mut Session)) {
        let mut live = self
            .link
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        change(live.entry(sid.to_owned()).or_default());
    }

    fn store_options(&self, sid: &str, result: &Value) {
        if let Some(options) = result.get("configOptions") {
            self.with(sid, |session| session.options = options.clone());
        }
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        if !self.link.open.load(Ordering::SeqCst) {
            return Err("ompsdk connection closed".to_owned());
        }
        let (tx, rx) = mpsc::channel();
        let id = self.link.register(Waiting::Call(tx));
        if let Err(error) = self
            .link
            .send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        {
            self.link.forget(&id);
            return Err(error);
        }
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                let mut pending = self.link.pending.lock().unwrap_or_else(|poison| poison.into_inner());
                if pending.remove(&id).is_none() {
                    // The reply won the race and is being delivered now.
                    drop(pending);
                    return rx.recv().unwrap_or_else(|_| Err(format!("{method} timed out")));
                }
                if method == "session/new" {
                    pending.insert(id, Waiting::Abandoned);
                }
                Err(format!("{method} timed out"))
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(writer) = self.link.writer.lock() {
            let _ = writer.shutdown(Shutdown::Both);
        }
    }
}

impl Link {
    fn register(&self, waiting: Waiting) -> String {
        let id = format!("{}-{}", self.prefix, self.next.fetch_add(1, Ordering::SeqCst));
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(id.clone(), waiting);
        id
    }

    fn forget(&self, id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(id);
    }

    fn send(&self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        let mut writer = self.writer.lock().unwrap_or_else(|poison| poison.into_inner());
        writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.flush())
            .map_err(|error| format!("ompsdk write failed: {error}"))
    }

    fn route(&self, message: Value) {
        let method = message.get("method").and_then(Value::as_str);
        match (method, message.get("id")) {
            (Some(method), Some(id)) => self.agent_request(method, id, &message["params"]),
            (Some(method), None) => self.notification(method, &message["params"]),
            (None, Some(id)) => self.response(id, &message),
            (None, None) => {}
        }
    }

    fn agent_request(&self, method: &str, id: &Value, params: &Value) {
        let reply = match method {
            "session/request_permission" => json!({
                "outcome": {"outcome": "selected", "optionId": permission_option(params)},
            }),
            "elicitation/create" => {
                let sid = params["sessionId"].as_str().unwrap_or_default().to_owned();
                let schema = params["requestedSchema"].clone();
                if is_confirm(&schema) {
                    // Triage declines a confirm, since the user never saw the step it asks about.
                    let _ = self.send(&json!({"jsonrpc": "2.0", "id": id, "result": {"action": "decline"}}));
                } else {
                    let elicitation = Elicitation {
                        rpc_id: id.clone(),
                        message: params["message"].as_str().unwrap_or_default().to_owned(),
                        schema,
                    };
                    self.live_entry(&sid, |session| session.elicitations.push(elicitation));
                }
                return;
            }
            _ => {
                let _ = self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}));
                return;
            }
        };
        let _ = self.send(&json!({"jsonrpc": "2.0", "id": id, "result": reply}));
    }

    fn notification(&self, method: &str, params: &Value) {
        if method != "_ompsdk/turn_end" {
            return;
        }
        let sid = params["sessionId"].as_str().unwrap_or_default().to_owned();
        let stop = params["stopReason"].as_str().unwrap_or("error").to_owned();
        self.live_entry(&sid, |session| session.turn = Turn::Ended(stop));
    }

    fn response(&self, id: &Value, message: &Value) {
        let Some(key) = id.as_str() else { return };
        let waiting = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(key);
        match waiting {
            Some(Waiting::Call(reply)) => {
                let result = match message.get("error") {
                    Some(error) => Err(rpc_error(error)),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = reply.send(result);
            }
            Some(Waiting::Prompt(sid)) => {
                let stop = message["result"]["stopReason"]
                    .as_str()
                    .unwrap_or("error")
                    .to_owned();
                self.live_entry(&sid, |session| session.turn = Turn::Ended(stop));
            }
            Some(Waiting::Abandoned) => {
                if let Some(sid) = message["result"]["sessionId"].as_str() {
                    // The close goes unregistered. Its reply finds no waiter and is dropped.
                    let id = format!("{}-{}", self.prefix, self.next.fetch_add(1, Ordering::SeqCst));
                    let _ = self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "session/close", "params": {"sessionId": sid}}));
                }
            }
            None => {}
        }
    }

    fn live_entry(&self, sid: &str, change: impl FnOnce(&mut Session)) {
        let mut live = self.live.lock().unwrap_or_else(|poison| poison.into_inner());
        change(live.entry(sid.to_owned()).or_default());
    }
}

/// The loop that routes every line from the daemon. When the socket ends, every call waiting
/// on a reply fails, and the client reconnects on its next use.
fn read_loop(link: &Link, stream: UnixStream) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if let Ok(message) = serde_json::from_str::<Value>(&line) {
            link.route(message);
        }
    }
    link.open.store(false, Ordering::SeqCst);
    let waiting: Vec<Waiting> = link
        .pending
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .drain()
        .map(|(_, waiting)| waiting)
        .collect();
    for waiting in waiting {
        if let Waiting::Call(reply) = waiting {
            let _ = reply.send(Err("ompsdk connection closed".to_owned()));
        }
    }
}

/// Read the JSON-RPC message and its underlying agent detail, when present.
fn rpc_error(error: &Value) -> String {
    let message = error["message"].as_str().unwrap_or("ompsdk error");
    match error["data"]["details"].as_str() {
        Some(details) => format!("{message}: {details}"),
        None => message.to_owned(),
    }
}

/// Choose the option a permission request grants: `allow_always`, `allow_once`, then the first.
fn permission_option(params: &Value) -> String {
    let options = params["options"].as_array().cloned().unwrap_or_default();
    let kind = |want: &str| {
        options
            .iter()
            .find(|option| option["kind"].as_str() == Some(want))
    };
    kind("allow_always")
        .or_else(|| kind("allow_once"))
        .or_else(|| options.first())
        .and_then(|option| option["optionId"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// Whether a schema is the one-field boolean confirm that `acp-agent.ts` writes.
fn is_confirm(schema: &Value) -> bool {
    let Some(properties) = schema["properties"].as_object() else {
        return false;
    };
    properties.len() == 1 && properties["value"]["type"] == "boolean"
}

/// A distinct id per connection, so ids from a reconnect never match an old reply.
fn boot_id() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    nanos ^ std::process::id()
}

/// The model value and thinking level to set for a launch selector.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub model: String,
    pub thinking: Option<String>,
}

/// Resolve a selector to the `model` config value the session takes. `@role[:level]` reads
/// `modelRoles` from omp's config. `provider/id[:level]` takes the id as it is. A level the
/// selector names wins, then the explicit `thinking`, then a role's own level. The model must
/// be one of the session's `model` options.
pub fn resolve_model(selector: &str, thinking: Option<&str>, options: &Value) -> Result<Resolved, String> {
    let (base, named) = split_level(selector);
    let (model, role_level) = match base.strip_prefix('@') {
        Some(role) => {
            let configured = role_selector(role)?;
            let (model, level) = split_level(&configured);
            (model.to_owned(), level.map(str::to_owned))
        }
        None if base.contains('/') => (base.to_owned(), None),
        None => return Err("ompsdk engine takes provider/id or @role".to_owned()),
    };
    if !option_values(options, "model").contains(&model) {
        return Err(format!("unknown model {model}"));
    }
    let thinking = named
        .map(str::to_owned)
        .or_else(|| thinking.map(str::to_owned))
        .or(role_level);
    Ok(Resolved { model, thinking })
}

/// Split a trailing `:<level>` off a selector when the suffix is a thinking level.
fn split_level(selector: &str) -> (&str, Option<&str>) {
    match selector.rsplit_once(':') {
        Some((base, level)) if THINKING_LEVELS.contains(&level) => (base, Some(level)),
        _ => (selector, None),
    }
}

/// The values one config option offers.
fn option_values(options: &Value, id: &str) -> Vec<String> {
    options
        .as_array()
        .into_iter()
        .flatten()
        .find(|option| option["id"].as_str() == Some(id))
        .and_then(|option| option["options"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|choice| choice["value"].as_str().map(str::to_owned))
        .collect()
}

/// The selector a model role maps to in omp's config.
fn role_selector(role: &str) -> Result<String, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let config = std::fs::read_to_string(PathBuf::from(home).join(".omp/agent/config.yml"))
        .map_err(|error| format!("could not read omp config: {error}"))?;
    role_in(&config, role).ok_or_else(|| format!("unknown model role @{role}"))
}

/// The value of `role` under the `modelRoles:` block of an omp config.
fn role_in(config: &str, role: &str) -> Option<String> {
    let mut lines = config.lines();
    lines.find(|line| line.trim_end() == "modelRoles:")?;
    for line in lines {
        if !line.starts_with("  ") {
            break;
        }
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        if key.trim() == role {
            return Some(value.trim().trim_matches(|c| c == '"' || c == '\'').to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Value {
        json!([{
            "id": "model",
            "options": [{"value": "anthropic/claude-smol"}, {"value": "openai/gpt-x"}],
        }])
    }

    #[test]
    fn role_value_keeps_its_own_level() {
        let config = "modelRoles:\n  smol: anthropic/claude-smol:low\n  plan: openai/gpt-x\nother: 1\n";
        let configured = role_in(config, "smol").unwrap();
        assert_eq!(configured, "anthropic/claude-smol:low");
        assert_eq!(split_level(&configured), ("anthropic/claude-smol", Some("low")));
        assert_eq!(split_level("anthropic/claude-smol"), ("anthropic/claude-smol", None));
    }

    #[test]
    fn provider_id_passes_with_thinking_precedence() {
        let resolved = resolve_model("openai/gpt-x:high", Some("low"), &options()).unwrap();
        assert_eq!(resolved, Resolved { model: "openai/gpt-x".into(), thinking: Some("high".into()) });
        let resolved = resolve_model("openai/gpt-x", Some("low"), &options()).unwrap();
        assert_eq!(resolved.thinking.as_deref(), Some("low"));
        let resolved = resolve_model("openai/gpt-x", None, &options()).unwrap();
        assert_eq!(resolved.thinking, None);
    }

    #[test]
    fn refuses_bare_names_and_unknown_models() {
        assert_eq!(
            resolve_model("gpt-x", None, &options()).unwrap_err(),
            "ompsdk engine takes provider/id or @role"
        );
        assert_eq!(
            resolve_model("openai/missing", None, &options()).unwrap_err(),
            "unknown model openai/missing"
        );
    }

    #[test]
    fn reads_role_values_from_the_model_roles_block() {
        let config = "modelRoles:\n  smol: anthropic/claude-smol:low\n  plan: openai/gpt-x\nother: 1\n";
        assert_eq!(role_in(config, "plan").as_deref(), Some("openai/gpt-x"));
        assert_eq!(role_in(config, "missing"), None);
        assert_eq!(role_in("other: 1\n", "smol"), None);
    }

    #[test]
    fn confirm_schema_is_exactly_one_boolean() {
        assert!(is_confirm(&json!({"properties": {"value": {"type": "boolean"}}})));
        assert!(!is_confirm(&json!({"properties": {"value": {"type": "string"}}})));
        assert!(!is_confirm(&json!({"properties": {"value": {"type": "boolean"}, "q0": {}}})));
    }

    #[test]
    fn late_session_new_reply_closes_the_abandoned_session() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let link = Link {
            writer: Mutex::new(ours),
            prefix: "t".to_owned(),
            next: AtomicU64::new(0),
            open: AtomicBool::new(true),
            pending: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
        };
        link.pending.lock().unwrap().insert("t-9".to_owned(), Waiting::Abandoned);
        link.route(json!({"jsonrpc": "2.0", "id": "t-9", "result": {"sessionId": "sid-late"}}));
        let mut line = String::new();
        BufReader::new(theirs).read_line(&mut line).unwrap();
        let sent: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(sent["method"], "session/close");
        assert_eq!(sent["params"]["sessionId"], "sid-late");
        assert!(link.pending.lock().unwrap().is_empty());
    }
}
