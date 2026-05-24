//! Tab completion for the `:` Ex-command line.
//!
//! Mirrors vim's wildmenu behaviour at a basic level: Tab on the head
//! completes against the known command vocabulary; once a head is
//! selected, subsequent tokens are completed contextually (e.g.
//! `:dash <Tab>` proposes `save / save! / rm / new`, `:tile add <Tab>`
//! proposes the implemented viz kinds, `:open <Tab>` proposes the
//! cached dashboard uids from `:dashboards`).
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

impl CompletionRequest {
    /// Longest common prefix shared by every candidate. Used by the
    /// first-Tab "fill in what's unambiguous" behaviour.
    pub fn common_prefix(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let mut prefix = self.items[0].as_str();
        for s in &self.items[1..] {
            let mut i = 0;
            for (a, b) in prefix.bytes().zip(s.bytes()) {
                if a == b {
                    i += 1;
                } else {
                    break;
                }
            }
            prefix = &prefix[..i];
        }
        prefix.to_string()
    }
}

/// Full Ex-command vocabulary. Keep alphabetised; the head completer
/// preserves this order so first-tab results are deterministic.
const HEAD_COMMANDS: &[&str] = &[
    "axiom",
    "dash",
    "dashboards",
    "dashinfo",
    "datasets",
    "db",
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
    "range",
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
const DASH_SUBS: &[&str] = &["new", "rm", "save", "save!"];

/// Sub-commands for `:tile`.
const TILE_SUBS: &[&str] = &["add", "inspect", "json", "mv", "rm", "size", "title"];

/// Compute completions for the cmdline at the given char-cursor
/// (mirrors `CmdLine.cursor` which counts chars, not bytes). Returns
/// `None` when no completion source applies for this position.
///
/// Matching policy: case-sensitive prefix. An empty token matches
/// every candidate in its category — which is what users expect right
/// after pressing a space and then Tab.
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

/// Filter `candidates` by `token` (prefix match) and wrap with the
/// splice range. Deduplicates + sorts for stable display order.
fn filter_candidates<I>(candidates: I, token: &str, range: (usize, usize)) -> CompletionRequest
where
    I: Iterator,
    I::Item: Into<String>,
{
    let mut items: Vec<String> = candidates
        .map(Into::into)
        .filter(|s| s.starts_with(token))
        .collect();
    items.sort();
    items.dedup();
    CompletionRequest { items, range }
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
mod tests {
    use super::*;

    fn ctx() -> Context<'static> {
        Context { dashboards: &[] }
    }

    fn ds(uid: &str) -> DashboardSummary {
        DashboardSummary {
            uid: uid.into(),
            id: None,
            updated_at: None,
            updated_by: None,
            version: None,
            dashboard: Default::default(),
        }
    }

    #[test]
    fn head_completion_returns_matching_known_commands() {
        let r = completions_for("d", 1, &ctx()).unwrap();
        assert!(r.items.contains(&"dash".to_string()));
        assert!(r.items.contains(&"dashboards".to_string()));
        // 'q' doesn't have the `d` prefix.
        assert!(!r.items.contains(&"q".to_string()));
    }

    #[test]
    fn head_completion_with_empty_buffer_offers_all_heads() {
        let r = completions_for("", 0, &ctx()).unwrap();
        assert!(r.items.contains(&"quit".to_string()));
        assert!(r.items.contains(&"tile".to_string()));
    }

    #[test]
    fn dash_subcommands_after_head_and_space() {
        let r = completions_for("dash ", 5, &ctx()).unwrap();
        assert_eq!(
            r.items,
            vec![
                "new".to_string(),
                "rm".to_string(),
                "save".to_string(),
                "save!".to_string()
            ]
        );
        // Splice range is empty at the trailing position.
        assert_eq!(r.range, (5, 5));
    }

    #[test]
    fn tile_subcommands_include_json_inspector() {
        let r = completions_for("tile ", 5, &ctx()).unwrap();
        assert!(r.items.contains(&"json".to_string()));
        assert!(r.items.contains(&"inspect".to_string()));
    }

    #[test]
    fn dash_subcommands_filter_by_prefix() {
        let r = completions_for("dash sa", 7, &ctx()).unwrap();
        assert_eq!(r.items, vec!["save".to_string(), "save!".to_string()]);
        assert_eq!(r.range, (5, 7));
    }

    #[test]
    fn tile_add_third_token_completes_viz_kinds() {
        let r = completions_for("tile add ", 9, &ctx()).unwrap();
        assert!(r.items.contains(&"line".to_string()));
        assert!(r.items.contains(&"top_list".to_string()));
    }

    #[test]
    fn tile_add_third_token_filters() {
        let r = completions_for("tile add s", 10, &ctx()).unwrap();
        for want in ["scatter", "statistic", "spacer"] {
            assert!(r.items.contains(&want.to_string()), "missing {want}");
        }
        assert!(!r.items.contains(&"line".to_string()));
    }

    #[test]
    fn viz_command_completes_kinds() {
        let r = completions_for("viz ", 4, &ctx()).unwrap();
        assert!(r.items.contains(&"heatmap".to_string()));
    }

    #[test]
    fn common_prefix_finds_longest_shared_start() {
        let r = CompletionRequest {
            items: vec!["save".to_string(), "save!".to_string()],
            range: (0, 0),
        };
        assert_eq!(r.common_prefix(), "save");
    }

    #[test]
    fn common_prefix_returns_empty_when_no_overlap() {
        let r = CompletionRequest {
            items: vec!["alpha".into(), "beta".into()],
            range: (0, 0),
        };
        assert_eq!(r.common_prefix(), "");
    }

    #[test]
    fn open_completes_against_cached_dashboards() {
        let list = vec![ds("prod-1"), ds("prod-2"), ds("staging")];
        let ctx = Context { dashboards: &list };
        let r = completions_for("open prod", 9, &ctx).unwrap();
        assert_eq!(r.items, vec!["prod-1", "prod-2"]);
    }

    #[test]
    fn dash_rm_completes_uids() {
        let list = vec![ds("only-one")];
        let ctx = Context { dashboards: &list };
        let r = completions_for("dash rm ", 8, &ctx).unwrap();
        assert_eq!(r.items, vec!["only-one"]);
    }

    #[test]
    fn unknown_third_token_returns_none() {
        // `:tile rm <foo>` has no defined completion source.
        let r = completions_for("tile rm something", 17, &ctx());
        assert!(r.is_none());
    }
}
