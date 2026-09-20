//! `mochi-testwin`: spawn and drive disposable Win32 windows for tiling tests.
//!
//! The first invocation, `spawn`, stays in the foreground and owns the windows.
//! Any later invocation talks to it, either over the control pipe or straight
//! through Win32 on the window handles. See the crate README for the contract
//! with the daemon.

use clap::{Parser, Subcommand};

/// Spawn and drive test windows of the class `MochiTestWindow`.
#[derive(Debug, Parser)]
#[command(
    name = "mochi-testwin",
    version,
    about = "Disposable Win32 windows for testing Mochi against a real desktop.",
    long_about = "Spawns plain Win32 windows of the class MochiTestWindow, with \
WS_EX_TOOLWINDOW so that other window managers ignore them. Start the daemon \
with --manage-class MochiTestWindow to have Mochi tile exactly these windows. \
Every subcommand prints one JSON object, or one JSON object per line."
)]
struct Cli {
    /// What to do.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create test windows and keep them alive until `close --all` or Ctrl+C.
    Spawn {
        /// How many windows to create.
        #[arg(long, default_value_t = 3)]
        count: u32,
        /// Monitor index, in EnumDisplayMonitors order. See `monitors`.
        #[arg(long, default_value_t = 0)]
        monitor: usize,
        /// Title prefix; the window index is appended.
        #[arg(long, default_value = mochi_testbed::DEFAULT_TITLE_PREFIX)]
        title_prefix: String,
        /// Left edge in virtual screen coordinates. With --y the batch is
        /// placed exactly there instead of being staggered.
        #[arg(long, requires = "y")]
        x: Option<i32>,
        /// Top edge in virtual screen coordinates.
        #[arg(long, requires = "x")]
        y: Option<i32>,
        /// Width in physical pixels.
        #[arg(long, alias = "width", requires = "h")]
        w: Option<i32>,
        /// Height in physical pixels.
        #[arg(long, alias = "height", requires = "w")]
        h: Option<i32>,
        /// Minimum size the windows defend on WM_GETMINMAXINFO, as W H. This
        /// is how an application with a minimum size is simulated.
        #[arg(long, num_args = 2, value_names = ["W", "H"])]
        min_size: Option<Vec<i32>>,
        /// Create owned popups: each window gets a hidden owner window, which
        /// the usual manageability rules skip.
        #[arg(long)]
        owned: bool,
        /// Create the windows with an empty title, which the usual
        /// manageability rules skip.
        #[arg(long)]
        no_title: bool,
    },
    /// Print every test window on the desktop as JSON.
    List,
    /// Print the monitors as `--monitor` indexes them.
    Monitors,
    /// Close test windows.
    Close {
        /// Close all of them and let the host process exit.
        #[arg(long, conflicts_with = "hwnd")]
        all: bool,
        /// Close one window.
        #[arg(long, value_parser = parse_hwnd, required_unless_present = "all")]
        hwnd: Option<i64>,
    },
    /// Bring one window to the foreground.
    Focus {
        /// The window, decimal or 0x hex.
        #[arg(long, value_parser = parse_hwnd)]
        hwnd: i64,
    },
    /// Resize one window without moving it.
    Resize {
        /// The window, decimal or 0x hex.
        #[arg(long, value_parser = parse_hwnd)]
        hwnd: i64,
        /// New width in physical pixels.
        #[arg(long, alias = "width")]
        w: i32,
        /// New height in physical pixels.
        #[arg(long, alias = "height")]
        h: i32,
    },
    /// Move one window without resizing it.
    Move {
        /// The window, decimal or 0x hex.
        #[arg(long, value_parser = parse_hwnd)]
        hwnd: i64,
        /// New left edge in virtual screen coordinates.
        #[arg(long)]
        x: i32,
        /// New top edge in virtual screen coordinates.
        #[arg(long)]
        y: i32,
    },
    /// Minimize one window.
    Minimize {
        /// The window, decimal or 0x hex.
        #[arg(long, value_parser = parse_hwnd)]
        hwnd: i64,
    },
    /// Restore one minimized window.
    Restore {
        /// The window, decimal or 0x hex.
        #[arg(long, value_parser = parse_hwnd)]
        hwnd: i64,
    },
    /// Cloak or uncloak one window, the way a manager hides a workspace.
    Cloak {
        /// The window, decimal or 0x hex.
        #[arg(long, value_parser = parse_hwnd)]
        hwnd: i64,
        /// Hide the window through DWM.
        #[arg(long, conflicts_with = "off")]
        on: bool,
        /// Show it again.
        #[arg(long, required_unless_present = "on")]
        off: bool,
    },
    /// Give one window a transparency, or take it away again.
    Alpha {
        /// The window, decimal or 0x hex.
        #[arg(long, value_parser = parse_hwnd)]
        hwnd: i64,
        /// Opacity from 0, invisible, to 255, opaque.
        #[arg(long, conflicts_with = "clear", required_unless_present = "clear")]
        value: Option<u8>,
        /// Drop the transparency and the layered bit with it.
        #[arg(long)]
        clear: bool,
    },
    /// Give one window a new title.
    Rename {
        /// The window, decimal or 0x hex.
        #[arg(long, value_parser = parse_hwnd)]
        hwnd: i64,
        /// The new title.
        #[arg(long)]
        title: String,
    },
}

/// Accepts `4660` and `0x1234`, which is what the JSON output and the logs use.
fn parse_hwnd(text: &str) -> Result<i64, String> {
    let text = text.trim();
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16),
        None => text.parse::<i64>(),
    };
    parsed.map_err(|_| format!("`{text}` is not a window handle, use 4660 or 0x1234"))
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    imp::run(Cli::parse().command)
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    let _ = Cli::parse();
    eprintln!(r#"{{"ok":false,"error":"mochi-testwin only runs on Windows"}}"#);
    std::process::ExitCode::from(2)
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use mochi_testbed::control::{self, Request, Response, Session};
    use mochi_testbed::{Result, SpawnOptions, TestWindows};
    use serde_json::json;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    use super::Command;

    /// Set by Ctrl+C and by a `close --all` that arrives over the pipe.
    static SHUTDOWN: AtomicBool = AtomicBool::new(false);

    /// How often the host looks whether it should still be alive.
    const HOST_POLL: Duration = Duration::from_millis(100);

    /// Every batch the host owns. `spawn` over the pipe appends to this.
    type Batches = Arc<Mutex<Vec<TestWindows>>>;

    pub fn run(command: Command) -> std::process::ExitCode {
        mochi_testbed::ensure_per_monitor_v2();
        match dispatch(command) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", json!({"ok": false, "error": e.to_string()}));
                std::process::ExitCode::FAILURE
            }
        }
    }

    fn dispatch(command: Command) -> Result<()> {
        match command {
            Command::Spawn {
                count,
                monitor,
                title_prefix,
                x,
                y,
                w,
                h,
                min_size,
                owned,
                no_title,
            } => spawn(SpawnOptions {
                count,
                monitor,
                title_prefix,
                // The whole point of the host: a JSON line per window event.
                emit_events: true,
                size: w.zip(h),
                position: x.zip(y),
                min_size: min_size.and_then(|values| match values[..] {
                    [width, height] => Some((width, height)),
                    _ => None,
                }),
                owned,
                no_title,
                // No flag for this one: a menu bar exists for the tests that
                // ask what a keystroke does to a window that has one, and a
                // hand driven tiling session has no use for it. The control
                // pipe carries it for a host that does.
                menu_bar: false,
                // Nor for this one: a window without the tool window bit is
                // visible to any other window manager on the desktop, so only
                // a test that knows the desktop is its own asks for it.
                taskbar: false,
            }),
            Command::List => {
                print(&json!(mochi_testbed::list_windows()?));
                Ok(())
            }
            Command::Monitors => {
                print(&json!(mochi_testbed::monitors()?));
                Ok(())
            }
            Command::Close { all: _, hwnd } => close(hwnd),
            Command::Focus { hwnd } => {
                let focused = mochi_testbed::focus_window(hwnd)?;
                print(&json!({
                    "ok": true,
                    "command": "focus",
                    "hwnd": hwnd,
                    "focused": focused,
                    "note": if focused {
                        "the window is in the foreground"
                    } else {
                        "raised, but Windows refused the foreground change; \
                         that is normal from a console that is not itself in front"
                    },
                }));
                Ok(())
            }
            Command::Resize { hwnd, w, h } => {
                mochi_testbed::resize_window(hwnd, w, h)?;
                print(&result("resize", hwnd));
                Ok(())
            }
            Command::Move { hwnd, x, y } => {
                mochi_testbed::move_window(hwnd, x, y)?;
                print(&result("move", hwnd));
                Ok(())
            }
            Command::Minimize { hwnd } => {
                mochi_testbed::minimize_window(hwnd)?;
                print(&result("minimize", hwnd));
                Ok(())
            }
            Command::Restore { hwnd } => {
                mochi_testbed::restore_window(hwnd)?;
                print(&result("restore", hwnd));
                Ok(())
            }
            Command::Cloak { hwnd, on, off: _ } => {
                mochi_testbed::set_cloaked(hwnd, on)?;
                print(&result("cloak", hwnd));
                Ok(())
            }
            Command::Alpha {
                hwnd,
                value,
                clear: _,
            } => {
                mochi_testbed::set_window_alpha(hwnd, value)?;
                print(&result("alpha", hwnd));
                Ok(())
            }
            Command::Rename { hwnd, title } => {
                mochi_testbed::rename_window(hwnd, &title)?;
                print(&result("rename", hwnd));
                Ok(())
            }
        }
    }

    /// The state of one window after a command, so the caller can check the
    /// effect without a second invocation.
    fn result(command: &str, hwnd: i64) -> serde_json::Value {
        match mochi_testbed::window_info(hwnd) {
            Ok(window) => json!({"ok": true, "command": command, "window": window}),
            Err(e) => {
                json!({"ok": true, "command": command, "hwnd": hwnd, "window": null, "note": e.to_string()})
            }
        }
    }

    fn print(value: &serde_json::Value) {
        match serde_json::to_string_pretty(value) {
            Ok(text) => println!("{text}"),
            Err(e) => eprintln!("{}", json!({"ok": false, "error": e.to_string()})),
        }
    }

    /// `spawn`: become the host, or hand the request to the host that is
    /// already running.
    fn spawn(options: SpawnOptions) -> Result<()> {
        if control::live_host().is_some() {
            // The running host owns the message loops, so the options travel to
            // it rather than being dropped on the floor here.
            let response = control::send(&Request::Spawn {
                options: control::SpawnRequest::from(&options),
            })?;
            print(&json!(response));
            return Ok(());
        }

        let batch = TestWindows::spawn_with(&options)?;
        let batches: Batches = Arc::new(Mutex::new(vec![batch]));

        let served = Arc::clone(&batches);
        control::serve(move |request| handle(&served, request))?;
        control::write_session(&Session::current())?;
        install_ctrl_handler();

        println!(
            "{}",
            json!({
                "host": "ready",
                "pid": std::process::id(),
                "pipe": control::PIPE_NAME,
                "class": mochi_testbed::TEST_WINDOW_CLASS,
                "session": control::session_path().to_string_lossy(),
                "windows": handles(&batches),
                "hint": format!(
                    "start the daemon with {} {} and watch the event lines below",
                    mochi_testbed::DAEMON_MANAGE_FLAG,
                    mochi_testbed::TEST_WINDOW_CLASS,
                ),
            })
        );

        while !SHUTDOWN.load(Ordering::SeqCst) && live_count(&batches) > 0 {
            std::thread::sleep(HOST_POLL);
        }

        let mut owned = lock(&batches);
        let mut failure = None;
        for batch in owned.iter_mut() {
            if let Err(e) = batch.close_all() {
                failure = Some(e);
            }
        }
        owned.clear();
        drop(owned);
        control::remove_session();

        println!(
            "{}",
            json!({"host": "exit", "pid": std::process::id(), "reason": reason()})
        );
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn reason() -> &'static str {
        if SHUTDOWN.load(Ordering::SeqCst) {
            "close all or Ctrl+C"
        } else {
            "the last test window was closed"
        }
    }

    /// `close`: `--all` goes through the host so the process exits too,
    /// `--hwnd` is a plain `WM_CLOSE` on that one window.
    fn close(hwnd: Option<i64>) -> Result<()> {
        if let Some(hwnd) = hwnd {
            mochi_testbed::close_window(hwnd)?;
            mochi_testbed::wait_until_gone(&[hwnd], Duration::from_secs(5))?;
            print(&json!({"ok": true, "command": "close", "hwnd": hwnd, "closed": 1}));
            return Ok(());
        }

        let before = mochi_testbed::list_windows()?;
        let handles: Vec<i64> = before.iter().map(|w| w.hwnd).collect();

        // Ask the host first: it owns the message loops and should shut itself
        // down. Without a host, posting WM_CLOSE does the same to the windows.
        let via = if control::live_host().is_some() {
            control::send(&Request::CloseAll)?;
            "control pipe"
        } else {
            for &hwnd in &handles {
                let _ = mochi_testbed::close_window(hwnd);
            }
            "WM_CLOSE"
        };

        mochi_testbed::wait_until_gone(&handles, Duration::from_secs(5))?;
        print(&json!({
            "ok": true,
            "command": "close",
            "via": via,
            "closed": handles.len(),
        }));
        Ok(())
    }

    fn handle(batches: &Batches, request: Request) -> Response {
        match request {
            Request::Ping => Response::ok(),
            Request::List => match mochi_testbed::list_windows() {
                Ok(windows) => Response::ok().with_windows(windows),
                Err(e) => Response::failed(e.to_string()),
            },
            Request::Spawn { options } => {
                let options = options.into_options();
                match TestWindows::spawn_with(&options) {
                    Ok(batch) => {
                        let windows = batch.windows();
                        lock(batches).push(batch);
                        Response::ok().with_windows(windows)
                    }
                    Err(e) => Response::failed(e.to_string()),
                }
            }
            Request::CloseAll => {
                SHUTDOWN.store(true, Ordering::SeqCst);
                Response::ok()
            }
        }
    }

    fn lock(batches: &Batches) -> std::sync::MutexGuard<'_, Vec<TestWindows>> {
        // A poisoned mutex here means a window thread panicked. The windows
        // still have to be closed, so carry on with the state as it is.
        batches.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn live_count(batches: &Batches) -> usize {
        lock(batches).iter().map(TestWindows::live_count).sum()
    }

    fn handles(batches: &Batches) -> Vec<i64> {
        lock(batches)
            .iter()
            .flat_map(TestWindows::handles)
            .collect()
    }

    fn install_ctrl_handler() {
        unsafe extern "system" fn on_ctrl(_kind: u32) -> windows::core::BOOL {
            SHUTDOWN.store(true, Ordering::SeqCst);
            // Handled: the main loop closes the windows and exits cleanly.
            true.into()
        }
        // A failure here only means Ctrl+C kills the process the hard way,
        // which leaves no windows behind either: they die with the process.
        let _ = unsafe { SetConsoleCtrlHandler(Some(on_ctrl), true) };
    }
}
