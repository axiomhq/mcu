//! params tests.

use super::*;
use crossterm::event::KeyCode;

/// Submit a `:` ex-command through the real key path so the param
/// cache refresh runs exactly as it does in production.
fn submit(app: &mut App, cmd: &str) {
    app.on_key(key(KeyCode::Char(':')));
    type_text(app, cmd);
    app.on_key(key(KeyCode::Enter));
}

#[test]
fn param_rows_cache_matches_live_rows_after_recompute() {
    let mut app = test_app();
    // `set_buffer` runs `recompute_diagnostics`, which fills the cache.
    set_buffer(&mut app, "param $host: string;\nfoo:bar");
    assert_eq!(app.param_rows_cache, app.param_rows());
    assert!(app.param_rows_cache.iter().any(|r| r.name == "host"));
}

#[test]
fn param_rows_cache_refreshes_after_p_set_and_clear() {
    let mut app = test_app();
    set_buffer(&mut app, "param $host: string;\nfoo:bar");
    submit(&mut app, "p host=\"db-01\"");
    let row = app
        .param_rows_cache
        .iter()
        .find(|r| r.name == "host")
        .expect("host row in cache after :p set");
    assert_eq!(row.status, crate::params::ParamStatus::Ok);
    assert_eq!(row.value.as_deref(), Some("\"db-01\""));

    // `:p!` clears every value; the cached row must drop back to NotSet.
    submit(&mut app, "p!");
    let row = app
        .param_rows_cache
        .iter()
        .find(|r| r.name == "host")
        .expect("host row still declared after clear");
    assert_eq!(row.status, crate::params::ParamStatus::NotSet);
    assert_eq!(app.param_rows_cache, app.param_rows());
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
    app.params.cli.insert("host".into(), "\"db-01\"".into());
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "host").unwrap();
    assert_eq!(r.status, crate::params::ParamStatus::Ok);
}
#[test]
fn param_rows_typecheck_string_mismatch_when_int_given() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.params.cli.insert("host".into(), "42".into());
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "host").unwrap();
    assert_eq!(r.status, crate::params::ParamStatus::TypeMismatch);
}
#[test]
fn param_rows_duration_ok() {
    let mut app = test_app();
    set_query(&mut app, "param $w: Duration;\nfoo:bar");
    app.params.cli.insert("w".into(), "5m".into());
    let rows = app.param_rows();
    let r = rows.iter().find(|r| r.name == "w").unwrap();
    assert_eq!(r.status, crate::params::ParamStatus::Ok);
}
#[test]
fn param_rows_undeclared_provided_is_warning() {
    let mut app = test_app();
    // empty buffer — nothing declared
    app.params.cli.insert("orphan".into(), "\"x\"".into());
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
    assert_eq!(app.params.selected, 0);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.params.selected, 1);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.params.selected, 2);
    app.on_key(key(KeyCode::Char('j'))); // wraps
    assert_eq!(app.params.selected, 0);
}
#[test]
fn params_pane_x_clears_selected() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.params.cli.insert("host".into(), "\"db-01\"".into());
    app.set_focus(Pane::Params);
    app.on_key(key(KeyCode::Char('x')));
    assert!(!app.params.cli.contains_key("host"));
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
    app.params.cli.insert("host".into(), "\"db-01\"".into());
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
        app.params.cli.get("host").map(String::as_str),
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
fn params_pane_esc_returns_to_editor() {
    let mut app = test_app();
    set_query(&mut app, "param $host: string;\nfoo:bar");
    app.set_focus(Pane::Params);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Pane::Editor);
}
