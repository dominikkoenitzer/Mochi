//! Focus follows mouse.
//!
//! Two implementations, same [`Event::MouseFocus`] output:
//!
//! * the default polls the cursor every [`POLL_INTERVAL`] and reports when the
//!   window under it changes,
//! * with the `mouse-hook` feature a `WH_MOUSE_LL` hook reports the same thing
//!   without the latency, at the cost of a global hook in every input path.
//!
//! Polling is the default on purpose. A low level mouse hook that blocks makes
//! the whole desktop feel sticky, and the poll is accurate enough for a feature
//! that is off in the user's configuration anyway.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result};

use super::{Event, EventSender};
use crate::platform::{Hwnd, Platform};

/// How often the cursor is sampled when focus-follows-mouse is on.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(75);

/// How long the thread sleeps while the feature is switched off.
const IDLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Watches the cursor and reports the window under it.
pub struct MouseTracker {
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MouseTracker {
    /// Starts the watcher thread. It idles until [`MouseTracker::set_enabled`].
    pub fn start(tx: EventSender, platform: Arc<dyn Platform>, enabled: bool) -> Result<Self> {
        let enabled = Arc::new(AtomicBool::new(enabled));
        let stop = Arc::new(AtomicBool::new(false));

        let handle = {
            let enabled = Arc::clone(&enabled);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("mochi-mouse".into())
                .spawn(move || poll_loop(&tx, platform.as_ref(), &enabled, &stop))
                .context("could not spawn the mouse tracker thread")?
        };

        Ok(Self {
            enabled,
            stop,
            handle: Some(handle),
        })
    }

    /// Turns reporting on or off without stopping the thread.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        tracing::debug!(enabled = on, "focus-follows-mouse");
    }

    /// True while reporting is on.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Stops the thread and waits for it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            tracing::error!("the mouse tracker thread panicked");
        }
    }
}

impl Drop for MouseTracker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn poll_loop(tx: &EventSender, platform: &dyn Platform, enabled: &AtomicBool, stop: &AtomicBool) {
    let mut last: Option<Hwnd> = None;
    while !stop.load(Ordering::Relaxed) {
        if !enabled.load(Ordering::Relaxed) {
            last = None;
            std::thread::sleep(IDLE_INTERVAL);
            continue;
        }
        if let Ok((x, y)) = platform.cursor_position()
            && let Some(hwnd) = platform.window_at(x, y)
            && last != Some(hwnd)
        {
            last = Some(hwnd);
            if tx.send(Event::MouseFocus { hwnd }).is_err() {
                return;
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The `WH_MOUSE_LL` variant, built only with the `mouse-hook` feature.
///
/// It lives on its own thread with a message loop, exactly like the WinEvent
/// hooks, and never does work inside the callback beyond a channel send.
#[cfg(feature = "mouse-hook")]
pub mod hook {
    use std::cell::RefCell;
    use std::sync::mpsc::{Sender, channel};
    use std::thread::JoinHandle;

    use anyhow::{Context, Result, anyhow};
    use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GA_ROOT, GetAncestor, GetMessageW, HHOOK, MSG,
        PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL,
        WM_MOUSEMOVE, WM_QUIT, WindowFromPoint,
    };

    use crate::events::{Event, EventSender};
    use crate::platform::Hwnd;

    thread_local! {
        static SENDER: RefCell<Option<EventSender>> = const { RefCell::new(None) };
        static LAST: RefCell<Option<Hwnd>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && wparam.0 as u32 == WM_MOUSEMOVE {
            // SAFETY: for WH_MOUSE_LL the lparam is a MSLLHOOKSTRUCT.
            let point = unsafe { *(lparam.0 as *const POINT) };
            let window = unsafe { WindowFromPoint(point) };
            if !window.0.is_null() {
                let root = unsafe { GetAncestor(window, GA_ROOT) };
                let hwnd = Hwnd(if root.0.is_null() { window.0 } else { root.0 } as isize);
                let changed = LAST.with(|cell| {
                    let mut last = cell.borrow_mut();
                    if *last == Some(hwnd) {
                        false
                    } else {
                        *last = Some(hwnd);
                        true
                    }
                });
                if changed {
                    SENDER.with(|cell| {
                        if let Some(tx) = cell.borrow().as_ref() {
                            let _ = tx.send(Event::MouseFocus { hwnd });
                        }
                    });
                }
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// A running low level mouse hook.
    pub struct MouseHook {
        thread_id: u32,
        handle: Option<JoinHandle<()>>,
    }

    impl MouseHook {
        /// Installs the hook on a fresh thread.
        pub fn start(tx: EventSender) -> Result<Self> {
            let (ready_tx, ready_rx) = channel::<Result<u32, String>>();
            let handle = std::thread::Builder::new()
                .name("mochi-mouse-hook".into())
                .spawn(move || hook_thread(tx, ready_tx))
                .context("could not spawn the mouse hook thread")?;
            let thread_id = ready_rx
                .recv()
                .context("the mouse hook thread died during startup")?
                .map_err(|e| anyhow!("{e}"))?;
            Ok(Self {
                thread_id,
                handle: Some(handle),
            })
        }

        /// Removes the hook and joins the thread.
        pub fn stop(&mut self) {
            let Some(handle) = self.handle.take() else {
                return;
            };
            let _ = unsafe {
                PostThreadMessageW(
                    self.thread_id,
                    WM_QUIT,
                    Default::default(),
                    Default::default(),
                )
            };
            let _ = handle.join();
        }
    }

    impl Drop for MouseHook {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn hook_thread(tx: EventSender, ready: Sender<Result<u32, String>>) {
        SENDER.with(|cell| *cell.borrow_mut() = Some(tx));
        let hook: HHOOK = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(proc), None, 0) } {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("SetWindowsHookExW failed: {e}")));
                return;
            }
        };
        let thread_id = unsafe { GetCurrentThreadId() };
        if ready.send(Ok(thread_id)).is_err() {
            let _ = unsafe { UnhookWindowsHookEx(hook) };
            return;
        }
        let mut message = MSG::default();
        loop {
            let result = unsafe { GetMessageW(&raw mut message, None, 0, 0) };
            if result.0 <= 0 {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        let _ = unsafe { UnhookWindowsHookEx(hook) };
        SENDER.with(|cell| *cell.borrow_mut() = None);
    }
}
