# Step 10 — Feature parity with the upstream MPL codemirror integration

## Goal

Match `@axiomhq/mpl-codemirror` capabilities in this TUI. The web/codemirror
package consumes the `axiomhq/mpl` wasm bridge to deliver:

1. **Context-aware completions** — keywords, function lists, datasets,
   metrics, tags, params; with `apply` snippets, info text, and param-typed
   gating for `ifdef` bodies and filter values.
2. **Diagnostics with code actions** — wraps `compile()` and produces
   `{from, to, severity, message, help, actions[]}`. Actions are
   one-click text replacements (e.g. "Replace with `avg`").
3. **Tokenize for syntax highlighting** — span list of
   `{from, to, type}` covering 10 token types (keyword, variable,
   string, number, bool, regex, operator, punctuation, type, comment).
4. **Hover docs** — function signatures + keyword reference docs at a
   point.
5. **Signature help** — when inside a function call, surface
   `{label, args[], activeParam}`.
6. **Smart identifier rendering on accept** — `applyTextForIdent`
   honours backtick context, escapes `` ` `` and `\`, and only wraps
   when the name violates `plain_ident`.
7. **System params** — host-injected `$__interval` etc., merged with
   inline `param $x: T;` declarations during completion + diagnostic.

## Why this needs upstream cooperation

`/tmp/mpl-check/src/wasm/{completions,diagnostics,tokenize}.rs` contain the
engines. Their pure-Rust entry points are visibility-restricted:

| symbol                           | actual visibility | what we need |
|----------------------------------|-------------------|--------------|
| `compute_completions_with_params`| `pub(super)`      | `pub`        |
| `CompletionResult` + variants    | `pub(super)`      | `pub`        |
| `ParamItem`, `KeywordItem`, ...  | `pub(super)`      | `pub`        |
| `Span`                           | `pub(super)`      | `pub`        |
| `collect_tokens` + `Token`/`TokenType` | `pub(super)`| `pub`        |
| diagnostics builder + types      | `pub(super)`      | `pub`        |
| the entire `wasm` module         | `#[cfg(feature = "wasm")]` | gated behind a non-wasm feature too |

The public `#[wasm_bindgen]` wrappers take and return `JsValue`. `JsValue`
*is* a normal Rust struct and `wasm-bindgen` does compile for native targets
— but every operation on a `JsValue` (`from_str`, `is_null`,
`serde_wasm_bindgen::from_value`, etc.) calls `extern "C"` shims like
`__wbindgen_string_new` and `__wbindgen_object_clone_ref` that are
wasm-host imports. On native they are stubs that panic at the first
call site (verified empirically: `wasm_bindgen-0.2.121/src/lib.rs:1289`).

So the wrapper functions build but cannot run natively. We could in principle
embed a JS runtime (deno_core / rusty_v8) to back the shims, but that's a
~10 MB dependency and a JIT spin-up per call — absurd for a terminal UI.

That leaves: get past the wrappers and call the pure-Rust engine directly,
which today requires a visibility change.

### Upstream change (preferred path)

Open a PR against `axiomhq/mpl` adding a `native` cargo feature:

```toml
# Cargo.toml
[features]
default = []
wasm    = ["wasm-bindgen", "js-sys"]
native  = []  # new
```

```rust
// src/lib.rs
#[cfg(any(feature = "wasm", feature = "native"))]
pub mod engine;  // renamed from `wasm`, contains the pure-Rust logic

#[cfg(feature = "wasm")]
mod wasm_bridge;  // the `#[wasm_bindgen]` wrappers, separate from engine
```

Inside the renamed `engine` module, change `pub(super)` to `pub` for the
items in the table above. The `wasm_bridge` module imports them and wraps
into `JsValue`. No behaviour change; just visibility + module split.

This is the lowest-friction option: ~50 lines moved across files. The
user owns the repo, so it's a self-merge.

### Fallback if upstream change can't happen

Vendor the engine source into `mcu` under `src/mpl_engine/` and
strip `#[wasm_bindgen]` annotations. Drift risk vs upstream; would need a
sync script in `scripts/` and CI to flag divergence. Pursue only as a
short-term unblock.

## Native-side work (assuming the upstream PR lands)

Each numbered substep keeps the app running and passes
`cargo fmt && cargo clippy -D warnings && cargo test`.

### 10.1 — Engine consumption + `CompletionResult` plumbing

- Bump `mpl-lang` to a version exposing the `native` feature.
- Replace `src/completions.rs` byte-scanner with a thin call into
  `mpl_lang::engine::compute_completions_with_params(query, cursor, &params)`.
- Map `engine::CompletionResult` variants to a local
  `CompletionPayload` enum the UI consumes. Keep the existing
  `CompletionState { items, selected, replace_range_bytes }` for the
  popup; just enrich each item with `apply: Option<String>`, `info`,
  and `kind_icon`.
- Drop the local `STDLIB`-via-`serde_json` scrape; use the engine's
  cached `*_COMPLETIONS` directly through whatever public accessor the
  PR exposes.
- Tests: parity tests that mirror selected cases from
  `/tmp/mpl-check/src/wasm/completions/tests.rs`.

### 10.2 — Smart insert (apply text + ident escaping)

- Use `engine::CompletionResult.{from, to}` as the replace range.
- For `Dataset`/`Metric`/`Tag` items, apply the same
  `applyTextForIdent(name, in_backtick)` rule as
  `mpl-codemirror/src/completions.ts`. Detect `in_backtick` by reading
  the byte before `from` in the document.
- For keyword items, use the engine-supplied `apply` if present
  (e.g. `"where "`); otherwise insert the literal label.
- For `ifdef`, expand a snippet `ifdef($<name>) { where <cursor> }`
  with cursor placement (the textarea doesn't support tab stops, so
  pick a single insertion point after `where `).
- Replace the current `render_completion` in `src/completions.rs`.
- Tests: round-trip through the editor for dotted names, dashed names,
  backslash-containing names, already-opened-backtick partials.

### 10.3 — Diagnostics with code actions

- Swap `src/mpl.rs::validate` from `mpl_lang::compile` direct usage to
  `engine::compute_diagnostics(query, &system_params)` and surface the
  full `Vec<DiagnosticItem>` (multiple errors instead of just the first).
- Render diagnostics as an overlay or status-line list with
  `severity`-coloured chevrons.
- Add a key (e.g. `Ctrl-.`) that, when the cursor is on a diagnostic
  with `actions[]`, opens a small picker; selection applies the
  `{from, to, insert}` edit. This is the same "quick fix" UX the
  codemirror linter provides.
- Tests: error spans render at correct line/col; action application
  produces the expected edited buffer.

### 10.4 — Syntax highlighting via `engine::collect_tokens`

- Per render, call `engine::collect_tokens(text)` and convert spans
  to `ratatui::style::Style` per `TokenType`.
- Replace tui-textarea's plain-text rendering with styled-line
  rendering by walking the buffer and applying the token stream.
  (`tui-textarea` exposes `set_line_number_style`,
  `set_cursor_line_style`, etc., but not per-token styling out of the
  box; we'll need a custom render path or feed pre-styled `Line`s
  into a `Paragraph` for the editor pane.)
- Token → style mapping mirrors the CSS classes in
  `mpl-codemirror/src/language.ts`: keyword=cyan, variable=default,
  string=green, number=yellow, bool=magenta, regex=magenta,
  operator/punctuation=dim, type=cyan-italic, comment=dim-italic.
- Tests: snapshot of styled spans for a representative query.

### 10.5 — Hover docs and signature help

- Bind `K` (Normal mode, vim convention) to show hover info for the
  symbol under the cursor: keyword doc from the same table as
  `mpl-codemirror/src/hover.ts`, or function info via
  `STDLIB.lookup_function(label)` (already public).
- Inside a function call (open `(` to the left without a matching
  `)`), render signature help in the status line:
  `avg(value: float)` with the active arg highlighted. Port the
  scan logic from `mpl-codemirror/src/signature-help.ts::findCallContext`.
- Both popups close on any key that isn't pure navigation.

### 10.6 — Tag completion

- When the engine returns `CompletionResult::Tag { dataset, metric }`,
  hit the metrics-info endpoint
  (`/v1/query/metrics/info/datasets/<ds>/metrics/<m>/tags`) on demand
  (with the same disk cache used for datasets/metrics).
- Suggest tag names; if the user has typed `<tag> == "<partial>"`,
  follow up with a tag-values request
  (`.../tags/<tag>/values`).
- Cache key: `(dataset, metric)` for tags, `(dataset, metric, tag)`
  for values, with the same 24h discovery window logic.

### 10.7 — System params + inline `param` support

- Surface a way for the user to declare `$__interval`-style params
  (config file, or `:set param ...` ex-command) and feed them into
  every `compute_completions_with_params` / `compute_diagnostics`
  call so `ifdef`, value-position params, and metric-position params
  Just Work.
- Already covered by the engine; UI work is the config + plumbing.

## Sequencing

Land 10.1 first; it deletes the most code and unlocks the rest. Then
10.2 → 10.3 → 10.4 in that order (each is a separable feature on top
of 10.1). 10.5, 10.6, 10.7 are independent and can land in any order
after that.

If the upstream PR is gated on review/release, do **10.4 first
locally** by porting just `tokenize::collect_tokens` into our crate
(it's 153 lines, the smallest and most self-contained), then revert
that copy once `mpl-lang` exposes it.

## Out of scope for now

- `replace` / `join` pipe rules (the upstream engine intentionally
  omits them from completion until parser support stabilises).
- A full LSP — none of this needs to leave the mcu process.

## Tracking

After each substep:

1. Verify with the standard trio (`fmt`, `clippy -D warnings`, `test`).
2. Update this file's status table:

   | Sub | Title                              | Status |
   |-----|------------------------------------|--------|
   | 10.0| Upstream `native` feature in mpl   | done    |
   | 10.1| Engine consumption + plumbing      | done    |
   | 10.2| Smart insert / apply / escaping    | done    |
   | 10.3| Diagnostics + code actions         | done    |
   | 10.4| Syntax highlighting via tokenize   | done    |
   | 10.5| Hover + signature help             | done    |
   | 10.6| Tag + tag-value completion         | done    |
   | 10.7| System params + inline `param`     | superseded — server resolves |

3. Also refresh the top-level `PLAN.md` inventory.
