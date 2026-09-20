//! `mochic`: sends commands to the running Mochi daemon.
//!
//! One subcommand per protocol command, with the same argument shapes the
//! hotkey file already uses. Anything that fails prints one line to stderr and
//! exits non-zero, so a binding that stops working says so instead of failing
//! in silence.
//!
//! The grammar itself lives in [`mochi_client::cli`], because the hotkey daemon
//! binds keys to the same words this binary takes and the two must not drift.

mod process;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use mochi_client::{
    Command, Error, Response,
    cli::{Cli, Cmd},
};

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mochic: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    // Three subcommands do not travel over the pipe.
    match cli.command {
        Cmd::Start {
            ref config,
            ref hotkeys,
            no_hotkeys,
            dry_run,
        } => return start(config.as_deref(), hotkeys.as_deref(), no_hotkeys, dry_run),
        Cmd::Quickstart => return quickstart(),
        Cmd::Schema => {
            println!("{}", mochi_core::config::json_schema());
            return Ok(());
        }
        Cmd::Subscribe { ref name } => return subscribe(name),
        _ => {}
    }

    let raw_hotkeys = matches!(cli.command, Cmd::Hotkeys { json: true });
    let Some(command) = cli.command.to_command() else {
        unreachable!("the subcommands without a protocol command returned above")
    };
    let response = send(&command)?;

    // A response of the wrong kind has not answered the command: an `ok` to a
    // `query` would print nothing at all and still exit 0, which on a terminal
    // and in a script looks exactly like a value that happens to be empty.
    let (wanted, given) = (answer_kind(&command), response_kind(&response));
    if given != wanted && given != "error" {
        bail!(
            "mochi answered `{given}` to `{}`, which has to be answered with a `{wanted}` response",
            command.name()
        );
    }

    match response {
        Response::Ok => {}
        Response::State { state } => println!("{}", serde_json::to_string_pretty(&state)?),
        Response::Query { answer } => println!("{}", scalar(&answer)),
        Response::Hotkeys { hotkeys } => {
            if raw_hotkeys {
                println!("{}", serde_json::to_string_pretty(&hotkeys)?);
            } else {
                print!("{}", hotkey_table(&hotkeys));
            }
        }
        Response::Error { message } => bail!(message),
    }

    Ok(())
}

/// The kind of response a command has to be answered with.
fn answer_kind(command: &Command) -> &'static str {
    match command {
        Command::State => "state",
        Command::Query { .. } => "query",
        Command::Hotkeys => "hotkeys",
        _ => "ok",
    }
}

/// The kind a response is, spelled the way the wire spells it.
fn response_kind(response: &Response) -> &'static str {
    match response {
        Response::Ok => "ok",
        Response::State { .. } => "state",
        Response::Query { .. } => "query",
        Response::Hotkeys { .. } => "hotkeys",
        Response::Error { .. } => "error",
    }
}

/// Sends a command, turning "no daemon" into a message a human can act on.
fn send(command: &Command) -> Result<Response> {
    match mochi_client::send(command) {
        Ok(response) => Ok(response),
        Err(Error::NotRunning) => bail!("mochi is not running. Start it with `mochic start`."),
        Err(Error::Daemon(message)) => bail!(message),
        Err(e) => Err(e).context("could not talk to mochi"),
    }
}

/// Prints a JSON scalar without its quotes, so shells can use the value directly.
fn scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Lays the bindings out as `alt + shift + h    move left`, one per line,
/// followed by any line of the file that did not parse.
///
/// The document belongs to the daemon, so anything that is not a list of
/// bindings is printed as JSON instead of being silently dropped.
fn hotkey_table(hotkeys: &serde_json::Value) -> String {
    let Some(rows) = binding_rows(hotkeys) else {
        let json = serde_json::to_string_pretty(hotkeys).unwrap_or_else(|_| hotkeys.to_string());
        return format!("{json}\n");
    };

    let mut table = if rows.is_empty() {
        "no hotkeys are bound\n".to_owned()
    } else {
        let width = rows
            .iter()
            .map(|(keys, _)| keys.chars().count())
            .max()
            .unwrap_or(0);
        let mut table = String::new();
        for (keys, command) in &rows {
            table.push_str(keys);
            table.push_str(&" ".repeat(width - keys.chars().count() + 4));
            table.push_str(command);
            table.push('\n');
        }
        table
    };

    // A suspended set of bindings looks exactly like a working one in a table,
    // so the state that explains it has to be on the screen as well.
    match hotkeys.get("gate").and_then(serde_json::Value::as_str) {
        Some("game-mode") => {
            table.push_str(
                "\ngame mode: every binding above is suspended except toggle-game-mode\n",
            );
        }
        Some("off") => {
            table.push_str(
                "\nhotkeys are off, turn them back on with `mochic set-hotkeys enable`\n",
            );
        }
        _ => {}
    }

    let errors = hotkeys
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !errors.is_empty() {
        table.push_str("\nlines that did not parse:\n");
        for error in errors {
            let text = error
                .as_str()
                .map_or_else(|| error.to_string(), str::to_owned);
            table.push_str("  ");
            table.push_str(&text);
            table.push('\n');
        }
    }

    table
}

/// Pulls `keys` and `command` out of every entry of the bindings document, or
/// `None` when it is shaped some other way.
///
/// The daemon sends an object with the bindings under `bindings`; a bare list
/// is accepted too, so a third-party tool speaking the protocol can send the
/// short form.
fn binding_rows(hotkeys: &serde_json::Value) -> Option<Vec<(String, String)>> {
    hotkeys
        .get("bindings")
        .unwrap_or(hotkeys)
        .as_array()?
        .iter()
        .map(|entry| {
            let keys = entry.get("keys")?.as_str()?.to_owned();
            let command = match entry.get("command")? {
                serde_json::Value::String(text) => text.clone(),
                // A binding may also carry its command already split into words.
                serde_json::Value::Array(words) => words
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => return None,
            };
            Some((keys, command))
        })
        .collect()
}

/// Starts the daemon, handing it the switches it would have taken directly.
///
/// Every one of these has to be passed through rather than left to the daemon's
/// own defaults: `mochic start` is what an autostart entry runs, and a user
/// whose hotkey file is not in the usual place would otherwise have no way to
/// say so from there.
fn start(
    config: Option<&std::path::Path>,
    hotkeys: Option<&std::path::Path>,
    no_hotkeys: bool,
    dry_run: bool,
) -> Result<()> {
    if mochi_client::is_running() {
        println!("mochi is already running");
    } else {
        let mut args = Vec::new();
        if dry_run {
            args.push("--dry-run".to_owned());
        }
        if let Some(path) = config {
            args.push("--config".to_owned());
            args.push(path.display().to_string());
        }
        if let Some(path) = hotkeys {
            args.push("--hotkeys".to_owned());
            args.push(path.display().to_string());
        }
        if no_hotkeys {
            args.push("--no-hotkeys".to_owned());
        }
        process::start_daemon(&args)?;
    }
    Ok(())
}

fn quickstart() -> Result<()> {
    let profile = std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .context("USERPROFILE is not set")?;

    write_unless_present(&profile.join("mochi.json"), DEFAULT_CONFIG)?;

    // The hotkey directory may already hold a file under the name a standalone
    // hotkey daemon uses, which is one of the names Mochi reads. Writing the
    // other one next to it would shadow a working set of bindings with the
    // defaults, so either name counts as "there is already a file here".
    let keys = profile.join(".config").join("mochi");
    if keys.join("whkdrc").exists() {
        println!(
            "{} already exists, leaving it alone",
            keys.join("whkdrc").display()
        );
    } else {
        write_unless_present(&keys.join("hotkeys"), mochi_hotkey::DEFAULT)?;
    }
    Ok(())
}

/// Writes a starting file, and says either way which one it meant.
fn write_unless_present(path: &std::path::Path, contents: &str) -> Result<()> {
    if path.exists() {
        println!("{} already exists, leaving it alone", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(path, contents)
        .with_context(|| format!("could not write {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The stub configuration `quickstart` writes.
///
/// Kept minimal on purpose: `mochi-core` owns the schema and fills this in.
const DEFAULT_CONFIG: &str = r#"{
  "$schema": "https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json",
  "window_hiding_behaviour": "Cloak",
  "default_workspace_padding": 10,
  "default_container_padding": 10,
  "monitors": []
}
"#;

/// How often a subscription checks that the daemon is still there.
///
/// The read blocks inside the client crate, so a daemon that has stopped cannot
/// be noticed from the reading side; the command pipe is polled instead.
const SUBSCRIBE_POLL: std::time::Duration = std::time::Duration::from_secs(2);

fn subscribe(name: &str) -> Result<()> {
    // Checked here so a name the pipe namespace cannot hold is reported as the
    // argument it is. Left to `mochi_client::subscribe` it arrives wrapped as a
    // pipe I/O failure, though no pipe was ever touched, and anyhow then prints
    // the same sentence a second time as the cause.
    if let Err(e) = mochi_client::validate_pipe_name(name) {
        match e {
            Error::Io(io) => bail!("{io}"),
            other => bail!("{other}"),
        }
    }

    let subscription = match mochi_client::subscribe(name) {
        Ok(s) => s,
        Err(Error::NotRunning) => {
            bail!("mochi is not running. Start it with `mochic start`.")
        }
        // `{e}` and not the source chain: every `Error::Io` carries the same
        // sentence as its cause and would otherwise be printed twice.
        Err(e) => bail!("could not subscribe: {e}"),
    };
    eprintln!(
        "subscribed on {}{name}, press Ctrl-C to stop",
        mochi_client::PIPE_PREFIX
    );

    // `next_notification` has no path that ends the iterator: when the daemon
    // goes away it recycles the pipe and blocks again, waiting for a daemon
    // that may never come back. Reading on its own thread leaves this one free
    // to notice that the daemon has gone and say so.
    let (lines, notifications) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for notification in subscription {
            if lines.send(notification).is_err() {
                break;
            }
        }
    });

    loop {
        match notifications.recv_timeout(SUBSCRIBE_POLL) {
            Ok(Ok(n)) => println!("{}", serde_json::to_string(&n)?),
            Ok(Err(e)) => bail!("the subscriber pipe failed: {e}"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !mochi_client::is_running() {
                    eprintln!("mochi has stopped, ending the subscription");
                    return Ok(());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("the subscriber pipe closed, ending the subscription");
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_scalars_print_without_json_quotes() {
        assert_eq!(scalar(&serde_json::json!("0.1.0")), "0.1.0");
        assert_eq!(scalar(&serde_json::json!(2)), "2");
        assert_eq!(scalar(&serde_json::json!(true)), "true");
    }

    #[test]
    fn the_hotkey_table_lines_the_commands_up() {
        let table = hotkey_table(&serde_json::json!([
            {"keys": "alt + shift + h", "command": "move left"},
            {"keys": "alt + h", "command": ["focus", "left"]},
        ]));
        assert_eq!(
            table,
            "alt + shift + h    move left\nalt + h            focus left\n"
        );
    }

    #[test]
    fn an_empty_binding_list_says_so() {
        assert_eq!(
            hotkey_table(&serde_json::json!([])),
            "no hotkeys are bound\n"
        );
    }

    #[test]
    fn an_unfamiliar_bindings_document_is_printed_as_json() {
        let table = hotkey_table(&serde_json::json!({"enabled": false}));
        assert!(table.contains("\"enabled\""), "{table}");
    }

    #[test]
    fn the_daemons_own_document_prints_its_bindings() {
        let table = hotkey_table(&serde_json::json!({
            "path": "C:\\keys",
            "gate": "all",
            "bindings": [{"keys": "alt + h", "command": "focus left"}],
            "errors": [],
        }));
        assert_eq!(table, "alt + h    focus left\n");
    }

    #[test]
    fn a_suspended_or_broken_file_says_so_under_the_table() {
        let table = hotkey_table(&serde_json::json!({
            "gate": "game-mode",
            "bindings": [{"keys": "alt + shift + g", "command": "toggle-game-mode"}],
            "errors": ["line 4: nosuchkey is not a key name"],
        }));
        assert!(table.contains("game mode:"), "{table}");
        assert!(table.contains("line 4: nosuchkey"), "{table}");

        let off = hotkey_table(&serde_json::json!({"gate": "off", "bindings": []}));
        assert!(off.contains("no hotkeys are bound"), "{off}");
        assert!(off.contains("set-hotkeys enable"), "{off}");
    }

    /// The schema committed at the repository root is what the CLI prints.
    ///
    /// Configuration files point their `$schema` at that file, so a change to
    /// the config types that is not written back would leave every editor
    /// validating against a stale schema.
    #[test]
    fn the_committed_schema_is_current() {
        let committed = include_str!("../../../schema.json");
        assert_eq!(
            committed.trim(),
            mochi_core::config::json_schema().trim(),
            "schema.json is out of date, regenerate it with `mochic schema > schema.json`"
        );
    }

    /// A daemon answering the wrong kind has not run the command.
    ///
    /// `ok` to a `query` prints nothing and exits 0, so without this check a
    /// query that produced no answer is indistinguishable from a success.
    #[test]
    fn an_answer_of_the_wrong_kind_is_not_an_answer() {
        let query = Command::Query {
            target: mochi_client::QueryTarget::Version,
        };
        assert_eq!(answer_kind(&query), "query");
        assert_eq!(answer_kind(&Command::State), "state");
        assert_eq!(answer_kind(&Command::Hotkeys), "hotkeys");
        assert_eq!(answer_kind(&Command::Retile), "ok");

        assert_eq!(response_kind(&Response::Ok), "ok");
        assert_ne!(response_kind(&Response::Ok), answer_kind(&query));
        assert_eq!(
            response_kind(&Response::Query {
                answer: serde_json::json!("0.1.3")
            }),
            answer_kind(&query)
        );
        assert_eq!(
            response_kind(&Response::State {
                state: serde_json::json!({})
            }),
            answer_kind(&Command::State)
        );
    }

    #[test]
    fn the_quickstart_stub_is_valid_json() {
        let value: serde_json::Value = serde_json::from_str(DEFAULT_CONFIG).unwrap();
        assert!(value.is_object());
    }
}
