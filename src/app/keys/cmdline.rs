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
            (Enter, _) if self.cmdline.completions.visible => self.accept_cmdline_completion(),
            (Enter, _) => {
                let cmd = std::mem::take(&mut self.cmdline.buf);
                self.cmdline.cursor = 0;
                self.mode = Mode::Normal;
                self.execute_command(cmd.trim());
                self.restore_cmdline_focus();
            }
            // Empty cmdline + Backspace cancels, like vim.
            (Backspace, _) if self.cmdline.buf.is_empty() => self.mode = Mode::Normal,
            (Backspace, _) => self.cmdline.backspace(),
            (Delete, _) => self.cmdline.delete_forward(),
            (Left, _) => self.cmdline.move_left(),
            (Right, _) => self.cmdline.move_right(),
            (Home, _) | (Char('a'), M::CONTROL) => self.cmdline.move_home(),
            (End, _) | (Char('e'), M::CONTROL) => self.cmdline.move_end(),
            (Char('u'), M::CONTROL) => {
                // Clear from cursor to start — readline standard.
                let to = self.cmdline.byte_cursor();
                self.cmdline.buf.drain(..to);
                self.cmdline.cursor = 0;
            }
            (Char('k'), M::CONTROL) => {
                let from = self.cmdline.byte_cursor();
                self.cmdline.buf.truncate(from);
            }
            (Char(c), m) if m == M::NONE || m == M::SHIFT => self.cmdline.insert_char(c),
            _ => {}
        }
    }

    pub(in crate::app) fn enter_command_mode(&mut self) {
        self.cmdline.reset();
        self.mode = Mode::Command;
        self.status = String::new();
    }
}
