use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::super::activity_catalog::remove_spec_backend_key;
use super::super::artifact::{
    ArtifactCondition, ArtifactHealth, ArtifactKind, ArtifactProvenance,
    FIX_RETIRED_ACTIVITY_BACKENDS_CMD,
};
use crate::OrbitRuntime;
use crate::application::managed_assets::MANAGED_ASSET_MANIFEST_FILE;
use crate::runtime::OrbitRuntimeRoots;
use orbit_common::security::release::sha256_hex;

fn workspace_runtime(root: &Path) -> (OrbitRuntime, PathBuf, PathBuf) {
    let global_root = root.join("global");
    let workspace_root = root.join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    let activities = workspace_root.join("resources/activities");
    std::fs::create_dir_all(&activities).expect("create workspace activities");
    (runtime, workspace_root, activities)
}

fn agent_loop_yaml(name: &str, extra_spec: &str) -> String {
    format!(
        "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: {name}\nspec:\n  type: agent_loop\n  description: workspace fixture\n  instruction: do the work\n  # keep this comment\n{extra_spec}"
    )
}

fn health_of(report: &[ArtifactHealth], kind: ArtifactKind) -> &ArtifactHealth {
    report
        .iter()
        .find(|health| health.kind == kind)
        .unwrap_or_else(|| panic!("missing artifact health for {kind:?}"))
}

fn seeded_runtime(root: &Path) -> (OrbitRuntime, PathBuf, PathBuf) {
    let global_root = root.join("global");
    let workspace_root = root.join("repo/.orbit");
    let runtime = OrbitRuntime::initialize_from_resolved_roots(
        OrbitRuntimeRoots {
            global_root: global_root.clone(),
            shared_root: workspace_root.clone(),
            local_root: workspace_root.clone(),
        },
        None,
    )
    .expect("initialize runtime with defaults");
    (runtime, global_root, workspace_root)
}

#[cfg(unix)]
#[test]
fn doctor_retirement_confines_linked_and_regular_skill_assets() {
    const CHILD_FLAG: &str = "ORBIT_ARTIFACT_RETIRE_FIXTURE_CHILD";
    if std::env::var_os(CHILD_FLAG).is_none() {
        run_isolated_child_fixture(
            "doctor_retirement_confines_linked_and_regular_skill_assets",
            CHILD_FLAG,
        );
        return;
    }

    use std::os::unix::fs::symlink;

    let root = tempdir().expect("fixture root");
    let (runtime, _, _) = workspace_runtime(root.path());
    let skills = root.path().join("global/skills");
    let external = root.path().join("external");
    std::fs::create_dir_all(&skills).expect("skills catalog");
    std::fs::create_dir_all(&external).expect("external fixture");
    let original = "previous managed skill\n";
    let modified = "operator edited skill\n";
    let mut assets = BTreeMap::new();
    let mut linked_targets = Vec::new();

    for (name, body) in [
        ("confined-clean", original),
        ("confined-modified", modified),
    ] {
        let path = skills.join(name).join("SKILL.md");
        std::fs::create_dir_all(path.parent().expect("skill directory"))
            .expect("create confined skill");
        std::fs::write(&path, body).expect("write confined skill");
        assets.insert(format!("{name}/SKILL.md"), sha256_hex(original.as_bytes()));
    }

    for (name, body, intermediate) in [
        ("linked-dir-clean", original, true),
        ("linked-dir-modified", modified, true),
        ("linked-file-clean", original, false),
        ("linked-file-modified", modified, false),
    ] {
        let external_path = if intermediate {
            let directory = external.join(name);
            std::fs::create_dir_all(&directory).expect("external skill directory");
            directory.join("SKILL.md")
        } else {
            external.join(format!("{name}.md"))
        };
        std::fs::write(&external_path, body).expect("write external skill");
        let link = if intermediate {
            let link = skills.join(name);
            symlink(external_path.parent().expect("external directory"), &link)
                .expect("link intermediate directory");
            link
        } else {
            let directory = skills.join(name);
            std::fs::create_dir_all(&directory).expect("confined skill directory");
            let link = directory.join("SKILL.md");
            symlink(&external_path, &link).expect("link final file");
            link
        };
        assets.insert(format!("{name}/SKILL.md"), sha256_hex(original.as_bytes()));
        linked_targets.push((name, link, external_path, body));
    }

    let manifest_path = skills.join(MANAGED_ASSET_MANIFEST_FILE);
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "assetKind": "skill",
            "assets": assets,
        }))
        .expect("encode manifest"),
    )
    .expect("write retired provenance");

    assert_eq!(
        runtime
            .remove_stale_definition_artifacts()
            .expect("doctor retirement pass"),
        2,
        "only confined files may leave the catalog"
    );
    assert!(!skills.join("confined-clean/SKILL.md").exists());
    assert!(!skills.join("confined-modified/SKILL.md").exists());
    assert_eq!(
        std::fs::read_to_string(
            root.path()
                .join("global/.retired-managed/skills/confined-modified/SKILL.md")
        )
        .expect("read preserved confined edit"),
        modified
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest after doctor"))
            .expect("parse manifest after doctor");
    let remaining = manifest["assets"]
        .as_object()
        .expect("remaining provenance");
    assert_eq!(remaining.len(), linked_targets.len());
    let report = runtime
        .inspect_definition_artifacts()
        .expect("doctor still reports skipped assets");
    let findings = &health_of(&report, ArtifactKind::Skill).findings;
    for (name, link, external_path, body) in &linked_targets {
        assert!(
            link.symlink_metadata()
                .expect("link survives")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(external_path).expect("external bytes survive"),
            *body
        );
        assert!(
            remaining.contains_key(&format!("{name}/SKILL.md")),
            "{name} provenance must remain"
        );
        let finding = findings
            .iter()
            .find(|finding| finding.name == format!("{name}/SKILL.md"))
            .expect("doctor must still report linked retired asset");
        assert_eq!(finding.condition, ArtifactCondition::Deprecated);
        assert!(
            finding.detail.contains("retirement is skipped"),
            "doctor must explain the linked-path refusal for {name}: {}",
            finding.detail
        );
        assert!(
            !root
                .path()
                .join(format!("global/.retired-managed/skills/{name}/SKILL.md"))
                .exists(),
            "doctor must not preserve an external file under {name}"
        );
    }
    assert_eq!(
        runtime
            .remove_stale_definition_artifacts()
            .expect("second doctor pass"),
        0
    );
}

#[test]
fn workspace_http_backend_is_a_catalog_fault_and_named_repair() {
    let root = tempdir().expect("tempdir");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    let path = activities.join("workspace_finisher.yaml");
    std::fs::write(
        &path,
        agent_loop_yaml("workspace_finisher", "  backend: http\n"),
    )
    .expect("write fixture");

    let catalog_err = runtime
        .v2_activity_catalog()
        .expect_err("production catalog must reject spec.backend: http");
    let catalog_text = catalog_err.to_string();
    assert!(
        catalog_text.contains("workspace_finisher.yaml"),
        "{catalog_text}"
    );
    assert!(catalog_text.contains("backend: http"), "{catalog_text}");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let finding = health_of(&report, ArtifactKind::Activity)
        .findings
        .iter()
        .find(|finding| finding.name == "workspace_finisher")
        .expect("workspace activity must be reported");
    assert_eq!(finding.condition, ArtifactCondition::Faulty);
    assert!(
        finding.detail.contains(path.to_string_lossy().as_ref()),
        "{}",
        finding.detail
    );
    assert!(
        finding.detail.contains("spec.backend: http"),
        "{}",
        finding.detail
    );
    assert!(
        finding.detail.contains("schemaVersion 2 parse failed"),
        "{}",
        finding.detail
    );
    assert!(
        finding.detail.contains("backend: http"),
        "{}",
        finding.detail
    );
    assert!(
        finding
            .remediation
            .contains(FIX_RETIRED_ACTIVITY_BACKENDS_CMD),
        "{}",
        finding.remediation
    );
}

#[test]
fn unknown_tool_cannot_pass_doctor_while_failing_catalog() {
    let root = tempdir().expect("tempdir");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    std::fs::write(
        activities.join("constellation_survey.yaml"),
        agent_loop_yaml(
            "constellation_survey",
            "  tools:\n    - orbit.not_a_real_tool\n",
        ),
    )
    .expect("write fixture");

    let catalog_err = runtime
        .v2_activity_catalog()
        .expect_err("removed tool must fail catalog construction");
    let catalog_text = catalog_err.to_string();
    assert!(
        catalog_text.contains("orbit.not_a_real_tool"),
        "{catalog_text}"
    );

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let finding = health_of(&report, ArtifactKind::Activity)
        .findings
        .iter()
        .find(|finding| finding.name == "constellation_survey")
        .expect("doctor must surface the same catalog fault");
    assert_eq!(finding.condition, ArtifactCondition::Faulty);
    assert!(
        finding.detail.contains("orbit.not_a_real_tool"),
        "{}",
        finding.detail
    );
    assert!(
        !finding
            .remediation
            .contains(FIX_RETIRED_ACTIVITY_BACKENDS_CMD),
        "unknown tools are not the backend repair: {}",
        finding.remediation
    );
}

#[test]
fn repair_removes_only_known_backends_across_files_and_is_idempotent() {
    let root = tempdir().expect("tempdir");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    let http_path = activities.join("workspace_finisher.yaml");
    let auto_path = activities.join("agent_review.yaml");
    let unknown_path = activities.join("custom_loop.yaml");
    let malformed_path = activities.join("broken.yaml");
    let comment_marker = "# keep this comment";
    let http_body = agent_loop_yaml("workspace_finisher", "  backend: http\n  model: grok\n");
    let auto_body = agent_loop_yaml("agent_review", "  backend: auto\n");
    let unknown_body = agent_loop_yaml("custom_loop", "  backend: weave\n");
    let malformed_body = "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: broken\nspec: [\n";
    std::fs::write(&http_path, &http_body).expect("write http fixture");
    std::fs::write(&auto_path, &auto_body).expect("write auto fixture");
    std::fs::write(&unknown_path, &unknown_body).expect("write unknown fixture");
    std::fs::write(&malformed_path, malformed_body).expect("write malformed fixture");

    let report = runtime
        .repair_retired_activity_backends()
        .expect("repair pass");
    assert_eq!(report.repaired.len(), 2, "{report:?}");
    assert!(report.repaired.contains(&http_path), "{report:?}");
    assert!(report.repaired.contains(&auto_path), "{report:?}");
    assert_eq!(report.skipped.len(), 2, "{report:?}");
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.path == unknown_path && skip.reason.contains("weave")),
        "{report:?}"
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.path == malformed_path && skip.reason.contains("malformed")),
        "{report:?}"
    );

    let http_after = std::fs::read_to_string(&http_path).expect("read repaired http");
    assert!(!http_after.contains("backend:"), "{http_after}");
    assert!(http_after.contains("model: grok"), "{http_after}");
    assert!(http_after.contains(comment_marker), "{http_after}");
    let auto_after = std::fs::read_to_string(&auto_path).expect("read repaired auto");
    assert!(!auto_after.contains("backend:"), "{auto_after}");
    assert_eq!(
        std::fs::read_to_string(&unknown_path).expect("unknown file survives"),
        unknown_body
    );
    assert_eq!(
        std::fs::read_to_string(&malformed_path).expect("malformed file survives"),
        malformed_body
    );

    let after_repair = runtime
        .inspect_definition_artifacts()
        .expect("inspect after repair");
    let leftovers = health_of(&after_repair, ArtifactKind::Activity)
        .findings
        .iter()
        .map(|finding| finding.name.as_str())
        .collect::<Vec<_>>();
    assert!(
        leftovers.contains(&"custom_loop") && leftovers.contains(&"broken"),
        "{leftovers:?}"
    );
    assert!(
        !leftovers.contains(&"workspace_finisher") && !leftovers.contains(&"agent_review"),
        "{leftovers:?}"
    );

    let second = runtime
        .repair_retired_activity_backends()
        .expect("second repair pass");
    assert!(second.repaired.is_empty(), "{second:?}");
    assert_eq!(second.skipped.len(), 2, "{second:?}");

    std::fs::remove_file(&unknown_path).expect("remove unknown backend fixture");
    std::fs::remove_file(&malformed_path).expect("remove malformed fixture");
    runtime
        .v2_activity_catalog()
        .expect("catalog loads after known backends are removed");
    assert!(
        health_of(
            &runtime
                .inspect_definition_artifacts()
                .expect("inspect healthy workspace"),
            ArtifactKind::Activity,
        )
        .findings
        .is_empty()
    );
}

#[test]
fn remove_spec_backend_key_preserves_unrelated_bytes() {
    let raw = "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: demo\nspec:\n  type: agent_loop\n  backend: http  # retired\n  instruction: stay\n";
    let next = remove_spec_backend_key(raw, "http").expect("remove backend");
    assert_eq!(
        next,
        "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: demo\nspec:\n  type: agent_loop\n  instruction: stay\n"
    );
}

#[test]
fn remove_spec_backend_key_refuses_flow_style() {
    let raw = "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: demo\nspec: {type: agent_loop, backend: http, instruction: stay}\n";
    let error = remove_spec_backend_key(raw, "http").expect_err("flow style is not rewritten");
    assert!(
        error.contains("flow-style") || error.contains("block-style"),
        "{error}"
    );
}

#[test]
fn missing_shipped_activity_is_reported_and_not_restored_by_the_fix_flag() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, _workspace) = seeded_runtime(root.path());
    let path = global_root.join("resources/activities/git_merge.yaml");
    assert!(path.is_file(), "seeded catalog must include git_merge");
    std::fs::remove_file(&path).expect("delete shipped default");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let finding = health_of(&report, ArtifactKind::Activity)
        .findings
        .iter()
        .find(|finding| finding.name == "git_merge")
        .expect("missing shipped activity must be reported");
    assert_eq!(finding.condition, ArtifactCondition::Missing);
    assert_eq!(finding.provenance, ArtifactProvenance::OrbitWritten);
    assert_eq!(finding.path, path);
    assert!(finding.is_unloadable_shipped_default());
    assert!(
        finding.remediation.contains("orbit init"),
        "{}",
        finding.remediation
    );

    assert_eq!(
        runtime
            .remove_stale_definition_artifacts()
            .expect("fix flag retires deprecated artifacts only"),
        0
    );
    assert!(
        !path.exists(),
        "missing defaults are restored by init/sync, not --fix-stale-artifacts"
    );
}

#[test]
fn locally_modified_shipped_activity_is_not_reported_missing() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, _workspace) = seeded_runtime(root.path());
    let path = global_root.join("resources/activities/git_merge.yaml");
    let current = std::fs::read_to_string(&path).expect("read shipped activity");
    std::fs::write(&path, format!("{current}# operator edit\n")).expect("keep a custom override");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    assert!(
        health_of(&report, ArtifactKind::Activity)
            .findings
            .iter()
            .all(|finding| finding.name != "git_merge"),
        "an on-disk custom override must not look missing: {:?}",
        health_of(&report, ArtifactKind::Activity).findings
    );
}

#[test]
fn missing_shipped_job_is_reported() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, _workspace) = seeded_runtime(root.path());
    let path = global_root.join("resources/jobs/task_gate_pipeline.yaml");
    std::fs::remove_file(&path).expect("delete shipped job");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let finding = health_of(&report, ArtifactKind::Job)
        .findings
        .iter()
        .find(|finding| finding.name == "task_gate_pipeline")
        .expect("missing shipped job must be reported");
    assert_eq!(finding.condition, ArtifactCondition::Missing);
    assert!(finding.is_unloadable_shipped_default());
}

#[test]
fn catalog_without_a_managed_manifest_does_not_report_missing_shipped_defaults() {
    let root = tempdir().expect("tempdir");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    std::fs::write(
        activities.join("custom_loop.yaml"),
        agent_loop_yaml("custom_loop", ""),
    )
    .expect("write workspace-only activity");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let missing = health_of(&report, ArtifactKind::Activity)
        .findings
        .iter()
        .filter(|finding| finding.condition == ArtifactCondition::Missing)
        .count();
    assert_eq!(
        missing,
        0,
        "custom catalogs without a managed manifest are not missing shipped defaults: {:?}",
        health_of(&report, ArtifactKind::Activity).findings
    );
}

/// Re-run `test_name` in a child test process with an isolated `HOME`, failing
/// if it does not finish within a bounded wait instead of hanging CI, or if the
/// filter matched no test (libtest names omit the crate prefix).
#[cfg(unix)]
fn run_isolated_child_fixture(test_name: &str, child_flag: &str) {
    use std::time::{Duration, Instant};

    let home = tempdir().expect("isolated home");
    let stdout_path = home.path().join("child.stdout");
    let stderr_path = home.path().join("child.stderr");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let mut child = command
        .args(["--exact", &format!("{module}::{test_name}"), "--nocapture"])
        .env(child_flag, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .stdout(std::fs::File::create(&stdout_path).expect("child stdout"))
        .stderr(std::fs::File::create(&stderr_path).expect("child stderr"))
        .spawn()
        .expect("spawn isolated fixture");
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll isolated fixture") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let output = format!(
        "{}\n{}",
        std::fs::read_to_string(&stdout_path).unwrap_or_default(),
        std::fs::read_to_string(&stderr_path).unwrap_or_default()
    );
    let status = status.unwrap_or_else(|| panic!("{test_name} did not finish in time\n{output}"));
    assert!(status.success(), "{output}");
    assert!(
        output.contains("test result: ok. 1 passed;"),
        "the isolated fixture must run exactly one test: {output}"
    );
}

#[cfg(unix)]
#[test]
fn retired_backend_repair_confines_links_cycles_and_special_files() {
    const CHILD_FLAG: &str = "ORBIT_ACTIVITY_REPAIR_FIXTURE_CHILD";
    if std::env::var_os(CHILD_FLAG).is_none() {
        run_isolated_child_fixture(
            "retired_backend_repair_confines_links_cycles_and_special_files",
            CHILD_FLAG,
        );
        return;
    }

    use std::os::unix::fs::{FileTypeExt, symlink};

    let root = tempdir().expect("fixture root");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    let external = root.path().join("external");
    std::fs::create_dir_all(external.join("dir")).expect("external fixture");

    // Links below the catalog root to external retired-backend activities.
    let external_final = external.join("final.yaml");
    let external_nested = external.join("dir/nested.yaml");
    let external_hard = external.join("hard.yaml");
    let external_final_body = agent_loop_yaml("external_final", "  backend: http\n");
    let external_nested_body = agent_loop_yaml("external_nested", "  backend: http\n");
    let external_hard_body = agent_loop_yaml("hard_linked", "  backend: http\n");
    std::fs::write(&external_final, &external_final_body).expect("write external final");
    std::fs::write(&external_nested, &external_nested_body).expect("write external nested");
    std::fs::write(&external_hard, &external_hard_body).expect("write external hard");
    let final_link = activities.join("final_link.yaml");
    symlink(&external_final, &final_link).expect("link final file");
    let dir_link = activities.join("linked");
    symlink(external.join("dir"), &dir_link).expect("link intermediate directory");
    let hard_link = activities.join("hard_linked.yaml");
    std::fs::hard_link(&external_hard, &hard_link).expect("hard link external file");

    // A linked directory cycle and a FIFO that would block a plain read.
    std::fs::create_dir_all(activities.join("nested")).expect("nested catalog dir");
    let cycle = activities.join("nested/cycle");
    symlink(&activities, &cycle).expect("link directory cycle");
    let fifo = activities.join("pipe.yaml");
    let mkfifo = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(mkfifo.success(), "mkfifo fixture");

    // Ordinary in-catalog retired backends, including one in a real subdirectory.
    let regular = activities.join("regular_loop.yaml");
    let deep = activities.join("nested/deep_loop.yaml");
    std::fs::write(
        &regular,
        agent_loop_yaml("regular_loop", "  backend: http\n"),
    )
    .expect("write regular");
    std::fs::write(&deep, agent_loop_yaml("deep_loop", "  backend: auto\n")).expect("write deep");

    // A configured root that is itself a link stays a root and is repaired.
    let linked_catalog = root.path().join("linked_catalog");
    std::fs::create_dir_all(&linked_catalog).expect("linked catalog");
    let global_resources = root.path().join("global/resources");
    std::fs::create_dir_all(&global_resources).expect("global resources");
    symlink(&linked_catalog, global_resources.join("activities")).expect("link global root");
    std::fs::write(
        linked_catalog.join("global_loop.yaml"),
        agent_loop_yaml("global_loop", "  backend: http\n"),
    )
    .expect("write global root activity");
    let global_loop = global_resources.join("activities/global_loop.yaml");

    let refused = [&final_link, &dir_link, &cycle, &fifo];
    let report = runtime
        .inspect_definition_artifacts()
        .expect("doctor scan finishes");
    let findings = &health_of(&report, ArtifactKind::Activity).findings;
    for path in refused {
        let finding = findings
            .iter()
            .find(|finding| &finding.path == path)
            .unwrap_or_else(|| panic!("doctor must report {}: {findings:?}", path.display()));
        assert_eq!(finding.condition, ArtifactCondition::Faulty);
        assert!(
            !finding
                .remediation
                .contains(FIX_RETIRED_ACTIVITY_BACKENDS_CMD),
            "a refused entry is not offered the automatic repair: {finding:?}"
        );
    }
    assert!(
        findings
            .iter()
            .all(|finding| !finding.path.starts_with(&external)),
        "doctor must not inspect through links: {findings:?}"
    );

    let expected_repaired = {
        let mut paths = vec![
            deep.clone(),
            global_loop.clone(),
            hard_link.clone(),
            regular.clone(),
        ];
        paths.sort();
        paths
    };
    let mut expected_skipped: Vec<PathBuf> = refused.iter().map(|path| (*path).clone()).collect();
    expected_skipped.sort();
    for pass in ["first", "second"] {
        let repair = runtime
            .repair_retired_activity_backends()
            .expect("repair pass");
        let mut repaired = repair.repaired.clone();
        repaired.sort();
        let mut skipped: Vec<PathBuf> = repair
            .skipped
            .iter()
            .map(|skip| skip.path.clone())
            .collect();
        skipped.sort();
        if pass == "first" {
            assert_eq!(repaired, expected_repaired, "{pass} pass: {repair:?}");
        } else {
            assert!(repaired.is_empty(), "repair is idempotent: {repair:?}");
        }
        assert_eq!(
            skipped, expected_skipped,
            "{pass} pass reports refusals: {repair:?}"
        );

        assert_eq!(
            std::fs::read_to_string(&external_final).expect("external final"),
            external_final_body
        );
        assert_eq!(
            std::fs::read_to_string(&external_nested).expect("external nested"),
            external_nested_body
        );
        assert_eq!(
            std::fs::read_to_string(&external_hard).expect("external hard link target"),
            external_hard_body
        );
        for link in [&final_link, &dir_link, &cycle] {
            assert!(
                link.symlink_metadata()
                    .expect("link survives")
                    .file_type()
                    .is_symlink()
            );
        }
        assert!(
            fifo.symlink_metadata()
                .expect("fifo survives")
                .file_type()
                .is_fifo()
        );
    }
    for path in &expected_repaired {
        let after = std::fs::read_to_string(path).expect("read repaired activity");
        assert!(!after.contains("backend:"), "{}: {after}", path.display());
    }
    assert!(
        global_resources
            .join("activities")
            .symlink_metadata()
            .expect("configured root link survives")
            .file_type()
            .is_symlink()
    );
}
