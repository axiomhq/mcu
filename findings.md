# Codebase review — findings

Quick measurement first, then concrete waste with line refs.

## 1. The shape of the problem

```
file                              total    tests     code
src/app.rs                         9377     3630     5747
src/ui.rs                          2735      191     2544
src/viz.rs                         2075      477     1598
src/axiom.rs                       1181      318      863
src/command.rs                      857      295      562
src/motion.rs                       746      216      530
src/completions.rs                  808      296      512
src/chart.rs                        705      207      498
src/cache.rs                        700      211      489
src/dashboard.rs                    767      319      448
src/mpl.rs                          509      119      390
src/highlight.rs                   664      288      376
src/cmdline_complete.rs             398      128      270
src/hover.rs                        303       80      223
src/main.rs                         354      143      211
src/share.rs                        210       91      119
src/params.rs                       95         0       95
src/config.rs                      156       74        82
src/term.rs                         34         8       26
src/editor.rs                       19         0       19
                                  -----   -----    -----
                                  22732    7210    15522
```

- **22.7 k LOC total, of which ~7.2 k are tests (~32 %)**. Real code is closer
  to **15.5 k**.
- `app.rs` alone is **9.4 k lines** (5.7 k code + 3.6 k tests, **210 unit
  tests in one file**). It's the dominant pain point.
- 37 `#[allow(dead_code)]` annotations across the tree — strong signal of
  speculative scaffolding ("kept here so the surface is complete before
  step 17e"). Several point at types whose every field is unused.
- 28 "step N" markers in code comments referencing roadmap items, most of
  which never landed. They reserve API surface in advance.

The application is functionally reasonable. The waste is concentrated in
three places: (1) `app.rs` is a god-module mixing every concern; (2) async
fetch + overlay rendering have copy-paste shapes that beg for one helper
each; (3) the dashboard data model carries two parallel representations
(wire + internal) while only the wire one is actually used end-to-end.

## 2. Module separation by concern

Even after pulling tests out (§ 6), several files are still doing
multiple jobs each. Production-code LOC per file:

```
src/app.rs    5747   god-module             needs ~12-way split
src/ui.rs     2544   mixed render concerns  needs ~7-way split
src/viz.rs    1598   one fn per viz kind    needs one-file-per-kind split
src/axiom.rs   863   wire types + HTTP +
                     MPL parsers (!)        split MPL parsers out
src/command.rs 562   one parser, focused    keep
src/motion.rs  530   one motion engine      keep
src/completions.rs 512 one completion engine keep
src/chart.rs   498   data types + render    borderline; split if growing
src/cache.rs   489   one cache type         keep
src/dashboard.rs 448 wire-adjacent model    shrinks under § 5.2 anyway
```

Four files are mixing concerns at the file level. The others are
single-concern at their current size and don't need restructuring.

### 2.1 `src/app.rs` — god-module (5747 code LOC)

`App` has **~45 fields** and **~190 methods**; the impl block runs
[L820–L4973](file:///Users/heinzgies/Projects/mcu/src/app.rs#L820-L4973).
Natural seams (one module each):

```
events.rs            AppEvent + drain/handle_event           ~250 LOC
keymap.rs            on_key dispatch + Ctrl-w + set_focus    ~180
vim/                 normal / insert / visual / motions /
                     operators / yank / paste / indent       ~600
panes/dashboard.rs   handle_dashboard_key + tile sub-modes   ~400
panes/legend.rs      legend nav + tag picker + persistence   ~250
panes/params.rs      params nav + prefill_command            ~120
cmdline.rs           handle_command_key + Tab popup          ~200
commands/            execute_command match + per-:cmd impls  ~900
time_picker.rs       :time + custom date picker              ~280
queries.rs           fetch_* + run_query + run_tile_queries  ~500
dashboard_ops.rs     adopt, fetch_dashboard_by_uid, save,
                     delete, picker, zoom                   ~700
files.rs             :w :e :q + dirty + open_file            ~250
util.rs              referenced_tags, cursor offsets, etc.   ~250
```

Top-level mechanical wins (no behaviour change):
- The **`handle_event` `match`** at [L4439–L4670](file:///Users/heinzgies/Projects/mcu/src/app.rs#L4439-L4670)
  is ~230 lines across 13 variants. Each arm becomes its own free fn
  (`on_datasets_fetched`, `on_metrics_fetched`, ...).
- The **`execute_command` `match`** dispatches by string head; split per
  command family (file / dashboard / tile / time / viz).
- `cmd_tile` ([L3490–L3640](file:///Users/heinzgies/Projects/mcu/src/app.rs#L3490-L3640)) is one ~150-line nested match
  for `add` / `rm` / `mv` / `size` / `title` / `json`. One sub-fn per
  arm collapses cleanly.

These three splits alone move ~600 lines into smaller files; the rest is
just `pub(crate)` and `mod`.

### 2.2 `src/ui.rs` — seven concerns in one file (2544 code LOC)

38 free functions across at least seven distinct surfaces:

| concern | functions | LOC |
|---|---|---:|
| **top-level layout** | `capped`, `draw` ([L28-L218](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L28-L218)), `pane_block` | ~200 |
| **dashboard grid** | `draw_dashboard_grid`, `compute_row_heights`, `resolve_slot`, `draw_grid_tile`, `draw_inline_legend`, `fit_inline_legend` ([L234-L838](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L234-L838)) | ~600 |
| **overlays / modals / pickers** | 13 fns: confirm-delete, tile-inspect, time picker (3), add-pick, dashinfo, dashboards picker, error, legend details, hover popup, help modal, cmdline completion popup, completion popup, quickfix popup ([L842-L2545](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L842-L2545)) | ~1100 |
| **editor pane** | `draw_editor`, `editor_title`, `diagnostic_count_suffix` ([L2056-L2178](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L2056-L2178)) | ~120 |
| **params pane** | `draw_params` ([L1506-L1610](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1506-L1610)) | ~100 |
| **status bar + cmdline** | `draw_status`, `diagnostic_status_or_default`, `diagnostic_count_summary`, `draw_command_line` ([L2179-L2356](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L2179-L2356)) | ~180 |
| **help parser** | `render_keys_help` ([L1928-L1989](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1928-L1989)) | ~60 |
| **generic helpers** | `wrap_message` | ~40 |

Proposed split:

```
src/ui/
  mod.rs          // pub fn draw() + dispatch + pane_block + capped     ~200
  grid.rs         // dashboard grid + tile rendering + inline legend    ~600
  editor.rs       // draw_editor + editor_title + diagnostic suffix     ~120
  status.rs       // status bar + cmdline + diagnostic summary          ~180
  params.rs       // draw_params                                        ~100
  help.rs         // render_keys_help + draw_help_modal                  ~120
  overlays/
    mod.rs        // common helpers (centered_box, modal_frame,
                  //                 picker_list) — see § 3.4            ~80
    pickers.rs    // dashboards / time-preset / add-pick                ~300
    modals.rs     // confirm-delete / tile-inspect / error / dashinfo   ~300
    legend.rs     // legend details modal                               ~120
    popups.rs     // hover / completion / cmdline-completion / quickfix ~250
    time_custom.rs// custom date picker (its own beast)                 ~120
```

Every file <300 lines, every overlay file shares the same three helpers
from § 3.4. Today the overlay code is **~1100 lines of near-clones**.

### 2.3 `src/viz.rs` — one file per viz kind (1598 code LOC)

The entire file is structured as `pub fn draw()` dispatching to
`draw_<kind>()`. Every kind has its own state, its own helpers, its own
format code. One file each is the natural shape:

```
src/viz/
  mod.rs         // VizSpec, VizKind, draw() dispatch                    ~120
  pragma.rs      // parse_pragma, format_pragma, upsert_pragma           ~100
  agg.rs         // Agg enum + label + format_value                       ~80
  palette.rs     // palette_color, viridis_rgb, color helpers              ~60
  fmt.rs         // truncate_to_width, format_x_label
                 //   (the latter calls through to chart::format_time_label)
  note.rs        // draw_note + markdown subset (render_markdown,
                 //   render_inline, strip_leading_pragma)               ~190
  statistic.rs   // draw_statistic                                       ~120
  top_list.rs    // top_list_rows + draw_top_list                        ~140
  pie.rs         // pie_rows + draw_pie                                  ~110
  heatmap.rs     // heatmap_bin + HeatmapBinned + draw_heatmap           ~270
  table.rs       // TableResult/TableCell + series_to_table + draw_table ~180
  spacer.rs      // draw_spacer                                           ~10
  log_stream.rs  // delete (§ 4.4) or wire properly                   (gone)
  monitor_list.rs// delete (§ 4.4) or wire properly                   (gone)
```

The `mod.rs` shrinks to a `match kind { VizKind::Pie => pie::draw(...), ... }`
plus the public types. Every leaf module is independently testable
against its own `Series` fixture.

### 2.4 `src/axiom.rs` — wire types + HTTP + MPL parsers (!) (863 code LOC)

The surprising mix here is **MPL parsing in the HTTP client module**:

- [`extract_dataset_metric`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L792-L840), [`extract_dataset`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L758-L765),
  [`skip_leading_comments_and_ws`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L766-L791)
  are MPL source-text parsers. They have nothing to do with Axiom's HTTP
  API — they exist because the dataset/metric names get sniffed out of
  the editor buffer before any request is built. They belong in
  `src/mpl.rs` (next to the existing analyser).

The rest of the file is two clean concerns:

- Wire types: `DashboardSummary`, `DashboardUpsertRequest`,
  `DashboardWriteResponse`, `DashboardError`, `DashboardDocument`,
  `Chart`, `ChartBase`, `LayoutItem`, `DatasetSummary`, `MetricInfo`,
  `MetricsQueryResponse`, `MetricsSeries` — ~280 LOC.
- HTTP client: `Client`/`Inner` + 10 endpoint methods + `send_upsert`
  + `extract_trace_id` + `urlencoding` + `snippet` — ~500 LOC.

Proposed split:

```
src/axiom/
  mod.rs         // re-exports                                          ~10
  types.rs       // all the #[derive(Serialize, Deserialize)] structs   ~280
  client.rs      // Client + endpoint methods (after § 3.2 dedup)        ~350
  util.rs        // extract_trace_id, urlencoding, snippet                ~50
src/mpl.rs       // existing + extract_dataset_metric (moved in)
```

Moving the MPL parsers fixes a real misclassification — right now
`completions.rs`, `app.rs`, and the dashboard module all import the
"MPL parser" from the HTTP client crate, which is the wrong dependency
direction.

### 2.5 `src/chart.rs` — borderline (498 code LOC)

Has two concerns: data types (`Series`, `Bounds`, `LegendSummary`,
`color_for`) and rendering (`draw_graph`, `draw_legend`, the time-axis
formatters). At 498 LOC it's borderline; not worth splitting today,
but if `Series` grows fields or `draw_graph` gains kinds, split into
`chart/types.rs` + `chart/render.rs`.

### 2.6 Files that are already single-concern

Leave alone: `command.rs` (parser), `motion.rs` (motion engine),
`completions.rs` (completion engine), `cache.rs` (one cache type),
`highlight.rs`, `mpl.rs`, `hover.rs`, `share.rs`, `config.rs`,
`main.rs`, `cmdline_complete.rs`, `params.rs`. These are all in the
300–700 LOC range and each is doing one thing. They're fine.

## 3. Copy-paste shapes (high-confidence dedup)

### 3.1 Async fetch boilerplate

Every background fetcher in `App` repeats:

```rust
if self.busy { ...return; }
let client = match self.ensure_client() { ... };
let tx = self.events_tx.clone();
let cache = self.cache.clone();
self.runtime.spawn(async move {
    let route = resolve_route(...).await?;
    let result = client.call(...).await;
    if let Ok(v) = &result { cache.write().unwrap().replace_*(...); save(); }
    let _ = tx.send(AppEvent::*Fetched { ... });
});
```

Call sites: [`fetch_datasets`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L2321-L2349),
[`fetch_metrics_for_current_query`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L2351-L2406),
[`fetch_tags`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L2407-L2449),
[`fetch_tag_values`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L2451-L2501),
[`run_query`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L2598-L2660),
[`run_tile_queries`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L3441-L3540),
[`run_focused_tile_query`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L1181-L1240),
[`fetch_dashboard_by_uid`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L3921-L4010),
[`cmd_dash_new`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L4011-L4080),
[`cmd_dash_save`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L4081-L4180),
[`cmd_dash_rm`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L4181-L4230),
[`cmd_dashboards`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L4231-L4280).

One helper:

```rust
fn spawn_with_client<F, Fut, T>(&mut self, label: &str, busy: bool, body: F)
where F: FnOnce(AxiomClient, Sender<AppEvent>, Arc<RwLock<Cache>>) -> Fut,
      Fut: Future<Output = T> + Send + 'static, T: Send + 'static;
```

… would cut ~250 lines.

### 3.2 HTTP request boilerplate in `axiom::Client`

Six near-identical `GET → check status → from_str` bodies:

- [`list_datasets`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L348-L368)
- [`get_dashboard`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L461-L487)
- [`list_dashboards`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L496-L517)
- [`list_metrics`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L524-L556)
- [`list_metric_tags`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L562-L596)
- [`list_metric_tag_values`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L601-L636)

A twin of [`send_upsert`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L432-L482) —
`async fn get_json<T: DeserializeOwned>(&self, url, label) -> Result<T>` —
collapses ~120 LOC to ~30.

### 3.3 Cache write boilerplate

Same `entry().or_default().insert(key, CachedX { fetched_at: unix_now(), ... })`
shape repeats in
[`replace_tags`](file:///Users/heinzgies/Projects/mcu/src/cache.rs#L174-L188),
[`replace_tag_values`](file:///Users/heinzgies/Projects/mcu/src/cache.rs#L213-L235),
[`replace_metrics`](file:///Users/heinzgies/Projects/mcu/src/cache.rs#L415-L423),
[`replace_dashboard`](file:///Users/heinzgies/Projects/mcu/src/cache.rs#L274-L283),
[`replace_dashboards`](file:///Users/heinzgies/Projects/mcu/src/cache.rs#L259-L264).
Mirror `has_*` accessors do the same lookup negative. A generic
`Cached<T> { fetched_at, data: T }` plus one `upsert`/`hit` pair
would consolidate.

> Net caveat: the on-disk JSON keys differ per variant (`tags`/`values`/
> `metrics`/`items`/`resource`). A clean dedup needs a schema bump; not
> urgent.

### 3.4 Overlay / modal / popup rendering

All overlays in `ui.rs` follow:

```
centered_rect(parent, w, h)
f.render_widget(Clear, area)
let block = Block::default().borders(ALL).border_style(...).title(...)
f.render_widget(Paragraph::new(...), inner) // or List, or rows
```

Same skeleton, ~15 copies:

- [`draw_confirm_delete_overlay`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L847-L891)
- [`draw_tile_inspect_overlay`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L898-L922)
- [`draw_time_preset_overlay`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L942-L1019)
- [`draw_time_custom_overlay`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1021-L1110)
- [`draw_add_pick_overlay`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1140-L1175)
- [`draw_dashinfo_overlay`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1180-L1325)
- [`draw_dashboards_picker`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1335-L1463)
- [`draw_error_overlay`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1465-L1497)
- [`draw_legend_details`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1607-L1727)
- [`draw_hover_popup`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1759-L1840)
- [`draw_help_modal`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L1846-L1898)
- [`draw_cmdline_completion_popup`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L2030-L2065)
- [`draw_completion_popup`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L2068-L2126)
- [`draw_quickfix_popup`](file:///Users/heinzgies/Projects/mcu/src/ui.rs#L2140-L2196)

`draw_completion_popup` + `draw_quickfix_popup` are near-clones (anchor-at-
cursor + list). `draw_dashboards_picker` + `draw_time_preset_overlay` +
`draw_add_pick_overlay` are near-clones (centred list with cursor highlight).
`draw_confirm_delete_overlay` + `draw_tile_inspect_overlay` +
`draw_error_overlay` + `draw_help_modal` are near-clones (centred paragraph).

Three small helpers cover all of it:

```rust
fn centered_box(parent: Rect, w_pct: u16, h_pct: u16) -> Rect;
fn modal_frame(title: &str) -> Block<'_>;
fn picker_list<I: Display>(items: &[I], selected: usize) -> List<'_>;
```

Realistic shrink: **~400 LOC out of ui.rs**.

### 3.5 Pane keymap shapes

Every `handle_*_key` repeats `match (code, mods)` with hjkl/arrows + Esc +
Enter + `g`/`G`. Affected:
[`handle_move_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L1276-L1310),
[`handle_resize_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L1311-L1340),
[`handle_add_pick_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L1382-L1420),
[`handle_params_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L1535-L1580),
[`handle_legend_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L1620-L1670),
[`handle_legend_details_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L1680-L1715),
[`handle_help_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L2615-L2648),
[`handle_time_preset_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L3155-L3205),
[`handle_dashboards_picker_key`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L4181-L4218).

A `CursorNav { len, selected }` helper that takes a `KeyEvent` and yields
`Move(delta) | Accept | Cancel | Other(k)` removes the duplicated arms.

## 4. Speculative scaffolding to delete or annotate

### 4.1 `src/dashboard.rs`

Twelve `#[allow(dead_code)]` annotations in one file. Most of the internal
`Tile`/`Dashboard`/`Layout`/`GridPos` model is never read — the renderer
walks `axiom::Chart` directly off `loaded_dashboard`.

- [`Tile::id`/`title`/`time_override`/`pos`](file:///Users/heinzgies/Projects/mcu/src/dashboard.rs#L195-L206)
- [`Dashboard::id`/`name`/`time_range`/`variables`/`layout`](file:///Users/heinzgies/Projects/mcu/src/dashboard.rs#L346-L356)
- [`GridPos`](file:///Users/heinzgies/Projects/mcu/src/dashboard.rs#L167-L177) — populated, never read.
- [`Layout`](file:///Users/heinzgies/Projects/mcu/src/dashboard.rs#L178-L194) — default-constructed, never consumed.
- [`Query::Apl`/`Note`/`Empty`](file:///Users/heinzgies/Projects/mcu/src/dashboard.rs#L141-L166) — only `Mpl` is exercised; `Apl`/`Note` are placeholders.
- [`Query::text`](file:///Users/heinzgies/Projects/mcu/src/dashboard.rs#L153-L165) — `#[allow(dead_code)]`.

Pragmatic shape: keep `VizKind` (it has variants the wire `Chart` enum
lacks) and `extract_query`; drop the rest. The `Dashboard`/`Tile` model
adds ~80 lines of indirection for zero runtime benefit today.

### 4.2 `src/axiom.rs`

- [`Client::create_dashboard`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L373-L394)
  is `#[allow(dead_code)]`. Currently `cmd_dash_save` always uses PUT
  (upsert), so create is unused. Either wire it (proper "new" path) or
  delete.
- Five "audit field" `#[allow(dead_code)]` annotations across
  [`DashboardSummary`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L39-L66),
  [`DashboardWriteResponse`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L87-L97),
  [`DashboardError`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L106-L116) — fine
  to keep as decode-only fields, but they should drop the `pub` and the
  `dead_code` annotation; serde keeps them with `#[serde(skip_serializing)]`.
- [`extract_dataset`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L758-L765) is a one-line wrapper around
  `extract_dataset_metric(...)?.0`. Only 2 of 10 call sites use it. Drop
  it; let callers call `.0` themselves.

### 4.3 `src/cache.rs`

- [`set_legend_tags`](file:///Users/heinzgies/Projects/mcu/src/cache.rs#L351-L372) and
  [`set_legend_tags_for_metric`](file:///Users/heinzgies/Projects/mcu/src/cache.rs#L373-L412)
  duplicate the clear-on-empty + insert branches. Collapse into
  `set_legend_tags(query_hash: Option<&str>, dataset, metric, tags)`.
- Two stale `#[allow(dead_code)]` annotations on `tag_values_for` and
  `metric_names` (they're both used; just drop the annotations).

### 4.4 `src/viz.rs`

- [`draw_log_stream`](file:///Users/heinzgies/Projects/mcu/src/viz.rs#L1339-L1393)
  declares `let events: &[EventRow] = &[];` and only renders the empty-
  state branch in practice. The `EventRow` / `Level` types
  ([L1286-L1346](file:///Users/heinzgies/Projects/mcu/src/viz.rs#L1286-L1346))
  exist for a fetcher that hasn't shipped. ~110 dead lines.
- [`draw_monitor_list`](file:///Users/heinzgies/Projects/mcu/src/viz.rs#L438-L470) is a placeholder ("renderer
  ready; GET /v1/monitors fetch wires in step 16b"). ~30 lines.
- [`draw_unsupported_placeholder`](file:///Users/heinzgies/Projects/mcu/src/viz.rs#L213-L237) — explicitly
  `#[allow(dead_code)]`.
- [`TableResult` / `TableCell::{Int, Bool, Time}`](file:///Users/heinzgies/Projects/mcu/src/viz.rs#L1409-L1430)
  variants `#[allow(dead_code)] // populated by the APL decoder in a
  follow-up`. The `Number`/`String` variants are the only ones the
  current MPL path produces.
- [`format_x_label`](file:///Users/heinzgies/Projects/mcu/src/viz.rs#L1235-L1244) has a self-doc comment saying
  it's a copy of `chart::format_time_label`. Should call through.
- [`truncate_to_width`](file:///Users/heinzgies/Projects/mcu/src/viz.rs#L922-L937) is duplicated inline in
  `draw_dashinfo_overlay`. Make it `pub(crate)` and reuse.

### 4.5 `src/params.rs`

All 7 fields of `SystemParam` are `#[allow(dead_code)]` except `name`.
Either delete the unused ones or make them `pub(crate)` and stop
annotating.

## 5. Two-representation traps

### 5.1 `extract_dataset` + `classify_chart_query` wrappers

Same pattern in two places: a public wrapper that just forwards into a
private worker.

- [`extract_dataset`](file:///Users/heinzgies/Projects/mcu/src/axiom.rs#L758-L765) →
  `extract_dataset_metric(mpl)?.0`
- [`classify_chart_query`](file:///Users/heinzgies/Projects/mcu/src/dashboard.rs#L264-L266) →
  `extract_query(chart)`

Either delete the wrapper or `pub(crate)` the worker; today the only
purpose is to keep "public-looking" names while the impl is private.

### 5.2 Dashboard wire vs internal model

`axiom::Chart`/`ChartBase` (wire) and `dashboard::Tile`/`Dashboard`
(internal) coexist. The grid renderer (and most of `app.rs`) iterates
`loaded_dashboard.dashboard.charts` directly — i.e. the wire model.
`dashboard::Dashboard` is built but never used as the source of truth
except for the editor pragma round-trip (`VizKind`).

Smallest correct shape: keep `VizKind` + `extract_query` as a small
classifier module; drop `Dashboard`/`Tile`/`Layout`/`GridPos`. ~200
lines.

## 6. Tests inlined where they shouldn't be

**Every test in this codebase lives in an inline `#[cfg(test)] mod tests`
at the bottom of its host file.** That's the right pattern for small
modules but it's the wrong default once a file passes ~500 lines of code
— the host file balloons, jumping between code and tests becomes a
seek-and-scroll problem, and tooling that mass-edits the production code
(rename, restructure) ends up touching massive irrelevant test blocks.

Numbers:

| file | code | tests inlined | total | tests % |
|---|---:|---:|---:|---:|
| `src/app.rs` | 5747 | **3630** | 9377 | 39 % |
| `src/viz.rs` | 1598 | 477 | 2075 | 23 % |
| `src/dashboard.rs` | 448 | 319 | 767 | 42 % |
| `src/axiom.rs` | 863 | 318 | 1181 | 27 % |
| `src/command.rs` | 562 | 295 | 857 | 34 % |
| `src/completions.rs` | 512 | 296 | 808 | 37 % |
| `src/highlight.rs` | 376 | 288 | 664 | 43 % |
| `src/cache.rs` | 489 | 211 | 700 | 30 % |
| `src/motion.rs` | 530 | 216 | 746 | 29 % |
| ... | | | | |
| **total** | **15522** | **7210** | **22732** | **32 %** |

`app.rs` is the egregious case: **3630 of its 9377 lines are tests** in a
single inline `mod tests` block ([L5751-L9377](file:///Users/heinzgies/Projects/mcu/src/app.rs#L5751-L9377)),
210 `#[test]` fns.

The idiomatic Rust pattern for a module this size is the **sibling-file**
layout: either

```rust
// src/foo.rs
#[cfg(test)]
#[path = "foo_tests.rs"]
mod tests;
```

or convert the module to a directory:

```
src/foo/
  mod.rs       // re-exports + #[cfg(test)] mod tests;
  tests.rs     // 100% test code
```

Either works; the `#[path]` form is the lower-churn migration (no
directory restructure, no `mod.rs` boilerplate). The end result is the
same: production and test code in separate files, navigable
independently, ctags-friendly, mass-edits don't smear across blocks of
tests.

**Per-module proposal** (lowest-effort first):

| module | move tests to |
|---|---|
| `src/app.rs` | `src/app_tests.rs` (split into multiple test files matches the eventual module split in § 2) |
| `src/viz.rs` | `src/viz_tests.rs` |
| `src/ui.rs` | `src/ui_tests.rs` |
| `src/axiom.rs` | `src/axiom_tests.rs` |
| `src/dashboard.rs` | `src/dashboard_tests.rs` |
| `src/cache.rs` | `src/cache_tests.rs` |
| `src/chart.rs` | `src/chart_tests.rs` |
| `src/command.rs` | `src/command_tests.rs` |
| `src/completions.rs` | `src/completions_tests.rs` |
| `src/highlight.rs` | `src/highlight_tests.rs` |
| `src/motion.rs` | `src/motion_tests.rs` |
| `src/mpl.rs` | `src/mpl_tests.rs` |
| `src/hover.rs` | `src/hover_tests.rs` |
| `src/cmdline_complete.rs` | `src/cmdline_complete_tests.rs` |
| `src/config.rs` | `src/config_tests.rs` |
| `src/share.rs` | `src/share_tests.rs` |
| `src/highlight.rs` | `src/highlight_tests.rs` |

Small files (`term.rs` 8 test lines, `editor.rs` 0, `params.rs` 0) stay
inline.

Shared test helpers — `test_app()`, `key()`, `ctrl()`, `type_text()`,
`app_with_series()`, `seed_cache()`, `set_buffer()`, `multi_chart_resource()`,
etc. — collect into one `src/test_support.rs` (gated by `#[cfg(test)]`)
that every `*_tests.rs` pulls in via `use crate::test_support::*;`. Today
they're scattered (e.g. `multi_chart_resource()` at
[`src/app.rs:8206`](file:///Users/heinzgies/Projects/mcu/src/app.rs#L8206), used in 30+ tests in the same file).

### Mechanics of the migration

For each file:

1. Cut the entire `#[cfg(test)] mod tests { ... }` block.
2. Paste into a new `src/<name>_tests.rs` (no outer `mod tests` wrapper;
   `#[path]` already says "this file IS the module").
3. Above the cut point, add
   ```rust
   #[cfg(test)]
   #[path = "<name>_tests.rs"]
   mod tests;
   ```
4. Add `use super::*;` and any specific imports inside the test file.
   The test file has visibility into `pub(crate)` items via the
   `mod tests` declaration in the parent.
5. `cargo test` to confirm green.

No behaviour change. Test count and assertions identical. The only risk
is missing `use super::*;` imports, which the compiler surfaces
immediately.

Doing this **before** the `app.rs` split in § 2 is the right order: once
the 3.6k-line test block is no longer interleaved with production code,
the production code is small enough that the seams are obvious.

### Per-file ratio sanity

After this move, the production-code totals match what we should be
reviewing:

```
src/app.rs              5747 LOC of code  (down from 9377)
src/ui.rs               2544
src/viz.rs              1598
src/axiom.rs             863
src/command.rs           562
src/motion.rs            530
src/completions.rs       512
src/chart.rs             498
src/cache.rs             489
src/dashboard.rs         448
src/mpl.rs               390
src/highlight.rs         376
...
                       -----
                       15522 LOC of real code
```

Which is still too much, but the targets in § 3–§ 5 act on the
production side directly; the test relocation just makes the production
code legible enough to act on.

## 7. Roadmap-comment debt

28 grep hits for `step N` / `step Na` referring to a development plan
that doesn't appear to exist in the repo. They double as "this is dead
on purpose, will activate in step X". Either commit to a roadmap file
(and link to it) or remove the references — they're misleading without
context.

## 8. Concrete LOC estimates

| Action | Approx LOC removed |
|---|---|
| Async-fetch helper across 12 sites | 250 |
| HTTP `get_json` helper (6 sites) | 90 |
| Overlay/modal/popup helpers (15 sites) | 400 |
| Pane-keymap helper (9 sites) | 150 |
| Cache `set_legend_tags` collapse | 30 |
| Drop `dashboard::Dashboard`/`Tile`/`Layout`/`GridPos` | 200 |
| Drop `draw_log_stream` + types | 130 |
| Drop `draw_monitor_list` + `draw_unsupported_placeholder` | 50 |
| Drop unused `axiom::create_dashboard` (or wire it) | 25 |
| Drop `extract_dataset` wrapper + `classify_chart_query` wrapper | 20 |
| Inline `truncate_to_width` + `format_x_label` | 20 |
| Drop unused `SystemParam` fields | 10 |
| **Total realistic** | **~1.4 k LOC** |

Plus another ~1 k LOC of pure file-relocation noise (the app.rs split)
which doesn't shrink the binary but makes the rest of the project
reviewable.

After the cleanup: ~14 k LOC real code (down from 15.5 k), with
`app.rs` reduced to a ~500-line shell + ~12 focused submodules of
200–700 lines each. That's a defensible size for a TUI metrics
client with a dashboard editor.

## 9. Suggested order of attack

1. **Move every inline `#[cfg(test)] mod tests` into a sibling
   `<name>_tests.rs` via `#[path]`** (§ 6). Pure mechanical, zero risk,
   makes everything else reviewable. `app.rs` alone goes from 9.4k to
   5.7k lines.

2. **Delete the obviously dead surface** (§ 4): `draw_log_stream`,
   `draw_monitor_list`, `draw_unsupported_placeholder`,
   `create_dashboard` (or wire it), unused `SystemParam` fields, stale
   `#[allow(dead_code)]` annotations, `extract_dataset` /
   `classify_chart_query` wrappers. Targets ~250 LOC and 15+ `dead_code`
   annotations.

3. **Collapse the three high-traffic helpers** (§ 3, one commit each):
   - `spawn_with_client` in `app.rs`
   - `get_json` in `axiom.rs`
   - `centered_box` + `modal_frame` + `picker_list` in `ui.rs`

4. **Decide on the dashboard model** (§ 5.2): either commit to the
   internal `Dashboard`/`Tile` representation (and route the renderer
   through it), or drop it. Don't keep both.

5. **Module separation by concern** (§ 2). Do them in this order so
   each commit shrinks the next target:
   - **5a. `axiom.rs`**: move MPL parsers (`extract_dataset_metric`,
     `extract_dataset`, `skip_leading_comments_and_ws`) to `mpl.rs`.
     Split rest into `axiom/{mod,types,client,util}.rs` (§ 2.4).
   - **5b. `viz.rs`**: one file per viz kind under `viz/`. Step 2
     already dropped `log_stream` + `monitor_list`, so the remaining
     kinds split cleanly (§ 2.3).
   - **5c. `ui.rs`**: split into `ui/{mod,grid,editor,status,params,help,overlays/...}.rs`
     using the shared helpers from step 3 (§ 2.2).
   - **5d. `app.rs`**: 12-way split along the seams listed in § 2.1.
     One submodule at a time, each commit keeps tests green.

Each step is independently shippable and leaves the app running.
Steps 1–4 are pure cleanup; step 5 is the big restructuring move
but depends on the helpers from step 3 existing first.
