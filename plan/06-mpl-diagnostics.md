# Step 06 — MPL diagnostics

## Incremental outcome

The app can catch MPL compile errors before making an API request, if the Rust `mpl-lang` API exposes
what is needed. If not, this step documents the blocker and keeps query execution fully functional.

## User-visible improvement

- Syntax/type errors appear faster and more locally than API errors.
- First diagnostic is visible in the status bar.

## Scope

### Add

- `mpl.rs` wrapper around public `mpl-lang` compile API.
- Diagnostic state in `App`.
- Optional pre-flight compile before query execution.

### Keep simple

- Show first diagnostic in status before adding inline markers.
- Do not build a custom parser if `mpl-lang` API is insufficient.

## Dependency

```toml
mpl-lang = { git = "https://github.com/axiomhq/mpl", default-features = false }
```

Only add this after verifying the crate builds in this application.

## Tasks

1. Verify actual public API:
   - compile function name/signature,
   - required environment/spec arguments,
   - error variants,
   - span line/column or byte-offset representation.
2. Add a thin `mpl.rs` helper returning app-level diagnostics.
3. On `Enter`, compile first:
   - if compile succeeds, run API query,
   - if compile fails, update status and skip API request.
4. Optionally debounce diagnostics on editor changes if compile is fast and cheap.
5. Add inline markers only after span mapping is confirmed.

## Acceptance criteria

- Valid query still runs.
- Invalid local MPL query displays a diagnostic without making an API request.
- If `mpl-lang` cannot support this yet, app behavior remains unchanged and the blocker is documented.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Manual run with valid and invalid MPL.

## Confirmed (during implementation)

- `mpl-lang` v0.5 on crates.io exposes `pub fn compile<S>(query, system_params) -> Result<(Query, Warnings), CompileError>` directly (no WASM/fork required).
- `CompileError` implements `miette::Diagnostic`; the first labeled span gives `(byte_offset, byte_length)` plus an optional human-readable hint text.
- `compile` is fast enough to run synchronously on every `Enter` press; debounced on-edit validation is deferred.
- Default crate features needed: `clock` (kept on); `bincode` is unused here and disabled via `default-features = false` plus no extra features.

## Outcome

- New `src/mpl.rs` wraps `compile` into a `validate(&str) -> Result<(), Diagnostic>` API and converts byte offsets to 1-indexed `(line, column)`.
- `App::run_query` calls `mpl::validate` before any HTTP work; failures land in the status bar as `MPL error at L:C: <message>` and the request is skipped.
- Inline editor markers can reuse `Diagnostic::byte_offset/byte_length` later; the data path is in place.
