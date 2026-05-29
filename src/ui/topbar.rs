//! Top "menu" strip that makes the editing **scope** unambiguous.
//!
//! The bottom status bar already shows the Vim-style mode (NORMAL,
//! INSERT, …) and the focused pane (LEGEND, PARAMS, DASH-MOVE, …), but
//! none of that answers the more fundamental question of *what the
//! buffer represents*: is `:w` going to write an MPL/APL file, or the
//! JSON of a dashboard? That distinction is [`BufferMode`] and it
//! lives off-screen today, so users frequently can't tell whether
//! they're "in a query" or "in a dashboard".
//!
//! This module renders a single-row tab strip at the very top of the
//! frame:
//!
//! ```text
//!  QUERY │ DASHBOARD                           My Dashboard (abcd1234)
//! ```
//!
//! The active tab is rendered as a black-on-yellow badge (matching the
//! existing mode-badge styling in the bottom status line so the chrome
//! feels consistent). The inactive tab is dim grey. The right edge
//! carries the current artifact's name — buffer filename in Query
//! mode, dashboard name + uid in Dashboard mode — so the strip is
//! useful as a "you are here" hint, not just a mode toggle.
//!
//! Purely informational: no keybinds attach here. Switching modes is
//! still driven by `:dashboard <uid>` / `:edit` exactly as before.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, BufferMode};
use crate::axiom::DashboardSummaryExt;

/// Active-tab badge style — matches the bottom-bar `NORMAL` badge so
/// the chrome reads as one family.
fn active_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

/// Inactive-tab text style — dim grey, no background, so the eye is
/// drawn to the active tab.
fn inactive_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Separator between the two tabs. Kept as its own helper so tests
/// can assert the shape without hard-coding it twice.
fn separator() -> Span<'static> {
    Span::styled(" │ ", Style::default().fg(Color::DarkGray))
}

/// Build the left-hand tab spans for a given mode. Pure: no `App`
/// access, easy to unit-test. Returns three spans: query-tab,
/// separator, dashboard-tab.
pub(super) fn tab_spans(mode: BufferMode) -> Vec<Span<'static>> {
    let (query_style, dash_style) = match mode {
        BufferMode::Mpl => (active_style(), inactive_style()),
        BufferMode::Dashboard => (inactive_style(), active_style()),
    };
    vec![
        Span::styled(" QUERY ", query_style),
        separator(),
        Span::styled(" DASHBOARD ", dash_style),
    ]
}

/// Right-hand context string: which file we're editing, or which
/// dashboard is loaded. Empty when there's no meaningful name to
/// show (fresh untitled buffer, no dashboard).
pub(super) fn context_label(app: &App) -> String {
    match app.buffer_mode {
        BufferMode::Mpl => app
            .current_file
            .as_ref()
            .map(|p| mpl_label_for_path(p))
            .unwrap_or_default(),
        BufferMode::Dashboard => app
            .loaded_dashboard
            .as_ref()
            .map(|d| format!("{} ({})", d.name_or_unnamed(), d.uid))
            .unwrap_or_default(),
    }
}

/// Path → display label for the Query-mode right edge. Pulled out of
/// [`context_label`] so it can be unit-tested without constructing a
/// whole `App`. Returns the file name only; full paths would crowd
/// the 1-row strip on narrow terminals. If the path lacks a final
/// component (e.g. ends in `..` or is `/`), falls back to the full
/// path so the user still sees *something*.
pub(super) fn mpl_label_for_path(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

pub(super) fn draw_topbar(f: &mut Frame, app: &mut App, area: Rect) {
    let spans = tab_spans(app.buffer_mode);
    // Stash the tab geometry so `App::on_mouse` can route a topbar
    // click. Labels are ASCII plus the 1-wide `│` separator, so a
    // char count equals the display width here. The separator strip
    // counts toward the DASHBOARD tab — a harmless 3-cell overlap.
    let query_w = spans[0].content.chars().count() as u16;
    let sep_w = spans[1].content.chars().count() as u16;
    let dash_w = spans[2].content.chars().count() as u16;
    app.mouse_geom.topbar = area;
    app.mouse_geom.topbar_query_end_x = area.x.saturating_add(query_w);
    app.mouse_geom.topbar_dash_end_x = area.x.saturating_add(query_w + sep_w + dash_w);

    let left = Line::from(spans);
    f.render_widget(Paragraph::new(left), area);

    let right_text = context_label(app);
    if !right_text.is_empty() {
        let right = Line::from(Span::styled(right_text, Style::default().fg(Color::Gray)))
            .alignment(ratatui::layout::Alignment::Right);
        f.render_widget(Paragraph::new(right), area);
    }
}
