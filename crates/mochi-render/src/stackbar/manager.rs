//! The stackbar thread and the handle the daemon holds.

use std::collections::HashMap;
use std::sync::Arc;

use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory,
};

use crate::stackbar::window::{ClickCallback, StackbarWindow};
use mochi_core::config::{StackbarConfig, StackbarMode};

use crate::stackbar::{StackbarModeExt, StackbarSpec, StackbarStyle};
use crate::win::{WorkerHandle, spawn_worker};
use crate::{Result, WindowHandle};

/// How many spare bars to keep for the next stack.
const MAX_IDLE: usize = 4;

/// What the daemon can ask the stackbar thread to do.
enum StackbarMessage {
    Set(Vec<StackbarSpec>),
    Clear,
    Config(Box<StackbarConfig>),
}

/// A handle to the stackbar thread.
///
/// Cloning is cheap; the thread stops when the last clone is dropped.
#[derive(Clone)]
pub struct StackbarManager {
    worker: Arc<WorkerHandle<StackbarMessage>>,
}

impl StackbarManager {
    /// Starts the stackbar thread.
    ///
    /// `on_click` is called when a tab is clicked, **on the stackbar thread**.
    /// It must not block: the daemon drops the handle into its own channel and
    /// returns.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadStart`] when the thread, Direct2D or
    /// DirectWrite cannot be started.
    pub fn new<F>(config: StackbarConfig, on_click: F) -> Result<Self>
    where
        F: Fn(WindowHandle) + Send + Sync + 'static,
    {
        let callback: ClickCallback = Arc::new(on_click);
        let worker = spawn_worker(
            "stackbar",
            move || Stackbars::new(config, callback),
            |stackbars: &mut Stackbars, message| stackbars.handle(message),
        )?;
        Ok(Self {
            worker: Arc::new(worker),
        })
    }

    /// Declares the complete set of stackbars that should be on screen.
    ///
    /// Containers the configured mode does not cover are dropped here, so the
    /// daemon can send every container and let this decide.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the stackbar thread has stopped.
    pub fn set_stackbars(&self, specs: Vec<StackbarSpec>) -> Result<()> {
        self.worker.send(StackbarMessage::Set(specs))
    }

    /// Takes every stackbar off the screen.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the stackbar thread has stopped.
    pub fn clear(&self) -> Result<()> {
        self.worker.send(StackbarMessage::Clear)
    }

    /// Swaps in a new configuration and repaints.
    ///
    /// # Errors
    ///
    /// [`crate::RenderError::ThreadGone`] when the stackbar thread has stopped.
    pub fn set_config(&self, config: StackbarConfig) -> Result<()> {
        self.worker.send(StackbarMessage::Config(Box::new(config)))
    }

    /// Stops the thread and destroys every bar.
    pub fn stop(&self) {
        self.worker.stop();
    }
}

impl std::fmt::Debug for StackbarManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackbarManager").finish_non_exhaustive()
    }
}

/// The stackbar thread's state.
struct Stackbars {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    style: StackbarStyle,
    on_click: ClickCallback,
    active: HashMap<u64, StackbarWindow>,
    idle: Vec<StackbarWindow>,
}

impl Stackbars {
    fn new(config: StackbarConfig, on_click: ClickCallback) -> Result<Self> {
        // SAFETY: a single threaded factory used only from this thread, which
        // also owns every window it draws into.
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }?;
        // SAFETY: the shared DirectWrite factory is reference counted by the
        // system and is safe to hold for the life of the thread.
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }?;

        Ok(Self {
            factory,
            dwrite,
            style: StackbarStyle::from(&config),
            on_click,
            active: HashMap::new(),
            idle: Vec::new(),
        })
    }

    fn handle(&mut self, message: StackbarMessage) {
        match message {
            StackbarMessage::Set(specs) => self.set(specs),
            StackbarMessage::Clear => self.clear(),
            StackbarMessage::Config(config) => self.reconfigure(*config),
        }
    }

    fn set(&mut self, specs: Vec<StackbarSpec>) {
        let mut next: HashMap<u64, StackbarWindow> = HashMap::new();
        for spec in specs {
            if !self.style.mode.shows(spec.tabs.len()) {
                continue;
            }
            let Some(mut bar) = self.take_window(spec.id) else {
                continue;
            };
            if let Err(error) = bar.update(&spec, &self.style) {
                tracing::warn!(container = spec.id, %error, "could not draw a stackbar");
            }
            if let Some(duplicate) = next.insert(spec.id, bar) {
                self.recycle(duplicate);
            }
        }

        let leftovers: Vec<StackbarWindow> = self.active.drain().map(|(_, bar)| bar).collect();
        for bar in leftovers {
            self.recycle(bar);
        }
        self.active = next;
    }

    fn take_window(&mut self, id: u64) -> Option<StackbarWindow> {
        if let Some(bar) = self.active.remove(&id) {
            return Some(bar);
        }
        if let Some(bar) = self.idle.pop() {
            return Some(bar);
        }
        match StackbarWindow::new(
            &self.factory,
            &self.dwrite,
            self.style.clone(),
            Arc::clone(&self.on_click),
        ) {
            Ok(bar) => Some(bar),
            Err(error) => {
                tracing::warn!(%error, "could not create a stackbar window");
                None
            }
        }
    }

    fn clear(&mut self) {
        let bars: Vec<StackbarWindow> = self.active.drain().map(|(_, bar)| bar).collect();
        for bar in bars {
            self.recycle(bar);
        }
    }

    fn reconfigure(&mut self, config: StackbarConfig) {
        self.style = StackbarStyle::from(&config);
        // The bars pick the new configuration up on the next set; a mode of
        // Never means there should be none at all.
        if self.style.mode == StackbarMode::Never {
            self.clear();
        }
    }

    fn recycle(&mut self, mut bar: StackbarWindow) {
        bar.hide();
        if self.idle.len() < MAX_IDLE {
            self.idle.push(bar);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use mochi_core::Rect;

    use super::*;
    use crate::stackbar::StackbarTab;

    /// Puts a real bar on the screen for a moment. Ignored by default because
    /// it needs a desktop and draws on it:
    /// `cargo test -p mochi-render -- --ignored`.
    #[test]
    #[ignore = "draws on the desktop"]
    fn a_stackbar_appears_and_goes_away() {
        let clicks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&clicks);

        let manager = StackbarManager::new(
            StackbarConfig {
                mode: Some(StackbarMode::Always),
                ..Default::default()
            },
            move |_| {
                counter.fetch_add(1, Ordering::Relaxed);
            },
        )
        .expect("the stackbar thread should start");

        manager
            .set_stackbars(vec![StackbarSpec {
                id: 1,
                rect: Rect::new(400, 400, 1600, 900),
                above: WindowHandle::NONE,
                tabs: vec![
                    StackbarTab::new(WindowHandle(1), "firefox", true),
                    StackbarTab::new(WindowHandle(2), "Code", false),
                    StackbarTab::new(WindowHandle(3), "WindowsTerminal", false),
                ],
            }])
            .expect("the bar should be accepted");

        std::thread::sleep(Duration::from_millis(2000));
        manager.clear().expect("the bar should come down");
        std::thread::sleep(Duration::from_millis(200));
        manager.stop();

        assert_eq!(
            clicks.load(Ordering::Relaxed),
            0,
            "nothing clicked it, so nothing was reported"
        );
    }
}
