use super::*;

impl App {
    /// Drop into Command mode with `text` already on the line and the
    /// cursor at the end. Shared by the params pane's add/edit bindings.
    /// Remembers the current pane so the cmdline can return focus to it
    /// once the command is submitted or cancelled.
    pub(super) fn prefill_command(&mut self, text: &str) {
        self.cmdline.return_focus = Some(self.focus);
        self.cmdline.reset();
        self.cmdline.buf = text.to_string();
        self.cmdline.cursor = self.cmdline.buf.chars().count();
        self.mode = Mode::Command;
        self.status = String::new();
        // The cmdline lives at the bottom of the screen and consumes
        // keys through `handle_command_key` while `mode == Command`;
        // pane focus is irrelevant during that period. We drop to
        // Editor so any pane-specific key handlers stop firing.
        self.focus = Pane::Editor;
    }

    /// Restore pane focus after the command line closes. Used by both
    /// the Enter and Esc paths so cancelling a prefilled `:p` also
    /// brings the user back to the pane they came from.
    pub(super) fn restore_cmdline_focus(&mut self) {
        if let Some(pane) = self.cmdline.return_focus.take() {
            // `set_focus` enforces the same invariants as any other
            // focus change (e.g. won't focus Legend with no series).
            self.set_focus(pane);
        }
    }

    pub(super) fn handle_command_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Tab / Shift-Tab drive the completion popup. Every other key
        // (besides navigation/accept) hides it so successive insert +
        // tab cycles always start from a fresh candidate set.
        match (key.code, key.modifiers) {
            (Tab, _) => return self.handle_cmdline_tab(false),
            (BackTab, _) => return self.handle_cmdline_tab(true),
            (Up, _) | (Down, _) | (Enter, _) | (Esc, _) | (Char('c'), M::CONTROL) => {}
            _ => self.cmdline.completions.hide(),
        }
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('c'), M::CONTROL) => {
                self.cmdline.reset();
                self.cmdline.completions.hide();
                self.mode = Mode::Normal;
                self.restore_cmdline_focus();
            }
            (Up, _) if self.cmdline.completions.visible => self.move_cmdline_completion(-1),
            (Down, _) if self.cmdline.completions.visible => self.move_cmdline_completion(1),
            (Up, _) => self.cmdline_history_back(),
            (Down, _) => self.cmdline_history_forward(),
            (Enter, _) if self.cmdline.completions.visible => self.accept_cmdline_completion(),
            (Enter, _) => {
                let cmd = std::mem::take(&mut self.cmdline.buf);
                self.cmdline.cursor = 0;
                self.cmdline.reset_history_nav();
                self.mode = Mode::Normal;
                let trimmed = cmd.trim();
                if !trimmed.is_empty() {
                    self.history.push(trimmed);
                    // Best-effort persist; on failure we surface a
                    // single status-line note but keep running.
                    if let Err(e) = self.history.save() {
                        self.status = format!("history save failed: {e}");
                    }
                }
                self.execute_command(trimmed);
                self.restore_cmdline_focus();
            }
            // Everything from here on is an editing key; mutating
            // the buffer must invalidate the history-nav prefix so
            // the next Up captures a fresh one (vim semantics).
            // Empty cmdline + Backspace cancels, like vim.
            (Backspace, _) if self.cmdline.buf.is_empty() => self.mode = Mode::Normal,
            (Backspace, _) => {
                self.cmdline.reset_history_nav();
                self.cmdline.backspace();
            }
            (Delete, _) => {
                self.cmdline.reset_history_nav();
                self.cmdline.delete_forward();
            }
            (Left, _) => {
                self.cmdline.reset_history_nav();
                self.cmdline.move_left();
            }
            (Right, _) => {
                self.cmdline.reset_history_nav();
                self.cmdline.move_right();
            }
            (Home, _) | (Char('a'), M::CONTROL) => {
                self.cmdline.reset_history_nav();
                self.cmdline.move_home();
            }
            (End, _) | (Char('e'), M::CONTROL) => {
                self.cmdline.reset_history_nav();
                self.cmdline.move_end();
            }
            (Char('u'), M::CONTROL) => {
                self.cmdline.reset_history_nav();
                // Clear from cursor to start — readline standard.
                let to = self.cmdline.byte_cursor();
                self.cmdline.buf.drain(..to);
                self.cmdline.cursor = 0;
            }
            (Char('k'), M::CONTROL) => {
                self.cmdline.reset_history_nav();
                let from = self.cmdline.byte_cursor();
                self.cmdline.buf.truncate(from);
            }
            // Vim's cmdline Ctrl-W: delete the word before the cursor.
            (Char('w'), M::CONTROL) => {
                self.cmdline.reset_history_nav();
                self.cmdline.delete_word_backward();
            }
            (Char(c), m) if m == M::NONE || m == M::SHIFT => {
                self.cmdline.reset_history_nav();
                self.cmdline.insert_char(c);
            }
            _ => {}
        }
    }

    /// Vim's `c_<Up>`. First press captures the buffer up to the
    /// cursor as the filter prefix and stashes the live buffer.
    /// Each subsequent press jumps to the next-older matching entry.
    /// At the oldest match, the cursor doesn't move (silent no-op,
    /// matching vim).
    pub(super) fn cmdline_history_back(&mut self) {
        // Capture prefix + stash on first Up only — detected by
        // `history_cursor.is_none()` (we're still at the live
        // buffer position).
        if self.cmdline.history_cursor.is_none() {
            let cursor_byte = self.cmdline.byte_cursor();
            self.cmdline.history_prefix = self.cmdline.buf[..cursor_byte].to_string();
            self.cmdline.history_stash = Some((self.cmdline.buf.clone(), self.cmdline.cursor));
        }
        let prefix = self.cmdline.history_prefix.clone();
        if let Some(idx) = self.history.walk_back(self.cmdline.history_cursor, &prefix)
            && let Some(entry) = self.history.get(idx)
        {
            let entry = entry.to_string();
            self.cmdline.cursor = entry.chars().count();
            self.cmdline.buf = entry;
            self.cmdline.history_cursor = Some(idx);
        }
    }

    /// Vim's `c_<Down>`. Walks toward newer entries; past the
    /// most-recent match it restores the stashed live buffer
    /// (what the user was typing before they pressed Up) and
    /// clears the nav state.
    pub(super) fn cmdline_history_forward(&mut self) {
        // Nothing to do if we never started walking.
        if self.cmdline.history_cursor.is_none() {
            return;
        }
        let prefix = self.cmdline.history_prefix.clone();
        match self
            .history
            .walk_forward(self.cmdline.history_cursor, &prefix)
        {
            Some(idx) => {
                if let Some(entry) = self.history.get(idx) {
                    let entry = entry.to_string();
                    self.cmdline.cursor = entry.chars().count();
                    self.cmdline.buf = entry;
                    self.cmdline.history_cursor = Some(idx);
                }
            }
            None => {
                // Past the most-recent match — restore the live
                // buffer the user had typed before nav started.
                if let Some((buf, cursor)) = self.cmdline.history_stash.take() {
                    self.cmdline.buf = buf;
                    self.cmdline.cursor = cursor;
                }
                self.cmdline.history_cursor = None;
                self.cmdline.history_prefix.clear();
            }
        }
    }

    pub(in crate::app) fn enter_command_mode(&mut self) {
        self.cmdline.reset();
        self.mode = Mode::Command;
        self.status = String::new();
    }
}
