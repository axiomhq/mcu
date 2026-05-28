//! Per-pane / per-mode key handlers + the `on_key` dispatch entry
//! point.
//!
//! `App::on_key` is the only public method here — it consumes a raw
//! `KeyEvent`, decides which surface owns the keystroke (overlay,
//! pane, mode), and delegates to the corresponding `handle_*_key`
//! method. The handlers themselves are private to the keys module unless
//! used by sibling app modules/tests; they mutate `App` state and call
//! back into editing / command / completion paths that live in other
//! submodules.

use super::*;

mod cmdline;
mod dashboard;
mod editor;
mod mouse;
mod overlays;
mod panes;
mod trace;

impl App {
    pub fn on_key(&mut self, key: KeyEvent) {
        // Any keystroke may mutate state; flag a repaint up front so
        // every early-return path below still triggers a redraw.
        self.needs_redraw = true;
        // Overlays own their keymap entirely when visible; checked
        // before pane / mode dispatch so motion keys don't bleed
        // through. Picker > time > help > dashinfo > tile-inspect.
        if self.dashboards.visible {
            return self.handle_dashboards_picker_key(key);
        }
        if self.time.picker.is_some() {
            return self.handle_time_picker_key(key);
        }
        if self.help.visible {
            return self.handle_help_key(key);
        }
        if self.dashinfo_visible {
            self.dashinfo_visible = false;
            return;
        }
        if self.history_overlay_visible {
            // Any key dismisses — v1 is read-only, no interaction.
            self.history_overlay_visible = false;
            return;
        }
        if self.tile_inspect_json.is_some() {
            self.tile_inspect_json = None;
            return;
        }

        // `Ctrl-w` is the window-prefix in Normal/Visual modes only.
        // Vim repurposes it in the two text-entry modes:
        //   - Insert mode: `delete-word-backward` (handled natively by
        //     `tui-textarea` when we let the key reach `editor.input`).
        //   - Command mode: `delete-word-backward` (handled by our own
        //     `CmdLine::delete_word_backward` in `handle_command_key`).
        // To switch windows from either entry mode, press Esc first.
        if self.pending_ctrl_w {
            self.pending_ctrl_w = false;
            return self.handle_ctrl_w_followup(key);
        }
        let in_text_entry = matches!(self.mode, Mode::Command | Mode::Insert);
        if !in_text_entry
            && key.modifiers == KeyModifiers::CONTROL
            && key.code == KeyCode::Char('w')
        {
            self.pending_ctrl_w = true;
            return;
        }

        // Legend / params / dashboard / table own their own bindings
        // when focused.
        match self.focus {
            Pane::Legend => return self.handle_legend_key(key),
            Pane::Params => return self.handle_params_key(key),
            Pane::Dashboard => return self.handle_dashboard_key(key),
            Pane::Table => return self.handle_table_key(key),
            Pane::TraceTree => return self.handle_trace_key(key),
            Pane::TraceDetail => return self.handle_trace_detail_key(key),
            Pane::Editor => {}
        }
        match self.mode {
            Mode::Insert => self.handle_insert_key(key),
            Mode::Normal => self.handle_normal_key(key),
            Mode::Command => self.handle_command_key(key),
            Mode::Visual | Mode::VisualLine => self.handle_visual_key(key),
        }
    }
}
