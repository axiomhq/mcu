//! query tests.

use super::*;

#[test]
fn query_text_preserves_interval_reference() {
    // The server substitutes `$__interval`; the host must not. Verify
    // the buffer round-trips through `query_text` unchanged.
    let mut app = test_app();
    app.editor = tui_textarea::TextArea::default();
    for c in "home:temp | align to $__interval using avg".chars() {
        app.editor.insert_char(c);
    }
    assert!(
        app.query_text().contains("$__interval"),
        "got: {}",
        app.query_text()
    );
}
#[test]
fn datasets_event_updates_status() {
    let mut app = test_app();
    let datasets = vec![
        DatasetSummary {
            name: "k8s".to_string(),
            description: None,
            edge_deployment: None,
            kind: None,
        },
        DatasetSummary {
            name: "logs".to_string(),
            description: None,
            edge_deployment: None,
            kind: None,
        },
    ];
    app.busy = true;
    // Simulate the spawned task having already updated the cache.
    {
        let mut c = app.cache.write();
        c.replace_datasets(datasets.clone());
    }
    app.handle_event(AppEvent::DatasetsFetched(Ok(datasets)));
    assert!(!app.busy);
    assert_eq!(app.cache.read().dataset_names(), vec!["k8s", "logs"]);
    assert!(app.status.contains("2 dataset"));
}
#[test]
fn metrics_event_updates_status_and_cache_view() {
    let mut app = test_app();
    let mut metrics: BTreeMap<String, MetricInfo> = BTreeMap::new();
    metrics.insert(
        "temp".to_string(),
        MetricInfo {
            kind: Some("Mixed".to_string()),
            temporality: None,
            unit: None,
        },
    );
    app.busy = true;
    {
        let mut c = app.cache.write();
        c.replace_metrics("home", metrics.clone());
    }
    app.handle_event(AppEvent::MetricsFetched {
        dataset: "home".to_string(),
        result: Ok(metrics),
    });
    assert!(!app.busy);
    let names = app.cache.read().metric_names("home");
    assert_eq!(names, vec!["temp"]);
    assert!(app.status.contains("1 metric"));
}
#[test]
fn query_result_updates_series_and_status() {
    let mut app = test_app();
    let mut tags = std::collections::HashMap::new();
    tags.insert("room".to_string(), "Eingang".into());
    let resp = MetricsQueryResponse {
        series: vec![MetricsSeries {
            metric: "temp".to_string(),
            tags,
            start: 1_000,
            resolution: 60,
            data: vec![Some(1.0), None, Some(3.0)],
        }],
        trace_id: None,
    };
    app.busy = true;
    app.last_query_id = 7;
    app.handle_event(AppEvent::QueryFinished {
        id: 7,
        result: Ok(resp),
    });
    assert!(!app.busy);
    assert_eq!(app.series.len(), 1);
    assert_eq!(app.series[0].name, "temp {Eingang}");
    assert_eq!(app.series[0].points.len(), 2);
    assert_eq!(app.series[0].points[0], (1000.0, 1.0));
    assert_eq!(app.series[0].points[1], (1120.0, 3.0));
    assert!(app.status.contains("1 series"));
}
#[test]
fn stale_query_response_is_ignored() {
    let mut app = test_app();
    let prior = app.series.clone();
    app.last_query_id = 5;
    app.busy = true;
    app.handle_event(AppEvent::QueryFinished {
        id: 3,
        result: Ok(MetricsQueryResponse {
            series: vec![MetricsSeries {
                metric: "x".to_string(),
                tags: std::collections::HashMap::new(),
                start: 0,
                resolution: 60,
                data: vec![Some(0.0)],
            }],
            trace_id: None,
        }),
    });
    assert!(app.busy);
    assert_eq!(app.series.len(), prior.len());
}
#[test]
fn referenced_tags_extracts_filter_predicates() {
    let mut got =
        referenced_tags("ds:m | where service.name == \"frontend\" and host != \"box-1\"");
    got.sort();
    assert_eq!(got, vec!["host", "service.name"]);
}
#[test]
fn referenced_tags_supports_backticked_names() {
    let got = referenced_tags("ds:m | where `service.name` == \"frontend\"");
    assert_eq!(got, vec!["service.name"]);
}
#[test]
fn referenced_tags_ignores_occurrences_inside_strings() {
    let got = referenced_tags("ds:m | where host == \"weird == not.a.tag\"");
    assert_eq!(got, vec!["host"]);
}
#[test]
fn referenced_tags_picks_up_inequality_operators() {
    let mut got = referenced_tags("ds:m | where a < 1 and b > 2 and c <= 3 and d >= 4");
    got.sort();
    assert_eq!(got, vec!["a", "b", "c", "d"]);
}
#[test]
fn referenced_tags_empty_when_no_filter() {
    assert!(referenced_tags("ds:m | align to 1m using avg").is_empty());
}
#[test]
fn tag_values_fetched_event_updates_status_when_idle() {
    let mut app = test_app();
    app.handle_event(AppEvent::TagValuesFetched {
        dataset: "home".to_string(),
        metric: "temp".to_string(),
        tag: "host".to_string(),
        result: Ok(vec!["a".to_string(), "b".to_string()]),
    });
    assert!(
        app.status.contains("2 value") && app.status.contains("home:temp.host"),
        "status: {}",
        app.status
    );
}
#[test]
fn tag_values_fetched_event_does_not_clobber_busy_status() {
    let mut app = test_app();
    app.busy = true;
    app.status = "running query…".to_string();
    app.handle_event(AppEvent::TagValuesFetched {
        dataset: "home".to_string(),
        metric: "temp".to_string(),
        tag: "host".to_string(),
        result: Ok(vec!["a".to_string()]),
    });
    assert_eq!(app.status, "running query…");
}
#[test]
fn fetch_tag_values_skipped_when_already_cached() {
    let mut app = test_app();
    app.cache
        .write()
        .replace_tag_values("home", "temp", "host", vec!["a".to_string()]);
    let before = app.status.clone();
    app.fetch_tag_values("home".to_string(), "temp".to_string(), "host".to_string());
    assert!(!app.busy);
    assert_eq!(app.status, before);
}
#[test]
fn tags_fetched_event_caches_to_disk_layer() {
    let mut app = test_app();
    app.handle_event(AppEvent::TagsFetched {
        dataset: "home".to_string(),
        metric: "temp".to_string(),
        result: Ok(vec!["host".to_string(), "region".to_string()]),
    });
    // The handler doesn't write the cache itself (the spawned task does);
    // it just updates status. Verify it didn't blow up.
    assert!(
        app.status.contains("2 tag") && app.status.contains("home:temp"),
        "status: {}",
        app.status
    );
}
#[test]
fn fetch_tags_skipped_when_already_cached() {
    let mut app = test_app();
    app.cache
        .write()
        .replace_tags("home", "temp", vec!["host".to_string()]);
    // The fetch attempt should short-circuit without flipping `busy` or
    // emitting any status change.
    let before = app.status.clone();
    app.fetch_tags("home".to_string(), "temp".to_string());
    assert!(!app.busy, "busy must not be set when cache hit");
    assert_eq!(app.status, before, "status must not change on cache hit");
}
#[test]
fn metric_completion_kicks_in_after_colon() {
    let mut app = test_app();
    seed_cache(&app);
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "home:t");
    app.on_key(key(KeyCode::Tab));
    assert!(app.completions.visible);
    assert_eq!(app.completions.kind_label, "metric");
    let labels: Vec<&str> = app
        .completions
        .items
        .iter()
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(labels, vec!["temp"]);
}
#[test]
fn query_error_keeps_previous_series() {
    let mut app = test_app();
    let prior_len = app.series.len();
    app.last_query_id = 1;
    app.busy = true;
    app.handle_event(AppEvent::QueryFinished {
        id: 1,
        result: Err(anyhow::anyhow!("bad query")),
    });
    assert!(!app.busy);
    assert_eq!(app.series.len(), prior_len);
    assert!(app.status.contains("bad query"));
}
#[test]
fn sig_help_active_arg_tracks_commas() {
    let mut app = test_app();
    // Cursor right after the first comma inside histogram().
    set_buffer(&mut app, "home:temp | bucket to 1m using histogram(0.5, ");
    app.editor.move_cursor(tui_textarea::CursorMove::End);
    app.recompute_sig_help();
    let sh = app.sig_help.as_ref().expect("sig help should be set");
    assert_eq!(sh.label, "histogram");
    assert_eq!(sh.active, 1);
}
#[test]
fn sig_help_clears_outside_call() {
    let mut app = test_app();
    set_buffer(&mut app, "home:temp | align to 1m using avg");
    app.editor.move_cursor(tui_textarea::CursorMove::End);
    app.recompute_sig_help();
    assert!(app.sig_help.is_none(), "got {:?}", app.sig_help);
}
#[test]
fn tx_lands_one_before_target() {
    let mut app = test_app();
    set_buffer(&mut app, "hello world");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('t')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(app.editor.cursor(), (0, 5)); // one before `w` of world
}
#[test]
fn query_hash_is_stable_under_whitespace_and_comments() {
    let sp = vec![];
    let a = mpl::query_hash("home:temp | align to 1m using avg", &sp);
    let b = mpl::query_hash("  home:temp    | align to 1m using avg  ", &sp);
    let c = mpl::query_hash("home:temp\n| align to 1m using avg\n", &sp);
    // `//` line comment is stripped by the compile-based hash.
    let d = mpl::query_hash(
        "home:temp // pick the temperature metric\n| align to 1m using avg",
        &sp,
    );
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(a, d);
}
#[test]
fn query_hash_normalizes_time_windows_and_alignment() {
    let sp = vec![];
    // Source-level time windows collapse (MPL syntax: `[1h..]`).
    let a = mpl::query_hash("home:temp[1h..] | align to 1m using avg", &sp);
    let b = mpl::query_hash("home:temp[24h..] | align to 1m using avg", &sp);
    let c = mpl::query_hash("home:temp | align to 1m using avg", &sp);
    assert_eq!(a, b);
    assert_eq!(a, c);
    // Align intervals collapse.
    let d = mpl::query_hash("home:temp | align to 5m using avg", &sp);
    let e = mpl::query_hash("home:temp | align using avg", &sp);
    assert_eq!(c, d);
    assert_eq!(c, e);
    // Structural changes still differ (different aggregator).
    let f = mpl::query_hash("home:temp | align using sum", &sp);
    assert_ne!(c, f);
    // Different metric clearly differs.
    let g = mpl::query_hash("home:cpu | align to 1m using avg", &sp);
    assert_ne!(c, g);
}
#[test]
fn trace_command_with_no_query_reports_unavailable() {
    let mut app = test_app();
    app.execute_command("trace");
    assert!(
        app.status.contains("no trace id"),
        "status was {:?}",
        app.status
    );
}
#[test]
fn trace_command_returns_global_last_trace_id_outside_grid() {
    let mut app = test_app();
    app.last_trace_id = Some("abc123".into());
    app.execute_command("trace");
    assert_eq!(app.status, "trace: abc123");
}
#[test]
fn trace_command_in_grid_uses_focused_tile_trace_id() {
    let mut app = test_app();
    // Load a multi-tile dashboard so view_mode flips to Grid and
    // selected_chart_idx points at the first chart.
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let chart_id = app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0]
        .base()
        .expect("test fixture is Chart::Known")
        .id
        .clone();
    // Per-tile fetch lands with a trace id.
    let mut resp = one_series_response("x");
    resp.trace_id = Some("tile-trace-9".into());
    app.tile_results
        .insert(chart_id.clone(), Default::default());
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: chart_id.clone(),
        epoch: app.tile_query_epoch,
        result: Ok(resp),
    });
    // Global last_trace_id is a red herring — grid view must
    // prefer the focused tile's trace.
    app.last_trace_id = Some("editor-trace".into());
    app.execute_command("trace");
    assert!(
        app.status.contains("tile-trace-9"),
        "status was {:?}",
        app.status
    );
    assert!(
        !app.status.contains("editor-trace"),
        "status leaked editor trace: {:?}",
        app.status
    );
}
#[test]
fn trace_command_in_grid_reports_pending_when_tile_has_no_result() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // No TileQueryFinished events delivered — tile_results is empty
    // for the focused chart.
    app.execute_command("trace");
    assert!(
        app.status.contains("no trace id"),
        "status was {:?}",
        app.status
    );
}

// ---- Status-bar trace id resolver ------------------------------------
//
// The status bar must mirror `:trace` semantics: in Grid view the
// focused tile's trace wins; outside Grid `last_trace_id` wins; in
// Grid with no tile result we display nothing rather than falling
// back to a stale editor trace.

#[test]
fn status_trace_id_outside_grid_uses_last_trace_id() {
    let mut app = test_app();
    app.last_trace_id = Some("editor-trace".into());
    assert_eq!(
        crate::ui::status_trace_id(&app).as_deref(),
        Some("editor-trace")
    );
}

#[test]
fn status_trace_id_in_grid_prefers_focused_tile_trace() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let chart_id = app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0]
        .base()
        .expect("test fixture is Chart::Known")
        .id
        .clone();
    let mut resp = one_series_response("x");
    resp.trace_id = Some("tile-trace-9".into());
    app.tile_results
        .insert(chart_id.clone(), Default::default());
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id,
        epoch: app.tile_query_epoch,
        result: Ok(resp),
    });
    // The editor's trace is a red herring — the status bar must show
    // the focused tile's, not this one.
    app.last_trace_id = Some("editor-trace".into());
    assert_eq!(
        crate::ui::status_trace_id(&app).as_deref(),
        Some("tile-trace-9")
    );
}

#[test]
fn status_trace_id_in_grid_returns_none_when_tile_pending() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // No TileQueryFinished delivered — the focused tile has no result
    // yet. Falling back to last_trace_id here would display the
    // editor's trace and re-introduce the bug.
    app.last_trace_id = Some("editor-trace".into());
    assert_eq!(crate::ui::status_trace_id(&app), None);
}

// ---- Per-tile elapsed-time bookkeeping --------------------------------

#[test]
fn tile_query_finished_consumes_started_at_into_elapsed() {
    // Simulate a tile that's been kicked off (busy + started_at)
    // and verify that delivering a `TileQueryFinished` event computes
    // an `elapsed` duration from it. We can't assert an exact value
    // (Instant is monotonic), but we can assert the transition
    // happens and the duration is non-negative.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let chart_id = app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0]
        .base()
        .expect("test fixture is Chart::Known")
        .id
        .clone();
    // Pretend the fetch was kicked off ~50ms ago.
    let started = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(50))
        .expect("now - 50ms shouldn't overflow on any sane system");
    app.tile_results.insert(
        chart_id.clone(),
        crate::app::types::TileQueryResult {
            busy: true,
            started_at: Some(started),
            ..Default::default()
        },
    );
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: chart_id.clone(),
        epoch: app.tile_query_epoch,
        result: Ok(one_series_response("x")),
    });
    let tile = app
        .tile_results
        .get(&chart_id)
        .expect("tile_results entry survives finish");
    assert!(!tile.busy, "fetch is no longer in flight");
    assert!(
        tile.started_at.is_none(),
        "started_at must be consumed, not left dangling"
    );
    let elapsed = tile.elapsed.expect("elapsed must be populated");
    assert!(
        elapsed >= std::time::Duration::from_millis(50),
        "elapsed must reflect at least the simulated 50ms gap, got {elapsed:?}"
    );
    // Sanity upper bound: even on a slow CI box, the synchronous
    // handle_event shouldn't add minutes to the wall clock.
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "elapsed implausibly large: {elapsed:?}"
    );
}

#[test]
fn tile_query_finished_records_elapsed_even_on_error() {
    // Slow errors are interesting too — "timed out after 30s" tells a
    // different story than "failed in 80ms". Make sure the error path
    // still populates `elapsed`.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let chart_id = app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0]
        .base()
        .expect("test fixture is Chart::Known")
        .id
        .clone();
    app.tile_results.insert(
        chart_id.clone(),
        crate::app::types::TileQueryResult {
            busy: true,
            started_at: Some(std::time::Instant::now()),
            ..Default::default()
        },
    );
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: chart_id.clone(),
        epoch: app.tile_query_epoch,
        result: Err(anyhow::anyhow!("synthetic failure")),
    });
    let tile = app.tile_results.get(&chart_id).unwrap();
    assert!(!tile.busy);
    assert!(
        tile.elapsed.is_some(),
        "failed fetches must still record how long they took"
    );
    assert!(tile.error.is_some());
}

// ---- OTEL unit resolution on result land ------------------------------

#[test]
fn tile_query_finished_resolves_unit_from_metric_info() {
    // End-to-end: seed the cache with `MetricInfo.unit = Some("By")`
    // for the tile's metric, dispatch a fake `TileQueryFinished`,
    // and verify the per-tile unit lands. The classify path through
    // `resolve_unit -> unit::parse` should pick `BytesBinary`.
    let mut app = test_app();
    // Adopt a dashboard so the fetched tile has a chart in the
    // model with a known (dataset, metric) we can stamp the unit
    // on. `multi_chart_resource()` uses metric `top-left:rate`
    // (extract_dataset_metric reads "top-left" as the metric and
    // synthesises dataset from the project default — see fixture
    // helpers). We'll seed the cache for the dataset extracted
    // from the first chart's MPL.
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    let chart_id = chart
        .base()
        .expect("test fixture is Chart::Known")
        .id
        .clone();
    let mpl = match crate::dashboard::extract_query(chart) {
        crate::dashboard::Query::Mpl(s) => s,
        other => panic!("test fixture chart must have MPL, got {other:?}"),
    };
    let (dataset, metric) =
        crate::mpl::extract_dataset_metric(&mpl).expect("fixture MPL parses to dataset+metric");

    // Seed MetricInfo.unit on the cache for that metric.
    {
        let mut c = app.cache.write();
        let mut metrics: std::collections::BTreeMap<String, crate::axiom::MetricInfo> =
            std::collections::BTreeMap::new();
        metrics.insert(
            metric,
            crate::axiom::MetricInfo {
                kind: None,
                temporality: None,
                unit: Some("By".to_string()),
            },
        );
        c.replace_metrics(&dataset, metrics);
    }

    // The tile_results slot exists because `run_tile_queries` ran
    // during adoption but it's still busy (no fetch can land in
    // unit tests). Synthesise the finished event directly.
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: chart_id.clone(),
        epoch: app.tile_query_epoch,
        result: Ok(one_series_response("bytes-in")),
    });
    let tile = app
        .tile_results
        .get(&chart_id)
        .expect("tile_results entry after fetch land");
    let unit = tile.unit.as_ref().expect("unit resolved from MetricInfo");
    assert_eq!(unit.family(), crate::unit::UnitFamily::BytesBinary);
    assert_eq!(unit.raw(), "By");
}

// ---- Live `// @unit` editing -----------------------------------------

#[test]
fn editing_unit_pragma_updates_solo_unit_without_rerun() {
    let mut app = test_app();

    set_buffer(&mut app, "// @unit By\nhome:temp");
    let unit = app.unit.as_ref().expect("unit pragma resolved live");
    assert_eq!(unit.family(), crate::unit::UnitFamily::BytesBinary);

    set_buffer(&mut app, "// @unit ms\nhome:temp");
    let unit = app.unit.as_ref().expect("edited unit pragma resolved live");
    assert_eq!(unit.family(), crate::unit::UnitFamily::Time);
    assert_eq!(unit.raw(), "ms");
}

#[test]
fn live_unit_resolution_does_not_strip_viz_comment() {
    let mut app = test_app();

    // `// @viz` is just a leading MPL comment for this path. The
    // dataset/metric extractor skips it and the unit parser still
    // finds the later `// @unit` in the same leading comment block.
    set_buffer(&mut app, "// @viz line\n// @unit By\nhome:temp");
    let unit = app
        .unit
        .as_ref()
        .expect("unit pragma found after viz pragma");
    assert_eq!(unit.family(), crate::unit::UnitFamily::BytesBinary);
}

#[test]
fn live_unit_resolution_prefers_metric_info_over_pragma() {
    let mut app = test_app();
    {
        let mut metrics = BTreeMap::new();
        metrics.insert(
            "temp".to_string(),
            MetricInfo {
                kind: None,
                temporality: None,
                unit: Some("s".to_string()),
            },
        );
        app.cache.write().replace_metrics("home", metrics);
    }

    set_buffer(&mut app, "// @unit By\nhome:temp");
    let unit = app.unit.as_ref().expect("metric metadata resolved live");
    assert_eq!(unit.family(), crate::unit::UnitFamily::Time);
    assert_eq!(unit.raw(), "s");
}

#[test]
fn editing_unit_pragma_updates_focused_dashboard_tile_without_rerun() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let chart_id = app.current_chart_id().expect("fixture has a focused chart");
    app.tile_results.insert(
        chart_id.clone(),
        crate::app::types::TileQueryResult {
            series: vec![],
            ..Default::default()
        },
    );

    set_buffer(&mut app, "// @viz line\n// @unit By\ntop-left:rate");
    let unit = app
        .tile_results
        .get(&chart_id)
        .and_then(|tile| tile.unit.as_ref())
        .expect("focused tile unit updated live");
    assert_eq!(unit.family(), crate::unit::UnitFamily::BytesBinary);
}
