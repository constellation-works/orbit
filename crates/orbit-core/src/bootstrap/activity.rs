use std::borrow::Cow;
use std::path::Path;

use orbit_common::OrbitError;

use crate::application::{
    ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets,
};
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

/// Seed every entry in [`DEFAULT_ACTIVITY_FILES`] as a YAML file under
/// `activities_dir`. Mirrors the skill / executor / policy seeding pattern:
/// the asset YAML is embedded in the binary via `include_str!` and copied
/// out on `orbit init` so the [`V2ActivityCatalog`] can discover it without
/// depending on a git checkout of this repo.
///
/// When `overwrite` is false, existing files are preserved — users who've
/// edited a previously-seeded activity won't lose their changes on re-init.
pub(crate) fn seed_default_activities(
    activities_dir: &Path,
    overwrite: bool,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    reconcile_managed_assets(
        activities_dir,
        "activity",
        ManagedAssetLayout::YamlStem,
        DEFAULT_ACTIVITY_FILES,
        overwrite,
        |_, content| Ok(Cow::Borrowed(content)),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use orbit_engine::activity_job::load_activity_asset;
    use orbit_engine::{inject_system_crew_input, resolve_crew_settings};
    use orbit_types::workflow::ActivityV2Spec;
    use orbit_types::workflow::activity_job::{OnDenial, tool_allowed};
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

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
model = "gpt-5.6-sol"
backend = "cli"

[crews.system]
provider = "codex"
model = "gpt-5.6-luna"
backend = "cli"
"#,
        )
        .expect("write crew config");
        let runtime = crate::OrbitRuntime::from_roots(&global, &workspace)
            .expect("build catalog routing runtime");
        let run_input = json!({ "crew": "sol" });

        let expected = BTreeMap::from([
            ("agent_implement", ("codex", "gpt-5.6-sol".to_string())),
            // The exploration invocation names no crew either: an operator
            // chooses one per submission, and an omitted choice falls through
            // to the run's crew exactly like every other activity here.
            ("agent_invoke", ("codex", "gpt-5.6-sol".to_string())),
            // [ORB-11333] The reviewer names no crew literally either: the
            // gate injects the configured `operation.review_crew` per run.
            ("agent_review_repair", ("codex", "gpt-5.6-sol".to_string())),
            ("epic_orchestrator", ("codex", "gpt-5.6-sol".to_string())),
            (
                "step_failure_recovery",
                ("codex", "gpt-5.6-luna".to_string()),
            ),
            (
                "pr_conflict_recovery",
                ("codex", "gpt-5.6-luna".to_string()),
            ),
            ("task_pilot", ("codex", "gpt-5.6-luna".to_string())),
            ("triage_failed_runs", ("codex", "gpt-5.6-luna".to_string())),
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
                "task_pilot"
                | "step_failure_recovery"
                | "pr_conflict_recovery"
                | "triage_failed_runs" => {
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
            ("list_triage_candidates", "list_triage_candidates"),
            ("scan_unresolved_work", "scan_unresolved_work"),
            ("apply_triage_dispositions", "apply_triage_dispositions"),
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
    fn agent_implement_guidance_allows_bounded_scope_expansion() {
        let (_, yaml) = DEFAULT_ACTIVITY_FILES
            .iter()
            .find(|(name, _)| *name == "agent_implement")
            .expect("agent implement activity is seeded");
        assert_eq!(
            *yaml,
            include_str!("../../../../.orbit/resources/activities/agent_implement.yaml"),
            "shipped and workspace implementation activities must remain byte-identical"
        );
        let asset = load_activity_asset(yaml).expect("parse agent implement activity");
        match asset.spec.spec {
            ActivityV2Spec::AgentLoop(spec) => {
                let instruction = spec
                    .instruction
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                assert!(!yaml.contains("\n  role:"));
                assert!(instruction.contains("not as a perfect inventory"));
                assert!(instruction.contains("make the smallest compatible change"));
                assert!(instruction.contains("stop after a task comment"));
                assert!(instruction.contains("before the first edit"));
                assert!(instruction.contains("before validation"));
                assert!(instruction.contains("git rev-parse --show-toplevel"));
                assert!(instruction.contains("worktree_mismatch"));
                for contract in [
                    "task.terminal",
                    "pwd -p",
                    "context_files",
                    "eperm",
                    "orbit.friction.add",
                    "orbit.task.start",
                    "move the task to `review`",
                    "execution_summary",
                ] {
                    assert!(
                        instruction.contains(contract),
                        "implementation contract disappeared: {contract}"
                    );
                }
            }
            _ => panic!("expected agent_loop activity"),
        }
    }

    #[test]
    fn agent_implement_tools_remain_the_implementation_baseline() {
        let (_, yaml) = DEFAULT_ACTIVITY_FILES
            .iter()
            .find(|(name, _)| *name == "agent_implement")
            .expect("agent implement activity is seeded");
        let asset = load_activity_asset(yaml).expect("parse agent implement activity");
        let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
            panic!("expected agent_loop activity");
        };
        assert_eq!(
            spec.tools,
            [
                "orbit.task.*",
                "orbit.friction.*",
                "orbit.search",
                "proc.spawn"
            ]
        );
        assert!(
            spec.tools
                .iter()
                .all(|tool| !tool.starts_with("github.") && !tool.contains("ceiling")),
            "agent_implement must not widen to GitHub reads or grow a task ceiling: {:?}",
            spec.tools
        );
    }

    #[test]
    fn agent_implement_context_loading_reads_files_and_lists_directories() {
        let (_, yaml) = DEFAULT_ACTIVITY_FILES
            .iter()
            .find(|(name, _)| *name == "agent_implement")
            .expect("agent implement activity is seeded");
        let asset = load_activity_asset(yaml).expect("parse agent implement activity");
        let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
            panic!("expected agent_loop activity");
        };
        let instruction = spec
            .instruction
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();

        assert!(
            instruction.contains("each `file:` target with the provider-native file-read tool")
        );
        assert!(instruction.contains("each `dir:` selector"));
        assert!(instruction.contains("do not call the file-read tool on the directory"));
        assert!(instruction.contains("resolves beneath the workspace root"));
        assert!(instruction.contains("`rg --files <directory>`"));
        assert!(!instruction.contains("is a directory"));
    }

    #[test]
    fn agent_response_contract_matches_durable_handoff_shape() {
        for (name, required) in [("agent_implement", false), ("triage_failed_runs", true)] {
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
                other => panic!("expected agent_loop {name} activity, got {other:?}"),
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
        let workspace_yaml =
            include_str!("../../../../.orbit/resources/activities/task_pilot.yaml");
        assert_eq!(
            *yaml, workspace_yaml,
            "shipped and workspace task-pilot resources must remain byte-identical"
        );
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
                assert!(
                    spec.proc_allowed_programs
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .all(|program| matches!(program.as_str(), "git" | "rg"))
                );
                assert!(
                    !spec
                        .proc_allowed_programs
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|program| program == "orbit"),
                    "proc.spawn must not bypass the scoped Orbit tool allowlist"
                );
                assert!(spec.instruction.contains("Never update a task"));
                assert!(spec.instruction.contains("one to five"));
            }
            other => panic!("expected agent_loop task_pilot activity, got {other:?}"),
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
            serde_json::json!(["low", "medium", "hard"])
        );
    }

    /// [ORB-10129] The triage agent's hard bounds are structural: its tool
    /// allowlist must exclude every write/dispatch surface (code edits,
    /// commits/pushes/merges, PR approval, pipeline invocation, task
    /// lifecycle writes), `proc_allowed_programs` must not include `git`,
    /// and `on_denial: terminate` must be set so a denied tool kills the
    /// run (termination semantics proven by
    /// `replay_denial_terminate_surfaces_structural_tool_denied` in
    /// orbit-engine).
    #[test]
    fn triage_agent_allowlist_makes_write_surfaces_structurally_impossible() {
        let (_, yaml) = DEFAULT_ACTIVITY_FILES
            .iter()
            .find(|(name, _)| *name == "triage_failed_runs")
            .expect("triage agent activity is seeded");
        let workspace_yaml =
            include_str!("../../../../.orbit/resources/activities/triage_failed_runs.yaml");
        assert_eq!(
            *yaml, workspace_yaml,
            "shipped and workspace triage resources must remain byte-identical"
        );
        let asset = load_activity_asset(yaml).expect("parse triage agent activity");
        assert_eq!(
            asset.spec.fs_profile.as_deref(),
            Some("reviewer"),
            "direct triage must not inherit unrestricted workspace writes"
        );
        match asset.spec.spec {
            ActivityV2Spec::AgentLoop(spec) => {
                assert!(!yaml.contains("\n  role:"));
                assert_eq!(spec.on_denial, OnDenial::Terminate);
                for denied in [
                    // code edits
                    "fs.write",
                    "fs.patch",
                    "fs.create",
                    "fs.move",
                    "fs.copy",
                    // pipeline / job dispatch
                    "orbit.pipeline.invoke",
                    "orbit.pipeline.wait",
                    // task lifecycle writes other than the activity's one
                    // evidence-gated blocked -> done reconciliation
                    "orbit.task.start",
                    "orbit.task.approve",
                    "orbit.task.reject",
                    "orbit.task.add",
                    "orbit.task.delete",
                    "orbit.task.locks.reserve",
                    "orbit.task.locks.release",
                ] {
                    assert!(
                        !tool_allowed(denied, &spec.tools),
                        "triage agent must not be able to call `{denied}`"
                    );
                }
                for allowed in [
                    "orbit.task.show",
                    "orbit.task.update",
                    "orbit.friction.add",
                    "proc.spawn",
                ] {
                    assert!(
                        tool_allowed(allowed, &spec.tools),
                        "triage agent should be able to call `{allowed}`"
                    );
                }
                // No `git` subprocess: commits/pushes/merges stay impossible
                // even through proc.spawn.
                assert_eq!(
                    spec.proc_allowed_programs.as_deref(),
                    Some(&["rg".to_string()][..])
                );
            }
            other => panic!("expected agent_loop activity, got {other:?}"),
        }
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
                "max_attempts"
            ])
        );
        assert_eq!(
            asset.spec.input_schema_json["additionalProperties"],
            serde_json::json!(false)
        );
        match asset.spec.spec {
            ActivityV2Spec::AgentLoop(spec) => {
                assert!(!yaml.contains("\n  role:"));
                assert!(!yaml.contains("\n  backend:"));
                assert!(!yaml.contains("\n  provider:"));
                assert!(
                    spec.instruction
                        .contains("You are Orbit's step-failure recovery agent.")
                );
            }
            other => panic!("expected agent_loop activity, got {other:?}"),
        }
    }

    #[test]
    fn epic_orchestrator_is_a_worktree_finisher_not_a_dispatcher() {
        let (_, yaml) = DEFAULT_ACTIVITY_FILES
            .iter()
            .find(|(name, _)| *name == "epic_orchestrator")
            .expect("epic orchestrator activity is seeded");
        assert_eq!(
            *yaml,
            include_str!("../../../../.orbit/resources/activities/epic_orchestrator.yaml"),
            "shipped and workspace epic_orchestrator activities must remain byte-identical"
        );
        let lowered = yaml.to_ascii_lowercase();
        for stale in [
            "no-code-change",
            "no code change",
            "must not edit the repository",
            "do not edit repository files",
            "no git write",
            "no worktree edit",
            "no `agent_implement`",
        ] {
            assert!(
                !lowered.contains(stale),
                "stale dispatcher language remains: {stale}"
            );
        }
        let asset = load_activity_asset(yaml).expect("parse epic orchestrator");
        assert_eq!(asset.spec.fs_profile.as_deref(), None);
        match asset.spec.spec {
            ActivityV2Spec::AgentLoop(spec) => {
                assert_eq!(spec.wall_clock_timeout_seconds, 10800);
                assert_eq!(spec.on_denial, OnDenial::Terminate);
                assert!(tool_allowed("orbit.task.add", &spec.tools));
                assert!(tool_allowed("orbit.task.update", &spec.tools));
                assert!(!tool_allowed("orbit.session_log.append", &spec.tools));
                assert!(tool_allowed("orbit.search", &spec.tools));
                assert!(tool_allowed("proc.spawn", &spec.tools));
                for denied in [
                    "orbit.workflow.ship",
                    "orbit.workflow.run.resume",
                    "orbit.pipeline.invoke",
                    "orbit.pipeline.wait",
                    "fs.write",
                    "fs.patch",
                ] {
                    assert!(
                        !tool_allowed(denied, &spec.tools),
                        "finisher must not keep dispatcher or raw-write surfaces: {denied}"
                    );
                }
                let instruction = spec
                    .instruction
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                assert!(instruction.contains("epic worktree finisher"));
                assert!(instruction.contains("git rev-parse --show-toplevel"));
                assert!(instruction.contains("worktree_mismatch"));
                assert!(instruction.contains("pwd -p"));
                assert!(instruction.contains("human merge authority"));
                assert!(instruction.contains("execution_summary"));
                assert!(instruction.contains("move the epic to `review`"));
                assert!(!instruction.contains("session resume"));
                assert!(!instruction.contains("orbit.session_log"));
                assert!(!instruction.contains("shrink the scan set"));
                assert_eq!(
                    spec.proc_allowed_programs.as_deref(),
                    Some(
                        &[
                            "git".to_string(),
                            "make".to_string(),
                            "cargo".to_string(),
                            "rg".to_string(),
                            "orbit".to_string(),
                        ][..]
                    )
                );
            }
            other => panic!("expected agent_loop epic_orchestrator, got {other:?}"),
        }
    }
}
