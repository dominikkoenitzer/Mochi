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
//! # What is still only stored
//!
//! Borders, transparency and animations are parsed, kept in
//! [`crate::state::Settings`] and reported by `mochic state`, but nothing draws
//! them yet; that is milestone 5.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use mochi_client::{
    BooleanState, Command, Notification, NotificationEvent, QueryTarget, Response, WindowRef,
};
use mochi_core::model::{HidingBehaviour, Monitor, Window, WindowId};
use mochi_core::rules::{
    ApplicationIdentifier, MatchingRule, MatchingStrategy, RuleDecision, WindowInfo as RuleInfo,
};
use mochi_core::{Changes, Rect, State as CoreState};

use crate::config;
use crate::events::{Event, EventReceiver, EventSender, MonitorEventKind, WindowEventKind};
use crate::ipc::Subscribers;
use crate::platform::types::Unmanageable;
use crate::platform::{
    CloakUnsupported, Hwnd, MonitorInfo, Platform, ShowState, WindowInfo, is_manageable_with,
};
use crate::state::{Settings, State, snapshot};

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

    /// Remembers how to recognise a window, so a reused handle is not touched.
    pub fn identify(&mut self, hwnd: Hwnd, pid: u32, class: &str) {
        self.identity.insert(hwnd, (pid, class.to_owned()));
    }

    /// Records that a window was taken off screen with `behaviour`.
    pub fn hide(&mut self, hwnd: Hwnd, behaviour: HidingBehaviour) {
        self.windows.insert(hwnd, behaviour);
        self.write();
    }

    /// Forgets a window and reports how it had been hidden.
    pub fn show(&mut self, hwnd: Hwnd) -> Option<HidingBehaviour> {
        let previous = self.windows.remove(&hwnd);
        if previous.is_some() {
            self.write();
        }
        previous
    }

    /// Writes the mirror, if there is one.
    fn write(&self) {
        let Some(path) = self.record.as_deref() else {
            return;
        };
        let entries: Vec<crate::recover::Entry> = self
            .windows
            .iter()
            .map(|(hwnd, behaviour)| {
                let (pid, class) = self.identity.get(hwnd).cloned().unwrap_or_default();
                crate::recover::Entry {
                    hwnd: hwnd.0,
                    pid,
                    class,
                    behaviour: *behaviour,
                    faded: self.faded.contains(hwnd),
                }
            })
            .collect();
        crate::recover::save(path, &entries);
    }

    /// True when Mochi is the reason the window is off screen.
    pub fn contains(&self, hwnd: Hwnd) -> bool {
        self.windows.contains_key(&hwnd)
    }

    /// Records that Mochi set an alpha value on a window.
    pub fn fade(&mut self, hwnd: Hwnd) {
        self.faded.insert(hwnd);
        self.write();
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

    /// Everything Mochi owes the user back, emptying the record.
    fn drain(&mut self) -> (Vec<(Hwnd, HidingBehaviour)>, Vec<Hwnd>) {
        if let Some(path) = self.record.as_deref() {
            crate::recover::save(path, &[]);
        }
        self.identity.clear();
        (
            std::mem::take(&mut self.windows).into_iter().collect(),
            std::mem::take(&mut self.faded).into_iter().collect(),
        )
    }
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
    for (hwnd, behaviour) in windows {
        let result = match behaviour {
            HidingBehaviour::Cloak => platform.set_cloaked(hwnd, false),
            HidingBehaviour::Minimize => platform.show(hwnd, ShowState::Restore),
            HidingBehaviour::Hide => platform.show(hwnd, ShowState::ShowNoActivate),
        };
        if let Err(e) = result {
            tracing::error!(%hwnd, error = %e, "restore: could not show a window");
        }
    }
    for hwnd in faded {
        if let Err(e) = platform.set_transparency(hwnd, None) {
            tracing::error!(%hwnd, error = %e, "restore: could not clear the alpha");
        }
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
    /// The window the user is dragging right now.
    dragging: Option<Hwnd>,
    /// The foreground window, as far as Mochi knows.
    foreground: Option<Hwnd>,
    /// Borders, transparency and animation. Optional in every part; a
    /// setting that is off means the matching manager does not exist.
    visuals: crate::visuals::Visuals,
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
        let record = crate::recover::default_path();
        let recovered = crate::recover::recover(platform.as_ref(), &record);
        if !recovered.is_empty() {
            tracing::info!(
                count = recovered.len(),
                "brought back windows a previous session left off screen"
            );
        }

        let hidden = Arc::new(Mutex::new(Hidden::with_record(record)));
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
            dragging: None,
            foreground: None,
            visuals,
        };

        wm.refresh_monitors();
        wm.load_config();
        wm.foreground = wm.platform.foreground_window();
        wm.adopt_existing_windows();
        wm.retile();
        Ok(wm)
    }

    /// Hands the mouse tracker over so `focus-follows-mouse` can toggle it.
    pub fn attach_mouse_tracker(&mut self, tracker: crate::events::mouse::MouseTracker) {
        tracker.set_enabled(self.core.focus_follows_mouse.is_some());
        self.mouse = Some(tracker);
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
    fn load_config(&mut self) {
        let path = self.session.config_path.clone();
        let loaded = match config::load(&path) {
            Ok(loaded) => loaded,
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "keeping the previous configuration");
                return;
            }
        };

        loaded.config.apply_to(&mut self.core);
        self.core.rules.extend(loaded.app_rules);
        for broken in self.core.rules.validate() {
            tracing::warn!(error = %broken, "a rule was dropped");
        }
        self.session.settings.apply(&loaded.config);
        self.visuals.set_settings(&loaded.config);
        self.session.app_config_path.clone_from(&loaded.app_path);

        self.workspace_rules = loaded
            .config
            .workspace_rules()
            .into_iter()
            .map(|(monitor, workspace, rule, initial_only)| WorkspaceRule {
                monitor,
                workspace,
                rule,
                initial_only,
            })
            .collect();

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
        self.core.rules.should_ignore(&rule_info(info))
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
            }
            Err(e) => tracing::warn!(hwnd = %info.hwnd, error = %e, "could not manage a window"),
        }
    }

    /// Which workspace a new window belongs on.
    ///
    /// A `workspace_rules` match wins. Without one the window stays on the
    /// monitor Windows put it on, which is far less surprising than dragging
    /// everything onto whatever monitor happens to be focused.
    fn destination(&self, info: &WindowInfo, window: &Window) -> (usize, usize) {
        let rule_info = window.info();
        for rule in &self.workspace_rules {
            if rule.initial_only && self.routed.contains(&info.hwnd) {
                continue;
            }
            if rule.rule.matches(&rule_info)
                && self.core.workspace(rule.monitor, rule.workspace).is_ok()
            {
                return (rule.monitor, rule.workspace);
            }
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

    /// Drops a window from the model, whatever the reason.
    fn unmanage(&mut self, hwnd: Hwnd, why: &str) {
        let id = window_id(hwnd);
        let Some(window) = self.core.window(id).cloned() else {
            return;
        };
        tracing::info!(%hwnd, title = %window.title, why, "unmanaging");
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
        if let Ok(mut hidden) = self.hidden.lock() {
            hidden.show(hwnd);
        }
        self.routed.remove(&hwnd);
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
            if let Ok((monitor, workspace)) = self.core.focused_indices() {
                let targets = self.visuals_targets(monitor, workspace);
                self.visuals.update(&targets);
            }
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
                let identity = self.platform.window_info(hwnd).ok();
                if let Ok(mut hidden) = self.hidden.lock() {
                    if let Some(info) = identity {
                        hidden.identify(hwnd, info.pid, &info.class);
                    }
                    hidden.hide(hwnd, behaviour);
                }
            }
            Err(e) => tracing::error!(%hwnd, ?behaviour, error = %e, "could not hide a window"),
        }
    }

    /// Brings a window back, undoing whatever took it off screen.
    fn show_window(&mut self, hwnd: Hwnd) {
        let previous = self
            .hidden
            .lock()
            .ok()
            .and_then(|mut hidden| hidden.show(hwnd));
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
        if let Err(e) = result {
            tracing::error!(%hwnd, error = %e, "could not show a window");
        }
    }

    /// Gives a window the foreground, unless it already has it.
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
        // The visuals pass runs even with nothing to tile: a workspace holding
        // only floating windows still wants a border on the focused one, and an
        // emptied workspace has to drop the borders it had.
        let targets = self.visuals_targets(monitor, workspace);
        self.visuals.update(&targets);
    }

    /// Every visible window of a workspace, classified for the borders and
    /// transparency pass: which one has the focus, the [`mochi_render::BorderKind`]
    /// it should draw with, and the rest as the transparency set. Empty when
    /// the workspace is not the one showing on its monitor, matching
    /// [`WindowManager::placements_for`].
    fn visuals_targets(&self, monitor: usize, workspace: usize) -> crate::visuals::VisualsTargets {
        use mochi_render::BorderKind;

        let mut targets = crate::visuals::VisualsTargets::default();
        let Some(display) = self.core.monitors().get(monitor) else {
            return targets;
        };
        if display.focused_workspace_idx() != workspace {
            return targets;
        }
        let Ok(target) = self.core.workspace(monitor, workspace) else {
            return targets;
        };
        if target.is_maximized() {
            return targets;
        }

        let focused = target.focused_window_id().map(handle);
        targets.focused = focused;
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
                let rect = target.full_rect(
                    work_area,
                    self.core.default_workspace_padding,
                    self.core.default_container_padding,
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
                    push(floating_hwnd, info.rect);
                }
            }
        }

        targets
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
            let rect = target.full_rect(
                work_area,
                self.core.default_workspace_padding,
                self.core.default_container_padding,
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

    /// Where window events become tree updates.
    fn on_window_event(&mut self, kind: WindowEventKind, hwnd: Hwnd) {
        match kind {
            WindowEventKind::LocationChange => {}
            WindowEventKind::Destroyed => self.unmanage(hwnd, "destroyed"),
            WindowEventKind::Hidden | WindowEventKind::Cloaked => {
                // Mochi's own cloak comes back as an event; ignoring it is what
                // keeps a workspace switch from unmanaging everything it hid.
                if !self.we_hid(hwnd) {
                    self.unmanage(hwnd, kind.as_str());
                }
            }
            WindowEventKind::Created | WindowEventKind::Shown | WindowEventKind::Uncloaked => {
                self.window_appeared(hwnd);
            }
            WindowEventKind::MinimizeStart => {
                if self.core.is_managed(window_id(hwnd)) {
                    self.minimized.insert(hwnd);
                    self.unmanage(hwnd, "minimized");
                }
            }
            WindowEventKind::MinimizeEnd => {
                self.minimized.remove(&hwnd);
                self.window_appeared(hwnd);
            }
            WindowEventKind::Foreground => self.window_focused(hwnd),
            WindowEventKind::MoveSizeStart => self.dragging = Some(hwnd),
            WindowEventKind::MoveSizeEnd => self.window_dropped(hwnd),
            WindowEventKind::NameChange => self.window_renamed(hwnd),
        }
    }

    fn we_hid(&self, hwnd: Hwnd) -> bool {
        self.hidden.lock().is_ok_and(|hidden| hidden.contains(hwnd))
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
        self.dragging = None;
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
            RuleDecision::Tile => {}
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
        self.notify(NotificationEvent::MonitorsChanged {
            count: self.core.monitors().len(),
        });
        self.retile();
    }

    fn rebuild_monitors(&mut self, infos: &[MonitorInfo]) {
        let focused_device = self
            .core
            .focused_monitor()
            .map(|monitor| monitor.device.clone())
            .unwrap_or_default();

        let mut previous = Vec::new();
        while let Some(monitor) = self.core.remove_monitor(0) {
            previous.push(monitor);
        }

        for info in infos {
            let carried = previous
                .iter()
                .position(|m| !m.device.is_empty() && m.device == info.device_name)
                .or_else(|| previous.iter().position(|m| m.id == info.id.0));
            let mut monitor = match carried {
                Some(idx) => previous.remove(idx),
                None => Monitor::new(info.id.0, info.size, info.work_area),
            };
            apply_monitor_info(&mut monitor, info);
            self.core.add_monitor(monitor);
        }

        // Whatever was left behind on a monitor that is gone.
        let orphans: Vec<(usize, Window)> = previous
            .iter()
            .flat_map(|monitor| {
                monitor
                    .workspaces()
                    .iter()
                    .enumerate()
                    .flat_map(|(idx, workspace)| {
                        workspace.all_windows().map(move |w| (idx, w.clone()))
                    })
            })
            .collect();
        if !orphans.is_empty() {
            tracing::warn!(
                count = orphans.len(),
                "rehoming windows from a monitor that went away"
            );
        }
        for (workspace, window) in orphans {
            if let Some(first) = self.core.monitors_mut().get_mut(0) {
                first.ensure_workspaces(workspace + 1);
            }
            let _ = self.core.add_window_to(0, workspace, window);
        }

        if let Some(idx) = self
            .core
            .monitors()
            .position(|monitor| monitor.device == focused_device)
        {
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
    fn run_op(
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
    fn run_layout_op(
        &mut self,
        op: impl FnOnce(&mut CoreState) -> mochi_core::Result<Changes>,
    ) -> Response {
        let response = self.run_op(op);
        if response.is_ok()
            && let Ok((monitor, workspace)) = self.core.focused_indices()
            && let Ok(target) = self.core.workspace(monitor, workspace)
        {
            let layout = target.effective_layout().to_string();
            self.notify(NotificationEvent::LayoutChange {
                monitor,
                workspace,
                layout,
            });
        }
        response
    }

    fn handle_command(&mut self, command: Command) -> (Response, Flow) {
        use mochi_client as wire;

        let response = match command {
            Command::State => Response::State {
                state: snapshot(&self.session, &self.core, self.foreground),
            },
            Command::Query { target } => self.query(target),
            Command::Stop { whkd } => {
                tracing::info!(whkd, "stop requested");
                return (Response::Ok, Flow::Stop);
            }
            Command::Start { .. } => Response::Ok,
            Command::Quickstart => self.quickstart(),

            // --- lifecycle ---------------------------------------------
            Command::TogglePause => {
                let response = self.run_op(CoreState::toggle_pause);
                tracing::info!(paused = self.core.is_paused, "pause toggled");
                if self.core.is_paused {
                    self.visuals.clear();
                } else {
                    self.retile();
                }
                self.notify(NotificationEvent::Pause {
                    paused: self.core.is_paused,
                });
                response
            }
            Command::ReloadConfiguration => {
                self.reload_config();
                Response::Ok
            }
            Command::Retile => self.run_op(CoreState::retile),

            // --- focus and movement ------------------------------------
            Command::Focus { direction } => {
                self.run_op(|core| core.focus_direction(direction_of(direction)))
            }
            Command::Move { direction } => {
                self.run_op(|core| core.move_direction(direction_of(direction)))
            }
            Command::ResizeAxis { axis, sizing } => {
                self.run_op(|core| core.resize_axis(axis_of(axis), sizing_of(sizing)))
            }
            Command::Promote => self.run_op(CoreState::promote),

            // --- window state ------------------------------------------
            Command::ToggleFloat => self.run_op(CoreState::toggle_float),
            Command::ToggleMaximize => self.run_op(CoreState::toggle_maximize),
            Command::ToggleMonocle => self.run_op(CoreState::toggle_monocle),
            Command::Minimize => self.run_op(CoreState::minimize_focused_window),
            Command::Close => self.run_op(CoreState::close_focused_window),
            Command::Manage => self.manage_foreground(),
            Command::Unmanage => self.unmanage_foreground(),

            // --- stacks -------------------------------------------------
            Command::Stack { direction } => self.run_op(|core| core.stack(direction_of(direction))),
            Command::Unstack => self.run_op(CoreState::unstack),
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

            // --- workspaces ----------------------------------------------
            Command::FocusWorkspace { index } => self.run_op(|core| core.focus_workspace(index)),
            Command::MoveToWorkspace { index } => {
                self.run_op(|core| core.move_to_workspace(index, true))
            }
            Command::CycleWorkspace { direction } => {
                self.run_op(|core| core.cycle_workspace(cycle_of(direction)))
            }
            Command::FocusLastWorkspace => self.run_op(CoreState::focus_last_workspace),
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

            // --- visuals: stored, nothing draws them yet -----------------------
            Command::ToggleTransparency => {
                self.session.settings.transparency = !self.session.settings.transparency;
                self.stored("transparency")
            }
            Command::Border { state } => {
                Settings::set(&mut self.session.settings.border, state);
                self.stored("border")
            }
            Command::BorderWidth { width } => {
                self.session.settings.border_width = width;
                self.stored("border-width")
            }
            Command::BorderOffset { offset } => {
                self.session.settings.border_offset = offset;
                self.stored("border-offset")
            }
            Command::BorderStyle { style } => {
                self.session.settings.border_style = style;
                self.stored("border-style")
            }
            Command::BorderColour { kind, r, g, b } => {
                tracing::info!(%kind, r, g, b, "border colour stored, nothing draws it yet");
                Response::Ok
            }
            Command::Animation { state } => {
                Settings::set(&mut self.session.settings.animation, state);
                self.stored("animation")
            }
            Command::AnimationDuration { duration } => {
                self.session.settings.animation_duration = duration;
                self.stored("animation-duration")
            }
            Command::AnimationFps { fps } => {
                self.session.settings.animation_fps = fps;
                self.stored("animation-fps")
            }
            Command::AnimationStyle { style } => {
                self.session.settings.animation_style = style;
                self.stored("animation-style")
            }
        };

        let _ = std::marker::PhantomData::<wire::Layout>;
        (response, Flow::Continue)
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
        match decision {
            RuleDecision::Ignore => self.core.rules.ignore_rules.push(rule),
            _ => self.core.rules.floating_applications.push(rule),
        }
        // A rule that arrives after the window it describes has to catch up.
        self.reapply_rules();
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

    fn stored(&self, what: &str) -> Response {
        tracing::info!(setting = what, "stored, nothing draws it yet");
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
    pub fn reload_config(&mut self) {
        let path = self.session.config_path().to_path_buf();
        self.load_config();
        self.notify(NotificationEvent::Reload {
            path: path.display().to_string(),
        });
        self.retile();
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
        self.subscribers.notify(Notification::new(event));
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
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A platform with no desktop behind it, so the loop can be tested anywhere.
    struct FakePlatform {
        monitors: Vec<MonitorInfo>,
        windows: Mutex<Vec<WindowInfo>>,
        moves: AtomicUsize,
        cloaks: Mutex<Vec<(Hwnd, bool)>>,
        shows: Mutex<Vec<(Hwnd, ShowState)>>,
        focused: Mutex<Vec<Hwnd>>,
        placements: Mutex<Vec<(Hwnd, Rect)>>,
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
                windows: Mutex::new(windows),
                moves: AtomicUsize::new(0),
                cloaks: Mutex::new(Vec::new()),
                shows: Mutex::new(Vec::new()),
                focused: Mutex::new(Vec::new()),
                placements: Mutex::new(Vec::new()),
            }
        }

        fn last_placements(&self) -> Vec<(Hwnd, Rect)> {
            self.placements.lock().unwrap().clone()
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
        fn set_positions(&self, placements: &[WindowPlacement]) -> Result<()> {
            self.moves.fetch_add(placements.len(), Ordering::SeqCst);
            *self.placements.lock().unwrap() =
                placements.iter().map(|p| (p.hwnd, p.rect)).collect();
            Ok(())
        }
        fn set_cloaked(&self, hwnd: Hwnd, cloaked: bool) -> Result<()> {
            self.cloaks.lock().unwrap().push((hwnd, cloaked));
            Ok(())
        }
        fn show(&self, hwnd: Hwnd, state: ShowState) -> Result<()> {
            self.shows.lock().unwrap().push((hwnd, state));
            Ok(())
        }
        fn focus(&self, hwnd: Hwnd) -> Result<()> {
            self.focused.lock().unwrap().push(hwnd);
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

    fn manager(windows: Vec<WindowInfo>) -> (WindowManager, Arc<FakePlatform>) {
        let platform = Arc::new(FakePlatform::new(windows));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut session = State::new(PathBuf::from(r"C:\nowhere\mochi.json"), true);
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
        let mut session = State::new(PathBuf::from(r"C:\nowhere\mochi.json"), true);
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
        let mut session = State::new(PathBuf::from(r"C:\nowhere\mochi.json"), true);
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
    fn focus_moves_between_the_tiles() {
        let (mut wm, _) = manager(vec![window(1, "One"), window(2, "Two")]);
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
        let (response, flow) = wm.handle_command(Command::Stop { whkd: false });
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
}
