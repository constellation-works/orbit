//! Authoritative inputs to the shared material fingerprint.
//!
//! Dependency resolution [ORB-12544]: task ids are globally unique and a
//! dependency may name a task owned by another workspace on the same machine,
//! so those fixtures bind two workspaces into one coordination registry — the
//! shape that made `prepare_task_pilot` report an accepted prerequisite as
//! not found.
//!
//! Configurable freshness [ORB-13638]: which task edits and head moves make a
//! consumer's accepted assessment stale, resolved routine > config > default.

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use orbit_automation::members::MemberHost;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::TaskCreateParams;
use orbit_types::task::{
    Task, TaskPriority, TaskRelationType, TaskStatus, TaskType, task_dependencies_ready,
};
use orbit_types::task::{TaskComplexity, TaskRelation};
use orbit_types::workflow::automation::members::{
    FreshnessOverride, MaterialField, MemberAssessment, PreparationEligibility,
    PreparationFreshness, PreparationPolicy, SourceSensitivity, StateMember, StateTrigger,
    StateTriggerKind,
};
use std::path::PathBuf;
use tempfile::TempDir;

use super::super::{consumer_key, members::Host, preparation};
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}

/// Two checkouts registered on one machine: `dependent` carries the git
/// history preparation fingerprints against, `owner` only has to own tasks.
struct TwoWorkspaces {
    _root: TempDir,
    dependent: OrbitRuntime,
    owner: OrbitRuntime,
    revision: String,
}

fn two_workspaces() -> TwoWorkspaces {
    let root = tempfile::tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");

    let repo_root = root.path().join("dependent");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    git(&repo_root, &["init", "--initial-branch=agent-main"]);
    git(&repo_root, &["config", "user.name", "Test"]);
    git(
        &repo_root,
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(repo_root.join("sample.txt"), "baseline").unwrap();
    git(&repo_root, &["add", "sample.txt"]);
    git(&repo_root, &["commit", "-m", "baseline"]);

    let owner_root = root.path().join("owner").join(".orbit");
    std::fs::create_dir_all(&owner_root).expect("create owner workspace root");

    let dependent =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build dependent runtime");
    let owner = OrbitRuntime::from_roots(&global_root, &owner_root).expect("build owner runtime");
    let revision = preparation::head_revision(&dependent, "agent-main").expect("head revision");

    assert_ne!(
        dependent.workspace_id().expect("dependent workspace id"),
        owner.workspace_id().expect("owner workspace id"),
        "the fixture must register two distinct workspaces"
    );

    TwoWorkspaces {
        _root: root,
        dependent,
        owner,
        revision,
    }
}

/// Dependency evidence is material only when opted in [ORB-13638].
fn dependency_policy() -> PreparationPolicy {
    PreparationPolicy {
        freshness: PreparationFreshness {
            material_fields: vec![MaterialField::Dependencies],
            ..Default::default()
        },
        ..Default::default()
    }
}

fn create_task(runtime: &OrbitRuntime, title: &str, dependencies: Vec<String>) -> Task {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".into(),
            parent_id: None,
            title: title.into(),
            description: "test".into(),
            acceptance_criteria: Vec::new(),
            dependencies,
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: "test plan".into(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".into()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Proposed,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task")
}

/// The reported defect: an accepted dependency on a task owned by another
/// workspace on this machine failed preparation with `task not found`.
/// Resolving it through its registered owner also keeps the fingerprint
/// bound to the prerequisite's meaning, so completing it invalidates the
/// preparation that was computed while it was still open.
#[test]
fn same_host_cross_workspace_dependency_prepares_and_invalidates_on_status_change() {
    let fixture = two_workspaces();
    let prerequisite = create_task(&fixture.owner, "prerequisite", Vec::new());
    let dependent = create_task(
        &fixture.dependent,
        "dependent",
        vec![prerequisite.id.clone()],
    );
    assert_eq!(
        dependent
            .relations
            .iter()
            .filter(|relation| relation.relation_type == TaskRelationType::BlockedBy)
            .map(|relation| relation.target.as_str())
            .collect::<Vec<_>>(),
        vec![prerequisite.id.as_str()],
        "the cross-workspace dependency is accepted at creation"
    );

    let before = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &dependency_policy(),
    )
    .expect("prepare a task whose prerequisite lives in another local workspace");

    // Admission reads the same prerequisite through the registry-wide status
    // projection, so preparation and readiness must agree at each step.
    let statuses = fixture
        .dependent
        .task_status_index()
        .expect("status projection");
    assert_eq!(statuses.get(&prerequisite.id), Some(&TaskStatus::Proposed));
    assert!(
        !task_dependencies_ready(&dependent, &statuses),
        "an open prerequisite is unmet, wherever it is owned"
    );

    fixture
        .owner
        .apply_task_automation_update(
            &prerequisite.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("complete the prerequisite in its own workspace");

    let after = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &dependency_policy(),
    )
    .expect("re-prepare after the prerequisite changed");
    assert_ne!(
        before, after,
        "the prerequisite's status is material, so preparation computed before it \
         completed must not stay valid"
    );

    let statuses = fixture
        .dependent
        .task_status_index()
        .expect("status projection");
    assert!(
        task_dependencies_ready(&dependent, &statuses),
        "readiness agrees once the prerequisite is done"
    );

    // Reading across the boundary is not authority over it: the dependent
    // workspace still lists only its own task.
    assert_eq!(
        fixture
            .dependent
            .list_tasks()
            .expect("list")
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>(),
        vec![dependent.id.clone()]
    );
    assert_eq!(
        fixture
            .owner
            .get_task(&prerequisite.id)
            .expect("owner still owns its task")
            .status,
        TaskStatus::Done
    );
}

fn set_owner_status(fixture: &TwoWorkspaces, id: &str, status: TaskStatus) {
    fixture
        .owner
        .apply_task_automation_update(
            id,
            TaskAutomationUpdate {
                status: Some(status),
                ..TaskAutomationUpdate::default()
            },
        )
        .unwrap_or_else(|error| panic!("move {id} to {status}: {error}"));
}

/// Task-pilot evidence and readiness apply the archived-dependency rule
/// through the prerequisite's registered owner: archived after `done`, it
/// prepares exactly as the `done` prerequisite did; reopened and then
/// archived, it is unmet again and the preparation goes stale.
#[test]
fn cross_workspace_dependency_archived_after_done_stays_satisfied_for_task_pilot() {
    let fixture = two_workspaces();
    let prerequisite = create_task(&fixture.owner, "prerequisite", Vec::new());
    let dependent = create_task(
        &fixture.dependent,
        "dependent",
        vec![prerequisite.id.clone()],
    );
    let fingerprint = || {
        preparation::fingerprint(
            &fixture.dependent,
            &dependent,
            &fixture.revision,
            &dependency_policy(),
        )
        .expect("prepare the dependent")
    };

    set_owner_status(&fixture, &prerequisite.id, TaskStatus::Done);
    let while_done = fingerprint();
    set_owner_status(&fixture, &prerequisite.id, TaskStatus::Archived);

    assert_eq!(
        fingerprint(),
        while_done,
        "archiving finished work must not change what task-pilot prepared against"
    );
    let statuses = fixture
        .dependent
        .dependency_status_index([&dependent])
        .expect("dependency projection");
    assert_eq!(statuses.get(&prerequisite.id), Some(&TaskStatus::Done));
    assert!(task_dependencies_ready(&dependent, &statuses));

    set_owner_status(&fixture, &prerequisite.id, TaskStatus::Backlog);
    set_owner_status(&fixture, &prerequisite.id, TaskStatus::Archived);

    assert_ne!(
        fingerprint(),
        while_done,
        "a prerequisite reopened before its archive is no longer done"
    );
    let statuses = fixture
        .dependent
        .dependency_status_index([&dependent])
        .expect("dependency projection");
    assert_eq!(statuses.get(&prerequisite.id), Some(&TaskStatus::Archived));
    assert!(!task_dependencies_ready(&dependent, &statuses));
}

/// A dependency this machine's registry can never resolve stays explicitly
/// unverified: preparation records the reference instead of inventing a
/// status for it, and never reports it as satisfied.
#[test]
fn another_host_dependency_is_recorded_as_unverifiable_not_satisfied() {
    let fixture = two_workspaces();
    let dependent = create_task(
        &fixture.dependent,
        "dependent",
        vec!["ZZZ-00001".to_string()],
    );

    let with_reference = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &dependency_policy(),
    )
    .expect("prepare a task referencing another host's task");

    let mut without_reference = dependent.clone();
    without_reference.relations.clear();
    let without_reference = preparation::fingerprint(
        &fixture.dependent,
        &without_reference,
        &fixture.revision,
        &dependency_policy(),
    )
    .expect("prepare the same task without the reference");
    assert_ne!(
        with_reference, without_reference,
        "an unverifiable prerequisite is still part of the task's material"
    );

    let mut satisfied = dependent.clone();
    satisfied.relations.clear();
    let statuses = fixture
        .dependent
        .task_status_index()
        .expect("status projection");
    assert_eq!(
        statuses.get("ZZZ-00001"),
        None,
        "no status is ever invented for another host's task"
    );
    assert_eq!(
        preparation::fingerprint(
            &fixture.dependent,
            &satisfied,
            &fixture.revision,
            &dependency_policy(),
        )
        .expect("fingerprint"),
        without_reference
    );
}

/// A prerequisite under a prefix this machine does own, with nothing bound to
/// it, is gone rather than elsewhere: preparation fails closed and names it.
#[test]
fn a_deleted_prerequisite_fails_preparation_closed_with_an_actionable_reason() {
    let fixture = two_workspaces();
    let prerequisite = create_task(&fixture.owner, "prerequisite", Vec::new());
    let dependent = create_task(
        &fixture.dependent,
        "dependent",
        vec![prerequisite.id.clone()],
    );
    preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &dependency_policy(),
    )
    .expect("prepare while the prerequisite exists");

    fixture
        .owner
        .delete_task(&prerequisite.id)
        .expect("delete the prerequisite in its own workspace");

    let error = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &dependency_policy(),
    )
    .expect_err("a vanished prerequisite must not prepare");
    let message = error.to_string();
    assert!(
        message.contains(&prerequisite.id) && message.contains(&dependent.id),
        "the refusal must name both tasks: {message}"
    );
    assert!(
        message.contains("no workspace registered on this machine owns"),
        "the refusal must state why it is unresolvable: {message}"
    );

    let statuses = fixture
        .dependent
        .task_status_index()
        .expect("status projection");
    assert!(
        !task_dependencies_ready(&dependent, &statuses),
        "admission agrees the dependency is unmet rather than satisfied"
    );
}

/// The budget guard still applies to references this machine cannot resolve,
/// so an unverifiable dependency cannot buy unbounded scanning.
#[test]
fn preparation_keeps_its_dependency_budget() {
    let fixture = two_workspaces();
    let mut dependent = create_task(&fixture.dependent, "dependent", Vec::new());
    dependent.relations = (1..=51)
        .map(|number| orbit_types::task::TaskRelation {
            relation_type: TaskRelationType::BlockedBy,
            target: format!("ZZZ-{number:05}"),
        })
        .collect();

    let error = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &dependency_policy(),
    )
    .expect_err("51 dependencies exceed the scan budget");
    assert!(
        error.to_string().contains("dependency_scan_budget"),
        "unexpected error: {error}"
    );
    let _ = Utc::now();
}

/// One workspace whose repository holds a selector target, an unrelated file
/// and room for instructions, with optional workspace `config.toml`.
struct Workspace {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
}

fn workspace(config_toml: Option<&str>) -> Workspace {
    let root = tempfile::tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo = root.path().join("repo");
    let workspace_root = repo.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(workspace_root.join("routines")).expect("create routines dir");
    if let Some(config_toml) = config_toml {
        std::fs::write(workspace_root.join("config.toml"), config_toml).expect("write config");
    }
    git(&repo, &["init", "--initial-branch=agent-main"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    commit(&repo, "src/lib.rs", "pub fn target() {}\n");
    commit(&repo, "other.txt", "unrelated\n");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
        .expect("build runtime")
        .with_automation_machine_identity(Some("fixture-machine".into()));
    Workspace {
        _root: root,
        runtime,
        repo,
    }
}

fn commit(repo: &Path, path: &str, content: &str) {
    let file = repo.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, content).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-m", path]);
}

fn pilot_trigger(freshness: FreshnessOverride) -> StateTrigger {
    StateTrigger {
        kind: StateTriggerKind::PreparationEligible,
        owner_machine: "fixture-machine".into(),
        branch: "agent-main".into(),
        debounce_minutes: 2,
        max_wait_minutes: 10,
        max_items: 50,
        retries: 1,
        deadline_minutes: 30,
        batch_size: None,
        eligibility: PreparationEligibility::default(),
        freshness,
    }
}

fn observed_member(runtime: &OrbitRuntime, trigger: &StateTrigger, id: &str) -> StateMember {
    Host::new(runtime, "task-pilot", trigger)
        .observe(None, Utc::now())
        .expect("observe")
        .candidates
        .into_iter()
        .find(|member| member.key == id)
        .expect("the eligible task is a candidate")
}

fn observed(runtime: &OrbitRuntime, trigger: &StateTrigger, id: &str) -> String {
    observed_member(runtime, trigger, id).fingerprint
}

fn update(runtime: &OrbitRuntime, id: &str, params: TaskUpdateParams) {
    runtime.update_task(id, params).expect("update task");
}

fn scoped_task(runtime: &OrbitRuntime, title: &str, selectors: &[&str]) -> String {
    let id = create_task(runtime, title, Vec::new()).id;
    update(
        runtime,
        &id,
        TaskUpdateParams {
            context_files: Some(selectors.iter().map(|s| (*s).to_string()).collect()),
            ..Default::default()
        },
    );
    id
}

/// [ORB-13638] The reported re-pilot waves: with the default freshness only
/// an edit to title, description, criteria, plan or selectors re-admits an
/// already-assessed task. Crew, tags, type, complexity, relations, a
/// dependency completing, new instruction files and head moves do not.
#[test]
fn default_freshness_readmits_only_meaning_and_selector_edits() {
    let ws = workspace(None);
    let prerequisite = create_task(&ws.runtime, "prerequisite", Vec::new());
    let id = scoped_task(&ws.runtime, "task", &["file:src/lib.rs"]);
    let trigger = pilot_trigger(FreshnessOverride::default());
    let assessed = observed(&ws.runtime, &trigger, &id);

    let unrelated: Vec<(&str, TaskUpdateParams)> = vec![
        (
            "crew",
            TaskUpdateParams {
                crew: Some(Some("opus".into())),
                ..Default::default()
            },
        ),
        (
            "tags",
            TaskUpdateParams {
                tags: Some(vec!["retagged".into()]),
                ..Default::default()
            },
        ),
        (
            "type",
            TaskUpdateParams {
                task_type: Some(TaskType::Feature),
                ..Default::default()
            },
        ),
        (
            "complexity",
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Low),
                ..Default::default()
            },
        ),
        (
            "relations",
            TaskUpdateParams {
                relations: Some(vec![TaskRelation {
                    relation_type: TaskRelationType::BlockedBy,
                    target: prerequisite.id.clone(),
                }]),
                ..Default::default()
            },
        ),
    ];
    for (label, params) in unrelated {
        update(&ws.runtime, &id, params);
        assert_eq!(observed(&ws.runtime, &trigger, &id), assessed, "{label}");
    }
    ws.runtime
        .apply_task_automation_update(
            &prerequisite.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("complete the prerequisite");
    assert_eq!(
        observed(&ws.runtime, &trigger, &id),
        assessed,
        "dependency status"
    );
    commit(&ws.repo, "AGENTS.md", "New instructions.\n");
    commit(&ws.repo, "src/lib.rs", "pub fn target() { /* merged */ }\n");
    assert_eq!(
        observed(&ws.runtime, &trigger, &id),
        assessed,
        "instruction files and the branch head"
    );

    let meaning: Vec<(&str, TaskUpdateParams)> = vec![
        (
            "title",
            TaskUpdateParams {
                title: Some("retitled".into()),
                ..Default::default()
            },
        ),
        (
            "description",
            TaskUpdateParams {
                description: Some("rescoped".into()),
                ..Default::default()
            },
        ),
        (
            "criteria",
            TaskUpdateParams {
                acceptance_criteria: Some(vec!["a new criterion".into()]),
                ..Default::default()
            },
        ),
        (
            "plan",
            TaskUpdateParams {
                plan: Some("a new plan".into()),
                ..Default::default()
            },
        ),
        (
            "selectors",
            TaskUpdateParams {
                context_files: Some(vec!["file:other.txt".into()]),
                ..Default::default()
            },
        ),
    ];
    let mut previous = assessed;
    for (label, params) in meaning {
        update(&ws.runtime, &id, params);
        let next = observed(&ws.runtime, &trigger, &id);
        assert_ne!(next, previous, "{label} re-admits the task");
        previous = next;
    }
}

/// [ORB-13638] A field a consumer opts into re-admits the task when it
/// changes, and the consumer's widened set is itself new material.
#[test]
fn opted_in_crew_and_tags_readmit() {
    let ws = workspace(None);
    let id = scoped_task(&ws.runtime, "task", &["file:src/lib.rs"]);
    let default = observed(
        &ws.runtime,
        &pilot_trigger(FreshnessOverride::default()),
        &id,
    );
    let trigger = pilot_trigger(FreshnessOverride {
        material_fields: Some(vec![
            MaterialField::Title,
            MaterialField::Crew,
            MaterialField::Tags,
        ]),
        ..Default::default()
    });
    let before = observed(&ws.runtime, &trigger, &id);
    assert_ne!(before, default, "a non-default material set is material");

    update(
        &ws.runtime,
        &id,
        TaskUpdateParams {
            crew: Some(Some("opus".into())),
            ..Default::default()
        },
    );
    let after_crew = observed(&ws.runtime, &trigger, &id);
    assert_ne!(after_crew, before, "an opted-in crew change re-admits");
    update(
        &ws.runtime,
        &id,
        TaskUpdateParams {
            tags: Some(vec!["retagged".into()]),
            ..Default::default()
        },
    );
    let after_tags = observed(&ws.runtime, &trigger, &id);
    assert_ne!(after_tags, after_crew, "an opted-in tag change re-admits");
    update(
        &ws.runtime,
        &id,
        TaskUpdateParams {
            plan: Some("a new plan".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        observed(&ws.runtime, &trigger, &id),
        after_tags,
        "the plan is not in this consumer's set"
    );
}

/// [ORB-13638] `ignore` never re-admits on a head move, `any` always does,
/// and `context_files` only when the new commits touch a path one of the
/// task's selectors names: `file:` exactly, `dir:` by prefix, `symbol:`
/// through its file.
#[test]
fn source_sensitivity_modes_decide_which_head_moves_readmit() {
    let ws = workspace(None);
    let file = scoped_task(&ws.runtime, "file", &["file:src/lib.rs"]);
    let dir = scoped_task(&ws.runtime, "dir", &["dir:src"]);
    let symbol = scoped_task(&ws.runtime, "symbol", &["symbol:src/lib.rs#target:fn"]);
    let modes = [
        SourceSensitivity::Ignore,
        SourceSensitivity::ContextFiles,
        SourceSensitivity::Any,
    ];
    let snapshot = |id: &str| {
        modes
            .iter()
            .map(|mode| {
                observed(
                    &ws.runtime,
                    &pilot_trigger(FreshnessOverride {
                        source_sensitivity: Some(*mode),
                        ..Default::default()
                    }),
                    id,
                )
            })
            .collect::<Vec<_>>()
    };
    let changed = |before: &[String], after: &[String]| {
        before
            .iter()
            .zip(after)
            .map(|(before, after)| before != after)
            .collect::<Vec<_>>()
    };
    let ids = [&file, &dir, &symbol];
    let before = ids.map(|id| snapshot(id));

    commit(&ws.repo, "other.txt", "unrelated merge\n");
    let unrelated = ids.map(|id| snapshot(id));
    for (before, after) in before.iter().zip(&unrelated) {
        assert_eq!(
            changed(before, after),
            vec![false, false, true],
            "unrelated commit"
        );
    }

    commit(
        &ws.repo,
        "src/lib.rs",
        "pub fn target() { /* touched */ }\n",
    );
    let touched = ids.map(|id| snapshot(id));
    for (before, after) in unrelated.iter().zip(&touched) {
        assert_eq!(
            changed(before, after),
            vec![false, true, true],
            "selector touched"
        );
    }

    commit(&ws.repo, "src/new.rs", "pub fn added() {}\n");
    let added = ids.map(|id| snapshot(id));
    assert_eq!(
        changed(&touched[1], &added[1]),
        vec![false, true, true],
        "a new file under a dir: selector touches it"
    );
    assert_eq!(
        changed(&touched[0], &added[0]),
        vec![false, false, true],
        "a sibling file does not touch a file: selector"
    );
}

/// [ORB-13638] Freshness resolves per key routine > `config.toml` > default,
/// and every consumer of one routine — the scheduling host, and the
/// prepare/apply/promotion path that re-reads the routine — resolves the same
/// value.
#[test]
fn freshness_resolves_routine_over_config_over_default_for_every_consumer() {
    let ws = workspace(Some(
        "[workflow.task_pilot_freshness]\nmaterial_fields = [\"title\", \"crew\"]\nsource_sensitivity = \"any\"\n",
    ));
    let configured = PreparationFreshness {
        material_fields: vec![MaterialField::Title, MaterialField::Crew],
        source_sensitivity: SourceSensitivity::Any,
    };
    assert_eq!(
        preparation::resolve_policy(&ws.runtime, None).freshness,
        configured,
        "config replaces the default"
    );
    assert_eq!(
        preparation::resolve_policy(
            &ws.runtime,
            Some(&pilot_trigger(FreshnessOverride::default()))
        )
        .freshness,
        configured,
        "a routine without a block inherits config"
    );
    let routine = FreshnessOverride {
        source_sensitivity: Some(SourceSensitivity::Ignore),
        ..Default::default()
    };
    assert_eq!(
        preparation::resolve_policy(&ws.runtime, Some(&pilot_trigger(routine.clone()))).freshness,
        PreparationFreshness {
            source_sensitivity: SourceSensitivity::Ignore,
            ..configured.clone()
        },
        "a routine key wins; its absent keys keep config"
    );
    assert_eq!(
        preparation::resolve_policy(&workspace(None).runtime, None).freshness,
        PreparationFreshness::default(),
        "no config and no routine is the built-in default"
    );

    // The prepare/apply side resolves through the routine catalog by the
    // claim's consumer key; it must agree with the scheduling host.
    std::fs::write(
        ws.runtime.shared_root().join("routines/pilot.yaml"),
        "schemaVersion: 1\nname: pilot\nenabled: false\ntarget: job:task_pilot_pipeline\n\
         trigger:\n  state:\n    kind: preparation_eligible\n    owner_machine: fixture-machine\n\
         \x20   branch: agent-main\n    debounce_minutes: 2\n    max_wait_minutes: 10\n\
         \x20   max_items: 50\n    retries: 1\n    deadline_minutes: 30\n\
         \x20   freshness: {source_sensitivity: ignore}\n\
         policy:\n  overlap: forbid\n  timeout_minutes: 30\n  retries: {max: 1, backoff_minutes: 5}\n",
    )
    .expect("write routine");
    let consumer = consumer_key(&ws.runtime, "routine", "pilot").expect("consumer key");
    assert_eq!(
        preparation::consumer_policy(&ws.runtime, &consumer).expect("consumer policy"),
        preparation::resolve_policy(&ws.runtime, Some(&pilot_trigger(routine.clone()))),
    );

    // Behaviour follows the resolved value: config opts crew and the head in,
    // the routine opts the head back out.
    let id = scoped_task(&ws.runtime, "task", &["file:src/lib.rs"]);
    let inherited = pilot_trigger(FreshnessOverride::default());
    let overridden = pilot_trigger(routine);
    let (inherited_before, overridden_before) = (
        observed(&ws.runtime, &inherited, &id),
        observed(&ws.runtime, &overridden, &id),
    );
    commit(&ws.repo, "other.txt", "unrelated merge\n");
    assert_ne!(observed(&ws.runtime, &inherited, &id), inherited_before);
    assert_eq!(observed(&ws.runtime, &overridden, &id), overridden_before);
    update(
        &ws.runtime,
        &id,
        TaskUpdateParams {
            crew: Some(Some("opus".into())),
            ..Default::default()
        },
    );
    assert_ne!(
        observed(&ws.runtime, &overridden, &id),
        overridden_before,
        "crew is material through config"
    );
}

/// [ORB-13638] Upgrading must not re-pilot every assessed task: an assessment
/// certified under `material_v1` is carried forward while that contract's
/// hash, recomputed at the revision its attempt pinned, still matches — even
/// after the head moved. A meaning edit, or no pinned revision, re-assesses.
#[test]
fn material_v1_assessment_is_carried_forward_until_the_task_changes() {
    let ws = workspace(None);
    let id = scoped_task(&ws.runtime, "task", &["file:src/lib.rs"]);
    let revision = preparation::head_revision(&ws.runtime, "agent-main").expect("head");
    git(
        &ws.repo,
        &["update-ref", "refs/orbit/automation/attempt-v1", &revision],
    );
    let task = ws.runtime.get_task(&id).expect("task");
    let instructions = preparation::instructions(&ws.runtime, &revision).expect("instructions");
    let certified = preparation::legacy_fingerprint(
        &ws.runtime,
        &task,
        &revision,
        &instructions,
        &PreparationEligibility::default(),
    )
    .expect("legacy fingerprint");
    let assessment = MemberAssessment {
        input_fingerprint: certified.clone(),
        resulting_fingerprint: certified,
        ready: true,
        receipt_id: "attempt-v1".into(),
    };

    commit(&ws.repo, "src/lib.rs", "pub fn target() { /* merged */ }\n");
    let trigger = pilot_trigger(FreshnessOverride::default());
    let host = Host::new(&ws.runtime, "task-pilot", &trigger);
    let member = observed_member(&ws.runtime, &trigger, &id);
    assert_ne!(member.fingerprint, assessment.resulting_fingerprint);
    assert!(host.carries_forward(&member, &assessment));
    assert!(
        !host.carries_forward(
            &member,
            &MemberAssessment {
                receipt_id: "never-pinned".into(),
                ..assessment.clone()
            }
        ),
        "without the pinned revision the certificate cannot be checked"
    );

    update(
        &ws.runtime,
        &id,
        TaskUpdateParams {
            title: Some("retitled".into()),
            ..Default::default()
        },
    );
    let member = observed_member(&ws.runtime, &trigger, &id);
    assert!(
        !Host::new(&ws.runtime, "task-pilot", &trigger).carries_forward(&member, &assessment),
        "a meaning edit since the v1 assessment re-assesses"
    );
}
