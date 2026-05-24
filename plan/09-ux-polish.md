# Step 09 — UX polish

## Incremental outcome

The core app is already functional; this step improves the daily interaction loop without changing
major architecture.

## User-visible improvement

- Clearer errors.
- Better loading feedback.
- Easier re-running and browsing results.
- More readable time axes and legends.

## Scope

### Add selectively

Implement only polish that improves the working app and does not destabilize query execution.

## Candidate tasks

1. Error overlay:
   - show last query/API error centered over graph,
   - dismiss with `Esc`.
2. Refresh:
   - `r` in Normal mode re-runs the current query.
3. Legend scrolling:
   - support many series,
   - show scroll indicator.
4. Better x-axis labels:
   - `HH:MM` for short ranges,
   - `MM-DD HH:MM` for longer ranges.
5. Time navigation:
   - `[` / `]` shifts the current time window only if the query/range model supports it safely.
6. Deployment switcher:
   - list configured deployments,
   - switch active client without restart.

## Acceptance criteria

- Existing query/edit/chart behavior remains intact.
- Added polish has clear key hints in status bar.
- Errors are easier to understand and dismiss.
- Large result sets are easier to inspect.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Manual run through edit → query → error → refresh → legend navigation.
