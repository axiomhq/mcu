use super::*;

impl App {
    /// Modal keymap for the help overlay. j/k/Up/Down/Ctrl-d/u scroll;
    /// g/G jump to top/bottom; any other key dismisses (including
    /// Esc, q, and `?` itself — the modal behaves like a peek).
    pub(super) fn handle_help_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        let scroll = &mut self.help.scroll;
        match (key.code, key.modifiers) {
            (Down, _) | (Char('j'), M::NONE) => *scroll = scroll.saturating_add(1),
            (Up, _) | (Char('k'), M::NONE) => *scroll = scroll.saturating_sub(1),
            (Char('d'), M::CONTROL) => *scroll = scroll.saturating_add(10),
            (Char('u'), M::CONTROL) => *scroll = scroll.saturating_sub(10),
            (PageDown, _) | (Char('f'), M::CONTROL) => *scroll = scroll.saturating_add(20),
            (PageUp, _) | (Char('b'), M::CONTROL) => *scroll = scroll.saturating_sub(20),
            (Char('g'), M::NONE) => *scroll = 0,
            (Char('G'), _) => *scroll = u16::MAX,
            _ => self.help.visible = false,
        }
    }

    /// Modal keymap for the `:time` overlay. Dispatches by sub-state:
    /// the preset list takes simple cursor motion + Enter (with the
    /// trailing "Custom…" row transitioning into the calendar view);
    /// the calendar view takes day/week/month navigation + Tab to
    /// switch focus between start and end.
    pub(super) fn handle_time_picker_key(&mut self, key: KeyEvent) {
        let state = match self.time.picker.take() {
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

    pub(super) fn handle_time_preset_key(&mut self, cursor: usize, key: KeyEvent) {
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
                    if let Some(d) = parse_iso_date(&self.time.range.start) {
                        picker.start = d;
                    }
                    if let Some(d) = parse_iso_date(&self.time.range.end) {
                        picker.end = d;
                    }
                    self.time.picker = Some(TimePickerState::Custom(picker));
                    return;
                }
                let (_, duration) = TIME_PRESETS[cursor];
                self.set_time_range(format!("now-{duration}"), "now".to_string());
                return;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::BackTab, _) => {
                next_cursor = (cursor + n - 1) % n
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Tab, _) => {
                next_cursor = (cursor + 1) % n
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => next_cursor = 0,
            (KeyCode::Char('G'), _) => next_cursor = n - 1,
            _ => {}
        }
        self.time.picker = Some(TimePickerState::Presets {
            cursor: next_cursor,
        });
    }

    pub(super) fn handle_time_custom_key(&mut self, mut picker: CustomRangePicker, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        // Day/week/month shifts and Tab all keep the overlay open;
        // factored into a closure to avoid repeating the wrap+return.
        let keep = |p: CustomRangePicker| Some(TimePickerState::Custom(p));
        self.time.picker = match (key.code, key.modifiers) {
            // Esc steps back to the preset list rather than closing,
            // so the user can undo Custom without losing their place.
            (Esc, _) => Some(TimePickerState::Presets {
                cursor: TIME_PRESET_CUSTOM_INDEX,
            }),
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
            (Left, _) | (Char('h'), M::NONE) => {
                picker.shift_days(-1);
                keep(picker)
            }
            (Right, _) | (Char('l'), M::NONE) => {
                picker.shift_days(1);
                keep(picker)
            }
            (Up, _) | (Char('k'), M::NONE) => {
                picker.shift_days(-7);
                keep(picker)
            }
            (Down, _) | (Char('j'), M::NONE) => {
                picker.shift_days(7);
                keep(picker)
            }
            (Char('<'), _) | (Char(','), M::SHIFT) | (Char('['), M::NONE) => {
                picker.shift_month(-1);
                keep(picker)
            }
            (Char('>'), _) | (Char('.'), M::SHIFT) | (Char(']'), M::NONE) => {
                picker.shift_month(1);
                keep(picker)
            }
            _ => keep(picker),
        };
    }

    /// Keymap for the dashboard picker overlay. The filter is
    /// edit-as-you-type; printable characters extend it, Backspace
    /// removes the last char, and navigation keys scroll the filtered
    /// list.
    pub(super) fn handle_dashboards_picker_key(&mut self, key: KeyEvent) {
        use KeyCode::*;
        use KeyModifiers as M;
        match (key.code, key.modifiers) {
            (Esc, _) => self.dashboards.hide(),
            (Up, _) | (Char('k'), M::CONTROL) => {
                self.dashboards.move_cursor(-1);
            }
            (Down, _) | (Char('j'), M::CONTROL) => {
                self.dashboards.move_cursor(1);
            }
            (PageUp, _) => {
                self.dashboards.move_cursor(-10);
            }
            (PageDown, _) => {
                self.dashboards.move_cursor(10);
            }
            (Enter, _) => {
                if let Some(sel) = self.dashboards.selected() {
                    use crate::axiom::DashboardSummaryExt;
                    let uid = sel.uid.clone();
                    let name = sel.name_or_unnamed().to_string();
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
