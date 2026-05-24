//! Dashboard-grid `App` operations: time-range mutation, tile
//! selection / zoom, dashboard adoption, and the buffer-pragma →
//! `viz_kind` sync that fires on every diagnostic recompute.

use super::*;

impl App {

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
    pub(super) fn set_time_range(&mut self, start: String, end: String) {
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
        // user having to remember per-mode shortcuts; `:run` handles both.
        if self.view_mode == ViewMode::Grid && self.loaded_dashboard.is_some() {
            self.run_tile_queries();
        } else if !self.query_text().trim().is_empty() {
            self.run_query();
        }
    }

    /// Serialise the current dashboard to pretty JSON. Errors when
    /// no dashboard is loaded. Pure helper exposed for tests of the
    /// round-trip; production code goes through `write_file`.
    #[cfg(test)]
    pub(super) fn dashboard_to_json(&self) -> anyhow::Result<String> {
        use anyhow::anyhow;
        let resource = self
            .loaded_dashboard
            .as_ref()
            .ok_or_else(|| anyhow!("no dashboard loaded"))?;
        serde_json::to_string_pretty(resource).map_err(Into::into)
    }

    /// Switch into Grid view mode when the loaded dashboard has ≥2
    /// charts; otherwise stay in Solo. Called from `adopt_dashboard`
    /// and `open_file` so the user never has to manually flip into
    /// grid view to see a multi-tile dashboard.
    pub(super) fn auto_switch_view_mode(&mut self) {
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
        if let Some(text) = Self::build_query_seed(&pragma_line, &query) {
            self.editor = editor::editor_with_text(&text);
            self.recompute_diagnostics();
        }
        // For MPL tiles, pin the editor-side query context to the
        // tile's (dataset, metric) so the upcoming legend-tag reload
        // finds the right per-metric cache slot. We don't know the
        // AST hash without running the pipeline, so pass empty —
        // `resolve_legend_tags` falls through to the by-metric store.
        if let Query::Mpl(mpl) = &query
            && let Ok((ds, m)) = crate::mpl::extract_dataset_metric(mpl)
        {
            self.last_query_context = Some(QueryContext {
                hash: String::new(),
                dataset: ds,
                metric: m,
            });
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

    pub(super) fn adopt_dashboard(&mut self, uid: String, resource: crate::axiom::DashboardSummary) {
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
        // Query::Empty leaves the editor alone — the tile renderer
        // surfaces the note body / placeholder directly.
        if let Some(text) = Self::build_query_seed(&pragma_line, &focused_query) {
            self.editor = editor::editor_with_text(&text);
            self.recompute_diagnostics();
        }
        // Capture the seed *after* `recompute_diagnostics` so it matches
        // what `query_text()` returns for an untouched buffer (line
        // endings normalised by the editor).
        self.last_adopted_seed = match &focused_query {
            Query::Empty => None,
            _ => Some(self.query_text()),
        };
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

    /// Reconcile the focused tile's `kind`, `opts`, and MPL query text
    /// with whatever's in the editor buffer. Called by
    /// [`recompute_diagnostics`] on every buffer change, so the dashboard
    /// model is always in sync without scheduling extra passes.
    ///
    /// Pragma parse errors are pushed onto `self.diagnostics` so they
    /// surface alongside MPL diagnostics in the status bar and pane chrome.
    /// On error we keep the previous kind/opts so the chart doesn't
    /// flicker between renders while the user is mid-edit.
    /// Build the editor seed for `query` using the supplied `// @viz`
    /// pragma line. Returns `None` for `Query::Empty` (caller leaves
    /// the editor untouched). APL queries get a `// APL query —
    /// execution lands in step 14b` banner so the MPL parser doesn't
    /// flag every line.
    pub(super) fn build_query_seed(pragma_line: &str, query: &crate::dashboard::Query) -> Option<String> {
        use crate::dashboard::Query;
        match query {
            Query::Mpl(mpl) => Some(format!("{pragma_line}{mpl}")),
            Query::Apl(apl) => Some(format!(
                "{pragma_line}// APL query — execution lands in step 14b\n// {}\n",
                apl.replace('\n', "\n// ")
            )),
            Query::Empty => None,
        }
    }

    pub(super) fn sync_dashboard_from_buffer(&mut self, text: &str) {
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
}
