//! Heatmap tile: 2D grid of binned tag-value × time cells coloured by
//! aggregate, plus the colour palette + axis helpers it owns.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use super::agg::format_value;
use super::truncate_to_width;
use crate::chart::Series;
use crate::term;

/// Bin a set of series into a 2D matrix indexed by `[y_bin][x_bin]`.
/// `y_keys[y]` is the tag-value label for row `y`; cells contain the
/// average of the points that fell into that bin, or `None` when empty.
///
/// `x_bins` and `y_bins` are pre-clamped by the caller; the function
/// assumes both are > 0.
pub(super) fn heatmap_bin(
    series: &[Series],
    hidden: &[bool],
    by_tag: &str,
    x_bins: usize,
    y_bins: usize,
) -> HeatmapBinned {
    // Bucket series by tag value. `serde_json::Value` doesn't implement
    // `Ord` (because `f64` lacks total ordering), so we wrap it in
    // `TagKey` whose `Ord` impl uses `f64::total_cmp` for the numeric
    // case — same technique as `ordered_float::OrderedFloat`, without
    // pulling in the dep just for one map key.
    let mut by_value: BTreeMap<TagKey, Vec<usize>> = BTreeMap::new();
    for (i, s) in series.iter().enumerate() {
        if hidden.get(i).copied().unwrap_or(false) {
            continue;
        }
        let Some(v) = s.tags.iter().find(|(k, _)| k == by_tag).map(|(_, v)| v) else {
            continue;
        };
        by_value.entry(TagKey(v.clone())).or_default().push(i);
    }
    // Take rows in tag-value order, capped at `y_bins`. Keep the raw
    // key + index list paired so the binning loop below can iterate
    // both in lock-step without re-looking-up.
    let rows: Vec<(TagKey, Vec<usize>)> = by_value.into_iter().take(y_bins).collect();
    let y_keys: Vec<String> = rows
        .iter()
        .map(|(k, _)| crate::chart::tag_text(&k.0))
        .collect();

    // Global x range across the included series.
    let mut x_lo = f64::INFINITY;
    let mut x_hi = f64::NEG_INFINITY;
    for (_, idxs) in &rows {
        for i in idxs {
            for &(x, y) in &series[*i].points {
                if x.is_finite() && y.is_finite() {
                    x_lo = x_lo.min(x);
                    x_hi = x_hi.max(x);
                }
            }
        }
    }
    if !x_lo.is_finite() || !x_hi.is_finite() {
        return HeatmapBinned::empty();
    }
    if (x_hi - x_lo).abs() < f64::EPSILON {
        x_hi += 1.0;
    }

    // Sum + count per cell, then average at the end.
    let mut sum = vec![vec![0.0_f64; x_bins]; rows.len()];
    let mut cnt = vec![vec![0_u32; x_bins]; rows.len()];
    for (yi, (_, idxs)) in rows.iter().enumerate() {
        for i in idxs {
            for &(x, y) in &series[*i].points {
                if !(x.is_finite() && y.is_finite()) {
                    continue;
                }
                let frac = (x - x_lo) / (x_hi - x_lo);
                let mut xi = (frac * x_bins as f64).floor() as isize;
                if xi >= x_bins as isize {
                    xi = x_bins as isize - 1;
                }
                if xi < 0 {
                    xi = 0;
                }
                let xi = xi as usize;
                sum[yi][xi] += y;
                cnt[yi][xi] += 1;
            }
        }
    }

    let mut cells: Vec<Vec<Option<f64>>> = vec![vec![None; x_bins]; rows.len()];
    let (mut v_lo, mut v_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for yi in 0..rows.len() {
        for xi in 0..x_bins {
            if cnt[yi][xi] > 0 {
                let avg = sum[yi][xi] / cnt[yi][xi] as f64;
                cells[yi][xi] = Some(avg);
                v_lo = v_lo.min(avg);
                v_hi = v_hi.max(avg);
            }
        }
    }

    HeatmapBinned {
        cells,
        y_keys,
        x_range: (x_lo, x_hi),
        v_range: if v_lo.is_finite() && v_hi.is_finite() {
            Some((v_lo, v_hi))
        } else {
            None
        },
    }
}

/// Newtype around `serde_json::Value` that implements `Ord` so we can
/// use tag values as `BTreeMap` keys. Variant ordering matches the
/// natural "Null < Bool < Number < String < Array < Object" sequence;
/// numeric comparison uses `f64::total_cmp` (i.e. the same total
/// ordering `ordered_float::OrderedFloat` provides).
#[derive(Clone, Debug, PartialEq, Eq)]
struct TagKey(serde_json::Value);

impl TagKey {
    fn rank(v: &serde_json::Value) -> u8 {
        match v {
            serde_json::Value::Null => 0,
            serde_json::Value::Bool(_) => 1,
            serde_json::Value::Number(_) => 2,
            serde_json::Value::String(_) => 3,
            serde_json::Value::Array(_) => 4,
            serde_json::Value::Object(_) => 5,
        }
    }
}

impl PartialOrd for TagKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TagKey {
    fn cmp(&self, other: &Self) -> Ordering {
        use serde_json::Value::*;
        let (a, b) = (&self.0, &other.0);
        match Self::rank(a).cmp(&Self::rank(b)) {
            Ordering::Equal => match (a, b) {
                (Null, Null) => Ordering::Equal,
                (Bool(x), Bool(y)) => x.cmp(y),
                (Number(x), Number(y)) => x
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&y.as_f64().unwrap_or(0.0)),
                (String(x), String(y)) => x.cmp(y),
                // Arrays / objects fall back to their JSON encoding so
                // they at least sort deterministically; in practice
                // tag values are scalars.
                (Array(_), Array(_)) | (Object(_), Object(_)) => a.to_string().cmp(&b.to_string()),
                _ => Ordering::Equal,
            },
            other => other,
        }
    }
}

pub(super) struct HeatmapBinned {
    pub(super) cells: Vec<Vec<Option<f64>>>,
    pub(super) y_keys: Vec<String>,
    pub(super) x_range: (f64, f64),
    pub(super) v_range: Option<(f64, f64)>,
}

impl HeatmapBinned {
    fn empty() -> Self {
        Self {
            cells: Vec::new(),
            y_keys: Vec::new(),
            x_range: (0.0, 1.0),
            v_range: None,
        }
    }
}

/// 2D grid coloured by value. Requires every contributing series to have
/// a tag whose key matches the `by_tag=` option; otherwise the renderer
/// shows a placeholder.
///
/// Options:
///   * `by_tag`     — required; tag key to spread on the y axis.
///   * `x_bins`     — default min(60, inner.width - 12).
///   * `y_bins`     — default inner.height - 2 (one bin per row).
///   * `palette`    — `viridis` (default) or `mono`.
pub(super) fn draw_heatmap(
    f: &mut Frame,
    series: &[Series],
    hidden: &[bool],
    opts: &BTreeMap<String, String>,
    block: Block<'_>,
    area: Rect,
) {
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(by_tag) = opts.get("by_tag") else {
        let p = Paragraph::new(
            "(heatmap requires `by_tag=<tag>` in the pragma; e.g. `// @viz heatmap by_tag=room`)",
        )
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    };

    // Reserve the rightmost 6 columns for the colour-bar legend.
    let legend_w: u16 = 6;
    let label_w: u16 = inner.width.saturating_sub(legend_w + 4).clamp(6, 20);
    let grid_w = inner.width.saturating_sub(label_w + legend_w + 2);
    let grid_h = inner.height.saturating_sub(1).max(1); // 1 row reserved for the x axis label.

    let x_bins = opts
        .get("x_bins")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(grid_w as usize)
        .min(grid_w as usize)
        .max(1);
    let y_bins = opts
        .get("y_bins")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(grid_h as usize)
        .min(grid_h as usize)
        .max(1);
    let palette = opts.get("palette").map(String::as_str).unwrap_or("viridis");

    let binned = heatmap_bin(series, hidden, by_tag, x_bins, y_bins);
    if binned.y_keys.is_empty() {
        let p = Paragraph::new(format!(
            "(no series tagged with `{by_tag}` — try `:viz heatmap by_tag=<other>`)"
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    }
    let Some((v_lo, v_hi)) = binned.v_range else {
        let p = Paragraph::new("(all bins empty)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    };

    // Layout: [labels label_w] [grid grid_w] [legend legend_w]
    let buf = f.buffer_mut();
    let grid_x0 = inner.x + label_w + 1;
    let grid_y0 = inner.y;
    for (yi, _key) in binned.y_keys.iter().enumerate().take(grid_h as usize) {
        let row_y = grid_y0 + yi as u16;
        // Label, right-aligned in the label column.
        let label = truncate_to_width(&binned.y_keys[yi], label_w as usize);
        let label = format!("{label:>width$}", width = label_w as usize);
        for (ci, ch) in label.chars().enumerate() {
            let cx = inner.x + ci as u16;
            if cx >= inner.x + label_w {
                break;
            }
            buf[(cx, row_y)]
                .set_char(ch)
                .set_style(Style::default().fg(Color::DarkGray));
        }
        // Grid cells.
        for xi in 0..x_bins {
            let cx = grid_x0 + xi as u16;
            if cx >= grid_x0 + grid_w {
                break;
            }
            let cell = binned
                .cells
                .get(yi)
                .and_then(|r| r.get(xi))
                .copied()
                .flatten();
            let bg = match cell {
                Some(v) => palette_color(palette, normalize(v, v_lo, v_hi)),
                None => Color::Reset,
            };
            buf[(cx, row_y)]
                .set_char(' ')
                .set_style(Style::default().bg(bg));
        }
    }

    // Colour-bar legend on the right edge: 3 labels (min/mid/max).
    let legend_x0 = inner.x + label_w + 1 + grid_w + 1;
    for yi in 0..grid_h {
        let t = if grid_h <= 1 {
            1.0
        } else {
            1.0 - (yi as f64) / ((grid_h - 1) as f64)
        };
        let bg = palette_color(palette, t);
        for xi in 0..(legend_w.saturating_sub(4)) {
            let cx = legend_x0 + xi;
            if cx >= inner.x + inner.width {
                break;
            }
            buf[(cx, grid_y0 + yi)]
                .set_char(' ')
                .set_style(Style::default().bg(bg));
        }
    }
    // Numeric labels on the legend column.
    let label_x = legend_x0 + legend_w.saturating_sub(3);
    let labels = [
        (0u16, format_value(v_hi, 1, None)),
        (
            (grid_h / 2).min(grid_h.saturating_sub(1)),
            format_value((v_lo + v_hi) / 2.0, 1, None),
        ),
        (grid_h.saturating_sub(1), format_value(v_lo, 1, None)),
    ];
    for (yi, lbl) in labels {
        let y = grid_y0 + yi;
        for (ci, ch) in lbl.chars().enumerate() {
            let cx = label_x + ci as u16;
            if cx >= inner.x + inner.width {
                break;
            }
            buf[(cx, y)]
                .set_char(ch)
                .set_style(Style::default().fg(Color::Gray));
        }
    }

    // X axis: range label centred under the grid.
    let axis_y = grid_y0 + grid_h;
    if axis_y < inner.y + inner.height {
        let axis_text = format!(
            "{}  ———  {}",
            format_x_label(binned.x_range.0),
            format_x_label(binned.x_range.1)
        );
        let span = Span::styled(axis_text, Style::default().fg(Color::DarkGray));
        let p = Paragraph::new(Line::from(span)).alignment(Alignment::Center);
        let axis_rect = Rect {
            x: grid_x0,
            y: axis_y,
            width: grid_w,
            height: 1,
        };
        f.render_widget(p, axis_rect);
    }
}

pub(super) fn normalize(v: f64, lo: f64, hi: f64) -> f64 {
    if (hi - lo).abs() < f64::EPSILON {
        return 0.5;
    }
    ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
}

fn format_x_label(v: f64) -> String {
    // Mirror chart.rs's `format_time_label` heuristic for unix-seconds /
    // unix-millis ranges; otherwise fall back to numeric.
    let secs = if v > 9.0e11 && v < 9.0e12 {
        v / 1000.0
    } else if v > 9.0e8 && v < 9.0e9 {
        v
    } else {
        return format_value(v, 1, None);
    };
    let secs_i = secs as i64;
    match time::OffsetDateTime::from_unix_timestamp(secs_i) {
        Ok(dt) => format!("{:02}:{:02}", dt.hour(), dt.minute()),
        Err(_) => format_value(secs, 1, None),
    }
}

/// Map a normalised `t ∈ [0,1]` to a colour using the named palette.
/// `viridis` uses 5 truecolor stops; `mono` uses greyscale. Both fall
/// back to a 5-step indexed approximation when truecolor isn't available.
fn palette_color(palette: &str, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    if term::supports_truecolor() {
        match palette {
            "mono" => {
                let g = (t * 255.0).round() as u8;
                Color::Rgb(g, g, g)
            }
            _ => viridis_rgb(t),
        }
    } else {
        // 5-bucket indexed fallback. Same ordering for both palettes so
        // the colour-bar reads as a gradient regardless of palette.
        let idx = (t * 4.999) as usize;
        match palette {
            "mono" => [
                Color::Black,
                Color::DarkGray,
                Color::Gray,
                Color::White,
                Color::White,
            ][idx],
            _ => [
                Color::Indexed(54),  // dark purple
                Color::Indexed(61),  // blue
                Color::Indexed(36),  // teal
                Color::Indexed(148), // yellow-green
                Color::Indexed(226), // yellow
            ][idx],
        }
    }
}

/// 5-stop linear viridis approximation. Numbers picked from the
/// canonical viridis colourmap; close enough for a TUI heatmap.
pub(super) fn viridis_rgb(t: f64) -> Color {
    const STOPS: &[(f64, (u8, u8, u8))] = &[
        (0.00, (68, 1, 84)),
        (0.25, (59, 82, 139)),
        (0.50, (33, 145, 140)),
        (0.75, (94, 201, 98)),
        (1.00, (253, 231, 37)),
    ];
    for w in STOPS.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if t <= t1 {
            let span = t1 - t0;
            let k = if span > 0.0 { (t - t0) / span } else { 0.0 };
            let lerp =
                |a: u8, b: u8| -> u8 { (a as f64 + (b as f64 - a as f64) * k).round() as u8 };
            return Color::Rgb(lerp(c0.0, c1.0), lerp(c0.1, c1.1), lerp(c0.2, c1.2));
        }
    }
    let last = STOPS.last().unwrap().1;
    Color::Rgb(last.0, last.1, last.2)
}
