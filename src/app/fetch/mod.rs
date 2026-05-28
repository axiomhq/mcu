//! Async fetch + event handling. Every method here either kicks off
//! a background task (`fetch_*`, `run_query`, `run_tile_queries`,
//! `fetch_dashboard_by_uid`) or consumes the events those tasks
//! produce (`drain_events` + the giant `handle_event` match).
//!
//! Keeping the lifecycle in one place makes it easy to reason about
//! the cache writes that happen on success and the error surfacing
//! that happens on failure.

use super::*;

mod dashboard;
mod discovery;
mod query;
mod trace;

impl App {
    /// Drain background events and apply them to app state.
    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.events_rx.try_recv() {
            // A handled background event can change any visible state;
            // repaint on the next loop iteration.
            self.needs_redraw = true;
            self.handle_event(ev);
        }
    }

    pub(super) fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::DatasetsFetched(result) => {
                self.busy = false;
                match result {
                    Ok(d) => self.status = format!("loaded {} dataset(s)", d.len()),
                    Err(e) => self.set_error(format!("datasets error: {e}")),
                }
            }
            AppEvent::DashboardsFetched(result) => {
                self.busy = false;
                match result {
                    Ok(items) => {
                        let n = items.len();
                        self.dashboards.open(items);
                        self.status = format!("{n} dashboard(s)");
                    }
                    Err(e) => self.set_error(format!("dashboards error: {e}")),
                }
            }
            // Background refresh — update the picker if still visible;
            // surface failures softly without clobbering current state.
            AppEvent::DashboardsRefreshed(Ok(items)) if self.dashboards.visible => {
                let n = items.len();
                self.dashboards.refresh_items(items);
                self.status = format!("{n} dashboard(s) (refreshed)");
            }
            AppEvent::DashboardsRefreshed(Ok(_)) => {}
            AppEvent::DashboardsRefreshed(Err(e)) => {
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
                    let still_focused =
                        self.loaded_dashboard.as_ref().is_some_and(|d| d.uid == uid);
                    if !still_focused {
                        // User moved on to a different dashboard while
                        // the refresh was in flight. Cache is already
                        // updated; nothing else to do.
                        return;
                    }
                    let pristine = !self.dashboard_dirty
                        && self.last_adopted_seed.as_deref() == Some(self.query_text().as_str());
                    if pristine {
                        use crate::axiom::DashboardSummaryExt;
                        let name = resource.name_or_unnamed().to_string();
                        self.adopt_dashboard(uid, resource);
                        self.status = format!("refreshed `{name}`");
                    } else {
                        // Editor has unsaved work — leave `loaded_dashboard`
                        // (charts, layout, time window, and especially
                        // `version`) untouched. Replacing the resource
                        // here would lose the user's pending edits;
                        // bumping just `version` to match the server
                        // would silently defeat optimistic concurrency,
                        // turning the next `:w` into a quiet clobber
                        // of whatever the other writer changed. The
                        // fresh server snapshot is already in the
                        // cache for the next session; if the server
                        // moved on, `:w` will surface a version
                        // conflict and the user can `:w!` to
                        // overwrite or reload to discard local edits.
                        let _ = resource;
                        self.status = "dashboard refreshed (editor kept; reload to discard edits)"
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
                            cache_save_with(&self.cache, |c| {
                                c.replace_dashboard(&write.dashboard.uid, write.dashboard.clone())
                            });
                        }
                        self.loaded_dashboard = Some(write.dashboard);
                        self.dashboard_dirty = false;
                        self.status = format!(
                            "{verb} dashboard {uid} — version {}",
                            new_version
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "?".to_string())
                        );
                        // `:wq` / `:x` armed a deferred quit; honour
                        // it now that the save round-tripped. Guard
                        // against the race where the user edited
                        // again between dispatch and response — we
                        // honour the save that landed but cancel the
                        // quit so the new edits aren't lost.
                        if self.quit_after_save {
                            self.quit_after_save = false;
                            if self.dashboard_dirty {
                                self.status = "saved — quit aborted (buffer modified)".to_string();
                            } else {
                                self.persist_query();
                                self.should_quit = true;
                            }
                        }
                    }
                    Err(e) => {
                        // Failed save must not leave a stale
                        // `quit_after_save` armed — otherwise a later
                        // successful save would ghost-quit the app.
                        self.quit_after_save = false;
                        self.set_error(format!("save {uid}: {e}"));
                    }
                }
            }
            AppEvent::TileQueryFinished {
                chart_id,
                epoch,
                result,
            } => {
                // Drop results from a superseded dashboard load. The
                // epoch is bumped in `run_tile_queries`; any task
                // spawned before that bump is by definition stale.
                if epoch != self.tile_query_epoch {
                    return;
                }
                // The slot may also have been removed locally (tile
                // deleted, dashboard cleared without a re-fetch).
                // Bail before touching anything if the slot is gone.
                if !self.tile_results.contains_key(&chart_id) {
                    return;
                }
                // Resolve the OTEL unit BEFORE we take the
                // `tile_results` mutable borrow — `resolve_tile_unit`
                // needs `&self` to read the loaded dashboard and the
                // cache. Only call it on success; on error we keep
                // the previous unit so the axis doesn't shuffle.
                let resolved_unit = match &result {
                    Ok(resp) => self.resolve_tile_unit(&chart_id, &resp.series),
                    Err(_) => None,
                };
                let entry = self
                    .tile_results
                    .get_mut(&chart_id)
                    .expect("presence checked above");
                entry.busy = false;
                // Consume `started_at` into `elapsed` so the grid
                // renderer can show `[3.5s]` in the bottom border.
                // Failed fetches still get a duration — it's useful
                // for debugging slow errors ("timed out after 30s"
                // looks very different from "failed in 80ms").
                entry.elapsed = entry.started_at.take().map(|t| t.elapsed());
                match result {
                    Ok(resp) => {
                        entry.trace_id = resp.trace_id.clone();
                        entry.series = response_to_series(&resp);
                        entry.error = None;
                        entry.unit = resolved_unit;
                    }
                    Err(e) => {
                        entry.error = Some(format!("{e}"));
                        // Leave `entry.unit` as-is on error so the
                        // last successful resolution keeps driving
                        // the axis; clearing it would make the
                        // chart shuffle units on a flaky tile.
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
            AppEvent::TileAplFinished {
                chart_id,
                epoch,
                result,
            } => {
                // Same stale-result + slot-removed guards as MPL.
                if epoch != self.tile_query_epoch {
                    return;
                }
                if !self.tile_results.contains_key(&chart_id) {
                    return;
                }
                // Resolve the focused viz kind for this tile up front
                // so we don't have to re-walk `loaded_dashboard` under
                // the mutable borrow. APL doesn't have a metrics-
                // metadata unit, so `entry.unit` stays at its last
                // value (typically None for APL-only tiles).
                let viz_kind = self
                    .loaded_dashboard
                    .as_ref()
                    .and_then(|r| {
                        r.dashboard
                            .charts
                            .iter()
                            .find(|c| c.base().is_some_and(|b| b.id == chart_id))
                    })
                    .map(crate::dashboard::VizKind::from_chart)
                    .unwrap_or(crate::dashboard::VizKind::Line);
                let entry = self
                    .tile_results
                    .get_mut(&chart_id)
                    .expect("presence checked above");
                entry.busy = false;
                entry.elapsed = entry.started_at.take().map(|t| t.elapsed());
                match result {
                    Ok(resp) => {
                        entry.trace_id = resp.trace_id.clone();
                        // Per-kind decoder routing:
                        //   * Table / LogStream — render the raw
                        //     tabular response.
                        //   * Everything else — reshape into
                        //     `Vec<Series>`; on shape mismatch
                        //     surface the decoder's error message
                        //     (e.g. "no time column found…") so the
                        //     user knows what to fix.
                        let wants_table = matches!(
                            viz_kind,
                            crate::dashboard::VizKind::Table | crate::dashboard::VizKind::LogStream
                        );
                        if wants_table {
                            entry.table = Some(crate::viz::apl_decode::to_table_result(&resp));
                            entry.series.clear();
                            entry.error = None;
                        } else {
                            match crate::viz::apl_decode::to_series(
                                &resp,
                                &std::collections::BTreeMap::new(),
                            ) {
                                Ok(series) => {
                                    entry.series = series;
                                    entry.table = None;
                                    entry.error = None;
                                }
                                Err(e) => {
                                    // Clear any previously-decoded data
                                    // so the tile doesn't show stale
                                    // series/table underneath the fresh
                                    // decode error.
                                    entry.series.clear();
                                    entry.table = None;
                                    entry.error = Some(format!("APL: {e}"));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        entry.error = Some(format!("{e}"));
                    }
                }
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
                            cache_save_with(&self.cache, |c| c.forget_dashboard(&uid));
                        }
                        self.status = format!("deleted dashboard {uid}");
                    }
                    Err(e) => {
                        self.set_error(format!("delete {uid}: {e}"));
                    }
                }
            }
            AppEvent::MetricsFetched { dataset, result } => {
                self.busy = false;
                match result {
                    Ok(metrics) => {
                        self.status = format!("loaded {} metric(s) for `{dataset}`", metrics.len())
                    }
                    Err(e) => self.set_error(format!("metrics error for `{dataset}`: {e}")),
                }
            }
            // Background prefetches — don't clobber foreground status while
            // a query is in flight.
            AppEvent::TagsFetched {
                dataset,
                metric,
                result,
            } if !self.busy => {
                self.status = match result {
                    Ok(tags) => {
                        format!("loaded {} tag(s) for `{dataset}:{metric}`", tags.len())
                    }
                    Err(e) => format!("tags error for `{dataset}:{metric}`: {e}"),
                };
            }
            AppEvent::TagsFetched { .. } => {}
            AppEvent::TagValuesFetched {
                dataset,
                metric,
                tag,
                result,
            } if !self.busy => {
                self.status = match result {
                    Ok(values) => format!(
                        "loaded {} value(s) for `{dataset}:{metric}.{tag}`",
                        values.len()
                    ),
                    Err(e) => format!("values error for `{dataset}:{metric}.{tag}`: {e}"),
                };
            }
            AppEvent::TagValuesFetched { .. } => {}
            AppEvent::TraceFetchFinished { query_id, result } => {
                self.handle_trace_fetch_finished(query_id, result);
            }
            AppEvent::AplQueryFinished { id, result } => {
                if id != self.last_query_id {
                    return;
                }
                self.busy = false;
                match result {
                    Ok(resp) => {
                        self.last_trace_id = resp.trace_id.clone();
                        // Route by the buffer's viz kind: Table /
                        // LogStream render the raw tabular response
                        // (typed columns survive); everything else
                        // reshapes into chart series via the same
                        // decoder the dashboard path uses.
                        let wants_table = matches!(
                            self.viz_kind,
                            crate::dashboard::VizKind::Table | crate::dashboard::VizKind::LogStream
                        );
                        if wants_table {
                            let t = crate::viz::apl_decode::to_table_result(&resp);
                            let rows = t.rows.len();
                            self.table_result = Some(t);
                            // Fresh data: park the cursor at the top.
                            // Otherwise a previous query's selection
                            // could point past the new row count.
                            self.table_selected = 0;
                            // Drop any stale chart series from a
                            // prior MPL run so the table is the
                            // unambiguous data source.
                            self.series.clear();
                            self.legend.hidden.clear();
                            self.legend.selected = 0;
                            self.status = if rows == 0 {
                                "APL query returned no rows".to_string()
                            } else {
                                format!("{rows} rows")
                            };
                        } else {
                            match crate::viz::apl_decode::to_series(
                                &resp,
                                &std::collections::BTreeMap::new(),
                            ) {
                                Ok(new_series) => {
                                    // Switching to a chart viz drops any
                                    // table_result from a prior APL run.
                                    self.table_result = None;
                                    let count = new_series.len();
                                    if count == 0 {
                                        self.status = "APL query returned no series".to_string();
                                    } else {
                                        self.series = new_series;
                                        self.legend.hidden = vec![false; count];
                                        if self.legend.selected >= count {
                                            self.legend.selected = 0;
                                        }
                                        self.legend.details_cursor = 0;
                                        self.legend.pending_g = false;
                                        self.reload_legend_label_tags();
                                        self.status = format!("{count} series");
                                    }
                                }
                                Err(e) => {
                                    self.set_error(format!(
                                        "APL: {e} (hint: add `// @viz table` to render the raw rows)"
                                    ));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        self.set_error(format!("query error: {e}"));
                    }
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
                        // Resolve the OTEL unit while we still have
                        // the wire-form series (with typed tag
                        // values for tier-2 lookup).
                        self.unit = self.resolve_editor_unit(&resp.series);
                        let new_series = response_to_series(&resp);
                        let count = new_series.len();
                        // MPL never produces a table_result; clear
                        // any leftover from a prior APL run so the
                        // Table viz doesn't display the wrong data
                        // source.
                        self.table_result = None;
                        if count == 0 {
                            self.status = "query returned no series".to_string();
                        } else {
                            self.series = new_series;
                            // Reset legend state. Carrying `hidden` across
                            // queries would require name-stable matching
                            // and surprises the user when the result set
                            // changes shape. The details modal's tag
                            // cursor and the half-typed `gg` jump are
                            // dropped too: the new series almost
                            // certainly has a different tag set, so the
                            // old cursor index points at nothing useful.
                            self.legend.hidden = vec![false; count];
                            if self.legend.selected >= count {
                                self.legend.selected = 0;
                            }
                            self.legend.details_cursor = 0;
                            self.legend.pending_g = false;
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
}
