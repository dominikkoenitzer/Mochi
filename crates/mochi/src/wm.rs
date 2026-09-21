//! The window manager loop.
//!
//! One thread owns the [`mochi_core::State`] tree and consumes one channel.
//! Every other thread in the daemon is a producer: the WinEvent hooks, the
//! hidden message window, the mouse tracker, the configuration watcher and the
//! IPC server. There is no shared mutable state and therefore no lock ordering
//! to get wrong.
//!
//! # The shape of one turn
//!
//! Everything that changes the desktop follows the same three steps: translate
//! the event or the command into a call on [`mochi_core::State`], take the
//! [`Changes`] it hands back, and apply them to real windows in one private
//! `apply_changes` step. Nothing else in the daemon writes to a window.
//!
//! # Visuals
//!
//! Borders, transparency and animations are drawn by [`crate::visuals`]. The
//! configuration they were built from is kept in `visual_config`, so a
//! `mochic border-width` can change one key and push it into the managers
//! straight away instead of waiting for the next reload.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use mochi_client::{
    BooleanState, Command, Notification, NotificationEvent, QueryTarget, Response, WindowRef,
};
use mochi_core::config::{AnimationConfig, Colour, Config};
use mochi_core::model::{HidingBehaviour, Monitor, Window, WindowId};
use mochi_core::rules::{
    ApplicationIdentifier, MatchingRule, MatchingStrategy, RuleDecision, WindowInfo as RuleInfo,
};
use mochi_core::{Changes, Rect, State as CoreState};

use crate::config;
use crate::events::hotkey::{Gate, HotkeyDaemon};
use crate::events::{Event, EventReceiver, EventSender, MonitorEventKind, WindowEventKind};
use crate::ipc::Subscribers;
use crate::platform::types::FRAME_WINDOW_CLASS;
use crate::platform::types::Unmanageable;
use crate::platform::{
    CloakUnsupported, Hwnd, MonitorInfo, Platform, ShowState, WindowInfo, is_manageable_with,
};
use crate::state::{State, snapshot};

/// What a command that needs the keyboard hook answers without one.
const NO_HOTKEYS: &str = "this daemon binds no keys, it was started with --no-hotkeys";

/// What a command that would change the desktop answers while paused.
const PAUSED: &str = "mochi is paused, nothing was changed";

/// How long a `slow_application_identifiers` window is given to finish opening
/// before the layout is applied to it a second time.
const SLOW_APPLICATION_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

/// The `mochic hotkeys` rows of a set of bindings, in file order.
fn rows_of(bindings: &mochi_hotkey::Bindings) -> Vec<(String, String)> {
    bindings
        .iter()
        .map(|binding| (binding.trigger.to_string(), binding.source.clone()))
        .collect()
}

/// The freshly loaded configuration, with every visual key the file does not
/// carry taken from the one in force.
///
/// The same rule `Settings::apply` follows, applied to the copy the border,
/// transparency and animation managers are built from, so the two cannot drift
/// apart. A file that *does* set a key wins, which is what makes editing the
/// file the way to undo a command.
fn keeping_visuals(mut loaded: Config, live: &Config) -> Config {
    loaded.border = loaded.border.or(live.border);
    loaded.border_width = loaded.border_width.or(live.border_width);
    loaded.border_offset = loaded.border_offset.or(live.border_offset);
    loaded.border_style = loaded.border_style.or(live.border_style);
    loaded.border_colours = loaded.border_colours.or(live.border_colours);
    loaded.transparency = loaded.transparency.or(live.transparency);
    loaded.transparency_alpha = loaded.transparency_alpha.or(live.transparency_alpha);

    match (loaded.animation.as_mut(), live.animation) {
        (Some(fresh), Some(current)) => {
            fresh.enabled = fresh.enabled.or(current.enabled);
            fresh.duration = fresh.duration.or(current.duration);
            fresh.style = fresh.style.or(current.style);
            fresh.fps = fresh.fps.or(current.fps);
        }
        (None, Some(current)) => loaded.animation = Some(current),
        _ => {}
    }

    loaded
}

/// The hotkey file to read on a reload.
///
/// The file is accepted under two names, and renaming it from the borrowed one
/// to Mochi's own is a thing a user does exactly once, on purpose. Re-reading
/// whichever name happened to exist at startup would find nothing there, and a
/// missing hotkey file is deliberately not an error, so the whole keyboard
/// would come unbound without a word being logged.
///
/// `candidates` is empty when the path was named rather than found, and then
/// the named path is the only answer: `--hotkeys` means that file.
fn hotkey_file_now(current: &std::path::Path, candidates: &[PathBuf]) -> PathBuf {
    candidates
        .iter()
        .find(|candidate| candidate.exists())
        .cloned()
        .unwrap_or_else(|| current.to_path_buf())
}

/// Which of the monitors we already had is the one this enumeration describes.
///
/// The obvious answer, the GDI device name, is the wrong one. `\\.\DISPLAY1` is
/// whichever screen Windows is currently calling the first, and a DisplayPort
/// renegotiation reassigns it: the panels come back in the other order, the name
/// follows the order rather than the hardware, and a workspace carried across by
/// name lands on the other screen. The `HMONITOR` behind it is no better, since
/// it is not promised to survive a reconfiguration at all.
///
/// Geometry is what survives. Two monitors cannot occupy the same rectangle of
/// the virtual desktop, so the rectangle identifies a screen exactly, and it is
/// unchanged by a renegotiation that only renames things. The name and the
/// handle are still tried afterwards, because a screen whose resolution changed
/// or that was moved in the display settings has a new rectangle and has to be
/// recognised by something.
///
/// What this cannot tell apart: two panels that swapped *places*. Their
/// rectangles swap with them, so the workspaces stay with the position rather
/// than following the hardware. No identifier available here decides that one.
/// `EnumDisplayDevicesW` gives a model name, not a serial, and on plenty of
/// machines every panel reports `Generic PnP Monitor`, this user's included.
fn same_panel(previous: &[Monitor], info: &MonitorInfo) -> Option<usize> {
    previous
        .iter()
        .position(|m| m.size == info.size)
        .or_else(|| {
            previous
                .iter()
                .position(|m| !m.device.is_empty() && m.device == info.device_name)
        })
        .or_else(|| previous.iter().position(|m| m.id == info.id.0))
}

/// Whether the loop keeps going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Wait for the next event.
    Continue,
    /// Leave the loop and shut down.
    Stop,
}

/// Every window Mochi has taken off screen, and how.
///
/// This is the list [`WindowManager::restore_all`] and the panic hook work
/// from, so it has to be exact: an entry that is not really hidden costs one
/// pointless call, a hidden window that is *missing* from here is a window the
/// user cannot get back without another window manager.
#[derive(Debug, Default)]
pub struct Hidden {
    windows: BTreeMap<Hwnd, HidingBehaviour>,
    faded: BTreeSet<Hwnd>,
    /// Where the record is mirrored so a crash can be undone, and what is
    /// needed to tell a reused handle from the window that was hidden.
    record: Option<PathBuf>,
    identity: BTreeMap<Hwnd, (u32, String)>,
}

impl Hidden {
    /// Mirrors every change to `path`, so the next start can undo a crash.
    pub fn with_record(path: PathBuf) -> Self {
        Self {
            record: Some(path),
            ..Self::default()
        }
    }

    /// Takes over the entries a previous session could not put back.
    ///
    /// Written straight into the in-memory sets without touching the file:
    /// what is on disk already says exactly this, and rewriting it here would
    /// be a no-op that could only go wrong.
    pub fn adopt(&mut self, entries: Vec<crate::recover::Entry>) {
        for entry in entries {
            let hwnd = Hwnd(entry.hwnd);
            if entry.pid != 0 || !entry.class.is_empty() {
                self.identity.insert(hwnd, (entry.pid, entry.class));
            }
            if let Some(behaviour) = entry.behaviour {
                self.windows.insert(hwnd, behaviour);
            }
            if entry.faded {
                self.faded.insert(hwnd);
            }
        }
    }

    /// Remembers how to recognise a window, so a reused handle is not touched.
    pub fn identify(&mut self, hwnd: Hwnd, pid: u32, class: &str) {
        self.identity.insert(hwnd, (pid, class.to_owned()));
    }

    /// Records that a window was taken off screen with `behaviour`.
    pub fn hide(&mut self, hwnd: Hwnd, behaviour: HidingBehaviour) {
        self.windows.insert(hwnd, behaviour);
        self.write();
    }

    /// How a window was taken off screen, without forgetting it.
    ///
    /// The read half of [`Hidden::show`], so a caller can find out what to
    /// undo, undo it, and only then let go of the record.
    pub fn behaviour_of(&self, hwnd: Hwnd) -> Option<HidingBehaviour> {
        self.windows.get(&hwnd).copied()
    }

    /// Forgets a window and reports how it had been hidden.
    pub fn show(&mut self, hwnd: Hwnd) -> Option<HidingBehaviour> {
        let previous = self.windows.remove(&hwnd);
        if previous.is_some() {
            // Nothing is holding this handle any more, so the note on how to
            // recognise it goes too. It used to be kept until the next restore,
            // which is once per session: the map grew by one entry for every
            // distinct window ever hidden, and a stale entry could later be
            // written against a handle Windows had recycled for someone else.
            if !self.faded.contains(&hwnd) {
                self.identity.remove(&hwnd);
            }
            self.write();
        }
        previous
    }

    /// Writes the mirror, if there is one.
    fn write(&self) {
        let Some(path) = self.record.as_deref() else {
            return;
        };
        // Every window Mochi has touched, not only the hidden ones. A window
        // that is merely faded is still Mochi's doing and is left translucent
        // by a hard kill, with nothing on disk that says who did it or that it
        // is on screen at all.
        let mut handles: BTreeSet<Hwnd> = self.windows.keys().copied().collect();
        handles.extend(self.faded.iter().copied());
        let entries: Vec<crate::recover::Entry> = handles
            .into_iter()
            .map(|hwnd| {
                let (pid, class) = self.identity.get(&hwnd).cloned().unwrap_or_default();
                crate::recover::Entry {
                    hwnd: hwnd.0,
                    pid,
                    class,
                    behaviour: self.windows.get(&hwnd).copied(),
                    faded: self.faded.contains(&hwnd),
                }
            })
            .collect();
        crate::recover::save(path, &entries);
    }

    /// Every window Mochi currently has off screen.
    pub fn handles(&self) -> Vec<Hwnd> {
        self.windows.keys().copied().collect()
    }

    /// True when Mochi is the reason the window is off screen.
    pub fn contains(&self, hwnd: Hwnd) -> bool {
        self.windows.contains_key(&hwnd)
    }

    /// Records that Mochi set an alpha value on a window.
    pub fn fade(&mut self, hwnd: Hwnd) {
        // Only when something actually changed, the way `unfade` already does
        // it. The caller re-fades the whole unfocused set on every pass, so an
        // unconditional write meant one full serialise-and-rename of the crash
        // record per faded window per focus change — a dozen windows open and
        // one alt-tab was a dozen file writes, synchronously, on the thread
        // that also has to answer every hotkey.
        if self.faded.insert(hwnd) {
            self.write();
        }
    }

    /// Forgets that Mochi had faded a window, because it has since been put
    /// back to opaque. Does not touch the window itself.
    pub fn unfade(&mut self, hwnd: Hwnd) {
        if self.faded.remove(&hwnd) {
            self.write();
        }
    }

    /// How many windows are off screen because of Mochi.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// True when Mochi is hiding nothing.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.faded.is_empty()
    }

    /// Everything Mochi owes the user back, emptying the in-memory record.
    ///
    /// The mirror on disk is deliberately left alone here. It is the only
    /// thing that knows where these windows went, the restore loop that
    /// follows is not instant, and the console close path is killed by the OS
    /// after a fixed timeout: erasing the file first means every window that
    /// did not get its turn is lost with nothing left pointing at it. A record
    /// that still names a window already back on screen costs one harmless
    /// call on the next start. [`Hidden::settle`] replaces it at the end.
    fn drain(&mut self) -> (Vec<(Hwnd, HidingBehaviour)>, Vec<Hwnd>) {
        (
            std::mem::take(&mut self.windows).into_iter().collect(),
            std::mem::take(&mut self.faded).into_iter().collect(),
        )
    }

    /// Takes back whatever the restore could not deal with and rewrites the
    /// mirror, which empties it when there is nothing left.
    fn settle(&mut self, windows: Vec<(Hwnd, HidingBehaviour)>, faded: Vec<Hwnd>) {
        self.windows.extend(windows);
        self.faded.extend(faded);
        let live: Vec<Hwnd> = self
            .windows
            .keys()
            .copied()
            .chain(self.faded.iter().copied())
            .collect();
        self.identity.retain(|hwnd, _| live.contains(hwnd));
        self.write();
    }
}

/// Whether this command is one that acts on whatever window is focused.
///
/// `unmanaged_window_operation_behaviour` is documented as deciding whether a
/// command aimed at a window Mochi does not manage runs anyway or is refused,
/// and the setting was stored, reported by `mochic state` and read by nothing:
/// under `no-op` every one of these behaved exactly as under `op`.
///
/// Only commands that reach for the focused window are listed. Anything that
/// names its own target, asks a question, or changes a global setting is not
/// aimed at a window at all and is never refused.
fn acts_on_the_focused_window(command: &Command) -> bool {
    matches!(
        command,
        Command::Focus { .. }
            | Command::CycleFocus { .. }
            | Command::Move { .. }
            | Command::CycleMove { .. }
            | Command::ResizeAxis { .. }
            | Command::ResizeEdge { .. }
            | Command::Promote
            | Command::PromoteFocus
            | Command::ToggleFloat
            | Command::ToggleMaximize
            | Command::ToggleMonocle
            | Command::Minimize
            | Command::Close
            | Command::Unmanage
            | Command::Stack { .. }
            | Command::Unstack
            | Command::StackAll
            | Command::UnstackAll
            | Command::CycleStack { .. }
            | Command::FocusStackWindow { .. }
            | Command::MoveToWorkspace { .. }
            | Command::SendToWorkspace { .. }
            | Command::MoveToNamedWorkspace { .. }
            | Command::SendToNamedWorkspace { .. }
            | Command::MoveToMonitor { .. }
            | Command::SendToMonitor { .. }
    )
}

/// The hiding record, whatever state the mutex is in.
///
/// Poison is deliberately ignored, as it already is on the restore path. It
/// used to make every other reader skip itself, and `hide_window` is one of
/// them: it still cloaked the window, then failed to write it down, so the
/// window went off screen with no entry in memory and none on disk. Not in
/// the restore hook, not in the crash mirror, invisible to `restore-windows`.
/// The data behind this mutex is a map of window handles; a panic elsewhere
/// cannot make it meaningless, and pretending that it did is what loses a
/// window for good.
fn record(hidden: &Mutex<Hidden>) -> std::sync::MutexGuard<'_, Hidden> {
    hidden
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Puts every window Mochi took off screen back, and clears any alpha it set.
///
/// The body of the restore hook, as a free function so that the panic hook, the
/// shutdown path and the tests all run exactly the same code. It empties the
/// record as it goes, so calling it twice is harmless.
pub fn restore(platform: &dyn Platform, hidden: &Mutex<Hidden>) {
    let (windows, faded) = match hidden.lock() {
        Ok(mut list) => list.drain(),
        Err(poisoned) => poisoned.into_inner().drain(),
    };
    if windows.is_empty() && faded.is_empty() {
        tracing::info!("restore: mochi is not hiding a window");
        return;
    }
    tracing::warn!(
        count = windows.len(),
        "restore: putting windows back on screen"
    );
    let mut owed = Vec::new();
    for (hwnd, behaviour) in windows {
        let result = match behaviour {
            HidingBehaviour::Cloak => platform.set_cloaked(hwnd, false),
            HidingBehaviour::Minimize => platform.show(hwnd, ShowState::Restore),
            HidingBehaviour::Hide => platform.show(hwnd, ShowState::ShowNoActivate),
        };
        if let Err(e) = result {
            // A window that now outranks Mochi will refuse this call today and
            // every time after, so writing it down again only guarantees the
            // same error at the next stop and the next start. The record is
            // what `mochic stop` keeps its promises from, and a promise that
            // can never be kept is worse than an honest refusal: it hides the
            // one window the user actually has to go and rescue by hand.
            if platform.outranks_us(hwnd) {
                tracing::warn!(
                    %hwnd,
                    "restore: mochi took this window off screen and can no longer put it back,                      because the window now outranks it. Bring it back from the taskbar or with                      alt+tab. Mochi is letting go of it rather than promising again"
                );
                continue;
            }
            tracing::error!(%hwnd, error = %e, "restore: could not show a window");
            owed.push((hwnd, behaviour));
        }
    }
    let mut still_faded = Vec::new();
    for hwnd in faded {
        if let Err(e) = platform.set_transparency(hwnd, None) {
            tracing::error!(%hwnd, error = %e, "restore: could not clear the alpha");
            still_faded.push(hwnd);
        }
    }
    // Only now, with every window either back or written down again.
    match hidden.lock() {
        Ok(mut list) => list.settle(owed, still_faded),
        Err(poisoned) => poisoned.into_inner().settle(owed, still_faded),
    }
}

/// A `workspace_rules` entry: where a window matching `rule` opens.
#[derive(Debug, Clone)]
struct WorkspaceRule {
    monitor: usize,
    workspace: usize,
    rule: MatchingRule,
    /// True for an `initial_workspace_rules` entry, which only applies the
    /// first time a window is seen.
    initial_only: bool,
}

/// What was focused, so a change can be announced to the subscribers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Focus {
    monitor: usize,
    workspace: usize,
    window: Option<WindowId>,
}

/// Owns the state and reacts to everything.
pub struct WindowManager {
    platform: Arc<dyn Platform>,
    rx: EventReceiver,
    tx: EventSender,
    /// The tiling model. Every command is a method on this.
    core: CoreState,
    /// The facts about this process that the model does not carry.
    session: State,
    subscribers: Subscribers,
    hidden: Arc<Mutex<Hidden>>,
    mouse: Option<crate::events::mouse::MouseTracker>,
    /// Classes `--manage-class` forces into management.
    manage_classes: Vec<String>,
    /// Where a new window opens, from `workspace_rules`.
    workspace_rules: Vec<WorkspaceRule>,
    /// Windows that have been routed once, for `initial_workspace_rules`.
    routed: HashSet<Hwnd>,
    /// Windows Mochi dropped from the model because the user minimized them.
    minimized: HashSet<Hwnd>,
    /// Rules a `mochic` command added, and the workspace rules with them.
    ///
    /// Kept because a reload REPLACES the rule sets rather than merging them:
    /// `Config::apply_to` assigns `state.rules` outright, unlike every other
    /// key, which is only written when the file names it. Without this, typing
    /// `mochic ignore-rule exe wallpaper64.exe equals` and then editing any
    /// unrelated key in mochi.json silently threw the rule away, and the
    /// application it was keeping out started being tiled again with nothing
    /// logged. The visuals are already deliberately sticky across a reload for
    /// the same reason; rules were the one thing that was not.
    added_rules: Vec<(RuleDecision, MatchingRule)>,
    /// Workspace rules a `mochic` command added, rebuilt after every reload.
    added_workspace_rules: Vec<WorkspaceRule>,
    /// The foreground window, as far as Mochi knows.
    foreground: Option<Hwnd>,
    /// The configuration the visuals were built from. Every tiling key ends
    /// up in the model, but these have no home except here, and a command
    /// that changes one has to hand the whole set back to the managers.
    visual_config: Config,
    /// Borders, transparency and animation. Optional in every part; a
    /// setting that is off means the matching manager does not exist.
    visuals: crate::visuals::Visuals,
    /// The keyboard hook, when this daemon binds keys at all.
    hotkeys: Option<HotkeyDaemon>,
    /// The hotkey file, for a reload and for `mochic hotkeys`.
    hotkey_path: Option<PathBuf>,
    /// The gate and the pause to put back when game mode ends.
    before_game_mode: Option<(Gate, bool)>,
    /// Every name the hotkey file is accepted under, in preference order.
    ///
    /// Empty when the path was named on the command line or in the
    /// environment: that names one file and nothing else counts.
    hotkey_candidates: Vec<PathBuf>,
    /// One row per binding, the way `mochic hotkeys` prints them. Kept here
    /// rather than read back from the hook thread, which owns the bindings and
    /// must not be asked questions while it is matching key presses.
    hotkey_rows: Vec<(String, String)>,
    /// The lines of the hotkey file that did not parse.
    hotkey_errors: Vec<String>,
    /// Every notification this manager sent, recorded in tests only.
    ///
    /// [`Subscribers`] writes to named pipes, so a test that has not connected
    /// one has no way to see what went out, and "an event fired that should
    /// not have" is exactly what the tests below check.
    #[cfg(test)]
    sent: Mutex<Vec<NotificationEvent>>,
}

impl WindowManager {
    /// Builds the manager, reads the configuration and takes over the desktop.
    ///
    /// The order matters: the monitors have to exist before the configuration
    /// can give them workspaces, and the workspaces have to exist before a
    /// window can be routed onto one.
    ///
    /// # Errors
    ///
    /// When the subscriber thread cannot be started. A configuration that does
    /// not parse is logged and the defaults are used, because a window manager
    /// that refuses to start leaves the user with no way to move a window.
    pub fn new(
        platform: Arc<dyn Platform>,
        tx: EventSender,
        rx: EventReceiver,
        session: State,
    ) -> Result<Self> {
        // A previous session may have been killed while windows were off
        // screen. Put those back before anything else looks at the desktop,
        // so they are enumerated and tiled like any other window.
        // A dry run takes no window off screen, so it has nothing to record and
        // nothing to recover. It must also not so much as open the file. The
        // record belongs to whichever daemon is really holding windows off
        // screen, there is exactly one of them per user, and every unit test in
        // this module builds its manager this way: pointed at the real path, a
        // test run replaced a live daemon's record with fake handles, and the
        // windows that daemon genuinely had cloaked would have had nothing left
        // on disk to bring them back the moment it stopped.
        let mut carried = Hidden::default();
        if session.dry_run {
            tracing::debug!("dry run: the off-screen record is left alone");
        } else {
            let record = crate::recover::default_path();
            let recovered = crate::recover::recover(platform.as_ref(), &record);
            if !recovered.back.is_empty() {
                tracing::info!(
                    count = recovered.back.len(),
                    "brought back windows a previous session left off screen"
                );
            }

            // Whatever the recovery could not finish is adopted, not started
            // over. A fresh record rewrites the file on its first hide, and the
            // entries the last session was still owed were the only thing that
            // knew those windows exist: not in the model, not enumerable while
            // cloaked, and gone from disk the moment any other window went off
            // screen.
            carried = Hidden::with_record(record);
            if !recovered.unfinished.is_empty() {
                tracing::warn!(
                    count = recovered.unfinished.len(),
                    "still holding windows a previous session could not put back"
                );
                carried.adopt(recovered.unfinished);
            }
        }
        let hidden = Arc::new(Mutex::new(carried));
        let visuals = crate::visuals::Visuals::new(Arc::clone(&platform), Arc::clone(&hidden));

        let mut wm = Self {
            platform,
            rx,
            tx,
            core: CoreState::new(),
            manage_classes: session.manage_classes.clone(),
            session,
            subscribers: Subscribers::start()?,
            hidden,
            mouse: None,
            workspace_rules: Vec::new(),
            routed: HashSet::new(),
            minimized: HashSet::new(),
            added_rules: Vec::new(),
            added_workspace_rules: Vec::new(),
            foreground: None,
            visual_config: Config::default(),
            visuals,
            hotkeys: None,
            hotkey_path: None,
            before_game_mode: None,
            hotkey_candidates: Vec::new(),
            hotkey_rows: Vec::new(),
            hotkey_errors: Vec::new(),
            #[cfg(test)]
            sent: Mutex::new(Vec::new()),
        };

        wm.refresh_monitors();
        let _ = wm.load_config();
        wm.adopt_existing_windows();
        // The model starts focused on the first monitor's first workspace,
        // which is rarely the screen the user is looking at. Caching the
        // foreground handle was not enough: nothing pointed the MODEL at it,
        // so until the user happened to click something, every command that
        // works from the focus acted on whichever monitor enumerated first. On
        // a desk where that is a second screen with nothing on it, a focus or
        // move binding pressed straight after a start did nothing at all.
        //
        // `window_focused` is the same path a real foreground event takes, and
        // it deliberately does not ask Windows for the foreground back,
        // because the window already has it.
        if let Some(foreground) = wm.platform.foreground_window() {
            wm.window_focused(foreground);
        }
        wm.retile();
        Ok(wm)
    }

    /// Hands the mouse tracker over so `focus-follows-mouse` can toggle it.
    pub fn attach_mouse_tracker(&mut self, tracker: crate::events::mouse::MouseTracker) {
        tracker.set_enabled(self.core.focus_follows_mouse.is_some());
        self.mouse = Some(tracker);
    }

    /// Reads the hotkey file and installs the keyboard hook.
    ///
    /// Failing to install the hook is not fatal. A window manager that refuses
    /// to start because a key could not be bound would leave the user with an
    /// untiled desktop over something `mochic` can still do by hand.
    pub fn start_hotkeys(&mut self, path: PathBuf, candidates: Vec<PathBuf>) {
        let (bindings, errors) = config::load_hotkeys(&path);
        self.hotkey_rows = rows_of(&bindings);
        self.hotkey_errors = errors;
        self.hotkey_path = Some(path);
        self.hotkey_candidates = candidates;

        match HotkeyDaemon::start(self.tx.clone(), bindings) {
            Ok(daemon) => self.hotkeys = Some(daemon),
            Err(e) => tracing::error!(error = %e, "no keyboard hook, Mochi binds no keys"),
        }
    }

    /// Re-reads the hotkey file and hands the bindings to the hook thread.
    pub fn reload_hotkeys(&mut self) {
        let Some(mut path) = self.hotkey_path.clone() else {
            return;
        };
        let found = hotkey_file_now(&path, &self.hotkey_candidates);
        if found != path {
            tracing::info!(
                from = %path.display(),
                to = %found.display(),
                "the hotkey file is under its other name now"
            );
            path = found;
            self.hotkey_path = Some(path.clone());
        }
        let (bindings, errors) = config::load_hotkeys(&path);
        self.hotkey_rows = rows_of(&bindings);
        self.hotkey_errors = errors;

        // Game mode admits exactly one action, so a file saved without a
        // binding for it leaves every key swallowed and nothing able to lift
        // the suspension. That is not a hypothetical moment: it is exactly when
        // somebody is editing their bindings, and the file is reloaded the
        // instant they save. The keyboard is not Mochi's to keep.
        let can_leave = bindings.iter().any(|binding| {
            matches!(
                binding.action,
                mochi_hotkey::Action::Command(Command::ToggleGameMode)
            )
        });

        if let Some(hotkeys) = self.hotkeys.as_mut() {
            hotkeys.replace(bindings);
        }

        let stuck = self
            .hotkeys
            .as_ref()
            .is_some_and(|hotkeys| hotkeys.gate() == Gate::GameMode)
            && !can_leave;
        if stuck {
            tracing::warn!(
                "the reloaded hotkey file has no binding for toggle-game-mode, so game                  mode would have no way out; leaving it"
            );
            let _ = self.leave_game_mode("the reloaded file cannot toggle it");
            if let Some(hotkeys) = self.hotkeys.as_mut() {
                hotkeys.set_gate(Gate::All);
            }
        }
    }

    /// Whether the window the user is actually looking at is one Mochi owns.
    fn foreground_is_managed(&self) -> bool {
        self.foreground
            .or_else(|| self.platform.foreground_window())
            .is_some_and(|hwnd| self.core.is_managed(window_id(hwnd)))
    }

    /// Whether a change to `path` is a change to the hotkey file.
    ///
    /// Not equality with the path in hand. The watcher reports the name it was
    /// started on for the rest of the session, while `reload_hotkeys` moves
    /// `hotkey_path` to the other accepted name the moment the file is renamed
    /// to it. Comparing the two means that after exactly one rename every
    /// later save of the hotkey file is mistaken for a change to `mochi.json`,
    /// which reloads the configuration: the bindings never get re-read, and
    /// the reload throws away the rules and layouts a command had set.
    fn is_the_hotkey_file(&self, path: &std::path::Path) -> bool {
        if self.hotkey_path.as_deref() == Some(path) {
            return true;
        }
        // Only when the path was found rather than named; `--hotkeys` means
        // that file, and nothing else counts as the hotkey file.
        self.hotkey_candidates.iter().any(|name| name == path)
    }

    /// The hotkey file this daemon watches, if it binds keys at all.
    pub fn hotkey_path(&self) -> Option<&std::path::Path> {
        self.hotkey_path.as_deref()
    }

    /// Removes the keyboard hook.
    ///
    /// Called the moment the loop stops rather than left to the drop at the end
    /// of `main`: between those two points nothing reads the channel, and a
    /// bound key would be swallowed for a shutdown that can no longer act on it.
    pub fn stop_hotkeys(&mut self) {
        if let Some(mut hotkeys) = self.hotkeys.take() {
            hotkeys.stop();
        }
    }

    /// A sender for producers that are created after the manager.
    pub fn sender(&self) -> EventSender {
        self.tx.clone()
    }

    /// The windows Mochi has taken off screen, and therefore owes the user back.
    ///
    /// [`WindowManager::restore_all`] and the panic hook read it, which is why
    /// it is an `Arc<Mutex<..>>` rather than a plain field.
    pub fn hidden(&self) -> Arc<Mutex<Hidden>> {
        Arc::clone(&self.hidden)
    }

    /// Installs the restore hook that runs on panic and on the way out.
    pub fn install_restore_hook(&self) {
        let platform = Arc::clone(&self.platform);
        let hidden = Arc::clone(&self.hidden);
        crate::safety::set_restore_hook(move || restore(platform.as_ref(), &hidden));
    }

    /// Runs until a `stop` command or a closed channel.
    ///
    /// # Errors
    ///
    /// Never today; the result keeps the shutdown path in `main` uniform.
    pub fn run(&mut self) -> Result<()> {
        tracing::info!(
            platform = self.platform.name(),
            monitors = self.core.monitors().len(),
            windows = self.core.all_window_ids().count(),
            classes = ?self.manage_classes,
            "mochi is up"
        );

        while let Ok(event) = self.rx.recv() {
            if self.on_event(event) == Flow::Stop {
                break;
            }
        }

        self.visuals.stop();
        self.subscribers
            .notify(Notification::new(NotificationEvent::Stop));
        tracing::info!("the event loop has ended");
        Ok(())
    }

    /// Where every managed window actually is, read off the desktop.
    ///
    /// The model holds no rectangle, so `mochic state` could only ever report
    /// the tile a window was ASSIGNED. That reads as perfectly placed for the
    /// one case somebody is running the command to diagnose: a window that
    /// could not be moved, could not be uncloaked, or simply ignored the
    /// rectangle it was handed. This is measured, so the two can be compared.
    ///
    /// A window that cannot be read is left out rather than guessed at; that
    /// is what one which has just died looks like.
    fn on_screen(&self) -> crate::state::OnScreen {
        self.core
            .all_window_ids()
            .filter_map(|id| {
                let hwnd = handle(id);
                let info = self.platform.window_info(hwnd).ok()?;
                Some((
                    id.get(),
                    (info.visible_frame(), info.visible && !info.cloaked),
                ))
            })
            .collect()
    }

    /// The tiling model, for tests and for `mochic state`.
    pub fn state(&self) -> &CoreState {
        &self.core
    }

    /// The daemon facts around the model.
    pub fn session(&self) -> &State {
        &self.session
    }

    // -----------------------------------------------------------------
    // startup
    // -----------------------------------------------------------------

    /// Reads `mochi.json`, applies it to the model and remembers the routing.
    ///
    /// # Errors
    ///
    /// When the file is there and does not parse. The previous configuration
    /// is kept, and the caller has to say so rather than report success for a
    /// file nothing was read from.
    fn load_config(&mut self) -> Result<()> {
        let path = self.session.config_path.clone();
        let loaded = match config::load(&path) {
            Ok(loaded) => loaded,
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "keeping the previous configuration");
                return Err(e);
            }
        };

        loaded.config.apply_to(&mut self.core);
        self.core.rules.extend(loaded.app_rules);
        // After the file and the community list, so a rule the user typed still
        // wins the same arguments it won before the reload.
        for (decision, rule) in std::mem::take(&mut self.added_rules) {
            self.push_rule(decision, rule.clone());
            self.added_rules.push((decision, rule));
        }
        for broken in self.core.rules.drop_invalid() {
            tracing::warn!(error = %broken, "a rule was dropped");
        }
        self.session.settings.apply(&loaded.config);
        // Not a plain replace. `Settings::apply` is deliberately sticky: a key
        // the file does not mention keeps whatever a `mochic` command set. The
        // managers are built from this second copy, so replacing it wholesale
        // made the two disagree permanently, and they are meant to be the same
        // answer. `mochic state` would go on reporting transparency that had
        // just been switched off underneath it, and the next toggle would read
        // the stale value and write the state it was already in.
        self.visual_config = keeping_visuals(loaded.config.clone(), &self.visual_config);
        self.visuals.set_settings(&self.visual_config);
        self.session.app_config_path.clone_from(&loaded.app_path);

        self.workspace_rules = loaded
            .config
            .workspace_rules(&self.core)
            .into_iter()
            .map(|(monitor, workspace, rule, initial_only)| WorkspaceRule {
                monitor,
                workspace,
                rule,
                initial_only,
            })
            .collect();
        // Same reasoning as the rules above: the file's list replaces this one
        // wholesale, so what a command added has to be put back.
        self.workspace_rules
            .extend(self.added_workspace_rules.iter().cloned());

        if let Some(mouse) = &self.mouse {
            mouse.set_enabled(self.core.focus_follows_mouse.is_some());
        }

        tracing::info!(
            path = %path.display(),
            present = loaded.present,
            rules = self.core.rules.len(),
            workspace_rules = self.workspace_rules.len(),
            padding = self.core.default_workspace_padding,
            "configuration applied"
        );
        Ok(())
    }

    /// Takes over every window that is already on the desktop.
    fn adopt_existing_windows(&mut self) {
        let Ok(windows) = self.platform.windows() else {
            tracing::error!("could not enumerate the windows, managing nothing");
            return;
        };
        for info in windows {
            let Some(info) = self.uncloak_stale(info) else {
                continue;
            };
            if !self.is_candidate(&info) {
                continue;
            }
            self.manage(&info);
        }
    }

    /// Uncloaks a window that is only unmanageable because it is cloaked.
    ///
    /// A daemon that was killed with `taskkill /F` never ran its restore hook,
    /// so its windows are still cloaked: invisible, out of Alt-Tab and out of
    /// reach. The next start has to give them back, and startup is the only
    /// moment where a cloaked window can safely be assumed to be Mochi's doing
    /// rather than a live virtual desktop switch.
    ///
    /// Returns the window as it looks after the uncloak, or `None` when it was
    /// left alone.
    fn uncloak_stale(&self, info: WindowInfo) -> Option<WindowInfo> {
        let allow_tool = self.class_is_forced(&info.class);
        if !self.manage_classes.is_empty() && !allow_tool {
            return Some(info);
        }
        if is_manageable_with(&info, allow_tool) != Err(Unmanageable::Cloaked) {
            return Some(info);
        }
        // Windows cloaks a suspended UWP app itself, so a cloaked frame window
        // is no evidence that a previous session hid it. Uncloaking it would
        // drag every app the user closed back onto the desktop on every start.
        // The UWP windows Mochi really did hide come back through the off
        // screen record, which knows instead of guessing.
        if info.class.eq_ignore_ascii_case(FRAME_WINDOW_CLASS) {
            tracing::debug!(
                hwnd = %info.hwnd,
                title = %info.title,
                "leaving a cloaked UWP window alone, Windows suspends them like this"
            );
            return Some(info);
        }

        // Would it be a window Mochi manages once it is visible again?
        let mut probe = info.clone();
        probe.cloaked = false;
        if is_manageable_with(&probe, allow_tool).is_err() || self.rules_say_ignore(&probe) {
            return Some(info);
        }

        tracing::warn!(
            hwnd = %info.hwnd,
            title = %info.title,
            "uncloaking a window a previous session left hidden"
        );
        if let Err(e) = self.platform.set_cloaked(info.hwnd, false) {
            tracing::error!(hwnd = %info.hwnd, error = %e, "could not uncloak");
            return Some(info);
        }
        self.platform.window_info(info.hwnd).ok().or(Some(probe))
    }

    // -----------------------------------------------------------------
    // manageability
    // -----------------------------------------------------------------

    fn class_is_forced(&self, class: &str) -> bool {
        self.manage_classes
            .iter()
            .any(|forced| forced.eq_ignore_ascii_case(class))
    }

    fn rules_say_ignore(&self, info: &WindowInfo) -> bool {
        // The whole ladder, not the ignore list alone: a rule file drops a
        // broad class and then names the one window it wants back, and asking
        // only the ignore half would leave that window behind here even though
        // every other path rescues it.
        self.core.rules.decide(&rule_info(info)) == RuleDecision::Ignore
    }

    /// Whether Mochi may take this window, before the user's rules are asked.
    ///
    /// With `--manage-class` the answer is no for every class that was not
    /// named, which is what makes an end-to-end run safe on a desktop that is
    /// already being managed by something else.
    fn is_candidate(&self, info: &WindowInfo) -> bool {
        if !self.manage_classes.is_empty() {
            return self.class_is_forced(&info.class) && is_manageable_with(info, true).is_ok();
        }
        match is_manageable_with(info, false) {
            Ok(()) => true,
            // A `manage_rules` entry may overrule the soft verdicts, never the
            // ones that describe a window nothing could tile.
            Err(reason) => {
                reason.is_overridable() && self.core.rules.should_manage(&rule_info(info))
            }
        }
    }

    // -----------------------------------------------------------------
    // the model
    // -----------------------------------------------------------------

    /// Adds a window to the model, obeying the rules and the workspace routing.
    fn manage(&mut self, info: &WindowInfo) {
        let window = core_window(info);
        if self.core.rules.decide(&window.info()) == RuleDecision::Ignore {
            tracing::debug!(hwnd = %info.hwnd, title = %info.title, "an ignore rule matched");
            return;
        }

        let slow = self.core.rules.is_slow(&window.info());
        let (monitor, workspace) = self.destination(info, &window);
        let before = self.focus();
        match self.core.add_window_to(monitor, workspace, window) {
            Ok(changes) => {
                self.routed.insert(info.hwnd);
                tracing::info!(
                    hwnd = %info.hwnd,
                    title = %info.title,
                    exe = %info.exe,
                    monitor,
                    workspace,
                    "managing"
                );
                self.notify(NotificationEvent::Manage {
                    window: window_ref(info),
                });
                self.apply_changes(changes);
                self.announce(before);
                if slow {
                    self.defer_retile(info.hwnd);
                }
                // A UWP window belongs to the frame host until the application
                // inside it has created its own child window, and that is what
                // the real process is read from. Asked too early there is no
                // child yet, so the window was judged as
                // ApplicationFrameHost.exe: every `exe` and `path` rule written
                // for the real application missed it, an ignore rule for it
                // never fired, and nothing ever asked again. The slow
                // application list cannot help, because it is consulted here,
                // after the window has already been judged.
                if crate::platform::is_frame_host(&info.exe) {
                    self.defer_recheck(info.hwnd);
                }
            }
            Err(e) => tracing::warn!(hwnd = %info.hwnd, error = %e, "could not manage a window"),
        }
    }

    /// Asks for this window to be read and judged again a beat from now.
    ///
    /// A synthetic rename, because `window_renamed` is already exactly the
    /// right thing: it re-reads the window from the desktop and puts the fresh
    /// identity back through the rules, which is what a window judged on a half
    /// built identity needs. It fires at most once per window, scheduled where
    /// the window is first managed, so there is no way for it to loop.
    ///
    /// On its own thread for the same reason as the deferred retile below: the
    /// loop owns every piece of state and must never sleep.
    fn defer_recheck(&self, hwnd: Hwnd) {
        tracing::debug!(%hwnd, "judged on the frame host, asking again in a moment");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SLOW_APPLICATION_SETTLE);
            let _ = tx.send(Event::Window {
                kind: WindowEventKind::NameChange,
                hwnd,
            });
        });
    }

    /// Asks for a second layout pass a beat from now.
    ///
    /// `slow_application_identifiers` names the applications whose window is
    /// not ready to be positioned at the moment it appears; the first placement
    /// lands on a window that then resizes itself out of its tile. The loop
    /// owns every piece of state and is the only thread that may touch it, so
    /// it must not sleep: waiting here would freeze the keyboard, the IPC
    /// server and every other window for as long as the slowest application
    /// takes to draw itself. The wait happens on a thread of its own and comes
    /// back as the same `retile` a user could have typed.
    fn defer_retile(&self, hwnd: Hwnd) {
        tracing::debug!(%hwnd, "a slow application was given a second layout pass");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SLOW_APPLICATION_SETTLE);
            let (reply, answer) = crate::events::Reply::channel();
            if tx.send(Event::command(Command::Retile, reply)).is_err() {
                // The loop has ended; there is nothing left to retile.
                return;
            }
            // Holding the answer until the loop has handled the command keeps
            // it from logging a client that hung up on its own retile.
            let _ = answer.recv();
        });
    }

    /// Which workspace a new window belongs on.
    ///
    /// A `workspace_rules` match wins. Without one the window stays on the
    /// monitor Windows put it on, which is far less surprising than dragging
    /// everything onto whatever monitor happens to be focused.
    fn destination(&self, info: &WindowInfo, window: &Window) -> (usize, usize) {
        if let Some(routed) = self.routed_by_rule(info, window) {
            return routed;
        }

        let monitor = info
            .monitor
            .and_then(|id| self.core.monitor_idx_for_id(id.0))
            .unwrap_or_else(|| self.core.focused_monitor_idx());
        let workspace = self
            .core
            .monitors()
            .get(monitor)
            .map_or(0, Monitor::focused_workspace_idx);
        (monitor, workspace)
    }

    /// The workspace a `workspace_rules` entry asks for, if one matches.
    ///
    /// Only the rule half of [`WindowManager::destination`]: the fallback
    /// there is "wherever the window already is", which is an answer, not a
    /// match, and a caller that moves a window has to be able to tell the two
    /// apart.
    fn routed_by_rule(&self, info: &WindowInfo, window: &Window) -> Option<(usize, usize)> {
        let rule_info = window.info();
        for rule in &self.workspace_rules {
            if rule.initial_only && self.routed.contains(&info.hwnd) {
                continue;
            }
            if rule.rule.matches(&rule_info)
                && self.core.workspace(rule.monitor, rule.workspace).is_ok()
            {
                return Some((rule.monitor, rule.workspace));
            }
        }
        None
    }

    /// Lets go of every window Windows has started refusing.
    ///
    /// A window Mochi may not move cannot be tiled, and the refusal is only
    /// ever discovered by trying, deep inside a layout pass and usually on the
    /// animation thread, where there is no model to change. This is where the
    /// model catches up: every event drains the platform's list first, so a
    /// window that turns out to be out of reach loses its tile on the next
    /// thing that happens rather than keeping an empty one until it closes.
    fn drop_unreachable_windows(&mut self) {
        for hwnd in self.platform.take_unreachable() {
            self.unmanage(hwnd, "out of reach");
        }
    }

    /// Lets go of a window that has gone off screen without Mochi hiding it.
    ///
    /// Mochi drops a managed window when it is told the cloak or the hide
    /// happened. That notice can be missed: it arrives while the daemon is
    /// starting, or something takes the window away in a manner that raises no
    /// event Mochi is watching. The model then holds a tile for a window
    /// nobody can see, the layout goes on reserving its share of the screen,
    /// and the desktop shows the remaining windows squeezed around a hole.
    ///
    /// Seen on a real desktop: three windows tiled as a half and two quarters
    /// with one quarter empty, because a shell-cloaked window still held it.
    /// `restore-windows` could not help, because Mochi had no record of hiding
    /// it and it was not Mochi that had.
    ///
    /// Only the windows that are supposed to be on screen are asked about,
    /// which is the focused workspace of each monitor, and the question is two
    /// flag reads. A window Mochi hid itself is skipped: that one is off
    /// screen on purpose and is written down.
    fn drop_vanished_windows(&mut self) {
        let vanished: Vec<Hwnd> = self
            .core
            .visible_window_ids()
            .into_iter()
            .map(handle)
            .filter(|hwnd| !self.we_hid(*hwnd) && !self.minimized.contains(hwnd))
            .filter(|hwnd| !self.platform.is_on_screen(*hwnd))
            .collect();
        for hwnd in vanished {
            self.unmanage(hwnd, "off screen, and not by us");
        }
    }

    /// Drops a window from the model, whatever the reason.
    fn unmanage(&mut self, hwnd: Hwnd, why: &str) {
        let id = window_id(hwnd);
        // Before the lookup, because the foreground is cached for every window
        // that takes it, managed or not. A window Mochi never managed can be
        // the foreground, and when it died the lookup below returned early and
        // left its handle here for good: Windows reuses handle values, and
        // `focus_hwnd` refuses a handle it believes is already focused, so the
        // next window to inherit it could never be given the foreground.
        if self.foreground == Some(hwnd) {
            self.foreground = None;
        }
        let Some(window) = self.core.window(id).cloned() else {
            return;
        };
        tracing::info!(%hwnd, title = %window.title, why, "unmanaging");

        // A window Mochi is hiding has to be put back before it is forgotten.
        // Dropping the record without uncloaking leaves the window invisible,
        // out of the model and out of the restore record at the same moment:
        // nothing is left that knows it exists, `mochic stop` cannot bring it
        // back, and neither can the next start. Any reason at all gets here
        // while a workspace is hidden, a minimize the user made, a cloak from
        // a virtual desktop switch, a rule that changed under it.
        //
        // A window that is gone is the one exception: there is nothing to
        // uncloak and the call would only fail loudly.
        if self.we_hid(hwnd) && self.platform.window_info(hwnd).is_ok() {
            tracing::debug!(%hwnd, "putting a hidden window back before letting go of it");
            self.show_window(hwnd);
        }

        let before = self.focus();
        match self.core.remove_window(id) {
            Ok(changes) => {
                self.notify(NotificationEvent::Unmanage {
                    window: WindowRef::new(hwnd.as_i64(), window.title, window.exe),
                });
                self.apply_changes(changes);
                self.announce(before);
            }
            Err(e) => tracing::debug!(%hwnd, error = %e, "the window was already gone"),
        }
        {
            let mut hidden = record(&self.hidden);
            hidden.show(hwnd);
        }
        // The routing ledger is deliberately *not* cleared here. A user
        // minimize, a virtual desktop cloak and a rule change all end up in
        // `unmanage` with the window still alive, and forgetting that it has
        // been routed once makes `initial_workspace_rules` fire again and
        // teleport it off whatever workspace the user had moved it to. Only a
        // destroyed window leaves the ledger, in `on_window_event`.
    }

    // -----------------------------------------------------------------
    // applying changes
    // -----------------------------------------------------------------

    /// Turns a [`Changes`] from `mochi-core` into calls on real windows.
    ///
    /// The order is the one the change set documents, with `restore` pulled in
    /// front of the retile: a window that is still maximized cannot be given a
    /// tile, and restoring it afterwards would undo the move.
    fn apply_changes(&mut self, changes: Changes) {
        if changes.is_empty() {
            return;
        }
        if self.core.is_paused {
            tracing::debug!("paused, no window was touched");
            return;
        }

        for id in &changes.hide {
            self.hide_window(handle(*id));
        }
        for id in &changes.restore {
            if let Err(e) = self.platform.show(handle(*id), ShowState::Restore) {
                tracing::debug!(hwnd = %handle(*id), error = %e, "could not restore");
            }
        }
        for target in &changes.retiled {
            self.apply_workspace(target.monitor, target.workspace);
        }
        if !changes.retiled.is_empty() {
            // One pass for the whole desktop, after every workspace in this
            // change set has been placed: the border manager is given the
            // complete set and removes whatever it was not shown, so a pass
            // per monitor would have them deleting each other's borders.
            let targets = self.visuals_targets();
            self.visuals.update(&targets);
        }
        for id in &changes.show {
            self.show_window(handle(*id));
        }
        for id in &changes.minimize {
            self.minimized.insert(handle(*id));
            if let Err(e) = self.platform.show(handle(*id), ShowState::Minimize) {
                tracing::debug!(hwnd = %handle(*id), error = %e, "could not minimize");
            }
        }
        if let Some(id) = changes.maximize
            && let Err(e) = self.platform.show(handle(id), ShowState::Maximize)
        {
            tracing::debug!(hwnd = %handle(id), error = %e, "could not maximize");
        }
        if let Some(id) = changes.close
            && let Err(e) = self.platform.close(handle(id))
        {
            tracing::debug!(hwnd = %handle(id), error = %e, "could not ask the window to close");
        }
        if let Some(id) = changes.focus {
            self.focus_hwnd(handle(id));
            // A pure focus change, moving between containers or windows
            // without a retile, does not appear in `changes.retiled`, so
            // borders and transparency need their own nudge here or they
            // would only ever follow a layout change.
            let targets = self.visuals_targets();
            self.visuals.update(&targets);
        } else if self.keyboard_was_left_behind(&changes) {
            // Nothing was named to focus and the keyboard is somewhere the user
            // is no longer looking. Handing it to the desktop is the only thing
            // that makes the model and the real foreground agree again.
            if let Err(e) = self.platform.focus_desktop() {
                tracing::debug!(error = %e, "could not take the keyboard off the old window");
            } else {
                self.foreground = None;
            }
            let targets = self.visuals_targets();
            self.visuals.update(&targets);
        }
        if let Some(rect) = changes.warp_mouse_to
            && self.core.mouse_follows_focus
            && !rect.is_empty()
        {
            let (x, y) = rect.center();
            if let Err(e) = self.platform.set_cursor_position(x, y) {
                tracing::debug!(error = %e, "could not warp the mouse");
            }
        }
    }

    /// Takes a window off screen the way the configuration asked for.
    fn hide_window(&mut self, hwnd: Hwnd) {
        // Read before the window is taken off screen. A hidden, cloaked or
        // minimized window is exactly the sort `window_info` fails on, and a
        // record that carries pid 0 and no class can never be matched against
        // the live window on the next start.
        let identity = self.platform.window_info(hwnd).ok();
        let mut behaviour = self.core.window_hiding_behaviour;
        let mut result = match behaviour {
            HidingBehaviour::Cloak => self.platform.set_cloaked(hwnd, true),
            HidingBehaviour::Minimize => self.platform.show(hwnd, ShowState::Minimize),
            HidingBehaviour::Hide => self.platform.show(hwnd, ShowState::Hide),
        };
        // A few windows cannot be cloaked by anyone, tool windows and splash
        // screens among them. Hiding is the closest thing, and the record
        // keeps the method actually used so the restore path undoes it.
        if matches!(behaviour, HidingBehaviour::Cloak)
            && result
                .as_ref()
                .is_err_and(|e| e.downcast_ref::<CloakUnsupported>().is_some())
        {
            tracing::debug!(%hwnd, "window cannot be cloaked, hiding it instead");
            behaviour = HidingBehaviour::Hide;
            result = self.platform.show(hwnd, ShowState::Hide);
        }
        match result {
            // The record is only written after the call succeeded, so the
            // restore path never promises a window it did not actually hide.
            Ok(()) => {
                let mut hidden = record(&self.hidden);
                if let Some(info) = identity {
                    hidden.identify(hwnd, info.pid, &info.class);
                }
                hidden.hide(hwnd, behaviour);
                tracing::debug!(
                    %hwnd,
                    ?behaviour,
                    off_screen = hidden.len(),
                    "took a window off screen and recorded it"
                );
            }
            Err(e) => tracing::error!(%hwnd, ?behaviour, error = %e, "could not hide a window"),
        }
    }

    /// Brings a window back, undoing whatever took it off screen.
    fn show_window(&mut self, hwnd: Hwnd) {
        // Read the record, do not take it. It is cleared further down, once
        // the window is actually back on screen.
        //
        // Clearing first and uncloaking after leaves a gap: the uncloak is a
        // cross-process call into the shell, so it takes tens of milliseconds
        // and longer while explorer is busy, and for that whole time the
        // window is off screen with nothing naming it. A `taskkill /f`, an End
        // task, or the power going out inside that gap strands it: cloaked,
        // out of the model, out of the record, and invisible to
        // `restore-windows`, which reads the record. Every workspace switch
        // comes through here, so the gap was opened dozens of times a day.
        // Uncloaking first costs at most one redundant uncloak on the next
        // start, which `recover` already tolerates.
        //
        // `record()`, not `lock().ok()`: this was the last reader that skipped
        // itself on a poisoned mutex, which would make it believe a window it
        // is holding was never hidden and leave it exactly where the ordering
        // above was about to.
        let previous = record(&self.hidden).behaviour_of(hwnd);
        let result = match previous {
            Some(HidingBehaviour::Cloak) => self.platform.set_cloaked(hwnd, false),
            Some(HidingBehaviour::Minimize) => self.platform.show(hwnd, ShowState::Restore),
            Some(HidingBehaviour::Hide) => self.platform.show(hwnd, ShowState::ShowNoActivate),
            // Not hidden by Mochi. Uncloak anyway when something else left it
            // cloaked, but never un-minimize a window the user minimized.
            None => match self.platform.window_info(hwnd) {
                Ok(info) if info.cloaked => self.platform.set_cloaked(hwnd, false),
                _ => Ok(()),
            },
        };
        match result {
            // Back on screen, so nothing is owed for it any more. This is the
            // only place the entry goes, and it goes after the fact.
            Ok(()) => {
                record(&self.hidden).show(hwnd);
            }
            // The entry was never removed, so a window that refused to come
            // back is still named by the record and the next attempt, the
            // restore hook and `restore-windows` all still know about it.
            Err(e) => tracing::error!(%hwnd, error = %e, "could not show a window"),
        }
    }

    /// Gives a window the foreground, unless it already has it.
    /// Whether a change set that named no window to focus has stranded the
    /// keyboard on a window the user has navigated away from.
    ///
    /// Two ways that happens, and the model reports neither as a focus change
    /// because from its point of view there is simply nothing to focus:
    ///
    /// - the focused monitor moved to a workspace with no window on it, so the
    ///   keyboard stays on the screen the user just left, every border goes
    ///   unfocused, and the next thing typed lands on the other screen;
    /// - the window holding the keyboard was just taken off screen, which is
    ///   every switch to an empty workspace. Windows then hands the foreground
    ///   to whatever it likes, which can be a window on another monitor.
    fn keyboard_was_left_behind(&self, changes: &Changes) -> bool {
        // The window holding the keyboard has just been taken off screen by
        // this very change set. Whatever else is true, it cannot keep it.
        if self
            .foreground
            .is_some_and(|held| changes.hide.iter().any(|id| handle(*id) == held))
        {
            return true;
        }
        if !changes.focused_monitor_changed {
            return false;
        }
        // The model moved to another monitor and named no window to focus
        // there. That strands the keyboard only when it is on a window Mochi
        // put somewhere the user is no longer looking -- which means a window
        // Mochi manages. A window it does not manage was never moved by it, is
        // still on screen, and is where the user deliberately put the focus;
        // taking the keyboard off that and handing it to the desktop undoes
        // the user's own choice and gives them nothing back.
        self.foreground
            .is_none_or(|held| self.core.is_managed(window_id(held)))
    }

    fn focus_hwnd(&mut self, hwnd: Hwnd) {
        if self.foreground == Some(hwnd) {
            return;
        }
        match self.platform.focus(hwnd) {
            Ok(()) => self.foreground = Some(hwnd),
            Err(e) => tracing::debug!(%hwnd, error = %e, "could not focus"),
        }
    }

    /// Moves every window of one workspace onto its tile.
    fn apply_workspace(&mut self, monitor: usize, workspace: usize) {
        let placements = self.placements_for(monitor, workspace);
        if !placements.is_empty() {
            tracing::debug!(monitor, workspace, windows = placements.len(), "retiling");
            self.apply_layout(&placements);
        }
        // The visuals are not drawn here. They belong to the whole desktop at
        // once and are done in `apply_changes`, after every workspace of the
        // change set has been placed. A workspace with nothing to tile still
        // gets its pass that way: a workspace holding only floating windows
        // wants a border on the focused one, and an emptied workspace has to
        // drop the borders it had.
    }

    /// Every visible window of the desktop, classified for the borders and
    /// transparency pass: which one has the focus, the [`mochi_render::BorderKind`]
    /// it should draw with, and the rest as the transparency set.
    ///
    /// Every monitor at once, deliberately. The border manager is handed the
    /// complete desired set and removes every border it was not shown, and a
    /// change set carries one `retiled` entry per monitor, so a set built from
    /// a single monitor has the screens taking each other's borders away on
    /// every pass.
    fn visuals_targets(&self) -> crate::visuals::VisualsTargets {
        let mut targets = crate::visuals::VisualsTargets::default();
        let focused_monitor = self.core.focused_indices().ok().map(|(monitor, _)| monitor);
        for monitor in 0..self.core.monitors().len() {
            let workspace = self
                .core
                .monitors()
                .get(monitor)
                .map_or(0, Monitor::focused_workspace_idx);
            self.push_visuals_targets(
                monitor,
                workspace,
                focused_monitor == Some(monitor),
                &mut targets,
            );
        }
        targets
    }

    /// The part of [`WindowManager::visuals_targets`] that belongs to one
    /// workspace. Adds nothing when the workspace is not the one showing on
    /// its monitor, matching [`WindowManager::placements_for`], and nothing for
    /// a maximized workspace, which is Windows' business rather than the
    /// layout's.
    ///
    /// `has_focus` is true only for the monitor the desktop focus is on: one
    /// window on the whole desktop carries the focused border, and every other
    /// screen draws unfocused ones.
    fn push_visuals_targets(
        &self,
        monitor: usize,
        workspace: usize,
        has_focus: bool,
        targets: &mut crate::visuals::VisualsTargets,
    ) {
        use mochi_render::BorderKind;

        let Some(display) = self.core.monitors().get(monitor) else {
            return;
        };
        if display.focused_workspace_idx() != workspace {
            return;
        }
        let Ok(target) = self.core.workspace(monitor, workspace) else {
            return;
        };
        if target.is_maximized() {
            return;
        }

        let focused = if has_focus {
            target.focused_window_id().map(handle)
        } else {
            None
        };
        if has_focus {
            targets.focused = focused;
        }
        let focused_kind = if target.is_monocle() {
            BorderKind::Monocle
        } else if target.focus_is_floating() {
            BorderKind::Floating
        } else if target
            .focused_container()
            .is_some_and(mochi_core::model::Container::is_stack)
        {
            BorderKind::Stack
        } else {
            BorderKind::Single
        };

        let platform = self.platform.as_ref();
        let rules = &self.core.rules;
        let mut push = |hwnd: Hwnd, rect: Rect| {
            let kind = if Some(hwnd) == focused {
                focused_kind
            } else {
                BorderKind::Unfocused
            };
            // A window named by a transparency ignore rule keeps its borders
            // but is never faded: some apps paint wrongly once they are layered.
            if kind == BorderKind::Unfocused
                && !platform
                    .window_info(hwnd)
                    .is_ok_and(|info| rules.should_stay_opaque(&rule_info(&info)))
            {
                targets.unfocused.push(hwnd);
            }
            targets.tiled.push((hwnd, rect, kind));
        };

        if let Some(container) = target.monocle_container() {
            if let Some(id) = container.focused_window_id()
                && let Some(work_area) = self.core.work_area_for(monitor, workspace)
            {
                // Scaled, like every other tile, and it has to be spelled
                // out because getting it wrong was invisible on one monitor:
                // at a scale of 1.0 a monocle came out ten physical pixels
                // larger per side than the tile it replaced on a 150% screen,
                // and the border followed the same wrong rectangle, while on a
                // 96 DPI screen the two agreed exactly.
                let rect = target.full_rect_scaled(
                    work_area,
                    self.core.default_workspace_padding,
                    self.core.default_container_padding,
                    self.core.padding_scale(monitor),
                );
                push(handle(id), rect);
            }
        } else {
            for (container, rect) in target.containers().iter().zip(target.latest_layout()) {
                if let Some(id) = container.focused_window_id() {
                    push(handle(id), *rect);
                }
            }
            for window in target.floating_windows().iter() {
                let floating_hwnd = handle(window.id);
                if let Ok(info) = self.platform.window_info(floating_hwnd) {
                    // The window rect includes the invisible resize border, so
                    // a border drawn around it sits several pixels outside the
                    // frame the user actually sees.
                    push(floating_hwnd, info.visible_frame());
                }
            }
        }
    }

    /// The rectangle every visible window of a workspace should occupy.
    ///
    /// A workspace nobody is looking at gets nothing: its windows are off
    /// screen, and moving a cloaked window only makes it flicker when it comes
    /// back. Floating windows are left exactly where the user put them.
    fn placements_for(&self, monitor: usize, workspace: usize) -> Vec<(Hwnd, Rect)> {
        let Some(display) = self.core.monitors().get(monitor) else {
            return Vec::new();
        };
        if display.focused_workspace_idx() != workspace {
            return Vec::new();
        }
        let Ok(target) = self.core.workspace(monitor, workspace) else {
            return Vec::new();
        };
        // A maximized window is Windows' business, not the layout's.
        if target.is_maximized() {
            return Vec::new();
        }
        let Some(work_area) = self.core.work_area_for(monitor, workspace) else {
            return Vec::new();
        };

        if let Some(container) = target.monocle_container() {
            let rect = target.full_rect_scaled(
                work_area,
                self.core.default_workspace_padding,
                self.core.default_container_padding,
                self.core.padding_scale(monitor),
            );
            return container
                .focused_window_id()
                .map(|id| (handle(id), rect))
                .into_iter()
                .collect();
        }

        target
            .containers()
            .iter()
            .zip(target.latest_layout())
            .filter_map(|(container, rect)| {
                container.focused_window_id().map(|id| (handle(id), *rect))
            })
            .collect()
    }

    // -----------------------------------------------------------------
    // event handling
    // -----------------------------------------------------------------

    fn on_event(&mut self, event: Event) -> Flow {
        self.drop_unreachable_windows();
        self.drop_vanished_windows();
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
                if self.is_the_hotkey_file(&path) {
                    tracing::info!(path = %path.display(), "the hotkey file changed on disk");
                    self.reload_hotkeys();
                } else {
                    tracing::info!(path = %path.display(), "the configuration changed on disk");
                    // Already logged by the loader; a file saved half way
                    // through an edit is an everyday event here.
                    let _ = self.reload_config();
                }
                Flow::Continue
            }
            Event::Hotkey { trigger, action } => self.on_hotkey(trigger, *action),
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

    /// Runs what a bound key press asked for.
    ///
    /// The press never reached the desktop, so a command that fails has to say
    /// so in the log: there is no shell to print an error to and no window that
    /// saw the key.
    fn on_hotkey(&mut self, trigger: mochi_hotkey::Trigger, action: mochi_hotkey::Action) -> Flow {
        match action {
            mochi_hotkey::Action::Command(command) => {
                let name = command.name();
                let (response, flow) = self.handle_command(command);
                match response.error_message() {
                    Some(message) => {
                        tracing::warn!(%trigger, command = name, message, "hotkey was refused");
                    }
                    None => tracing::info!(%trigger, command = name, "hotkey"),
                }
                flow
            }
            mochi_hotkey::Action::Shell { shell, line } => {
                crate::events::hotkey::run_shell(shell, &line);
                Flow::Continue
            }
        }
    }

    /// Where window events become tree updates.
    fn on_window_event(&mut self, kind: WindowEventKind, hwnd: Hwnd) {
        match kind {
            WindowEventKind::LocationChange => self.follow_external_maximize(hwnd),
            WindowEventKind::Destroyed => {
                if self.lives_in_the_tray(hwnd) {
                    // The application put its window away rather than closing:
                    // the handle is still a live window and the same one comes
                    // back when the user opens it from the tray. It leaves the
                    // layout like any other window that went off screen, but it
                    // keeps its place in the ledgers, because what comes back
                    // is the window that left and not a new one.
                    self.unmanage(hwnd, "a tray application put its window away");
                } else {
                    self.unmanage(hwnd, "destroyed");
                    // The handle is free now and Windows will hand it to another
                    // window, which has to be routed by the initial rules on its
                    // own account.
                    self.routed.remove(&hwnd);
                    self.minimized.remove(&hwnd);
                }
            }
            WindowEventKind::Hidden | WindowEventKind::Cloaked => {
                // Mochi's own cloak comes back as an event; ignoring it is what
                // keeps a workspace switch from unmanaging everything it hid.
                let ours = self.we_hid(hwnd);
                tracing::debug!(
                    %hwnd,
                    kind = kind.as_str(),
                    ours,
                    off_screen = record(&self.hidden).len(),
                    managed = self.core.is_managed(window_id(hwnd)),
                    "a window went off screen"
                );
                if !ours {
                    self.unmanage(hwnd, kind.as_str());
                }
            }
            WindowEventKind::Created | WindowEventKind::Shown | WindowEventKind::Uncloaked => {
                self.window_appeared(hwnd);
            }
            WindowEventKind::MinimizeStart => {
                // Mochi's own minimize comes back as an event, exactly the way
                // its own cloak does. Acting on it would unmanage every window
                // the workspace switch had just hidden, and `unmanage` clears
                // the hidden record as it goes, so nothing would be left that
                // knows those windows are off screen and owed back.
                let ours = self.we_hid(hwnd);
                tracing::debug!(
                    %hwnd,
                    ours,
                    already_minimized = self.minimized.contains(&hwnd),
                    off_screen = record(&self.hidden).len(),
                    managed = self.core.is_managed(window_id(hwnd)),
                    "a window reported itself minimized"
                );
                if ours || self.minimized.contains(&hwnd) {
                    tracing::debug!(%hwnd, "our own minimize, leaving the window managed");
                } else if self.core.is_managed(window_id(hwnd)) {
                    self.minimized.insert(hwnd);
                    self.unmanage(hwnd, "minimized");
                }
            }
            WindowEventKind::MinimizeEnd => {
                self.minimized.remove(&hwnd);
                self.window_appeared(hwnd);
            }
            WindowEventKind::Foreground => self.window_focused(hwnd),
            WindowEventKind::MoveSizeStart => {}
            WindowEventKind::MoveSizeEnd => self.window_dropped(hwnd),
            WindowEventKind::NameChange => self.window_renamed(hwnd),
        }
    }

    /// Keeps the model honest when the USER maximizes or restores a window.
    ///
    /// Windows has no event of its own for this; pressing the maximize button
    /// arrives as an ordinary location change. Nothing read it, and
    /// `WindowInfo::maximized` had no reader anywhere in the daemon, so the
    /// model went on calling the window normally tiled while Windows had it
    /// zoomed. A zoomed window ignores every rectangle `SetWindowPos` gives it,
    /// so the window simply sat at full screen, refusing its tile, with nothing
    /// logged and no command able to explain it.
    ///
    /// Adopted rather than undone: the user asked for the window to be big, and
    /// Mochi has its own maximize that means exactly that. Undoing it would
    /// mean a maximize button that visibly fights back.
    ///
    /// No ledger is needed to tell Mochi's own maximize from the user's, unlike
    /// the cloak and minimize paths. The model is updated BEFORE the call that
    /// maximizes the window is issued, so by the time the location change
    /// arrives the model and Windows already agree, and agreeing is exactly
    /// what this returns early on. A ledger was written first and it was worse
    /// than redundant: it also suppressed the RESTORE direction.
    fn follow_external_maximize(&mut self, hwnd: Hwnd) {
        let id = window_id(hwnd);
        // The cheap check first. This runs on the flood path: a window being
        // dragged emits hundreds of these a second.
        if !self.core.is_managed(id) {
            return;
        }
        let Some((monitor, workspace)) = self.core.locate_window(id) else {
            return;
        };
        let Ok(target) = self.core.workspace(monitor, workspace) else {
            return;
        };
        // Monocle owns the screen already; a maximize underneath it is not
        // something the model can hold, and `toggle_maximize` refuses anyway.
        if target.is_monocle() {
            return;
        }

        let model_says = target.maximized_window().map(|w| w.id) == Some(id);
        let windows_says = self.platform.is_maximized(hwnd);
        if model_says == windows_says {
            return;
        }
        // Only the window the model is actually pointing at can be toggled, and
        // maximizing a window is a click on it, so it is the focused one.
        if self.core.focused_window_id() != Some(id) {
            return;
        }

        match self.core.toggle_maximize() {
            Ok(mut changes) => {
                tracing::info!(
                    %hwnd,
                    maximized = windows_says,
                    "following a maximize the user made"
                );
                if windows_says {
                    // It is already maximized on screen. Telling Windows to do
                    // it again is at best wasted and at worst a second round of
                    // events to read.
                    changes.maximize = None;
                } else {
                    // Likewise: the user already restored it.
                    changes.restore.retain(|other| *other != id);
                }
                self.apply_changes(changes);
            }
            Err(e) => tracing::debug!(%hwnd, error = %e, "could not follow the maximize"),
        }
    }

    fn we_hid(&self, hwnd: Hwnd) -> bool {
        record(&self.hidden).contains(hwnd)
    }

    /// Whether a destroy is an application closing to the tray rather than a
    /// window that is gone.
    ///
    /// Both halves are needed. `tray_and_multi_window_applications` names the
    /// applications that keep a hidden window alive when they are "closed",
    /// and the platform says whether this particular handle is one of those or
    /// really dead: quitting such an application for good destroys its window
    /// like anything else, and a dead handle left in the routing ledger is
    /// inherited by whatever window Windows hands the value to next.
    fn lives_in_the_tray(&self, hwnd: Hwnd) -> bool {
        let Some(window) = self.core.window(window_id(hwnd)) else {
            return false;
        };
        self.core.rules.is_tray_or_multi_window(&window.info())
            && self.platform.window_info(hwnd).is_ok()
    }

    /// A window was created, shown or uncloaked.
    fn window_appeared(&mut self, hwnd: Hwnd) {
        if self.core.is_paused || self.core.is_managed(window_id(hwnd)) {
            return;
        }
        let Ok(info) = self.platform.window_info(hwnd) else {
            return;
        };
        if !self.is_candidate(&info) {
            return;
        }
        self.manage(&info);
    }

    /// A window took the foreground.
    fn window_focused(&mut self, hwnd: Hwnd) {
        self.foreground = Some(hwnd);
        let id = window_id(hwnd);

        if !self.core.is_managed(id) {
            // Some applications show their window and only then activate it.
            self.window_appeared(hwnd);
            if !self.core.is_managed(id) {
                let window = self.platform.window_info(hwnd).ok();
                self.notify(NotificationEvent::FocusChange {
                    window: window.as_ref().map(window_ref),
                });
                return;
            }
        }

        let before = self.focus();
        match self.core.focus_window(id) {
            Ok(mut changes) => {
                // It already has the foreground; asking Windows again would
                // only fight whatever gave it to the window in the first place.
                changes.focus = None;
                changes.warp_mouse_to = None;
                self.apply_changes(changes);
            }
            Err(e) => tracing::debug!(%hwnd, error = %e, "could not follow the foreground"),
        }
        self.announce(before);
    }

    /// The user let go of a title bar or a resize grip.
    ///
    /// A tiled window that was dragged onto another tile swaps with it, one
    /// dragged onto another monitor moves there, and anything else snaps back
    /// to where the layout wants it.
    fn window_dropped(&mut self, hwnd: Hwnd) {
        let id = window_id(hwnd);
        let Some((monitor, workspace)) = self.core.locate_window(id) else {
            return;
        };
        // A floating window keeps whatever the user did to it.
        if self
            .core
            .workspace(monitor, workspace)
            .is_ok_and(|target| target.floating_windows().position(|w| w.id == id).is_some())
        {
            return;
        }

        let Ok((x, y)) = self.platform.cursor_position() else {
            self.retile();
            return;
        };

        let before = self.focus();
        let dropped_on = self.core.monitor_idx_at(x, y);
        let changes = match dropped_on {
            Some(target) if target != monitor => {
                let _ = self.core.focus_window(id);
                self.core.move_to_monitor(target, true)
            }
            Some(_) => {
                let source = self
                    .core
                    .workspace(monitor, workspace)
                    .ok()
                    .and_then(|w| w.container_idx_for_window(id));
                let under_cursor = self
                    .core
                    .workspace(monitor, workspace)
                    .ok()
                    .and_then(|w| w.latest_layout().iter().position(|r| r.contains(x, y)));
                match (source, under_cursor) {
                    (Some(from), Some(onto)) if from != onto => {
                        let _ = self.core.focus_window(id);
                        if let Ok(target) = self.core.workspace_mut(monitor, workspace) {
                            target.swap_focused_container(onto);
                        }
                        tracing::debug!(%hwnd, from, onto, "swapped two tiles by drag");
                        self.core.retile()
                    }
                    _ => self.core.retile(),
                }
            }
            None => self.core.retile(),
        };

        match changes {
            Ok(changes) => self.apply_changes(changes),
            Err(e) => tracing::debug!(%hwnd, error = %e, "could not settle a dropped window"),
        }
        self.announce(before);
    }

    /// A window changed its title.
    ///
    /// Two things depend on it: an application that opens its window before it
    /// has a title is only manageable once the title arrives, and a rule that
    /// matches on the title has to be re-asked when the title changes.
    fn window_renamed(&mut self, hwnd: Hwnd) {
        let Ok(info) = self.platform.window_info(hwnd) else {
            return;
        };
        let id = window_id(hwnd);

        if !self.core.is_managed(id) {
            if !info.title.trim().is_empty() {
                self.window_appeared(hwnd);
            }
            return;
        }

        self.set_title(id, &info.title);
        match self.core.rules.decide(&rule_info(&info)) {
            RuleDecision::Ignore => self.unmanage(hwnd, "an ignore rule started matching"),
            RuleDecision::Float => match self.core.float_window(id) {
                Ok(changes) => self.apply_changes(changes),
                Err(e) => tracing::debug!(%hwnd, error = %e, "could not float"),
            },
            RuleDecision::Tile => self.contents_changed(&info, id),
        }
    }

    /// A window that reuses itself was given different contents.
    ///
    /// `object_name_change_applications` names the applications that open a
    /// new document in the window they already have: nothing is created, the
    /// title simply changes. The workspace routing is written against the
    /// document, and for every other application it is asked when the window
    /// appears, so for these the title change is the moment to ask it again.
    ///
    /// Two things are deliberately not done here. `initial_workspace_rules`
    /// are not asked, because this window has been routed once already and
    /// that is the whole meaning of them; `routed_by_rule` skips them for the
    /// same reason `destination` does. And a window that is off screen is left
    /// alone: Mochi hid it, the model does not know that, and moving it onto a
    /// visible workspace would tile a hole where a cloaked window is.
    fn contents_changed(&mut self, info: &WindowInfo, id: WindowId) {
        if self.core.is_paused || !self.core.rules.changes_object_name(&rule_info(info)) {
            return;
        }
        let Some(window) = self.core.window(id).cloned() else {
            return;
        };
        let Some(here) = self.core.locate_window(id) else {
            return;
        };
        let Some(there) = self.routed_by_rule(info, &window) else {
            return;
        };
        if there == here || self.we_hid(info.hwnd) {
            return;
        }

        let before = self.focus();
        match self.core.remove_window(id) {
            // The workspace it came from closes the gap straight away; the
            // window itself is placed by the change set the add hands back.
            Ok(changes) => self.apply_changes(changes),
            Err(e) => {
                tracing::debug!(hwnd = %info.hwnd, error = %e, "could not move a renamed window");
                return;
            }
        }
        match self.core.add_window_to(there.0, there.1, window) {
            Ok(changes) => {
                tracing::info!(
                    hwnd = %info.hwnd,
                    title = %info.title,
                    monitor = there.0,
                    workspace = there.1,
                    "a workspace rule matched the new contents of a reused window"
                );
                self.apply_changes(changes);
                self.announce(before);
            }
            Err(e) => {
                tracing::warn!(hwnd = %info.hwnd, error = %e, "could not route a renamed window");
            }
        }
    }

    /// Writes a fresh title into the model's cache.
    fn set_title(&mut self, id: WindowId, title: &str) {
        let Some((monitor, workspace)) = self.core.locate_window(id) else {
            return;
        };
        let Ok(target) = self.core.workspace_mut(monitor, workspace) else {
            return;
        };
        if let Some(window) = target
            .containers_mut()
            .iter_mut()
            .find_map(|container| container.window_mut(id))
        {
            title.clone_into(&mut window.title);
            return;
        }
        if let Some(window) = target
            .floating_windows_mut()
            .iter_mut()
            .find(|window| window.id == id)
        {
            title.clone_into(&mut window.title);
        }
    }

    /// A display was attached, detached, resized or rescaled.
    ///
    /// The monitor ring is rebuilt rather than replaced, so a monitor that is
    /// still there keeps its workspaces, its layouts and its padding. Monitors
    /// that went away hand their windows to the first remaining monitor, on the
    /// same workspace index; that is best effort and deliberately simple,
    /// because the alternative is losing track of a window entirely.
    fn on_monitor_event(&mut self, kind: MonitorEventKind) {
        tracing::info!(event = kind.as_str(), "monitor event");
        let Ok(monitors) = self.platform.monitors() else {
            tracing::error!("could not enumerate the monitors");
            return;
        };
        self.rebuild_monitors(&monitors);
        // Before the retile, so the layout pass it triggers hands every border
        // to its window again. A display change can leave a window on exactly
        // the rectangle it already had while the DPI under it changed, and the
        // per-pass diff would see an unchanged spec and send nothing, leaving
        // the frame drawn at the old screen's measurements.
        self.visuals.invalidate_borders();
        self.notify(NotificationEvent::MonitorsChanged {
            count: self.core.monitors().len(),
        });
        self.retile();
    }

    fn rebuild_monitors(&mut self, infos: &[MonitorInfo]) {
        // An empty enumeration is not the same as "there are no monitors": a mode
        // switch, a driver restart or a disconnected session can make every handle
        // fail to read, and the platform reports that as Ok(vec![]). Rebuilding on
        // it would drain the ring with nothing to rehome onto, so every managed
        // window would leave the model while still cloaked or hidden. Keep what we
        // have and wait for the next event.
        if infos.is_empty() {
            tracing::warn!("ignoring an empty monitor enumeration");
            return;
        }

        // Which screen had the focus, remembered the same way a monitor is
        // carried across: by where it is, not by what Windows is calling it
        // this time round.
        let focused_area = self.core.focused_monitor().ok().map(|monitor| monitor.size);

        let mut previous = Vec::new();
        while let Some(monitor) = self.core.remove_monitor(0) {
            previous.push(monitor);
        }

        // How many workspaces the screens that are staying have, so a new one
        // is never the odd screen out with a single empty workspace.
        let usual_workspaces = previous
            .iter()
            .map(|monitor| monitor.workspaces().len())
            .max()
            .unwrap_or(1)
            .max(1);

        let mut fresh = Vec::new();
        for info in infos {
            let carried = same_panel(&previous, info);
            let mut monitor = match carried {
                Some(idx) => previous.remove(idx),
                None => {
                    fresh.push(self.core.monitors().len());
                    Monitor::new(info.id.0, info.size, info.work_area)
                }
            };
            apply_monitor_info(&mut monitor, info);
            self.core.add_monitor(monitor);
        }

        // A display that has just been attached has never been configured. Only
        // that one: shaping every monitor here would put the workspaces of the
        // screens that never moved back to their configured layout and padding,
        // throwing away whatever a command had set on them.
        for idx in fresh {
            let config = self.visual_config.clone();
            if !config.apply_to_monitor(&mut self.core, idx) {
                // No entry configures it, so match the screens it joined
                // rather than coming up with one workspace.
                if let Some(monitor) = self.core.monitors_mut().get_mut(idx) {
                    monitor.ensure_workspaces(usual_workspaces);
                }
            }
        }

        // Whatever was left behind on a monitor that is gone.
        //
        // Taken workspace by workspace, not window by window. Flattening the
        // vanished monitor through `all_windows` and re-adding each window
        // destroyed everything about how they were arranged: a stack of three
        // became three containers, a window floated by hand came back tiled
        // because no rule said to float it, and with `Append` behaviour the
        // whole screen collapsed into one stack. `Workspace::absorb` keeps the
        // containers, the stacks, the floating list and the modes, which is
        // what the model's own reconcile path has always done.
        let vanished: Vec<Monitor> = previous;
        let carried: usize = vanished
            .iter()
            .flat_map(|monitor| monitor.workspaces().iter())
            .map(|workspace| workspace.all_windows().count())
            .sum();
        if carried > 0 {
            tracing::warn!(
                count = carried,
                "rehoming windows from a monitor that went away"
            );
        }
        for mut gone in vanished {
            for (idx, workspace) in std::mem::take(gone.workspaces_mut())
                .into_vec()
                .into_iter()
                .enumerate()
            {
                let Some(survivor) = self.core.monitors_mut().get_mut(0) else {
                    continue;
                };
                survivor.ensure_workspaces(idx + 1);
                if let Some(target) = survivor.workspaces_mut().get_mut(idx) {
                    target.absorb(workspace);
                }
            }
        }

        // Then make the screen agree about what is off it. The retile that
        // follows a display change only ever shows a window, so without this
        // everything that arrived on a workspace nobody is looking at stayed
        // on screen, untiled, over the surviving monitor's layout, while the
        // model recorded it as hidden.
        if carried > 0 {
            let visible: std::collections::BTreeSet<_> =
                self.core.visible_window_ids().into_iter().collect();
            let mut settling = Changes::none();
            for id in self.core.all_window_ids() {
                if !visible.contains(&id) {
                    settling.hide.push(id);
                }
            }
            settling.settle();
            self.apply_changes(settling);
        }

        if let Some(idx) = focused_area.and_then(|area| {
            self.core
                .monitors()
                .position(|monitor| monitor.size == area)
        }) {
            self.core.monitors_mut().focus(idx);
        }

        tracing::info!(
            monitors = self.core.monitors().len(),
            "monitor ring rebuilt"
        );
    }

    fn on_mouse_focus(&mut self, hwnd: Hwnd) {
        if self.core.focus_follows_mouse.is_none() || self.core.is_paused {
            return;
        }
        if self.foreground == Some(hwnd) || !self.core.is_managed(window_id(hwnd)) {
            return;
        }
        tracing::debug!(%hwnd, "focus follows mouse");
        self.focus_hwnd(hwnd);
    }

    // -----------------------------------------------------------------
    // commands
    // -----------------------------------------------------------------

    /// Runs one operation on the model and applies whatever it changed.
    ///
    /// Refused while paused. `apply_changes` touches no window in that state,
    /// so the model would drift away from the desktop and every command,
    /// `mochic close` included, would answer `Ok` for something that did not
    /// happen.
    fn run_op(
        &mut self,
        op: impl FnOnce(&mut CoreState) -> mochi_core::Result<Changes>,
    ) -> Response {
        if self.core.is_paused {
            return Response::error(PAUSED);
        }
        self.run_op_even_when_paused(op)
    }

    /// The same without the pause check, for the command that lifts it.
    fn run_op_even_when_paused(
        &mut self,
        op: impl FnOnce(&mut CoreState) -> mochi_core::Result<Changes>,
    ) -> Response {
        let before = self.focus();
        match op(&mut self.core) {
            Ok(changes) => {
                self.apply_changes(changes);
                self.announce(before);
                Response::Ok
            }
            Err(e) => Response::error(e),
        }
    }

    /// The same, followed by a `layout-change` notification.
    /// The layout and mirroring of the focused workspace, or `None` when there
    /// is no workspace to ask.
    fn focused_arrangement(
        &self,
    ) -> Option<(mochi_core::layout::Layout, mochi_core::layout::Flip)> {
        let (monitor, workspace) = self.core.focused_indices().ok()?;
        let target = self.core.workspace(monitor, workspace).ok()?;
        Some((target.effective_layout(), target.layout_flip))
    }

    fn run_layout_op(
        &mut self,
        op: impl FnOnce(&mut CoreState) -> mochi_core::Result<Changes>,
    ) -> Response {
        // What the arrangement was before, so a command that asks for the
        // layout a workspace already has does not tell every subscriber it
        // changed. The flip is part of it: mirroring a layout keeps its name
        // and is still a different arrangement, so a bar that redraws on this
        // notification has to hear about it.
        let before = self.focused_arrangement();
        let response = self.run_op(op);
        if response.is_ok()
            && let Ok((monitor, workspace)) = self.core.focused_indices()
            && let Ok(target) = self.core.workspace(monitor, workspace)
        {
            let after = (target.effective_layout(), target.layout_flip);
            if Some(after) != before {
                self.notify(NotificationEvent::LayoutChange {
                    monitor,
                    workspace,
                    layout: after.0.to_string(),
                });
            }
        }
        response
    }

    fn handle_command(&mut self, command: Command) -> (Response, Flow) {
        use mochi_client as wire;

        if self.core.unmanaged_window_operation_behaviour
            == mochi_core::model::OperationBehaviour::NoOp
            && acts_on_the_focused_window(&command)
            && !self.foreground_is_managed()
        {
            return (
                Response::error("the focused window is not managed"),
                Flow::Continue,
            );
        }

        let response = match command {
            Command::State => {
                // Refreshed here, not only when a subscribe command arrives.
                // The fan-out thread drops a subscriber whose pipe has died,
                // and the copy this document reports never heard about it, so
                // `mochic state` listed bars that were gone for good.
                self.session.subscribers = self.subscribers.names().to_vec();
                Response::State {
                    state: snapshot(
                        &self.session,
                        &self.core,
                        self.foreground,
                        &self.on_screen(),
                    ),
                }
            }
            Command::Query { target } => self.query(target),
            Command::Why => self.explain_foreground(),
            Command::Doctor => self.diagnose(),
            Command::Stop => {
                tracing::info!("stop requested");
                return (Response::Ok, Flow::Stop);
            }
            Command::Start => Response::Ok,
            Command::Quickstart => self.quickstart(),

            // --- lifecycle ---------------------------------------------
            Command::TogglePause => {
                let response = self.run_op_even_when_paused(CoreState::toggle_pause);
                tracing::info!(paused = self.core.is_paused, "pause toggled");
                if self.core.is_paused {
                    self.visuals.clear();
                } else {
                    // Look at the desktop again before tiling it. While paused
                    // the model only ever shrinks: a destroyed, minimized or
                    // cloaked window still leaves it, because a window that is
                    // gone must never stay in the model, but a window that
                    // appears is dropped on the floor. So anything opened
                    // during the pause, and anything that was minimized and
                    // restored across it, was never managed again and sat
                    // untiled over the layout for the rest of the session.
                    self.adopt_newly_eligible();
                    self.retile();
                }
                self.notify(NotificationEvent::Pause {
                    paused: self.core.is_paused,
                });
                response
            }
            Command::ReloadConfiguration => match self.reload_config() {
                Ok(()) => Response::Ok,
                Err(e) => Response::error(e),
            },
            Command::Retile => self.run_op(CoreState::retile),
            Command::RestoreWindows => self.restore_stranded_windows(),

            // --- focus and movement ------------------------------------
            Command::Focus { direction } => {
                self.run_op(|core| core.focus_direction(direction_of(direction)))
            }
            Command::CycleFocus { direction } => {
                self.run_op(|core| core.cycle_focus(cycle_of(direction)))
            }
            Command::Move { direction } => {
                self.run_op(|core| core.move_direction(direction_of(direction)))
            }
            Command::CycleMove { direction } => {
                self.run_op(|core| core.cycle_move(cycle_of(direction)))
            }
            Command::ResizeAxis { axis, sizing } => {
                self.run_op(|core| core.resize_axis(axis_of(axis), sizing_of(sizing)))
            }
            Command::ResizeEdge { direction, sizing } => {
                self.run_op(|core| core.resize_edge(direction_of(direction), sizing_of(sizing)))
            }
            Command::Promote => self.run_op(CoreState::promote),
            Command::PromoteFocus => self.run_op(CoreState::promote_focus),

            // --- window state ------------------------------------------
            Command::ToggleFloat => self.run_op(CoreState::toggle_float),
            Command::ToggleFloatOverride => {
                // Not an operation on the model's geometry: nothing already on
                // screen moves, the next window to appear is the one that
                // notices. `add_window` reads the flag.
                self.core.float_override = !self.core.float_override;
                tracing::info!(
                    float_override = self.core.float_override,
                    "float override toggled"
                );
                Response::Ok
            }
            Command::ToggleMaximize => self.run_op(CoreState::toggle_maximize),
            Command::ToggleMonocle => self.run_op(CoreState::toggle_monocle),
            Command::Minimize => self.run_op(CoreState::minimize_focused_window),
            Command::Close => self.run_op(CoreState::close_focused_window),
            Command::Manage => self.manage_foreground(),
            Command::Unmanage => self.unmanage_foreground(),

            // --- stacks -------------------------------------------------
            Command::Stack { direction } => self.run_op(|core| core.stack(direction_of(direction))),
            Command::Unstack => self.run_op(CoreState::unstack),
            Command::StackAll => self.run_op(CoreState::stack_all),
            Command::FocusStackWindow { index } => {
                self.run_op(|core| core.focus_stack_window(index))
            }
            Command::UnstackAll => self.run_op(CoreState::unstack_all),
            Command::CycleStack { direction } => {
                self.run_op(|core| core.cycle_stack(cycle_of(direction)))
            }

            // --- layouts -------------------------------------------------
            Command::CycleLayout { direction } => {
                self.run_layout_op(|core| core.cycle_layout(cycle_of(direction)))
            }
            Command::ChangeLayout { layout } => {
                self.run_layout_op(|core| core.change_layout(layout_of(layout)))
            }
            Command::FlipLayout { axis } => {
                self.run_layout_op(|core| core.flip_layout(axis_of(axis)))
            }
            // Not a layout change: the workspace keeps the layout it had, it
            // just stops being arranged by it, so no `layout-change` goes out.
            Command::ToggleTiling => self.run_op(CoreState::toggle_tiling),

            // --- workspaces ----------------------------------------------
            Command::FocusWorkspace { index } => self.run_op(|core| core.focus_workspace(index)),
            Command::MoveToWorkspace { index } => {
                self.run_op(|core| core.move_to_workspace(index, true))
            }
            Command::SendToWorkspace { index } => {
                self.run_op(|core| core.move_to_workspace(index, false))
            }
            Command::CycleWorkspace { direction } => {
                self.run_op(|core| core.cycle_workspace(cycle_of(direction)))
            }
            Command::FocusLastWorkspace => self.run_op(CoreState::focus_last_workspace),
            Command::FocusNamedWorkspace { name } => {
                self.run_op(|core| core.focus_named_workspace(&name))
            }
            Command::MoveToNamedWorkspace { name } => {
                self.run_op(|core| core.move_to_named_workspace(&name, true))
            }
            Command::SendToNamedWorkspace { name } => {
                self.run_op(|core| core.move_to_named_workspace(&name, false))
            }
            Command::WorkspacePadding {
                monitor,
                workspace,
                size,
            } => self.set_padding(monitor, workspace, Some(size), None),
            Command::ContainerPadding {
                monitor,
                workspace,
                size,
            } => self.set_padding(monitor, workspace, None, Some(size)),

            // --- monitors -------------------------------------------------
            Command::FocusMonitor { index } => self.run_op(|core| core.focus_monitor(index)),
            Command::MoveToMonitor { index } => {
                self.run_op(|core| core.move_to_monitor(index, true))
            }
            Command::SendToMonitor { index } => {
                self.run_op(|core| core.move_to_monitor(index, false))
            }
            Command::CycleMonitor { direction } => {
                self.run_op(|core| core.cycle_monitor(cycle_of(direction)))
            }

            // --- input behaviour -------------------------------------------
            Command::FocusFollowsMouse { state } => {
                self.core.focus_follows_mouse = state
                    .is_enabled()
                    .then_some(mochi_core::model::FocusFollowsMouseImplementation::Mochi);
                if let Some(mouse) = &self.mouse {
                    mouse.set_enabled(state.is_enabled());
                }
                Response::Ok
            }
            Command::MouseFollowsFocus { state } => {
                self.core.mouse_follows_focus = state.is_enabled();
                Response::Ok
            }
            Command::WindowContainerBehaviour { behaviour } => {
                self.core.window_container_behaviour = container_behaviour_of(behaviour);
                Response::Ok
            }
            Command::ToggleWindowContainerBehaviour => {
                self.core.window_container_behaviour = match self.core.window_container_behaviour {
                    mochi_core::model::WindowContainerBehaviour::Create => {
                        mochi_core::model::WindowContainerBehaviour::Append
                    }
                    mochi_core::model::WindowContainerBehaviour::Append => {
                        mochi_core::model::WindowContainerBehaviour::Create
                    }
                };
                Response::Ok
            }
            Command::CrossMonitorMoveBehaviour { behaviour } => {
                self.core.cross_monitor_move_behaviour = move_behaviour_of(behaviour);
                Response::Ok
            }
            Command::WindowHidingBehaviour { behaviour } => {
                // Windows already off screen keep the method they were hidden
                // with: the record stores it per window, and restoring one with
                // the wrong call would leave it invisible for good.
                self.core.window_hiding_behaviour = hiding_behaviour_of(behaviour);
                Response::Ok
            }
            Command::UnmanagedWindowOperationBehaviour { behaviour } => {
                self.core.unmanaged_window_operation_behaviour = operation_behaviour_of(behaviour);
                Response::Ok
            }

            // --- rules ------------------------------------------------------
            Command::FloatRule {
                identifier,
                id,
                matching_strategy,
            } => self.add_rule(identifier, id, matching_strategy, RuleDecision::Float),
            Command::IgnoreRule {
                identifier,
                id,
                matching_strategy,
            } => self.add_rule(identifier, id, matching_strategy, RuleDecision::Ignore),
            Command::ManageRule {
                identifier,
                id,
                matching_strategy,
            } => self.add_rule(identifier, id, matching_strategy, RuleDecision::Tile),
            Command::WorkspaceRule {
                identifier,
                id,
                monitor,
                workspace,
                initial_only,
                matching_strategy,
            } => self.add_workspace_rule(
                identifier,
                id,
                matching_strategy,
                monitor,
                workspace,
                initial_only,
            ),

            // --- subscriptions -----------------------------------------------
            Command::SubscribePipe { name } => match self.subscribers.add(&name) {
                Ok(()) => {
                    self.session.subscribers = self.subscribers.names().to_vec();
                    Response::Ok
                }
                Err(e) => Response::error(e),
            },
            Command::UnsubscribePipe { name } => match self.subscribers.remove(&name) {
                Ok(()) => {
                    self.session.subscribers = self.subscribers.names().to_vec();
                    Response::Ok
                }
                Err(e) => Response::error(e),
            },

            // --- visuals -----------------------------------------------
            Command::ToggleTransparency => {
                let on = !self.session.settings.transparency;
                self.visual_config.transparency = Some(on);
                self.apply_visuals("transparency")
            }
            Command::Border { state } => {
                self.visual_config.border = Some(state.is_enabled());
                self.apply_visuals("border")
            }
            Command::BorderWidth { width } => {
                self.visual_config.border_width = Some(width);
                self.apply_visuals("border-width")
            }
            Command::BorderOffset { offset } => {
                self.visual_config.border_offset = Some(offset);
                self.apply_visuals("border-offset")
            }
            Command::BorderStyle { style } => {
                self.visual_config.border_style = Some(border_style_of(style));
                self.apply_visuals("border-style")
            }
            Command::BorderColour { kind, r, g, b } => {
                let colour = Colour::new(r, g, b);
                let colours = self.visual_config.border_colours.get_or_insert_default();
                match kind {
                    wire::WindowKind::Single => colours.single = Some(colour),
                    wire::WindowKind::Stack => colours.stack = Some(colour),
                    wire::WindowKind::Monocle => colours.monocle = Some(colour),
                    wire::WindowKind::Floating => colours.floating = Some(colour),
                    wire::WindowKind::Unfocused => colours.unfocused = Some(colour),
                }
                self.apply_visuals("border-colour")
            }
            Command::Animation { state } => {
                self.animation_settings().enabled = Some(state.is_enabled());
                self.apply_visuals("animation")
            }
            Command::AnimationDuration { duration } => {
                self.animation_settings().duration = Some(duration);
                self.apply_visuals("animation-duration")
            }
            Command::AnimationFps { fps } => {
                self.animation_settings().fps = Some(fps);
                self.apply_visuals("animation-fps")
            }
            Command::AnimationStyle { style } => {
                self.animation_settings().style = Some(animation_style_of(style));
                // The wire spelling is what `mochic state` prints, and the file
                // only carries the curves it names itself.
                self.session.settings.animation_style = style;
                self.apply_visuals("animation-style")
            }

            // --- hotkeys -----------------------------------------------
            Command::Hotkeys => Response::Hotkeys {
                hotkeys: self.hotkey_document(),
            },
            Command::SetHotkeys { state } => self.set_hotkeys(boolean(state)),
            Command::ToggleGameMode => self.toggle_game_mode(),
        };

        (response, Flow::Continue)
    }

    /// What `mochic hotkeys` prints: the file, the gate, the bindings and the
    /// lines that did not parse.
    fn hotkey_document(&self) -> serde_json::Value {
        let bindings: Vec<_> = self
            .hotkey_rows
            .iter()
            .map(|(keys, command)| serde_json::json!({ "keys": keys, "command": command }))
            .collect();
        serde_json::json!({
            "path": self.hotkey_path.as_ref().map(|p| p.display().to_string()),
            "gate": self.hotkeys.as_ref().map_or("off", |h| h.gate().as_str()),
            "bindings": bindings,
            "errors": self.hotkey_errors,
        })
    }

    /// Leaves game mode, putting back the pause it found.
    ///
    /// Shared, because there are two ways out that are not the toggle itself
    /// and both have to consume the remembered pause: asking for the keyboard
    /// with `set-hotkeys`, and a reload that leaves no binding able to toggle.
    /// A leave that forgets the memory arms it for the next press, which is
    /// then read as ENTERING and records game mode's own pause as the user's.
    ///
    /// The gate is left to the caller, which knows what it wants to put there.
    fn leave_game_mode(&mut self, why: &str) -> Response {
        let (_, was_paused) = self.before_game_mode.take().unwrap_or((Gate::All, false));
        tracing::info!(game_mode = false, why, "leaving game mode");
        if self.core.is_paused != was_paused {
            let (response, _) = self.handle_command(Command::TogglePause);
            if !response.is_ok() {
                return response;
            }
        }
        Response::Ok
    }

    /// `set-hotkeys`: bind keys, or stop binding them without stopping tiling.
    fn set_hotkeys(&mut self, enable: bool) -> Response {
        let Some(gate) = self.hotkeys.as_ref().map(HotkeyDaemon::gate) else {
            return Response::error(NO_HOTKEYS);
        };

        // Asking for the keyboard while game mode holds it is a request to
        // LEAVE game mode, not a way to end up half in it. Leaving here is what
        // consumes the remembered pause.
        //
        // Without this the memory stays armed and still says `All`, so the next
        // press of the game-mode key is read as ENTERING and overwrites it with
        // the pause game mode itself set. From then on every round trip
        // restores `paused = true`, and the toggle can never unpause again:
        // the keyboard comes back, the desktop stays untiled, and `mochic
        // state` reports `paused: true` with nothing to say why.
        if gate == Gate::GameMode {
            let response = self.leave_game_mode("the keyboard was asked for");
            if !response.is_ok() {
                return response;
            }
        }

        if let Some(hotkeys) = self.hotkeys.as_mut() {
            hotkeys.set_gate(if enable { Gate::All } else { Gate::Off });
        }
        Response::Ok
    }

    /// `toggle-game-mode`: the game in front gets every key but this one, and
    /// tiling stops until it is pressed again.
    ///
    /// The gate is the whole state. Nothing is written down and nothing is
    /// restarted, so a daemon that is killed in game mode comes back normal
    /// rather than in a half-suspended state nobody can leave.
    fn toggle_game_mode(&mut self) -> Response {
        let Some(hotkeys) = self.hotkeys.as_ref() else {
            return Response::error(NO_HOTKEYS);
        };
        let entering = hotkeys.gate() != Gate::GameMode;

        // What to put back, remembered rather than assumed. Leaving used to
        // set the gate to `All` and derive the pause from "we must be leaving",
        // so it handed back a keyboard the user had switched off with
        // `set-hotkeys disable` and resumed tiling they had paused themselves.
        // Neither was game mode's to give back.
        let (gate, paused) = if entering {
            let remembered = (hotkeys.gate(), self.core.is_paused);
            self.before_game_mode = Some(remembered);
            (Gate::GameMode, true)
        } else {
            self.before_game_mode.take().unwrap_or((Gate::All, false))
        };

        if let Some(hotkeys) = self.hotkeys.as_mut() {
            hotkeys.set_gate(gate);
        }

        // Tiling follows the keys. Going through the command keeps the pause
        // notification, the visuals and the model in step with `toggle-pause`.
        if self.core.is_paused != paused {
            let (response, _) = self.handle_command(Command::TogglePause);
            if !response.is_ok() {
                return response;
            }
        }
        tracing::info!(game_mode = entering, "game mode");
        Response::Ok
    }

    /// `doctor`: check the daemon's picture of the desktop against the real one.
    ///
    /// The test suite cannot do this. It drives a fake desktop that only ever
    /// changes when Mochi changes it, so every test passes while the model and
    /// the screen disagree. A real desktop moves on its own: applications cloak
    /// their own windows, a virtual desktop takes one away, Windows refuses a
    /// call because the window outranks us, a handle is reused. Every defect
    /// found on this project by looking at a screen rather than at a test was
    /// one of these, so this asks the questions a test cannot.
    ///
    /// Each finding names a window and says what is wrong in the terms the
    /// user would see it: a tile reserved for a window that is not there is a
    /// hole on their screen.
    fn diagnose(&self) -> Response {
        let mut findings: Vec<serde_json::Value> = Vec::new();
        let mut note = |kind: &str, hwnd: Hwnd, detail: String| {
            let (title, exe) = self
                .core
                .window(window_id(hwnd))
                .map(|w| (w.title.clone(), w.exe.clone()))
                .unwrap_or_default();
            findings.push(serde_json::json!({
                "kind": kind,
                "hwnd": hwnd.to_string(),
                "title": title,
                "exe": exe,
                "detail": detail,
            }));
        };

        for id in self.core.all_window_ids().collect::<Vec<_>>() {
            let hwnd = handle(id);
            let hidden_by_us = self.we_hid(hwnd);
            match self.platform.window_info(hwnd) {
                Err(_) => note(
                    "gone",
                    hwnd,
                    "the window no longer exists, and Mochi still has it".into(),
                ),
                Ok(info) => {
                    if !info.reachable {
                        note(
                            "unreachable",
                            hwnd,
                            "Windows will not let Mochi move this window, so its tile can never be filled".into(),
                        );
                    }
                    if !hidden_by_us
                        && !self.minimized.contains(&hwnd)
                        && !self.platform.is_on_screen(hwnd)
                    {
                        note(
                            "hole",
                            hwnd,
                            "the window is off screen but still holds a tile, so the layout has a hole in it".into(),
                        );
                    }
                }
            }
        }

        // The other direction: the record of what Mochi took off screen is what
        // `mochic stop` and the next start work from. An entry that is wrong
        // costs a pointless call; one that is missing is a window the user
        // cannot get back without another window manager.
        for hwnd in record(&self.hidden).handles() {
            match self.platform.window_info(hwnd) {
                Err(_) => note(
                    "stale-record",
                    hwnd,
                    "Mochi has this written down as hidden, but the window is gone".into(),
                ),
                Ok(_) if self.platform.is_on_screen(hwnd) => note(
                    "wrong-record",
                    hwnd,
                    "Mochi has this written down as hidden, and it is on screen".into(),
                ),
                // The worst state the record can reach, and the one it cannot
                // see for itself: Mochi took the window off screen while it
                // had the rights to, and no longer has them. Every restore
                // from here fails, so `mochic stop` cannot keep its promise
                // and neither can the next start. It happens when Mochi is
                // restarted with fewer rights than it had, which is exactly
                // what starting it once from an administrator terminal and
                // once normally does.
                Ok(_) if self.platform.outranks_us(hwnd) => note(
                    "cannot-restore",
                    hwnd,
                    "Mochi took this window off screen and can no longer put it back, because the window now outranks it: every restore is refused. Bring it back yourself from the taskbar or with alt+tab".into(),
                ),
                Ok(_) => {}
            }
        }

        // Nothing above this is about the keyboard, and this is the finding
        // that cost the most to work out by hand. A low-level keyboard hook in
        // a normal process is given no key presses at all while a window that
        // outranks it holds the focus, so every binding stops working inside
        // that one window and there is nothing on screen, and nothing in the
        // log, to say so. It reads exactly like the hotkeys being broken.
        for info in self.platform.windows().unwrap_or_default() {
            if info.visible && !info.title.is_empty() && self.platform.outranks_us(info.hwnd) {
                findings.push(serde_json::json!({
                    "kind": "hotkeys-blocked",
                    "hwnd": info.hwnd.to_string(),
                    "title": info.title,
                    "exe": info.exe,
                    "detail": "this window runs as administrator and Mochi does not, so Windows gives Mochi no key presses at all while it has the focus: every Mochi hotkey is dead in this window".to_owned(),
                }));
            }
        }

        Response::Doctor {
            doctor: serde_json::json!({
                "managed": self.core.all_window_ids().count(),
                "off_screen": record(&self.hidden).handles().len(),
                "findings": findings,
            }),
        }
    }

    /// `why`: explain what Mochi makes of the window in front.
    ///
    /// Mochi already decides this for every window, and already writes the
    /// answer down -- in the log file, which is the one place someone who is
    /// not already debugging Mochi will never look. This asks the same
    /// question on purpose and answers it in words, so that "why is this
    /// window not tiling" stops being a question only the author can answer.
    fn explain_foreground(&self) -> Response {
        let Some(hwnd) = self
            .foreground
            .or_else(|| self.platform.foreground_window())
        else {
            return Response::error("no window is focused");
        };
        let info = match self.platform.window_info(hwnd) {
            Ok(info) => info,
            Err(e) => return Response::error(e),
        };
        let mut why = serde_json::json!({
            "hwnd": info.hwnd.to_string(),
            "title": info.title,
            "exe": info.exe,
            "class": info.class,
        });
        let object = why.as_object_mut().expect("json! was given an object");

        // Said for a managed window as much as an unmanaged one: the keyboard
        // is a separate question from the tiling, and a window can be tiled
        // perfectly while none of the keys work in it.
        if self.platform.outranks_us(hwnd) {
            object.insert("hotkeys_blocked".into(), true.into());
        }

        if let Some((monitor, workspace)) = self.core.locate_window(window_id(hwnd)) {
            object.insert("managed".into(), true.into());
            object.insert("monitor".into(), monitor.into());
            object.insert("workspace".into(), workspace.into());
            return Response::Why { why };
        }
        object.insert("managed".into(), false.into());

        // Asked in the order the daemon itself asks, so the reason given is
        // the one that actually decided this window's fate and not merely the
        // first that happens to be true of it.
        if self.core.is_paused {
            object.insert("reason".into(), "paused".into());
            return Response::Why { why };
        }
        if !self.manage_classes.is_empty() && !self.class_is_forced(&info.class) {
            object.insert("reason".into(), "manage-class".into());
            return Response::Why { why };
        }
        let verdict = is_manageable_with(&info, false);
        let rescued = matches!(verdict, Err(reason)
            if reason.is_overridable() && self.core.rules.should_manage(&rule_info(&info)));
        match verdict {
            Err(reason) if !rescued => {
                object.insert("reason".into(), reason.as_str().into());
                object.insert("overridable".into(), reason.is_overridable().into());
            }
            _ if self.rules_say_ignore(&info) => {
                object.insert("reason".into(), "rule".into());
            }
            // Manageable, not ignored and still not in the model: it has only
            // just appeared, or a previous attempt to take it failed.
            _ => {
                object.insert("reason".into(), "unknown".into());
            }
        }
        Response::Why { why }
    }

    /// `manage`: take the foreground window whatever the heuristics think.
    fn manage_foreground(&mut self) -> Response {
        let Some(hwnd) = self
            .foreground
            .or_else(|| self.platform.foreground_window())
        else {
            return Response::error("no window is focused");
        };
        if self.core.is_managed(window_id(hwnd)) {
            return Response::Ok;
        }
        let info = match self.platform.window_info(hwnd) {
            Ok(info) => info,
            Err(e) => return Response::error(e),
        };
        let window = core_window(&info);
        let (monitor, workspace) = self.destination(&info, &window);
        let before = self.focus();
        match self.core.add_window_to(monitor, workspace, window) {
            Ok(changes) => {
                self.routed.insert(hwnd);
                self.notify(NotificationEvent::Manage {
                    window: window_ref(&info),
                });
                self.apply_changes(changes);
                self.announce(before);
                Response::Ok
            }
            Err(e) => Response::error(e),
        }
    }

    /// `unmanage`: drop the foreground window and leave it where it is.
    fn unmanage_foreground(&mut self) -> Response {
        let Some(hwnd) = self
            .foreground
            .or_else(|| self.platform.foreground_window())
        else {
            return Response::error("no window is focused");
        };
        if !self.core.is_managed(window_id(hwnd)) {
            return Response::error("the focused window is not managed");
        }
        self.unmanage(hwnd, "unmanage command");
        Response::Ok
    }

    fn set_padding(
        &mut self,
        monitor: usize,
        workspace: usize,
        workspace_padding: Option<i32>,
        container_padding: Option<i32>,
    ) -> Response {
        match self.core.workspace_mut(monitor, workspace) {
            Ok(target) => {
                if workspace_padding.is_some() {
                    target.workspace_padding = workspace_padding;
                }
                if container_padding.is_some() {
                    target.container_padding = container_padding;
                }
            }
            Err(e) => return Response::error(e),
        }
        self.run_op(CoreState::retile)
    }

    /// Puts one rule in the list its decision belongs to.
    ///
    /// The single place that knows the mapping, so adding a rule now and
    /// putting it back after a reload cannot drift apart.
    fn push_rule(&mut self, decision: RuleDecision, rule: MatchingRule) {
        match decision {
            RuleDecision::Ignore => {
                // Into both lists, exactly as the configuration file does it.
                // A rule the user types at their own keyboard carries the same
                // authority as one they wrote in their own file, so a manage
                // rule out of a community list cannot cancel it.
                self.core.rules.ignore_rules.push(rule.clone());
                self.core.rules.own_ignore_rules.push(rule);
            }
            RuleDecision::Float => self.core.rules.floating_applications.push(rule),
            RuleDecision::Tile => self.core.rules.manage_rules.push(rule),
        }
    }

    fn add_rule(
        &mut self,
        identifier: mochi_client::RuleIdentifier,
        id: String,
        strategy: mochi_client::MatchingStrategy,
        decision: RuleDecision,
    ) -> Response {
        let rule = MatchingRule::simple(identifier_of(identifier), id, strategy_of(strategy));
        if let Err(e) = rule.validate() {
            return Response::error(e);
        }
        self.push_rule(decision, rule.clone());
        self.added_rules.push((decision, rule));
        // A rule that arrives after the window it describes has to catch up.
        self.reapply_rules();
        // And a manage rule is about windows that are not in the model at all,
        // which is the one case re-asking the model cannot reach.
        if decision == RuleDecision::Tile {
            self.adopt_newly_eligible();
        }
        Response::Ok
    }

    /// Takes over every window on the desktop that the rules now allow and the
    /// model does not already hold.
    ///
    /// Only a `manage_rules` entry can turn a window Mochi was skipping into
    /// one it manages, so this is the other half of adding one: without it the
    /// rule would take effect on the application's *next* window and leave the
    /// one the user was looking at untiled.
    fn adopt_newly_eligible(&mut self) {
        let Ok(windows) = self.platform.windows() else {
            tracing::error!("could not enumerate the windows, adopting nothing");
            return;
        };
        for info in windows {
            if self.core.window(window_id(info.hwnd)).is_some() {
                continue;
            }
            if !self.is_candidate(&info) {
                continue;
            }
            self.manage(&info);
        }
    }

    /// Adds a `workspace_rules` entry for the rest of this session.
    ///
    /// Not written to the configuration file. Everything `mochic` sets is
    /// session state that a reload replaces with the file, which is what makes
    /// the file the source of truth and a command a thing you can try out.
    fn add_workspace_rule(
        &mut self,
        identifier: mochi_client::RuleIdentifier,
        id: String,
        strategy: mochi_client::MatchingStrategy,
        monitor: usize,
        workspace: usize,
        initial_only: bool,
    ) -> Response {
        let rule = MatchingRule::simple(identifier_of(identifier), id, strategy_of(strategy));
        if let Err(e) = rule.validate() {
            return Response::error(e);
        }
        // A rule pointing at a screen that is not attached would silently send
        // windows nowhere, and the mistake would only show up as a window that
        // never opens where it was told to.
        if monitor >= self.core.monitors().len() {
            return Response::error(mochi_core::Error::MonitorNotFound(monitor));
        }
        if workspace >= mochi_core::MAX_WORKSPACES {
            return Response::error(mochi_core::Error::WorkspaceIndexOutOfRange(workspace));
        }
        // Workspaces exist on demand everywhere else, and routing skips a rule
        // whose destination is not there yet. Without this the command would
        // be accepted, do nothing, and give no hint why.
        if let Some(monitor) = self.core.monitors_mut().get_mut(monitor) {
            monitor.ensure_workspaces(workspace + 1);
        }
        let entry = WorkspaceRule {
            monitor,
            workspace,
            rule,
            initial_only,
        };
        // Remembered so a reload puts it back, the same as the rules above.
        self.added_workspace_rules.push(entry.clone());
        self.workspace_rules.push(WorkspaceRule {
            monitor: entry.monitor,
            workspace: entry.workspace,
            rule: entry.rule.clone(),
            initial_only: entry.initial_only,
        });
        Response::Ok
    }

    /// Puts back every window that is off screen with nothing in the model to
    /// explain it.
    ///
    /// The escape hatch for the one failure that cannot be recovered from
    /// inside Windows: a window Mochi hid and then stopped tracking is
    /// invisible, out of Alt-Tab and unreachable, and no amount of clicking
    /// brings it back. A blunt "show everything" would work, but it would also
    /// drag every window on every hidden workspace onto the screen, so this
    /// compares the hiding record against the model and only touches the
    /// windows the model does not account for.
    fn restore_stranded_windows(&mut self) -> Response {
        let visible: std::collections::BTreeSet<_> =
            self.core.visible_window_ids().into_iter().collect();
        let accounted: std::collections::BTreeSet<Hwnd> = self
            .core
            .all_window_ids()
            .filter(|id| !visible.contains(id))
            .map(handle)
            .collect();

        let stranded: Vec<Hwnd> = {
            record(&self.hidden)
                .handles()
                .into_iter()
                .filter(|hwnd| !accounted.contains(hwnd))
                .collect()
        };

        if stranded.is_empty() {
            tracing::info!("no window is off screen without a reason");
            return Response::Ok;
        }

        let mut given_back = 0;
        for hwnd in stranded {
            let behaviour = record(&self.hidden).show(hwnd);
            let result = match behaviour {
                Some(HidingBehaviour::Cloak) | None => self.platform.set_cloaked(hwnd, false),
                Some(HidingBehaviour::Minimize) => self.platform.show(hwnd, ShowState::Restore),
                Some(HidingBehaviour::Hide) => self.platform.show(hwnd, ShowState::ShowNoActivate),
            };
            match result {
                Ok(()) => given_back += 1,
                Err(e) => {
                    // Written back down, the same way `show_window` does. This
                    // is the last-resort escape hatch: clearing the record and
                    // then failing left the window cloaked and invisible to the
                    // very command the user would run again to rescue it.
                    tracing::error!(%hwnd, error = %e, "could not give a window back");
                    if let Some(behaviour) = behaviour {
                        record(&self.hidden).hide(hwnd, behaviour);
                    }
                }
            }
        }
        tracing::warn!(count = given_back, "gave stranded windows back");
        Response::Ok
    }

    /// Re-asks the rules about every window that is already managed.
    fn reapply_rules(&mut self) {
        let windows: Vec<(WindowId, RuleDecision)> = self
            .core
            .monitors()
            .iter()
            .flat_map(|monitor| monitor.workspaces().iter())
            .flat_map(mochi_core::Workspace::all_windows)
            .map(|window| (window.id, self.core.rules.decide(&window.info())))
            .collect();

        for (id, decision) in windows {
            match decision {
                RuleDecision::Ignore => self.unmanage(handle(id), "a rule was added"),
                RuleDecision::Float => match self.core.float_window(id) {
                    Ok(changes) => self.apply_changes(changes),
                    Err(e) => tracing::debug!(%id, error = %e, "could not float"),
                },
                RuleDecision::Tile => {}
            }
        }
    }

    fn query(&self, target: QueryTarget) -> Response {
        let answer = match target {
            QueryTarget::MonitorCount => serde_json::json!(self.core.monitors().len()),
            QueryTarget::WindowCount => serde_json::json!(self.core.all_window_ids().count()),
            QueryTarget::Paused => serde_json::json!(self.core.is_paused),
            QueryTarget::DryRun => serde_json::json!(self.session.dry_run),
            QueryTarget::ConfigPath => {
                serde_json::json!(self.session.config_path().display().to_string())
            }
            QueryTarget::Version => serde_json::json!(self.session.version),
            QueryTarget::FocusedMonitorIndex => serde_json::json!(self.core.focused_monitor_idx()),
            QueryTarget::FocusedWorkspaceIndex => match self.core.focused_indices() {
                Ok((_, workspace)) => serde_json::json!(workspace),
                Err(e) => return Response::error(e),
            },
            QueryTarget::FocusedWorkspaceName => match self.core.focused_indices() {
                Ok((monitor, workspace)) => match self.core.monitors().get(monitor) {
                    Some(display) => serde_json::json!(display.workspace_name(workspace)),
                    None => return Response::error("no monitor is focused"),
                },
                Err(e) => return Response::error(e),
            },
            QueryTarget::FocusedContainerIndex => match self.core.focused_workspace() {
                Ok(workspace) => serde_json::json!(workspace.focused_container_idx()),
                Err(e) => return Response::error(e),
            },
            QueryTarget::FocusedWindowIndex => {
                match self
                    .core
                    .focused_container()
                    .map(|container| container.windows().focused_idx())
                {
                    Some(index) => serde_json::json!(index),
                    None => return Response::error("no window is focused"),
                }
            }
        };
        Response::Query { answer }
    }

    fn quickstart(&self) -> Response {
        let path = self.session.config_path();
        match config::write_default(path) {
            Ok(true) => {
                tracing::info!(path = %path.display(), "wrote a default configuration");
                Response::Ok
            }
            Ok(false) => Response::error(format!("{} already exists", path.display())),
            Err(e) => Response::error(e),
        }
    }

    /// The animation block of the live configuration, created on demand so a
    /// lone `mochic animation-fps` works against a file that never mentions
    /// animation.
    fn animation_settings(&mut self) -> &mut AnimationConfig {
        self.visual_config.animation.get_or_insert_default()
    }

    /// Hands the visual settings to the managers and redraws the workspace
    /// that is on screen.
    ///
    /// Every `mochic border`, `mochic transparency` and `mochic animation`
    /// call ends here. They used to write a field and leave the drawing to the
    /// next configuration reload, which made a new colour or duration look
    /// like it had been swallowed.
    fn apply_visuals(&mut self, what: &str) -> Response {
        self.session.settings.apply(&self.visual_config);
        self.visuals.set_settings(&self.visual_config);
        // Paused means Mochi has taken its visuals off the desktop on purpose.
        // The settings are kept and the retile that unpauses draws them.
        if !self.core.is_paused {
            let targets = self.visuals_targets();
            self.visuals.update(&targets);
        }
        tracing::info!(setting = what, "applied");
        Response::Ok
    }

    // -----------------------------------------------------------------
    // the rest of the surface
    // -----------------------------------------------------------------

    /// Recomputes every layout and applies it.
    pub fn retile(&mut self) {
        match self.core.retile() {
            Ok(changes) => self.apply_changes(changes),
            Err(e) => tracing::error!(error = %e, "could not retile"),
        }
    }

    /// Turns computed rectangles into one batched window move.
    ///
    /// The only place that writes a position, and a no-op under `--dry-run`
    /// because the platform swallows the call.
    pub fn apply_layout(&self, placements: &[(Hwnd, Rect)]) {
        if self.core.is_paused || placements.is_empty() {
            return;
        }
        self.visuals.apply_layout(placements);
    }

    /// Re-reads the configuration file and applies it to the model.
    ///
    /// The hotkey file goes with it: a user who presses reload after editing
    /// their setup means both files, and saving either one is noticed on its
    /// own anyway.
    ///
    /// # Errors
    ///
    /// When the configuration file did not parse. Nothing was read from it, so
    /// no `reload` is announced either: a subscriber that redraws on one would
    /// be redrawing for settings that never changed.
    pub fn reload_config(&mut self) -> Result<()> {
        let path = self.session.config_path().to_path_buf();
        let loaded = self.load_config();
        self.reload_hotkeys();
        loaded?;
        self.notify(NotificationEvent::Reload {
            path: path.display().to_string(),
        });
        self.retile();
        Ok(())
    }

    /// Uncloaks and restores everything Mochi took off screen.
    ///
    /// Delegates to the closure installed by
    /// [`WindowManager::install_restore_hook`] so that the panic hook and this
    /// call always do exactly the same thing.
    pub fn restore_all(&mut self) {
        crate::safety::restore_all();
    }

    /// Re-enumerates the monitors into the model.
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
                self.rebuild_monitors(&monitors);
            }
            Err(e) => tracing::error!(error = %e, "could not enumerate monitors"),
        }
    }

    // -----------------------------------------------------------------
    // notifications
    // -----------------------------------------------------------------

    fn focus(&self) -> Focus {
        let (monitor, workspace) = self.core.focused_indices().unwrap_or((0, 0));
        Focus {
            monitor,
            workspace,
            window: self.core.focused_window_id(),
        }
    }

    /// Tells the subscribers what moved since `before`.
    fn announce(&mut self, before: Focus) {
        let after = self.focus();
        if (before.monitor, before.workspace) != (after.monitor, after.workspace) {
            let name = self
                .core
                .monitors()
                .get(after.monitor)
                .map(|display| display.workspace_name(after.workspace));
            self.notify(NotificationEvent::WorkspaceChange {
                monitor: after.monitor,
                workspace: after.workspace,
                name,
            });
        }
        if before.window != after.window {
            let window = after
                .window
                .and_then(|id| self.core.window(id))
                .map(|w| WindowRef::new(w.id.get() as i64, w.title.clone(), w.exe.clone()));
            self.notify(NotificationEvent::FocusChange { window });
        }
    }

    fn notify(&self, event: NotificationEvent) {
        #[cfg(test)]
        if let Ok(mut sent) = self.sent.lock() {
            sent.push(event.clone());
        }
        self.subscribers.notify(Notification::new(event));
    }

    /// The layouts every `layout-change` notification so far carried.
    #[cfg(test)]
    fn layout_changes(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                NotificationEvent::LayoutChange { layout, .. } => Some(layout.clone()),
                _ => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// conversions
// ---------------------------------------------------------------------------

/// The model's handle for a window.
const fn window_id(hwnd: Hwnd) -> WindowId {
    WindowId(hwnd.0)
}

/// The platform's handle for a window.
const fn handle(id: WindowId) -> Hwnd {
    Hwnd(id.0)
}

fn core_window(info: &WindowInfo) -> Window {
    Window::new(info.hwnd.0)
        .with_title(info.title.clone())
        .with_exe(info.exe.clone())
        .with_class(info.class.clone())
        .with_path(info.path.clone())
}

fn rule_info(info: &WindowInfo) -> RuleInfo<'_> {
    RuleInfo::new(&info.title, &info.class, &info.exe, &info.path)
}

fn window_ref(info: &WindowInfo) -> WindowRef {
    WindowRef::new(info.hwnd.as_i64(), info.title.clone(), info.exe.clone())
}

fn apply_monitor_info(monitor: &mut Monitor, info: &MonitorInfo) {
    monitor.id = info.id.0;
    monitor.size = info.size;
    monitor.work_area = info.work_area;
    monitor.dpi = info.dpi;
    monitor.device.clone_from(&info.device_name);
    monitor.device_id.clone_from(&info.device_description);
    monitor.name = info
        .device_name
        .trim_start_matches(r"\\.\")
        .trim()
        .to_owned();
    if monitor.name.is_empty() {
        monitor.name = info.device_description.clone();
    }
}

fn direction_of(direction: mochi_client::Direction) -> mochi_core::Direction {
    match direction {
        mochi_client::Direction::Left => mochi_core::Direction::Left,
        mochi_client::Direction::Right => mochi_core::Direction::Right,
        mochi_client::Direction::Up => mochi_core::Direction::Up,
        mochi_client::Direction::Down => mochi_core::Direction::Down,
    }
}

fn axis_of(axis: mochi_client::Axis) -> mochi_core::Axis {
    match axis {
        mochi_client::Axis::Horizontal => mochi_core::Axis::Horizontal,
        mochi_client::Axis::Vertical => mochi_core::Axis::Vertical,
    }
}

fn sizing_of(sizing: mochi_client::Sizing) -> mochi_core::Sizing {
    match sizing {
        mochi_client::Sizing::Increase => mochi_core::Sizing::Increase,
        mochi_client::Sizing::Decrease => mochi_core::Sizing::Decrease,
    }
}

fn cycle_of(direction: mochi_client::CycleDirection) -> mochi_core::CycleDirection {
    match direction {
        mochi_client::CycleDirection::Next => mochi_core::CycleDirection::Next,
        mochi_client::CycleDirection::Previous => mochi_core::CycleDirection::Previous,
    }
}

fn layout_of(layout: mochi_client::Layout) -> mochi_core::Layout {
    match layout {
        mochi_client::Layout::Bsp => mochi_core::Layout::Bsp,
        mochi_client::Layout::Columns => mochi_core::Layout::Columns,
        mochi_client::Layout::Rows => mochi_core::Layout::Rows,
        mochi_client::Layout::VerticalStack => mochi_core::Layout::VerticalStack,
        mochi_client::Layout::HorizontalStack => mochi_core::Layout::HorizontalStack,
        mochi_client::Layout::UltrawideVerticalStack => mochi_core::Layout::UltrawideVerticalStack,
        mochi_client::Layout::Grid => mochi_core::Layout::Grid,
    }
}

fn identifier_of(identifier: mochi_client::RuleIdentifier) -> ApplicationIdentifier {
    match identifier {
        mochi_client::RuleIdentifier::Exe => ApplicationIdentifier::Exe,
        mochi_client::RuleIdentifier::Class => ApplicationIdentifier::Class,
        mochi_client::RuleIdentifier::Title => ApplicationIdentifier::Title,
        mochi_client::RuleIdentifier::Path => ApplicationIdentifier::Path,
    }
}

fn strategy_of(strategy: mochi_client::MatchingStrategy) -> MatchingStrategy {
    match strategy {
        mochi_client::MatchingStrategy::Equals => MatchingStrategy::Equals,
        mochi_client::MatchingStrategy::Contains => MatchingStrategy::Contains,
        mochi_client::MatchingStrategy::StartsWith => MatchingStrategy::StartsWith,
        mochi_client::MatchingStrategy::EndsWith => MatchingStrategy::EndsWith,
        mochi_client::MatchingStrategy::Regex => MatchingStrategy::Regex,
    }
}

fn container_behaviour_of(
    behaviour: mochi_client::ContainerBehaviour,
) -> mochi_core::model::WindowContainerBehaviour {
    match behaviour {
        mochi_client::ContainerBehaviour::Create => {
            mochi_core::model::WindowContainerBehaviour::Create
        }
        mochi_client::ContainerBehaviour::Append => {
            mochi_core::model::WindowContainerBehaviour::Append
        }
    }
}

fn move_behaviour_of(behaviour: mochi_client::MoveBehaviour) -> mochi_core::model::MoveBehaviour {
    match behaviour {
        mochi_client::MoveBehaviour::Swap => mochi_core::model::MoveBehaviour::Swap,
        mochi_client::MoveBehaviour::Insert => mochi_core::model::MoveBehaviour::Insert,
        mochi_client::MoveBehaviour::NoOp => mochi_core::model::MoveBehaviour::NoOp,
    }
}

fn hiding_behaviour_of(
    behaviour: mochi_client::HidingBehaviour,
) -> mochi_core::model::HidingBehaviour {
    match behaviour {
        mochi_client::HidingBehaviour::Hide => mochi_core::model::HidingBehaviour::Hide,
        mochi_client::HidingBehaviour::Minimize => mochi_core::model::HidingBehaviour::Minimize,
        mochi_client::HidingBehaviour::Cloak => mochi_core::model::HidingBehaviour::Cloak,
    }
}

fn operation_behaviour_of(
    behaviour: mochi_client::OperationBehaviour,
) -> mochi_core::model::OperationBehaviour {
    match behaviour {
        mochi_client::OperationBehaviour::Op => mochi_core::model::OperationBehaviour::Op,
        mochi_client::OperationBehaviour::NoOp => mochi_core::model::OperationBehaviour::NoOp,
    }
}

fn border_style_of(style: mochi_client::BorderStyle) -> mochi_core::config::BorderStyle {
    match style {
        mochi_client::BorderStyle::System => mochi_core::config::BorderStyle::System,
        mochi_client::BorderStyle::Rounded => mochi_core::config::BorderStyle::Rounded,
        mochi_client::BorderStyle::Square => mochi_core::config::BorderStyle::Square,
    }
}

pub(crate) fn animation_style_of(
    style: mochi_client::AnimationStyle,
) -> mochi_core::animation::AnimationStyle {
    use mochi_client::AnimationStyle as Wire;
    use mochi_core::animation::AnimationStyle as Curve;
    match style {
        Wire::Linear => Curve::Linear,
        Wire::EaseInSine => Curve::EaseInSine,
        Wire::EaseOutSine => Curve::EaseOutSine,
        Wire::EaseInOutSine => Curve::EaseInOutSine,
        Wire::EaseInQuad => Curve::EaseInQuad,
        Wire::EaseOutQuad => Curve::EaseOutQuad,
        Wire::EaseInOutQuad => Curve::EaseInOutQuad,
        Wire::EaseInCubic => Curve::EaseInCubic,
        Wire::EaseOutCubic => Curve::EaseOutCubic,
        Wire::EaseInOutCubic => Curve::EaseInOutCubic,
    }
}

/// Flips a boolean setting from a command line `enable`/`disable`.
pub fn boolean(state: BooleanState) -> bool {
    state.is_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ShowState;
    use crate::platform::WindowPlacement;
    use crate::platform::types::{MonitorId, style};
    use crate::state::Settings;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The 4K main screen, the one every single monitor test runs on.
    fn main_screen() -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(1),
            device_name: r"\\.\DISPLAY1".into(),
            device_description: "Fake".into(),
            size: Rect::new(0, 0, 3840, 2160),
            work_area: Rect::new(0, 0, 3840, 2112),
            dpi: 144,
            primary: true,
        }
    }

    /// The portrait screen to its right, at a different DPI on purpose: a
    /// cross monitor move that forgets to rescale lands in the wrong place.
    fn portrait_screen() -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(2),
            device_name: r"\\.\DISPLAY2".into(),
            device_description: "Fake portrait".into(),
            size: Rect::new(3840, 0, 4920, 1920),
            work_area: Rect::new(3840, 0, 4920, 1920),
            dpi: 96,
            primary: false,
        }
    }

    /// A platform with no desktop behind it, so the loop can be tested anywhere.
    struct FakePlatform {
        monitors: Mutex<Vec<MonitorInfo>>,
        windows: Mutex<Vec<WindowInfo>>,
        moves: AtomicUsize,
        cloaks: Mutex<Vec<(Hwnd, bool)>>,
        shows: Mutex<Vec<(Hwnd, ShowState)>>,
        focused: Mutex<Vec<Hwnd>>,
        /// How many times the keyboard was handed to the desktop.
        desktop_focus: AtomicUsize,
        placements: Mutex<Vec<(Hwnd, Rect)>>,
        history: Mutex<Vec<(Hwnd, Rect)>>,
        /// Windows that refuse to be positioned, which is what an elevated
        /// window looks like to a process that is not elevated.
        refuses: Mutex<Vec<Hwnd>>,
        /// The refusals [`Platform::take_unreachable`] has not handed over yet,
        /// mirroring the real platform: a refusal is discovered by a failed
        /// move and reported to the model exactly once.
        unreported: Mutex<Vec<Hwnd>>,
        /// Windows that stop answering `window_info` once they are off screen,
        /// which is what a cloaked or hidden window often does.
        vanishing: Mutex<Vec<Hwnd>>,
        /// A crash record to count while a window is being put back.
        record_watch: Mutex<Option<PathBuf>>,
        /// How many entries it held at each of those moments.
        record_seen: Mutex<Vec<usize>>,
        /// Windows that cannot be uncloaked, the way an elevated one cannot.
        unrestorable: Mutex<Vec<Hwnd>>,
        /// Windows Windows reports as maximized, which a test drives directly
        /// to stand in for the user pressing the maximize button.
        zoomed: Mutex<Vec<Hwnd>>,
    }

    impl FakePlatform {
        fn new(windows: Vec<WindowInfo>) -> Self {
            Self::with_monitors(windows, vec![main_screen()])
        }

        fn with_monitors(windows: Vec<WindowInfo>, monitors: Vec<MonitorInfo>) -> Self {
            Self {
                monitors: Mutex::new(monitors),
                windows: Mutex::new(windows),
                moves: AtomicUsize::new(0),
                cloaks: Mutex::new(Vec::new()),
                shows: Mutex::new(Vec::new()),
                focused: Mutex::new(Vec::new()),
                desktop_focus: AtomicUsize::new(0),
                placements: Mutex::new(Vec::new()),
                history: Mutex::new(Vec::new()),
                refuses: Mutex::new(Vec::new()),
                unreported: Mutex::new(Vec::new()),
                vanishing: Mutex::new(Vec::new()),
                record_watch: Mutex::new(None),
                record_seen: Mutex::new(Vec::new()),
                unrestorable: Mutex::new(Vec::new()),
                zoomed: Mutex::new(Vec::new()),
            }
        }

        fn last_placements(&self) -> Vec<(Hwnd, Rect)> {
            self.placements.lock().unwrap().clone()
        }

        /// Plugs a screen in or pulls it out. The daemon only notices on the
        /// next monitor event, exactly like a real display change.
        fn set_monitors(&self, monitors: Vec<MonitorInfo>) {
            *self.monitors.lock().unwrap() = monitors;
        }

        /// The user pressed the maximize button on this window.
        fn user_maximizes(&self, hwnd: Hwnd) {
            self.zoomed.lock().unwrap().push(hwnd);
        }

        /// And pressed restore again.
        fn user_restores(&self, hwnd: Hwnd) {
            self.zoomed.lock().unwrap().retain(|other| *other != hwnd);
        }

        /// Makes one window unpositionable from now on.
        fn refuse(&self, hwnd: Hwnd) {
            self.refuses.lock().unwrap().push(hwnd);
        }

        /// Makes one window unreadable from the moment it goes off screen.
        fn vanish_when_hidden(&self, hwnd: Hwnd) {
            self.vanishing.lock().unwrap().push(hwnd);
        }

        /// Counts the entries of the crash record on every uncloak from now on.
        fn watch_record(&self, path: &Path) {
            *self.record_watch.lock().unwrap() = Some(path.to_path_buf());
        }

        /// Makes one window impossible to put back on screen.
        fn cannot_be_restored(&self, hwnd: Hwnd) {
            self.unrestorable.lock().unwrap().push(hwnd);
        }

        /// Drops a window that was told to vanish once it is off screen.
        fn went_off_screen(&self, hwnd: Hwnd) {
            if self.vanishing.lock().unwrap().contains(&hwnd) {
                self.windows.lock().unwrap().retain(|w| w.hwnd != hwnd);
            }
        }

        /// Forgets every move made so far.
        fn clear_history(&self) {
            self.history.lock().unwrap().clear();
        }

        /// The last rectangle one window was moved to, in any batch.
        fn rect_of(&self, hwnd: Hwnd) -> Option<Rect> {
            self.history
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(h, _)| *h == hwnd)
                .map(|&(_, rect)| rect)
        }
    }

    impl Platform for FakePlatform {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn monitors(&self) -> Result<Vec<MonitorInfo>> {
            Ok(self.monitors.lock().unwrap().clone())
        }
        fn windows(&self) -> Result<Vec<WindowInfo>> {
            Ok(self.windows.lock().unwrap().clone())
        }
        fn window_info(&self, hwnd: Hwnd) -> Result<WindowInfo> {
            self.windows
                .lock()
                .unwrap()
                .iter()
                .find(|w| w.hwnd == hwnd)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no such window"))
        }
        fn foreground_window(&self) -> Option<Hwnd> {
            self.windows.lock().unwrap().first().map(|w| w.hwnd)
        }
        fn window_at(&self, _x: i32, _y: i32) -> Option<Hwnd> {
            None
        }
        fn cursor_position(&self) -> Result<(i32, i32)> {
            Ok((0, 0))
        }

        fn is_maximized(&self, hwnd: Hwnd) -> bool {
            self.zoomed.lock().unwrap().contains(&hwnd)
        }
        fn outranks_us(&self, hwnd: Hwnd) -> bool {
            // Mirrors the real thing: the executable is unreadable for exactly
            // the windows whose process Mochi may not open.
            self.windows
                .lock()
                .unwrap()
                .iter()
                .find(|w| w.hwnd == hwnd)
                .is_some_and(|w| w.exe.is_empty())
        }
                fn is_on_screen(&self, hwnd: Hwnd) -> bool {
            // A handle the fake desktop no longer holds answers true, the way
            // the real one does: that window is gone, not hidden.
            self.windows
                .lock()
                .unwrap()
                .iter()
                .find(|w| w.hwnd == hwnd)
                .is_none_or(|w| w.visible && !w.cloaked)
        }
        fn set_positions(&self, placements: &[WindowPlacement]) -> Result<()> {
            // The real platform falls back to one window at a time when a
            // batch is refused, so the windows it can move still move and only
            // the rest is reported. This mirrors that.
            let refused = self.refuses.lock().unwrap().clone();
            let (allowed, denied): (Vec<_>, Vec<_>) = placements
                .iter()
                .map(|p| (p.hwnd, p.rect))
                .partition(|(hwnd, _)| !refused.contains(hwnd));
            self.moves.fetch_add(allowed.len(), Ordering::SeqCst);
            *self.placements.lock().unwrap() = allowed.clone();
            self.history.lock().unwrap().extend(allowed);
            if denied.is_empty() {
                Ok(())
            } else {
                let mut unreported = self.unreported.lock().unwrap();
                unreported.extend(denied.iter().map(|&(hwnd, _)| hwnd));
                Err(anyhow::anyhow!(
                    "{} of {} windows could not be positioned",
                    denied.len(),
                    placements.len()
                ))
            }
        }
        fn take_unreachable(&self) -> Vec<Hwnd> {
            std::mem::take(&mut *self.unreported.lock().unwrap())
        }

        fn set_cloaked(&self, hwnd: Hwnd, cloaked: bool) -> Result<()> {
            if let Some(path) = self.record_watch.lock().unwrap().clone() {
                let count = crate::recover::load(&path).map_or(0, |entries| entries.len());
                self.record_seen.lock().unwrap().push(count);
            }
            if !cloaked && self.unrestorable.lock().unwrap().contains(&hwnd) {
                return Err(anyhow::anyhow!("this window cannot be uncloaked"));
            }
            self.cloaks.lock().unwrap().push((hwnd, cloaked));
            if cloaked {
                self.went_off_screen(hwnd);
            }
            Ok(())
        }
        fn show(&self, hwnd: Hwnd, state: ShowState) -> Result<()> {
            self.shows.lock().unwrap().push((hwnd, state));
            if matches!(state, ShowState::Hide | ShowState::Minimize) {
                self.went_off_screen(hwnd);
            }
            Ok(())
        }
        fn focus(&self, hwnd: Hwnd) -> Result<()> {
            self.focused.lock().unwrap().push(hwnd);
            Ok(())
        }
        fn focus_desktop(&self) -> Result<()> {
            self.desktop_focus.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn close(&self, _hwnd: Hwnd) -> Result<()> {
            Ok(())
        }
        fn set_transparency(&self, _hwnd: Hwnd, _alpha: Option<u8>) -> Result<()> {
            Ok(())
        }
        fn set_topmost(&self, _hwnd: Hwnd, _topmost: bool) -> Result<()> {
            Ok(())
        }
        fn set_cursor_position(&self, _x: i32, _y: i32) -> Result<()> {
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

    /// Serialises the tests that drive game mode.
    ///
    /// The hotkey gate is ONE process-wide atomic (`events::hotkey::GATE`),
    /// deliberately, so that `set-hotkeys disable` is in force by the time it
    /// answers. Tests run in parallel inside one process, so two of them
    /// entering and leaving game mode at the same time read each other's gate
    /// and fail in ways that have nothing to do with what they assert.
    fn game_mode_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn manager(windows: Vec<WindowInfo>) -> (WindowManager, Arc<FakePlatform>) {
        manager_on(windows, vec![main_screen()])
    }

    /// A manager that starts with `hwnd` holding the foreground, the way a
    /// desktop looks when the user was last working in that window.
    ///
    /// Startup points the model at whatever `Platform::foreground_window`
    /// reports, and the fake answers that with the FIRST window it was given.
    /// A test about the focus has to name the window it starts on rather than
    /// lean on the order the windows happened to be adopted in.
    fn manager_focused_on(
        windows: Vec<WindowInfo>,
        hwnd: Hwnd,
    ) -> (WindowManager, Arc<FakePlatform>) {
        let (mut wm, platform) = manager(windows);
        wm.on_window_event(WindowEventKind::Foreground, hwnd);
        (wm, platform)
    }

    /// [`manager_focused_on`] over several screens.
    fn manager_on_focused_on(
        windows: Vec<WindowInfo>,
        monitors: Vec<MonitorInfo>,
        hwnd: Hwnd,
    ) -> (WindowManager, Arc<FakePlatform>) {
        let (mut wm, platform) = manager_on(windows, monitors);
        wm.on_window_event(WindowEventKind::Foreground, hwnd);
        (wm, platform)
    }

    /// A manager over a chosen set of screens.
    fn manager_on(
        windows: Vec<WindowInfo>,
        monitors: Vec<MonitorInfo>,
    ) -> (WindowManager, Arc<FakePlatform>) {
        let platform = Arc::new(FakePlatform::with_monitors(windows, monitors));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut session = State::new(PathBuf::from(r"C: owhere\mochi.json"), true);
        session.settings = Settings::default();
        let wm = WindowManager::new(Arc::clone(&platform) as Arc<dyn Platform>, tx, rx, session)
            .unwrap();
        (wm, platform)
    }

    #[test]
    fn startup_puts_every_window_into_the_model_and_tiles_it() {
        let (wm, platform) = manager(vec![window(1, "Editor"), window(2, "Browser")]);
        assert_eq!(wm.state().monitors().len(), 1);
        assert_eq!(wm.state().all_window_ids().count(), 2);
        assert_eq!(wm.state().workspace(0, 0).unwrap().containers().len(), 2);

        // Two windows, side by side inside the work area minus the padding.
        let placed = platform.last_placements();
        assert_eq!(placed.len(), 2);
        assert!(placed.iter().all(|(_, rect)| rect.width() > 0));
        assert!(!placed[0].1.intersects(&placed[1].1), "{placed:?}");
    }

    #[test]
    fn the_monitor_ring_carries_the_platform_facts() {
        let (wm, _) = manager(vec![]);
        let monitor = wm.state().monitors().get(0).unwrap();
        assert_eq!(monitor.id, 1);
        assert_eq!(monitor.name, "DISPLAY1");
        assert_eq!(monitor.device, r"\\.\DISPLAY1");
        assert_eq!(monitor.device_id, "Fake");
        assert_eq!(monitor.dpi, 144);
        assert_eq!(monitor.work_area, Rect::new(0, 0, 3840, 2112));
    }

    #[test]
    fn a_tool_window_is_only_managed_when_its_class_was_named() {
        let mut tool = window(3, "MochiTest 1");
        tool.class = "MochiTestWindow".into();
        tool.ex_style = crate::platform::types::ex_style::WS_EX_TOOLWINDOW;

        let (wm, _) = manager(vec![tool.clone()]);
        assert_eq!(wm.state().all_window_ids().count(), 0, "left alone");

        let platform = Arc::new(FakePlatform::new(vec![tool]));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut session = State::new(PathBuf::from(r"C: owhere\mochi.json"), true);
        session.manage_classes = vec!["MochiTestWindow".into()];
        let wm = WindowManager::new(platform, tx, rx, session).unwrap();
        assert_eq!(wm.state().all_window_ids().count(), 1, "managed by class");
    }

    #[test]
    fn manage_class_ignores_every_other_window_on_the_desktop() {
        let mut tool = window(3, "MochiTest 1");
        tool.class = "MochiTestWindow".into();
        tool.ex_style = crate::platform::types::ex_style::WS_EX_TOOLWINDOW;

        let platform = Arc::new(FakePlatform::new(vec![
            window(1, "The user's editor"),
            tool,
            window(2, "The user's browser"),
        ]));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut session = State::new(PathBuf::from(r"C: owhere\mochi.json"), true);
        session.manage_classes = vec!["MochiTestWindow".into()];
        let wm = WindowManager::new(platform, tx, rx, session).unwrap();

        assert_eq!(
            wm.state().all_window_ids().collect::<Vec<_>>(),
            vec![WindowId(3)],
            "only the test window was taken"
        );
    }

    #[test]
    fn the_whkdrc_commands_all_reach_the_model() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two"), window(3, "Three")]);

        for command in [
            Command::Focus {
                direction: mochi_client::Direction::Left,
            },
            Command::Move {
                direction: mochi_client::Direction::Right,
            },
            Command::ResizeAxis {
                axis: mochi_client::Axis::Horizontal,
                sizing: mochi_client::Sizing::Increase,
            },
            Command::Promote,
            Command::ToggleFloat,
            Command::ToggleFloat,
            Command::ToggleMonocle,
            Command::ToggleMonocle,
            Command::ToggleMaximize,
            Command::ToggleMaximize,
            Command::CycleLayout {
                direction: mochi_client::CycleDirection::Next,
            },
            Command::FlipLayout {
                axis: mochi_client::Axis::Horizontal,
            },
            Command::ChangeLayout {
                layout: mochi_client::Layout::Bsp,
            },
            Command::FocusWorkspace { index: 1 },
            Command::MoveToWorkspace { index: 2 },
            Command::FocusLastWorkspace,
            Command::CycleWorkspace {
                direction: mochi_client::CycleDirection::Next,
            },
            Command::FocusWorkspace { index: 0 },
            Command::Retile,
            Command::TogglePause,
            Command::TogglePause,
            Command::Minimize,
            Command::Close,
        ] {
            let name = command.name();
            let (response, flow) = wm.handle_command(command);
            assert_eq!(flow, Flow::Continue);
            assert!(
                response.is_ok(),
                "{name}: {}",
                response.error_message().unwrap_or_default()
            );
        }
    }

    #[test]
    fn a_fresh_start_focuses_the_screen_the_user_is_actually_on() {
        // Startup cached the foreground handle in the daemon and told the
        // MODEL nothing, so the model sat on the first monitor's first
        // workspace however the desktop actually looked. Until the user
        // happened to click something, every command that works from the focus
        // acted on whichever screen enumerated first - and on a desk where
        // that screen is a portrait panel with nothing on it, a focus or move
        // binding pressed straight after a start did nothing at all.
        let mut over_there = window(7, "On the portrait screen");
        over_there.monitor = Some(MonitorId(2));
        let (wm, _) = manager_on(
            vec![over_there, window(1, "Editor")],
            vec![main_screen(), portrait_screen()],
        );

        assert_eq!(wm.foreground, Some(Hwnd(7)), "the fake reports this one");
        assert_eq!(
            wm.state().focused_monitor_idx(),
            1,
            "the model is looking at a different screen than the user is"
        );
        assert_eq!(
            wm.state().focused_window_id(),
            Some(WindowId(7)),
            "the model and the desktop disagree about what is focused"
        );
    }

    #[test]
    fn focus_moves_between_the_tiles() {
        let (mut wm, _) = manager_focused_on(vec![window(1, "One"), window(2, "Two")], Hwnd(2));
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(2)));

        wm.handle_command(Command::Focus {
            direction: mochi_client::Direction::Left,
        });
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(1)));

        wm.handle_command(Command::Focus {
            direction: mochi_client::Direction::Right,
        });
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(2)));
    }

    /// The window in each container of the first workspace, in ring order.
    fn ring_order(wm: &WindowManager) -> Vec<Option<WindowId>> {
        wm.state()
            .workspace(0, 0)
            .unwrap()
            .containers()
            .iter()
            .map(mochi_core::model::Container::focused_window_id)
            .collect()
    }

    #[test]
    fn cycle_focus_walks_the_container_ring_by_position() {
        // The point of this one next to `focus`: it never has to decide what
        // is to the left, so it works the same on every layout.
        let (mut wm, _) = manager_focused_on(
            vec![window(1, "One"), window(2, "Two"), window(3, "Three")],
            Hwnd(3),
        );
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(3)));

        assert_eq!(
            wm.handle_command(Command::CycleFocus {
                direction: mochi_client::CycleDirection::Next,
            })
            .0,
            Response::Ok
        );
        assert_eq!(
            wm.state().focused_window_id(),
            Some(WindowId(1)),
            "the ring wraps"
        );

        wm.handle_command(Command::CycleFocus {
            direction: mochi_client::CycleDirection::Previous,
        });
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(3)));
        assert_eq!(
            ring_order(&wm),
            vec![Some(WindowId(1)), Some(WindowId(2)), Some(WindowId(3))],
            "focus alone moved nothing"
        );
    }

    #[test]
    fn cycle_move_swaps_the_focused_window_along_the_ring() {
        let (mut wm, _) = manager_focused_on(
            vec![window(1, "One"), window(2, "Two"), window(3, "Three")],
            Hwnd(3),
        );
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(3)));

        assert_eq!(
            wm.handle_command(Command::CycleMove {
                direction: mochi_client::CycleDirection::Next,
            })
            .0,
            Response::Ok
        );
        assert_eq!(
            ring_order(&wm),
            vec![Some(WindowId(3)), Some(WindowId(2)), Some(WindowId(1))],
            "the wrap swapped the last container with the first"
        );
        assert_eq!(
            wm.state().focused_window_id(),
            Some(WindowId(3)),
            "the focus travelled with the window"
        );
    }

    #[test]
    fn promote_focus_focuses_the_front_of_the_ring_without_moving_anything() {
        let (mut wm, _) = manager_focused_on(
            vec![window(1, "One"), window(2, "Two"), window(3, "Three")],
            Hwnd(3),
        );
        let before = ring_order(&wm);
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(3)));

        assert_eq!(wm.handle_command(Command::PromoteFocus).0, Response::Ok);
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(1)));
        assert_eq!(
            ring_order(&wm),
            before,
            "promote-focus is the half of promote that moves no window"
        );
    }

    #[test]
    fn toggle_tiling_stops_arranging_a_workspace_without_unmanaging_anything() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        assert!(wm.state().workspace(0, 0).unwrap().tile);

        assert_eq!(wm.handle_command(Command::ToggleTiling).0, Response::Ok);
        assert!(!wm.state().workspace(0, 0).unwrap().tile);
        assert!(
            wm.state()
                .workspace(0, 0)
                .unwrap()
                .latest_layout()
                .is_empty(),
            "an untiled workspace lays nothing out"
        );
        assert_eq!(
            wm.state().all_window_ids().count(),
            2,
            "this is not unmanage: both windows are still Mochi's"
        );

        wm.handle_command(Command::ToggleTiling);
        assert!(wm.state().workspace(0, 0).unwrap().tile);
        assert_eq!(wm.state().workspace(0, 0).unwrap().latest_layout().len(), 2);
    }

    #[test]
    fn send_to_workspace_moves_the_window_and_leaves_the_focus_behind() {
        let (mut wm, _) = manager_focused_on(vec![window(1, "One"), window(2, "Two")], Hwnd(2));
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(2)));

        assert_eq!(
            wm.handle_command(Command::SendToWorkspace { index: 1 }).0,
            Response::Ok
        );
        assert_eq!(wm.state().locate_window(WindowId(2)), Some((0, 1)));
        assert_eq!(
            wm.state().focused_indices().unwrap(),
            (0, 0),
            "send is move-to-workspace without the following"
        );
        assert_eq!(wm.state().workspace(0, 0).unwrap().containers().len(), 1);
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(1)));

        // The contrast, on the very next command: `move-to-workspace` follows.
        wm.handle_command(Command::MoveToWorkspace { index: 2 });
        assert_eq!(wm.state().focused_indices().unwrap(), (0, 2));
    }

    #[test]
    fn send_to_monitor_moves_the_window_and_leaves_the_focus_behind() {
        let (mut wm, _) = manager_on_focused_on(
            vec![window(1, "One"), window(2, "Two")],
            vec![main_screen(), portrait_screen()],
            Hwnd(2),
        );
        assert_eq!(wm.state().focused_window_id(), Some(WindowId(2)));

        assert_eq!(
            wm.handle_command(Command::SendToMonitor { index: 1 }).0,
            Response::Ok
        );
        assert_eq!(wm.state().locate_window(WindowId(2)), Some((1, 0)));
        assert_eq!(
            wm.state().focused_monitor_idx(),
            0,
            "send is move-to-monitor without the following"
        );
        assert_eq!(wm.state().workspace(0, 0).unwrap().containers().len(), 1);
        assert_eq!(wm.state().workspace(1, 0).unwrap().containers().len(), 1);
    }

    #[test]
    fn resize_edge_moves_the_edge_it_was_given_and_no_other() {
        let (mut wm, _) = manager_focused_on(vec![window(1, "One"), window(2, "Two")], Hwnd(2));
        // The focused window is the right hand tile, so its right edge is the
        // edge of the screen and there is nothing on that side to push.
        let before = wm.state().rect_for_window(WindowId(2)).unwrap();

        assert_eq!(
            wm.handle_command(Command::ResizeEdge {
                direction: mochi_client::Direction::Right,
                sizing: mochi_client::Sizing::Increase,
            })
            .0,
            Response::Ok
        );
        assert_eq!(
            wm.state().rect_for_window(WindowId(2)),
            Some(before),
            "resize-axis would have moved the other edge instead"
        );

        wm.handle_command(Command::ResizeEdge {
            direction: mochi_client::Direction::Left,
            sizing: mochi_client::Sizing::Increase,
        });
        let after = wm.state().rect_for_window(WindowId(2)).unwrap();
        assert!(after.left < before.left, "{after:?} against {before:?}");
        assert_eq!(after.right, before.right, "the far edge stayed put");
        assert_eq!(after.top, before.top);
        assert_eq!(after.bottom, before.bottom);
    }

    #[test]
    fn toggle_float_override_floats_the_next_window_to_appear() {
        let (mut wm, platform) = manager(vec![window(1, "One")]);
        assert!(!wm.state().float_override);

        assert_eq!(
            wm.handle_command(Command::ToggleFloatOverride).0,
            Response::Ok
        );
        assert!(wm.state().float_override);

        platform.windows.lock().unwrap().push(window(2, "Two"));
        wm.on_window_event(WindowEventKind::Created, Hwnd(2));
        assert_eq!(
            wm.state().workspace(0, 0).unwrap().floating_windows().len(),
            1,
            "the new window was tiled anyway"
        );
        assert_eq!(
            wm.state().workspace(0, 0).unwrap().containers().len(),
            1,
            "the window that was already tiled was disturbed"
        );

        wm.handle_command(Command::ToggleFloatOverride);
        assert!(!wm.state().float_override);
        platform.windows.lock().unwrap().push(window(3, "Three"));
        wm.on_window_event(WindowEventKind::Created, Hwnd(3));
        assert_eq!(
            wm.state().workspace(0, 0).unwrap().containers().len(),
            2,
            "new windows are tiled again"
        );
        assert_eq!(
            wm.state().workspace(0, 0).unwrap().floating_windows().len(),
            1,
            "the one that floated was pulled back into the layout"
        );
    }

    #[test]
    fn a_hidden_window_is_put_back_before_it_is_ever_let_go_of() {
        // The worst thing this daemon can do is forget a window it is hiding.
        // Every reason to unmanage can arrive while a workspace is off screen:
        // the user minimizes something, a virtual desktop switch cloaks it, a
        // rule changes under it. Dropping the record without uncloaking leaves
        // the window invisible, out of the model and out of the restore
        // record at once, and nothing that is left knows it exists.
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);

        // Put both off screen, the way a workspace switch does.
        wm.handle_command(Command::FocusWorkspace { index: 1 });
        assert_eq!(wm.hidden().lock().unwrap().len(), 2, "both are hidden");
        platform.cloaks.lock().unwrap().clear();

        // Now something else takes one of them away while it is still hidden.
        wm.unmanage(Hwnd(1), "minimized");

        let uncloaked: Vec<_> = platform
            .cloaks
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, on)| !*on)
            .map(|(h, _)| *h)
            .collect();
        assert!(
            uncloaked.contains(&Hwnd(1)),
            "the window was forgotten while still cloaked, so nothing can \
             bring it back: not the model, not the restore record, not the \
             next start"
        );
        assert!(
            !wm.hidden().lock().unwrap().contains(Hwnd(1)),
            "it was uncloaked but the record still claims to owe it back"
        );
        assert_eq!(
            wm.hidden().lock().unwrap().len(),
            1,
            "the other hidden window was disturbed"
        );
    }

    #[test]
    fn a_window_that_is_gone_is_not_chased_with_an_uncloak() {
        // A destroyed window is the one case where there is nothing to put
        // back, and asking the platform would only fail loudly.
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.handle_command(Command::FocusWorkspace { index: 1 });
        platform.cloaks.lock().unwrap().clear();
        platform
            .windows
            .lock()
            .unwrap()
            .retain(|w| w.hwnd != Hwnd(1));

        wm.unmanage(Hwnd(1), "destroyed");

        assert!(
            platform.cloaks.lock().unwrap().is_empty(),
            "a dead window was chased with an uncloak"
        );
        assert!(!wm.hidden().lock().unwrap().contains(Hwnd(1)));
    }

    #[test]
    fn a_dry_run_never_touches_the_real_off_screen_record() {
        // There is one record per user and the running daemon owns it. A dry
        // run cloaks nothing, so it has nothing to put there, and every test in
        // this module builds its manager this way: while this pointed at the
        // real path, running the suite overwrote a live daemon's record with
        // fake handles, leaving the windows it had genuinely cloaked with
        // nothing on disk to bring them back.
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        assert!(
            wm.hidden().lock().unwrap().record.is_none(),
            "a dry run is mirroring to the record the real daemon owns"
        );

        // Still true once something has actually been hidden.
        wm.handle_command(Command::FocusWorkspace { index: 1 });
        assert_eq!(wm.hidden().lock().unwrap().len(), 2, "it did hide them");
        assert!(
            wm.hidden().lock().unwrap().record.is_none(),
            "hiding a window gave the dry run a record to write"
        );
    }

    #[test]
    fn a_workspace_switch_cloaks_and_uncloaks_exactly_what_it_should() {
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);
        platform.cloaks.lock().unwrap().clear();

        wm.handle_command(Command::FocusWorkspace { index: 1 });
        let cloaked: Vec<_> = platform
            .cloaks
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, on)| *on)
            .map(|(h, _)| *h)
            .collect();
        assert_eq!(cloaked.len(), 2, "both windows went off screen");
        assert_eq!(wm.hidden().lock().unwrap().len(), 2);

        wm.handle_command(Command::FocusWorkspace { index: 0 });
        let uncloaked: Vec<_> = platform
            .cloaks
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, on)| !*on)
            .map(|(h, _)| *h)
            .collect();
        assert_eq!(uncloaked.len(), 2, "both came back");
        assert!(wm.hidden().lock().unwrap().is_empty());
    }

    #[test]
    fn mochi_ignores_the_cloak_events_its_own_workspace_switch_produces() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.handle_command(Command::FocusWorkspace { index: 1 });

        wm.on_window_event(WindowEventKind::Cloaked, Hwnd(1));
        wm.on_window_event(WindowEventKind::Cloaked, Hwnd(2));
        assert_eq!(
            wm.state().all_window_ids().count(),
            2,
            "a workspace switch must not unmanage anything"
        );
    }

    #[test]
    fn a_window_cloaked_by_something_else_leaves_the_model() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.on_window_event(WindowEventKind::Cloaked, Hwnd(2));
        assert_eq!(wm.state().all_window_ids().count(), 1);
    }

    #[test]
    fn a_destroyed_window_leaves_the_model_and_the_rest_retiles() {
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.on_window_event(WindowEventKind::Destroyed, Hwnd(2));
        assert_eq!(wm.state().all_window_ids().count(), 1);

        let placed = platform.last_placements();
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].0, Hwnd(1));
        assert_eq!(
            Some(placed[0].1),
            wm.state().rect_for_window(WindowId(1)),
            "the survivor was moved to the rectangle the model gave it"
        );
        let work_area = wm.state().work_area_for(0, 0).unwrap();
        assert!(work_area.contains_rect(&placed[0].1), "{placed:?}");
    }

    #[test]
    fn minimize_drops_the_window_and_restoring_it_brings_it_back() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.on_window_event(WindowEventKind::MinimizeStart, Hwnd(2));
        assert_eq!(wm.state().all_window_ids().count(), 1);

        wm.on_window_event(WindowEventKind::MinimizeEnd, Hwnd(2));
        assert_eq!(wm.state().all_window_ids().count(), 2);
    }

    #[test]
    fn a_window_that_only_gets_its_title_later_is_managed_on_the_name_change() {
        let mut late = window(3, "");
        late.title = String::new();
        let (mut wm, platform) = manager(vec![late]);
        assert_eq!(
            wm.state().all_window_ids().count(),
            0,
            "no title, no window"
        );

        platform.windows.lock().unwrap()[0].title = "Now I have one".into();
        wm.on_window_event(WindowEventKind::NameChange, Hwnd(3));
        assert_eq!(wm.state().all_window_ids().count(), 1);
        assert_eq!(
            wm.state().window(WindowId(3)).unwrap().title,
            "Now I have one"
        );
    }

    #[test]
    fn the_state_document_comes_from_the_model() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        match wm.handle_command(Command::State).0 {
            Response::State { state } => {
                assert_eq!(
                    state["monitors"][0]["workspaces"][0]["containers"][0]["windows"][0]["title"],
                    "Editor"
                );
                assert_eq!(state["window_count"], 1);
                assert_eq!(state["paused"], false);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn every_query_target_is_answered() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        for (target, expected) in [
            (QueryTarget::MonitorCount, serde_json::json!(1)),
            (QueryTarget::WindowCount, serde_json::json!(1)),
            (QueryTarget::Paused, serde_json::json!(false)),
            (QueryTarget::DryRun, serde_json::json!(true)),
            (QueryTarget::FocusedMonitorIndex, serde_json::json!(0)),
            (QueryTarget::FocusedWorkspaceIndex, serde_json::json!(0)),
            (QueryTarget::FocusedContainerIndex, serde_json::json!(0)),
            (QueryTarget::FocusedWindowIndex, serde_json::json!(0)),
            (QueryTarget::FocusedWorkspaceName, serde_json::json!("1")),
        ] {
            match wm.handle_command(Command::Query { target }).0 {
                Response::Query { answer } => assert_eq!(answer, expected, "for {target}"),
                other => panic!("unexpected response for {target}: {other:?}"),
            }
        }
    }

    #[test]
    fn stop_ends_the_loop_and_answers_ok() {
        let (mut wm, _) = manager(vec![]);
        let (response, flow) = wm.handle_command(Command::Stop);
        assert_eq!(response, Response::Ok);
        assert_eq!(flow, Flow::Stop);
    }

    #[test]
    fn a_paused_manager_moves_nothing() {
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.handle_command(Command::TogglePause);
        assert!(wm.state().is_paused);

        let before = platform.moves.load(Ordering::SeqCst);
        wm.handle_command(Command::Retile);
        wm.apply_layout(&[(Hwnd(1), Rect::new(0, 0, 100, 100))]);
        assert_eq!(platform.moves.load(Ordering::SeqCst), before);

        wm.handle_command(Command::TogglePause);
        assert!(
            platform.moves.load(Ordering::SeqCst) > before,
            "unpausing retiles"
        );
    }

    #[test]
    fn an_ignore_rule_added_at_runtime_unmanages_the_window() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        let (response, _) = wm.handle_command(Command::IgnoreRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "Code.exe".into(),
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        assert_eq!(response, Response::Ok);
        assert_eq!(wm.state().all_window_ids().count(), 0);
    }

    #[test]
    fn renaming_the_hotkey_file_to_mochis_own_name_does_not_unbind_the_keyboard() {
        let dir = std::env::temp_dir().join(format!("mochi-keyname-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let preferred = dir.join("hotkeys");
        let legacy = dir.join("whkdrc");
        let candidates = vec![preferred.clone(), legacy.clone()];

        // The state a migrated desktop starts in: only the borrowed name is
        // there, and that is what startup resolved to.
        std::fs::write(
            &legacy,
            "alt + h : focus left
",
        )
        .unwrap();
        assert_eq!(hotkey_file_now(&legacy, &candidates), legacy);

        // The user renames it to Mochi's own name. Before the fix the reload
        // re-read the path from startup, found nothing, and silently dropped
        // every binding, because a missing hotkey file is not an error.
        std::fs::rename(&legacy, &preferred).unwrap();
        assert_eq!(
            hotkey_file_now(&legacy, &candidates),
            preferred,
            "a reload after the rename would have read the file that is gone"
        );

        // With neither there, the path stays put so the watcher keeps waiting
        // on something rather than on nothing.
        std::fs::remove_file(&preferred).unwrap();
        assert_eq!(hotkey_file_now(&legacy, &candidates), legacy);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hotkey_path_that_was_named_is_never_second_guessed() {
        // `--hotkeys` names one file. Looking for another would quietly read a
        // file the user did not ask for.
        let named = std::path::PathBuf::from(r"D:\somewhere\keys");
        assert_eq!(hotkey_file_now(&named, &[]), named);
    }

    #[test]
    fn a_window_judged_as_the_frame_host_is_judged_again() {
        // A UWP window belongs to ApplicationFrameHost.exe until the
        // application inside it has made its own child window, which is what
        // the real process is read from. Asked too early there is no child yet,
        // so Calculator, Settings and the Store are all the same program: every
        // `exe` rule written for the real application misses, and an ignore
        // rule for it never fires. `manage` now schedules a second look, and
        // this is what that second look has to achieve.
        let mut frame = window(1, "Settings");
        frame.exe = crate::platform::FRAME_HOST.to_owned();
        frame.path = format!(r"C:\Windows\System32\{}", crate::platform::FRAME_HOST);

        let (mut wm, platform) = manager(vec![frame]);
        let (response, _) = wm.handle_command(Command::IgnoreRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "SystemSettings.exe".into(),
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        assert!(response.is_ok());
        assert!(
            wm.state().is_managed(WindowId(1)),
            "the frame host does not match the rule, so it is managed"
        );

        // The child window exists now, so the real identity can be read.
        {
            let mut windows = platform.windows.lock().unwrap();
            windows[0].exe = "SystemSettings.exe".to_owned();
            windows[0].path = r"C:\Windows\SystemSettings.exe".to_owned();
        }
        wm.on_window_event(WindowEventKind::NameChange, Hwnd(1));

        assert!(
            !wm.state().is_managed(WindowId(1)),
            "the window was never judged again, so the ignore rule written for              the real application could not fire"
        );
    }

    #[test]
    fn the_frame_host_is_recognised_whatever_its_case() {
        assert!(crate::platform::is_frame_host("ApplicationFrameHost.exe"));
        assert!(crate::platform::is_frame_host("applicationframehost.exe"));
        assert!(!crate::platform::is_frame_host("explorer.exe"));
        assert!(!crate::platform::is_frame_host(""));
    }

    #[test]
    fn a_rule_typed_at_the_keyboard_survives_a_reload() {
        // `Config::apply_to` assigns `state.rules` outright rather than writing
        // only what the file names, so every reload used to throw away whatever
        // a `mochic` command had added. Typing `mochic ignore-rule exe
        // wallpaper64.exe equals` and then editing any unrelated key in
        // mochi.json silently lost the rule, and the application it was keeping
        // out was tiled again with nothing logged.
        let dir = std::env::temp_dir().join(format!("mochi-rule-reload-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let config = dir.join("mochi.json");
        std::fs::write(&config, r#"{ "default_workspace_padding": 10 }"#).expect("write");

        let platform = Arc::new(FakePlatform::new(vec![window(1, "Editor")]));
        let (tx, rx) = std::sync::mpsc::channel();
        let session = crate::state::State::new(config.clone(), true);
        let mut wm = WindowManager::new(platform, tx, rx, session).expect("a manager");

        let (response, _) = wm.handle_command(Command::IgnoreRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "wallpaper64.exe".into(),
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        assert!(response.is_ok(), "the rule should be accepted");
        let before = wm.state().rules.own_ignore_rules.len();
        assert_eq!(before, 1, "the rule went into the user's own list");

        // The user edits something else entirely and saves.
        std::fs::write(&config, r#"{ "default_workspace_padding": 20 }"#).expect("rewrite");
        wm.reload_config().expect("the file parses");

        assert_eq!(
            wm.state().rules.own_ignore_rules.len(),
            before,
            "the reload threw away a rule the user typed"
        );
        assert_eq!(
            wm.state().default_workspace_padding,
            20,
            "and the file's own change should still have been applied"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reload_that_removes_the_toggle_does_not_trap_the_keyboard() {
        let _guard = game_mode_guard();
        // Game mode admits exactly one action. Save the hotkey file with the
        // toggle-game-mode line deleted, or with a typo on it, and every bound
        // key stays swallowed with nothing able to lift the suspension. The
        // file is reloaded the instant it is saved, so this is exactly the
        // moment somebody is editing their bindings.
        let dir = std::env::temp_dir().join(format!("mochi-gm-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let keys = dir.join("hotkeys");
        std::fs::write(
            &keys,
            "alt + g : toggle-game-mode
alt + h : focus left
",
        )
        .expect("write");

        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        wm.start_hotkeys(keys.clone(), vec![keys.clone()]);
        wm.handle_command(Command::ToggleGameMode);
        assert!(wm.state().is_paused, "game mode is on");

        // The user edits the file and loses the toggle.
        std::fs::write(
            &keys,
            "alt + h : focus left
",
        )
        .expect("rewrite");
        wm.reload_hotkeys();

        assert!(
            !wm.state().is_paused,
            "game mode was left on with no binding able to turn it off"
        );

        // And a reload that KEEPS the toggle leaves game mode alone.
        std::fs::write(
            &keys,
            "alt + g : toggle-game-mode
alt + j : focus down
",
        )
        .expect("rewrite");
        wm.handle_command(Command::ToggleGameMode);
        assert!(wm.state().is_paused, "back into game mode");
        wm.reload_hotkeys();
        assert!(
            wm.state().is_paused,
            "a reload that kept the toggle should not have left game mode"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asking_for_the_keyboard_during_game_mode_does_not_poison_the_memory() {
        let _guard = game_mode_guard();
        // The natural reaction to "my hotkeys are dead" is `set-hotkeys
        // enable`. That used to set the gate to All and leave the game-mode
        // memory armed and still saying All, so the NEXT press of the toggle
        // was read as ENTERING and overwrote the memory with the pause game
        // mode itself had set. From then on every round trip restored
        // `paused = true`: the keyboard came back, the desktop stayed untiled,
        // and the toggle could never unpause again.
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        wm.start_hotkeys(PathBuf::from("test"), Vec::new());
        assert!(!wm.state().is_paused, "not paused to begin with");

        wm.handle_command(Command::ToggleGameMode);
        assert!(wm.state().is_paused, "game mode pauses tiling");

        // The user asks for their keyboard back.
        wm.handle_command(Command::SetHotkeys {
            state: mochi_client::BooleanState::Enable,
        });
        assert!(
            !wm.state().is_paused,
            "asking for the keyboard should leave game mode, tiling and all"
        );

        // And a full round trip afterwards still ends up unpaused.
        wm.handle_command(Command::ToggleGameMode);
        assert!(wm.state().is_paused, "it enters again");
        wm.handle_command(Command::ToggleGameMode);
        assert!(
            !wm.state().is_paused,
            "the toggle could no longer unpause: the memory was poisoned"
        );
    }

    #[test]
    fn game_mode_gives_back_the_keyboard_and_the_pause_it_found() {
        let _guard = game_mode_guard();
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        wm.start_hotkeys(std::env::temp_dir().join("no-such-hotkeys"), Vec::new());

        // The user switched the keyboard off and paused tiling themselves.
        wm.handle_command(Command::SetHotkeys {
            state: mochi_client::BooleanState::Disable,
        });
        wm.handle_command(Command::TogglePause);
        assert!(wm.state().is_paused);

        wm.handle_command(Command::ToggleGameMode);
        wm.handle_command(Command::ToggleGameMode);

        // Leaving used to hand back every binding and resume tiling, neither of
        // which game mode had taken away. It also made game mode a back door
        // around `set-hotkeys disable`.
        assert_eq!(
            wm.hotkeys.as_ref().map(HotkeyDaemon::gate),
            Some(Gate::Off),
            "game mode re-armed a keyboard the user had switched off"
        );
        assert!(
            wm.state().is_paused,
            "game mode resumed a pause the user set themselves"
        );
    }

    #[test]
    fn a_window_that_refuses_to_come_back_stays_in_the_record() {
        let (mut wm, platform) = manager(vec![window(1, "Editor"), window(2, "Browser")]);
        // Off screen, by Mochi, recorded.
        wm.handle_command(Command::FocusWorkspace { index: 1 });
        assert!(wm.hidden.lock().unwrap().contains(Hwnd(1)));

        // Now it cannot be uncloaked: an elevated window, a dead shell proxy,
        // a window with no application view.
        platform.cannot_be_restored(Hwnd(1));
        wm.handle_command(Command::FocusWorkspace { index: 0 });

        // The record was cleared before the call, so the window used to end up
        // still cloaked and in no list at all: out of the restore hook, out of
        // the crash mirror, out of reach of `restore-windows`. Unrecoverable
        // without another window manager.
        assert!(
            wm.hidden.lock().unwrap().contains(Hwnd(1)),
            "a window that refused to come back was forgotten while still hidden"
        );
    }

    #[test]
    fn a_window_opened_while_paused_is_managed_when_tiling_comes_back() {
        let (mut wm, platform) = manager(vec![window(1, "Editor")]);
        wm.handle_command(Command::TogglePause);
        assert!(wm.state().is_paused);

        // The desktop carries on while Mochi is not arranging it.
        platform
            .windows
            .lock()
            .unwrap()
            .push(window(2, "Opened while paused"));
        wm.on_window_event(WindowEventKind::Created, Hwnd(2));
        assert!(
            wm.state().window(WindowId(2)).is_none(),
            "a paused daemon should not be tiling anything yet"
        );

        wm.handle_command(Command::TogglePause);

        // Pause used to be one-way: windows still left the model while it was
        // on, but nothing that appeared could get back in, so this window
        // stayed untiled over the layout for the rest of the session.
        assert!(
            wm.state().window(WindowId(2)).is_some(),
            "the window opened during the pause was never picked up"
        );
    }

    #[test]
    fn a_shell_class_with_a_version_suffix_is_still_the_shell() {
        use crate::platform::types::{Unmanageable, is_manageable};
        for class in [
            "XamlExplorerHostIslandWindow_WASDK",
            "TopLevelWindowForOverflowXamlIsland",
            "Windows.UI.Composition.DesktopWindowContentBridge_1234",
        ] {
            let mut w = window(1, "Shell surface");
            w.class = class.into();
            assert_eq!(
                is_manageable(&w),
                Err(Unmanageable::ShellClass),
                "{class} was managed like an ordinary window"
            );
        }
    }

    #[test]
    fn a_window_mochi_never_managed_does_not_keep_the_foreground_when_it_dies() {
        let (mut wm, platform) = manager(vec![window(1, "Editor")]);

        // A window Mochi does not manage can still take the foreground, and
        // the cache is written for every window, managed or not.
        let mut stray = window(9, "A dialog");
        stray.ex_style = crate::platform::types::ex_style::WS_EX_TOOLWINDOW;
        platform.windows.lock().unwrap().push(stray);
        wm.on_window_event(WindowEventKind::Foreground, Hwnd(9));
        assert_eq!(wm.foreground, Some(Hwnd(9)));

        wm.on_window_event(WindowEventKind::Destroyed, Hwnd(9));

        // It died unmanaged, so the lookup in `unmanage` returned early. The
        // handle used to stay cached for good, and Windows reuses handle
        // values: the next window to inherit 9 could never be focused, because
        // `focus_hwnd` refuses a handle it believes already has the foreground.
        assert_eq!(
            wm.foreground, None,
            "a dead unmanaged window kept the foreground"
        );
    }

    #[test]
    fn a_hotkey_file_that_changed_its_name_is_still_the_hotkey_file() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        let dir = std::env::temp_dir().join(format!("mochi-route-{}", std::process::id()));
        let preferred = dir.join("hotkeys");
        let legacy = dir.join("whkdrc");

        // What startup found, and what the watcher reports for the rest of the
        // session: it captures one path and never changes it.
        wm.hotkey_path = Some(legacy.clone());
        wm.hotkey_candidates = vec![preferred.clone(), legacy.clone()];
        assert!(wm.is_the_hotkey_file(&legacy));

        // The user renames the file, so a reload moves the path it reads.
        wm.hotkey_path = Some(preferred.clone());

        // The watcher still reports the old name. Comparing that against the
        // path in hand answered "this is mochi.json", so every later save of
        // the hotkey file ran a full configuration reload: the bindings were
        // never re-read, and the reload threw away the rules and the layouts a
        // command had set.
        assert!(
            wm.is_the_hotkey_file(&legacy),
            "a hotkey save after the rename was mistaken for a config change"
        );
        assert!(wm.is_the_hotkey_file(&preferred));
        let elsewhere = std::env::temp_dir().join("mochi.json");
        assert!(!wm.is_the_hotkey_file(&elsewhere));
    }

    #[test]
    fn a_hotkey_path_given_on_the_command_line_is_the_only_one_that_counts() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        let named = std::env::temp_dir().join("named-keys");
        wm.hotkey_path = Some(named.clone());
        // No candidates is what `--hotkeys` produces: that file and no other.
        wm.hotkey_candidates = Vec::new();
        assert!(wm.is_the_hotkey_file(&named));
        assert!(!wm.is_the_hotkey_file(&std::env::temp_dir().join("whkdrc")));
    }

    #[test]
    fn crossing_to_an_empty_screen_takes_the_keyboard_with_it() {
        // The two-monitor case that bites in practice: windows on one screen,
        // nothing on the other. The model moves its focused monitor and names
        // no window to focus, because there is none. Left there, the keyboard
        // stays on the screen the user just navigated away from while every
        // border says nothing is focused, so the next thing typed goes into a
        // window they are no longer looking at.
        let (mut wm, platform) = manager_on(
            vec![window(1, "Editor"), window(2, "Browser")],
            vec![main_screen(), portrait_screen()],
        );
        platform.focused.lock().unwrap().clear();
        assert_eq!(platform.desktop_focus.load(Ordering::SeqCst), 0);

        wm.handle_command(Command::FocusMonitor { index: 1 });

        assert_eq!(wm.state().focused_monitor_idx(), 1, "the model did cross");
        assert!(
            wm.state().focused_window_id().is_none(),
            "there is nothing on the empty screen to focus"
        );
        assert_eq!(
            platform.desktop_focus.load(Ordering::SeqCst),
            1,
            "the keyboard was left behind on the screen with the windows"
        );
        assert!(
            platform.focused.lock().unwrap().is_empty(),
            "a window was focused on a screen that has none: {:?}",
            platform.focused.lock().unwrap()
        );
    }

    #[test]
    fn switching_to_an_empty_workspace_does_not_leave_the_keyboard_on_a_cloaked_window() {
        // The common half of the same defect, on one screen: every window of
        // the workspace goes off screen, including the one holding the
        // keyboard, and the model names nothing to focus because the workspace
        // arrived at is empty. Left there, the daemon believes the foreground
        // is a window it has just cloaked, and Windows hands the keyboard to
        // whatever it likes, which can be a window on another monitor.
        let (mut wm, platform) =
            manager_focused_on(vec![window(1, "Editor"), window(2, "Browser")], Hwnd(2));
        platform.focused.lock().unwrap().clear();
        assert_eq!(wm.foreground, Some(Hwnd(2)), "it starts on a real window");

        wm.handle_command(Command::FocusWorkspace { index: 1 });

        assert_eq!(
            platform.desktop_focus.load(Ordering::SeqCst),
            1,
            "the keyboard was left on a window that is now off screen"
        );
        assert_eq!(
            wm.foreground, None,
            "the daemon still believes a cloaked window holds the keyboard"
        );
    }

    #[test]
    fn the_doctor_reports_a_tile_held_by_a_window_that_is_not_there() {
        // The check the 961 tests cannot do for themselves. They drive a fake
        // desktop that only changes when Mochi changes it, so the model and
        // the screen can never drift apart in a test the way they do on a real
        // one. `doctor` asks the question against whatever desktop is actually
        // there, which is where every defect of this kind has been found.
        let (mut wm, platform) = manager(vec![window(1, "Editor"), window(2, "Browser")]);
        for info in platform.windows.lock().unwrap().iter_mut() {
            if info.hwnd == Hwnd(2) {
                info.cloaked = true;
            }
        }

        let Response::Doctor { doctor } = wm.handle_command(Command::Doctor).0 else {
            panic!("doctor answered with the wrong kind of response");
        };
        let findings = doctor["findings"].as_array().expect("findings is a list");
        assert_eq!(findings.len(), 1, "{doctor:#}");
        assert_eq!(findings[0]["kind"], "hole");
        assert_eq!(findings[0]["title"], "Browser");
    }

    #[test]
    fn the_doctor_says_when_a_window_is_swallowing_every_hotkey() {
        // The finding that cost the most to work out by hand, and the one
        // nothing else can tell you. A low-level keyboard hook in a normal
        // process is given no key presses at all while a window that outranks
        // it holds the focus. Every binding stops working, but only inside
        // that one window, and neither the screen nor the log says a word. It
        // is indistinguishable from the hotkeys being broken.
        let mut admin = window(9, "Administrator: Terminal");
        // An unreadable executable is exactly what a process Mochi may not
        // open looks like, which is the same boundary that blocks the hook.
        admin.exe = String::new();
        let (mut wm, _platform) = manager(vec![window(1, "Editor"), admin]);

        let Response::Doctor { doctor } = wm.handle_command(Command::Doctor).0 else {
            panic!("doctor answered with the wrong kind of response");
        };
        let findings = doctor["findings"].as_array().expect("a list");
        let blocked: Vec<_> = findings
            .iter()
            .filter(|f| f["kind"] == "hotkeys-blocked")
            .collect();
        assert_eq!(blocked.len(), 1, "{doctor:#}");
        assert_eq!(blocked[0]["title"], "Administrator: Terminal");
    }

    #[test]
    fn the_doctor_reports_a_hidden_window_it_can_no_longer_put_back() {
        let mut stuck = window(5, "Hidden and out of reach");
        stuck.exe = String::new();
        stuck.cloaked = true;
        let (wm, _platform) = manager(vec![window(1, "Editor"), stuck]);
        record(&wm.hidden).hide(Hwnd(5), HidingBehaviour::Cloak);

        let Response::Doctor { doctor } = wm.diagnose() else {
            panic!("doctor answered with the wrong kind of response");
        };
        let findings = doctor["findings"].as_array().expect("a list");
        assert!(
            findings.iter().any(|f| f["kind"] == "cannot-restore"),
            "a window Mochi hid and can no longer reach went unreported: {doctor:#}"
        );
    }

    #[test]
    fn the_doctor_is_quiet_when_the_model_and_the_desktop_agree() {
        // The other half: a check that always finds something is a check
        // nobody reads.
        let (mut wm, _platform) = manager(vec![window(1, "Editor"), window(2, "Browser")]);
        let Response::Doctor { doctor } = wm.handle_command(Command::Doctor).0 else {
            panic!("doctor answered with the wrong kind of response");
        };
        assert!(
            doctor["findings"].as_array().expect("a list").is_empty(),
            "{doctor:#}"
        );
        assert_eq!(doctor["managed"], 2);
    }

    #[test]
    fn a_window_cloaked_behind_mochis_back_does_not_keep_its_tile() {
        // Measured on his desktop: three windows tiled as a half and two
        // quarters, one quarter empty, because a shell-cloaked window still
        // held it. `mochic restore-windows` could not help -- Mochi had no
        // record of hiding it, because it was not Mochi that hid it. Whatever
        // took the window away raised no event Mochi acted on, and the model
        // went on reserving a share of the screen for a window nobody could
        // see, squeezing the other two around a hole.
        let (mut wm, platform) = manager(vec![
            window(1, "Editor"),
            window(2, "Browser"),
            window(3, "Chat"),
        ]);
        assert!(wm.state().is_managed(window_id(Hwnd(3))));

        // Cloaked by something that is not Mochi, with nothing written down.
        for info in platform.windows.lock().unwrap().iter_mut() {
            if info.hwnd == Hwnd(3) {
                info.cloaked = true;
            }
        }
        assert!(!wm.we_hid(Hwnd(3)), "the test cloaked it, not Mochi");

        wm.on_event(Event::Window {
            kind: WindowEventKind::Foreground,
            hwnd: Hwnd(1),
        });

        assert!(
            !wm.state().is_managed(window_id(Hwnd(3))),
            "a window that is off screen still holds a tile"
        );
        assert!(
            wm.state().is_managed(window_id(Hwnd(1)))
                && wm.state().is_managed(window_id(Hwnd(2))),
            "the windows that are still on screen were swept up too"
        );
    }

    #[test]
    fn a_window_mochi_hid_itself_keeps_its_place() {
        // The other side of the guard. Switching workspace takes every window
        // of the old one off screen on purpose, and those are written down.
        // Sweeping them up here would unmanage the whole workspace the moment
        // the user left it.
        let (mut wm, _platform) = manager(vec![window(1, "Editor"), window(2, "Browser")]);
        wm.handle_command(Command::FocusWorkspace { index: 1 });

        wm.on_event(Event::Window {
            kind: WindowEventKind::Foreground,
            hwnd: Hwnd(1),
        });

        assert!(
            wm.state().is_managed(window_id(Hwnd(1))),
            "a window Mochi took off screen itself was forgotten"
        );
    }

    #[test]
    fn the_keyboard_is_never_taken_off_a_window_mochi_does_not_manage() {
        // The rescue exists for a window Mochi moved out from under the user:
        // one it cloaked, or one left behind on a screen the user navigated
        // away from. A window Mochi does not manage is none of those. It was
        // never moved, it is still on screen, and the user is looking straight
        // at it, so taking the keyboard off it and handing it to the desktop
        // undoes a choice the user made and nothing else.
        //
        // Measured on a real desktop before this guard existed: with the
        // foreground on a game an ignore rule had skipped, an unrelated window
        // appearing on the other monitor sent the foreground to "Program
        // Manager", where it stayed. The game could not be typed into at all.
        let (mut wm, platform) = manager_on(
            vec![window(1, "Editor"), window(2, "Browser")],
            vec![main_screen(), portrait_screen()],
        );
        // A window of the user's that Mochi never took: an ignore rule skipped
        // it, or Windows refuses to let Mochi touch it.
        wm.foreground = Some(Hwnd(404));
        assert!(!wm.state().is_managed(window_id(Hwnd(404))));
        platform.desktop_focus.store(0, Ordering::SeqCst);

        wm.handle_command(Command::FocusMonitor { index: 1 });

        assert_eq!(
            platform.desktop_focus.load(Ordering::SeqCst),
            0,
            "the keyboard was taken off a window Mochi does not manage"
        );
        assert_eq!(
            wm.foreground,
            Some(Hwnd(404)),
            "the user's own window lost the keyboard"
        );
    }

    #[test]
    fn switching_to_a_workspace_that_has_windows_leaves_the_desktop_alone() {
        // The guard on the other side: an ordinary workspace switch names a
        // window, so the desktop must never be touched.
        let (mut wm, platform) = manager(vec![window(1, "Editor"), window(2, "Browser")]);
        wm.handle_command(Command::FocusWorkspace { index: 1 });
        platform.desktop_focus.store(0, Ordering::SeqCst);

        wm.handle_command(Command::FocusWorkspace { index: 0 });

        assert_eq!(
            platform.desktop_focus.load(Ordering::SeqCst),
            0,
            "the desktop was focused although the workspace has windows"
        );
        assert!(wm.foreground.is_some(), "it dropped the keyboard entirely");
    }

    #[test]
    fn crossing_to_a_screen_that_has_a_window_focuses_the_window_not_the_desktop() {
        // The other half of the same branch: when there IS something to focus,
        // the desktop must not be touched, or every monitor change would drop
        // the keyboard on the way past.
        //
        // The test has to start on the main screen: a window that is already
        // the foreground is deliberately not focused again, so crossing TO the
        // window under test would prove nothing.
        let mut over_there = window(7, "On the portrait screen");
        over_there.monitor = Some(MonitorId(2));
        let (mut wm, platform) = manager_on_focused_on(
            vec![over_there, window(1, "Editor"), window(2, "Browser")],
            vec![main_screen(), portrait_screen()],
            Hwnd(1),
        );
        platform.focused.lock().unwrap().clear();

        wm.handle_command(Command::FocusMonitor { index: 1 });

        assert_eq!(wm.state().focused_monitor_idx(), 1);
        assert_eq!(
            platform.desktop_focus.load(Ordering::SeqCst),
            0,
            "the desktop was focused although there was a window to focus"
        );
        assert_eq!(
            platform.focused.lock().unwrap().as_slice(),
            &[Hwnd(7)],
            "the window on the other screen was not focused"
        );
    }

    #[test]
    fn unplugging_a_screen_keeps_the_stacks_and_the_floating_windows() {
        let (mut wm, platform) = manager_on(
            vec![window(1, "Editor")],
            vec![main_screen(), portrait_screen()],
        );
        wm.handle_command(Command::FocusMonitor { index: 1 });

        // Two windows stacked together, and one floated by hand, on the screen
        // that is about to go away.
        for hwnd in [7, 8, 9] {
            let mut w = window(hwnd, "On the portrait screen");
            w.monitor = Some(MonitorId(2));
            wm.manage(&w);
        }
        wm.handle_command(Command::FocusStackWindow { index: 0 });
        wm.handle_command(Command::StackAll);
        assert_eq!(wm.state().workspace(1, 0).unwrap().containers().len(), 1);
        wm.handle_command(Command::ToggleFloat);
        assert_eq!(
            wm.state().workspace(1, 0).unwrap().floating_windows().len(),
            1
        );

        platform.set_monitors(vec![main_screen()]);
        wm.refresh_monitors();

        // Rehoming used to flatten the vanished screen window by window and
        // re-ask the rules about each one, so a stack of three came back as
        // three containers and a hand-floated window came back tiled, because
        // no rule ever said to float it.
        let survivor = wm.state().workspace(0, 0).unwrap();
        assert_eq!(
            survivor.floating_windows().len(),
            1,
            "the floating window came back tiled"
        );
        let biggest = survivor
            .containers()
            .iter()
            .map(mochi_core::model::Container::len)
            .max()
            .unwrap_or(0);
        assert_eq!(
            biggest, 2,
            "the stack was flattened into separate containers"
        );
    }

    #[test]
    fn a_window_rehomed_off_a_lost_screen_goes_off_the_air_with_its_workspace() {
        let (mut wm, platform) = manager_on(
            vec![window(1, "Editor")],
            vec![main_screen(), portrait_screen()],
        );
        // A window on the portrait screen, on a workspace index the surviving
        // screen is not looking at.
        wm.handle_command(Command::FocusMonitor { index: 1 });
        wm.handle_command(Command::FocusWorkspace { index: 2 });
        let mut stray = window(7, "On the portrait screen");
        // The helper hardcodes the main screen's id; this one really is over
        // on the portrait panel.
        stray.monitor = Some(MonitorId(2));
        wm.manage(&stray);
        assert!(wm.state().window(WindowId(7)).is_some());
        wm.handle_command(Command::FocusMonitor { index: 0 });
        wm.handle_command(Command::FocusWorkspace { index: 0 });

        let before = platform.cloaks.lock().unwrap().len();
        platform.set_monitors(vec![main_screen()]);
        wm.refresh_monitors();

        // It lands on a workspace nobody is looking at, so it has to go off
        // screen. The retile that follows a display change only ever shows, so
        // dropping the change set left it on screen over the layout while the
        // model called it hidden.
        let after: Vec<_> = platform.cloaks.lock().unwrap()[before..].to_vec();
        assert!(
            after.contains(&(Hwnd(7), true)),
            "the rehomed window was left on screen: {after:?}"
        );
    }

    #[test]
    fn a_screen_plugged_in_after_startup_gets_the_workspaces_its_entry_describes() {
        // One screen at startup, exactly as his desk was: the daemon comes up
        // with the 4K and the portrait panel still unplugged.
        let (mut wm, platform) = manager(vec![window(1, "Editor")]);
        let json = r#"{
            "monitors": [
                { "workspaces": [ {"name":"1"}, {"name":"2"}, {"name":"3"} ] },
                { "workspaces": [ {"name":"a","layout":"Columns"}, {"name":"b"} ] }
            ]
        }"#;
        wm.visual_config = serde_json::from_str(json).unwrap();
        wm.refresh_monitors();
        assert_eq!(wm.state().monitors().len(), 1);

        // The second panel arrives.
        platform.set_monitors(vec![main_screen(), portrait_screen()]);
        wm.refresh_monitors();
        assert_eq!(wm.state().monitors().len(), 2);

        let fresh = wm.state().monitors().get(1).unwrap();
        assert_eq!(
            fresh.workspaces().len(),
            2,
            "a screen attached after startup came up with its own defaults              instead of the entry that describes it"
        );
        assert_eq!(
            fresh.workspaces().get(0).unwrap().name.as_deref(),
            Some("a")
        );
        assert_eq!(
            fresh.workspaces().get(0).unwrap().layout,
            mochi_core::layout::Layout::Columns
        );
    }

    #[test]
    fn plugging_a_screen_in_leaves_the_other_screens_workspaces_alone() {
        let (mut wm, platform) = manager(vec![window(1, "Editor")]);
        let json = r#"{ "monitors": [ { "workspaces": [ {"name":"1","layout":"BSP"} ] },
                                      { "workspaces": [ {"name":"a"} ] } ] }"#;
        wm.visual_config = serde_json::from_str(json).unwrap();
        wm.refresh_monitors();

        // Something a command changed on the screen that is staying put.
        wm.handle_command(Command::ChangeLayout {
            layout: mochi_client::Layout::Columns,
        });
        assert_eq!(
            wm.state().workspace(0, 0).unwrap().layout,
            mochi_core::layout::Layout::Columns
        );

        platform.set_monitors(vec![main_screen(), portrait_screen()]);
        wm.refresh_monitors();

        // Configuring every monitor on a display change would have put this
        // back to BSP, which is the file's answer, not the one in force.
        assert_eq!(
            wm.state().workspace(0, 0).unwrap().layout,
            mochi_core::layout::Layout::Columns,
            "a display change reset a layout that a command had set"
        );
    }

    #[test]
    fn the_container_behaviour_can_be_switched_without_touching_the_file() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        assert_eq!(
            wm.state().window_container_behaviour,
            mochi_core::model::WindowContainerBehaviour::Create
        );

        let (response, _) = wm.handle_command(Command::ToggleWindowContainerBehaviour);
        assert_eq!(response, Response::Ok);
        assert_eq!(
            wm.state().window_container_behaviour,
            mochi_core::model::WindowContainerBehaviour::Append
        );

        // And it takes effect on the next window, not on the next reload.
        wm.manage(&window(2, "Browser"));
        assert_eq!(
            wm.state().workspace(0, 0).unwrap().containers().len(),
            1,
            "the new window should have joined the focused container"
        );

        wm.handle_command(Command::WindowContainerBehaviour {
            behaviour: mochi_client::ContainerBehaviour::Create,
        });
        wm.manage(&window(3, "Terminal"));
        assert_eq!(wm.state().workspace(0, 0).unwrap().containers().len(), 2);
    }

    #[test]
    fn a_maximize_the_user_made_is_followed_into_the_model() {
        // Nothing read `WindowInfo::maximized` anywhere in the daemon, so
        // pressing the maximize button left Windows with the window zoomed and
        // the model still calling it normally tiled. A zoomed window ignores
        // every rectangle `SetWindowPos` gives it, so it sat at full screen
        // refusing its tile, with nothing logged and no command able to explain
        // it. Caught on the real desktop by comparing `rect` with `actual_rect`
        // in `mochic state`.
        let (mut wm, platform) =
            manager_focused_on(vec![window(1, "Editor"), window(2, "Browser")], Hwnd(2));
        assert!(
            !wm.state().workspace(0, 0).unwrap().is_maximized(),
            "nothing is maximized to begin with"
        );

        platform.user_maximizes(Hwnd(2));
        wm.on_window_event(WindowEventKind::LocationChange, Hwnd(2));

        let workspace = wm.state().workspace(0, 0).unwrap();
        assert!(workspace.is_maximized(), "the model did not follow");
        assert_eq!(
            workspace.maximized_window().map(|w| w.id),
            Some(WindowId(2)),
            "it followed the wrong window"
        );

        // And back again when the user restores it.
        platform.user_restores(Hwnd(2));
        wm.on_window_event(WindowEventKind::LocationChange, Hwnd(2));
        assert!(
            !wm.state().workspace(0, 0).unwrap().is_maximized(),
            "the model stayed maximized after the user restored it"
        );
    }

    #[test]
    fn mochis_own_maximize_is_not_mistaken_for_the_users() {
        // The ledger's whole job. Mochi maximizing a window produces exactly
        // the same location change the user's button does; without telling them
        // apart, Mochi would read its own work as a user action and toggle
        // straight back off again.
        let (mut wm, platform) =
            manager_focused_on(vec![window(1, "Editor"), window(2, "Browser")], Hwnd(2));

        wm.handle_command(Command::ToggleMaximize);
        assert!(wm.state().workspace(0, 0).unwrap().is_maximized());

        // Windows now reports it zoomed, and the event arrives.
        platform.user_maximizes(Hwnd(2));
        wm.on_window_event(WindowEventKind::LocationChange, Hwnd(2));

        assert!(
            wm.state().workspace(0, 0).unwrap().is_maximized(),
            "Mochi followed its own maximize and toggled it back off"
        );
    }

    #[test]
    fn a_window_that_starts_refusing_loses_its_tile() {
        // What an elevated window looks like to a Mochi that is not elevated:
        // it can be read and enumerated, so it is managed like anything else,
        // and only the first attempt to move it discovers that Windows will
        // not allow it. Keeping it in the layout hands it a tile it can never
        // be put in, so the tile stays empty, its border is drawn around
        // nothing and the other window is squeezed into half a screen for it.
        let (mut wm, platform) = manager(vec![window(1, "Editor"), window(2, "Terminal")]);
        let shared = wm.state().workspace(0, 0).unwrap();
        assert_eq!(shared.containers().len(), 2);
        let shared_width = shared.latest_layout()[0].width();

        platform.refuse(Hwnd(2));
        wm.retile();

        // The next thing that happens is when the model catches up.
        wm.on_event(Event::Window {
            kind: WindowEventKind::Foreground,
            hwnd: Hwnd(1),
        });

        let workspace = wm.state().workspace(0, 0).unwrap();
        assert_eq!(
            workspace.containers().len(),
            1,
            "the window Windows refuses to move still had a tile"
        );
        assert!(
            workspace.latest_layout()[0].width() > shared_width,
            "the window that is left should have the space back, not keep sharing              the screen with a tile nothing can be put in"
        );
    }

    #[test]
    fn refusing_commands_for_an_unmanaged_window_actually_refuses_them() {
        let (mut wm, platform) = manager(vec![window(1, "Editor")]);
        wm.handle_command(Command::UnmanagedWindowOperationBehaviour {
            behaviour: mochi_client::OperationBehaviour::NoOp,
        });

        // A window Mochi does not manage has the foreground.
        let mut stray = window(9, "A dialog");
        stray.ex_style = crate::platform::types::ex_style::WS_EX_TOOLWINDOW;
        platform.windows.lock().unwrap().push(stray);
        wm.on_window_event(WindowEventKind::Foreground, Hwnd(9));
        assert!(!wm.foreground_is_managed());

        // The setting was stored, reported by `mochic state` and read by
        // nothing, so `no-op` behaved exactly like `op`.
        let (response, _) = wm.handle_command(Command::Move {
            direction: mochi_client::Direction::Left,
        });
        assert!(
            response.error_message().is_some(),
            "a command aimed at an unmanaged window ran under no-op"
        );

        // A command that names its own target is not aimed at a window and is
        // never refused.
        let (response, _) = wm.handle_command(Command::FocusWorkspace { index: 1 });
        assert_eq!(response, Response::Ok);

        // And under the default the same command runs.
        wm.handle_command(Command::UnmanagedWindowOperationBehaviour {
            behaviour: mochi_client::OperationBehaviour::Op,
        });
        let (response, _) = wm.handle_command(Command::Move {
            direction: mochi_client::Direction::Left,
        });
        assert_eq!(response, Response::Ok);
    }

    #[test]
    fn the_three_other_behaviours_reach_the_model() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);

        wm.handle_command(Command::CrossMonitorMoveBehaviour {
            behaviour: mochi_client::MoveBehaviour::Insert,
        });
        wm.handle_command(Command::WindowHidingBehaviour {
            behaviour: mochi_client::HidingBehaviour::Minimize,
        });
        wm.handle_command(Command::UnmanagedWindowOperationBehaviour {
            behaviour: mochi_client::OperationBehaviour::NoOp,
        });

        assert_eq!(
            wm.state().cross_monitor_move_behaviour,
            mochi_core::model::MoveBehaviour::Insert
        );
        assert_eq!(
            wm.state().window_hiding_behaviour,
            mochi_core::model::HidingBehaviour::Minimize
        );
        assert_eq!(
            wm.state().unmanaged_window_operation_behaviour,
            mochi_core::model::OperationBehaviour::NoOp
        );
    }

    #[test]
    fn a_manage_rule_added_at_runtime_adopts_the_window_it_describes() {
        // A tool window: skipped by the heuristics, and skipped for a reason a
        // manage rule is allowed to overrule.
        let mut tool = window(3, "The palette");
        tool.exe = "Palette.exe".into();
        tool.ex_style = crate::platform::types::ex_style::WS_EX_TOOLWINDOW;

        let (mut wm, _) = manager(vec![window(1, "Editor"), tool]);
        assert_eq!(wm.state().all_window_ids().count(), 1, "skipped at startup");

        let (response, _) = wm.handle_command(Command::ManageRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "Palette.exe".into(),
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        assert_eq!(response, Response::Ok);

        // The window that was already on the desktop, not merely the next one
        // the application opens.
        assert_eq!(wm.state().all_window_ids().count(), 2);
        assert!(wm.state().window(WindowId(3)).is_some());
    }

    #[test]
    fn a_manage_rule_is_not_quietly_filed_as_a_float_rule() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        wm.handle_command(Command::ManageRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "Palette.exe".into(),
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        assert_eq!(wm.state().rules.manage_rules.len(), 1);
        assert!(
            wm.state().rules.floating_applications.is_empty(),
            "a manage rule that lands in the floating list would float every              window it was meant to rescue"
        );
    }

    #[test]
    fn an_ignore_rule_typed_at_the_keyboard_outranks_a_later_manage_rule() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.handle_command(Command::IgnoreRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "Code.exe".into(),
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        wm.handle_command(Command::ManageRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "Code.exe".into(),
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        // The user's own word is the last one, whichever order the two arrive
        // in. Otherwise a rule file could cancel what they typed themselves.
        assert_eq!(wm.state().all_window_ids().count(), 0);
    }

    #[test]
    fn a_workspace_rule_pointed_at_a_screen_that_is_not_there_is_refused() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        let (response, _) = wm.handle_command(Command::WorkspaceRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "Code.exe".into(),
            monitor: 4,
            workspace: 0,
            initial_only: false,
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        assert!(
            response.error_message().is_some(),
            "a rule that routes nowhere would only show up as a window that              never opens where it was told to"
        );
        assert!(wm.workspace_rules.is_empty());
    }

    #[test]
    fn a_workspace_rule_added_at_runtime_routes_the_next_window() {
        let (mut wm, _) = manager(vec![window(1, "Editor")]);
        let (response, _) = wm.handle_command(Command::WorkspaceRule {
            identifier: mochi_client::RuleIdentifier::Exe,
            id: "Mail.exe".into(),
            monitor: 0,
            workspace: 3,
            initial_only: false,
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        assert_eq!(response, Response::Ok);

        let mut mail = window(7, "Inbox");
        mail.exe = "Mail.exe".into();
        wm.manage(&mail);

        assert_eq!(wm.state().workspace(0, 3).unwrap().containers().len(), 1);
    }

    #[test]
    fn restore_windows_gives_back_a_window_nothing_accounts_for() {
        let (mut wm, platform) = manager(vec![window(1, "Editor")]);

        // A window off screen with no entry in the model to explain it: the
        // shape of every bug that loses one, whatever the cause was.
        wm.hidden
            .lock()
            .unwrap()
            .hide(Hwnd(99), HidingBehaviour::Cloak);

        let (response, _) = wm.handle_command(Command::RestoreWindows);
        assert_eq!(response, Response::Ok);
        assert!(
            platform.cloaks.lock().unwrap().contains(&(Hwnd(99), false)),
            "the stranded window was left off screen"
        );
        assert!(!wm.hidden.lock().unwrap().contains(Hwnd(99)));
    }

    #[test]
    fn restore_windows_leaves_a_hidden_workspace_where_it_is() {
        let (mut wm, platform) = manager(vec![window(1, "Editor"), window(2, "Browser")]);
        // Both windows are on workspace 0; moving to another workspace hides
        // them legitimately, and the record says so.
        wm.handle_command(Command::FocusWorkspace { index: 1 });
        assert!(wm.hidden.lock().unwrap().contains(Hwnd(1)));

        let before = platform.cloaks.lock().unwrap().len();
        let (response, _) = wm.handle_command(Command::RestoreWindows);
        assert_eq!(response, Response::Ok);
        assert_eq!(
            platform.cloaks.lock().unwrap().len(),
            before,
            "a rescue that drags every hidden workspace back on screen is not              a rescue"
        );
        assert!(wm.hidden.lock().unwrap().contains(Hwnd(1)));
    }

    #[test]
    fn stacking_the_whole_workspace_shows_one_window_and_unstacking_gives_them_back() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two"), window(3, "Three")]);
        let (response, _) = wm.handle_command(Command::StackAll);
        assert_eq!(response, Response::Ok);
        assert_eq!(wm.state().workspace(0, 0).unwrap().containers().len(), 1);

        let (response, _) = wm.handle_command(Command::UnstackAll);
        assert_eq!(response, Response::Ok);
        assert_eq!(wm.state().workspace(0, 0).unwrap().containers().len(), 3);
    }

    #[test]
    fn a_float_rule_added_at_runtime_takes_the_window_out_of_the_layout() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.handle_command(Command::FloatRule {
            identifier: mochi_client::RuleIdentifier::Title,
            id: "Two".into(),
            matching_strategy: mochi_client::MatchingStrategy::Equals,
        });
        let workspace = wm.state().workspace(0, 0).unwrap();
        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(workspace.floating_windows().len(), 1);
    }

    #[test]
    fn padding_commands_reach_the_workspace_and_retile() {
        let (mut wm, platform) = manager(vec![window(1, "One")]);
        let before = platform.last_placements()[0].1;
        wm.handle_command(Command::WorkspacePadding {
            monitor: 0,
            workspace: 0,
            size: 100,
        });
        let after = platform.last_placements()[0].1;
        assert_eq!(
            wm.state().workspace(0, 0).unwrap().workspace_padding,
            Some(100)
        );
        assert_eq!(Some(after), wm.state().rect_for_window(WindowId(1)));
        assert!(after.width() < before.width(), "{before:?} then {after:?}");
        assert!(after.left > before.left, "{before:?} then {after:?}");
        assert!(
            !wm.handle_command(Command::WorkspacePadding {
                monitor: 9,
                workspace: 0,
                size: 10,
            })
            .0
            .is_ok()
        );
    }

    #[test]
    fn unmanage_drops_the_foreground_window_and_manage_takes_it_back() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.foreground = Some(Hwnd(1));

        assert_eq!(wm.handle_command(Command::Unmanage).0, Response::Ok);
        assert_eq!(wm.state().all_window_ids().count(), 1);
        assert!(!wm.state().is_managed(WindowId(1)));

        // Dropping a window moves the focus, so point `manage` back at it.
        wm.foreground = Some(Hwnd(1));
        assert_eq!(wm.handle_command(Command::Manage).0, Response::Ok);
        assert_eq!(wm.state().all_window_ids().count(), 2);
    }

    #[test]
    fn the_restore_hook_puts_back_everything_that_was_hidden() {
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.handle_command(Command::FocusWorkspace { index: 1 });
        assert_eq!(wm.hidden().lock().unwrap().len(), 2);

        platform.cloaks.lock().unwrap().clear();
        // The body of the hook, without touching the process-wide slot that
        // the safety tests own.
        restore(platform.as_ref(), &wm.hidden());

        let uncloaked: Vec<_> = platform.cloaks.lock().unwrap().clone();
        assert_eq!(uncloaked.len(), 2);
        assert!(uncloaked.iter().all(|(_, on)| !*on));
        assert!(wm.hidden().lock().unwrap().is_empty());
        // Idempotent: a second pass has nothing left to do.
        restore(platform.as_ref(), &wm.hidden());
        assert_eq!(platform.cloaks.lock().unwrap().len(), 2);
    }

    #[test]
    fn hiding_with_minimize_restores_with_a_show_call() {
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.core.window_hiding_behaviour = HidingBehaviour::Minimize;
        wm.handle_command(Command::FocusWorkspace { index: 1 });
        assert!(
            platform
                .shows
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, state)| *state == ShowState::Minimize)
                .count()
                >= 2
        );

        wm.handle_command(Command::FocusWorkspace { index: 0 });
        assert!(
            platform
                .shows
                .lock()
                .unwrap()
                .iter()
                .any(|(_, state)| *state == ShowState::Restore)
        );
    }

    #[test]
    fn boolean_is_the_command_line_spelling() {
        assert!(boolean(BooleanState::Enable));
        assert!(!boolean(BooleanState::Disable));
    }

    /// A window that is cloaked when the daemon starts.
    fn cloaked_window(hwnd: isize, title: &str, class: &str) -> WindowInfo {
        WindowInfo {
            class: class.into(),
            cloaked: true,
            ..window(hwnd, title)
        }
    }

    #[test]
    fn a_cloaked_window_a_dead_session_left_behind_is_given_back() {
        let (_wm, platform) = manager(vec![cloaked_window(1, "Editor", "Chrome_WidgetWin_1")]);
        assert!(
            platform.cloaks.lock().unwrap().contains(&(Hwnd(1), false)),
            "a cloaked ordinary window is the signature of a killed session"
        );
    }

    #[test]
    fn a_suspended_uwp_window_is_left_cloaked() {
        let (_wm, platform) = manager(vec![cloaked_window(1, "Settings", FRAME_WINDOW_CLASS)]);
        assert!(
            platform.cloaks.lock().unwrap().is_empty(),
            "Windows cloaks suspended UWP apps itself, uncloaking them puts closed apps back on screen"
        );
    }

    #[test]
    fn a_window_that_cannot_be_moved_does_not_stop_the_others() {
        let (mut wm, platform) = manager(vec![
            window(1, "One"),
            window(2, "Elevated"),
            window(3, "Three"),
        ]);
        platform.refuse(Hwnd(2));
        platform.clear_history();

        wm.handle_command(Command::Retile);

        assert_eq!(
            wm.state().all_window_ids().count(),
            3,
            "a window Mochi cannot move is still a window it manages"
        );
        assert!(
            platform.rect_of(Hwnd(1)).is_some() && platform.rect_of(Hwnd(3)).is_some(),
            "the windows that can move still moved"
        );
        assert!(
            platform.rect_of(Hwnd(2)).is_none(),
            "the refused window was reported, not moved"
        );
    }

    #[test]
    fn a_window_moved_to_the_other_screen_is_placed_on_it() {
        let (mut wm, platform) = manager_on(
            vec![window(1, "One"), window(2, "Two")],
            vec![main_screen(), portrait_screen()],
        );
        assert_eq!(wm.state().monitors().len(), 2);
        let moved = wm
            .state()
            .focused_window_id()
            .map(handle)
            .expect("nothing is focused");

        assert_eq!(
            wm.handle_command(Command::MoveToMonitor { index: 1 }).0,
            Response::Ok
        );

        assert_eq!(wm.state().workspace(1, 0).unwrap().containers().len(), 1);
        assert_eq!(wm.state().workspace(0, 0).unwrap().containers().len(), 1);
        assert_eq!(wm.state().focused_monitor_idx(), 1, "the focus follows");
        let rect = platform
            .rect_of(moved)
            .expect("the window was never placed");
        assert!(
            portrait_screen().work_area.contains_rect(&rect),
            "{rect:?} is not inside the portrait screen"
        );
    }

    #[test]
    fn focus_and_cycle_walk_the_monitor_ring() {
        let (mut wm, _) = manager_on(
            vec![window(1, "One")],
            vec![main_screen(), portrait_screen()],
        );
        assert_eq!(wm.state().focused_monitor_idx(), 0);

        wm.handle_command(Command::FocusMonitor { index: 1 });
        assert_eq!(wm.state().focused_monitor_idx(), 1);

        wm.handle_command(Command::CycleMonitor {
            direction: mochi_client::CycleDirection::Next,
        });
        assert_eq!(wm.state().focused_monitor_idx(), 0, "the ring wraps");
    }

    #[test]
    fn a_screen_that_is_unplugged_hands_its_windows_back_and_retiles_them() {
        let (mut wm, platform) = manager_on(
            vec![window(1, "One"), window(2, "Two")],
            vec![main_screen(), portrait_screen()],
        );
        let moved = wm
            .state()
            .focused_window_id()
            .map(handle)
            .expect("nothing is focused");
        wm.handle_command(Command::MoveToMonitor { index: 1 });

        platform.set_monitors(vec![main_screen()]);
        wm.on_monitor_event(MonitorEventKind::DisplayChange);

        assert_eq!(wm.state().monitors().len(), 1);
        assert_eq!(
            wm.state().all_window_ids().count(),
            2,
            "a window may never disappear with the screen it was on"
        );
        assert_eq!(wm.state().workspace(0, 0).unwrap().containers().len(), 2);
        let rect = platform
            .rect_of(moved)
            .expect("the window was never replaced");
        assert!(
            main_screen().work_area.contains_rect(&rect),
            "{rect:?} is off the only screen that is left"
        );
    }

    #[test]
    fn an_empty_enumeration_does_not_lose_the_windows() {
        let (mut wm, platform) = manager_on(
            vec![window(1, "One"), window(2, "Two")],
            vec![main_screen(), portrait_screen()],
        );

        // A mode switch can make every handle fail to read, which the platform
        // reports as an empty list rather than an error.
        platform.set_monitors(vec![]);
        wm.on_monitor_event(MonitorEventKind::DisplayChange);

        assert_eq!(
            wm.state().monitors().len(),
            2,
            "the monitor ring was torn down on an empty enumeration"
        );
        assert_eq!(
            wm.state().all_window_ids().count(),
            2,
            "the managed windows left the model and could never be shown again"
        );
    }

    #[test]
    fn a_screen_that_comes_back_can_be_moved_to_again() {
        let (mut wm, platform) = manager_on(
            vec![window(1, "One"), window(2, "Two")],
            vec![main_screen(), portrait_screen()],
        );
        platform.set_monitors(vec![main_screen()]);
        wm.on_monitor_event(MonitorEventKind::DisplayChange);
        assert_eq!(wm.state().monitors().len(), 1);

        platform.set_monitors(vec![main_screen(), portrait_screen()]);
        wm.on_monitor_event(MonitorEventKind::DisplayChange);
        assert_eq!(wm.state().monitors().len(), 2);
        assert_eq!(
            wm.state().all_window_ids().count(),
            2,
            "the windows stay where the unplug left them"
        );

        let moved = wm
            .state()
            .focused_window_id()
            .map(handle)
            .expect("nothing is focused");
        assert_eq!(
            wm.handle_command(Command::MoveToMonitor { index: 1 }).0,
            Response::Ok
        );
        let rect = platform
            .rect_of(moved)
            .expect("the window was never placed");
        assert!(
            portrait_screen().work_area.contains_rect(&rect),
            "{rect:?} is not inside the screen that came back"
        );
    }

    #[test]
    fn a_renegotiation_that_renames_the_displays_leaves_the_workspaces_on_their_panels() {
        let (mut wm, platform) = manager_on(
            vec![window(1, "One"), window(2, "Two")],
            vec![main_screen(), portrait_screen()],
        );
        let moved = wm.state().focused_window_id().expect("nothing is focused");
        wm.handle_command(Command::MoveToMonitor { index: 1 });
        wm.handle_command(Command::FocusMonitor { index: 1 });
        assert!(
            wm.state()
                .workspace(1, 0)
                .unwrap()
                .all_windows()
                .any(|w| w.id == moved),
            "the window never reached the portrait panel"
        );

        // The DisplayPort renegotiation: the same two panels, plugged into the
        // same ports, come back with the GDI name and the handle the other one
        // had. Nothing about either physical screen changed.
        let mut portrait_first = portrait_screen();
        portrait_first.device_name = r"\\.\DISPLAY1".into();
        portrait_first.id = MonitorId(1);
        let mut main_second = main_screen();
        main_second.device_name = r"\\.\DISPLAY2".into();
        main_second.id = MonitorId(2);
        platform.set_monitors(vec![portrait_first, main_second]);
        wm.on_monitor_event(MonitorEventKind::DisplayChange);

        let panel = wm
            .state()
            .monitors()
            .position(|m| m.size == portrait_screen().size)
            .expect("the portrait panel is gone");
        assert!(
            wm.state()
                .workspace(panel, 0)
                .unwrap()
                .all_windows()
                .any(|w| w.id == moved),
            "the workspaces followed the GDI device name onto the other screen"
        );
        assert_eq!(
            wm.state().focused_monitor_idx(),
            panel,
            "the focus followed the name rather than the screen it was on"
        );
    }

    #[test]
    fn a_reload_keeps_the_visuals_a_command_turned_on() {
        let (mut wm, _) = manager(vec![window(1, "One")]);
        wm.handle_command(Command::ToggleTransparency);
        wm.handle_command(Command::AnimationDuration { duration: 80 });
        assert!(wm.session.settings.transparency);

        // The configuration file at this path does not exist, so it carries no
        // visual key at all: everything the two commands set has to survive it.
        wm.load_config().expect("a missing file is not an error");

        assert!(
            wm.session.settings.transparency,
            "`mochic state` stopped reporting it"
        );
        assert_eq!(
            wm.visual_config.transparency,
            Some(true),
            "the managers were rebuilt without the transparency that is on"
        );
        assert_eq!(wm.session.settings.animation_duration, 80);
        assert_eq!(
            wm.visual_config.animation.unwrap_or_default().duration,
            Some(80),
            "the animator was rebuilt with the default duration"
        );

        // The next toggle has to turn it off rather than be swallowed by a
        // stale reading of a setting the reload had already switched off.
        wm.handle_command(Command::ToggleTransparency);
        assert!(!wm.session.settings.transparency);
        assert_eq!(wm.visual_config.transparency, Some(false));
    }

    #[test]
    fn a_change_layout_to_the_layout_it_already_had_announces_nothing() {
        let (mut wm, _) = manager(vec![window(1, "One")]);
        wm.handle_command(Command::ChangeLayout {
            layout: mochi_client::Layout::Rows,
        });
        assert_eq!(
            wm.layout_changes().len(),
            1,
            "a real layout change has to go out"
        );

        wm.handle_command(Command::ChangeLayout {
            layout: mochi_client::Layout::Rows,
        });
        assert_eq!(
            wm.layout_changes().len(),
            1,
            "a layout that did not change was announced to every subscriber"
        );
    }

    #[test]
    fn a_visual_command_lands_in_the_configuration_the_managers_read() {
        let (mut wm, _) = manager(vec![window(1, "One")]);
        for command in [
            Command::BorderWidth { width: 12 },
            Command::BorderOffset { offset: -3 },
            Command::BorderStyle {
                style: mochi_client::BorderStyle::Rounded,
            },
            Command::BorderColour {
                kind: mochi_client::WindowKind::Single,
                r: 255,
                g: 187,
                b: 223,
            },
        ] {
            assert_eq!(wm.handle_command(command).0, Response::Ok);
        }

        assert_eq!(wm.visual_config.border_width, Some(12));
        assert_eq!(wm.visual_config.border_offset, Some(-3));
        assert_eq!(
            wm.visual_config.border_style,
            Some(mochi_core::config::BorderStyle::Rounded)
        );
        assert_eq!(
            wm.visual_config.border_colours.and_then(|c| c.single),
            Some(Colour::new(255, 187, 223)),
            "a colour used to be logged and thrown away"
        );
    }

    #[test]
    fn mochic_state_reports_a_visual_change_without_a_reload() {
        let (mut wm, _) = manager(vec![window(1, "One")]);
        wm.handle_command(Command::BorderWidth { width: 9 });
        wm.handle_command(Command::BorderColour {
            kind: mochi_client::WindowKind::Unfocused,
            r: 49,
            g: 50,
            b: 68,
        });
        wm.handle_command(Command::AnimationStyle {
            style: mochi_client::AnimationStyle::EaseOutQuad,
        });

        let settings = &wm.session().settings;
        assert_eq!(settings.border_width, 9);
        assert_eq!(
            settings.border_colours.unfocused,
            Some(Colour::new(49, 50, 68))
        );
        assert_eq!(
            settings.animation_style,
            mochi_client::AnimationStyle::EaseOutQuad
        );
    }

    #[test]
    fn turning_animation_on_starts_the_animator_with_the_settings_given() {
        let (mut wm, _) = manager(vec![window(1, "One")]);
        assert!(!wm.visuals.is_animating());

        wm.handle_command(Command::AnimationDuration { duration: 80 });
        wm.handle_command(Command::AnimationFps { fps: 30 });
        wm.handle_command(Command::Animation {
            state: BooleanState::Enable,
        });

        assert!(
            wm.visuals.is_animating(),
            "the animator runs as soon as the command lands, not after a reload"
        );
        assert_eq!(wm.visuals.animation().duration, Some(80));
        assert_eq!(wm.visuals.animation().fps, Some(30));

        wm.handle_command(Command::Animation {
            state: BooleanState::Disable,
        });
        assert!(!wm.visuals.is_animating());
    }

    #[test]
    fn toggling_transparency_twice_ends_where_it_started() {
        let (mut wm, _) = manager(vec![window(1, "One")]);
        wm.handle_command(Command::ToggleTransparency);
        assert!(wm.session().settings.transparency);
        assert_eq!(wm.visual_config.transparency, Some(true));

        wm.handle_command(Command::ToggleTransparency);
        assert!(!wm.session().settings.transparency);
        assert_eq!(wm.visual_config.transparency, Some(false));
    }

    #[test]
    fn a_workspace_switch_that_minimizes_keeps_owing_every_window_back() {
        // With `window_hiding_behaviour: minimize` Mochi's own minimize comes
        // back as a MinimizeStart event, exactly the way its cloak comes back
        // as a Cloaked event. Acting on it unmanages the whole workspace that
        // was just hidden, and `unmanage` clears the hidden record too, so
        // nothing is left that knows those windows are off screen.
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.core.window_hiding_behaviour = HidingBehaviour::Minimize;
        wm.handle_command(Command::FocusWorkspace { index: 1 });

        wm.on_window_event(WindowEventKind::MinimizeStart, Hwnd(1));
        wm.on_window_event(WindowEventKind::MinimizeStart, Hwnd(2));
        assert_eq!(
            wm.state().all_window_ids().count(),
            2,
            "a workspace switch unmanaged the windows it had just hidden"
        );
        assert_eq!(
            wm.hidden().lock().unwrap().len(),
            2,
            "mochi forgot it owes these windows back, so nothing will restore them"
        );

        platform.shows.lock().unwrap().clear();
        wm.handle_command(Command::FocusWorkspace { index: 0 });
        let restored: Vec<_> = platform
            .shows
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, state)| *state == ShowState::Restore)
            .map(|(hwnd, _)| *hwnd)
            .collect();
        assert!(
            restored.contains(&Hwnd(1)) && restored.contains(&Hwnd(2)),
            "the windows stayed minimized on the way back: {restored:?}"
        );
    }

    #[test]
    fn a_destroyed_window_stops_being_the_foreground() {
        // Windows hands the handle of a dead window to the next one, and
        // `focus_hwnd` refuses a handle it believes already has the focus.
        let (mut wm, platform) = manager(vec![window(1, "One")]);
        wm.on_window_event(WindowEventKind::Foreground, Hwnd(1));
        assert_eq!(wm.foreground, Some(Hwnd(1)));

        wm.on_window_event(WindowEventKind::Destroyed, Hwnd(1));
        assert_eq!(
            wm.foreground, None,
            "a destroyed handle was left as the foreground"
        );

        platform.focused.lock().unwrap().clear();
        wm.focus_hwnd(Hwnd(1));
        assert_eq!(
            platform.focused.lock().unwrap().as_slice(),
            [Hwnd(1)],
            "a new window on the recycled handle was never focused"
        );
    }

    #[test]
    fn a_window_that_only_went_off_screen_keeps_its_place_in_the_routing_ledger() {
        // `initial_workspace_rules` apply the first time a window is seen. A
        // user minimize, a virtual desktop cloak and a rule change all reach
        // `unmanage` with the window still alive, and forgetting the routing
        // there teleports the window back to its initial workspace.
        let (mut wm, _) = manager(vec![window(1, "One")]);
        assert!(wm.routed.contains(&Hwnd(1)));

        wm.on_window_event(WindowEventKind::MinimizeStart, Hwnd(1));
        assert!(
            wm.routed.contains(&Hwnd(1)),
            "a minimized window would be routed by the initial rules all over again"
        );

        wm.on_window_event(WindowEventKind::MinimizeEnd, Hwnd(1));
        wm.on_window_event(WindowEventKind::Destroyed, Hwnd(1));
        assert!(
            !wm.routed.contains(&Hwnd(1)),
            "a dead handle stayed in the ledger, so the window that inherits it              is treated as one that has already been routed"
        );
    }

    #[test]
    fn an_initial_workspace_rule_does_not_fire_again_after_a_minimize() {
        let (mut wm, _) = manager(vec![window(1, "One")]);
        wm.workspace_rules.push(WorkspaceRule {
            monitor: 0,
            workspace: 0,
            rule: MatchingRule::simple(
                ApplicationIdentifier::Title,
                "One".to_owned(),
                MatchingStrategy::Equals,
            ),
            initial_only: true,
        });
        // The user moves it somewhere else.
        wm.handle_command(Command::MoveToWorkspace { index: 1 });
        assert_eq!(wm.state().locate_window(WindowId(1)), Some((0, 1)));

        wm.on_window_event(WindowEventKind::MinimizeStart, Hwnd(1));
        wm.on_window_event(WindowEventKind::MinimizeEnd, Hwnd(1));
        assert_eq!(
            wm.state().locate_window(WindowId(1)),
            Some((0, 1)),
            "the window teleported back to its initial workspace on a restore"
        );
    }

    /// A rule that matches one window by its exact title.
    fn titled(title: &str) -> MatchingRule {
        MatchingRule::simple(
            ApplicationIdentifier::Title,
            title.to_owned(),
            MatchingStrategy::Equals,
        )
    }

    #[test]
    fn an_application_that_closes_to_the_tray_keeps_its_place_in_the_ledgers() {
        // An application in `tray_and_multi_window_applications` does not die
        // when its window goes away: the handle is still a window and the same
        // one comes back out of the tray. Treated as an ordinary destroy it
        // leaves the routing ledger, and `initial_workspace_rules` then route
        // it a second time, onto a workspace the user had moved it off.
        let (mut wm, _) = manager(vec![window(1, "Tray")]);
        wm.core
            .rules
            .tray_and_multi_window_applications
            .push(titled("Tray"));
        wm.workspace_rules.push(WorkspaceRule {
            monitor: 0,
            workspace: 0,
            rule: titled("Tray"),
            initial_only: true,
        });
        wm.handle_command(Command::MoveToWorkspace { index: 1 });
        assert_eq!(wm.state().locate_window(WindowId(1)), Some((0, 1)));

        // Closed to the tray: the window is gone from the screen, the handle
        // is not gone from the desktop.
        wm.on_window_event(WindowEventKind::Destroyed, Hwnd(1));
        assert_eq!(
            wm.state().all_window_ids().count(),
            0,
            "a window that is not on screen has no business in the layout"
        );
        assert!(
            wm.routed.contains(&Hwnd(1)),
            "the window was forgotten while it sat in the tray"
        );

        // Opened again from the tray.
        wm.on_window_event(WindowEventKind::Shown, Hwnd(1));
        assert_eq!(
            wm.state().locate_window(WindowId(1)),
            Some((0, 1)),
            "the window came back on its initial workspace instead of the one \
             the user had left it on"
        );
    }

    #[test]
    fn a_window_that_really_is_gone_still_leaves_the_ledger() {
        // The other half of the same rule: quitting a tray application for
        // good destroys its window like anything else, and the handle Windows
        // hands out next must be routed on its own account.
        let (mut wm, platform) = manager(vec![window(1, "Tray")]);
        wm.core
            .rules
            .tray_and_multi_window_applications
            .push(titled("Tray"));
        platform.windows.lock().unwrap().clear();

        wm.on_window_event(WindowEventKind::Destroyed, Hwnd(1));
        assert!(
            !wm.routed.contains(&Hwnd(1)),
            "a dead handle stayed in the ledger because the application once \
             had a tray icon"
        );
    }

    #[test]
    fn a_reused_window_is_routed_again_when_its_contents_change() {
        // An application in `object_name_change_applications` opens a document
        // in the window it already has. Nothing is created, so the workspace
        // routing is never asked, and the rule the user wrote for that
        // document never applies.
        let (mut wm, platform) = manager(vec![window(1, "Inbox")]);
        // The workspace the rule points at, the way a configuration file with
        // a `workspace_rules` entry in it would have made it.
        wm.core
            .monitors_mut()
            .get_mut(0)
            .unwrap()
            .ensure_workspaces(2);
        wm.core
            .rules
            .object_name_change_applications
            .push(MatchingRule::simple(
                ApplicationIdentifier::Exe,
                "Code.exe".to_owned(),
                MatchingStrategy::Equals,
            ));
        wm.workspace_rules.push(WorkspaceRule {
            monitor: 0,
            workspace: 1,
            rule: titled("Report"),
            initial_only: false,
        });
        assert_eq!(wm.state().locate_window(WindowId(1)), Some((0, 0)));

        platform.windows.lock().unwrap()[0].title = "Report".into();
        wm.on_window_event(WindowEventKind::NameChange, Hwnd(1));
        assert_eq!(
            wm.state().locate_window(WindowId(1)),
            Some((0, 1)),
            "the new contents never reached the workspace rule written for them"
        );
        assert_eq!(wm.state().window(WindowId(1)).unwrap().title, "Report");
        assert!(
            wm.hidden().lock().unwrap().contains(Hwnd(1)),
            "the window was moved to a workspace nobody is looking at and left              on screen, and nothing knows it is owed back"
        );
    }

    #[test]
    fn a_window_that_was_not_named_a_reuser_is_left_where_it_is() {
        // The same rename without the rule. A title change is an everyday
        // event and must not move anybody's window on its own.
        let (mut wm, platform) = manager(vec![window(1, "Inbox")]);
        wm.core
            .monitors_mut()
            .get_mut(0)
            .unwrap()
            .ensure_workspaces(2);
        wm.workspace_rules.push(WorkspaceRule {
            monitor: 0,
            workspace: 1,
            rule: titled("Report"),
            initial_only: false,
        });

        platform.windows.lock().unwrap()[0].title = "Report".into();
        wm.on_window_event(WindowEventKind::NameChange, Hwnd(1));
        assert_eq!(
            wm.state().locate_window(WindowId(1)),
            Some((0, 0)),
            "a plain title change moved a window to another workspace"
        );
    }

    #[test]
    fn a_slow_application_is_given_a_second_layout_pass() {
        // A window in `slow_application_identifiers` is not ready to be placed
        // at the moment it appears, so the first placement is the one it
        // resizes itself out of. The loop owns the state and cannot wait for
        // it; the second pass has to come back through the channel.
        let (mut wm, platform) = manager(vec![window(1, "One")]);
        wm.core
            .rules
            .slow_application_identifiers
            .push(titled("Slow"));

        platform.windows.lock().unwrap().push(window(2, "Slow"));
        wm.on_window_event(WindowEventKind::Created, Hwnd(2));
        assert_eq!(wm.state().all_window_ids().count(), 2);

        let deferred = wm
            .rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a slow application was tiled once and never again");
        platform.clear_history();
        wm.on_event(deferred);
        assert_eq!(
            platform.rect_of(Hwnd(2)),
            wm.state().rect_for_window(WindowId(2)),
            "the deferred pass did not put the slow window in its tile"
        );
    }

    #[test]
    fn an_ordinary_application_is_not_tiled_twice() {
        let (wm, _) = manager(vec![window(1, "One")]);
        assert!(
            wm.rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "every window is now paying for the slow ones"
        );
    }

    #[test]
    fn a_floating_windows_border_follows_the_frame_the_user_sees() {
        // `GetWindowRect` includes the invisible resize border, so a border
        // drawn around it sits several pixels off on every edge.
        let mut floating = window(1, "Floating");
        floating.rect = Rect::new(0, 0, 800, 600);
        floating.frame = Rect::new(7, 0, 793, 593);
        let (mut wm, _) = manager(vec![floating, window(2, "Tiled")]);
        wm.on_window_event(WindowEventKind::Foreground, Hwnd(1));
        wm.handle_command(Command::ToggleFloat);

        let targets = wm.visuals_targets();
        let (_, rect, _) = targets
            .tiled
            .iter()
            .find(|(hwnd, _, _)| *hwnd == Hwnd(1))
            .expect("the floating window got no border at all");
        assert_eq!(
            *rect,
            Rect::new(7, 0, 793, 593),
            "the border was drawn around the window rect, not the visible frame"
        );
    }

    /// A window that Windows reports on the second screen.
    fn window_on_portrait(hwnd: isize, title: &str) -> WindowInfo {
        WindowInfo {
            monitor: Some(MonitorId(2)),
            rect: Rect::new(3840, 0, 4920, 1920),
            frame: Rect::new(3840, 0, 4920, 1920),
            ..window(hwnd, title)
        }
    }

    #[test]
    fn every_monitor_gets_a_border_in_the_same_pass() {
        // `BorderManager::update` is handed the complete desired set and
        // removes every border it was not shown, so one monitor's targets on
        // their own delete the borders of all the others.
        let (wm, _) = manager_on(
            vec![window(1, "Main"), window_on_portrait(2, "Portrait")],
            vec![main_screen(), portrait_screen()],
        );
        assert_eq!(wm.state().monitors().len(), 2);
        assert_eq!(wm.state().locate_window(WindowId(2)), Some((1, 0)));

        let targets = wm.visuals_targets();
        let drawn: Vec<Hwnd> = targets.tiled.iter().map(|(hwnd, _, _)| *hwnd).collect();
        assert!(
            drawn.contains(&Hwnd(1)) && drawn.contains(&Hwnd(2)),
            "one monitor's pass left the other monitor's window out, so its              border is removed on every retile: {drawn:?}"
        );
        assert_eq!(
            targets.focused.iter().count(),
            1,
            "only the window the desktop focus is on may carry the focused border"
        );
    }

    #[test]
    fn a_maximized_workspace_only_drops_the_borders_of_its_own_monitor() {
        let (mut wm, _) = manager_on(
            vec![window(1, "Main"), window_on_portrait(2, "Portrait")],
            vec![main_screen(), portrait_screen()],
        );
        wm.on_window_event(WindowEventKind::Foreground, Hwnd(1));
        wm.handle_command(Command::ToggleMaximize);
        assert!(wm.state().workspace(0, 0).unwrap().is_maximized());

        let targets = wm.visuals_targets();
        let drawn: Vec<Hwnd> = targets.tiled.iter().map(|(hwnd, _, _)| *hwnd).collect();
        assert_eq!(
            drawn,
            vec![Hwnd(2)],
            "maximizing on one screen took the borders off every other screen"
        );
    }

    #[test]
    fn a_window_is_named_in_the_record_before_it_goes_off_screen() {
        // A cloaked, hidden or minimized window is exactly the sort
        // `window_info` fails on, and an entry with pid 0 and no class can
        // never be told apart from a handle Windows has since reused.
        let subject = WindowInfo {
            pid: 4242,
            ..window(1, "One")
        };
        let (mut wm, platform) = manager(vec![subject]);
        platform.vanish_when_hidden(Hwnd(1));

        wm.handle_command(Command::FocusWorkspace { index: 1 });
        let hidden = wm.hidden();
        let hidden = hidden.lock().unwrap();
        assert_eq!(
            hidden.identity.get(&Hwnd(1)),
            Some(&(4242, "Chrome_WidgetWin_1".to_owned())),
            "the identity was read after the window was hidden, so the record \
             carries a name that can never match"
        );
    }

    #[test]
    fn a_paused_daemon_refuses_the_commands_it_would_not_carry_out() {
        let (mut wm, platform) = manager(vec![window(1, "One"), window(2, "Two")]);
        wm.handle_command(Command::TogglePause);
        assert!(wm.state().is_paused);

        platform.clear_history();
        for command in [Command::Close, Command::Promote, Command::Retile] {
            let name = command.name();
            let (response, _) = wm.handle_command(command);
            assert_eq!(
                response.error_message(),
                Some(PAUSED),
                "{name} answered Ok while paused, having changed nothing"
            );
        }
        assert!(platform.rect_of(Hwnd(1)).is_none());

        // The command that lifts the pause still has to work.
        let (response, _) = wm.handle_command(Command::TogglePause);
        assert!(response.is_ok());
        assert!(!wm.state().is_paused);
    }

    #[test]
    fn a_reload_of_a_file_that_did_not_parse_is_not_an_ok() {
        let (mut wm, _) = manager(vec![window(1, "One")]);
        let broken = std::env::temp_dir().join("mochi-wm-broken-config.json");
        std::fs::write(&broken, b"{ this is not json").unwrap();
        wm.session.config_path = broken.clone();

        let (response, _) = wm.handle_command(Command::ReloadConfiguration);
        assert!(
            response.error_message().is_some(),
            "mochic reload-configuration reported success for a file nothing \
             could be read from"
        );
        let _ = std::fs::remove_file(&broken);
    }

    /// A crash record path of its own, emptied before the test runs.
    fn record_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mochi-wm-{name}.json"));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_window_that_is_only_faded_is_written_to_the_crash_record() {
        // A faded window is on screen and translucent. A hard kill leaves it
        // that way, and nothing else on the desktop knows why.
        let path = record_path("faded");
        let mut hidden = Hidden::with_record(path.clone());
        hidden.fade(Hwnd(7));

        let entries = crate::recover::load(&path).expect("the record did not parse");
        assert_eq!(
            entries.len(),
            1,
            "a faded window left nothing behind, so nothing will ever make it opaque again"
        );
        assert!(entries[0].faded);
        assert_eq!(entries[0].behaviour, None, "it was never taken off screen");

        // Another window coming back must not take the alpha record with it.
        hidden.hide(Hwnd(8), HidingBehaviour::Cloak);
        hidden.show(Hwnd(8));
        let entries = crate::recover::load(&path).expect("the record did not parse");
        assert_eq!(
            entries.len(),
            1,
            "showing one window erased the record of another that is still faded"
        );
        assert_eq!(entries[0].hwnd, 7);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_record_written_before_the_faded_only_shape_still_parses() {
        let path = record_path("old-shape");
        std::fs::write(
            &path,
            br#"[{"hwnd":1,"pid":4,"class":"C","behaviour":"Cloak","faded":false}]"#,
        )
        .unwrap();
        let entries = crate::recover::load(&path).expect("an existing record stopped parsing");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].behaviour, Some(HidingBehaviour::Cloak));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_crash_record_outlives_the_restore_of_the_first_window() {
        // The console close path is killed by the OS after a fixed timeout, so
        // a record erased before the loop starts loses every window that did
        // not get its turn.
        let path = record_path("restore-order");
        let platform = Arc::new(FakePlatform::new(vec![]));
        let hidden = Mutex::new(Hidden::with_record(path.clone()));
        {
            let mut list = hidden.lock().unwrap();
            list.hide(Hwnd(1), HidingBehaviour::Cloak);
            list.hide(Hwnd(2), HidingBehaviour::Cloak);
        }
        platform.watch_record(&path);

        restore(platform.as_ref(), &hidden);
        assert_eq!(
            platform.record_seen.lock().unwrap().as_slice(),
            [2, 2],
            "the record was emptied before a single window had been put back"
        );
        assert!(
            !path.exists(),
            "the record survived a restore that dealt with everything in it"
        );
        assert!(hidden.lock().unwrap().is_empty());
    }

    #[test]
    fn a_window_the_restore_could_not_put_back_stays_in_the_record() {
        let path = record_path("restore-failure");
        let platform = Arc::new(FakePlatform::new(vec![]));
        platform.cannot_be_restored(Hwnd(2));
        let hidden = Mutex::new(Hidden::with_record(path.clone()));
        {
            let mut list = hidden.lock().unwrap();
            list.identify(Hwnd(2), 4242, "Chrome_WidgetWin_1");
            list.hide(Hwnd(1), HidingBehaviour::Cloak);
            list.hide(Hwnd(2), HidingBehaviour::Cloak);
        }

        restore(platform.as_ref(), &hidden);
        let entries = crate::recover::load(&path).expect("the record did not parse");
        assert_eq!(
            entries.iter().map(|entry| entry.hwnd).collect::<Vec<_>>(),
            vec![2],
            "the window that could not be uncloaked was forgotten, and nothing \
             else will ever bring it back"
        );
        assert_eq!(entries[0].pid, 4242, "it was written back without its name");
        assert_eq!(hidden.lock().unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
