use super::*;

impl App {
    /// Keymap for the dashboard grid pane. The dispatch order is:
    ///
    ///   1. Active sub-mode (Move/Resize/ConfirmDelete/PickViz) owns
    ///      every key while engaged — Esc cancels back to Idle.
    ///   2. `Idle` accepts the navigation + entry-point shortcuts
    ///      (m, s, d, a, v, R, Enter, hjkl/arrows, Tab).
    pub(super) fn handle_dashboard_key(&mut self, key: KeyEvent) {
        // Sub-mode takes precedence; the verb parser is silenced
        // while any sub-mode is active so an in-progress shove
        // can't be hijacked by a stray digit.
        match self.tile_submode.clone() {
            TileSubMode::Move {
                original_layout,
                original_id,
                dx,
                dy,
            } => return self.handle_move_key(key, original_layout, original_id, dx, dy),
            TileSubMode::Resize {
                original_layout,
                original_id,
                dw,
                dh,
            } => return self.handle_resize_key(key, original_layout, original_id, dw, dh),
            TileSubMode::ConfirmDelete => return self.handle_confirm_delete_key(key),
            TileSubMode::PickViz { cursor, action } => {
                return self.handle_pick_viz_key(key, cursor, action);
            }
            TileSubMode::Idle => {}
        }
        // Idle: run every key through the verb parser first.
        // Digits accumulate as a count; verbs (y/x/p/P/o/O/u) emit
        // a `DashCommand`; everything else passes through to the
        // navigation/sub-mode-entry keymap with the count attached.
        use crate::app::dashboard_cmd::DashStep;
        let (passthrough_key, repeat_count) = match self.dashboard_cmd.feed(key) {
            DashStep::Pending => return,
            DashStep::Emit(cmd) => return self.run_dash_command(cmd),
            DashStep::Passthrough { key, count } => (key, count),
        };
        self.handle_dashboard_idle_key(passthrough_key, repeat_count);
    }

    /// Dispatch a verb emitted by the dashboard parser. Each verb
    /// snapshots the dashboard for one-level undo *before* mutating
    /// (or trusts a callee like [`Self::cut_focused`] /
    /// [`Self::paste_yanked`] that does the snapshot itself).
    pub(super) fn run_dash_command(&mut self, cmd: crate::app::dashboard_cmd::DashCommand) {
        use crate::app::dashboard_cmd::DashCommand::*;
        match cmd {
            Yank { n } => {
                self.yank_focused(n);
            }
            Cut { n } => {
                self.cut_focused(n);
            }
            Paste { after, n } => self.paste_yanked(after, n),
            Open { above, n } => self.enter_tile_open_pick(above, n),
            Undo => self.dashboard_undo(),
        }
    }

    /// The pre-step-19 idle keymap, repeated `count` times for keys
    /// where `count > 1` makes sense (just navigation today). `count
    /// = 0` means no explicit count was typed.
    pub(super) fn handle_dashboard_idle_key(&mut self, key: KeyEvent, count: usize) {
        use KeyCode::*;
        use KeyModifiers as M;
        let reps = count.max(1);
        // Spatial nav repeats with the count (`3j` moves three
        // tiles down). Other actions ignore the count.
        match (key.code, key.modifiers) {
            (Esc, _) => self.focus = Pane::Editor,
            (Left, _) | (Char('h'), M::NONE) => {
                for _ in 0..reps {
                    self.move_dashboard_selection_spatial(SpatialDir::Left);
                }
            }
            (Right, _) | (Char('l'), M::NONE) => {
                for _ in 0..reps {
                    self.move_dashboard_selection_spatial(SpatialDir::Right);
                }
            }
            (Up, _) | (Char('k'), M::NONE) => {
                for _ in 0..reps {
                    self.move_dashboard_selection_spatial(SpatialDir::Up);
                }
            }
            (Down, _) | (Char('j'), M::NONE) => {
                for _ in 0..reps {
                    self.move_dashboard_selection_spatial(SpatialDir::Down);
                }
            }
            (Tab, _) => self.move_dashboard_selection(reps as isize),
            (BackTab, _) => self.move_dashboard_selection(-(reps as isize)),
            (Enter, _) | (Char('v'), M::NONE) => self.zoom_selected_chart(),
            (Char(':'), M::NONE) | (Char(':'), M::SHIFT) => self.prefill_command(""),
            (Char('?'), _) => self.open_help(),
            (Char('m'), M::NONE) => self.enter_tile_move(),
            (Char('s'), M::NONE) => self.enter_tile_resize(),
            (Char('d'), M::NONE) => self.enter_tile_confirm_delete(),
            (Char('a'), M::NONE) => self.enter_tile_add_pick(),
            (Char('d'), M::CONTROL) => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_add(10)
            }
            (Char('u'), M::CONTROL) => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_sub(10)
            }
            (Char('f'), M::CONTROL) => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_add(20)
            }
            (Char('b'), M::CONTROL) => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_sub(20)
            }
            (Char('g'), M::NONE) => self.dashboard_scroll = 0,
            (Char('G'), M::NONE) | (Char('G'), M::SHIFT) => self.dashboard_scroll = u16::MAX,
            _ => {}
        }
    }

    pub(super) fn enter_tile_move(&mut self) {
        let Some((original_layout, original_id)) = self.snapshot_full_layout() else {
            self.status = "no tile selected".to_string();
            return;
        };
        self.tile_submode = TileSubMode::Move {
            original_layout,
            original_id,
            dx: 0,
            dy: 0,
        };
        self.status = "MOVE: arrows = nudge, Enter = commit, Esc = cancel".to_string();
    }

    pub(super) fn enter_tile_resize(&mut self) {
        let Some((original_layout, original_id)) = self.snapshot_full_layout() else {
            self.status = "no tile selected".to_string();
            return;
        };
        self.tile_submode = TileSubMode::Resize {
            original_layout,
            original_id,
            dw: 0,
            dh: 0,
        };
        self.status =
            "RESIZE: Right/Down grow, Left/Up shrink, Enter = commit, Esc = cancel".to_string();
    }

    pub(super) fn enter_tile_confirm_delete(&mut self) {
        if self.current_chart_id().is_none() {
            self.status = "no tile selected".to_string();
            return;
        }
        self.tile_submode = TileSubMode::ConfirmDelete;
        self.status = "DELETE: y to confirm, any other key cancels".to_string();
    }

    pub(super) fn enter_tile_add_pick(&mut self) {
        if self.loaded_dashboard.is_none() {
            self.status = "no dashboard loaded".to_string();
            return;
        }
        self.tile_submode = TileSubMode::PickViz {
            cursor: 0,
            action: PickVizAction::Add,
        };
        self.status = "ADD: arrows pick kind, Enter inserts, Esc cancels".to_string();
    }

    pub(super) fn handle_move_key(
        &mut self,
        key: KeyEvent,
        original_layout: Vec<crate::axiom::LayoutItem>,
        original_id: String,
        cur_dx: i32,
        cur_dy: i32,
    ) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Compute the new cumulative delta from this key.
        let (ndx, ndy) = match (key.code, key.modifiers) {
            (Left, _) | (Char('h'), M::NONE) => (cur_dx - 1, cur_dy),
            (Right, _) | (Char('l'), M::NONE) => (cur_dx + 1, cur_dy),
            (Up, _) | (Char('k'), M::NONE) => (cur_dx, cur_dy - 1),
            (Down, _) | (Char('j'), M::NONE) => (cur_dx, cur_dy + 1),
            (Enter, _) => {
                // Promote the cumulative-preview's pre-move layout
                // into the one-level undo slot before clearing the
                // submode — `u` after the commit then restores the
                // layout the user started with.
                self.snapshot_layout_for_undo(original_layout);
                self.tile_submode = TileSubMode::Idle;
                self.status = "move committed".to_string();
                return;
            }
            (Esc, _) => return self.revert_full_layout(original_layout),
            _ => return,
        };
        self.try_apply_move_preview(&original_layout, &original_id, ndx, ndy);
    }

    pub(super) fn handle_resize_key(
        &mut self,
        key: KeyEvent,
        original_layout: Vec<crate::axiom::LayoutItem>,
        original_id: String,
        cur_dw: i32,
        cur_dh: i32,
    ) {
        use KeyCode::*;
        use KeyModifiers as M;
        let (ndw, ndh) = match (key.code, key.modifiers) {
            (Right, _) | (Char('l'), M::NONE) => (cur_dw + 1, cur_dh),
            (Left, _) | (Char('h'), M::NONE) => (cur_dw - 1, cur_dh),
            (Down, _) | (Char('j'), M::NONE) => (cur_dw, cur_dh + 1),
            (Up, _) | (Char('k'), M::NONE) => (cur_dw, cur_dh - 1),
            (Enter, _) => {
                self.snapshot_layout_for_undo(original_layout);
                self.tile_submode = TileSubMode::Idle;
                self.status = "resize committed".to_string();
                return;
            }
            (Esc, _) => return self.revert_full_layout(original_layout),
            _ => return,
        };
        self.try_apply_resize_preview(&original_layout, &original_id, ndw, ndh);
    }

    pub(super) fn handle_confirm_delete_key(&mut self, key: KeyEvent) {
        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.tile_submode = TileSubMode::Idle;
            self.status = "delete cancelled".to_string();
            return;
        }
        let Some(id) = self.current_chart_id() else {
            self.tile_submode = TileSubMode::Idle;
            return;
        };
        if let Some(resource) = self.loaded_dashboard.as_mut()
            && tile_ops::delete(
                &mut resource.dashboard.charts,
                &mut resource.dashboard.layout,
                &id,
            )
            .is_ok()
        {
            self.dashboard_dirty = true;
            let n = resource.dashboard.charts.len();
            if self.selected_chart_idx >= n {
                self.selected_chart_idx = n.saturating_sub(1);
            }
            self.status = format!("deleted tile {id}");
        }
        self.tile_submode = TileSubMode::Idle;
    }

    /// Unified modal keymap for the viz-kind picker shared by `a`
    /// (add) and `o`/`O` (open new row). Up/Down navigate; Enter
    /// commits via [`PickVizAction`]:
    ///
    /// * [`PickVizAction::Add`] — insert at the first free grid slot
    ///   via [`tile_ops::insert_tile`].
    /// * [`PickVizAction::Open`] — open `remaining` new rows above /
    ///   below the focused tile via [`App::open_new_row_with_kind`].
    ///   `5o` snapshots once on the first commit and reuses the
    ///   picked kind for the remaining repetitions.
    pub(super) fn handle_pick_viz_key(
        &mut self,
        key: KeyEvent,
        cursor: usize,
        action: PickVizAction,
    ) {
        let kinds = add_pick_kinds();
        let n = kinds.len();
        use KeyCode::*;
        let label_cancel = match action {
            PickVizAction::Add => "add cancelled",
            PickVizAction::Open { .. } => "open cancelled",
        };
        match (key.code, key.modifiers) {
            (Esc, _) => {
                self.tile_submode = TileSubMode::Idle;
                self.status = label_cancel.to_string();
            }
            (Up, _) | (Char('k'), _) => {
                self.tile_submode = TileSubMode::PickViz {
                    cursor: (cursor + n - 1) % n,
                    action,
                }
            }
            (Down, _) | (Char('j'), _) => {
                self.tile_submode = TileSubMode::PickViz {
                    cursor: (cursor + 1) % n,
                    action,
                }
            }
            (Enter, _) => {
                let kind = kinds[cursor];
                self.snapshot_dashboard_for_undo();
                match action {
                    PickVizAction::Add => {
                        if let Some(resource) = self.loaded_dashboard.as_mut() {
                            let id = tile_ops::insert_tile(
                                &mut resource.dashboard.charts,
                                &mut resource.dashboard.layout,
                                kind,
                                "new tile",
                            );
                            self.dashboard_dirty = true;
                            self.selected_chart_idx = resource.dashboard.charts.len() - 1;
                            self.status = format!("added {} tile {id}", kind.as_str());
                        }
                    }
                    PickVizAction::Open { above, remaining } => {
                        let mut placed = 0usize;
                        for _ in 0..remaining {
                            if !self.open_new_row_with_kind(above, kind) {
                                break;
                            }
                            placed += 1;
                        }
                        let label = if above {
                            "opened above"
                        } else {
                            "opened below"
                        };
                        self.status = format!("{label}: {placed} {}", kind.as_str());
                    }
                }
                self.tile_submode = TileSubMode::Idle;
            }
            _ => {}
        }
    }
}
