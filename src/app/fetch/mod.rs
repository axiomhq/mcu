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

impl App {
    /// Drain background events and apply them to app state.
    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.events_rx.try_recv() {
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
                        // Editor has unsaved work — don't clobber it.
                        // Refresh just the resource metadata so saves
                        // round-trip against the latest version.
                        self.loaded_dashboard = Some(resource);
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
                    }
                    Err(e) => {
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
                // deleted, dashboard cleared without a re-fetch). In
                // that case skip the update so we don't resurrect it.
                let Some(entry) = self.tile_results.get_mut(&chart_id) else {
                    return;
                };
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
