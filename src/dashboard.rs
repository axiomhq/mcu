//! Viz-kind / query classification helpers shared by the editor, the
//! grid renderer, and the dashboard adoption path.
//!
//! Earlier revisions also held an internal `Dashboard`/`Tile`/`Layout`
//! model that mirrored the wire shape on `App`. That model was never
//! the source of truth — the grid renderer walks `axiom::Chart`
//! directly off `loaded_dashboard`, and only the focused tile's viz
//! kind / opts / time range ever changed during a session. Step 4 of
//! the cleanup plan collapsed those structures onto `App` directly
//! (`App.viz_kind`, `App.viz_opts`, `App.time.range`); what remained
//! here is the classifier surface every caller actually consumes.

use crate::axiom::{Chart, ChartBase, DashboardSummary, KnownChart};

/// Sidecar key on `ChartBase.extras` that records the query language
/// ax wrote for a tile. Lets [`extract_query`] reach a deterministic
/// verdict on tiles we authored without falling back to chart-kind
/// heuristics — flip a Statistic chart to APL with `:apl` and the
/// next reload still classifies it as APL even though the chart kind
/// would otherwise say MPL.
///
/// Foreign-authored charts (Axiom web UI, other tools) won't carry
/// this; they fall through to the kind-based rules below. Round-trips
/// because [`ChartBase.extras`] is a serde passthrough.
pub const LANG_SIDECAR_KEY: &str = "axLang";

/// Query language a tile is authored in. The discriminator that
/// [`extract_query`] returns alongside the query text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Lang {
    #[default]
    Mpl,
    Apl,
}

impl Lang {
    /// Sidecar-value form, written under [`LANG_SIDECAR_KEY`] in
    /// `ChartBase.extras` and read back by [`lang_from_sidecar`].
    pub fn as_sidecar(self) -> &'static str {
        match self {
            Lang::Mpl => "mpl",
            Lang::Apl => "apl",
        }
    }

    /// Display label for the status bar (`NORMAL · APL`).
    pub fn label(self) -> &'static str {
        match self {
            Lang::Mpl => "MPL",
            Lang::Apl => "APL",
        }
    }

    /// `:apl` / `:mpl` command name.
    pub fn ex_command(self) -> &'static str {
        match self {
            Lang::Mpl => "mpl",
            Lang::Apl => "apl",
        }
    }
}

/// Read the explicit-language sidecar from a chart, if ax wrote one.
/// Returns `None` for foreign charts that never carried the marker.
fn lang_from_sidecar(chart: &Chart) -> Option<Lang> {
    let base = chart.base()?;
    match base.extras.get(LANG_SIDECAR_KEY)?.as_str()? {
        "apl" => Some(Lang::Apl),
        "mpl" => Some(Lang::Mpl),
        _ => None,
    }
}

/// Narrow syntax sniff: `true` only when the leading non-whitespace
/// shape is **unambiguously** APL. We deliberately accept just two
/// prefixes — bracketed dataset (`['logs'] | ...`) and `let`
/// bindings (`let foo = ...`) — because MPL grammar permits
/// neither. Everything else (backtick datasets, bare identifiers,
/// comments, even unparseable garbage) is left to the chart-kind
/// fallback so we don't reintroduce the classifier drift the old
/// `mpl_lang::compile` approach suffered.
///
/// Used by [`extract_query`] as a final disambiguator when no
/// sidecar is present and the chart kind alone would mis-classify
/// user-authored APL on a metrics chart as MPL.
fn looks_like_apl(text: &str) -> bool {
    let s = text.trim_start();
    if s.starts_with('[') {
        return true;
    }
    if let Some(rest) = s.strip_prefix("let") {
        return rest.starts_with(|c: char| c.is_ascii_whitespace());
    }
    false
}

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
            Chart::Known(KnownChart::TimeSeries(_)) => VizKind::Line,
            Chart::Known(KnownChart::Heatmap(_)) => VizKind::Heatmap,
            Chart::Known(KnownChart::LogStream(_)) => VizKind::LogStream,
            Chart::Known(KnownChart::Pie(_)) => VizKind::Pie,
            Chart::Known(KnownChart::Scatter(_)) => VizKind::Scatter,
            Chart::Known(KnownChart::Table(_)) => VizKind::Table,
            Chart::Known(KnownChart::TopK(_)) => VizKind::TopList,
            Chart::Known(KnownChart::Statistic(_)) => VizKind::Statistic,
            Chart::Known(KnownChart::Note(_)) => VizKind::Note,
            Chart::Unknown(_) => VizKind::Line,
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

    /// `true` for the kinds whose renderer is implemented today.
    /// Used by tests to assert that the dispatch table in
    /// [`crate::viz::draw`] covers every variant.
    #[cfg(test)]
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

    /// Every variant in display order. Single source of truth for the
    /// add-tile / open-tile picker and the `:tile add` completion list.
    pub const ALL: &'static [VizKind] = &[
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
    ];

    /// Wrap a [`ChartBase`] in the wire `Chart` variant matching this
    /// kind. Inverse of [`VizKind::from_chart`].
    ///
    /// TUI-only kinds (`Bar`, `Area`, `MonitorList`, `Spacer`) don't have
    /// a dedicated wire variant; they fall back to `Chart::TimeSeries`
    /// so PUT round-trips cleanly. The TUI-only intent is preserved in
    /// the editor buffer's `// @viz` pragma.
    pub fn to_chart(self, base: ChartBase) -> Chart {
        match self {
            VizKind::Line | VizKind::Bar | VizKind::Area => {
                Chart::Known(KnownChart::TimeSeries(base))
            }
            VizKind::Scatter => Chart::Known(KnownChart::Scatter(base)),
            VizKind::Pie => Chart::Known(KnownChart::Pie(base)),
            VizKind::Heatmap => Chart::Known(KnownChart::Heatmap(base)),
            VizKind::Table => Chart::Known(KnownChart::Table(base)),
            VizKind::TopList => Chart::Known(KnownChart::TopK(base)),
            VizKind::Statistic => Chart::Known(KnownChart::Statistic(base)),
            VizKind::LogStream => Chart::Known(KnownChart::LogStream(base)),
            VizKind::Note => Chart::Known(KnownChart::Note(base)),
            VizKind::MonitorList | VizKind::Spacer => Chart::Known(KnownChart::TimeSeries(base)),
        }
    }
}

/// What kind of query a tile runs. `Mpl` and `Apl` are the runtime
/// variants; `Empty` covers charts with no query body (notes,
/// spacers, monitor-list-without-filter, etc.) — their renderer
/// doesn't read this field.
#[derive(Clone, Debug)]
pub enum Query {
    /// Metrics MPL query, sent to `/v1/query/_mpl`.
    Mpl(String),
    /// APL query, sent to `/v1/datasets/_apl`.
    Apl(String),
    /// No query (note bodies, spacers, etc.).
    Empty,
}

/// Extract the executable query string from an Axiom `Chart`.
///
/// Discrimination strategy: **sidecar marker, then chart kind**.
/// Earlier revisions ran `mpl_lang::compile` on the text and
/// inferred the language from the parser's verdict. That doesn't
/// work in practice — the local `mpl_lang` crate's grammar and
/// stdlib are subsets of what the Axiom server accepts, so valid
/// real-world MPL routinely failed the local check, flipped to
/// `Query::Apl`, and rendered as the "not yet executable" banner
/// with no data. The sidecar lets ax-authored tiles claim a
/// language explicitly; everything else falls through to the
/// kind-based rules.
///
/// Rules (in order):
///   1. `chart.extras["axLang"] == "apl"` → `Query::Apl` (ax wrote
///      this tile and stamped its language).
///   2. `chart.extras["axLang"] == "mpl"` → `Query::Mpl`.
///   3. Explicit `mpl` key on the query object → `Query::Mpl`.
///      Set by local edits in
///      [`crate::app::App::sync_buffer_to_focused_tile`] for MPL
///      tiles authored before sidecars existed; still honoured for
///      round-trip stability.
///   4. `apl` key on a `LogStream` chart → `Query::Apl`. LogStream
///      is genuinely APL on the Axiom side.
///   5. `apl` key on any other chart kind (or `Chart::Unknown`)
///      → `Query::Mpl`. Metrics chart kinds (TimeSeries,
///      Statistic, TopK, Heatmap, Pie, Scatter, Table, Note) ship
///      with MPL queries by default in the Axiom UI.
///   6. No query → `Query::Empty`.
///
/// Trade-off for rule 5: a foreign-authored `TimeSeries` chart with
/// genuine APL (no sidecar) will be dispatched to the MPL endpoint
/// and surface a server-side error in `tile_results.error`. That's
/// strictly better than the previous behaviour where the local
/// classifier also returned APL but the fetcher refused to dispatch
/// at all — the user saw "APL (not yet executable)" with no hint of
/// what was actually wrong. To flip a foreign tile to APL
/// explicitly, run `:apl` once and the sidecar takes over.
pub fn extract_query(chart: &Chart) -> Query {
    use crate::axiom::KnownChart;
    let Some(base) = chart.base() else {
        return Query::Empty;
    };
    let q = match base.query.as_ref() {
        Some(v) => v,
        None => return Query::Empty,
    };
    // The sidecar wins outright when it pins the language. Pull the
    // text from whichever key carries it; allow either so a tile that
    // was MPL yesterday and got `:apl`-flipped today still reads
    // cleanly even before the next save normalises the keys.
    let pick_text = |obj: &serde_json::Value| -> Option<String> {
        if let Some(v) = obj.get("mpl").and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
        if let Some(v) = obj.get("apl").and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
        None
    };
    match lang_from_sidecar(chart) {
        Some(Lang::Apl) => return pick_text(q).map(Query::Apl).unwrap_or(Query::Empty),
        Some(Lang::Mpl) => return pick_text(q).map(Query::Mpl).unwrap_or(Query::Empty),
        None => {}
    }
    // No sidecar: kind-based fallback (the historical behaviour),
    // plus a narrow syntax sniff to catch foreign-authored APL on
    // non-LogStream chart kinds (the Axiom web UI lets users write
    // APL on a TimeSeries/Pie/etc. chart; those have no sidecar so
    // chart-kind alone would mis-classify them as MPL).
    if let Some(mpl) = q.get("mpl").and_then(|v| v.as_str()) {
        return Query::Mpl(mpl.to_string());
    }
    if let Some(text) = q.get("apl").and_then(|v| v.as_str()) {
        return match chart {
            Chart::Known(KnownChart::LogStream(_)) => Query::Apl(text.to_string()),
            _ if looks_like_apl(text) => Query::Apl(text.to_string()),
            _ => Query::Mpl(text.to_string()),
        };
    }
    Query::Empty
}

/// The language a chart's query is in. Convenience wrapper around
/// [`extract_query`] when the caller only needs the discriminator.
pub fn extract_lang(chart: &Chart) -> Option<Lang> {
    match extract_query(chart) {
        Query::Mpl(_) => Some(Lang::Mpl),
        Query::Apl(_) => Some(Lang::Apl),
        Query::Empty => None,
    }
}

/// Convert the local-canonical query form into the wire form the v2
/// dashboards API expects. Locally, queries the editor mutated live
/// under the `mpl` key (so [`extract_query`] takes the explicit-key
/// shortcut and never has to ask the chart kind). On the wire,
/// every chart's query MUST live under the `apl` key regardless of
/// language — matching what the server returns on GET. Mutates the
/// passed document in place; intended to be called on a clone right
/// before PUT.
///
/// Any sibling extras on the query object are preserved. A chart
/// with no `mpl` key (e.g. a true APL banner, or already in wire
/// form) is left untouched.
pub fn normalize_queries_to_wire(doc: &mut crate::axiom::DashboardDocument) {
    for chart in &mut doc.charts {
        let Some(base) = chart.base_mut() else {
            continue;
        };
        // `extras` is `#[serde(flatten)]`, so any key here serializes
        // as a top-level field on the chart object. The Axiom server
        // PUT endpoint rejects unknown chart fields with a schema
        // error, so the language sidecar — a local-only marker —
        // must not leave the process. It's restored automatically on
        // reload via the layered classifier (sidecar would come back,
        // but in its absence the syntax sniff + chart-kind fallback
        // cover the common cases).
        base.extras.remove(LANG_SIDECAR_KEY);

        let Some(query) = base.query.as_mut() else {
            continue;
        };
        let Some(obj) = query.as_object_mut() else {
            continue;
        };
        if let Some(mpl_val) = obj.remove("mpl") {
            // If both keys somehow co-existed at this point, the
            // `mpl` value is authoritative — it carries the user's
            // most recent edit. The pre-existing `apl` is dropped.
            obj.insert("apl".to_string(), mpl_val);
        }
    }
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
        // Matches the legacy `DEFAULT_START` / `DEFAULT_END` constants
        // so file-mode startup is a no-op at runtime.
        Self {
            start: "now-1h".to_string(),
            end: "now".to_string(),
        }
    }
}

impl TimeRange {
    /// Build a `TimeRange` from a loaded dashboard resource, falling
    /// back to `now-1h` / `now` when the document omits either field.
    pub fn from_resource(resource: &DashboardSummary) -> Self {
        let doc = &resource.dashboard;
        Self {
            start: doc
                .time_window_start
                .clone()
                .unwrap_or_else(|| "now-1h".to_string()),
            end: doc
                .time_window_end
                .clone()
                .unwrap_or_else(|| "now".to_string()),
        }
    }
}

#[cfg(test)]
mod tests;
