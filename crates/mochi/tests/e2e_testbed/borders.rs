//! Border frames, and the windows that take them away.
//!
//! Mochi draws a border as a top-level window of its own, so whether one is
//! still on screen after the window it framed has closed, hidden, minimised,
//! gone fullscreen or died with its process is a question about real windows,
//! read straight off the desktop. A border with nothing under it is a pink
//! rectangle over whatever the user looks at next.

use std::io::{BufRead, BufReader};
use std::sync::mpsc;

use super::*;

/// The class of a border frame, `mochi-render`'s `BorderWindow`.
const BORDER_CLASS: &str = "MochiBorder";
/// How far a border may sit from the frame it surrounds on any edge: its
/// width, its offset and the margin it keeps for anti-aliasing, with room.
pub(super) const BORDER_SLACK: i32 = 24;

// ---------------------------------------------------------------------------
// configuration
// ---------------------------------------------------------------------------

/// The visual settings of the desk: borders on every window, the focused one
/// pink, animations on.
pub(super) fn rice() -> Value {
    serde_json::json!({
        "$schema": "https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json",
        "window_hiding_behaviour": "Cloak",
        "cross_monitor_move_behaviour": "Insert",
        "mouse_follows_focus": false,
        "default_workspace_padding": 14,
        "default_container_padding": 10,
        "border": true,
        "border_width": 6,
        "border_offset": -1,
        "border_style": "Rounded",
        "border_colours": {
            "single": "#ffbbdf",
            "stack": "#ffbbdf",
            "monocle": "#ffbbdf",
            "floating": "#ffbbdf",
            "unfocused": "#313244"
        },
        "transparency": false,
        "animation": { "enabled": true, "duration": 250, "style": "EaseOutQuad", "fps": 60 }
    })
}

/// Writes a JSON document next to the logs and hands back its path.
pub(super) fn write_json(name: &str, value: &Value) -> PathBuf {
    let path = temp_dir().join(name);
    std::fs::write(&path, serde_json::to_string_pretty(value).expect("JSON"))
        .expect("could not write a test file");
    path
}

// ---------------------------------------------------------------------------
// reading the desktop
// ---------------------------------------------------------------------------

/// The border frames this daemon has on screen.
pub(super) fn borders(daemon: u32) -> Vec<Rect> {
    mochi_testbed::windows_of_class(BORDER_CLASS)
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.pid == daemon && b.visible)
        .map(|b| b.rect)
        .collect()
}

/// Every test window on screen, whichever process owns it.
pub(super) fn shown() -> Vec<TestWindowInfo> {
    all_infos()
        .into_iter()
        .filter(|w| w.visible && !w.cloaked && !w.minimized)
        .collect()
}

/// Whether `border` is drawn around `frame`.
fn frames(border: Rect, frame: Rect) -> bool {
    close_enough(border, frame, BORDER_SLACK)
}

/// Every window in `framed` has exactly one border around it, and every border
/// on screen is around one of them.
///
/// The second half is the one that matters on a real desk: a border with no
/// window under it is a pink rectangle floating over whatever the user is
/// looking at, and nothing but a restart of the daemon takes it away.
fn borders_match(daemon: u32, framed: &[TestWindowInfo]) -> Result<(), String> {
    let drawn = borders(daemon);
    for border in &drawn {
        if !framed.iter().any(|w| frames(*border, w.frame)) {
            return Err(format!(
                "a border at {border} frames no window on screen; windows: {:?}",
                framed.iter().map(|w| w.frame).collect::<Vec<_>>()
            ));
        }
    }
    for window in framed {
        let around = drawn.iter().filter(|b| frames(**b, window.frame)).count();
        if around != 1 {
            return Err(format!(
                "{} at {} has {around} borders around it; borders: {drawn:?}",
                window.hwnd_hex(),
                window.frame
            ));
        }
    }
    Ok(())
}

/// Waits for [`borders_match`] over every test window on screen except
/// `unframed`, and reports the last mismatch when it never does.
pub(super) fn wait_for_borders(daemon: u32, unframed: &[i64]) -> Result<(), String> {
    let framed = || -> Vec<TestWindowInfo> {
        shown()
            .into_iter()
            .filter(|w| !unframed.contains(&w.hwnd))
            .collect()
    };
    if wait_for(STEP, || borders_match(daemon, &framed()).is_ok()).is_ok() {
        return Ok(());
    }
    borders_match(daemon, &framed())
}

/// No border sits where `frame` was.
pub(super) fn no_border_at(daemon: u32, frame: Rect) -> Result<(), String> {
    wait_for(STEP, || !borders(daemon).iter().any(|b| frames(*b, frame))).map_err(|_| {
        format!(
            "a border is still drawn at {frame}, where the window was: {:?}",
            borders(daemon)
        )
    })
}

// ---------------------------------------------------------------------------
// windows of other processes
// ---------------------------------------------------------------------------

/// `mochi-testwin`, built next to the daemon by
/// `cargo build -p mochi-testbed --bin mochi-testwin`.
pub(super) fn testwin_binary() -> Result<PathBuf, String> {
    let path = daemon_binary()
        .ok_or("no mochi binary")?
        .with_file_name("mochi-testwin.exe");
    if path.exists() {
        Ok(path)
    } else {
        Err(format!(
            "{} is missing; build it with cargo build -p mochi-testbed --bin mochi-testwin",
            path.display()
        ))
    }
}

/// A test window that lives in a `mochi-testwin` process of its own, so that
/// killing the process kills only it.
struct Foreign {
    child: Child,
    hwnd: i64,
}

impl Foreign {
    fn spawn(prefix: &str) -> Result<Foreign, String> {
        let mut child = OsCommand::new(testwin_binary()?)
            .args(["spawn", "--count", "1", "--title-prefix", prefix])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start mochi-testwin: {e}"))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, rx) = mpsc::channel();
        // Read to the end, not just to the ready line: the host prints an
        // event for every move, and a full pipe would stall its windows.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str::<Value>(&line)
                    && event["host"] == "ready"
                {
                    let _ = tx.send(event["windows"][0].as_i64());
                }
            }
        });
        let hwnd = rx.recv_timeout(Duration::from_secs(20)).ok().flatten();
        match hwnd {
            Some(hwnd) => Ok(Foreign { child, hwnd }),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err("mochi-testwin never reported its window".to_owned())
            }
        }
    }

    /// Terminates the process, as Task Manager or a crash does.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Foreign {
    fn drop(&mut self) {
        self.kill();
    }
}

// ---------------------------------------------------------------------------
// the tests
// ---------------------------------------------------------------------------

#[test]
fn a_window_that_goes_away_takes_its_border_with_it() {
    skip_unless_allowed!("a_window_that_goes_away_takes_its_border_with_it");

    let config = write_json("mochi-borders.json", &rice());
    let mut daemon = Daemon::start_with_config("borders", &config, &["--no-hotkeys"]);
    let log = daemon.log();
    let pid = daemon.child.id();
    let windows = TestWindows::spawn(4, 0).expect("could not spawn the test windows");
    let handles = windows.handles();

    let mut steps = Steps::default();

    steps.step(
        "every tiled window has a border and every border a window",
        || {
            wait_for(Duration::from_secs(10), || managed_count() == 4)
                .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
            wait_for_tiling(&windows, 4)?;
            wait_for_borders(pid, &[])
        },
    );

    steps.step("a minimised window takes its border down", || {
        let hwnd = handles[1];
        let frame = frame_of(&windows, hwnd).ok_or("no frame")?;
        mochi_testbed::minimize_window(hwnd).map_err(|e| e.to_string())?;
        wait_for_tiling(&windows, 3)?;
        no_border_at(pid, frame)?;
        wait_for_borders(pid, &[])
    });

    steps.step(
        "a restored window gets its tile and its border back",
        || {
            mochi_testbed::restore_window(handles[1]).map_err(|e| e.to_string())?;
            wait_for(Duration::from_secs(5), || managed_count() == 4)
                .map_err(|_| format!("state shows {} windows", managed_count()))?;
            let frames = wait_for_tiling(&windows, 4)?;
            layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
            wait_for_borders(pid, &[])
        },
    );

    steps.step("a window that hides itself takes its border down", || {
        let hwnd = handles[2];
        let frame = frame_of(&windows, hwnd).ok_or("no frame")?;
        mochi_testbed::hide_window(hwnd).map_err(|e| e.to_string())?;
        wait_for_tiling(&windows, 3)?;
        no_border_at(pid, frame)?;
        wait_for_borders(pid, &[])
    });

    steps.step(
        "a window that shows itself again is tiled with a border",
        || {
            mochi_testbed::show_window(handles[2]).map_err(|e| e.to_string())?;
            wait_for(Duration::from_secs(5), || managed_count() == 4)
                .map_err(|_| format!("state shows {} windows", managed_count()))?;
            wait_for_tiling(&windows, 4)?;
            wait_for_borders(pid, &[])
        },
    );

    steps.step("a fullscreen window carries no border", || {
        let hwnd = handles[0];
        let screen = mochi_testbed::monitor_at(0)
            .map_err(|e| e.to_string())?
            .rect;
        let style = mochi_testbed::window_info(hwnd)
            .map_err(|e| e.to_string())?
            .style;
        mochi_testbed::set_style(hwnd, style & !WS_CAPTION).map_err(|e| e.to_string())?;
        mochi_testbed::set_rect(hwnd, screen).map_err(|e| e.to_string())?;
        let held = wait_for(STEP, || fullscreen_monitor_of(hwnd).is_some())
            .map_err(|_| "the daemon never saw the window go fullscreen".to_owned());
        let framed = held.and_then(|()| {
            let frame = mochi_testbed::frame_bounds(hwnd).map_err(|e| e.to_string())?;
            no_border_at(pid, frame)
        });
        // Back into its tile whatever happened, so the steps after this one
        // start from four tiled windows.
        let _ = mochi_testbed::set_style(hwnd, style);
        let _ = mochi_testbed::set_rect(
            hwnd,
            Rect::new(
                screen.left + 80,
                screen.top + 80,
                screen.left + 600,
                screen.top + 400,
            ),
        );
        let _ = wait_for(STEP, || fullscreen_monitor_of(hwnd).is_none());
        framed?;
        wait_for_tiling(&windows, 4)?;
        wait_for_borders(pid, &[])
    });

    steps.step("a closed window leaves no border behind", || {
        let hwnd = handles[3];
        let frame = frame_of(&windows, hwnd).ok_or("no frame")?;
        mochi_testbed::close_window(hwnd).map_err(|e| e.to_string())?;
        mochi_testbed::wait_until_gone(&[hwnd], STEP).map_err(|e| e.to_string())?;
        wait_for(STEP, || managed_count() == 3)
            .map_err(|_| format!("state shows {} windows", managed_count()))?;
        wait_for_tiling(&windows, 3)?;
        no_border_at(pid, frame)?;
        wait_for_borders(pid, &[])
    });

    steps.step("stop takes every border down", || {
        daemon.stop();
        wait_for(STEP, || borders(pid).is_empty())
            .map_err(|_| format!("{} borders outlived the daemon", borders(pid).len()))
    });

    drop(windows);
    steps.finish(&log);
}

#[test]
fn a_killed_process_takes_its_border_with_it() {
    skip_unless_allowed!("a_killed_process_takes_its_border_with_it");

    let config = write_json("mochi-kill.json", &rice());
    let daemon = Daemon::start_with_config("kill", &config, &["--no-hotkeys"]);
    let log = daemon.log();
    let pid = daemon.child.id();
    let windows = TestWindows::spawn(2, 0).expect("could not spawn the test windows");
    let mut victim = Foreign::spawn("Victim").expect("could not start the window to kill");

    let mut steps = Steps::default();

    steps.step("the daemon tiles both processes' windows", || {
        wait_for(Duration::from_secs(10), || managed_count() == 3)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        wait_for(STEP, || shown().len() == 3).map_err(|e| e.to_string())?;
        wait_until_still(&windows);
        wait_for_borders(pid, &[])
    });

    let frame = mochi_testbed::frame_bounds(victim.hwnd).ok();

    steps.step("the killed window leaves the model", || {
        victim.kill();
        mochi_testbed::wait_until_gone(&[victim.hwnd], STEP).map_err(|e| e.to_string())?;
        wait_for(STEP, || managed_count() == 2)
            .map_err(|_| format!("state still shows {} windows", managed_count()))
    });

    steps.step("the killed window leaves no border behind", || {
        if let Some(frame) = frame {
            no_border_at(pid, frame)?;
        }
        wait_for_borders(pid, &[])
    });

    steps.step("the two left fill the screen again", || {
        let frames = wait_for_tiling(&windows, 2)?;
        let area = area().ok_or("no state to read the work area from")?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
        check(
            close_enough(bounds(&frames), area.tiled, area.seam + 4),
            format!(
                "the tiles span {} but the padded work area is {}",
                bounds(&frames),
                area.tiled
            ),
        )
    });

    drop(victim);
    drop(windows);
    drop(daemon);
    steps.finish(&log);
}

#[test]
fn windows_opened_and_closed_in_quick_succession_leave_nothing_behind() {
    skip_unless_allowed!("windows_opened_and_closed_in_quick_succession_leave_nothing_behind");

    let config = write_json("mochi-churn.json", &rice());
    let daemon = Daemon::start_with_config("churn", &config, &["--no-hotkeys"]);
    let log = daemon.log();
    let pid = daemon.child.id();
    let stay = TestWindows::spawn(2, 0).expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("two windows are tiled to start with", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        wait_for_tiling(&stay, 2)?;
        wait_for_borders(pid, &[])
    });

    let before = visible_frames(&stay);

    steps.step("twenty windows come and go", || {
        for round in 0..10 {
            let mut burst = TestWindows::spawn(2, 0).map_err(|e| e.to_string())?;
            // Half of them close before the daemon can have tiled them, half
            // once it has had the time to.
            if round % 2 == 1 {
                std::thread::sleep(Duration::from_millis(150));
            }
            burst.close_all().map_err(|e| e.to_string())?;
        }
        check(
            mochi_client::is_running(),
            "the daemon stopped answering".to_owned(),
        )
    });

    steps.step("the model holds the two that stayed", || {
        wait_for(Duration::from_secs(5), || managed_count() == 2)
            .map_err(|_| format!("state shows {} windows", managed_count()))
    });

    steps.step("the two that stayed are back in their tiles", || {
        let after = wait_for_tiling(&stay, 2)?;
        check(
            after.len() == before.len()
                && after
                    .iter()
                    .zip(&before)
                    .all(|(a, b)| close_enough(*a, *b, 4)),
            format!("the layout moved from {before:?} to {after:?}"),
        )
    });

    steps.step("no border is left from the windows that went", || {
        wait_for_borders(pid, &[])
    });

    drop(stay);
    drop(daemon);
    steps.finish(&log);
}
