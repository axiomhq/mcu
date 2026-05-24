# Step 11 — Viz kinds + time-series variants

## Incremental outcome

The buffer declares which Axiom dashboard element it renders. The current
multi-series line chart becomes the `line` variant of a small `VizKind`
enum; `bar`, `area`, and `scatter` ship in the same step because they
reuse the existing series pipeline and ratatui's `Chart` widget.

This step also lays the **dashboard-shaped internal model** that every
later step builds on. Even though the user-visible experience is still
"one buffer, one chart", the in-memory representation is
`Dashboard { tiles: Vec<Tile> }` with exactly one tile in single-buffer
mode. Steps 17 and 18 then load real multi-tile dashboards into the
same structure without touching the renderers.

## User-visible improvement

- A header pragma — `// @viz <kind> [k=v ...]` — selects the element type.
- Ad-hoc switching via `:viz <kind>`; the pragma is rewritten on save.
- Status bar surfaces the active element (e.g. `line`, `bar`, `scatter`).
- Existing `.mpl` files keep rendering as line charts (default).

## Scope

### Add

- `src/viz.rs` with `VizKind` enum + `VizOpts` (BTreeMap) + pragma parser.
- `viz::draw` dispatch fn taking `(kind, &[Series], &VizOpts, area, …)`.
- New variants: `bar`, `area`, `scatter` implemented inside `chart.rs`
  by swapping `GraphType` and the marker per kind.
- `:viz` Ex-command.
- Tests: pragma round-trip; unknown kind → diagnostic; default = line.

### Keep simple

- Per-kind options are stored as untyped `String`s; typed accessors come
  on demand (`opts.usize("n")`, `opts.str("agg")`).
- No new endpoints. All four variants in this step are pure renderer
  changes on the existing `MetricsQueryResponse → Vec<Series>` pipeline.
- No legend changes (a follow-up step adds top-N / group-by controls
  for kinds that need them).

## Data model sketch

```rust
// src/viz.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VizKind {
    Line, Bar, Area, Scatter,
    // Reserved for later steps; parser already accepts them so files
    // authored ahead of implementation produce a clear "unsupported yet"
    // diagnostic instead of "unknown kind".
    Statistic, TopList, Pie, Heatmap, Table,
    LogStream, MonitorList, Note, Spacer,
}

pub struct VizSpec {
    pub kind: VizKind,
    pub opts: std::collections::BTreeMap<String, String>,
}

/// Parses the first `// @viz …` comment in the buffer. Returns `None`
/// when absent; `Err` only when the line is malformed (helps surface
/// typos as diagnostics rather than silently defaulting).
pub fn parse_pragma(src: &str) -> Result<Option<VizSpec>, PragmaError>;
```

```rust
// src/dashboard.rs — the canonical internal container.
// Step 11 only ever holds one tile; the shape exists now so later
// steps can load real dashboards without reworking core state.
pub type TileId = u32;

pub struct Dashboard {
    pub id: Option<String>,         // server id when loaded; None for local
    pub name: String,
    pub time_range: TimeRange,      // applies to all tiles by default
    pub variables: BTreeMap<String, String>, // dashboard-scoped params
    pub tiles: Vec<Tile>,
    pub layout: Layout,             // step 18 fills this in; default = full-screen
}

pub struct Tile {
    pub id: TileId,
    pub title: String,
    pub kind: VizKind,
    pub opts: BTreeMap<String, String>,
    pub query: Query,               // MPL/APL/Note body/Monitors filter/none
    pub time_override: Option<TimeRange>,
    pub pos: GridPos,               // ignored in solo mode; respected in step 18
}

pub enum Query {
    Mpl(String),
    Apl(String),                    // step 14+15
    Monitors(MonitorsFilter),       // step 16
    Note(String),                   // step 16 — markdown body
    Empty,                          // spacer
}

pub struct TimeRange { pub start: TimeExpr, pub end: TimeExpr }
pub enum TimeExpr { Relative(String), Absolute(i64) }
pub struct GridPos { pub x: u16, pub y: u16, pub w: u16, pub h: u16 }
pub struct Layout  { pub cols: u16, pub row_h: u16 }
```

### How the pragma maps onto this model

For `.mpl` files the on-disk source is still a single MPL query. On
open, we synthesise a one-tile `Dashboard`:

- `tiles[0].query  = Query::Mpl(buffer_text_without_pragma)`
- `tiles[0].kind   = pragma.kind` (or `Line` default)
- `tiles[0].opts   = pragma.opts`
- `dashboard.time_range` = today's implicit window, surfaced via
  `:range <start>..<end>` (defaults match current behaviour).
- `dashboard.variables` = the existing `:p NAME=VAL` params table.

On save we re-emit the pragma at the top of the buffer. This means
`.mpl` editing keeps today's feel while the runtime is already
dashboard-shaped.

## Tasks

1. Add `src/viz.rs` and `src/dashboard.rs`; wire both in `main.rs`.
2. Move `App.series`, `App.last_error`, `App.busy`, `App.legend_*`,
   `App.last_trace_id` onto a `TileState` map keyed by `TileId`. With
   only one tile today the diff is mechanical, but it unblocks per-tile
   state in every later step.
3. Parse the pragma on every buffer change; reconcile into the
   single-tile `Dashboard` and refresh the affected `TileState`.
4. Reroute `ui::draw` so the graph pane calls `viz::draw(tile, state,
   area, …)` instead of `chart::draw_graph` directly. The line case
   delegates to today's `draw_graph` unchanged.
5. Implement `bar`, `area`, `scatter` in `chart.rs`:
   - `bar`  → `GraphType::Bar` + `Marker::Bar`,
   - `area` → `GraphType::Line` filled (ratatui ≥ 0.29 `GraphType::Line`
     with `Style::default().bg(...)`; if filled-area is unsupported,
     stack faint braille fill under the line),
   - `scatter` → `GraphType::Scatter` + `Marker::Dot`.
6. `:viz <kind>` mutates `tiles[focused].kind`, then re-serialises the
   pragma on next save.
7. `:range <start>..<end>` sets `Dashboard.time_range` (used by the
   query runner in place of today's implicit window).
8. Status-line: show `· viz: <kind>` next to the trace id; in solo mode
   the focused tile is implicit, so no tile breadcrumb yet.
9. Tests:
   - `parse_pragma` handles missing / leading-whitespace / unknown-kind.
   - Pragma rewrite is idempotent.
   - `viz::draw` dispatches to the right branch for each kind.
   - Single-buffer open → save round-trips kind + opts.

## Acceptance criteria

- A file without a pragma renders identically to today.
- `// @viz bar` swaps the existing series to bars without altering data
  or axis bounds.
- `:viz scatter` → file gains `// @viz scatter` line; rerunning the
  query keeps the kind.
- Unknown kind shows a diagnostic anchored at the pragma line.
- Internal `Dashboard` always has exactly one tile in this step;
  serialising it to the dashboard JSON envelope (defined in step 17)
  and reloading produces an identical buffer.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`
- Manual: open `test.mpl`, cycle through `:viz line|bar|area|scatter`,
  confirm visuals; reopen, confirm persisted kind.
