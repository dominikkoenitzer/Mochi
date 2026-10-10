//! The documents `mochic` prints when it is given `--json`.
//!
//! Each one is a single JSON document on stdout, so a script can read the
//! answer without parsing text meant for a person. The field names are part of
//! the interface: within a major version fields are added, never renamed or
//! removed. `docs/json-output.md` lists every one.

use std::ffi::OsString;
use std::path::Path;

use mochi_client::QueryTarget;
use mochi_client::cli::Cmd;
use serde::Serialize;

/// The version of `mochic` that wrote a document.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Prints one document on stdout, pretty, the way `mochic` prints all JSON.
pub fn print<T: Serialize>(document: &T) {
    match serde_json::to_string_pretty(document) {
        Ok(json) => println!("{json}"),
        // Every document here is plain strings, numbers and lists, so this
        // cannot happen. If it ever does, the caller still gets JSON.
        Err(e) => println!(
            "{{\"error\":{{\"code\":\"internal\",\"message\":{}}}}}",
            serde_json::Value::String(format!("could not write the answer as JSON: {e}"))
        ),
    }
}

/// True when the command was given `--json`.
pub fn wants_json(command: &Cmd) -> bool {
    matches!(
        command,
        Cmd::State { json: true }
            | Cmd::Query { json: true, .. }
            | Cmd::Check { json: true, .. }
            | Cmd::Schema { json: true, .. }
            | Cmd::Doctor { json: true }
            | Cmd::Why { json: true }
            | Cmd::Hotkeys { json: true }
    )
}

/// What kind of failure an error document reports. The codes are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No daemon is running to answer.
    DaemonNotRunning,
    /// The daemon could not be reached, or answered with something that does
    /// not fit the command.
    Ipc,
    /// The daemon understood the command and refused it.
    Daemon,
    /// There is no configuration file to check.
    Config,
    /// A file could not be written.
    Io,
    /// The command line did not parse.
    Usage,
    /// Anything else.
    Internal,
}

/// An error that knows which code it is reported under. Its text is the
/// message a person sees without `--json`.
#[derive(Debug)]
pub struct Coded {
    code: ErrorCode,
    message: String,
}

impl Coded {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Coded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Coded {}

/// `{"error": {"code": "...", "message": "..."}}`, the one shape every
/// failure takes under `--json`.
#[derive(Debug, Serialize)]
pub struct ErrorOutput {
    pub error: ErrorBody,
}

/// The inside of an [`ErrorOutput`].
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// The same text `mochic` prints to stderr without `--json`.
    pub message: String,
}

impl ErrorOutput {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            error: ErrorBody {
                code,
                message: message.into(),
            },
        }
    }

    /// The document for an error, under the code it was marked with, or
    /// `internal` when nothing marked it.
    pub fn from_error(error: &anyhow::Error) -> Self {
        let code = error
            .downcast_ref::<Coded>()
            .or_else(|| {
                error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<Coded>())
            })
            .map_or(ErrorCode::Internal, |coded| coded.code);
        Self::new(code, format!("{error:#}"))
    }
}

/// The error document for a command line that did not parse, when the
/// command line asked for JSON. `None` means clap should report it as usual:
/// there was no `--json`, or the "error" is a help or version request.
pub fn usage_error(args: &[OsString], error: &clap::Error) -> Option<ErrorOutput> {
    use clap::error::ErrorKind;

    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        return None;
    }
    if !args
        .iter()
        .skip(1)
        .any(|arg| arg.to_str() == Some("--json"))
    {
        return None;
    }
    let rendered = error.render().to_string();
    let first = rendered
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("the command line could not be parsed");
    Some(ErrorOutput::new(
        ErrorCode::Usage,
        first.strip_prefix("error: ").unwrap_or(first),
    ))
}

/// `mochic query TARGET --json`.
#[derive(Debug, Serialize)]
pub struct QueryOutput {
    pub version: &'static str,
    /// The target asked for, spelled the way the command line spells it.
    pub target: QueryTarget,
    /// The daemon's answer, as the daemon gave it: a number, a string, a
    /// boolean or null.
    pub value: serde_json::Value,
}

impl QueryOutput {
    pub fn new(target: QueryTarget, value: serde_json::Value) -> Self {
        Self {
            version: VERSION,
            target,
            value,
        }
    }
}

/// `mochic schema --output PATH --json`.
#[derive(Debug, Serialize)]
pub struct SchemaWritten {
    pub version: &'static str,
    /// The file the schema was written to.
    pub path: String,
}

impl SchemaWritten {
    pub fn new(path: &Path) -> Self {
        Self {
            version: VERSION,
            path: path.display().to_string(),
        }
    }
}

/// Which kind of file `check` found at the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// A `mochi.json`.
    Config,
    /// A community application rules file.
    AppRules,
}

/// `mochic check [PATH] --json`.
#[derive(Debug, Serialize)]
pub struct CheckOutput {
    pub version: &'static str,
    /// The file that was checked.
    pub path: String,
    /// Where that path came from.
    pub source: String,
    pub kind: CheckKind,
    /// True when the daemon could start with this file. Warnings do not make
    /// a file unusable.
    pub usable: bool,
    /// One sentence saying what to fix first. Null when the file is usable.
    pub headline: Option<String>,
    /// What makes the file unusable. Each entry may run to several lines.
    pub errors: Vec<String>,
    /// What the daemon would carry on past, the hotkey file included.
    pub warnings: Vec<String>,
    /// Plain facts about what was read, one line each.
    pub notes: Vec<String>,
}

impl CheckOutput {
    /// Puts the findings of one check together. A file that could not be read
    /// has its reason as its only error.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: String,
        source: String,
        kind: CheckKind,
        unreadable: Option<String>,
        headline: Option<String>,
        mut errors: Vec<String>,
        warnings: Vec<String>,
        notes: Vec<String>,
    ) -> Self {
        let headline = match unreadable {
            Some(reason) => {
                errors.insert(0, reason);
                Some("the configuration could not be read".to_owned())
            }
            None if errors.is_empty() => None,
            None => Some(
                headline.unwrap_or_else(|| "the configuration cannot be used as it stands".into()),
            ),
        };
        Self {
            version: VERSION,
            path,
            source,
            kind,
            usable: errors.is_empty(),
            headline,
            errors,
            warnings,
            notes,
        }
    }
}
