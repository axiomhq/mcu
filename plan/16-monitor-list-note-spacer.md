# Step 16 — Monitor list + Note + Spacer

## Incremental outcome

The remaining three Axiom dashboard elements: one new backend (monitors)
and two static, data-free elements that exist mainly to enable layout
work later.

- `monitor_list` — list monitors with their current state.
- `note`        — markdown body (rendered subset).
- `spacer`      — empty pane; trivial.

## User-visible improvement

- `// @viz monitor_list` shows every monitor in the org with state
  (Firing / OK / No-data) and last-changed time; filter via
  `org=...` or `tag=...` options.
- `// @viz note` renders the buffer body (after the pragma) as
  styled markdown — headings, bold/italic, lists, inline code, fenced
  code blocks.
- `// @viz spacer` is a no-op tile (visible only as the bordered pane
  in the current layout; lays groundwork for grid layouts later).

## Dashboard compatibility

Note bodies are a **tile field**, not the editor buffer. For the
`.mpl` single-tile case this still feels like "the buffer renders as
markdown" because the buffer text becomes `Tile.query = Note(text)`.
For multi-tile dashboards (step 17+) the note body lives in the
dashboard JSON and is editable inline (step 18's tile inspector).

Monitor-list fetches are cached on the tile so several monitor tiles
in one dashboard don't issue duplicate `/v1/monitors` requests within
a configurable TTL.

## Scope

### Add

- `src/axiom_monitors.rs`:
  - `list_monitors() -> Vec<MonitorSummary>` via `GET /v1/monitors`.
  - Confirm endpoint path + response shape (Axiom's docs cover `v2`
    monitors; pin the right route during this step and capture a
    fixture).
  - `MonitorSummary { id, name, state, severity, last_check, query,
    notifiers: Vec<String> }`.
- `src/viz/monitor_list.rs`:
  - Table-like rendering reusing the step 14 table widget via a
    purpose-built `TableResult` adapter.
  - State colours: Firing red-bold, OK green, No-data dim yellow,
    Paused grey.
  - Sort defaults: firing first, then by `last_check` desc.
- `src/viz/note.rs`:
  - Parse `tile.query` (the `Query::Note(body)` variant) with
    `pulldown-cmark`. In `.mpl` single-tile mode the body is the
    buffer text minus the pragma; in dashboard mode it comes straight
    from the loaded JSON.
  - Map events to `ratatui::text::Line`s with styles:
    H1/H2/H3 = cyan-bold sized via prefix `# `, lists = `• `,
    code spans = yellow on dark grey, fenced blocks = dim grey
    background, links shown as `text (url)`.
  - No images, no tables-in-markdown (the table viz is already a
    thing).
- `src/viz/spacer.rs`:
  - Renders nothing inside the pane block. One screenful of tests
    confirms no panics on zero-size area.

### Keep simple

- Monitor list does not edit / acknowledge monitors; it's read-only.
  Mutations stay a follow-up.
- Note does not re-run the query — there is no query. Errors from a
  half-edited pragma surface as a diagnostic only.
- Spacer takes no options.

## Data model

```rust
pub enum MonitorState { Firing, Ok, NoData, Paused }

pub struct MonitorSummary {
    pub id: String,
    pub name: String,
    pub state: MonitorState,
    pub severity: Option<String>,
    pub last_check: Option<i64>,
    pub query_apl: Option<String>,
}

pub struct MonitorListOpts {
    pub filter_state: Option<MonitorState>,
    pub filter_tag: Option<(String, String)>,
}

pub struct NoteOpts; // none
pub struct SpacerOpts; // none
```

## Tasks

1. Monitors endpoint: pin path / auth / decode; fixture under
   `tests/fixtures/monitors.json`.
2. Per-tile query dispatch: kinds `monitor_list`, `note`, `spacer`
   never call the metrics/APL path. Monitor tiles trigger a monitors
   fetch (TTL-cached); note/spacer just re-render on resize.
3. Monitor renderer + colour rules + filters; legend pane becomes a
   state filter (`All / Firing / OK / NoData / Paused`).
4. Note renderer using `pulldown-cmark`. Add as a dep. Render off the
   tile's `Query::Note(body)` field (preserve trailing newlines).
5. Spacer: trivial implementation, one snapshot test.

## Acceptance criteria

- `// @viz monitor_list` shows the org's monitors with correct state
  colours; filter via legend works.
- `// @viz note` followed by a typical README-style markdown body
  renders headings + lists + code recognisably.
- `// @viz spacer` shows an empty bordered pane with no crash, no
  console noise.
- All previous kinds continue to work unchanged.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`
- Markdown rendering test: a small fixture with H1/H2/list/code yields
  the expected `Vec<Line>` styles.
- Manual: open the org's monitors, confirm Firing-first sort and
  state filter; switch buffer between `note`, `spacer`, and a real
  chart kind to ensure dispatch is clean.
