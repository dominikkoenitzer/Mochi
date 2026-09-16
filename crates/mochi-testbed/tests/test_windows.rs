//! End-to-end tests against real windows on the real desktop.
//!
//! These are safe to run while another window manager is active: everything
//! they create is a `WS_EX_TOOLWINDOW` window of the class `MochiTestWindow`,
//! which no window manager picks up by itself, which never appears in the
//! taskbar or in Alt-Tab, and which is closed again before the test returns.
//! `TestWindows` closes its batch on `Drop`, so a panic cleans up too.
//!
//! Every wait has a deadline and no test sleeps for a fixed time.

use std::time::Duration;

use mochi_testbed::{TEST_WINDOW_CLASS, TestWindows, WS_EX_TOOLWINDOW_BIT, layout_assert};

/// Long enough for a loaded machine, short enough that a hang is not a hang.
const DEADLINE: Duration = Duration::from_secs(10);

/// Skips instead of failing where there is no desktop to draw on, for example
/// in a CI job that runs without a window station.
fn desktop_available() -> bool {
    mochi_testbed::monitors().is_ok_and(|monitors| !monitors.is_empty())
}

#[test]
fn two_test_windows_are_spawned_listed_and_closed() {
    if !desktop_available() {
        eprintln!("skipped: this session has no monitors");
        return;
    }

    let mut batch = TestWindows::spawn(2, 0).expect("two test windows on monitor 0");
    assert_eq!(batch.len(), 2);

    let handles = batch.handles();
    for &hwnd in &handles {
        batch
            .wait_for_rect(hwnd, |rect| !rect.is_empty(), DEADLINE)
            .unwrap_or_else(|e| panic!("window 0x{hwnd:x} never got a rect: {e}"));
    }

    // The desktop-wide listing has to find exactly the windows of this batch,
    // which is the same path `mochi-testwin list` and the daemon's enumeration
    // take.
    let listed = mochi_testbed::list_windows().expect("enumerate test windows");
    let mine: Vec<_> = listed
        .into_iter()
        .filter(|window| handles.contains(&window.hwnd))
        .collect();
    assert_eq!(mine.len(), 2, "both windows are listed");

    let expected_titles: Vec<&str> = batch
        .spawned()
        .iter()
        .map(|window| window.title.as_str())
        .collect();
    for window in &mine {
        assert_eq!(window.class, TEST_WINDOW_CLASS);
        assert!(
            expected_titles.contains(&window.title.as_str()),
            "unexpected title {:?}, wanted one of {expected_titles:?}",
            window.title
        );
        assert!(window.title.starts_with("MochiTest "));
        assert_eq!(window.pid, std::process::id(), "spawned in this process");
        assert_eq!(window.monitor, Some(0), "placed on the requested monitor");
        assert!(window.visible && !window.minimized);

        // The contract that keeps the user's window manager off these windows.
        assert!(
            window.ex_style & WS_EX_TOOLWINDOW_BIT != 0,
            "0x{:x} lost WS_EX_TOOLWINDOW, another manager would grab it",
            window.hwnd
        );

        // A real Windows 11 frame: the window rect is larger than what the user
        // sees, by the invisible resize border tiling has to compensate for.
        assert!(
            window.rect.contains(&window.frame),
            "frame {} is not inside rect {}",
            window.frame,
            window.rect
        );
    }

    // The batch is placed inside the work area of monitor 0, which is also the
    // first real use of the layout assertions.
    let work_area = mochi_testbed::monitor_at(0).expect("monitor 0").work_area;
    let frames: Vec<_> = mine.iter().map(|window| window.frame).collect();
    layout_assert::assert_all_within(&frames, work_area);

    batch.close_all().expect("close both windows");

    for &hwnd in &handles {
        assert!(
            !mochi_testbed::window_exists(hwnd),
            "0x{hwnd:x} is still a window after close_all"
        );
    }
    let still_listed = mochi_testbed::list_windows().expect("enumerate after closing");
    assert!(
        !still_listed
            .iter()
            .any(|window| handles.contains(&window.hwnd)),
        "a closed window is still being listed"
    );
    assert!(batch.windows().is_empty());
}

#[test]
fn a_test_window_follows_the_calls_a_window_manager_makes() {
    if !desktop_available() {
        eprintln!("skipped: this session has no monitors");
        return;
    }

    let batch = TestWindows::spawn(1, 0).expect("one test window on monitor 0");
    let hwnd = batch.handles()[0];
    let work_area = mochi_testbed::monitor_at(0).expect("monitor 0").work_area;

    // Move and resize the way a layout would, then wait for the window to
    // report the new rect rather than sleeping and hoping.
    let target = mochi_testbed::Rect::from_size(
        work_area.left + 64,
        work_area.top + 64,
        work_area.width() / 3,
        work_area.height() / 3,
    );
    mochi_testbed::set_rect(hwnd, target).expect("set_rect");
    let settled = batch
        .wait_for_rect(hwnd, |rect| rect == target, DEADLINE)
        .expect("the window takes the rect it was given");
    assert_eq!(settled, target);

    // The perceived frame sits inside that rect, never outside it.
    let frame = mochi_testbed::frame_bounds(hwnd).expect("extended frame bounds");
    assert!(
        target.contains(&frame),
        "frame {frame} escaped rect {target}"
    );

    mochi_testbed::rename_window(hwnd, "MochiTest renamed").expect("rename");
    let renamed = mochi_testbed::window_info(hwnd).expect("window info");
    assert_eq!(renamed.title, "MochiTest renamed");

    mochi_testbed::minimize_window(hwnd).expect("minimize");
    let minimized = wait_until(DEADLINE, || {
        mochi_testbed::window_info(hwnd).is_ok_and(|window| window.minimized)
    });
    assert!(minimized, "the window never reported itself as minimized");

    mochi_testbed::restore_window(hwnd).expect("restore");
    let restored = wait_until(DEADLINE, || {
        mochi_testbed::window_info(hwnd).is_ok_and(|window| !window.minimized)
    });
    assert!(restored, "the window never came back from minimized");

    // Dropping the batch has to close the window, which is what keeps a failing
    // test from leaving windows on the desktop.
    drop(batch);
    assert!(
        wait_until(DEADLINE, || !mochi_testbed::window_exists(hwnd)),
        "dropping the handle left 0x{hwnd:x} open"
    );
}

fn wait_until(timeout: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}
