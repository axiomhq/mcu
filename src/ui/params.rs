//! Right-hand params pane (read-only listing of `$name = value`).

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::pane_block;
use crate::app::App;

/// Right-hand pane next to the editor. Lists every CLI/`:param` value
/// in `app.params.cli` as `$name = value`. Read-only; values are
/// managed via `:p NAME=VALUE` / `:p NAME=` / `:p!`. Empty state shows
/// a hint pointing at `:help` so the surface is discoverable.
pub(super) fn draw_params(f: &mut Frame, app: &App, area: Rect, focused: bool) {
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
            if focused && i == app.params.selected {
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
