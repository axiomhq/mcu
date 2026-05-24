# Step 01 — Layout and app shell

## Incremental outcome

The existing sine-chart demo becomes a structured TUI shell with graph, legend, editor, and status
areas. The app is still fully runnable without network access or MPL dependencies.

## User-visible improvement

- Three-pane layout appears instead of a single chart.
- Focus can move between panes.
- Status bar shows mode, focus, and key hints.
- The existing demo chart still renders.

## Scope

### Add

- `tui-textarea` dependency.
- `App` state with focus and quit flag.
- Basic module split:
  - `main.rs`: terminal setup/teardown and app bootstrap.
  - `app.rs`: state and event handling.
  - `ui.rs`: layout and rendering.
  - `editor.rs`: editor widget initialization.

### Keep simple

- Use the current sine data as demo chart data.
- Use one static legend entry.
- No API, no async runtime, no MPL compile/highlight yet.

## Target layout

```
┌─────────────────────────────────────────────────┬──────────────────┐
│                  GRAPH PANE                     │   LEGEND PANE    │
│              demo time-series chart             │  ● sin(x)        │
├─────────────────────────────────────────────────┴──────────────────┤
│  EDITOR PANE                                                        │
│  sample MPL query                                                   │
└────────────────────────────────────────────────────────────────────┘
 [NORMAL] focus: graph | Tab focus | q quit
```

## Tasks

1. Add dependency:

   ```toml
   tui-textarea = { version = "0.7", features = ["crossterm"] }
   ```

2. Create app state:

   ```rust
   enum Focus { Graph, Legend, Editor }

   struct App {
       focus: Focus,
       editor: TextArea<'static>,
       should_quit: bool,
   }
   ```

3. Render layout:
   - terminal split into main body + one-line status bar,
   - body split into 75% top and 25% editor,
   - top split into 80% graph and 20% legend.
4. Highlight the focused pane border.
5. Implement keys:
   - `Tab`: cycle graph → legend → editor → graph,
   - `Esc`: focus graph,
   - `q`: quit.

## Acceptance criteria

- `cargo run` starts the TUI.
- App shows graph, legend, editor, and status bar.
- `Tab` visibly changes focused border.
- `Esc` returns focus to graph.
- `q` exits and restores the terminal.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Manual run: start app, press `Tab`, `Esc`, `q`.
