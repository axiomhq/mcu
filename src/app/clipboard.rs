//! Tile clipboard, dashboard-level undo, and the `y` / `x` / `p` /
//! `P` / `o` / `O` / `u` host-side dispatchers.
//!
//! Pure on the data side; the App-side methods just stitch the
//! results into `loaded_dashboard` and update `selected_chart_idx` /
//! `dashboard_dirty` / `status`. Snapshots for one-level undo are
//! always captured *before* any mutation, so even partial failures
//! can be reverted.

use crate::axiom::{Chart, ChartBase, ChartKnownExt, LayoutItem};
use crate::dashboard::VizKind;

use super::tile_ops_shove::{ShoveDir, ShoveOutcome, shove_insert};
use super::types::{DashboardSnapshot, PickVizAction, TileSnapshot, TileSubMode};
use super::{App, ViewMode};

impl App {
    // ── snapshot / undo plumbing ─────────────────────────────────

    /// Capture the current `loaded_dashboard` state into
    /// `dashboard_undo`. Cheap (one `Vec<Chart>` clone + one
    /// `Vec<LayoutItem>` clone); call *before* any mutating verb.
    pub(super) fn snapshot_dashboard_for_undo(&mut self) {
        let Some(resource) = self.loaded_dashboard.as_ref() else {
            return;
        };
        self.dashboard_undo = Some(DashboardSnapshot {
            charts: resource.dashboard.charts.clone(),
            layout: resource.dashboard.layout.clone(),
            selected_idx: self.selected_chart_idx,
            dirty: self.dashboard_dirty,
        });
    }

    /// Variant of [`Self::snapshot_dashboard_for_undo`] for the
    /// Move/Resize commit path: charts haven't changed during the
    /// submode, but the *layout* has been cumulatively shoved by
    /// the preview. The pre-submode layout lives in the submode's
    /// `original_layout` slot — we plug that in so `u` after commit
    /// restores the user's starting layout, not the last preview.
    pub(super) fn snapshot_layout_for_undo(&mut self, pre_submode_layout: Vec<LayoutItem>) {
        let Some(resource) = self.loaded_dashboard.as_ref() else {
            return;
        };
        self.dashboard_undo = Some(DashboardSnapshot {
            charts: resource.dashboard.charts.clone(),
            layout: pre_submode_layout,
            selected_idx: self.selected_chart_idx,
            dirty: self.dashboard_dirty,
        });
    }

    /// `u` — swap the live dashboard with `dashboard_undo`. Single
    /// slot means a second `u` redoes the change (vim's default).
    pub(super) fn dashboard_undo(&mut self) {
        let Some(snap) = self.dashboard_undo.take() else {
            self.status = "nothing to undo".to_string();
            return;
        };
        let Some(resource) = self.loaded_dashboard.as_mut() else {
            self.status = "nothing to undo".to_string();
            return;
        };
        // Capture the *current* state into the slot so the next `u`
        // restores it — toggle semantics.
        let redo = DashboardSnapshot {
            charts: resource.dashboard.charts.clone(),
            layout: resource.dashboard.layout.clone(),
            selected_idx: self.selected_chart_idx,
            dirty: self.dashboard_dirty,
        };
        resource.dashboard.charts = snap.charts;
        resource.dashboard.layout = snap.layout;
        let n = resource.dashboard.charts.len();
        self.selected_chart_idx = if n == 0 {
            0
        } else {
            snap.selected_idx.min(n - 1)
        };
        self.dashboard_dirty = snap.dirty;
        self.dashboard_undo = Some(redo);
        self.status = "undo".to_string();
    }

    // ── y / x ────────────────────────────────────────────────────

    /// Capture the focused tile plus the next `n - 1` tiles in
    /// row-major order into [`Self::tile_yank`]. Returns the number
    /// of tiles actually captured (saturates at layout length).
    pub(super) fn yank_focused(&mut self, n: usize) -> usize {
        let snapshots = match self.collect_tile_snapshots(n) {
            Some(s) => s,
            None => {
                self.status = "nothing to yank".to_string();
                return 0;
            }
        };
        let count = snapshots.len();
        self.tile_yank = Some(snapshots);
        self.status = if count == 1 {
            "yanked 1 tile".to_string()
        } else {
            format!("yanked {count} tiles")
        };
        count
    }

    /// `x` — delete-and-yank `n` tiles. Snapshot for undo, capture,
    /// then delete. Selection clamps to the new layout.
    pub(super) fn cut_focused(&mut self, n: usize) -> usize {
        let snapshots = match self.collect_tile_snapshots(n) {
            Some(s) => s,
            None => {
                self.status = "nothing to cut".to_string();
                return 0;
            }
        };
        self.snapshot_dashboard_for_undo();
        let ids: Vec<String> = snapshots
            .iter()
            .map(|s| s.chart.known_base().id.clone())
            .collect();
        let count = snapshots.len();
        if let Some(resource) = self.loaded_dashboard.as_mut() {
            resource
                .dashboard
                .charts
                .retain(|c| !ids.contains(&c.known_base().id));
            resource.dashboard.layout.retain(|l| !ids.contains(&l.i));
            let new_len = resource.dashboard.charts.len();
            if self.selected_chart_idx >= new_len {
                self.selected_chart_idx = new_len.saturating_sub(1);
            }
        }
        self.tile_yank = Some(snapshots);
        self.dashboard_dirty = true;
        self.status = if count == 1 {
            "cut 1 tile".to_string()
        } else {
            format!("cut {count} tiles")
        };
        count
    }

    /// Build the snapshot vector. Returns `None` when no dashboard
    /// is loaded or the layout is empty.
    fn collect_tile_snapshots(&self, n: usize) -> Option<Vec<TileSnapshot>> {
        let resource = self.loaded_dashboard.as_ref()?;
        if resource.dashboard.charts.is_empty() {
            return None;
        }
        let take = n.max(1);
        // Build row-major order of (chart_idx, layout_clone). Charts
        // without a layout entry get a synthesised (0,0,6,6) so
        // yank-then-paste never silently drops them.
        let mut entries: Vec<(usize, LayoutItem)> = resource
            .dashboard
            .charts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let li = resource
                    .dashboard
                    .layout
                    .iter()
                    .find(|l| l.i == c.known_base().id)
                    .cloned()
                    .unwrap_or(LayoutItem {
                        i: c.known_base().id.clone(),
                        x: 0,
                        y: Some(0),
                        w: 6,
                        h: 6,
                        extras: Default::default(),
                    });
                (i, li)
            })
            .collect();
        entries.sort_by(|(_, a), (_, b)| {
            (a.y.unwrap_or(0), a.x, a.i.as_str()).cmp(&(b.y.unwrap_or(0), b.x, b.i.as_str()))
        });
        // Find the row-major position of the focused chart.
        let focus_id = resource
            .dashboard
            .charts
            .get(self.selected_chart_idx)
            .map(|c| c.known_base().id.as_str())?;
        let start = entries
            .iter()
            .position(|(_, li)| li.i == focus_id)
            .unwrap_or(0);
        let end = (start + take).min(entries.len());
        Some(
            entries[start..end]
                .iter()
                .map(|(idx, li)| TileSnapshot {
                    chart: resource.dashboard.charts[*idx].clone(),
                    layout: li.clone(),
                })
                .collect(),
        )
    }

    // ── p / P ────────────────────────────────────────────────────

    /// `p` (after = true) / `P` (after = false) — paste the yanked
    /// tile(s) below / above the focused tile. Multi-tile pastes
    /// preserve the captured bounding-box shape; pasted ids are
    /// regenerated so they don't collide.
    pub(super) fn paste_yanked(&mut self, after: bool, n: usize) {
        let Some(snapshots) = self.tile_yank.clone() else {
            self.status = "nothing to paste".to_string();
            return;
        };
        if snapshots.is_empty() {
            self.status = "nothing to paste".to_string();
            return;
        }
        let Some(focus) = self.focused_layout_or_default() else {
            self.status = "no tile to paste at".to_string();
            return;
        };
        let bbox = bbox_of(&snapshots);
        if bbox.w > super::tile_layout::GRID_COLS {
            self.status = "paste blocked: yanked block wider than 12 cols".to_string();
            return;
        }
        // Compute paste origin (`p` below, `P` above), clamped to grid.
        let origin_x = focus
            .x
            .min(super::tile_layout::GRID_COLS.saturating_sub(bbox.w));
        let origin_y = if after {
            focus.y.unwrap_or(0) + focus.h
        } else {
            focus.y.unwrap_or(0).saturating_sub(bbox.h)
        };

        self.snapshot_dashboard_for_undo();
        let total = n.max(1);
        let mut inserted = 0usize;
        let mut last_inserted_id: Option<String> = None;
        let mut total_new_rows = 0u32;
        for rep in 0..total {
            // Stack repeats vertically when `after = true`; above for `P`.
            let rep_y_offset = if after {
                rep as u32 * bbox.h
            } else {
                // For `P`, stack each repeat further above by bbox.h.
                rep as u32 * bbox.h
            };
            let rep_origin_y = if after {
                origin_y + rep_y_offset
            } else {
                // For `P`, push earlier reps higher (clamped at 0).
                origin_y.saturating_sub(rep_y_offset)
            };
            for snap in &snapshots {
                let dx = origin_x as i32 - bbox.x as i32;
                let dy = rep_origin_y as i32 + (snap.layout.y.unwrap_or(0) as i32 - bbox.y as i32);
                let Some(resource) = self.loaded_dashboard.as_mut() else {
                    break;
                };
                let new_id = next_unique_chart_id(&resource.dashboard.charts);
                let new_x = (snap.layout.x as i32 + dx).max(0) as u32;
                let new_y = dy.max(0) as u32;
                let new_li = LayoutItem {
                    i: new_id.clone(),
                    x: new_x,
                    y: Some(new_y),
                    w: snap.layout.w,
                    h: snap.layout.h,
                    extras: snap.layout.extras.clone(),
                };
                match shove_insert(&mut resource.dashboard.layout, new_li, ShoveDir::Down) {
                    Ok(ShoveOutcome { new_rows, .. }) => {
                        total_new_rows += new_rows;
                        let mut chart = snap.chart.clone();
                        set_chart_id(&mut chart, new_id.clone());
                        resource.dashboard.charts.push(chart);
                        inserted += 1;
                        last_inserted_id = Some(new_id);
                    }
                    Err(reason) => {
                        self.status = format!("paste blocked: {reason}");
                        return;
                    }
                }
            }
        }
        if inserted == 0 {
            self.status = "paste blocked: nothing inserted".to_string();
            return;
        }
        self.dashboard_dirty = true;
        // Focus the most-recently inserted tile so subsequent ops
        // operate on the paste target.
        if let (Some(resource), Some(id)) = (self.loaded_dashboard.as_ref(), last_inserted_id)
            && let Some(pos) = resource
                .dashboard
                .charts
                .iter()
                .position(|c| c.known_base().id == id)
        {
            self.selected_chart_idx = pos;
        }
        self.status = match total_new_rows {
            0 => format!("pasted {inserted} tile(s)"),
            r => format!("pasted {inserted} tile(s), +{r} row(s)"),
        };
    }

    // ── o / O ────────────────────────────────────────────────────

    /// `o` / `O` entry point — prompt the user for a viz kind via
    /// [`TileSubMode::PickViz`] with [`PickVizAction::Open`]. The first
    /// picked kind is reused for the remaining `n - 1` openings so `5o`
    /// only prompts once.
    pub(super) fn enter_tile_open_pick(&mut self, above: bool, n: usize) {
        if self.loaded_dashboard.is_none() {
            self.status = "no dashboard loaded".to_string();
            return;
        }
        self.tile_submode = TileSubMode::PickViz {
            cursor: 0,
            action: PickVizAction::Open {
                above,
                remaining: n.max(1),
            },
        };
        let label = if above { "OPEN-ABOVE" } else { "OPEN-BELOW" };
        self.status = format!("{label}: arrows pick kind, Enter inserts, Esc cancels");
    }

    /// Commit one `o`/`O` insertion after the user picks a kind.
    /// Called once per repeat by the PickViz handler when the action
    /// is [`PickVizAction::Open`].
    #[allow(clippy::missing_panics_doc)]
    pub(super) fn open_new_row_with_kind(&mut self, above: bool, kind: VizKind) -> bool {
        let Some(focus) = self.focused_layout_or_default() else {
            self.status = "no tile to open from".to_string();
            return false;
        };
        let h = focus.h.max(1);
        let new_y = if above {
            focus.y.unwrap_or(0).saturating_sub(h)
        } else {
            focus.y.unwrap_or(0) + focus.h
        };
        // Default to a focused-tile-shaped pane, clamped to grid.
        let w = focus.w.min(super::tile_layout::GRID_COLS);
        let x = focus.x.min(super::tile_layout::GRID_COLS.saturating_sub(w));
        let Some(resource) = self.loaded_dashboard.as_mut() else {
            return false;
        };
        let new_id = next_unique_chart_id(&resource.dashboard.charts);
        let new_li = LayoutItem {
            i: new_id.clone(),
            x,
            y: Some(new_y),
            w,
            h,
            extras: Default::default(),
        };
        match shove_insert(&mut resource.dashboard.layout, new_li, ShoveDir::Down) {
            Ok(_) => {
                let base = ChartBase {
                    id: new_id.clone(),
                    name: Some("new tile".to_string()),
                    query: Some(serde_json::json!({ "mpl": "" })),
                    extras: Default::default(),
                };
                resource.dashboard.charts.push(kind.to_chart(base));
                self.selected_chart_idx = resource.dashboard.charts.len() - 1;
                self.dashboard_dirty = true;
                true
            }
            Err(reason) => {
                self.status = format!("open blocked: {reason}");
                false
            }
        }
    }

    // ── shared helpers ───────────────────────────────────────────

    /// Focused tile's layout entry; synthesises a (0,0,6,6) default
    /// for orphan charts so paste/open always have an anchor.
    fn focused_layout_or_default(&self) -> Option<LayoutItem> {
        let resource = self.loaded_dashboard.as_ref()?;
        let chart = resource.dashboard.charts.get(self.selected_chart_idx)?;
        let id = chart.known_base().id.as_str();
        Some(
            resource
                .dashboard
                .layout
                .iter()
                .find(|l| l.i == id)
                .cloned()
                .unwrap_or(LayoutItem {
                    i: id.to_string(),
                    x: 0,
                    y: Some(0),
                    w: 6,
                    h: 6,
                    extras: Default::default(),
                }),
        )
    }

    /// `true` when the current state permits picking a viz kind for
    /// `o`/`O`. Convenience for `legend_label_tags` / similar.
    #[allow(dead_code)]
    pub(super) fn in_open_pick(&self) -> bool {
        matches!(
            self.tile_submode,
            TileSubMode::PickViz {
                action: PickVizAction::Open { .. },
                ..
            }
        ) && self.view_mode == ViewMode::Grid
    }
}

/// Axis-aligned bounding box of a set of layout snapshots.
struct Bbox {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

fn bbox_of(snapshots: &[TileSnapshot]) -> Bbox {
    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for s in snapshots {
        let x = s.layout.x;
        let y = s.layout.y.unwrap_or(0);
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + s.layout.w);
        max_y = max_y.max(y + s.layout.h);
    }
    if min_x == u32::MAX {
        return Bbox {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
    }
    Bbox {
        x: min_x,
        y: min_y,
        w: max_x - min_x,
        h: max_y - min_y,
    }
}

/// Generate a `c<n>` id that doesn't collide with any existing chart.
fn next_unique_chart_id(charts: &[Chart]) -> String {
    let used: std::collections::HashSet<&str> =
        charts.iter().map(|c| c.known_base().id.as_str()).collect();
    let mut n = charts.len();
    loop {
        let candidate = format!("c{n}");
        if !used.contains(candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

/// Rewrite a `Chart`'s `ChartBase.id`. Necessary because `Chart` is
/// an enum over per-variant `ChartBase`; there's no shared mutable
/// accessor on the type.
fn set_chart_id(chart: &mut Chart, id: String) {
    chart.known_base_mut().id = id;
}
