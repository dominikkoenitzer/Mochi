//! A portrait screen beside a landscape one, and monitors that come and go.
//!
//! The runner has one display, so these tests start the daemon on simulated
//! monitors (`--simulate-monitors`) with the geometry of the desk this is used
//! at: a 1080x1920 portrait screen right of a 3840x2160 landscape one. The
//! windows are real and really moved; only the list of monitors comes from a
//! file. Rewriting the file and posting `WM_DISPLAYCHANGE` to the daemon takes
//! the path a monitor being unplugged or reset takes.
//!
//! What the simulation cannot prove: anything Windows itself does when a
//! display changes (moving windows off a detached screen, the timing of a
//! driver reset), and mixed DPI, because every window on the runner is at 96
//! DPI and the simulated monitors say so too.

use std::path::Path;

use super::borders::{BORDER_SLACK, borders, rice, shown, wait_for_borders, write_json};
use super::*;

/// The class of the daemon's hidden message window, which hears display changes.
const EVENT_WINDOW_CLASS: &str = "MochiEventWindow";

/// The portrait screen of the desk, right of the landscape one, as Windows
/// reports it there.
const PORTRAIT: Rect = Rect::new(3840, 120, 4920, 2040);
/// The landscape screen of the desk.
const LANDSCAPE: Rect = Rect::new(0, 0, 3840, 2160);
/// The landscape screen without its taskbar.
const LANDSCAPE_WORK: Rect = Rect::new(0, 0, 3840, 2112);

// ---------------------------------------------------------------------------
// configuration
// ---------------------------------------------------------------------------

/// The desk's own configuration: the portrait screen is monitor 0 and stacks
/// its windows in rows, the landscape one is monitor 1 and uses BSP, and each
/// is pinned to its index by its rectangle.
fn desk_config() -> Value {
    let mut config = rice();
    let workspaces = |layout: &str| {
        serde_json::json!([
            { "name": "1", "layout": layout },
            { "name": "2", "layout": layout },
        ])
    };
    config["monitor_index_preferences"] = serde_json::json!({
        "0": rect_value(PORTRAIT),
        "1": rect_value(LANDSCAPE),
    });
    config["monitors"] = serde_json::json!([
        { "workspaces": workspaces("Rows") },
        { "workspaces": workspaces("BSP") },
    ]);
    config
}

fn rect_value(r: Rect) -> Value {
    serde_json::json!({ "left": r.left, "top": r.top, "right": r.right, "bottom": r.bottom })
}

/// One simulated monitor as `--simulate-monitors` reads it.
fn monitor(device: &str, size: Rect, work_area: Rect) -> Value {
    serde_json::json!({
        "device_name": device,
        "size": rect_value(size),
        "work_area": rect_value(work_area),
        "dpi": 96,
    })
}

fn portrait() -> Value {
    monitor(r"\\.\DISPLAY2", PORTRAIT, PORTRAIT)
}

fn landscape() -> Value {
    monitor(r"\\.\DISPLAY1", LANDSCAPE, LANDSCAPE_WORK)
}

/// Starts the daemon on the monitors in `monitors`, with `config`.
fn start_on(tag: &str, config: &Value, monitors: &Path) -> Daemon {
    let config = write_json(&format!("mochi-{tag}.json"), config);
    let monitors = monitors.to_str().expect("a UTF-8 temp path");
    Daemon::start_with_config(
        tag,
        &config,
        &["--no-hotkeys", "--simulate-monitors", monitors],
    )
}

// ---------------------------------------------------------------------------
// reading the desktop
// ---------------------------------------------------------------------------

/// The frames of the windows on screen inside `screen`.
fn frames_on(screen: Rect) -> Vec<Rect> {
    shown()
        .into_iter()
        .map(|w| w.frame)
        .filter(|f| {
            let (x, y) = (f.left + f.width() / 2, f.top + f.height() / 2);
            x >= screen.left && x < screen.right && y >= screen.top && y < screen.bottom
        })
        .collect()
}

/// Waits until `count` windows rest inside `screen`.
fn wait_for_tiling_on(screen: Rect, count: usize) -> Result<Vec<Rect>, String> {
    let mut last = Vec::new();
    let mut still = 0;
    let end = Instant::now() + STEP * 2;
    while Instant::now() < end {
        let now = frames_on(screen);
        if now.len() == count && now == last {
            still += 1;
            if still >= 3 {
                return Ok(now);
            }
        } else {
            still = 0;
        }
        last = now;
        std::thread::sleep(TICK);
    }
    Err(format!(
        "expected {count} windows resting on {screen}, saw {}: {last:?}",
        last.len()
    ))
}

/// The state's index of the monitor at `size`.
fn monitor_index(size: Rect) -> Option<usize> {
    state()?["monitors"]
        .as_array()?
        .iter()
        .position(|m| rect(&m["size"]) == Some(size))
}

/// How many monitors the daemon holds.
fn monitor_count() -> usize {
    state()
        .and_then(|s| s["monitors"].as_array().map(Vec::len))
        .unwrap_or(0)
}

/// The geometry of the focused workspace of one monitor.
fn area_on(monitor: usize) -> Option<Area> {
    let state = state()?;
    let monitor = state["monitors"].as_array()?.get(monitor)?.clone();
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

/// The geometry of the portrait screen's focused workspace, wherever it is in
/// the daemon's list.
fn portrait_area() -> Result<Area, String> {
    monitor_index(PORTRAIT)
        .and_then(area_on)
        .ok_or_else(|| "the daemon holds no monitor at the portrait rectangle".to_owned())
}

/// The layout of the focused workspace of one monitor.
fn layout_on(monitor: usize) -> Option<String> {
    let state = state()?;
    let monitor = &state["monitors"][monitor];
    let workspace = monitor["focused_workspace"].as_u64().unwrap_or(0) as usize;
    monitor["workspaces"][workspace]["layout"]
        .as_str()
        .map(str::to_owned)
}

/// The tiles are stacked top to bottom, each the full width of the area, all
/// of the same height, and together they fill it.
fn stacked_evenly(frames: &[Rect], area: &Area) -> Result<(), String> {
    let mut rows = frames.to_vec();
    rows.sort_by_key(|r| r.top);
    let edge = area.seam;
    for row in &rows {
        if (row.left - area.tiled.left).abs() > edge || (row.right - area.tiled.right).abs() > edge
        {
            return Err(format!(
                "{row} does not span the width of {}; the tiles are {rows:?}",
                area.tiled
            ));
        }
    }
    let heights: Vec<i32> = rows.iter().map(Rect::height).collect();
    let (low, high) = (
        heights.iter().min().copied().unwrap_or(0),
        heights.iter().max().copied().unwrap_or(0),
    );
    check(
        high - low <= 4,
        format!("the rows are {heights:?} high, not an even split; tiles {rows:?}"),
    )?;
    layout_assert::check_no_overlap(&rows).map_err(|v| v.to_string())?;
    check(
        close_enough(bounds(&rows), area.tiled, edge + 4),
        format!(
            "the rows span {} but the padded work area is {}",
            bounds(&rows),
            area.tiled
        ),
    )
}

/// `count` test windows placed inside `screen`, so the daemon puts them there.
fn spawn_on(screen: Rect, count: u32) -> TestWindows {
    TestWindows::spawn_with(&SpawnOptions {
        position: Some((screen.left + 60, screen.top + 60)),
        size: Some((600, 400)),
        ..SpawnOptions::new(count, 0)
    })
    .expect("could not spawn the test windows")
}

/// Rewrites the simulated monitors and tells the daemon the display changed.
fn change_monitors(daemon: u32, file: &Path, monitors: &[Value]) -> Result<(), String> {
    std::fs::write(file, Value::Array(monitors.to_vec()).to_string())
        .map_err(|e| format!("could not rewrite the monitors: {e}"))?;
    let window = mochi_testbed::windows_of_class(EVENT_WINDOW_CLASS)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|w| w.pid == daemon)
        .ok_or("the daemon has no message window")?;
    mochi_testbed::post_display_change(window.hwnd).map_err(|e| e.to_string())?;
    wait_for(STEP, || monitor_count() == monitors.len()).map_err(|_| {
        format!(
            "the daemon holds {} monitors after a change to {}",
            monitor_count(),
            monitors.len()
        )
    })
}

// ---------------------------------------------------------------------------
// the tests
// ---------------------------------------------------------------------------

#[test]
fn a_portrait_screen_stacks_two_and_three_windows_evenly() {
    skip_unless_allowed!("a_portrait_screen_stacks_two_and_three_windows_evenly");

    let file = write_json(
        "monitors-desk.json",
        &Value::Array(vec![portrait(), landscape()]),
    );
    let daemon = start_on("portrait-rows", &desk_config(), &file);
    let log = daemon.log();
    let pid = daemon.child.id();

    let mut steps = Steps::default();

    steps.step("the daemon sees the two simulated monitors", || {
        check(
            monitor_count() == 2,
            format!("the daemon holds {} monitors", monitor_count()),
        )?;
        let index = monitor_index(PORTRAIT).ok_or("no monitor at the portrait rectangle")?;
        check(
            layout_on(index).is_some_and(|l| l.eq_ignore_ascii_case("rows")),
            format!("the portrait screen uses {:?}, not Rows", layout_on(index)),
        )
    });

    let first = spawn_on(PORTRAIT, 2);

    steps.step(
        "two windows on the portrait screen split it 50/50, stacked",
        || {
            wait_for(Duration::from_secs(10), || managed_count() == 2)
                .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
            let frames = wait_for_tiling_on(PORTRAIT, 2)?;
            stacked_evenly(&frames, &portrait_area()?)
        },
    );

    let second = spawn_on(PORTRAIT, 1);

    steps.step("three windows split it in even thirds", || {
        wait_for(Duration::from_secs(10), || managed_count() == 3)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        let frames = wait_for_tiling_on(PORTRAIT, 3)?;
        stacked_evenly(&frames, &portrait_area()?)
    });

    steps.step("every window on the portrait screen has its border", || {
        wait_for_borders(pid, &[])
    });

    drop(second);
    drop(first);
    drop(daemon);
    steps.finish(&log);
}

#[test]
fn the_default_layout_stacks_two_windows_on_a_portrait_screen() {
    skip_unless_allowed!("the_default_layout_stacks_two_windows_on_a_portrait_screen");

    let file = write_json(
        "monitors-default.json",
        &Value::Array(vec![portrait(), landscape()]),
    );
    let daemon = start_on("portrait-default", &rice(), &file);
    let log = daemon.log();

    let mut steps = Steps::default();
    let windows = spawn_on(PORTRAIT, 2);

    steps.step(
        "two windows on the portrait screen split it 50/50, stacked",
        || {
            wait_for(Duration::from_secs(10), || managed_count() == 2)
                .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
            let index = monitor_index(PORTRAIT).ok_or("no monitor at the portrait rectangle")?;
            let frames = wait_for_tiling_on(PORTRAIT, 2)?;
            stacked_evenly(&frames, &area_on(index).ok_or("no portrait area")?)
        },
    );

    steps.step(
        "two windows on the landscape screen sit side by side",
        || {
            let landscape = spawn_on(LANDSCAPE_WORK, 2);
            wait_for(Duration::from_secs(10), || managed_count() == 4)
                .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
            let frames = wait_for_tiling_on(LANDSCAPE_WORK, 2)?;
            drop(landscape);
            let mut columns = frames;
            columns.sort_by_key(|r| r.left);
            check(
                columns[0].right <= columns[1].left
                    && (columns[0].width() - columns[1].width()).abs() <= 4,
                format!("the landscape tiles are {columns:?}, not two even columns"),
            )
        },
    );

    drop(windows);
    drop(daemon);
    steps.finish(&log);
}

#[test]
fn the_portrait_screen_keeps_its_layout_when_the_monitors_swap_order() {
    skip_unless_allowed!("the_portrait_screen_keeps_its_layout_when_the_monitors_swap_order");

    // The order Windows enumerates in after a driver reset or a cable pulled
    // and put back: the landscape screen first.
    let file = write_json(
        "monitors-swapped.json",
        &Value::Array(vec![landscape(), portrait()]),
    );
    let daemon = start_on("portrait-swapped", &desk_config(), &file);
    let log = daemon.log();

    let mut steps = Steps::default();

    steps.step(
        "the portrait screen still gets the rows its rectangle is pinned to",
        || {
            let index = monitor_index(PORTRAIT).ok_or("no monitor at the portrait rectangle")?;
            check(
                layout_on(index).is_some_and(|l| l.eq_ignore_ascii_case("rows")),
                format!("the portrait screen uses {:?}, not Rows", layout_on(index)),
            )
        },
    );

    let windows = spawn_on(PORTRAIT, 2);

    steps.step("two windows on it split it 50/50, stacked", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        let index = monitor_index(PORTRAIT).ok_or("no monitor at the portrait rectangle")?;
        let frames = wait_for_tiling_on(PORTRAIT, 2)?;
        stacked_evenly(&frames, &area_on(index).ok_or("no portrait area")?)
    });

    drop(windows);
    drop(daemon);
    steps.finish(&log);
}

#[test]
fn a_monitor_that_goes_and_comes_back_is_tiled_again() {
    skip_unless_allowed!("a_monitor_that_goes_and_comes_back_is_tiled_again");

    let file = write_json(
        "monitors-hotplug.json",
        &Value::Array(vec![portrait(), landscape()]),
    );
    let daemon = start_on("hotplug", &desk_config(), &file);
    let log = daemon.log();
    let pid = daemon.child.id();

    let tall = spawn_on(PORTRAIT, 2);
    let wide = spawn_on(LANDSCAPE_WORK, 2);

    let mut steps = Steps::default();

    steps.step("both screens are tiled", || {
        wait_for(Duration::from_secs(10), || managed_count() == 4)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        stacked_evenly(&wait_for_tiling_on(PORTRAIT, 2)?, &portrait_area()?)?;
        wait_for_tiling_on(LANDSCAPE_WORK, 2).map(|_| ())
    });

    steps.step(
        "unplugging the portrait screen moves its windows over",
        || {
            change_monitors(pid, &file, &[landscape()])?;
            let frames = wait_for_tiling_on(LANDSCAPE_WORK, 4)?;
            layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
            check(
                managed_count() == 4,
                format!("state shows {} windows", managed_count()),
            )
        },
    );

    steps.step("no border is left on the screen that went", || {
        wait_for(STEP, || {
            borders(pid)
                .iter()
                .all(|b| b.right <= PORTRAIT.left + BORDER_SLACK)
        })
        .map_err(|_| format!("borders still on the portrait screen: {:?}", borders(pid)))?;
        wait_for_borders(pid, &[])
    });

    steps.step(
        "plugging it back in, enumerated second, restores its layout",
        || {
            change_monitors(pid, &file, &[landscape(), portrait()])?;
            let index = monitor_index(PORTRAIT).ok_or("no monitor at the portrait rectangle")?;
            check(
                layout_on(index).is_some_and(|l| l.eq_ignore_ascii_case("rows")),
                format!(
                    "the portrait screen came back with {:?}, not Rows",
                    layout_on(index)
                ),
            )
        },
    );

    let late = spawn_on(PORTRAIT, 2);

    steps.step("new windows on the returned screen stack 50/50", || {
        wait_for(Duration::from_secs(10), || managed_count() == 6)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        let index = monitor_index(PORTRAIT).ok_or("no monitor at the portrait rectangle")?;
        let on_portrait = wait_for_tiling_on(PORTRAIT, frames_on(PORTRAIT).len().max(2))?;
        stacked_evenly(&on_portrait, &area_on(index).ok_or("no portrait area")?)
    });

    steps.step(
        "every screen is tiled without overlap and every border sits on a window",
        || {
            for screen in [PORTRAIT, LANDSCAPE_WORK] {
                layout_assert::check_no_overlap(&frames_on(screen)).map_err(|v| v.to_string())?;
            }
            wait_for_borders(pid, &[])
        },
    );

    drop(late);
    drop(wide);
    drop(tall);
    drop(daemon);
    steps.finish(&log);
}

#[test]
fn a_workspace_switch_on_one_monitor_leaves_the_other_alone() {
    skip_unless_allowed!("a_workspace_switch_on_one_monitor_leaves_the_other_alone");

    let file = write_json(
        "monitors-workspaces.json",
        &Value::Array(vec![portrait(), landscape()]),
    );
    let daemon = start_on("workspaces", &desk_config(), &file);
    let log = daemon.log();

    let tall = spawn_on(PORTRAIT, 2);
    let wide = spawn_on(LANDSCAPE_WORK, 2);

    let mut steps = Steps::default();

    steps.step("both screens are tiled", || {
        wait_for(Duration::from_secs(10), || managed_count() == 4)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        wait_for_tiling_on(PORTRAIT, 2)?;
        wait_for_tiling_on(LANDSCAPE_WORK, 2).map(|_| ())
    });

    let portrait_before = frames_on(PORTRAIT);

    steps.step(
        "workspace 2 on the landscape screen hides only its windows",
        || {
            let index = monitor_index(LANDSCAPE).ok_or("no landscape monitor")?;
            command(&Command::FocusMonitor { index })?;
            command(&Command::FocusWorkspace { index: 1 })?;
            wait_for(STEP, || infos(&wide).iter().all(off_screen))
                .map_err(|_| "the landscape windows stayed on screen".to_owned())?;
            check(
                infos(&tall).iter().all(|w| !off_screen(w)),
                "a portrait window went off screen".to_owned(),
            )?;
            check(
                frames_on(PORTRAIT) == portrait_before,
                format!(
                    "the portrait tiles moved from {portrait_before:?} to {:?}",
                    frames_on(PORTRAIT)
                ),
            )
        },
    );

    steps.step("workspace 1 brings them back", || {
        command(&Command::FocusWorkspace { index: 0 })?;
        wait_for(STEP, || infos(&wide).iter().all(|w| !off_screen(w)))
            .map_err(|_| "the landscape windows stayed off screen".to_owned())?;
        wait_for_tiling_on(LANDSCAPE_WORK, 2).map(|_| ())
    });

    steps.step(
        "a window moved to the portrait screen joins its rows",
        || {
            let index = monitor_index(LANDSCAPE).ok_or("no landscape monitor")?;
            command(&Command::FocusMonitor { index })?;
            let target = monitor_index(PORTRAIT).ok_or("no portrait monitor")?;
            command(&Command::MoveToMonitor { index: target })?;
            let frames = wait_for_tiling_on(PORTRAIT, 3)?;
            stacked_evenly(&frames, &area_on(target).ok_or("no portrait area")?)?;
            wait_for_tiling_on(LANDSCAPE_WORK, 1).map(|_| ())
        },
    );

    drop(wide);
    drop(tall);
    drop(daemon);
    steps.finish(&log);
}
