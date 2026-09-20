//! Reading a hotkey file into bindings.
//!
//! The format follows the common hotkey file conventions, so a file written
//! for a standalone hotkey daemon parses here unchanged:
//!
//! ```text
//! .shell pwsh
//! # a comment
//! alt + h         : mochic focus left
//! alt + shift + g : powershell -NoProfile -File "C:\path\x.ps1"
//! alt + [1,2,3]   : focus-workspace [0,1,2]
//! ```
//!
//! Parsing never stops at the first bad line. [`Bindings::parse`] collects every
//! error and fails as a whole, which is what a `--check` wants, while
//! [`Bindings::parse_lossy`] keeps the good lines and hands the errors back, so
//! one typo cannot cost a user the rest of their keyboard.

use std::collections::HashMap;
use std::sync::OnceLock;

use mochi_client::Command;

use crate::shell::Shell;
use crate::trigger::Trigger;

/// What a binding does when its trigger fires.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send a command to the daemon.
    Command(Command),
    /// Hand a line to a shell and let it do whatever it likes.
    Shell {
        /// The shell named by the file's `.shell` line.
        shell: Shell,
        /// The line, exactly as written after the `:`.
        line: String,
    },
}

/// One binding: a trigger, what it does, and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// The key combination that fires it.
    pub trigger: Trigger,
    /// What firing it does.
    pub action: Action,
    /// The 1-based line of the file it was read from.
    pub line: usize,
    /// The right-hand side as written, for `mochic hotkeys` to print back.
    ///
    /// For a binding that came out of a `[a,b,c]` group this is the expanded
    /// form, so every binding shows the command it actually runs.
    pub source: String,
}

/// One reason a line could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ParseError {
    /// The 1-based line of the file.
    pub line: usize,
    /// The fragment that was wrong: the trigger, the command, or the whole line.
    pub text: String,
    /// What was wrong with it, as a sentence naming the offending text.
    pub message: String,
}

/// Every error a file produced, in the order the lines appear.
///
/// Dereferences to a slice, so the usual `len`, `iter` and indexing all work.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))]
pub struct ParseErrors(Vec<ParseError>);

impl ParseErrors {
    /// Takes the errors out.
    pub fn into_inner(self) -> Vec<ParseError> {
        self.0
    }
}

impl std::ops::Deref for ParseErrors {
    type Target = [ParseError];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Every binding in a file, ready for a keyboard hook to query.
///
/// Lookup goes through a [`HashMap`], so [`Bindings::get`] is one hash of nine
/// bytes and no allocation at all; it is called on every key press. The struct
/// owns everything it holds, which makes it `Send` and `'static` so the daemon
/// can pass a `Box<Bindings>` to its hook thread.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bindings {
    by_trigger: HashMap<Trigger, Binding>,
    /// The triggers in source order, so `iter` does not have to sort.
    order: Vec<Trigger>,
    shell: Shell,
}

impl Bindings {
    /// Reads a whole file, failing if any line is wrong.
    ///
    /// Every error is reported, not just the first, so one run of `mochic`
    /// tells a user everything they have to fix.
    pub fn parse(text: &str) -> Result<Self, ParseErrors> {
        let (bindings, errors) = Self::parse_lossy(text);
        if errors.is_empty() {
            Ok(bindings)
        } else {
            Err(ParseErrors(errors))
        }
    }

    /// Reads a whole file, keeping every line that was readable.
    ///
    /// This is what the daemon loads a live file with: a typo on one line
    /// costs that binding and nothing else.
    pub fn parse_lossy(text: &str) -> (Self, Vec<ParseError>) {
        let mut bindings = Self::default();
        let mut errors = Vec::new();

        // A file saved by a Windows editor or written by `Out-File` starts with
        // a byte order mark. It is not whitespace, so leaving it in front of the
        // first line would cost that line: a `.shell` directive stops being one
        // and every shell binding under it would run in the wrong shell.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);

        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let content = strip_comment(raw).trim();
            if content.is_empty() {
                continue;
            }
            if let Err(error) = bindings.read_line(line, content) {
                errors.extend(error);
            }
        }

        (bindings, errors)
    }

    /// The binding a key press fires, if there is one.
    ///
    /// A hash lookup on a `Copy` key: no allocation, safe to call from inside a
    /// low-level keyboard hook.
    pub fn get(&self, trigger: Trigger) -> Option<&Binding> {
        self.by_trigger.get(&trigger)
    }

    /// How many bindings were read.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// True when the file bound nothing.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The shell the file's `.shell` line named, [`Shell::Cmd`] by default.
    pub fn shell(&self) -> Shell {
        self.shell
    }

    /// Every binding, in the order the file lists them.
    pub fn iter(&self) -> impl Iterator<Item = &Binding> {
        self.order.iter().filter_map(|trigger| self.get(*trigger))
    }
}

impl Bindings {
    /// Reads one non-empty, comment-free line, adding whatever it binds.
    ///
    /// A line with a `[a,b,c]` group turns into several bindings, and any of
    /// them can fail on its own, so the error side is a whole list.
    fn read_line(&mut self, line: usize, content: &str) -> Result<(), Vec<ParseError>> {
        let fail = |text: &str, message: String| {
            vec![ParseError {
                line,
                text: text.to_owned(),
                message,
            }]
        };

        if content.starts_with('.') {
            return self.read_directive(line, content).map_err(|e| vec![e]);
        }

        let Some((left, right)) = split_at_colon(content) else {
            return Err(fail(
                content,
                "there is no `:` between the trigger and what it runs".to_owned(),
            ));
        };
        let (left, right) = (left.trim(), right.trim());

        let pairs = match expand(left, right) {
            Ok(pairs) => pairs,
            Err(message) => return Err(fail(content, message)),
        };

        let mut errors = Vec::new();
        for (trigger_text, action_text) in pairs {
            if let Err(error) = self.add(line, &trigger_text, &action_text) {
                errors.push(error);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Reads a `.shell` line. It is the only directive the format has.
    fn read_directive(&mut self, line: usize, content: &str) -> Result<(), ParseError> {
        let error = |text: &str, message: String| ParseError {
            line,
            text: text.to_owned(),
            message,
        };

        let rest = content
            .strip_prefix(".shell")
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
            .ok_or_else(|| {
                error(
                    content,
                    format!("`{content}` is not a directive, the only one is `.shell`"),
                )
            })?
            .trim();

        if rest.is_empty() {
            return Err(error(
                content,
                "`.shell` needs a shell after it, cmd, powershell, pwsh or bash".to_owned(),
            ));
        }

        self.shell = rest.parse().map_err(|message| error(rest, message))?;
        Ok(())
    }
}

impl Bindings {
    /// Parses one already-expanded `trigger : action` pair and stores it.
    fn add(
        &mut self,
        line: usize,
        trigger_text: &str,
        action_text: &str,
    ) -> Result<(), ParseError> {
        let trigger: Trigger = trigger_text.parse().map_err(|message| ParseError {
            line,
            text: trigger_text.to_owned(),
            message,
        })?;

        if let Some(first) = self.by_trigger.get(&trigger) {
            return Err(ParseError {
                line,
                text: trigger_text.to_owned(),
                message: format!(
                    "`{trigger}` is already bound on line {}, a key can only do one thing",
                    first.line
                ),
            });
        }

        let action = action_for(action_text, self.shell).map_err(|message| ParseError {
            line,
            text: action_text.to_owned(),
            message,
        })?;

        self.order.push(trigger);
        self.by_trigger.insert(
            trigger,
            Binding {
                trigger,
                action,
                line,
                source: action_text.to_owned(),
            },
        );
        Ok(())
    }
}

/// Decides whether a right-hand side is a Mochi command or a shell line.
///
/// Spelling `mochic` out makes the intent explicit: the rest has to be a Mochi
/// command, and if it is not, that is an error rather than a shell line that
/// would fail at the first key press.
///
/// Without the prefix the line is a Mochi command only when the first word
/// names a subcommand of the CLI grammar *and* the whole line parses as one.
/// The second half of that is not pedantry. `start` is a subcommand of `mochic`
/// and also the Windows command for opening something, so `alt + enter : start
/// wt` has to stay a shell line; going by the first word alone would steal it.
fn action_for(text: &str, shell: Shell) -> Result<Action, String> {
    let words = split_words(text);
    let Some(first) = words.first() else {
        return Err("there is nothing after the `:` to run".to_owned());
    };

    let explicit = first.eq_ignore_ascii_case("mochic") || first.eq_ignore_ascii_case("mochic.exe");
    let rest = if explicit { &words[1..] } else { &words[..] };

    if explicit {
        return command_from_words(rest).map(Action::Command);
    }

    match rest.first() {
        Some(word) if is_subcommand(word) => match command_from_words(rest) {
            Ok(command) => Ok(Action::Command(command)),
            Err(_) => Ok(Action::Shell {
                shell,
                line: text.to_owned(),
            }),
        },
        _ => Ok(Action::Shell {
            shell,
            line: text.to_owned(),
        }),
    }
}

/// Turns the words of a `mochic` binding into the command they send.
///
/// The grammar lives in `mochi-client` next to the CLI, so a binding and the
/// command line a user would type can never disagree.
pub fn command_from_words(words: &[String]) -> Result<Command, String> {
    mochi_client::cli::command_from_args(words)
}

/// True when `word` names a subcommand of the `mochic` grammar.
///
/// The names come from clap itself rather than a list kept here by hand, so a
/// new subcommand is bindable the moment it exists.
fn is_subcommand(word: &str) -> bool {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();

    NAMES
        .get_or_init(|| {
            use clap::CommandFactory;

            mochi_client::cli::Cli::command()
                .get_subcommands()
                .flat_map(|sub| {
                    std::iter::once(sub.get_name().to_owned())
                        .chain(sub.get_all_aliases().map(str::to_owned))
                })
                .collect()
        })
        .iter()
        .any(|name| name == word)
}

/// Cuts a trailing comment off a line.
///
/// A `#` starts a comment when it opens the line or follows whitespace and is
/// not inside double quotes. That keeps `"C:\note#1.ps1"` intact while letting
/// a binding carry a note after it.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut after_space = true;

    for (at, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '#' if !quoted && after_space => return &line[..at],
            _ => {}
        }
        after_space = character.is_whitespace();
    }

    line
}

/// Splits a line at the `:` that separates the trigger from what it runs.
///
/// Colons inside a `[a,b,c]` group are skipped, and so is everything after the
/// first one, which is why a Windows path on the right-hand side survives.
///
/// A line whose only colon sits inside an unclosed `[` falls back to that one,
/// so the error a user gets names the bracket instead of the missing colon.
fn split_at_colon(line: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut nested = None;

    for (at, character) in line.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some((&line[..at], &line[at + 1..])),
            ':' => nested = nested.or(Some(at)),
            _ => {}
        }
    }

    nested.map(|at| (&line[..at], &line[at + 1..]))
}

/// A side of a binding split around its `[a,b,c]` group: the text before it,
/// its items, and the text after it.
type Group<'a> = (&'a str, Vec<&'a str>, &'a str);

/// Pulls the one `[a,b,c]` group out of a side of a binding.
///
/// Gives back the text before the group, its items, and the text after it, or
/// `None` when the side has no group at all.
fn split_group(side: &str) -> Result<Option<Group<'_>>, String> {
    let Some(open) = side.find('[') else {
        return match side.find(']') {
            Some(_) => Err(format!("`{side}` closes a `]` that was never opened")),
            None => Ok(None),
        };
    };

    let close = side[open..]
        .find(']')
        .map(|at| open + at)
        .ok_or_else(|| format!("`{side}` opens a `[` that is never closed"))?;

    let (before, after) = (&side[..open], &side[close + 1..]);
    if after.contains('[') || after.contains(']') {
        return Err(format!(
            "`{side}` has more than one [...] group, which is one too many"
        ));
    }

    let items: Vec<&str> = side[open + 1..close].split(',').map(str::trim).collect();
    if items.iter().any(|item| item.is_empty()) {
        return Err(format!("`{side}` has an empty item in its [...] group"));
    }

    Ok(Some((before, items, after)))
}

/// Expands the `[a,b,c]` groups of one line into the bindings it stands for.
///
/// The trigger decides. `alt + [1,2] : focus-workspace [0,1]` is two bindings,
/// paired in order, and a trigger group without a partner on the right has
/// nothing to pair with and is an error rather than a guess.
///
/// Without a group on the left the right-hand side is taken exactly as written,
/// brackets and all. It has to be: the right-hand side is a command line, and a
/// perfectly ordinary one contains brackets. `[console]::beep(440,200)` is a
/// PowerShell line, not a group of one, and `echo ]` is a line that prints a
/// bracket, not a syntax error. Reading either of them as a group cost the user
/// a binding that should simply have worked.
fn expand(left: &str, right: &str) -> Result<Vec<(String, String)>, String> {
    let joined = |before: &str, item: &str, after: &str| format!("{before}{item}{after}");

    let Some((lb, ls, la)) = split_group(left)? else {
        return Ok(vec![(left.to_owned(), right.to_owned())]);
    };
    let Some((rb, rs, ra)) = split_group(right)? else {
        return Err(format!(
            "the trigger expands to {} bindings but the command is a single line",
            ls.len()
        ));
    };
    if ls.len() != rs.len() {
        return Err(format!(
            "the trigger expands to {} bindings but the command expands to {}",
            ls.len(),
            rs.len()
        ));
    }
    Ok(ls
        .iter()
        .zip(rs.iter())
        .map(|(l, r)| (joined(lb, l, la), joined(rb, r, ra)))
        .collect())
}

/// Splits a command line into words, keeping double-quoted runs together.
///
/// Backslashes are left alone because the arguments in a hotkey file are mostly
/// Windows paths; the quotes themselves are dropped.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;

    for character in text.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }

    if started {
        words.push(current);
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use mochi_client::{CycleDirection, Direction};

    fn parse(text: &str) -> Bindings {
        Bindings::parse(text).expect("should have parsed")
    }

    fn errors(text: &str) -> Vec<String> {
        Bindings::parse(text)
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn action(text: &str, trigger: &str) -> Action {
        parse(text)
            .get(trigger.parse().unwrap())
            .expect("no such binding")
            .action
            .clone()
    }

    fn focus_left() -> Action {
        Action::Command(Command::Focus {
            direction: Direction::Left,
        })
    }

    #[test]
    fn bindings_can_be_sent_to_a_hook_thread() {
        fn assert_send_and_static<T: Send + 'static>() {}
        assert_send_and_static::<Bindings>();
    }

    #[test]
    fn an_empty_file_binds_nothing_and_defaults_to_cmd() {
        let bindings = parse("");
        assert!(bindings.is_empty());
        assert_eq!(bindings.len(), 0);
        assert_eq!(bindings.shell(), Shell::Cmd);
        assert_eq!(Bindings::default(), bindings);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let bindings = parse("# a note\n\n   \nalt + h : mochic focus left\n# another\n");
        assert_eq!(bindings.len(), 1);
    }

    #[test]
    fn a_trailing_comment_is_cut_off_the_line() {
        let bindings = parse("alt + h : mochic focus left   # vim keys");
        let binding = bindings.get("alt + h".parse().unwrap()).unwrap();
        assert_eq!(binding.source, "mochic focus left");
    }

    #[test]
    fn a_hash_inside_quotes_is_not_a_comment() {
        let bindings = parse(r#"alt + g : notepad "C:\notes\day#1.txt""#);
        let binding = bindings.get("alt + g".parse().unwrap()).unwrap();
        assert_eq!(binding.source, r#"notepad "C:\notes\day#1.txt""#);
    }

    #[test]
    fn the_shell_directive_picks_the_shell() {
        assert_eq!(parse(".shell pwsh").shell(), Shell::Pwsh);
        assert_eq!(parse(".shell BASH").shell(), Shell::Bash);
        assert_eq!(
            parse(".shell powershell   # the old one").shell(),
            Shell::Powershell
        );
    }

    #[test]
    fn a_byte_order_mark_does_not_cost_the_first_line() {
        // Windows editors save UTF-8 with a BOM, and the first line of a hotkey
        // file is usually the `.shell` directive the rest of it depends on.
        let bindings = parse("\u{feff}.shell pwsh\nalt + g : ls");
        assert_eq!(bindings.shell(), Shell::Pwsh);
        assert_eq!(
            bindings.get("alt + g".parse().unwrap()).unwrap().action,
            Action::Shell {
                shell: Shell::Pwsh,
                line: "ls".to_owned(),
            }
        );
        assert_eq!(parse("\u{feff}alt + h : mochic focus left").len(), 1);
    }

    #[test]
    fn the_shell_directive_applies_to_the_lines_under_it() {
        let bindings = parse(".shell bash\nalt + g : ls -la");
        assert_eq!(
            bindings.get("alt + g".parse().unwrap()).unwrap().action,
            Action::Shell {
                shell: Shell::Bash,
                line: "ls -la".to_owned(),
            }
        );
    }

    #[test]
    fn the_mochic_prefix_is_optional() {
        assert_eq!(
            action("alt + h : mochic focus left", "alt + h"),
            focus_left()
        );
        assert_eq!(action("alt + h : focus left", "alt + h"), focus_left());
        assert_eq!(
            action("alt + h : mochic.exe focus left", "alt + h"),
            focus_left()
        );
        assert_eq!(
            action("alt + h : MOCHIC focus left", "alt + h"),
            focus_left()
        );
    }

    #[test]
    fn a_first_word_that_is_no_subcommand_is_a_shell_line() {
        let text = r#"alt + shift + g : powershell -NoProfile -File "C:\x.ps1""#;
        assert_eq!(
            action(text, "alt + shift + g"),
            Action::Shell {
                shell: Shell::Cmd,
                line: r#"powershell -NoProfile -File "C:\x.ps1""#.to_owned(),
            }
        );
        assert!(matches!(
            action("alt + enter : start wt", "alt + enter"),
            Action::Shell { .. }
        ));
    }

    #[test]
    fn an_explicit_prefix_turns_a_bad_command_into_an_error() {
        let found = errors("alt + h : mochic focus sideways");
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("line 1:"), "{found:?}");
        assert!(found[0].contains("sideways"), "{found:?}");
    }

    #[test]
    fn a_subcommand_name_that_is_also_a_shell_command_stays_a_shell_line() {
        // `start` is a subcommand of `mochic` and the Windows command for
        // opening something. `start wt` is not a valid `mochic start`, so it
        // belongs to the shell.
        assert!(is_subcommand("start"));
        assert_eq!(
            action("alt + enter : start wt", "alt + enter"),
            Action::Shell {
                shell: Shell::Cmd,
                line: "start wt".to_owned(),
            }
        );
    }

    #[test]
    fn a_known_subcommand_with_bad_arguments_needs_the_prefix_to_be_an_error() {
        // Without the prefix nothing is claimed, so the line goes to the shell.
        assert!(matches!(
            action("alt + h : focus sideways", "alt + h"),
            Action::Shell { .. }
        ));
        // With it, the user said this is a Mochi command, so say what is wrong.
        let found = errors("alt + h : mochic focus sideways");
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("sideways"), "{found:?}");
    }

    #[test]
    fn a_bare_mochic_prefix_with_nothing_after_it_is_an_error() {
        assert_eq!(errors("alt + h : mochic").len(), 1);
    }

    #[test]
    fn a_shell_line_keeps_its_quoting_untouched() {
        let bindings = parse(r#"alt + g : notepad "C:\Program Files\a b.txt""#);
        let binding = bindings.get("alt + g".parse().unwrap()).unwrap();
        assert_eq!(binding.source, r#"notepad "C:\Program Files\a b.txt""#);
    }

    #[test]
    fn a_quoted_argument_reaches_the_command_as_one_word() {
        let bindings = parse(r#"alt + g : mochic float-rule exe "a b.exe""#);
        assert!(matches!(
            bindings.get("alt + g".parse().unwrap()).unwrap().action,
            Action::Command(Command::FloatRule { .. })
        ));
    }

    #[test]
    fn a_windows_path_after_the_first_colon_survives() {
        let bindings = parse(r#"alt + g : cmd /C "C:\Users\x\run.bat""#);
        let binding = bindings.get("alt + g".parse().unwrap()).unwrap();
        assert_eq!(binding.source, r#"cmd /C "C:\Users\x\run.bat""#);
    }

    #[test]
    fn a_bracket_group_expands_pairwise() {
        let bindings = parse("alt + [1,2,3] : focus-workspace [0,1,2]");
        assert_eq!(bindings.len(), 3);
        for (key, index) in [("alt + 1", 0), ("alt + 2", 1), ("alt + 3", 2)] {
            assert_eq!(
                bindings.get(key.parse().unwrap()).unwrap().action,
                Action::Command(Command::FocusWorkspace { index }),
                "for {key}"
            );
        }
    }

    #[test]
    fn every_binding_of_a_group_keeps_the_line_it_came_from() {
        let bindings = parse("# header\nalt + [1,2] : focus-workspace [0,1]");
        for binding in bindings.iter() {
            assert_eq!(binding.line, 2);
        }
        assert_eq!(
            bindings.get("alt + 2".parse().unwrap()).unwrap().source,
            "focus-workspace 1"
        );
    }

    #[test]
    fn a_group_works_with_modifiers_on_both_sides() {
        let bindings = parse("alt + shift + [1,2] : mochic move-to-workspace [0,1]");
        assert_eq!(bindings.len(), 2);
        assert_eq!(
            bindings
                .get("alt + shift + 2".parse().unwrap())
                .unwrap()
                .action,
            Action::Command(Command::MoveToWorkspace { index: 1 })
        );
    }

    #[test]
    fn a_group_of_one_is_allowed() {
        assert_eq!(parse("alt + [1] : focus-workspace [0]").len(), 1);
    }

    #[test]
    fn a_group_on_the_trigger_alone_is_an_error() {
        let found = errors("alt + [1,2] : mochic retile");
        assert_eq!(found.len(), 1);
        assert!(
            found[0].contains("2 bindings but the command is a single line"),
            "{found:?}"
        );
    }

    #[test]
    fn brackets_on_the_command_side_are_text_when_the_trigger_has_no_group() {
        // A command line is allowed to contain brackets, and plenty do. Nothing
        // here may read one as a group the trigger never asked for.
        let bindings = Bindings::parse(
            ".shell pwsh\n\
             alt + b : [console]::beep(440,200)\n\
             alt + g : echo ]\n",
        )
        .expect("a line with brackets in it is a line, not a syntax error");

        let line = |text: &str| match &bindings.get(text.parse().unwrap()).unwrap().action {
            Action::Shell { line, .. } => line.clone(),
            other => panic!("{text} should be a shell line, got {other:?}"),
        };
        assert_eq!(line("alt + b"), "[console]::beep(440,200)");
        assert_eq!(line("alt + g"), "echo ]");
    }

    #[test]
    fn a_group_on_the_command_alone_is_taken_literally() {
        // Written without the `mochic` prefix this is a shell line, so the
        // brackets stay. That is the same rule as above, seen from the side
        // where it is least expected.
        let bindings = Bindings::parse("alt + h : focus-workspace [0,1]").unwrap();
        assert_eq!(
            bindings.get("alt + h".parse().unwrap()).unwrap().action,
            Action::Shell {
                shell: Shell::Cmd,
                line: "focus-workspace [0,1]".to_owned(),
            }
        );

        // With the prefix it has to be a command, and a command it is not.
        let found = errors("alt + h : mochic focus-workspace [0,1]");
        assert!(found[0].contains("line 1"), "{found:?}");
    }

    #[test]
    fn a_trigger_group_still_needs_a_partner() {
        let found = errors("alt + [1,2] : focus-workspace 0");
        assert!(
            found[0].contains("2 bindings but the command is a single line"),
            "{found:?}"
        );
    }

    #[test]
    fn a_mismatched_group_arity_is_an_error() {
        let found = errors("alt + [1,2,3] : focus-workspace [0,1]");
        assert!(
            found[0].contains("3 bindings but the command expands to 2"),
            "{found:?}"
        );
    }

    #[test]
    fn a_malformed_group_is_an_error() {
        assert!(errors("alt + [1,2 : focus-workspace [0,1]")[0].contains("never closed"));
        assert!(errors("alt + 1] : mochic retile")[0].contains("never opened"));
        assert!(errors("alt + [1,,2] : focus-workspace [0,1,2]")[0].contains("empty item"));
        assert!(errors("alt + [1,2][3] : focus-workspace [0,1]")[0].contains("one too many"));
    }

    #[test]
    fn a_duplicate_trigger_names_both_lines() {
        let found = errors("alt + h : mochic focus left\nalt + h : mochic focus right");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0],
            "line 2: `alt + h` is already bound on line 1, a key can only do one thing"
        );
    }

    #[test]
    fn a_duplicate_is_found_across_different_spellings() {
        let found =
            errors("ALT + Shift + H : mochic focus left\nshift + alt + h : mochic focus up");
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("already bound on line 1"));
    }

    #[test]
    fn a_line_without_a_colon_is_an_error() {
        assert!(errors("alt + h mochic focus left")[0].contains("no `:`"));
    }

    #[test]
    fn an_unknown_key_carries_its_line_and_text() {
        let found = Bindings::parse("# note\nalt + wiggle : mochic retile").unwrap_err();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].text, "alt + wiggle");
        assert_eq!(found[0].message, "`wiggle` is not a key name");
    }

    #[test]
    fn an_unknown_modifier_carries_its_line_and_text() {
        let found = Bindings::parse("hyper + h : mochic retile").unwrap_err();
        assert_eq!(found[0].message, "`hyper` is not a modifier");
    }

    #[test]
    fn an_unknown_shell_is_an_error() {
        assert!(errors(".shell fish")[0].contains("not a shell"));
        assert!(errors(".shell")[0].contains("needs a shell after it"));
        assert!(errors(".reload every 5s")[0].contains("not a directive"));
    }

    #[test]
    fn every_bad_line_is_reported_not_just_the_first() {
        let found = errors(concat!(
            "alt + wiggle : mochic retile\n",
            "alt + h      : mochic focus left\n",
            "hyper + j    : mochic retile\n",
            "alt + h      : mochic focus up\n",
        ));
        assert_eq!(found.len(), 3);
        assert!(found[0].starts_with("line 1:"));
        assert!(found[1].starts_with("line 3:"));
        assert!(found[2].starts_with("line 4:"));
    }

    #[test]
    fn the_error_list_displays_one_error_per_line() {
        let found =
            Bindings::parse("alt + wiggle : mochic retile\nhyper + j : mochic retile").unwrap_err();
        assert_eq!(found.to_string().lines().count(), 2);
        assert_eq!(found.len(), 2);
        assert_eq!(found.into_inner().len(), 2);
    }

    #[test]
    fn a_lossy_parse_keeps_every_line_that_worked() {
        let (bindings, found) = Bindings::parse_lossy(concat!(
            "alt + h      : mochic focus left\n",
            "alt + wiggle : mochic retile\n",
            "alt + l      : mochic focus right\n",
        ));
        assert_eq!(bindings.len(), 2);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        assert_eq!(
            bindings.get("alt + h".parse().unwrap()).unwrap().action,
            focus_left()
        );
    }

    #[test]
    fn a_lossy_parse_of_a_clean_file_reports_nothing() {
        let (bindings, found) = Bindings::parse_lossy("alt + h : mochic focus left");
        assert!(found.is_empty());
        assert_eq!(bindings.len(), 1);
    }

    #[test]
    fn a_lossy_parse_keeps_the_good_half_of_an_expanded_line() {
        let (bindings, found) =
            Bindings::parse_lossy("alt + [1,wiggle] : mochic focus-workspace [0,1]");
        assert_eq!(bindings.len(), 1);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn iteration_follows_the_file_not_the_hash_order() {
        let bindings = parse(concat!(
            "alt + l : mochic focus right\n",
            "alt + h : mochic focus left\n",
            "alt + [1,2] : focus-workspace [0,1]\n",
        ));
        let triggers: Vec<String> = bindings.iter().map(|b| b.trigger.to_string()).collect();
        assert_eq!(triggers, ["alt + l", "alt + h", "alt + 1", "alt + 2"]);
        let lines: Vec<usize> = bindings.iter().map(|b| b.line).collect();
        assert_eq!(lines, [1, 2, 3, 3]);
    }

    #[test]
    fn a_binding_records_the_line_it_came_from() {
        let bindings = parse("\n\n# note\nalt + v : mochic cycle-layout next");
        let binding = bindings.get("alt + v".parse().unwrap()).unwrap();
        assert_eq!(binding.line, 4);
        assert_eq!(binding.source, "mochic cycle-layout next");
        assert_eq!(
            binding.action,
            Action::Command(Command::CycleLayout {
                direction: CycleDirection::Next
            })
        );
    }

    #[test]
    fn a_shell_runs_its_line_through_the_right_arguments() {
        assert_eq!(Shell::Cmd.args_for("start wt"), ["/C", "start wt"]);
        assert_eq!(
            Shell::Pwsh.args_for("Get-Date"),
            ["-NoProfile", "-Command", "Get-Date"]
        );
        assert_eq!(
            Shell::Powershell.args_for("Get-Date"),
            ["-NoProfile", "-Command", "Get-Date"]
        );
        assert_eq!(Shell::Bash.args_for("ls -la"), ["-c", "ls -la"]);
        assert_eq!(Shell::Cmd.program(), "cmd.exe");
        assert_eq!(Shell::Pwsh.to_string(), "pwsh");
        assert_eq!("BASH".parse::<Shell>().unwrap(), Shell::Bash);
        assert!("fish".parse::<Shell>().is_err());
    }
}
