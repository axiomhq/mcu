//! Phase 1 APL-language coverage:
//!
//! * `extract_query` honours the `axLang` sidecar over chart kind.
//! * `:apl` / `:mpl` flip the focused tile's query-object key and
//!   stamp the sidecar (or just flip `buffer_lang` in standalone).
//! * `:tile add <kind> apl` inserts a tile pre-marked as APL.
//! * APL-tile editor seeds are raw text (no `// APL` comment banner).
//! * `sync_buffer_to_focused_tile` round-trips APL edits.
//! * `normalize_queries_to_wire` leaves APL-keyed queries alone.

use super::*;
use crate::dashboard::{Lang, Query, extract_lang, extract_query};

fn timeseries_with_query(query: serde_json::Value) -> crate::axiom::Chart {
    use crate::axiom::{Chart, ChartBase, KnownChart};
    Chart::Known(KnownChart::TimeSeries(ChartBase {
        id: "c1".into(),
        name: Some("c1".into()),
        query: Some(query),
        extras: Default::default(),
    }))
}

fn timeseries_with_query_and_lang(query: serde_json::Value, lang: Lang) -> crate::axiom::Chart {
    use crate::axiom::{Chart, ChartBase, KnownChart};
    let mut extras: serde_json::Map<String, serde_json::Value> = Default::default();
    extras.insert(
        crate::dashboard::LANG_SIDECAR_KEY.to_string(),
        serde_json::Value::String(lang.as_sidecar().to_string()),
    );
    Chart::Known(KnownChart::TimeSeries(ChartBase {
        id: "c1".into(),
        name: Some("c1".into()),
        query: Some(query),
        extras,
    }))
}

fn dashboard_with_chart(chart: crate::axiom::Chart) -> crate::axiom::DashboardSummary {
    crate::axiom::DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![chart],
            ..Default::default()
        },
    }
}

#[test]
fn sidecar_apl_on_metrics_chart_overrides_kind_default() {
    // A TimeSeries chart with the `axLang=apl` sidecar must classify
    // as APL even though the chart-kind fallback would say MPL. This
    // is the whole point of the sidecar: deterministic language on
    // tiles ax authored.
    let chart =
        timeseries_with_query_and_lang(serde_json::json!({ "apl": "['logs'] | count" }), Lang::Apl);
    assert!(matches!(extract_query(&chart), Query::Apl(_)));
    assert_eq!(extract_lang(&chart), Some(Lang::Apl));
}

#[test]
fn sidecar_mpl_on_logstream_overrides_kind_default() {
    // Mirror: a LogStream chart explicitly marked MPL classifies as
    // MPL. (Not a normal user flow but the sidecar must win both ways.)
    use crate::axiom::{Chart, ChartBase, KnownChart};
    let mut extras: serde_json::Map<String, serde_json::Value> = Default::default();
    extras.insert(
        crate::dashboard::LANG_SIDECAR_KEY.to_string(),
        serde_json::Value::String("mpl".into()),
    );
    let chart = Chart::Known(KnownChart::LogStream(ChartBase {
        id: "c1".into(),
        name: Some("c1".into()),
        query: Some(serde_json::json!({ "apl": "cpu:rate" })),
        extras,
    }));
    assert!(matches!(extract_query(&chart), Query::Mpl(_)));
}

#[test]
fn sidecar_apl_reads_text_from_either_key() {
    // A tile that was MPL yesterday and got `:apl`-flipped today
    // might still have its text under `mpl` until the next sync.
    // The sidecar wins, but we still surface the text.
    let chart =
        timeseries_with_query_and_lang(serde_json::json!({ "mpl": "some text" }), Lang::Apl);
    match extract_query(&chart) {
        Query::Apl(s) => assert_eq!(s, "some text"),
        other => panic!("expected APL, got {other:?}"),
    }
}

#[test]
fn cmd_lang_in_standalone_buffer_only_flips_buffer_lang() {
    // No dashboard loaded → `:apl` mutates `App.buffer_lang` and
    // touches nothing else.
    let mut app = test_app();
    assert_eq!(app.buffer_lang, Lang::Mpl);
    app.execute_command("apl");
    assert_eq!(app.buffer_lang, Lang::Apl);
    assert!(app.loaded_dashboard.is_none());
    app.execute_command("mpl");
    assert_eq!(app.buffer_lang, Lang::Mpl);
}

#[test]
fn cmd_lang_in_dashboard_mode_rewrites_key_and_stamps_sidecar() {
    // Adopt a one-chart dashboard with an MPL TimeSeries tile, then
    // `:apl`. Expected:
    //   * the query object now has the `apl` key, not `mpl`,
    //   * `chart.extras["axLang"] == "apl"`,
    //   * dashboard_dirty flips to true.
    let mut app = test_app();
    let resource = dashboard_with_chart(timeseries_with_query(
        serde_json::json!({ "mpl": "http_requests:rate" }),
    ));
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    app.dashboard_dirty = false; // ignore adoption-side dirty.
    app.execute_command("apl");
    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    let base = chart.base().unwrap();
    let q = base.query.as_ref().unwrap();
    assert!(q.get("apl").is_some(), "query: {q}");
    assert!(q.get("mpl").is_none(), "mpl key must be dropped: {q}");
    assert_eq!(
        base.extras.get(crate::dashboard::LANG_SIDECAR_KEY),
        Some(&serde_json::Value::String("apl".into()))
    );
    assert!(app.dashboard_dirty);
    assert_eq!(app.active_lang(), Lang::Apl);
}

#[test]
fn cmd_lang_does_not_convert_buffer_text() {
    // `:apl` preserves the user's typed text verbatim. The user is
    // expected to rewrite it; we just flip the key.
    let mut app = test_app();
    let resource = dashboard_with_chart(timeseries_with_query(
        serde_json::json!({ "mpl": "http_requests:rate" }),
    ));
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    app.execute_command("apl");
    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    let text = crate::dashboard::extract_query(chart);
    match text {
        Query::Apl(s) => assert_eq!(s, "http_requests:rate"),
        other => panic!("expected APL, got {other:?}"),
    }
}

#[test]
fn tile_add_apl_inserts_apl_marked_tile() {
    // `:tile add line apl my-chart` builds an APL tile with the
    // sidecar set and an empty `apl` key. The user can type APL
    // into the editor and `:w` round-trips correctly.
    let mut app = test_app();
    let resource = dashboard_with_chart(timeseries_with_query(
        serde_json::json!({ "mpl": "anchor:rate" }),
    ));
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    app.execute_command("tile add line apl my-apl");
    let charts = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts;
    assert_eq!(charts.len(), 2);
    let new_chart = &charts[1];
    let base = new_chart.base().unwrap();
    assert_eq!(base.name.as_deref(), Some("my-apl"));
    let q = base.query.as_ref().unwrap();
    assert!(q.get("apl").is_some());
    assert!(q.get("mpl").is_none());
    assert_eq!(
        base.extras.get(crate::dashboard::LANG_SIDECAR_KEY),
        Some(&serde_json::Value::String("apl".into()))
    );
    assert_eq!(extract_lang(new_chart), Some(Lang::Apl));
}

#[test]
fn apl_tile_seeds_editor_with_raw_text_no_banner() {
    // The pre-execution comment-banner is gone: APL tiles seed as
    // raw editable text so the user can type / save normally.
    let mut app = test_app();
    use crate::axiom::{Chart, ChartBase, KnownChart};
    let resource = crate::axiom::DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![Chart::Known(KnownChart::LogStream(ChartBase {
                id: "c1".into(),
                name: Some("logs".into()),
                query: Some(serde_json::json!({
                    "apl": "['logs'] | where severity == 'error' | limit 50"
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
    let buf = buffer(&app);
    assert!(buf.contains("// @viz log_stream"));
    assert!(buf.contains("['logs'] | where severity == 'error' | limit 50"));
    assert!(
        !buf.contains("// APL"),
        "no comment banner allowed in buffer: {buf:?}"
    );
}

#[test]
fn sync_buffer_to_focused_tile_round_trips_apl_edits() {
    // Adopt an APL tile, type new APL text, sync, then verify the
    // chart's `apl` key carries the new text and dashboard is dirty.
    let mut app = test_app();
    let resource = dashboard_with_chart(timeseries_with_query_and_lang(
        serde_json::json!({ "apl": "['logs'] | count" }),
        Lang::Apl,
    ));
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    app.dashboard_dirty = false;
    // Replace the editor with the pragma + new APL.
    set_buffer(
        &mut app,
        "// @viz line\n['logs'] | summarize n = count() by bin(_time, 1m)",
    );
    app.sync_buffer_to_focused_tile();
    let chart = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0];
    match extract_query(chart) {
        Query::Apl(s) => assert_eq!(s, "['logs'] | summarize n = count() by bin(_time, 1m)"),
        other => panic!("expected APL, got {other:?}"),
    }
    assert!(app.dashboard_dirty);
}

#[test]
fn normalize_queries_to_wire_leaves_apl_keyed_queries_alone() {
    // Wire-shape pass: an APL tile (key = `apl`) needs no transform.
    // Only the legacy `mpl`-keyed convention gets rewritten.
    let mut doc = crate::axiom::DashboardDocument {
        charts: vec![
            timeseries_with_query(serde_json::json!({ "mpl": "cpu:rate" })),
            timeseries_with_query_and_lang(
                serde_json::json!({ "apl": "['logs'] | count" }),
                Lang::Apl,
            ),
        ],
        ..Default::default()
    };
    crate::dashboard::normalize_queries_to_wire(&mut doc);
    // MPL chart: text moved to `apl`, `mpl` gone.
    let q0 = doc.charts[0].base().unwrap().query.as_ref().unwrap();
    assert!(q0.get("mpl").is_none());
    assert_eq!(q0.get("apl").and_then(|v| v.as_str()), Some("cpu:rate"));
    // APL chart: untouched.
    let q1 = doc.charts[1].base().unwrap().query.as_ref().unwrap();
    assert_eq!(
        q1.get("apl").and_then(|v| v.as_str()),
        Some("['logs'] | count")
    );
    // Both charts: the `axLang` sidecar must have been scrubbed.
    // (Phase-1 stamped it via `timeseries_with_query_and_lang`; if
    // it leaked into the wire payload the Axiom server's PUT
    // schema validator would reject the request with an unknown-key
    // error — we hit this in the wild loading + saving an APL
    // dashboard.)
    for chart in &doc.charts {
        assert!(
            chart
                .base()
                .unwrap()
                .extras
                .get(crate::dashboard::LANG_SIDECAR_KEY)
                .is_none(),
            "axLang sidecar leaked into wire payload",
        );
    }
}

// ── Phase 2: APL dispatch + handler ────────────────────────────────

fn apl_query_status_stub() -> axiom_rs::datasets::QueryStatus {
    serde_json::from_value(serde_json::json!({
        "elapsedTime": 0,
        "blocksExamined": 0,
        "rowsExamined": 0,
        "rowsMatched": 0,
        "numGroups": 0,
        "isPartial": false,
        "continuationToken": null,
        "cacheStatus": 0,
        "minBlockTime": "2024-01-01T00:00:00Z",
        "maxBlockTime": "2024-01-01T00:00:00Z"
    }))
    .expect("status stub decodes")
}

fn apl_table_response(fixture_json: &str) -> crate::axiom::AplQueryResult {
    let raw: serde_json::Value = serde_json::from_str(fixture_json).expect("fixture parses");
    // The status wrapper is optional in our fixtures; merge if absent.
    let mut wrapper = serde_json::json!({
        "status": serde_json::to_value(apl_query_status_stub()).unwrap(),
        "tables": raw,
    });
    if raw.get("tables").is_some() {
        wrapper = raw;
        wrapper.as_object_mut().unwrap().insert(
            "status".into(),
            serde_json::to_value(apl_query_status_stub()).unwrap(),
        );
    }
    serde_json::from_value(wrapper).expect("response decodes")
}

fn one_chart_dashboard_apl_kind(
    kind: crate::dashboard::VizKind,
    apl: &str,
) -> crate::axiom::DashboardSummary {
    use crate::axiom::{ChartBase, DashboardDocument, DashboardSummary};
    let mut extras: serde_json::Map<String, serde_json::Value> = Default::default();
    extras.insert(
        crate::dashboard::LANG_SIDECAR_KEY.to_string(),
        serde_json::Value::String("apl".into()),
    );
    let base = ChartBase {
        id: "c1".into(),
        name: Some("apl-tile".into()),
        query: Some(serde_json::json!({ "apl": apl })),
        extras,
    };
    DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: DashboardDocument {
            name: Some("d".into()),
            charts: vec![kind.to_chart(base)],
            ..Default::default()
        },
    }
}

/// Apl `TileAplFinished` handler on a `Line` viz kind routes the
/// response into `entry.series` (decoder produces Vec<Series>).
#[test]
fn apl_tile_finished_on_line_kind_populates_series() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(one_chart_dashboard_apl_kind(
            crate::dashboard::VizKind::Line,
            "['logs'] | summarize n=count() by bin(_time, 1h)",
        )),
    });
    // Pre-seed the in-flight tile entry so the handler's
    // "slot must exist" guard passes (mimics run_tile_queries).
    app.tile_results
        .insert("c1".into(), crate::app::types::TileQueryResult::default());
    let resp = apl_table_response(APL_TIME_SERIES_FIXTURE);
    app.handle_event(AppEvent::TileAplFinished {
        chart_id: "c1".into(),
        epoch: app.tile_query_epoch,
        result: Ok(resp),
    });
    let entry = app.tile_results.get("c1").expect("entry present");
    assert!(entry.error.is_none(), "unexpected error: {:?}", entry.error);
    assert_eq!(entry.series.len(), 1);
    assert!(entry.table.is_none());
    assert_eq!(entry.series[0].points.len(), 3);
}

/// On a `Table` viz kind the handler routes the response into
/// `entry.table` so the grid renderer shows the raw columns.
#[test]
fn apl_tile_finished_on_table_kind_populates_table() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(one_chart_dashboard_apl_kind(
            crate::dashboard::VizKind::Table,
            "['logs'] | summarize count() by level",
        )),
    });
    app.tile_results
        .insert("c1".into(), crate::app::types::TileQueryResult::default());
    let resp = apl_table_response(APL_TWO_COLUMN_FIXTURE);
    app.handle_event(AppEvent::TileAplFinished {
        chart_id: "c1".into(),
        epoch: app.tile_query_epoch,
        result: Ok(resp),
    });
    let entry = app.tile_results.get("c1").expect("entry present");
    assert!(entry.error.is_none(), "unexpected error: {:?}", entry.error);
    let table = entry.table.as_ref().expect("table populated");
    assert_eq!(table.columns, vec!["level", "n"]);
    assert_eq!(table.rows.len(), 2);
    assert!(entry.series.is_empty());
}

/// A series-kind tile whose APL response has no time column surfaces
/// the decoder's error in `entry.error` (no silent placeholder).
#[test]
fn apl_tile_finished_decoder_error_surfaces_in_entry() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(one_chart_dashboard_apl_kind(
            crate::dashboard::VizKind::Line,
            "['logs'] | summarize n=count() by level",
        )),
    });
    app.tile_results
        .insert("c1".into(), crate::app::types::TileQueryResult::default());
    // Fixture has only `level` + `n` — no time column. Series
    // decoder must reject and the handler surfaces the message.
    let resp = apl_table_response(APL_TWO_COLUMN_FIXTURE);
    app.handle_event(AppEvent::TileAplFinished {
        chart_id: "c1".into(),
        epoch: app.tile_query_epoch,
        result: Ok(resp),
    });
    let entry = app.tile_results.get("c1").expect("entry present");
    let err = entry.error.as_deref().expect("error populated");
    assert!(err.starts_with("APL:"), "err: {err}");
    assert!(err.contains("time column"), "err: {err}");
}

/// Stale (epoch mismatch) results are dropped silently — protects
/// against late results from a previous dashboard run overwriting a
/// fresh tile with the same id.
#[test]
fn apl_tile_finished_drops_stale_epoch() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(one_chart_dashboard_apl_kind(
            crate::dashboard::VizKind::Line,
            "['logs'] | count",
        )),
    });
    app.tile_results
        .insert("c1".into(), crate::app::types::TileQueryResult::default());
    let resp = apl_table_response(APL_TIME_SERIES_FIXTURE);
    let stale_epoch = app.tile_query_epoch.wrapping_sub(1);
    app.handle_event(AppEvent::TileAplFinished {
        chart_id: "c1".into(),
        epoch: stale_epoch,
        result: Ok(resp),
    });
    let entry = app.tile_results.get("c1").expect("entry preserved");
    assert!(entry.series.is_empty());
    assert!(entry.table.is_none());
    assert!(entry.error.is_none());
}

// Fixtures used by the dispatch tests above. Same shape as the
// decoder fixtures in `src/viz/apl_decode.rs`, kept inline so the
// test bodies stay self-contained.
const APL_TIME_SERIES_FIXTURE: &str = r#"{
  "tables": [{
    "name": "0",
    "sources": [{"name": "logs"}],
    "fields": [
      {"name": "_time", "type": "datetime"},
      {"name": "n", "type": "long"}
    ],
    "order": [],
    "groups": [],
    "range": null,
    "buckets": {"field": "_time", "size": 3600000000000},
    "columns": [
      ["2024-01-01T00:00:00Z", "2024-01-01T01:00:00Z", "2024-01-01T02:00:00Z"],
      [5, 8, 12]
    ]
  }]
}"#;

const APL_TWO_COLUMN_FIXTURE: &str = r#"{
  "tables": [{
    "name": "0",
    "sources": [{"name": "logs"}],
    "fields": [
      {"name": "level", "type": "string"},
      {"name": "n", "type": "long"}
    ],
    "order": [],
    "groups": [{"name": "level"}],
    "range": null,
    "buckets": null,
    "columns": [
      ["error", "info"],
      [3, 75]
    ]
  }]
}"#;

/// Regression: loading a server-authored dashboard whose `apl`-keyed
/// query is genuinely APL on a non-LogStream chart kind must show
/// `APL` in the status bar and **not** flag MPL syntax errors on the
/// raw APL text. Two bugs were fixed at once: (a) the kind-fallback
/// mis-classified bracketed APL as MPL, (b) `recompute_diagnostics`
/// ran the MPL analyzer regardless of language.
/// Regression: in standalone-buffer mode (no dashboard loaded), the
/// `RunQuery` command must dispatch through the APL endpoint when
/// `buffer_lang == Apl`. Before this fix the buffer flowed through
/// `mpl::extract_dataset_metric`, which rejected APL syntax with
/// `MPL error: …` and never sent the request.
/// Regression: typing MPL, then running `:apl`, must clear the
/// stale MPL diagnostics. Before this fix the status bar still
/// showed "1 error - 2:1: MPL syntax error" after the flip because
/// `cmd_lang` only updated `buffer_lang`; the lang-gated
/// `recompute_diagnostics` then only fired on the next buffer
/// keystroke.
/// Regression: in solo mode with `// @viz table`, an APL response
/// must populate `app.table_result` (raw decoder output) instead
/// of going through `to_series` → `series_to_table`. The old path
/// aggregated each series down to a single row, so an N-row APL
/// response displayed as one row.
/// j/k/g/G/Esc bindings on the solo Table pane behave like the
/// legend pane: clamp at edges, `gg` is two-step, `Esc` returns to
/// the editor. Driven through `handle_table_key` so the dispatch
/// in `keys/mod.rs` is exercised too.
/// Regression: persisting an APL buffer and re-opening must restore
/// `:apl` state (not silently drop the user back to MPL).
/// Drives the full `persist_query` → `load_query{,_lang}` →
/// `with_cache` round-trip through a real on-disk path.
#[test]
fn persist_query_round_trips_buffer_lang_through_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("discovery.json");
    // Build an app pointed at the temp cache, flip to APL, type
    // an APL query, persist.
    let handle = tokio::runtime::Runtime::new().unwrap().handle().clone();
    let cache = crate::cache::Cache::with_path(String::new(), cache_path.clone());
    let mut app = crate::app::App::with_cache(handle.clone(), cache);
    app.cmd_lang(Lang::Apl);
    set_buffer(&mut app, "['logs'] | count");
    app.persist_query();
    // New app from a fresh cache pointed at the same path —
    // mimics the next process launch.
    let cache2 = crate::cache::Cache::with_path(String::new(), cache_path);
    let app2 = crate::app::App::with_cache(handle, cache2);
    assert_eq!(app2.buffer_lang, Lang::Apl, "buffer_lang must round-trip");
    assert_eq!(
        app2.query_text(),
        "['logs'] | count",
        "query text must round-trip"
    );
    assert!(
        app2.status.contains("APL"),
        "status should advertise restored APL: {:?}",
        app2.status
    );
}

#[test]
fn table_pane_keys_navigate_rows_and_clamp_at_edges() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.viz_kind = crate::dashboard::VizKind::Table;
    // Seed a 4-row table_result directly so we don't have to fake
    // a network response.
    app.table_result = Some(crate::viz::TableResult {
        columns: vec!["level".into(), "n".into()],
        rows: (0..4)
            .map(|i| {
                vec![
                    crate::viz::table::TableCell::Str(format!("row{i}")),
                    crate::viz::table::TableCell::Int(i as i64),
                ]
            })
            .collect(),
    });
    app.set_focus(crate::app::Pane::Table);
    assert_eq!(app.focus, crate::app::Pane::Table);
    // Drive through the public dispatcher so the `Pane::Table` arm
    // in `keys::mod` is exercised end-to-end.
    let press = |app: &mut crate::app::App, code: KeyCode, m: KeyModifiers| {
        app.on_key(KeyEvent::new(code, m));
    };
    // `j` advances; clamps at the last row.
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(app.table_selected, 1);
    for _ in 0..10 {
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    }
    assert_eq!(app.table_selected, 3, "j must clamp at last row");
    // `k` decrements; clamps at 0.
    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(app.table_selected, 2);
    for _ in 0..10 {
        press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    }
    assert_eq!(app.table_selected, 0, "k must clamp at row 0");
    // `G` jumps to last.
    press(&mut app, KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(app.table_selected, 3);
    // `gg` two-step jumps to first.
    press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(app.table_pending_g, "first g must arm pending");
    press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(app.table_selected, 0);
    assert!(!app.table_pending_g);
    // `Esc` returns focus to the editor.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.focus, crate::app::Pane::Editor);
}

/// `set_focus(Pane::Table)` refuses to enter the table pane when
/// there's nothing to select — prevents the user getting trapped
/// in an empty pane that can't render a cursor.
#[test]
fn set_focus_refuses_table_pane_with_no_rows() {
    let mut app = test_app();
    // No table_result at all.
    app.set_focus(crate::app::Pane::Table);
    assert_eq!(app.focus, crate::app::Pane::Editor);
    assert!(app.status.contains("no table"), "status: {:?}", app.status);
    // Empty rows is also refused.
    app.table_result = Some(crate::viz::TableResult {
        columns: vec!["x".into()],
        rows: vec![],
    });
    app.set_focus(crate::app::Pane::Table);
    assert_eq!(app.focus, crate::app::Pane::Editor);
}

#[test]
fn apl_query_finished_with_table_viz_populates_table_result() {
    let mut app = test_app();
    app.buffer_lang = Lang::Apl;
    app.viz_kind = crate::dashboard::VizKind::Table;
    app.last_query_id = 7;
    // Two-column response with multiple rows. The series
    // → table adapter would collapse this to one row per group.
    let raw = serde_json::json!({
        "status": {
            "elapsedTime": 0, "blocksExamined": 0, "rowsExamined": 0,
            "rowsMatched": 0, "numGroups": 0, "isPartial": false,
            "continuationToken": null, "cacheStatus": 0,
            "minBlockTime": "2024-01-01T00:00:00Z",
            "maxBlockTime": "2024-01-01T00:00:00Z"
        },
        "tables": [{
            "name": "0", "sources": [{"name": "logs"}],
            "fields": [
                {"name": "level", "type": "string"},
                {"name": "n", "type": "long"}
            ],
            "order": [], "groups": [{"name": "level"}],
            "range": null, "buckets": null,
            "columns": [["error", "warn", "info"], [3, 12, 75]]
        }]
    });
    let resp: crate::axiom::AplQueryResult = serde_json::from_value(raw).unwrap();
    app.handle_event(AppEvent::AplQueryFinished {
        id: 7,
        result: Ok(resp),
    });
    let table = app
        .table_result
        .as_ref()
        .expect("table_result must be populated for Table viz");
    assert_eq!(table.columns, vec!["level", "n"]);
    assert_eq!(
        table.rows.len(),
        3,
        "all rows must survive the decode (the old path collapsed to 1)"
    );
    // Series must be cleared so the renderer has an unambiguous
    // data source.
    assert!(app.series.is_empty());
    assert_eq!(app.status, "3 rows");
}

#[test]
fn cmd_apl_in_solo_mode_clears_stale_mpl_diagnostics() {
    let mut app = test_app();
    // Put some text that the MPL analyzer flags as an error.
    set_buffer(&mut app, "// header\nthis is definitely not MPL");
    app.recompute_diagnostics();
    assert!(
        !app.diagnostics.is_empty(),
        "precondition: MPL analyzer should flag the buffer"
    );
    // Flip to APL. Diagnostics must clear immediately.
    app.cmd_lang(Lang::Apl);
    assert_eq!(app.buffer_lang, Lang::Apl);
    assert!(
        app.diagnostics.is_empty(),
        "`:apl` left stale diagnostics: {:?}",
        app.diagnostics
    );
}

#[test]
fn run_query_in_solo_mode_dispatches_apl_when_buffer_is_apl() {
    let mut app = test_app();
    app.buffer_lang = Lang::Apl;
    set_buffer(&mut app, "['logs'] | summarize count() by bin(_time, 1h)");
    let before = app.last_query_id;
    app.run_query();
    // The MPL guard rail used to set `MPL error: …` here; APL
    // dispatch must instead bump the query id and flip busy.
    assert!(app.busy, "APL dispatch should have set busy=true");
    assert_eq!(
        app.last_query_id,
        before.wrapping_add(1),
        "APL dispatch must claim a query id like MPL does"
    );
    assert!(
        app.status.contains("APL"),
        "status should hint at APL dispatch, got: {:?}",
        app.status
    );
}

#[test]
fn loading_apl_dashboard_does_not_report_mpl_errors() {
    use crate::axiom::{Chart, ChartBase, DashboardDocument, KnownChart};
    let mut app = test_app();
    let chart = Chart::Known(KnownChart::TimeSeries(ChartBase {
        id: "c1".into(),
        name: Some("errors-per-hour".into()),
        query: Some(serde_json::json!({
            "apl": "['logs'] | summarize n=count() by bin(_time, 1h)"
        })),
        extras: Default::default(),
    }));
    let resource = crate::axiom::DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: DashboardDocument {
            name: Some("d".into()),
            charts: vec![chart],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    // Language: APL (sniff caught the bracket prefix).
    assert_eq!(app.active_lang(), Lang::Apl);
    // No MPL diagnostics on an APL buffer.
    assert!(
        app.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        app.diagnostics
    );
    // Simulating a keystroke must not reintroduce MPL errors.
    app.recompute_diagnostics();
    assert!(
        app.diagnostics.is_empty(),
        "keystroke re-introduced MPL diagnostics: {:?}",
        app.diagnostics
    );
}

#[test]
fn active_lang_follows_focused_tile_in_dashboard_mode() {
    // Three-tile dashboard: tile 0 MPL, tile 1 APL (sidecar), tile 2 MPL.
    // Moving focus updates `active_lang` accordingly.
    let mut app = test_app();
    let mut doc = crate::axiom::DashboardDocument {
        name: Some("d".into()),
        ..Default::default()
    };
    doc.charts = vec![
        timeseries_with_query(serde_json::json!({ "mpl": "a:rate" })),
        timeseries_with_query_and_lang(serde_json::json!({ "apl": "['logs'] | count" }), Lang::Apl),
        timeseries_with_query(serde_json::json!({ "mpl": "c:rate" })),
    ];
    let resource = crate::axiom::DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: doc,
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    assert_eq!(app.active_lang(), Lang::Mpl);
    app.set_focused_chart(1);
    assert_eq!(app.active_lang(), Lang::Apl);
    app.set_focused_chart(2);
    assert_eq!(app.active_lang(), Lang::Mpl);
}
