//! `:trace <id>` async fetch + ladder handler.
//!
//! Two responsibilities:
//!
//! 1. **Dispatch** — given a resolved `(trace_id, dataset,
//!    deployment_override)` tuple, build the APL query, spawn the
//!    background task, and stash a [`PendingTraceFetch`] so the
//!    response handler can wire the result back into app state.
//!
//! 2. **Ladder + decode** — drain the
//!    [`AppEvent::TraceFetchFinished`] event, walk the search
//!    window wider on empty results, and on success build a
//!    [`TraceView`] + flip into [`ViewMode::Trace`].
//!
//! Why a separate event variant (not [`AppEvent::AplQueryFinished`]):
//! editor queries (`:r`) and trace fetches both go through the
//! `_apl` endpoint, so they'd share the response shape. But they
//! also share **the same response semantics** at the event level
//! — bumping `last_query_id` for one would invalidate the other.
//! A dedicated event keeps the two id namespaces fully independent
//! so an interleaved `:trace abc` + `:r` doesn't silently cancel
//! either.
//!
//! Why a fresh [`AxiomClient`] per fetch (not the cached
//! `App.client`): trace fetches can target a different
//! `deployment_override` than the one the editor's client was
//! built with. Building a fresh client on dispatch is one TLS
//! handshake per invocation — negligible for a deliberate
//! interactive operation, and it avoids an entire class of
//! "wrong-edge dispatch" bugs.

use super::*;
use crate::axiom::Client as AxiomClient;
use crate::config::Config;
use crate::trace::TraceModel;
use crate::viz::apl_decode;

impl App {
    /// Entry point from `:trace <id>` (replacing the placeholder
    /// in `cmd_trace`). Resolves dataset + deployment with the
    /// documented fallback chain, records the resolution, and
    /// kicks off the first (`now-1h`) ladder window.
    ///
    /// Returns `Err(String)` so the caller can route the message
    /// through `set_error` — config / settings failures show up
    /// as a status overlay, not a panic.
    pub fn start_trace_fetch(
        &mut self,
        trace_id: String,
        dataset_arg: Option<String>,
        deployment_arg: Option<String>,
    ) -> Result<(), String> {
        if trace_id.trim().is_empty() {
            return Err("trace id is empty".to_string());
        }

        // ---- Dataset resolution ----------------------------------
        //
        // Precedence: explicit `dataset=` arg → last in-session
        // dataset → `Settings.trace.dataset`. An unset chain is a
        // clear user error — we don't want to silently pick an
        // arbitrary dataset and confuse the user about why a trace
        // didn't show up.
        let dataset = match dataset_arg {
            Some(d) if !d.trim().is_empty() => d.trim().to_string(),
            _ => match self.last_trace_dataset.clone() {
                Some(d) => d,
                None => match self.settings.read().trace().dataset.clone() {
                    Some(d) => d,
                    None => {
                        return Err(
                            "no trace dataset; set with `:trace set dataset=NAME`".to_string()
                        );
                    }
                },
            },
        };
        // Strip any surrounding quotes the user (or a prior sticky
        // value) baked into the name. Done here — the single
        // resolution chokepoint — so the cleaned name flows into both
        // `pending.dataset` (the APL literal) and the sticky
        // `last_trace_dataset`, which re-cleans itself on the next
        // call too. See `helpers::normalize_dataset_name`.
        let dataset = crate::app::helpers::normalize_dataset_name(&dataset);
        if dataset.is_empty() {
            return Err("trace dataset is empty".to_string());
        }

        // ---- Deployment resolution -------------------------------
        //
        // Precedence: explicit `deployment=` arg →
        // `Settings.trace.deployment` → `None` (let `Config.select`
        // pick the active deployment when the client is built).
        // Empty values collapse to `None` so `deployment=` with no
        // value doesn't silently flip into the global default.
        let deployment_override = deployment_arg
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| self.settings.read().trace().deployment.clone());

        // ---- Stash + dispatch ------------------------------------
        self.trace_query_counter = self.trace_query_counter.wrapping_add(1);
        let pending = PendingTraceFetch {
            query_id: self.trace_query_counter,
            trace_id: trace_id.trim().to_string(),
            dataset: dataset.clone(),
            deployment_override: deployment_override.clone(),
            window: TraceFetchWindow::Hour,
        };
        // Remember the dataset so subsequent `:trace <id>` calls
        // stick to it. Done before the dispatch — even if the
        // dispatch fails (config error, etc.) we want to honour
        // the user's intent.
        self.last_trace_dataset = Some(dataset);
        self.pending_trace_fetch = Some(pending);
        // If the very first dispatch fails (e.g. no `~/.axiom.toml`,
        // bad deployment), clear the pending fetch we just stashed —
        // no background task was spawned, so leaving it set would make
        // a later `Esc` falsely report "trace fetch cancelled".
        if let Err(e) = self.dispatch_trace_window() {
            self.pending_trace_fetch = None;
            return Err(e);
        }
        Ok(())
    }

    /// Spawn the background query for the current pending fetch's
    /// window. Called once on `start_trace_fetch` and again from
    /// the ladder handler when the previous window came up empty.
    fn dispatch_trace_window(&mut self) -> Result<(), String> {
        let pending = self.pending_trace_fetch.clone().ok_or_else(|| {
            "internal: dispatch_trace_window called with no pending fetch".to_string()
        })?;

        // Build a fresh client — see module docs for the rationale.
        let cfg = self
            .resolve_config()
            .map_err(|e| format!("trace client: {e}"))?;
        let client = build_trace_client(&cfg, pending.deployment_override.as_deref())
            .map_err(|e| format!("trace client: {e}"))?;

        // APL query — the dataset goes through `serde_json::to_string`
        // to handle quotes/backticks safely; the trace_id ditto.
        // Sort by `_time asc` so the response is in ingest order
        // (the decoder re-derives `start_ns` from `_time`, so the
        // sort is a cheap server-side guarantee rather than a hard
        // requirement).
        let dataset_lit =
            serde_json::to_string(&pending.dataset).map_err(|e| format!("escape dataset: {e}"))?;
        let trace_lit = serde_json::to_string(&pending.trace_id)
            .map_err(|e| format!("escape trace_id: {e}"))?;
        let apl = format!("[{dataset_lit}] | where trace_id == {trace_lit} | sort by _time asc");

        let start = pending.window.as_relative_start().to_string();
        let end = "now".to_string();
        let tx = self.events_tx.clone();
        let query_id = pending.query_id;
        self.runtime.spawn(async move {
            let result = crate::app::helpers::run_apl_query_task(&client, &apl, &start, &end).await;
            let _ = tx.send(AppEvent::TraceFetchFinished { query_id, result });
        });

        self.status = format!(
            "searching trace {} … ({})",
            short_trace_label(&pending.trace_id),
            pending.window.label()
        );
        Ok(())
    }

    /// Drain a `TraceFetchFinished` event.
    ///
    /// Three paths:
    /// 1. **Stale**     — `query_id` doesn't match the current
    ///    pending. Drop silently; a newer `:trace` already
    ///    superseded this fetch.
    /// 2. **Empty**     — walk the window wider; if no wider
    ///    window remains, surface "not found" and clear pending.
    /// 3. **Non-empty** — decode via `to_trace_model`, build a
    ///    [`TraceView`], flip into [`ViewMode::Trace`], clear
    ///    pending.
    ///
    /// Errors from the HTTP / SDK layer surface as `set_error`;
    /// decode errors (malformed response, missing required
    /// column) likewise. Both clear `pending_trace_fetch` so
    /// `Esc` afterwards doesn't try to cancel a fetch that
    /// already failed.
    pub(super) fn handle_trace_fetch_finished(
        &mut self,
        query_id: u64,
        result: anyhow::Result<crate::axiom::AplQueryResult>,
    ) {
        // (1) Stale. A second `:trace <id>` already bumped the
        // counter so this event refers to a fetch the user
        // abandoned. Drop without touching state.
        let still_current = self
            .pending_trace_fetch
            .as_ref()
            .is_some_and(|p| p.query_id == query_id);
        if !still_current {
            return;
        }

        let resp = match result {
            Ok(r) => r,
            Err(e) => {
                self.pending_trace_fetch = None;
                self.set_error(format!("trace fetch: {e}"));
                return;
            }
        };

        // Inspect the response shape — if the table is missing or
        // has zero rows, treat it as empty and possibly bump the
        // ladder. We can't lean on `to_trace_model` for this
        // because it returns `EmptyTrace` *either* when tables are
        // empty *or* when every row was dropped (empty span_id);
        // both cases want the same ladder behaviour, but only the
        // former is a legitimate "keep searching" signal.
        let row_count = resp
            .tables
            .first()
            .and_then(|t| t.columns().first())
            .map(Vec::len)
            .unwrap_or(0);
        if row_count == 0 {
            self.advance_or_give_up();
            return;
        }

        // (3) Non-empty — decode + transition.
        let pending = self
            .pending_trace_fetch
            .clone()
            .expect("still_current implies Some");
        let model = match apl_decode::to_trace_model(
            &resp,
            pending.trace_id.clone(),
            pending.dataset.clone(),
        ) {
            Ok(m) => m,
            Err(e) => {
                self.pending_trace_fetch = None;
                self.set_error(format!("trace decode: {e}"));
                return;
            }
        };
        self.enter_trace_view(model);
    }

    /// Empty-result ladder step. Bumps to the next wider window
    /// and re-dispatches; if no wider window remains, surfaces
    /// "trace not found" and clears pending.
    fn advance_or_give_up(&mut self) {
        let Some(pending) = self.pending_trace_fetch.as_mut() else {
            return;
        };
        match pending.window.next() {
            Some(next) => {
                pending.window = next;
                if let Err(e) = self.dispatch_trace_window() {
                    self.pending_trace_fetch = None;
                    self.set_error(format!("trace fetch (ladder): {e}"));
                }
            }
            None => {
                let trace_id = pending.trace_id.clone();
                let dataset = pending.dataset.clone();
                self.pending_trace_fetch = None;
                self.set_error(format!(
                    "trace `{}` not found in `{dataset}` within the last 30 days",
                    short_trace_label(&trace_id)
                ));
            }
        }
    }

    /// Install a freshly-decoded [`TraceModel`] as the active
    /// view. Captures the current `view_mode` as the return
    /// target so `Esc` lands the user back where they came from.
    /// Idempotent re-entry: a second `:trace <id>` while already
    /// in `ViewMode::Trace` swaps the model in place and keeps
    /// the original `return_mode` so `Esc` still pops back to
    /// the pre-trace world (not to the previously-loaded trace).
    fn enter_trace_view(&mut self, model: TraceModel) {
        let return_mode = self
            .trace_view
            .as_ref()
            .map(|v| v.return_mode)
            .unwrap_or(self.view_mode);
        let trace_label = short_trace_label(&model.trace_id);
        let span_count = model.spans.len();
        let err_count = model.spans.iter().filter(|s| s.is_error).count();
        let dataset = model.dataset.clone();
        self.trace_view = Some(TraceView::new(model, return_mode));
        self.pending_trace_fetch = None;
        self.view_mode = ViewMode::Trace;
        // `set_focus` enforces `trace_view.is_some()` — which we
        // just set — so this always succeeds.
        self.set_focus(Pane::TraceTree);
        self.status =
            format!("trace {trace_label} · {span_count} span(s) · {err_count} err · {dataset}");
    }
}

/// Build a fresh AxiomClient against the deployment named by
/// `deployment` (or the user's active default when `None`),
/// resolving against the supplied `cfg`. The caller passes
/// [`App::resolve_config`]'s result so tests can inject a synthetic
/// config instead of reading `~/.axiom.toml`.
fn build_trace_client(cfg: &Config, deployment: Option<&str>) -> anyhow::Result<AxiomClient> {
    let (_name, dep) = cfg.select(deployment)?;
    AxiomClient::new(dep)
}

/// Shorten a trace id for log/status display. Real ids are 16-32
/// hex chars; we keep the first 12 (matches what `:traces ls` will
/// show in step 25 for column alignment).
fn short_trace_label(id: &str) -> String {
    if id.chars().count() <= 16 {
        id.to_string()
    } else {
        format!("{}…", crate::util::take_chars(id, 12))
    }
}
