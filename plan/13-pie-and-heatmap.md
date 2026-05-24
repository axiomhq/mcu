# Step 13 — Pie + Heatmap

## Incremental outcome

Two more series-driven kinds — both visually unconventional in a
terminal, so each ships with an MVP that prioritises legibility over
fidelity to the web UI.

- `pie`     — share-of-total per series. MVP renders a legend of
              percentage bars; an optional braille donut is a follow-up.
- `heatmap` — 2D grid coloured by value. Uses truecolor backgrounds;
              degrades to 256-colour blocks when truecolor is missing.

## User-visible improvement

- `// @viz pie agg=sum` shows each series as a coloured row with its
  share, sorted desc, plus an explicit total.
- `// @viz heatmap x_bins=60 y_bins=24` produces a 2D heat grid; useful
  for "latency over time" or "errors per service per minute".

## Dashboard compatibility

Both kinds are pure renderers over the focused tile's series result.
No new shared state. The legend pane toggles read/write `TileState`
for the focused tile only.

## Scope

### Add

- `src/viz/pie.rs`:
  - Reduce each visible series with the configured `Agg`.
  - Compute share (`value / total`), sort desc.
  - Render rows: `▇▇▇▇░░░░░░  41.2%  service=api`.
  - Optional braille donut behind feature flag `pie_donut`. Wire the
    flag but ship it disabled; this is the experimental visual.
- `src/viz/heatmap.rs`:
  - Input shape: an `(x_bin, y_bin)` matrix of `Option<f64>` derived
    from the active series. Two ways to source it:
    1. Series with a `bucket` tag → that tag becomes the y axis.
    2. APL queries returning `time, bucket, count` columns (uses the
       table query path from step 14 when ready). For this step,
       require option 1 and document it.
  - Custom widget: paint `Buffer::cell_mut(x, y)` with `bg` set from
    a value→color map (viridis-like 5–8 stops, hand-coded RGB).
  - Colour-bar legend on the right edge with min/max numeric labels.

### Keep simple

- Pie has no labels-on-slices, no exploded slices, no donut hole text.
- Heatmap interpolation = nearest bin; no smoothing.
- Truecolor detection: probe `COLORTERM=truecolor|24bit`. Otherwise
  bucket the value into 8 indexed colours.

## Data model

```rust
pub struct PieOpts { pub agg: Agg /* default Sum */, pub donut: bool }

pub struct HeatmapOpts {
    pub x_bins: usize,        // default 60
    pub y_bins: usize,        // default = pane height − 2
    pub by_tag: Option<String>, // tag whose values are the y axis
    pub scale: Scale,         // Linear | Log10
    pub palette: Palette,     // Viridis (default) | Magma | Mono
}

fn viridis(t: f64) -> ratatui::style::Color; // t in [0,1]
```

## Tasks

1. Pie MVP rows (no donut), respecting hidden-series toggles from the
   legend.
2. Heatmap binning helper: given `Vec<Series>` + `by_tag`, produce a
   `Vec<Vec<Option<f64>>>` of shape `[y_bins][x_bins]`.
3. Custom heatmap widget — implement `Widget for Heatmap<'a>`; write
   per-cell `bg` via `Buffer::cell_mut`. Account for double-cell
   characters; use a single space so width = 1 column = 1 bin (or
   two spaces when option `cell_width=2`).
4. Colour-bar legend on right margin (4–6 cols): coloured column +
   `min`, `mid`, `max` labels matching `x_axis_labels`-style formatting.
5. Truecolor probe in `src/term.rs` (new tiny module) consulted by the
   palette functions.
6. Tests:
   - Pie shares sum to 100 ± rounding error.
   - Heatmap binning handles empty series, single point, NaN.
   - Palette is monotonic in `t`.

## Acceptance criteria

- `// @viz pie agg=sum` on a multi-series query: rows sorted desc,
  percentages add to ~100, colours match the time-series legend so
  switching kinds preserves identity.
- `// @viz heatmap by_tag=room` on the `temp` example: cells coloured;
  legend correct; resizing the pane re-bins x without redrawing
  garbage.
- Terminals without truecolor still render a recognisable heatmap.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`
- Manual: open `home:temp | …` with `by_tag=room`, check the heatmap
  cells across a 24h window; switch palettes via `:viz heatmap
  palette=mono`.
- Manual on a non-truecolor terminal (e.g. `TERM=xterm-256color
  COLORTERM=` ) — confirm fallback palette.
