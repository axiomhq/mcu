//! Context-aware MPL completions, powered by `mpl_language_server`.
//!
//! The heavy lifting — classifying the cursor position, walking the
//! `STDLIB` for stdlib function lists, filtering by partial token,
//! handling backticked identifiers, system params and inline
//! `param $x: T;` declarations — happens inside the engine. This module
//! is a thin adapter that:
//!
//!   1. Converts our [`crate::params::SystemParam`]s into the engine's
//!      [`ParamItem`] shape.
//!   2. Calls [`compute_completions_with_params`].
//!   3. Maps the engine's [`CompletionResult`] into our [`CompletionPayload`],
//!      filling in cache-backed candidate strings for `Dataset`/`Metric`
//!      where the engine only supplies the surrounding context.
//!   4. Pre-computes the per-item `apply` text used on accept, honouring
//!      the engine's snippet hints for keywords (e.g. `"where "`,
//!      `"ifdef("`) and the MPL grammar's backtick-quoting rules for
//!      dataset / metric / tag identifiers (`mpl.pest`).
//!      `accept_completion` then just inserts `item.apply` verbatim.

use mpl_language_server::{
    CompletionResult, ParamItem, ParamType, Span, compute_completions_with_params,
};

use crate::axiom;
use crate::cache::Cache;
use crate::params::{ParamKind, SystemParam};

/// Coarse category for the popup title and downstream branching (cache
/// prefetch on accept, etc.). Mirrors the engine's `CompletionResult`
/// variants but flattens the function-category enum into a single discriminant
/// per kind we care about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    Dataset,
    Metric {
        dataset: String,
    },
    /// Cursor is in a position where a tag name is expected.
    Tag {
        dataset: String,
        #[allow(dead_code)] // surfaced via popup title in a future step
        metric: String,
    },
    /// Cursor is in a tag-value position (right of `<tag> == "<partial>`).
    /// Carries the resolved `(dataset, metric, tag)` so accept can also
    /// kick off a refresh if cache is stale.
    TagValue {
        #[allow(dead_code)]
        dataset: String,
        #[allow(dead_code)]
        metric: String,
        tag: String,
    },
    /// Keyword-position completion (pipe operators, `to`, `by`, `using`, …).
    Keyword,
    AlignFn,
    MapFn,
    GroupFn,
    BucketFn,
    ComputeFn,
    Param,
}

/// One completion entry: what shows in the popup and what gets inserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// Label shown to the user in the popup.
    pub label: String,
    /// Pre-computed insert text. Built once in [`compute`] so accept logic
    /// is a single string write — no kind-dispatch at insert time.
    pub apply: String,
}

/// What the popup needs from one engine round-trip.
#[derive(Debug, Clone)]
pub struct CompletionPayload {
    pub kind: CompletionKind,
    pub items: Vec<CompletionItem>,
    /// Byte range in the joined editor text that an accept replaces.
    pub replace_range: (usize, usize),
}

/// Run the engine for the cursor at `byte_offset` in `query`, then materialise
/// items for cache-backed kinds and pre-compute every item's apply text.
/// Returns `None` if the engine reports nothing or all categories that need
/// cache data have nothing to offer.
pub fn compute(
    query: &str,
    byte_offset: usize,
    system_params: &[SystemParam],
    cache: &Cache,
) -> Option<CompletionPayload> {
    // Tag-value override: when the cursor sits in a `<tag> <op> [value]<cursor>`
    // position we drive the popup from `Cache::tag_values_for`. The engine
    // has no dedicated variant for this (it returns the surrounding
    // `Keywords` list at best, `None` more often), so we look up
    // `(dataset, metric, tag)` ourselves. Only fires when the lookup
    // succeeds AND the cache has values — otherwise we fall through to
    // the engine's reading of the cursor.
    if let Some(payload) = tag_value_payload(query, byte_offset, cache) {
        return Some(payload);
    }

    let extras = to_engine_params(system_params);
    let result = compute_completions_with_params(query, byte_offset, &extras)?;

    let (kind, items, span) = match result {
        CompletionResult::Keywords { span, options } => {
            let items = options
                .into_iter()
                .map(|o| CompletionItem {
                    label: o.label.to_string(),
                    apply: o.apply.unwrap_or(o.label).to_string(),
                })
                .collect();
            (CompletionKind::Keyword, items, span)
        }
        CompletionResult::AlignFunctions { span, options } => {
            (CompletionKind::AlignFn, plain_items(options), span)
        }
        CompletionResult::MapFunctions { span, options } => {
            (CompletionKind::MapFn, plain_items(options), span)
        }
        CompletionResult::GroupFunctions { span, options } => {
            (CompletionKind::GroupFn, plain_items(options), span)
        }
        CompletionResult::BucketFunctions { span, options } => {
            (CompletionKind::BucketFn, plain_items(options), span)
        }
        CompletionResult::ComputeFunctions { span, options } => {
            (CompletionKind::ComputeFn, plain_items(options), span)
        }
        CompletionResult::Params { span, options } => {
            let items = options
                .into_iter()
                .map(|o| CompletionItem {
                    apply: o.label.clone(),
                    label: o.label,
                })
                .collect();
            (CompletionKind::Param, items, span)
        }
        CompletionResult::Dataset { span } => {
            let opened = backtick_before(query, span);
            let partial = &query[span.from..span.to];
            let items = filter_by_partial(cache.dataset_names(), partial)
                .into_iter()
                .map(|label| ident_item(&label, opened))
                .collect();
            (CompletionKind::Dataset, items, span)
        }
        CompletionResult::Metric { span, dataset } => {
            let opened = backtick_before(query, span);
            let partial = &query[span.from..span.to];
            let items = filter_by_partial(cache.metric_names(&dataset), partial)
                .into_iter()
                .map(|label| ident_item(&label, opened))
                .collect();
            (CompletionKind::Metric { dataset }, items, span)
        }
        CompletionResult::Tag {
            span,
            dataset,
            metric,
        } => {
            let opened = backtick_before(query, span);
            let partial = &query[span.from..span.to];
            let pool = cache.tags_for(&dataset, &metric);
            let items = filter_by_partial(pool, partial)
                .into_iter()
                .map(|label| ident_item(&label, opened))
                .collect();
            (CompletionKind::Tag { dataset, metric }, items, span)
        }
    };

    Some(CompletionPayload {
        kind,
        items,
        replace_range: (span.from, span.to),
    })
}

/// Function and similar plain-identifier items: apply text equals label.
fn plain_items(options: Vec<mpl_language_server::FunctionItem>) -> Vec<CompletionItem> {
    options
        .into_iter()
        .map(|o| CompletionItem {
            apply: o.label.clone(),
            label: o.label,
        })
        .collect()
}

/// Build a `CompletionItem` for an MPL identifier (dataset / metric / tag).
/// Honours the backtick-quoting rules from the MPL grammar:
///
///   - if the user has already typed `` ` `` (engine has advanced `span.from`
///     past it), insert the escaped body plus a closing backtick;
///   - else, wrap in backticks only when the name violates the plain-ident
///     grammar (`[A-Za-z_][A-Za-z0-9_]*`);
///   - else, insert the bare label.
fn ident_item(label: &str, opened_backtick: bool) -> CompletionItem {
    let apply = if opened_backtick {
        format!("{}`", escape_backtick_inner(label))
    } else if is_plain_ident(label) {
        label.to_string()
    } else {
        format!("`{}`", escape_backtick_inner(label))
    };
    CompletionItem {
        label: label.to_string(),
        apply,
    }
}

fn backtick_before(query: &str, span: Span) -> bool {
    span.from > 0 && query.as_bytes()[span.from - 1] == b'`'
}

fn is_plain_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn escape_backtick_inner(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out
}

/// Case-insensitive prefix filter. The engine applies the same filter to its
/// own option lists; we reproduce it here for cache-backed kinds.
fn filter_by_partial(pool: Vec<String>, partial: &str) -> Vec<String> {
    if partial.is_empty() {
        return pool;
    }
    let needle = partial.to_ascii_lowercase();
    pool.into_iter()
        .filter(|s| s.to_ascii_lowercase().starts_with(&needle))
        .collect()
}

/// Translate our host-side system params into the engine's wire shape.
/// The engine expects labels with the leading `$` (e.g. `"$__interval"`)
/// because that's what users type and what gets fuzzy-matched against the
/// partial token. Our [`SystemParam::name`] stores the bare name.
fn to_engine_params(params: &[SystemParam]) -> Vec<ParamItem> {
    params
        .iter()
        .map(|p| ParamItem {
            label: format!("${}", p.name),
            typ: param_kind_to_engine_type(p.kind),
            optional: false,
        })
        .collect()
}

fn param_kind_to_engine_type(k: ParamKind) -> ParamType {
    match k {
        ParamKind::Duration => ParamType::Duration,
        ParamKind::String => ParamType::String,
        ParamKind::Int => ParamType::Int,
        ParamKind::Float => ParamType::Float,
        ParamKind::Bool => ParamType::Bool,
        ParamKind::Dataset => ParamType::Dataset,
        ParamKind::Metric => ParamType::Metric,
        ParamKind::Regex => ParamType::Regex,
    }
}

/// Short label for the popup title.
pub fn kind_label(kind: &CompletionKind) -> &'static str {
    match kind {
        CompletionKind::Dataset => "dataset",
        CompletionKind::Metric { .. } => "metric",
        CompletionKind::Tag { .. } => "tag",
        CompletionKind::TagValue { .. } => "tag value",
        CompletionKind::Keyword => "keyword",
        CompletionKind::AlignFn => "align fn",
        CompletionKind::MapFn => "map fn",
        CompletionKind::GroupFn => "group fn",
        CompletionKind::BucketFn => "bucket fn",
        CompletionKind::ComputeFn => "compute fn",
        CompletionKind::Param => "param",
    }
}

/// Tag-value position info, recovered by a backwards byte scan from the
/// cursor. Returned by [`detect_tag_value_position`] when the cursor sits
/// to the right of `<tag> <cmp_op>`.
#[derive(Debug, PartialEq, Eq)]
struct TagValueCtx {
    tag: String,
    /// Byte range in the buffer whose contents the accept replaces —
    /// either a bare partial token or the body of an unclosed `"…"`.
    span: Span,
    /// `true` iff the partial begins right after an unescaped `"`. Drives
    /// whether the accepted apply text emits an opening quote.
    in_quotes: bool,
}

/// Try to produce a `TagValue` payload from `cache`. Returns `None` when
/// the cursor isn't in a tag-value position, when `(dataset, metric)`
/// can't be parsed from the buffer, or when the cache holds no values
/// for the resolved `(dataset, metric, tag)`.
fn tag_value_payload(query: &str, cursor: usize, cache: &Cache) -> Option<CompletionPayload> {
    let ctx = detect_tag_value_position(query, cursor)?;
    let (dataset, metric) = axiom::extract_dataset_metric(query).ok()?;
    if dataset.is_empty() || metric.is_empty() {
        return None;
    }
    let pool = cache.tag_values_for(&dataset, &metric, &ctx.tag);
    if pool.is_empty() {
        return None;
    }
    let partial = &query[ctx.span.from..ctx.span.to];
    let items = filter_by_partial(pool, partial)
        .into_iter()
        .map(|label| string_value_item(&label, ctx.in_quotes))
        .collect();
    Some(CompletionPayload {
        kind: CompletionKind::TagValue {
            dataset,
            metric,
            tag: ctx.tag,
        },
        items,
        replace_range: (ctx.span.from, ctx.span.to),
    })
}

/// Backwards byte scan from `cursor` to recognise the
/// `<tag> <cmp_op> ["]<partial>` shape. Tolerates whitespace freely. Stops
/// at newlines so a stale `where` on a previous line can't false-positive.
fn detect_tag_value_position(query: &str, cursor: usize) -> Option<TagValueCtx> {
    let bytes = query.as_bytes();
    let cursor = cursor.min(bytes.len());

    // (1) Locate the start of the value partial.
    //
    //     If an unescaped `"` is reachable to the left without crossing a
    //     newline, the partial is the in-progress string body. Otherwise
    //     it's a bare word ending at `cursor`.
    let mut quote_pos: Option<usize> = None;
    let mut k = cursor;
    while k > 0 {
        let b = bytes[k - 1];
        if b == b'\n' {
            break;
        }
        if b == b'"' && !is_byte_escaped(bytes, k - 1) {
            quote_pos = Some(k - 1);
            break;
        }
        k -= 1;
    }
    let (value_start, in_quotes) = match quote_pos {
        Some(qp) => (qp + 1, true),
        None => {
            let mut j = cursor;
            while j > 0 && is_value_word_byte(bytes[j - 1]) {
                j -= 1;
            }
            (j, false)
        }
    };

    // (2) Walk left of the value partial, skipping `"` if any and whitespace.
    let mut p = value_start;
    if in_quotes && p > 0 && bytes[p - 1] == b'"' {
        p -= 1;
    }
    while p > 0 && matches!(bytes[p - 1], b' ' | b'\t') {
        p -= 1;
    }

    // (3) Require a comparison operator ending at `p`.
    let op_len = cmp_op_len_ending_at(bytes, p)?;
    let mut q = p - op_len;
    while q > 0 && (bytes[q - 1] == b' ' || bytes[q - 1] == b'\t') {
        q -= 1;
    }

    // (4) Tag identifier ends at `q`. Backtick-quoted or plain.
    let tag = ident_ending_at(bytes, q)?;

    Some(TagValueCtx {
        tag,
        span: Span::new(value_start, cursor),
        in_quotes,
    })
}

fn is_byte_escaped(bytes: &[u8], at: usize) -> bool {
    let mut count = 0;
    let mut k = at;
    while k > 0 && bytes[k - 1] == b'\\' {
        count += 1;
        k -= 1;
    }
    count % 2 == 1
}

fn is_value_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
}

/// Returns the length (1 or 2) of a comparison operator ending at `end`,
/// or `None`. Recognises `==`, `!=`, `<=`, `>=`, `<`, `>`.
fn cmp_op_len_ending_at(bytes: &[u8], end: usize) -> Option<usize> {
    if end >= 2 {
        let pair = (bytes[end - 2], bytes[end - 1]);
        if matches!(
            pair,
            (b'=', b'=') | (b'!', b'=') | (b'<', b'=') | (b'>', b'=')
        ) {
            return Some(2);
        }
    }
    if end >= 1
        && (bytes[end - 1] == b'<' || bytes[end - 1] == b'>')
        // Reject if it's the leading char of `<=`/`>=` — those are handled above.
        && bytes.get(end).copied() != Some(b'=')
    {
        return Some(1);
    }
    None
}

/// Returns the backtick-unwrapped or plain identifier ending exactly at
/// `end` byte position, or `None`.
fn ident_ending_at(bytes: &[u8], end: usize) -> Option<String> {
    if end == 0 {
        return None;
    }
    if bytes[end - 1] == b'`' {
        // Backticked. Find the opening backtick.
        let mut k = end - 1;
        while k > 0 && bytes[k - 1] != b'`' {
            k -= 1;
        }
        if k == 0 {
            return None;
        }
        let raw = std::str::from_utf8(&bytes[k..end - 1]).ok()?;
        if raw.is_empty() {
            None
        } else {
            Some(raw.to_string())
        }
    } else {
        let mut j = end;
        while j > 0 && is_value_word_byte(bytes[j - 1]) {
            j -= 1;
        }
        if j == end {
            return None;
        }
        let s = std::str::from_utf8(&bytes[j..end]).ok()?;
        Some(s.to_string())
    }
}

/// Render a tag-value insertion. The body is the value literal with the
/// engine's string-escape rules (we only escape `\\` and `\"` — the MPL
/// string grammar is small). When the user already typed an opening `"`,
/// emit just the body and the closing quote.
fn string_value_item(label: &str, opened_quote: bool) -> CompletionItem {
    let body = escape_string_inner(label);
    let apply = if opened_quote {
        format!("{body}\"")
    } else {
        format!("\"{body}\"")
    };
    CompletionItem {
        label: label.to_string(),
        apply,
    }
}

fn escape_string_inner(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axiom::{DatasetSummary, MetricInfo};
    use std::collections::BTreeMap;

    fn cache_with(datasets: &[&str], metrics: &[(&str, &[&str])]) -> Cache {
        let mut c = Cache::in_memory(String::new());
        c.replace_datasets(
            datasets
                .iter()
                .map(|n| DatasetSummary {
                    name: n.to_string(),
                    description: None,
                    edge_deployment: None,
                    kind: None,
                })
                .collect(),
        );
        for (ds, ms) in metrics {
            let mut map: BTreeMap<String, MetricInfo> = BTreeMap::new();
            for m in *ms {
                map.insert(m.to_string(), MetricInfo::default());
            }
            c.replace_metrics(ds, map);
        }
        c
    }

    /// Find an item by label, panicking with the available labels listed.
    fn find<'a>(items: &'a [CompletionItem], label: &str) -> &'a CompletionItem {
        items.iter().find(|i| i.label == label).unwrap_or_else(|| {
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            panic!("no item {label:?} in {labels:?}")
        })
    }

    #[test]
    fn empty_query_yields_dataset_completion() {
        let cache = cache_with(&["home", "logs"], &[]);
        let p = compute("", 0, &[], &cache).expect("payload");
        assert_eq!(p.kind, CompletionKind::Dataset);
        let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"home"));
        assert!(labels.contains(&"logs"));
    }

    #[test]
    fn after_colon_yields_metric_with_dataset() {
        let cache = cache_with(&["home"], &[("home", &["temp", "switch"])]);
        let p = compute("home:t", 6, &[], &cache).expect("payload");
        match &p.kind {
            CompletionKind::Metric { dataset } => assert_eq!(dataset, "home"),
            other => panic!("unexpected {other:?}"),
        }
        let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"temp"));
        assert!(!labels.contains(&"switch"));
    }

    #[test]
    fn after_align_using_yields_align_fns_from_stdlib() {
        let cache = cache_with(&[], &[]);
        let q = "home:temp | align to 1m using ";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        assert_eq!(p.kind, CompletionKind::AlignFn);
        let avg = find(&p.items, "avg");
        assert_eq!(avg.apply, "avg");
        find(&p.items, "sum");
    }

    #[test]
    fn after_map_yields_map_fns_from_stdlib() {
        let cache = cache_with(&[], &[]);
        let q = "home:temp | map ";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        assert_eq!(p.kind, CompletionKind::MapFn);
        find(&p.items, "rate");
    }

    #[test]
    fn keyword_apply_includes_trailing_space() {
        // The engine ships `apply: "where "` so accepting positions the
        // cursor ready to type the predicate.
        let cache = cache_with(&[], &[]);
        let q = "home:temp | wh";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        assert_eq!(p.kind, CompletionKind::Keyword);
        let where_item = find(&p.items, "where");
        assert_eq!(where_item.apply, "where ");
    }

    #[test]
    fn dataset_apply_is_plain_when_name_is_plain() {
        let cache = cache_with(&["home"], &[]);
        let p = compute("", 0, &[], &cache).expect("payload");
        let item = find(&p.items, "home");
        assert_eq!(item.apply, "home");
    }

    #[test]
    fn dataset_apply_is_backticked_when_name_has_dashes() {
        let cache = cache_with(&["homeassistant-metrics"], &[]);
        let p = compute("", 0, &[], &cache).expect("payload");
        let item = find(&p.items, "homeassistant-metrics");
        assert_eq!(item.apply, "`homeassistant-metrics`");
    }

    #[test]
    fn dataset_apply_with_opened_backtick_emits_only_closing() {
        let cache = cache_with(&["home", "logs"], &[]);
        // The engine advances `span.from` past the opening backtick. With
        // `opened_backtick = true` the apply text is just `body` + closing.
        let p = compute("`hom", 4, &[], &cache).expect("payload");
        assert_eq!(p.replace_range.0, 1, "engine should skip past backtick");
        let item = find(&p.items, "home");
        assert_eq!(item.apply, "home`");
    }

    #[test]
    fn metric_apply_dotted_is_backticked() {
        let cache = cache_with(&["home"], &[("home", &["ha.sensor.temperature"])]);
        let p = compute("home:ha", 7, &[], &cache).expect("payload");
        let item = find(&p.items, "ha.sensor.temperature");
        assert_eq!(item.apply, "`ha.sensor.temperature`");
    }

    #[test]
    fn metric_apply_embedded_backtick_escaped() {
        let cache = cache_with(&["home"], &[("home", &["weird`name"])]);
        let p = compute("home:w", 6, &[], &cache).expect("payload");
        let item = find(&p.items, "weird`name");
        assert_eq!(item.apply, "`weird\\`name`");
    }

    // ── tag + tag-value completion ─────────────────────────────────

    /// Build a cache with one (dataset, metric) plus a tag list and a
    /// value list for one of those tags.
    fn cache_with_tags(
        dataset: &str,
        metric: &str,
        tags: &[&str],
        tag_values: Option<(&str, &[&str])>,
    ) -> Cache {
        let mut c = cache_with(&[dataset], &[(dataset, &[metric])]);
        c.replace_tags(
            dataset,
            metric,
            tags.iter().map(|s| s.to_string()).collect(),
        );
        if let Some((tag, values)) = tag_values {
            c.replace_tag_values(
                dataset,
                metric,
                tag,
                values.iter().map(|s| s.to_string()).collect(),
            );
        }
        c
    }

    #[test]
    fn tag_completion_offers_cached_tag_names() {
        let cache = cache_with_tags("home", "temp", &["host", "region"], None);
        let q = "home:temp | where ";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        match &p.kind {
            CompletionKind::Tag { dataset, metric } => {
                assert_eq!(dataset, "home");
                assert_eq!(metric, "temp");
            }
            other => panic!("unexpected {other:?}"),
        }
        let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"host"), "got {labels:?}");
        assert!(labels.contains(&"region"), "got {labels:?}");
    }

    #[test]
    fn tag_apply_backticks_dotted_name() {
        let cache = cache_with_tags("home", "temp", &["host.name"], None);
        let q = "home:temp | where ho";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        let item = find(&p.items, "host.name");
        assert_eq!(item.apply, "`host.name`");
    }

    #[test]
    fn tag_completion_empty_when_cache_miss() {
        // No tags cached for (home, temp); engine still emits the Tag
        // variant but our adapter surfaces an empty item list. The popup
        // hides on empty.
        let cache = cache_with(&["home"], &[("home", &["temp"])]);
        let q = "home:temp | where ";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        assert!(matches!(p.kind, CompletionKind::Tag { .. }));
        assert!(p.items.is_empty(), "got {:?}", p.items);
    }

    #[test]
    fn tag_value_completion_in_open_string() {
        let cache = cache_with_tags(
            "home",
            "temp",
            &["host"],
            Some(("host", &["web-1", "web-2", "db-1"])),
        );
        let q = "home:temp | where host == \"we";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        match &p.kind {
            CompletionKind::TagValue {
                dataset,
                metric,
                tag,
            } => {
                assert_eq!(dataset, "home");
                assert_eq!(metric, "temp");
                assert_eq!(tag, "host");
            }
            other => panic!("unexpected {other:?}"),
        }
        let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"web-1"), "got {labels:?}");
        assert!(labels.contains(&"web-2"), "got {labels:?}");
        assert!(!labels.contains(&"db-1"), "db-1 should be filtered out");
    }

    #[test]
    fn tag_value_apply_emits_closing_quote_only_when_quote_opened() {
        let cache = cache_with_tags("home", "temp", &["host"], Some(("host", &["web-1"])));
        // Quote already open → apply is just `body"` (no leading quote).
        let q = "home:temp | where host == \"we";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        let item = find(&p.items, "web-1");
        assert_eq!(item.apply, "web-1\"");
    }

    #[test]
    fn tag_value_apply_wraps_in_quotes_when_no_quote_yet() {
        let cache = cache_with_tags("home", "temp", &["host"], Some(("host", &["web-1"])));
        let q = "home:temp | where host == ";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        let item = find(&p.items, "web-1");
        assert_eq!(item.apply, "\"web-1\"");
    }

    #[test]
    fn tag_value_position_respects_backticked_tag() {
        let cache = cache_with_tags(
            "home",
            "temp",
            &["host.name"],
            Some(("host.name", &["web-1"])),
        );
        let q = "home:temp | where `host.name` == \"";
        let p = compute(q, q.len(), &[], &cache).expect("payload");
        match &p.kind {
            CompletionKind::TagValue { tag, .. } => assert_eq!(tag, "host.name"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn tag_value_skipped_when_cache_miss() {
        // No values cached → override doesn't fire; we fall through to the
        // engine, which returns Keywords for the string position.
        let cache = cache_with_tags("home", "temp", &["host"], None);
        let q = "home:temp | where host == \"we";
        match compute(q, q.len(), &[], &cache) {
            None => {}
            Some(p) => {
                assert!(
                    !matches!(p.kind, CompletionKind::TagValue { .. }),
                    "unexpected TagValue payload: {:?}",
                    p.kind
                );
            }
        }
    }

    #[test]
    fn system_param_surfaces_as_param_kind() {
        let cache = cache_with(&[], &[]);
        let sys = vec![SystemParam {
            name: "__interval".to_string(),
            kind: ParamKind::Duration,
        }];
        let q = "home:temp | align to $ using avg";
        let cursor = q.find('$').unwrap() + 1;
        let p = compute(q, cursor, &sys, &cache).expect("payload");
        assert_eq!(p.kind, CompletionKind::Param);
        let item = find(&p.items, "$__interval");
        assert_eq!(item.apply, "$__interval");
    }
}
