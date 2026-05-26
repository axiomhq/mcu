use super::*;

impl App {
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
            Err(e) => return self.set_error(format!("config error: {e}")),
        };
        // Snappy path: serve cache + refresh in background (Refreshed event).
        // Cold path: foreground fetch (Opened event, sets busy).
        let cached = self.cache.read().unwrap().cached_dashboard(&uid);
        let event_ctor: fn(_, _) -> AppEvent = match cached {
            Some(resource) => {
                use crate::axiom::DashboardSummaryExt;
                let name = resource.name_or_unnamed().to_string();
                self.adopt_dashboard(uid.clone(), resource);
                self.status = format!("loaded `{name}` (cached, refreshing…)");
                |uid, result| AppEvent::DashboardRefreshed { uid, result }
            }
            None => {
                self.busy = true;
                self.status = format!("fetching dashboard {uid}…");
                |uid, result| AppEvent::DashboardOpened { uid, result }
            }
        };
        let tx = self.events_tx.clone();
        let cache = self.cache.clone();
        let uid_for_task = uid.clone();
        self.runtime.spawn(async move {
            let result = client.get_dashboard(&uid_for_task).await;
            if let Ok(resource) = &result {
                cache_save_with(&cache, |c| {
                    c.replace_dashboard(&uid_for_task, resource.clone())
                });
            }
            let _ = tx.send(event_ctor(uid_for_task, result));
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
    /// Stale-result protection: bumping `tile_query_epoch` invalidates
    /// every in-flight task spawned before this call, so a slow result
    /// from the previous dashboard can't overwrite a fresh tile that
    /// happens to share an id (`c1`, `c2`, etc. are typical defaults).
    /// The `tile_results.clear()` below covers the local map; the
    /// epoch covers the in-flight tasks that haven't returned yet.
    pub(in crate::app) fn run_tile_queries(&mut self) {
        self.tile_query_epoch = self.tile_query_epoch.wrapping_add(1);
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
                Some((c.known_base().id.clone(), mpl))
            })
            .collect();
        if charts.is_empty() {
            return;
        }
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => return self.set_error(format!("tile fetch: {e}")),
        };
        let cache = self.cache.clone();
        let params = self.params.cli.clone();
        let (start, end) = self.active_time_range();
        for (chart_id, mpl) in charts {
            // Initial busy state — grid renderer reads this to show a “loading…” hint.
            self.tile_results.insert(
                chart_id.clone(),
                TileQueryResult {
                    busy: true,
                    ..Default::default()
                },
            );
            let dataset = match mpl::extract_dataset_metric(&mpl) {
                Ok((d, _)) => d,
                Err(e) => {
                    self.tile_results.insert(
                        chart_id.clone(),
                        TileQueryResult {
                            error: Some(format!("MPL: {e}")),
                            ..Default::default()
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
            let epoch = self.tile_query_epoch;
            self.runtime.spawn(async move {
                let result =
                    run_query_task(&cache, &client, &dataset, &mpl, &start, &end, &params).await;
                let _ = tx.send(AppEvent::TileQueryFinished {
                    chart_id,
                    epoch,
                    result,
                });
            });
        }
    }
}
