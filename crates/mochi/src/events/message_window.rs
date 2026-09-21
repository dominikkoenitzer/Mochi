//! A hidden window that listens for desktop-wide news.
//!
//! Display, work area and session changes are broadcast to *top-level* windows.
//! A `HWND_MESSAGE` window would never see them, so this is a real top-level
//! window that is simply never shown: zero sized, `WS_EX_TOOLWINDOW`, no
//! `WS_VISIBLE`. It has no pixels and no taskbar button, and Mochi's own
//! manageability check skips tool windows, so it cannot tile itself.

use std::cell::RefCell;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use mochi_client::SessionChangeKind;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    PostThreadMessageW, RegisterClassW, TranslateMessage, WM_QUIT, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_POPUP,
};
use windows::core::w;

use super::{Event, EventSender, MonitorEventKind};

/// Window messages this module reacts to, straight out of `winuser.h`.
mod msg {
    /// Resolution, colour depth or the monitor set changed.
    pub const WM_DISPLAYCHANGE: u32 = 0x007E;
    /// A system parameter changed. `wparam` says which.
    pub const WM_SETTINGCHANGE: u32 = 0x001A;
    /// The DPI of the monitor under this window changed.
    pub const WM_DPICHANGED: u32 = 0x02E0;
    /// A terminal services session event.
    pub const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
    /// A power management event.
    pub const WM_POWERBROADCAST: u32 = 0x0218;
    /// Windows is asking whether this process minds the session ending.
    pub const WM_QUERYENDSESSION: u32 = 0x0011;
    /// The session really is ending. `wparam` is false for a cancelled one.
    pub const WM_ENDSESSION: u32 = 0x0016;

    /// `SPI_SETWORKAREA`, sent when an appbar such as the taskbar moves.
    pub const SPI_SETWORKAREA: usize = 0x002F;

    /// The console session connected to this session.
    pub const WTS_CONSOLE_CONNECT: usize = 0x1;
    /// The console session disconnected from this session.
    pub const WTS_CONSOLE_DISCONNECT: usize = 0x2;
    /// The session was locked.
    pub const WTS_SESSION_LOCK: usize = 0x7;
    /// The session was unlocked.
    pub const WTS_SESSION_UNLOCK: usize = 0x8;

    /// The machine woke up on its own, for example for a scheduled task.
    pub const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
    /// The machine woke up because the user asked it to.
    pub const PBT_APMRESUMESUSPEND: usize = 0x0007;

    /// `NOTIFY_FOR_THIS_SESSION`.
    pub const NOTIFY_FOR_THIS_SESSION: u32 = 0;
}

thread_local! {
    /// Set on the message thread before the window is created.
    static SENDER: RefCell<Option<EventSender>> = const { RefCell::new(None) };
}

fn emit(event: Event) {
    SENDER.with(|cell| {
        if let Some(tx) = cell.borrow().as_ref() {
            let _ = tx.send(event);
        }
    });
}

unsafe extern "system" fn wndproc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // Logoff, restart and shutdown. Nothing else in the process hears
        // about these: the console control handler's logoff and shutdown
        // events are documented as reaching services only, and an interactive
        // program is killed before they would fire. This hidden window is a
        // real top-level window, so it is the one thing that does get asked,
        // and it used to fall through to DefWindowProc, which answers "fine"
        // and lets the process be terminated with every hidden window still
        // cloaked and every faded window still translucent. With no second
        // window manager on the machine there is nothing to undo that.
        //
        // The restore runs here, synchronously, because there is no promise of
        // another message afterwards.
        msg::WM_QUERYENDSESSION => {
            tracing::warn!("the session is ending, putting the windows back");
            crate::safety::restore_all();
            LRESULT(1)
        }
        msg::WM_ENDSESSION => {
            // Only when it really is ending; wparam is false for a session end
            // that something else cancelled, and then the windows are wanted
            // back under management, which the next retile does.
            if wparam.0 != 0 {
                crate::safety::restore_all();
            }
            LRESULT(0)
        }
        msg::WM_DISPLAYCHANGE => {
            emit(Event::Monitors(MonitorEventKind::DisplayChange));
            LRESULT(0)
        }
        msg::WM_SETTINGCHANGE => {
            if wparam.0 == msg::SPI_SETWORKAREA {
                emit(Event::Monitors(MonitorEventKind::WorkAreaChange));
            }
            LRESULT(0)
        }
        msg::WM_DPICHANGED => {
            emit(Event::Monitors(MonitorEventKind::DpiChange));
            LRESULT(0)
        }
        msg::WM_WTSSESSION_CHANGE => {
            let kind = match wparam.0 {
                msg::WTS_SESSION_LOCK => Some(SessionChangeKind::Lock),
                msg::WTS_SESSION_UNLOCK => Some(SessionChangeKind::Unlock),
                msg::WTS_CONSOLE_CONNECT => Some(SessionChangeKind::ConsoleConnect),
                msg::WTS_CONSOLE_DISCONNECT => Some(SessionChangeKind::ConsoleDisconnect),
                _ => None,
            };
            if let Some(kind) = kind {
                emit(Event::Session(kind));
            }
            LRESULT(0)
        }
        msg::WM_POWERBROADCAST => {
            if matches!(
                wparam.0,
                msg::PBT_APMRESUMEAUTOMATIC | msg::PBT_APMRESUMESUSPEND
            ) {
                emit(Event::Session(SessionChangeKind::Resume));
            }
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// The hidden window and the thread that pumps it.
pub struct MessageWindow {
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

impl MessageWindow {
    /// Creates the window on a fresh thread and starts pumping messages.
    pub fn start(tx: EventSender) -> Result<Self> {
        let (ready_tx, ready_rx) = channel::<Result<u32, String>>();

        let handle = std::thread::Builder::new()
            .name("mochi-messages".into())
            .spawn(move || message_thread(tx, ready_tx))
            .context("could not spawn the message window thread")?;

        let thread_id = ready_rx
            .recv()
            .context("the message window thread died during startup")?
            .map_err(|e| anyhow!("{e}"))?;

        tracing::debug!(thread_id, "hidden message window ready");
        Ok(Self {
            thread_id,
            handle: Some(handle),
        })
    }

    /// Destroys the window and joins the thread.
    pub fn stop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        if let Err(e) = unsafe {
            PostThreadMessageW(
                self.thread_id,
                WM_QUIT,
                Default::default(),
                Default::default(),
            )
        } {
            tracing::warn!(error = %e, "could not post WM_QUIT to the message window thread");
        }
        super::join_before(handle, super::STOP_DEADLINE, "message window");
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        self.stop();
    }
}

fn message_thread(tx: EventSender, ready: Sender<Result<u32, String>>) {
    SENDER.with(|cell| *cell.borrow_mut() = Some(tx));

    let instance = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(format!("GetModuleHandleW failed: {e}")));
            return;
        }
    };

    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: w!("MochiEventWindow"),
        ..Default::default()
    };
    // A zero atom usually means the class is already registered, which is fine.
    let _ = unsafe { RegisterClassW(&raw const class) };

    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("MochiEventWindow"),
            w!("Mochi"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    };
    let window = match window {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(format!("CreateWindowExW failed: {e}")));
            return;
        }
    };

    // Without this the window never sees WM_WTSSESSION_CHANGE.
    if let Err(e) = unsafe { WTSRegisterSessionNotification(window, msg::NOTIFY_FOR_THIS_SESSION) }
    {
        tracing::warn!(error = %e, "session change notifications are unavailable");
    }

    let thread_id = unsafe { GetCurrentThreadId() };
    if ready.send(Ok(thread_id)).is_err() {
        let _ = unsafe { DestroyWindow(window) };
        return;
    }

    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&raw mut message, None, 0, 0) };
        if result.0 <= 0 {
            if result.0 < 0 {
                tracing::error!("GetMessageW failed on the message window thread");
            }
            break;
        }
        unsafe {
            let _ = TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }

    let _ = unsafe { WTSUnRegisterSessionNotification(window) };
    let _ = unsafe { DestroyWindow(window) };
    SENDER.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_comes_up_and_goes_away_again() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut window = MessageWindow::start(tx).expect("window should be created");
        window.stop();
        // A second stop must not hang or panic.
        window.stop();
    }
}
