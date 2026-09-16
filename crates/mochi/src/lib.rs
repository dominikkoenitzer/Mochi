//! The Mochi daemon, as a library.
//!
//! The binary in `main.rs` is only startup and shutdown order; everything else
//! lives here so that the tiling model in `mochi-core` can be wired in against
//! real types and so the Win32 layer is testable.
//!
//! * [`platform`] every Win32 call, behind one trait with a dry-run twin,
//! * [`events`] the producers: WinEvent hooks, a hidden message window, the mouse,
//! * [`ipc`] the named pipe server and the notification fan-out,
//! * [`state`] what the daemon knows,
//! * [`wm`] the single-threaded loop that owns it,
//! * [`safety`] the panic hook and the restore hook point,
//! * [`config`] path resolution and the file watcher,
//! * [`logging`], [`single_instance`], [`cli`] the boring but necessary parts.
//!
//! See `crates/mochi/README.md` for the module map and the hook points, and
//! `docs/ipc.md` for the protocol.

#![deny(missing_docs)]

pub mod cli;
pub mod config;
pub mod events;
pub mod ipc;
pub mod logging;
pub mod platform;
pub mod safety;
pub mod single_instance;
pub mod state;
pub mod wm;
