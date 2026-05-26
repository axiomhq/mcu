//! Multi-tile dashboard grid renderer.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::pane_block;
use crate::app::App;
use crate::axiom::{Chart, ChartKnownExt, DashboardSummaryExt, KnownChart, LayoutItem};
use crate::viz;

/// Minimum number of terminal rows per virtual grid row **for rows
/// containing at least one non-Note tile**. At 4 cells/virt-row a
/// `h=2` tile gets 8 terminal rows - enough for a 1-row title
/// chrome plus a small chart. We let layouts grow past the viewport
/// and rely on scrolling rather than squashing.
pub(super) const MIN_GRID_ROW_HEIGHT: u32 = 4;

/// Per-virt-row height used for rows that **only** contain Note
/// tiles. Notes are typically a heading or short paragraph and
/// don't need a chart-sized minimum; 2 terminal rows per virt-row
/// gives a 2-virt-row Note a 4-row tile (top border + 2 content +
/// bottom border).
pub(super) const NOTE_ROW_HEIGHT: u32 = 2;

/// Multi-tile grid view of the loaded dashboard. Projects each
/// `LayoutItem`'s 12-column coordinates into `Rect`s carved out of the
/// graph pane, draws a bordered chrome block per tile, and highlights
/// the currently-selected one in yellow.
pub(super) fn draw_dashboard_grid(f: &mut Frame, app: &mut App, area: Rect) {
    // Resolve everything we need from `loaded_dashboard` up front so
    // we can drop the borrow before mutating `app.dashboard_scroll`.
    let Some(resource) = app.loaded_dashboard.as_ref() else {
        return;
    };
    let charts = resource.dashboard.charts.clone();
    let layout = resource.dashboard.layout.clone();
    let dash_name = resource.name_or_unnamed().to_string();

    // Outer frame for the whole dashboard pane.
    let focused = app.focus == crate::app::Pane::Dashboard;
    let submode_badge = match &app.tile_submode {
        crate::app::TileSubMode::Idle => "",
        crate::app::TileSubMode::Move { .. } => " MOVE",
        crate::app::TileSubMode::Resize { .. } => " RESIZE",
        crate::app::TileSubMode::ConfirmDelete => " DELETE?",
        crate::app::TileSubMode::PickViz {
            action: crate::app::PickVizAction::Add,
            ..
        } => " ADD",
        crate::app::TileSubMode::PickViz {
            action: crate::app::PickVizAction::Open { above: false, .. },
            ..
        } => " OPEN↓",
        crate::app::TileSubMode::PickViz {
            action: crate::app::PickVizAction::Open { above: true, .. },
            ..
        } => " OPEN↑",
    };
    let dirty_pip = if app.dashboard_dirty { " [+]" } else { "" };
    let title = format!(
        " dashboard · {}{}{} · {} tile(s) · [{}/{}] ",
        dash_name,
        dirty_pip,
        submode_badge,
        charts.len(),
        if charts.is_empty() {
            0
        } else {
            app.selected_chart_idx + 1
        },
        charts.len()
    );
    let outer = pane_block(&title, focused);
    let pane_inner = outer.inner(area);
    f.render_widget(outer, area);
    if pane_inner.width < 4 || pane_inner.height < 3 || charts.is_empty() {
        f.render_widget(
            Paragraph::new("(no tiles to render)")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center),
            pane_inner,
        );
        return;
    }

    // Reserve a 1-cell scrollbar gutter on the right if needed.
    // We'll decide based on overflow below; for now compute the
    // viewport assuming the gutter exists, then reclaim it if not.
    let viewport = Rect {
        x: pane_inner.x,
        y: pane_inner.y,
        width: pane_inner.width.saturating_sub(1),
        height: pane_inner.height,
    };

    // The wire layout uses a 12-column virtual grid (server spec).
    let col_w_f = viewport.width as f32 / 12.0;

    // Total virtual-grid rows = max(y + h) across layout, or fall
    // back to ceil(charts.len() / 2) when entries are missing.
    let virt_rows = layout
        .iter()
        .map(|l| l.y.unwrap_or(0) + l.h)
        .max()
        .unwrap_or_else(|| ((charts.len() as u32).div_ceil(2)) * 6)
        .max(1);

    // Per-virt-row variable heights: a row that hosts only Note
    // tiles shrinks to `NOTE_ROW_HEIGHT`, everything else gets at
    // least `MIN_GRID_ROW_HEIGHT`. Surplus space (when the layout
    // would otherwise leave the viewport bottom empty) is given to
    // non-Note rows so Notes stay compact.
    let row_heights =
        compute_row_heights(&charts, &layout, virt_rows as usize, viewport.height as u32);
    let mut row_tops: Vec<u32> = Vec::with_capacity(row_heights.len() + 1);
    let mut acc = 0u32;
    row_tops.push(0);
    for h in &row_heights {
        acc = acc.saturating_add(*h);
        row_tops.push(acc);
    }
    let content_h = acc;

    // Scrolling math. If everything fits, no scrollbar; otherwise
    // reclaim the gutter and clamp `dashboard_scroll`.
    let max_scroll = content_h.saturating_sub(viewport.height as u32) as u16;
    let needs_scroll = max_scroll > 0;
    let viewport = if needs_scroll {
        viewport
    } else {
        // Reclaim the gutter cell since we don't need it.
        Rect {
            width: pane_inner.width,
            ..viewport
        }
    };

    // Auto-scroll: bring the selected tile fully into view. Uses
    // the variable row-heights so a tile under a shrunken Note row
    // still maps to the right terminal coordinates.
    if let Some(chart) = charts.get(app.selected_chart_idx) {
        let (_, gy, _, gh) = resolve_slot(&layout, chart, app.selected_chart_idx);
        let top = row_tops[gy as usize] as u16;
        let bot = row_tops[(gy + gh) as usize] as u16;
        if top < app.dashboard_scroll {
            app.dashboard_scroll = top;
        } else if bot > app.dashboard_scroll.saturating_add(viewport.height) {
            app.dashboard_scroll = bot.saturating_sub(viewport.height);
        }
    }
    app.dashboard_scroll = app.dashboard_scroll.min(max_scroll);
    let scroll = app.dashboard_scroll as u32;

    // Render each tile, clipping to the viewport. Tiles entirely
    // outside are skipped; tiles partially outside get their
    // remaining visible band drawn (chrome at the clipped edge is
    // truncated, which is the conventional scroll behaviour).
    for (i, chart) in charts.iter().enumerate() {
        let (gx, gy, gw, gh) = resolve_slot(&layout, chart, i);
        let content_top = row_tops[gy as usize];
        let content_bot = row_tops[(gy + gh) as usize];
        let viewport_top = scroll;
        let viewport_bot = scroll.saturating_add(viewport.height as u32);
        if content_bot <= viewport_top || content_top >= viewport_bot {
            continue;
        }
        let vis_top = content_top.max(viewport_top);
        let vis_bot = content_bot.min(viewport_bot);
        let y = viewport.y + (vis_top - viewport_top) as u16;
        let h = (vis_bot - vis_top) as u16;

        let x = viewport.x + (gx as f32 * col_w_f) as u16;
        let w = ((gw as f32 * col_w_f) as u16).max(3);
        let w = w.min(viewport.width.saturating_sub(x - viewport.x));
        if w < 3 || h < 3 {
            continue;
        }
        let rect = Rect {
            x,
            y,
            width: w,
            height: h,
        };
        let selected = i == app.selected_chart_idx;
        draw_grid_tile(f, app, chart, rect, selected && focused);
    }

    // Scrollbar in the reserved gutter column.
    if needs_scroll {
        let bar_x = pane_inner.x + pane_inner.width - 1;
        let bar_h = pane_inner.height;
        let thumb_h =
            ((viewport.height as u32 * bar_h as u32 / content_h).max(1) as u16).min(bar_h);
        let track_room = bar_h.saturating_sub(thumb_h);
        let thumb_y = if max_scroll == 0 {
            0
        } else {
            (track_room as u32 * scroll / max_scroll as u32) as u16
        };
        for dy in 0..bar_h {
            let glyph = if dy >= thumb_y && dy < thumb_y + thumb_h {
                "█"
            } else {
                "│"
            };
            let style = if dy >= thumb_y && dy < thumb_y + thumb_h {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            f.render_widget(
                Paragraph::new(glyph).style(style),
                Rect {
                    x: bar_x,
                    y: pane_inner.y + dy,
                    width: 1,
                    height: 1,
                },
            );
        }
    }
}

/// Build the per-virt-row height vector. Rows whose only inhabitants
/// are `Chart::Note` tiles use `NOTE_ROW_HEIGHT`; all other rows use
/// `MIN_GRID_ROW_HEIGHT`. If the resulting content height falls short
/// of `viewport_h`, the surplus is distributed across non-Note rows
/// (Note rows stay at their minimum so headers don't grow into the
/// space below them).
pub(super) fn compute_row_heights(
    charts: &[Chart],
    layout: &[LayoutItem],
    virt_rows: usize,
    viewport_h: u32,
) -> Vec<u32> {
    let mut has_non_note = vec![false; virt_rows];
    for (i, chart) in charts.iter().enumerate() {
        if matches!(chart, Chart::Known(KnownChart::Note(_))) {
            continue;
        }
        let (_gx, gy, _gw, gh) = resolve_slot(layout, chart, i);
        let lo = gy as usize;
        let hi = (gy + gh).min(virt_rows as u32) as usize;
        for slot in has_non_note.iter_mut().take(hi).skip(lo) {
            *slot = true;
        }
    }
    let mut heights: Vec<u32> = has_non_note
        .iter()
        .map(|&n| {
            if n {
                MIN_GRID_ROW_HEIGHT
            } else {
                NOTE_ROW_HEIGHT
            }
        })
        .collect();
    let total: u32 = heights.iter().sum();
    if viewport_h > total {
        let non_note_rows: Vec<usize> = has_non_note
            .iter()
            .enumerate()
            .filter_map(|(r, &n)| if n { Some(r) } else { None })
            .collect();
        if !non_note_rows.is_empty() {
            let extra = viewport_h - total;
            let per = extra / non_note_rows.len() as u32;
            let rem = (extra % non_note_rows.len() as u32) as usize;
            for (k, &r) in non_note_rows.iter().enumerate() {
                heights[r] += per + if k < rem { 1 } else { 0 };
            }
        }
    }
    heights
}

/// Resolve the virtual-grid slot for a chart, falling back to a
/// 2-per-row auto-stack at default 6×6 dimensions when no
/// `LayoutItem` exists. Returns `(x, y, w, h)`.
fn resolve_slot(layout: &[LayoutItem], chart: &Chart, idx: usize) -> (u32, u32, u32, u32) {
    if let Some(l) = layout.iter().find(|l| l.i == chart.known_base().id) {
        (l.x, l.y.unwrap_or(0), l.w, l.h)
    } else {
        let row = (idx / 2) as u32;
        let col = (idx % 2) as u32;
        (col * 6, row * 6, 6, 6)
    }
}

/// Render one tile of the grid. The bordered chrome block stays the
/// same as in 18a; what's new in this turn is that when a per-tile
/// query result is cached on `App.tile_results`, we dispatch through
/// `viz::draw` to render the actual visualisation (line/bar/pie/...)
/// inside the tile's inner Rect. Falls back to a one-line preview of
/// the query text when:
///
///   * the query is APL (not yet executable - wait for step 14b/15b),
///   * the query is missing or empty,
///   * the per-tile fetch is still in flight (shows "loading..."),
///   * the per-tile fetch errored (shows the error message).
fn draw_grid_tile(f: &mut Frame, app: &App, chart: &Chart, area: Rect, highlighted: bool) {
    let base = chart.known_base();
    let kind_glyph = match chart {
        Chart::Known(KnownChart::TimeSeries(_)) => "⌈⌉",
        Chart::Known(KnownChart::Heatmap(_)) => "▦",
        Chart::Known(KnownChart::LogStream(_)) => "≡",
        Chart::Known(KnownChart::Pie(_)) => "●",
        Chart::Known(KnownChart::Scatter(_)) => "⋮",
        Chart::Known(KnownChart::Table(_)) => "⊞",
        Chart::Known(KnownChart::TopK(_)) => "≡",
        Chart::Known(KnownChart::Statistic(_)) => "No",
        Chart::Known(KnownChart::Note(_)) => "✎",
        Chart::Unknown(_) => "?",
    };
    // Per-tile status pip in the title bar.
    let pip = app
        .tile_results
        .get(&base.id)
        .map(
            |t| match (t.busy, t.error.is_some(), !t.series.is_empty()) {
                (true, _, _) => "· ⚫",
                (false, true, _) => "· ⛔",
                (false, false, true) => "· ✔",
                _ => "",
            },
        )
        .unwrap_or("");
    let title = format!(
        " {} {} · {} {} ",
        kind_glyph,
        chart
            .type_str()
            .expect("mcu expects Chart::Known; got Chart::Unknown"),
        base.name.as_deref().unwrap_or("(unnamed)"),
        pip,
    );
    let border_style = if highlighted {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    // Map the wire chart to an internal VizKind so we can hand off to
    // viz::draw.
    let viz_kind = crate::dashboard::VizKind::from_chart(chart);

    // Decide what to render in the tile body.
    enum Body<'a> {
        Viz { series: &'a [crate::chart::Series] },
        Loading,
        Apl(String),
        Note(String),
        Empty,
        Error(String),
    }
    // Note tiles never trigger a metrics fetch - their "body" is the
    // tile's markdown text, which the wire stashes in `ChartBase.extras`.
    // The exact key isn't modelled in this codebase, so probe the
    // conventional ones and fall back to an empty body (which the
    // renderer collapses to a single divider line).
    let body = if matches!(chart, Chart::Known(KnownChart::Note(_))) {
        let extras = &chart.known_base().extras;
        let text = ["markdown", "text", "body", "content"]
            .into_iter()
            .find_map(|k| extras.get(k).and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        Body::Note(text)
    } else if let Some(t) = app.tile_results.get(&base.id) {
        if let Some(err) = t.error.as_deref() {
            Body::Error(err.to_string())
        } else if !t.series.is_empty() {
            Body::Viz { series: &t.series }
        } else if t.busy {
            Body::Loading
        } else {
            // Finished, no error, no series - "no data" outcome.
            Body::Empty
        }
    } else {
        // No entry means we never kicked off a fetch. The shared
        // classifier decides whether the chart is truly APL (only
        // those should render the APL placeholder) versus "no query
        // at all".
        match crate::dashboard::extract_query(chart) {
            crate::dashboard::Query::Apl(text) => Body::Apl(text),
            _ => Body::Empty,
        }
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(title, border_style));

    match body {
        Body::Viz { series } => {
            // Hand off to the real renderer. Empty hidden mask + no
            // selection highlight — those are solo-mode UI concerns.
            //
            // Grid view also gets a 1-row inline legend strip
            // carved out of the tile's inner bottom row, so each
            // tile self-documents — the right-hand side legend
            // only ever shows the focused tile's series, so the
            // others need their own label strip. Solo view skips
            // this (the chart owns the whole pane; the side
            // legend handles labelling).
            let hidden = vec![false; series.len()];
            let body_text = String::new();
            let wants_inline_legend = matches!(
                viz_kind,
                crate::dashboard::VizKind::Line
                    | crate::dashboard::VizKind::Bar
                    | crate::dashboard::VizKind::Area
                    | crate::dashboard::VizKind::Scatter
            ) && !series.is_empty();
            let inner = block.inner(area);
            if wants_inline_legend && inner.height >= 4 {
                f.render_widget(block, area);
                let chart_area = Rect {
                    x: inner.x,
                    y: inner.y,
                    width: inner.width,
                    height: inner.height - 1,
                };
                let strip = Rect {
                    x: inner.x,
                    y: inner.y + inner.height - 1,
                    width: inner.width,
                    height: 1,
                };
                viz::draw(
                    f,
                    viz_kind,
                    series,
                    &hidden,
                    None,
                    &std::collections::BTreeMap::new(),
                    &body_text,
                    Block::default(),
                    chart_area,
                );
                draw_inline_legend(f, series, &app.legend.label_tags, strip);
            } else {
                viz::draw(
                    f,
                    viz_kind,
                    series,
                    &hidden,
                    None,
                    &std::collections::BTreeMap::new(),
                    &body_text,
                    block,
                    area,
                );
            }
        }
        Body::Loading => {
            let inner = block.inner(area);
            f.render_widget(block, area);
            f.render_widget(
                Paragraph::new("loading...")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(ratatui::layout::Alignment::Center),
                inner,
            );
        }
        Body::Apl(text) => {
            let inner = block.inner(area);
            f.render_widget(block, area);
            let lines = vec![
                Line::from(Span::styled(
                    "APL (not yet executable)",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
                Line::from(Span::styled(text, Style::default().fg(Color::Gray))),
            ];
            f.render_widget(
                Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
                inner,
            );
        }
        Body::Empty => {
            let inner = block.inner(area);
            f.render_widget(block, area);
            f.render_widget(
                Paragraph::new("(no data)")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(ratatui::layout::Alignment::Center),
                inner,
            );
        }
        Body::Error(msg) => {
            let inner = block.inner(area);
            f.render_widget(block, area);
            f.render_widget(
                Paragraph::new(msg)
                    .style(Style::default().fg(Color::Red))
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                inner,
            );
        }
        Body::Note(text) => {
            // Hand off to the note renderer; empty bodies are
            // collapsed to a single thicker horizontal divider line
            // (no border block) by `draw_note` itself.
            viz::draw(
                f,
                viz_kind,
                &[],
                &[],
                None,
                &std::collections::BTreeMap::new(),
                &text,
                block,
                area,
            );
        }
    }
}

/// One-row inline legend strip rendered under time-series-family
/// tiles in the dashboard grid. Layout:
///
///   <metric-header>:  ● label  ● label  ...
///
/// The shared metric (plus any tag values identical across every
/// series) is lifted into the leading header; per-bullet labels
/// carry only the differentiating bits. When there isn't room for
/// every entry, the renderer truncates and appends ` ...`. When
/// there isn't even room for the header + one entry, the header is
/// dropped so a few bullets can still fit.
fn draw_inline_legend(
    f: &mut Frame,
    series: &[crate::chart::Series],
    picked: &[String],
    area: Rect,
) {
    if area.width == 0 || area.height == 0 || series.is_empty() {
        return;
    }
    let summary = crate::chart::summarize_legend(series, picked);
    let labels: Vec<&str> = summary.rows.iter().map(|s| s.as_str()).collect();

    // Try with the header prefix first; fall back to no-prefix if
    // even one entry can't fit alongside it.
    let header_prefix_w = if summary.header.is_empty() {
        0
    } else {
        // "<header>: " — trailing 2 spaces matches the inter-entry
        // separator so the eye reads the prefix as part of the row.
        summary.header.chars().count() + 2
    };
    let mut use_header = header_prefix_w > 0;
    let mut plan = fit_inline_legend(
        &labels,
        (area.width as usize).saturating_sub(header_prefix_w),
    );
    if use_header && plan.shown.is_empty() {
        // No room for any bullet alongside the header — drop it.
        use_header = false;
        plan = fit_inline_legend(&labels, area.width as usize);
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    if use_header {
        spans.push(Span::styled(
            summary.header.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(": ".to_string()));
    }
    for (i, &shown_idx) in plan.shown.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            "● ".to_string(),
            Style::default().fg(series[shown_idx].color),
        ));
        spans.push(Span::styled(
            summary.rows[shown_idx].clone(),
            Style::default().fg(Color::Gray),
        ));
    }
    if plan.ellipsis {
        spans.push(Span::styled(
            " ...".to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Plan for fitting an inline legend onto a single row of given
/// `width`. Pure / testable: returns the indices of the entries to
/// show in order and whether to append `...` to signal more. Width
/// accounting uses `chars().count()` to match the rest of the
/// codebase (labels are short, mostly-ASCII tag values).
pub(super) fn fit_inline_legend(labels: &[&str], width: usize) -> InlineLegendPlan {
    const BULLET_W: usize = 2; // "● "
    const SEP_W: usize = 2; // "  " between entries
    const ELLIPSIS_W: usize = 4; // " ..."
    let mut shown: Vec<usize> = Vec::new();
    let mut used = 0usize;
    for (i, label) in labels.iter().enumerate() {
        let label_w = label.chars().count();
        let sep_w = if shown.is_empty() { 0 } else { SEP_W };
        let entry_w = sep_w + BULLET_W + label_w;
        // Reserve room for the ellipsis if at least one more entry
        // would follow. If this is the last entry, no reservation is
        // needed — we'd rather show it than ellipsise it away.
        let more_follow = i + 1 < labels.len();
        let reserved = if more_follow { ELLIPSIS_W } else { 0 };
        if used + entry_w + reserved > width {
            return InlineLegendPlan {
                shown,
                ellipsis: true,
            };
        }
        shown.push(i);
        used += entry_w;
    }
    InlineLegendPlan {
        shown,
        ellipsis: false,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct InlineLegendPlan {
    pub(super) shown: Vec<usize>,
    pub(super) ellipsis: bool,
}
