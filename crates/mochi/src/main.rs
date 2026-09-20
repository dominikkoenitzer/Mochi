//! The Mochi daemon.
//!
//! Owns all window manager state on a single thread and talks to Win32.
//! See `docs/ipc.md` for the protocol and `crates/mochi/README.md` for the
//! module map and the hook points the tiling model plugs into.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows::core::BOOL;

use mochi::events::{Event, EventSender, ShutdownReason};
use mochi::{cli, config, events, ipc, logging, platform, safety, single_instance, state, wm};

/// Where the console control handler finds the channel.
static SHUTDOWN: OnceLock<Mutex<Option<EventSender>>> = OnceLock::new();

/// Handles `Ctrl-C`, the console close button, logoff and shutdown.
///
/// The handler runs on a thread the system creates for it, so it must not touch
/// the window manager state; it only nudges the loop. The exception is the
/// second half: for `CTRL_CLOSE_EVENT`, `CTRL_LOGOFF_EVENT` and
/// `CTRL_SHUTDOWN_EVENT` Windows terminates the process as soon as this
/// function returns, so asking the loop to shut down is not enough on its own.
/// Those three restore the desktop here, synchronously, before returning.
/// [`safety::restore_all`] is idempotent and safe from any thread, so the loop
/// doing it again a moment later costs nothing.
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> BOOL {
    let mut handled = false;
    if let Some(slot) = SHUTDOWN.get()
        && let Ok(guard) = slot.lock()
        && let Some(tx) = guard.as_ref()
    {
        let _ = tx.send(Event::Shutdown(ShutdownReason::Signal));
        handled = true;
    }

    match ctrl_type {
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            tracing::warn!(
                ctrl_type,
                "the process is about to be terminated, restoring the desktop now"
            );
            safety::restore_all();
        }
        CTRL_C_EVENT | CTRL_BREAK_EVENT => tracing::info!(ctrl_type, "interrupted"),
        _ => {}
    }

    handled.into()
}

fn main() -> Result<()> {
    let args = cli::Args::get();

    let log_guard = logging::init()?;
    safety::install_panic_hook();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        dry_run = args.dry_run,
        logs = %log_guard.directory().display(),
        "mochi starting"
    );
    if args.dry_run {
        tracing::warn!("dry run: no window will be moved, cloaked, focused or closed");
    }

    // Refuse to be the second window manager in this session.
    let _instance = single_instance::SingleInstance::acquire()?;

    // The manifest should already have done this; the call is the fallback.
    if !platform::dpi::ensure_per_monitor_v2() {
        tracing::warn!("this process is not per-monitor DPI aware, rectangles may be scaled");
    }

    let config_path = config::resolve_path(args.config.as_deref())?;
    tracing::info!(config = %config_path.display(), exists = config_path.exists(), "configuration");

    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    let platform = platform::new(args.dry_run);
    let mut session = state::State::new(config_path.clone(), args.dry_run);
    session.manage_classes.clone_from(&args.manage_class);
    if !session.manage_classes.is_empty() {
        tracing::warn!(
            classes = ?session.manage_classes,
            "managing only these window classes, every other window is left alone"
        );
    }

    let mut manager =
        wm::WindowManager::new(std::sync::Arc::clone(&platform), tx.clone(), rx, session)?;
    manager.install_restore_hook();
    let mut restore_guard = safety::RestoreGuard::new();

    // Console control events: Ctrl-C when run from a terminal, and the close
    // button. A detached daemon never sees them, which is why `mochic stop`
    // exists, but a developer running it in a terminal expects Ctrl-C to work.
    let _ = SHUTDOWN.set(Mutex::new(Some(tx.clone())));
    if let Err(e) = unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), true) } {
        tracing::debug!(error = %e, "no console control handler, Ctrl-C will not be graceful");
    }

    // Producers. Each one owns a thread and is shut down in reverse order.
    let mut hooks = events::winevent::WinEventHooks::start(tx.clone())
        .context("could not install the WinEvent hooks")?;
    let mut message_window = events::message_window::MessageWindow::start(tx.clone())
        .context("could not create the hidden message window")?;
    let mouse =
        events::mouse::MouseTracker::start(tx.clone(), std::sync::Arc::clone(&platform), false)?;
    manager.attach_mouse_tracker(mouse);
    let config_watcher = config::ConfigWatcher::start(config_path, tx.clone())?;
    tracing::debug!(watching = %config_watcher.path().display(), "configuration watcher");

    // The keyboard hook comes up after the desktop is tiled, so the first key
    // press cannot arrive before there is a model for it to act on.
    if args.no_hotkeys {
        tracing::info!("--no-hotkeys: this daemon binds no keys");
    } else {
        let candidates = config::hotkey_candidates(args.hotkeys.as_deref())?;
        manager.start_hotkeys(
            config::resolve_hotkeys_path(args.hotkeys.as_deref())?,
            candidates,
        );
    }
    let others = config::hotkey_candidates(args.hotkeys.as_deref())?;
    let hotkey_watcher = manager
        .hotkey_path()
        .map(Path::to_path_buf)
        .map(|path| config::ConfigWatcher::start_any(path, others, tx.clone()))
        .transpose()?;
    if let Some(watcher) = hotkey_watcher.as_ref() {
        tracing::debug!(watching = %watcher.path().display(), "hotkey watcher");
    }
    let mut pipe = ipc::PipeServer::start(tx.clone())?;

    // The loop owns the state until something asks it to stop.
    // A panic on any producer thread stops the daemon instead of leaving it
    // running with that thread missing.
    {
        let stopper = tx.clone();
        safety::set_shutdown_hook(move || {
            let _ = stopper.send(Event::Shutdown(ShutdownReason::Panicked));
        });
    }

    let result = manager.run();

    // The desktop comes back FIRST, before anything is torn down.
    //
    // It used to come last, after four `join()` calls with no deadline. Any of
    // them can block forever: each producer thread is woken with a posted
    // message, and `PostThreadMessageW` fails once a thread's queue is full,
    // which is exactly what a window storming location-change events does to
    // the hook thread. The join was then waiting on a thread nothing could
    // wake, with the hotkeys already unbound and the pipe already stopped, so
    // there was no key and no command left to ask for a restore and every
    // window on every inactive workspace stayed cloaked behind a live process
    // nobody could talk to.
    //
    // Giving the windows back before the teardown makes a hung producer cost a
    // lingering process instead of a lost desktop.
    manager.restore_all();
    restore_guard.disarm();

    tracing::info!("stopping the producers");
    manager.stop_hotkeys();
    pipe.stop();
    drop(hotkey_watcher);
    drop(config_watcher);
    hooks.stop();
    message_window.stop();

    if let Some(slot) = SHUTDOWN.get()
        && let Ok(mut guard) = slot.lock()
    {
        *guard = None;
    }

    tracing::info!("mochi stopped");
    drop(log_guard);
    result
}
