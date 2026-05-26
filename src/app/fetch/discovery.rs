use super::*;

impl App {
    pub(in crate::app) fn fetch_datasets(&mut self) {
        let Some((client, tx, cache)) = self.fetch_prepare(Some("fetching datasets…".to_string()))
        else {
            return;
        };
        self.runtime.spawn(async move {
            let result = client.list_datasets().await;
            if let Ok(datasets) = &result {
                cache_save_with(&cache, |c| c.replace_datasets(datasets.clone()));
            }
            let _ = tx.send(AppEvent::DatasetsFetched(result));
        });
    }

    pub(in crate::app) fn fetch_metrics_for_current_query(&mut self) {
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
            let mut route = match resolve_route(&cache, &client, &dataset).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(AppEvent::MetricsFetched {
                        dataset,
                        result: Err(e),
                    });
                    return;
                }
            };
            let mut refreshed = false;
            let result = loop {
                let r = client
                    .list_metrics(&route.url, &dataset, &start, &end)
                    .await;
                match r {
                    Err(e) if !refreshed && is_axiom_404(&e) => {
                        refreshed = true;
                        match refresh_dataset_route(&cache, &client, &dataset).await {
                            Ok(r) => route = r,
                            Err(_) => break Err(e),
                        }
                    }
                    other => break other,
                }
            };
            if let Ok(metrics) = &result {
                cache_save_with(&cache, |c| c.replace_metrics(&dataset, metrics.clone()));
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
            let mut route = match resolve_route(&cache, &client, &dataset).await {
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
            let mut refreshed = false;
            let result = loop {
                let r = client
                    .list_metric_tags(&route.url, &dataset, &metric, &start, &end)
                    .await;
                match r {
                    Err(e) if !refreshed && is_axiom_404(&e) => {
                        refreshed = true;
                        match refresh_dataset_route(&cache, &client, &dataset).await {
                            Ok(r) => route = r,
                            Err(_) => break Err(e),
                        }
                    }
                    other => break other,
                }
            };
            if let Ok(tags) = &result {
                cache_save_with(&cache, |c| c.replace_tags(&dataset, &metric, tags.clone()));
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
            let mut route = match resolve_route(&cache, &client, &dataset).await {
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
            let mut refreshed = false;
            let result = loop {
                let r = client
                    .list_metric_tag_values(&route.url, &dataset, &metric, &tag, &start, &end)
                    .await;
                match r {
                    Err(e) if !refreshed && is_axiom_404(&e) => {
                        refreshed = true;
                        match refresh_dataset_route(&cache, &client, &dataset).await {
                            Ok(r) => route = r,
                            Err(_) => break Err(e),
                        }
                    }
                    other => break other,
                }
            };
            if let Ok(values) = &result {
                cache_save_with(&cache, |c| {
                    c.replace_tag_values(&dataset, &metric, &tag, values.clone())
                });
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
}
