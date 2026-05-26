use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Chart, Dataset, GraphType, List, ListItem, ListState, Paragraph},
};

use crate::dashboard::VizKind;

/// One named time-series.
#[derive(Clone, Debug)]
pub struct Series {
    pub name: String,
    /// Tag bindings on this series. Values keep their wire JSON type;
    /// use [`tag_text`] to format them for the UI — `Value::Display`
    /// is JSON encoding so strings come back quoted.
    pub tags: Vec<(String, serde_json::Value)>,
    pub points: Vec<(f64, f64)>,
    pub color: Color,
}

/// Plain-text view of a tag value: unwrap `Value::String` directly so
/// the surrounding JSON quotes are gone; let `Display` handle every
/// other variant (numbers, booleans, arrays render naturally).
pub fn tag_text(v: &serde_json::Value) -> String {
    v.as_str()
        .map(String::from)
        .unwrap_or_else(|| v.to_string())
}

/// Stable color palette used to assign colors to series in order.
pub const PALETTE: &[Color] = &[
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Magenta,
    Color::Red,
    Color::Blue,
    Color::LightCyan,
    Color::LightYellow,
];

pub fn color_for(index: usize) -> Color {
    PALETTE[index % PALETTE.len()]
}

/// Axis bounds derived from a set of series.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: [f64; 2],
    pub y: [f64; 2],
}

impl Bounds {
    /// Safe default for empty data.
    pub fn empty() -> Self {
        Self {
            x: [0.0, 1.0],
            y: [0.0, 1.0],
        }
    }

    pub fn from_series(series: &[Series]) -> Self {
        let mut x_min = f64::INFINITY;
        let mut x_max = f64::NEG_INFINITY;
        let mut y_min = f64::INFINITY;
        let mut y_max = f64::NEG_INFINITY;
        let mut any = false;

        for s in series {
            for &(x, y) in &s.points {
                if !x.is_finite() || !y.is_finite() {
                    continue;
                }
                any = true;
                if x < x_min {
                    x_min = x;
                }
                if x > x_max {
                    x_max = x;
                }
                if y < y_min {
                    y_min = y;
                }
                if y > y_max {
                    y_max = y;
                }
            }
        }

        if !any {
            return Self::empty();
        }

        let (x_lo, x_hi) = pad_axis(x_min, x_max, 0.0);
        let (y_lo, y_hi) = pad_axis(y_min, y_max, 0.05);

        Self {
            x: [x_lo, x_hi],
            y: [y_lo, y_hi],
        }
    }
}

/// Expand `[min, max]` so a constant value or single point still has a visible span,
/// then add proportional padding to non-constant ranges.
fn pad_axis(min: f64, max: f64, pad_frac: f64) -> (f64, f64) {
    if (max - min).abs() < f64::EPSILON {
        let pad = min.abs().max(1.0) * 0.05;
        return (min - pad, max + pad);
    }
    let pad = (max - min) * pad_frac;
    (min - pad, max + pad)
}

fn format_label(v: f64) -> String {
    if v.abs() >= 1000.0 || (v != 0.0 && v.abs() < 0.01) {
        format!("{v:.2e}")
    } else {
        format!("{v:.2}")
    }
}

fn axis_labels(lo: f64, hi: f64) -> [String; 3] {
    let mid = (lo + hi) / 2.0;
    [format_label(lo), format_label(mid), format_label(hi)]
}

/// X-axis label formatter that detects unix timestamps and renders
/// `HH:MM` for short windows or `MM-DD HH:MM` for longer ones. Falls
/// back to numeric `axis_labels` for non-time data (synthetic demo,
/// non-temporal queries).
fn x_axis_labels(lo: f64, hi: f64) -> [String; 3] {
    if let Some(unit) = detect_time_unit(lo, hi) {
        let lo_s = unit.to_seconds(lo);
        let hi_s = unit.to_seconds(hi);
        let mid_s = (lo_s + hi_s) / 2.0;
        let span_secs = (hi_s - lo_s).abs();
        let use_date = span_secs > 24.0 * 3600.0;
        return [
            format_time_label(lo_s, use_date),
            format_time_label(mid_s, use_date),
            format_time_label(hi_s, use_date),
        ];
    }
    axis_labels(lo, hi)
}

#[derive(Clone, Copy)]
enum TimeUnit {
    Seconds,
    Millis,
}

impl TimeUnit {
    fn to_seconds(self, v: f64) -> f64 {
        match self {
            TimeUnit::Seconds => v,
            TimeUnit::Millis => v / 1000.0,
        }
    }
}

fn detect_time_unit(lo: f64, hi: f64) -> Option<TimeUnit> {
    // Plausible unix-seconds range: ~2001-09-09 .. ~2255 — (1e9, 9e9).
    // Plausible unix-millis range : same window × 1000.
    if lo > 9.0e11 && hi < 9.0e12 {
        Some(TimeUnit::Millis)
    } else if lo > 9.0e8 && hi < 9.0e9 {
        Some(TimeUnit::Seconds)
    } else {
        None
    }
}

fn format_time_label(secs: f64, use_date: bool) -> String {
    let secs_i = secs as i64;
    let Ok(dt) = time::OffsetDateTime::from_unix_timestamp(secs_i) else {
        return format_label(secs);
    };
    if use_date {
        format!(
            "{:02}-{:02} {:02}:{:02}",
            dt.month() as u8,
            dt.day(),
            dt.hour(),
            dt.minute()
        )
    } else {
        format!("{:02}:{:02}", dt.hour(), dt.minute())
    }
}

/// Map a [`VizKind`] to the `(marker, graph_type)` pair ratatui uses to
/// draw a [`Dataset`]. Only the time-series-family kinds (Line / Bar /
/// Area / Scatter) are routed through here; the rest fall through to a
/// placeholder in [`crate::viz::draw`].
///
/// Notes on the mapping:
///   * `Area` is approximated by combining the Bar marker (solid block
///     characters) with `GraphType::Line` — ratatui has no dedicated
///     filled-area type yet, but Bar+Line renders as a thicker,
///     baseline-anchored stroke that reads as "area" at a glance.
///   * `Bar`  uses Bar marker + `GraphType::Bar`, which actually draws
///     baseline-to-point bars per data point.
///   * `Scatter` uses Dot marker + `GraphType::Scatter` so individual
///     points don't get connected.
fn marker_and_type_for(kind: VizKind) -> (symbols::Marker, GraphType) {
    match kind {
        VizKind::Bar => (symbols::Marker::Bar, GraphType::Bar),
        VizKind::Area => (symbols::Marker::Bar, GraphType::Line),
        VizKind::Scatter => (symbols::Marker::Dot, GraphType::Scatter),
        // Everything else (including the not-yet-implemented kinds that
        // viz::draw routes through the placeholder) renders as a line.
        _ => (symbols::Marker::Braille, GraphType::Line),
    }
}

/// Render the chart, skipping series flagged in `hidden`. When the
/// legend has focus and a series is selected, every other visible series
/// is rendered in dark grey so the selected one stands out by contrast.
/// The marker/graph-type pair is chosen by `kind`; see
/// [`marker_and_type_for`] for the mapping.
#[allow(clippy::too_many_arguments)]
pub fn draw_graph(
    f: &mut Frame,
    series: &[Series],
    hidden: &[bool],
    selected: Option<usize>,
    kind: VizKind,
    block: Block<'_>,
    area: Rect,
) {
    let visible: Vec<(usize, &Series)> = series
        .iter()
        .enumerate()
        .filter(|(i, _)| !hidden.get(*i).copied().unwrap_or(false))
        .collect();
    let has_data = visible.iter().any(|(_, s)| !s.points.is_empty());
    if !has_data {
        let placeholder = Paragraph::new("No data — press Enter to run query")
            .alignment(Alignment::Center)
            .block(block);
        f.render_widget(placeholder, area);
        return;
    }

    // Bounds only consider visible series so the y-axis isn't dragged by
    // a hidden outlier.
    let visible_series: Vec<Series> = visible.iter().map(|(_, s)| (*s).clone()).collect();
    let bounds = Bounds::from_series(&visible_series);
    // ratatui's `Chart` paints datasets in order; later datasets win on
    // overlap. Render non-selected first so the selected series visibly
    // sits on top of the dimmed background.
    let mut ordered: Vec<&(usize, &Series)> = visible.iter().collect();
    if let Some(sel) = selected {
        ordered.sort_by_key(|(i, _)| (*i == sel) as u8);
    }
    let (marker, graph_type) = marker_and_type_for(kind);
    let datasets: Vec<Dataset<'_>> = ordered
        .into_iter()
        .filter(|(_, s)| !s.points.is_empty())
        .map(|(idx, s)| {
            let dimmed = selected.is_some() && selected != Some(*idx);
            let style = if dimmed {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(s.color)
            };
            Dataset::default()
                .name(s.name.clone())
                .marker(marker)
                .graph_type(graph_type)
                .style(style)
                .data(&s.points)
        })
        .collect();

    let x_labels = x_axis_labels(bounds.x[0], bounds.x[1]);
    let y_labels = axis_labels(bounds.y[0], bounds.y[1]);

    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(Axis::default().bounds(bounds.x).labels(x_labels))
        .y_axis(Axis::default().bounds(bounds.y).labels(y_labels));

    f.render_widget(chart, area);
}

/// Summary of a series list for legend rendering. Lifts the metric
/// name (and any tag values that are identical across every series)
/// into a single `header` line, then leaves per-series `rows` with
/// only the differentiating bits. Rendered as `<header>\n<rows>` by
/// `draw_legend`; the inline grid-tile legend uses the same shape.
///
/// When the user has picked label-tags (`legend_label_tags` on App),
/// `rows` is the joined values of those tags per series and shared
/// tag values aren't lifted into the header — the user already chose
/// the columns they care about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegendSummary {
    pub header: String,
    pub rows: Vec<String>,
}

/// Build a `LegendSummary` from a series slice. Pure / testable.
pub fn summarize_legend(series: &[Series], picked: &[String]) -> LegendSummary {
    if series.is_empty() {
        return LegendSummary::default();
    }

    // Shared metric across every series, if any (the `format_series_name`
    // convention is `"metric {tags}"` or just `"metric"`, so we split
    // on the first ` {`).
    let first_metric = metric_of(&series[0].name);
    let shared_metric = series
        .iter()
        .all(|s| metric_of(&s.name) == first_metric)
        .then(|| first_metric.to_string());

    // User-driven picked-tags mode: row label is the joined values
    // of the picked keys, falling back to the series name when none
    // of the keys are present on this series.
    if !picked.is_empty() {
        let rows: Vec<String> = series
            .iter()
            .map(|s| {
                let vals: Vec<String> = picked
                    .iter()
                    .filter_map(|k| {
                        s.tags
                            .iter()
                            .find(|(tk, _)| tk == k)
                            .map(|(_, v)| tag_text(v))
                    })
                    .collect();
                if vals.is_empty() {
                    s.name.clone()
                } else {
                    vals.join(", ")
                }
            })
            .collect();
        return LegendSummary {
            header: shared_metric.unwrap_or_default(),
            rows,
        };
    }

    // Auto mode: also lift tag (k, v) pairs that are identical across
    // every series — they're noise on per-row labels.
    let shared_tags: Vec<(String, serde_json::Value)> = series[0]
        .tags
        .iter()
        .filter(|(k, v)| {
            series
                .iter()
                .all(|s| s.tags.iter().any(|(k2, v2)| k2 == k && v2 == v))
        })
        .cloned()
        .collect();

    let mut header_parts: Vec<String> = Vec::new();
    if let Some(m) = &shared_metric {
        header_parts.push(m.clone());
    }
    if !shared_tags.is_empty() {
        let joined = shared_tags
            .iter()
            .map(|(k, v)| format!("{k}={}", tag_text(v)))
            .collect::<Vec<_>>()
            .join(", ");
        header_parts.push(format!("{{{joined}}}"));
    }
    let header = header_parts.join(" ");

    let rows: Vec<String> = series
        .iter()
        .map(|s| {
            let differing: Vec<String> = s
                .tags
                .iter()
                .filter(|(k, v)| !shared_tags.iter().any(|(sk, sv)| sk == k && sv == v))
                .map(|(k, v)| format!("{k}={}", tag_text(v)))
                .collect();
            if differing.is_empty() {
                // No distinguishing tags — the colour bullet alone
                // identifies this series. Leave the row text empty.
                String::new()
            } else {
                differing.join(", ")
            }
        })
        .collect();

    LegendSummary { header, rows }
}

/// Extract the metric prefix from a `format_series_name`-shaped
/// name. Returns the whole input when there's no `" {"` separator
/// (the no-tags case).
fn metric_of(name: &str) -> &str {
    name.split_once(" {").map(|(m, _)| m).unwrap_or(name)
}

#[allow(clippy::too_many_arguments)]
pub fn draw_legend(
    f: &mut Frame,
    series: &[Series],
    picked: &[String],
    hidden: &[bool],
    selected: usize,
    focused: bool,
    block: Block<'_>,
    area: Rect,
) {
    if series.is_empty() {
        let placeholder = Paragraph::new("(no series)").block(block);
        f.render_widget(placeholder, area);
        return;
    }

    let summary = summarize_legend(series, picked);

    // Render the border + title; carve a header row out of the
    // inner area when the summary has something to lift.
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (header_area, list_area) = if !summary.header.is_empty() && inner.height >= 2 {
        (
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
            Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: inner.height - 1,
            },
        )
    } else {
        (
            Rect {
                x: inner.x,
                y: inner.y,
                width: 0,
                height: 0,
            },
            inner,
        )
    };

    if header_area.height > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                summary.header.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))),
            header_area,
        );
    }

    let items: Vec<ListItem<'_>> = series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let is_hidden = hidden.get(i).copied().unwrap_or(false);
            let is_selected = i == selected;
            let bullet = if is_hidden { "○ " } else { "● " };
            let bullet_color = if is_hidden { Color::DarkGray } else { s.color };
            let mut name_style = Style::default();
            if is_hidden {
                name_style = name_style
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::CROSSED_OUT);
            }
            if is_selected {
                name_style = name_style.add_modifier(Modifier::BOLD);
                if focused {
                    name_style = name_style.bg(Color::Rgb(60, 60, 110));
                }
            }
            let gutter = if is_selected && !focused { ">" } else { " " };
            let label = summary
                .rows
                .get(i)
                .cloned()
                .unwrap_or_else(|| s.name.clone());
            ListItem::new(Line::from(vec![
                Span::raw(gutter.to_string()),
                Span::styled(bullet.to_string(), Style::default().fg(bullet_color)),
                Span::styled(label, name_style),
            ]))
        })
        .collect();

    // Render as a stateful list so ratatui auto-adjusts the scroll
    // offset to keep `selected` in view. We rebuild the state every
    // frame from `selected` — the offset is purely a function of
    // the cursor and viewport height, so there's nothing to persist.
    let mut list_state = ListState::default();
    if !series.is_empty() {
        list_state.select(Some(selected.min(series.len() - 1)));
    }
    f.render_stateful_widget(List::new(items), list_area, &mut list_state);
}

#[cfg(test)]
mod tests;
