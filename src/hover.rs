//! Hover docs and signature help.
//!
//! Two related lookups against `mpl_language_server::function_info`:
//!
//!   * [`resolve_function_at`] — extracts the identifier at the cursor
//!     (including `::` separators so `prom::rate` is one symbol) and
//!     resolves it to a [`HoverInfo`]. Used by the `K` Normal-mode binding.
//!
//!   * [`find_call_context`] — scans back from the cursor looking for an
//!     unmatched `(`, ignoring text inside `"…"` strings and `` `…` ``
//!     identifiers. When found, identifies the function being called and
//!     counts top-level commas between `(` and cursor to derive the active
//!     argument index. Used to drive the status-line signature help.
//!
//! Both functions own the buffer slice they operate on and don't mutate
//! state — recompute on every relevant keystroke from the app layer.
//!
//! The "outside any call" case returns `None`. Callers should hide the
//! signature line when that happens, and `recompute_diagnostics`-style
//! the result on every cursor move.

use mpl_language_server::{CompletionArg, FunctionInfo, function_info};

/// Information surfaced in the hover popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverInfo {
    pub label: String,
    /// `(name, type-display-string)` per arg. We unwrap the engine's
    /// `ArgType` to a string here so the UI layer never needs to touch
    /// `mpl_lang`.
    pub args: Vec<(String, String)>,
    pub info: Option<String>,
}

/// Information surfaced in the status-line signature help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigHelp {
    pub label: String,
    pub args: Vec<(String, String)>,
    /// Index of the active arg, derived from the comma count between the
    /// open `(` and the cursor. May exceed `args.len()` for trailing args
    /// — render that as "past the end" (no arg highlighted).
    pub active: usize,
}

/// Resolve the identifier at `cursor` to a [`HoverInfo`] from the stdlib.
/// Returns `None` if there's no identifier at the cursor or the engine
/// doesn't know about it (user-defined idents, datasets, etc.).
#[must_use]
pub fn resolve_function_at(text: &str, cursor: usize) -> Option<HoverInfo> {
    let label = ident_at(text, cursor)?;
    let info = function_info(&label)?;
    Some(to_hover(info))
}

/// Walk back from `cursor` to find an unmatched `(`. Returns the function
/// being called and the active arg index. Returns `None` when:
///
///   * no open paren is reachable without crossing a balanced `(`/`)` pair,
///   * the identifier before the paren is unknown to `function_info`,
///   * the cursor is inside an unclosed `"…"` string literal.
#[must_use]
pub fn find_call_context(text: &str, cursor: usize) -> Option<SigHelp> {
    let bytes = text.as_bytes();
    let cursor = cursor.min(bytes.len());

    // Reject when the cursor is inside an open string literal — sig help
    // is meaningless there.
    if cursor_in_string(bytes, cursor) {
        return None;
    }

    // Scan back honouring nested `()` and skipping `"…"` / `` `…` ``.
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut open_paren: Option<usize> = None;
    let mut i = cursor;
    while i > 0 {
        let b = bytes[i - 1];
        match b {
            b')' => depth += 1,
            b'(' => {
                if depth == 0 {
                    open_paren = Some(i - 1);
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => commas += 1,
            b'"' => {
                // Skip backward over the body. The opener is the next
                // unescaped `"` to the left.
                i -= 1;
                while i > 0 {
                    if bytes[i - 1] == b'"' && !is_byte_escaped(bytes, i - 1) {
                        i -= 1;
                        break;
                    }
                    i -= 1;
                }
                continue;
            }
            b'`' => {
                i -= 1;
                while i > 0 && bytes[i - 1] != b'`' {
                    i -= 1;
                }
                i = i.saturating_sub(1);
                continue;
            }
            _ => {}
        }
        i -= 1;
    }
    let paren_pos = open_paren?;

    // Identifier immediately before `(` (skipping whitespace).
    let mut p = paren_pos;
    while p > 0 && bytes[p - 1].is_ascii_whitespace() {
        p -= 1;
    }
    let label = ident_ending_at(bytes, p)?;
    let info = function_info(&label)?;
    Some(SigHelp {
        label: info.label,
        args: info.args.into_iter().map(arg_pair).collect(),
        active: commas,
    })
}

fn to_hover(info: FunctionInfo) -> HoverInfo {
    HoverInfo {
        label: info.label,
        args: info.args.into_iter().map(arg_pair).collect(),
        info: info.info,
    }
}

fn arg_pair(a: CompletionArg) -> (String, String) {
    (a.name.to_string(), a.typ.to_string())
}

/// Extract the identifier at `cursor`. Idents may include `::` separators
/// so `prom::rate` is one symbol.
fn ident_at(text: &str, cursor: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let cursor = cursor.min(bytes.len());

    // Expand left as long as the byte is part of an ident or `::`.
    let mut start = cursor;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    // Expand right.
    let mut end = cursor;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let raw = std::str::from_utf8(&bytes[start..end]).ok()?;
    // Strip a stray leading `:` left over when the cursor sits right after
    // a leading `:` that wasn't part of a `::` pair (defensive).
    let trimmed = raw.trim_matches(':');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn ident_ending_at(bytes: &[u8], end: usize) -> Option<String> {
    let mut j = end;
    while j > 0 && is_ident_byte(bytes[j - 1]) {
        j -= 1;
    }
    if j == end {
        return None;
    }
    let s = std::str::from_utf8(&bytes[j..end]).ok()?;
    let trimmed = s.trim_matches(':');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b':'
}

/// Cheap check: count unescaped `"` chars from line start to `cursor`. If
/// odd, the cursor is inside a string. Scans only the current line so
/// multi-line buffers don't accidentally inherit a quote from above.
fn cursor_in_string(bytes: &[u8], cursor: usize) -> bool {
    let line_start = bytes[..cursor]
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let mut count = 0u32;
    let mut i = line_start;
    while i < cursor {
        if bytes[i] == b'"' && !is_byte_escaped(bytes, i) {
            count += 1;
        }
        i += 1;
    }
    count % 2 == 1
}

fn is_byte_escaped(bytes: &[u8], at: usize) -> bool {
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

    #[test]
    fn resolve_function_at_returns_avg() {
        let q = "home:temp | align to 1m using avg";
        let cursor = q.len(); // cursor at end of `avg`
        let info = resolve_function_at(q, cursor).expect("avg should resolve");
        assert_eq!(info.label, "avg");
        assert!(!info.args.is_empty() || info.info.is_some(), "{info:?}");
    }

    #[test]
    fn resolve_function_at_qualified() {
        let q = "home:temp | map prom::rate";
        let cursor = q.len();
        let info = resolve_function_at(q, cursor).expect("prom::rate should resolve");
        assert_eq!(info.label, "prom::rate");
    }

    #[test]
    fn resolve_function_at_cursor_in_middle_of_ident() {
        let q = "avg";
        let info = resolve_function_at(q, 1).expect("cursor inside ident still resolves");
        assert_eq!(info.label, "avg");
    }

    #[test]
    fn resolve_function_at_unknown_returns_none() {
        assert!(resolve_function_at("home:temp", 4).is_none());
    }

    #[test]
    fn find_call_context_first_arg() {
        let q = "home:temp | bucket to 1m using histogram(0.99";
        let ctx = find_call_context(q, q.len()).expect("inside call");
        assert_eq!(ctx.label, "histogram");
        assert_eq!(ctx.active, 0);
    }

    #[test]
    fn find_call_context_second_arg() {
        let q = "home:temp | bucket to 1m using histogram(0.99, ";
        let ctx = find_call_context(q, q.len()).expect("inside call");
        assert_eq!(ctx.label, "histogram");
        assert_eq!(ctx.active, 1);
    }

    #[test]
    fn find_call_context_skips_string_commas() {
        // Comma inside the string literal must not bump `active`. `histogram`
        // is a real bucket function so the lookup succeeds.
        let q = "home:temp | bucket to 1m using histogram(\"a, b\", ";
        let ctx = find_call_context(q, q.len()).expect("inside call");
        assert_eq!(ctx.label, "histogram");
        assert_eq!(ctx.active, 1);
    }

    #[test]
    fn find_call_context_handles_nested_parens() {
        // The inner `(...)` is a balanced subcall and its comma must not
        // count toward the outer call's active arg.
        let q = "home:temp | bucket to 1m using histogram(rate(0.5), ";
        let ctx = find_call_context(q, q.len()).expect("inside outer call");
        assert_eq!(ctx.label, "histogram");
        assert_eq!(ctx.active, 1);
    }

    #[test]
    fn find_call_context_returns_none_outside_call() {
        assert!(find_call_context("home:temp | align to 1m using avg", 33).is_none());
    }

    #[test]
    fn find_call_context_returns_none_inside_string() {
        let q = "home:temp | where x == \"hello, world";
        assert!(find_call_context(q, q.len()).is_none());
    }
}
