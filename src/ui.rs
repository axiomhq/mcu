use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::app::{App, Mode};
use crate::chart;
use crate::viz;

const POPUP_MAX_ITEMS: usize = 10;
const POPUP_MIN_WIDTH: u16 = 12;
const POPUP_MAX_WIDTH: u16 = 40;

// Soft caps for the secondary panes so they don't eat huge chunks of
// big terminals just to display a handful of lines/items. The
// percentage acts as a target on small screens; the absolute cap
// kicks in once the terminal is large enough.
const BOTTOM_ROW_PCT: u16 = 25;
const BOTTOM_ROW_MIN: u16 = 5;
const BOTTOM_ROW_MAX: u16 = 12;
const RIGHT_COL_PCT: u16 = 20;
const RIGHT_COL_MIN: u16 = 16;
const RIGHT_COL_MAX: u16 = 40;

fn capped(total: u16, pct: u16, min: u16, max: u16) -> u16 {
    let target = (total as u32 * pct as u32 / 100) as u16;
    target.clamp(min.min(total), max).min(total)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let bottom_h = capped(root[0].height, BOTTOM_ROW_PCT, BOTTOM_ROW_MIN, BOTTOM_ROW_MAX);
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(bottom_h)])
        .split(root[0]);

    let right_w = capped(body[0].width, RIGHT_COL_PCT, RIGHT_COL_MIN, RIGHT_COL_MAX);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(right_w)])
        .split(body[0]);

    let legend_focused = app.focus == crate::app::Pane::Legend;
    let selected_for_graph = if legend_focused {
        Some(app.legend_selected)
    } else {
        None
    };

    if app.view_mode == crate::app::ViewMode::Grid && app.loaded_dashboard.is_some() {
        draw_dashboard_grid(f, app, top[0]);
    } else {
        let viz_kind = app.dashboard.focused_tile().kind;
        let viz_opts = app.dashboard.focused_tile().opts.clone();
        // `body` is the tile's underlying text. Note tiles read this
        // as markdown; metrics tiles ignore it (they consume `series`
        // instead).
        let viz_body = app.query_text();
        viz::draw(
            f,
            viz_kind,
            &app.series,
            &app.legend_hidden,
            selected_for_graph,
            &viz_opts,
            &viz_body,
            pane_block(&format!("graph · {}", viz_kind.as_str()), false),
            top[0],
        );
    }
    let legend_labels: Vec<String> = app.series.iter().map(|s| app.legend_label_for(s)).collect();
    chart::draw_legend(
        f,
        &app.series,
        &legend_labels,
        &app.legend_hidden,
        app.legend_selected,
        legend_focused,
        pane_block("legend", legend_focused),
        top[1],
    );
    // Editor row: editor on the left, params on the right - match the
    // chart row's right-column width so the two right-hand panes line up.
    let editor_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(right_w)])
        .split(body[1]);
    let editor_area = editor_row[0];
    let params_area = editor_row[1];
    draw_editor(f, app, editor_area);
    let params_focused = app.focus == crate::app::Pane::Params;
    draw_params(f, app, params_area, params_focused);

    draw_status(f, app, root[1]);

    if app.completions.visible {
        draw_completion_popup(f, app, editor_area);
    }

    if app.quickfix.visible {
        draw_quickfix_popup(f, app, editor_area);
    }

    if app.help_visible {
        draw_help_modal(f, app.help_scroll, top[0]);
    }

    if app.hover.is_some() {
        draw_hover_popup(f, app, editor_area);
    }

    if app.legend_details_visible {
        draw_legend_details(f, app, top[0]);
    }

    if let Some(err) = app.last_error.as_deref() {
        draw_error_overlay(f, err, top[0]);
    }

    // Dashinfo overlay sits below the picker in z-order; both are
    // mutually exclusive in practice.
    if app.dashinfo_visible
        && let Some(resource) = app.loaded_dashboard.as_ref()
    {
        draw_dashinfo_overlay(f, resource, f.area());
    }

    // Tile add-picker overlay: shown when `tile_submode == AddPick`.
    if let crate::app::TileSubMode::AddPick { cursor } = &app.tile_submode {
        draw_add_pick_overlay(f, *cursor, f.area());
    }

    // `:time` overlay: presets list, or two-month calendar picker for
    // the Custom variant. Drawn above tile overlays so its key handler
    // (which owns the modal input) is what the user sees.
    if let Some(state) = app.time_picker.as_ref() {
        draw_time_picker_overlay(f, app, state, f.area());
    }

    // Tile-JSON inspector overlay (from `:tile json`).
    if let Some(json) = app.tile_inspect_json.as_deref() {
        draw_tile_inspect_overlay(f, json, f.area());
    }

    // Confirm-delete overlay: shown when `tile_submode == ConfirmDelete`.
    if matches!(app.tile_submode, crate::app::TileSubMode::ConfirmDelete) {
        draw_confirm_delete_overlay(f, app, f.area());
    }

    // Dashboard picker comes last so it stacks above any other overlay.
    if app.dashboards.visible {
        draw_dashboards_picker(f, app, f.area());
    }
}

/// Multi-tile grid view of the loaded dashboard. Projects each
/// `LayoutItem`'s 12-column coordinates into `Rect`s carved out of the
/// graph pane, draws a bordered chrome block per tile, and highlights
/// the currently-selected one in yellow.
///
/// This is the step-18a read-only renderer: each tile shows its kind
/// glyph, name, and a one-line preview of its MPL/APL query. Per-tile
/// live data lights up in 18b once the per-`TileId` query-result state
/// migration lands.
/// Minimum number of terminal rows per virtual grid row **for rows
/// containing at least one non-Note tile**. At 4 cells/virt-row a
/// `h=2` tile gets 8 terminal rows - enough for a 1-row title
/// chrome plus a small chart. We let layouts grow past the viewport
/// and rely on scrolling rather than squashing.
const MIN_GRID_ROW_HEIGHT: u32 = 4;

/// Per-virt-row height used for rows that **only** contain Note
/// tiles. Notes are typically a heading or short paragraph and
/// don't need a chart-sized minimum; 2 terminal rows per virt-row
/// gives a 2-virt-row Note a 4-row tile (top border + 2 content +
/// bottom border).
const NOTE_ROW_HEIGHT: u32 = 2;

fn draw_dashboard_grid(f: &mut Frame, app: &mut App, area: Rect) {
    // Resolve everything we need from `loaded_dashboard` up front so
    // we can drop the borrow before mutating `app.dashboard_scroll`.
    let Some(resource) = app.loaded_dashboard.as_ref() else {
        return;
    };
    let charts = resource.dashboard.charts.clone();
    let layout = resource.dashboard.layout.clone();
    let dash_name = resource.name().to_string();

    // Outer frame for the whole dashboard pane.
    let focused = app.focus == crate::app::Pane::Dashboard;
    let submode_badge = match &app.tile_submode {
        crate::app::TileSubMode::Idle => "",
        crate::app::TileSubMode::Move { .. } => " MOVE",
        crate::app::TileSubMode::Resize { .. } => " RESIZE",
        crate::app::TileSubMode::ConfirmDelete => " DELETE?",
        crate::app::TileSubMode::AddPick { .. } => " ADD",
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
fn compute_row_heights(
    charts: &[crate::axiom::Chart],
    layout: &[crate::axiom::LayoutItem],
    virt_rows: usize,
    viewport_h: u32,
) -> Vec<u32> {
    let mut has_non_note = vec![false; virt_rows];
    for (i, chart) in charts.iter().enumerate() {
        if matches!(chart, crate::axiom::Chart::Note(_)) {
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
fn resolve_slot(
    layout: &[crate::axiom::LayoutItem],
    chart: &crate::axiom::Chart,
    idx: usize,
) -> (u32, u32, u32, u32) {
    if let Some(l) = layout.iter().find(|l| l.i == chart.base().id) {
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
fn draw_grid_tile(
    f: &mut Frame,
    app: &App,
    chart: &crate::axiom::Chart,
    area: Rect,
    highlighted: bool,
) {
    let base = chart.base();
    let kind_glyph = match chart {
        crate::axiom::Chart::TimeSeries(_) => "⌈⌉",
        crate::axiom::Chart::Heatmap(_) => "▦",
        crate::axiom::Chart::LogStream(_) => "≡",
        crate::axiom::Chart::Pie(_) => "●",
        crate::axiom::Chart::Scatter(_) => "⋮",
        crate::axiom::Chart::Table(_) => "⊞",
        crate::axiom::Chart::TopK(_) => "≡",
        crate::axiom::Chart::Statistic(_) => "No",
        crate::axiom::Chart::Note(_) => "✎",
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
        chart.type_str(),
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
    let body = if matches!(chart, crate::axiom::Chart::Note(_)) {
        let extras = &chart.base().extras;
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
        match crate::dashboard::classify_chart_query(chart) {
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
            // selection highlight - those are solo-mode UI concerns.
            let hidden = vec![false; series.len()];
            let body_text = String::new();
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

/// Modal overlay for `d` (delete) confirmation. Renders over the
/// dashboard pane; `y` confirms, any other key cancels (handled in
/// `App::handle_confirm_delete_key`).
fn draw_confirm_delete_overlay(f: &mut Frame, app: &App, screen: Rect) {
    let chart_label = app
        .loaded_dashboard
        .as_ref()
        .and_then(|r| r.dashboard.charts.get(app.selected_chart_idx))
        .map(|c| c.base().name.clone().unwrap_or_else(|| c.base().id.clone()))
        .unwrap_or_default();
    let lines = vec![
        Line::from(Span::styled(
            "Delete tile?",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::raw(chart_label)),
        Line::from(""),
        Line::from(Span::styled(
            "y to confirm • any other key to cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let width = 50u16.min(screen.width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(screen.height.saturating_sub(2));
    let area = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" delete ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
        inner,
    );
}

/// Read-only overlay showing a pretty-printed JSON dump of the
/// focused tile's chart payload. Opened by `:tile json` /
/// `:tile inspect` so the user can see exactly what the server
/// returned for a chart (handy when query classification looks wrong
/// - e.g. MPL queries stored under the `apl` key).
fn draw_tile_inspect_overlay(f: &mut Frame, json: &str, screen: Rect) {
    let width = screen.width.saturating_mul(8) / 10;
    let height = screen.height.saturating_mul(8) / 10;
    let width = width.clamp(60, 140);
    let height = height.clamp(15, 50);
    let area = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(" tile JSON (any key closes) ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(json.to_string())
            .style(Style::default().fg(Color::Gray))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// `:time` quick-select / custom calendar overlay. Dispatches by
/// state variant.
fn draw_time_picker_overlay(
    f: &mut Frame,
    app: &crate::app::App,
    state: &crate::app::TimePickerState,
    screen: Rect,
) {
    match state {
        crate::app::TimePickerState::Presets { cursor } => {
            draw_time_preset_overlay(f, app, *cursor, screen);
        }
        crate::app::TimePickerState::Custom(picker) => {
            draw_time_custom_overlay(f, picker, screen);
        }
    }
}

fn draw_time_preset_overlay(
    f: &mut Frame,
    app: &crate::app::App,
    cursor: usize,
    screen: Rect,
) {
    let presets = crate::app::TIME_PRESETS;
    // +1 entry for the trailing "Custom..." row.
    let row_count = (presets.len() as u16) + 1;
    let width = 36u16.min(screen.width.saturating_sub(4));
    // Borders (2) + title pad (1) + rows + hint (2).
    let height = (row_count + 5).min(screen.height.saturating_sub(2));
    let area = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(
                " time · {} → {} ",
                app.dashboard.time_range.start, app.dashboard.time_range.end
            ),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 {
        return;
    }

    // Reserve the last row for the keymap hint.
    let list_h = inner.height.saturating_sub(1);
    let list_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: list_h,
    };
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + list_h,
        width: inner.width,
        height: 1,
    };

    let mut items: Vec<ListItem<'_>> = presets
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            let style = if i == cursor {
                Style::default()
                    .bg(Color::Rgb(20, 60, 80))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(format!("  last {label}")).style(style)
        })
        .collect();
    let custom_idx = crate::app::TIME_PRESET_CUSTOM_INDEX;
    let custom_style = if cursor == custom_idx {
        Style::default()
            .bg(Color::Rgb(20, 60, 80))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Yellow)
    };
    items.push(ListItem::new("  Custom...").style(custom_style));
    f.render_widget(List::new(items), list_area);
    f.render_widget(
        Paragraph::new("j/k move · Enter apply · Esc close")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(ratatui::layout::Alignment::Center),
        hint_area,
    );
}

fn draw_time_custom_overlay(
    f: &mut Frame,
    picker: &crate::app::CustomRangePicker,
    screen: Rect,
) {
    // Two Monthly widgets side-by-side, each ~24 cols wide; plus a
    // header row showing the two selected dates and a footer hint.
    let width = 60u16.min(screen.width.saturating_sub(4));
    let height = 14u16.min(screen.height.saturating_sub(2));
    let area = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " custom range ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    // Header: "Start: YYYY-MM-DD  →  End: YYYY-MM-DD" with the
    // focused side highlighted.
    let focused_start = picker.focus == crate::app::CustomField::Start;
    let start_style = if focused_start {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let end_style = if !focused_start {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let header = Line::from(vec![
        Span::raw(" Start: "),
        Span::styled(format!(" {} ", picker.start), start_style),
        Span::raw("   End: "),
        Span::styled(format!(" {} ", picker.end), end_style),
    ]);
    let header_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    f.render_widget(Paragraph::new(header), header_area);

    // Hint row at the bottom.
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + inner.height - 1,
        width: inner.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new("Tab focus · h/j/k/l day/week · </> month · Enter apply · Esc back")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(ratatui::layout::Alignment::Center),
        hint_area,
    );

    // Calendar grid area between header and hint.
    let cal_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    let halves = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cal_area);

    draw_month(f, halves[0], picker.start, focused_start);
    draw_month(f, halves[1], picker.end, !focused_start);
}

fn draw_month(f: &mut Frame, area: Rect, selected: time::Date, focused: bool) {
    use ratatui::widgets::calendar::{CalendarEventStore, Monthly};
    let mut store = CalendarEventStore::default();
    let highlight = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    store.add(selected, highlight);
    let widget = Monthly::new(selected, store)
        .show_month_header(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .show_weekdays_header(Style::default().fg(Color::DarkGray))
        .show_surrounding(Style::default().fg(Color::Rgb(60, 60, 60)));
    f.render_widget(widget, area);
}

/// Modal kind-picker for `a` (add tile). Up/Down navigate, Enter
/// commits.
fn draw_add_pick_overlay(f: &mut Frame, cursor: usize, screen: Rect) {
    let kinds = crate::app::add_pick_kinds();
    let width = 30u16.min(screen.width.saturating_sub(4));
    let height = (kinds.len() as u16 + 3).min(screen.height.saturating_sub(2));
    let area = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(" add tile ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let items: Vec<ListItem<'_>> = kinds
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let style = if i == cursor {
                Style::default()
                    .bg(Color::Rgb(40, 90, 40))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(format!("  {}", k.as_str())).style(style)
        })
        .collect();
    f.render_widget(List::new(items), inner);
}

/// `:dashinfo` overlay - a read-only summary of the currently-loaded
/// dashboard. Shows name, description, time window, and a per-chart
/// table (id, type, name). Any key dismisses; the dismissal is wired
/// in `App::on_key`.
fn draw_dashinfo_overlay(f: &mut Frame, resource: &crate::axiom::DashboardSummary, screen: Rect) {
    let width = screen.width.saturating_mul(8) / 10;
    let height = screen.height.saturating_mul(8) / 10;
    let width = width.clamp(50, 120);
    let height = height.clamp(12, 40);
    let x = screen.x + (screen.width.saturating_sub(width)) / 2;
    let y = screen.y + (screen.height.saturating_sub(height)) / 2;
    let area = Rect {
        x,
        y,
        width,
        height,
    };

    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(
            format!(" dashboard · {} ", resource.name()),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height < 4 {
        return;
    }

    // Header lines: description, uid, time window.
    let doc = &resource.dashboard;
    let mut header: Vec<Line<'_>> = Vec::new();
    if let Some(desc) = resource.description() {
        header.push(Line::from(Span::styled(
            desc.to_string(),
            Style::default().fg(Color::Gray),
        )));
    }
    header.push(Line::from(vec![
        Span::styled("uid: ", Style::default().fg(Color::DarkGray)),
        Span::raw(resource.uid.clone()),
        Span::raw("  ·  "),
        Span::styled("updated: ", Style::default().fg(Color::DarkGray)),
        Span::raw(
            resource
                .updated_at
                .clone()
                .unwrap_or_else(|| "-".to_string()),
        ),
    ]));
    if doc.time_window_start.is_some() || doc.time_window_end.is_some() {
        header.push(Line::from(vec![
            Span::styled("window: ", Style::default().fg(Color::DarkGray)),
            Span::raw(doc.time_window_start.clone().unwrap_or_default()),
            Span::raw("  →  "),
            Span::raw(doc.time_window_end.clone().unwrap_or_default()),
        ]));
    }
    let header_h = header.len() as u16;
    let header_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: header_h.min(inner.height),
    };
    f.render_widget(Paragraph::new(header), header_rect);

    let table_y = inner.y + header_h + 1;
    if table_y >= inner.y + inner.height {
        return;
    }
    let table_h = inner.y + inner.height - table_y - 1;

    // Chart table.
    if doc.charts.is_empty() {
        let empty = Paragraph::new("(no charts on this dashboard)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(
            empty,
            Rect {
                x: inner.x,
                y: table_y,
                width: inner.width,
                height: 1,
            },
        );
    } else {
        let header_row = Line::from(vec![
            Span::styled(
                format!("{:<10}", "type"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<14}", "id"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "name",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let rows: Vec<Line<'_>> = std::iter::once(header_row)
            .chain(doc.charts.iter().map(|c| {
                let b = c.base();
                let id_short = if b.id.len() > 12 {
                    format!("{}...", &b.id[..11])
                } else {
                    b.id.clone()
                };
                Line::from(vec![
                    Span::styled(
                        format!("{:<10}", c.type_str()),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!("{:<14}", id_short),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(b.name.clone().unwrap_or_else(|| "-".to_string())),
                ])
            }))
            .collect();
        f.render_widget(
            Paragraph::new(rows),
            Rect {
                x: inner.x,
                y: table_y,
                width: inner.width,
                height: table_h,
            },
        );
    }

    // Footer hint.
    let hint = Paragraph::new(Line::from(Span::styled(
        "any key to close",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(
        hint,
        Rect {
            x: inner.x,
            y: inner.y + inner.height - 1,
            width: inner.width,
            height: 1,
        },
    );
}

/// Searchable dashboard picker. Renders as a centred modal with:
///
///   * filter line at the top (live-edited; cursor is the trailing `▁`)
///   * scrollable filtered list in the middle (current row reversed)
///   * key-hint footer
///
/// Selection on Enter is handled by
/// [`crate::app::App::handle_dashboards_picker_key`] which records the
/// dashboard id on `App.last_picked_dashboard`.
fn draw_dashboards_picker(f: &mut Frame, app: &App, screen: Rect) {
    let picker = &app.dashboards;
    let indices = picker.filtered_indices();

    // Modal size: 70% wide, 70% tall, capped.
    let width = screen.width.saturating_mul(7) / 10;
    let height = screen.height.saturating_mul(7) / 10;
    let width = width.clamp(40, 100);
    let height = height.clamp(10, 30);
    let x = screen.x + (screen.width.saturating_sub(width)) / 2;
    let y = screen.y + (screen.height.saturating_sub(height)) / 2;
    let area = Rect {
        x,
        y,
        width,
        height,
    };

    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(
                " dashboards · {}/{}  (Esc closes, Enter selects) ",
                indices.len(),
                picker.items.len()
            ),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height < 3 {
        return;
    }

    // Layout: filter row (1), gap (1), list (rest - 2), hint (1).
    let filter_y = inner.y;
    let list_y = inner.y + 2;
    let list_h = inner.height.saturating_sub(3);
    let hint_y = inner.y + inner.height - 1;

    // Filter line.
    let filter_line = Line::from(vec![
        Span::styled("filter› ", Style::default().fg(Color::Yellow)),
        Span::styled(
            picker.filter.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("▁", Style::default().fg(Color::DarkGray)),
    ]);
    let filter_rect = Rect {
        x: inner.x,
        y: filter_y,
        width: inner.width,
        height: 1,
    };
    f.render_widget(Paragraph::new(filter_line), filter_rect);

    // List.
    if indices.is_empty() {
        let empty = Paragraph::new("(no matches)")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(ratatui::layout::Alignment::Center);
        let r = Rect {
            x: inner.x,
            y: list_y,
            width: inner.width,
            height: list_h,
        };
        f.render_widget(empty, r);
    } else {
        // Scroll window: keep the cursor in view.
        let visible = list_h as usize;
        let scroll_start = if picker.cursor >= visible {
            picker.cursor - visible + 1
        } else {
            0
        };
        let items: Vec<ListItem<'_>> = indices
            .iter()
            .enumerate()
            .skip(scroll_start)
            .take(visible)
            .map(|(filter_idx, item_idx)| {
                let d = &picker.items[*item_idx];
                let name = d.name().to_string();
                let uid = format!("  ({})", d.uid);
                let desc = d
                    .description()
                    .map(|s| format!("  - {s}"))
                    .unwrap_or_default();
                let line = Line::from(vec![
                    Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(uid, Style::default().fg(Color::DarkGray)),
                    Span::styled(desc, Style::default().fg(Color::Gray)),
                ]);
                let style = if filter_idx == picker.cursor {
                    Style::default()
                        .bg(Color::Rgb(60, 60, 110))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(line).style(style)
            })
            .collect();
        let list = List::new(items);
        let r = Rect {
            x: inner.x,
            y: list_y,
            width: inner.width,
            height: list_h,
        };
        f.render_widget(list, r);
    }

    // Hint row.
    let hint = Line::from(Span::styled(
        "↑/↓ navigate • type to filter • Enter select • Esc cancel",
        Style::default().fg(Color::DarkGray),
    ));
    let hint_rect = Rect {
        x: inner.x,
        y: hint_y,
        width: inner.width,
        height: 1,
    };
    f.render_widget(Paragraph::new(hint), hint_rect);
}

fn draw_error_overlay(f: &mut Frame, msg: &str, graph_area: Rect) {
    // Wrap the message at the available width and pick a height that fits
    // the wrapped content within reasonable bounds (max 80% of the pane).
    let inner_width = graph_area.width.saturating_sub(6).max(20) as usize;
    let wrapped = wrap_message(msg, inner_width);
    let line_count = wrapped.len() as u16 + 2; // +2 for borders
    let max_h = graph_area.height.saturating_mul(4) / 5;
    let height = line_count.min(max_h).max(3);
    let width = (inner_width as u16 + 4)
        .min(graph_area.width.saturating_sub(4).max(20))
        .max(20);
    let x = graph_area.x + (graph_area.width.saturating_sub(width)) / 2;
    let y = graph_area.y + (graph_area.height.saturating_sub(height)) / 2;
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    let lines: Vec<Line<'_>> = wrapped.into_iter().map(Line::from).collect();
    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red))
            .title(Span::styled(
                " error - Esc to dismiss ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(Clear, area);
    f.render_widget(para, area);
}

/// Right-hand pane next to the editor. Lists every CLI/`:param` value
/// in `app.cli_params` as `$name = value`. Read-only; values are
/// managed via `:p NAME=VALUE` / `:p NAME=` / `:p!`. Empty state shows
/// a hint pointing at `:help` so the surface is discoverable.
fn draw_params(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    use crate::params::ParamStatus;

    let block = pane_block("params", focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = app.param_rows();
    if rows.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "no params",
                Style::default().add_modifier(Modifier::DIM),
            )),
            Line::from(""),
            Line::from(Span::styled(
                if focused {
                    "a: add  e: edit"
                } else {
                    ":p NAME=VALUE"
                },
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let selected_bg = Style::default().bg(Color::Rgb(40, 40, 60));
    let lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let (marker, marker_style) = match row.status {
                ParamStatus::Ok => (
                    "✓",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ParamStatus::TypeMismatch => (
                    "✗",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                ParamStatus::NotSet => ("○", Style::default().fg(Color::Yellow)),
                ParamStatus::OptionalUnset => ("○", Style::default().fg(Color::DarkGray)),
                ParamStatus::NotDeclared => ("⚠", Style::default().fg(Color::Yellow)),
            };

            let mut spans = vec![
                Span::raw(" "),
                Span::styled(marker.to_string(), marker_style),
                Span::raw(" "),
                Span::styled(format!("${}", row.name), Style::default().fg(Color::Cyan)),
            ];
            if let Some(ty) = &row.declared_type {
                spans.push(Span::styled(
                    format!(" : {ty}"),
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(
                    " : (undeclared)".to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                ));
            }
            if let Some(v) = &row.value {
                spans.push(Span::raw("  "));
                let value_style = match row.status {
                    ParamStatus::TypeMismatch => Style::default().fg(Color::Red),
                    _ => Style::default(),
                };
                spans.push(Span::styled(v.clone(), value_style));
            } else if !row.optional {
                spans.push(Span::styled(
                    "  (unset)".to_string(),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ));
            }

            let mut line = Line::from(spans);
            if focused && i == app.params_selected {
                line.style = selected_bg;
                for sp in &mut line.spans {
                    sp.style = sp.style.patch(selected_bg);
                }
            }
            line
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

/// Modal listing the tags of the currently-selected legend entry. Each
/// tag row is selectable; pressing Space toggles whether that tag
/// contributes to the legend label. The row under `details_cursor` gets
/// a row background; rows whose key is in `legend_label_tags` carry a
/// `✓` marker.
fn draw_legend_details(f: &mut Frame, app: &App, graph_area: Rect) {
    let Some(series) = app.series.get(app.legend_selected) else {
        return;
    };

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(vec![
            Span::styled(
                "name: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(series.name.clone(), Style::default().fg(Color::Gray)),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "tags  (j/k move, Space toggles, Esc/e closes):",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    if series.tags.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no tags)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, (k, v)) in series.tags.iter().enumerate() {
            let is_cursor = i == app.details_cursor;
            let is_picked = app.legend_label_tags.iter().any(|sk| sk == k);
            let bg = if is_cursor {
                Some(Color::Rgb(60, 60, 110))
            } else {
                None
            };
            let with_bg = |mut s: Style| -> Style {
                if let Some(b) = bg {
                    s = s.bg(b);
                }
                s
            };
            let mark = if is_picked { "✓" } else { " " };
            let mark_style = with_bg(
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            );
            let mut key_style = with_bg(Style::default().fg(Color::Yellow));
            if is_cursor {
                key_style = key_style.add_modifier(Modifier::BOLD);
            }
            let val_style = with_bg(Style::default().fg(Color::Gray));
            let pad_style = with_bg(Style::default());
            lines.push(Line::from(vec![
                Span::styled(" ".to_string(), pad_style),
                Span::styled(format!("{mark} "), mark_style),
                Span::styled(format!("{k:<16}"), key_style),
                Span::styled("  ".to_string(), pad_style),
                Span::styled(v.clone(), val_style),
                Span::styled(" ".to_string(), pad_style),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  points: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            series.points.len().to_string(),
            Style::default().fg(Color::Gray),
        ),
    ]));

    let body_w = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0) as u16;
    let width = (body_w.saturating_add(4)).clamp(40, graph_area.width.saturating_sub(2).max(40));
    let height = (lines.len() as u16 + 2).min(graph_area.height);
    let x = graph_area.x + graph_area.width.saturating_sub(width) / 2;
    let y = graph_area.y + graph_area.height.saturating_sub(height) / 2;
    let area = Rect {
        x,
        y,
        width,
        height,
    };

    let title = format!(
        " series details - {}/{} ",
        app.legend_selected + 1,
        app.series.len()
    );
    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(Clear, area);
    f.render_widget(para, area);
}

/// Build the `func(arg1: T, *arg2: T*)` span list for the status line. The
/// active argument is highlighted with bold + reversed colours so it
/// stands out even in a busy line.
fn render_sig_help(sh: &crate::hover::SigHelp) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(sh.args.len() * 2 + 3);
    spans.push(Span::styled(
        sh.label.clone(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("("));
    for (i, (name, typ)) in sh.args.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(", "));
        }
        let body = format!("{name}: {typ}");
        let style = if i == sh.active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(body, style));
    }
    spans.push(Span::raw(")"));
    spans
}

/// Hover popup: signature header + `info` doc paragraph. Anchored at the
/// editor cursor, mirroring the completion popup's positioning.
fn draw_hover_popup(f: &mut Frame, app: &mut App, editor_area: Rect) {
    let Some(hover) = app.hover.as_ref() else {
        return;
    };

    // Build content lines: signature on top, blank, doc paragraph below.
    let mut sig_spans: Vec<Span<'static>> = vec![
        Span::styled(
            hover.label.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("("),
    ];
    for (i, (name, typ)) in hover.args.iter().enumerate() {
        if i > 0 {
            sig_spans.push(Span::raw(", "));
        }
        sig_spans.push(Span::styled(
            format!("{name}: {typ}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    sig_spans.push(Span::raw(")"));

    let mut lines: Vec<Line<'_>> = vec![Line::from(sig_spans)];
    if let Some(doc) = hover.info.as_deref() {
        lines.push(Line::raw(""));
        for piece in doc.split('\n') {
            lines.push(Line::from(Span::styled(
                piece.to_string(),
                Style::default().fg(Color::Gray),
            )));
        }
    }

    let body_w = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0) as u16;
    let width = (body_w.saturating_add(4)).clamp(20, 80);
    let height = (lines.len() as u16).saturating_add(2).min(20);

    let (cursor_row, cursor_col) = app.editor.cursor();
    let anchor_x = editor_area
        .x
        .saturating_add(1 + cursor_col as u16)
        .min(editor_area.x + editor_area.width.saturating_sub(width));
    let mut anchor_y = editor_area.y.saturating_add(2 + cursor_row as u16);
    let screen = f.area();
    if anchor_y + height > screen.height {
        anchor_y = editor_area
            .y
            .saturating_add(1 + cursor_row as u16)
            .saturating_sub(height);
    }
    let area = Rect {
        x: anchor_x,
        y: anchor_y,
        width: width.min(screen.width.saturating_sub(anchor_x)),
        height: height.min(screen.height.saturating_sub(anchor_y)),
    };
    if area.width < 4 || area.height < 2 {
        return;
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                " hover - any key dismisses ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(Clear, area);
    f.render_widget(para, area);
}

/// Help modal listing the key bindings. Triggered by `:help`; any key
/// dismisses it. Rendered centred over the chart pane.
/// Embedded help content. Sourced from `docs/keys.md` at compile
/// time so the file ships with the binary and the in-app modal stays
/// in lockstep with the markdown reference.
const KEYS_HELP_SOURCE: &str = include_str!("../docs/keys.md");

fn draw_help_modal(f: &mut Frame, scroll: u16, graph_area: Rect) {
    let lines = render_keys_help(KEYS_HELP_SOURCE);

    // Layout: 80% of the graph pane in both dimensions, clamped to a
    // sensible band so it never gets unreadably narrow or eats the
    // whole screen on a 200-col terminal.
    let width = (graph_area.width.saturating_mul(8) / 10)
        .clamp(40, 100)
        .min(graph_area.width.saturating_sub(2).max(20));
    let height = (graph_area.height.saturating_mul(9) / 10)
        .clamp(8, 50)
        .min(graph_area.height.saturating_sub(2).max(5));
    let x = graph_area.x + (graph_area.width.saturating_sub(width)) / 2;
    let y = graph_area.y + (graph_area.height.saturating_sub(height)) / 2;
    let area = Rect {
        x,
        y,
        width,
        height,
    };

    // Clamp the scroll offset so G (= u16::MAX) lands on the last
    // page instead of off-screen.
    let inner_h = height.saturating_sub(2) as usize;
    let max_scroll = (lines.len()).saturating_sub(inner_h) as u16;
    let scroll = scroll.min(max_scroll);

    let title = if max_scroll == 0 {
        " help · any key dismisses ".to_string()
    } else {
        format!(
            " help · j/k scroll · g/G top/bottom · any other key dismisses ({}/{}) ",
            scroll + 1,
            max_scroll + 1
        )
    };
    let para = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(
                    title,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(Clear, area);
    f.render_widget(para, area);
}

/// Parse the help-file format into styled `Line`s.
///
/// Format:
///   * `## Section`             — a coloured heading.
///   * `key<TAB>description`    — two-column row.
///   * blank line               — vertical gap.
///   * `# anything`             — dropped (comment for editors).
///
/// The first heading is treated as a tiny preface paragraph (the
/// `# Key bindings` h1 plus its intro lines), so the in-app modal
/// skips lines until it hits the first `## ` block.
fn render_keys_help(src: &str) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut started = false;
    for raw in src.lines() {
        let line = raw.trim_end_matches('\r');
        if !started {
            if line.starts_with("## ") {
                started = true;
            } else {
                continue;
            }
        }
        if let Some(rest) = line.strip_prefix("## ") {
            // Blank line above section headers (except the first) so
            // sections breathe.
            if !out.is_empty()
                && !out
                    .last()
                    .map(|l| l.spans.is_empty())
                    .unwrap_or(false)
            {
                out.push(Line::raw(""));
            }
            out.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            continue;
        }
        if line.starts_with('#') {
            // h1 / single-hash comment — skip.
            continue;
        }
        if line.is_empty() {
            out.push(Line::raw(""));
            continue;
        }
        if let Some((key, desc)) = line.split_once('\t') {
            out.push(Line::from(vec![
                Span::styled(
                    format!("  {key:<22}"),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw("  "),
                Span::styled(desc.to_string(), Style::default().fg(Color::Gray)),
            ]));
        } else {
            // Plain prose row (e.g. paragraphs between sections).
            out.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(Color::Gray),
            )));
        }
    }
    out
}

/// Simple width-aware wrapper that preserves existing line breaks and only
/// splits over-long lines on whitespace (or hard-breaks if no whitespace is
/// reachable). Avoids pulling in `textwrap` for this one-off.
fn wrap_message(msg: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in msg.lines() {
        if line.chars().count() <= width {
            out.push(line.to_string());
            continue;
        }
        let mut buf = String::new();
        for word in line.split_whitespace() {
            if buf.is_empty() {
                buf.push_str(word);
            } else if buf.chars().count() + 1 + word.chars().count() <= width {
                buf.push(' ');
                buf.push_str(word);
            } else {
                out.push(std::mem::take(&mut buf));
                buf.push_str(word);
            }
            // Hard-break any token wider than the budget.
            while buf.chars().count() > width {
                let cutoff = buf
                    .char_indices()
                    .nth(width)
                    .map(|(i, _)| i)
                    .unwrap_or(buf.len());
                let rest = buf.split_off(cutoff);
                out.push(std::mem::take(&mut buf));
                buf = rest;
            }
        }
        if !buf.is_empty() {
            out.push(buf);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn pane_block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(style)
}

/// Editor pane renderer. Replaces `tui-textarea`'s default widget so we can
/// apply per-token styles from [`crate::highlight`]. We keep using
/// `tui-textarea` for edit state - only the drawing path is custom.
///
/// Lost visuals (acceptable):
///   * selection rendering (no visual mode),
///   * search overlay (no search),
///   * tui-textarea's `cursor_line_style` - replaced with a faint dark-grey
///     background on the cursor's row.
///
/// Cursor placement assumes char-width == display-width, which holds for
/// ASCII MPL queries. Backticked Unicode metric names will drift one
/// column; revisit only if anyone files it.
fn draw_editor(f: &mut Frame, app: &App, area: Rect) {
    let title = editor_title(app);
    let block = pane_block(&title, app.focus == crate::app::Pane::Editor);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let joined = app.editor.lines().join("\n");
    // The engine's `collect_tokens` is parser-aware (knows about
    // `pipe_keyword` vs argument keywords, regex literals, etc.) but only
    // emits a subset of grammar rules - `align`/`to`/`using`/`::`/extra `|`s
    // come back unclassified. The byte-scan fallback covers everything; we
    // always layer them so the engine's accuracy wins on overlap and the
    // fallback fills the gaps. When the engine returns `None` (mid-edit
    // parse failure) we use the fallback alone.
    let fallback = crate::highlight::fallback_tokens(&joined);
    let merged = match mpl_language_server::collect_tokens(&joined) {
        Some(engine) => crate::highlight::merge_tokens(&engine, &fallback),
        None => fallback,
    };
    let mut lines = crate::highlight::highlight_lines(&joined, Some(&merged));

    let (cursor_row, cursor_col) = app.editor.cursor();
    let visible_rows = inner.height as usize;
    let top = cursor_row.saturating_sub(visible_rows.saturating_sub(1));

    // Highlight the cursor row with a faint background - replaces the
    // tui-textarea `cursor_line_style` we no longer use.
    if let Some(line) = lines.get_mut(cursor_row) {
        let bg = Style::default().bg(Color::Rgb(28, 28, 28));
        line.style = bg;
        for sp in &mut line.spans {
            sp.style = sp.style.bg(Color::Rgb(28, 28, 28));
        }
    }

    // Visual selection: paint every row that the selection touches.
    // Whole-line granularity - character-precise spans would require
    // splitting at column boundaries, which doesn't pay for itself.
    if let Some((start_row, end_row, _)) = app.visual_row_range() {
        let sel_bg = Color::Rgb(60, 60, 110);
        for row in start_row..=end_row {
            let Some(line) = lines.get_mut(row) else {
                continue;
            };
            line.style = line.style.bg(sel_bg);
            for sp in &mut line.spans {
                sp.style = sp.style.bg(sel_bg);
            }
        }
    }

    let displayed: Vec<Line<'_>> = lines.into_iter().skip(top).take(visible_rows).collect();
    f.render_widget(Paragraph::new(displayed), inner);

    // Terminal cursor: only show when the editor is the focused surface
    // (Insert mode + Command mode are both "buffer" interactions, but in
    // Command mode the cmdline owns the cursor; in Normal mode we still
    // want a block to mark position).
    if app.mode != Mode::Command {
        let cursor_visible_row = cursor_row.saturating_sub(top);
        if cursor_visible_row < visible_rows {
            let x = inner
                .x
                .saturating_add(cursor_col as u16)
                .min(inner.x + inner.width.saturating_sub(1));
            let y = inner
                .y
                .saturating_add(cursor_visible_row as u16)
                .min(inner.y + inner.height.saturating_sub(1));
            f.set_cursor_position((x, y));
        }
    }
}

fn editor_title(app: &App) -> String {
    let name: String = match app.current_file.as_deref() {
        Some(p) => p
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| p.display().to_string()),
        None => "[No Name]".to_string(),
    };
    let modified = if app.is_dirty() { " [+]" } else { "" };
    let diag_suffix = diagnostic_count_suffix(app);
    format!("editor · {name}{modified}{diag_suffix}")
}

/// `" · 2 errors"` / `" · 1 error, 3 warnings"` / `""`. Info / hint counts are
/// intentionally omitted from the title to keep the chrome quiet - they
/// still show in the quick-fix picker.
fn diagnostic_count_suffix(app: &App) -> String {
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for d in &app.diagnostics {
        match d.severity {
            crate::mpl::Severity::Error => errors += 1,
            crate::mpl::Severity::Warning => warnings += 1,
            _ => {}
        }
    }
    if errors == 0 && warnings == 0 {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    if errors > 0 {
        parts.push(format!(
            "{errors} error{}",
            if errors == 1 { "" } else { "s" }
        ));
    }
    if warnings > 0 {
        parts.push(format!(
            "{warnings} warning{}",
            if warnings == 1 { "" } else { "s" }
        ));
    }
    format!(" · {}", parts.join(", "))
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    // Command mode replaces the status line entirely with a `:` prompt.
    if app.mode == Mode::Command {
        draw_command_line(f, app, area);
        return;
    }

    // When a non-editor pane has focus, the mode badge switches to a
    // dedicated label so the user can see which surface consumes keys.
    let pane_focus = app.focus;
    let (mode_label, mode_fg, mode_bg) = if pane_focus == crate::app::Pane::Legend {
        ("LEGEND".to_string(), Color::Black, Color::Cyan)
    } else if pane_focus == crate::app::Pane::Params {
        ("PARAMS".to_string(), Color::Black, Color::LightBlue)
    } else if pane_focus == crate::app::Pane::Dashboard {
        let base = "DASH".to_string();
        let label = match &app.tile_submode {
            crate::app::TileSubMode::Idle => base,
            crate::app::TileSubMode::Move { .. } => format!("{base}-MOVE"),
            crate::app::TileSubMode::Resize { .. } => format!("{base}-RESIZE"),
            crate::app::TileSubMode::ConfirmDelete => format!("{base}-DEL?"),
            crate::app::TileSubMode::AddPick { .. } => format!("{base}-ADD"),
        };
        (label, Color::Black, Color::Rgb(180, 140, 220))
    } else {
        let (fg, bg) = match app.mode {
            Mode::Normal => (Color::Black, Color::Yellow),
            Mode::Insert => (Color::Black, Color::Green),
            Mode::Visual | Mode::VisualLine => (Color::Black, Color::Magenta),
            Mode::Command => unreachable!(),
        };
        (app.mode.label().to_string(), fg, bg)
    };

    // Left chunk: mode badge + (diagnostic summary OR signature help OR
    // running status). Priority: errors > warnings > sig help > status.
    let mut left_spans = vec![
        Span::styled(
            format!(" {mode_label} "),
            Style::default()
                .fg(mode_fg)
                .bg(mode_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    let has_diag = app.diagnostics.iter().any(|d| {
        matches!(
            d.severity,
            crate::mpl::Severity::Error | crate::mpl::Severity::Warning
        )
    });
    if !has_diag && let Some(sh) = app.sig_help.as_ref() {
        left_spans.extend(render_sig_help(sh));
    } else {
        let (status_text, status_style) = diagnostic_status_or_default(app);
        left_spans.push(Span::styled(status_text, status_style));
    }
    let left = Line::from(left_spans);

    let mut right_parts: Vec<String> = Vec::new();
    if let Some(resource) = app.loaded_dashboard.as_ref() {
        right_parts.push(format!("dash: {}", resource.uid));
    }
    if let Some(t) = app.last_trace_id.as_deref() {
        right_parts.push(format!("trace: {t}"));
    }
    let right_text = right_parts.join("  ");
    let right = Line::from(Span::styled(
        right_text,
        Style::default().fg(Color::DarkGray),
    ))
    .alignment(ratatui::layout::Alignment::Right);

    f.render_widget(Paragraph::new(left), area);
    f.render_widget(Paragraph::new(right), area);
}

/// Pick the status string + style. Diagnostic summary wins when present;
/// otherwise the running query's `app.status` is shown in grey.
fn diagnostic_status_or_default(app: &App) -> (String, Style) {
    let first_error = app
        .diagnostics
        .iter()
        .find(|d| d.severity == crate::mpl::Severity::Error);
    let first_warn = app
        .diagnostics
        .iter()
        .find(|d| d.severity == crate::mpl::Severity::Warning);

    if let Some(d) = first_error {
        return (
            format!(
                "{} - {}:{}: {}",
                diagnostic_count_summary(app),
                d.line,
                d.column,
                d.message
            ),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        );
    }
    if let Some(d) = first_warn {
        return (
            format!(
                "{} - {}:{}: {}",
                diagnostic_count_summary(app),
                d.line,
                d.column,
                d.message
            ),
            Style::default().fg(Color::Yellow),
        );
    }

    let status_text = if app.busy {
        format!("{} ...", app.status)
    } else {
        app.status.clone()
    };
    (status_text, Style::default().fg(Color::Gray))
}

fn diagnostic_count_summary(app: &App) -> String {
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for d in &app.diagnostics {
        match d.severity {
            crate::mpl::Severity::Error => errors += 1,
            crate::mpl::Severity::Warning => warnings += 1,
            _ => {}
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if errors > 0 {
        parts.push(format!(
            "{errors} error{}",
            if errors == 1 { "" } else { "s" }
        ));
    }
    if warnings > 0 {
        parts.push(format!(
            "{warnings} warning{}",
            if warnings == 1 { "" } else { "s" }
        ));
    }
    parts.join(", ")
}

fn draw_command_line(f: &mut Frame, app: &App, area: Rect) {
    let prompt = ":";
    let line = Line::from(vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(app.cmdline.buf.clone()),
    ]);
    f.render_widget(Paragraph::new(line), area);

    // Place the terminal cursor right after the `:` plus typed chars.
    let cursor_col = area.x + prompt.chars().count() as u16 + app.cmdline.cursor as u16;
    let cursor_col = cursor_col.min(area.x + area.width.saturating_sub(1));
    f.set_cursor_position((cursor_col, area.y));

    // Tab-completion popup. Floats just above the cmdline.
    if app.cmdline_completions.visible && !app.cmdline_completions.items.is_empty() {
        draw_cmdline_completion_popup(f, app, area);
    }
}

/// Wildmenu-style popup for `:` cmdline completions. Renders a single
/// row above the cmdline with all candidates separated by spaces, the
/// current selection highlighted. When the row would overflow the
/// terminal width, scrolls horizontally so the selection stays
/// visible.
fn draw_cmdline_completion_popup(f: &mut Frame, app: &App, cmdline_area: Rect) {
    if cmdline_area.y == 0 {
        return; // no room above
    }
    let items = &app.cmdline_completions.items;
    let selected = app.cmdline_completions.selected;

    // Build the spans for each item with spaces between. Highlighted
    // item gets a reverse-video badge.
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(items.len() * 2);
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let style = if i == selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {item} "), style));
    }

    let row = Rect {
        x: cmdline_area.x,
        y: cmdline_area.y - 1,
        width: cmdline_area.width,
        height: 1,
    };
    // Background fill so we don't read through whatever was rendered
    // on the line beneath.
    f.render_widget(Clear, row);
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(28, 28, 28))),
        row,
    );
}

fn draw_completion_popup(f: &mut Frame, app: &mut App, editor_area: Rect) {
    let items_len = app.completions.items.len();
    if items_len == 0 {
        return;
    }
    let width = compute_popup_width(&app.completions.items);
    let height = (items_len.min(POPUP_MAX_ITEMS) as u16) + 2; // +2 for borders

    // Place the popup just below the editor cursor. Fall back to the editor's
    // top-left when geometry would push it off-screen.
    let (cursor_row, cursor_col) = app.editor.cursor();
    // The editor block has 1-cell borders; cursor is relative to inner area.
    let anchor_x = editor_area
        .x
        .saturating_add(1 + cursor_col as u16)
        .min(editor_area.x + editor_area.width.saturating_sub(width));
    let mut anchor_y = editor_area.y.saturating_add(2 + cursor_row as u16);
    let screen = f.area();
    if anchor_y + height > screen.height {
        // Flip above the cursor if no room below.
        anchor_y = editor_area
            .y
            .saturating_add(1 + cursor_row as u16)
            .saturating_sub(height);
    }
    let popup = Rect {
        x: anchor_x,
        y: anchor_y,
        width: width.min(screen.width.saturating_sub(anchor_x)),
        height: height.min(screen.height.saturating_sub(anchor_y)),
    };
    if popup.width < 4 || popup.height < 3 {
        return;
    }

    let items: Vec<ListItem<'_>> = app
        .completions
        .items
        .iter()
        .map(|it| ListItem::new(Line::from(Span::raw(it.label.clone()))))
        .collect();
    let mut state = ListState::default();
    state.select(Some(app.completions.selected));

    let title = if app.completions.kind_label.is_empty() {
        "completions".to_string()
    } else {
        format!("completions · {}", app.completions.kind_label)
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(Clear, popup);
    f.render_stateful_widget(list, popup, &mut state);
}

fn compute_popup_width(items: &[crate::completions::CompletionItem]) -> u16 {
    let max_item = items
        .iter()
        .map(|i| i.label.chars().count() as u16)
        .max()
        .unwrap_or(0);
    let with_padding = max_item.saturating_add(4); // borders + padding
    with_padding.clamp(POPUP_MIN_WIDTH, POPUP_MAX_WIDTH)
}

fn draw_quickfix_popup(f: &mut Frame, app: &mut App, editor_area: Rect) {
    let items_len = app.quickfix.actions.len();
    if items_len == 0 {
        return;
    }
    let width = quickfix_popup_width(&app.quickfix);
    let height = (items_len.min(POPUP_MAX_ITEMS) as u16) + 2; // +2 for borders

    let (cursor_row, cursor_col) = app.editor.cursor();
    let anchor_x = editor_area
        .x
        .saturating_add(1 + cursor_col as u16)
        .min(editor_area.x + editor_area.width.saturating_sub(width));
    let mut anchor_y = editor_area.y.saturating_add(2 + cursor_row as u16);
    let screen = f.area();
    if anchor_y + height > screen.height {
        anchor_y = editor_area
            .y
            .saturating_add(1 + cursor_row as u16)
            .saturating_sub(height);
    }
    let popup = Rect {
        x: anchor_x,
        y: anchor_y,
        width: width.min(screen.width.saturating_sub(anchor_x)),
        height: height.min(screen.height.saturating_sub(anchor_y)),
    };
    if popup.width < 4 || popup.height < 3 {
        return;
    }

    let items: Vec<ListItem<'_>> = app
        .quickfix
        .actions
        .iter()
        .map(|a| ListItem::new(Line::from(Span::raw(a.name.clone()))))
        .collect();
    let mut state = ListState::default();
    state.select(Some(app.quickfix.selected));

    let title = format!("quick fix · {}", app.quickfix.title);
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Magenta)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(Clear, popup);
    f.render_stateful_widget(list, popup, &mut state);
}

fn quickfix_popup_width(picker: &crate::app::QuickFixPicker) -> u16 {
    let max_item = picker
        .actions
        .iter()
        .map(|a| a.name.chars().count() as u16)
        .max()
        .unwrap_or(0);
    let title_w = picker.title.chars().count() as u16 + "quick fix · ".len() as u16;
    let with_padding = max_item.max(title_w).saturating_add(4);
    with_padding.clamp(POPUP_MIN_WIDTH, POPUP_MAX_WIDTH)
}

#[cfg(test)]
mod row_height_tests {
    use super::{MIN_GRID_ROW_HEIGHT, NOTE_ROW_HEIGHT, compute_row_heights};
    use crate::axiom::{Chart, ChartBase, LayoutItem};

    fn note(id: &str) -> Chart {
        Chart::Note(ChartBase {
            id: id.into(),
            name: None,
            query: None,
            extras: Default::default(),
        })
    }
    fn ts(id: &str) -> Chart {
        Chart::TimeSeries(ChartBase {
            id: id.into(),
            name: None,
            query: None,
            extras: Default::default(),
        })
    }
    fn slot(i: &str, x: u32, y: u32, w: u32, h: u32) -> LayoutItem {
        LayoutItem {
            i: i.into(),
            x,
            y: Some(y),
            w,
            h,
            extras: Default::default(),
        }
    }

    #[test]
    fn note_only_rows_shrink() {
        // Layout: Note h=2 at y=0, TimeSeries h=3 at y=2. Total
        // virt_rows = 5. Rows 0,1 are note-only; rows 2-4 are
        // non-note.
        let charts = vec![note("n"), ts("t")];
        let layout = vec![slot("n", 0, 0, 12, 2), slot("t", 0, 2, 12, 3)];
        let h = compute_row_heights(&charts, &layout, 5, 0);
        assert_eq!(h[0], NOTE_ROW_HEIGHT);
        assert_eq!(h[1], NOTE_ROW_HEIGHT);
        assert_eq!(h[2], MIN_GRID_ROW_HEIGHT);
        assert_eq!(h[3], MIN_GRID_ROW_HEIGHT);
        assert_eq!(h[4], MIN_GRID_ROW_HEIGHT);
    }

    #[test]
    fn row_with_both_note_and_chart_keeps_chart_min() {
        // Note h=4 and a chart h=2 share rows 0-1.
        let charts = vec![note("n"), ts("t")];
        let layout = vec![slot("n", 0, 0, 6, 4), slot("t", 6, 0, 6, 2)];
        let h = compute_row_heights(&charts, &layout, 4, 0);
        // Rows 0-1: chart present → min. Rows 2-3: note only → shrunk.
        assert_eq!(h[0], MIN_GRID_ROW_HEIGHT);
        assert_eq!(h[1], MIN_GRID_ROW_HEIGHT);
        assert_eq!(h[2], NOTE_ROW_HEIGHT);
        assert_eq!(h[3], NOTE_ROW_HEIGHT);
    }

    #[test]
    fn surplus_grows_only_non_note_rows() {
        // 1 note row + 1 chart row, viewport big enough for surplus.
        let charts = vec![note("n"), ts("t")];
        let layout = vec![slot("n", 0, 0, 12, 1), slot("t", 0, 1, 12, 1)];
        let viewport = NOTE_ROW_HEIGHT + MIN_GRID_ROW_HEIGHT + 10;
        let h = compute_row_heights(&charts, &layout, 2, viewport);
        assert_eq!(h[0], NOTE_ROW_HEIGHT, "note row stays compact");
        assert_eq!(h[1], MIN_GRID_ROW_HEIGHT + 10, "chart row absorbs surplus");
    }

    #[test]
    fn total_overflows_viewport_no_growth() {
        // Content already exceeds viewport → no surplus, no growth.
        let charts = vec![ts("a"), ts("b")];
        let layout = vec![slot("a", 0, 0, 12, 5), slot("b", 0, 5, 12, 5)];
        let h = compute_row_heights(&charts, &layout, 10, 4);
        for v in &h {
            assert_eq!(*v, MIN_GRID_ROW_HEIGHT);
        }
    }
}

#[cfg(test)]
mod help_render_tests {
    use super::{KEYS_HELP_SOURCE, render_keys_help};

    #[test]
    fn embedded_help_file_has_expected_sections() {
        // Sanity-check that the embedded help file isn't empty and
        // covers a few representative bindings the user would expect
        // to find by hitting `?`.
        assert!(KEYS_HELP_SOURCE.contains("## Normal mode: motion"));
        assert!(KEYS_HELP_SOURCE.contains("## Dashboard pane"));
        assert!(KEYS_HELP_SOURCE.contains("## Time picker"));
        assert!(KEYS_HELP_SOURCE.contains(":trace"));
        assert!(KEYS_HELP_SOURCE.contains(":time / :range"));
    }

    #[test]
    fn render_keys_help_skips_preface_and_keeps_sections() {
        let src = "# Title\nIntro line.\n\n## First\nh\tleft\nj\tdown\n\n## Second\nq\tquit\n";
        let lines = render_keys_help(src);
        // The h1 `# Title` and its intro must be stripped; the first
        // emitted line is the `First` heading.
        let first = format!("{:?}", lines[0]);
        assert!(
            first.contains("First"),
            "expected first heading, got {first:?}"
        );
        // Make sure every key column shows up somewhere.
        let rendered = lines
            .iter()
            .map(|l| format!("{l:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        for needle in ["h ", "j ", "q ", "left", "down", "quit", "Second"] {
            assert!(
                rendered.contains(needle),
                "missing {needle:?} in rendered help: {rendered}"
            );
        }
    }

    #[test]
    fn render_keys_help_drops_single_hash_comments() {
        let src = "## S\n# this is a comment\nk\tup\n";
        let lines = render_keys_help(src);
        let rendered = lines
            .iter()
            .map(|l| format!("{l:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!rendered.contains("comment"));
        assert!(rendered.contains("up"));
    }
}
