//! The net under the trapeze.
//!
//! The worst failure mode of a tiling window manager is not a crash, it is a
//! crash that leaves windows cloaked: they are gone from the screen, gone from
//! Alt-Tab, and the only way back is a reboot or another manager. So every path
//! out of the process, clean or not, runs [`restore_all`].
//!
//! Right now [`restore_all`] only logs. The hook is here so that the tiling
//! step has one obvious place to plug into: call [`set_restore_hook`] once with
//! a closure that uncloaks and repositions every managed window.
//!
//! # What does and does not reach [`restore_all`]
//!
//! | Way out | Reaches `restore_all` | How |
//! |---|---|---|
//! | normal `stop` command | yes | the loop returns and `main` calls it |
//! | `Ctrl-C`, `Ctrl-Break` | yes | the console control handler asks the loop to stop |
//! | console window closed, logoff, shutdown | yes | the console control handler calls it *synchronously*, because Windows terminates the process the moment the handler returns |
//! | panic on any thread | yes | [`install_panic_hook`] |
//! | early return or `?` in `main` | yes | [`RestoreGuard`] |
//! | `TerminateProcess`, Task Manager "End task", power loss | **no** | nothing runs in the process |
//!
//! The last row is the one a hard kill cannot cover: user-mode code does not get
//! to run, so any window still cloaked stays cloaked until something uncloaks
//! it. The mitigation is not in this module but in the tiling step: cloak as
//! late and uncloak as early as possible, and keep [`crate::wm::WindowManager::cloaked`]
//! exact, because a stale entry there is a window the user cannot get back.

use std::cell::Cell;
use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

type RestoreHook = Box<dyn Fn() + Send + 'static>;

fn hook() -> &'static Mutex<Option<RestoreHook>> {
    static HOOK: OnceLock<Mutex<Option<RestoreHook>>> = OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

thread_local! {
    /// True while this thread is inside the restore hook.
    ///
    /// A panic *inside* the hook runs the panic hook, which calls
    /// [`restore_all`] again on the same thread. `std::sync::Mutex` is not
    /// re-entrant, so without this flag that second call would deadlock on a
    /// lock the same thread already holds, with every window still cloaked.
    static RESTORING: Cell<bool> = const { Cell::new(false) };
}

/// Locks the hook slot, ignoring poison.
///
/// A poisoned lock means a previous hook panicked. The desktop still has to be
/// put back, so the poison is deliberately ignored: refusing to restore is a
/// far worse outcome than running a hook that failed once before.
fn lock() -> MutexGuard<'static, Option<RestoreHook>> {
    hook().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Installs the closure that undoes everything Mochi did to the desktop.
///
/// The closure runs from a panic hook and from `Drop`, so it must not assume
/// the window manager state is intact: it gets its own snapshot of whatever it
/// needs to uncloak, typically an `Arc<Mutex<Vec<Hwnd>>>` that the loop keeps
/// up to date.
///
/// It should also not panic. [`restore_all`] catches an unwind on the ordinary
/// paths, but a hook that panics while the process is *already* panicking is a
/// panic inside a panic hook, which Rust turns into an immediate abort before
/// any `catch_unwind` can see it.
pub fn set_restore_hook(f: impl Fn() + Send + 'static) {
    *lock() = Some(Box::new(f));
}

/// Puts the desktop back the way Mochi found it.
///
/// Safe to call more than once, from any thread, and from inside the hook
/// itself: a re-entrant call returns immediately instead of deadlocking.
/// This function never panics and never propagates one.
pub fn restore_all() {
    if RESTORING.get() {
        // Re-entered on the thread that is already restoring, which only
        // happens when the hook panicked and the panic hook called us back.
        tracing::error!("restore_all re-entered from inside the restore hook, ignoring");
        return;
    }

    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            RESTORING.set(false);
        }
    }
    RESTORING.set(true);
    let _reset = Reset;

    let slot = lock();
    let Some(f) = slot.as_ref() else {
        tracing::info!("restore_all: nothing is managed yet, the desktop is untouched");
        return;
    };
    tracing::info!("restore_all: putting every managed window back");
    // The lock is held while the hook runs, so two threads racing to shut down
    // cannot uncloak the same window twice. The thread-local flag above is what
    // makes that safe against re-entry.
    if std::panic::catch_unwind(AssertUnwindSafe(f)).is_err() {
        tracing::error!("the restore hook panicked, some windows may still be cloaked");
    }
}

/// Runs [`restore_all`] when it goes out of scope, however that happens.
pub struct RestoreGuard {
    armed: bool,
}

impl RestoreGuard {
    /// Arms the guard.
    pub const fn new() -> Self {
        Self { armed: true }
    }

    /// Disarms the guard, for a shutdown that already restored everything.
    pub const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Default for RestoreGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if self.armed {
            restore_all();
        }
    }
}

/// Wraps the default panic hook so a panic logs and then restores the desktop.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map_or_else(|| "unknown".to_owned(), ToString::to_string);
        tracing::error!(%location, payload = %panic_message(info), "mochi panicked");
        restore_all();
        previous(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The hook is process-global, so these tests must not run side by side.
    fn exclusive() -> MutexGuard<'static, ()> {
        static GATE: Mutex<()> = Mutex::new(());
        GATE.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    fn the_guard_runs_the_hook_exactly_once_per_drop() {
        let _gate = exclusive();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        set_restore_hook(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        {
            let _guard = RestoreGuard::new();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        {
            let mut guard = RestoreGuard::new();
            guard.disarm();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a disarmed guard is quiet");

        restore_all();
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // Leave no hook behind for the other tests in this binary.
        set_restore_hook(|| {});
    }

    #[test]
    fn a_hook_that_panics_neither_deadlocks_nor_poisons_the_slot() {
        let _gate = exclusive();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        set_restore_hook(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            panic!("the restore hook fell over");
        });

        // Quiet the default hook so the test output stays readable, and prove
        // that the panic does not escape restore_all.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        restore_all();
        // A second call must still reach the hook: the poisoned mutex is ignored.
        restore_all();
        std::panic::set_hook(previous);

        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // And a fresh hook can still be installed afterwards.
        let after = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&after);
        set_restore_hook(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        restore_all();
        assert_eq!(after.load(Ordering::SeqCst), 1);

        set_restore_hook(|| {});
    }

    #[test]
    fn restore_all_called_from_inside_the_hook_returns_instead_of_deadlocking() {
        let _gate = exclusive();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        set_restore_hook(move || {
            if counter.fetch_add(1, Ordering::SeqCst) < 4 {
                // This is what the panic hook does when the hook panics.
                restore_all();
            }
        });

        restore_all();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the re-entrant call must not run the hook again"
        );

        set_restore_hook(|| {});
    }

    #[test]
    fn a_panic_on_any_thread_reaches_the_restore_hook() {
        let _gate = exclusive();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        set_restore_hook(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let previous = std::panic::take_hook();
        // Silence the panic that the next few lines cause on purpose, then wrap
        // the silent hook the way startup wraps the real one.
        std::panic::set_hook(Box::new(|_| {}));
        install_panic_hook();
        let joined = std::thread::spawn(|| panic!("a worker thread fell over")).join();
        std::panic::set_hook(previous);

        assert!(joined.is_err(), "the thread should have panicked");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a panic off the main thread must still restore the desktop"
        );

        set_restore_hook(|| {});
    }
}
