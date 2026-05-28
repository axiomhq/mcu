# Step 26 — Codebase review remediation (bugs, waste, duplication, maintainability)

## Status

**Pending.** Independent of the feature ladder (steps 01–25). Each
phase below is self-contained and leaves the app buildable with tests
green, so they can land as separate commits in priority order.

## Incremental outcome

Closes out the findings from the full-codebase review (turn after
step 24). Three reachable panics that crash the whole TUI are fixed
first, then a set of state-desync / correctness bugs, then the
per-frame render waste, then the systemic duplication, then the
documentation/dead-code maintainability items. No user-facing feature
changes — the app behaves the same, minus the crashes and minus the
wrong-cursor / stale-state surprises.

## Source

Findings came from a 21-reviewer parallel pass over all 70 non-test
source files, each finding adversarially verified (3 skeptics per
BUG, 1 per other), then re-validated by hand. 106 confirmed findings;
10 rejected as false (kept out of this plan deliberately — see
"Explicitly NOT doing" below). Severity tags: P0 = crashes today,
P1 = wrong behavior / stale state, P2 = waste, P3 = duplication,
P4 = maintainability.

---

## Phase A — Crash fixes (P0)

Three reachable panics + the shared helper that kills a whole class.

### A1. Char-safe string truncation helper (kills ~6 panic sites)

Fixed-byte-offset `&str` slicing panics when the offset isn't a UTF-8
char boundary. Reproduced: `:trace aＡＡＡＡＡＡＡＡＡ` →
`end byte index 12 is not a char boundary`.

- **Add** `pub fn truncate_chars(s: &str, n: usize) -> String` (and/or
  `fn ellipsize(s: &str, n: usize) -> String` that appends `…`/`...`
  when truncated) to `src/util.rs`. Char-based threshold, char-based
  take.
- **Replace** the byte slices at:
  - `src/app/fetch/trace.rs:301-307` — `short_trace_label` `&id[..12]`.
    Reachable from `:trace <id>` (only `.trim()`, no validation) and
    fires in `dispatch_trace_window` **before any network call**.
  - `src/app/keys/trace.rs:592-598` — `short_id` (duplicate).
  - `src/ui/trace.rs:815-821` — `short_id` (duplicate). Collapse all
    three into one helper while here (see D5).
  - `src/app/file_io.rs:66` — `&body[..body.len().min(1024)]` in
    `looks_like_dashboard_file`; use `body.get(..1024).unwrap_or(body)`
    or a char-boundary walk.
  - `src/ui/overlays.rs:226-227` — `&b.id[..11]` in `:dashinfo`
    (server-supplied id). Also fix the `{:<14}` column overflow
    (11 + 3 = 14) by taking 9–10 chars.
  - `src/mpl.rs:456-457` — `byte_offset_to_line_col` non-boundary
    slice (latent; guard with `is_char_boundary` or clamp).

### A2. Top-list inverted `clamp` panic

- `src/viz/top_list.rs:91` — `label_w … .clamp(4, inner.width / 3)`.
  Only guard above is `inner.width == 0` (line 68); widths 1–11 give
  `clamp(4, <4)` → `u16::clamp` panics (`min > max`). **Fix:**
  `.clamp(4, (inner.width / 3).max(4))` (the trailing `.max(4)` then
  becomes redundant — remove it).

### A3. Grid `row_tops` out-of-bounds panic on a partial layout

- `src/ui/grid.rs:148-149` and `:165-166` — `virt_rows` is derived
  only from `layout` entries (grid.rs:106-111), but `resolve_slot`
  (grid.rs:285-301) gives any chart **missing** from `layout` an
  auto-stack slot at `gy = (idx/2)*6`. A server dashboard whose
  `layout` doesn't reference every chart (no normalization in
  `adopt_dashboard`) makes `gy + gh > virt_rows` → `row_tops[...]`
  panics. **Fix:** fold the auto-stack fallback into the `virt_rows`
  max (preferred — keeps coordinates correct), and defensively clamp
  the four index sites to `row_tops.len() - 1`.

### Phase A tasks

1. Add `util::truncate_chars` / `util::ellipsize` + unit tests
   (ASCII, exact-boundary, multibyte-straddle, shorter-than-n).
2. Route all six byte-slice sites through it.
3. Fix the top-list clamp and add a render test at `inner.width` ∈
   {1, 4, 11, 12}.
4. Fix grid `virt_rows`/clamp and add a test: dashboard with a
   non-empty `layout` that omits a chart renders without panic.

### Phase A acceptance

- `cargo test` green; new regression test per fix.
- Manual: `:trace <multibyte>`; a top-list tile shrunk to 1 cell;
  load a dashboard whose `layout` omits a chart.

---

## Phase B — State-desync & correctness (P1)

### B1. Failed trace dispatch leaves stale `pending_trace_fetch`

- `src/app/fetch/trace.rs:107-108` — `pending_trace_fetch = Some(..)`
  then `dispatch_trace_window()?` early-returns on error (missing
  `~/.axiom.toml`, bad deployment) without clearing it; a later `Esc`
  reports "trace fetch cancelled" for a fetch that never started.
  **Fix:** `if let Err(e) = self.dispatch_trace_window() { self.pending_trace_fetch = None; return Err(e); }`.

### B2. Opening a dashboard never clears `dashboard_dirty`

- `src/app/file_io.rs:33-44` — the dashboard branch sets
  `buffer_mode`/`current_file`/`saved_buffer` but not
  `dashboard_dirty` (and `adopt_dashboard` doesn't either). After a
  dirty session, `:e! clean.axiom.json` still reports dirty. **Fix:**
  `self.dashboard_dirty = false;` in that branch (mirror the write
  path at file_io.rs:131).

### B3. `Nr<Enter>` inserts N newlines instead of one

- `src/app/editing.rs:138` — `replace_chars` builds
  `repeat_n(ch, count)` for every char including `'\n'`, so `3r<CR>`
  inserts three line breaks; vim collapses to one (the comment at
  editing.rs:132 even claims it does). **Fix:** special-case
  `ch == '\n'` to a single `"\n"`.

### B4. Empty-buffer Backspace-cancel leaks cmdline focus

- `src/app/keys/cmdline.rs:78` — this arm only sets
  `self.mode = Mode::Normal`, unlike the `Esc` path (cmdline.rs:46-51)
  which also `reset()`s, `completions.hide()`s, and
  `restore_cmdline_focus()`s. When the cmdline was opened via
  `prefill_command` (params `a`/`i`/`e`), focus is never restored.
  **Fix:** mirror the `Esc` arm.

### B5. `splice_editor_range` deletes the wrong length across lines

- `src/app/completions_impl.rs:213-222` — takes `(row, start_char)`
  from `range.0` and `(_, end_char)` from `range.1`, **discarding the
  end row**, then `delete_str(end_char - start_char)`. For a
  multi-line quickfix/completion replacement that char delta is
  meaningless. Safe only while every action is single-line. **Fix:**
  compute the true char count between the two byte offsets (count
  chars in `query[range.0..range.1]`), then delete that.

### B6. Discovery fetch has no timeout → `busy` stuck forever

- `src/app/fetch/discovery.rs:33-64` — foreground
  `fetch_metrics_for_current_query` calls `client.list_metrics(...)`
  unbounded (unlike `run_query_task`/`run_apl_query_task`, which wrap
  `tokio::time::timeout(QUERY_TIMEOUT, …)`). A hung edge leaves
  `self.busy = true` forever. **Fix:** wrap the call (ideally the
  whole route-resolve + retry loop) in `QUERY_TIMEOUT`; surface a
  timeout error into `MetricsFetched` so the handler clears `busy`.

### B7. `parse_timespan_ns` drops minute/hour durations (latent)

- `src/viz/apl_decode.rs:808-828` — handles only `µs/us/ns/ms/s`.
  Go's `Duration.String()` (which the Axiom `duration` column
  appears to use — see `277.731738ms` in the corpus) emits
  `1m0s`/`1h2m3s` for spans ≥ 60 s; `strip_suffix('s')` leaves those
  unparseable → `0` → zero-width waterfall bar. **Not** in the
  current 40-fixture corpus (verified: all are sub-second), so this
  is hardening, not a live failure. **Fix:** parse the compound
  grammar — accumulate leading-decimal + unit segments
  (`h/m/s/ms/us/µs/ns`) and sum each × its ns scale. Add fixtures:
  `"1m0s"`, `"1h2m3.5s"`, `"90s"`.

### B8. Stale tile data left visible after an APL decode error

- `src/app/fetch/mod.rs:281-294` — an APL non-table decode error
  leaves the previous `entry.table`/`table_result`/`series` on the
  tile, so the user sees stale data under a fresh error. **Fix:**
  clear the cached result fields when recording the decode error.
- Related, lower urgency: `src/app/fetch/query.rs:44-69`
  (`run_focused_tile_query` reuses the busy slot without epoch
  protection on the MPL parse-error path) — note for follow-up; a
  full epoch guard is larger than this step. Document, don't
  necessarily fix here.

### B9. `parse_range` accepts inverted ranges

- `src/axiom.rs:369-377` — no `start <= end` check; an inverted
  `:time` range is sent verbatim. **Fix:** return a clear error when
  `s > e` (cheap, prevents a confusing empty server response).

### Phase B tasks

1. One small targeted fix per item above, each with a unit/behavior
   test (B1 stale-pending, B3 `3r<CR>` = one newline, B4 focus
   restored, B5 multi-line splice, B7 compound durations, B9 inverted
   range rejected).
2. B6: add a timeout test analogous to the existing query-timeout
   coverage.

### Phase B acceptance

- All new tests pass; existing 866 unit tests stay green.
- Manual: trace fetch with no `~/.axiom.toml`; `3r<CR>`; params
  `a` then empty Backspace; long span on the waterfall after B7.

---

## Phase C — Wasteful per-frame work (P2)

Theme: these run on **every draw** (per keystroke/tick), not per data
change. Fix by borrowing instead of cloning, or caching on change.

1. `src/ui/grid.rs:40-41` — deep-clones the whole `charts` + `layout`
   vecs every frame to drop a borrow. Restructure: compute the scroll
   target first, or scope the borrow, so no clone is needed.
2. `src/ui/editor.rs:36-49` — re-tokenizes + re-highlights the entire
   buffer every frame. Cache highlight spans, invalidate on edit.
3. `src/app/mod.rs:672-674` (via `src/ui/params.rs:28`) — params pane
   recompiles the whole MPL query every frame. Cache the compile.
4. `src/chart.rs:271-272` — `draw_graph` clones every visible
   `Series` (all points) just to compute bounds; compute bounds by
   reference. `src/chart.rs:328-418` / `:444` — `summarize_legend`
   rebuilt from scratch every frame (also `src/ui/grid.rs:495,589`).
5. `src/app/keys/trace.rs:454,479` — clone the entire per-span
   search-blob `Vec<String>` on every filter keystroke; index by
   reference.
6. `src/viz/table.rs:163-170,196` — formats every cell to a `String`
   twice per frame; `src/viz/table.rs:87-92` `series_to_table` does
   O(rows × tag_keys) linear `find` per cell.
7. `src/app/mod.rs:728-733` — `is_dirty()` re-joins the whole editor
   buffer every frame in MPL mode; compare without full join (e.g.
   track a dirty flag on edit like Dashboard mode does).
8. `src/app/types.rs:996-1019` — `DashboardPicker::refresh_items`
   rebuilds `filtered_indices` three times per call.
9. `src/completions.rs:253-261` — allocates a lowercased `String` per
   candidate every keystroke; lowercase the query once, or store
   pre-lowered candidate keys.
10. `src/ui/mod.rs:215` — `viz_opts` `BTreeMap` cloned every frame;
    borrow it. `src/ui/status.rs:23-35,121-142` — several owned
    `String`s allocated per frame.
11. `src/ui/trace.rs:584` (`plan_detail_rows` from `draw_detail`) —
    materializes all attribute/event rows every frame despite
    virtualizing the render; build only the visible window.

Lower-impact (note, batch if cheap): `apl_decode.rs:307-321`
per-row `key.clone()`; `apl_decode.rs:618-622` `sort_key` clones
`span_id` per comparison (use `sort_by_key` over indices into a
prebuilt key vec, or `sort_unstable_by` with a borrow);
`apl_decode.rs:402` repeated per-row type lookup; `fetch/trace.rs:214`
double clone of `trace_id`/`dataset`; `keys/overlays.rs:167`
redundant `uid` clone; `motion.rs:322-327` redundant rescan;
`unit.rs:548-562` computes canonical factors twice; `history.rs:157`
dead `removed_existing` computation; `tile_ops_shove.rs:230` clones
full `LayoutItem` per candidate; `file_io.rs:128` stores serialized
JSON into `saved_buffer` that's never read for dashboards.

### Phase C acceptance

- No behavior change; existing tests green. Where a perf smoke test
  exists (trace 1,498-span fixture), confirm per-frame budget holds
  or improves. `cargo clippy --all-targets` clean.

---

## Phase D — Duplication extraction (P3)

1. **`LayoutItem` plumbing** (biggest win):
   - `y.unwrap_or(0)` normalization is copy-pasted across **16 sites
     in 5 files** despite an existing `y_of` helper — route them all
     through `y_of` (`tile_ops_shove.rs`, `tile_layout.rs`, `ui/grid.rs`,
     `ex_cmds.rs`, `clipboard.rs`).
   - `layout.iter().find/position(|l| l.i == id)` slot lookup in **11
     sites** → a `fn slot_for<'a>(layout, id) -> Option<&'a LayoutItem>`.
   - AABB overlap implemented twice (`overlaps_any` in
     `tile_layout.rs:107-118` vs `rects_overlap` in
     `tile_ops_shove.rs:259-265`) → one helper.
   - `max(y + h)` virtual-row math in 3 places
     (`tile_ops_shove.rs:255`, `tile_layout.rs:205`, `ui/grid.rs:106`)
     → one helper (ties into A3).
2. **Focused dashboard chart access** — `loaded_dashboard…charts.get(
   selected_chart_idx)` open-coded in **8 sites**
   (`app/mod.rs`, `app/dashboard.rs`, `clipboard.rs`, `ex_cmds.rs`) →
   `App::focused_chart()` / `focused_chart_mut()`.
3. **Wrap-around index** `((i % n) + n) % n` in 3 selection movers +
   3 copies in `completions_impl.rs:61-77,183-191,224-232` → one
   `fn wrap_index(i: isize, n: usize) -> usize`.
4. **Atomic write reuse** — `src/app/file_io.rs:94-127` `write_file`
   reimplements `util::atomic::atomic_write_text` (extracted in
   `cc6b84b`); call the helper. Also fixes the misleading doc at
   file_io.rs:80 (the `.tmp` rename claim) for free.
5. **`short_id`/`short_trace_label`** — three copies (done in A1).
6. Smaller: per-renderer `opts.get("agg")…` parse across 4 viz tiles
   → shared helper; `discovery.rs:44` 404-retry loop copy-pasted 4×;
   etcetera config-path builder in `cache.rs`/`history.rs`/`settings.rs`;
   axiom-rs client-builder boilerplate (`axiom.rs:109,154`);
   `TraceFetchWindow::as_relative_start` ≡ `label` (`types.rs:474`);
   pie/top_list percentage-bar builder (`pie.rs:100`, `top_list.rs:106`);
   completion + quickfix picker-popup placement (`ui/popups.rs:21,98`);
   diagnostic count/pluralization (`status.rs:229`); path→filename
   label (`editor.rs:104`).

### Phase D acceptance

- Pure refactors; no behavior change; tests green; clippy clean. Each
  extracted helper gets at least one direct unit test where logic is
  non-trivial (wrap_index, AABB overlap, slot_for).

---

## Phase E — Maintainability (P4)

1. **Comment/doc drift:** `:refresh` (`ex_cmds.rs:123`) claims to
   re-run the query but only fetches datasets; `TileQueryResult.elapsed`
   (`types.rs:152`) doc says "wall-clock" but it's a monotonic
   `Instant` diff; `trace.rs:184` roots-sort doc omits the span_id
   tiebreak; `axiom.rs:348` `map_axiom_err` doc says callers match a
   message prefix but they downcast; `file_io.rs:58` var named `ext`
   holds the full file name; `ex_cmds.rs:62` stale doc on
   `parse_two_u32`/`parse_optional_count`; `command.rs:45` references
   nonexistent `Operator::Move`; `ui/grid.rs:697` `format_elapsed`
   "≤5 chars" invariant false for double-digit minutes/hours.
2. **Dead code:** `quit_after_save` modified-buffer guard never fires
   (`fetch/mod.rs:130-151`); dead `Char('\t')` arm in time-picker
   keymap (`keys/overlays.rs:105`); dead match arms in `unit.rs:222`;
   `History::push` dead branch (`history.rs:157`); unreachable
   `(0, max_y)` fallback already covered (rejected finding — skip).
3. **Scroll-to-selection:** params pane (`ui/params.rs:50-116`) and
   time picker (`ui/time_picker.rs:60-83`) let the selected row scroll
   out of view on short terminals — add scroll-to-selection.
4. **Durability:** `src/util/atomic.rs:23-38` doesn't `fsync` the
   parent directory after rename, weakening the crash-safety it
   documents — add a parent-dir `File::open(..).sync_all()` after the
   rename.
5. **Smaller:** detail-pane `model.tree[cursor]` lacks an empty-tree
   guard (`ui/trace.rs:582`) — defensible today (decoder rejects
   empty traces) but add a guard for robustness; `clipboard.rs:267`
   identical if/else branches; `hover.rs:103` backtick-ident skip
   ignores escaped backticks; `cmdline_complete.rs:100` `("trace", _)`
   arm forwards every trailing slot regardless of position;
   `:trace set KEY=` silently unsets instead of rejecting
   (`ex_cmds.rs:632`).

### Phase E acceptance

- Comments match code; no dead-code clippy warnings; params/time
  picker keep the selection visible on a 10-row terminal; `atomic`
  module fsyncs the dir. Tests green.

---

## Verification (every phase)

- `cargo fmt`
- `cargo clippy --all-targets` — no new warnings
- `cargo test` (and `cargo llvm-cov` for the touched modules)
- Manual smoke per phase as noted in the acceptance blocks.

## Files touched (by phase)

- **A:** `src/util.rs`, `src/app/fetch/trace.rs`, `src/app/keys/trace.rs`,
  `src/ui/trace.rs`, `src/app/file_io.rs`, `src/ui/overlays.rs`,
  `src/mpl.rs`, `src/viz/top_list.rs`, `src/ui/grid.rs`.
- **B:** `src/app/fetch/trace.rs`, `src/app/file_io.rs`,
  `src/app/editing.rs`, `src/app/keys/cmdline.rs`,
  `src/app/completions_impl.rs`, `src/app/fetch/discovery.rs`,
  `src/viz/apl_decode.rs`, `src/app/fetch/mod.rs`, `src/axiom.rs`.
- **C:** `src/ui/grid.rs`, `src/ui/editor.rs`, `src/ui/mod.rs`,
  `src/ui/status.rs`, `src/ui/trace.rs`, `src/ui/params.rs`,
  `src/chart.rs`, `src/viz/table.rs`, `src/app/mod.rs`,
  `src/app/types.rs`, `src/completions.rs`, `src/app/keys/trace.rs`.
- **D:** `src/app/tile_layout.rs`, `src/app/tile_ops_shove.rs`,
  `src/ui/grid.rs`, `src/app/clipboard.rs`, `src/app/mod.rs`,
  `src/app/dashboard.rs`, `src/app/ex_cmds.rs`,
  `src/app/completions_impl.rs`, `src/app/file_io.rs`, `src/viz/*`,
  `src/ui/popups.rs`, `src/cache.rs`, `src/history.rs`,
  `src/settings.rs`, `src/axiom.rs`, `src/app/types.rs`.
- **E:** docs/comments across the above + `src/util/atomic.rs`,
  `src/ui/params.rs`, `src/ui/time_picker.rs`, `src/unit.rs`,
  `src/command.rs`, `src/hover.rs`, `src/cmdline_complete.rs`.

## Explicitly NOT doing (verified-false findings)

The review's verification pass rejected 10 findings as false; keeping
them out on purpose so they don't get "re-fixed":

- `main.rs:55-67` terminal restore — `let result = run(...)` has **no
  `?`**; cleanup runs unconditionally before returning `result`.
- `ex_cmds.rs:1249` `:run dashboard` "1 tile(s)" — `run_tile_queries`
  repopulates `tile_results` **synchronously** before returning.
- `cache.rs:117` empty `fallback_base_url` — empty string is a
  deliberate sentinel handled in `edge_client`; never used as a raw
  URL.
- Plus 7 lower-severity MAINT/WASTE rejections (e.g.
  `tile_layout.rs:225` unreachable fallback, `motion.rs:307`
  back-step — re-examine only if a real bug surfaces).

## Out of scope

- Full per-tile fetch epoch/generation guard (B8 note) — larger
  redesign; track separately.
- Unicode display-width handling in the waterfall (`ui/trace.rs`
  assumes 1 cell/char) — rejected as low-value for v1; revisit if
  CJK span names show up.
