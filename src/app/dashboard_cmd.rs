//! Tiny vim-flavoured command parser for the dashboard pane.
//!
//! The dashboard's verb surface is narrow — yank / cut / paste /
//! open / undo — so we don't reuse `crate::command::Parser`'s full
//! operator-pending grammar. Instead this parser only knows two
//! shapes:
//!
//! ```text
//! command := [count] verb
//! verb    := y | x | p | P | o | O | u
//! ```
//!
//! Counts accumulate decimally (`12y` → yank 12 tiles). Leading `0`
//! is *not* a count digit — it falls through as a verbless key so
//! existing direct bindings like `0` / `gg` keep working.
//!
//! Non-digit, non-verb keys are emitted as [`DashStep::Passthrough`]
//! after clearing the count, so the caller's idle keymap (navigation,
//! `m`/`s`/`d`/`a`, …) runs as before. Vim's `2h` semantics — "move
//! left twice" — are honoured by the caller: it sees `Passthrough(h)`
//! plus `count = 2` and repeats the action itself.
//!
//! All state is local to a single `App.dashboard_cmd`; the parser
//! never holds borrows into App.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A fully parsed dashboard command, ready for the host to dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashCommand {
    /// `y` (with count `n`) — snapshot `n` tiles starting at the
    /// focused one into the tile yank register.
    Yank { n: usize },
    /// `x` (with count `n`) — delete-and-yank `n` tiles. No confirm.
    Cut { n: usize },
    /// `p` / `P` — paste the yanked tile(s). `after = true` for `p`
    /// (below focused), `false` for `P` (above).
    Paste { after: bool, n: usize },
    /// `o` / `O` — open a new tile in a new row below (`above = false`,
    /// `o`) or above (`above = true`, `O`) the focused tile. The host
    /// drops into the existing `PickViz` overlay for the first one and
    /// reuses the selected kind for the remaining `n - 1`.
    Open { above: bool, n: usize },
    /// `u` — one-level dashboard undo.
    Undo,
}

/// Result of feeding one key event to the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashStep {
    /// Digit absorbed; the parser is waiting for the verb.
    Pending,
    /// A full command was parsed.
    Emit(DashCommand),
    /// The key is not part of any verb. The caller should run its
    /// normal idle handler. `count` is whatever count was pending
    /// when the key arrived (0 if none) — useful for repeating
    /// motions like `2h`.
    Passthrough { key: KeyEvent, count: usize },
}

#[derive(Debug, Default, Clone)]
pub struct DashboardParser {
    /// Decimal count accumulator. `0` means "no explicit count".
    count: usize,
}

impl DashboardParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test/debug accessor — current pending count.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.count
    }

    /// Reset to a clean state. Used when the host needs to cancel
    /// pending input (e.g. on focus change).
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.count = 0;
    }

    pub fn feed(&mut self, key: KeyEvent) -> DashStep {
        use KeyCode::*;
        use KeyModifiers as M;

        // Decimal count digits. `0` only counts when the count is
        // already started — otherwise it's the LineStart-style direct
        // key and we pass it through.
        if let (Char(d @ '0'..='9'), M::NONE) = (key.code, key.modifiers)
            && !(d == '0' && self.count == 0)
        {
            self.count = self
                .count
                .saturating_mul(10)
                .saturating_add((d as u8 - b'0') as usize);
            return DashStep::Pending;
        }

        // Verbs (require no modifier other than the implicit Shift
        // baked into the keycode for uppercase letters).
        let n = self.take_count();
        let allow_no_mod = key.modifiers == M::NONE || key.modifiers == M::SHIFT;
        if allow_no_mod {
            let cmd = match key.code {
                Char('y') => Some(DashCommand::Yank { n }),
                Char('x') => Some(DashCommand::Cut { n }),
                Char('p') => Some(DashCommand::Paste { after: true, n }),
                Char('P') => Some(DashCommand::Paste { after: false, n }),
                Char('o') => Some(DashCommand::Open { above: false, n }),
                Char('O') => Some(DashCommand::Open { above: true, n }),
                // `u` takes no count (vim's undo doesn't either).
                Char('u') if n == 1 => Some(DashCommand::Undo),
                _ => None,
            };
            if let Some(cmd) = cmd {
                return DashStep::Emit(cmd);
            }
        }

        // Non-verb key — propagate the count back to the caller so
        // motion keys can honour it. We've already taken `n` (with
        // the no-count → 1 default), so re-derive the raw count.
        let raw_count = if n == 1 { 0 } else { n };
        DashStep::Passthrough {
            key,
            count: raw_count,
        }
    }

    /// Drain the count accumulator, defaulting to 1.
    fn take_count(&mut self) -> usize {
        let c = self.count;
        self.count = 0;
        if c == 0 { 1 } else { c }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn shift(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn bare_verb_yields_count_1() {
        let mut p = DashboardParser::new();
        assert_eq!(p.feed(k('y')), DashStep::Emit(DashCommand::Yank { n: 1 }));
        assert_eq!(p.feed(k('x')), DashStep::Emit(DashCommand::Cut { n: 1 }));
        assert_eq!(
            p.feed(k('p')),
            DashStep::Emit(DashCommand::Paste { after: true, n: 1 })
        );
        assert_eq!(
            p.feed(shift('P')),
            DashStep::Emit(DashCommand::Paste { after: false, n: 1 })
        );
        assert_eq!(
            p.feed(k('o')),
            DashStep::Emit(DashCommand::Open { above: false, n: 1 })
        );
        assert_eq!(
            p.feed(shift('O')),
            DashStep::Emit(DashCommand::Open { above: true, n: 1 })
        );
        assert_eq!(p.feed(k('u')), DashStep::Emit(DashCommand::Undo));
    }

    #[test]
    fn single_digit_count_multiplies_verb() {
        let mut p = DashboardParser::new();
        assert_eq!(p.feed(k('3')), DashStep::Pending);
        assert_eq!(p.feed(k('y')), DashStep::Emit(DashCommand::Yank { n: 3 }));
    }

    #[test]
    fn multi_digit_count_accumulates_decimally() {
        let mut p = DashboardParser::new();
        assert_eq!(p.feed(k('1')), DashStep::Pending);
        assert_eq!(p.feed(k('2')), DashStep::Pending);
        assert_eq!(
            p.feed(k('p')),
            DashStep::Emit(DashCommand::Paste { after: true, n: 12 })
        );
    }

    #[test]
    fn leading_zero_passes_through() {
        let mut p = DashboardParser::new();
        let step = p.feed(k('0'));
        // `0` with empty count → passthrough so existing `gg`/`0` bindings work.
        match step {
            DashStep::Passthrough { key, count } => {
                assert_eq!(key.code, KeyCode::Char('0'));
                assert_eq!(count, 0);
            }
            other => panic!("expected Passthrough, got {other:?}"),
        }
    }

    #[test]
    fn zero_after_digit_extends_count() {
        let mut p = DashboardParser::new();
        assert_eq!(p.feed(k('1')), DashStep::Pending);
        assert_eq!(p.feed(k('0')), DashStep::Pending);
        assert_eq!(p.feed(k('y')), DashStep::Emit(DashCommand::Yank { n: 10 }));
    }

    #[test]
    fn count_then_non_verb_is_passthrough_with_count() {
        // `2h` — caller will see `Passthrough(h, count=2)` and repeat
        // the navigation twice.
        let mut p = DashboardParser::new();
        assert_eq!(p.feed(k('2')), DashStep::Pending);
        match p.feed(k('h')) {
            DashStep::Passthrough { key, count } => {
                assert_eq!(key.code, KeyCode::Char('h'));
                assert_eq!(count, 2);
            }
            other => panic!("expected Passthrough, got {other:?}"),
        }
        // Count is consumed.
        assert_eq!(p.count(), 0);
    }

    #[test]
    fn bare_non_verb_is_passthrough_with_zero_count() {
        let mut p = DashboardParser::new();
        match p.feed(k('h')) {
            DashStep::Passthrough { key, count } => {
                assert_eq!(key.code, KeyCode::Char('h'));
                assert_eq!(count, 0);
            }
            other => panic!("expected Passthrough, got {other:?}"),
        }
    }

    #[test]
    fn reset_clears_count() {
        let mut p = DashboardParser::new();
        assert_eq!(p.feed(k('5')), DashStep::Pending);
        p.reset();
        assert_eq!(p.count(), 0);
        // Next verb runs with default count 1.
        assert_eq!(p.feed(k('y')), DashStep::Emit(DashCommand::Yank { n: 1 }));
    }

    #[test]
    fn control_modifier_is_passthrough_not_verb() {
        // Ctrl-d (half-page down) must not be mistaken for the
        // ConfirmDelete flow or any new verb.
        let mut p = DashboardParser::new();
        match p.feed(ctrl('d')) {
            DashStep::Passthrough { key, count } => {
                assert_eq!(key.code, KeyCode::Char('d'));
                assert!(key.modifiers.contains(KeyModifiers::CONTROL));
                assert_eq!(count, 0);
            }
            other => panic!("expected Passthrough, got {other:?}"),
        }
    }

    #[test]
    fn count_with_u_is_passthrough_so_undo_stays_count_free() {
        // `3u` is a passthrough — vim's undo doesn't take a count,
        // and we prefer to surface the unexpected combo via the
        // idle keymap rather than silently dropping the count.
        let mut p = DashboardParser::new();
        assert_eq!(p.feed(k('3')), DashStep::Pending);
        match p.feed(k('u')) {
            DashStep::Passthrough { key, count } => {
                assert_eq!(key.code, KeyCode::Char('u'));
                assert_eq!(count, 3);
            }
            other => panic!("expected Passthrough, got {other:?}"),
        }
    }
}
