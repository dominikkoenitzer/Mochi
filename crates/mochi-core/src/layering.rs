//! Which windows can be faded without breaking how they draw.
//!
//! Fading an unfocused window means giving it `WS_EX_LAYERED` and an alpha.
//! That is safe for a window that paints into the surface Windows keeps for it,
//! and it is not for one that presents through `DirectComposition` instead.
//! Such a window is created with `WS_EX_NOREDIRECTIONBITMAP`: it has no
//! redirection surface, and once it is layered the compositor shows that
//! missing surface rather than its content. Chromium and every Electron app
//! (browsers, Discord, Spotify, VS Code) draw this way and turn into a flat
//! grey pane. UWP frames and other XAML hosts carry the same bit and are
//! left opaque for the same reason.
//!
//! The answer is decided here, from the window's own style bits, so that no
//! user has to find these apps one by one and name them in
//! `transparency_ignore_rules`. Those rules still apply on top.

/// The window has no redirection surface and presents through
/// `DirectComposition`. Value from `winuser.h`.
pub const WS_EX_NOREDIRECTIONBITMAP: u32 = 0x0020_0000;

/// The class family of every Chromium and Electron top-level window
/// (`Chrome_WidgetWin_0`, `Chrome_WidgetWin_1`, ...).
///
/// Chromium decides how it presents per machine and per GPU, and falls back to
/// other paths when the GPU process gives up, so the style bit alone is not a
/// promise for these windows. None of them fades reliably, so the family is
/// kept opaque whatever it reports.
pub const CHROMIUM_CLASS_PREFIX: &str = "Chrome_WidgetWin_";

/// `true` when the window can be layered and faded without losing its content.
///
/// `ex_style` is the window's extended style word and `class` its window class
/// name, both as Windows reports them.
#[must_use]
pub fn can_fade(ex_style: u32, class: &str) -> bool {
    ex_style & WS_EX_NOREDIRECTIONBITMAP == 0 && !class.starts_with(CHROMIUM_CLASS_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Extended styles read from live windows on Windows 11.
    const WS_EX_WINDOWEDGE: u32 = 0x0000_0100;
    const WS_EX_LAYERED: u32 = 0x0008_0000;

    #[test]
    fn an_ordinary_win32_window_can_be_faded() {
        // File Explorer, already faded: layered and window edge, nothing else.
        assert!(can_fade(WS_EX_WINDOWEDGE, "CabinetWClass"));
        assert!(can_fade(WS_EX_LAYERED | WS_EX_WINDOWEDGE, "CabinetWClass"));
        assert!(can_fade(0, "Notepad"));
    }

    #[test]
    fn a_window_without_a_redirection_surface_is_never_faded() {
        // Spotify and Discord main windows report exactly this word.
        let electron = WS_EX_NOREDIRECTIONBITMAP | WS_EX_WINDOWEDGE;
        assert_eq!(electron, 0x0020_0100);
        assert!(!can_fade(electron, "Chrome_WidgetWin_1"));

        // The bit decides on its own, whatever the class.
        assert!(!can_fade(electron, "CASCADIA_HOSTING_WINDOW_CLASS"));
        assert!(!can_fade(electron, "ApplicationFrameWindow"));
        assert!(!can_fade(
            WS_EX_NOREDIRECTIONBITMAP,
            "SomeDirectCompositionApp"
        ));
    }

    #[test]
    fn the_chromium_class_family_stays_opaque_without_the_bit() {
        // Chromium presenting through the redirection surface, as it does
        // with DirectComposition unavailable.
        assert!(!can_fade(WS_EX_WINDOWEDGE, "Chrome_WidgetWin_1"));
        assert!(!can_fade(WS_EX_WINDOWEDGE, "Chrome_WidgetWin_0"));
        assert!(!can_fade(0, "Chrome_WidgetWin_2"));
    }

    #[test]
    fn only_the_prefix_matches() {
        // A class that merely mentions Chrome is not Chromium's.
        assert!(can_fade(WS_EX_WINDOWEDGE, "MyChrome_WidgetWin_1"));
        assert!(can_fade(WS_EX_WINDOWEDGE, "chrome_widgetwin_1"));
        assert!(can_fade(WS_EX_WINDOWEDGE, ""));
    }
}
