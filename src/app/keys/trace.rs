//! Trace-tree pane keymap.
//!
//! Owns keystrokes while `App.focus == Pane::TraceTree`. The
//! sibling [`App::handle_trace_detail_key`] owns the right-hand
//! detail pane.
//!
//! Step 24 additions (over the step 23 surface):
//!
//! * Fold verbs `h` / `l` / `zM` / `zR` / `zv`. Cursor identity
//!   survives collapse — when the user folds an ancestor of the
//!   selected row, the cursor snaps up to the deepest still-
//!   visible ancestor instead of vanishing.
//! * `/` enters a vim-style filter input mode; characters narrow
//!   the visible tree (matches plus their ancestors stay), and
//!   the match set refines incrementally — appending a character
//!   re-scans only the prior match set instead of the full span
//!   list.
//! * `gt` / `gT` jump to the next / previous span whose
//!   `service.name` differs from the currently-selected one.
//! * `y` yanks the selected span as pretty-printed JSON onto the
//!   shared `App.yank` register (charwise) — `p` in the editor
//!   pastes it.
//!
//! Bindings — vim-shaped:
//!
//! * `j` / `k`             — cursor ± 1 visible row
//! * `gg` / `G`            — first / last visible row
//! * `Ctrl-D` / `Ctrl-U`   — half-page step (preferred)
//! * `h` / `l`             — collapse / expand subtree under cursor
//! * `zM` / `zR` / `zv`    — collapse all / expand all / reveal cursor
//! * `/`                   — filter prompt (substring, case-insensitive)
//! * `gt` / `gT`           — next / previous different service
//! * `y`                   — yank selected span as JSON
//! * `Tab`                 — swap focus to the detail pane
//! * `Esc`                 — exit trace, restore previous view
//! * `:`                   — enter the cmdline
//! * `?`                   — open help modal

use super::*;
use crate::app::ViewMode;
use crate::app::types::TraceInputMode;
use crate::trace::{SpanJson, build_search_blob, deepest_visible_ancestor, span_matches_query};

/// Upper bound on the accumulated motion count. A trace never has
/// anywhere near this many spans; the cap just stops a wedged key
/// from overflowing `usize` on multiplication.
const MAX_TRACE_COUNT: usize = 1_000_000;

impl App {
    pub(super) fn handle_trace_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;

        // Defensive: no trace view → exit politely. The
        // dispatcher in `keys/mod.rs` already gates focus on
        // `trace_view.is_some()`, so this is belt-and-braces.
        if self.trace_view.is_none() {
            self.exit_trace_view();
            return;
        }

        // Route to the filter-input sub-handler when active. The
        // sub-handler owns Esc / Enter / Backspace too so it must
        // run *before* the Normal-mode key matcher.
        let in_filter = self
            .trace_view
            .as_ref()
            .map(|v| v.input_mode == TraceInputMode::Filter)
            .unwrap_or(false);
        if in_filter {
            self.handle_trace_filter_key(key);
            return;
        }

        // ---- count prefix (vim `10j`) -------------------------
        // Digit keys accumulate a numeric count; the next motion
        // consumes it. A leading `0` isn't a motion in the tree
        // (no "line 0"), so it's ignored unless it extends an
        // existing count (`10`). A digit also breaks any pending
        // `g` / `z` sequence.
        if let (Char(c), M::NONE) = (key.code, key.modifiers)
            && c.is_ascii_digit()
        {
            let d = (c as u8 - b'0') as usize;
            let has_count = self
                .trace_view
                .as_ref()
                .is_some_and(|v| v.pending_count.is_some());
            if d == 0 && !has_count {
                return;
            }
            self.table_pending_g = false;
            if let Some(v) = self.trace_view.as_mut() {
                let cur = v.pending_count.unwrap_or(0);
                v.pending_count = Some((cur.saturating_mul(10) + d).min(MAX_TRACE_COUNT));
                v.pending_z = false;
            }
            return;
        }

        // Snapshot + consume the count for this (non-digit) key.
        // The only arm that must keep it across keystrokes is the
        // `g`-latch setter (so `10gg` works), which restores it.
        let explicit_count = self.trace_view.as_ref().and_then(|v| v.pending_count);
        if let Some(v) = self.trace_view.as_mut() {
            v.pending_count = None;
        }
        let count = explicit_count.unwrap_or(1);

        // `gg` / `gt` / `gT` two-step. Any non-`g`-prefixed key
        // clears the latch the same way the legend / table panes do.
        let was_pending_g = self.table_pending_g;
        self.table_pending_g = false;
        // `z` two-step for `zM` / `zR` / `zv`. Stored on the view
        // (not on `App`) because it's trace-mode state.
        let was_pending_z = self.trace_view.as_ref().is_some_and(|v| v.pending_z);
        if let Some(v) = self.trace_view.as_mut() {
            v.pending_z = false;
        }

        let count_i = count.min(i32::MAX as usize) as i32;
        match (key.code, key.modifiers) {
            (Esc, _) => self.exit_trace_view(),
            (Char(':'), _) => self.prefill_command(""),
            (Char('?'), _) => self.open_help(),
            (Char('j'), M::NONE) | (Down, _) => self.move_trace_cursor(count_i),
            (Char('k'), M::NONE) | (Up, _) => self.move_trace_cursor(-count_i),
            // `{n}gg` jumps to visible line `n` (1-indexed); bare
            // `gg` to the first row.
            (Char('g'), M::NONE) if was_pending_g => match explicit_count {
                Some(n) => self.set_trace_cursor_line(n),
                None => self.set_trace_cursor_first_visible(),
            },
            (Char('g'), M::NONE) => {
                self.table_pending_g = true;
                // Preserve the count so `10gg` reaches line 10.
                if let Some(v) = self.trace_view.as_mut() {
                    v.pending_count = explicit_count;
                }
            }
            (Char('t'), M::NONE) if was_pending_g => self.jump_service(1),
            (Char('T'), _) if was_pending_g => self.jump_service(-1),
            // `{n}G` jumps to visible line `n`; bare `G` to the last.
            (Char('G'), _) => match explicit_count {
                Some(n) => self.set_trace_cursor_line(n),
                None => self.set_trace_cursor_last_visible(),
            },
            (Char('d'), M::CONTROL) => {
                let step = (self.trace_visible_height() as i32 / 2).max(1);
                self.move_trace_cursor(step.saturating_mul(count_i));
            }
            (Char('u'), M::CONTROL) => {
                let step = (self.trace_visible_height() as i32 / 2).max(1);
                self.move_trace_cursor(-step.saturating_mul(count_i));
            }

            // ---- Folds ----
            (Char('h'), M::NONE) | (Left, _) => self.fold_collapse_cursor(),
            (Char('l'), M::NONE) | (Right, _) => self.fold_expand_cursor(),
            (Char('z'), M::NONE) => {
                if let Some(v) = self.trace_view.as_mut() {
                    v.pending_z = true;
                }
            }
            (Char('M'), _) if was_pending_z => self.fold_collapse_all(),
            (Char('R'), _) if was_pending_z => self.fold_expand_all(),
            (Char('v'), M::NONE) if was_pending_z => self.fold_reveal_cursor(),

            // ---- Filter ----
            (Char('/'), _) => self.filter_enter(),

            // ---- Yank ----
            (Char('y'), M::NONE) => self.yank_selected_span(),

            (Tab, _) => self.set_focus(Pane::TraceDetail),
            _ => {}
        }
    }

    /// Filter-input sub-handler. The trace pane is in this mode
    /// whenever `TraceView.input_mode == Filter`. Every printable
    /// char appends to `filter`; `Backspace` shortens it; `Enter`
    /// commits (exits input mode, filter stays active); `Esc`
    /// cancels (clears the filter wholesale).
    fn handle_trace_filter_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        match key.code {
            Esc => self.filter_cancel(),
            Enter => self.filter_commit(),
            Backspace => self.filter_backspace(),
            Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => self.filter_push_char(c),
            _ => {}
        }
    }

    /// Bindings while `Pane::TraceDetail` has focus. Unchanged
    /// since step 23.
    pub(super) fn handle_trace_detail_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        let was_pending_g = self.table_pending_g;
        self.table_pending_g = false;

        if self.trace_view.is_none() {
            self.exit_trace_view();
            return;
        }
        let detail_h = self.last_trace_detail_height.max(1);
        let half = (detail_h as i32 / 2).max(1);
        match (key.code, key.modifiers) {
            (Esc, _) => self.exit_trace_view(),
            (Char(':'), _) => self.prefill_command(""),
            (Char('?'), _) => self.open_help(),
            (Tab, _) => self.set_focus(Pane::TraceTree),
            (Char('j'), M::NONE) | (Down, _) => self.scroll_trace_detail(1),
            (Char('k'), M::NONE) | (Up, _) => self.scroll_trace_detail(-1),
            (Char('g'), M::NONE) if was_pending_g => {
                if let Some(v) = self.trace_view.as_mut() {
                    v.detail_scroll = 0;
                }
            }
            (Char('g'), M::NONE) => self.table_pending_g = true,
            (Char('G'), _) => {
                if let Some(v) = self.trace_view.as_mut() {
                    v.detail_scroll = u16::MAX;
                }
            }
            (Char('d'), M::CONTROL) => self.scroll_trace_detail(half),
            (Char('u'), M::CONTROL) => self.scroll_trace_detail(-half),
            _ => {}
        }
    }

    fn scroll_trace_detail(&mut self, delta: i32) {
        if let Some(v) = self.trace_view.as_mut() {
            let next = (v.detail_scroll as i32 + delta).max(0).min(u16::MAX as i32) as u16;
            v.detail_scroll = next;
        }
    }

    /// Exit the trace view and restore the previously-active
    /// `ViewMode`. Same shape as step 23; nothing new for step 24.
    pub(crate) fn exit_trace_view(&mut self) {
        let return_mode = self
            .trace_view
            .as_ref()
            .map(|v| v.return_mode)
            .unwrap_or(ViewMode::Solo);
        self.trace_view = None;
        if self.pending_trace_fetch.is_some() {
            self.pending_trace_fetch = None;
        }
        self.view_mode = return_mode;
        self.focus = if return_mode == ViewMode::Grid {
            Pane::Dashboard
        } else {
            Pane::Editor
        };
        self.status = "trace closed".to_string();
    }

    // ============================================================
    //                       Cursor motion
    // ============================================================
    //
    // Every helper below assumes `self.trace_view` is `Some`. The
    // top-level `handle_trace_key` guard enforces that, so the
    // helpers use `expect` rather than defensive early-out so
    // coverage doesn't carry dead branches.

    /// Move the cursor by `delta` visible rows, clamped to the
    /// visible window. Operates in "visible-row index" space and
    /// then translates back to the tree-row index `view.cursor`
    /// actually stores.
    fn move_trace_cursor(&mut self, delta: i32) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let visible = view.visible_rows();
        if visible.is_empty() {
            return;
        }
        let cursor_tree = view.cursor.min(view.model.tree.len().saturating_sub(1));
        let current_vis = visible.iter().position(|&i| i == cursor_tree).unwrap_or(0);
        let next_vis = (current_vis as i32 + delta).clamp(0, visible.len() as i32 - 1) as usize;
        let next_tree = visible[next_vis];

        let visible_h = self.trace_visible_height() as usize;
        let view = self.trace_view.as_mut().expect("guarded by caller");
        view.cursor = next_tree;
        let scroll = view.scroll as usize;
        if next_vis < scroll {
            view.scroll = next_vis as u16;
        } else if visible_h > 0 && next_vis >= scroll + visible_h {
            view.scroll = (next_vis + 1).saturating_sub(visible_h) as u16;
        }
    }

    fn set_trace_cursor_first_visible(&mut self) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let visible = view.visible_rows();
        let Some(&first) = visible.first() else {
            return;
        };
        let view = self.trace_view.as_mut().expect("guarded by caller");
        view.cursor = first;
        view.scroll = 0;
    }

    fn set_trace_cursor_last_visible(&mut self) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let visible = view.visible_rows();
        let Some(&last) = visible.last() else {
            return;
        };
        let last_vis = visible.len() - 1;
        let visible_h = self.trace_visible_height() as usize;
        let view = self.trace_view.as_mut().expect("guarded by caller");
        view.cursor = last;
        view.scroll = if visible_h > 0 {
            (last_vis + 1).saturating_sub(visible_h) as u16
        } else {
            0
        };
    }

    /// Jump to 1-indexed visible line `line` (vim `{n}G` / `{n}gg`),
    /// clamped to the last visible row. Operates in visible-row
    /// space so folds / filters don't throw the numbering off.
    fn set_trace_cursor_line(&mut self, line: usize) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let visible = view.visible_rows();
        if visible.is_empty() {
            return;
        }
        let vis_idx = line.saturating_sub(1).min(visible.len() - 1);
        let tree = visible[vis_idx];
        let visible_h = self.trace_visible_height() as usize;
        let view = self.trace_view.as_mut().expect("guarded by caller");
        view.cursor = tree;
        let scroll = view.scroll as usize;
        if vis_idx < scroll {
            view.scroll = vis_idx as u16;
        } else if visible_h > 0 && vis_idx >= scroll + visible_h {
            view.scroll = (vis_idx + 1).saturating_sub(visible_h) as u16;
        }
    }

    // ============================================================
    //                          Folds
    // ============================================================

    /// `h`: collapse the parent rooted at the cursor's span. If
    /// the cursor is on a leaf the keystroke is a no-op. Folding
    /// an ancestor of the current cursor isn't possible from this
    /// path (the cursor *is* the parent being folded), but
    /// `zM` reaches the case — see [`Self::fold_collapse_all`].
    fn fold_collapse_cursor(&mut self) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let cursor_tree = view.cursor.min(view.model.tree.len().saturating_sub(1));
        let row = view.model.tree[cursor_tree];
        if !row.has_children {
            return;
        }
        let span_idx = row.span_idx;
        let view = self.trace_view.as_mut().expect("guarded by caller");
        view.collapsed.insert(span_idx);
        // Cursor stays on the same row (which is still visible —
        // collapsing affects descendants, not the parent itself).
    }

    /// `l`: expand the parent rooted at the cursor's span. No-op
    /// on leaves and already-expanded parents.
    fn fold_expand_cursor(&mut self) {
        let view = self.trace_view.as_mut().expect("guarded by caller");
        let cursor_tree = view.cursor.min(view.model.tree.len().saturating_sub(1));
        let span_idx = view.model.tree[cursor_tree].span_idx;
        view.collapsed.remove(&span_idx);
    }

    /// `zM`: collapse every parent in the trace. Cursor snaps to
    /// the deepest still-visible ancestor of its current row so
    /// it doesn't end up stranded inside a folded subtree.
    fn fold_collapse_all(&mut self) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let cursor_tree = view.cursor.min(view.model.tree.len().saturating_sub(1));
        let cursor_span = view.model.tree[cursor_tree].span_idx;
        let all_parents: std::collections::HashSet<usize> = view
            .model
            .tree
            .iter()
            .filter(|r| r.has_children)
            .map(|r| r.span_idx)
            .collect();
        // Walk in the future-collapsed world to pick the snap target.
        let snap_span = deepest_visible_ancestor(&view.model, &all_parents, cursor_span);
        let snap_tree = view
            .model
            .tree
            .iter()
            .position(|r| r.span_idx == snap_span)
            .unwrap_or(0);

        let view = self.trace_view.as_mut().expect("checked above");
        view.collapsed = all_parents;
        view.cursor = snap_tree;
        view.scroll = 0;
    }

    /// `zR`: expand everything. Cursor stays put.
    fn fold_expand_all(&mut self) {
        let view = self.trace_view.as_mut().expect("guarded by caller");
        view.collapsed.clear();
    }

    /// `zv`: open just enough folds to reveal the cursor row.
    /// Walk the cursor's ancestor chain via `parent_span_id` and
    /// drop every collapsed parent from the set.
    fn fold_reveal_cursor(&mut self) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let cursor_tree = view.cursor.min(view.model.tree.len().saturating_sub(1));
        let cursor_span = view.model.tree[cursor_tree].span_idx;
        // Collect ancestor span_idxs first (immutable borrow on model).
        let mut to_uncollapse: Vec<usize> = Vec::new();
        let mut cur = cursor_span;
        while let Some(parent_id) = view.model.spans[cur].parent_span_id.as_deref() {
            let Some(&parent_idx) = view.model.by_id.get(parent_id) else {
                break;
            };
            to_uncollapse.push(parent_idx);
            cur = parent_idx;
        }
        let view = self.trace_view.as_mut().expect("checked above");
        for idx in to_uncollapse {
            view.collapsed.remove(&idx);
        }
    }

    // ============================================================
    //                          Filter
    // ============================================================

    /// `/`: enter filter input mode. Lazily builds the per-span
    /// search blobs on first use so a trace that's never filtered
    /// never pays the build cost. Pre-existing filter content
    /// stays so the user can append to it.
    fn filter_enter(&mut self) {
        let view = self.trace_view.as_mut().expect("guarded by caller");
        if view.search_blobs.is_none() {
            let blobs: Vec<String> = view.model.spans.iter().map(build_search_blob).collect();
            view.search_blobs = Some(blobs);
        }
        view.input_mode = TraceInputMode::Filter;
        // If the filter was already non-empty and we have no
        // cached match set, run one full scan so the prompt opens
        // with accurate live state.
        if !view.filter.is_empty() && view.filter_matches.is_none() {
            let q = view.filter.clone();
            self.refilter_full(&q);
        }
    }

    /// `Esc` while in filter input: clear the filter, drop the
    /// match set, return to Normal.
    fn filter_cancel(&mut self) {
        let view = self.trace_view.as_mut().expect("guarded by caller");
        view.filter.clear();
        view.filter_matches = None;
        view.input_mode = TraceInputMode::Normal;
        self.status = "filter cleared".to_string();
    }

    /// `Enter` while in filter input: commit. Cursor snaps to
    /// the first matching row so the user can immediately
    /// start navigating from a real match.
    fn filter_commit(&mut self) {
        let view = self.trace_view.as_mut().expect("guarded by caller");
        view.input_mode = TraceInputMode::Normal;
        let Some(matches) = view.filter_matches.as_ref() else {
            return;
        };
        if matches.is_empty() {
            self.status = "no matches".to_string();
            return;
        }
        // First tree row whose span_idx is in the match set.
        // `matches` is in span-index order, not DFS-tree order;
        // scanning `tree` picks the structurally earliest match.
        let match_set: std::collections::HashSet<usize> = matches.iter().copied().collect();
        let first_tree = view
            .model
            .tree
            .iter()
            .position(|r| match_set.contains(&r.span_idx));
        if let Some(idx) = first_tree {
            view.cursor = idx;
            view.scroll = 0;
        }
        self.status = format!("/{}: {} hit", view.filter, matches.len());
    }

    /// `Backspace` shortens the filter. Match set has to be
    /// rebuilt from the full span list because shrinking a
    /// substring query can re-admit spans the previous scan
    /// rejected.
    fn filter_backspace(&mut self) {
        let view = self.trace_view.as_mut().expect("guarded by caller");
        if view.filter.pop().is_none() {
            // Empty filter + Backspace = leave input mode.
            view.input_mode = TraceInputMode::Normal;
            view.filter_matches = None;
            return;
        }
        let q = view.filter.clone();
        if q.is_empty() {
            view.filter_matches = None;
            return;
        }
        self.refilter_full(&q);
    }

    /// Append a character to the filter (lowercased) and narrow
    /// the prior match set. Substring `str::contains` makes this
    /// a one-pass narrowing: any span that didn't match `prefix`
    /// can't match `prefix + c` either.
    fn filter_push_char(&mut self, c: char) {
        let view = self.trace_view.as_mut().expect("guarded by caller");
        for lc in c.to_lowercase() {
            view.filter.push(lc);
        }
        // Borrow the cached blobs / prior matches instead of cloning
        // the whole Vec<String> on every keystroke. The block ends all
        // immutable field borrows before we write `filter_matches`.
        let new_matches: Vec<usize> = {
            let q = &view.filter;
            let blobs = view.search_blobs.as_deref().unwrap_or(&[]);
            match &view.filter_matches {
                Some(prev) => prev
                    .iter()
                    .copied()
                    .filter(|&i| {
                        blobs
                            .get(i)
                            .map(|b| span_matches_query(b, q))
                            .unwrap_or(false)
                    })
                    .collect(),
                None => (0..blobs.len())
                    .filter(|&i| span_matches_query(&blobs[i], q))
                    .collect(),
            }
        };
        view.filter_matches = Some(new_matches);
    }

    /// Full rescan against every span. Used by Backspace and
    /// (defensively) by `filter_enter` if a stale filter is
    /// reopened without a cached match set.
    fn refilter_full(&mut self, q: &str) {
        let view = self.trace_view.as_mut().expect("guarded by caller");
        let matches: Vec<usize> = {
            let blobs = view.search_blobs.as_deref().unwrap_or(&[]);
            (0..blobs.len())
                .filter(|&i| span_matches_query(&blobs[i], q))
                .collect()
        };
        view.filter_matches = Some(matches);
    }

    // ============================================================
    //                       Service jumps
    // ============================================================

    /// `gt` / `gT`: jump to the next / previous visible row whose
    /// `service.name` differs from the currently-selected row's.
    /// Wraps; if every visible row is the same service (or there
    /// are no visible rows), the cursor doesn't move and the
    /// status bar explains.
    fn jump_service(&mut self, dir: i32) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let visible = view.visible_rows();
        if visible.is_empty() {
            return;
        }
        let cursor_tree = view.cursor.min(view.model.tree.len().saturating_sub(1));
        let cur_vis = visible.iter().position(|&i| i == cursor_tree).unwrap_or(0);
        let current_service = view.model.spans[view.model.tree[cursor_tree].span_idx]
            .service
            .clone();

        // Walk the visible rows in `dir` order with wrap. Cap the
        // walk at visible.len() steps so a single-service trace
        // returns to the start instead of looping forever.
        let n = visible.len();
        let step: i32 = if dir >= 0 { 1 } else { -1 };
        let mut probe = cur_vis as i32;
        let mut found: Option<usize> = None;
        for _ in 0..n {
            probe += step;
            if probe < 0 {
                probe = n as i32 - 1;
            } else if probe >= n as i32 {
                probe = 0;
            }
            let tree_idx = visible[probe as usize];
            let span = &view.model.spans[view.model.tree[tree_idx].span_idx];
            if span.service != current_service {
                found = Some(probe as usize);
                break;
            }
        }

        match found {
            Some(next_vis) => {
                let next_tree = visible[next_vis];
                let visible_h = self.trace_visible_height() as usize;
                let view = self.trace_view.as_mut().expect("checked above");
                view.cursor = next_tree;
                let scroll = view.scroll as usize;
                if next_vis < scroll {
                    view.scroll = next_vis as u16;
                } else if visible_h > 0 && next_vis >= scroll + visible_h {
                    view.scroll = (next_vis + 1).saturating_sub(visible_h) as u16;
                }
            }
            None => {
                self.status = "single service".to_string();
            }
        }
    }

    // ============================================================
    //                          Yank
    // ============================================================

    /// `y`: serialize the selected span via [`SpanJson`] and drop
    /// it onto the shared editor yank register charwise. `p` in
    /// the editor splices the JSON at the cursor.
    fn yank_selected_span(&mut self) {
        let view = self.trace_view.as_ref().expect("guarded by caller");
        let cursor_tree = view.cursor.min(view.model.tree.len().saturating_sub(1));
        let span_idx = view.model.tree[cursor_tree].span_idx;
        let span = &view.model.spans[span_idx];
        // `SpanJson` is a plain typed projection over owned
        // strings + `BTreeMap<String, Json>` — `to_string_pretty`
        // can't error short of OOM, so `expect` is the honest
        // contract here.
        let json = serde_json::to_string_pretty(&SpanJson::from_span(&view.model.trace_id, span))
            .expect("SpanJson serialises");
        let span_id = span.span_id.clone();
        self.yank = Some(crate::app::types::YankEntry {
            text: json,
            linewise: false,
        });
        self.status = format!("yanked span {}", short_id(&span_id));
    }

    // ============================================================
    //                          Mouse
    // ============================================================

    /// Click in the tree body at body-relative `(dx, dy)`. Resolves
    /// the visible row under the pointer, focuses the tree, and moves
    /// the cursor there. A click inside the fold-marker band of a
    /// parent row (the 2-cell `▸`/`▾` glyph at `depth*2`) also toggles
    /// that subtree's collapse state — that's the click-to-fold
    /// affordance. The scroll origin comes from the renderer's stash.
    pub(super) fn mouse_select_trace_row(&mut self, dx: u16, dy: u16) {
        let Some(view) = self.trace_view.as_ref() else {
            return;
        };
        let visible = view.visible_rows();
        if visible.is_empty() {
            self.set_focus(Pane::TraceTree);
            return;
        }
        let vis_idx = self.mouse_geom.trace_tree_scroll + dy as usize;
        if vis_idx >= visible.len() {
            // Click on the empty band below the last row: focus only.
            self.set_focus(Pane::TraceTree);
            return;
        }
        let tree_idx = visible[vis_idx];
        let row = view.model.tree[tree_idx];
        let depth = row.depth as usize;
        let has_children = row.has_children;
        let span_idx = row.span_idx;
        // Fold-marker band: `tree_guides` lays each depth level in 2
        // display cells, and the marker glyph occupies the next 2 — so
        // the marker sits at `[depth*2, depth*2+2)` from the body edge.
        let in_fold_band =
            has_children && (dx as usize) >= depth * 2 && (dx as usize) < depth * 2 + 2;

        self.set_focus(Pane::TraceTree);
        if let Some(v) = self.trace_view.as_mut() {
            v.cursor = tree_idx;
            if in_fold_band {
                if v.collapsed.contains(&span_idx) {
                    v.collapsed.remove(&span_idx);
                } else {
                    v.collapsed.insert(span_idx);
                }
            }
        }
    }

    /// Wheel over the tree: the trace model derives `scroll` from the
    /// cursor (the renderer keeps the cursor visible), so a wheel notch
    /// steps the cursor rather than scrolling an independent viewport.
    /// No-op without a loaded trace — `move_trace_cursor` assumes a
    /// caller guard that the mouse path (unlike the keymap dispatcher)
    /// doesn't otherwise provide.
    pub(super) fn mouse_scroll_trace_tree(&mut self, delta: i32) {
        if self.trace_view.is_none() {
            return;
        }
        self.move_trace_cursor(delta);
    }

    /// Wheel over the detail pane: line-scroll its independent offset.
    pub(super) fn mouse_scroll_trace_detail_pane(&mut self, delta: i32) {
        self.scroll_trace_detail(delta);
    }

    // ============================================================
    //                          Helpers
    // ============================================================

    fn trace_visible_height(&self) -> u16 {
        let prompt = match self.trace_view.as_ref() {
            Some(v) if v.input_mode == TraceInputMode::Filter => 1,
            _ => 0,
        };
        self.last_trace_body_height.saturating_sub(prompt)
    }
}

/// 8-char abbreviation of a span_id for the status line. Mirrors
/// the trace-header `short_id` in `src/ui/trace.rs` (kept
/// separate to avoid pulling a UI helper into the keymap).
fn short_id(id: &str) -> String {
    if id.chars().count() <= 8 {
        id.to_string()
    } else {
        format!("{}\u{2026}", crate::util::take_chars(id, 8))
    }
}
