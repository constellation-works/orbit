//! Executable catalog contracts: crew routing, policy, schema, and seeding.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_engine::activity_job::load_activity_asset;
use orbit_engine::{inject_system_crew_input, resolve_crew_settings};
use orbit_policy::PolicyEngine;
use orbit_tools::{ToolContext, ToolRegistry};
use orbit_types::policy::{FsProfile, PolicyDef};
use orbit_types::workflow::activity_job::OnDenial;
use orbit_types::workflow::{ActivityV2Spec, JobRunState};
use serde_json::json;
use tempfile::tempdir;

use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

use super::super::activity::seed_default_activities;

#[test]
fn shipped_agent_catalog_preserves_provider_and_model_routing() {
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::create_dir_all(&workspace).expect("create workspace root");
    std::fs::write(
        workspace.join("config.toml"),
        r#"[workflow]
default_crew = "sol"
system_crew = "system"

[crews.sol]
provider = "codex"
model = "gpt-6-sol"
backend = "cli"

[crews.system]
provider = "codex"
model = "gpt-6-luna"
backend = "cli"
"#,
    )
    .expect("write crew config");
    let runtime = crate::OrbitRuntime::from_roots(&global, &workspace)
        .expect("build catalog routing runtime");
    let run_input = json!({ "crew": "sol" });

    let expected = BTreeMap::from([
        ("agent_implement", ("codex", "gpt-6-sol".to_string())),
        // The exploration invocation names no crew either: an operator
        // chooses one per submission, and an omitted choice falls through
        // to the run's crew exactly like every other activity here.
        ("agent_invoke", ("codex", "gpt-6-sol".to_string())),
        // [ORB-11333] The reviewer names no crew literally either: the
        // gate injects the configured `operation.review_crew` per run.
        ("agent_review_repair", ("codex", "gpt-6-sol".to_string())),
        ("step_failure_recovery", ("codex", "gpt-6-luna".to_string())),
        ("pr_conflict_recovery", ("codex", "gpt-6-luna".to_string())),
        ("task_pilot", ("codex", "gpt-6-luna".to_string())),
    ]);
    let mut actual = BTreeMap::new();

    for (name, yaml) in DEFAULT_ACTIVITY_FILES {
        let asset = load_activity_asset(yaml)
            .unwrap_or_else(|error| panic!("load shipped activity {name}: {error}"));
        let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
            continue;
        };
        // [ORB-10877] Every system/utility activity resolves its crew from
        // `workflow.system_crew` at dispatch. No shipped activity names a
        // crew literally, so none of them depend on a family-specific
        // `[crews]` entry existing on the machine that runs it.
        let activity_input = match *name {
            "task_pilot" | "step_failure_recovery" | "pr_conflict_recovery" => {
                inject_system_crew_input(&runtime, &json!({ "system_crew": true }))
                    .expect("inject configured system crew")
            }
            _ => json!({}),
        };
        let resolved = resolve_crew_settings(&runtime, &spec, &activity_input, &run_input)
            .unwrap_or_else(|error| panic!("resolve shipped activity {name}: {error}"))
            .unwrap_or_else(|| panic!("shipped activity {name} did not resolve a crew"));
        actual.insert(
            *name,
            (
                resolved.provider.as_str(),
                resolved.model.expect("shipped crew has a model"),
            ),
        );
    }

    assert_eq!(actual, expected);
}

#[test]
fn seeded_deterministic_activities_match_actions() {
    let root = tempdir().expect("create tempdir");
    let activities_dir = root.path().join("resources/activities");
    seed_default_activities(&activities_dir, true).expect("seed default activities");

    for (name, action) in [
        ("git_commit", "git_commit"),
        ("git_rebase", "git_rebase"),
        ("pr_prepare", "pr_prepare"),
        ("pr_failure_handoff", "pr_failure_handoff"),
        ("pr_complete", "pr_complete"),
        ("pr_promote", "pr_promote"),
        ("task_complete", "task_complete"),
        ("release_locks", "release_locks"),
        ("scan_unresolved_work", "scan_unresolved_work"),
        ("worktree_gc", "worktree_gc"),
    ] {
        let yaml = std::fs::read_to_string(activities_dir.join(format!("{name}.yaml")))
            .unwrap_or_else(|error| panic!("read {name} activity: {error}"));
        let asset = load_activity_asset(&yaml)
            .unwrap_or_else(|error| panic!("parse {name} activity: {error}"));
        assert_eq!(asset.name, name);
        match asset.spec.spec {
            ActivityV2Spec::Deterministic(spec) => {
                assert_eq!(spec.action, action);
            }
            _ => panic!("expected deterministic activity"),
        }
    }
}

#[test]
fn agent_implement_seeds_a_deny_policy_with_proc_spawn() {
    let (_, yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "agent_implement")
        .expect("agent implement activity is seeded");
    let asset = load_activity_asset(yaml).expect("parse agent implement activity");
    let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("expected agent_loop activity");
    };
    assert!(spec.tools.is_empty());
    let deny = spec.tool_disallow_list.expect("shipped deny policy");
    let policy = orbit_types::workflow::ActivityToolDenyPolicy {
        activity: "agent_implement".to_string(),
        disallow_list: deny,
    };
    assert!(policy.denies("orbit.agent.invoke"));
    assert!(!policy.denies("proc.spawn"));
    assert!(spec.proc_disallowed_programs.is_some());
    assert!(spec.proc_allowed_programs.is_none());
}

#[test]
fn agent_implement_activity_loads_after_instruction_rewording() {
    let (_, yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "agent_implement")
        .expect("agent implement activity is seeded");
    let original = load_activity_asset(yaml).expect("load shipped activity");
    let mut reworded: serde_yaml::Value = serde_yaml::from_str(yaml).expect("parse activity yaml");
    reworded["spec"]["instruction"] =
        serde_yaml::Value::String("Complete the assigned task and report the result.".to_string());
    let reworded = serde_yaml::to_string(&reworded).expect("serialize reworded activity");
    let loaded = load_activity_asset(&reworded).expect("load activity with reworded instruction");

    assert_eq!(loaded.name, original.name);
    assert_eq!(
        loaded.spec.input_schema_json,
        original.spec.input_schema_json
    );
    assert_eq!(
        loaded.spec.output_schema_json,
        original.spec.output_schema_json
    );
    let ActivityV2Spec::AgentLoop(spec) = loaded.spec.spec else {
        panic!("expected agent_loop activity");
    };
    assert!(spec.require_completion_envelope);
    assert!(spec.tool_disallow_list.is_some());
}

#[test]
fn agent_response_contract_matches_durable_handoff_shape() {
    for (name, required) in [("agent_implement", false), ("task_pilot", true)] {
        let (_, yaml) = DEFAULT_ACTIVITY_FILES
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .unwrap_or_else(|| panic!("{name} activity is seeded"));
        let asset = load_activity_asset(yaml)
            .unwrap_or_else(|error| panic!("parse {name} activity: {error}"));
        match asset.spec.spec {
            ActivityV2Spec::AgentLoop(spec) => assert_eq!(
                spec.require_response_envelope, required,
                "{name} response contract drifted"
            ),
            _ => panic!("expected agent_loop activity"),
        }
    }
}

/// [ORB-10449] The step-completion protocol contract, asserted across every
/// shipped `agent_loop` activity so an exception has to be *declared*.
///
/// The flag defaults to `true`, so a new activity inherits the check by
/// omitting it; this test exists to make the opt-out list explicit and to
/// force a deliberate edit here when one is added. Every seeded agent-loop
/// activity does work whose absence must stop the pipeline.
#[test]
fn agent_step_completion_contract_is_required_except_where_declared() {
    const DECLARED_OPT_OUTS: &[&str] = &[];

    let mut checked = 0;
    for (name, yaml) in DEFAULT_ACTIVITY_FILES {
        let asset = load_activity_asset(yaml)
            .unwrap_or_else(|error| panic!("parse {name} activity: {error}"));
        let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
            continue;
        };
        checked += 1;
        let expected = !DECLARED_OPT_OUTS.contains(name);
        assert_eq!(
            spec.require_completion_envelope, expected,
            "{name} step-completion contract drifted; add it to DECLARED_OPT_OUTS \
             only if the activity not running at all is harmless"
        );
        // Opting into the content contract without the completion contract
        // is incoherent: an absent envelope fails content validation too,
        // so the pair would disagree about the same invocation.
        assert!(
            !spec.require_response_envelope || spec.require_completion_envelope,
            "{name} requires response content but not step completion"
        );
    }
    assert!(checked > 1, "expected several agent_loop activities");
}

#[test]
fn task_pilot_is_read_only_bounded_and_uses_advisory_output() {
    let (_, yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "task_pilot")
        .expect("task pilot activity is seeded");
    let asset = load_activity_asset(yaml).expect("parse task pilot activity");
    assert_eq!(
        asset.spec.fs_profile.as_deref(),
        Some("reviewer"),
        "read-only direct activities must not inherit unrestricted workspace writes"
    );
    assert!(
        asset.spec.output_schema_json.get("required").is_none(),
        "agent-returned task-pilot fields stay advisory until deterministic apply"
    );
    assert_eq!(
        asset.spec.input_schema_json["properties"]["task_ids"]["maxItems"],
        serde_json::json!(5)
    );
    match asset.spec.spec {
        ActivityV2Spec::AgentLoop(spec) => {
            assert!(spec.require_response_envelope);
            assert_eq!(spec.on_denial, OnDenial::Terminate);
            assert!(!spec.tools.iter().any(|tool| tool == "orbit.task.update"));
            assert!(!spec.tools.iter().any(|tool| tool == "orbit.task.*"));
            assert!(!spec.tools.iter().any(|tool| {
                matches!(
                    tool.as_str(),
                    "fs.write" | "fs.patch" | "orbit.pipeline.invoke"
                )
            }));
            assert!(spec.proc_allowed_programs.is_none());
            assert!(spec.proc_disallowed_programs.is_some());
        }
        _ => panic!("expected agent_loop task_pilot activity"),
    }
}

/// [ORB-11261] The task-pilot output schema's per-task field types must
/// mirror what `apply_task_pilot_results` actually enforces
/// (`string_array_value` in `adapter::engine_host::v2_host::task_pilot`),
/// so a producer sees the concrete string-array contract for
/// `adr_conflicts` and its siblings instead of an avoidable shape failure
/// discovered only after full exploration. Optionality (no `required` at
/// any level) must survive alongside the added types.
#[test]
fn task_pilot_output_schema_declares_advisory_field_types() {
    let (_, yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "task_pilot")
        .expect("task pilot activity is seeded");
    let asset = load_activity_asset(yaml).expect("parse task pilot activity");
    let schema = &asset.spec.output_schema_json;
    assert!(
        schema.get("required").is_none(),
        "top-level output schema must stay optional"
    );

    let task_item = &schema["properties"]["tasks"]["items"];
    assert_eq!(task_item["type"], serde_json::json!("object"));
    assert!(
        task_item.get("required").is_none(),
        "per-task output schema must stay optional until apply_task_pilot_results validates"
    );

    let properties = &task_item["properties"];
    for field in [
        "blocked_by",
        "adr_conflicts",
        "utility_warnings",
        "surface_warnings",
        "context_files_before",
        "context_files_after",
    ] {
        assert_eq!(
            properties[field]["type"],
            serde_json::json!("array"),
            "{field} must declare an array type consistent with the deterministic apply parser"
        );
        assert_eq!(
            properties[field]["items"]["type"],
            serde_json::json!("string"),
            "{field} items must declare a string type consistent with `string_array_value`"
        );
    }
    assert_eq!(
        properties["disposition"]["enum"],
        serde_json::json!(["selectors", "verified_no_diff", "host_operational"])
    );
    assert_eq!(
        properties["recommended_complexity"]["enum"],
        serde_json::json!(["low", "medium", "hard", "xhard", "unassessed"])
    );
    assert_eq!(
        properties["confidence"]["enum"],
        serde_json::json!(["high", "medium", "low"])
    );
    for field in ["evidence_gaps", "reassessment_triggers"] {
        assert_eq!(properties[field]["type"], serde_json::json!("array"));
        assert_eq!(
            properties[field]["items"]["type"],
            serde_json::json!("string")
        );
    }
}

/// [ORB-12275] The wait-envelope contract published to workflow authors
/// must spell terminal success as `JobRunState::Success` (`success`).
/// `succeeded` remains a compatibility token in
/// `pipeline_wait_status_is_success`, not an advertised enum value.
#[test]
fn invoke_and_wait_status_enum_matches_job_run_state_display() {
    use crate::application::job::pipeline::PIPELINE_WAIT_MAX_TIMEOUT_SECONDS;
    use crate::application::job::pipeline::pipeline_wait_status_is_success;

    let (_, wait_yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "invoke_and_wait")
        .expect("invoke_and_wait activity is seeded");
    let wait = load_activity_asset(wait_yaml).expect("parse invoke_and_wait");
    assert_eq!(
        wait.spec.input_schema_json["properties"]["timeout_seconds"]["maximum"],
        PIPELINE_WAIT_MAX_TIMEOUT_SECONDS
    );
    let statuses = wait.spec.output_schema_json["properties"]["status"]["enum"]
        .as_array()
        .expect("invoke_and_wait status enum");
    let statuses: Vec<&str> = statuses
        .iter()
        .map(|value| value.as_str().expect("status enum values are strings"))
        .collect();

    let success = JobRunState::Success.to_string();
    assert_eq!(success, "success");
    assert!(
        statuses.contains(&success.as_str()),
        "wait contract must advertise the JobRunState Display spelling, got {statuses:?}"
    );
    assert!(
        !statuses.contains(&"succeeded"),
        "succeeded is a compatibility token, not a published wait status"
    );
    assert!(pipeline_wait_status_is_success(&success));

    for status in &statuses {
        let parsed = status.parse::<JobRunState>().unwrap_or_else(|err| {
            panic!("declared wait status {status} is not a JobRunState rendering: {err}")
        });
        assert_eq!(
            parsed.to_string(),
            *status,
            "declared wait status {status} drifted from JobRunState Display"
        );
    }

    assert_eq!(
        wait.spec.output_schema_json["properties"]["duration_ms"]["type"],
        json!("integer")
    );
    assert!(
        !wait.spec.description.contains("succeeded"),
        "invoke_and_wait prose must not advertise status: succeeded"
    );
    assert!(wait.spec.description.contains("`success`"));

    let (_, guard_yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "pipeline_success_guard")
        .expect("pipeline_success_guard activity is seeded");
    let guard = load_activity_asset(guard_yaml).expect("parse pipeline_success_guard");
    assert!(
        guard.spec.description.contains("`status: success`"),
        "guard description must name the wait entry's success token"
    );
    assert!(
        !guard.spec.description.contains("`status: succeeded`"),
        "guard description must not keep the stale succeeded token"
    );
}

#[test]
fn seeded_activities_include_step_failure_recovery() {
    let root = tempdir().expect("create tempdir");
    let activities_dir = root.path().join("resources/activities");
    seed_default_activities(&activities_dir, true).expect("seed default activities");

    let yaml = std::fs::read_to_string(activities_dir.join("step_failure_recovery.yaml"))
        .expect("read step failure recovery activity");
    let asset = load_activity_asset(&yaml).expect("parse step failure recovery activity");
    assert_eq!(asset.name, "step_failure_recovery");
    assert_eq!(
        asset.spec.input_schema_json["required"],
        serde_json::json!([
            "failed_step_id",
            "activity_name",
            "error_message",
            "attempt",
            "max_attempts",
            "workspace_path",
            "repo_root",
            "run_id",
            "failed_step_input",
            "crew",
            "crew_config_key",
            "system_crew"
        ])
    );
    assert_eq!(
        asset.spec.input_schema_json["anyOf"],
        serde_json::json!([
            { "required": ["task_id"] },
            { "required": ["task_ids"] }
        ])
    );
    assert_eq!(
        asset.spec.input_schema_json["additionalProperties"],
        serde_json::json!(false)
    );
    match asset.spec.spec {
        ActivityV2Spec::AgentLoop(spec) => {
            assert!(spec.tools.is_empty());
            let policy = orbit_types::workflow::ActivityToolDenyPolicy {
                activity: asset.name,
                disallow_list: spec.tool_disallow_list.expect("shipped tool policy"),
            };
            assert!(policy.denies("orbit.workflow.ship"));
            assert!(!policy.denies("proc.spawn"));
            assert!(spec.proc_allowed_programs.is_none());
            assert!(spec.proc_disallowed_programs.is_some());
            assert_eq!(spec.on_denial, orbit_types::workflow::OnDenial::Terminate);
            assert!(!spec.instruction.is_empty());
        }
        _ => panic!("expected agent_loop activity"),
    }
}

/// [ORB-12103] Recovery once "repaired" a failed push by adding an `origin`
/// that pointed at the primary checkout a linked worktree shares its
/// `.git/config` with, turning a failed publication green. The boundary is
/// now both stated in the contract and enforced by the only subprocess
/// surface the activity has: `proc.spawn` refuses a `git` invocation that
/// would write persistent repository configuration.
#[test]
fn step_failure_recovery_cannot_write_persistent_git_configuration() {
    let (_, yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "step_failure_recovery")
        .expect("step failure recovery activity is seeded");
    let asset = load_activity_asset(yaml).expect("parse step failure recovery activity");
    let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("expected agent_loop activity");
    };
    for stated in [
        "Repository configuration is out of bounds.",
        "Manufacturing a step's precondition is not recovery",
        "report `recovered: false`",
    ] {
        assert!(
            spec.instruction.contains(stated),
            "[ORB-12103] recovery contract must state `{stated}`"
        );
    }
    let programs = spec.proc_disallowed_programs.clone().unwrap_or_default();
    assert!(
        !programs.iter().any(|program| program == "git"),
        "recovery still inspects and delivers with git"
    );

    let repo = tempdir().expect("create tempdir");
    let repo_path = repo.path().to_string_lossy().into_owned();
    run_git(repo.path(), &["init"]).expect("git init");
    let mut ctx = recovery_tool_context(repo.path(), Vec::new());
    ctx.proc_disallowed_programs = Some(programs);
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let denial = registry
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "git",
                "args": ["-C", repo_path, "remote", "add", "origin", repo_path],
            }),
        )
        .expect_err("recovery must not be able to write a git remote");
    assert!(
        matches!(&denial, OrbitError::PolicyDenied(message)
            if message.contains("persistent repository configuration")),
        "expected the configuration boundary to refuse the call, got: {denial}"
    );
    assert_eq!(
        run_git(repo.path(), &["remote"]).expect("list remotes"),
        "",
        "the refused call must leave repository configuration untouched"
    );

    // The refusal is specific to configuration writes, not to git itself:
    // the same context still reaches the boundary for inspection. (The child
    // can fail for host reasons — no Landlock, no git — so only the
    // configuration refusal itself is asserted against.)
    if let Err(error) = registry.execute(
        "proc.spawn",
        &ctx,
        json!({ "program": "git", "args": ["remote", "-v"] }),
    ) {
        assert!(
            !error
                .to_string()
                .contains("persistent repository configuration"),
            "read-only git inspection must not hit the configuration boundary: {error}"
        );
    }
}

/// The tool context `step_failure_recovery` runs with: activity-scoped, its
/// own program allowlist, and workspace-wide filesystem access.
fn recovery_tool_context(workspace_root: &Path, programs: Vec<String>) -> ToolContext {
    let mut fs_profiles = HashMap::new();
    fs_profiles.insert(
        "implementer".to_string(),
        FsProfile {
            read: vec!["./**".to_string()],
            modify: vec!["./**".to_string()],
        },
    );
    let policy = PolicyDef {
        name: "test".to_string(),
        description: None,
        deny_read: Vec::new(),
        deny_modify: Vec::new(),
        fs_profiles,
        created_at: None,
        updated_at: None,
    };
    ToolContext {
        workspace_root: Some(workspace_root.to_path_buf()),
        policy_engine: Some(Arc::new(
            PolicyEngine::from_def(&policy).expect("policy engine"),
        )),
        fs_profile: Some("implementer".to_string()),
        proc_allowed_programs: programs,
        proc_spawn_activity_scoped: true,
        ..Default::default()
    }
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String, OrbitError> {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|error| OrbitError::Execution(format!("git {args:?}: {error}")))?;
    if !output.status.success() {
        return Err(OrbitError::Execution(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
