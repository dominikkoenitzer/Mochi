//! A window that outranks the daemon.
//!
//! The runner runs elevated, so the daemon is started a level lower, at the
//! medium integrity an ordinary program has, and the test's own window is the
//! elevated one. That is the desk's case the other way round: an
//! administrator terminal next to an unelevated Mochi. Windows refuses a lower
//! process every move of a higher one's window, so today's behaviour is to
//! leave it alone and tile everything else as if it were not there.

use super::borders::{no_border_at, rice, testwin_binary, wait_for_borders, write_json};
use super::*;

#[test]
fn an_elevated_window_is_left_alone_and_the_rest_tile_around_it() {
    skip_unless_allowed!("an_elevated_window_is_left_alone_and_the_rest_tile_around_it");
    if !integrity::current_is_high() {
        eprintln!(
            "skipping an_elevated_window_is_left_alone_and_the_rest_tile_around_it: \
             this process is not elevated, so nothing it opens outranks a daemon it starts"
        );
        return;
    }

    let config = write_json("mochi-elevated.json", &rice());
    let lowered =
        integrity::Lowered::start("elevated", &config).expect("could not start a lowered daemon");
    let log = lowered.log.display().to_string();
    let pid = lowered.daemon_pid();
    // The test process is elevated, so its own window is the one that
    // outranks the daemon; the host started at the daemon's level holds the
    // two ordinary ones.
    let admin = TestWindows::spawn_with(&SpawnOptions {
        title_prefix: "Admin".to_owned(),
        ..SpawnOptions::new(1, 0)
    })
    .expect("could not spawn the elevated window");
    let admin_hwnd = admin.handles()[0];
    let admin_rect = mochi_testbed::window_rect(admin_hwnd).expect("no rect");
    let host = lowered
        .spawn_windows(2)
        .expect("could not start the ordinary windows");

    let mut steps = Steps::default();

    steps.step("the daemon runs below the test", || {
        let level = pid.and_then(integrity::of_process);
        check(
            level.is_some_and(|l| l < integrity::HIGH),
            format!("the daemon runs at integrity {level:?}"),
        )
    });

    steps.step("the two ordinary windows are tiled", || {
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        wait_for(STEP, || host.windows().len() == 2).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(600));
        let frames: Vec<Rect> = host.windows().iter().map(|w| w.frame).collect();
        let area = area().ok_or("no state to read the work area from")?;
        layout_assert::check_no_overlap(&frames).map_err(|v| v.to_string())?;
        check(
            close_enough(bounds(&frames), area.tiled, area.seam + 4),
            format!(
                "the two tiles span {} but the padded work area is {}: a hole is kept for the elevated window",
                bounds(&frames),
                area.tiled
            ),
        )
    });

    steps.step("the elevated window is not managed", || {
        let held = state()
            .map(|s| s.to_string().contains(&format!("\"hwnd\":{admin_hwnd}")))
            .unwrap_or(false);
        check(!held, "the model holds the elevated window".to_owned())
    });

    steps.step("the elevated window stays where it was", || {
        let now = mochi_testbed::window_rect(admin_hwnd).map_err(|e| e.to_string())?;
        check(
            now == admin_rect,
            format!("the elevated window moved from {admin_rect} to {now}"),
        )
    });

    steps.step("the elevated window gets no border", || {
        let frame = mochi_testbed::frame_bounds(admin_hwnd).map_err(|e| e.to_string())?;
        let daemon = pid.ok_or("no daemon pid")?;
        no_border_at(daemon, frame)?;
        wait_for_borders(daemon, &[admin_hwnd])
    });

    drop(host);
    drop(admin);
    drop(lowered);
    steps.finish(&log);
}

/// What the daemon says about its own rights: `state.elevated`.
fn rights() -> Option<(bool, bool)> {
    let state = state()?;
    let elevated = &state["elevated"];
    Some((
        elevated["configured"].as_bool()?,
        elevated["running"].as_bool()?,
    ))
}

/// The daemon's findings about itself.
fn doctor() -> Option<Value> {
    match send(&Command::Doctor) {
        Ok(Response::Doctor { doctor }) => Some(doctor),
        _ => None,
    }
}

/// What the daemon makes of the window in front.
fn why() -> Option<Value> {
    match send(&Command::Why) {
        Ok(Response::Why { why }) => Some(why),
        _ => None,
    }
}

fn findings_of(doctor: &Value, kind: &str) -> usize {
    doctor["findings"]
        .as_array()
        .map_or(0, |all| all.iter().filter(|f| f["kind"] == kind).count())
}

/// The setting on, with the daemon started the way `mochic start` leaves it
/// after an accepted prompt: at this process's rights. On the runner, which is
/// elevated, that is the case the setting exists for: the test's own windows
/// run as administrator, and the daemon reaches them and hears the keys
/// pressed over them. Started unelevated, the same daemon has to say that the
/// file asks for rights it lacks, and still tile what it can reach.
#[test]
fn with_the_setting_on_the_windows_of_an_administrator_are_tiled_and_keep_their_keys() {
    skip_unless_allowed!(
        "with_the_setting_on_the_windows_of_an_administrator_are_tiled_and_keep_their_keys"
    );
    let high = integrity::current_is_high();

    let mut on = rice();
    on["elevated"] = Value::Bool(true);
    let config = write_json("mochi-elevated-on.json", &on);
    let scratch = temp_dir().join("elevated-hotkeys");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("could not make the hotkey scratch directory");
    let keys = scratch.join("hotkeys");
    std::fs::write(&keys, ".shell cmd\n\nf13 : toggle-pause\n")
        .expect("could not write the hotkey file");

    let daemon = Daemon::start_with_config(
        "elevated-on",
        &config,
        &["--hotkeys", &keys.to_string_lossy()],
    );
    let log = daemon.log();
    // Opened by this process, so they run at its rights: as administrator on
    // the runner, which is exactly what an unelevated Mochi cannot reach.
    let windows = TestWindows::spawn_with(&SpawnOptions {
        title_prefix: "Admin".to_owned(),
        ..SpawnOptions::new(2, 0)
    })
    .expect("could not spawn the test windows");

    let mut steps = Steps::default();

    steps.step("the daemon reports the setting and its own rights", || {
        let read = wait_some(STEP, rights).ok_or("state carries no elevated block")?;
        check(
            read == (true, high),
            format!("state says configured/running {read:?}, expected (true, {high})"),
        )
    });

    steps.step("both windows are tiled", || {
        // Each window the daemon adopts takes the foreground, so wait until it holds both.
        wait_for(Duration::from_secs(10), || managed_count() == 2)
            .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
        wait_for_tiling(&windows, 2).map(|_| ())
    });

    steps.step(
        "the doctor names the mismatch only when there is one",
        || {
            let doctor = doctor().ok_or("no doctor answer")?;
            check(
                doctor["elevated"] == Value::Bool(high),
                format!(
                    "doctor says elevated {}, the process is {high}",
                    doctor["elevated"]
                ),
            )?;
            let not_elevated = findings_of(&doctor, "not-elevated");
            check(
                not_elevated == usize::from(!high),
                format!(
                    "{not_elevated} not-elevated findings, expected {}",
                    usize::from(!high)
                ),
            )?;
            check(
                findings_of(&doctor, "hotkeys-blocked") == 0,
                format!("a window is reported out of the keyboard's reach: {doctor}"),
            )
        },
    );

    let front = windows.handles()[0];
    steps.step(
        "the window in front is managed and its keys are not withheld",
        || {
            mochi_testbed::bring_to_front(front, STEP)
                .map_err(|e| format!("the test window never came to the front: {e}"))?;
            let why = why().ok_or("no why answer")?;
            check(
                why["managed"] == Value::Bool(true),
                format!("why says {why}"),
            )?;
            check(
                why.get("hotkeys_blocked").is_none(),
                format!("the keys pressed over the window are withheld: {why}"),
            )
        },
    );

    steps.step("a key pressed over that window reaches the daemon", || {
        check(paused() == Some(false), "the daemon came up paused")?;
        tap(keys::F13, None);
        check(
            paused() == Some(true),
            "F13 over the window did not pause tiling".to_owned(),
        )?;
        tap(keys::F13, None);
        check(
            paused() == Some(false),
            "the second F13 did not resume tiling".to_owned(),
        )
    });

    drop(windows);
    drop(daemon);
    steps.finish(&log);
}

/// The setting off, by default and spelled out: nothing about the daemon's
/// rights changes, and the doctor has nothing to say about them.
#[test]
fn the_elevated_setting_off_leaves_the_daemon_as_it_was() {
    skip_unless_allowed!("the_elevated_setting_off_leaves_the_daemon_as_it_was");
    let high = integrity::current_is_high();
    let mut off = rice();
    off["elevated"] = Value::Bool(false);
    let explicit = write_json("mochi-elevated-off.json", &off);

    let mut steps = Steps::default();
    let mut logs = Vec::new();
    for (name, config) in [("by default", config_path()), ("spelled out", explicit)] {
        let daemon = Daemon::start_with_config(
            &format!("elevated-off-{}", logs.len()),
            &config,
            &["--no-hotkeys"],
        );
        logs.push(daemon.log());
        let windows = TestWindows::spawn(2, 0).expect("could not spawn the test windows");

        steps.step(
            &format!("{name}: the daemon reports the setting off"),
            || {
                let read = wait_some(STEP, rights).ok_or("state carries no elevated block")?;
                check(
                    read == (false, high),
                    format!("state says configured/running {read:?}, expected (false, {high})"),
                )
            },
        );
        steps.step(
            &format!("{name}: the doctor finds nothing about rights"),
            || {
                let doctor = doctor().ok_or("no doctor answer")?;
                check(
                    findings_of(&doctor, "not-elevated") == 0,
                    format!("a not-elevated finding with the setting off: {doctor}"),
                )
            },
        );
        steps.step(&format!("{name}: both windows are tiled"), || {
            wait_for(Duration::from_secs(10), || managed_count() == 2)
                .map_err(|_| format!("state shows {} managed windows", managed_count()))?;
            wait_for_tiling(&windows, 2).map(|_| ())
        });
        drop(windows);
        drop(daemon);
    }
    steps.finish(&logs.join(" and "));
}

/// Integrity levels, and a daemon started one below this process.
mod integrity {
    use std::ffi::c_void;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use mochi_testbed::TestWindowInfo;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        CreateWellKnownSid, DuplicateTokenEx, GetSidSubAuthority, GetSidSubAuthorityCount,
        GetTokenInformation, PSID, SID_AND_ATTRIBUTES, SecurityImpersonation, SetTokenInformation,
        TOKEN_ACCESS_MASK, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, TokenPrimary, WinMediumLabelSid,
    };
    use windows::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateProcessAsUserW, GetCurrentProcess, OpenProcess, OpenProcessToken,
        PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
    };
    use windows::core::{PCWSTR, PWSTR};

    /// `SECURITY_MANDATORY_HIGH_RID`, an elevated process.
    pub const HIGH: u32 = 0x3000;
    /// `SE_GROUP_INTEGRITY`.
    const GROUP_INTEGRITY: u32 = 0x20;

    /// The integrity level of a token.
    fn of_token(token: HANDLE) -> Option<u32> {
        // u64s, so the label read out of it is aligned for its pointer.
        let mut buffer = [0u64; 16];
        let mut length = 0u32;
        unsafe {
            GetTokenInformation(
                token,
                TokenIntegrityLevel,
                Some(buffer.as_mut_ptr().cast::<c_void>()),
                size_of_val(&buffer) as u32,
                &raw mut length,
            )
            .ok()?;
            let label = &*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
            let count = *GetSidSubAuthorityCount(label.Label.Sid);
            Some(*GetSidSubAuthority(label.Label.Sid, u32::from(count) - 1))
        }
    }

    /// The integrity level of a running process.
    pub fn of_process(pid: u32) -> Option<u32> {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut token = HANDLE::default();
            let opened = OpenProcessToken(process, TOKEN_QUERY, &raw mut token);
            let _ = CloseHandle(process);
            opened.ok()?;
            let level = of_token(token);
            let _ = CloseHandle(token);
            level
        }
    }

    /// Whether this process runs elevated.
    pub fn current_is_high() -> bool {
        of_process(std::process::id()).is_some_and(|l| l >= HIGH)
    }

    /// Runs `command_line` with this process's token lowered to medium
    /// integrity, which is where an unelevated program runs.
    fn spawn_medium(command_line: &str) -> Result<u32, String> {
        unsafe {
            let mut own = HANDLE::default();
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
                &raw mut own,
            )
            .map_err(|e| format!("OpenProcessToken: {e}"))?;
            let mut token = HANDLE::default();
            let duplicated = DuplicateTokenEx(
                own,
                TOKEN_ACCESS_MASK(0x000F_01FF),
                None,
                SecurityImpersonation,
                TokenPrimary,
                &raw mut token,
            );
            let _ = CloseHandle(own);
            duplicated.map_err(|e| format!("DuplicateTokenEx: {e}"))?;

            let mut sid = [0u8; 68];
            let mut size = sid.len() as u32;
            let sid_ptr = PSID(sid.as_mut_ptr().cast::<c_void>());
            CreateWellKnownSid(WinMediumLabelSid, None, Some(sid_ptr), &raw mut size)
                .map_err(|e| format!("CreateWellKnownSid: {e}"))?;
            let label = TOKEN_MANDATORY_LABEL {
                Label: SID_AND_ATTRIBUTES {
                    Sid: sid_ptr,
                    Attributes: GROUP_INTEGRITY,
                },
            };
            SetTokenInformation(
                token,
                TokenIntegrityLevel,
                (&raw const label).cast::<c_void>(),
                (size_of::<TOKEN_MANDATORY_LABEL>() as u32) + size,
            )
            .map_err(|e| format!("SetTokenInformation: {e}"))?;

            let mut line: Vec<u16> = command_line.encode_utf16().chain(Some(0)).collect();
            let startup = STARTUPINFOW {
                cb: size_of::<STARTUPINFOW>() as u32,
                ..Default::default()
            };
            let mut info = PROCESS_INFORMATION::default();
            let created = CreateProcessAsUserW(
                Some(token),
                PCWSTR::null(),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW,
                None,
                PCWSTR::null(),
                &raw const startup,
                &raw mut info,
            );
            let _ = CloseHandle(token);
            created.map_err(|e| format!("CreateProcessAsUserW: {e}"))?;
            let _ = CloseHandle(info.hThread);
            let _ = CloseHandle(info.hProcess);
            Ok(info.dwProcessId)
        }
    }

    /// Ends a process tree by pid.
    fn end_tree(pid: u32) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// A daemon at medium integrity, run through `cmd` so its output still
    /// reaches a log file.
    pub struct Lowered {
        shell: u32,
        pub log: PathBuf,
    }

    impl Lowered {
        pub fn start(tag: &str, config: &Path) -> Result<Lowered, String> {
            let _ = mochi_client::send(&mochi_client::Command::Stop);
            super::wait_for(super::STEP, || !mochi_client::is_running())?;
            let log = super::temp_dir().join(format!("mochi-{tag}.log"));
            let daemon = super::daemon_binary().ok_or("no mochi binary")?;
            let line = format!(
                "cmd.exe /d /c \"set RUST_LOG=debug&& \"{}\" {} {} --config \"{}\" --no-hotkeys > \"{}\" 2>&1\"",
                daemon.display(),
                mochi_testbed::DAEMON_MANAGE_FLAG,
                mochi_testbed::TEST_WINDOW_CLASS,
                config.display(),
                log.display()
            );
            let shell = spawn_medium(&line)?;
            let lowered = Lowered { shell, log };
            super::wait_for(Duration::from_secs(30), mochi_client::is_running).map_err(|_| {
                format!(
                    "the lowered daemon never opened its pipe; log: {}",
                    lowered.log.display()
                )
            })?;
            Ok(lowered)
        }

        /// The pid of the daemon itself, the child of the shell.
        pub fn daemon_pid(&self) -> Option<u32> {
            let name = super::daemon_binary()?;
            let name = name.file_name()?.to_str()?;
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("IMAGENAME eq {name}"), "/FO", "CSV", "/NH"])
                .output()
                .ok()?;
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.split(',').nth(1))
                .filter_map(|pid| pid.trim_matches('"').parse::<u32>().ok())
                .find(|&pid| of_process(pid).is_some_and(|l| l < HIGH))
        }

        /// `count` test windows in a `mochi-testwin` host at the daemon's level.
        pub fn spawn_windows(&self, count: u32) -> Result<Host, String> {
            let exe = super::testwin_binary()?;
            let line = format!(
                "\"{}\" spawn --count {count} --title-prefix Plain",
                exe.display()
            );
            let pid = spawn_medium(&line)?;
            let host = Host { pid };
            super::wait_for(Duration::from_secs(20), || {
                host.windows().len() == count as usize
            })
            .map_err(|_| "the lowered host never showed its windows".to_owned())?;
            Ok(host)
        }
    }

    impl Drop for Lowered {
        fn drop(&mut self) {
            if mochi_client::is_running() {
                let _ = mochi_client::send(&mochi_client::Command::Stop);
                let _ = super::wait_for(Duration::from_secs(10), || !mochi_client::is_running());
            }
            end_tree(self.shell);
        }
    }

    /// A `mochi-testwin` host started at medium integrity.
    pub struct Host {
        pid: u32,
    }

    impl Host {
        /// Its windows, read off the desktop.
        pub fn windows(&self) -> Vec<TestWindowInfo> {
            mochi_testbed::list_windows()
                .unwrap_or_default()
                .into_iter()
                .filter(|w| w.pid == self.pid && w.visible)
                .collect()
        }
    }

    impl Drop for Host {
        fn drop(&mut self) {
            end_tree(self.pid);
        }
    }
}
