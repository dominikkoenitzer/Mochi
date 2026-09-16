//! A Direct2D render target that paints into a DIB and is shown with
//! `UpdateLayeredWindow`.
//!
//! A plain `ID2D1HwndRenderTarget` cannot give a window per-pixel alpha, and
//! `SetLayeredWindowAttributes` with a colour key leaves jagged, keyed edges
//! where a rounded corner should fade out. The combination used here is the one
//! that keeps the antialiasing: Direct2D renders premultiplied BGRA into a top
//! down 32 bit DIB section, and `UpdateLayeredWindow` hands that bitmap to the
//! compositor with `AC_SRC_ALPHA`, so every pixel keeps its own alpha and the
//! untouched middle of the window stays perfectly transparent.

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, ID2D1DCRenderTarget, ID2D1Factory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};

use crate::Result;
use crate::win::BASE_DPI;

/// The GDI half: a memory DC with a 32 bit top down DIB selected into it.
struct Dib {
    memory_dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Dib {
    fn new(width: i32, height: i32) -> Result<Self> {
        let header = BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(40),
            biWidth: width,
            // Negative height: a top down bitmap, so row 0 is the top row and
            // Direct2D and GDI agree which way up the image is.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            ..Default::default()
        };

        // SAFETY: CreateCompatibleDC(NULL) makes a memory DC compatible with the
        // screen. It is deleted in Drop.
        let memory_dc = unsafe { CreateCompatibleDC(None) };
        if memory_dc.is_invalid() {
            return Err(windows::core::Error::from_thread().into());
        }

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `info` describes the 32 bit top down DIB above and `bits` is a
        // live pointer slot. The bitmap is owned here and deleted in Drop; its
        // pixels belong to the bitmap, so `bits` is never freed separately.
        let bitmap = unsafe {
            CreateDIBSection(
                Some(memory_dc),
                &info,
                DIB_RGB_COLORS,
                &raw mut bits,
                None,
                0,
            )
        };

        let bitmap = match bitmap {
            Ok(bitmap) => bitmap,
            Err(error) => {
                // SAFETY: the DC was created above and has nothing selected into
                // it yet.
                unsafe {
                    let _ = DeleteDC(memory_dc);
                }
                return Err(error.into());
            }
        };

        // SAFETY: both handles are live and the bitmap is not selected into any
        // other DC. The previous object is restored in Drop, as GDI requires
        // before a bitmap may be deleted.
        let previous = unsafe { SelectObject(memory_dc, HGDIOBJ::from(bitmap)) };

        Ok(Self {
            memory_dc,
            bitmap,
            previous,
        })
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: undo the objects in the reverse order they were made:
        // deselect the bitmap, delete it, delete the DC. All three handles were
        // created by this struct and are not shared.
        unsafe {
            SelectObject(self.memory_dc, self.previous);
            let _ = DeleteObject(HGDIOBJ::from(self.bitmap));
            let _ = DeleteDC(self.memory_dc);
        }
    }
}

/// An off-screen surface sized to one layered window.
pub(crate) struct LayeredSurface {
    /// Declared before `dib` on purpose: fields drop in declaration order, so
    /// the render target releases its binding before the DC is deleted.
    target: ID2D1DCRenderTarget,
    dib: Dib,
    width: i32,
    height: i32,
}

impl LayeredSurface {
    /// Builds a surface of exactly `width` by `height` physical pixels.
    ///
    /// # Errors
    ///
    /// Any failure from GDI or Direct2D.
    pub(crate) fn new(factory: &ID2D1Factory, width: i32, height: i32) -> Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let dib = Dib::new(width, height)?;

        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            // Pinned to 96 so that one Direct2D unit is one physical pixel and
            // all the geometry maths in this crate stays in pixels.
            dpiX: BASE_DPI as f32,
            dpiY: BASE_DPI as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };

        // SAFETY: the properties struct is fully initialised and lives across
        // the call.
        let target = unsafe { factory.CreateDCRenderTarget(&properties) }?;

        let bounds = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: the DC belongs to `dib`, which this surface owns and keeps
        // alive for as long as the render target.
        unsafe { target.BindDC(dib.memory_dc, &bounds) }?;

        Ok(Self {
            target,
            dib,
            width,
            height,
        })
    }

    /// The Direct2D render target to draw into.
    pub(crate) fn target(&self) -> &ID2D1DCRenderTarget {
        &self.target
    }

    /// `true` when the surface is already exactly this size.
    pub(crate) fn fits(&self, width: i32, height: i32) -> bool {
        self.width == width.max(1) && self.height == height.max(1)
    }

    /// Pushes the painted bitmap onto `hwnd`, moving and sizing it at the same
    /// time.
    ///
    /// One call does position, size and pixels together, which is what keeps a
    /// resizing border from tearing.
    ///
    /// # Errors
    ///
    /// When the window has gone away or the compositor rejects the update.
    pub(crate) fn present(&self, hwnd: HWND, x: i32, y: i32) -> Result<()> {
        let position = POINT { x, y };
        let size = SIZE {
            cx: self.width,
            cy: self.height,
        };
        let origin = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            // Per-pixel alpha only; the whole-window alpha stays at full.
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        // SAFETY: every pointer is to a live local, the source DC holds the DIB
        // this surface owns, and `hwnd` is a window this crate created with
        // WS_EX_LAYERED, which UpdateLayeredWindow requires.
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&position),
                Some(&size),
                Some(self.dib.memory_dc),
                Some(&origin),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )?;
        }
        Ok(())
    }
}
