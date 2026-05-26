//! tile tests.

use super::*;

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
fn tile_query_event_stores_series_under_chart_id() {
    let mut app = test_app();
    // Slot must exist before the event arrives — the handler now drops
    // results targeting a tile that was deleted or never spawned.
    app.tile_results.insert("c-foo".into(), Default::default());
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: "c-foo".into(),
        epoch: app.tile_query_epoch,
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
    app.tile_results.insert("c1".into(), Default::default());
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: "c1".into(),
        epoch: app.tile_query_epoch,
        result: Ok(one_series_response("a")),
    });
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: "c1".into(),
        epoch: app.tile_query_epoch,
        result: Err(anyhow::anyhow!("server is down")),
    });
    let t = app.tile_results.get("c1").unwrap();
    assert!(!t.busy);
    assert_eq!(t.error.as_deref(), Some("server is down"));
    // Last good series survives.
    assert_eq!(t.series.len(), 1);
}
#[test]
fn tile_query_event_with_stale_epoch_is_dropped() {
    // A slow result from a superseded dashboard load must not
    // resurrect or clobber a tile in the current dashboard.
    let mut app = test_app();
    let stale_epoch = app.tile_query_epoch;
    // Pre-seed a slot for `c-foo` so the only thing keeping the
    // event from applying is the epoch check.
    app.tile_results.insert("c-foo".into(), Default::default());
    // Simulate a dashboard swap that bumps the epoch.
    app.tile_query_epoch = app.tile_query_epoch.wrapping_add(1);
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: "c-foo".into(),
        epoch: stale_epoch,
        result: Ok(one_series_response("stale-series")),
    });
    let entry = app.tile_results.get("c-foo").unwrap();
    assert!(
        entry.series.is_empty(),
        "stale-epoch event must not write series"
    );
    assert!(!entry.busy, "slot stays at default (busy=false)");
}
#[test]
fn tile_query_event_for_unknown_chart_is_dropped() {
    // Tile was deleted between dispatch and arrival — the result
    // must not resurrect the slot.
    let mut app = test_app();
    let chart_id = "deleted-tile".to_string();
    app.handle_event(AppEvent::TileQueryFinished {
        chart_id: chart_id.clone(),
        epoch: app.tile_query_epoch,
        result: Ok(one_series_response("a")),
    });
    assert!(
        !app.tile_results.contains_key(&chart_id),
        "event for unknown chart_id must not insert a slot"
    );
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
    assert_eq!(charts[0].type_str(), Some("TopK"));
    assert!(tile_ops::delete(&mut charts, &mut layout, &id).is_ok());
    assert!(charts.is_empty() && layout.is_empty());
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
fn tile_add_inserts_via_ex_command() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("tile add statistic");
    let charts = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts;
    assert_eq!(charts.len(), 5);
    assert_eq!(charts.last().unwrap().type_str(), Some("Statistic"));
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
        .known_base()
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
fn move_into_neighbour_now_shoves_instead_of_blocking() {
    // tl at (0,0,6,6); tr at (6,0,6,6). Pressing `m` then `l` moves
    // tl right by 1, which would have collided with tr in step-18.
    // Step-19 cascades: tr shoves to (7,0,6,6)… wait, 7+6=13 > 12,
    // so tr falls through to Down at y=6, overlapping bl, which
    // cascades down too.
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.on_key(key(KeyCode::Char('m')));
    app.on_key(key(KeyCode::Right));
    let layout = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout;
    let by = |id: &str| layout.iter().find(|l| l.i == id).unwrap().clone();
    // tl moved to x=1.
    assert_eq!(by("tl").x, 1);
    // tr's right-shove would overflow; fell through to Down.
    assert_eq!(by("tr").y, Some(6));
    // Status reports cascade.
    assert!(
        app.status.contains("shoved") || app.status.contains("row"),
        "status was {:?}",
        app.status
    );
}
#[test]
fn move_esc_reverts_full_cascade_to_original() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Snapshot (id, x, y, w, h) per tile so we can compare without
    // a PartialEq on `LayoutItem` (extras is a serde_json::Map).
    let snapshot = |app: &App| -> Vec<(String, u32, u32, u32, u32)> {
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .layout
            .iter()
            .map(|l| (l.i.clone(), l.x, l.y.unwrap_or(0), l.w, l.h))
            .collect()
    };
    let before = snapshot(&app);
    app.on_key(key(KeyCode::Char('m')));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Right));
    assert!(matches!(app.tile_submode, TileSubMode::Move { .. }));
    app.on_key(key(KeyCode::Esc));
    let after = snapshot(&app);
    assert_eq!(after, before);
    assert!(matches!(app.tile_submode, TileSubMode::Idle));
}
#[test]
fn move_left_into_occupant_still_blocks() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.selected_chart_idx = 1; // tr at (6,0,6,6)
    app.on_key(key(KeyCode::Char('m')));
    app.on_key(key(KeyCode::Left));
    let li = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout[1];
    // Unchanged.
    assert_eq!(li.x, 6);
    assert!(app.status.contains("blocked"));
}
#[test]
fn resize_grow_right_shoves_neighbour() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // tl (6 wide) grows right; tr (at x=6) shoves down by fallback.
    app.on_key(key(KeyCode::Char('s')));
    app.on_key(key(KeyCode::Right));
    let layout = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout;
    let by = |id: &str| layout.iter().find(|l| l.i == id).unwrap().clone();
    assert_eq!(by("tl").w, 7);
    // tr fell through to Down (7+6>12), so y advanced.
    assert!(by("tr").y.unwrap_or(0) > 0);
}
#[test]
fn yank_focused_then_paste_creates_copy_with_fresh_id() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // y, then p — focused is "tl".
    app.on_key(key(KeyCode::Char('y')));
    assert!(app.tile_yank.is_some());
    app.on_key(key(KeyCode::Char('p')));
    let resource = app.loaded_dashboard.as_ref().unwrap();
    assert_eq!(resource.dashboard.charts.len(), 5);
    // Pasted tile id differs from the original ids.
    let ids: Vec<&str> = resource
        .dashboard
        .charts
        .iter()
        .map(|c| c.known_base().id.as_str())
        .collect();
    assert!(ids.contains(&"tl"));
    // No id collision.
    let unique: std::collections::HashSet<&&str> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}
#[test]
fn cut_removes_tile_and_populates_yank_register() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    let before = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .len();
    app.on_key(key(KeyCode::Char('x')));
    let after = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .len();
    assert_eq!(after, before - 1);
    assert!(app.tile_yank.is_some());
    assert!(app.dashboard_dirty);
}
#[test]
fn tile_mv_bang_shoves_instead_of_blocking() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Plain `tile mv 6 0` from tl (0,0) would collide with tr at
    // (6,0) → strict reject.
    app.execute_command("tile mv 6 0");
    assert!(app.last_error.is_some(), "strict mv should error");
    app.last_error = None;
    // The bang opts into shove: tr cascades out of the way.
    app.execute_command("tile mv! 6 0");
    assert!(app.last_error.is_none(), "shove mv should succeed");
    let li = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout;
    let tl = li.iter().find(|l| l.i == "tl").unwrap();
    assert_eq!((tl.x, tl.y.unwrap_or(0)), (6, 0));
}
#[test]
fn tile_yank_then_paste_via_ex_commands() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("tile yank");
    assert!(app.tile_yank.is_some());
    app.execute_command("tile paste");
    let n = app
        .loaded_dashboard
        .as_ref()
        .unwrap()
        .dashboard
        .charts
        .len();
    assert_eq!(n, 5);
}
#[test]
fn tile_open_bang_opens_above_focused() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    // Focus on bl (y=6) so `open!` has somewhere above to insert.
    app.selected_chart_idx = 2;
    app.execute_command("tile open! line");
    let charts = &app.loaded_dashboard.as_ref().unwrap().dashboard.charts;
    assert_eq!(charts.len(), 5);
    // Last chart should be the newly inserted one.
    let new_id = charts.last().unwrap().known_base().id.as_str();
    let li = &app.loaded_dashboard.as_ref().unwrap().dashboard.layout;
    let new_li = li.iter().find(|l| l.i == new_id).unwrap();
    // "above" bl (y=6) means y=0 (6 - 6).
    assert_eq!(new_li.y, Some(0));
}
#[test]
fn tile_undo_ex_command_round_trips_with_paste() {
    let mut app = test_app();
    app.handle_event(AppEvent::DashboardOpened {
        uid: "u".into(),
        result: Ok(multi_chart_resource()),
    });
    app.execute_command("tile yank");
    app.execute_command("tile paste");
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        5
    );
    app.execute_command("tile undo");
    assert_eq!(
        app.loaded_dashboard
            .as_ref()
            .unwrap()
            .dashboard
            .charts
            .len(),
        4
    );
}
