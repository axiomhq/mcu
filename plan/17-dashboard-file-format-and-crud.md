# Step 17 — Dashboard file format + load/save

## Incremental outcome

The internal `Dashboard` from step 11 gets a stable on-disk format and a
loader/saver. The first cut is local-file-only (`*.axiom.json`); a
small follow-up wires Axiom's server CRUD endpoints behind the same
code path. After this step the TUI can open a real, multi-tile
dashboard exported from Axiom and re-render every tile using the
viz kinds shipped in steps 11–16.

## User-visible improvement

- `:open <path>` opens a dashboard file; the app switches from
  single-buffer mode to dashboard mode (still solo-tile renderer
  for now — grid is step 18).
- `:tile <n>` (or `Ctrl-w h/l` between adjacent tiles) cycles the
  focused tile.
- `:w` writes the dashboard JSON back to disk, preserving the on-disk
  byte order where possible.
- `:dash ls` lists dashboards on the server; `:dash open <id>` fetches
  one; `:dash save` pushes the current dashboard back.

## Prerequisites pinned (from the real OpenAPI spec at
`https://axiom.co/docs/restapi/endpoints/getDashboards.md`)

- Endpoints are **v2**, not v1, and use `uid` as the path-friendly key:
  - `GET    /v2/dashboards?limit=1000&offset=0`  — paginated list
  - `GET    /v2/dashboards/uid/{uid}`            — single fetch
  - `POST   /v2/dashboards`                       — create
  - `PUT    /v2/dashboards/uid/{uid}`            — full replace
  - `DELETE /v2/dashboards/uid/{uid}`            — delete
  - `PATCH  /v2/dashboards/uid/{uid}/charts/{chartId}` — single chart
  Auth header: `Authorization: Bearer <token>` (same as everywhere else).
- Response envelope is `DashboardResource`:
  ```
  { uid, id, version, createdAt, createdBy, updatedAt, updatedBy,
    dashboard: DashboardDocument }
  ```
  Conflict detection is on `version` (monotonic int), not `updatedAt`.
- `DashboardDocument` is `additionalProperties: false`. Round-trip
  fidelity is therefore **load-bearing**: any field we drop on decode
  will be rejected by PUT. Use a `#[serde(flatten)] extras` bucket on
  every level we don't fully model.
- Chart variants are a `oneOf` discriminated by `type` (string), with
  values: `TimeSeries`, `Heatmap`, `LogStream`, `Pie`, `Scatter`,
  `Table`, `TopK`, `Statistic`, `Note`. **Note divergence from TUI**:
  TUI has `Bar`, `Area`, `Spacer`, `MonitorList` which the server does
  not; server has `Scatter` which the TUI hasn't implemented yet. The
  TUI<->wire mapping lives in `dashboard::Tile::from_chart` (step 17b),
  with TUI-only kinds round-tripping via `extras`.
- Layout is a `Vec<LayoutItem>` with `i` (chart id), `x` (0..=11),
  `y` (nullable u32 for auto-stack), `w`, `h`. 12-column grid.
- Time window: `timeWindowStart` / `timeWindowEnd` are strings using
  `qr-now-{duration}` or epoch-ms-as-string. Comparison window is
  separate (`against` enum or `againstTimestamp`).
- API token note: list/get only return dashboards shared org-wide or
  with a group; private dashboards are invisible to token auth.
- Capture one real dashboard JSON export under
  `tests/fixtures/dashboard_full.json` as the canonical schema source
  (still TODO — needs a human with org access).

## Scope

### Add

- `src/dashboard_io.rs`:
  - `pub fn load(path) -> Result<DashboardSummary>` (deserialise the
    same `DashboardResource` envelope the server returns).
  - `pub fn save(path, &DashboardSummary)` (serialise + pretty-print
    + atomic rename). Default extension `.axiom.json`.
- Extend `src/axiom.rs` (already started):
  - [x] `get_dashboard(uid)`              — 17a, done
  - [x] `list_dashboards()`               — prior turn, done
  - [ ] `put_dashboard(uid, &DashboardDocument, version)`
  - [ ] `create_dashboard(&DashboardDocument)`
  - [ ] `delete_dashboard(uid)`
  - [ ] `patch_dashboard_chart(uid, chartId, partial)` (optional)
- Ex-commands:
  - [x] `:dashboards` / `:db`             — prior turn, done
  - [x] `:open <uid>`                     — 17a, done
  - [x] `:dashinfo` / `:di`               — 17a, done
  - [ ] `:edit <path>` for local files
  - [ ] `:w` / `:write [path]`
  - [ ] `:dash save` / `:dash save!`      — PUT with conflict detection
  - [ ] `:dash new from-buffer`
  - [ ] `:dash rm <uid>` (with confirmation)
- An in-memory "mode" flag on `App` (`Solo` vs `Dashboard`); rendering
  in this step stays solo (only the focused tile is drawn) — step 18
  adds the grid view.

### Keep simple

- One open dashboard at a time. Tabs come later if anyone asks.
- `:dash save` PUTs the whole document; no diff / patch.
- Conflict detection on save = compare server `updatedAt` to the value
  we loaded; surface a diagnostic and require `:dash save!` to force.
- No auto-refresh; user-initiated `:r` reruns all tiles.

## Data model deltas vs step 11

The wire types live in `src/axiom.rs` and mirror the server schema
exactly (so PUT round-trips). The internal `dashboard::Dashboard` from
step 11 stays the TUI-facing model; an adapter in step 17b lowers
`DashboardSummary` (wire) onto `Dashboard` (internal) and back.

Wire side (already in `src/axiom.rs` after 17a):

```rust
pub struct DashboardSummary {              // == DashboardResource
    pub uid: String,
    pub id: Option<String>,
    pub updated_at: Option<String>,
    pub updated_by: Option<String>,
    pub dashboard: DashboardDocument,
    // version, createdAt, createdBy: TODO (need for conflict detection)
}

pub struct DashboardDocument {
    pub name: Option<String>,
    pub description: Option<String>,
    pub charts: Vec<Chart>,
    pub layout: Vec<LayoutItem>,
    pub time_window_start: Option<String>, // "qr-now-1h" or epoch-ms-str
    pub time_window_end: Option<String>,   // "qr-now" or epoch-ms-str
    #[serde(flatten)]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

#[serde(tag = "type")]
pub enum Chart {
    TimeSeries(ChartBase), Heatmap(ChartBase), LogStream(ChartBase),
    Pie(ChartBase), Scatter(ChartBase), Table(ChartBase),
    TopK(ChartBase), Statistic(ChartBase), Note(ChartBase),
}
```

Internal side (step 17b will extend `dashboard::Dashboard`):

```rust
pub struct Dashboard {
    pub uid: Option<String>,        // wire `uid`, present after load
    pub version: Option<i64>,       // wire `version`, for conflict check
    pub name: String,
    pub description: Option<String>,
    pub time_range: TimeRange,
    pub variables: BTreeMap<String, Variable>,
    pub tiles: Vec<Tile>,
    pub layout: Layout,
    pub extras: serde_json::Value,  // round-trip bucket
}
```

## Tasks

### 17a — fetch + view (done)
- [x] `Client::get_dashboard(uid)` against `GET /v2/dashboards/uid/{uid}`.
- [x] Wire types: `DashboardSummary`, `DashboardDocument`, `Chart`
      (tagged), `ChartBase`, `LayoutItem` with `extras` buckets.
- [x] `:open <uid>`, picker-Enter → fetch, `:dashinfo` overlay.
- [x] Decode tests for envelope, chart variants, layout, extras
      round-trip.

### 17b — wire → internal adapter + first tile renders
- [ ] `dashboard::Dashboard::from_resource(&DashboardSummary)` and the
      inverse `to_resource`. TUI-only viz kinds round-trip via
      `chart.extras`.
- [ ] Map `Chart` `type` → `VizKind` (Note: `TopK ↔ TopList`).
- [ ] Translate `query: serde_json::Value` to MPL-ish where possible;
      otherwise store as opaque "server-side query" and surface a tile
      placeholder.
- [ ] Render the loaded dashboard's first tile in solo mode (existing
      `App.dashboard` slot). Unimplemented or untranslatable queries
      get a “not yet supported” placeholder — no crash, no silent skip.

### 17c — local file format
- [ ] `src/dashboard_io.rs` with `load(path)` / `save(path, &resource)`.
- [ ] `:edit <path>` opens; `:w` / `:write [path]` saves.
- [ ] Default extension `.axiom.json`; autodetect MPL vs dashboard by
      extension + magic-key sniff (top-level `dashboard` object).
- [ ] Capture `tests/fixtures/dashboard_full.json` from a real export;
      add a byte-stable round-trip test using `serde_json::to_string_pretty`.

### 17d — server writes (done)
- [x] `Client::put_dashboard(uid, &DashboardDocument, expected_version, overwrite, message)`
      against `PUT /v2/dashboards/uid/{uid}`. Encodes the
      `DashboardUpsertRequest` envelope (dashboard, version, overwrite,
      uid, message). `overwrite=true` skips the version check; without
      it the server returns 412 with `currentVersion`.
- [x] `Client::create_dashboard(&DashboardDocument, uid?, message?)`
      via `POST /v2/dashboards`. Wired in 17e (`:dash new from-buffer`).
- [x] `Client::delete_dashboard(uid)` via `DELETE`.
- [x] `:dash save`, `:dash save!`, `:dash rm <uid>` Ex-commands.
- [x] `AppEvent::DashboardSaved` re-stamps the in-memory copy with the
      server's new version on success; `DashboardDeleted` clears the
      copy when its uid matches.
- [x] Structured `DashboardError` decoder so 412s surface the server's
      current version in the error overlay.

### 17e — `:dash new from-buffer` (done) + variables (deferred)
- [x] `:dash new from-buffer [name]` POSTs a single-chart dashboard
      built from the current MPL buffer + `// @viz` pragma.
- [x] `build_dashboard_doc_from_buffer` pure helper: maps each `VizKind`
      to its server-side `Chart` variant (TUI-only kinds fall back to
      `TimeSeries`); stashes `owner` / `refreshTime` / `schemaVersion`
      in `extras` to satisfy the server's required-fields contract.
- [ ] **Deferred: dashboard variables.** The v2 OpenAPI spec doesn't
      document a `variables` / `parameters` field on `DashboardDocument`,
      so promoting `:p` params to dashboard variables can't be modelled
      until we capture a real dashboard with variables and see how the
      server actually serialises them (likely as `extras` keys today).

## Acceptance criteria

- Round-trip: load `tests/fixtures/dashboard_full.json`, save it, and
  the bytes are byte-equal (modulo key order which is stable through
  serde + `serde_json::to_string_pretty`).
- A real exported dashboard loads without losing fields the TUI
  doesn't model yet — they survive in `extras` and reappear on save.
- `:dashboards` shows the org's dashboards; selecting one (or
  `:open <uid>`) fetches and renders the focused tile correctly when
  its kind is one of the implemented kinds; unimplemented kinds render
  a "not yet supported" placeholder tile (no crash, no silent skip).
- Stale-write detection (17d): editing a dashboard that another client
  updated produces an `Error` diagnostic, and `:dash save!` overrides.

## Verification

- `cargo fmt && cargo clippy --all-targets && cargo test`
- Fixture round-trip test + extras-preservation test.
- Manual: export a small dashboard from Axiom, open it, verify each
  tile kind renders or gracefully degrades; save and re-export.
