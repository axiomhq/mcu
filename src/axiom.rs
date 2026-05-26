//! Thin facade over [`axiom_rs`] for the endpoints mcu needs.
//!
//! The bulk of the wire protocol — dashboards (v2), metrics-info, MPL queries
//! — lives in `axiom-rs` upstream (`../axiom-rs`). This module:
//!
//! - re-exports SDK types under their historical mcu names so the
//!   rest of the crate doesn't care about the swap,
//! - keeps a local [`DatasetSummary`] because the SDK's [`axiom_rs::datasets::Dataset`]
//!   exposes a different subset of fields (no `edgeDeployment`/`kind`),
//! - wraps [`axiom_rs::Client`] with a thin [`Client`] that exposes the
//!   small set of methods our event loop calls, mapping `axiom_rs::Error`
//!   to `anyhow::Error` at the boundary.
//!
//! Edge URL resolution and caching still live in `crate::cache`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::config::Deployment;

pub use axiom_rs::dashboards::{
    Chart, ChartBase, CreateOptions, Dashboard as DashboardSummary, DashboardDocument,
    DashboardWriteResponse, DashboardWriteStatus, KnownChart, LayoutItem, UpsertOptions,
};
pub use axiom_rs::metrics::{MetricInfo, MetricsQueryResponse, MetricsSeries};

// Used by tests that round-trip the upsert request body. Re-exported under
// the historical name so the test surface doesn't drift away from the rest
// of the crate.
#[cfg(test)]
pub use axiom_rs::dashboards::UpsertRequest as DashboardUpsertRequest;

/// Wire shape returned by `GET /v1/datasets`. Distinct from
/// [`axiom_rs::datasets::Dataset`] because we surface `edgeDeployment` and
/// `kind` (used by `crate::cache` to route queries to the right edge URL),
/// and we don't need the `who`/`created` fields the SDK does require.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetSummary {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "edgeDeployment", default)]
    pub edge_deployment: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// Convenience accessor matching the historical `DashboardSummary::name`
/// shape (returns `&str`, falling back to `"(unnamed)"`). The SDK's own
/// `Dashboard::name` returns `Option<&str>`; we keep the fallback as a
/// helper extension so call-site code stays terse.
pub trait DashboardSummaryExt {
    fn name_or_unnamed(&self) -> &str;
}

impl DashboardSummaryExt for DashboardSummary {
    fn name_or_unnamed(&self) -> &str {
        self.dashboard.name.as_deref().unwrap_or("(unnamed)")
    }
}

/// `Chart::base()` returns `Option<&ChartBase>` because the SDK can
/// carry forward-compat `Chart::Unknown` variants. mcu only
/// ever round-trips charts it constructed itself via
/// `VizKind::to_chart`, which always builds `Chart::Known`, so the
/// invariant holds. If a future server response contains an Unknown
/// chart, code paths that hit `known_base*` will panic with a
/// diagnostic message rather than silently misrender.
pub trait ChartKnownExt {
    fn known_base(&self) -> &ChartBase;
    fn known_base_mut(&mut self) -> &mut ChartBase;
}

impl ChartKnownExt for Chart {
    fn known_base(&self) -> &ChartBase {
        self.base()
            .expect("mcu expects Chart::Known; got Chart::Unknown")
    }

    fn known_base_mut(&mut self) -> &mut ChartBase {
        self.base_mut()
            .expect("mcu expects Chart::Known; got Chart::Unknown")
    }
}

/// Thin wrapper around [`axiom_rs::Client`] plus a raw [`reqwest::Client`]
/// for the one endpoint (`GET /v1/datasets`) where we need fields the SDK
/// doesn't surface.
///
/// The SDK builds its `edge_http` at construction time from the configured
/// edge URL — so a single `axiom_rs::Client` can only ever talk to one
/// edge. We route metrics-info and `_mpl` query traffic at a per-dataset
/// edge URL (resolved via `cache::EdgeRoute`), so we lazily build and
/// cache one SDK client per resolved edge URL in `edge_clients`. `inner`
/// stays as the control-plane client used for the dashboards/datasets
/// endpoints that don't depend on the edge.
#[derive(Debug, Clone)]
pub struct Client {
    inner: axiom_rs::Client,
    /// Raw HTTP client kept solely for `list_datasets`. Other endpoints go
    /// through `inner` or through a per-edge client from `edge_clients`.
    http: reqwest::Client,
    base_url: String,
    token: String,
    org_id: String,
    /// Cache of edge-bound SDK clients, keyed by the resolved edge URL
    /// (e.g. `https://eu-central-1.aws.edge.axiom.co`). Built lazily by
    /// [`Client::edge_client`].
    edge_clients: Arc<Mutex<HashMap<String, axiom_rs::Client>>>,
}

impl Client {
    pub fn new(deployment: &Deployment) -> Result<Self> {
        let base_url = deployment.url.trim_end_matches('/').to_string();
        let token = deployment.token.clone();
        let org_id = deployment.org_id.clone();

        let mut builder = axiom_rs::Client::builder()
            .no_env()
            .with_url(&base_url)
            .with_token(&token);
        if !org_id.is_empty() {
            builder = builder.with_org_id(&org_id);
        }
        let inner = builder.build().context("building axiom-rs client")?;

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("mcu/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;

        Ok(Self {
            inner,
            http,
            base_url,
            token,
            org_id,
            edge_clients: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Return an SDK client whose `edge_http` is bound to `edge_url`. Used
    /// for the metrics-info family and `_mpl` query endpoints, which the
    /// SDK serves off its edge HTTP client. Cached per URL so we don't
    /// rebuild a `reqwest::Client` on every call.
    ///
    /// When `edge_url` is empty we fall back to the control-plane client —
    /// matching the behaviour of `cache::EdgeRoute` when a dataset has no
    /// `edgeDeployment` (the URL there is the deployment base URL anyway).
    fn edge_client(&self, edge_url: &str) -> Result<axiom_rs::Client> {
        let key = edge_url.trim_end_matches('/').to_string();
        if key.is_empty() {
            return Ok(self.inner.clone());
        }
        if let Some(c) = self.edge_clients.lock().unwrap().get(&key) {
            return Ok(c.clone());
        }
        let mut builder = axiom_rs::Client::builder()
            .no_env()
            .with_url(&self.base_url)
            .with_token(&self.token)
            .with_edge_url(&key);
        if !self.org_id.is_empty() {
            builder = builder.with_org_id(&self.org_id);
        }
        let client = builder
            .build()
            .with_context(|| format!("building axiom-rs client for edge {key}"))?;
        self.edge_clients
            .lock()
            .unwrap()
            .insert(key, client.clone());
        Ok(client)
    }

    /// List datasets visible to the configured token, including the
    /// `edgeDeployment` and `kind` fields the SDK currently omits.
    pub async fn list_datasets(&self) -> Result<Vec<DatasetSummary>> {
        let url = format!("{}/v1/datasets", self.base_url);
        let mut req = self.http.get(&url).bearer_auth(&self.token);
        if !self.org_id.is_empty() {
            req = req.header("X-Axiom-Org-Id", &self.org_id);
        }
        let resp = req
            .header("Accept", "application/json")
            .send()
            .await
            .context("sending /v1/datasets request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "axiom /v1/datasets {} — {}",
                status.as_u16(),
                snippet(&body, 200)
            ));
        }
        serde_json::from_str::<Vec<DatasetSummary>>(&body)
            .with_context(|| format!("decoding /v1/datasets: {}", snippet(&body, 200)))
    }

    pub async fn create_dashboard(
        &self,
        doc: &DashboardDocument,
        uid: Option<&str>,
        message: Option<&str>,
    ) -> Result<DashboardWriteResponse> {
        let opts = CreateOptions {
            uid: uid.map(str::to_owned),
            message: message.map(str::to_owned),
        };
        self.inner
            .dashboards()
            .create(doc, opts)
            .await
            .map_err(map_axiom_err)
    }

    pub async fn put_dashboard(
        &self,
        uid: &str,
        doc: &DashboardDocument,
        expected_version: Option<i64>,
        overwrite: bool,
        message: Option<&str>,
    ) -> Result<DashboardWriteResponse> {
        let opts = UpsertOptions {
            expected_version,
            overwrite,
            message: message.map(str::to_owned),
        };
        self.inner
            .dashboards()
            .put(uid, doc, opts)
            .await
            .map_err(map_axiom_err)
    }

    pub async fn delete_dashboard(&self, uid: &str) -> Result<()> {
        self.inner
            .dashboards()
            .delete(uid)
            .await
            .map_err(map_axiom_err)
    }

    pub async fn get_dashboard(&self, uid: &str) -> Result<DashboardSummary> {
        self.inner
            .dashboards()
            .get(uid)
            .await
            .map_err(map_axiom_err)
    }

    pub async fn list_dashboards(&self) -> Result<Vec<DashboardSummary>> {
        self.inner.dashboards().list().await.map_err(map_axiom_err)
    }

    pub async fn list_metrics(
        &self,
        edge_url: &str,
        dataset: &str,
        start: &str,
        end: &str,
    ) -> Result<BTreeMap<String, MetricInfo>> {
        let (start, end) = parse_range(start, end)?;
        self.edge_client(edge_url)?
            .metrics()
            .list(dataset, start, end)
            .await
            .map_err(map_axiom_err)
    }

    pub async fn list_metric_tags(
        &self,
        edge_url: &str,
        dataset: &str,
        metric: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<String>> {
        let (start, end) = parse_range(start, end)?;
        self.edge_client(edge_url)?
            .metrics()
            .tags(dataset, metric, start, end)
            .await
            .map_err(map_axiom_err)
    }

    pub async fn list_metric_tag_values(
        &self,
        edge_url: &str,
        dataset: &str,
        metric: &str,
        tag: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<String>> {
        let (start, end) = parse_range(start, end)?;
        self.edge_client(edge_url)?
            .metrics()
            .tag_values(dataset, metric, tag, start, end)
            .await
            .map_err(map_axiom_err)
    }

    pub async fn query_mpl(
        &self,
        edge_url: &str,
        edge_deployment: Option<&str>,
        mpl: &str,
        start: &str,
        end: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<MetricsQueryResponse> {
        let (start, end) = parse_range(start, end)?;
        let opts = axiom_rs::metrics::MplQueryOptions {
            edge_deployment: edge_deployment.map(ToString::to_string),
            params: params.clone(),
        };
        self.edge_client(edge_url)?
            .metrics()
            .query(mpl, start, end, opts)
            .await
            .map_err(map_axiom_err)
    }
}

/// Map `axiom_rs::Error` to `anyhow::Error`, preserving the typed
/// `DashboardVersionConflict` so callers can match on the message prefix
/// without losing context.
fn map_axiom_err(err: axiom_rs::Error) -> anyhow::Error {
    anyhow::Error::new(err)
}

/// Parse the time-range pair we get from `cache::session_range()` /
/// dashboard `timeWindowStart`-style strings.
///
/// The SDK signature takes `chrono::DateTime<Utc>`, so anything relative
/// (`now`, `now-7d`, `now+15m`) has to be resolved client-side before we
/// can call into it. We accept:
///
/// - RFC3339 timestamps (`2024-05-01T00:00:00Z`),
/// - `now`,
/// - `now[+-]<N><unit>` with units `ns`/`us`/`ms`/`s`/`m`/`h`/`d`/`w`,
///   matching the relative grammar Axiom dashboards use.
///
/// The `qr-` prefix used by the dashboard schema is already stripped
/// upstream by `app::helpers::normalize_time_expr`.
fn parse_range(
    start: &str,
    end: &str,
) -> Result<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> {
    let now = chrono::Utc::now();
    let s = parse_time_expr(start, now).with_context(|| format!("parsing start time {start:?}"))?;
    let e = parse_time_expr(end, now).with_context(|| format!("parsing end time {end:?}"))?;
    Ok((s, e))
}

/// Resolve a single Axiom-style time expression against `now`.
fn parse_time_expr(
    expr: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<chrono::DateTime<chrono::Utc>> {
    let s = expr.trim();
    if s.is_empty() {
        return Err(anyhow!("empty time expression"));
    }
    // Relative: `now`, `now-<dur>`, `now+<dur>`.
    if let Some(rest) = s.strip_prefix("now") {
        if rest.is_empty() {
            return Ok(now);
        }
        let (sign, dur) = match rest.as_bytes().first() {
            Some(b'-') => (-1i64, &rest[1..]),
            Some(b'+') => (1i64, &rest[1..]),
            _ => return Err(anyhow!("expected `+` or `-` after `now`, got {rest:?}")),
        };
        let delta = parse_duration(dur)?;
        return now
            .checked_add_signed(chrono::Duration::nanoseconds(sign * delta))
            .ok_or_else(|| anyhow!("time expression overflows: {s:?}"));
    }
    // Absolute RFC3339.
    Ok(chrono::DateTime::parse_from_rfc3339(s)?.with_timezone(&chrono::Utc))
}

/// Parse an Axiom-style duration literal (`7d`, `15m`, `500ms`, ...) into
/// nanoseconds. Single `<number><unit>` token; no chained components.
fn parse_duration(s: &str) -> Result<i64> {
    if s.is_empty() {
        return Err(anyhow!("empty duration"));
    }
    let split = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| anyhow!("duration {s:?} missing unit"))?;
    if split == 0 {
        return Err(anyhow!("duration {s:?} missing leading number"));
    }
    let (num, unit) = s.split_at(split);
    let n: i64 = num
        .parse()
        .with_context(|| format!("duration {s:?} number {num:?} out of range"))?;
    let nanos_per_unit: i64 = match unit {
        "ns" => 1,
        "us" | "µs" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60 * 1_000_000_000,
        "h" => 60 * 60 * 1_000_000_000,
        "d" => 24 * 60 * 60 * 1_000_000_000,
        "w" => 7 * 24 * 60 * 60 * 1_000_000_000,
        other => return Err(anyhow!("duration {s:?} unknown unit {other:?}")),
    };
    n.checked_mul(nanos_per_unit)
        .ok_or_else(|| anyhow!("duration {s:?} overflows i64 nanoseconds"))
}

/// Truncate `s` to at most `max` characters, appending `…` when cut.
/// Kept here because [`Client::list_datasets`] is still hand-rolled.
fn snippet(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let cut: String = trimmed.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests;
