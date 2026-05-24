//! Async fetch + event handling. Every method here either kicks off
//! a background task (`fetch_*`, `run_query`, `run_tile_queries`,
//! `fetch_dashboard_by_uid`) or consumes the events those tasks
//! produce (`drain_events` + the giant `handle_event` match).
//!
//! Keeping the lifecycle in one place makes it easy to reason about
//! the cache writes that happen on success and the error surfacing
//! that happens on failure.

use super::*;

impl App {

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

    pub(super) fn fetch_datasets(&mut self) {
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

    pub(super) fn fetch_metrics_for_current_query(&mut self) {
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
    pub(super) fn prefetch_tag_values_from_query(&mut self, mpl: &str) {
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

    pub(super) fn ensure_client(&mut self) -> anyhow::Result<&AxiomClient> {
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
    pub(super) fn fetch_prepare(
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

    pub(super) fn run_query(&mut self) {
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
    pub(super) fn run_tile_queries(&mut self) {
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
            AppEvent::MetricsFetched { dataset, result } => {
                self.busy = false;
                match result {
                    Ok(metrics) => {
                        self.status =
                            format!("loaded {} metric(s) for `{dataset}`", metrics.len())
                    }
                    Err(e) => self.set_error(format!("metrics error for `{dataset}`: {e}")),
                }
            }
            // Background prefetches — don't clobber foreground status while
            // a query is in flight.
            AppEvent::TagsFetched { dataset, metric, result } if !self.busy => {
                self.status = match result {
                    Ok(tags) => {
                        format!("loaded {} tag(s) for `{dataset}:{metric}`", tags.len())
                    }
                    Err(e) => format!("tags error for `{dataset}:{metric}`: {e}"),
                };
            }
            AppEvent::TagsFetched { .. } => {}
            AppEvent::TagValuesFetched { dataset, metric, tag, result } if !self.busy => {
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
}
