//! A mint never carries a template footprint selector the target checkout
//! lacks, so the pilot never drops a minted anchor with a
//! promotion-holding warning [ORB-28151].

use orbit_types::workflow::AutoTaskDefinition;

use crate::OrbitRuntime;
use crate::application::auto_tasks::scheduler::template_params;

/// The shipped `doc-duties` template, which declares the `CONTRIBUTING.md`
/// anchor unconditionally.
fn doc_duties_definition() -> AutoTaskDefinition {
    serde_yaml::from_str(include_str!(
        "../../../../assets/auto_tasks/doc-duties.yaml"
    ))
    .expect("shipped doc-duties template parses")
}

#[allow(
    clippy::disallowed_methods,
    reason = "test fixture setup writes files in place; nothing durable is replaced"
)]
fn checkout_with_docs() -> OrbitRuntime {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    let root = runtime.paths().repo_root.clone();
    std::fs::create_dir_all(root.join("docs")).expect("fixture docs dir");
    write_fixture(&runtime, "docs/guide.md");
    write_fixture(&runtime, "README.md");
    runtime
}

#[allow(
    clippy::disallowed_methods,
    reason = "test fixture setup writes files in place; nothing durable is replaced"
)]
fn write_fixture(runtime: &OrbitRuntime, name: &str) {
    std::fs::write(runtime.paths().repo_root.join(name), "fixture\n").expect("fixture file");
}

#[test]
fn mint_drops_a_missing_contributing_anchor_but_keeps_the_rest() {
    let runtime = checkout_with_docs();
    let definition = doc_duties_definition();
    assert!(
        definition
            .template
            .context_files
            .contains(&"file:CONTRIBUTING.md".to_string()),
        "the template still declares the anchor; the mint filters it"
    );

    let params = template_params(&runtime, &definition).expect("mint params");
    assert_eq!(
        params.context_files,
        vec!["dir:docs".to_string(), "file:README.md".to_string()],
        "a checkout without CONTRIBUTING.md mints no such selector"
    );
}

#[test]
fn mint_keeps_the_anchor_where_the_checkout_has_it() {
    let runtime = checkout_with_docs();
    write_fixture(&runtime, "CONTRIBUTING.md");

    let params = template_params(&runtime, &doc_duties_definition()).expect("mint params");
    assert_eq!(
        params.context_files,
        vec![
            "dir:docs".to_string(),
            "file:README.md".to_string(),
            "file:CONTRIBUTING.md".to_string(),
        ],
        "a checkout with CONTRIBUTING.md keeps the full footprint"
    );
}

#[test]
fn mint_keeps_selectors_whose_absence_it_cannot_prove() {
    let runtime = checkout_with_docs();
    let mut definition = doc_duties_definition();
    // An anchor escaping the checkout is a template bug downstream
    // validation reports; the mint must not silently drop it.
    definition
        .template
        .context_files
        .push("file:../escape.md".to_string());

    let params = template_params(&runtime, &definition).expect("mint params");
    assert!(
        params
            .context_files
            .contains(&"file:../escape.md".to_string()),
        "an escaping anchor stays for validation to judge: {:?}",
        params.context_files
    );
    assert!(
        !params
            .context_files
            .contains(&"file:CONTRIBUTING.md".to_string()),
        "the provably missing anchor is still dropped: {:?}",
        params.context_files
    );
}
