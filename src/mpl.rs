//! MPL diagnostics powered by `mpl_language_server::compute_diagnostics`.
//!
//! Returns the full list of errors, warnings, info, and hints the engine
//! produces — including engine-supplied quick-fix [`DiagnosticAction`]s
//! that the host can apply as a single `(span, insert)` text edit. The
//! caller drives recomputation; this module is pure.

use std::collections::HashMap;

use mpl_lang::visitor::QueryWalker as _;
use mpl_language_server::{
    DiagnosticItem, Severity as EngineSeverity, SystemParamSpec, compute_diagnostics,
    to_compile_params,
};

use crate::params::{ParamKind, SystemParam};

/// Severity, mirroring `mpl_language_server::Severity` so callers don't have
/// to depend on the engine crate directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn is_error(self) -> bool {
        matches!(self, Severity::Error)
    }
}

/// A one-click quick-fix the engine suggests for a diagnostic. Apply by
/// replacing `[byte_offset, byte_offset + byte_length)` in the query buffer
/// with `insert`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticAction {
    pub name: String,
    pub byte_offset: usize,
    pub byte_length: usize,
    pub insert: String,
}

/// One diagnostic mapped to editor coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub help: Option<String>,
    pub byte_offset: usize,
    pub byte_length: usize,
    /// 1-indexed line and column derived from `byte_offset`.
    pub line: usize,
    pub column: usize,
    pub actions: Vec<DiagnosticAction>,
}

impl Diagnostic {
    /// `"<severity> at line:col: message"` — used in status bar / overlays.
    pub fn header(&self) -> String {
        let label = match self.severity {
            Severity::Error => "MPL error",
            Severity::Warning => "MPL warning",
            Severity::Info => "MPL info",
            Severity::Hint => "MPL hint",
        };
        format!("{label} at {}:{}: {}", self.line, self.column, self.message)
    }

    /// True iff `byte` falls inside `[byte_offset, byte_offset + byte_length)`.
    /// For zero-length spans the range is treated as inclusive at both ends so
    /// the cursor can "be on" an empty span (point markers).
    pub fn span_contains(&self, byte: usize) -> bool {
        if self.byte_length == 0 {
            byte == self.byte_offset
        } else {
            byte >= self.byte_offset && byte < self.byte_offset + self.byte_length
        }
    }
}

/// Run the engine over `query` with the host's system params in scope.
/// Returns an empty `Vec` when the query is clean.
pub fn analyze(query: &str, system_params: &[SystemParam]) -> Vec<Diagnostic> {
    let specs = to_engine_specs(system_params);
    let params: HashMap<_, _> = to_compile_params(&specs);
    compute_diagnostics(query, &params)
        .into_iter()
        .map(|item| convert(query, item))
        .collect()
}

/// Stable hex hash of a query, used as a cache key (e.g. to remember the
/// user's chosen legend-label tags per query). Computed from the compiled
/// `Query` AST so comments and whitespace are normalized for free, then
/// walked with [`NormalizeForHashVisitor`] to strip time windows and
/// interval clauses (so `last 1h` vs `last 24h`, or `align to 1m` vs
/// `align to 5m`, hash identically — they describe the same metric
/// surface, just at different resolutions). Falls back to whitespace-
/// normalized source text when compile fails (in-progress / broken
/// queries) so we still get a deterministic key.
///
/// Not normalized: filter order (compile preserves source order), and
/// function-level durations like `rate(5m)` (those change the value
/// semantics, not just the time window).
pub fn query_hash(query: &str, system_params: &[SystemParam]) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let payload = normalized_payload(query, system_params);
    let mut h = DefaultHasher::new();
    payload.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Visible for tests: the normalized string that `query_hash` actually
/// hashes. Useful for debugging cache-key mismatches.
pub fn normalized_payload(query: &str, system_params: &[SystemParam]) -> String {
    let specs = to_engine_specs(system_params);
    let params: HashMap<_, _> = to_compile_params(&specs);
    match mpl_lang::compile(query, params) {
        Ok((mut q, _warnings)) => {
            let _ = NormalizeForHashVisitor.walk(&mut q);
            serde_json::to_string(&q).unwrap_or_else(|_| normalize_text(query))
        }
        Err(_) => normalize_text(query),
    }
}

/// Walker that clears time-window fields on the compiled `Query` so the
/// hash of `home:temp last 1h` matches `home:temp last 24h`. Targets:
///
/// - `Source.time` — the `last 1h` / `from..to` window.
/// - `Align.time` — the `align to 1m` interval.
/// - `BucketBy.time` — the bucketing interval.
///
/// Function-level durations (e.g. `rate(5m)`) are intentionally left
/// alone since they change the metric's value semantics.
struct NormalizeForHashVisitor;

impl mpl_lang::visitor::QueryVisitor for NormalizeForHashVisitor {
    type Error = std::convert::Infallible;

    fn visit_source(
        &mut self,
        source: &mut mpl_lang::query::Source,
    ) -> Result<mpl_lang::visitor::VisitRes, Self::Error> {
        source.time = None;
        Ok(mpl_lang::visitor::VisitRes::Walk)
    }

    fn visit_align(
        &mut self,
        align: &mut mpl_lang::query::Align,
    ) -> Result<mpl_lang::visitor::VisitRes, Self::Error> {
        align.time = None;
        Ok(mpl_lang::visitor::VisitRes::Walk)
    }

    fn visit_bucket_by(
        &mut self,
        bucket_by: &mut mpl_lang::query::BucketBy,
    ) -> Result<mpl_lang::visitor::VisitRes, Self::Error> {
        bucket_by.time = None;
        Ok(mpl_lang::visitor::VisitRes::Walk)
    }
}

impl mpl_lang::visitor::QueryWalker for NormalizeForHashVisitor {}

fn normalize_text(query: &str) -> String {
    // Collapse all whitespace to single spaces. MPL treats newlines as
    // insignificant; it has no block comments. Line comments (`// ...`)
    // aren't stripped here — use the compile-based hash when you need
    // that. This fallback exists only for queries the engine can't
    // parse, where comment-stripping isn't well-defined anyway.
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn convert(query: &str, item: DiagnosticItem) -> Diagnostic {
    let (line, column) = byte_offset_to_line_col(query, item.span.from);
    Diagnostic {
        severity: map_severity(item.severity),
        message: item.message,
        help: item.help,
        byte_offset: item.span.from,
        byte_length: item.span.to.saturating_sub(item.span.from),
        line,
        column,
        actions: item
            .actions
            .into_iter()
            .map(|a| DiagnosticAction {
                name: a.name,
                byte_offset: a.span.from,
                byte_length: a.span.to.saturating_sub(a.span.from),
                insert: a.insert,
            })
            .collect(),
    }
}

fn map_severity(s: EngineSeverity) -> Severity {
    match s {
        EngineSeverity::Error => Severity::Error,
        EngineSeverity::Warning => Severity::Warning,
        EngineSeverity::Info => Severity::Info,
        EngineSeverity::Hint => Severity::Hint,
    }
}

/// The default system-param registry pre-converted to engine specs.
/// Convenience entry point for callers (e.g. the dashboard query
/// classifier) that want the same `$__interval` etc. visibility the
/// live editor diagnostics get, without duplicating the bridge.
pub fn engine_specs_for_defaults() -> Vec<SystemParamSpec> {
    to_engine_specs(&crate::params::default_system_params())
}

/// Bridge our [`SystemParam`] (host shape) into the engine's
/// [`SystemParamSpec`] wire shape. Entries whose [`ParamKind`] has no
/// engine-side type spelling are dropped — same drop-unknown semantics the
/// engine itself uses on the JS side.
///
/// `SystemParamSpec.name` is the **bare** identifier (no leading `$`) —
/// `to_compile_params` keys the resulting `HashMap` by that name, and the
/// engine's `compile()` looks up references by their bare ident too.
fn to_engine_specs(params: &[SystemParam]) -> Vec<SystemParamSpec> {
    params
        .iter()
        .filter_map(|p| {
            engine_type_name(p.kind).map(|t| SystemParamSpec {
                name: p.name.clone(),
                type_name: t.to_string(),
                optional: false,
            })
        })
        .collect()
}

fn engine_type_name(k: ParamKind) -> Option<&'static str> {
    match k {
        ParamKind::Duration => Some("Duration"),
        ParamKind::String => Some("string"),
        ParamKind::Int => Some("int"),
        ParamKind::Float => Some("float"),
        ParamKind::Bool => Some("bool"),
        ParamKind::Dataset => Some("Dataset"),
        ParamKind::Regex => Some("Regex"),
        // The engine has no diagnostic-time type for `Metric` — they only
        // appear in completion type-gating. Dropped here, exactly as the
        // engine's own `to_compile_params` filters unknown spellings.
        ParamKind::Metric => None,
    }
}

/// Build the right-hand pane's row list: one entry per declared
/// `param $name: type;` in the buffer, plus a row for every CLI/`:p`
/// value the buffer does **not** declare (so it isn't silently dropped
/// before the server has a chance to complain).
///
/// When the buffer fails to compile we can't see the declared params,
/// so we degrade gracefully to the second half: just the user-provided
/// values, all flagged `NotDeclared`. That avoids a flickering empty
/// pane while the user is mid-edit.
pub fn param_rows(
    query: &str,
    system_params: &[SystemParam],
    provided: &std::collections::BTreeMap<String, String>,
) -> Vec<crate::params::ParamRow> {
    use crate::params::{ParamRow, ParamStatus};
    use std::collections::BTreeSet;

    let specs = to_engine_specs(system_params);
    let compile_params: HashMap<_, _> = to_compile_params(&specs);
    // `Query::params()` returns the union of user-declared params and
    // the system params we passed to `compile` (e.g. `__interval`).
    // Filter the system half out by name — those are server-resolved
    // and have nothing to do with this pane.
    let system_names: std::collections::HashSet<&str> =
        system_params.iter().map(|s| s.name.as_str()).collect();

    let (declared, declared_names): (Vec<ParamRow>, BTreeSet<String>) =
        match mpl_lang::compile(query, compile_params) {
            Ok((q, _warnings)) => {
                let mut names = BTreeSet::new();
                let rows = q
                    .params()
                    .iter()
                    .filter(|p| !system_names.contains(p.name.as_str()))
                    .map(|p| {
                        names.insert(p.name.clone());
                        let optional = matches!(p.typ, mpl_lang::query::ParamType::Optional(_));
                        let type_str = format!("{}", p.typ);
                        let value = provided.get(&p.name).cloned();
                        let status = match &value {
                            None if optional => ParamStatus::OptionalUnset,
                            None => ParamStatus::NotSet,
                            Some(v) if value_matches_type(v, &p.typ) => ParamStatus::Ok,
                            Some(_) => ParamStatus::TypeMismatch,
                        };
                        ParamRow {
                            name: p.name.clone(),
                            declared_type: Some(type_str),
                            optional,
                            value,
                            status,
                        }
                    })
                    .collect();
                (rows, names)
            }
            Err(_) => (Vec::new(), BTreeSet::new()),
        };

    let mut rows = declared;
    for (name, value) in provided {
        if declared_names.contains(name) {
            continue;
        }
        rows.push(ParamRow {
            name: name.clone(),
            declared_type: None,
            optional: false,
            value: Some(value.clone()),
            status: ParamStatus::NotDeclared,
        });
    }
    rows
}

/// Returns true iff `value` parses as the MPL `param_value` rule **and**
/// the resulting grammar rule is compatible with the declared
/// `ParamType`. Mirrors the per-pair check the engine performs inside
/// `ProvidedParams::parse_and_validate`, scoped to one pair so we can
/// surface per-row status.
fn value_matches_type(value: &str, declared: &mpl_lang::query::ParamType) -> bool {
    use mpl_lang::query::{ParamType, TagType, TerminalParamType};
    use mpl_lang::{MPLParser, Rule};
    use pest::Parser as _;

    let Ok(mut pairs) = MPLParser::parse(Rule::param_value, value) else {
        return false;
    };
    let Some(outer) = pairs.next() else {
        return false;
    };
    // Reject trailing garbage (`db-01` would otherwise quietly match `db`).
    if outer.as_span().end() != value.len() {
        return false;
    }
    // `param_value` is a choice; the actual matched alternative is the
    // sole inner pair.
    let inner_rule = outer.into_inner().next().map(|p| p.as_rule());
    let Some(rule) = inner_rule else {
        return false;
    };
    let terminal = match declared {
        ParamType::Terminal(t) | ParamType::Optional(t) => *t,
    };
    match terminal {
        TerminalParamType::Duration => rule == Rule::time_relative,
        TerminalParamType::Regex => rule == Rule::regex,
        // Datasets are bare identifiers (or backticked); both reduce to
        // `plain_ident` / `escaped_ident` inside the silent `ident` rule.
        TerminalParamType::Dataset => {
            matches!(rule, Rule::plain_ident | Rule::escaped_ident)
        }
        TerminalParamType::Tag(TagType::String) => rule == Rule::string,
        // `42` is also a valid float for typing purposes — the server
        // accepts integer literals for `Float`-typed params.
        TerminalParamType::Tag(TagType::Float) => matches!(rule, Rule::float | Rule::int),
        TerminalParamType::Tag(TagType::Int) => rule == Rule::int,
        TerminalParamType::Tag(TagType::Bool) => rule == Rule::bool,
        // No literal grammar for null; user can't usefully type one.
        TerminalParamType::Tag(TagType::Null) => false,
    }
}

/// Compute 1-indexed (line, column) for `byte_offset` into `text`.
pub fn byte_offset_to_line_col(text: &str, byte_offset: usize) -> (usize, usize) {
    let clamped = byte_offset.min(text.len());
    let prefix = &text[..clamped];
    let line = 1 + prefix.bytes().filter(|&b| b == b'\n').count();
    let column = match prefix.rfind('\n') {
        Some(nl) => prefix[nl + 1..].chars().count() + 1,
        None => prefix.chars().count() + 1,
    };
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_query_has_no_errors() {
        // The engine still emits hints (e.g. "unnecessary backtick escaping")
        // even when the query compiles cleanly; what matters for `run_query`
        // is that no `Error`-severity diagnostic is present.
        let diags = analyze("`home`:`temp` | align to 1h using avg", &[]);
        assert!(diags.iter().all(|d| !d.severity.is_error()), "{diags:?}");
    }

    #[test]
    fn syntax_error_reports_span_and_line() {
        let diags = analyze("`home`:* | align to 1m", &[]);
        let first = diags.first().expect("at least one diagnostic");
        assert!(first.severity.is_error(), "{:?}", first.severity);
        assert!(
            first.message.to_lowercase().contains("syntax")
                || first.message.to_lowercase().contains("expected"),
            "msg={}",
            first.message
        );
        assert_eq!(first.line, 1);
    }

    #[test]
    fn empty_query_is_reported_as_error() {
        let diags = analyze("", &[]);
        let first = diags.first().expect("at least one diagnostic");
        assert!(first.severity.is_error());
        assert_eq!(first.line, 1);
        assert_eq!(first.column, 1);
    }

    #[test]
    fn deprecated_duration_warning_carries_replace_action() {
        // `duration` in lowercase is deprecated; engine emits a warning
        // with a quick-fix replacing it with `Duration`.
        let q = "param $window: duration; home:temp | align to $window using avg";
        let diags = analyze(q, &[]);
        let warn = diags
            .iter()
            .find(|d| d.severity == Severity::Warning)
            .expect("expected a warning");
        let action = warn
            .actions
            .iter()
            .find(|a| a.insert == "Duration")
            .expect("expected a Replace-with-`Duration` action");
        assert_eq!(action.byte_length, "duration".len());
        assert_eq!(
            &q[action.byte_offset..action.byte_offset + action.byte_length],
            "duration"
        );
    }

    #[test]
    fn span_contains_handles_zero_length_spans() {
        let d = Diagnostic {
            severity: Severity::Error,
            message: String::new(),
            help: None,
            byte_offset: 5,
            byte_length: 0,
            line: 1,
            column: 6,
            actions: vec![],
        };
        assert!(d.span_contains(5));
        assert!(!d.span_contains(4));
        assert!(!d.span_contains(6));
    }

    #[test]
    fn span_contains_inclusive_exclusive() {
        let d = Diagnostic {
            severity: Severity::Error,
            message: String::new(),
            help: None,
            byte_offset: 2,
            byte_length: 3,
            line: 1,
            column: 3,
            actions: vec![],
        };
        assert!(d.span_contains(2));
        assert!(d.span_contains(4));
        assert!(!d.span_contains(5));
        assert!(!d.span_contains(1));
    }

    #[test]
    fn system_param_silences_undefined_param_warning() {
        let q = "home:temp | align to $__interval using avg";
        // Without a system param, the parser should warn about $__interval.
        let without = analyze(q, &[]);
        // With the param registered, the warning goes away.
        let sys = vec![SystemParam {
            name: "__interval".to_string(),
            kind: ParamKind::Duration,
        }];
        let with = analyze(q, &sys);
        assert!(
            without.len() > with.len(),
            "registering $__interval should suppress at least one diagnostic; without={without:?} with={with:?}"
        );
    }

    #[test]
    fn byte_offset_helper() {
        assert_eq!(byte_offset_to_line_col("abc", 0), (1, 1));
        assert_eq!(byte_offset_to_line_col("abc", 3), (1, 4));
        assert_eq!(byte_offset_to_line_col("a\nbc", 2), (2, 1));
        assert_eq!(byte_offset_to_line_col("a\nbc", 4), (2, 3));
        assert_eq!(byte_offset_to_line_col("ab", 999), (1, 3));
    }
}
