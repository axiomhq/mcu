//! Top-level render dispatch + shared chrome helpers.
//!
//! The bulk of the rendering lives in surface-specific submodules:
//!
//! - `grid`        — multi-tile dashboard view + inline legend strip
//! - `editor`      — main editor pane (highlighted text + cursor)
//! - `status`      — bottom status bar + `:` cmdline + cmdline popup
//! - `params`      — right-hand params pane
//! - `hover`       — hover popup anchored at the editor cursor
//! - `help`        — `:help` modal + embedded keys-md parser
//! - `overlays`    — centred modal overlays (confirm-delete, dashinfo,
//!   dashboards picker, error banner, legend-details, …)
//! - `time_picker` — `:time` quick-select + custom calendar picker
//! - `popups`      — cursor-anchored completion / quickfix popups
//!
//! Anything cross-cutting (pane chrome, modal frames, centred-area
//! geometry, message wrapping) lives here in `mod.rs`.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Clear},
};

use crate::app::App;

use crate::chart;
use crate::viz;

mod editor;
mod grid;
mod help;
mod hover;
mod overlays;
mod params;
mod popups;
mod status;
mod time_picker;
mod topbar;
mod trace;

// Re-exported so `:trace` (bare form) can open the same trace the
// status bar shows, and so unit tests can assert the resolver
// without rendering a frame.
pub(crate) use status::status_trace_id;

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

/// Centred sub-rectangle inside `parent`. `width` and `height` are in
/// terminal cells; if either exceeds the parent's corresponding
/// dimension, the rectangle collapses on that axis (the `saturating_sub`
/// keeps things sane).
pub(super) fn centered_area(parent: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: parent.x + (parent.width.saturating_sub(width)) / 2,
        y: parent.y + (parent.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// Shared modal/overlay chrome: clears whatever's behind `area`, draws
/// a single-border block tinted with `color` and titled `title` (in the
/// same colour, bold), and returns the inner rect for the caller to fill
/// with content.
pub(super) fn modal_frame(f: &mut Frame, area: Rect, title: &str, color: Color) -> Rect {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// Standard pane chrome: 1-cell border in yellow when focused, dark
/// grey otherwise, with the supplied `title` set on the top border.
pub(super) fn pane_block(title: &str, focused: bool) -> Block<'_> {
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

/// Simple width-aware wrapper that preserves existing line breaks and only
/// splits over-long lines on whitespace (or hard-breaks if no whitespace is
/// reachable). Avoids pulling in `textwrap` for this one-off.
pub(super) fn wrap_message(msg: &str, width: usize) -> Vec<String> {
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

pub fn draw(f: &mut Frame, app: &mut App) {
    // Root layout: 1-row topbar (query/dashboard menu strip), main
    // content, 1-row status bar. The topbar is purely informational
    // chrome — see [`topbar`] — but anchoring it here keeps the rest
    // of `draw` ignorant of it.
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    topbar::draw_topbar(f, app, root[0]);

    // Trace view takes over the entire body — no editor / params
    // / legend chrome. The status bar + topbar still render via
    // the shared paths below; overlay handling is also unchanged
    // except that none of the dashboard overlays apply (their
    // visibility flags don't get set in this view).
    if app.view_mode == crate::app::ViewMode::Trace {
        trace::draw_trace(f, app, root[1]);
        status::draw_status(f, app, root[2]);
        if let Some(err) = app.last_error.as_deref() {
            overlays::draw_error_overlay(f, err, root[1]);
        }
        if app.help.visible {
            help::draw_help_modal(f, app.help.scroll, root[1]);
        }
        return;
    }

    let bottom_h = capped(
        root[1].height,
        BOTTOM_ROW_PCT,
        BOTTOM_ROW_MIN,
        BOTTOM_ROW_MAX,
    );
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(bottom_h)])
        .split(root[1]);

    let right_w = capped(body[0].width, RIGHT_COL_PCT, RIGHT_COL_MIN, RIGHT_COL_MAX);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(right_w)])
        .split(body[0]);

    let legend_focused = app.focus == crate::app::Pane::Legend;
    let selected_for_graph = if legend_focused {
        Some(app.legend.selected)
    } else {
        None
    };

    if app.view_mode == crate::app::ViewMode::Grid && app.loaded_dashboard.is_some() {
        grid::draw_dashboard_grid(f, app, top[0]);
    } else {
        let viz_kind = app.viz_kind;
        let viz_opts = app.viz_opts.clone();
        // `body` is the tile's underlying text. Note tiles read this
        // as markdown; metrics tiles ignore it (they consume `series`
        // instead).
        let viz_body = app.query_text();
        // APL Table / LogStream short-circuit: when the buffer
        // language produced a `table_result` (typed columns from
        // the APL decoder), render it directly. The default
        // `viz::draw` path for Table would otherwise go through
        // `series_to_table`, which forces every row through
        // `Agg::Last` — collapsing N typed rows into a single
        // row of last-values.
        let wants_table = matches!(
            viz_kind,
            crate::dashboard::VizKind::Table | crate::dashboard::VizKind::LogStream
        );
        if wants_table && let Some(t) = app.table_result.as_ref() {
            // Always show a selection cursor so the user can see
            // scroll position; only paint it actively while the
            // Table pane is focused so an unfocused table doesn't
            // grab visual weight from the editor.
            let selected = if t.rows.is_empty() {
                None
            } else {
                Some(app.table_selected.min(t.rows.len() - 1))
            };
            let table_focused = app.focus == crate::app::Pane::Table;
            viz::draw_table_result(
                f,
                t,
                selected,
                table_focused,
                pane_block(&format!("graph · {}", viz_kind.as_str()), table_focused),
                top[0],
            );
        } else {
            viz::draw(
                f,
                viz_kind,
                &app.series,
                &app.legend.hidden,
                selected_for_graph,
                &viz_opts,
                &viz_body,
                app.unit.as_ref(),
                pane_block(&format!("graph · {}", viz_kind.as_str()), false),
                top[0],
            );
        }
    }
    // Side legend source switches in Grid view: instead of the
    // editor's query result, mirror the focused dashboard tile's
    // series so the legend reflects the panel the user is
    // navigating. Hidden state isn't tracked per-tile yet, so the
    // Grid path uses a fresh all-visible mask; selection is
    // clamped into range.
    let (legend_series, legend_hidden_owned, legend_selected_for_render, legend_title): (
        &[crate::chart::Series],
        Option<Vec<bool>>,
        usize,
        String,
    ) = if app.view_mode == crate::app::ViewMode::Grid
        && let Some(resource) = app.loaded_dashboard.as_ref()
        && let Some(chart) = resource.dashboard.charts.get(app.selected_chart_idx)
        // `Chart::Unknown` has no `ChartBase` (no id, no name) so it
        // can't drive the legend; fall through to the editor's
        // last series in that case (handled by the `else` below).
        && let Some(base) = chart.base()
    {
        let label = base.name.clone().unwrap_or_else(|| base.id.clone());
        let series_slice: &[crate::chart::Series] = app
            .tile_results
            .get(&base.id)
            .map(|t| t.series.as_slice())
            .unwrap_or(&[]);
        let n = series_slice.len();
        let selected = if n == 0 {
            0
        } else {
            app.legend.selected.min(n - 1)
        };
        (
            series_slice,
            Some(vec![false; n]),
            selected,
            format!("legend · {label}"),
        )
    } else {
        (
            app.series.as_slice(),
            None,
            app.legend.selected,
            "legend".to_string(),
        )
    };
    let legend_hidden_slice: &[bool] = match legend_hidden_owned.as_ref() {
        Some(v) => v.as_slice(),
        None => app.legend.hidden.as_slice(),
    };
    chart::draw_legend(
        f,
        legend_series,
        &app.legend.label_tags,
        legend_hidden_slice,
        legend_selected_for_render,
        legend_focused,
        pane_block(&legend_title, legend_focused),
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
    editor::draw_editor(f, app, editor_area);
    let params_focused = app.focus == crate::app::Pane::Params;
    params::draw_params(f, app, params_area, params_focused);

    // Stash Solo/Grid pane rects for mouse hit-testing (step 27).
    // `dashboard` is only meaningful when the grid pane occupies
    // `top[0]`; otherwise it's zeroed so a click can't match it.
    let grid_active = app.view_mode == crate::app::ViewMode::Grid && app.loaded_dashboard.is_some();
    app.mouse_geom.graph = top[0];
    app.mouse_geom.legend = top[1];
    app.mouse_geom.editor = editor_area;
    app.mouse_geom.params = params_area;
    app.mouse_geom.dashboard = if grid_active { top[0] } else { Rect::default() };

    status::draw_status(f, app, root[2]);

    if app.completions.visible {
        popups::draw_completion_popup(f, app, editor_area);
    }

    if app.quickfix.visible {
        popups::draw_quickfix_popup(f, app, editor_area);
    }

    if app.help.visible {
        help::draw_help_modal(f, app.help.scroll, top[0]);
    }

    if app.hover.is_some() {
        hover::draw_hover_popup(f, app, editor_area);
    }

    if app.legend.details_visible {
        overlays::draw_legend_details(f, app, top[0]);
    }

    if let Some(err) = app.last_error.as_deref() {
        overlays::draw_error_overlay(f, err, top[0]);
    }

    // Dashinfo overlay sits below the picker in z-order; both are
    // mutually exclusive in practice.
    if app.dashinfo_visible
        && let Some(resource) = app.loaded_dashboard.as_ref()
    {
        overlays::draw_dashinfo_overlay(f, resource, f.area());
    }

    // `:history` overlay — read-only listing of past `:` commands.
    if app.history_overlay_visible {
        overlays::draw_history_overlay(f, app, f.area());
    }

    // Viz-kind picker overlay — shared by `a` (add) and `o`/`O` (open).
    if let crate::app::TileSubMode::PickViz { cursor, action } = &app.tile_submode {
        overlays::draw_pick_viz_overlay(f, *cursor, *action, f.area());
    }

    // `:time` overlay: presets list, or two-month calendar picker for
    // the Custom variant. Drawn above tile overlays so its key handler
    // (which owns the modal input) is what the user sees.
    if let Some(state) = app.time.picker.as_ref() {
        time_picker::draw_time_picker_overlay(f, app, state, f.area());
    }

    // Tile-JSON inspector overlay (from `:tile json`).
    if let Some(json) = app.tile_inspect_json.as_deref() {
        overlays::draw_tile_inspect_overlay(f, json, f.area());
    }

    // Confirm-delete overlay: shown when `tile_submode == ConfirmDelete`.
    if matches!(app.tile_submode, crate::app::TileSubMode::ConfirmDelete) {
        overlays::draw_confirm_delete_overlay(f, app, f.area());
    }

    // Dashboard picker comes last so it stacks above any other overlay.
    if app.dashboards.visible {
        overlays::draw_dashboards_picker(f, app, f.area());
    }
}

#[cfg(test)]
mod tests;
