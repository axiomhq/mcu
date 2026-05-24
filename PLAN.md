# metrics-tui — Implementation Plan

This plan is split into incremental steps under [`plan/`](plan/). Each step should leave the
application fully functional on its own: buildable, runnable, and improved over the previous step.

## Steps

1. [Step 01 — Layout and app shell](plan/01-layout-and-app-shell.md)
2. [Step 02 — Modal editor](plan/02-modal-editor.md)
3. [Step 03 — Chart data model](plan/03-chart-data-model.md)
4. [Step 04 — Config and dataset discovery](plan/04-config-and-dataset-discovery.md)
5. [Step 05 — Async query execution](plan/05-async-query-execution.md)
6. [Step 06 — MPL diagnostics](plan/06-mpl-diagnostics.md)
7. [Step 07 — Completions](plan/07-completions.md)
8. [Step 08 — Syntax highlighting](plan/08-syntax-highlighting.md)
9. [Step 09 — UX polish](plan/09-ux-polish.md)
10. [Step 10 — MPL feature parity with codemirror integration](plan/10-mpl-feature-parity.md)
11. [Step 11 — Viz kinds + time-series variants](plan/11-viz-kinds-and-time-series-variants.md)
12. [Step 12 — Statistic + Top list](plan/12-statistic-and-top-list.md)
13. [Step 13 — Pie + Heatmap](plan/13-pie-and-heatmap.md)
14. [Step 14 — Table](plan/14-table.md)
15. [Step 15 — Log stream](plan/15-log-stream.md)
16. [Step 16 — Monitor list + Note + Spacer](plan/16-monitor-list-note-spacer.md)
17. [Step 17 — Dashboard file format + load/save](plan/17-dashboard-file-format-and-crud.md)
18. [Step 18 — Dashboard grid view + tile editing](plan/18-dashboard-grid-layout-and-editing.md)

Steps 11–16 add the remaining Axiom dashboard element types (bar / area /
scatter / statistic / top list / pie / heatmap / table / log stream /
monitor list / note / spacer) on top of the existing line chart.
Step 11 also introduces the canonical `Dashboard { tiles }` internal
model that steps 17 and 18 reuse to load, edit, and save real
multi-tile Axiom dashboards.

## Workflow

After completing any step, print the step inventory: a table listing every step with its status
(`done`, `next`, `pending`) and a one-line link to the step file. This keeps progress visible
without having to re-read the plan.

## Cross-cutting assumptions to validate early

- Confirm exact Axiom Metrics query endpoint, request body, response schema, and discovery endpoints.
- Confirm `~/.axiom.toml` field names and deployment-selection behavior.
- Confirm `mpl-lang` exported compile API, error/span types, stdlib access, and tokenizer/completion
  availability.
- Confirm `tui-textarea` APIs for cursor movement, undo/redo, block styling, and token-level styling.

## Baseline repository state

- `src/main.rs` is currently a single-file ratatui sine-chart demo that quits on `q`.
- `Cargo.toml` currently contains `ratatui`, `crossterm`, and `anyhow` only.
- Step 01 should preserve the demo app's basic behavior while introducing the real layout shell.
