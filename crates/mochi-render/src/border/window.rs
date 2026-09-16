//! One border window.
//!
//! Layered, click-through, never focusable, and stacked directly above the
//! window it belongs to rather than pinned on top of everything.

use std::sync::OnceLock;

use mochi_core::Rect;
use mochi_core::config::Colour;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ROUNDED_RECT, ID2D1Factory,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HTTRANSPARENT, RegisterClassExW, SW_HIDE,
    SW_SHOWNA, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow, WM_ERASEBKGND,
    WM_NCHITTEST, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::border::{BorderColoursExt, BorderConfig, BorderKind, BorderStyleExt};
use crate::color::{ColourExt, TRANSPARENT};
use crate::geometry::{FrameGeometry, RectF, frame_geometry};
use crate::win::{
    LayeredSurface, dpi_for_rect, is_window, is_window_visible, module_handle, stack_above, wide,
};
use crate::{RenderError, Result};

/// The window class name. Deliberately recognisable in Spy++ and in a
/// `mochic state` dump, and used by the ignore rules so the daemon never tries
/// to manage its own furniture.
const CLASS_NAME: &str = "MochiBorder";

/// What is currently painted on the surface, so that a move does not repaint.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Painted {
    size: (i32, i32),
    stroke: RectF,
    stroke_width: f32,
    radius: f32,
    colour: Colour,
}

/// A single border frame.
///
/// Created, used and destroyed on the border thread. It is not `Send`: an
/// `HWND` may only be touched from the thread that made it.
pub struct BorderWindow {
    hwnd: HWND,
    factory: ID2D1Factory,
    surface: Option<LayeredSurface>,
    config: BorderConfig,
    painted: Option<Painted>,
    position: Option<(i32, i32)>,
    visible: bool,
    topmost: bool,
}

impl BorderWindow {
    /// Creates a hidden border window.
    ///
    /// # Errors
    ///
    /// When the window class cannot be registered or the window cannot be made.
    pub fn new(factory: &ID2D1Factory, config: BorderConfig) -> Result<Self> {
        let class = ensure_class()?;
        let title = wide("mochi border");

        // SAFETY: the class was registered above and both wide strings outlive
        // the call. The window is created hidden, with no parent and no menu,
        // and is destroyed in Drop.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(module_handle().into()),
                None,
            )
        }?;

        Ok(Self {
            hwnd,
            factory: factory.clone(),
            surface: None,
            config,
            painted: None,
            position: None,
            visible: false,
            topmost: false,
        })
    }

    /// The border window's own handle, for logging and for the daemon's ignore
    /// list.
    #[must_use]
    pub fn handle(&self) -> crate::WindowHandle {
        crate::WindowHandle::from(self.hwnd)
    }

    /// Swaps in a new configuration. The next [`BorderWindow::track`] repaints.
    pub fn set_config(&mut self, config: BorderConfig) {
        if self.config != config {
            self.config = config;
            // Force a repaint even if the geometry works out the same.
            self.painted = None;
        }
    }

    /// Points the frame at `target`, which sits at `rect`, and colours it for
    /// `kind`.
    ///
    /// Repaints only when the size, the shape or the colour changed; a window
    /// that merely moved is moved, not redrawn, which is what keeps a 60 fps
    /// animation cheap on a 4K monitor.
    ///
    /// # Errors
    ///
    /// When Direct2D or the window manager refuses. The caller logs and carries
    /// on: a missing border must never take the daemon down.
    pub fn track(&mut self, target: HWND, rect: Rect, kind: BorderKind) -> Result<()> {
        if !self.config.enabled || rect.is_empty() {
            self.hide();
            return Ok(());
        }

        // A border for a window that has gone away, or that the shell has
        // hidden, has nothing to sit on.
        let has_target = !target.0.is_null();
        if has_target && (!is_window(target) || !is_window_visible(target)) {
            self.hide();
            return Ok(());
        }

        let dpi = dpi_for_rect(rect);
        let geometry = frame_geometry(
            rect,
            self.config.width,
            self.config.offset,
            self.config.style.is_rounded(),
            dpi,
        );
        if geometry.is_empty() {
            self.hide();
            return Ok(());
        }

        let colour = self.config.colours.resolve(kind);
        let size = (geometry.window.width(), geometry.window.height());
        let wanted = Painted {
            size,
            stroke: geometry.stroke,
            stroke_width: geometry.stroke_width,
            radius: geometry.radius,
            colour,
        };

        let position = (geometry.window.left, geometry.window.top);
        if self.painted == Some(wanted) {
            // Same pixels: a move is enough, and a move of a layered window is
            // handled entirely by the compositor.
            if self.position != Some(position) || !self.visible {
                self.move_to(geometry.window)?;
            }
        } else {
            self.paint(&geometry, colour)?;
            self.present(geometry.window)?;
            self.painted = Some(wanted);
        }

        self.restack(target)?;
        self.visible = true;
        Ok(())
    }

    /// Paints the frame into the off-screen surface.
    fn paint(&mut self, geometry: &FrameGeometry, colour: Colour) -> Result<()> {
        let width = geometry.window.width();
        let height = geometry.window.height();

        if self
            .surface
            .as_ref()
            .is_none_or(|surface| !surface.fits(width, height))
        {
            self.surface = Some(LayeredSurface::new(&self.factory, width, height)?);
        }
        let Some(surface) = self.surface.as_ref() else {
            return Ok(());
        };
        let target = surface.target();

        // SAFETY: the render target belongs to this surface and is only used on
        // this thread. Every pointer below is to a live local. BeginDraw is
        // always paired with EndDraw, including on the error path, because
        // EndDraw is what reports the failure.
        let result = unsafe {
            target.BeginDraw();
            target.Clear(Some(&TRANSPARENT));
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);

            let brush = target.CreateSolidColorBrush(&colour.to_d2d(), None);
            match brush {
                Ok(brush) => {
                    if geometry.radius > 0.0 {
                        let rounded = D2D1_ROUNDED_RECT {
                            rect: geometry.stroke.into(),
                            radiusX: geometry.radius,
                            radiusY: geometry.radius,
                        };
                        target.DrawRoundedRectangle(&rounded, &brush, geometry.stroke_width, None);
                    } else {
                        let square: D2D_RECT_F = geometry.stroke.into();
                        target.DrawRectangle(&square, &brush, geometry.stroke_width, None);
                    }
                }
                Err(error) => tracing::warn!(%error, "border brush"),
            }

            target.EndDraw(None, None)
        };

        if let Err(error) = result {
            // D2DERR_RECREATE_TARGET and friends: throw the surface away so the
            // next frame builds a fresh one.
            self.surface = None;
            self.painted = None;
            return Err(RenderError::Win32(error));
        }
        Ok(())
    }

    /// Blits the painted surface onto the window at its new position and size.
    fn present(&mut self, window: Rect) -> Result<()> {
        let Some(surface) = self.surface.as_ref() else {
            return Ok(());
        };
        surface.present(self.hwnd, window.left, window.top)?;
        self.position = Some((window.left, window.top));
        Ok(())
    }

    /// Moves the window without touching its pixels.
    fn move_to(&mut self, window: Rect) -> Result<()> {
        // SAFETY: the window belongs to this struct and this thread. No size
        // change, no activation, no z-order change: restack does that.
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                window.left,
                window.top,
                window.width(),
                window.height(),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        }?;
        self.position = Some((window.left, window.top));
        Ok(())
    }

    /// Puts the frame directly above its target in the z-order.
    ///
    /// See [`crate::win::stack_above`]: passing the target as `hWndInsertAfter`
    /// is what makes the border ride with its window instead of floating over
    /// unrelated ones.
    fn restack(&mut self, target: HWND) -> Result<()> {
        self.topmost = stack_above(self.hwnd, target, self.topmost, None)?;
        Ok(())
    }

    /// Takes the frame off the screen without destroying it, so it can be
    /// reused for the next container.
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        // SAFETY: our own window. SW_HIDE never activates anything.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.visible = false;
    }

    /// Puts a hidden frame back on the screen, without stealing the focus.
    pub fn show(&mut self) {
        if self.visible {
            return;
        }
        // SAFETY: our own window. SW_SHOWNA shows without activating, which
        // matters because the frame must never take the focus off a real
        // window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNA);
        }
        self.visible = true;
    }

    /// Destroys the window now rather than at drop time.
    ///
    /// # Errors
    ///
    /// When `DestroyWindow` fails, which in practice means the window was
    /// already gone.
    pub fn destroy(mut self) -> Result<()> {
        let result = self.destroy_inner();
        // Drop must not try again.
        self.hwnd = HWND(std::ptr::null_mut());
        result
    }

    fn destroy_inner(&mut self) -> Result<()> {
        if self.hwnd.0.is_null() {
            return Ok(());
        }
        // The surface has to go before the window it paints into.
        self.surface = None;
        // SAFETY: our own window, destroyed from the thread that created it,
        // which is what DestroyWindow requires.
        unsafe { DestroyWindow(self.hwnd) }?;
        Ok(())
    }
}

impl Drop for BorderWindow {
    fn drop(&mut self) {
        if let Err(error) = self.destroy_inner() {
            tracing::debug!(%error, "border window was already gone");
        }
    }
}

/// Registers the border window class once per process.
fn ensure_class() -> Result<&'static Vec<u16>> {
    static CLASS: OnceLock<std::result::Result<Vec<u16>, String>> = OnceLock::new();

    let class = CLASS.get_or_init(|| {
        let name = wide(CLASS_NAME);
        let descriptor = WNDCLASSEXW {
            cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap_or(0),
            lpfnWndProc: Some(border_wndproc),
            hInstance: module_handle().into(),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };

        // SAFETY: the descriptor is fully initialised and `name` outlives the
        // call; Windows copies the class name into its own storage.
        let atom = unsafe { RegisterClassExW(&descriptor) };
        if atom == 0 {
            return Err(windows::core::Error::from_thread().to_string());
        }
        Ok(name)
    });

    class
        .as_ref()
        .map_err(|error| RenderError::ThreadStart("border class", error.clone()))
}

/// The border window procedure.
///
/// Borders have no content of their own: `UpdateLayeredWindow` owns their
/// pixels, so there is nothing to do on `WM_PAINT`.
unsafe extern "system" fn border_wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // Belt and braces next to WS_EX_TRANSPARENT: every hit test falls
        // through to whatever is underneath, so a border can never eat a click
        // meant for a window.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => LRESULT(1),
        // SAFETY: the default handler is the right thing for every other
        // message and the arguments are passed through untouched.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
