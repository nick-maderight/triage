//! CLI entry. Owner: server worker.
//!
//! triage serve [--listen 127.0.0.1:7340] [--db PATH] [--session NAME] [--model SELECTOR] [--thinking LEVEL] [--allow-host HOST]... [--no-notify]
//! triage emit [FILE|-]           POST one CloudEvent (JSON file or stdin) to /v1/ingest
//! triage finding --source URI --title T [--situation ..] [--task ..] [--action ..] [--result ..]
//!                [--option LABEL]... [--ref PATH]... [--org-id ID] [--cwd DIR] [--priority N] [--id ID]
//!                Convenience wrapper that builds a triage.finding CloudEvent and posts it.
//! triage queue                   print the queue as JSON (for scripts and the status line)
//! triage status-line             print `✋N` style count for the tmux status bar, empty when zero
//! Default DB: $XDG_DATA_HOME/triage/triage.db or ~/.local/share/triage/triage.db.
//! Default URL for client commands: http://127.0.0.1:7340, override with TRIAGE_URL.

mod cards;
mod client;
mod db;
mod events;
mod omp;
mod push;
mod server;
mod tmux;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "triage")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Serve {
        #[arg(long, default_value = "127.0.0.1:7340")]
        listen: std::net::SocketAddr,
        #[arg(long)]
        db: Option<PathBuf>,
        /// tmux session new windows open in [default: triage]
        #[arg(long)]
        session: Option<String>,
        /// omp model selector for launched sessions [default: omp's own]
        #[arg(long)]
        model: Option<String>,
        /// omp thinking level for launched sessions [default: omp's own]
        #[arg(long)]
        thinking: Option<String>,
        /// Host name the inbox accepts besides loopback, such as a tailnet name; repeatable
        #[arg(long = "allow-host", value_name = "HOST")]
        allow_hosts: Vec<String>,
        #[arg(long)]
        no_notify: bool,
    },
    Emit {
        #[arg(default_value = "-")]
        file: PathBuf,
    },
    Finding {
        #[arg(long)]
        source: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        situation: String,
        #[arg(long, default_value = "")]
        task: String,
        #[arg(long, default_value = "")]
        action: String,
        #[arg(long, default_value = "")]
        result: String,
        #[arg(long = "option")]
        options: Vec<String>,
        #[arg(long = "ref")]
        refs: Vec<String>,
        #[arg(long)]
        org_id: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long, default_value_t = 2)]
        priority: u8,
        #[arg(long)]
        id: Option<String>,
    },
    Queue,
    StatusLine,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("triage: {error}");
        std::process::exit(1);
    }
}

#[tokio::main]
async fn run() -> Result<(), String> {
    match Cli::parse().command {
        Commands::Serve { listen, db, session, model, thinking, allow_hosts, no_notify } => {
            // Validate before opening the DB so a bad flag exits 1 before anything is created.
            cards::launch_flags(model.as_deref(), thinking.as_deref())?;
            let path = match db {
                Some(path) => path,
                None => default_db_path()?,
            };
            if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let db = db::Db::open(&path).map_err(|error| error.to_string())?;
            let tracker = cards::Tracker::from_db(&db).map_err(|error| error.to_string())?;
            let (bus, _) = tokio::sync::broadcast::channel(64);
            let push = push::Push::load(&db, &allow_hosts)?;
            let state = std::sync::Arc::new(server::AppState {
                db,
                bus,
                tracker: std::sync::Mutex::new(tracker),
                launch: cards::Launch { session, model, thinking },
                push,
                allowed_hosts: allow_hosts,
            });
            server::serve(state, listen, !no_notify)
                .await
                .map_err(|error| error.to_string())
        }
        Commands::Emit { file } => {
            let mut input = String::new();
            if file == PathBuf::from("-") {
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).map_err(|error| error.to_string())?;
            } else {
                input = std::fs::read_to_string(file).map_err(|error| error.to_string())?;
            }
            let event: events::CloudEvent = serde_json::from_str(&input).map_err(|error| error.to_string())?;
            print_json(client::ingest(&event)?)
        }
        Commands::Finding {
            source,
            title,
            situation,
            task,
            action,
            result,
            options,
            refs,
            org_id,
            cwd,
            priority,
            id,
        } => {
            if !(1..=5).contains(&priority) {
                return Err("priority must be between 1 and 5".into());
            }
            let options = if options.is_empty() { vec!["Do as recommended".to_string()] } else { options };
            let options: Vec<_> = options
                .into_iter()
                .enumerate()
                .map(|(index, label)| serde_json::json!({"label": label, "recommended": index == 0}))
                .collect();
            let cwd = cwd
                .map(|path| path.to_string_lossy().into_owned())
                .or_else(|| std::env::current_dir().ok().map(|path| path.to_string_lossy().into_owned()))
                .unwrap_or_default();
            let event = events::CloudEvent {
                specversion: "1.0".into(),
                id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                source,
                r#type: events::T_FINDING.into(),
                subject: None,
                time: None,
                priority: Some(priority),
                data: serde_json::json!({
                    "title": title,
                    "star": {"situation": situation, "task": task, "action": action, "result": result},
                    "options": options,
                    "refs": refs,
                    "org_id": org_id,
                    "cwd": cwd
                }),
            };
            print_json(client::ingest(&event)?)
        }
        Commands::Queue => print_json(client::queue()?),
        Commands::StatusLine => {
            match client::queue() {
                Ok(reply) => {
                    if let Some(count) = reply.get("cards").and_then(serde_json::Value::as_array).map(Vec::len).filter(|count| *count > 0) {
                        println!("✋{count}");
                    }
                }
                Err(_) => println!("✋?"),
            }
            Ok(())
        }
    }
}

fn default_db_path() -> Result<PathBuf, String> {
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME");
    let home = std::env::var_os("HOME");
    default_db_path_from(xdg_data_home.as_deref(), home.as_deref())
}

fn default_db_path_from(
    xdg_data_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, String> {
    if let Some(data_home) = xdg_data_home
        .map(std::path::Path::new)
        .filter(|path| path.is_absolute())
    {
        return Ok(data_home.join("triage/triage.db"));
    }
    let home = home
        .map(std::path::Path::new)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "XDG_DATA_HOME or HOME must be an absolute path".to_string())?;
    Ok(home.join(".local/share/triage/triage.db"))
}

fn print_json(value: serde_json::Value) -> Result<(), String> {
    println!("{}", serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::default_db_path_from;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    #[test]
    fn default_db_path_uses_only_absolute_environment_paths() {
        let home = Some(std::ffi::OsStr::new("/home/triage-test"));
        let expected = PathBuf::from("/home/triage-test/.local/share/triage/triage.db");

        assert_eq!(
            default_db_path_from(Some(OsStr::new("")), home),
            Ok(expected.clone())
        );
        assert_eq!(
            default_db_path_from(Some(OsStr::new("relative")), home),
            Ok(expected)
        );
        assert!(default_db_path_from(None, Some(OsStr::new("relative-home"))).is_err());
    }
}
