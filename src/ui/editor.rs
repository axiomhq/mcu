//! Editor pane: highlight, cursor, dirty marker, diagnostic count.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::Paragraph,
};

use super::pane_block;
use crate::app::{App, Mode};

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
pub(super) fn draw_editor(f: &mut Frame, app: &mut App, area: Rect) {
    let title = editor_title(app);
    let block = pane_block(&title, app.focus == crate::app::Pane::Editor);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Stash the editor's inner rect for click-to-position (step 27).
    // `editor_scroll_top` is stashed below once it's computed.
    app.mouse_geom.editor_inner = inner;
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
    app.mouse_geom.editor_scroll_top = top;

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
