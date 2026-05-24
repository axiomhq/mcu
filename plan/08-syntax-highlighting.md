# Step 08 — Syntax highlighting

> **Closed (subsumed by Step 10.4).** The engine in `mpl-language-server`
> ships a `collect_tokens` function that produces span+kind data covering
> 9 token types; we consume it from [src/highlight.rs](../src/highlight.rs)
> and render styled `Line`s in [src/ui.rs](../src/ui.rs)'s `draw_editor`.
> The original Step 08 plan (a from-scratch tokeniser) is no longer
> necessary; this file is kept for historical context only.


## Incremental outcome

The editor gains visual syntax styling if the rendering path supports it. If token-level styling is
not practical with `tui-textarea`, the app remains fully functional and this step can be deferred.

## User-visible improvement

- MPL keywords, strings, numbers, comments, and operators are visually distinct.

## Scope

### Add

- Tokenization/highlighting helper.
- Styled editor rendering path only if it does not compromise editing correctness.

### Keep simple

- Highlighting is cosmetic; never use it for validation.
- Prefer a public `mpl-lang` tokenizer. Use a small fallback lexer only for obvious token classes.

## Token style target

| Token       | Style             |
|-------------|-------------------|
| Keyword     | blue, bold        |
| String      | green             |
| Number      | yellow            |
| Bool        | magenta           |
| Regexp      | red               |
| Operator    | cyan              |
| Identifier  | default/white     |
| Type        | light magenta     |
| Punctuation | dim               |
| Comment     | dark gray, italic |

## Tasks

1. Verify whether `tui-textarea` can render token-level spans.
2. If yes:
   - implement `highlight(query) -> styled lines`,
   - render styled editor content while preserving cursor behavior.
3. If no:
   - either defer highlighting, or
   - use a custom display overlay only if it remains robust.
4. Prefer `mpl-lang` tokenizer if exposed.
5. Fallback lexer may cover only:
   - keywords,
   - quoted strings,
   - numbers,
   - comments,
   - basic operators/punctuation.

## Acceptance criteria

- Editing behavior from Step 02 still works.
- Highlighting updates after edits.
- Invalid/incomplete queries do not break rendering.
- If deferred, the app still builds/runs and the reason is documented.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Manual run: edit strings, keywords, incomplete quotes, and comments.
