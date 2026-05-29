//! Tile / layout helpers for the dashboard grid: kind picker,
//! spatial navigation, the pure-function `tile_ops` operating on a
//! `(charts, layout)` pair, and the single-buffer → dashboard
//! document builder.
//!
//! Everything here is pure (no `App` borrow) so each piece can be
//! unit-tested in isolation.

use crate::dashboard::VizKind;

/// Backwards-compatible alias for [`VizKind::ALL`]. Kept so add-pick /
/// open-pick call sites don't need a one-line edit each.
pub(crate) fn add_pick_kinds() -> &'static [VizKind] {
    VizKind::ALL
}

/// Cardinal directions for spatial navigation in the dashboard grid.
/// Decoupled from key codes so the navigator can be unit-tested
/// without a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpatialDir {
    Left,
    Right,
    Up,
    Down,
}

/// Pick the chart whose centroid is nearest in `dir` from the
/// currently-selected chart. Returns `Some(idx)` when a candidate
/// exists, `None` when nothing lies in that direction (caller falls
/// back to row-major cycling).
///
/// Layout items live on a 12-column grid (`x ∈ 0..=11`); `y` is
/// nullable so we treat missing values as 0 for distance purposes.
/// Charts without a matching `LayoutItem` get a phantom (0, 0, 1, 1).
/// Ties broken by Manhattan distance from the source centroid.
pub(crate) fn pick_next_chart_in_direction(
    layout: &[crate::axiom::LayoutItem],
    charts: &[crate::axiom::Chart],
    selected: usize,
    dir: SpatialDir,
) -> Option<usize> {
    fn centroid(layout: &[crate::axiom::LayoutItem], chart_id: &str) -> (f32, f32) {
        match layout.iter().find(|l| l.i == chart_id) {
            Some(l) => {
                let x = l.x as f32 + l.w as f32 / 2.0;
                let y = l.y.unwrap_or(0) as f32 + l.h as f32 / 2.0;
                (x, y)
            }
            None => (0.0, 0.0),
        }
    }
    // `Chart::Unknown` has no `ChartBase.id`, so we can't locate it in
    // the layout grid. Treat such a focused tile as "no spatial
    // navigation source" and skip Unknown candidates in the scan
    // — they remain visible in the grid (their raw JSON round-trips)
    // but aren't reachable by hjkl until the SDK knows about them.
    let src_id = charts.get(selected)?.base()?.id.clone();
    let (sx, sy) = centroid(layout, &src_id);
    let mut best: Option<(usize, f32)> = None;
    for (i, c) in charts.iter().enumerate() {
        if i == selected {
            continue;
        }
        let Some(base) = c.base() else { continue };
        let (cx, cy) = centroid(layout, &base.id);
        // Must actually lie in the requested direction.
        let in_dir = match dir {
            SpatialDir::Right => cx > sx,
            SpatialDir::Left => cx < sx,
            SpatialDir::Down => cy > sy,
            SpatialDir::Up => cy < sy,
        };
        if !in_dir {
            continue;
        }
        // Prefer matches on the perpendicular axis (smallest cross-axis
        // distance), then nearest along the chosen axis.
        let (primary, cross) = match dir {
            SpatialDir::Right | SpatialDir::Left => ((cx - sx).abs(), (cy - sy).abs()),
            SpatialDir::Up | SpatialDir::Down => ((cy - sy).abs(), (cx - sx).abs()),
        };
        let score = cross * 2.0 + primary; // weight cross-axis 2×
        if best.is_none() || score < best.unwrap().1 {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
}

/// Maximum column index in the server's virtual grid. The schema
/// constrains `x` to 0..=11 and chart widths to fit — i.e. a 12-col
/// grid. Resize/move clamp against this.
pub(crate) const GRID_COLS: u32 = 12;

/// Layout-mutating helpers operate on a `(charts, layout)` pair drawn
/// from `loaded_dashboard.dashboard`. Pure functions — no `App` borrow
/// — so each can be unit-tested in isolation and reused by the
/// keyboard sub-modes + the `:tile` Ex-commands.
pub(crate) mod tile_ops {
    use super::GRID_COLS;
    use crate::axiom::{Chart, ChartBase, LayoutItem};

    /// `true` if `candidate` overlaps any layout entry whose `i` is
    /// **not** `ignore_id`. Two rectangles overlap when they share at
    /// least one cell in both axes.
    pub fn overlaps_any(candidate: &LayoutItem, layout: &[LayoutItem], ignore_id: &str) -> bool {
        layout
            .iter()
            .any(|l| l.i != ignore_id && crate::app::tile_ops_shove::rects_overlap(candidate, l))
    }

    /// Translate the tile `chart_id` by `(dx, dy)` virtual-grid cells.
    /// Returns `Err(msg)` when the move would push the tile off the
    /// 12-col grid or overlap another tile; the layout is unchanged in
    /// that case.
    pub fn translate(
        layout: &mut [LayoutItem],
        chart_id: &str,
        dx: i32,
        dy: i32,
    ) -> Result<(), &'static str> {
        let li_idx = layout
            .iter()
            .position(|l| l.i == chart_id)
            .ok_or("tile has no layout entry")?;
        let mut new_li = layout[li_idx].clone();
        let cur_x = new_li.x as i32;
        let cur_y = new_li.y.unwrap_or(0) as i32;
        let nx = cur_x + dx;
        let ny = cur_y + dy;
        if nx < 0 || ny < 0 {
            return Err("edge of grid");
        }
        if (nx as u32) + new_li.w > GRID_COLS {
            return Err("edge of grid");
        }
        new_li.x = nx as u32;
        new_li.y = Some(ny as u32);
        if overlaps_any(&new_li, layout, chart_id) {
            return Err("would overlap another tile");
        }
        layout[li_idx] = new_li;
        Ok(())
    }

    /// Grow/shrink the tile's `w`/`h` by `(dw, dh)`. Clamped to a
    /// 1-cell minimum and to `GRID_COLS` total width. Overlap rejected.
    pub fn resize(
        layout: &mut [LayoutItem],
        chart_id: &str,
        dw: i32,
        dh: i32,
    ) -> Result<(), &'static str> {
        let li_idx = layout
            .iter()
            .position(|l| l.i == chart_id)
            .ok_or("tile has no layout entry")?;
        let mut new_li = layout[li_idx].clone();
        let nw = new_li.w as i32 + dw;
        let nh = new_li.h as i32 + dh;
        if nw < 1 || nh < 1 {
            return Err("minimum size 1x1");
        }
        if new_li.x + (nw as u32) > GRID_COLS {
            return Err("exceeds 12-col grid");
        }
        new_li.w = nw as u32;
        new_li.h = nh as u32;
        if overlaps_any(&new_li, layout, chart_id) {
            return Err("would overlap another tile");
        }
        layout[li_idx] = new_li;
        Ok(())
    }

    /// Delete the tile (chart + matching layout entry). Returns `Err`
    /// if no chart with that id exists.
    pub fn delete(
        charts: &mut Vec<Chart>,
        layout: &mut Vec<LayoutItem>,
        chart_id: &str,
    ) -> Result<(), &'static str> {
        let cidx = charts
            .iter()
            .position(|c| c.base().is_some_and(|b| b.id == chart_id))
            .ok_or("unknown chart id")?;
        charts.remove(cidx);
        layout.retain(|l| l.i != chart_id);
        Ok(())
    }

    /// Find the first free slot for a new `w × h` tile, scanning
    /// row-major across the virtual grid. Always returns *some*
    /// position: when the grid is packed full the new tile lands
    /// directly below the lowest existing tile.
    pub fn first_free_slot(layout: &[LayoutItem], w: u32, h: u32) -> (u32, u32) {
        let max_y = layout
            .iter()
            .map(|l| l.y.unwrap_or(0) + l.h)
            .max()
            .unwrap_or(0);
        for y in 0..=max_y {
            for x in 0..=GRID_COLS.saturating_sub(w) {
                let candidate = LayoutItem {
                    i: String::new(),
                    x,
                    y: Some(y),
                    w,
                    h,
                    extras: Default::default(),
                };
                if !overlaps_any(&candidate, layout, "") {
                    return (x, y);
                }
            }
        }
        (0, max_y)
    }

    /// Insert a new tile with the given chart kind, language, and
    /// name. The id is generated by suffixing the next free numeric
    /// tail to the caller-supplied prefix (defaults to `c`). Returns
    /// the new id.
    ///
    /// Language drives two things on the new chart:
    ///   * the query-object key (`mpl` for MPL, `apl` for APL) —
    ///     matches what [`crate::app::App::sync_buffer_to_focused_tile`]
    ///     will write the user's text into on the next edit;
    ///   * the `axLang` sidecar on `ChartBase.extras` — makes
    ///     [`crate::dashboard::extract_query`] classify deterministically
    ///     on the next reload without falling back to chart-kind
    ///     heuristics.
    pub fn insert_tile(
        charts: &mut Vec<Chart>,
        layout: &mut Vec<LayoutItem>,
        kind: crate::dashboard::VizKind,
        lang: crate::dashboard::Lang,
        name: &str,
    ) -> String {
        // Generate a chart id that doesn't collide.
        // `Chart::Unknown` tiles contribute no id to the reserved set;
        // the `"cN"` namespace they live outside of can't collide with
        // them anyway.
        let used: std::collections::HashSet<&str> = charts
            .iter()
            .filter_map(|c| c.base().map(|b| b.id.as_str()))
            .collect();
        let mut n = charts.len();
        let id = loop {
            let candidate = format!("c{n}");
            if !used.contains(candidate.as_str()) {
                break candidate;
            }
            n += 1;
        };
        let (w, h) = (6, 6);
        let (x, y) = first_free_slot(layout, w, h);
        let query_key = match lang {
            crate::dashboard::Lang::Mpl => "mpl",
            crate::dashboard::Lang::Apl => "apl",
        };
        let mut extras: serde_json::Map<String, serde_json::Value> = Default::default();
        extras.insert(
            crate::dashboard::LANG_SIDECAR_KEY.to_string(),
            serde_json::Value::String(lang.as_sidecar().to_string()),
        );
        let base = ChartBase {
            id: id.clone(),
            name: Some(name.to_string()),
            query: Some(serde_json::json!({ query_key: "" })),
            extras,
        };
        charts.push(kind.to_chart(base));
        layout.push(LayoutItem {
            i: id.clone(),
            x,
            y: Some(y),
            w,
            h,
            extras: Default::default(),
        });
        id
    }

    /// Rename the chart's `name` field. Returns `Err` for unknown id.
    pub fn set_title(
        charts: &mut [Chart],
        chart_id: &str,
        title: &str,
    ) -> Result<(), &'static str> {
        let chart = charts
            .iter_mut()
            .find(|c| c.base().is_some_and(|b| b.id == chart_id))
            .ok_or("unknown chart id")?;
        // `find` above guarantees this is `Chart::Known`; bail safely
        // if a future refactor breaks that invariant.
        let base = chart.base_mut().ok_or("chart has no base")?;
        base.name = Some(title.to_string());
        Ok(())
    }
}

/// Build a server-shaped `DashboardDocument` from a single MPL buffer.
/// Used by `:dash new from-buffer` to POST a one-chart dashboard.
///
/// `kind` picks the chart variant on the wire; for TUI-only viz kinds
/// (`Bar`, `Area`, `MonitorList`, `Spacer`) we fold back to
/// `TimeSeries` because the server has no equivalent.
pub fn build_dashboard_doc_from_buffer(
    name: &str,
    kind: VizKind,
    mpl: &str,
) -> crate::axiom::DashboardDocument {
    use crate::axiom::{ChartBase, DashboardDocument, LayoutItem};
    use serde_json::{Map, json};

    let chart_id = "c1".to_string();
    let query = json!({ "mpl": mpl });
    let base = ChartBase {
        id: chart_id.clone(),
        name: Some(name.to_string()),
        query: Some(query),
        extras: Default::default(),
    };
    let chart = kind.to_chart(base);
    // Server requires owner, refreshTime, schemaVersion, timeWindow*
    // to be present. We don't model those internally yet, so stash
    // them in `extras` to satisfy the schema.
    let mut extras = Map::new();
    extras.insert("owner".to_string(), json!("X-AXIOM-EVERYONE"));
    extras.insert("refreshTime".to_string(), json!(60));
    extras.insert("schemaVersion".to_string(), json!(2));
    DashboardDocument {
        name: Some(name.to_string()),
        description: None,
        charts: vec![chart],
        layout: vec![LayoutItem {
            i: chart_id,
            x: 0,
            y: Some(0),
            w: 12,
            h: 6,
            extras: Default::default(),
        }],
        time_window_start: Some("qr-now-1h".to_string()),
        time_window_end: Some("qr-now".to_string()),
        extras,
    }
}
