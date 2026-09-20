//! What Mochi's architecture costs, measured against the real desktop.
//!
//! Mochi binds hotkeys inside the daemon: the key press and the command it runs
//! happen in one process. The arrangement it replaces spawns a process for
//! every single press, and that process then opens a pipe to say one sentence.
//! This file measures both, so the difference is a number rather than a claim.
//!
//! Three measurements, all printed, almost none of them asserted:
//!
//! 1. a bound key press to the command being carried out, in process;
//! 2. the same command on the same daemon, sent by starting a process;
//! 3. a retile, from the command landing to every window being placed, for four
//!    windows and then eight.
//!
//! ```text
//! set MOCHI_E2E=1
//! set MOCHI_LATENCY=1
//! cargo test -p mochi --test e2e_latency -- --nocapture --test-threads 1
//! ```
//!
//! Safety: the daemon is started with `--manage-class MochiTestWindow` and a
//! scratch hotkey file that binds one key, so it touches no window this file did
//! not spawn and no key a keyboard can produce. Only F13 is ever injected: it
//! exists in the virtual key table, on no keyboard sold this century, and in no
//! layout, which is the only reason injecting keys into a desktop somebody is
//! sitting at is acceptable.

#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Child, Command as OsCommand, Stdio};
use std::time::{Duration, Instant};

use mochi_client::{Command, QueryTarget, Response, send};
use mochi_testbed::{Rect, TestWindows};

/// How many samples each measurement takes. One sample of anything that
/// involves the desktop is noise, so nothing here is reported from fewer.
const SAMPLES: usize = 24;

/// How long one sample may take before it is written off as lost. Generous: a
/// lost sample means the instrument failed, not that the daemon was slow.
const SAMPLE_DEADLINE: Duration = Duration::from_secs(2);

/// How long the window frames must hold still before a retile counts as over.
const QUIET: Duration = Duration::from_millis(120);

/// How long to keep watching the frames of one retile before giving up on
/// seeing them move. Short, because the answer is usually "they did not".
const MOVEMENT_DEADLINE: Duration = Duration::from_millis(800);

/// A breath between samples, so one measurement does not land on the tail of
/// the one before it. Never inside a timed section.
const BETWEEN: Duration = Duration::from_millis(40);

/// One poll tick for the setup waits. Nothing timed ever sleeps.
const TICK: Duration = Duration::from_millis(40);

// ---------------------------------------------------------------------------
// gate
// ---------------------------------------------------------------------------

/// Why this file must not run, or `None` when it may.
///
/// Two opt-ins, not one. `MOCHI_E2E` says real windows are allowed; a
/// measurement wants the machine to itself as well, and a measurement that ran
/// by accident on a build server would put a made-up number on somebody's pull
/// request.
fn allowed() -> Option<String> {
    if std::env::var("MOCHI_E2E").ok().as_deref() != Some("1") {
        return Some("MOCHI_E2E is not 1".to_owned());
    }
    if std::env::var("MOCHI_LATENCY").ok().as_deref() != Some("1") {
        return Some("MOCHI_LATENCY is not 1".to_owned());
    }
    if !mochi_testbed::monitors().is_ok_and(|m| !m.is_empty()) {
        return Some("no interactive desktop in this window station".to_owned());
    }
    if daemon_binary().is_none() {
        return Some("the mochi binary was not built".to_owned());
    }
    if client_binary().is_none() {
        return Some("mochic was not built; run cargo build -p mochic first".to_owned());
    }
    None
}

/// Prints the skip reason and leaves the test when it must not run.
macro_rules! skip_unless_allowed {
    ($name:expr) => {
        if let Some(reason) = allowed() {
            eprintln!("skipping {}: {reason}", $name);
            eprintln!(
                "  run it with MOCHI_E2E=1 MOCHI_LATENCY=1 on an interactive Windows desktop,"
            );
            eprintln!("  with mochic built, and -- --nocapture --test-threads 1");
            return;
        }
    };
}

/// The daemon cargo built for this test run.
fn daemon_binary() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_mochi"));
    path.exists().then_some(path)
}

/// The command line client.
///
/// It belongs to another package, so cargo sets no `CARGO_BIN_EXE_` for it, but
/// it lands in the same directory as the daemon.
fn client_binary() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_mochi")).with_file_name("mochic.exe");
    path.exists().then_some(path)
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
    /// Starts the daemon managing only the testbed class, logging to `%TEMP%`.
    ///
    /// `extra` carries either `--no-hotkeys` or the scratch hotkey file. The
    /// user's own hotkey file is never read: the daemon would bind the keys the
    /// person at this desk is using.
    fn start(tag: &str, extra: &[&str]) -> Daemon {
        // A daemon left over from an earlier run holds the single instance
        // mutex, and the new one would exit at once.
        let _ = send(&Command::Stop);
        let _ = wait_for(Duration::from_secs(5), || !mochi_client::is_running());

        let log = temp_dir().join(format!("mochi-{tag}.log"));
        let file = std::fs::File::create(&log).expect("could not create the daemon log");
        let errors = file.try_clone().expect("could not clone the log handle");

        let child = OsCommand::new(daemon_binary().expect("no mochi binary"))
            .arg(mochi_testbed::DAEMON_MANAGE_FLAG)
            .arg(mochi_testbed::TEST_WINDOW_CLASS)
            .arg("--config")
            .arg(config_path())
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file))
            .stderr(Stdio::from(errors))
            .spawn()
            .expect("could not start the daemon");

        let daemon = Daemon { child, log };
        wait_for(Duration::from_secs(10), mochi_client::is_running).unwrap_or_else(|_| {
            panic!(
                "the daemon never opened its pipe; log: {}",
                daemon.log.display()
            )
        });
        daemon
    }

    /// Asks the daemon to stop and waits for the pipe to go away.
    fn stop(&mut self) {
        if mochi_client::is_running() {
            let _ = send(&Command::Stop);
        }
        let _ = wait_for(Duration::from_secs(10), || !mochi_client::is_running());
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
        let _ = self.child.kill();
    }
}

/// Where the scratch config, the hotkey file and the logs live. Never inside
/// the repository.
fn temp_dir() -> PathBuf {
    let dir = PathBuf::from(std::env::var("TEMP").unwrap_or_else(|_| ".".to_owned()))
        .join("mochi-latency");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A minimal config of this file's own, so the numbers do not depend on
/// whatever the person at this desk has in theirs. No animation in particular:
/// an animated move finishes long after the daemon has placed the window.
fn config_path() -> PathBuf {
    let path = temp_dir().join("mochi.json");
    std::fs::write(
        &path,
        concat!(
            "{\n",
            "  \"window_hiding_behaviour\": \"Cloak\",\n",
            "  \"default_workspace_padding\": 14,\n",
            "  \"default_container_padding\": 10,\n",
            "  \"animation\": { \"enabled\": false }\n",
            "}\n"
        ),
    )
    .expect("could not write the scratch config");
    path
}

// ---------------------------------------------------------------------------
// talking to the daemon
// ---------------------------------------------------------------------------

/// The pause flag, read with the cheapest question the protocol takes.
///
/// `state` would serialise every monitor, workspace and window on every poll,
/// and the poll loop is the instrument's own resolution limit.
fn paused() -> Option<bool> {
    match send(&Command::Query {
        target: QueryTarget::Paused,
    }) {
        Ok(Response::Query { answer }) => answer.as_bool(),
        _ => None,
    }
}

/// How many windows the model holds.
fn managed_count() -> u64 {
    match send(&Command::Query {
        target: QueryTarget::WindowCount,
    }) {
        Ok(Response::Query { answer }) => answer.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// Polls `check` until it is true or the deadline passes. Setup only.
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

/// Spins on the pause flag until it reads `want`, and says how long that took
/// from `start`.
///
/// No sleep anywhere in here. `thread::sleep` on Windows rounds up to the
/// system timer tick, which is coarser than most of what this file measures, so
/// a polite poll loop would be measuring the politeness. Each turn of the loop
/// is a pipe round trip, which paces it well enough on its own.
fn spin_until_paused_is(start: Instant, want: bool) -> Option<Duration> {
    while start.elapsed() < SAMPLE_DEADLINE {
        if paused() == Some(want) {
            return Some(start.elapsed());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// the key press
// ---------------------------------------------------------------------------

/// The keys this file presses.
mod keys {
    /// `VK_F13`. The only key injected anywhere in this file.
    pub const F13: u16 = 0x7C;
}

/// Presses a key and returns immediately.
///
/// The `tap` in `e2e_testbed.rs` sleeps afterwards so that the assertion behind
/// it cannot read the daemon too early. Sleeping here would be measuring the
/// sleep, so this returns the moment the desktop has taken the input and the
/// caller polls for the effect instead.
fn press(vk: u16) {
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

    let inputs = [event(vk, false), event(vk, true)];
    let size = std::mem::size_of::<INPUT>() as i32;
    let sent = unsafe { SendInput(&inputs, size) } as usize;
    assert_eq!(
        sent,
        inputs.len(),
        "the desktop refused an injected key press"
    );
}

// ---------------------------------------------------------------------------
// statistics
// ---------------------------------------------------------------------------

/// The samples of one measurement, and the ones that never arrived.
struct Samples {
    label: String,
    values: Vec<Duration>,
    lost: usize,
}

impl Samples {
    fn new(label: &str) -> Samples {
        Samples {
            label: label.to_owned(),
            values: Vec::new(),
            lost: 0,
        }
    }

    fn push(&mut self, taken: Duration) {
        self.values.push(taken);
    }

    fn lose(&mut self) {
        self.lost += 1;
    }

    /// Min, median and max, or `None` when every sample was lost.
    ///
    /// The median of an even count is the upper of the two middle samples. The
    /// mean is deliberately absent: one scheduling hiccup moves it, and it
    /// would be the number people quote.
    fn summary(&self) -> Option<(Duration, Duration, Duration)> {
        if self.values.is_empty() {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_unstable();
        Some((
            sorted[0],
            sorted[sorted.len() / 2],
            sorted[sorted.len() - 1],
        ))
    }

    fn median(&self) -> Option<Duration> {
        self.summary().map(|(_, median, _)| median)
    }

    /// One line, aligned with the others.
    fn print(&self) {
        match self.summary() {
            Some((min, median, max)) => eprintln!(
                "  {:<46} n={:<3} min {:>8.2} ms   median {:>8.2} ms   max {:>8.2} ms{}",
                self.label,
                self.values.len(),
                ms(min),
                ms(median),
                ms(max),
                if self.lost > 0 {
                    format!("   ({} lost)", self.lost)
                } else {
                    String::new()
                }
            ),
            None => eprintln!(
                "  {:<46} no samples landed ({} lost)",
                self.label, self.lost
            ),
        }
    }
}

/// A duration in milliseconds, for printing.
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

// ---------------------------------------------------------------------------
// measurement one and two: the same command, two architectures
// ---------------------------------------------------------------------------

#[test]
fn a_bound_key_carries_out_a_command_faster_than_starting_a_process_for_it() {
    skip_unless_allowed!("a_bound_key_carries_out_a_command_faster_than_starting_a_process_for_it");

    // One binding, on a key nothing else on this desktop can produce.
    // `toggle-pause` is the command to measure with because its whole effect is
    // a boolean the daemon hands back on request, so the moment it has been
    // carried out is observable without looking at a single window.
    let file = temp_dir().join("hotkeys");
    std::fs::write(&file, "f13 : toggle-pause\n").expect("could not write the hotkey file");

    let mut daemon = Daemon::start("latency", &["--hotkeys", &file.to_string_lossy()]);
    let client = client_binary().expect("no mochic binary");

    // The first pipe connection and the first sample pay for pages that are not
    // resident yet and for a hook thread that has not run. Neither is what
    // anybody wants to read off this.
    for _ in 0..8 {
        let _ = paused();
    }

    // The instrument's own floor. Every number below is observed by polling the
    // daemon over the pipe, so nothing here can resolve anything faster than
    // one round trip.
    let mut floor = Samples::new("one pipe round trip (the instrument's floor)");
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let answered = paused().is_some();
        let taken = start.elapsed();
        if answered {
            floor.push(taken);
        } else {
            floor.lose();
        }
    }

    let mut in_process = Samples::new("bound key press to command carried out");
    let mut subprocess = Samples::new("same command, started as a process");

    // The two measurements alternate rather than running one after the other:
    // whatever the desktop is doing while this runs then lands on both of them
    // instead of on whichever went second.
    for _ in 0..SAMPLES {
        let before = paused().expect("the daemon stopped answering");
        let start = Instant::now();
        press(keys::F13);
        match spin_until_paused_is(start, !before) {
            Some(taken) => in_process.push(taken),
            None => in_process.lose(),
        }
        std::thread::sleep(BETWEEN);

        let before = paused().expect("the daemon stopped answering");
        let start = Instant::now();
        // Identical work on an identical daemon. The only difference is that a
        // process has to be created, linked and run before the pipe is opened,
        // which is what an out of process hotkey setup pays on every press.
        let mut child = OsCommand::new(&client)
            .arg("toggle-pause")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("could not start mochic");
        let taken = spin_until_paused_is(start, !before);
        let _ = child.wait();
        match taken {
            Some(taken) => subprocess.push(taken),
            None => subprocess.lose(),
        }
        std::thread::sleep(BETWEEN);
    }

    // Leave the daemon the way it was found, whatever the samples did.
    if paused() == Some(true) {
        let _ = send(&Command::TogglePause);
    }
    daemon.stop();
    let _ = std::fs::remove_file(&file);

    eprintln!();
    eprintln!("one command, two architectures:");
    floor.print();
    in_process.print();
    subprocess.print();
    if let (Some(fast), Some(slow)) = (in_process.median(), subprocess.median()) {
        eprintln!(
            "  starting a process costs {:.2} ms more per key press, {:.1}x the in process path",
            ms(slow) - ms(fast),
            ms(slow) / ms(fast).max(f64::MIN_POSITIVE)
        );
    }
    eprintln!();

    // The only thing worth asserting. The numbers themselves are an instrument
    // reading, not a gate, but if binding a key in process were ever slower
    // than starting a program to do the same thing, something is badly wrong.
    let fast = in_process
        .median()
        .expect("every in process sample was lost, so the instrument is broken");
    let slow = subprocess
        .median()
        .expect("every subprocess sample was lost, so the instrument is broken");
    assert!(
        fast <= slow,
        "the in process path took {:.2} ms and starting a process took {:.2} ms",
        ms(fast),
        ms(slow)
    );
}

// ---------------------------------------------------------------------------
// measurement three: a retile
// ---------------------------------------------------------------------------

/// The frames of every window in these batches that is on screen.
fn frames(batches: &[&TestWindows]) -> Vec<Rect> {
    batches
        .iter()
        .flat_map(|b| b.windows())
        .filter(|w| w.visible && !w.cloaked && !w.minimized)
        .map(|w| w.frame)
        .collect()
}

/// Waits until the frames stop changing, so a sample never starts halfway
/// through a layout. Setup only, and it sleeps, which is why nothing timed uses
/// it.
fn wait_until_still(batches: &[&TestWindows]) {
    let mut last = frames(batches);
    let end = Instant::now() + Duration::from_secs(3);
    let mut stable = 0;
    while Instant::now() < end {
        std::thread::sleep(TICK);
        let now = frames(batches);
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

/// Spins on the frames and reports when the last one moved, measured from
/// `start`, or `None` when none of them ever did.
///
/// This is `wait_until_still`'s idea with the sleep taken out and a clock put
/// in: stillness is still "reads that agree for a while", but what gets
/// reported is the moment of the last change rather than the moment stillness
/// was proved, because the proof costs a fixed `QUIET` that has nothing to do
/// with the daemon.
fn last_frame_movement(start: Instant, batches: &[&TestWindows]) -> Option<Duration> {
    let mut last = frames(batches);
    let mut moved_at = None;
    let mut quiet_since = Instant::now();
    while start.elapsed() < MOVEMENT_DEADLINE {
        let now = frames(batches);
        if now == last {
            if quiet_since.elapsed() >= QUIET {
                return moved_at;
            }
        } else {
            last = now;
            moved_at = Some(start.elapsed());
            quiet_since = Instant::now();
        }
    }
    moved_at
}

/// One retile measurement: the command, and what the windows did.
struct Retile {
    /// The pipe round trip. The daemon computes the layout and places every
    /// window before it answers, so this is the command landing to the last
    /// window position going out.
    command: Samples,
    /// When the last frame moved, for the samples in which any frame moved.
    movement: Samples,
    /// How many of the samples moved a window at all.
    moved: usize,
}

/// Issues `retile` `SAMPLES` times and times both the command and the frames.
fn measure_retile(label: &str, batches: &[&TestWindows]) -> Retile {
    let mut out = Retile {
        command: Samples::new(&format!("{label}: command in to every window placed")),
        movement: Samples::new(&format!("{label}: until the last frame moved")),
        moved: 0,
    };

    for _ in 0..SAMPLES {
        wait_until_still(batches);
        let start = Instant::now();
        let answer = send(&Command::Retile);
        let taken = start.elapsed();
        match answer {
            Ok(Response::Ok) => out.command.push(taken),
            _ => out.command.lose(),
        }
        if let Some(moved_at) = last_frame_movement(start, batches) {
            out.moved += 1;
            out.movement.push(moved_at);
        }
        std::thread::sleep(BETWEEN);
    }
    out
}

#[test]
fn a_retile_places_four_and_then_eight_windows() {
    skip_unless_allowed!("a_retile_places_four_and_then_eight_windows");

    let mut daemon = Daemon::start("retile", &["--no-hotkeys"]);

    let first = TestWindows::spawn(4, 0).expect("could not spawn the first four windows");
    wait_for(Duration::from_secs(10), || managed_count() == 4)
        .unwrap_or_else(|_| panic!("the daemon adopted {} of 4 windows", managed_count()));
    wait_until_still(&[&first]);
    let four = measure_retile("4 windows", &[&first]);

    let second = TestWindows::spawn(4, 0).expect("could not spawn the second four windows");
    wait_for(Duration::from_secs(10), || managed_count() == 8)
        .unwrap_or_else(|_| panic!("the daemon adopted {} of 8 windows", managed_count()));
    wait_until_still(&[&first, &second]);
    let eight = measure_retile("8 windows", &[&first, &second]);

    // Down in the reverse order of coming up, and before anything is printed,
    // so a panic in the reporting still leaves a clean desktop.
    daemon.stop();
    drop(second);
    drop(first);

    eprintln!();
    eprintln!("retile:");
    for measured in [&four, &eight] {
        measured.command.print();
        if measured.moved == 0 {
            eprintln!(
                "  {:<46} no frame moved in any of {SAMPLES} samples: a retile of a \
                 workspace that is already tiled puts every window back where it was",
                measured.movement.label
            );
        } else {
            measured.movement.print();
            eprintln!(
                "  (a frame moved in {} of {SAMPLES} samples)",
                measured.moved
            );
        }
    }
    eprintln!();
}
