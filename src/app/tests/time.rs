//! time tests.

use super::*;
use ::time;

#[test]
fn zoom_promotes_tile_series_into_solo_view_not_sin_demo() {
    // Regression: zooming a tile re-seeded the editor with the
    // tile's MPL but left `app.series` as the sin(x) demo, so
    // the Solo chart pane showed the placeholder until the user
    // hit `:r`. Zoom must promote `tile_results[chart].series`
    // into `app.series` synchronously.
    let mut app = app_with_series(1); // editor starts with sin demo
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let tile_series = vec![crate::chart::Series {
        name: "top-left {h1}".into(),
        tags: vec![("host".into(), "h1".into())],
        points: vec![(0.0, 1.0), (1.0, 2.0)],
        color: crate::chart::color_for(0),
    }];
    app.tile_results.insert(
        "tl".into(),
        TileQueryResult {
            busy: false,
            series: tile_series.clone(),
            error: None,
            trace_id: Some("abc123".into()),
        },
    );
    // tl is selected by default.
    app.zoom_selected_chart();
    assert_eq!(app.view_mode, ViewMode::Solo);
    assert_eq!(app.focus, Pane::Editor);
    // Solo chart now reads tile data, not the sin demo.
    assert_eq!(app.series.len(), 1);
    assert_eq!(app.series[0].tags, tile_series[0].tags);
    assert_eq!(app.series[0].points, tile_series[0].points);
    // Trace id picked up so `:trace` reports the tile's id.
    assert_eq!(app.last_trace_id.as_deref(), Some("abc123"));
    // Legend bookkeeping resized to match new series.
    assert_eq!(app.legend.hidden, vec![false]);
}
#[test]
fn zoom_without_tile_data_clears_series_instead_of_keeping_sin_demo() {
    // No `tile_results` entry (race: zoom before first fetch).
    // Better to clear than to mislead the user with the
    // sin demo labelled as the zoomed tile.
    let mut app = app_with_series(1);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.tile_results.clear();
    app.zoom_selected_chart();
    assert_eq!(app.view_mode, ViewMode::Solo);
    assert!(app.series.is_empty());
    assert!(app.legend.hidden.is_empty());
}
#[test]
fn time_command_no_args_opens_preset_picker() {
    let mut app = test_app();
    app.execute_command("time");
    // Picker opens; default cursor lands on the `1h` row if no
    // preset matches the current window (here `now-1h` isn't in
    // TIME_PRESETS so cursor falls back to 0).
    match app.time.picker {
        Some(TimePickerState::Presets { cursor }) => assert_eq!(cursor, 0),
        other => panic!("expected Presets state, got {other:?}"),
    }
}
#[test]
fn time_command_sets_start_and_end() {
    let mut app = test_app();
    app.execute_command("time now-15m now");
    assert_eq!(app.active_time_range(), ("now-15m".into(), "now".into()));
}
#[test]
fn time_command_single_arg_sets_start_only() {
    let mut app = test_app();
    app.execute_command("time now-7d");
    let (s, e) = app.active_time_range();
    assert_eq!(s, "now-7d");
    assert_eq!(e, "now");
}
#[test]
fn time_command_reset_restores_defaults() {
    let mut app = test_app();
    app.execute_command("time now-15m now-5m");
    app.execute_command("time reset");
    assert_eq!(app.active_time_range(), ("now-1h".into(), "now".into()));
}
#[test]
fn time_command_whitespace_only_args_opens_picker() {
    let mut app = test_app();
    app.execute_command("time   ");
    // split_whitespace yields no args — we treat that as "open the
    // picker", same as bare `:time`.
    assert!(matches!(
        app.time.picker,
        Some(TimePickerState::Presets { .. })
    ));
    // And the active range is untouched.
    assert_eq!(app.active_time_range(), ("now-1h".into(), "now".into()));
}
#[test]
fn time_command_with_loaded_dashboard_mirrors_to_wire_and_dirties() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Sanity: adopt resets the dirty flag.
    app.dashboard_dirty = false;
    app.execute_command("time now-2h now-30m");
    let res = app.loaded_dashboard.as_ref().unwrap();
    assert_eq!(res.dashboard.time_window_start.as_deref(), Some("now-2h"));
    assert_eq!(res.dashboard.time_window_end.as_deref(), Some("now-30m"));
    assert!(
        app.dashboard_dirty,
        "setting :time should dirty the dashboard"
    );
}
#[test]
fn time_picker_no_args_matches_qr_prefixed_preset() {
    // If the dashboard came in with `qr-now-6h` / `qr-now`, the
    // picker should still highlight the `6h` row instead of
    // falling back to cursor 0.
    let mut app = test_app();
    app.time.range = crate::dashboard::TimeRange {
        start: "qr-now-6h".into(),
        end: "qr-now".into(),
    };
    app.execute_command("time");
    match app.time.picker {
        Some(TimePickerState::Presets { cursor }) => {
            // 6h is index 1 in TIME_PRESETS.
            assert_eq!(cursor, 1);
        }
        other => panic!("expected Presets state, got {other:?}"),
    }
}
#[test]
fn time_command_sets_window() {
    let mut app = test_app();
    app.execute_command("time now-5m now");
    assert_eq!(app.active_time_range(), ("now-5m".into(), "now".into()));
}
#[test]
fn time_picker_enter_on_preset_applies_it_and_closes() {
    let mut app = test_app();
    app.execute_command("time");
    // Move to the `6h` preset (index 1) and confirm.
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Enter));
    assert!(app.time.picker.is_none());
    assert_eq!(app.active_time_range(), ("now-12h".into(), "now".into()));
}
#[test]
fn time_picker_custom_row_enter_transitions_to_calendar() {
    let mut app = test_app();
    app.execute_command("time");
    // Jump to the bottom (the synthetic Custom… row) and Enter.
    app.on_key(key(KeyCode::Char('G')));
    app.on_key(key(KeyCode::Enter));
    match &app.time.picker {
        Some(TimePickerState::Custom(p)) => {
            // Seeded to yesterday → today by default; just sanity
            // check that we have a non-default-zero start.
            assert!(p.start <= p.end);
        }
        other => panic!("expected Custom state, got {other:?}"),
    }
}
#[test]
fn time_picker_custom_enter_applies_rfc3339_range() {
    let mut app = test_app();
    app.execute_command("time");
    app.on_key(key(KeyCode::Char('G')));
    app.on_key(key(KeyCode::Enter));
    // Force a deterministic picker so we can assert the exact
    // serialised range.
    app.time.picker = Some(TimePickerState::Custom(CustomRangePicker {
        start: time::Date::from_calendar_date(2024, time::Month::May, 1).unwrap(),
        end: time::Date::from_calendar_date(2024, time::Month::May, 15).unwrap(),
        focus: CustomField::Start,
    }));
    app.on_key(key(KeyCode::Enter));
    assert!(app.time.picker.is_none());
    assert_eq!(
        app.active_time_range(),
        ("2024-05-01T00:00:00Z".into(), "2024-05-15T23:59:59Z".into())
    );
}
#[test]
fn time_picker_custom_swaps_start_and_end_when_inverted() {
    let mut app = test_app();
    app.time.picker = Some(TimePickerState::Custom(CustomRangePicker {
        start: time::Date::from_calendar_date(2024, time::Month::May, 15).unwrap(),
        end: time::Date::from_calendar_date(2024, time::Month::May, 1).unwrap(),
        focus: CustomField::Start,
    }));
    app.on_key(key(KeyCode::Enter));
    // to_range normalises ordering so the API always gets start ≤ end.
    assert_eq!(
        app.active_time_range(),
        ("2024-05-01T00:00:00Z".into(), "2024-05-15T23:59:59Z".into())
    );
}
#[test]
fn time_picker_custom_esc_returns_to_preset_list() {
    let mut app = test_app();
    app.execute_command("time");
    app.on_key(key(KeyCode::Char('G')));
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Esc));
    match app.time.picker {
        Some(TimePickerState::Presets { cursor }) => {
            assert_eq!(cursor, TIME_PRESET_CUSTOM_INDEX);
        }
        other => panic!("expected Presets state, got {other:?}"),
    }
}
#[test]
fn time_picker_custom_arrow_keys_shift_focused_date() {
    let mut app = test_app();
    let start = time::Date::from_calendar_date(2024, time::Month::May, 10).unwrap();
    let end = time::Date::from_calendar_date(2024, time::Month::May, 20).unwrap();
    app.time.picker = Some(TimePickerState::Custom(CustomRangePicker {
        start,
        end,
        focus: CustomField::Start,
    }));
    // l moves the focused (Start) date forward one day.
    app.on_key(key(KeyCode::Char('l')));
    // j moves it forward a week.
    app.on_key(key(KeyCode::Char('j')));
    // Tab switches focus to End.
    app.on_key(key(KeyCode::Tab));
    // h moves End back one day.
    app.on_key(key(KeyCode::Char('h')));
    match &app.time.picker {
        Some(TimePickerState::Custom(p)) => {
            assert_eq!(p.start, start + time::Duration::days(8));
            assert_eq!(p.end, end - time::Duration::days(1));
            assert_eq!(p.focus, CustomField::End);
        }
        other => panic!("expected Custom state, got {other:?}"),
    }
}
