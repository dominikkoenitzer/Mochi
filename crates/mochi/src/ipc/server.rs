//! The command pipe.
//!
//! One acceptor thread creates pipe instances and blocks in `ConnectNamedPipe`.
//! Every accepted connection gets its own short lived thread, so a client that
//! stalls mid-message cannot block the next one. Commands are handed to the
//! window manager loop through the shared channel together with a one-shot
//! reply channel, and the connection thread blocks on that until the loop
//! answers or the timeout expires.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use mochi_client::security::PipeSecurity;
use mochi_client::{Command, PIPE_NAME, Response, protocol};
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::IO::CancelIoEx;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe,
};
use windows::core::HSTRING;

use crate::events::{Event, EventSender, Reply, ShutdownReason};

/// Read and write buffer size for a pipe instance.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// `ERROR_PIPE_BUSY`: every instance of the pipe is already taken.
const ERROR_PIPE_BUSY: i32 = 231;
/// `ERROR_BROKEN_PIPE`: the peer hung up, which is not worth a log line.
const ERROR_BROKEN_PIPE: i32 = 109;

/// How long [`PipeServer::stop`] keeps trying to wake the acceptor thread.
const WAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// How many accepts may fail back to back before the acceptor gives up.
///
/// A single failure is ordinary: a client that connects and hangs up before
/// `ConnectNamedPipe` completes produces `ERROR_NO_DATA`. Only a run of them
/// with no successful accept in between means something is really wrong.
const MAX_CONSECUTIVE_ACCEPT_FAILURES: u32 = 64;

/// How long a connection thread waits for the loop before giving up.
///
/// The loop is single threaded, so a slow command delays every other client.
/// Five seconds is far longer than any operation should take and short enough
/// that a wedged daemon does not hang a hotkey.
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How many connections may be in flight at once.
///
/// One `mochic` invocation is one short lived connection, so the real number is
/// one or two. The cap is here because a connection costs a thread and three
/// handles and nothing else bounds it. Reaching it breaks the oldest connection
/// rather than refusing the new one: the new one might be `mochic stop`.
const MAX_CONNECTIONS: usize = 64;

/// How long a connection may sit without sending anything before it is closed.
///
/// A client that has nothing to say has no reason to hold a slot, and the ones
/// that do are answered in microseconds.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Longest nap between two checks for input on an idle connection.
///
/// The wait starts far shorter than this and backs off, so the usual case of a
/// client that writes its command straight after connecting is not delayed.
const MAX_IDLE_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// The acceptor thread.
pub struct PipeServer {
    pipe: String,
    stop: Arc<AtomicBool>,
    connections: Arc<Connections>,
    handle: Option<JoinHandle<()>>,
}

/// One accepted connection: the pipe instance and the thread serving it.
struct Connection {
    pipe: Arc<File>,
    worker: Option<JoinHandle<()>>,
}

/// Every connection currently being served, oldest first.
///
/// The acceptor used to spawn a detached thread per connection and forget it,
/// so an idle client cost a thread and three handles that nothing ever
/// reclaimed, and `stop` joined the acceptor only. Holding them here is what
/// makes both the cap and the reclaim at shutdown possible.
#[derive(Default)]
struct Connections {
    live: Mutex<BTreeMap<u64, Connection>>,
    next_id: AtomicU64,
}

impl Connections {
    /// Takes ownership of a connection and returns its id.
    ///
    /// If that puts the server over [`MAX_CONNECTIONS`], the oldest connection
    /// is broken so its worker winds up. The new one is always served.
    fn register(&self, pipe: Arc<File>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let evict = {
            let mut live = lock(&self.live);
            live.insert(id, Connection { pipe, worker: None });
            if live.len() > MAX_CONNECTIONS {
                // The new id is the largest, so this is never the new one.
                live.keys()
                    .next()
                    .copied()
                    .and_then(|oldest| live.get(&oldest).map(|c| (oldest, Arc::clone(&c.pipe))))
            } else {
                None
            }
        };
        if let Some((oldest, pipe)) = evict {
            tracing::debug!(
                connection = oldest,
                "too many connections, closing the oldest"
            );
            break_connection(&pipe);
        }
        id
    }

    /// Records the worker thread, unless it already finished.
    fn attach(&self, id: u64, worker: JoinHandle<()>) {
        if let Some(connection) = lock(&self.live).get_mut(&id) {
            connection.worker = Some(worker);
        }
    }

    /// Called by a worker on its way out.
    fn finished(&self, id: u64) {
        lock(&self.live).remove(&id);
    }

    /// Breaks every live connection and joins every worker.
    fn shutdown(&self) {
        // Drained first so the lock is free while the workers wind up: a worker
        // needs it to take itself out of the map.
        let drained = std::mem::take(&mut *lock(&self.live));
        for connection in drained.values() {
            break_connection(&connection.pipe);
        }
        for (id, mut connection) in drained {
            if let Some(worker) = connection.worker.take()
                && worker.join().is_err()
            {
                tracing::error!(connection = id, "a connection thread panicked");
            }
        }
    }
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Wakes a worker that is parked on a connection and closes the client out.
///
/// `CancelIoEx` is what releases a read that is already blocked; the disconnect
/// makes sure nothing else can be read afterwards.
fn break_connection(pipe: &File) {
    let handle = raw(pipe);
    let _ = unsafe { CancelIoEx(handle, None) };
    let _ = unsafe { DisconnectNamedPipe(handle) };
}

impl PipeServer {
    /// Creates the first pipe instance on `\\.\pipe\mochi` and starts accepting.
    ///
    /// Fails if another daemon already owns the pipe.
    pub fn start(tx: EventSender) -> Result<Self> {
        Self::start_on(PIPE_NAME, tx)
    }

    /// Same as [`PipeServer::start`] on an explicit pipe path, for tests.
    pub fn start_on(pipe: &str, tx: EventSender) -> Result<Self> {
        // `FILE_FLAG_FIRST_PIPE_INSTANCE` is what actually proves the name is
        // free: without it any process may create further instances of an
        // existing pipe and take turns answering clients. With it the call
        // fails with ERROR_ACCESS_DENIED when somebody else got there first.
        let first = create_instance(pipe, true).context("could not create the command pipe")?;

        let stop = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(Connections::default());
        let handle = {
            let stop = Arc::clone(&stop);
            let connections = Arc::clone(&connections);
            let pipe = pipe.to_owned();
            std::thread::Builder::new()
                .name("mochi-ipc".into())
                .spawn(move || accept_loop(&pipe, first, &tx, &stop, &connections))
                .context("could not spawn the IPC thread")?
        };

        tracing::info!(pipe, "listening");
        Ok(Self {
            pipe: pipe.to_owned(),
            stop,
            connections,
            handle: Some(handle),
        })
    }

    /// The pipe path being served.
    pub fn pipe(&self) -> &str {
        &self.pipe
    }

    /// Stops accepting and joins the thread.
    ///
    /// The acceptor is parked in a blocking `ConnectNamedPipe`, so it is woken
    /// by connecting to our own pipe once and hanging up again. That connect
    /// can lose a race against a real client and come back `ERROR_PIPE_BUSY`,
    /// in which case the acceptor would still be parked and the join below
    /// would never return, so it is retried until it lands.
    pub fn stop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        self.stop.store(true, Ordering::SeqCst);
        if !self.wake_acceptor() {
            tracing::warn!(
                pipe = self.pipe,
                "could not wake the IPC acceptor, shutdown may wait for the next client"
            );
        }
        if handle.join().is_err() {
            tracing::error!("the IPC thread panicked");
        } else {
            tracing::debug!("command pipe closed");
        }
        // The acceptor is only half of it: every connection thread holds an
        // `EventSender` clone, so leaving them parked would keep the event
        // channel open and the daemon alive.
        self.connections.shutdown();
    }

    /// Connects to our own pipe once so the blocking accept returns.
    ///
    /// Returns false only if the pipe could not be reached at all within
    /// [`WAKE_TIMEOUT`], which on a live daemon means the acceptor is gone.
    fn wake_acceptor(&self) -> bool {
        let deadline = std::time::Instant::now() + WAKE_TIMEOUT;
        loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.pipe)
            {
                // Dropped straight away: the worker sees end of stream and exits.
                Ok(_) => return true,
                // The pipe is already gone, so the acceptor is not parked in it.
                Err(e) if matches!(e.raw_os_error(), Some(2) | Some(3)) => return true,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => {
                    tracing::debug!(error = %e, "waking the IPC acceptor failed");
                    return false;
                }
            }
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Creates one instance of the command pipe.
///
/// `first` must be true for exactly the very first instance: that is the one
/// that claims the name. Passing it for a later instance fails, which is the
/// documented behaviour of `FILE_FLAG_FIRST_PIPE_INSTANCE`.
///
/// The pipe carries an explicit security descriptor. The default one grants
/// read access to `Everyone` and the anonymous account, which would let any
/// other session on the machine occupy instances of a per-user daemon's
/// control channel. See [`mochi_client::security`].
fn create_instance(pipe: &str, first: bool) -> Result<File> {
    let security = PipeSecurity::current_user_only()
        .context("could not build the pipe security descriptor")?;
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let handle = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(pipe),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(security.attributes()),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("CreateNamedPipeW failed");
    }
    // SAFETY: the handle is fresh and owned by us from here on.
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

fn raw(file: &File) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    HANDLE(file.as_raw_handle())
}

fn accept_loop(
    pipe: &str,
    first: File,
    tx: &EventSender,
    stop: &Arc<AtomicBool>,
    connections: &Arc<Connections>,
) {
    let mut instance = first;
    // A run of failures with nothing in between would be a hot loop, so the
    // acceptor gives up after this many in a row. One success resets it.
    let mut failures = 0u32;
    loop {
        // ERROR_PIPE_CONNECTED means a client got in before we asked, which is
        // a success, not a failure. ERROR_NO_DATA means it connected and hung
        // up again before we got here, which is the same thing one step later:
        // the instance is used up and has to be recycled, but nothing is wrong.
        match unsafe { ConnectNamedPipe(raw(&instance), None) } {
            Ok(()) => failures = 0,
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => failures = 0,
            Err(e) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                // Recycle the instance rather than tearing the server down: one
                // rude client must never cost the daemon its control channel,
                // because `mochic stop` is how the desktop gets restored.
                failures += 1;
                tracing::debug!(error = %e, failures, "ConnectNamedPipe failed, recycling");
                let _ = unsafe { DisconnectNamedPipe(raw(&instance)) };
                if failures >= MAX_CONSECUTIVE_ACCEPT_FAILURES {
                    tracing::error!(failures, "the IPC acceptor gave up");
                    // Tell the loop, do not just stop listening. Breaking on
                    // its own left the daemon alive and tiling with no pipe,
                    // so every `mochic` call answered "mochi is not running"
                    // and the restore path was unreachable: the user's only
                    // way out was Task Manager, which skips the restore
                    // entirely and leaves every hidden window hidden.
                    let _ = tx.send(Event::Shutdown(ShutdownReason::IpcLost));
                    break;
                }
                continue;
            }
        }

        if stop.load(Ordering::SeqCst) {
            let _ = unsafe { DisconnectNamedPipe(raw(&instance)) };
            break;
        }

        // Hand the connected instance to a worker and immediately create the
        // next one, so the pipe name never disappears between clients. Only the
        // first instance may claim the name, hence `false` here.
        let connected = Arc::new(instance);
        match create_instance(pipe, false) {
            Ok(next) => instance = next,
            Err(e) => {
                tracing::error!(error = %e, "could not create the next pipe instance");
                serve(&connected, tx.clone(), stop);
                // Same reasoning as the give-up above: the pipe name is gone
                // the moment this thread returns, so the loop has to be told.
                let _ = tx.send(Event::Shutdown(ShutdownReason::IpcLost));
                break;
            }
        }

        let id = connections.register(Arc::clone(&connected));
        let tx = tx.clone();
        let stop = Arc::clone(stop);
        let owner = Arc::clone(connections);
        match std::thread::Builder::new()
            .name("mochi-ipc-conn".into())
            .spawn(move || {
                serve(&connected, tx, &stop);
                owner.finished(id);
            }) {
            Ok(worker) => connections.attach(id, worker),
            Err(e) => {
                tracing::error!(error = %e, "could not spawn a connection thread");
                connections.finished(id);
            }
        }
    }
}

/// Waits until the connection has something to say.
///
/// Returns false when the server is stopping, or when the client has sat there
/// for [`IDLE_TIMEOUT`] without writing anything: both mean the connection
/// should be closed. Blocking straight in the read instead is what used to park
/// a worker thread for as long as a silent client kept its handle open.
fn wait_for_input(reader: &BufReader<File>, pipe: &File, stop: &AtomicBool) -> bool {
    if !reader.buffer().is_empty() {
        return true;
    }
    let deadline = std::time::Instant::now() + IDLE_TIMEOUT;
    let mut nap = std::time::Duration::from_millis(1);
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let mut available = 0u32;
        // An error means the connection is broken or already disconnected. The
        // read reports that better than this can, so let it.
        if unsafe { PeekNamedPipe(raw(pipe), None, 0, None, Some(&mut available), None) }.is_err() {
            return true;
        }
        if available > 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            tracing::debug!("closing a connection that never said anything");
            return false;
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(MAX_IDLE_POLL);
    }
}

/// Serves one connection until the client hangs up or goes quiet.
fn serve(pipe: &File, tx: EventSender, stop: &AtomicBool) {
    let reader_file = match pipe.try_clone() {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "could not clone a pipe handle");
            return;
        }
    };
    let mut reader = BufReader::new(reader_file);
    let mut writer = pipe;

    loop {
        if !wait_for_input(&reader, pipe, stop) {
            break;
        }
        let command = match protocol::read_message::<_, Command>(&mut reader) {
            Ok(None) => break,
            Ok(Some(c)) => c,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let _ = protocol::write_message(&mut writer, &Response::error(e.to_string()));
                continue;
            }
            Err(e) => {
                // A client that hung up is not worth a log line.
                if e.raw_os_error() != Some(ERROR_BROKEN_PIPE) {
                    tracing::debug!(error = %e, "pipe read failed");
                }
                break;
            }
        };

        tracing::debug!(command = command.name(), "ipc command");
        let (reply, rx) = Reply::channel();
        if tx.send(Event::command(command, reply)).is_err() {
            let _ =
                protocol::write_message(&mut writer, &Response::error("mochi is shutting down"));
            break;
        }

        let response = match rx.recv_timeout(REPLY_TIMEOUT) {
            Ok(r) => r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Response::error("mochi did not answer within five seconds")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Response::error("mochi is shutting down")
            }
        };
        if protocol::write_message(&mut writer, &response).is_err() {
            break;
        }
    }

    // Let the client drain the last answer before the handle closes.
    let _ = unsafe { FlushFileBuffers(raw(pipe)) };
    let _ = unsafe { DisconnectNamedPipe(raw(pipe)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use mochi_client::{Response, send_to};

    /// Answers every command with `Ok` so the transport can be tested alone.
    fn echo_loop(rx: std::sync::mpsc::Receiver<Event>) -> JoinHandle<Vec<String>> {
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            while let Ok(event) = rx.recv() {
                if let Event::Command { command, reply } = event {
                    seen.push(command.name().to_owned());
                    let response = if matches!(*command, mochi_client::Command::State) {
                        Response::State {
                            state: serde_json::json!({"windows": 0}),
                        }
                    } else if matches!(*command, mochi_client::Command::Promote) {
                        Response::error("nope")
                    } else {
                        Response::Ok
                    };
                    reply.send(response);
                }
            }
            seen
        })
    }

    #[test]
    fn a_command_goes_out_and_a_response_comes_back() {
        let pipe = format!(r"\\.\pipe\mochi-test-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let loop_handle = echo_loop(rx);
        let mut server = PipeServer::start_on(&pipe, tx.clone()).unwrap();
        assert_eq!(server.pipe(), pipe);

        assert_eq!(
            send_to(&pipe, &mochi_client::Command::Retile).unwrap(),
            Response::Ok
        );

        match send_to(&pipe, &mochi_client::Command::State).unwrap() {
            Response::State { state } => assert_eq!(state["windows"], 0),
            other => panic!("unexpected response: {other:?}"),
        }

        // An error response becomes a client-side error, not a panic.
        let err = send_to(&pipe, &mochi_client::Command::Promote).unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");

        server.stop();
        drop(tx);
        let seen = loop_handle.join().unwrap();
        assert_eq!(seen, ["retile", "state", "promote"]);

        // The pipe is gone once the server stopped.
        assert!(matches!(
            send_to(&pipe, &mochi_client::Command::Retile).unwrap_err(),
            mochi_client::Error::NotRunning
        ));
    }

    #[test]
    fn a_second_server_cannot_squat_the_same_pipe_name() {
        let pipe = format!(r"\\.\pipe\mochi-test-squat-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let loop_handle = echo_loop(rx);
        let mut first = PipeServer::start_on(&pipe, tx.clone()).unwrap();

        let second = PipeServer::start_on(&pipe, tx.clone());
        assert!(
            second.is_err(),
            "a second listener on the same name must be refused"
        );

        first.stop();
        drop(tx);
        loop_handle.join().unwrap();
    }

    /// Talks to the server the way a hand written client would: raw bytes in,
    /// raw lines out, no help from `mochi_client`.
    fn raw(pipe: &str) -> File {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(pipe)
            {
                Ok(f) => return f,
                // Busy means every instance is taken; not-found means the
                // acceptor has not finished creating the next one yet. Both are
                // transient and both are what a real client retries through.
                Err(e) if matches!(e.raw_os_error(), Some(ERROR_PIPE_BUSY) | Some(2)) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "{pipe} never became connectable: {e}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("could not open {pipe}: {e}"),
            }
        }
    }

    #[test]
    fn rude_clients_do_not_stall_or_kill_the_server() {
        use std::io::{BufRead, Write};

        let pipe = format!(r"\\.\pipe\mochi-test-rude-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let loop_handle = echo_loop(rx);
        let mut server = PipeServer::start_on(&pipe, tx.clone()).unwrap();

        // 1. Connects and sends nothing, then hangs up.
        drop(raw(&pipe));

        // 2. Sends half a line and hangs up without ever finishing it.
        {
            let mut client = raw(&pipe);
            client.write_all(br#"{"cmd":"sta"#).unwrap();
            client.flush().unwrap();
        }

        // 3. Sends invalid JSON, gets an error, and keeps using the connection.
        {
            let mut client = raw(&pipe);
            let reader_half = client.try_clone().unwrap();
            let mut reader = BufReader::new(reader_half);
            client.write_all(b"\n\nnot json at all\n").unwrap();
            client.flush().unwrap();

            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let response: Response = serde_json::from_str(line.trim()).unwrap();
            assert!(
                response
                    .error_message()
                    .is_some_and(|m| m.contains("malformed message")),
                "{response:?}"
            );

            // Same connection, now a valid command.
            client.write_all(b"{\"cmd\":\"retile\"}\n").unwrap();
            client.flush().unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Response>(line.trim()).unwrap(),
                Response::Ok
            );
        }

        // 4. Disconnects in the middle of a response: write the command, then
        //    drop the handle without ever reading the answer.
        {
            let mut client = raw(&pipe);
            client.write_all(b"{\"cmd\":\"state\"}\n").unwrap();
            client.flush().unwrap();
        }

        // 5. Several clients at once, all answered.
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let pipe = pipe.clone();
                std::thread::spawn(move || send_to(&pipe, &mochi_client::Command::Retile).unwrap())
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), Response::Ok);
        }

        // After all of that the server is still healthy and still stops cleanly.
        assert_eq!(
            send_to(&pipe, &mochi_client::Command::Retile).unwrap(),
            Response::Ok
        );
        server.stop();
        drop(tx);
        // Not joined: a connection worker may still hold a sender clone, and
        // this test is about the server, not about the echo loop winding down.
        drop(loop_handle);
    }

    /// Idle connections must not pile up, and shutdown must take them with it.
    ///
    /// Every worker holds an `EventSender` clone, so the echo loop can only
    /// finish once every worker is gone. Joining it after `stop()` is therefore
    /// the honest measurement of whether the workers were reclaimed at all.
    #[test]
    fn idle_clients_are_capped_and_reclaimed_at_shutdown() {
        let pipe = format!(r"\\.\pipe\mochi-test-idle-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let loop_handle = echo_loop(rx);
        let mut server = PipeServer::start_on(&pipe, tx.clone()).unwrap();

        // Comfortably past the cap: connect, then never say anything.
        let idle: Vec<File> = (0..MAX_CONNECTIONS * 2 + 16).map(|_| raw(&pipe)).collect();

        // `mochic stop` has to get through no matter how many of these there
        // are, so the control channel is checked while they all sit there.
        assert_eq!(
            send_to(&pipe, &mochi_client::Command::Retile).unwrap(),
            Response::Ok,
            "idle clients locked the control channel out"
        );

        server.stop();
        drop(tx);

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done_tx.send(loop_handle.join().is_ok());
        });
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap_or(false),
            "connection workers were never reclaimed: each one still holds an event sender"
        );
        drop(idle);
    }

    #[test]
    fn a_client_that_connects_and_never_speaks_does_not_block_shutdown() {
        let pipe = format!(r"\\.\pipe\mochi-test-silent-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let loop_handle = echo_loop(rx);
        let mut server = PipeServer::start_on(&pipe, tx.clone()).unwrap();

        // Held open for the whole test: the worker thread is parked in a read.
        let silent = raw(&pipe);
        assert_eq!(
            send_to(&pipe, &mochi_client::Command::Retile).unwrap(),
            Response::Ok,
            "a silent client must not block the next one"
        );

        let started = std::time::Instant::now();
        server.stop();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "stop waited on the silent client"
        );

        drop(silent);
        drop(tx);
        drop(loop_handle);
    }
}
