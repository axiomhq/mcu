//! `:time` quick-select / two-month custom calendar picker.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};

use super::{centered_area, modal_frame};
use crate::app::App;

/// `:time` quick-select / custom calendar overlay. Dispatches by
/// state variant.
pub(super) fn draw_time_picker_overlay(
    f: &mut Frame,
    app: &App,
    state: &crate::app::TimePickerState,
    screen: Rect,
) {
    match state {
        crate::app::TimePickerState::Presets { cursor } => {
            draw_time_preset_overlay(f, app, *cursor, screen);
        }
        crate::app::TimePickerState::Custom(picker) => {
            draw_time_custom_overlay(f, picker, screen);
        }
    }
}

fn draw_time_preset_overlay(f: &mut Frame, app: &App, cursor: usize, screen: Rect) {
    let presets = crate::app::TIME_PRESETS;
    // +1 entry for the trailing "Custom..." row.
    let row_count = (presets.len() as u16) + 1;
    let width = 36u16.min(screen.width.saturating_sub(4));
    // Borders (2) + title pad (1) + rows + hint (2).
    let height = (row_count + 5).min(screen.height.saturating_sub(2));
    let title = format!(" time · {} → {} ", app.time.range.start, app.time.range.end);
    let inner = modal_frame(f, centered_area(screen, width, height), &title, Color::Cyan);
    if inner.height < 3 {
        return;
    }

    // Reserve the last row for the keymap hint.
    let list_h = inner.height.saturating_sub(1);
    let list_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: list_h,
    };
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + list_h,
        width: inner.width,
        height: 1,
    };

    let mut items: Vec<ListItem<'_>> = presets
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            let style = if i == cursor {
                Style::default()
                    .bg(Color::Rgb(20, 60, 80))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(format!("  last {label}")).style(style)
        })
        .collect();
    let custom_idx = crate::app::TIME_PRESET_CUSTOM_INDEX;
    let custom_style = if cursor == custom_idx {
        Style::default()
            .bg(Color::Rgb(20, 60, 80))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Yellow)
    };
    items.push(ListItem::new("  Custom...").style(custom_style));
    f.render_widget(List::new(items), list_area);
    f.render_widget(
        Paragraph::new("j/k move · Enter apply · Esc close")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(ratatui::layout::Alignment::Center),
        hint_area,
    );
}

fn draw_time_custom_overlay(f: &mut Frame, picker: &crate::app::CustomRangePicker, screen: Rect) {
    // Two Monthly widgets side-by-side, each ~24 cols wide; plus a
    // header row showing the two selected dates and a footer hint.
    let width = 60u16.min(screen.width.saturating_sub(4));
    let height = 14u16.min(screen.height.saturating_sub(2));
    let inner = modal_frame(
        f,
        centered_area(screen, width, height),
        " custom range ",
        Color::Cyan,
    );
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    // Header: "Start: YYYY-MM-DD  →  End: YYYY-MM-DD" with the
    // focused side highlighted.
    let focused_start = picker.focus == crate::app::CustomField::Start;
    let start_style = if focused_start {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let end_style = if !focused_start {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let header = Line::from(vec![
        Span::raw(" Start: "),
        Span::styled(format!(" {} ", picker.start), start_style),
        Span::raw("   End: "),
        Span::styled(format!(" {} ", picker.end), end_style),
    ]);
    let header_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    f.render_widget(Paragraph::new(header), header_area);

    // Hint row at the bottom.
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + inner.height - 1,
        width: inner.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new("Tab focus · h/j/k/l day/week · </> month · Enter apply · Esc back")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(ratatui::layout::Alignment::Center),
        hint_area,
    );

    // Calendar grid area between header and hint.
    let cal_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    let halves = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cal_area);

    draw_month(f, halves[0], picker.start, focused_start);
    draw_month(f, halves[1], picker.end, !focused_start);
}

fn draw_month(f: &mut Frame, area: Rect, selected: time::Date, focused: bool) {
    use ratatui::widgets::calendar::{CalendarEventStore, Monthly};
    let mut store = CalendarEventStore::default();
    let highlight = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    store.add(selected, highlight);
    let widget = Monthly::new(selected, store)
        .show_month_header(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .show_weekdays_header(Style::default().fg(Color::DarkGray))
        .show_surrounding(Style::default().fg(Color::Rgb(60, 60, 60)));
    f.render_widget(widget, area);
}
