//! Borders, transparency and animation, wired from the daemon's settings.
//!
//! Every Win32 detail lives in `mochi-render`; [`Visuals`] only translates the
//! daemon's own types ([`Hwnd`], [`mochi_core::Rect`]) into the render
//! crate's ([`WindowHandle`], the same `Rect`) and decides what the desired
//! end state is. Each of the three managers only exists while its setting is
//! on, so tiling keeps working with every visual switched off, and
//! [`Visuals::set_settings`] creates or tears one down exactly when a
//! configuration reload flips it.
//!
//! # Restore
//!
//! Whenever [`Visuals::update`] fades a window it also records the window in
//! the shared [`Hidden`] list with [`Hidden::fade`], and unrecords it the
//! moment it stops being faded. That is what lets [`crate::wm::restore`] and
//! the panic hook clear every alpha this module ever set, even if the process
//! never gets to call [`Visuals::clear`] itself.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use mochi_core::Rect;
use mochi_core::config::Config;
use mochi_render::{
    AnimationConfig, AnimationConfigExt, Animator, BorderConfig, BorderKind, BorderManager,
    BorderSpec, FrameUpdate, TransparencyManager, WindowHandle,
};

use crate::platform::{Hwnd, Platform, WindowPlacement};
use crate::wm::Hidden;

/// The render crate's handle for one of the daemon's own.
const fn handle(hwnd: Hwnd) -> WindowHandle {
    WindowHandle(hwnd.0)
}

/// The daemon's handle for one of the render crate's own.
const fn hwnd_of(handle: WindowHandle) -> Hwnd {
    Hwnd(handle.0)
}

/// Everything [`Visuals::update`] needs for one workspace: which window has
/// the focus, every visible window with the rectangle and [`BorderKind`] it
/// should be drawn with, and the subset that should be faded.
#[derive(Debug, Default, Clone)]
pub struct VisualsTargets {
    /// The window the desktop focus is on, if any is visible on this
    /// workspace.
    pub focused: Option<Hwnd>,
    /// Every visible managed window, its target rectangle and which border it
    /// should get.
    pub tiled: Vec<(Hwnd, Rect, BorderKind)>,
    /// Exactly the windows that are not focused, for transparency.
    pub unfocused: Vec<Hwnd>,
}

/// Owns the optional border, transparency and animation managers and decides
/// what each should show.
pub struct Visuals {
    platform: Arc<dyn Platform>,
    hidden: Arc<Mutex<Hidden>>,
    borders: Arc<Mutex<Option<BorderManager>>>,
    transparency: Option<TransparencyManager>,
    animator: Option<Animator>,
    animation: AnimationConfig,
    /// Windows this pass has faded, mirrored into `hidden` so a crash undoes
    /// them; kept here so the next pass knows what to un-mirror.
    faded: BTreeSet<Hwnd>,
}

impl Visuals {
    /// A visuals owner with nothing switched on yet.
    ///
    /// Call [`Visuals::set_settings`] with the loaded configuration to create
    /// whichever managers the config turns on.
    #[must_use]
    pub fn new(platform: Arc<dyn Platform>, hidden: Arc<Mutex<Hidden>>) -> Self {
        Self {
            platform,
            hidden,
            borders: Arc::new(Mutex::new(None)),
            transparency: None,
            animator: None,
            animation: AnimationConfig::default(),
            faded: BTreeSet::new(),
        }
    }

    /// Applies the visual keys of a freshly loaded configuration.
    ///
    /// A manager whose setting just turned on is created; one whose setting
    /// just turned off is cleared and torn down; one that stays on gets its
    /// new settings pushed into it. Safe to call on every reload, including
    /// the first one.
    pub fn set_settings(&mut self, config: &Config) {
        self.apply_border_settings(config);
        self.apply_transparency_settings(config);
        self.apply_animation_settings(config);
    }

    fn apply_border_settings(&mut self, config: &Config) {
        let border_config = BorderConfig::from(config);
        let mut guard = match self.borders.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if border_config.enabled {
            match guard.as_ref() {
                Some(existing) => {
                    if let Err(e) = existing.set_config(border_config) {
                        tracing::error!(error = %e, "could not update the border settings");
                    }
                }
                None => match BorderManager::new(border_config) {
                    Ok(manager) => *guard = Some(manager),
                    Err(e) => tracing::error!(error = %e, "could not start the border manager"),
                },
            }
        } else if let Some(manager) = guard.take() {
            if let Err(e) = manager.clear() {
                tracing::error!(error = %e, "could not clear borders");
            }
            manager.stop();
        }
    }

    fn apply_transparency_settings(&mut self, config: &Config) {
        let on = config.transparency.unwrap_or(false);
        let alpha = config.transparency_alpha.unwrap_or(200);
        if on {
            match &mut self.transparency {
                Some(manager) => manager.set_alpha(alpha),
                None => self.transparency = Some(TransparencyManager::new(alpha)),
            }
            return;
        }
        let still = match self.transparency.take() {
            Some(mut manager) => {
                if let Err(e) = manager.clear_all() {
                    tracing::error!(error = %e, "could not clear transparency");
                }
                Self::still_faded(&self.faded, Some(&manager))
            }
            None => BTreeSet::new(),
        };
        self.unfade_all(&still);
    }

    fn apply_animation_settings(&mut self, config: &Config) {
        self.animation = config.animation.unwrap_or_default();
        let on = self.animation.enabled.unwrap_or(false);
        if on && self.animator.is_none() {
            let platform = Arc::clone(&self.platform);
            let borders = Arc::clone(&self.borders);
            match Animator::new(move |frame: &[FrameUpdate]| {
                let batch: Vec<WindowPlacement> = frame
                    .iter()
                    .map(|update| WindowPlacement::new(hwnd_of(update.handle), update.rect))
                    .collect();
                // Borders first, and the order is the whole point. This one
                // only drops a message in the border thread's channel and
                // returns, while `set_positions` is synchronous and blocks on
                // each target application's message pump - `SWP_FRAMECHANGED`
                // makes every one of them recalculate and repaint its frame
                // before the call comes back. Moving the window first meant
                // the border could not even begin to follow until that had
                // finished, so it trailed its window by the cost of the move,
                // every frame, on every animation. Posted first, the border
                // thread does its work while that call is still in flight.
                if let Ok(guard) = borders.lock()
                    && let Some(manager) = guard.as_ref()
                    && let Err(e) = manager.follow_frame(frame)
                {
                    tracing::error!(error = %e, "could not move borders to follow a frame");
                }
                if let Err(e) = platform.set_positions(&batch) {
                    tracing::error!(error = %e, "could not apply an animation frame");
                }
            }) {
                Ok(animator) => self.animator = Some(animator),
                Err(e) => tracing::error!(error = %e, "could not start the animation thread"),
            }
        } else if !on && let Some(animator) = self.animator.take() {
            let _ = animator.cancel_all();
        }
    }

    /// Turns computed rectangles into one batched window move: animated when
    /// the setting is on, applied directly through [`Platform::set_positions`]
    /// otherwise. Borders follow every animation frame through
    /// [`BorderManager::follow_frame`].
    pub fn apply_layout(&self, placements: &[(Hwnd, Rect)]) {
        if placements.is_empty() {
            return;
        }
        if let Some(animator) = &self.animator {
            let jobs: Vec<_> = placements
                .iter()
                .filter_map(|&(target, to)| {
                    // The perceived frame, not `GetWindowRect`: `to` is a
                    // layout rectangle and `Platform::set_positions`
                    // compensates for the invisible resize border on every
                    // frame, so starting from the window rect would have the
                    // window jump outward by that border and ease back.
                    match self.platform.window_info(target) {
                        Ok(info) => {
                            let from = info.visible_frame();
                            // A window already at its target is not animated.
                            // It would otherwise get a full `SWP_FRAMECHANGED`
                            // move on every frame of the animation, which makes
                            // it recalculate and repaint its whole non-client
                            // area, and `EndDeferWindowPos` blocks on its
                            // message pump while it does. A layout usually
                            // moves one or two windows and leaves the rest
                            // exactly where they were, so this is most of the
                            // batch. The border path already refuses the same
                            // work for a border whose rectangle did not change.
                            if from == to {
                                return None;
                            }
                            Some(self.animation.job(handle(target), from, to))
                        }
                        // The read failed, which is what a window that has just
                        // died looks like. It is still placed, starting from
                        // its target: this is the one case where `from` is a
                        // guess, and skipping on `from == to` here would mean
                        // never moving a window whose info could not be read.
                        Err(_) => Some(self.animation.job(handle(target), to, to)),
                    }
                })
                .collect();
            if jobs.is_empty() {
                return;
            }
            if let Err(e) = animator.animate(jobs) {
                tracing::error!(error = %e, "could not animate a layout, applying it directly");
                self.apply_direct(placements);
            }
        } else {
            self.apply_direct(placements);
        }
    }

    fn apply_direct(&self, placements: &[(Hwnd, Rect)]) {
        let batch: Vec<WindowPlacement> = placements
            .iter()
            .map(|&(target, rect)| WindowPlacement::new(target, rect))
            .collect();
        if let Err(e) = self.platform.set_positions(&batch) {
            tracing::error!(error = %e, count = batch.len(), "could not apply a layout");
        }
    }

    /// Sets the end state for borders and transparency after a layout pass.
    ///
    /// `targets.tiled` is every visible managed window with the rectangle and
    /// [`BorderKind`] it should have; `targets.focused` names which one of
    /// them holds the focus, so its border goes through
    /// [`BorderManager::update`]'s `focused` slot; `targets.unfocused` is
    /// exactly the set transparency should fade.
    pub fn update(&mut self, targets: &VisualsTargets) {
        if let Ok(guard) = self.borders.lock()
            && let Some(manager) = guard.as_ref()
        {
            let mut focused_spec = None;
            let mut others = Vec::with_capacity(targets.tiled.len());
            for &(target, rect, kind) in &targets.tiled {
                let spec = BorderSpec::new(handle(target), rect, kind);
                if Some(target) == targets.focused {
                    focused_spec = Some(spec);
                } else {
                    others.push(spec);
                }
            }
            if let Err(e) = manager.update(focused_spec, others) {
                tracing::error!(error = %e, "could not update borders");
            }
        }

        if let Some(manager) = &mut self.transparency {
            let wanted: Vec<WindowHandle> = targets.unfocused.iter().copied().map(handle).collect();
            if let Err(e) = manager.update(&wanted) {
                tracing::error!(error = %e, "could not update transparency");
            }
            // Everything the manager still has faded, which is the unfocused
            // set plus anything it could not put back: a window whose restore
            // failed is still translucent, so the crash record has to keep it.
            let now: BTreeSet<Hwnd> = targets
                .unfocused
                .iter()
                .copied()
                .chain(self.faded.iter().copied())
                .filter(|&target| manager.is_faded(handle(target)))
                .collect();
            self.sync_fade_record(now);
        }
    }

    /// Mirrors what [`TransparencyManager`] just faded and put back into
    /// [`Hidden`]: newly faded windows are recorded, windows that stopped
    /// being faded are un-recorded.
    fn sync_fade_record(&mut self, now: BTreeSet<Hwnd>) {
        if let Ok(mut hidden) = self.hidden.lock() {
            for &target in &now {
                hidden.fade(target);
            }
            for &target in self.faded.difference(&now) {
                hidden.unfade(target);
            }
        }
        self.faded = now;
    }

    /// Un-records the windows this instance had marked as faded, without
    /// touching the windows themselves. Used once a setting turns
    /// transparency off and [`TransparencyManager::clear_all`] has already put
    /// them back.
    ///
    /// `still_faded` is what `clear_all` could not put back. Those windows are
    /// still translucent, so they stay in the crash record.
    fn unfade_all(&mut self, still_faded: &BTreeSet<Hwnd>) {
        if let Ok(mut hidden) = self.hidden.lock() {
            for &target in self.faded.difference(still_faded) {
                hidden.unfade(target);
            }
        }
        self.faded = still_faded.clone();
    }

    /// The windows this instance faded that `manager` still has faded, because
    /// putting them back failed.
    fn still_faded(
        faded: &BTreeSet<Hwnd>,
        manager: Option<&TransparencyManager>,
    ) -> BTreeSet<Hwnd> {
        let Some(manager) = manager else {
            return BTreeSet::new();
        };
        faded
            .iter()
            .copied()
            .filter(|&target| manager.is_faded(handle(target)))
            .collect()
    }

    /// Forgets what is on screen so the next pass repaints every border.
    ///
    /// The daemon calls this when the displays change. A border's stroke and
    /// corner radius are worked out at the DPI of the screen its rectangle
    /// lands on, so a window carried across a display change without its
    /// rectangle changing still needs repainting, and the per-pass diff would
    /// otherwise see nothing to do and send nothing.
    pub fn invalidate_borders(&self) {
        if let Ok(guard) = self.borders.lock()
            && let Some(manager) = guard.as_ref()
        {
            manager.invalidate();
        }
    }

    /// Clears every visual: destroys the border frames, puts every faded
    /// window back to opaque and drops any animation in flight. Called on
    /// pause and before [`Visuals::stop`].
    pub fn clear(&mut self) {
        if let Ok(guard) = self.borders.lock()
            && let Some(manager) = guard.as_ref()
            && let Err(e) = manager.clear()
        {
            tracing::error!(error = %e, "could not clear borders");
        }
        if let Some(manager) = &mut self.transparency
            && let Err(e) = manager.clear_all()
        {
            tracing::error!(error = %e, "could not clear transparency");
        }
        let still = Self::still_faded(&self.faded, self.transparency.as_ref());
        self.unfade_all(&still);
        if let Some(animator) = &self.animator {
            let _ = animator.cancel_all();
        }
    }

    /// The animation settings the animator is running with.
    #[must_use]
    pub const fn animation(&self) -> &AnimationConfig {
        &self.animation
    }

    /// Whether moves are animated right now.
    #[must_use]
    pub const fn is_animating(&self) -> bool {
        self.animator.is_some()
    }

    /// Clears every visual and stops every worker thread. Call once, on
    /// daemon shutdown.
    pub fn stop(&mut self) {
        self.clear();
        let manager = match self.borders.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(manager) = manager {
            manager.stop();
        }
        self.transparency = None;
        self.animator = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::WindowInfo;

    const FOCUSED: Hwnd = Hwnd(1);
    const OTHER: Hwnd = Hwnd(2);
    const THIRD: Hwnd = Hwnd(3);

    fn rect() -> Rect {
        Rect::new(0, 0, 800, 600)
    }

    /// The pure decision this module makes for a normal tiled pass: the
    /// focused window gets its kind, everything else is `Unfocused` and lands
    /// in the transparency set. This mirrors what `wm.rs` builds before
    /// calling [`Visuals::update`], exercised directly so it can be tested
    /// without a desktop.
    fn classify(focused: Hwnd, focused_kind: BorderKind, rest: &[Hwnd]) -> VisualsTargets {
        let mut tiled = vec![(focused, rect(), focused_kind)];
        let mut unfocused = Vec::new();
        for &w in rest {
            tiled.push((w, rect(), BorderKind::Unfocused));
            unfocused.push(w);
        }
        VisualsTargets {
            focused: Some(focused),
            tiled,
            unfocused,
        }
    }

    #[test]
    fn a_single_window_container_is_single() {
        let targets = classify(FOCUSED, BorderKind::Single, &[]);
        assert_eq!(targets.tiled, vec![(FOCUSED, rect(), BorderKind::Single)]);
        assert!(targets.unfocused.is_empty());
    }

    #[test]
    fn a_stacked_container_is_stack_and_the_rest_is_unfocused() {
        let targets = classify(FOCUSED, BorderKind::Stack, &[OTHER, THIRD]);
        assert_eq!(targets.tiled[0].2, BorderKind::Stack);
        assert_eq!(targets.unfocused, vec![OTHER, THIRD]);
        assert!(
            targets
                .tiled
                .iter()
                .filter(|&&(w, _, _)| w != FOCUSED)
                .all(|&(_, _, kind)| kind == BorderKind::Unfocused)
        );
    }

    #[test]
    fn monocle_leaves_nothing_unfocused() {
        let targets = classify(FOCUSED, BorderKind::Monocle, &[]);
        assert_eq!(targets.tiled[0].2, BorderKind::Monocle);
        assert!(targets.unfocused.is_empty());
    }

    #[test]
    fn a_floating_focus_is_floating() {
        let targets = classify(FOCUSED, BorderKind::Floating, &[OTHER]);
        assert_eq!(targets.tiled[0].2, BorderKind::Floating);
        assert_eq!(targets.unfocused, vec![OTHER]);
    }

    #[test]
    fn handle_and_hwnd_round_trip() {
        let original = Hwnd(0x1234);
        assert_eq!(hwnd_of(handle(original)), original);
    }

    /// A platform that knows one window and records every move it is asked
    /// for, so an animated layout can be inspected without a desktop.
    struct RecordingPlatform {
        window: WindowInfo,
        moves: Mutex<Vec<WindowPlacement>>,
    }

    impl RecordingPlatform {
        fn new(window: WindowInfo) -> Self {
            Self {
                window,
                moves: Mutex::new(Vec::new()),
            }
        }

        /// The first placement the animation produced, waiting for the
        /// animation thread to get round to it.
        fn first_move(&self) -> WindowPlacement {
            for _ in 0..200 {
                if let Some(first) = self.moves.lock().expect("not poisoned").first() {
                    return *first;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("the animation never moved the window");
        }
    }

    impl Platform for RecordingPlatform {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn monitors(&self) -> anyhow::Result<Vec<crate::platform::MonitorInfo>> {
            Ok(Vec::new())
        }

        fn windows(&self) -> anyhow::Result<Vec<WindowInfo>> {
            Ok(vec![self.window.clone()])
        }

        fn window_info(&self, hwnd: Hwnd) -> anyhow::Result<WindowInfo> {
            if hwnd == self.window.hwnd {
                Ok(self.window.clone())
            } else {
                Err(anyhow::anyhow!("no such window"))
            }
        }

        fn foreground_window(&self) -> Option<Hwnd> {
            None
        }

        fn window_at(&self, _x: i32, _y: i32) -> Option<Hwnd> {
            None
        }

        fn cursor_position(&self) -> anyhow::Result<(i32, i32)> {
            Ok((0, 0))
        }

        fn is_maximized(&self, _hwnd: Hwnd) -> bool {
            false
        }
        fn is_on_screen(&self, _hwnd: Hwnd) -> bool {
            true
        }

        fn set_positions(&self, placements: &[WindowPlacement]) -> anyhow::Result<()> {
            self.moves
                .lock()
                .expect("not poisoned")
                .extend_from_slice(placements);
            Ok(())
        }

        fn set_cloaked(&self, _hwnd: Hwnd, _cloaked: bool) -> anyhow::Result<()> {
            Ok(())
        }

        fn show(&self, _hwnd: Hwnd, _state: crate::platform::ShowState) -> anyhow::Result<()> {
            Ok(())
        }

        fn focus(&self, _hwnd: Hwnd) -> anyhow::Result<()> {
            Ok(())
        }

        fn focus_desktop(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn close(&self, _hwnd: Hwnd) -> anyhow::Result<()> {
            Ok(())
        }

        fn set_transparency(&self, _hwnd: Hwnd, _alpha: Option<u8>) -> anyhow::Result<()> {
            Ok(())
        }

        fn set_topmost(&self, _hwnd: Hwnd, _topmost: bool) -> anyhow::Result<()> {
            Ok(())
        }

        fn set_cursor_position(&self, _x: i32, _y: i32) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// The perceived frame of a window whose invisible resize border sticks
    /// out by seven pixels on three sides, which is what an ordinary Windows
    /// window looks like.
    const SEEN: Rect = Rect::new(100, 100, 900, 700);
    const WINDOW_RECT: Rect = Rect::new(93, 100, 907, 707);

    fn animated_config() -> Config {
        serde_json::from_str(
            r#"{"animation":{"enabled":true,"duration":250,"style":"Linear","fps":60}}"#,
        )
        .expect("the animation block has to load")
    }

    /// The first frame of an animated move is where the user already sees the
    /// window. Starting from `GetWindowRect` instead makes the platform
    /// compensate for the invisible border a second time, so every move begins
    /// with the window jumping outward.
    #[test]
    fn an_animated_move_starts_at_the_perceived_frame() {
        let target = Hwnd(0x1234);
        let mut info = WindowInfo::placeholder(target);
        info.rect = WINDOW_RECT;
        info.frame = SEEN;
        let platform = Arc::new(RecordingPlatform::new(info));
        let hidden = Arc::new(Mutex::new(Hidden::default()));
        let mut visuals = Visuals::new(Arc::clone(&platform) as Arc<dyn Platform>, hidden);
        visuals.set_settings(&animated_config());
        assert!(visuals.is_animating(), "the animator has to be running");

        visuals.apply_layout(&[(target, Rect::new(1000, 100, 1800, 700))]);

        let first = platform.first_move();
        assert_eq!(first.hwnd, target);

        // Asserted on the bottom edge, which is timing-independent on purpose.
        // `SEEN` and `WINDOW_RECT` differ by the invisible resize border, and
        // the target shares SEEN's bottom edge, so a move that starts from the
        // perceived frame holds that edge still for the whole animation while
        // one starting from the window rect has to travel the seven pixels.
        // Comparing the first frame to SEEN outright instead would be asserting
        // that the animation thread is scheduled within a millisecond of the
        // command, which is true in practice and is not on a loaded test runner.
        assert_eq!(
            first.rect.bottom, SEEN.bottom,
            "the animation did not start from the perceived frame: it compensated              for the invisible border a second time, so the window jumps outward              and eases back"
        );
        assert_ne!(
            first.rect.bottom, WINDOW_RECT.bottom,
            "it started from the window rect"
        );
        visuals.stop();
    }

    #[test]
    fn a_window_whose_info_cannot_be_read_is_still_placed() {
        // The trap in the optimisation above, and it is a silent one. `from`
        // falls back to `to` when the window cannot be read, so a naive
        // "skip when from == to" would stop placing exactly the windows whose
        // read failed, and they would never be moved again by any layout.
        let known = Hwnd(0x1234);
        let unreadable = Hwnd(0x9999);
        let mut info = WindowInfo::placeholder(known);
        info.rect = WINDOW_RECT;
        info.frame = SEEN;
        let platform = Arc::new(RecordingPlatform::new(info));
        let hidden = Arc::new(Mutex::new(Hidden::default()));
        let mut visuals = Visuals::new(Arc::clone(&platform) as Arc<dyn Platform>, hidden);
        visuals.set_settings(&animated_config());

        let to = Rect::new(1000, 100, 1800, 700);
        visuals.apply_layout(&[(unreadable, to)]);

        let first = platform.first_move();
        assert_eq!(
            first.hwnd, unreadable,
            "a window that could not be read was dropped from the layout"
        );
        assert_eq!(first.rect, to, "and it should be placed at its target");
        visuals.stop();
    }

    #[test]
    fn a_window_already_at_its_target_is_not_animated() {
        // A layout usually moves one or two windows and leaves the rest exactly
        // where they were. Every one of those used to get a full
        // `SWP_FRAMECHANGED` move on every frame, so it recalculated and
        // repainted its whole non-client area for nothing, and
        // `EndDeferWindowPos` blocked on its message pump while it did.
        let target = Hwnd(0x1234);
        let mut info = WindowInfo::placeholder(target);
        info.rect = WINDOW_RECT;
        info.frame = SEEN;
        let platform = Arc::new(RecordingPlatform::new(info));
        let hidden = Arc::new(Mutex::new(Hidden::default()));
        let mut visuals = Visuals::new(Arc::clone(&platform) as Arc<dyn Platform>, hidden);
        visuals.set_settings(&animated_config());

        // Asked for exactly where it already is.
        visuals.apply_layout(&[(target, SEEN)]);
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(
            platform.moves.lock().expect("not poisoned").is_empty(),
            "a window that was not moving was moved anyway"
        );

        // And a window that really does move is still animated.
        visuals.apply_layout(&[(target, Rect::new(1000, 100, 1800, 700))]);
        let first = platform.first_move();
        assert_eq!(first.rect, SEEN, "it starts where the window already is");
        visuals.stop();
    }
}
