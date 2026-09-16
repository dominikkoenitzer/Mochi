//! End-to-end proof against real windows.
//!
//! These tests start the real daemon against the real desktop, so they need an
//! interactive window station and they refuse to run without one. They are
//! opt-in: set `MOCHI_E2E=1`.
//!
//! ```text
//! set MOCHI_E2E=1
//! cargo test -p mochi --test e2e_testbed -- --test-threads 1
//! ```
//!
//! Safety: the daemon is always started with `--manage-class MochiTestWindow`,
//! so it manages exactly the windows this test spawned and leaves every other
//! window on the desktop alone.

#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Child, Command as OsCommand, Stdio};
use std::time::{Duration, Instant};

use mochi_client::{
    Axis, Command, CycleDirection, Direction, Layout, Response, Sizing, send, send_to,
};
use mochi_testbed::{Rect, TestWindowInfo, TestWindows, layout_assert};
use serde_json::Value;

/// The longest any single step waits for the desktop to settle.
const STEP: Duration = Duration::from_secs(3);
/// One poll tick. Nothing in here sleeps blind for longer.
const TICK: Duration = Duration::from_millis(40);

// ---------------------------------------------------------------------------
// gate
// ---------------------------------------------------------------------------

/// True when this process may take over real windows.
///
/// Two conditions: the opt-in variable, and a window station that actually has
/// a desktop. On a CI runner without an interactive session the second check
/// fails even if someone sets the variable.
fn allowed() -> Option<String> {
    if std::env::var("MOCHI_E2E").ok().as_deref() != Some("1") {
        return Some("MOCHI_E2E is not 1".to_owned());
    }
    if !has_interactive_desktop() {
        return Some("no interactive desktop in this window station".to_owned());
    }
    if daemon_binary().is_none() {
        return Some("the mochi binary was not built".to_owned());
    }
    None
}

/// Whether this process is attached to a desktop that can own windows.
fn has_interactive_desktop() -> bool {
    // A session without a desktop enumerates no monitor, which is also exactly
    // the condition that makes spawning a window pointless.
    mochi_testbed::monitors().is_ok_and(|m| !m.is_empty())
}

/// The daemon `cargo` built for this test run.
fn daemon_binary() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_mochi"));
    path.exists().then_some(path)
}

/// Prints the skip reason and returns true when the test must not run.
macro_rules! skip_unless_allowed {
    ($name:expr) => {
        if let Some(reason) = allowed() {
            eprintln!("skipping {}: {reason}", $name);
            eprintln!("  run it with MOCHI_E2E=1 on an interactive Windows desktop");
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// the daemon under test
// ---------------------------------------------------------------------------

/// A running `mochi`, stopped and reaped on drop whatever the test did.
struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Daemon {
    /// Starts the daemon against the test config, managing only the testbed
    /// class, with `RUST_LOG=debug` going to a log file under `%TEMP%`.
    fn start(tag: &str) -> Daemon {
        // A daemon left over from an earlier run would take the single
        // instance mutex and the new one would exit at once.
        let _ = send(&Command::Stop { whkd: false });
        wait_for(STEP, || !mochi_client::is_running()).expect("an old daemon refused to stop");

        let dir = temp_dir();
        let log = dir.join(format!("mochi-{tag}.log"));
        let file = std::fs::File::create(&log).expect("could not create the daemon log");
        let errors = file.try_clone().expect("could not clone the log handle");

        let child = OsCommand::new(daemon_binary().expect("no mochi binary"))
            .arg(mochi_testbed::DAEMON_MANAGE_FLAG)
            .arg(mochi_testbed::TEST_WINDOW_CLASS)
            .arg("--config")
            .arg(config_path())
            .env("RUST_LOG", "debug")
            .stdin(Stdio::null())
            .stdout(Stdio::from(file))
            .stderr(Stdio::from(errors))
            .spawn()
            .expect("could not start the daemon");

        let daemon = Daemon { child, log };
        wait_for(Duration::from_secs(10), mochi_client::is_running)
            .unwrap_or_else(|_| panic!("the daemon never opened its pipe; log: {}", daemon.log()));
        daemon
    }

    /// The log file, for a failure message.
    fn log(&self) -> String {
        self.log.display().to_string()
    }

    /// Asks the daemon to stop and waits for the pipe to go away.
    fn stop(&mut self) {
        if mochi_client::is_running() {
            let _ = send(&Command::Stop { whkd: false });
        }
        let _ = wait_for(Duration::from_secs(10), || !mochi_client::is_running());
        let _ = self.child.wait();
    }

    /// Terminates the process the way a crash would, with no restore hook.
    fn hard_kill(&mut self) {
        let _ = OsCommand::new("taskkill")
            .args(["/F", "/PID", &self.child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.wait();
        let _ = wait_for(STEP, || !mochi_client::is_running());
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
        let _ = self.child.kill();
    }
}

/// Where the scratch config and the logs live. Never inside the repository.
fn temp_dir() -> PathBuf {
    let dir =
        PathBuf::from(std::env::var("TEMP").unwrap_or_else(|_| ".".to_owned())).join("mochi-e2e");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The config the daemon runs with. `MOCHI_E2E_CONFIG` overrides it; otherwise
/// a minimal one is written next to the logs, so the test is self contained.
fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var("MOCHI_E2E_CONFIG") {
        let path = PathBuf::from(path);
        if path.exists() {
            return path;
        }
    }
    let path = temp_dir().join("mochi.json");
    if !path.exists() {
        std::fs::write(
            &path,
            concat!(
                "{\n",
                "  \"$schema\": \"https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json\",\n",
                "  \"window_hiding_behaviour\": \"Cloak\",\n",
                "  \"default_workspace_padding\": 14,\n",
                "  \"default_container_padding\": 10\n",
                "}\n"
            ),
        )
        .expect("could not write the fallback config");
    }
    path
}

// ---------------------------------------------------------------------------
// polling
// ---------------------------------------------------------------------------

/// Polls `check` until it is true or the deadline passes.
fn wait_for(deadline: Duration, mut check: impl FnMut() -> bool) -> Result<(), String> {
    let end = Instant::now() + deadline;
    loop {
        if check() {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!("still false after {deadline:?}"));
        }
        std::thread::sleep(TICK);
    }
}

/// Polls `read` until it returns `Some`, then hands the value back.
fn wait_some<T>(deadline: Duration, mut read: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + deadline;
    loop {
        if let Some(value) = read() {
            return Some(value);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(TICK);
    }
}

// ---------------------------------------------------------------------------
// talking to the daemon
// ---------------------------------------------------------------------------

/// Sends a command and fails the step when the daemon did not accept it.
fn command(cmd: &Command) -> Result<(), String> {
    match send_to(mochi_client::PIPE_NAME, cmd) {
        Ok(Response::Error { message }) => Err(format!("{cmd:?} was refused: {message}")),
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{cmd:?} could not be sent: {e}")),
    }
}

/// The whole daemon state, the document `mochic state` prints.
fn state() -> Option<Value> {
    match send(&Command::State) {
        Ok(Response::State { state }) => Some(state),
        _ => None,
    }
}

/// How many windows the model holds.
fn managed_count() -> usize {
    state()
        .and_then(|s| s["window_count"].as_u64())
        .unwrap_or(0) as usize
}

/// The handle the model considers focused, as the testbed spells handles.
fn focused_window() -> Option<i64> {
    state().and_then(|s| s["focused_window"].as_i64())
}

// ---------------------------------------------------------------------------
// the windows
// ---------------------------------------------------------------------------

/// The live info of the batch, in spawn order.
fn infos(windows: &TestWindows) -> Vec<TestWindowInfo> {
    windows.windows()
}

/// The extended frame bounds of every window of the batch that is on screen.
fn visible_frames(windows: &TestWindows) -> Vec<Rect> {
    infos(windows)
        .iter()
        .filter(|w| w.visible && !w.cloaked && !w.minimized)
        .map(|w| w.frame)
        .collect()
}

/// The frame of one handle.
fn frame_of(windows: &TestWindows, hwnd: i64) -> Option<Rect> {
    infos(windows)
        .into_iter()
        .find(|w| w.hwnd == hwnd)
        .map(|w| w.frame)
}

/// Waits until the frames of the batch stop changing, so an assertion never
/// reads a layout halfway through.
fn wait_until_still(windows: &TestWindows) {
    let mut last = visible_frames(windows);
    let end = Instant::now() + STEP;
    let mut stable = 0;
    while Instant::now() < end {
        std::thread::sleep(TICK);
        let now = visible_frames(windows);
        if now == last {
            stable += 1;
            if stable >= 3 {
                return;
            }
        } else {
            stable = 0;
            last = now;
        }
    }
}

/// Waits until the batch has `count` windows on screen and their frames rest.
fn wait_for_tiling(windows: &TestWindows, count: usize) -> Result<Vec<Rect>, String> {
    wait_for(STEP, || visible_frames(windows).len() == count).map_err(|_| {
        format!(
            "expected {count} windows on screen, saw {}",
            visible_frames(windows).len()
        )
    })?;
    wait_until_still(windows);
    Ok(visible_frames(windows))
}

// ---------------------------------------------------------------------------
// the step log
// ---------------------------------------------------------------------------

/// Records the outcome of every step so one run reports on all of them rather
/// than stopping at the first surprise.
#[derive(Default)]
struct Steps {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

impl Steps {
    fn step(&mut self, name: &str, body: impl FnOnce() -> Result<(), String>) {
        match body() {
            Ok(()) => {
                eprintln!("  ok    {name}");
                self.passed.push(name.to_owned());
            }
            Err(e) => {
                eprintln!("  FAIL  {name}: {e}");
                self.failed.push((name.to_owned(), e));
            }
        }
    }

    fn finish(self, log: &str) {
        eprintln!(
            "{} steps passed, {} failed",
            self.passed.len(),
            self.failed.len()
        );
        assert!(
            self.failed.is_empty(),
            "{} of {} steps failed against real windows (daemon log: {log})\n{}",
            self.failed.len(),
            self.passed.len() + self.failed.len(),
            self.failed
                .iter()
                .map(|(n, e)| format!("  {n}: {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// Fails unless `condition` holds.
fn check(condition: bool, message: impl Into<String>) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

// ---------------------------------------------------------------------------
// the geometry of a tiled workspace
// ---------------------------------------------------------------------------

/// What the daemon says the focused workspace covers, paddings included.
struct Area {
    /// The work area of the workspace, physical pixels.
    work_area: Rect,
    /// The rectangle the tiles have to fill: work area minus workspace padding.
    tiled: Rect,
    /// The seam a container padding leaves between two tiles.
    seam: i32,
}

/// Reads the focused workspace geometry out of the state document.
fn area() -> Option<Area> {
    let state = state()?;
    let monitor = state["monitors"]
        .as_array()?
        .get(state["focused_monitor"].as_u64().unwrap_or(0) as usize)?
        .clone();
    let scale = monitor["scale"].as_f64().unwrap_or(1.0);
    let workspace = monitor["workspaces"]
        .as_array()?
        .get(monitor["focused_workspace"].as_u64().unwrap_or(0) as usize)?
        .clone();
    let work_area = rect(&workspace["work_area"])?;
    let pad = (workspace["workspace_padding"].as_i64().unwrap_or(0) as f64 * scale).round() as i32;
    let container =
        (workspace["container_padding"].as_i64().unwrap_or(0) as f64 * scale).round() as i32;
    Some(Area {
        work_area,
        tiled: Rect::new(
            work_area.left + pad,
            work_area.top + pad,
            work_area.right - pad,
            work_area.bottom - pad,
        ),
        seam: container.max(1) * 2,
    })
}

/// A `Rect` out of the state document.
fn rect(value: &Value) -> Option<Rect> {
    Some(Rect::new(
        value["left"].as_i64()? as i32,
        value["top"].as_i64()? as i32,
        value["right"].as_i64()? as i32,
        value["bottom"].as_i64()? as i32,
    ))
}

/// The bounding box of a set of frames.
fn bounds(frames: &[Rect]) -> Rect {
    let mut it = frames.iter();
    let first = *it.next().expect("no frames");
    it.fold(first, |acc, r| {
        Rect::new(
            acc.left.min(r.left),
            acc.top.min(r.top),
            acc.right.max(r.right),
            acc.bottom.max(r.bottom),
        )
    })
}

/// Whether two rectangles agree to within `tolerance` on every edge.
fn close_enough(a: Rect, b: Rect, tolerance: i32) -> bool {
    (a.left - b.left).abs() <= tolerance
        && (a.top - b.top).abs() <= tolerance
        && (a.right - b.right).abs() <= tolerance
        && (a.bottom - b.bottom).abs() <= tolerance
}

// ---------------------------------------------------------------------------
// test one: the whole command surface against four real windows
// ---------------------------------------------------------------------------

#[test]
fn the_daemon_tiles_and_drives_four_real_windows() {
    skip_unless_allowed!("the_daemon_tiles_and_drives_four_real_windows");

    let mut daemon = Daemon::start("tiling");
    let log = daemon.log();
    let windows = TestWindows::spawn(4, 0).expect("could not spawn the test windows");
    let handles = windows.handles();

    let mut steps = Steps::default();

    steps.step("the daemon adopts the four windows", || {
        wait_for(Duration::from_secs(10), || managed_count() == 4)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))
    });

    steps.step("the four frames tile the work area", || {
        let frames = wait_for_tiling(&windows, 4)?;
        let area = area().ok_or("no state to read the work area from")?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
        layout_assert::check_all_within(&frames, area.work_area).map_err(|v| v.to_string())?;
        let box_of = bounds(&frames);
        check(
            close_enough(box_of, area.tiled, area.seam + 4),
            format!(
                "the tiles span {box_of} but the padded work area is {}",
                area.tiled
            ),
        )?;
        layout_assert::check_covers(&frames, box_of, area.seam + 4).map_err(|v| v.to_string())
    });

    // --- focus -----------------------------------------------------------
    for direction in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        steps.step(&format!("focus {direction}"), || {
            let before = focused_window();
            command(&Command::Focus { direction })?;
            let after = wait_some(STEP, || {
                let now = focused_window();
                (now.is_some() && now != before).then_some(now)
            })
            .flatten();
            let after = after.ok_or_else(|| format!("focus never moved off {before:?}"))?;
            check(
                handles.contains(&after),
                format!("focus landed on {after:#x}, which is not a test window"),
            )?;
            let foreground = wait_some(STEP, || {
                infos(&windows)
                    .into_iter()
                    .find(|w| w.foreground)
                    .map(|w| w.hwnd)
            });
            check(
                foreground == Some(after),
                format!("the model focused {after:#x} but the foreground is {foreground:?}"),
            )
        });
    }

    // --- move ------------------------------------------------------------
    for direction in [Direction::Left, Direction::Right] {
        steps.step(&format!("move {direction} swaps two frames"), || {
            let focused = focused_window().ok_or("nothing is focused")?;
            let before = frame_of(&windows, focused).ok_or("the focused window has no frame")?;
            command(&Command::Move { direction })?;
            let moved = wait_some(STEP, || {
                frame_of(&windows, focused).filter(|now| *now != before)
            });
            let moved = moved.ok_or_else(|| format!("{focused:#x} stayed at {before}"))?;
            wait_until_still(&windows);
            let frames = visible_frames(&windows);
            check(
                frames.len() == 4,
                format!("{} windows on screen", frames.len()),
            )?;
            layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
            check(
                moved != before,
                "the focused window kept its tile".to_owned(),
            )
        });
    }

    // --- resize ----------------------------------------------------------
    steps.step("resize-axis horizontal grows then returns", || {
        let focused = focused_window().ok_or("nothing is focused")?;
        let before = frame_of(&windows, focused).ok_or("no frame")?;
        command(&Command::ResizeAxis {
            axis: Axis::Horizontal,
            sizing: Sizing::Increase,
        })?;
        let grown = wait_some(STEP, || {
            frame_of(&windows, focused).filter(|now| now.width() > before.width() + 4)
        })
        .ok_or_else(|| {
            format!(
                "width stayed at {} after increase",
                frame_of(&windows, focused).map_or(-1, |r| r.width())
            )
        })?;
        command(&Command::ResizeAxis {
            axis: Axis::Horizontal,
            sizing: Sizing::Decrease,
        })?;
        let back = wait_some(STEP, || {
            frame_of(&windows, focused).filter(|now| now.width() < grown.width() - 4)
        })
        .ok_or("width did not come back down")?;
        check(
            (back.width() - before.width()).abs() <= 8,
            format!(
                "width returned to {} instead of {}",
                back.width(),
                before.width()
            ),
        )
    });

    // --- float -----------------------------------------------------------
    steps.step("toggle-float leaves the grid and comes back", || {
        let focused = focused_window().ok_or("nothing is focused")?;
        let before = frame_of(&windows, focused).ok_or("no frame")?;
        command(&Command::ToggleFloat)?;
        wait_for(STEP, || state().is_some_and(|s| floating_count(&s) == 1))
            .map_err(|_| "the state never reported a floating window".to_owned())?;
        wait_until_still(&windows);
        let others: Vec<Rect> = infos(&windows)
            .iter()
            .filter(|w| w.hwnd != focused && !w.cloaked && w.visible)
            .map(|w| w.frame)
            .collect();
        check(
            others.len() == 3,
            format!("{} tiled windows left", others.len()),
        )?;
        layout_assert::check_no_overlap(&others).map_err(|v| v.to_string())?;
        command(&Command::ToggleFloat)?;
        wait_for(STEP, || state().is_some_and(|s| floating_count(&s) == 0))
            .map_err(|_| "the window never came back into the grid".to_owned())?;
        wait_until_still(&windows);
        let frames = wait_for_tiling(&windows, 4)?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
        let _ = before;
        Ok(())
    });

    // --- monocle ---------------------------------------------------------
    steps.step("toggle-monocle fills the area and restores", || {
        let focused = focused_window().ok_or("nothing is focused")?;
        command(&Command::ToggleMonocle)?;
        let area = area().ok_or("no state")?;
        wait_for(STEP, || {
            frame_of(&windows, focused).is_some_and(|f| close_enough(f, area.tiled, area.seam + 8))
        })
        .map_err(|_| {
            format!(
                "the monocle frame is {:?}, the padded area is {}",
                frame_of(&windows, focused),
                area.tiled
            )
        })?;
        let hidden = infos(&windows)
            .iter()
            .filter(|w| w.hwnd != focused && (w.cloaked || !w.visible))
            .count();
        check(
            hidden == 3,
            format!("{hidden} of the other 3 windows went away"),
        )?;
        command(&Command::ToggleMonocle)?;
        let frames = wait_for_tiling(&windows, 4)?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())
    });

    // --- maximize --------------------------------------------------------
    steps.step("toggle-maximize and back", || {
        let focused = focused_window().ok_or("nothing is focused")?;
        let before = frame_of(&windows, focused).ok_or("no frame")?;
        command(&Command::ToggleMaximize)?;
        let big = wait_some(STEP, || {
            frame_of(&windows, focused).filter(|f| f.width() > before.width() + 20)
        })
        .ok_or("the window never grew")?;
        let _ = big;
        command(&Command::ToggleMaximize)?;
        wait_for(STEP, || {
            frame_of(&windows, focused).is_some_and(|f| close_enough(f, before, 12))
        })
        .map_err(|_| {
            format!(
                "the window came back to {:?} instead of {before}",
                frame_of(&windows, focused)
            )
        })?;
        wait_for_tiling(&windows, 4).map(|_| ())
    });

    // --- minimize and restore --------------------------------------------
    steps.step("minimize reflows to three, restore back to four", || {
        let focused = focused_window().ok_or("nothing is focused")?;
        command(&Command::Minimize)?;
        wait_for(STEP, || managed_count() == 3)
            .map_err(|_| format!("state shows {} windows", managed_count()))?;
        let frames = wait_for_tiling(&windows, 3)?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
        mochi_testbed::restore_window(focused).map_err(|e| e.to_string())?;
        wait_for(Duration::from_secs(5), || managed_count() == 4)
            .map_err(|_| format!("state shows {} windows after restore", managed_count()))?;
        wait_for_tiling(&windows, 4).map(|_| ())
    });

    // --- layouts ---------------------------------------------------------
    steps.step("cycle-layout next and back to bsp", || {
        let before = layout_name().ok_or("no layout in the state")?;
        command(&Command::CycleLayout {
            direction: CycleDirection::Next,
        })?;
        wait_for(STEP, || layout_name().is_some_and(|l| l != before))
            .map_err(|_| format!("the layout stayed {before}"))?;
        wait_until_still(&windows);
        let frames = visible_frames(&windows);
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
        command(&Command::ChangeLayout {
            layout: Layout::Bsp,
        })?;
        // The state document spells the layout the way `mochi.json` does,
        // PascalCase, while the command line takes it kebab-case.
        wait_for(STEP, || {
            layout_name().is_some_and(|l| l.eq_ignore_ascii_case("bsp"))
        })
        .map_err(|_| format!("the layout is {:?}, not bsp", layout_name()))?;
        wait_for_tiling(&windows, 4).map(|_| ())
    });

    steps.step("flip-layout horizontal mirrors the tiles", || {
        let before = wait_for_tiling(&windows, 4)?;
        command(&Command::FlipLayout {
            axis: Axis::Horizontal,
        })?;
        wait_for(STEP, || visible_frames(&windows) != before)
            .map_err(|_| "nothing moved".to_owned())?;
        wait_until_still(&windows);
        let after = visible_frames(&windows);
        layout_assert::check_no_overlap(&after).map_err(|v| v.to_string())?;
        check(
            close_enough(bounds(&after), bounds(&before), 8),
            "the flipped layout covers a different area".to_owned(),
        )?;
        command(&Command::FlipLayout {
            axis: Axis::Horizontal,
        })?;
        wait_for_tiling(&windows, 4).map(|_| ())
    });

    // --- workspaces -------------------------------------------------------
    steps.step("focus-workspace 1 cloaks all four", || {
        command(&Command::FocusWorkspace { index: 1 })?;
        wait_for(STEP, || {
            infos(&windows).iter().filter(|w| w.cloaked).count() == 4
        })
        .map_err(|_| {
            format!(
                "{} of 4 windows are cloaked",
                infos(&windows).iter().filter(|w| w.cloaked).count()
            )
        })
    });

    steps.step("focus-workspace 0 uncloaks them again", || {
        command(&Command::FocusWorkspace { index: 0 })?;
        wait_for(STEP, || {
            infos(&windows).iter().all(|w| !w.cloaked && w.visible)
        })
        .map_err(|_| {
            format!(
                "{} windows are still cloaked",
                infos(&windows).iter().filter(|w| w.cloaked).count()
            )
        })?;
        wait_for_tiling(&windows, 4).map(|_| ())
    });

    steps.step("move-to-workspace 1 then focus-last-workspace", || {
        let focused = focused_window().ok_or("nothing is focused")?;
        command(&Command::MoveToWorkspace { index: 1 })?;
        wait_for(STEP, || {
            infos(&windows)
                .iter()
                .filter(|w| w.hwnd != focused)
                .all(|w| w.cloaked)
        })
        .map_err(|_| "the three windows left behind were not cloaked".to_owned())?;
        check(
            frame_of(&windows, focused).is_some(),
            "the moved window vanished".to_owned(),
        )?;
        command(&Command::FocusLastWorkspace)?;
        wait_for(STEP, || {
            infos(&windows)
                .iter()
                .filter(|w| w.hwnd != focused)
                .all(|w| !w.cloaked)
        })
        .map_err(|_| "the original workspace did not come back".to_owned())?;
        // put the window back where the rest of the test expects it
        command(&Command::FocusWorkspace { index: 1 })?;
        wait_for(STEP, || managed_count() >= 1).map_err(|e| e.to_string())?;
        command(&Command::MoveToWorkspace { index: 0 })?;
        wait_for(Duration::from_secs(5), || managed_count() == 4)
            .map_err(|_| format!("only {} windows came back", managed_count()))?;
        wait_for_tiling(&windows, 4).map(|_| ())
    });

    // --- retile, pause, reload -------------------------------------------
    steps.step("retile reproduces the same layout", || {
        let before = wait_for_tiling(&windows, 4)?;
        command(&Command::Retile)?;
        wait_until_still(&windows);
        let after = visible_frames(&windows);
        check(
            after.len() == before.len()
                && after
                    .iter()
                    .zip(&before)
                    .all(|(a, b)| close_enough(*a, *b, 4)),
            format!("retile changed the layout: {before:?} then {after:?}"),
        )
    });

    steps.step(
        "toggle-pause lets a window stray, unpause snaps it back",
        || {
            let focused = focused_window().ok_or("nothing is focused")?;
            let before = frame_of(&windows, focused).ok_or("no frame")?;
            command(&Command::TogglePause)?;
            wait_for(STEP, || {
                state().is_some_and(|s| s["paused"].as_bool() == Some(true))
            })
            .map_err(|_| "the daemon never reported paused".to_owned())?;
            mochi_testbed::move_window(focused, before.left + 60, before.top + 60)
                .map_err(|e| e.to_string())?;
            wait_for(STEP, || {
                frame_of(&windows, focused).is_some_and(|f| f.left != before.left)
            })
            .map_err(|_| "the window did not move while paused".to_owned())?;
            std::thread::sleep(Duration::from_millis(200));
            check(
                frame_of(&windows, focused).is_some_and(|f| f.left != before.left),
                "a paused daemon pulled the window back".to_owned(),
            )?;
            command(&Command::TogglePause)?;
            wait_for(STEP, || {
                frame_of(&windows, focused).is_some_and(|f| close_enough(f, before, 8))
            })
            .map_err(|_| {
                format!(
                    "unpause left the window at {:?} instead of {before}",
                    frame_of(&windows, focused)
                )
            })
        },
    );

    steps.step("reload-configuration keeps the four windows", || {
        command(&Command::ReloadConfiguration)?;
        wait_for(STEP, || managed_count() == 4)
            .map_err(|_| format!("state shows {} windows after a reload", managed_count()))?;
        wait_for_tiling(&windows, 4).map(|_| ())
    });

    // --- shutdown ---------------------------------------------------------
    steps.step("stop leaves every window visible and uncloaked", || {
        daemon.stop();
        wait_for(Duration::from_secs(5), || {
            infos(&windows)
                .iter()
                .all(|w| w.visible && !w.cloaked && !w.minimized)
        })
        .map_err(|_| {
            format!(
                "after stop: {:?}",
                infos(&windows)
                    .iter()
                    .map(|w| (w.hwnd_hex(), w.visible, w.cloaked, w.minimized))
                    .collect::<Vec<_>>()
            )
        })
    });

    drop(windows);
    steps.finish(&log);
}

/// How many floating windows the focused workspace holds.
fn floating_count(state: &Value) -> usize {
    let monitor = state["focused_monitor"].as_u64().unwrap_or(0) as usize;
    let workspace = state["monitors"][monitor]["focused_workspace"]
        .as_u64()
        .unwrap_or(0) as usize;
    state["monitors"][monitor]["workspaces"][workspace]["floating_windows"]
        .as_array()
        .map_or(0, Vec::len)
}

/// The layout name of the focused workspace.
fn layout_name() -> Option<String> {
    let state = state()?;
    let monitor = state["focused_monitor"].as_u64().unwrap_or(0) as usize;
    let workspace = state["monitors"][monitor]["focused_workspace"]
        .as_u64()
        .unwrap_or(0) as usize;
    state["monitors"][monitor]["workspaces"][workspace]["layout"]
        .as_str()
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// test two: a daemon that was killed leaves cloaked windows behind
// ---------------------------------------------------------------------------

#[test]
fn a_hard_killed_daemon_gives_its_windows_back_on_the_next_start() {
    skip_unless_allowed!("a_hard_killed_daemon_gives_its_windows_back_on_the_next_start");

    let mut daemon = Daemon::start("hard-kill");
    let log = daemon.log();
    let windows = TestWindows::spawn(2, 0).expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("the daemon adopts both windows", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} windows", managed_count()))
    });

    steps.step("focus-workspace 1 cloaks both", || {
        command(&Command::FocusWorkspace { index: 1 })?;
        wait_for(STEP, || infos(&windows).iter().all(|w| w.cloaked))
            .map_err(|_| "the windows were not cloaked".to_owned())
    });

    steps.step("a hard kill leaves them cloaked", || {
        daemon.hard_kill();
        check(
            infos(&windows).iter().all(|w| w.cloaked),
            "the windows came back on their own, so the kill was not hard".to_owned(),
        )
    });

    let mut second = Daemon::start("hard-kill-2");
    let second_log = second.log();

    steps.step("the next start uncloaks and re-tiles them", || {
        wait_for(Duration::from_secs(10), || {
            infos(&windows).iter().all(|w| !w.cloaked && w.visible)
        })
        .map_err(|_| {
            format!(
                "{} of 2 windows are still cloaked",
                infos(&windows).iter().filter(|w| w.cloaked).count()
            )
        })?;
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} windows", managed_count()))?;
        let frames = wait_for_tiling(&windows, 2)?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
        let area = area().ok_or("no state")?;
        layout_assert::check_all_within(&frames, area.work_area).map_err(|v| v.to_string())
    });

    steps.step("stop leaves both windows visible", || {
        second.stop();
        wait_for(Duration::from_secs(5), || {
            infos(&windows).iter().all(|w| w.visible && !w.cloaked)
        })
        .map_err(|_| "a window stayed hidden after stop".to_owned())
    });

    drop(windows);
    steps.finish(&format!("{log} and {second_log}"));
}

// ---------------------------------------------------------------------------
// the pure helpers, which run everywhere
// ---------------------------------------------------------------------------

#[cfg(test)]
mod pure {
    use super::*;

    #[test]
    fn the_bounding_box_of_a_tiling_is_the_area_it_covers() {
        let frames = [
            Rect::new(0, 0, 100, 50),
            Rect::new(100, 0, 200, 50),
            Rect::new(0, 50, 200, 100),
        ];
        assert_eq!(bounds(&frames), Rect::new(0, 0, 200, 100));
    }

    #[test]
    fn close_enough_allows_exactly_the_tolerance() {
        let a = Rect::new(0, 0, 100, 100);
        assert!(close_enough(a, Rect::new(4, -4, 96, 104), 4));
        assert!(!close_enough(a, Rect::new(5, 0, 100, 100), 4));
    }

    #[test]
    fn the_gate_names_the_reason_it_skips() {
        // The variable is not set in a normal `cargo test` run, and that has to
        // be the reason reported first so the message is actionable.
        if std::env::var("MOCHI_E2E").is_err() {
            assert_eq!(allowed().as_deref(), Some("MOCHI_E2E is not 1"));
        }
    }

    #[test]
    fn a_rect_survives_the_state_document() {
        let value = serde_json::json!({"left": 1, "top": 2, "right": 3, "bottom": 4});
        assert_eq!(rect(&value), Some(Rect::new(1, 2, 3, 4)));
        assert_eq!(rect(&serde_json::json!({"left": 1})), None);
    }
}
