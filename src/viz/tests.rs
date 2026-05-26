use super::*;

use super::agg::{Agg, format_value};
use super::heatmap::{heatmap_bin, normalize, viridis_rgb};
use super::note::{render_markdown, strip_leading_pragma};
use super::pie::pie_rows;
use super::pragma::format_pragma;
use super::table::{TableCell, series_to_table};
use super::top_list::top_list_rows;
use ratatui::style::{Color, Style};

fn spec(kind: VizKind) -> VizSpec {
    VizSpec {
        kind,
        opts: BTreeMap::new(),
    }
}

#[test]
fn parse_returns_none_when_no_pragma() {
    assert_eq!(parse_pragma("home:temp | align to 1m"), Ok(None));
    assert_eq!(parse_pragma(""), Ok(None));
    assert_eq!(parse_pragma("// just a normal comment\nfoo"), Ok(None));
}

#[test]
fn parse_finds_pragma_at_top() {
    let got = parse_pragma("// @viz bar\nhome:temp").unwrap().unwrap();
    assert_eq!(got, spec(VizKind::Bar));
}

#[test]
fn parse_allows_leading_whitespace_and_blank_lines() {
    let got = parse_pragma("\n  // @viz scatter\nhome:temp")
        .unwrap()
        .unwrap();
    assert_eq!(got, spec(VizKind::Scatter));
}

#[test]
fn parse_collects_options() {
    let got = parse_pragma("// @viz top_list n=10 by=host\nfoo")
        .unwrap()
        .unwrap();
    assert_eq!(got.kind, VizKind::TopList);
    assert_eq!(got.opts.get("n").map(String::as_str), Some("10"));
    assert_eq!(got.opts.get("by").map(String::as_str), Some("host"));
}

#[test]
fn parse_stops_at_first_non_comment() {
    // The pragma is below a real code line — must not be parsed.
    let got = parse_pragma("home:temp\n// @viz bar").unwrap();
    assert_eq!(got, None);
}

#[test]
fn parse_reports_unknown_kind_with_line_index() {
    let err = parse_pragma("// @viz nope\nfoo").unwrap_err();
    assert_eq!(err.0, 0);
    assert!(matches!(err.1, PragmaError::UnknownKind { .. }));
}

#[test]
fn parse_reports_missing_kind() {
    let err = parse_pragma("// @viz\nfoo").unwrap_err();
    assert_eq!(err.0, 0);
    assert_eq!(err.1, PragmaError::MissingKind);
}

#[test]
fn parse_reports_malformed_option() {
    let err = parse_pragma("// @viz line broken-token\nfoo").unwrap_err();
    assert!(matches!(err.1, PragmaError::MalformedOption { .. }));
}

#[test]
fn parse_ignores_at_vizfoo_lookalike() {
    // `@vizfoo` is not `@viz`. Must be treated as a plain comment.
    assert_eq!(parse_pragma("// @vizfoo bar\nx"), Ok(None));
}

#[test]
fn format_round_trips_with_parse() {
    let mut opts = BTreeMap::new();
    opts.insert("n".to_string(), "5".to_string());
    opts.insert("agg".to_string(), "avg".to_string());
    let s = VizSpec {
        kind: VizKind::TopList,
        opts,
    };
    let line = format_pragma(&s);
    let buf = format!("{line}\nfoo");
    assert_eq!(parse_pragma(&buf).unwrap(), Some(s));
}

#[test]
fn upsert_inserts_when_missing() {
    let out = upsert_pragma("home:temp\n", &spec(VizKind::Bar));
    assert_eq!(out, "// @viz bar\nhome:temp\n");
}

#[test]
fn upsert_rewrites_existing_in_place() {
    let out = upsert_pragma("// @viz line\nhome:temp\n", &spec(VizKind::Scatter));
    assert_eq!(out, "// @viz scatter\nhome:temp\n");
}

#[test]
fn upsert_is_idempotent() {
    let once = upsert_pragma("home:temp\n", &spec(VizKind::Area));
    let twice = upsert_pragma(&once, &spec(VizKind::Area));
    assert_eq!(once, twice);
}

#[test]
fn upsert_preserves_absence_of_trailing_newline() {
    let out = upsert_pragma("home:temp", &spec(VizKind::Bar));
    assert!(!out.ends_with('\n'));
}

// ── Agg ────────────────────────────────────────────────────────────

fn pts(ys: &[f64]) -> Vec<(f64, f64)> {
    ys.iter().enumerate().map(|(i, y)| (i as f64, *y)).collect()
}

#[test]
fn agg_empty_input_is_none_except_count() {
    assert_eq!(Agg::Last.apply(&[]), None);
    assert_eq!(Agg::Avg.apply(&[]), None);
    assert_eq!(Agg::Sum.apply(&[]), None);
    assert_eq!(Agg::Count.apply(&[]), Some(0.0));
}

#[test]
fn agg_skips_non_finite() {
    let p = pts(&[1.0, f64::NAN, 3.0, f64::INFINITY, 5.0]);
    assert_eq!(Agg::Sum.apply(&p), Some(9.0));
    assert_eq!(Agg::Avg.apply(&p), Some(3.0));
    assert_eq!(Agg::Min.apply(&p), Some(1.0));
    assert_eq!(Agg::Max.apply(&p), Some(5.0));
    assert_eq!(Agg::Count.apply(&p), Some(3.0));
}

#[test]
fn agg_first_last_preserve_order() {
    let p = pts(&[7.0, 3.0, 9.0, 1.0]);
    assert_eq!(Agg::First.apply(&p), Some(7.0));
    assert_eq!(Agg::Last.apply(&p), Some(1.0));
}

#[test]
fn agg_parses_canonical_and_aliases() {
    assert_eq!(Agg::parse("avg"), Some(Agg::Avg));
    assert_eq!(Agg::parse("mean"), Some(Agg::Avg));
    assert_eq!(Agg::parse("count"), Some(Agg::Count));
    assert_eq!(Agg::parse("nope"), None);
}

// ── top_list_rows ────────────────────────────────────────────────────

fn mkseries(name: &str, ys: &[f64]) -> Series {
    Series {
        name: name.to_string(),
        tags: vec![],
        points: pts(ys),
        color: Color::Cyan,
    }
}

#[test]
fn top_list_sorts_desc_by_default_and_caps_at_n() {
    let s = vec![
        mkseries("a", &[1.0, 1.0, 1.0]), // avg 1
        mkseries("b", &[3.0, 3.0, 3.0]), // avg 3
        mkseries("c", &[2.0, 2.0, 2.0]), // avg 2
    ];
    let rows = top_list_rows(&s, &[false; 3], Agg::Avg, 2, false);
    assert_eq!(rows.len(), 2);
    // Largest first.
    assert_eq!(s[rows[0].0].name, "b");
    assert_eq!(s[rows[1].0].name, "c");
}

#[test]
fn top_list_ascending_reverses_order() {
    let s = vec![
        mkseries("a", &[1.0]),
        mkseries("b", &[3.0]),
        mkseries("c", &[2.0]),
    ];
    let rows = top_list_rows(&s, &[false; 3], Agg::Last, 10, true);
    assert_eq!(s[rows[0].0].name, "a");
    assert_eq!(s[rows[2].0].name, "b");
}

#[test]
fn top_list_skips_hidden_series() {
    let s = vec![
        mkseries("a", &[1.0]),
        mkseries("b", &[3.0]),
        mkseries("c", &[2.0]),
    ];
    let hidden = vec![false, true, false];
    let rows = top_list_rows(&s, &hidden, Agg::Last, 10, false);
    assert_eq!(rows.len(), 2);
    for (i, _) in &rows {
        assert_ne!(s[*i].name, "b");
    }
}

#[test]
fn top_list_drops_all_nan_series() {
    let s = vec![
        mkseries("good", &[1.0, 2.0]),
        mkseries("bad", &[f64::NAN, f64::NAN]),
    ];
    // `Avg` on all-NaN returns None → series dropped.
    let rows = top_list_rows(&s, &[false; 2], Agg::Avg, 10, false);
    assert_eq!(rows.len(), 1);
    assert_eq!(s[rows[0].0].name, "good");
}

// ── format_value ─────────────────────────────────────────────────────

#[test]
fn format_value_appends_unit_when_present() {
    assert_eq!(format_value(2.50, 2, Some("ms")), "2.50 ms");
    assert_eq!(format_value(2.50, 2, None), "2.50");
}

#[test]
fn format_value_uses_scientific_for_extreme_magnitudes() {
    let big = format_value(1.2e9, 2, None);
    assert!(big.contains('e'), "expected scientific notation, got {big}");
    let tiny = format_value(1.2e-4, 2, None);
    assert!(
        tiny.contains('e'),
        "expected scientific notation, got {tiny}"
    );
}

// ── pie ────────────────────────────────────────────────────────────

#[test]
fn pie_rows_normalises_shares_to_one() {
    let s = vec![mkseries("a", &[10.0]), mkseries("b", &[30.0])];
    let rows = pie_rows(&s, &[false; 2], Agg::Sum);
    let total_share: f64 = rows.iter().map(|(_, _, share)| share).sum();
    assert!((total_share - 1.0).abs() < 1e-9);
    assert_eq!(s[rows[0].0].name, "b");
}

#[test]
fn pie_rows_empty_when_total_nonpositive() {
    let s = vec![mkseries("a", &[-1.0])];
    assert!(pie_rows(&s, &[false; 1], Agg::Sum).is_empty());
    assert!(pie_rows(&[], &[], Agg::Sum).is_empty());
}

#[test]
fn pie_rows_drops_negative_aggregates() {
    let s = vec![mkseries("good", &[5.0]), mkseries("bad", &[-2.0])];
    let rows = pie_rows(&s, &[false; 2], Agg::Sum);
    assert_eq!(rows.len(), 1);
    assert_eq!(s[rows[0].0].name, "good");
}

// ── heatmap ─────────────────────────────────────────────────────────

fn tagged(name: &str, tag: &str, ys: &[f64]) -> Series {
    Series {
        name: name.to_string(),
        tags: vec![("room".to_string(), tag.into())],
        points: pts(ys),
        color: Color::Cyan,
    }
}

#[test]
fn heatmap_bin_groups_by_tag_value_and_averages_per_cell() {
    let s = vec![
        tagged("a", "kitchen", &[1.0, 2.0]),
        tagged("b", "kitchen", &[3.0, 4.0]),
        tagged("c", "hall", &[10.0, 20.0]),
    ];
    let b = heatmap_bin(&s, &[false; 3], "room", 2, 4);
    assert_eq!(b.y_keys.len(), 2);
    assert!(b.y_keys.contains(&"kitchen".to_string()));
    assert!(b.y_keys.contains(&"hall".to_string()));
    let ki = b.y_keys.iter().position(|k| k == "kitchen").unwrap();
    assert_eq!(b.cells[ki][0], Some(2.0));
    assert_eq!(b.cells[ki][1], Some(3.0));
    let (lo, hi) = b.v_range.unwrap();
    assert_eq!(lo, 2.0);
    assert_eq!(hi, 20.0);
}

#[test]
fn heatmap_bin_returns_empty_when_no_series_have_the_tag() {
    let s = vec![mkseries("a", &[1.0])];
    let b = heatmap_bin(&s, &[false; 1], "room", 2, 2);
    assert!(b.y_keys.is_empty());
    assert!(b.v_range.is_none());
}

// ── palette ────────────────────────────────────────────────────────

#[test]
fn viridis_green_channel_is_largely_monotonic() {
    // Green rises 1 → 231 across viridis; we allow tiny non-monotonic
    // dips from the linear interpolation between hand-picked stops.
    let mut prev = 0u8;
    for i in 0..=10 {
        let t = i as f64 / 10.0;
        if let Color::Rgb(_, g, _) = viridis_rgb(t) {
            assert!(
                g >= prev || (prev as i32 - g as i32).abs() < 10,
                "green channel regressed at t={t}: prev={prev} new={g}"
            );
            prev = g;
        }
    }
}

// ── table ────────────────────────────────────────────────────────────

#[test]
fn series_to_table_collects_tag_columns_alphabetically_then_value() {
    let s = vec![
        Series {
            name: "a".into(),
            tags: vec![
                ("zone".into(), "east".into()),
                ("host".into(), "db-1".into()),
            ],
            points: pts(&[1.0, 2.0, 3.0]),
            color: Color::Cyan,
        },
        Series {
            name: "b".into(),
            tags: vec![("host".into(), "db-2".into())],
            points: pts(&[10.0, 20.0]),
            color: Color::Yellow,
        },
    ];
    let t = series_to_table(&s, &[false; 2], Agg::Last);
    assert_eq!(t.columns, vec!["host", "zone", "value"]);
    assert_eq!(t.rows.len(), 2);
    // Row 0: host=db-1, zone=east, value=3.0 (Agg::Last)
    assert_eq!(t.rows[0][0], TableCell::Str("db-1".into()));
    assert_eq!(t.rows[0][1], TableCell::Str("east".into()));
    assert_eq!(t.rows[0][2], TableCell::Float(3.0));
    // Row 1: host=db-2, zone=NULL (missing), value=20.0
    assert_eq!(t.rows[1][0], TableCell::Str("db-2".into()));
    assert_eq!(t.rows[1][1], TableCell::Null);
    assert_eq!(t.rows[1][2], TableCell::Float(20.0));
}

#[test]
fn series_to_table_skips_hidden_series() {
    let s = vec![
        Series {
            name: "a".into(),
            tags: vec![("h".into(), "x".into())],
            points: pts(&[1.0]),
            color: Color::Cyan,
        },
        Series {
            name: "b".into(),
            tags: vec![("h".into(), "y".into())],
            points: pts(&[2.0]),
            color: Color::Yellow,
        },
    ];
    let t = series_to_table(&s, &[false, true], Agg::Last);
    assert_eq!(t.rows.len(), 1);
    assert_eq!(t.rows[0][0], TableCell::Str("x".into()));
}

#[test]
fn table_cell_render_handles_each_variant() {
    assert_eq!(TableCell::Null.render(), "—");
    assert_eq!(TableCell::Int(42).render(), "42");
    assert_eq!(TableCell::Float(2.5).render(), "2.50");
    assert_eq!(TableCell::Str("hi".into()).render(), "hi");
    assert_eq!(TableCell::Bool(true).render(), "true");
}

#[test]
fn normalize_handles_constant_range() {
    assert_eq!(normalize(5.0, 5.0, 5.0), 0.5);
    assert_eq!(normalize(0.0, 0.0, 10.0), 0.0);
    assert_eq!(normalize(10.0, 0.0, 10.0), 1.0);
    assert_eq!(normalize(-1.0, 0.0, 10.0), 0.0);
    assert_eq!(normalize(100.0, 0.0, 10.0), 1.0);
}

// ── note (mini-markdown) ─────────────────────────────────────────────────

fn render_to_text(body: &str) -> Vec<String> {
    render_markdown(body)
        .into_iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn markdown_renders_headings_with_indents() {
    let txt = render_to_text("# H1\n## H2\n### H3");
    assert!(txt[0].ends_with("H1"));
    assert!(txt[1].ends_with("H2"));
    assert!(txt[2].ends_with("H3"));
}

#[test]
fn markdown_renders_list_bullets() {
    let txt = render_to_text("- alpha\n- beta");
    assert!(txt[0].contains('•'));
    assert!(txt[0].contains("alpha"));
    assert!(txt[1].contains("beta"));
}

#[test]
fn markdown_inline_bold_italic_code() {
    let lines = render_markdown("hello **bold** *it* `c` end");
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    let joined: String = line.spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(joined, "hello bold it c end");
    let styled: Vec<_> = line
        .spans
        .iter()
        .filter(|s| s.style != Style::default())
        .map(|s| s.content.to_string())
        .collect();
    assert!(styled.contains(&"bold".to_string()));
    assert!(styled.contains(&"it".to_string()));
    assert!(styled.contains(&"c".to_string()));
}

#[test]
fn markdown_code_fence_blocks_swallow_inline_formatting() {
    let lines = render_markdown("```\nlet x = **not bold**;\n```");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].spans.len(), 1);
    assert!(lines[0].spans[0].content.contains("**not bold**"));
}

#[test]
fn strip_leading_pragma_removes_just_the_pragma_line() {
    let body = "// @viz note\n# Title\n\nbody text\n";
    let stripped = strip_leading_pragma(body);
    assert!(stripped.starts_with("# Title"));
}

#[test]
fn strip_leading_pragma_is_noop_without_pragma() {
    let body = "# Title\n";
    assert_eq!(strip_leading_pragma(body), body);
}
