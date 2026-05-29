//! Mouse support tests (step 27).
//!
//! These drive `App::on_mouse` directly with synthetic
//! [`MouseEvent`]s and pre-seeded `mouse_geom` rects — the same shape
//! as the keyboard tests, no real terminal required. The renderer
//! stashes the geometry in production; here we set it by hand so the
//! hit-testing logic is exercised in isolation.

use super::*;
use crate::trace::{Span as TraceSpan, SpanKind, TraceModel, TreeRow};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::BTreeMap;

// ---- Event constructors -------------------------------------------

fn mdown(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn mscroll(col: u16, row: u16, up: bool) -> MouseEvent {
    MouseEvent {
        kind: if up {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        },
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Minimal parent→children trace view (DFS order). Each tuple is
/// `(id, parent, name)`; depth + `has_children` derived from the
/// parent links.
fn trace_view_from(rows: &[(&str, Option<&str>, &str)]) -> crate::app::types::TraceView {
    let n = rows.len();
    let mut spans = Vec::with_capacity(n);
    let mut by_id: BTreeMap<String, usize> = BTreeMap::new();
    for (i, (id, parent, name)) in rows.iter().enumerate() {
        spans.push(TraceSpan {
            span_id: (*id).into(),
            parent_span_id: parent.map(str::to_string),
            name: (*name).into(),
            service: "svc".into(),
            kind: SpanKind::Unknown,
            status_code: None,
            is_error: false,
            start_ns: i as i64 * 1_000_000,
            end_ns: i as i64 * 1_000_000 + 1_000_000,
            duration_ns: 1_000_000,
            events: Vec::new(),
            attributes: BTreeMap::new(),
            resource: BTreeMap::new(),
        });
        by_id.insert((*id).into(), i);
    }
    let depth_for = |idx: usize, spans: &[TraceSpan], by_id: &BTreeMap<String, usize>| -> u16 {
        let mut d = 0;
        let mut cur = idx;
        while let Some(p) = spans[cur].parent_span_id.as_deref() {
            match by_id.get(p) {
                Some(&pi) => {
                    d += 1;
                    cur = pi;
                }
                None => break,
            }
        }
        d
    };
    let has_kids: Vec<bool> = (0..n)
        .map(|i| {
            let id = spans[i].span_id.as_str();
            rows.iter().any(|(_, p, _)| *p == Some(id))
        })
        .collect();
    let tree: Vec<TreeRow> = (0..n)
        .map(|i| TreeRow {
            span_idx: i,
            depth: depth_for(i, &spans, &by_id),
            has_children: has_kids[i],
            is_orphan: false,
        })
        .collect();
    let roots: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, (_, p, _))| p.is_none().then_some(i))
        .collect();
    let model = TraceModel {
        trace_id: "tid".into(),
        dataset: "ds".into(),
        spans,
        by_id,
        roots,
        t0_ns: 0,
        t1_ns: n as i64 * 1_000_000,
        tree,
    };
    crate::app::types::TraceView::new(model, ViewMode::Solo)
}

fn cursor_span(app: &App) -> String {
    let v = app.trace_view.as_ref().unwrap();
    v.model.spans[v.model.tree[v.cursor].span_idx]
        .span_id
        .clone()
}

// ---- Topbar tabs (feature 3) --------------------------------------

#[test]
fn topbar_query_tab_returns_to_solo_editor() {
    let mut app = test_app();
    app.loaded_dashboard = Some(multi_chart_resource());
    app.view_mode = ViewMode::Grid;
    app.focus = Pane::Dashboard;
    app.mouse_geom.topbar = rect(0, 0, 80, 1);
    app.mouse_geom.topbar_query_end_x = 7;
    app.mouse_geom.topbar_dash_end_x = 21;
    app.on_mouse(mdown(3, 0)); // inside " QUERY "
    assert_eq!(app.view_mode, ViewMode::Solo);
    assert_eq!(app.focus, Pane::Editor);
}

#[test]
fn topbar_dashboard_tab_opens_grid() {
    let mut app = test_app();
    app.loaded_dashboard = Some(multi_chart_resource());
    app.view_mode = ViewMode::Solo;
    app.focus = Pane::Editor;
    app.mouse_geom.topbar = rect(0, 0, 80, 1);
    app.mouse_geom.topbar_query_end_x = 7;
    app.mouse_geom.topbar_dash_end_x = 21;
    app.on_mouse(mdown(15, 0)); // inside " DASHBOARD "
    assert_eq!(app.view_mode, ViewMode::Grid);
    assert_eq!(app.focus, Pane::Dashboard);
}

// ---- Pane focus (feature 2) ---------------------------------------

#[test]
fn click_legend_pane_focuses_it() {
    let mut app = app_with_series(3);
    app.mouse_geom.legend = rect(50, 1, 30, 10);
    app.on_mouse(mdown(60, 5));
    assert_eq!(app.focus, Pane::Legend);
}

#[test]
fn click_params_pane_focuses_it() {
    let mut app = app_with_series(2);
    app.mouse_geom.params = rect(50, 12, 30, 6);
    app.on_mouse(mdown(55, 14));
    assert_eq!(app.focus, Pane::Params);
}

#[test]
fn click_graph_without_table_is_a_noop() {
    // A plain chart pane isn't focusable; the click must not emit a
    // "no table rows" status nor change focus.
    let mut app = app_with_series(2);
    app.mouse_geom.graph = rect(0, 1, 49, 10);
    app.on_mouse(mdown(10, 5));
    assert_eq!(app.focus, Pane::Editor);
}

// ---- Editor cursor (feature 7) ------------------------------------

#[test]
fn click_in_editor_moves_cursor_to_cell() {
    let mut app = test_app();
    set_query(&mut app, "hello\nworld");
    // Inner rect starts at (1,1) after the border; no scroll.
    app.mouse_geom.editor_inner = rect(1, 1, 40, 10);
    app.mouse_geom.editor_scroll_top = 0;
    // Cell (col 3, row 2) → buffer row 1 ("world"), col 2.
    app.on_mouse(mdown(3, 2));
    assert_eq!(app.focus, Pane::Editor);
    assert_eq!(app.editor.cursor(), (1, 2));
}

#[test]
fn click_past_line_end_clamps_to_line_length() {
    let mut app = test_app();
    set_query(&mut app, "hi\nlonger line");
    app.mouse_geom.editor_inner = rect(1, 1, 60, 10);
    app.mouse_geom.editor_scroll_top = 0;
    // Row 0 ("hi", len 2); click far to the right clamps to col 2.
    app.on_mouse(mdown(50, 1));
    assert_eq!(app.editor.cursor(), (0, 2));
}

#[test]
fn click_below_last_row_clamps_to_last_row() {
    let mut app = test_app();
    set_query(&mut app, "one\ntwo");
    app.mouse_geom.editor_inner = rect(1, 1, 40, 10);
    app.mouse_geom.editor_scroll_top = 0;
    // Row way past the 2 buffer lines clamps to the last row (1).
    app.on_mouse(mdown(2, 8));
    assert_eq!(app.editor.cursor().0, 1);
}

#[test]
fn editor_click_honours_scroll_top() {
    let mut app = test_app();
    set_query(&mut app, "l0\nl1\nl2\nl3\nl4");
    app.mouse_geom.editor_inner = rect(1, 1, 40, 3);
    app.mouse_geom.editor_scroll_top = 2; // first visible row is l2
    // Click the first visible row → buffer row 2.
    app.on_mouse(mdown(2, 1));
    assert_eq!(app.editor.cursor().0, 2);
}

// ---- Grid tiles (feature 1) ---------------------------------------

#[test]
fn click_grid_tile_selects_and_focuses_it() {
    let mut app = test_app();
    app.loaded_dashboard = Some(multi_chart_resource());
    app.view_mode = ViewMode::Grid;
    app.buffer_mode = crate::app::BufferMode::Dashboard;
    app.mouse_geom.grid_tiles = vec![
        (0, rect(1, 2, 30, 10)),
        (1, rect(31, 2, 30, 10)),
        (2, rect(1, 12, 30, 10)),
    ];
    app.on_mouse(mdown(35, 5)); // inside tile 1
    assert_eq!(app.selected_chart_idx, 1);
    assert_eq!(app.focus, Pane::Dashboard);
}

#[test]
fn click_grid_gap_does_not_select_a_tile() {
    let mut app = test_app();
    app.loaded_dashboard = Some(multi_chart_resource());
    app.view_mode = ViewMode::Grid;
    app.mouse_geom.grid_tiles = vec![(0, rect(1, 2, 10, 10))];
    app.mouse_geom.dashboard = rect(0, 1, 80, 30);
    app.on_mouse(mdown(40, 5)); // empty area of the grid pane
    assert_eq!(app.selected_chart_idx, 0);
    assert_eq!(app.focus, Pane::Dashboard); // pane focus still applies
}

// ---- Trace: select + fold + scroll (features 4,5,6) ---------------

#[test]
fn click_trace_row_selects_span() {
    let mut app = test_app();
    app.trace_view = Some(trace_view_from(&[
        ("r", None, "root"),
        ("c", Some("r"), "child"),
    ]));
    app.view_mode = ViewMode::Trace;
    app.last_trace_body_height = 10;
    app.mouse_geom.trace_tree_body = rect(0, 2, 50, 10);
    app.mouse_geom.trace_tree_scroll = 0;
    // dy = 1 → second visible row (child). Click on the name, not the marker.
    app.on_mouse(mdown(20, 3));
    assert_eq!(app.focus, Pane::TraceTree);
    assert_eq!(cursor_span(&app), "c");
}

#[test]
fn click_fold_marker_collapses_parent() {
    let mut app = test_app();
    app.trace_view = Some(trace_view_from(&[
        ("r", None, "root"),
        ("c", Some("r"), "child"),
    ]));
    app.view_mode = ViewMode::Trace;
    app.last_trace_body_height = 10;
    app.mouse_geom.trace_tree_body = rect(0, 2, 50, 10);
    app.mouse_geom.trace_tree_scroll = 0;
    // Root is depth 0 → marker band [0,2). Click col 0 of row 0.
    app.on_mouse(mdown(0, 2));
    let v = app.trace_view.as_ref().unwrap();
    assert!(v.collapsed.contains(&0), "root should be collapsed");
    assert_eq!(v.visible_rows(), vec![0], "child hidden after fold");
}

#[test]
fn click_fold_marker_again_expands_parent() {
    let mut app = test_app();
    app.trace_view = Some(trace_view_from(&[
        ("r", None, "root"),
        ("c", Some("r"), "child"),
    ]));
    app.view_mode = ViewMode::Trace;
    app.last_trace_body_height = 10;
    app.mouse_geom.trace_tree_body = rect(0, 2, 50, 10);
    app.mouse_geom.trace_tree_scroll = 0;
    app.on_mouse(mdown(0, 2)); // collapse
    app.on_mouse(mdown(0, 2)); // expand
    let v = app.trace_view.as_ref().unwrap();
    assert!(v.collapsed.is_empty());
    assert_eq!(v.visible_rows(), vec![0, 1]);
}

#[test]
fn scroll_trace_tree_moves_cursor() {
    let mut app = test_app();
    let rows: Vec<(String, Option<String>, String)> = (0..10)
        .map(|i| (format!("s{i}"), None, format!("n{i}")))
        .collect();
    let row_refs: Vec<(&str, Option<&str>, &str)> = rows
        .iter()
        .map(|(id, _, name)| (id.as_str(), None, name.as_str()))
        .collect();
    app.trace_view = Some(trace_view_from(&row_refs));
    app.view_mode = ViewMode::Trace;
    app.last_trace_body_height = 10;
    app.mouse_geom.trace_tree_body = rect(0, 2, 50, 10);
    app.mouse_geom.trace_tree_scroll = 0;
    assert_eq!(cursor_span(&app), "s0");
    app.on_mouse(mscroll(10, 5, false)); // scroll down: cursor += SCROLL_STEP
    assert_eq!(cursor_span(&app), "s3");
    app.on_mouse(mscroll(10, 5, true)); // scroll up
    assert_eq!(cursor_span(&app), "s0");
}

#[test]
fn scroll_trace_detail_pane_moves_detail_offset() {
    let mut app = test_app();
    app.trace_view = Some(trace_view_from(&[("r", None, "root")]));
    app.view_mode = ViewMode::Trace;
    app.mouse_geom.trace_tree_body = rect(0, 2, 30, 10);
    app.mouse_geom.trace_detail = rect(30, 2, 50, 10);
    assert_eq!(app.trace_view.as_ref().unwrap().detail_scroll, 0);
    app.on_mouse(mscroll(40, 5, false)); // wheel down over detail pane
    assert_eq!(
        app.trace_view.as_ref().unwrap().detail_scroll,
        3,
        "detail pane scrolls by SCROLL_STEP lines"
    );
}

#[test]
fn scroll_solo_table_moves_row_selection() {
    let mut app = test_app();
    app.table_result = Some(crate::viz::TableResult {
        columns: vec!["level".into(), "n".into()],
        rows: (0..8)
            .map(|i| {
                vec![
                    crate::viz::table::TableCell::Str(format!("row{i}")),
                    crate::viz::table::TableCell::Int(i as i64),
                ]
            })
            .collect(),
    });
    app.mouse_geom.graph = rect(0, 1, 60, 12);
    assert_eq!(app.table_selected, 0);
    app.on_mouse(mscroll(10, 5, false)); // wheel down over the table
    assert_eq!(
        app.table_selected, 3,
        "table selection moves by SCROLL_STEP"
    );
    app.on_mouse(mscroll(10, 5, true)); // wheel up clamps at 0
    assert_eq!(app.table_selected, 0);
}

#[test]
fn click_trace_detail_focuses_detail() {
    let mut app = test_app();
    app.trace_view = Some(trace_view_from(&[("r", None, "root")]));
    app.view_mode = ViewMode::Trace;
    app.mouse_geom.trace_tree_body = rect(0, 2, 30, 10);
    app.mouse_geom.trace_detail = rect(30, 2, 50, 10);
    app.on_mouse(mdown(40, 5));
    assert_eq!(app.focus, Pane::TraceDetail);
}

// ---- Overlay gating + stale geometry ------------------------------

#[test]
fn click_is_swallowed_while_help_overlay_visible() {
    let mut app = app_with_series(2);
    app.help.visible = true;
    app.mouse_geom.legend = rect(50, 1, 30, 10);
    app.on_mouse(mdown(60, 5));
    assert_eq!(app.focus, Pane::Editor, "click must not bleed past overlay");
    assert!(app.help.visible, "help stays open");
}

#[test]
fn click_dismisses_dashinfo_overlay() {
    let mut app = test_app();
    app.dashinfo_visible = true;
    app.on_mouse(mdown(10, 10));
    assert!(!app.dashinfo_visible);
}

#[test]
fn trace_geometry_does_not_misfire_in_solo_view() {
    // Stale trace rect from a previous Trace session must not select a
    // span while we're back in Solo.
    let mut app = test_app();
    app.trace_view = Some(trace_view_from(&[
        ("r", None, "root"),
        ("c", Some("r"), "child"),
    ]));
    app.view_mode = ViewMode::Solo; // not Trace
    app.mouse_geom.trace_tree_body = rect(0, 2, 50, 10);
    app.mouse_geom.trace_tree_scroll = 0;
    app.on_mouse(mdown(20, 3));
    // Cursor untouched (still the default root) and focus unchanged.
    assert_eq!(cursor_span(&app), "r");
    assert_eq!(app.focus, Pane::Editor);
}

#[test]
fn scroll_with_no_trace_view_is_a_noop() {
    let mut app = test_app();
    app.view_mode = ViewMode::Trace;
    app.mouse_geom.trace_tree_body = rect(0, 2, 50, 10);
    // No panic, no state change.
    app.on_mouse(mscroll(10, 5, false));
    assert!(app.trace_view.is_none());
}
