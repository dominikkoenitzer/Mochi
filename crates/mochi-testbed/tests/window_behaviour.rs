//! The behaviours a tiling test leans on, against real windows on the real
//! desktop: that a spawned batch has already settled, that an explicit
//! placement is taken literally, that a minimum size is defended against the
//! call a window manager actually makes, and that the states a manager drives
//! from the outside flip and flip back.
//!
//! Everything here is a `WS_EX_TOOLWINDOW` window of the class
//! `MochiTestWindow`, which no window manager picks up by itself and which is
//! closed again before the test returns, batch drop included.

#![cfg(windows)]

use std::time::Duration;

use mochi_testbed::{Error, Rect, SpawnOptions, TestWindows};

/// Long enough for a loaded machine, short enough that a hang is not a hang.
const DEADLINE: Duration = Duration::from_secs(10);

fn desktop_available() -> bool {
    let available = mochi_testbed::monitors().is_ok_and(|monitors| !monitors.is_empty());
    if !available {
        eprintln!("skipped: this session has no monitors");
    }
    available
}

fn wait_until(condition: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + DEADLINE;
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

#[test]
fn spawn_returns_windows_that_have_already_settled() {
    if !desktop_available() {
        return;
    }

    let batch = TestWindows::spawn(2, 0).expect("two test windows");

    // No wait here on purpose: whatever spawn hands back has to be true the
    // moment it returns, or every layout assertion after it is a race.
    for window in batch.windows() {
        assert!(window.visible, "0x{:x} is not visible yet", window.hwnd);
        assert!(!window.minimized);
        assert!(!window.frame.is_empty(), "0x{:x} has no frame", window.hwnd);

        // Stable, not just present: the same frame again a poll later.
        std::thread::sleep(Duration::from_millis(60));
        let again = mochi_testbed::frame_bounds(window.hwnd).expect("frame bounds");
        assert_eq!(
            again, window.frame,
            "0x{:x} was still moving when spawn returned",
            window.hwnd
        );
    }
}

#[test]
fn an_explicit_placement_is_taken_literally() {
    if !desktop_available() {
        return;
    }

    let work = mochi_testbed::monitor_at(0).expect("monitor 0").work_area;
    let wanted = Rect::from_size(work.left + 120, work.top + 96, 640, 460);
    let batch = TestWindows::spawn_with(&SpawnOptions {
        count: 1,
        position: Some((wanted.left, wanted.top)),
        size: Some((wanted.width(), wanted.height())),
        ..SpawnOptions::new(1, 0)
    })
    .expect("one placed test window");
    let hwnd = batch.handles()[0];

    let rect = mochi_testbed::window_rect(hwnd).expect("window rect");
    assert_eq!(
        rect, wanted,
        "the window did not land where it was asked to"
    );

    // The perceived frame sits inside that rect, off by no more than the
    // invisible resize border a tiling manager has to compensate for.
    let window = mochi_testbed::window_info(hwnd).expect("window info");
    assert!(
        rect.contains(&window.frame),
        "frame {} escaped rect {rect}",
        window.frame
    );
    let (left, top, right, bottom) = window.border();
    for inset in [left, top, right, bottom] {
        assert!(
            (0..=20).contains(&inset),
            "an inset of {inset} px is not an invisible border: rect {rect}, frame {}",
            window.frame
        );
    }
}

#[test]
fn a_minimum_size_is_defended_against_the_call_a_manager_makes() {
    if !desktop_available() {
        return;
    }

    let work = mochi_testbed::monitor_at(0).expect("monitor 0").work_area;
    let min = (520, 400);
    let batch = TestWindows::spawn_with(&SpawnOptions {
        position: Some((work.left + 64, work.top + 64)),
        size: Some((700, 560)),
        min_size: Some(min),
        ..SpawnOptions::new(1, 0)
    })
    .expect("one test window with a minimum size");
    let hwnd = batch.handles()[0];

    // `SetWindowPos` goes straight past WM_GETMINMAXINFO, so this is the call
    // that tells whether the window really defends its minimum.
    let squeezed = Rect::from_size(work.left + 64, work.top + 64, 200, 150);
    mochi_testbed::set_rect(hwnd, squeezed).expect("set_rect");

    let settled = batch
        .wait_for_rect(hwnd, |rect| rect.width() >= min.0, DEADLINE)
        .expect("the window refuses to go below its minimum width");
    assert!(
        settled.width() >= min.0 && settled.height() >= min.1,
        "{settled} is smaller than the minimum {min:?}"
    );
    assert_eq!(
        (settled.left, settled.top),
        (squeezed.left, squeezed.top),
        "the move was refused along with the resize"
    );

    // Without a minimum a layout may make a test window as small as it likes.
    let free = TestWindows::spawn_with(&SpawnOptions {
        position: Some((work.left + 64, work.top + 64)),
        size: Some((700, 560)),
        ..SpawnOptions::new(1, 0)
    })
    .expect("one plain test window");
    let plain = free.handles()[0];
    mochi_testbed::set_rect(plain, squeezed).expect("set_rect");
    let small = free
        .wait_for_rect(plain, |rect| rect.width() <= squeezed.width(), DEADLINE)
        .expect("a window without a minimum takes the rect it is given");
    assert!(small.width() <= squeezed.width(), "{small}");
}

#[test]
fn an_owned_window_reports_its_owner_and_the_owner_is_never_listed() {
    if !desktop_available() {
        return;
    }

    let batch = TestWindows::spawn_with(&SpawnOptions {
        owned: true,
        ..SpawnOptions::new(1, 0)
    })
    .expect("one owned test window");
    let hwnd = batch.handles()[0];

    let owner = mochi_testbed::window_owner(hwnd).expect("an owned popup has an owner");
    assert_ne!(owner, 0);
    assert_ne!(owner, hwnd);

    let listed = mochi_testbed::list_windows().expect("enumerate test windows");
    let mine = listed
        .iter()
        .find(|window| window.hwnd == hwnd)
        .expect("the owned window is listed");
    assert_eq!(mine.owner, Some(owner));
    assert!(
        !listed.iter().any(|window| window.hwnd == owner),
        "the hidden owner showed up in the listing"
    );
}

#[test]
fn a_window_spawned_without_a_title_has_an_empty_one() {
    if !desktop_available() {
        return;
    }

    let batch = TestWindows::spawn_with(&SpawnOptions {
        no_title: true,
        ..SpawnOptions::new(1, 0)
    })
    .expect("one untitled test window");
    let hwnd = batch.handles()[0];

    assert_eq!(mochi_testbed::window_info(hwnd).expect("info").title, "");
    let listed = mochi_testbed::list_windows().expect("enumerate test windows");
    let mine = listed
        .iter()
        .find(|window| window.hwnd == hwnd)
        .expect("an untitled window is still listed");
    assert!(mine.title.is_empty(), "{:?}", mine.title);
}

#[test]
fn cloaking_and_alpha_flip_and_flip_back() {
    if !desktop_available() {
        return;
    }

    let batch = TestWindows::spawn(1, 0).expect("one test window");
    let hwnd = batch.handles()[0];
    assert!(!mochi_testbed::cloaked(hwnd));
    assert_eq!(mochi_testbed::window_alpha(hwnd), None);

    // Cloaking is how a manager hides a workspace: the window stays visible to
    // `IsWindowVisible` and disappears from the screen.
    mochi_testbed::set_cloaked(hwnd, true).expect("cloak");
    assert!(
        wait_until(|| mochi_testbed::window_info(hwnd).is_ok_and(|w| w.cloaked)),
        "the window never reported itself as cloaked"
    );
    mochi_testbed::set_cloaked(hwnd, false).expect("uncloak");
    assert!(
        wait_until(|| mochi_testbed::window_info(hwnd).is_ok_and(|w| !w.cloaked)),
        "the window stayed cloaked"
    );

    mochi_testbed::set_window_alpha(hwnd, Some(128)).expect("alpha");
    let faded = mochi_testbed::window_info(hwnd).expect("info");
    assert_eq!(faded.alpha, Some(128));
    assert!(faded.is_layered(), "the layered bit did not come with it");

    mochi_testbed::set_window_alpha(hwnd, None).expect("clear the alpha");
    let opaque = mochi_testbed::window_info(hwnd).expect("info");
    assert_eq!(opaque.alpha, None);
    assert!(!opaque.is_layered(), "the layered bit outlived the alpha");
}

#[test]
fn focus_moves_the_foreground_flag_between_two_windows() {
    if !desktop_available() {
        return;
    }

    let batch = TestWindows::spawn(2, 0).expect("two test windows");
    let (first, second) = (batch.handles()[0], batch.handles()[1]);

    // Windows refuses a foreground change from a process that does not own the
    // foreground lock, which is the normal case for a test runner. The library
    // says so rather than pretending, and there is nothing to assert then.
    if !mochi_testbed::focus_window(first).expect("focus the first window") {
        eprintln!("skipped: this process may not take the foreground");
        return;
    }
    assert!(
        wait_until(|| mochi_testbed::window_info(first).is_ok_and(|w| w.foreground)),
        "the focused window never reported itself as foreground"
    );

    if mochi_testbed::focus_window(second).expect("focus the second window") {
        assert!(
            wait_until(|| mochi_testbed::window_info(first).is_ok_and(|w| !w.foreground)),
            "two windows were in the foreground at once"
        );
        assert!(
            mochi_testbed::window_info(second).expect("info").foreground,
            "the foreground did not move to the second window"
        );
    }
}

#[test]
fn a_wait_that_never_comes_true_times_out_with_a_useful_error() {
    if !desktop_available() {
        return;
    }

    let batch = TestWindows::spawn(1, 0).expect("one test window");
    let hwnd = batch.handles()[0];

    let started = std::time::Instant::now();
    let error = batch
        .wait_for_rect(hwnd, |_| false, Duration::from_millis(250))
        .expect_err("a predicate that is never true has to time out");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the timeout was ignored"
    );
    assert!(matches!(error, Error::Timeout(_)), "{error:?}");

    let message = error.to_string();
    // The message has to name the window and what was last seen, or a failing
    // layout test says nothing about why it failed.
    assert!(message.contains("timed out waiting for"), "{message}");
    assert!(message.contains(&format!("0x{hwnd:x}")), "{message}");
    assert!(message.contains("last seen"), "{message}");

    // The window is untouched by a wait that gave up.
    assert!(mochi_testbed::window_exists(hwnd));
}

#[test]
fn a_window_with_a_menu_bar_spawns_like_any_other_and_starts_outside_menu_mode() {
    if !desktop_available() {
        return;
    }

    let batch = TestWindows::spawn_with(&SpawnOptions {
        count: 1,
        menu_bar: true,
        ..SpawnOptions::new(1, 0)
    })
    .expect("one test window with a menu bar");
    let hwnd = batch.handles()[0];

    let window = batch.windows().pop().expect("the window is still there");
    assert!(window.visible, "0x{hwnd:x} is not visible");
    assert!(!window.frame.is_empty(), "0x{hwnd:x} has no frame");

    // Nothing has pressed a key, so the menu that is now hanging on this
    // window has not been entered. What a bare Alt does to it is measured
    // end to end in `crates/mochi/tests/e2e_testbed.rs`, where there is a
    // foreground window to press it against.
    assert!(
        !mochi_testbed::in_menu_mode(hwnd),
        "0x{hwnd:x} claims to be in a menu before anything touched it"
    );
    assert!(
        !mochi_testbed::in_menu_mode(0),
        "a handle that is not a window cannot be in a menu"
    );
}
