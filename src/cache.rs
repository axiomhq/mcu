//! On-disk discovery cache.
//!
//! Stored as JSON at `$XDG_CACHE_HOME/mcu/discovery.json` (or
//! `$HOME/.cache/mcu/discovery.json`). Holds the list of datasets and
//! per-dataset metric inventories so the app starts with usable state offline
//! and avoids re-fetching during a session.
//!
//! The last edited query is persisted alongside it as plain text in `query.mpl`
//! so the editor restores the previous session's buffer.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::axiom::{DashboardSummary, DatasetSummary, MetricInfo};

/// A routing decision for a single dataset: the edge URL to hit and, if known,
/// the `cloud.<region>.<provider>` identifier to put in the request body.
#[derive(Debug, Clone)]
pub struct EdgeRoute {
    pub url: String,
    pub deployment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheData {
    #[serde(default)]
    pub datasets: Vec<CachedDataset>,
    #[serde(default)]
    pub metrics_by_dataset: BTreeMap<String, CachedMetrics>,
    /// Per-metric tag inventory. Keyed by dataset, then metric. Populated by
    /// the metrics-info `/metrics/<m>/tags` endpoint and refreshed on demand
    /// when the user accepts a metric completion.
    #[serde(default)]
    pub tags_by_metric: BTreeMap<String, BTreeMap<String, CachedTags>>,
    /// Per-tag value inventory. Keyed by dataset, then metric, then tag.
    /// Populated by the metrics-info `/metrics/<m>/tags/<t>/values` endpoint
    /// when a tag is referenced in a query (e.g. `where host == ...`).
    #[serde(default)]
    pub tag_values_by_metric: BTreeMap<String, BTreeMap<String, BTreeMap<String, CachedTagValues>>>,
    /// Selected legend-label tag keys keyed by a stable hash of the
    /// normalized query text. Most specific source for the
    /// `legend_label_tags` default — same query identifies same chart.
    #[serde(default)]
    pub legend_tags_by_query_hash: BTreeMap<String, Vec<String>>,
    /// Selected legend-label tag keys keyed by `(dataset, metric)`. Used
    /// as fallback when no exact-query entry exists — related queries on
    /// the same metric tend to want the same labels.
    #[serde(default)]
    pub legend_tags_by_metric: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    /// Last-seen full dashboard listing from `GET /v2/dashboards`. Shown
    /// instantly when the user opens `:dash ls` while a background
    /// refresh fetches the live copy.
    #[serde(default)]
    pub dashboards: Option<CachedDashboardList>,
    /// Per-uid dashboard resources from `GET /v2/dashboards/uid/{uid}`,
    /// used to adopt a cached copy instantly while a background refresh
    /// fetches the live one.
    #[serde(default)]
    pub dashboards_by_uid: BTreeMap<String, CachedDashboard>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedDashboardList {
    pub fetched_at: i64,
    #[serde(default)]
    pub items: Vec<DashboardSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDashboard {
    pub fetched_at: i64,
    pub resource: DashboardSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDataset {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub edge_deployment: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedMetrics {
    pub fetched_at: i64,
    #[serde(default)]
    pub metrics: BTreeMap<String, MetricInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedTags {
    pub fetched_at: i64,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedTagValues {
    pub fetched_at: i64,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug)]
pub struct Cache {
    path: Option<PathBuf>,
    data: CacheData,
    fallback_base_url: String,
}

impl Cache {
    /// Load from the standard cache location. Missing or malformed files yield
    /// an empty cache and are silently replaced on the next save.
    pub fn load(fallback_base_url: String) -> Self {
        let path = default_path();
        let data = path
            .as_deref()
            .and_then(read_data_from_disk)
            .unwrap_or_default();
        Self {
            path,
            data,
            fallback_base_url,
        }
    }

    /// In-memory cache used by tests; never touches the filesystem.
    #[cfg(test)]
    pub fn in_memory(fallback_base_url: String) -> Self {
        Self {
            path: None,
            data: CacheData::default(),
            fallback_base_url,
        }
    }

    pub fn dataset_count(&self) -> usize {
        self.data.datasets.len()
    }

    pub fn dataset_names(&self) -> Vec<String> {
        self.data.datasets.iter().map(|d| d.name.clone()).collect()
    }

    /// Cached tag names for a specific `(dataset, metric)` pair. Returns an
    /// empty `Vec` when nothing is cached. Will feed tag completions in 10.x;
    /// Returns the cached tag-name list for `(dataset, metric)`, empty when
    /// nothing is cached. Consumed by the completion popup's `Tag` variant.
    pub fn tags_for(&self, dataset: &str, metric: &str) -> Vec<String> {
        self.data
            .tags_by_metric
            .get(dataset)
            .and_then(|m| m.get(metric))
            .map(|t| t.tags.clone())
            .unwrap_or_default()
    }

    /// `true` when we have *any* cached tag list (even empty) for the pair.
    /// Used to avoid re-fetching on every completion accept.
    pub fn has_tags(&self, dataset: &str, metric: &str) -> bool {
        self.data
            .tags_by_metric
            .get(dataset)
            .map(|m| m.contains_key(metric))
            .unwrap_or(false)
    }

    pub fn replace_tags(&mut self, dataset: &str, metric: &str, mut tags: Vec<String>) {
        tags.sort();
        tags.dedup();
        self.data
            .tags_by_metric
            .entry(dataset.to_string())
            .or_default()
            .insert(
                metric.to_string(),
                CachedTags {
                    fetched_at: unix_now(),
                    tags,
                },
            );
    }

    /// Cached values for a single tag on a `(dataset, metric)`. Empty when
    /// nothing is cached. Feeds tag-value completion.
    pub fn tag_values_for(&self, dataset: &str, metric: &str, tag: &str) -> Vec<String> {
        self.data
            .tag_values_by_metric
            .get(dataset)
            .and_then(|m| m.get(metric))
            .and_then(|t| t.get(tag))
            .map(|v| v.values.clone())
            .unwrap_or_default()
    }

    pub fn has_tag_values(&self, dataset: &str, metric: &str, tag: &str) -> bool {
        self.data
            .tag_values_by_metric
            .get(dataset)
            .and_then(|m| m.get(metric))
            .map(|t| t.contains_key(tag))
            .unwrap_or(false)
    }

    pub fn replace_tag_values(
        &mut self,
        dataset: &str,
        metric: &str,
        tag: &str,
        mut values: Vec<String>,
    ) {
        values.sort();
        values.dedup();
        self.data
            .tag_values_by_metric
            .entry(dataset.to_string())
            .or_default()
            .entry(metric.to_string())
            .or_default()
            .insert(
                tag.to_string(),
                CachedTagValues {
                    fetched_at: unix_now(),
                    values,
                },
            );
    }

    /// Cached metric names for a dataset; feeds completions.
    pub fn metric_names(&self, dataset: &str) -> Vec<String> {
        self.data
            .metrics_by_dataset
            .get(dataset)
            .map(|m| m.metrics.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Look up the edge route for a dataset that is already known.
    /// Returns `None` if the dataset isn't in the cache; callers should
    /// refresh and retry in that case.
    pub fn edge_route_for(&self, dataset: &str) -> Option<EdgeRoute> {
        let ds = self.data.datasets.iter().find(|d| d.name == dataset)?;
        Some(make_edge_route(
            ds.edge_deployment.as_deref(),
            &self.fallback_base_url,
        ))
    }

    /// Cached dashboard listing, if any has been fetched this session
    /// or persisted from a prior run.
    pub fn cached_dashboards(&self) -> Option<Vec<DashboardSummary>> {
        self.data.dashboards.as_ref().map(|d| d.items.clone())
    }

    /// Replace the cached dashboard listing with a fresh fetch.
    pub fn replace_dashboards(&mut self, items: Vec<DashboardSummary>) {
        self.data.dashboards = Some(CachedDashboardList {
            fetched_at: unix_now(),
            items,
        });
    }

    /// Cached full dashboard resource for `uid`, if any.
    pub fn cached_dashboard(&self, uid: &str) -> Option<DashboardSummary> {
        self.data
            .dashboards_by_uid
            .get(uid)
            .map(|c| c.resource.clone())
    }

    /// Insert or replace the cached resource for `uid`.
    pub fn replace_dashboard(&mut self, uid: &str, resource: DashboardSummary) {
        self.data.dashboards_by_uid.insert(
            uid.to_string(),
            CachedDashboard {
                fetched_at: unix_now(),
                resource,
            },
        );
    }

    /// Remove a cached dashboard resource (e.g. after `:dash rm <uid>`).
    pub fn forget_dashboard(&mut self, uid: &str) {
        self.data.dashboards_by_uid.remove(uid);
        if let Some(list) = self.data.dashboards.as_mut() {
            list.items.retain(|d| d.uid != uid);
        }
    }

    pub fn replace_datasets(&mut self, datasets: Vec<DatasetSummary>) {
        let now = unix_now();
        self.data.datasets = datasets
            .into_iter()
            .map(|d| CachedDataset {
                name: d.name,
                description: d.description,
                edge_deployment: d.edge_deployment,
                kind: d.kind,
                fetched_at: now,
            })
            .collect();
    }

    /// Two-step fallback for the default legend-label tags shown when a
    /// query produces results:
    ///
    /// 1. Exact-query match (keyed by normalized AST hash).
    /// 2. Same dataset+metric match.
    ///
    /// Returns an empty vec if neither is cached.
    pub fn resolve_legend_tags(
        &self,
        query_hash: &str,
        dataset: &str,
        metric: &str,
    ) -> Vec<String> {
        if let Some(tags) = self.data.legend_tags_by_query_hash.get(query_hash) {
            return tags.clone();
        }
        self.data
            .legend_tags_by_metric
            .get(dataset)
            .and_then(|m| m.get(metric))
            .cloned()
            .unwrap_or_default()
    }

    /// Persist the user's tag selection at both fallback levels so a
    /// later run picks them up automatically. Empty `tags` clears both
    /// levels for the given keys (so the user can explicitly opt out).
    pub fn set_legend_tags(
        &mut self,
        query_hash: &str,
        dataset: &str,
        metric: &str,
        tags: Vec<String>,
    ) {
        if tags.is_empty() {
            self.data.legend_tags_by_query_hash.remove(query_hash);
            if let Some(per_metric) = self.data.legend_tags_by_metric.get_mut(dataset) {
                per_metric.remove(metric);
                if per_metric.is_empty() {
                    self.data.legend_tags_by_metric.remove(dataset);
                }
            }
            return;
        }
        self.data
            .legend_tags_by_query_hash
            .insert(query_hash.to_string(), tags.clone());
        self.data
            .legend_tags_by_metric
            .entry(dataset.to_string())
            .or_default()
            .insert(metric.to_string(), tags);
    }

    /// Persist legend tags keyed only by `(dataset, metric)`. Used
    /// from the dashboard tag picker where there's no editor
    /// query-hash to scope by — the tile's MPL is the source of
    /// truth, but its hash isn't the same as the editor's.
    pub fn set_legend_tags_for_metric(&mut self, dataset: &str, metric: &str, tags: Vec<String>) {
        if tags.is_empty() {
            if let Some(per_metric) = self.data.legend_tags_by_metric.get_mut(dataset) {
                per_metric.remove(metric);
                if per_metric.is_empty() {
                    self.data.legend_tags_by_metric.remove(dataset);
                }
            }
            return;
        }
        self.data
            .legend_tags_by_metric
            .entry(dataset.to_string())
            .or_default()
            .insert(metric.to_string(), tags);
    }

    pub fn replace_metrics(&mut self, dataset: &str, metrics: BTreeMap<String, MetricInfo>) {
        self.data.metrics_by_dataset.insert(
            dataset.to_string(),
            CachedMetrics {
                fetched_at: unix_now(),
                metrics,
            },
        );
    }

    /// Persist current state to disk. No-op when path is unset (tests).
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        atomic_write(path, &self.data)
    }

    /// Load the last persisted query text, if any. Missing/empty files yield `None`.
    pub fn load_query(&self) -> Option<String> {
        let path = self.query_path()?;
        let text = fs::read_to_string(&path).ok()?;
        if text.is_empty() { None } else { Some(text) }
    }

    /// Persist the current query text. No-op when path is unset (tests).
    pub fn save_query(&self, query: &str) -> Result<()> {
        let Some(path) = self.query_path() else {
            return Ok(());
        };
        atomic_write_text(&path, query)
    }

    /// Path used for the persisted query buffer.
    fn query_path(&self) -> Option<PathBuf> {
        self.path.as_ref().map(|p| p.with_file_name("query.mpl"))
    }

    #[cfg(test)]
    pub fn debug_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

fn default_path() -> Option<PathBuf> {
    // `etcetera` handles XDG_CACHE_HOME / ~/.cache on Linux,
    // ~/Library/Caches on macOS, and %LOCALAPPDATA% on Windows.
    use etcetera::BaseStrategy;
    let strategy = etcetera::choose_base_strategy().ok()?;
    Some(strategy.cache_dir().join("mcu").join("discovery.json"))
}

fn read_data_from_disk(p: &Path) -> Option<CacheData> {
    let text = fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

fn atomic_write(path: &Path, data: &CacheData) -> Result<()> {
    let json = serde_json::to_string_pretty(data).context("serializing cache")?;
    atomic_write_text(path, &json)
}

fn atomic_write_text(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cache path {:?} has no parent directory", path))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    // `NamedTempFile::new_in` creates a uniquely-named temp file in the
    // same directory as the target, so the subsequent `persist` is a
    // same-filesystem rename (atomic on POSIX). Collisions between
    // concurrent writers can't happen even if two mcu instances
    // race — each gets its own temp name.
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temp file in {}", parent.display()))?;
    tmp.write_all(contents.as_bytes())
        .with_context(|| format!("writing {}", tmp.path().display()))?;
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("flushing {}", tmp.path().display()))?;
    tmp.persist(path)
        .with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

fn make_edge_route(edge_deployment: Option<&str>, fallback: &str) -> EdgeRoute {
    if let Some(s) = edge_deployment
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "null")
        && let Some(region) = s.strip_prefix("cloud.")
    {
        return EdgeRoute {
            url: format!("https://{region}.edge.axiom.co"),
            deployment: Some(s.to_string()),
        };
    }
    EdgeRoute {
        url: fallback.to_string(),
        deployment: None,
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
