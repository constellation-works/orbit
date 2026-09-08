//! Registered integration identity stays distinct from GitHub's release default.

use serde_json::json;

use super::super::collect::collect;
use super::support::FakeQueries;

#[test]
fn registered_integration_does_not_reclassify_github_default_as_integration() {
    let head = "1111111111111111111111111111111111111111";
    let queries = FakeQueries::authenticated()
        .with_head("agent-main", head)
        .with_head("main", head);
    let evidence = collect(&queries, &json!({"integration_branch": "agent-main"}))
        .expect("collect with resolved sweep input");

    assert_eq!(evidence["repository"]["default_branch"], "main");
    let heads = evidence["heads"].as_array().expect("scanned heads");
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0]["branch"], "agent-main");
    assert_eq!(heads[0]["kind"], "integration");
    assert_eq!(heads[1]["branch"], "main");
    assert_eq!(heads[1]["kind"], "release");
}
