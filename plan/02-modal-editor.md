# Step 02 — Modal editor

## Incremental outcome

The editor becomes usable for writing multi-line MPL queries in Normal/Insert modes. Query execution
is still a stub, so the app remains functional without Axiom credentials.

## User-visible improvement

- Insert mode allows editing text.
- Normal mode supports basic vim-like movement/actions.
- Pressing `Enter` in Normal mode reports that query execution is not implemented yet.

## Scope

### Add

- `Mode` state: `Normal` / `Insert`.
- Editor key routing.
- Helper to read the full editor buffer as a `String`.

### Keep simple

- Implement only a small vim subset.
- Do not add completions, diagnostics, or highlighting.

## Tasks

1. Extend app state:

   ```rust
   enum Mode { Normal, Insert }
   ```

2. Insert mode behavior:
   - `Esc`: return to Normal mode,
   - all other editing keys go to `TextArea::input()`.
3. Normal mode behavior:
   - `i`: enter Insert,
   - `a`: move cursor right then enter Insert,
   - `h/j/k/l`: cursor movement,
   - `x`: delete character,
   - `dd`: delete current line,
   - `u`: undo if supported by `tui-textarea`,
   - `Ctrl-r`: redo if supported by `tui-textarea`,
   - `Enter`: set status to `query execution not implemented`.
4. Global behavior:
   - `Tab`: cycle focus,
   - `q`: quit only in Normal mode.
5. Status bar shows mode and focused pane.

## Acceptance criteria

- App starts in Normal mode.
- `i` enters Insert mode and text can be typed.
- `Esc` returns to Normal mode.
- Multi-line editing works.
- Normal-mode movement works.
- `Enter` updates status without blocking or crashing.
- `q` does not quit while typing in Insert mode.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Manual run: edit a multi-line query, switch modes, verify quit behavior.
