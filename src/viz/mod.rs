//! Viz-kind dispatch.
//!
//! Each kind ships in its own sub-module; this module wires them
//! together via the [`draw`] function and exposes the shared
//! pragma / aggregation / table types every caller (and every kind)
//! consumes.
//!
//! The pragma is *sugar*: the canonical source of the active kind is
//! `App.viz_kind`. When the buffer changes we re-parse the pragma into
//! a [`VizSpec`] and write the kind/opts onto `App`; when the kind
//! changes via `:viz` we rewrite (or insert) the pragma on the buffer.

use std::collections::BTreeMap;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::chart::{self, Series};
use crate::dashboard::VizKind;

mod agg;
pub mod apl_decode;
mod heatmap;
mod note;
mod pie;
mod pragma;
mod statistic;
// `pub` so tests can construct `TableCell` variants directly. The
// production API surface is the re-exports below.
pub mod table;
mod top_list;

pub use pragma::{PragmaError, VizSpec, parse_pragma, upsert_pragma};
pub use table::{TableResult, draw_table_result};

/// Render the focused tile into `area`.
///
/// `body` is the tile's underlying text. For metrics tiles this is the
/// MPL query (rendered indirectly through `series`); for `note` it is
/// the markdown body. Most kinds ignore it.
///
/// `unit` is the resolved OTEL/UCUM unit for the series (via
/// [`crate::app::helpers::resolve_unit`]). Time-series renderers use
/// it to scale y-axis labels (e.g. bytes → MiB); the statistic
/// renderer uses it to suffix its single big number. Kinds that
/// can't sensibly carry a unit (Note, Spacer) ignore it.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    f: &mut Frame,
    kind: VizKind,
    series: &[Series],
    hidden: &[bool],
    selected: Option<usize>,
    opts: &BTreeMap<String, String>,
    body: &str,
    unit: Option<&crate::unit::Unit>,
    block: Block<'_>,
    area: Rect,
) {
    match kind {
        VizKind::Line | VizKind::Bar | VizKind::Area | VizKind::Scatter => {
            chart::draw_graph(f, series, hidden, selected, kind, unit, block, area);
        }
        VizKind::Statistic => statistic::draw_statistic(f, series, hidden, opts, unit, block, area),
        VizKind::TopList => top_list::draw_top_list(f, series, hidden, opts, block, area),
        VizKind::Pie => pie::draw_pie(f, series, hidden, opts, block, area),
        VizKind::Heatmap => heatmap::draw_heatmap(f, series, hidden, opts, block, area),
        VizKind::Table => table::draw_table(f, series, hidden, opts, block, area),
        VizKind::LogStream => draw_log_stream(f, opts, block, area),
        VizKind::Note => note::draw_note(f, body, block, area),
        VizKind::Spacer => draw_spacer(f, block, area),
        VizKind::MonitorList => draw_monitor_list(f, opts, block, area),
    }
}

// ── shared helpers ───────────────────────────────────────────────────

/// Truncate `s` to at most `w` characters, replacing the tail with `…`
/// when it would have been clipped. Used by pie + heatmap labels.
pub(crate) fn truncate_to_width(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ── placeholder kinds ────────────────────────────────────────────────

/// The spacer renders nothing inside its pane. Useful for grid layouts
/// where the user wants visual breathing room between tiles.
fn draw_spacer(f: &mut Frame, block: Block<'_>, area: Rect) {
    f.render_widget(block, area);
}

/// Placeholder renderer for the `log_stream` viz kind. The events
/// fetcher never landed; until it does, a tile of this kind shows a
/// static "not implemented" notice. The `VizKind::LogStream` variant
/// stays so existing dashboards containing log-stream charts still
/// load — they just render this placeholder instead of crashing.
fn draw_log_stream(f: &mut Frame, _opts: &BTreeMap<String, String>, block: Block<'_>, area: Rect) {
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = vec![
        Line::from(Span::styled(
            "log stream",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "log stream rendering not implemented in ax.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

/// Placeholder renderer for `monitor_list`. Same story as
/// `draw_log_stream`: the monitors fetcher never landed, so the
/// variant stays wired to a not-implemented notice rather than
/// breaking dashboards that contain it.
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
            "monitor list rendering not implemented in ax.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

#[cfg(test)]
mod tests;
