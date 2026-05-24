use super::*;
use crate::axiom::{
    DatasetSummary, MetricInfo, MetricsQueryResponse, MetricsSeries,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn test_app() -> App {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = rt.handle().clone();
    // Leak the runtime so the handle remains valid for the duration of the test.
    Box::leak(Box::new(rt));
    App::with_cache(handle, Cache::in_memory(String::new()))
}

#[test]
fn starts_in_normal_mode() {
    let app = test_app();
    assert_eq!(app.mode, Mode::Normal);
    assert!(!app.should_quit);
    assert!(!app.completions.visible);
}

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
    let dir = std::env::temp_dir().join(format!("metrics-tui-test-rt-{}", std::process::id()));
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
fn dd_deletes_current_line() {
    let mut app = test_app();
    let original_lines = app.editor.lines().len();
    // Editor is focused by default in Normal mode; press d d.
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('d')));
    assert_eq!(app.editor.lines().len(), original_lines - 1);
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
fn query_text_preserves_interval_reference() {
    // The server substitutes `$__interval`; the host must not. Verify
    // the buffer round-trips through `query_text` unchanged.
    let mut app = test_app();
    app.editor = tui_textarea::TextArea::default();
    for c in "home:temp | align to $__interval using avg".chars() {
        app.editor.insert_char(c);
    }
    assert!(
        app.query_text().contains("$__interval"),
        "got: {}",
        app.query_text()
    );
}

#[test]
fn default_registry_contains_interval() {
    let app = test_app();
    assert!(
        app.system_params.iter().any(|p| p.name == "__interval"),
        "system_params: {:?}",
        app.system_params
    );
}

#[test]
fn r_in_normal_mode_runs_query() {
    let mut app = test_app();
    app.on_key(key(KeyCode::Char('r')));
    assert!(
        app.status.contains("running") || app.status.contains("error"),
        "unexpected status: {}",
        app.status
    );
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
fn ctrl_r_redo_path_runs() {
    let mut app = test_app();
    app.on_key(ctrl(KeyCode::Char('r')));
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn datasets_event_updates_status() {
    let mut app = test_app();
    let datasets = vec![
        DatasetSummary {
            name: "k8s".to_string(),
            description: None,
            edge_deployment: None,
            kind: None,
        },
        DatasetSummary {
            name: "logs".to_string(),
            description: None,
            edge_deployment: None,
            kind: None,
        },
    ];
    app.busy = true;
    // Simulate the spawned task having already updated the cache.
    {
        let mut c = app.cache.write().unwrap();
        c.replace_datasets(datasets.clone());
    }
    app.handle_event(AppEvent::DatasetsFetched(Ok(datasets)));
    assert!(!app.busy);
    assert_eq!(app.cache.read().unwrap().dataset_names(), vec!["k8s", "logs"]);
    assert!(app.status.contains("2 dataset"));
}

#[test]
fn metrics_event_updates_status_and_cache_view() {
    let mut app = test_app();
    let mut metrics: BTreeMap<String, MetricInfo> = BTreeMap::new();
    metrics.insert(
        "temp".to_string(),
        MetricInfo {
            kind: Some("Mixed".to_string()),
            temporality: None,
            unit: None,
        },
    );
    app.busy = true;
    {
        let mut c = app.cache.write().unwrap();
        c.replace_metrics("home", metrics.clone());
    }
    app.handle_event(AppEvent::MetricsFetched {
        dataset: "home".to_string(),
        result: Ok(metrics),
    });
    assert!(!app.busy);
    let names = app.cache.read().unwrap().metric_names("home");
    assert_eq!(names, vec!["temp"]);
    assert!(app.status.contains("1 metric"));
}

#[test]
fn query_result_updates_series_and_status() {
    let mut app = test_app();
    let mut tags = std::collections::HashMap::new();
    tags.insert("room".to_string(), "Eingang".to_string());
    let resp = MetricsQueryResponse {
        series: vec![MetricsSeries {
            metric: "temp".to_string(),
            tags,
            start: 1_000,
            resolution: 60,
            data: vec![Some(1.0), None, Some(3.0)],
        }],
        trace_id: None,
    };
    app.busy = true;
    app.last_query_id = 7;
    app.handle_event(AppEvent::QueryFinished {
        id: 7,
        result: Ok(resp),
    });
    assert!(!app.busy);
    assert_eq!(app.series.len(), 1);
    assert_eq!(app.series[0].name, "temp {Eingang}");
    assert_eq!(app.series[0].points.len(), 2);
    assert_eq!(app.series[0].points[0], (1000.0, 1.0));
    assert_eq!(app.series[0].points[1], (1120.0, 3.0));
    assert!(app.status.contains("1 series"));
}

#[test]
fn stale_query_response_is_ignored() {
    let mut app = test_app();
    let prior = app.series.clone();
    app.last_query_id = 5;
    app.busy = true;
    app.handle_event(AppEvent::QueryFinished {
        id: 3,
        result: Ok(MetricsQueryResponse {
            series: vec![MetricsSeries {
                metric: "x".to_string(),
                tags: std::collections::HashMap::new(),
                start: 0,
                resolution: 60,
                data: vec![Some(0.0)],
            }],
            trace_id: None,
        }),
    });
    assert!(app.busy);
    assert_eq!(app.series.len(), prior.len());
}

fn type_text(app: &mut App, s: &str) {
    for c in s.chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
}

/// Seed the cache with two datasets and one metric, so context-aware
/// completion tests have real data to draw on.
fn seed_cache(app: &App) {
    let mut c = app.cache.write().unwrap();
    c.replace_datasets(vec![
        DatasetSummary {
            name: "home".to_string(),
            description: None,
            edge_deployment: None,
            kind: None,
        },
        DatasetSummary {
            name: "homeassistant-logs".to_string(),
            description: None,
            edge_deployment: None,
            kind: None,
        },
    ]);
    let mut metrics: BTreeMap<String, MetricInfo> = BTreeMap::new();
    metrics.insert("temp".to_string(), MetricInfo::default());
    c.replace_metrics("home", metrics);
}

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
fn referenced_tags_extracts_filter_predicates() {
    let mut got =
        referenced_tags("ds:m | where service.name == \"frontend\" and host != \"box-1\"");
    got.sort();
    assert_eq!(got, vec!["host", "service.name"]);
}

#[test]
fn referenced_tags_supports_backticked_names() {
    let got = referenced_tags("ds:m | where `service.name` == \"frontend\"");
    assert_eq!(got, vec!["service.name"]);
}

#[test]
fn referenced_tags_ignores_occurrences_inside_strings() {
    let got = referenced_tags("ds:m | where host == \"weird == not.a.tag\"");
    assert_eq!(got, vec!["host"]);
}

#[test]
fn referenced_tags_picks_up_inequality_operators() {
    let mut got = referenced_tags("ds:m | where a < 1 and b > 2 and c <= 3 and d >= 4");
    got.sort();
    assert_eq!(got, vec!["a", "b", "c", "d"]);
}

#[test]
fn referenced_tags_empty_when_no_filter() {
    assert!(referenced_tags("ds:m | align to 1m using avg").is_empty());
}

#[test]
fn tag_values_fetched_event_updates_status_when_idle() {
    let mut app = test_app();
    app.handle_event(AppEvent::TagValuesFetched {
        dataset: "home".to_string(),
        metric: "temp".to_string(),
        tag: "host".to_string(),
        result: Ok(vec!["a".to_string(), "b".to_string()]),
    });
    assert!(
        app.status.contains("2 value") && app.status.contains("home:temp.host"),
        "status: {}",
        app.status
    );
}

#[test]
fn tag_values_fetched_event_does_not_clobber_busy_status() {
    let mut app = test_app();
    app.busy = true;
    app.status = "running query…".to_string();
    app.handle_event(AppEvent::TagValuesFetched {
        dataset: "home".to_string(),
        metric: "temp".to_string(),
        tag: "host".to_string(),
        result: Ok(vec!["a".to_string()]),
    });
    assert_eq!(app.status, "running query…");
}

#[test]
fn fetch_tag_values_skipped_when_already_cached() {
    let mut app = test_app();
    app.cache.write().unwrap().replace_tag_values(
        "home",
        "temp",
        "host",
        vec!["a".to_string()],
    );
    let before = app.status.clone();
    app.fetch_tag_values("home".to_string(), "temp".to_string(), "host".to_string());
    assert!(!app.busy);
    assert_eq!(app.status, before);
}

#[test]
fn tags_fetched_event_caches_to_disk_layer() {
    let mut app = test_app();
    app.handle_event(AppEvent::TagsFetched {
        dataset: "home".to_string(),
        metric: "temp".to_string(),
        result: Ok(vec!["host".to_string(), "region".to_string()]),
    });
    // The handler doesn't write the cache itself (the spawned task does);
    // it just updates status. Verify it didn't blow up.
    assert!(
        app.status.contains("2 tag") && app.status.contains("home:temp"),
        "status: {}",
        app.status
    );
}

#[test]
fn fetch_tags_skipped_when_already_cached() {
    let mut app = test_app();
    app.cache
        .write()
        .unwrap()
        .replace_tags("home", "temp", vec!["host".to_string()]);
    // The fetch attempt should short-circuit without flipping `busy` or
    // emitting any status change.
    let before = app.status.clone();
    app.fetch_tags("home".to_string(), "temp".to_string());
    assert!(!app.busy, "busy must not be set when cache hit");
    assert_eq!(app.status, before, "status must not change on cache hit");
}

#[test]
fn metric_completion_kicks_in_after_colon() {
    let mut app = test_app();
    seed_cache(&app);
    app.editor = tui_textarea::TextArea::default();
    app.on_key(key(KeyCode::Char('i')));
    type_text(&mut app, "home:t");
    app.on_key(key(KeyCode::Tab));
    assert!(app.completions.visible);
    assert_eq!(app.completions.kind_label, "metric");
    let labels: Vec<&str> = app
        .completions
        .items
        .iter()
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(labels, vec!["temp"]);
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
fn query_error_keeps_previous_series() {
    let mut app = test_app();
    let prior_len = app.series.len();
    app.last_query_id = 1;
    app.busy = true;
    app.handle_event(AppEvent::QueryFinished {
        id: 1,
        result: Err(anyhow::anyhow!("bad query")),
    });
    assert!(!app.busy);
    assert_eq!(app.series.len(), prior_len);
    assert!(app.status.contains("bad query"));
}

// ── 10.3 diagnostics + quick fix ────────────────────────────────────

/// Replace the buffer with `text` without touching `saved_buffer`, then
/// rerun diagnostics like a real keystroke would.
fn set_buffer(app: &mut App, text: &str) {
    app.editor = crate::editor::editor_with_text(text);
    app.recompute_diagnostics();
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
fn run_query_blocked_by_error_diagnostic_keeps_busy_unset() {
    let mut app = test_app();
    set_buffer(&mut app, "`home`:* | align to 1m");
    app.run_query();
    assert!(!app.busy, "run_query must not flip busy on a parse error");
    assert!(
        app.status.to_lowercase().contains("mpl error")
            || app.status.to_lowercase().contains("expected"),
        "status: {}",
        app.status
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

// ── 10.5 hover + signature help ────────────────────────────────────

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
fn sig_help_active_arg_tracks_commas() {
    let mut app = test_app();
    // Cursor right after the first comma inside histogram().
    set_buffer(&mut app, "home:temp | bucket to 1m using histogram(0.5, ");
    app.editor.move_cursor(tui_textarea::CursorMove::End);
    app.recompute_sig_help();
    let sh = app.sig_help.as_ref().expect("sig help should be set");
    assert_eq!(sh.label, "histogram");
    assert_eq!(sh.active, 1);
}

#[test]
fn sig_help_clears_outside_call() {
    let mut app = test_app();
    set_buffer(&mut app, "home:temp | align to 1m using avg");
    app.editor.move_cursor(tui_textarea::CursorMove::End);
    app.recompute_sig_help();
    assert!(app.sig_help.is_none(), "got {:?}", app.sig_help);
}

// ── vim grammar: cursor positioning ─────────────────────────────────

fn buffer(app: &App) -> String {
    app.editor.lines().join("\n")
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

// ── vim grammar: word ops + yank/paste ───────────────────────────

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
fn da_paren_includes_parens() {
    let mut app = test_app();
    set_buffer(&mut app, "f(a, b) | g");
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(0, 3));
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(key(KeyCode::Char('(')));
    assert_eq!(buffer(&app), "f | g");
}

#[test]
fn yy_then_p_duplicates_line() {
    let mut app = test_app();
    set_buffer(&mut app, "alpha\nbeta");
    app.editor.move_cursor(tui_textarea::CursorMove::Top);
    app.on_key(key(KeyCode::Char('y')));
    app.on_key(key(KeyCode::Char('y')));
    app.on_key(key(KeyCode::Char('p')));
    assert_eq!(buffer(&app), "alpha\nalpha\nbeta");
}

#[test]
fn dd_yanks_so_p_pastes_back() {
    let mut app = test_app();
    set_buffer(&mut app, "alpha\nbeta\ngamma");
    app.editor.move_cursor(tui_textarea::CursorMove::Jump(1, 0));
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('d')));
    // Cursor now on `gamma`. `P` puts the yanked `beta` line back
    // above it.
    app.on_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
    assert_eq!(buffer(&app), "alpha\nbeta\ngamma");
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

// ── vim grammar: ^, f, ;, ., visual ─────────────────────────────────

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
fn tx_lands_one_before_target() {
    let mut app = test_app();
    set_buffer(&mut app, "hello world");
    app.editor.move_cursor(tui_textarea::CursorMove::Head);
    app.on_key(key(KeyCode::Char('t')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(app.editor.cursor(), (0, 5)); // one before `w` of world
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

// ── legend pane ──────────────────────────────────────────────

fn app_with_series(n: usize) -> App {
    let mut app = test_app();
    app.series = (0..n)
        .map(|i| crate::chart::Series {
            name: format!("s{i}"),
            tags: vec![("k".to_string(), format!("v{i}"))],
            points: vec![(0.0, i as f64)],
            color: crate::chart::color_for(i),
        })
        .collect();
    app.legend_hidden = vec![false; n];
    app
}

#[test]
fn ctrl_w_w_cycles_focus_editor_legend_params() {
    let mut app = app_with_series(3);
    assert_eq!(app.focus, Pane::Editor);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(app.focus, Pane::Legend);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(app.focus, Pane::Params);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(app.focus, Pane::Editor);
}

#[test]
fn ctrl_w_l_focuses_params_from_editor() {
    let mut app = app_with_series(2);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('l')));
    assert_eq!(app.focus, Pane::Params);
}

// ── params pane ───────────────────────────────────────────────────

fn set_query(app: &mut App, text: &str) {
    // Replace the editor buffer wholesale. `editor_with_text` mirrors
    // what `open_file` uses; good enough for tests.
    app.editor = crate::editor::editor_with_text(text);
}

#[test]
fn param_rows_declared_unset_is_not_set() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "host").expect("row");
    assert_eq!(r.status, crate::params::ParamStatus::NotSet);
    // `TerminalParamType` Display prints tag types lowercase.
    assert_eq!(r.declared_type.as_deref(), Some("string"));
}

#[test]
fn param_rows_declared_optional_unset_is_optional_unset() {
    let mut app = test_app();
    set_query(&mut app, "param $host: Option<string>;\nfoo:bar");
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "host").expect("row");
    assert_eq!(r.status, crate::params::ParamStatus::OptionalUnset);
    assert!(r.optional);
}

#[test]
fn param_rows_typecheck_string_ok() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.cli_params.insert("host".into(), "\"db-01\"".into());
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "host").unwrap();
    assert_eq!(r.status, crate::params::ParamStatus::Ok);
}

#[test]
fn param_rows_typecheck_string_mismatch_when_int_given() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.cli_params.insert("host".into(), "42".into());
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "host").unwrap();
    assert_eq!(r.status, crate::params::ParamStatus::TypeMismatch);
}

#[test]
fn param_rows_duration_ok() {
    let mut app = test_app();
    set_query(&mut app, "param $w: Duration;\nfoo:bar");
    app.cli_params.insert("w".into(), "5m".into());
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "w").unwrap();
    assert_eq!(r.status, crate::params::ParamStatus::Ok);
}

#[test]
fn param_rows_undeclared_provided_is_warning() {
    let mut app = test_app();
    // empty buffer — nothing declared
    app.cli_params.insert("orphan".into(), "\"x\"".into());
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "orphan").unwrap();
    assert_eq!(r.status, crate::params::ParamStatus::NotDeclared);
    assert!(r.declared_type.is_none());
}

#[test]
fn params_pane_jk_navigates() {
    let mut app = test_app();
    set_query(
        &mut app,
        "param $a: string;\nparam $b: string;\nparam $c: string;\nfoo:bar",
    );
    app.set_focus(Pane::Params);
    assert_eq!(app.params_selected, 0);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.params_selected, 1);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.params_selected, 2);
    app.on_key(key(KeyCode::Char('j'))); // wraps
    assert_eq!(app.params_selected, 0);
}

#[test]
fn params_pane_x_clears_selected() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.cli_params.insert("host".into(), "\"db-01\"".into());
    app.set_focus(Pane::Params);
    app.on_key(key(KeyCode::Char('x')));
    assert!(!app.cli_params.contains_key("host"));
}

#[test]
fn params_pane_a_drops_into_command_with_prefix() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.set_focus(Pane::Params);
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.mode, Mode::Command);
    assert_eq!(app.cmdline.buf, "p ");
}

#[test]
fn params_pane_e_prefills_command_with_current_value() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.cli_params.insert("host".into(), "\"db-01\"".into());
    app.set_focus(Pane::Params);
    app.on_key(key(KeyCode::Char('e')));
    assert_eq!(app.mode, Mode::Command);
    assert_eq!(app.cmdline.buf, "p host=\"db-01\"");
}

#[test]
fn params_pane_a_then_enter_returns_focus_to_params() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.set_focus(Pane::Params);
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.mode, Mode::Command);
    // Type a valid `p` body and submit.
    for c in "p host=\"db-01\"".chars().skip(2) {
        // first two chars already in `buf` as the prefill
        app.on_key(key(KeyCode::Char(c)));
    }
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.focus, Pane::Params, "focus should return to Params");
    assert_eq!(
        app.cli_params.get("host").map(String::as_str),
        Some("\"db-01\"")
    );
}

#[test]
fn params_pane_a_then_esc_returns_focus_to_params() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.set_focus(Pane::Params);
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.focus, Pane::Params);
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
fn params_pane_esc_returns_to_editor() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.set_focus(Pane::Params);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Pane::Editor);
}

#[test]
fn ctrl_w_j_from_legend_goes_to_params() {
    let mut app = app_with_series(2);
    app.set_focus(Pane::Legend);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.focus, Pane::Params);
}

#[test]
fn ctrl_w_k_focuses_legend() {
    let mut app = app_with_series(2);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.focus, Pane::Legend);
}

#[test]
fn ctrl_w_l_from_dashboard_goes_to_legend_in_grid() {
    // After loading a dashboard the app lands focused on the
    // Dashboard pane in Grid view; Ctrl-w l should hop right
    // into the Legend column.
    let mut app = app_with_series(2);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(app.focus, Pane::Dashboard);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('l')));
    assert_eq!(app.focus, Pane::Legend);
}

#[test]
fn ctrl_w_h_from_legend_goes_to_dashboard_in_grid() {
    // Mirror of the `l` test: from the Legend column, Ctrl-w h
    // should land back on the Dashboard tile area.
    let mut app = app_with_series(2);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.set_focus(Pane::Legend);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('h')));
    assert_eq!(app.focus, Pane::Dashboard);
}

#[test]
fn ctrl_w_h_from_legend_goes_to_editor_in_solo() {
    // Without a loaded dashboard the previous behaviour stands:
    // Ctrl-w h from Legend falls back to Editor.
    let mut app = app_with_series(2);
    app.set_focus(Pane::Legend);
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('h')));
    assert_eq!(app.focus, Pane::Editor);
}

#[test]
fn ctrl_w_to_legend_refused_when_no_series() {
    let mut app = test_app();
    app.series.clear();
    app.legend_hidden.clear();
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(app.focus, Pane::Editor);
    assert!(app.status.contains("no series"), "got {:?}", app.status);
}

#[test]
fn legend_jk_moves_selection() {
    let mut app = app_with_series(3);
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.legend_selected, 1);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.legend_selected, 2);
    // wraps
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.legend_selected, 0);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.legend_selected, 2);
}

#[test]
fn legend_space_toggles_visibility() {
    let mut app = app_with_series(2);
    app.set_focus(Pane::Legend);
    app.legend_selected = 1;
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend_hidden, vec![false, true]);
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend_hidden, vec![false, false]);
}

#[test]
fn legend_a_smart_toggles_all() {
    let mut app = app_with_series(3);
    app.set_focus(Pane::Legend);
    // All visible — `a` hides all.
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.legend_hidden, vec![true, true, true]);
    // Any hidden — `a` shows all.
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.legend_hidden, vec![false, false, false]);
    // Mixed — `a` shows all (since any are hidden).
    app.legend_hidden = vec![true, false, false];
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.legend_hidden, vec![false, false, false]);
}

#[test]
fn legend_details_jk_moves_cursor_and_space_toggles_label_tag() {
    let mut app = app_with_series(1);
    // Replace the synthesised single-tag series with one carrying
    // three tags so we can navigate.
    app.series[0].tags = vec![
        ("dc".to_string(), "us-east".to_string()),
        ("host".to_string(), "db-01".to_string()),
        ("region".to_string(), "us".to_string()),
    ];
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    assert!(app.legend_details_visible);
    assert_eq!(app.details_cursor, 0);
    // Move down to `host`.
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.details_cursor, 1);
    // Toggle host as a label tag.
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend_label_tags, vec!["host".to_string()]);
    let summary = crate::chart::summarize_legend(&app.series, &app.legend_label_tags);
    assert_eq!(summary.rows, vec!["db-01".to_string()]);
    // Move down to `region` and toggle.
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(
        app.legend_label_tags,
        vec!["host".to_string(), "region".to_string()]
    );
    let summary = crate::chart::summarize_legend(&app.series, &app.legend_label_tags);
    assert_eq!(summary.rows, vec!["db-01, us".to_string()]);
    // Untoggle host: cursor is on `region` (idx 2), `k` moves to `host` (1).
    app.on_key(key(KeyCode::Char('k')));
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend_label_tags, vec!["region".to_string()]);
    // Esc closes the modal without leaving the legend.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.legend_details_visible);
    assert_eq!(app.focus, Pane::Legend);
}

// ── :param command ──────────────────────────────────────────

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
        assert_eq!(app.cli_params.get("host").map(String::as_str), Some(v));
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
        app.cli_params.get("host").map(String::as_str),
        Some("\"db-01\"")
    );
    assert!(!app.cli_params.contains_key("$host"));
}

#[test]
fn cmd_param_rejects_invalid_mpl() {
    let mut app = test_app();
    // `db-01` is neither an int, a float, a string literal, a bool,
    // a duration, nor a valid ident (`-` isn't an ident char).
    app.execute_command("p host=db-01");
    assert!(app.last_error.is_some(), "expected an error");
    assert!(!app.cli_params.contains_key("host"));
}

#[test]
fn cmd_param_empty_value_clears_one() {
    let mut app = test_app();
    app.cli_params
        .insert("host".to_string(), "\"x\"".to_string());
    app.execute_command("p host=");
    assert!(!app.cli_params.contains_key("host"));
}

#[test]
fn cmd_param_bang_clears_all() {
    let mut app = test_app();
    app.cli_params.insert("a".to_string(), "1".to_string());
    app.cli_params.insert("b".to_string(), "2".to_string());
    app.execute_command("p!");
    assert!(app.cli_params.is_empty());
}

#[test]
fn cmd_param_missing_equals_errors() {
    let mut app = test_app();
    app.execute_command("p host");
    assert!(app.last_error.is_some());
}

#[test]
fn query_hash_is_stable_under_whitespace_and_comments() {
    let sp = vec![];
    let a = mpl::query_hash("home:temp | align to 1m using avg", &sp);
    let b = mpl::query_hash("  home:temp    | align to 1m using avg  ", &sp);
    let c = mpl::query_hash("home:temp\n| align to 1m using avg\n", &sp);
    // `//` line comment is stripped by the compile-based hash.
    let d = mpl::query_hash(
        "home:temp // pick the temperature metric\n| align to 1m using avg",
        &sp,
    );
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(a, d);
}

#[test]
fn query_hash_normalizes_time_windows_and_alignment() {
    let sp = vec![];
    // Source-level time windows collapse (MPL syntax: `[1h..]`).
    let a = mpl::query_hash("home:temp[1h..] | align to 1m using avg", &sp);
    let b = mpl::query_hash("home:temp[24h..] | align to 1m using avg", &sp);
    let c = mpl::query_hash("home:temp | align to 1m using avg", &sp);
    assert_eq!(a, b);
    assert_eq!(a, c);
    // Align intervals collapse.
    let d = mpl::query_hash("home:temp | align to 5m using avg", &sp);
    let e = mpl::query_hash("home:temp | align using avg", &sp);
    assert_eq!(c, d);
    assert_eq!(c, e);
    // Structural changes still differ (different aggregator).
    let f = mpl::query_hash("home:temp | align using sum", &sp);
    assert_ne!(c, f);
    // Different metric clearly differs.
    let g = mpl::query_hash("home:cpu | align to 1m using avg", &sp);
    assert_ne!(c, g);
}

#[test]
fn toggle_persists_to_cache_via_query_context() {
    let mut app = app_with_series(1);
    app.series[0].tags = vec![("host".to_string(), "db-01".to_string())];
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
fn finished_query_loads_legend_tags_from_cache() {
    let mut app = test_app();
    // Seed cache with a (dataset, metric) entry (no hash match).
    {
        let mut c = app.cache.write().unwrap();
        c.set_legend_tags(
            "hash-x",
            "home",
            "temp",
            vec!["host".to_string(), "region".to_string()],
        );
    }
    // Drive the run_query path indirectly: set the context as run_query would.
    app.last_query_context = Some(QueryContext {
        hash: "unrelated".to_string(),
        dataset: "home".to_string(),
        metric: "temp".to_string(),
    });
    let mut tags = std::collections::HashMap::new();
    tags.insert("host".to_string(), "db-01".to_string());
    tags.insert("region".to_string(), "us".to_string());
    let resp = MetricsQueryResponse {
        series: vec![MetricsSeries {
            metric: "temp".to_string(),
            tags,
            start: 0,
            resolution: 60,
            data: vec![Some(1.0)],
        }],
        trace_id: None,
    };
    app.busy = true;
    app.last_query_id = 7;
    app.handle_event(AppEvent::QueryFinished {
        id: 7,
        result: Ok(resp),
    });
    // Fallback to (dataset, metric) hit.
    assert_eq!(
        app.legend_label_tags,
        vec!["host".to_string(), "region".to_string()]
    );
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
            tags: vec![
                ("host".into(), "h1".into()),
                ("region".into(), "us".into()),
            ],
            points: vec![],
            color: crate::chart::color_for(0),
        },
        crate::chart::Series {
            name: "top-left {h2,us}".into(),
            tags: vec![
                ("host".into(), "h2".into()),
                ("region".into(), "us".into()),
            ],
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
    assert!(app.legend_details_visible);
    assert_eq!(app.details_cursor, 0);

    // Toggle `host` (cursor on row 0) — expect it to land in
    // legend_label_tags.
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend_label_tags, vec!["host".to_string()]);

    // Move to row 1 and toggle `region` too.
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.details_cursor, 1);
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(
        app.legend_label_tags,
        vec!["host".to_string(), "region".to_string()]
    );

    // `summarize_legend` of the active slice with the picked
    // tags now produces clean per-series labels.
    let summary = crate::chart::summarize_legend(
        app.active_legend_series(),
        &app.legend_label_tags,
    );
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
    app.legend_hidden.clear();
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
    assert!(app.legend_details_visible);
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
fn zoom_promotes_tile_series_into_solo_view_not_sin_demo() {
    // Regression: zooming a tile re-seeded the editor with the
    // tile's MPL but left `app.series` as the sin(x) demo, so
    // the Solo chart pane showed the placeholder until the user
    // hit `:r`. Zoom must promote `tile_results[chart].series`
    // into `app.series` synchronously.
    let mut app = app_with_series(1); // editor starts with sin demo
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let tile_series = vec![crate::chart::Series {
        name: "top-left {h1}".into(),
        tags: vec![("host".into(), "h1".into())],
        points: vec![(0.0, 1.0), (1.0, 2.0)],
        color: crate::chart::color_for(0),
    }];
    app.tile_results.insert(
        "tl".into(),
        TileQueryResult {
            busy: false,
            series: tile_series.clone(),
            error: None,
            trace_id: Some("abc123".into()),
        },
    );
    // tl is selected by default.
    app.zoom_selected_chart();
    assert_eq!(app.view_mode, ViewMode::Solo);
    assert_eq!(app.focus, Pane::Editor);
    // Solo chart now reads tile data, not the sin demo.
    assert_eq!(app.series.len(), 1);
    assert_eq!(app.series[0].tags, tile_series[0].tags);
    assert_eq!(app.series[0].points, tile_series[0].points);
    // Trace id picked up so `:trace` reports the tile's id.
    assert_eq!(app.last_trace_id.as_deref(), Some("abc123"));
    // Legend bookkeeping resized to match new series.
    assert_eq!(app.legend_hidden, vec![false]);
}

#[test]
fn zoom_without_tile_data_clears_series_instead_of_keeping_sin_demo() {
    // No `tile_results` entry (race: zoom before first fetch).
    // Better to clear than to mislead the user with the
    // sin demo labelled as the zoomed tile.
    let mut app = app_with_series(1);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.tile_results.clear();
    app.zoom_selected_chart();
    assert_eq!(app.view_mode, ViewMode::Solo);
    assert!(app.series.is_empty());
    assert!(app.legend_hidden.is_empty());
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
        tags: vec![
            ("host".into(), "h1".into()),
            ("region".into(), "us".into()),
        ],
        points: vec![],
        color: crate::chart::color_for(0),
    }];
    let series_b = vec![crate::chart::Series {
        name: "top-right {e1}".into(),
        tags: vec![
            ("env".into(), "prod".into()),
            ("zone".into(), "a".into()),
        ],
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
    assert_eq!(app.legend_label_tags, vec!["host".to_string()]);
    app.on_key(key(KeyCode::Esc));

    // Switch to tile B — its (env, zone) tags shouldn't inherit
    // A's `host` selection.
    app.set_focus(Pane::Dashboard);
    app.move_dashboard_selection(1);
    assert_eq!(app.selected_chart_idx, 1);
    assert!(
        app.legend_label_tags.is_empty(),
        "expected empty tag selection for tile B, got {:?}",
        app.legend_label_tags
    );

    // Pick `env` on tile B.
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    app.on_key(key(KeyCode::Char(' ')));
    assert_eq!(app.legend_label_tags, vec!["env".to_string()]);
    app.on_key(key(KeyCode::Esc));

    // Back to A — must restore the previously-picked `host`.
    app.set_focus(Pane::Dashboard);
    app.move_dashboard_selection(-1);
    assert_eq!(app.selected_chart_idx, 0);
    assert_eq!(app.legend_label_tags, vec!["host".to_string()]);

    // And forward to B again — `env` is still set.
    app.move_dashboard_selection(1);
    assert_eq!(app.legend_label_tags, vec!["env".to_string()]);
}

#[test]
fn legend_label_falls_back_when_tag_missing() {
    let mut app = app_with_series(1);
    app.series[0].tags = vec![("region".to_string(), "us".to_string())];
    app.legend_label_tags = vec!["host".to_string()];
    // No host tag — fall back to the series.name so the row is
    // never blank.
    let summary = crate::chart::summarize_legend(&app.series, &app.legend_label_tags);
    assert_eq!(summary.rows, vec![app.series[0].name.clone()]);
}

#[test]
fn legend_e_opens_details() {
    let mut app = app_with_series(1);
    app.set_focus(Pane::Legend);
    app.on_key(key(KeyCode::Char('e')));
    assert!(app.legend_details_visible);
    // Esc dismisses.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.legend_details_visible);
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
    assert!(app.help_visible);
    // Esc dismisses the help modal but must not move focus to Editor.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.help_visible);
    assert_eq!(app.focus, Pane::Legend);
}

#[test]
fn help_modal_scrolls_with_j_k_then_dismisses_on_other_key() {
    let mut app = test_app();
    app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
    assert!(app.help_visible);
    assert_eq!(app.help_scroll, 0);
    // j scrolls down by one line; modal stays open.
    app.on_key(key(KeyCode::Char('j')));
    assert!(app.help_visible);
    assert_eq!(app.help_scroll, 1);
    // Ctrl-d jumps 10 lines further.
    app.on_key(ctrl(KeyCode::Char('d')));
    assert_eq!(app.help_scroll, 11);
    // G clamps to the bottom (renderer is responsible for the
    // actual content-aware clamp; app-side we just set the max).
    app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
    assert_eq!(app.help_scroll, u16::MAX);
    // Any other key dismisses the modal.
    app.on_key(key(KeyCode::Char('x')));
    assert!(!app.help_visible);
}

#[test]
fn help_reopens_at_top_after_scrolling() {
    let mut app = test_app();
    app.open_help();
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.help_scroll, 2);
    app.on_key(key(KeyCode::Esc));
    assert!(!app.help_visible);
    // Next open lands at the top regardless of prior scroll state.
    app.open_help();
    assert!(app.help_visible);
    assert_eq!(app.help_scroll, 0);
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
    assert!(app.help_visible);
    // Dismiss — focus stays on the dashboard.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.help_visible);
    assert_eq!(app.focus, Pane::Dashboard);
}

#[test]
fn new_query_resets_legend_hidden() {
    let mut app = app_with_series(3);
    app.legend_hidden = vec![true, false, true];
    app.legend_selected = 2;
    // Synthesise a new query result with two series.
    let mut tags = std::collections::HashMap::new();
    tags.insert("k".to_string(), "v".to_string());
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
    assert_eq!(app.legend_hidden, vec![false, false]);
    assert_eq!(app.legend_selected, 0);
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

// ── viz pragma sync ───────────────────────────────────────────

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
fn removing_pragma_falls_back_to_line() {
    let mut app = test_app();
    set_buffer(&mut app, "// @viz bar\nhome:temp");
    assert_eq!(app.viz_kind, VizKind::Bar);
    set_buffer(&mut app, "home:temp");
    assert_eq!(app.viz_kind, VizKind::Line);
}

#[test]
fn unknown_pragma_kind_pushes_warning_diagnostic() {
    let mut app = test_app();
    set_buffer(&mut app, "// @viz nope\nhome:temp");
    let w = app
        .diagnostics
        .iter()
        .find(|d| matches!(d.severity, mpl::Severity::Warning))
        .expect("expected a warning diagnostic for unknown viz kind");
    assert!(w.message.contains("unknown viz kind"));
    assert_eq!(w.line, 1);
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

// ── dashboards picker ───────────────────────────────────────────────

fn dash(uid: &str, name: &str, desc: Option<&str>) -> DashboardSummary {
    DashboardSummary {
        uid: uid.to_string(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some(name.to_string()),
            description: desc.map(str::to_string),
            ..Default::default()
        },
    }
}

#[test]
fn dashboard_picker_open_sorts_by_name_case_insensitive() {
    let mut p = DashboardPicker::default();
    p.open(vec![
        dash("1", "zoo", None),
        dash("2", "alpha", None),
        dash("3", "Bravo", None),
    ]);
    let names: Vec<_> = p.items.iter().map(|d| d.name()).collect();
    assert_eq!(names, vec!["alpha", "Bravo", "zoo"]);
}

#[test]
fn dashboard_picker_empty_filter_returns_all_indices() {
    let mut p = DashboardPicker::default();
    p.open(vec![dash("1", "a", None), dash("2", "b", None)]);
    assert_eq!(p.filtered_indices(), vec![0, 1]);
}

#[test]
fn dashboard_picker_filter_matches_name_and_description() {
    let mut p = DashboardPicker::default();
    p.open(vec![
        dash("1", "Cluster", None),
        dash("2", "Pods", Some("kubernetes pod lifecycle")),
        dash("3", "Other", None),
    ]);
    p.filter = "kub".to_string();
    let hits: Vec<_> = p
        .filtered_indices()
        .iter()
        .map(|i| p.items[*i].name())
        .collect();
    assert_eq!(hits, vec!["Pods"]);
}

#[test]
fn dashboard_picker_filter_is_case_insensitive() {
    let mut p = DashboardPicker::default();
    p.open(vec![dash("1", "Cluster Overview", None)]);
    p.filter = "CLUSTER".to_string();
    assert_eq!(p.filtered_indices().len(), 1);
}

#[test]
fn dashboard_picker_move_cursor_wraps_within_filtered_set() {
    let mut p = DashboardPicker::default();
    p.open(vec![
        dash("1", "a", None),
        dash("2", "b", None),
        dash("3", "c", None),
    ]);
    assert_eq!(p.move_cursor(1), 1);
    assert_eq!(p.move_cursor(1), 2);
    assert_eq!(p.move_cursor(1), 0); // wraps
    assert_eq!(p.move_cursor(-1), 2); // wraps back
}

#[test]
fn dashboard_picker_hide_clears_filter_and_cursor() {
    let mut p = DashboardPicker::default();
    p.open(vec![dash("1", "a", None)]);
    p.filter = "x".into();
    p.cursor = 5;
    p.visible = true;
    p.hide();
    assert!(!p.visible);
    assert!(p.filter.is_empty());
    assert_eq!(p.cursor, 0);
}

#[test]
fn dashboard_picker_keymap_filters_and_selects() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardsFetched(Ok(vec![
        dash("id-a", "alpha", None),
        dash("id-b", "beta", None),
        dash("id-c", "gamma", None),
    ])));
    assert!(app.dashboards.visible);
    assert_eq!(app.dashboards.items.len(), 3);
    // Type `b` — should filter to `beta` only.
    app.on_key(key(KeyCode::Char('b')));
    let indices = app.dashboards.filtered_indices();
    assert_eq!(indices.len(), 1);
    assert_eq!(app.dashboards.items[indices[0]].name(), "beta");
    // Press Enter — picker closes, uid is remembered.
    app.on_key(key(KeyCode::Enter));
    assert!(!app.dashboards.visible);
    assert_eq!(app.last_picked_dashboard.as_deref(), Some("id-b"));
}

#[test]
fn dashboard_picker_backspace_removes_one_filter_char() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardsFetched(Ok(vec![dash("1", "a", None)])));
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(key(KeyCode::Char('b')));
    assert_eq!(app.dashboards.filter, "ab");
    app.on_key(key(KeyCode::Backspace));
    assert_eq!(app.dashboards.filter, "a");
}

#[test]
fn dashboard_open_event_loads_resource_and_sets_status() {
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u1".into(),
        id: Some("42".into()),
        updated_at: Some("2026-01-01T00:00:00Z".into()),
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("prod overview".into()),
            charts: vec![
                crate::axiom::Chart::TimeSeries(crate::axiom::ChartBase {
                    id: "c1".into(),
                    name: Some("rps".into()),
                    query: None,
                    extras: Default::default(),
                }),
                crate::axiom::Chart::Note(crate::axiom::ChartBase {
                    id: "c2".into(),
                    name: None,
                    query: None,
                    extras: Default::default(),
                }),
            ],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u1".into(),
        result: Ok(resource),
    });
    assert!(app.loaded_dashboard.is_some());
    assert_eq!(app.last_picked_dashboard.as_deref(), Some("u1"));
    assert!(app.status.contains("prod overview"));
    assert!(app.status.contains("2 chart"));
    assert!(!app.busy);
}

#[test]
fn dashboard_open_adopts_internal_dashboard_and_seeds_mpl_buffer() {
    // When the focused chart has MPL, the editor buffer should
    // become `// @viz <kind>\n<mpl>` so the next :r executes it.
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u1".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("prod".into()),
            charts: vec![crate::axiom::Chart::TimeSeries(crate::axiom::ChartBase {
                id: "c1".into(),
                name: Some("rps".into()),
                query: Some(serde_json::json!({ "mpl": "http_requests:rate" })),
                extras: Default::default(),
            })],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u1".into(),
        result: Ok(resource),
    });
    // Focused viz kind reflects the first chart.
    assert_eq!(app.viz_kind, crate::dashboard::VizKind::Line);
    // Buffer seeded with pragma + mpl.
    let buf = app.query_text();
    assert!(buf.contains("// @viz line"), "buffer: {buf:?}");
    assert!(buf.contains("http_requests:rate"), "buffer: {buf:?}");
}

#[test]
fn dashboard_open_with_apl_query_seeds_commented_banner() {
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("d".into()),
            charts: vec![crate::axiom::Chart::Pie(crate::axiom::ChartBase {
                id: "c1".into(),
                name: Some("by-region".into()),
                query: Some(serde_json::json!({
                    "apl": "['logs'] | summarize count() by region"
                })),
                extras: Default::default(),
            })],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    let buf = app.query_text();
    assert!(buf.contains("// @viz pie"));
    assert!(buf.contains("APL query"));
    assert!(buf.contains("['logs']"));
}

#[test]
fn dashboard_open_with_no_charts_leaves_buffer_alone() {
    // Empty dashboard — the from_resource adapter inserts a Note
    // placeholder so focused_tile() doesn't panic, but adopt should
    // not stomp the user's existing buffer.
    let mut app = test_app();
    let original_buf = app.query_text();
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("empty".into()),
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    assert_eq!(app.query_text(), original_buf);
}

// ── dashboard file format (17c) ────────────────────────────────────

/// Minimal but realistic `DashboardResource` JSON: one TimeSeries
/// chart with an MPL query, one layout entry, and a handful of
/// top-level + nested unmodelled fields that must survive a
/// round-trip via `extras`.
const FIXTURE_DASHBOARD_JSON: &str = r#"{
  "uid": "dash-1",
  "id": "42",
  "updatedAt": "2026-05-23T10:00:00Z",
  "dashboard": {
"name": "prod",
"description": "the only one that matters",
"charts": [
  {
    "id": "c1",
    "type": "TimeSeries",
    "name": "rps",
    "query": { "mpl": "http_requests:rate" }
  }
],
"layout": [
  { "i": "c1", "x": 0, "y": 0, "w": 12, "h": 6 }
],
"timeWindowStart": "qr-now-1h",
"timeWindowEnd": "qr-now",
"refreshTime": 60,
"schemaVersion": 2,
"owner": "X-AXIOM-EVERYONE"
  }
}"#;

#[test]
fn dash_new_buffer_builds_timeseries_chart_with_mpl() {
    let doc = build_dashboard_doc_from_buffer("my dash", VizKind::Line, "http_rps:rate");
    assert_eq!(doc.name.as_deref(), Some("my dash"));
    assert_eq!(doc.charts.len(), 1);
    assert_eq!(doc.charts[0].type_str(), "TimeSeries");
    // MPL survives through the opaque query JSON.
    let q = doc.charts[0].base().query.as_ref().unwrap();
    assert_eq!(q["mpl"], "http_rps:rate");
    // Layout placed in the top-left corner spanning full width.
    assert_eq!(doc.layout[0].i, "c1");
    assert_eq!(doc.layout[0].w, 12);
    // Server-required scalars stashed in extras.
    assert_eq!(doc.extras["refreshTime"], 60);
    assert_eq!(doc.extras["schemaVersion"], 2);
    assert_eq!(doc.extras["owner"], "X-AXIOM-EVERYONE");
}

#[test]
fn dash_new_buffer_maps_each_viz_kind_to_a_chart_type() {
    let cases = [
        (VizKind::Line, "TimeSeries"),
        (VizKind::Bar, "TimeSeries"),  // TUI-only → fallback
        (VizKind::Area, "TimeSeries"), // TUI-only → fallback
        (VizKind::Scatter, "Scatter"),
        (VizKind::Pie, "Pie"),
        (VizKind::Heatmap, "Heatmap"),
        (VizKind::Table, "Table"),
        (VizKind::TopList, "TopK"), // rename across the boundary
        (VizKind::Statistic, "Statistic"),
        (VizKind::LogStream, "LogStream"),
        (VizKind::Note, "Note"),
        (VizKind::MonitorList, "TimeSeries"), // TUI-only → fallback
        (VizKind::Spacer, "TimeSeries"),      // TUI-only → fallback
    ];
    for (kind, expected) in cases {
        let doc = build_dashboard_doc_from_buffer("x", kind, "q");
        assert_eq!(
            doc.charts[0].type_str(),
            expected,
            "{kind:?} should map to {expected}"
        );
    }
}

#[test]
fn dash_new_buffer_doc_serialises_to_a_valid_upsert_request() {
    // The doc must be encodable as the body for POST /v2/dashboards.
    let doc = build_dashboard_doc_from_buffer("x", VizKind::Line, "q");
    let body = crate::axiom::DashboardUpsertRequest {
        dashboard: &doc,
        version: None,
        overwrite: false,
        uid: None,
        message: None,
    };
    let v = serde_json::to_value(&body).unwrap();
    assert_eq!(v["dashboard"]["name"], "x");
    assert_eq!(v["dashboard"]["charts"][0]["type"], "TimeSeries");
    // overwrite defaults to false → omitted; version is None → omitted.
    assert!(v.get("overwrite").is_none());
    assert!(v.get("version").is_none());
}

// ── dashboard grid (18a) ──────────────────────────────────────────────

fn multi_chart_resource() -> DashboardSummary {
    use crate::axiom::{Chart, ChartBase, LayoutItem};
    // 2x2 grid of charts, each in its own quadrant of the 12-col,
    // 12-row virtual space.
    let mk = |id: &str, name: &str| {
        Chart::TimeSeries(ChartBase {
            id: id.into(),
            name: Some(name.into()),
            query: Some(serde_json::json!({ "mpl": format!("{name}:rate") })),
            extras: Default::default(),
        })
    };
    let layout = |id: &str, x: u32, y: u32| LayoutItem {
        i: id.into(),
        x,
        y: Some(y),
        w: 6,
        h: 6,
        extras: Default::default(),
    };
    DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("grid".into()),
            charts: vec![
                mk("tl", "top-left"),
                mk("tr", "top-right"),
                mk("bl", "bottom-left"),
                mk("br", "bottom-right"),
            ],
            layout: vec![
                layout("tl", 0, 0),
                layout("tr", 6, 0),
                layout("bl", 0, 6),
                layout("br", 6, 6),
            ],
            ..Default::default()
        },
    }
}

#[test]
fn loading_multi_chart_dashboard_auto_switches_to_grid_view() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(app.view_mode, ViewMode::Grid);
    assert_eq!(app.focus, Pane::Dashboard);
    assert_eq!(app.selected_chart_idx, 0);
}

#[test]
fn dashboard_pane_colon_enters_command_mode_and_esc_returns_to_dashboard() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(app.focus, Pane::Dashboard);
    // `:` from grid view drops into the ex-cmdline…
    app.on_key(key(KeyCode::Char(':')));
    assert_eq!(app.mode, Mode::Command);
    assert!(app.cmdline.buf.is_empty());
    // …and Esc returns focus to the dashboard pane (not the editor).
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.focus, Pane::Dashboard);
}

#[test]
fn loading_single_chart_dashboard_stays_in_solo() {
    let mut app = test_app();
    let resource = DashboardSummary {
        uid: "u".into(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: None,
        dashboard: crate::axiom::DashboardDocument {
            name: Some("single".into()),
            charts: vec![crate::axiom::Chart::TimeSeries(crate::axiom::ChartBase {
                id: "c1".into(),
                name: None,
                query: Some(serde_json::json!({ "mpl": "x:y" })),
                extras: Default::default(),
            })],
            ..Default::default()
        },
    };
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(resource),
    });
    assert_eq!(app.view_mode, ViewMode::Solo);
}

// ── cmdline tab completion ───────────────────────────────────

/// Drive the cmdline into command mode and stash `text` as the
/// initial buffer + cursor position.
fn open_cmdline(app: &mut App, text: &str) {
    app.mode = Mode::Command;
    app.cmdline.buf = text.to_string();
    app.cmdline.cursor = text.chars().count();
}

#[test]
fn tab_with_single_candidate_splices_and_appends_space() {
    let mut app = test_app();
    open_cmdline(&mut app, "sol");
    app.on_key(key(KeyCode::Tab));
    // `:sol` only matches `solo`.
    assert_eq!(app.cmdline.buf, "solo ");
    assert!(!app.cmdline_completions.visible);
}

#[test]
fn tab_with_multiple_candidates_splices_top_score_and_shows_popup() {
    let mut app = test_app();
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    // Fuzzy matching against `d` returns multiple heads; the top-scored
    // one is spliced into the buffer and the popup opens so the user
    // can Tab through alternatives.
    assert!(app.cmdline_completions.visible);
    assert!(app.cmdline_completions.items.len() > 1);
    assert_eq!(app.cmdline.buf, app.cmdline_completions.items[0]);
}

#[test]
fn tab_with_partial_token_completes_to_top_score() {
    let mut app = test_app();
    open_cmdline(&mut app, "dash sa");
    app.on_key(key(KeyCode::Tab));
    // `sa` only matches `save` now (no more `save!`), so this is a
    // single-candidate completion that splices + appends a space.
    assert_eq!(app.cmdline.buf, "dash save ");
    assert!(!app.cmdline_completions.visible);
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
    assert_eq!(app.cmdline_completions.selected, 1);
}

#[test]
fn shift_tab_cycles_backward() {
    let mut app = test_app();
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    // BackTab from selection 0 wraps to the last candidate.
    app.on_key(key(KeyCode::BackTab));
    let n = app.cmdline_completions.items.len();
    assert_eq!(app.cmdline_completions.selected, n - 1);
}

#[test]
fn typing_a_character_dismisses_completion_popup() {
    let mut app = test_app();
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    assert!(app.cmdline_completions.visible);
    app.on_key(key(KeyCode::Char('v')));
    assert!(!app.cmdline_completions.visible);
}

#[test]
fn enter_accepts_highlighted_completion_without_executing() {
    let mut app = test_app();
    // `d` matches several heads; Tab opens the popup with the top
    // candidate spliced. Enter then accepts the selection + appends
    // a space while staying in Command mode (not executing).
    open_cmdline(&mut app, "d");
    app.on_key(key(KeyCode::Tab));
    assert!(app.cmdline_completions.visible);
    let highlighted = app.cmdline.buf.clone();
    app.on_key(key(KeyCode::Enter));
    assert!(!app.cmdline_completions.visible);
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
    assert!(!app.cmdline_completions.visible);
}

// ── 18c: per-tile live data ──────────────────────────────────

fn one_series_response(metric: &str) -> MetricsQueryResponse {
    MetricsQueryResponse {
        series: vec![MetricsSeries {
            metric: metric.into(),
            tags: Default::default(),
            start: 1_000,
            resolution: 60,
            data: vec![Some(1.0), Some(2.0), Some(3.0)],
        }],
        trace_id: None,
    }
}

#[test]
fn tile_query_event_stores_series_under_chart_id() {
    let mut app = test_app();
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: "c-foo".into(),
        result: Ok(one_series_response("http_rps")),
    });
    let t = app.tile_results.get("c-foo").unwrap();
    assert!(!t.busy);
    assert!(t.error.is_none());
    assert_eq!(t.series.len(), 1);
    assert_eq!(t.series[0].name, "http_rps");
}

#[test]
fn tile_query_error_keeps_previous_series_and_records_error() {
    let mut app = test_app();
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: "c1".into(),
        result: Ok(one_series_response("a")),
    });
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: "c1".into(),
        result: Err(anyhow::anyhow!("server is down")),
    });
    let t = app.tile_results.get("c1").unwrap();
    assert!(!t.busy);
    assert_eq!(t.error.as_deref(), Some("server is down"));
    // Last good series survives.
    assert_eq!(t.series.len(), 1);
}

#[test]
fn time_command_no_args_opens_preset_picker() {
    let mut app = test_app();
    app.execute_command("time");
    // Picker opens; default cursor lands on the `1h` row if no
    // preset matches the current window (here `now-1h` isn't in
    // TIME_PRESETS so cursor falls back to 0).
    match app.time_picker {
        Some(TimePickerState::Presets { cursor }) => assert_eq!(cursor, 0),
        other => panic!("expected Presets state, got {other:?}"),
    }
}

#[test]
fn time_command_sets_start_and_end() {
    let mut app = test_app();
    app.execute_command("time now-15m now");
    assert_eq!(app.active_time_range(), ("now-15m".into(), "now".into()));
}

#[test]
fn time_command_single_arg_sets_start_only() {
    let mut app = test_app();
    app.execute_command("time now-7d");
    let (s, e) = app.active_time_range();
    assert_eq!(s, "now-7d");
    assert_eq!(e, "now");
}

#[test]
fn time_command_reset_restores_defaults() {
    let mut app = test_app();
    app.execute_command("time now-15m now-5m");
    app.execute_command("time reset");
    assert_eq!(app.active_time_range(), ("now-1h".into(), "now".into()));
}

#[test]
fn time_command_whitespace_only_args_opens_picker() {
    let mut app = test_app();
    app.execute_command("time   ");
    // split_whitespace yields no args — we treat that as "open the
    // picker", same as bare `:time`.
    assert!(matches!(
        app.time_picker,
        Some(TimePickerState::Presets { .. })
    ));
    // And the active range is untouched.
    assert_eq!(app.active_time_range(), ("now-1h".into(), "now".into()));
}

#[test]
fn time_command_with_loaded_dashboard_mirrors_to_wire_and_dirties() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Sanity: adopt resets the dirty flag.
    app.dashboard_dirty = false;
    app.execute_command("time now-2h now-30m");
    let res = app.loaded_dashboard.as_ref().unwrap();
    assert_eq!(
        res.dashboard.time_window_start.as_deref(),
        Some("now-2h")
    );
    assert_eq!(
        res.dashboard.time_window_end.as_deref(),
        Some("now-30m")
    );
    assert!(app.dashboard_dirty, "setting :time should dirty the dashboard");
}

#[test]
fn active_time_range_strips_qr_prefix_for_mpl_endpoint() {
    // Dashboards from the Axiom web UI store `qr-now-7d` / `qr-now`,
    // but the `_mpl` endpoint only accepts the bare relative form.
    // active_time_range must normalise on the way out so we don't
    // 400 with `invalid field: "qr"`.
    let mut app = test_app();
    app.execute_command("time qr-now-7d qr-now");
    // What we store is verbatim (so `:dash save` round-trips)…
    assert_eq!(app.time_range.start.as_str(), "qr-now-7d");
    assert_eq!(app.time_range.end.as_str(), "qr-now");
    // …but what the query layer reads is normalised.
    assert_eq!(
        app.active_time_range(),
        ("now-7d".to_string(), "now".to_string())
    );
}

#[test]
fn time_picker_no_args_matches_qr_prefixed_preset() {
    // If the dashboard came in with `qr-now-6h` / `qr-now`, the
    // picker should still highlight the `6h` row instead of
    // falling back to cursor 0.
    let mut app = test_app();
    app.time_range = crate::dashboard::TimeRange {
        start: "qr-now-6h".into(),
        end: "qr-now".into(),
    };
    app.execute_command("time");
    match app.time_picker {
        Some(TimePickerState::Presets { cursor }) => {
            // 6h is index 1 in TIME_PRESETS.
            assert_eq!(cursor, 1);
        }
        other => panic!("expected Presets state, got {other:?}"),
    }
}

#[test]
fn time_command_sets_window() {
    let mut app = test_app();
    app.execute_command("time now-5m now");
    assert_eq!(app.active_time_range(), ("now-5m".into(), "now".into()));
}

#[test]
fn time_picker_enter_on_preset_applies_it_and_closes() {
    let mut app = test_app();
    app.execute_command("time");
    // Move to the `6h` preset (index 1) and confirm.
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Enter));
    assert!(app.time_picker.is_none());
    assert_eq!(app.active_time_range(), ("now-12h".into(), "now".into()));
}

#[test]
fn time_picker_custom_row_enter_transitions_to_calendar() {
    let mut app = test_app();
    app.execute_command("time");
    // Jump to the bottom (the synthetic Custom… row) and Enter.
    app.on_key(key(KeyCode::Char('G')));
    app.on_key(key(KeyCode::Enter));
    match &app.time_picker {
        Some(TimePickerState::Custom(p)) => {
            // Seeded to yesterday → today by default; just sanity
            // check that we have a non-default-zero start.
            assert!(p.start <= p.end);
        }
        other => panic!("expected Custom state, got {other:?}"),
    }
}

#[test]
fn time_picker_custom_enter_applies_rfc3339_range() {
    let mut app = test_app();
    app.execute_command("time");
    app.on_key(key(KeyCode::Char('G')));
    app.on_key(key(KeyCode::Enter));
    // Force a deterministic picker so we can assert the exact
    // serialised range.
    app.time_picker = Some(TimePickerState::Custom(CustomRangePicker {
        start: time::Date::from_calendar_date(2024, time::Month::May, 1).unwrap(),
        end: time::Date::from_calendar_date(2024, time::Month::May, 15).unwrap(),
        focus: CustomField::Start,
    }));
    app.on_key(key(KeyCode::Enter));
    assert!(app.time_picker.is_none());
    assert_eq!(
        app.active_time_range(),
        (
            "2024-05-01T00:00:00Z".into(),
            "2024-05-15T23:59:59Z".into()
        )
    );
}

#[test]
fn time_picker_custom_swaps_start_and_end_when_inverted() {
    let mut app = test_app();
    app.time_picker = Some(TimePickerState::Custom(CustomRangePicker {
        start: time::Date::from_calendar_date(2024, time::Month::May, 15).unwrap(),
        end: time::Date::from_calendar_date(2024, time::Month::May, 1).unwrap(),
        focus: CustomField::Start,
    }));
    app.on_key(key(KeyCode::Enter));
    // to_range normalises ordering so the API always gets start ≤ end.
    assert_eq!(
        app.active_time_range(),
        (
            "2024-05-01T00:00:00Z".into(),
            "2024-05-15T23:59:59Z".into()
        )
    );
}

#[test]
fn time_picker_custom_esc_returns_to_preset_list() {
    let mut app = test_app();
    app.execute_command("time");
    app.on_key(key(KeyCode::Char('G')));
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Esc));
    match app.time_picker {
        Some(TimePickerState::Presets { cursor }) => {
            assert_eq!(cursor, TIME_PRESET_CUSTOM_INDEX);
        }
        other => panic!("expected Presets state, got {other:?}"),
    }
}

#[test]
fn time_picker_custom_arrow_keys_shift_focused_date() {
    let mut app = test_app();
    let start = time::Date::from_calendar_date(2024, time::Month::May, 10).unwrap();
    let end = time::Date::from_calendar_date(2024, time::Month::May, 20).unwrap();
    app.time_picker = Some(TimePickerState::Custom(CustomRangePicker {
        start,
        end,
        focus: CustomField::Start,
    }));
    // l moves the focused (Start) date forward one day.
    app.on_key(key(KeyCode::Char('l')));
    // j moves it forward a week.
    app.on_key(key(KeyCode::Char('j')));
    // Tab switches focus to End.
    app.on_key(key(KeyCode::Tab));
    // h moves End back one day.
    app.on_key(key(KeyCode::Char('h')));
    match &app.time_picker {
        Some(TimePickerState::Custom(p)) => {
            assert_eq!(p.start, start + time::Duration::days(8));
            assert_eq!(p.end, end - time::Duration::days(1));
            assert_eq!(p.focus, CustomField::End);
        }
        other => panic!("expected Custom state, got {other:?}"),
    }
}

#[test]
fn custom_range_picker_shift_month_handles_year_wrap_and_short_months() {
    // Jan 31 + 1 month → Feb 29 (2024 is leap).
    let mut p = CustomRangePicker {
        start: time::Date::from_calendar_date(2024, time::Month::January, 31).unwrap(),
        end: time::Date::from_calendar_date(2024, time::Month::January, 1).unwrap(),
        focus: CustomField::Start,
    };
    p.shift_month(1);
    assert_eq!(
        p.start,
        time::Date::from_calendar_date(2024, time::Month::February, 29).unwrap()
    );
    // Going back from January wraps the year.
    let mut p = CustomRangePicker {
        start: time::Date::from_calendar_date(2024, time::Month::January, 15).unwrap(),
        end: time::Date::from_calendar_date(2024, time::Month::January, 16).unwrap(),
        focus: CustomField::Start,
    };
    p.shift_month(-1);
    assert_eq!(
        p.start,
        time::Date::from_calendar_date(2023, time::Month::December, 15).unwrap()
    );
}

#[test]
fn trace_command_with_no_query_reports_unavailable() {
    let mut app = test_app();
    app.execute_command("trace");
    assert!(
        app.status.contains("no trace id"),
        "status was {:?}",
        app.status
    );
}

#[test]
fn trace_command_returns_global_last_trace_id_outside_grid() {
    let mut app = test_app();
    app.last_trace_id = Some("abc123".into());
    app.execute_command("trace");
    assert_eq!(app.status, "trace: abc123");
}

#[test]
fn trace_command_in_grid_uses_focused_tile_trace_id() {
    let mut app = test_app();
    // Load a multi-tile dashboard so view_mode flips to Grid and
    // selected_chart_idx points at the first chart.
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let chart_id = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts[0]
        .base()
        .id
        .clone();
    // Per-tile fetch lands with a trace id.
    let mut resp = one_series_response("x");
    resp.trace_id = Some("tile-trace-9".into());
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: chart_id.clone(),
        result: Ok(resp),
    });
    // Global last_trace_id is a red herring — grid view must
    // prefer the focused tile's trace.
    app.last_trace_id = Some("editor-trace".into());
    app.execute_command("trace");
    assert!(
        app.status.contains("tile-trace-9"),
        "status was {:?}",
        app.status
    );
    assert!(
        !app.status.contains("editor-trace"),
        "status leaked editor trace: {:?}",
        app.status
    );
}

#[test]
fn trace_command_in_grid_reports_pending_when_tile_has_no_result() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // No TileQueryFinished events delivered — tile_results is empty
    // for the focused chart.
    app.execute_command("trace");
    assert!(
        app.status.contains("no trace id"),
        "status was {:?}",
        app.status
    );
}

#[test]
fn dashboard_open_clears_stale_tile_results() {
    // Tile results from a prior dashboard must not bleed into a
    // freshly loaded one.
    let mut app = test_app();
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: "old-id".into(),
        result: Ok(one_series_response("stale")),
    });
    assert!(app.tile_results.contains_key("old-id"));
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert!(!app.tile_results.contains_key("old-id"));
}

// ── 18b: pure tile_ops helpers ────────────────────────────────────

fn mk_layout(i: &str, x: u32, y: u32, w: u32, h: u32) -> crate::axiom::LayoutItem {
    crate::axiom::LayoutItem {
        i: i.into(),
        x,
        y: Some(y),
        w,
        h,
        extras: Default::default(),
    }
}

#[test]
fn tile_ops_overlap_detects_shared_cells() {
    let layout = vec![mk_layout("a", 0, 0, 6, 6), mk_layout("b", 6, 0, 6, 6)];
    // Edge-touching is NOT overlap (b starts at x=6, a ends at 5).
    let candidate = mk_layout("new", 5, 0, 1, 6);
    assert!(tile_ops::overlaps_any(&candidate, &layout, "new"));
    let candidate = mk_layout("new", 6, 6, 6, 6);
    assert!(!tile_ops::overlaps_any(&candidate, &layout, "new"));
}

#[test]
fn tile_ops_translate_rejects_overlap_and_offgrid() {
    let mut layout = vec![mk_layout("a", 0, 0, 6, 6), mk_layout("b", 6, 0, 6, 6)];
    // Moving `b` left by 1 would overlap `a`.
    assert_eq!(
        tile_ops::translate(&mut layout, "b", -1, 0).err(),
        Some("would overlap another tile")
    );
    // Off-grid rejected.
    assert_eq!(
        tile_ops::translate(&mut layout, "b", 1, 0).err(),
        Some("edge of grid")
    );
    assert_eq!(
        tile_ops::translate(&mut layout, "a", -1, 0).err(),
        Some("edge of grid")
    );
    // Down is fine.
    assert!(tile_ops::translate(&mut layout, "a", 0, 6).is_ok());
}

#[test]
fn tile_ops_resize_clamps_to_grid_and_minimum() {
    let mut layout = vec![mk_layout("a", 0, 0, 6, 6)];
    // Shrink to 1x1.
    assert!(tile_ops::resize(&mut layout, "a", -5, -5).is_ok());
    assert_eq!((layout[0].w, layout[0].h), (1, 1));
    // Further shrink rejected.
    assert_eq!(
        tile_ops::resize(&mut layout, "a", -1, 0).err(),
        Some("minimum size 1x1")
    );
    // Grow beyond 12 cols rejected.
    assert_eq!(
        tile_ops::resize(&mut layout, "a", 12, 0).err(),
        Some("exceeds 12-col grid")
    );
}

#[test]
fn tile_ops_first_free_slot_skips_occupied_region() {
    let layout = vec![mk_layout("a", 0, 0, 6, 6), mk_layout("b", 6, 0, 6, 6)];
    // First free 6x6 should land directly below `a` at (0, 6).
    let (x, y) = tile_ops::first_free_slot(&layout, 6, 6);
    assert_eq!((x, y), (0, 6));
}

#[test]
fn tile_ops_insert_and_delete_round_trip() {
    let mut charts = vec![];
    let mut layout = vec![];
    let id = tile_ops::insert_tile(
        &mut charts,
        &mut layout,
        crate::dashboard::VizKind::TopList,
        "top errors",
    );
    assert_eq!(charts.len(), 1);
    assert_eq!(layout.len(), 1);
    assert_eq!(charts[0].type_str(), "TopK");
    assert!(tile_ops::delete(&mut charts, &mut layout, &id).is_ok());
    assert!(charts.is_empty() && layout.is_empty());
}

// ── 18b: keymap-driven sub-modes ────────────────────────────────

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
fn move_overlap_rejected_with_status() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.selected_chart_idx = 1; // top-right
    app.on_key(key(KeyCode::Char('m')));
    // Left would overlap top-left (0,0,6,6); we're at (6,0,6,6).
    app.on_key(key(KeyCode::Left));
    let li = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout[1];
    assert_eq!(li.x, 6); // unchanged
    assert!(app.status.contains("move blocked"));
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
fn d_then_y_deletes_selected_tile() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        4
    );
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.tile_submode, TileSubMode::ConfirmDelete));
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        3
    );
    assert!(app.dashboard_dirty);
    assert!(matches!(app.tile_submode, TileSubMode::Idle));
}

#[test]
fn d_then_any_other_key_cancels_delete() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('n')));
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        4
    );
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
        TileSubMode::AddPick { cursor: 0 }
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

// ── 18b: :tile Ex-commands ──────────────────────────────────────

#[test]
fn tile_add_inserts_via_ex_command() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("tile add statistic");
    let charts = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts;
    assert_eq!(charts.len(), 5);
    assert_eq!(charts.last().unwrap().type_str(), "Statistic");
    assert!(app.dashboard_dirty);
}

#[test]
fn tile_title_renames_selected_tile() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("tile title renamed bigly");
    let title = app.loaded_dashboard.as_ref().unwrap().dashboard.charts[0]
        .base()
        .name
        .clone();
    assert_eq!(title.as_deref(), Some("renamed bigly"));
}

#[test]
fn tile_size_via_ex_command_respects_collisions() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Top-left is at (0,0,6,6). Grow to 12 wide → collides with top-right.
    app.execute_command("tile size 12 6");
    assert!(app.last_error.is_some());
}

#[test]
fn tile_mv_via_ex_command_moves_to_absolute() {
    let mut app = test_app();
    let mut r = multi_chart_resource();
    r.dashboard.charts.truncate(1);
    r.dashboard.layout.truncate(1);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(r),
    });
    app.execute_command("grid");
    app.execute_command("tile mv 3 0");
    let li = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout[0];
    assert_eq!(li.x, 3);
    assert_eq!(li.y, Some(0));
}

#[test]
fn tile_rm_via_ex_command_drops_selected_tile() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("tile rm");
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        3
    );
}

#[test]
fn dashboard_dirty_clears_on_save_event() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("tile title new");
    assert!(app.dashboard_dirty);
    let resp = crate::axiom::DashboardWriteResponse {
        status: crate::axiom::DashboardWriteStatus::Updated,
        overwritten: Some(false),
        dashboard: app.loaded_dashboard.clone().unwrap(),
    };
    app.handle_event(AppEvent::DashboardSaved {
        uid: "u".into(),
        result: Ok(resp),
    });
    assert!(!app.dashboard_dirty);
}

#[test]
fn dashboard_pane_arrow_keys_navigate_spatially() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    assert_eq!(app.selected_chart_idx, 0); // top-left
    app.on_key(key(KeyCode::Right));
    assert_eq!(app.selected_chart_idx, 1); // top-right
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.selected_chart_idx, 3); // bottom-right
    app.on_key(key(KeyCode::Left));
    assert_eq!(app.selected_chart_idx, 2); // bottom-left
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.selected_chart_idx, 0); // back to top-left
}

#[test]
fn dashboard_pane_tab_cycles_in_row_major_order() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    for expected in [1usize, 2, 3, 0] {
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.selected_chart_idx, expected);
    }
}

#[test]
fn dashboard_pane_enter_zooms_into_solo_with_selected_chart() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Move to the bottom-right chart, then Enter.
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.view_mode, ViewMode::Solo);
    assert_eq!(app.focus, Pane::Editor);
    assert!(app.query_text().contains("bottom-right:rate"));
}

#[test]
fn dashboard_pane_esc_returns_focus_to_editor_without_changing_view() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Pane::Editor);
    assert_eq!(app.view_mode, ViewMode::Grid); // not changed
}

#[test]
fn grid_solo_commands_toggle_view_mode() {
    let mut app = test_app();
    app.execute_command("grid");
    // No dashboard → status message, no mode change.
    assert_eq!(app.view_mode, ViewMode::Solo);
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("solo");
    assert_eq!(app.view_mode, ViewMode::Solo);
    app.execute_command("grid");
    assert_eq!(app.view_mode, ViewMode::Grid);
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
    assert_eq!(app.loaded_dashboard.as_ref().unwrap().name(), "prod");
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
fn dashboard_round_trip_preserves_extras() {
    // Load → serialise → reload → re-serialise. The two
    // serialised forms must be byte-equal; this catches any field
    // we silently drop on the decode side (which would break
    // PUT round-trip against the real server, since the schema is
    // `additionalProperties: false`).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.axiom.json");
    let dst = dir.path().join("dst.axiom.json");
    std::fs::write(&src, FIXTURE_DASHBOARD_JSON).unwrap();

    let mut app = test_app();
    app.open_file(src).unwrap();
    let first_serialise = app.dashboard_to_json().unwrap();
    app.write_file(Some(dst.clone())).unwrap();

    let mut app2 = test_app();
    app2.open_file(dst).unwrap();
    let second_serialise = app2.dashboard_to_json().unwrap();

    assert_eq!(
        first_serialise, second_serialise,
        "dashboard JSON did not round-trip byte-stably"
    );
    // Extras spot check: unmodelled fields survived.
    let re: serde_json::Value = serde_json::from_str(&second_serialise).unwrap();
    assert_eq!(re["dashboard"]["refreshTime"], 60);
    assert_eq!(re["dashboard"]["schemaVersion"], 2);
    assert_eq!(re["dashboard"]["owner"], "X-AXIOM-EVERYONE");
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
fn dashinfo_command_requires_loaded_dashboard() {
    let mut app = test_app();
    app.execute_command("dashinfo");
    assert!(!app.dashinfo_visible);
    assert!(app.status.contains("no dashboard loaded"));
}

#[test]
fn dashinfo_command_toggles_when_dashboard_loaded() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(DashboardSummary {
            uid: "u".into(),
            id: None,
            updated_at: None,
            updated_by: None,
            version: None,
            dashboard: Default::default(),
        }),
    });
    app.execute_command("dashinfo");
    assert!(app.dashinfo_visible);
    // Any key dismisses.
    app.on_key(key(KeyCode::Esc));
    assert!(!app.dashinfo_visible);
}

#[test]
fn open_without_arg_fails_without_prior_pick() {
    let mut app = test_app();
    app.execute_command("open");
    assert!(app.last_error.is_some());
}

#[test]
fn dashboard_picker_esc_closes_without_selecting() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardsFetched(Ok(vec![dash("x", "y", None)])));
    assert!(app.dashboards.visible);
    app.on_key(key(KeyCode::Esc));
    assert!(!app.dashboards.visible);
    assert!(app.last_picked_dashboard.is_none());
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
