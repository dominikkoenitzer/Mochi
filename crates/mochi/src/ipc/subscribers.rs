//! Notification fan-out to subscriber pipes.
//!
//! Subscribers create their own pipe and the daemon connects to it as a client,
//! so a bar can be started in any order relative to the daemon. Writing to a
//! pipe whose reader is not draining blocks, which is why the fan-out lives on
//! its own thread: a wedged bar must never stall the window manager loop.
//!
//! Its own thread is not enough by itself. A blocking write stalls the fan-out
//! as well, which means every other subscriber stops hearing anything and
//! [`Subscribers::stop`] never returns, so the daemon never exits. The writes
//! are therefore overlapped and bounded by [`WRITE_TIMEOUT`]: a subscriber that
//! has not taken its notification by then is cancelled and dropped.

use std::collections::HashMap;
use std::fs::File;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use mochi_client::{Notification, PIPE_PREFIX, protocol};
use windows::Win32::Foundation::{ERROR_IO_PENDING, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_OVERLAPPED, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

/// How long one subscriber gets to accept one notification.
///
/// A subscriber pipe buffers 64 KiB, so several hundred notifications have to
/// be sitting unread before a write blocks at all. A bar that far behind that
/// still has not moved after this long is not coming back, and the point of the
/// timeout is that finding that out is bounded.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

enum Message {
    Add(String, Pipe),
    Remove(String),
    Notify(Box<Notification>),
}

/// One subscriber connection plus the event its overlapped writes complete on.
struct Pipe {
    file: File,
    event: OwnedHandle,
}

impl Pipe {
    fn handle(&self) -> HANDLE {
        HANDLE(self.file.as_raw_handle())
    }

    /// Writes one framed line, giving up after [`WRITE_TIMEOUT`].
    ///
    /// The pipe is opened with `FILE_FLAG_OVERLAPPED`, so this is the only way
    /// it may be written: an ordinary `write_all` on such a handle would be
    /// both wrong and, against a subscriber that stopped reading, unbounded.
    fn write(&self, line: &[u8]) -> std::io::Result<()> {
        let handle = self.handle();
        let event = HANDLE(self.event.as_raw_handle());
        unsafe { ResetEvent(event) }.map_err(std::io::Error::other)?;
        let mut overlapped = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        // SAFETY: `line` and `overlapped` both outlive the operation. Every
        // path out of here either waits for the write to complete or cancels it
        // and then waits for the cancellation to land, so the kernel is never
        // left holding a pointer to either of them.
        let started = unsafe { WriteFile(handle, Some(line), None, Some(&mut overlapped)) };
        match started {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                let wait = unsafe { WaitForSingleObject(event, WRITE_TIMEOUT.as_millis() as u32) };
                if wait != WAIT_OBJECT_0 {
                    let _ = unsafe { CancelIoEx(handle, Some(&overlapped)) };
                    let mut cancelled = 0u32;
                    let _ =
                        unsafe { GetOverlappedResult(handle, &overlapped, &mut cancelled, true) };
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the subscriber did not read its notification in time",
                    ));
                }
            }
            Err(e) => return Err(std::io::Error::other(e)),
        }

        let mut written = 0u32;
        unsafe { GetOverlappedResult(handle, &overlapped, &mut written, true) }
            .map_err(std::io::Error::other)?;
        if written as usize != line.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "a notification was only partly written",
            ));
        }
        Ok(())
    }
}

/// The set of connected subscribers.
pub struct Subscribers {
    tx: Sender<Message>,
    /// Shared with the fan-out thread, because that is where a subscriber which
    /// stopped reading is noticed. A private copy here would leave `mochic
    /// state` listing bars that are gone and grow without bound.
    names: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

/// A poisoned name list is still a perfectly good name list.
fn lock(names: &Mutex<Vec<String>>) -> MutexGuard<'_, Vec<String>> {
    names.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Subscribers {
    /// Starts the fan-out thread.
    pub fn start() -> Result<Self> {
        let (tx, rx) = channel::<Message>();
        let names = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let names = Arc::clone(&names);
            std::thread::Builder::new()
                .name("mochi-subscribers".into())
                .spawn(move || {
                    let mut pipes: HashMap<String, Pipe> = HashMap::new();
                    while let Ok(message) = rx.recv() {
                        match message {
                            Message::Add(name, pipe) => {
                                tracing::info!(subscriber = %name, "subscribed");
                                pipes.insert(name, pipe);
                            }
                            Message::Remove(name) => {
                                if pipes.remove(&name).is_some() {
                                    tracing::info!(subscriber = %name, "unsubscribed");
                                }
                            }
                            Message::Notify(notification) => {
                                // Framed once and handed to every subscriber:
                                // the bounded write needs the whole line in one
                                // buffer anyway.
                                let mut line = Vec::new();
                                if let Err(e) = protocol::write_message(&mut line, &*notification) {
                                    tracing::error!(error = %e, "could not frame a notification");
                                    continue;
                                }
                                let mut dropped: Vec<String> = Vec::new();
                                pipes.retain(|name, pipe| match pipe.write(&line) {
                                    Ok(()) => true,
                                    Err(e) => {
                                        tracing::info!(
                                            subscriber = %name,
                                            error = %e,
                                            "dropping a subscriber that stopped reading"
                                        );
                                        dropped.push(name.clone());
                                        false
                                    }
                                });
                                if !dropped.is_empty() {
                                    lock(&names).retain(|n| !dropped.contains(n));
                                }
                            }
                        }
                    }
                })
                .context("could not spawn the subscriber thread")?
        };

        Ok(Self {
            tx,
            names,
            handle: Some(handle),
        })
    }

    /// Registers a subscriber pipe. Fails if the pipe does not exist yet.
    ///
    /// The connection is opened here rather than on the fan-out thread so that
    /// a client that misspelled its pipe name gets a real error back.
    pub fn add(&mut self, name: &str) -> Result<()> {
        let pipe = open(name)?;
        {
            let mut names = lock(&self.names);
            if !names.iter().any(|n| n == name) {
                names.push(name.to_owned());
            }
        }
        self.tx
            .send(Message::Add(name.to_owned(), pipe))
            .context("the subscriber thread is gone")
    }

    /// Forgets a subscriber pipe.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        lock(&self.names).retain(|n| n != name);
        self.tx
            .send(Message::Remove(name.to_owned()))
            .context("the subscriber thread is gone")
    }

    /// Queues a notification for every subscriber. Never blocks the caller.
    pub fn notify(&self, notification: Notification) {
        if lock(&self.names).is_empty() {
            return;
        }
        let _ = self.tx.send(Message::Notify(Box::new(notification)));
    }

    /// The registered subscriber names, for `mochic state`.
    pub fn names(&self) -> Vec<String> {
        lock(&self.names).clone()
    }

    /// Drops every subscriber and joins the thread.
    pub fn stop(&mut self) {
        lock(&self.names).clear();
        // Dropping the sender ends the receive loop.
        let (dead, _) = channel();
        let tx = std::mem::replace(&mut self.tx, dead);
        drop(tx);
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            tracing::error!("the subscriber thread panicked");
        }
    }
}

impl Drop for Subscribers {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Connects to a subscriber pipe as a client.
///
/// The name arrives from a client, so it is validated before it is pasted onto
/// `\\.\pipe\`. That prefix is a device namespace: a name with a separator in
/// it names something that is not a pipe, and the daemon would write its
/// notification stream into whatever that turned out to be.
///
/// For the same reason the connection pins its quality of service. Whoever sent
/// `subscribe-pipe` chose what the daemon connects to, and a pipe server handed
/// a full impersonation token can act as the daemon's account for as long as
/// the subscription lasts.
fn open(name: &str) -> Result<Pipe> {
    mochi_client::validate_pipe_name(name)?;
    let path = format!("{PIPE_PREFIX}{name}");
    let flags = (FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION).0;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(flags)
        .open(&path)
        .with_context(|| format!("no subscriber pipe at {path}"))?;
    let event = unsafe { CreateEventW(None, true, false, None) }
        .context("could not create a write event")?;
    // SAFETY: a fresh event handle, owned by the `Pipe` from here on.
    let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
    Ok(Pipe { file, event })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mochi_client::{NotificationEvent, create_pipe};

    #[test]
    fn a_missing_pipe_is_reported_to_the_client() {
        let mut subs = Subscribers::start().unwrap();
        let err = subs.add("mochi-test-no-such-pipe").unwrap_err();
        assert!(err.to_string().contains("no subscriber pipe"));
        assert!(subs.names().is_empty());
    }

    #[test]
    fn a_name_that_escapes_the_pipe_namespace_is_refused() {
        let mut subs = Subscribers::start().unwrap();
        for bad in [
            r"..\..\..\..\Users\someone\notes.txt",
            r"\\other\pipe\thing",
            "has/slash",
            "",
        ] {
            let err = subs.add(bad).unwrap_err();
            assert!(
                err.to_string().contains("not a usable pipe name"),
                "{bad:?} gave {err}"
            );
        }
        assert!(subs.names().is_empty());
    }

    #[test]
    fn notifications_reach_a_real_subscriber_pipe() {
        let name = format!("mochi-test-subs-{}", std::process::id());
        let mut subscription = create_pipe(&name).unwrap();

        let mut subs = Subscribers::start().unwrap();
        subs.add(&name).unwrap();
        assert_eq!(subs.names(), std::slice::from_ref(&name));

        let sent = Notification::new(NotificationEvent::MonitorsChanged { count: 2 });
        subs.notify(sent.clone());

        let got = subscription.next_notification().unwrap().unwrap();
        assert_eq!(got, sent);

        subs.remove(&name).unwrap();
        assert!(subs.names().is_empty());
    }

    /// Enough notifications to overflow a 64 KiB pipe buffer several times.
    const NOTIFICATIONS: usize = 4000;

    /// One bar that stops reading must not take the fan-out down with it.
    ///
    /// The wedged subscriber never drains its pipe, so its buffer fills and the
    /// daemon's write to it cannot complete. Everything else has to carry on:
    /// the other subscriber keeps receiving, and `stop()` returns.
    #[test]
    fn a_subscriber_that_stopped_reading_does_not_wedge_the_others() {
        let wedged_name = format!("mochi-test-wedged-{}", std::process::id());
        let live_name = format!("mochi-test-live-{}", std::process::id());
        // Created and then never read from, exactly like a hung bar.
        let _wedged = create_pipe(&wedged_name).unwrap();
        let mut live = create_pipe(&live_name).unwrap();

        let (seen_tx, seen_rx) = channel();
        std::thread::spawn(move || {
            while let Ok(Some(n)) = live.next_notification() {
                if seen_tx.send(n).is_err() {
                    break;
                }
            }
        });

        // On its own thread, so a fan-out that never comes back is a failed
        // test rather than a hung test run.
        let (done_tx, done_rx) = channel();
        std::thread::spawn(move || {
            let mut subs = Subscribers::start().unwrap();
            subs.add(&wedged_name).unwrap();
            subs.add(&live_name).unwrap();
            for count in 0..NOTIFICATIONS {
                subs.notify(Notification::new(NotificationEvent::MonitorsChanged {
                    count,
                }));
            }
            let started = std::time::Instant::now();
            subs.stop();
            let _ = done_tx.send(started.elapsed());
        });

        let patience = std::time::Duration::from_secs(20);
        let mut last = None;
        while last != Some(NOTIFICATIONS - 1) {
            let got = seen_rx
                .recv_timeout(patience)
                .expect("the fan-out stopped delivering to a subscriber that was still reading");
            if let NotificationEvent::MonitorsChanged { count } = got.event {
                last = Some(count);
            }
        }

        let elapsed = done_rx
            .recv_timeout(patience)
            .expect("Subscribers::stop never returned");
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "stop() waited {elapsed:?} on a wedged subscriber"
        );
    }

    /// `mochic state` must not list bars that are gone.
    #[test]
    fn a_subscriber_that_went_away_is_dropped_from_the_names() {
        let name = format!("mochi-test-gone-{}", std::process::id());
        let subscription = create_pipe(&name).unwrap();

        let mut subs = Subscribers::start().unwrap();
        subs.add(&name).unwrap();
        assert_eq!(subs.names(), std::slice::from_ref(&name));

        // The bar exits: its end of the pipe closes and the daemon's next write
        // fails, which is how a dead subscriber is noticed.
        drop(subscription);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !subs.names().is_empty() && std::time::Instant::now() < deadline {
            subs.notify(Notification::new(NotificationEvent::MonitorsChanged {
                count: 1,
            }));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            subs.names().is_empty(),
            "a subscriber that is gone stayed in the reported names: {:?}",
            subs.names()
        );
    }
}
