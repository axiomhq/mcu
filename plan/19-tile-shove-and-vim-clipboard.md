# Step 19 — Auto-shove on move/resize + vim-style tile yank/cut/paste/open

## Status

**Implemented.** Auto-shove, tile clipboard, one-level dashboard undo,
and the `:tile mv!`/`size!`/`yank`/`cut`/`paste`/`open`/`undo`
Ex-command surface are all live. Implementation notes (small
deviations from the original plan):

* `tile_ops_shove` lives at `src/app/tile_ops_shove.rs` as planned.
  17 unit tests covering single-shove, three-tile chains, right→down
  fallback at col 12, dense-grid cascade, resize grow/shrink,
  `shove_insert`, and the "unrelated rows stay put" invariant.
* `TileSubMode::Move`/`Resize` carry `original_layout: Vec<LayoutItem>`,
  `original_id: String`, and a cumulative `(dx,dy)` / `(dw,dh)`. The
  preview is recomputed from `(original_layout, original_id, delta)`
  on every arrow keypress — ratcheting bug from step 18 is fixed.
* `DashboardParser` (count + verb) lives at `src/app/dashboard_cmd.rs`
  with 10 unit tests; wires into `handle_dashboard_key`'s idle
  branch. Counts also repeat navigation (`3j`, `2l`).
* Tile clipboard + open-pick overlay + undo helpers live at
  `src/app/clipboard.rs`. `TileSubMode::OpenPick { cursor, above,
  remaining }` was added so `5o` only prompts for a kind once.
* `:tile` Ex-commands now strip a trailing `!` from the sub-command
  (e.g. `:tile mv! 3 0`); outer head-bang is unused for `:tile`.
* App-level integration tests (13 new) cover shove via `m`+arrows,
  Esc-reverts-cascade, yank/cut/paste round-trips, `u` toggle
  (undo/redo), `3y` count, `5o` single-prompt, and `:tile mv!` /
  `:tile paste` / `:tile open!` / `:tile undo` Ex paths.
* `docs/keys.md` updated with a new Dashboard pane section and
  `:tile` sub-command additions.

Total new tests: 30 (17 shove + 10 parser + 13 app integration +
4 :tile ex-cmd). Total test suite: **509 passing**, up from 492
at step-18 close.


## Incremental outcome

Two upgrades to the dashboard grid editor from step 18:

1. **Move/resize stop rejecting collisions.** Overlapping tiles get
   pushed out of the way (right first, falling through to down) so
   the user can drag a tile through other tiles freely. New rows
   appear naturally — *only the tiles in the cascade chain move; the
   rest of the dashboard stays put*.
2. **Tile clipboard with vim-style verbs.** `y`/`x`/`p`/`P`/`o`/`O`
   accept counts (`3y`, `2x`, `5o`) and round-trip multi-tile blocks
   through a per-`App` tile yank register, preserving each block's
   relative shape.

A small **dashboard-level undo** (`u`) snapshots before every
mutation so the user can back out of a bad shove or paste in one
keystroke.

## User-visible improvement

### Auto-shove (move + resize)

- In `MOVE` sub-mode, `l` / `Right` repeatedly pushes the tile
  rightward through other tiles; victims slide right; if the chain
  hits column 12, the tail wraps to a fresh row below. `Esc` puts
  every shoved tile back exactly where it started.
- In `RESIZE` sub-mode, `Right`/`Down` grow the tile through
  neighbours instead of bouncing off them.
- Move-left and move-up keep the existing "blocked" semantics (no
  destructive shove off the top/left edge).
- Status line reports the cascade: `move ok: 3 tiles shoved`,
  `resize ok: 1 tile shoved + 1 new row`.

### Tile clipboard

- `y` — yank focused tile into `App.tile_yank`. `3y` yanks the
  focused + next two tiles in row-major order. Status:
  `yanked 3 tiles`.
- `x` — delete-and-yank (cut). `2x` cuts focused + next. No
  confirm — `d` retains its confirm-delete behaviour for users who
  fat-finger.
- `p` — paste below focused tile (new row, `y = focused.y + h`).
  Multi-tile pastes preserve the captured bounding-box shape.
  Cascade resolves any overlap with existing tiles.
- `P` — paste above focused tile (`y = focused.y - pasted.h`,
  clamped to `0` via the cascade).
- `o` — open a fresh tile in a new row below focused (same w/h as
  focused, `x = 0`). `3o` stacks three. Drops directly into the
  add-pick overlay so the user picks the kind, then commits.
- `O` — open above. Otherwise identical to `o`.
- `u` — undo the last dashboard mutation (one level). Restores both
  the chart list and layout from a single snapshot taken at the start
  of each mutating command.

### Counts work uniformly

Any of `y`/`x`/`p`/`P`/`o`/`O` can be prefixed with a decimal count:
`3y`, `12p`, `2O`. `0` is *not* a count — it falls through to the
existing `gg`/`0`-style direct keys so we don't shadow them. Counts
clear on any non-digit, non-verb key (e.g. typing `2h` navigates two
cells left, then resets count to 0 — same as vim).

## Scope

### Add

- `src/app/tile_ops_shove.rs` (new sibling of `tile_layout::tile_ops`):
  - `pub enum ShoveDir { Right, Down }`
  - `pub struct ShoveOutcome { pub moved: Vec<String>, pub new_rows: u32 }`
  - `pub fn shove_move(layout: &mut Vec<LayoutItem>, id: &str, dx: i32, dy: i32) -> Result<ShoveOutcome, &'static str>`
  - `pub fn shove_resize(layout: &mut Vec<LayoutItem>, id: &str, dw: i32, dh: i32) -> Result<ShoveOutcome, &'static str>`
  - `pub fn shove_insert(layout: &mut Vec<LayoutItem>, new_tile: LayoutItem, dir: ShoveDir) -> Result<ShoveOutcome, &'static str>`
  - Plus private `cascade(layout, blocker_id, dir)` BFS helper.

- `src/app/dashboard_cmd.rs` (new):
  - `pub struct DashboardParser { count: usize }`
  - `pub enum DashCommand { Yank{n}, Cut{n}, Paste{after, n}, Open{above, n}, Undo, Passthrough(KeyEvent) }`
  - `pub fn feed(&mut self, key: KeyEvent) -> DashStep { Pending | Cancel | Emit(DashCommand) }`
  - Grammar is the small `count? verb` flavour the oracle recommended;
    no operator-pending state.

- `App` fields:
  ```rust
  pub tile_yank: Option<Vec<TileSnapshot>>,         // captured by y/x
  pub dashboard_undo: Option<DashboardSnapshot>,    // one-level undo
  dashboard_cmd: DashboardParser,                   // count accumulator
  ```
  where
  ```rust
  pub struct TileSnapshot {
      pub chart: crate::axiom::Chart,
      pub layout: crate::axiom::LayoutItem,   // absolute coords as captured
  }
  pub struct DashboardSnapshot {
      pub charts: Vec<crate::axiom::Chart>,
      pub layout: Vec<crate::axiom::LayoutItem>,
      pub selected_idx: usize,
  }
  ```

- `TileSubMode` rewrites:
  ```rust
  Move   { original_layout: Vec<LayoutItem>, dx: i32, dy: i32 }
  Resize { original_layout: Vec<LayoutItem>, dw: i32, dh: i32 }
  ```
  Each arrow key updates the cumulative delta, then recomputes the
  preview from `original_layout` + `delta` via `shove_move` /
  `shove_resize`. This avoids the "ratcheting" bug where moving
  right-then-left would leave shoved tiles stranded.

### Keep

- Existing `tile_ops::{translate, resize, delete, insert_tile, …}`
  stay pure and unchanged. The shove path lives alongside them and
  is the new default for keyboard sub-modes; `:tile mv`/`size`
  Ex-commands keep using the strict (reject-on-collision)
  `translate`/`resize` so scripts get the old behaviour. A
  `:tile mv! …` / `:tile size! …` bang variant opts into shove for
  the Ex layer.
- `d` (confirm-delete) and `a` (add-pick) work as before.
- The 12-col clamp, 1-cell minimum size, and `y: Option<u32>`
  serialisation are preserved. Cascades always materialise `y` to
  `Some(_)`.

### Out of scope (deferred)

- Multi-level undo. Step 19 ships one-level only.
- Auto-compaction (collapse trailing empty rows after deletions /
  shoves). Manual `:tile compact` may land in step 20.
- Operator-pending dashboard verbs (`yj`, `2yl`, `dap`). The flat
  `count + verb` grammar is enough for the requested surface.
- Mouse drag-to-move.

## Data-model deltas

| Field                                | Owner       | Lifetime                                  |
|--------------------------------------|-------------|-------------------------------------------|
| `App.tile_yank`                       | App         | Survives until next `y`/`x` overwrites.   |
| `App.dashboard_undo`                  | App         | Refreshed on every mutating command.      |
| `App.dashboard_cmd`                   | App         | Per-keystroke count accumulator.          |
| `TileSubMode::Move.original_layout`   | App         | Snapshotted on `m`; reverted on `Esc`.    |
| `TileSubMode::Resize.original_layout` | App         | Snapshotted on `s`; reverted on `Esc`.    |

`tile_yank` survives navigation, view-mode flips, and even dashboard
swaps (paste into a different dashboard is intentional, like vim's
unnamed register across buffers).

## Algorithm: cascade-shove

Pure, on a cloned `Vec<LayoutItem>`. Commit only after the final
layout passes validation (no overlaps, no off-grid, no `w<1`/`h<1`).

```text
shove_move(layout, id, dx, dy):
    moved = clone(layout)
    blocker = find_mut(moved, id)
    apply delta to blocker (clamped to [0, GRID_COLS - blocker.w] for x)
    primary_dir = if dy != 0 { Down } else { Right }   // strict positive
    if dx < 0 or dy < 0:
        return strict_translate(layout, id, dx, dy)    // legacy reject
    cascade(moved, id, primary_dir)
    validate(moved); commit if ok

cascade(layout, blocker_id, dir):
    queue = collect_overlapping(layout, blocker_id)
        .sorted_by(dir.sort_key)         // (x,y,id) for Right, (y,x,id) for Down
    visited = {blocker_id}
    loop_cap = max(256, layout.len()² * 4)
    while let Some(victim_id) = queue.pop_front():
        if !visited.insert(victim_id): continue
        if loop_cap == 0: return Err("cascade did not converge")
        loop_cap -= 1
        let new_dir = shove_one(layout, victim_id, blocker_id, dir)
        for next_overlap in collect_overlapping(layout, victim_id)
                            .sorted_by(new_dir.sort_key):
            queue.push_back(next_overlap)

shove_one(layout, victim_id, blocker_id, dir):
    blocker = find(layout, blocker_id)
    victim = find_mut(layout, victim_id)
    match dir:
        Right:
            let nx = blocker.x + blocker.w
            if nx + victim.w <= GRID_COLS:
                victim.x = max(victim.x, nx)
                return Right
            // overflows: fall through to Down
            fall through
        Down:
            let ny = blocker.y_or_0() + blocker.h
            victim.y = Some(max(victim.y_or_0(), ny))
            return Down
```

Invariants:

- `x`/`y` along any cascade chain are monotonically non-decreasing →
  the BFS terminates.
- `victim.x = max(victim.x, ...)` (and similarly for `y`) guarantees
  the cascade never moves a tile *backwards* into the freed area.
- Right-fallback-to-down keeps `victim.x` unchanged on the Down
  branch (we don't snap to column 0 — that would shuffle the layout
  more than necessary).

## Algorithm: yank / paste shape

Yank:

```text
yank_n(charts, layout, focused_idx, n):
    let order = sort_row_major(layout)              // (y, x, id)
    let start_pos = order.position(focused_idx)
    let take = order[start_pos .. min(start_pos + n, order.len())]
    return take.map(|idx| TileSnapshot {
        chart: charts[idx].clone_with_fresh_query_id_kept(),
        layout: layout[idx_for(charts[idx].id)].clone(),
    })
```

Counts saturate at `len() - start_pos` (oracle's note: never wrap).

Paste:

```text
paste(snapshots, focused_layout, after):
    let bbox = bounding_box(snapshots.layout)        // (x0, y0, w, h)
    if bbox.w > GRID_COLS: return Err("yanked block wider than 12 cols")
    let origin = if after {
        (focused.x.min(GRID_COLS - bbox.w), focused.y + focused.h)
    } else {
        (focused.x.min(GRID_COLS - bbox.w), focused.y.saturating_sub(bbox.h))
    }
    for snap in snapshots:
        let new_id = next_unique_id(charts);
        let new_layout = snap.layout
            .with_id(new_id)
            .translate_to(origin + (snap.layout.{x,y} - bbox.{x0,y0}));
        // Insert as a hard blocker; existing tiles shove out of the way.
        shove_insert(layout, new_layout, ShoveDir::Down);
        charts.push(snap.chart.with_id(new_id));
```

Multi-tile yanks therefore land as the same visual rectangle they
were copied from, translated to the paste origin; tiles that
previously occupied that rectangle cascade rightward/downward.

## Algorithm: `o` / `O` open new row

```text
open_below(focused, kind, count):
    for _ in 0..count:
        let new = LayoutItem {
            x: focused.x,
            y: Some(focused.y + focused.h),
            w: focused.w,
            h: focused.h,
            …,
        };
        shove_insert(layout, new, ShoveDir::Down);
        charts.push(chart_with_default(kind, "new tile"));
open_above is symmetric (y = focused.y.saturating_sub(focused.h)).
```

If `kind` is unspecified, `o` drops the user into the existing
`AddPick` overlay so they can pick the viz kind — same UX as the
`a` shortcut, just with the slot pre-decided.

## Tasks

1. **`tile_ops_shove.rs`** — pure module + tests:
   - `shove_move`, `shove_resize`, `shove_insert`.
   - Unit tests:
     - single overlap, right shove fits.
     - chain of three tiles, right shove cascades.
     - chain that wraps from right → down at col 12.
     - down-shove only path (move-down into a row).
     - cycle-prevention: cap triggers `"cascade did not converge"`.
     - validation rejects committed layouts that still overlap.
     - `dx<0` / `dy<0` falls through to strict reject (parity with
       step 18).
2. **`TileSubMode` rewrite** — store `original_layout: Vec<LayoutItem>`
   + cumulative delta:
   - `enter_tile_move` snapshots full layout, resets delta to `(0,0)`.
   - Each arrow key updates the delta, then recomputes the layout
     by cloning `original_layout` and replaying `shove_move(... ,
     delta.dx, delta.dy)`.
   - `Esc` writes `original_layout` back wholesale.
   - `Enter` commits and clears the submode.
3. **`DashboardParser`** — `count + verb` grammar:
   - `feed(KeyEvent)` returns `Pending` for digits and `Emit(DashCommand)`
     for verbs.
   - Non-digit, non-verb keys clear the count and return
     `Passthrough(key)` so `handle_dashboard_key` can run its normal
     idle branch (navigation, mode entry, etc.).
   - `0` while `count == 0` passes through (so `gg` / `0` keep
     working).
4. **`App.tile_yank`** + helpers:
   - `yank_focused(n)`, `cut_focused(n)`, `paste(after, n)`,
     `open_with_kind(above, n, kind)`.
   - Each refreshes `dashboard_undo` *before* mutating.
5. **`App.dashboard_undo`** + `u` binding:
   - `take_dashboard_undo()` restores the snapshot, swaps the current
     state into the slot (so a second `u` is a redo — vim's
     single-level undo behaves this way too).
6. **`handle_dashboard_key` wiring**:
   - Run every key through `DashboardParser` first.
   - On `Emit(DashCommand::…)` dispatch to the new helpers.
   - On `Passthrough(key)` fall back to today's idle branch.
   - Wire `u` to `take_dashboard_undo`.
7. **`:tile` Ex-command updates**:
   - `:tile mv x y` keeps strict semantics.
   - `:tile mv! x y` / `:tile size! w h` opt into shove.
   - `:tile yank [n]`, `:tile cut [n]`, `:tile paste [P]`,
     `:tile open [O] [kind]` mirror the keyboard verbs for scripts.
8. **Status / help**:
   - Status line summarises cascade results
     (`yanked 3 tiles`, `move ok: 2 tiles shoved`,
     `paste ok: 5 tiles, +1 row`).
   - Extend `docs/keys.md` with a "Dashboard clipboard" section.
   - Help modal pulls from the same source automatically.

## Acceptance criteria

- Moving a tile into an occupied cell shoves the occupant; the
  shoved tile's borders update each frame; `Esc` restores the
  original layout exactly (`assert_eq!(layout, original_layout)`).
- Resize-grow over a neighbour shoves; resize-shrink leaves
  neighbours untouched.
- Move-left / move-up that would collide still report
  `move blocked: would overlap another tile`.
- A move that runs the cascade past col 12 places the tail tile in
  a new row at `y = blocker.y + blocker.h`, without moving any
  tiles that weren't in the cascade chain (golden test on a 6-tile
  layout).
- `3y` then `p` produces three identical tiles immediately below
  the focused one, preserving the original block shape. Pasted
  tiles get fresh `c<n>` ids that don't collide with existing
  charts.
- `2x` removes two tiles into the yank register; `p` restores them
  at the new focus.
- `o` opens a kind-picker then inserts a tile in a brand-new row
  below focused; `5o` repeats five times in sequence (one picker
  per insert? — design as: first picker chooses the kind, the next
  4 reuse it; status reflects the count).
- `u` rolls back the most recent yank/cut/paste/open/move/resize
  in one keystroke. A second `u` redoes it (single-slot toggle).
- `:tile mv 3 0` on a collision still reports the strict error;
  `:tile mv! 3 0` succeeds and reports the cascade count.
- All step-18 acceptance tests still pass — `tile_ops` behaviour
  is unchanged.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`.
- New `tile_ops_shove` unit tests (see Task 1).
- New `dashboard_cmd::Parser` unit tests:
  - `1`+`2`+`y` → `Yank{n:12}` (digits compose).
  - `y` (no digits) → `Yank{n:1}`.
  - `2`+`h` → `Passthrough(h)` *and* count cleared after the
    passthrough returns. (Matches vim — `2h` moves twice; the host
    consumes the count itself.)
  - `3`+`Esc` → `Cancel`, count cleared.
- App-level tests under `src/app/tests.rs`:
  - shove via `m` + `l` repeatedly cascades and `Esc` restores.
  - `3y` + `p` round-trips three tiles with stable ids.
  - `u` reverts; `u` again redoes.
- Round-trip golden: load fixture → `m` shove → `:w` → reload →
  diff matches the recomputed layout.

## Risks / mitigations

- **Cascade ordering is observable.** Two equally valid victim
  orderings can produce different final layouts. The sort key in
  `cascade` (`(x,y,id)` for Right, `(y,x,id)` for Down) makes the
  outcome deterministic across runs and platforms.
- **Ratcheting in Move submode.** Mitigated by snapshotting the
  full layout on entry and recomputing from snapshot + cumulative
  delta on every arrow keypress (oracle's catch — the existing
  step-18 design of storing only the moved tile's `LayoutItem`
  would have produced inconsistent results once shove arrived).
- **Loop runaway.** `loop_cap = max(256, n² * 4)` plus a `visited`
  set; failure surfaces as a status error and the cloned layout
  is discarded, leaving the original intact.
- **No multi-level undo.** Acceptable for v1 because the worst
  case (an unwanted shove of many tiles) is one `u` away. If users
  ask for deeper history, promote `dashboard_undo` to a bounded
  ring buffer in step 20.

## What this enables next (out of scope)

- Multi-level undo / redo.
- `:tile compact` to garbage-collect trailing empty rows.
- Visual mode in the dashboard pane (`V` selects a rectangle of
  tiles → operator works on the whole rect).
- Auto-pack / "tidy" command that re-flows everything to top-left.
- Operator-pending tile verbs (`yj` / `2dl`) once the flat parser
  is shown to be insufficient.
