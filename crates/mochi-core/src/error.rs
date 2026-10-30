//! The error type every fallible operation in this crate returns.

use crate::model::WindowId;

/// Everything that can go wrong in pure window-management logic.
///
/// The variants are cheap to clone and compare so they can travel over the IPC
/// wire without losing information.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// There are no monitors, or the focused index points past the end.
    #[error("no monitor is focused")]
    NoFocusedMonitor,

    /// The focused monitor has no workspaces.
    #[error("monitor {0} has no focused workspace")]
    NoFocusedWorkspace(usize),

    /// An operation needed a focused window and there was none.
    #[error("no window is focused")]
    NoFocusedWindow,

    /// An operation needed a focused container and there was none.
    #[error("no container is focused")]
    NoFocusedContainer,

    /// The monitor index does not exist.
    #[error("monitor {0} does not exist")]
    MonitorNotFound(usize),

    /// The workspace index does not exist on that monitor.
    #[error("workspace {workspace} does not exist on monitor {monitor}")]
    WorkspaceNotFound {
        /// The monitor that was searched.
        monitor: usize,
        /// The workspace index that was asked for.
        workspace: usize,
    },

    /// No workspace anywhere carries that name.
    #[error("no workspace is named {0:?}")]
    WorkspaceNameNotFound(String),

    /// The window is not managed by any workspace.
    #[error("window {0} is not managed")]
    WindowNotFound(WindowId),

    /// Workspaces are created on demand, but only up to [`crate::MAX_WORKSPACES`].
    #[error("workspace index {0} is out of range, at most {max} workspaces are supported", max = crate::MAX_WORKSPACES)]
    WorkspaceIndexOutOfRange(usize),

    /// The operation only makes sense with more than one of something.
    #[error("{0}")]
    NothingToDo(&'static str),

    /// A string could not be parsed into one of the small enums.
    #[error("{value:?} is not a valid {kind}")]
    Parse {
        /// The kind of value that was expected, for example `direction`.
        kind: &'static str,
        /// The string that could not be parsed.
        value: String,
    },

    /// A rule carried a pattern that is not a valid regular expression.
    #[error("invalid regular expression {pattern:?}: {message}")]
    InvalidRegex {
        /// The pattern as written in the config.
        pattern: String,
        /// The compiler's complaint.
        message: String,
    },

    /// A rule carried an empty identifier, which names no window and would be
    /// a wildcard for the substring strategies.
    #[error("a {kind} rule has an empty id, which would match every window")]
    EmptyRuleId {
        /// The piece of window metadata the rule looks at.
        kind: crate::rules::ApplicationIdentifier,
    },

    /// A `scratchpads` entry had no name, so no command could ever reach it.
    #[error("a scratchpad has an empty name, so no command can reach it")]
    EmptyScratchpadName,

    /// JSON could not be turned into a config or a rule set.
    #[error("invalid json: {0}")]
    Json(String),
}

impl Error {
    /// A failure to read `text` as JSON, in the words of the file and at the
    /// place [`json_place`] gives, which is the place `mochic check` prints.
    #[must_use]
    pub fn json(text: &str, error: &serde_json::Error) -> Self {
        let words = json_words(error);
        Self::Json(match json_place(text, error) {
            Some((line, column)) => format!("{words} at line {line} column {column}"),
            None => words,
        })
    }
}

/// For failures that have no text to point into, such as a value turned into
/// a type or a type turned back into JSON. A parse of text uses [`Error::json`].
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(plain_json_error(&error.to_string()))
    }
}

/// serde's message without its ` at line L column C`, in the words of the file.
#[must_use]
pub fn json_words(error: &serde_json::Error) -> String {
    let message = error.to_string();
    let position = format!(" at line {} column {}", error.line(), error.column());
    plain_json_error(message.strip_suffix(&position).unwrap_or(&message))
}

/// The line and column, both counted from 1, where reading `text` failed, or
/// `None` when serde knows no place.
#[must_use]
pub fn json_place(text: &str, error: &serde_json::Error) -> Option<(usize, usize)> {
    let (line, column) = (error.line(), error.column());
    if line == 0 {
        return None;
    }
    // Column 0 is serde standing just past a line break: the mistake is at
    // the end of the line before.
    if column == 0
        && let Some(before) = line.checked_sub(2).and_then(|at| text.lines().nth(at))
    {
        return Some((line - 1, before.len() + 1));
    }
    Some((line, column))
}

/// A serde message with the Rust words in it said the way a config file says
/// them: `i32` is a whole number, a sequence is a list, a map is an object.
///
/// Text the user typed is echoed as typed, so a value that happens to hold
/// one of these words is not rewritten: a string in quotes, and the name in
/// `unknown variant` or `unknown field`.
#[must_use]
pub fn plain_json_error(message: &str) -> String {
    let mut serde_words = Vec::new();
    let mut typed = Vec::new();
    let mut rest = message;
    while let Some((start, end)) = typed_span(rest) {
        serde_words.push(&rest[..start]);
        typed.push(&rest[start..end]);
        rest = &rest[end..];
    }
    serde_words.push(rest);
    let out_of_range = serde_words
        .iter()
        .any(|words| words.contains("invalid value: integer `"));
    let mut plain = String::with_capacity(message.len());
    for (at, words) in serde_words.iter().enumerate() {
        plain.push_str(&plain_words(words, out_of_range));
        if let Some(text) = typed.get(at) {
            plain.push_str(text);
        }
    }
    plain
}

/// The first stretch of `message` that holds text from the file, as a byte
/// range: a string serde or a custom error printed in quotes, or the name in
/// `unknown variant` and `unknown field`, which serde prints raw between
/// backticks. A stretch whose end cannot be found runs to the end, so a
/// doubtful case is left as typed rather than rewritten.
fn typed_span(message: &str) -> Option<(usize, usize)> {
    let quoted = message.find('"').map(|start| {
        let mut escaped = false;
        let end = message[start + 1..]
            .char_indices()
            .find(|&(_, c)| {
                let closes = c == '"' && !escaped;
                escaped = c == '\\' && !escaped;
                closes
            })
            .map_or(message.len(), |(at, _)| start + 1 + at + 1);
        (start, end)
    });
    let named = ["unknown variant `", "unknown field `"]
        .iter()
        .filter_map(|lead| message.find(lead).map(|at| at + lead.len()))
        .min()
        .map(|start| {
            // The name is raw and may hold a backtick, so it ends at the last
            // place serde's own words pick up again.
            let end = ["`, expected ", "`, there are no "]
                .iter()
                .filter_map(|tail| message[start..].rfind(tail))
                .max()
                .map_or(message.len(), |at| start + at);
            (start, end)
        });
    match (quoted, named) {
        (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// [`plain_json_error`] for a stretch of serde's own words.
fn plain_words(message: &str, out_of_range: bool) -> String {
    const PHRASES: [(&str, &str); 10] = [
        (
            "EOF while parsing a value",
            "the file ends where a value should be",
        ),
        (
            "EOF while parsing an object",
            "the file ends before the closing }",
        ),
        (
            "EOF while parsing a list",
            "the file ends before the closing ]",
        ),
        (
            "EOF while parsing a string",
            "the file ends inside a string",
        ),
        (
            "control character (\\u0000-\\u001F) found while parsing a string",
            "a string is not closed on its line, or holds a tab",
        ),
        ("expected ident", "expected true, false or null"),
        ("invalid type: map", "invalid type: an object"),
        ("invalid type: sequence", "invalid type: a list"),
        ("expected a sequence", "expected a list"),
        ("expected a map", "expected an object"),
    ];
    let mut out = message.replace("floating point `", "decimal number `");
    for (from, to) in PHRASES {
        out = out.replace(from, to);
    }
    let mut plain = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(at) = rest.find("expected ") {
        let (head, tail) = rest.split_at(at + "expected ".len());
        plain.push_str(head);
        let word_end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        let word = &tail[..word_end];
        rest = &tail[word_end..];
        if let Some(words) = whole_number(word, out_of_range) {
            plain.push_str(&words);
            continue;
        }
        match word {
            "f32" | "f64" => plain.push_str("a number"),
            "struct" => {
                plain.push_str("an object");
                // The Rust name of the struct follows, which the file never spells.
                let name_end = rest
                    .trim_start()
                    .find(|c: char| !c.is_ascii_alphanumeric())
                    .map_or(rest.len(), |end| end + rest.len() - rest.trim_start().len());
                rest = &rest[name_end..];
            }
            _ => plain.push_str(word),
        }
    }
    plain.push_str(rest);
    plain
}

/// A Rust integer type as a config file says it, with its bounds when the
/// value given was a whole number outside them.
fn whole_number(word: &str, out_of_range: bool) -> Option<String> {
    let (min, max) = match word {
        "i8" => (i8::MIN.to_string(), i8::MAX.to_string()),
        "i16" => (i16::MIN.to_string(), i16::MAX.to_string()),
        "i32" => (i32::MIN.to_string(), i32::MAX.to_string()),
        "i64" => (i64::MIN.to_string(), i64::MAX.to_string()),
        "i128" => (i128::MIN.to_string(), i128::MAX.to_string()),
        "isize" => (isize::MIN.to_string(), isize::MAX.to_string()),
        "u8" => (u8::MIN.to_string(), u8::MAX.to_string()),
        "u16" => (u16::MIN.to_string(), u16::MAX.to_string()),
        "u32" => (u32::MIN.to_string(), u32::MAX.to_string()),
        "u64" => (u64::MIN.to_string(), u64::MAX.to_string()),
        "u128" => (u128::MIN.to_string(), u128::MAX.to_string()),
        "usize" => (usize::MIN.to_string(), usize::MAX.to_string()),
        _ => return None,
    };
    Some(if out_of_range {
        format!("a whole number from {min} to {max}")
    } else if min == "0" {
        "a whole number of 0 or more".to_owned()
    } else {
        "a whole number".to_owned()
    })
}

/// A [`Result`](std::result::Result) with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
