//! Plain data about monitors and windows, plus the manageability heuristics.
//!
//! Nothing in this module calls Win32, so all of it is unit testable on any
//! machine. The values themselves are filled in by `win32`.

use mochi_core::Rect;
use serde::{Deserialize, Serialize};

/// A window handle. Stored as `isize` so it survives JSON as a plain number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hwnd(pub isize);

impl std::fmt::Display for Hwnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:x}", self.0)
    }
}

impl Hwnd {
    /// The null handle.
    pub const NULL: Self = Self(0);

    /// True for the null handle.
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }

    /// The handle as a signed integer, for the wire types.
    pub const fn as_i64(self) -> i64 {
        self.0 as i64
    }
}

/// A monitor handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MonitorId(pub isize);

impl std::fmt::Display for MonitorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:x}", self.0)
    }
}

/// Window styles Mochi looks at. Values from `winuser.h`.
pub mod style {
    /// The window is a child of another window.
    pub const WS_CHILD: u32 = 0x4000_0000;
    /// The window is visible.
    pub const WS_VISIBLE: u32 = 0x1000_0000;
    /// The window is minimized.
    pub const WS_MINIMIZE: u32 = 0x2000_0000;
    /// The window is maximized.
    pub const WS_MAXIMIZE: u32 = 0x0100_0000;
    /// The window has a title bar and a border.
    pub const WS_CAPTION: u32 = 0x00C0_0000;
    /// The window has a sizing border.
    pub const WS_THICKFRAME: u32 = 0x0004_0000;
    /// The window has a window menu.
    pub const WS_SYSMENU: u32 = 0x0008_0000;
    /// The window is a pop-up.
    pub const WS_POPUP: u32 = 0x8000_0000;
    /// The window is disabled.
    pub const WS_DISABLED: u32 = 0x0800_0000;
}

/// Extended window styles Mochi looks at. Values from `winuser.h`.
pub mod ex_style {
    /// A floating tool window: small title bar, never in the taskbar.
    pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    /// Forces a taskbar button even for windows that would not get one.
    pub const WS_EX_APPWINDOW: u32 = 0x0004_0000;
    /// The window does not take focus when clicked.
    pub const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    /// The window is layered, required for per-window transparency.
    pub const WS_EX_LAYERED: u32 = 0x0008_0000;
    /// The window sits above all non-topmost windows.
    pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
    /// Mouse input falls through the window.
    pub const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
}

/// Windows narrower or shorter than this are treated as splash screens, popups
/// or invisible helpers rather than as application windows.
pub const MIN_MANAGEABLE_DIMENSION: i32 = 40;

/// The class every UWP window has. The application itself lives in a child
/// window; this is the frame Windows wraps around it, and the one a window
/// manager sees.
pub const FRAME_WINDOW_CLASS: &str = "ApplicationFrameWindow";

/// Window classes that belong to the shell and must never be touched.
pub const SHELL_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "Windows.UI.Core.CoreWindow",
    "ForegroundStaging",
    "MultitaskingViewFrame",
    "XamlExplorerHostIslandWindow",
    "Windows.Internal.Shell.TabProxyWindow",
    "TaskListThumbnailWnd",
    "Xaml_WindowedPopupClass",
    "tooltips_class32",
    "DV2ControlHost",
    "SysShadow",
    "Button",
    "#32768",
    "EdgeUiInputTopWndClass",
    // The Windows 10 notification area overflow flyout.
    "NotifyIconOverflowWindow",
    // The compositor's own notification sink. It is visible, titled and big
    // enough to look like an application window, so it needs naming.
    "Dwm",
    "IME",
    "MSCTFIME UI",
];

/// Shell window class *families*, matched on the prefix.
///
/// Some shell classes carry a suffix that changes between Windows releases:
/// this machine has both `XamlExplorerHostIslandWindow` (Task View) and
/// `XamlExplorerHostIslandWindow_WASDK` alive at the same time. Listing the
/// exact spellings means every new suffix is a new false positive waiting to
/// happen, so these are matched by prefix instead.
pub const SHELL_CLASS_PREFIXES: &[&str] = &[
    // Task View, Alt-Tab and the snap assist overlays.
    "XamlExplorerHostIslandWindow",
    // The Windows 11 notification area overflow.
    "TopLevelWindowForOverflowXamlIsland",
    // XAML content hosted straight on the desktop by the shell.
    "Windows.UI.Composition.DesktopWindowContentBridge",
];

/// Processes whose windows are part of the shell chrome.
pub const SHELL_EXES: &[&str] = &[
    "dwm.exe",
    "SearchHost.exe",
    "SearchApp.exe",
    "StartMenuExperienceHost.exe",
    "ShellExperienceHost.exe",
    "TextInputHost.exe",
    "PeopleExperienceHost.exe",
    "LockApp.exe",
    "ScreenClippingHost.exe",
    "Widgets.exe",
    "WidgetBoard.exe",
];

/// Everything Mochi knows about one monitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    /// Raw `HMONITOR`. Unstable across display changes, re-enumerate after one.
    pub id: MonitorId,
    /// GDI device name, for example `\\.\DISPLAY1`.
    pub device_name: String,
    /// Friendly name from `EnumDisplayDevicesW`, for example `Generic PnP Monitor`.
    pub device_description: String,
    /// Full monitor rectangle in virtual screen coordinates.
    pub size: Rect,
    /// Monitor rectangle minus the taskbar and other appbars.
    pub work_area: Rect,
    /// Effective DPI. 96 is 100% scaling.
    pub dpi: u32,
    /// True for the monitor that holds the origin of the virtual screen.
    pub primary: bool,
}

impl MonitorInfo {
    /// DPI scaling as a factor, 1.0 at 96 DPI.
    pub fn scale_factor(&self) -> f64 {
        f64::from(self.dpi) / 96.0
    }
}

/// Everything Mochi knows about one top-level window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// The window handle.
    pub hwnd: Hwnd,
    /// Window title, empty when the window has none.
    pub title: String,
    /// Window class name.
    pub class: String,
    /// File name of the owning process, for example `firefox.exe`.
    pub exe: String,
    /// Full path of the owning process. Empty when it could not be read,
    /// which normally means the process is elevated and Mochi is not.
    pub path: String,
    /// Owning process id.
    pub pid: u32,
    /// `GWL_STYLE`.
    pub style: u32,
    /// `GWL_EXSTYLE`.
    pub ex_style: u32,
    /// `GetWindowRect`, which includes the invisible resize border.
    pub rect: Rect,
    /// `DWMWA_EXTENDED_FRAME_BOUNDS`, what the user perceives as the window.
    pub frame: Rect,
    /// True when DWM is hiding the window, which is how virtual desktops and
    /// most window managers, including Mochi, hide windows.
    pub cloaked: bool,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// `IsIconic`.
    pub minimized: bool,
    /// `IsZoomed`.
    pub maximized: bool,
    /// Owner window, if the window is owned.
    pub owner: Option<Hwnd>,
    /// Monitor the window currently sits on.
    pub monitor: Option<MonitorId>,
}

impl WindowInfo {
    /// A blank window, for tests and for events about windows that already died.
    pub fn placeholder(hwnd: Hwnd) -> Self {
        Self {
            hwnd,
            title: String::new(),
            class: String::new(),
            exe: String::new(),
            path: String::new(),
            pid: 0,
            style: 0,
            ex_style: 0,
            rect: Rect::default(),
            frame: Rect::default(),
            cloaked: false,
            visible: false,
            minimized: false,
            maximized: false,
            owner: None,
            monitor: None,
        }
    }

    /// True when the extended style bit is set.
    pub const fn has_ex_style(&self, bit: u32) -> bool {
        self.ex_style & bit != 0
    }

    /// True when the style bit is set.
    pub const fn has_style(&self, bit: u32) -> bool {
        self.style & bit != 0
    }

    /// The perceived size, falling back to the window rect when DWM gave nothing.
    pub fn visible_frame(&self) -> Rect {
        if self.frame.width() > 0 && self.frame.height() > 0 {
            self.frame
        } else {
            self.rect
        }
    }

    /// Short form for logs: `0x1234 "Title" (firefox.exe)`.
    pub fn describe(&self) -> String {
        format!("{} {:?} ({})", self.hwnd, self.title, self.exe)
    }

    /// See [`is_manageable`].
    pub fn is_manageable(&self) -> bool {
        is_manageable(self).is_ok()
    }
}

/// Why a window is not a tiling candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmanageable {
    /// `IsWindowVisible` said no.
    NotVisible,
    /// DWM is hiding the window: another virtual desktop, a suspended UWP app,
    /// or a window some other manager has cloaked away.
    Cloaked,
    /// `WS_CHILD`: not a top-level window.
    Child,
    /// `WS_EX_TOOLWINDOW` without `WS_EX_APPWINDOW`.
    ToolWindow,
    /// `WS_EX_NOACTIVATE`: the window refuses to be activated, so it can never
    /// take part in a focus ring. Overlays and on-screen keyboards use it.
    NoActivate,
    /// Owned by another window and not forced into the taskbar: a dialog,
    /// a palette or a popup.
    Owned,
    /// No title and no `WS_EX_APPWINDOW`, so nothing a user would call a window.
    NoTitle,
    /// Smaller than [`MIN_MANAGEABLE_DIMENSION`] in at least one direction.
    TooSmall,
    /// The class belongs to the shell.
    ShellClass,
    /// The process belongs to the shell.
    ShellProcess,
}

impl Unmanageable {
    /// A short reason suitable for a log line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotVisible => "not visible",
            Self::Cloaked => "cloaked",
            Self::Child => "child window",
            Self::ToolWindow => "tool window",
            Self::NoActivate => "no-activate window",
            Self::Owned => "owned window",
            Self::NoTitle => "no title",
            Self::TooSmall => "too small",
            Self::ShellClass => "shell class",
            Self::ShellProcess => "shell process",
        }
    }
}

impl std::fmt::Display for Unmanageable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Unmanageable {
    /// True for the verdicts a `manage_rules` entry is allowed to overrule.
    ///
    /// A rule may say "this untitled owned tool window is a real window"; it may
    /// not say "this child window of the taskbar is a real window", because the
    /// remaining verdicts describe windows that cannot be tiled at all.
    pub const fn is_overridable(self) -> bool {
        matches!(
            self,
            Self::ToolWindow | Self::NoActivate | Self::Owned | Self::NoTitle
        )
    }
}

/// Decides whether a window is a tiling candidate, and says why not when it is not.
///
/// This is the static half of the decision. User rules from the configuration
/// are applied on top of it by [`crate::wm::WindowManager`].
pub fn is_manageable(w: &WindowInfo) -> Result<(), Unmanageable> {
    is_manageable_with(w, false)
}

/// [`is_manageable`] with the tool window rejection optionally lifted.
///
/// `allow_tool_window` is what `--manage-class` turns on, and it is the only
/// rule that switch is allowed to relax: a class named on the command line is
/// still rejected when it is a child window, a shell window, invisible,
/// cloaked or too small.
pub fn is_manageable_with(w: &WindowInfo, allow_tool_window: bool) -> Result<(), Unmanageable> {
    if w.has_style(style::WS_CHILD) {
        return Err(Unmanageable::Child);
    }
    if SHELL_CLASSES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(&w.class))
    {
        return Err(Unmanageable::ShellClass);
    }
    if SHELL_EXES.iter().any(|e| e.eq_ignore_ascii_case(&w.exe)) {
        return Err(Unmanageable::ShellProcess);
    }

    let forced = w.has_ex_style(ex_style::WS_EX_APPWINDOW);
    if w.has_ex_style(ex_style::WS_EX_TOOLWINDOW) && !forced && !allow_tool_window {
        return Err(Unmanageable::ToolWindow);
    }
    if w.has_ex_style(ex_style::WS_EX_NOACTIVATE) && !forced {
        return Err(Unmanageable::NoActivate);
    }
    if w.owner.is_some_and(|o| !o.is_null()) && !forced {
        return Err(Unmanageable::Owned);
    }
    if w.title.trim().is_empty() && !forced {
        return Err(Unmanageable::NoTitle);
    }

    // A minimized window keeps its slot in the layout, so the visibility and
    // size checks are skipped for it: Windows reports silly rects while iconic.
    if !w.minimized {
        if !w.visible {
            return Err(Unmanageable::NotVisible);
        }
        if w.cloaked {
            return Err(Unmanageable::Cloaked);
        }
        let frame = w.visible_frame();
        if frame.width() < MIN_MANAGEABLE_DIMENSION || frame.height() < MIN_MANAGEABLE_DIMENSION {
            return Err(Unmanageable::TooSmall);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_window() -> WindowInfo {
        WindowInfo {
            title: "Some Editor".into(),
            class: "Chrome_WidgetWin_1".into(),
            exe: "Code.exe".into(),
            path: r"C:\Program Files\Code.exe".into(),
            pid: 4242,
            style: style::WS_VISIBLE | style::WS_CAPTION | style::WS_THICKFRAME,
            rect: Rect::new(0, 0, 1280, 800),
            frame: Rect::new(8, 0, 1272, 792),
            visible: true,
            ..WindowInfo::placeholder(Hwnd(0x1234))
        }
    }

    #[test]
    fn a_normal_application_window_is_manageable() {
        assert_eq!(is_manageable(&app_window()), Ok(()));
        assert!(app_window().is_manageable());
    }

    #[test]
    fn child_windows_are_rejected_first() {
        let mut w = app_window();
        w.style |= style::WS_CHILD;
        assert_eq!(is_manageable(&w), Err(Unmanageable::Child));
    }

    #[test]
    fn the_desktop_and_the_taskbar_are_rejected() {
        for class in [
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
        ] {
            let mut w = app_window();
            w.class = class.into();
            assert_eq!(
                is_manageable(&w),
                Err(Unmanageable::ShellClass),
                "{class} slipped through"
            );
        }
    }

    #[test]
    fn the_compositors_notification_window_is_rejected() {
        // Seen on this machine: visible, titled, 237x39, and not a tool window.
        let mut w = app_window();
        w.title = "DWM Notification Window".into();
        w.class = "Dwm".into();
        w.exe = "dwm.exe".into();
        assert_eq!(is_manageable(&w), Err(Unmanageable::ShellClass));

        let mut w = app_window();
        w.exe = "dwm.exe".into();
        assert_eq!(is_manageable(&w), Err(Unmanageable::ShellProcess));
    }

    #[test]
    fn shell_processes_are_rejected_even_with_a_title() {
        let mut w = app_window();
        w.exe = "SearchHost.exe".into();
        w.class = "Windows.UI.Core.CoreWindow".into();
        assert_eq!(is_manageable(&w), Err(Unmanageable::ShellClass));

        let mut w = app_window();
        w.exe = "StartMenuExperienceHost.exe".into();
        assert_eq!(is_manageable(&w), Err(Unmanageable::ShellProcess));
    }

    #[test]
    fn tool_windows_are_rejected_unless_they_force_a_taskbar_button() {
        let mut w = app_window();
        w.ex_style = ex_style::WS_EX_TOOLWINDOW;
        assert_eq!(is_manageable(&w), Err(Unmanageable::ToolWindow));

        w.ex_style |= ex_style::WS_EX_APPWINDOW;
        assert_eq!(is_manageable(&w), Ok(()));
    }

    #[test]
    fn manage_class_lifts_the_tool_window_rejection_and_nothing_else() {
        // What `--manage-class MochiTestWindow` has to accept.
        let mut w = app_window();
        w.class = "MochiTestWindow".into();
        w.ex_style = ex_style::WS_EX_TOOLWINDOW;
        assert_eq!(is_manageable(&w), Err(Unmanageable::ToolWindow));
        assert_eq!(is_manageable_with(&w, true), Ok(()));

        // Every other rejection survives the switch.
        type Break = &'static dyn Fn(&mut WindowInfo);
        let breakages: [(Break, Unmanageable); 5] = [
            (&|w| w.style |= style::WS_CHILD, Unmanageable::Child),
            (&|w| w.visible = false, Unmanageable::NotVisible),
            (&|w| w.cloaked = true, Unmanageable::Cloaked),
            (
                &|w| w.frame = Rect::new(0, 0, 10, 10),
                Unmanageable::TooSmall,
            ),
            (&|w| w.title = String::new(), Unmanageable::NoTitle),
        ];
        for (broken, expected) in breakages {
            let mut w = app_window();
            w.class = "MochiTestWindow".into();
            w.ex_style = ex_style::WS_EX_TOOLWINDOW;
            broken(&mut w);
            assert_eq!(is_manageable_with(&w, true), Err(expected));
        }
    }

    #[test]
    fn only_the_soft_verdicts_may_be_overridden_by_a_manage_rule() {
        assert!(Unmanageable::ToolWindow.is_overridable());
        assert!(Unmanageable::NoTitle.is_overridable());
        assert!(Unmanageable::Owned.is_overridable());
        assert!(Unmanageable::NoActivate.is_overridable());
        assert!(!Unmanageable::Child.is_overridable());
        assert!(!Unmanageable::ShellClass.is_overridable());
        assert!(!Unmanageable::Cloaked.is_overridable());
        assert!(!Unmanageable::TooSmall.is_overridable());
    }

    #[test]
    fn owned_windows_are_rejected_unless_they_force_a_taskbar_button() {
        let mut w = app_window();
        w.owner = Some(Hwnd(0x9999));
        assert_eq!(is_manageable(&w), Err(Unmanageable::Owned));

        // A null owner is the same as no owner.
        w.owner = Some(Hwnd::NULL);
        assert_eq!(is_manageable(&w), Ok(()));

        w.owner = Some(Hwnd(0x9999));
        w.ex_style = ex_style::WS_EX_APPWINDOW;
        assert_eq!(is_manageable(&w), Ok(()));
    }

    #[test]
    fn untitled_windows_are_rejected() {
        let mut w = app_window();
        w.title = "   ".into();
        assert_eq!(is_manageable(&w), Err(Unmanageable::NoTitle));
    }

    #[test]
    fn invisible_and_cloaked_windows_are_rejected() {
        let mut w = app_window();
        w.visible = false;
        assert_eq!(is_manageable(&w), Err(Unmanageable::NotVisible));

        let mut w = app_window();
        w.cloaked = true;
        assert_eq!(is_manageable(&w), Err(Unmanageable::Cloaked));
    }

    #[test]
    fn tiny_windows_are_rejected_by_their_frame_not_their_rect() {
        let mut w = app_window();
        w.rect = Rect::new(0, 0, 1000, 1000);
        w.frame = Rect::new(0, 0, 10, 1000);
        assert_eq!(is_manageable(&w), Err(Unmanageable::TooSmall));

        // With no DWM frame the window rect is used instead.
        let mut w = app_window();
        w.frame = Rect::default();
        w.rect = Rect::new(0, 0, 20, 20);
        assert_eq!(is_manageable(&w), Err(Unmanageable::TooSmall));
    }

    #[test]
    fn minimized_windows_keep_their_slot() {
        let mut w = app_window();
        w.minimized = true;
        w.visible = false;
        w.rect = Rect::new(-32000, -32000, -31840, -31972);
        w.frame = Rect::default();
        assert_eq!(is_manageable(&w), Ok(()));
    }

    #[test]
    fn scale_factor_follows_dpi() {
        let mut m = MonitorInfo {
            id: MonitorId(1),
            device_name: r"\\.\DISPLAY1".into(),
            device_description: "Generic PnP Monitor".into(),
            size: Rect::new(0, 0, 3840, 2160),
            work_area: Rect::new(0, 0, 3840, 2112),
            dpi: 144,
            primary: true,
        };
        assert!((m.scale_factor() - 1.5).abs() < f64::EPSILON);
        m.dpi = 96;
        assert!((m.scale_factor() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn handles_display_as_hex_and_serialise_as_numbers() {
        assert_eq!(Hwnd(0x1f4).to_string(), "0x1f4");
        assert_eq!(serde_json::to_string(&Hwnd(500)).unwrap(), "500");
        assert_eq!(serde_json::to_string(&MonitorId(7)).unwrap(), "7");
        assert!(Hwnd::NULL.is_null());
    }
}
