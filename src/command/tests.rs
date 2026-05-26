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
fn r_then_char_is_replace() {
    // Vim `r<c>`: the parser pends after `r`, then emits ReplaceChar
    // with the next char as the replacement. Default count is 1.
    assert_eq!(
        run(&[k('r'), k('x')]),
        vec![
            Step::Pending,
            Step::Emit(Command::ReplaceChar { ch: 'x', count: 1 })
        ]
    );
}

#[test]
fn count_r_char_carries_the_count() {
    // `3rz` replaces three chars with `z`.
    assert_eq!(
        run(&[k('3'), k('r'), k('z')]),
        vec![
            Step::Pending, // digit
            Step::Pending, // r
            Step::Emit(Command::ReplaceChar { ch: 'z', count: 3 })
        ]
    );
}

#[test]
fn r_enter_replaces_with_newline() {
    use ratatui::crossterm::event::KeyCode;
    let enter_key = ratatui::crossterm::event::KeyEvent::new(
        KeyCode::Enter,
        ratatui::crossterm::event::KeyModifiers::NONE,
    );
    assert_eq!(
        run(&[k('r'), enter_key]),
        vec![
            Step::Pending,
            Step::Emit(Command::ReplaceChar { ch: '\n', count: 1 })
        ]
    );
}

#[test]
fn r_then_esc_cancels() {
    use ratatui::crossterm::event::KeyCode;
    let esc_key = ratatui::crossterm::event::KeyEvent::new(
        KeyCode::Esc,
        ratatui::crossterm::event::KeyModifiers::NONE,
    );
    assert_eq!(run(&[k('r'), esc_key]), vec![Step::Pending, Step::Cancel]);
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
