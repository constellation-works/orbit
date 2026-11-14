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
/// the workspace after an upgrade must refresh only copies Orbit still owns.
#[test]
fn runtime_open_refreshes_unmodified_workspace_catalog_copies() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "runtime_open_refreshes_unmodified_workspace_catalog_copies",
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
        // Retired entries and operator opt-outs must retain their provenance.
        std::fs::write(catalog.join("removed_default.yaml"), &old).expect("retired copy");
        std::fs::write(
            catalog.join(MANAGED_ASSET_MANIFEST_FILE),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "assetKind": kind,
                "assets": {name: sha256(&old), edited_name: sha256(&edited_old), "removed_default": sha256(&old)},
                "optedOut": ["removed_opt_out"]
            })).expect("legacy manifest"),
        ).expect("record older shipped digests");
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
        assert_eq!(
            std::fs::read(catalog.join("removed_default.yaml")).unwrap(),
            old.as_bytes()
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
        assert_eq!(manifest["assets"]["removed_default"], sha256(&old));
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
    let preserved: Vec<_> = [
        "activities/agent_implement.yaml",
        "activities/operator.yaml",
        "activities/candidate_resume.yaml",
        "activities/removed_default.yaml",
        "jobs/task_pr_pipeline.yaml",
        "jobs/operator.yaml",
        "jobs/task_local_pipeline.yaml",
        "jobs/removed_default.yaml",
    ]
    .into_iter()
    .map(|relative| {
        let path = workspace.join("resources").join(relative);
        let bytes = std::fs::read(&path).unwrap();
        (path, bytes)
    })
    .collect();
    reconcile_workspace_managed_artifacts(&global, &workspace, None, "main", false)
        .expect("explicit sync refreshes copies");
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
