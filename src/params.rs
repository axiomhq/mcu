//! Host-supplied system parameters.
//!
//! Mirrors the `mplSystemParams` facet from `@axiomhq/mpl-codemirror`, but
//! **without** client-side substitution. System parameters like
//! `$__interval` are built-ins on the Axiom MetricsDB server — the server
//! resolves them at query time from the request's time window. The host's
//! only job here is to tell the *engine* "this `$<name>` is a declared
//! identifier of this type" so completions and diagnostics don't flag it
//! as undefined.
//!
//! Concretely:
//!
//!   * Each entry is sent to the engine via
//!     [`mpl_language_server::SystemParamSpec`] for `compute_diagnostics`
//!     and as a [`mpl_language_server::ParamItem`] for
//!     `compute_completions_with_params`.
//!   * The MPL query string sent to the API is the editor buffer verbatim;
//!     the server substitutes the value during evaluation.
//!
//! By convention all system-param names start with `__` (e.g. `__interval`)
//! so they cannot collide with user-declared `param $name: T;` entries.

/// One host-supplied parameter registration. Owns its identifier so the
/// registry can be mutated at runtime (e.g. when configuration changes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemParam {
    /// Name without the leading `$`. By convention starts with `__`.
    pub name: String,
    /// MPL type. Drives engine-side type gating during completion / diagnostic.
    pub kind: ParamKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    Duration,
    #[allow(dead_code)] // wired in once additional system params are registered
    String,
    #[allow(dead_code)]
    Int,
    #[allow(dead_code)]
    Float,
    #[allow(dead_code)]
    Bool,
    #[allow(dead_code)]
    Dataset,
    #[allow(dead_code)]
    Metric,
    #[allow(dead_code)]
    Regex,
}

/// Default registry. One entry: `__interval` of type `Duration`. The
/// Axiom MetricsDB server always resolves this from the query's time
/// window, so the host doesn't carry a value.
pub fn default_system_params() -> Vec<SystemParam> {
    vec![SystemParam {
        name: "__interval".to_string(),
        kind: ParamKind::Duration,
    }]
}

// ── user-declared params (the form the right-hand pane renders) ────────────

/// Validation state for one row in the params pane. Drives the marker
/// (✓ / ✗ / ○ / ⚠) and the row's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamStatus {
    /// Declared in the buffer, provided, and the value's MPL parse tree
    /// matches the declared `TerminalParamType`.
    Ok,
    /// Declared, provided, but the value isn't valid MPL or its grammar
    /// rule doesn't match the declared type.
    TypeMismatch,
    /// Declared (non-optional) and the user hasn't provided a value yet.
    NotSet,
    /// Declared `Option<T>` and not provided — fine, just informational.
    OptionalUnset,
    /// User provided a value for a name the buffer doesn't declare. The
    /// server will warn; we surface it here so the row isn't invisible.
    NotDeclared,
}

/// One row in the params pane.
#[derive(Debug, Clone)]
pub struct ParamRow {
    /// Without leading `$`.
    pub name: String,
    /// `None` when the row exists only because the user provided a value
    /// the buffer doesn't declare.
    pub declared_type: Option<String>,
    pub optional: bool,
    /// Current value from `app.cli_params`, if any.
    pub value: Option<String>,
    pub status: ParamStatus,
}
