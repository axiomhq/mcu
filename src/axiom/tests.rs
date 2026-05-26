use super::*;

#[test]
fn snippet_truncates_long_text() {
    let s = "a".repeat(500);
    let got = snippet(&s, 10);
    assert_eq!(got.chars().count(), 11);
    assert!(got.ends_with('…'));
}

#[test]
fn snippet_passes_through_short_text() {
    assert_eq!(snippet("  hi  ", 10), "hi");
}

#[test]
fn parse_time_expr_handles_now_and_relatives() {
    let now = chrono::DateTime::parse_from_rfc3339("2024-05-01T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    // bare `now`
    assert_eq!(parse_time_expr("now", now).unwrap(), now);

    // `now-7d`
    let seven_days_ago = parse_time_expr("now-7d", now).unwrap();
    assert_eq!(now - seven_days_ago, chrono::Duration::days(7));

    // `now-15m`
    let fifteen_min_ago = parse_time_expr("now-15m", now).unwrap();
    assert_eq!(now - fifteen_min_ago, chrono::Duration::minutes(15));

    // `now+1h`
    let one_hour_later = parse_time_expr("now+1h", now).unwrap();
    assert_eq!(one_hour_later - now, chrono::Duration::hours(1));

    // RFC3339 passes through.
    let abs = parse_time_expr("2024-04-01T00:00:00Z", now).unwrap();
    assert_eq!(abs.to_rfc3339(), "2024-04-01T00:00:00+00:00");
}

#[test]
fn parse_time_expr_rejects_garbage() {
    let now = chrono::Utc::now();
    assert!(parse_time_expr("", now).is_err());
    assert!(parse_time_expr("now-", now).is_err());
    assert!(parse_time_expr("now-7", now).is_err());
    assert!(parse_time_expr("now-7y", now).is_err());
    assert!(parse_time_expr("qr-now-7d", now).is_err()); // prefix must be stripped upstream
}

#[test]
fn decodes_dataset_summary_with_edge_deployment() {
    let body = r#"[
        {
            "name": "my-metrics",
            "description": "metrics dataset",
            "edgeDeployment": "cloud.eu-central-1.aws",
            "kind": "metrics"
        },
        {
            "name": "events-only",
            "description": null
        }
    ]"#;
    let datasets: Vec<DatasetSummary> = serde_json::from_str(body).unwrap();
    assert_eq!(datasets.len(), 2);
    assert_eq!(datasets[0].name, "my-metrics");
    assert_eq!(
        datasets[0].edge_deployment.as_deref(),
        Some("cloud.eu-central-1.aws")
    );
    assert_eq!(datasets[0].kind.as_deref(), Some("metrics"));
    assert_eq!(datasets[1].edge_deployment, None);
    assert_eq!(datasets[1].kind, None);
}

#[test]
fn dashboard_summary_ext_name_or_unnamed() {
    // Re-export & extension trait: an SDK dashboard with `name` set surfaces
    // through `DashboardSummaryExt::name_or_unnamed`.
    let body = r#"{
        "uid": "u",
        "dashboard": { "name": "Errors" }
    }"#;
    let d: DashboardSummary = serde_json::from_str(body).unwrap();
    assert_eq!(d.name_or_unnamed(), "Errors");

    let body_no_name = r#"{ "uid": "u", "dashboard": {} }"#;
    let d2: DashboardSummary = serde_json::from_str(body_no_name).unwrap();
    assert_eq!(d2.name_or_unnamed(), "(unnamed)");
}
