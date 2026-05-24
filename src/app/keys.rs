//! Per-pane / per-mode key handlers + the `on_key` dispatch entry
//! point.
//!
//! `App::on_key` is the only public method here — it consumes a raw
//! `KeyEvent`, decides which surface owns the keystroke (overlay,
//! pane, mode), and delegates to the corresponding `handle_*_key`
//! method. The handlers themselves are private; they mutate `App`
//! state and call back into editing / command / completion paths
//! that live in other submodules.

use super::*;

impl App {

    pub fn on_key(&mut self, key: KeyEvent) {
        // Overlays own their keymap entirely when visible; checked
        // before pane / mode dispatch so motion keys don't bleed
        // through. Picker > time > help > dashinfo > tile-inspect.
        if self.dashboards.visible { return self.handle_dashboards_picker_key(key); }
        if self.time_picker.is_some() { return self.handle_time_picker_key(key); }
        if self.help_visible { return self.handle_help_key(key); }
        if self.dashinfo_visible { self.dashinfo_visible = false; return; }
        if self.tile_inspect_json.is_some() { self.tile_inspect_json = None; return; }

        // `Ctrl-w` is the window-prefix in any mode; the next key picks
        // the target pane. Handled before pane/mode dispatch so it works
        // from Insert, Visual, and the legend itself.
        if self.pending_ctrl_w {
            self.pending_ctrl_w = false;
            return self.handle_ctrl_w_followup(key);
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('w') {
            self.pending_ctrl_w = true;
            return;
        }

        // Legend / params / dashboard own their own bindings when focused.
        match self.focus {
            Pane::Legend => return self.handle_legend_key(key),
            Pane::Params => return self.handle_params_key(key),
            Pane::Dashboard => return self.handle_dashboard_key(key),
            Pane::Editor => {}
        }
        match self.mode {
            Mode::Insert => self.handle_insert_key(key),
            Mode::Normal => self.handle_normal_key(key),
            Mode::Command => self.handle_command_key(key),
            Mode::Visual | Mode::VisualLine => self.handle_visual_key(key),
        }
    }

    /// Keymap for the dashboard grid pane. The dispatch order is:
    ///
    ///   1. Active sub-mode (Move/Resize/ConfirmDelete/AddPick) owns
    ///      every key while engaged — Esc cancels back to Idle.
    ///   2. `Idle` accepts the navigation + entry-point shortcuts
    ///      (m, s, d, a, v, R, Enter, hjkl/arrows, Tab).
    fn handle_dashboard_key(&mut self, key: KeyEvent) {
        // Sub-mode takes precedence.
        match self.tile_submode.clone() {
            TileSubMode::Move { original } => return self.handle_move_key(key, original),
            TileSubMode::Resize { original } => return self.handle_resize_key(key, original),
            TileSubMode::ConfirmDelete => return self.handle_confirm_delete_key(key),
            TileSubMode::AddPick { cursor } => return self.handle_add_pick_key(key, cursor),
            TileSubMode::Idle => {}
        }
        // `:` drops into the ex-command line while preserving the
        // current pane so Enter/Esc returns to the grid; `?` opens
        // the help modal (dismissal is centralised in `on_key`).
        // `j`/`k` are owned by spatial nav, so vertical scroll uses
        // vim's by-screen bindings; the renderer clamps each frame.
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) => self.focus = Pane::Editor,
            (Left, _) | (Char('h'), M::NONE) =>
                self.move_dashboard_selection_spatial(SpatialDir::Left),
            (Right, _) | (Char('l'), M::NONE) =>
                self.move_dashboard_selection_spatial(SpatialDir::Right),
            (Up, _) | (Char('k'), M::NONE) =>
                self.move_dashboard_selection_spatial(SpatialDir::Up),
            (Down, _) | (Char('j'), M::NONE) =>
                self.move_dashboard_selection_spatial(SpatialDir::Down),
            (Tab, _) => self.move_dashboard_selection(1),
            (BackTab, _) => self.move_dashboard_selection(-1),
            (Enter, _) | (Char('v'), M::NONE) => self.zoom_selected_chart(),
            (Char(':'), M::NONE) | (Char(':'), M::SHIFT) => self.prefill_command(""),
            (Char('?'), _) => self.open_help(),
            (Char('m'), M::NONE) => self.enter_tile_move(),
            (Char('s'), M::NONE) => self.enter_tile_resize(),
            (Char('d'), M::NONE) => self.enter_tile_confirm_delete(),
            (Char('a'), M::NONE) => self.enter_tile_add_pick(),
            (Char('d'), M::CONTROL) =>
                self.dashboard_scroll = self.dashboard_scroll.saturating_add(10),
            (Char('u'), M::CONTROL) =>
                self.dashboard_scroll = self.dashboard_scroll.saturating_sub(10),
            (Char('f'), M::CONTROL) =>
                self.dashboard_scroll = self.dashboard_scroll.saturating_add(20),
            (Char('b'), M::CONTROL) =>
                self.dashboard_scroll = self.dashboard_scroll.saturating_sub(20),
            (Char('g'), M::NONE) => self.dashboard_scroll = 0,
            (Char('G'), M::NONE) | (Char('G'), M::SHIFT) => self.dashboard_scroll = u16::MAX,
            _ => {}
        }
    }

    fn enter_tile_move(&mut self) {
        let Some(original) = self.snapshot_selected_layout() else {
            self.status = "no tile selected".to_string();
            return;
        };
        self.tile_submode = TileSubMode::Move { original };
        self.status = "MOVE: arrows = nudge, Enter = commit, Esc = cancel".to_string();
    }

    fn enter_tile_resize(&mut self) {
        let Some(original) = self.snapshot_selected_layout() else {
            self.status = "no tile selected".to_string();
            return;
        };
        self.tile_submode = TileSubMode::Resize { original };
        self.status =
            "RESIZE: Right/Down grow, Left/Up shrink, Enter = commit, Esc = cancel".to_string();
    }

    fn enter_tile_confirm_delete(&mut self) {
        if self.current_chart_id().is_none() {
            self.status = "no tile selected".to_string();
            return;
        }
        self.tile_submode = TileSubMode::ConfirmDelete;
        self.status = "DELETE: y to confirm, any other key cancels".to_string();
    }

    fn enter_tile_add_pick(&mut self) {
        if self.loaded_dashboard.is_none() {
            self.status = "no dashboard loaded".to_string();
            return;
        }
        self.tile_submode = TileSubMode::AddPick { cursor: 0 };
        self.status = "ADD: arrows pick kind, Enter inserts, Esc cancels".to_string();
    }

    fn handle_move_key(&mut self, key: KeyEvent, original: crate::axiom::LayoutItem) {
        let Some(id) = self.current_chart_id() else {
            self.tile_submode = TileSubMode::Idle;
            return;
        };
        let mut translate = |dx: i32, dy: i32| {
            let Some(resource) = self.loaded_dashboard.as_mut() else { return };
            match tile_ops::translate(&mut resource.dashboard.layout, &id, dx, dy) {
                Ok(()) => self.dashboard_dirty = true,
                Err(reason) => self.status = format!("move blocked: {reason}"),
            }
        };
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Left, _) | (Char('h'), M::NONE) => translate(-1, 0),
            (Right, _) | (Char('l'), M::NONE) => translate(1, 0),
            (Up, _) | (Char('k'), M::NONE) => translate(0, -1),
            (Down, _) | (Char('j'), M::NONE) => translate(0, 1),
            (Enter, _) => {
                self.tile_submode = TileSubMode::Idle;
                self.status = "move committed".to_string();
            }
            (Esc, _) => self.revert_layout(original),
            _ => {}
        }
    }

    fn handle_resize_key(&mut self, key: KeyEvent, original: crate::axiom::LayoutItem) {
        let Some(id) = self.current_chart_id() else {
            self.tile_submode = TileSubMode::Idle;
            return;
        };
        let mut resize = |dw: i32, dh: i32| {
            let Some(resource) = self.loaded_dashboard.as_mut() else { return };
            match tile_ops::resize(&mut resource.dashboard.layout, &id, dw, dh) {
                Ok(()) => self.dashboard_dirty = true,
                Err(reason) => self.status = format!("resize blocked: {reason}"),
            }
        };
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Right, _) | (Char('l'), M::NONE) => resize(1, 0),
            (Left, _) | (Char('h'), M::NONE) => resize(-1, 0),
            (Down, _) | (Char('j'), M::NONE) => resize(0, 1),
            (Up, _) | (Char('k'), M::NONE) => resize(0, -1),
            (Enter, _) => {
                self.tile_submode = TileSubMode::Idle;
                self.status = "resize committed".to_string();
            }
            (Esc, _) => self.revert_layout(original),
            _ => {}
        }
    }

    fn handle_confirm_delete_key(&mut self, key: KeyEvent) {
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

    fn handle_add_pick_key(&mut self, key: KeyEvent, cursor: usize) {
        let kinds = add_pick_kinds();
        let n = kinds.len();
        use KeyCode::*;
        match (key.code, key.modifiers) {
            (Esc, _) => {
                self.tile_submode = TileSubMode::Idle;
                self.status = "add cancelled".to_string();
            }
            (Up, _) | (Char('k'), _) =>
                self.tile_submode = TileSubMode::AddPick { cursor: (cursor + n - 1) % n },
            (Down, _) | (Char('j'), _) =>
                self.tile_submode = TileSubMode::AddPick { cursor: (cursor + 1) % n },
            (Enter, _) => {
                let kind = kinds[cursor];
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
                self.tile_submode = TileSubMode::Idle;
            }
            _ => {}
        }
    }

    fn handle_ctrl_w_followup(&mut self, key: KeyEvent) {
        // Spatial layout (matches the rendered grid):
        //   +---------+---+
        //   |  graph  | L |   (top:    Legend)
        //   +---------+---+
        //   |  editor | P |   (bottom: Params)
        //   +---------+---+
        // In Grid view the graph slot is the Dashboard pane, so the
        // top-left neighbour of Legend is Dashboard (not Editor).
        // `w` cycles Editor → Legend → Params → (Dashboard if Grid)
        // → Editor; directional keys use the layout to pick the
        // spatial neighbour and fall back to the source pane when
        // there's no neighbour in that direction.
        use KeyCode::*;
        use KeyModifiers as M;
        use Pane::*;
        let grid = self.view_mode == ViewMode::Grid;
        let has_dash = grid && self.loaded_dashboard.is_some();
        // `w` cycles Editor → Legend → Params → (Dashboard if Grid) → Editor.
        let cycle = || match self.focus {
            Editor => Legend,
            Legend => Params,
            Params => if grid { Dashboard } else { Editor },
            Dashboard => Editor,
        };
        let next = match (key.code, key.modifiers) {
            (Char('w'), _) => cycle(),
            // `Ctrl-w d` jumps straight to the dashboard pane.
            (Char('d'), _) if has_dash => Dashboard,
            (Char('d'), _) => {
                self.status = ":Ctrl-w d: no grid view".to_string();
                return;
            }
            // Directional moves: in Grid, Legend's left is Dashboard;
            // otherwise Editor. Dashboard's right is Legend.
            (Char('h'), M::NONE) | (Left, _) => match self.focus {
                Legend if has_dash => Dashboard,
                Legend | Params => Editor,
                p => p,
            },
            (Char('l'), M::NONE) | (Right, _) => match self.focus {
                Editor => Params,
                Dashboard => Legend,
                p => p,
            },
            (Char('j'), M::NONE) | (Down, _) => match self.focus {
                Legend => Params,
                Dashboard => Editor,
                p => p,
            },
            (Char('k'), M::NONE) | (Up, _) => match self.focus {
                Params => Legend,
                Editor if grid => Dashboard,
                Editor => Legend,
                p => p,
            },
            _ => return,
        };
        self.set_focus(next);
    }

    pub(super) fn set_focus(&mut self, pane: Pane) {
        if pane == Pane::Legend && self.series.is_empty() {
            self.status = "no series to focus".to_string();
            return;
        }
        self.focus = pane;
        if pane != Pane::Legend {
            self.legend_details_visible = false;
        }
        if pane == Pane::Params {
            // Clamp on entry so a stale index from a previous buffer
            // shape doesn't render off the end.
            let n = self.param_rows().len();
            if n == 0 {
                self.params_selected = 0;
            } else if self.params_selected >= n {
                self.params_selected = n - 1;
            }
        }
    }

    fn handle_params_key(&mut self, key: KeyEvent) {
        let rows = self.param_rows();
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('h'), M::NONE) | (Left, _) => self.set_focus(Pane::Editor),
            (Char('j'), M::NONE) | (Down, _) => self.move_params_selection(1, &rows),
            (Char('k'), M::NONE) | (Up, _) => self.move_params_selection(-1, &rows),
            (Char('g'), M::NONE) => self.params_selected = 0,
            (Char('G'), _) if !rows.is_empty() => self.params_selected = rows.len() - 1,
            // `a` / `i` — add new param. Drop into `:p ` and type `NAME=VALUE`.
            (Char('a'), M::NONE) | (Char('i'), M::NONE) => self.prefill_command("p "),
            // `e` / `Enter` — edit selected row, pre-filled with current value.
            (Char('e'), M::NONE) | (Enter, _) => {
                if let Some(row) = rows.get(self.params_selected) {
                    let v = row.value.as_deref().unwrap_or("");
                    self.prefill_command(&format!("p {}={}", row.name, v));
                }
            }
            // `x` clears the selected value.
            (Char('x'), M::NONE) => {
                if let Some(row) = rows.get(self.params_selected).cloned() {
                    self.status = if self.cli_params.remove(&row.name).is_some() {
                        format!("cleared ${}", row.name)
                    } else {
                        format!("${} not set", row.name)
                    };
                }
            }
            (Char('?'), _) => self.open_help(),
            _ => {}
        }
    }

    fn move_params_selection(&mut self, delta: i32, rows: &[crate::params::ParamRow]) {
        if rows.is_empty() {
            self.params_selected = 0;
            return;
        }
        let n = rows.len() as i32;
        let cur = self.params_selected as i32;
        let next = (cur + delta).rem_euclid(n);
        self.params_selected = next as usize;
    }

    /// Drop into Command mode with `text` already on the line and the
    /// cursor at the end. Shared by the params pane's add/edit bindings.
    /// Remembers the current pane so the cmdline can return focus to it
    /// once the command is submitted or cancelled.
    fn prefill_command(&mut self, text: &str) {
        self.cmdline_return_focus = Some(self.focus);
        self.cmdline.reset();
        self.cmdline.buf = text.to_string();
        self.cmdline.cursor = self.cmdline.buf.chars().count();
        self.mode = Mode::Command;
        self.status = String::new();
        // The cmdline lives at the bottom of the screen and consumes
        // keys through `handle_command_key` while `mode == Command`;
        // pane focus is irrelevant during that period. We drop to
        // Editor so any pane-specific key handlers stop firing.
        self.focus = Pane::Editor;
    }

    /// Restore pane focus after the command line closes. Used by both
    /// the Enter and Esc paths so cancelling a prefilled `:p` also
    /// brings the user back to the pane they came from.
    fn restore_cmdline_focus(&mut self) {
        if let Some(pane) = self.cmdline_return_focus.take() {
            // `set_focus` enforces the same invariants as any other
            // focus change (e.g. won't focus Legend with no series).
            self.set_focus(pane);
        }
    }

    fn handle_legend_key(&mut self, key: KeyEvent) {
        // Details modal owns its own bindings while open.
        if self.legend_details_visible {
            self.handle_legend_details_key(key);
            return;
        }

        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('h'), M::NONE) | (Left, _) => self.set_focus(Pane::Editor),
            (Char('j'), M::NONE) | (Down, _) => self.move_legend_selection(1),
            (Char('k'), M::NONE) | (Up, _) => self.move_legend_selection(-1),
            // `gg` to top is just `g` here — legend's own one-key state.
            (Char('g'), M::NONE) => self.legend_selected = 0,
            (Char('G'), _) if !self.active_legend_series().is_empty() =>
                self.legend_selected = self.active_legend_series().len() - 1,
            (Char(' '), M::NONE) | (Enter, _) => self.legend_toggle_current(),
            (Char('a'), M::NONE) => self.legend_toggle_all(),
            (Char('e'), M::NONE) if !self.active_legend_series().is_empty() => {
                self.legend_details_visible = true;
                self.details_cursor = 0;
            }
            (Char('?'), _) => self.open_help(),
            _ => {}
        }
    }

    fn move_legend_selection(&mut self, delta: i32) {
        let n = self.active_legend_series().len();
        if n == 0 {
            return;
        }
        let n = n as i32;
        let cur = self.legend_selected as i32;
        let next = (cur + delta).rem_euclid(n);
        self.legend_selected = next as usize;
    }

    fn legend_toggle_current(&mut self) {
        if let Some(flag) = self.legend_hidden.get_mut(self.legend_selected) {
            *flag = !*flag;
        }
    }

    fn handle_legend_details_key(&mut self, key: KeyEvent) {
        let tag_count = self
            .active_legend_index()
            .and_then(|i| self.active_legend_series().get(i))
            .map(|s| s.tags.len())
            .unwrap_or(0);
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('e'), M::NONE) =>
                self.legend_details_visible = false,
            (Char('j'), M::NONE) | (Down, _) if tag_count > 0 =>
                self.details_cursor = (self.details_cursor + 1) % tag_count,
            (Char('k'), M::NONE) | (Up, _) if tag_count > 0 =>
                self.details_cursor = self.details_cursor.checked_sub(1).unwrap_or(tag_count - 1),
            (Char('g'), M::NONE) => self.details_cursor = 0,
            (Char('G'), _) if tag_count > 0 => self.details_cursor = tag_count - 1,
            (Char(' '), M::NONE) | (Enter, _) => self.toggle_label_tag_at_cursor(),
            _ => {}
        }
    }

    fn toggle_label_tag_at_cursor(&mut self) {
        // Clone the key first so we don't hold a borrow across the
        // mutation of `legend_label_tags`.
        let key = {
            let Some(idx) = self.active_legend_index() else {
                return;
            };
            let series_slice = self.active_legend_series();
            let Some(series) = series_slice.get(idx) else {
                return;
            };
            let Some((k, _)) = series.tags.get(self.details_cursor) else {
                return;
            };
            k.clone()
        };
        if let Some(pos) = self.legend_label_tags.iter().position(|kk| kk == &key) {
            self.legend_label_tags.remove(pos);
        } else {
            self.legend_label_tags.push(key);
        }
        self.persist_legend_label_tags();
    }

    /// Smart toggle: if any series is currently hidden, show all; otherwise
    /// hide all. Vim's `:hidden` toggle convention.
    fn legend_toggle_all(&mut self) {
        if self.legend_hidden.is_empty() {
            return;
        }
        let any_hidden = self.legend_hidden.iter().any(|h| *h);
        let target = !any_hidden;
        for h in &mut self.legend_hidden {
            *h = target;
        }
    }

    fn handle_insert_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Completion popup intercepts a small set of keys.
        if self.completions.visible {
            match (key.code, key.modifiers) {
                (Esc, _) => return self.completions.hide(),
                (Tab, M::NONE) | (Enter, M::NONE) => return self.accept_completion(),
                (Up, _) | (Char('p'), M::CONTROL) => return self.move_completion_selection(-1),
                (Down, _) | (Char('n'), M::CONTROL) => return self.move_completion_selection(1),
                _ => {}
            }
        }

        // Trigger keys: Tab and Ctrl-Space.
        if matches!((key.code, key.modifiers), (Tab, M::NONE) | (Char(' '), M::CONTROL)) {
            return self.open_completions();
        }
        if key.code == Esc {
            self.mode = Mode::Normal;
            return;
        }
        if self.editor.input(key) {
            if self.completions.visible {
                self.refresh_completions();
            }
            self.recompute_diagnostics();
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Hover popup: any key other than `K` dismisses it.
        if self.hover.is_some() && !matches!((key.code, key.modifiers), (Char('K'), _)) {
            self.hover = None;
        }
        // The quick-fix picker takes over a small set of keys while visible.
        if self.quickfix.visible {
            match (key.code, key.modifiers) {
                (Esc, _) => return self.quickfix.hide(),
                (Enter, _) => return self.accept_quickfix(),
                (Up, _) | (Char('p'), M::CONTROL) => return self.move_quickfix_selection(-1),
                (Down, _) | (Char('n'), M::CONTROL) => return self.move_quickfix_selection(1),
                _ => return,
            }
        }
        match self.cmd_parser.feed(key) {
            Step::Pending | Step::Cancel => {}
            Step::Emit(cmd) => self.run_command(cmd),
        }
        // Any keystroke may have moved the cursor or edited the buffer;
        // refresh the signature-help line so the status bar follows.
        self.recompute_sig_help();
    }

    /// Visual-mode key handler. Motion keys go through the same parser
    /// (we only consume `Command::Move` emissions); operator keys collapse
    /// the current selection into a range and apply it.
    fn handle_visual_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('v'), M::NONE) => return self.exit_visual(),
            (Char('V'), _) => {
                self.mode = Mode::VisualLine;
                return;
            }
            (Char(op), _) if matches!(op, 'd' | 'c' | 'y' | 'x' | '>' | '<') => {
                let operator = match op {
                    'd' | 'x' => Operator::Delete,
                    'c' => Operator::Change,
                    'y' => Operator::Yank,
                    '>' => Operator::IndentRight,
                    '<' => Operator::IndentLeft,
                    _ => unreachable!(),
                };
                return self.apply_visual(operator);
            }
            _ => {}
        }
        // Otherwise: feed the parser but only honour pure-motion emissions.
        // Operators / find-char / etc. are dropped — user can Esc back to Normal.
        if let Step::Emit(Command::Move { motion, count }) = self.cmd_parser.feed(key) {
            self.apply_motion(motion, count);
        }
        self.recompute_sig_help();
    }

    pub(super) fn enter_command_mode(&mut self) {
        self.cmdline.reset();
        self.mode = Mode::Command;
        self.status = String::new();
    }

    /// Modal keymap for the help overlay. j/k/Up/Down/Ctrl-d/u scroll;
    /// g/G jump to top/bottom; any other key dismisses (including
    /// Esc, q, and `?` itself — the modal behaves like a peek).
    fn handle_help_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        let scroll = &mut self.help_scroll;
        match (key.code, key.modifiers) {
            (Down, _) | (Char('j'), M::NONE) => *scroll = scroll.saturating_add(1),
            (Up, _) | (Char('k'), M::NONE) => *scroll = scroll.saturating_sub(1),
            (Char('d'), M::CONTROL) => *scroll = scroll.saturating_add(10),
            (Char('u'), M::CONTROL) => *scroll = scroll.saturating_sub(10),
            (PageDown, _) | (Char('f'), M::CONTROL) => *scroll = scroll.saturating_add(20),
            (PageUp, _) | (Char('b'), M::CONTROL) => *scroll = scroll.saturating_sub(20),
            (Char('g'), M::NONE) => *scroll = 0,
            (Char('G'), _) => *scroll = u16::MAX,
            _ => self.help_visible = false,
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Tab / Shift-Tab drive the completion popup. Every other key
        // (besides navigation/accept) hides it so successive insert +
        // tab cycles always start from a fresh candidate set.
        match (key.code, key.modifiers) {
            (Tab, _) => return self.handle_cmdline_tab(false),
            (BackTab, _) => return self.handle_cmdline_tab(true),
            (Up, _) | (Down, _) | (Enter, _) | (Esc, _) | (Char('c'), M::CONTROL) => {}
            _ => self.cmdline_completions.hide(),
        }
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('c'), M::CONTROL) => {
                self.cmdline.reset();
                self.cmdline_completions.hide();
                self.mode = Mode::Normal;
                self.restore_cmdline_focus();
            }
            (Up, _) if self.cmdline_completions.visible => self.move_cmdline_completion(-1),
            (Down, _) if self.cmdline_completions.visible => self.move_cmdline_completion(1),
            (Enter, _) if self.cmdline_completions.visible => self.accept_cmdline_completion(),
            (Enter, _) => {
                let cmd = std::mem::take(&mut self.cmdline.buf);
                self.cmdline.cursor = 0;
                self.mode = Mode::Normal;
                self.execute_command(cmd.trim());
                self.restore_cmdline_focus();
            }
            // Empty cmdline + Backspace cancels, like vim.
            (Backspace, _) if self.cmdline.buf.is_empty() => self.mode = Mode::Normal,
            (Backspace, _) => self.cmdline.backspace(),
            (Delete, _) => self.cmdline.delete_forward(),
            (Left, _) => self.cmdline.move_left(),
            (Right, _) => self.cmdline.move_right(),
            (Home, _) | (Char('a'), M::CONTROL) => self.cmdline.move_home(),
            (End, _) | (Char('e'), M::CONTROL) => self.cmdline.move_end(),
            (Char('u'), M::CONTROL) => {
                // Clear from cursor to start — readline standard.
                let to = self.cmdline.byte_cursor();
                self.cmdline.buf.drain(..to);
                self.cmdline.cursor = 0;
            }
            (Char('k'), M::CONTROL) => {
                let from = self.cmdline.byte_cursor();
                self.cmdline.buf.truncate(from);
            }
            (Char(c), m) if m == M::NONE || m == M::SHIFT => self.cmdline.insert_char(c),
            _ => {}
        }
    }

    /// Modal keymap for the `:time` overlay. Dispatches by sub-state:
    /// the preset list takes simple cursor motion + Enter (with the
    /// trailing "Custom…" row transitioning into the calendar view);
    /// the calendar view takes day/week/month navigation + Tab to
    /// switch focus between start and end.
    fn handle_time_picker_key(&mut self, key: KeyEvent) {
        let state = match self.time_picker.take() {
            Some(s) => s,
            None => return,
        };
        match state {
            TimePickerState::Presets { cursor } => {
                self.handle_time_preset_key(cursor, key);
            }
            TimePickerState::Custom(picker) => {
                self.handle_time_custom_key(picker, key);
            }
        }
    }

    fn handle_time_preset_key(&mut self, cursor: usize, key: KeyEvent) {
        // Cursor range is 0..=TIME_PRESETS.len() — the last index is
        // the synthetic "Custom…" row.
        let n = TIME_PRESETS.len() + 1;
        let mut next_cursor = cursor;
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                // Already taken out via `take()`; just leave None.
                return;
            }
            (KeyCode::Enter, _) => {
                if cursor == TIME_PRESET_CUSTOM_INDEX {
                    // Transition to the calendar overlay, seeded from
                    // whatever the dashboard's current window parses
                    // as (defaulting to yesterday→today).
                    let mut picker = CustomRangePicker::seed();
                    if let Some(d) = parse_iso_date(&self.time_range.start) {
                        picker.start = d;
                    }
                    if let Some(d) = parse_iso_date(&self.time_range.end) {
                        picker.end = d;
                    }
                    self.time_picker = Some(TimePickerState::Custom(picker));
                    return;
                }
                let (_, duration) = TIME_PRESETS[cursor];
                self.set_time_range(format!("now-{duration}"), "now".to_string());
                return;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::BackTab, _) =>
                next_cursor = (cursor + n - 1) % n,
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Tab, _) =>
                next_cursor = (cursor + 1) % n,
            (KeyCode::Char('g'), KeyModifiers::NONE) => next_cursor = 0,
            (KeyCode::Char('G'), _) => next_cursor = n - 1,
            _ => {}
        }
        self.time_picker = Some(TimePickerState::Presets { cursor: next_cursor });
    }

    fn handle_time_custom_key(&mut self, mut picker: CustomRangePicker, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Day/week/month shifts and Tab all keep the overlay open;
        // factored into a closure to avoid repeating the wrap+return.
        let keep = |p: CustomRangePicker| Some(TimePickerState::Custom(p));
        self.time_picker = match (key.code, key.modifiers) {
            // Esc steps back to the preset list rather than closing,
            // so the user can undo Custom without losing their place.
            (Esc, _) => Some(TimePickerState::Presets { cursor: TIME_PRESET_CUSTOM_INDEX }),
            (Enter, _) => {
                let (start, end) = picker.to_range();
                self.set_time_range(start, end);
                None
            }
            (Tab, _) | (BackTab, _) | (Char('\t'), _) => {
                picker.focus = match picker.focus {
                    CustomField::Start => CustomField::End,
                    CustomField::End => CustomField::Start,
                };
                keep(picker)
            }
            (Left, _) | (Char('h'), M::NONE) => { picker.shift_days(-1); keep(picker) }
            (Right, _) | (Char('l'), M::NONE) => { picker.shift_days(1); keep(picker) }
            (Up, _) | (Char('k'), M::NONE) => { picker.shift_days(-7); keep(picker) }
            (Down, _) | (Char('j'), M::NONE) => { picker.shift_days(7); keep(picker) }
            (Char('<'), _) | (Char(','), M::SHIFT) | (Char('['), M::NONE) =>
                { picker.shift_month(-1); keep(picker) }
            (Char('>'), _) | (Char('.'), M::SHIFT) | (Char(']'), M::NONE) =>
                { picker.shift_month(1); keep(picker) }
            _ => keep(picker),
        };
    }

    /// Keymap for the dashboard picker overlay. The filter is
    /// edit-as-you-type; printable characters extend it, Backspace
    /// removes the last char, and navigation keys scroll the filtered
    /// list.
    fn handle_dashboards_picker_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) => self.dashboards.hide(),
            (Up, _) | (Char('k'), M::CONTROL) => { self.dashboards.move_cursor(-1); }
            (Down, _) | (Char('j'), M::CONTROL) => { self.dashboards.move_cursor(1); }
            (PageUp, _) => { self.dashboards.move_cursor(-10); }
            (PageDown, _) => { self.dashboards.move_cursor(10); }
            (Enter, _) => {
                if let Some(sel) = self.dashboards.selected() {
                    let uid = sel.uid.clone();
                    let name = sel.name().to_string();
                    self.last_picked_dashboard = Some(uid.clone());
                    self.fetch_dashboard_by_uid(uid.clone());
                    self.status = format!("opening dashboard `{name}` …");
                }
                self.dashboards.hide();
            }
            (Backspace, _) => {
                self.dashboards.filter.pop();
                self.dashboards.cursor = 0;
            }
            (Char(c), m) if !m.contains(M::CONTROL) => {
                self.dashboards.filter.push(c);
                self.dashboards.cursor = 0;
            }
            _ => {}
        }
    }
}
