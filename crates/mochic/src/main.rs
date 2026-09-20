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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use mochi_client::{
    Command, Error, Response,
    cli::{Cli, Cmd},
};
use mochi_core::config::Config;
use mochi_core::rules::{
    MatchingRule, RuleSets, load_app_specific_configuration_reporting_unknown_keys,
};

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    // `check` answers with an exit code rather than a message, because a file
    // that does not check is not this program failing, it is the answer. It is
    // also the one command that never touches the pipe, so it still works when
    // the desktop is broken, which is the only moment anybody runs it. The
    // shared grammar refuses to bind it to a key, along with the other
    // commands that never reach the daemon.
    if let Cmd::Check { ref path } = cli.command {
        return check(path.as_deref());
    }

    match run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mochic: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
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

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

/// Everything one file's check produced, in the order it should be printed.
#[derive(Debug, Default)]
struct Findings {
    /// Reasons the file cannot do what it says. Each may run to several lines.
    errors: Vec<String>,
    /// Things the daemon would carry on past, said out loud anyway.
    warnings: Vec<String>,
    /// The plain facts of what was read, one line each.
    notes: Vec<String>,
    /// The raw `app_specific_configuration_path`, exactly as the file spells
    /// it, so the caller can expand and follow it.
    app_path: Option<String>,
    /// The one sentence a user who has just broken their desktop needs: which
    /// line of which file to fix. `None` while nothing is wrong.
    headline: Option<String>,
}

/// Reads the configuration, says what is wrong with it, and answers with the
/// exit code the shell should see.
fn check(explicit: Option<&Path>) -> std::process::ExitCode {
    let (path, source) = match resolve_config_path(explicit) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("mochic: {e:#}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let label = path.display().to_string();
    println!("checking {label}\n  ({source})");

    let text = match read_config(&path) {
        Ok(text) => text,
        Err(reason) => {
            println!("\n{reason}");
            println!("\nnot usable: nothing was checked");
            return std::process::ExitCode::FAILURE;
        }
    };

    // A community rule file handed straight to `check` is checked as one.
    // Both files are JSON objects and `mochi.json` ignores what it does not
    // know, so an `applications.json` parses as a configuration of nothing at
    // all: without this it would come back "usable" with one warning per
    // application in it, which is several hundred lines of nonsense about a
    // file that is perfectly fine.
    if looks_like_app_rules(&text) {
        println!("  this is an application rules file, not a mochi.json");
        return report(&label, check_app_rules_text(&label, &text));
    }

    let mut found = check_config_text(&label, &text);

    // The community rule file is not Mochi's file: the daemon logs what is
    // wrong with it and carries on, so everything it produces is a warning.
    if let Some(raw) = found.app_path.clone() {
        let app_path = PathBuf::from(expand_env(&raw));
        found.notes.push(format!(
            "application rules: {} (app_specific_configuration_path)",
            app_path.display()
        ));
        match read_config(&app_path) {
            Ok(app_text) => {
                let app = check_app_rules_text(&app_path.display().to_string(), &app_text);
                found.notes.extend(app.notes);
                found.warnings.extend(app.warnings);
                found.warnings.extend(app.errors);
            }
            Err(reason) => found.warnings.push(format!(
                "{reason}\n    The daemon logs this and carries on with the rules in {label} alone."
            )),
        }
    }

    report(&label, found)
}

/// Prints the findings and answers with the exit code they call for.
///
/// Notes, then what is wrong, then what is merely worth saying, then one line
/// of verdict. A warning never changes the exit code: a community rule file
/// with a mistyped list name is still a desktop that starts.
fn report(label: &str, found: Findings) -> std::process::ExitCode {
    for note in &found.notes {
        println!("  {note}");
    }
    if !found.errors.is_empty() {
        println!();
        for error in &found.errors {
            println!("{error}");
        }
    }
    if !found.warnings.is_empty() {
        println!();
        for warning in &found.warnings {
            println!("warning: {warning}");
        }
    }

    println!();
    if found.errors.is_empty() {
        match found.warnings.len() {
            0 => println!("ok: {label} is usable"),
            1 => println!("ok: {label} is usable, with 1 warning"),
            n => println!("ok: {label} is usable, with {n} warnings"),
        }
        std::process::ExitCode::SUCCESS
    } else {
        // One sentence, and it already names the file: the line somebody reads
        // when the desktop in front of them has stopped tiling.
        println!(
            "not usable: {}",
            found
                .headline
                .as_deref()
                .unwrap_or("the configuration cannot be used as it stands")
        );
        std::process::ExitCode::FAILURE
    }
}

/// `true` when the text is a community `applications.json` rather than a
/// `mochi.json`.
///
/// The test is what each parser makes of it, not what the file is called: a
/// configuration that produced no setting at all and no rule, out of a document
/// the application rule reader turned into rules, is an application rule file.
/// Nothing else can be true of both at once.
fn looks_like_app_rules(text: &str) -> bool {
    Config::from_json(text).is_ok_and(|config| config == Config::default())
        && load_app_specific_configuration_reporting_unknown_keys(text)
            .is_ok_and(|(sets, _)| !sets.is_empty())
}

/// Resolves the configuration path the way the daemon resolves it.
///
/// The precedence is the one in `crates/mochi/src/config.rs::resolve_path`: the
/// `--config` argument, then `MOCHI_CONFIG`, then `%USERPROFILE%\mochi.json`.
/// `mochic` does not depend on the `mochi` crate, so this is a second copy of
/// one rule and the two have to change together. A `check` that reads a
/// different file than the daemon would is worse than no `check` at all: it
/// would report a desktop fixed that is still broken.
fn resolve_config_path(explicit: Option<&Path>) -> Result<(PathBuf, String)> {
    if let Some(path) = explicit {
        return Ok((path.to_path_buf(), "given on the command line".to_owned()));
    }
    if let Some(from_env) = std::env::var_os("MOCHI_CONFIG").filter(|v| !v.is_empty()) {
        return Ok((
            PathBuf::from(from_env),
            "the file the daemon would load, named by MOCHI_CONFIG".to_owned(),
        ));
    }
    let profile = std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .context("USERPROFILE is not set, so the configuration path cannot be resolved")?;
    Ok((
        profile.join("mochi.json"),
        "the file the daemon would load: no path given, MOCHI_CONFIG is not set".to_owned(),
    ))
}

/// Reads a file, naming the two mistakes an ordinary `read_to_string` reports
/// as an error number: a path that is a directory, and a path to nothing.
fn read_config(path: &Path) -> std::result::Result<String, String> {
    if path.is_dir() {
        return Err(format!(
            "{} is a directory, not a file. Name the file inside it.",
            path.display()
        ));
    }
    if !path.exists() {
        return Err(format!(
            "there is no file at {}. The daemon would start on its built-in defaults; \
             `mochic quickstart` writes a starting file.",
            path.display()
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// Checks the text of a `mochi.json`.
///
/// Takes the text rather than a path so the whole judgement can be tested
/// without touching a disk.
fn check_config_text(label: &str, text: &str) -> Findings {
    let mut found = Findings::default();

    let config = match Config::from_json(text) {
        Ok(config) => config,
        Err(flattened) => {
            let block = parse_failure(label, text, &flattened.to_string());
            // The first line is already `file:line:column: what went wrong`,
            // which is the whole sentence somebody staring at a broken desktop
            // needs. The rest of the block is the source line under a caret.
            found.headline = block.lines().next().map(str::to_owned);
            found.errors.push(block);
            return found;
        }
    };

    found.app_path = config
        .app_specific_configuration_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());

    let sets = config.rule_sets();
    let lists = rule_lists(&sets)
        .iter()
        .filter(|(_, _, rules)| !rules.is_empty())
        .count();
    found.notes.push(if sets.is_empty() {
        "parses, no rules".to_owned()
    } else {
        format!(
            "parses, {} {} in {lists} {}",
            sets.len(),
            if sets.len() == 1 { "rule" } else { "rules" },
            if lists == 1 { "list" } else { "lists" }
        )
    });

    // Every broken rule, not the first: a file usually carries a handful, and
    // finding them one reload at a time is the loop this command exists to end.
    let before = found.errors.len();
    for (key, _, rules) in rule_lists(&sets) {
        found.errors.extend(broken_rules(key, Some(text), rules));
    }
    let broken = found.errors.len() - before;
    if broken == 1 {
        found.headline = Some(format!(
            "1 rule in {label} cannot do what it says, and the daemon would drop it"
        ));
    } else if broken > 1 {
        found.headline = Some(format!(
            "{broken} rules in {label} cannot do what they say, and the daemon would drop them"
        ));
    }

    // A key the parser threw away is the quietest mistake in the file: it
    // parses, it looks applied, and it does nothing at all.
    found
        .warnings
        .extend(inert_keys(text).into_iter().map(|key| {
            let suggestion = nearest_key(&key)
                .map_or_else(String::new, |known| format!(" Did you mean {known:?}?"));
            format!("{label} has a key {key:?} that Mochi does not read. It parses and is then ignored.{suggestion}")
        }));

    found
}

/// Checks the text of a community `applications.json`.
fn check_app_rules_text(label: &str, text: &str) -> Findings {
    let mut found = Findings::default();

    let (sets, unknown) = match load_app_specific_configuration_reporting_unknown_keys(text) {
        Ok(pair) => pair,
        Err(flattened) => {
            let block = parse_failure_json(label, text, &flattened.to_string());
            found.headline = block.lines().next().map(str::to_owned);
            found.errors.push(block);
            return found;
        }
    };

    found.notes.push(format!(
        "{} rules read from the application file",
        sets.len()
    ));
    // A mistyped list name parses into no rules at all, because every list
    // defaults to empty, so nothing else would ever mention it.
    found
        .warnings
        .extend(unknown.into_iter().map(|line| format!("{label}: {line}")));
    for (_, app_key, rules) in rule_lists(&sets) {
        found
            .errors
            .extend(broken_rules(app_key, Some(text), rules));
    }
    if !found.errors.is_empty() {
        let broken = found.errors.len();
        found.headline = Some(format!(
            "{broken} of the {} rules in {label} cannot do what they say",
            sets.len()
        ));
    }

    found
}

/// Every rule list, with the key it is spelled with in `mochi.json` and the key
/// the community `applications.json` spells the same list with.
///
/// This mirrors the nine lists `RuleSets::validate` walks. `own_ignore_rules`
/// is deliberately not among them: `Config::rule_sets` fills it with a copy of
/// `ignore_rules`, and reporting a rule twice for one mistake helps nobody.
fn rule_lists(sets: &RuleSets) -> [(&'static str, &'static str, &Vec<MatchingRule>); 9] {
    [
        ("ignore_rules", "ignore", &sets.ignore_rules),
        ("manage_rules", "manage", &sets.manage_rules),
        (
            "floating_applications",
            "floating",
            &sets.floating_applications,
        ),
        (
            "tray_and_multi_window_applications",
            "tray_and_multi_window",
            &sets.tray_and_multi_window_applications,
        ),
        (
            "object_name_change_applications",
            "object_name_change",
            &sets.object_name_change_applications,
        ),
        (
            "border_overflow_applications",
            "border_overflow",
            &sets.border_overflow_applications,
        ),
        ("layered_whitelist", "layered", &sets.layered_whitelist),
        (
            "transparency_ignore_rules",
            "transparency_ignore",
            &sets.transparency_ignore_rules,
        ),
        (
            "slow_application_identifiers",
            "slow_application",
            &sets.slow_application_identifiers,
        ),
    ]
}

/// One report per rule in `rules` that cannot do what it says.
///
/// The rule is printed as the JSON it was written as, so it can be found by
/// eye, and its line is named when the identifier appears exactly once in the
/// file. An identifier that JSON had to escape, `steamapps\\common`, is not
/// found by this search and simply goes unlocated rather than pointing at the
/// wrong line.
fn broken_rules(list: &str, text: Option<&str>, rules: &[MatchingRule]) -> Vec<String> {
    let mut reports = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        let Err(error) = rule.validate() else {
            continue;
        };
        // A regex compiler's complaint runs to four lines with a caret of its
        // own, so the continuation is indented under the first line rather
        // than falling back to column zero and reading as a second finding.
        let mut report = format!(
            "{list}[{index}]: {}",
            error.to_string().replace('\n', "\n    ")
        );
        if let Ok(json) = serde_json::to_string(rule) {
            report.push_str(&format!("\n    {json}"));
        }
        let located = text.zip(
            rule.conditions()
                .iter()
                .map(|condition| condition.id.as_str())
                .find(|id| !id.is_empty()),
        );
        if let Some(line) = located.and_then(|(text, id)| unique_line(text, id)) {
            report.push_str(&format!("\n    written on line {line}"));
        }
        reports.push(report);
    }
    reports
}

/// The top level keys the parser read and then threw away.
///
/// Derived by asking the real parser rather than from a list written down
/// here: a list of keys in this file goes stale the day a key is wired up, and
/// it would also miss that `layered_applications` is an accepted alias that
/// never appears in the schema. Every field of `Config` is an `Option`, so a
/// key the parser reads always moves the result away from the default, and a
/// key that leaves the default untouched is a key nothing was done with.
fn inert_keys(text: &str) -> Vec<String> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let default = Config::default();
    map.into_iter()
        .filter(|(key, value)| {
            // `$schema` and anything else a tool hangs off the file is not
            // Mochi's to read. A `null` cannot be told apart from a key that
            // was thrown away, because every field is already `None`.
            if key.starts_with('$') || value.is_null() {
                return false;
            }
            let mut only = serde_json::Map::new();
            only.insert(key.clone(), value.clone());
            Config::from_json(&serde_json::Value::Object(only).to_string())
                .is_ok_and(|parsed| parsed == default)
        })
        .map(|(key, _)| key)
        .collect()
}

/// The key a misspelling was most likely meant to be.
///
/// The candidates come out of `mochi_core::config::json_schema`, which is
/// generated from the config types, so they cannot drift from what the parser
/// accepts. The match is the longest shared opening, which catches the
/// transpositions and dropped letters a hand written key actually carries.
fn nearest_key(typo: &str) -> Option<String> {
    let schema: serde_json::Value =
        serde_json::from_str(&mochi_core::config::json_schema()).ok()?;
    // A third of the key may differ, at least one character and at most four.
    // Wider than that and `border` starts being offered for `animation`, which
    // is worse than offering nothing: a guess a user follows is a second wrong
    // file to find the fault in.
    let allowed = (typo.len() / 3).clamp(1, 4);
    schema
        .get("properties")?
        .as_object()?
        .keys()
        .map(|known| (edit_distance(known, typo), known.clone()))
        .filter(|(distance, _)| *distance <= allowed)
        .min_by_key(|(distance, known)| (*distance, known.len()))
        .map(|(_, known)| known)
}

/// The number of single character edits between two keys.
///
/// The plain Levenshtein table, over bytes: config keys are ASCII, and a
/// multi-byte key is a key that was never going to be a near miss anyway.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, left) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(!left.eq_ignore_ascii_case(right));
            current[j + 1] = substitute.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// A parse failure of `mochi.json`, with the line, the column and the text.
fn parse_failure(label: &str, text: &str, flattened: &str) -> String {
    // `mochi_core::Error::Json` carries the serde message as a string and has
    // thrown the line and column away, so they are recovered by handing the
    // same text to the same deserialiser `Config::from_json` runs. If that ever
    // disagrees, the flattened message is printed exactly as it came.
    let Err(e) = serde_json::from_str::<Config>(text) else {
        return format!("{label}: {flattened}");
    };
    position_block(label, text, &e)
}

/// The same for a community rule file, which is read as a `serde_json::Value`
/// first and so fails at the same two stages.
fn parse_failure_json(label: &str, text: &str, flattened: &str) -> String {
    let Err(e) = serde_json::from_str::<serde_json::Value>(text) else {
        return format!("{label}: {flattened}");
    };
    position_block(label, text, &e)
}

/// `file:line:column: what went wrong`, then the line itself under a caret.
fn position_block(label: &str, text: &str, e: &serde_json::Error) -> String {
    let (line, column) = (e.line(), e.column());
    let message = e.to_string();
    if line == 0 {
        return format!("{label}: {message}");
    }
    let mut block = format!("{label}:{line}:{column}: {}", without_position(&message));
    if let Some(caret) = caret(text, line, column) {
        block.push('\n');
        block.push_str(&caret);
    }
    block
}

/// Drops serde's own ` at line L column C`, which the prefix already said.
fn without_position(message: &str) -> String {
    message
        .find(" at line ")
        .map_or_else(|| message.to_owned(), |at| message[..at].to_owned())
}

/// The offending line with a caret under the offending column.
fn caret(text: &str, line: usize, column: usize) -> Option<String> {
    let source = text.lines().nth(line.checked_sub(1)?)?;
    let number = line.to_string();
    let gutter = " ".repeat(number.len());
    // serde counts the column in bytes from the start of the line, so the
    // padding is counted in bytes too. A tab is copied across rather than
    // replaced, so the caret lands where the terminal put the character.
    let lead: String = source
        .char_indices()
        .take_while(|(at, _)| at + 1 < column)
        .map(|(_, c)| if c == '\t' { '\t' } else { ' ' })
        .collect();
    Some(format!("  {number} | {source}\n  {gutter} | {lead}^"))
}

/// The line `needle` is on, when the file holds it exactly once.
fn unique_line(text: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut hits = text.match_indices(needle);
    let (at, _) = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    Some(text[..at].bytes().filter(|byte| *byte == b'\n').count() + 1)
}

/// Expands the three environment variable spellings a config can carry.
///
/// A copy of `crates/mochi/src/config.rs::expand_env`, for the same reason
/// [`resolve_config_path`] is a copy: `mochic` does not depend on the daemon
/// crate, and a `check` that resolves `app_specific_configuration_path` to a
/// different file than the daemon would is checking the wrong file. The two
/// have to change together.
fn expand_env(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(start) = rest.find(['$', '%']) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];

        let (name, consumed) = if let Some(tail) = rest.strip_prefix('%') {
            match tail.find('%') {
                Some(end) => (&tail[..end], end + 2),
                None => {
                    out.push('%');
                    rest = tail;
                    continue;
                }
            }
        } else {
            let tail = &rest[1..];
            let (tail, marker) = tail
                .strip_prefix("Env:")
                .or_else(|| tail.strip_prefix("env:"))
                .map_or((tail, 1), |t| (t, 5));
            let len = tail
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(tail.len());
            (&tail[..len], marker + len)
        };

        match std::env::var_os(name).filter(|v| !v.is_empty()) {
            Some(value) if !name.is_empty() => out.push_str(&value.to_string_lossy()),
            _ => out.push_str(&rest[..consumed]),
        }
        rest = &rest[consumed..];
    }

    out.push_str(rest);
    out
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

    // --- check ----------------------------------------------------------

    #[test]
    fn a_good_file_is_usable_and_says_what_it_read() {
        let found = check_config_text("mochi.json", DEFAULT_CONFIG);
        assert!(found.errors.is_empty(), "{:?}", found.errors);
        assert!(found.warnings.is_empty(), "{:?}", found.warnings);
        assert!(
            found.notes.iter().any(|n| n.contains("parses")),
            "{found:?}"
        );
    }

    /// The whole point of the command: one line naming the line to fix.
    #[test]
    fn broken_json_is_reported_with_its_line_and_column() {
        let text = "{\n  \"border\": true\n  \"border_width\": 6\n}\n";
        let found = check_config_text(r"C:\Users\x\mochi.json", text);
        assert_eq!(found.errors.len(), 1, "{found:?}");
        let error = &found.errors[0];
        assert!(error.starts_with(r"C:\Users\x\mochi.json:3:3:"), "{error}");
        assert!(error.contains("expected `,` or `}`"), "{error}");
        // The offending text, under a caret.
        assert!(error.contains("\"border_width\": 6"), "{error}");
        assert!(error.contains('^'), "{error}");
        // And the one line the verdict prints is the position, not the caret.
        assert_eq!(
            found.headline.as_deref(),
            Some(error.lines().next().unwrap())
        );
    }

    #[test]
    fn an_unknown_enum_value_is_reported_where_it_stands() {
        let found = check_config_text("mochi.json", r#"{ "border_style": "Roundedd" }"#);
        assert_eq!(found.errors.len(), 1, "{found:?}");
        assert!(found.errors[0].contains("unknown variant"), "{found:?}");
        assert!(found.errors[0].starts_with("mochi.json:1:"), "{found:?}");
    }

    #[test]
    fn every_broken_rule_is_reported_with_its_list_and_its_line() {
        let text = r#"{
  "ignore_rules": [
    { "kind": "Exe", "id": "(unclosed", "matching_strategy": "Regex" }
  ],
  "floating_applications": [
    { "kind": "Class", "id": "", "matching_strategy": "Equals" }
  ]
}
"#;
        let found = check_config_text("mochi.json", text);
        assert_eq!(found.errors.len(), 2, "{found:?}");

        let regex = &found.errors[0];
        assert!(regex.starts_with("ignore_rules[0]:"), "{regex}");
        assert!(regex.contains("invalid regular expression"), "{regex}");
        assert!(regex.contains("written on line 3"), "{regex}");

        let empty = &found.errors[1];
        assert!(empty.starts_with("floating_applications[0]:"), "{empty}");
        assert!(empty.contains("empty"), "{empty}");

        // The verdict line counts them and says what the daemon would do.
        let headline = found.headline.as_deref().unwrap_or_default();
        assert!(headline.starts_with("2 rules in mochi.json"), "{headline}");
        assert!(headline.contains("would drop them"), "{headline}");
    }

    /// Whatever `RuleSets::validate` complains about has to turn up here too,
    /// with a list name attached. The lists are named one by one in
    /// [`rule_lists`], so a tenth list added to `RuleSets` would otherwise be
    /// checked by the daemon and skipped in silence by this command.
    #[test]
    fn no_list_of_rules_goes_unchecked() {
        let broken = MatchingRule::simple(
            mochi_core::rules::ApplicationIdentifier::Exe,
            "(unclosed",
            mochi_core::rules::MatchingStrategy::Regex,
        );
        let sets = RuleSets {
            ignore_rules: vec![broken.clone()],
            manage_rules: vec![broken.clone()],
            floating_applications: vec![broken.clone()],
            tray_and_multi_window_applications: vec![broken.clone()],
            object_name_change_applications: vec![broken.clone()],
            border_overflow_applications: vec![broken.clone()],
            layered_whitelist: vec![broken.clone()],
            transparency_ignore_rules: vec![broken.clone()],
            slow_application_identifiers: vec![broken.clone()],
            own_ignore_rules: Vec::new(),
        };

        let reported: Vec<String> = rule_lists(&sets)
            .iter()
            .flat_map(|(key, _, rules)| broken_rules(key, None, rules))
            .collect();
        assert_eq!(reported.len(), sets.validate().len());
        assert_eq!(reported.len(), 9, "{reported:?}");
    }

    #[test]
    fn a_key_the_parser_throws_away_is_named() {
        let found = check_config_text("mochi.json", r#"{ "boarder_width": 6 }"#);
        assert!(found.errors.is_empty(), "{found:?}");
        assert_eq!(found.warnings.len(), 1, "{found:?}");
        assert!(found.warnings[0].contains("\"boarder_width\""), "{found:?}");
        assert!(found.warnings[0].contains("border_width"), "{found:?}");
    }

    /// The reason the inert key list is asked of the parser instead of written
    /// down: `layered_applications` is an alias that appears in no schema, and
    /// a hand written list would call the one key a migrated file depends on a
    /// typo. `$schema` is not Mochi's key and is not anybody's mistake either.
    #[test]
    fn an_alias_and_the_schema_link_are_not_mistaken_for_dead_keys() {
        assert!(inert_keys(r#"{ "layered_applications": [] }"#).is_empty());
        assert!(inert_keys(r#"{ "$schema": "https://example.invalid/s.json" }"#).is_empty());
        assert!(inert_keys(r#"{ "border": null }"#).is_empty());
        assert_eq!(
            inert_keys(r#"{ "nonsense": 1 }"#),
            vec!["nonsense".to_owned()]
        );
        assert!(inert_keys("not json at all").is_empty());
    }

    #[test]
    fn the_community_file_reports_its_mistyped_list_names_as_warnings() {
        let text = r#"{
  "$schema": "https://example.invalid/applications.json",
  "Zoom": { "floating_applications": [{ "kind": "Exe", "id": "Zoom.exe" }] }
}
"#;
        let found = check_app_rules_text("applications.json", text);
        assert!(found.errors.is_empty(), "{found:?}");
        assert_eq!(found.warnings.len(), 1, "{found:?}");
        assert!(
            found.warnings[0].contains("floating_applications"),
            "{found:?}"
        );
        assert!(
            found.warnings[0].contains("did you mean \"floating\""),
            "{found:?}"
        );
    }

    #[test]
    fn the_community_file_reports_its_broken_rules_under_its_own_key_names() {
        let text = r#"{ "Zoom": { "ignore": [{ "kind": "Exe", "id": "(", "matching_strategy": "Regex" }] } }"#;
        let found = check_app_rules_text("applications.json", text);
        assert_eq!(found.errors.len(), 1, "{found:?}");
        assert!(found.errors[0].starts_with("ignore[0]:"), "{found:?}");
    }

    #[test]
    fn the_community_file_names_the_path_it_would_be_read_from() {
        let text = r#"{ "app_specific_configuration_path": "$Env:USERPROFILE/applications.json" }"#;
        let found = check_config_text("mochi.json", text);
        assert_eq!(
            found.app_path.as_deref(),
            Some("$Env:USERPROFILE/applications.json")
        );
    }

    #[test]
    fn the_path_resolves_the_way_the_daemon_resolves_it() {
        let explicit = PathBuf::from(r"D:\somewhere\else.json");
        let (path, source) = resolve_config_path(Some(&explicit)).unwrap();
        assert_eq!(path, explicit);
        assert!(source.contains("command line"), "{source}");

        // Only meaningful when the variable is unset, the normal case.
        if std::env::var_os("MOCHI_CONFIG").is_none() {
            let (path, source) = resolve_config_path(None).unwrap();
            assert_eq!(path.file_name().unwrap(), "mochi.json");
            assert!(source.contains("MOCHI_CONFIG"), "{source}");
        }
    }

    #[test]
    fn the_environment_variable_spellings_expand_like_the_daemons() {
        let profile = std::env::var("USERPROFILE").unwrap_or_default();
        if profile.is_empty() {
            return;
        }
        for raw in [
            "$Env:USERPROFILE/applications.json",
            "$env:USERPROFILE/applications.json",
            "%USERPROFILE%/applications.json",
            "$USERPROFILE/applications.json",
        ] {
            assert_eq!(
                expand_env(raw),
                format!("{profile}/applications.json"),
                "for {raw}"
            );
        }
        assert_eq!(
            expand_env("%MOCHI_NOT_SET_ANYWHERE%"),
            "%MOCHI_NOT_SET_ANYWHERE%"
        );
        assert_eq!(
            expand_env(r"C:\Users\x\applications.json"),
            r"C:\Users\x\applications.json"
        );
    }

    /// A community rule file parses as a configuration of nothing at all,
    /// because `mochi.json` ignores the keys it does not know. Checking one as
    /// a configuration would call every application in it a dead key, which on
    /// the file on this machine is several hundred lines about a file that is
    /// perfectly fine.
    #[test]
    fn an_application_rules_file_is_recognised_for_what_it_is() {
        let apps = r#"{ "Zoom": { "ignore": [{ "kind": "Exe", "id": "Zoom.exe" }] } }"#;
        assert!(looks_like_app_rules(apps));
        assert!(!looks_like_app_rules(DEFAULT_CONFIG));
        // An empty object is neither, and is a configuration by default.
        assert!(!looks_like_app_rules("{}"));
        assert!(!looks_like_app_rules(
            r#"{ "ignore_rules": [{ "kind": "Exe", "id": "x.exe" }] }"#
        ));
    }

    #[test]
    fn a_line_is_only_named_when_the_file_holds_it_once() {
        let text = "a\nbb\ncc\nbb\n";
        assert_eq!(unique_line(text, "cc"), Some(3));
        assert_eq!(unique_line(text, "bb"), None);
        assert_eq!(unique_line(text, "zz"), None);
        assert_eq!(unique_line(text, ""), None);
    }
}
