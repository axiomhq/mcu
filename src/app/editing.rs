//! Vim-style editing primitives — Normal-mode command execution,
//! visual-mode selection and operators, motion + operator + target
//! resolution, range yank/delete/paste/indent, and the `enter_insert_at`
//! transition into Insert mode.
//!
//! Nothing here knows about panes or async work; everything operates
//! on the `App.editor` `TextArea` plus the `cmd_parser` / `yank` /
//! `last_change` state on `App` itself.

use super::*;

impl App {
    /// Flat dispatcher for [`Command`]s produced by the Normal-mode parser.
    ///
    /// Adding a new Normal-mode feature should be a single arm here plus
    /// (sometimes) a helper in `motion.rs` once that exists. The parser is
    /// already wide enough to recognise `dw`, `ciw`, `da"`, `gu`, etc. —
    /// arms for those just need to be filled in.
    pub(super) fn run_command(&mut self, cmd: Command) {
        // Record buffer-mutating commands so `.` can replay them. Done
        // *before* dispatch so a recursive `.` doesn't overwrite itself.
        if Self::is_mutating(&cmd) {
            self.last_change = Some(cmd.clone());
        }
        match cmd {
            Command::Move { motion, count } => self.apply_motion(motion, count),
            Command::Apply { op, target, count } => self.apply_operator(op, target, count),
            Command::EnterInsert(at) => self.enter_insert_at(at),
            Command::EnterCommand => self.enter_command_mode(),
            Command::RunQuery => self.run_query(),
            Command::Undo if !self.editor.undo() => self.status = "nothing to undo".to_string(),
            Command::Undo => {}
            Command::Redo if !self.editor.redo() => self.status = "nothing to redo".to_string(),
            Command::Redo => {}
            Command::Quickfix => self.open_quickfix(),
            Command::Hover => {
                let text = self.query_text();
                let cursor = editor_cursor_byte_offset(&self.editor);
                match hover::resolve_function_at(&text, cursor) {
                    Some(info) => self.hover = Some(info),
                    None => self.status = "no docs for symbol under cursor".to_string(),
                }
            }
            Command::Help => self.open_help(),
            Command::FetchDatasets => self.fetch_datasets(),
            Command::FetchMetrics => self.fetch_metrics_for_current_query(),
            // Esc in Editor Normal mode: dismiss the error overlay if
            // present; else, if we arrived in Solo by zooming a tile,
            // return to the grid (vim's "back out" intuition for Esc).
            Command::DismissError if self.dismiss_error() => {
                self.status = "error dismissed".to_string()
            }
            Command::DismissError
                if self.view_mode == ViewMode::Solo && self.loaded_dashboard.is_some() =>
            {
                self.cmd_grid()
            }
            Command::DismissError => {}
            Command::DeleteCharUnder { count } => {
                for _ in 0..count {
                    self.editor.delete_next_char();
                }
            }
            Command::ReplaceChar { ch, count } => self.replace_chars(ch, count),
            Command::Paste { after, count } => self.paste(after, count),
            Command::RepeatFind { reverse, count } => self.repeat_find(reverse, count),
            Command::RepeatLastChange => self.repeat_last_change(),
            Command::EnterVisual { linewise } => self.enter_visual(linewise),
        }
    }

    /// Classify which commands count as a "change" for `.` replay. Pure
    /// cursor moves and discovery commands don't qualify.
    fn is_mutating(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::Apply { .. }
                | Command::Paste { .. }
                | Command::DeleteCharUnder { .. }
                | Command::ReplaceChar { .. }
                | Command::EnterInsert(_)
        )
    }

    /// Vim `Nrc`: replace `count` chars at/after the cursor with `ch`.
    /// If the line has fewer than `count` chars remaining, the command
    /// aborts (no partial edit) — same as vim. The cursor ends up on
    /// the last replaced char (`pos + count - 1`).
    fn replace_chars(&mut self, ch: char, count: usize) {
        if count == 0 {
            return;
        }
        let (row, col) = self.editor.cursor();
        let line_len = self
            .editor
            .lines()
            .get(row)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        // Newline replacement (`r<Enter>`) collapses `count` chars into
        // one line break, so the line-length check is unchanged.
        if col + count > line_len {
            self.status = format!("r: line has fewer than {count} char(s) remaining");
            return;
        }
        let replacement: String = std::iter::repeat_n(ch, count).collect();
        for _ in 0..count {
            self.editor.delete_next_char();
        }
        self.editor.insert_str(&replacement);
        // Vim leaves the cursor on the last replaced char (newline
        // replacement is the exception: cursor moves to the new line).
        if ch != '\n' {
            self.editor.move_cursor(tui_textarea::CursorMove::Back);
        }
    }

    fn repeat_find(&mut self, reverse: bool, count: usize) {
        let Some(memo) = self.last_find else {
            self.status = "no previous f/t to repeat".to_string();
            return;
        };
        let forward = if reverse { !memo.forward } else { memo.forward };
        let motion = Motion::FindChar {
            ch: memo.ch,
            forward,
            till: memo.till,
        };
        self.apply_motion(motion, count.max(1));
    }

    fn repeat_last_change(&mut self) {
        let Some(cmd) = self.last_change.clone() else {
            self.status = "no change to repeat".to_string();
            return;
        };
        // Don't re-store `.` itself as the last change.
        self.run_command(cmd);
    }

    fn enter_visual(&mut self, linewise: bool) {
        let cursor = editor_cursor_byte_offset(&self.editor);
        self.visual_anchor = Some(cursor);
        self.mode = if linewise {
            Mode::VisualLine
        } else {
            Mode::Visual
        };
    }

    /// Row range covered by the active Visual selection, for the UI to
    /// paint. `None` when not in Visual mode. Bool is `linewise`.
    pub fn visual_row_range(&self) -> Option<(usize, usize, bool)> {
        let range = self.visual_range()?;
        let buf = self.query_text();
        let (start_row, _) = byte_offset_to_row_col(&buf, range.start);
        let last = range.end.saturating_sub(1).min(buf.len());
        let (end_row, _) = byte_offset_to_row_col(&buf, last);
        Some((start_row, end_row, range.linewise))
    }

    /// Resolve the current Visual selection to a byte range, rounding to
    /// whole lines if [`Mode::VisualLine`].
    fn visual_range(&self) -> Option<Range> {
        let anchor = self.visual_anchor?;
        let cursor = editor_cursor_byte_offset(&self.editor);
        let (mut start, mut end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        // Visual selection is inclusive of the byte under the cursor.
        let buf = self.query_text();
        if end < buf.len() {
            end = motion::next_char_at(&buf, end);
        }
        let linewise = self.mode == Mode::VisualLine;
        if linewise {
            // Expand to full lines.
            let new_start = buf[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
            let new_end = buf[end.min(buf.len())..]
                .find('\n')
                .map(|p| end + p + 1)
                .unwrap_or(buf.len());
            start = new_start;
            end = new_end;
        }
        Some(Range {
            start,
            end,
            linewise,
        })
    }

    pub(super) fn exit_visual(&mut self) {
        self.mode = Mode::Normal;
        self.visual_anchor = None;
    }

    pub(super) fn apply_visual(&mut self, op: Operator) {
        let Some(range) = self.visual_range() else {
            self.exit_visual();
            return;
        };
        let buf = self.query_text();
        match op {
            Operator::Delete => self.delete_range(&buf, range),
            Operator::Yank => self.yank_range(&buf, range),
            Operator::Change => {
                self.delete_range(&buf, range);
                self.mode = Mode::Insert;
                self.visual_anchor = None;
                return;
            }
            Operator::IndentRight => self.indent_range(&buf, range, true),
            Operator::IndentLeft => self.indent_range(&buf, range, false),
        }
        self.exit_visual();
    }

    /// Translate a [`Motion`] into a `tui-textarea` cursor move and apply
    /// it `count` times. For motions that need byte-offset arithmetic
    /// (`FirstNonBlank`, `FindChar`) we compute the target directly.
    pub(super) fn apply_motion(&mut self, motion: Motion, count: usize) {
        match motion {
            Motion::FirstNonBlank => {
                let buf = self.query_text();
                let cursor = editor_cursor_byte_offset(&self.editor);
                let target = motion::first_non_blank(&buf, cursor);
                let (row, col) = byte_offset_to_row_col(&buf, target);
                self.editor
                    .move_cursor(CursorMove::Jump(row as u16, col as u16));
                return;
            }
            Motion::FindChar { ch, forward, till } => {
                let buf = self.query_text();
                let mut pos = editor_cursor_byte_offset(&self.editor);
                for _ in 0..count.max(1) {
                    let Some(next) = (if forward {
                        motion::find_char_forward(&buf, pos, ch)
                    } else {
                        motion::find_char_back(&buf, pos, ch)
                    }) else {
                        return;
                    };
                    pos = next;
                }
                let target = if till {
                    if forward {
                        motion::prev_char_at(&buf, pos)
                    } else {
                        motion::next_char_at(&buf, pos)
                    }
                } else {
                    pos
                };
                self.last_find = Some(FindMemo { ch, forward, till });
                let (row, col) = byte_offset_to_row_col(&buf, target);
                self.editor
                    .move_cursor(CursorMove::Jump(row as u16, col as u16));
                return;
            }
            _ => {}
        }
        let cm = match motion {
            Motion::Left => CursorMove::Back,
            Motion::Right => CursorMove::Forward,
            Motion::Up => CursorMove::Up,
            Motion::Down => CursorMove::Down,
            Motion::WordForward => CursorMove::WordForward,
            Motion::WordBack => CursorMove::WordBack,
            Motion::WordEnd => CursorMove::WordEnd,
            Motion::LineStart => CursorMove::Head,
            Motion::LineEnd => CursorMove::End,
            Motion::FileStart => CursorMove::Top,
            Motion::FileEnd => CursorMove::Bottom,
            Motion::FirstNonBlank | Motion::FindChar { .. } | Motion::CurrentLine => return,
        };
        for _ in 0..count {
            self.editor.move_cursor(cm);
        }
    }

    /// Resolve a [`Target`] to a byte range and apply `op` to it.
    fn apply_operator(&mut self, op: Operator, target: Target, count: usize) {
        let buf = self.query_text();
        let cursor = editor_cursor_byte_offset(&self.editor);
        let Some(range) = self.resolve_target(&buf, cursor, target, count, op) else {
            return;
        };
        match op {
            Operator::Delete => self.delete_range(&buf, range),
            Operator::Yank => self.yank_range(&buf, range),
            Operator::Change => {
                self.delete_range(&buf, range);
                self.mode = Mode::Insert;
            }
            Operator::IndentRight => self.indent_range(&buf, range, true),
            Operator::IndentLeft => self.indent_range(&buf, range, false),
        }
    }

    fn resolve_target(
        &self,
        buf: &str,
        cursor: usize,
        target: Target,
        count: usize,
        op: Operator,
    ) -> Option<Range> {
        match target {
            Target::Motion(m) => {
                motion::resolve_motion(buf, cursor, m, count, op == Operator::Change)
            }
            Target::Object(o) => motion::resolve_object(buf, cursor, o),
        }
    }

    fn enter_insert_at(&mut self, at: InsertAt) {
        match at {
            InsertAt::AtCursor => {}
            InsertAt::AfterCursor => self.editor.move_cursor(CursorMove::Forward),
            InsertAt::LineStart => self.editor.move_cursor(CursorMove::Head),
            InsertAt::LineEnd => self.editor.move_cursor(CursorMove::End),
            InsertAt::OpenBelow => {
                self.editor.move_cursor(CursorMove::End);
                self.editor.insert_str("\n");
            }
            InsertAt::OpenAbove => {
                self.editor.move_cursor(CursorMove::Head);
                self.editor.insert_str("\n");
                // `insert_str` left the cursor on the line below the new
                // blank line; step back up.
                self.editor.move_cursor(CursorMove::Up);
            }
        }
        self.mode = Mode::Insert;
    }

    /// Delete `range` from the buffer, populating the yank register with
    /// the deleted text so `p`/`P` can put it back (vim convention).
    fn delete_range(&mut self, buf: &str, range: Range) {
        if range.is_empty() {
            return;
        }
        self.yank = Some(YankEntry {
            text: range.slice(buf).to_string(),
            linewise: range.linewise,
        });
        let (row, col) = byte_offset_to_row_col(buf, range.start);
        self.editor
            .move_cursor(CursorMove::Jump(row as u16, col as u16));
        let char_count = range.slice(buf).chars().count();
        self.editor.delete_str(char_count);
    }

    fn yank_range(&mut self, buf: &str, range: Range) {
        if range.is_empty() {
            return;
        }
        self.yank = Some(YankEntry {
            text: range.slice(buf).to_string(),
            linewise: range.linewise,
        });
    }

    fn paste(&mut self, after: bool, count: usize) {
        let Some(entry) = self.yank.clone() else {
            self.status = "nothing to paste".to_string();
            return;
        };
        let body: String = std::iter::repeat_n(entry.text.as_str(), count.max(1)).collect();
        if entry.linewise {
            let trimmed = body.trim_end_matches('\n');
            let new_lines = trimmed.matches('\n').count() + 1;
            if after {
                self.editor.move_cursor(CursorMove::End);
                self.editor.insert_str("\n");
                self.editor.insert_str(trimmed);
            } else {
                self.editor.move_cursor(CursorMove::Head);
                self.editor.insert_str(trimmed);
                self.editor.insert_str("\n");
                // After both insertions the cursor sits at the start of
                // the original line, which is now `new_lines` rows below
                // the pasted block. Step back up so the cursor lands on
                // the first pasted line, matching vim.
                for _ in 0..new_lines {
                    self.editor.move_cursor(CursorMove::Up);
                }
            }
            self.editor.move_cursor(CursorMove::Head);
        } else {
            if after {
                self.editor.move_cursor(CursorMove::Forward);
            }
            self.editor.insert_str(&body);
        }
    }

    /// Indent (or dedent) every line that the byte range touches.
    /// `right == true` adds [`INDENT`] at the line start; otherwise removes
    /// up to that many leading spaces (or one tab).
    fn indent_range(&mut self, buf: &str, range: Range, right: bool) {
        const INDENT: &str = "    ";
        let (first_row, _) = byte_offset_to_row_col(buf, range.start);
        let end_for_row = if range.end == range.start {
            range.end
        } else {
            range.end - 1
        };
        let (last_row, _) = byte_offset_to_row_col(buf, end_for_row);
        for row in first_row..=last_row {
            self.editor.move_cursor(CursorMove::Jump(row as u16, 0));
            if right {
                self.editor.insert_str(INDENT);
            } else {
                let lines = self.editor.lines();
                let Some(line) = lines.get(row) else { continue };
                let mut to_remove = 0usize;
                for c in line.chars().take(INDENT.len()) {
                    if c == '\t' {
                        to_remove = 1;
                        break;
                    } else if c == ' ' {
                        to_remove += 1;
                    } else {
                        break;
                    }
                }
                for _ in 0..to_remove {
                    self.editor.delete_next_char();
                }
            }
        }
    }
}
