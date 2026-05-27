//! `// @unit <expr>` pragma parsing.
//!
//! Lives in the leading-comment block of the editor buffer, alongside
//! the existing `// @viz` pragma:
//!
//! ```text
//! // @viz line
//! // @unit MiBy/s
//! ```
//!
//! Format: `// @unit <expr>`. Anything after `@unit ` up to
//! end-of-line is the expression. We validate it via
//! [`crate::unit::parse`] (UCUM plus deliberate currency extensions)
//! so a buffer that types its way through a half-finished pragma
//! surfaces a diagnostic instead of silently attaching a garbage unit.

use super::Unit;

/// What went wrong parsing a `@unit` line. Surfaced as a diagnostic
/// by the caller, mirroring the `@viz` pragma's error path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitPragmaError {
    /// The line started `// @unit` but had no expression afterwards.
    MissingExpr,
    /// The expression didn't validate as a supported unit.
    InvalidExpr { token: String },
}

impl std::fmt::Display for UnitPragmaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitPragmaError::MissingExpr => f.write_str("`@unit` pragma is missing the expression"),
            UnitPragmaError::InvalidExpr { token } => {
                write!(f, "`@unit` expression is not a supported unit: `{token}`")
            }
        }
    }
}

/// Parse the first `// @unit` line of `src`. Returns:
///
/// - `Ok(Some(unit))` — pragma present and parses.
/// - `Ok(None)` — no `@unit` line found in the leading-comment block.
/// - `Err((line_index, err))` — malformed pragma; caller surfaces
///   the diagnostic anchored at `line_index` (zero-based).
pub fn parse_unit_pragma(src: &str) -> Result<Option<Unit>, (usize, UnitPragmaError)> {
    for (line_idx, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") {
            // Pragmas only live in the leading-comment block. Blank
            // lines are allowed between pragma and code; non-blank,
            // non-comment lines end the search.
            if trimmed.is_empty() {
                continue;
            }
            break;
        }
        let body = trimmed.trim_start_matches('/').trim_start();
        let Some(rest) = body.strip_prefix("@unit") else {
            continue;
        };
        // Require a space or end-of-line after `@unit` so `@units`
        // isn't a match.
        if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
            continue;
        }
        let expr = rest.trim();
        if expr.is_empty() {
            return Err((line_idx, UnitPragmaError::MissingExpr));
        }
        let Some(unit) = super::parse(expr) else {
            return Err((
                line_idx,
                UnitPragmaError::InvalidExpr {
                    token: expr.to_string(),
                },
            ));
        };
        return Ok(Some(unit));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unit::UnitFamily;

    #[test]
    fn finds_unit_pragma_at_top_of_buffer() {
        let src = "// @viz line\n// @unit MiBy\n\nhttp.rps:rate";
        let u = parse_unit_pragma(src).unwrap().expect("pragma found");
        assert_eq!(u.family(), UnitFamily::BytesBinary);
        assert_eq!(u.raw(), "MiBy");
    }

    #[test]
    fn returns_none_when_no_unit_pragma_present() {
        let src = "// @viz line\nhttp.rps:rate";
        assert!(parse_unit_pragma(src).unwrap().is_none());
    }

    #[test]
    fn ignores_unit_pragma_after_non_comment_line() {
        // Pragmas only live in the leading-comment block. A line of
        // real MPL ends the search; a later `// @unit` is text in
        // the middle of the query, not a pragma.
        let src = "http.rps:rate\n// @unit ms";
        assert!(parse_unit_pragma(src).unwrap().is_none());
    }

    #[test]
    fn allows_blank_lines_between_comments() {
        // Blank lines don't end the leading-comment block.
        let src = "// @viz line\n\n// @unit s\n\nhttp.rps";
        let u = parse_unit_pragma(src).unwrap().expect("pragma found");
        assert_eq!(u.family(), UnitFamily::Time);
    }

    #[test]
    fn errors_on_missing_expression() {
        let src = "// @unit\n";
        let err = parse_unit_pragma(src).unwrap_err();
        assert_eq!(err.0, 0);
        assert_eq!(err.1, UnitPragmaError::MissingExpr);
    }

    #[test]
    fn errors_on_invalid_unit() {
        // Empty after the `@unit ` is missing-expr; but garbage that
        // neither the currency extension nor the UCUM library can
        // parse lands here.
        let src = "// @unit ???\n";
        let err = parse_unit_pragma(src).unwrap_err();
        assert_eq!(err.0, 0);
        match err.1 {
            UnitPragmaError::InvalidExpr { token } => assert_eq!(token, "???"),
            other => panic!("expected InvalidExpr, got {other:?}"),
        }
    }

    #[test]
    fn ignores_unitsuffix_keywords() {
        // `@units` (note the trailing `s`) is NOT `@unit` — the
        // post-keyword char must be whitespace or end-of-line.
        let src = "// @units MiBy\n";
        assert!(parse_unit_pragma(src).unwrap().is_none());
    }

    #[test]
    fn handles_compound_rate_pragma() {
        let src = "// @unit By/s\n";
        let u = parse_unit_pragma(src).unwrap().expect("pragma found");
        assert_eq!(u.family(), UnitFamily::BytesPerTime);
    }

    #[test]
    fn handles_friendly_mass_concentration_pragma() {
        let src = "// @unit µg/m³\nhome:pm25";
        let u = parse_unit_pragma(src).unwrap().expect("pragma found");
        assert_eq!(u.family(), UnitFamily::MassConcentration);
        assert_eq!(u.raw(), "µg/m3");
    }

    #[test]
    fn handles_currency_pragma_extension() {
        let src = "// @unit EUR\nhome:price";
        let u = parse_unit_pragma(src).unwrap().expect("pragma found");
        assert_eq!(
            u.family(),
            UnitFamily::Currency(iso_currency::Currency::EUR)
        );
    }

    #[test]
    fn coexists_with_viz_pragma_in_any_order() {
        // Either order should work — both pragmas are independent.
        for src in ["// @viz line\n// @unit ms\n", "// @unit ms\n// @viz line\n"] {
            let u = parse_unit_pragma(src)
                .unwrap()
                .expect("pragma found in either order");
            assert_eq!(u.family(), UnitFamily::Time);
        }
    }
}
