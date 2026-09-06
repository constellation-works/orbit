use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::super::MANAGED_ASSET_MANIFEST_FILE;
#[cfg(unix)]
use super::super::job::seed_default_jobs;
use crate::OrbitRuntime;
use crate::application::job::JobCatalogFilter;
use crate::bootstrap::activity::seed_default_activities;
use crate::bootstrap::init::{InitOptions, InitResult, init_workspace_at_root};
use crate::runtime::OrbitRuntimeRoots;
use orbit_config::ConfigSeed;

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
    format!("{:x}", Sha256::digest(content.as_bytes()))
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

fn activity_yaml(name: &str, tool: Option<&str>) -> String {
    match tool {
        Some(tool) => format!(
            r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: agent_loop
  description: retired test activity
  input_schema_json: {{type: object}}
  output_schema_json: {{type: object}}
  instruction: retired
  tools: [{tool}]
  max_iterations: 1
"#
        ),
        None => format!(
            r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: deterministic
  description: operator activity
  input_schema_json: {{type: object}}
  output_schema_json: {{type: object}}
  action: sleep
  config: {{}}
"#
        ),
    }
}

fn job_yaml(name: &str, action: Option<&str>) -> String {
    let steps = action.map_or_else(
        || "  steps: []\n".to_string(),
        |action| {
            format!(
                r#"  steps:
    - id: removed
      spec:
        type: deterministic
        action: {action}
        config: {{}}
"#
            )
        },
    );
    format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  max_active_runs: 1
{steps}"#
    )
}

#[cfg(unix)]
fn make_read_only(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))
        .expect("make managed asset directory read-only");
}

#[cfg(unix)]
fn make_writable(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("restore managed asset directory permissions");
}

#[cfg(unix)]
#[test]
fn steady_state_reconcile_skips_asset_and_manifest_writes() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    let jobs_dir = global_root.join("resources/jobs");
    make_read_only(&activities_dir);
    make_read_only(&jobs_dir);

    let activities = seed_default_activities(&activities_dir, true);
    let jobs = seed_default_jobs(&jobs_dir, true);

    make_writable(&activities_dir);
    make_writable(&jobs_dir);

    let activities = activities.expect("unchanged activities do not require write access");
    let jobs = jobs.expect("unchanged jobs do not require write access");

    assert_eq!(activities.refreshed, 0);
    assert_eq!(activities.retired, 0);
    assert!(activities.warnings.is_empty());
    assert_eq!(jobs.refreshed, 0);
    assert_eq!(jobs.retired, 0);
    assert!(jobs.warnings.is_empty());
}

#[cfg(unix)]
#[test]
fn runtime_bootstrap_reads_seeded_global_assets_without_write_access() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    let jobs_dir = global_root.join("resources/jobs");
    make_read_only(&activities_dir);
    make_read_only(&jobs_dir);

    let result = OrbitRuntime::from_roots(&global_root, &workspace_root)
        .and_then(|runtime| runtime.list_job_catalog_with_last_run(true, JobCatalogFilter::All));

    make_writable(&activities_dir);
    make_writable(&jobs_dir);

    result.expect("ordinary read-only runtime operation succeeds");
}

#[test]
fn refresh_writes_asset_and_manifest_when_embedded_digest_changed() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    let path = activities_dir.join("sleep.yaml");
    let original = std::fs::read_to_string(&path).expect("read seeded sleep activity");
    let stale = format!("{original}# stale digest\n");
    std::fs::write(&path, &stale).expect("materialize drifted bundled asset");
    add_managed_manifest_entry(&activities_dir, "sleep", &stale);

    let refreshed =
        seed_default_activities(&activities_dir, true).expect("refresh drifted bundled asset");
    assert!(
        refreshed.refreshed >= 1,
        "a digest change must rewrite the asset, got {refreshed:?}"
    );
    assert!(refreshed.warnings.is_empty(), "{:?}", refreshed.warnings);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read refreshed sleep activity"),
        original
    );

    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(activities_dir.join(MANAGED_ASSET_MANIFEST_FILE))
            .expect("read refreshed manifest"),
    )
    .expect("parse refreshed manifest");
    assert_eq!(
        manifest["assets"]["sleep"].as_str().expect("sleep digest"),
        sha256(&original)
    );
}

#[test]
fn runtime_bootstrap_refreshes_orbit_written_stale_activity_before_catalog_load() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    let path = activities_dir.join("agent_implement.yaml");
    let current = std::fs::read_to_string(&path).expect("read current activity");
    let stale = current.replacen("  tools:\n", "  tools:\n    - fs.read\n", 1);
    assert_ne!(stale, current, "fixture must contain the retired tool");
    std::fs::write(&path, &stale).expect("write stale activity");
    add_managed_manifest_entry(&activities_dir, "agent_implement", &stale);

    let runtime = OrbitRuntime::initialize_from_resolved_roots(
        OrbitRuntimeRoots {
            global_root: global_root.clone(),
            shared_root: workspace_root.clone(),
            local_root: workspace_root.clone(),
        },
        None,
    )
    .expect("bootstrap refreshes the Orbit-written stale activity");
    runtime
        .v2_activity_catalog()
        .expect("the current binary loads the refreshed catalog");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read refreshed activity"),
        current
    );
}

#[test]
fn runtime_bootstrap_preserves_locally_modified_stale_managed_activity() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    let path = activities_dir.join("agent_implement.yaml");
    let current = std::fs::read_to_string(&path).expect("read current activity");
    let stale = current.replacen("  tools:\n", "  tools:\n    - fs.read\n", 1);
    let locally_modified = format!("{stale}# operator edit\n");
    std::fs::write(&path, &locally_modified).expect("write locally modified stale activity");
    add_managed_manifest_entry(&activities_dir, "agent_implement", &stale);

    OrbitRuntime::initialize_from_resolved_roots(
        OrbitRuntimeRoots {
            global_root: global_root.clone(),
            shared_root: workspace_root.clone(),
            local_root: workspace_root,
        },
        None,
    )
    .expect("bootstrap preserves a locally modified managed activity");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read preserved activity"),
        locally_modified
    );
}

#[test]
fn refresh_retires_managed_activity_and_job_assets_and_is_idempotent() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    let jobs_dir = global_root.join("resources/jobs");
    let clean_activity_name = "retired_activity_clean";
    let modified_activity_name = "retired_activity_modified";
    let clean_job_name = "retired_job_clean";
    let modified_job_name = "retired_job_modified";
    let clean_activity = activity_yaml(clean_activity_name, Some("removed.tool"));
    let original_modified_activity = activity_yaml(modified_activity_name, Some("removed.tool"));
    let clean_job = job_yaml(clean_job_name, Some("removed_action"));
    let original_modified_job = job_yaml(modified_job_name, Some("removed_action"));

    for (dir, name, content) in [
        (
            &activities_dir,
            clean_activity_name,
            clean_activity.as_str(),
        ),
        (
            &activities_dir,
            modified_activity_name,
            original_modified_activity.as_str(),
        ),
        (&jobs_dir, clean_job_name, clean_job.as_str()),
        (&jobs_dir, modified_job_name, original_modified_job.as_str()),
    ] {
        add_managed_manifest_entry(dir, name, content);
        std::fs::write(dir.join(format!("{name}.yaml")), content)
            .expect("materialize previous managed asset");
    }

    let modified_activity = activity_yaml(modified_activity_name, Some("still.removed"));
    let modified_job = job_yaml(modified_job_name, Some("still_removed"));
    std::fs::write(
        activities_dir.join(format!("{modified_activity_name}.yaml")),
        &modified_activity,
    )
    .expect("locally modify retired activity");
    std::fs::write(
        jobs_dir.join(format!("{modified_job_name}.yaml")),
        &modified_job,
    )
    .expect("locally modify retired job");

    let reconciled = init_global(&global_root);
    assert_eq!(reconciled.retired_default_activities, 2);
    assert_eq!(reconciled.retired_default_jobs, 2);
    assert_eq!(reconciled.managed_asset_warnings.len(), 2);
    for warning in &reconciled.managed_asset_warnings {
        assert!(warning.contains("locally modified"));
        assert!(warning.contains("preserved"));
        assert!(warning.contains("migrate"));
    }

    assert!(
        !activities_dir
            .join(format!("{clean_activity_name}.yaml"))
            .exists()
    );
    assert!(!jobs_dir.join(format!("{clean_job_name}.yaml")).exists());
    assert_eq!(
        std::fs::read_to_string(global_root.join(format!(
            "resources/.retired-managed/activities/{modified_activity_name}.yaml"
        )))
        .expect("read preserved modified activity"),
        modified_activity
    );
    assert_eq!(
        std::fs::read_to_string(global_root.join(format!(
            "resources/.retired-managed/jobs/{modified_job_name}.yaml"
        )))
        .expect("read preserved modified job"),
        modified_job
    );

    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build reconciled runtime");
    let activity_catalog = runtime
        .v2_activity_catalog()
        .expect("load activity catalog");
    for retired in [clean_activity_name, modified_activity_name] {
        assert!(
            !activity_catalog.names().any(|name| name == retired),
            "retired activity `{retired}` must not remain active"
        );
    }
    let jobs = runtime
        .list_job_catalog_with_last_run(true, JobCatalogFilter::All)
        .expect("list reconciled jobs");
    for retired in [clean_job_name, modified_job_name] {
        assert!(
            !jobs.iter().any(|(entry, _)| entry.job_id == retired),
            "retired job `{retired}` must not remain active"
        );
    }

    let repeated = init_global(&global_root);
    assert_eq!(repeated.retired_default_activities, 0);
    assert_eq!(repeated.retired_default_jobs, 0);
    assert!(repeated.managed_asset_warnings.is_empty());
}

// --- Definition-artifact health [ORB-10800] ---------------------------------

mod artifacts {
    use super::*;
    use std::path::PathBuf;

    use crate::application::artifact_health::{
        ArtifactCondition, ArtifactHealth, ArtifactKind, ArtifactProvenance,
    };

    fn init_workspace(root: &Path) -> (PathBuf, PathBuf) {
        let global_root = root.join("global");
        let workspace_root = root.join("repo/.orbit");
        init_global(&global_root);
        init_workspace_at_root(
            &workspace_root,
            InitOptions {
                refresh_defaults: true,
                global_root_override: Some(global_root.clone()),
                routine_host_id: Some("test-host".to_string()),
                config_seed: Some(ConfigSeed::default()),
                ..Default::default()
            },
        )
        .expect("initialize workspace");
        (global_root, workspace_root)
    }

    fn health_of(report: &[ArtifactHealth], kind: ArtifactKind) -> &ArtifactHealth {
        report
            .iter()
            .find(|health| health.kind == kind)
            .unwrap_or_else(|| panic!("missing artifact health for {kind:?}"))
    }

    /// Criterion: all five kinds record digest provenance after seeding.
    #[test]
    fn every_artifact_kind_records_digest_provenance_after_init() {
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());

        for dir in [
            global_root.join("skills"),
            global_root.join("resources/jobs"),
            global_root.join("resources/activities"),
            workspace_root.join("auto_tasks"),
            workspace_root.join("routines"),
        ] {
            let manifest = dir.join(MANAGED_ASSET_MANIFEST_FILE);
            let raw = std::fs::read_to_string(&manifest)
                .unwrap_or_else(|e| panic!("read manifest {}: {e}", manifest.display()));
            let parsed: Value = serde_json::from_str(&raw).expect("parse manifest");
            assert!(
                parsed["assets"]
                    .as_object()
                    .expect("assets object")
                    .values()
                    .all(|digest| digest.as_str().is_some_and(|d| d.len() == 64)),
                "every managed asset in {} must record a sha256 digest",
                dir.display()
            );
        }

        // Skill manifests key on the relative path, not a bare stem, because
        // skills are directory trees.
        let skill_manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(global_root.join("skills").join(MANAGED_ASSET_MANIFEST_FILE))
                .expect("read skill manifest"),
        )
        .expect("parse skill manifest");
        let keys: Vec<&str> = skill_manifest["assets"]
            .as_object()
            .expect("assets object")
            .keys()
            .map(String::as_str)
            .collect();
        assert!(keys.contains(&"orbit/SKILL.md"), "{keys:?}");
        assert!(
            keys.contains(&"orbit/references/task-review.md"),
            "reference files are managed too: {keys:?}"
        );
        assert!(
            keys.contains(&"orbit/references/setup/first-run.md"),
            "references nest, so a manifest key is a full relative path: {keys:?}"
        );
        assert!(
            keys.contains(&"orbit/references/setup/linux-sandbox.md"),
            "Linux sandbox setup is an embedded managed reference: {keys:?}"
        );
    }

    /// A freshly initialized workspace is clean on every artifact kind.
    #[test]
    fn freshly_initialized_workspace_reports_no_artifact_findings() {
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());
        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");

        let report = runtime
            .inspect_definition_artifacts()
            .expect("inspect artifacts");
        assert_eq!(report.len(), 5);
        for health in &report {
            assert!(
                health.findings.is_empty(),
                "{:?} reported findings on a fresh workspace: {:?}",
                health.kind,
                health.findings
            );
            assert!(health.scanned > 0, "{:?} scanned nothing", health.kind);
        }
    }

    /// Skill directories remain visible to health checks after their entry
    /// point disappears, regardless of whether the manifest still remembers
    /// the deleted payload. Repair never treats the directory itself as proof
    /// of Orbit authorship.
    #[test]
    fn skill_residue_is_reported_and_fix_preserves_unproven_content() {
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());
        let skills_dir = global_root.join("skills");

        let untracked_dir = skills_dir.join("orbit-search");
        let untracked_reference = untracked_dir.join("references/manual.md");
        std::fs::create_dir_all(untracked_reference.parent().expect("reference parent"))
            .expect("create untracked residue");
        std::fs::write(&untracked_reference, "operator reference\n")
            .expect("write untracked residue");

        let tracked_dir = skills_dir.join("orbit-task");
        let tracked_reference = tracked_dir.join("references/task.md");
        std::fs::create_dir_all(tracked_reference.parent().expect("reference parent"))
            .expect("create tracked residue");
        let previous_reference = "previous Orbit reference\n";
        let modified_reference = "operator-modified reference\n";
        std::fs::write(&tracked_reference, modified_reference)
            .expect("write locally modified tracked residue");
        add_managed_manifest_entry(
            &skills_dir,
            "orbit-task/SKILL.md",
            "deleted Orbit entry point\n",
        );
        add_managed_manifest_entry(
            &skills_dir,
            "orbit-task/references/task.md",
            previous_reference,
        );

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        let report = runtime
            .inspect_definition_artifacts()
            .expect("inspect artifacts");
        let skill_health = health_of(&report, ArtifactKind::Skill);
        assert_eq!(
            skill_health.scanned, 4,
            "two healthy skills plus two residues"
        );
        for residue in [&untracked_dir, &tracked_dir] {
            let name = residue
                .file_name()
                .and_then(|name| name.to_str())
                .expect("residue name");
            let finding = skill_health
                .findings
                .iter()
                .find(|finding| {
                    finding.name == name && finding.condition == ArtifactCondition::Residual
                })
                .expect("residual skill directory is reported");
            assert_eq!(finding.path, *residue);
            assert_eq!(finding.provenance, ArtifactProvenance::UserAuthored);
            assert!(
                finding.detail.contains(residue.to_string_lossy().as_ref()),
                "{}",
                finding.detail
            );
            assert!(!finding.is_unloadable_shipped_default());
        }

        assert_eq!(
            runtime
                .remove_stale_definition_artifacts()
                .expect("repair stale artifacts"),
            1,
            "only the manifest-tracked modified reference leaves the active catalog"
        );
        assert_eq!(
            std::fs::read_to_string(&untracked_reference).expect("untracked residue survives"),
            "operator reference\n"
        );
        let preserved = global_root.join(".retired-managed/skills/orbit-task/references/task.md");
        assert_eq!(
            std::fs::read_to_string(&preserved).expect("modified residue is preserved"),
            modified_reference
        );

        let after_fix = runtime
            .inspect_definition_artifacts()
            .expect("inspect after repair");
        let remaining = health_of(&after_fix, ArtifactKind::Skill)
            .findings
            .iter()
            .filter(|finding| finding.condition == ArtifactCondition::Residual)
            .count();
        assert_eq!(
            remaining, 2,
            "repair leaves residual directories for review"
        );
    }

    /// Criteria: deprecated artifacts are detected across the newly covered
    /// kinds; the fix flag removes only what Orbit provably wrote and
    /// preserves a locally modified one instead of deleting it.
    #[test]
    fn fix_removes_orbit_written_deprecated_artifacts_and_preserves_modified_ones() {
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());

        let auto_tasks_dir = workspace_root.join("auto_tasks");
        let routines_dir = workspace_root.join("routines");
        let clean = "retired-auto-task";
        let modified = "retired_routine";
        let clean_body = std::fs::read_to_string(auto_tasks_dir.join("qa-sweep.yaml"))
            .expect("read a seeded auto-task to reuse as retired content");
        let modified_body = std::fs::read_to_string(routines_dir.join("task_triage.yaml"))
            .expect("read a seeded routine");

        // Both look exactly like assets a previous release seeded.
        add_managed_manifest_entry(&auto_tasks_dir, clean, &clean_body);
        std::fs::write(auto_tasks_dir.join(format!("{clean}.yaml")), &clean_body)
            .expect("materialize retired auto-task");
        add_managed_manifest_entry(&routines_dir, modified, &modified_body);
        let edited_body = format!("{modified_body}# operator edit\n");
        std::fs::write(routines_dir.join(format!("{modified}.yaml")), &edited_body)
            .expect("materialize locally modified retired routine");

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        let report = runtime
            .inspect_definition_artifacts()
            .expect("inspect artifacts");

        let auto_task_finding = health_of(&report, ArtifactKind::AutoTask)
            .findings
            .iter()
            .find(|finding| finding.name == clean)
            .expect("deprecated auto-task is reported");
        assert_eq!(auto_task_finding.condition, ArtifactCondition::Deprecated);
        assert_eq!(
            auto_task_finding.provenance,
            ArtifactProvenance::OrbitWritten
        );
        assert!(
            auto_task_finding
                .remediation
                .contains("orbit doctor --fix-stale-artifacts"),
            "{}",
            auto_task_finding.remediation
        );

        let routine_finding = health_of(&report, ArtifactKind::Routine)
            .findings
            .iter()
            .find(|finding| finding.name == modified)
            .expect("deprecated routine is reported");
        assert_eq!(routine_finding.condition, ArtifactCondition::Deprecated);
        assert_eq!(
            routine_finding.provenance,
            ArtifactProvenance::LocallyModified
        );

        let retired = runtime
            .remove_stale_definition_artifacts()
            .expect("retire deprecated artifacts");
        assert_eq!(retired, 2);

        assert!(
            !auto_tasks_dir.join(format!("{clean}.yaml")).exists(),
            "an unmodified Orbit-written deprecated artifact is deleted"
        );
        assert!(
            !routines_dir.join(format!("{modified}.yaml")).exists(),
            "a modified deprecated artifact leaves the active catalog"
        );
        assert_eq!(
            std::fs::read_to_string(
                workspace_root.join(format!(".retired-managed/routines/{modified}.yaml"))
            )
            .expect("locally modified content is preserved, not destroyed"),
            edited_body
        );

        // Idempotent: a second pass has nothing left to retire.
        assert_eq!(
            runtime
                .remove_stale_definition_artifacts()
                .expect("second retire pass"),
            0
        );
    }

    /// Criteria: a faulty *user-authored* artifact is reported but never
    /// removed or rewritten by the fix flag.
    #[test]
    fn faulty_user_authored_artifacts_are_reported_and_never_touched() {
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());

        let broken_path = workspace_root.join("routines/operator-routine.yaml");
        let broken_body = "schemaVersion: 1\nname: broken\n";
        std::fs::write(&broken_path, broken_body).expect("write malformed routine");

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        let report = runtime
            .inspect_definition_artifacts()
            .expect("inspect artifacts");
        let finding = health_of(&report, ArtifactKind::Routine)
            .findings
            .iter()
            .find(|finding| finding.name == "operator-routine")
            .expect("faulty routine is reported");
        assert_eq!(finding.condition, ArtifactCondition::Faulty);
        assert_eq!(finding.provenance, ArtifactProvenance::UserAuthored);
        assert!(
            !finding.is_unloadable_shipped_default(),
            "a workspace-authored fault must not escalate the doctor exit code"
        );
        assert!(
            finding.remediation.contains("operator-routine.yaml"),
            "{}",
            finding.remediation
        );

        assert_eq!(
            runtime
                .remove_stale_definition_artifacts()
                .expect("retire pass"),
            0
        );
        assert_eq!(
            std::fs::read_to_string(&broken_path).expect("faulty user file survives"),
            broken_body
        );
    }

    /// An Orbit-written default that no longer parses is a broken install, and
    /// is the one artifact fault that escalates to an Error row.
    #[test]
    fn unloadable_shipped_default_is_distinguished_from_a_user_authored_fault() {
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());

        // Corrupt a shipped default *and* record it as what Orbit wrote, which
        // is what a truncated install or a bad upgrade looks like on disk.
        let activities_dir = global_root.join("resources/activities");
        let corrupt = "schemaVersion: 2\nkind: Activity\nmetadata: {}\n";
        std::fs::write(activities_dir.join("sleep.yaml"), corrupt)
            .expect("corrupt shipped default");
        add_managed_manifest_entry(&activities_dir, "sleep", corrupt);

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        let report = runtime
            .inspect_definition_artifacts()
            .expect("inspect artifacts");
        let finding = health_of(&report, ArtifactKind::Activity)
            .findings
            .iter()
            .find(|finding| {
                finding.name == "sleep" && finding.condition == ArtifactCondition::Faulty
            })
            .expect("faulty shipped default is reported");
        assert_eq!(finding.provenance, ArtifactProvenance::OrbitWritten);
        assert!(finding.is_unloadable_shipped_default());
        assert!(
            finding.remediation.contains("orbit init"),
            "{}",
            finding.remediation
        );
    }

    /// An Orbit-written copy of an older release is stale; refreshing is the
    /// repair, and the fix flag deliberately leaves it alone.
    #[test]
    fn drifted_orbit_written_default_is_stale_and_not_removed_by_the_fix_flag() {
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());

        let auto_tasks_dir = workspace_root.join("auto_tasks");
        let path = auto_tasks_dir.join("qa-sweep.yaml");
        let older_release = format!(
            "{}# shipped by an older release\n",
            std::fs::read_to_string(&path).expect("read seeded auto-task")
        );
        std::fs::write(&path, &older_release).expect("simulate an older release's copy");
        add_managed_manifest_entry(&auto_tasks_dir, "qa-sweep", &older_release);

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        let finding = health_of(
            &runtime
                .inspect_definition_artifacts()
                .expect("inspect artifacts"),
            ArtifactKind::AutoTask,
        )
        .findings
        .iter()
        .find(|finding| finding.name == "qa-sweep")
        .cloned()
        .expect("drifted default is reported");
        assert_eq!(finding.condition, ArtifactCondition::Stale);
        assert_eq!(finding.provenance, ArtifactProvenance::OrbitWritten);
        assert!(
            finding.remediation.contains("orbit workspace init"),
            "{}",
            finding.remediation
        );

        assert_eq!(
            runtime
                .remove_stale_definition_artifacts()
                .expect("retire pass"),
            0,
            "staleness is repaired by refreshing, never by deleting"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("stale file survives"),
            older_release
        );
    }

    /// A user file wearing a bundled default's name keeps that default from
    /// being installed — reported as stale, and never deleted.
    #[test]
    fn untracked_collision_with_a_bundled_default_is_reported_as_stale() {
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());

        let path = workspace_root.join("auto_tasks/qa-sweep.yaml");
        let user_body = std::fs::read_to_string(&path)
            .expect("read seeded auto-task")
            .replace("enabled: false", "enabled: false # operator copy");
        std::fs::write(&path, &user_body).expect("write colliding user file");
        // Drop the managed provenance, leaving an untracked file behind.
        let manifest_path =
            workspace_root.join(format!("auto_tasks/{MANAGED_ASSET_MANIFEST_FILE}"));
        let mut manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path).expect("read auto-task manifest"),
        )
        .expect("parse manifest");
        manifest["assets"]
            .as_object_mut()
            .expect("assets object")
            .remove("qa-sweep");
        std::fs::write(
            &manifest_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&manifest).expect("serialize manifest")
            ),
        )
        .expect("write manifest without provenance");

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        let finding = health_of(
            &runtime
                .inspect_definition_artifacts()
                .expect("inspect artifacts"),
            ArtifactKind::AutoTask,
        )
        .findings
        .iter()
        .find(|finding| finding.name == "qa-sweep")
        .cloned()
        .expect("collision is reported");
        assert_eq!(finding.condition, ArtifactCondition::Stale);
        assert_eq!(finding.provenance, ArtifactProvenance::UserAuthored);

        assert_eq!(
            runtime
                .remove_stale_definition_artifacts()
                .expect("retire pass"),
            0
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("user file survives"),
            user_body
        );
    }

    /// Removal refuses a manifest key that would escape the managed directory,
    /// and never follows a symlink at the boundary.
    #[test]
    fn removal_rejects_escaping_paths_and_does_not_follow_symlinks() {
        let root = tempdir().expect("create tempdir");
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

#[test]
fn first_manifest_preserves_user_assets_and_warns_about_ambiguous_legacy_yaml() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    let activities_dir = global_root.join("resources/activities");
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&activities_dir).expect("create legacy activity dir");
    std::fs::create_dir_all(&jobs_dir).expect("create legacy job dir");

    let user_activity_name = "operator_activity";
    let user_job_name = "operator_job";
    let user_activity = activity_yaml(user_activity_name, None);
    let user_job = job_yaml(user_job_name, None);
    let colliding_activity = activity_yaml("sleep", None);
    let colliding_job = job_yaml("worktree_gc_pipeline", None);
    std::fs::write(
        activities_dir.join(format!("{user_activity_name}.yaml")),
        &user_activity,
    )
    .expect("write user activity");
    std::fs::write(jobs_dir.join(format!("{user_job_name}.yaml")), &user_job)
        .expect("write user job");
    std::fs::write(activities_dir.join("sleep.yaml"), &colliding_activity)
        .expect("write user activity colliding with a bundled name");
    std::fs::write(jobs_dir.join("worktree_gc_pipeline.yaml"), &colliding_job)
        .expect("write user job colliding with a bundled name");

    let migrated = init_global(&global_root);
    assert_eq!(migrated.managed_asset_warnings.len(), 2);
    for warning in &migrated.managed_asset_warnings {
        assert!(warning.contains("no managed provenance"));
        assert!(warning.contains("preserved in place"));
        assert!(warning.contains("move or delete"));
    }
    assert_eq!(
        std::fs::read_to_string(activities_dir.join(format!("{user_activity_name}.yaml")))
            .expect("read preserved user activity"),
        user_activity
    );
    assert_eq!(
        std::fs::read_to_string(jobs_dir.join(format!("{user_job_name}.yaml")))
            .expect("read preserved user job"),
        user_job
    );
    assert_eq!(
        std::fs::read_to_string(activities_dir.join("sleep.yaml"))
            .expect("read preserved colliding user activity"),
        colliding_activity
    );
    assert_eq!(
        std::fs::read_to_string(jobs_dir.join("worktree_gc_pipeline.yaml"))
            .expect("read preserved colliding user job"),
        colliding_job
    );

    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build migrated runtime");
    assert!(
        runtime
            .v2_activity_catalog()
            .expect("load activity catalog")
            .names()
            .any(|name| name == user_activity_name)
    );
    assert!(
        runtime
            .list_job_catalog_with_last_run(true, JobCatalogFilter::All)
            .expect("list jobs")
            .iter()
            .any(|(entry, _)| entry.job_id == user_job_name)
    );
}
