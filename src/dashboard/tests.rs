use super::*;

#[test]
fn viz_kind_round_trips_through_as_str_and_parse() {
    for k in [
        VizKind::Line,
        VizKind::Bar,
        VizKind::Area,
        VizKind::Scatter,
        VizKind::Statistic,
        VizKind::TopList,
        VizKind::Pie,
        VizKind::Heatmap,
        VizKind::Table,
        VizKind::LogStream,
        VizKind::MonitorList,
        VizKind::Note,
        VizKind::Spacer,
    ] {
        assert_eq!(VizKind::parse(k.as_str()), Some(k), "round-trip for {k:?}");
    }
}

#[test]
fn viz_kind_parse_accepts_aliases() {
    assert_eq!(VizKind::parse("stat"), Some(VizKind::Statistic));
    assert_eq!(VizKind::parse("toplist"), Some(VizKind::TopList));
    assert_eq!(VizKind::parse("logs"), Some(VizKind::LogStream));
    assert_eq!(VizKind::parse("logstream"), Some(VizKind::LogStream));
    assert_eq!(VizKind::parse("monitors"), Some(VizKind::MonitorList));
}

#[test]
fn viz_kind_parse_rejects_unknown() {
    assert_eq!(VizKind::parse(""), None);
    assert_eq!(VizKind::parse("nope"), None);
    assert_eq!(VizKind::parse("LINE"), None); // case-sensitive
}

#[test]
fn implemented_set_matches_current_scope() {
    let implemented: Vec<_> = [
        VizKind::Line,
        VizKind::Bar,
        VizKind::Area,
        VizKind::Scatter,
        VizKind::Statistic,
        VizKind::TopList,
        VizKind::Pie,
        VizKind::Heatmap,
        VizKind::Table,
        VizKind::LogStream,
        VizKind::MonitorList,
        VizKind::Note,
        VizKind::Spacer,
    ]
    .into_iter()
    .filter(|k| k.is_implemented())
    .collect();
    assert_eq!(
        implemented,
        vec![
            VizKind::Line,
            VizKind::Bar,
            VizKind::Area,
            VizKind::Scatter,
            VizKind::Statistic,
            VizKind::TopList,
            VizKind::Pie,
            VizKind::Heatmap,
            VizKind::Table,
            VizKind::LogStream,
            VizKind::MonitorList,
            VizKind::Note,
            VizKind::Spacer,
        ]
    );
}

use crate::axiom::{ChartBase, DashboardDocument, DashboardSummary, KnownChart};
use serde_json::json;

// Fixtures lifted verbatim from `GET /v2/dashboards/uid/…` against
// a real account. Two MPL examples (home overview) and one APL
// example (probe-* dashboards).
const REAL_MPL_BACKTICK_STAT: &str =
    "`home`:temp\n| where type == \"temperature\"\n| where room != \"Außen\"\n| group using avg";
const REAL_MPL_BACKTICK_TIMESERIES: &str =
    "`home`:power\n| group by circuit using sum\n| align to 5m using avg";
const REAL_APL_BRACKET: &str = "[\"axiom-audit-logs\"] | summarize n=count() by bin_auto(_time)";

fn statistic_with_apl(text: &str) -> Chart {
    Chart::Known(KnownChart::Statistic(crate::axiom::ChartBase {
        id: "c1".into(),
        name: None,
        query: Some(json!({ "apl": text })),
        extras: Default::default(),
    }))
}

#[test]
fn real_home_overview_mpl_statistic_classifies_as_mpl() {
    // ``home`:temp | where … | group using avg` — stored under
    // the `apl` key on a Statistic chart. The discriminator is
    // the leading backtick.
    let chart = statistic_with_apl(REAL_MPL_BACKTICK_STAT);
    assert!(matches!(extract_query(&chart), Query::Mpl(_)));
}

#[test]
fn real_home_overview_mpl_timeseries_classifies_as_mpl() {
    let chart = Chart::Known(KnownChart::TimeSeries(crate::axiom::ChartBase {
        id: "c1".into(),
        name: None,
        query: Some(json!({ "apl": REAL_MPL_BACKTICK_TIMESERIES })),
        extras: Default::default(),
    }));
    assert!(matches!(extract_query(&chart), Query::Mpl(_)));
}

#[test]
fn real_probe_apl_with_bracketed_dataset_classifies_as_apl() {
    // `["axiom-audit-logs"] | summarize n=count() by …` — stored
    // under `apl` on a TimeSeries chart. Pipes don't make this
    // MPL; the leading `[` does make it APL.
    let chart = Chart::Known(KnownChart::TimeSeries(crate::axiom::ChartBase {
        id: "c1".into(),
        name: None,
        query: Some(json!({ "apl": REAL_APL_BRACKET })),
        extras: Default::default(),
    }));
    assert!(matches!(extract_query(&chart), Query::Apl(_)));
}

#[test]
fn bare_metric_classifies_as_mpl() {
    // Bare `metric:agg` shape — valid MPL the engine accepts.
    let chart = statistic_with_apl("cpu:rate");
    assert!(matches!(extract_query(&chart), Query::Mpl(_)));
}

#[test]
fn invalid_mpl_syntax_classifies_as_apl() {
    // Anything the engine rejects — even if it textually looks
    // metric-shaped — falls through to APL so the metrics endpoint
    // doesn't get pinged with garbage.
    let chart = statistic_with_apl("this is definitely not a valid query");
    assert!(matches!(extract_query(&chart), Query::Apl(_)));
}

#[test]
fn bare_identifier_dataset_with_pipes_classifies_as_apl() {
    // `axiom-history | count` is valid APL (dataset name without
    // brackets). No colon before the pipe → not MPL.
    let chart = statistic_with_apl("axiom-history | count");
    assert!(matches!(extract_query(&chart), Query::Apl(_)));
}

#[test]
fn explicit_mpl_key_still_wins_when_present() {
    let chart = Chart::Known(KnownChart::TimeSeries(crate::axiom::ChartBase {
        id: "c1".into(),
        name: None,
        query: Some(json!({
            "mpl": "correct:value",
            "apl": "['logs'] | count",
        })),
        extras: Default::default(),
    }));
    match extract_query(&chart) {
        Query::Mpl(s) => assert_eq!(s, "correct:value"),
        other => panic!("expected Mpl, got {other:?}"),
    }
}

#[test]
fn extract_query_chart_without_query_yields_empty() {
    let chart = Chart::Known(KnownChart::TopK(ChartBase {
        id: "c3".into(),
        name: Some("errors".into()),
        query: None,
        extras: Default::default(),
    }));
    assert!(matches!(extract_query(&chart), Query::Empty));
}

#[test]
fn default_time_range_matches_legacy_constants() {
    let r = TimeRange::default();
    assert_eq!(r.start, "now-1h");
    assert_eq!(r.end, "now");
}

#[test]
fn time_range_from_resource_carries_window() {
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: DashboardDocument {
            name: Some("d".into()),
            time_window_start: Some("qr-now-7d".into()),
            time_window_end: Some("qr-now".into()),
            ..Default::default()
        },
    };
    let r = TimeRange::from_resource(&resource);
    assert_eq!(r.start, "qr-now-7d");
    assert_eq!(r.end, "qr-now");
}

#[test]
fn time_range_from_resource_falls_back_to_legacy_defaults() {
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: DashboardDocument {
            name: Some("d".into()),
            ..Default::default()
        },
    };
    let r = TimeRange::from_resource(&resource);
    assert_eq!(r.start, "now-1h");
    assert_eq!(r.end, "now");
}
