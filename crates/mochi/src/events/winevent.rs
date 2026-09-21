//! `SetWinEventHook` on a dedicated thread.
//!
//! Out-of-context hooks are delivered to the thread that installed them, and
//! only while that thread pumps messages. So the hooks live on their own thread
//! with a bare message loop; the callback does nothing but filter and forward.

use std::cell::RefCell;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, TranslateMessage,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_QUIT,
};

use super::{Event, EventSender, WindowEventKind, raw};
use crate::platform::Hwnd;

thread_local! {
    /// Set once on the hook thread before the message loop starts. The callback
    /// always runs on that same thread, so no lock is needed.
    static SENDER: RefCell<Option<EventSender>> = const { RefCell::new(None) };
}

/// The `EVENT_*` ranges Mochi hooks.
///
/// Fewer, wider ranges would mean fewer hooks but far more callbacks; these
/// five are the narrowest split that still covers everything.
const RANGES: &[(u32, u32)] = &[
    (raw::EVENT_SYSTEM_FOREGROUND, raw::EVENT_SYSTEM_FOREGROUND),
    (
        raw::EVENT_SYSTEM_MOVESIZESTART,
        raw::EVENT_SYSTEM_MOVESIZEEND,
    ),
    (
        raw::EVENT_SYSTEM_MINIMIZESTART,
        raw::EVENT_SYSTEM_MINIMIZEEND,
    ),
    (raw::EVENT_OBJECT_CREATE, raw::EVENT_OBJECT_HIDE),
    (
        raw::EVENT_OBJECT_LOCATIONCHANGE,
        raw::EVENT_OBJECT_NAMECHANGE,
    ),
    (raw::EVENT_OBJECT_CLOAKED, raw::EVENT_OBJECT_UNCLOAKED),
];

unsafe extern "system" fn callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only top-level windows. Controls, carets, cursors and menu items all
    // arrive here too and would drown the channel.
    if id_object != raw::OBJID_WINDOW || id_child != raw::CHILDID_SELF || window.0.is_null() {
        return;
    }
    let Some(kind) = WindowEventKind::from_raw(event) else {
        return;
    };
    let hwnd = Hwnd(window.0 as isize);
    SENDER.with(|cell| {
        if let Some(tx) = cell.borrow().as_ref() {
            // A closed channel means the loop is gone and we are about to stop.
            let _ = tx.send(Event::Window { kind, hwnd });
        }
    });
}

/// A running set of hooks. Dropping it unhooks and joins the thread.
pub struct WinEventHooks {
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

impl WinEventHooks {
    /// Installs the hooks on a fresh thread and starts pumping messages.
    pub fn start(tx: EventSender) -> Result<Self> {
        let (ready_tx, ready_rx) = channel::<Result<u32, String>>();

        let handle = std::thread::Builder::new()
            .name("mochi-winevent".into())
            .spawn(move || hook_thread(tx, ready_tx))
            .context("could not spawn the WinEvent thread")?;

        let thread_id = ready_rx
            .recv()
            .context("the WinEvent thread died during startup")?
            .map_err(|e| anyhow!("{e}"))?;

        tracing::debug!(thread_id, hooks = RANGES.len(), "winevent hooks installed");
        Ok(Self {
            thread_id,
            handle: Some(handle),
        })
    }

    /// Asks the hook thread to unhook and exit, then waits for it.
    pub fn stop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        // WM_QUIT to the thread, not to a window: the loop has no window.
        if let Err(e) = unsafe {
            PostThreadMessageW(
                self.thread_id,
                WM_QUIT,
                Default::default(),
                Default::default(),
            )
        } {
            tracing::warn!(error = %e, "could not post WM_QUIT to the WinEvent thread");
        }
        super::join_before(handle, super::STOP_DEADLINE, "winevent");
    }
}

impl Drop for WinEventHooks {
    fn drop(&mut self) {
        self.stop();
    }
}

fn hook_thread(tx: EventSender, ready: Sender<Result<u32, String>>) {
    SENDER.with(|cell| *cell.borrow_mut() = Some(tx));

    let mut hooks = Vec::with_capacity(RANGES.len());
    for &(min, max) in RANGES {
        let hook = unsafe {
            SetWinEventHook(
                min,
                max,
                None,
                Some(callback),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if hook.is_invalid() {
            for h in hooks {
                let _ = unsafe { UnhookWinEvent(h) };
            }
            let _ = ready.send(Err(format!(
                "SetWinEventHook failed for range 0x{min:04x}..=0x{max:04x}"
            )));
            return;
        }
        hooks.push(hook);
    }

    let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    if ready.send(Ok(thread_id)).is_err() {
        // Nobody is waiting for us any more.
        for h in hooks {
            let _ = unsafe { UnhookWinEvent(h) };
        }
        return;
    }

    let mut msg = MSG::default();
    loop {
        // GetMessageW returns 0 for WM_QUIT and -1 for an error.
        let result = unsafe { GetMessageW(&raw mut msg, None, 0, 0) };
        if result.0 <= 0 {
            if result.0 < 0 {
                tracing::error!("GetMessageW failed on the WinEvent thread");
            }
            break;
        }
        unsafe {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }

    for h in hooks {
        let _ = unsafe { UnhookWinEvent(h) };
    }
    SENDER.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ranges_cover_every_event_we_map() {
        let interesting = [
            raw::EVENT_SYSTEM_FOREGROUND,
            raw::EVENT_SYSTEM_MOVESIZESTART,
            raw::EVENT_SYSTEM_MOVESIZEEND,
            raw::EVENT_SYSTEM_MINIMIZESTART,
            raw::EVENT_SYSTEM_MINIMIZEEND,
            raw::EVENT_OBJECT_CREATE,
            raw::EVENT_OBJECT_DESTROY,
            raw::EVENT_OBJECT_SHOW,
            raw::EVENT_OBJECT_HIDE,
            raw::EVENT_OBJECT_LOCATIONCHANGE,
            raw::EVENT_OBJECT_NAMECHANGE,
            raw::EVENT_OBJECT_CLOAKED,
            raw::EVENT_OBJECT_UNCLOAKED,
        ];
        for event in interesting {
            assert!(
                RANGES.iter().any(|&(lo, hi)| (lo..=hi).contains(&event)),
                "0x{event:04x} is mapped but not hooked"
            );
        }
    }

    #[test]
    fn hooks_install_and_come_back_down() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut hooks = WinEventHooks::start(tx).expect("hooks should install");
        hooks.stop();
        // A second stop must not hang or panic.
        hooks.stop();
    }
}
