//! Wire types and a blocking client for the Mochi daemon.
//!
//! # Protocol
//!
//! The daemon listens on the named pipe `\\.\pipe\mochi` ([`PIPE_NAME`]) in byte
//! mode. The framing is newline delimited JSON: one JSON value per line, `\n`
//! terminated, UTF-8. See [`protocol`] for the reader and writer.
//!
//! A command exchange is one round trip:
//!
//! 1. the client connects to `\\.\pipe\mochi`,
//! 2. writes exactly one [`Command`] line,
//! 3. reads exactly one [`Response`] line,
//! 4. closes the connection.
//!
//! Subscriptions go the other way round. The subscriber creates its own pipe,
//! sends [`Command::SubscribePipe`], and the daemon connects to that pipe and
//! writes [`Notification`] lines until the pipe breaks. See [`subscribe`].
//!
//! ```no_run
//! use mochi_client::{Command, Direction, send};
//!
//! send(&Command::Focus { direction: Direction::Left })?;
//! # Ok::<_, mochi_client::Error>(())
//! ```

#![deny(missing_docs)]

mod command;
mod notification;
mod response;

pub mod protocol;

/// The `mochic` grammar, shared so a hotkey binding parses exactly like the CLI.
#[cfg(feature = "clap")]
pub mod cli;

#[cfg(windows)]
mod client;
#[cfg(windows)]
pub mod security;
#[cfg(windows)]
mod subscribe;

pub use command::{
    AnimationStyle, Axis, BooleanState, BorderStyle, Command, ContainerBehaviour, CycleDirection,
    Direction, HidingBehaviour, Layout, MatchingStrategy, MoveBehaviour, OperationBehaviour,
    ParseEnumError, QueryTarget, RuleIdentifier, Sizing, WindowKind,
};
pub use notification::{Notification, NotificationEvent, SessionChangeKind, WindowRef};
pub use response::Response;

#[cfg(windows)]
pub use client::{Error, PIPE_NAME, PIPE_PREFIX, Result, is_running, send, send_to};
#[cfg(windows)]
pub use subscribe::{Subscription, create_pipe, subscribe, validate_pipe_name};

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(cmd: Command) -> String {
        let json = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cmd, "round trip changed the command: {json}");
        json
    }

    #[test]
    fn unit_commands_are_a_bare_tag() {
        assert_eq!(round_trip(Command::State), r#"{"cmd":"state"}"#);
        assert_eq!(round_trip(Command::Retile), r#"{"cmd":"retile"}"#);
        assert_eq!(
            round_trip(Command::ReloadConfiguration),
            r#"{"cmd":"reload-configuration"}"#
        );
        assert_eq!(
            round_trip(Command::FocusLastWorkspace),
            r#"{"cmd":"focus-last-workspace"}"#
        );
        assert_eq!(
            round_trip(Command::ToggleTiling),
            r#"{"cmd":"toggle-tiling"}"#
        );
        assert_eq!(
            round_trip(Command::PromoteFocus),
            r#"{"cmd":"promote-focus"}"#
        );
        assert_eq!(
            round_trip(Command::ToggleFloatOverride),
            r#"{"cmd":"toggle-float-override"}"#
        );
    }

    #[test]
    fn whkdrc_commands_all_round_trip() {
        // One entry per distinct command bound in the user's whkdrc.
        let commands = [
            Command::Focus {
                direction: Direction::Left,
            },
            Command::Move {
                direction: Direction::Down,
            },
            Command::ResizeAxis {
                axis: Axis::Horizontal,
                sizing: Sizing::Increase,
            },
            Command::ToggleFloat,
            Command::ToggleMaximize,
            Command::ToggleMonocle,
            Command::Minimize,
            Command::Close,
            Command::CycleLayout {
                direction: CycleDirection::Next,
            },
            Command::FlipLayout {
                axis: Axis::Horizontal,
            },
            Command::CycleWorkspace {
                direction: CycleDirection::Previous,
            },
            Command::FocusLastWorkspace,
            Command::FocusWorkspace { index: 0 },
            Command::MoveToWorkspace { index: 8 },
            Command::TogglePause,
            Command::ReloadConfiguration,
            Command::Retile,
            Command::Stop,
        ];
        for cmd in commands {
            round_trip(cmd);
        }
    }

    #[test]
    fn payloads_sit_next_to_the_tag() {
        assert_eq!(
            round_trip(Command::ResizeAxis {
                axis: Axis::Vertical,
                sizing: Sizing::Decrease
            }),
            r#"{"cmd":"resize-axis","axis":"vertical","sizing":"decrease"}"#
        );
        assert_eq!(
            round_trip(Command::ResizeEdge {
                direction: Direction::Left,
                sizing: Sizing::Increase
            }),
            r#"{"cmd":"resize-edge","direction":"left","sizing":"increase"}"#
        );
        assert_eq!(
            round_trip(Command::FocusWorkspace { index: 3 }),
            r#"{"cmd":"focus-workspace","index":3}"#
        );
        assert_eq!(
            round_trip(Command::SendToWorkspace { index: 3 }),
            r#"{"cmd":"send-to-workspace","index":3}"#
        );
        assert_eq!(
            round_trip(Command::SendToMonitor { index: 1 }),
            r#"{"cmd":"send-to-monitor","index":1}"#
        );
        assert_eq!(
            round_trip(Command::WorkspacePadding {
                monitor: 1,
                workspace: 2,
                size: 14
            }),
            r#"{"cmd":"workspace-padding","monitor":1,"workspace":2,"size":14}"#
        );
    }

    #[test]
    fn optional_fields_have_defaults() {
        let cmd: Command = serde_json::from_str(r#"{"cmd":"stop"}"#).unwrap();
        assert_eq!(cmd, Command::Stop);

        let cmd: Command =
            serde_json::from_str(r#"{"cmd":"float-rule","identifier":"exe","id":"wt.exe"}"#)
                .unwrap();
        assert_eq!(
            cmd,
            Command::FloatRule {
                identifier: RuleIdentifier::Exe,
                id: "wt.exe".into(),
                matching_strategy: MatchingStrategy::Equals,
            }
        );

        let cmd: Command =
            serde_json::from_str(r#"{"cmd":"border-colour","r":255,"g":187,"b":223}"#).unwrap();
        assert_eq!(
            cmd,
            Command::BorderColour {
                kind: WindowKind::Single,
                r: 255,
                g: 187,
                b: 223,
            }
        );
    }

    #[test]
    fn every_command_name_matches_its_tag() {
        // A representative value for every variant, so `name()` cannot drift.
        let all = [
            Command::Start,
            Command::Stop,
            Command::Quickstart,
            Command::TogglePause,
            Command::ToggleGameMode,
            Command::ReloadConfiguration,
            Command::Retile,
            Command::State,
            Command::Query {
                target: QueryTarget::MonitorCount,
            },
            Command::Hotkeys,
            Command::Focus {
                direction: Direction::Up,
            },
            Command::CycleFocus {
                direction: CycleDirection::Next,
            },
            Command::Move {
                direction: Direction::Right,
            },
            Command::CycleMove {
                direction: CycleDirection::Previous,
            },
            Command::ResizeAxis {
                axis: Axis::Vertical,
                sizing: Sizing::Increase,
            },
            Command::ResizeEdge {
                direction: Direction::Left,
                sizing: Sizing::Decrease,
            },
            Command::Promote,
            Command::PromoteFocus,
            Command::ToggleFloat,
            Command::ToggleFloatOverride,
            Command::ToggleMaximize,
            Command::ToggleMonocle,
            Command::Minimize,
            Command::Close,
            Command::Manage,
            Command::Unmanage,
            Command::Stack {
                direction: Direction::Left,
            },
            Command::Unstack,
            Command::CycleStack {
                direction: CycleDirection::Next,
            },
            Command::CycleLayout {
                direction: CycleDirection::Next,
            },
            Command::ChangeLayout {
                layout: Layout::Bsp,
            },
            Command::FlipLayout {
                axis: Axis::Horizontal,
            },
            Command::ToggleTiling,
            Command::FocusWorkspace { index: 0 },
            Command::MoveToWorkspace { index: 0 },
            Command::SendToWorkspace { index: 0 },
            Command::CycleWorkspace {
                direction: CycleDirection::Next,
            },
            Command::FocusLastWorkspace,
            Command::WorkspacePadding {
                monitor: 0,
                workspace: 0,
                size: 0,
            },
            Command::ContainerPadding {
                monitor: 0,
                workspace: 0,
                size: 0,
            },
            Command::FocusMonitor { index: 0 },
            Command::MoveToMonitor { index: 0 },
            Command::SendToMonitor { index: 0 },
            Command::CycleMonitor {
                direction: CycleDirection::Next,
            },
            Command::FocusFollowsMouse {
                state: BooleanState::Enable,
            },
            Command::MouseFollowsFocus {
                state: BooleanState::Disable,
            },
            Command::SetHotkeys {
                state: BooleanState::Enable,
            },
            Command::ToggleTransparency,
            Command::Border {
                state: BooleanState::Enable,
            },
            Command::BorderWidth { width: 6 },
            Command::BorderOffset { offset: -1 },
            Command::BorderColour {
                kind: WindowKind::Single,
                r: 1,
                g: 2,
                b: 3,
            },
            Command::BorderStyle {
                style: BorderStyle::Rounded,
            },
            Command::Animation {
                state: BooleanState::Enable,
            },
            Command::AnimationDuration { duration: 250 },
            Command::AnimationStyle {
                style: AnimationStyle::EaseOutQuad,
            },
            Command::AnimationFps { fps: 60 },
            Command::FloatRule {
                identifier: RuleIdentifier::Exe,
                id: "a".into(),
                matching_strategy: MatchingStrategy::Equals,
            },
            Command::IgnoreRule {
                identifier: RuleIdentifier::Path,
                id: "a".into(),
                matching_strategy: MatchingStrategy::Contains,
            },
            Command::SubscribePipe { name: "bar".into() },
            Command::UnsubscribePipe { name: "bar".into() },
        ];

        assert_eq!(all.len(), 61, "add new variants to this list");
        let mut seen = std::collections::HashSet::new();
        for cmd in &all {
            let json: serde_json::Value = serde_json::to_value(cmd).unwrap();
            assert_eq!(json["cmd"], cmd.name(), "name() disagrees with serde");
            assert!(seen.insert(cmd.name()), "duplicate tag {}", cmd.name());
            let back: Command = serde_json::from_value(json).unwrap();
            assert_eq!(&back, cmd);
        }
    }

    #[test]
    fn wire_enums_parse_from_their_own_spelling() {
        for d in Direction::ALL {
            assert_eq!(d.as_str().parse::<Direction>().unwrap(), *d);
        }
        for l in Layout::ALL {
            assert_eq!(l.as_str().parse::<Layout>().unwrap(), *l);
            assert_eq!(
                serde_json::to_value(l).unwrap(),
                serde_json::Value::String(l.as_str().into()),
                "serde and as_str disagree for {l}"
            );
        }
        for s in MatchingStrategy::ALL {
            assert_eq!(
                serde_json::to_value(s).unwrap(),
                serde_json::Value::String(s.as_str().into())
            );
        }
        assert_eq!("LEFT".parse::<Direction>().unwrap(), Direction::Left);
        assert!("sideways".parse::<Direction>().is_err());
    }

    #[test]
    fn responses_round_trip() {
        let cases = [
            Response::Ok,
            Response::error("boom"),
            Response::State {
                state: serde_json::json!({"monitors": []}),
            },
            Response::Query {
                answer: serde_json::json!(2),
            },
            Response::Hotkeys {
                hotkeys: serde_json::json!([{"keys": "alt + h", "command": "focus left"}]),
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), case);
        }
        assert_eq!(
            serde_json::to_string(&Response::Ok).unwrap(),
            r#"{"response":"ok"}"#
        );
    }

    #[test]
    fn notifications_round_trip_and_flatten_the_event() {
        let n = Notification::new(NotificationEvent::Manage {
            window: WindowRef::new(123, "Terminal", "wt.exe"),
        });
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(
            json,
            r#"{"event":"manage","window":{"hwnd":123,"title":"Terminal","exe":"wt.exe"}}"#
        );
        assert_eq!(serde_json::from_str::<Notification>(&json).unwrap(), n);

        let n = Notification::new(NotificationEvent::MonitorsChanged { count: 2 })
            .with_state(serde_json::json!({"monitors": 2}));
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(serde_json::from_str::<Notification>(&json).unwrap(), n);
        assert!(json.contains(r#""event":"monitors-changed""#));

        let n = Notification::new(NotificationEvent::SessionChange {
            kind: SessionChangeKind::Lock,
        });
        assert_eq!(
            serde_json::to_string(&n).unwrap(),
            r#"{"event":"session-change","kind":"lock"}"#
        );
    }
}
