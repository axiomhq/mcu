use std::sync::mpsc;
use std::sync::{Arc, RwLock};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::runtime::Handle;
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

mod ex_cmds;
mod helpers;
mod tile_layout;
mod types;

// Re-export the items external modules / tests reach into via
// `crate::app::*`. Internal helpers stay private and are pulled in
// through `use` below.
pub use tile_layout::{SpatialDir, build_dashboard_doc_from_buffer};
pub(crate) use tile_layout::{
    add_pick_kinds, pick_next_chart_in_direction, tile_ops,
};
pub use types::*;

use helpers::*;

pub struct App {
    pub mode: Mode,
    pub editor: TextArea<'static>,
    pub series: Vec<Series>,
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
    /// Current cmdline completion popup state. `None` when no Tab has
    /// been pressed since the cmdline opened (or since the last
    /// non-Tab key reset it).
    pub cmdline_completions: CmdlineCompletionState,
    /// Live diagnostics for the current buffer.
    /// Recomputed by [`App::recompute_diagnostics`] on every buffer-mutating key.
    pub diagnostics: Vec<mpl::Diagnostic>,
    /// Trace identifier of the most recently completed query, surfaced on the
    /// right of the status bar so users can correlate against server logs.
    /// `None` before the first run or when the response carried no trace.
    pub last_trace_id: Option<String>,
    /// `true` while the help modal is on screen.
    pub help_visible: bool,
    /// Top row of the help modal that's currently visible. `0` puts
    /// the first line of `docs/keys.md` at the top; increased by
    /// j/Ctrl-d/G key handlers when the help modal is open so the
    /// content (now sourced from a file and longer than a screen) is
    /// scrollable instead of clipped.
    pub help_scroll: u16,
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
    /// Index of the highlighted series in the legend (and the chart — the
    /// selected series is drawn with a brighter marker when the legend is
    /// focused so the user can see what they're about to toggle).
    pub legend_selected: usize,
    /// Per-series visibility flag, parallel to `series`. `true` means
    /// hidden from the chart. Resized on every successful query.
    pub legend_hidden: Vec<bool>,
    /// `true` while a details modal for the selected legend entry is open.
    pub legend_details_visible: bool,
    /// Cursor row inside the details modal (index into
    /// `series[legend_selected].tags`).
    pub details_cursor: usize,
    /// Tag keys, in selection order, that replace the auto-generated
    /// series label in the legend. Empty = use `series.name` as before.
    /// Reloaded from cache on every successful query (two-step fallback:
    /// AST hash, then dataset+metric); user toggles persist back via
    /// both keys so the next run remembers.
    pub legend_label_tags: Vec<String>,
    /// Identifies the query whose results currently sit in `series`.
    /// `None` before the first query completes. Captured at query
    /// dispatch so toggles persist to the right cache keys even if the
    /// user has since edited the buffer.
    pub last_query_context: Option<QueryContext>,
    /// Cursor row in the params pane. Index into the row list produced
    /// by [`crate::mpl::param_rows`] for the current buffer + provided
    /// values; clamped on every recompute so deletions don't dangle.
    pub params_selected: usize,
    /// When `Some`, the next time the command line is dismissed (either
    /// via `Enter` or `Esc`) focus is restored to this pane. Set by
    /// [`prefill_command`] so that `a`/`e` in the Params pane drop into
    /// `:p` but return the user to Params after submit. `None` for
    /// commands entered the normal way (`:` from Normal mode).
    cmdline_return_focus: Option<Pane>,
    /// `true` after `Ctrl-w` has been seen; the next key is interpreted
    /// as a window/pane command.
    pending_ctrl_w: bool,
    /// Host-supplied system parameters (e.g. `$__interval`). Substituted into
    /// the query text before validation and before sending to the API.
    pub system_params: Vec<params::SystemParam>,
    /// User-declared `param $name: type;` values supplied via `-p NAME=VALUE`
    /// on the command line. Sent verbatim to the server as `queryParams`;
    /// the server typechecks against the buffer's declared params.
    pub cli_params: std::collections::BTreeMap<String, String>,
    /// Path of the `.mpl` file currently being edited, if any. Set by `:e <path>`
    /// or the CLI argument; cleared when `:enew` (TODO) opens a fresh buffer.
    /// Searchable picker over the org's dashboards. Hidden by default;
    /// `:dashboards` (or `:db`) opens it.
    pub dashboards: DashboardPicker,
    /// Last dashboard uid the user picked from `:dashboards`. Captured
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
    /// `:time` overlay state. `Some(_)` while visible; the variant
    /// distinguishes the preset list from the custom date picker.
    pub time_picker: Option<TimePickerState>,
    /// When `Some`, an overlay shows the focused tile's raw chart
    /// JSON. Set by `:tile json` / `:tile inspect`; any key dismisses
    /// (handled in `on_key`).
    pub tile_inspect_json: Option<String>,
    /// Which mode the current buffer/file represents. `Mpl` is the
    /// long-standing default (a single MPL/MQL buffer is the source of
    /// truth); `Dashboard` means `loaded_dashboard` holds the canonical
    /// state and `:w` writes the dashboard JSON, not the buffer text.
    pub buffer_mode: BufferMode,
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
    /// Set whenever a tile mutation touches `loaded_dashboard`.
    /// Cleared on `DashboardSaved` and on `write_file` in dashboard
    /// mode. Surfaced as `[+]` in the status line.
    pub dashboard_dirty: bool,
    /// Per-tile query results, keyed by chart id (wire `ChartBase.id`).
    /// Populated by `run_tile_queries` after `adopt_dashboard`; read
    /// by the grid renderer to draw live data in each tile.
    pub tile_results: std::collections::BTreeMap<String, TileQueryResult>,
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
    /// Active query time range, shared by every tile in the loaded
    /// dashboard and by the editor's `:r` runs. Seeded from the
    /// dashboard's `timeWindowStart` / `End` (or the legacy
    /// `now-1h` / `now` defaults on file-mode startup) and mutated
    /// in place by `:time` and the picker.
    pub time_range: TimeRange,
    /// Counter incremented on each query start; only matching responses are accepted.
    last_query_id: u64,
    runtime: Handle,
    events_tx: mpsc::Sender<AppEvent>,
    events_rx: mpsc::Receiver<AppEvent>,
    client: Option<AxiomClient>,
}

impl App {
    pub fn new(runtime: Handle) -> Self {
        Self::with_cache(runtime, default_cache())
    }

    pub fn with_cache(runtime: Handle, cache: Cache) -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        let cached_count = cache.dataset_count();
        let saved_query = cache.load_query();
        let editor = match &saved_query {
            Some(text) => editor::editor_with_text(text),
            None => editor::new_editor(),
        };
        let cache = Arc::new(RwLock::new(cache));
        let status = match (cached_count, saved_query.is_some()) {
            (0, false) => "ready".to_string(),
            (0, true) => "restored previous query".to_string(),
            (n, false) => format!("loaded {n} dataset(s) from cache"),
            (n, true) => format!("loaded {n} dataset(s); restored previous query"),
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
            cmdline_completions: CmdlineCompletionState::default(),
            system_params: params::default_system_params(),
            cli_params: std::collections::BTreeMap::new(),
            current_file: None,
            saved_buffer: initial_text.clone(),
            viz_kind: initial_viz_kind,
            viz_opts: initial_viz_opts,
            time_range: TimeRange::default(),
            dashboards: DashboardPicker::default(),
            last_picked_dashboard: None,
            loaded_dashboard: None,
            dashinfo_visible: false,
            time_picker: None,
            buffer_mode: BufferMode::Mpl,
            tile_inspect_json: None,
            view_mode: ViewMode::Solo,
            selected_chart_idx: 0,
            dashboard_scroll: 0,
            tile_submode: TileSubMode::Idle,
            dashboard_dirty: false,
            tile_results: std::collections::BTreeMap::new(),
            last_adopted_seed: None,
            last_error: None,
            series: demo_series(),
            status,
            should_quit: false,
            busy: false,
            cache,
            completions: CompletionState::default(),
            quickfix: QuickFixPicker::default(),
            diagnostics: Vec::new(),
            last_trace_id: None,
            help_visible: false,
            help_scroll: 0,
            hover: None,
            sig_help: None,
            cmd_parser: command::Parser::new(),
            yank: None,
            last_find: None,
            last_change: None,
            visual_anchor: None,
            focus: Pane::Editor,
            legend_selected: 0,
            legend_hidden: Vec::new(),
            legend_details_visible: false,
            details_cursor: 0,
            legend_label_tags: Vec::new(),
            last_query_context: None,
            params_selected: 0,
            cmdline_return_focus: None,
            pending_ctrl_w: false,
            last_query_id: 0,
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

    pub fn on_key(&mut self, key: KeyEvent) {
        // Dashboard picker takes precedence over every other key handler
        // when it's visible. Owns its own keymap (arrows + Enter +
        // printable for the filter); only Esc closes it.
        if self.dashboards.visible {
            self.handle_dashboards_picker_key(key);
            return;
        }

        // `:time` quick-select overlay. Owns its own modal keymap so
        // motion keys don't bleed through to the editor/dashboard.
        if self.time_picker.is_some() {
            self.handle_time_picker_key(key);
            return;
        }

        // Help modal: owns its own scroll-friendly keymap. j/k/Ctrl-d/u
        // scroll, g/G jump to top/bottom, any other key dismisses.
        // Handled here so the modal works from every pane and mode,
        // not just the few that had ad-hoc guards before.
        if self.help_visible {
            self.handle_help_key(key);
            return;
        }

        // `:dashinfo` overlay: any key dismisses. Sits above the picker
        // logically but below it in priority — they're mutually
        // exclusive in practice (picker hides itself on Enter).
        if self.dashinfo_visible {
            self.dashinfo_visible = false;
            return;
        }

        // `:tile json` inspect overlay: any key dismisses.
        if self.tile_inspect_json.is_some() {
            self.tile_inspect_json = None;
            return;
        }

        // `Ctrl-w` is the window-prefix in any mode; the next key picks
        // the target pane. Handled before mode dispatch so it works from
        // Insert, Visual, and the legend itself.
        if self.pending_ctrl_w {
            self.pending_ctrl_w = false;
            self.handle_ctrl_w_followup(key);
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('w') {
            self.pending_ctrl_w = true;
            return;
        }

        // Legend / params / dashboard own their own bindings when
        // focused; the modal editor's mode is irrelevant on those
        // surfaces.
        if self.focus == Pane::Legend {
            self.handle_legend_key(key);
            return;
        }
        if self.focus == Pane::Params {
            self.handle_params_key(key);
            return;
        }
        if self.focus == Pane::Dashboard {
            self.handle_dashboard_key(key);
            return;
        }

        match self.mode {
            Mode::Insert => self.handle_insert_key(key),
            Mode::Normal => self.handle_normal_key(key),
            Mode::Command => self.handle_command_key(key),
            Mode::Visual | Mode::VisualLine => self.handle_visual_key(key),
        }
    }

    /// Keymap for the dashboard grid pane. The dispatch order is:
    ///
    ///   1. Active sub-mode (Move/Resize/ConfirmDelete/AddPick) owns
    ///      every key while engaged — Esc cancels back to Idle.
    ///   2. `Idle` accepts the navigation + entry-point shortcuts
    ///      (m, s, d, a, v, R, Enter, hjkl/arrows, Tab).
    fn handle_dashboard_key(&mut self, key: KeyEvent) {
        // Sub-mode takes precedence.
        match self.tile_submode.clone() {
            TileSubMode::Move { original } => return self.handle_move_key(key, original),
            TileSubMode::Resize { original } => return self.handle_resize_key(key, original),
            TileSubMode::ConfirmDelete => return self.handle_confirm_delete_key(key),
            TileSubMode::AddPick { cursor } => return self.handle_add_pick_key(key, cursor),
            TileSubMode::Idle => {}
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.focus = Pane::Editor;
            }
            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                self.move_dashboard_selection_spatial(SpatialDir::Left);
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                self.move_dashboard_selection_spatial(SpatialDir::Right);
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.move_dashboard_selection_spatial(SpatialDir::Up);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.move_dashboard_selection_spatial(SpatialDir::Down);
            }
            (KeyCode::Tab, _) => {
                self.move_dashboard_selection(1);
            }
            (KeyCode::BackTab, _) => {
                self.move_dashboard_selection(-1);
            }
            (KeyCode::Enter, _) | (KeyCode::Char('v'), KeyModifiers::NONE) => {
                self.zoom_selected_chart();
            }
            // `:` drops into the ex-command line while preserving the
            // current pane so Enter/Esc returns to the grid. Without
            // this arm the colon was silently swallowed by the final
            // `_ => {}` and the user had to Esc back to the editor to
            // run any `:` command from grid view.
            (KeyCode::Char(':'), KeyModifiers::NONE)
            | (KeyCode::Char(':'), KeyModifiers::SHIFT) => self.prefill_command(""),
            // `?` opens the help modal. Centralised dismissal in
            // `on_key` means we just trigger here — scrolling and
            // closing happen above pane dispatch.
            (KeyCode::Char('?'), _) => self.open_help(),
            (KeyCode::Char('m'), KeyModifiers::NONE) => self.enter_tile_move(),
            (KeyCode::Char('s'), KeyModifiers::NONE) => self.enter_tile_resize(),
            (KeyCode::Char('d'), KeyModifiers::NONE) => self.enter_tile_confirm_delete(),
            (KeyCode::Char('a'), KeyModifiers::NONE) => self.enter_tile_add_pick(),
            (KeyCode::Char('R'), KeyModifiers::SHIFT)
            | (KeyCode::Char('R'), KeyModifiers::NONE) => {
                self.run_focused_tile_query();
            }
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                self.run_tile_queries();
                self.status = format!("refetching {} tile(s)…", self.tile_results.len().max(1));
            }
            // Vertical scroll. `j`/`k` are owned by spatial nav above
            // so we use vim's scroll-by-screen bindings here. The
            // renderer clamps to valid range each frame.
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_add(10);
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_sub(10);
            }
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_add(20);
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_sub(20);
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                self.dashboard_scroll = 0;
            }
            (KeyCode::Char('G'), KeyModifiers::NONE)
            | (KeyCode::Char('G'), KeyModifiers::SHIFT) => {
                self.dashboard_scroll = u16::MAX; // renderer clamps to max
            }
            _ => {}
        }
    }

    fn current_chart_id(&self) -> Option<String> {
        self.loaded_dashboard
            .as_ref()
            .and_then(|r| r.dashboard.charts.get(self.selected_chart_idx))
            .map(|c| c.base().id.clone())
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
            && let Some(chart) = resource
                .dashboard
                .charts
                .get(self.selected_chart_idx)
            && let crate::dashboard::Query::Mpl(mpl) =
                crate::dashboard::extract_query(chart)
            && let Ok((ds, m)) = crate::mpl::extract_dataset_metric(&mpl)
        {
            // Tile context: ignore the editor's query-hash store
            // (the tile's hash isn't the editor's) and key purely
            // by `(dataset, metric)`. Empty hash misses the
            // by-hash store; `resolve_legend_tags` then falls
            // through to the per-metric one.
            self.cache.read().unwrap().resolve_legend_tags("", &ds, &m)
        } else if let Some(ctx) = self.last_query_context.clone() {
            self.cache
                .read()
                .unwrap()
                .resolve_legend_tags(&ctx.hash, &ctx.dataset, &ctx.metric)
        } else {
            Vec::new()
        };
        self.legend_label_tags = tags;
    }

    /// Series slice driving the legend pane right now: the focused
    /// tile's series when a dashboard is loaded in Grid view,
    /// otherwise the editor's last query result. Matches the source
    /// `chart::draw_legend` already uses for rendering so the `e`
    /// tag picker and friends reflect what the user is looking at.
    pub fn active_legend_series(&self) -> &[Series] {
        if self.view_mode == ViewMode::Grid
            && let Some(resource) = self.loaded_dashboard.as_ref()
            && let Some(chart) = resource
                .dashboard
                .charts
                .get(self.selected_chart_idx)
            && let Some(tr) = self.tile_results.get(&chart.base().id)
        {
            return &tr.series;
        }
        &self.series
    }

    /// `legend_selected` clamped into the active series slice.
    /// Returns `None` when there's nothing selectable.
    fn active_legend_index(&self) -> Option<usize> {
        let n = self.active_legend_series().len();
        if n == 0 { None } else { Some(self.legend_selected.min(n - 1)) }
    }

    /// Snapshot the selected tile's layout entry, synthesising a
    /// default one if missing so sub-modes always have something to
    /// revert to.
    fn snapshot_selected_layout(&mut self) -> Option<crate::axiom::LayoutItem> {
        let id = self.current_chart_id()?;
        let resource = self.loaded_dashboard.as_mut()?;
        if let Some(li) = resource.dashboard.layout.iter().find(|l| l.i == id) {
            return Some(li.clone());
        }
        // Synthesize and append so subsequent edits have something to
        // mutate.
        let li = crate::axiom::LayoutItem {
            i: id,
            x: 0,
            y: Some(0),
            w: 6,
            h: 6,
            extras: Default::default(),
        };
        resource.dashboard.layout.push(li.clone());
        Some(li)
    }

    /// `R` shortcut in the dashboard pane: refetch just the focused
    /// tile's MPL query. APL / no-query tiles surface a status hint.
    pub fn run_focused_tile_query(&mut self) {
        let Some(id) = self.current_chart_id() else {
            self.status = "no tile selected".to_string();
            return;
        };
        let mpl = self
            .loaded_dashboard
            .as_ref()
            .and_then(|r| r.dashboard.charts.iter().find(|c| c.base().id == id))
            .and_then(|c| match crate::dashboard::extract_query(c) {
                crate::dashboard::Query::Mpl(s) => Some(s),
                _ => None,
            });
        let Some(mpl) = mpl else {
            self.status = format!("tile {id}: no MPL query to rerun");
            return;
        };
        let dataset = match mpl::extract_dataset_metric(&mpl) {
            Ok((d, _)) => d,
            Err(e) => {
                self.tile_results.insert(
                    id.clone(),
                    TileQueryResult {
                        busy: false,
                        series: vec![],
                        error: Some(format!("MPL: {e}")),
                        trace_id: None,
                    },
                );
                return;
            }
        };
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => {
                self.set_error(format!("tile fetch: {e}"));
                return;
            }
        };
        // Mark the tile busy in-place so the chrome flips to the
        // spinner pip.
        let entry = self.tile_results.entry(id.clone()).or_default();
        entry.busy = true;
        entry.error = None;
        let cache = self.cache.clone();
        let params = self.cli_params.clone();
        let (start, end) = self.active_time_range();
        let tx = self.events_tx.clone();
        let chart_id = id.clone();
        self.runtime.spawn(async move {
            let result =
                run_query_task(&cache, &client, &dataset, &mpl, &start, &end, &params).await;
            let _ = tx.send(AppEvent::TileQueryFinished { chart_id, result });
        });
        self.status = format!("refetching tile {id}…");
    }

    fn enter_tile_move(&mut self) {
        let Some(original) = self.snapshot_selected_layout() else {
            self.status = "no tile selected".to_string();
            return;
        };
        self.tile_submode = TileSubMode::Move { original };
        self.status = "MOVE: arrows = nudge, Enter = commit, Esc = cancel".to_string();
    }

    fn enter_tile_resize(&mut self) {
        let Some(original) = self.snapshot_selected_layout() else {
            self.status = "no tile selected".to_string();
            return;
        };
        self.tile_submode = TileSubMode::Resize { original };
        self.status =
            "RESIZE: Right/Down grow, Left/Up shrink, Enter = commit, Esc = cancel".to_string();
    }

    fn enter_tile_confirm_delete(&mut self) {
        if self.current_chart_id().is_none() {
            self.status = "no tile selected".to_string();
            return;
        }
        self.tile_submode = TileSubMode::ConfirmDelete;
        self.status = "DELETE: y to confirm, any other key cancels".to_string();
    }

    fn enter_tile_add_pick(&mut self) {
        if self.loaded_dashboard.is_none() {
            self.status = "no dashboard loaded".to_string();
            return;
        }
        self.tile_submode = TileSubMode::AddPick { cursor: 0 };
        self.status = "ADD: arrows pick kind, Enter inserts, Esc cancels".to_string();
    }

    fn handle_move_key(&mut self, key: KeyEvent, original: crate::axiom::LayoutItem) {
        let Some(id) = self.current_chart_id() else {
            self.tile_submode = TileSubMode::Idle;
            return;
        };
        let mut translate = |dx: i32, dy: i32| {
            let Some(resource) = self.loaded_dashboard.as_mut() else {
                return;
            };
            match tile_ops::translate(&mut resource.dashboard.layout, &id, dx, dy) {
                Ok(()) => {
                    self.dashboard_dirty = true;
                }
                Err(reason) => {
                    self.status = format!("move blocked: {reason}");
                }
            }
        };
        match (key.code, key.modifiers) {
            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => translate(-1, 0),
            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => translate(1, 0),
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => translate(0, -1),
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => translate(0, 1),
            (KeyCode::Enter, _) => {
                self.tile_submode = TileSubMode::Idle;
                self.status = "move committed".to_string();
            }
            (KeyCode::Esc, _) => self.revert_layout(original),
            _ => {}
        }
    }

    fn handle_resize_key(&mut self, key: KeyEvent, original: crate::axiom::LayoutItem) {
        let Some(id) = self.current_chart_id() else {
            self.tile_submode = TileSubMode::Idle;
            return;
        };
        let mut resize = |dw: i32, dh: i32| {
            let Some(resource) = self.loaded_dashboard.as_mut() else {
                return;
            };
            match tile_ops::resize(&mut resource.dashboard.layout, &id, dw, dh) {
                Ok(()) => {
                    self.dashboard_dirty = true;
                }
                Err(reason) => {
                    self.status = format!("resize blocked: {reason}");
                }
            }
        };
        match (key.code, key.modifiers) {
            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => resize(1, 0),
            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => resize(-1, 0),
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => resize(0, 1),
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => resize(0, -1),
            (KeyCode::Enter, _) => {
                self.tile_submode = TileSubMode::Idle;
                self.status = "resize committed".to_string();
            }
            (KeyCode::Esc, _) => self.revert_layout(original),
            _ => {}
        }
    }

    fn revert_layout(&mut self, original: crate::axiom::LayoutItem) {
        if let Some(resource) = self.loaded_dashboard.as_mut()
            && let Some(li) = resource
                .dashboard
                .layout
                .iter_mut()
                .find(|l| l.i == original.i)
        {
            *li = original;
        }
        self.tile_submode = TileSubMode::Idle;
        self.status = "reverted".to_string();
    }

    fn handle_confirm_delete_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let Some(id) = self.current_chart_id() else {
                    self.tile_submode = TileSubMode::Idle;
                    return;
                };
                if let Some(resource) = self.loaded_dashboard.as_mut()
                    && let Ok(()) = tile_ops::delete(
                        &mut resource.dashboard.charts,
                        &mut resource.dashboard.layout,
                        &id,
                    )
                {
                    self.dashboard_dirty = true;
                    let n = resource.dashboard.charts.len();
                    if self.selected_chart_idx >= n {
                        self.selected_chart_idx = n.saturating_sub(1);
                    }
                    self.status = format!("deleted tile {id}");
                }
                self.tile_submode = TileSubMode::Idle;
            }
            _ => {
                self.tile_submode = TileSubMode::Idle;
                self.status = "delete cancelled".to_string();
            }
        }
    }

    fn handle_add_pick_key(&mut self, key: KeyEvent, cursor: usize) {
        // The picker shows every implemented `VizKind`.
        let kinds = add_pick_kinds();
        let n = kinds.len();
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.tile_submode = TileSubMode::Idle;
                self.status = "add cancelled".to_string();
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                let next = (cursor + n - 1) % n;
                self.tile_submode = TileSubMode::AddPick { cursor: next };
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                let next = (cursor + 1) % n;
                self.tile_submode = TileSubMode::AddPick { cursor: next };
            }
            (KeyCode::Enter, _) => {
                let kind = kinds[cursor];
                if let Some(resource) = self.loaded_dashboard.as_mut() {
                    let id = tile_ops::insert_tile(
                        &mut resource.dashboard.charts,
                        &mut resource.dashboard.layout,
                        kind,
                        "new tile",
                    );
                    self.dashboard_dirty = true;
                    self.selected_chart_idx = resource.dashboard.charts.len() - 1;
                    self.status = format!("added {} tile {id}", kind.as_str());
                }
                self.tile_submode = TileSubMode::Idle;
            }
            _ => {}
        }
    }

    fn handle_ctrl_w_followup(&mut self, key: KeyEvent) {
        // Spatial layout (matches the rendered grid):
        //   +---------+---+
        //   |  graph  | L |   (top:    Legend)
        //   +---------+---+
        //   |  editor | P |   (bottom: Params)
        //   +---------+---+
        // In Grid view the graph slot is the Dashboard pane, so the
        // top-left neighbour of Legend is Dashboard (not Editor).
        // `w` cycles Editor → Legend → Params → (Dashboard if Grid)
        // → Editor; directional keys use the layout to pick the
        // spatial neighbour and fall back to the source pane when
        // there's no neighbour in that direction.
        let cycle = || -> Pane {
            match self.focus {
                Pane::Editor => Pane::Legend,
                Pane::Legend => Pane::Params,
                Pane::Params => {
                    if self.view_mode == ViewMode::Grid {
                        Pane::Dashboard
                    } else {
                        Pane::Editor
                    }
                }
                Pane::Dashboard => Pane::Editor,
            }
        };
        let next = match (key.code, key.modifiers) {
            (KeyCode::Char('w'), _) => cycle(),
            // `Ctrl-w d` jumps straight to the dashboard pane. No-op if
            // no dashboard is loaded.
            (KeyCode::Char('d'), _) => {
                if self.loaded_dashboard.is_some() && self.view_mode == ViewMode::Grid {
                    Pane::Dashboard
                } else {
                    self.status = ":Ctrl-w d: no grid view".to_string();
                    return;
                }
            }
            (KeyCode::Char('h'), KeyModifiers::NONE) | (KeyCode::Left, _) => match self.focus {
                // In Grid view, Legend's left neighbour is the
                // Dashboard tile area (the graph slot); in Solo
                // there's no top-left pane, so fall back to Editor.
                Pane::Legend => {
                    if self.view_mode == ViewMode::Grid && self.loaded_dashboard.is_some() {
                        Pane::Dashboard
                    } else {
                        Pane::Editor
                    }
                }
                Pane::Params => Pane::Editor,
                Pane::Editor => Pane::Editor,
                // Dashboard is already leftmost — no-op.
                Pane::Dashboard => Pane::Dashboard,
            },
            (KeyCode::Char('l'), KeyModifiers::NONE) | (KeyCode::Right, _) => match self.focus {
                Pane::Editor => Pane::Params,
                Pane::Legend => Pane::Legend,
                Pane::Params => Pane::Params,
                // Dashboard's right neighbour is the Legend column.
                Pane::Dashboard => Pane::Legend,
            },
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => match self.focus {
                Pane::Legend => Pane::Params,
                Pane::Editor => Pane::Editor,
                Pane::Params => Pane::Params,
                Pane::Dashboard => Pane::Editor,
            },
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => match self.focus {
                Pane::Params => Pane::Legend,
                Pane::Editor => {
                    if self.view_mode == ViewMode::Grid {
                        Pane::Dashboard
                    } else {
                        Pane::Legend
                    }
                }
                Pane::Legend => Pane::Legend,
                Pane::Dashboard => Pane::Dashboard,
            },
            (KeyCode::Esc, _) => return,
            _ => return,
        };
        self.set_focus(next);
    }

    fn set_focus(&mut self, pane: Pane) {
        if pane == Pane::Legend && self.series.is_empty() {
            self.status = "no series to focus".to_string();
            return;
        }
        self.focus = pane;
        if pane != Pane::Legend {
            self.legend_details_visible = false;
        }
        if pane == Pane::Params {
            // Clamp on entry so a stale index from a previous buffer
            // shape doesn't render off the end.
            let n = self.param_rows().len();
            if n == 0 {
                self.params_selected = 0;
            } else if self.params_selected >= n {
                self.params_selected = n - 1;
            }
        }
    }

    /// Recompute the params pane's row list for the current buffer +
    /// `cli_params`. Cheap; mirrors the diagnostics-on-every-keystroke
    /// pattern.
    pub fn param_rows(&self) -> Vec<crate::params::ParamRow> {
        crate::mpl::param_rows(&self.query_text(), &self.system_params, &self.cli_params)
    }

    fn handle_params_key(&mut self, key: KeyEvent) {
        let rows = self.param_rows();
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('h'), KeyModifiers::NONE) | (KeyCode::Left, _) => {
                self.set_focus(Pane::Editor);
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                self.move_params_selection(1, &rows);
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                self.move_params_selection(-1, &rows);
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                self.params_selected = 0;
            }
            (KeyCode::Char('G'), _) if !rows.is_empty() => {
                self.params_selected = rows.len() - 1;
            }
            // `a` / `i` — add new param. Drop into command mode with a
            // bare `p ` prefix so the user types `NAME=VALUE`.
            (KeyCode::Char('a'), KeyModifiers::NONE) | (KeyCode::Char('i'), KeyModifiers::NONE) => {
                self.prefill_command("p ");
            }
            // `e` / `Enter` — edit selected row. Pre-fills with the
            // current value so the user can tweak in place.
            (KeyCode::Char('e'), KeyModifiers::NONE) | (KeyCode::Enter, _) => {
                if let Some(row) = rows.get(self.params_selected) {
                    let v = row.value.as_deref().unwrap_or("");
                    self.prefill_command(&format!("p {}={}", row.name, v));
                }
            }
            // `x` / `dd` — clear the selected value.
            (KeyCode::Char('x'), KeyModifiers::NONE) => {
                if let Some(row) = rows.get(self.params_selected).cloned() {
                    if self.cli_params.remove(&row.name).is_some() {
                        self.status = format!("cleared ${}", row.name);
                    } else {
                        self.status = format!("${} not set", row.name);
                    }
                }
            }
            (KeyCode::Char('?'), _) => self.open_help(),
            (KeyCode::Char('q'), KeyModifiers::NONE) => self.cmd_quit(false),
            _ => {}
        }
    }

    fn move_params_selection(&mut self, delta: i32, rows: &[crate::params::ParamRow]) {
        if rows.is_empty() {
            self.params_selected = 0;
            return;
        }
        let n = rows.len() as i32;
        let cur = self.params_selected as i32;
        let next = (cur + delta).rem_euclid(n);
        self.params_selected = next as usize;
    }

    /// Drop into Command mode with `text` already on the line and the
    /// cursor at the end. Shared by the params pane's add/edit bindings.
    /// Remembers the current pane so the cmdline can return focus to it
    /// once the command is submitted or cancelled.
    fn prefill_command(&mut self, text: &str) {
        self.cmdline_return_focus = Some(self.focus);
        self.cmdline.reset();
        self.cmdline.buf = text.to_string();
        self.cmdline.cursor = self.cmdline.buf.chars().count();
        self.mode = Mode::Command;
        self.status = String::new();
        // The cmdline lives at the bottom of the screen and consumes
        // keys through `handle_command_key` while `mode == Command`;
        // pane focus is irrelevant during that period. We drop to
        // Editor so any pane-specific key handlers stop firing.
        self.focus = Pane::Editor;
    }

    /// Restore pane focus after the command line closes. Used by both
    /// the Enter and Esc paths so cancelling a prefilled `:p` also
    /// brings the user back to the pane they came from.
    fn restore_cmdline_focus(&mut self) {
        if let Some(pane) = self.cmdline_return_focus.take() {
            // `set_focus` enforces the same invariants as any other
            // focus change (e.g. won't focus Legend with no series).
            self.set_focus(pane);
        }
    }

    fn handle_legend_key(&mut self, key: KeyEvent) {
        // Details modal owns its own bindings while open.
        if self.legend_details_visible {
            self.handle_legend_details_key(key);
            return;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('h'), KeyModifiers::NONE) | (KeyCode::Left, _) => {
                self.set_focus(Pane::Editor)
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                self.move_legend_selection(1);
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                self.move_legend_selection(-1);
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                // `gg` to top — simple two-key here; pending_g lives in the
                // parser but the legend has its own little state.
                self.legend_selected = 0;
            }
            (KeyCode::Char('G'), _) if !self.active_legend_series().is_empty() => {
                self.legend_selected = self.active_legend_series().len() - 1;
            }
            (KeyCode::Char(' '), KeyModifiers::NONE) | (KeyCode::Enter, _) => {
                self.legend_toggle_current();
            }
            (KeyCode::Char('a'), KeyModifiers::NONE) => {
                self.legend_toggle_all();
            }
            (KeyCode::Char('e'), KeyModifiers::NONE)
                if !self.active_legend_series().is_empty() =>
            {
                self.legend_details_visible = true;
                self.details_cursor = 0;
            }
            (KeyCode::Char('?'), _) => self.open_help(),
            (KeyCode::Char('q'), KeyModifiers::NONE) => self.cmd_quit(false),
            _ => {}
        }
    }

    fn move_legend_selection(&mut self, delta: i32) {
        let n = self.active_legend_series().len();
        if n == 0 {
            return;
        }
        let n = n as i32;
        let cur = self.legend_selected as i32;
        let next = (cur + delta).rem_euclid(n);
        self.legend_selected = next as usize;
    }

    fn legend_toggle_current(&mut self) {
        if let Some(flag) = self.legend_hidden.get_mut(self.legend_selected) {
            *flag = !*flag;
        }
    }

    fn handle_legend_details_key(&mut self, key: KeyEvent) {
        let tag_count = self
            .active_legend_index()
            .and_then(|i| self.active_legend_series().get(i))
            .map(|s| s.tags.len())
            .unwrap_or(0);
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _)
            | (KeyCode::Char('e'), KeyModifiers::NONE)
            | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                self.legend_details_visible = false;
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) if tag_count > 0 => {
                self.details_cursor = (self.details_cursor + 1) % tag_count;
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) if tag_count > 0 => {
                self.details_cursor = if self.details_cursor == 0 {
                    tag_count - 1
                } else {
                    self.details_cursor - 1
                };
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => self.details_cursor = 0,
            (KeyCode::Char('G'), _) if tag_count > 0 => {
                self.details_cursor = tag_count - 1;
            }
            (KeyCode::Char(' '), KeyModifiers::NONE) | (KeyCode::Enter, _) => {
                self.toggle_label_tag_at_cursor();
            }
            _ => {}
        }
    }

    fn toggle_label_tag_at_cursor(&mut self) {
        // Clone the key first so we don't hold a borrow across the
        // mutation of `legend_label_tags`.
        let key = {
            let Some(idx) = self.active_legend_index() else {
                return;
            };
            let series_slice = self.active_legend_series();
            let Some(series) = series_slice.get(idx) else {
                return;
            };
            let Some((k, _)) = series.tags.get(self.details_cursor) else {
                return;
            };
            k.clone()
        };
        if let Some(pos) = self.legend_label_tags.iter().position(|kk| kk == &key) {
            self.legend_label_tags.remove(pos);
        } else {
            self.legend_label_tags.push(key);
        }
        self.persist_legend_label_tags();
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
            && let Some(chart) = resource
                .dashboard
                .charts
                .get(self.selected_chart_idx)
            && let crate::dashboard::Query::Mpl(mpl) =
                crate::dashboard::extract_query(chart)
            && let Ok((ds, m)) = crate::mpl::extract_dataset_metric(&mpl)
        {
            let mut cache = self.cache.write().unwrap();
            cache.set_legend_tags_for_metric(&ds, &m, self.legend_label_tags.clone());
            if let Err(e) = cache.save() {
                eprintln!("metrics-tui: cache save failed: {e}");
            }
            return;
        }
        let Some(ctx) = &self.last_query_context else {
            return;
        };
        let mut cache = self.cache.write().unwrap();
        cache.set_legend_tags(
            &ctx.hash,
            &ctx.dataset,
            &ctx.metric,
            self.legend_label_tags.clone(),
        );
        if let Err(e) = cache.save() {
            eprintln!("metrics-tui: cache save failed: {e}");
        }
    }

    /// Smart toggle: if any series is currently hidden, show all; otherwise
    /// hide all. Vim's `:hidden` toggle convention.
    fn legend_toggle_all(&mut self) {
        if self.legend_hidden.is_empty() {
            return;
        }
        let any_hidden = self.legend_hidden.iter().any(|h| *h);
        let target = !any_hidden;
        for h in &mut self.legend_hidden {
            *h = target;
        }
    }

    fn handle_insert_key(&mut self, key: KeyEvent) {
        // Completion popup intercepts a small set of keys.
        if self.completions.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.completions.hide();
                    return;
                }
                (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::Enter, KeyModifiers::NONE) => {
                    self.accept_completion();
                    return;
                }
                (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                    self.move_completion_selection(-1);
                    return;
                }
                (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                    self.move_completion_selection(1);
                    return;
                }
                _ => {}
            }
        }

        // Trigger keys: Tab and Ctrl-Space.
        if matches!(
            (key.code, key.modifiers),
            (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::Char(' '), KeyModifiers::CONTROL),
        ) {
            self.open_completions();
            return;
        }

        if key.code == KeyCode::Esc {
            self.mode = Mode::Normal;
            return;
        }

        let consumed = self.editor.input(key);
        if consumed {
            if self.completions.visible {
                self.refresh_completions();
            }
            self.recompute_diagnostics();
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {

        // Hover popup: any key other than `K` dismisses it (so the user can
        // also re-trigger by pressing `K` over a different ident).
        if self.hover.is_some() && !matches!((key.code, key.modifiers), (KeyCode::Char('K'), _)) {
            self.hover = None;
        }

        // The quick-fix picker takes over a small set of keys while visible.
        if self.quickfix.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                    self.quickfix.hide();
                    return;
                }
                (KeyCode::Enter, _) => {
                    self.accept_quickfix();
                    return;
                }
                (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                    self.move_quickfix_selection(-1);
                    return;
                }
                (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                    self.move_quickfix_selection(1);
                    return;
                }
                _ => return,
            }
        }

        match self.cmd_parser.feed(key) {
            Step::Pending | Step::Cancel => {}
            Step::Emit(cmd) => self.run_command(cmd),
        }
        // Any keystroke may have moved the cursor or edited the buffer;
        // refresh the signature-help line so the status bar follows.
        self.recompute_sig_help();
    }

    /// Flat dispatcher for [`Command`]s produced by the Normal-mode parser.
    ///
    /// Adding a new Normal-mode feature should be a single arm here plus
    /// (sometimes) a helper in `motion.rs` once that exists. The parser is
    /// already wide enough to recognise `dw`, `ciw`, `da"`, `gu`, etc. —
    /// arms for those just need to be filled in.
    fn run_command(&mut self, cmd: Command) {
        // Record buffer-mutating commands so `.` can replay them. Done
        // *before* dispatch so a recursive `.` doesn't overwrite itself.
        if Self::is_mutating(&cmd) {
            self.last_change = Some(cmd.clone());
        }
        match cmd {
            Command::Move { motion, count } => self.apply_motion(motion, count),
            Command::Apply { op, target, count } => self.apply_operator(op, target, count),
            Command::EnterInsert(at) => self.enter_insert_at(at),
            Command::EnterCommand => self.enter_command_mode(),
            Command::RunQuery | Command::RefreshQuery => self.run_query(),
            Command::Undo => {
                if !self.editor.undo() {
                    self.status = "nothing to undo".to_string();
                }
            }
            Command::Redo => {
                if !self.editor.redo() {
                    self.status = "nothing to redo".to_string();
                }
            }
            Command::Quickfix => self.open_quickfix(),
            Command::Hover => {
                let text = self.query_text();
                let cursor = editor_cursor_byte_offset(&self.editor);
                match hover::resolve_function_at(&text, cursor) {
                    Some(info) => self.hover = Some(info),
                    None => self.status = "no docs for symbol under cursor".to_string(),
                }
            }
            Command::Help => self.open_help(),
            Command::Quit => self.cmd_quit(false),
            Command::FetchDatasets => self.fetch_datasets(),
            Command::FetchMetrics => self.fetch_metrics_for_current_query(),
            Command::DismissError => {
                // Esc in Editor Normal mode: dismiss the error
                // overlay if there is one; otherwise, when we
                // arrived in Solo by zooming a dashboard tile, the
                // same key returns to the grid — mirroring the
                // "back out" intuition vim users have for Esc.
                if self.dismiss_error() {
                    self.status = "error dismissed".to_string();
                } else if self.view_mode == ViewMode::Solo
                    && self.loaded_dashboard.is_some()
                {
                    self.cmd_grid();
                }
            }
            Command::DeleteCharUnder { count } => {
                for _ in 0..count {
                    self.editor.delete_next_char();
                }
            }
            Command::Paste { after, count } => self.paste(after, count),
            Command::RepeatFind { reverse, count } => self.repeat_find(reverse, count),
            Command::RepeatLastChange => self.repeat_last_change(),
            Command::EnterVisual { linewise } => self.enter_visual(linewise),
        }
    }

    /// Classify which commands count as a "change" for `.` replay. Pure
    /// cursor moves and discovery commands don't qualify.
    fn is_mutating(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::Apply { .. }
                | Command::Paste { .. }
                | Command::DeleteCharUnder { .. }
                | Command::EnterInsert(_)
        )
    }

    fn repeat_find(&mut self, reverse: bool, count: usize) {
        let Some(memo) = self.last_find else {
            self.status = "no previous f/t to repeat".to_string();
            return;
        };
        let forward = if reverse { !memo.forward } else { memo.forward };
        let motion = Motion::FindChar {
            ch: memo.ch,
            forward,
            till: memo.till,
        };
        self.apply_motion(motion, count.max(1));
    }

    fn repeat_last_change(&mut self) {
        let Some(cmd) = self.last_change.clone() else {
            self.status = "no change to repeat".to_string();
            return;
        };
        // Don't re-store `.` itself as the last change.
        self.run_command(cmd);
    }

    fn enter_visual(&mut self, linewise: bool) {
        let cursor = editor_cursor_byte_offset(&self.editor);
        self.visual_anchor = Some(cursor);
        self.mode = if linewise {
            Mode::VisualLine
        } else {
            Mode::Visual
        };
    }

    /// Row range covered by the active Visual selection, for the UI to
    /// paint. `None` when not in Visual mode. Bool is `linewise`.
    pub fn visual_row_range(&self) -> Option<(usize, usize, bool)> {
        let range = self.visual_range()?;
        let buf = self.query_text();
        let (start_row, _) = byte_offset_to_row_col(&buf, range.start);
        let last = range.end.saturating_sub(1).min(buf.len());
        let (end_row, _) = byte_offset_to_row_col(&buf, last);
        Some((start_row, end_row, range.linewise))
    }

    /// Resolve the current Visual selection to a byte range, rounding to
    /// whole lines if [`Mode::VisualLine`].
    fn visual_range(&self) -> Option<Range> {
        let anchor = self.visual_anchor?;
        let cursor = editor_cursor_byte_offset(&self.editor);
        let (mut start, mut end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        // Visual selection is inclusive of the byte under the cursor.
        let buf = self.query_text();
        if end < buf.len() {
            end = motion::next_char_at(&buf, end);
        }
        let linewise = self.mode == Mode::VisualLine;
        if linewise {
            // Expand to full lines.
            let new_start = buf[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
            let new_end = buf[end.min(buf.len())..]
                .find('\n')
                .map(|p| end + p + 1)
                .unwrap_or(buf.len());
            start = new_start;
            end = new_end;
        }
        Some(Range {
            start,
            end,
            linewise,
        })
    }

    fn exit_visual(&mut self) {
        self.mode = Mode::Normal;
        self.visual_anchor = None;
    }

    /// Visual-mode key handler. Motion keys go through the same parser
    /// (we only consume `Command::Move` emissions); operator keys collapse
    /// the current selection into a range and apply it.
    fn handle_visual_key(&mut self, key: KeyEvent) {
        // Direct overrides.
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.exit_visual();
                return;
            }
            (KeyCode::Char('v'), KeyModifiers::NONE) => {
                self.exit_visual();
                return;
            }
            (KeyCode::Char('V'), _) => {
                self.mode = Mode::VisualLine;
                return;
            }
            (KeyCode::Char(op), _) if matches!(op, 'd' | 'c' | 'y' | 'x' | '>' | '<') => {
                let operator = match op {
                    'd' | 'x' => Operator::Delete,
                    'c' => Operator::Change,
                    'y' => Operator::Yank,
                    '>' => Operator::IndentRight,
                    '<' => Operator::IndentLeft,
                    _ => unreachable!(),
                };
                self.apply_visual(operator);
                return;
            }
            _ => {}
        }
        // Otherwise: feed the parser but only honour pure-motion emissions.
        // Anything else (operators, find-char, etc.) is dropped — the user
        // can always Esc and re-key in Normal mode.
        if let Step::Emit(Command::Move { motion, count }) = self.cmd_parser.feed(key) {
            self.apply_motion(motion, count);
        }
        self.recompute_sig_help();
    }

    fn apply_visual(&mut self, op: Operator) {
        let Some(range) = self.visual_range() else {
            self.exit_visual();
            return;
        };
        let buf = self.query_text();
        match op {
            Operator::Delete => self.delete_range(&buf, range),
            Operator::Yank => self.yank_range(&buf, range),
            Operator::Change => {
                self.delete_range(&buf, range);
                self.mode = Mode::Insert;
                self.visual_anchor = None;
                return;
            }
            Operator::IndentRight => self.indent_range(&buf, range, true),
            Operator::IndentLeft => self.indent_range(&buf, range, false),
        }
        self.exit_visual();
    }

    /// Translate a [`Motion`] into a `tui-textarea` cursor move and apply
    /// it `count` times. For motions that need byte-offset arithmetic
    /// (`FirstNonBlank`, `FindChar`) we compute the target directly.
    fn apply_motion(&mut self, motion: Motion, count: usize) {
        match motion {
            Motion::FirstNonBlank => {
                let buf = self.query_text();
                let cursor = editor_cursor_byte_offset(&self.editor);
                let target = motion::first_non_blank(&buf, cursor);
                let (row, col) = byte_offset_to_row_col(&buf, target);
                self.editor
                    .move_cursor(CursorMove::Jump(row as u16, col as u16));
                return;
            }
            Motion::FindChar { ch, forward, till } => {
                let buf = self.query_text();
                let mut pos = editor_cursor_byte_offset(&self.editor);
                for _ in 0..count.max(1) {
                    let Some(next) = (if forward {
                        motion::find_char_forward(&buf, pos, ch)
                    } else {
                        motion::find_char_back(&buf, pos, ch)
                    }) else {
                        return;
                    };
                    pos = next;
                }
                let target = if till {
                    if forward {
                        motion::prev_char_at(&buf, pos)
                    } else {
                        motion::next_char_at(&buf, pos)
                    }
                } else {
                    pos
                };
                self.last_find = Some(FindMemo { ch, forward, till });
                let (row, col) = byte_offset_to_row_col(&buf, target);
                self.editor
                    .move_cursor(CursorMove::Jump(row as u16, col as u16));
                return;
            }
            _ => {}
        }
        let cm = match motion {
            Motion::Left => CursorMove::Back,
            Motion::Right => CursorMove::Forward,
            Motion::Up => CursorMove::Up,
            Motion::Down => CursorMove::Down,
            Motion::WordForward => CursorMove::WordForward,
            Motion::WordBack => CursorMove::WordBack,
            Motion::WordEnd => CursorMove::WordEnd,
            Motion::LineStart => CursorMove::Head,
            Motion::LineEnd => CursorMove::End,
            Motion::FileStart => CursorMove::Top,
            Motion::FileEnd => CursorMove::Bottom,
            Motion::FirstNonBlank | Motion::FindChar { .. } | Motion::CurrentLine => return,
        };
        for _ in 0..count {
            self.editor.move_cursor(cm);
        }
    }

    /// Resolve a [`Target`] to a byte range and apply `op` to it.
    fn apply_operator(&mut self, op: Operator, target: Target, count: usize) {
        let buf = self.query_text();
        let cursor = editor_cursor_byte_offset(&self.editor);
        let Some(range) = self.resolve_target(&buf, cursor, target, count, op) else {
            return;
        };
        match op {
            Operator::Delete => self.delete_range(&buf, range),
            Operator::Yank => self.yank_range(&buf, range),
            Operator::Change => {
                self.delete_range(&buf, range);
                self.mode = Mode::Insert;
            }
            Operator::IndentRight => self.indent_range(&buf, range, true),
            Operator::IndentLeft => self.indent_range(&buf, range, false),
        }
    }

    fn resolve_target(
        &self,
        buf: &str,
        cursor: usize,
        target: Target,
        count: usize,
        op: Operator,
    ) -> Option<Range> {
        match target {
            Target::Motion(m) => {
                motion::resolve_motion(buf, cursor, m, count, op == Operator::Change)
            }
            Target::Object(o) => motion::resolve_object(buf, cursor, o),
        }
    }

    fn enter_insert_at(&mut self, at: InsertAt) {
        match at {
            InsertAt::AtCursor => {}
            InsertAt::AfterCursor => self.editor.move_cursor(CursorMove::Forward),
            InsertAt::LineStart => self.editor.move_cursor(CursorMove::Head),
            InsertAt::LineEnd => self.editor.move_cursor(CursorMove::End),
            InsertAt::OpenBelow => {
                self.editor.move_cursor(CursorMove::End);
                self.editor.insert_str("\n");
            }
            InsertAt::OpenAbove => {
                self.editor.move_cursor(CursorMove::Head);
                self.editor.insert_str("\n");
                // `insert_str` left the cursor on the line below the new
                // blank line; step back up.
                self.editor.move_cursor(CursorMove::Up);
            }
        }
        self.mode = Mode::Insert;
    }

    /// Delete `range` from the buffer, populating the yank register with
    /// the deleted text so `p`/`P` can put it back (vim convention).
    fn delete_range(&mut self, buf: &str, range: Range) {
        if range.is_empty() {
            return;
        }
        self.yank = Some(YankEntry {
            text: range.slice(buf).to_string(),
            linewise: range.linewise,
        });
        let (row, col) = byte_offset_to_row_col(buf, range.start);
        self.editor
            .move_cursor(CursorMove::Jump(row as u16, col as u16));
        let char_count = range.slice(buf).chars().count();
        self.editor.delete_str(char_count);
    }

    fn yank_range(&mut self, buf: &str, range: Range) {
        if range.is_empty() {
            return;
        }
        self.yank = Some(YankEntry {
            text: range.slice(buf).to_string(),
            linewise: range.linewise,
        });
    }

    fn paste(&mut self, after: bool, count: usize) {
        let Some(entry) = self.yank.clone() else {
            self.status = "nothing to paste".to_string();
            return;
        };
        let body: String = std::iter::repeat_n(entry.text.as_str(), count.max(1)).collect();
        if entry.linewise {
            let trimmed = body.trim_end_matches('\n');
            let new_lines = trimmed.matches('\n').count() + 1;
            if after {
                self.editor.move_cursor(CursorMove::End);
                self.editor.insert_str("\n");
                self.editor.insert_str(trimmed);
            } else {
                self.editor.move_cursor(CursorMove::Head);
                self.editor.insert_str(trimmed);
                self.editor.insert_str("\n");
                // After both insertions the cursor sits at the start of
                // the original line, which is now `new_lines` rows below
                // the pasted block. Step back up so the cursor lands on
                // the first pasted line, matching vim.
                for _ in 0..new_lines {
                    self.editor.move_cursor(CursorMove::Up);
                }
            }
            self.editor.move_cursor(CursorMove::Head);
        } else {
            if after {
                self.editor.move_cursor(CursorMove::Forward);
            }
            self.editor.insert_str(&body);
        }
    }

    /// Indent (or dedent) every line that the byte range touches.
    /// `right == true` adds [`INDENT`] at the line start; otherwise removes
    /// up to that many leading spaces (or one tab).
    fn indent_range(&mut self, buf: &str, range: Range, right: bool) {
        const INDENT: &str = "    ";
        let (first_row, _) = byte_offset_to_row_col(buf, range.start);
        let end_for_row = if range.end == range.start {
            range.end
        } else {
            range.end - 1
        };
        let (last_row, _) = byte_offset_to_row_col(buf, end_for_row);
        for row in first_row..=last_row {
            self.editor.move_cursor(CursorMove::Jump(row as u16, 0));
            if right {
                self.editor.insert_str(INDENT);
            } else {
                let lines = self.editor.lines();
                let Some(line) = lines.get(row) else { continue };
                let mut to_remove = 0usize;
                for c in line.chars().take(INDENT.len()) {
                    if c == '\t' {
                        to_remove = 1;
                        break;
                    } else if c == ' ' {
                        to_remove += 1;
                    } else {
                        break;
                    }
                }
                for _ in 0..to_remove {
                    self.editor.delete_next_char();
                }
            }
        }
    }

    fn fetch_datasets(&mut self) {
        let Some((client, tx, cache)) =
            self.fetch_prepare(Some("fetching datasets…".to_string()))
        else {
            return;
        };
        self.runtime.spawn(async move {
            let result = client.list_datasets().await;
            if let Ok(datasets) = &result {
                let mut c = cache.write().unwrap();
                c.replace_datasets(datasets.clone());
                if let Err(e) = c.save() {
                    eprintln!("metrics-tui: cache save failed: {e}");
                }
            }
            let _ = tx.send(AppEvent::DatasetsFetched(result));
        });
    }

    fn fetch_metrics_for_current_query(&mut self) {
        let mpl = self.query_text();
        let dataset = match mpl::extract_dataset_metric(&mpl).map(|p| p.0) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("MPL error: {e}");
                return;
            }
        };
        let Some((client, tx, cache)) =
            self.fetch_prepare(Some(format!("fetching metrics for `{dataset}`…")))
        else {
            return;
        };
        let (start, end) = rfc3339_now_window(DISCOVERY_WINDOW_HOURS);
        self.runtime.spawn(async move {
            let route = match resolve_route(&cache, &client, &dataset).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(AppEvent::MetricsFetched {
                        dataset,
                        result: Err(e),
                    });
                    return;
                }
            };
            let result = client
                .list_metrics(&route.url, &dataset, &start, &end)
                .await;
            if let Ok(metrics) = &result {
                let mut c = cache.write().unwrap();
                c.replace_metrics(&dataset, metrics.clone());
                if let Err(e) = c.save() {
                    eprintln!("metrics-tui: cache save failed: {e}");
                }
            }
            let _ = tx.send(AppEvent::MetricsFetched { dataset, result });
        });
    }

    /// Kick off a background fetch of tags for `(dataset, metric)`. Fire-and-
    /// forget: does not flip `self.busy` (so multiple background fetches can
    /// coexist with a foreground query) and emits no "fetching…" status to
    /// avoid clobbering the user's view. Skipped when the cache already has
    /// tags for this pair, or when client configuration can't be resolved.
    pub fn fetch_tags(&mut self, dataset: String, metric: String) {
        if self.cache.read().unwrap().has_tags(&dataset, &metric) {
            return;
        }
        let Some((client, tx, cache)) = self.fetch_prepare(None) else {
            return;
        };
        let (start, end) = rfc3339_now_window(DISCOVERY_WINDOW_HOURS);
        self.runtime.spawn(async move {
            let route = match resolve_route(&cache, &client, &dataset).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(AppEvent::TagsFetched {
                        dataset,
                        metric,
                        result: Err(e),
                    });
                    return;
                }
            };
            let result = client
                .list_metric_tags(&route.url, &dataset, &metric, &start, &end)
                .await;
            if let Ok(tags) = &result {
                let mut c = cache.write().unwrap();
                c.replace_tags(&dataset, &metric, tags.clone());
                if let Err(e) = c.save() {
                    eprintln!("metrics-tui: cache save failed: {e}");
                }
            }
            let _ = tx.send(AppEvent::TagsFetched {
                dataset,
                metric,
                result,
            });
        });
    }

    /// Kick off a background fetch of observed values for a single tag of a
    /// `(dataset, metric)`. Skipped when values are already cached or when
    /// another fetch is already busy. Silent on errors — status line only.
    pub fn fetch_tag_values(&mut self, dataset: String, metric: String, tag: String) {
        if self
            .cache
            .read()
            .unwrap()
            .has_tag_values(&dataset, &metric, &tag)
        {
            return;
        }
        let Some((client, tx, cache)) = self.fetch_prepare(None) else {
            return;
        };
        let (start, end) = rfc3339_now_window(DISCOVERY_WINDOW_HOURS);
        self.runtime.spawn(async move {
            let route = match resolve_route(&cache, &client, &dataset).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(AppEvent::TagValuesFetched {
                        dataset,
                        metric,
                        tag,
                        result: Err(e),
                    });
                    return;
                }
            };
            let result = client
                .list_metric_tag_values(&route.url, &dataset, &metric, &tag, &start, &end)
                .await;
            if let Ok(values) = &result {
                let mut c = cache.write().unwrap();
                c.replace_tag_values(&dataset, &metric, &tag, values.clone());
                if let Err(e) = c.save() {
                    eprintln!("metrics-tui: cache save failed: {e}");
                }
            }
            let _ = tx.send(AppEvent::TagValuesFetched {
                dataset,
                metric,
                tag,
                result,
            });
        });
    }

    /// Scan the (already-resolved) query for tag references — identifiers
    /// immediately followed by a comparison operator inside a `where` /
    /// `filter` clause — and fire a background values fetch for each. Skips
    /// pairs that are already cached. Best-effort; failures stay in status.
    fn prefetch_tag_values_from_query(&mut self, mpl: &str) {
        let (dataset, metric) = match mpl::extract_dataset_metric(mpl) {
            Ok(d) => d,
            Err(_) => return,
        };
        if dataset.is_empty() || metric.is_empty() {
            return;
        }
        for tag in referenced_tags(mpl) {
            self.fetch_tag_values(dataset.clone(), metric.clone(), tag);
        }
    }

    fn ensure_client(&mut self) -> anyhow::Result<&AxiomClient> {
        if self.client.is_none() {
            let cfg = Config::load()?;
            let (_name, dep) = cfg.active()?;
            self.client = Some(AxiomClient::new(dep)?);
        }
        Ok(self.client.as_ref().unwrap())
    }

    /// Sync prologue shared by every `runtime.spawn`'d fetch. Builds
    /// the `(client, tx, cache)` triple suitable to `move` into an
    /// async block.
    ///
    /// `status`:
    /// - `Some(msg)` — foreground: the busy gate is enforced
    ///   (returns `None` after setting an "already busy" status),
    ///   `self.busy` is flipped to `true`, and the status line is
    ///   set to `msg`. Config errors raise the error overlay.
    /// - `None` — background: no busy gate, no status change, no
    ///   error reporting on missing config (silent).
    ///
    /// Returns `None` when the caller should bail out; the status
    /// or error overlay has already been written in that case.
    fn fetch_prepare(
        &mut self,
        status: Option<String>,
    ) -> Option<(AxiomClient, mpsc::Sender<AppEvent>, Arc<RwLock<Cache>>)> {
        let foreground = status.is_some();
        if foreground && self.busy {
            self.status = "already busy".to_string();
            return None;
        }
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => {
                if foreground {
                    self.set_error(format!("config error: {e}"));
                }
                return None;
            }
        };
        if let Some(msg) = status {
            self.busy = true;
            self.status = msg;
        }
        Some((client, self.events_tx.clone(), self.cache.clone()))
    }

    /// Drain background events and apply them to app state.
    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.events_rx.try_recv() {
            self.handle_event(ev);
        }
    }

    fn run_query(&mut self) {
        if self.busy {
            self.status = "already busy".to_string();
            return;
        }
        if self.query_text().trim().is_empty() {
            self.status = "empty query".to_string();
            return;
        }
        // The MetricsDB server resolves `$__interval` and friends from the
        // request's time window, so we send the buffer verbatim.
        let mpl = self.query_text();
        let (dataset, metric) = match mpl::extract_dataset_metric(&mpl) {
            Ok(dm) => dm,
            Err(e) => {
                self.status = format!("MPL error: {e}");
                return;
            }
        };
        // Snapshot the query's identity now so toggles after the result
        // arrives persist under stable keys even if the user has since
        // edited the buffer.
        self.last_query_context = Some(QueryContext {
            hash: mpl::query_hash(&mpl, &self.system_params),
            dataset: dataset.clone(),
            metric,
        });
        // Honour the live diagnostic stream: if there are any errors in the
        // buffer, refuse to send. Recompute first so we always check against
        // the latest buffer state, not whatever was cached.
        self.recompute_diagnostics();
        if let Some(first_err) = self.diagnostics.iter().find(|d| d.severity.is_error()) {
            self.status = first_err.header();
            return;
        }
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => {
                self.status = format!("config error: {e}");
                return;
            }
        };

        // Fire off background prefetches for any tags referenced in this
        // query, so the next `where`-clause completion has values ready.
        // Must happen *before* we set `busy = true` to avoid the prefetcher
        // tripping any future busy-aware guards.
        self.prefetch_tag_values_from_query(&mpl);

        self.last_query_id = self.last_query_id.wrapping_add(1);
        let id = self.last_query_id;
        self.busy = true;
        self.status = "running query…".to_string();
        // Treat "the user just ran a query" as a natural checkpoint to persist.
        self.persist_query();
        let tx = self.events_tx.clone();
        let cache = self.cache.clone();
        let params = self.cli_params.clone();
        let (start, end) = self.active_time_range();
        self.runtime.spawn(async move {
            let result =
                run_query_task(&cache, &client, &dataset, &mpl, &start, &end, &params).await;
            let _ = tx.send(AppEvent::QueryFinished { id, result });
        });
    }

    fn enter_command_mode(&mut self) {
        self.cmdline.reset();
        self.mode = Mode::Command;
        self.status = String::new();
    }

    /// Show the help modal, resetting the scroll offset so the next
    /// open lands at the top instead of wherever the user left it.
    /// Single entry point so the reset can't be forgotten by ad-hoc
    /// callers.
    fn open_help(&mut self) {
        self.help_visible = true;
        self.help_scroll = 0;
    }

    /// Modal keymap for the help overlay. j/k/Up/Down/Ctrl-d/u scroll;
    /// g/G jump to top/bottom; any other key dismisses (including
    /// Esc, q, and `?` itself — the modal behaves like a peek).
    fn handle_help_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.help_scroll = self.help_scroll.saturating_add(1);
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                self.help_scroll = self.help_scroll.saturating_add(10);
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.help_scroll = self.help_scroll.saturating_sub(10);
            }
            (KeyCode::PageDown, _) | (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.help_scroll = self.help_scroll.saturating_add(20);
            }
            (KeyCode::PageUp, _) | (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.help_scroll = self.help_scroll.saturating_sub(20);
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                self.help_scroll = 0;
            }
            (KeyCode::Char('G'), _) => {
                self.help_scroll = u16::MAX;
            }
            _ => {
                self.help_visible = false;
            }
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) {
        // Tab / Shift-Tab drive the completion popup; handled before
        // anything else so they never reach the printable-char path
        // below. Every other key resets the popup so successive
        // insert + tab cycles always start from a fresh candidate set.
        match (key.code, key.modifiers) {
            (KeyCode::Tab, _) => {
                self.handle_cmdline_tab(false);
                return;
            }
            (KeyCode::BackTab, _) => {
                self.handle_cmdline_tab(true);
                return;
            }
            _ => {
                // Hide the popup the moment the user does anything
                // other than navigation/accept keys. Up/Down navigate
                // the popup; Enter accepts; Esc/Ctrl-c hide it
                // explicitly via their own arms below.
                if !matches!(
                    (key.code, key.modifiers),
                    (KeyCode::Up, _) | (KeyCode::Down, _) | (KeyCode::Enter, _) | (KeyCode::Esc, _)
                ) && !matches!(
                    (key.code, key.modifiers),
                    (KeyCode::Char('c'), KeyModifiers::CONTROL)
                ) {
                    self.cmdline_completions.hide();
                }
            }
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.cmdline.reset();
                self.cmdline_completions.hide();
                self.mode = Mode::Normal;
                self.restore_cmdline_focus();
            }
            (KeyCode::Up, _) if self.cmdline_completions.visible => {
                self.move_cmdline_completion(-1);
            }
            (KeyCode::Down, _) if self.cmdline_completions.visible => {
                self.move_cmdline_completion(1);
            }
            (KeyCode::Enter, _) => {
                // Enter accepts the highlighted completion if the
                // popup is up; otherwise it executes the cmdline.
                if self.cmdline_completions.visible {
                    self.accept_cmdline_completion();
                    return;
                }
                let cmd = std::mem::take(&mut self.cmdline.buf);
                self.cmdline.cursor = 0;
                self.mode = Mode::Normal;
                self.execute_command(cmd.trim());
                self.restore_cmdline_focus();
            }
            (KeyCode::Backspace, _) => {
                if self.cmdline.buf.is_empty() {
                    // Empty cmdline + Backspace cancels, like vim.
                    self.mode = Mode::Normal;
                } else {
                    self.cmdline.backspace();
                }
            }
            (KeyCode::Delete, _) => self.cmdline.delete_forward(),
            (KeyCode::Left, _) => self.cmdline.move_left(),
            (KeyCode::Right, _) => self.cmdline.move_right(),
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.cmdline.move_home();
            }
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.cmdline.move_end();
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                // Clear from cursor to start — standard readline behaviour.
                let to = self.cmdline.byte_cursor();
                self.cmdline.buf.drain(..to);
                self.cmdline.cursor = 0;
            }
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                // Clear from cursor to end.
                let from = self.cmdline.byte_cursor();
                self.cmdline.buf.truncate(from);
            }
            (KeyCode::Char(c), m) if m == KeyModifiers::NONE || m == KeyModifiers::SHIFT => {
                self.cmdline.insert_char(c);
            }
            _ => {}
        }
    }

    /// Drive the cmdline completion popup on Tab / Shift-Tab. First
    /// Tab from a hidden state: compute candidates, splice in the
    /// longest common prefix, and — if there's still more than one
    /// candidate — show the popup with the first item selected.
    /// Subsequent Tabs cycle (Shift-Tab cycles backward) and splice
    /// the highlighted candidate over the current token in real time.
    pub fn handle_cmdline_tab(&mut self, backward: bool) {
        if !self.cmdline_completions.visible {
            // Fresh Tab: recompute the candidate set against the
            // current buffer + cursor.
            let ctx = crate::cmdline_complete::Context {
                dashboards: &self.dashboards.items,
            };
            let req = match crate::cmdline_complete::completions_for(
                &self.cmdline.buf,
                self.cmdline.cursor,
                &ctx,
            ) {
                Some(r) if !r.items.is_empty() => r,
                _ => return,
            };
            // Splice the longest common prefix immediately so single-
            // candidate paths are zero-friction.
            let prefix = req.common_prefix();
            self.splice_cmdline_token(req.range, &prefix);
            if req.items.len() == 1 {
                // Exact match: also append a trailing space so the
                // user can type the next arg without an extra
                // keystroke.
                self.cmdline.buf.push(' ');
                self.cmdline.cursor = self.cmdline.buf.chars().count();
                return;
            }
            // Multiple candidates: show the popup. Recompute the
            // splice range against the just-updated buffer so future
            // accepts overwrite the token we just typed in.
            let new_token_start = req.range.0;
            let new_token_end = new_token_start + prefix.len();
            self.cmdline_completions.items = req.items;
            self.cmdline_completions.selected = 0;
            self.cmdline_completions.replace_range = (new_token_start, new_token_end);
            self.cmdline_completions.visible = true;
            return;
        }
        // Popup already visible: cycle.
        let delta = if backward { -1 } else { 1 };
        self.move_cmdline_completion(delta);
    }

    fn move_cmdline_completion(&mut self, delta: isize) {
        let n = self.cmdline_completions.items.len();
        if n == 0 {
            return;
        }
        let i = self.cmdline_completions.selected as isize + delta;
        let wrapped = ((i % n as isize) + n as isize) % n as isize;
        self.cmdline_completions.selected = wrapped as usize;
        // Splice the new selection into the buffer so the user sees
        // each candidate as they cycle (vim wildmenu style).
        let item = self.cmdline_completions.items[self.cmdline_completions.selected].clone();
        let range = self.cmdline_completions.replace_range;
        self.splice_cmdline_token(range, &item);
        // Re-anchor the range so the next cycle replaces the just-
        // spliced text instead of an older slice.
        self.cmdline_completions.replace_range = (range.0, range.0 + item.len());
    }

    fn accept_cmdline_completion(&mut self) {
        // The current selection is already in the buffer (from the
        // last cycle); just hide the popup. Append a trailing space
        // to match the single-candidate path's affordance.
        if !self.cmdline.buf.ends_with(' ') {
            self.cmdline.buf.push(' ');
            self.cmdline.cursor = self.cmdline.buf.chars().count();
        }
        self.cmdline_completions.hide();
    }

    /// Replace `buf[range.0..range.1]` with `text` and reposition the
    /// char cursor at the end of the inserted text.
    fn splice_cmdline_token(&mut self, range: (usize, usize), text: &str) {
        let (start, end) = range;
        if start > self.cmdline.buf.len() || end > self.cmdline.buf.len() {
            return;
        }
        self.cmdline.buf.replace_range(start..end, text);
        let new_byte = start + text.len();
        // Convert byte position back to char count for `CmdLine.cursor`.
        self.cmdline.cursor = self.cmdline.buf[..new_byte].chars().count();
    }

    /// Active query time range, in the order the Axiom API wants it
    /// (`start`, `end`). Sourced from `self.time_range`, which is
    /// seeded from the loaded dashboard's `timeWindowStart`/`End`
    /// (or the legacy `now-1h`/`now` defaults) and mutated in place
    /// by `:time`. Both editor (`run_query`) and per-tile fetches
    /// (`run_tile_queries`, `run_focused_tile_query`) read this so
    /// the whole dashboard shares one consistent window.
    ///
    /// The returned strings go through [`normalize_time_expr`] so the
    /// `qr-` prefix Axiom's web UI stores in dashboards (e.g.
    /// `qr-now-7d`) is stripped before hitting the `_mpl` endpoint
    /// — that endpoint only understands the bare relative form
    /// (`now-7d`) and 400s otherwise.
    pub fn active_time_range(&self) -> (String, String) {
        (
            normalize_time_expr(&self.time_range.start),
            normalize_time_expr(&self.time_range.end),
        )
    }

    /// Common path for every time-range mutation: write the in-memory
    /// model, mirror onto the wire copy so `:dash save` persists, mark
    /// the dashboard dirty, status-line the change, and kick a refetch
    /// so the user sees the new window immediately.
    fn set_time_range(&mut self, start: String, end: String) {
        self.time_range = TimeRange {
            start: start.clone(),
            end: end.clone(),
        };
        if let Some(resource) = self.loaded_dashboard.as_mut() {
            resource.dashboard.time_window_start = Some(start.clone());
            resource.dashboard.time_window_end = Some(end.clone());
            self.dashboard_dirty = true;
        }
        self.status = format!("time: {start} → {end}");
        // Refetch so the dashboard reflects the new window without the
        // user having to remember `:r` (Solo) or `Ctrl-R` (Grid).
        if self.view_mode == ViewMode::Grid && self.loaded_dashboard.is_some() {
            self.run_tile_queries();
        } else if !self.query_text().trim().is_empty() {
            self.run_query();
        }
    }

    /// Modal keymap for the `:time` overlay. Dispatches by sub-state:
    /// the preset list takes simple cursor motion + Enter (with the
    /// trailing "Custom…" row transitioning into the calendar view);
    /// the calendar view takes day/week/month navigation + Tab to
    /// switch focus between start and end.
    fn handle_time_picker_key(&mut self, key: KeyEvent) {
        let state = match self.time_picker.take() {
            Some(s) => s,
            None => return,
        };
        match state {
            TimePickerState::Presets { cursor } => {
                self.handle_time_preset_key(cursor, key);
            }
            TimePickerState::Custom(picker) => {
                self.handle_time_custom_key(picker, key);
            }
        }
    }

    fn handle_time_preset_key(&mut self, cursor: usize, key: KeyEvent) {
        // Cursor range is 0..=TIME_PRESETS.len() — the last index is
        // the synthetic "Custom…" row.
        let n = TIME_PRESETS.len() + 1;
        let mut next_cursor = cursor;
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                // Already taken out via `take()`; just leave None.
                return;
            }
            (KeyCode::Enter, _) => {
                if cursor == TIME_PRESET_CUSTOM_INDEX {
                    // Transition to the calendar overlay, seeded from
                    // whatever the dashboard's current window parses
                    // as (defaulting to yesterday→today).
                    let mut picker = CustomRangePicker::seed();
                    if let Some(d) = parse_iso_date(&self.time_range.start) {
                        picker.start = d;
                    }
                    if let Some(d) = parse_iso_date(&self.time_range.end) {
                        picker.end = d;
                    }
                    self.time_picker = Some(TimePickerState::Custom(picker));
                    return;
                }
                let (_, duration) = TIME_PRESETS[cursor];
                self.set_time_range(format!("now-{duration}"), "now".to_string());
                return;
            }
            (KeyCode::Up, _)
            | (KeyCode::Char('k'), KeyModifiers::NONE)
            | (KeyCode::BackTab, _) => {
                next_cursor = (cursor + n - 1) % n;
            }
            (KeyCode::Down, _)
            | (KeyCode::Char('j'), KeyModifiers::NONE)
            | (KeyCode::Tab, _) => {
                next_cursor = (cursor + 1) % n;
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                next_cursor = 0;
            }
            (KeyCode::Char('G'), _) => {
                next_cursor = n - 1;
            }
            _ => {}
        }
        self.time_picker = Some(TimePickerState::Presets { cursor: next_cursor });
    }

    fn handle_time_custom_key(&mut self, mut picker: CustomRangePicker, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                // Step back to the preset list rather than closing
                // outright — lets the user undo Custom without losing
                // their place in the picker.
                self.time_picker = Some(TimePickerState::Presets {
                    cursor: TIME_PRESET_CUSTOM_INDEX,
                });
            }
            (KeyCode::Enter, _) => {
                let (start, end) = picker.to_range();
                self.set_time_range(start, end);
                // set_time_range doesn't touch time_picker; explicit None.
                self.time_picker = None;
            }
            (KeyCode::Tab, _)
            | (KeyCode::BackTab, _)
            | (KeyCode::Char('\t'), _) => {
                picker.focus = match picker.focus {
                    CustomField::Start => CustomField::End,
                    CustomField::End => CustomField::Start,
                };
                self.time_picker = Some(TimePickerState::Custom(picker));
            }
            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                picker.shift_days(-1);
                self.time_picker = Some(TimePickerState::Custom(picker));
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                picker.shift_days(1);
                self.time_picker = Some(TimePickerState::Custom(picker));
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                picker.shift_days(-7);
                self.time_picker = Some(TimePickerState::Custom(picker));
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                picker.shift_days(7);
                self.time_picker = Some(TimePickerState::Custom(picker));
            }
            (KeyCode::Char('<'), _)
            | (KeyCode::Char(','), KeyModifiers::SHIFT)
            | (KeyCode::Char('['), KeyModifiers::NONE) => {
                picker.shift_month(-1);
                self.time_picker = Some(TimePickerState::Custom(picker));
            }
            (KeyCode::Char('>'), _)
            | (KeyCode::Char('.'), KeyModifiers::SHIFT)
            | (KeyCode::Char(']'), KeyModifiers::NONE) => {
                picker.shift_month(1);
                self.time_picker = Some(TimePickerState::Custom(picker));
            }
            _ => {
                // Unrecognised key — keep the overlay open and the
                // picker state intact.
                self.time_picker = Some(TimePickerState::Custom(picker));
            }
        }
    }

    /// Serialise the current dashboard to pretty JSON. Errors when
    /// no dashboard is loaded. Pure helper exposed for tests of the
    /// round-trip; production code goes through `write_file`.
    #[cfg(test)]
    fn dashboard_to_json(&self) -> anyhow::Result<String> {
        use anyhow::anyhow;
        let resource = self
            .loaded_dashboard
            .as_ref()
            .ok_or_else(|| anyhow!("no dashboard loaded"))?;
        serde_json::to_string_pretty(resource).map_err(Into::into)
    }

    /// Adopt a freshly-loaded dashboard into the App. Swaps
    /// `self.dashboard` to the internal model derived from the wire
    /// `DashboardSummary`, and — if the focused chart carries an
    /// MPL query — seeds the editor buffer with that MPL plus a
    /// `// @viz` pragma matching the chart's kind, so the next
    /// `:r` (run query) executes the right thing.
    ///
    /// Charts using APL get their text seeded into the buffer behind a
    /// `// APL (read-only until 14b)` banner; the MPL parser will
    /// complain via diagnostics, which is the right signal until APL
    /// execution lands. Charts with no query at all leave the buffer
    /// untouched.
    /// Fan out one async fetch per MPL chart in the loaded dashboard.
    /// APL charts and chart variants without an MPL query are skipped
    /// (their tile renders an "APL" / "no query" placeholder).
    /// Each task posts an `AppEvent::TileQueryFinished` with the
    /// chart id; the handler stores the result in `App.tile_results`.
    ///
    /// Stale-result protection: when a new dashboard loads we clear
    /// `tile_results` first, so a slow task from the previous
    /// dashboard can't overwrite a fresh tile that happens to share an
    /// id (`c1`, `c2`, etc. are typical defaults).
    fn run_tile_queries(&mut self) {
        self.tile_results.clear();
        let Some(resource) = self.loaded_dashboard.as_ref() else {
            return;
        };
        // Snapshot what we need to spawn without holding any borrow.
        // Uses `extract_query` so MPL-stored-under-`apl` charts
        // (the home-overview case) also get fetched.
        let charts: Vec<(String, String)> = resource
            .dashboard
            .charts
            .iter()
            .filter_map(|c| {
                let mpl = match crate::dashboard::extract_query(c) {
                    crate::dashboard::Query::Mpl(s) if !s.trim().is_empty() => s,
                    _ => return None,
                };
                Some((c.base().id.clone(), mpl))
            })
            .collect();
        if charts.is_empty() {
            return;
        }
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => {
                self.set_error(format!("tile fetch: {e}"));
                return;
            }
        };
        let cache = self.cache.clone();
        let params = self.cli_params.clone();
        let (start, end) = self.active_time_range();
        for (chart_id, mpl) in charts {
            // Initial busy state — grid renderer reads this to show a
            // “loading…” hint.
            self.tile_results.insert(
                chart_id.clone(),
                TileQueryResult {
                    busy: true,
                    series: vec![],
                    error: None,
                    trace_id: None,
                },
            );
            let dataset = match mpl::extract_dataset_metric(&mpl) {
                Ok((d, _)) => d,
                Err(e) => {
                    self.tile_results.insert(
                        chart_id.clone(),
                        TileQueryResult {
                            busy: false,
                            series: vec![],
                            error: Some(format!("MPL: {e}")),
                            trace_id: None,
                        },
                    );
                    continue;
                }
            };
            let tx = self.events_tx.clone();
            let client = client.clone();
            let cache = cache.clone();
            let params = params.clone();
            let start = start.clone();
            let end = end.clone();
            self.runtime.spawn(async move {
                let result =
                    run_query_task(&cache, &client, &dataset, &mpl, &start, &end, &params).await;
                let _ = tx.send(AppEvent::TileQueryFinished { chart_id, result });
            });
        }
    }

    /// Switch into Grid view mode when the loaded dashboard has ≥2
    /// charts; otherwise stay in Solo. Called from `adopt_dashboard`
    /// and `open_file` so the user never has to manually flip into
    /// grid view to see a multi-tile dashboard.
    fn auto_switch_view_mode(&mut self) {
        let n = self
            .loaded_dashboard
            .as_ref()
            .map(|r| r.dashboard.charts.len())
            .unwrap_or(0);
        if n >= 2 {
            self.view_mode = ViewMode::Grid;
            self.focus = Pane::Dashboard;
        } else {
            self.view_mode = ViewMode::Solo;
        }
        self.selected_chart_idx = 0;
    }

    /// Build a pretty-printed JSON dump of the focused tile's `Chart`,
    /// or `None` if no dashboard / tile is selected. Used by
    /// `:tile json` to show the raw wire payload so we can debug
    /// query-classification questions.
    pub fn focused_chart_json(&self) -> Option<String> {
        let resource = self.loaded_dashboard.as_ref()?;
        let chart = resource.dashboard.charts.get(self.selected_chart_idx)?;
        serde_json::to_string_pretty(chart).ok()
    }

    /// Move the dashboard-pane selection by `delta`. Wraps within the
    /// chart list. No-op outside Grid mode.
    pub fn move_dashboard_selection(&mut self, delta: isize) {
        if self.view_mode != ViewMode::Grid {
            return;
        }
        let n = self
            .loaded_dashboard
            .as_ref()
            .map(|r| r.dashboard.charts.len())
            .unwrap_or(0);
        if n == 0 {
            return;
        }
        let i = self.selected_chart_idx as isize + delta;
        let wrapped = ((i % n as isize) + n as isize) % n as isize;
        self.selected_chart_idx = wrapped as usize;
        self.reload_legend_label_tags();
    }

    /// Spatial navigation in the dashboard grid: pick the chart whose
    /// `LayoutItem` centroid is nearest in the given direction.
    /// Falls back to row-major sequence cycling when no chart in the
    /// direction is closer than the current one (e.g. user is already
    /// on the edge).
    pub fn move_dashboard_selection_spatial(&mut self, dir: SpatialDir) {
        if self.view_mode != ViewMode::Grid {
            return;
        }
        let Some(resource) = self.loaded_dashboard.as_ref() else {
            return;
        };
        let charts = &resource.dashboard.charts;
        if charts.is_empty() {
            return;
        }
        if let Some(next) = pick_next_chart_in_direction(
            &resource.dashboard.layout,
            charts,
            self.selected_chart_idx,
            dir,
        ) {
            self.selected_chart_idx = next;
            self.reload_legend_label_tags();
            return;
        }
        // No spatial match — fall back to row-major cycle.
        // `move_dashboard_selection` already reloads tags.
        let delta = match dir {
            SpatialDir::Right | SpatialDir::Down => 1,
            SpatialDir::Left | SpatialDir::Up => -1,
        };
        self.move_dashboard_selection(delta);
    }

    /// Zoom the highlighted grid tile back into the single-tile
    /// renderer by re-seeding the editor buffer with that chart's
    /// MPL/APL. Drops view mode back to Solo + focuses the editor.
    pub fn zoom_selected_chart(&mut self) {
        use crate::dashboard::Query;
        let Some(resource) = self.loaded_dashboard.as_ref() else {
            return;
        };
        let Some(chart) = resource
            .dashboard
            .charts
            .get(self.selected_chart_idx)
            .cloned()
        else {
            return;
        };
        let kind = VizKind::from_chart(&chart);
        let query = crate::dashboard::extract_query(&chart);
        // The focused tile is whichever chart the user just zoomed
        // in on; reset opts (the wire chart has none) so the buffer
        // pragma is the only source of viz options.
        self.viz_kind = kind;
        self.viz_opts.clear();
        let pragma_line = format!("// @viz {}\n", kind.as_str());
        match &query {
            Query::Mpl(mpl) => {
                let text = format!("{pragma_line}{mpl}");
                self.editor = editor::editor_with_text(&text);
                self.recompute_diagnostics();
                // Pin the editor-side query context to the tile's
                // (dataset, metric) so the upcoming legend-tag
                // reload finds the right per-metric cache slot
                // (and any toggle persists under the tile's keys).
                // We don't know the AST hash without running the
                // pipeline; pass empty so `resolve_legend_tags`
                // falls through to the by-metric store.
                if let Ok((ds, m)) = crate::mpl::extract_dataset_metric(mpl) {
                    self.last_query_context = Some(QueryContext {
                        hash: String::new(),
                        dataset: ds,
                        metric: m,
                    });
                }
            }
            Query::Apl(apl) => {
                let text = format!(
                    "{pragma_line}// APL query — execution lands in step 14b\n// {apl}\n",
                    apl = apl.replace('\n', "\n// ")
                );
                self.editor = editor::editor_with_text(&text);
                self.recompute_diagnostics();
            }
            Query::Empty => {}
        }
        // Adopt the tile's last-known series into the Solo-view
        // `app.series` so the chart pane shows the real data
        // immediately instead of the sin(x) demo placeholder. The
        // tile data is already in `tile_results` from the dashboard
        // background fetch — we just promote it. A subsequent `:r`
        // (or the editor's run-on-Enter) will refresh it if the
        // user wants a fresh point-in-time.
        let chart_id = chart.base().id.clone();
        if let Some(tile) = self.tile_results.get(&chart_id) {
            self.series = tile.series.clone();
            self.legend_hidden = vec![false; self.series.len()];
            if self.legend_selected >= self.series.len() {
                self.legend_selected = 0;
            }
            if let Some(tid) = tile.trace_id.clone() {
                self.last_trace_id = Some(tid);
            }
        } else {
            // No tile data yet (zoom raced the fetch, or the tile
            // has no MPL). Clear so the user doesn't see stale
            // demo data labelled with a different tile's title.
            self.series.clear();
            self.legend_hidden.clear();
            self.legend_selected = 0;
        }
        self.view_mode = ViewMode::Solo;
        self.focus = Pane::Editor;
        // Now that `last_query_context` is pinned to the tile and
        // view mode is Solo, pick up that metric's saved tag
        // selection (or clear if there's nothing cached).
        self.reload_legend_label_tags();
        let title = chart
            .base()
            .name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| kind.as_str().to_string());
        self.status = format!("zoomed `{title}`");
    }

    fn adopt_dashboard(&mut self, uid: String, resource: crate::axiom::DashboardSummary) {
        use crate::dashboard::Query;
        let name = resource.name().to_string();
        let chart_count = resource.dashboard.charts.len();
        self.time_range = TimeRange::from_resource(&resource);
        // Focus snaps to the first chart — matches the grid's
        // initial selection and the prior `Dashboard::tiles[0]`
        // semantics. Empty dashboards fall through to defaults.
        let first_chart = resource.dashboard.charts.first().cloned();
        let (focused_kind, focused_query) = match first_chart.as_ref() {
            Some(c) => (VizKind::from_chart(c), crate::dashboard::extract_query(c)),
            None => (VizKind::default(), Query::Empty),
        };
        self.viz_kind = focused_kind;
        self.viz_opts.clear();
        self.last_picked_dashboard = Some(uid);
        self.loaded_dashboard = Some(resource);

        let pragma_line = format!("// @viz {}\n", focused_kind.as_str());
        let mut seeded: Option<String> = None;
        match &focused_query {
            Query::Mpl(mpl) => {
                let text = format!("{pragma_line}{mpl}");
                self.editor = editor::editor_with_text(&text);
                self.recompute_diagnostics();
                seeded = Some(text);
            }
            Query::Apl(apl) => {
                let text = format!(
                    "{pragma_line}// APL query — execution lands in step 14b\n// {apl}\n",
                    apl = apl.replace('\n', "\n// ")
                );
                self.editor = editor::editor_with_text(&text);
                self.recompute_diagnostics();
                seeded = Some(text);
            }
            Query::Empty => {
                // Leave the editor alone; tile renderer surfaces the
                // note body / placeholder directly.
            }
        }
        // Capture the seed *after* `recompute_diagnostics` so it
        // matches what `query_text()` will return for an untouched
        // buffer (line endings normalised by the editor).
        self.last_adopted_seed = seeded.map(|_| self.query_text());
        self.auto_switch_view_mode();
        // Adopted; pick up the initially focused tile's saved tags
        // (if any) so the legend renders the right labels from frame
        // zero, before any tile data lands.
        self.reload_legend_label_tags();
        // Kick off per-tile fetches so the grid renders live data.
        // Solo mode also benefits when the focused chart turns out to
        // have an MPL query — the existing single-tile flow runs on
        // `:r`, so this just primes things.
        self.run_tile_queries();
        self.status = format!("loaded `{name}` — {chart_count} chart(s); :dashinfo for details");
    }

    /// Kick off the async `GET /v2/dashboards/uid/{uid}` fetch.
    /// Shared between picker-Enter and `:open <uid>`.
    ///
    /// Snappy path: if the cache already has a copy for `uid`, adopt
    /// it immediately and spawn a background refresh; the fresh copy
    /// lands via `DashboardRefreshed` and silently updates the cached
    /// resource + version metadata, only re-adopting when the editor
    /// buffer is still pristine from the original adopt.
    ///
    /// Cold path: with no cache hit, the foreground `DashboardOpened`
    /// flow runs (sets `busy`, status "fetching dashboard …"). The
    /// dashboard endpoint is orthogonal to the datasets/query
    /// pipelines, so this intentionally does **not** gate on
    /// `self.busy` — startup paths (`-d <uid>`) and picker-Enter
    /// must succeed even when a datasets fetch is in flight.
    pub fn fetch_dashboard_by_uid(&mut self, uid: String) {
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => {
                self.set_error(format!("config error: {e}"));
                return;
            }
        };
        let cached = self.cache.read().unwrap().cached_dashboard(&uid);
        if let Some(resource) = cached {
            let name = resource.name().to_string();
            self.adopt_dashboard(uid.clone(), resource);
            self.status = format!("loaded `{name}` (cached, refreshing…)");
            let tx = self.events_tx.clone();
            let cache = self.cache.clone();
            let uid_for_task = uid.clone();
            self.runtime.spawn(async move {
                let result = client.get_dashboard(&uid_for_task).await;
                if let Ok(resource) = &result {
                    let mut c = cache.write().unwrap();
                    c.replace_dashboard(&uid_for_task, resource.clone());
                    if let Err(e) = c.save() {
                        eprintln!("metrics-tui: cache save failed: {e}");
                    }
                }
                let _ = tx.send(AppEvent::DashboardRefreshed {
                    uid: uid_for_task,
                    result,
                });
            });
            return;
        }
        self.busy = true;
        self.status = format!("fetching dashboard {uid}…");
        let tx = self.events_tx.clone();
        let cache = self.cache.clone();
        let uid_for_task = uid.clone();
        self.runtime.spawn(async move {
            let result = client.get_dashboard(&uid_for_task).await;
            if let Ok(resource) = &result {
                let mut c = cache.write().unwrap();
                c.replace_dashboard(&uid_for_task, resource.clone());
                if let Err(e) = c.save() {
                    eprintln!("metrics-tui: cache save failed: {e}");
                }
            }
            let _ = tx.send(AppEvent::DashboardOpened {
                uid: uid_for_task,
                result,
            });
        });
    }

    /// Keymap for the dashboard picker overlay. The filter is
    /// edit-as-you-type; printable characters extend it, Backspace
    /// removes the last char, and navigation keys scroll the filtered
    /// list.
    fn handle_dashboards_picker_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.dashboards.hide();
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                self.dashboards.move_cursor(-1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.dashboards.move_cursor(1);
            }
            (KeyCode::PageUp, _) => {
                self.dashboards.move_cursor(-10);
            }
            (KeyCode::PageDown, _) => {
                self.dashboards.move_cursor(10);
            }
            (KeyCode::Enter, _) => {
                if let Some(sel) = self.dashboards.selected() {
                    let uid = sel.uid.clone();
                    let name = sel.name().to_string();
                    self.last_picked_dashboard = Some(uid.clone());
                    self.fetch_dashboard_by_uid(uid.clone());
                    self.status = format!("opening dashboard `{name}` …");
                }
                self.dashboards.hide();
            }
            (KeyCode::Backspace, _) => {
                self.dashboards.filter.pop();
                self.dashboards.cursor = 0;
            }
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                self.dashboards.filter.push(c);
                self.dashboards.cursor = 0;
            }
            _ => {}
        }
    }

    fn do_open(&mut self, path: std::path::PathBuf, force: bool) {
        if !force && self.is_dirty() {
            self.set_error("E37: No write since last change (add ! to override)".to_string());
            return;
        }
        match self.open_file(path) {
            Ok(p) => self.status = format!("opened {}", display_path(&p)),
            Err(e) => self.set_error(format!("open failed: {e}")),
        }
    }

    /// Read `path` into the App. The behaviour branches on the file's
    /// content:
    ///
    /// * If the path ends in `.axiom.json` *or* the JSON has a
    ///   top-level `dashboard` object key, it's treated as a saved
    ///   `DashboardResource` envelope: parse it, adopt as the loaded
    ///   dashboard, switch `buffer_mode` to `Dashboard`.
    /// * Otherwise it's a plain MPL buffer (existing behaviour);
    ///   buffer_mode stays `Mpl`.
    ///
    /// `current_file` is updated either way so `:w` writes to the same
    /// place.
    pub fn open_file(&mut self, path: std::path::PathBuf) -> anyhow::Result<std::path::PathBuf> {
        use anyhow::Context;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", display_path(&path)))?;
        if Self::looks_like_dashboard_file(&path, &text) {
            // Dashboard JSON: parse + adopt.
            let resource: crate::axiom::DashboardSummary = serde_json::from_str(&text)
                .with_context(|| format!("parsing dashboard JSON {}", display_path(&path)))?;
            let uid = resource.uid.clone();
            self.adopt_dashboard(uid, resource);
            self.buffer_mode = BufferMode::Dashboard;
            self.current_file = Some(path.clone());
            self.saved_buffer = text;
            self.last_error = None;
            return Ok(path);
        }
        self.buffer_mode = BufferMode::Mpl;
        self.editor = editor::editor_with_text(&text);
        self.saved_buffer = text;
        self.current_file = Some(path.clone());
        self.last_error = None;
        self.recompute_diagnostics();
        Ok(path)
    }

    /// Sniff whether `path` + `body` smell like a saved Axiom
    /// dashboard. Extension is the fast path; the magic-key probe is
    /// the safety net for files with non-canonical extensions.
    fn looks_like_dashboard_file(path: &std::path::Path, body: &str) -> bool {
        if let Some(ext) = path.file_name().and_then(|n| n.to_str())
            && (ext.ends_with(".axiom.json") || ext.ends_with(".dashboard.json"))
        {
            return true;
        }
        // Magic-key sniff: a `DashboardResource` envelope always has a
        // nested `"dashboard"` object. Bound the probe to the first 1k
        // bytes so we don't scan megabytes of unrelated JSON.
        let head = &body[..body.len().min(1024)];
        head.contains("\"dashboard\"") && head.contains("\"uid\"")
    }

    /// Write the current artifact to `path` (or `current_file` if
    /// `None`). Routes on `buffer_mode`:
    ///
    /// * `Mpl` — writes the editor buffer (long-standing behaviour).
    /// * `Dashboard` — serialises `loaded_dashboard` to pretty JSON
    ///   and writes that. The buffer is **not** synced back into the
    ///   focused chart (that's a 17d/17e concern); the user explicitly
    ///   edits a dashboard's structure through `:dash`-prefixed
    ///   commands.
    ///
    /// Writes go through a `<path>.tmp` → rename dance so a crash
    /// mid-write doesn't truncate the previous good copy.
    pub fn write_file(
        &mut self,
        path: Option<std::path::PathBuf>,
    ) -> anyhow::Result<std::path::PathBuf> {
        use anyhow::{Context, anyhow};
        let target = match path {
            Some(p) => p,
            None => self
                .current_file
                .clone()
                .ok_or_else(|| anyhow!("E32: No file name"))?,
        };
        if let Some(parent) = target.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", display_path(parent)))?;
        }
        let text = match self.buffer_mode {
            BufferMode::Mpl => self.query_text(),
            BufferMode::Dashboard => {
                let resource = self
                    .loaded_dashboard
                    .as_ref()
                    .ok_or_else(|| anyhow!("no dashboard loaded"))?;
                serde_json::to_string_pretty(resource).context("serialising dashboard JSON")?
            }
        };
        // Atomic-ish write: temp file in same dir + rename.
        let mut tmp = target.clone();
        let mut filename = target
            .file_name()
            .ok_or_else(|| anyhow!("target has no file name"))?
            .to_os_string();
        filename.push(".tmp");
        tmp.set_file_name(filename);
        std::fs::write(&tmp, &text).with_context(|| format!("writing {}", display_path(&tmp)))?;
        std::fs::rename(&tmp, &target).with_context(|| {
            format!(
                "renaming {} → {}",
                display_path(&tmp),
                display_path(&target)
            )
        })?;
        self.saved_buffer = text;
        self.current_file = Some(target.clone());
        if self.buffer_mode == BufferMode::Dashboard {
            self.dashboard_dirty = false;
        }
        Ok(target)
    }

    /// `true` when the editor buffer has unsaved changes compared to the last
    /// load or write.
    pub fn is_dirty(&self) -> bool {
        self.query_text() != self.saved_buffer
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
        if let Err(e) = self.cache.read().unwrap().save_query(&text) {
            eprintln!("metrics-tui: query cache save failed: {e}");
        }
    }

    fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::DatasetsFetched(Ok(datasets)) => {
                let count = datasets.len();
                self.busy = false;
                self.status = format!("loaded {count} dataset(s)");
            }
            AppEvent::DatasetsFetched(Err(e)) => {
                self.busy = false;
                self.set_error(format!("datasets error: {e}"));
            }
            AppEvent::DashboardsFetched(Ok(items)) => {
                self.busy = false;
                let n = items.len();
                self.dashboards.open(items);
                self.status = format!("{n} dashboard(s)");
            }
            AppEvent::DashboardsFetched(Err(e)) => {
                self.busy = false;
                self.set_error(format!("dashboards error: {e}"));
            }
            AppEvent::DashboardsRefreshed(Ok(items)) => {
                let n = items.len();
                // Quietly update the picker if it's still showing the
                // cached list; otherwise the cache write (already done
                // in the spawn closure) is enough for next time.
                if self.dashboards.visible {
                    self.dashboards.refresh_items(items);
                    self.status = format!("{n} dashboard(s) (refreshed)");
                }
            }
            AppEvent::DashboardsRefreshed(Err(e)) => {
                // Background failure — keep the cached list visible and
                // log a soft status message.
                self.status = format!("dashboards refresh failed: {e}");
            }
            AppEvent::DashboardOpened { uid, result } => {
                self.busy = false;
                match result {
                    Ok(resource) => {
                        self.adopt_dashboard(uid, resource);
                    }
                    Err(e) => {
                        self.set_error(format!("open {uid}: {e}"));
                    }
                }
            }
            AppEvent::DashboardRefreshed { uid, result } => match result {
                Ok(resource) => {
                    let still_focused = self
                        .loaded_dashboard
                        .as_ref()
                        .is_some_and(|d| d.uid == uid);
                    if !still_focused {
                        // User moved on to a different dashboard while
                        // the refresh was in flight. Cache is already
                        // updated; nothing else to do.
                        return;
                    }
                    let pristine = !self.dashboard_dirty
                        && self.last_adopted_seed.as_deref() == Some(self.query_text().as_str());
                    if pristine {
                        let name = resource.name().to_string();
                        self.adopt_dashboard(uid, resource);
                        self.status = format!("refreshed `{name}`");
                    } else {
                        // Editor has unsaved work — don't clobber it.
                        // Refresh just the resource metadata so saves
                        // round-trip against the latest version.
                        self.loaded_dashboard = Some(resource);
                        self.status =
                            "dashboard refreshed (editor kept; reload to discard edits)"
                                .to_string();
                    }
                }
                Err(e) => {
                    // Background failure — keep the cached copy and
                    // surface the error softly.
                    self.status = format!("refresh {uid} failed: {e}");
                }
            },
            AppEvent::DashboardSaved { uid, result } => {
                self.busy = false;
                match result {
                    Ok(write) => {
                        let new_version = write.dashboard.version;
                        let verb = match write.status {
                            crate::axiom::DashboardWriteStatus::Created => "created",
                            crate::axiom::DashboardWriteStatus::Updated => "updated",
                        };
                        // Re-stamp the in-memory copy with the new
                        // version + audit fields so the next save
                        // round-trips correctly.
                        // Keep the per-uid cache in sync with the
                        // server's bumped version so the next session
                        // adopts a current resource immediately.
                        {
                            let mut c = self.cache.write().unwrap();
                            c.replace_dashboard(&write.dashboard.uid, write.dashboard.clone());
                            if let Err(e) = c.save() {
                                eprintln!("metrics-tui: cache save failed: {e}");
                            }
                        }
                        self.loaded_dashboard = Some(write.dashboard);
                        self.dashboard_dirty = false;
                        self.status = format!(
                            "{verb} dashboard {uid} — version {}",
                            new_version
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "?".to_string())
                        );
                    }
                    Err(e) => {
                        self.set_error(format!("save {uid}: {e}"));
                    }
                }
            }
            AppEvent::TileQueryFinished { chart_id, result } => {
                // The slot may have been cleared (dashboard swap,
                // tile deleted) between dispatch and arrival; in that
                // case drop the result silently.
                let entry = self.tile_results.entry(chart_id.clone()).or_default();
                entry.busy = false;
                match result {
                    Ok(resp) => {
                        entry.trace_id = resp.trace_id.clone();
                        entry.series = response_to_series(&resp);
                        entry.error = None;
                    }
                    Err(e) => {
                        entry.error = Some(format!("{e}"));
                    }
                }
                // If the finished tile is the currently-focused one,
                // reload tags now — `adopt_dashboard` ran the lookup
                // before any tile data was around, but the lookup is
                // metric-keyed and doesn't depend on data, so this is
                // a cheap no-op in the steady state. It still matters
                // for the case where the dashboard adopted from
                // cache, the user toggled tags, then the background
                // refresh landed and could have stomped buffer
                // state — keeping things in sync defensively.
                if self.current_chart_id().as_deref() == Some(&chart_id) {
                    self.reload_legend_label_tags();
                }
            }
            AppEvent::DashboardDeleted { uid, result } => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        // Clear the local copy if the deletion targeted
                        // it; otherwise leave the in-memory dashboard
                        // alone (we just rm'd a different one).
                        if self.loaded_dashboard.as_ref().is_some_and(|d| d.uid == uid) {
                            self.loaded_dashboard = None;
                            self.last_picked_dashboard = None;
                            self.last_adopted_seed = None;
                        }
                        // Evict from the dashboard cache so we don't
                        // re-adopt a tombstoned dashboard on the next
                        // `:open <uid>`.
                        {
                            let mut c = self.cache.write().unwrap();
                            c.forget_dashboard(&uid);
                            if let Err(e) = c.save() {
                                eprintln!("metrics-tui: cache save failed: {e}");
                            }
                        }
                        self.status = format!("deleted dashboard {uid}");
                    }
                    Err(e) => {
                        self.set_error(format!("delete {uid}: {e}"));
                    }
                }
            }
            AppEvent::MetricsFetched {
                dataset,
                result: Ok(metrics),
            } => {
                self.busy = false;
                let count = metrics.len();
                self.status = format!("loaded {count} metric(s) for `{dataset}`");
            }
            AppEvent::MetricsFetched {
                dataset,
                result: Err(e),
            } => {
                self.busy = false;
                self.set_error(format!("metrics error for `{dataset}`: {e}"));
            }
            AppEvent::TagsFetched {
                dataset,
                metric,
                result: Ok(tags),
            } => {
                // Background prefetch — only update status if no foreground op
                // is in flight, otherwise we'd clobber e.g. "running query…".
                if !self.busy {
                    let count = tags.len();
                    self.status = format!("loaded {count} tag(s) for `{dataset}:{metric}`");
                }
            }
            AppEvent::TagsFetched {
                dataset,
                metric,
                result: Err(e),
            } => {
                if !self.busy {
                    self.status = format!("tags error for `{dataset}:{metric}`: {e}");
                }
            }
            AppEvent::TagValuesFetched {
                dataset,
                metric,
                tag,
                result: Ok(values),
            } => {
                if !self.busy {
                    let count = values.len();
                    self.status = format!("loaded {count} value(s) for `{dataset}:{metric}.{tag}`");
                }
            }
            AppEvent::TagValuesFetched {
                dataset,
                metric,
                tag,
                result: Err(e),
            } => {
                if !self.busy {
                    self.status = format!("values error for `{dataset}:{metric}.{tag}`: {e}");
                }
            }
            AppEvent::QueryFinished { id, result } => {
                if id != self.last_query_id {
                    // Stale response from a superseded query; ignore.
                    return;
                }
                self.busy = false;
                match result {
                    Ok(resp) => {
                        self.last_trace_id = resp.trace_id.clone();
                        let new_series = response_to_series(&resp);
                        let count = new_series.len();
                        if count == 0 {
                            self.status = "query returned no series".to_string();
                        } else {
                            self.series = new_series;
                            // Reset legend state. Carrying `hidden` across
                            // queries would require name-stable matching
                            // and surprises the user when the result set
                            // changes shape.
                            self.legend_hidden = vec![false; count];
                            if self.legend_selected >= count {
                                self.legend_selected = 0;
                            }
                            // Restore the user's tag-label choice
                            // from cache for the current active
                            // context (Solo here = editor's last
                            // query). Centralised so Grid-view
                            // focus changes use the same path.
                            self.reload_legend_label_tags();
                            self.status = format!("{count} series");
                        }
                    }
                    Err(e) => {
                        // Keep previously good series on error.
                        self.set_error(format!("query error: {e}"));
                    }
                }
            }
        }
    }

    fn open_completions(&mut self) {
        let Some(payload) = self.compute_completion_payload() else {
            self.completions.hide();
            self.status = "no completions".to_string();
            return;
        };
        if payload.items.is_empty() {
            self.completions.hide();
            self.maybe_kick_off_discovery(&payload.kind);
            return;
        }
        self.completions = state_from(payload, 0);
    }

    fn refresh_completions(&mut self) {
        let previous_selected = self.completions.selected;
        let Some(payload) = self.compute_completion_payload() else {
            self.completions.hide();
            return;
        };
        if payload.items.is_empty() {
            self.completions.hide();
            return;
        }
        let selected = previous_selected.min(payload.items.len() - 1);
        self.completions = state_from(payload, selected);
    }

    fn compute_completion_payload(&self) -> Option<completions::CompletionPayload> {
        let query = self.query_text();
        let cursor_byte = editor_cursor_byte_offset(&self.editor);
        completions::compute(
            &query,
            cursor_byte,
            &self.system_params,
            &self.cache.read().unwrap(),
        )
    }

    /// When a cache-backed context has nothing to offer, transparently kick off the
    /// fetch the user would otherwise have to invoke manually (`D` / `M`).
    fn maybe_kick_off_discovery(&mut self, kind: &completions::CompletionKind) {
        if self.busy {
            self.status = "no completions".to_string();
            return;
        }
        match kind {
            completions::CompletionKind::Dataset
                if self.cache.read().unwrap().dataset_count() == 0 =>
            {
                self.status = "no datasets cached — fetching…".to_string();
                self.fetch_datasets();
            }
            completions::CompletionKind::Metric { dataset }
                if !dataset.is_empty()
                    && self.cache.read().unwrap().metric_names(dataset).is_empty() =>
            {
                self.status = format!("no metrics cached for `{dataset}` — fetching…");
                self.fetch_metrics_for_current_query();
            }
            _ => {
                self.status = "no completions".to_string();
            }
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
        if !self.cli_params.is_empty() {
            let n = self.cli_params.len();
            let plural = if n == 1 { "param" } else { "params" };
            self.status = format!("{}; {n} CLI {plural}", self.status);
        }
        if self.cache.read().unwrap().dataset_count() == 0 {
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
    pub fn recompute_diagnostics(&mut self) {
        let text = self.query_text();
        self.diagnostics = mpl::analyze(&text, &self.system_params);
        self.sync_dashboard_from_buffer(&text);
        self.recompute_sig_help();
    }

    /// Reconcile the focused tile's `kind`, `opts`, and MPL query text
    /// with whatever's in the editor buffer. Called by
    /// [`recompute_diagnostics`] on every buffer change, so the dashboard
    /// model is always in sync without scheduling extra passes.
    ///
    /// Pragma parse errors are pushed onto `self.diagnostics` so they
    /// surface alongside MPL diagnostics in the status bar and pane chrome.
    /// On error we keep the previous kind/opts so the chart doesn't
    /// flicker between renders while the user is mid-edit.
    fn sync_dashboard_from_buffer(&mut self, text: &str) {
        match viz::parse_pragma(text) {
            Ok(Some(spec)) => {
                self.viz_kind = spec.kind;
                self.viz_opts = spec.opts;
            }
            Ok(None) => {
                self.viz_kind = VizKind::default();
                self.viz_opts.clear();
            }
            Err((line_idx, err)) => {
                self.diagnostics
                    .push(pragma_diagnostic(text, line_idx, &err));
            }
        }
    }

    /// Refresh the status-line signature help from the current cursor.
    /// Cheap (single backwards byte scan + one stdlib lookup); fine to call
    /// on every keystroke and cursor move.
    pub fn recompute_sig_help(&mut self) {
        let text = self.query_text();
        let cursor = editor_cursor_byte_offset(&self.editor);
        self.sig_help = hover::find_call_context(&text, cursor);
    }

    /// Open the quick-fix picker for whichever diagnostic the editor cursor
    /// is sitting in. Falls back to the first diagnostic with any actions
    /// when the cursor isn't on one. No-op when nothing is fixable.
    fn open_quickfix(&mut self) {
        let cursor_byte = editor_cursor_byte_offset(&self.editor);
        let target = self
            .diagnostics
            .iter()
            .find(|d| d.span_contains(cursor_byte) && !d.actions.is_empty())
            .or_else(|| self.diagnostics.iter().find(|d| !d.actions.is_empty()));
        let Some(diag) = target else {
            self.status = "no quick fix available".to_string();
            return;
        };
        self.quickfix = QuickFixPicker {
            visible: true,
            actions: diag.actions.clone(),
            selected: 0,
            title: diag.message.clone(),
        };
    }

    fn move_quickfix_selection(&mut self, delta: isize) {
        if self.quickfix.actions.is_empty() {
            return;
        }
        let len = self.quickfix.actions.len();
        let i = self.quickfix.selected as isize + delta;
        let wrapped = ((i % len as isize) + len as isize) % len as isize;
        self.quickfix.selected = wrapped as usize;
    }

    fn accept_quickfix(&mut self) {
        if !self.quickfix.visible {
            return;
        }
        let Some(action) = self.quickfix.actions.get(self.quickfix.selected).cloned() else {
            self.quickfix.hide();
            return;
        };
        let query = self.query_text();
        let start_byte = action.byte_offset;
        let end_byte = action.byte_offset + action.byte_length;
        let (row, start_char) = byte_offset_to_row_col(&query, start_byte);
        let (_, end_char) = byte_offset_to_row_col(&query, end_byte);
        let replace_chars = end_char.saturating_sub(start_char);

        self.editor
            .move_cursor(CursorMove::Jump(row as u16, start_char as u16));
        self.editor.delete_str(replace_chars);
        self.editor.insert_str(&action.insert);
        self.status = format!("applied: {}", action.name);
        self.quickfix.hide();
        self.recompute_diagnostics();
    }

    fn move_completion_selection(&mut self, delta: isize) {
        if self.completions.items.is_empty() {
            return;
        }
        let len = self.completions.items.len();
        let i = self.completions.selected as isize + delta;
        let wrapped = ((i % len as isize) + len as isize) % len as isize;
        self.completions.selected = wrapped as usize;
    }

    fn accept_completion(&mut self) {
        if !self.completions.visible {
            return;
        }
        let item = match self.completions.items.get(self.completions.selected) {
            Some(it) => it.clone(),
            None => {
                self.completions.hide();
                return;
            }
        };
        let Some(kind) = self.completions.kind.clone() else {
            self.completions.hide();
            return;
        };
        let (start_byte, end_byte) = self.completions.replace_range_bytes;
        let query = self.query_text();
        let (row, start_char) = byte_offset_to_row_col(&query, start_byte);
        let (_, end_char) = byte_offset_to_row_col(&query, end_byte);
        let replace_chars = end_char.saturating_sub(start_char);

        self.editor
            .move_cursor(CursorMove::Jump(row as u16, start_char as u16));
        self.editor.delete_str(replace_chars);
        self.editor.insert_str(&item.apply);
        self.completions.hide();
        self.recompute_diagnostics();

        // When the user just picked a metric, kick off a background tag fetch
        // for the `(dataset, metric)` pair so the next `where`-position
        // completion can offer tag names. Cached pairs are skipped inside
        // `fetch_tags`.
        if let completions::CompletionKind::Metric { dataset } = &kind
            && !dataset.is_empty()
        {
            self.fetch_tags(dataset.clone(), item.label.clone());
        }

        // When the user just picked a tag name, prefetch its values so the
        // value popup has data the moment they type the comparison operator.
        if let completions::CompletionKind::Tag { dataset, metric } = &kind
            && !dataset.is_empty()
            && !metric.is_empty()
        {
            self.fetch_tag_values(dataset.clone(), metric.clone(), item.label.clone());
        }
    }
}

#[cfg(test)]
mod tests;
