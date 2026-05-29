# Step 27 — Mouse support

## Status

**In progress.** Requires steps 18 (grid), 23/24 (trace view), and the
existing editor / pane-focus infrastructure.

## Incremental outcome

The TUI becomes navigable by mouse in addition to the keyboard. Mouse
events are captured by crossterm and dispatched through a new
`App::on_mouse` entry point that mirrors the overlay-gating discipline
of `App::on_key`. No keyboard binding changes; the mouse is purely
additive.

## User-visible improvement

1. **Click a dashboard-grid tile** to select/focus it (Grid view).
2. **Click a panel** (editor, legend, params, table, trace tree,
   trace detail) to focus it.
3. **Click the topbar tabs** to switch buffer/view: `QUERY` →
   query/editor, `DASHBOARD` → loaded dashboard grid.
4. **Scroll wheel** in the trace view scrolls the tree (cursor step)
   and the detail pane (line step); scroll also works in the grid and
   the solo table.
5. **Click a span** in the trace tree to select it.
6. **Click the fold marker** (`▸`/`▾`) of a trace span to collapse /
   expand its subtree.
7. **Click in the editor** to move the text cursor to that cell.

## Architecture

### Decision: per-surface stashed geometry (not a hit-region list)

The renderer already follows a "stash geometry during `draw`, consume
in the key handler next frame" convention (`App.last_trace_body_height`,
`App.last_trace_detail_height`, `App.dashboard_scroll`). Mouse support
extends that convention with a small set of typed per-surface fields
rather than introducing a generic `Vec<(Rect, HitTarget)>` hit list.
Rationale: matches the existing idiom, fields are typed per pane, no
shared mutable vec to clear/rebuild each frame (which would make a
missed registration a silent click-through bug). A unified hit list
would only pay off past ~15 independently-clickable surfaces; we have
7–8, each with bespoke coordinate translation.

### New `App` fields (populated by `draw`, consumed by `on_mouse`)

- Pane outer rects for focus hit-testing: editor, legend, params,
  table/graph, dashboard, trace tree, trace detail.
- Editor: inner rect (post-border) + scroll `top` (first visible row).
- Grid: pane inner rect + `Vec<(chart_idx, Rect)>` of visible tiles.
- Trace tree: body rect + the scroll origin used that frame.
- Trace detail: inner rect.
- Topbar: rect + the end-x columns of the `QUERY` and `DASHBOARD`
  tab labels.

All stale-tolerant: rects are one frame behind, same trade-off as the
existing `last_trace_*` fields. Imperceptible at the 100ms poll.

### Dispatch

- `main.rs`: enable `EnableMouseCapture` on setup, `DisableMouseCapture`
  on teardown **and** in the panic hook (mirror the raw-mode / alt-screen
  restore). Match `Event::Mouse(m) => app.on_mouse(m)` in the loop.
- New `src/app/keys/mouse.rs`: `App::on_mouse(MouseEvent)`.
  - Gate on overlay visibility first, exactly like `on_key` (clicks
    consumed/ignored while help / picker / time / dashinfo /
    tile-inspect / history is up).
  - `Down(Left)` → hit-test stashed rects → focus pane / select tile /
    move editor cursor / switch view / toggle fold / select span.
  - `ScrollUp`/`ScrollDown` → route to the pane **under the pointer**
    (not the focused pane); no-op if over nothing recognized.
  - Drag / move / release ignored in v1.

## Scope

### Add

- Mouse capture enable/disable in `src/main.rs` (setup, teardown,
  panic hook) and `Event::Mouse` arm in the event loop.
- `src/app/keys/mouse.rs` — `App::on_mouse` + helpers.
- Per-surface geometry fields on `App` (listed above) with sane
  defaults in the constructor.
- Geometry stashing in `src/ui/mod.rs`, `src/ui/grid.rs`,
  `src/ui/trace.rs`, `src/ui/editor.rs`, `src/ui/topbar.rs`.
- A pure `screen_to_buffer(inner, top, col, row) -> (row, col)`
  helper for the editor, unit-tested without `App`.

### Edge cases the implementation must handle (each gets a test)

1. Click on a pane border (outer rect, not inner) → focuses pane,
   does not translate to content coordinates.
2. Click using stale geometry from a different `view_mode` → no-op
   (gate hit-tests by current `view_mode`).
3. Scroll while `trace_view.is_none()` → no-op.
4. Click in grid while `loaded_dashboard.is_none()` → no-op.
5. Click topbar `QUERY`/`DASHBOARD` honours `set_focus` validation
   (e.g. dashboard tab with nothing loaded → status message, no panic).
6. Editor click past end-of-line / past last row → clamps to a valid
   `(row, col)`.
7. Trace fold-marker click vs row-body click discriminated by the
   marker column band (`body.x + depth*2 .. +2`).

### Keep simple

- No drag-and-drop tile reordering, no right-click menus, no
  scroll-to-zoom in charts. All backlog.
- Topbar gets **no new TRACE tab** in this step (open question; trace
  is still entered via `:trace`). The existing two tabs become
  clickable only.
- Editor click keeps the existing ASCII char-width ≈ display-width
  assumption; Unicode metric names drift consistently, not worth
  fixing now.

## Tasks (incremental, reviewable units)

1. **Foundation + click-to-focus + topbar tabs** (features 2, 3).
   Capture enable/disable, `on_mouse` skeleton, stash pane rects +
   topbar tab x-ranges, `Down(Left)` → `set_focus` / topbar switch.
2. **Grid tile click + grid scroll** (feature 1, part of 4). Stash
   `(idx, rect)` in the grid render loop; click → `set_focused_chart`;
   wheel → `dashboard_scroll`.
3. **Trace mouse** (features 4, 5, 6). Stash tree body rect + scroll
   and detail inner rect; click → select span / toggle fold; wheel →
   `move_trace_cursor` / `scroll_trace_detail`.
4. **Editor click-to-position cursor** (feature 7). Stash inner rect
   + `top`; `screen_to_buffer`; `CursorMove::Jump`.

## Acceptance criteria

- All 7 features work against a real terminal.
- Mouse is additive: every existing keybinding still passes its tests.
- Overlays swallow mouse clicks (no click-through).
- Scroll routes to the pane under the pointer.
- No new clippy warnings; `cargo fmt` clean; `cargo llvm-cov` green
  with new code covered.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo llvm-cov`
- Manual against a real terminal: click each pane, each topbar tab,
  grid tiles, trace spans + fold markers, editor cells; scroll in
  trace tree / detail / grid / table.

## Files touched

- `src/main.rs` — capture enable/disable, `Event::Mouse` arm.
- `src/app/mod.rs` — new geometry fields + defaults.
- `src/app/keys/mod.rs` — wire the `mouse` submodule.
- `src/app/keys/mouse.rs` — `App::on_mouse` (new).
- `src/ui/mod.rs`, `src/ui/grid.rs`, `src/ui/trace.rs`,
  `src/ui/editor.rs`, `src/ui/topbar.rs` — geometry stashing.
- `docs/keys.md` — mouse reference (if a keymap doc section fits).

## Out of scope

- Drag-to-reorder tiles, resize-by-drag.
- Right-click context menus.
- A clickable TRACE tab in the topbar (pending product decision).
- Text selection by mouse drag in the editor.

## Confirmed (during implementation)

* **Per-surface geometry landed as `MouseGeometry`** (one grouped
  struct on `App.mouse_geom`) rather than scattered `last_*` fields —
  same convention, less field noise. Rects default to the zero rect so
  the mouse is inert before the first `draw`.
* **Topbar tabs map to existing view commands**: `QUERY` → `cmd_solo`,
  `DASHBOARD` → `cmd_grid`. No TRACE tab added (the `buffer_mode` badge
  has no clean toggle back to Mpl, and a trace tab needs a loaded
  trace — deferred pending a product decision).
* **Grid + editor scroll-wheel deliberately omitted.** Both derive
  their scroll offset from the current selection / cursor (the renderer
  snaps the viewport to keep it visible), so a free wheel scroll would
  fight that snap-back. Scroll is wired for the trace tree, trace
  detail, and the solo table, which have clean per-pane scroll.
* **Trace wheel steps the cursor** (`move_trace_cursor`) because the
  trace model has no independent viewport offset — `scroll` is derived
  from the cursor each frame.
* **Fold-marker hit band** is `[depth*2, depth*2+2)` from the tree body
  edge: `tree_guides` lays 2 display cells per depth level and the
  `▸`/`▾` glyph occupies the next 2. A click there toggles the fold;
  elsewhere on the row it only selects.
* **mpl now a git dependency.** `../mpl` was a local path dep and was
  mid-refactor (didn't compile). Switched `Cargo.toml` to pin both
  `mpl-lang` and `mpl-language-server` to git `tag = v0.5.5`
  (`mpl-language-server` isn't on crates.io and only exists in the repo
  from v0.5.5), so the build no longer depends on the local working
  tree.
* **Test-config isolation.** `test_app()` read the developer's real
  `~/.axiom.toml` through every client-building path; a multi-deployment
  config with no `active_deployments` made `Config::select` fail and
  broke 9 unrelated tests. Added an `App.config_override` seam
  (`resolve_config()`) and a synthetic single-deployment config in the
  harness — mirrors the existing in-memory cache/history/settings
  isolation. Production is unchanged (`config_override` is always
  `None`).
* **Verification green.** `cargo fmt` + `cargo clippy --all-targets`
  clean; `cargo llvm-cov`: 922 unit tests + 1 integration test pass.
  New-code coverage: `src/app/keys/mouse.rs` 95% lines / 95% regions;
  `src/app/keys/trace.rs` 92%. Remaining gaps are pre-existing
  render-path (`ui/*`) lines the suite doesn't drive via `TestBackend`.
