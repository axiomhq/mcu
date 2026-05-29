use super::*;
use crate::axiom::{
    DashboardSummaryExt, DatasetSummary, MetricInfo, MetricsQueryResponse, MetricsSeries,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;

pub(super) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub(super) fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

pub(super) fn test_app() -> App {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = rt.handle().clone();
    // Leak the runtime so the handle remains valid for the duration of the test.
    Box::leak(Box::new(rt));
    let mut app = App::with_cache(handle, Cache::in_memory(String::new()));
    // Inject a synthetic single-deployment config so client-building
    // paths (`ensure_client`, trace fetch, share URL) resolve cleanly
    // instead of reading the developer's real `~/.axiom.toml` — which
    // may have multiple deployments and no `active_deployments`, making
    // `select(None)` fail. Mirrors the in-memory cache / history /
    // settings isolation above. A single deployment means `select(None)`
    // returns it without needing `active_deployments`.
    app.config_override = Some(
        crate::config::Config::parse(
            "[deployments.test]\n\
             url = \"https://example.test\"\n\
             token = \"xaat-test-token\"\n\
             org_id = \"test-org\"\n",
        )
        .expect("synthetic test config parses"),
    );
    app
}

pub(super) fn type_text(app: &mut App, s: &str) {
    for c in s.chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
}

/// Seed the cache with two datasets and one metric, so context-aware
/// completion tests have real data to draw on.
pub(super) fn seed_cache(app: &App) {
    let mut c = app.cache.write();
    c.replace_datasets(vec![
        DatasetSummary {
            name: "home".to_string(),
            description: None,
            edge_deployment: None,
            kind: None,
        },
        DatasetSummary {
            name: "homeassistant-logs".to_string(),
            description: None,
            edge_deployment: None,
            kind: None,
        },
    ]);
    let mut metrics: BTreeMap<String, MetricInfo> = BTreeMap::new();
    metrics.insert("temp".to_string(), MetricInfo::default());
    c.replace_metrics("home", metrics);
}

/// Replace the buffer with `text` without touching `saved_buffer`, then
/// rerun diagnostics like a real keystroke would.
pub(super) fn set_buffer(app: &mut App, text: &str) {
    app.editor = crate::editor::editor_with_text(text);
    app.recompute_diagnostics();
}

pub(super) fn buffer(app: &App) -> String {
    app.editor.lines().join("\n")
}

pub(super) fn app_with_series(n: usize) -> App {
    let mut app = test_app();
    app.series = (0..n)
        .map(|i| crate::chart::Series {
            name: format!("s{i}"),
            tags: vec![("k".to_string(), format!("v{i}").into())],
            points: vec![(0.0, i as f64)],
            color: crate::chart::color_for(i),
        })
        .collect();
    app.legend.hidden = vec![false; n];
    app
}

pub(super) fn set_query(app: &mut App, text: &str) {
    // Replace the editor buffer wholesale. `editor_with_text` mirrors
    // what `open_file` uses; good enough for tests.
    app.editor = crate::editor::editor_with_text(text);
}

pub(super) fn dash(uid: &str, name: &str, desc: Option<&str>) -> DashboardSummary {
    DashboardSummary {
        uid: uid.to_string(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some(name.to_string()),
            description: desc.map(str::to_string),
            ..Default::default()
        },
    }
}

/// Minimal but realistic `DashboardResource` JSON: one TimeSeries
/// chart with an MPL query, one layout entry, and a handful of
/// top-level + nested unmodelled fields that must survive a
/// round-trip via `extras`.
const FIXTURE_DASHBOARD_JSON: &str = r#"{
  "uid": "dash-1",
  "id": "42",
  "updatedAt": "2026-05-23T10:00:00Z",
  "dashboard": {
"name": "prod",
"description": "the only one that matters",
"charts": [
  {
    "id": "c1",
    "type": "TimeSeries",
    "name": "rps",
    "query": { "mpl": "http_requests:rate" }
  }
],
"layout": [
  { "i": "c1", "x": 0, "y": 0, "w": 12, "h": 6 }
],
"timeWindowStart": "qr-now-1h",
"timeWindowEnd": "qr-now",
"refreshTime": 60,
"schemaVersion": 2,
"owner": "X-AXIOM-EVERYONE"
  }
}"#;

pub(super) fn multi_chart_resource() -> DashboardSummary {
    use crate::axiom::{Chart, ChartBase, KnownChart, LayoutItem};
    // 2x2 grid of charts, each in its own quadrant of the 12-col,
    // 12-row virtual space.
    let mk = |id: &str, name: &str| {
        Chart::Known(KnownChart::TimeSeries(ChartBase {
            id: id.into(),
            name: Some(name.into()),
            query: Some(serde_json::json!({ "mpl": format!("{name}:rate") })),
            extras: Default::default(),
        }))
    };
    let layout = |id: &str, x: u32, y: u32| LayoutItem {
        i: id.into(),
        x,
        y: Some(y),
        w: 6,
        h: 6,
        extras: Default::default(),
    };
    DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("grid".into()),
            charts: vec![
                mk("tl", "top-left"),
                mk("tr", "top-right"),
                mk("bl", "bottom-left"),
                mk("br", "bottom-right"),
            ],
            layout: vec![
                layout("tl", 0, 0),
                layout("tr", 6, 0),
                layout("bl", 0, 6),
                layout("br", 6, 6),
            ],
            ..Default::default()
        },
    }
}

/// Drive the cmdline into command mode and stash `text` as the
/// initial buffer + cursor position.
pub(super) fn open_cmdline(app: &mut App, text: &str) {
    app.mode = Mode::Command;
    app.cmdline.buf = text.to_string();
    app.cmdline.cursor = text.chars().count();
}

pub(super) fn one_series_response(metric: &str) -> MetricsQueryResponse {
    MetricsQueryResponse {
        series: vec![MetricsSeries {
            metric: metric.into(),
            tags: Default::default(),
            start: 1_000,
            resolution: 60,
            data: vec![Some(1.0), Some(2.0), Some(3.0)],
        }],
        trace_id: None,
    }
}

pub(super) fn mk_layout(i: &str, x: u32, y: u32, w: u32, h: u32) -> crate::axiom::LayoutItem {
    crate::axiom::LayoutItem {
        i: i.into(),
        x,
        y: Some(y),
        w,
        h,
        extras: Default::default(),
    }
}

mod apl;
mod cmdline;
mod completion;
mod dashboard;
mod editor;
mod focus;
mod legend;
mod misc;
mod mouse;
mod params;
mod query;
mod tile;
mod time;
mod trace;
