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
    assert!(r.items.contains(&"dashinfo".to_string()));
    // 'q' doesn't have a `d`.
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
    // Empty token: alphabetical order, full set. `save` was
    // collapsed into `:w` / `:w!` in step 19.
    assert_eq!(
        r.items,
        vec!["ls".to_string(), "new".to_string(), "rm".to_string()]
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
fn dash_subcommands_filter_by_fuzzy_match() {
    // `n` matches `new` (single fuzzy hit).
    let r = completions_for("dash n", 6, &ctx()).unwrap();
    assert_eq!(r.items, vec!["new".to_string()]);
    assert_eq!(r.range, (5, 6));
}

#[test]
fn head_completion_is_fuzzy() {
    // Fuzzy matches non-prefix subsequences: `hp` matches `help`
    // (h_p) but not strict-prefix candidates that lack a `p` after
    // the leading `h`.
    let r = completions_for("hp", 2, &ctx()).unwrap();
    assert!(r.items.contains(&"help".to_string()));
    assert!(!r.items.contains(&"h".to_string()));
}

#[test]
fn head_completion_ranks_prefix_above_scattered() {
    // `da` should rank `dash` / `dashinfo` / `datasets` (prefix matches)
    // ahead of any pure-subsequence match.
    let r = completions_for("da", 2, &ctx()).unwrap();
    assert!(!r.items.is_empty());
    assert!(
        r.items[0].starts_with("da"),
        "prefix match should win first slot, got {:?}",
        r.items
    );
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
