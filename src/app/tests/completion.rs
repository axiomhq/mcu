//! completion tests.

use super::*;

#[test]
fn tab_with_empty_cache_kicks_off_dataset_fetch() {
    let mut app = test_app();
    // No datasets cached; editor is empty.
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    app.on_key(key(KeyCode::Tab));
    assert!(!app.completions.visible);
    // Either the fetch was spawned (status mentions fetching) or config
    // resolution failed (status mentions config error).
    assert!(
        app.status.contains("fetching") || app.status.contains("error"),
        "unexpected status: {}",
        app.status
    );
}
#[test]
fn tab_in_insert_mode_opens_dataset_completions() {
    let mut app = test_app();
    seed_cache(&app);
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "ho");
    app.on_key(key(KeyCode::Tab));
    assert!(app.completions.visible);
    assert_eq!(app.completions.kind_label, "dataset");
    let labels: Vec<&str> = app
        .completions
        .items
        .iter()
        .map(|i| i.label.as_str())
        .collect();
    assert!(labels.contains(&"home"), "got {labels:?}");
    assert!(labels.contains(&"homeassistant-logs"), "got {labels:?}");
}
#[test]
fn tab_with_single_candidate_splices_and_appends_space() {
    let mut app = test_app();
    open_cmdline(&mut app, "sol");
    app.on_key(key(KeyCode::Tab));
    // `:sol` only matches `solo`.
    assert_eq!(app.cmdline.buf, "solo ");
    assert!(!app.cmdline.completions.visible);
}
#[test]
fn tab_with_multiple_candidates_splices_top_score_and_shows_popup() {
    let mut app = test_app();
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    // Fuzzy matching against `d` returns multiple heads; the top-scored
    // one is spliced into the buffer and the popup opens so the user
    // can Tab through alternatives.
    assert!(app.cmdline.completions.visible);
    assert!(app.cmdline.completions.items.len() > 1);
    assert_eq!(app.cmdline.buf, app.cmdline.completions.items[0]);
}
#[test]
fn tab_with_partial_token_completes_to_top_score() {
    let mut app = test_app();
    open_cmdline(&mut app, "dash n");
    app.on_key(key(KeyCode::Tab));
    // `n` only matches `new` in the step-19 `:dash` subset (the
    // `save` sub was collapsed into `:w` / `:w!`), so this is a
    // single-candidate completion that splices + appends a space.
    assert_eq!(app.cmdline.buf, "dash new ");
    assert!(!app.cmdline.completions.visible);
}
