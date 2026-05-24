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

#[test]
fn extract_dataset_from_backticked() {
    assert_eq!(
        extract_dataset_metric("`home`:`temp` | align to 5m using avg")
            .unwrap()
            .0,
        "home"
    );
    assert_eq!(
        extract_dataset_metric("`k8s-metrics-dev`:cpu_usage[1h..]")
            .unwrap()
            .0,
        "k8s-metrics-dev"
    );
}

#[test]
fn extract_dataset_from_plain() {
    assert_eq!(
        extract_dataset_metric("home:temp | align to 1m").unwrap().0,
        "home"
    );
}

#[test]
fn extract_dataset_skips_leading_line_comment() {
    // The dashboard adoption seeds the editor with a `// @viz`
    // pragma above the real query. Without comment-skipping the
    // dataset parser used to read `//` as the dataset name and
    // ask the server for it, producing
    // `dataset "//" not found in this deployment`.
    let q = "// @viz statistic\n`home`:temp\n| group using avg";
    let (ds, m) = extract_dataset_metric(q).unwrap();
    assert_eq!(ds, "home");
    assert_eq!(m, "temp");
}

#[test]
fn extract_dataset_skips_multiple_comments() {
    let q = "// pragma\n// another\n/* block */ `home`:temp";
    assert_eq!(extract_dataset_metric(q).unwrap().0, "home");
}

#[test]
fn extract_dataset_errors_on_garbage() {
    assert!(extract_dataset_metric("").is_err());
    assert!(extract_dataset_metric("`unterminated").is_err());
}
