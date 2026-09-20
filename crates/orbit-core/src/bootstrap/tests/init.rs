//! What `orbit init` must guarantee: private directory trees, inert seeded
//! defaults that survive operator edits, and skill-link reconciliation that
//! reaps Orbit's own stale links without touching anyone else's.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::fs::io::create_dir_symlink;
use orbit_config::ConfigSeed;
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::routine::RoutineSeedIdentity;
use crate::application::skill::seed_default_skills;

use super::super::init::{
    InitOptions, ensure_orbit_root_initialized, ensure_skill_links, global_skills_dir, init_global,
    init_workspace_at_root, orbit_layout_paths,
};

/// Make global-init routing entirely fixture-owned even when the test
/// binary was launched by a managed Orbit run. This is one scoped guard so
/// its process-wide lock also restores the parent environment on drop.
fn global_init_env(home: &Path) -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .copied()
            .map(|name| (name, None))
            .chain(std::iter::once(("HOME", home.to_str()))),
    )
}

#[cfg(unix)]
#[test]
fn global_and_workspace_init_create_private_directory_trees_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_MARKER: &str = "ORBIT_TEST_PRIVATE_INIT_DIRECTORIES";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let status = std::process::Command::new("sh")
            .args(["-c", "umask 000; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("current test executable"))
            .arg("global_and_workspace_init_create_private_directory_trees_under_permissive_umask")
            .env(CHILD_MARKER, "1")
            .status()
            .expect("run test under permissive umask");
        assert!(status.success(), "permissive-umask child failed");
        return;
    }

    fn assert_private_directories<'a>(directories: impl IntoIterator<Item = &'a Path>) {
        for directory in directories {
            let mode = fs::metadata(directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode,
                0o700,
                "Orbit-owned directory {} has mode {mode:04o}",
                directory.display()
            );
        }
    }

    let temp = tempdir().expect("tempdir");
    let global_root = temp.path().join("global/.orbit");
    init_workspace_at_root(
        &global_root,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        },
    )
    .expect("initialize global root");

    let workspace_root = temp.path().join("workspace/.orbit");
    init_workspace_at_root(
        &workspace_root,
        InitOptions {
            global_root_override: Some(global_root.clone()),
            ..Default::default()
        },
    )
    .expect("initialize workspace root");

    let global = orbit_layout_paths(&global_root);
    assert_private_directories(
        [
            &global_root,
            &global.resources_dir,
            &global.activities_dir,
            &global.jobs_dir,
            &global.executors_dir,
            &global.policies_dir,
            &global_skills_dir(&global_root),
        ]
        .into_iter()
        .map(PathBuf::as_path),
    );
    let workspace = orbit_layout_paths(&workspace_root);
    assert_private_directories(
        [
            &workspace_root,
            &workspace.resources_dir,
            &workspace.state_dir,
            &workspace.audit_dir,
            &workspace.job_runs_dir,
            &workspace.logs_dir,
            &workspace.scoreboard_dir,
            &workspace.worktrees_dir,
        ]
        .into_iter()
        .map(PathBuf::as_path),
    );
    assert!(
        !workspace.state_dir.join("diagnostics").exists(),
        "workspace init must not scaffold unused state/diagnostics"
    );
    assert!(
        !workspace_root.join("knowledge").exists(),
        "workspace init must not scaffold unused knowledge/"
    );
}

#[test]
fn fresh_workspace_init_seeds_disabled_worktree_gc_routine() {
    let temp = tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_root = temp.path().join("repo/.orbit");
    let result = init_workspace_at_root(
        &orbit_root,
        InitOptions {
            global_root_override: Some(global_root.clone()),
            refresh_defaults: true,
            routine_seed_identity: Some(
                RoutineSeedIdentity::new("repo").expect("routine seed identity"),
            ),
            ..Default::default()
        },
    )
    .expect("initialize fresh workspace");

    assert_eq!(
        result.refreshed_default_routines,
        crate::application::routine::DEFAULT_ROUTINE_FILES.len()
    );
    let yaml = fs::read_to_string(orbit_root.join("routines/worktree_gc.yaml"))
        .expect("read seeded worktree GC routine");
    assert!(!yaml.contains("__ORBIT_"));
    let routine = orbit_common::protocol::yaml::parse_routine_yaml(&yaml)
        .expect("seeded worktree GC routine parses");
    assert!(!routine.enabled);
    assert_eq!(
        routine.target,
        orbit_types::workflow::RoutineTarget::Job("worktree_gc_pipeline".to_string())
    );
    assert_eq!(
        routine.policy.overlap,
        orbit_types::workflow::OverlapPolicy::Forbid
    );

    let routine_path = orbit_root.join("routines/worktree_gc.yaml");
    fs::write(&routine_path, "operator edited").expect("hand edit routine");
    init_workspace_at_root(
        &orbit_root,
        InitOptions {
            global_root_override: Some(global_root.clone()),
            refresh_defaults: true,
            routine_seed_identity: Some(
                RoutineSeedIdentity::new("repo").expect("routine seed identity"),
            ),
            ..Default::default()
        },
    )
    .expect("plain re-init");
    assert_eq!(
        fs::read_to_string(&routine_path).expect("read preserved routine"),
        "operator edited",
        "plain re-init preserves a hand-edited routine"
    );

    init_workspace_at_root(
        &orbit_root,
        InitOptions {
            global_root_override: Some(global_root),
            force: true,
            refresh_defaults: true,
            routine_seed_identity: Some(
                RoutineSeedIdentity::new("repo").expect("routine seed identity"),
            ),
            ..Default::default()
        },
    )
    .expect("forced re-init");
    let forced =
        fs::read_to_string(&routine_path).expect("read force-overwritten worktree GC routine");
    assert!(!forced.contains("operator edited"));
    let forced = orbit_common::protocol::yaml::parse_routine_yaml(&forced)
        .expect("force-overwritten routine parses");
    assert!(!forced.enabled);
}

#[test]
fn workspace_init_seeds_inert_defaults_without_clobbering_edits() {
    let temp = tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_root = temp.path().join("repo/.orbit");
    init_workspace_at_root(
        &global_root,
        InitOptions {
            global_only: true,
            ..Default::default()
        },
    )
    .expect("initialize scratch global root");
    let options = InitOptions {
        global_root_override: Some(global_root.clone()),
        ..Default::default()
    };

    let initial =
        init_workspace_at_root(&orbit_root, options.clone()).expect("initialize fresh workspace");
    assert_eq!(
        initial.seeded_default_auto_tasks,
        crate::application::auto_tasks::DEFAULT_AUTO_TASK_FILES.len()
    );
    let friction_path = orbit_root.join("auto_tasks/friction-curation.yaml");
    let qa_path = orbit_root.join("auto_tasks/qa-sweep.yaml");
    let security_path = orbit_root.join("auto_tasks/security-review.yaml");
    let code_review_path = orbit_root.join("auto_tasks/code-review.yaml");
    let friction = fs::read_to_string(&friction_path).expect("read seeded friction definition");
    let friction_definition = orbit_common::protocol::yaml::parse_auto_task_yaml(&friction)
        .expect("seeded friction definition parses through loader schema");
    assert!(!friction_definition.enabled);
    // [ORB-10877] Shipped recurring work names the portable system lane,
    // not a family-specific crew.
    assert_eq!(friction_definition.template.crew.as_deref(), Some("system"));
    assert!(
        friction.contains("\n  crew: system"),
        "[ORB-10877] seeded friction default must name the system crew"
    );
    assert!(matches!(
        friction_definition.dedupe,
        orbit_types::workflow::DedupePolicy::SkipIfOpen
    ));
    let qa_definition = orbit_common::protocol::yaml::parse_auto_task_yaml(
        &fs::read_to_string(&qa_path).expect("read seeded QA definition"),
    )
    .expect("seeded QA definition parses through loader schema");
    assert!(!qa_definition.enabled);
    assert!(matches!(
        qa_definition.dedupe,
        orbit_types::workflow::DedupePolicy::SkipIfOpen
    ));
    let security_definition = orbit_common::protocol::yaml::parse_auto_task_yaml(
        &fs::read_to_string(&security_path).expect("read seeded security-review definition"),
    )
    .expect("seeded security-review definition parses through loader schema");
    assert!(!security_definition.enabled);
    assert!(matches!(
        security_definition.dedupe,
        orbit_types::workflow::DedupePolicy::SkipIfOpen
    ));
    let code_review_definition = orbit_common::protocol::yaml::parse_auto_task_yaml(
        &fs::read_to_string(&code_review_path).expect("read seeded code-review definition"),
    )
    .expect("seeded code-review definition parses through loader schema");
    assert!(!code_review_definition.enabled);
    assert!(matches!(
        code_review_definition.dedupe,
        orbit_types::workflow::DedupePolicy::SkipIfOpen
    ));
    assert!(!orbit_root.join("state/auto-tasks.json").exists());
    let loaded = crate::application::auto_tasks::collect_auto_tasks(&orbit_root);
    assert!(
        loaded.errors.is_empty(),
        "seeded definition must load cleanly"
    );
    assert_eq!(
        loaded.definitions.len(),
        crate::application::auto_tasks::DEFAULT_AUTO_TASK_FILES.len()
    );
    assert!(
        loaded
            .definitions
            .iter()
            .any(|loaded| loaded.definition.name == "friction-curation")
    );
    assert!(
        loaded
            .definitions
            .iter()
            .any(|loaded| loaded.definition.name == "qa-sweep")
    );
    assert!(
        loaded
            .definitions
            .iter()
            .any(|loaded| loaded.definition.name == "security-review")
    );
    assert!(
        loaded
            .definitions
            .iter()
            .any(|loaded| loaded.definition.name == "code-review")
    );

    let runtime = OrbitRuntime::from_roots(&global_root, &orbit_root)
        .expect("open freshly initialized workspace");
    for loaded in &loaded.definitions {
        let expected = loaded
            .definition
            .template
            .complexity
            .expect("shipped definition carries assessed complexity");
        assert!(expected.is_assessed(), "{}", loaded.definition.name);
        let minted = runtime
            .auto_task_mint(&loaded.definition.name)
            .unwrap_or_else(|error| panic!("mint {}: {error}", loaded.definition.name));
        assert_eq!(
            minted.complexity,
            Some(expected),
            "scratch-workspace mint must inherit {} complexity",
            loaded.definition.name
        );
    }

    let authored_friction = "operator-authored friction definition\n";
    let authored_qa = "operator-authored QA definition\n";
    let authored_security = "operator-authored security-review definition\n";
    let authored_code_review = "operator-authored code-review definition\n";
    fs::write(&friction_path, authored_friction).expect("write friction edit");
    fs::write(&qa_path, authored_qa).expect("write QA edit");
    fs::write(&security_path, authored_security).expect("write security-review edit");
    fs::write(&code_review_path, authored_code_review).expect("write code-review edit");
    let repeated = init_workspace_at_root(&orbit_root, options).expect("reinitialize workspace");
    assert_eq!(repeated.seeded_default_auto_tasks, 0);
    assert_eq!(
        fs::read_to_string(friction_path).expect("read preserved friction definition"),
        authored_friction
    );
    assert_eq!(
        fs::read_to_string(qa_path).expect("read preserved QA definition"),
        authored_qa
    );
    assert_eq!(
        fs::read_to_string(security_path).expect("read preserved security-review definition"),
        authored_security
    );
    assert_eq!(
        fs::read_to_string(code_review_path).expect("read preserved code-review definition"),
        authored_code_review
    );
}

#[test]
fn global_init_seeds_skills_and_home_level_links() {
    let home = tempdir().expect("home tempdir");
    let _env = global_init_env(home.path());

    let result = init_global(
        None,
        InitOptions {
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        },
    );

    let result = result.expect("init global");
    // Skills are now reconciled per managed file rather than per skill
    // directory, so a refresh counts every SKILL.md *and* reference file.
    assert_eq!(
        result.refreshed_skill_files,
        crate::application::skill::DEFAULT_SKILL_FILES.len()
    );
    assert!(result.created_skills_symlink);
    assert!(
        home.path()
            .join(".orbit")
            .join("skills")
            .join("orbit")
            .join("SKILL.md")
            .exists()
    );
    // Reference files seed as a nested tree, not just the router document.
    assert!(
        home.path()
            .join(".orbit")
            .join("skills")
            .join("orbit")
            .join("references")
            .join("task-execution.md")
            .exists()
    );
    assert!(
        !home
            .path()
            .join(".orbit")
            .join("resources")
            .join("skills")
            .join("orbit")
            .join("SKILL.md")
            .exists()
    );
    assert_skill_link_exists(home.path().join(".agents").join("skills").join("orbit"));
    assert_skill_link_exists(home.path().join(".claude").join("skills").join("orbit"));
}

#[test]
fn workspace_init_leaves_repo_skills_unseeded() {
    let home = tempdir().expect("home tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    // The shared guard serializes every HOME mutation in this test binary,
    // including the runtime resolve tests; a module-local lock did not.
    let _env = orbit_common::test_env::scoped([("HOME", home.path().to_str())]);

    let orbit_root = workspace.path().join(".orbit");
    seed_default_skills(
        &orbit_root.join("resources").join("skills"),
        &orbit_root,
        true,
    )
    .expect("seed legacy workspace resource skills");
    seed_default_skills(&orbit_root.join("skills"), &orbit_root, true)
        .expect("seed legacy workspace skills");
    let custom_skill = orbit_root.join("resources").join("skills").join("custom");
    fs::create_dir_all(&custom_skill).expect("create custom skill");
    fs::write(
        custom_skill.join("SKILL.md"),
        "# Custom\n\n## Purpose\n\nKeep me.\n",
    )
    .expect("write custom skill");
    let legacy_skill = orbit_root.join("resources").join("skills").join("orbit-pr");
    fs::create_dir_all(&legacy_skill).expect("create legacy skill");
    fs::write(
        legacy_skill.join("SKILL.md"),
        "---\nname: orbit-pr\n---\n\n# Orbit PR\n",
    )
    .expect("write legacy skill");

    let result = init_workspace_at_root(
        &orbit_root,
        InitOptions {
            refresh_defaults: true,
            global_root_override: Some(home.path().join(".orbit")),
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        },
    );

    let result = result.expect("init workspace");
    // Skills are now reconciled per managed file rather than per skill
    // directory, so a refresh counts every SKILL.md *and* reference file.
    assert_eq!(
        result.refreshed_skill_files,
        crate::application::skill::DEFAULT_SKILL_FILES.len()
    );
    assert!(result.created_skills_symlink);
    assert!(
        !orbit_root
            .join("resources")
            .join("skills")
            .join("orbit")
            .join("SKILL.md")
            .exists()
    );
    assert!(!orbit_root.join("skills").exists());
    assert!(orbit_root.join("state").join("logs").exists());
    assert!(custom_skill.join("SKILL.md").exists());
    assert!(!legacy_skill.exists());
    assert!(
        home.path()
            .join(".orbit")
            .join("skills")
            .join("orbit")
            .join("SKILL.md")
            .exists()
    );
    assert_skill_link_exists(home.path().join(".claude").join("skills").join("orbit"));
}

/// A `--root` scratch root makes one directory serve as both the global and
/// the workspace root. Every runtime open seeds the global skill catalog and
/// then reaps workspace-seeded leftovers; when the two roots are the same
/// path, the reap used to delete the catalog it had just written.
/// [ORB-10926]
#[test]
fn runtime_open_keeps_global_skills_when_root_doubles_as_workspace() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("scratch-root");
    let router = global_skills_dir(&root).join("orbit").join("SKILL.md");

    ensure_orbit_root_initialized(&root, &root).expect("first runtime open");
    assert!(
        router.exists(),
        "init must seed the global skill catalog at {}",
        router.display()
    );

    // Legacy workspace-seeded skills still live in the resources tree of the
    // same root, and must still be reaped.
    let legacy_skills = root.join("resources").join("skills");
    seed_default_skills(&legacy_skills, &root, true).expect("seed legacy workspace skills");
    assert!(legacy_skills.join("orbit").join("SKILL.md").exists());

    ensure_orbit_root_initialized(&root, &root).expect("second runtime open");

    assert!(
        router.exists(),
        "a runtime open must not delete the global skill catalog it seeded"
    );
    assert!(
        global_skills_dir(&root)
            .join("orbit")
            .join("references")
            .join("task-execution.md")
            .exists(),
        "reference files under the global catalog must survive too"
    );
    assert!(
        !legacy_skills.exists(),
        "legacy workspace-seeded skills must still be reaped"
    );
}

#[test]
fn global_init_writes_crew_settings_as_custom_crew_to_config_toml() {
    let home = tempdir().expect("home tempdir");
    let _env = global_init_env(home.path());

    let settings = BTreeMap::from([(
        "custom".to_string(),
        orbit_config::CrewSeed {
            provider: Some("codex".into()),
            model: Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.into()),
        },
    )]);

    let result = init_global(
        None,
        InitOptions {
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default().with_crews(settings)),
            ..Default::default()
        },
    );

    let result = result.expect("init global with crew settings");
    assert!(result.created_config);

    let config_path = home.path().join(".orbit").join("config.toml");
    let contents = fs::read_to_string(&config_path).expect("read config");
    assert!(!contents.contains("[agent.reviewer]"));
    assert!(contents.contains("default_crew = \"custom\""));
    assert!(contents.contains("provider = \"codex\""));
    assert!(contents.contains(&format!(
        "model = \"{}\"",
        orbit_common::test_fixtures::TEST_CODEX_MODEL
    )));

    // Round-trips through toml: custom crew is one flat assignment.
    let parsed: toml::Value = toml::from_str(&contents).expect("parse");
    let custom = parsed
        .get("crews")
        .and_then(|v| v.as_table())
        .and_then(|v| v.get("custom"))
        .and_then(|v| v.as_table())
        .expect("custom crew table");
    assert_eq!(custom.len(), 2);
    assert_eq!(
        custom.get("provider").and_then(|v| v.as_str()),
        Some("codex")
    );
    assert_eq!(
        custom.get("model").and_then(|v| v.as_str()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL)
    );
}

#[test]
fn global_init_with_existing_config_does_not_overwrite_crew_settings() {
    let home = tempdir().expect("home tempdir");
    let _env = global_init_env(home.path());

    // Pre-seed config.toml with user content.
    let orbit_root = home.path().join(".orbit");
    fs::create_dir_all(&orbit_root).expect("mkdir .orbit");
    let config_path = orbit_root.join("config.toml");
    let user_content = "# pre-existing user config\n";
    fs::write(&config_path, user_content).expect("preseed");

    let settings = BTreeMap::from([(
        "custom".to_string(),
        orbit_config::CrewSeed {
            provider: Some("claude".into()),
            model: None,
        },
    )]);

    let result = init_global(
        None,
        InitOptions {
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default().with_crews(settings)),
            ..Default::default()
        },
    );

    let result = result.expect("init global");
    assert!(!result.created_config);
    let final_contents = fs::read_to_string(&config_path).expect("read config");
    assert_eq!(final_contents, user_content);
}

#[test]
fn global_init_without_crew_settings_writes_clean_template() {
    let home = tempdir().expect("home tempdir");
    let _env = global_init_env(home.path());

    let result = init_global(
        None,
        InitOptions {
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        },
    );

    let result = result.expect("init global");
    assert!(result.created_config);
    let config_path = home.path().join(".orbit").join("config.toml");
    let contents = fs::read_to_string(&config_path).expect("read config");
    for line in contents.lines() {
        assert!(
            !line.trim_start().starts_with("[agent."),
            "unexpected uncommented agent section: {line}",
        );
    }
    assert!(!contents.contains("[crews."));
    assert!(!contents.contains("default_crew"));
}

fn retired_skill_ids() -> [&'static str; 5] {
    [
        "orbit-knowledge",
        "orbit-search",
        "orbit-task",
        "orbit-task-pilot",
        "orbit-workflow",
    ]
}

fn write_skill_dir(dir: &Path) {
    fs::create_dir_all(dir).expect("create skill dir");
    fs::write(dir.join("SKILL.md"), "# skill\n").expect("write SKILL.md");
}

fn seed_orbit_owned_link(skills_root: &Path, links_dir: &Path, skill_id: &str) -> PathBuf {
    let target = skills_root.join(skill_id);
    let link = links_dir.join(skill_id);
    fs::create_dir_all(links_dir).expect("create links dir");
    create_dir_symlink(&target, &link).expect("create orbit-owned skill link");
    link
}

#[test]
fn ensure_skill_links_reaps_dangling_retired_orbit_owned_links() {
    let temp = tempdir().expect("tempdir");
    let skills_root = temp.path().join("skills");
    let links_dir = temp.path().join("client").join("skills");
    write_skill_dir(&skills_root.join("orbit"));

    let mut retired_links = Vec::new();
    for id in retired_skill_ids() {
        retired_links.push(seed_orbit_owned_link(&skills_root, &links_dir, id));
    }
    seed_orbit_owned_link(&skills_root, &links_dir, "orbit");

    let changed = ensure_skill_links(&skills_root, &["orbit"], &links_dir, false)
        .expect("reconcile skill links");
    assert!(changed, "reaping retired dangling links is a change");

    assert_skill_link_exists(links_dir.join("orbit"));
    for link in retired_links {
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "retired dangling Orbit link must be reaped: {}",
            link.display()
        );
    }
}

#[test]
fn ensure_skill_links_leaves_live_custom_and_non_orbit_entries() {
    let temp = tempdir().expect("tempdir");
    let skills_root = temp.path().join("skills");
    let links_dir = temp.path().join("client").join("skills");
    write_skill_dir(&skills_root.join("orbit"));

    let custom_target = temp.path().join("custom-skill");
    write_skill_dir(&custom_target);
    let custom_link = links_dir.join("my-custom");
    fs::create_dir_all(&links_dir).expect("create links dir");
    create_dir_symlink(&custom_target, &custom_link).expect("create custom skill link");

    let live_retired_target = skills_root.join("orbit-task");
    write_skill_dir(&live_retired_target);
    let live_retired_link = seed_orbit_owned_link(&skills_root, &links_dir, "orbit-task");

    let foreign_dangling = links_dir.join("foreign-broken");
    create_dir_symlink(&temp.path().join("does-not-exist"), &foreign_dangling)
        .expect("create foreign dangling link");

    let real_dir = links_dir.join("operator-dir");
    write_skill_dir(&real_dir);

    ensure_skill_links(&skills_root, &["orbit"], &links_dir, false).expect("reconcile skill links");

    assert_skill_link_exists(links_dir.join("orbit"));
    assert_eq!(
        fs::read_link(&custom_link).expect("read custom link"),
        custom_target
    );
    assert!(
        custom_link.join("SKILL.md").exists(),
        "live custom skill symlink must be left untouched"
    );
    assert_eq!(
        fs::read_link(&live_retired_link).expect("read live retired link"),
        live_retired_target,
        "Orbit-owned link whose target still exists must be left"
    );
    assert!(
        fs::symlink_metadata(&foreign_dangling)
            .expect("foreign dangling metadata")
            .file_type()
            .is_symlink(),
        "dangling symlink that does not point at the Orbit skills root stays"
    );
    assert!(
        real_dir.join("SKILL.md").exists(),
        "operator-owned skill directory must be left untouched"
    );
}

fn assert_skill_link_exists(path: PathBuf) {
    let metadata = fs::symlink_metadata(&path).expect("link metadata");
    assert!(
        metadata.file_type().is_symlink(),
        "expected {} to be a symlink",
        path.display()
    );
    assert!(path.join("SKILL.md").exists());
}
