//! Auto-task delete and restore: a deleted definition takes its cursor with
//! it, a deleted shipped default stays out of every later reseed and doctor
//! pass, and restore brings back the shipped content.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;

use chrono::{Duration, Utc};
use orbit_automation::auto_tasks::scheduler::{SchedulerOptions, run_auto_task_scheduler_at};
use orbit_store::compose::auto_task::load_cursor_state;
use orbit_types::workflow::DedupePolicy;
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::auto_tasks::crud::{
    set_manual_mint_after_admission_barriers, set_manual_mint_after_preload_barriers,
};
use crate::application::auto_tasks::delete::{
    AutoTaskDeleteParams, set_delete_before_lock_barrier,
};
use crate::application::auto_tasks::{
    DEFAULT_AUTO_TASK_FILES, auto_tasks_dir, cursor_state_path, definition_path,
    render_default_auto_task,
};
use crate::application::health::artifact::ArtifactKind;
use crate::application::managed_assets::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetLayout, load_managed_asset_manifest,
};
use crate::bootstrap::init::{InitOptions, init_workspace_at_root};

use super::{PausedDispatch, interval_params, seed_cursor};

fn delete(name: &str) -> AutoTaskDeleteParams {
    AutoTaskDeleteParams {
        name: name.to_string(),
        reason: Some("not used in this workspace".to_string()),
        force: false,
    }
}

fn cursor_names(runtime: &OrbitRuntime) -> Vec<String> {
    load_cursor_state(&cursor_state_path(&runtime.paths().state_dir))
        .expect("load cursor state")
        .definitions
        .into_keys()
        .collect()
}

fn cursor_bytes(runtime: &OrbitRuntime) -> Option<Vec<u8>> {
    std::fs::read(cursor_state_path(&runtime.paths().state_dir)).ok()
}

/// A workspace initialized with the shipped defaults, as `orbit workspace
/// init` leaves it.
fn seeded_runtime(root: &Path) -> (OrbitRuntime, PathBuf, PathBuf) {
    let global_root = root.join("global");
    let workspace_root = root.join("repo/.orbit");
    init_workspace_at_root(
        &global_root,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("initialize global root");
    reseed(&global_root, &workspace_root);
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    assert_eq!(runtime.paths().local_dir, workspace_root);
    (runtime, global_root, workspace_root)
}

/// The reseed `orbit workspace init --force` performs.
fn reseed(global_root: &Path, workspace_root: &Path) {
    init_workspace_at_root(
        workspace_root,
        InitOptions {
            global_root_override: Some(global_root.to_path_buf()),
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("reseed workspace");
}

fn opted_out(workspace_root: &Path) -> Vec<String> {
    load_managed_asset_manifest(
        &auto_tasks_dir(workspace_root).join(MANAGED_ASSET_MANIFEST_FILE),
        "auto_task",
        ManagedAssetLayout::YamlStem,
    )
    .expect("load auto-task manifest")
    .map(|manifest| manifest.opted_out.into_iter().collect())
    .unwrap_or_default()
}

fn auto_task_findings(runtime: &OrbitRuntime) -> Vec<String> {
    runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts")
        .into_iter()
        .find(|health| health.kind == ArtifactKind::AutoTask)
        .expect("auto-task health")
        .findings
        .into_iter()
        .map(|finding| format!("{} {:?}", finding.name, finding.condition))
        .collect()
}

#[test]
fn deleting_a_user_authored_definition_removes_file_and_cursor_without_an_opt_out() {
    let root = tempdir().expect("tempdir");
    let (runtime, _, workspace_root) = seeded_runtime(root.path());
    runtime
        .auto_task_add(interval_params("weekly-digest", 10_080))
        .expect("add");
    seed_cursor(&runtime, "weekly-digest");
    seed_cursor(&runtime, "backlog-hygiene");

    let report = runtime
        .auto_task_delete(delete("weekly-digest"))
        .expect("delete");

    assert!(!report.opted_out, "a user-authored name records no opt-out");
    assert!(report.cursor_removed);
    assert!(report.consumer.is_none());
    assert_eq!(report.deleted_by, runtime.actor_label());
    assert!(!definition_path(&workspace_root, "weekly-digest").exists());
    assert!(runtime.auto_task_show("weekly-digest").unwrap().is_none());
    assert_eq!(
        cursor_names(&runtime),
        vec!["backlog-hygiene".to_string()],
        "only the deleted definition's cursor goes"
    );
    assert!(opted_out(&workspace_root).is_empty());

    let audit = runtime
        .list_audit_events_with_kind(
            None,
            Some("orbit.auto_task.delete".to_string()),
            Some("auto_task".to_string()),
            None,
            None,
            10,
        )
        .expect("list audit events");
    assert_eq!(audit.len(), 1, "delete writes one audit record");
    let arguments = audit[0].arguments_json.as_deref().expect("audit payload");
    let payload: serde_json::Value = serde_json::from_str(arguments).expect("payload is JSON");
    assert_eq!(payload["name"], "weekly-digest");
    assert_eq!(payload["reason"], "not used in this workspace");
    assert_eq!(payload["deleted_by"], runtime.actor_label());
}

#[test]
fn a_deleted_shipped_default_survives_reseed_and_doctor() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, workspace_root) = seeded_runtime(root.path());
    let path = definition_path(&workspace_root, "code-review");
    assert!(path.is_file(), "init seeds the shipped code-review default");

    let report = runtime
        .auto_task_delete(delete("code-review"))
        .expect("delete shipped default");
    assert!(report.opted_out);
    assert_eq!(opted_out(&workspace_root), vec!["code-review".to_string()]);

    reseed(&global_root, &workspace_root);

    assert!(
        !path.exists(),
        "a reseed must not re-create an opted-out default"
    );
    assert!(
        definition_path(&workspace_root, "backlog-hygiene").is_file(),
        "other shipped defaults are still managed"
    );
    assert_eq!(opted_out(&workspace_root), vec!["code-review".to_string()]);
    let findings = auto_task_findings(&runtime);
    assert!(
        findings
            .iter()
            .all(|finding| !finding.starts_with("code-review ")),
        "doctor must not report an opted-out default: {findings:?}"
    );
}

#[test]
fn restore_reinstates_the_shipped_content_and_clears_the_opt_out() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, workspace_root) = seeded_runtime(root.path());
    runtime
        .auto_task_delete(delete("security-review"))
        .expect("delete shipped default");

    let restored = runtime
        .auto_task_restore("security-review")
        .expect("restore");

    assert!(!restored.enabled, "a restored default is inert, as shipped");
    let (_, embedded) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "security-review")
        .expect("shipped security-review");
    let path = definition_path(&workspace_root, "security-review");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read restored definition"),
        render_default_auto_task(embedded, runtime.workspace_base_branch())
    );
    assert!(opted_out(&workspace_root).is_empty());

    // Managed again: a reseed leaves it in place and doctor sees no drift.
    reseed(&global_root, &workspace_root);
    assert!(path.is_file());
    let findings = auto_task_findings(&runtime);
    assert!(
        findings
            .iter()
            .all(|finding| !finding.starts_with("security-review ")),
        "{findings:?}"
    );

    let error = runtime
        .auto_task_restore("security-review")
        .expect_err("an existing definition is not overwritten");
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[test]
fn restore_refuses_a_name_orbit_does_not_ship() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let error = runtime
        .auto_task_restore("weekly-digest")
        .expect_err("only shipped defaults can be restored");
    assert!(
        error.to_string().contains("not a shipped default"),
        "{error}"
    );
}

#[test]
fn delete_refuses_while_a_minted_task_is_open_unless_forced() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    runtime
        .auto_task_add(interval_params("nightly-chore", 1440))
        .expect("add");
    let minted = runtime.auto_task_mint("nightly-chore").expect("mint");

    let error = runtime
        .auto_task_delete(delete("nightly-chore"))
        .expect_err("an open minted task refuses the delete");
    assert!(
        error.to_string().contains(&minted.id),
        "the refusal names the open task: {error}"
    );
    assert!(
        runtime.auto_task_show("nightly-chore").unwrap().is_some(),
        "a refused delete leaves the definition"
    );

    let report = runtime
        .auto_task_delete(AutoTaskDeleteParams {
            force: true,
            ..delete("nightly-chore")
        })
        .expect("forced delete");
    assert_eq!(report.open_tasks, vec![minted.id.clone()]);
    assert!(runtime.auto_task_show("nightly-chore").unwrap().is_none());
    assert!(
        runtime.get_task(&minted.id).is_ok(),
        "a forced delete leaves the open task itself alone"
    );
}

#[test]
fn scheduler_mint_winning_delete_lock_refuses_non_force_delete() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let name = "racy-chore";
    runtime
        .auto_task_add(interval_params(name, 60))
        .expect("add definition");
    seed_cursor(&runtime, name);
    let mint_reached = Arc::new(Barrier::new(2));
    let mint_resume = Arc::new(Barrier::new(2));
    let delete_reached = Arc::new(Barrier::new(2));
    let dispatch = PausedDispatch {
        runtime: &runtime,
        mint: Some((Arc::clone(&mint_reached), Arc::clone(&mint_resume))),
        admission: None,
    };

    thread::scope(|scope| {
        let scheduler = scope.spawn(|| {
            run_auto_task_scheduler_at(
                &dispatch,
                Utc::now() + Duration::minutes(65),
                SchedulerOptions::default(),
            )
            .expect("scheduler pass")
        });
        mint_reached.wait(); // The scheduler holds the cursor lock at mint.
        let delete_barrier = Arc::clone(&delete_reached);
        let deletion = scope.spawn(|| {
            set_delete_before_lock_barrier(Some(delete_barrier));
            let result = runtime.auto_task_delete(delete(name));
            set_delete_before_lock_barrier(None);
            result
        });
        delete_reached.wait(); // Delete has reached its lock boundary.
        mint_resume.wait();

        let outcome = scheduler.join().expect("scheduler thread");
        assert_eq!(outcome.reports[0].action, "fired");
        let minted = outcome.reports[0].task_id.as_deref().expect("minted task");
        let error = deletion
            .join()
            .expect("deletion thread")
            .expect_err("open mint must refuse non-force deletion");
        assert!(error.to_string().contains(minted), "{error}");
        assert!(runtime.auto_task_show(name).unwrap().is_some());
        let state = load_cursor_state(&cursor_state_path(&runtime.paths().state_dir))
            .expect("cursor state");
        assert_eq!(
            state.definitions[name].last_task_id.as_deref(),
            Some(minted)
        );
    });
}

#[test]
fn preloaded_scheduler_cannot_recreate_cursor_after_real_delete() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let name = "deleted-before-admission";
    runtime
        .auto_task_add(interval_params(name, 60))
        .expect("add definition");
    seed_cursor(&runtime, name);
    let loaded = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let dispatch = PausedDispatch {
        runtime: &runtime,
        mint: None,
        admission: Some((Arc::clone(&loaded), Arc::clone(&resume))),
    };

    thread::scope(|scope| {
        let scheduler = scope.spawn(|| {
            run_auto_task_scheduler_at(
                &dispatch,
                Utc::now() + Duration::minutes(65),
                SchedulerOptions::default(),
            )
            .expect("scheduler pass")
        });
        loaded.wait(); // A validated definition was loaded before deletion.
        let (sender, receiver) = mpsc::channel();
        let runtime_ref = &runtime;
        let deletion = scope.spawn(move || {
            sender
                .send(runtime_ref.auto_task_delete(delete(name)))
                .expect("send delete result");
        });
        let deleted = receiver.recv_timeout(std::time::Duration::from_secs(20));
        resume.wait();
        deleted
            .expect("delete must finish before the preloaded scheduler resumes")
            .expect("delete succeeds");
        deletion.join().expect("deletion thread");

        let outcome = scheduler.join().expect("scheduler thread");
        assert_eq!(outcome.reports.len(), 1, "definition was preloaded");
        assert_eq!(outcome.reports[0].action, "skipped");
        assert_eq!(
            outcome.reports[0].reason.as_deref(),
            Some("definition_removed")
        );
        assert!(runtime.auto_task_show(name).unwrap().is_none());
        assert!(!cursor_names(&runtime).contains(&name.to_string()));
        assert!(runtime.list_tasks().expect("minted tasks").is_empty());
    });
}

#[test]
fn manual_mint_winning_admission_refuses_non_force_delete() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let name = "racy-manual-mint";
    let params = interval_params(name, 60);
    assert_eq!(params.dedupe, DedupePolicy::SkipIfOpen);
    runtime.auto_task_add(params).expect("add definition");
    runtime
        .auto_task_toggle(name, false)
        .expect("disable definition");
    seed_cursor(&runtime, name);
    let definition = definition_path(&runtime.paths().local_dir, name);
    let definition_before = std::fs::read(&definition).expect("definition bytes");
    let cursor_before = cursor_bytes(&runtime).expect("seeded cursor");
    let admitted = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let delete_reached = Arc::new(Barrier::new(2));

    thread::scope(|scope| {
        let admitted_for_mint = Arc::clone(&admitted);
        let release_for_mint = Arc::clone(&release);
        let mint = scope.spawn(|| {
            set_manual_mint_after_admission_barriers(Some((admitted_for_mint, release_for_mint)));
            let result = runtime.auto_task_mint(name);
            set_manual_mint_after_admission_barriers(None);
            result
        });
        admitted.wait(); // The minted task exists and mint still holds the lock.
        let delete_barrier = Arc::clone(&delete_reached);
        let deletion = scope.spawn(|| {
            set_delete_before_lock_barrier(Some(delete_barrier));
            let result = runtime.auto_task_delete(delete(name));
            set_delete_before_lock_barrier(None);
            result
        });
        delete_reached.wait(); // Delete has reached its lock boundary.
        release.wait();

        let minted = mint.join().expect("mint thread").expect("manual mint");
        let error = deletion
            .join()
            .expect("deletion thread")
            .expect_err("open mint must refuse non-force deletion");
        assert!(
            error.to_string().contains(&minted.id),
            "the refusal names the open task: {error}"
        );
        assert_eq!(
            std::fs::read(&definition).expect("definition preserved"),
            definition_before,
            "a refused delete leaves the definition bytes in place"
        );
        assert_eq!(
            cursor_bytes(&runtime).expect("cursor still present"),
            cursor_before,
            "manual mint must not rewrite scheduler cursor bytes"
        );
        let shown = runtime
            .auto_task_show(name)
            .expect("show")
            .expect("definition remains");
        assert!(!shown.enabled, "mint still ignores enabled");

        let report = runtime
            .auto_task_delete(AutoTaskDeleteParams {
                force: true,
                ..delete(name)
            })
            .expect("explicit force deletion remains supported");
        assert_eq!(report.open_tasks, vec![minted.id.clone()]);
        assert!(runtime.auto_task_show(name).unwrap().is_none());
        assert!(
            runtime.get_task(&minted.id).is_ok(),
            "force deletion leaves the open task itself alone"
        );
    });
}

#[test]
fn preloaded_manual_mint_cannot_create_from_a_deleted_definition() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let name = "deleted-before-manual-mint";
    let params = interval_params(name, 60);
    assert_eq!(params.dedupe, DedupePolicy::SkipIfOpen);
    runtime.auto_task_add(params).expect("add definition");
    runtime
        .auto_task_toggle(name, false)
        .expect("disable definition");
    seed_cursor(&runtime, name);
    let loaded = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));

    thread::scope(|scope| {
        let loaded_for_mint = Arc::clone(&loaded);
        let resume_for_mint = Arc::clone(&resume);
        let mint = scope.spawn(|| {
            set_manual_mint_after_preload_barriers(Some((loaded_for_mint, resume_for_mint)));
            let result = runtime.auto_task_mint(name);
            set_manual_mint_after_preload_barriers(None);
            result
        });
        loaded.wait(); // A validated definition was loaded before deletion.
        let (sender, receiver) = mpsc::channel();
        let runtime_ref = &runtime;
        let deletion = scope.spawn(move || {
            sender
                .send(runtime_ref.auto_task_delete(delete(name)))
                .expect("send delete result");
        });
        let deleted = receiver.recv_timeout(std::time::Duration::from_secs(20));
        let cursor_after_delete = cursor_bytes(&runtime);
        resume.wait();
        deleted
            .expect("delete must finish before the preloaded mint resumes")
            .expect("delete succeeds");
        deletion.join().expect("deletion thread");

        let error = mint
            .join()
            .expect("mint thread")
            .expect_err("a deleted definition must not mint");
        assert!(
            error.to_string().contains(name),
            "the refusal names the definition: {error}"
        );
        assert!(runtime.auto_task_show(name).unwrap().is_none());
        assert!(
            runtime.list_tasks().expect("minted tasks").is_empty(),
            "the preloaded definition must not become a task"
        );
        assert!(
            !cursor_names(&runtime).contains(&name.to_string()),
            "mint must not recreate the deleted cursor"
        );
        assert_eq!(
            cursor_bytes(&runtime),
            cursor_after_delete,
            "manual mint must not rewrite scheduler cursor bytes"
        );
    });
}

#[test]
fn delete_of_an_unknown_definition_errors() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let error = runtime
        .auto_task_delete(delete("missing"))
        .expect_err("unknown name");
    assert!(error.to_string().contains("no such auto-task"), "{error}");
}
