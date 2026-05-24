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
/// `MonitorList`) that don't exist server-side; conversely the server
/// has `Scatter` which the TUI hasn't implemented. Mapping between
/// the two lives in `dashboard::Tile::from_chart` (step 17b).
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

    pub async fn list_datasets(&self) -> Result<Vec<DatasetSummary>> {
        let url = format!("{}/v1/datasets", self.inner.base_url);
        let resp = self
            .auth(self.inner.http.get(&url))
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
            .with_context(|| format!("decoding datasets response: {}", snippet(&body, 200)))
    }

    /// Create a new dashboard. Hits `POST /v2/dashboards`. Returns
    /// the server's write response, which includes the freshly-minted
    /// `DashboardResource` (with `uid`, `version`, audit fields).
    ///
    /// On `409 Conflict` the server returns a `DashboardError` whose
    /// `code` distinguishes the failure mode (e.g. uid collision).
    ///
    /// Wired to `:dash new from-buffer` in step 17e; declared here so
    /// the client surface is complete before then.
    #[allow(dead_code)]
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
        let resp = self
            .auth(self.inner.http.get(&url))
            .header("Accept", "application/json")
            .send()
            .await
            .context("sending /v2/dashboards/uid request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "axiom /v2/dashboards/uid/{} {} — {}",
                uid,
                status.as_u16(),
                snippet(&body, 200)
            ));
        }
        serde_json::from_str::<DashboardSummary>(&body).with_context(|| {
            format!(
                "decoding /v2/dashboards/uid/{uid} response: {}",
                snippet(&body, 200)
            )
        })
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
        let resp = self
            .auth(self.inner.http.get(&url))
            .header("Accept", "application/json")
            .send()
            .await
            .context("sending /v2/dashboards request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "axiom /v2/dashboards {} — {}",
                status.as_u16(),
                snippet(&body, 200)
            ));
        }
        serde_json::from_str::<Vec<DashboardSummary>>(&body)
            .with_context(|| format!("decoding dashboards response: {}", snippet(&body, 200)))
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
        let resp = self
            .auth(self.inner.http.get(&url))
            .header("Accept", "application/vnd.metrics-info.v2+json")
            .send()
            .await
            .context("sending metrics-info request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "axiom metrics-info {} — {}",
                status.as_u16(),
                snippet(&body, 200)
            ));
        }
        serde_json::from_str(&body)
            .with_context(|| format!("decoding metrics-info: {}", snippet(&body, 200)))
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
        let resp = self
            .auth(self.inner.http.get(&url))
            .header("Accept", "application/json")
            .send()
            .await
            .context("sending tags request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "axiom metric-tags {} — {}",
                status.as_u16(),
                snippet(&body, 200)
            ));
        }
        serde_json::from_str::<Vec<String>>(&body)
            .with_context(|| format!("decoding metric-tags: {}", snippet(&body, 200)))
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
        let resp = self
            .auth(self.inner.http.get(&url))
            .header("Accept", "application/json")
            .send()
            .await
            .context("sending tag-values request")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "axiom tag-values {} — {}",
                status.as_u16(),
                snippet(&body, 200)
            ));
        }
        serde_json::from_str::<Vec<String>>(&body)
            .with_context(|| format!("decoding tag-values: {}", snippet(&body, 200)))
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

/// Parse the dataset name out of an MPL query.
pub fn extract_dataset(mpl: &str) -> Result<String> {
    Ok(extract_dataset_metric(mpl)?.0)
}

/// Skip leading whitespace plus any number of MPL line comments
/// (`// …\n`) and block comments (`/* … */`). MPL pragmas like the
/// dashboard adoption's `// @viz statistic` live in these comments,
/// and without this step the dataset parser sees `//` as the dataset.
fn skip_leading_comments_and_ws(mut s: &str) -> &str {
    loop {
        let trimmed = s.trim_start();
        if let Some(rest) = trimmed.strip_prefix("//") {
            // Line comment: skip up to and including the newline.
            s = match rest.find('\n') {
                Some(i) => &rest[i + 1..],
                None => "",
            };
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/*") {
            // Block comment: skip up to and including `*/`.
            s = match rest.find("*/") {
                Some(i) => &rest[i + 2..],
                None => "",
            };
            continue;
        }
        return trimmed;
    }
}

/// Parse `dataset:metric` out of an MPL query. Returns `(dataset, metric)`,
/// both with backtick quoting stripped. The metric portion is empty if the
/// query lacks a colon (i.e. only the dataset has been typed so far).
pub fn extract_dataset_metric(mpl: &str) -> Result<(String, String)> {
    let s = skip_leading_comments_and_ws(mpl);
    // Tolerate a few stray `|` pipes (or whitespace) before the
    // dataset — callers occasionally hand us a buffer with a leading
    // continuation line.
    let s = s.trim_start_matches(|c: char| c == '|' || c.is_whitespace());
    let s = skip_leading_comments_and_ws(s);

    let (dataset, rest) = if let Some(rest) = s.strip_prefix('`') {
        let end = rest
            .find('`')
            .ok_or_else(|| anyhow!("MPL query has unterminated backtick around dataset"))?;
        (&rest[..end], &rest[end + 1..])
    } else {
        let end = s
            .find(|c: char| c == ':' || c.is_whitespace())
            .ok_or_else(|| anyhow!("MPL query missing `dataset:metric` prefix"))?;
        (&s[..end], &s[end..])
    };

    if dataset.is_empty() {
        return Err(anyhow!("MPL query has empty dataset name"));
    }

    // After the dataset name we expect `:metric`. Tolerate the absence so
    // callers like the tag prefetcher can keep working on a half-typed query.
    let rest = rest.trim_start();
    let metric = if let Some(rest) = rest.strip_prefix(':') {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('`') {
            let end = rest
                .find('`')
                .ok_or_else(|| anyhow!("MPL query has unterminated backtick around metric"))?;
            rest[..end].to_string()
        } else {
            let end = rest
                .find(|c: char| c == '|' || c.is_whitespace())
                .unwrap_or(rest.len());
            rest[..end].to_string()
        }
    } else {
        String::new()
    };

    Ok((dataset.to_string(), metric))
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
mod tests {
    use super::*;

    #[test]
    fn snippet_truncates_long_text() {
        let s = "a".repeat(500);
        let got = snippet(&s, 10);
        assert_eq!(got.chars().count(), 11);
        assert!(got.ends_with('…'));
    }

    #[test]
    fn decodes_dashboard_resource_envelope() {
        // Real shape from `GET /v2/dashboards`: each item is a
        // DashboardResource with the document nested under `dashboard`.
        let body = r#"[
            {
                "uid": "abc123",
                "id": "42",
                "version": 7,
                "createdAt": "2026-05-01T10:00:00Z",
                "updatedAt": "2026-05-23T10:00:00Z",
                "createdBy": "u1",
                "updatedBy": "u2",
                "dashboard": {
                    "name": "Cluster Overview",
                    "description": "pod lifecycle",
                    "charts": [],
                    "layout": []
                }
            }
        ]"#;
        let v: Vec<DashboardSummary> = serde_json::from_str(body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].uid, "abc123");
        assert_eq!(v[0].id.as_deref(), Some("42"));
        assert_eq!(v[0].updated_at.as_deref(), Some("2026-05-23T10:00:00Z"));
        assert_eq!(v[0].name(), "Cluster Overview");
        assert_eq!(v[0].description(), Some("pod lifecycle"));
    }

    #[test]
    fn decodes_dashboard_resource_tolerates_extra_fields() {
        // Server schema bumps shouldn't break the picker decode.
        let body = r#"[{
            "uid": "x",
            "dashboard": {"name": "y", "newField": 1},
            "futureTopLevelField": true
        }]"#;
        let v: Vec<DashboardSummary> = serde_json::from_str(body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name(), "y");
    }

    #[test]
    fn dashboard_name_falls_back_when_document_missing_name() {
        let body = r#"[{"uid": "x", "dashboard": {}}]"#;
        let v: Vec<DashboardSummary> = serde_json::from_str(body).unwrap();
        assert_eq!(v[0].name(), "(unnamed)");
    }

    #[test]
    fn decodes_chart_variants_with_type_discriminator() {
        // One of each chart type the server emits, with id + name +
        // an opaque query field. The decoder should land each on the
        // right `Chart` variant and preserve `query` verbatim.
        let body = r#"{
            "name": "sample",
            "charts": [
                {"id": "c1", "type": "TimeSeries", "name": "ts", "query": {"a": 1}},
                {"id": "c2", "type": "Heatmap", "name": "hm", "query": {}},
                {"id": "c3", "type": "LogStream", "name": "ls"},
                {"id": "c4", "type": "Pie"},
                {"id": "c5", "type": "Scatter"},
                {"id": "c6", "type": "Table"},
                {"id": "c7", "type": "TopK"},
                {"id": "c8", "type": "Statistic"},
                {"id": "c9", "type": "Note", "name": "hi"}
            ]
        }"#;
        let doc: DashboardDocument = serde_json::from_str(body).unwrap();
        let types: Vec<&str> = doc.charts.iter().map(|c| c.type_str()).collect();
        assert_eq!(
            types,
            vec![
                "TimeSeries",
                "Heatmap",
                "LogStream",
                "Pie",
                "Scatter",
                "Table",
                "TopK",
                "Statistic",
                "Note",
            ]
        );
        // Spot-check that nested query JSON survives intact.
        let ts = doc.charts.first().unwrap();
        assert_eq!(ts.base().query.as_ref().unwrap()["a"], 1);
    }

    #[test]
    fn decodes_layout_items() {
        let body = r#"{
            "name": "x",
            "layout": [
                {"i": "c1", "x": 0, "y": 0, "w": 6, "h": 4},
                {"i": "c2", "x": 6, "y": null, "w": 6, "h": 4, "static": true}
            ]
        }"#;
        let doc: DashboardDocument = serde_json::from_str(body).unwrap();
        assert_eq!(doc.layout.len(), 2);
        assert_eq!(doc.layout[0].i, "c1");
        assert_eq!(doc.layout[0].y, Some(0));
        assert_eq!(doc.layout[1].y, None);
        // Unmodelled `static` survives in extras.
        assert!(doc.layout[1].extras.contains_key("static"));
    }

    #[test]
    fn upsert_request_omits_version_when_overwrite() {
        let doc = DashboardDocument {
            name: Some("x".into()),
            ..Default::default()
        };
        let body = DashboardUpsertRequest {
            dashboard: &doc,
            version: None,
            overwrite: true,
            uid: Some("u1"),
            message: None,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["overwrite"], true);
        assert!(v.get("version").is_none(), "version should be omitted");
        assert!(v.get("message").is_none());
        assert_eq!(v["uid"], "u1");
    }

    #[test]
    fn upsert_request_omits_overwrite_when_default() {
        let doc = DashboardDocument::default();
        let body = DashboardUpsertRequest {
            dashboard: &doc,
            version: Some(5),
            overwrite: false,
            uid: None,
            message: None,
        };
        let v = serde_json::to_value(&body).unwrap();
        // `overwrite: false` is the schema default, so we don't emit
        // it; this keeps the on-the-wire payload minimal.
        assert!(
            v.get("overwrite").is_none(),
            "overwrite=false should be omitted, got {v}"
        );
        assert_eq!(v["version"], 5);
    }

    #[test]
    fn decodes_write_response_status() {
        let body = r#"{
            "status": "updated",
            "overwritten": false,
            "dashboard": {
                "uid": "u1",
                "version": 8,
                "dashboard": {"name": "x"}
            }
        }"#;
        let w: DashboardWriteResponse = serde_json::from_str(body).unwrap();
        assert_eq!(w.status, DashboardWriteStatus::Updated);
        assert_eq!(w.dashboard.version, Some(8));
    }

    #[test]
    fn decodes_412_error_with_current_version() {
        let body = r#"{
            "code": "version_conflict",
            "message": "dashboard version is stale",
            "currentVersion": 9,
            "uid": "u1"
        }"#;
        let e: DashboardError = serde_json::from_str(body).unwrap();
        assert_eq!(e.code, "version_conflict");
        assert_eq!(e.current_version, Some(9));
    }

    #[test]
    fn dashboard_document_round_trips_extras() {
        // Unknown top-level fields (`refreshTime`, `schemaVersion`,
        // `against`, `owner`) survive both decode and re-encode —
        // critical because the server's spec is
        // `additionalProperties: false`, so PUT would reject anything we
        // dropped on the floor.
        let original = serde_json::json!({
            "name": "keepers",
            "refreshTime": 60,
            "schemaVersion": 2,
            "owner": "X-AXIOM-EVERYONE",
            "against": "-1h",
            "timeWindowStart": "qr-now-1h",
            "timeWindowEnd": "qr-now"
        });
        let doc: DashboardDocument = serde_json::from_value(original.clone()).unwrap();
        let re = serde_json::to_value(&doc).unwrap();
        // Every key from the original lands somewhere in the re-encode.
        for (k, v) in original.as_object().unwrap() {
            assert_eq!(&re[k], v, "field `{k}` did not round-trip");
        }
    }

    #[test]
    fn decodes_dataset_summary() {
        let body = r#"[
            {"name": "k8s", "description": "k8s metrics", "edgeDeployment": "cloud.us-east-1.aws", "kind": "otel:metrics:v1"},
            {"name": "logs"}
        ]"#;
        let datasets: Vec<DatasetSummary> = serde_json::from_str(body).unwrap();
        assert_eq!(datasets.len(), 2);
        assert_eq!(datasets[0].name, "k8s");
        assert_eq!(datasets[0].kind.as_deref(), Some("otel:metrics:v1"));
        assert_eq!(
            datasets[0].edge_deployment.as_deref(),
            Some("cloud.us-east-1.aws")
        );
        assert!(datasets[1].kind.is_none());
    }

    #[test]
    fn decodes_metrics_info() {
        let body = r#"{
            "switch": {"type": "Mixed", "temporality": "Mixed", "unit": null},
            "temp":   {"type": "Mixed", "temporality": "Mixed", "unit": "C"}
        }"#;
        let m: BTreeMap<String, MetricInfo> = serde_json::from_str(body).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m["switch"].kind.as_deref(), Some("Mixed"));
        assert_eq!(m["temp"].unit.as_deref(), Some("C"));
    }

    #[test]
    fn extract_dataset_from_backticked() {
        assert_eq!(
            extract_dataset("`home`:`temp` | align to 5m using avg").unwrap(),
            "home"
        );
        assert_eq!(
            extract_dataset("`k8s-metrics-dev`:cpu_usage[1h..]").unwrap(),
            "k8s-metrics-dev"
        );
    }

    #[test]
    fn extract_dataset_from_plain() {
        assert_eq!(extract_dataset("home:temp | align to 1m").unwrap(), "home");
    }

    #[test]
    fn extract_dataset_skips_leading_line_comment() {
        // The dashboard adoption seeds the editor with a `// @viz`
        // pragma above the real query. Without comment-skipping the
        // dataset parser used to read `//` as the dataset name and
        // ask the server for it, producing
        // `dataset "//" not found in this deployment`.
        let q = "// @viz statistic\n`home`:temp\n| group using avg";
        let (ds, m) = extract_dataset_metric(q).unwrap();
        assert_eq!(ds, "home");
        assert_eq!(m, "temp");
    }

    #[test]
    fn extract_dataset_skips_multiple_comments() {
        let q = "// pragma\n// another\n/* block */ `home`:temp";
        assert_eq!(extract_dataset(q).unwrap(), "home");
    }

    #[test]
    fn extract_dataset_errors_on_garbage() {
        assert!(extract_dataset("").is_err());
        assert!(extract_dataset("`unterminated").is_err());
    }

    #[test]
    fn decodes_metrics_query_response() {
        let body = r#"{
            "metadata": {},
            "series": [
                {
                    "metric": "temp",
                    "tags": {"room": "Eingang"},
                    "start": 1764547200,
                    "resolution": 3600,
                    "data": [18.24, null, 18.11]
                }
            ]
        }"#;
        let resp: MetricsQueryResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.series.len(), 1);
        let s = &resp.series[0];
        assert_eq!(s.metric, "temp");
        assert_eq!(s.tags.get("room").map(String::as_str), Some("Eingang"));
        assert_eq!(s.start, 1764547200);
        assert_eq!(s.resolution, 3600);
        assert_eq!(s.data, vec![Some(18.24), None, Some(18.11)]);
    }

    #[test]
    fn urlencodes_rfc3339_timestamps() {
        assert_eq!(urlencoding("now"), "now");
        assert_eq!(urlencoding("now-1h"), "now-1h");
        assert_eq!(
            urlencoding("2026-05-14T00:00:00Z"),
            "2026-05-14T00%3A00%3A00Z"
        );
    }
}
