//! Minimal Axiom REST client.
//!
//! Endpoints used:
//!
//! - `GET {base_url}/v1/datasets`
//!   Headers: `Authorization: Bearer`, `X-Axiom-Org-Id`.
//!   Returns dataset objects with `name`, `edgeDeployment`, and `kind`.
//!
//! - `GET {edge_url}/v1/query/metrics/info/datasets/{ds}/metrics?start=…&end=…`
//!   Headers: `Authorization: Bearer`, `X-Axiom-Org-Id`,
//!   `Accept: application/vnd.metrics-info.v2+json`.
//!   Returns `{ "<metric>": { "type": "...", "temporality": "...", "unit": null }, ... }`.
//!
//! - `POST {edge_url}/v1/query/_mpl`
//!   Headers: `Authorization: Bearer`, `X-Axiom-Org-Id`,
//!   `Content-Type: application/json`, `Accept: application/json+metrics.v2`.
//!   Body: `{"apl": "<mpl>", "startTime": "…", "endTime": "…",
//!   "queryEdgeDeployment": "cloud.<region>.<provider>"}`.
//!   Note: the field is literally named `apl` but contains MPL.
//!
//! Edge URL resolution and caching live in `crate::cache`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::Deployment;

/// Wire representation of `DashboardResource` from the v2 API. Step 17
/// will model the nested `DashboardDocument` (charts, layout,
/// timeWindow) properly; today we only decode what the picker shows.
///
/// `uid` is the path-friendly id used by `GET /v2/dashboards/uid/{uid}`,
/// `PUT`, and `DELETE`. The picker keys its selection on `uid`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DashboardSummary {
    pub uid: String,
    /// Internal numeric id (also stringified). Distinct from `uid`; the
    /// picker shows it only as a debug breadcrumb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)]
    pub id: Option<String>,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)]
    pub updated_at: Option<String>,
    #[serde(rename = "updatedBy", default, skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)]
    pub updated_by: Option<String>,
    /// Server-assigned monotonic version. Required as `version` on the
    /// next PUT unless `overwrite=true` is sent; missing here means the
    /// resource hasn't been persisted yet (hand-authored file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    /// Nested `DashboardDocument`. Step 17b decodes/encodes the
    /// charts/layout/time-window keys we know about; everything else
    /// passes through verbatim via `DashboardDocument.extras`.
    #[serde(default)]
    pub dashboard: DashboardDocument,
}

/// Request body for `POST /v2/dashboards` and `PUT /v2/dashboards/uid/{uid}`.
/// `version` is required for updates when `overwrite=false`; the server
/// returns `412 Precondition Failed` otherwise.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardUpsertRequest<'a> {
    pub dashboard: &'a DashboardDocument,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub overwrite: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<&'a str>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Response body for create + update. `status` distinguishes the two
/// even though both endpoints share the response shape.
#[derive(Debug, Clone, Deserialize)]
pub struct DashboardWriteResponse {
    pub status: DashboardWriteStatus,
    #[allow(dead_code)]
    #[serde(default)]
    pub overwritten: Option<bool>,
    pub dashboard: DashboardSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DashboardWriteStatus {
    Created,
    Updated,
}

/// Error body returned by 400/409/412 from the dashboard endpoints.
/// `current_version` is populated by 412 specifically so the client
/// can show “you have v3, server is at v5”.
#[derive(Debug, Clone, Deserialize)]
pub struct DashboardError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(rename = "currentVersion", default)]
    pub current_version: Option<i64>,
    #[allow(dead_code)]
    #[serde(default)]
    pub uid: Option<String>,
}

impl DashboardSummary {
    /// Top-level name with a sensible fallback for malformed records.
    pub fn name(&self) -> &str {
        self.dashboard.name.as_deref().unwrap_or("(unnamed)")
    }

    /// Optional one-line description.
    pub fn description(&self) -> Option<&str> {
        self.dashboard.description.as_deref()
    }
}

/// Decoded `DashboardDocument` from the v2 schema. Carries the fields
/// the TUI knows how to do something with (name, description, charts,
/// layout, time window) plus an `extras` bucket that captures every
/// other JSON key under `flatten` — so when step 17c serialises a
/// dashboard back, unmodelled fields (`refreshTime`, `schemaVersion`,
/// `against`, `owner`, `uid`, …) survive untouched.
///
/// The server's spec is `additionalProperties: false` on `Dashboard`,
/// which means we must round-trip exactly what came in or PUT will
/// reject. The `extras` bucket is the safety net for that.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DashboardDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub charts: Vec<Chart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layout: Vec<LayoutItem>,
    #[serde(
        rename = "timeWindowStart",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub time_window_start: Option<String>,
    #[serde(
        rename = "timeWindowEnd",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub time_window_end: Option<String>,
    /// Every other field the server returned. Preserved verbatim for
    /// round-tripping. Includes `owner`, `refreshTime`, `schemaVersion`,
    /// `against`, `againstTimestamp`, `uid`, …
    #[serde(flatten)]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

/// One chart from a dashboard. The v2 schema is a `oneOf` discriminated
/// by a `type` string with these exact values:
/// `TimeSeries | Heatmap | LogStream | Pie | Scatter | Table | TopK |
/// Statistic | Note`. We tag with `type` and keep the rest of the
/// chart in `extras` because each variant has its own query shape we
/// don't fully model yet.
///
/// Note: this list intentionally diverges from our internal
/// `dashboard::VizKind`. The TUI has extras (`Bar`, `Area`, `Spacer`,
/// `MonitorList`) that don't exist server-side. Mapping between the
/// two lives in `dashboard::VizKind::from_chart`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum Chart {
    TimeSeries(ChartBase),
    Heatmap(ChartBase),
    LogStream(ChartBase),
    Pie(ChartBase),
    Scatter(ChartBase),
    Table(ChartBase),
    TopK(ChartBase),
    Statistic(ChartBase),
    Note(ChartBase),
}

impl Chart {
    pub fn base(&self) -> &ChartBase {
        match self {
            Chart::TimeSeries(b)
            | Chart::Heatmap(b)
            | Chart::LogStream(b)
            | Chart::Pie(b)
            | Chart::Scatter(b)
            | Chart::Table(b)
            | Chart::TopK(b)
            | Chart::Statistic(b)
            | Chart::Note(b) => b,
        }
    }

    /// Human-readable type name, exactly as it appears on the wire.
    pub fn type_str(&self) -> &'static str {
        match self {
            Chart::TimeSeries(_) => "TimeSeries",
            Chart::Heatmap(_) => "Heatmap",
            Chart::LogStream(_) => "LogStream",
            Chart::Pie(_) => "Pie",
            Chart::Scatter(_) => "Scatter",
            Chart::Table(_) => "Table",
            Chart::TopK(_) => "TopK",
            Chart::Statistic(_) => "Statistic",
            Chart::Note(_) => "Note",
        }
    }
}

/// Fields shared by every chart variant. The `query` shape differs per
/// variant (e.g. `TimeSeriesChartQuery` vs `SimpleChartQuery`) so we
/// stash it as raw JSON for now and parse it lazily when we render.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChartBase {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<serde_json::Value>,
    /// Anything else on this chart variant (e.g. `tableSettings`,
    /// `colorScheme`). Preserved verbatim for round-trip.
    #[serde(flatten)]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

/// Grid placement for a chart, keyed by the chart's id (`i`). The
/// server's coordinate system is 12 columns wide (`x` ∈ 0..=11) and
/// `y` can be `null` to auto-stack. We mirror this exactly so layout
/// PUT round-trips.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LayoutItem {
    pub i: String,
    pub x: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<u32>,
    pub w: u32,
    pub h: u32,
    #[serde(flatten)]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricInfo {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub temporality: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricsQueryResponse {
    #[serde(default)]
    pub series: Vec<MetricsSeries>,
    /// Trace ID for this query — surfaces in the status bar so users can
    /// correlate against server-side logs. Populated from the
    /// `x-axiom-trace-id` response header (or `traceparent`); not part of
    /// the JSON body, so serde leaves it `None` on decode.
    #[serde(skip)]
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricsSeries {
    pub metric: String,
    #[serde(default)]
    pub tags: std::collections::HashMap<String, String>,
    pub start: i64,
    pub resolution: u64,
    #[serde(default)]
    pub data: Vec<Option<f64>>,
}

#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    base_url: String,
    token: String,
    org_id: String,
}

impl Client {
    pub fn new(deployment: &Deployment) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("metrics-tui/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                base_url: deployment.url.trim_end_matches('/').to_string(),
                token: deployment.token.clone(),
                org_id: deployment.org_id.clone(),
            }),
        })
    }

    /// Attach auth + org headers. Callers must set their own `Accept` header;
    /// `reqwest::header()` *appends*, so a default Accept here would silently
    /// produce two values and let the server pick the wrong response shape.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut r = req.bearer_auth(&self.inner.token);
        if !self.inner.org_id.is_empty() {
            r = r.header("X-Axiom-Org-Id", &self.inner.org_id);
        }
        r
    }

    /// Shared GET-JSON path. Builds the request, sends it, reads the
    /// body once, surfaces non-2xx as a structured `axiom <label>` error
    /// (with body excerpt) and on success decodes the body as `T`.
    /// `label` shows up in both the transport-error context and the
    /// not-success/decode messages so users can tell endpoints apart.
    async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        accept: &str,
        label: &str,
    ) -> Result<T> {
        let resp = self
            .auth(self.inner.http.get(url))
            .header("Accept", accept)
            .send()
            .await
            .with_context(|| format!("sending {label} request"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "axiom {} {} — {}",
                label,
                status.as_u16(),
                snippet(&body, 200)
            ));
        }
        serde_json::from_str::<T>(&body)
            .with_context(|| format!("decoding {label}: {}", snippet(&body, 200)))
    }

    pub async fn list_datasets(&self) -> Result<Vec<DatasetSummary>> {
        let url = format!("{}/v1/datasets", self.inner.base_url);
        self.get_json(&url, "application/json", "/v1/datasets")
            .await
    }

    /// Create a new dashboard. Hits `POST /v2/dashboards`. Returns
    /// the server's write response, which includes the freshly-minted
    /// `DashboardResource` (with `uid`, `version`, audit fields).
    ///
    /// On `409 Conflict` the server returns a `DashboardError` whose
    /// `code` distinguishes the failure mode (e.g. uid collision).
    pub async fn create_dashboard(
        &self,
        doc: &DashboardDocument,
        uid: Option<&str>,
        message: Option<&str>,
    ) -> Result<DashboardWriteResponse> {
        let body = DashboardUpsertRequest {
            dashboard: doc,
            version: None,
            overwrite: false,
            uid,
            message,
        };
        self.send_upsert(
            "POST",
            format!("{}/v2/dashboards", self.inner.base_url),
            &body,
        )
        .await
    }

    /// Update a dashboard by uid via `PUT /v2/dashboards/uid/{uid}`.
    ///
    /// `expected_version` is the version we loaded; pass it unless
    /// `overwrite=true`, in which case the server skips the version
    /// check entirely. A version mismatch surfaces as `412 Precondition
    /// Failed` with the server's `currentVersion` in the error body.
    pub async fn put_dashboard(
        &self,
        uid: &str,
        doc: &DashboardDocument,
        expected_version: Option<i64>,
        overwrite: bool,
        message: Option<&str>,
    ) -> Result<DashboardWriteResponse> {
        let body = DashboardUpsertRequest {
            dashboard: doc,
            version: if overwrite { None } else { expected_version },
            overwrite,
            uid: Some(uid),
            message,
        };
        self.send_upsert(
            "PUT",
            format!("{}/v2/dashboards/uid/{}", self.inner.base_url, uid),
            &body,
        )
        .await
    }

    /// Shared POST/PUT path. Pulls the response body once, then
    /// either decodes the success envelope or maps the error body to
    /// a structured `DashboardError` so callers can react to 412s
    /// specifically.
    async fn send_upsert(
        &self,
        method: &str,
        url: String,
        body: &DashboardUpsertRequest<'_>,
    ) -> Result<DashboardWriteResponse> {
        let req = match method {
            "POST" => self.inner.http.post(&url),
            "PUT" => self.inner.http.put(&url),
            _ => unreachable!("send_upsert only supports POST/PUT"),
        };
        let resp = self
            .auth(req)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("sending {method} {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return serde_json::from_str::<DashboardWriteResponse>(&text).with_context(|| {
                format!("decoding {method} {url} response: {}", snippet(&text, 200))
            });
        }
        // Try the structured error first; fall back to a raw body excerpt.
        if let Ok(err) = serde_json::from_str::<DashboardError>(&text) {
            let mut msg = format!(
                "axiom {} {} — {}: {}",
                method,
                status.as_u16(),
                err.code,
                err.message
            );
            if let Some(cv) = err.current_version {
                msg.push_str(&format!(" (server version: {cv})"));
            }
            if let Some(reason) = err.reason.as_deref() {
                msg.push_str(&format!(" — {reason}"));
            }
            return Err(anyhow!(msg));
        }
        Err(anyhow!(
            "axiom {} {} — {}",
            method,
            status.as_u16(),
            snippet(&text, 200)
        ))
    }

    /// Delete a dashboard by uid. Hits `DELETE /v2/dashboards/uid/{uid}`.
    pub async fn delete_dashboard(&self, uid: &str) -> Result<()> {
        let url = format!("{}/v2/dashboards/uid/{}", self.inner.base_url, uid);
        let resp = self
            .auth(self.inner.http.delete(&url))
            .header("Accept", "application/json")
            .send()
            .await
            .context("sending DELETE /v2/dashboards/uid request")?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(anyhow!(
            "axiom DELETE /v2/dashboards/uid/{} {} — {}",
            uid,
            status.as_u16(),
            snippet(&body, 200)
        ))
    }

    /// Fetch a single dashboard by uid. The `uid` is the path-friendly
    /// id; both `list_dashboards()` and the picker key on it.
    ///
    /// Returns the full `DashboardResource` envelope (top-level
    /// version + audit fields plus the nested `DashboardDocument`).
    /// Step 17b will adapt the envelope into the internal `Dashboard`
    /// model; for now callers consume the resource directly.
    pub async fn get_dashboard(&self, uid: &str) -> Result<DashboardSummary> {
        let url = format!("{}/v2/dashboards/uid/{}", self.inner.base_url, uid);
        let label = format!("/v2/dashboards/uid/{uid}");
        self.get_json(&url, "application/json", &label).await
    }

    /// Fetch the org's dashboards. Hits `GET /v2/dashboards` with
    /// `?limit=1000` (the server's max page size) so we get the whole
    /// org in one round-trip when it fits. Multi-page support can come
    /// later if anyone runs into the cap.
    ///
    /// Note: when authenticated with an API token, the server only
    /// returns dashboards shared org-wide or with a group — private
    /// dashboards stay invisible. Axiom's docs are explicit about this.
    pub async fn list_dashboards(&self) -> Result<Vec<DashboardSummary>> {
        let url = format!("{}/v2/dashboards?limit=1000", self.inner.base_url);
        self.get_json(&url, "application/json", "/v2/dashboards")
            .await
    }

    /// List metrics for `dataset` over `[start, end]`. The metrics-info endpoint
    /// only accepts RFC3339 timestamps (e.g. `2026-05-13T12:00:00Z`); relative
    /// expressions like `now-1h` are rejected.
    pub async fn list_metrics(
        &self,
        edge_url: &str,
        dataset: &str,
        start: &str,
        end: &str,
    ) -> Result<BTreeMap<String, MetricInfo>> {
        let url = format!(
            "{}/v1/query/metrics/info/datasets/{}/metrics?start={}&end={}",
            edge_url.trim_end_matches('/'),
            dataset,
            urlencoding(start),
            urlencoding(end),
        );
        self.get_json(
            &url,
            "application/vnd.metrics-info.v2+json",
            "metrics-info",
        )
        .await
    }

    /// List tag names for a specific `(dataset, metric)` pair. The endpoint
    /// returns a plain JSON array of strings under the default Accept header
    /// (unlike `/metrics`, which needs the v2 vnd-Accept to return a
    /// `{name: {type,...}}` map).
    pub async fn list_metric_tags(
        &self,
        edge_url: &str,
        dataset: &str,
        metric: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<String>> {
        let url = format!(
            "{}/v1/query/metrics/info/datasets/{}/metrics/{}/tags?start={}&end={}",
            edge_url.trim_end_matches('/'),
            dataset,
            urlencoding(metric),
            urlencoding(start),
            urlencoding(end),
        );
        self.get_json(&url, "application/json", "metric-tags").await
    }

    /// List the observed values for a single tag of a `(dataset, metric)`.
    /// Returns a plain JSON array of strings.
    pub async fn list_metric_tag_values(
        &self,
        edge_url: &str,
        dataset: &str,
        metric: &str,
        tag: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<String>> {
        let url = format!(
            "{}/v1/query/metrics/info/datasets/{}/metrics/{}/tags/{}/values?start={}&end={}",
            edge_url.trim_end_matches('/'),
            dataset,
            urlencoding(metric),
            urlencoding(tag),
            urlencoding(start),
            urlencoding(end),
        );
        self.get_json(&url, "application/json", "tag-values").await
    }

    /// Run an MPL query against the supplied edge URL.
    pub async fn query_mpl(
        &self,
        edge_url: &str,
        edge_deployment: Option<&str>,
        mpl: &str,
        start: &str,
        end: &str,
        params: &std::collections::BTreeMap<String, String>,
    ) -> Result<MetricsQueryResponse> {
        let url = format!("{}/v1/query/_mpl", edge_url.trim_end_matches('/'));

        #[derive(Serialize)]
        struct Req<'a> {
            apl: &'a str,
            #[serde(rename = "startTime")]
            start_time: &'a str,
            #[serde(rename = "endTime")]
            end_time: &'a str,
            #[serde(
                rename = "queryEdgeDeployment",
                skip_serializing_if = "Option::is_none"
            )]
            query_edge_deployment: Option<&'a str>,
            // User-declared MPL `param` values. Omitted when empty so we
            // don't surprise older API revisions.
            #[serde(rename = "queryParams", skip_serializing_if = "BTreeMap::is_empty")]
            query_params: &'a std::collections::BTreeMap<String, String>,
        }

        let body = Req {
            apl: mpl,
            start_time: start,
            end_time: end,
            query_edge_deployment: edge_deployment,
            query_params: params,
        };

        let resp = self
            .auth(self.inner.http.post(&url))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json+metrics.v2")
            .json(&body)
            .send()
            .await
            .context("sending /v1/query/_mpl request")?;

        let status = resp.status();
        let trace_id = extract_trace_id(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "axiom query {} — {}{}",
                status.as_u16(),
                snippet(&body, 400),
                trace_id
                    .as_deref()
                    .map(|t| format!(" (trace {t})"))
                    .unwrap_or_default(),
            ));
        }
        let mut decoded: MetricsQueryResponse = serde_json::from_str(&body)
            .with_context(|| format!("decoding query response: {}", snippet(&body, 200)))?;
        decoded.trace_id = trace_id;
        Ok(decoded)
    }
}

/// Pull the trace identifier out of an Axiom response. Tries the
/// service-specific header first, then the W3C `traceparent` field as a
/// fallback. Returns `None` when neither is set or is unprintable.
fn extract_trace_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    for name in ["x-axiom-trace-id", "traceparent"] {
        if let Some(val) = headers.get(name).and_then(|v| v.to_str().ok()) {
            return Some(val.to_string());
        }
    }
    None
}

/// Minimal percent-encoding for the small set of characters we actually pass
/// through query strings (mostly RFC3339 timestamps with `:` and `+`).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

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
#[path = "axiom_tests.rs"]
mod tests;
