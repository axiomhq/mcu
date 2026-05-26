//! Pie tile: percentage bars over normalised aggregates.

use std::collections::BTreeMap;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use super::agg::{Agg, format_value};
use super::truncate_to_width;
use crate::chart::Series;

/// Compute the percentage rows the pie renders: `(series_idx, value,
/// share_0_to_1)`, sorted descending by value, with all-negative or
/// zero-total inputs returning an empty vec. Extracted so it's directly
/// unit-testable.
pub(super) fn pie_rows(series: &[Series], hidden: &[bool], agg: Agg) -> Vec<(usize, f64, f64)> {
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
pub(super) fn draw_pie(
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
