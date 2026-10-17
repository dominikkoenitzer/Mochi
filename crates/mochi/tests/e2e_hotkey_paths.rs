//! A bound key fires over an elevated window, and nothing got slower for it.
//!
//! Windows does not call a keyboard hook in a process that is not elevated
//! while an elevated window is in front, so Mochi also registers its bindings
//! with `RegisterHotKey` (see `events/hotkey.rs`). This file proves that on a
//! real desktop, and measures what it costs:
//!
//! 1. with the daemon and the window in front at the same integrity level, a
//!    press is heard by the hook, exactly once;
//! 2. with the daemon at medium integrity and an elevated window in front, the
//!    same press is heard by the registration instead, exactly once;
//! 3. a combination another program registered first is named in the log, and
//!    the hook still catches it whenever it can see the press;
//! 4. with `MOCHI_MEASURE=1`: idle CPU, press-to-command latency on both paths,
//!    and the daemon's own CPU per press, for any daemon binary named in
//!    `MOCHI_DAEMON_UNDER_TEST`, so a build of `main` can be measured by the
//!    same instrument as this one.
//!
//! The elevated case needs this process to be elevated, because it plays the
//! administrator window and starts the daemon a level below itself. CI runners
//! are; `MOCHI_E2E_ELEVATED=1` turns a missing elevation into a failure there
//! instead of a skip.
//!
//! ```text
//! set MOCHI_E2E=1
//! cargo test -p mochi --test e2e_hotkey_paths -- --nocapture --test-threads 1
//! ```
//!
//! Safety: only F13 to F16 are bound or pressed, keys no keyboard has. The
//! daemon manages only the testbed's window class and reads a scratch hotkey
//! file, never the user's own.

#![cfg(windows)]

use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as OsCommand, Stdio};
use std::time::{Duration, Instant};

use mochi_client::{Command, QueryTarget, Response, send};
use mochi_testbed::TestWindows;
use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, HWND};
use windows::Win32::Security::{
    DuplicateTokenEx, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, PSID,
    SECURITY_ATTRIBUTES, SID_AND_ATTRIBUTES, SecurityImpersonation, SetTokenInformation,
    TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TokenIntegrityLevel, TokenPrimary,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessAsUserW, GetCurrentProcess, OpenProcess, OpenProcessToken,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
    TerminateProcess, WaitForSingleObject,
};
use windows::Win32::System::WindowsProgramming::QueryProcessCycleTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, RegisterHotKey, SendInput, UnregisterHotKey, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{PCWSTR, PWSTR};

/// The keys this file binds and presses. Nothing else is ever injected.
mod keys {
    /// Bound to `toggle-pause`, free for the daemon to register.
    pub const F14: u16 = 0x7D;
    /// Bound to `toggle-pause` too, but registered by this test first.
    pub const F15: u16 = 0x7E;
    /// Bound to nothing: the press every other key on a keyboard is.
    pub const F16: u16 = 0x7F;
}

/// The integrity levels, as the last sub-authority of the label SID.
const MEDIUM: u32 = 0x2000;
const HIGH: u32 = 0x3000;

/// How long one press may take to show before it counts as not heard.
const HEARD: Duration = Duration::from_secs(2);

/// How long to watch for a press that should not be heard at all.
const UNHEARD: Duration = Duration::from_millis(800);

/// One poll tick for setup waits. Nothing timed sleeps.
const TICK: Duration = Duration::from_millis(40);

// ---------------------------------------------------------------------------
// gate
// ---------------------------------------------------------------------------

/// Why this file must not run, or `None` when it may.
fn allowed() -> Option<String> {
    if std::env::var("MOCHI_E2E").ok().as_deref() != Some("1") {
        return Some("MOCHI_E2E is not 1".to_owned());
    }
    if !mochi_testbed::monitors().is_ok_and(|m| !m.is_empty()) {
        return Some("no interactive desktop in this window station".to_owned());
    }
    if !daemon_binary().exists() {
        return Some(format!("{} does not exist", daemon_binary().display()));
    }
    None
}

macro_rules! skip_unless_allowed {
    ($name:expr) => {
        if let Some(reason) = allowed() {
            eprintln!("skipping {}: {reason}", $name);
            return;
        }
    };
}

/// The daemon under test: this build's, or the one `MOCHI_DAEMON_UNDER_TEST`
/// names, which is how CI measures `main` with this file's instrument.
fn daemon_binary() -> PathBuf {
    std::env::var_os("MOCHI_DAEMON_UNDER_TEST")
        .map_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mochi")), PathBuf::from)
}

fn temp_dir() -> PathBuf {
    let dir = PathBuf::from(std::env::var("TEMP").unwrap_or_else(|_| ".".to_owned()))
        .join("mochi-hotkey-paths");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn config_path() -> PathBuf {
    let path = temp_dir().join("mochi.json");
    std::fs::write(&path, "{\n  \"animation\": { \"enabled\": false }\n}\n")
        .expect("could not write the scratch config");
    path
}

fn hotkey_path() -> PathBuf {
    let path = temp_dir().join("hotkeys");
    std::fs::write(&path, "f14 : toggle-pause\nf15 : toggle-pause\n")
        .expect("could not write the hotkey file");
    path
}

// ---------------------------------------------------------------------------
// integrity levels
// ---------------------------------------------------------------------------

/// The integrity level of a token, as its label's last sub-authority.
fn token_integrity(token: HANDLE) -> Option<u32> {
    // u64s, not bytes: the label starts with a pointer and must be read from
    // an address aligned for one.
    let mut buffer = [0u64; 16];
    let mut length = 0u32;
    unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buffer.as_mut_ptr().cast()),
            u32::try_from(size_of_val(&buffer)).ok()?,
            &raw mut length,
        )
        .ok()?;
        let label = &*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        Some(*GetSidSubAuthority(
            label.Label.Sid,
            u32::from(count).checked_sub(1)?,
        ))
    }
}

/// The integrity level of a running process.
fn process_integrity(pid: u32) -> Option<u32> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &raw mut token);
        let _ = CloseHandle(process);
        opened.ok()?;
        let level = token_integrity(token);
        let _ = CloseHandle(token);
        level
    }
}

fn own_integrity() -> Option<u32> {
    process_integrity(std::process::id())
}

/// The process that owns the window in front.
fn foreground_pid() -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&raw mut pid)) };
    pid
}

// ---------------------------------------------------------------------------
// the daemon, at either level
// ---------------------------------------------------------------------------

/// A running daemon, stopped and reaped on drop.
struct Daemon {
    process: Running,
    log: PathBuf,
}

enum Running {
    /// Started the ordinary way, at this process's own level.
    Same(Child),
    /// Started below this process, with a lowered copy of its token.
    Medium(HANDLE),
}

impl Daemon {
    fn args() -> Vec<String> {
        vec![
            mochi_testbed::DAEMON_MANAGE_FLAG.to_owned(),
            mochi_testbed::TEST_WINDOW_CLASS.to_owned(),
            "--config".to_owned(),
            config_path().to_string_lossy().into_owned(),
            "--hotkeys".to_owned(),
            hotkey_path().to_string_lossy().into_owned(),
        ]
    }

    fn start(tag: &str, medium: bool) -> Daemon {
        let _ = send(&Command::Stop);
        let _ = wait_for(Duration::from_secs(5), || !mochi_client::is_running());

        let log = temp_dir().join(format!("mochi-{tag}.log"));
        let process = if medium {
            Running::Medium(spawn_at_medium(&daemon_binary(), &Self::args(), &log))
        } else {
            let file = std::fs::File::create(&log).expect("could not create the daemon log");
            let errors = file.try_clone().expect("could not clone the log handle");
            Running::Same(
                OsCommand::new(daemon_binary())
                    .args(Self::args())
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(file))
                    .stderr(Stdio::from(errors))
                    .spawn()
                    .expect("could not start the daemon"),
            )
        };
        let daemon = Daemon { process, log };
        wait_for(Duration::from_secs(10), mochi_client::is_running).unwrap_or_else(|_| {
            panic!(
                "the daemon never opened its pipe; log:\n{}",
                daemon.log_text()
            )
        });
        // The hotkeys are in force before the pipe opens, but give the log a
        // moment to land on disk.
        wait_for(Duration::from_secs(5), || {
            daemon.log_text().contains("hotkeys ready")
        })
        .unwrap_or_else(|_| panic!("the hotkeys never came up; log:\n{}", daemon.log_text()));
        daemon
    }

    fn handle(&self) -> HANDLE {
        match &self.process {
            Running::Same(child) => HANDLE(child.as_raw_handle()),
            Running::Medium(handle) => *handle,
        }
    }

    /// The daemon's console log, without the colour codes it writes there.
    fn log_text(&self) -> String {
        let raw = std::fs::read_to_string(&self.log).unwrap_or_default();
        let mut plain = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                // `ESC [ ... m`: skip through the final letter.
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                plain.push(c);
            }
        }
        plain
    }

    /// How many presses the daemon has carried out through one listener.
    fn heard(&self, via: &str) -> usize {
        let needle = format!("via=\"{via}\"");
        self.log_text()
            .lines()
            .filter(|line| line.contains("hotkey") && line.contains(&needle))
            .count()
    }

    /// CPU cycles the daemon has used so far, every thread, user and kernel.
    ///
    /// Cycles rather than `GetProcessTimes`, which only moves in whole timer
    /// ticks of 15.6 ms and reads one press as either nothing or a tick.
    fn cycles(&self) -> u64 {
        let mut cycles = 0u64;
        unsafe { QueryProcessCycleTime(self.handle(), &raw mut cycles) }
            .expect("QueryProcessCycleTime failed");
        cycles
    }

    fn stop(&mut self) {
        if mochi_client::is_running() {
            let _ = send(&Command::Stop);
        }
        let _ = wait_for(Duration::from_secs(10), || !mochi_client::is_running());
        match &mut self.process {
            Running::Same(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            Running::Medium(handle) => unsafe {
                if WaitForSingleObject(*handle, 5000).0 != 0 {
                    let _ = TerminateProcess(*handle, 1);
                }
            },
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
        if let Running::Medium(handle) = self.process {
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

/// Starts `exe` at medium integrity with its output in `log`.
///
/// A copy of this process's own token with the label lowered, which is what
/// an elevated program does to start something unelevated. No privilege is
/// needed to lower a token, only to raise one.
fn spawn_at_medium(exe: &Path, args: &[String], log: &Path) -> HANDLE {
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
            &raw mut own,
        )
        .expect("OpenProcessToken failed");
        let mut token = HANDLE::default();
        DuplicateTokenEx(
            own,
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &raw mut token,
        )
        .expect("DuplicateTokenEx failed");
        let _ = CloseHandle(own);

        // S-1-16-8192, the medium mandatory label: revision 1, one
        // sub-authority, the six byte authority 16, sub-authority 0x2000.
        // Held as u32s so the sub-authority sits where a SID reader expects
        // it, on a four byte boundary.
        let mut sid: [u32; 3] = [
            u32::from_le_bytes([1, 1, 0, 0]),
            u32::from_le_bytes([0, 0, 0, 16]),
            MEDIUM,
        ];
        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: PSID(sid.as_mut_ptr().cast()),
                Attributes: 0x20, // SE_GROUP_INTEGRITY
            },
        };
        SetTokenInformation(
            token,
            TokenIntegrityLevel,
            (&raw const label).cast(),
            u32::try_from(size_of::<TOKEN_MANDATORY_LABEL>() + size_of_val(&sid)).unwrap(),
        )
        .expect("could not lower the token to medium integrity");
        assert_eq!(
            token_integrity(token),
            Some(MEDIUM),
            "the token did not lower"
        );

        let inherit = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let path = wide(&log.to_string_lossy());
        let file = CreateFileW(
            PCWSTR(path.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&raw const inherit),
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .expect("could not create the daemon log");

        let startup = STARTUPINFOW {
            cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap(),
            dwFlags: STARTF_USESTDHANDLES,
            hStdOutput: file,
            hStdError: file,
            ..STARTUPINFOW::default()
        };
        let mut line = format!("\"{}\"", exe.display());
        for arg in args {
            line.push_str(&format!(" \"{arg}\""));
        }
        let mut line = wide(&line);
        let mut info = PROCESS_INFORMATION::default();
        CreateProcessAsUserW(
            Some(token),
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &raw const startup,
            &raw mut info,
        )
        .expect("could not start the daemon at medium integrity");
        let _ = CloseHandle(file);
        let _ = CloseHandle(token);
        let _ = CloseHandle(info.hThread);
        assert_eq!(
            process_integrity(info.dwProcessId),
            Some(MEDIUM),
            "the daemon did not start at medium integrity"
        );
        info.hProcess
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// talking to the daemon, pressing keys
// ---------------------------------------------------------------------------

fn paused() -> Option<bool> {
    match send(&Command::Query {
        target: QueryTarget::Paused,
    }) {
        Ok(Response::Query { answer }) => answer.as_bool(),
        _ => None,
    }
}

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

/// Spins on the pause flag until it reads `want`. A pipe round trip per turn
/// paces it; a sleep would be measuring the timer tick.
fn spin_until_paused_is(start: Instant, want: bool, deadline: Duration) -> Option<Duration> {
    while start.elapsed() < deadline {
        if paused() == Some(want) {
            return Some(start.elapsed());
        }
    }
    None
}

fn press(vk: u16) {
    let event = |up: bool| INPUT {
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
    };
    let inputs = [event(false), event(true)];
    let sent = unsafe { SendInput(&inputs, i32::try_from(size_of::<INPUT>()).unwrap()) };
    assert_eq!(sent as usize, inputs.len(), "the desktop refused the press");
}

/// Presses `vk` and says whether the pause flag flipped, and how fast.
fn toggle_with(vk: u16, deadline: Duration) -> Option<Duration> {
    let before = paused().expect("the daemon stopped answering");
    let start = Instant::now();
    press(vk);
    spin_until_paused_is(start, !before, deadline)
}

/// Puts an elevated test window in front, and proves it is.
fn elevated_window_in_front(windows: &TestWindows) {
    let hwnd = windows.spawned()[0].hwnd;
    wait_for(Duration::from_secs(5), || {
        let _ = mochi_testbed::focus_window(hwnd);
        mochi_testbed::foreground_window() == hwnd
    })
    .expect("the test window would not come to the front");
    assert_eq!(
        process_integrity(foreground_pid()),
        Some(HIGH),
        "the window in front is not elevated, so this proves nothing"
    );
}

/// Registers F15 for this thread, the way another program would.
struct Squatter;

impl Squatter {
    const ID: i32 = 0x0F15;

    fn take() -> Squatter {
        unsafe {
            RegisterHotKey(
                None::<HWND>,
                Self::ID,
                HOT_KEY_MODIFIERS(0),
                u32::from(keys::F15),
            )
        }
        .expect("F15 was already registered before the test took it");
        Squatter
    }
}

impl Drop for Squatter {
    fn drop(&mut self) {
        let _ = unsafe { UnregisterHotKey(None, Self::ID) };
    }
}

// ---------------------------------------------------------------------------
// the proof
// ---------------------------------------------------------------------------

#[test]
fn a_bound_key_fires_over_an_elevated_window_exactly_once() {
    skip_unless_allowed!("a_bound_key_fires_over_an_elevated_window_exactly_once");
    if own_integrity() != Some(HIGH) {
        assert!(
            std::env::var("MOCHI_E2E_ELEVATED").ok().as_deref() != Some("1"),
            "MOCHI_E2E_ELEVATED=1 but this process is not elevated"
        );
        eprintln!("skipping: the elevated case needs an elevated test process");
        return;
    }

    let windows = TestWindows::spawn(1, 0).expect("could not spawn the test window");
    let squatter = Squatter::take();
    let mut steps = 0;

    // 1. Same level as the window in front: the hook hears it, once.
    {
        let mut daemon = Daemon::start("same", false);
        assert!(
            daemon.log_text().contains("another program has registered"),
            "the key another program holds was not named in the log:\n{}",
            daemon.log_text()
        );
        steps += 1;

        elevated_window_in_front(&windows);
        assert!(
            toggle_with(keys::F14, HEARD).is_some(),
            "F14 did nothing with the daemon at the window's own level"
        );
        // A second listener firing too would have flipped it back already.
        std::thread::sleep(UNHEARD);
        assert_eq!(daemon.heard("hook"), 1, "log:\n{}", daemon.log_text());
        assert_eq!(daemon.heard("registered"), 0, "log:\n{}", daemon.log_text());
        steps += 1;

        // The key another program registered: the hook still has it.
        assert!(
            toggle_with(keys::F15, HEARD).is_some(),
            "the hook did not fall back for the key another program holds"
        );
        assert_eq!(daemon.heard("hook"), 2, "log:\n{}", daemon.log_text());
        steps += 1;
        daemon.stop();
    }

    // 2. What Windows itself hands a medium process, with no Mochi involved.
    let delivered = what_windows_delivers(&windows);
    steps += 1;

    // 3. Medium daemon, elevated window in front: Mochi hears exactly what the
    //    control heard, through the registration and never the hook.
    {
        let mut daemon = Daemon::start("medium", true);
        elevated_window_in_front(&windows);
        let fired = toggle_with(keys::F14, HEARD).is_some();
        std::thread::sleep(UNHEARD);
        eprintln!(
            "registration over an elevated window: {}; Mochi fired: {fired}",
            if delivered {
                "delivered"
            } else {
                "not delivered"
            }
        );
        assert_eq!(
            fired,
            delivered,
            "Mochi and the control disagree about F14 over an elevated window. log:\n{}",
            daemon.log_text()
        );
        assert_eq!(
            daemon.heard("registered"),
            usize::from(delivered),
            "log:\n{}",
            daemon.log_text()
        );
        assert_eq!(
            daemon.heard("hook"),
            0,
            "the hook heard a press over an elevated window, so the window was \
             not what this test thinks it was. log:\n{}",
            daemon.log_text()
        );
        steps += 1;

        // The key another program holds is the documented limit: over an
        // elevated window nothing of Mochi's hears it.
        assert!(
            toggle_with(keys::F15, UNHEARD).is_none(),
            "F15 fired over an elevated window although another program holds it"
        );
        steps += 1;
        daemon.stop();
    }

    drop(squatter);
    eprintln!("hotkey paths proven: {steps} steps passed");
}

// ---------------------------------------------------------------------------
// the measurement
// ---------------------------------------------------------------------------

/// Samples of one measurement.
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

    fn take(&mut self, sample: Option<Duration>) {
        match sample {
            Some(value) => self.values.push(value),
            None => self.lost += 1,
        }
    }

    fn print(&self) {
        let mut sorted = self.values.clone();
        sorted.sort_unstable();
        if sorted.is_empty() {
            eprintln!("  {:<44} never fired ({} lost)", self.label, self.lost);
            return;
        }
        let at = |q: f64| {
            let index = ((sorted.len() - 1) as f64 * q).round() as usize;
            sorted[index].as_secs_f64() * 1000.0
        };
        eprintln!(
            "  {:<44} n={:<3} median {:>7.3} ms   p90 {:>7.3} ms   max {:>7.3} ms{}",
            self.label,
            sorted.len(),
            at(0.5),
            at(0.9),
            at(1.0),
            if self.lost > 0 {
                format!("   ({} lost)", self.lost)
            } else {
                String::new()
            }
        );
    }
}

const SAMPLES: usize = 40;
const IDLE: Duration = Duration::from_secs(30);
const UNBOUND_PRESSES: usize = 400;

/// Idle CPU and the daemon's own cost per press, for one daemon.
fn measure_cpu(daemon: &Daemon, label: &str, bound: u16) {
    // Idle: nothing pressed, nothing asked, for half a minute.
    std::thread::sleep(Duration::from_secs(3));
    let before = daemon.cycles();
    std::thread::sleep(IDLE);
    let idle = daemon.cycles() - before;
    eprintln!(
        "  {label}: idle {:.3} Mcycles over {}s",
        idle as f64 / 1e6,
        IDLE.as_secs()
    );

    // A key nobody bound, which is what nearly every press on a desktop is.
    let before = daemon.cycles();
    for _ in 0..UNBOUND_PRESSES {
        press(keys::F16);
    }
    std::thread::sleep(Duration::from_millis(500));
    let unbound = daemon.cycles() - before;
    eprintln!(
        "  {label}: an unbound press costs the daemon {:.1} kcycles ({UNBOUND_PRESSES} presses)",
        unbound as f64 / 1e3 / UNBOUND_PRESSES as f64
    );

    // A bound key, carried out, with nobody polling the daemon meanwhile.
    let before = daemon.cycles();
    for _ in 0..SAMPLES {
        press(bound);
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(500));
    let fired = daemon.cycles() - before;
    eprintln!(
        "  {label}: a bound press costs the daemon {:.1} kcycles, command included ({SAMPLES} presses)",
        fired as f64 / 1e3 / SAMPLES as f64
    );
    // An even count of toggles leaves the flag where it was.
    let _ = wait_for(Duration::from_secs(2), || paused() == Some(false));
    if paused() == Some(true) {
        let _ = send(&Command::TogglePause);
    }
}

fn measure_latency(label: &str) {
    for _ in 0..8 {
        let _ = paused();
    }
    let mut floor = Samples::new(&format!("{label}: one pipe round trip (floor)"));
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let answered = paused().is_some();
        floor.take(answered.then(|| start.elapsed()));
    }
    let mut latency = Samples::new(&format!("{label}: press to command carried out"));
    for _ in 0..SAMPLES {
        latency.take(toggle_with(keys::F14, HEARD));
        std::thread::sleep(Duration::from_millis(40));
    }
    if paused() == Some(true) {
        let _ = send(&Command::TogglePause);
    }
    floor.print();
    latency.print();
}

#[test]
fn what_each_path_costs() {
    skip_unless_allowed!("what_each_path_costs");
    if std::env::var("MOCHI_MEASURE").ok().as_deref() != Some("1") {
        eprintln!("skipping what_each_path_costs: MOCHI_MEASURE is not 1");
        return;
    }
    let which = std::env::var("MOCHI_DAEMON_LABEL").unwrap_or_else(|_| "this build".to_owned());
    let windows = TestWindows::spawn(1, 0).expect("could not spawn the test window");
    eprintln!();
    eprintln!("measuring {which}: {}", daemon_binary().display());

    {
        let mut daemon = Daemon::start("measure-hook", false);
        elevated_window_in_front(&windows);
        measure_latency("hook path");
        measure_cpu(&daemon, "hook path", keys::F14);
        daemon.stop();
    }
    if own_integrity() == Some(HIGH) {
        let mut daemon = Daemon::start("measure-registered", true);
        elevated_window_in_front(&windows);
        measure_latency("elevated window in front");
        measure_cpu(&daemon, "elevated window in front", keys::F14);
        daemon.stop();
    } else {
        eprintln!("  elevated window in front: not measured, this process is not elevated");
    }
    eprintln!();
}

// ---------------------------------------------------------------------------
// what Windows itself delivers, with no Mochi involved
// ---------------------------------------------------------------------------

/// The key the control helper listens for. Nothing of Mochi's binds it here.
const F13: u16 = 0x7C;

/// Set in the helper's environment to make [`control_helper`] run.
const HELPER_ENV: &str = "MOCHI_HOTKEY_PATHS_HELPER";

/// A medium integrity process with a window, a keyboard hook that only
/// listens, and a registration for F13. It reports each one hearing F13 on
/// stdout, which the parent redirected to a file. Not a test of its own: it
/// returns at once unless the parent started it.
#[test]
fn control_helper() {
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, CreateWindowExW, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, MSG,
        PostThreadMessageW, SetWindowsHookExW, TranslateMessage, WH_KEYBOARD_LL, WINDOW_EX_STYLE,
        WM_HOTKEY, WM_KEYDOWN, WM_QUIT, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows::core::w;

    if std::env::var(HELPER_ENV).ok().as_deref() != Some("1") {
        return;
    }

    unsafe extern "system" fn listen(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == 0 && wparam.0 as u32 == WM_KEYDOWN {
            let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if event.vkCode == u32::from(F13) {
                println!("heard by the hook");
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    unsafe {
        let registered = RegisterHotKey(None, 0x13, HOT_KEY_MODIFIERS(0), u32::from(F13));
        let instance = GetModuleHandleW(None).expect("no module handle");
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(listen), Some(instance.into()), 0);
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            w!("hotkey paths control"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            400,
            200,
            None,
            None,
            None,
            None,
        )
        .expect("no window");
        println!(
            "ready hwnd={} registered={} hook={} integrity={:?}",
            window.0 as i64,
            registered.is_ok(),
            hook.is_ok(),
            own_integrity()
        );

        let thread = GetCurrentThreadId();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0));
        });
        let mut message = MSG::default();
        while GetMessageW(&raw mut message, None, 0, 0).0 > 0 {
            if message.message == WM_HOTKEY {
                println!("heard by the registration");
            }
            let _ = TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}

/// What the control helper heard so far: (hook, registration).
fn control_heard(log: &Path) -> (usize, usize) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    (
        text.matches("heard by the hook").count(),
        text.matches("heard by the registration").count(),
    )
}

/// Runs the control: F13 pressed once with the helper's own medium window in
/// front and once with an elevated window in front. Returns what the
/// registration heard over the elevated window, after printing the matrix.
fn what_windows_delivers(windows: &TestWindows) -> bool {
    let log = temp_dir().join("control.log");
    let exe = std::env::current_exe().expect("no test binary path");
    // Inherited by the helper through CreateProcessAsUserW's default
    // environment. Nothing else in this process reads it.
    unsafe { std::env::set_var(HELPER_ENV, "1") };
    let helper = spawn_at_medium(
        &exe,
        &[
            "control_helper".to_owned(),
            "--exact".to_owned(),
            "--nocapture".to_owned(),
            "--test-threads".to_owned(),
            "1".to_owned(),
        ],
        &log,
    );
    unsafe { std::env::remove_var(HELPER_ENV) };

    let mut hwnd = 0i64;
    wait_for(Duration::from_secs(10), || {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        hwnd = text
            .split("ready hwnd=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        hwnd != 0
    })
    .unwrap_or_else(|_| {
        panic!(
            "the control helper never came up:\n{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        )
    });

    // Its own medium window in front.
    wait_for(Duration::from_secs(5), || {
        let _ = mochi_testbed::focus_window(hwnd);
        mochi_testbed::foreground_window() == hwnd
    })
    .expect("the control window would not come to the front");
    assert_eq!(process_integrity(foreground_pid()), Some(MEDIUM));
    press(F13);
    std::thread::sleep(UNHEARD);
    let medium = control_heard(&log);

    // An elevated window in front.
    elevated_window_in_front(windows);
    press(F13);
    std::thread::sleep(UNHEARD);
    let after = control_heard(&log);
    let elevated = (after.0 - medium.0, after.1 - medium.1);

    unsafe {
        if WaitForSingleObject(helper, 0).0 != 0 {
            let _ = TerminateProcess(helper, 0);
        }
        let _ = CloseHandle(helper);
    }

    eprintln!("what Windows hands a medium process for one injected F13:");
    eprintln!(
        "  medium window in front:   hook {}   registration {}",
        medium.0, medium.1
    );
    eprintln!(
        "  elevated window in front: hook {}   registration {}",
        elevated.0, elevated.1
    );
    assert_eq!(
        medium,
        (1, 1),
        "the control did not hear F13 over its own window, so the instrument is broken:\n{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    elevated.1 == 1
}
