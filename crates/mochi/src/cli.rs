//! Command line arguments of the daemon.

use std::path::PathBuf;

use clap::Parser;

/// `mochi`, the window manager daemon.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "mochi",
    version,
    about = "The Mochi tiling window manager daemon",
    long_about = "Runs the Mochi window manager. Started for you by `mochic start`; \
                  run it directly to watch the log."
)]
pub struct Args {
    /// Log every window write instead of performing it.
    ///
    /// Monitors and windows are still enumerated and events are still logged,
    /// so this is safe to run next to another window manager.
    #[arg(long)]
    pub dry_run: bool,

    /// Configuration file to use.
    ///
    /// Defaults to $MOCHI_CONFIG, then %USERPROFILE%\mochi.json.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Hotkey file to bind.
    ///
    /// Defaults to $MOCHI_HOTKEYS, then %USERPROFILE%\.config\mochi\hotkeys,
    /// then a whkdrc in that same directory.
    #[arg(long, value_name = "PATH")]
    pub hotkeys: Option<PathBuf>,

    /// Bind no keys at all.
    ///
    /// For a desktop that drives Mochi from a separate hotkey daemon, and for
    /// a second Mochi started to look at something while the real one runs.
    #[arg(long, conflicts_with = "hotkeys")]
    pub no_hotkeys: bool,

    /// Manage only windows of this class, and manage them even though they
    /// are tool windows.
    ///
    /// May be repeated. This is the testbed switch: with it Mochi takes over
    /// exactly the given classes and leaves every other window on the desktop
    /// completely alone, which is what makes an end-to-end tiling run safe
    /// next to another window manager and the user's real applications. The
    /// tool window rejection is the only manageability rule it overrides.
    #[arg(long, value_name = "CLASS")]
    pub manage_class: Vec<String>,
}

impl Args {
    /// Parses the process arguments.
    pub fn get() -> Self {
        Self::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_argument_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn dry_run_and_config_are_parsed() {
        let args = Args::try_parse_from(["mochi", "--dry-run", "--config", r"D:\x.json"]).unwrap();
        assert!(args.dry_run);
        assert_eq!(args.config.unwrap(), PathBuf::from(r"D:\x.json"));

        let args = Args::try_parse_from(["mochi"]).unwrap();
        assert!(!args.dry_run);
        assert!(args.config.is_none());
        assert!(args.manage_class.is_empty());
    }

    #[test]
    fn the_hotkey_file_can_be_named_or_turned_off() {
        let args = Args::try_parse_from(["mochi", "--hotkeys", r"D:\keys"]).unwrap();
        assert_eq!(args.hotkeys.unwrap(), PathBuf::from(r"D:\keys"));
        assert!(!args.no_hotkeys);

        let args = Args::try_parse_from(["mochi", "--no-hotkeys"]).unwrap();
        assert!(args.no_hotkeys);
        assert!(args.hotkeys.is_none());

        // Naming a file and refusing to bind keys at the same time is a
        // contradiction, and clap says so rather than picking one.
        assert!(Args::try_parse_from(["mochi", "--no-hotkeys", "--hotkeys", r"D:\keys"]).is_err());
    }

    #[test]
    fn manage_class_may_be_repeated() {
        let args = Args::try_parse_from([
            "mochi",
            "--manage-class",
            "MochiTestWindow",
            "--manage-class",
            "OtherClass",
        ])
        .unwrap();
        assert_eq!(args.manage_class, ["MochiTestWindow", "OtherClass"]);
    }
}
