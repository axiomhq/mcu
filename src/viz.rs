//! Viz-kind pragma parsing + render dispatch.
//!
//! The pragma is a single comment line, anywhere in the buffer's leading
//! comment block:
//!
//! ```text
//! // @viz line
//! // @viz top_list n=10 by=host
//! // @viz statistic agg=last unit=ms
//! ```
//!
//! Format: `// @viz <kind> [k=v ...]`. Whitespace is ignored. Options
//! after the kind are stored verbatim in a `BTreeMap<String, String>`;
//! typed accessors (`opts.usize("n")`, etc.) live on the consumers that
//! need them.
//!
//! The pragma is *sugar*. The canonical source of the active kind is
//! `Dashboard.tiles[0].kind`. When the buffer changes we re-parse the
//! pragma into a [`VizSpec`] and reconcile it onto the focused tile.
//! When the kind changes via `:viz`, we rewrite (or insert) the pragma
//! on save.

use std::collections::BTreeMap;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Cell as TCell, Dataset, Paragraph, Row, Table},
};

use crate::chart::{self, Series};
use crate::dashboard::VizKind;
use crate::term;

/// Result of parsing the leading-pragma line of a buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VizSpec {
    pub kind: VizKind,
    pub opts: BTreeMap<String, String>,
}

/// What went wrong parsing a pragma line. Surfaced as a diagnostic by
/// the caller — never as a hard error, because a half-typed buffer is a
/// normal mid-edit state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PragmaError {
    /// The line started `// @viz` but had no kind token afterwards.
    MissingKind,
    /// The kind token was not a known [`VizKind`] identifier.
    UnknownKind { token: String },
    /// An option token didn't look like `k=v`.
    MalformedOption { token: String },
}

impl std::fmt::Display for PragmaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PragmaError::MissingKind => f.write_str("`@viz` pragma is missing the kind"),
            PragmaError::UnknownKind { token } => write!(f, "unknown viz kind: `{token}`"),
            PragmaError::MalformedOption { token } => {
                write!(f, "expected `key=value` in viz pragma, got `{token}`")
            }
        }
    }
}

/// Parse the first `// @viz` line of `src`. Returns:
///
/// - `Ok(Some(spec))` — pragma found and parsed.
/// - `Ok(None)` — no pragma line; caller should use [`VizSpec::default`].
/// - `Err((line_index, err))` — pragma line was malformed; caller can
///   surface this as a diagnostic anchored at `line_index` (zero-based).
pub fn parse_pragma(src: &str) -> Result<Option<VizSpec>, (usize, PragmaError)> {
    for (line_idx, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") {
            // Stop scanning once we hit non-comment content; pragmas only
            // live in the leading comment block. Blank lines are allowed
            // between pragma and code, so we don't break on those.
            if trimmed.is_empty() {
                continue;
            }
            break;
        }
        let body = trimmed.trim_start_matches('/').trim_start();
        let Some(rest) = body.strip_prefix("@viz") else {
            continue;
        };
        // Require a space or end-of-line after `@viz` so `@vizfoo` is not
        // a match.
        if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
            continue;
        }
        let rest = rest.trim();
        let mut tokens = rest.split_whitespace();
        let kind_tok = tokens.next().ok_or((line_idx, PragmaError::MissingKind))?;
        let kind = VizKind::parse(kind_tok).ok_or_else(|| {
            (
                line_idx,
                PragmaError::UnknownKind {
                    token: kind_tok.to_string(),
                },
            )
        })?;
        let mut opts = BTreeMap::new();
        for tok in tokens {
            let Some((k, v)) = tok.split_once('=') else {
                return Err((
                    line_idx,
                    PragmaError::MalformedOption {
                        token: tok.to_string(),
                    },
                ));
            };
            opts.insert(k.to_string(), v.to_string());
        }
        return Ok(Some(VizSpec { kind, opts }));
    }
    Ok(None)
}

/// Format a [`VizSpec`] as a pragma line (without a trailing newline).
/// Used by `:viz` to insert/rewrite the line at the top of the buffer.
pub fn format_pragma(spec: &VizSpec) -> String {
    let mut out = format!("// @viz {}", spec.kind.as_str());
    // `BTreeMap` iteration is sorted, so option order is stable.
    for (k, v) in &spec.opts {
        out.push(' ');
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out
}

/// Rewrite (or insert) the pragma line in `src`. Returns the new buffer
/// text. Idempotent: calling it twice with the same spec yields the same
/// output.
pub fn upsert_pragma(src: &str, spec: &VizSpec) -> String {
    let new_line = format_pragma(spec);
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
    // Find the existing `// @viz` line in the leading comment block.
    let mut existing: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") {
            if trimmed.is_empty() {
                continue;
            }
            break;
        }
        let body = trimmed.trim_start_matches('/').trim_start();
        if let Some(rest) = body.strip_prefix("@viz")
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            existing = Some(i);
            break;
        }
    }
    match existing {
        Some(i) => lines[i] = new_line,
        None => lines.insert(0, new_line),
    }
    // Preserve the trailing newline if the original buffer had one. `str::lines`
    // drops it, so we add one back when needed.
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Render the focused tile into `area`. Single-tile mode in step 11+.
///
/// `body` is the tile's underlying text. For metrics tiles this is the
/// MPL query (rendered indirectly through `series`); for `note` it is
/// the markdown body. Most kinds ignore it.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    f: &mut Frame,
    kind: VizKind,
    series: &[Series],
    hidden: &[bool],
    selected: Option<usize>,
    opts: &BTreeMap<String, String>,
    body: &str,
    block: Block<'_>,
    area: Rect,
) {
    match kind {
        VizKind::Line | VizKind::Bar | VizKind::Area | VizKind::Scatter => {
            chart::draw_graph(f, series, hidden, selected, kind, block, area);
        }
        VizKind::Statistic => draw_statistic(f, series, hidden, opts, block, area),
        VizKind::TopList => draw_top_list(f, series, hidden, opts, block, area),
        VizKind::Pie => draw_pie(f, series, hidden, opts, block, area),
        VizKind::Heatmap => draw_heatmap(f, series, hidden, opts, block, area),
        VizKind::Table => draw_table(f, series, hidden, opts, block, area),
        VizKind::LogStream => draw_log_stream(f, opts, block, area),
        VizKind::Note => draw_note(f, body, block, area),
        VizKind::Spacer => draw_spacer(f, block, area),
        VizKind::MonitorList => draw_monitor_list(f, opts, block, area),
    }
}

// Retained for forward-compat: if a future `VizKind` lands ahead of
// its renderer, the match arm in `draw` can route here so the user
// sees an explicit placeholder rather than a build failure.
#[allow(dead_code)]
fn draw_unsupported_placeholder(f: &mut Frame, kind: VizKind, block: Block<'_>, area: Rect) {
    let msg = format!(
        "{} not yet implemented — use `:viz line` to switch back.",
        kind.as_str()
    );
    let placeholder = Paragraph::new(msg)
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray))
        .block(block);
    f.render_widget(placeholder, area);
}

// ── note / spacer / monitor list ───────────────────────────────────────────────────

/// Render `body` as a hand-rolled markdown subset. The supported subset:
///
///   * `# ` / `## ` / `### ` headings
///   * `- ` / `* ` unordered list items
///   * Inline `**bold**`, `*italic*`, and `` `code` ``
///   * Fenced code blocks delimited by ``` lines (rendered with a dim
///     background; nested formatting is suppressed inside).
///
/// Anything outside this set renders as plain text. Pulling in a full
/// markdown crate is overkill for the kind of notes a TUI dashboard
/// typically holds; the subset above is what Axiom's own dashboard
/// notes use in practice.
fn draw_note(f: &mut Frame, body: &str, block: Block<'_>, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // Skip the `// @viz note` pragma line if present; the user's note
    // starts on the line after it.
    let stripped = strip_leading_pragma(body);

    // Empty-note rendering: collapse the bordered 4-row box down to a
    // single thicker horizontal divider line. Skip the `block` entirely
    // so the row reads as a section break rather than an empty tile.
    if stripped.trim().is_empty() {
        let rule_y = area.y + area.height / 2;
        let rule = Rect {
            x: area.x,
            y: rule_y,
            width: area.width,
            height: 1,
        };
        let glyphs: String = "━".repeat(rule.width as usize);
        f.render_widget(
            Paragraph::new(glyphs).style(Style::default().fg(Color::DarkGray)),
            rule,
        );
        return;
    }

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines: Vec<Line<'_>> = render_markdown(stripped);
    f.render_widget(Paragraph::new(lines), inner);
}

fn strip_leading_pragma(body: &str) -> &str {
    let bytes = body.as_bytes();
    let mut i = 0usize;
    // Walk past any leading comment lines that contain `@viz`.
    while i < bytes.len() {
        // Trim leading whitespace on the line.
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        // Comment lines that mention `@viz` are pragmas; skip them.
        if bytes[j..].starts_with(b"//") && body[j..].contains("@viz") {
            // Advance to next newline.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        break;
    }
    &body[i..]
}

fn render_markdown(body: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_code_block = false;
    for raw in body.lines() {
        if raw.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            // The fence itself isn't rendered.
            continue;
        }
        if in_code_block {
            out.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Rgb(20, 20, 20)),
            )));
            continue;
        }
        let trimmed = raw.trim_start();
        // Headings.
        if let Some(rest) = trimmed.strip_prefix("### ") {
            out.push(Line::from(Span::styled(
                format!("  {rest}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            out.push(Line::from(Span::styled(
                format!(" {rest}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            out.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED),
            )));
            continue;
        }
        // Unordered list.
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut spans = vec![Span::raw("  • ")];
            spans.extend(render_inline(rest));
            out.push(Line::from(spans));
            continue;
        }
        out.push(Line::from(render_inline(raw)));
    }
    out
}

/// Render inline markdown: `**bold**`, `*italic*`, `` `code` ``. Naive
/// tokeniser — no nesting, no escapes. Matches the smallest subset
/// people actually use in dashboard notes.
fn render_inline(s: &str) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let bytes = s.as_bytes();
    let mut plain_start = 0usize;
    let flush_plain = |out: &mut Vec<Span<'static>>, s: &str, start: usize, end: usize| {
        if end > start {
            out.push(Span::raw(s[start..end].to_string()));
        }
    };
    while i < bytes.len() {
        if bytes[i] == b'*'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'*'
            && let Some(end_rel) = s[i + 2..].find("**")
        {
            flush_plain(&mut out, s, plain_start, i);
            let inner = &s[i + 2..i + 2 + end_rel];
            out.push(Span::styled(
                inner.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            i += 2 + end_rel + 2;
            plain_start = i;
            continue;
        }
        if bytes[i] == b'*'
            && let Some(end_rel) = s[i + 1..].find('*')
        {
            flush_plain(&mut out, s, plain_start, i);
            let inner = &s[i + 1..i + 1 + end_rel];
            out.push(Span::styled(
                inner.to_string(),
                Style::default().add_modifier(Modifier::ITALIC),
            ));
            i += 1 + end_rel + 1;
            plain_start = i;
            continue;
        }
        if bytes[i] == b'`'
            && let Some(end_rel) = s[i + 1..].find('`')
        {
            flush_plain(&mut out, s, plain_start, i);
            let inner = &s[i + 1..i + 1 + end_rel];
            out.push(Span::styled(
                inner.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Rgb(20, 20, 20)),
            ));
            i += 1 + end_rel + 1;
            plain_start = i;
            continue;
        }
        i += 1;
    }
    flush_plain(&mut out, s, plain_start, bytes.len());
    if out.is_empty() {
        out.push(Span::raw(String::new()));
    }
    out
}

/// The spacer renders nothing inside its pane. Useful for grid layouts
/// (step 18) where the user wants visual breathing room between tiles.
fn draw_spacer(f: &mut Frame, block: Block<'_>, area: Rect) {
    f.render_widget(block, area);
}

/// Stub renderer for `monitor_list`. The monitors REST endpoint lands
/// in step 16b; until then the placeholder shows the next-action hint.
fn draw_monitor_list(
    f: &mut Frame,
    _opts: &BTreeMap<String, String>,
    block: Block<'_>,
    area: Rect,
) {
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = vec![
        Line::from(Span::styled(
            "monitor list",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "renderer ready; GET /v1/monitors fetch wires in step 16b.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

// ── aggregations ────────────────────────────────────────────────────

/// Reduction over a series' `y` values. Surfaces as the `agg=` option on
/// `statistic` and `top_list` pragmas. NaN/infinite values are skipped
/// in every branch so a single bad point can't poison the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    Last,
    First,
    Avg,
    Sum,
    Min,
    Max,
    Count,
}

impl Agg {
    /// Apply this reduction. Returns `None` for an empty input or when
    /// every point is NaN/infinite (or hidden by the caller).
    pub fn apply(self, points: &[(f64, f64)]) -> Option<f64> {
        // Iterator over finite `y` values, preserving input order.
        let finite = || points.iter().map(|p| p.1).filter(|y| y.is_finite());
        match self {
            // Last: walk backwards over the slice to avoid scanning the whole
            // iterator just to take the final element.
            Agg::Last => points.iter().rev().map(|p| p.1).find(|y| y.is_finite()),
            Agg::First => finite().next(),
            Agg::Sum => {
                let mut any = false;
                let mut s = 0.0;
                for y in finite() {
                    s += y;
                    any = true;
                }
                any.then_some(s)
            }
            Agg::Avg => {
                let mut n = 0usize;
                let mut s = 0.0;
                for y in finite() {
                    s += y;
                    n += 1;
                }
                (n > 0).then(|| s / n as f64)
            }
            Agg::Min => finite().reduce(f64::min),
            Agg::Max => finite().reduce(f64::max),
            Agg::Count => {
                // Count is well-defined even when no points are finite: zero.
                Some(finite().count() as f64)
            }
        }
    }

    /// Parse from the lower-case identifier used in pragmas (`agg=last`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "last" => Agg::Last,
            "first" => Agg::First,
            "avg" | "mean" => Agg::Avg,
            "sum" => Agg::Sum,
            "min" => Agg::Min,
            "max" => Agg::Max,
            "count" => Agg::Count,
            _ => return None,
        })
    }
}

// ── statistic ─────────────────────────────────────────────────────────

/// Centered big-number readout of one aggregated series, with a single
/// braille sparkline below. Multi-series queries show the first visible
/// series — documented behaviour; multi-stat tiles come later.
///
/// Options:
///   * `agg`      — `last` (default) / `first` / `avg` / `sum` / `min` / `max` / `count`
///   * `unit`     — free-form suffix appended to the value (`ms`, `req/s`, …)
///   * `decimals` — digits after the decimal point (default 2)
///
/// `compare=` is reserved for a later step (it needs a second query
/// against the prior window, which is harder to retrofit without the
/// per-tile state of step 17).
fn draw_statistic(
    f: &mut Frame,
    series: &[Series],
    hidden: &[bool],
    opts: &BTreeMap<String, String>,
    block: Block<'_>,
    area: Rect,
) {
    let agg = opts
        .get("agg")
        .and_then(|s| Agg::parse(s))
        .unwrap_or(Agg::Last);
    let unit = opts.get("unit").cloned();
    let decimals: usize = opts
        .get("decimals")
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);

    let visible = series
        .iter()
        .enumerate()
        .find(|(i, _)| !hidden.get(*i).copied().unwrap_or(false))
        .map(|(_, s)| s);

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(s) = visible else {
        let p = Paragraph::new("(no data)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    };

    let value_text = match agg.apply(&s.points) {
        Some(v) => format_value(v, decimals, unit.as_deref()),
        None => "—".to_string(),
    };

    // Tight-size the number area to exactly what we render (value +
    // label, plus one row of breathing room when the pane is tall
    // enough) and let the sparkline take everything else. The previous
    // implementation reserved ⅔ of the pane for the number with a
    // sparkline capped at 3 rows, which left up to ~5 empty rows below
    // the label on tall statistic tiles.
    let number_rows: u16 = 2; // value + label
    let pad_top: u16 = if inner.height >= number_rows + 3 { 1 } else { 0 };
    let number_area_h = (pad_top + number_rows).min(inner.height);
    let spark_rows = inner.height.saturating_sub(number_area_h);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(number_area_h),
            Constraint::Length(spark_rows),
        ])
        .split(inner);

    let mut lines: Vec<Line<'_>> = (0..pad_top).map(|_| Line::raw("")).collect();
    lines.push(Line::from(Span::styled(
        value_text,
        Style::default().fg(s.color).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!("{}  [{}]", s.name, agg_label(agg)),
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        chunks[0],
    );
    if spark_rows == 0 {
        return;
    }

    // Sparkline via the same `Chart` widget so axis-free drawing is free.
    // We render an axes-less chart by clipping y to the data range.
    let pts: Vec<(f64, f64)> = s
        .points
        .iter()
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .copied()
        .collect();
    if pts.is_empty() {
        return;
    }
    let (mut x_lo, mut x_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut y_lo, mut y_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &(x, y) in &pts {
        x_lo = x_lo.min(x);
        x_hi = x_hi.max(x);
        y_lo = y_lo.min(y);
        y_hi = y_hi.max(y);
    }
    if (x_hi - x_lo).abs() < f64::EPSILON {
        x_hi += 1.0;
    }
    if (y_hi - y_lo).abs() < f64::EPSILON {
        y_hi += 1.0;
    }
    let ds = Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(ratatui::widgets::GraphType::Line)
        .style(Style::default().fg(s.color))
        .data(&pts);
    let chart = ratatui::widgets::Chart::new(vec![ds])
        .x_axis(ratatui::widgets::Axis::default().bounds([x_lo, x_hi]))
        .y_axis(ratatui::widgets::Axis::default().bounds([y_lo, y_hi]));
    f.render_widget(chart, chunks[1]);
}

fn agg_label(a: Agg) -> &'static str {
    match a {
        Agg::Last => "last",
        Agg::First => "first",
        Agg::Avg => "avg",
        Agg::Sum => "sum",
        Agg::Min => "min",
        Agg::Max => "max",
        Agg::Count => "count",
    }
}

fn format_value(v: f64, decimals: usize, unit: Option<&str>) -> String {
    let body = if v.abs() >= 1e6 || (v != 0.0 && v.abs() < 1e-2) {
        format!("{v:.*e}", decimals)
    } else {
        format!("{v:.*}", decimals)
    };
    match unit {
        Some(u) => format!("{body} {u}"),
        None => body,
    }
}

// ── top list ────────────────────────────────────────────────────────

/// Compute the sorted-and-truncated set of `(series_idx, agg_value)`
/// pairs that the top-list renders. Extracted from the renderer so it's
/// directly unit-testable.
fn top_list_rows(
    series: &[Series],
    hidden: &[bool],
    agg: Agg,
    n: usize,
    ascending: bool,
) -> Vec<(usize, f64)> {
    let mut rows: Vec<(usize, f64)> = series
        .iter()
        .enumerate()
        .filter(|(i, _)| !hidden.get(*i).copied().unwrap_or(false))
        .filter_map(|(i, s)| agg.apply(&s.points).map(|v| (i, v)))
        .collect();
    if ascending {
        rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    } else {
        rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }
    rows.truncate(n);
    rows
}

/// Sorted horizontal bars: one row per series, scaled to the largest
/// aggregated value in the visible set.
///
/// Options:
///   * `agg`        — default `avg`
///   * `n`          — max rows, default `10`
///   * `ascending`  — default `false` (largest first)
fn draw_top_list(
    f: &mut Frame,
    series: &[Series],
    hidden: &[bool],
    opts: &BTreeMap<String, String>,
    block: Block<'_>,
    area: Rect,
) {
    let agg = opts
        .get("agg")
        .and_then(|s| Agg::parse(s))
        .unwrap_or(Agg::Avg);
    let n: usize = opts.get("n").and_then(|s| s.parse().ok()).unwrap_or(10);
    let ascending = opts
        .get("ascending")
        .map(|s| matches!(s.as_str(), "true" | "1" | "yes"))
        .unwrap_or(false);

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = top_list_rows(series, hidden, agg, n.min(inner.height as usize), ascending);
    if rows.is_empty() {
        let p = Paragraph::new("(no data)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    }

    // Scale bars to the largest absolute value so a single negative series
    // doesn't render an empty row.
    let max_abs = rows.iter().map(|(_, v)| v.abs()).fold(0.0_f64, f64::max);

    // Layout per row: [bar 60%]  [label]  [value]
    let label_w = rows
        .iter()
        .map(|(i, _)| series[*i].name.chars().count() as u16)
        .max()
        .unwrap_or(8)
        .clamp(4, inner.width / 3)
        .max(4);
    let value_w: u16 = 10;
    let bar_w = inner.width.saturating_sub(label_w + value_w + 4);

    let lines: Vec<Line<'_>> = rows
        .iter()
        .map(|(idx, v)| {
            let s = &series[*idx];
            let frac = if max_abs > 0.0 {
                v.abs() / max_abs
            } else {
                0.0
            };
            let fill = ((bar_w as f64) * frac).round() as u16;
            let mut bar = String::with_capacity(bar_w as usize);
            for _ in 0..fill {
                bar.push('▇');
            }
            for _ in fill..bar_w {
                bar.push('░');
            }
            Line::from(vec![
                Span::styled(bar, Style::default().fg(s.color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<width$}", s.name, width = label_w as usize),
                    Style::default(),
                ),
                Span::raw("  "),
                Span::styled(
                    format!(
                        "{:>width$}",
                        format_value(*v, 2, None),
                        width = value_w as usize
                    ),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

// ── pie ───────────────────────────────────────────────────────────────────

/// Compute the percentage rows the pie renders: `(series_idx, value,
/// share_0_to_1)`, sorted descending by value, with all-negative or
/// zero-total inputs returning an empty vec. Extracted so it's directly
/// unit-testable.
fn pie_rows(series: &[Series], hidden: &[bool], agg: Agg) -> Vec<(usize, f64, f64)> {
    let raw: Vec<(usize, f64)> = series
        .iter()
        .enumerate()
        .filter(|(i, _)| !hidden.get(*i).copied().unwrap_or(false))
        .filter_map(|(i, s)| agg.apply(&s.points).map(|v| (i, v)))
        // Pie semantics only make sense for non-negative shares.
        .filter(|(_, v)| *v >= 0.0)
        .collect();
    let total: f64 = raw.iter().map(|(_, v)| *v).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let mut rows: Vec<(usize, f64, f64)> =
        raw.into_iter().map(|(i, v)| (i, v, v / total)).collect();
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    rows
}

/// Pie chart rendered as a legend of percentage bars. Donut-glyph mode
/// is reserved for a later step; the row-based layout reads cleanly in
/// a terminal and gives more space to the labels.
///
/// Options:
///   * `agg`   — default `sum`
fn draw_pie(
    f: &mut Frame,
    series: &[Series],
    hidden: &[bool],
    opts: &BTreeMap<String, String>,
    block: Block<'_>,
    area: Rect,
) {
    let agg = opts
        .get("agg")
        .and_then(|s| Agg::parse(s))
        .unwrap_or(Agg::Sum);

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = pie_rows(series, hidden, agg);
    if rows.is_empty() {
        let p = Paragraph::new("(no data — pie requires non-negative aggregates)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    }

    let total: f64 = rows.iter().map(|(_, v, _)| v).sum();
    let bar_w: u16 = inner.width.saturating_sub(28).max(8);
    let label_w: u16 = inner.width.saturating_sub(bar_w + 16).max(8);

    let header = Line::from(vec![
        Span::styled(
            format!("total: {}", format_value(total, 2, None)),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!(
                "({} slice{})",
                rows.len(),
                if rows.len() == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let mut lines = vec![header, Line::raw("")];
    for (idx, v, share) in &rows {
        let s = &series[*idx];
        let fill = ((bar_w as f64) * share).round() as u16;
        let mut bar = String::with_capacity(bar_w as usize);
        for _ in 0..fill {
            bar.push('▇');
        }
        for _ in fill..bar_w {
            bar.push('░');
        }
        let pct = format!("{:>5.1}%", share * 100.0);
        let label = truncate_to_width(&s.name, label_w as usize);
        lines.push(Line::from(vec![
            Span::styled(bar, Style::default().fg(s.color)),
            Span::raw("  "),
            Span::styled(pct, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(
                format!("{label:<width$}", width = label_w as usize),
                Style::default(),
            ),
            Span::raw("  "),
            Span::styled(
                format_value(*v, 2, None),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn truncate_to_width(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ── heatmap ────────────────────────────────────────────────────────────────

/// Bin a set of series into a 2D matrix indexed by `[y_bin][x_bin]`.
/// `y_keys[y]` is the tag-value label for row `y`; cells contain the
/// average of the points that fell into that bin, or `None` when empty.
///
/// `x_bins` and `y_bins` are pre-clamped by the caller; the function
/// assumes both are > 0.
fn heatmap_bin(
    series: &[Series],
    hidden: &[bool],
    by_tag: &str,
    x_bins: usize,
    y_bins: usize,
) -> HeatmapBinned {
    // Bucket series by tag value.
    let mut by_value: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, s) in series.iter().enumerate() {
        if hidden.get(i).copied().unwrap_or(false) {
            continue;
        }
        let Some(v) = s.tags.iter().find(|(k, _)| k == by_tag).map(|(_, v)| v) else {
            continue;
        };
        by_value.entry(v.clone()).or_default().push(i);
    }
    let mut y_keys: Vec<String> = by_value.keys().cloned().collect();
    y_keys.truncate(y_bins);

    // Global x range across the included series.
    let mut x_lo = f64::INFINITY;
    let mut x_hi = f64::NEG_INFINITY;
    for key in &y_keys {
        for i in &by_value[key] {
            for &(x, y) in &series[*i].points {
                if x.is_finite() && y.is_finite() {
                    x_lo = x_lo.min(x);
                    x_hi = x_hi.max(x);
                }
            }
        }
    }
    if !x_lo.is_finite() || !x_hi.is_finite() {
        return HeatmapBinned::empty();
    }
    if (x_hi - x_lo).abs() < f64::EPSILON {
        x_hi += 1.0;
    }

    // Sum + count per cell, then average at the end.
    let mut sum = vec![vec![0.0_f64; x_bins]; y_keys.len()];
    let mut cnt = vec![vec![0_u32; x_bins]; y_keys.len()];
    for (yi, key) in y_keys.iter().enumerate() {
        for i in &by_value[key] {
            for &(x, y) in &series[*i].points {
                if !(x.is_finite() && y.is_finite()) {
                    continue;
                }
                let frac = (x - x_lo) / (x_hi - x_lo);
                let mut xi = (frac * x_bins as f64).floor() as isize;
                if xi >= x_bins as isize {
                    xi = x_bins as isize - 1;
                }
                if xi < 0 {
                    xi = 0;
                }
                let xi = xi as usize;
                sum[yi][xi] += y;
                cnt[yi][xi] += 1;
            }
        }
    }

    let mut cells: Vec<Vec<Option<f64>>> = vec![vec![None; x_bins]; y_keys.len()];
    let (mut v_lo, mut v_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for yi in 0..y_keys.len() {
        for xi in 0..x_bins {
            if cnt[yi][xi] > 0 {
                let avg = sum[yi][xi] / cnt[yi][xi] as f64;
                cells[yi][xi] = Some(avg);
                v_lo = v_lo.min(avg);
                v_hi = v_hi.max(avg);
            }
        }
    }

    HeatmapBinned {
        cells,
        y_keys,
        x_range: (x_lo, x_hi),
        v_range: if v_lo.is_finite() && v_hi.is_finite() {
            Some((v_lo, v_hi))
        } else {
            None
        },
    }
}

struct HeatmapBinned {
    cells: Vec<Vec<Option<f64>>>,
    y_keys: Vec<String>,
    x_range: (f64, f64),
    v_range: Option<(f64, f64)>,
}

impl HeatmapBinned {
    fn empty() -> Self {
        Self {
            cells: Vec::new(),
            y_keys: Vec::new(),
            x_range: (0.0, 1.0),
            v_range: None,
        }
    }
}

/// 2D grid coloured by value. Requires every contributing series to have
/// a tag whose key matches the `by_tag=` option; otherwise the renderer
/// shows a placeholder.
///
/// Options:
///   * `by_tag`     — required; tag key to spread on the y axis.
///   * `x_bins`     — default min(60, inner.width - 12).
///   * `y_bins`     — default inner.height - 2 (one bin per row).
///   * `palette`    — `viridis` (default) or `mono`.
fn draw_heatmap(
    f: &mut Frame,
    series: &[Series],
    hidden: &[bool],
    opts: &BTreeMap<String, String>,
    block: Block<'_>,
    area: Rect,
) {
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(by_tag) = opts.get("by_tag") else {
        let p = Paragraph::new(
            "(heatmap requires `by_tag=<tag>` in the pragma; e.g. `// @viz heatmap by_tag=room`)",
        )
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    };

    // Reserve the rightmost 6 columns for the colour-bar legend.
    let legend_w: u16 = 6;
    let label_w: u16 = inner.width.saturating_sub(legend_w + 4).clamp(6, 20);
    let grid_w = inner.width.saturating_sub(label_w + legend_w + 2);
    let grid_h = inner.height.saturating_sub(1).max(1); // 1 row reserved for the x axis label.

    let x_bins = opts
        .get("x_bins")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(grid_w as usize)
        .min(grid_w as usize)
        .max(1);
    let y_bins = opts
        .get("y_bins")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(grid_h as usize)
        .min(grid_h as usize)
        .max(1);
    let palette = opts.get("palette").map(String::as_str).unwrap_or("viridis");

    let binned = heatmap_bin(series, hidden, by_tag, x_bins, y_bins);
    if binned.y_keys.is_empty() {
        let p = Paragraph::new(format!(
            "(no series tagged with `{by_tag}` — try `:viz heatmap by_tag=<other>`)"
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    }
    let Some((v_lo, v_hi)) = binned.v_range else {
        let p = Paragraph::new("(all bins empty)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    };

    // Layout: [labels label_w] [grid grid_w] [legend legend_w]
    let buf = f.buffer_mut();
    let grid_x0 = inner.x + label_w + 1;
    let grid_y0 = inner.y;
    for (yi, _key) in binned.y_keys.iter().enumerate().take(grid_h as usize) {
        let row_y = grid_y0 + yi as u16;
        // Label, right-aligned in the label column.
        let label = truncate_to_width(&binned.y_keys[yi], label_w as usize);
        let label = format!("{label:>width$}", width = label_w as usize);
        for (ci, ch) in label.chars().enumerate() {
            let cx = inner.x + ci as u16;
            if cx >= inner.x + label_w {
                break;
            }
            buf[(cx, row_y)]
                .set_char(ch)
                .set_style(Style::default().fg(Color::DarkGray));
        }
        // Grid cells.
        for xi in 0..x_bins {
            let cx = grid_x0 + xi as u16;
            if cx >= grid_x0 + grid_w {
                break;
            }
            let cell = binned
                .cells
                .get(yi)
                .and_then(|r| r.get(xi))
                .copied()
                .flatten();
            let bg = match cell {
                Some(v) => palette_color(palette, normalize(v, v_lo, v_hi)),
                None => Color::Reset,
            };
            buf[(cx, row_y)]
                .set_char(' ')
                .set_style(Style::default().bg(bg));
        }
    }

    // Colour-bar legend on the right edge: 3 labels (min/mid/max).
    let legend_x0 = inner.x + label_w + 1 + grid_w + 1;
    for yi in 0..grid_h {
        let t = if grid_h <= 1 {
            1.0
        } else {
            1.0 - (yi as f64) / ((grid_h - 1) as f64)
        };
        let bg = palette_color(palette, t);
        for xi in 0..(legend_w.saturating_sub(4)) {
            let cx = legend_x0 + xi;
            if cx >= inner.x + inner.width {
                break;
            }
            buf[(cx, grid_y0 + yi)]
                .set_char(' ')
                .set_style(Style::default().bg(bg));
        }
    }
    // Numeric labels on the legend column.
    let label_x = legend_x0 + legend_w.saturating_sub(3);
    let labels = [
        (0u16, format_value(v_hi, 1, None)),
        (
            (grid_h / 2).min(grid_h.saturating_sub(1)),
            format_value((v_lo + v_hi) / 2.0, 1, None),
        ),
        (grid_h.saturating_sub(1), format_value(v_lo, 1, None)),
    ];
    for (yi, lbl) in labels {
        let y = grid_y0 + yi;
        for (ci, ch) in lbl.chars().enumerate() {
            let cx = label_x + ci as u16;
            if cx >= inner.x + inner.width {
                break;
            }
            buf[(cx, y)]
                .set_char(ch)
                .set_style(Style::default().fg(Color::Gray));
        }
    }

    // X axis: range label centred under the grid.
    let axis_y = grid_y0 + grid_h;
    if axis_y < inner.y + inner.height {
        let axis_text = format!(
            "{}  ———  {}",
            format_x_label(binned.x_range.0),
            format_x_label(binned.x_range.1)
        );
        let span = Span::styled(axis_text, Style::default().fg(Color::DarkGray));
        let p = Paragraph::new(Line::from(span)).alignment(Alignment::Center);
        let axis_rect = Rect {
            x: grid_x0,
            y: axis_y,
            width: grid_w,
            height: 1,
        };
        f.render_widget(p, axis_rect);
    }
}

fn normalize(v: f64, lo: f64, hi: f64) -> f64 {
    if (hi - lo).abs() < f64::EPSILON {
        return 0.5;
    }
    ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
}

fn format_x_label(v: f64) -> String {
    // Mirror chart.rs's `format_time_label` heuristic for unix-seconds /
    // unix-millis ranges; otherwise fall back to numeric.
    let secs = if v > 9.0e11 && v < 9.0e12 {
        v / 1000.0
    } else if v > 9.0e8 && v < 9.0e9 {
        v
    } else {
        return format_value(v, 1, None);
    };
    let secs_i = secs as i64;
    match time::OffsetDateTime::from_unix_timestamp(secs_i) {
        Ok(dt) => format!("{:02}:{:02}", dt.hour(), dt.minute()),
        Err(_) => format_value(secs, 1, None),
    }
}

/// Map a normalised `t ∈ [0,1]` to a colour using the named palette.
/// `viridis` uses 5 truecolor stops; `mono` uses greyscale. Both fall
/// back to a 5-step indexed approximation when truecolor isn't available.
fn palette_color(palette: &str, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    if term::supports_truecolor() {
        match palette {
            "mono" => {
                let g = (t * 255.0).round() as u8;
                Color::Rgb(g, g, g)
            }
            _ => viridis_rgb(t),
        }
    } else {
        // 5-bucket indexed fallback. Same ordering for both palettes so
        // the colour-bar reads as a gradient regardless of palette.
        let idx = (t * 4.999) as usize;
        match palette {
            "mono" => [
                Color::Black,
                Color::DarkGray,
                Color::Gray,
                Color::White,
                Color::White,
            ][idx],
            _ => [
                Color::Indexed(54),  // dark purple
                Color::Indexed(61),  // blue
                Color::Indexed(36),  // teal
                Color::Indexed(148), // yellow-green
                Color::Indexed(226), // yellow
            ][idx],
        }
    }
}

// ── log stream ───────────────────────────────────────────────────────────────

/// Severity bucket parsed from common log fields. Anything else falls
/// back to `Info`. Used by the renderer to colour rows consistently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // event-decoder lands in the follow-up.
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    #[allow(dead_code)] // consumed by the follow-up event decoder.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "trace" => Level::Trace,
            "debug" => Level::Debug,
            "warn" | "warning" => Level::Warn,
            "error" | "err" => Level::Error,
            "fatal" | "critical" | "crit" => Level::Fatal,
            _ => Level::Info,
        }
    }

    fn color(self) -> Color {
        match self {
            Level::Trace => Color::DarkGray,
            Level::Debug => Color::Gray,
            Level::Info => Color::Cyan,
            Level::Warn => Color::Yellow,
            Level::Error => Color::Red,
            Level::Fatal => Color::Magenta,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }
}

/// One decoded log row. Step 15 ships the type but not the network
/// fetcher; step 15b wires it to `POST /v1/datasets/_apl` and a polling
/// tokio task.
#[derive(Clone, Debug)]
#[allow(dead_code)] // populated by the events decoder in a follow-up.
pub struct EventRow {
    pub time: i64,
    pub level: Level,
    pub message: String,
    pub fields: BTreeMap<String, String>,
}

/// Renderer for the `log_stream` viz. Reads from `app.log_events` once
/// the App-side plumbing for the events endpoint lands; until then
/// shows a placeholder explaining the next step. Importantly the
/// renderer is already a faithful one-pass log-row layout, so wiring
/// real data into `events` is a one-line change.
fn draw_log_stream(f: &mut Frame, _opts: &BTreeMap<String, String>, block: Block<'_>, area: Rect) {
    let events: &[EventRow] = &[]; // wired to `App.log_events` in step 15b.
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if events.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "log stream",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "renderer ready; events fetcher (POST /v1/datasets/_apl) wires in step 15b.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "in the meantime: rows render with `_time / LEVEL / message` once data arrives.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
        return;
    }

    // Tail-first layout: most recent at the bottom (typical for live tail).
    // We render up to `inner.height` rows from the tail of `events`.
    let take = (inner.height as usize).min(events.len());
    let start = events.len() - take;
    let lines: Vec<Line<'_>> = events[start..]
        .iter()
        .map(|ev| {
            let ts = format_x_label(ev.time as f64);
            let lvl_color = ev.level.color();
            Line::from(vec![
                Span::styled(ts, Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(
                    ev.level.label(),
                    Style::default().fg(lvl_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(ev.message.clone()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

// ── table ──────────────────────────────────────────────────────────────────

/// Tabular result from a query. Step 14 only populates this via the
/// `series_to_table` adapter (one row per series, columns = union of
/// tag keys + a value column); step 14b will populate it from an APL
/// `_apl` response.
#[derive(Clone, Debug, PartialEq)]
pub struct TableResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<TableCell>>,
}

/// One cell value. Numeric / textual distinction drives right- vs
/// left-alignment in the renderer and gives the future APL decoder a
/// natural target.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)] // Int/Bool/Time populated by the APL decoder in a follow-up.
pub enum TableCell {
    Null,
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Time(i64),
}

impl TableCell {
    fn is_numeric(&self) -> bool {
        matches!(self, TableCell::Int(_) | TableCell::Float(_))
    }

    fn render(&self) -> String {
        match self {
            TableCell::Null => "—".to_string(),
            TableCell::Int(n) => n.to_string(),
            TableCell::Float(v) => format_value(*v, 2, None),
            TableCell::Str(s) => s.clone(),
            TableCell::Bool(b) => b.to_string(),
            TableCell::Time(t) => format_x_label(*t as f64),
        }
    }
}

/// Adapt a list of series into a tabular form. Each series becomes a
/// row; columns are the sorted union of tag keys plus a final `value`
/// column carrying the per-series aggregate. Hidden series are dropped.
///
/// This is the bridge between today's metrics-MPL response shape and
/// the table renderer — step 14b adds a parallel constructor that
/// decodes an APL `_apl` response directly into [`TableResult`].
pub fn series_to_table(series: &[Series], hidden: &[bool], agg: Agg) -> TableResult {
    // Stable column order: tag keys sorted alphabetically, value last.
    let mut tag_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (i, s) in series.iter().enumerate() {
        if hidden.get(i).copied().unwrap_or(false) {
            continue;
        }
        for (k, _) in &s.tags {
            tag_keys.insert(k.clone());
        }
    }
    let mut columns: Vec<String> = tag_keys.iter().cloned().collect();
    columns.push("value".to_string());

    let mut rows = Vec::with_capacity(series.len());
    for (i, s) in series.iter().enumerate() {
        if hidden.get(i).copied().unwrap_or(false) {
            continue;
        }
        let mut row: Vec<TableCell> = Vec::with_capacity(columns.len());
        for key in &tag_keys {
            match s.tags.iter().find(|(k, _)| k == key) {
                Some((_, v)) => row.push(TableCell::Str(v.clone())),
                None => row.push(TableCell::Null),
            }
        }
        row.push(
            agg.apply(&s.points)
                .map(TableCell::Float)
                .unwrap_or(TableCell::Null),
        );
        rows.push(row);
    }
    TableResult { columns, rows }
}

/// Render a [`TableResult`]-shaped view of the visible series. Uses
/// `ratatui::widgets::Table`; columns are sized greedily by max content
/// width, capped per column so a wide string column doesn't squeeze out
/// the rest.
///
/// Options:
///   * `agg` — default `last`. Reused by `series_to_table` for the
///     `value` column.
fn draw_table(
    f: &mut Frame,
    series: &[Series],
    hidden: &[bool],
    opts: &BTreeMap<String, String>,
    block: Block<'_>,
    area: Rect,
) {
    let agg = opts
        .get("agg")
        .and_then(|s| Agg::parse(s))
        .unwrap_or(Agg::Last);
    let t = series_to_table(series, hidden, agg);

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if t.rows.is_empty() {
        let p = Paragraph::new("(no data)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    }

    // Per-column max content width (header included), capped at 20 to
    // keep one bad column from eating the pane.
    let col_count = t.columns.len();
    let mut widths: Vec<u16> = t.columns.iter().map(|c| c.chars().count() as u16).collect();
    for row in &t.rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.render().chars().count() as u16);
            }
        }
    }
    for w in &mut widths {
        *w = (*w).min(20);
    }
    let constraints: Vec<Constraint> = widths.iter().map(|w| Constraint::Length(*w + 2)).collect();

    let header = Row::new(t.columns.iter().map(|c| {
        TCell::from(c.clone()).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    }));

    let rows: Vec<Row> = t
        .rows
        .iter()
        .map(|r| {
            let cells: Vec<TCell> = r
                .iter()
                .map(|c| {
                    let style = if c.is_numeric() {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    TCell::from(c.render()).style(style)
                })
                .collect();
            Row::new(cells)
        })
        .collect();

    let _ = col_count;
    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(1);
    f.render_widget(table, inner);
}

/// 5-stop linear viridis approximation. Numbers picked from the
/// canonical viridis colourmap; close enough for a TUI heatmap.
fn viridis_rgb(t: f64) -> Color {
    const STOPS: &[(f64, (u8, u8, u8))] = &[
        (0.00, (68, 1, 84)),
        (0.25, (59, 82, 139)),
        (0.50, (33, 145, 140)),
        (0.75, (94, 201, 98)),
        (1.00, (253, 231, 37)),
    ];
    for w in STOPS.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if t <= t1 {
            let span = t1 - t0;
            let k = if span > 0.0 { (t - t0) / span } else { 0.0 };
            let lerp =
                |a: u8, b: u8| -> u8 { (a as f64 + (b as f64 - a as f64) * k).round() as u8 };
            return Color::Rgb(lerp(c0.0, c1.0), lerp(c0.1, c1.1), lerp(c0.2, c1.2));
        }
    }
    let last = STOPS.last().unwrap().1;
    Color::Rgb(last.0, last.1, last.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: VizKind) -> VizSpec {
        VizSpec {
            kind,
            opts: BTreeMap::new(),
        }
    }

    #[test]
    fn parse_returns_none_when_no_pragma() {
        assert_eq!(parse_pragma("home:temp | align to 1m"), Ok(None));
        assert_eq!(parse_pragma(""), Ok(None));
        assert_eq!(parse_pragma("// just a normal comment\nfoo"), Ok(None));
    }

    #[test]
    fn parse_finds_pragma_at_top() {
        let got = parse_pragma("// @viz bar\nhome:temp").unwrap().unwrap();
        assert_eq!(got, spec(VizKind::Bar));
    }

    #[test]
    fn parse_allows_leading_whitespace_and_blank_lines() {
        let got = parse_pragma("\n  // @viz scatter\nhome:temp")
            .unwrap()
            .unwrap();
        assert_eq!(got, spec(VizKind::Scatter));
    }

    #[test]
    fn parse_collects_options() {
        let got = parse_pragma("// @viz top_list n=10 by=host\nfoo")
            .unwrap()
            .unwrap();
        assert_eq!(got.kind, VizKind::TopList);
        assert_eq!(got.opts.get("n").map(String::as_str), Some("10"));
        assert_eq!(got.opts.get("by").map(String::as_str), Some("host"));
    }

    #[test]
    fn parse_stops_at_first_non_comment() {
        // The pragma is below a real code line — must not be parsed.
        let got = parse_pragma("home:temp\n// @viz bar").unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn parse_reports_unknown_kind_with_line_index() {
        let err = parse_pragma("// @viz nope\nfoo").unwrap_err();
        assert_eq!(err.0, 0);
        assert!(matches!(err.1, PragmaError::UnknownKind { .. }));
    }

    #[test]
    fn parse_reports_missing_kind() {
        let err = parse_pragma("// @viz\nfoo").unwrap_err();
        assert_eq!(err.0, 0);
        assert_eq!(err.1, PragmaError::MissingKind);
    }

    #[test]
    fn parse_reports_malformed_option() {
        let err = parse_pragma("// @viz line broken-token\nfoo").unwrap_err();
        assert!(matches!(err.1, PragmaError::MalformedOption { .. }));
    }

    #[test]
    fn parse_ignores_at_vizfoo_lookalike() {
        // `@vizfoo` is not `@viz`. Must be treated as a plain comment.
        assert_eq!(parse_pragma("// @vizfoo bar\nx"), Ok(None));
    }

    #[test]
    fn format_round_trips_with_parse() {
        let mut opts = BTreeMap::new();
        opts.insert("n".to_string(), "5".to_string());
        opts.insert("agg".to_string(), "avg".to_string());
        let s = VizSpec {
            kind: VizKind::TopList,
            opts,
        };
        let line = format_pragma(&s);
        let buf = format!("{line}\nfoo");
        assert_eq!(parse_pragma(&buf).unwrap(), Some(s));
    }

    #[test]
    fn upsert_inserts_when_missing() {
        let out = upsert_pragma("home:temp\n", &spec(VizKind::Bar));
        assert_eq!(out, "// @viz bar\nhome:temp\n");
    }

    #[test]
    fn upsert_rewrites_existing_in_place() {
        let out = upsert_pragma("// @viz line\nhome:temp\n", &spec(VizKind::Scatter));
        assert_eq!(out, "// @viz scatter\nhome:temp\n");
    }

    #[test]
    fn upsert_is_idempotent() {
        let once = upsert_pragma("home:temp\n", &spec(VizKind::Area));
        let twice = upsert_pragma(&once, &spec(VizKind::Area));
        assert_eq!(once, twice);
    }

    #[test]
    fn upsert_preserves_absence_of_trailing_newline() {
        let out = upsert_pragma("home:temp", &spec(VizKind::Bar));
        assert!(!out.ends_with('\n'));
    }

    // ── Agg ────────────────────────────────────────────────────────────

    fn pts(ys: &[f64]) -> Vec<(f64, f64)> {
        ys.iter().enumerate().map(|(i, y)| (i as f64, *y)).collect()
    }

    #[test]
    fn agg_empty_input_is_none_except_count() {
        assert_eq!(Agg::Last.apply(&[]), None);
        assert_eq!(Agg::Avg.apply(&[]), None);
        assert_eq!(Agg::Sum.apply(&[]), None);
        assert_eq!(Agg::Count.apply(&[]), Some(0.0));
    }

    #[test]
    fn agg_skips_non_finite() {
        let p = pts(&[1.0, f64::NAN, 3.0, f64::INFINITY, 5.0]);
        assert_eq!(Agg::Sum.apply(&p), Some(9.0));
        assert_eq!(Agg::Avg.apply(&p), Some(3.0));
        assert_eq!(Agg::Min.apply(&p), Some(1.0));
        assert_eq!(Agg::Max.apply(&p), Some(5.0));
        assert_eq!(Agg::Count.apply(&p), Some(3.0));
    }

    #[test]
    fn agg_first_last_preserve_order() {
        let p = pts(&[7.0, 3.0, 9.0, 1.0]);
        assert_eq!(Agg::First.apply(&p), Some(7.0));
        assert_eq!(Agg::Last.apply(&p), Some(1.0));
    }

    #[test]
    fn agg_parses_canonical_and_aliases() {
        assert_eq!(Agg::parse("avg"), Some(Agg::Avg));
        assert_eq!(Agg::parse("mean"), Some(Agg::Avg));
        assert_eq!(Agg::parse("count"), Some(Agg::Count));
        assert_eq!(Agg::parse("nope"), None);
    }

    // ── top_list_rows ────────────────────────────────────────────────────

    fn mkseries(name: &str, ys: &[f64]) -> Series {
        Series {
            name: name.to_string(),
            tags: vec![],
            points: pts(ys),
            color: ratatui::style::Color::Cyan,
        }
    }

    #[test]
    fn top_list_sorts_desc_by_default_and_caps_at_n() {
        let s = vec![
            mkseries("a", &[1.0, 1.0, 1.0]), // avg 1
            mkseries("b", &[3.0, 3.0, 3.0]), // avg 3
            mkseries("c", &[2.0, 2.0, 2.0]), // avg 2
        ];
        let rows = top_list_rows(&s, &[false; 3], Agg::Avg, 2, false);
        assert_eq!(rows.len(), 2);
        // Largest first.
        assert_eq!(s[rows[0].0].name, "b");
        assert_eq!(s[rows[1].0].name, "c");
    }

    #[test]
    fn top_list_ascending_reverses_order() {
        let s = vec![
            mkseries("a", &[1.0]),
            mkseries("b", &[3.0]),
            mkseries("c", &[2.0]),
        ];
        let rows = top_list_rows(&s, &[false; 3], Agg::Last, 10, true);
        assert_eq!(s[rows[0].0].name, "a");
        assert_eq!(s[rows[2].0].name, "b");
    }

    #[test]
    fn top_list_skips_hidden_series() {
        let s = vec![
            mkseries("a", &[1.0]),
            mkseries("b", &[3.0]),
            mkseries("c", &[2.0]),
        ];
        let hidden = vec![false, true, false];
        let rows = top_list_rows(&s, &hidden, Agg::Last, 10, false);
        assert_eq!(rows.len(), 2);
        for (i, _) in &rows {
            assert_ne!(s[*i].name, "b");
        }
    }

    #[test]
    fn top_list_drops_all_nan_series() {
        let s = vec![
            mkseries("good", &[1.0, 2.0]),
            mkseries("bad", &[f64::NAN, f64::NAN]),
        ];
        // `Avg` on all-NaN returns None → series dropped.
        let rows = top_list_rows(&s, &[false; 2], Agg::Avg, 10, false);
        assert_eq!(rows.len(), 1);
        assert_eq!(s[rows[0].0].name, "good");
    }

    // ── format_value ─────────────────────────────────────────────────────

    #[test]
    fn format_value_appends_unit_when_present() {
        assert_eq!(format_value(2.50, 2, Some("ms")), "2.50 ms");
        assert_eq!(format_value(2.50, 2, None), "2.50");
    }

    #[test]
    fn format_value_uses_scientific_for_extreme_magnitudes() {
        let big = format_value(1.2e9, 2, None);
        assert!(big.contains('e'), "expected scientific notation, got {big}");
        let tiny = format_value(1.2e-4, 2, None);
        assert!(
            tiny.contains('e'),
            "expected scientific notation, got {tiny}"
        );
    }

    // ── pie ────────────────────────────────────────────────────────────

    #[test]
    fn pie_rows_normalises_shares_to_one() {
        let s = vec![mkseries("a", &[10.0]), mkseries("b", &[30.0])];
        let rows = pie_rows(&s, &[false; 2], Agg::Sum);
        let total_share: f64 = rows.iter().map(|(_, _, share)| share).sum();
        assert!((total_share - 1.0).abs() < 1e-9);
        assert_eq!(s[rows[0].0].name, "b");
    }

    #[test]
    fn pie_rows_empty_when_total_nonpositive() {
        let s = vec![mkseries("a", &[-1.0])];
        assert!(pie_rows(&s, &[false; 1], Agg::Sum).is_empty());
        assert!(pie_rows(&[], &[], Agg::Sum).is_empty());
    }

    #[test]
    fn pie_rows_drops_negative_aggregates() {
        let s = vec![mkseries("good", &[5.0]), mkseries("bad", &[-2.0])];
        let rows = pie_rows(&s, &[false; 2], Agg::Sum);
        assert_eq!(rows.len(), 1);
        assert_eq!(s[rows[0].0].name, "good");
    }

    // ── heatmap ─────────────────────────────────────────────────────────

    fn tagged(name: &str, tag: &str, ys: &[f64]) -> Series {
        Series {
            name: name.to_string(),
            tags: vec![("room".to_string(), tag.to_string())],
            points: pts(ys),
            color: ratatui::style::Color::Cyan,
        }
    }

    #[test]
    fn heatmap_bin_groups_by_tag_value_and_averages_per_cell() {
        let s = vec![
            tagged("a", "kitchen", &[1.0, 2.0]),
            tagged("b", "kitchen", &[3.0, 4.0]),
            tagged("c", "hall", &[10.0, 20.0]),
        ];
        let b = heatmap_bin(&s, &[false; 3], "room", 2, 4);
        assert_eq!(b.y_keys.len(), 2);
        assert!(b.y_keys.contains(&"kitchen".to_string()));
        assert!(b.y_keys.contains(&"hall".to_string()));
        let ki = b.y_keys.iter().position(|k| k == "kitchen").unwrap();
        assert_eq!(b.cells[ki][0], Some(2.0));
        assert_eq!(b.cells[ki][1], Some(3.0));
        let (lo, hi) = b.v_range.unwrap();
        assert_eq!(lo, 2.0);
        assert_eq!(hi, 20.0);
    }

    #[test]
    fn heatmap_bin_returns_empty_when_no_series_have_the_tag() {
        let s = vec![mkseries("a", &[1.0])];
        let b = heatmap_bin(&s, &[false; 1], "room", 2, 2);
        assert!(b.y_keys.is_empty());
        assert!(b.v_range.is_none());
    }

    // ── palette ────────────────────────────────────────────────────────

    #[test]
    fn viridis_green_channel_is_largely_monotonic() {
        // Green rises 1 → 231 across viridis; we allow tiny non-monotonic
        // dips from the linear interpolation between hand-picked stops.
        let mut prev = 0u8;
        for i in 0..=10 {
            let t = i as f64 / 10.0;
            if let Color::Rgb(_, g, _) = viridis_rgb(t) {
                assert!(
                    g >= prev || (prev as i32 - g as i32).abs() < 10,
                    "green channel regressed at t={t}: prev={prev} new={g}"
                );
                prev = g;
            }
        }
    }

    // ── table ────────────────────────────────────────────────────────────

    #[test]
    fn series_to_table_collects_tag_columns_alphabetically_then_value() {
        let s = vec![
            Series {
                name: "a".into(),
                tags: vec![
                    ("zone".into(), "east".into()),
                    ("host".into(), "db-1".into()),
                ],
                points: pts(&[1.0, 2.0, 3.0]),
                color: ratatui::style::Color::Cyan,
            },
            Series {
                name: "b".into(),
                tags: vec![("host".into(), "db-2".into())],
                points: pts(&[10.0, 20.0]),
                color: ratatui::style::Color::Yellow,
            },
        ];
        let t = series_to_table(&s, &[false; 2], Agg::Last);
        assert_eq!(t.columns, vec!["host", "zone", "value"]);
        assert_eq!(t.rows.len(), 2);
        // Row 0: host=db-1, zone=east, value=3.0 (Agg::Last)
        assert_eq!(t.rows[0][0], TableCell::Str("db-1".into()));
        assert_eq!(t.rows[0][1], TableCell::Str("east".into()));
        assert_eq!(t.rows[0][2], TableCell::Float(3.0));
        // Row 1: host=db-2, zone=NULL (missing), value=20.0
        assert_eq!(t.rows[1][0], TableCell::Str("db-2".into()));
        assert_eq!(t.rows[1][1], TableCell::Null);
        assert_eq!(t.rows[1][2], TableCell::Float(20.0));
    }

    #[test]
    fn series_to_table_skips_hidden_series() {
        let s = vec![
            Series {
                name: "a".into(),
                tags: vec![("h".into(), "x".into())],
                points: pts(&[1.0]),
                color: ratatui::style::Color::Cyan,
            },
            Series {
                name: "b".into(),
                tags: vec![("h".into(), "y".into())],
                points: pts(&[2.0]),
                color: ratatui::style::Color::Yellow,
            },
        ];
        let t = series_to_table(&s, &[false, true], Agg::Last);
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0][0], TableCell::Str("x".into()));
    }

    // ── note (mini-markdown) ─────────────────────────────────────────────────

    fn render_to_text(body: &str) -> Vec<String> {
        render_markdown(body)
            .into_iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn markdown_renders_headings_with_indents() {
        let txt = render_to_text("# H1\n## H2\n### H3");
        assert!(txt[0].ends_with("H1"));
        assert!(txt[1].ends_with("H2"));
        assert!(txt[2].ends_with("H3"));
    }

    #[test]
    fn markdown_renders_list_bullets() {
        let txt = render_to_text("- alpha\n- beta");
        assert!(txt[0].contains('•'));
        assert!(txt[0].contains("alpha"));
        assert!(txt[1].contains("beta"));
    }

    #[test]
    fn markdown_inline_bold_italic_code() {
        let lines = render_markdown("hello **bold** *it* `c` end");
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        let joined: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(joined, "hello bold it c end");
        let styled: Vec<_> = line
            .spans
            .iter()
            .filter(|s| s.style != Style::default())
            .map(|s| s.content.to_string())
            .collect();
        assert!(styled.contains(&"bold".to_string()));
        assert!(styled.contains(&"it".to_string()));
        assert!(styled.contains(&"c".to_string()));
    }

    #[test]
    fn markdown_code_fence_blocks_swallow_inline_formatting() {
        let lines = render_markdown("```\nlet x = **not bold**;\n```");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert!(lines[0].spans[0].content.contains("**not bold**"));
    }

    #[test]
    fn strip_leading_pragma_removes_just_the_pragma_line() {
        let body = "// @viz note\n# Title\n\nbody text\n";
        let stripped = strip_leading_pragma(body);
        assert!(stripped.starts_with("# Title"));
    }

    #[test]
    fn strip_leading_pragma_is_noop_without_pragma() {
        let body = "# Title\n";
        assert_eq!(strip_leading_pragma(body), body);
    }

    // ── log stream ────────────────────────────────────────────────────────

    #[test]
    fn level_parses_common_aliases() {
        assert_eq!(Level::parse("trace"), Level::Trace);
        assert_eq!(Level::parse("DEBUG"), Level::Debug);
        assert_eq!(Level::parse("warn"), Level::Warn);
        assert_eq!(Level::parse("warning"), Level::Warn);
        assert_eq!(Level::parse("err"), Level::Error);
        assert_eq!(Level::parse("FATAL"), Level::Fatal);
        assert_eq!(Level::parse("crit"), Level::Fatal);
        // Unknown → Info.
        assert_eq!(Level::parse("notice"), Level::Info);
        assert_eq!(Level::parse(""), Level::Info);
    }

    #[test]
    fn table_cell_render_handles_each_variant() {
        assert_eq!(TableCell::Null.render(), "—");
        assert_eq!(TableCell::Int(42).render(), "42");
        assert_eq!(TableCell::Float(2.5).render(), "2.50");
        assert_eq!(TableCell::Str("hi".into()).render(), "hi");
        assert_eq!(TableCell::Bool(true).render(), "true");
    }

    #[test]
    fn normalize_handles_constant_range() {
        assert_eq!(normalize(5.0, 5.0, 5.0), 0.5);
        assert_eq!(normalize(0.0, 0.0, 10.0), 0.0);
        assert_eq!(normalize(10.0, 0.0, 10.0), 1.0);
        assert_eq!(normalize(-1.0, 0.0, 10.0), 0.0);
        assert_eq!(normalize(100.0, 0.0, 10.0), 1.0);
    }
}
