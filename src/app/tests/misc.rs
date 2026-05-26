//! misc tests.

use super::*;
use ::time;

#[test]
fn run_query_blocked_by_error_diagnostic_keeps_busy_unset() {
    let mut app = test_app();
    set_buffer(&mut app, "`home`:* | align to 1m");
    app.run_query();
    assert!(!app.busy, "run_query must not flip busy on a parse error");
    assert!(
        app.status.to_lowercase().contains("mpl error")
            || app.status.to_lowercase().contains("expected"),
        "status: {}",
        app.status
    );
}
#[test]
fn finished_query_loads_legend_tags_from_cache() {
    let mut app = test_app();
    // Seed cache with a (dataset, metric) entry (no hash match).
    {
        let mut c = app.cache.write().unwrap();
        c.set_legend_tags(
            "hash-x",
            "home",
            "temp",
            vec!["host".to_string(), "region".to_string()],
        );
    }
    // Drive the run_query path indirectly: set the context as run_query would.
    app.last_query_context = Some(QueryContext {
        hash: "unrelated".to_string(),
        dataset: "home".to_string(),
        metric: "temp".to_string(),
    });
    let mut tags = std::collections::HashMap::new();
    tags.insert("host".to_string(), "db-01".into());
    tags.insert("region".to_string(), "us".into());
    let resp = MetricsQueryResponse {
        series: vec![MetricsSeries {
            metric: "temp".to_string(),
            tags,
            start: 0,
            resolution: 60,
            data: vec![Some(1.0)],
        }],
        trace_id: None,
    };
    app.busy = true;
    app.last_query_id = 7;
    app.handle_event(AppEvent::QueryFinished {
        id: 7,
        result: Ok(resp),
    });
    // Fallback to (dataset, metric) hit.
    assert_eq!(
        app.legend.label_tags,
        vec!["host".to_string(), "region".to_string()]
    );
}
#[test]
fn removing_pragma_falls_back_to_line() {
    let mut app = test_app();
    set_buffer(&mut app, "// @viz bar\nhome:temp");
    assert_eq!(app.viz_kind, VizKind::Bar);
    set_buffer(&mut app, "home:temp");
    assert_eq!(app.viz_kind, VizKind::Line);
}
#[test]
fn unknown_pragma_kind_pushes_warning_diagnostic() {
    let mut app = test_app();
    set_buffer(&mut app, "// @viz nope\nhome:temp");
    let w = app
        .diagnostics
        .iter()
        .find(|d| matches!(d.severity, mpl::Severity::Warning))
        .expect("expected a warning diagnostic for unknown viz kind");
    assert!(w.message.contains("unknown viz kind"));
    assert_eq!(w.line, 1);
}
#[test]
fn dash_new_buffer_builds_timeseries_chart_with_mpl() {
    let doc = build_dashboard_doc_from_buffer("my dash", VizKind::Line, "http_rps:rate");
    assert_eq!(doc.name.as_deref(), Some("my dash"));
    assert_eq!(doc.charts.len(), 1);
    assert_eq!(doc.charts[0].type_str(), Some("TimeSeries"));
    // MPL survives through the opaque query JSON.
    let q = doc.charts[0].known_base().query.as_ref().unwrap();
    assert_eq!(q["mpl"], "http_rps:rate");
    // Layout placed in the top-left corner spanning full width.
    assert_eq!(doc.layout[0].i, "c1");
    assert_eq!(doc.layout[0].w, 12);
    // Server-required scalars stashed in extras.
    assert_eq!(doc.extras["refreshTime"], 60);
    assert_eq!(doc.extras["schemaVersion"], 2);
    assert_eq!(doc.extras["owner"], "X-AXIOM-EVERYONE");
}
#[test]
fn dash_new_buffer_maps_each_viz_kind_to_a_chart_type() {
    let cases = [
        (VizKind::Line, "TimeSeries"),
        (VizKind::Bar, "TimeSeries"),  // TUI-only → fallback
        (VizKind::Area, "TimeSeries"), // TUI-only → fallback
        (VizKind::Scatter, "Scatter"),
        (VizKind::Pie, "Pie"),
        (VizKind::Heatmap, "Heatmap"),
        (VizKind::Table, "Table"),
        (VizKind::TopList, "TopK"), // rename across the boundary
        (VizKind::Statistic, "Statistic"),
        (VizKind::LogStream, "LogStream"),
        (VizKind::Note, "Note"),
        (VizKind::MonitorList, "TimeSeries"), // TUI-only → fallback
        (VizKind::Spacer, "TimeSeries"),      // TUI-only → fallback
    ];
    for (kind, expected) in cases {
        let doc = build_dashboard_doc_from_buffer("x", kind, "q");
        assert_eq!(
            doc.charts[0].type_str(),
            Some(expected),
            "{kind:?} should map to {expected}"
        );
    }
}
#[test]
fn dash_new_buffer_doc_serialises_to_a_valid_upsert_request() {
    // The doc must be encodable as the body for POST /v2/dashboards.
    let doc = build_dashboard_doc_from_buffer("x", VizKind::Line, "q");
    let body = crate::axiom::DashboardUpsertRequest {
        dashboard: &doc,
        version: None,
        overwrite: false,
        uid: None,
        message: None,
    };
    let v = serde_json::to_value(&body).unwrap();
    assert_eq!(v["dashboard"]["name"], "x");
    assert_eq!(v["dashboard"]["charts"][0]["type"], "TimeSeries");
    // overwrite defaults to false → omitted; version is None → omitted.
    assert!(v.get("overwrite").is_none());
    assert!(v.get("version").is_none());
}
#[test]
fn loading_multi_chart_dashboard_auto_switches_to_grid_view() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(app.view_mode, ViewMode::Grid);
    assert_eq!(app.focus, Pane::Dashboard);
    assert_eq!(app.selected_chart_idx, 0);
}
#[test]
fn loading_single_chart_dashboard_stays_in_solo() {
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("single".into()),
            charts: vec![crate::axiom::Chart::Known(
                crate::axiom::KnownChart::TimeSeries(crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: None,
                    query: Some(serde_json::json!({ "mpl": "x:y" })),
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
    assert_eq!(app.view_mode, ViewMode::Solo);
}
#[test]
fn active_time_range_strips_qr_prefix_for_mpl_endpoint() {
    // Dashboards from the Axiom web UI store `qr-now-7d` / `qr-now`,
    // but the `_mpl` endpoint only accepts the bare relative form.
    // active_time_range must normalise on the way out so we don't
    // 400 with `invalid field: "qr"`.
    let mut app = test_app();
    app.execute_command("time qr-now-7d qr-now");
    // What we store is verbatim (so `:dash save` round-trips)…
    assert_eq!(app.time.range.start.as_str(), "qr-now-7d");
    assert_eq!(app.time.range.end.as_str(), "qr-now");
    // …but what the query layer reads is normalised.
    assert_eq!(
        app.active_time_range(),
        ("now-7d".to_string(), "now".to_string())
    );
}
#[test]
fn custom_range_picker_shift_month_handles_year_wrap_and_short_months() {
    // Jan 31 + 1 month → Feb 29 (2024 is leap).
    let mut p = CustomRangePicker {
        start: time::Date::from_calendar_date(2024, time::Month::January, 31).unwrap(),
        end: time::Date::from_calendar_date(2024, time::Month::January, 1).unwrap(),
        focus: CustomField::Start,
    };
    p.shift_month(1);
    assert_eq!(
        p.start,
        time::Date::from_calendar_date(2024, time::Month::February, 29).unwrap()
    );
    // Going back from January wraps the year.
    let mut p = CustomRangePicker {
        start: time::Date::from_calendar_date(2024, time::Month::January, 15).unwrap(),
        end: time::Date::from_calendar_date(2024, time::Month::January, 16).unwrap(),
        focus: CustomField::Start,
    };
    p.shift_month(-1);
    assert_eq!(
        p.start,
        time::Date::from_calendar_date(2023, time::Month::December, 15).unwrap()
    );
}
#[test]
fn d_then_y_deletes_selected_tile() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        4
    );
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.tile_submode, TileSubMode::ConfirmDelete));
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        3
    );
    assert!(app.dashboard_dirty);
    assert!(matches!(app.tile_submode, TileSubMode::Idle));
}
#[test]
fn d_then_any_other_key_cancels_delete() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('n')));
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        4
    );
    assert!(matches!(app.tile_submode, TileSubMode::Idle));
}
#[test]
fn dash_save_subcommand_is_unknown_after_step_19() {
    // The `:dash save` pattern was collapsed into `:w` / `:w!`.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("dash save");
    let err = app.last_error.as_deref().unwrap_or("");
    assert!(err.contains("unknown sub-command"), "got: {err:?}");
}
