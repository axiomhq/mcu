//! Statistic tile: centred big-number readout plus a braille sparkline.

use std::collections::BTreeMap;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Dataset, Paragraph},
};

use super::agg::{Agg, agg_label, format_value};
use crate::chart::Series;

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
pub(super) fn draw_statistic(
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
    let pad_top: u16 = if inner.height >= number_rows + 3 {
        1
    } else {
        0
    };
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
