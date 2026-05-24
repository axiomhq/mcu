//! Canonical internal model for the TUI's visualisation surface.
//!
//! Step 11 introduces this module ahead of the multi-tile work in steps 17
//! and 18. Today the [`Dashboard`] always carries exactly one [`Tile`],
//! mirroring the single-buffer-one-element world the rest of the app still
//! lives in. The shape exists now so later steps can load a real
//! multi-tile dashboard JSON without reworking core state.
//!
//! State migration note: `App.series`, `App.legend_*`, `App.busy`,
//! `App.last_error`, and `App.last_trace_id` currently live on `App`. They
//! are conceptually the *focused tile's* state and will move onto a
//! per-`TileId` map in step 17 (when loading a dashboard actually creates
//! more than one tile). Mechanically straightforward; deferred to keep
//! step 11 focused on viz-kind dispatch.

use std::collections::BTreeMap;

use crate::axiom::{Chart, DashboardSummary, LayoutItem};

/// Identifier for a tile within a [`Dashboard`]. Stable for the lifetime
/// of the dashboard; new tiles get a monotonically increasing id.
pub type TileId = u32;

/// Which Axiom dashboard element a tile renders. Variants outside
/// `Line/Bar/Area/Scatter` are accepted by the parser so files authored
/// ahead of the implementation produce an "unsupported yet" diagnostic
/// rather than "unknown kind".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VizKind {
    #[default]
    Line,
    Bar,
    Area,
    Scatter,
    Statistic,
    TopList,
    Pie,
    Heatmap,
    Table,
    LogStream,
    MonitorList,
    Note,
    Spacer,
}

impl VizKind {
    /// Map an Axiom wire `Chart` variant to our internal `VizKind`.
    ///
    /// Notes on the cross-mapping:
    /// * `TopK` (server) ↔ `TopList` (TUI). Naming difference, same
    ///   element.
    /// * `Scatter` is rendered today through the same series code path
    ///   as line/bar; the rendering is approximate (no per-point
    ///   markers in the metrics chart) but doesn't crash.
    /// * `Bar`, `Area`, `Spacer`, `MonitorList` are TUI-only sub-kinds
    ///   and never appear in the wire `Chart` enum, so they're not
    ///   reachable here.
    pub fn from_chart(chart: &Chart) -> Self {
        match chart {
            Chart::TimeSeries(_) => VizKind::Line,
            Chart::Heatmap(_) => VizKind::Heatmap,
            Chart::LogStream(_) => VizKind::LogStream,
            Chart::Pie(_) => VizKind::Pie,
            Chart::Scatter(_) => VizKind::Scatter,
            Chart::Table(_) => VizKind::Table,
            Chart::TopK(_) => VizKind::TopList,
            Chart::Statistic(_) => VizKind::Statistic,
            Chart::Note(_) => VizKind::Note,
        }
    }

    /// Lower-case identifier used in `// @viz <kind>` pragmas and `:viz` commands.
    pub fn as_str(self) -> &'static str {
        match self {
            VizKind::Line => "line",
            VizKind::Bar => "bar",
            VizKind::Area => "area",
            VizKind::Scatter => "scatter",
            VizKind::Statistic => "statistic",
            VizKind::TopList => "top_list",
            VizKind::Pie => "pie",
            VizKind::Heatmap => "heatmap",
            VizKind::Table => "table",
            VizKind::LogStream => "log_stream",
            VizKind::MonitorList => "monitor_list",
            VizKind::Note => "note",
            VizKind::Spacer => "spacer",
        }
    }

    /// Parse a pragma identifier. Accepts both `top_list` (canonical) and
    /// `toplist` (no underscore) for the multi-word kinds; same for the
    /// other compounds so older notes survive a rename.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "line" => VizKind::Line,
            "bar" => VizKind::Bar,
            "area" => VizKind::Area,
            "scatter" => VizKind::Scatter,
            "statistic" | "stat" => VizKind::Statistic,
            "top_list" | "toplist" => VizKind::TopList,
            "pie" => VizKind::Pie,
            "heatmap" => VizKind::Heatmap,
            "table" => VizKind::Table,
            "log_stream" | "logstream" | "logs" => VizKind::LogStream,
            "monitor_list" | "monitors" => VizKind::MonitorList,
            "note" => VizKind::Note,
            "spacer" => VizKind::Spacer,
            _ => return None,
        })
    }

    /// `true` for the kinds whose renderer is implemented today. Used by
    /// tests and by future placeholder UI for the kinds that still route
    /// through [`crate::viz::draw_unsupported_placeholder`].
    #[allow(dead_code)] // kept as the canonical list; consumed by tests.
    pub fn is_implemented(self) -> bool {
        matches!(
            self,
            VizKind::Line
                | VizKind::Bar
                | VizKind::Area
                | VizKind::Scatter
                | VizKind::Statistic
                | VizKind::TopList
                | VizKind::Pie
                | VizKind::Heatmap
                | VizKind::Table
                | VizKind::LogStream
                | VizKind::Note
                | VizKind::Spacer
                | VizKind::MonitorList
        )
    }
}

/// What kind of query a tile runs. `Mpl` is the only variant exercised
/// today; the others are placeholders for steps 14–16.
#[derive(Clone, Debug)]
#[allow(dead_code)] // variants and inner strings populated in later steps
pub enum Query {
    /// Metrics MPL query, sent to `/v1/query/_mpl`.
    Mpl(String),
    /// APL query, sent to `/v1/datasets/_apl`. Step 14+.
    Apl(String),
    /// Markdown body for a note tile. Step 16.
    Note(String),
    /// No query (spacer, monitor-list-without-filter, etc.). Step 16.
    Empty,
}

impl Query {
    /// Borrow the query text when it has one. Used by the editor binding
    /// (step 18) and by the existing query runner.
    #[allow(dead_code)]
    pub fn text(&self) -> Option<&str> {
        match self {
            Query::Mpl(s) | Query::Apl(s) | Query::Note(s) => Some(s.as_str()),
            Query::Empty => None,
        }
    }
}

/// Coordinates in the dashboard grid. Step 11 never reads these (single
/// tile spans the entire pane); step 18 turns them into ratatui `Rect`s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridPos {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// Grid layout parameters. `cols` is the column count, `row_h` the
/// per-row height in terminal cells. Defaults match a typical Axiom
/// dashboard (12 cols, 6-cell rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub cols: u16,
    pub row_h: u16,
}

impl Default for Layout {
    fn default() -> Self {
        Self { cols: 12, row_h: 6 }
    }
}

/// A single visualisation in a [`Dashboard`].
///
/// Step 11 only consumes `kind`, `opts`, and `query`. The rest are
/// modelled now so steps 17 and 18 can populate them without churning
/// callers — hence the per-field `dead_code` allow.
#[derive(Clone, Debug)]
pub struct Tile {
    #[allow(dead_code)]
    pub id: TileId,
    #[allow(dead_code)]
    pub title: String,
    pub kind: VizKind,
    pub opts: BTreeMap<String, String>,
    pub query: Query,
    #[allow(dead_code)]
    pub time_override: Option<TimeRange>,
    #[allow(dead_code)]
    pub pos: GridPos,
}

impl Tile {
    /// Convenience: a tile that wraps an MPL buffer for single-buffer mode.
    pub fn from_mpl(
        id: TileId,
        mpl: String,
        kind: VizKind,
        opts: BTreeMap<String, String>,
    ) -> Self {
        Self {
            id,
            title: String::new(),
            kind,
            opts,
            query: Query::Mpl(mpl),
            time_override: None,
            pos: GridPos::default(),
        }
    }

    /// Build a tile from an Axiom wire `Chart`. The chart's `query`
    /// field is decoded into one of `Query::Mpl | Apl | Empty`; the
    /// `name` becomes the tile title; layout is supplied separately
    /// from the matching `LayoutItem` (paired by chart id).
    pub fn from_chart(id: TileId, chart: &Chart, layout: Option<&LayoutItem>) -> Self {
        let base = chart.base();
        let kind = VizKind::from_chart(chart);
        let title = base.name.clone().unwrap_or_default();
        let query = extract_query(chart);
        let pos = layout.map(|l| GridPos {
            x: l.x as u16,
            y: l.y.unwrap_or(0) as u16,
            w: l.w as u16,
            h: l.h as u16,
        });
        Self {
            id,
            title,
            kind,
            opts: BTreeMap::new(),
            query,
            time_override: None,
            pos: pos.unwrap_or_default(),
        }
    }

    /// Set the MPL query text (single-tile, single-buffer mode).
    pub fn set_mpl(&mut self, mpl: String) {
        self.query = Query::Mpl(mpl);
    }
}

/// Public form of [`extract_query`] for callers outside this module
/// (notably `App::run_tile_queries`, which needs to know whether to
/// fan out a fetch for a given chart). Mirrors the internal mapping
/// exactly so the fetcher and the tile model agree on what's MPL.
pub fn classify_chart_query(chart: &Chart) -> Query {
    extract_query(chart)
}

/// Extract the executable query string from an Axiom `Chart`.
///
/// **Reality check (verified against the real v2 API)**: the public
/// OpenAPI documents `{ "mpl": "…" }` and `{ "apl": "…" }` as
/// alternative shapes, but every chart in practice ships its query
/// under the `apl` key regardless of language. We can't trust the
/// key name to discriminate, so we feed the text to `mpl_lang::compile`
/// and let the real parser decide: if it parses as MPL, it's MPL;
/// otherwise it's APL.
///
/// Why parse-then-classify instead of pattern-match: APL and MPL both
/// use pipes, both can lead with a bare identifier, and the only
/// truly reliable answer comes from running the actual grammar.
///
/// Charts with no `query` fall back to `Query::Empty`.
fn extract_query(chart: &Chart) -> Query {
    let _ = chart; // chart variant no longer affects classification
    let q = match chart.base().query.as_ref() {
        Some(v) => v,
        None => return Query::Empty,
    };
    // Explicit `mpl` key always wins (defensive: spec allows it).
    if let Some(mpl) = q.get("mpl").and_then(|v| v.as_str()) {
        return Query::Mpl(mpl.to_string());
    }
    // The `apl` key holds either language in practice. Try the MPL
    // parser; success means it's MPL, failure means APL.
    if let Some(text) = q.get("apl").and_then(|v| v.as_str()) {
        if is_valid_mpl(text) {
            return Query::Mpl(text.to_string());
        }
        return Query::Apl(text.to_string());
    }
    Query::Empty
}

/// Run the query through `mpl_lang::compile` with the host's
/// default system-param registry (notably `$__interval`, which the
/// Axiom server substitutes at runtime on every dashboard tile).
/// Returns `true` when the engine accepts it as MPL.
///
/// Without the registry, real-world MPL dashboards (e.g. the Home
/// Assistant one with 19 charts) all fail to compile with
/// `UndefinedParam { param: "__interval" }` even though their syntax
/// is perfectly valid MPL. Using the same registry the live editor
/// uses keeps classification consistent with what users see in solo
/// mode.
fn is_valid_mpl(text: &str) -> bool {
    use mpl_language_server::to_compile_params;
    use std::collections::HashMap;
    let specs = crate::mpl::engine_specs_for_defaults();
    let params: HashMap<_, _> = to_compile_params(&specs);
    mpl_lang::compile(text, params).is_ok()
}

/// A time-range expression. Strings are stored verbatim so they
/// round-trip through the dashboard file format unchanged (Axiom accepts
/// `now-1h`, RFC3339, etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeRange {
    pub start: String,
    pub end: String,
}

impl Default for TimeRange {
    fn default() -> Self {
        // Matches the existing `DEFAULT_START` / `DEFAULT_END` constants in
        // `app.rs` so step 11 is a no-op at runtime.
        Self {
            start: "now-1h".to_string(),
            end: "now".to_string(),
        }
    }
}

/// A complete dashboard. Today's single-buffer mode holds exactly one
/// tile here; steps 17+ load real multi-tile dashboards into this shape.
#[derive(Clone, Debug, Default)]
pub struct Dashboard {
    #[allow(dead_code)]
    pub id: Option<String>,
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub time_range: TimeRange,
    #[allow(dead_code)]
    pub variables: BTreeMap<String, String>,
    pub tiles: Vec<Tile>,
    #[allow(dead_code)]
    pub layout: Layout,
    next_tile_id: TileId,
}

impl Dashboard {
    /// Build a single-tile dashboard wrapping the given MPL buffer with
    /// the given viz kind + options. This is the constructor the `App`
    /// uses on file load.
    pub fn single_tile_from_mpl(
        mpl: String,
        kind: VizKind,
        opts: BTreeMap<String, String>,
    ) -> Self {
        let mut d = Self {
            name: "untitled".to_string(),
            ..Default::default()
        };
        let id = d.next_id();
        d.tiles.push(Tile::from_mpl(id, mpl, kind, opts));
        d
    }

    /// Adopt an Axiom `DashboardResource` (wire type) into the
    /// internal model. Each `Chart` becomes a `Tile`; matching
    /// `LayoutItem`s populate `Tile.pos`. Charts with no extractable
    /// MPL/APL still produce a tile so the dashboard's structure is
    /// preserved — the renderer just shows a placeholder body.
    ///
    /// If the resource has no charts (rare: empty new dashboard), a
    /// single empty tile is created so `focused_tile()` doesn't panic.
    pub fn from_resource(resource: &DashboardSummary) -> Self {
        let doc = &resource.dashboard;
        let mut d = Self {
            id: Some(resource.uid.clone()),
            name: resource.name().to_string(),
            time_range: TimeRange {
                start: doc
                    .time_window_start
                    .clone()
                    .unwrap_or_else(|| "now-1h".to_string()),
                end: doc
                    .time_window_end
                    .clone()
                    .unwrap_or_else(|| "now".to_string()),
            },
            ..Default::default()
        };
        for chart in &doc.charts {
            let id = d.next_id();
            let layout = doc.layout.iter().find(|l| l.i == chart.base().id);
            d.tiles.push(Tile::from_chart(id, chart, layout));
        }
        if d.tiles.is_empty() {
            // Keep the focused-tile invariant: a brand-new empty
            // dashboard still gets one placeholder tile.
            let id = d.next_id();
            d.tiles.push(Tile {
                id,
                title: "(empty)".to_string(),
                kind: VizKind::Note,
                opts: BTreeMap::new(),
                query: Query::Note("_This dashboard has no charts yet._".to_string()),
                time_override: None,
                pos: GridPos::default(),
            });
        }
        d
    }

    fn next_id(&mut self) -> TileId {
        let id = self.next_tile_id;
        self.next_tile_id = self.next_tile_id.wrapping_add(1);
        id
    }

    /// The first (and, in step 11, only) tile. Panics if empty — a
    /// `Dashboard` is never constructed without at least one tile, but
    /// we don't enforce that statically.
    pub fn focused_tile(&self) -> &Tile {
        self.tiles
            .first()
            .expect("Dashboard always has at least one tile in step 11")
    }

    /// Mutable variant of [`focused_tile`].
    pub fn focused_tile_mut(&mut self) -> &mut Tile {
        self.tiles
            .first_mut()
            .expect("Dashboard always has at least one tile in step 11")
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn single_tile_dashboard_carries_kind_and_opts() {
        let mut opts = BTreeMap::new();
        opts.insert("n".to_string(), "10".to_string());
        let d = Dashboard::single_tile_from_mpl("foo:bar".to_string(), VizKind::Bar, opts);
        assert_eq!(d.tiles.len(), 1);
        let t = d.focused_tile();
        assert_eq!(t.kind, VizKind::Bar);
        assert_eq!(t.opts.get("n").map(String::as_str), Some("10"));
        assert!(matches!(&t.query, Query::Mpl(s) if s == "foo:bar"));
    }

    use crate::axiom::{ChartBase, DashboardDocument, DashboardSummary, LayoutItem};
    use serde_json::json;

    fn chart_with_mpl(id: &str, name: &str, mpl: &str) -> Chart {
        Chart::TimeSeries(ChartBase {
            id: id.to_string(),
            name: Some(name.to_string()),
            query: Some(json!({ "mpl": mpl })),
            extras: Default::default(),
        })
    }

    // Fixtures lifted verbatim from `GET /v2/dashboards/uid/…` against
    // a real account. Two MPL examples (home overview) and one APL
    // example (probe-* dashboards).
    const REAL_MPL_BACKTICK_STAT: &str = "`home`:temp\n| where type == \"temperature\"\n| where room != \"Außen\"\n| group using avg";
    const REAL_MPL_BACKTICK_TIMESERIES: &str =
        "`home`:power\n| group by circuit using sum\n| align to 5m using avg";
    const REAL_APL_BRACKET: &str =
        "[\"axiom-audit-logs\"] | summarize n=count() by bin_auto(_time)";

    fn statistic_with_apl(text: &str) -> Chart {
        Chart::Statistic(crate::axiom::ChartBase {
            id: "c1".into(),
            name: None,
            query: Some(json!({ "apl": text })),
            extras: Default::default(),
        })
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
        let chart = Chart::TimeSeries(crate::axiom::ChartBase {
            id: "c1".into(),
            name: None,
            query: Some(json!({ "apl": REAL_MPL_BACKTICK_TIMESERIES })),
            extras: Default::default(),
        });
        assert!(matches!(extract_query(&chart), Query::Mpl(_)));
    }

    #[test]
    fn real_probe_apl_with_bracketed_dataset_classifies_as_apl() {
        // `["axiom-audit-logs"] | summarize n=count() by …` — stored
        // under `apl` on a TimeSeries chart. Pipes don't make this
        // MPL; the leading `[` does make it APL.
        let chart = Chart::TimeSeries(crate::axiom::ChartBase {
            id: "c1".into(),
            name: None,
            query: Some(json!({ "apl": REAL_APL_BRACKET })),
            extras: Default::default(),
        });
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
        let chart = Chart::TimeSeries(crate::axiom::ChartBase {
            id: "c1".into(),
            name: None,
            query: Some(json!({
                "mpl": "correct:value",
                "apl": "['logs'] | count",
            })),
            extras: Default::default(),
        });
        match extract_query(&chart) {
            Query::Mpl(s) => assert_eq!(s, "correct:value"),
            other => panic!("expected Mpl, got {other:?}"),
        }
    }

    #[test]
    fn from_resource_maps_chart_types_to_viz_kinds() {
        let resource = DashboardSummary {
            uid: "u1".into(),
            id: None,
            updated_at: None,
            updated_by: None,
            version: None,
            dashboard: DashboardDocument {
                name: Some("d".into()),
                charts: vec![
                    chart_with_mpl("c1", "latency", "http_latency:p99"),
                    Chart::Pie(ChartBase {
                        id: "c2".into(),
                        name: Some("by-region".into()),
                        query: Some(json!({ "apl": "['logs'] | summarize count() by region" })),
                        extras: Default::default(),
                    }),
                    Chart::TopK(ChartBase {
                        id: "c3".into(),
                        name: Some("errors".into()),
                        query: None,
                        extras: Default::default(),
                    }),
                ],
                ..Default::default()
            },
        };
        let d = Dashboard::from_resource(&resource);
        assert_eq!(d.id.as_deref(), Some("u1"));
        assert_eq!(d.tiles.len(), 3);
        assert_eq!(d.tiles[0].kind, VizKind::Line);
        assert_eq!(d.tiles[1].kind, VizKind::Pie);
        assert_eq!(d.tiles[2].kind, VizKind::TopList);
        assert!(matches!(
            &d.tiles[0].query,
            Query::Mpl(s) if s == "http_latency:p99"
        ));
        assert!(matches!(
            &d.tiles[1].query,
            Query::Apl(s) if s.starts_with("['logs']")
        ));
        assert!(matches!(d.tiles[2].query, Query::Empty));
    }

    #[test]
    fn from_resource_pairs_layout_by_chart_id() {
        let resource = DashboardSummary {
            uid: "u".into(),
            id: None,
            updated_at: None,
            updated_by: None,
            version: None,
            dashboard: DashboardDocument {
                name: Some("d".into()),
                charts: vec![chart_with_mpl("c1", "x", "a:b")],
                layout: vec![LayoutItem {
                    i: "c1".into(),
                    x: 3,
                    y: Some(2),
                    w: 6,
                    h: 4,
                    extras: Default::default(),
                }],
                ..Default::default()
            },
        };
        let d = Dashboard::from_resource(&resource);
        let pos = d.tiles[0].pos;
        assert_eq!(pos.x, 3);
        assert_eq!(pos.y, 2);
        assert_eq!(pos.w, 6);
        assert_eq!(pos.h, 4);
    }

    #[test]
    fn from_resource_creates_placeholder_tile_when_no_charts() {
        let resource = DashboardSummary {
            uid: "u".into(),
            id: None,
            updated_at: None,
            updated_by: None,
            version: None,
            dashboard: DashboardDocument {
                name: Some("empty".into()),
                ..Default::default()
            },
        };
        let d = Dashboard::from_resource(&resource);
        // Invariant: focused_tile never panics.
        assert_eq!(d.tiles.len(), 1);
        assert_eq!(d.focused_tile().kind, VizKind::Note);
    }

    #[test]
    fn from_resource_carries_time_window() {
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
        let d = Dashboard::from_resource(&resource);
        assert_eq!(d.time_range.start, "qr-now-7d");
        assert_eq!(d.time_range.end, "qr-now");
    }

    #[test]
    fn default_time_range_matches_legacy_constants() {
        let r = TimeRange::default();
        assert_eq!(r.start, "now-1h");
        assert_eq!(r.end, "now");
    }
}
