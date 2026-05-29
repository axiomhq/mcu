//! Dashboard-grid `App` operations: time-range mutation, tile
//! selection / zoom, dashboard adoption, and the buffer-pragma →
//! `viz_kind` sync that fires on every diagnostic recompute.

use super::*;

impl App {
    /// Resolve the language the user is currently editing in.
    ///
    /// * [`BufferMode::Dashboard`]: the focused tile's language
    ///   (`extract_lang` on the chart). Falls back to
    ///   [`App::buffer_lang`] when the dashboard has no charts or
    ///   the focused tile has no query (Note / Spacer / Unknown).
    /// * [`BufferMode::Mpl`]: the standalone-buffer
    ///   [`App::buffer_lang`], flipped by `:apl` / `:mpl`.
    ///
    /// Used by the status bar to render `NORMAL · APL` /
    /// `NORMAL · MPL` and by `run_query` to dispatch standalone
    /// queries to the right endpoint.
    pub fn active_lang(&self) -> crate::dashboard::Lang {
        if self.buffer_mode != BufferMode::Dashboard {
            return self.buffer_lang;
        }
        self.loaded_dashboard
            .as_ref()
            .and_then(|r| r.dashboard.charts.get(self.selected_chart_idx))
            .and_then(crate::dashboard::extract_lang)
            .unwrap_or(self.buffer_lang)
    }

    /// Active query time range, in the order the Axiom API wants it
    /// (`start`, `end`). Sourced from `self.time.range`, which is
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
            normalize_time_expr(&self.time.range.start),
            normalize_time_expr(&self.time.range.end),
        )
    }

    /// Common path for every time-range mutation: write the in-memory
    /// model, mirror onto the wire copy so `:dash save` persists, mark
    /// the dashboard dirty, status-line the change, and kick a refetch
    /// so the user sees the new window immediately.
    pub(super) fn set_time_range(&mut self, start: String, end: String) {
        self.time.range = TimeRange {
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
        let wrapped = (((i % n as isize) + n as isize) % n as isize) as usize;
        self.set_focused_chart(wrapped);
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
            self.set_focused_chart(next);
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
        // Pull the chart up-front for the post-seed bookkeeping
        // (tile-results adoption, title, legend tags). The seed
        // helper does its own clone so this one is independent.
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
        // Re-seed the editor + viz state from the focused tile.
        // `seed_editor_from_focused_tile` returns the extracted
        // `Query` so we don't extract twice.
        let Some(query) = self.seed_editor_from_focused_tile() else {
            return;
        };
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
        // Unknown chart variants have no `ChartBase` (id/name/extras),
        // so they can't be zoomed into: there's nothing for the editor
        // to seed from and nothing for `tile_results` to key on. The
        // `seed_editor_from_focused_tile` guard above already returns
        // `None` for Unknowns, so by here `chart` is `Chart::Known` in
        // practice. We still go through `base()` so an SDK upgrade
        // can't turn this into a panic.
        let Some(chart_id) = chart.base().map(|b| b.id.clone()) else {
            return;
        };
        if let Some(tile) = self.tile_results.get(&chart_id) {
            self.series = tile.series.clone();
            self.legend.hidden = vec![false; self.series.len()];
            if self.legend.selected >= self.series.len() {
                self.legend.selected = 0;
            }
            if let Some(tid) = tile.trace_id.clone() {
                self.last_trace_id = Some(tid);
            }
        } else {
            // No tile data yet (zoom raced the fetch, or the tile
            // has no MPL). Clear so the user doesn't see stale
            // demo data labelled with a different tile's title.
            self.series.clear();
            self.legend.hidden.clear();
            self.legend.selected = 0;
        }
        self.view_mode = ViewMode::Solo;
        self.focus = Pane::Editor;
        // Now that `last_query_context` is pinned to the tile and
        // view mode is Solo, pick up that metric's saved tag
        // selection (or clear if there's nothing cached).
        self.reload_legend_label_tags();
        let title = chart
            .base()
            .and_then(|b| b.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| kind.as_str().to_string());
        self.status = format!("zoomed `{title}`");
    }

    pub(super) fn adopt_dashboard(
        &mut self,
        uid: String,
        resource: crate::axiom::DashboardSummary,
    ) {
        use crate::axiom::DashboardSummaryExt;
        use crate::dashboard::Query;
        let name = resource.name_or_unnamed().to_string();
        let chart_count = resource.dashboard.charts.len();
        self.time.range = TimeRange::from_resource(&resource);
        // Focus snaps to the first chart — matches the grid's
        // initial selection and the prior `Dashboard::tiles[0]`
        // semantics. Empty dashboards fall through to defaults.
        self.selected_chart_idx = 0;
        self.last_picked_dashboard = Some(uid);
        self.loaded_dashboard = Some(resource);
        // The dashboard is now the canonical artifact. `:w` and
        // friends route on `buffer_mode` and expect this to flip so
        // a save targets the server (or a `:w <path>` JSON dump),
        // not the focused tile's editor buffer. File-loaded
        // dashboards set the same flag in `open_file`.
        self.buffer_mode = BufferMode::Dashboard;
        // No on-disk backing for server-loaded dashboards; clear any
        // stale `current_file` from a previous MPL session so `:w`
        // routes to the server PUT path, not a leftover .mpl path.
        self.current_file = None;

        // Seed the editor from the focused (first) tile. Empty
        // dashboards leave the editor untouched (no charts to seed
        // from), preserving any MPL the user was already writing.
        let focused_query = if chart_count > 0 {
            self.seed_editor_from_focused_tile().unwrap_or(Query::Empty)
        } else {
            Query::Empty
        };
        // `last_adopted_seed` powers the background-refresh re-adopt
        // guard: a fresh adopt that matches the buffer means "pristine,
        // safe to re-adopt". With per-tile re-seeding (each navigation
        // rewrites the buffer) this stays a per-adopt snapshot —
        // refreshes re-adopt only when the user hasn't dirtied the
        // dashboard.
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
    /// the editor untouched). Both MPL and APL queries seed as raw
    /// text — the language is tracked separately (via the chart's
    /// `axLang` sidecar or `App.buffer_lang`) and surfaced in the
    /// status bar so the user always knows which dialect they're in.
    pub(super) fn build_query_seed(
        pragma_line: &str,
        query: &crate::dashboard::Query,
    ) -> Option<String> {
        use crate::dashboard::Query;
        match query {
            Query::Mpl(text) | Query::Apl(text) => Some(format!("{pragma_line}{text}")),
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

    /// Move dashboard focus to `idx`, persisting the currently focused
    /// tile's editor edits first (so we don't lose work when navigating
    /// away) and then re-seeding the editor from the new focus. No-op
    /// when `idx` is already the focused chart — navigation that
    /// resolves to the same tile must not clobber pending edits.
    pub(super) fn set_focused_chart(&mut self, idx: usize) {
        if idx == self.selected_chart_idx {
            // Still reload tags in case caller relied on the side effect.
            self.reload_legend_label_tags();
            return;
        }
        // Save current edits to the outgoing tile before the
        // selection moves; otherwise the next seed would clobber them.
        self.sync_buffer_to_focused_tile();
        self.selected_chart_idx = idx;
        self.seed_editor_from_focused_tile();
        self.reload_legend_label_tags();
    }

    /// Re-seed the editor buffer + viz state from the currently focused
    /// tile in `loaded_dashboard`. The pragma line (`// @viz <kind>`)
    /// is always present; the body is the tile's MPL, an APL banner,
    /// or a `(no query)` placeholder for Note/Spacer tiles.
    ///
    /// Only MPL tiles get live diagnostics. APL banners and Empty seeds
    /// are comment-only buffers that the MPL parser would reject —
    /// Resolve the OTEL unit for the tile identified by `chart_id`,
    /// given the just-arrived wire-form `series`. Looks up
    /// (dataset, metric) from the tile's MPL query, then runs the
    /// shared three-tier discovery. `None` when no source carries a
    /// recognised unit, or when the tile has no MPL (APL/unknown).
    pub(super) fn resolve_tile_unit(
        &self,
        chart_id: &str,
        series: &[crate::axiom::MetricsSeries],
    ) -> Option<crate::unit::Unit> {
        let resource = self.loaded_dashboard.as_ref()?;
        let chart = resource
            .dashboard
            .charts
            .iter()
            .find(|c| c.base().is_some_and(|b| b.id == chart_id))?;
        let crate::dashboard::Query::Mpl(mpl) = crate::dashboard::extract_query(chart) else {
            return None;
        };
        let (dataset, metric) = crate::mpl::extract_dataset_metric(&mpl).ok()?;
        let cache = self.cache.read();
        crate::app::helpers::resolve_unit(&cache, &dataset, &metric, series, &mpl)
    }

    /// Resolve the OTEL unit for the editor's last query. Used by the
    /// solo-view `QueryFinished` handler. Pulls (dataset, metric)
    /// from `last_query_context` (set when the query was kicked off,
    /// so it survives buffer edits between dispatch and response)
    /// and the editor's current text for the tier-3 pragma.
    pub(super) fn resolve_editor_unit(
        &self,
        series: &[crate::axiom::MetricsSeries],
    ) -> Option<crate::unit::Unit> {
        let ctx = self.last_query_context.as_ref()?;
        let cache = self.cache.read();
        crate::app::helpers::resolve_unit(
            &cache,
            &ctx.dataset,
            &ctx.metric,
            series,
            &self.query_text(),
        )
    }

    /// Resolve the best unit from editor-visible state, without
    /// waiting for a fetch response. This mirrors the persisted
    /// discovery order as closely as the live editor can:
    ///
    /// 1. cached `MetricInfo.unit` for the query's `(dataset, metric)`
    /// 2. existing rendered-series `otel.metric.unit` tags, if any
    /// 3. `// @unit` pragma in the buffer
    ///
    /// The raw buffer is passed through unchanged. `// @viz` and
    /// other leading comments are comments as far as MPL is
    /// concerned; `extract_dataset_metric` already skips them, and
    /// the unit pragma parser scans the same leading comment block.
    fn resolve_live_unit_from_buffer(
        &self,
        text: &str,
        series: &[crate::chart::Series],
    ) -> Option<crate::unit::Unit> {
        if let Ok((dataset, metric)) = crate::mpl::extract_dataset_metric(text) {
            let cache = self.cache.read();
            if let Some(info) = cache.metric_info(&dataset, &metric)
                && let Some(raw) = info.unit.as_deref()
                && let Some(unit) = crate::unit::parse(raw)
            {
                return Some(unit);
            }
        }

        for s in series {
            for (name, value) in &s.tags {
                if name == "otel.metric.unit"
                    && let Some(raw) = value.as_str()
                    && let Some(unit) = crate::unit::parse(raw)
                {
                    return Some(unit);
                }
            }
        }

        crate::unit::pragma::parse_unit_pragma(text).ok().flatten()
    }

    /// Keep unit display in sync with `// @unit` edits. Fetch
    /// completion remains authoritative for response-tag discovery,
    /// but the editor should not need a rerun just to change axis
    /// labels from `By` to `MiB` or `ms` to `s`.
    pub(super) fn sync_live_unit_from_buffer(&mut self, text: &str) {
        if self.buffer_mode == BufferMode::Dashboard {
            let Some(chart_id) = self.current_chart_id() else {
                return;
            };
            let series = self
                .tile_results
                .get(&chart_id)
                .map(|t| t.series.clone())
                .unwrap_or_default();
            let unit = self.resolve_live_unit_from_buffer(text, &series);
            if let Some(tile) = self.tile_results.get_mut(&chart_id) {
                tile.unit = unit.clone();
            }
            if self.view_mode == ViewMode::Solo {
                self.unit = unit;
            }
        } else {
            let series = self.series.clone();
            self.unit = self.resolve_live_unit_from_buffer(text, &series);
        }
    }

    /// surfacing that as a status-bar error is noise, so we suppress it.
    /// Returns the extracted [`Query`] so callers (zoom) can use it for
    /// follow-up state updates without re-extracting.
    pub(super) fn seed_editor_from_focused_tile(&mut self) -> Option<crate::dashboard::Query> {
        use crate::dashboard::Query;
        let resource = self.loaded_dashboard.as_ref()?;
        let chart = resource
            .dashboard
            .charts
            .get(self.selected_chart_idx)?
            .clone();
        let kind = VizKind::from_chart(&chart);
        let query = crate::dashboard::extract_query(&chart);
        self.viz_kind = kind;
        self.viz_opts.clear();
        let pragma_line = format!("// @viz {}\n", kind.as_str());
        let text = Self::build_query_seed(&pragma_line, &query)
            .unwrap_or_else(|| format!("{pragma_line}// (no query for this tile)\n"));
        self.editor = editor::editor_with_text(&text);
        if matches!(query, Query::Mpl(_)) {
            self.recompute_diagnostics();
        } else {
            // Refresh pragma-derived viz state without invoking the MPL
            // analyzer (whose error on a comment-only buffer is the
            // exact noise we're suppressing).
            self.diagnostics.clear();
            self.sync_dashboard_from_buffer(&text);
            self.recompute_sig_help();
        }
        Some(query)
    }

    /// Push the editor buffer back into the focused tile's query,
    /// honouring whichever language ([`Lang::Mpl`] / [`Lang::Apl`])
    /// the tile is currently in. No-op outside Dashboard mode, when
    /// the focused tile has no query (Note/Spacer), or when the
    /// stripped text already matches the chart's stored text (the
    /// equality guard keeps `dashboard_dirty` from flipping on cursor
    /// moves, undo/redo round-trips, or seed operations that just
    /// loaded the same text we're about to write).
    ///
    /// The buffer's leading `// @viz <kind>` pragma is stripped — the
    /// chart's wire kind comes from `Chart::Known(…)`, not the pragma.
    /// Changing the pragma in the buffer adjusts the TUI render kind
    /// for the current session but doesn't rewrite the server-side
    /// chart variant (that's `:viz <kind>` for the editor / a separate
    /// "convert chart type" command for the dashboard).
    pub(super) fn sync_buffer_to_focused_tile(&mut self) {
        use crate::dashboard::{Lang, Query};
        if self.buffer_mode != BufferMode::Dashboard {
            return;
        }
        let Some(resource) = self.loaded_dashboard.as_mut() else {
            return;
        };
        let Some(chart) = resource.dashboard.charts.get_mut(self.selected_chart_idx) else {
            return;
        };
        // Note / Spacer tiles still have no editable body — the seed
        // path shows the `// (no query for this tile)` placeholder
        // and we drop edits silently so the user doesn't accidentally
        // convert a Note into a query tile by typing into it.
        let (lang, existing_text) = match crate::dashboard::extract_query(chart) {
            Query::Mpl(s) => (Lang::Mpl, s),
            Query::Apl(s) => (Lang::Apl, s),
            Query::Empty => return,
        };
        let buf = self.editor.lines().join("\n");
        let new_text = strip_viz_pragma(&buf);
        if new_text == existing_text {
            return;
        }
        // Apply the editor's new text back onto the focused tile.
        // `Chart::Unknown` carries raw JSON with no `ChartBase`, so
        // there's nothing to mutate; we skip silently — the editor
        // shouldn't have been routed to an Unknown tile in the first
        // place, but bail safely either way.
        let Some(base) = chart.base_mut() else {
            return;
        };
        // Stamp the language sidecar so the next reload classifies
        // deterministically without falling back to chart-kind
        // heuristics. Round-trips through `ChartBase.extras`.
        base.extras.insert(
            crate::dashboard::LANG_SIDECAR_KEY.to_string(),
            serde_json::Value::String(lang.as_sidecar().to_string()),
        );
        // Write the edited text into the language's key, drop the
        // sibling, preserve any other extras on the query object.
        // Two independent constraints both point to single-key
        // objects:
        //
        //  1. Server side: dual-keyed `{ apl, mpl }` PUTs are
        //     rejected with 400 — the root cause of the "editing a
        //     chart's query breaks :w" bug. A single-key object
        //     (either key alone) is accepted per the v2 OpenAPI.
        //
        //  2. Local side: `extract_query` short-circuits when the
        //     key matches the sidecar's language. Encoding the
        //     user's intent explicitly makes renders take the
        //     direct path on the next read.
        let (write_key, drop_key) = match lang {
            Lang::Mpl => ("mpl", "apl"),
            Lang::Apl => ("apl", "mpl"),
        };
        let new_query = match base.query.take() {
            Some(mut v) if v.is_object() => {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove(drop_key);
                    obj.insert(
                        write_key.to_string(),
                        serde_json::Value::String(new_text.to_string()),
                    );
                }
                v
            }
            _ => serde_json::json!({ write_key: new_text }),
        };
        base.query = Some(new_query);
        self.dashboard_dirty = true;
    }
}

/// Strip a leading `// @viz <kind>` pragma line off the editor buffer
/// so what's left is the raw MPL we can store on the wire chart. If
/// the buffer has no pragma we return it untouched.
fn strip_viz_pragma(buf: &str) -> &str {
    let rest = match buf.strip_prefix("// @viz ") {
        Some(r) => r,
        None => return buf,
    };
    match rest.find('\n') {
        Some(i) => &rest[i + 1..],
        None => "",
    }
}
