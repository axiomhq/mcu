//! focus tests.

use super::*;

#[test]
fn ctrl_r_redo_path_runs() {
    let mut app = test_app();
    app.on_key(ctrl(KeyCode::Char('r')));
    assert_eq!(app.mode, Mode::Normal);
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
    app.legend.hidden.clear();
    app.on_key(ctrl(KeyCode::Char('w')));
    app.on_key(key(KeyCode::Char('w')));
    assert_eq!(app.focus, Pane::Editor);
    assert!(app.status.contains("no series"), "got {:?}", app.status);
}
