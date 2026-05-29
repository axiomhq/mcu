//! Supporting types for `App`: events, input modes, panes, view mode,
//! tile-edit submode, cmdline + completion state, dashboard picker,
//! yank entry, time-picker state, etc.
//!
//! These are all separated from `App` itself because they're
//! independent of its god-struct nature — most have small, focused
//! method surfaces. Keeping them here lets `mod.rs` shrink to the
//! parts that *do* depend on `App`'s ~45 fields.

use std::collections::{BTreeMap, HashSet};

use ratatui::layout::Rect;

use crate::axiom::{
    AplQueryResult, DashboardSummary, DashboardSummaryExt, DatasetSummary, MetricInfo,
    MetricsQueryResponse,
};
use crate::chart::Series;
use crate::completions;
use crate::dashboard::TimeRange;
use crate::mpl;
use crate::trace::TraceModel;

pub enum AppEvent {
    DatasetsFetched(anyhow::Result<Vec<DatasetSummary>>),
    MetricsFetched {
        dataset: String,
        result: anyhow::Result<BTreeMap<String, MetricInfo>>,
    },
    TagsFetched {
        dataset: String,
        metric: String,
        result: anyhow::Result<Vec<String>>,
    },
    TagValuesFetched {
        dataset: String,
        metric: String,
        tag: String,
        result: anyhow::Result<Vec<String>>,
    },
    QueryFinished {
        id: u64,
        result: anyhow::Result<MetricsQueryResponse>,
    },
    /// Standalone-buffer APL query result. Same id-based
    /// stale-result protection as [`AppEvent::QueryFinished`];
    /// the handler decodes via
    /// [`crate::viz::apl_decode::to_series`] and surfaces a
    /// decoder error in `status` if the response shape can't be
    /// reshaped into chart series (e.g. no time column).
    AplQueryFinished {
        id: u64,
        result: anyhow::Result<AplQueryResult>,
    },
    /// Result of a `:trace <id>` fetch (the ladder dispatch in
    /// `src/app/fetch/trace.rs`). Carries its own monotonically
    /// increasing `query_id` from `App.trace_query_counter` —
    /// **independent** of the editor's `last_query_id` so an
    /// in-flight editor `:r` can't accidentally cancel a trace
    /// fetch and vice versa. The handler checks
    /// `pending_trace_fetch.query_id == query_id` to drop
    /// superseded results (user ran a second `:trace` while the
    /// first was still searching).
    TraceFetchFinished {
        query_id: u64,
        result: anyhow::Result<AplQueryResult>,
    },
    DashboardsFetched(anyhow::Result<Vec<DashboardSummary>>),
    /// Single dashboard fetched via `GET /v2/dashboards/uid/{uid}`,
    /// triggered by selecting an entry in the picker or running
    /// `:open <uid>`. `uid` is carried alongside the result so the
    /// handler can surface a contextual error if the fetch fails.
    DashboardOpened {
        uid: String,
        result: anyhow::Result<DashboardSummary>,
    },
    /// Result of `:dash save` (PUT). On success the server returns the
    /// new resource with a bumped version; the handler stamps that
    /// version onto `loaded_dashboard` so the next save doesn't 412.
    DashboardSaved {
        uid: String,
        result: anyhow::Result<crate::axiom::DashboardWriteResponse>,
    },
    /// Result of `:dash rm` (DELETE). On success the handler clears
    /// `loaded_dashboard` if its uid matches the one we just deleted.
    DashboardDeleted {
        uid: String,
        result: anyhow::Result<()>,
    },
    /// Result of a single per-tile MPL query, kicked off in parallel
    /// when a dashboard is adopted in Grid view. `chart_id` is the
    /// wire chart id (`ChartBase.id`); the handler stores the result
    /// in `App.tile_results` under that key, but only when `epoch`
    /// matches `App.tile_query_epoch` — stale results from a
    /// superseded dashboard load are dropped on arrival.
    TileQueryFinished {
        chart_id: String,
        epoch: u64,
        result: anyhow::Result<MetricsQueryResponse>,
    },
    /// Per-tile APL query result. Parallel to
    /// [`AppEvent::TileQueryFinished`] but carries the
    /// [`AplQueryResult`] table shape; the handler picks between
    /// [`crate::viz::apl_decode::to_series`] and
    /// [`crate::viz::apl_decode::to_table_result`] based on the
    /// focused tile's current viz kind. Stale-result protection via
    /// the same `epoch` mechanism.
    TileAplFinished {
        chart_id: String,
        epoch: u64,
        result: anyhow::Result<AplQueryResult>,
    },
    /// Background refresh of the org's dashboard list. Fires after a
    /// cached list was shown immediately on `:dash ls`. Errors are
    /// surfaced quietly via `status` so they don't disrupt the picker.
    DashboardsRefreshed(anyhow::Result<Vec<DashboardSummary>>),
    /// Background refresh of a single dashboard by uid. Fires after a
    /// cached resource was adopted instantly. The handler updates the
    /// cached version metadata and, if the editor buffer is still
    /// pristine from the original adopt, re-adopts the fresh copy.
    DashboardRefreshed {
        uid: String,
        result: anyhow::Result<DashboardSummary>,
    },
}

/// Per-tile query state for the grid renderer. Stored in
/// `App.tile_results` and consumed by `draw_dashboard_grid` to render
/// live data in each grid cell instead of just an MPL preview.
#[derive(Debug, Clone, Default)]
pub struct TileQueryResult {
    /// `true` while the async fetch is in-flight; flips to `false` on
    /// success or error.
    pub busy: bool,
    /// Last successful series snapshot. Kept across errors so an
    /// occasional failed refresh doesn't blank the tile. Populated by
    /// the MPL path and by the APL path when the response decodes
    /// into series via [`crate::viz::apl_decode::to_series`].
    pub series: Vec<Series>,
    /// Last successful tabular result, when the tile's viz kind is
    /// `Table` / `LogStream` or when the APL response can't be
    /// reshaped into series. Mutually exclusive with a non-empty
    /// [`Self::series`] in practice — the dispatch in
    /// [`crate::app::App::run_tile_queries`] writes into exactly one
    /// of the two based on the viz kind + decoder verdict.
    pub table: Option<crate::viz::TableResult>,
    /// Last error message, if the most recent fetch failed.
    pub error: Option<String>,
    /// Server trace id from the most recent successful fetch.
    /// Surfaced by `:trace` so the user can grab it for support/debug.
    pub trace_id: Option<String>,
    /// Monotonic instant the in-flight fetch was kicked off. `Some`
    /// while `busy` is `true`, then consumed into [`Self::elapsed`]
    /// when the result lands. Use `Instant` (not wall clock) so the
    /// elapsed value can't go negative if the system clock jumps.
    pub started_at: Option<std::time::Instant>,
    /// Elapsed duration of the most recent completed fetch, measured
    /// monotonically as `Instant::now() - started_at` (not wall clock).
    /// Cleared while a new fetch is in flight so the tile border
    /// doesn't display a stale time over a spinner.
    pub elapsed: Option<std::time::Duration>,
    /// OTEL/UCUM unit resolved for this tile's series. Set when the
    /// fetch lands, via the three-tier discovery in
    /// [`crate::app::helpers::resolve_unit`]: metric metadata, then
    /// `otel.metric.unit` series tag, then `// @unit` pragma. `None`
    /// when no source carried a recognised unit; the renderer
    /// formats axis ticks with no suffix and no scaling.
    pub unit: Option<crate::unit::Unit>,
}

/// Default time range applied to every MPL query (the `_mpl` endpoint accepts
/// relative expressions).
pub(super) const DEFAULT_START: &str = "now-1h";
pub(super) const DEFAULT_END: &str = "now";

/// Quick-select choices for the `:time` picker overlay. Each entry is
/// `(label_shown_in_picker, duration_passed_to_axiom)`; selecting one
/// applies `start = format!("now-{}", duration)` and `end = "now"`.
/// Ordered short-to-long so the cursor lands on a sensible default.
pub const TIME_PRESETS: &[(&str, &str)] = &[
    ("3h", "3h"),
    ("6h", "6h"),
    ("12h", "12h"),
    ("24h", "24h"),
    ("2d", "2d"),
    ("7d", "7d"),
    ("30d", "30d"),
    ("90d", "90d"),
];

/// Sentinel cursor index for the "Custom…" row in the preset picker;
/// it sits just below the last preset and transitions into the
/// calendar overlay when selected.
pub const TIME_PRESET_CUSTOM_INDEX: usize = TIME_PRESETS.len();

/// State for the `:time` overlay. The Presets variant is the
/// quick-select list; Custom opens a two-calendar date picker for
/// arbitrary start/end days.
#[derive(Debug, Clone)]
pub enum TimePickerState {
    Presets { cursor: usize },
    Custom(CustomRangePicker),
}

/// Help-modal state. `visible` toggles the overlay; `scroll` is the top
/// row of `docs/keys.md` currently visible (j / Ctrl-d / G adjust it).
/// Grouped because every consumer touches both fields together.
#[derive(Debug, Default, Clone, Copy)]
pub struct HelpState {
    pub visible: bool,
    pub scroll: u16,
}

/// Legend pane state. Every field is reshaped together on each new
/// query: `hidden` is resized to match the new series count;
/// `selected` is clamped if the series count shrank; `label_tags`
/// is reloaded from cache (AST-hash, then dataset+metric fallback);
/// `details_visible` and `details_cursor` are reset when the user
/// closes the details modal.
#[derive(Debug, Default, Clone)]
pub struct LegendState {
    pub selected: usize,
    pub hidden: Vec<bool>,
    pub details_visible: bool,
    pub details_cursor: usize,
    pub label_tags: Vec<String>,
    /// Vim `gg` is two presses: the first sets this flag, the second
    /// (also `g`) actually jumps to the first line. Any other key
    /// clears the flag. Shared between the legend pane and its
    /// details modal, both of which use the same handler shape.
    pub pending_g: bool,
}

/// Params pane state. `selected` is the cursor row over rows produced
/// by `mpl::param_rows`; clamped after edits. `system` holds host-
/// supplied values (`$__interval` etc.). `cli` holds `-p NAME=VALUE`
/// arguments. The two dicts are always passed together to MPL routines.
#[derive(Debug, Default, Clone)]
pub struct ParamsState {
    pub selected: usize,
    pub system: Vec<crate::params::SystemParam>,
    pub cli: BTreeMap<String, String>,
}

/// Time-range + `:time` picker state. `picker` is `Some` while the
/// overlay is open; the variant inside distinguishes preset-list mode
/// from custom-date mode. `range` is the active query window, used by
/// every tile and the editor's `:r` runs. Mutated by the picker on
/// apply and by [`crate::app::App::adopt_dashboard`] on dashboard load.
#[derive(Debug, Default, Clone)]
pub struct TimeState {
    pub picker: Option<TimePickerState>,
    pub range: TimeRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomField {
    Start,
    End,
}

/// State for the Custom date-picker overlay. Both endpoints carry a
/// `time::Date`; on apply the start becomes `YYYY-MM-DDT00:00:00Z` and
/// the end becomes `YYYY-MM-DDT23:59:59Z` so the chosen days are fully
/// inclusive.
#[derive(Debug, Clone)]
pub struct CustomRangePicker {
    pub start: time::Date,
    pub end: time::Date,
    pub focus: CustomField,
}

impl CustomRangePicker {
    /// Seed both endpoints to (yesterday → today) UTC so the user has a
    /// meaningful starting window even before they touch the cursor.
    pub fn seed() -> Self {
        let today = time::OffsetDateTime::now_utc().date();
        let yesterday = today.checked_sub(time::Duration::days(1)).unwrap_or(today);
        Self {
            start: yesterday,
            end: today,
            focus: CustomField::Start,
        }
    }

    /// Mutable accessor for the currently-focused date so the keymap
    /// can shift it without re-matching on `focus`.
    pub(super) fn focused_mut(&mut self) -> &mut time::Date {
        match self.focus {
            CustomField::Start => &mut self.start,
            CustomField::End => &mut self.end,
        }
    }

    /// Shift the focused date by `days` days, clamping to the valid
    /// `time::Date` range so we never panic on Jan-1-Min / Dec-31-Max.
    pub fn shift_days(&mut self, days: i64) {
        let d = *self.focused_mut();
        if let Some(next) = d.checked_add(time::Duration::days(days)) {
            *self.focused_mut() = next;
        }
    }

    /// Move the focused date by one month (positive = forward,
    /// negative = back). Clamps day-of-month when the destination
    /// month is shorter (Jan 31 + 1 month → Feb 28/29).
    pub fn shift_month(&mut self, delta: i32) {
        let d = *self.focused_mut();
        let (mut y, mut m) = (d.year(), u8::from(d.month()) as i32);
        m += delta;
        while m < 1 {
            m += 12;
            y -= 1;
        }
        while m > 12 {
            m -= 12;
            y += 1;
        }
        let month = match time::Month::try_from(m as u8) {
            Ok(mo) => mo,
            Err(_) => return,
        };
        // Clamp the day to the destination month's length.
        let max_day = month.length(y);
        let day = d.day().min(max_day);
        if let Ok(next) = time::Date::from_calendar_date(y, month, day) {
            *self.focused_mut() = next;
        }
    }

    /// Convert the picker into Axiom-acceptable RFC3339 strings.
    /// `start` is midnight UTC; `end` is 23:59:59 UTC so the chosen
    /// end day is fully included.
    pub fn to_range(&self) -> (String, String) {
        let (lo, hi) = if self.end < self.start {
            (self.end, self.start)
        } else {
            (self.start, self.end)
        };
        (format!("{lo}T00:00:00Z"), format!("{hi}T23:59:59Z"))
    }
}

/// Discovery window for `list_metrics`. The `metrics/info` endpoint only accepts
/// RFC3339 timestamps, so we materialise these per-request from system time.
pub(super) const DISCOVERY_WINDOW_HOURS: i64 = 24;

pub(super) fn rfc3339_now_window(hours_back: i64) -> (String, String) {
    let end = time::OffsetDateTime::now_utc();
    let start = end - time::Duration::hours(hours_back);
    let fmt = &time::format_description::well_known::Rfc3339;
    (
        start.format(fmt).expect("rfc3339 start"),
        end.format(fmt).expect("rfc3339 end"),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    /// Vim-style ex-command line. Entered by pressing `:` in Normal mode.
    /// The status bar becomes an input field; `Enter` runs the command,
    /// `Esc` cancels back to Normal mode.
    Command,
    /// Visual mode — motions extend a live selection from
    /// [`App::visual_anchor`] to the cursor. Operators (`d`/`c`/`y`/`>`/`<`)
    /// apply to that range and return to Normal mode.
    Visual,
    /// Linewise Visual mode — selection is rounded to whole lines.
    VisualLine,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        }
    }
}

/// Memo of the most recent `f`/`F`/`t`/`T` find target so `;` / `,` can
/// repeat it. Cleared by `Esc` only via the parser's `reset`.
#[derive(Debug, Clone, Copy)]
pub struct FindMemo {
    pub ch: char,
    pub forward: bool,
    pub till: bool,
}

/// Which surface receives keystrokes. The legend is interactive: scroll
/// through series, toggle visibility, show tag details. The editor handles
/// everything else — there's only one focusable editor for now (multi-tab
/// support is on the backlog).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Editor,
    Legend,
    /// Right-hand pane below the legend, listing user-declared params
    /// (from the buffer) plus any CLI/`:p` provided values. Focusable so
    /// the user can add / edit / clear values without typing colon
    /// commands directly.
    Params,
    /// Dashboard grid pane: shown when `App.view_mode == Grid`,
    /// replacing the single graph area with a layout of bordered tile
    /// chrome blocks. Arrow keys cycle selection; `Enter`/`v` zooms
    /// back into Solo on the selected tile.
    Dashboard,
    /// Solo Table viz pane: focused when the user wants to navigate
    /// the rows of an APL table result. Only enterable while
    /// `App.table_result.is_some()` — `set_focus` refuses otherwise.
    /// `j/k/g/G/Ctrl-D/Ctrl-U/PgDn/PgUp` move the selection,
    /// `Esc/h/Left` returns to the editor.
    Table,
    /// Trace span tree pane: focused while `App.view_mode ==
    /// ViewMode::Trace`. Only enterable when `App.trace_view`
    /// is `Some` — [`super::App::set_focus`] refuses otherwise.
    /// Pairs with [`Pane::TraceDetail`] (the right-side detail
    /// pane); `Tab` swaps between the two.
    TraceTree,
    /// Trace detail pane: right-hand column of the trace view
    /// rendering the selected span's identity / timing / status /
    /// attributes / events. Same gating as [`Pane::TraceTree`]
    /// (only enterable when `trace_view.is_some()`). Owns its
    /// own scroll offset (`TraceView.detail_scroll`).
    TraceDetail,
}

/// Where the main visualisation area focuses. `Solo` is the
/// long-standing single-tile renderer; `Grid` shows all of a loaded
/// dashboard's charts at once. `Trace` is the indented span-tree
/// view opened by `:trace <id>` (step 22); the entire body region
/// is handed to the trace renderer, and editor / params / legend
/// chrome is hidden.
///
/// Solo is the default for fresh sessions and `.mpl` buffers;
/// loading a multi-chart dashboard auto-switches to Grid
/// (overridable with `:solo`).
/// Per-surface geometry stashed by the renderer each frame and
/// consumed by [`super::App::on_mouse`] on the next event. Mirrors
/// the existing "stash geometry during draw, consume next frame"
/// convention already used for `last_trace_body_height` etc. Every
/// rect is one frame behind; at the 100ms event poll the staleness is
/// imperceptible, and hit-tests are additionally gated by the current
/// `view_mode` so a rect left over from another view can't misfire.
///
/// Rects default to the zero rect (`0×0` at origin), which never
/// matches a real click — so before the first `draw` the mouse is
/// inert rather than wrong.
#[derive(Debug, Clone, Default)]
pub struct MouseGeometry {
    /// Topbar row (`root[0]`) and the end-x columns of the `QUERY`
    /// and `DASHBOARD` tab labels, used to discriminate tab clicks.
    pub topbar: Rect,
    pub topbar_query_end_x: u16,
    pub topbar_dash_end_x: u16,
    /// Editor pane outer rect (border-inclusive, for focus clicks)
    /// plus its inner rect and first-visible-row scroll offset for
    /// translating a click cell into a `(row, col)` buffer position.
    pub editor: Rect,
    pub editor_inner: Rect,
    pub editor_scroll_top: usize,
    /// Solo-view secondary panes (border-inclusive outer rects).
    pub legend: Rect,
    pub params: Rect,
    /// Solo graph / table pane.
    pub graph: Rect,
    /// Grid view: dashboard pane outer rect and the per-tile
    /// `(chart_idx, rect)` list rebuilt every frame.
    pub dashboard: Rect,
    pub grid_tiles: Vec<(usize, Rect)>,
    /// Trace view: tree body rect + the scroll origin used that frame
    /// (so a click row maps to `visible[scroll + dy]`), and the detail
    /// pane rect.
    pub trace_tree_body: Rect,
    pub trace_tree_scroll: usize,
    pub trace_detail: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Solo,
    Grid,
    /// `:trace <id>` is active. `App.trace_view` carries the
    /// fetched model; the trace renderer in `src/ui/trace.rs`
    /// owns the body. Exit via `Esc` or `:q` restores
    /// `App.trace_view.return_mode` (the [`ViewMode`] active
    /// when `:trace` was invoked).
    Trace,
}

/// Ladder windows walked by the trace fetcher when an earlier,
/// narrower window came up empty. Ordered narrowest → widest;
/// [`Self::next`] returns the next wider window or `None` once we
/// hit `Month`, at which point the fetch surfaces a clean "not
/// found" error rather than walking further.
///
/// The choice of windows (1h / 24h / 7d / 30d) matches both the
/// `:traces ls` default (step 25) and Axiom's typical trace
/// retention bracket. 30d is the practical ceiling — most edges
/// don't keep traces longer than that, and a stale id past 30d is
/// almost certainly a typo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceFetchWindow {
    Hour,
    Day,
    Week,
    Month,
}

impl TraceFetchWindow {
    /// Axiom-relative start expression for [`crate::axiom::Client::query_apl`].
    pub fn as_relative_start(self) -> &'static str {
        match self {
            Self::Hour => "now-1h",
            Self::Day => "now-24h",
            Self::Week => "now-7d",
            Self::Month => "now-30d",
        }
    }

    /// Short human-readable label used in status-bar messages
    /// (`searching now-7d…`). Identical to the relative-start
    /// expression today, so it just forwards.
    pub fn label(self) -> &'static str {
        self.as_relative_start()
    }

    /// Next wider window in the ladder, or `None` when we've
    /// exhausted the search. Critical that `Month.next()` returns
    /// `None` — returning `Some(Month)` would loop forever.
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Hour => Some(Self::Day),
            Self::Day => Some(Self::Week),
            Self::Week => Some(Self::Month),
            Self::Month => None,
        }
    }
}

/// In-flight `:trace <id>` ladder state. Set when the dispatch
/// fires; updated in place each time an empty result bumps the
/// window; cleared when a non-empty result lands (transition into
/// [`ViewMode::Trace`]), when the ladder exhausts, or when the
/// user cancels with `Esc`.
///
/// `query_id` is **independent** of `App.last_query_id` — see
/// [`AppEvent::TraceFetchFinished`] for the rationale.
#[derive(Debug, Clone)]
pub struct PendingTraceFetch {
    pub query_id: u64,
    pub trace_id: String,
    pub dataset: String,
    pub deployment_override: Option<String>,
    pub window: TraceFetchWindow,
}

/// Materialised trace view. Built by the
/// [`AppEvent::TraceFetchFinished`] handler on a non-empty
/// result, owned by `App.trace_view`, and torn down on `Esc` /
/// `:q` exit.
///
/// `return_mode` is captured at construction time so the user
/// returns to exactly where they came from — a trace opened
/// from Grid view returns to Grid; one opened from Solo
/// returns to Solo.
#[derive(Debug, Clone)]
pub struct TraceView {
    pub model: TraceModel,
    pub cursor: usize,
    pub scroll: u16,
    /// Scroll offset for the right-hand detail pane (in rows
    /// from the top of the materialised section list). Lives
    /// on the view (not on `App`) because it's state of *this*
    /// trace — swapping to a different trace resets it.
    pub detail_scroll: u16,
    pub return_mode: ViewMode,
    /// `span_idx` (into `model.spans`) of every parent the user
    /// has folded shut with `h` / `zM`. Keyed by span_idx (not
    /// row index) so a future re-flatten of `model.tree` doesn't
    /// silently drop the fold state. Empty by default; `zR`
    /// clears it.
    pub collapsed: HashSet<usize>,
    /// Substring query for the `/` filter. Empty string means
    /// "filter inactive" — the renderer shows the full tree.
    /// Lowercased at the keymap layer so the per-frame match
    /// scan is case-insensitive without re-allocating.
    pub filter: String,
    /// Modal input state for the trace tree pane. `Normal` runs
    /// the j/k/h/l keymap; `Filter` routes every printable char
    /// into `filter`. Toggled by `/` (enter), `Esc` (cancel), and
    /// `Enter` (commit).
    pub input_mode: TraceInputMode,
    /// `z` two-step latch for `zM` / `zR` / `zv`. Same shape as
    /// `App.table_pending_g` — set true on a bare `z`, consumed
    /// (or cleared) by the very next keypress.
    pub pending_z: bool,
    /// Vim-style numeric count prefix accumulated across digit
    /// keystrokes (`10j` → move ten rows). `None` between
    /// sequences; built up by digit keys and consumed by the next
    /// motion. Lives on the view because it's trace-mode keystroke
    /// state, parallel to [`Self::pending_z`].
    pub pending_count: Option<usize>,
    /// Lazy per-span lowercased "search blob" — one string per
    /// span (indexed by span_idx) covering name, service, every
    /// attribute / resource value, and event names + attribute
    /// values. Built once on first `/` use; reused for every
    /// subsequent keystroke so the hot loop never re-traverses
    /// the typed structs.
    pub search_blobs: Option<Vec<String>>,
    /// Indices into `model.spans` of every span whose blob
    /// matched the *current* `filter`. Used by the renderer's
    /// `visible_rows` builder (ancestors are added on the fly).
    /// The keymap maintains this incrementally: appending a
    /// character narrows the prior match set; any other edit
    /// (Backspace / Esc / paste) triggers a full rescan.
    pub filter_matches: Option<Vec<usize>>,
}

impl TraceView {
    /// Cheap constructor used by the fetch handler. Initialises
    /// every step-24 field to its inactive default so the rest of
    /// the code can treat a fresh trace identically to a never-
    /// touched trace.
    pub fn new(model: TraceModel, return_mode: ViewMode) -> Self {
        Self {
            model,
            cursor: 0,
            scroll: 0,
            detail_scroll: 0,
            return_mode,
            collapsed: HashSet::new(),
            filter: String::new(),
            input_mode: TraceInputMode::Normal,
            pending_z: false,
            pending_count: None,
            search_blobs: None,
            filter_matches: None,
        }
    }

    /// Set of `span_idx` that must remain visible under the
    /// current filter — the matches plus every ancestor. Returns
    /// `None` when the filter is inactive (empty string), which
    /// the visible-rows builder treats as "everything passes".
    ///
    /// Cheap when no filter is active (one branch). When active,
    /// allocates a `HashSet` sized to `matches.len() * 2`; the
    /// 1.5k-span fixture stays comfortably inside the per-frame
    /// budget.
    pub fn filter_set(&self) -> Option<HashSet<usize>> {
        if self.filter.is_empty() {
            return None;
        }
        let matches = self.filter_matches.as_deref().unwrap_or(&[]);
        Some(crate::trace::ancestor_closure(&self.model, matches))
    }

    /// Row indices into `model.tree` that should appear in the
    /// viewport given the current fold + filter state. Recomputed
    /// each call; the renderer + keymap both call it and the
    /// O(tree.len()) cost is well below the 1ms/frame budget.
    pub fn visible_rows(&self) -> Vec<usize> {
        let set = self.filter_set();
        crate::trace::visible_rows(&self.model.tree, &self.collapsed, set.as_ref())
    }
}

/// Input mode for the trace tree pane. `Normal` runs the
/// motion / fold / yank keymap; `Filter` accumulates characters
/// into [`TraceView::filter`] until the user commits with `Enter`
/// or cancels with `Esc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TraceInputMode {
    #[default]
    Normal,
    Filter,
}

/// Tile editing sub-mode while focus is on `Pane::Dashboard`.
///
/// `Idle` is the default — arrow keys navigate selection. Any of
/// `m` / `s` / `d` / `a` enters a sub-mode where the keymap changes:
///
/// * `Move { original_layout, dx, dy }` — arrow keys accumulate a
///   delta. On every change the previewed layout is recomputed from
///   `original_layout + (dx, dy)` via [`super::tile_ops_shove::shove_move`],
///   so move-right-then-left correctly *undoes* an earlier shove
///   instead of leaving stranded victims. `Enter` commits the current
///   preview; `Esc` restores `original_layout` wholesale.
/// * `Resize { original_layout, dw, dh }` — same accumulator model
///   for resize via [`super::tile_ops_shove::shove_resize`]. Right/Down
///   grow `w`/`h`; Left/Up shrink (clamped to a 1-cell minimum and
///   12-col width).
/// * `ConfirmDelete` — `y` removes the selected tile; any other key
///   cancels. No keyboard accelerator can fire by accident here.
/// * `PickViz{cursor, action}` — viz-kind picker overlay shared by `a`
///   (add) and `o`/`O` (open new row); arrow keys move the cursor
///   across the implemented `VizKind`s and `Enter` commits via the
///   `action`-specific code path.
#[derive(Debug, Clone, Default)]
pub enum TileSubMode {
    #[default]
    Idle,
    Move {
        original_layout: Vec<crate::axiom::LayoutItem>,
        original_id: String,
        dx: i32,
        dy: i32,
    },
    Resize {
        original_layout: Vec<crate::axiom::LayoutItem>,
        original_id: String,
        dw: i32,
        dh: i32,
    },
    ConfirmDelete,
    /// Modal viz-kind picker. Used by both `a` (add tile at first
    /// free slot) and `o`/`O` (open a new row with a tile). The
    /// `action` carries the commit-time behaviour so the same
    /// keymap + overlay covers both.
    PickViz {
        cursor: usize,
        action: PickVizAction,
    },
}

/// Commit behaviour for [`TileSubMode::PickViz`].
#[derive(Debug, Clone, Copy)]
pub enum PickVizAction {
    /// `a` — insert a tile of the chosen kind at the first free
    /// grid slot.
    Add,
    /// `o`/`O` — open `remaining` new rows above/below the focused
    /// tile, each with a tile of the chosen kind. `5o` only prompts
    /// once; subsequent rows reuse the picked kind.
    Open { above: bool, remaining: usize },
}

/// A single tile captured by `y` / `x` for later paste. Stored in
/// `App.tile_yank`; survives navigation, view-mode flips and
/// dashboard swaps (vim's unnamed register behaves the same way
/// across buffers).
#[derive(Debug, Clone)]
pub struct TileSnapshot {
    /// Full chart (cloned). Id is rewritten on paste so multiple
    /// pastes don't collide.
    pub chart: crate::axiom::Chart,
    /// Layout entry as captured. Absolute coords; the paste path
    /// translates the bounding box to its new origin so multi-tile
    /// shapes round-trip exactly.
    pub layout: crate::axiom::LayoutItem,
}

/// One-level dashboard undo slot. Captured at the start of every
/// mutating dashboard command (move/resize commit, yank, cut, paste,
/// open, `:tile ...`). `u` swaps this with the live dashboard, so a
/// second `u` redoes the change — vim's `u` toggle.
#[derive(Debug, Clone)]
pub struct DashboardSnapshot {
    pub charts: Vec<crate::axiom::Chart>,
    pub layout: Vec<crate::axiom::LayoutItem>,
    pub selected_idx: usize,
    pub dirty: bool,
}

/// The (hash, dataset, metric) triple identifying the query whose results
/// are currently shown. Used to look up and persist the user's choice of
/// legend-label tags through the two-step cache fallback.
#[derive(Debug, Clone)]
pub struct QueryContext {
    pub hash: String,
    pub dataset: String,
    pub metric: String,
}

/// `:` command-line state: the line buffer + cursor, the Tab-completion
/// popup, and the pane to restore focus to on dismiss. Folded into a
/// single struct because opening / typing / dismissing the line is the
/// natural transaction boundary — all three fields are touched together.
#[derive(Default, Debug, Clone)]
pub struct CmdLine {
    /// Text after the `:` prompt, without the prompt itself.
    pub buf: String,
    /// Cursor position in `buf`, measured in chars (not bytes).
    pub cursor: usize,
    /// Tab-completion popup state. Cleared on cmdline dismiss.
    pub completions: CmdlineCompletionState,
    /// When `Some`, the pane to restore focus to when the line
    /// closes. Used by `prefill_command` so `a`/`e` from the Params
    /// pane returns there after submit. `None` for `:` entered from
    /// Normal mode.
    pub return_focus: Option<Pane>,
    /// Vim-style history navigation state. `None` while the user
    /// is editing the live buffer; `Some(i)` after the first Up,
    /// pointing at the recalled history entry. Cleared on dismiss
    /// and on any non-Up/Down/Enter/Esc key.
    pub history_cursor: Option<usize>,
    /// The filter prefix captured at the moment of the first Up,
    /// matching vim semantics: only entries whose text starts
    /// with this prefix are visited. The prefix is the buffer
    /// **up to the cursor** at capture time — chars after the
    /// cursor are discarded once the user starts walking.
    pub history_prefix: String,
    /// The live buffer + cursor stashed when the user first
    /// pressed Up, so Down-past-most-recent restores exactly what
    /// they were typing.
    pub history_stash: Option<(String, usize)>,
}

impl CmdLine {
    pub fn reset(&mut self) {
        self.buf.clear();
        self.cursor = 0;
        self.reset_history_nav();
    }

    /// Clear the history-navigation transients. Called both from
    /// [`reset`] (open / dismiss) and from every non-navigation
    /// key in `handle_command_key`, so the next Up re-captures a
    /// fresh prefix from whatever the user has now typed.
    ///
    /// [`reset`]: CmdLine::reset
    pub fn reset_history_nav(&mut self) {
        self.history_cursor = None;
        self.history_prefix.clear();
        self.history_stash = None;
    }

    pub(super) fn byte_cursor(&self) -> usize {
        self.buf
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.buf.len())
    }

    pub fn insert_char(&mut self, c: char) {
        let i = self.byte_cursor();
        self.buf.insert(i, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.byte_cursor();
        self.cursor -= 1;
        let start = self.byte_cursor();
        self.buf.drain(start..end);
    }

    /// Delete the word before the cursor, vim-style.
    ///
    /// Skips trailing whitespace, then removes a run of characters of
    /// the same class as the one just before the cursor — either
    /// keyword chars (alphanumeric and `_`, matching vim's default
    /// `iskeyword`) or non-keyword non-whitespace symbols. So
    /// `:dash ls|` becomes `:dash |` becomes `:|`, and `--deployment=|`
    /// peels off `deployment`, then `=`, then `--`. A pure-whitespace
    /// run before the cursor drains everything to the start.
    pub fn delete_word_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.byte_cursor();
        let chars: Vec<char> = self.buf.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i == 0 {
            self.buf.drain(..end);
            self.cursor = 0;
            return;
        }
        let keyword = |c: char| c.is_alphanumeric() || c == '_';
        let target_kw = keyword(chars[i - 1]);
        while i > 0 && !chars[i - 1].is_whitespace() && keyword(chars[i - 1]) == target_kw {
            i -= 1;
        }
        self.cursor = i;
        let start = self.byte_cursor();
        self.buf.drain(start..end);
    }

    pub fn delete_forward(&mut self) {
        let start = self.byte_cursor();
        if start >= self.buf.len() {
            return;
        }
        let next = self.buf[start..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| start + i)
            .unwrap_or(self.buf.len());
        self.buf.drain(start..next);
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_right(&mut self) {
        let max = self.buf.chars().count();
        if self.cursor < max {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.buf.chars().count();
    }
}

/// Visible state of the autocomplete popup. Refreshed on every keystroke
/// by [`App::refresh_completions`]; on accept we read `items[selected].apply`
/// directly — the engine + adapter have already pre-rendered the insert
/// text with the right backtick / keyword-snippet behaviour.
#[derive(Default, Debug)]
pub struct CompletionState {
    pub visible: bool,
    pub items: Vec<completions::CompletionItem>,
    pub selected: usize,
    /// Byte range in the (multi-line) joined editor text covered by the partial token
    /// that will be replaced on accept.
    pub replace_range_bytes: (usize, usize),
    /// Label for the completion category, displayed in the popup title.
    pub kind_label: &'static str,
    /// Engine-classified kind of the current completion. `None` when the
    /// popup is hidden. Used post-accept to trigger background prefetches
    /// (e.g. tag fetch after a metric pick).
    pub kind: Option<completions::CompletionKind>,
}

impl CompletionState {
    pub(super) fn hide(&mut self) {
        self.visible = false;
        self.items.clear();
        self.selected = 0;
        self.kind_label = "";
        self.kind = None;
    }
}

/// State for the `:` cmdline tab-completion popup. Lives on `App` so
/// the UI can read it without re-running the completer every frame,
/// and so successive Tabs cycle deterministically through the list.
#[derive(Debug, Default, Clone)]
pub struct CmdlineCompletionState {
    pub visible: bool,
    pub items: Vec<String>,
    pub selected: usize,
    /// Byte range in `cmdline.buf` covered by the partial token that
    /// each item replaces on accept. Matches the engine's
    /// `CompletionRequest.range` exactly.
    pub replace_range: (usize, usize),
}

impl CmdlineCompletionState {
    pub fn hide(&mut self) {
        self.visible = false;
        self.items.clear();
        self.selected = 0;
        self.replace_range = (0, 0);
    }
}

/// Open quick-fix picker. Items are the engine-supplied actions for the
/// diagnostic the cursor is sitting on; accept replaces `[byte_offset,
/// byte_offset + byte_length)` with `insert`, then we recompute diagnostics.
#[derive(Debug, Default)]
pub struct QuickFixPicker {
    pub visible: bool,
    pub actions: Vec<mpl::DiagnosticAction>,
    pub selected: usize,
    /// Header rendered above the action list — the diagnostic message.
    pub title: String,
}

impl QuickFixPicker {
    pub(super) fn hide(&mut self) {
        self.visible = false;
        self.actions.clear();
        self.selected = 0;
        self.title.clear();
    }
}

/// Searchable picker over the org's dashboards. Opened by `:dash ls`,
/// closed with `Esc`. Filter input is inline at the top of the modal;
/// every keystroke that isn't navigation extends or backspaces it.
///
/// Selection on `Enter` records the dashboard id on `App.last_picked_dashboard`
/// for step 17's `:open` to consume; today it just surfaces a status
/// message and closes — actual load lands when the dashboard file
/// format does.
#[derive(Debug, Default)]
pub struct DashboardPicker {
    pub visible: bool,
    /// All dashboards fetched from the server, in name order.
    pub items: Vec<DashboardSummary>,
    /// Substring filter applied case-insensitively to `name` and
    /// `description`. Empty filter = show everything.
    pub filter: String,
    /// Index into [`filtered_indices`] of the currently-highlighted row.
    pub cursor: usize,
}

impl DashboardPicker {
    pub fn hide(&mut self) {
        self.visible = false;
        self.filter.clear();
        self.cursor = 0;
    }

    pub fn open(&mut self, items: Vec<DashboardSummary>) {
        let mut sorted = items;
        sorted.sort_by_key(|a| a.name_or_unnamed().to_lowercase());
        self.items = sorted;
        self.filter.clear();
        self.cursor = 0;
        self.visible = true;
    }

    /// Replace `items` while preserving the visible state, the user's
    /// current `filter`, and — when possible — the highlighted uid.
    /// Used by the background-refresh path so the picker doesn't lose
    /// the user's place when fresh data arrives.
    pub fn refresh_items(&mut self, items: Vec<DashboardSummary>) {
        let selected_uid = self.selected().map(|d| d.uid.clone());
        let mut sorted = items;
        sorted.sort_by_key(|a| a.name_or_unnamed().to_lowercase());
        self.items = sorted;
        let n = self.filtered_indices().len();
        if n == 0 {
            self.cursor = 0;
            return;
        }
        if let Some(uid) = selected_uid {
            let indices = self.filtered_indices();
            if let Some(pos) = indices
                .iter()
                .position(|i| self.items.get(*i).is_some_and(|d| d.uid == uid))
            {
                self.cursor = pos;
                return;
            }
        }
        if self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    /// Indices into `items` that match the current filter, in original
    /// (sorted) order. Empty filter returns every index.
    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.items.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.name_or_unnamed().to_lowercase().contains(&needle)
                    || d.description()
                        .map(|s| s.to_lowercase().contains(&needle))
                        .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Bump the cursor by `delta`, clamped to the current filtered set.
    /// Returns the new cursor index for convenience.
    pub fn move_cursor(&mut self, delta: isize) -> usize {
        let n = self.filtered_indices().len();
        if n == 0 {
            self.cursor = 0;
            return 0;
        }
        let i = self.cursor as isize + delta;
        let wrapped = ((i % n as isize) + n as isize) % n as isize;
        self.cursor = wrapped as usize;
        self.cursor
    }

    /// The `DashboardSummary` currently under the cursor, if any.
    pub fn selected(&self) -> Option<&DashboardSummary> {
        let indices = self.filtered_indices();
        indices.get(self.cursor).and_then(|i| self.items.get(*i))
    }
}

/// Contents of the single yank register populated by `y`/`d`/`c` and
/// Which kind of artifact the editor + file commands operate on. Two
/// modes today — a long-standing single-buffer MPL workflow and the
/// dashboard mode introduced in step 17 — distinguished so `:w`
/// writes the right thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BufferMode {
    /// Editor buffer is canonical; `:w` writes the buffer text.
    #[default]
    Mpl,
    /// `loaded_dashboard` is canonical; `:w` writes the dashboard
    /// JSON. The editor still shows the focused tile's MPL/APL but
    /// changes to the buffer do not currently propagate back to the
    /// dashboard tile on save (deferred to 17d/17e).
    Dashboard,
}

/// Contents of the single yank register populated by `y`/`d`/`c` and
/// consumed by `p`/`P`. `linewise` decides whether paste opens a new
/// line (`true`) or splices at the cursor (`false`).
#[derive(Debug, Clone, Default)]
pub struct YankEntry {
    pub text: String,
    pub linewise: bool,
}
