use super::grid::{
    InlineLegendPlan, MIN_GRID_ROW_HEIGHT, NOTE_ROW_HEIGHT, compute_row_heights, fit_inline_legend,
};
use super::help::{KEYS_HELP_SOURCE, render_keys_help};
use crate::axiom::{Chart, ChartBase, KnownChart, LayoutItem};

fn note(id: &str) -> Chart {
    Chart::Known(KnownChart::Note(ChartBase {
        id: id.into(),
        name: None,
        query: None,
        extras: Default::default(),
    }))
}
fn ts(id: &str) -> Chart {
    Chart::Known(KnownChart::TimeSeries(ChartBase {
        id: id.into(),
        name: None,
        query: None,
        extras: Default::default(),
    }))
}
fn slot(i: &str, x: u32, y: u32, w: u32, h: u32) -> LayoutItem {
    LayoutItem {
        i: i.into(),
        x,
        y: Some(y),
        w,
        h,
        extras: Default::default(),
    }
}

#[test]
fn note_only_rows_shrink() {
    // Layout: Note h=2 at y=0, TimeSeries h=3 at y=2. Total
    // virt_rows = 5. Rows 0,1 are note-only; rows 2-4 are
    // non-note.
    let charts = vec![note("n"), ts("t")];
    let layout = vec![slot("n", 0, 0, 12, 2), slot("t", 0, 2, 12, 3)];
    let h = compute_row_heights(&charts, &layout, 5, 0);
    assert_eq!(h[0], NOTE_ROW_HEIGHT);
    assert_eq!(h[1], NOTE_ROW_HEIGHT);
    assert_eq!(h[2], MIN_GRID_ROW_HEIGHT);
    assert_eq!(h[3], MIN_GRID_ROW_HEIGHT);
    assert_eq!(h[4], MIN_GRID_ROW_HEIGHT);
}

#[test]
fn row_with_both_note_and_chart_keeps_chart_min() {
    // Note h=4 and a chart h=2 share rows 0-1.
    let charts = vec![note("n"), ts("t")];
    let layout = vec![slot("n", 0, 0, 6, 4), slot("t", 6, 0, 6, 2)];
    let h = compute_row_heights(&charts, &layout, 4, 0);
    // Rows 0-1: chart present → min. Rows 2-3: note only → shrunk.
    assert_eq!(h[0], MIN_GRID_ROW_HEIGHT);
    assert_eq!(h[1], MIN_GRID_ROW_HEIGHT);
    assert_eq!(h[2], NOTE_ROW_HEIGHT);
    assert_eq!(h[3], NOTE_ROW_HEIGHT);
}

#[test]
fn surplus_grows_only_non_note_rows() {
    // 1 note row + 1 chart row, viewport big enough for surplus.
    let charts = vec![note("n"), ts("t")];
    let layout = vec![slot("n", 0, 0, 12, 1), slot("t", 0, 1, 12, 1)];
    let viewport = NOTE_ROW_HEIGHT + MIN_GRID_ROW_HEIGHT + 10;
    let h = compute_row_heights(&charts, &layout, 2, viewport);
    assert_eq!(h[0], NOTE_ROW_HEIGHT, "note row stays compact");
    assert_eq!(h[1], MIN_GRID_ROW_HEIGHT + 10, "chart row absorbs surplus");
}

#[test]
fn total_overflows_viewport_no_growth() {
    // Content already exceeds viewport → no surplus, no growth.
    let charts = vec![ts("a"), ts("b")];
    let layout = vec![slot("a", 0, 0, 12, 5), slot("b", 0, 5, 12, 5)];
    let h = compute_row_heights(&charts, &layout, 10, 4);
    for v in &h {
        assert_eq!(*v, MIN_GRID_ROW_HEIGHT);
    }
}

fn plan(shown: &[usize], ellipsis: bool) -> InlineLegendPlan {
    InlineLegendPlan {
        shown: shown.to_vec(),
        ellipsis,
    }
}

#[test]
fn empty_labels_yields_empty_plan() {
    assert_eq!(fit_inline_legend(&[], 40), plan(&[], false));
}

#[test]
fn single_entry_fits_when_room_for_bullet_plus_label() {
    // "● foo" = 2 (bullet) + 3 (label) = 5 columns.
    assert_eq!(fit_inline_legend(&["foo"], 5), plan(&[0], false));
    assert_eq!(fit_inline_legend(&["foo"], 4), plan(&[], true));
}

#[test]
fn all_entries_fit_when_width_is_generous() {
    let p = fit_inline_legend(&["a", "b", "c"], 40);
    assert_eq!(p, plan(&[0, 1, 2], false));
}

#[test]
fn truncates_with_ellipsis_when_remainder_would_overflow() {
    let p = fit_inline_legend(&["a", "b", "c"], 10);
    assert_eq!(p, plan(&[0], true));
}

#[test]
fn last_entry_does_not_need_ellipsis_reservation() {
    let p = fit_inline_legend(&["a", "b"], 8);
    assert_eq!(p, plan(&[0, 1], false));
}

#[test]
fn zero_width_shows_nothing_and_signals_truncation() {
    let p = fit_inline_legend(&["a", "b"], 0);
    assert_eq!(p, plan(&[], true));
}

#[test]
fn long_label_alone_gets_dropped_in_favour_of_ellipsis() {
    let p = fit_inline_legend(&["a-very-long-label"], 5);
    assert_eq!(p, plan(&[], true));
}

#[test]
fn embedded_help_file_has_expected_sections() {
    // Sanity-check that the embedded help file isn't empty and
    // covers a few representative bindings the user would expect
    // to find by hitting `?`.
    assert!(KEYS_HELP_SOURCE.contains("## Normal mode: motion"));
    assert!(KEYS_HELP_SOURCE.contains("## Dashboard pane"));
    assert!(KEYS_HELP_SOURCE.contains("## Time picker"));
    assert!(KEYS_HELP_SOURCE.contains(":trace"));
    assert!(KEYS_HELP_SOURCE.contains(":time"));
}

#[test]
fn render_keys_help_skips_preface_and_keeps_sections() {
    let src = "# Title\nIntro line.\n\n## First\nh\tleft\nj\tdown\n\n## Second\nq\tquit\n";
    let lines = render_keys_help(src);
    // The h1 `# Title` and its intro must be stripped; the first
    // emitted line is the `First` heading.
    let first = format!("{:?}", lines[0]);
    assert!(
        first.contains("First"),
        "expected first heading, got {first:?}"
    );
    // Make sure every key column shows up somewhere.
    let rendered = lines
        .iter()
        .map(|l| format!("{l:?}"))
        .collect::<Vec<_>>()
        .join(" ");
    for needle in ["h ", "j ", "q ", "left", "down", "quit", "Second"] {
        assert!(
            rendered.contains(needle),
            "missing {needle:?} in rendered help: {rendered}"
        );
    }
}

#[test]
fn render_keys_help_drops_single_hash_comments() {
    let src = "## S\n# this is a comment\nk\tup\n";
    let lines = render_keys_help(src);
    let rendered = lines
        .iter()
        .map(|l| format!("{l:?}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!rendered.contains("comment"));
    assert!(rendered.contains("up"));
}
