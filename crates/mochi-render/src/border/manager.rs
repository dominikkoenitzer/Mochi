//! The border thread and the cheap handle the daemon holds.

use std::collections::HashMap;
use std::sync::Arc;

use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Factory,
};

use crate::border::window::BorderWindow;
use crate::border::{BorderConfig, BorderSpec};
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
}

impl BorderManager {
    /// Starts the border thread.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadStart`] when the thread or the Direct2D
    /// factory cannot be created.
    pub fn new(config: BorderConfig) -> Result<Self> {
        let worker = spawn_worker(
            "border",
            move || Borders::new(config),
            |borders: &mut Borders, message| borders.handle(message),
        )?;
        Ok(Self {
            worker: Arc::new(worker),
        })
    }

    /// Declares the complete set of borders that should be on screen.
    ///
    /// Anything not in the list is taken down. Send this after every layout
    /// pass; working out the difference is the border thread's job.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the border thread has stopped.
    pub fn set_borders(&self, specs: Vec<BorderSpec>) -> Result<()> {
        self.worker.send(BorderMessage::Set(specs))
    }

    /// Takes every border off the screen, keeping the thread alive.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the border thread has stopped.
    pub fn clear(&self) -> Result<()> {
        self.worker.send(BorderMessage::Clear)
    }

    /// Swaps in a new configuration and repaints.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the border thread has stopped.
    pub fn set_config(&self, config: BorderConfig) -> Result<()> {
        self.worker.send(BorderMessage::Config(config))
    }

    /// Stops the thread and destroys every border window.
    ///
    /// Called automatically when the last handle is dropped; the daemon calls it
    /// explicitly on shutdown so that the frames are gone before the windows
    /// they belong to are restored.
    pub fn stop(&self) {
        self.worker.stop();
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
}

impl Borders {
    fn new(config: BorderConfig) -> Result<Self> {
        // SAFETY: a single threaded factory is only ever used from the thread
        // that created it, which is this one; the border windows are created
        // here too.
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }?;

        Ok(Self {
            factory,
            config,
            active: HashMap::new(),
            idle: Vec::new(),
        })
    }

    fn handle(&mut self, message: BorderMessage) {
        match message {
            BorderMessage::Set(specs) => self.set(specs),
            BorderMessage::Clear => self.clear(),
            BorderMessage::Config(config) => self.reconfigure(config),
        }
    }

    fn set(&mut self, specs: Vec<BorderSpec>) {
        if !self.config.enabled {
            self.clear();
            return;
        }

        let mut next: HashMap<isize, BorderWindow> = HashMap::with_capacity(specs.len());
        for spec in specs {
            let key = spec.target.0;
            let Some(mut window) = self.take_window(key) else {
                continue;
            };

            if let Err(error) = window.track(spec.target.hwnd(), spec.rect, spec.kind) {
                tracing::warn!(target = %WindowHandle(key), %error, "could not draw a border");
            }
            if let Some(duplicate) = next.insert(key, window) {
                // Two specs for the same window: keep one, recycle the other.
                self.recycle(duplicate);
            }
        }

        let leftovers: Vec<BorderWindow> = self.active.drain().map(|(_, window)| window).collect();
        for window in leftovers {
            self.recycle(window);
        }
        self.active = next;
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

    fn clear(&mut self) {
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
