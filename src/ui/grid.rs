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
use crate::axiom::{Chart, DashboardSummaryExt, KnownChart, LayoutItem};
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
    // `loaded_dashboard` is read through several short-lived shared
    // borrows below rather than cloned up front: the only `&mut` we
    // need is the `dashboard_scroll` write, which is sequenced between
    // the layout-math borrow and the tile-render borrow. Cloning
    // `charts` + `layout` every frame (the previous approach) was pure
    // borrow-checker appeasement and deep-cloned every tile per redraw.
    if app.loaded_dashboard.is_none() {
        return;
    }

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
    let dash_name = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .name_or_unnamed()
        .to_string();
    let charts_len = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .len();
    let title = format!(
        " dashboard · {}{}{} · {} tile(s) · [{}/{}] ",
        dash_name,
        dirty_pip,
        submode_badge,
        charts_len,
        if charts_len == 0 {
            0
        } else {
            app.selected_chart_idx + 1
        },
        charts_len
    );
    let outer = pane_block(&title, focused);
    let pane_inner = outer.inner(area);
    f.render_widget(outer, area);
    // Reset the per-frame tile hit-map (step 27). Repopulated in the
    // render loop below; left empty on the early-return paths so a
    // click can't match a tile that wasn't drawn.
    app.mouse_geom.grid_tiles.clear();
    if pane_inner.width < 4 || pane_inner.height < 3 || charts_len == 0 {
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

    // Phase 1 — layout math under one shared borrow. Produces only
    // small derived vectors/scalars, never a `Chart` clone.
    //
    // `virt_rows` resolves through `resolve_slot` (rather than just the
    // `layout` entries) so `row_tops` stays big enough for auto-stacked
    // charts: a chart with no matching `LayoutItem` gets an auto-stack
    // slot at `gy = (idx/2)*6`, which can sit below the last `layout`
    // row. Indexing `row_tops` with that slot used to panic when
    // `layout` was non-empty but didn't cover every chart.
    let (row_tops, content_h, max_scroll, needs_scroll) = {
        let resource = app.loaded_dashboard.as_ref().unwrap();
        let charts = &resource.dashboard.charts;
        let layout = &resource.dashboard.layout;
        let virt_rows = charts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let (_, gy, _, gh) = resolve_slot(layout, c, i);
                gy + gh
            })
            .max()
            .unwrap_or(6)
            .max(1);
        // Per-virt-row variable heights: a row that hosts only Note
        // tiles shrinks to `NOTE_ROW_HEIGHT`, everything else gets at
        // least `MIN_GRID_ROW_HEIGHT`. Surplus space is given to
        // non-Note rows so Notes stay compact.
        let row_heights =
            compute_row_heights(charts, layout, virt_rows as usize, viewport.height as u32);
        let mut row_tops: Vec<u32> = Vec::with_capacity(row_heights.len() + 1);
        let mut acc = 0u32;
        row_tops.push(0);
        for h in &row_heights {
            acc = acc.saturating_add(*h);
            row_tops.push(acc);
        }
        let content_h = acc;
        let max_scroll = content_h.saturating_sub(viewport.height as u32) as u16;
        (row_tops, content_h, max_scroll, max_scroll > 0)
    };

    // Scrolling math. If everything fits, no scrollbar; otherwise
    // reclaim the gutter and clamp `dashboard_scroll`.
    let viewport = if needs_scroll {
        viewport
    } else {
        // Reclaim the gutter cell since we don't need it.
        Rect {
            width: pane_inner.width,
            ..viewport
        }
    };

    // Phase 2 — auto-scroll target, then the single `&mut` write.
    // Bring the selected tile fully into view, using the variable
    // row-heights so a tile under a shrunken Note row still maps to
    // the right terminal coordinates.
    let last_row_top = row_tops.len() - 1;
    let mut new_scroll = app.dashboard_scroll;
    {
        let resource = app.loaded_dashboard.as_ref().unwrap();
        let layout = &resource.dashboard.layout;
        if let Some(chart) = resource.dashboard.charts.get(app.selected_chart_idx) {
            let (_, gy, _, gh) = resolve_slot(layout, chart, app.selected_chart_idx);
            // Defensive clamp: `virt_rows` already covers every chart's
            // resolved slot, so these never saturate in practice.
            let top = row_tops[(gy as usize).min(last_row_top)] as u16;
            let bot = row_tops[((gy + gh) as usize).min(last_row_top)] as u16;
            if top < new_scroll {
                new_scroll = top;
            } else if bot > new_scroll.saturating_add(viewport.height) {
                new_scroll = bot.saturating_sub(viewport.height);
            }
        }
    }
    new_scroll = new_scroll.min(max_scroll);
    app.dashboard_scroll = new_scroll;
    let scroll = new_scroll as u32;

    // Phase 3 — render each tile, clipping to the viewport. Tiles
    // entirely outside are skipped; tiles partially outside get their
    // remaining visible band drawn (chrome at the clipped edge is
    // truncated, the conventional scroll behaviour). The
    // `&app.loaded_dashboard` borrow and the `&App` handed to
    // `draw_grid_tile` are both shared, so they coexist without
    // cloning.
    // Collect tile hit-rects in a local while the shared borrow of
    // `loaded_dashboard` is live, then write them into `mouse_geom`
    // after the borrow ends — `draw_grid_tile` takes `&App`, so a
    // `&mut self.mouse_geom` push inside this block would conflict
    // with the `resource` borrow.
    let mut tile_hits: Vec<(usize, Rect)> = Vec::new();
    {
        let resource = app.loaded_dashboard.as_ref().unwrap();
        let charts = &resource.dashboard.charts;
        let layout = &resource.dashboard.layout;
        for (i, chart) in charts.iter().enumerate() {
            let (gx, gy, gw, gh) = resolve_slot(layout, chart, i);
            let content_top = row_tops[(gy as usize).min(last_row_top)];
            let content_bot = row_tops[((gy + gh) as usize).min(last_row_top)];
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
            tile_hits.push((i, rect));
            draw_grid_tile(f, app, chart, rect, selected && focused);
        }
    }
    app.mouse_geom.grid_tiles = tile_hits;

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
    // `Chart::Unknown` has no `ChartBase.id`, so it can never match a
    // layout entry by id — fall through to the auto-stack slot, which
    // gives the tile a visible footprint even when the SDK doesn't
    // model the variant. `:w` still round-trips the raw JSON; we
    // just can't honour any custom layout linkage for it.
    let id = chart.base().map(|b| b.id.as_str()).unwrap_or("");
    if !id.is_empty()
        && let Some(l) = layout.iter().find(|l| l.i == id)
    {
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
    // `Chart::Unknown` has no `ChartBase`: synthesise a placeholder
    // `base` view (empty id/name, no extras) so the rest of this
    // function can render a stub tile instead of panicking. The tile
    // is still visible in the grid (so the user knows it exists and
    // `:w` will round-trip it), it just can't be filled with live
    // data or correlated with `tile_results`.
    let placeholder_extras = serde_json::Map::new();
    let (base_id, base_name, base_extras): (&str, Option<&str>, &serde_json::Map<_, _>) =
        match chart.base() {
            Some(b) => (b.id.as_str(), b.name.as_deref(), &b.extras),
            None => ("", None, &placeholder_extras),
        };
    // Per-tile status pip in the title bar. Unknown tiles have an
    // empty `base_id` and so never match `tile_results`; that's the
    // intended outcome — we never spawned a fetch for them.
    let pip = app
        .tile_results
        .get(base_id)
        .map(
            |t| match (t.busy, t.error.is_some(), !t.series.is_empty()) {
                (true, _, _) => "· ⚫",
                (false, true, _) => "· ⛔",
                (false, false, true) => "· ✔",
                _ => "",
            },
        )
        .unwrap_or("");
    // For Unknown we fall back to the literal string "unknown" so the
    // title bar isn't blank.
    let type_label = chart.type_str().unwrap_or("unknown");
    let title = format!(
        " {} {} · {} {} ",
        kind_glyph,
        type_label,
        base_name.unwrap_or("(unnamed)"),
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
        Table(&'a crate::viz::TableResult),
        Loading,
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
        // `Note` is a Known variant, so `base_extras` is the real map.
        let extras = base_extras;
        let text = ["markdown", "text", "body", "content"]
            .into_iter()
            .find_map(|k| extras.get(k).and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        Body::Note(text)
    } else if let Some(t) = app.tile_results.get(base_id) {
        if let Some(err) = t.error.as_deref() {
            Body::Error(err.to_string())
        } else if let Some(table) = t.table.as_ref()
            && !table.rows.is_empty()
        {
            // Tabular APL response (Table / LogStream kinds, or any
            // APL response the series decoder couldn't reshape).
            Body::Table(table)
        } else if !t.series.is_empty() {
            Body::Viz { series: &t.series }
        } else if t.busy {
            Body::Loading
        } else {
            // Finished, no error, no series, no table — "no data" outcome.
            Body::Empty
        }
    } else {
        // No entry means we never kicked off a fetch. Render as empty;
        // pre-execution APL placeholders no longer apply now that the
        // APL fetcher is wired up.
        Body::Empty
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(title, border_style));
    // Bottom-right border annotation showing the most recent fetch's
    // wall-clock duration (e.g. `[3.5s]`). The pip in the top title
    // tells you the *state* (loading / ok / errored); this tells you
    // *how long it took* — useful for spotting slow tiles at a glance.
    // Hidden while the fetch is in flight so the spinner isn't paired
    // with a stale time.
    if let Some(t) = app.tile_results.get(base_id)
        && !t.busy
        && let Some(elapsed) = t.elapsed
    {
        let dim = Style::default().fg(Color::DarkGray);
        block = block.title_bottom(
            ratatui::text::Line::from(vec![
                Span::styled("[", dim),
                Span::styled(format_elapsed(elapsed), dim),
                Span::styled("]", dim),
            ])
            .right_aligned(),
        );
    }

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
                    app.tile_results.get(base_id).and_then(|t| t.unit.as_ref()),
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
                    app.tile_results.get(base_id).and_then(|t| t.unit.as_ref()),
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
        Body::Table(t) => {
            // APL-table response (or any TableResult-shaped tile).
            // Routed through the dedicated table renderer so column
            // types survive (the series-adapter path would force every
            // cell through `Agg::Last`).
            // Grid tiles are non-selectable; no selection state.
            viz::draw_table_result(f, t, None, false, block, area);
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
                None,
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

/// Format a fetch duration for the tile's bottom-right border
/// annotation. Picks units so the rendered string is always ≤5 chars:
///
/// - sub-second  →  `850ms`
/// - <10s        →  `3.5s` (one decimal)
/// - <60s        →  `12s`  (whole seconds)
/// - <1h         →  `1m02s` (minutes + zero-padded seconds)
/// - ≥1h         →  `1h05m` (hours + zero-padded minutes)
///
/// The width budget matters: the tile border is narrow and we render
/// this on the inner edge of a `Block`, so an overflowing string
/// would shove the right corner off-screen.
pub(super) fn format_elapsed(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let total_secs = d.as_secs();
    if total_secs < 10 {
        // One-decimal seconds. Round half-to-even via `as f64` is fine
        // for display.
        let s = d.as_secs_f64();
        return format!("{s:.1}s");
    }
    if total_secs < 60 {
        return format!("{total_secs}s");
    }
    if total_secs < 3_600 {
        let m = total_secs / 60;
        let s = total_secs % 60;
        return format!("{m}m{s:02}s");
    }
    let h = total_secs / 3_600;
    let m = (total_secs % 3_600) / 60;
    format!("{h}h{m:02}m")
}
