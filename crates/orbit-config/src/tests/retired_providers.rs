//! Regression checks for the retired provider lanes (ORB-18096).

use super::{roots, write_config};

use crate::ResolvedConfig;
use tempfile::tempdir;

#[test]
fn crew_naming_a_retired_provider_fails_with_named_refusal() {
    for provider in ["cursor", "pi", "opencode"] {
        let global = tempdir().expect("global");
        let workspace = tempdir().expect("workspace");
        write_config(
            global.path(),
            &format!("[crews.legacy]\nprovider = \"{provider}\"\nmodel = \"fixture\"\n"),
        );
        let error = ResolvedConfig::load(&roots(global.path(), workspace.path()))
            .expect_err("retired provider must not load")
            .to_string();
        assert!(error.contains("legacy"), "{error}");
        assert!(error.contains(provider), "{error}");
        assert!(error.contains("retired"), "{error}");
        assert!(error.contains("pick a supported provider"), "{error}");
        assert!(!error.contains("unknown provider"), "{error}");
    }
}

#[test]
fn retired_provider_refusal_is_case_insensitive() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    write_config(
        global.path(),
        "[crews.legacy]\nprovider = \"Cursor\"\nmodel = \"fixture\"\n",
    );
    let error = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect_err("retired provider must not load")
        .to_string();
    assert!(
        error.contains("cursor") && error.contains("retired"),
        "{error}"
    );
}

#[test]
fn supported_provider_crew_still_loads_and_defaults_have_no_retired_crews() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    write_config(global.path(), "");
    let loaded = ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("defaults");
    for retired in ["cursor", "pi", "opencode"] {
        assert!(!loaded.crews.contains_key(retired), "{retired} seeded");
        assert!(
            loaded
                .crews
                .values()
                .all(|crew| crew.assignment.provider != retired),
            "a default crew uses {retired}"
        );
    }
    write_config(
        global.path(),
        "[workflow]\ndefault_crew = \"main\"\n[crews.main]\nprovider = \"copilot\"\nmodel = \"fixture\"\n",
    );
    let loaded = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect("supported provider loads");
    assert_eq!(loaded.crews["main"].assignment.provider, "copilot");
}
