//! Completion + quickfix popup state machines: opening on a keystroke,
//! refreshing as the user types, moving the cursor inside the popup,
//! and committing the picked item back into the editor / cmdline.

use super::*;

impl App {
    /// Drive the cmdline completion popup on Tab / Shift-Tab. First
    /// Tab from a hidden state: compute fuzzy candidates. A single
    /// match auto-completes + appends a space; multiple matches show
    /// the popup with the top-scored item spliced into the buffer.
    /// Subsequent Tabs cycle (Shift-Tab cycles backward) and splice
    /// the highlighted candidate over the current token in real time.
    pub fn handle_cmdline_tab(&mut self, backward: bool) {
        if self.cmdline.completions.visible {
            // Popup already visible: cycle.
            return self.move_cmdline_completion(if backward { -1 } else { 1 });
        }
        // Snapshot the discovery cache's dataset names and the
        // config file's deployment names so the completer can
        // populate the `:trace set dataset=` / `deployment=`
        // value slots. `Config::load` is cheap (single small
        // TOML read) and matches the same fresh-load pattern
        // already used by the fetch path; a load failure simply
        // collapses the deployment suggestion list to empty.
        let datasets = self.cache.read().dataset_names();
        let deployments: Vec<String> = self
            .resolve_config()
            .map(|cfg| cfg.deployments.keys().cloned().collect())
            .unwrap_or_default();
        let ctx = crate::cmdline_complete::Context {
            dashboards: &self.dashboards.items,
            datasets: &datasets,
            deployments: &deployments,
        };
        let req = match crate::cmdline_complete::completions_for(
            &self.cmdline.buf,
            self.cmdline.cursor,
            &ctx,
        ) {
            Some(r) if !r.items.is_empty() => r,
            _ => return,
        };
        // With fuzzy matching there's no meaningful shared prefix, so
        // splice the top-scored candidate directly. Single match: also
        // append a trailing space so the user can type the next arg.
        let top = req.items[0].clone();
        self.splice_cmdline_token(req.range, &top);
        if req.items.len() == 1 {
            self.cmdline.buf.push(' ');
            self.cmdline.cursor = self.cmdline.buf.chars().count();
            return;
        }
        // Multi: show popup; re-anchor splice range to the spliced text.
        let new_token_end = req.range.0 + top.len();
        self.cmdline.completions.items = req.items;
        self.cmdline.completions.selected = 0;
        self.cmdline.completions.replace_range = (req.range.0, new_token_end);
        self.cmdline.completions.visible = true;
    }

    pub(super) fn move_cmdline_completion(&mut self, delta: isize) {
        let n = self.cmdline.completions.items.len();
        if n == 0 {
            return;
        }
        let i = self.cmdline.completions.selected as isize + delta;
        let wrapped = ((i % n as isize) + n as isize) % n as isize;
        self.cmdline.completions.selected = wrapped as usize;
        // Splice the new selection into the buffer so the user sees
        // each candidate as they cycle (vim wildmenu style).
        let item = self.cmdline.completions.items[self.cmdline.completions.selected].clone();
        let range = self.cmdline.completions.replace_range;
        self.splice_cmdline_token(range, &item);
        // Re-anchor the range so the next cycle replaces the just-
        // spliced text instead of an older slice.
        self.cmdline.completions.replace_range = (range.0, range.0 + item.len());
    }

    pub(super) fn accept_cmdline_completion(&mut self) {
        // The current selection is already in the buffer (from the
        // last cycle); just hide the popup. Append a trailing space
        // to match the single-candidate path's affordance.
        if !self.cmdline.buf.ends_with(' ') {
            self.cmdline.buf.push(' ');
            self.cmdline.cursor = self.cmdline.buf.chars().count();
        }
        self.cmdline.completions.hide();
    }

    /// Replace `buf[range.0..range.1]` with `text` and reposition the
    /// char cursor at the end of the inserted text.
    fn splice_cmdline_token(&mut self, range: (usize, usize), text: &str) {
        let (start, end) = range;
        if start > self.cmdline.buf.len() || end > self.cmdline.buf.len() {
            return;
        }
        self.cmdline.buf.replace_range(start..end, text);
        let new_byte = start + text.len();
        // Convert byte position back to char count for `CmdLine.cursor`.
        self.cmdline.cursor = self.cmdline.buf[..new_byte].chars().count();
    }

    pub(super) fn open_completions(&mut self) {
        let Some(payload) = self.compute_completion_payload() else {
            self.completions.hide();
            self.status = "no completions".to_string();
            return;
        };
        if payload.items.is_empty() {
            self.completions.hide();
            self.maybe_kick_off_discovery(&payload.kind);
            return;
        }
        self.completions = state_from(payload, 0);
    }

    pub(super) fn refresh_completions(&mut self) {
        let previous_selected = self.completions.selected;
        let Some(payload) = self.compute_completion_payload() else {
            self.completions.hide();
            return;
        };
        if payload.items.is_empty() {
            self.completions.hide();
            return;
        }
        let selected = previous_selected.min(payload.items.len() - 1);
        self.completions = state_from(payload, selected);
    }

    fn compute_completion_payload(&self) -> Option<completions::CompletionPayload> {
        let query = self.query_text();
        let cursor_byte = editor_cursor_byte_offset(&self.editor);
        completions::compute(&query, cursor_byte, &self.params.system, &self.cache.read())
    }

    /// When a cache-backed context has nothing to offer, transparently kick off the
    /// fetch the user would otherwise have to invoke manually (`D` / `M`).
    pub(super) fn maybe_kick_off_discovery(&mut self, kind: &completions::CompletionKind) {
        if self.busy {
            self.status = "no completions".to_string();
            return;
        }
        match kind {
            completions::CompletionKind::Dataset if self.cache.read().dataset_count() == 0 => {
                self.status = "no datasets cached — fetching…".to_string();
                self.fetch_datasets();
            }
            completions::CompletionKind::Metric { dataset }
                if !dataset.is_empty() && self.cache.read().metric_names(dataset).is_empty() =>
            {
                self.status = format!("no metrics cached for `{dataset}` — fetching…");
                self.fetch_metrics_for_current_query();
            }
            _ => {
                self.status = "no completions".to_string();
            }
        }
    }

    /// Open the quick-fix picker for whichever diagnostic the editor cursor
    /// is sitting in. Falls back to the first diagnostic with any actions
    /// when the cursor isn't on one. No-op when nothing is fixable.
    pub(super) fn open_quickfix(&mut self) {
        let cursor_byte = editor_cursor_byte_offset(&self.editor);
        let target = self
            .diagnostics
            .iter()
            .find(|d| d.span_contains(cursor_byte) && !d.actions.is_empty())
            .or_else(|| self.diagnostics.iter().find(|d| !d.actions.is_empty()));
        let Some(diag) = target else {
            self.status = "no quick fix available".to_string();
            return;
        };
        self.quickfix = QuickFixPicker {
            visible: true,
            actions: diag.actions.clone(),
            selected: 0,
            title: diag.message.clone(),
        };
    }

    pub(super) fn move_quickfix_selection(&mut self, delta: isize) {
        if self.quickfix.actions.is_empty() {
            return;
        }
        let len = self.quickfix.actions.len();
        let i = self.quickfix.selected as isize + delta;
        self.quickfix.selected = crate::util::wrap_index(i, len);
    }

    pub(super) fn accept_quickfix(&mut self) {
        if !self.quickfix.visible {
            return;
        }
        let Some(action) = self.quickfix.actions.get(self.quickfix.selected).cloned() else {
            self.quickfix.hide();
            return;
        };
        self.splice_editor_range(
            (action.byte_offset, action.byte_offset + action.byte_length),
            &action.insert,
        );
        self.status = format!("applied: {}", action.name);
        self.quickfix.hide();
        self.recompute_diagnostics();
    }

    /// Replace the editor's text in the byte range `[start, end)` with
    /// `insert`, leaving the cursor at the end of the inserted text.
    /// Shared by quickfix and completion accepts.
    fn splice_editor_range(&mut self, range: (usize, usize), insert: &str) {
        let query = self.query_text();
        let (row, start_char) = byte_offset_to_row_col(&query, range.0);
        // Count the chars actually spanned by the byte range. Using
        // `end_char - start_char` is wrong when the range crosses a
        // line boundary: the two columns live on different rows, so
        // their difference isn't the number of chars between the
        // offsets. Newlines inside the range count as one char each
        // (which `delete_str` also treats as a single char).
        let replace_chars = query
            .get(range.0..range.1)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        self.editor
            .move_cursor(CursorMove::Jump(row as u16, start_char as u16));
        self.editor.delete_str(replace_chars);
        self.editor.insert_str(insert);
    }

    pub(super) fn move_completion_selection(&mut self, delta: isize) {
        if self.completions.items.is_empty() {
            return;
        }
        let len = self.completions.items.len();
        let i = self.completions.selected as isize + delta;
        self.completions.selected = crate::util::wrap_index(i, len);
    }

    pub(super) fn accept_completion(&mut self) {
        if !self.completions.visible {
            return;
        }
        let item = match self.completions.items.get(self.completions.selected) {
            Some(it) => it.clone(),
            None => {
                self.completions.hide();
                return;
            }
        };
        let Some(kind) = self.completions.kind.clone() else {
            self.completions.hide();
            return;
        };
        self.splice_editor_range(self.completions.replace_range_bytes, &item.apply);
        self.completions.hide();
        self.recompute_diagnostics();

        // When the user just picked a metric, kick off a background tag fetch
        // for the `(dataset, metric)` pair so the next `where`-position
        // completion can offer tag names. Cached pairs are skipped inside
        // `fetch_tags`.
        if let completions::CompletionKind::Metric { dataset } = &kind
            && !dataset.is_empty()
        {
            self.fetch_tags(dataset.clone(), item.label.clone());
        }

        // When the user just picked a tag name, prefetch its values so the
        // value popup has data the moment they type the comparison operator.
        if let completions::CompletionKind::Tag { dataset, metric } = &kind
            && !dataset.is_empty()
            && !metric.is_empty()
        {
            self.fetch_tag_values(dataset.clone(), metric.clone(), item.label.clone());
        }
    }
}
