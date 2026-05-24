# Step 05 — Async query execution

## Incremental outcome

Pressing `Enter` in Normal mode runs the editor query against Axiom and plots returned time-series
results. The app becomes useful for its main purpose.

## User-visible improvement

- Current editor query can be executed.
- Chart updates with real data.
- Status shows running/success/error.
- Previous good chart remains visible if a new query fails.

## Scope

### Add

- Query request method in `axiom.rs`.
- Response decoding into internal `Series`.
- Query request IDs to ignore stale responses.
- Spinner/status while running.

### Keep simple

- Validate the query only through the API unless `mpl-lang` compile is already easy to add.
- Use the first confirmed response shape; avoid over-general decoding until needed.

## Tasks

1. Confirm exact Metrics/MPL query endpoint and body.
2. Add query state:

   ```rust
   enum QueryState {
       Idle,
       Running { id: u64, started_at: Instant },
       Error(String),
   }
   ```

3. On `Enter` in Normal mode:
   - read editor buffer,
   - create next query ID,
   - set running state,
   - spawn async query task,
   - send `QueryFinished { id, result }` to app.
4. Decode successful response into `Vec<Series>`.
5. Assign stable colors from a fixed palette.
6. Ignore responses whose ID is older than the current running query.
7. Keep old series on error and show the error in status.

## Acceptance criteria

- Valid query updates graph and legend with real series.
- Invalid query shows an error but does not clear old data.
- Starting multiple queries quickly cannot let older responses overwrite newer results.
- UI remains responsive during requests.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Unit test stale-response handling if practical.
- Manual run with a known-good query and a known-bad query.

## Confirmed (during implementation)

Source: `axiomhq/skills` repo, `skills/query-metrics`.

- Endpoint: `POST {edge_url}/v1/query/_mpl`.
- Required headers: `Authorization: Bearer`, `X-Axiom-Org-Id`, `Content-Type: application/json`,
  `Accept: application/json+metrics.v2`.
- Request body:
  ```json
  {
    "apl": "<MPL query>",
    "startTime": "now-1h",
    "endTime": "now",
    "queryEdgeDeployment": "cloud.us-east-1.aws"
  }
  ```
  Note: the field is literally named `apl` but contains MPL syntax. Time strings accept either
  RFC3339 or relative expressions like `now`, `now-1h`, `now-1d`.
- Edge URL resolution from each dataset's `edgeDeployment` field:
  - `cloud.us-east-1.aws` → `https://us-east-1.aws.edge.axiom.co`
  - `cloud.eu-central-1.aws` → `https://eu-central-1.aws.edge.axiom.co`
  - missing/null → fall back to the deployment URL in `~/.axiom.toml`.
- Response shape (200 OK):
  ```json
  {
    "metadata": {},
    "series": [
      {
        "metric": "temp",
        "tags": {"room": "Eingang", "unit": "C"},
        "start": 1764547200,
        "resolution": 3600,
        "data": [18.24, null, 18.11]
      }
    ]
  }
  ```
  Each sample timestamp is `start + i*resolution` (unix seconds). Gaps appear as JSON `null`.
- Errors are JSON `{"code": 4xx, "message": "..."}` with status code in the response.
