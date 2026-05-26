use super::*;

fn s(points: Vec<(f64, f64)>) -> Series {
    Series {
        name: "test".to_string(),
        tags: vec![],
        points,
        color: Color::Cyan,
    }
}

#[test]
fn bounds_empty_input_is_safe_default() {
    assert_eq!(Bounds::from_series(&[]), Bounds::empty());
    assert_eq!(Bounds::from_series(&[s(vec![])]), Bounds::empty());
}

#[test]
fn bounds_single_point_has_visible_span() {
    let b = Bounds::from_series(&[s(vec![(5.0, 10.0)])]);
    assert!(b.x[0] < b.x[1]);
    assert!(b.y[0] < b.y[1]);
}

#[test]
fn bounds_constant_y_gets_padded() {
    let b = Bounds::from_series(&[s(vec![(0.0, 7.0), (1.0, 7.0), (2.0, 7.0)])]);
    assert!(b.y[0] < 7.0);
    assert!(b.y[1] > 7.0);
}

#[test]
fn bounds_multi_series_union() {
    let a = s(vec![(0.0, -1.0), (10.0, 1.0)]);
    let b = s(vec![(5.0, 2.0), (20.0, -2.0)]);
    let bounds = Bounds::from_series(&[a, b]);
    assert!(bounds.x[0] <= 0.0 && bounds.x[1] >= 20.0);
    assert!(bounds.y[0] <= -2.0 && bounds.y[1] >= 2.0);
}

#[test]
fn bounds_ignores_non_finite_values() {
    let b = Bounds::from_series(&[s(vec![
        (0.0, 1.0),
        (1.0, f64::NAN),
        (2.0, f64::INFINITY),
        (3.0, 2.0),
    ])]);
    assert!(b.y[0] <= 1.0 && b.y[1] >= 2.0);
    assert!(b.y[0].is_finite() && b.y[1].is_finite());
}

#[test]
fn color_for_cycles_palette() {
    assert_eq!(color_for(0), PALETTE[0]);
    assert_eq!(color_for(PALETTE.len()), PALETTE[0]);
    assert_eq!(color_for(PALETTE.len() + 3), PALETTE[3]);
}

#[test]
fn x_labels_use_hh_mm_for_short_unix_seconds_range() {
    // 2025-01-01T00:00:00Z .. 2025-01-01T01:00:00Z
    let labels = x_axis_labels(1_735_689_600.0, 1_735_693_200.0);
    for l in &labels {
        // Format `HH:MM` is exactly 5 chars and contains a colon.
        assert_eq!(l.len(), 5, "got {l}");
        assert!(l.contains(':'), "got {l}");
    }
}

#[test]
fn x_labels_use_date_for_multi_day_range() {
    // 7-day window — should switch to `MM-DD HH:MM`.
    let labels = x_axis_labels(1_735_689_600.0, 1_735_689_600.0 + 7.0 * 86_400.0);
    for l in &labels {
        assert!(l.contains('-'), "got {l}");
        assert!(l.contains(':'), "got {l}");
    }
}

#[test]
fn x_labels_handle_unix_millis() {
    let labels = x_axis_labels(1_735_689_600_000.0, 1_735_693_200_000.0);
    for l in &labels {
        assert_eq!(l.len(), 5);
    }
}

#[test]
fn x_labels_fall_back_to_numeric_for_non_time_data() {
    let labels = x_axis_labels(0.0, 100.0);
    // Numeric `format_label` outputs decimal-point strings; never colon-only.
    assert!(labels.iter().all(|l| !l.contains(':')));
}

// ----- summarize_legend -----

fn ts(name: &str, tags: &[(&str, &str)]) -> Series {
    Series {
        name: name.to_string(),
        tags: tags
            .iter()
            .map(|(k, v)| (k.to_string(), (*v).into()))
            .collect(),
        points: vec![],
        color: Color::Cyan,
    }
}

#[test]
fn summarize_empty_yields_empty() {
    let got = summarize_legend(&[], &[]);
    assert_eq!(got.header, "");
    assert!(got.rows.is_empty());
}

#[test]
fn summarize_lifts_shared_metric_and_shared_tags() {
    let series = vec![
        ts("cpu {h1,us}", &[("host", "h1"), ("region", "us")]),
        ts("cpu {h2,us}", &[("host", "h2"), ("region", "us")]),
        ts("cpu {h3,us}", &[("host", "h3"), ("region", "us")]),
    ];
    let got = summarize_legend(&series, &[]);
    assert_eq!(got.header, "cpu {region=us}");
    assert_eq!(
        got.rows,
        vec![
            "host=h1".to_string(),
            "host=h2".to_string(),
            "host=h3".to_string(),
        ]
    );
}

#[test]
fn summarize_skips_shared_tags_when_user_picked() {
    // When the user has explicitly picked label-tags, the row
    // is just the joined values of those tags, regardless of
    // what else is shared.
    let series = vec![
        ts("cpu {h1}", &[("host", "h1"), ("region", "us")]),
        ts("cpu {h2}", &[("host", "h2"), ("region", "us")]),
    ];
    let got = summarize_legend(&series, &["host".to_string()]);
    assert_eq!(got.header, "cpu");
    assert_eq!(got.rows, vec!["h1".to_string(), "h2".to_string()]);
}

#[test]
fn summarize_falls_back_when_picked_tag_missing() {
    // Picked key not on the series — row falls back to s.name
    // so it's never blank.
    let series = vec![ts("cpu {us}", &[("region", "us")])];
    let got = summarize_legend(&series, &["host".to_string()]);
    assert_eq!(got.rows, vec!["cpu {us}".to_string()]);
}

#[test]
fn summarize_mixed_metric_drops_header_metric() {
    let series = vec![
        ts("cpu {h1}", &[("host", "h1")]),
        ts("mem {h1}", &[("host", "h1")]),
    ];
    let got = summarize_legend(&series, &[]);
    // Metric differs — not lifted. host=h1 is shared — lifted.
    assert_eq!(got.header, "{host=h1}");
}

#[test]
fn summarize_single_series_lifts_everything_leaves_empty_rows() {
    let series = vec![ts("cpu {h1,us}", &[("host", "h1"), ("region", "us")])];
    let got = summarize_legend(&series, &[]);
    assert_eq!(got.header, "cpu {host=h1, region=us}");
    // Single series: all tags shared, so the row text is empty
    // (the bullet colour alone identifies it).
    assert_eq!(got.rows, vec!["".to_string()]);
}

#[test]
fn summarize_no_shared_tags_keeps_full_per_row() {
    let series = vec![
        ts("cpu {h1}", &[("host", "h1")]),
        ts("cpu {h2}", &[("host", "h2")]),
    ];
    let got = summarize_legend(&series, &[]);
    assert_eq!(got.header, "cpu");
    assert_eq!(got.rows, vec!["host=h1".to_string(), "host=h2".to_string()]);
}
