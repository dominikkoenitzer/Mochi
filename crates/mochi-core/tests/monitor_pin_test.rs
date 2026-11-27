// Two identical panels, one pinned by rectangle: each pin holds in any entry order.
use mochi_core::config::Config;
use mochi_core::{Monitor, Rect, State};

#[test]
fn a_rectangle_pin_wins_over_an_earlier_model_name_pin() {
    let config = Config::from_json(
        r#"{
            "display_index_preferences": { "0": "Generic PnP Monitor" },
            "monitor_index_preferences": { "1": { "left": 0, "top": 0, "right": 1920, "bottom": 1080 } },
            "monitors": [
                { "workspaces": [{ "name": "one", "layout": "BSP" }] },
                { "workspaces": [{ "name": "two", "layout": "Rows" }] }
            ]
        }"#,
    )
    .unwrap();
    let mut state = State::new();
    state.add_monitor(
        Monitor::new(1, Rect::new(0, 0, 1920, 1080), Rect::new(0, 0, 1920, 1040))
            .with_device(r"\\.\DISPLAY1", "Generic PnP Monitor"),
    );
    state.add_monitor(
        Monitor::new(
            2,
            Rect::new(1920, 0, 3840, 1080),
            Rect::new(1920, 0, 3840, 1040),
        )
        .with_device(r"\\.\DISPLAY2", "Generic PnP Monitor"),
    );

    assert_eq!(config.monitor_assignments(&state), vec![Some(1), Some(0)]);
}
