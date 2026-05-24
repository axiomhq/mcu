//! One-shot probe: run every MPL query from the Home Assistant
//! dashboard through `mpl_lang::compile` and report which classify
//! as MPL. Not asserted — we just print outcomes.

use mpl_language_server::{SystemParamSpec, to_compile_params};
use std::collections::HashMap;

#[test]
fn ha_dashboard_classification() {
    let raw = include_str!("../tests/ha_queries.txt");
    // Mirrors `crate::params::default_system_params()` — the binary
    // isn't exposed as a library so we hand-roll the spec here. Keep
    // in sync with `src/params.rs`.
    let specs = vec![SystemParamSpec {
        name: "__interval".to_string(),
        type_name: "Duration".to_string(),
        optional: false,
    }];
    let params: HashMap<_, _> = to_compile_params(&specs);
    let mut total = 0usize;
    let mut ok = 0usize;
    for (i, q) in raw.split_terminator('\n').enumerate() {
        if q.is_empty() {
            continue;
        }
        total += 1;
        let res = mpl_lang::compile(q, params.clone());
        let head: String = q.chars().take(60).collect();
        match &res {
            Ok(_) => {
                ok += 1;
                eprintln!("#{i:02} ok    {head}");
            }
            Err(e) => eprintln!("#{i:02} FAIL  {head}\n         err: {e:?}"),
        }
    }
    eprintln!("\nsummary: {ok}/{total} classified as MPL");
    assert_eq!(ok, total, "every HA dashboard query should classify as MPL");
}
