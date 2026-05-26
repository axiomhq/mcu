//! legend tests.

use super::*;

#[test]
fn legend_jk_clamps_at_edges() {
    // Vim defaults: j/k stop at the last/first item, no wrap.
    let mut app = app_with_series(3);
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.legend.selected, 1);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.legend.selected, 2);
    // Clamps at the last item.
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.legend.selected, 2);
    // Walk back to the top, then clamp there too.
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.legend.selected, 1);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.legend.selected, 0);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.legend.selected, 0);
}

#[test]
#[allow(non_snake_case)] // mirrors vim keystrokes `gg` / `G`
fn legend_gg_and_G_jump_to_edges() {
    let mut app = app_with_series(4);
    app.set_focus(Pane::Legend);
    // `G` jumps to the last item in one keystroke.
    app.on_key(key(KeyCode::Char('G')));
    assert_eq!(app.legend.selected, 3);
    // A single `g` only arms the jump; the cursor doesn't move yet.
    app.on_key(key(KeyCode::Char('g')));
    assert_eq!(app.legend.selected, 3);
    // Second `g` fires.
    app.on_key(key(KeyCode::Char('g')));
    assert_eq!(app.legend.selected, 0);
    // A non-`g` key between the two cancels the pending jump.
    app.on_key(key(KeyCode::Char('G')));
    app.on_key(key(KeyCode::Char('g')));
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('g')));
    assert_eq!(app.legend.selected, 3); // didn't jump to 0
}
#[test]
fn legend_space_toggles_visibility() {
    let mut app = app_with_series(2);
    app.set_focus(Pane::Legend);
    app.legend.selected = 1;
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend.hidden, vec![false, true]);
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend.hidden, vec![false, false]);
}
#[test]
fn legend_a_smart_toggles_all() {
    let mut app = app_with_series(3);
    app.set_focus(Pane::Legend);
    // All visible — `a` hides all.
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.legend.hidden, vec![true, true, true]);
    // Any hidden — `a` shows all.
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.legend.hidden, vec![false, false, false]);
    // Mixed — `a` shows all (since any are hidden).
    app.legend.hidden = vec![true, false, false];
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.legend.hidden, vec![false, false, false]);
}
#[test]
fn legend_details_jk_moves_cursor_and_space_toggles_label_tag() {
    let mut app = app_with_series(1);
    // Replace the synthesised single-tag series with one carrying
    // three tags so we can navigate.
    app.series[0].tags = vec![
        ("dc".to_string(), "us-east".into()),
        ("host".to_string(), "db-01".into()),
        ("region".to_string(), "us".into()),
    ];
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    assert!(app.legend.details_visible);
    assert_eq!(app.legend.details_cursor, 0);
    // Move down to `host`.
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.legend.details_cursor, 1);
    // Toggle host as a label tag.
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend.label_tags, vec!["host".to_string()]);
    let summary = crate::chart::summarize_legend(&app.series, &app.legend.label_tags);
    assert_eq!(summary.rows, vec!["db-01".to_string()]);
    // Move down to `region` and toggle.
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(
        app.legend.label_tags,
        vec!["host".to_string(), "region".to_string()]
    );
    let summary = crate::chart::summarize_legend(&app.series, &app.legend.label_tags);
    assert_eq!(summary.rows, vec!["db-01, us".to_string()]);
    // Untoggle host: cursor is on `region` (idx 2), `k` moves to `host` (1).
    app.on_key(key(KeyCode::Char('k')));
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend.label_tags, vec!["region".to_string()]);
    // Esc closes the modal without leaving the legend.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.legend.details_visible);
    assert_eq!(app.focus, Pane::Legend);
}
#[test]
fn legend_details_picker_reads_focused_dashboard_tile_series() {
    // Regression: opening the `e` tag picker in Grid view used
    // to render `app.series` (the demo sin(x)/(no tags)). It
    // must read the focused tile's series from `tile_results`
    // instead, and toggles must persist `legend_label_tags`.
    let mut app = app_with_series(1); // editor has sin(x) demo
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Inject a faux tile result for `tl` with two tagged series.
    let tile_series = vec![
        crate::chart::Series {
            name: "top-left {h1,us}".into(),
            tags: vec![("host".into(), "h1".into()), ("region".into(), "us".into())],
            points: vec![],
            color: crate::chart::color_for(0),
        },
        crate::chart::Series {
            name: "top-left {h2,us}".into(),
            tags: vec![("host".into(), "h2".into()), ("region".into(), "us".into())],
            points: vec![],
            color: crate::chart::color_for(1),
        },
    ];
    app.tile_results.insert(
        "tl".into(),
        TileQueryResult {
            busy: false,
            series: tile_series,
            error: None,
            trace_id: None,
        },
    );
    assert_eq!(app.selected_chart_idx, 0); // `tl`

    // active_legend_series should now point at the tile, not the editor.
    let active = app.active_legend_series();
    assert_eq!(active.len(), 2);
    assert_eq!(active[0].tags.len(), 2);

    // Move into the Legend pane and open the picker.
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    assert!(app.legend.details_visible);
    assert_eq!(app.legend.details_cursor, 0);

    // Toggle `host` (cursor on row 0) — expect it to land in
    // legend_label_tags.
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend.label_tags, vec!["host".to_string()]);

    // Move to row 1 and toggle `region` too.
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.legend.details_cursor, 1);
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(
        app.legend.label_tags,
        vec!["host".to_string(), "region".to_string()]
    );

    // `summarize_legend` of the active slice with the picked
    // tags now produces clean per-series labels.
    let summary =
        crate::chart::summarize_legend(app.active_legend_series(), &app.legend.label_tags);
    assert_eq!(
        summary.rows,
        vec!["h1, us".to_string(), "h2, us".to_string()]
    );
}
#[test]
fn legend_e_opens_picker_for_dashboard_tile_even_when_editor_empty() {
    // The `e` opener used to gate on `!self.series.is_empty()`;
    // in Grid view the editor may be empty/demo but the focused
    // tile has data, so the gate must read `active_legend_series`.
    let mut app = test_app();
    app.series.clear();
    app.legend.hidden.clear();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.tile_results.insert(
        "tl".into(),
        TileQueryResult {
            busy: false,
            series: vec![crate::chart::Series {
                name: "top-left {h1}".into(),
                tags: vec![("host".into(), "h1".into())],
                points: vec![],
                color: crate::chart::color_for(0),
            }],
            error: None,
            trace_id: None,
        },
    );
    // set_focus(Legend) refuses on empty editor series; route
    // through Pane mutation directly for this test.
    app.focus = Pane::Legend;
    app.on_key(key(KeyCode::Char('e')));
    assert!(app.legend.details_visible);
}
#[test]
fn legend_label_tags_swap_per_tile_when_switching_focus() {
    // Per-tile state: editing tags on tile A and switching to
    // tile B must show B's (empty) selection, not A's. Returning
    // to A restores its selection from cache.
    let mut app = app_with_series(1);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Tile A (tl, idx 0): two tagged series.
    let series_a = vec![crate::chart::Series {
        name: "top-left {h1}".into(),
        tags: vec![("host".into(), "h1".into()), ("region".into(), "us".into())],
        points: vec![],
        color: crate::chart::color_for(0),
    }];
    let series_b = vec![crate::chart::Series {
        name: "top-right {e1}".into(),
        tags: vec![("env".into(), "prod".into()), ("zone".into(), "a".into())],
        points: vec![],
        color: crate::chart::color_for(0),
    }];
    app.tile_results.insert(
        "tl".into(),
        TileQueryResult {
            busy: false,
            series: series_a,
            error: None,
            trace_id: None,
        },
    );
    app.tile_results.insert(
        "tr".into(),
        TileQueryResult {
            busy: false,
            series: series_b,
            error: None,
            trace_id: None,
        },
    );
    // Pick `host` on tile A via the picker (cursor starts at 0).
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend.label_tags, vec!["host".to_string()]);
    app.on_key(key(KeyCode::Esc));

    // Switch to tile B — its (env, zone) tags shouldn't inherit
    // A's `host` selection.
    app.set_focus(Pane::Dashboard);
    app.move_dashboard_selection(1);
    assert_eq!(app.selected_chart_idx, 1);
    assert!(
        app.legend.label_tags.is_empty(),
        "expected empty tag selection for tile B, got {:?}",
        app.legend.label_tags
    );

    // Pick `env` on tile B.
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend.label_tags, vec!["env".to_string()]);
    app.on_key(key(KeyCode::Esc));

    // Back to A — must restore the previously-picked `host`.
    app.set_focus(Pane::Dashboard);
    app.move_dashboard_selection(-1);
    assert_eq!(app.selected_chart_idx, 0);
    assert_eq!(app.legend.label_tags, vec!["host".to_string()]);

    // And forward to B again — `env` is still set.
    app.move_dashboard_selection(1);
    assert_eq!(app.legend.label_tags, vec!["env".to_string()]);
}
#[test]
fn legend_label_falls_back_when_tag_missing() {
    let mut app = app_with_series(1);
    app.series[0].tags = vec![("region".to_string(), "us".into())];
    app.legend.label_tags = vec!["host".to_string()];
    // No host tag — fall back to the series.name so the row is
    // never blank.
    let summary = crate::chart::summarize_legend(&app.series, &app.legend.label_tags);
    assert_eq!(summary.rows, vec![app.series[0].name.clone()]);
}
#[test]
fn legend_e_opens_details() {
    let mut app = app_with_series(1);
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    assert!(app.legend.details_visible);
    // Esc dismisses.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.legend.details_visible);
    // Still focused on the legend.
    assert_eq!(app.focus, Pane::Legend);
}
#[test]
fn legend_esc_returns_to_editor() {
    let mut app = app_with_series(1);
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Pane::Editor);
}
#[test]
fn legend_h_also_returns_to_editor() {
    let mut app = app_with_series(1);
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('h')));
    assert_eq!(app.focus, Pane::Editor);
}
#[test]
fn legend_q_is_a_noop() {
    // `q` in panes is no longer quit — `:q` is the only quit path.
    let mut app = app_with_series(1);
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('q')));
    assert!(!app.should_quit);
    assert_eq!(app.focus, Pane::Legend);
}
#[test]
fn legend_help_dismiss_does_not_change_focus() {
    let mut app = app_with_series(1);
    app.set_focus(Pane::Legend);
    app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
    assert!(app.help.visible);
    // Esc dismisses the help modal but must not move focus to Editor.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.help.visible);
    assert_eq!(app.focus, Pane::Legend);
}
