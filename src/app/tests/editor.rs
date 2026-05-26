//! editor tests.

use super::*;

#[test]
fn starts_in_normal_mode() {
    let app = test_app();
    assert_eq!(app.mode, Mode::Normal);
    assert!(!app.should_quit);
    assert!(!app.completions.visible);
}
#[test]
fn enter_in_normal_triggers_query() {
    let mut app = test_app();
    app.on_key(key(KeyCode::Enter));
    assert!(
        app.status.contains("running") || app.status.contains("error"),
        "unexpected status: {}",
        app.status
    );
}
#[test]
fn default_registry_contains_interval() {
    let app = test_app();
    assert!(
        app.params.system.iter().any(|p| p.name == "__interval"),
        "system_params: {:?}",
        app.params.system
    );
}
#[test]
fn enter_in_normal_mode_runs_query() {
    // `r` is unbound now (collides with vim's replace-char). `<Enter>`
    // is the single-key way to (re)run; the previous `r` test became
    // misleading and was renamed/updated to track <Enter> instead.
    let mut app = test_app();
    app.on_key(key(KeyCode::Enter));
    assert!(
        app.status.contains("running") || app.status.contains("error"),
        "unexpected status: {}",
        app.status
    );
}

#[test]
fn r_then_char_replaces_char_under_cursor() {
    let mut app = test_app();
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "abc");
    app.on_key(key(KeyCode::Esc));
    // After Esc the cursor is past `c` (col 3); step back so we're on
    // a valid char, then `r x` swaps it.
    app.on_key(key(KeyCode::Char('h')));
    app.on_key(key(KeyCode::Char('r')));
    app.on_key(key(KeyCode::Char('x')));
    assert_eq!(app.editor.lines(), &["abx".to_string()]);
}

#[test]
fn r_with_count_replaces_count_chars() {
    // `3rz` on "abcdef" with cursor at the start rewrites the first
    // three chars to z's and leaves the cursor on the last replaced
    // char (vim semantics).
    let mut app = test_app();
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "abcdef");
    app.on_key(key(KeyCode::Esc));
    // Move cursor to start of line.
    app.on_key(key(KeyCode::Char('0')));
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('r')));
    app.on_key(key(KeyCode::Char('z')));
    assert_eq!(app.editor.lines(), &["zzzdef".to_string()]);
    assert_eq!(app.editor.cursor(), (0, 2));
}

#[test]
fn r_then_esc_cancels() {
    let mut app = test_app();
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "abc");
    app.on_key(key(KeyCode::Esc));
    let before = app.editor.lines().to_vec();
    app.on_key(key(KeyCode::Char('r')));
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.editor.lines(), &before[..]);
}

#[test]
fn r_past_end_of_line_is_a_no_op() {
    // Vim refuses `5rx` on a 3-char line; we surface a status message
    // and leave the buffer untouched.
    let mut app = test_app();
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "ab");
    app.on_key(key(KeyCode::Esc));
    app.on_key(key(KeyCode::Char('0')));
    app.on_key(key(KeyCode::Char('5')));
    app.on_key(key(KeyCode::Char('r')));
    app.on_key(key(KeyCode::Char('x')));
    assert_eq!(app.editor.lines(), &["ab".to_string()]);
}
#[test]
fn esc_in_normal_mode_dismisses_error_overlay() {
    let mut app = test_app();
    app.set_error("datasets error: HTTP 500\nbody: oops".to_string());
    assert!(app.last_error.is_some());
    app.on_key(key(KeyCode::Esc));
    assert!(app.last_error.is_none());
    assert_eq!(app.status, "error dismissed");
}
#[test]
fn enter_accepts_selected_completion_when_popup_visible() {
    let mut app = test_app();
    seed_cache(&app);
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "ho");
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Enter));
    assert!(!app.completions.visible);
    assert_eq!(app.editor.lines(), &["home".to_string()]);
}
#[test]
fn esc_dismisses_popup_before_leaving_insert() {
    let mut app = test_app();
    seed_cache(&app);
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "ho");
    app.on_key(key(KeyCode::Tab));
    assert!(app.completions.visible);
    app.on_key(key(KeyCode::Esc));
    assert!(!app.completions.visible);
    assert_eq!(app.mode, Mode::Insert);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
}
#[test]
fn typing_refreshes_visible_popup_items() {
    let mut app = test_app();
    seed_cache(&app);
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "home:temp | align to 1m using ");
    app.on_key(key(KeyCode::Tab));
    assert_eq!(app.completions.kind_label, "align fn");
    let count_all = app.completions.items.len();
    assert!(count_all >= 2, "got {:?}", app.completions.items);
    type_text(&mut app, "a");
    // "a" should narrow to functions starting with 'a' (avg).
    assert!(
        app.completions
            .items
            .iter()
            .all(|i| i.label.starts_with("a")),
        "got {:?}",
        app.completions.items
    );
    let labels: Vec<&str> = app
        .completions
        .items
        .iter()
        .map(|i| i.label.as_str())
        .collect();
    assert!(labels.contains(&"avg"), "got {labels:?}");
}
#[test]
fn recompute_picks_up_syntax_error() {
    let mut app = test_app();
    set_buffer(&mut app, "`home`:* | align to 1m");
    let err = app
        .diagnostics
        .iter()
        .find(|d| d.severity.is_error())
        .expect("expected an error diagnostic");
    assert_eq!(err.line, 1);
    assert!(err.byte_offset > 0);
}
#[test]
fn k_opens_hover_for_known_function() {
    let mut app = test_app();
    set_buffer(&mut app, "home:temp | align to 1m using avg");
    // Cursor at end — sits on `avg`.
    app.editor.move_cursor(tui_textarea::CursorMove::End);
    app.on_key(key(KeyCode::Char('K')));
    let hover = app.hover.as_ref().expect("hover should be set");
    assert_eq!(hover.label, "avg");
}
#[test]
fn k_unknown_symbol_sets_status() {
    let mut app = test_app();
    set_buffer(&mut app, "home:temp");
    // Cursor on `home` — not a stdlib function.
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('K')));
    assert!(app.hover.is_none());
    assert!(app.status.contains("no docs"), "status: {}", app.status);
}
#[test]
fn next_normal_key_dismisses_hover() {
    let mut app = test_app();
    set_buffer(&mut app, "home:temp | align to 1m using avg");
    app.editor.move_cursor(tui_textarea::CursorMove::End);
    app.on_key(key(KeyCode::Char('K')));
    assert!(app.hover.is_some());
    // Any other key clears it.
    app.on_key(key(KeyCode::Char('h')));
    assert!(app.hover.is_none());
}
#[test]
fn capital_a_appends_at_line_end() {
    let mut app = test_app();
    set_buffer(&mut app, "foo");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
    assert_eq!(app.mode, Mode::Insert);
    assert_eq!(app.editor.cursor(), (0, 3));
}
#[test]
fn lowercase_o_opens_line_below() {
    let mut app = test_app();
    set_buffer(&mut app, "foo\nbar");
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(0, 1));
    app.on_key(key(KeyCode::Char('o')));
    assert_eq!(app.mode, Mode::Insert);
    assert_eq!(buffer(&app), "foo\n\nbar");
    assert_eq!(app.editor.cursor().0, 1);
}
#[test]
fn capital_o_opens_line_above() {
    let mut app = test_app();
    set_buffer(&mut app, "foo\nbar");
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(1, 0));
    app.on_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT));
    assert_eq!(buffer(&app), "foo\n\nbar");
    assert_eq!(app.editor.cursor().0, 1);
}
#[test]
fn gg_jumps_to_top() {
    let mut app = test_app();
    set_buffer(&mut app, "a\nb\nc");
    app.editor.move_cursor(tui_textarea::CursorMove::Bottom);
    app.on_key(key(KeyCode::Char('g')));
    app.on_key(key(KeyCode::Char('g')));
    assert_eq!(app.editor.cursor().0, 0);
}
#[test]
fn capital_g_jumps_to_bottom() {
    let mut app = test_app();
    set_buffer(&mut app, "a\nb\nc");
    app.editor.move_cursor(tui_textarea::CursorMove::Top);
    app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
    assert_eq!(app.editor.cursor().0, 2);
}
#[test]
fn dw_deletes_word_with_trailing_space() {
    let mut app = test_app();
    set_buffer(&mut app, "foo bar baz");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(buffer(&app), "bar baz");
}
#[test]
fn cw_stops_at_word_end_and_enters_insert() {
    let mut app = test_app();
    set_buffer(&mut app, "foo bar");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('c')));
    app.on_key(key(KeyCode::Char('w')));
    // `cw` deletes only the word, not the trailing space.
    assert_eq!(buffer(&app), " bar");
    assert_eq!(app.mode, Mode::Insert);
}
#[test]
fn ciw_replaces_inner_word() {
    let mut app = test_app();
    set_buffer(&mut app, "foo bar baz");
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(0, 5));
    app.on_key(key(KeyCode::Char('c')));
    app.on_key(key(KeyCode::Char('i')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(buffer(&app), "foo  baz");
    assert_eq!(app.mode, Mode::Insert);
}
#[test]
fn di_quote_deletes_string_body() {
    let mut app = test_app();
    set_buffer(&mut app, "where x == \"hello\"");
    app.editor
        .move_cursor(tui_textarea::CursorMove::Jump(0, 13));
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('i')));
    app.on_key(KeyEvent::new(KeyCode::Char('"'), KeyModifiers::SHIFT));
    assert_eq!(buffer(&app), "where x == \"\"");
}
#[test]
fn indent_right_adds_four_spaces() {
    let mut app = test_app();
    set_buffer(&mut app, "foo");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT));
    app.on_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT));
    assert_eq!(buffer(&app), "    foo");
}
#[test]
fn indent_left_removes_leading_spaces() {
    let mut app = test_app();
    set_buffer(&mut app, "    foo");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT));
    app.on_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT));
    assert_eq!(buffer(&app), "foo");
}
#[test]
fn caret_jumps_to_first_non_blank() {
    let mut app = test_app();
    set_buffer(&mut app, "    foo");
    app.editor.move_cursor(tui_textarea::CursorMove::End);
    app.on_key(key(KeyCode::Char('^')));
    assert_eq!(app.editor.cursor(), (0, 4));
}
#[test]
fn fx_jumps_to_next_x_on_line() {
    let mut app = test_app();
    set_buffer(&mut app, "hello world");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('f')));
    app.on_key(key(KeyCode::Char('o')));
    assert_eq!(app.editor.cursor(), (0, 4)); // `o` in `hello`
}
#[test]
fn semicolon_repeats_last_find() {
    let mut app = test_app();
    set_buffer(&mut app, "abc abc abc");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    // `fa` from col 0 (which is `a`) searches strictly forward, lands
    // on the next `a` at col 4.
    app.on_key(key(KeyCode::Char('f')));
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.editor.cursor(), (0, 4));
    app.on_key(key(KeyCode::Char(';')));
    assert_eq!(app.editor.cursor(), (0, 8));
}
#[test]
fn comma_reverses_last_find() {
    let mut app = test_app();
    set_buffer(&mut app, "abc abc abc");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('f')));
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.editor.cursor(), (0, 4));
    // `,` reverses: find `a` backward — lands on col 0.
    app.on_key(key(KeyCode::Char(',')));
    assert_eq!(app.editor.cursor(), (0, 0));
}
#[test]
fn df_deletes_through_target_inclusive() {
    let mut app = test_app();
    set_buffer(&mut app, "hello world");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('f')));
    app.on_key(key(KeyCode::Char('o')));
    // `dfo` deletes `hello` (through the first `o` inclusive).
    assert_eq!(buffer(&app), " world");
}
#[test]
fn dt_stops_before_target() {
    let mut app = test_app();
    set_buffer(&mut app, "hello world");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('t')));
    app.on_key(key(KeyCode::Char('o')));
    // `dto` deletes `hell` (stops before the `o`).
    assert_eq!(buffer(&app), "o world");
}
#[test]
fn dot_repeats_last_change() {
    let mut app = test_app();
    set_buffer(&mut app, "foo bar baz qux");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(buffer(&app), "bar baz qux");
    app.on_key(key(KeyCode::Char('.')));
    assert_eq!(buffer(&app), "baz qux");
}
#[test]
fn visual_d_deletes_selection() {
    let mut app = test_app();
    set_buffer(&mut app, "hello world");
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(0, 6));
    app.on_key(key(KeyCode::Char('v')));
    assert_eq!(app.mode, Mode::Visual);
    // Extend selection to end of `world`.
    for _ in 0..4 {
        app.on_key(key(KeyCode::Char('l')));
    }
    app.on_key(key(KeyCode::Char('d')));
    // 5 chars (w-o-r-l-d) selected inclusively — buffer becomes `hello `.
    assert_eq!(buffer(&app), "hello ");
    assert_eq!(app.mode, Mode::Normal);
}
#[test]
fn visual_line_y_yanks_full_line() {
    let mut app = test_app();
    set_buffer(&mut app, "alpha\nbeta\ngamma");
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(1, 1));
    app.on_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));
    assert_eq!(app.mode, Mode::VisualLine);
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(app.mode, Mode::Normal);
    let yank = app.yank.as_ref().expect("yank populated");
    assert!(yank.linewise);
    assert!(yank.text.contains("beta"));
}
#[test]
fn normal_mode_colon_command_does_not_change_focus() {
    // Sanity: the colon path from Normal mode must not return focus
    // anywhere — it didn't come from a pane.
    let mut app = test_app();
    assert_eq!(app.focus, Pane::Editor);
    app.on_key(key(KeyCode::Char(':')));
    assert_eq!(app.mode, Mode::Command);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Pane::Editor);
}
#[test]
fn toggle_persists_to_cache_via_query_context() {
    let mut app = app_with_series(1);
    app.series[0].tags = vec![("host".to_string(), "db-01".into())];
    app.last_query_context = Some(QueryContext {
        hash: "h1".to_string(),
        dataset: "home".to_string(),
        metric: "temp".to_string(),
    });
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    app.on_key(key(KeyCode::Char(' ')));
    // Cache now has the choice keyed both ways.
    let cache = app.cache.read().unwrap();
    assert_eq!(
        cache.resolve_legend_tags("h1", "home", "temp"),
        vec!["host"]
    );
    // Unknown hash falls back to the dataset/metric entry.
    assert_eq!(
        cache.resolve_legend_tags("different", "home", "temp"),
        vec!["host"]
    );
}
#[test]
fn esc_from_solo_with_dashboard_returns_to_grid() {
    // Zooming a tile lands in Solo with focus on Editor.
    // Pressing Esc in Normal mode should flip back to Grid —
    // mirroring the "back out" intuition vim users have for
    // Esc, and removing the need for `:grid` after every zoom.
    let mut app = app_with_series(2);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.tile_results.insert(
        "tl".into(),
        TileQueryResult {
            busy: false,
            series: vec![crate::chart::Series {
                name: "top-left".into(),
                tags: vec![],
                points: vec![(0.0, 1.0)],
                color: crate::chart::color_for(0),
            }],
            error: None,
            trace_id: None,
        },
    );
    app.zoom_selected_chart();
    assert_eq!(app.view_mode, ViewMode::Solo);
    assert_eq!(app.focus, Pane::Editor);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.view_mode, ViewMode::Grid);
    assert_eq!(app.focus, Pane::Dashboard);
}
#[test]
fn esc_prefers_dismissing_error_over_returning_to_grid() {
    // If there's an active error overlay, Esc dismisses it
    // first; the next Esc returns to grid. Otherwise users
    // would silently lose error context on view switches.
    let mut app = app_with_series(1);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.zoom_selected_chart();
    app.set_error("boom".into());
    assert_eq!(app.view_mode, ViewMode::Solo);
    app.on_key(key(KeyCode::Esc));
    assert!(app.last_error.is_none());
    assert_eq!(app.view_mode, ViewMode::Solo);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.view_mode, ViewMode::Grid);
}
#[test]
fn esc_in_solo_without_dashboard_stays_in_solo() {
    // Plain editing session (no dashboard): Esc has no
    // "back" target, so it falls back to its existing
    // behaviour (dismiss-error / no-op).
    let mut app = app_with_series(1);
    assert_eq!(app.view_mode, ViewMode::Solo);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.view_mode, ViewMode::Solo);
}
#[test]
fn new_query_resets_legend_hidden() {
    let mut app = app_with_series(3);
    app.legend.hidden = vec![true, false, true];
    app.legend.selected = 2;
    // Synthesise a new query result with two series.
    let mut tags = std::collections::HashMap::new();
    tags.insert("k".to_string(), serde_json::Value::from("v"));
    let resp = MetricsQueryResponse {
        series: vec![
            MetricsSeries {
                metric: "m1".to_string(),
                tags: tags.clone(),
                start: 0,
                resolution: 60,
                data: vec![Some(1.0)],
            },
            MetricsSeries {
                metric: "m2".to_string(),
                tags,
                start: 0,
                resolution: 60,
                data: vec![Some(2.0)],
            },
        ],
        trace_id: None,
    };
    app.busy = true;
    app.last_query_id = 42;
    app.handle_event(AppEvent::QueryFinished {
        id: 42,
        result: Ok(resp),
    });
    assert_eq!(app.legend.hidden, vec![false, false]);
    assert_eq!(app.legend.selected, 0);
}
#[test]
fn visual_esc_exits_without_modification() {
    let mut app = test_app();
    set_buffer(&mut app, "foo bar");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('v')));
    app.on_key(key(KeyCode::Char('l')));
    app.on_key(key(KeyCode::Char('l')));
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(buffer(&app), "foo bar");
    assert!(app.visual_anchor.is_none());
}
#[test]
fn default_buffer_is_line_kind() {
    let app = test_app();
    assert_eq!(app.viz_kind, VizKind::Line);
}
#[test]
fn pragma_in_buffer_switches_kind() {
    let mut app = test_app();
    set_buffer(&mut app, "// @viz scatter\nhome:temp | align to 1m");
    assert_eq!(app.viz_kind, VizKind::Scatter);
}
#[test]
fn second_tab_cycles_through_candidates() {
    let mut app = test_app();
    // `d` matches multiple heads, so Tab opens the popup.
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    let first = app.cmdline.buf.clone();
    app.on_key(key(KeyCode::Tab));
    let second = app.cmdline.buf.clone();
    assert_ne!(
        first, second,
        "second Tab should swap in the next candidate"
    );
    assert_eq!(app.cmdline.completions.selected, 1);
}
#[test]
fn shift_tab_cycles_backward() {
    let mut app = test_app();
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    // BackTab from selection 0 wraps to the last candidate.
    app.on_key(key(KeyCode::BackTab));
    let n = app.cmdline.completions.items.len();
    assert_eq!(app.cmdline.completions.selected, n - 1);
}
#[test]
fn typing_a_character_dismisses_completion_popup() {
    let mut app = test_app();
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    assert!(app.cmdline.completions.visible);
    app.on_key(key(KeyCode::Char('v')));
    assert!(!app.cmdline.completions.visible);
}
#[test]
fn enter_accepts_highlighted_completion_without_executing() {
    let mut app = test_app();
    // `d` matches several heads; Tab opens the popup with the top
    // candidate spliced. Enter then accepts the selection + appends
    // a space while staying in Command mode (not executing).
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    assert!(app.cmdline.completions.visible);
    let highlighted = app.cmdline.buf.clone();
    app.on_key(key(KeyCode::Enter));
    assert!(!app.cmdline.completions.visible);
    assert_eq!(app.cmdline.buf, format!("{highlighted} "));
    assert_eq!(app.mode, Mode::Command);
}
#[test]
fn esc_in_cmdline_dismisses_popup_and_command_mode() {
    let mut app = test_app();
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
    assert!(!app.cmdline.completions.visible);
}
#[test]
fn m_enters_move_submode_and_arrow_translates() {
    // Use a single-tile dashboard so there's room to move without
    // colliding with siblings.
    let mut app = test_app();
    let mut r = multi_chart_resource();
    r.dashboard.charts.truncate(1);
    r.dashboard.layout.truncate(1);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(r),
    });
    app.execute_command("grid");
    app.on_key(key(KeyCode::Char('m')));
    assert!(matches!(app.tile_submode, TileSubMode::Move { .. }));
    app.on_key(key(KeyCode::Down));
    let li = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout[0];
    assert_eq!(li.y, Some(1));
    assert!(app.dashboard_dirty);
    // Esc reverts to original.
    app.on_key(key(KeyCode::Esc));
    let li = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout[0];
    assert_eq!(li.y, Some(0));
    assert!(matches!(app.tile_submode, TileSubMode::Idle));
}
#[test]
fn s_enters_resize_submode_and_arrow_grows() {
    let mut app = test_app();
    // Use a single-tile dashboard so grow won't collide.
    let mut r = multi_chart_resource();
    r.dashboard.charts.truncate(1);
    r.dashboard.layout.truncate(1);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(r),
    });
    app.execute_command("grid");
    app.on_key(key(KeyCode::Char('s')));
    assert!(matches!(app.tile_submode, TileSubMode::Resize { .. }));
    // Down arrow grows h by 1.
    app.on_key(key(KeyCode::Down));
    let li = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout[0];
    assert_eq!(li.h, 7);
    assert!(app.dashboard_dirty);
    // Commit with Enter.
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.tile_submode, TileSubMode::Idle));
}
#[test]
fn a_enters_add_pick_and_enter_inserts() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Char('a')));
    assert!(matches!(
        app.tile_submode,
        TileSubMode::PickViz {
            cursor: 0,
            action: PickVizAction::Add
        }
    ));
    // Down once → Bar.
    app.on_key(key(KeyCode::Down));
    // Enter inserts.
    app.on_key(key(KeyCode::Enter));
    let n = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .len();
    assert_eq!(n, 5);
    assert_eq!(app.selected_chart_idx, 4);
    assert!(app.dashboard_dirty);
    assert!(matches!(app.tile_submode, TileSubMode::Idle));
}
#[test]
fn count_then_yank_captures_multiple_tiles() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // 3 then y — yank 3 tiles starting from focused (tl) in row-major.
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('y')));
    let yank = app.tile_yank.as_ref().expect("yank slot");
    assert_eq!(yank.len(), 3);
    assert!(app.status.contains("yanked 3"));
}
#[test]
fn count_then_open_below_inserts_n_tiles_after_one_kind_pick() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('o')));
    app.on_key(key(KeyCode::Enter));
    let n = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .len();
    assert_eq!(n, 7); // 4 + 3
}
#[test]
fn count_then_motion_repeats_navigation() {
    // `2l` from tl (idx 0) → spatial Right to tr (1), spatial Right
    // has no further match, falls back to row-major cycle → bl (2).
    // Demonstrates that the count is honoured by the idle keymap.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Char('2')));
    app.on_key(key(KeyCode::Char('l')));
    assert_eq!(app.selected_chart_idx, 2);
}
