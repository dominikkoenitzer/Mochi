//! A real, complete hotkey file, parsed end to end.
//!
//! The fixture is the file this project's author runs every day, so a change
//! that would break a working desktop breaks this test first.
//!
//! The fixture next door is his file line for line, only the comments are
//! reworded. It is here so a change to the parser, the key table or the shared
//! `mochic` grammar cannot quietly break the keyboard he uses every day.

use mochi_client::{Command, CycleDirection, Direction, Sizing};
use mochi_hotkey::{Action, Bindings, Shell, Trigger};

/// His file, byte for byte apart from the comments.
const HOTKEYS: &str = include_str!("fixtures/hotkeys.conf");

fn bindings() -> Bindings {
    match Bindings::parse(HOTKEYS) {
        Ok(bindings) => bindings,
        Err(errors) => panic!("the fixture should parse:\n{errors}"),
    }
}

fn trigger(text: &str) -> Trigger {
    text.parse().expect("the test wrote a bad trigger")
}

fn action(text: &str) -> Action {
    bindings()
        .get(trigger(text))
        .unwrap_or_else(|| panic!("nothing is bound to {text}"))
        .action
        .clone()
}

fn command(text: &str) -> Command {
    match action(text) {
        Action::Command(command) => command,
        other => panic!("{text} is {other:?}, not a Mochi command"),
    }
}

#[test]
fn the_real_file_parses_to_every_binding_it_holds() {
    let bindings = bindings();
    assert_eq!(bindings.len(), 56);
    assert!(!bindings.is_empty());
    assert_eq!(bindings.shell(), Shell::Cmd);
}

#[test]
fn the_file_has_as_many_bindings_as_it_has_binding_lines() {
    let lines = HOTKEYS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('.'))
        .count();
    assert_eq!(bindings().len(), lines);
}

#[test]
fn alt_shift_q_closes_the_focused_window() {
    assert_eq!(command("alt + shift + q"), Command::Close);
}

#[test]
fn alt_3_focuses_the_third_workspace_counting_from_zero() {
    assert_eq!(command("alt + 3"), Command::FocusWorkspace { index: 2 });
}

#[test]
fn game_mode_is_a_command_rather_than_a_script() {
    assert_eq!(command("alt + shift + g"), Command::ToggleGameMode);
}

#[test]
fn the_terminal_launcher_is_a_shell_line() {
    // `start` is also the name of a subcommand, and this line is proof that a
    // right-hand side that does not parse as one falls through to the shell.
    assert_eq!(
        action("alt + return"),
        Action::Shell {
            shell: Shell::Cmd,
            line: "start wt".to_owned(),
        }
    );
    assert_eq!(
        Shell::Cmd.args_for("start wt"),
        vec!["/C".to_owned(), "start wt".to_owned()]
    );
}

#[test]
fn the_vim_keys_and_the_arrows_do_the_same_thing() {
    for (letter, arrow) in [("h", "left"), ("j", "down"), ("k", "up"), ("l", "right")] {
        assert_eq!(
            command(&format!("alt + {letter}")),
            command(&format!("alt + {arrow}")),
            "alt + {letter} and alt + {arrow} should agree"
        );
        assert_eq!(
            command(&format!("alt + shift + {letter}")),
            command(&format!("alt + shift + {arrow}")),
        );
    }
}

#[test]
fn every_workspace_number_is_bound_twice_over() {
    for number in 1..=9usize {
        assert_eq!(
            command(&format!("alt + {number}")),
            Command::FocusWorkspace { index: number - 1 }
        );
        assert_eq!(
            command(&format!("alt + shift + {number}")),
            Command::MoveToWorkspace { index: number - 1 }
        );
    }
}

#[test]
fn the_resize_keys_cover_both_axes_in_both_directions() {
    use mochi_client::Axis::{Horizontal, Vertical};

    let cases = [
        ("alt + u", Horizontal, Sizing::Decrease),
        ("alt + p", Horizontal, Sizing::Increase),
        ("alt + i", Vertical, Sizing::Decrease),
        ("alt + o", Vertical, Sizing::Increase),
    ];
    for (key, axis, sizing) in cases {
        assert_eq!(
            command(key),
            Command::ResizeAxis { axis, sizing },
            "for {key}"
        );
    }
}

#[test]
fn the_window_manager_controls_are_all_there() {
    assert_eq!(command("alt + shift + p"), Command::TogglePause);
    assert_eq!(command("alt + shift + r"), Command::ReloadConfiguration);
    assert_eq!(command("alt + shift + w"), Command::Retile);
    assert_eq!(command("alt + shift + e"), Command::Stop);
}

#[test]
fn the_window_state_and_layout_keys_are_all_there() {
    assert_eq!(command("alt + t"), Command::ToggleFloat);
    assert_eq!(command("alt + shift + space"), Command::ToggleFloat);
    assert_eq!(command("alt + f"), Command::ToggleMaximize);
    assert_eq!(command("alt + shift + f"), Command::ToggleMonocle);
    assert_eq!(command("alt + m"), Command::Minimize);
    assert_eq!(
        command("alt + v"),
        Command::CycleLayout {
            direction: CycleDirection::Next
        }
    );
    assert_eq!(command("alt + d"), Command::FocusLastWorkspace);
    assert_eq!(
        command("alt + a"),
        Command::CycleWorkspace {
            direction: CycleDirection::Previous
        }
    );
}

#[test]
fn every_binding_reports_the_line_it_was_read_from() {
    let bindings = bindings();
    let lines: Vec<usize> = bindings.iter().map(|binding| binding.line).collect();
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted, "iteration should follow the file");
    assert_eq!(bindings.get(trigger("alt + h")).unwrap().line, 6);
    assert_eq!(bindings.get(trigger("alt + return")).unwrap().line, 82);
}

#[test]
fn every_binding_keeps_the_text_it_was_written_as() {
    for binding in bindings().iter() {
        assert!(!binding.source.is_empty());
        assert_eq!(binding.source.trim(), binding.source);
        assert!(!binding.source.contains('#'), "{}", binding.source);
    }
    assert_eq!(
        bindings().get(trigger("alt + h")).unwrap().source,
        "mochic focus left"
    );
}

#[test]
fn nothing_in_the_file_is_bound_twice() {
    let bindings = bindings();
    for binding in bindings.iter() {
        assert_eq!(
            bindings.get(binding.trigger).map(|found| found.line),
            Some(binding.line)
        );
    }
}

#[test]
fn the_same_file_written_without_the_mochic_prefix_parses_the_same_way() {
    let stripped = HOTKEYS.replace(": mochic ", ": ");
    let plain = Bindings::parse(&stripped).expect("the prefix is optional");
    let original = bindings();

    assert_eq!(plain.len(), original.len());
    for binding in original.iter() {
        let Action::Command(ref expected) = binding.action else {
            continue;
        };
        assert_eq!(
            plain.get(binding.trigger).map(|found| &found.action),
            Some(&Action::Command(expected.clone())),
            "for {}",
            binding.trigger
        );
    }
}

#[test]
fn a_direction_still_reaches_the_daemon_as_a_direction() {
    assert_eq!(
        command("alt + l"),
        Command::Focus {
            direction: Direction::Right
        }
    );
    assert_eq!(
        command("alt + shift + k"),
        Command::Move {
            direction: Direction::Up
        }
    );
}
