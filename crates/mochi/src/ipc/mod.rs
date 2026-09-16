//! The named pipe server and the subscriber fan-out.
//!
//! See `docs/ipc.md` for the wire protocol.

mod server;
mod subscribers;

pub use server::PipeServer;
pub use subscribers::Subscribers;
