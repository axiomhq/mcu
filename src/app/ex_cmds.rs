//! Ex-command (`:foo`) implementation. Every `cmd_*` method here is
//! reached via [`App::execute_command`], which parses the typed
//! command line and dispatches. The dispatch lives in the same file
//! because it's a flat table — splitting it would force every entry
//! to be public.

use crate::dashboard::VizKind;

use super::*;

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

    pub(super) fn cmd_quit(&mut self, force: bool) {
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
}
