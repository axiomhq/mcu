# Step 18 — Dashboard grid view + tile editing

## Status (post-18a)

**18a — read-only grid + spatial selection (DONE):**
- `ViewMode { Solo, Grid }` + `Pane::Dashboard`.
- Loading a dashboard with ≥2 charts auto-switches to Grid view and
  focuses `Pane::Dashboard`.
- Grid renderer projects the server's 12-column `LayoutItem` x/y/w/h
  onto the graph pane's `Rect`s, with one bordered chrome block per
  chart showing kind glyph + name + MPL/APL preview.
- Arrow keys (and `h/j/k/l`) navigate spatially by centroid distance;
  `Tab` / `Shift-Tab` cycle row-major; `Enter` / `v` zooms the selected
  chart back into Solo (re-seeds the editor buffer); `Esc` returns
  focus to the editor without leaving Grid view.
- `Ctrl-w d` jumps straight to the dashboard pane when grid is
  available; `:grid` / `:solo` Ex-commands flip manually.

**18b — tile editing (DONE):**
- `TileSubMode { Idle, Move, Resize, ConfirmDelete, AddPick }` state
  machine on App. Each sub-mode owns the keymap while active; `Esc`
  reverts to `Idle` (Move/Resize restore the original `LayoutItem`).
- `m` / `s` / `d` / `a` keyboard shortcuts in the dashboard pane.
  Sub-mode badge in the pane title (`MOVE`, `RESIZE`, `DELETE?`,
  `ADD`) and in the bottom status badge (`DASH-MOVE`, etc.).
- `:tile add|rm|mv|size|title` Ex-commands sharing the same
  `tile_ops::*` mutators as the keyboard sub-modes.
- Pure `tile_ops` module: `translate`, `resize`, `delete`,
  `insert_tile`, `set_title`, `overlaps_any`, `first_free_slot` —
  each unit-tested.
- Collision rejection: move/resize that would overlap another tile is
  rejected with `"move blocked: would overlap another tile"` /
  `"resize blocked: …"` in the status; the layout snaps back. Off-grid
  moves and below-minimum resizes equally rejected.
- Confirm-delete and add-tile-picker modal overlays.
- `dashboard_dirty` flag set on every mutation, cleared on
  `DashboardSaved` and dashboard-mode `write_file`. Surfaced as `[+]`
  in the dashboard pane title.

**18b — still pending (the bigger lift):**
- Per-`TileId` async query state (App.series →
  `BTreeMap<TileId, TileState>`) so each grid cell renders live data
  concurrently. Today the grid shows a one-line MPL/APL preview per
  tile; zoom-to-Solo (`Enter`/`v`) is how you actually run queries.
- Editor⇄tile binding (`flush_editor_into_tile` /
  `load_tile_into_editor`) on selection change — typing in the editor
  while focused on the grid currently doesn't propagate to the
  selected chart's query.
- `R` / `Ctrl-R` rerun-tile shortcuts.
- Per-tile chrome status pips (running / error / cached / ok).
- Help-modal additions documenting the new shortcuts.

---


## Incremental outcome

Render the loaded dashboard as a **grid of tiles** instead of a single
focused tile. Add the editing affordances (move/resize/add/delete tiles,
edit a tile's query/kind/options) that turn the TUI into a real
dashboard editor.

## User-visible improvement

- Opening a dashboard renders all tiles laid out per `Layout` +
  `GridPos` in a new **Dashboard pane** above the editor.
- `Ctrl-w d` focuses the dashboard pane (consistent with the
  existing `Ctrl-w j/k/l/w` chord family); `Ctrl-w j` returns to
  the editor.
- Inside the dashboard pane: arrow keys navigate spatially between
  tiles, `Tab` / `Shift-Tab` cycles in z-order.
- The **selected tile drives the editor pane**: its query text is
  loaded into the editor, and edits write back to that tile. Switching
  selection auto-commits any pending edit into the previously selected
  tile (no "unsaved buffer lost" surprises).
- Tile-scoped shortcuts (only while the dashboard pane is focused):
  - `m` — **move** sub-mode; arrow keys nudge the tile by one grid
    cell. `Enter` commits, `Esc` reverts.
  - `s` — **resize** sub-mode; arrow keys shrink/grow `w`/`h` by one
    cell (Right/Down grow, Left/Up shrink). `Enter` commits,
    `Esc` reverts.
  - `d` — **delete** focused tile after a `y/N` confirm prompt.
  - `v` — **view zoom**: tile fills the pane, editor stays bound to
    it but read-only-ish (Normal mode); `Esc`/`q` returns to grid.
  - `e` — **edit zoom**: same as `v`, but focus jumps into the editor
    and the tile's query opens in Insert-ready Normal mode; `Esc`
    drops back to grid focus, `q` exits zoom entirely.
  - `a` — **add tile** picker (kind list); placed at first free slot.
  - `R` — rerun the focused tile's query; `Ctrl-R` reruns all tiles.
- `:tile add <kind>`, `:tile rm`, `:tile mv <x> <y>`, `:tile size <w>
  <h>`, `:tile title <text>` mutate the layout (Ex equivalents of the
  shortcuts above; useful for scripting / macros).
- `q` from a zoomed tile returns to grid; `q` from grid quits (with
  dirty prompt).

## Scope

### Add

- `src/dashboard_layout.rs`:
  - Translate `Layout { cols, row_h }` + each tile's `GridPos { x, y,
    w, h }` into ratatui `Rect`s.
  - Min-size policy: tiles below a threshold render a compact
    placeholder (icon + truncated title); avoids unreadable
    micro-charts.
- `src/viz/tile_chrome.rs`:
  - Per-tile bordered block with title, kind glyph, busy spinner,
    error pip, and a focus indicator (yellow border on focus, dim
    otherwise — matching the existing `pane_block` style).
  - Sub-mode badge in the border title when the tile is in `MOVE` or
    `RESIZE` so the user always knows what arrow keys will do.
- New `App.focus` variant `Pane::Dashboard`, in addition to the
  existing `Editor` / `Legend` / `Params`.
- New tile sub-mode on `App`:
  ```rust
  enum TileSubMode {
      Idle,
      Move   { original: GridPos },   // Esc reverts to original
      Resize { original: GridPos },
      ConfirmDelete,
  }
  ```
- Editor binding to selection:
  - `App` carries `editor_bound_to: Option<TileId>` in grid mode.
  - On selection change, flush the editor buffer into the previous
    tile's `Query`, then load the new tile's query into the editor.
  - Note tiles bind their markdown body to the editor; spacer tiles
    leave the editor empty + read-only with a dim "(spacer)" hint.
- View mode on `App`: `Grid` vs `Solo(TileId, SoloKind)` where
  `SoloKind = View | Edit` (driven by `v` vs `e`). Solo reuses the
  existing layout (graph + legend + editor + params + status); the
  `Edit` variant also focuses the editor pane on entry.

### Keep simple

- Layout is row-major, fixed-column grid (matches Axiom's web UI's
  default). No free-form pixel positioning.
- No drag-with-mouse — keyboard only. Mouse can come later.
- Multi-select and bulk operations are out of scope.
- Auto-layout / auto-fit is out of scope; tiles keep their saved
  positions.
- Move/Resize collision policy: a move that overlaps another tile
  is rejected with a brief flash on the status bar; no auto-shove.
  Keeps the model simple; auto-pack can come later.

## Data model deltas

```rust
pub enum ViewMode {
    Solo,                  // step-11 single-tile renderer (default for .mpl)
    Grid,                  // step-18 full layout
    SoloFromGrid(TileId),  // zoomed-in editor over one tile of a Grid
}
```

`Tile.pos` is already on the model from step 11; step 18 is the first
step that actually consumes it.

## Tasks

1. Grid renderer: project `GridPos` into `Rect`s; respect terminal
   minimum-size; clip overflow with a "(too small)" placeholder.
2. Focus + spatial navigation:
   - `Ctrl-w d` focuses the dashboard pane.
   - Arrow keys when `TileSubMode::Idle` pick the nearest tile in the
     chosen cardinal direction by centroid distance with a tie-break
     on overlap in the perpendicular axis.
   - `Tab` / `Shift-Tab` step through tiles in `GridPos` z-order
     (row-major, top-left first).
3. Editor binding:
   - On every selection change, run `flush_editor_into_tile(prev)`
     followed by `load_tile_into_editor(next)`.
   - Mark the *dashboard* dirty (not just the buffer) so `:w` knows
     to write the dashboard file. A small per-tile dirty pip in the
     border surfaces uncommitted edits visually.
4. Per-tile chrome: title bar, status pip (running / error / cached /
   ok), kind glyph, sub-mode badge.
5. Move sub-mode:
   - Enter on `m` (capture `original = tile.pos`).
   - Arrow keys mutate `tile.pos.x` / `tile.pos.y` by one cell.
   - `Enter` commits, `Esc` restores `original`.
6. Resize sub-mode:
   - Enter on `s` (same `original` capture).
   - Right/Down grow `w`/`h`; Left/Up shrink (clamped to a minimum
     of 1 cell in each dimension; clamped to `Layout.cols` width).
   - `Enter` commits, `Esc` restores.
7. Delete confirm:
   - `d` enters `ConfirmDelete`; show a `y/N` overlay over the focused
     tile. `y` removes the tile (and cancels its async tasks); any
     other key cancels. Default is No.
8. Zoom:
   - `v` → `Solo(id, View)`: focus stays on the dashboard pane (the
     editor and legend are still around the zoomed tile).
   - `e` → `Solo(id, Edit)`: same view, but `App.focus = Editor` on
     entry so the user is one keystroke from `i` to insert.
   - `Esc`/`q` returns to grid.
9. Add tile (`a`):
   - Kind picker (List widget with the implemented kinds).
   - Inserts a tile with sensible defaults at the first free grid
     slot; selects it; opens it in `Solo(_, Edit)` so the user lands
     in the editor with an empty query.
10. Rerun:
    - `R` reruns the focused tile only; `Ctrl-R` reruns all.
11. Mutating Ex-commands (`:tile add|rm|mv|size|title`) operate on
    the focused tile (or take an explicit id) and share the same
    underlying mutators as the shortcuts.
12. Dirty tracking: a dashboard-level dirty flag plus a per-tile flag
    for uncommitted editor edits. `:w` clears both.
13. Per-tile async results (keyed by `TileId` since step 11) light up
    the grid concurrently; one spinner per tile.
14. Help modal additions: extend the `:help` table in `ui.rs` with a
    new "— Dashboard pane —" section listing every shortcut above.

## Acceptance criteria

- A dashboard with N tiles renders N bordered panes; resizing the
  terminal reflows without crashing.
- `Ctrl-w d` focuses the dashboard pane; arrow keys move spatially;
  `Tab` / `Shift-Tab` cycle in z-order.
- Changing the focused tile updates the editor pane to that tile's
  query within one frame; typing in the editor mutates the bound
  tile in-memory and marks the dashboard dirty.
- `m` then arrows visibly nudge the tile in the grid; `Esc` restores
  the pre-move position pixel-for-pixel.
- `s` then arrows resize symmetrically (Right/Down grow, Left/Up
  shrink); minimum size enforced; `Esc` restores.
- `d` shows a yes/no confirm; `y` deletes and cancels any in-flight
  query for that tile; any other key cancels.
- `v` and `e` both zoom the focused tile; `e` additionally lands the
  cursor in the editor.
- `:tile add line` creates a new tile at the first free grid slot
  with sensible defaults; `:w` round-trips through `dashboard_io`.
- Solo mode for `.mpl` files still works exactly as it did before
  step 18 \u2014 no regression for single-buffer users.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`
- Layout unit tests on a few canonical grid sizes.
- Round-trip test: load fixture dashboard → mutate via `:tile` cmds →
  save → diff matches an expected golden file.
- Manual: open a real dashboard with 6+ tiles of mixed kinds, edit
  one tile's query in the inspector, save, reload from server.

## What this enables next (out of scope here)

- Server-side dashboard sharing / permissions UI.
- Live-collab markers when another user is editing.
- Variable dropdowns at the dashboard header bar.
- Cross-tile linking (click a series to filter neighbouring tiles).
