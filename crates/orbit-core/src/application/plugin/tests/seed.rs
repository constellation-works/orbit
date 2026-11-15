//! A plugin auto-task whose seeded `<ns>-<name>` equals a shipped default is
//! refused, with or without `--force`; any other name seeds as before.

use orbit_common::OrbitError;
use orbit_tools::plugin::load_plugin_dir;

use super::definition_fixture::DefinitionPlugin;
use crate::application::plugin::seed::{PluginSeedAction, seed_plugin_definitions};
use crate::runtime::plugin::definitions::{load_plugin_definitions, shipped_job_names};

/// `code` + `review` seeds as `code-review`, a shipped default: seeding must
/// refuse it before writing anything, so `--force` cannot overwrite the
/// default and inherit its settings.
#[test]
fn plugin_auto_task_colliding_with_shipped_default_is_refused() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "plugin_auto_task_colliding_with_shipped_default_is_refused",
    ) {
        return;
    }

    let fixture = super::fixture::PluginFixture::new();
    let mut colliding = DefinitionPlugin::new("code");
    colliding.auto_task = "review";
    let source = colliding.write(&fixture);
    let plugin = load_plugin_dir(&source).expect("load colliding plugin");
    let definitions =
        load_plugin_definitions(&plugin, &shipped_job_names()).unwrap_or_else(|message| {
            panic!("colliding plugin definitions must load: {message}");
        });

    for force in [false, true] {
        let routines_dir = tempfile::tempdir().expect("routines dir");
        let auto_tasks_dir = tempfile::tempdir().expect("auto-tasks dir");
        let error = seed_plugin_definitions(
            &plugin,
            &definitions,
            routines_dir.path(),
            auto_tasks_dir.path(),
            force,
        )
        .expect_err("a plugin auto-task colliding with a shipped default must be refused");
        assert!(
            matches!(error, OrbitError::InvalidInput(_)),
            "a shipped-default collision must be a typed invalid input, got: {error}"
        );
        let message = error.to_string();
        assert!(
            message.contains("code-review"),
            "the refusal must name the colliding seeded name, got: {message}"
        );
        assert!(
            message.contains("shipped default"),
            "the refusal must name the shipped-default collision, got: {message}"
        );
        assert!(
            std::fs::read_dir(routines_dir.path())
                .expect("read routines dir")
                .next()
                .is_none(),
            "a refused auto-task must not leave a seeded routine behind (force={force})"
        );
        assert!(
            std::fs::read_dir(auto_tasks_dir.path())
                .expect("read auto-tasks dir")
                .next()
                .is_none(),
            "a refused auto-task must not leave a seeded file behind (force={force})"
        );
    }
}

/// A name outside the shipped catalog still seeds, then reports unchanged on
/// a second pass.
#[test]
fn non_colliding_plugin_auto_task_seeds_as_before() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "non_colliding_plugin_auto_task_seeds_as_before",
    ) {
        return;
    }

    let fixture = super::fixture::PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    let plugin = load_plugin_dir(&source).expect("load plugin");
    let definitions =
        load_plugin_definitions(&plugin, &shipped_job_names()).unwrap_or_else(|message| {
            panic!("plugin definitions must load: {message}");
        });

    let routines_dir = tempfile::tempdir().expect("routines dir");
    let auto_tasks_dir = tempfile::tempdir().expect("auto-tasks dir");
    let outcomes = seed_plugin_definitions(
        &plugin,
        &definitions,
        routines_dir.path(),
        auto_tasks_dir.path(),
        false,
    )
    .expect("a non-colliding plugin auto-task must seed");
    assert_eq!(
        outcomes.len(),
        2,
        "the fixture seeds one routine and one auto-task: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.name == "graph-reindex"
                && outcome.action == PluginSeedAction::Created),
        "the non-colliding auto-task must be created: {outcomes:?}"
    );
    assert!(
        auto_tasks_dir.path().join("graph-reindex.yaml").exists(),
        "the seeded auto-task file must exist"
    );

    let reseed = seed_plugin_definitions(
        &plugin,
        &definitions,
        routines_dir.path(),
        auto_tasks_dir.path(),
        false,
    )
    .expect("reseeding unchanged definitions must succeed");
    assert!(
        reseed
            .iter()
            .all(|outcome| outcome.action == PluginSeedAction::Unchanged),
        "reseeding unchanged definitions must report unchanged: {reseed:?}"
    );
}
