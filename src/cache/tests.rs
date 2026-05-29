use super::*;
use crate::axiom::DashboardSummaryExt;

fn ds(name: &str, edge: Option<&str>) -> DatasetSummary {
    DatasetSummary {
        name: name.to_string(),
        description: None,
        edge_deployment: edge.map(str::to_string),
        kind: Some("otel:metrics:v1".to_string()),
    }
}

#[test]
fn replace_and_lookup_datasets() {
    let mut cache = Cache::in_memory("https://api.example.com".to_string());
    cache.replace_datasets(vec![
        ds("home", Some("cloud.us-east-1.aws")),
        ds("local", None),
    ]);
    assert_eq!(cache.dataset_count(), 2);
    assert_eq!(cache.dataset_names(), vec!["home", "local"]);

    let route = cache.edge_route_for("home").unwrap();
    assert_eq!(route.url, "https://us-east-1.aws.edge.axiom.co");
    assert_eq!(route.deployment.as_deref(), Some("cloud.us-east-1.aws"));

    let route = cache.edge_route_for("local").unwrap();
    assert_eq!(route.url, "https://api.example.com");
    assert!(route.deployment.is_none());

    assert!(cache.edge_route_for("ghost").is_none());
}

#[test]
fn replace_tag_values_stores_per_tag_inventory() {
    let mut cache = Cache::in_memory("u".to_string());
    cache.replace_tag_values(
        "home",
        "temp",
        "host",
        vec!["a".to_string(), "b".to_string(), "a".to_string()],
    );
    assert!(cache.has_tag_values("home", "temp", "host"));
    assert_eq!(cache.tag_values_for("home", "temp", "host"), vec!["a", "b"]);
    assert!(!cache.has_tag_values("home", "temp", "region"));
}

#[test]
fn replace_tags_stores_per_metric_inventory() {
    let mut cache = Cache::in_memory("u".to_string());
    cache.replace_tags(
        "home",
        "temp",
        vec!["host".to_string(), "region".to_string(), "host".to_string()],
    );
    assert!(cache.has_tags("home", "temp"));
    assert_eq!(cache.tags_for("home", "temp"), vec!["host", "region"]);
    assert!(!cache.has_tags("home", "other"));
    assert!(cache.tags_for("home", "other").is_empty());
}

#[test]
fn replace_metrics_stores_per_dataset_inventory() {
    let mut cache = Cache::in_memory("https://api.example.com".to_string());
    let mut metrics = BTreeMap::new();
    metrics.insert(
        "temp".to_string(),
        MetricInfo {
            kind: Some("Mixed".to_string()),
            temporality: Some("Mixed".to_string()),
            unit: None,
        },
    );
    cache.replace_metrics("home", metrics);
    assert_eq!(cache.metric_names("home"), vec!["temp"]);
    assert!(cache.metric_names("ghost").is_empty());
}

#[test]
fn legend_tags_query_hash_wins_over_dataset_metric() {
    let mut cache = Cache::in_memory("u".to_string());
    cache.set_legend_tags("hash-a", "home", "temp", vec!["host".to_string()]);
    cache.set_legend_tags(
        "hash-b",
        "home",
        "temp",
        vec!["host".to_string(), "region".to_string()],
    );
    // Exact-hash match returns hash-b's tags.
    assert_eq!(
        cache.resolve_legend_tags("hash-b", "home", "temp"),
        vec!["host", "region"]
    );
    // Unknown hash but known (dataset, metric) — returns the
    // most-recently-set value for that pair (hash-b overwrote).
    assert_eq!(
        cache.resolve_legend_tags("hash-z", "home", "temp"),
        vec!["host", "region"]
    );
    // No match at all.
    assert!(
        cache
            .resolve_legend_tags("hash-z", "home", "cpu")
            .is_empty()
    );
}

#[test]
fn legend_tags_empty_value_clears_entries() {
    let mut cache = Cache::in_memory("u".to_string());
    cache.set_legend_tags("h1", "home", "temp", vec!["host".to_string()]);
    cache.set_legend_tags("h1", "home", "temp", vec![]);
    assert!(cache.resolve_legend_tags("h1", "home", "temp").is_empty());
}

#[test]
fn save_skipped_when_path_unset() {
    let cache = Cache::in_memory("u".to_string());
    assert!(cache.debug_path().is_none());
    cache.save().unwrap();
    cache.save_query("foo").unwrap();
    assert!(cache.load_query().is_none());
}

#[test]
fn query_round_trips_through_disk() {
    let tmp = tempdir();
    let path = tmp.join("discovery.json");
    let cache = Cache {
        path: Some(path),
        data: CacheData::default(),
        fallback_base_url: String::new(),
    };
    cache
        .save_query("home:temp | align to 1m using avg")
        .unwrap();
    assert_eq!(
        cache.load_query().as_deref(),
        Some("home:temp | align to 1m using avg")
    );
}

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir().join(format!("ax-test-{}-{}", std::process::id(), unix_now()));
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn dash(uid: &str, name: &str) -> DashboardSummary {
    DashboardSummary {
        uid: uid.to_string(),
        id: None,
        updated_at: None,
        updated_by: None,
        version: Some(1),
        dashboard: crate::axiom::DashboardDocument {
            name: Some(name.to_string()),
            ..Default::default()
        },
    }
}

#[test]
fn dashboard_list_round_trips() {
    let mut cache = Cache::in_memory("u".to_string());
    assert!(cache.cached_dashboards().is_none());
    cache.replace_dashboards(vec![dash("a", "Alpha"), dash("b", "Beta")]);
    let items = cache.cached_dashboards().expect("cached list");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].uid, "a");
    assert_eq!(items[1].name_or_unnamed(), "Beta");
}

#[test]
fn per_uid_dashboard_round_trips_and_forget_evicts_from_both() {
    let mut cache = Cache::in_memory("u".to_string());
    cache.replace_dashboards(vec![dash("a", "Alpha"), dash("b", "Beta")]);
    cache.replace_dashboard("a", dash("a", "Alpha"));
    cache.replace_dashboard("b", dash("b", "Beta"));
    assert!(cache.cached_dashboard("a").is_some());
    assert!(cache.cached_dashboard("b").is_some());

    cache.forget_dashboard("a");
    assert!(cache.cached_dashboard("a").is_none());
    assert!(cache.cached_dashboard("b").is_some());
    // Also pruned from the listing so :dash ls doesn't show a
    // tombstoned uid that the next :open would 404 on.
    let items = cache.cached_dashboards().expect("list");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].uid, "b");
}

#[test]
fn edge_route_handles_null_and_unknown_strings() {
    let r = make_edge_route(None, "https://api.example.com");
    assert!(r.deployment.is_none());
    let r = make_edge_route(Some("null"), "https://api.example.com");
    assert!(r.deployment.is_none());
    let r = make_edge_route(Some(""), "https://api.example.com");
    assert!(r.deployment.is_none());
    let r = make_edge_route(Some("self-hosted"), "https://api.example.com");
    assert!(r.deployment.is_none());
    let r = make_edge_route(Some("cloud.eu-central-1.aws"), "https://api.example.com");
    assert_eq!(r.url, "https://eu-central-1.aws.edge.axiom.co");
}
