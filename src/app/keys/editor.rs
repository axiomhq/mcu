use super::*;

impl App {
    pub(super) fn handle_insert_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Completion popup intercepts a small set of keys.
        if self.completions.visible {
            match (key.code, key.modifiers) {
                (Esc, _) => return self.completions.hide(),
                (Tab, M::NONE) | (Enter, M::NONE) => return self.accept_completion(),
                (Up, _) | (Char('p'), M::CONTROL) => return self.move_completion_selection(-1),
                (Down, _) | (Char('n'), M::CONTROL) => return self.move_completion_selection(1),
                _ => {}
            }
        }

        // Trigger keys: Tab and Ctrl-Space.
        if matches!(
            (key.code, key.modifiers),
            (Tab, M::NONE) | (Char(' '), M::CONTROL)
        ) {
            return self.open_completions();
        }
        if key.code == Esc {
            self.mode = Mode::Normal;
            return;
        }
        if self.editor.input(key) {
            if self.completions.visible {
                self.refresh_completions();
            }
            self.recompute_diagnostics();
        }
    }

    pub(super) fn handle_normal_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Hover popup: any key other than `K` dismisses it.
        if self.hover.is_some() && !matches!((key.code, key.modifiers), (Char('K'), _)) {
            self.hover = None;
        }
        // The quick-fix picker takes over a small set of keys while visible.
        if self.quickfix.visible {
            match (key.code, key.modifiers) {
                (Esc, _) => return self.quickfix.hide(),
                (Enter, _) => return self.accept_quickfix(),
                (Up, _) | (Char('p'), M::CONTROL) => return self.move_quickfix_selection(-1),
                (Down, _) | (Char('n'), M::CONTROL) => return self.move_quickfix_selection(1),
                _ => return,
            }
        }
        match self.cmd_parser.feed(key) {
            Step::Pending | Step::Cancel => {}
            Step::Emit(cmd) => self.run_command(cmd),
        }
        // Any keystroke may have moved the cursor or edited the buffer;
        // refresh the signature-help line so the status bar follows.
        self.recompute_sig_help();
    }

    /// Visual-mode key handler. Motion keys go through the same parser
    /// (we only consume `Command::Move` emissions); operator keys collapse
    /// the current selection into a range and apply it.
    pub(super) fn handle_visual_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('v'), M::NONE) => return self.exit_visual(),
            (Char('V'), _) => {
                self.mode = Mode::VisualLine;
                return;
            }
            (Char(op), _) if matches!(op, 'd' | 'c' | 'y' | 'x' | '>' | '<') => {
                let operator = match op {
                    'd' | 'x' => Operator::Delete,
                    'c' => Operator::Change,
                    'y' => Operator::Yank,
                    '>' => Operator::IndentRight,
                    '<' => Operator::IndentLeft,
                    _ => unreachable!(),
                };
                return self.apply_visual(operator);
            }
            _ => {}
        }
        // Otherwise: feed the parser but only honour pure-motion emissions.
        // Operators / find-char / etc. are dropped — user can Esc back to Normal.
        if let Step::Emit(Command::Move { motion, count }) = self.cmd_parser.feed(key) {
            self.apply_motion(motion, count);
        }
        self.recompute_sig_help();
    }
}
