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
            .and_then(|r| r.dashboard.charts.iter().find(|c| c.known_base().id == id))
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
                        error: Some(format!("MPL: {e}")),
                        ..Default::default()
                    },
                );
                return;
            }
        };
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => return self.set_error(format!("tile fetch: {e}")),
        };
        // Mark the tile busy in-place so the chrome flips to the spinner pip.
        let entry = self.tile_results.entry(id.clone()).or_default();
        entry.busy = true;
        entry.error = None;
        let cache = self.cache.clone();
        let params = self.params.cli.clone();
        let (start, end) = self.active_time_range();
        let tx = self.events_tx.clone();
        let chart_id = id.clone();
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
        self.status = format!("refetching tile {id}…");
    }

    pub(in crate::app) fn run_query(&mut self) {
        if self.busy {
            self.status = "already busy".to_string();
            return;
        }
        let mpl = self.query_text();
        if mpl.trim().is_empty() {
            self.status = "empty query".to_string();
            return;
        }
        // The MetricsDB server resolves `$__interval` and friends from the
        // request's time window, so we send the buffer verbatim.
        let (dataset, metric) = match mpl::extract_dataset_metric(&mpl) {
            Ok(dm) => dm,
            Err(e) => {
                self.status = format!("MPL error: {e}");
                return;
            }
        };
        // Snapshot the query identity so post-result toggles persist under
        // stable keys even if the user has since edited the buffer.
        self.last_query_context = Some(QueryContext {
            hash: mpl::query_hash(&mpl, &self.params.system),
            dataset: dataset.clone(),
            metric,
        });
        // Refuse to send while the buffer carries error-level diagnostics.
        // Recompute first so the check sees the latest buffer state.
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
        let params = self.params.cli.clone();
        let (start, end) = self.active_time_range();
        self.runtime.spawn(async move {
            let result =
                run_query_task(&cache, &client, &dataset, &mpl, &start, &end, &params).await;
            let _ = tx.send(AppEvent::QueryFinished { id, result });
        });
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
    pub(in crate::app) fn fetch_prepare(
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
}
