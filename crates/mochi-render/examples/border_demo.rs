//! Two demo windows with Mochi borders on the primary monitor, one of them
//! animated, then everything goes away again.
//!
//! ```text
//! cargo run -p mochi-render --example border_demo [milliseconds]
//! ```
//!
//! The demo creates its own plain windows to hang the borders on, so it never
//! touches a window it did not make. The left one gets the focused colour and
//! is animated with the same driver the daemon uses; the right one gets the
//! unfocused colour and stays put.

use std::time::Duration;

use mochi_render::{
    AnimationConfig, Animator, BorderConfig, BorderKind, BorderManager, BorderSpec, FrameUpdate,
    Rect, WindowHandle,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics, MSG,
    PM_REMOVE, PeekMessageW, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNA, SWP_NOACTIVATE,
    SWP_NOZORDER, SetWindowPos, ShowWindow, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::PCWSTR;

/// Catppuccin Mocha base, so the pink border has something dark to sit on.
const BASE: u32 = 0x002e_1e1e; // COLORREF is 0x00bbggrr

fn main() -> mochi_render::Result<()> {
    // The daemon gets this from its manifest; an example has to ask for it, or
    // every coordinate below would be scaled behind our back.
    // SAFETY: called once, before any window exists.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let millis: u64 = std::env::args()
        .nth(1)
        .and_then(|argument| argument.parse().ok())
        .unwrap_or(2000);

    // SAFETY: GetSystemMetrics takes an enum and cannot fail.
    let (screen_width, screen_height) =
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let unit_x = |fraction: f32| (screen_width as f32 * fraction) as i32;
    let unit_y = |fraction: f32| (screen_height as f32 * fraction) as i32;

    let left_rect = Rect::new(unit_x(0.08), unit_y(0.14), unit_x(0.44), unit_y(0.56));
    let right_rect = Rect::new(unit_x(0.52), unit_y(0.14), unit_x(0.88), unit_y(0.56));
    let moved_rect = Rect::new(
        left_rect.left + unit_x(0.04),
        left_rect.top + unit_y(0.20),
        left_rect.right + unit_x(0.04),
        left_rect.bottom + unit_y(0.20),
    );

    println!("primary monitor {screen_width}x{screen_height}");
    println!("focused   {left_rect:?} -> {moved_rect:?}");
    println!("unfocused {right_rect:?}");

    let left = DemoWindow::new(left_rect)?;
    let right = DemoWindow::new(right_rect)?;

    let borders = BorderManager::new(BorderConfig::default())?;
    let still = BorderSpec::new(right.handle(), right_rect, BorderKind::Unfocused);
    borders.set_borders(vec![
        BorderSpec::new(left.handle(), left_rect, BorderKind::Single),
        still,
    ])?;

    // Give the first frame a moment, then animate the left window and let its
    // border ride along, exactly the way the daemon will drive it.
    pump_for(Duration::from_millis(millis / 4));

    let moving = borders.clone();
    let animator = Animator::new(move |frame: &[FrameUpdate]| {
        // One callback per frame: the daemon would push this through a single
        // DeferWindowPos batch.
        let mut specs = vec![still];
        for update in frame {
            move_window(update.handle.hwnd(), update.rect);
            specs.push(BorderSpec::new(
                update.handle,
                update.rect,
                BorderKind::Single,
            ));
        }
        if let Err(error) = moving.set_borders(specs) {
            eprintln!("border update failed: {error}");
        }
    })?;

    let animation = AnimationConfig::default();
    println!(
        "animating {} ms, {:?}, {} fps",
        animation.duration_ms,
        animation.style,
        animation.fps()
    );
    animator.animate(vec![animation.job(left.handle(), left_rect, moved_rect)])?;

    pump_for(Duration::from_millis(millis));

    borders.stop();
    drop(animator);
    drop(left);
    drop(right);
    Ok(())
}

/// Runs the demo windows' message loop for a while.
fn pump_for(duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline {
        let mut message = MSG::default();
        // SAFETY: `message` is a live local and a null window filter means every
        // window of this thread.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: the message was just filled in.
            unsafe {
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Moves a demo window without touching its z-order or the focus.
fn move_window(hwnd: HWND, rect: Rect) {
    // SAFETY: a window this example created.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

/// A plain dark popup for a border to hang on.
struct DemoWindow {
    hwnd: HWND,
}

impl DemoWindow {
    fn new(rect: Rect) -> mochi_render::Result<Self> {
        let class: Vec<u16> = "MochiBorderDemo\0".encode_utf16().collect();
        let title: Vec<u16> = "mochi border demo\0".encode_utf16().collect();

        // SAFETY: registering the same class twice simply fails, which is why
        // the error is ignored; the CreateWindowExW below is what really
        // reports a problem.
        unsafe {
            let descriptor = WNDCLASSEXW {
                cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap_or(0),
                lpfnWndProc: Some(demo_wndproc),
                hInstance: GetModuleHandleW(PCWSTR::null()).unwrap_or_default().into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                hbrBackground: HBRUSH(
                    CreateSolidBrush(windows::Win32::Foundation::COLORREF(BASE)).0,
                ),
                ..Default::default()
            };
            RegisterClassExW(&descriptor);
        }

        // Topmost so that a window opening behind our back cannot bury the
        // demo halfway through it; it also exercises the topmost path in the
        // border's z-order tracking.
        // SAFETY: the class exists and both strings outlive the call.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                None,
                None,
                None,
                None,
            )
        }?;

        // SAFETY: our own window; SW_SHOWNA shows it without taking the focus
        // away from whatever the user was doing.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
        }

        Ok(Self { hwnd })
    }

    fn handle(&self) -> WindowHandle {
        WindowHandle::from(self.hwnd)
    }
}

impl Drop for DemoWindow {
    fn drop(&mut self) {
        // SAFETY: our own window, destroyed on the thread that created it.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Nothing to do: the class background brush paints the window.
unsafe extern "system" fn demo_wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: straight pass-through to the default handler.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
