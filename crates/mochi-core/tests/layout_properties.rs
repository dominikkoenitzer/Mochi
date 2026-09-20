//! Property style tests: long random sequences of real commands, checked
//! against invariants after every single step.
//!
//! The random generator is a seeded LCG so a failure always reproduces: the
//! seed is printed with every assertion.

use mochi_core::geometry::{Axis, Direction, Rect};
use mochi_core::layout::{Flip, Layout, MIN_TILE_SIZE, Sizing};
use mochi_core::model::{Container, CycleDirection, Monitor, State, Window, WindowId};

/// The 4K panel on the left and the portrait panel on the right, the pair this
/// crate is written for, plus two awkward shapes.
const AREAS: [Rect; 4] = [
    Rect::new(0, 0, 3840, 2160),
    Rect::new(3840, 0, 4920, 1920),
    Rect::new(-1080, 200, 0, 1000),
    Rect::new(0, 0, 640, 480),
];

/// A small deterministic generator. Nothing here needs a good one.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1))
    }

    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    /// A number in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            self.next_u32() as usize % n
        }
    }

    fn bool(&mut self) -> bool {
        self.next_u32().is_multiple_of(2)
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    /// A resize delta, including the absurd ones a saturated resize command
    /// can store.
    fn delta(&mut self) -> i32 {
        let magnitude = self.pick(&[0, 1, 50, 200, 1_000, 100_000, i32::MAX / 2]);
        if self.bool() { magnitude } else { -magnitude }
    }

    /// A resize delta in the range a few dozen key presses can produce.
    fn small_delta(&mut self) -> i32 {
        let magnitude = self.pick(&[0, 50, 100, 200, 400]);
        if self.bool() { magnitude } else { -magnitude }
    }
}

/// The rectangles must not overlap, must stay inside the area and must be
/// usable as a window rectangle.
fn assert_sane(rects: &[Rect], area: Rect, context: &str) {
    for (i, a) in rects.iter().enumerate() {
        assert!(
            a.width() >= 0 && a.height() >= 0,
            "{context}: rect {i} is inverted: {a:?}"
        );
        assert!(
            area.contains_rect(a),
            "{context}: rect {i} escapes {area:?}: {a:?}"
        );
        for (j, b) in rects.iter().enumerate().skip(i + 1) {
            assert!(
                !a.intersects(b),
                "{context}: rects {i} and {j} overlap: {a:?} {b:?}"
            );
        }
    }
}

/// With no container padding the rectangles have to cover the area exactly.
fn assert_covers(rects: &[Rect], area: Rect, context: &str) {
    let covered: i64 = rects.iter().map(Rect::area).sum();
    assert_eq!(
        covered,
        area.area(),
        "{context}: the tiles do not add up to the area"
    );
}

#[test]
fn every_layout_tiles_exactly_whatever_the_resize_deltas_are() {
    for seed in 0..2_000_u64 {
        let mut rng = Rng::new(seed);
        let area = rng.pick(&AREAS);
        let layout = rng.pick(&Layout::ALL);
        let len = 1 + rng.below(12);
        let flip = Flip {
            horizontal: rng.bool(),
            vertical: rng.bool(),
        };
        let resize: Vec<Option<Rect>> = (0..len)
            .map(|_| {
                if rng.below(3) == 0 {
                    None
                } else {
                    Some(Rect::new(
                        rng.delta(),
                        rng.delta(),
                        rng.delta(),
                        rng.delta(),
                    ))
                }
            })
            .collect();

        let context = format!("seed {seed}, {layout} with {len} in {area:?}");
        let rects = layout.calculate(area, len, 0, flip, &resize);
        assert_eq!(rects.len(), len, "{context}");
        assert_sane(&rects, area, &context);
        assert_covers(&rects, area, &context);
    }
}

/// Every tile keeps at least one pixel in each dimension for the counts and
/// the deltas of ordinary use. Deep splits plus large deltas can still squeeze
/// a region down to a couple of pixels, and nothing can pad a two pixel strip
/// and leave it visible; that is the documented limit of `padded_clamped`.
#[test]
fn container_padding_never_inverts_or_erases_a_tile() {
    for seed in 0..2_000_u64 {
        let mut rng = Rng::new(seed ^ 0x5eed);
        let area = rng.pick(&AREAS[..3]);
        let layout = rng.pick(&Layout::ALL);
        let len = 1 + rng.below(6);
        let padding = rng.pick(&[0, 10, 14, 40]);
        let resize: Vec<Option<Rect>> = (0..len)
            .map(|_| {
                Some(Rect::new(
                    rng.small_delta(),
                    rng.small_delta(),
                    rng.small_delta(),
                    rng.small_delta(),
                ))
            })
            .collect();

        let context = format!("seed {seed}, {layout} with {len} in {area:?}, padding {padding}");
        let rects = layout.calculate(area, len, padding, Flip::NONE, &resize);
        assert_sane(&rects, area, &context);
        for (i, rect) in rects.iter().enumerate() {
            assert!(
                rect.width() >= 1 && rect.height() >= 1,
                "{context}: rect {i} has no pixels left: {rect:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// the same thing, but through the command surface
// ---------------------------------------------------------------------------

const MAIN: Rect = Rect::new(0, 0, 3840, 2160);
const MAIN_WORK_AREA: Rect = Rect::new(0, 0, 3840, 2112);
const SIDE: Rect = Rect::new(3840, 0, 4920, 1920);

fn two_monitors(padding: i32) -> State {
    let mut state = State::new();
    state.default_workspace_padding = padding + 4;
    state.default_container_padding = padding;
    let mut main = Monitor::new(1, MAIN, MAIN_WORK_AREA).with_dpi(144);
    main.ensure_workspaces(9);
    let mut side = Monitor::new(2, SIDE, SIDE).with_dpi(96);
    side.ensure_workspaces(9);
    state.add_monitor(main);
    state.add_monitor(side);
    state
}

/// Everything that has to hold after any command at all.
fn check_state(state: &State, context: &str) {
    let mut seen: Vec<WindowId> = Vec::new();
    for id in state.all_window_ids() {
        assert!(
            !seen.contains(&id),
            "{context}: window {id} is in two places"
        );
        seen.push(id);
    }

    for monitor in 0..state.monitors().len() {
        let count = state.monitors().get(monitor).unwrap().workspaces().len();
        for workspace in 0..count {
            let ws = state.workspace(monitor, workspace).unwrap();
            let containers = ws.containers().len();
            assert_eq!(
                ws.resize_dimensions.len(),
                containers,
                "{context}: monitor {monitor} workspace {workspace} has a stale resize vector"
            );
            for container in ws.containers().iter() {
                assert!(
                    !container.is_empty(),
                    "{context}: an empty container was left in the ring"
                );
            }

            let rects = ws.latest_layout();
            if rects.is_empty() {
                continue;
            }
            assert_eq!(rects.len(), containers, "{context}: layout length");
            let area = state.work_area_for(monitor, workspace).unwrap();
            assert_sane(rects, area, context);
        }
    }

    // Every visible window is listed exactly once.
    let visible = state.visible_window_ids();
    let mut once: Vec<WindowId> = Vec::new();
    for id in &visible {
        assert!(!once.contains(id), "{context}: {id} is shown twice");
        once.push(*id);
    }
}

#[test]
fn random_command_sequences_keep_the_layout_and_the_screen_consistent() {
    for seed in 0..400_u64 {
        let mut rng = Rng::new(seed ^ 0xc0ffee);
        let mut state = two_monitors(if seed % 2 == 0 { 10 } else { 0 });
        let mut next_id = 1_isize;

        // What the daemon believes is on screen, kept up to date purely from
        // the change sets, so a missing show or hide shows up as a mismatch.
        let mut on_screen: Vec<WindowId> = Vec::new();

        for step in 0..120 {
            let context = format!("seed {seed}, step {step}");
            let choice = rng.below(22);
            let direction = rng.pick(&Direction::ALL);
            let cycle = if rng.bool() {
                CycleDirection::Next
            } else {
                CycleDirection::Previous
            };

            let changes = match choice {
                0..=3 => {
                    let window = Window::new(next_id);
                    next_id += 1;
                    on_screen.push(WindowId(next_id - 1));
                    state.add_window(window).unwrap()
                }
                4 => {
                    let ids: Vec<WindowId> = state.all_window_ids().collect();
                    if ids.is_empty() {
                        continue;
                    }
                    let id = ids[rng.below(ids.len())];
                    on_screen.retain(|other| *other != id);
                    state.remove_window(id).unwrap()
                }
                5..=6 => state.focus_direction(direction).unwrap(),
                7..=9 => state.move_direction(direction).unwrap(),
                10 => state.cycle_focus(cycle).unwrap(),
                11 => state.cycle_move(cycle).unwrap(),
                12..=13 => state
                    .resize_axis(
                        if rng.bool() {
                            Axis::Horizontal
                        } else {
                            Axis::Vertical
                        },
                        if rng.bool() {
                            Sizing::Increase
                        } else {
                            Sizing::Decrease
                        },
                    )
                    .unwrap_or_default(),
                14 => state
                    .flip_layout(if rng.bool() {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    })
                    .unwrap(),
                15 => state.cycle_layout(cycle).unwrap(),
                16 => state.focus_workspace(rng.below(9)).unwrap(),
                17 => state.move_to_workspace(rng.below(9), rng.bool()).unwrap(),
                18 => state.toggle_float().unwrap(),
                19 => state.toggle_monocle().unwrap(),
                20 => state.stack(direction).unwrap(),
                _ => state.unstack().unwrap(),
            };

            // Apply the change set the way the daemon does.
            for id in &changes.hide {
                on_screen.retain(|other| other != id);
            }
            for id in &changes.show {
                if !on_screen.contains(id) {
                    on_screen.push(*id);
                }
            }
            for id in &changes.minimize {
                on_screen.retain(|other| other != id);
            }

            for id in &changes.hide {
                assert!(
                    !changes.show.contains(id),
                    "{context}: {id} is shown and hidden at once"
                );
            }
            assert_eq!(
                changes.hide.len(),
                dedup(&changes.hide).len(),
                "{context}: a window is hidden twice"
            );
            assert_eq!(
                changes.show.len(),
                dedup(&changes.show).len(),
                "{context}: a window is shown twice"
            );

            check_state(&state, &context);

            let mut expected = state.visible_window_ids();
            expected.sort_unstable();
            let mut actual = on_screen.clone();
            actual.retain(|id| state.is_managed(*id));
            actual.sort_unstable();
            assert_eq!(
                actual, expected,
                "{context}: the screen and the model disagree"
            );
        }
    }
}

fn dedup(ids: &[WindowId]) -> Vec<WindowId> {
    let mut out: Vec<WindowId> = Vec::new();
    for id in ids {
        if !out.contains(id) {
            out.push(*id);
        }
    }
    out
}

#[test]
fn resizing_is_reversible_and_bounded() {
    // Leaning on the resize key must not drift the layout: the same number of
    // decreases has to bring every boundary back to where it started.
    //
    // Every container is pressed until the layout stops moving, so the run
    // always includes the press the clamp could only half grant. That press is
    // where the drift used to come from, and four presses on five containers
    // never reached it.
    for layout in Layout::ALL {
        for len in 2..=8 {
            let mut state = two_monitors(0);
            state.change_layout(layout).unwrap();
            for id in 1..=len {
                state.add_window(Window::new(id as isize)).unwrap();
            }
            for container in 0..len {
                state
                    .focused_workspace_mut()
                    .unwrap()
                    .focus_container(container);

                for axis in [Axis::Horizontal, Axis::Vertical] {
                    for sizing in [Sizing::Increase, Sizing::Decrease] {
                        let before: Vec<Rect> =
                            state.workspace(0, 0).unwrap().latest_layout().to_vec();

                        let mut presses = 0;
                        while presses < 40 {
                            if state.resize_axis(axis, sizing).unwrap().is_empty() {
                                break;
                            }
                            presses += 1;
                        }
                        for _ in 0..presses {
                            state.resize_axis(axis, sizing.opposite()).unwrap();
                        }

                        let after: Vec<Rect> =
                            state.workspace(0, 0).unwrap().latest_layout().to_vec();
                        assert_eq!(
                            before, after,
                            "{layout} with {len} containers drifted after {presses} \
                             {sizing} presses on container {container} along {axis:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn every_window_on_a_shared_boundary_can_move_it() {
    // A boundary several windows sit on used to take its delta from one
    // representative pair of containers, so the resize keypress of every other
    // window on that boundary did nothing at all.
    //
    // A window with an edge inside the area has a boundary to push on that
    // side, so at least one of its two edges has to be able to move something.
    // A window that spans the whole area along an axis has no boundary there
    // and is skipped: Columns has nothing to resize vertically.
    const AREA: Rect = AREAS[0];
    const NUDGE: i32 = 200;

    for layout in [
        Layout::Grid,
        Layout::VerticalStack,
        Layout::HorizontalStack,
        Layout::UltrawideVerticalStack,
    ] {
        for len in 1..=9 {
            let base = layout.calculate(AREA, len, 0, Flip::NONE, &[]);
            for window in 0..len {
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let touches_a_boundary = base[window].start(axis) > AREA.start(axis)
                        || base[window].end(axis) < AREA.end(axis);
                    if !touches_a_boundary {
                        continue;
                    }

                    let moved = [
                        (true, NUDGE),
                        (true, -NUDGE),
                        (false, NUDGE),
                        (false, -NUDGE),
                    ]
                    .into_iter()
                    .any(|(far_edge, nudge)| {
                        let delta = match (axis, far_edge) {
                            (Axis::Horizontal, true) => Rect::new(0, 0, nudge, 0),
                            (Axis::Horizontal, false) => Rect::new(nudge, 0, 0, 0),
                            (Axis::Vertical, true) => Rect::new(0, 0, 0, nudge),
                            (Axis::Vertical, false) => Rect::new(0, nudge, 0, 0),
                        };
                        let mut resize = vec![None; len];
                        resize[window] = Some(delta);
                        layout.calculate(AREA, len, 0, Flip::NONE, &resize) != base
                    });

                    assert!(
                        moved,
                        "{layout} with {len} windows: no delta on either {axis:?} edge \
                         of window {window} moves anything"
                    );
                }
            }
        }
    }
}

#[test]
fn a_stack_never_loses_a_window() {
    let mut state = two_monitors(10);
    for id in 1..=4 {
        state.add_window(Window::new(id)).unwrap();
    }

    // Pile everything into the leftmost container, one window at a time.
    for id in [2, 3, 4] {
        state.focus_window(WindowId(id)).unwrap();
        assert!(
            state.stack(Direction::Left).unwrap().retiled.len() == 1,
            "window {id} had nothing to its left"
        );
    }
    let container = state.focused_container().cloned().unwrap();
    assert_eq!(container.len(), 4);
    assert_eq!(state.all_window_ids().count(), 4);

    // The visible one is the only visible one.
    assert_eq!(state.visible_window_ids().len(), 1);

    // Peeling them off again gives every window its own tile. Unstacking
    // follows the window it peeled off, so the stack has to be focused again
    // for every step.
    state.unstack().unwrap();
    for id in [1, 2] {
        state.focus_window(WindowId(id)).unwrap();
        state.unstack().unwrap();
    }
    assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 4);
    assert_eq!(state.visible_window_ids().len(), 4);
    let ids: Vec<WindowId> = state
        .workspace(0, 0)
        .unwrap()
        .containers()
        .iter()
        .filter_map(Container::focused_window_id)
        .collect();
    assert_eq!(dedup(&ids).len(), 4);
}

#[test]
fn no_random_resize_sequence_shrinks_a_tile_below_the_minimum() {
    // Every layout, every container count the hotkeys can produce, and a long
    // random run of resize presses on random containers: no tile may end up
    // thinner than the minimum while the work area has room for them all.
    for layout in Layout::ALL {
        for len in 1..=12_usize {
            for seed in 0..6_u64 {
                let mut rng = Rng::new(seed ^ 0x5e51_2e00 ^ len as u64);
                let mut state = two_monitors(0);
                state.default_workspace_padding = 0;
                state.change_layout(layout).unwrap();
                for id in 1..=len {
                    state.add_window(Window::new(id as isize)).unwrap();
                }

                for step in 0..60 {
                    state
                        .focused_workspace_mut()
                        .unwrap()
                        .focus_container(rng.below(len));
                    let axis = rng.pick(&[Axis::Horizontal, Axis::Vertical]);
                    let sizing = rng.pick(&[Sizing::Increase, Sizing::Decrease]);
                    state.resize_axis(axis, sizing).ok();

                    let context =
                        format!("{layout} with {len} containers, seed {seed}, step {step}");
                    let rects = state.workspace(0, 0).unwrap().latest_layout();
                    for (i, rect) in rects.iter().enumerate() {
                        assert!(
                            rect.width() >= MIN_TILE_SIZE && rect.height() >= MIN_TILE_SIZE,
                            "{context}: tile {i} is {}x{}, below the {MIN_TILE_SIZE} px minimum",
                            rect.width(),
                            rect.height()
                        );
                    }
                    check_state(&state, &context);
                }
            }
        }
    }
}
