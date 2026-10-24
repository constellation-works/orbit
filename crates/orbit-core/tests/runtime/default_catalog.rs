//! Published catalogs must describe the definitions a fresh workspace receives.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_core::RoutineSeedIdentity;
use orbit_core::application::auto_tasks::collect_auto_tasks;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};

/// Read the leading code identifier of each top-level Markdown list item in
/// one section. Descriptions, counts, ordering and emphasis are not contracts.
fn catalog_names(page: &str, heading_suffix: &str) -> BTreeSet<String> {
    let mut lines = page.lines();
    lines
        .find(|line| line.starts_with("## ") && line.ends_with(heading_suffix))
        .expect("published catalog section");
    lines
        .take_while(|line| !line.starts_with("## "))
        .filter_map(|line| line.strip_prefix("- "))
        .filter_map(|item| item.trim_start_matches('*').strip_prefix('`'))
        .map(|item| {
            item.split_once('`')
                .expect("closed catalog identifier")
                .0
                .to_owned()
        })
        .collect()
}

#[test]
fn published_lists_cover_initialized_defaults_and_owner_pinned_routines() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "published_lists_cover_initialized_defaults_and_owner_pinned_routines",
    ) {
        return;
    }

    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated catalogs");
    let global = dir.path().join("global");
    let workspace = dir.path().join("repo/.orbit");
    init_workspace_at_root(
        &global,
        InitOptions {
            global_only: true,
            ..Default::default()
        },
    )
    .expect("initialize installed skills");
    init_workspace_at_root(
        &workspace,
        InitOptions {
            global_root_override: Some(global.clone()),
            routine_seed_identity: Some(
                RoutineSeedIdentity::new("catalog", "hm_catalog", "main").expect("seed identity"),
            ),
            ..Default::default()
        },
    )
    .expect("initialize workspace defaults");

    let defaults = collect_auto_tasks(&workspace);
    assert!(defaults.errors.is_empty(), "{:?}", defaults.errors);
    let auto_tasks: BTreeSet<_> = defaults
        .definitions
        .into_iter()
        .map(|loaded| loaded.definition.name)
        .collect();
    assert!(!auto_tasks.is_empty(), "initialization must seed a catalog");
    let mut pinned_routines = BTreeSet::new();
    for entry in std::fs::read_dir(workspace.join("routines")).expect("seeded routines") {
        let path = entry.expect("routine entry").path();
        if path.extension().is_none_or(|extension| extension != "yaml") {
            continue;
        }
        let definition = parse_routine_yaml(&std::fs::read_to_string(path).expect("routine YAML"))
            .expect("valid seeded routine");
        if definition
            .trigger
            .state
            .is_some_and(|state| !state.owner_machine.is_empty())
        {
            pinned_routines.insert(
                definition
                    .name
                    .strip_suffix("-catalog")
                    .expect("seeded workspace suffix")
                    .to_owned(),
            );
        }
    }
    assert!(
        !pinned_routines.is_empty(),
        "fixture must exercise owner pins"
    );

    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for references in [
        global.join("skills/orbit-setup/references"),
        repo.join("plugin/skills/orbit-setup/references"),
    ] {
        let auto_page = references.join("auto-tasks.md");
        assert_eq!(
            catalog_names(
                &std::fs::read_to_string(&auto_page).expect("auto-task reference"),
                "seeded definitions"
            ),
            auto_tasks,
            "published defaults must match initialization [ORB-17809]: {}",
            auto_page.display()
        );
        let host_page = references.join("multi-host.md");
        assert_eq!(
            catalog_names(
                &std::fs::read_to_string(&host_page).expect("multi-host reference"),
                "Scheduling and claims"
            ),
            pinned_routines,
            "published owner pins must match initialization [ORB-17809]: {}",
            host_page.display()
        );
    }
    assert_eq!(
        catalog_names(
            &std::fs::read_to_string(repo.join("docs/design/auto-tasks/1_overview.md"))
                .expect("design catalog"),
            "Embedded default catalog",
        ),
        auto_tasks,
        "the design catalog must match initialization [ORB-17809]"
    );
}
