//! cmdline tests.

use super::*;
use crate::editor;

#[test]
fn bare_q_does_not_quit_anywhere() {
    // After the unification pass, `:q` is the only quit path; bare `q`
    // is a no-op in Normal mode and an insert in Insert mode.
    let mut app = test_app();
    app.on_key(key(KeyCode::Char('i')));
    app.on_key(key(KeyCode::Char('q')));
    assert!(!app.should_quit, "q in Insert must insert, not quit");
    assert!(app.editor.lines().iter().any(|l| l.contains('q')));

    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
    app.on_key(key(KeyCode::Char('q')));
    assert!(!app.should_quit, "bare q in Normal must not quit");

    // `:q` (dirty) -> E37; `:q!` overrides.
    app.execute_command("q");
    assert!(
        app.last_error.as_deref().is_some_and(|e| e.contains("E37")),
        "expected E37 error, got: {:?}",
        app.last_error
    );
    app.execute_command("q!");
    assert!(app.should_quit);
}
#[test]
fn write_and_open_round_trip_through_disk() {
    use std::fs;
    let mut app = test_app();
    let dir = std::env::temp_dir().join(format!("mcu-test-rt-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("q.mpl");

    // Type a query.
    app.editor = tui_textarea::TextArea::default();
    for c in "home:temp | align to 1m using avg".chars() {
        app.editor.insert_char(c);
    }
    assert!(app.is_dirty());

    // `:w <path>` writes to disk and clears the dirty flag.
    app.execute_command(&format!("w {}", path.display()));
    assert!(!app.is_dirty(), "buffer should be clean after :w");
    assert_eq!(app.current_file.as_deref(), Some(path.as_path()));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "home:temp | align to 1m using avg"
    );

    // Fresh app, `:e <path>` loads the same text back.
    let mut app2 = test_app();
    app2.execute_command(&format!("e {}", path.display()));
    assert_eq!(
        app2.editor.lines().join("\n"),
        "home:temp | align to 1m using avg"
    );
    assert!(!app2.is_dirty());

    let _ = fs::remove_file(&path);
}
#[test]
fn write_without_path_or_current_file_errors() {
    let mut app = test_app();
    app.execute_command("w");
    assert!(
        app.last_error
            .as_deref()
            .is_some_and(|e| e.contains("E32") || e.contains("No file name")),
        "expected E32 error, got: {:?}",
        app.last_error
    );
}
#[test]
fn edit_dirty_buffer_without_bang_refuses() {
    let mut app = test_app();
    for c in "xyz".chars() {
        app.editor.insert_char(c);
    }
    assert!(app.is_dirty());
    app.execute_command("e nonexistent.mpl");
    assert!(
        app.last_error.as_deref().is_some_and(|e| e.contains("E37")),
        "got: {:?}",
        app.last_error
    );
}
#[test]
fn set_error_truncates_status_to_first_line() {
    let mut app = test_app();
    app.set_error("first line\nsecond line goes here".to_string());
    assert_eq!(app.status, "first line");
    assert_eq!(
        app.last_error.as_deref(),
        Some("first line\nsecond line goes here")
    );
}
#[test]
fn quickfix_applies_engine_action() {
    // `duration` (lowercase) is deprecated; engine emits a quick-fix
    // replacing it with `Duration`.
    let mut app = test_app();
    set_buffer(
        &mut app,
        "param $window: duration; home:temp | align to $window using avg",
    );
    let warn = app
        .diagnostics
        .iter()
        .find(|d| d.severity == mpl::Severity::Warning && !d.actions.is_empty())
        .expect("expected a fixable warning")
        .clone();

    // Place cursor at the start of the diagnostic span and open the picker.
    let (row, col) = mpl::byte_offset_to_line_col(&app.query_text(), warn.byte_offset);
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(
        (row - 1) as u16,
        (col - 1) as u16,
    ));
    app.on_key(key(KeyCode::Char('g')));
    app.on_key(key(KeyCode::Char('a')));
    assert!(app.quickfix.visible, "quickfix popup did not open");
    assert!(!app.quickfix.actions.is_empty());

    app.on_key(key(KeyCode::Enter));
    assert!(!app.quickfix.visible, "picker should close after accept");
    assert!(
        app.query_text().contains("Duration"),
        "expected the buffer to contain `Duration`, got: {}",
        app.query_text()
    );
    assert!(
        !app.query_text().contains("duration"),
        "expected the lowercase `duration` to be replaced, got: {}",
        app.query_text()
    );
}
#[test]
fn quickfix_noop_when_no_fixable_diagnostic() {
    let mut app = test_app();
    // Clean query — no fixable diagnostics.
    set_buffer(&mut app, "home:temp | align to 1m using avg");
    app.on_key(key(KeyCode::Char('g')));
    app.on_key(key(KeyCode::Char('a')));
    assert!(!app.quickfix.visible, "picker should not open when no fix");
    assert!(
        app.status.contains("no quick fix"),
        "status: {}",
        app.status
    );
}
#[test]
fn cmd_param_accepts_string_int_float_bool_duration() {
    let mut app = test_app();
    for v in ["\"db-01\"", "42", "5.0", "true", "5m"] {
        app.execute_command(&format!("p host={v}"));
        assert!(
            app.last_error.is_none(),
            "expected `{v}` to be a valid MPL param value; status={:?}",
            app.status
        );
        assert_eq!(app.params.cli.get("host").map(String::as_str), Some(v));
    }
}
#[test]
fn cmd_param_lists() {
    let mut app = test_app();
    app.execute_command("p host=\"db-01\"");
    app.execute_command("param");
    assert!(
        app.status.contains("$host=\"db-01\""),
        "got {:?}",
        app.status
    );
}
#[test]
fn cmd_param_dollar_prefix_canonicalized() {
    let mut app = test_app();
    app.execute_command("p $host=\"db-01\"");
    assert_eq!(
        app.params.cli.get("host").map(String::as_str),
        Some("\"db-01\"")
    );
    assert!(!app.params.cli.contains_key("$host"));
}
#[test]
fn cmd_param_rejects_invalid_mpl() {
    let mut app = test_app();
    // `db-01` is neither an int, a float, a string literal, a bool,
    // a duration, nor a valid ident (`-` isn't an ident char).
    app.execute_command("p host=db-01");
    assert!(app.last_error.is_some(), "expected an error");
    assert!(!app.params.cli.contains_key("host"));
}
#[test]
fn cmd_param_empty_value_clears_one() {
    let mut app = test_app();
    app.params
        .cli
        .insert("host".to_string(), "\"x\"".to_string());
    app.execute_command("p host=");
    assert!(!app.params.cli.contains_key("host"));
}
#[test]
fn cmd_param_bang_clears_all() {
    let mut app = test_app();
    app.params.cli.insert("a".to_string(), "1".to_string());
    app.params.cli.insert("b".to_string(), "2".to_string());
    app.execute_command("p!");
    assert!(app.params.cli.is_empty());
}
#[test]
fn cmd_param_missing_equals_errors() {
    let mut app = test_app();
    app.execute_command("p host");
    assert!(app.last_error.is_some());
}
#[test]
fn help_modal_scrolls_with_j_k_then_dismisses_on_other_key() {
    let mut app = test_app();
    app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
    assert!(app.help.visible);
    assert_eq!(app.help.scroll, 0);
    // j scrolls down by one line; modal stays open.
    app.on_key(key(KeyCode::Char('j')));
    assert!(app.help.visible);
    assert_eq!(app.help.scroll, 1);
    // Ctrl-d jumps 10 lines further.
    app.on_key(ctrl(KeyCode::Char('d')));
    assert_eq!(app.help.scroll, 11);
    // G clamps to the bottom (renderer is responsible for the
    // actual content-aware clamp; app-side we just set the max).
    app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
    assert_eq!(app.help.scroll, u16::MAX);
    // Any other key dismisses the modal.
    app.on_key(key(KeyCode::Char('x')));
    assert!(!app.help.visible);
}
#[test]
fn help_reopens_at_top_after_scrolling() {
    let mut app = test_app();
    app.open_help();
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.help.scroll, 2);
    app.on_key(key(KeyCode::Esc));
    assert!(!app.help.visible);
    // Next open lands at the top regardless of prior scroll state.
    app.open_help();
    assert!(app.help.visible);
    assert_eq!(app.help.scroll, 0);
}
#[test]
fn help_question_mark_works_from_dashboard_pane() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(app.focus, Pane::Dashboard);
    app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
    assert!(app.help.visible);
    // Dismiss — focus stays on the dashboard.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.help.visible);
    assert_eq!(app.focus, Pane::Dashboard);
}
#[test]
fn cmd_viz_inserts_pragma_and_updates_tile() {
    let mut app = test_app();
    set_buffer(&mut app, "home:temp");
    app.cmd_viz(Some("bar"));
    assert_eq!(app.viz_kind, VizKind::Bar);
    assert!(
        buffer(&app).starts_with("// @viz bar"),
        "expected pragma prepended, got: {:?}",
        buffer(&app)
    );
}
#[test]
fn cmd_viz_rewrites_existing_pragma_in_place() {
    let mut app = test_app();
    set_buffer(&mut app, "// @viz line\nhome:temp");
    app.cmd_viz(Some("scatter"));
    let lines = buffer(&app);
    assert!(lines.starts_with("// @viz scatter\n"));
    // No duplicate pragma line:
    assert_eq!(lines.matches("// @viz").count(), 1);
}
#[test]
fn open_file_routes_to_dashboard_mode_for_axiom_json_extension() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prod.axiom.json");
    std::fs::write(&path, FIXTURE_DASHBOARD_JSON).unwrap();
    let mut app = test_app();
    app.open_file(path.clone()).unwrap();
    assert_eq!(app.buffer_mode, BufferMode::Dashboard);
    assert!(app.loaded_dashboard.is_some());
    assert_eq!(
        app.loaded_dashboard.as_ref().unwrap().name_or_unnamed(),
        "prod"
    );
    // The MPL chart was seeded into the editor buffer.
    assert!(app.query_text().contains("http_requests:rate"));
}
#[test]
fn open_file_routes_to_dashboard_mode_via_magic_key_sniff() {
    // Same content under a non-canonical extension still loads as
    // a dashboard via the `"dashboard"` + `"uid"` sniff.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prod.json");
    std::fs::write(&path, FIXTURE_DASHBOARD_JSON).unwrap();
    let mut app = test_app();
    app.open_file(path).unwrap();
    assert_eq!(app.buffer_mode, BufferMode::Dashboard);
}
#[test]
fn open_file_stays_in_mpl_mode_for_plain_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("buffer.mpl");
    std::fs::write(&path, "http_requests:rate\n// @viz line").unwrap();
    let mut app = test_app();
    app.open_file(path).unwrap();
    assert_eq!(app.buffer_mode, BufferMode::Mpl);
    assert!(app.loaded_dashboard.is_none());
}
#[test]
fn write_file_in_dashboard_mode_serialises_loaded_dashboard() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.axiom.json");
    std::fs::write(&path, FIXTURE_DASHBOARD_JSON).unwrap();
    let mut app = test_app();
    app.open_file(path.clone()).unwrap();
    // Stomp the editor buffer to demonstrate it is NOT what's
    // written in dashboard mode.
    app.editor = editor::editor_with_text("this should not appear on disk");
    app.write_file(None).unwrap();
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(!on_disk.contains("should not appear"));
    assert!(on_disk.contains("http_requests:rate"));
}
#[test]
fn open_without_arg_fails_without_prior_pick() {
    let mut app = test_app();
    app.execute_command("open");
    assert!(app.last_error.is_some());
}
#[test]
fn cmd_viz_rejects_unknown_kind() {
    let mut app = test_app();
    set_buffer(&mut app, "home:temp");
    app.cmd_viz(Some("nonsense"));
    assert!(
        app.last_error
            .as_deref()
            .unwrap_or("")
            .contains("unknown viz kind"),
        "expected error overlay; got: {:?}",
        app.last_error
    );
    // Buffer untouched.
    assert_eq!(buffer(&app), "home:temp");
}
#[test]
fn open_below_prompts_for_kind_then_inserts_in_new_row() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // `o` opens the kind picker.
    app.on_key(key(KeyCode::Char('o')));
    assert!(matches!(
        app.tile_submode,
        TileSubMode::PickViz {
            action: PickVizAction::Open {
                above: false,
                remaining: 1
            },
            ..
        }
    ));
    // Enter picks the default kind (Line) → insert one tile below.
    app.on_key(key(KeyCode::Enter));
    let resource = app.loaded_dashboard.as_ref().unwrap();
    assert_eq!(resource.dashboard.charts.len(), 5);
    // The new tile lives below "tl" (focused), so y > 0.
    let new_li = resource.dashboard.layout.last().unwrap();
    assert!(new_li.y.unwrap_or(0) >= 6);
}
#[test]
fn bare_h_runs_navigation_once_with_no_count() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.selected_chart_idx = 1; // tr
    app.on_key(key(KeyCode::Char('h')));
    assert_eq!(app.selected_chart_idx, 0); // tl
}
#[test]
fn write_in_dashboard_mode_without_file_attempts_server_put() {
    // No `current_file`, dashboard mode → `:w` routes to the
    // server PUT path. The test runtime has no live Axiom client,
    // so `fetch_prepare` bails with an error overlay — that's
    // the observable effect we assert on.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert!(app.current_file.is_none());
    app.execute_command("w");
    // Either fetch_prepare set last_error, or status hints at the
    // saving path — both prove we took the server branch.
    let err = app.last_error.as_deref().unwrap_or("");
    let status = app.status.as_str();
    assert!(
        err.contains("axiom") || err.contains("client") || status.contains("saving"),
        "expected server-side save attempt; status={status:?} err={err:?}"
    );
}
#[test]
fn write_in_dashboard_mode_with_path_writes_json_file() {
    use std::io::Write as _;
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.axiom.json");
    let cmd = format!("w {}", path.display());
    app.execute_command(&cmd);
    let body = std::fs::read_to_string(&path).expect("file written");
    assert!(body.contains("\"uid\""), "got: {body}");
    assert!(body.contains("\"dashboard\""));
    // Suppress unused write warning for the tempfile import in
    // earlier rustc versions.
    let _ = writeln!(std::io::sink(), "{}", path.display());
}
