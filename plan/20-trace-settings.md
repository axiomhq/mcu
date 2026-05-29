# Step 20 — App-private settings + `:trace` config

## Status

**Done.** Disposition A: bare `:trace` keeps its legacy
trace-id reporter; sub-commands (`set` / `get` / `unset`) and the
`<id>` placeholder land alongside.

+33 tests vs. step-19 baseline
(12 `settings::tests`, 13 `app::tests::query` trace-dispatcher,
8 `cmdline_complete::tests` trace-completion). Full suite: 736
unit + 1 integration, fmt/clippy clean.

## Incremental outcome

A new app-private settings file (separate from `~/.axiom.toml`) holds
ax's own preferences. The first two keys it carries are
`trace_dataset` and `trace_deployment` — the defaults the upcoming
trace view (steps 22+) will consult when the user runs `:trace <id>`
without arguments.

No trace UI ships this step. The settings file lands, the
`:trace set` / `:trace get` ex-commands work and persist, and the
cmdline completer learns the sub-commands. The values are read but
nothing in the editor / dashboard view consumes them yet.

## User-visible improvement

- `:trace set dataset=axiom-traces-dev` persists the default trace
  dataset across launches.
- `:trace set deployment=staging` persists the default deployment to
  use for trace queries (independent from `active_deployments` in
  `~/.axiom.toml`, so traces can live on a different Axiom edge than
  the metrics workflow).
- `:trace get` echoes the current pair (or "(unset)") to the status
  bar.
- `:trace<Tab>` and `:trace set<Tab>` complete to the supported
  sub-commands and keys.

## Scope

### Add

- `src/settings.rs` — load/save a small TOML at the platform config
  dir (`etcetera::BaseStrategy::config_dir().join("ax/settings.toml")`,
  i.e. `$XDG_CONFIG_HOME/ax/settings.toml` on Linux). Atomic write
  via tempfile + rename, mirroring `cache.rs` / `history.rs`.
- `App.settings: Arc<RwLock<Settings>>` (same shape as the cache so
  background tasks added later can read without blocking the UI
  thread).
- `:trace` ex-command dispatcher (`set`, `get`, `unset`) under
  `src/app/ex_cmds.rs`. `:trace <id>` (without `set` / `get` /
  `unset`) returns "trace view not implemented yet" — wired in
  step 22.
- Cmdline completion entries in `src/cmdline_complete.rs` for the
  new sub-commands. `set` arg completion suggests
  `dataset=<cached-dataset>` and `deployment=<config-deployment-name>`.

### Keep simple

- Two keys only. Future settings (e.g. picker default window) layer
  in without schema churn — `Settings` is a single struct with
  `#[serde(default)]` fields.
- No migration code; the file is created on first write.
- Don't validate that the dataset actually exists or that the
  deployment resolves — bad values surface as errors when the trace
  view (step 22+) tries to use them. Validation here would require
  network I/O just to set a string.

## Settings file shape

`$XDG_CONFIG_HOME/ax/settings.toml`:

```toml
[trace]
dataset    = "axiom-traces-dev"
deployment = "staging"
```

The `[trace]` table is the only one for now. Top-level keys are
avoided so the next feature group (e.g. `[ui]`, `[picker]`) drops
into its own table without colliding.

## Why a new file, not `~/.axiom.toml`

`~/.axiom.toml` is shared with the official Axiom CLI; ax has been
careful not to add fields to it. App-specific preferences (especially
ones the CLI would never read) belong in ax's own config dir.
Symmetric to `discovery.json` (cache) and `history.json` (state)
already living under `etcetera` paths.

## Tasks

1. Add `src/settings.rs` with `Settings { trace: TraceSettings }`,
   atomic load/save, default `Settings::default()` if the file is
   missing. Mirror the `Cache::load` / `Cache::save` ergonomics.
2. Wire `App::settings: Arc<RwLock<Settings>>` and load it in
   `App::with_cache_and_history`.
3. Implement `:trace set KEY=VALUE [KEY=VALUE…]` in
   `src/app/ex_cmds.rs`. Accept `dataset`, `deployment`; reject
   unknown keys with a status error. Persist on success.
4. Implement `:trace get` (status-bar echo) and `:trace unset KEY…`.
5. Cmdline completion: `:trace` → `set | get | unset | <id>`;
   `:trace set ` → `dataset= | deployment=`; values complete from
   the discovery cache (datasets) and the loaded `Config`
   (deployments).
6. Unit tests under `src/settings/tests.rs`:
   * round-trip a populated `Settings` through save/load.
   * default-on-missing-file.
   * malformed TOML returns error without panicking.
7. Ex-command tests covering `set` / `get` / `unset` happy path and
   the unknown-key rejection.

## Acceptance criteria

- `:trace set dataset=axiom-traces-dev deployment=staging` persists;
  relaunching the binary shows the same values via `:trace get`.
- Bad key (`:trace set foo=bar`) rejects with status error; no file
  write occurs.
- `:trace<Tab>` cycles through `set / get / unset`.
- `:trace set <Tab>` offers `dataset=` and `deployment=`.
- `:trace <id>` (any other arg shape) prints "trace view not
  implemented yet" — placeholder for step 22.
- Existing test suite (≥509 tests at step-19 close) keeps passing.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo llvm-cov` — new module + ex-command path must be covered.
- Manual: round-trip `:trace set` / relaunch / `:trace get`.

## Files touched

- `src/settings.rs` (new) and `src/settings/tests.rs` (new).
- `src/app/mod.rs` — add `settings` field + bootstrap load.
- `src/app/ex_cmds.rs` — `:trace` dispatcher.
- `src/cmdline_complete.rs` — sub-command completion entries.
- `src/main.rs` — only if startup wiring needs adjusting.
- `docs/keys.md` — short "Settings" line under the ex-commands
  reference.

## Out of scope

- `:trace <id>` real behaviour — step 22.
- `:traces ls` — step 25.
- Picker default time window key — folds into a future step once the
  picker exists.
