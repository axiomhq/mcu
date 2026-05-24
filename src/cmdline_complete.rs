//! Tab completion for the `:` Ex-command line.
//!
//! Vim-wildmenu-shaped, but **fuzzy** rather than prefix: typing `:dl`
//! and hitting Tab matches `:datasets` and `:dashinfo` (any
//! subsequence with the right characters in order), with the closest
//! match selected first. Sub-command and contextual slots (`:dash sa`,
//! `:open prod`, `:tile add s`) all use the same scorer.
//!
//! The entry point is [`completions_for`], which is a pure function
//! over the cmdline buffer and a small `Context` carrying the data the
//! contextual completers need (currently the cached dashboard list).
//! `App` reaches for it from `handle_command_key` when the user
//! presses Tab.
//!
//! Splicing is described by [`CompletionRequest`]: the candidate list
//! plus the *byte range in the buffer* that the chosen item replaces.
//! Keeping the range explicit lets the caller place the cursor
//! correctly without re-tokenising.

use crate::axiom::DashboardSummary;
use crate::dashboard::VizKind;
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Per-call context handed to the completer. Borrowed slices so the
/// caller doesn't have to clone.
pub struct Context<'a> {
    pub dashboards: &'a [DashboardSummary],
}

/// Result of completing the current cmdline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    /// Candidate completions, sorted lexicographically and deduplicated.
    pub items: Vec<String>,
    /// Byte range in the buffer covering the token to be replaced.
    /// `(start, end)` is a half-open range; `end - start == 0` for an
    /// empty trailing slot (e.g. `:dash ` with the cursor at the end).
    pub range: (usize, usize),
}

/// Full Ex-command vocabulary. Keep alphabetised; empty-token
/// completion preserves this order for deterministic first-Tab
/// results.
const HEAD_COMMANDS: &[&str] = &[
    "axiom",
    "dash",
    "dashinfo",
    "datasets",
    "di",
    "ds",
    "e",
    "edit",
    "grid",
    "h",
    "help",
    "m",
    "metrics",
    "open",
    "p",
    "param",
    "q",
    "quit",
    "r",
    "refresh",
    "run",
    "solo",
    "tile",
    "time",
    "trace",
    "viz",
    "w",
    "wq",
    "write",
    "x",
];

/// Sub-commands for `:dash`.
const DASH_SUBS: &[&str] = &["ls", "new", "rm", "save"];

/// Sub-commands for `:tile`.
const TILE_SUBS: &[&str] = &["add", "inspect", "json", "mv", "rm", "size", "title"];

/// Compute completions for the cmdline at the given char-cursor
/// (mirrors `CmdLine.cursor` which counts chars, not bytes). Returns
/// `None` when no completion source applies for this position.
///
/// Matching policy: **smart-case fuzzy** (nucleo-matcher). An empty
/// token returns every candidate in its category, sorted
/// alphabetically; a non-empty token keeps only candidates that
/// contain the typed characters as an in-order subsequence and orders
/// them by descending match score (prefix / word-start matches
/// outrank scattered ones).
pub fn completions_for(
    buf: &str,
    char_cursor: usize,
    ctx: &Context<'_>,
) -> Option<CompletionRequest> {
    let byte_cursor = char_to_byte(buf, char_cursor);
    let (token, range) = current_token(buf, byte_cursor);
    let head = head_of(buf);
    let prefix_args = args_before_cursor(buf, byte_cursor);

    // First token → head completion.
    if prefix_args == 0 {
        return Some(filter_candidates(
            HEAD_COMMANDS.iter().copied(),
            token,
            range,
        ));
    }
    // Strip any trailing `!` from the head before sub-command lookup
    // (the bang affects execute_command, not the identifier).
    let head = head.trim_end_matches('!');

    match (head, prefix_args) {
        ("dash", 1) => Some(filter_candidates(DASH_SUBS.iter().copied(), token, range)),
        ("tile", 1) => Some(filter_candidates(TILE_SUBS.iter().copied(), token, range)),
        ("tile", 2) => {
            // `:tile add <viz>` is the only second-arg slot with a
            // closed vocabulary.
            let sub = nth_arg(buf, 1).unwrap_or("");
            if sub == "add" {
                Some(filter_candidates(
                    viz_kind_names().into_iter(),
                    token,
                    range,
                ))
            } else {
                None
            }
        }
        ("viz", 1) => Some(filter_candidates(
            viz_kind_names().into_iter(),
            token,
            range,
        )),
        ("dash", 2) if nth_arg(buf, 1) == Some("rm") => Some(filter_candidates(
            ctx.dashboards.iter().map(|d| d.uid.as_str()),
            token,
            range,
        )),
        ("open", 1) => Some(filter_candidates(
            ctx.dashboards.iter().map(|d| d.uid.as_str()),
            token,
            range,
        )),
        ("run" | "r", 1) => Some(filter_candidates(
            ["tile", "dashboard"].into_iter(),
            token,
            range,
        )),
        _ => None,
    }
}

fn viz_kind_names() -> Vec<&'static str> {
    use VizKind::*;
    [
        Line,
        Bar,
        Area,
        Scatter,
        Statistic,
        TopList,
        Pie,
        Heatmap,
        Table,
        LogStream,
        MonitorList,
        Note,
        Spacer,
    ]
    .into_iter()
    .map(|k| k.as_str())
    .collect()
}

/// Filter `candidates` by `token` using nucleo's fuzzy scorer. Empty
/// `token` short-circuits to alphabetical order; otherwise items are
/// ordered by descending score with ties broken alphabetically.
fn filter_candidates<I>(candidates: I, token: &str, range: (usize, usize)) -> CompletionRequest
where
    I: Iterator,
    I::Item: Into<String>,
{
    let mut items: Vec<String> = candidates.map(Into::into).collect();
    items.sort();
    items.dedup();
    if token.is_empty() {
        return CompletionRequest { items, range };
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut needle_buf = Vec::new();
    let needle = Utf32Str::new(token, &mut needle_buf);
    let mut scored: Vec<(String, u16)> = items
        .into_iter()
        .filter_map(|s| {
            let mut h_buf = Vec::new();
            let h = Utf32Str::new(&s, &mut h_buf);
            matcher.fuzzy_match(h, needle).map(|score| (s, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    CompletionRequest {
        items: scored.into_iter().map(|(s, _)| s).collect(),
        range,
    }
}

/// Locate the token under (or just-before) `byte_cursor`. Returns the
/// token's text plus its byte range in `buf`; if the cursor is in
/// whitespace, the range is empty at the cursor.
fn current_token(buf: &str, byte_cursor: usize) -> (&str, (usize, usize)) {
    let bytes = buf.as_bytes();
    if byte_cursor > bytes.len() {
        return ("", (bytes.len(), bytes.len()));
    }
    let mut start = byte_cursor;
    while start > 0 && !bytes[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    let mut end = byte_cursor;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    (&buf[start..end], (start, end))
}

/// The head of the cmdline (first whitespace-separated token), with
/// any trailing `!` left intact. Empty string for empty buffer.
fn head_of(buf: &str) -> &str {
    buf.split_whitespace().next().unwrap_or("")
}

/// `n`th whitespace-separated argument (0 = head). `None` when out of
/// range.
fn nth_arg(buf: &str, n: usize) -> Option<&str> {
    buf.split_whitespace().nth(n)
}

/// Number of whitespace-separated *complete* arguments BEFORE the
/// token currently under the cursor.
///
/// With `:dash sa|` (cursor after `sa`)  →  1 (just `dash` is complete).
/// With `:dash save |` (cursor after the trailing space)  →  2.
fn args_before_cursor(buf: &str, byte_cursor: usize) -> usize {
    let head_part = &buf[..byte_cursor.min(buf.len())];
    let mut count = 0;
    let mut in_token = false;
    for c in head_part.chars() {
        if c.is_whitespace() {
            if in_token {
                count += 1;
                in_token = false;
            }
        } else {
            in_token = true;
        }
    }
    count
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests;
