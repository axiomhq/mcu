use super::*;

impl App {
    pub(super) fn handle_ctrl_w_followup(&mut self, key: KeyEvent) {
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
            Params => {
                if grid {
                    Dashboard
                } else {
                    Editor
                }
            }
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

    pub(in crate::app) fn set_focus(&mut self, pane: Pane) {
        if pane == Pane::Legend && self.series.is_empty() {
            self.status = "no series to focus".to_string();
            return;
        }
        self.focus = pane;
        if pane != Pane::Legend {
            self.legend.details_visible = false;
        }
        if pane == Pane::Params {
            // Clamp on entry so a stale index from a previous buffer
            // shape doesn't render off the end.
            let n = self.param_rows().len();
            if n == 0 {
                self.params.selected = 0;
            } else if self.params.selected >= n {
                self.params.selected = n - 1;
            }
        }
    }

    pub(super) fn handle_params_key(&mut self, key: KeyEvent) {
        let rows = self.param_rows();
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('h'), M::NONE) | (Left, _) => self.set_focus(Pane::Editor),
            (Char('j'), M::NONE) | (Down, _) => self.move_params_selection(1, &rows),
            (Char('k'), M::NONE) | (Up, _) => self.move_params_selection(-1, &rows),
            (Char('g'), M::NONE) => self.params.selected = 0,
            (Char('G'), _) if !rows.is_empty() => self.params.selected = rows.len() - 1,
            // `a` / `i` — add new param. Drop into `:p ` and type `NAME=VALUE`.
            (Char('a'), M::NONE) | (Char('i'), M::NONE) => self.prefill_command("p "),
            // `e` / `Enter` — edit selected row, pre-filled with current value.
            (Char('e'), M::NONE) | (Enter, _) => {
                if let Some(row) = rows.get(self.params.selected) {
                    let v = row.value.as_deref().unwrap_or("");
                    self.prefill_command(&format!("p {}={}", row.name, v));
                }
            }
            // `x` clears the selected value.
            (Char('x'), M::NONE) => {
                if let Some(row) = rows.get(self.params.selected).cloned() {
                    self.status = if self.params.cli.remove(&row.name).is_some() {
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

    pub(super) fn move_params_selection(&mut self, delta: i32, rows: &[crate::params::ParamRow]) {
        if rows.is_empty() {
            self.params.selected = 0;
            return;
        }
        let n = rows.len() as i32;
        let cur = self.params.selected as i32;
        let next = (cur + delta).rem_euclid(n);
        self.params.selected = next as usize;
    }

    pub(super) fn handle_legend_key(&mut self, key: KeyEvent) {
        // Details modal owns its own bindings while open.
        if self.legend.details_visible {
            self.handle_legend_details_key(key);
            return;
        }

        use KeyCode::*;
        use KeyModifiers as M;
        // Vim `gg` jump: first `g` arms `pending_g`, second `g` fires.
        // Any other key resets the flag (matches vim's quasi-modal feel).
        let was_pending_g = self.legend.pending_g;
        self.legend.pending_g = false;
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('h'), M::NONE) | (Left, _) => self.set_focus(Pane::Editor),
            (Char('j'), M::NONE) | (Down, _) => self.move_legend_selection(1),
            (Char('k'), M::NONE) | (Up, _) => self.move_legend_selection(-1),
            (Char('g'), M::NONE) => {
                if was_pending_g {
                    self.legend.selected = 0;
                } else {
                    self.legend.pending_g = true;
                }
            }
            (Char('G'), _) if !self.active_legend_series().is_empty() => {
                self.legend.selected = self.active_legend_series().len() - 1
            }
            (Char(' '), M::NONE) | (Enter, _) => self.legend_toggle_current(),
            (Char('a'), M::NONE) => self.legend_toggle_all(),
            (Char('e'), M::NONE) if !self.active_legend_series().is_empty() => {
                self.legend.details_visible = true;
                self.legend.details_cursor = 0;
            }
            (Char('?'), _) => self.open_help(),
            _ => {}
        }
    }

    /// Move the legend cursor by `delta`, clamping at the edges —
    /// matches vim's default `j`/`k` behaviour where the cursor stops
    /// at the first/last line instead of wrapping.
    pub(super) fn move_legend_selection(&mut self, delta: i32) {
        let n = self.active_legend_series().len();
        if n == 0 {
            return;
        }
        let cur = self.legend.selected as i32;
        let next = (cur + delta).clamp(0, n as i32 - 1);
        self.legend.selected = next as usize;
    }

    pub(super) fn legend_toggle_current(&mut self) {
        if let Some(flag) = self.legend.hidden.get_mut(self.legend.selected) {
            *flag = !*flag;
        }
    }

    pub(super) fn handle_legend_details_key(&mut self, key: KeyEvent) {
        let tag_count = self
            .active_legend_index()
            .and_then(|i| self.active_legend_series().get(i))
            .map(|s| s.tags.len())
            .unwrap_or(0);
        use KeyCode::*;
        use KeyModifiers as M;
        let was_pending_g = self.legend.pending_g;
        self.legend.pending_g = false;
        match (key.code, key.modifiers) {
            (Esc, _) | (Char('e'), M::NONE) => self.legend.details_visible = false,
            (Char('j'), M::NONE) | (Down, _) if tag_count > 0 => {
                self.legend.details_cursor = (self.legend.details_cursor + 1).min(tag_count - 1)
            }
            (Char('k'), M::NONE) | (Up, _) if tag_count > 0 => {
                self.legend.details_cursor = self.legend.details_cursor.saturating_sub(1)
            }
            (Char('g'), M::NONE) => {
                if was_pending_g {
                    self.legend.details_cursor = 0;
                } else {
                    self.legend.pending_g = true;
                }
            }
            (Char('G'), _) if tag_count > 0 => self.legend.details_cursor = tag_count - 1,
            (Char(' '), M::NONE) | (Enter, _) => self.toggle_label_tag_at_cursor(),
            _ => {}
        }
    }

    pub(super) fn toggle_label_tag_at_cursor(&mut self) {
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
            let Some((k, _)) = series.tags.get(self.legend.details_cursor) else {
                return;
            };
            k.clone()
        };
        if let Some(pos) = self.legend.label_tags.iter().position(|kk| kk == &key) {
            self.legend.label_tags.remove(pos);
        } else {
            self.legend.label_tags.push(key);
        }
        self.persist_legend_label_tags();
    }

    /// Smart toggle: if any series is currently hidden, show all; otherwise
    /// hide all. Vim's `:hidden` toggle convention.
    pub(super) fn legend_toggle_all(&mut self) {
        if self.legend.hidden.is_empty() {
            return;
        }
        let any_hidden = self.legend.hidden.iter().any(|h| *h);
        let target = !any_hidden;
        for h in &mut self.legend.hidden {
            *h = target;
        }
    }
}
