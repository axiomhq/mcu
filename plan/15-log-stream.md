# Step 15 — Log stream

## Incremental outcome

A live-tail log view backed by the events endpoint. This is the biggest
single step in the visualisation series — it adds a new query shape, a
poll loop, scrollback, and follow-mode.

## User-visible improvement

- `// @viz log_stream interval=5s rows=2000` runs an APL query that
  yields events and tails them.
- Auto-follow scrolls to the newest row; pressing `Ctrl-f` toggles
  follow off; `Ctrl-l` clears the buffer.
- Per-row syntax: timestamp dim, severity colour-coded, message
  highlighted; columns selectable via the legend (which becomes a
  "fields" picker for this viz).

## Dashboard compatibility

The tail loop runs **per tile**, not globally. A multi-tile dashboard
may have several `log_stream` tiles concurrently; each owns its own
ring buffer, cursor, and poll cadence. The async task is spawned when
the tile is materialised and cancelled when the tile is removed, its
kind changes, or the dashboard is closed.

Follow-mode is per-tile UI state; switching focus between tiles in the
grid (step 18) does not reset it.

## Scope

### Add

- `src/axiom_events.rs`:
  - `query_events(apl, start, end, cursor) -> EventsPage`.
  - `EventsPage { rows: Vec<EventRow>, next_cursor: Option<String> }`.
  - Decode the APL events response shape (matches/_time/_sysTime/+
    custom fields). Document the exact endpoint after empirical
    verification: candidates are `POST /v1/datasets/{ds}/query` with
    `Accept: application/json` and APL body, or `_apl` with a
    `streaming=true` flag — settle in this step.
- `src/viz/log_stream.rs`:
  - `RingBuffer<EventRow>` capped at `opts.rows` (default 2000) — old
    entries drop off the front.
  - Render: each row = `HH:MM:SS.fff  LEVEL  message`, with optional
    pinned columns rendered before `message`.
  - Scroll state: `follow: bool`, `offset: usize` (from tail).
- Async tick: a tokio task that fires `query_events` every
  `interval` (default 5s) with `start = last_seen_time`. Backoff +
  cancel on viz change.

### Keep simple

- No regex highlight inside messages (yet).
- No virtual scrolling tricks; the ring is small enough that
  re-rendering all visible rows each tick is fine.
- Time window: follow mode uses `now-interval` → `now`; manual scroll
  fetches an older `[t1, t2]` page on demand via the same cursor flow.

## Data model

```rust
pub struct EventRow {
    pub time: i64,                 // unix millis
    pub level: Option<Level>,      // parsed from common fields
    pub message: String,           // best-effort: `message`/`msg`/`event`
    pub fields: BTreeMap<String, serde_json::Value>,
}

pub enum Level { Trace, Debug, Info, Warn, Error, Fatal }

pub struct LogStreamOpts {
    pub interval: Duration,        // default 5s
    pub rows: usize,                // default 2000
    pub columns: Vec<String>,       // pinned fields (default empty)
    pub level_field: String,        // default "level"
    pub message_field: String,      // default "message"
}
```

## Tasks

1. Pin down endpoint + auth headers; commit a sample response under
   `tests/fixtures/events_page.json`; write decode tests.
2. Async tick wiring keyed by `TileId`: spawn when a tile with kind
   `log_stream` is mounted, cancel + drain on dismount / kind change.
   `App` owns the `JoinHandle` map; results are posted back to the
   tile's `TileState`.
3. Renderer:
   - Bottom-anchored when `follow=true`; clamp offset to `len -
     visible` when paginating.
   - Level colours: Trace dim, Debug grey, Info cyan, Warn yellow,
     Error red, Fatal magenta-bold.
   - Field pinning: legend lists known fields (union seen so far);
     Space toggles a field into the pinned-columns prefix; Enter
     focuses to inspect the row.
4. Inspector overlay (`Enter`): full JSON of the focused row,
   reuse the help-modal chrome.
5. `Ctrl-f` / `Ctrl-l` / `gg` / `G` bindings only active when viz is
   `log_stream` and the legend is unfocused.
6. Backpressure: drop incoming rows when the ring is full and the user
   has scrolled out of follow mode; surface a `… N rows hidden` banner.

## Acceptance criteria

- `// @viz log_stream` on a dataset like `logs | where level == "error"`
  shows live errors; new rows appear within ≤ `interval`.
- Disabling follow keeps the viewport stable as new rows stream in.
- Switching viz back to `table` stops the poll task within one tick.
- Buffer never grows beyond `rows`.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`
- Decode tests on the captured fixture.
- Manual: open a high-rate logs query, watch for missed rows
  (`next_cursor` continuity), Ctrl-f toggling, level colours.
