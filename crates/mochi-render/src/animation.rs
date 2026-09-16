//! The animation driver.
//!
//! The daemon knows where a window is and where it should end up; this module
//! turns that into a series of intermediate rectangles on a timer thread and
//! hands each frame back in one callback, so the daemon can push a whole frame
//! through a single `DeferWindowPos` batch.
//!
//! The curves and the configuration block both come from `mochi-core`:
//! [`AnimationStyle`] is the enum the config file names and
//! [`AnimationConfig`] is the `animation` block itself. [`AnimationConfigExt`]
//! adds the two things a driver needs on top of it, a [`Duration`] and a frame
//! rate a timer thread can actually serve.
//!
//! Timing is kept out of the thread on purpose: [`Timeline`] is a pure state
//! machine driven by an [`Instant`] the caller passes in, which is what the
//! tests do.
//!
//! ```no_run
//! use mochi_render::{AnimationConfig, AnimationConfigExt, Animator, Rect, WindowHandle};
//!
//! # fn main() -> mochi_render::Result<()> {
//! let config = AnimationConfig::default();
//! let animator = Animator::new(|frame: &[mochi_render::FrameUpdate]| {
//!     // one DeferWindowPos batch per frame
//!     for update in frame {
//!         let _ = (update.handle, update.rect, update.finished);
//!     }
//! })?;
//!
//! animator.animate(vec![config.job(
//!     WindowHandle(0x1234),
//!     Rect::new(0, 0, 800, 600),
//!     Rect::new(100, 100, 900, 700),
//! )])?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mochi_core::Rect;
use mochi_core::animation::AnimationStyle;
use mochi_core::config::AnimationConfig;

use crate::geometry::lerp_rect;
use crate::{RenderError, Result, WindowHandle};

/// The lowest frame rate worth running, and the highest one worth allowing.
const FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=1000;

/// What a driver needs from the `animation` block of the config file.
///
/// [`AnimationConfig`] answers what the config file says; these three answer
/// what the timer thread has to do about it.
pub trait AnimationConfigExt {
    /// The configured duration as a [`Duration`].
    #[must_use]
    fn duration(&self) -> Duration;

    /// The configured frame rate, clamped to something a timer thread can
    /// serve. [`AnimationConfig::fps`] reports the raw number.
    #[must_use]
    fn frame_rate(&self) -> u32;

    /// A job for one window, using this configuration.
    #[must_use]
    fn job(&self, handle: WindowHandle, from: Rect, to: Rect) -> AnimationJob;
}

impl AnimationConfigExt for AnimationConfig {
    fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms())
    }

    fn frame_rate(&self) -> u32 {
        self.fps().clamp(*FPS_RANGE.start(), *FPS_RANGE.end())
    }

    fn job(&self, handle: WindowHandle, from: Rect, to: Rect) -> AnimationJob {
        AnimationJob {
            handle,
            from,
            to,
            duration: self.duration(),
            easing: self.style(),
            fps: self.frame_rate(),
        }
    }
}

/// What the interpolation has to know about a curve.
pub trait AnimationStyleExt {
    /// `true` for the curves that leave `0.0..=1.0` on the way, so a caller
    /// that wants to clamp knows which ones it would ruin. It is why the driver
    /// interpolates with [`crate::geometry::lerp_rect`] instead of
    /// [`Rect::lerp`], which clamps.
    #[must_use]
    fn overshoots(self) -> bool;
}

impl AnimationStyleExt for AnimationStyle {
    fn overshoots(self) -> bool {
        matches!(
            self,
            AnimationStyle::EaseInBack
                | AnimationStyle::EaseOutBack
                | AnimationStyle::EaseInOutBack
                | AnimationStyle::EaseInElastic
                | AnimationStyle::EaseOutElastic
                | AnimationStyle::EaseInOutElastic
        )
    }
}

/// One window on its way from `from` to `to`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationJob {
    /// The window to move. Also the identity of the job: a second job for the
    /// same window replaces the first.
    pub handle: WindowHandle,
    /// Where the window is now.
    pub from: Rect,
    /// Where it should end up.
    pub to: Rect,
    /// How long to take.
    pub duration: Duration,
    /// Which curve to follow.
    pub easing: AnimationStyle,
    /// How many frames per second this job wants.
    pub fps: u32,
}

impl AnimationJob {
    /// A job with an explicit duration, curve and frame rate.
    #[must_use]
    pub fn new(
        handle: WindowHandle,
        from: Rect,
        to: Rect,
        duration: Duration,
        easing: AnimationStyle,
        fps: u32,
    ) -> Self {
        Self {
            handle,
            from,
            to,
            duration,
            easing,
            fps: fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end()),
        }
    }
}

/// Where one window should be for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameUpdate {
    /// The window to move.
    pub handle: WindowHandle,
    /// Where to put it.
    pub rect: Rect,
    /// `true` on the last frame of this window's job, and on that frame only:
    /// the job is dropped as it is reported, so an arriving window is flushed
    /// exactly once. The daemon can use it to write the rect back into its
    /// model, or to end a `DeferWindowPos` batch with the real target rather
    /// than an interpolated one.
    pub finished: bool,
}

/// A job in flight.
#[derive(Debug, Clone, Copy)]
struct Running {
    from: Rect,
    to: Rect,
    start: Instant,
    duration: Duration,
    easing: AnimationStyle,
    fps: u32,
}

/// The pure timing state machine behind [`Animator`].
///
/// Holds one job per window and turns a point in time into a batch of frame
/// updates. Everything here is deterministic: no threads, no clock of its own.
#[derive(Debug, Default)]
pub struct Timeline {
    jobs: HashMap<isize, Running>,
    last_frame: Option<Instant>,
}

impl Timeline {
    /// An empty timeline.
    #[must_use]
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            last_frame: None,
        }
    }

    /// Adds a job, replacing any job already running for that window.
    ///
    /// Replacing is how cancellation works: a new target for a window that is
    /// still moving simply takes over, starting from wherever the daemon says
    /// the window is now.
    pub fn insert(&mut self, job: AnimationJob, now: Instant) {
        self.jobs.insert(
            job.handle.0,
            Running {
                from: job.from,
                to: job.to,
                start: now,
                duration: job.duration,
                easing: job.easing,
                fps: job.fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end()),
            },
        );
        // Render the first frame of a new job immediately rather than waiting
        // out the rest of the current frame interval.
        self.last_frame = None;
    }

    /// Drops the job for one window, if any. Returns `true` when there was one.
    pub fn cancel(&mut self, handle: WindowHandle) -> bool {
        self.jobs.remove(&handle.0).is_some()
    }

    /// Drops every job.
    pub fn clear(&mut self) {
        self.jobs.clear();
        self.last_frame = None;
    }

    /// How many jobs are in flight.
    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// `true` when there is nothing to animate.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// The gap between frames, taken from the fastest job in flight.
    ///
    /// One timer serves every job, so a 144 fps job drags a 60 fps job along
    /// with it. That is cheaper than one timer per window and no window ever
    /// gets fewer frames than it asked for.
    #[must_use]
    pub fn frame_interval(&self) -> Duration {
        let fps = self.jobs.values().map(|job| job.fps).max().unwrap_or(60);
        let fps = fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end());
        Duration::from_secs_f64(1.0 / f64::from(fps))
    }

    /// How long to wait before producing the next frame.
    #[must_use]
    pub fn time_until_next_frame(&self, now: Instant) -> Duration {
        match self.last_frame {
            None => Duration::ZERO,
            Some(last) => (last + self.frame_interval()).saturating_duration_since(now),
        }
    }

    /// `true` when a frame is due.
    #[must_use]
    pub fn is_due(&self, now: Instant) -> bool {
        !self.is_empty() && self.time_until_next_frame(now).is_zero()
    }

    /// Produces the frame for `now` and drops the jobs that have arrived.
    ///
    /// Every job in flight contributes exactly one update, so the daemon gets
    /// the whole frame in one callback and can push it through a single
    /// `DeferWindowPos` batch. A job that has arrived is reported once, with
    /// `finished` set and the exact target rectangle, and is then gone.
    pub fn tick(&mut self, now: Instant) -> Vec<FrameUpdate> {
        self.last_frame = Some(now);

        let mut updates = Vec::with_capacity(self.jobs.len());
        let mut finished = Vec::new();

        for (handle, job) in &self.jobs {
            let elapsed = now.saturating_duration_since(job.start);
            let progress = if job.duration.is_zero() {
                1.0
            } else {
                (elapsed.as_secs_f64() / job.duration.as_secs_f64()).clamp(0.0, 1.0)
            };
            let done = progress >= 1.0;

            let rect = if done {
                job.to
            } else {
                lerp_rect(job.from, job.to, job.easing.evaluate(progress))
            };

            updates.push(FrameUpdate {
                handle: WindowHandle(*handle),
                rect,
                finished: done,
            });
            if done {
                finished.push(*handle);
            }
        }

        for handle in finished {
            self.jobs.remove(&handle);
        }

        // A stable order keeps the frames the daemon applies reproducible, and
        // makes the tests independent of the hash seed.
        updates.sort_unstable_by_key(|update| update.handle.0);
        updates
    }
}

/// What the driver thread can be told to do.
enum Command {
    Animate(Vec<AnimationJob>),
    Cancel(WindowHandle),
    CancelAll,
    Stop,
}

/// The thread behind the handle, so that dropping the last [`Animator`] stops
/// the timer.
struct Inner {
    sender: Mutex<Sender<Command>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let handle = self
            .join
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            {
                let sender = self
                    .sender
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = sender.send(Command::Stop);
            }
            if handle.join().is_err() {
                tracing::warn!("the animation thread panicked");
            }
        }
    }
}

/// A handle to the animation thread.
///
/// Cloning is cheap; the thread stops when the last clone is dropped.
#[derive(Clone)]
pub struct Animator {
    inner: Arc<Inner>,
}

impl Animator {
    /// Starts the animation thread.
    ///
    /// `apply` is called once per frame with every window that moved in it, on
    /// the animation thread. It must be quick and must not block: the daemon
    /// wraps `DeferWindowPos` here and hands the same slice to
    /// [`crate::border::BorderManager::follow_frame`], which is what makes the
    /// borders ride along. The crate documentation has the whole flow.
    ///
    /// # Errors
    ///
    /// [`RenderError::ThreadStart`] when the thread cannot be spawned.
    pub fn new<F>(mut apply: F) -> Result<Self>
    where
        F: FnMut(&[FrameUpdate]) + Send + 'static,
    {
        let (sender, receiver) = channel::<Command>();
        let join = std::thread::Builder::new()
            .name("mochi-animation".to_string())
            .spawn(move || run(&receiver, &mut apply))
            .map_err(|error| RenderError::ThreadStart("animation", error.to_string()))?;

        Ok(Self {
            inner: Arc::new(Inner {
                sender: Mutex::new(sender),
                join: Mutex::new(Some(join)),
            }),
        })
    }

    /// Starts or replaces the jobs for these windows.
    ///
    /// A window that is already moving picks up the new target from wherever
    /// the job says it is now, so a burst of layout changes does not queue up.
    ///
    /// # Errors
    ///
    /// [`RenderError::ThreadGone`] when the animation thread has stopped.
    pub fn animate(&self, jobs: Vec<AnimationJob>) -> Result<()> {
        self.send(Command::Animate(jobs))
    }

    /// Stops animating one window. It stays wherever the last frame put it.
    ///
    /// # Errors
    ///
    /// [`RenderError::ThreadGone`] when the animation thread has stopped.
    pub fn cancel(&self, handle: WindowHandle) -> Result<()> {
        self.send(Command::Cancel(handle))
    }

    /// Stops every animation, for example when the daemon is told to pause.
    ///
    /// # Errors
    ///
    /// [`RenderError::ThreadGone`] when the animation thread has stopped.
    pub fn cancel_all(&self) -> Result<()> {
        self.send(Command::CancelAll)
    }

    fn send(&self, command: Command) -> Result<()> {
        let sender = self
            .inner
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sender
            .send(command)
            .map_err(|_| RenderError::ThreadGone("animation"))
    }
}

impl std::fmt::Debug for Animator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Animator").finish_non_exhaustive()
    }
}

/// The timer thread: wait for work, then produce frames until the work is done.
fn run<F>(receiver: &Receiver<Command>, apply: &mut F)
where
    F: FnMut(&[FrameUpdate]),
{
    let mut timeline = Timeline::new();

    loop {
        let command = if timeline.is_empty() {
            receiver.recv().ok()
        } else {
            match receiver.recv_timeout(timeline.time_until_next_frame(Instant::now())) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };

        if let Some(command) = command {
            if handle_command(&mut timeline, command) {
                return;
            }
            // Take anything else that is already queued before drawing, so a
            // burst of layout changes costs one frame, not one frame each.
            while let Ok(queued) = receiver.try_recv() {
                if handle_command(&mut timeline, queued) {
                    return;
                }
            }
        }

        let now = Instant::now();
        if timeline.is_due(now) {
            let frame = timeline.tick(now);
            if !frame.is_empty() {
                apply(&frame);
            }
        }
    }
}

/// Applies one command. Returns `true` when the thread should stop.
fn handle_command(timeline: &mut Timeline, command: Command) -> bool {
    match command {
        Command::Animate(jobs) => {
            let now = Instant::now();
            for job in jobs {
                timeline.insert(job, now);
            }
            false
        }
        Command::Cancel(handle) => {
            timeline.cancel(handle);
            false
        }
        Command::CancelAll => {
            timeline.clear();
            false
        }
        Command::Stop => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: WindowHandle = WindowHandle(0x1111);
    const B: WindowHandle = WindowHandle(0x2222);

    fn job(handle: WindowHandle, from: Rect, to: Rect, ms: u64) -> AnimationJob {
        AnimationJob::new(
            handle,
            from,
            to,
            Duration::from_millis(ms),
            AnimationStyle::Linear,
            60,
        )
    }

    /// The `animation` block of the real config file.
    fn rice() -> AnimationConfig {
        serde_json::from_str(r#"{"enabled":true,"duration":250,"style":"EaseOutQuad","fps":60}"#)
            .expect("the animation block has to load")
    }

    #[test]
    fn a_job_walks_from_start_to_end() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(
            job(
                A,
                Rect::new(0, 0, 100, 100),
                Rect::new(200, 0, 300, 100),
                100,
            ),
            start,
        );

        let first = timeline.tick(start);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].rect, Rect::new(0, 0, 100, 100));
        assert!(!first[0].finished);

        let middle = timeline.tick(start + Duration::from_millis(50));
        assert_eq!(middle[0].rect, Rect::new(100, 0, 200, 100));
        assert!(!middle[0].finished);

        let last = timeline.tick(start + Duration::from_millis(100));
        assert_eq!(last[0].rect, Rect::new(200, 0, 300, 100));
        assert!(last[0].finished);
        assert!(timeline.is_empty(), "a finished job is dropped");
    }

    #[test]
    fn a_finished_job_is_flushed_exactly_once() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(job(A, Rect::default(), Rect::new(1, 1, 2, 2), 100), start);
        timeline.insert(job(B, Rect::default(), Rect::new(1, 1, 2, 2), 400), start);

        let done = start + Duration::from_millis(100);
        let frame = timeline.tick(done);
        assert_eq!(frame.len(), 2);
        assert!(frame[0].finished, "A has arrived");
        assert!(!frame[1].finished, "B is still going");

        // However often the thread ticks afterwards, A is never reported again.
        for step in 1..=5 {
            let frame = timeline.tick(done + Duration::from_millis(step));
            assert!(
                frame.iter().all(|update| update.handle != A),
                "A was flushed twice"
            );
        }
        assert_eq!(timeline.len(), 1);
    }

    #[test]
    fn the_last_frame_is_exactly_the_target() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        let to = Rect::new(37, 91, 1337, 999);
        timeline.insert(
            AnimationJob::new(
                A,
                Rect::new(0, 0, 10, 10),
                to,
                Duration::from_millis(250),
                AnimationStyle::EaseOutElastic,
                60,
            ),
            start,
        );
        let frame = timeline.tick(start + Duration::from_secs(5));
        assert_eq!(frame[0].rect, to);
        assert!(frame[0].finished);
    }

    #[test]
    fn a_zero_length_job_finishes_on_the_first_frame() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(
            job(A, Rect::new(0, 0, 10, 10), Rect::new(5, 5, 15, 15), 0),
            start,
        );
        let frame = timeline.tick(start);
        assert_eq!(frame[0].rect, Rect::new(5, 5, 15, 15));
        assert!(frame[0].finished);
        assert!(timeline.is_empty());
    }

    #[test]
    fn a_new_target_cancels_the_old_one() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(
            job(
                A,
                Rect::new(0, 0, 100, 100),
                Rect::new(400, 0, 500, 100),
                100,
            ),
            start,
        );
        let _ = timeline.tick(start + Duration::from_millis(50));

        // Half way through, the layout changes again.
        let restart = start + Duration::from_millis(50);
        timeline.insert(
            job(
                A,
                Rect::new(200, 0, 300, 100),
                Rect::new(0, 0, 100, 100),
                100,
            ),
            restart,
        );
        assert_eq!(timeline.len(), 1, "one job per window, not two");

        let frame = timeline.tick(restart);
        assert_eq!(frame[0].rect, Rect::new(200, 0, 300, 100));
        let frame = timeline.tick(restart + Duration::from_millis(100));
        assert_eq!(frame[0].rect, Rect::new(0, 0, 100, 100));
        assert!(frame[0].finished);
    }

    #[test]
    fn every_window_of_a_frame_comes_in_one_batch() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(
            job(
                A,
                Rect::new(0, 0, 100, 100),
                Rect::new(100, 0, 200, 100),
                100,
            ),
            start,
        );
        timeline.insert(
            job(B, Rect::new(0, 0, 50, 50), Rect::new(0, 50, 50, 100), 200),
            start,
        );

        let frame = timeline.tick(start + Duration::from_millis(50));
        assert_eq!(frame.len(), 2, "one callback carries the whole frame");
        assert_eq!(
            frame[0].handle, A,
            "sorted, so the daemon sees a stable order"
        );
        assert_eq!(frame[1].handle, B);
        assert_eq!(frame[0].rect, Rect::new(50, 0, 150, 100));
        assert_eq!(
            frame[1].rect,
            Rect::new(0, 13, 50, 63),
            "a quarter of the way, rounded"
        );

        // A finishes first; B keeps going on its own.
        let frame = timeline.tick(start + Duration::from_millis(100));
        assert!(frame[0].finished);
        assert!(!frame[1].finished);
        assert_eq!(timeline.len(), 1);
    }

    #[test]
    fn cancelling_removes_just_that_window() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(job(A, Rect::default(), Rect::new(1, 1, 2, 2), 100), start);
        timeline.insert(job(B, Rect::default(), Rect::new(1, 1, 2, 2), 100), start);

        assert!(timeline.cancel(A));
        assert!(!timeline.cancel(A), "cancelling twice is not an error");
        assert_eq!(timeline.len(), 1);

        timeline.clear();
        assert!(timeline.is_empty());
    }

    #[test]
    fn the_frame_rate_follows_the_fastest_job() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        assert_eq!(
            timeline.frame_interval(),
            Duration::from_secs_f64(1.0 / 60.0)
        );

        timeline.insert(
            AnimationJob::new(
                A,
                Rect::default(),
                Rect::new(1, 1, 2, 2),
                Duration::from_millis(100),
                AnimationStyle::Linear,
                30,
            ),
            start,
        );
        assert_eq!(
            timeline.frame_interval(),
            Duration::from_secs_f64(1.0 / 30.0)
        );

        timeline.insert(
            AnimationJob::new(
                B,
                Rect::default(),
                Rect::new(1, 1, 2, 2),
                Duration::from_millis(100),
                AnimationStyle::Linear,
                144,
            ),
            start,
        );
        assert_eq!(
            timeline.frame_interval(),
            Duration::from_secs_f64(1.0 / 144.0),
            "the fastest job sets the pace for the batch"
        );
    }

    #[test]
    fn a_silly_frame_rate_is_clamped() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(
            AnimationJob::new(
                A,
                Rect::default(),
                Rect::new(1, 1, 2, 2),
                Duration::from_millis(100),
                AnimationStyle::Linear,
                0,
            ),
            start,
        );
        assert_eq!(timeline.frame_interval(), Duration::from_secs(1));
        assert_eq!(
            AnimationConfig {
                fps: Some(0),
                ..Default::default()
            }
            .frame_rate(),
            1
        );
        assert_eq!(
            AnimationConfig {
                fps: Some(100_000),
                ..Default::default()
            }
            .frame_rate(),
            1000
        );
    }

    #[test]
    fn frames_are_paced_by_the_interval() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(job(A, Rect::default(), Rect::new(1, 1, 2, 2), 100), start);

        // A fresh job renders at once.
        assert!(timeline.is_due(start));
        assert_eq!(timeline.time_until_next_frame(start), Duration::ZERO);

        let _ = timeline.tick(start);
        assert!(!timeline.is_due(start), "not two frames in one interval");
        let interval = timeline.frame_interval();
        assert_eq!(timeline.time_until_next_frame(start), interval);
        assert!(timeline.is_due(start + interval));

        // Waiting longer than one interval does not queue frames up.
        assert_eq!(
            timeline.time_until_next_frame(start + interval * 3),
            Duration::ZERO
        );
    }

    #[test]
    fn an_empty_timeline_is_never_due() {
        let timeline = Timeline::new();
        assert!(!timeline.is_due(Instant::now()));
        assert!(timeline.is_empty());
        assert_eq!(timeline.len(), 0);
    }

    #[test]
    fn easing_shapes_the_path() {
        let start = Instant::now();
        let mut timeline = Timeline::new();
        timeline.insert(
            AnimationJob::new(
                A,
                Rect::new(0, 0, 100, 100),
                Rect::new(1000, 0, 1100, 100),
                Duration::from_millis(100),
                AnimationStyle::EaseOutQuad,
                60,
            ),
            start,
        );
        let frame = timeline.tick(start + Duration::from_millis(50));
        // EaseOutQuad is three quarters of the way there at the half way point.
        assert_eq!(frame[0].rect.left, 750);
    }

    #[test]
    fn only_the_back_and_elastic_curves_overshoot() {
        for style in AnimationStyle::ALL {
            let leaves_the_range = (0..=100)
                .map(|step| style.evaluate(f64::from(step) / 100.0))
                .any(|value| !(-1e-9..=1.0 + 1e-9).contains(&value));
            assert_eq!(
                style.overshoots(),
                leaves_the_range,
                "{style} is misfiled, and the driver must never clamp an overshooting curve"
            );
        }
    }

    #[test]
    fn the_config_builds_jobs_from_the_rice_block() {
        let config = rice();
        assert!(config.is_enabled());
        assert_eq!(config.duration(), Duration::from_millis(250));
        assert_eq!(config.style(), AnimationStyle::EaseOutQuad);
        assert_eq!(config.frame_rate(), 60);

        let job = config.job(A, Rect::default(), Rect::new(1, 1, 2, 2));
        assert_eq!(job.handle, A);
        assert_eq!(job.duration, Duration::from_millis(250));
        assert_eq!(job.easing, AnimationStyle::EaseOutQuad);
        assert_eq!(job.fps, 60);
    }

    #[test]
    fn an_empty_config_block_still_builds_a_job() {
        let config = AnimationConfig::default();
        assert!(
            !config.is_enabled(),
            "animation is off until it is asked for"
        );
        let job = config.job(A, Rect::default(), Rect::new(1, 1, 2, 2));
        assert_eq!(job.duration, Duration::from_millis(250));
        assert_eq!(job.easing, AnimationStyle::Linear);
        assert_eq!(job.fps, 60);
    }

    #[test]
    fn the_driver_thread_runs_a_whole_animation() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel::<Vec<FrameUpdate>>();
        let animator = Animator::new(move |frame: &[FrameUpdate]| {
            let _ = tx.send(frame.to_vec());
        })
        .unwrap();

        let config = AnimationConfig {
            duration: Some(60),
            ..rice()
        };
        animator
            .animate(vec![config.job(
                A,
                Rect::new(0, 0, 100, 100),
                Rect::new(300, 0, 400, 100),
            )])
            .unwrap();

        let mut frames = Vec::new();
        while let Ok(frame) = rx.recv_timeout(Duration::from_secs(2)) {
            let finished = frame.iter().any(|update| update.finished);
            frames.push(frame);
            if finished {
                break;
            }
        }

        assert!(
            frames.len() >= 2,
            "expected several frames, got {}",
            frames.len()
        );
        let last = frames.last().expect("at least one frame");
        assert_eq!(last[0].rect, Rect::new(300, 0, 400, 100));
        assert!(last[0].finished);
    }

    #[test]
    fn cancelling_stops_the_frames() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel::<Vec<FrameUpdate>>();
        let animator = Animator::new(move |frame: &[FrameUpdate]| {
            let _ = tx.send(frame.to_vec());
        })
        .unwrap();

        let config = AnimationConfig {
            duration: Some(2000),
            ..rice()
        };
        animator
            .animate(vec![config.job(
                A,
                Rect::new(0, 0, 100, 100),
                Rect::new(300, 0, 400, 100),
            )])
            .unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(1));
        animator.cancel_all().unwrap();

        // Drain whatever was already in flight, then expect silence.
        while rx.recv_timeout(Duration::from_millis(120)).is_ok() {}
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
