//! Notification fan-out to subscriber pipes.
//!
//! Subscribers create their own pipe and the daemon connects to it as a client,
//! so a bar can be started in any order relative to the daemon. Writing to a
//! pipe whose reader is not draining blocks, which is why the fan-out lives on
//! its own thread: a wedged bar must never stall the window manager loop.

use std::collections::HashMap;
use std::fs::File;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use mochi_client::{Notification, PIPE_PREFIX, protocol};

enum Message {
    Add(String, File),
    Remove(String),
    Notify(Box<Notification>),
}

/// The set of connected subscribers.
pub struct Subscribers {
    tx: Sender<Message>,
    names: Vec<String>,
    handle: Option<JoinHandle<()>>,
}

impl Subscribers {
    /// Starts the fan-out thread.
    pub fn start() -> Result<Self> {
        let (tx, rx) = channel::<Message>();
        let handle = std::thread::Builder::new()
            .name("mochi-subscribers".into())
            .spawn(move || {
                let mut pipes: HashMap<String, File> = HashMap::new();
                while let Ok(message) = rx.recv() {
                    match message {
                        Message::Add(name, file) => {
                            tracing::info!(subscriber = %name, "subscribed");
                            pipes.insert(name, file);
                        }
                        Message::Remove(name) => {
                            if pipes.remove(&name).is_some() {
                                tracing::info!(subscriber = %name, "unsubscribed");
                            }
                        }
                        Message::Notify(notification) => {
                            pipes.retain(|name, file| {
                                match protocol::write_message(&mut &*file, &notification) {
                                    Ok(()) => true,
                                    Err(e) => {
                                        tracing::info!(
                                            subscriber = %name,
                                            error = %e,
                                            "dropping a subscriber that stopped reading"
                                        );
                                        false
                                    }
                                }
                            });
                        }
                    }
                }
            })
            .context("could not spawn the subscriber thread")?;

        Ok(Self {
            tx,
            names: Vec::new(),
            handle: Some(handle),
        })
    }

    /// Registers a subscriber pipe. Fails if the pipe does not exist yet.
    ///
    /// The connection is opened here rather than on the fan-out thread so that
    /// a client that misspelled its pipe name gets a real error back.
    pub fn add(&mut self, name: &str) -> Result<()> {
        let file = open(name)?;
        if !self.names.iter().any(|n| n == name) {
            self.names.push(name.to_owned());
        }
        self.tx
            .send(Message::Add(name.to_owned(), file))
            .context("the subscriber thread is gone")
    }

    /// Forgets a subscriber pipe.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.names.retain(|n| n != name);
        self.tx
            .send(Message::Remove(name.to_owned()))
            .context("the subscriber thread is gone")
    }

    /// Queues a notification for every subscriber. Never blocks the caller.
    pub fn notify(&self, notification: Notification) {
        if self.names.is_empty() {
            return;
        }
        let _ = self.tx.send(Message::Notify(Box::new(notification)));
    }

    /// The registered subscriber names, for `mochic state`.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Drops every subscriber and joins the thread.
    pub fn stop(&mut self) {
        self.names.clear();
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
fn open(name: &str) -> Result<File> {
    mochi_client::validate_pipe_name(name)?;
    let path = format!("{PIPE_PREFIX}{name}");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .with_context(|| format!("no subscriber pipe at {path}"))
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
}
