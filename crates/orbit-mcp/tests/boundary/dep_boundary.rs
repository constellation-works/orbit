#![allow(missing_docs, clippy::expect_used)]

use std::collections::BTreeSet;

use toml::Value;

const MANIFEST: &str = include_str!("../../Cargo.toml");

#[test]
fn mcp_depends_only_on_its_leaf_domains() {
    let manifest: Value = toml::from_str(MANIFEST).expect("crate manifest parses");
    let dependencies = manifest
        .get("dependencies")
        .and_then(Value::as_table)
        .expect("dependencies table")
        .keys()
        .filter(|name| name.starts_with("orbit-"))
        .cloned()
        .collect::<BTreeSet<_>>();

    assert_eq!(
        dependencies,
        BTreeSet::from([
            "orbit-common".to_string(),
            "orbit-registry".to_string(),
            "orbit-tools".to_string(),
            "orbit-types".to_string(),
        ]),
        "orbit-mcp must stay independent of command, runtime, and Web crates"
    );
}
