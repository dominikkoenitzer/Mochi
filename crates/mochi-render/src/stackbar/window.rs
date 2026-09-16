//! One stackbar window.
//!
//! Unlike a border this window takes the mouse, so it is a plain opaque popup
//! rather than a click-through layered one. Its state lives in a box behind
//! `GWLP_USERDATA` because the window procedure has to reach it to paint and to
//! answer a click; everything inside that box is behind a `RefCell`, so a
//! message arriving in the middle of an update is skipped rather than being
//! allowed to alias.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::OnceLock;

use mochi_core::Rect;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, ID2D1Factory, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetWindowLongPtrW, MA_NOACTIVATE, RegisterClassExW, SW_HIDE, SetWindowLongPtrW, ShowWindow,
    WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCCREATE, WM_PAINT, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::color::ColourExt;
use crate::stackbar::layout::TabLayout;
use crate::stackbar::{StackbarSpec, StackbarStyle, StackbarTab};
use crate::win::{module_handle, stack_above, wide};
use crate::{RenderError, Result, WindowHandle};

/// The window class name, recognisable in Spy++ and in the ignore rules.
const CLASS_NAME: &str = "MochiStackbar";

/// Padding between a tab's edge and its label.
const LABEL_PADDING: f32 = 10.0;

/// Points to device independent pixels.
const POINTS_TO_DIP: f32 = 96.0 / 72.0;

/// What the daemon can be told when a tab is clicked.
pub(crate) type ClickCallback = Arc<dyn Fn(WindowHandle) + Send + Sync>;

/// Everything the window procedure needs. Behind a `RefCell` so that a message
/// arriving during an update cannot alias it.
struct Inner {
    style: StackbarStyle,
    tabs: Vec<StackbarTab>,
    layout: TabLayout,
    renderer: Option<ID2D1HwndRenderTarget>,
    format: Option<IDWriteTextFormat>,
    size: (u32, u32),
}

/// The box behind `GWLP_USERDATA`.
struct StackbarState {
    inner: RefCell<Inner>,
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    on_click: ClickCallback,
}

/// A stackbar window.
///
/// Created, used and destroyed on the stackbar thread.
pub(crate) struct StackbarWindow {
    hwnd: HWND,
    /// Boxed so that the address handed to `GWLP_USERDATA` stays put when this
    /// struct is moved. Dropped only after the window is destroyed, so the
    /// window procedure can never see a dangling pointer.
    state: Box<StackbarState>,
    visible: bool,
    topmost: bool,
}

impl StackbarWindow {
    /// Creates a hidden stackbar window.
    ///
    /// # Errors
    ///
    /// When the class or the window cannot be created.
    pub(crate) fn new(
        factory: &ID2D1Factory,
        dwrite: &IDWriteFactory,
        style: StackbarStyle,
        on_click: ClickCallback,
    ) -> Result<Self> {
        let class = ensure_class()?;
        let title = wide("mochi stackbar");

        let state = Box::new(StackbarState {
            inner: RefCell::new(Inner {
                style,
                tabs: Vec::new(),
                layout: TabLayout::new(Rect::default(), 0, 0, 0),
                renderer: None,
                format: None,
                size: (0, 0),
            }),
            factory: factory.clone(),
            dwrite: dwrite.clone(),
            on_click,
        });
        let pointer: *const StackbarState = &raw const *state;

        // SAFETY: the class is registered, both wide strings outlive the call,
        // and `pointer` addresses the box above, which this struct keeps alive
        // until after DestroyWindow. WM_NCCREATE parks it in GWLP_USERDATA.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
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
                Some(pointer.cast()),
            )
        }?;

        Ok(Self {
            hwnd,
            state,
            visible: false,
            topmost: false,
        })
    }

    /// Points the bar at a container and repaints it.
    ///
    /// # Errors
    ///
    /// When the window cannot be moved.
    pub(crate) fn update(&mut self, spec: &StackbarSpec, style: &StackbarStyle) -> Result<()> {
        let layout = TabLayout::new(spec.rect, spec.tabs.len(), style.height, style.tab_width);
        if layout.is_empty() {
            self.hide();
            return Ok(());
        }

        {
            let Ok(mut inner) = self.state.inner.try_borrow_mut() else {
                // A message is being handled for this window right now; the next
                // update will catch up.
                return Ok(());
            };
            if inner.style.font_family != style.font_family
                || inner.style.font_size != style.font_size
            {
                inner.format = None;
            }
            inner.style = style.clone();
            inner.tabs = spec.tabs.clone();
            inner.layout = layout;
        }

        self.topmost = stack_above(self.hwnd, spec.above.hwnd(), self.topmost, Some(layout.bar))?;
        self.visible = true;

        // SAFETY: our own window. A null rectangle invalidates all of it; the
        // paint happens when the loop next runs dry, so a burst of updates
        // costs one paint.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        Ok(())
    }

    /// Takes the bar off the screen without destroying it.
    pub(crate) fn hide(&mut self) {
        if !self.visible {
            return;
        }
        // SAFETY: our own window; SW_HIDE never activates anything.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.visible = false;
    }
}

impl Drop for StackbarWindow {
    fn drop(&mut self) {
        if self.hwnd.0.is_null() {
            return;
        }
        // SAFETY: our own window, destroyed on the thread that created it. The
        // state box is dropped straight after, so the window procedure cannot
        // be entered again with a dangling pointer.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        self.hwnd = HWND(std::ptr::null_mut());
    }
}

/// Registers the stackbar window class once per process.
fn ensure_class() -> Result<&'static Vec<u16>> {
    static CLASS: OnceLock<std::result::Result<Vec<u16>, String>> = OnceLock::new();

    let class = CLASS.get_or_init(|| {
        let name = wide(CLASS_NAME);
        let descriptor = WNDCLASSEXW {
            cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap_or(0),
            lpfnWndProc: Some(stackbar_wndproc),
            hInstance: module_handle().into(),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        // SAFETY: the descriptor is fully initialised and the name outlives the
        // call; Windows copies it.
        let atom = unsafe { RegisterClassExW(&descriptor) };
        if atom == 0 {
            return Err(windows::core::Error::from_thread().to_string());
        }
        Ok(name)
    });

    class
        .as_ref()
        .map_err(|error| RenderError::ThreadStart("stackbar class", error.clone()))
}

/// The stackbar window procedure.
unsafe extern "system" fn stackbar_wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: on WM_NCCREATE lparam is a CREATESTRUCTW, and its
        // lpCreateParams is the pointer passed to CreateWindowExW above.
        unsafe {
            let create = lparam.0 as *const CREATESTRUCTW;
            if let Some(create) = create.as_ref() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
    }

    // SAFETY: GWLP_USERDATA holds either null or the pointer stored above,
    // which addresses a box that outlives the window. The reference is shared,
    // never unique, so a nested message cannot alias it; the mutable state
    // behind it is a RefCell.
    let state = unsafe {
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const StackbarState;
        pointer.as_ref()
    };
    let Some(state) = state else {
        // SAFETY: plain pass-through before the state arrives.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };

    match message {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: BeginPaint and EndPaint are paired around the drawing and
            // `paint` is a live local.
            unsafe {
                BeginPaint(hwnd, &mut paint);
            }
            if let Err(error) = draw(state, hwnd) {
                tracing::warn!(%error, "stackbar paint");
            }
            // SAFETY: as above, and EndPaint is what validates the window.
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // A click on a tab must focus the window it names, not the bar.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP => {
            let x = i32::from(((lparam.0 as u32) & 0xffff) as i16);
            let clicked = state.inner.try_borrow().ok().and_then(|inner| {
                inner
                    .layout
                    .hit(x)
                    .and_then(|index| inner.tabs.get(index))
                    .map(|tab| tab.target)
            });
            if let Some(target) = clicked {
                (state.on_click)(target);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: our own window; the pointer is cleared so that a late
            // message cannot follow it.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            LRESULT(0)
        }
        // SAFETY: everything else is the default handler's business.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Paints the tabs.
fn draw(state: &StackbarState, hwnd: HWND) -> Result<()> {
    let Ok(mut inner) = state.inner.try_borrow_mut() else {
        return Ok(());
    };

    let layout = inner.layout;
    if layout.is_empty() {
        return Ok(());
    }
    let width = u32::try_from(layout.bar.width().max(1)).unwrap_or(1);
    let height = u32::try_from(layout.bar.height().max(1)).unwrap_or(1);

    ensure_renderer(&mut inner, &state.factory, hwnd, width, height)?;
    ensure_format(&mut inner, &state.dwrite)?;

    let (Some(target), Some(format)) = (inner.renderer.clone(), inner.format.clone()) else {
        return Ok(());
    };
    let style = inner.style.clone();
    let tabs = inner.tabs.clone();
    drop(inner);

    // SAFETY: the render target belongs to this window and this thread, and
    // every pointer below is to a live local. BeginDraw is paired with EndDraw,
    // which is also what reports a lost target.
    let result = unsafe {
        target.BeginDraw();
        target.Clear(Some(&style.unfocused_background.to_d2d()));

        for (index, tab) in tabs.iter().enumerate() {
            let Some((left, right)) = layout.tab_bounds(index) else {
                break;
            };
            let background = if tab.focused {
                style.focused_background
            } else {
                style.unfocused_background
            };
            let text_colour = if tab.focused {
                style.focused_text
            } else {
                style.unfocused_text
            };

            let bounds = D2D_RECT_F {
                left: left as f32,
                top: 0.0,
                right: right as f32,
                bottom: height as f32,
            };

            if let Ok(brush) = target.CreateSolidColorBrush(&background.to_d2d(), None) {
                target.FillRectangle(&bounds, &brush);
            }

            let label = wide_no_nul(&tab.label);
            if label.is_empty() {
                continue;
            }
            let text_bounds = D2D_RECT_F {
                left: bounds.left + LABEL_PADDING,
                top: bounds.top,
                right: (bounds.right - LABEL_PADDING).max(bounds.left + LABEL_PADDING),
                bottom: bounds.bottom,
            };
            if let Ok(brush) = target.CreateSolidColorBrush(&text_colour.to_d2d(), None) {
                target.DrawText(
                    &label,
                    &format,
                    &text_bounds,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }

        target.EndDraw(None, None)
    };

    if let Err(error) = result {
        // The target is gone, for instance after a display change; the next
        // paint builds a new one.
        if let Ok(mut inner) = state.inner.try_borrow_mut() {
            inner.renderer = None;
        }
        return Err(RenderError::Win32(error));
    }
    Ok(())
}

/// Makes sure there is a render target of the right size.
fn ensure_renderer(
    inner: &mut Inner,
    factory: &ID2D1Factory,
    hwnd: HWND,
    width: u32,
    height: u32,
) -> Result<()> {
    if let Some(target) = inner.renderer.as_ref() {
        if inner.size == (width, height) {
            return Ok(());
        }
        let size = D2D_SIZE_U { width, height };
        // SAFETY: the target belongs to this window; resizing it to the window's
        // new client size is what it is for.
        if unsafe { target.Resize(&size) }.is_ok() {
            inner.size = (width, height);
            return Ok(());
        }
        inner.renderer = None;
    }

    let properties = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            // The bar is opaque, so there is no alpha to keep.
            alphaMode: D2D1_ALPHA_MODE_IGNORE,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let hwnd_properties = D2D1_HWND_RENDER_TARGET_PROPERTIES {
        hwnd,
        pixelSize: D2D_SIZE_U { width, height },
        presentOptions: D2D1_PRESENT_OPTIONS_NONE,
    };

    // SAFETY: both structs are fully initialised and live across the call, and
    // the window belongs to this thread.
    let target = unsafe { factory.CreateHwndRenderTarget(&properties, &hwnd_properties) }?;
    inner.renderer = Some(target);
    inner.size = (width, height);
    Ok(())
}

/// Makes sure there is a text format for the configured font.
fn ensure_format(inner: &mut Inner, dwrite: &IDWriteFactory) -> Result<()> {
    if inner.format.is_some() {
        return Ok(());
    }

    let family = wide(inner.style.font_family.as_deref().unwrap_or("Segoe UI"));
    let locale = wide("en-us");
    let size = (inner.style.font_size.max(1.0)) * POINTS_TO_DIP;

    // SAFETY: both wide strings outlive the call and DirectWrite copies what it
    // needs out of them.
    let format = unsafe {
        dwrite.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            PCWSTR(locale.as_ptr()),
        )
    }?;

    // SAFETY: the format was just created and is only used from this thread.
    unsafe {
        let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
    }

    inner.format = Some(format);
    Ok(())
}

/// UTF-16 without the terminator, which is what `DrawText` wants.
fn wide_no_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}
