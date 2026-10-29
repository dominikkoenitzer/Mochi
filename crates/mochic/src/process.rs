//! Starting and stopping the daemon and the hotkey daemon.

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};

/// `DETACHED_PROCESS`: the child gets no console and survives this one.
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP`: Ctrl-C in this console does not reach the child.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// How long `start` waits for the daemon to open its pipe.
const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Finds an executable next to `mochic` first, then on `PATH`.
///
/// The sibling lookup matters during development: a freshly built `mochic`
/// must start the `mochi` it was built with, not an older one on `PATH`.
pub fn find_executable(stem: &str) -> Option<PathBuf> {
    let file = format!("{stem}.exe");

    if let Ok(current) = std::env::current_exe()
        && let Some(dir) = current.parent()
    {
        let sibling = dir.join(&file);
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// Spawns a process detached from this console.
///
/// The handle is returned rather than just the pid, because `start_daemon` has
/// to be able to ask whether this daemon is still alive.
fn spawn_detached(program: &PathBuf, args: &[String]) -> Result<Child> {
    use std::os::windows::process::CommandExt;

    Command::new(program)
        .args(args)
        // `DETACHED_PROCESS` denies the child a console but not the handles it
        // inherits, so without this the daemon would write its whole tracing
        // output down this terminal's stdout and stderr for the rest of its
        // life, and `mochic start > log.txt` would never finish because the
        // daemon still holds the write end of that file.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .with_context(|| format!("could not start {}", program.display()))
}

/// The outside world `start` works with, so the tests can stand in for it
/// without a single `mochi` being started.
pub trait Launch {
    /// True when a daemon answers on the control pipe.
    fn daemon_is_running(&self) -> bool;
    /// Starts the daemon detached and waits until it answers on its pipe.
    fn daemon(&self, args: &[String]) -> Result<()>;
    /// Runs `mochi` on this console and returns once it has exited.
    fn attached(&self, args: &[String]) -> Result<ExitStatus>;
    /// True when this process runs with an elevated token.
    fn is_elevated(&self) -> bool;
    /// Starts the daemon as administrator through the Windows prompt and waits
    /// until it answers on its pipe, or reports that the prompt was declined.
    fn daemon_as_administrator(&self, args: &[String]) -> Result<Administrator>;
}

/// How a start as administrator ended, short of an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Administrator {
    /// The prompt was accepted and the daemon answers on its pipe.
    Started,
    /// The prompt was declined; nothing was started.
    Declined,
}

/// The real processes and the real pipe.
pub struct System;

impl Launch for System {
    fn daemon_is_running(&self) -> bool {
        mochi_client::is_running()
    }

    fn daemon(&self, args: &[String]) -> Result<()> {
        start_daemon(args)
    }

    fn is_elevated(&self) -> bool {
        win::is_elevated()
    }

    fn daemon_as_administrator(&self, args: &[String]) -> Result<Administrator> {
        start_daemon_as_administrator(args)
    }

    fn attached(&self, args: &[String]) -> Result<ExitStatus> {
        let exe = mochi_executable()?;
        // Same console, same output, same process group: the user reads the
        // dry run where they typed it, and Ctrl-C or closing the window reaches
        // it as well as this process.
        run_attached(
            &win::Console,
            || {
                Command::new(&exe)
                    .args(args)
                    .spawn()
                    .with_context(|| format!("could not start {}", exe.display()))
            },
            |mut child| {
                let pid = child.id();
                child
                    .wait()
                    .with_context(|| format!("could not wait for mochi (pid {pid})"))
            },
        )
    }
}

/// Whether Ctrl-C ends this process. A seam, so the tests can see when it
/// changes without a console of their own.
trait CtrlC {
    fn set_ignored(&self, ignored: bool);
}

/// Starts a program on this console and returns once it has exited.
///
/// Ctrl-C reaches every process on the console, so it used to end this one at
/// once and give the prompt back while the child still wrote its last lines.
/// This process ignores it from the spawn to the end of the wait, failed or
/// not. Never before the spawn: the child inherits the setting, and Ctrl-C
/// could not stop it. Ctrl-Break and closing the window still end both.
fn run_attached<C>(
    ctrl_c: &impl CtrlC,
    spawn: impl FnOnce() -> Result<C>,
    wait: impl FnOnce(C) -> Result<ExitStatus>,
) -> Result<ExitStatus> {
    let child = spawn()?;
    ctrl_c.set_ignored(true);
    let status = wait(child);
    ctrl_c.set_ignored(false);
    status
}

/// `mochic start`: the daemon, or with `dry_run` an observer on this console.
///
/// A dry run is never started detached. It opens no control pipe, so a
/// detached one could not be reached by anything: `start_daemon` waited for a
/// pipe that never came, reported a failure, and left the observer running
/// with no console, no pipe and no keys until somebody found it in Task
/// Manager. Run on this console it prints what it would do right here, Ctrl-C
/// ends it, and `start` returns only after it has exited. It also does not
/// look for a running daemon first, because a dry run is safe beside one.
///
/// `administrator` carries the arguments for a start as administrator when the
/// configuration asks for one (`"elevated": true`). They differ from `args` in
/// naming every file with an absolute path, because an elevated process
/// inherits neither this shell's environment nor its working directory. The
/// prompt is shown only when this process is not elevated already: an elevated
/// `mochic` starts the daemon the ordinary way and the daemon inherits its
/// token. A declined prompt is not an error. Mochi is the user's only window
/// manager, so it still has to come up, with normal rights, and the watchdog
/// must not meet a failure it would answer with the next prompt.
///
/// # Errors
///
/// When `mochi` cannot be started, or a dry run ends with a failure.
pub fn start(
    launch: &impl Launch,
    args: &[String],
    dry_run: bool,
    administrator: Option<&[String]>,
) -> Result<()> {
    if dry_run {
        let mut args = args.to_vec();
        args.insert(0, "--dry-run".to_owned());
        let status = launch.attached(&args)?;
        if !status.success() {
            bail!("the dry run exited with {}", exit_code(status));
        }
        return Ok(());
    }
    if launch.daemon_is_running() {
        println!("mochi is already running");
        return Ok(());
    }
    if let Some(administrator) = administrator
        && !launch.is_elevated()
    {
        match launch.daemon_as_administrator(administrator)? {
            Administrator::Started => return Ok(()),
            Administrator::Declined => {
                println!(
                    "the administrator prompt was declined, starting mochi with normal rights"
                );
            }
        }
    }
    launch.daemon(args)
}

fn mochi_executable() -> Result<PathBuf> {
    find_executable("mochi").context("mochi.exe is neither next to mochic nor on PATH")
}

fn exit_code(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "an unknown code".to_owned(),
        |code| format!("code {code}"),
    )
}

/// Starts the daemon and waits until it answers on its pipe.
///
/// Success is this daemon answering, not any daemon: two `mochic start` at once
/// both spawn, the single instance mutex refuses the loser, and the loser would
/// otherwise see the winner's pipe and report a start that never happened.
fn start_daemon(args: &[String]) -> Result<()> {
    let exe = mochi_executable()?;
    let mut child = spawn_detached(&exe, args)?;
    let pid = child.id();
    wait_for_pipe(&format!("mochi (pid {pid})"), || {
        child
            .try_wait()
            .with_context(|| format!("could not check on mochi (pid {pid})"))
            .map(|status| status.map(exit_code))
    })?;
    // Printed only now: a line on stdout is what a script reads to decide the
    // start worked.
    println!("started {} (pid {pid})", exe.display());
    Ok(())
}

/// Starts the daemon as administrator and waits until it answers on its pipe.
///
/// The daemon is started through `conhost.exe --headless`, as the Watchdog task
/// and the Start menu entry start it: `mochi.exe` is a console program, and one
/// relaunched bare through the prompt gets a terminal window on Windows 11 that
/// takes the focus from whatever the user was doing. The handle waited on is
/// conhost's, which lives as long as the daemon it hosts and exits with its
/// code.
fn start_daemon_as_administrator(args: &[String]) -> Result<Administrator> {
    let exe = mochi_executable()?;
    let Some(process) = win::run_as_administrator(&exe, args)? else {
        return Ok(Administrator::Declined);
    };
    wait_for_pipe("mochi (as administrator)", || {
        Ok(process.exit_code().map(|code| format!("code {code}")))
    })?;
    println!("started {} as administrator", exe.display());
    Ok(Administrator::Started)
}

/// Waits until a daemon that was just started answers on the pipe. `exited`
/// says whether that daemon has already ended, and with which code.
fn wait_for_pipe(name: &str, mut exited: impl FnMut() -> Result<Option<String>>) -> Result<()> {
    let deadline = std::time::Instant::now() + START_TIMEOUT;
    loop {
        // Asked before the pipe, so a daemon that was refused is reported as
        // the exit it is instead of costing the whole timeout, and instead of
        // being covered up by whichever daemon does hold the pipe.
        if let Some(code) = exited()? {
            if mochi_client::is_running() {
                bail!(
                    "{name} exited with {code}; another mochi already holds {}, so nothing was started here",
                    mochi_client::PIPE_NAME
                );
            }
            // `mochic check` before the log, on purpose. The daemon is started
            // detached with no stderr, so whatever reason it gave for giving up
            // went to a dated log file and never to this terminal. Far and away
            // the most common reason is a configuration file it could not read,
            // and `check` finds exactly that, offline, naming the file, the line
            // and the column.
            bail!(
                "{name} exited with {code} without opening {}.
Run `mochic check` to test your configuration, or read the log in %LOCALAPPDATA%\\mochi",
                mochi_client::PIPE_NAME
            );
        }
        if mochi_client::is_running() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!(
        "{name} is running but did not open {} within {} seconds.
Run `mochic check` to test your configuration, or read the log in %LOCALAPPDATA%\\mochi",
        mochi_client::PIPE_NAME,
        START_TIMEOUT.as_secs()
    )
}

/// Quotes one argument the way the C runtime splits a command line back into
/// arguments: the prompt takes all parameters as a single string.
fn quote_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return argument.to_owned();
    }
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for c in argument.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// The Windows calls behind a start as administrator.
mod win {
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use anyhow::{Context, Result, bail};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_CANCELLED, HANDLE, STILL_ACTIVE, WAIT_OBJECT_0,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    };
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        ShellExecuteExW,
    };
    use windows::core::PCWSTR;

    use super::{CtrlC, quote_argument};

    /// This process's own Ctrl-C setting.
    pub struct Console;

    impl CtrlC for Console {
        fn set_ignored(&self, ignored: bool) {
            // SAFETY: no handler routine is passed, so the call only sets or
            // clears this process's flag. A failure leaves Ctrl-C as it was.
            let _ = unsafe { SetConsoleCtrlHandler(None, ignored) };
        }
    }

    /// Whether this process runs with an elevated token. Asked of the token,
    /// never guessed from a path or a group membership.
    pub fn is_elevated() -> bool {
        // SAFETY: the token handle is opened here, read with a buffer of the
        // size the call is told, and closed before returning.
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut length = 0;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some((&raw mut elevation).cast::<c_void>()),
                u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(u32::MAX),
                &raw mut length,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elevation.TokenIsElevated != 0
        }
    }

    /// A process started through the prompt. The handle closes with it.
    pub struct Started(HANDLE);

    impl Started {
        /// The exit code once the process has ended, `None` while it runs.
        pub fn exit_code(&self) -> Option<u32> {
            // SAFETY: the handle is owned by `self` and open until drop.
            unsafe {
                if WaitForSingleObject(self.0, 0) != WAIT_OBJECT_0 {
                    return None;
                }
                let mut code = 0u32;
                if GetExitCodeProcess(self.0, &raw mut code).is_err()
                    || code == STILL_ACTIVE.0.cast_unsigned()
                {
                    return None;
                }
                Some(code)
            }
        }
    }

    impl Drop for Started {
        fn drop(&mut self) {
            // SAFETY: the handle came from ShellExecuteExW and is closed once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Shows the administrator prompt for `conhost.exe --headless <exe> <args>`
    /// and returns the started process, or `None` when the prompt was declined.
    pub fn run_as_administrator(exe: &Path, args: &[String]) -> Result<Option<Started>> {
        let system = std::env::var_os("SystemRoot")
            .filter(|v| !v.is_empty())
            .context("SystemRoot is not set, so conhost.exe cannot be found")?;
        let conhost = Path::new(&system).join("System32").join("conhost.exe");
        let mut parameters = vec![
            "--headless".to_owned(),
            quote_argument(&exe.display().to_string()),
        ];
        parameters.extend(args.iter().map(|a| quote_argument(a)));
        let parameters = parameters.join(" ");
        // An elevated process starts in System32 unless told otherwise. The
        // files are named absolutely already; this keeps the rest predictable.
        let directory = std::env::current_dir().ok();

        let verb = wide(OsStr::new("runas"));
        let file = wide(conhost.as_os_str());
        let params = wide(OsStr::new(&parameters));
        let dir = directory.as_deref().map(|d| wide(d.as_os_str()));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>()).unwrap_or(u32::MAX),
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            lpDirectory: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            nShow: 0,
            ..Default::default()
        };
        // SAFETY: every string the structure points at outlives the call.
        unsafe {
            // The shell wants an apartment. A failure only means this thread
            // has one already, which serves just as well.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            if let Err(error) = ShellExecuteExW(&raw mut info) {
                if error.code() == ERROR_CANCELLED.to_hresult() {
                    return Ok(None);
                }
                return Err(error).with_context(|| {
                    format!("could not start {} as administrator", exe.display())
                });
            }
        }
        if info.hProcess.is_invalid() {
            bail!(
                "the administrator start of {} returned no process to wait for",
                exe.display()
            );
        }
        Ok(Some(Started(info.hProcess)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_executable_is_not_found() {
        assert!(find_executable("mochi-definitely-not-installed").is_none());
    }

    #[test]
    fn the_running_test_binary_is_found_next_to_itself() {
        let current = std::env::current_exe().unwrap();
        let stem = current.file_stem().unwrap().to_string_lossy().to_string();
        assert_eq!(find_executable(&stem), Some(current));
    }

    /// Stands in for the processes and the pipe, and writes down every call.
    struct Fake {
        running: bool,
        exit: u32,
        elevated: bool,
        prompt: Administrator,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(running: bool, exit: u32) -> Self {
            Self {
                running,
                exit,
                elevated: false,
                prompt: Administrator::Started,
                calls: std::cell::RefCell::default(),
            }
        }

        fn elevated(mut self) -> Self {
            self.elevated = true;
            self
        }

        fn declining(mut self) -> Self {
            self.prompt = Administrator::Declined;
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn record(&self, call: &str) {
            self.calls.borrow_mut().push(call.to_owned());
        }
    }

    impl Launch for Fake {
        fn daemon_is_running(&self) -> bool {
            self.calls.borrow_mut().push("pipe".to_owned());
            self.running
        }

        fn daemon(&self, args: &[String]) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("detached {}", args.join(" ")));
            Ok(())
        }

        fn is_elevated(&self) -> bool {
            self.calls.borrow_mut().push("token".to_owned());
            self.elevated
        }

        fn daemon_as_administrator(&self, args: &[String]) -> Result<Administrator> {
            self.calls
                .borrow_mut()
                .push(format!("administrator {}", args.join(" ")));
            Ok(self.prompt)
        }

        fn attached(&self, args: &[String]) -> Result<ExitStatus> {
            use std::os::windows::process::ExitStatusExt;
            self.calls
                .borrow_mut()
                .push(format!("attached {}", args.join(" ")));
            Ok(ExitStatus::from_raw(self.exit))
        }
    }

    impl CtrlC for Fake {
        fn set_ignored(&self, ignored: bool) {
            let call = if ignored {
                "ignore ctrl-c"
            } else {
                "heed ctrl-c"
            };
            self.record(call);
        }
    }

    #[test]
    fn ctrl_c_is_ignored_only_from_the_spawn_to_the_end_of_the_wait() {
        use std::os::windows::process::ExitStatusExt;
        let fake = Fake::new(false, 0);
        let status = run_attached(
            &fake,
            || {
                fake.record("spawn");
                Ok(())
            },
            |()| {
                fake.record("wait");
                Ok(ExitStatus::from_raw(0))
            },
        )
        .expect("the wait should succeed");
        assert!(status.success());
        assert_eq!(
            fake.calls(),
            ["spawn", "ignore ctrl-c", "wait", "heed ctrl-c"],
            "ignored before the spawn, the child would inherit it and Ctrl-C could not stop it"
        );
    }

    #[test]
    fn ctrl_c_is_heeded_again_when_the_wait_fails() {
        let fake = Fake::new(false, 0);
        let error = run_attached(
            &fake,
            || {
                fake.record("spawn");
                Ok(())
            },
            |()| -> Result<ExitStatus> {
                fake.record("wait");
                bail!("the wait failed")
            },
        )
        .expect_err("a failed wait is an error");
        assert_eq!(error.to_string(), "the wait failed");
        assert_eq!(
            fake.calls(),
            ["spawn", "ignore ctrl-c", "wait", "heed ctrl-c"]
        );
    }

    #[test]
    fn ctrl_c_is_left_alone_when_nothing_starts() {
        let fake = Fake::new(false, 0);
        run_attached(
            &fake,
            || -> Result<()> {
                fake.record("spawn");
                bail!("no such program")
            },
            |()| unreachable!("nothing was started to wait for"),
        )
        .expect_err("a failed spawn is an error");
        assert_eq!(fake.calls(), ["spawn"]);
    }

    /// Tells a copy of this test binary which helper it is.
    const CTRL_C_HELPER: &str = "MOCHIC_TEST_CTRL_C_HELPER";

    /// Runs one ignored helper test of this binary as its own process.
    fn helper(name: &str, role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", name, "--ignored", "--nocapture"])
            .env(CTRL_C_HELPER, role);
        command
    }

    #[test]
    fn ctrl_c_on_the_console_ends_the_child_and_not_mochic() {
        use std::os::windows::process::CommandExt;
        // `CREATE_NO_WINDOW`: a console of its own with no window, so the
        // Ctrl-C raised in it reaches nothing else.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = helper("process::tests::ctrl_c_waiter", "waiter")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("the child exited with 0xc000013a"),
            "Ctrl-C must end the child, and the waiter must live to see it ({}):\n{stdout}\n{stderr}",
            output.status
        );
        assert!(
            output.status.success(),
            "{}:\n{stdout}\n{stderr}",
            output.status
        );
    }

    /// Stands in for `mochic start --dry-run` in a console of its own: starts
    /// a child on it, raises Ctrl-C there and says how the child ended.
    #[test]
    #[ignore = "run only by ctrl_c_on_the_console_ends_the_child_and_not_mochic"]
    fn ctrl_c_waiter() {
        use std::io::BufRead;
        use windows::Win32::System::Console::{
            CTRL_C_EVENT, GenerateConsoleCtrlEvent, GetConsoleProcessList,
        };
        if std::env::var(CTRL_C_HELPER).as_deref() != Ok("waiter") {
            return;
        }
        // Heeded, as a shell leaves it. A process started in a new process
        // group, as some tools start the test run, inherits it ignored.
        win::Console.set_ignored(false);
        let status = run_attached(
            &win::Console,
            || {
                helper("process::tests::ctrl_c_child", "child")
                    .stdout(Stdio::piped())
                    .spawn()
                    .context("could not start the child")
            },
            |mut child| {
                let stdout = child.stdout.take().context("the child has no stdout")?;
                let mut lines = std::io::BufReader::new(stdout).lines();
                // Only a child that runs code of its own is sure to be on the
                // console and get the Ctrl-C.
                anyhow::ensure!(
                    lines.any(|line| line.is_ok_and(|line| line == "ready")),
                    "the child ended before it was ready"
                );
                let mut on = [0u32; 8];
                // SAFETY: the call writes at most `on.len()` process ids.
                let count = unsafe { GetConsoleProcessList(&mut on) } as usize;
                let mut on = on[..count.min(on.len())].to_vec();
                let mut expected = vec![std::process::id(), child.id()];
                on.sort_unstable();
                expected.sort_unstable();
                anyhow::ensure!(
                    on == expected,
                    "this console holds {on:?}, not only {expected:?}, so no Ctrl-C is raised on it"
                );
                // SAFETY: plain values; the console is this helper's own.
                unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) }
                    .context("could not raise Ctrl-C")?;
                let status = child.wait().context("could not wait for the child")?;
                // Each process gets the Ctrl-C on a thread of its own, so this
                // one may see it after the child has gone. Waiting here keeps
                // the test from depending on that order.
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok(status)
            },
        )
        .unwrap();
        let code = status.code().expect("an exit code").cast_unsigned();
        println!("the child exited with {code:#x}");
    }

    /// The waiter's child: says it is running, then waits to be stopped.
    #[test]
    #[ignore = "run only by ctrl_c_on_the_console_ends_the_child_and_not_mochic"]
    fn ctrl_c_child() {
        if std::env::var(CTRL_C_HELPER).as_deref() != Ok("child") {
            return;
        }
        println!("ready");
        std::thread::sleep(std::time::Duration::from_secs(5));
    }

    fn args() -> Vec<String> {
        vec!["--config".to_owned(), r"D:\mochi.json".to_owned()]
    }

    #[test]
    fn a_dry_run_runs_on_this_console_and_is_waited_for() {
        let fake = Fake::new(false, 0);
        start(&fake, &args(), true, None).expect("the dry run should succeed");
        assert_eq!(
            fake.calls(),
            [r"attached --dry-run --config D:\mochi.json"],
            "a dry run started detached opens no pipe and is left running where nothing can reach it"
        );
    }

    #[test]
    fn a_dry_run_runs_beside_a_running_daemon() {
        let fake = Fake::new(true, 0);
        start(&fake, &args(), true, None).expect("the dry run should succeed");
        assert_eq!(
            fake.calls(),
            [r"attached --dry-run --config D:\mochi.json"],
            "a dry run is safe next to a daemon and must not be turned away by one"
        );
    }

    #[test]
    fn a_dry_run_that_fails_is_reported() {
        let fake = Fake::new(false, 2);
        let error = start(&fake, &args(), true, None).expect_err("an exit code 2 is not a success");
        assert!(error.to_string().contains("code 2"), "{error}");
        assert_eq!(fake.calls().len(), 1);
    }

    #[test]
    fn a_start_goes_detached_when_no_daemon_runs() {
        let fake = Fake::new(false, 0);
        start(&fake, &args(), false, None).expect("the start should succeed");
        assert_eq!(fake.calls(), ["pipe", r"detached --config D:\mochi.json"]);
    }

    #[test]
    fn a_start_next_to_a_running_daemon_starts_nothing() {
        let fake = Fake::new(true, 0);
        start(&fake, &args(), false, None).expect("already running is not an error");
        assert_eq!(fake.calls(), ["pipe"]);
    }

    /// What `main` hands over when the file asks for rights: every file named.
    fn administrator_args() -> Vec<String> {
        vec![
            "--config".to_owned(),
            r"D:\mochi.json".to_owned(),
            "--hotkeys".to_owned(),
            r"D:\keys.txt".to_owned(),
        ]
    }

    #[test]
    fn a_file_that_asks_for_rights_starts_the_daemon_through_the_prompt() {
        let fake = Fake::new(false, 0);
        start(&fake, &args(), false, Some(&administrator_args()))
            .expect("the start should succeed");
        assert_eq!(
            fake.calls(),
            [
                "pipe",
                "token",
                r"administrator --config D:\mochi.json --hotkeys D:\keys.txt"
            ],
            "the elevated daemon inherits nothing from this shell, so every file is named"
        );
    }

    #[test]
    fn a_declined_prompt_starts_mochi_with_normal_rights() {
        let fake = Fake::new(false, 0).declining();
        start(&fake, &args(), false, Some(&administrator_args()))
            .expect("a declined prompt is not an error");
        assert_eq!(
            fake.calls(),
            [
                "pipe",
                "token",
                r"administrator --config D:\mochi.json --hotkeys D:\keys.txt",
                r"detached --config D:\mochi.json",
            ],
            "the only window manager must still come up, with the ordinary arguments"
        );
    }

    #[test]
    fn an_elevated_mochic_starts_the_daemon_without_a_prompt() {
        let fake = Fake::new(false, 0).elevated();
        start(&fake, &args(), false, Some(&administrator_args()))
            .expect("the start should succeed");
        assert_eq!(
            fake.calls(),
            ["pipe", "token", r"detached --config D:\mochi.json"],
            "the daemon inherits an elevated token, so a prompt would only ask twice"
        );
    }

    #[test]
    fn a_running_daemon_is_not_started_again_as_administrator() {
        let fake = Fake::new(true, 0);
        start(&fake, &args(), false, Some(&administrator_args()))
            .expect("already running is not an error");
        assert_eq!(fake.calls(), ["pipe"], "no prompt for a daemon that runs");
    }

    #[test]
    fn a_dry_run_never_asks_for_rights() {
        let fake = Fake::new(false, 0);
        start(&fake, &args(), true, Some(&administrator_args()))
            .expect("the dry run should succeed");
        assert_eq!(fake.calls(), [r"attached --dry-run --config D:\mochi.json"]);
    }

    #[test]
    fn arguments_are_quoted_the_way_the_c_runtime_reads_them() {
        assert_eq!(quote_argument(r"C:\mochi\mochi.exe"), r"C:\mochi\mochi.exe");
        assert_eq!(
            quote_argument(r"C:\Program Files\Mochi\mochi.exe"),
            r#""C:\Program Files\Mochi\mochi.exe""#
        );
        assert_eq!(
            quote_argument(r"D:\my files\"),
            r#""D:\my files\\""#,
            "a trailing backslash must not escape the closing quote"
        );
        assert_eq!(quote_argument(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_argument(""), r#""""#);
    }

    #[test]
    fn the_token_of_this_process_can_be_read() {
        // CI runs elevated and a desk does not; either way the answer comes
        // from the token and the call does not fail
        let _ = win::is_elevated();
    }
}
