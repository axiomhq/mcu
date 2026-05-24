# Step 03 — Chart data model

## Incremental outcome

The chart uses the same internal `Series` model that real query results will use later. The app still
uses local demo data and remains fully runnable offline.

## User-visible improvement

- Legend and graph are driven by shared series data.
- Empty data is handled cleanly with a placeholder message.
- Axis bounds are computed dynamically from data.

## Scope

### Add

- `chart.rs` for series model, bounds calculation, graph rendering, and legend rendering.
- Unit tests for chart bounds.

### Keep simple

- Continue using generated sine/cosine or similar local data.
- No API response decoding yet.

## Data model

```rust
struct Series {
    name: String,
    tags: Vec<(String, String)>,
    points: Vec<(f64, f64)>, // unix timestamp seconds or demo x value, y value
    color: Color,
}
```

## Tasks

1. Move demo data into `Vec<Series>` in `App`.
2. Convert `Series` values into ratatui `Dataset`s for rendering.
3. Implement bounds helpers:
   - empty input returns a safe default,
   - constant values get padding,
   - multiple series combine into one x/y domain.
4. Render graph empty state when there are no points.
5. Render legend entries from `Series`, using matching colors.

## Acceptance criteria

- Demo chart looks at least as good as before.
- Legend entries come from `App.series`.
- Clearing series or points does not panic.
- Bounds tests cover empty, constant, and multi-series data.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Manual run: verify chart and legend render correctly.
