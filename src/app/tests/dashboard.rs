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
fn dashboard_open_with_logstream_apl_seeds_raw_apl_text() {
    // Since APL execution landed, APL tiles seed as raw editable
    // text (no `//` comment prefix). The language is tracked via
    // the chart's `axLang` sidecar / `App.buffer_lang` and
    // surfaced in the status bar; the buffer itself is just the
    // APL query the user can type into directly.
    //
    // Pie / TimeSeries / etc. with APL text in the `apl` key
    // still dispatch as MPL (kind-based fallback) and surface
    // server errors, unless the user runs `:apl` to flip the
    // sidecar.
    use crate::dashboard::Lang;
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![crate::axiom::Chart::Known(
                crate::axiom::KnownChart::LogStream(crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: Some("recent-errors".into()),
                    query: Some(serde_json::json!({
                        "apl": "['logs'] | where severity == 'error'"
                    })),
                    extras: Default::default(),
                }),
            )],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    let buf = app.query_text();
    assert!(buf.contains("// @viz log_stream"), "buffer: {buf:?}");
    assert!(
        !buf.contains("// APL query"),
        "old comment-banner must be gone: {buf:?}"
    );
    // Raw APL text, no `//` line prefix.
    assert!(
        buf.contains("['logs'] | where severity == 'error'"),
        "buffer: {buf:?}"
    );
    // Language discriminator is APL for a LogStream chart.
    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    assert_eq!(crate::dashboard::extract_lang(chart), Some(Lang::Apl));
}

#[test]
fn dashboard_open_with_apl_text_on_pie_seeds_as_plain_text() {
    // A Pie chart with APL syntax in its `apl` key now seeds as
    // plain text — the user can see and edit it directly. The
    // fetcher will hit the metrics endpoint and surface the
    // server's rejection (e.g. "unknown function summarize") in
    // the tile's error slot, which is strictly more informative
    // than the old "not yet executable" banner.
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
    // No banner — the user gets the raw query text.
    assert!(!buf.contains("APL query"));
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
        .map(|c| c.base().expect("test fixture is Chart::Known").id.clone())
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
        .map(|c| c.base().expect("test fixture is Chart::Known").id.clone())
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

// ── editor ↔ focused-tile sync (plan/19 follow-up) ────────────────────
//
// In dashboard mode the editor buffer is a live view onto the focused
// tile's MPL. Navigating between tiles re-seeds the buffer; edits flow
// back into the focused tile so `:w` saves a coherent state.

/// Helper: pull the MPL string out of `loaded_dashboard.charts[idx]`.
/// Panics if the chart is missing or the query isn't MPL.
fn tile_mpl(app: &App, idx: usize) -> String {
    let chart = &app
        .loaded_dashboard
        .as_ref()
        .expect("loaded dashboard")
        .dashboard
        .charts[idx];
    let q = chart
        .base()
        .expect("test fixture is Chart::Known")
        .query
        .as_ref()
        .expect("chart has query");
    q.get("mpl")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .expect("query has `mpl` key")
}

#[test]
fn dashboard_nav_reseeds_editor_to_focused_tile() {
    // Spatial nav must re-seed the editor from whichever tile is now
    // focused — otherwise the diagnostic shown in the status bar
    // refers to a buffer that doesn't belong to anything on screen.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert!(app.query_text().contains("top-left:rate"));
    app.on_key(key(KeyCode::Right));
    assert!(
        app.query_text().contains("top-right:rate"),
        "after Right nav, buffer: {:?}",
        app.query_text()
    );
    app.on_key(key(KeyCode::Down));
    assert!(app.query_text().contains("bottom-right:rate"));
    app.on_key(key(KeyCode::Left));
    assert!(app.query_text().contains("bottom-left:rate"));
}

#[test]
fn dashboard_nav_reseeds_clears_stale_mpl_diagnostic() {
    // The reported user bug: status bar shows a "1:1: MPL syntax error"
    // from a buffer that has nothing to do with the loaded dashboard.
    // After re-seeding from the focused tile, the editor contains the
    // tile's MPL and the diagnostics reflect that tile — not whatever
    // was in the buffer before.
    use crate::axiom::{Chart, ChartBase, KnownChart, LayoutItem};
    let mk = |id: &str, mpl: &str| {
        Chart::Known(KnownChart::TimeSeries(ChartBase {
            id: id.into(),
            name: Some(id.into()),
            query: Some(serde_json::json!({ "mpl": mpl })),
            extras: Default::default(),
        }))
    };
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("clean".into()),
            charts: vec![mk("a", "home:temp"), mk("b", "home:humidity")],
            layout: vec![
                LayoutItem {
                    i: "a".into(),
                    x: 0,
                    y: Some(0),
                    w: 6,
                    h: 6,
                    extras: Default::default(),
                },
                LayoutItem {
                    i: "b".into(),
                    x: 6,
                    y: Some(0),
                    w: 6,
                    h: 6,
                    extras: Default::default(),
                },
            ],
            ..Default::default()
        },
    };
    let mut app = test_app();
    // Pre-populate the editor with garbage MPL so we know the
    // re-seed actually replaced it.
    set_buffer(&mut app, "definitely not valid <<< mpl");
    assert!(app.diagnostics.iter().any(|d| d.severity.is_error()));
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    // Adoption seeds from the first chart; diagnostics should now be clean.
    assert!(
        app.diagnostics.iter().all(|d| !d.severity.is_error()),
        "diagnostics after adopt: {:?}",
        app.diagnostics
            .iter()
            .map(|d| &d.message)
            .collect::<Vec<_>>()
    );
    // And navigating to another tile keeps them clean.
    app.on_key(key(KeyCode::Right));
    assert!(app.diagnostics.iter().all(|d| !d.severity.is_error()));
}

/// Test helper: zoom into the currently focused dashboard tile and
/// drop into Insert mode at end-of-buffer (last line, end of line).
/// Mirrors what a user does to start editing a tile.
fn zoom_and_enter_insert(app: &mut App) {
    app.on_key(key(KeyCode::Enter)); // zoom → Solo + focus Editor
    assert_eq!(app.focus, Pane::Editor, "zoom failed to focus editor");
    app.on_key(key(KeyCode::Char('i'))); // → Insert mode
    assert_eq!(app.mode, Mode::Insert, "failed to enter Insert mode");
    app.editor.move_cursor(tui_textarea::CursorMove::Bottom);
    app.editor.move_cursor(tui_textarea::CursorMove::End);
}

#[test]
fn dashboard_buffer_edit_writes_back_to_focused_tile() {
    // Typing in the editor while a dashboard tile is focused must
    // mutate that tile's MPL. The dirty flag flips so `:w` is required
    // before quit, and the new MPL appears in the serialised dashboard.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert!(!app.dashboard_dirty);
    assert_eq!(tile_mpl(&app, 0), "top-left:rate");
    zoom_and_enter_insert(&mut app);
    type_text(&mut app, " | rate(1m)");
    app.on_key(key(KeyCode::Esc));
    let mpl = tile_mpl(&app, 0);
    assert!(mpl.ends_with(" | rate(1m)"), "tile MPL after edit: {mpl:?}");
    assert!(app.dashboard_dirty);
}

#[test]
fn dashboard_buffer_edit_leaves_unfocused_tiles_untouched() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let untouched_before = tile_mpl(&app, 1);
    zoom_and_enter_insert(&mut app);
    type_text(&mut app, "X");
    app.on_key(key(KeyCode::Esc));
    assert_ne!(tile_mpl(&app, 0), "top-left:rate"); // focused tile changed
    assert_eq!(tile_mpl(&app, 1), untouched_before); // sibling untouched
}

#[test]
fn dashboard_nav_then_back_preserves_edits_per_tile() {
    // Edit tile 0, navigate to tile 1, navigate back to tile 0. Tile
    // 0's edit must still be there — i.e. nav fully round-trips edits
    // through the wire chart.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    zoom_and_enter_insert(&mut app);
    type_text(&mut app, " | A");
    app.on_key(key(KeyCode::Esc));
    let edited = tile_mpl(&app, 0);
    assert!(edited.ends_with(" | A"), "tile 0 MPL: {edited:?}");
    // Back to Grid to navigate between tiles, then re-check.
    app.execute_command("grid");
    app.on_key(key(KeyCode::Right)); // → tile 1
    assert!(app.query_text().contains("top-right:rate"));
    app.on_key(key(KeyCode::Left)); // ← back to tile 0
    assert!(
        app.query_text().contains(" | A"),
        "buffer after round-trip: {:?}",
        app.query_text()
    );
    // The wire chart still has the edit too.
    assert_eq!(tile_mpl(&app, 0), edited);
}

#[test]
fn dashboard_apl_tile_edits_do_not_overwrite_apl() {
    // APL tiles are seeded with a commented "APL query — execution
    // lands in step 14b" banner. Editing that banner must NOT push
    // garbage into the chart's `apl` field.
    use crate::axiom::{Chart, ChartBase, KnownChart};
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![Chart::Known(KnownChart::Pie(ChartBase {
                id: "c1".into(),
                name: Some("by-region".into()),
                query: Some(serde_json::json!({
                    "apl": "['logs'] | summarize count() by region"
                })),
                extras: Default::default(),
            }))],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    let original_apl = app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0]
        .base()
        .expect("test fixture is Chart::Known")
        .query
        .as_ref()
        .unwrap()
        .get("apl")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    // Single-chart dashboard auto-switches to Solo + focus Editor;
    // no need to zoom.
    assert_eq!(app.focus, Pane::Editor);
    app.on_key(key(KeyCode::Char('i')));
    app.editor.move_cursor(tui_textarea::CursorMove::End);
    type_text(&mut app, " GARBAGE");
    app.on_key(key(KeyCode::Esc));
    let after_apl = app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0]
        .base()
        .expect("test fixture is Chart::Known")
        .query
        .as_ref()
        .unwrap()
        .get("apl")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert_eq!(after_apl, original_apl, "APL must not be overwritten");
}

#[test]
fn dashboard_quit_after_adopt_is_clean() {
    // `:q` on a freshly adopted dashboard must succeed: the editor
    // buffer was overwritten by the seed, but no actual dashboard
    // edit has happened. Without this guard, every dashboard would
    // refuse to close until forced with `:q!`.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert!(!app.is_dirty(), "clean adopt should not be dirty");
    app.execute_command("q");
    assert!(app.should_quit, "`:q` after clean adopt must quit");
    assert!(app.last_error.is_none(), "`:q` raised: {:?}", app.status);
}

#[test]
fn dashboard_quit_after_nav_is_clean() {
    // Navigating between tiles re-seeds the editor (changing
    // `query_text()`) but does not dirty the dashboard. `:q` must
    // still succeed.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Down));
    assert!(!app.is_dirty(), "nav alone should not be dirty");
    app.execute_command("q");
    assert!(app.should_quit);
}

#[test]
fn dashboard_quit_after_edit_blocks_without_bang() {
    // After a real edit, `:q` must block (E37) and `:q!` must force.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    zoom_and_enter_insert(&mut app);
    type_text(&mut app, " | rate(1m)");
    app.on_key(key(KeyCode::Esc));
    assert!(app.dashboard_dirty);
    assert!(app.is_dirty(), "edit must mark dashboard dirty");
    app.execute_command("q");
    assert!(!app.should_quit, "`:q` must block when dirty");
    assert!(
        app.last_error
            .as_ref()
            .map(|e| e.contains("E37"))
            .unwrap_or(false),
        "expected E37 error, got: {:?}",
        app.last_error
    );
    app.dismiss_error();
    app.execute_command("q!");
    assert!(app.should_quit, "`:q!` must force-quit");
}

#[test]
fn dashboard_wq_server_loaded_waits_for_save_event_before_quit() {
    // Server-loaded dashboard `:wq` must NOT quit synchronously — the
    // PUT is async and the runtime drops on main-loop exit, so an
    // immediate quit can abort the in-flight HTTP request and the
    // user's edits are lost. Instead we arm `quit_after_save` and let
    // the `DashboardSaved` event handler trigger the quit on success.
    use crate::axiom::{DashboardWriteResponse, DashboardWriteStatus};
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert!(app.current_file.is_none());
    zoom_and_enter_insert(&mut app);
    type_text(&mut app, " | rate(1m)");
    app.on_key(key(KeyCode::Esc));
    assert!(app.dashboard_dirty);

    // Fake a successful dispatch by arming the flag directly. (The
    // real path goes through `put_loaded_dashboard`, which requires
    // an HTTP client we don't have in tests.) This isolates the
    // "quit-on-save" contract from the dispatch plumbing.
    app.quit_after_save = true;
    assert!(!app.should_quit, "flag alone must not quit");

    let resp = DashboardWriteResponse {
        status: DashboardWriteStatus::Updated,
        overwritten: Some(false),
        dashboard: app.loaded_dashboard.clone().unwrap(),
    };
    app.handle_event(AppEvent::DashboardSaved {
        uid: "u".into(),
        result: Ok(resp),
    });
    assert!(
        app.should_quit,
        "successful DashboardSaved with armed flag must quit"
    );
    assert!(!app.quit_after_save, "flag must be consumed");
}

#[test]
fn dashboard_wq_save_error_clears_flag_and_keeps_running() {
    // If the server save fails (412 conflict, network error, etc.),
    // `:wq` must surface the error AND keep the app running so the
    // user can retry. The flag must be cleared so a later successful
    // save doesn't ghost-quit.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.quit_after_save = true;
    app.handle_event(AppEvent::DashboardSaved {
        uid: "u".into(),
        result: Err(anyhow::anyhow!("412 version conflict")),
    });
    assert!(!app.should_quit, "failed save must NOT quit");
    assert!(!app.quit_after_save, "failed save must clear the flag");
    assert!(
        app.last_error.is_some(),
        "failed save must surface an error"
    );
}

#[test]
fn dashboard_wq_no_dispatch_does_not_arm_quit() {
    // When the PUT can't be dispatched (busy gate, no client, no
    // dashboard, etc.), `:wq` MUST NOT arm `quit_after_save` — no
    // `DashboardSaved` event will ever arrive and the app would hang
    // forever waiting for one. We force the busy gate here since the
    // dev machine has a working axiom.toml and the no-client path
    // can't be reproduced reliably.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.busy = true; // make `fetch_prepare` short-circuit
    app.execute_command("wq");
    assert!(!app.should_quit, "failed-dispatch `:wq` must not quit");
    assert!(
        !app.quit_after_save,
        "failed-dispatch `:wq` must not arm quit_after_save (would hang)"
    );
}

#[test]
fn dashboard_wq_writes_file_and_quits() {
    // `:wq` on a file-backed dashboard must write the JSON to disk
    // AND set `should_quit`. This is the synchronous path (no
    // network); failures here would mean the command surface isn't
    // honouring the standard vim contract for `:wq`.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dash.axiom.json");

    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Stash the dashboard at `path` so subsequent :wq writes there.
    app.write_file(Some(path.clone())).unwrap();
    assert_eq!(app.current_file.as_deref(), Some(path.as_path()));

    // Edit a tile so the dashboard is dirty.
    zoom_and_enter_insert(&mut app);
    type_text(&mut app, " | rate(1m)");
    app.on_key(key(KeyCode::Esc));
    assert!(app.dashboard_dirty);

    // Now :wq must write the edits to `path` and quit.
    app.execute_command("wq");
    assert!(
        app.should_quit,
        "`:wq` failed to quit (status: {:?})",
        app.status
    );
    assert!(!app.dashboard_dirty, "`:wq` must clear dirty flag");
    assert!(
        !app.quit_after_save,
        "file-based sync `:wq` must not touch quit_after_save"
    );

    // Re-load `path` in a fresh app to verify the edits actually
    // landed on disk.
    let mut app2 = test_app();
    app2.open_file(path).unwrap();
    assert!(
        tile_mpl(&app2, 0).ends_with(" | rate(1m)"),
        "persisted MPL after :wq: {:?}",
        tile_mpl(&app2, 0)
    );
}

#[test]
fn dashboard_save_after_edit_captures_edited_mpl() {
    // End-to-end: edit a tile, write the dashboard to disk, re-load,
    // verify the on-disk JSON has the edited MPL. This is the user's
    // headline scenario: "writing the dashboard" should persist what
    // they see on screen.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dash.axiom.json");

    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    zoom_and_enter_insert(&mut app);
    type_text(&mut app, " | rate(1m)");
    app.on_key(key(KeyCode::Esc));
    app.write_file(Some(path.clone())).unwrap();

    let mut app2 = test_app();
    app2.open_file(path).unwrap();
    assert!(
        tile_mpl(&app2, 0).ends_with(" | rate(1m)"),
        "persisted tile MPL: {:?}",
        tile_mpl(&app2, 0)
    );
}

// ---------- Forward-compat / concurrency regression tests --------------

/// Regression for the `DashboardRefreshed` clobber bug: when a
/// background refresh lands and the user has unsaved edits, the
/// editor body AND the optimistic-concurrency `version` must stay
/// pinned to the user's adopted snapshot. Bumping the local version
/// to match the server would silently defeat the next `:w`'s
/// 412 check and overwrite the other writer's changes.
#[test]
fn dashboard_refresh_with_dirty_edits_keeps_version_and_body() {
    let mut app = test_app();
    let mut original = multi_chart_resource();
    original.version = Some(7);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(original.clone()),
    });
    // Dirty the dashboard: rename the focused tile, which sets
    // `dashboard_dirty = true` and mutates `charts[0].name`.
    app.execute_command("tile title renamed");
    assert!(app.dashboard_dirty);
    let dirty_names: Vec<Option<String>> = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .iter()
        .map(|c| c.base().and_then(|b| b.name.clone()))
        .collect();

    // Server-side: someone else updated the dashboard, bumping
    // version to 9 and changing chart names. A background refresh
    // delivers this fresh snapshot.
    let mut fresh = multi_chart_resource();
    fresh.version = Some(9);
    for (i, c) in fresh.dashboard.charts.iter_mut().enumerate() {
        if let Some(b) = c.base_mut() {
            b.name = Some(format!("server-renamed-{i}"));
        }
    }
    app.handle_event(AppEvent::DashboardRefreshed {
        uid: "u".into(),
        result: Ok(fresh),
    });

    let after = app.loaded_dashboard.as_ref().unwrap();
    // Version MUST remain at the user's adopted snapshot (7), not
    // the server's latest (9) — otherwise the next `:w` would
    // silently clobber the server's changes.
    assert_eq!(
        after.version,
        Some(7),
        "refresh on dirty must not bump local version"
    );
    // Body MUST be the user's edited copy, not the server's fresh
    // one. `axiom_rs::dashboards::Chart` doesn't implement
    // `PartialEq`, so compare names instead — the rename is the
    // observable edit, and the server's payload uses
    // `"server-renamed-*"`, so any swap would be visible here.
    let after_names: Vec<Option<String>> = after
        .dashboard
        .charts
        .iter()
        .map(|c| c.base().and_then(|b| b.name.clone()))
        .collect();
    assert_eq!(
        after_names, dirty_names,
        "refresh on dirty must not replace edited charts"
    );
    assert!(app.dashboard_dirty, "dirty flag must persist");
}

/// Regression for the `Chart::Unknown` panic: a future Axiom chart
/// variant arrives as `Chart::Unknown(raw_json)`. Adopting such a
/// dashboard, navigating onto an Unknown tile, and re-serialising
/// must NOT panic — the raw JSON must round-trip via `serde`.
#[test]
fn dashboard_with_chart_unknown_survives_adopt_and_save() {
    use crate::axiom::{Chart, ChartBase, KnownChart, LayoutItem};
    // Build a dashboard with one Known tile and one Unknown tile.
    let known = Chart::Known(KnownChart::TimeSeries(ChartBase {
        id: "c-known".into(),
        name: Some("rps".into()),
        query: Some(serde_json::json!({ "mpl": "http_rps:rate" })),
        extras: Default::default(),
    }));
    // `Chart::Unknown` is `untagged`, so any JSON shape that doesn't
    // decode as a `KnownChart` lands here. A made-up future type
    // (e.g. `"sankey"`) is the canonical example.
    let unknown = Chart::Unknown(serde_json::json!({
        "id": "c-unk",
        "type": "sankey",
        "name": "future-viz",
        "weirdField": [1, 2, 3]
    }));
    let resource = crate::axiom::DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: Some(1),
        dashboard: crate::axiom::DashboardDocument {
            name: Some("mixed".into()),
            charts: vec![known, unknown],
            layout: vec![
                LayoutItem {
                    i: "c-known".into(),
                    x: 0,
                    y: Some(0),
                    w: 6,
                    h: 6,
                    extras: Default::default(),
                },
                LayoutItem {
                    i: "c-unk".into(),
                    x: 6,
                    y: Some(0),
                    w: 6,
                    h: 6,
                    extras: Default::default(),
                },
            ],
            ..Default::default()
        },
    };

    let mut app = test_app();
    // Adopt: every code path that touched `.known_base()` used to
    // panic here.
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });

    // Both tiles must be present after adoption (Unknown is NOT
    // silently dropped — that would be data loss on the next `:w`).
    let charts = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts;
    assert_eq!(charts.len(), 2);
    assert!(matches!(charts[0], Chart::Known(_)));
    assert!(matches!(charts[1], Chart::Unknown(_)));

    // Focus the Unknown tile and let the renderer + helpers touch
    // it. None of these may panic.
    app.selected_chart_idx = 1;
    let _ = app.active_legend_series();
    app.execute_command("trace");

    // Re-serialise: the raw JSON of the Unknown tile must survive
    // verbatim (forward-compat round-trip is the whole point of
    // `Chart::Unknown`).
    let json = serde_json::to_value(&app.loaded_dashboard.as_ref().unwrap().dashboard).unwrap();
    let unk_chart = &json["charts"][1];
    assert_eq!(unk_chart["type"], "sankey");
    assert_eq!(unk_chart["weirdField"], serde_json::json!([1, 2, 3]));
}

// ---- Edit-then-save MPL roundtrip: don't dual-key the wire query ----

#[test]
fn editing_mpl_query_normalises_to_mpl_key_dropping_apl() {
    // Regression for the "editing any chart's query produces 400 on
    // :w" bug.
    //
    // Real Axiom dashboards ship MPL queries under the `apl` key
    // (see `extract_query`'s docstring). The previous implementation
    // of `sync_buffer_to_focused_tile` always inserted a fresh `mpl`
    // key on save WITHOUT removing the original `apl` key, so the
    // resulting `{ apl: stale, mpl: new }` object was rejected by
    // the server with 400.
    //
    // The corrected behaviour: write the new text to the `mpl` key
    // AND drop any sibling `apl` key. This keeps two invariants:
    //   (a) single-key payload on PUT, no 400 from the server.
    //   (b) `extract_query` reads `mpl` first and returns
    //       `Query::Mpl` directly, taking the explicit-key fast
    //       path. The chart-kind fallback (LogStream → Apl, else
    //       → Mpl) only runs when no `mpl` key is present.
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u1".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: Some(1),
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![crate::axiom::Chart::Known(
                crate::axiom::KnownChart::TimeSeries(crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: Some("rps".into()),
                    // Wire shape Axiom actually uses: MPL text under
                    // the `apl` key, mirroring real production
                    // dashboards.
                    query: Some(serde_json::json!({ "apl": "old_metric:rate" })),
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

    // Simulate the user editing the buffer: clear it and replace
    // the MPL with new text. The pragma line stays at the top — it
    // gets stripped by `strip_viz_pragma` before the write-back.
    app.editor = tui_textarea::TextArea::from(vec![
        "// @viz line".to_string(),
        "new_metric:rate".to_string(),
    ]);

    // Trigger the write-back path. Focus changes (e.g. `:grid` or
    // tile navigation) and explicit `:w` both go through
    // `sync_buffer_to_focused_tile`.
    app.sync_buffer_to_focused_tile();

    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    let base = chart.base().expect("Chart::Known has a base");
    let query = base.query.as_ref().expect("query was set on open");
    let obj = query.as_object().expect("query is a JSON object");

    // (1) Single-key payload: stale `apl` is gone.
    assert!(
        !obj.contains_key("apl"),
        "stale `apl` key must be dropped to avoid dual-key 400: {obj:?}"
    );
    // (2) Edited text lives under the canonical `mpl` key, which
    //     `extract_query` consults first — no classifier flap.
    assert_eq!(
        obj.get("mpl").and_then(|v| v.as_str()),
        Some("new_metric:rate"),
        "edited text must live under the `mpl` key: {obj:?}"
    );
    // (3) `extract_query` returns `Query::Mpl` via the explicit
    //     `mpl`-key fast path — no chart-kind fallback needed.
    match crate::dashboard::extract_query(chart) {
        crate::dashboard::Query::Mpl(text) => assert_eq!(text, "new_metric:rate"),
        other => panic!("expected Query::Mpl after edit, got {other:?}"),
    }
    // (4) Dirty bit flipped, so the next :w will actually PUT.
    assert!(app.dashboard_dirty);
}

#[test]
fn editing_mpl_query_under_existing_mpl_key_updates_in_place() {
    // Symmetric case: charts that DO use the `mpl` key (legal per
    // OpenAPI, observed in fixtures) keep using it on edit. No
    // stray `apl` key is created.
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u1".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: Some(1),
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![crate::axiom::Chart::Known(
                crate::axiom::KnownChart::TimeSeries(crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: Some("rps".into()),
                    query: Some(serde_json::json!({ "mpl": "old:rate" })),
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
    app.editor =
        tui_textarea::TextArea::from(vec!["// @viz line".to_string(), "new:rate".to_string()]);
    app.sync_buffer_to_focused_tile();

    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    let obj = chart
        .base()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .as_object()
        .unwrap();
    assert!(!obj.contains_key("apl"), "no stray `apl` key: {obj:?}");
    assert_eq!(obj.get("mpl").and_then(|v| v.as_str()), Some("new:rate"));
}

#[test]
fn editing_mpl_query_with_dual_keys_normalises_to_mpl_only() {
    // Defence-in-depth: if a malformed dashboard arrives with BOTH
    // `apl` and `mpl` keys, edits collapse to the single `mpl` key
    // so the next PUT succeeds and `extract_query` keeps returning
    // `Query::Mpl` deterministically.
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u1".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: Some(1),
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![crate::axiom::Chart::Known(
                crate::axiom::KnownChart::TimeSeries(crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: Some("rps".into()),
                    query: Some(serde_json::json!({
                        "apl": "stale:rate",
                        "mpl": "old:rate",
                    })),
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
    app.editor =
        tui_textarea::TextArea::from(vec!["// @viz line".to_string(), "new:rate".to_string()]);
    app.sync_buffer_to_focused_tile();

    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    let obj = chart
        .base()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .as_object()
        .unwrap();
    assert!(
        !obj.contains_key("apl"),
        "stale `apl` must be removed: {obj:?}"
    );
    assert_eq!(obj.get("mpl").and_then(|v| v.as_str()), Some("new:rate"));
}

#[test]
fn editing_mpl_query_keeps_mid_edit_text_routed_as_mpl() {
    // After the classifier was retired in favour of chart-kind-based
    // discrimination, this test still earns its keep: it pins down
    // the `mpl`-key fast path. `extract_query` checks the `mpl` key
    // before falling back to chart-kind dispatch, so local edits
    // (which always write to `mpl`) round-trip as `Query::Mpl`
    // regardless of whether the text is syntactically valid — the
    // user might be mid-typing or using server-only constructs.
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u1".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: Some(1),
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![crate::axiom::Chart::Known(
                crate::axiom::KnownChart::TimeSeries(crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: Some("rps".into()),
                    query: Some(serde_json::json!({ "apl": "http_requests:rate" })),
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
    // Mid-edit: user has typed an unclosed call. Doesn't matter
    // whether the local parser accepts it — we no longer ask.
    let broken_mpl = "rate(http_requests";
    app.editor =
        tui_textarea::TextArea::from(vec!["// @viz line".to_string(), broken_mpl.to_string()]);
    app.sync_buffer_to_focused_tile();

    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    // `extract_query` returns `Query::Mpl` because the buffer write
    // landed under the explicit `mpl` key, which the function
    // checks before consulting the chart kind.
    match crate::dashboard::extract_query(chart) {
        crate::dashboard::Query::Mpl(text) => assert_eq!(text, broken_mpl),
        other => panic!(
            "mid-edit MPL must stay routed as Query::Mpl, not silently \
             flip to APL. got {other:?}"
        ),
    }
}

#[test]
fn normalize_queries_to_wire_moves_mpl_to_apl() {
    // The local model uses `mpl` keys for edited MPL queries (so
    // `extract_query` doesn't re-classify mid-edit text). The wire
    // form the Axiom v2 API expects has every query under `apl`,
    // regardless of language. `normalize_queries_to_wire` bridges
    // the two; it runs on a clone of the document just before PUT.
    let mut doc = crate::axiom::DashboardDocument {
        name: Some("d".into()),
        charts: vec![
            // Chart edited locally → has `mpl` key. Must be moved
            // to `apl` for the wire.
            crate::axiom::Chart::Known(crate::axiom::KnownChart::TimeSeries(
                crate::axiom::ChartBase {
                    id: "c-edited".into(),
                    name: Some("edited".into()),
                    query: Some(serde_json::json!({ "mpl": "new:rate" })),
                    extras: Default::default(),
                },
            )),
            // Chart untouched since load → already in wire form.
            // Must stay untouched.
            crate::axiom::Chart::Known(crate::axiom::KnownChart::TimeSeries(
                crate::axiom::ChartBase {
                    id: "c-stable".into(),
                    name: Some("stable".into()),
                    query: Some(serde_json::json!({ "apl": "old:rate" })),
                    extras: Default::default(),
                },
            )),
            // True APL chart → must NOT be turned into an `mpl` key
            // by some symmetry mistake.
            crate::axiom::Chart::Known(crate::axiom::KnownChart::Pie(crate::axiom::ChartBase {
                id: "c-apl".into(),
                name: Some("by-region".into()),
                query: Some(serde_json::json!({
                    "apl": "['logs'] | summarize count() by region"
                })),
                extras: Default::default(),
            })),
        ],
        ..Default::default()
    };
    crate::dashboard::normalize_queries_to_wire(&mut doc);

    // Chart 0: mpl → apl.
    let obj0 = doc.charts[0]
        .base()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .as_object()
        .unwrap();
    assert!(!obj0.contains_key("mpl"), "wire form must drop `mpl` key");
    assert_eq!(obj0.get("apl").and_then(|v| v.as_str()), Some("new:rate"));

    // Chart 1: untouched.
    let obj1 = doc.charts[1]
        .base()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .as_object()
        .unwrap();
    assert_eq!(obj1.get("apl").and_then(|v| v.as_str()), Some("old:rate"));
    assert!(!obj1.contains_key("mpl"));

    // Chart 2: APL stays APL.
    let obj2 = doc.charts[2]
        .base()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .as_object()
        .unwrap();
    assert_eq!(
        obj2.get("apl").and_then(|v| v.as_str()),
        Some("['logs'] | summarize count() by region")
    );
    assert!(!obj2.contains_key("mpl"));
}

#[test]
fn normalize_queries_to_wire_with_dual_keys_prefers_mpl_text() {
    // Defence-in-depth: if a dual-key state somehow leaks through
    // to the wire-normalisation step, the `mpl` value (the local
    // canonical edit) wins. The stale `apl` is dropped, then `mpl`
    // is moved into the `apl` slot. End state is single-key with
    // the user's latest text.
    let mut doc = crate::axiom::DashboardDocument {
        charts: vec![crate::axiom::Chart::Known(
            crate::axiom::KnownChart::TimeSeries(crate::axiom::ChartBase {
                id: "c".into(),
                name: None,
                query: Some(serde_json::json!({
                    "apl": "stale:rate",
                    "mpl": "new:rate",
                })),
                extras: Default::default(),
            }),
        )],
        ..Default::default()
    };
    crate::dashboard::normalize_queries_to_wire(&mut doc);
    let obj = doc.charts[0]
        .base()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .as_object()
        .unwrap();
    assert!(!obj.contains_key("mpl"));
    assert_eq!(obj.get("apl").and_then(|v| v.as_str()), Some("new:rate"));
}
