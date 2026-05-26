//! Table tile: tabular layout over series + tag columns, with the
//! `TableResult` / `TableCell` model that the future APL decoder will
//! populate directly.

use std::collections::BTreeMap;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Cell as TCell, Paragraph, Row, Table},
};

use super::agg::{Agg, format_value};
use crate::chart::Series;

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

    pub(super) fn render(&self) -> String {
        match self {
            TableCell::Null => "—".to_string(),
            TableCell::Int(n) => n.to_string(),
            TableCell::Float(v) => format_value(*v, 2, None),
            TableCell::Str(s) => s.clone(),
            TableCell::Bool(b) => b.to_string(),
            // Time variant lives for the future APL decoder; until
            // then it renders as a plain numeric unix-seconds value.
            TableCell::Time(t) => format_value(*t as f64, 0, None),
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
                Some((_, v)) => row.push(TableCell::Str(crate::chart::tag_text(v))),
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
pub(super) fn draw_table(
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

    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(1);
    f.render_widget(table, inner);
}
