use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::runtime::Handle;

/// Max concurrent per-tile MPL fetches in flight at once. A typical
/// dashboard has ≤1 dozen tiles so this is mostly a hard cap against
/// pathological 50+ tile dashboards bursting the Axiom edge.
const TILE_FETCH_CONCURRENCY: usize = 8;
use tui_textarea::{CursorMove, TextArea};

use crate::axiom::{Client as AxiomClient, DashboardSummary};
use crate::cache::Cache;
use crate::chart::Series;
use crate::command::{self, Command, InsertAt, Motion, Operator, Step, Target};
use crate::completions;
use crate::config::Config;
use crate::dashboard::{TimeRange, VizKind};
use crate::editor;
use crate::hover;
use crate::motion::{self, Range};
use crate::mpl;
use crate::params;
use crate::share;
use crate::viz;

mod clipboard;
mod completions_impl;
mod dashboard;
mod dashboard_cmd;
mod editing;
pub(crate) mod ex_cmds;
mod fetch;
mod file_io;
mod helpers;
// One UI-facing helper that benefits from being callable from the
// status-bar renderer; the rest of `helpers` stays private to `app`.
pub use helpers::humanize_time_range;
mod keys;
mod tile_layout;
mod tile_ops_shove;
mod types;

// Re-export the items external modules / tests reach into via
// `crate::app::*`. Internal helpers stay private and are pulled in
// through `use` below.
pub use tile_layout::{SpatialDir, build_dashboard_doc_from_buffer};
pub(crate) use tile_layout::{add_pick_kinds, pick_next_chart_in_direction, tile_ops};
pub use types::*;

use helpers::*;

pub struct App {
    pub mode: Mode,
    pub editor: TextArea<'static>,
    pub series: Vec<Series>,
    /// Table-shaped result from the most recent APL query in
    /// standalone-buffer mode. Set by the `AplQueryFinished`
    /// handler when the active viz kind is Table or LogStream;
    /// otherwise the handler clears it so a stale APL table can't
    /// bleed into a fresh MPL result. `None` in MPL mode.
    pub table_result: Option<crate::viz::TableResult>,
    /// Row index for the solo Table viz selection. Reset to 0
    /// whenever a new `table_result` lands; clamped to
    /// `len()-1` on render so a smaller follow-up response
    /// doesn't render off the end. Only meaningful while
    /// `table_result.is_some()`.
    pub table_selected: usize,
    /// `gg` two-step latch for the table pane (mirrors
    /// `LegendState::pending_g`). Reset on any non-`g` key so the
    /// modal feel matches vim.
    pub table_pending_g: bool,
    /// OTEL/UCUM unit resolved for the editor's current query, set
    /// when `QueryFinished` lands. Drives axis scaling in the solo
    /// chart pane the same way `TileQueryResult.unit` drives grid
    /// tiles. `None` when the discovery fall-through finds no unit.
    pub unit: Option<crate::unit::Unit>,
    pub status: String,
    /// Most recent error in full. Surfaced as a centred overlay over the chart
    /// pane when present; dismissed with `Esc` in Normal mode.
    pub last_error: Option<String>,
    pub should_quit: bool,
    pub busy: bool,
    /// Shared discovery cache; persisted to disk by background tasks.
    pub cache: Arc<RwLock<Cache>>,
    pub completions: CompletionState,
    pub quickfix: QuickFixPicker,
    pub cmdline: CmdLine,
    /// Persistent `:` command history. Loaded from disk in [`App::new`]
    /// and updated atomically on every successful submit.
    pub history: crate::history::History,
    /// `:history` overlay visibility. Toggled by the ex-command;
    /// dismissed by `Esc`/`q`/`Enter` while visible.
    pub history_overlay_visible: bool,
    /// Live diagnostics for the current buffer.
    /// Recomputed by [`App::recompute_diagnostics`] on every buffer-mutating key.
    pub diagnostics: Vec<mpl::Diagnostic>,
    /// Trace identifier of the most recently completed query, surfaced on the
    /// right of the status bar so users can correlate against server logs.
    /// `None` before the first run or when the response carried no trace.
    pub last_trace_id: Option<String>,
    /// Help modal state. Grouped because `visible` and `scroll` are
    /// always touched together (open -> reset scroll; close -> reset both).
    pub help: HelpState,
    /// Hover popup contents. `Some` when the user pressed `K` over a known
    /// stdlib function; any subsequent key dismisses it.
    pub hover: Option<hover::HoverInfo>,
    /// Active signature-help line, recomputed alongside diagnostics on
    /// every buffer-mutating or cursor-moving keystroke.
    pub sig_help: Option<hover::SigHelp>,
    /// Streaming parser for Normal-mode key chords. Holds whatever
    /// partial state has accumulated between keystrokes (count digits,
    /// pending operator, `g`-prefix flag, text-object selector).
    cmd_parser: command::Parser,
    /// Single-slot yank register populated by `y`/`d`/`c` operations and
    /// consumed by `p`/`P`. No named registers — vim's `"a.."z` are
    /// almost never used in practice and the cost-to-value ratio is poor.
    yank: Option<YankEntry>,
    /// Last `f`/`F`/`t`/`T` argument so `;` and `,` can repeat it.
    last_find: Option<FindMemo>,
    /// Last buffer-mutating command, replayed by `.`. We don't capture
    /// inserted text yet — `cw` then text + Esc replays only the delete.
    last_change: Option<Command>,
    /// In Visual mode, the byte offset where the selection started.
    /// `None` outside Visual mode.
    visual_anchor: Option<usize>,
    /// Which pane currently consumes keystrokes.
    pub focus: Pane,
    /// Legend pane state: which series is highlighted, which are hidden,
    /// the details modal's open / cursor state, and the tag keys that
    /// replace the auto-generated series label. Grouped because every
    /// successful query reshapes the whole bundle (resize `hidden`,
    /// clamp `selected`, reload `label_tags`).
    pub legend: LegendState,
    /// Identifies the query whose results currently sit in `series`.
    /// `None` before the first query completes. Captured at query
    /// dispatch so toggles persist to the right cache keys even if the
    /// user has since edited the buffer.
    pub last_query_context: Option<QueryContext>,
    /// Params pane state: cursor row + the merged system / cli param
    /// dictionaries that drive `mpl::param_rows`. Grouped because
    /// `system` and `cli` are always passed together to `mpl::analyze`,
    /// `mpl::query_hash`, and `mpl::param_rows`, and `selected` is
    /// reclamped after edits to either dict.
    pub params: ParamsState,
    // `cmdline_return_focus` folded into `cmdline.return_focus` (CmdLine).
    // `cmdline_completions` folded into `cmdline.completions` (CmdLine).
    /// `true` after `Ctrl-w` has been seen; the next key is interpreted
    /// as a window/pane command.
    pending_ctrl_w: bool,
    // `system_params` -> `params.system`; `cli_params` -> `params.cli`
    //  (see `params` field above).
    //
    /// Path of the `.mpl` file currently being edited, if any. Set by `:e <path>`
    /// or the CLI argument; cleared when `:enew` (TODO) opens a fresh buffer.
    /// Searchable picker over the org's dashboards. Hidden by default;
    /// `:dash ls` opens it.
    pub dashboards: DashboardPicker,
    /// Last dashboard uid the user picked from `:dash ls`. Captured
    /// so `:open` (without args) can re-fetch the same one.
    pub last_picked_dashboard: Option<String>,
    /// The dashboard currently loaded in memory. Set by
    /// `AppEvent::DashboardOpened`. Step 17b will adapt this into the
    /// internal `Dashboard` model and start rendering its tiles; for
    /// now it backs the `:dashinfo` overlay.
    pub loaded_dashboard: Option<DashboardSummary>,
    /// Toggle for the `:dashinfo` overlay. Closes on `Esc` (handled in
    /// `on_key`) and toggles via the Ex-command.
    pub dashinfo_visible: bool,
    /// Time-range + `:time` picker state. Grouped because the picker
    /// is the only writer to `range` and `range` is the picker's seed
    /// on open — they're always touched together.
    pub time: TimeState,
    /// When `Some`, an overlay shows the focused tile's raw chart
    /// JSON. Set by `:tile json` / `:tile inspect`; any key dismisses
    /// (handled in `on_key`).
    pub tile_inspect_json: Option<String>,
    /// Which mode the current buffer/file represents. `Mpl` is the
    /// long-standing default (a single MPL/MQL buffer is the source of
    /// truth); `Dashboard` means `loaded_dashboard` holds the canonical
    /// state and `:w` writes the dashboard JSON, not the buffer text.
    pub buffer_mode: BufferMode,
    /// Language of the standalone editor buffer when
    /// [`BufferMode::Mpl`] is active. Ignored in
    /// [`BufferMode::Dashboard`] mode — there the focused tile's
    /// language is the source of truth (see [`App::active_lang`]).
    /// Flipped by `:apl` / `:mpl` in standalone mode. Defaults to
    /// [`crate::dashboard::Lang::Mpl`] so existing `.mpl` workflows
    /// behave unchanged.
    pub buffer_lang: crate::dashboard::Lang,
    /// Top-pane view: single-tile (`Solo`) or multi-tile (`Grid`).
    /// Auto-flips to `Grid` when a dashboard with ≥2 charts loads;
    /// `:solo` / `:grid` toggle manually.
    pub view_mode: ViewMode,
    /// Index into `loaded_dashboard.dashboard.charts` of the
    /// currently-selected tile in Grid mode. Wraps within bounds and
    /// resets to 0 when a new dashboard is adopted.
    pub selected_chart_idx: usize,
    /// Vertical scroll offset (in terminal rows) for the dashboard
    /// grid pane. Grid content is laid out at a minimum per-virtual-
    /// row height (see `MIN_GRID_ROW_HEIGHT` in `ui.rs`) so that
    /// large dashboards exceed the viewport and need scrolling. The
    /// renderer clamps this to `[0, max_scroll]` each frame; key
    /// handlers + auto-scroll only set a desired value.
    pub dashboard_scroll: u16,
    /// Active tile editing sub-mode. `Idle` outside of `m`/`s`/`d`/`a`.
    pub tile_submode: TileSubMode,
    /// Count + verb parser for the dashboard pane (vim-style `3y`,
    /// `2x`, `5o` etc.). Only consumed in `TileSubMode::Idle`.
    pub(super) dashboard_cmd: dashboard_cmd::DashboardParser,
    /// Tile-level yank register, populated by `y` / `x`. Survives
    /// navigation, view-mode flips, and dashboard swaps. Distinct
    /// from `App.yank` (editor text register).
    pub tile_yank: Option<Vec<TileSnapshot>>,
    /// One-level dashboard undo. Set before every mutating
    /// dashboard command; `u` swaps it with the current state so a
    /// second `u` redoes — matches vim's single-slot undo toggle.
    pub dashboard_undo: Option<DashboardSnapshot>,
    /// Set whenever a tile mutation touches `loaded_dashboard`.
    /// Cleared on `DashboardSaved` and on `write_file` in dashboard
    /// mode. Surfaced as `[+]` in the status line.
    pub dashboard_dirty: bool,
    /// Armed by `:wq` / `:x` on a server-loaded dashboard: the PUT is
    /// async, so we can't quit synchronously without aborting the
    /// in-flight HTTP request when the tokio runtime drops. Instead
    /// the cmd dispatches the save, sets this flag, and the
    /// `DashboardSaved` event handler completes the quit on success
    /// (or clears the flag on error so the user can retry).
    pub quit_after_save: bool,
    /// Per-tile query results, keyed by chart id (wire `ChartBase.id`).
    /// Populated by `run_tile_queries` after `adopt_dashboard`; read
    /// by the grid renderer to draw live data in each tile.
    pub tile_results: std::collections::BTreeMap<String, TileQueryResult>,
    /// Monotonic counter bumped whenever the slate of in-flight tile
    /// queries becomes irrelevant (dashboard swap, full-dashboard
    /// refresh). Each spawned task captures the epoch at dispatch and
    /// the handler drops events whose epoch doesn't match the current
    /// one — prevents a slow result from dashboard A from resurrecting
    /// or clobbering a tile in dashboard B that happens to share its
    /// chart id (`c1`, `c2`, … are the typical defaults).
    pub tile_query_epoch: u64,
    /// Snapshot of the editor buffer captured the last time
    /// `adopt_dashboard` seeded it from the focused chart. Used by the
    /// background dashboard-refresh path to decide whether re-adopting
    /// the fresh resource would clobber user edits.
    pub last_adopted_seed: Option<String>,
    pub current_file: Option<std::path::PathBuf>,
    /// Snapshot of the buffer the last time it was loaded or written to disk;
    /// used to compute the dirty flag without relying on `tui-textarea` internals.
    pub saved_buffer: String,
    /// Focused tile's viz kind. In Solo / file mode this is the
    /// kind the editor's `// @viz` pragma selects; in Grid mode it
    /// tracks whichever chart the user last zoomed in on (since the
    /// editor + status bar live in solo terms). Kept in sync with
    /// the buffer's pragma by [`App::sync_dashboard_from_buffer`],
    /// which runs after every buffer-mutating or buffer-loading path
    /// via [`App::recompute_diagnostics`].
    pub viz_kind: VizKind,
    /// Focused tile's `// @viz:opts` map (e.g. `n=10` for top-list).
    /// Same lifecycle as [`Self::viz_kind`].
    pub viz_opts: std::collections::BTreeMap<String, String>,
    // `time_range` moved to `time.range` (TimeState); see field above.
    /// Counter incremented on each query start; only matching responses are accepted.
    last_query_id: u64,
    runtime: Handle,
    events_tx: mpsc::Sender<AppEvent>,
    events_rx: mpsc::Receiver<AppEvent>,
    client: Option<AxiomClient>,
    /// `~/.axiom.toml` deployment chosen via `--deployment NAME` on the
    /// command line. Overrides the persistent `active_deployments` field
    /// for this launch. `None` means "use the config file's default".
    /// Consulted only when [`Self::ensure_client`] (and the share-URL
    /// builder in `ex_cmds`) calls `Config::select`; mid-session changes
    /// have no effect because the `AxiomClient` is cached after first use.
    pub deployment_override: Option<String>,
    /// Caps concurrent in-flight per-tile MPL fetches. `run_tile_queries`
    /// spawns one task per MPL chart; on a large dashboard (e.g. 50
    /// tiles) the unthrottled burst would routinely 429 the Axiom
    /// edge. 8 keeps interactive latency low without flooding.
    pub(super) tile_fetch_semaphore: Arc<tokio::sync::Semaphore>,
}

impl App {
    pub fn new(runtime: Handle) -> Self {
        Self::with_cache_and_history(runtime, default_cache(), crate::history::History::load())
    }

    /// Test-only entry point: in-memory cache, in-memory history.
    /// Used by `test_app()` so the suite never touches the user's
    /// real `~/.local/share/mcu/` or `~/.cache/mcu/` directories.
    #[cfg(test)]
    pub fn with_cache(runtime: Handle, cache: Cache) -> Self {
        Self::with_cache_and_history(runtime, cache, crate::history::History::default())
    }

    pub fn with_cache_and_history(
        runtime: Handle,
        cache: Cache,
        history: crate::history::History,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        let cached_count = cache.dataset_count();
        let saved_query = cache.load_query();
        // Restore the saved language sidecar (":apl" / ":mpl" state)
        // so re-opening doesn't silently drop the user back to MPL
        // after they were editing APL last session. Missing sidecar
        // defaults to the language enum's `Default` (MPL).
        let saved_lang = cache.load_query_lang().and_then(|s| match s.as_str() {
            "apl" => Some(crate::dashboard::Lang::Apl),
            "mpl" => Some(crate::dashboard::Lang::Mpl),
            _ => None,
        });
        let editor = match &saved_query {
            Some(text) => editor::editor_with_text(text),
            None => editor::new_editor(),
        };
        let cache = Arc::new(RwLock::new(cache));
        // Annotate the restore message with the language so the
        // user sees at startup which mode the restored buffer is
        // in (status bar shows it too, but the boot message is
        // the first feedback).
        let restored_tag = match saved_lang {
            Some(crate::dashboard::Lang::Apl) => " (APL)",
            _ => "",
        };
        let status = match (cached_count, saved_query.is_some()) {
            (0, false) => "ready".to_string(),
            (0, true) => format!("restored previous query{restored_tag}"),
            (n, false) => format!("loaded {n} dataset(s) from cache"),
            (n, true) => format!("loaded {n} dataset(s); restored previous query{restored_tag}"),
        };
        let initial_text = saved_query
            .clone()
            .unwrap_or_else(|| editor.lines().join("\n"));
        // Seed `viz_kind` / `viz_opts` from the buffer's `// @viz`
        // pragma so the first frame renders the right chart kind
        // before any edit runs `sync_dashboard_from_buffer`.
        // Pragma errors fall through silently — they'll resurface
        // as soon as `sync_dashboard_from_buffer` runs on the
        // first edit.
        let (initial_viz_kind, initial_viz_opts) = match viz::parse_pragma(&initial_text) {
            Ok(Some(spec)) => (spec.kind, spec.opts),
            _ => (VizKind::default(), std::collections::BTreeMap::new()),
        };
        Self {
            mode: Mode::Normal,
            editor,
            cmdline: CmdLine::default(),
            history,
            history_overlay_visible: false,
            params: ParamsState {
                selected: 0,
                system: params::default_system_params(),
                cli: std::collections::BTreeMap::new(),
            },
            current_file: None,
            saved_buffer: initial_text.clone(),
            viz_kind: initial_viz_kind,
            viz_opts: initial_viz_opts,
            time: TimeState::default(),
            dashboards: DashboardPicker::default(),
            last_picked_dashboard: None,
            loaded_dashboard: None,
            dashinfo_visible: false,
            buffer_mode: BufferMode::Mpl,
            buffer_lang: saved_lang.unwrap_or_default(),
            tile_inspect_json: None,
            view_mode: ViewMode::Solo,
            selected_chart_idx: 0,
            dashboard_scroll: 0,
            tile_submode: TileSubMode::Idle,
            dashboard_cmd: dashboard_cmd::DashboardParser::new(),
            tile_yank: None,
            dashboard_undo: None,
            dashboard_dirty: false,
            quit_after_save: false,
            tile_results: std::collections::BTreeMap::new(),
            tile_query_epoch: 0,
            last_adopted_seed: None,
            last_error: None,
            series: demo_series(),
            table_result: None,
            table_selected: 0,
            table_pending_g: false,
            unit: None,
            status,
            should_quit: false,
            busy: false,
            cache,
            completions: CompletionState::default(),
            quickfix: QuickFixPicker::default(),
            diagnostics: Vec::new(),
            last_trace_id: None,
            help: HelpState::default(),
            hover: None,
            sig_help: None,
            cmd_parser: command::Parser::new(),
            yank: None,
            deployment_override: None,
            last_find: None,
            last_change: None,
            visual_anchor: None,
            focus: Pane::Editor,
            legend: LegendState::default(),
            last_query_context: None,
            pending_ctrl_w: false,
            last_query_id: 0,
            tile_fetch_semaphore: Arc::new(tokio::sync::Semaphore::new(TILE_FETCH_CONCURRENCY)),
            runtime,
            events_tx,
            events_rx,
            client: None,
        }
    }

    /// Current editor buffer as a single string. System-param references
    /// like `$__interval` are preserved verbatim — the Axiom MetricsDB
    /// server resolves them from the request's time window.
    pub fn query_text(&self) -> String {
        self.editor.lines().join("\n")
    }

    fn current_chart_id(&self) -> Option<String> {
        // `Chart::Unknown` has no `ChartBase.id`, so a focused Unknown
        // tile has no current id; callers already handle `None`.
        self.loaded_dashboard
            .as_ref()
            .and_then(|r| r.dashboard.charts.get(self.selected_chart_idx))
            .and_then(|c| c.base().map(|b| b.id.clone()))
    }

    /// Reload `legend_label_tags` from the cache for the current
    /// active context, so the picker buffer + render labels reflect
    /// the focused tile (or editor query) instead of the previous
    /// one's selection.
    ///
    /// Wiring: this is called whenever the active context changes
    /// — tile focus moves in Grid view, dashboard adoption, view
    /// mode flips, the focused tile's first data lands, and the
    /// editor finishes a query. The lookup is cheap (two HashMap
    /// hits), and the value silently becomes empty when nothing is
    /// cached for the new context, which clears any stale leftover
    /// from the previous tile.
    fn reload_legend_label_tags(&mut self) {
        let tags = if self.view_mode == ViewMode::Grid
            && let Some(resource) = self.loaded_dashboard.as_ref()
            && let Some(chart) = resource.dashboard.charts.get(self.selected_chart_idx)
            && let crate::dashboard::Query::Mpl(mpl) = crate::dashboard::extract_query(chart)
            && let Ok((ds, m)) = crate::mpl::extract_dataset_metric(&mpl)
        {
            // Tile context: ignore the editor's query-hash store
            // (the tile's hash isn't the editor's) and key purely
            // by `(dataset, metric)`. Empty hash misses the
            // by-hash store; `resolve_legend_tags` then falls
            // through to the per-metric one.
            self.cache.read().resolve_legend_tags("", &ds, &m)
        } else if let Some(ctx) = self.last_query_context.clone() {
            self.cache
                .read()
                .resolve_legend_tags(&ctx.hash, &ctx.dataset, &ctx.metric)
        } else {
            Vec::new()
        };
        self.legend.label_tags = tags;
    }

    /// Series slice driving the legend pane right now: the focused
    /// tile's series when a dashboard is loaded in Grid view,
    /// otherwise the editor's last query result. Matches the source
    /// `chart::draw_legend` already uses for rendering so the `e`
    /// tag picker and friends reflect what the user is looking at.
    pub fn active_legend_series(&self) -> &[Series] {
        if self.view_mode == ViewMode::Grid
            && let Some(resource) = self.loaded_dashboard.as_ref()
            && let Some(chart) = resource.dashboard.charts.get(self.selected_chart_idx)
            && let Some(base) = chart.base()
            && let Some(tr) = self.tile_results.get(&base.id)
        {
            return &tr.series;
        }
        &self.series
    }

    /// `legend_selected` clamped into the active series slice.
    /// Returns `None` when there's nothing selectable.
    fn active_legend_index(&self) -> Option<usize> {
        let n = self.active_legend_series().len();
        if n == 0 {
            None
        } else {
            Some(self.legend.selected.min(n - 1))
        }
    }

    /// Snapshot the entire layout vector and the focused tile's id.
    /// Used by Move/Resize sub-modes so cascade shoves can be
    /// previewed against a stable baseline and `Esc` can revert
    /// every shoved tile in one shot.
    ///
    /// Synthesises a default `LayoutItem` for the focused tile if it
    /// somehow has no layout entry, so the sub-mode always has
    /// something to mutate.
    fn snapshot_full_layout(&mut self) -> Option<(Vec<crate::axiom::LayoutItem>, String)> {
        let id = self.current_chart_id()?;
        let resource = self.loaded_dashboard.as_mut()?;
        if !resource.dashboard.layout.iter().any(|l| l.i == id) {
            resource.dashboard.layout.push(crate::axiom::LayoutItem {
                i: id.clone(),
                x: 0,
                y: Some(0),
                w: 6,
                h: 6,
                extras: Default::default(),
            });
        }
        Some((resource.dashboard.layout.clone(), id))
    }

    /// Restore the entire layout vector. Cheap: a single move of
    /// the `Vec<LayoutItem>` plus a status update.
    fn revert_full_layout(&mut self, original: Vec<crate::axiom::LayoutItem>) {
        if let Some(resource) = self.loaded_dashboard.as_mut() {
            resource.dashboard.layout = original;
        }
        self.tile_submode = TileSubMode::Idle;
        self.status = "reverted".to_string();
    }

    /// Preview a Move sub-mode delta: clone `original_layout`, run
    /// the auto-shove, and replace the dashboard's layout iff the
    /// shove succeeded. On failure the visible layout stays at the
    /// last successful preview (matching how vim leaves the buffer
    /// at the last accepted state after a rejected motion) and the
    /// stored `(dx, dy)` is *not* advanced — the next arrow key
    /// retries from the same baseline.
    fn try_apply_move_preview(
        &mut self,
        original_layout: &[crate::axiom::LayoutItem],
        original_id: &str,
        ndx: i32,
        ndy: i32,
    ) {
        let mut candidate = original_layout.to_vec();
        match crate::app::tile_ops_shove::shove_move(&mut candidate, original_id, ndx, ndy) {
            Ok(outcome) => {
                if let Some(resource) = self.loaded_dashboard.as_mut() {
                    resource.dashboard.layout = candidate;
                }
                self.dashboard_dirty = true;
                self.tile_submode = TileSubMode::Move {
                    original_layout: original_layout.to_vec(),
                    original_id: original_id.to_string(),
                    dx: ndx,
                    dy: ndy,
                };
                // Only report cascade detail when something other than
                // the moved tile shifted, to keep the status quiet
                // for the common single-tile case.
                let extras = outcome.moved.len().saturating_sub(1);
                self.status = match (extras, outcome.new_rows) {
                    (0, 0) => String::new(),
                    (n, 0) => format!("move ok: {n} tile(s) shoved"),
                    (n, r) => format!("move ok: {n} tile(s) shoved, +{r} row(s)"),
                };
            }
            Err(reason) => {
                self.status = format!("move blocked: {reason}");
            }
        }
    }

    /// Resize counterpart to [`Self::try_apply_move_preview`].
    fn try_apply_resize_preview(
        &mut self,
        original_layout: &[crate::axiom::LayoutItem],
        original_id: &str,
        ndw: i32,
        ndh: i32,
    ) {
        let mut candidate = original_layout.to_vec();
        match crate::app::tile_ops_shove::shove_resize(&mut candidate, original_id, ndw, ndh) {
            Ok(outcome) => {
                if let Some(resource) = self.loaded_dashboard.as_mut() {
                    resource.dashboard.layout = candidate;
                }
                self.dashboard_dirty = true;
                self.tile_submode = TileSubMode::Resize {
                    original_layout: original_layout.to_vec(),
                    original_id: original_id.to_string(),
                    dw: ndw,
                    dh: ndh,
                };
                let extras = outcome.moved.len().saturating_sub(1);
                self.status = match (extras, outcome.new_rows) {
                    (0, 0) => String::new(),
                    (n, 0) => format!("resize ok: {n} tile(s) shoved"),
                    (n, r) => format!("resize ok: {n} tile(s) shoved, +{r} row(s)"),
                };
            }
            Err(reason) => {
                self.status = format!("resize blocked: {reason}");
            }
        }
    }

    /// Recompute the params pane's row list for the current buffer +
    /// `cli_params`. Cheap; mirrors the diagnostics-on-every-keystroke
    /// pattern.
    pub fn param_rows(&self) -> Vec<crate::params::ParamRow> {
        crate::mpl::param_rows(&self.query_text(), &self.params.system, &self.params.cli)
    }

    /// Write the current `legend_label_tags` to the cache and flush
    /// to disk. Two keying modes:
    ///
    ///   * **Grid view, dashboard tile focused** — key by the tile's
    ///     `(dataset, metric)` extracted from its MPL. The tile's
    ///     query hash isn't the editor's, so we deliberately skip
    ///     the by-hash store and rely on the per-metric one.
    ///   * **Solo / editor view** — key by `last_query_context`'s
    ///     hash + `(dataset, metric)`, same as before.
    ///
    /// Silent no-op when neither path yields a key.
    fn persist_legend_label_tags(&self) {
        if self.view_mode == ViewMode::Grid
            && let Some(resource) = self.loaded_dashboard.as_ref()
            && let Some(chart) = resource.dashboard.charts.get(self.selected_chart_idx)
            && let crate::dashboard::Query::Mpl(mpl) = crate::dashboard::extract_query(chart)
            && let Ok((ds, m)) = crate::mpl::extract_dataset_metric(&mpl)
        {
            let tags = self.legend.label_tags.clone();
            cache_save_with(&self.cache, |c| c.set_legend_tags_for_metric(&ds, &m, tags));
            return;
        }
        let Some(ctx) = &self.last_query_context else {
            return;
        };
        let (h, d, m, tags) = (
            ctx.hash.clone(),
            ctx.dataset.clone(),
            ctx.metric.clone(),
            self.legend.label_tags.clone(),
        );
        cache_save_with(&self.cache, |c| c.set_legend_tags(&h, &d, &m, tags));
    }

    /// Show the help modal, resetting the scroll offset so the next
    /// open lands at the top instead of wherever the user left it.
    /// Single entry point so the reset can't be forgotten by ad-hoc
    /// callers.
    fn open_help(&mut self) {
        self.help.visible = true;
        self.help.scroll = 0;
    }

    /// `true` when there are unsaved changes.
    ///
    /// * MPL mode: the editor buffer diverges from the last load/write.
    /// * Dashboard mode: the dashboard model itself is dirty
    ///   (`dashboard_dirty`). The editor buffer is a live view onto
    ///   the focused tile and gets rewritten on every navigation, so
    ///   comparing it against `saved_buffer` would flag every nav as
    ///   "unsaved" — the writeback already mirrors real edits into
    ///   `loaded_dashboard` and flips `dashboard_dirty` there.
    pub fn is_dirty(&self) -> bool {
        match self.buffer_mode {
            BufferMode::Mpl => self.query_text() != self.saved_buffer,
            BufferMode::Dashboard => self.dashboard_dirty,
        }
    }

    /// Set both the status line summary and the dismissable error overlay.
    /// Keeps the status line in sync so the bar reads the same as the overlay
    /// header. Truncates very long errors for the status line only.
    pub fn set_error(&mut self, msg: String) {
        let summary: String = msg
            .lines()
            .next()
            .unwrap_or(&msg)
            .chars()
            .take(140)
            .collect();
        self.status = summary;
        self.last_error = Some(msg);
    }

    /// Dismiss the error overlay. Returns `true` when an overlay was visible.
    pub fn dismiss_error(&mut self) -> bool {
        self.last_error.take().is_some()
    }

    /// Write the current editor contents to the on-disk session cache.
    /// Skipped when a `current_file` is open — the user owns that file via
    /// `:w`, and we shouldn't double-shadow it. Failures are logged to stderr
    /// (visible after the alt-screen tears down) but never surfaced as a
    /// user-facing error — persistence is best-effort.
    pub fn persist_query(&self) {
        if self.current_file.is_some() {
            return;
        }
        let text = self.query_text();
        let cache = self.cache.read();
        if let Err(e) = cache.save_query(&text) {
            eprintln!("mcu: query cache save failed: {e}");
        }
        // Persist the buffer language sidecar so the next launch
        // restores `:apl` / `:mpl` state alongside the query text.
        // Best-effort like the text save.
        if let Err(e) = cache.save_query_lang(self.buffer_lang.as_sidecar()) {
            eprintln!("mcu: query lang sidecar save failed: {e}");
        }
    }

    /// Kick off background discovery once at startup if the cache is empty so the
    /// first completion attempt has something to show, and run the persisted
    /// query (if any) so the chart pane is populated on launch.
    pub fn bootstrap(&mut self) {
        self.bootstrap_inner(true);
    }

    /// Same as [`bootstrap`] but suppresses the auto-run of the
    /// restored saved query. Used when `-d <uid>` is going to seed
    /// the editor from a dashboard — running the stale saved query
    /// first would just push wrong results into `self.series`.
    pub fn bootstrap_skip_initial_query(&mut self) {
        self.bootstrap_inner(false);
    }

    fn bootstrap_inner(&mut self, run_initial_query: bool) {
        if !self.params.cli.is_empty() {
            let n = self.params.cli.len();
            let plural = if n == 1 { "param" } else { "params" };
            self.status = format!("{}; {n} CLI {plural}", self.status);
        }
        if self.cache.read().dataset_count() == 0 {
            self.fetch_datasets();
        }
        self.recompute_diagnostics();
        if run_initial_query && !self.query_text().trim().is_empty() {
            self.run_query();
        }
    }

    /// Re-run the MPL engine over the current buffer and update
    /// `self.diagnostics`. Cheap enough (~ms range on our queries) to run on
    /// every buffer-mutating keystroke; debounce if it ever shows up in a
    /// profile.
    ///
    /// Also pushes the buffer back into the focused dashboard tile when
    /// in `BufferMode::Dashboard` — the writeback is guarded by an
    /// equality check so seed operations (which re-run diagnostics) and
    /// pure cursor moves are no-ops on `dashboard_dirty`.
    pub fn recompute_diagnostics(&mut self) {
        let text = self.query_text();
        // Only run the MPL analyzer on MPL buffers. APL buffers
        // would otherwise pick up a spurious "MPL syntax error" on
        // every keystroke; the APL parser lives server-side and
        // surfaces real errors through `tile_results.error` after
        // a `:r`.
        if self.active_lang() == crate::dashboard::Lang::Mpl {
            self.diagnostics = mpl::analyze(&text, &self.params.system);
        } else {
            self.diagnostics.clear();
        }
        self.sync_dashboard_from_buffer(&text);
        self.recompute_sig_help();
        self.sync_buffer_to_focused_tile();
        self.sync_live_unit_from_buffer(&text);
    }

    /// Refresh the status-line signature help from the current cursor.
    /// Cheap (single backwards byte scan + one stdlib lookup); fine to call
    /// on every keystroke and cursor move.
    pub fn recompute_sig_help(&mut self) {
        // APL buffers don't have MPL signature data — suppress so
        // an APL `summarize(...)` doesn't display the unrelated MPL
        // `summarize` signature.
        if self.active_lang() != crate::dashboard::Lang::Mpl {
            self.sig_help = None;
            return;
        }
        let text = self.query_text();
        let cursor = editor_cursor_byte_offset(&self.editor);
        self.sig_help = hover::find_call_context(&text, cursor);
    }
}

#[cfg(test)]
mod tests;
