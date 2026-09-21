#![allow(missing_docs, clippy::expect_used)]

use std::collections::BTreeSet;

use toml::Value;

const MANIFEST: &str = include_str!("../Cargo.toml");

#[test]
fn registry_depends_only_on_shared_leaves() {
    let manifest: Value = toml::from_str(MANIFEST).expect("crate manifest parses");
    let mut names = BTreeSet::new();

    for section in ["dependencies", "build-dependencies"] {
        let Some(table) = manifest.get(section).and_then(Value::as_table) else {
            continue;
        };
        for (name, value) in table {
            let package_name = value
                .as_table()
                .and_then(|table| table.get("package"))
                .and_then(Value::as_str)
                .unwrap_or(name);
            if package_name.starts_with("orbit-") {
                names.insert(package_name.to_string());
            }
        }
    }

    // [ORB-12725] `orbit-config` joins the two shared leaves: this machine's
    // identity is the `[machine]` table in the global `config.toml`, and
    // Registry reads it through that crate's admission rather than keeping a
    // second parser and a second set of validators.
    assert_eq!(
        names,
        BTreeSet::from([
            "orbit-common".to_string(),
            "orbit-config".to_string(),
            "orbit-types".to_string(),
        ]),
        "orbit-registry must stay independent of database, command, transport, and runtime crates"
    );
}
