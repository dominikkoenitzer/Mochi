//! Per-window transparency for unfocused windows.
//!
//! This is the one place in the crate that touches a window Mochi did not
//! create, so it is deliberately small: set a style bit, set an alpha, put both
//! back. [`TransparencyManager`] is the bookkeeping on top of those three
//! calls, so that the daemon can hand it a whole unfocused set per pass and
//! nothing is faded twice or left faded.
//!
//! # The gotcha
//!
//! `WS_EX_LAYERED` changes how a window is composited, and not every toolkit
//! copes:
//!
//! - **Electron** apps (Discord, VS Code, Spotify) often go black, keep a stale
//!   frame or stop repainting until they are resized. Chromium draws through its
//!   own compositor and does not always notice that the layered style arrived.
//! - **UWP and WinUI** windows (Settings, Terminal, Photos) are hosted in
//!   `ApplicationFrameWindow` and the alpha lands on the frame rather than the
//!   content, or is ignored outright.
//! - Windows that already set `WS_EX_LAYERED` themselves, for their own
//!   translucency or their own colour key, are ruined by
//!   [`clear_alpha`] taking the bit away again. Check [`is_layered`] **before**
//!   the first [`set_alpha`] and leave those windows alone.
//! - A layered window loses hardware overlay, so video and games can drop
//!   frames.
//!
//! The daemon therefore keeps a `transparency_ignore` list of rules, exactly
//! like the ignore rules for tiling, and skips those windows here. There is no
//! way to detect the problem from the outside, so the list is the only cure.

use std::collections::{BTreeMap, BTreeSet};

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
    WINDOW_EX_STYLE, WS_EX_LAYERED,
};

use crate::{Result, WindowHandle};

/// Fully opaque, the alpha a window has when nothing has touched it.
pub const OPAQUE: u8 = 255;

/// `true` when the window already has `WS_EX_LAYERED`.
///
/// Call this before the first [`set_alpha`] on a window and remember the
/// answer: a window that was layered on its own must never have the bit taken
/// away again.
#[must_use]
pub fn is_layered(hwnd: HWND) -> bool {
    crate::win::ex_style(hwnd).contains(WS_EX_LAYERED)
}

/// Fades a window to `alpha`, where 0 is invisible and 255 is opaque.
///
/// Sets `WS_EX_LAYERED` if the window does not have it yet, then applies a
/// whole-window alpha with `LWA_ALPHA`.
///
/// An [`OPAQUE`] alpha never brings the style with it: a window that is not
/// layered is opaque already, so layering it would be every hazard in the
/// module header for no visible change. A window that is already layered does
/// get the opaque alpha applied, rather than losing the style, so that the
/// daemon can go back and forth without the window flickering; use
/// [`clear_alpha`] to really put it back the way it was.
///
/// The style and the alpha go together: if the alpha cannot be applied the
/// style bit is taken off again, so a window that refuses is left exactly as it
/// was found rather than layered for good.
///
/// # Errors
///
/// [`crate::RenderError::Win32`] when the window has gone away or refuses the
/// style.
pub fn set_alpha(hwnd: HWND, alpha: u8) -> Result<()> {
    if !crate::win::is_window(hwnd) {
        return Err(windows::core::Error::from_thread().into());
    }

    let previous = current_ex_style(hwnd);
    let was_layered = previous.contains(WS_EX_LAYERED);
    if !was_layered {
        if alpha == OPAQUE {
            return Ok(());
        }
        set_ex_style(hwnd, previous | WS_EX_LAYERED)?;
    }

    // SAFETY: the window is live and now layered, which is what
    // SetLayeredWindowAttributes requires. LWA_ALPHA ignores the colour key, so
    // the zero passed for it is not read.
    let applied = unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA) };
    if let Err(error) = applied {
        if !was_layered {
            // Half a fade is worse than none: the style would stay on the
            // window for good, and the next pass would read it as somebody
            // else's compositing and never take it off again.
            let _ = set_ex_style(hwnd, previous);
        }
        return Err(error.into());
    }
    Ok(())
}

/// Puts a window back to fully opaque and removes `WS_EX_LAYERED`.
///
/// Do not call this on a window that was already layered before Mochi saw it;
/// see [`is_layered`].
///
/// # Errors
///
/// [`crate::RenderError::Win32`] when the window has gone away or refuses the
/// style.
pub fn clear_alpha(hwnd: HWND) -> Result<()> {
    if !crate::win::is_window(hwnd) {
        return Err(windows::core::Error::from_thread().into());
    }

    if !is_layered(hwnd) {
        return Ok(());
    }

    // Back to opaque first: taking the style away from a window that is
    // currently faded can leave the last composited frame on screen until
    // something repaints it.
    // SAFETY: the window is live and layered.
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), OPAQUE, LWA_ALPHA) }?;

    let style = WINDOW_EX_STYLE(current_ex_style(hwnd).0 & !WS_EX_LAYERED.0);
    set_ex_style(hwnd, style)?;
    Ok(())
}

/// The window's extended style bits.
fn current_ex_style(hwnd: HWND) -> WINDOW_EX_STYLE {
    // SAFETY: the caller has checked the handle with IsWindow. A window that
    // dies in between returns 0, which the callers treat as "no bits set".
    WINDOW_EX_STYLE(unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32)
}

/// Writes the window's extended style bits back.
fn set_ex_style(hwnd: HWND, style: WINDOW_EX_STYLE) -> Result<()> {
    // SetWindowLongPtrW returns the previous value and reports failure by
    // returning zero with a set error code, so the thread error is cleared
    // first to tell the two apart.
    // SAFETY: the handle was checked by the caller; only the extended style
    // word is written, with a value derived from the one just read.
    let previous = unsafe {
        windows::Win32::Foundation::SetLastError(windows::Win32::Foundation::WIN32_ERROR(0));
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style.0 as isize)
    };

    if previous == 0 {
        let error = windows::core::Error::from_thread();
        if error.code().is_err() {
            return Err(error.into());
        }
    }
    Ok(())
}

/// The three window operations a [`TransparencyManager`] performs.
///
/// The real implementation is [`Win32Alpha`], which is the three free functions
/// above; the tests use a fake, which is how the bookkeeping is tested without
/// a desktop.
pub trait WindowAlpha {
    /// Whether the window was layered before Mochi touched it.
    fn is_layered(&self, handle: WindowHandle) -> bool;

    /// Fades the window.
    ///
    /// # Errors
    ///
    /// When the window has gone away or refuses the style.
    fn set_alpha(&self, handle: WindowHandle, alpha: u8) -> Result<()>;

    /// Puts the window back the way it was.
    ///
    /// # Errors
    ///
    /// When the window has gone away or refuses the style.
    fn clear_alpha(&self, handle: WindowHandle) -> Result<()>;
}

/// The real window operations.
#[derive(Debug, Clone, Copy, Default)]
pub struct Win32Alpha;

impl WindowAlpha for Win32Alpha {
    fn is_layered(&self, handle: WindowHandle) -> bool {
        is_layered(handle.hwnd())
    }

    fn set_alpha(&self, handle: WindowHandle, alpha: u8) -> Result<()> {
        set_alpha(handle.hwnd(), alpha)
    }

    fn clear_alpha(&self, handle: WindowHandle) -> Result<()> {
        clear_alpha(handle.hwnd())
    }
}

/// Fades the unfocused windows and keeps track of which ones it faded.
///
/// The daemon hands over the whole unfocused set after every focus change:
/// everything in it is faded to [`TransparencyManager::alpha`], everything this
/// manager faded before and that is no longer in it is put back, and everything
/// else is left alone. A window that was already layered when it was first seen
/// is remembered and never touched again, because taking the style away from it
/// would break whatever it was using it for.
#[derive(Debug, Clone)]
pub struct TransparencyManager<A: WindowAlpha = Win32Alpha> {
    alpha: u8,
    backend: A,
    /// The windows this manager faded, and the alpha it last set on them.
    faded: BTreeMap<isize, u8>,
    /// The windows that were layered before this manager saw them.
    foreign: BTreeSet<isize>,
}

impl TransparencyManager<Win32Alpha> {
    /// A manager that fades unfocused windows to `alpha`.
    #[must_use]
    pub fn new(alpha: u8) -> Self {
        Self::with_backend(alpha, Win32Alpha)
    }
}

impl<A: WindowAlpha> TransparencyManager<A> {
    /// A manager over explicit window operations.
    #[must_use]
    pub fn with_backend(alpha: u8, backend: A) -> Self {
        Self {
            alpha,
            backend,
            faded: BTreeMap::new(),
            foreign: BTreeSet::new(),
        }
    }

    /// The alpha unfocused windows are faded to.
    #[must_use]
    pub const fn alpha(&self) -> u8 {
        self.alpha
    }

    /// Changes the alpha. The next [`TransparencyManager::update`] applies it
    /// to every window that is still faded.
    pub const fn set_alpha(&mut self, alpha: u8) {
        self.alpha = alpha;
    }

    /// How many windows are faded right now.
    #[must_use]
    pub fn faded_count(&self) -> usize {
        self.faded.len()
    }

    /// `true` when this manager has faded that window.
    #[must_use]
    pub fn is_faded(&self, handle: WindowHandle) -> bool {
        self.faded.contains_key(&handle.0)
    }

    /// `true` when that window was layered before the manager saw it, and is
    /// therefore left alone.
    #[must_use]
    pub fn is_foreign(&self, handle: WindowHandle) -> bool {
        self.foreign.contains(&handle.0)
    }

    /// Fades everything in `unfocused` and puts everything else back.
    ///
    /// A window that is already faded to the same alpha costs nothing, so this
    /// is cheap enough to call after every focus change. A window that refuses
    /// the call, normally because it has just died, keeps whatever the
    /// bookkeeping already said about it and the rest of the set is still
    /// applied.
    ///
    /// # Errors
    ///
    /// The first error any window reported, after every other window has been
    /// dealt with.
    pub fn update(&mut self, unfocused: &[WindowHandle]) -> Result<()> {
        let mut failure = None;
        let wanted: BTreeSet<isize> = unfocused.iter().map(|handle| handle.0).collect();

        for handle in unfocused {
            if self.foreign.contains(&handle.0) {
                continue;
            }
            if self.faded.get(&handle.0) == Some(&self.alpha) {
                continue;
            }
            if !self.faded.contains_key(&handle.0) && self.backend.is_layered(*handle) {
                // Somebody else owns this window's compositing.
                self.foreign.insert(handle.0);
                continue;
            }
            match self.backend.set_alpha(*handle, self.alpha) {
                Ok(()) => {
                    self.faded.insert(handle.0, self.alpha);
                }
                Err(error) => {
                    // Whatever alpha this manager set on an earlier pass is
                    // still on the window: [`set_alpha`] puts a window it
                    // could not fade back the way it found it, so a refusal
                    // here never leaves a state nobody is tracking.
                    failure.get_or_insert(error);
                }
            }
        }

        let stale: Vec<isize> = self
            .faded
            .keys()
            .copied()
            .filter(|key| !wanted.contains(key))
            .collect();
        for key in stale {
            match self.backend.clear_alpha(WindowHandle(key)) {
                Ok(()) => {
                    self.faded.remove(&key);
                }
                // The window is still faded, so it stays in the bookkeeping
                // and the next pass tries again. Forgetting it here would
                // leave it translucent with nothing left to put it back.
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }

        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Puts every window this manager faded back, and forgets everything.
    ///
    /// This is the restore path: the daemon calls it when transparency is
    /// switched off and on shutdown. Afterwards the manager is as fresh as a
    /// new one, so the next update checks every window again, except for any
    /// window that could not be put back.
    ///
    /// # Errors
    ///
    /// The first error any window reported. Every window is still attempted,
    /// and a window that could not be put back keeps its entry, because it is
    /// still faded and somebody has to try again.
    pub fn clear_all(&mut self) -> Result<()> {
        let mut failure = None;
        let mut still_faded = BTreeMap::new();
        for (key, alpha) in std::mem::take(&mut self.faded) {
            if let Err(error) = self.backend.clear_alpha(WindowHandle(key)) {
                still_faded.insert(key, alpha);
                failure.get_or_insert(error);
            }
        }
        self.faded = still_faded;
        self.foreign.clear();

        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A handle that is certainly not a window. The functions must refuse it
    /// rather than reach into the window manager.
    #[test]
    fn a_dead_handle_is_refused() {
        let dead = HWND(std::ptr::null_mut());
        assert!(!is_layered(dead));
        assert!(set_alpha(dead, 235).is_err());
        assert!(clear_alpha(dead).is_err());
    }

    #[test]
    fn opaque_is_full_alpha() {
        assert_eq!(OPAQUE, u8::MAX);
    }

    const A: WindowHandle = WindowHandle(0x1111);
    const B: WindowHandle = WindowHandle(0x2222);
    const OWN: WindowHandle = WindowHandle(0x3333);
    const DEAD: WindowHandle = WindowHandle(0x4444);
    /// A window that takes an alpha but never gives it back.
    const STUCK: WindowHandle = WindowHandle(0x5555);

    /// What a fake window did, so a test can assert on the calls themselves
    /// rather than on the bookkeeping only.
    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Set(isize, u8),
        Clear(isize),
    }

    /// A desktop of four windows: two ordinary, one that is layered already and
    /// one that has died.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<Call>>,
        /// Windows that refuse a new alpha from now on, on top of [`DEAD`].
        refusing: RefCell<BTreeSet<isize>>,
    }

    impl Fake {
        fn calls(&self) -> std::cell::Ref<'_, Vec<Call>> {
            self.calls.borrow()
        }

        fn forget(&self) {
            self.calls.borrow_mut().clear();
        }

        /// From now on this window refuses a new alpha, keeping the one it has.
        fn refuse(&self, handle: WindowHandle) {
            self.refusing.borrow_mut().insert(handle.0);
        }

        /// Undoes [`Fake::refuse`].
        fn allow(&self, handle: WindowHandle) {
            self.refusing.borrow_mut().remove(&handle.0);
        }
    }

    impl WindowAlpha for &Fake {
        fn is_layered(&self, handle: WindowHandle) -> bool {
            handle == OWN
        }

        fn set_alpha(&self, handle: WindowHandle, alpha: u8) -> Result<()> {
            self.calls.borrow_mut().push(Call::Set(handle.0, alpha));
            if handle == DEAD || self.refusing.borrow().contains(&handle.0) {
                return Err(crate::RenderError::ThreadGone("fake window"));
            }
            Ok(())
        }

        fn clear_alpha(&self, handle: WindowHandle) -> Result<()> {
            self.calls.borrow_mut().push(Call::Clear(handle.0));
            if handle == STUCK {
                return Err(crate::RenderError::ThreadGone("fake window"));
            }
            Ok(())
        }
    }

    #[test]
    fn the_unfocused_set_is_faded_and_the_rest_put_back() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);
        assert_eq!(manager.alpha(), 235);

        manager.update(&[A, B]).unwrap();
        assert_eq!(
            *fake.calls(),
            vec![Call::Set(A.0, 235), Call::Set(B.0, 235)]
        );
        assert_eq!(manager.faded_count(), 2);

        // A takes the focus, so it goes back to opaque and B stays faded.
        fake.forget();
        manager.update(&[B]).unwrap();
        assert_eq!(*fake.calls(), vec![Call::Clear(A.0)], "B was already faded");
        assert!(!manager.is_faded(A));
        assert!(manager.is_faded(B));
    }

    #[test]
    fn an_unchanged_set_costs_nothing() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);
        manager.update(&[A, B]).unwrap();

        fake.forget();
        manager.update(&[A, B]).unwrap();
        assert!(fake.calls().is_empty(), "nothing changed, nothing was set");
    }

    #[test]
    fn a_window_that_was_already_layered_is_never_touched() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);

        manager.update(&[A, OWN]).unwrap();
        assert_eq!(*fake.calls(), vec![Call::Set(A.0, 235)]);
        assert!(manager.is_foreign(OWN));
        assert!(!manager.is_faded(OWN));

        // Not on the way back out either: clear_alpha would take its style bit.
        fake.forget();
        manager.update(&[]).unwrap();
        assert_eq!(*fake.calls(), vec![Call::Clear(A.0)]);

        fake.forget();
        manager.update(&[OWN]).unwrap();
        assert!(fake.calls().is_empty(), "it is left alone for good");
    }

    #[test]
    fn a_new_alpha_is_applied_to_everything_still_faded() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);
        manager.update(&[A, B]).unwrap();

        fake.forget();
        manager.set_alpha(200);
        manager.update(&[A, B]).unwrap();
        assert_eq!(
            *fake.calls(),
            vec![Call::Set(A.0, 200), Call::Set(B.0, 200)]
        );
    }

    #[test]
    fn a_window_that_refuses_is_dropped_and_the_rest_still_runs() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);

        let failure = manager.update(&[DEAD, A]);
        assert!(failure.is_err(), "the caller hears about it");
        assert!(!manager.is_faded(DEAD));
        assert!(manager.is_faded(A), "A was faded anyway");
    }

    #[test]
    fn clear_all_puts_everything_back_and_forgets_it() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);
        manager.update(&[A, B, OWN]).unwrap();

        fake.forget();
        manager.clear_all().unwrap();
        assert_eq!(*fake.calls(), vec![Call::Clear(A.0), Call::Clear(B.0)]);
        assert_eq!(manager.faded_count(), 0);
        assert!(
            !manager.is_foreign(OWN),
            "a fresh manager checks every window again"
        );

        fake.forget();
        manager.clear_all().unwrap();
        assert!(fake.calls().is_empty(), "twice over is not an error");
    }

    /// A window this manager already faded and that then refuses a new alpha
    /// still carries the old one, so it has to stay in the bookkeeping;
    /// forgetting it leaves it translucent with nobody left to put it back.
    #[test]
    fn a_window_that_refuses_a_new_alpha_is_still_this_managers_to_put_back() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);
        manager.update(&[A]).unwrap();
        assert!(manager.is_faded(A));

        fake.refuse(A);
        manager.set_alpha(150);
        assert!(manager.update(&[A]).is_err(), "the caller hears about it");
        assert!(manager.is_faded(A), "235 is still on the window");

        fake.allow(A);
        fake.forget();
        manager.clear_all().unwrap();
        assert_eq!(*fake.calls(), vec![Call::Clear(A.0)], "and it is put back");
    }

    /// The restore path is the same the other way round: a window that cannot
    /// be put back is still faded, so the manager keeps it and tries again
    /// rather than dropping it on the floor.
    #[test]
    fn a_window_that_cannot_be_put_back_stays_in_the_bookkeeping() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);
        manager.update(&[STUCK]).unwrap();
        assert!(manager.is_faded(STUCK));

        // It takes the focus, so the manager tries to put it back and fails.
        fake.forget();
        assert!(manager.update(&[]).is_err());
        assert!(manager.is_faded(STUCK), "it is still faded");
        assert_eq!(*fake.calls(), vec![Call::Clear(STUCK.0)]);

        // Every later pass tries again.
        fake.forget();
        assert!(manager.update(&[]).is_err());
        assert_eq!(*fake.calls(), vec![Call::Clear(STUCK.0)], "tried again");
    }

    #[test]
    fn clear_all_keeps_the_window_it_could_not_put_back() {
        let fake = Fake::default();
        let mut manager = TransparencyManager::with_backend(235, &fake);
        manager.update(&[A, STUCK]).unwrap();

        fake.forget();
        assert!(manager.clear_all().is_err());
        assert!(
            !manager.is_faded(A),
            "A went back to opaque and is forgotten"
        );
        assert!(manager.is_faded(STUCK), "STUCK is still faded");

        fake.forget();
        assert!(manager.clear_all().is_err());
        assert_eq!(
            *fake.calls(),
            vec![Call::Clear(STUCK.0)],
            "the second call tries the one that is still faded, and only that one"
        );
    }

    /// A window of this process, created by the test and destroyed with it, so
    /// the Win32 functions can be exercised without touching anybody else's.
    struct TestWindow(HWND);

    impl TestWindow {
        fn new() -> Self {
            let class = crate::win::wide("STATIC");
            // SAFETY: STATIC is a predefined class, both arguments outlive the
            // call, and the window is a hidden, parentless popup of this
            // process which DestroyWindow takes down again in Drop.
            let hwnd = unsafe {
                windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    windows::core::PCWSTR(class.as_ptr()),
                    windows::core::PCWSTR::null(),
                    windows::Win32::UI::WindowsAndMessaging::WS_POPUP,
                    0,
                    0,
                    10,
                    10,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .expect("a window of our own");
            Self(hwnd)
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateWindowExW on this thread and
            // is destroyed exactly once.
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0) };
        }
    }

    /// Fading to [`OPAQUE`] changes nothing on screen, so it must not bring
    /// `WS_EX_LAYERED` with it: that is the whole hazard of the module header
    /// for no visible effect.
    #[test]
    fn an_opaque_alpha_does_not_layer_a_window() {
        let window = TestWindow::new();
        assert!(!is_layered(window.0));

        set_alpha(window.0, OPAQUE).expect("an opaque window is opaque already");
        assert!(!is_layered(window.0), "nothing was layered for nothing");

        // A real fade still layers, and an opaque alpha on top of it is still
        // applied rather than flickering the style off.
        set_alpha(window.0, 200).expect("a real fade");
        assert!(is_layered(window.0));
        set_alpha(window.0, OPAQUE).expect("back to opaque, still layered");
        assert!(is_layered(window.0));

        clear_alpha(window.0).expect("and the style comes off");
        assert!(!is_layered(window.0));
    }
}
