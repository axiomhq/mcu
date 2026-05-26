//! Free helpers used by `App` methods. No `App` borrow — everything
//! here is either a small data conversion (response → series), a
//! source-text scanner (`referenced_tags`, `ident_before`), or a
//! plumbing helper around the async query path (`resolve_route`,
//! `run_query_task`).

use std::sync::{Arc, RwLock};

use tui_textarea::TextArea;

use crate::axiom::{Client as AxiomClient, MetricsQueryResponse, MetricsSeries};
use crate::cache::{Cache, EdgeRoute};
use crate::chart::{Series, color_for, tag_text};
use crate::completions;
use crate::mpl;
use crate::viz;

use super::types::CompletionState;

pub(super) fn state_from(
    payload: completions::CompletionPayload,
    selected: usize,
) -> CompletionState {
    let kind_label = completions::kind_label(&payload.kind);
    CompletionState {
        visible: true,
        items: payload.items,
        selected,
        replace_range_bytes: payload.replace_range,
        kind_label,
        kind: Some(payload.kind),
    }
}

/// Lossy display of a path for status messages — keeps the code free of
/// `path.display()` ceremony at every call site.
pub(super) fn display_path(p: &std::path::Path) -> String {
    p.display().to_string()
}

/// Extract identifiers that appear immediately before a comparison operator
/// (`==`, `!=`, `<`, `>`, `<=`, `>=`) in `query`. Identifiers may be plain
/// (alphanumeric + `_` + `.`) or backtick-quoted. String literals are
/// skipped so `"a == b"` doesn't register. The result is deduped and order
/// is unspecified.
///
/// This is a deliberately lightweight scan, not an MPL parser: in `where`-
/// like positions the identifier immediately before a comparison is
/// always a tag name, so we don't need full grammar awareness to drive a
/// tag-value prefetcher.
pub(super) fn referenced_tags(query: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let bytes = query.as_bytes();
    let len = bytes.len();
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut i = 0;
    while i < len {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < len && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < len {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if i < len {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'/' => {
                // Line comment.
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        if is_cmp_op_at(bytes, i)
            && let Some(name) = ident_before(bytes, i)
        {
            out.insert(name);
        }
        i += 1;
    }
    out.into_iter().collect()
}

pub(super) fn is_cmp_op_at(bytes: &[u8], i: usize) -> bool {
    if i + 1 < bytes.len() {
        match (bytes[i], bytes[i + 1]) {
            (b'=', b'=') | (b'!', b'=') | (b'<', b'=') | (b'>', b'=') => return true,
            _ => {}
        }
    }
    // Single-char `<` / `>`. Avoid false positives on `<=` / `>=` (handled above)
    // and on the leading char of `<=` etc. We accept the char only when the next
    // char is not `=`.
    if i < bytes.len()
        && (bytes[i] == b'<' || bytes[i] == b'>')
        && bytes.get(i + 1).copied() != Some(b'=')
    {
        return true;
    }
    false
}

/// Returns the identifier ending at `pos` (exclusive), skipping leading
/// whitespace. Handles backtick-quoted names by unescaping the surrounding
/// backticks.
pub(super) fn ident_before(bytes: &[u8], pos: usize) -> Option<String> {
    let mut j = pos;
    while j > 0 && bytes[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    if j == 0 {
        return None;
    }
    if bytes[j - 1] == b'`' {
        let end = j - 1;
        let mut k = end;
        while k > 0 && bytes[k - 1] != b'`' {
            k -= 1;
        }
        if k == 0 {
            return None;
        }
        // bytes[k - 1] == b'`' is the opening backtick.
        let inner = &bytes[k..end];
        if inner.is_empty() {
            return None;
        }
        return Some(String::from_utf8_lossy(inner).into_owned());
    }
    let end = j;
    while j > 0 && is_tag_byte(bytes[j - 1]) {
        j -= 1;
    }
    if j == end {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[j..end]).into_owned())
}

pub(super) fn is_tag_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
}

pub(super) fn editor_cursor_byte_offset(textarea: &TextArea<'_>) -> usize {
    let (row, char_col) = textarea.cursor();
    let lines = textarea.lines();
    let mut offset = 0;
    for line in lines.iter().take(row) {
        offset += line.len() + 1; // +1 for the synthetic '\n' join
    }
    if let Some(line) = lines.get(row) {
        let byte_col = line
            .char_indices()
            .nth(char_col)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        offset += byte_col;
    }
    offset
}

pub(super) fn byte_offset_to_row_col(text: &str, byte_offset: usize) -> (usize, usize) {
    let clamped = byte_offset.min(text.len());
    let prefix = &text[..clamped];
    let row = prefix.bytes().filter(|&b| b == b'\n').count();
    let col = match prefix.rfind('\n') {
        Some(nl) => prefix[nl + 1..].chars().count(),
        None => prefix.chars().count(),
    };
    (row, col)
}

/// Resolve the edge route for `dataset`, refreshing the cache once on miss.
pub(super) async fn resolve_route(
    cache: &Arc<RwLock<Cache>>,
    client: &AxiomClient,
    dataset: &str,
) -> anyhow::Result<EdgeRoute> {
    if let Some(r) = cache.read().unwrap().edge_route_for(dataset) {
        return Ok(r);
    }
    refresh_dataset_route(cache, client, dataset).await
}

/// Force a `list_datasets` refresh and re-resolve the route for `dataset`.
/// Used both on cold cache miss and after a metrics 404 (which we treat as
/// a sign that the cached `edgeDeployment` for the dataset is stale).
pub(super) async fn refresh_dataset_route(
    cache: &Arc<RwLock<Cache>>,
    client: &AxiomClient,
    dataset: &str,
) -> anyhow::Result<EdgeRoute> {
    let datasets = client
        .list_datasets()
        .await
        .map_err(|e| e.context("refreshing dataset list to resolve edge URL"))?;
    cache_save_with(cache, |c| c.replace_datasets(datasets));
    cache
        .read()
        .unwrap()
        .edge_route_for(dataset)
        .ok_or_else(|| anyhow::anyhow!("dataset `{dataset}` not found in this deployment"))
}

/// True iff `err` (or any frame in its anyhow chain) is an axiom-rs API
/// error reporting HTTP 404.
///
/// Metrics-info / `_mpl` calls go through a per-dataset edge URL that we
/// cache via `cache::EdgeRoute`. When that URL is stale (the dataset's
/// `edgeDeployment` moved), the server returns 404; callers use this
/// predicate to drive a one-shot route refresh + retry instead of
/// surfacing the error to the user.
pub(super) fn is_axiom_404(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<axiom_rs::Error>()
            .map(|e| matches!(e, axiom_rs::Error::Axiom(a) if a.status == 404))
            .unwrap_or(false)
    })
}

/// Normalise a time-range string before sending it to the metrics
/// query endpoint. The Axiom dashboard schema stores relative
/// expressions with a `qr-` prefix (e.g. `qr-now-7d`, `qr-now`) for
/// the web UI's range picker, but `POST /v1/query/_mpl` rejects that
/// prefix with `invalid field: "qr"`. Stripping it makes
/// `qr-now-7d` ≡ `now-7d` and `qr-now` ≡ `now`, which is what the
/// API actually accepts.
pub(super) fn normalize_time_expr(s: &str) -> String {
    s.strip_prefix("qr-").unwrap_or(s).to_string()
}

/// Parse a date out of the configured time-range string when it's an
/// RFC3339 timestamp (e.g. `2024-05-01T00:00:00Z` or just `2024-05-01`).
/// Returns `None` for relative expressions (`now-1h`, `qr-now-7d`), in
/// which case the calendar picker keeps its seeded default.
pub(super) fn parse_iso_date(s: &str) -> Option<time::Date> {
    // Try RFC3339 first; fall back to bare `YYYY-MM-DD`.
    if let Ok(odt) = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
    {
        return Some(odt.date());
    }
    let ymd = time::format_description::parse("[year]-[month]-[day]").ok()?;
    time::Date::parse(s, &ymd).ok()
}

pub(super) async fn run_query_task(
    cache: &Arc<RwLock<Cache>>,
    client: &AxiomClient,
    dataset: &str,
    mpl: &str,
    start: &str,
    end: &str,
    params: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<MetricsQueryResponse> {
    let mut route = resolve_route(cache, client, dataset).await?;
    let mut refreshed = false;
    loop {
        let result = client
            .query_mpl(
                &route.url,
                route.deployment.as_deref(),
                mpl,
                start,
                end,
                params,
            )
            .await;
        match result {
            Err(e) if !refreshed && is_axiom_404(&e) => {
                refreshed = true;
                route = refresh_dataset_route(cache, client, dataset).await?;
            }
            other => return other,
        }
    }
}

/// Build a `Diagnostic` for a pragma parse failure at `line_idx`.
/// Column points at column 1 of that line; length spans the line. This
/// matches how the engine reports its own line-level diagnostics, so the
/// status bar treatment is uniform.
pub(super) fn pragma_diagnostic(
    text: &str,
    line_idx: usize,
    err: &viz::PragmaError,
) -> mpl::Diagnostic {
    // Byte offset of the start of `line_idx`.
    let byte_offset = text
        .split_inclusive('\n')
        .take(line_idx)
        .map(|s| s.len())
        .sum::<usize>();
    let line_len = text.lines().nth(line_idx).map(str::len).unwrap_or(0);
    mpl::Diagnostic {
        severity: mpl::Severity::Warning,
        message: err.to_string(),
        help: Some(
            "valid kinds: line, bar, area, scatter, statistic, top_list, pie, heatmap, \
             table, log_stream, monitor_list, note, spacer"
                .to_string(),
        ),
        byte_offset,
        byte_length: line_len,
        line: line_idx + 1,
        column: 1,
        actions: Vec::new(),
    }
}

/// Acquire the shared cache for write, run `f` against it, then persist
/// to disk. The save-failure path logs to stderr (matching the inline
/// pattern this replaced) so it never fights the UI for the status
/// line.
pub(super) fn cache_save_with<F: FnOnce(&mut Cache)>(cache: &Arc<RwLock<Cache>>, f: F) {
    let mut c = cache.write().unwrap();
    f(&mut c);
    if let Err(e) = c.save() {
        eprintln!("mcu: cache save failed: {e}");
    }
}

pub(super) fn default_cache() -> Cache {
    // We don't yet have a base URL — `Cache::load` only needs a fallback for
    // datasets that lack `edgeDeployment`. Use a placeholder; it gets replaced
    // when the first real query reaches `route_for`.
    Cache::load(String::new())
}

/// Convert an Axiom MPL response into the internal `Series` model used by the chart.
/// Validate that `value` parses as the engine's `param_value` rule. This
/// is what `mpl_lang::query::ProvidedParams::parse_and_validate` does
/// internally per provided pair; we surface it eagerly so `:p host=db-01`
/// (a bare ident with a `-`) is rejected at set-time rather than at
/// query-time. Returns a short message; on success the value is left to
/// the server to typecheck against the declared param's type.
pub(super) fn validate_param_value(value: &str) -> Result<(), String> {
    use mpl_lang::{MPLParser, Rule};
    use pest::Parser as _;
    let mut pairs = MPLParser::parse(Rule::param_value, value).map_err(|e| {
        // Pest's full error is multi-line and noisy in a status bar;
        // extract the most useful first line.
        e.to_string()
            .lines()
            .next()
            .unwrap_or("parse error")
            .to_string()
    })?;
    // `parse` doesn't enforce consuming the entire input — it'll happily
    // accept `db-01` by matching just `db` as an ident. Reject anything
    // with trailing garbage so e.g. `host=db-01` is caught at set-time.
    let pair = pairs.next().ok_or_else(|| "empty parse".to_string())?;
    let end = pair.as_span().end();
    if end != value.len() {
        return Err(format!(
            "trailing garbage after `{}`",
            &value[..end].trim_end()
        ));
    }
    Ok(())
}

pub(super) fn response_to_series(resp: &MetricsQueryResponse) -> Vec<Series> {
    resp.series
        .iter()
        .enumerate()
        .map(|(i, s)| metrics_series_to_series(s, i))
        .collect()
}

pub(super) fn metrics_series_to_series(s: &MetricsSeries, palette_index: usize) -> Series {
    let res = s.resolution.max(1) as i64;
    let points: Vec<(f64, f64)> = s
        .data
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            v.map(|y| {
                let x = (s.start + (i as i64) * res) as f64;
                (x, y)
            })
        })
        .collect();

    let mut tag_pairs: Vec<(String, serde_json::Value)> =
        s.tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    tag_pairs.sort_by(|a, b| a.0.cmp(&b.0));

    Series {
        name: format_series_name(&s.metric, &tag_pairs),
        tags: tag_pairs,
        points,
        color: color_for(palette_index),
    }
}

pub(super) fn format_series_name(metric: &str, tags: &[(String, serde_json::Value)]) -> String {
    // Prefer a short identifying tag set (room/host/service/device); fall back to all tags.
    const PREFERRED: &[&str] = &["room", "host", "service.name", "device", "endpoint"];
    let mut chosen: Vec<String> = PREFERRED
        .iter()
        .filter_map(|k| tags.iter().find(|(t, _)| t == k).map(|(_, v)| tag_text(v)))
        .collect();
    if chosen.is_empty() {
        chosen = tags
            .iter()
            .map(|(k, v)| format!("{k}={}", tag_text(v)))
            .collect();
    }
    if chosen.is_empty() {
        metric.to_string()
    } else {
        format!("{metric} {{{}}}", chosen.join(","))
    }
}

pub(super) fn demo_series() -> Vec<Series> {
    let sin_points: Vec<(f64, f64)> = (0..100)
        .map(|i| {
            let x = i as f64 * 0.1;
            (x, x.sin())
        })
        .collect();
    let cos_points: Vec<(f64, f64)> = (0..100)
        .map(|i| {
            let x = i as f64 * 0.1;
            (x, (x * 0.5).cos())
        })
        .collect();

    vec![
        Series {
            name: "sin(x)".to_string(),
            tags: vec![],
            points: sin_points,
            color: color_for(0),
        },
        Series {
            name: "cos(x/2)".to_string(),
            tags: vec![],
            points: cos_points,
            color: color_for(1),
        },
    ]
}
