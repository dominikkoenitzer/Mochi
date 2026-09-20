//! Mochi's own hotkey daemon: a low-level keyboard hook on its own thread.
//!
//! `WH_KEYBOARD_LL` is the only mechanism that can bind a key Windows or another
//! application has already taken, and the only one that can hand a game every
//! key on demand. It comes with two duties, and this module exists to keep both.
//!
//! **Be fast.** The callback runs inside the raw input path for every key press
//! on the desktop. Windows silently removes a hook whose callback overruns
//! `LowLevelHooksTimeout` (300 ms by default), so this one does a hash lookup
//! and a non-blocking channel send and nothing else. No locks: the bindings live
//! in thread-local storage on the hook thread and a reload is *posted* to that
//! thread as a message rather than shared with it, and the one thing that is
//! shared, which of them are live, is a single atomic.
//!
//! **Swallow nothing that was not asked for.** A key press is only ever
//! withheld from the desktop when it matches a binding exactly. Every other key,
//! including every key that merely starts with the right modifiers, is passed
//! straight on. If Mochi dies the hook dies with the process and the keyboard is
//! the system's again, which is why there is no state to repair after a crash.
//!
//! **Leave no menu open.** Swallowing the key but not the modifier leaves the
//! application in front with Alt going down and coming back up and nothing in
//! between, which `DefWindowProc` reads as "activate the menu bar". Measured on
//! a real window with a real menu bar, by
//! `an_alt_binding_does_not_leave_the_application_in_menu_mode` in
//! `crates/mochi/tests/e2e_testbed.rs`: every `alt + key` binding used to leave
//! the window sitting in its File menu. A swallowed press under Alt or Win now
//! injects one masking keystroke, so the modifier is no longer a bare press.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use mochi_hotkey::{Action, Bindings, Key, Modifiers, Shell, Trigger};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LWIN, VK_MENU,
    VK_RCONTROL, VK_RMENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN,
    WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::{Event, EventSender};

/// `HC_ACTION`: the hook has a real key event to look at.
const HC_ACTION: i32 = 0;

/// Hand the hook thread a new set of bindings. `wparam` is a leaked `Box`.
const MSG_REPLACE: u32 = WM_APP + 1;

/// Which bindings are live, as a [`Gate`] discriminant.
///
/// The one piece of hotkey state that is shared rather than posted. A gate
/// change has to be in force the moment the command that asked for it answers:
/// `mochic set-hotkeys disable` that returns while the next key press still
/// fires is wrong, and a message the hook thread has not read yet would do
/// exactly that. One integer is cheap to share and the hook reads it on every
/// press anyway.
static GATE: AtomicUsize = AtomicUsize::new(0);

/// Which bindings the hook lets through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gate {
    /// Every binding fires. The normal state.
    #[default]
    All,
    /// Only the binding that leaves game mode fires, so the game in front gets
    /// every other key on the keyboard.
    GameMode,
    /// Nothing fires. The hook stays installed and swallows nothing at all;
    /// `mochic set-hotkeys enable` from a terminal brings it back.
    Off,
}

impl Gate {
    /// A short name for logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::GameMode => "game-mode",
            Self::Off => "off",
        }
    }

    const fn from_usize(value: usize) -> Self {
        match value {
            1 => Self::GameMode,
            2 => Self::Off,
            _ => Self::All,
        }
    }

    const fn as_usize(self) -> usize {
        match self {
            Self::All => 0,
            Self::GameMode => 1,
            Self::Off => 2,
        }
    }

    /// Whether an action survives this gate.
    fn admits(self, action: &Action) -> bool {
        match self {
            Self::All => true,
            Self::GameMode => matches!(
                action,
                Action::Command(mochi_client::Command::ToggleGameMode)
            ),
            Self::Off => false,
        }
    }
}

thread_local! {
    /// The bindings the hook matches against. Only the hook thread touches this.
    static BINDINGS: RefCell<Bindings> = RefCell::new(Bindings::default());
    /// Where a match is reported.
    static SENDER: RefCell<Option<EventSender>> = const { RefCell::new(None) };
    /// Keys whose press was swallowed, so their release can be swallowed too.
    ///
    /// An application that never saw the press must not see the release either;
    /// it would read as a key that went up on its own. Eight slots are more
    /// than a human can hold down at once.
    static SWALLOWED: RefCell<[u16; 8]> = const { RefCell::new([0; 8]) };
}

/// True while the key is physically down, according to the async key state.
fn down(key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    // The high bit is the physical state; the low bit is the toggle state and
    // would make CapsLock look like a permanently held modifier.
    (unsafe { GetAsyncKeyState(i32::from(key.0)) } as u16 & 0x8000) != 0
}

/// The modifiers held right now.
///
/// AltGr is the reason this is not four `GetAsyncKeyState` calls. On a Swiss
/// German layout, and every other layout with an AltGr, the key reports itself
/// as right Alt *and* left Ctrl, so `@`, `[`, `]`, `{`, `}` and `|` would all
/// look like Ctrl+Alt combinations and a Ctrl+Alt binding would eat them. When
/// the pair is seen together, neither half counts: left Ctrl is discarded and
/// right Alt with it, leaving the other three real modifier keys to speak for
/// themselves.
fn current_modifiers() -> Modifiers {
    let altgr = down(VK_RMENU) && down(VK_LCONTROL);
    let alt = if altgr { down(VK_LMENU) } else { down(VK_MENU) };
    let ctrl = if altgr {
        down(VK_RCONTROL)
    } else {
        down(VK_CONTROL)
    };
    Modifiers {
        alt,
        ctrl,
        shift: down(VK_SHIFT),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

/// True for the keys that only ever qualify another key.
///
/// A modifier on its own is never a trigger, so the hook can stop early and
/// never has to consider swallowing one. Anything that swallowed a modifier
/// would strand every application that is watching for it to come back up.
const fn is_modifier(vk: u16) -> bool {
    matches!(
        vk,
        0x10 | 0x11 | 0x12 | 0xA0..=0xA5 | 0x5B | 0x5C // shift, ctrl, alt, their sides, both Win keys
    )
}

/// Records a swallowed press. True when this key was not already down, which
/// is how auto-repeat is told apart from a fresh press.
fn remember_swallowed(vk: u16) -> bool {
    SWALLOWED.with(|cell| {
        let mut keys = cell.borrow_mut();
        if keys.contains(&vk) {
            return false;
        }
        match keys.iter_mut().find(|slot| **slot == 0) {
            Some(slot) => {
                *slot = vk;
                true
            }
            None => false,
        }
    })
}

/// Whether this release belongs to a press that was swallowed, clearing it.
fn take_swallowed(vk: u16) -> bool {
    SWALLOWED.with(|cell| {
        let mut keys = cell.borrow_mut();
        match keys.iter_mut().find(|slot| **slot == vk) {
            Some(slot) => {
                *slot = 0;
                true
            }
            None => false,
        }
    })
}

/// The mark on every key event Mochi injects itself, read back out of
/// `KBDLLHOOKSTRUCT::dwExtraInfo`. "MOCH" as four bytes.
///
/// This is what stops the mask below from feeding itself back: the hook hands
/// a marked event straight to the next hook without looking at it, so it can
/// never match a binding, never be swallowed, and never produce a mask of its
/// own. One integer compare, at the top of the callback.
const MASK_TAG: usize = 0x4D4F_4348;

/// The key the mask presses: plain Ctrl.
///
/// It has to be a key that does nothing on its own in any application, and
/// Ctrl is the one key that is *defined* to do nothing on its own: it only ever
/// qualifies something else. It is also a modifier, and a modifier is never a
/// trigger here, so even without the tag above it could not match a binding or
/// be swallowed. Everything else considered was worse: a function key can be
/// bound by the application in front, `VK_NONCONVERT` does something real under
/// a Japanese IME, and a character key would type.
const MASK_KEY: u16 = 0x11;

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION {
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Mochi's own mask, on its way out. Never looked at twice.
        if event.dwExtraInfo == MASK_TAG {
            return unsafe { CallNextHookEx(None, code, wparam, lparam) };
        }

        let vk = event.vkCode as u16;
        let message = wparam.0 as u32;

        if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
            if !is_modifier(vk)
                && let Some(modifiers) = press(vk)
            {
                // Only the first press of a held key masks: auto-repeat is
                // still inside the same Alt, which is already masked.
                if remember_swallowed(vk) && opens_a_menu(modifiers) {
                    mask_the_modifier();
                }
                return LRESULT(1);
            }
        } else if matches!(message, WM_KEYUP | WM_SYSKEYUP) && take_swallowed(vk) {
            return LRESULT(1);
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Whether these modifiers open a menu when they go down and come up again
/// with nothing in between.
///
/// Alt is the measured one: on a window with a menu bar, `DefWindowProc` reads
/// that sequence as "activate the menu bar", and since the key in the middle
/// was swallowed the application never saw anything in between. The Win key is
/// the same sequence with the Start menu. Ctrl and Shift open nothing.
const fn opens_a_menu(modifiers: Modifiers) -> bool {
    modifiers.alt || modifiers.win
}

/// Gives the application in front one keystroke to see between the modifier
/// going down and coming up, so that the modifier no longer reads as a bare
/// press.
///
/// Costs one `SendInput` of two events inside the hook callback, which is a few
/// microseconds and only on a press that was Mochi's anyway. The events are
/// tagged, so when they come back through this hook they are passed on at the
/// first line; nothing can loop here.
fn mask_the_modifier() {
    let event = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(MASK_KEY),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: MASK_TAG,
            },
        },
    };

    let inputs = [event(false), event(true)];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        // Nothing to repair: the worst case is the menu bar the mask was
        // meant to prevent, which is what the desktop did before it existed.
        tracing::debug!("the desktop refused the masking keystroke");
    }
}

/// Matches one key press and reports it. The modifiers that were held when it
/// matched, or `None` when the press was not ours.
fn press(vk: u16) -> Option<Modifiers> {
    let trigger = Trigger {
        modifiers: current_modifiers(),
        key: Key::new(vk),
    };

    let gate = Gate::from_usize(GATE.load(Ordering::Relaxed));
    let action = BINDINGS.with(|cell| {
        let bindings = cell.borrow();
        let binding = bindings.get(trigger)?;
        gate.admits(&binding.action).then(|| binding.action.clone())
    });

    let action = action?;

    SENDER.with(|cell| {
        if let Some(tx) = cell.borrow().as_ref() {
            // A full or closed channel means the loop is gone or hopelessly
            // behind. Dropping the key press is the right answer either way:
            // the hook must not block the desktop's input.
            let _ = tx.send(Event::Hotkey {
                trigger,
                action: Box::new(action),
            });
        }
    });
    Some(trigger.modifiers)
}

/// The keyboard hook and the thread that pumps it.
pub struct HotkeyDaemon {
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
    count: usize,
}

impl HotkeyDaemon {
    /// Installs the hook on a fresh thread and starts matching.
    pub fn start(tx: EventSender, bindings: Bindings) -> Result<Self> {
        let (ready_tx, ready_rx) = channel::<Result<u32, String>>();
        let count = bindings.len();
        GATE.store(Gate::All.as_usize(), Ordering::Relaxed);

        let handle = std::thread::Builder::new()
            .name("mochi-hotkeys".into())
            .spawn(move || hotkey_thread(tx, bindings, ready_tx))
            .context("could not spawn the hotkey thread")?;

        let thread_id = ready_rx
            .recv()
            .context("the hotkey thread died during startup")?
            .map_err(|e| anyhow!("{e}"))?;

        tracing::info!(thread_id, bindings = count, "hotkeys ready");
        Ok(Self {
            thread_id,
            handle: Some(handle),
            count,
        })
    }

    /// How many bindings are loaded.
    pub const fn bindings(&self) -> usize {
        self.count
    }

    /// Which bindings are live.
    pub fn gate(&self) -> Gate {
        Gate::from_usize(GATE.load(Ordering::Relaxed))
    }

    /// Hands the hook thread a new set of bindings, for a reload.
    ///
    /// The box is leaked into the message and claimed again on the other side.
    /// If the post fails the thread is already gone, so it is reclaimed here
    /// instead.
    pub fn replace(&mut self, bindings: Bindings) {
        self.count = bindings.len();
        let raw = Box::into_raw(Box::new(bindings));
        if let Err(e) = unsafe {
            PostThreadMessageW(self.thread_id, MSG_REPLACE, WPARAM(raw as usize), LPARAM(0))
        } {
            tracing::warn!(error = %e, "could not hand the new bindings to the hotkey thread");
            drop(unsafe { Box::from_raw(raw) });
        }
    }

    /// Changes which bindings fire.
    ///
    /// In force by the time this returns: the hook reads the gate on every key
    /// press, so there is no window in which a key the user just turned off
    /// still acts.
    pub fn set_gate(&mut self, gate: Gate) {
        GATE.store(gate.as_usize(), Ordering::Relaxed);
        tracing::info!(gate = gate.as_str(), "hotkey gate");
    }

    /// Removes the hook and joins the thread.
    pub fn stop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        if let Err(e) = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
        {
            tracing::warn!(error = %e, "could not post WM_QUIT to the hotkey thread");
        }
        if handle.join().is_err() {
            tracing::error!("the hotkey thread panicked");
        } else {
            tracing::debug!("hotkeys stopped");
        }
    }
}

impl Drop for HotkeyDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn hotkey_thread(tx: EventSender, bindings: Bindings, ready: Sender<Result<u32, String>>) {
    BINDINGS.with(|cell| *cell.borrow_mut() = bindings);
    SENDER.with(|cell| *cell.borrow_mut() = Some(tx));

    let instance = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(format!("GetModuleHandleW failed: {e}")));
            return;
        }
    };

    // The hook has to be installed from the thread that pumps it: the callback
    // is delivered to this thread's message queue.
    let hook = match install(instance.into()) {
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

    let hook = pump(hook, instance.into());

    let _ = unsafe { UnhookWindowsHookEx(hook) };
    SENDER.with(|cell| *cell.borrow_mut() = None);
    BINDINGS.with(|cell| *cell.borrow_mut() = Bindings::default());
}

/// Installs the keyboard hook. Must be called on the thread that pumps it.
fn install(instance: HINSTANCE) -> windows::core::Result<HHOOK> {
    // Thread id 0 makes this a global hook.
    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance), 0) }
}

/// The message loop. Returns the live hook when `WM_QUIT` arrives.
fn pump(mut hook: HHOOK, instance: HINSTANCE) -> HHOOK {
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&raw mut message, None, 0, 0) };
        if result.0 <= 0 {
            if result.0 < 0 {
                tracing::error!("GetMessageW failed on the hotkey thread");
            }
            break;
        }

        match message.message {
            MSG_REPLACE => {
                // The other end leaked exactly one box into this message.
                let bindings = *unsafe { Box::from_raw(message.wParam.0 as *mut Bindings) };
                tracing::info!(bindings = bindings.len(), "hotkeys reloaded");
                BINDINGS.with(|cell| *cell.borrow_mut() = bindings);

                // Windows silently removes a low-level hook whose callback
                // once overran its timeout, and nothing tells the process it
                // happened. A reload is the user's way of saying a key stopped
                // working, so it always ends with a hook that is certainly
                // installed, and certainly at the front of the chain.
                match install(instance) {
                    Ok(fresh) => {
                        let _ = unsafe { UnhookWindowsHookEx(hook) };
                        hook = fresh;
                    }
                    Err(e) => tracing::error!(error = %e, "could not reinstall the keyboard hook"),
                }
            }
            _ => unsafe {
                let _ = TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            },
        }
    }
    hook
}

/// Starts a shell binding's command line, detached, with no console window.
///
/// Nothing is waited for and nothing is read back: a hotkey that starts a
/// program has done its job the moment the program is running. The child is
/// reaped by the system, not by Mochi.
pub fn run_shell(shell: Shell, line: &str) {
    use std::os::windows::process::CommandExt;

    /// `CREATE_NO_WINDOW`: no console flashes up for a `cmd` or `pwsh` line.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut command = std::process::Command::new(shell.program());
    match shell.raw_tail(line) {
        Some(tail) => command.raw_arg(tail),
        None => command.args(shell.args_for(line)),
    };
    command.creation_flags(CREATE_NO_WINDOW);

    match command.spawn() {
        Ok(child) => tracing::info!(pid = child.id(), shell = %shell, line, "hotkey ran a command"),
        Err(e) => tracing::error!(error = %e, shell = %shell, line, "hotkey command did not start"),
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LSHIFT, VK_RSHIFT};

    use super::*;

    fn bindings(text: &str) -> Bindings {
        Bindings::parse(text).expect("the fixture should parse")
    }

    #[test]
    fn the_hook_comes_up_and_goes_away_again() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut hotkeys = HotkeyDaemon::start(tx, bindings("alt + f13 : mochic retile\n"))
            .expect("hook installs");
        assert_eq!(hotkeys.bindings(), 1);
        hotkeys.stop();
        // A second stop must not hang or panic.
        hotkeys.stop();
    }

    #[test]
    fn a_reload_reaches_the_hook_thread() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut hotkeys = HotkeyDaemon::start(tx, bindings("alt + f13 : mochic retile\n"))
            .expect("hook installs");
        hotkeys.replace(bindings(
            "alt + f13 : mochic retile\nalt + f14 : mochic toggle-pause\n",
        ));
        assert_eq!(hotkeys.bindings(), 2);
        hotkeys.stop();
    }

    #[test]
    fn a_gate_change_is_in_force_the_moment_it_returns() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut hotkeys = HotkeyDaemon::start(tx, bindings("alt + f13 : mochic retile\n"))
            .expect("hook installs");
        assert_eq!(hotkeys.gate(), Gate::All);

        // Not "the hook thread will get to it": the next key press has to see
        // this already, whichever thread reads it.
        hotkeys.set_gate(Gate::Off);
        assert_eq!(Gate::from_usize(GATE.load(Ordering::Relaxed)), Gate::Off);
        assert_eq!(hotkeys.gate(), Gate::Off);

        hotkeys.set_gate(Gate::GameMode);
        assert_eq!(hotkeys.gate(), Gate::GameMode);
        hotkeys.stop();

        // A fresh daemon binds everything again, whatever the last one left.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut next = HotkeyDaemon::start(tx, bindings("alt + f13 : mochic retile\n"))
            .expect("hook installs");
        assert_eq!(next.gate(), Gate::All);
        next.stop();
    }

    #[test]
    fn game_mode_admits_only_the_way_out_of_it() {
        let leave = Action::Command(mochi_client::Command::ToggleGameMode);
        let other = Action::Command(mochi_client::Command::Retile);

        assert!(Gate::All.admits(&leave) && Gate::All.admits(&other));
        assert!(Gate::GameMode.admits(&leave));
        assert!(!Gate::GameMode.admits(&other));
        assert!(!Gate::Off.admits(&leave) && !Gate::Off.admits(&other));
    }

    #[test]
    fn every_modifier_key_is_known_to_be_one() {
        for vk in [
            VK_SHIFT,
            VK_CONTROL,
            VK_MENU,
            VK_LSHIFT,
            VK_RSHIFT,
            VK_LCONTROL,
            VK_RCONTROL,
            VK_LMENU,
            VK_RMENU,
            VK_LWIN,
            VK_RWIN,
        ] {
            assert!(is_modifier(vk.0), "{vk:?} should count as a modifier");
        }
        // The keys a binding actually triggers on must not.
        for vk in [0x41u16, 0x48, 0x20, 0x7C, 0x25] {
            assert!(
                !is_modifier(vk),
                "0x{vk:02X} should not count as a modifier"
            );
        }
    }

    #[test]
    fn a_swallowed_press_swallows_its_release_exactly_once() {
        assert!(!take_swallowed(0x48));
        assert!(remember_swallowed(0x48));
        // Auto-repeat presses the same key again before it comes up, and only
        // the first of those is a new press.
        assert!(!remember_swallowed(0x48));
        assert!(take_swallowed(0x48));
        assert!(!take_swallowed(0x48));
    }

    #[test]
    fn only_the_modifiers_that_open_a_menu_are_masked() {
        let none = Modifiers::default();
        assert!(!opens_a_menu(none));
        assert!(!opens_a_menu(Modifiers { ctrl: true, ..none }));
        assert!(!opens_a_menu(Modifiers {
            shift: true,
            ..none
        }));
        // Alt is the measured one, Win is the Start menu.
        assert!(opens_a_menu(Modifiers { alt: true, ..none }));
        assert!(opens_a_menu(Modifiers { win: true, ..none }));
    }

    #[test]
    fn the_masking_key_can_never_come_back_as_a_binding() {
        // Two independent reasons, and either one alone would be enough.
        // The tag: a marked event leaves the hook at the first line.
        assert_eq!(MASK_TAG, 0x4D4F_4348);
        // The key: a modifier is never a trigger, so it is never swallowed.
        assert!(is_modifier(MASK_KEY));
        assert_eq!(MASK_KEY, VK_CONTROL.0);
    }

    #[test]
    fn the_gate_survives_a_round_trip_through_a_message() {
        for gate in [Gate::All, Gate::GameMode, Gate::Off] {
            assert_eq!(Gate::from_usize(gate.as_usize()), gate);
        }
    }
}
