//! `:help` modal + the `docs/keys.md` parser that powers it.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::{centered_area, modal_frame};

/// Embedded help content. Sourced from `docs/keys.md` at compile time
/// so the file ships with the binary and the in-app modal stays in
/// lockstep with the markdown reference.
pub(super) const KEYS_HELP_SOURCE: &str = include_str!("../../docs/keys.md");

/// Help modal listing the key bindings. Triggered by `:help`; any key
/// dismisses it. Rendered centred over the chart pane.
pub(super) fn draw_help_modal(f: &mut Frame, scroll: u16, graph_area: Rect) {
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

    // Clamp the scroll offset so G (= u16::MAX) lands on the last
    // page instead of off-screen.
    let inner_h = height.saturating_sub(2) as usize;
    let max_scroll = (lines.len()).saturating_sub(inner_h) as u16;
    let scroll = scroll.min(max_scroll);

    let title = if max_scroll == 0 {
        " help ".to_string()
    } else {
        format!(" help · {}/{} ", scroll + 1, max_scroll + 1)
    };
    let inner = modal_frame(
        f,
        centered_area(graph_area, width, height),
        &title,
        Color::Cyan,
    );
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
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
pub(super) fn render_keys_help(src: &str) -> Vec<Line<'static>> {
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
            if !out.is_empty() && !out.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
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
                Span::styled(format!("  {key:<22}"), Style::default().fg(Color::Yellow)),
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
