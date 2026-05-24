//! Hover popup: signature header + `info` doc paragraph, anchored at
//! the editor cursor.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::modal_frame;
use crate::app::App;

/// Hover popup: signature header + `info` doc paragraph. Anchored at the
/// editor cursor, mirroring the completion popup's positioning.
pub(super) fn draw_hover_popup(f: &mut Frame, app: &mut App, editor_area: Rect) {
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

    let inner = modal_frame(f, area, " hover ", Color::Cyan);
    f.render_widget(Paragraph::new(lines), inner);
}
