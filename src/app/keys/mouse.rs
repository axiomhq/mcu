//! Mouse event dispatch.
//!
//! [`App::on_mouse`] is the mouse counterpart to
//! [`super::super::App::on_key`]. It mirrors the same overlay-gating
//! discipline (an open modal swallows the event) and then routes by the
//! geometry the renderer stashed last frame in [`App::mouse_geom`].
//!
//! The mouse is purely additive: it never changes a keyboard binding,
//! and an unrecognized click / scroll is a no-op rather than a guess.
//! Hit-tests are gated by the current [`ViewMode`] so a rect left over
//! from a different view can't misfire.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::*;

/// How many rows a single wheel notch moves a scrollable pane.
const SCROLL_STEP: i32 = 3;

/// `true` when `(col, row)` lands inside `rect` (zero-area rects, the
/// pre-first-draw default, never match).
fn hit(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

impl App {
    pub fn on_mouse(&mut self, ev: MouseEvent) {
        // Overlays own the screen while visible — same precedence as
        // `on_key`. A click / scroll over a modal is consumed (and, for
        // the dismiss-on-any-key overlays, dismisses) rather than
        // bleeding through to the pane underneath.
        if self.mouse_blocked_by_overlay() {
            // An overlay consumed the click (and may have dismissed
            // itself); repaint so any dismissal is reflected under the
            // event-gated render loop.
            self.needs_redraw = true;
            return;
        }

        let (col, row) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_mouse_left_down(col, row),
            MouseEventKind::ScrollDown => self.on_mouse_scroll(col, row, SCROLL_STEP),
            MouseEventKind::ScrollUp => self.on_mouse_scroll(col, row, -SCROLL_STEP),
            // Drag / move / release / other buttons: ignored in v1.
            // Returning early avoids repainting on high-frequency
            // motion events the gated loop would otherwise redraw on.
            _ => return,
        }
        // A handled click/scroll may have mutated state; flag a repaint
        // up front the same way `on_key` does.
        self.needs_redraw = true;
    }

    /// Mirror of the overlay precedence in `on_key`. Returns `true`
    /// when a modal is up; the dismiss-on-any-key overlays are torn
    /// down here so a click dismisses them just like a keystroke.
    fn mouse_blocked_by_overlay(&mut self) -> bool {
        if self.dashboards.visible || self.time.picker.is_some() || self.help.visible {
            return true;
        }
        if self.dashinfo_visible {
            self.dashinfo_visible = false;
            return true;
        }
        if self.history_overlay_visible {
            self.history_overlay_visible = false;
            return true;
        }
        if self.tile_inspect_json.is_some() {
            self.tile_inspect_json = None;
            return true;
        }
        false
    }

    fn on_mouse_left_down(&mut self, col: u16, row: u16) {
        // Topbar tabs take precedence on their row.
        if hit(self.mouse_geom.topbar, col, row) {
            self.on_topbar_click(col);
            return;
        }
        // Trace view owns the whole body; nothing else is focusable.
        if self.view_mode == ViewMode::Trace {
            let body = self.mouse_geom.trace_tree_body;
            if hit(body, col, row) {
                // Select the span (and toggle its fold if the click
                // landed on the marker); `mouse_select_trace_row`
                // focuses the tree itself.
                self.mouse_select_trace_row(col - body.x, row - body.y);
            } else if hit(self.mouse_geom.trace_detail, col, row) {
                self.set_focus(Pane::TraceDetail);
            }
            return;
        }
        // Grid: a click on a tile selects + focuses it.
        if self.view_mode == ViewMode::Grid
            && let Some(idx) = self.grid_tile_at(col, row)
        {
            self.set_focused_chart(idx);
            self.set_focus(Pane::Dashboard);
            return;
        }
        // Editor: click inside the text area positions the cursor.
        if hit(self.mouse_geom.editor_inner, col, row) {
            self.set_focus(Pane::Editor);
            self.editor_click(col, row);
            return;
        }
        // Anything else: focus whichever pane was clicked.
        if let Some(pane) = self.pane_at(col, row) {
            self.set_focus(pane);
        }
    }

    /// First grid tile whose rect contains `(col, row)`.
    fn grid_tile_at(&self, col: u16, row: u16) -> Option<usize> {
        self.mouse_geom
            .grid_tiles
            .iter()
            .find(|(_, r)| hit(*r, col, row))
            .map(|(i, _)| *i)
    }

    /// Move the editor cursor to the clicked cell. Translation goes
    /// through [`editor_cell_to_buffer`], which clamps to a valid
    /// `(row, col)` (past-EOL / past-last-row clicks land on the
    /// nearest real position). Keeps the current Vim mode.
    fn editor_click(&mut self, col: u16, row: u16) {
        let inner = self.mouse_geom.editor_inner;
        let top = self.mouse_geom.editor_scroll_top;
        let (r, c) = editor_cell_to_buffer(inner, top, col, row, self.editor.lines());
        self.editor
            .move_cursor(CursorMove::Jump(r as u16, c as u16));
    }

    /// Resolve a Solo/Grid click to a focusable pane. Editor / legend /
    /// params are always candidates; the graph pane is focusable only
    /// when it is hosting a navigable table; the dashboard pane is
    /// present in Grid view. Border cells count (outer rects) so the
    /// 1-cell pane frame isn't a dead zone.
    fn pane_at(&self, col: u16, row: u16) -> Option<Pane> {
        let g = &self.mouse_geom;
        if hit(g.editor, col, row) {
            return Some(Pane::Editor);
        }
        if hit(g.legend, col, row) {
            return Some(Pane::Legend);
        }
        if hit(g.params, col, row) {
            return Some(Pane::Params);
        }
        if self.view_mode == ViewMode::Grid && hit(g.dashboard, col, row) {
            return Some(Pane::Dashboard);
        }
        if hit(g.graph, col, row) {
            // The solo graph pane is only focusable as a Table; for a
            // plain chart there's no pane to focus, so swallow the click
            // rather than emit `set_focus`'s "no table rows" status.
            let table_ok = self
                .table_result
                .as_ref()
                .is_some_and(|t| !t.rows.is_empty());
            return table_ok.then_some(Pane::Table);
        }
        None
    }

    /// Topbar tab click: `QUERY` returns to the single-tile editor view,
    /// `DASHBOARD` opens the loaded dashboard grid. The tab boundaries
    /// come from the x-ranges stashed by the renderer. `set_focus`-style
    /// validation lives in the underlying `cmd_*` helpers (they no-op
    /// with a status message when there's nothing to switch to).
    fn on_topbar_click(&mut self, col: u16) {
        let g = &self.mouse_geom;
        if col < g.topbar_query_end_x {
            self.cmd_solo();
        } else if col < g.topbar_dash_end_x {
            self.cmd_grid();
        }
    }

    /// Scroll-wheel routing: the pane *under the pointer* scrolls, not
    /// the focused pane (matches GUI/TUI convention). Over an
    /// unrecognized region the event is ignored.
    ///
    /// Grid and editor are deliberately excluded: both derive their
    /// scroll offset from the current selection / cursor (the renderer
    /// snaps the viewport to keep it visible), so a free wheel scroll
    /// would fight that snap-back. Trace tree + detail + the solo table
    /// have clean per-pane scroll semantics.
    fn on_mouse_scroll(&mut self, col: u16, row: u16, delta: i32) {
        if self.view_mode == ViewMode::Trace {
            if hit(self.mouse_geom.trace_tree_body, col, row) {
                self.mouse_scroll_trace_tree(delta);
            } else if hit(self.mouse_geom.trace_detail, col, row) {
                self.mouse_scroll_trace_detail_pane(delta);
            }
            return;
        }
        // Solo table pane: wheel moves the row selection.
        if hit(self.mouse_geom.graph, col, row)
            && let Some(len) = self
                .table_result
                .as_ref()
                .map(|t| t.rows.len())
                .filter(|&n| n > 0)
        {
            self.move_table_selection(delta, len);
        }
    }
}

/// Map a click cell to an editor buffer `(row, col)`, clamped to a
/// valid position. Pure so it can be reasoned about (and exercised
/// end-to-end via `on_mouse`) without a terminal. Assumes char-width
/// == display-width, matching the editor renderer's own assumption.
fn editor_cell_to_buffer(
    inner: Rect,
    top: usize,
    col: u16,
    row: u16,
    lines: &[String],
) -> (usize, usize) {
    let rel_row = row.saturating_sub(inner.y) as usize;
    let last_row = lines.len().saturating_sub(1);
    let buf_row = (top + rel_row).min(last_row);
    let rel_col = col.saturating_sub(inner.x) as usize;
    let line_len = lines.get(buf_row).map(|l| l.chars().count()).unwrap_or(0);
    (buf_row, rel_col.min(line_len))
}
