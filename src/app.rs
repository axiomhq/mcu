use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, RwLock};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::runtime::Handle;
use tui_textarea::{CursorMove, TextArea};

use crate::axiom::{
    Client as AxiomClient, DashboardSummary, DatasetSummary, MetricInfo,
    MetricsQueryResponse, MetricsSeries,
};
use crate::cache::{Cache, EdgeRoute};
use crate::chart::{Series, color_for};
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

/// Background events delivered into the UI loop from spawned async tasks.
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
    /// in `App.tile_results` under that key.
    TileQueryFinished {
        chart_id: String,
        result: anyhow::Result<MetricsQueryResponse>,
    },
    /// Background refresh of the org's dashboard list. Fires after a
    /// cached list was shown immediately on `:dashboards`. Errors are
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
    /// occasional failed refresh doesn't blank the tile.
    pub series: Vec<Series>,
    /// Last error message, if the most recent fetch failed.
    pub error: Option<String>,
    /// Server trace id from the most recent successful fetch.
    /// Surfaced by `:trace` so the user can grab it for support/debug.
    pub trace_id: Option<String>,
}

/// Default time range applied to every MPL query (the `_mpl` endpoint accepts
/// relative expressions).
const DEFAULT_START: &str = "now-1h";
const DEFAULT_END: &str = "now";

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
        let yesterday = today
            .checked_sub(time::Duration::days(1))
            .unwrap_or(today);
        Self {
            start: yesterday,
            end: today,
            focus: CustomField::Start,
        }
    }

    /// Mutable accessor for the currently-focused date so the keymap
    /// can shift it without re-matching on `focus`.
    fn focused_mut(&mut self) -> &mut time::Date {
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
        (
            format!("{lo}T00:00:00Z"),
            format!("{hi}T23:59:59Z"),
        )
    }
}

/// Discovery window for `list_metrics`. The `metrics/info` endpoint only accepts
/// RFC3339 timestamps, so we materialise these per-request from system time.
const DISCOVERY_WINDOW_HOURS: i64 = 24;

fn rfc3339_now_window(hours_back: i64) -> (String, String) {
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
}

/// Where the main visualisation area focuses. `Solo` is the
/// long-standing single-tile renderer; `Grid` shows all of a loaded
/// dashboard's charts at once. Solo is the default for fresh sessions
/// and `.mpl` buffers; loading a multi-chart dashboard auto-switches
/// to Grid (overridable with `:solo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Solo,
    Grid,
}

/// Tile editing sub-mode while focus is on `Pane::Dashboard`.
///
/// `Idle` is the default — arrow keys navigate selection. Any of
/// `m` / `s` / `d` / `a` enters a sub-mode where the keymap changes:
///
/// * `Move{original}` — arrow keys nudge the selected tile by one
///   virtual-grid cell. `Enter` commits; `Esc` restores `original`.
///   Mutations that would overlap another tile are rejected.
/// * `Resize{original}` — Right/Down grow `w`/`h`; Left/Up shrink
///   (clamped to a 1-cell minimum and 12-col width).
/// * `ConfirmDelete` — `y` removes the selected tile; any other key
///   cancels. No keyboard accelerator can fire by accident here.
/// * `AddPick{cursor}` — kind-picker overlay; arrow keys move the
///   cursor across the implemented `VizKind`s and `Enter` inserts a
///   new tile at the first free grid slot.
#[derive(Debug, Clone, Default)]
pub enum TileSubMode {
    #[default]
    Idle,
    Move {
        original: crate::axiom::LayoutItem,
    },
    Resize {
        original: crate::axiom::LayoutItem,
    },
    ConfirmDelete,
    AddPick {
        cursor: usize,
    },
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

/// Buffer + cursor for the `:` command line.
#[derive(Default, Debug, Clone)]
pub struct CmdLine {
    /// Text after the `:` prompt, without the prompt itself.
    pub buf: String,
    /// Cursor position in `buf`, measured in chars (not bytes).
    pub cursor: usize,
}

impl CmdLine {
    pub fn reset(&mut self) {
        self.buf.clear();
        self.cursor = 0;
    }

    fn byte_cursor(&self) -> usize {
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
    fn hide(&mut self) {
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
#[derive(Debug, Default)]
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
    fn hide(&mut self) {
        self.visible = false;
        self.actions.clear();
        self.selected = 0;
        self.title.clear();
    }
}

/// Searchable picker over the org's dashboards. Opened by `:dashboards`,
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
        sorted.sort_by_key(|a| a.name().to_lowercase());
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
        sorted.sort_by_key(|a| a.name().to_lowercase());
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
                d.name().to_lowercase().contains(&needle)
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

    /// Dispatch a stripped command string to the matching action. Empty input
    /// is a no-op (matches vim). Unknown commands surface as an error overlay.
    pub fn execute_command(&mut self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }
        let mut parts = cmd.split_whitespace();
        let raw_head = parts.next().unwrap_or("");
        let args: Vec<&str> = parts.collect();
        // Honour vim's trailing-bang convention: `q!`, `w!`, `e!`, `wq!`.
        let (head, bang) = match raw_head.strip_suffix('!') {
            Some(rest) => (rest, true),
            None => (raw_head, false),
        };
        match head {
            "q" | "quit" => self.cmd_quit(bang),
            "w" | "write" => self.cmd_write(args.first().copied()),
            "wq" => self.cmd_write_quit(args.first().copied(), bang),
            "x" => self.cmd_update_quit(args.first().copied()),
            "e" | "edit" => self.cmd_edit(args.first().copied(), bang),
            "r" | "run" => self.run_query(),
            "ds" | "datasets" => self.fetch_datasets(),
            "m" | "metrics" => self.fetch_metrics_for_current_query(),
            "refresh" => {
                // Refresh both discovery layers and re-run the current query.
                self.fetch_datasets();
            }
            "help" | "h" => self.open_help(),
            "ax" | "axiom" => self.cmd_axiom_open(),
            "viz" => self.cmd_viz(args.first().copied()),
            "dashboards" | "db" => self.cmd_dashboards(),
            "open" => self.cmd_open(args.first().copied()),
            "trace" => self.cmd_trace(),
            "time" | "range" => self.cmd_time(&args),
            "dashinfo" | "di" => self.cmd_dashinfo(),
            "dash" => self.cmd_dash(&args, bang),
            "grid" => self.cmd_grid(),
            "solo" => self.cmd_solo(),
            "tile" => self.cmd_tile(&args),
            "p" | "param" => {
                // Use the raw `cmd` slice (not `args`) so values can
                // contain spaces and `=`. `param` itself was already
                // stripped of its trailing `!` and is the first token.
                let rest = cmd
                    .split_whitespace()
                    .next()
                    .map(|h| cmd[h.len()..].trim_start())
                    .unwrap_or("");
                self.cmd_param(rest, bang);
            }
            _ => self.set_error(format!("unknown command: {raw_head}")),
        }
    }

    /// `:param`               — list current params.
    /// `:param!`              — clear all params.
    /// `:param NAME=VALUE`    — set or update a param value.
    /// `:param NAME=`         — clear that param.
    ///
    /// VALUE must be valid MPL (the same `param_value` grammar the engine
    /// uses): durations like `5m`, numbers `42`/`5.0`, booleans `true`,
    /// strings `"hello world"`, regexes `/foo/`, idents. Invalid values
    /// are rejected with an error so typos surface immediately.
    ///
    /// Mirrors the `-p NAME=VALUE` CLI flag: same name canonicalization
    /// (leading `$` stripped) so `:p $foo=bar` and `:p foo=bar` behave the
    /// same.
    fn cmd_param(&mut self, rest: &str, clear_all: bool) {
        if clear_all {
            let n = self.cli_params.len();
            self.cli_params.clear();
            self.status = format!("cleared {n} param(s)");
            return;
        }
        let rest = rest.trim();
        if rest.is_empty() {
            if self.cli_params.is_empty() {
                self.status = "no params set".to_string();
            } else {
                let s = self
                    .cli_params
                    .iter()
                    .map(|(k, v)| format!("${k}={v}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                self.status = s;
            }
            return;
        }
        let Some((name, value)) = rest.split_once('=') else {
            self.set_error(format!("expected NAME=VALUE, got `{rest}`"));
            return;
        };
        let name = name.trim().strip_prefix('$').unwrap_or_else(|| name.trim());
        if name.is_empty() {
            self.set_error("empty parameter name".to_string());
            return;
        }
        if value.is_empty() {
            if self.cli_params.remove(name).is_some() {
                self.status = format!("cleared ${name}");
            } else {
                self.status = format!("${name} not set");
            }
            return;
        }
        // Reject values that aren't valid MPL. We delegate to the same
        // `param_value` grammar rule the engine uses when validating
        // provided params, so error semantics match exactly.
        if let Err(e) = validate_param_value(value) {
            self.set_error(format!("invalid value for ${name}: {e}"));
            return;
        }
        self.cli_params.insert(name.to_string(), value.to_string());
        self.status = format!("set ${name}={value}");
    }

    /// `:ax` / `:axiom` — open the current query in the Axiom web UI.
    ///
    /// Builds a deep-link URL with `initForm=<json>` and hands it to the
    /// system browser. Sends the buffer verbatim — server-side `$__interval`
    /// and other system params are resolved by Axiom when the page loads.
    fn cmd_axiom_open(&mut self) {
        let mpl = self.query_text();
        if mpl.trim().is_empty() {
            self.status = "empty query".to_string();
            return;
        }
        // Dataset is best-effort: the metrics explorer just needs `apl` set,
        // and `metricsDataset` is a hint that selects the right tab.
        let dataset = mpl::extract_dataset_metric(&mpl).map(|p| p.0).ok();
        let (deployment_url, org_id) = match Config::load() {
            Ok(cfg) => match cfg.active() {
                Ok((_, dep)) => (dep.url.clone(), dep.org_id.clone()),
                Err(e) => {
                    self.set_error(format!("axiom config: {e}"));
                    return;
                }
            },
            Err(e) => {
                self.set_error(format!("axiom config: {e}"));
                return;
            }
        };
        if org_id.is_empty() {
            self.set_error("axiom config missing org_id".to_string());
            return;
        }
        let url = share::build_axiom_url(&deployment_url, &org_id, &mpl, dataset.as_deref());
        match share::open_in_browser(&url) {
            Ok(()) => self.status = "opened in axiom".to_string(),
            Err(e) => self.set_error(format!("open failed: {e}")),
        }
    }

    /// `:viz`                 — print the current kind.
    /// `:viz <kind>`          — set the focused tile's kind and rewrite the
    ///                          buffer's `// @viz` pragma so the choice
    ///                          persists in the file.
    fn cmd_viz(&mut self, kind_arg: Option<&str>) {
        let Some(kind_str) = kind_arg else {
            self.status = format!("viz: {}", self.viz_kind.as_str());
            return;
        };
        let Some(kind) = VizKind::parse(kind_str) else {
            self.set_error(format!("unknown viz kind: `{kind_str}`"));
            return;
        };
        // Update the focused-tile kind, then re-emit the pragma into
        // the buffer so saving the file persists the choice.
        self.viz_kind = kind;
        let opts = self.viz_opts.clone();
        let spec = viz::VizSpec { kind, opts };
        let new_text = viz::upsert_pragma(&self.query_text(), &spec);
        self.editor = editor::editor_with_text(&new_text);
        // Re-sync diagnostics + dashboard from the rewritten buffer.
        self.recompute_diagnostics();
        self.status = format!("viz: {}", kind.as_str());
    }

    /// `:open [uid]` — fetch a single dashboard by uid. With no
    /// argument, retries the last-picked dashboard. The fetch is async;
    /// the result lands via `AppEvent::DashboardOpened`.
    fn cmd_open(&mut self, uid_arg: Option<&str>) {
        let uid = match uid_arg {
            Some(s) => s.trim_matches('"').to_string(),
            None => match self.last_picked_dashboard.as_deref() {
                Some(prev) => prev.to_string(),
                None => {
                    self.set_error(
                        ":open requires a dashboard uid (or use :dashboards first)".to_string(),
                    );
                    return;
                }
            },
        };
        if uid.is_empty() {
            self.set_error(":open: empty uid".to_string());
            return;
        }
        self.fetch_dashboard_by_uid(uid);
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

    /// `:time` / `:range` — inspect or change the active query window.
    ///
    /// Forms:
    ///   * `:time` — open the quick-select preset overlay
    ///     (3h / 6h / … / 90d, plus a "Custom…" row that opens a
    ///     two-month calendar picker).
    ///   * `:time reset` (or `default`) — restore `now-1h` / `now`.
    ///   * `:time <start>` — set start, keep end at `now`.
    ///   * `:time <start> <end>` — set both.
    ///
    /// Values are passed through verbatim, so any expression the
    /// Axiom API accepts works: relative (`now-15m`, `qr-now-7d`),
    /// absolute RFC3339 (`2024-05-01T00:00:00Z`), etc. When a
    /// dashboard is loaded, the new range is also written back into
    /// `loaded_dashboard.dashboard.time_window_{start,end}` and the
    /// dirty flag is set so `:dash save` persists it.
    fn cmd_time(&mut self, args: &[&str]) {
        if args.is_empty() {
            // Default the cursor to the preset that matches the current
            // start (e.g. `now-6h` highlights the `6h` row), falling
            // back to the first entry when nothing matches.
            // Compare against the normalised values so a stored
            // `qr-now-6h` / `qr-now` highlights the matching preset.
            let (cur_start, cur_end) = self.active_time_range();
            let cursor = TIME_PRESETS
                .iter()
                .position(|(_, d)| cur_start == format!("now-{d}") && cur_end == "now")
                .unwrap_or(0);
            self.time_picker = Some(TimePickerState::Presets { cursor });
            return;
        }
        let (new_start, new_end) = match args {
            ["reset"] | ["default"] => (
                DEFAULT_START.to_string(),
                DEFAULT_END.to_string(),
            ),
            [start] => (start.to_string(), DEFAULT_END.to_string()),
            [start, end] => (start.to_string(), end.to_string()),
            _ => {
                self.set_error(
                    ":time: usage — `:time`, `:time reset`, `:time <start>`, or `:time <start> <end>`"
                        .to_string(),
                );
                return;
            }
        };
        if new_start.trim().is_empty() || new_end.trim().is_empty() {
            self.set_error(":time: start/end must be non-empty".to_string());
            return;
        }
        self.set_time_range(new_start, new_end);
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

    /// `:trace` — report the trace id of the focused panel so the user
    /// can hand it off to support or paste it into Axiom's trace search.
    ///
    /// Resolution order:
    ///   1. In Grid view with a dashboard loaded, the focused tile's
    ///      per-fetch trace id (`tile_results[chart_id].trace_id`).
    ///   2. Otherwise the editor's last query trace (`last_trace_id`).
    ///
    /// The id ends up in `self.status`, which the status bar shows in
    /// full so it's easy to select with the mouse.
    fn cmd_trace(&mut self) {
        // Prefer the focused tile's trace when we're actually looking
        // at a panel; this is the whole point of the command.
        if self.view_mode == ViewMode::Grid
            && let Some(resource) = self.loaded_dashboard.as_ref()
            && let Some(chart) = resource.dashboard.charts.get(self.selected_chart_idx)
        {
            let chart_id = chart.base().id.clone();
            let label = chart
                .base()
                .name
                .clone()
                .unwrap_or_else(|| chart_id.clone());
            match self
                .tile_results
                .get(&chart_id)
                .and_then(|t| t.trace_id.clone())
            {
                Some(id) => self.status = format!("trace `{label}`: {id}"),
                None => {
                    self.status =
                        format!("no trace id for `{label}` yet (tile hasn't returned)")
                }
            }
            return;
        }
        match self.last_trace_id.as_deref() {
            Some(id) => self.status = format!("trace: {id}"),
            None => self.status = "no trace id available (run a query first)".to_string(),
        }
    }

    /// `:dashinfo` / `:di` — toggle the overlay summarising the loaded
    /// dashboard's charts. No-op (with status message) if no dashboard
    /// has been opened yet.
    fn cmd_dashinfo(&mut self) {
        if self.loaded_dashboard.is_none() {
            self.status = "no dashboard loaded; try :dashboards or :open <uid>".to_string();
            return;
        }
        self.dashinfo_visible = !self.dashinfo_visible;
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

    /// `:tile <sub> [args]` — mutate the selected tile.
    ///
    /// Sub-commands (all operate on the currently-selected tile):
    /// * `add <kind>` — insert a new tile of the given viz kind
    /// * `rm` — delete the selected tile (no confirm; that's the `d`
    ///   keyboard flow)
    /// * `mv <x> <y>` — move to absolute virtual-grid coordinates
    /// * `size <w> <h>` — resize to absolute w/h
    /// * `title <text>` — rename the selected tile
    fn cmd_tile(&mut self, args: &[&str]) {
        let Some(sub) = args.first().copied() else {
            self.set_error(":tile needs a sub-command (add, rm, mv, size, title)".to_string());
            return;
        };
        if self.loaded_dashboard.is_none() {
            self.set_error(":tile: no dashboard loaded".to_string());
            return;
        }
        match sub {
            "json" | "inspect" => {
                if let Some(json) = self.focused_chart_json() {
                    self.tile_inspect_json = Some(json);
                } else {
                    self.status = ":tile json: no tile selected".to_string();
                }
            }
            "add" => {
                let Some(kind_str) = args.get(1) else {
                    self.set_error(":tile add <kind>: kind required".to_string());
                    return;
                };
                let Some(kind) = crate::dashboard::VizKind::parse(kind_str) else {
                    self.set_error(format!(":tile add {kind_str}: unknown viz kind"));
                    return;
                };
                let name = args[2..].join(" ");
                let name = if name.is_empty() {
                    "new tile".to_string()
                } else {
                    name
                };
                let resource = self.loaded_dashboard.as_mut().unwrap();
                let id = tile_ops::insert_tile(
                    &mut resource.dashboard.charts,
                    &mut resource.dashboard.layout,
                    kind,
                    &name,
                );
                self.dashboard_dirty = true;
                self.selected_chart_idx = resource.dashboard.charts.len() - 1;
                self.status = format!("added {} tile {id}", kind.as_str());
            }
            "rm" => {
                let Some(id) = self.current_chart_id() else {
                    self.set_error(":tile rm: no tile selected".to_string());
                    return;
                };
                let resource = self.loaded_dashboard.as_mut().unwrap();
                match tile_ops::delete(
                    &mut resource.dashboard.charts,
                    &mut resource.dashboard.layout,
                    &id,
                ) {
                    Ok(()) => {
                        self.dashboard_dirty = true;
                        let n = resource.dashboard.charts.len();
                        if self.selected_chart_idx >= n {
                            self.selected_chart_idx = n.saturating_sub(1);
                        }
                        self.status = format!("deleted tile {id}");
                    }
                    Err(e) => self.set_error(format!(":tile rm: {e}")),
                }
            }
            "mv" => {
                let (Some(x_s), Some(y_s)) = (args.get(1), args.get(2)) else {
                    self.set_error(":tile mv <x> <y>: two integer args required".to_string());
                    return;
                };
                let (Ok(x), Ok(y)) = (x_s.parse::<u32>(), y_s.parse::<u32>()) else {
                    self.set_error(":tile mv: x and y must be non-negative integers".to_string());
                    return;
                };
                let Some(id) = self.current_chart_id() else {
                    self.set_error(":tile mv: no tile selected".to_string());
                    return;
                };
                // Compute delta from current position so the shared
                // collision-checking helper does the rest.
                let resource = self.loaded_dashboard.as_mut().unwrap();
                let cur = resource
                    .dashboard
                    .layout
                    .iter()
                    .find(|l| l.i == id)
                    .cloned();
                let (cx, cy) = cur
                    .as_ref()
                    .map(|l| (l.x as i32, l.y.unwrap_or(0) as i32))
                    .unwrap_or((0, 0));
                match tile_ops::translate(
                    &mut resource.dashboard.layout,
                    &id,
                    x as i32 - cx,
                    y as i32 - cy,
                ) {
                    Ok(()) => {
                        self.dashboard_dirty = true;
                        self.status = format!(":tile mv {x} {y} ok");
                    }
                    Err(e) => self.set_error(format!(":tile mv: {e}")),
                }
            }
            "size" => {
                let (Some(w_s), Some(h_s)) = (args.get(1), args.get(2)) else {
                    self.set_error(":tile size <w> <h>: two integer args required".to_string());
                    return;
                };
                let (Ok(w), Ok(h)) = (w_s.parse::<u32>(), h_s.parse::<u32>()) else {
                    self.set_error(":tile size: w and h must be positive integers".to_string());
                    return;
                };
                if w == 0 || h == 0 {
                    self.set_error(":tile size: w and h must be ≥1".to_string());
                    return;
                }
                let Some(id) = self.current_chart_id() else {
                    self.set_error(":tile size: no tile selected".to_string());
                    return;
                };
                let resource = self.loaded_dashboard.as_mut().unwrap();
                let cur = resource
                    .dashboard
                    .layout
                    .iter()
                    .find(|l| l.i == id)
                    .cloned();
                let (cw, ch) = cur
                    .as_ref()
                    .map(|l| (l.w as i32, l.h as i32))
                    .unwrap_or((6, 6));
                match tile_ops::resize(
                    &mut resource.dashboard.layout,
                    &id,
                    w as i32 - cw,
                    h as i32 - ch,
                ) {
                    Ok(()) => {
                        self.dashboard_dirty = true;
                        self.status = format!(":tile size {w} {h} ok");
                    }
                    Err(e) => self.set_error(format!(":tile size: {e}")),
                }
            }
            "title" => {
                let title = args[1..].join(" ");
                if title.is_empty() {
                    self.set_error(":tile title <text>: text required".to_string());
                    return;
                }
                let Some(id) = self.current_chart_id() else {
                    self.set_error(":tile title: no tile selected".to_string());
                    return;
                };
                let resource = self.loaded_dashboard.as_mut().unwrap();
                match tile_ops::set_title(&mut resource.dashboard.charts, &id, &title) {
                    Ok(()) => {
                        self.dashboard_dirty = true;
                        self.status = format!(":tile title `{title}`");
                    }
                    Err(e) => self.set_error(format!(":tile title: {e}")),
                }
            }
            other => {
                self.set_error(format!(
                    ":tile {other}: unknown sub-command (add, rm, mv, size, title)"
                ));
            }
        }
    }

    /// `:grid` — enter multi-tile grid view. Only meaningful when a
    /// dashboard is loaded; otherwise a status message explains why.
    pub fn cmd_grid(&mut self) {
        if self.loaded_dashboard.is_none() {
            self.status = ":grid: no dashboard loaded".to_string();
            return;
        }
        self.view_mode = ViewMode::Grid;
        self.focus = Pane::Dashboard;
        let n = self
            .loaded_dashboard
            .as_ref()
            .map(|r| r.dashboard.charts.len())
            .unwrap_or(0);
        if self.selected_chart_idx >= n {
            self.selected_chart_idx = 0;
        }
        self.reload_legend_label_tags();
    }

    /// `:solo` — return to single-tile view. Focus drops back to the
    /// editor so the user can type immediately.
    pub fn cmd_solo(&mut self) {
        self.view_mode = ViewMode::Solo;
        // Dashboard tile grid isn't rendered in Solo — redirect
        // focus to the Editor so the user isn't stranded on an
        // invisible pane. Legend stays addressable because the
        // side column is still drawn.
        if self.focus == Pane::Dashboard {
            self.focus = Pane::Editor;
        }
        // Switch back to the editor's cached tags so the legend
        // doesn't keep the last-focused tile's selection.
        self.reload_legend_label_tags();
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

    /// `:dash <sub> [args]` — dashboard CRUD against the server.
    ///
    /// Sub-commands:
    /// * `save`         — PUT current dashboard, version-checked
    /// * `save!`        — PUT with `overwrite=true` (skip version check)
    /// * `rm <uid>`     — DELETE a dashboard by uid
    ///
    /// `:dash save` without a loaded dashboard, or `:dash rm` without
    /// an arg, surfaces an error overlay instead of silently doing
    /// nothing.
    fn cmd_dash(&mut self, args: &[&str], bang: bool) {
        let sub = match args.first().copied() {
            Some(s) => s,
            None => {
                self.set_error(":dash needs a sub-command (save, save!, rm)".to_string());
                return;
            }
        };
        match sub {
            "save" => self.cmd_dash_save(bang),
            "rm" => self.cmd_dash_rm(args.get(1).copied()),
            "new" => self.cmd_dash_new(&args[1..]),
            other => {
                self.set_error(format!(
                    ":dash {other}: unknown sub-command (save, save!, rm, new)"
                ));
            }
        }
    }

    /// `:dash new from-buffer [name]` — POST a fresh dashboard built
    /// from the current MPL buffer. The buffer's `// @viz <kind>`
    /// pragma picks the chart type; the rest of the buffer becomes the
    /// chart's MPL query. Anything outside `from-buffer` is reserved
    /// for future variants (e.g. `from-template`).
    ///
    /// Mapping from `VizKind` back to the server's `Chart` enum is
    /// 1:1 for the kinds the server recognises (`TimeSeries`,
    /// `Heatmap`, `LogStream`, `Pie`, `Scatter`, `Table`, `TopK`,
    /// `Statistic`, `Note`). TUI-only kinds (`Bar`, `Area`,
    /// `MonitorList`, `Spacer`) get folded back to `TimeSeries` since
    /// the server has no equivalent — the chart's renderer-side
    /// flavour is the TUI's job, not the server's.
    fn cmd_dash_new(&mut self, args: &[&str]) {
        let source = args.first().copied();
        if source != Some("from-buffer") {
            self.set_error(
                ":dash new from-buffer [name]: only `from-buffer` is supported today".to_string(),
            );
            return;
        }
        let name = args[1..].join(" ");
        let name = if name.is_empty() {
            self.current_file
                .as_ref()
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string()
        } else {
            name
        };
        let mpl = self.query_text();
        let kind = self.viz_kind;
        let doc = build_dashboard_doc_from_buffer(&name, kind, &mpl);

        let Some((client, tx, _cache)) =
            self.fetch_prepare(Some(format!("creating dashboard `{name}`…")))
        else {
            return;
        };
        self.runtime.spawn(async move {
            let result = client.create_dashboard(&doc, None, None).await;
            // The handler doesn't know the uid yet (server assigns it),
            // so we pass a placeholder and let the result carry it.
            let _ = tx.send(AppEvent::DashboardSaved {
                uid: String::new(),
                result,
            });
        });
    }

    /// `:dash save` (and `:dash save!`). PUTs the in-memory dashboard
    /// back to the server. With `!`, the server's version check is
    /// skipped (`overwrite=true`); otherwise a stale-version response
    /// surfaces as a precise error.
    fn cmd_dash_save(&mut self, overwrite: bool) {
        // Clone everything we need up-front so we can release the
        // immutable borrow on `self.loaded_dashboard` before reaching
        // for `&mut self` via `ensure_client`.
        let (uid, doc, version) = match self.loaded_dashboard.as_ref() {
            Some(r) => (r.uid.clone(), r.dashboard.clone(), r.version),
            None => {
                self.set_error(":dash save: no dashboard loaded".to_string());
                return;
            }
        };
        let status_msg = if overwrite {
            format!("saving dashboard {uid} (overwrite)…")
        } else {
            format!("saving dashboard {uid}…")
        };
        let Some((client, tx, _cache)) = self.fetch_prepare(Some(status_msg)) else {
            return;
        };
        self.runtime.spawn(async move {
            let result = client
                .put_dashboard(&uid, &doc, version, overwrite, None)
                .await;
            let _ = tx.send(AppEvent::DashboardSaved {
                uid: uid.clone(),
                result,
            });
        });
    }

    /// `:dash rm <uid>` — delete a dashboard. Requires an explicit uid
    /// argument to keep the command from ever firing accidentally
    /// against the loaded dashboard.
    fn cmd_dash_rm(&mut self, uid_arg: Option<&str>) {
        let uid = match uid_arg {
            Some(s) => s.trim_matches('"').to_string(),
            None => {
                self.set_error(":dash rm <uid>: uid argument required".to_string());
                return;
            }
        };
        let Some((client, tx, _cache)) =
            self.fetch_prepare(Some(format!("deleting dashboard {uid}…")))
        else {
            return;
        };
        let uid_for_task = uid.clone();
        self.runtime.spawn(async move {
            let result = client.delete_dashboard(&uid_for_task).await;
            let _ = tx.send(AppEvent::DashboardDeleted {
                uid: uid_for_task,
                result,
            });
        });
    }

    /// `:dashboards` / `:db` — open the searchable dashboard picker.
    ///
    /// Snappy path: if the cache holds a prior listing, the picker
    /// opens instantly against the cached items and a background
    /// refresh is kicked off; the fresh list lands via
    /// `DashboardsRefreshed` and quietly replaces the picker contents
    /// while preserving the user's filter + selection.
    ///
    /// Cold path: with no cache, the foreground `DashboardsFetched`
    /// flow runs (sets `busy`, status "fetching dashboards…").
    fn cmd_dashboards(&mut self) {
        let cached = self.cache.read().unwrap().cached_dashboards();
        if let Some(items) = cached {
            // Snappy path: serve the cache, then refresh in the
            // background. The refresh is fire-and-forget and uses
            // the silent (background) prepare so it never trips
            // the busy gate.
            let n = items.len();
            self.dashboards.open(items);
            self.status = format!("{n} dashboard(s) (cached, refreshing…)");
            let Some((client, tx, cache)) = self.fetch_prepare(None) else {
                return;
            };
            self.runtime.spawn(async move {
                let result = client.list_dashboards().await;
                if let Ok(items) = &result {
                    let mut c = cache.write().unwrap();
                    c.replace_dashboards(items.clone());
                    if let Err(e) = c.save() {
                        eprintln!("metrics-tui: cache save failed: {e}");
                    }
                }
                let _ = tx.send(AppEvent::DashboardsRefreshed(result));
            });
            return;
        }
        let Some((client, tx, cache)) =
            self.fetch_prepare(Some("fetching dashboards…".to_string()))
        else {
            return;
        };
        self.runtime.spawn(async move {
            let result = client.list_dashboards().await;
            if let Ok(items) = &result {
                let mut c = cache.write().unwrap();
                c.replace_dashboards(items.clone());
                if let Err(e) = c.save() {
                    eprintln!("metrics-tui: cache save failed: {e}");
                }
            }
            let _ = tx.send(AppEvent::DashboardsFetched(result));
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

    fn cmd_quit(&mut self, force: bool) {
        if !force && self.is_dirty() {
            self.set_error("E37: No write since last change (add ! to override)".to_string());
            return;
        }
        self.persist_query();
        self.should_quit = true;
    }

    fn cmd_write(&mut self, path: Option<&str>) {
        match self.write_file(path.map(std::path::PathBuf::from)) {
            Ok(p) => self.status = format!("wrote {}", display_path(&p)),
            Err(e) => self.set_error(format!("write failed: {e}")),
        }
    }

    fn cmd_write_quit(&mut self, path: Option<&str>, _force: bool) {
        if let Err(e) = self.write_file(path.map(std::path::PathBuf::from)) {
            self.set_error(format!("write failed: {e}"));
            return;
        }
        self.persist_query();
        self.should_quit = true;
    }

    /// `:x` — write only when modified, then quit. Equivalent to `:wq` when
    /// dirty, or `:q` when clean.
    fn cmd_update_quit(&mut self, path: Option<&str>) {
        if (self.is_dirty() || path.is_some())
            && let Err(e) = self.write_file(path.map(std::path::PathBuf::from))
        {
            self.set_error(format!("write failed: {e}"));
            return;
        }
        self.persist_query();
        self.should_quit = true;
    }

    fn cmd_edit(&mut self, path: Option<&str>, force: bool) {
        // `:e!` with no path reloads the current file from disk.
        if path.is_none() {
            if !force {
                self.set_error("E32: No file name".to_string());
                return;
            }
            let Some(current) = self.current_file.clone() else {
                self.set_error("E32: No file name".to_string());
                return;
            };
            return self.do_open(current, force);
        }
        let path = std::path::PathBuf::from(path.unwrap());
        self.do_open(path, force);
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

fn state_from(payload: completions::CompletionPayload, selected: usize) -> CompletionState {
    let kind_label = completions::kind_label(&payload.kind);
    CompletionState {
        visible: true,
        items: payload.items,
        selected,
        replace_range_bytes: payload.replace_range,
        kind_label,
        kind: Some(payload.kind),
    }
}

/// Lossy display of a path for status messages — keeps the code free of
/// `path.display()` ceremony at every call site.
fn display_path(p: &std::path::Path) -> String {
    p.display().to_string()
}

/// Extract identifiers that appear immediately before a comparison operator
/// (`==`, `!=`, `<`, `>`, `<=`, `>=`) in `query`. Identifiers may be plain
/// (alphanumeric + `_` + `.`) or backtick-quoted. String literals are
/// skipped so `"a == b"` doesn't register. The result is deduped and order
/// is unspecified.
///
/// This is a deliberately lightweight scan, not an MPL parser: in `where`-
/// like positions the identifier immediately before a comparison is
/// always a tag name, so we don't need full grammar awareness to drive a
/// tag-value prefetcher.
fn referenced_tags(query: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let bytes = query.as_bytes();
    let len = bytes.len();
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut i = 0;
    while i < len {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < len && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < len {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if i < len {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'/' => {
                // Line comment.
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        if is_cmp_op_at(bytes, i)
            && let Some(name) = ident_before(bytes, i)
        {
            out.insert(name);
        }
        i += 1;
    }
    out.into_iter().collect()
}

fn is_cmp_op_at(bytes: &[u8], i: usize) -> bool {
    if i + 1 < bytes.len() {
        match (bytes[i], bytes[i + 1]) {
            (b'=', b'=') | (b'!', b'=') | (b'<', b'=') | (b'>', b'=') => return true,
            _ => {}
        }
    }
    // Single-char `<` / `>`. Avoid false positives on `<=` / `>=` (handled above)
    // and on the leading char of `<=` etc. We accept the char only when the next
    // char is not `=`.
    if i < bytes.len()
        && (bytes[i] == b'<' || bytes[i] == b'>')
        && bytes.get(i + 1).copied() != Some(b'=')
    {
        return true;
    }
    false
}

/// Returns the identifier ending at `pos` (exclusive), skipping leading
/// whitespace. Handles backtick-quoted names by unescaping the surrounding
/// backticks.
fn ident_before(bytes: &[u8], pos: usize) -> Option<String> {
    let mut j = pos;
    while j > 0 && bytes[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    if j == 0 {
        return None;
    }
    if bytes[j - 1] == b'`' {
        let end = j - 1;
        let mut k = end;
        while k > 0 && bytes[k - 1] != b'`' {
            k -= 1;
        }
        if k == 0 {
            return None;
        }
        // bytes[k - 1] == b'`' is the opening backtick.
        let inner = &bytes[k..end];
        if inner.is_empty() {
            return None;
        }
        return Some(String::from_utf8_lossy(inner).into_owned());
    }
    let end = j;
    while j > 0 && is_tag_byte(bytes[j - 1]) {
        j -= 1;
    }
    if j == end {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[j..end]).into_owned())
}

fn is_tag_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
}

fn editor_cursor_byte_offset(textarea: &TextArea<'_>) -> usize {
    let (row, char_col) = textarea.cursor();
    let lines = textarea.lines();
    let mut offset = 0;
    for line in lines.iter().take(row) {
        offset += line.len() + 1; // +1 for the synthetic '\n' join
    }
    if let Some(line) = lines.get(row) {
        let byte_col = line
            .char_indices()
            .nth(char_col)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        offset += byte_col;
    }
    offset
}

fn byte_offset_to_row_col(text: &str, byte_offset: usize) -> (usize, usize) {
    let clamped = byte_offset.min(text.len());
    let prefix = &text[..clamped];
    let row = prefix.bytes().filter(|&b| b == b'\n').count();
    let col = match prefix.rfind('\n') {
        Some(nl) => prefix[nl + 1..].chars().count(),
        None => prefix.chars().count(),
    };
    (row, col)
}

/// Resolve the edge route for `dataset`, refreshing the cache once on miss.
async fn resolve_route(
    cache: &Arc<RwLock<Cache>>,
    client: &AxiomClient,
    dataset: &str,
) -> anyhow::Result<EdgeRoute> {
    if let Some(r) = cache.read().unwrap().edge_route_for(dataset) {
        return Ok(r);
    }
    let datasets = client
        .list_datasets()
        .await
        .map_err(|e| e.context("refreshing dataset list to resolve edge URL"))?;
    {
        let mut c = cache.write().unwrap();
        c.replace_datasets(datasets);
        if let Err(e) = c.save() {
            eprintln!("metrics-tui: cache save failed: {e}");
        }
    }
    cache
        .read()
        .unwrap()
        .edge_route_for(dataset)
        .ok_or_else(|| anyhow::anyhow!("dataset `{dataset}` not found in this deployment"))
}

/// Normalise a time-range string before sending it to the metrics
/// query endpoint. The Axiom dashboard schema stores relative
/// expressions with a `qr-` prefix (e.g. `qr-now-7d`, `qr-now`) for
/// the web UI's range picker, but `POST /v1/query/_mpl` rejects that
/// prefix with `invalid field: "qr"`. Stripping it makes
/// `qr-now-7d` ≡ `now-7d` and `qr-now` ≡ `now`, which is what the
/// API actually accepts.
fn normalize_time_expr(s: &str) -> String {
    s.strip_prefix("qr-").unwrap_or(s).to_string()
}

/// Parse a date out of the configured time-range string when it's an
/// RFC3339 timestamp (e.g. `2024-05-01T00:00:00Z` or just `2024-05-01`).
/// Returns `None` for relative expressions (`now-1h`, `qr-now-7d`), in
/// which case the calendar picker keeps its seeded default.
fn parse_iso_date(s: &str) -> Option<time::Date> {
    // Try RFC3339 first; fall back to bare `YYYY-MM-DD`.
    if let Ok(odt) = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
    {
        return Some(odt.date());
    }
    let ymd =
        time::format_description::parse("[year]-[month]-[day]").ok()?;
    time::Date::parse(s, &ymd).ok()
}

async fn run_query_task(
    cache: &Arc<RwLock<Cache>>,
    client: &AxiomClient,
    dataset: &str,
    mpl: &str,
    start: &str,
    end: &str,
    params: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<MetricsQueryResponse> {
    let route = resolve_route(cache, client, dataset).await?;
    client
        .query_mpl(
            &route.url,
            route.deployment.as_deref(),
            mpl,
            start,
            end,
            params,
        )
        .await
}

/// Build a `Diagnostic` for a pragma parse failure at `line_idx`.
/// Column points at column 1 of that line; length spans the line. This
/// matches how the engine reports its own line-level diagnostics, so the
/// status bar treatment is uniform.
fn pragma_diagnostic(text: &str, line_idx: usize, err: &viz::PragmaError) -> mpl::Diagnostic {
    // Byte offset of the start of `line_idx`.
    let byte_offset = text
        .split_inclusive('\n')
        .take(line_idx)
        .map(|s| s.len())
        .sum::<usize>();
    let line_len = text.lines().nth(line_idx).map(str::len).unwrap_or(0);
    mpl::Diagnostic {
        severity: mpl::Severity::Warning,
        message: err.to_string(),
        help: Some(
            "valid kinds: line, bar, area, scatter, statistic, top_list, pie, heatmap, \
             table, log_stream, monitor_list, note, spacer"
                .to_string(),
        ),
        byte_offset,
        byte_length: line_len,
        line: line_idx + 1,
        column: 1,
        actions: Vec::new(),
    }
}

/// Ordered list of viz kinds shown in the add-tile picker. Mirrors
/// [`VizKind::is_implemented`] but in a stable order that's nicer to
/// look at than enum-declaration order.
pub(crate) fn add_pick_kinds() -> &'static [crate::dashboard::VizKind] {
    use crate::dashboard::VizKind;
    &[
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
}

/// Cardinal directions for spatial navigation in the dashboard grid.
/// Decoupled from key codes so the navigator can be unit-tested
/// without a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpatialDir {
    Left,
    Right,
    Up,
    Down,
}

/// Pick the chart whose centroid is nearest in `dir` from the
/// currently-selected chart. Returns `Some(idx)` when a candidate
/// exists, `None` when nothing lies in that direction (caller falls
/// back to row-major cycling).
///
/// Layout items live on a 12-column grid (`x ∈ 0..=11`); `y` is
/// nullable so we treat missing values as 0 for distance purposes.
/// Charts without a matching `LayoutItem` get a phantom (0, 0, 1, 1).
/// Ties broken by Manhattan distance from the source centroid.
pub(crate) fn pick_next_chart_in_direction(
    layout: &[crate::axiom::LayoutItem],
    charts: &[crate::axiom::Chart],
    selected: usize,
    dir: SpatialDir,
) -> Option<usize> {
    fn centroid(layout: &[crate::axiom::LayoutItem], chart_id: &str) -> (f32, f32) {
        match layout.iter().find(|l| l.i == chart_id) {
            Some(l) => {
                let x = l.x as f32 + l.w as f32 / 2.0;
                let y = l.y.unwrap_or(0) as f32 + l.h as f32 / 2.0;
                (x, y)
            }
            None => (0.0, 0.0),
        }
    }
    let src_id = charts.get(selected)?.base().id.clone();
    let (sx, sy) = centroid(layout, &src_id);
    let mut best: Option<(usize, f32)> = None;
    for (i, c) in charts.iter().enumerate() {
        if i == selected {
            continue;
        }
        let (cx, cy) = centroid(layout, &c.base().id);
        // Must actually lie in the requested direction.
        let in_dir = match dir {
            SpatialDir::Right => cx > sx,
            SpatialDir::Left => cx < sx,
            SpatialDir::Down => cy > sy,
            SpatialDir::Up => cy < sy,
        };
        if !in_dir {
            continue;
        }
        // Prefer matches on the perpendicular axis (smallest cross-axis
        // distance), then nearest along the chosen axis.
        let (primary, cross) = match dir {
            SpatialDir::Right | SpatialDir::Left => ((cx - sx).abs(), (cy - sy).abs()),
            SpatialDir::Up | SpatialDir::Down => ((cy - sy).abs(), (cx - sx).abs()),
        };
        let score = cross * 2.0 + primary; // weight cross-axis 2×
        if best.is_none() || score < best.unwrap().1 {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
}

/// Maximum column index in the server's virtual grid. The schema
/// constrains `x` to 0..=11 and chart widths to fit — i.e. a 12-col
/// grid. Resize/move clamp against this.
pub(crate) const GRID_COLS: u32 = 12;

/// Layout-mutating helpers operate on a `(charts, layout)` pair drawn
/// from `loaded_dashboard.dashboard`. Pure functions — no `App` borrow
/// — so each can be unit-tested in isolation and reused by the
/// keyboard sub-modes + the `:tile` Ex-commands.
pub(crate) mod tile_ops {
    use super::GRID_COLS;
    use crate::axiom::{Chart, ChartBase, LayoutItem};

    /// `true` if `candidate` overlaps any layout entry whose `i` is
    /// **not** `ignore_id`. Two rectangles overlap when they share at
    /// least one cell in both axes.
    pub fn overlaps_any(candidate: &LayoutItem, layout: &[LayoutItem], ignore_id: &str) -> bool {
        let (ax1, ay1) = (candidate.x, candidate.y.unwrap_or(0));
        let (ax2, ay2) = (ax1 + candidate.w, ay1 + candidate.h);
        layout.iter().any(|l| {
            if l.i == ignore_id {
                return false;
            }
            let (bx1, by1) = (l.x, l.y.unwrap_or(0));
            let (bx2, by2) = (bx1 + l.w, by1 + l.h);
            ax1 < bx2 && ax2 > bx1 && ay1 < by2 && ay2 > by1
        })
    }

    /// Translate the tile `chart_id` by `(dx, dy)` virtual-grid cells.
    /// Returns `Err(msg)` when the move would push the tile off the
    /// 12-col grid or overlap another tile; the layout is unchanged in
    /// that case.
    pub fn translate(
        layout: &mut [LayoutItem],
        chart_id: &str,
        dx: i32,
        dy: i32,
    ) -> Result<(), &'static str> {
        let li_idx = layout
            .iter()
            .position(|l| l.i == chart_id)
            .ok_or("tile has no layout entry")?;
        let mut new_li = layout[li_idx].clone();
        let cur_x = new_li.x as i32;
        let cur_y = new_li.y.unwrap_or(0) as i32;
        let nx = cur_x + dx;
        let ny = cur_y + dy;
        if nx < 0 || ny < 0 {
            return Err("edge of grid");
        }
        if (nx as u32) + new_li.w > GRID_COLS {
            return Err("edge of grid");
        }
        new_li.x = nx as u32;
        new_li.y = Some(ny as u32);
        if overlaps_any(&new_li, layout, chart_id) {
            return Err("would overlap another tile");
        }
        layout[li_idx] = new_li;
        Ok(())
    }

    /// Grow/shrink the tile's `w`/`h` by `(dw, dh)`. Clamped to a
    /// 1-cell minimum and to `GRID_COLS` total width. Overlap rejected.
    pub fn resize(
        layout: &mut [LayoutItem],
        chart_id: &str,
        dw: i32,
        dh: i32,
    ) -> Result<(), &'static str> {
        let li_idx = layout
            .iter()
            .position(|l| l.i == chart_id)
            .ok_or("tile has no layout entry")?;
        let mut new_li = layout[li_idx].clone();
        let nw = new_li.w as i32 + dw;
        let nh = new_li.h as i32 + dh;
        if nw < 1 || nh < 1 {
            return Err("minimum size 1x1");
        }
        if new_li.x + (nw as u32) > GRID_COLS {
            return Err("exceeds 12-col grid");
        }
        new_li.w = nw as u32;
        new_li.h = nh as u32;
        if overlaps_any(&new_li, layout, chart_id) {
            return Err("would overlap another tile");
        }
        layout[li_idx] = new_li;
        Ok(())
    }

    /// Delete the tile (chart + matching layout entry). Returns `Err`
    /// if no chart with that id exists.
    pub fn delete(
        charts: &mut Vec<Chart>,
        layout: &mut Vec<LayoutItem>,
        chart_id: &str,
    ) -> Result<(), &'static str> {
        let cidx = charts
            .iter()
            .position(|c| c.base().id == chart_id)
            .ok_or("unknown chart id")?;
        charts.remove(cidx);
        layout.retain(|l| l.i != chart_id);
        Ok(())
    }

    /// Find the first free slot for a new `w × h` tile, scanning
    /// row-major across the virtual grid. Always returns *some*
    /// position: when the grid is packed full the new tile lands
    /// directly below the lowest existing tile.
    pub fn first_free_slot(layout: &[LayoutItem], w: u32, h: u32) -> (u32, u32) {
        let max_y = layout
            .iter()
            .map(|l| l.y.unwrap_or(0) + l.h)
            .max()
            .unwrap_or(0);
        for y in 0..=max_y {
            for x in 0..=GRID_COLS.saturating_sub(w) {
                let candidate = LayoutItem {
                    i: String::new(),
                    x,
                    y: Some(y),
                    w,
                    h,
                    extras: Default::default(),
                };
                if !overlaps_any(&candidate, layout, "") {
                    return (x, y);
                }
            }
        }
        (0, max_y)
    }

    /// Insert a new tile with the given chart kind + name. The id is
    /// generated by suffixing the next free numeric tail to the
    /// caller-supplied prefix (defaults to `c`). Returns the new id.
    pub fn insert_tile(
        charts: &mut Vec<Chart>,
        layout: &mut Vec<LayoutItem>,
        kind: crate::dashboard::VizKind,
        name: &str,
    ) -> String {
        // Generate a chart id that doesn't collide.
        let used: std::collections::HashSet<&str> =
            charts.iter().map(|c| c.base().id.as_str()).collect();
        let mut n = charts.len();
        let id = loop {
            let candidate = format!("c{n}");
            if !used.contains(candidate.as_str()) {
                break candidate;
            }
            n += 1;
        };
        let (w, h) = (6, 6);
        let (x, y) = first_free_slot(layout, w, h);
        let base = ChartBase {
            id: id.clone(),
            name: Some(name.to_string()),
            query: Some(serde_json::json!({ "mpl": "" })),
            extras: Default::default(),
        };
        let chart = match kind {
            crate::dashboard::VizKind::Line
            | crate::dashboard::VizKind::Bar
            | crate::dashboard::VizKind::Area => Chart::TimeSeries(base),
            crate::dashboard::VizKind::Scatter => Chart::Scatter(base),
            crate::dashboard::VizKind::Pie => Chart::Pie(base),
            crate::dashboard::VizKind::Heatmap => Chart::Heatmap(base),
            crate::dashboard::VizKind::Table => Chart::Table(base),
            crate::dashboard::VizKind::TopList => Chart::TopK(base),
            crate::dashboard::VizKind::Statistic => Chart::Statistic(base),
            crate::dashboard::VizKind::LogStream => Chart::LogStream(base),
            crate::dashboard::VizKind::Note => Chart::Note(base),
            crate::dashboard::VizKind::MonitorList | crate::dashboard::VizKind::Spacer => {
                Chart::TimeSeries(base)
            }
        };
        charts.push(chart);
        layout.push(LayoutItem {
            i: id.clone(),
            x,
            y: Some(y),
            w,
            h,
            extras: Default::default(),
        });
        id
    }

    /// Rename the chart's `name` field. Returns `Err` for unknown id.
    pub fn set_title(
        charts: &mut [Chart],
        chart_id: &str,
        title: &str,
    ) -> Result<(), &'static str> {
        let chart = charts
            .iter_mut()
            .find(|c| c.base().id == chart_id)
            .ok_or("unknown chart id")?;
        // Mutating the inner ChartBase requires going through the enum.
        let base = match chart {
            Chart::TimeSeries(b)
            | Chart::Heatmap(b)
            | Chart::LogStream(b)
            | Chart::Pie(b)
            | Chart::Scatter(b)
            | Chart::Table(b)
            | Chart::TopK(b)
            | Chart::Statistic(b)
            | Chart::Note(b) => b,
        };
        base.name = Some(title.to_string());
        Ok(())
    }
}

/// Build a server-shaped `DashboardDocument` from a single MPL buffer.
/// Used by `:dash new from-buffer` to POST a one-chart dashboard.
///
/// `kind` picks the chart variant on the wire; for TUI-only viz kinds
/// (`Bar`, `Area`, `MonitorList`, `Spacer`) we fold back to
/// `TimeSeries` because the server has no equivalent.
pub fn build_dashboard_doc_from_buffer(
    name: &str,
    kind: VizKind,
    mpl: &str,
) -> crate::axiom::DashboardDocument {
    use crate::axiom::{Chart, ChartBase, DashboardDocument, LayoutItem};
    use serde_json::{Map, json};

    let chart_id = "c1".to_string();
    let query = json!({ "mpl": mpl });
    let base = ChartBase {
        id: chart_id.clone(),
        name: Some(name.to_string()),
        query: Some(query),
        extras: Default::default(),
    };
    let chart = match kind {
        VizKind::Line | VizKind::Bar | VizKind::Area => Chart::TimeSeries(base),
        VizKind::Scatter => Chart::Scatter(base),
        VizKind::Pie => Chart::Pie(base),
        VizKind::Heatmap => Chart::Heatmap(base),
        VizKind::Table => Chart::Table(base),
        VizKind::TopList => Chart::TopK(base),
        VizKind::Statistic => Chart::Statistic(base),
        VizKind::LogStream => Chart::LogStream(base),
        VizKind::Note => Chart::Note(base),
        // TUI-only — fall back to TimeSeries.
        VizKind::MonitorList | VizKind::Spacer => Chart::TimeSeries(base),
    };
    // Server requires owner, refreshTime, schemaVersion, timeWindow*
    // to be present. We don't model those internally yet, so stash
    // them in `extras` to satisfy the schema.
    let mut extras = Map::new();
    extras.insert("owner".to_string(), json!("X-AXIOM-EVERYONE"));
    extras.insert("refreshTime".to_string(), json!(60));
    extras.insert("schemaVersion".to_string(), json!(2));
    DashboardDocument {
        name: Some(name.to_string()),
        description: None,
        charts: vec![chart],
        layout: vec![LayoutItem {
            i: chart_id,
            x: 0,
            y: Some(0),
            w: 12,
            h: 6,
            extras: Default::default(),
        }],
        time_window_start: Some("qr-now-1h".to_string()),
        time_window_end: Some("qr-now".to_string()),
        extras,
    }
}

/// Build the single-tile dashboard that wraps the initial buffer text.
/// On a fresh app this is the demo query; on file-load it's the file's
fn default_cache() -> Cache {
    // We don't yet have a base URL — `Cache::load` only needs a fallback for
    // datasets that lack `edgeDeployment`. Use a placeholder; it gets replaced
    // when the first real query reaches `route_for`.
    Cache::load(String::new())
}

/// Convert an Axiom MPL response into the internal `Series` model used by the chart.
/// Validate that `value` parses as the engine's `param_value` rule. This
/// is what `mpl_lang::query::ProvidedParams::parse_and_validate` does
/// internally per provided pair; we surface it eagerly so `:p host=db-01`
/// (a bare ident with a `-`) is rejected at set-time rather than at
/// query-time. Returns a short message; on success the value is left to
/// the server to typecheck against the declared param's type.
fn validate_param_value(value: &str) -> Result<(), String> {
    use mpl_lang::{MPLParser, Rule};
    use pest::Parser as _;
    let mut pairs = MPLParser::parse(Rule::param_value, value).map_err(|e| {
        // Pest's full error is multi-line and noisy in a status bar;
        // extract the most useful first line.
        e.to_string()
            .lines()
            .next()
            .unwrap_or("parse error")
            .to_string()
    })?;
    // `parse` doesn't enforce consuming the entire input — it'll happily
    // accept `db-01` by matching just `db` as an ident. Reject anything
    // with trailing garbage so e.g. `host=db-01` is caught at set-time.
    let pair = pairs.next().ok_or_else(|| "empty parse".to_string())?;
    let end = pair.as_span().end();
    if end != value.len() {
        return Err(format!(
            "trailing garbage after `{}`",
            &value[..end].trim_end()
        ));
    }
    Ok(())
}

fn response_to_series(resp: &MetricsQueryResponse) -> Vec<Series> {
    resp.series
        .iter()
        .enumerate()
        .map(|(i, s)| metrics_series_to_series(s, i))
        .collect()
}

fn metrics_series_to_series(s: &MetricsSeries, palette_index: usize) -> Series {
    let res = s.resolution.max(1) as i64;
    let points: Vec<(f64, f64)> = s
        .data
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            v.map(|y| {
                let x = (s.start + (i as i64) * res) as f64;
                (x, y)
            })
        })
        .collect();

    let mut tag_pairs: Vec<(String, String)> =
        s.tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    tag_pairs.sort_by(|a, b| a.0.cmp(&b.0));

    Series {
        name: format_series_name(&s.metric, &tag_pairs),
        tags: tag_pairs,
        points,
        color: color_for(palette_index),
    }
}

fn format_series_name(metric: &str, tags: &[(String, String)]) -> String {
    // Prefer a short identifying tag set (room/host/service/device); fall back to all tags.
    const PREFERRED: &[&str] = &["room", "host", "service.name", "device", "endpoint"];
    let mut chosen: Vec<String> = PREFERRED
        .iter()
        .filter_map(|k| tags.iter().find(|(t, _)| t == k).map(|(_, v)| v.clone()))
        .collect();
    if chosen.is_empty() {
        chosen = tags.iter().map(|(k, v)| format!("{k}={v}")).collect();
    }
    if chosen.is_empty() {
        metric.to_string()
    } else {
        format!("{metric} {{{}}}", chosen.join(","))
    }
}

fn demo_series() -> Vec<Series> {
    let sin_points: Vec<(f64, f64)> = (0..100)
        .map(|i| {
            let x = i as f64 * 0.1;
            (x, x.sin())
        })
        .collect();
    let cos_points: Vec<(f64, f64)> = (0..100)
        .map(|i| {
            let x = i as f64 * 0.1;
            (x, (x * 0.5).cos())
        })
        .collect();

    vec![
        Series {
            name: "sin(x)".to_string(),
            tags: vec![],
            points: sin_points,
            color: color_for(0),
        },
        Series {
            name: "cos(x/2)".to_string(),
            tags: vec![],
            points: cos_points,
            color: color_for(1),
        },
    ]
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
