//! Hotkey bindings for Mochi: parsing, key names and lookup.
//!
//! This crate is the half of Mochi's hotkey daemon that has no opinion about
//! Windows. It turns a hotkey file into a [`Bindings`] table and answers "what
//! does this key press do" with a hash lookup; installing the keyboard hook and
//! spawning processes happen elsewhere. Nothing here touches the system, so the
//! whole format is testable on any platform.
//!
//! A file is a `.shell` line, comments, and one binding per line:
//!
//! ```
//! use mochi_hotkey::{Action, Bindings, Trigger};
//! use mochi_client::{Command, Direction};
//!
//! let bindings = Bindings::parse("alt + h : mochic focus left").unwrap();
//! let trigger: Trigger = "alt + h".parse().unwrap();
//!
//! assert_eq!(bindings.len(), 1);
//! assert_eq!(
//!     bindings.get(trigger).map(|b| &b.action),
//!     Some(&Action::Command(Command::Focus { direction: Direction::Left })),
//! );
//! ```
//!
//! The grammar on the right of the `:` is the `mochic` grammar itself, taken
//! from `mochi-client`, so a binding cannot drift away from the command line it
//! was copied from.

#![deny(missing_docs)]

mod default;
mod key;
mod parse;
mod shell;
mod trigger;

pub use default::DEFAULT;
pub use key::Key;
pub use parse::{Action, Binding, Bindings, ParseError, ParseErrors, command_from_words};
pub use shell::Shell;
pub use trigger::{Modifiers, Trigger};
