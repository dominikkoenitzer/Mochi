//! Which shell runs the non-Mochi half of a hotkey file.
//!
//! A binding whose right-hand side is not a `mochic` command is handed to a
//! shell verbatim, the way a hotkey file expects. The file picks the shell with
//! a `.shell` line; without one the default is [`Shell::Cmd`].

/// The shells a `.shell` line can name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Shell {
    /// `cmd.exe`, the default when the file says nothing.
    #[default]
    Cmd,
    /// Windows PowerShell, the one that ships with Windows.
    Powershell,
    /// PowerShell 7 and later, installed separately.
    Pwsh,
    /// `bash`, for the Git or MSYS installs that provide one.
    Bash,
}

impl Shell {
    /// The executable to spawn, without a path so `PATH` resolves it.
    pub fn program(self) -> &'static str {
        match self {
            Self::Cmd => "cmd.exe",
            Self::Powershell => "powershell.exe",
            Self::Pwsh => "pwsh.exe",
            Self::Bash => "bash.exe",
        }
    }

    /// The tail of the command line that runs `line`, for a shell that parses
    /// its own command line rather than an argument vector.
    ///
    /// `cmd.exe` and `powershell.exe` both take everything after their switch
    /// as text and apply their own quoting rules to it. Passing that text as a
    /// normal argument makes the caller quote it first, and neither shell
    /// unquotes it the way the C runtime would: `echo x > "C:\a b\c.txt"`
    /// arrives with the quotes escaped and the redirect goes nowhere. They get
    /// the line verbatim instead. `None` means the shell follows the usual
    /// argument rules and [`Shell::args_for`] is right.
    #[must_use]
    pub fn raw_tail(self, line: &str) -> Option<String> {
        match self {
            Self::Cmd => Some(format!("/C {line}")),
            Self::Powershell | Self::Pwsh => Some(format!("-NoProfile -Command {line}")),
            Self::Bash => None,
        }
    }

    /// The full argument vector that makes [`Shell::program`] run `line`.
    ///
    /// The line is passed as one argument on purpose: it is the user's own
    /// text, quoting and all, and splitting it here would change its meaning.
    pub fn args_for(self, line: &str) -> Vec<String> {
        let flags: &[&str] = match self {
            Self::Cmd => &["/C"],
            Self::Powershell | Self::Pwsh => &["-NoProfile", "-Command"],
            Self::Bash => &["-c"],
        };
        let mut args: Vec<String> = flags.iter().map(|&flag| flag.to_owned()).collect();
        args.push(line.to_owned());
        args
    }

    /// The spelling a `.shell` line uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Powershell => "powershell",
            Self::Pwsh => "pwsh",
            Self::Bash => "bash",
        }
    }

    /// Every shell, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Cmd, Self::Powershell, Self::Pwsh, Self::Bash];
}

impl std::fmt::Display for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Shell {
    /// A sentence naming the shells that would have worked.
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|shell| shell.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("`{s}` is not a shell, expected cmd, powershell, pwsh or bash"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_shells_get_their_command_line_verbatim() {
        // Quoting this text would be the caller's undoing: cmd applies its own
        // rules and a redirect into a quoted path is the case that shows it.
        let line = r#"echo ran > "C:\a b\c.txt""#;
        assert_eq!(Shell::Cmd.raw_tail(line).unwrap(), format!("/C {line}"));
        assert_eq!(
            Shell::Pwsh.raw_tail(line).unwrap(),
            format!("-NoProfile -Command {line}")
        );
        assert_eq!(
            Shell::Powershell.raw_tail(line).unwrap(),
            format!("-NoProfile -Command {line}")
        );
        // bash takes an argument vector like any other program.
        assert_eq!(Shell::Bash.raw_tail(line), None);
        assert_eq!(
            Shell::Bash.args_for(line),
            vec!["-c".to_owned(), line.to_owned()]
        );
    }
}
