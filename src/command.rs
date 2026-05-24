//! Vim-style Normal-mode command grammar.
//!
//! Vim's Normal mode is a tiny prefix grammar:
//!
//! ```text
//! command := [count] operator [count] (motion | text-object)
//!          | [count] motion                       # bare motion
//!          | [count] operator operator            # linewise (`dd`, `cc`, `yy`)
//!          | [count] g <key>                      # `g`-prefixed verbs
//!          | direct-key                           # `i`, `?`, `Enter`, ...
//! ```
//!
//! [`Parser`] is a tiny streaming state machine that consumes [`KeyEvent`]s
//! one at a time and produces a [`Command`] when a complete sequence has
//! been recognised. The host (see `App::run_command`) is a flat dispatcher
//! over the resulting [`Command`] enum — there is no per-keychord state on
//! `App` any more (`pending_d`, `pending_g` are gone).
//!
//! Coverage in this revision is deliberately a strict superset of the
//! bindings the host currently dispatches. The parser additionally
//! recognises `d`/`c`/`y` as operators and `i`/`a` text-object selectors
//! so that adding `dw`/`ci"`/`yy`/etc. later is a host-side change only —
//! no parser changes required.
//!
//! Counts compose multiplicatively: `2d3w` → `Apply{op:Delete,
//! target:Motion(WordForward), count:6}`. Doubled operators (`dd`, `cc`,
//! `yy`) collapse to `Apply{target: Motion(CurrentLine), …}` with the
//! count interpreted as a line count.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What an operator does once it has a [`Target`]. Extend with `Lower`,
/// `Upper`, etc. when those bindings get wired — the parser is already
/// shaped to recognise them as one-line additions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
    IndentRight,
    IndentLeft,
}

/// Cursor-motion verbs. Each resolves to either a new cursor position
/// (when used with [`Operator::Move`]) or a byte range (when used as the
/// target of a `d`/`c`/`y` operator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBack,
    WordEnd,
    LineStart,
    LineEnd,
    FirstNonBlank,
    FileStart,
    FileEnd,
    /// `f<ch>` (`forward: true, till: false`), `t<ch>` (`forward: true,
    /// till: true`), `F<ch>` and `T<ch>` mirror-images. Operates only on
    /// the current line.
    FindChar {
        ch: char,
        forward: bool,
        till: bool,
    },
    /// Synthetic linewise sentinel produced by doubled operators (`dd`,
    /// `cc`, `yy`). The host treats this as "the current line, including
    /// the trailing newline".
    CurrentLine,
}

/// Text-object selectors (`iw`/`aw`/`i"`/`a(`/…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObject {
    /// `iw` / `aw`.
    Word { around: bool },
    /// `i(` `a(` `i[` `a[` `i{` `a{` `i<` `a<` (and their closers, `ib`,
    /// `aB`). `open` is normalised to the opening bracket.
    Pair { open: char, around: bool },
    /// `i"` / `a"` / `i'` / `a'` / `` i` `` / `` a` ``.
    Quote { quote: char, around: bool },
}

/// What an operator acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Motion(Motion),
    Object(TextObject),
}

/// Where `i` / `a` / `I` / `A` / `o` / `O` drop the cursor before entering
/// Insert mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertAt {
    /// `i` — cursor stays put.
    AtCursor,
    /// `a` — cursor advances one char first.
    AfterCursor,
    /// `I` — jump to the first column of the line.
    LineStart,
    /// `A` — jump to the end of the line.
    LineEnd,
    /// `o` — open a new line below the cursor.
    OpenBelow,
    /// `O` — open a new line above the cursor.
    OpenAbove,
}

/// A fully-parsed Normal-mode command, ready for the host to dispatch.
//
// `enum_variant_names`: several variants legitimately end in `Command`
// (`EnterCommand`) or `Query` (`RunQuery`, `RefreshQuery`). They name
// concepts, not the enum.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Pure cursor move (no edit).
    Move {
        motion: Motion,
        count: usize,
    },
    /// Apply an operator. `target = Motion(CurrentLine)` is the linewise
    /// shortcut produced by doubled operators.
    Apply {
        op: Operator,
        target: Target,
        count: usize,
    },
    EnterInsert(InsertAt),
    EnterCommand,
    RunQuery,
    /// `r` (Normal, no modifier) — re-runs the current query. Deliberately
    /// shadows vim's `r<c>` (replace char) because refresh is far more
    /// useful in this app.
    RefreshQuery,
    Undo,
    Redo,
    /// `g a` — open the quick-fix picker.
    Quickfix,
    Hover,
    Help,
    Quit,
    /// `;` (`reverse: false`) — repeat the last `f`/`F`/`t`/`T` find on the
    /// current line. `,` (`reverse: true`) — same but with direction flipped.
    /// The host stores the last-find arguments; the parser is stateless on
    /// this point.
    RepeatFind {
        reverse: bool,
        count: usize,
    },
    /// `.` — re-apply the most recently dispatched buffer-mutating command.
    /// The host owns the memory of "what was last done"; the parser just
    /// emits the marker.
    RepeatLastChange,
    /// `v` / `V` — enter Visual mode. Linewise (`V`) selects whole lines.
    EnterVisual {
        linewise: bool,
    },
    FetchDatasets,
    FetchMetrics,
    DismissError,
    /// `x` — delete `count` chars under the cursor. Special-cased because
    /// `tui-textarea` has a dedicated API and there's no point routing it
    /// through the generic operator path.
    DeleteCharUnder {
        count: usize,
    },
    /// `p` (after = `true`) / `P` (after = `false`) — paste from the yank
    /// register. Linewise vs charwise is decided by what was yanked.
    Paste {
        after: bool,
        count: usize,
    },
}

/// Result of feeding one [`KeyEvent`] to a [`Parser`].
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Key absorbed; waiting for more input to form a complete command.
    Pending,
    /// Sequence was cancelled (Esc, unknown key in operator-pending state,
    /// …). Parser has been reset.
    Cancel,
    /// A complete command is ready to dispatch.
    Emit(Command),
}

#[derive(Default, Debug)]
pub struct Parser {
    /// Decimal count accumulator. `0` means "no explicit count".
    count: usize,
    /// `Some((op, n))` after an operator key. `n` is the count that was
    /// active when the operator was seen; it later multiplies with any
    /// count typed before the motion.
    pending_op: Option<(Operator, usize)>,
    /// `true` after `g` — the next key forms `gg`, `ga`, …
    g_prefix: bool,
    /// `Some(around)` after the operator + `i`/`a` — next key is the
    /// text-object selector. `around = true` means `a<obj>`.
    awaiting_object: Option<bool>,
    /// After `f`/`F`/`t`/`T`, the next char becomes the find target.
    awaiting_find: Option<FindArgs>,
}

#[derive(Debug, Clone, Copy)]
struct FindArgs {
    forward: bool,
    till: bool,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, key: KeyEvent) -> Step {
        // 0. Find-char target: any printable char becomes the operand;
        //    anything else cancels. Highest precedence so `f<Esc>` etc.
        //    cleans up cleanly.
        if let Some(args) = self.awaiting_find.take() {
            return match (key.code, key.modifiers) {
                (KeyCode::Char(c), _) => {
                    let motion = Motion::FindChar {
                        ch: c,
                        forward: args.forward,
                        till: args.till,
                    };
                    if let Some((op, op_count)) = self.pending_op.take() {
                        let mo_count = self.take_count();
                        let count = combine_counts(op_count, mo_count);
                        Step::Emit(Command::Apply {
                            op,
                            target: Target::Motion(motion),
                            count,
                        })
                    } else {
                        let count = self.take_count();
                        Step::Emit(Command::Move { motion, count })
                    }
                }
                _ => {
                    self.reset();
                    Step::Cancel
                }
            };
        }

        // 1. Text-object selector takes precedence (operator + i/a + sel).
        if let Some(around) = self.awaiting_object.take() {
            let Some((op, op_count)) = self.pending_op.take() else {
                self.reset();
                return Step::Cancel;
            };
            let sel_count = self.take_count();
            let count = combine_counts(op_count, sel_count);
            return match parse_text_object(key, around) {
                Some(obj) => Step::Emit(Command::Apply {
                    op,
                    target: Target::Object(obj),
                    count,
                }),
                None => {
                    self.reset();
                    Step::Cancel
                }
            };
        }

        // 2. `g` prefix.
        if self.g_prefix {
            self.g_prefix = false;
            return match (key.code, key.modifiers) {
                (KeyCode::Char('a'), KeyModifiers::NONE) => {
                    self.count = 0;
                    Step::Emit(Command::Quickfix)
                }
                (KeyCode::Char('g'), KeyModifiers::NONE) => {
                    let count = self.take_count();
                    Step::Emit(Command::Move {
                        motion: Motion::FileStart,
                        count,
                    })
                }
                _ => {
                    self.reset();
                    Step::Cancel
                }
            };
        }

        // 3. Digit count accumulation. `0` only counts when `count > 0` —
        //    otherwise it's the LineStart motion.
        if let (KeyCode::Char(d @ '0'..='9'), KeyModifiers::NONE) = (key.code, key.modifiers)
            && !(d == '0' && self.count == 0)
        {
            self.count = self
                .count
                .saturating_mul(10)
                .saturating_add((d as u8 - b'0') as usize);
            return Step::Pending;
        }

        // 4. In operator-pending state, the next key chooses a motion or
        //    text object (or the doubled-op linewise shortcut).
        if let Some((op, op_count)) = self.pending_op {
            // Doubled operator → linewise current line. Allow Shift since
            // `>` / `<` are shifted on most layouts.
            let mods_ok =
                key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT;
            if mods_ok && let KeyCode::Char(c) = key.code {
                if doubled_op_char(op) == Some(c) {
                    let n = self.take_count();
                    let count = combine_counts(op_count, n);
                    self.reset();
                    return Step::Emit(Command::Apply {
                        op,
                        target: Target::Motion(Motion::CurrentLine),
                        count,
                    });
                }
                // `i` / `a` opens a text-object selector.
                if c == 'i' || c == 'a' {
                    self.awaiting_object = Some(c == 'a');
                    return Step::Pending;
                }
            }
            if let Some(motion) = key_to_motion(key) {
                let mo_count = self.take_count();
                let count = combine_counts(op_count, mo_count);
                self.reset();
                return Step::Emit(Command::Apply {
                    op,
                    target: Target::Motion(motion),
                    count,
                });
            }
            // `f`/`F`/`t`/`T` open a char-find target.
            if let Some(args) = find_args_from_key(key) {
                self.awaiting_find = Some(args);
                return Step::Pending;
            }
            // Anything else cancels the pending operator (including Esc).
            self.reset();
            return Step::Cancel;
        }

        // 5. Idle state.

        // Operators.
        let op = match (key.code, key.modifiers) {
            (KeyCode::Char('d'), KeyModifiers::NONE) => Some(Operator::Delete),
            (KeyCode::Char('c'), KeyModifiers::NONE) => Some(Operator::Change),
            (KeyCode::Char('y'), KeyModifiers::NONE) => Some(Operator::Yank),
            (KeyCode::Char('>'), _) => Some(Operator::IndentRight),
            (KeyCode::Char('<'), _) => Some(Operator::IndentLeft),
            _ => None,
        };
        if let Some(op) = op {
            let c = if self.count == 0 { 1 } else { self.count };
            self.count = 0;
            self.pending_op = Some((op, c));
            return Step::Pending;
        }

        // `g` prefix.
        if matches!(
            (key.code, key.modifiers),
            (KeyCode::Char('g'), KeyModifiers::NONE)
        ) {
            self.g_prefix = true;
            return Step::Pending;
        }

        // `f`/`F`/`t`/`T` open a char-find target at the top level.
        if let Some(args) = find_args_from_key(key) {
            self.awaiting_find = Some(args);
            return Step::Pending;
        }

        // Motion at the top level → Move.
        if let Some(motion) = key_to_motion(key) {
            let count = self.take_count();
            return Step::Emit(Command::Move { motion, count });
        }

        // Direct commands.
        let cmd = match (key.code, key.modifiers) {
            (KeyCode::Char('i'), KeyModifiers::NONE) => Command::EnterInsert(InsertAt::AtCursor),
            (KeyCode::Char('a'), KeyModifiers::NONE) => Command::EnterInsert(InsertAt::AfterCursor),
            (KeyCode::Char('I'), _) => Command::EnterInsert(InsertAt::LineStart),
            (KeyCode::Char('A'), _) => Command::EnterInsert(InsertAt::LineEnd),
            (KeyCode::Char('o'), KeyModifiers::NONE) => Command::EnterInsert(InsertAt::OpenBelow),
            (KeyCode::Char('O'), _) => Command::EnterInsert(InsertAt::OpenAbove),
            (KeyCode::Char('G'), _) => {
                let count = self.take_count();
                return Step::Emit(Command::Move {
                    motion: Motion::FileEnd,
                    count,
                });
            }
            (KeyCode::Char('Y'), _) => {
                // `Y` is vim shorthand for `yy` (linewise yank current line).
                let count = self.take_count();
                return Step::Emit(Command::Apply {
                    op: Operator::Yank,
                    target: Target::Motion(Motion::CurrentLine),
                    count,
                });
            }
            (KeyCode::Char('p'), KeyModifiers::NONE) => {
                let count = self.take_count();
                return Step::Emit(Command::Paste { after: true, count });
            }
            (KeyCode::Char('P'), _) => {
                let count = self.take_count();
                return Step::Emit(Command::Paste {
                    after: false,
                    count,
                });
            }
            (KeyCode::Char(';'), KeyModifiers::NONE) => {
                let count = self.take_count();
                return Step::Emit(Command::RepeatFind {
                    reverse: false,
                    count,
                });
            }
            (KeyCode::Char(','), KeyModifiers::NONE) => {
                let count = self.take_count();
                return Step::Emit(Command::RepeatFind {
                    reverse: true,
                    count,
                });
            }
            (KeyCode::Char('.'), KeyModifiers::NONE) => {
                self.count = 0;
                Command::RepeatLastChange
            }
            (KeyCode::Char('v'), KeyModifiers::NONE) => {
                self.count = 0;
                Command::EnterVisual { linewise: false }
            }
            (KeyCode::Char('V'), _) => {
                self.count = 0;
                Command::EnterVisual { linewise: true }
            }
            (KeyCode::Char('x'), KeyModifiers::NONE) => {
                let count = if self.count == 0 { 1 } else { self.count };
                Command::DeleteCharUnder { count }
            }
            (KeyCode::Char('u'), KeyModifiers::NONE) => Command::Undo,
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => Command::Redo,
            (KeyCode::Char('r'), KeyModifiers::NONE) => Command::RefreshQuery,
            (KeyCode::Enter, KeyModifiers::NONE) => Command::RunQuery,
            (KeyCode::Char(':'), KeyModifiers::NONE) => Command::EnterCommand,
            (KeyCode::Char('?'), _) => Command::Help,
            (KeyCode::Char('K'), _) => Command::Hover,
            (KeyCode::Char('q'), KeyModifiers::NONE) => Command::Quit,
            (KeyCode::Char('D'), _) => Command::FetchDatasets,
            (KeyCode::Char('M'), _) => Command::FetchMetrics,
            (KeyCode::Esc, _) => Command::DismissError,
            _ => {
                self.count = 0;
                return Step::Cancel;
            }
        };
        self.count = 0;
        Step::Emit(cmd)
    }

    fn take_count(&mut self) -> usize {
        let c = self.count;
        self.count = 0;
        if c == 0 { 1 } else { c }
    }

    fn reset(&mut self) {
        self.count = 0;
        self.pending_op = None;
        self.g_prefix = false;
        self.awaiting_object = None;
        self.awaiting_find = None;
    }
}

fn find_args_from_key(key: KeyEvent) -> Option<FindArgs> {
    let no_ctrl = key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT;
    if !no_ctrl {
        return None;
    }
    match key.code {
        KeyCode::Char('f') => Some(FindArgs {
            forward: true,
            till: false,
        }),
        KeyCode::Char('t') => Some(FindArgs {
            forward: true,
            till: true,
        }),
        KeyCode::Char('F') => Some(FindArgs {
            forward: false,
            till: false,
        }),
        KeyCode::Char('T') => Some(FindArgs {
            forward: false,
            till: true,
        }),
        _ => None,
    }
}

fn combine_counts(a: usize, b: usize) -> usize {
    a.saturating_mul(b).max(1)
}

fn doubled_op_char(op: Operator) -> Option<char> {
    match op {
        Operator::Delete => Some('d'),
        Operator::Change => Some('c'),
        Operator::Yank => Some('y'),
        Operator::IndentRight => Some('>'),
        Operator::IndentLeft => Some('<'),
    }
}

fn key_to_motion(key: KeyEvent) -> Option<Motion> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('h'), KeyModifiers::NONE) | (KeyCode::Left, _) => Some(Motion::Left),
        (KeyCode::Char('l'), KeyModifiers::NONE) | (KeyCode::Right, _) => Some(Motion::Right),
        (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => Some(Motion::Down),
        (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => Some(Motion::Up),
        (KeyCode::Char('w'), KeyModifiers::NONE) => Some(Motion::WordForward),
        (KeyCode::Char('b'), KeyModifiers::NONE) => Some(Motion::WordBack),
        (KeyCode::Char('e'), KeyModifiers::NONE) => Some(Motion::WordEnd),
        (KeyCode::Char('0'), KeyModifiers::NONE) => Some(Motion::LineStart),
        (KeyCode::Char('$'), KeyModifiers::NONE) => Some(Motion::LineEnd),
        (KeyCode::Char('^'), _) => Some(Motion::FirstNonBlank),
        _ => None,
    }
}

fn parse_text_object(key: KeyEvent, around: bool) -> Option<TextObject> {
    if key.modifiers != KeyModifiers::NONE && key.modifiers != KeyModifiers::SHIFT {
        return None;
    }
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    Some(match c {
        'w' | 'W' => TextObject::Word { around },
        '"' | '\'' | '`' => TextObject::Quote { quote: c, around },
        '(' | ')' | 'b' => TextObject::Pair { open: '(', around },
        '[' | ']' => TextObject::Pair { open: '[', around },
        '{' | '}' | 'B' => TextObject::Pair { open: '{', around },
        '<' | '>' => TextObject::Pair { open: '<', around },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn shift(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    }
    fn code(kc: KeyCode) -> KeyEvent {
        KeyEvent::new(kc, KeyModifiers::NONE)
    }

    /// Feed a sequence of keys and assert the parser produces the given
    /// sequence of Steps. Trailing `Pending`s are tolerated only at the end.
    fn run(keys: &[KeyEvent]) -> Vec<Step> {
        let mut p = Parser::new();
        keys.iter().map(|k| p.feed(*k)).collect()
    }

    #[test]
    fn single_motion_emits_move() {
        assert_eq!(
            run(&[k('j')]),
            vec![Step::Emit(Command::Move {
                motion: Motion::Down,
                count: 1,
            })]
        );
    }

    #[test]
    fn arrow_keys_alias_motions() {
        assert_eq!(
            run(&[code(KeyCode::Left)]),
            vec![Step::Emit(Command::Move {
                motion: Motion::Left,
                count: 1,
            })]
        );
    }

    #[test]
    fn count_prefix_is_multiplied_into_motion() {
        assert_eq!(
            run(&[k('5'), k('j')]),
            vec![
                Step::Pending,
                Step::Emit(Command::Move {
                    motion: Motion::Down,
                    count: 5,
                })
            ]
        );
    }

    #[test]
    fn multi_digit_count() {
        assert_eq!(
            run(&[k('1'), k('2'), k('w')]),
            vec![
                Step::Pending,
                Step::Pending,
                Step::Emit(Command::Move {
                    motion: Motion::WordForward,
                    count: 12,
                })
            ]
        );
    }

    #[test]
    fn zero_with_no_count_is_line_start() {
        assert_eq!(
            run(&[k('0')]),
            vec![Step::Emit(Command::Move {
                motion: Motion::LineStart,
                count: 1,
            })]
        );
    }

    #[test]
    fn zero_after_digit_is_a_digit() {
        // `10j` → 10 lines down.
        assert_eq!(
            run(&[k('1'), k('0'), k('j')]),
            vec![
                Step::Pending,
                Step::Pending,
                Step::Emit(Command::Move {
                    motion: Motion::Down,
                    count: 10,
                })
            ]
        );
    }

    #[test]
    fn doubled_operator_emits_current_line() {
        assert_eq!(
            run(&[k('d'), k('d')]),
            vec![
                Step::Pending,
                Step::Emit(Command::Apply {
                    op: Operator::Delete,
                    target: Target::Motion(Motion::CurrentLine),
                    count: 1,
                })
            ]
        );
    }

    #[test]
    fn dd_with_count_passes_count_through() {
        assert_eq!(
            run(&[k('3'), k('d'), k('d')]),
            vec![
                Step::Pending,
                Step::Pending,
                Step::Emit(Command::Apply {
                    op: Operator::Delete,
                    target: Target::Motion(Motion::CurrentLine),
                    count: 3,
                })
            ]
        );
    }

    #[test]
    fn operator_plus_motion() {
        assert_eq!(
            run(&[k('d'), k('w')]),
            vec![
                Step::Pending,
                Step::Emit(Command::Apply {
                    op: Operator::Delete,
                    target: Target::Motion(Motion::WordForward),
                    count: 1,
                })
            ]
        );
    }

    #[test]
    fn counts_compose_multiplicatively() {
        // `2d3w` → delete 6 words.
        assert_eq!(
            run(&[k('2'), k('d'), k('3'), k('w')]),
            vec![
                Step::Pending,
                Step::Pending,
                Step::Pending,
                Step::Emit(Command::Apply {
                    op: Operator::Delete,
                    target: Target::Motion(Motion::WordForward),
                    count: 6,
                })
            ]
        );
    }

    #[test]
    fn text_object_inner() {
        // `ciw` — change inner word.
        assert_eq!(
            run(&[k('c'), k('i'), k('w')]),
            vec![
                Step::Pending,
                Step::Pending,
                Step::Emit(Command::Apply {
                    op: Operator::Change,
                    target: Target::Object(TextObject::Word { around: false }),
                    count: 1,
                })
            ]
        );
    }

    #[test]
    fn text_object_around() {
        // `da"` — delete around quoted string.
        assert_eq!(
            run(&[k('d'), k('a'), k('"')]),
            vec![
                Step::Pending,
                Step::Pending,
                Step::Emit(Command::Apply {
                    op: Operator::Delete,
                    target: Target::Object(TextObject::Quote {
                        quote: '"',
                        around: true,
                    }),
                    count: 1,
                })
            ]
        );
    }

    #[test]
    fn text_object_pair_normalises_to_open_bracket() {
        // Closing bracket selects the same object as opening.
        assert_eq!(
            run(&[k('d'), k('i'), k(')')]),
            vec![
                Step::Pending,
                Step::Pending,
                Step::Emit(Command::Apply {
                    op: Operator::Delete,
                    target: Target::Object(TextObject::Pair {
                        open: '(',
                        around: false,
                    }),
                    count: 1,
                })
            ]
        );
    }

    #[test]
    fn g_a_emits_quickfix() {
        assert_eq!(
            run(&[k('g'), k('a')]),
            vec![Step::Pending, Step::Emit(Command::Quickfix)]
        );
    }

    #[test]
    fn g_followed_by_unknown_cancels() {
        assert_eq!(run(&[k('g'), k('z')]), vec![Step::Pending, Step::Cancel]);
    }

    #[test]
    fn esc_in_pending_op_cancels() {
        let mut p = Parser::new();
        assert_eq!(p.feed(k('d')), Step::Pending);
        assert_eq!(p.feed(code(KeyCode::Esc)), Step::Cancel);
        // Parser is reset; next key starts fresh.
        assert_eq!(
            p.feed(k('j')),
            Step::Emit(Command::Move {
                motion: Motion::Down,
                count: 1,
            })
        );
    }

    #[test]
    fn x_emits_delete_char_under_with_count() {
        assert_eq!(
            run(&[k('3'), k('x')]),
            vec![
                Step::Pending,
                Step::Emit(Command::DeleteCharUnder { count: 3 })
            ]
        );
    }

    #[test]
    fn enter_and_colon_are_direct() {
        assert_eq!(
            run(&[code(KeyCode::Enter)]),
            vec![Step::Emit(Command::RunQuery)]
        );
        assert_eq!(run(&[k(':')]), vec![Step::Emit(Command::EnterCommand)]);
    }

    #[test]
    fn ctrl_r_is_redo() {
        assert_eq!(run(&[ctrl('r')]), vec![Step::Emit(Command::Redo)]);
    }

    #[test]
    fn plain_r_is_refresh_query() {
        assert_eq!(run(&[k('r')]), vec![Step::Emit(Command::RefreshQuery)]);
    }

    #[test]
    fn shift_modifiers_accepted_on_dataset_metric_hover() {
        // `D`, `M`, `K`, `?` are all typed with Shift on a US keyboard;
        // some terminals report Shift as a modifier.
        assert_eq!(run(&[shift('D')]), vec![Step::Emit(Command::FetchDatasets)]);
        assert_eq!(run(&[shift('M')]), vec![Step::Emit(Command::FetchMetrics)]);
        assert_eq!(run(&[shift('K')]), vec![Step::Emit(Command::Hover)]);
    }

    #[test]
    fn unknown_key_in_idle_cancels() {
        assert_eq!(run(&[k('z')]), vec![Step::Cancel]);
    }
}
