//! Explore-brief delivery at the implement dispatch boundary. A brief the
//! task pilot stored reaches the implementer only while it is current: the
//! task's material is still what the pilot's receipt sealed (admission's own
//! status change is not a material edit), and the code the brief covers has
//! not drifted past the limits since its revision. Every other case is
//! recorded without delivering it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use orbit_common::security::release::sha256_hex;
use orbit_core::OrbitRuntime;
use orbit_core::TaskStatus;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::{JobRunStoreBackend, V2AuditEventInsertParams};
use orbit_tools::ToolContext;
use orbit_types::task::EXPLORE_BRIEF_ARTIFACT;
use orbit_types::workflow::{
    ExploreBriefAdmission, ExploreBriefDropReason, ExploreBriefOutcome, PipelineState,
    V2AuditEnvelope, V2AuditEvent, V2AuditEventKind,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::git_test_support;

const ENABLED: &str = "[workflow.explore_brief]\nenabled = true\n";

struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Fixture {
    /// A Git repository holding `src/lib.rs`, with `.orbit/config.toml`
    /// holding `config`.
    fn new(config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let workspace = repo.join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), config).unwrap();
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "fixture@example.com"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let fixture = Self {
            _root: root,
            repo,
            runtime,
            jobs,
        };
        fixture.commit("src/lib.rs", "pub fn greet() {}\n");
        fixture
    }

    /// Write `path` and commit it, returning the new commit.
    fn commit(&self, path: &str, contents: &str) -> String {
        let file = self.repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, contents).unwrap();
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", path]);
        git(&self.repo, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn task(&self) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: "Explore brief fixture".into(),
                description: "Make greet return a greeting.".into(),
                acceptance_criteria: vec!["greet returns a greeting.".into()],
                plan: "Edit greet.".into(),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    /// Pilot `task` through the real prepare and apply actions, returning a
    /// brief that maps `src/lib.rs`.
    fn pilot(&self, task: &str) {
        let path = self.repo.canonicalize().unwrap();
        let prepared = self
            .runtime
            .run_deterministic(
                "prepare_task_pilot",
                &json!({}),
                &json!({
                    "workspace_path": path,
                    "task_ids": [task],
                    "base_branch": "main",
                }),
                ToolContext::default(),
            )
            .expect("prepare task pilot");
        let revision = prepared["source"]["source_revision"]
            .as_str()
            .expect("prepared source revision")
            .to_string();
        let tree = git(&self.repo, &["rev-parse", &format!("{revision}^{{tree}}")])
            .trim()
            .to_string();
        let output = self
            .runtime
            .run_deterministic(
                "apply_task_pilot_results",
                &json!({}),
                &json!({
                    "workspace_path": prepared["workspace_path"],
                    "prepared": prepared,
                    "results": [{
                        "partition_index": 0,
                        "observed_revision": revision,
                        "observed_tree": tree,
                        "task_ids": [task],
                        "tasks": [{
                            "task_id": task,
                            "context_files_after": ["file:src/lib.rs"],
                            "disposition": "selectors",
                            "recommended_crew": "opus",
                            "recommended_complexity": "low",
                            "assessment_rationale": "src/lib.rs holds greet.",
                            "validation_approach": "Call greet.",
                            "confidence": "high",
                            "evidence_gaps": [],
                            "reassessment_triggers": [],
                            "blocked_by": [],
                            "adr_conflicts": [],
                            "utility_warnings": [],
                            "surface_warnings": [],
                            "duplicate_of": Value::Null,
                            "already_landed": Value::Null,
                            "explore_brief": {
                                "change": [{
                                    "target": "symbol:src/lib.rs#greet:function",
                                    "what": "Return a greeting.",
                                }],
                                "pattern": [{"target": "file:README.md", "why": "The only doc."}],
                                "confidence": "high",
                            },
                        }],
                    }],
                }),
                ToolContext::default(),
            )
            .expect("apply task pilot");
        assert_eq!(output["applied_count"], 1, "{output}");
        assert!(
            self.runtime
                .get_task_artifact(task, EXPLORE_BRIEF_ARTIFACT)
                .unwrap()
                .is_some(),
            "the pilot stored the brief: {output}"
        );
    }

    /// Pilot `task` with a brief that maps the shell-awkward drift targets
    /// alongside `src/lib.rs`: a space, an apostrophe and a bracket path.
    fn pilot_covering_awkward_paths(&self, task: &str) {
        let path = self.repo.canonicalize().unwrap();
        let prepared = self
            .runtime
            .run_deterministic(
                "prepare_task_pilot",
                &json!({}),
                &json!({
                    "workspace_path": path,
                    "task_ids": [task],
                    "base_branch": "main",
                }),
                ToolContext::default(),
            )
            .expect("prepare task pilot");
        let revision = prepared["source"]["source_revision"]
            .as_str()
            .expect("prepared source revision")
            .to_string();
        let tree = git(&self.repo, &["rev-parse", &format!("{revision}^{{tree}}")])
            .trim()
            .to_string();
        let output = self
            .runtime
            .run_deterministic(
                "apply_task_pilot_results",
                &json!({}),
                &json!({
                    "workspace_path": prepared["workspace_path"],
                    "prepared": prepared,
                    "results": [{
                        "partition_index": 0,
                        "observed_revision": revision,
                        "observed_tree": tree,
                        "task_ids": [task],
                        "tasks": [{
                            "task_id": task,
                            "context_files_after": [
                                "file:src/lib.rs",
                                "file:docs/Getting Started.md",
                                "file:it's.md",
                                "file:app/[slug]/page.tsx",
                            ],
                            "disposition": "selectors",
                            "recommended_crew": "opus",
                            "recommended_complexity": "low",
                            "assessment_rationale": "src/lib.rs holds greet.",
                            "validation_approach": "Call greet.",
                            "confidence": "high",
                            "evidence_gaps": [],
                            "reassessment_triggers": [],
                            "blocked_by": [],
                            "adr_conflicts": [],
                            "utility_warnings": [],
                            "surface_warnings": [],
                            "duplicate_of": Value::Null,
                            "already_landed": Value::Null,
                            "explore_brief": {
                                "change": [{
                                    "target": "symbol:src/lib.rs#greet:function",
                                    "what": "Return a greeting.",
                                }],
                                "pattern": [{"target": "file:README.md", "why": "The only doc."}],
                                "maps": [
                                    "file:docs/Getting Started.md",
                                    "file:it's.md",
                                    "file:app/[slug]/page.tsx",
                                ],
                                "confidence": "high",
                            },
                        }],
                    }],
                }),
                ToolContext::default(),
            )
            .expect("apply task pilot");
        assert_eq!(output["applied_count"], 1, "{output}");
        assert!(
            self.runtime
                .get_task_artifact(task, EXPLORE_BRIEF_ARTIFACT)
                .unwrap()
                .is_some(),
            "the pilot stored the brief: {output}"
        );
    }

    /// Admit `task` as an implementation would: its status moves to
    /// `in-progress`, which is not a material edit.
    fn start(&self, task: &str) {
        self.runtime
            .apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    /// A pipeline run whose `worktree` step recorded `base_sha`.
    fn run(&self, task: &str, base_sha: &str) -> String {
        let run = self
            .jobs
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({ "task_id": task })),
                None,
            )
            .unwrap();
        let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
        state.pipeline = json!({ "worktree": { "base_sha": base_sha } });
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }

    fn admission(&self, task: &str, base_sha: &str) -> ExploreBriefAdmission {
        let run = self.run(task, base_sha);
        RuntimeHost::explore_brief_for_activity(
            &self.runtime,
            &run,
            "agent_implement",
            &json!({ "task_id": task }),
        )
        .expect("a single-task input gets an outcome")
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = git_test_support::run(repo, args).unwrap();
    assert!(output.success, "git {args:?} failed: {}", output.stderr);
    output.stdout
}

fn an_unchanged_task_receives_its_brief_until_a_material_edit() {
    let fx = Fixture::new(ENABLED);
    let task = fx.task();
    let inspected = git(&fx.repo, &["rev-parse", "HEAD"]).trim().to_string();
    let absent = fx.admission(&task, &inspected);
    assert_eq!(absent.record.label(), "absent", "no brief is stored yet");
    assert!(absent.brief.is_none());

    fx.pilot(&task);
    fx.start(&task);
    let stored = fx
        .runtime
        .get_task_artifact(&task, EXPLORE_BRIEF_ARTIFACT)
        .unwrap()
        .unwrap();

    let fresh = fx.admission(&task, &inspected);
    assert_eq!(fresh.record.label(), "fresh", "{:?}", fresh.record);
    assert_eq!(fresh.record.sha256, Some(sha256_hex(&stored.content)));
    assert_eq!(
        fresh.record.inspection_revision.as_deref(),
        Some(inspected.as_str())
    );
    let brief = fresh.brief.expect("a fresh brief is delivered");
    assert_eq!(brief["status"], "fresh");
    assert_eq!(brief["inspection_revision"], inspected);
    assert_eq!(
        brief["brief"]["change"][0]["target"],
        "symbol:src/lib.rs#greet:function"
    );
    assert!(brief.get("drift").is_none());

    let moved = fx.commit("src/lib.rs", "pub fn greet() {}\npub fn wave() {}\n");
    let drifted = fx.admission(&task, &moved);
    assert_eq!(drifted.record.label(), "drifted", "{:?}", drifted.record);
    assert_eq!(drifted.record.drifted_paths, ["src/lib.rs"]);
    let brief = drifted.brief.expect("a drifted brief is delivered");
    assert_eq!(
        brief["drift"]["files"],
        json!([{ "path": "src/lib.rs", "added": 1, "deleted": 0 }])
    );
    assert_eq!(
        brief["drift"]["diff_command"],
        format!("git diff {inspected} {moved} -- ':(literal)src/lib.rs'"),
        "the patch itself is never inlined"
    );

    let rewritten = fx.commit("src/lib.rs", &"// line\n".repeat(250));
    let stale = fx.admission(&task, &rewritten);
    assert_eq!(stale.record.outcome, ExploreBriefOutcome::Dropped);
    assert_eq!(stale.record.reason, Some(ExploreBriefDropReason::Stale));
    assert!(stale.brief.is_none());

    // A material edit with no pilot receipt after it: the stored brief, its
    // receipt and the fingerprint they share still agree with one another,
    // but no longer with the task.
    fx.runtime
        .update_task_as_human(
            &task,
            TaskUpdateParams {
                description: Some("Make greet return a farewell instead.".into()),
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
    let edited = fx.admission(&task, &inspected);
    assert_eq!(
        edited.record.label(),
        "dropped: material_changed",
        "{:?}",
        edited.record
    );
    assert!(
        edited.brief.is_none(),
        "a superseded map is never delivered"
    );
    assert_eq!(edited.record.sha256, fresh.record.sha256);
}

fn a_brief_kept_in_shadow_is_classified_but_not_delivered() {
    let fx = Fixture::new(&format!("{ENABLED}deliver = false\n"));
    let task = fx.task();
    let inspected = git(&fx.repo, &["rev-parse", "HEAD"]).trim().to_string();
    fx.pilot(&task);
    fx.start(&task);

    let shadow = fx.admission(&task, &inspected);
    assert_eq!(shadow.record.label(), "fresh", "{:?}", shadow.record);
    assert!(shadow.brief.is_none(), "deliver = false gives no brief");
}

fn the_run_record_names_each_dispatch_outcome() {
    let fx = Fixture::new(ENABLED);
    let task = fx.task();
    let inspected = git(&fx.repo, &["rev-parse", "HEAD"]).trim().to_string();
    let run = fx.run(&task, &inspected);
    let mut record = fx.admission(&task, &inspected).record.clone();
    record.outcome = ExploreBriefOutcome::Dropped;
    record.reason = Some(ExploreBriefDropReason::MaterialChanged);
    let event = V2AuditEvent {
        envelope: V2AuditEnvelope {
            schema_version: 1,
            event_type: "activity.explore_brief_applied".into(),
            event_id: "explore-brief-1".into(),
            ts: Utc::now(),
            run_id: run.clone(),
            agent_identity: "claude".into(),
            parent_event_id: None,
            workspace_path: None,
        },
        kind: V2AuditEventKind::ExploreBriefApplied {
            step_id: "implement".into(),
            activity_name: "agent_implement".into(),
            task_id: task.clone(),
            brief: record,
        },
    };
    fx.runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: fx.runtime.workspace_id().unwrap(),
            event_id: event.envelope.event_id.clone(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: "explore_brief_applied".into(),
            ts: event.envelope.ts,
            run_id: run.clone(),
            agent_identity: "claude".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: serde_json::to_string(&event).unwrap(),
        })
        .unwrap();

    let listed = fx.runtime.collect_run_tag_instructions(&run).unwrap();
    assert_eq!(listed.state, "observed");
    assert_eq!(
        listed.header_lines(),
        ["Explore brief: implement (agent_implement): dropped: material_changed"]
    );
    let projected = listed.to_json();
    assert_eq!(
        projected["explore_briefs"][0]["outcome"],
        "dropped: material_changed"
    );
    assert_eq!(
        projected["explore_briefs"][0]["brief"]["reason"],
        "material_changed"
    );
}

fn a_quoted_diff_command_shows_shell_awkward_drift_targets() {
    let fx = Fixture::new(ENABLED);
    fx.commit("docs/Getting Started.md", "before\n");
    fx.commit("it's.md", "before\n");
    fx.commit("app/[slug]/page.tsx", "before\n");
    fx.commit("app/s/page.tsx", "sibling\n");
    let task = fx.task();
    let inspected = git(&fx.repo, &["rev-parse", "HEAD"]).trim().to_string();
    fx.pilot_covering_awkward_paths(&task);
    fx.start(&task);

    // One distinct added line per covered target; the `app/s` sibling stays
    // unchanged so shell or Git glob expansion would visibly select it.
    for (path, marker) in [
        ("docs/Getting Started.md", "space-after"),
        ("it's.md", "quote-after"),
        ("app/[slug]/page.tsx", "bracket-after"),
    ] {
        let file = fx.repo.join(path);
        let mut contents = std::fs::read_to_string(&file).unwrap();
        contents.push_str(marker);
        contents.push('\n');
        std::fs::write(file, contents).unwrap();
    }
    git(&fx.repo, &["add", "-A"]);
    git(&fx.repo, &["commit", "-q", "-m", "drift awkward targets"]);
    let base = git(&fx.repo, &["rev-parse", "HEAD"]).trim().to_string();

    let drifted = fx.admission(&task, &base);
    assert_eq!(drifted.record.label(), "drifted", "{:?}", drifted.record);
    let mut drifted_paths = drifted.record.drifted_paths.clone();
    drifted_paths.sort();
    assert_eq!(
        drifted_paths,
        ["app/[slug]/page.tsx", "docs/Getting Started.md", "it's.md",],
        "{:?}",
        drifted.record
    );
    let brief = drifted.brief.expect("a drifted brief is delivered");
    assert_eq!(brief["drift"]["range"], format!("{inspected}..{base}"));
    assert_eq!(
        brief["drift"]["files"]
            .as_array()
            .expect("drift files")
            .len(),
        3,
        "{brief}"
    );
    let command = brief["drift"]["diff_command"]
        .as_str()
        .expect("diff command is a string")
        .to_string();
    assert!(
        command.starts_with(&format!("git diff {inspected} {base} -- ")),
        "the command keeps the pinned range: {command}"
    );
    for path in ["docs/Getting Started.md", "it's.md", "app/[slug]/page.tsx"] {
        let quoted = orbit_common::process::shell::quote_posix_arg(&format!(":(literal){path}"));
        assert!(
            command.contains(&quoted),
            "the command carries {path} as one quoted literal pathspec: {command}"
        );
    }
    assert!(
        !command.contains("space-after"),
        "the patch itself is never inlined: {command}"
    );

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .current_dir(&fx.repo)
        .output()
        .expect("run the delivered diff command");
    assert!(
        output.status.success(),
        "the delivered command exits successfully: {command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("diff output is UTF-8");
    assert!(
        stdout.contains("+space-after"),
        "the space path is shown: {stdout}"
    );
    assert!(
        stdout.contains("+quote-after"),
        "the apostrophe path is shown: {stdout}"
    );
    assert!(
        stdout.contains("+bracket-after"),
        "the bracket path is shown: {stdout}"
    );
    assert!(
        !stdout.contains("app/s/page.tsx"),
        "shell expansion must not select the unchanged sibling: {stdout}"
    );
}

crate::scenario_table!(
    explore_brief,
    an_unchanged_task_receives_its_brief_until_a_material_edit,
    a_brief_kept_in_shadow_is_classified_but_not_delivered,
    the_run_record_names_each_dispatch_outcome,
    a_quoted_diff_command_shows_shell_awkward_drift_targets,
);
