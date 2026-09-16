//! The window manager loop.
//!
//! One thread owns [`State`] and consumes one channel. Every other thread in
//! the daemon is a producer: the WinEvent hooks, the hidden message window, the
//! mouse tracker, the configuration watcher and the IPC server. There is no
//! shared mutable state and therefore no lock ordering to get wrong.
//!
//! # Hook points for the tiling model
//!
//! The methods below are the seams `mochi-core` plugs into. They are marked
//! with `HOOK` in their documentation and all of them currently only log:
//!
//! * [`WindowManager::retile`] recompute and apply every layout,
//! * [`WindowManager::apply_layout`] turn rectangles into window moves,
//! * `WindowManager::on_window_event` feed the tree,
//! * [`WindowManager::reload_config`] parse and apply `mochi.json`,
//! * [`WindowManager::restore_all`] uncloak and restore on the way out,
//! * [`WindowManager::cloaked`] the list `restore_all` works from.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use mochi_client::{
    BooleanState, Command, Notification, NotificationEvent, QueryTarget, Response, WindowRef,
};
use mochi_core::Rect;

use crate::config;
use crate::events::{Event, EventReceiver, EventSender, MonitorEventKind, WindowEventKind};
use crate::ipc::Subscribers;
use crate::platform::{Hwnd, Platform, WindowPlacement};
use crate::state::{Settings, State};

/// Whether the loop keeps going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Wait for the next event.
    Continue,
    /// Leave the loop and shut down.
    Stop,
}

/// Owns the state and reacts to everything.
pub struct WindowManager {
    platform: Arc<dyn Platform>,
    rx: EventReceiver,
    tx: EventSender,
    state: State,
    subscribers: Subscribers,
    cloaked: Arc<Mutex<Vec<Hwnd>>>,
    mouse: Option<crate::events::mouse::MouseTracker>,
}

impl WindowManager {
    /// Builds the manager and takes a first look at the desktop.
    pub fn new(
        platform: Arc<dyn Platform>,
        tx: EventSender,
        rx: EventReceiver,
        state: State,
    ) -> Result<Self> {
        let mut wm = Self {
            platform,
            rx,
            tx,
            state,
            subscribers: Subscribers::start()?,
            cloaked: Arc::new(Mutex::new(Vec::new())),
            mouse: None,
        };
        wm.refresh_monitors();
        wm.refresh_windows();
        wm.state.focused = wm.platform.foreground_window();
        Ok(wm)
    }

    /// Hands the mouse tracker over so `focus-follows-mouse` can toggle it.
    pub fn attach_mouse_tracker(&mut self, tracker: crate::events::mouse::MouseTracker) {
        tracker.set_enabled(self.state.settings.focus_follows_mouse);
        self.mouse = Some(tracker);
    }

    /// A sender for producers that are created after the manager.
    pub fn sender(&self) -> EventSender {
        self.tx.clone()
    }

    /// HOOK. The handles Mochi has cloaked, and therefore owes the user back.
    ///
    /// The tiling step pushes every window it cloaks in here and removes it on
    /// uncloak. [`WindowManager::restore_all`] and the panic hook read it, which
    /// is why it is an `Arc<Mutex<..>>` rather than a plain field.
    pub fn cloaked(&self) -> Arc<Mutex<Vec<Hwnd>>> {
        Arc::clone(&self.cloaked)
    }

    /// Installs the restore hook that runs on panic and on the way out.
    pub fn install_restore_hook(&self) {
        let platform = Arc::clone(&self.platform);
        let cloaked = Arc::clone(&self.cloaked);
        crate::safety::set_restore_hook(move || {
            let handles = match cloaked.lock() {
                Ok(list) => list.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            if handles.is_empty() {
                tracing::info!("restore: no window is cloaked by mochi");
                return;
            }
            tracing::warn!(count = handles.len(), "restore: uncloaking windows");
            for hwnd in handles {
                if let Err(e) = platform.set_cloaked(hwnd, false) {
                    tracing::error!(%hwnd, error = %e, "restore: could not uncloak");
                }
            }
        });
    }

    /// Runs until a `stop` command or a closed channel.
    pub fn run(&mut self) -> Result<()> {
        tracing::info!(
            platform = self.platform.name(),
            monitors = self.state.monitors.len(),
            windows = self.state.windows.len(),
            manageable = self.state.manageable_count(),
            "mochi is up"
        );

        while let Ok(event) = self.rx.recv() {
            if self.on_event(event) == Flow::Stop {
                break;
            }
        }

        self.subscribers
            .notify(Notification::new(NotificationEvent::Stop));
        tracing::info!("the event loop has ended");
        Ok(())
    }

    /// The whole state, for `mochic state`.
    pub fn state(&self) -> &State {
        &self.state
    }

    // -----------------------------------------------------------------
    // event handling
    // -----------------------------------------------------------------

    fn on_event(&mut self, event: Event) -> Flow {
        match event {
            Event::Window { kind, hwnd } => {
                self.on_window_event(kind, hwnd);
                Flow::Continue
            }
            Event::Monitors(kind) => {
                self.on_monitor_event(kind);
                Flow::Continue
            }
            Event::Session(kind) => {
                tracing::info!(?kind, "session change");
                self.notify(NotificationEvent::SessionChange { kind });
                Flow::Continue
            }
            Event::MouseFocus { hwnd } => {
                self.on_mouse_focus(hwnd);
                Flow::Continue
            }
            Event::ConfigChanged(path) => {
                tracing::info!(path = %path.display(), "the configuration changed on disk");
                self.reload_config();
                Flow::Continue
            }
            Event::Command { command, reply } => {
                let name = command.name();
                let (response, flow) = self.handle_command(*command);
                tracing::debug!(command = name, ok = response.is_ok(), "command handled");
                reply.send(response);
                flow
            }
            Event::Shutdown(reason) => {
                tracing::info!(?reason, "shutting down");
                Flow::Stop
            }
        }
    }

    /// HOOK. Where window events become tree updates.
    ///
    /// Today this only keeps the flat list in [`State`] current and emits
    /// notifications. The tiling step adds, moves and removes containers here
    /// and calls [`WindowManager::retile`] when the layout changed.
    fn on_window_event(&mut self, kind: WindowEventKind, hwnd: Hwnd) {
        match kind {
            WindowEventKind::Destroyed => {
                if let Some(gone) = self.state.forget(hwnd) {
                    tracing::debug!(
                        event = %kind,
                        %hwnd,
                        title = %gone.info.title,
                        exe = %gone.info.exe,
                        "window event"
                    );
                    if gone.manageable {
                        self.notify(NotificationEvent::Unmanage {
                            window: window_ref(&gone.info),
                        });
                    }
                }
                if self.state.focused == Some(hwnd) {
                    self.state.focused = self.platform.foreground_window();
                }
                return;
            }
            WindowEventKind::LocationChange => {
                // Fires on every frame of a drag and on most animations, so it
                // is only interesting for windows already in the list.
                let Some(tracked) = self.state.window(hwnd) else {
                    return;
                };
                if !tracked.manageable {
                    return;
                }
                tracing::debug!(
                    event = %kind,
                    %hwnd,
                    title = %tracked.info.title,
                    exe = %tracked.info.exe,
                    "window event"
                );
                return;
            }
            _ => {}
        }

        let Ok(info) = self.platform.window_info(hwnd) else {
            // The window died between the event and this read, which is normal.
            tracing::trace!(event = %kind, %hwnd, "window event for a dead window");
            if let Some(gone) = self.state.forget(hwnd)
                && gone.manageable
            {
                self.notify(NotificationEvent::Unmanage {
                    window: window_ref(&gone.info),
                });
            }
            return;
        };

        tracing::debug!(
            event = %kind,
            %hwnd,
            title = %info.title,
            exe = %info.exe,
            class = %info.class,
            "window event"
        );

        let was_manageable = self.state.window(hwnd).is_some_and(|w| w.manageable);
        let reference = window_ref(&info);
        self.state.upsert(info);
        let is_manageable = self.state.window(hwnd).is_some_and(|w| w.manageable);

        if is_manageable && !was_manageable {
            self.notify(NotificationEvent::Manage {
                window: reference.clone(),
            });
        } else if was_manageable && !is_manageable {
            self.notify(NotificationEvent::Unmanage {
                window: reference.clone(),
            });
        }

        if kind == WindowEventKind::Foreground {
            self.state.focused = Some(hwnd);
            self.notify(NotificationEvent::FocusChange {
                window: Some(reference),
            });
        }
    }

    fn on_monitor_event(&mut self, kind: MonitorEventKind) {
        tracing::info!(event = kind.as_str(), "monitor event");
        self.refresh_monitors();
        self.refresh_windows();
        self.notify(NotificationEvent::MonitorsChanged {
            count: self.state.monitors.len(),
        });
        // HOOK: the tiling step re-homes workspaces and retiles from here.
        self.retile();
    }

    fn on_mouse_focus(&mut self, hwnd: Hwnd) {
        if !self.state.settings.focus_follows_mouse || self.state.paused {
            return;
        }
        if self.state.focused == Some(hwnd) {
            return;
        }
        let Some(tracked) = self.state.window(hwnd) else {
            return;
        };
        if !tracked.manageable {
            return;
        }
        tracing::debug!(%hwnd, title = %tracked.info.title, "focus follows mouse");
        if let Err(e) = self.platform.focus(hwnd) {
            tracing::debug!(%hwnd, error = %e, "could not focus under the mouse");
        }
    }

    // -----------------------------------------------------------------
    // commands
    // -----------------------------------------------------------------

    fn handle_command(&mut self, command: Command) -> (Response, Flow) {
        match command {
            Command::State => (
                Response::State {
                    state: self.state.to_json(),
                },
                Flow::Continue,
            ),
            Command::Query { target } => (self.query(target), Flow::Continue),
            Command::Stop { whkd } => {
                tracing::info!(whkd, "stop requested");
                (Response::Ok, Flow::Stop)
            }
            Command::Start { .. } => (Response::Ok, Flow::Continue),
            Command::Quickstart => (self.quickstart(), Flow::Continue),
            Command::TogglePause => {
                self.state.paused = !self.state.paused;
                tracing::info!(paused = self.state.paused, "pause toggled");
                self.notify(NotificationEvent::Pause {
                    paused: self.state.paused,
                });
                (Response::Ok, Flow::Continue)
            }
            Command::ReloadConfiguration => {
                self.reload_config();
                (Response::Ok, Flow::Continue)
            }
            Command::Retile => {
                self.refresh_windows();
                self.retile();
                (Response::Ok, Flow::Continue)
            }
            Command::SubscribePipe { name } => (
                match self.subscribers.add(&name) {
                    Ok(()) => {
                        self.state.subscribers = self.subscribers.names().to_vec();
                        Response::Ok
                    }
                    Err(e) => Response::error(e),
                },
                Flow::Continue,
            ),
            Command::UnsubscribePipe { name } => (
                match self.subscribers.remove(&name) {
                    Ok(()) => {
                        self.state.subscribers = self.subscribers.names().to_vec();
                        Response::Ok
                    }
                    Err(e) => Response::error(e),
                },
                Flow::Continue,
            ),

            // Settings that are real today.
            Command::FocusFollowsMouse { state } => {
                Settings::set(&mut self.state.settings.focus_follows_mouse, state);
                if let Some(mouse) = &self.mouse {
                    mouse.set_enabled(state.is_enabled());
                }
                (Response::Ok, Flow::Continue)
            }
            Command::MouseFollowsFocus { state } => {
                Settings::set(&mut self.state.settings.mouse_follows_focus, state);
                (Response::Ok, Flow::Continue)
            }

            // Settings that are stored but not drawn until the visuals land.
            Command::ToggleTransparency => {
                self.state.settings.transparency = !self.state.settings.transparency;
                (self.stored("transparency"), Flow::Continue)
            }
            Command::Border { state } => {
                Settings::set(&mut self.state.settings.border, state);
                (self.stored("border"), Flow::Continue)
            }
            Command::BorderWidth { width } => {
                self.state.settings.border_width = width;
                (self.stored("border-width"), Flow::Continue)
            }
            Command::BorderOffset { offset } => {
                self.state.settings.border_offset = offset;
                (self.stored("border-offset"), Flow::Continue)
            }
            Command::Animation { state } => {
                Settings::set(&mut self.state.settings.animation, state);
                (self.stored("animation"), Flow::Continue)
            }
            Command::AnimationDuration { duration } => {
                self.state.settings.animation_duration = duration;
                (self.stored("animation-duration"), Flow::Continue)
            }
            Command::AnimationFps { fps } => {
                self.state.settings.animation_fps = fps;
                (self.stored("animation-fps"), Flow::Continue)
            }
            Command::ChangeLayout { layout } => {
                self.state.settings.default_layout = layout;
                (pending("change-layout"), Flow::Continue)
            }

            // Everything below needs the monitor / workspace / container tree.
            other => (pending(other.name()), Flow::Continue),
        }
    }

    fn query(&self, target: QueryTarget) -> Response {
        let answer = match target {
            QueryTarget::MonitorCount => serde_json::json!(self.state.monitors.len()),
            QueryTarget::WindowCount => serde_json::json!(self.state.manageable_count()),
            QueryTarget::Paused => serde_json::json!(self.state.paused),
            QueryTarget::DryRun => serde_json::json!(self.state.dry_run),
            QueryTarget::ConfigPath => {
                serde_json::json!(self.state.config_path().display().to_string())
            }
            QueryTarget::Version => serde_json::json!(self.state.version),
            QueryTarget::FocusedMonitorIndex => match self
                .state
                .focused
                .and_then(|hwnd| self.state.monitor_index_of(hwnd))
            {
                Some(index) => serde_json::json!(index),
                None => return Response::error("no window is focused on a known monitor"),
            },
            QueryTarget::FocusedWindowIndex => match self.state.focused {
                Some(hwnd) => match self.state.manageable().position(|w| w.info.hwnd == hwnd) {
                    Some(index) => serde_json::json!(index),
                    None => return Response::error("the focused window is not managed"),
                },
                None => return Response::error("no window is focused"),
            },
            other => {
                return pending(match other {
                    QueryTarget::FocusedWorkspaceIndex => "query focused-workspace-index",
                    QueryTarget::FocusedContainerIndex => "query focused-container-index",
                    QueryTarget::FocusedWorkspaceName => "query focused-workspace-name",
                    _ => "query",
                });
            }
        };
        Response::Query { answer }
    }

    fn quickstart(&self) -> Response {
        let path = self.state.config_path();
        match config::write_default(path) {
            Ok(true) => {
                tracing::info!(path = %path.display(), "wrote a default configuration");
                Response::Ok
            }
            Ok(false) => Response::error(format!("{} already exists", path.display())),
            Err(e) => Response::error(e),
        }
    }

    fn stored(&self, what: &str) -> Response {
        tracing::info!(setting = what, "stored, nothing draws it yet");
        Response::Ok
    }

    // -----------------------------------------------------------------
    // hook points
    // -----------------------------------------------------------------

    /// HOOK. Recomputes every layout and applies it.
    ///
    /// The tiling step replaces the body with: ask `mochi-core` for the
    /// rectangles of the visible workspace on every monitor, then call
    /// [`WindowManager::apply_layout`] once per monitor.
    pub fn retile(&mut self) {
        if self.state.paused {
            tracing::debug!("retile skipped, mochi is paused");
            return;
        }
        tracing::info!(
            monitors = self.state.monitors.len(),
            manageable = self.state.manageable_count(),
            "retile: no layout engine is wired up yet, nothing was moved"
        );
    }

    /// HOOK. Turns computed rectangles into one batched window move.
    ///
    /// Already complete: it is the only place that writes positions, and it is
    /// a no-op under `--dry-run` because the platform swallows the call.
    pub fn apply_layout(&self, placements: &[(Hwnd, Rect)]) {
        if self.state.paused || placements.is_empty() {
            return;
        }
        let batch: Vec<WindowPlacement> = placements
            .iter()
            .map(|&(hwnd, rect)| WindowPlacement::new(hwnd, rect))
            .collect();
        if let Err(e) = self.platform.set_positions(&batch) {
            tracing::error!(error = %e, count = batch.len(), "could not apply a layout");
        }
    }

    /// HOOK. Re-reads the configuration file.
    ///
    /// Parsing belongs to `mochi-core`. This is where the parsed document is
    /// applied to [`State`] and a retile is triggered.
    pub fn reload_config(&mut self) {
        let path = self.state.config_path().to_path_buf();
        let exists = path.exists();
        tracing::info!(
            path = %path.display(),
            exists,
            "reload: the configuration parser lives in mochi-core, nothing was applied"
        );
        self.notify(NotificationEvent::Reload {
            path: path.display().to_string(),
        });
    }

    /// HOOK. Uncloaks and restores everything Mochi touched.
    ///
    /// Delegates to the closure installed by
    /// [`WindowManager::install_restore_hook`] so that the panic hook and this
    /// call always do exactly the same thing.
    pub fn restore_all(&mut self) {
        crate::safety::restore_all();
    }

    // -----------------------------------------------------------------
    // refresh
    // -----------------------------------------------------------------

    /// Re-enumerates the monitors.
    pub fn refresh_monitors(&mut self) {
        match self.platform.monitors() {
            Ok(monitors) => {
                for m in &monitors {
                    tracing::debug!(
                        device = %m.device_name,
                        description = %m.device_description,
                        width = m.size.width(),
                        height = m.size.height(),
                        dpi = m.dpi,
                        primary = m.primary,
                        "monitor"
                    );
                }
                self.state.set_monitors(monitors);
            }
            Err(e) => tracing::error!(error = %e, "could not enumerate monitors"),
        }
    }

    /// Re-enumerates every top-level window.
    pub fn refresh_windows(&mut self) {
        match self.platform.windows() {
            Ok(windows) => {
                self.state.set_windows(windows);
                for w in self.state.manageable() {
                    tracing::debug!(
                        hwnd = %w.info.hwnd,
                        title = %w.info.title,
                        exe = %w.info.exe,
                        class = %w.info.class,
                        "manageable window"
                    );
                }
            }
            Err(e) => tracing::error!(error = %e, "could not enumerate windows"),
        }
    }

    fn notify(&self, event: NotificationEvent) {
        self.subscribers.notify(Notification::new(event));
    }
}

fn window_ref(info: &crate::platform::WindowInfo) -> WindowRef {
    WindowRef::new(info.hwnd.as_i64(), info.title.clone(), info.exe.clone())
}

/// The answer for everything that waits on the tiling model.
fn pending(what: &str) -> Response {
    Response::error(format!(
        "`{what}` is accepted by the protocol but the tiling model from mochi-core is not wired up yet"
    ))
}

/// Flips a boolean setting from a command line `enable`/`disable`.
pub fn boolean(state: BooleanState) -> bool {
    state.is_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::types::{MonitorId, style};
    use crate::platform::{MonitorInfo, ShowState, WindowInfo};
    use crate::state::State;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A platform with no desktop behind it, so the loop can be tested anywhere.
    struct FakePlatform {
        monitors: Vec<MonitorInfo>,
        windows: Vec<WindowInfo>,
        writes: AtomicUsize,
    }

    impl FakePlatform {
        fn new(windows: Vec<WindowInfo>) -> Self {
            Self {
                monitors: vec![MonitorInfo {
                    id: MonitorId(1),
                    device_name: r"\\.\DISPLAY1".into(),
                    device_description: "Fake".into(),
                    size: Rect::new(0, 0, 3840, 2160),
                    work_area: Rect::new(0, 0, 3840, 2112),
                    dpi: 144,
                    primary: true,
                }],
                windows,
                writes: AtomicUsize::new(0),
            }
        }
    }

    impl Platform for FakePlatform {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn monitors(&self) -> Result<Vec<MonitorInfo>> {
            Ok(self.monitors.clone())
        }
        fn windows(&self) -> Result<Vec<WindowInfo>> {
            Ok(self.windows.clone())
        }
        fn window_info(&self, hwnd: Hwnd) -> Result<WindowInfo> {
            self.windows
                .iter()
                .find(|w| w.hwnd == hwnd)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no such window"))
        }
        fn foreground_window(&self) -> Option<Hwnd> {
            self.windows.first().map(|w| w.hwnd)
        }
        fn window_at(&self, _x: i32, _y: i32) -> Option<Hwnd> {
            None
        }
        fn cursor_position(&self) -> Result<(i32, i32)> {
            Ok((0, 0))
        }
        fn set_positions(&self, placements: &[WindowPlacement]) -> Result<()> {
            self.writes.fetch_add(placements.len(), Ordering::SeqCst);
            Ok(())
        }
        fn set_cloaked(&self, _hwnd: Hwnd, _cloaked: bool) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn show(&self, _hwnd: Hwnd, _state: ShowState) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn focus(&self, _hwnd: Hwnd) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn close(&self, _hwnd: Hwnd) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_transparency(&self, _hwnd: Hwnd, _alpha: Option<u8>) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_topmost(&self, _hwnd: Hwnd, _topmost: bool) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_cursor_position(&self, _x: i32, _y: i32) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn window(hwnd: isize, title: &str) -> WindowInfo {
        WindowInfo {
            title: title.into(),
            class: "Chrome_WidgetWin_1".into(),
            exe: "Code.exe".into(),
            style: style::WS_VISIBLE | style::WS_CAPTION,
            rect: Rect::new(0, 0, 800, 600),
            frame: Rect::new(0, 0, 800, 600),
            visible: true,
            monitor: Some(MonitorId(1)),
            ..WindowInfo::placeholder(Hwnd(hwnd))
        }
    }

    fn manager(windows: Vec<WindowInfo>) -> (WindowManager, EventSender) {
        let platform = Arc::new(FakePlatform::new(windows));
        let (tx, rx) = std::sync::mpsc::channel();
        let state = State::new(PathBuf::from(r"C:\nowhere\mochi.json"), true);
        let wm = WindowManager::new(platform, tx.clone(), rx, state).unwrap();
        (wm, tx)
    }

    #[test]
    fn startup_enumerates_the_desktop() {
        let (wm, _tx) = manager(vec![window(1, "Editor"), window(2, "Browser")]);
        assert_eq!(wm.state().monitors.len(), 1);
        assert_eq!(wm.state().manageable_count(), 2);
        assert_eq!(wm.state().focused, Some(Hwnd(1)));
    }

    #[test]
    fn state_and_query_are_answered_from_the_flat_model() {
        let (mut wm, _tx) = manager(vec![window(1, "Editor")]);

        let (response, flow) = wm.handle_command(Command::State);
        assert_eq!(flow, Flow::Continue);
        match response {
            Response::State { state } => assert_eq!(state["windows"]["1"]["title"], "Editor"),
            other => panic!("unexpected response: {other:?}"),
        }

        for (target, expected) in [
            (QueryTarget::MonitorCount, serde_json::json!(1)),
            (QueryTarget::WindowCount, serde_json::json!(1)),
            (QueryTarget::Paused, serde_json::json!(false)),
            (QueryTarget::DryRun, serde_json::json!(true)),
            (QueryTarget::FocusedMonitorIndex, serde_json::json!(0)),
            (QueryTarget::FocusedWindowIndex, serde_json::json!(0)),
        ] {
            match wm.handle_command(Command::Query { target }).0 {
                Response::Query { answer } => assert_eq!(answer, expected, "for {target}"),
                other => panic!("unexpected response for {target}: {other:?}"),
            }
        }
    }

    #[test]
    fn stop_ends_the_loop_and_answers_ok() {
        let (mut wm, _tx) = manager(vec![]);
        let (response, flow) = wm.handle_command(Command::Stop { whkd: false });
        assert_eq!(response, Response::Ok);
        assert_eq!(flow, Flow::Stop);
    }

    #[test]
    fn pause_toggles_and_is_visible_in_the_state() {
        let (mut wm, _tx) = manager(vec![]);
        assert!(!wm.state().paused);
        wm.handle_command(Command::TogglePause);
        assert!(wm.state().paused);
        wm.handle_command(Command::TogglePause);
        assert!(!wm.state().paused);
    }

    #[test]
    fn tiling_commands_answer_with_a_clear_not_yet() {
        let (mut wm, _tx) = manager(vec![window(1, "Editor")]);
        for command in [
            Command::Focus {
                direction: mochi_client::Direction::Left,
            },
            Command::Move {
                direction: mochi_client::Direction::Right,
            },
            Command::FocusWorkspace { index: 2 },
            Command::Promote,
            Command::ToggleMonocle,
        ] {
            let name = command.name();
            let (response, flow) = wm.handle_command(command);
            assert_eq!(flow, Flow::Continue);
            let message = response.error_message().unwrap_or_default().to_owned();
            assert!(message.contains(name), "{name}: {message}");
            assert!(message.contains("mochi-core"), "{name}: {message}");
        }
    }

    #[test]
    fn focus_follows_mouse_is_switched_at_runtime() {
        let (mut wm, _tx) = manager(vec![window(1, "Editor")]);
        assert!(!wm.state().settings.focus_follows_mouse);
        let (response, _) = wm.handle_command(Command::FocusFollowsMouse {
            state: BooleanState::Enable,
        });
        assert_eq!(response, Response::Ok);
        assert!(wm.state().settings.focus_follows_mouse);
        assert!(boolean(BooleanState::Enable));
    }

    #[test]
    fn a_destroyed_window_leaves_the_state() {
        let (mut wm, _tx) = manager(vec![window(1, "Editor")]);
        assert_eq!(wm.state().windows.len(), 1);
        wm.on_window_event(WindowEventKind::Destroyed, Hwnd(1));
        assert_eq!(wm.state().windows.len(), 0);
    }

    #[test]
    fn a_paused_manager_moves_nothing() {
        let platform = Arc::new(FakePlatform::new(vec![window(1, "Editor")]));
        let (tx, rx) = std::sync::mpsc::channel();
        let state = State::new(PathBuf::from(r"C:\nowhere\mochi.json"), true);
        let mut wm =
            WindowManager::new(Arc::clone(&platform) as Arc<dyn Platform>, tx, rx, state).unwrap();

        wm.handle_command(Command::TogglePause);
        wm.apply_layout(&[(Hwnd(1), Rect::new(0, 0, 100, 100))]);
        assert_eq!(platform.writes.load(Ordering::SeqCst), 0);

        wm.handle_command(Command::TogglePause);
        wm.apply_layout(&[(Hwnd(1), Rect::new(0, 0, 100, 100))]);
        assert_eq!(platform.writes.load(Ordering::SeqCst), 1);
    }
}
