# Step 12 — Statistic + Top list

## Incremental outcome

Two new kinds that aggregate the same `Vec<Series>` the time-series chart
already consumes, so no backend changes are needed:

- `statistic` — one big number per series with optional delta and
  sparkline (Axiom's "Stat" tile).
- `top_list` — sorted horizontal bars over an aggregated value per
  series (Axiom's "Top list").

## User-visible improvement

- `// @viz statistic agg=last unit=ms` renders the latest value of each
  visible series as a large glyph, with the comparison-window delta in
  green/red and a single-row sparkline underneath.
- `// @viz top_list n=10 agg=avg` renders sorted bars with labels and
  numeric values right-aligned.

## Dashboard compatibility

Both kinds read from a tile's series result and write nothing back to
shared `App` state. The optional comparison query (`compare=true`)
fires against the *tile's* effective time range —
`tile.time_override.unwrap_or(dashboard.time_range)` shifted by the
window length. Cache is keyed by `TileId`. No assumptions about how
many tiles are open.

## Scope

### Add

- `src/viz/stat.rs` and `src/viz/top_list.rs` (or sibling modules; the
  `src/viz.rs` file from step 11 becomes a module dir).
- `series_agg.rs` (or section in `chart.rs`) with `last / first / avg /
  sum / min / max / count` over `Vec<Option<f64>>` style data.
- Glyph table for statistic digits: 5-row ASCII (or Unicode block)
  digits, monospace.
- A second query call when `compare=Δ` is set, fired against the
  preceding equal-length window, for the delta + sparkline baseline.
  Skip when `compare` is unset.

### Keep simple

- Statistic shows only the first visible series when multiple are
  present; document this. Multi-stat tiles can come later via a
  layout step.
- Sparkline uses braille on a fixed inner row; no axis.
- Top-list bar width = pane width minus label + value columns; no
  log scale, no animation.

## Data model

```rust
pub enum Agg { Last, First, Avg, Sum, Min, Max, Count }

impl Agg {
    pub fn apply(self, points: &[(f64, f64)]) -> Option<f64>;
    pub fn parse(s: &str) -> Option<Self>;
}

pub struct StatOpts {
    pub agg: Agg,            // default Last
    pub unit: Option<String>,
    pub compare: bool,       // default false
    pub decimals: u8,        // default 2
}

pub struct TopListOpts {
    pub agg: Agg,            // default Avg
    pub n: usize,            // default 10
    pub ascending: bool,     // default false (largest first)
}
```

## Tasks

1. Add `Agg` + `apply` + tests (NaN/None handling, empty series → `None`).
2. Statistic renderer:
   - Headline: aggregated value + unit, centered, glyph height = min(5,
     pane.height − 2).
   - Subline: delta vs prior window (green ▲ / red ▼ / dim ●) when
     `compare=true`.
   - Sparkline row beneath using braille over the current window.
3. Top-list renderer:
   - For each visible series, compute `Agg::apply`; sort desc; truncate
     to `n`.
   - Row: `[colored ▇▇▇▇▇▇░░ label ······ 42.0]` with bar length scaled
     to max value in the visible set.
4. Hook the comparison query path in the per-tile query runner — issue
   a second request shifted by `(end - start)` only when the focused
   tile's viz needs it. Cache the prior-window response per `TileId` so
   toggling `compare` doesn't re-fire and so multi-tile dashboards
   (step 18) don't trample each other.
5. Surface controls in the focused tile's legend pane: for `top_list`
   the legend shows the same sorted ranking; for `statistic` the
   legend is hidden. Legend state lives on `TileState`, not `App`.

## Acceptance criteria

- `// @viz statistic` on a typical 1-metric query shows the latest value
  centered, no delta, no errors.
- Adding `compare=true` populates the prior-window comparison without
  hanging the main query (concurrent, cancellable).
- `// @viz top_list n=5 agg=sum` produces 5 sorted bars with the
  largest on top; legend selection highlights the matching row.
- Empty/all-None series → "no data" placeholder, no panic.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`
- Property test: `Agg::apply` ignores NaN/Inf, matches reference values
  on hand-picked vectors.
- Manual: switch a multi-series query between `top_list` and `line`,
  confirm ranking matches the values shown in the line chart at `now`.
