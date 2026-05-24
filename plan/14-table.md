# Step 14 — Table

## Incremental outcome

A tabular result shape, sourced from the same `/v1/query/_mpl` endpoint
plus (optionally) `/v1/datasets/_apl` for non-metric tabular queries.
Adds the first non-series internal data structure: `TableResult`.

## User-visible improvement

- `// @viz table` renders columnar query output as a scrollable table
  with column headers, right-aligned numeric cells, left-aligned text.
- Keyboard scrolling (j/k for rows, h/l for column window when the
  table is wider than the pane).
- Column sort via `:sort <col>` or by clicking the legend pane (which
  becomes a column list when the table viz is active).

## Dashboard compatibility

`TableResult` is a new variant of the per-tile result, alongside the
existing series shape:

```rust
pub enum TileResult { Series(Vec<Series>), Table(TableResult), … }
```

The APL client method is generic over query body and is shared with
step 15 (events). Routing between MPL/APL endpoints is decided by the
tile's `Query` variant, not by sniffing the buffer text.

## Scope

### Add

- `src/axiom_apl.rs` — new client method `query_apl_table(apl, start,
  end)` hitting `POST {edge}/v1/datasets/_apl` with body
  `{"apl": "...", "startTime": "...", "endTime": "..."}` and
  `Accept: application/json`. Decodes the `tables[0]` shape Axiom
  returns for tabular APL.
- `src/result.rs` (or extend `axiom.rs`) with:
  ```rust
  pub struct TableResult {
      pub columns: Vec<Column>,
      pub rows: Vec<Vec<Cell>>,
  }
  pub struct Column { pub name: String, pub ty: ColType }
  pub enum ColType { Int, Float, String, Time, Bool }
  pub enum Cell    { Null, Int(i64), Float(f64), Str(String), Time(i64), Bool(bool) }
  ```
- `src/viz/table.rs` rendering via `ratatui::widgets::Table` with
  `state: TableState`. Header style mirrors the legend chrome.
- Auto-pick between the metrics and APL endpoints based on the parsed
  query (metrics MPL has the `dataset:metric` prefix; APL doesn't).
  The pragma can force one via `// @viz table source=apl`.

### Keep simple

- Numeric formatting reuses `chart::format_label`.
- Column widths: greedy fit by max content width, capped per column;
  overflow → ellipsis. Manual width override via `:tcol <i> <width>`
  only if it falls out of testing — not required for acceptance.
- Sort is in-memory after fetch; no server-side ordering control.

## Tasks

1. Implement APL client method + tests against a captured sample
   response. Mock via a small `serde_json::from_str` round-trip case.
2. Result type + cell formatters; `Display` on `Cell` for tests.
3. Table renderer:
   - Build `Row` per data row, styled by `ColType`.
   - Scroll state on `TileState` (`table_row_offset`, `table_col_offset`).
   - Hook j/k/h/l when the editor isn't focused, viz is `table`, and
     the focused tile is this one.
4. Legend repurposed (per focused tile): when viz is `table`, list
   columns; Space toggles visibility; Enter sorts (toggles asc/desc).
5. Wire the dispatch in the per-tile query runner: pick metrics-MPL vs
   APL from `tile.query`, then store `TileResult::Series(...)` or
   `TileResult::Table(...)` on the tile and have `viz::draw` consume
   the appropriate variant.

## Acceptance criteria

- `// @viz table` on a metrics query that emits a single series shows
  one column per tag plus a `value` column, one row per series.
- `// @viz table` on an APL query like `dataset | summarize count() by
  bin_auto(_time), level` renders the bucketed counts.
- Scroll keys work, header stays pinned, errors fall back to the same
  red overlay as today.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`
- Decode tests for an APL response sample (committed under
  `tests/fixtures/apl_table.json`).
- Manual: switch a known multi-series query to `table`, verify the row
  count matches the legend entry count, sort by `value` asc/desc.
