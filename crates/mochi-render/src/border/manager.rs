//! The border thread and the cheap handle the daemon holds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Factory,
};

use crate::animation::FrameUpdate;
use crate::border::window::BorderWindow;
use crate::border::{BorderChanges, BorderConfig, BorderDiff, BorderSpec};
use crate::win::{WorkerHandle, spawn_worker};
use crate::{Result, WindowHandle};

/// How many spare border windows to keep around after a workspace shrinks.
///
/// Creating a window is not free, and a workspace that flips between four and
/// five containers should not create and destroy a window every time.
const MAX_IDLE: usize = 8;

/// What the daemon can ask the border thread to do.
enum BorderMessage {
    /// Replace everything on screen with this set.
    Set(Vec<BorderSpec>),
    /// Do exactly this much and leave every other border alone.
    Apply(Box<BorderChanges>),
    /// Take every border off the screen.
    Clear,
    /// New configuration; everything repaints.
    Config(BorderConfig),
}

/// A handle to the border thread.
///
/// Cloning is cheap and every clone talks to the same thread. When the last
/// clone is dropped the thread stops and its windows go with it.
#[derive(Clone)]
pub struct BorderManager {
    worker: Arc<WorkerHandle<BorderMessage>>,
    /// What the thread was last told, so that an unchanged pass sends nothing.
    /// Shared by every clone, because they all drive the same windows.
    diff: Arc<Mutex<BorderDiff>>,
}

impl BorderManager {
    /// Starts the border thread.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadStart`] when the thread or the Direct2D
    /// factory cannot be created.
    pub fn new(config: BorderConfig) -> Result<Self> {
        // The thread gets the same diff the handle holds, so that a draw which
        // fails can be un-recorded and tried again on the next pass.
        let diff = Arc::new(Mutex::new(BorderDiff::new()));
        let theirs = Arc::clone(&diff);
        let worker = spawn_worker(
            "border",
            move || Borders::new(config, theirs),
            |borders: &mut Borders, message| borders.handle(message),
        )?;
        Ok(Self {
            worker: Arc::new(worker),
            diff,
        })
    }

    /// Forgets what is on screen, so the next pass hands over every border again.
    ///
    /// The daemon calls this when the displays change. A border's measurements
    /// are worked out at the DPI of the screen its rectangle lands on, so a
    /// window that keeps the same rectangle across a DPI change still needs
    /// repainting, and an unchanged pass would send nothing at all.
    pub fn invalidate(&self) {
        self.with_diff(BorderDiff::invalidate);
    }

    /// Declares the borders for one layout pass, and sends only what changed.
    ///
    /// `focused` is the one container that has the focus, `others` is every
    /// other border that should be on screen; anything named by neither is
    /// taken down. A border whose rectangle and kind are the same as last time
    /// is not touched at all, one that only moved is moved, and one whose kind
    /// changed is repainted. When nothing changed nothing is sent.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the border thread has stopped.
    pub fn update(&self, focused: Option<BorderSpec>, others: Vec<BorderSpec>) -> Result<()> {
        let mut specs = others;
        // Last wins, so a window named by both lists ends up focused.
        specs.extend(focused);

        let changes = self.with_diff(|diff| diff.diff(specs));
        if changes.is_empty() {
            return Ok(());
        }
        self.worker.send(BorderMessage::Apply(Box::new(changes)))
    }

    /// Moves the borders of the windows in one animation frame.
    ///
    /// Call this from the [`crate::Animator`] apply callback with the frame it
    /// hands out: the border of every window that has one follows it, keeping
    /// the kind the last [`BorderManager::update`] gave it, and nothing is
    /// added, repainted or taken down. A frame that moved no window with a
    /// border sends nothing.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the border thread has stopped.
    pub fn follow_frame(&self, frame: &[FrameUpdate]) -> Result<()> {
        let changes = self.with_diff(|diff| diff.follow(frame));
        if changes.is_empty() {
            return Ok(());
        }
        self.worker.send(BorderMessage::Apply(Box::new(changes)))
    }

    /// Declares the complete set of borders that should be on screen.
    ///
    /// Anything not in the list is taken down. This is the unconditional form
    /// of [`BorderManager::update`]: every border in the list is handed to its
    /// window whether or not it changed. Prefer `update` in the per pass path.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the border thread has stopped.
    pub fn set_borders(&self, specs: Vec<BorderSpec>) -> Result<()> {
        self.with_diff(|diff| {
            let _ = diff.diff(specs.clone());
        });
        self.worker.send(BorderMessage::Set(specs))
    }

    /// Takes every border off the screen, keeping the thread alive.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the border thread has stopped.
    pub fn clear(&self) -> Result<()> {
        self.with_diff(BorderDiff::invalidate);
        self.worker.send(BorderMessage::Clear)
    }

    /// Swaps in a new configuration and repaints.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the border thread has stopped.
    pub fn set_config(&self, config: BorderConfig) -> Result<()> {
        // A new configuration changes every colour and every measurement, so
        // the next pass has to hand every border to its window again.
        self.with_diff(BorderDiff::invalidate);
        self.worker.send(BorderMessage::Config(config))
    }

    /// Stops the thread and destroys every border window.
    ///
    /// Called automatically when the last handle is dropped; the daemon calls it
    /// explicitly on shutdown so that the frames are gone before the windows
    /// they belong to are restored.
    pub fn stop(&self) {
        self.with_diff(BorderDiff::invalidate);
        self.worker.stop();
    }

    /// Runs `body` against the shared diff state.
    fn with_diff<T>(&self, body: impl FnOnce(&mut BorderDiff) -> T) -> T {
        let mut diff = self.diff.lock().unwrap_or_else(PoisonError::into_inner);
        body(&mut diff)
    }
}

impl std::fmt::Debug for BorderManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BorderManager").finish_non_exhaustive()
    }
}

/// The border thread's state. Lives and dies on that thread.
struct Borders {
    factory: ID2D1Factory,
    config: BorderConfig,
    /// One window per target, keyed by the target's handle.
    active: HashMap<isize, BorderWindow>,
    /// Hidden windows kept for the next container that needs one.
    idle: Vec<BorderWindow>,
    /// The manager's record of what is on screen, shared so that a draw which
    /// failed can be taken back out of it.
    diff: Arc<Mutex<BorderDiff>>,
    /// Targets Windows will not let this process stack a border against.
    ///
    /// A border is positioned relative to its target, so a target that belongs
    /// to an elevated process refuses the call that puts the frame next to it.
    /// There is no correct place left for that border: left on screen it is a
    /// rectangle drawn around a window it is not attached to, and stuck in the
    /// topmost band it ends up painted over every other window. It is taken
    /// down and the target is written down here, because retrying costs a
    /// failed call and a log line on every single pass.
    refused: std::collections::HashSet<isize>,
}

impl Borders {
    fn new(config: BorderConfig, diff: Arc<Mutex<BorderDiff>>) -> Result<Self> {
        // SAFETY: a single threaded factory is only ever used from the thread
        // that created it, which is this one; the border windows are created
        // here too.
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }?;

        Ok(Self {
            factory,
            config,
            diff,
            refused: std::collections::HashSet::new(),
            active: HashMap::new(),
            idle: Vec::new(),
        })
    }

    fn handle(&mut self, message: BorderMessage) {
        match message {
            BorderMessage::Set(specs) => self.set(specs),
            BorderMessage::Apply(changes) => self.apply(&changes),
            BorderMessage::Clear => self.clear(),
            BorderMessage::Config(config) => self.reconfigure(config),
        }
    }

    /// Replaces everything on screen with `specs`.
    fn set(&mut self, specs: Vec<BorderSpec>) {
        if !self.config.enabled {
            self.clear();
            return;
        }

        let wanted: std::collections::HashSet<isize> =
            specs.iter().map(|spec| spec.target.0).collect();
        let stale: Vec<isize> = self
            .active
            .keys()
            .copied()
            .filter(|key| !wanted.contains(key))
            .collect();
        for key in stale {
            self.take_down(key);
        }
        for spec in &specs {
            self.track(spec);
        }
    }

    /// Does exactly what the diff asked for and nothing else.
    fn apply(&mut self, changes: &BorderChanges) {
        if !self.config.enabled {
            self.clear();
            return;
        }
        // Removals first, so a window that has just been freed can be reused by
        // a border that is being added in the same pass.
        for handle in &changes.removed {
            self.take_down(handle.0);
        }
        for spec in changes.specs() {
            self.track(spec);
        }
    }

    /// Points one border window at its target, creating it if it has to.
    fn track(&mut self, spec: &BorderSpec) {
        let key = spec.target.0;
        if self.refused.contains(&key) {
            return;
        }
        let Some(mut window) = self.take_window(key) else {
            return;
        };
        if let Err(error) = window.track(spec.target.hwnd(), spec.rect, spec.kind) {
            if error.is_refusal() {
                // Said once, not once per pass. `BorderWindow::track` has
                // already taken the frame off the screen, so recycling the
                // window is all that is left to do.
                tracing::info!(
                    target = %WindowHandle(key),
                    "no border for this window: it belongs to an elevated process and mochi does                      not, so windows will not let a frame be stacked against it"
                );
                self.refused.insert(key);
                self.recycle(window);
                return;
            }
            // The diff already wrote this spec down as applied, so without
            // this the next pass would see nothing changed, send nothing, and
            // the border would stay missing until its window moved.
            self.diff
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .forget(WindowHandle(key));
            tracing::warn!(target = %WindowHandle(key), %error, "could not draw a border");
        }
        if let Some(duplicate) = self.active.insert(key, window) {
            self.recycle(duplicate);
        }
    }

    /// The window for `key`: the one already tracking it, a spare, or a new one.
    fn take_window(&mut self, key: isize) -> Option<BorderWindow> {
        if let Some(window) = self.active.remove(&key) {
            return Some(window);
        }
        if let Some(window) = self.idle.pop() {
            return Some(window);
        }
        match BorderWindow::new(&self.factory, self.config) {
            Ok(window) => Some(window),
            Err(error) => {
                tracing::warn!(%error, "could not create a border window");
                None
            }
        }
    }

    /// Takes one border off the screen.
    fn take_down(&mut self, key: isize) {
        // The refusal goes with it. Windows REUSES window handle values, and
        // "this one refuses a border" is only true of the window that earned
        // it: carried past the point where that window left the layout, it
        // silently denies a border to whatever is created at the same handle
        // next, for the rest of the session and with nothing logged.
        self.refused.remove(&key);
        if let Some(window) = self.active.remove(&key) {
            self.recycle(window);
        }
    }

    fn clear(&mut self) {
        self.refused.clear();
        let windows: Vec<BorderWindow> = self.active.drain().map(|(_, window)| window).collect();
        for window in windows {
            self.recycle(window);
        }
    }

    fn reconfigure(&mut self, config: BorderConfig) {
        self.config = config;
        for window in self.active.values_mut().chain(self.idle.iter_mut()) {
            window.set_config(config);
        }
        if !config.enabled {
            self.clear();
        }
    }

    /// Hides a window and keeps it for later, or lets it go if there are
    /// already enough spares.
    fn recycle(&mut self, mut window: BorderWindow) {
        window.hide();
        if self.idle.len() < MAX_IDLE {
            self.idle.push(window);
        }
        // Otherwise the window drops here, which destroys it on this thread.
    }
}
