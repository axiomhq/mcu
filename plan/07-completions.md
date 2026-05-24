# Step 07 — Completions

## Incremental outcome

The editor can show a small completion popup with static MPL keywords and discovered Axiom names.
Query execution and charting remain unchanged.

## User-visible improvement

- `Ctrl-Space` shows completion suggestions.
- User can insert a selected completion.
- Dataset names from discovery are available as suggestions.

## Scope

### Add

- Completion state and popup rendering.
- Static keyword completions.
- Dataset completions from Step 04 discovery cache.

### Keep simple

- Do not duplicate private `mpl-lang` context-aware completion logic.
- Metric/tag completions are added only after endpoints are confirmed.

## Tasks

1. Add completion state:
   - visible/hidden,
   - items,
   - selected index.
2. Add `Ctrl-Space` in Insert mode to populate and show suggestions.
3. Static items:
   - `where`, `filter`, `map`, `align`, `group`, `bucket`, `compute`, `by`, `using`.
4. Add dataset names once fetched.
5. Popup controls:
   - `Up`/`Down` or `Ctrl-n`/`Ctrl-p`: selection,
   - `Enter`/`Tab`: insert selected item,
   - `Esc`: dismiss.
6. Render popup near editor area. Cursor-anchored positioning can come later if needed.

## Acceptance criteria

- `Ctrl-Space` opens a popup.
- Selection can move.
- Accepting a completion inserts text into the editor.
- Dismissing completion returns to normal editing.
- Query execution still works after using completions.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Manual run: trigger, navigate, accept, dismiss completions.

## Outcome (revised)

The initial "union of everything" approach was replaced with context-aware completions:

- `completions::detect_context(query, byte_offset)` classifies the cursor position from the bytes
  preceding the partial token, skipping strings and backticked identifiers. Cases handled:
  - `Dataset` — before the first top-level `:`
  - `Metric { dataset }` — after `:`, before the first `|`
  - `PipeOperator` — right after `|` (empty or partial operator)
  - `AlignFn` / `GroupFn` / `BucketFn` / `ComputeFn` — after `<op> ... using` on the current pipe stage
  - `MapFn` — after `map`
- `completions::items_for_context(ctx, cache)` draws candidates from:
  - `cache.dataset_names()` for `Dataset`
  - `cache.metric_names(dataset)` for `Metric`
  - `PIPE_OPERATORS` static list for `PipeOperator`
  - `mpl_lang::STDLIB` serialised through `serde_json` for align/map/group/bucket/compute functions
    (top-level entries + `prom::*` submodule entries; operator-style names filtered out)
- Matching is case-insensitive prefix-only and sorted alphabetically. No "contains" fuzz.
- The popup title now shows the category, e.g. `completions · metric`.

## Outcome

- Removed Tab as a panel-swap. There is no longer a focus ring; the editor is always the active
  pane and the chart/legend render in a dimmed border. If pane focus is needed later, use a Ctrl-W
  prefix.
- `src/completions.rs`: static MPL keyword list, `word_range_at` prefix detection, `items_for`
  merges keywords + cached datasets + cached metrics (for the dataset in the current MPL query) and
  ranks prefix matches before substring matches.
- Insert-mode keys:
  - `Tab` / `Ctrl-Space` opens the popup, or accepts the selected item when the popup is visible.
  - `Up`/`Down`, `Ctrl-P`/`Ctrl-N` navigate selection (wrapping).
  - `Enter` accepts when popup is visible, otherwise inserts a newline.
  - `Esc` dismisses popup first, then leaves Insert mode on a second press.
  - Typing while popup is visible refreshes the filtered list.
- Popup is rendered as a floating list anchored just below the editor cursor (flips above if it
  would overflow), with `Clear` to wipe the underlying editor cells.
