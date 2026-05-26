//! dashboard tests.

use super::*;

#[test]
fn da_paren_includes_parens() {
    let mut app = test_app();
    set_buffer(&mut app, "f(a, b) | g");
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(0, 3));
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(key(KeyCode::Char('(')));
    assert_eq!(buffer(&app), "f | g");
}
#[test]
fn dashboard_picker_open_sorts_by_name_case_insensitive() {
    let mut p = DashboardPicker::default();
    p.open(vec![
        dash("1", "zoo", None),
        dash("2", "alpha", None),
        dash("3", "Bravo", None),
    ]);
    let names: Vec<_> = p.items.iter().map(|d| d.name_or_unnamed()).collect();
    assert_eq!(names, vec!["alpha", "Bravo", "zoo"]);
}
#[test]
fn dashboard_picker_empty_filter_returns_all_indices() {
    let mut p = DashboardPicker::default();
    p.open(vec![dash("1", "a", None), dash("2", "b", None)]);
    assert_eq!(p.filtered_indices(), vec![0, 1]);
}
#[test]
fn dashboard_picker_filter_matches_name_and_description() {
    let mut p = DashboardPicker::default();
    p.open(vec![
        dash("1", "Cluster", None),
        dash("2", "Pods", Some("kubernetes pod lifecycle")),
        dash("3", "Other", None),
    ]);
    p.filter = "kub".to_string();
    let hits: Vec<_> = p
        .filtered_indices()
        .iter()
        .map(|i| p.items[*i].name_or_unnamed())
        .collect();
    assert_eq!(hits, vec!["Pods"]);
}
#[test]
fn dashboard_picker_filter_is_case_insensitive() {
    let mut p = DashboardPicker::default();
    p.open(vec![dash("1", "Cluster Overview", None)]);
    p.filter = "CLUSTER".to_string();
    assert_eq!(p.filtered_indices().len(), 1);
}
#[test]
fn dashboard_picker_move_cursor_wraps_within_filtered_set() {
    let mut p = DashboardPicker::default();
    p.open(vec![
        dash("1", "a", None),
        dash("2", "b", None),
        dash("3", "c", None),
    ]);
    assert_eq!(p.move_cursor(1), 1);
    assert_eq!(p.move_cursor(1), 2);
    assert_eq!(p.move_cursor(1), 0); // wraps
    assert_eq!(p.move_cursor(-1), 2); // wraps back
}
#[test]
fn dashboard_picker_hide_clears_filter_and_cursor() {
    let mut p = DashboardPicker::default();
    p.open(vec![dash("1", "a", None)]);
    p.filter = "x".into();
    p.cursor = 5;
    p.visible = true;
    p.hide();
    assert!(!p.visible);
    assert!(p.filter.is_empty());
    assert_eq!(p.cursor, 0);
}
#[test]
fn dashboard_picker_keymap_filters_and_selects() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardsFetched(Ok(vec![
        dash("id-a", "alpha", None),
        dash("id-b", "beta", None),
        dash("id-c", "gamma", None),
    ])));
    assert!(app.dashboards.visible);
    assert_eq!(app.dashboards.items.len(), 3);
    // Type `b` — should filter to `beta` only.
    app.on_key(key(KeyCode::Char('b')));
    let indices = app.dashboards.filtered_indices();
    assert_eq!(indices.len(), 1);
    assert_eq!(app.dashboards.items[indices[0]].name_or_unnamed(), "beta");
    // Press Enter — picker closes, uid is remembered.
    app.on_key(key(KeyCode::Enter));
    assert!(!app.dashboards.visible);
    assert_eq!(app.last_picked_dashboard.as_deref(), Some("id-b"));
}
#[test]
fn dashboard_picker_backspace_removes_one_filter_char() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardsFetched(Ok(vec![dash("1", "a", None)])));
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(key(KeyCode::Char('b')));
    assert_eq!(app.dashboards.filter, "ab");
    app.on_key(key(KeyCode::Backspace));
    assert_eq!(app.dashboards.filter, "a");
}
#[test]
fn dashboard_open_event_loads_resource_and_sets_status() {
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u1".into(),
        id: Some("42".into()),
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("prod overview".into()),
            charts: vec![
                crate::axiom::Chart::Known(crate::axiom::KnownChart::TimeSeries(
                    crate::axiom::ChartBase {
                        id: "c1".into(),
                        name: Some("rps".into()),
                        query: None,
                        extras: Default::default(),
                    },
                )),
                crate::axiom::Chart::Known(crate::axiom::KnownChart::Note(
                    crate::axiom::ChartBase {
                        id: "c2".into(),
                        name: None,
                        query: None,
                        extras: Default::default(),
                    },
                )),
            ],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u1".into(),
        result: Ok(resource),
    });
    assert!(app.loaded_dashboard.is_some());
    assert_eq!(app.last_picked_dashboard.as_deref(), Some("u1"));
    assert!(app.status.contains("prod overview"));
    assert!(app.status.contains("2 chart"));
    assert!(!app.busy);
}
#[test]
fn dashboard_open_adopts_internal_dashboard_and_seeds_mpl_buffer() {
    // When the focused chart has MPL, the editor buffer should
    // become `// @viz <kind>\n<mpl>` so the next :r executes it.
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u1".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("prod".into()),
            charts: vec![crate::axiom::Chart::Known(
                crate::axiom::KnownChart::TimeSeries(crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: Some("rps".into()),
                    query: Some(serde_json::json!({ "mpl": "http_requests:rate" })),
                    extras: Default::default(),
                }),
            )],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u1".into(),
        result: Ok(resource),
    });
    // Focused viz kind reflects the first chart.
    assert_eq!(app.viz_kind, crate::dashboard::VizKind::Line);
    // Buffer seeded with pragma + mpl.
    let buf = app.query_text();
    assert!(buf.contains("// @viz line"), "buffer: {buf:?}");
    assert!(buf.contains("http_requests:rate"), "buffer: {buf:?}");
}
#[test]
fn dashboard_open_with_apl_query_seeds_commented_banner() {
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![crate::axiom::Chart::Known(crate::axiom::KnownChart::Pie(
                crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: Some("by-region".into()),
                    query: Some(serde_json::json!({
                        "apl": "['logs'] | summarize count() by region"
                    })),
                    extras: Default::default(),
                },
            ))],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    let buf = app.query_text();
    assert!(buf.contains("// @viz pie"));
    assert!(buf.contains("APL query"));
    assert!(buf.contains("['logs']"));
}
#[test]
fn dashboard_open_with_no_charts_leaves_buffer_alone() {
    // Empty dashboard — the from_resource adapter inserts a Note
    // placeholder so focused_tile() doesn't panic, but adopt should
    // not stomp the user's existing buffer.
    let mut app = test_app();
    let original_buf = app.query_text();
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("empty".into()),
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    assert_eq!(app.query_text(), original_buf);
}
#[test]
fn dashboard_pane_colon_enters_command_mode_and_esc_returns_to_dashboard() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(app.focus, Pane::Dashboard);
    // `:` from grid view drops into the ex-cmdline…
    app.on_key(key(KeyCode::Char(':')));
    assert_eq!(app.mode, Mode::Command);
    assert!(app.cmdline.buf.is_empty());
    // …and Esc returns focus to the dashboard pane (not the editor).
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.focus, Pane::Dashboard);
}
#[test]
fn dashboard_open_clears_stale_tile_results() {
    // Tile results from a prior dashboard must not bleed into a
    // freshly loaded one. Pre-seed a fake in-flight slot so the test
    // would actually fail if `DashboardOpened` forgot to clear.
    let mut app = test_app();
    app.tile_results.insert("old-id".into(), Default::default());
    assert!(app.tile_results.contains_key("old-id"));
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert!(!app.tile_results.contains_key("old-id"));
}
#[test]
fn dashboard_dirty_clears_on_save_event() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("tile title new");
    assert!(app.dashboard_dirty);
    let resp = crate::axiom::DashboardWriteResponse {
        status: crate::axiom::DashboardWriteStatus::Updated,
        overwritten: Some(false),
        dashboard: app.loaded_dashboard.clone().unwrap(),
    };
    app.handle_event(AppEvent::DashboardSaved {
        uid: "u".into(),
        result: Ok(resp),
    });
    assert!(!app.dashboard_dirty);
}
#[test]
fn dashboard_pane_arrow_keys_navigate_spatially() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(app.selected_chart_idx, 0); // top-left
    app.on_key(key(KeyCode::Right));
    assert_eq!(app.selected_chart_idx, 1); // top-right
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.selected_chart_idx, 3); // bottom-right
    app.on_key(key(KeyCode::Left));
    assert_eq!(app.selected_chart_idx, 2); // bottom-left
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.selected_chart_idx, 0); // back to top-left
}
#[test]
fn dashboard_pane_tab_cycles_in_row_major_order() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    for expected in [1usize, 2, 3, 0] {
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.selected_chart_idx, expected);
    }
}
#[test]
fn dashboard_pane_enter_zooms_into_solo_with_selected_chart() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Move to the bottom-right chart, then Enter.
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.view_mode, ViewMode::Solo);
    assert_eq!(app.focus, Pane::Editor);
    assert!(app.query_text().contains("bottom-right:rate"));
}
#[test]
fn dashboard_pane_esc_returns_focus_to_editor_without_changing_view() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Pane::Editor);
    assert_eq!(app.view_mode, ViewMode::Grid); // not changed
}
#[test]
fn dashboard_round_trip_preserves_extras() {
    // Load → serialise → reload → re-serialise. The two
    // serialised forms must be byte-equal; this catches any field
    // we silently drop on the decode side (which would break
    // PUT round-trip against the real server, since the schema is
    // `additionalProperties: false`).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.axiom.json");
    let dst = dir.path().join("dst.axiom.json");
    std::fs::write(&src, FIXTURE_DASHBOARD_JSON).unwrap();

    let mut app = test_app();
    app.open_file(src).unwrap();
    let first_serialise = app.dashboard_to_json().unwrap();
    app.write_file(Some(dst.clone())).unwrap();

    let mut app2 = test_app();
    app2.open_file(dst).unwrap();
    let second_serialise = app2.dashboard_to_json().unwrap();

    assert_eq!(
        first_serialise, second_serialise,
        "dashboard JSON did not round-trip byte-stably"
    );
    // Extras spot check: unmodelled fields survived.
    let re: serde_json::Value = serde_json::from_str(&second_serialise).unwrap();
    assert_eq!(re["dashboard"]["refreshTime"], 60);
    assert_eq!(re["dashboard"]["schemaVersion"], 2);
    assert_eq!(re["dashboard"]["owner"], "X-AXIOM-EVERYONE");
}
#[test]
fn dashinfo_command_requires_loaded_dashboard() {
    let mut app = test_app();
    app.execute_command("dashinfo");
    assert!(!app.dashinfo_visible);
    assert!(app.status.contains("no dashboard loaded"));
}
#[test]
fn dashinfo_command_toggles_when_dashboard_loaded() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(DashboardSummary {
            uid: "u".into(),
            id: None,
            updated_at: None,
            updated_by: None,
            version: None,
            dashboard: Default::default(),
        }),
    });
    app.execute_command("dashinfo");
    assert!(app.dashinfo_visible);
    // Any key dismisses.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.dashinfo_visible);
}
#[test]
fn dashboard_picker_esc_closes_without_selecting() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardsFetched(Ok(vec![dash("x", "y", None)])));
    assert!(app.dashboards.visible);
    app.on_key(key(KeyCode::Esc));
    assert!(!app.dashboards.visible);
    assert!(app.last_picked_dashboard.is_none());
}
#[test]
fn dashboard_undo_restores_after_cut() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let before_charts: Vec<String> = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .iter()
        .map(|c| c.known_base().id.clone())
        .collect();
    app.on_key(key(KeyCode::Char('x')));
    app.on_key(key(KeyCode::Char('u')));
    let after_charts: Vec<String> = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .iter()
        .map(|c| c.known_base().id.clone())
        .collect();
    assert_eq!(after_charts, before_charts);
}
#[test]
fn dashboard_undo_toggles_redo_on_second_press() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let before = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .len();
    app.on_key(key(KeyCode::Char('x'))); // cut → -1
    app.on_key(key(KeyCode::Char('u'))); // undo  → back to before
    app.on_key(key(KeyCode::Char('u'))); // redo  → -1 again
    let after = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .len();
    assert_eq!(after, before - 1);
}
