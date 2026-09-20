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
use mochi_testbed::{Rect, SpawnOptions, TestWindowInfo, TestWindows, layout_assert};
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
    /// Starts the daemon with no hotkeys bound.
    ///
    /// Every test but the hotkey one wants this: a daemon that installed a
    /// keyboard hook and read the user's own hotkey file would fight whatever
    /// is already binding those keys on the desktop the tests run on.
    fn start(tag: &str) -> Daemon {
        Daemon::start_with(tag, &["--no-hotkeys"])
    }

    /// Starts the daemon against the test config, managing only the testbed
    /// class, with `RUST_LOG=debug` going to a log file under `%TEMP%`.
    fn start_with(tag: &str, extra: &[&str]) -> Daemon {
        // A daemon left over from an earlier run would take the single
        // instance mutex and the new one would exit at once.
        let _ = send(&Command::Stop);
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
            .args(extra)
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
            let _ = send(&Command::Stop);
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

/// Whether the window manager has taken this window off screen.
///
/// Cloaking is the usual way, but a window the shell does not track cannot be
/// cloaked by anyone, and those are hidden instead. Both count as off screen.
fn off_screen(w: &TestWindowInfo) -> bool {
    w.cloaked || !w.visible
}

/// The extended frame bounds of every window of the batch that is on screen.
fn visible_frames(windows: &TestWindows) -> Vec<Rect> {
    infos(windows)
        .iter()
        .filter(|w| w.visible && !w.cloaked && !w.minimized)
        .map(|w| w.frame)
        .collect()
}

/// A visible test window that has another one beside it in `direction`.
///
/// "Beside" is the same question the daemon answers: the two tiles overlap on
/// the other axis, and one starts past the other along this one. Reading it off
/// the real frames keeps the test honest about whatever the layout actually
/// produced, on any screen.
fn window_with_a_neighbour(windows: &TestWindows, direction: Direction) -> Option<i64> {
    let tiles: Vec<(i64, Rect)> = infos(windows)
        .into_iter()
        .filter(|w| w.visible && !w.cloaked && !w.minimized)
        .map(|w| (w.hwnd, w.frame))
        .collect();

    tiles
        .iter()
        .find(|(_, from)| {
            tiles.iter().any(|(_, to)| {
                let overlaps_vertically = from.top < to.bottom && to.top < from.bottom;
                let overlaps_horizontally = from.left < to.right && to.left < from.right;
                match direction {
                    Direction::Left => overlaps_vertically && to.right <= from.left,
                    Direction::Right => overlaps_vertically && from.right <= to.left,
                    Direction::Up => overlaps_horizontally && to.bottom <= from.top,
                    Direction::Down => overlaps_horizontally && from.bottom <= to.top,
                }
            })
        })
        .map(|(hwnd, _)| *hwnd)
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
    //
    // Which window to start from is not a free choice. A four container BSP
    // puts one tile against the top of the screen and one against the left, so
    // "focus up" and "focus left" have nowhere to go from those, and which tile
    // holds the focus after four windows are adopted depends on what the
    // desktop made foreground. Asserting that focus always moves from wherever
    // it happens to be is asserting something that is not true, and it failed
    // on a machine whose starting focus differed from this one's. Each
    // direction starts from a window that demonstrably has a neighbour that
    // way, picked from the frames as they actually are.
    for direction in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        steps.step(&format!("focus {direction}"), || {
            let start = window_with_a_neighbour(&windows, direction).ok_or_else(|| {
                format!("no tile has a neighbour to the {direction}, so the layout is wrong")
            })?;
            mochi_testbed::focus_window(start).map_err(|e| e.to_string())?;
            wait_for(STEP, || focused_window() == Some(start))
                .map_err(|_| format!("{start:#x} never took the focus to start from"))?;

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
        wait_some(STEP, || {
            frame_of(&windows, focused).filter(|now| now.width() < grown.width() - 4)
        })
        .ok_or("width did not come back down")?;
        // The shrink itself may still be animating when the check above first
        // sees it cross the threshold, so let the batch settle before reading
        // the width the daemon actually converged on.
        wait_until_still(&windows);
        let back = frame_of(&windows, focused).ok_or("no frame")?;
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
    steps.step("focus-workspace 1 takes all four off screen", || {
        command(&Command::FocusWorkspace { index: 1 })?;
        wait_for(STEP, || {
            infos(&windows).iter().filter(|w| off_screen(w)).count() == 4
        })
        .map_err(|_| {
            format!(
                "{} of 4 windows are off screen",
                infos(&windows).iter().filter(|w| off_screen(w)).count()
            )
        })
    });

    steps.step("focus-workspace 0 brings them back", || {
        command(&Command::FocusWorkspace { index: 0 })?;
        wait_for(STEP, || infos(&windows).iter().all(|w| !off_screen(w))).map_err(|_| {
            format!(
                "{} windows are still off screen",
                infos(&windows).iter().filter(|w| off_screen(w)).count()
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
                .all(off_screen)
        })
        .map_err(|_| "the three windows left behind are still on screen".to_owned())?;
        check(
            frame_of(&windows, focused).is_some(),
            "the moved window vanished".to_owned(),
        )?;
        command(&Command::FocusLastWorkspace)?;
        wait_for(STEP, || {
            infos(&windows)
                .iter()
                .filter(|w| w.hwnd != focused)
                .all(|w| !off_screen(w))
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

    steps.step("focus-workspace 1 takes both off screen", || {
        command(&Command::FocusWorkspace { index: 1 })?;
        wait_for(STEP, || infos(&windows).iter().all(off_screen))
            .map_err(|_| "the windows are still on screen".to_owned())
    });

    steps.step("a hard kill leaves them off screen", || {
        daemon.hard_kill();
        check(
            infos(&windows).iter().all(off_screen),
            "the windows came back on their own, so the kill was not hard".to_owned(),
        )
    });

    let mut second = Daemon::start("hard-kill-2");
    let second_log = second.log();

    steps.step("the next start brings them back and re-tiles them", || {
        wait_for(Duration::from_secs(10), || {
            infos(&windows).iter().all(|w| !off_screen(w))
        })
        .map_err(|_| {
            format!(
                "{} of 2 windows are still off screen",
                infos(&windows).iter().filter(|w| off_screen(w)).count()
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
            infos(&windows).iter().all(|w| !off_screen(w))
        })
        .map_err(|_| "a window stayed off screen after stop".to_owned())
    });

    drop(windows);
    steps.finish(&format!("{log} and {second_log}"));
}

// ---------------------------------------------------------------------------
// test three: a visual command reaches the desktop, not just `mochic state`
// ---------------------------------------------------------------------------

/// The alpha the renderer falls back to when the file names no
/// `transparency_alpha`, which the scratch config does not.
const DEFAULT_ALPHA: u8 = 200;

#[test]
fn a_visual_command_changes_the_desktop_before_the_next_reload() {
    skip_unless_allowed!("a_visual_command_changes_the_desktop_before_the_next_reload");

    let mut daemon = Daemon::start("visuals");
    let log = daemon.log();
    let windows = TestWindows::spawn(2, 0).expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("the daemon adopts both windows", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} windows", managed_count()))?;
        wait_for_tiling(&windows, 2).map(|_| ())
    });

    steps.step("nothing is faded while transparency is off", || {
        check(
            infos(&windows).iter().all(|w| w.alpha.is_none()),
            "a window was already layered before the command",
        )
    });

    steps.step("transparency fades the unfocused window at once", || {
        command(&Command::ToggleTransparency)?;
        let faded = wait_some(STEP, || {
            let all = infos(&windows);
            let faded: Vec<_> = all.iter().filter(|w| w.alpha.is_some()).collect();
            (faded.len() == 1).then(|| (faded[0].hwnd, faded[0].alpha, faded[0].foreground))
        })
        .ok_or_else(|| {
            format!(
                "no single window faded: {:?}",
                infos(&windows)
                    .iter()
                    .map(|w| (w.hwnd_hex(), w.alpha, w.foreground))
                    .collect::<Vec<_>>()
            )
        })?;
        let (hwnd, alpha, foreground) = faded;
        check(
            alpha == Some(DEFAULT_ALPHA),
            format!("{hwnd:#x} faded to {alpha:?} instead of {DEFAULT_ALPHA}"),
        )?;
        check(!foreground, format!("{hwnd:#x} is the focused window"))
    });

    steps.step("turning it off puts the alpha back", || {
        command(&Command::ToggleTransparency)?;
        wait_for(STEP, || infos(&windows).iter().all(|w| w.alpha.is_none())).map_err(|_| {
            format!(
                "a window stayed faded: {:?}",
                infos(&windows)
                    .iter()
                    .map(|w| (w.hwnd_hex(), w.alpha))
                    .collect::<Vec<_>>()
            )
        })
    });

    steps.step("border, colour and animation commands are accepted", || {
        command(&Command::Border {
            state: mochi_client::BooleanState::Enable,
        })?;
        command(&Command::BorderWidth { width: 6 })?;
        command(&Command::BorderColour {
            kind: mochi_client::WindowKind::Single,
            r: 0xff,
            g: 0xbb,
            b: 0xdf,
        })?;
        command(&Command::Animation {
            state: mochi_client::BooleanState::Enable,
        })?;
        command(&Command::AnimationDuration { duration: 120 })?;
        let state = state().ok_or("no state after the visual commands")?;
        let settings = &state["settings"];
        check(
            settings["border"] == true
                && settings["border_width"] == 6
                && settings["animation"] == true
                && settings["animation_duration"] == 120,
            format!("state still reports {settings}"),
        )?;
        check(
            settings["border_colours"]["single"] == "#ffbbdf",
            format!("the colour did not land: {}", settings["border_colours"]),
        )
    });

    steps.step("the border manager started on a real desktop", || {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        check(
            !text.contains("could not start the border manager"),
            "the daemon logged a border manager failure".to_owned(),
        )
    });

    steps.step("stop clears every visual it applied", || {
        daemon.stop();
        wait_for(Duration::from_secs(5), || {
            infos(&windows)
                .iter()
                .all(|w| w.alpha.is_none() && w.visible && !w.cloaked)
        })
        .map_err(|_| {
            format!(
                "after stop: {:?}",
                infos(&windows)
                    .iter()
                    .map(|w| (w.hwnd_hex(), w.alpha, w.visible, w.cloaked))
                    .collect::<Vec<_>>()
            )
        })
    });

    drop(windows);
    steps.finish(&log);
}

// ---------------------------------------------------------------------------
// test four: stacking, which has no bar to show for it and needs the state
// document and the window rectangles instead
// ---------------------------------------------------------------------------

/// The containers of the focused workspace, as the state document has them.
fn containers() -> Vec<Value> {
    let Some(state) = state() else {
        return Vec::new();
    };
    let monitor = state["focused_monitor"].as_u64().unwrap_or(0) as usize;
    let workspace = state["monitors"][monitor]["focused_workspace"]
        .as_u64()
        .unwrap_or(0) as usize;
    state["monitors"][monitor]["workspaces"][workspace]["containers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The window the first container is showing.
fn stacked_focus() -> Option<i64> {
    containers().first()?["focused_window"].as_i64()
}

#[test]
fn a_stack_shows_one_window_at_a_time_and_cycling_swaps_them() {
    skip_unless_allowed!("a_stack_shows_one_window_at_a_time_and_cycling_swaps_them");

    let mut daemon = Daemon::start("stack");
    let log = daemon.log();
    let windows = TestWindows::spawn(2, 0).expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("the daemon tiles both windows side by side", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} windows", managed_count()))?;
        let frames = wait_for_tiling(&windows, 2)?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())
    });

    steps.step("stack merges them into one container", || {
        // A stack in a direction with no container there is a silent no-op,
        // so the focus goes to the left tile first and the merge runs into the
        // one that is certainly to its right.
        command(&Command::Focus {
            direction: Direction::Left,
        })?;
        command(&Command::Stack {
            direction: Direction::Right,
        })?;
        if containers().len() != 1 {
            command(&Command::Stack {
                direction: Direction::Left,
            })?;
        }
        wait_for(STEP, || containers().len() == 1)
            .map_err(|_| format!("{} containers after stacking", containers().len()))?;
        let container = containers().remove(0);
        check(
            container["stack"] == true,
            "the container does not call itself a stack".to_owned(),
        )?;
        check(
            container["windows"].as_array().map_or(0, Vec::len) == 2,
            format!("the stack holds {}", container["windows"]),
        )
    });

    steps.step("the shown window covers the whole tile", || {
        let shown = stacked_focus().ok_or("the stack shows nothing")?;
        let area = area().ok_or("no state to read the work area from")?;
        let frame =
            wait_some(STEP, || frame_of(&windows, shown)).ok_or("the shown window has no frame")?;
        check(
            close_enough(frame, area.tiled, area.seam + 4),
            format!(
                "the shown window is {frame}, the tiled area is {}",
                area.tiled
            ),
        )
    });

    steps.step("cycling the stack brings the other window forward", || {
        let before = stacked_focus().ok_or("the stack shows nothing")?;
        command(&Command::CycleStack {
            direction: CycleDirection::Next,
        })?;
        let after = wait_some(STEP, || stacked_focus().filter(|&shown| shown != before))
            .ok_or_else(|| format!("the stack still shows {before:#x}"))?;

        let area = area().ok_or("no state")?;
        let frame =
            wait_some(STEP, || frame_of(&windows, after)).ok_or("the new window has no frame")?;
        check(
            close_enough(frame, area.tiled, area.seam + 4),
            format!("the window brought forward sits at {frame}, not on the tile"),
        )
    });

    steps.step("unstack gives both windows their own tile back", || {
        command(&Command::Unstack)?;
        wait_for(STEP, || containers().len() == 2)
            .map_err(|_| format!("{} containers after unstacking", containers().len()))?;
        let frames = wait_for_tiling(&windows, 2)?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())
    });

    steps.step("stop leaves both windows visible", || {
        daemon.stop();
        wait_for(Duration::from_secs(5), || {
            infos(&windows).iter().all(|w| w.visible && !w.cloaked)
        })
        .map_err(|_| "a window stayed hidden after stop".to_owned())
    });

    drop(windows);
    steps.finish(&log);
}

// ---------------------------------------------------------------------------
// test five: an application that refuses to shrink
// ---------------------------------------------------------------------------

/// The minimum size the stubborn window defends, wider than half of any screen
/// this runs on, so its tile can never satisfy it.
const MIN_SIZE: (i32, i32) = (1600, 900);

/// Every test window on the desktop, whichever batch spawned it.
fn all_infos() -> Vec<TestWindowInfo> {
    mochi_testbed::list_windows().unwrap_or_default()
}

#[test]
fn a_window_that_defends_a_minimum_size_does_not_make_the_daemon_thrash() {
    skip_unless_allowed!("a_window_that_defends_a_minimum_size_does_not_make_the_daemon_thrash");

    let mut daemon = Daemon::start("min-size");
    let log = daemon.log();
    let plain = TestWindows::spawn(1, 0).expect("could not spawn the plain window");
    let stubborn = TestWindows::spawn_with(&SpawnOptions {
        min_size: Some(MIN_SIZE),
        title_prefix: "MochiTestFirm".to_owned(),
        ..SpawnOptions::new(1, 0)
    })
    .expect("could not spawn the stubborn window");
    let firm = stubborn.handles()[0];

    let mut steps = Steps::default();

    steps.step("the daemon adopts both windows", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} windows", managed_count()))
    });

    steps.step("the window keeps the size it refuses to go below", || {
        let info = wait_some(Duration::from_secs(5), || {
            all_infos().into_iter().find(|w| w.hwnd == firm)
        })
        .ok_or("the stubborn window vanished")?;
        check(
            info.rect.width() >= MIN_SIZE.0 && info.rect.height() >= MIN_SIZE.1,
            format!(
                "{} is {}x{}, under the minimum it defends",
                info.hwnd_hex(),
                info.rect.width(),
                info.rect.height()
            ),
        )
    });

    steps.step("the layout settles instead of oscillating", || {
        wait_until_still(&plain);
        let first: Vec<_> = all_infos().iter().map(|w| (w.hwnd, w.frame)).collect();
        // Long enough that a daemon fighting the window over its size would
        // have moved something again; this is the whole point of the step.
        std::thread::sleep(Duration::from_millis(800));
        let second: Vec<_> = all_infos().iter().map(|w| (w.hwnd, w.frame)).collect();
        check(
            first == second,
            format!("the layout is still moving: {first:?} then {second:?}"),
        )
    });

    steps.step("the daemon did not retile in a loop", || {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let retiles = text.matches("retiling").count();
        check(
            retiles < 50,
            format!("{retiles} retiles for two windows reads like a fight"),
        )
    });

    steps.step("the plain window is still usable", || {
        let area = area().ok_or("no state to read the work area from")?;
        let info = all_infos()
            .into_iter()
            .find(|w| w.hwnd == plain.handles()[0])
            .ok_or("the plain window vanished")?;
        check(
            info.frame.width() > 0 && info.frame.height() > 0,
            format!("{} was squeezed to nothing", info.hwnd_hex()),
        )?;
        check(
            area.work_area.contains(&info.frame),
            format!("{} sits outside the work area", info.hwnd_hex()),
        )
    });

    steps.step("both windows are still in the model", || {
        check(
            managed_count() == 2,
            format!("state shows {} windows", managed_count()),
        )
    });

    steps.step("stop leaves both windows visible", || {
        daemon.stop();
        wait_for(Duration::from_secs(5), || {
            all_infos().iter().all(|w| w.visible && !w.cloaked)
        })
        .map_err(|_| "a window stayed hidden after stop".to_owned())
    });

    drop(stubborn);
    drop(plain);
    steps.finish(&log);
}

// ---------------------------------------------------------------------------
// the shell cloak, which is the path every real application takes and which no
// other test in this file can reach
// ---------------------------------------------------------------------------

/// Whether this desktop is Mochi's alone.
///
/// A window without `WS_EX_TOOLWINDOW` is visible to every other window
/// manager, so this test must not run next to one. It is opt-in for that
/// reason, and CI sets it because a runner has nothing else managing windows.
fn cloak_test_allowed() -> bool {
    std::env::var("MOCHI_E2E_CLOAK").ok().as_deref() == Some("1")
}

#[test]
fn a_real_cloak_leaves_the_window_managed_and_brings_it_back() {
    skip_unless_allowed!("a_real_cloak_leaves_the_window_managed_and_brings_it_back");
    if !cloak_test_allowed() {
        eprintln!("skipping a_real_cloak_leaves_the_window_managed_and_brings_it_back:");
        eprintln!("  MOCHI_E2E_CLOAK is not 1. This test spawns a window without the tool");
        eprintln!("  window bit, which any other window manager on this desktop would take.");
        return;
    }

    let mut daemon = Daemon::start("cloak");
    let log = daemon.log();

    // Without the tool window bit the shell gives the window an application
    // view, which is the only way one process can cloak another's window.
    // Every other test here gets a tool window, so every cloak in them falls
    // back to ShowWindow and the path a real application takes is never run.
    let windows = TestWindows::spawn_with(&SpawnOptions {
        taskbar: true,
        ..SpawnOptions::new(2, 0)
    })
    .expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("both windows are managed", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} windows", managed_count()))
    });

    steps.step("a workspace switch really cloaks them", || {
        command(&Command::FocusWorkspace { index: 1 })?;
        wait_for(STEP, || infos(&windows).iter().all(|w| w.cloaked)).map_err(|_| {
            let how: Vec<_> = infos(&windows)
                .iter()
                .map(|w| format!("cloaked={} visible={}", w.cloaked, w.visible))
                .collect();
            format!(
                "the windows did not come back cloaked, so this ran the fallback \
                 and not the shell path it exists to cover: {how:?}"
            )
        })
    });

    steps.step("and they are still Mochi's to give back", || {
        // The defect this test exists for: the cloak echoes back as an event,
        // and a daemon that does not recognise its own work unmanages the
        // window it just hid. It is then invisible, out of the model and out
        // of the restore record at once, and nothing left knows it exists.
        check(
            managed_count() == 2,
            format!("the daemon let go of {} of them", 2 - managed_count()),
        )
    });

    steps.step("switching back uncloaks them", || {
        command(&Command::FocusWorkspace { index: 0 })?;
        wait_for(STEP, || infos(&windows).iter().all(|w| !w.cloaked))
            .map_err(|_| "a window stayed cloaked".to_owned())?;
        check(
            managed_count() == 2,
            "a window was lost on the way back".to_owned(),
        )
    });

    steps.step("stop leaves both on screen", || {
        command(&Command::FocusWorkspace { index: 1 })?;
        wait_for(STEP, || infos(&windows).iter().all(|w| w.cloaked))
            .map_err(|_| "the windows never went off screen".to_owned())?;
        daemon.stop();
        wait_for(Duration::from_secs(10), || {
            infos(&windows).iter().all(|w| !w.cloaked && w.visible)
        })
        .map_err(|_| {
            "a window was left cloaked after stop, which is the worst thing \
             this daemon can do"
                .to_owned()
        })
    });

    drop(windows);
    steps.finish(&log);
}

// ---------------------------------------------------------------------------
// test six: the hotkeys, the one part of the daemon the pipe cannot reach.
// Real key presses, injected into the real desktop.
// ---------------------------------------------------------------------------

/// The keys this test presses.
///
/// F13 to F16 exist in the virtual key table and on no keyboard sold this
/// century, and no layout produces them. Nothing else on this desktop is
/// listening for them, which is the only reason it is safe for a test to inject
/// key presses into a session the user is sitting in front of. No test may ever
/// press a key a person or another program could mean.
mod keys {
    /// `VK_F13`.
    pub const F13: u16 = 0x7C;
    /// `VK_F14`.
    pub const F14: u16 = 0x7D;
    /// `VK_F16`.
    pub const F16: u16 = 0x7F;
    /// `VK_LMENU`, the left Alt key.
    pub const LEFT_ALT: u16 = 0xA4;
    /// `VK_ESCAPE`. The only key here a person could also mean, and it is sent
    /// for one purpose: to leave a menu this test itself opened, into a window
    /// this test itself spawned and checked is in the foreground first.
    pub const ESCAPE: u16 = 0x1B;
}

/// Presses a key, with one modifier held around it, the way a person would.
fn tap(vk: u16, modifier: Option<u16>) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VIRTUAL_KEY,
    };

    fn event(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    let mut inputs = Vec::with_capacity(4);
    inputs.extend(modifier.map(|m| event(m, false)));
    inputs.push(event(vk, false));
    inputs.push(event(vk, true));
    inputs.extend(modifier.map(|m| event(m, true)));

    let size = std::mem::size_of::<INPUT>() as i32;
    let sent = unsafe { SendInput(&inputs, size) } as usize;
    assert_eq!(
        sent,
        inputs.len(),
        "the desktop refused an injected key press"
    );
    // The press travels through the hook thread and the command loop. Nothing
    // here polls a key, so this is the one place the test has to give it time.
    std::thread::sleep(Duration::from_millis(150));
}

/// Whether the daemon says tiling is paused.
fn paused() -> Option<bool> {
    state()?["paused"].as_bool()
}

/// The bindings the daemon holds, as `mochic hotkeys --json` prints them.
fn hotkey_document() -> Option<Value> {
    match send(&Command::Hotkeys) {
        Ok(Response::Hotkeys { hotkeys }) => Some(hotkeys),
        _ => None,
    }
}

/// How many bindings are loaded.
fn hotkey_count() -> usize {
    hotkey_document()
        .and_then(|d| d["bindings"].as_array().map(Vec::len))
        .unwrap_or(0)
}

#[test]
fn hotkeys_drive_the_daemon_and_stay_out_of_the_way() {
    skip_unless_allowed!("hotkeys_drive_the_daemon_and_stay_out_of_the_way");

    let scratch = temp_dir().join("hotkeys");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("could not make the hotkey scratch directory");
    let file = scratch.join("hotkeys");
    let marker = scratch.join("shell-ran.txt");

    let bindings = format!(
        ".shell cmd\n\nf13             : toggle-pause\nalt + f14       : toggle-game-mode\nf16             : echo ran > \"{}\"\n",
        marker.display()
    );
    std::fs::write(&file, &bindings).expect("could not write the hotkey file");

    let mut daemon = Daemon::start_with("hotkeys", &["--hotkeys", &file.to_string_lossy()]);
    let log = daemon.log();
    let windows = TestWindows::spawn(2, 0).expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("the daemon binds the file it was given", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} windows", managed_count()))?;
        let document = hotkey_document().ok_or("the daemon reported no hotkeys")?;
        check(
            document["bindings"].as_array().map(Vec::len) == Some(3),
            format!("expected three bindings, got {document}"),
        )?;
        check(
            document["errors"].as_array().is_some_and(Vec::is_empty),
            format!("the file did not parse cleanly: {document}"),
        )
    });

    steps.step("a bound key runs its command", || {
        check(paused() == Some(false), "the daemon came up paused")?;
        tap(keys::F13, None);
        check(
            paused() == Some(true),
            "F13 did not reach toggle-pause".to_owned(),
        )?;
        tap(keys::F13, None);
        check(
            paused() == Some(false),
            "the second F13 did not resume tiling".to_owned(),
        )
    });

    steps.step("the modifiers are part of the trigger", || {
        // Alt+F14 is bound; F14 alone is not, and neither is Alt+F13.
        tap(keys::F14, None);
        check(
            paused() == Some(false),
            "an unbound key acted anyway".to_owned(),
        )?;
        tap(keys::F13, Some(keys::LEFT_ALT));
        check(
            paused() == Some(false),
            "a bound key with an extra modifier acted anyway".to_owned(),
        )
    });

    steps.step("game mode suspends every binding but its own", || {
        tap(keys::F14, Some(keys::LEFT_ALT));
        check(
            paused() == Some(true),
            "game mode did not pause tiling".to_owned(),
        )?;
        check(
            hotkey_document().is_some_and(|d| d["gate"] == "game-mode"),
            "the daemon does not report itself in game mode".to_owned(),
        )?;

        // In game mode this key belongs to the game, not to Mochi.
        tap(keys::F13, None);
        check(
            paused() == Some(true),
            "a suspended binding fired during game mode".to_owned(),
        )
    });

    steps.step("the same key leaves game mode again", || {
        tap(keys::F14, Some(keys::LEFT_ALT));
        check(
            paused() == Some(false),
            "game mode did not resume tiling".to_owned(),
        )?;
        check(
            hotkey_document().is_some_and(|d| d["gate"] == "all"),
            "the bindings did not come back".to_owned(),
        )?;
        tap(keys::F13, None);
        let acted = paused() == Some(true);
        tap(keys::F13, None);
        check(acted, "the bindings are still suspended".to_owned())
    });

    steps.step("a shell binding starts its program", || {
        let _ = std::fs::remove_file(&marker);
        tap(keys::F16, None);
        wait_for(Duration::from_secs(5), || marker.exists())
            .map_err(|_| format!("{} was never written", marker.display()))
    });

    steps.step("saving the file rebinds the keys", || {
        std::fs::write(&file, ".shell cmd\n\nf13 : retile\n")
            .map_err(|e| format!("could not rewrite the hotkey file: {e}"))?;
        wait_for(Duration::from_secs(10), || hotkey_count() == 1)
            .map_err(|_| format!("the daemon still holds {} bindings", hotkey_count()))?;
        // F13 retiles now instead of pausing, so the pause state must not move.
        tap(keys::F13, None);
        check(
            paused() == Some(false),
            "the old binding fired after a reload".to_owned(),
        )
    });

    steps.step("a broken line costs only itself", || {
        std::fs::write(
            &file,
            ".shell cmd\n\nf13 : toggle-pause\nalt + nosuchkey : retile\n",
        )
        .map_err(|e| format!("could not rewrite the hotkey file: {e}"))?;
        wait_for(Duration::from_secs(10), || {
            hotkey_document().is_some_and(|d| {
                d["bindings"].as_array().map(Vec::len) == Some(1)
                    && d["errors"].as_array().map(Vec::len) == Some(1)
            })
        })
        .map_err(|_| format!("the daemon reports {:?}", hotkey_document()))?;
        tap(keys::F13, None);
        let acted = paused() == Some(true);
        tap(keys::F13, None);
        check(acted, "the good line stopped working too".to_owned())
    });

    steps.step("hotkeys can be turned off and on again", || {
        command(&Command::SetHotkeys {
            state: mochi_client::BooleanState::Disable,
        })?;
        tap(keys::F13, None);
        check(
            paused() == Some(false),
            "a key fired while hotkeys were off".to_owned(),
        )?;

        command(&Command::SetHotkeys {
            state: mochi_client::BooleanState::Enable,
        })?;
        tap(keys::F13, None);
        let acted = paused() == Some(true);
        tap(keys::F13, None);
        check(acted, "hotkeys did not come back".to_owned())
    });

    steps.step("tiling still works", || {
        wait_for_tiling(&windows, 2).map(|_| ())
    });

    steps.step("stop leaves both windows visible", || {
        daemon.stop();
        wait_for(Duration::from_secs(5), || {
            infos(&windows).iter().all(|w| w.visible && !w.cloaked)
        })
        .map_err(|_| "a window stayed hidden after stop".to_owned())
    });

    drop(windows);
    let _ = std::fs::remove_dir_all(&scratch);
    steps.finish(&log);
}

// ---------------------------------------------------------------------------
// test seven: what a swallowed `alt + key` binding leaves behind in the
// application that was in front. Nearly every binding a person writes holds
// Alt, and an application sees Alt go down, never sees the key the hook
// swallowed, and then sees Alt come up: the sequence `DefWindowProc` reads as
// "open the menu bar".
// ---------------------------------------------------------------------------

/// Refuses to inject anything unless the window under test is in the
/// foreground.
///
/// Checked again immediately before every single injection, not once at the
/// start: an Alt press that landed in somebody else's window would open a menu
/// on the desktop the user is sitting at, and it would measure that window
/// rather than this one.
fn only_when_foreground(hwnd: i64) -> Result<(), String> {
    check(
        mochi_testbed::foreground_window() == hwnd,
        format!(
            "{hwnd:#x} is not the foreground window ({:#x} is), so nothing may be injected",
            mochi_testbed::foreground_window()
        ),
    )
}

/// Leaves the menu, if the last keystroke opened one, and says whether it is
/// gone. Never leaves menu mode behind for the next step to trip over.
fn leave_menu(hwnd: i64) -> bool {
    if !mochi_testbed::in_menu_mode(hwnd) {
        return true;
    }
    if only_when_foreground(hwnd).is_ok() {
        tap(keys::ESCAPE, None);
    }
    !mochi_testbed::in_menu_mode(hwnd)
}

#[test]
fn an_alt_binding_does_not_leave_the_application_in_menu_mode() {
    skip_unless_allowed!("an_alt_binding_does_not_leave_the_application_in_menu_mode");

    let scratch = temp_dir().join("menu-mode");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("could not make the menu scratch directory");
    let file = scratch.join("hotkeys");
    std::fs::write(&file, "alt + f13 : toggle-pause\n").expect("could not write the hotkey file");

    let mut daemon = Daemon::start_with("menu-mode", &["--hotkeys", &file.to_string_lossy()]);
    let log = daemon.log();

    // One window, and it has a real menu bar: a window without one has no menu
    // to enter, so it would answer "no menu mode" to every sequence and prove
    // nothing at all.
    let windows = TestWindows::spawn_with(&SpawnOptions {
        menu_bar: true,
        ..SpawnOptions::new(1, 0)
    })
    .expect("could not spawn the test window");
    let hwnd = windows.handles()[0];

    let mut steps = Steps::default();

    steps.step("the window with the menu bar is in front", || {
        wait_for(Duration::from_secs(10), || managed_count() == 1)
            .map_err(|_| format!("state shows {} windows", managed_count()))?;
        check(
            hotkey_count() == 1,
            format!("the daemon holds {} bindings, not one", hotkey_count()),
        )?;
        mochi_testbed::focus_window(hwnd).map_err(|e| e.to_string())?;
        wait_for(STEP, || mochi_testbed::foreground_window() == hwnd).map_err(|_| {
            format!(
                "{hwnd:#x} never reached the foreground, so no key may be injected: \
                 the foreground is {:#x}",
                mochi_testbed::foreground_window()
            )
        })?;
        check(
            !mochi_testbed::in_menu_mode(hwnd),
            format!("{hwnd:#x} was already in a menu before anything was pressed"),
        )
    });

    // The baseline. Without it the step below is worthless: a window that
    // never enters menu mode would pass it for the wrong reason.
    steps.step("a bare alt does open the menu bar on this window", || {
        only_when_foreground(hwnd)?;
        tap(keys::LEFT_ALT, None);
        let opened = mochi_testbed::in_menu_mode(hwnd);
        let left = leave_menu(hwnd);
        check(
            opened,
            format!(
                "BASELINE BROKEN: a bare alt did not put {hwnd:#x} into menu mode. \
                 This test can then prove nothing about what a binding does, \
                 and the step below would pass whatever the hook is doing."
            ),
        )?;
        check(left, "escape did not get out of the menu again".to_owned())
    });

    steps.step("a swallowed alt binding does not open it", || {
        only_when_foreground(hwnd)?;
        check(paused() == Some(false), "the daemon came up paused")?;

        tap(keys::F13, Some(keys::LEFT_ALT));
        let fired = paused() == Some(true);
        let menu = mochi_testbed::in_menu_mode(hwnd);

        // Put the desktop and the daemon back the way they were, whichever way
        // the measurement went, before anything is reported.
        let left = leave_menu(hwnd);
        if only_when_foreground(hwnd).is_ok() {
            tap(keys::F13, Some(keys::LEFT_ALT));
        }
        let back = leave_menu(hwnd);

        check(
            fired,
            "alt + f13 never reached the daemon, so this step says nothing \
             about what the press left behind"
                .to_owned(),
        )?;
        check(
            !menu,
            format!(
                "alt + f13 left {hwnd:#x} in menu mode: the application saw alt go down \
                 and come back up with nothing in between, and opened its menu bar"
            ),
        )?;
        check(
            left && back,
            "the window stayed in a menu afterwards".to_owned(),
        )?;
        check(
            paused() == Some(false),
            "the second alt + f13 did not resume tiling".to_owned(),
        )
    });

    steps.step("stop leaves the window visible", || {
        daemon.stop();
        wait_for(Duration::from_secs(5), || {
            infos(&windows).iter().all(|w| w.visible && !w.cloaked)
        })
        .map_err(|_| "the window stayed hidden after stop".to_owned())
    });

    drop(windows);
    let _ = std::fs::remove_dir_all(&scratch);
    steps.finish(&log);
}

// ---------------------------------------------------------------------------
// test eight: a long session, because the worst failure mode is a window that
// is gone and cannot be brought back
// ---------------------------------------------------------------------------

/// How many times the command sequence below is repeated.
const ROUNDS: usize = 4;

/// Every command a hotkey can send, in a fixed order, balanced so the
/// workspace ends in the same shape it started in: every toggle is undone,
/// every workspace switch comes back, every stack is unstacked.
fn round_trip() -> Vec<Command> {
    vec![
        Command::Focus {
            direction: Direction::Right,
        },
        Command::Focus {
            direction: Direction::Down,
        },
        Command::Move {
            direction: Direction::Left,
        },
        Command::Move {
            direction: Direction::Right,
        },
        Command::ResizeAxis {
            axis: Axis::Horizontal,
            sizing: Sizing::Increase,
        },
        Command::ResizeAxis {
            axis: Axis::Horizontal,
            sizing: Sizing::Decrease,
        },
        Command::CycleLayout {
            direction: CycleDirection::Next,
        },
        Command::CycleLayout {
            direction: CycleDirection::Previous,
        },
        Command::FlipLayout {
            axis: Axis::Horizontal,
        },
        Command::FlipLayout {
            axis: Axis::Horizontal,
        },
        Command::ToggleFloat,
        Command::ToggleFloat,
        Command::ToggleMonocle,
        Command::ToggleMonocle,
        Command::Stack {
            direction: Direction::Right,
        },
        Command::CycleStack {
            direction: CycleDirection::Next,
        },
        Command::Unstack,
        Command::FocusWorkspace { index: 1 },
        Command::FocusWorkspace { index: 0 },
        Command::TogglePause,
        Command::TogglePause,
        Command::Promote,
        Command::Retile,
    ]
}

#[test]
fn a_long_session_of_every_command_never_loses_a_window() {
    skip_unless_allowed!("a_long_session_of_every_command_never_loses_a_window");

    let mut daemon = Daemon::start("long-session");
    let log = daemon.log();
    let windows = TestWindows::spawn(6, 0).expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("the daemon adopts all six windows", || {
        wait_for(Duration::from_secs(10), || managed_count() == 6)
            .map_err(|_| format!("state shows {} windows", managed_count()))?;
        wait_for_tiling(&windows, 6).map(|_| ())
    });

    for round in 1..=ROUNDS {
        steps.step(&format!("round {round} of every command"), || {
            for cmd in round_trip() {
                let name = cmd.name().to_owned();
                command(&cmd).map_err(|e| format!("{name}: {e}"))?;
            }
            wait_for(STEP, || managed_count() == 6).map_err(|_| {
                format!(
                    "state shows {} windows after round {round}",
                    managed_count()
                )
            })
        });
    }

    steps.step("every window is still tiled and nothing overlaps", || {
        wait_until_still(&windows);
        let frames = wait_for_tiling(&windows, 6)?;
        let area = area().ok_or("no state to read the work area from")?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
        layout_assert::check_all_within(&frames, area.work_area).map_err(|v| v.to_string())
    });

    steps.step("no window is off screen and none is cloaked", || {
        let hidden: Vec<_> = infos(&windows)
            .into_iter()
            .filter(|w| off_screen(w) || w.cloaked || !w.visible)
            .map(|w| w.hwnd_hex())
            .collect();
        check(hidden.is_empty(), format!("still hidden: {hidden:?}"))
    });

    steps.step("the daemon logged no errors", || {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let errors: Vec<_> = text
            .lines()
            .filter(|line| line.contains("ERROR"))
            .take(5)
            .collect();
        check(errors.is_empty(), format!("{errors:#?}"))
    });

    steps.step("stop gives every window back", || {
        daemon.stop();
        wait_for(Duration::from_secs(5), || {
            infos(&windows)
                .iter()
                .all(|w| w.visible && !w.cloaked && !w.minimized && w.alpha.is_none())
        })
        .map_err(|_| {
            format!(
                "after stop: {:?}",
                infos(&windows)
                    .iter()
                    .map(|w| (w.hwnd_hex(), w.visible, w.cloaked, w.minimized, w.alpha))
                    .collect::<Vec<_>>()
            )
        })
    });

    drop(windows);
    steps.finish(&log);
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
