//! Sibling tests for `config_path.rs`: selecting the effective `config.toml`
//! from a runtime's roots.

use std::path::PathBuf;

#[test]
fn root_path_tilde_expansion_uses_home_and_only_one_prefix() {
    let home = crate::paths::home_dir().expect("test host has a home directory");
    let cases = [
        ("~", home.clone()),
        ("~/x", home.join("x")),
        ("~/~/x", home.join("~/x")),
        ("~//x", home.join("x")),
    ];

    for (input, expected) in cases {
        let resolved = crate::paths::resolve_path_value(input, &PathBuf::from("."), "root path")
            .expect("tilde root path resolves");

        assert_eq!(resolved, expected, "unexpected resolution for {input:?}");
    }
}
