use std::path::Path;

use orbit_types::identity::lower_hex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir_in;

const MANAGED_ASSET_MANIFEST_FILE: &str = ".orbit-managed-assets.json";
use orbit_config::ConfigSeed;
use orbit_core::OrbitRuntime;
use orbit_core::RoutineSeedIdentity;
use orbit_core::bootstrap::init::{InitOptions, InitResult, init_workspace_at_root};

fn init_global(root: &Path) -> InitResult {
    init_workspace_at_root(
        root,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        },
    )
    .expect("initialize global defaults")
}

fn sha256(content: &str) -> String {
    lower_hex(Sha256::digest(content.as_bytes()))
}

fn add_managed_manifest_entry(dir: &Path, name: &str, content: &str) {
    let manifest_path = dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let raw = std::fs::read_to_string(&manifest_path).expect("read managed manifest");
    let mut manifest: Value = serde_json::from_str(&raw).expect("parse managed manifest");
    manifest["assets"]
        .as_object_mut()
        .expect("manifest assets object")
        .insert(name.to_string(), Value::String(sha256(content)));
    std::fs::write(
        manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize managed manifest")
        ),
    )
    .expect("write previous managed manifest");
}

/// Older workspace init versions materialized host defaults locally. Opening
/// the workspace after an upgrade refreshes current copies and retires dropped
/// copies by their recorded provenance.
#[test]
fn runtime_open_refreshes_and_retires_legacy_workspace_catalog_copies() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "runtime_open_refreshes_and_retires_legacy_workspace_catalog_copies",
    ) {
        return;
    }
    let root = tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).expect("isolated roots");
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    init_global(&global);

    for (kind, directory, name) in [
        ("activity", "activities", "task_pilot"),
        ("job", "jobs", "task_pilot_pipeline"),
    ] {
        let catalog = workspace.join("resources").join(directory);
        std::fs::create_dir_all(&catalog).expect("legacy workspace catalog");
        let shipped = std::fs::read_to_string(
            global
                .join("resources")
                .join(directory)
                .join(format!("{name}.yaml")),
        )
        .expect("current shipped body");
        let old = format!("# earlier release\n{shipped}");
        std::fs::write(catalog.join(format!("{name}.yaml")), &old).expect("older managed body");
        // A second shipped name has managed provenance but changed bytes.
        let edited_name = if kind == "activity" {
            "agent_implement"
        } else {
            "task_pr_pipeline"
        };
        let edited_shipped = std::fs::read_to_string(
            global
                .join("resources")
                .join(directory)
                .join(format!("{edited_name}.yaml")),
        )
        .expect("second shipped default");
        let edited_old = format!("# earlier release\n{edited_shipped}");
        let edited = format!("# operator edit\n{edited_old}");
        std::fs::write(catalog.join(format!("{edited_name}.yaml")), &edited).expect("edited copy");
        let authored = b"# user-authored activity or job\n";
        std::fs::write(catalog.join("operator.yaml"), authored).expect("user file");
        let collision_name = if kind == "activity" {
            "candidate_resume"
        } else {
            "task_local_pipeline"
        };
        std::fs::write(catalog.join(format!("{collision_name}.yaml")), authored)
            .expect("untracked shipped-name collision");
        // `agent_explore` reproduces the dropped ws_orbit-graph activity;
        // jobs use the same refresh policy for their own retired copies.
        let retired_name = if kind == "activity" {
            "agent_explore"
        } else {
            "removed_default"
        };
        std::fs::write(catalog.join(format!("{retired_name}.yaml")), &old).expect("retired copy");
        let mut managed_assets = serde_json::Map::new();
        managed_assets.insert(name.to_string(), serde_json::json!(sha256(&old)));
        managed_assets.insert(
            edited_name.to_string(),
            serde_json::json!(sha256(&edited_old)),
        );
        managed_assets.insert(retired_name.to_string(), serde_json::json!(sha256(&old)));
        std::fs::write(
            catalog.join(MANAGED_ASSET_MANIFEST_FILE),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "assetKind": kind,
                "assets": managed_assets,
                "optedOut": ["removed_opt_out"]
            }))
            .expect("legacy manifest"),
        )
        .expect("record older shipped digests");
    }

    // The global stamp is already current: workspace convergence must not
    // depend on this open also upgrading the host-global catalogs.
    let runtime =
        OrbitRuntime::from_roots(&global, &workspace).expect("automatic upgrade on runtime open");
    for (directory, name, edited_name) in [
        ("activities", "task_pilot", "agent_implement"),
        ("jobs", "task_pilot_pipeline", "task_pr_pipeline"),
    ] {
        let catalog = workspace.join("resources").join(directory);
        let shipped = std::fs::read(
            global
                .join("resources")
                .join(directory)
                .join(format!("{name}.yaml")),
        )
        .expect("current shipped bytes");
        assert_eq!(
            std::fs::read(catalog.join(format!("{name}.yaml"))).unwrap(),
            shipped,
            "an unmodified managed workspace copy must converge automatically"
        );
        let old = format!(
            "# earlier release\n{}",
            String::from_utf8(shipped.clone()).unwrap()
        );
        let edited_shipped = std::fs::read_to_string(
            global
                .join("resources")
                .join(directory)
                .join(format!("{edited_name}.yaml")),
        )
        .expect("second shipped default");
        let edited_old = format!("# earlier release\n{edited_shipped}");
        assert_eq!(
            std::fs::read(catalog.join(format!("{edited_name}.yaml"))).unwrap(),
            format!("# operator edit\n{edited_old}").as_bytes()
        );
        assert_eq!(
            std::fs::read(catalog.join("operator.yaml")).unwrap(),
            b"# user-authored activity or job\n"
        );
        let collision_name = if directory == "activities" {
            "candidate_resume"
        } else {
            "task_local_pipeline"
        };
        assert_eq!(
            std::fs::read(catalog.join(format!("{collision_name}.yaml"))).unwrap(),
            b"# user-authored activity or job\n",
            "even a shipped-name collision remains operator-owned"
        );
        let retired_name = if directory == "activities" {
            "agent_explore"
        } else {
            "removed_default"
        };
        assert!(
            !catalog.join(format!("{retired_name}.yaml")).exists(),
            "an unmodified tracked default dropped from the package is retired"
        );
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(catalog.join(MANAGED_ASSET_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest["assets"][name],
            sha256(std::str::from_utf8(&shipped).unwrap())
        );
        assert_eq!(manifest["assets"][edited_name], sha256(&edited_old));
        assert!(
            manifest["assets"].get(retired_name).is_none(),
            "retirement removes the stale manifest entry"
        );
        assert!(
            manifest["assets"].get(collision_name).is_none(),
            "refresh must not adopt an untracked copy"
        );
        assert_eq!(manifest["optedOut"], serde_json::json!(["removed_opt_out"]));
        assert!(
            !catalog
                .join(if directory == "activities" {
                    "candidate_validate.yaml"
                } else {
                    "task_auto_pipeline.yaml"
                })
                .exists(),
            "refresh must not seed absent workspace defaults"
        );
        // Reproduce a denied/deferred upgrade on an already-open runtime:
        // doctor and check mode must explain it without changing the files.
        std::fs::write(catalog.join(format!("{name}.yaml")), &old).expect("stale copy again");
        add_managed_manifest_entry(&catalog, name, &old);
    }
    use orbit_core::application::health::artifact::{ArtifactCondition, ArtifactProvenance};
    use orbit_core::{
        ManagedArtifactOutcome, ManagedArtifactScope, reconcile_workspace_managed_artifacts,
    };
    let activity_catalog = workspace.join("resources/activities");
    let retired_activity = format!(
        "# earlier release\n{}",
        std::fs::read_to_string(global.join("resources/activities/task_pilot.yaml"))
            .expect("current activity body")
    );
    let modified_retired_activity = format!("# operator edit\n{retired_activity}");
    std::fs::write(
        activity_catalog.join("agent_explore.yaml"),
        &retired_activity,
    )
    .expect("recreate dropped activity");
    std::fs::write(
        activity_catalog.join("verify_finding.yaml"),
        &modified_retired_activity,
    )
    .expect("write modified dropped activity");
    add_managed_manifest_entry(&activity_catalog, "agent_explore", &retired_activity);
    add_managed_manifest_entry(&activity_catalog, "verify_finding", &retired_activity);
    let health = runtime
        .inspect_definition_artifacts()
        .expect("doctor diagnoses legacy copies");
    let preview = reconcile_workspace_managed_artifacts(&global, &workspace, None, "main", true)
        .expect("preview local copy refresh");
    for (directory, name) in [
        ("activities", "task_pilot"),
        ("jobs", "task_pilot_pipeline"),
    ] {
        let path = workspace
            .join("resources")
            .join(directory)
            .join(format!("{name}.yaml"));
        let finding = health
            .iter()
            .flat_map(|health| &health.findings)
            .find(|finding| finding.path == path && finding.condition == ArtifactCondition::Stale)
            .expect("doctor identifies the unmodified stale workspace copy");
        assert_eq!(finding.provenance, ArtifactProvenance::OrbitWritten);
        assert!(finding.detail.contains("stale managed workspace copy"));
        assert!(finding.remediation.contains("orbit workspace sync"));
        assert!(preview.actions.iter().any(|action| action.path == path
            && action.scope == ManagedArtifactScope::WorkspaceLocal
            && action.outcome == ManagedArtifactOutcome::Refreshed));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# earlier release\n"),
            "doctor and sync --check must not write"
        );
    }
    assert!(
        preview.actions.iter().any(|action| {
            action.scope == ManagedArtifactScope::WorkspaceLocal
                && action.kind == "activity"
                && action.name == "agent_explore"
                && action.outcome == ManagedArtifactOutcome::Retired
        }),
        "check mode reports retirement of the dropped package activity: {preview:?}"
    );
    assert!(
        preview.actions.iter().any(|action| {
            action.scope == ManagedArtifactScope::WorkspaceLocal
                && action.kind == "activity"
                && action.name == "verify_finding"
                && action.outcome == ManagedArtifactOutcome::Preserved
                && action.detail.as_deref().is_some_and(|detail| {
                    detail.contains(".retired-managed/activities/verify_finding.yaml")
                })
        }),
        "check mode reports the modified activity's preservation destination: {preview:?}"
    );
    assert!(
        activity_catalog.join("agent_explore.yaml").is_file(),
        "check mode leaves the unmodified retired copy in place"
    );
    assert!(
        activity_catalog.join("verify_finding.yaml").is_file(),
        "check mode leaves the modified retired copy in place"
    );
    let preserved: Vec<_> = [
        "activities/agent_implement.yaml",
        "activities/operator.yaml",
        "activities/candidate_resume.yaml",
        "jobs/task_pr_pipeline.yaml",
        "jobs/operator.yaml",
        "jobs/task_local_pipeline.yaml",
    ]
    .into_iter()
    .map(|relative| {
        let path = workspace.join("resources").join(relative);
        let bytes = std::fs::read(&path).unwrap();
        (path, bytes)
    })
    .collect();
    let applied = reconcile_workspace_managed_artifacts(&global, &workspace, None, "main", false)
        .expect("explicit sync refreshes copies");
    let preserved_retired_activity =
        workspace.join("resources/.retired-managed/activities/verify_finding.yaml");
    assert!(!activity_catalog.join("agent_explore.yaml").exists());
    assert!(!activity_catalog.join("verify_finding.yaml").exists());
    assert_eq!(
        std::fs::read_to_string(&preserved_retired_activity).unwrap(),
        modified_retired_activity,
        "the modified retired activity is moved outside the active catalog"
    );
    assert!(
        applied.actions.iter().any(|action| {
            action.scope == ManagedArtifactScope::WorkspaceLocal
                && action.kind == "activity"
                && action.name == "verify_finding"
                && action.detail.as_deref().is_some_and(|detail| {
                    detail.contains(".retired-managed/activities/verify_finding.yaml")
                })
        }),
        "sync reports where it preserved the modified retired activity: {applied:?}"
    );
    let activity_manifest: Value = serde_json::from_slice(
        &std::fs::read(activity_catalog.join(MANAGED_ASSET_MANIFEST_FILE)).unwrap(),
    )
    .unwrap();
    assert!(activity_manifest["assets"].get("agent_explore").is_none());
    assert!(activity_manifest["assets"].get("verify_finding").is_none());
    let job_manifest: Value = serde_json::from_slice(
        &std::fs::read(
            workspace
                .join("resources/jobs")
                .join(MANAGED_ASSET_MANIFEST_FILE),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(job_manifest["assets"].get("removed_default").is_none());
    for (path, bytes) in preserved {
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "sync preserves {}",
            path.display()
        );
    }
    for (directory, name) in [
        ("activities", "task_pilot"),
        ("jobs", "task_pilot_pipeline"),
    ] {
        let relative = format!("resources/{directory}/{name}.yaml");
        assert_eq!(
            std::fs::read(workspace.join(&relative)).unwrap(),
            std::fs::read(global.join(&relative)).unwrap()
        );
    }
}

// --- Definition-artifact health [ORB-10800] ---------------------------------

mod artifacts {
    use super::*;
    use std::path::PathBuf;

    fn init_workspace(root: &Path) -> (PathBuf, PathBuf) {
        let global_root = root.join("global");
        let workspace_root = root.join("repo/.orbit");
        init_global(&global_root);
        init_workspace_at_root(
            &workspace_root,
            InitOptions {
                refresh_defaults: true,
                global_root_override: Some(global_root.clone()),
                routine_seed_identity: Some(
                    RoutineSeedIdentity::new("repo", "hm_test", "main")
                        .expect("routine seed identity"),
                ),
                config_seed: Some(ConfigSeed::default()),
                ..Default::default()
            },
        )
        .expect("initialize workspace");
        (global_root, workspace_root)
    }

    /// Removal refuses a manifest key that would escape the managed directory,
    /// and never follows a symlink at the boundary.
    #[test]
    fn removal_rejects_escaping_paths_and_does_not_follow_symlinks() {
        if !crate::admission_audit::isolate(
            module_path!(),
            "removal_rejects_escaping_paths_and_does_not_follow_symlinks",
        ) {
            return;
        }

        let root =
            tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());
        let auto_tasks_dir = workspace_root.join("auto_tasks");

        // A traversing key is refused when the manifest is loaded, before any
        // removal is attempted.
        let manifest_path = auto_tasks_dir.join(MANAGED_ASSET_MANIFEST_FILE);
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("read manifest"))
                .expect("parse manifest");
        manifest["assets"]
            .as_object_mut()
            .expect("assets object")
            .insert("../escaped".to_string(), Value::String(sha256("anything")));
        std::fs::write(
            &manifest_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&manifest).expect("serialize manifest")
            ),
        )
        .expect("write traversing manifest");

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        let error = runtime
            .remove_stale_definition_artifacts()
            .expect_err("a traversing manifest key must be refused");
        assert!(
            error.to_string().contains("not a safe managed asset path"),
            "{error}"
        );

        // A symlinked artifact is left in place: deleting it would act on a
        // target outside the catalog Orbit manages.
        #[cfg(unix)]
        {
            std::fs::write(
                &manifest_path,
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&{
                        let mut clean: Value = serde_json::from_str(
                            &std::fs::read_to_string(&manifest_path).expect("read manifest"),
                        )
                        .expect("parse manifest");
                        clean["assets"]
                            .as_object_mut()
                            .expect("assets object")
                            .remove("../escaped");
                        clean
                    })
                    .expect("serialize manifest")
                ),
            )
            .expect("restore manifest");

            let outside = root.path().join("outside.yaml");
            std::fs::write(&outside, "outside content\n").expect("write link target");
            let link = auto_tasks_dir.join("linked-auto-task.yaml");
            std::os::unix::fs::symlink(&outside, &link).expect("create symlinked artifact");
            add_managed_manifest_entry(&auto_tasks_dir, "linked-auto-task", "outside content\n");

            let runtime =
                OrbitRuntime::from_roots(&global_root, &workspace_root).expect("rebuild runtime");
            assert_eq!(
                runtime
                    .remove_stale_definition_artifacts()
                    .expect("retire pass"),
                0,
                "a symlinked artifact is not removed"
            );
            assert!(link.symlink_metadata().is_ok(), "the symlink survives");
            assert_eq!(
                std::fs::read_to_string(&outside).expect("link target survives"),
                "outside content\n"
            );
        }
    }
}
