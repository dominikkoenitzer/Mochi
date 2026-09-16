//! The thread pattern every visual uses.
//!
//! A visual owns exactly one thread. That thread creates the windows, pumps
//! their message loop and never blocks on the daemon. The daemon holds a
//! [`WorkerHandle`], drops a message in a channel and pokes the loop awake with
//! `PostThreadMessageW`; it never waits for a reply, so a wedged compositor
//! cannot stall tiling.
//!
//! Windows belong to the thread that created them, so every `HWND` in this
//! crate is created, painted and destroyed on the same thread, and the state
//! struct is dropped there too.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP,
    WM_QUIT,
};

use crate::{RenderError, Result};

/// Posted to the worker thread to say "there is something in your channel".
const WM_MOCHI_WAKE: u32 = WM_APP + 0x101;

/// A handle to a visual's thread.
///
/// Cloneable and shareable: the daemon can hand one to the animation callback
/// without any further locking. Dropping the last handle stops the thread and
/// takes its windows with it.
pub(crate) struct WorkerHandle<M> {
    name: &'static str,
    /// The `Sender` is behind a mutex only so that the handle is `Sync`; the
    /// lock is held for the length of one `send`.
    sender: Mutex<Sender<M>>,
    thread_id: u32,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl<M> WorkerHandle<M> {
    /// Queues a message and wakes the loop.
    ///
    /// # Errors
    ///
    /// [`RenderError::ThreadGone`] when the worker thread has stopped.
    pub(crate) fn send(&self, message: M) -> Result<()> {
        {
            let sender = self
                .sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sender
                .send(message)
                .map_err(|_| RenderError::ThreadGone(self.name))?;
        }
        self.wake()
    }

    /// Pokes the message loop so it drains the channel.
    fn wake(&self) -> Result<()> {
        // SAFETY: PostThreadMessageW only needs a thread id; a dead thread makes
        // it fail with an error rather than doing anything dangerous. The
        // message carries no pointers.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_MOCHI_WAKE, WPARAM(0), LPARAM(0))
                .map_err(|_| RenderError::ThreadGone(self.name))
        }
    }

    /// Stops the thread and waits for its windows to be destroyed.
    pub(crate) fn stop(&self) {
        let handle = {
            let mut join = self
                .join
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            join.take()
        };
        let Some(handle) = handle else { return };

        // SAFETY: posting WM_QUIT to a thread id is the documented way to end
        // another thread's message loop. If the thread is already gone the call
        // fails harmlessly and the join below returns at once.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if handle.join().is_err() {
            tracing::warn!("the {} thread panicked", self.name);
        }
    }
}

impl<M> Drop for WorkerHandle<M> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts a visual's thread.
///
/// `init` builds the thread's state (factories, window classes) on the new
/// thread, because Direct2D single threaded factories and window handles both
/// belong to the thread that made them. `handle` is called for every message,
/// on that same thread.
///
/// # Errors
///
/// [`RenderError::ThreadStart`] when the thread cannot be spawned or `init`
/// fails; the string carries the underlying error, because `windows::core::Error`
/// holds a COM pointer and cannot cross threads.
pub(crate) fn spawn_worker<M, S, I, H>(
    name: &'static str,
    init: I,
    mut handle: H,
) -> Result<WorkerHandle<M>>
where
    M: Send + 'static,
    S: 'static,
    I: FnOnce() -> Result<S> + Send + 'static,
    H: FnMut(&mut S, M) + Send + 'static,
{
    let (message_tx, message_rx) = channel::<M>();
    let (ready_tx, ready_rx) = channel::<std::result::Result<u32, String>>();

    let join = std::thread::Builder::new()
        .name(format!("mochi-{name}"))
        .spawn(move || {
            // A thread only gets a message queue once it has looked at it, and
            // PostThreadMessageW silently drops messages until then, so the
            // queue is forced into existence before the handle is handed out.
            let mut msg = MSG::default();
            // SAFETY: PeekMessageW with PM_NOREMOVE only inspects this thread's
            // own queue and writes into the live `msg` slot.
            unsafe {
                let _ = PeekMessageW(&mut msg, None, WM_APP, WM_APP, PM_NOREMOVE);
            }

            // SAFETY: GetCurrentThreadId has no arguments and cannot fail.
            let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };

            let mut state = match init() {
                Ok(state) => {
                    if ready_tx.send(Ok(thread_id)).is_err() {
                        return;
                    }
                    state
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error.to_string()));
                    return;
                }
            };

            run_loop(&message_rx, &mut state, &mut handle);
            drop(state);
        })
        .map_err(|error| RenderError::ThreadStart(name, error.to_string()))?;

    match ready_rx.recv() {
        Ok(Ok(thread_id)) => Ok(WorkerHandle {
            name,
            sender: Mutex::new(message_tx),
            thread_id,
            join: Mutex::new(Some(join)),
        }),
        Ok(Err(error)) => Err(RenderError::ThreadStart(name, error)),
        Err(_) => Err(RenderError::ThreadStart(
            name,
            "the thread died before it was ready".to_string(),
        )),
    }
}

/// The message loop: Win32 messages for the windows, channel messages for the
/// daemon's commands.
fn run_loop<M, S, H>(messages: &Receiver<M>, state: &mut S, handle: &mut H)
where
    H: FnMut(&mut S, M),
{
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a live stack slot; a null window filter means "every
        // window of this thread plus thread messages", which is what a worker
        // owning several windows wants.
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match result.0 {
            0 => break,  // WM_QUIT
            -1 => break, // the queue broke; nothing sensible is left
            _ if msg.message == WM_MOCHI_WAKE => drain(messages, state, handle),
            _ => {
                // SAFETY: the message was just filled in by GetMessageW.
                // TranslateMessage is skipped: no visual window takes keys.
                unsafe {
                    DispatchMessageW(&msg);
                }
            }
        }
    }

    // Anything queued behind the quit is dropped on purpose: the thread is
    // shutting down and its windows are about to go away.
}

/// Runs every queued command, coalescing bursts into one wake.
fn drain<M, S, H>(messages: &Receiver<M>, state: &mut S, handle: &mut H)
where
    H: FnMut(&mut S, M),
{
    loop {
        match messages.try_recv() {
            Ok(message) => handle(state, message),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
        }
    }
}
