//! Centred modal overlays: confirm-delete, tile-inspect, add-tile picker,
//! dashinfo, dashboards picker, error banner, legend-details.
//!
//! Each function here owns its width/height math, a `modal_frame` call,
//! and its content rendering. Cursor-anchored popups (completion,
//! quickfix, hover, cmdline) live in their own modules.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};

use super::{centered_area, modal_frame, wrap_message};
use crate::app::App;
use crate::axiom::{ChartKnownExt, DashboardSummaryExt};

/// Modal overlay for `d` (delete) confirmation. Renders over the
/// dashboard pane; `y` confirms, any other key cancels (handled in
/// `App::handle_confirm_delete_key`).
pub(super) fn draw_confirm_delete_overlay(f: &mut Frame, app: &App, screen: Rect) {
    let chart_label = app
        .loaded_dashboard
        .as_ref()
        .and_then(|r| r.dashboard.charts.get(app.selected_chart_idx))
        .map(|c| {
            c.known_base()
                .name
                .clone()
                .unwrap_or_else(|| c.known_base().id.clone())
        })
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
    let inner = modal_frame(
        f,
        centered_area(screen, width, height),
        " delete ",
        Color::Red,
    );
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
pub(super) fn draw_tile_inspect_overlay(f: &mut Frame, json: &str, screen: Rect) {
    let width = (screen.width.saturating_mul(8) / 10).clamp(60, 140);
    let height = (screen.height.saturating_mul(8) / 10).clamp(15, 50);
    let inner = modal_frame(
        f,
        centered_area(screen, width, height),
        " tile JSON ",
        Color::Magenta,
    );
    f.render_widget(
        Paragraph::new(json.to_string())
            .style(Style::default().fg(Color::Gray))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// Modal viz-kind picker shared by `a` (add tile) and `o`/`O` (open
/// new row). Up/Down navigate, Enter commits. The frame title
/// reflects the action so the user knows which path they're on.
pub(super) fn draw_pick_viz_overlay(
    f: &mut Frame,
    cursor: usize,
    action: crate::app::PickVizAction,
    screen: Rect,
) {
    let kinds = crate::app::add_pick_kinds();
    let width = 30u16.min(screen.width.saturating_sub(4));
    let height = (kinds.len() as u16 + 3).min(screen.height.saturating_sub(2));
    let title = match action {
        crate::app::PickVizAction::Add => " add tile ",
        crate::app::PickVizAction::Open { above: false, .. } => " open below ",
        crate::app::PickVizAction::Open { above: true, .. } => " open above ",
    };
    let inner = modal_frame(f, centered_area(screen, width, height), title, Color::Green);
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
pub(super) fn draw_dashinfo_overlay(
    f: &mut Frame,
    resource: &crate::axiom::DashboardSummary,
    screen: Rect,
) {
    let width = (screen.width.saturating_mul(8) / 10).clamp(50, 120);
    let height = (screen.height.saturating_mul(8) / 10).clamp(12, 40);
    let title = format!(" dashboard · {} ", resource.name_or_unnamed());
    let inner = modal_frame(
        f,
        centered_area(screen, width, height),
        &title,
        Color::Magenta,
    );
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
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
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
                let b = c.known_base();
                let id_short = if b.id.len() > 12 {
                    format!("{}...", &b.id[..11])
                } else {
                    b.id.clone()
                };
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{:<10}",
                            c.type_str()
                                .expect("mcu expects Chart::Known; got Chart::Unknown")
                        ),
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
pub(super) fn draw_dashboards_picker(f: &mut Frame, app: &App, screen: Rect) {
    let picker = &app.dashboards;
    let indices = picker.filtered_indices();

    // Modal size: 70% wide, 70% tall, capped.
    let width = (screen.width.saturating_mul(7) / 10).clamp(40, 100);
    let height = (screen.height.saturating_mul(7) / 10).clamp(10, 30);
    let title = format!(" dashboards · {}/{} ", indices.len(), picker.items.len());
    let inner = modal_frame(f, centered_area(screen, width, height), &title, Color::Cyan);
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
                let name = d.name_or_unnamed().to_string();
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

pub(super) fn draw_error_overlay(f: &mut Frame, msg: &str, graph_area: Rect) {
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
    let inner = modal_frame(
        f,
        centered_area(graph_area, width, height),
        " error ",
        Color::Red,
    );
    let lines: Vec<Line<'_>> = wrapped.into_iter().map(Line::from).collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// Modal listing the tags of the currently-selected legend entry. Each
/// tag row is selectable; pressing Space toggles whether that tag
/// contributes to the legend label. The row under `details_cursor` gets
/// a row background; rows whose key is in `legend_label_tags` carry a
/// `✓` marker.
pub(super) fn draw_legend_details(f: &mut Frame, app: &App, graph_area: Rect) {
    // Mirror the legend's source pick: in Grid view the picker
    // edits tags on the focused dashboard tile's series, not the
    // editor query's. Without this swap the modal always showed
    // sin(x) and "(no tags)" in dashboard mode.
    let series_slice = app.active_legend_series();
    if series_slice.is_empty() {
        return;
    }
    let idx = app.legend.selected.min(series_slice.len() - 1);
    let Some(series) = series_slice.get(idx) else {
        return;
    };

    // Fixed lines that frame the scrollable tag list. Kept as their own
    // vectors so we can compute width across everything but only scroll
    // the tag rows.
    let header_lines: Vec<Line<'static>> = vec![
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
    let footer_lines: Vec<Line<'static>> = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  points: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                series.points.len().to_string(),
                Style::default().fg(Color::Gray),
            ),
        ]),
    ];

    // Build the per-tag rows once; we'll slice into a visible window
    // below once we know the modal's height.
    let tag_rows: Vec<Line<'static>> = if series.tags.is_empty() {
        vec![Line::from(Span::styled(
            "  (no tags)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        series
            .tags
            .iter()
            .enumerate()
            .map(|(i, (k, v))| {
                let is_cursor = i == app.legend.details_cursor;
                let is_picked = app.legend.label_tags.iter().any(|sk| sk == k);
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
                Line::from(vec![
                    Span::styled(" ".to_string(), pad_style),
                    Span::styled(format!("{mark} "), mark_style),
                    Span::styled(format!("{k:<16}"), key_style),
                    Span::styled("  ".to_string(), pad_style),
                    Span::styled(crate::chart::tag_text(v), val_style),
                    Span::styled(" ".to_string(), pad_style),
                ])
            })
            .collect()
    };

    // Width is the widest line across everything, clamped to the graph.
    let line_width =
        |l: &Line<'_>| -> usize { l.spans.iter().map(|s| s.content.chars().count()).sum() };
    let body_w = header_lines
        .iter()
        .chain(tag_rows.iter())
        .chain(footer_lines.iter())
        .map(line_width)
        .max()
        .unwrap_or(0) as u16;
    let width = (body_w.saturating_add(4)).clamp(40, graph_area.width.saturating_sub(2).max(40));

    // Decide the modal height: ideally everything fits, but cap to the
    // graph area minus the two border rows.
    let total_lines = (header_lines.len() + tag_rows.len() + footer_lines.len()) as u16;
    let max_inner = graph_area.height.saturating_sub(2);
    let inner_h = total_lines.min(max_inner);
    let height = inner_h.saturating_add(2);
    let title = format!(" series details - {}/{} ", idx + 1, series_slice.len());
    let inner = modal_frame(
        f,
        centered_area(graph_area, width, height),
        &title,
        Color::Cyan,
    );

    // Slice the tag rows to a visible window that keeps the cursor in
    // view. When the list overflows, the first / last visible row is
    // replaced with a hint that tells the user how many rows are hidden.
    let fixed_h = (header_lines.len() + footer_lines.len()) as u16;
    let tag_capacity = inner.height.saturating_sub(fixed_h) as usize;
    let visible_rows: Vec<Line<'static>> = if tag_rows.len() <= tag_capacity || tag_capacity == 0 {
        tag_rows
    } else {
        let cursor = app.legend.details_cursor.min(tag_rows.len() - 1);
        let mut start = cursor
            .saturating_sub(tag_capacity - 1)
            .min(tag_rows.len().saturating_sub(tag_capacity));
        // Keep the cursor visible when it walks back upward too.
        if cursor < start {
            start = cursor;
        }
        let end = (start + tag_capacity).min(tag_rows.len());
        let mut window: Vec<Line<'static>> = tag_rows[start..end].to_vec();
        let hint_style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC);
        if start > 0 {
            window[0] = Line::from(Span::styled(
                format!("  ↑ {} more above", start),
                hint_style,
            ));
        }
        if end < tag_rows.len() {
            let last = window.len() - 1;
            window[last] = Line::from(Span::styled(
                format!("  ↓ {} more below", tag_rows.len() - end),
                hint_style,
            ));
        }
        window
    };

    let mut lines =
        Vec::with_capacity(header_lines.len() + visible_rows.len() + footer_lines.len());
    lines.extend(header_lines);
    lines.extend(visible_rows);
    lines.extend(footer_lines);
    f.render_widget(Paragraph::new(lines), inner);
}
