//! What the daemon costs, with limits a regression cannot pass.
//!
//! Four numbers, each measured on the runner against real windows and each
//! held to a ceiling:
//!
//! * CPU while nothing happens, over 30 seconds with four windows tiled.
//! * How long a new window takes from being created to resting in its tile.
//! * How long a retile of six windows takes, from the command to the last
//!   window in its new place.
//! * What a hundred windows opening and closing leave behind in the daemon:
//!   private memory, kernel handles, GDI and USER objects, border windows.
//!
//! Animations are off, so the times are the daemon's own and not the length of
//! an animation. The ceilings are two to five times what a runner measured on
//! the debug build (17.10.2026: idle CPU 0.000% of a core, open to tiled 27 ms
//! median and 29 ms slowest, a retile of six 13 ms and 14 ms, +112 KiB and no
//! object after a hundred windows): room for a noisy shared runner, and still
//! well below what a real regression costs. Every measurement is
//! printed and recorded, so the summary of each run shows the numbers as well
//! as the verdict.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    GR_GDIOBJECTS, GR_USEROBJECTS, GetGuiResources, GetProcessHandleCount, GetProcessTimes,
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

use super::borders::write_json;
use super::*;

/// Average CPU of the idle daemon, in percent of one core.
const IDLE_CPU_PERCENT: f64 = 0.2;
/// How long the idle daemon is watched.
const IDLE_WINDOW: Duration = Duration::from_secs(30);
/// From a window being created to resting in its tile, the median of the samples.
const OPEN_TO_TILED_MEDIAN: Duration = Duration::from_millis(60);
/// The same, the slowest sample.
const OPEN_TO_TILED_MAX: Duration = Duration::from_millis(150);
/// A retile of six windows, the median.
const RETILE_MEDIAN: Duration = Duration::from_millis(35);
/// The same, the slowest sample.
const RETILE_MAX: Duration = Duration::from_millis(100);
/// What a hundred windows that came and went may leave in private memory.
const CHURN_MEMORY_BYTES: i64 = 1024 * 1024;
/// And in kernel handles, GDI objects and USER objects, each.
const CHURN_OBJECTS: i64 = 4;

/// How many windows the open sample and the retile sample take.
const SAMPLES: usize = 12;

/// The test config without animations.
fn still_config() -> PathBuf {
    write_json(
        "mochi-performance.json",
        &serde_json::json!({
            "window_hiding_behaviour": "Cloak",
            "default_workspace_padding": 14,
            "default_container_padding": 10,
            "border": true,
            "border_width": 6,
            "border_offset": -1,
            "transparency": false,
            "animation": { "enabled": false }
        }),
    )
}

/// What the daemon holds at one moment.
#[derive(Debug, Clone, Copy)]
struct Usage {
    /// Kernel plus user time.
    cpu: Duration,
    private_bytes: i64,
    handles: i64,
    gdi: i64,
    user: i64,
}

/// Reads the daemon's usage through a process handle.
fn usage(pid: u32) -> Result<Usage, String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid)
            .map_err(|e| format!("OpenProcess: {e}"))?;
        let read = read_usage(process);
        let _ = CloseHandle(process);
        read
    }
}

unsafe fn read_usage(process: HANDLE) -> Result<Usage, String> {
    let filetime = |t: FILETIME| u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime);
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    unsafe {
        GetProcessTimes(
            process,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
        .map_err(|e| format!("GetProcessTimes: {e}"))?;
        let mut memory = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        GetProcessMemoryInfo(
            process,
            (&raw mut memory).cast::<PROCESS_MEMORY_COUNTERS>(),
            memory.cb,
        )
        .map_err(|e| format!("GetProcessMemoryInfo: {e}"))?;
        let mut handles = 0u32;
        GetProcessHandleCount(process, &raw mut handles)
            .map_err(|e| format!("GetProcessHandleCount: {e}"))?;
        Ok(Usage {
            // FILETIME counts in 100 ns.
            cpu: Duration::from_nanos((filetime(kernel) + filetime(user)) * 100),
            private_bytes: memory.PrivateUsage as i64,
            handles: i64::from(handles),
            gdi: i64::from(GetGuiResources(process, GR_GDIOBJECTS)),
            user: i64::from(GetGuiResources(process, GR_USEROBJECTS)),
        })
    }
}

/// Every border window the daemon owns, shown or kept for reuse.
fn border_windows(pid: u32) -> i64 {
    mochi_testbed::windows_of_class("MochiBorder")
        .unwrap_or_default()
        .iter()
        .filter(|b| b.pid == pid)
        .count() as i64
}

/// Prints a measurement and puts it in the run's list of outcomes.
fn measured(text: &str) {
    eprintln!("  measured {text}");
    super::record("measured", "measured", text, None);
}

/// The median and the maximum of a set of samples.
fn spread(samples: &mut [Duration]) -> (Duration, Duration) {
    samples.sort();
    (samples[samples.len() / 2], samples[samples.len() - 1])
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Every window of `batches` on screen, in one list.
fn all_frames(batches: &[&TestWindows]) -> Vec<Rect> {
    batches.iter().flat_map(|b| visible_frames(b)).collect()
}

/// Polls every millisecond until `done` holds; the elapsed
/// time since `start`, or `None` after `deadline`.
fn time_until(
    start: Instant,
    deadline: Duration,
    mut done: impl FnMut() -> bool,
) -> Option<Duration> {
    loop {
        if done() {
            return Some(start.elapsed());
        }
        if start.elapsed() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn the_daemon_stays_within_its_performance_budget() {
    skip_unless_allowed!("the_daemon_stays_within_its_performance_budget");

    let config = still_config();
    let daemon = Daemon::start_with_config("performance", &config, &["--no-hotkeys"]);
    let log = daemon.log();
    let pid = daemon.child.id();
    let base = TestWindows::spawn(4, 0).expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("four windows are tiled to start with", || {
        wait_for(Duration::from_secs(10), || managed_count() == 4)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        wait_for_tiling(&base, 4).map(|_| ())
    });

    steps.step(
        &format!("idle CPU over 30 s stays under {IDLE_CPU_PERCENT}% of a core"),
        || {
            // Let the start-up work finish before the clock starts.
            std::thread::sleep(Duration::from_secs(2));
            let before = usage(pid)?;
            let start = Instant::now();
            std::thread::sleep(IDLE_WINDOW);
            let after = usage(pid)?;
            let percent =
                (after.cpu - before.cpu).as_secs_f64() / start.elapsed().as_secs_f64() * 100.0;
            measured(&format!("idle CPU {percent:.3}% of a core"));
            check(
                percent < IDLE_CPU_PERCENT,
                format!("the idle daemon used {percent:.3}% of a core over 30 s"),
            )
        },
    );

    steps.step(
        &format!(
            "a new window rests in its tile within {} ms (median) and {} ms (slowest)",
            OPEN_TO_TILED_MEDIAN.as_millis(),
            OPEN_TO_TILED_MAX.as_millis()
        ),
        || {
            let mut samples = Vec::new();
            for _ in 0..SAMPLES {
                let start = Instant::now();
                let mut one = TestWindows::spawn(1, 0).map_err(|e| e.to_string())?;
                let hwnd = one.handles()[0];
                // The tile is asked for until the daemon has one, and then the
                // frame alone is watched, so the pipe is not hammered while
                // the daemon is doing the work being timed.
                let mut tile = None;
                let took = time_until(start, Duration::from_secs(3), || {
                    if tile.is_none() {
                        tile = super::tile_of(hwnd);
                    }
                    tile.zip(frame_of(&one, hwnd))
                        .is_some_and(|(tile, frame)| close_enough(tile, frame, 2))
                })
                .ok_or("a new window never reached its tile")?;
                check(
                    super::tile_of(hwnd) == tile,
                    "the tile changed while the window was moving into it".to_owned(),
                )?;
                samples.push(took);
                one.close_all().map_err(|e| e.to_string())?;
                wait_for(STEP, || managed_count() == 4).map_err(|e| e.to_string())?;
                wait_for_tiling(&base, 4)?;
            }
            let (median, max) = spread(&mut samples);
            measured(&format!(
                "open to tiled: median {:.1} ms, slowest {:.1} ms",
                ms(median),
                ms(max)
            ));
            check(
                median <= OPEN_TO_TILED_MEDIAN && max <= OPEN_TO_TILED_MAX,
                format!(
                    "open to tiled took a median {:.1} ms and at worst {:.1} ms",
                    ms(median),
                    ms(max)
                ),
            )
        },
    );

    let more = TestWindows::spawn(2, 0).expect("could not spawn two more windows");

    steps.step(
        &format!(
            "a retile of six windows lands within {} ms (median) and {} ms (slowest)",
            RETILE_MEDIAN.as_millis(),
            RETILE_MAX.as_millis()
        ),
        || {
            wait_for(Duration::from_secs(10), || managed_count() == 6)
                .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
            let batches = [&base, &more];
            wait_for(STEP, || all_frames(&batches).len() == 6).map_err(|e| e.to_string())?;
            wait_until_still(&base);
            let unflipped = all_frames(&batches);
            command(&Command::FlipLayout {
                axis: Axis::Horizontal,
            })?;
            wait_for(STEP, || all_frames(&batches) != unflipped).map_err(|e| e.to_string())?;
            wait_until_still(&base);
            wait_until_still(&more);
            let flipped = all_frames(&batches);
            // Every sample moves all six windows: a flip mirrors the layout.
            let mut samples = Vec::new();
            for round in 0..SAMPLES {
                let target = if round % 2 == 0 { &unflipped } else { &flipped };
                let start = Instant::now();
                command(&Command::FlipLayout {
                    axis: Axis::Horizontal,
                })?;
                let took = time_until(start, Duration::from_secs(3), || {
                    all_frames(&batches) == *target
                })
                .ok_or("the six windows never reached the flipped layout")?;
                samples.push(took);
                std::thread::sleep(Duration::from_millis(100));
            }
            let (median, max) = spread(&mut samples);
            measured(&format!(
                "retile of six: median {:.1} ms, slowest {:.1} ms",
                ms(median),
                ms(max)
            ));
            check(
                median <= RETILE_MEDIAN && max <= RETILE_MAX,
                format!(
                    "a retile of six took a median {:.1} ms and at worst {:.1} ms",
                    ms(median),
                    ms(max)
                ),
            )
        },
    );

    drop(more);

    steps.step(
        "a hundred windows opening and closing leave nothing behind in the daemon",
        || {
            wait_for(STEP, || managed_count() == 4).map_err(|e| e.to_string())?;
            let churn = |count: usize| -> Result<(), String> {
                for _ in 0..count / 5 {
                    let mut burst = TestWindows::spawn(5, 0).map_err(|e| e.to_string())?;
                    std::thread::sleep(Duration::from_millis(120));
                    burst.close_all().map_err(|e| e.to_string())?;
                }
                wait_for(Duration::from_secs(5), || managed_count() == 4)
                    .map_err(|_| format!("state shows {} windows after the churn", managed_count()))?;
                wait_for_tiling(&base, 4)?;
                std::thread::sleep(Duration::from_millis(500));
                Ok(())
            };
            // A first round fills whatever the daemon pools and caches, so the
            // second one measures what keeps growing.
            churn(20)?;
            let before = usage(pid)?;
            let before_borders = border_windows(pid);
            churn(100)?;
            let after = usage(pid)?;
            let grew = |a: i64, b: i64| b - a;
            let memory = grew(before.private_bytes, after.private_bytes);
            let handles = grew(before.handles, after.handles);
            let gdi = grew(before.gdi, after.gdi);
            let user = grew(before.user, after.user);
            let frames_kept = grew(before_borders, border_windows(pid));
            measured(&format!(
                "after 100 windows: private memory {:+} KiB, handles {handles:+}, GDI {gdi:+}, USER {user:+}, border windows {frames_kept:+}",
                memory / 1024
            ));
            check(
                memory <= CHURN_MEMORY_BYTES,
                format!("private memory grew by {} KiB", memory / 1024),
            )?;
            check(
                handles <= CHURN_OBJECTS && gdi <= CHURN_OBJECTS && user <= CHURN_OBJECTS,
                format!("handles grew by {handles}, GDI objects by {gdi}, USER objects by {user}"),
            )?;
            check(
                frames_kept <= 0,
                format!("the daemon holds {frames_kept} more border windows than before"),
            )
        },
    );

    drop(base);
    drop(daemon);
    steps.finish(&log);
}
