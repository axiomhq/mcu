use super::*;

#[test]
fn snippet_truncates_long_text() {
    let s = "a".repeat(500);
    let got = snippet(&s, 10);
    assert_eq!(got.chars().count(), 11);
    assert!(got.ends_with('…'));
}

#[test]
fn decodes_dashboard_resource_envelope() {
    // Real shape from `GET /v2/dashboards`: each item is a
    // DashboardResource with the document nested under `dashboard`.
    let body = r#"[
        {
            "uid": "abc123",
            "id": "42",
            "version": 7,
            "createdAt": "2026-05-01T10:00:00Z",
            "updatedAt": "2026-05-23T10:00:00Z",
            "createdBy": "u1",
            "updatedBy": "u2",
            "dashboard": {
                "name": "Cluster Overview",
                "description": "pod lifecycle",
                "charts": [],
                "layout": []
            }
        }
    ]"#;
    let v: Vec<DashboardSummary> = serde_json::from_str(body).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].uid, "abc123");
    assert_eq!(v[0].id.as_deref(), Some("42"));
    assert_eq!(v[0].updated_at.as_deref(), Some("2026-05-23T10:00:00Z"));
    assert_eq!(v[0].name(), "Cluster Overview");
    assert_eq!(v[0].description(), Some("pod lifecycle"));
}

#[test]
fn decodes_dashboard_resource_tolerates_extra_fields() {
    // Server schema bumps shouldn't break the picker decode.
    let body = r#"[{
        "uid": "x",
        "dashboard": {"name": "y", "newField": 1},
        "futureTopLevelField": true
    }]"#;
    let v: Vec<DashboardSummary> = serde_json::from_str(body).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].name(), "y");
}

#[test]
fn dashboard_name_falls_back_when_document_missing_name() {
    let body = r#"[{"uid": "x", "dashboard": {}}]"#;
    let v: Vec<DashboardSummary> = serde_json::from_str(body).unwrap();
    assert_eq!(v[0].name(), "(unnamed)");
}

#[test]
fn decodes_chart_variants_with_type_discriminator() {
    // One of each chart type the server emits, with id + name +
    // an opaque query field. The decoder should land each on the
    // right `Chart` variant and preserve `query` verbatim.
    let body = r#"{
        "name": "sample",
        "charts": [
            {"id": "c1", "type": "TimeSeries", "name": "ts", "query": {"a": 1}},
            {"id": "c2", "type": "Heatmap", "name": "hm", "query": {}},
            {"id": "c3", "type": "LogStream", "name": "ls"},
            {"id": "c4", "type": "Pie"},
            {"id": "c5", "type": "Scatter"},
            {"id": "c6", "type": "Table"},
            {"id": "c7", "type": "TopK"},
            {"id": "c8", "type": "Statistic"},
            {"id": "c9", "type": "Note", "name": "hi"}
        ]
    }"#;
    let doc: DashboardDocument = serde_json::from_str(body).unwrap();
    let types: Vec<&str> = doc.charts.iter().map(|c| c.type_str()).collect();
    assert_eq!(
        types,
        vec![
            "TimeSeries",
            "Heatmap",
            "LogStream",
            "Pie",
            "Scatter",
            "Table",
            "TopK",
            "Statistic",
            "Note",
        ]
    );
    // Spot-check that nested query JSON survives intact.
    let ts = doc.charts.first().unwrap();
    assert_eq!(ts.base().query.as_ref().unwrap()["a"], 1);
}

#[test]
fn decodes_layout_items() {
    let body = r#"{
        "name": "x",
        "layout": [
            {"i": "c1", "x": 0, "y": 0, "w": 6, "h": 4},
            {"i": "c2", "x": 6, "y": null, "w": 6, "h": 4, "static": true}
        ]
    }"#;
    let doc: DashboardDocument = serde_json::from_str(body).unwrap();
    assert_eq!(doc.layout.len(), 2);
    assert_eq!(doc.layout[0].i, "c1");
    assert_eq!(doc.layout[0].y, Some(0));
    assert_eq!(doc.layout[1].y, None);
    // Unmodelled `static` survives in extras.
    assert!(doc.layout[1].extras.contains_key("static"));
}

#[test]
fn upsert_request_omits_version_when_overwrite() {
    let doc = DashboardDocument {
        name: Some("x".into()),
        ..Default::default()
    };
    let body = DashboardUpsertRequest {
        dashboard: &doc,
        version: None,
        overwrite: true,
        uid: Some("u1"),
        message: None,
    };
    let v = serde_json::to_value(&body).unwrap();
    assert_eq!(v["overwrite"], true);
    assert!(v.get("version").is_none(), "version should be omitted");
    assert!(v.get("message").is_none());
    assert_eq!(v["uid"], "u1");
}

#[test]
fn upsert_request_omits_overwrite_when_default() {
    let doc = DashboardDocument::default();
    let body = DashboardUpsertRequest {
        dashboard: &doc,
        version: Some(5),
        overwrite: false,
        uid: None,
        message: None,
    };
    let v = serde_json::to_value(&body).unwrap();
    // `overwrite: false` is the schema default, so we don't emit
    // it; this keeps the on-the-wire payload minimal.
    assert!(
        v.get("overwrite").is_none(),
        "overwrite=false should be omitted, got {v}"
    );
    assert_eq!(v["version"], 5);
}

#[test]
fn decodes_write_response_status() {
    let body = r#"{
        "status": "updated",
        "overwritten": false,
        "dashboard": {
            "uid": "u1",
            "version": 8,
            "dashboard": {"name": "x"}
        }
    }"#;
    let w: DashboardWriteResponse = serde_json::from_str(body).unwrap();
    assert_eq!(w.status, DashboardWriteStatus::Updated);
    assert_eq!(w.dashboard.version, Some(8));
}

#[test]
fn decodes_412_error_with_current_version() {
    let body = r#"{
        "code": "version_conflict",
        "message": "dashboard version is stale",
        "currentVersion": 9,
        "uid": "u1"
    }"#;
    let e: DashboardError = serde_json::from_str(body).unwrap();
    assert_eq!(e.code, "version_conflict");
    assert_eq!(e.current_version, Some(9));
}

#[test]
fn dashboard_document_round_trips_extras() {
    // Unknown top-level fields (`refreshTime`, `schemaVersion`,
    // `against`, `owner`) survive both decode and re-encode —
    // critical because the server's spec is
    // `additionalProperties: false`, so PUT would reject anything we
    // dropped on the floor.
    let original = serde_json::json!({
        "name": "keepers",
        "refreshTime": 60,
        "schemaVersion": 2,
        "owner": "X-AXIOM-EVERYONE",
        "against": "-1h",
        "timeWindowStart": "qr-now-1h",
        "timeWindowEnd": "qr-now"
    });
    let doc: DashboardDocument = serde_json::from_value(original.clone()).unwrap();
    let re = serde_json::to_value(&doc).unwrap();
    // Every key from the original lands somewhere in the re-encode.
    for (k, v) in original.as_object().unwrap() {
        assert_eq!(&re[k], v, "field `{k}` did not round-trip");
    }
}

#[test]
fn decodes_dataset_summary() {
    let body = r#"[
        {"name": "k8s", "description": "k8s metrics", "edgeDeployment": "cloud.us-east-1.aws", "kind": "otel:metrics:v1"},
        {"name": "logs"}
    ]"#;
    let datasets: Vec<DatasetSummary> = serde_json::from_str(body).unwrap();
    assert_eq!(datasets.len(), 2);
    assert_eq!(datasets[0].name, "k8s");
    assert_eq!(datasets[0].kind.as_deref(), Some("otel:metrics:v1"));
    assert_eq!(
        datasets[0].edge_deployment.as_deref(),
        Some("cloud.us-east-1.aws")
    );
    assert!(datasets[1].kind.is_none());
}

#[test]
fn decodes_metrics_info() {
    let body = r#"{
        "switch": {"type": "Mixed", "temporality": "Mixed", "unit": null},
        "temp":   {"type": "Mixed", "temporality": "Mixed", "unit": "C"}
    }"#;
    let m: BTreeMap<String, MetricInfo> = serde_json::from_str(body).unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m["switch"].kind.as_deref(), Some("Mixed"));
    assert_eq!(m["temp"].unit.as_deref(), Some("C"));
}

#[test]
fn decodes_metrics_query_response() {
    let body = r#"{
        "metadata": {},
        "series": [
            {
                "metric": "temp",
                "tags": {"room": "Eingang"},
                "start": 1764547200,
                "resolution": 3600,
                "data": [18.24, null, 18.11]
            }
        ]
    }"#;
    let resp: MetricsQueryResponse = serde_json::from_str(body).unwrap();
    assert_eq!(resp.series.len(), 1);
    let s = &resp.series[0];
    assert_eq!(s.metric, "temp");
    assert_eq!(s.tags.get("room").map(String::as_str), Some("Eingang"));
    assert_eq!(s.start, 1764547200);
    assert_eq!(s.resolution, 3600);
    assert_eq!(s.data, vec![Some(18.24), None, Some(18.11)]);
}

#[test]
fn urlencodes_rfc3339_timestamps() {
    assert_eq!(urlencoding("now"), "now");
    assert_eq!(urlencoding("now-1h"), "now-1h");
    assert_eq!(
        urlencoding("2026-05-14T00:00:00Z"),
        "2026-05-14T00%3A00%3A00Z"
    );
}
