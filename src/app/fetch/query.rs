use super::*;

impl App {
    /// `R` shortcut in the dashboard pane: refetch just the focused
    /// tile, dispatching to the MPL or APL endpoint based on the
    /// tile's language. Tiles with no query (Note/Spacer/Empty)
    /// surface a status hint.
    pub fn run_focused_tile_query(&mut self) {
        let Some(id) = self.current_chart_id() else {
            self.status = "no tile selected".to_string();
            return;
        };
        let query = self
            .loaded_dashboard
            .as_ref()
            .and_then(|r| {
                r.dashboard
                    .charts
                    .iter()
                    .find(|c| c.base().is_some_and(|b| b.id == id))
            })
            .map(crate::dashboard::extract_query);
        let query = match query {
            Some(crate::dashboard::Query::Mpl(s)) if !s.trim().is_empty() => {
                crate::dashboard::Query::Mpl(s)
            }
            Some(crate::dashboard::Query::Apl(s)) if !s.trim().is_empty() => {
                crate::dashboard::Query::Apl(s)
            }
            _ => {
                self.status = format!("tile {id}: no query to rerun");
                return;
            }
        };
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => return self.set_error(format!("tile fetch: {e}")),
        };
        // Mark the tile busy in-place so the chrome flips to the spinner pip.
        // Clear the previous run's `elapsed` so the border stops
        // displaying a stale duration over the spinner, and stamp
        // `started_at` so the post-result handler can compute the
        // new elapsed.
        let entry = self.tile_results.entry(id.clone()).or_default();
        entry.busy = true;
        entry.error = None;
        entry.elapsed = None;
        entry.started_at = Some(std::time::Instant::now());
        let cache = self.cache.clone();
        let params = self.params.cli.clone();
        let (start, end) = self.active_time_range();
        let tx = self.events_tx.clone();
        let chart_id = id.clone();
        let epoch = self.tile_query_epoch;
        match query {
            crate::dashboard::Query::Mpl(mpl) => {
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
                self.runtime.spawn(async move {
                    let result =
                        run_query_task(&cache, &client, &dataset, &mpl, &start, &end, &params)
                            .await;
                    let _ = tx.send(AppEvent::TileQueryFinished {
                        chart_id,
                        epoch,
                        result,
                    });
                });
            }
            crate::dashboard::Query::Apl(apl) => {
                self.runtime.spawn(async move {
                    let result = run_apl_query_task(&client, &apl, &start, &end).await;
                    let _ = tx.send(AppEvent::TileAplFinished {
                        chart_id,
                        epoch,
                        result,
                    });
                });
            }
            crate::dashboard::Query::Empty => unreachable!("filtered above"),
        }
        self.status = format!("refetching tile {id}…");
    }

    /// Standalone-buffer APL execution path. Mirrors
    /// [`Self::run_query`]'s busy / id / status bookkeeping but
    /// skips MPL-only steps: there's no local dataset extraction
    /// (APL queries embed the dataset in their text), no MPL
    /// analyzer (already gated upstream), and no tag prefetch
    /// (the prefetcher only understands MPL `metric:agg` shape).
    fn run_apl_query(&mut self, apl: String) {
        // No `last_query_context` snapshot: the APL endpoint
        // doesn't share the `dataset / metric` identity that the
        // post-result toggle bookkeeping in `QueryContext` keys
        // off, so leaving the slot at its prior value is correct
        // (clearing it would invalidate unrelated cached state).
        let client = match self.ensure_client() {
            Ok(c) => c.clone(),
            Err(e) => {
                self.status = format!("config error: {e}");
                return;
            }
        };
        self.last_query_id = self.last_query_id.wrapping_add(1);
        let id = self.last_query_id;
        self.busy = true;
        self.status = "running APL query…".to_string();
        self.persist_query();
        let tx = self.events_tx.clone();
        let (start, end) = self.active_time_range();
        self.runtime.spawn(async move {
            let result = run_apl_query_task(&client, &apl, &start, &end).await;
            let _ = tx.send(AppEvent::AplQueryFinished { id, result });
        });
    }

    pub(in crate::app) fn run_query(&mut self) {
        if self.busy {
            self.status = "already busy".to_string();
            return;
        }
        let text = self.query_text();
        if text.trim().is_empty() {
            self.status = "empty query".to_string();
            return;
        }
        // Language-aware dispatch. The MPL path needs dataset / tag
        // resolution and the local analyzer; the APL path bypasses
        // all of that and hands the buffer straight to the server.
        if self.active_lang() == crate::dashboard::Lang::Apl {
            self.run_apl_query(text);
            return;
        }
        let mpl = text;
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
            let cfg = self.resolve_config()?;
            let (_name, dep) = cfg.select(self.deployment_override.as_deref())?;
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
