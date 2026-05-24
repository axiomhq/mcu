//! Byte-range resolution for motions and text objects.
//!
//! Companion to [`crate::command`]: the parser tells you *what* to act on
//! (`d3w`, `ciw`, `da"`, …), this module computes *which bytes*. The host
//! then applies the operator (`Delete`/`Yank`/`Change`/`Indent`) to that
//! [`Range`].
//!
//! Word boundaries follow vim's three-kind classification (matching
//! `tui-textarea`'s implementation so motion-cursor and motion-delete
//! agree): whitespace, ASCII punctuation, and "other" (word chars). A
//! word is a maximal run of one non-whitespace kind.

use crate::command::{Motion, TextObject};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: usize,
    pub end: usize,
    /// When `true`, the range covers whole lines (including trailing
    /// newline). Affects how paste handles the yanked text — linewise
    /// paste inserts on a new line rather than at the cursor.
    pub linewise: bool,
}

impl Range {
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    pub fn slice<'a>(&self, buf: &'a str) -> &'a str {
        &buf[self.start.min(buf.len())..self.end.min(buf.len())]
    }
}

/// Resolve a motion target to a byte range starting from `cursor`.
///
/// `op_is_change` is the vim `cw` quirk: when used as the target of a
/// `Change`, `WordForward` behaves like `WordEnd` (it stops at the end of
/// the current word, not the start of the next).
pub fn resolve_motion(
    buf: &str,
    cursor: usize,
    motion: Motion,
    count: usize,
    op_is_change: bool,
) -> Option<Range> {
    let n = buf.len();
    let cursor = cursor.min(n);
    let count = count.max(1);

    if motion == Motion::CurrentLine {
        return Some(current_line_range(buf, cursor, count));
    }

    // Apply the motion `count` times, walking forward or back.
    let effective = if op_is_change && motion == Motion::WordForward {
        Motion::WordEnd
    } else {
        motion
    };
    let mut pos = cursor;
    for _ in 0..count {
        pos = match effective {
            Motion::WordForward => word_forward(buf, pos),
            Motion::WordEnd => word_end_inclusive(buf, pos),
            Motion::WordBack => word_back(buf, pos),
            Motion::LineStart => line_start(buf, pos),
            Motion::FirstNonBlank => first_non_blank(buf, pos),
            Motion::LineEnd => line_end(buf, pos),
            Motion::Left => prev_char(buf, pos),
            Motion::Right => next_char(buf, pos),
            Motion::FileStart => 0,
            Motion::FileEnd => n,
            Motion::FindChar {
                ch,
                forward: true,
                till,
            } => {
                let p = find_char_forward(buf, pos, ch)?;
                // f<c>: range end = p + 1 (inclusive of ch).
                // t<c>: range end = p   (exclusive of ch).
                if till { p } else { next_char(buf, p) }
            }
            Motion::FindChar {
                ch,
                forward: false,
                till,
            } => {
                let p = find_char_back(buf, pos, ch)?;
                // F<c>: range start = p   (inclusive of ch).
                // T<c>: range start = p+1 (exclusive of ch).
                if till { next_char(buf, p) } else { p }
            }
            Motion::Up | Motion::Down => return None,
            Motion::CurrentLine => unreachable!(),
        };
    }

    // `WordEnd` is inclusive in vim — extend by one char so the range
    // `end` is exclusive like everything else. (FindChar handles its own
    // inclusivity inside the match arms above.)
    let pos = if effective == Motion::WordEnd && pos < n {
        next_char(buf, pos)
    } else {
        pos
    };

    let (start, end) = if pos >= cursor {
        (cursor, pos)
    } else {
        (pos, cursor)
    };
    Some(Range {
        start,
        end,
        linewise: false,
    })
}

/// Resolve a text-object selector to a byte range. Text objects don't
/// care about counts in any way we support yet, so the `count` is
/// accepted but ignored.
pub fn resolve_object(buf: &str, cursor: usize, obj: TextObject) -> Option<Range> {
    match obj {
        TextObject::Word { around } => word_object(buf, cursor, around),
        TextObject::Pair { open, around } => pair_object(buf, cursor, open, around),
        TextObject::Quote { quote, around } => quote_object(buf, cursor, quote, around),
    }
}

// ── Line helpers ────────────────────────────────────────────────────

/// Index of the first non-whitespace byte on the line containing `pos`.
/// Falls back to the byte after the last whitespace char on the line if
/// the line is all whitespace (i.e. the end of line).
pub fn first_non_blank(buf: &str, pos: usize) -> usize {
    let start = line_start(buf, pos);
    let end = line_end(buf, pos);
    let mut i = start;
    while i < end {
        let c = buf[i..].chars().next().unwrap();
        if !c.is_whitespace() {
            return i;
        }
        i += c.len_utf8();
    }
    end
}

/// Public re-exports so the host can compute char-find targets without
/// duplicating the boundary logic.
pub fn prev_char_at(buf: &str, pos: usize) -> usize {
    prev_char(buf, pos)
}
pub fn next_char_at(buf: &str, pos: usize) -> usize {
    next_char(buf, pos)
}

/// Find `ch` going forward, strictly after `from`, restricted to the
/// current line. Returns the byte offset of the match, or `None`.
pub fn find_char_forward(buf: &str, from: usize, ch: char) -> Option<usize> {
    let line_end = line_end(buf, from);
    let start = next_char(buf, from).min(line_end);
    let mut i = start;
    while i < line_end {
        let c = buf[i..].chars().next()?;
        if c == ch {
            return Some(i);
        }
        i += c.len_utf8();
    }
    None
}

/// Find `ch` going backward, strictly before `from`, restricted to the
/// current line.
pub fn find_char_back(buf: &str, from: usize, ch: char) -> Option<usize> {
    let line_start = line_start(buf, from);
    if from <= line_start {
        return None;
    }
    let mut i = from;
    while i > line_start {
        i = prev_char(buf, i);
        let c = buf[i..].chars().next()?;
        if c == ch {
            return Some(i);
        }
    }
    None
}

fn line_start(buf: &str, pos: usize) -> usize {
    buf[..pos.min(buf.len())]
        .rfind('\n')
        .map(|p| p + 1)
        .unwrap_or(0)
}

fn line_end(buf: &str, pos: usize) -> usize {
    let pos = pos.min(buf.len());
    buf[pos..].find('\n').map(|p| pos + p).unwrap_or(buf.len())
}

fn current_line_range(buf: &str, cursor: usize, count: usize) -> Range {
    let start = line_start(buf, cursor);
    let mut end = cursor;
    for _ in 0..count {
        end = line_end(buf, end);
        // Include the trailing newline so paste re-inserts as a line.
        if end < buf.len() {
            end += 1;
        }
    }
    // On the very last line with no trailing newline, also pull in the
    // preceding `\n` so the line truly disappears.
    if end == buf.len() && start > 0 && !buf.ends_with('\n') {
        return Range {
            start: start - 1,
            end,
            linewise: true,
        };
    }
    Range {
        start,
        end,
        linewise: true,
    }
}

fn prev_char(buf: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut p = pos - 1;
    while p > 0 && !buf.is_char_boundary(p) {
        p -= 1;
    }
    p
}

fn next_char(buf: &str, pos: usize) -> usize {
    let n = buf.len();
    if pos >= n {
        return n;
    }
    let mut p = pos + 1;
    while p < n && !buf.is_char_boundary(p) {
        p += 1;
    }
    p
}

// ── Word helpers ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Space,
    Punct,
    Other,
}

impl CharKind {
    fn of(c: char) -> Self {
        if c.is_whitespace() {
            Self::Space
        } else if c.is_ascii_punctuation() {
            Self::Punct
        } else {
            Self::Other
        }
    }
}

fn kind_at(buf: &str, byte: usize) -> Option<CharKind> {
    buf[byte..].chars().next().map(CharKind::of)
}

fn word_forward(buf: &str, cursor: usize) -> usize {
    let mut it = buf[cursor..].char_indices();
    let Some((_, first)) = it.next() else {
        return buf.len();
    };
    let mut prev = CharKind::of(first);
    for (i, c) in it {
        let cur = CharKind::of(c);
        if cur != CharKind::Space && prev != cur {
            return cursor + i;
        }
        prev = cur;
    }
    buf.len()
}

fn word_end_inclusive(buf: &str, cursor: usize) -> usize {
    // Step past the current char first — vim's `e` from the end of a
    // word jumps to the end of the *next* word, not stays put.
    let start = next_char(buf, cursor);
    let mut last_byte = start;
    let mut prev: Option<CharKind> = None;
    for (off, c) in buf[start..].char_indices() {
        let cur = CharKind::of(c);
        if let Some(p) = prev
            && p != CharKind::Space
            && cur != p
        {
            return start + off - c.len_utf8().min(off); // last byte of previous char
        }
        last_byte = start + off;
        prev = Some(cur);
    }
    last_byte
}

fn word_back(buf: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    // Walk back char-by-char; the cursor lands at the start of the
    // current/previous word.
    let prefix = &buf[..cursor];
    let mut rev = prefix.char_indices().rev();
    let Some((_, first)) = rev.next() else {
        return 0;
    };
    let mut cur = CharKind::of(first);
    let mut last_byte = prefix.char_indices().last().map(|(i, _)| i).unwrap_or(0);
    for (i, c) in rev {
        let next = CharKind::of(c);
        if cur != CharKind::Space && next != cur {
            return last_byte;
        }
        cur = next;
        last_byte = i;
    }
    if cur != CharKind::Space { 0 } else { last_byte }
}

// ── Text objects ────────────────────────────────────────────────────

fn word_object(buf: &str, cursor: usize, around: bool) -> Option<Range> {
    let kind = kind_at(buf, cursor)?;

    // `iw` on whitespace selects the whitespace run; `aw` extends to the
    // word past it.
    if kind == CharKind::Space {
        let mut start = cursor;
        while start > 0 {
            let prev = prev_char(buf, start);
            if kind_at(buf, prev)? == CharKind::Space {
                start = prev;
            } else {
                break;
            }
        }
        let mut end = cursor;
        while end < buf.len() && kind_at(buf, end) == Some(CharKind::Space) {
            end = next_char(buf, end);
        }
        if around && end < buf.len() {
            // Extend over the following word.
            let extra = kind_at(buf, end)?;
            while end < buf.len() && kind_at(buf, end) == Some(extra) {
                end = next_char(buf, end);
            }
        }
        return Some(Range {
            start,
            end,
            linewise: false,
        });
    }

    let mut start = cursor;
    while start > 0 {
        let prev = prev_char(buf, start);
        if kind_at(buf, prev) == Some(kind) {
            start = prev;
        } else {
            break;
        }
    }
    let mut end = cursor;
    while end < buf.len() && kind_at(buf, end) == Some(kind) {
        end = next_char(buf, end);
    }
    if around {
        // Vim's `aw`: extend over trailing inline whitespace (not `\n`);
        // if there's no trailing whitespace, extend over leading instead.
        let trailing_start = end;
        while end < buf.len() {
            let c = buf[end..].chars().next().unwrap();
            if c.is_whitespace() && c != '\n' {
                end = next_char(buf, end);
            } else {
                break;
            }
        }
        if end == trailing_start {
            while start > 0 {
                let prev = prev_char(buf, start);
                let c = buf[prev..].chars().next().unwrap();
                if c.is_whitespace() && c != '\n' {
                    start = prev;
                } else {
                    break;
                }
            }
        }
    }
    Some(Range {
        start,
        end,
        linewise: false,
    })
}

fn pair_object(buf: &str, cursor: usize, open: char, around: bool) -> Option<Range> {
    let close = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => return None,
    };
    let bytes = buf.as_bytes();
    let open_b = open as u8;
    let close_b = close as u8;

    // Walk back from cursor to find an unmatched opener.
    let mut depth: i32 = 0;
    let mut open_pos = None;
    let mut i = cursor;
    while i > 0 {
        i -= 1;
        let b = bytes[i];
        if b == close_b {
            depth += 1;
        } else if b == open_b {
            if depth == 0 {
                open_pos = Some(i);
                break;
            }
            depth -= 1;
        }
    }
    let open_pos = open_pos?;

    // Walk forward from cursor to find the matching closer.
    let mut depth: i32 = 0;
    let mut close_pos = None;
    let mut i = open_pos + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == open_b {
            depth += 1;
        } else if b == close_b {
            if depth == 0 {
                close_pos = Some(i);
                break;
            }
            depth -= 1;
        }
        i += 1;
    }
    let close_pos = close_pos?;

    let (start, end) = if around {
        (open_pos, close_pos + 1)
    } else {
        (open_pos + 1, close_pos)
    };
    Some(Range {
        start,
        end,
        linewise: false,
    })
}

fn quote_object(buf: &str, cursor: usize, quote: char, around: bool) -> Option<Range> {
    // Vim's `i"` only scans the current line so a stray quote in
    // commentary above doesn't sabotage the match.
    let bytes = buf.as_bytes();
    let ls = bytes[..cursor]
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let le = bytes[cursor..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| cursor + p)
        .unwrap_or(bytes.len());
    let qb = quote as u8;

    let mut positions: Vec<usize> = Vec::new();
    for (i, &b) in bytes[ls..le].iter().enumerate() {
        let pos = ls + i;
        if b == qb && !is_escaped(bytes, pos) {
            positions.push(pos);
        }
    }
    if positions.len() < 2 {
        return None;
    }
    // Pair them in order; find the pair that encloses the cursor.
    for pair in positions.chunks_exact(2) {
        let (a, b) = (pair[0], pair[1]);
        if cursor >= a && cursor <= b {
            let (start, end) = if around { (a, b + 1) } else { (a + 1, b) };
            return Some(Range {
                start,
                end,
                linewise: false,
            });
        }
    }
    None
}

fn is_escaped(bytes: &[u8], at: usize) -> bool {
    let mut count = 0;
    let mut k = at;
    while k > 0 && bytes[k - 1] == b'\\' {
        count += 1;
        k -= 1;
    }
    count % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Motion, TextObject};

    fn r(start: usize, end: usize) -> Range {
        Range {
            start,
            end,
            linewise: false,
        }
    }

    // ── Word motions ──────────────────────────────────────────────────

    #[test]
    fn dw_includes_trailing_space() {
        // cursor on `f`, `dw` should delete `foo ` (4 bytes).
        let buf = "foo bar";
        let got = resolve_motion(buf, 0, Motion::WordForward, 1, false).unwrap();
        assert_eq!(got, r(0, 4));
    }

    #[test]
    fn de_stops_at_word_end_inclusive() {
        let buf = "foo bar";
        let got = resolve_motion(buf, 0, Motion::WordEnd, 1, false).unwrap();
        assert_eq!(got, r(0, 3));
    }

    #[test]
    fn cw_quirk_acts_like_ce() {
        // `cw` should behave like `ce` and stop at end of word.
        let buf = "foo bar";
        let got = resolve_motion(buf, 0, Motion::WordForward, 1, true).unwrap();
        assert_eq!(got, r(0, 3));
    }

    #[test]
    fn db_from_word_end_deletes_current_word() {
        let buf = "foo bar";
        let got = resolve_motion(buf, 3, Motion::WordBack, 1, false).unwrap();
        assert_eq!(got, r(0, 3));
    }

    #[test]
    fn dw_count_two_spans_two_words() {
        let buf = "foo bar baz";
        let got = resolve_motion(buf, 0, Motion::WordForward, 2, false).unwrap();
        assert_eq!(got, r(0, 8));
    }

    // ── Current-line range ────────────────────────────────────────────

    #[test]
    fn dd_includes_trailing_newline() {
        let buf = "foo\nbar\nbaz";
        let got = resolve_motion(buf, 1, Motion::CurrentLine, 1, false).unwrap();
        assert_eq!(got.start, 0);
        assert_eq!(got.end, 4);
        assert!(got.linewise);
    }

    #[test]
    fn dd_on_last_line_pulls_in_leading_newline() {
        let buf = "foo\nbar";
        // Cursor on `bar` (last line, no trailing newline).
        let got = resolve_motion(buf, 4, Motion::CurrentLine, 1, false).unwrap();
        assert_eq!(got.start, 3);
        assert_eq!(got.end, 7);
        assert!(got.linewise);
    }

    #[test]
    fn dd_count_three() {
        let buf = "a\nb\nc\nd\n";
        let got = resolve_motion(buf, 0, Motion::CurrentLine, 3, false).unwrap();
        assert_eq!(got.start, 0);
        assert_eq!(got.end, 6);
    }

    // ── Word text objects ─────────────────────────────────────────────

    #[test]
    fn iw_selects_inner_word() {
        let buf = "foo bar baz";
        let got = resolve_object(buf, 5, TextObject::Word { around: false }).unwrap();
        assert_eq!(got, r(4, 7));
    }

    #[test]
    fn aw_extends_over_trailing_space() {
        let buf = "foo bar baz";
        let got = resolve_object(buf, 0, TextObject::Word { around: true }).unwrap();
        assert_eq!(got, r(0, 4));
    }

    #[test]
    fn aw_extends_left_when_no_trailing() {
        let buf = "foo bar";
        // cursor on `b` of `bar` — no trailing whitespace, extend left.
        let got = resolve_object(buf, 4, TextObject::Word { around: true }).unwrap();
        assert_eq!(got, r(3, 7));
    }

    // ── Quote objects ─────────────────────────────────────────────────

    #[test]
    fn i_quote_excludes_quotes() {
        let buf = "x == \"hello, world\"";
        let got = resolve_object(
            buf,
            10,
            TextObject::Quote {
                quote: '"',
                around: false,
            },
        )
        .unwrap();
        assert_eq!(buf[got.start..got.end].to_string(), "hello, world");
    }

    #[test]
    fn a_quote_includes_quotes() {
        let buf = "x == \"hi\"";
        let got = resolve_object(
            buf,
            6,
            TextObject::Quote {
                quote: '"',
                around: true,
            },
        )
        .unwrap();
        assert_eq!(buf[got.start..got.end].to_string(), "\"hi\"");
    }

    #[test]
    fn quote_object_respects_escape() {
        let buf = "\"a\\\"b\"";
        // cursor inside; should select `a\"b` (4 chars).
        let got = resolve_object(
            buf,
            1,
            TextObject::Quote {
                quote: '"',
                around: false,
            },
        )
        .unwrap();
        assert_eq!(&buf[got.start..got.end], "a\\\"b");
    }

    // ── Bracket pairs ─────────────────────────────────────────────────

    #[test]
    fn i_paren_excludes_brackets() {
        let buf = "f(a, b)";
        let got = resolve_object(
            buf,
            3,
            TextObject::Pair {
                open: '(',
                around: false,
            },
        )
        .unwrap();
        assert_eq!(&buf[got.start..got.end], "a, b");
    }

    #[test]
    fn a_paren_includes_brackets() {
        let buf = "f(a, b)";
        let got = resolve_object(
            buf,
            3,
            TextObject::Pair {
                open: '(',
                around: true,
            },
        )
        .unwrap();
        assert_eq!(&buf[got.start..got.end], "(a, b)");
    }

    #[test]
    fn i_paren_handles_nesting() {
        let buf = "f(g(x), y)";
        // cursor inside outer paren but past inner; should select outer body.
        let got = resolve_object(
            buf,
            7,
            TextObject::Pair {
                open: '(',
                around: false,
            },
        )
        .unwrap();
        assert_eq!(&buf[got.start..got.end], "g(x), y");
    }

    #[test]
    fn i_paren_inside_inner_selects_inner() {
        let buf = "f(g(x), y)";
        let got = resolve_object(
            buf,
            4,
            TextObject::Pair {
                open: '(',
                around: false,
            },
        )
        .unwrap();
        assert_eq!(&buf[got.start..got.end], "x");
    }
}
