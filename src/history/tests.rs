//! Unit tests for the `:` cmdline history.
//!
//! Coverage strategy: focus on the pure logic (push / walk_back /
//! walk_forward / cap / dedup-and-promote / prefix filter / JSON
//! round-trip / tolerate-corrupt). The on-disk save/load helpers
//! that resolve the platform data dir are exercised indirectly by
//! a round-trip test that bypasses path resolution.

use super::*;

fn h(entries: &[&str]) -> History {
    let mut hist = History::with_cap(500);
    for e in entries {
        hist.push(e);
    }
    hist
}

// --- push -------------------------------------------------------------

#[test]
fn push_trims_whitespace_and_skips_empty() {
    // Empty / whitespace-only entries pollute history; vim drops
    // them. We match that.
    let mut hist = History::with_cap(10);
    hist.push("");
    hist.push("   ");
    hist.push("\t");
    assert!(hist.entries().is_empty(), "empty/blank entries skipped");
}

#[test]
fn push_appends_unique_entries_in_order() {
    let hist = h(&["dashboard ls", "apl", "edit foo.mpl"]);
    assert_eq!(
        hist.entries()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        ["dashboard ls", "apl", "edit foo.mpl"]
    );
}

#[test]
fn push_promotes_duplicate_to_most_recent() {
    // Dedup-and-promote: typing a command that already exists
    // removes the earlier copy so the entry is unique and at
    // the tail. Keeps `:history` readable (no clutter) and
    // matches modern shell behavior.
    let hist = h(&["dashboard ls", "apl", "edit foo.mpl", "apl"]);
    assert_eq!(
        hist.entries()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        ["dashboard ls", "edit foo.mpl", "apl"]
    );
}

#[test]
fn push_caps_oldest_first() {
    // Cap=3, push 5 entries → oldest two dropped.
    let mut hist = History::with_cap(3);
    for cmd in ["a", "b", "c", "d", "e"] {
        hist.push(cmd);
    }
    assert_eq!(
        hist.entries()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        ["c", "d", "e"]
    );
}

#[test]
fn push_marks_dirty_only_on_actual_change() {
    let mut hist = History::with_cap(10);
    assert!(!hist.dirty, "fresh history isn't dirty");
    hist.push("a");
    assert!(hist.dirty, "first push dirties");
    hist.dirty = false;
    hist.push("   "); // blank is skipped
    assert!(!hist.dirty, "skipped push doesn't dirty");
}

// --- walk_back / walk_forward ----------------------------------------
//
// Cursor convention (Vim-compatible):
//   None     = "live buffer" position (below the most-recent entry)
//   Some(i)  = pointing at entries[i] (0-based, 0 = oldest)
//
// walk_back moves toward older entries (lower index); walk_forward
// toward newer (higher index, then `None` once past the most-recent).

#[test]
fn walk_back_from_live_lands_on_most_recent_matching() {
    let hist = h(&["dashboard ls", "apl", "edit foo.mpl"]);
    // No prefix filter → most-recent entry wins.
    let next = hist.walk_back(None, "");
    assert_eq!(next, Some(2));
}

#[test]
fn walk_back_steps_through_history_oldest_last() {
    let hist = h(&["a", "b", "c"]);
    assert_eq!(hist.walk_back(None, ""), Some(2));
    assert_eq!(hist.walk_back(Some(2), ""), Some(1));
    assert_eq!(hist.walk_back(Some(1), ""), Some(0));
    // At the oldest entry, further back is a no-op (returns None
    // meaning "stay where you are" — caller treats unchanged).
    assert_eq!(hist.walk_back(Some(0), ""), None);
}

#[test]
fn walk_back_with_prefix_only_matches_starting_with() {
    let hist = h(&["dashboard ls", "apl", "dash open foo", "edit foo.mpl"]);
    // Filter "dash" from live → most-recent dash* entry.
    assert_eq!(hist.walk_back(None, "dash"), Some(2)); // "dash open foo"
    assert_eq!(hist.walk_back(Some(2), "dash"), Some(0)); // "dashboard ls"
    assert_eq!(hist.walk_back(Some(0), "dash"), None); // no older match
}

#[test]
fn walk_forward_with_prefix_returns_none_past_most_recent() {
    let hist = h(&["dash a", "apl", "dash b"]);
    // Going forward from index 0 ("dash a") with prefix "dash"
    // → next match is index 2 ("dash b").
    assert_eq!(hist.walk_forward(Some(0), "dash"), Some(2));
    // Beyond the most-recent match → None = "restore live buffer".
    assert_eq!(hist.walk_forward(Some(2), "dash"), None);
}

#[test]
fn walk_forward_from_live_is_noop() {
    // Already at the live buffer; nothing newer exists.
    let hist = h(&["a", "b"]);
    // Sentinel: None stays None.
    assert_eq!(hist.walk_forward(None, ""), None);
}

#[test]
fn walk_on_empty_history_returns_none() {
    let hist = History::with_cap(10);
    assert_eq!(hist.walk_back(None, ""), None);
    assert_eq!(hist.walk_back(None, "anything"), None);
    assert_eq!(hist.walk_forward(None, ""), None);
}

// --- serialization ---------------------------------------------------

#[test]
fn json_round_trip() {
    let mut hist = History::with_cap(500);
    hist.push("dashboard ls");
    hist.push("apl");
    let json = hist.to_json();
    let parsed = History::from_json(&json, 500);
    assert_eq!(parsed.entries(), hist.entries());
}

#[test]
fn from_json_tolerates_garbage() {
    // Corrupt file shouldn't kill the process — silently empty.
    let parsed = History::from_json("not json at all", 500);
    assert!(parsed.entries().is_empty());
    let parsed = History::from_json("{\"version\": 99, \"entries\": [\"a\"]}", 500);
    // Unknown version: we accept the entries field if present
    // (forward-compat) so the user doesn't lose history on upgrade
    // from an older format.
    assert_eq!(
        parsed
            .entries()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
}

#[test]
fn from_json_clamps_to_cap() {
    // A file with 1000 entries loaded against a cap-of-3 keeps
    // only the most recent 3.
    let big: Vec<String> = (0..1000).map(|i| format!("cmd{i}")).collect();
    let val = serde_json::json!({"version": 1, "entries": big});
    let parsed = History::from_json(&val.to_string(), 3);
    assert_eq!(parsed.entries().len(), 3);
    assert_eq!(parsed.entries().back().map(String::as_str), Some("cmd999"));
}

// --- disk round trip ------------------------------------------------

#[test]
fn save_then_load_via_tempfile_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.json");

    let mut h = History::load_from(path.clone(), 500);
    assert!(h.entries().is_empty(), "no file yet → empty load");
    h.push("dashboard ls");
    h.push("apl");
    h.push("edit foo.mpl");
    h.save().expect("save");
    assert!(!h.dirty, "save clears dirty flag");
    assert!(path.exists(), "save created the file");

    // Fresh load reads the same entries back in order.
    let h2 = History::load_from(path.clone(), 500);
    assert_eq!(
        h2.entries().iter().map(String::as_str).collect::<Vec<_>>(),
        ["dashboard ls", "apl", "edit foo.mpl"]
    );
}

#[test]
fn save_is_noop_when_path_unset() {
    let mut h = History::in_memory();
    h.push("a");
    // No path → save shouldn't error, shouldn't create anything.
    h.save().expect("noop save");
    // Dirty stays true because we didn't actually persist; the next
    // call to a real `save_to` path would still need to write.
    assert!(h.dirty, "with no path, save is noop but doesn't lie");
}

#[test]
fn save_is_noop_when_not_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.json");
    let mut h = History::load_from(path.clone(), 500);
    h.push("a");
    h.save().expect("first save");
    assert!(!h.dirty);
    // Second save with no changes — file mtime must not change.
    let mtime1 = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    h.save().expect("noop save");
    let mtime2 = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(mtime1, mtime2, "no-op save must not touch the file");
}

#[test]
fn load_from_corrupt_file_yields_empty_but_keeps_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.json");
    std::fs::write(&path, "this is not json").unwrap();
    let mut h = History::load_from(path.clone(), 500);
    assert!(h.entries().is_empty(), "corrupt file → empty");
    // ... but subsequent saves overwrite the bad file rather than
    // refusing to write — this is what stops a single corrupt write
    // from wedging history forever.
    h.push("recovery");
    h.save().expect("save");
    let h2 = History::load_from(path, 500);
    assert_eq!(
        h2.entries().iter().map(String::as_str).collect::<Vec<_>>(),
        ["recovery"]
    );
}
