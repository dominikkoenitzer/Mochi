//! A window of the browser engine, moved and minimized from outside.
//!
//! The picture of such a window is drawn into a GPU child that belongs to
//! another process. Nothing places that child by hand: Windows carries it along
//! when the window travels to and from the spot minimized windows are parked
//! at. A move made from another thread can land in the middle of that journey,
//! and the child then stays a whole screen away from its window, which keeps
//! running and shows the same picture from then on.
//!
//! The stand-in windows here pin what the daemon does about it. They have the
//! class prefix and nothing else of the engine, and they do not lose a child,
//! so the last test runs the real thing under the real daemon.

use std::collections::BTreeSet;
use std::path::Path;

use mochi::platform::{Hwnd, Platform, ShowState, Win32Platform, WindowPlacement};
use mochi_testbed::TEST_ENGINE_WINDOW_CLASS;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetClientRect, GetWindowRect, GetWindowTextW, IsIconic,
    IsWindowVisible, IsZoomed, PostMessageW, SC_MINIMIZE, SC_RESTORE, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowPos, WM_SYSCOMMAND,
};

use super::borders::{rice, write_json};
use super::monitors::EVENT_WINDOW_CLASS;
use super::*;

/// How long a stand-in stops pumping for.
const STALL: Duration = Duration::from_millis(1500);
/// What a call may cost while its window is stalled. Well above a posted call
/// on a loaded runner, well below the stall a blocking call would sit through.
const BARELY: Duration = Duration::from_millis(500);
/// Where the tests send a window, a perceived frame.
const TARGET: (i32, i32, i32, i32) = (140, 120, 760, 620);
/// How far a frame may land from its target: rounding only.
const LANDED: i32 = 2;

fn target_rect() -> mochi_core::Rect {
    mochi_core::Rect::new(TARGET.0, TARGET.1, TARGET.2, TARGET.3)
}

fn platform_handle(hwnd: i64) -> Hwnd {
    Hwnd(hwnd as isize)
}

fn as_hwnd(hwnd: i64) -> HWND {
    HWND(hwnd as *mut _)
}

fn spawn_stand_in() -> TestWindows {
    TestWindows::spawn_with(&SpawnOptions {
        engine_class: true,
        ..SpawnOptions::new(1, 0)
    })
    .expect("could not spawn the stand-in")
}

fn is_iconic(hwnd: i64) -> bool {
    unsafe { IsIconic(as_hwnd(hwnd)) }.as_bool()
}

fn at_target(hwnd: i64) -> bool {
    let wanted = Rect::new(TARGET.0, TARGET.1, TARGET.2, TARGET.3);
    mochi_testbed::frame_bounds(hwnd).is_ok_and(|frame| close_enough(frame, wanted, LANDED))
}

/// Stalls the window's thread and gives it the time to start sleeping.
fn stall(hwnd: i64) {
    mochi_testbed::stall_window(hwnd, STALL).expect("could not stall the window");
    std::thread::sleep(Duration::from_millis(100));
}

#[test]
fn a_move_does_not_wait_for_a_busy_engine_window() {
    skip_unless_allowed!("a_move_does_not_wait_for_a_busy_engine_window");
    let windows = spawn_stand_in();
    let hwnd = windows.handles()[0];
    stall(hwnd);

    let started = Instant::now();
    Win32Platform::new()
        .set_positions(&[WindowPlacement::new(platform_handle(hwnd), target_rect())])
        .expect("the move was refused");
    let took = started.elapsed();

    assert!(
        took < BARELY,
        "the move sat through the window's stall for {took:?}, so it ran on the caller's thread \
         and can land in the middle of a minimize"
    );
    wait_for(STALL + STEP, || at_target(hwnd)).expect("the posted move never landed");
}

#[test]
fn a_state_change_does_not_wait_for_a_busy_engine_window() {
    skip_unless_allowed!("a_state_change_does_not_wait_for_a_busy_engine_window");
    let windows = spawn_stand_in();
    let hwnd = windows.handles()[0];
    let platform = Win32Platform::new();

    for (state, iconic) in [
        (ShowState::MinimizeNoActivate, true),
        (ShowState::RestoreNoActivate, false),
        (ShowState::Minimize, true),
        (ShowState::Restore, false),
    ] {
        stall(hwnd);
        let started = Instant::now();
        platform
            .show(platform_handle(hwnd), state)
            .expect("the state change was refused");
        let took = started.elapsed();
        assert!(
            took < BARELY,
            "{state:?} sat through the stall for {took:?}"
        );
        wait_for(STALL + STEP, || is_iconic(hwnd) == iconic)
            .unwrap_or_else(|_| panic!("{state:?} never landed"));
    }
}

/// Hiding and showing are no transition Windows carries a child through, and a
/// window whose hide is still queued reads as there to whoever looks next, and
/// the other way round as gone. So these two are done when the call returns.
#[test]
fn hiding_and_showing_an_engine_window_have_landed_when_the_call_returns() {
    skip_unless_allowed!("hiding_and_showing_an_engine_window_have_landed_when_the_call_returns");
    let windows = spawn_stand_in();
    let hwnd = windows.handles()[0];
    let platform = Win32Platform::new();

    for (state, visible) in [(ShowState::Hide, false), (ShowState::ShowNoActivate, true)] {
        stall(hwnd);
        platform
            .show(platform_handle(hwnd), state)
            .expect("the state change was refused");
        assert_eq!(
            unsafe { IsWindowVisible(as_hwnd(hwnd)) }.as_bool(),
            visible,
            "{state:?} had not landed when the call returned"
        );
    }
}

/// The two calls of a redraw, on a maximized window.
#[test]
fn a_redraw_keeps_a_maximized_engine_window_maximized() {
    skip_unless_allowed!("a_redraw_keeps_a_maximized_engine_window_maximized");
    let windows = spawn_stand_in();
    let hwnd = windows.handles()[0];
    let platform = Win32Platform::new();
    let zoomed = || unsafe { IsZoomed(as_hwnd(hwnd)) }.as_bool();

    platform
        .show(platform_handle(hwnd), ShowState::Maximize)
        .expect("the maximize was refused");
    wait_for(STEP, zoomed).expect("the window never maximized");
    for (state, iconic) in [
        (ShowState::MinimizeNoActivate, true),
        (ShowState::RestoreNoActivate, false),
    ] {
        platform
            .show(platform_handle(hwnd), state)
            .expect("the state change was refused");
        wait_for(STEP, || is_iconic(hwnd) == iconic)
            .unwrap_or_else(|_| panic!("{state:?} never landed"));
    }
    assert!(zoomed(), "the redraw took the window out of maximized");
}

/// What a restored window goes through in one go when it comes back to its
/// tile with the keyboard. A move that overtook the restore would only change
/// where the window goes the next time it is minimized, and it would come back
/// to its old place.
#[test]
fn a_restore_a_move_and_the_keyboard_land_in_order() {
    skip_unless_allowed!("a_restore_a_move_and_the_keyboard_land_in_order");
    if !cloak_test_allowed() {
        eprintln!(
            "skipping a_restore_a_move_and_the_keyboard_land_in_order: MOCHI_E2E_CLOAK is not 1."
        );
        eprintln!("  It spawns a window without the tool window bit, which any other window");
        eprintln!("  manager on this desktop would take.");
        return;
    }
    // A taskbar button, like every window of the engine.
    let windows = TestWindows::spawn_with(&SpawnOptions {
        engine_class: true,
        taskbar: true,
        ..SpawnOptions::new(1, 0)
    })
    .expect("could not spawn the stand-in");
    let hwnd = windows.handles()[0];
    let target = platform_handle(hwnd);
    let platform = Win32Platform::new();

    mochi_testbed::minimize_window(hwnd).expect("could not minimize the stand-in");
    wait_for(STEP, || is_iconic(hwnd)).expect("the stand-in never minimized");

    stall(hwnd);
    platform
        .show(target, ShowState::Restore)
        .expect("the restore was refused");
    platform
        .set_positions(&[WindowPlacement::new(target, target_rect())])
        .expect("the move was refused");
    platform.focus(target).expect("the focus was refused");

    wait_for(STALL + STEP, || !is_iconic(hwnd) && at_target(hwnd)).unwrap_or_else(|_| {
        panic!(
            "the window ended at {:?}, iconic {}",
            mochi_testbed::frame_bounds(hwnd),
            is_iconic(hwnd)
        )
    });
    wait_for(STEP, || mochi_testbed::foreground_window() == hwnd)
        .expect("the window did not get the keyboard");
}

/// Everything else still moves the way it always has, because the layout
/// counts on an ordinary window being where it was sent once the call returns.
#[test]
fn an_ordinary_window_has_moved_when_the_call_returns() {
    skip_unless_allowed!("an_ordinary_window_has_moved_when_the_call_returns");
    let ordinary = TestWindows::spawn_with(&SpawnOptions::new(1, 0))
        .expect("could not spawn the ordinary window");
    let engine = spawn_stand_in();
    let ordinary = ordinary.handles()[0];
    let engine = engine.handles()[0];

    Win32Platform::new()
        .set_positions(&[
            WindowPlacement::new(platform_handle(engine), target_rect()),
            WindowPlacement::new(platform_handle(ordinary), target_rect()),
        ])
        .expect("the batch was refused");

    assert!(
        at_target(ordinary),
        "the ordinary window is at {:?} after the call returned",
        mochi_testbed::frame_bounds(ordinary)
    );
    wait_for(STEP, || at_target(engine)).expect("the posted half of the batch never landed");
}

/// The redraw after a display change must not move the keyboard, not even for
/// the moment it takes to hand it back. A game holding a screen drops out of it
/// when the keyboard leaves, which is a display change, which is the next
/// redraw.
#[test]
fn a_redraw_leaves_the_keyboard_where_it_is() {
    skip_unless_allowed!("a_redraw_leaves_the_keyboard_where_it_is");
    a_redraw_with_the_keyboard_on("a_redraw_leaves_the_keyboard_where_it_is", false);
}

/// The window being redrawn can be the one being typed in. It goes down and
/// comes back without activating anything, and nothing else may pick up the
/// keyboard along the way.
#[test]
fn a_redrawn_window_keeps_the_keyboard_it_holds() {
    skip_unless_allowed!("a_redrawn_window_keeps_the_keyboard_it_holds");
    a_redraw_with_the_keyboard_on("a_redrawn_window_keeps_the_keyboard_it_holds", true);
}

fn a_redraw_with_the_keyboard_on(name: &str, engine_holds_it: bool) {
    if !cloak_test_allowed() {
        eprintln!("skipping {name}: MOCHI_E2E_CLOAK is not 1.");
        eprintln!("  It spawns a window without the tool window bit, which any other window");
        eprintln!("  manager on this desktop would take.");
        return;
    }

    let config = write_json("mochi-engine-redraw.json", &rice());
    let daemon = Daemon::start_with_config(
        "engine-redraw",
        &config,
        &[
            "--no-hotkeys",
            mochi_testbed::DAEMON_MANAGE_FLAG,
            TEST_ENGINE_WINDOW_CLASS,
        ],
    );
    let log = daemon.log();
    let keyboard = TestWindows::spawn_with(&SpawnOptions::new(1, 0))
        .expect("could not spawn the window that holds the keyboard");
    let engine = TestWindows::spawn_with(&SpawnOptions {
        engine_class: true,
        taskbar: true,
        ..SpawnOptions::new(1, 0)
    })
    .expect("could not spawn the stand-in");
    let keyboard = keyboard.handles()[0];
    let engine = engine.handles()[0];
    let holder = if engine_holds_it { engine } else { keyboard };

    let mut steps = Steps::default();

    steps.step("both windows are managed", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))
    });

    steps.step("the window to hold the keyboard holds it", || {
        std::thread::sleep(Duration::from_millis(600));
        mochi_testbed::bring_to_front(holder, STEP).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(600));
        check(
            mochi_testbed::foreground_window() == holder,
            "something took the keyboard before the display change",
        )
    });

    steps.step("the redraw runs and the keyboard stays", || {
        let pid = daemon.child.id();
        let event_window = mochi_testbed::windows_of_class(EVENT_WINDOW_CLASS)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|w| w.pid == pid)
            .ok_or("the daemon has no message window")?;
        mochi_testbed::post_display_change(event_window.hwnd).map_err(|e| e.to_string())?;

        // A spin, not a tick: the keyboard can be gone and back within a
        // millisecond, and a blink is all it takes to drop a game.
        let mut holders = BTreeSet::new();
        let mut went_down = false;
        let mut came_back = false;
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            let now = mochi_testbed::foreground_window();
            if now != holder {
                holders.insert(now);
            }
            let iconic = is_iconic(engine);
            went_down |= iconic;
            if went_down && !iconic && !came_back {
                came_back = true;
                // The hand back comes right behind the restore.
                let tail = Instant::now() + Duration::from_millis(500);
                while Instant::now() < tail {
                    let now = mochi_testbed::foreground_window();
                    if now != holder {
                        holders.insert(now);
                    }
                    std::thread::yield_now();
                }
                break;
            }
            std::thread::yield_now();
        }

        check(went_down, "the redraw never minimized the window")?;
        check(came_back, "the redraw never restored the window")?;
        if engine_holds_it {
            // A window going down may read as holding nothing for that moment.
            // Where the keyboard ends is what counts.
            let now = mochi_testbed::foreground_window();
            return check(
                now == holder,
                format!("the keyboard went from the redrawn window {holder} to {now}"),
            );
        }
        check(
            holders.is_empty(),
            format!(
                "the keyboard left {holder} during the redraw, for {holders:?}; the redrawn \
                 window is {engine}"
            ),
        )
    });

    steps.step("the redrawn window stays managed", || {
        wait_for(STEP, || managed_count() == 2)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))
    });

    drop(daemon);
    steps.finish(&log);
}

// ---------------------------------------------------------------------------
// the real thing
// ---------------------------------------------------------------------------

/// The top-level class of a real engine window.
const HOST_CLASS: &str = "Chrome_WidgetWin_1";
/// The class of the child the GPU process draws into.
const GPU_CHILD_CLASS: &str = "Intermediate D3D Window";
/// Cycles unless `MOCHI_E2E_ENGINE_CYCLES` says otherwise. The race was measured
/// at a few in a hundred, so this many all but always hits it on the old code.
const DEFAULT_CYCLES: u32 = 150;

/// An animation long enough to be in flight when the minimize comes.
fn long_animation() -> Value {
    let mut config = rice();
    config["animation"]["duration"] = 400.into();
    config
}

/// A page that never stops painting, so the host keeps its GPU child busy.
const PAGE: &str = "<!doctype html><title>MARKER</title>\
<body style=\"margin:0;background:#121212\"><canvas id=c width=900 height=700></canvas>\
<script>const g=document.getElementById('c').getContext('2d');let n=0;\
(function f(){n++;for(let i=0;i<40;i++){g.fillStyle=`hsl(${(n+i*9)%360},70%,50%)`;\
g.fillRect((n*3+i*23)%900,(i*17)%700,22,22)}requestAnimationFrame(f)})();</script>";

/// What one look at a host found, each child relative to the client area.
struct ChildReading {
    origin: POINT,
    client: RECT,
    children: Vec<(HWND, RECT)>,
}

impl ChildReading {
    /// True when every GPU child is outside the client area.
    fn is_displaced(&self) -> bool {
        !self.children.is_empty()
            && self.children.iter().all(|(_, r)| {
                r.right <= 0
                    || r.bottom <= 0
                    || r.left >= self.client.right
                    || r.top >= self.client.bottom
            })
    }

    fn describe(&self) -> String {
        format!(
            "origin {:?}, client {:?}, children {:?}",
            self.origin, self.client, self.children
        )
    }
}

/// A real engine process with one window, ended with all its helpers on drop.
struct EngineHost {
    child: Child,
    hwnd: i64,
    dir: PathBuf,
}

impl EngineHost {
    fn start() -> EngineHost {
        let marker = format!("engine-host-{}", std::process::id());
        let dir = temp_dir().join(&marker);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("could not create the host's folder");
        let page = dir.join("page.html");
        std::fs::write(&page, PAGE.replace("MARKER", &marker)).expect("could not write the page");

        let mut command = host_command(&page, &dir.join("profile"));
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("could not start the engine host");
        let mut host = EngineHost {
            child,
            hwnd: 0,
            dir,
        };
        host.hwnd = wait_some(Duration::from_secs(40), || window_titled(&marker))
            .expect("the engine host never showed its window");
        // A window showing does not mean the GPU process is up; on a slow runner it can take longer than any fixed pause.
        let _ = wait_some(Duration::from_secs(40), || {
            (!host.gpu_children().is_empty()).then_some(())
        });
        host
    }

    /// One look at the GPU children and the client area they belong in.
    fn reading(&self) -> ChildReading {
        let parent = as_hwnd(self.hwnd);
        let mut origin = POINT::default();
        let _ = unsafe { ClientToScreen(parent, &raw mut origin) };
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(parent, &raw mut client) };
        let children = children_of(parent)
            .into_iter()
            .filter(|&child| class_of(child) == GPU_CHILD_CLASS)
            .filter_map(|child| {
                let mut r = RECT::default();
                unsafe { GetWindowRect(child, &raw mut r) }.ok()?;
                Some((
                    child,
                    RECT {
                        left: r.left - origin.x,
                        top: r.top - origin.y,
                        right: r.right - origin.x,
                        bottom: r.bottom - origin.y,
                    },
                ))
            })
            .collect();
        ChildReading {
            origin,
            client,
            children,
        }
    }

    /// Each GPU child, as its rectangle relative to the client area.
    fn gpu_children(&self) -> Vec<(HWND, RECT)> {
        self.reading().children
    }

    fn child_is_displaced(&self) -> bool {
        self.reading().is_displaced()
    }

    /// Puts the children back so one lost child is not counted on every cycle
    /// that follows.
    fn put_children_back(&self) {
        for (child, _) in self.gpu_children() {
            let _ = unsafe {
                SetWindowPos(
                    child,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
        }
    }

    fn post_system_command(&self, command: u32) {
        let _ = unsafe {
            PostMessageW(
                Some(as_hwnd(self.hwnd)),
                WM_SYSCOMMAND,
                WPARAM(command as usize),
                LPARAM(0),
            )
        };
    }
}

impl Drop for EngineHost {
    fn drop(&mut self) {
        let _ = OsCommand::new("taskkill")
            .args(["/T", "/F", "/PID", &self.child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How to start the host: the one `MOCHI_E2E_ENGINE_HOST` names as
/// `program|argument|argument`, which gets the page as its last argument, or
/// else the first browser on this machine that is built on the engine.
fn host_command(page: &Path, profile: &Path) -> OsCommand {
    let flags = std::env::var("MOCHI_E2E_ENGINE_FLAGS").unwrap_or_default();
    let flags = flags.split('|').filter(|flag| !flag.is_empty());
    let user_data = format!("--user-data-dir={}", profile.display());

    if let Ok(named) = std::env::var("MOCHI_E2E_ENGINE_HOST") {
        let mut parts = named.split('|');
        let mut command = OsCommand::new(parts.next().expect("split yields at least one"));
        command
            .args(parts)
            .arg(user_data)
            .arg("--mute-audio")
            .args(flags)
            .arg(page);
        return command;
    }

    let browser = registered_engine_browser()
        .expect("no browser built on the engine is registered on this machine");
    let address = format!(
        "--app=file:///{}",
        page.display().to_string().replace('\\', "/")
    );
    let mut command = OsCommand::new(browser);
    command
        .arg(user_data)
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-sync",
            "--mute-audio",
            "--window-position=120,80",
            "--window-size=1000,760",
        ])
        .arg(address)
        .args(flags);
    command
}

/// The first registered browser with the engine's loader next to it.
fn registered_engine_browser() -> Option<PathBuf> {
    let out = OsCommand::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Clients\StartMenuInternet",
            "/s",
            "/ve",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if !line.trim().ends_with(r"\shell\open\command") {
            continue;
        }
        let value = lines.next()?;
        let (_, command) = value.split_once("REG_SZ")?;
        let exe = PathBuf::from(command.trim().trim_matches('"'));
        if has_engine_loader(&exe) {
            return Some(exe);
        }
    }
    None
}

fn has_engine_loader(exe: &Path) -> bool {
    let Some(entries) = exe.parent().and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return false;
    };
    entries.flatten().any(|version| {
        std::fs::read_dir(version.path()).is_ok_and(|mut files| {
            files.any(|file| {
                file.is_ok_and(|file| file.file_name().to_string_lossy().ends_with("_elf.dll"))
            })
        })
    })
}

fn window_titled(marker: &str) -> Option<i64> {
    mochi_testbed::windows_of_class(HOST_CLASS)
        .ok()?
        .into_iter()
        .filter(|w| w.visible)
        .find(|w| {
            let mut buffer = [0u16; 256];
            let length = unsafe { GetWindowTextW(as_hwnd(w.hwnd), &mut buffer) };
            String::from_utf16_lossy(&buffer[..length.max(0) as usize]).contains(marker)
        })
        .map(|w| w.hwnd)
}

fn class_of(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

fn children_of(parent: HWND) -> Vec<HWND> {
    unsafe extern "system" fn visit(child: HWND, found: LPARAM) -> windows::core::BOOL {
        unsafe { (*(found.0 as *mut Vec<HWND>)).push(child) };
        true.into()
    }
    let mut found: Vec<HWND> = Vec::new();
    let _ = unsafe {
        EnumChildWindows(
            Some(parent),
            Some(visit),
            LPARAM(std::ptr::from_mut(&mut found) as isize),
        )
    };
    found
}

/// The invisible border beside and below a window's frame.
fn invisible_border(hwnd: i64) -> (i32, i32) {
    let mut window = RECT::default();
    unsafe { GetWindowRect(as_hwnd(hwnd), &mut window) }.expect("could not read the window");
    let frame = mochi_testbed::frame_bounds(hwnd).expect("could not read the frame");
    (frame.left - window.left, window.bottom - frame.bottom)
}

/// A pause of up to 150 ms from a fixed sequence, so a run can be repeated.
fn jitter(state: &mut u64) -> Duration {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    Duration::from_millis(*state % 150)
}

/// The user minimizing and restoring a window while the layout is moving it.
///
/// A real engine window under the real daemon, because that is the only thing
/// that loses a child: a plain window with a child in a third process almost
/// never did in thousands of cycles. `MOCHI_E2E_ENGINE_HOST` and
/// `MOCHI_E2E_ENGINE_FLAGS` point it at another host or at software rendering.
#[test]
fn a_gpu_child_stays_on_its_window_under_animation_frames() {
    skip_unless_allowed!("a_gpu_child_stays_on_its_window_under_animation_frames");
    if !cloak_test_allowed() {
        eprintln!("skipping a_gpu_child_stays_on_its_window_under_animation_frames:");
        eprintln!("  MOCHI_E2E_CLOAK is not 1. This test opens a browser window, which any other");
        eprintln!("  window manager on this desktop would take.");
        return;
    }
    let cycles = std::env::var("MOCHI_E2E_ENGINE_CYCLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CYCLES);

    mochi_testbed::ensure_per_monitor_v2();
    let config = write_json("mochi-engine-frames.json", &long_animation());
    let daemon = Daemon::start_with_config(
        "engine-frames",
        &config,
        &[
            "--no-hotkeys",
            mochi_testbed::DAEMON_MANAGE_FLAG,
            HOST_CLASS,
        ],
    );
    let log = daemon.log();
    let neighbour = TestWindows::spawn_with(&SpawnOptions::new(1, 0))
        .expect("could not spawn the window beside the host");
    let host = EngineHost::start();
    let platform = Win32Platform::new();

    wait_for(Duration::from_secs(15), || managed_count() == 2).unwrap_or_else(|_| {
        panic!(
            "state shows {} managed windows; log: {log}",
            managed_count()
        )
    });
    assert!(
        !host.gpu_children().is_empty(),
        "the host has no GPU child, so this run would prove nothing"
    );
    assert!(
        !host.child_is_displaced(),
        "the GPU child is off its window before anything happened"
    );

    // A move measured while minimized relies on these two matching.
    let (_, below) = invisible_border(host.hwnd);
    host.post_system_command(SC_MINIMIZE);
    wait_for(STEP, || is_iconic(host.hwnd)).expect("the host never minimized");
    let (beside, _) = invisible_border(host.hwnd);
    host.post_system_command(SC_RESTORE);
    wait_for(STEP, || !is_iconic(host.hwnd)).expect("the host never restored");
    assert_eq!(
        beside, below,
        "the border a minimized host shows beside it is not the one it gets back below it"
    );

    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut displaced = 0;
    let mut momentary = 0;
    for cycle in 0..cycles {
        let axis = if cycle % 2 == 0 {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        command(&Command::FlipLayout { axis }).expect("the layout did not flip");
        std::thread::sleep(jitter(&mut state));

        host.post_system_command(SC_MINIMIZE);
        wait_for(STEP, || is_iconic(host.hwnd))
            .unwrap_or_else(|_| panic!("cycle {cycle}: the host never minimized; log: {log}"));
        std::thread::sleep(jitter(&mut state));

        // Most come back through the taskbar's route, the rest through a
        // focus command, which is the daemon's own restore.
        if cycle % 4 == 3 {
            // The restore is what counts here. Windows may refuse this process
            // the keyboard, and the wait below sees whether the window came back.
            let _ = platform.focus(platform_handle(host.hwnd));
        } else {
            host.post_system_command(SC_RESTORE);
        }
        wait_for(STEP, || !is_iconic(host.hwnd))
            .unwrap_or_else(|_| panic!("cycle {cycle}: the host never restored; log: {log}"));
        std::thread::sleep(Duration::from_millis(50));

        // A blank window is one that stays this way. A look can also land
        // inside the restore, which the next look no longer shows.
        let reading = host.reading();
        if reading.is_displaced() {
            std::thread::sleep(Duration::from_millis(500));
            if host.child_is_displaced() {
                displaced += 1;
                eprintln!("cycle {cycle}: GPU child stuck, {}", reading.describe());
                host.put_children_back();
            } else {
                momentary += 1;
                eprintln!(
                    "cycle {cycle}: GPU child back by itself, {}",
                    reading.describe()
                );
            }
        }
    }

    eprintln!(
        "engine frames: {displaced} of {cycles} cycles lost the GPU child, \
         {momentary} read off it for a moment"
    );
    drop(neighbour);
    drop(host);
    drop(daemon);
    assert_eq!(
        displaced, 0,
        "{displaced} of {cycles} cycles left the GPU child off its window; log: {log}"
    );
}
