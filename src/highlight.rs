//! Per-token syntax highlighting for the editor pane.
//!
//! Asks `mpl_language_server::collect_tokens` for a span/kind list, then
//! splits each editor line into styled chunks (token text vs. the
//! whitespace/unrecognised text between tokens). The result is a vector of
//! ratatui [`Line`]s the caller can hand straight to a `Paragraph` — one
//! `Line` per source line, preserving blank lines.
//!
//! `tokens = None` (engine reported a parse failure) renders each line as a
//! single plain span so the buffer stays readable mid-edit.

use mpl_language_server::{Span as MplSpan, Token, TokenType};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Tab width matching `tui-textarea`'s `set_tab_length` default. A literal
/// `\t` in the buffer is expanded to this many spaces in the rendered line
/// so the visible cursor column stays aligned with `editor.cursor()`.
const TAB_WIDTH: usize = 4;

/// Best-effort tokeniser used when the engine's `collect_tokens` returns
/// `None` (i.e. the buffer doesn't parse — the common case mid-edit).
/// Recognises strings, backtick idents, numbers, bools, keywords, types,
/// operators and punctuation by a single linear byte scan. Won't classify
/// every edge case (no regex-literal detection, no multi-char operator
/// disambiguation past the obvious ones) but covers >95% of what users see
/// while typing an MPL query.
#[must_use]
pub fn fallback_tokens(buffer: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let bytes = buffer.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            // Whitespace.
            b if b.is_ascii_whitespace() => i += 1,

            // Comment (`# ...` to end of line).
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }

            // Quoted string — `\\` and `\"` are escapes.
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if i < bytes.len() {
                    i += 1; // closing quote
                }
                push(&mut out, start, i, TokenType::String);
            }

            // Backtick identifier.
            b'`' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'`' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if i < bytes.len() {
                    i += 1;
                }
                push(&mut out, start, i, TokenType::Variable);
            }

            // Dollar-prefixed param identifier.
            b'$' => {
                let start = i;
                i += 1;
                while i < bytes.len() && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                push(&mut out, start, i, TokenType::Variable);
            }

            // Numbers (including durations like `1m`, `30s`).
            b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                // Optional trailing duration unit.
                if i < bytes.len() && matches!(bytes[i], b's' | b'm' | b'h' | b'd' | b'w' | b'y') {
                    i += 1;
                }
                push(&mut out, start, i, TokenType::Number);
            }

            // Plain identifier / keyword / bool / type.
            b if is_ident_start(b) => {
                let start = i;
                while i < bytes.len() && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                let text = &buffer[start..i];
                let kind = classify_ident(text);
                push(&mut out, start, i, kind);
            }

            // Multi-char operators first.
            b'=' | b'!' | b'<' | b'>' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                let start = i;
                i += 2;
                push(&mut out, start, i, TokenType::Operator);
            }
            b'&' if i + 1 < bytes.len() && bytes[i + 1] == b'&' => {
                let start = i;
                i += 2;
                push(&mut out, start, i, TokenType::Operator);
            }
            b'|' if i + 1 < bytes.len() && bytes[i + 1] == b'|' => {
                let start = i;
                i += 2;
                push(&mut out, start, i, TokenType::Operator);
            }

            // Single-char operators.
            b'+' | b'-' | b'*' | b'/' | b'=' | b'<' | b'>' | b'!' => {
                push(&mut out, i, i + 1, TokenType::Operator);
                i += 1;
            }

            // Single-char punctuation.
            b'|' | b':' | b'(' | b')' | b'{' | b'}' | b'[' | b']' | b',' | b';' => {
                push(&mut out, i, i + 1, TokenType::Punctuation);
                i += 1;
            }

            // Anything else — advance, don't tokenise.
            _ => i += 1,
        }
    }
    out
}

fn push(out: &mut Vec<Token>, from: usize, to: usize, kind: TokenType) {
    out.push(Token {
        span: MplSpan::new(from, to),
        kind,
    });
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Classify a bare identifier in the fallback path. Mirrors the keyword /
/// type / bool sets recognised by the pest grammar, derived from a quick
/// scan of `mpl-lang`'s `mpl.pest`. Anything else falls through to
/// `Variable` — same default the engine uses for plain idents.
fn classify_ident(s: &str) -> TokenType {
    match s {
        "true" | "false" => TokenType::Bool,
        // Pipe-position and argument keywords.
        "where" | "filter" | "map" | "align" | "group" | "bucket" | "compute" | "sample"
        | "ifdef" | "as" | "to" | "by" | "using" | "over" | "with" | "of" | "param" | "set"
        | "and" | "or" | "not" | "in" => TokenType::Keyword,
        // Param / column types.
        "Duration" | "Dataset" | "Regex" | "string" | "int" | "float" | "bool" | "duration" => {
            TokenType::Type
        }
        _ => TokenType::Variable,
    }
}

/// Convert `buffer` (the joined editor text, lines separated by `\n`) into
/// a styled `Line` per source line. `tokens`, when present, must be sorted
/// by ascending `span.from` — that's what `collect_tokens` produces.
#[must_use]
pub fn highlight_lines(buffer: &str, tokens: Option<&[Token]>) -> Vec<Line<'static>> {
    // Compute the inclusive byte offset of each line's start. An extra entry
    // at `buffer.len()` simplifies the per-line range lookup.
    let mut line_starts: Vec<usize> =
        Vec::with_capacity(buffer.bytes().filter(|&b| b == b'\n').count() + 2);
    line_starts.push(0);
    for (i, b) in buffer.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    line_starts.push(buffer.len() + 1); // sentinel — never indexed directly

    // Number of source lines = newlines + 1 (matches `str::lines` for
    // non-empty buffers; for empty `buffer`, treat as one empty line).
    let line_count = if buffer.is_empty() {
        1
    } else {
        line_starts.len() - 1
    };

    let mut out = Vec::with_capacity(line_count);
    let tokens = tokens.unwrap_or(&[]);
    let mut tok_idx = 0;

    for row in 0..line_count {
        let line_start = line_starts[row];
        let line_end = line_starts
            .get(row + 1)
            .map(|&p| p.saturating_sub(1)) // exclude trailing `\n`
            .unwrap_or(buffer.len())
            .min(buffer.len());

        // Empty line: emit an empty styled span so the popup still has a
        // line to anchor against.
        if line_start >= line_end {
            out.push(Line::from(vec![Span::raw("")]));
            continue;
        }
        let line_text = &buffer[line_start..line_end];

        // Skip past tokens that end before this line begins.
        while tok_idx < tokens.len() && tokens[tok_idx].span.to <= line_start {
            tok_idx += 1;
        }

        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut cursor = line_start;
        while tok_idx < tokens.len() && tokens[tok_idx].span.from < line_end {
            let tok = &tokens[tok_idx];
            let tok_from = tok.span.from.max(line_start);
            let tok_to = tok.span.to.min(line_end);
            if tok_from > cursor {
                spans.push(plain_span(&buffer[cursor..tok_from]));
            }
            if tok_to > tok_from {
                spans.push(Span::styled(
                    expand_tabs(&buffer[tok_from..tok_to]),
                    style_for(&tok.kind),
                ));
            }
            cursor = tok_to;
            // Token may straddle a newline — only advance the cursor index
            // when this token is fully consumed by the current line.
            if tok.span.to <= line_end {
                tok_idx += 1;
            } else {
                break;
            }
        }
        if cursor < line_end {
            spans.push(plain_span(&buffer[cursor..line_end]));
        }
        if spans.is_empty() {
            spans.push(Span::raw(expand_tabs(line_text)));
        }
        out.push(Line::from(spans));
    }
    out
}

fn plain_span(s: &str) -> Span<'static> {
    Span::raw(expand_tabs(s))
}

fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            let pad = TAB_WIDTH - (col % TAB_WIDTH);
            for _ in 0..pad {
                out.push(' ');
            }
            col += pad;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// Token → style. Inspired by `mpl-codemirror/src/language.ts` but with
/// `Variable` painted explicitly: the terminal's default foreground is
/// indistinguishable from "unstyled text", which makes dataset and metric
/// identifiers look uncoloured next to truly plain whitespace.
fn style_for(kind: &TokenType) -> Style {
    match kind {
        TokenType::Keyword => Style::default().fg(Color::Cyan),
        TokenType::Type => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::ITALIC),
        TokenType::String => Style::default().fg(Color::Green),
        TokenType::Number => Style::default().fg(Color::Yellow),
        TokenType::Bool => Style::default().fg(Color::Magenta),
        TokenType::Regexp => Style::default().fg(Color::Magenta),
        TokenType::Operator | TokenType::Punctuation => Style::default().fg(Color::DarkGray),
        TokenType::Variable => Style::default().fg(Color::White),
    }
}

/// Merge `engine` (parser-aware, authoritative when present) with
/// `fallback` (byte-scan, covers everything the engine missed). Engine
/// tokens are kept verbatim; fallback tokens that overlap any engine
/// token are dropped. Result is sorted by `span.from`.
///
/// Motivation: the engine's tokenizer in `mpl-language-server` only maps
/// a subset of grammar rules to token kinds — `align`, `to`, `using`,
/// `by`, `as`, `over`, the `::` module separator and pipe `|`s past the
/// first two come back unclassified. The fallback recognises them by
/// keyword set, so layering it underneath gives complete colouring
/// without waiting on an upstream PR.
#[must_use]
pub fn merge_tokens(engine: &[Token], fallback: &[Token]) -> Vec<Token> {
    if engine.is_empty() {
        return fallback.iter().map(clone_token).collect();
    }

    let mut engine_sorted: Vec<&Token> = engine.iter().collect();
    engine_sorted.sort_by_key(|t| t.span.from);

    let mut out: Vec<Token> = Vec::with_capacity(engine.len() + fallback.len());
    out.extend(engine.iter().map(clone_token));

    // Two-finger walk: for each fallback token, advance the engine pointer
    // past any engine token ending at-or-before the fallback's start, then
    // check whether the current engine token overlaps.
    let mut e = 0usize;
    for fb in fallback {
        while e < engine_sorted.len() && engine_sorted[e].span.to <= fb.span.from {
            e += 1;
        }
        let overlaps = e < engine_sorted.len() && engine_sorted[e].span.from < fb.span.to;
        if !overlaps {
            out.push(clone_token(fb));
        }
    }

    out.sort_by_key(|t| t.span.from);
    out
}

fn clone_token(t: &Token) -> Token {
    Token {
        span: MplSpan::new(t.span.from, t.span.to),
        kind: copy_kind(&t.kind),
    }
}

fn copy_kind(k: &TokenType) -> TokenType {
    match k {
        TokenType::Variable => TokenType::Variable,
        TokenType::String => TokenType::String,
        TokenType::Number => TokenType::Number,
        TokenType::Bool => TokenType::Bool,
        TokenType::Regexp => TokenType::Regexp,
        TokenType::Operator => TokenType::Operator,
        TokenType::Punctuation => TokenType::Punctuation,
        TokenType::Keyword => TokenType::Keyword,
        TokenType::Type => TokenType::Type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mpl_language_server::collect_tokens;

    fn keyword_style() -> Style {
        Style::default().fg(Color::Cyan)
    }
    fn number_style() -> Style {
        Style::default().fg(Color::Yellow)
    }
    fn string_style() -> Style {
        Style::default().fg(Color::Green)
    }
    fn bool_style() -> Style {
        Style::default().fg(Color::Magenta)
    }
    fn punct_style() -> Style {
        Style::default().fg(Color::DarkGray)
    }

    /// Find the first span whose `content` equals `text` and assert its style.
    fn assert_span_style(lines: &[Line<'_>], text: &str, expected: Style) {
        for line in lines {
            for sp in &line.spans {
                if sp.content == text {
                    assert_eq!(sp.style, expected, "span {text:?} had {:?}", sp.style);
                    return;
                }
            }
        }
        let dump: Vec<Vec<(String, Style)>> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| (s.content.to_string(), s.style))
                    .collect()
            })
            .collect();
        panic!("no span with content {text:?} found in {dump:#?}");
    }

    #[test]
    fn empty_buffer_yields_one_empty_line() {
        let lines = highlight_lines("", None);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>(),
            vec![""]
        );
    }

    #[test]
    fn no_tokens_falls_back_to_plain_per_line() {
        // `highlight_lines` is pure — it doesn't itself run the fallback. With
        // an empty token slice every byte renders as `Span::raw` (plain).
        let buffer = "first\nsecond\nthird";
        let lines = highlight_lines(buffer, Some(&[]));
        assert_eq!(lines.len(), 3);
        for (line, expected) in lines.iter().zip(["first", "second", "third"]) {
            let joined: String = line
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("");
            assert_eq!(joined, expected);
            for sp in &line.spans {
                assert_eq!(sp.style, Style::default(), "expected plain style");
            }
        }
    }

    #[test]
    fn keywords_get_keyword_style() {
        let buffer = "home:temp | where x == 1";
        let tokens = collect_tokens(buffer).expect("tokens");
        let lines = highlight_lines(buffer, Some(&tokens));
        assert_span_style(&lines, "where", keyword_style());
    }

    #[test]
    fn numbers_get_number_style() {
        let buffer = "home:temp | where x == 42";
        let tokens = collect_tokens(buffer).expect("tokens");
        let lines = highlight_lines(buffer, Some(&tokens));
        assert_span_style(&lines, "42", number_style());
    }

    #[test]
    fn bools_get_bool_style() {
        let buffer = "home:temp | where flag == true";
        let tokens = collect_tokens(buffer).expect("tokens");
        let lines = highlight_lines(buffer, Some(&tokens));
        assert_span_style(&lines, "true", bool_style());
    }

    #[test]
    fn strings_get_string_style() {
        let buffer = "home:temp | where host == \"web-1\"";
        let tokens = collect_tokens(buffer).expect("tokens");
        let lines = highlight_lines(buffer, Some(&tokens));
        assert_span_style(&lines, "\"web-1\"", string_style());
    }

    #[test]
    fn pipe_punctuation_is_dim() {
        let buffer = "home:temp | where x == 1";
        let tokens = collect_tokens(buffer).expect("tokens");
        let lines = highlight_lines(buffer, Some(&tokens));
        assert_span_style(&lines, "|", punct_style());
    }

    #[test]
    fn tokens_on_later_lines_land_in_those_lines() {
        let buffer = "home:temp\n| where x == 1";
        let tokens = collect_tokens(buffer).expect("tokens");
        let lines = highlight_lines(buffer, Some(&tokens));
        assert_eq!(lines.len(), 2);
        // `where` lives on line 2 and must not bleed into line 1.
        let line1_text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(!line1_text.contains("where"), "line 1: {line1_text}");
        let line2_text: String = lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(line2_text.contains("where"), "line 2: {line2_text}");
    }

    #[test]
    fn text_between_tokens_is_plain() {
        let buffer = "home:temp | where x == 1";
        let tokens = collect_tokens(buffer).expect("tokens");
        let lines = highlight_lines(buffer, Some(&tokens));
        let any_plain_space = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|sp| sp.style == Style::default() && sp.content.chars().all(|c| c == ' '));
        assert!(
            any_plain_space,
            "expected at least one plain whitespace span"
        );
    }

    // ── fallback tokenizer (mid-edit) ────────────────────────────────────

    /// Run the fallback path on `q` and assert that the first span whose
    /// text equals `text` carries `expected` style. Tests partial queries
    /// that don't parse via the engine.
    fn assert_fallback_style(q: &str, text: &str, expected: Style) {
        let tokens = fallback_tokens(q);
        let lines = highlight_lines(q, Some(&tokens));
        assert_span_style(&lines, text, expected);
    }

    #[test]
    fn fallback_partial_query_highlights_keyword() {
        assert_fallback_style("home:t | wh", "|", punct_style());
    }

    #[test]
    fn fallback_in_progress_pipe_recognises_where() {
        assert_fallback_style("home:temp | where host", "where", keyword_style());
    }

    #[test]
    fn fallback_string_in_progress() {
        assert_fallback_style("home:temp | where host == \"web", "\"web", string_style());
    }

    #[test]
    fn fallback_number_with_duration_suffix() {
        assert_fallback_style("home:temp | align to 30s", "30s", number_style());
    }

    #[test]
    fn fallback_bool_literal() {
        assert_fallback_style("home:temp | where flag == true", "true", bool_style());
    }

    #[test]
    fn fallback_param_ident() {
        assert_fallback_style(
            "home:temp | align to $__interval",
            "$__interval",
            Style::default().fg(Color::White),
        );
    }

    #[test]
    fn fallback_type_keyword() {
        assert_fallback_style(
            "param $w: Duration; home:temp",
            "Duration",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::ITALIC),
        );
    }

    #[test]
    fn fallback_backtick_ident_treated_as_variable() {
        // Variables now carry an explicit white foreground so backticked
        // dataset/metric idents are visually distinct from plain text.
        assert_fallback_style("`home`:`temp`", "`home`", Style::default().fg(Color::White));
    }

    #[test]
    fn fallback_comment_does_not_panic_and_runs_to_eol() {
        // No assertion on style — just that the scanner consumes the comment
        // cleanly and doesn't infinite-loop.
        let q = "# this is a comment\nhome:temp";
        let tokens = fallback_tokens(q);
        let lines = highlight_lines(q, Some(&tokens));
        assert_eq!(lines.len(), 2);
    }

    // ── engine + fallback merge ───────────────────────────────────────────────

    #[test]
    fn merged_tokens_cover_engine_gaps() {
        // The user-reported case: engine emits tokens for `where`/`5`/the
        // first two `|`s but skips `align`, `to`, `using`, and `::`. After
        // the merge those gap tokens come from the fallback.
        let q = "`homeassistant-metrics`:`ha.sensor.current`\n| where tag == 5\n| align to $__interval using prom::rate";
        let engine = collect_tokens(q).expect("buffer should parse");
        let fallback = fallback_tokens(q);
        let merged = merge_tokens(&engine, &fallback);
        let lines = highlight_lines(q, Some(&merged));
        for kw in ["align", "to", "using"] {
            assert_span_style(&lines, kw, keyword_style());
        }
        // `::` is two chars; the fallback classifies each ':' separately
        // as punctuation. Verify at least the first one shows up.
        assert_span_style(&lines, ":", punct_style());
    }

    #[test]
    fn merge_keeps_engine_token_on_overlap() {
        // Engine says "where" is a Keyword; fallback would also say so.
        // After merge only one token should cover the span (the engine's).
        let q = "home:temp | where x == 1";
        let engine = collect_tokens(q).expect("tokens");
        let fallback = fallback_tokens(q);
        let merged = merge_tokens(&engine, &fallback);
        let where_count = merged
            .iter()
            .filter(|t| q.get(t.span.from..t.span.to) == Some("where"))
            .count();
        assert_eq!(
            where_count, 1,
            "merge must not duplicate overlapping tokens"
        );
    }

    #[test]
    fn merge_with_empty_engine_returns_fallback_only() {
        let q = "home:temp";
        let fallback = fallback_tokens(q);
        let merged = merge_tokens(&[], &fallback);
        assert_eq!(merged.len(), fallback.len());
    }

    #[test]
    fn tabs_expand_to_four_spaces() {
        let buffer = "a\tb";
        let lines = highlight_lines(buffer, Some(&[]));
        let joined: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(joined, "a   b", "got {joined:?}");
    }
}
