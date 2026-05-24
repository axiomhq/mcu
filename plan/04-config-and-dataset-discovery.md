# Step 04 — Config and dataset discovery

## Incremental outcome

The app can read Axiom config and make one authenticated discovery request. Query execution is not
implemented yet, but credentials and API basics are proven.

## User-visible improvement

- A Normal-mode key can fetch datasets.
- Status bar shows success count or a clear configuration/API error.
- UI remains responsive while the request runs.

## Scope

### Add

- Async runtime/channel plumbing.
- `config.rs` for loading `~/.axiom.toml`.
- `axiom.rs` with minimal client and `list_datasets()`.

### Keep simple

- Fetch only datasets in this step.
- Do not decode metrics query responses yet.
- Keep config format isolated so it can be adjusted after validation.

## Dependencies

```toml
tokio   = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
serde   = { version = "1", features = ["derive"] }
toml    = "0.8"
```

## Tasks

1. Define config structs for the expected `~/.axiom.toml` shape.
2. Load config at startup or lazily on first discovery command.
3. Create an Axiom client with:
   - base URL,
   - auth token header,
   - org header if required by confirmed API behavior.
4. Add `D` key in Normal mode to fetch datasets.
5. Run the request in a spawned async task and return result through a channel.
6. Show status:
   - missing config,
   - auth/API error with HTTP status and body snippet,
   - success with dataset count.
7. Record confirmed endpoint/request/response details in comments or this step file.

## Acceptance criteria

- Without config, `D` reports a helpful error and app keeps running.
- With valid config, `D` fetches datasets and shows count.
- Network/API failures do not crash the app.
- UI input still works while request is in flight.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets`
- `cargo test`
- Manual run without config.
- Manual run with valid config if credentials are available.

## Confirmed (during implementation)

- Config path: `~/.axiom.toml`.
- Config shape: optional top-level `active_deployments = "<name>"`, plus one or more
  `[deployments.<name>]` tables with `url`, `token`, `org_id`.
- Endpoint: `GET {url}/v1/datasets`.
- Required headers: `Authorization: Bearer <token>`, `X-Axiom-Org-Id: <org_id>`,
  `Accept: application/json`.
- Response: JSON array of dataset objects. Each object has at least `name`, plus fields like
  `description`, `id`, `kind`, `edgeDeployment`, `retentionDays`, etc. Extra fields are ignored
  by the decoder.
- Verified live against the configured deployment (HTTP 200, 7 datasets returned).
