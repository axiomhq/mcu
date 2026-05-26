//! Bottom status line + `:` cmdline + cmdline completion popup.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

use crate::app::{App, Mode};

pub(super) fn draw_status(f: &mut Frame, app: &App, area: Rect) {
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
            crate::app::TileSubMode::PickViz {
                action: crate::app::PickVizAction::Add,
                ..
            } => format!("{base}-ADD"),
            crate::app::TileSubMode::PickViz {
                action: crate::app::PickVizAction::Open { above: false, .. },
                ..
            } => format!("{base}-OPEN↓"),
            crate::app::TileSubMode::PickViz {
                action: crate::app::PickVizAction::Open { above: true, .. },
                ..
            } => format!("{base}-OPEN↑"),
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
    if app.cmdline.completions.visible && !app.cmdline.completions.items.is_empty() {
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
    let items = &app.cmdline.completions.items;
    let selected = app.cmdline.completions.selected;

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
