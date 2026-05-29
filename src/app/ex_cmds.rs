//! Ex-command (`:foo`) implementation. Every `cmd_*` method here is
//! reached via [`App::execute_command`], which parses the typed
//! command line and dispatches. The dispatch lives in the same file
//! because it's a flat table — splitting it would force every entry
//! to be public.

use crate::dashboard::VizKind;
use crate::settings::SettingsStore;

use super::*;

/// Sub-commands for `:dash`, in display order. Shared with
/// `cmdline_complete` so the completion menu can't drift away from the
/// dispatch table here.
pub(crate) const DASH_SUBS: &[&str] = &["ls", "new", "rm"];

/// Sub-commands for `:tile`, in display order. Shared with
/// `cmdline_complete` for the same reason as `DASH_SUBS`.
pub(crate) const TILE_SUBS: &[&str] = &[
    "add", "cut", "inspect", "json", "mv", "open", "paste", "rm", "size", "title", "undo", "yank",
];

/// Sub-commands for `:trace`, in display order. Shared with
/// `cmdline_complete` for the same reason as `DASH_SUBS` /
/// `TILE_SUBS`. Bare `:trace` (no sub) is intentionally absent —
/// the completer never has to suggest "" — but the dispatcher
/// still handles it as the legacy trace-id reporter.
pub(crate) const TRACE_SUBS: &[&str] = &["get", "set", "unset"];

/// Known keys accepted by `:trace set` / `:trace unset`. Defined
/// once next to the dispatch so the completer's value-slot menu
/// (`dataset=` / `deployment=`) can't drift out of sync with the
/// strings the dispatcher actually accepts.
pub(crate) const TRACE_KEYS: &[&str] = &["dataset", "deployment"];

/// Single source of truth for which settings field each `:trace`
/// key maps to. Keeps `cmd_trace_set` / `cmd_trace_unset` free of
/// stringly-typed `match` arms scattered across two call sites.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum TraceKey {
    Dataset,
    Deployment,
}

impl TraceKey {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "dataset" => Some(Self::Dataset),
            "deployment" => Some(Self::Deployment),
            _ => None,
        }
    }

    fn set(self, store: &mut SettingsStore, value: Option<String>) {
        match self {
            Self::Dataset => store.set_trace_dataset(value),
            Self::Deployment => store.set_trace_deployment(value),
        }
    }
}

/// Parse `args[1]` / `args[2]` as `u32`. Used by `:tile mv` and `:tile size`.
/// `nonzero=true` rejects zero values (size needs ≥1). Returns the
/// already-formatted error string so callers can pass it straight to
/// `set_error`.
/// Parse `args[1]` as an optional decimal count, defaulting to 1.
/// Anything that doesn't parse as a positive integer becomes 1 —
/// matches vim's tolerance for spurious args.
fn parse_optional_count(arg: Option<&str>) -> usize {
    arg.and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(1)
}

fn parse_two_u32(
    args: &[&str],
    sub: &str,
    a: &str,
    b: &str,
    nonzero: bool,
) -> Result<(u32, u32), String> {
    let (Some(av), Some(bv)) = (args.get(1), args.get(2)) else {
        return Err(format!(
            ":tile {sub} <{a}> <{b}>: two integer args required"
        ));
    };
    let kind_hint = if nonzero { "positive" } else { "non-negative" };
    let (Ok(x), Ok(y)) = (av.parse::<u32>(), bv.parse::<u32>()) else {
        return Err(format!(
            ":tile {sub}: {a} and {b} must be {kind_hint} integers"
        ));
    };
    if nonzero && (x == 0 || y == 0) {
        return Err(format!(":tile {sub}: {a} and {b} must be ≥1"));
    }
    Ok((x, y))
}

impl App {
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
            "w" | "write" => self.cmd_write(args.first().copied(), bang),
            "wq" => self.cmd_write_quit(args.first().copied(), bang),
            "x" => self.cmd_update_quit(args.first().copied(), bang),
            "e" | "edit" => self.cmd_edit(args.first().copied(), bang),
            "r" | "run" => self.cmd_run(args.first().copied()),
            "ds" | "datasets" => self.fetch_datasets(),
            "m" | "metrics" => self.fetch_metrics_for_current_query(),
            "refresh" => {
                // Re-fetch the dataset list (discovery layer 1). Metric
                // discovery refreshes lazily per query; this does not
                // re-run the current query.
                self.fetch_datasets();
            }
            "help" | "h" => self.open_help(),
            "ax" | "axiom" => self.cmd_axiom_open(),
            "viz" => self.cmd_viz(args.first().copied()),
            "apl" => self.cmd_lang(crate::dashboard::Lang::Apl),
            "mpl" => self.cmd_lang(crate::dashboard::Lang::Mpl),
            "open" => self.cmd_open(args.first().copied()),
            "trace" => self.cmd_trace(&args),
            "span" => self.cmd_span(&args),
            "time" => self.cmd_time(&args),
            "dashinfo" | "di" => self.cmd_dashinfo(),
            "history" | "his" => self.cmd_history(),
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
            let n = self.params.cli.len();
            self.params.cli.clear();
            self.status = format!("cleared {n} param(s)");
            return;
        }
        let rest = rest.trim();
        if rest.is_empty() {
            if self.params.cli.is_empty() {
                self.status = "no params set".to_string();
            } else {
                let s = self
                    .params
                    .cli
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
            if self.params.cli.remove(name).is_some() {
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
        self.params.cli.insert(name.to_string(), value.to_string());
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
        // Dataset is best-effort: the explorer just needs `apl` set;
        // `metricsDataset` selects the right tab.
        let dataset = mpl::extract_dataset_metric(&mpl).map(|p| p.0).ok();
        let override_name = self.deployment_override.clone();
        let (deployment_url, org_id) = match Config::load().and_then(|cfg| {
            cfg.select(override_name.as_deref())
                .map(|(_, dep)| (dep.url.clone(), dep.org_id.clone()))
        }) {
            Ok(v) => v,
            Err(e) => return self.set_error(format!("axiom config: {e}")),
        };
        if org_id.is_empty() {
            return self.set_error("axiom config missing org_id".to_string());
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
    pub(super) fn cmd_viz(&mut self, kind_arg: Option<&str>) {
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

    /// `:apl` / `:mpl` — flip the current edit context's language.
    ///
    /// In dashboard mode this rewrites the focused tile's query-object
    /// key (`mpl` ↔ `apl`), stamps the `axLang` sidecar so the next
    /// reload classifies deterministically, and marks the dashboard
    /// dirty. In standalone MPL-buffer mode it flips
    /// [`App::buffer_lang`] for the duration of the session.
    ///
    /// Text is **not** auto-converted — the user is expected to
    /// rewrite the query when changing dialects. The next `:r` will
    /// dispatch the buffer text to the language's endpoint and
    /// surface any server-side rejection in the usual place.
    pub(super) fn cmd_lang(&mut self, lang: crate::dashboard::Lang) {
        let current = self.active_lang();
        if current == lang && self.buffer_mode != BufferMode::Dashboard {
            self.status = format!("lang: {} (unchanged)", lang.label());
            return;
        }
        if self.buffer_mode != BufferMode::Dashboard {
            self.buffer_lang = lang;
            // Re-run the language-gated diagnostics pass so any
            // stale errors from the previous dialect (e.g. an
            // unsolicited "MPL syntax error" left over on what is
            // now an APL buffer) clear immediately instead of
            // lingering until the next buffer-mutating keystroke.
            self.recompute_diagnostics();
            self.status = format!("lang: {} (buffer)", lang.label());
            return;
        }
        // Dashboard mode: rewrite the focused tile's query-object key.
        // Persist any pending editor edits first so we don't lose them
        // when we rewrite the query object underneath the buffer.
        self.sync_buffer_to_focused_tile();
        let Some(resource) = self.loaded_dashboard.as_mut() else {
            self.buffer_lang = lang;
            self.recompute_diagnostics();
            self.status = format!("lang: {} (buffer)", lang.label());
            return;
        };
        let Some(chart) = resource.dashboard.charts.get_mut(self.selected_chart_idx) else {
            self.set_error(":".to_string() + lang.ex_command() + ": no tile selected");
            return;
        };
        // Note / Spacer tiles have no query to flip.
        let Some(base) = chart.base_mut() else {
            self.set_error(format!(
                ":{}: tile has no chart base (Unknown variant)",
                lang.ex_command()
            ));
            return;
        };
        // Move text between keys. Both keys may be missing (newly
        // created tile that hasn't had text typed in yet) — that's
        // fine, we just stamp the sidecar so future edits land in the
        // right key.
        let (write_key, drop_key) = match lang {
            crate::dashboard::Lang::Mpl => ("mpl", "apl"),
            crate::dashboard::Lang::Apl => ("apl", "mpl"),
        };
        let mut query = base.query.take().unwrap_or_else(|| serde_json::json!({}));
        if !query.is_object() {
            query = serde_json::json!({});
        }
        if let Some(obj) = query.as_object_mut() {
            let text = obj
                .remove(drop_key)
                .or_else(|| obj.remove(write_key))
                .unwrap_or_else(|| serde_json::Value::String(String::new()));
            obj.insert(write_key.to_string(), text);
        }
        base.query = Some(query);
        base.extras.insert(
            crate::dashboard::LANG_SIDECAR_KEY.to_string(),
            serde_json::Value::String(lang.as_sidecar().to_string()),
        );
        self.dashboard_dirty = true;
        // Re-seed the editor so the buffer reflects the new key
        // (text is the same; only the underlying storage flipped).
        // Diagnostics get re-evaluated as part of seeding.
        self.seed_editor_from_focused_tile();
        self.status = format!("lang: {} (tile)", lang.label());
    }

    /// `:open [uid]` — fetch a single dashboard by uid. With no
    /// argument, retries the last-picked dashboard. The fetch is async;
    /// the result lands via `AppEvent::DashboardOpened`.
    fn cmd_open(&mut self, uid_arg: Option<&str>) {
        // `:open` from inside the trace view tears down the
        // trace before kicking off the dashboard fetch — the
        // dashboard adopt path flips `view_mode` to Grid/Solo
        // which would clobber the trace context anyway.
        if self.view_mode == ViewMode::Trace {
            self.trace_view = None;
            self.pending_trace_fetch = None;
            self.view_mode = ViewMode::Solo;
            if matches!(self.focus, Pane::TraceTree) {
                self.focus = Pane::Editor;
            }
        }
        let uid = match uid_arg {
            Some(s) => s.trim_matches('"').to_string(),
            None => match self.last_picked_dashboard.as_deref() {
                Some(prev) => prev.to_string(),
                None => {
                    self.set_error(
                        ":open requires a dashboard uid (or use :dash ls first)".to_string(),
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

    /// `:time` — inspect or change the active query window.
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
            self.time.picker = Some(TimePickerState::Presets { cursor });
            return;
        }
        let (new_start, new_end) = match args {
            ["reset"] | ["default"] => (DEFAULT_START.to_string(), DEFAULT_END.to_string()),
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

    /// `:span json` — open the inspect overlay on the currently
    /// selected trace span. Re-uses the existing `tile_inspect_json`
    /// overlay slot (the global key dispatcher dismisses it on any
    /// key) so we don't need a second overlay widget.
    ///
    /// Only legal while a trace is loaded; otherwise reports a
    /// clean error. The JSON shape matches the `y` keymap exactly
    /// — typed core + attribute/resource maps + events list, via
    /// the [`crate::trace::SpanJson`] projection.
    fn cmd_span(&mut self, args: &[&str]) {
        let Some(sub) = args.first().copied() else {
            self.set_error(":span needs a sub-command (json)".to_string());
            return;
        };
        match sub {
            "json" | "inspect" => {
                let Some(view) = self.trace_view.as_ref() else {
                    self.set_error(":span json: no trace loaded".to_string());
                    return;
                };
                if view.model.tree.is_empty() {
                    self.set_error(":span json: empty trace".to_string());
                    return;
                }
                let cursor = view.cursor.min(view.model.tree.len() - 1);
                let span_idx = view.model.tree[cursor].span_idx;
                let span = &view.model.spans[span_idx];
                let trace_id = view.model.trace_id.clone();
                match serde_json::to_string_pretty(&crate::trace::SpanJson::from_span(
                    &trace_id, span,
                )) {
                    Ok(s) => {
                        let label: String = span.span_id.chars().take(8).collect();
                        self.tile_inspect_json = Some(s);
                        self.status = format!("inspecting span {label}");
                    }
                    Err(e) => self.set_error(format!(":span json: serialise failed: {e}")),
                }
            }
            other => self.set_error(format!(":span {other}: unknown sub-command (json)")),
        }
    }

    /// `:trace` dispatcher.
    ///
    /// Three independent surfaces share the command name:
    ///
    /// - **Bare `:trace`** — report the trace id of the focused
    ///   panel (or editor's last query) on the status bar. This is
    ///   the historical behavior, preserved verbatim so support
    ///   workflows that rely on it don't regress.
    /// - **`:trace set KEY=VALUE…`** / **`:trace get`** /
    ///   **`:trace unset KEY…`** — read/write the app-private
    ///   trace defaults (`dataset`, `deployment`) that the
    ///   upcoming trace view (step 22+) will consult when no
    ///   explicit values are given.
    /// - **`:trace <id>`** — placeholder until step 22 wires the
    ///   real waterfall view; today it just reports the gap so the
    ///   user knows the id was understood.
    ///
    /// The sub-command match short-circuits *before* the legacy
    /// reporter, so unknown sub-commands fall through to the
    /// `<id>` arm rather than silently behaving like bare
    /// `:trace`. That keeps the user-facing error one of
    /// "placeholder" vs. "unknown key" instead of two distinct
    /// success messages that look identical.
    fn cmd_trace(&mut self, args: &[&str]) {
        match args.split_first() {
            None => self.cmd_trace_current(),
            Some((&"set", rest)) => self.cmd_trace_set(rest),
            Some((&"get", rest)) => self.cmd_trace_get(rest),
            Some((&"unset", rest)) => self.cmd_trace_unset(rest),
            Some(_) => self.cmd_trace_open(args),
        }
    }

    /// `:trace <id> [dataset=NAME] [deployment=NAME]` — fetch +
    /// open the trace view. Parses the first non-`key=value`
    /// token as the trace id; the remaining `key=value` pairs
    /// override the dataset / deployment fallback chain.
    ///
    /// Validation is strict: unknown keys, missing trace id, or
    /// an empty value rejects with a clear error rather than
    /// silently picking up the previous trace's settings. The
    /// fetch itself runs through
    /// [`Self::start_trace_fetch`] which surfaces dataset /
    /// deployment fallback errors as `set_error`.
    fn cmd_trace_open(&mut self, args: &[&str]) {
        let mut trace_id: Option<String> = None;
        let mut dataset_arg: Option<String> = None;
        let mut deployment_arg: Option<String> = None;
        for raw in args {
            match raw.split_once('=') {
                Some((k, v)) => match k.trim() {
                    "dataset" => dataset_arg = Some(v.trim().to_string()),
                    "deployment" => deployment_arg = Some(v.trim().to_string()),
                    other => {
                        self.set_error(format!(
                            ":trace: unknown key `{other}` (expected: dataset, deployment)"
                        ));
                        return;
                    }
                },
                None => {
                    // First bare token = trace id. Additional
                    // bare tokens are a user mistake (the id is
                    // a single hex blob); reject so we don't
                    // silently drop or concat them.
                    if trace_id.is_none() {
                        trace_id = Some(raw.trim().to_string());
                    } else {
                        self.set_error(format!(
                            ":trace: unexpected extra arg `{raw}` (one id at a time)"
                        ));
                        return;
                    }
                }
            }
        }
        let Some(id) = trace_id else {
            self.set_error(":trace <id> [dataset=NAME] [deployment=NAME]".to_string());
            return;
        };
        if let Err(e) = self.start_trace_fetch(id, dataset_arg, deployment_arg) {
            self.set_error(format!(":trace: {e}"));
        }
    }

    /// Bare `:trace` — open the trace currently shown bottom-right
    /// in the status bar (the focused tile's trace in Grid view,
    /// else the editor's last-query trace). "What you see is what
    /// opens": it reuses [`crate::ui::status_trace_id`], the exact
    /// resolver the status bar renders, so there's no second source
    /// of truth to drift.
    ///
    /// While already inside the trace view there's nothing to open,
    /// so it just reports the loaded id (the historical bare-`:trace`
    /// behavior in that context).
    fn cmd_trace_current(&mut self) {
        if let Some(view) = self.trace_view.as_ref() {
            self.status = format!("trace: {}", view.model.trace_id);
            return;
        }
        let Some(id) = crate::ui::status_trace_id(self) else {
            self.set_error("no trace id to open (run a query first)".to_string());
            return;
        };
        // Dataset / deployment come from the saved `:trace` defaults
        // (the trace lives in the trace dataset, not the metric
        // dataset the id was observed in).
        if let Err(e) = self.start_trace_fetch(id, None, None) {
            self.set_error(format!(":trace: {e}"));
        }
    }

    /// `:trace set KEY=VALUE [KEY=VALUE…]` — accept any combination
    /// of `dataset=…` / `deployment=…`. Persists atomically on
    /// success. Unknown keys reject the whole batch (no
    /// partial-write) so a typo in one pair can't quietly skip
    /// past while another succeeds.
    fn cmd_trace_set(&mut self, args: &[&str]) {
        if args.is_empty() {
            self.set_error(":trace set KEY=VALUE [KEY=VALUE…]".to_string());
            return;
        }
        // First pass: validate every pair, build the staged
        // updates. Second pass applies + saves. Two-phase keeps
        // the in-memory store untouched if any pair is bad.
        let mut staged: Vec<(TraceKey, String)> = Vec::with_capacity(args.len());
        for raw in args {
            let (k, v) = match raw.split_once('=') {
                Some(parts) => parts,
                None => {
                    self.set_error(format!(":trace set: expected KEY=VALUE, got `{raw}`"));
                    return;
                }
            };
            let key = match TraceKey::parse(k.trim()) {
                Some(key) => key,
                None => {
                    self.set_error(format!(
                        ":trace set: unknown key `{}` (expected: dataset, deployment)",
                        k.trim()
                    ));
                    return;
                }
            };
            // Canonicalise the dataset name so a quoted value
            // (`dataset='axiom-traces-prod'`) doesn't get persisted
            // with quotes and later double-wrapped into the APL
            // literal. Deployment is a config key, never an APL
            // literal, so it's left verbatim.
            let value = match key {
                TraceKey::Dataset => crate::app::helpers::normalize_dataset_name(v),
                _ => v.trim().to_string(),
            };
            staged.push((key, value));
        }
        // Apply staged updates in order; later pairs win on dup keys.
        // Stage the save result before re-borrowing `self` for
        // `set_error` / `cmd_trace_get` — the write guard must drop first.
        let save_result = {
            let mut store = self.settings.write();
            for (key, value) in &staged {
                key.set(&mut store, Some(value.clone()));
            }
            store.save()
        };
        if let Err(e) = save_result {
            // In-memory store now reflects the user's intent; if
            // disk write failed they should retry. Surface the error.
            self.set_error(format!(":trace set: save failed: {e}"));
            return;
        }
        // Sync the sticky in-session dataset to an explicit set so it
        // takes effect immediately. Resolution precedence is arg →
        // `last_trace_dataset` → settings; without this, a stale
        // sticky value (left by an earlier `:trace <id>`) would keep
        // shadowing the new default and `:trace set dataset=…` would
        // appear to do nothing. Last write wins on duplicate keys.
        if let Some((_, value)) = staged.iter().rev().find(|(k, _)| *k == TraceKey::Dataset) {
            self.last_trace_dataset = Some(value.clone());
        }
        self.cmd_trace_get(&[]);
    }

    /// `:trace get` — echo the current `(dataset, deployment)`
    /// pair to the status bar. Unset keys read as `(unset)` so
    /// the line is always two clearly-named columns.
    fn cmd_trace_get(&mut self, args: &[&str]) {
        if !args.is_empty() {
            self.set_error(":trace get takes no arguments".to_string());
            return;
        }
        let store = self.settings.read();
        let ds = store.trace().dataset.as_deref().unwrap_or("(unset)");
        let dep = store.trace().deployment.as_deref().unwrap_or("(unset)");
        self.status = format!("trace: dataset={ds} deployment={dep}");
    }

    /// `:trace unset KEY [KEY…]` — clear one or more keys. Like
    /// `set`, unknown keys reject the whole batch.
    fn cmd_trace_unset(&mut self, args: &[&str]) {
        if args.is_empty() {
            self.set_error(":trace unset KEY [KEY…]".to_string());
            return;
        }
        let mut staged: Vec<TraceKey> = Vec::with_capacity(args.len());
        for raw in args {
            match TraceKey::parse(raw.trim()) {
                Some(key) => staged.push(key),
                None => {
                    self.set_error(format!(
                        ":trace unset: unknown key `{}` (expected: dataset, deployment)",
                        raw.trim()
                    ));
                    return;
                }
            }
        }
        let save_result = {
            let mut store = self.settings.write();
            for key in &staged {
                key.set(&mut store, None);
            }
            store.save()
        };
        if let Err(e) = save_result {
            self.set_error(format!(":trace unset: save failed: {e}"));
            return;
        }
        // Drop the sticky dataset when the default is unset, so the
        // next `:trace <id>` falls through to settings (now empty)
        // and surfaces the "no trace dataset" error instead of
        // silently reusing the cleared value.
        if staged.contains(&TraceKey::Dataset) {
            self.last_trace_dataset = None;
        }
        self.cmd_trace_get(&[]);
    }

    /// `:history` / `:his` — toggle the read-only overlay listing
    /// previously-submitted `:` commands, newest first. The list is
    /// also what `<Up>`/`<Down>` walk through from the cmdline. The
    /// overlay is informational only: dismiss with `Esc`/`q`/`Enter`.
    fn cmd_history(&mut self) {
        if self.history.entries().is_empty() {
            self.status = "history is empty".to_string();
            return;
        }
        self.history_overlay_visible = !self.history_overlay_visible;
    }

    /// `:dashinfo` / `:di` — toggle the overlay summarising the loaded
    /// dashboard's charts. No-op (with status message) if no dashboard
    /// has been opened yet.
    fn cmd_dashinfo(&mut self) {
        if self.loaded_dashboard.is_none() {
            self.status = "no dashboard loaded; try :dash ls or :open <uid>".to_string();
            return;
        }
        self.dashinfo_visible = !self.dashinfo_visible;
    }

    /// `:tile <sub>[!] [args]` — mutate the selected tile.
    ///
    /// Sub-commands (all operate on the currently-selected tile):
    /// * `add <kind>` — insert a new tile of the given viz kind at
    ///   the first free grid slot.
    /// * `rm` — delete the selected tile (no confirm; that's the `d`
    ///   keyboard flow).
    /// * `mv <x> <y>` / `mv! <x> <y>` — move to absolute virtual-grid
    ///   coordinates. Strict (rejects collisions) by default; the
    ///   bang variant auto-shoves overlapping tiles out of the way
    ///   (matches the `m`+arrows keyboard flow).
    /// * `size <w> <h>` / `size! <w> <h>` — same strict / shove
    ///   split for resize.
    /// * `title <text>` — rename the selected tile.
    /// * `yank [n]` / `cut [n]` — mirror the `y` / `x` keyboard verbs.
    /// * `paste [n]` / `paste! [n]` — mirror `p` (below) / `P` (above).
    /// * `open <kind> [n]` / `open! <kind> [n]` — mirror `o` / `O`
    ///   with a kind already chosen (skips the picker overlay).
    /// * `undo` — one-level dashboard undo (vim's `u`).
    fn cmd_tile(&mut self, args: &[&str]) {
        let Some(raw_sub) = args.first().copied() else {
            self.set_error(":tile needs a sub-command (see :help)".to_string());
            return;
        };
        // Strip a trailing `!` on the sub-command so `:tile mv! 3 0`
        // and `:tile paste! 2` parse cleanly. Outer head-bang is
        // unused for `:tile`.
        let (sub, sub_bang) = match raw_sub.strip_suffix('!') {
            Some(rest) => (rest, true),
            None => (raw_sub, false),
        };
        if self.loaded_dashboard.is_none() {
            self.set_error(":tile: no dashboard loaded".to_string());
            return;
        }
        match sub {
            "json" | "inspect" => match self.focused_chart_json() {
                Some(json) => self.tile_inspect_json = Some(json),
                None => self.status = ":tile json: no tile selected".to_string(),
            },
            "add" => self.tile_add(&args[1..]),
            "rm" => self.tile_op("rm", |app, id| {
                let n_before;
                let r = {
                    let resource = app.loaded_dashboard.as_mut().unwrap();
                    let r = tile_ops::delete(
                        &mut resource.dashboard.charts,
                        &mut resource.dashboard.layout,
                        id,
                    );
                    n_before = resource.dashboard.charts.len();
                    r
                };
                if r.is_ok() {
                    if app.selected_chart_idx >= n_before {
                        app.selected_chart_idx = n_before.saturating_sub(1);
                    }
                    app.seed_editor_from_focused_tile();
                }
                r.map(|()| format!("deleted tile {id}"))
            }),
            "mv" => match parse_two_u32(args, "mv", "x", "y", false) {
                Ok((x, y)) => self.tile_op("mv", |app, id| {
                    let resource = app.loaded_dashboard.as_mut().unwrap();
                    let (cx, cy) = resource
                        .dashboard
                        .layout
                        .iter()
                        .find(|l| l.i == *id)
                        .map(|l| (l.x as i32, l.y.unwrap_or(0) as i32))
                        .unwrap_or((0, 0));
                    let dx = x as i32 - cx;
                    let dy = y as i32 - cy;
                    if sub_bang {
                        crate::app::tile_ops_shove::shove_move(
                            &mut resource.dashboard.layout,
                            id,
                            dx,
                            dy,
                        )
                        .map(|o| match (o.moved.len().saturating_sub(1), o.new_rows) {
                            (0, 0) => format!(":tile mv! {x} {y} ok"),
                            (n, 0) => format!(":tile mv! {x} {y} ok: {n} shoved"),
                            (n, r) => format!(":tile mv! {x} {y} ok: {n} shoved, +{r} row(s)"),
                        })
                    } else {
                        tile_ops::translate(&mut resource.dashboard.layout, id, dx, dy)
                            .map(|()| format!(":tile mv {x} {y} ok"))
                    }
                }),
                Err(msg) => self.set_error(msg),
            },
            "size" => match parse_two_u32(args, "size", "w", "h", true) {
                Ok((w, h)) => self.tile_op("size", |app, id| {
                    let resource = app.loaded_dashboard.as_mut().unwrap();
                    let (cw, ch) = resource
                        .dashboard
                        .layout
                        .iter()
                        .find(|l| l.i == *id)
                        .map(|l| (l.w as i32, l.h as i32))
                        .unwrap_or((6, 6));
                    let dw = w as i32 - cw;
                    let dh = h as i32 - ch;
                    if sub_bang {
                        crate::app::tile_ops_shove::shove_resize(
                            &mut resource.dashboard.layout,
                            id,
                            dw,
                            dh,
                        )
                        .map(|o| match (o.moved.len().saturating_sub(1), o.new_rows) {
                            (0, 0) => format!(":tile size! {w} {h} ok"),
                            (n, 0) => format!(":tile size! {w} {h} ok: {n} shoved"),
                            (n, r) => format!(":tile size! {w} {h} ok: {n} shoved, +{r} row(s)"),
                        })
                    } else {
                        tile_ops::resize(&mut resource.dashboard.layout, id, dw, dh)
                            .map(|()| format!(":tile size {w} {h} ok"))
                    }
                }),
                Err(msg) => self.set_error(msg),
            },
            "title" => {
                let title = args[1..].join(" ");
                if title.is_empty() {
                    self.set_error(":tile title <text>: text required".to_string());
                    return;
                }
                self.tile_op("title", |app, id| {
                    let resource = app.loaded_dashboard.as_mut().unwrap();
                    tile_ops::set_title(&mut resource.dashboard.charts, id, &title)
                        .map(|()| format!(":tile title `{title}`"))
                });
            }
            "yank" => {
                let n = parse_optional_count(args.get(1).copied());
                self.yank_focused(n);
            }
            "cut" => {
                let n = parse_optional_count(args.get(1).copied());
                self.cut_focused(n);
            }
            "paste" => {
                let n = parse_optional_count(args.get(1).copied());
                // `paste!` mirrors the `P` key (above); plain `paste`
                // is `p` (below).
                self.paste_yanked(!sub_bang, n);
            }
            "open" => {
                let Some(kind_str) = args.get(1) else {
                    self.set_error(":tile open <kind> [n]: kind required".to_string());
                    return;
                };
                let Some(kind) = crate::dashboard::VizKind::parse(kind_str) else {
                    self.set_error(format!(":tile open: unknown viz kind `{kind_str}`"));
                    return;
                };
                let n = parse_optional_count(args.get(2).copied());
                self.snapshot_dashboard_for_undo();
                let mut placed = 0usize;
                for _ in 0..n.max(1) {
                    if !self.open_new_row_with_kind(sub_bang, kind) {
                        break;
                    }
                    placed += 1;
                }
                let label = if sub_bang { "opened above" } else { "opened below" };
                self.status = format!("{label}: {placed} {}", kind.as_str());
            }
            "undo" => self.dashboard_undo(),
            other => self.set_error(format!(
                ":tile {other}: unknown sub-command (add, rm, mv, size, title, yank, cut, paste, open, undo)"
            )),
        }
    }

    /// Shared subcommand body for `:tile rm/mv/size/title`. Resolves
    /// the focused chart id, runs `op` (which mutates the dashboard
    /// and returns the success message), and flips the dirty/status
    /// flags. Errors surface as `:tile <sub>: <msg>`.
    fn tile_op<F>(&mut self, sub: &str, op: F)
    where
        F: FnOnce(&mut App, &str) -> Result<String, &'static str>,
    {
        let Some(id) = self.current_chart_id() else {
            self.set_error(format!(":tile {sub}: no tile selected"));
            return;
        };
        match op(self, &id) {
            Ok(msg) => {
                self.dashboard_dirty = true;
                self.status = msg;
            }
            Err(e) => self.set_error(format!(":tile {sub}: {e}")),
        }
    }

    /// `:tile add <kind> [apl|mpl] [name…]` body. Kept separate from
    /// `tile_op` because it adds a chart instead of mutating an
    /// existing one and updates `selected_chart_idx` so the newly
    /// added tile is focused.
    ///
    /// The second token is an optional language; defaults to
    /// [`App::active_lang`] so adding into an APL-flavoured editor
    /// keeps the dialect. The rest is the (optional) tile name.
    fn tile_add(&mut self, args: &[&str]) {
        let Some(kind_str) = args.first() else {
            self.set_error(":tile add <kind> [apl|mpl] [name]: kind required".to_string());
            return;
        };
        let Some(kind) = crate::dashboard::VizKind::parse(kind_str) else {
            self.set_error(format!(":tile add {kind_str}: unknown viz kind"));
            return;
        };
        // Optional language token immediately after kind. We don't
        // accept `apl` / `mpl` as tile names — if the user wants a
        // tile literally called `apl`, they need to specify the
        // language explicitly first (`:tile add line mpl apl`).
        let (lang, name_start) = match args.get(1).copied() {
            Some("apl") => (crate::dashboard::Lang::Apl, 2),
            Some("mpl") => (crate::dashboard::Lang::Mpl, 2),
            _ => (self.active_lang(), 1),
        };
        let name = args[name_start..].join(" ");
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
            lang,
            &name,
        );
        self.dashboard_dirty = true;
        self.selected_chart_idx = resource.dashboard.charts.len() - 1;
        // Newly added tile becomes the focus — re-seed so the
        // editor reflects its (empty) body, not the old tile's MPL.
        self.seed_editor_from_focused_tile();
        self.status = format!("added {} {} tile {id}", lang.label(), kind.as_str());
    }

    /// `:grid` — enter multi-tile grid view. Only meaningful when a
    /// dashboard is loaded; otherwise a status message explains why.
    pub fn cmd_grid(&mut self) {
        if self.loaded_dashboard.is_none() {
            self.status = ":grid: no dashboard loaded".to_string();
            return;
        }
        // From the trace view, `:grid` is a hard switch — drop
        // the loaded trace before falling into the dashboard
        // grid layout. Otherwise the user would be in Grid
        // view with `trace_view: Some(…)` quietly held alive,
        // which would re-surface on the next `Esc` in any
        // unexpected place.
        if self.view_mode == ViewMode::Trace {
            self.trace_view = None;
            self.pending_trace_fetch = None;
        }
        // Switching away from Solo: write the editor buffer back to
        // the focused tile so we don't lose unsaved edits made while
        // zoomed in.
        self.sync_buffer_to_focused_tile();
        self.view_mode = ViewMode::Grid;
        self.focus = Pane::Dashboard;
        let n = self
            .loaded_dashboard
            .as_ref()
            .map(|r| r.dashboard.charts.len())
            .unwrap_or(0);
        if self.selected_chart_idx >= n {
            self.selected_chart_idx = 0;
            // Clamped to a different tile; refresh the editor to
            // match the new focus.
            self.seed_editor_from_focused_tile();
        }
        self.reload_legend_label_tags();
    }

    /// `:solo` — return to single-tile view. Focus drops back to the
    /// editor so the user can type immediately.
    pub fn cmd_solo(&mut self) {
        // Mirror `cmd_grid`: explicit `:solo` from the trace
        // view tears down the trace before falling through to
        // the Solo layout.
        if self.view_mode == ViewMode::Trace {
            self.trace_view = None;
            self.pending_trace_fetch = None;
        }
        self.view_mode = ViewMode::Solo;
        // Dashboard tile grid isn't rendered in Solo — redirect
        // focus to the Editor so the user isn't stranded on an
        // invisible pane. Legend stays addressable because the
        // side column is still drawn. Same applies to TraceTree
        // when the user runs `:solo` out of Trace view.
        if matches!(self.focus, Pane::Dashboard | Pane::TraceTree) {
            self.focus = Pane::Editor;
        }
        // Switch back to the editor's cached tags so the legend
        // doesn't keep the last-focused tile's selection.
        self.reload_legend_label_tags();
    }

    /// `:dash <sub> [args]` — dashboard CRUD against the server.
    ///
    /// Sub-commands:
    /// * `ls`           — open the searchable dashboard picker
    /// * `rm <uid>`     — DELETE a dashboard by uid
    /// * `new from-buffer [name]` — POST a new dashboard from the buffer
    ///
    /// Saving the *loaded* dashboard is `:w` / `:w!` — same write
    /// pattern as MPL buffers, no `:dash save` alias.
    fn cmd_dash(&mut self, args: &[&str], _bang: bool) {
        let sub = match args.first().copied() {
            Some(s) => s,
            None => {
                self.set_error(":dash needs a sub-command (ls, rm, new)".to_string());
                return;
            }
        };
        match sub {
            "ls" => self.cmd_dashboards(),
            "rm" => self.cmd_dash_rm(args.get(1).copied()),
            "new" => self.cmd_dash_new(&args[1..]),
            other => {
                self.set_error(format!(":dash {other}: unknown sub-command (ls, rm, new)"));
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
        if args.first().copied() != Some("from-buffer") {
            return self.set_error(
                ":dash new from-buffer [name]: only `from-buffer` is supported today".to_string(),
            );
        }
        let joined = args[1..].join(" ");
        let name = if joined.is_empty() {
            self.current_file
                .as_ref()
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string()
        } else {
            joined
        };
        let doc = build_dashboard_doc_from_buffer(&name, self.viz_kind, &self.query_text());
        let Some((client, tx, _)) =
            self.fetch_prepare(Some(format!("creating dashboard `{name}`…")))
        else {
            return;
        };
        self.runtime.spawn(async move {
            // Server assigns the uid; pass an empty placeholder.
            let result = client.create_dashboard(&doc, None, None).await;
            let _ = tx.send(AppEvent::DashboardSaved {
                uid: String::new(),
                result,
            });
        });
    }

    /// Send the loaded dashboard to the server. Wired to `:w` /
    /// `:w!` in dashboard mode when no `current_file` is set
    /// (i.e. the dashboard was loaded from Axiom, not from disk).
    ///
    /// `overwrite=false` (plain `:w`) means the server's optimistic
    /// version check fires — a 412 surfaces as an error so the user
    /// can rebase and retry. `:w!` skips the check.
    ///
    /// Returns `true` iff the PUT task was actually spawned. The
    /// caller (`write_then_quit`) uses this to decide whether arming
    /// `quit_after_save` is safe — a silent `false` (busy gate, no
    /// client) means no `DashboardSaved` event will ever arrive, and
    /// arming the flag would hang the app waiting for it.
    pub(super) fn put_loaded_dashboard(&mut self, overwrite: bool) -> bool {
        let Some((uid, mut doc, version)) = self
            .loaded_dashboard
            .as_ref()
            .map(|r| (r.uid.clone(), r.dashboard.clone(), r.version))
        else {
            self.set_error(":w: no dashboard loaded".to_string());
            return false;
        };
        // Wire-shape normalisation: the v2 dashboards API expects
        // every chart's query to live under the `apl` key, even
        // when the text is MPL (matches what the server returns on
        // GET, see `extract_query`'s docstring). Locally we keep
        // edited MPL under the explicit `mpl` key so
        // `extract_query` takes the direct path instead of falling
        // back to chart-kind dispatch. Bridge the two forms by
        // moving any `mpl` key over to `apl` on the cloned
        // document just before it crosses the wire.
        crate::dashboard::normalize_queries_to_wire(&mut doc);
        let verb = if overwrite { ":w!" } else { ":w" };
        let Some((client, tx, _)) =
            self.fetch_prepare(Some(format!("{verb}: saving dashboard {uid}…")))
        else {
            return false;
        };
        let uid_for_event = uid.clone();
        self.runtime.spawn(async move {
            let result = client
                .put_dashboard(&uid, &doc, version, overwrite, None)
                .await;
            let _ = tx.send(AppEvent::DashboardSaved {
                uid: uid_for_event,
                result,
            });
        });
        true
    }

    /// `:dash rm <uid>` — delete a dashboard. Requires an explicit uid
    /// argument to keep the command from ever firing accidentally
    /// against the loaded dashboard.
    fn cmd_dash_rm(&mut self, uid_arg: Option<&str>) {
        let Some(uid_raw) = uid_arg else {
            return self.set_error(":dash rm <uid>: uid argument required".to_string());
        };
        let uid = uid_raw.trim_matches('"').to_string();
        let Some((client, tx, _)) = self.fetch_prepare(Some(format!("deleting dashboard {uid}…")))
        else {
            return;
        };
        self.runtime.spawn(async move {
            let result = client.delete_dashboard(&uid).await;
            let _ = tx.send(AppEvent::DashboardDeleted { uid, result });
        });
    }

    /// `:dash ls` — open the searchable dashboard picker.
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
        // Snappy path: serve the cache, then refresh in the background
        // (silent prepare, fire-and-forget). Otherwise fetch foreground.
        let cached = self.cache.read().cached_dashboards();
        let (status, event_ctor): (Option<String>, fn(_) -> AppEvent) = match &cached {
            Some(items) => {
                let n = items.len();
                self.dashboards.open(items.clone());
                self.status = format!("{n} dashboard(s) (cached, refreshing…)");
                (None, AppEvent::DashboardsRefreshed)
            }
            None => (
                Some("fetching dashboards…".to_string()),
                AppEvent::DashboardsFetched,
            ),
        };
        let Some((client, tx, cache)) = self.fetch_prepare(status) else {
            return;
        };
        self.runtime.spawn(async move {
            let result = client.list_dashboards().await;
            if let Ok(items) = &result {
                cache_save_with(&cache, |c| c.replace_dashboards(items.clone()));
            }
            let _ = tx.send(event_ctor(result));
        });
    }

    /// `:run [target]` — unified "refetch" command.
    ///
    /// * `:run` — current context: editor query in Solo, focused tile in Grid.
    /// * `:run tile` — refetch the focused tile (Grid only).
    /// * `:run dashboard` — refetch every tile on the loaded dashboard.
    fn cmd_run(&mut self, target: Option<&str>) {
        match target {
            None => match self.view_mode {
                ViewMode::Grid => self.run_focused_tile_query(),
                ViewMode::Solo => self.run_query(),
                // No query to re-run inside the trace view —
                // `:trace <id>` is the only way to refresh the
                // displayed trace, and that path goes through
                // `cmd_trace`, not `:r`.
                ViewMode::Trace => {
                    self.status =
                        "no query in trace view (use `:trace <id>` to switch traces)".to_string();
                }
            },
            Some("tile") => self.run_focused_tile_query(),
            Some("dashboard") => {
                self.run_tile_queries();
                self.status = format!("refetching {} tile(s)…", self.tile_results.len().max(1));
            }
            Some(other) => self.set_error(format!(
                ":run {other}: unknown target (try `tile` or `dashboard`)"
            )),
        }
    }

    pub(super) fn cmd_quit(&mut self, force: bool) {
        // In the trace view plain `:q` is a window-style close
        // (vim's `:q` from a help split): exit the trace and
        // restore the previous view-mode. But `:q!` keeps its
        // "force-quit the whole app" meaning — otherwise there's
        // no way to bail out of the app from the trace view.
        if self.view_mode == ViewMode::Trace {
            if force {
                self.persist_query();
                self.should_quit = true;
            } else {
                self.exit_trace_view();
            }
            return;
        }
        if !force && self.is_dirty() {
            return self
                .set_error("E37: No write since last change (add ! to override)".to_string());
        }
        self.persist_query();
        self.should_quit = true;
    }

    /// `:w [path]` / `:w! [path]` — save the current artifact.
    ///
    /// Routes on `buffer_mode` and whether `current_file` is set:
    ///
    /// * MPL buffer — write the editor text to `path` or
    ///   `current_file` (`:w!` is the same write; we have no
    ///   readonly concept).
    /// * Dashboard mode + explicit `path` — always serialise to
    ///   that path as JSON. Same in MPL terms: vim's `:w <alt>` lets
    ///   you fork to a new file regardless of where the buffer came
    ///   from.
    /// * Dashboard mode + no path + `current_file` set (loaded from
    ///   disk) — write JSON back to that file.
    /// * Dashboard mode + no path + no `current_file` (loaded from
    ///   the Axiom server) — PUT to the server.
    ///   * `:w`  → `overwrite=false` so the server's version check
    ///     rejects concurrent writes (412 surfaces as an error;
    ///     reload then retry, or use `:w!`).
    ///   * `:w!` → `overwrite=true` (last-write-wins).
    fn cmd_write(&mut self, path: Option<&str>, bang: bool) {
        if self.buffer_mode == BufferMode::Dashboard
            && path.is_none()
            && self.current_file.is_none()
        {
            // Bare `:w` ignores the dispatch result — success/failure
            // surfaces via the status line or error overlay. The
            // return value only matters for `:wq` / `:x` (write_then_quit).
            let _ = self.put_loaded_dashboard(bang);
            return;
        }
        match self.write_file(path.map(std::path::PathBuf::from)) {
            Ok(p) => self.status = format!("wrote {}", display_path(&p)),
            Err(e) => self.set_error(format!("write failed: {e}")),
        }
    }

    fn cmd_write_quit(&mut self, path: Option<&str>, bang: bool) {
        self.write_then_quit(path, bang, true);
    }

    /// `:x` — write only when modified, then quit. Equivalent to `:wq`
    /// when dirty, or `:q` when clean.
    fn cmd_update_quit(&mut self, path: Option<&str>, bang: bool) {
        // `is_dirty()` already folds `dashboard_dirty` into Dashboard
        // mode, so a separate disjunction is redundant.
        self.write_then_quit(path, bang, self.is_dirty() || path.is_some());
    }

    /// Shared body for `:wq` / `:x`: optionally write, then quit
    /// unless the write failed (in which case the error is surfaced
    /// and `should_quit` stays false).
    ///
    /// For server-side dashboard saves (async PUT), the quit fires
    /// immediately after the request is dispatched; the
    /// `DashboardSaved` event lands on the way out and the status is
    /// printed after the alt-screen tears down. Synchronous failure
    /// modes (no client, no dashboard) still abort the quit cleanly.
    fn write_then_quit(&mut self, path: Option<&str>, bang: bool, write: bool) {
        if !write {
            self.persist_query();
            self.should_quit = true;
            return;
        }
        // Server-loaded dashboard save is async: spawn the PUT and
        // defer the quit to the `DashboardSaved` handler. Quitting
        // here would let the main loop drop the tokio runtime before
        // the in-flight HTTP request lands, silently dropping the
        // user's edits.
        if self.buffer_mode == BufferMode::Dashboard
            && path.is_none()
            && self.current_file.is_none()
        {
            if self.put_loaded_dashboard(bang) {
                // Dispatch succeeded — wait for the save event.
                self.quit_after_save = true;
            }
            // Dispatch failed (no client / busy / no dashboard): the
            // status or error overlay already explains why; stay
            // running so the user can retry.
            return;
        }
        // Synchronous paths: write now, quit now.
        if let Err(e) = self.write_file(path.map(std::path::PathBuf::from)) {
            return self.set_error(format!("write failed: {e}"));
        }
        self.persist_query();
        self.should_quit = true;
    }

    fn cmd_edit(&mut self, path: Option<&str>, force: bool) {
        // `:e!` with no path reloads the current file from disk.
        let path = match (path, force) {
            (Some(p), _) => std::path::PathBuf::from(p),
            (None, true) => match self.current_file.clone() {
                Some(p) => p,
                None => return self.set_error("E32: No file name".to_string()),
            },
            (None, false) => return self.set_error("E32: No file name".to_string()),
        };
        self.do_open(path, force);
    }
}
