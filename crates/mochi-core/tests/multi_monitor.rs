//! Two monitors of different size and DPI, the way the author actually runs
//! Mochi: a 4K display at 150 percent on the left and a 1080x1920 portrait
//! panel at 100 percent on the right.
//!
//! Everything here goes through the public API only, so it doubles as a check
//! that the crate is usable from the daemon without reaching inside.

use mochi_core::geometry::{Direction, Rect};
use mochi_core::model::{Container, Monitor, MoveBehaviour, State, Window, WindowId};

/// The 4K panel: 3840x2160, 150 percent scaling, 48 pixels of taskbar.
const MAIN: Rect = Rect::new(0, 0, 3840, 2160);
const MAIN_WORK_AREA: Rect = Rect::new(0, 0, 3840, 2112);
/// The portrait panel to its right: 1080x1920, 100 percent scaling.
const SIDE: Rect = Rect::new(3840, 0, 4920, 1920);

fn two_monitors() -> State {
    let mut state = State::new();
    state.default_workspace_padding = 14;
    state.default_container_padding = 10;

    let mut main = Monitor::new(1, MAIN, MAIN_WORK_AREA)
        .with_name("DISPLAY1")
        .with_dpi(144);
    main.ensure_workspaces(9);

    let mut side = Monitor::new(2, SIDE, SIDE)
        .with_name("DISPLAY2")
        .with_dpi(96);
    side.ensure_workspaces(9);

    state.add_monitor(main);
    state.add_monitor(side);
    state
}

fn container_order(state: &State, monitor: usize, workspace: usize) -> Vec<WindowId> {
    state
        .workspace(monitor, workspace)
        .unwrap()
        .containers()
        .iter()
        .filter_map(Container::focused_window_id)
        .collect()
}

/// Puts `count` windows on the given workspace without focusing it.
fn fill(state: &mut State, monitor: usize, workspace: usize, ids: &[isize]) {
    for id in ids {
        state
            .add_window_to(monitor, workspace, Window::new(*id))
            .unwrap();
    }
}

#[test]
fn the_two_monitors_are_side_by_side() {
    let state = two_monitors();
    assert_eq!(state.monitor_idx_in_direction(Direction::Right), Some(1));
    assert!(state.monitors().get(1).unwrap().is_portrait());
    assert!((state.monitors().get(0).unwrap().scale_factor() - 1.5).abs() < f32::EPSILON);
}

#[test]
fn focus_right_from_the_rightmost_window_lands_on_the_portrait_monitor() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1, 2, 3]);
    fill(&mut state, 1, 0, &[50, 51]);
    state.focus_window(WindowId(3)).unwrap();

    // Window 3 is the bottom right tile of the 4K BSP layout.
    let changes = state.focus_direction(Direction::Right).unwrap();
    assert_eq!(state.focused_monitor_idx(), 1);
    assert!(changes.focused_monitor_changed);
    assert_eq!(state.focused_window_id(), Some(WindowId(51)));

    // Left again walks the portrait monitor first...
    state.focus_direction(Direction::Left).unwrap();
    assert_eq!(state.focused_monitor_idx(), 1);
    assert_eq!(state.focused_window_id(), Some(WindowId(50)));

    // ...and only crosses back from its leftmost window.
    let changes = state.focus_direction(Direction::Left).unwrap();
    assert_eq!(state.focused_monitor_idx(), 0);
    assert!(changes.focused_monitor_changed);
    assert_eq!(
        state.focused_window_id(),
        Some(WindowId(3)),
        "the monitor remembers which container was focused"
    );
}

#[test]
fn focus_up_and_down_inside_the_portrait_monitor() {
    let mut state = two_monitors();
    fill(&mut state, 1, 0, &[50, 51, 52]);
    state.focus_monitor(1).unwrap();
    state.focus_window(WindowId(50)).unwrap();

    // BSP on a portrait panel still cuts vertically first, so 50 is on the
    // left, 51 top right and 52 bottom right.
    state.focus_direction(Direction::Right).unwrap();
    assert_eq!(state.focused_window_id(), Some(WindowId(51)));
    state.focus_direction(Direction::Down).unwrap();
    assert_eq!(state.focused_window_id(), Some(WindowId(52)));
    state.focus_direction(Direction::Up).unwrap();
    assert_eq!(state.focused_window_id(), Some(WindowId(51)));
}

#[test]
fn focus_stops_at_the_outer_edges_instead_of_wrapping() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1, 2]);
    fill(&mut state, 1, 0, &[50]);

    state.focus_window(WindowId(1)).unwrap();
    assert!(state.focus_direction(Direction::Left).unwrap().is_empty());
    assert_eq!(state.focused_monitor_idx(), 0, "no wrap round the desktop");

    state.focus_window(WindowId(50)).unwrap();
    assert!(state.focus_direction(Direction::Right).unwrap().is_empty());
    assert_eq!(state.focused_monitor_idx(), 1);
    assert!(state.focus_direction(Direction::Up).unwrap().is_empty());
    assert!(state.focus_direction(Direction::Down).unwrap().is_empty());
}

#[test]
fn insert_puts_the_window_at_the_edge_it_came_in_through() {
    let mut state = two_monitors();
    state.cross_monitor_move_behaviour = MoveBehaviour::Insert;
    fill(&mut state, 0, 0, &[1, 2]);
    fill(&mut state, 1, 0, &[50, 51, 52]);
    // Focus something in the middle of the portrait monitor, so "next to the
    // focused container" and "at the edge" cannot be confused.
    state.focus_window(WindowId(51)).unwrap();

    state.focus_window(WindowId(2)).unwrap();
    state.move_direction(Direction::Right).unwrap();
    assert_eq!(
        container_order(&state, 1, 0),
        vec![WindowId(2), WindowId(50), WindowId(51), WindowId(52)],
        "a window entering from the left joins at the left end"
    );

    // And back: entering the 4K monitor from the right joins at the right end.
    state.move_direction(Direction::Left).unwrap();
    assert_eq!(
        container_order(&state, 0, 0),
        vec![WindowId(1), WindowId(2)],
        "a window entering from the right joins at the right end"
    );
}

#[test]
fn insert_upwards_and_downwards_uses_the_same_rule() {
    let mut state = two_monitors();
    state.cross_monitor_move_behaviour = MoveBehaviour::Insert;
    // Stack the portrait monitor underneath the 4K one for this test.
    let below = Rect::new(0, 2160, 1080, 4080);
    state.monitors_mut().get_mut(1).unwrap().size = below;
    state.monitors_mut().get_mut(1).unwrap().work_area = below;

    fill(&mut state, 0, 0, &[1, 2]);
    fill(&mut state, 1, 0, &[50, 51]);
    state.focus_window(WindowId(2)).unwrap();

    state.move_direction(Direction::Down).unwrap();
    assert_eq!(
        container_order(&state, 1, 0),
        vec![WindowId(2), WindowId(50), WindowId(51)],
        "entering from the top joins at the front"
    );

    state.move_direction(Direction::Up).unwrap();
    assert_eq!(
        container_order(&state, 0, 0),
        vec![WindowId(1), WindowId(2)],
        "entering from the bottom joins at the back"
    );
}

#[test]
fn swap_trades_places_with_the_focused_container_of_the_other_monitor() {
    let mut state = two_monitors();
    state.cross_monitor_move_behaviour = MoveBehaviour::Swap;
    fill(&mut state, 0, 0, &[1, 2, 3]);
    fill(&mut state, 1, 0, &[50, 51, 52]);

    // The portrait monitor is focused on its last container.
    state.focus_window(WindowId(52)).unwrap();
    state.focus_window(WindowId(2)).unwrap();
    assert_eq!(state.workspace(0, 0).unwrap().focused_container_idx(), 1);

    state.move_direction(Direction::Right).unwrap();

    assert_eq!(
        container_order(&state, 1, 0),
        vec![WindowId(50), WindowId(51), WindowId(2)],
        "the moved window takes the exact slot of the one it swapped with"
    );
    assert_eq!(
        container_order(&state, 0, 0),
        vec![WindowId(1), WindowId(52), WindowId(3)],
        "and the other window takes the slot that was vacated"
    );
    assert_eq!(state.focused_window_id(), Some(WindowId(2)));
    assert_eq!(state.focused_monitor_idx(), 1);
}

#[test]
fn moving_to_a_workspace_on_the_same_monitor_never_swaps() {
    let mut state = two_monitors();
    state.cross_monitor_move_behaviour = MoveBehaviour::Swap;
    fill(&mut state, 0, 0, &[1, 2]);
    fill(&mut state, 0, 3, &[30, 31]);

    state.focus_window(WindowId(2)).unwrap();
    state.move_to_workspace(3, false).unwrap();

    assert_eq!(state.locate_window(WindowId(2)), Some((0, 3)));
    assert_eq!(
        container_order(&state, 0, 3).len(),
        3,
        "cross_monitor_move_behaviour only applies across monitors"
    );
    assert_eq!(container_order(&state, 0, 0), vec![WindowId(1)]);
}

#[test]
fn a_window_opening_on_a_hidden_workspace_is_hidden_and_keeps_its_hands_off_the_focus() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1]);

    let changes = state.add_window_to(0, 5, Window::new(60)).unwrap();
    assert!(
        changes.hide.contains(&WindowId(60)),
        "a window on an invisible workspace has to be taken off screen: {changes:?}"
    );
    assert_ne!(
        changes.focus,
        Some(WindowId(60)),
        "and it must not steal the foreground from the visible workspace"
    );
    assert!(!state.visible_window_ids().contains(&WindowId(60)));
}

#[test]
fn a_window_opening_behind_a_monocle_is_hidden_too() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1]);
    state.toggle_monocle().unwrap();

    let changes = state.add_window(Window::new(2)).unwrap();
    assert!(state.workspace(0, 0).unwrap().is_monocle());
    assert!(
        changes.hide.contains(&WindowId(2)),
        "the monocle still covers the screen, so the newcomer is hidden: {changes:?}"
    );
}

#[test]
fn the_focused_window_of_a_hidden_workspace_survives_a_round_trip() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1, 2, 3]);
    state.focus_window(WindowId(2)).unwrap();
    state.toggle_monocle().unwrap();
    assert!(state.workspace(0, 0).unwrap().is_monocle());

    state.focus_workspace(4).unwrap();
    assert!(state.workspace(0, 0).unwrap().is_monocle(), "still monocle");

    let changes = state.focus_workspace(0).unwrap();
    assert!(state.workspace(0, 0).unwrap().is_monocle());
    assert_eq!(state.focused_window_id(), Some(WindowId(2)));
    assert_eq!(changes.show, vec![WindowId(2)]);
    assert_eq!(state.visible_window_ids(), vec![WindowId(2)]);
}

#[test]
fn a_maximized_window_survives_a_workspace_round_trip() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1, 2]);
    state.toggle_maximize().unwrap();
    assert!(state.workspace(0, 0).unwrap().is_maximized());

    state.focus_workspace(1).unwrap();
    let changes = state.focus_workspace(0).unwrap();
    assert!(state.workspace(0, 0).unwrap().is_maximized());
    assert_eq!(state.focused_window_id(), Some(WindowId(2)));
    assert_eq!(changes.show, vec![WindowId(2)]);
}

#[test]
fn floating_windows_follow_their_workspace() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1, 2]);
    state.toggle_float().unwrap();
    assert_eq!(
        state.workspace(0, 0).unwrap().floating_windows().len(),
        1,
        "window 2 floats"
    );

    let changes = state.focus_workspace(1).unwrap();
    assert!(changes.hide.contains(&WindowId(2)), "{changes:?}");
    assert!(!state.visible_window_ids().contains(&WindowId(2)));

    let changes = state.focus_workspace(0).unwrap();
    assert!(changes.show.contains(&WindowId(2)));
    assert!(state.visible_window_ids().contains(&WindowId(2)));
}

#[test]
fn moving_the_last_window_out_of_a_workspace_leaves_it_empty_and_refocuses() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1]);
    fill(&mut state, 0, 1, &[10]);

    state.move_to_workspace(1, false).unwrap();
    assert!(state.workspace(0, 0).unwrap().is_empty());
    assert_eq!(state.focused_window_id(), None);
    assert_eq!(state.focused_indices().unwrap(), (0, 0));

    // And with follow, the focus lands on the window that moved.
    state.focus_workspace(1).unwrap();
    state.focus_window(WindowId(1)).unwrap();
    state.move_to_workspace(2, true).unwrap();
    assert_eq!(state.focused_indices().unwrap(), (0, 2));
    assert_eq!(state.focused_window_id(), Some(WindowId(1)));
}

#[test]
fn focus_last_workspace_goes_back_to_where_a_move_came_from() {
    let mut state = two_monitors();
    fill(&mut state, 0, 0, &[1, 2]);
    state.move_to_workspace(6, true).unwrap();
    assert_eq!(state.focused_indices().unwrap(), (0, 6));

    state.focus_last_workspace().unwrap();
    assert_eq!(state.focused_indices().unwrap(), (0, 0));
    assert_eq!(state.focused_window_id(), Some(WindowId(1)));
}
