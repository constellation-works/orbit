//! Review-filed bug findings are verified before promotion: the verification
//! job's deterministic steps record a CONFIRMED or REJECTED verdict from stub
//! verifier results, a doubtful verdict is judged again by the next tier's
//! complexity pool, promotion and backlog admission refuse an unverified
//! finding, and the state trigger picks such findings up. Pool names are
//! fixture policy. Fixtures run in isolated children with real stores and a
//! real repository.

use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use orbit_core::application::automation::evaluate_routine;
use orbit_core::application::task::{
    FindingVerdict, FindingVerification, TaskAddParams, TaskUpdateParams,
};
use orbit_core::{OrbitRuntime, Task, TaskComplexity, TaskStatus, TaskType};
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, RuntimeHost, V2AuditWriter, V2SqliteSink, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_tools::ToolContext;
use orbit_types::workflow::RoutineDefinition;
use orbit_types::workflow::activity_job::{ActivityV2Spec, DeterministicSpec};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const JOB: &str = "finding_verification_pipeline";

#[test]
fn shipped_activities_have_unambiguous_tool_policies() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities");
    for entry in std::fs::read_dir(assets).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "yaml")
        {
            let asset = load_activity_asset(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert!(
                orbit_types::workflow::activity_tool_policy_deprecation(&asset.spec).is_none(),
                "{} must load without a tool policy deprecation warning",
                asset.name
            );
        }
    }
}

fn verifier_resolved_tools_preserve_its_read_only_boundary() {
    let fixture = Fixture::new();
    let finding = fixture.finding_requiring_tools(
        "Required tools stay outside verifier grant",
        &[
            "orbit.task.add",
            "orbit.task.artifact.put",
            "orbit.task.show",
        ],
    );
    let task_ids = vec![finding.id.to_string()];
    let asset = load_activity_asset(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities/verify_finding.yaml"),
        )
        .unwrap(),
    )
    .unwrap();
    let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("finding verification must run an agent");
    };
    let denied = spec.tool_disallow_list.unwrap();
    let resolved = fixture
        .runtime
        .resolve_activity_tool_denials(&task_ids, &asset.name, &denied)
        .unwrap();
    assert!(
        resolved
            .effective_tools
            .iter()
            .any(|tool| tool == "orbit.task.show")
    );
    for tool in [
        "orbit.agent.invoke",
        "orbit.auto_task.add",
        "orbit.auto_task.mint",
        "orbit.auto_task.update",
        "orbit.command.exec",
        "orbit.pipeline.invoke",
        "orbit.workflow.run.resume",
        "orbit.workflow.ship",
        "orbit.task.add",
        "orbit.task.update",
        "orbit.task.artifact.put",
        "orbit.task.reconcile_review",
        "orbit.friction.add",
        "orbit.friction.update",
    ] {
        assert!(
            !resolved
                .effective_tools
                .iter()
                .any(|allowed| allowed == tool),
            "finding verification must withhold mutation and dispatch tool {tool}"
        );
    }
    for tool in ["orbit.task.add", "orbit.task.artifact.put"] {
        assert!(
            !resolved
                .requested_tools
                .iter()
                .any(|requested| requested == tool),
            "the verifier omits covered requirement {tool} from requested tools"
        );
        assert!(
            !resolved
                .effective_tools
                .iter()
                .any(|effective| effective == tool),
            "the verifier never receives covered requirement {tool}"
        );
        assert!(
            resolved.omitted_requirement_notes.iter().any(|note| {
                note.contains(&finding.id.to_string())
                    && note.contains(tool)
                    && note.contains("verify_finding")
            }),
            "the verifier records why required tool {tool} was omitted: {:?}",
            resolved.omitted_requirement_notes
        );
    }
    assert!(
        resolved
            .requested_tools
            .iter()
            .any(|tool| tool == "orbit.task.show"),
        "permitted required tools remain requested"
    );
    assert!(
        resolved
            .effective_tools
            .iter()
            .any(|tool| tool == "orbit.task.show"),
        "permitted required tools remain available"
    );

    let implementing_denial = vec!["orbit.task.add".to_string()];
    let refused = fixture
        .runtime
        .resolve_activity_tool_denials(&task_ids, "agent_implement", &implementing_denial)
        .expect_err("implementing activities remain fail-closed for covered requirements");
    assert!(
        matches!(
            &refused,
            DispatchError::RequiredToolAdmission { task_id, tool_name, .. }
                if task_id.as_str() == finding.id.as_str() && tool_name == "orbit.task.add"
        ),
        "a covering implementer deny list still refuses its required tool: {refused}"
    );
}

crate::scenario_table!(
    verifier_tool_policy_scenarios,
    verifier_resolved_tools_preserve_its_read_only_boundary
);

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
}

/// The fixture's `config.toml` before its `[review]` table.
const FIXTURE_CONFIG: &str = r#"
[workflow]
default_crew = "medium_codex"
low_complexity_crews = ["low_lane"]
medium_complexity_crews = ["medium_codex", "medium_claude"]
hard_complexity_crews = ["hard_lane"]
[crews.low_lane]
provider = "codex"
model = "fixture-low"
[crews.medium_codex]
provider = "codex"
model = "fixture-medium-codex"
[crews.medium_claude]
provider = "claude"
model = "fixture-medium-claude"
[crews.hard_lane]
provider = "claude"
model = "fixture-hard"
"#;

impl Fixture {
    /// A workspace that opted into the verification gate.
    fn new() -> Self {
        Self::with_review("[review]\nverify_findings = true\n")
    }

    /// A workspace whose config ends with `review`, a `[review]` table or
    /// nothing.
    fn with_review(review: &str) -> Self {
        Self::with_config(&format!("{FIXTURE_CONFIG}{review}"))
    }

    /// A workspace whose whole `config.toml` is `config`.
    fn with_config(config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(repo.join(".orbit/config.toml"), config).unwrap();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Orbit Test"],
            &["config", "user.email", "orbit-test@example.com"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(&repo, args);
        }
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("src/lib.rs"),
            (1..=10)
                .map(|line| format!("// line {line}\n"))
                .collect::<String>(),
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "seed"]);
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm())
            .with_automation_machine_identity(Some("fixture-machine".into()));
        Self {
            _root: root,
            runtime,
            repo,
        }
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// A task a review would file: a proposed `bug` tagged `code-review`.
    fn finding(&self, title: &str, complexity: TaskComplexity) -> Task {
        self.task(title, complexity, TaskType::Bug, &["code-review"], None)
    }

    fn finding_requiring_tools(&self, title: &str, tools: &[&str]) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("{title} at src/lib.rs."),
                acceptance_criteria: vec!["The reported defect no longer occurs.".into()],
                tags: vec!["code-review".into()],
                complexity: TaskComplexity::Low,
                task_type: Some(TaskType::Bug),
                required_tools: tools.iter().map(|tool| (*tool).to_string()).collect(),
                ..Default::default()
            })
            .unwrap()
    }

    fn task(
        &self,
        title: &str,
        complexity: TaskComplexity,
        task_type: TaskType,
        tags: &[&str],
        status: Option<TaskStatus>,
    ) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("{title} at src/lib.rs."),
                acceptance_criteria: vec!["The reported defect no longer occurs.".into()],
                tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
                complexity,
                task_type: Some(task_type),
                status: Some(status.unwrap_or(TaskStatus::Proposed)),
                ..Default::default()
            })
            .unwrap()
    }

    fn action(&self, action: &str, input: Value) -> Value {
        self.runtime
            .run_deterministic(action, &json!({}), &input, ToolContext::default())
            .unwrap_or_else(|error| panic!("{action}: {error}"))
    }

    fn prepare(&self, task_ids: &[&str]) -> Value {
        self.action(
            "prepare_finding_verification",
            json!({
                "task_ids": task_ids, "workspace_path": self.repo, "base_branch": "main",
                "run_id": "fixture-run",
            }),
        )
    }

    /// Apply `verdicts` as the stub verifier of each prepared partition
    /// returned them: a partition's result holds the verdicts of its tasks.
    fn apply(&self, prepared: &Value, verdicts: &[Value], carried: Option<&Value>) -> Value {
        let results = prepared["partitions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|partition| {
                let tasks = verdicts
                    .iter()
                    .filter(|verdict| {
                        partition["task_ids"]
                            .as_array()
                            .unwrap()
                            .contains(&verdict["task_id"])
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                json!({"partition_index": partition["partition_index"], "tasks": tasks})
            })
            .collect::<Vec<_>>();
        let mut input = json!({
            "prepared": prepared, "results": results, "workspace_path": self.repo,
            "run_id": "fixture-run",
        });
        if let Some(carried) = carried {
            input["carried_task_outcomes"] = carried.clone();
        }
        self.action("apply_finding_verification", input)
    }

    /// One task's prepared verifier crew.
    fn prepared_crew(prepared: &Value, task: &Task) -> String {
        Self::prepared_entry(prepared, task)["crew"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// One task's prepared record.
    fn prepared_entry<'a>(prepared: &'a Value, task: &Task) -> &'a Value {
        prepared["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["task_id"] == task.id.as_str())
            .unwrap()
    }

    fn verification(&self, task: &Task) -> Option<FindingVerification> {
        self.runtime.finding_verification(&task.id).unwrap()
    }

    fn status(&self, task: &Task) -> TaskStatus {
        self.runtime.get_task(&task.id).unwrap().status
    }

    /// Record a CONFIRMED verdict for `task` through prepare and apply.
    fn confirm(&self, task: &Task) {
        let prepared = self.prepare(&[&task.id]);
        let applied = self.apply(&prepared, &[confirmed(task, "high")], None);
        assert_eq!(applied["recorded_count"], 1, "{applied}");
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let mut command = crate::git_test_support::command();
    let output = command.args(args).current_dir(repo).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn confirmed(task: &Task, confidence: &str) -> Value {
    json!({
        "task_id": task.id, "verdict": "CONFIRMED", "confidence": confidence,
        "severity": "high",
        "evidence": [{"location": "src/lib.rs:3", "note": "the defective branch"}],
        "reasoning": "The branch at line 3 is reached with the reported input.",
        "reproduction": "Call it with an empty input.",
    })
}

fn rejected(task: &Task, confidence: &str) -> Value {
    json!({
        "task_id": task.id, "verdict": "REJECTED", "confidence": confidence,
        "evidence": [{"location": "src/lib.rs:7", "note": "the guard the review missed"}],
        "reasoning": "Line 7 guards the path the review traced.",
        "rejection": {"reason": "premise_wrong", "detail": "The input is checked first."},
    })
}

fn outcome<'a>(applied: &'a Value, task: &Task) -> &'a Value {
    applied["task_outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outcome| outcome["task_id"] == task.id.as_str())
        .unwrap_or_else(|| panic!("no outcome for {}: {applied}", task.id))
}

/// The verdict comment `orbit task show` displays for `task`: its first line
/// names the verdict and the rest is the recorded verdict document.
fn shown_verdict(fixture: &Fixture, task: &Task) -> (String, Value) {
    let shown = fixture
        .runtime
        .run_tool("orbit.task.show", json!({"id": task.id}))
        .unwrap();
    let comment = shown["comments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|comment| comment["by"] == "finding-verification")
        .and_then(|comment| comment["message"].as_str())
        .unwrap_or_else(|| panic!("task show carries no verdict comment: {shown}"));
    let (header, document) = comment.split_once('\n').unwrap();
    (header.to_string(), serde_json::from_str(document).unwrap())
}

fn verdicts_are_recorded_with_sha_and_evidence_and_rejection_closes_the_task() {
    let fixture = Fixture::new();
    let real = fixture.finding("Real defect", TaskComplexity::Medium);
    let wrong = fixture.finding("Wrong premise", TaskComplexity::Medium);
    let prepared = fixture.prepare(&[&real.id, &wrong.id]);
    assert_eq!(prepared["task_count"], 2, "{prepared}");
    assert_eq!(prepared["source"]["source_revision"], fixture.head());

    let applied = fixture.apply(
        &prepared,
        &[confirmed(&real, "high"), rejected(&wrong, "high")],
        None,
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["recorded_count"], 2, "{applied}");
    assert_eq!(applied["escalation_count"], 0, "{applied}");

    let record = fixture.verification(&real).unwrap();
    assert_eq!(record.verdict, FindingVerdict::Confirmed);
    assert_eq!(record.verified_sha, fixture.head());
    assert_eq!(record.evidence[0].location, "src/lib.rs:3");
    assert_eq!(record.crews.len(), 1);
    assert_eq!(
        record.crews[0].crew,
        Fixture::prepared_crew(&prepared, &real)
    );
    assert!(!record.escalated);
    assert_eq!(fixture.status(&real), TaskStatus::Proposed);
    let (header, document) = shown_verdict(&fixture, &real);
    assert_eq!(header, "finding-verification: confirmed");
    assert_eq!(document["verdict"], "CONFIRMED");
    assert_eq!(document["verified_sha"], fixture.head());

    assert_eq!(fixture.status(&wrong), TaskStatus::Rejected);
    let (header, document) = shown_verdict(&fixture, &wrong);
    assert_eq!(header, "finding-verification: rejected");
    assert_eq!(document["rejection"]["reason"], "premise_wrong");
    assert_eq!(document["evidence"][0]["location"], "src/lib.rs:7");

    // A confirmed finding is promoted as any other task.
    fixture
        .runtime
        .approve_task(&real.id, None, None)
        .expect("a confirmed finding is approved");
    assert_eq!(fixture.status(&real), TaskStatus::Backlog);
}

fn a_verdict_citing_lines_absent_at_the_pinned_revision_is_not_recorded() {
    let fixture = Fixture::new();
    let past_end = fixture.finding("Past the end", TaskComplexity::Medium);
    let missing = fixture.finding("Missing file", TaskComplexity::Medium);
    let silent = fixture.finding("No verdict", TaskComplexity::Medium);
    let prepared = fixture.prepare(&[&past_end.id, &missing.id, &silent.id]);
    let mut beyond = confirmed(&past_end, "high");
    beyond["evidence"][0]["location"] = json!("src/lib.rs:11");
    let mut absent = confirmed(&missing, "high");
    absent["evidence"][0]["location"] = json!("src/missing.rs:1");

    let applied = fixture.apply(&prepared, &[beyond, absent], None);
    assert_eq!(applied["status"], "failed", "{applied}");
    assert_eq!(applied["recorded_count"], 0, "{applied}");
    assert_eq!(outcome(&applied, &past_end)["reason"], "invalid_evidence");
    assert_eq!(outcome(&applied, &missing)["reason"], "invalid_evidence");
    assert_eq!(outcome(&applied, &silent)["reason"], "missing_verdict");
    for task in [&past_end, &missing, &silent] {
        assert!(fixture.verification(task).is_none());
        assert_eq!(fixture.status(task), TaskStatus::Proposed);
    }
}

fn a_doubtful_verdict_is_reverified_by_the_next_tier_and_names_every_crew() {
    let fixture = Fixture::new();
    // A low-tier REJECTED verdict and a medium-tier low-confidence one both
    // go to the next tier before anything is recorded.
    let low = fixture.finding("Low tier rejection", TaskComplexity::Low);
    let unsure = fixture.finding("Unsure medium verdict", TaskComplexity::Medium);
    let settled = fixture.finding("Settled medium verdict", TaskComplexity::Medium);
    let prepared = fixture.prepare(&[&low.id, &unsure.id, &settled.id]);
    assert_eq!(Fixture::prepared_crew(&prepared, &low), "low_lane");
    let unsure_first = Fixture::prepared_crew(&prepared, &unsure);

    let first = fixture.apply(
        &prepared,
        &[
            rejected(&low, "high"),
            confirmed(&unsure, "low"),
            confirmed(&settled, "high"),
        ],
        None,
    );
    assert_eq!(first["status"], "succeeded", "{first}");
    assert_eq!(first["recorded_count"], 1, "{first}");
    assert_eq!(fixture.status(&low), TaskStatus::Proposed);
    assert!(fixture.verification(&low).is_none());
    assert!(fixture.verification(&unsure).is_none());
    let escalation = &first["escalation_prepared"];
    assert_eq!(escalation["round"], "escalation");
    let low_second = Fixture::prepared_crew(escalation, &low);
    assert!(
        ["medium_codex", "medium_claude"].contains(&low_second.as_str()),
        "a low-tier task escalates to the medium pool: {escalation}"
    );
    assert_eq!(Fixture::prepared_crew(escalation, &unsure), "hard_lane");

    let second = fixture.apply(
        escalation,
        &[confirmed(&low, "high"), rejected(&unsure, "medium")],
        Some(&first["task_outcomes"]),
    );
    assert_eq!(second["status"], "succeeded", "{second}");
    assert_eq!(second["escalation_count"], 0, "one escalation round");
    assert_eq!(outcome(&second, &settled)["outcome"], "verified");

    let record = fixture.verification(&low).unwrap();
    assert_eq!(record.verdict, FindingVerdict::Confirmed);
    assert!(record.escalated);
    let crews = record
        .crews
        .iter()
        .map(|verdict| (verdict.crew.as_str(), verdict.tier, verdict.verdict))
        .collect::<Vec<_>>();
    assert_eq!(
        crews,
        [
            ("low_lane", TaskComplexity::Low, FindingVerdict::Rejected),
            (
                low_second.as_str(),
                TaskComplexity::Medium,
                FindingVerdict::Confirmed
            ),
        ]
    );
    assert_eq!(fixture.status(&low), TaskStatus::Proposed);

    let record = fixture.verification(&unsure).unwrap();
    assert_eq!(record.verdict, FindingVerdict::Rejected);
    assert_eq!(
        record
            .crews
            .iter()
            .map(|verdict| verdict.crew.as_str())
            .collect::<Vec<_>>(),
        [unsure_first.as_str(), "hard_lane"]
    );
    assert_eq!(fixture.status(&unsure), TaskStatus::Rejected);
}

/// The initial and escalation checkpoints must explain each task's own
/// pressure-weighted draw, including when several tasks share a partition.
fn verification_partitions_record_pressure_in_both_rounds() {
    let config = FIXTURE_CONFIG
        .replace(
            "[workflow]",
            "[workflow]\ncrew_selection = \"usage_pressure\"",
        )
        .replace(
            "[\"medium_codex\", \"medium_claude\"]",
            "[\"medium_codex:3\", \"medium_claude:1\", \"low_lane:0\"]",
        );
    let fixture = Fixture::with_config(&format!("{config}\n[review]\nverify_findings = true\n"));
    crate::usage_pressure_draws::record_week(&fixture.runtime, "claude", 10.0, Duration::hours(6));
    crate::usage_pressure_draws::record_week(&fixture.runtime, "codex", 0.0, Duration::days(7));
    let medium = fixture.finding("Weighted initial verifier", TaskComplexity::Medium);
    let low = fixture.finding("Weighted escalation verifier", TaskComplexity::Low);
    let prepared = fixture.prepare(&[&medium.id, &low.id]);
    let assert_draw = |prepared: &Value, task: &Task| {
        let entry = Fixture::prepared_entry(prepared, task);
        crate::usage_pressure_draws::assert_pool(
            &entry["eligible_pool"],
            &[("medium_claude", 1, 8.0), ("medium_codex", 3, 3.0)],
        );
        let partition = prepared["partitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|partition| {
                partition["task_ids"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(task.id))
            })
            .unwrap();
        let recorded = partition["draws"]
            .as_array()
            .unwrap()
            .iter()
            .find(|draw| draw["task_id"] == task.id)
            .unwrap();
        assert_eq!(recorded["eligible_pool"], entry["eligible_pool"]);
    };
    assert_draw(&prepared, &medium);
    assert_eq!(
        Fixture::prepared_crew(&fixture.prepare(&[&medium.id]), &medium),
        Fixture::prepared_crew(&prepared, &medium),
        "seeded retry is stable"
    );
    let applied = fixture.apply(
        &prepared,
        &[confirmed(&medium, "high"), rejected(&low, "high")],
        None,
    );
    assert_draw(&applied["escalation_prepared"], &low);
}

fn the_verifier_is_not_the_filers_crew_when_the_pool_offers_another() {
    let fixture = Fixture::new();
    let params = |title: &str, complexity| TaskAddParams {
        title: title.into(),
        description: format!("{title} at src/lib.rs."),
        acceptance_criteria: vec!["The reported defect no longer occurs.".into()],
        tags: vec!["security-review".into()],
        complexity,
        task_type: Some(TaskType::Bug),
        status: Some(TaskStatus::Proposed),
        ..Default::default()
    };
    // Each draw is seeded by task: across these findings the pool draws both
    // of its crews for a filer that is neither, and never the filer's.
    let drawn = (0..6)
        .map(|index| {
            let task = fixture
                .runtime
                .add_task(params(
                    &format!("Operator-filed {index}"),
                    TaskComplexity::Medium,
                ))
                .unwrap();
            Fixture::prepared_crew(&fixture.prepare(&[&task.id]), &task)
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(drawn.len(), 2, "{drawn:?}");
    for index in 0..6 {
        let task = fixture
            .runtime
            .add_task_with_identity(
                params(&format!("Claude-filed {index}"), TaskComplexity::Medium),
                Some("claude".into()),
                None,
            )
            .unwrap();
        let prepared = fixture.prepare(&[&task.id]);
        assert_eq!(
            Fixture::prepared_crew(&prepared, &task),
            "medium_codex",
            "filed by {:?}: {prepared}",
            task.created_by
        );
    }
    // A pool of one keeps its only crew, even the filer's.
    let task = fixture
        .runtime
        .add_task_with_identity(
            params("Codex-filed low", TaskComplexity::Low),
            Some("codex".into()),
            None,
        )
        .unwrap();
    assert_eq!(
        Fixture::prepared_crew(&fixture.prepare(&[&task.id]), &task),
        "low_lane"
    );
}

/// Pools mixing crews whose providers cannot inspect pinned source natively
/// (gemini, grok) with ones that can. `shared_claude` sits in two tiers.
const MIXED_POOL_CONFIG: &str = r#"
[workflow]
default_crew = "default_codex"
low_complexity_crews = ["low_gemini", "shared_claude", "low_grok"]
medium_complexity_crews = ["medium_grok"]
hard_complexity_crews = ["shared_claude", "hard_codex", "hard_gemini"]
[crews.default_codex]
provider = "codex"
model = "fixture-default"
[crews.low_gemini]
provider = "gemini"
model = "fixture-low-gemini"
[crews.low_grok]
provider = "grok"
model = "fixture-low-grok"
[crews.shared_claude]
provider = "claude"
model = "fixture-shared-claude"
[crews.medium_grok]
provider = "grok"
model = "fixture-medium-grok"
[crews.hard_codex]
provider = "codex"
model = "fixture-hard-codex"
[crews.hard_gemini]
provider = "gemini"
model = "fixture-hard-gemini"
[review]
verify_findings = true
"#;

fn verifiers_are_drawn_only_from_inspection_capable_crews() {
    let fixture = Fixture::with_config(MIXED_POOL_CONFIG);
    let low = (0..6)
        .map(|index| fixture.finding(&format!("Low finding {index}"), TaskComplexity::Low))
        .collect::<Vec<_>>();
    let medium = (0..6)
        .map(|index| fixture.finding(&format!("Medium finding {index}"), TaskComplexity::Medium))
        .collect::<Vec<_>>();
    let ids = low
        .iter()
        .chain(&medium)
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let prepared = fixture.prepare(&ids);
    for task in &low {
        let entry = Fixture::prepared_entry(&prepared, task);
        assert_eq!(entry["crew"], "shared_claude", "{entry}");
        assert_eq!(entry["tier"], "low", "{entry}");
    }
    // No medium member can inspect: the hard tier verifies, over its capable
    // members only, and the assignment names the tier drawn from.
    for task in &medium {
        let entry = Fixture::prepared_entry(&prepared, task);
        assert!(
            ["shared_claude", "hard_codex"].contains(&entry["crew"].as_str().unwrap()),
            "{entry}"
        );
        assert_eq!(entry["tier"], "hard", "{entry}");
    }

    // A doubtful low verdict escalates past the medium pool to the hard one,
    // which still prefers a crew that has not judged the task.
    let first = fixture.apply(
        &prepared,
        &low.iter()
            .map(|task| rejected(task, "high"))
            .chain(medium.iter().map(|task| confirmed(task, "high")))
            .collect::<Vec<_>>(),
        None,
    );
    let escalation = &first["escalation_prepared"];
    assert_eq!(escalation["task_count"], 6, "{first}");
    for task in &low {
        let entry = Fixture::prepared_entry(escalation, task);
        assert_eq!(entry["crew"], "hard_codex", "{entry}");
        assert_eq!(entry["tier"], "hard", "{entry}");
    }
}

fn without_a_capable_pool_crew_the_default_verifies_and_without_one_preparation_fails() {
    let no_capable_pool = MIXED_POOL_CONFIG
        .replace(
            r#"low_complexity_crews = ["low_gemini", "shared_claude", "low_grok"]"#,
            r#"low_complexity_crews = ["low_gemini", "low_grok"]"#,
        )
        .replace(
            r#"hard_complexity_crews = ["shared_claude", "hard_codex", "hard_gemini"]"#,
            r#"hard_complexity_crews = ["hard_gemini"]"#,
        );
    let fixture = Fixture::with_config(&no_capable_pool);
    let task = fixture.finding("Defaulted", TaskComplexity::Low);
    let prepared = fixture.prepare(&[&task.id]);
    assert_eq!(Fixture::prepared_crew(&prepared, &task), "default_codex");

    let fixture = Fixture::with_config(&no_capable_pool.replace(
        r#"default_crew = "default_codex""#,
        r#"default_crew = "low_gemini""#,
    ));
    let task = fixture.finding("Uninspectable", TaskComplexity::Low);
    let error = fixture
        .runtime
        .run_deterministic(
            "prepare_finding_verification",
            &json!({}),
            &json!({
                "task_ids": [task.id], "workspace_path": fixture.repo, "base_branch": "main",
                "run_id": "fixture-run",
            }),
            ToolContext::default(),
        )
        .expect_err("no crew can inspect the finding");
    assert!(
        error.to_string().contains("inspection_crew_unavailable"),
        "{error}"
    );
}

fn promotion_and_backlog_admission_refuse_an_unverified_review_finding() {
    let fixture = Fixture::new();
    let finding = fixture.finding("Unverified", TaskComplexity::Medium);
    let refusal = fixture
        .runtime
        .approve_task(&finding.id, None, None)
        .expect_err("an unverified review finding is not approved")
        .to_string();
    assert!(
        refusal.contains(JOB),
        "the refusal names the verification job: {refusal}"
    );
    let refusal = fixture
        .runtime
        .update_task_with_identity(
            &finding.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            None,
            None,
        )
        .expect_err("an unverified review finding is not promoted")
        .to_string();
    assert!(refusal.contains(JOB), "{refusal}");
    assert_eq!(fixture.status(&finding), TaskStatus::Proposed);

    // Neither an untagged bug nor a tagged non-bug is a review finding.
    let untagged = fixture.task(
        "Operator bug",
        TaskComplexity::Medium,
        TaskType::Bug,
        &[],
        None,
    );
    let feature = fixture.task(
        "Review suggestion",
        TaskComplexity::Medium,
        TaskType::Feature,
        &["code-review"],
        None,
    );
    for task in [&untagged, &feature] {
        fixture.runtime.approve_task(&task.id, None, None).unwrap();
    }

    // A finding filed straight into backlog is kept off the drain.
    let filed = fixture.task(
        "Filed to backlog",
        TaskComplexity::Medium,
        TaskType::Bug,
        &["qa-sweep"],
        Some(TaskStatus::Backlog),
    );
    let backlog = fixture.action("list_backlog_tasks", json!({}));
    let held = backlog["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == filed.id.as_str())
        .unwrap_or_else(|| panic!("the finding is withheld: {backlog}"));
    assert_eq!(held["reason"], "unverified_review_finding", "{backlog}");
    assert!(held["detail"].as_str().unwrap().contains(JOB), "{held}");
    assert!(
        !backlog["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(filed.id))
    );

    fixture.confirm(&filed);
    fixture.confirm(&finding);
    // The fixture's selector-less tasks contend for the same context lock,
    // which is the only thing that may still keep it off.
    let backlog = fixture.action("list_backlog_tasks", json!({}));
    let reason = backlog["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == filed.id.as_str())
        .map(|entry| entry["reason"].clone());
    assert!(
        reason.is_none_or(|reason| reason == "context_lock_conflict"),
        "a confirmed finding is admitted: {backlog}"
    );
    fixture
        .runtime
        .approve_task(&finding.id, None, None)
        .expect("a confirmed finding is approved");
}

/// `[review] verify_findings` is off unless set: approval, promotion,
/// backlog admission and readiness ignore verification state, while the
/// state trigger still offers findings to a verification routine. The
/// setting is read when the gate is checked, so turning it on applies to
/// this already-open runtime.
fn without_the_setting_a_review_finding_is_approved_promoted_and_admitted() {
    for review in ["", "[review]\nverify_findings = false\n"] {
        let fixture = Fixture::with_review(review);
        let approved = fixture.finding("Approved unverified", TaskComplexity::Medium);
        fixture
            .runtime
            .approve_task(&approved.id, None, None)
            .unwrap_or_else(|error| panic!("approval ignores verification ({review:?}): {error}"));
        assert_eq!(fixture.status(&approved), TaskStatus::Backlog);

        let promoted = fixture.finding("Promoted unverified", TaskComplexity::Medium);
        fixture
            .runtime
            .update_task_with_identity(
                &promoted.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap_or_else(|error| panic!("promotion ignores verification ({review:?}): {error}"));
        assert_eq!(fixture.status(&promoted), TaskStatus::Backlog);

        let filed = fixture.task(
            "Filed to backlog unverified",
            TaskComplexity::Medium,
            TaskType::Bug,
            &["qa-sweep"],
            Some(TaskStatus::Backlog),
        );
        let backlog = fixture.action("list_backlog_tasks", json!({}));
        assert!(
            !backlog["excluded"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["reason"] == "unverified_review_finding"),
            "admission ignores verification ({review:?}): {backlog}"
        );
        let readiness = fixture
            .runtime
            .workspace_auto_readiness(&[], None, 50, &[])
            .unwrap();
        let reasons = readiness["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["task_id"] == filed.id.as_str())
            .map(|entry| entry["reason"].clone())
            .collect::<Vec<_>>();
        assert!(
            !reasons.is_empty(),
            "the finding is in readiness: {readiness}"
        );
        assert!(
            !reasons.contains(&json!("unverified_review_finding")),
            "readiness reports no unverified finding ({review:?}): {readiness}"
        );

        // The routine and the pipeline stay available with the gate off.
        let pending =
            evaluate_routine(&fixture.runtime, &verification_routine(), false, Utc::now()).unwrap();
        let members = pending.state.unwrap().members.unwrap();
        assert!(members.pending.contains_key(&filed.id), "{members:?}");

        // Opting in applies to the open runtime at its next check.
        std::fs::write(
            fixture.repo.join(".orbit/config.toml"),
            format!("{FIXTURE_CONFIG}[review]\nverify_findings = true\n"),
        )
        .unwrap();
        let held = fixture.finding("Held after opt-in", TaskComplexity::Medium);
        let refusal = fixture
            .runtime
            .approve_task(&held.id, None, None)
            .expect_err("the opted-in gate refuses an unverified finding")
            .to_string();
        assert!(refusal.contains(JOB), "{refusal}");
        let backlog = fixture.action("list_backlog_tasks", json!({}));
        assert!(
            backlog["excluded"].as_array().unwrap().iter().any(|entry| {
                entry["id"] == filed.id.as_str() && entry["reason"] == "unverified_review_finding"
            }),
            "{backlog}"
        );
    }
}

fn verification_routine() -> RoutineDefinition {
    serde_json::from_value(json!({
        "schemaVersion": 1, "name": "fixture-verification", "enabled": true,
        "target": format!("job:{JOB}"),
        "trigger": {"state": {
            "kind": "finding_unverified", "owner_machine": "fixture-machine", "branch": "main",
            "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50, "batch_size": 5,
            "retries": 1, "deadline_minutes": 90,
        }},
    }))
    .unwrap()
}

fn the_state_trigger_picks_up_only_unverified_review_findings() {
    let fixture = Fixture::new();
    let owed = fixture.finding("Owed a verdict", TaskComplexity::Medium);
    let confirmed = fixture.finding("Already confirmed", TaskComplexity::Medium);
    fixture.confirm(&confirmed);
    let untagged = fixture.task(
        "Operator bug",
        TaskComplexity::Medium,
        TaskType::Bug,
        &[],
        None,
    );
    let feature = fixture.task(
        "Review suggestion",
        TaskComplexity::Medium,
        TaskType::Feature,
        &["delivery-code-review"],
        None,
    );

    let routine = verification_routine();
    let now = Utc::now();
    let pending = evaluate_routine(&fixture.runtime, &routine, false, now).unwrap();
    assert_eq!(pending.reason, "debouncing", "{pending:?}");
    let members = pending.state.unwrap().members.unwrap();
    assert!(members.pending.contains_key(&owed.id));
    for task in [&confirmed, &untagged, &feature] {
        assert!(!members.pending.contains_key(&task.id), "{}", task.title);
    }

    let due =
        evaluate_routine(&fixture.runtime, &routine, true, now + Duration::minutes(3)).unwrap();
    assert_eq!(due.reason, "would_fire", "{due:?}");
    assert_eq!(due.batch.len(), 1);
    assert_eq!(due.batch[0].task_ids, std::slice::from_ref(&owed.id));
}

/// Force-deleting an unverified backlog finding during debounce must not
/// prevent the next finding from receiving a verification run.
fn deleting_a_pending_finding_does_not_block_its_successor() {
    // Admission submits a real run, with a short-lived substitute worker.
    orbit_core::test_support::install_substitute_pipeline_worker([
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--list".into(),
    ]);
    let (exited, exit) = std::sync::mpsc::sync_channel(1);
    orbit_core::test_support::install_worker_reap_hook(
        orbit_core::test_support::WorkerReapPhase::AfterExitRecorded,
        move |run| exited.send(run.to_owned()).unwrap(),
    );
    let fixture = Fixture::new();
    let jobs = fixture.runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/jobs/finding_verification_pipeline.yaml"),
        jobs.join("finding_verification_pipeline.yaml"),
    )
    .unwrap();
    let deleted = fixture.task(
        "Deleted unverified backlog finding",
        TaskComplexity::Medium,
        TaskType::Bug,
        &["code-review"],
        Some(TaskStatus::Backlog),
    );
    let routine = verification_routine();
    let now = Utc::now();
    let observed = evaluate_routine(&fixture.runtime, &routine, false, now).unwrap();
    let state = observed.state.unwrap();
    assert!(state.members.unwrap().pending.contains_key(&deleted.id));
    let store = fixture.runtime.automation_store().unwrap();

    let successor = fixture.finding("Next unverified finding", TaskComplexity::Medium);
    evaluate_routine(
        &fixture.runtime,
        &routine,
        false,
        now + Duration::minutes(1),
    )
    .unwrap();
    assert!(
        fixture
            .runtime
            .delete_task_guarded(&deleted.id, false)
            .is_err(),
        "a backlog finding requires force to delete"
    );
    fixture
        .runtime
        .delete_task_guarded(&deleted.id, true)
        .unwrap();

    let due = now + Duration::minutes(3);
    let preview = evaluate_routine(&fixture.runtime, &routine, true, due).unwrap();
    assert_eq!(preview.reason, "would_fire", "{preview:?}");
    assert_eq!(preview.batch.len(), 1);
    assert_eq!(
        preview.batch[0].task_ids,
        std::slice::from_ref(&successor.id)
    );
    assert!(
        !preview
            .state
            .unwrap()
            .members
            .unwrap()
            .pending
            .contains_key(&deleted.id)
    );
    assert!(
        store
            .automation_state(&state.consumer)
            .unwrap()
            .unwrap()
            .members
            .unwrap()
            .pending
            .contains_key(&deleted.id),
        "a dry run must leave retirement for the real evaluation"
    );

    let admitted = evaluate_routine(&fixture.runtime, &routine, false, due).unwrap();
    assert_eq!(admitted.reason, "fired", "{admitted:?}");
    assert_eq!(admitted.batch.len(), 1);
    assert_eq!(
        admitted.batch[0].task_ids,
        std::slice::from_ref(&successor.id)
    );
    let members = store
        .automation_state(&state.consumer)
        .unwrap()
        .unwrap()
        .members
        .unwrap();
    assert!(!members.pending.contains_key(&deleted.id));
    let active = members.active.unwrap();
    assert_eq!(active.task_ids(), vec![successor.id]);
    assert!(
        active.action_id.is_some(),
        "the successor has a submitted run"
    );
    assert_eq!(
        exit.recv_timeout(std::time::Duration::from_secs(10))
            .unwrap(),
        active.action_id.unwrap()
    );
}

crate::scenario_table!(
    deleted_pending_member_scenarios,
    deleting_a_pending_finding_does_not_block_its_successor
);

fn material_edits_supersede_prepared_verdicts() {
    let fixture = Fixture::new();
    let edits = [
        TaskUpdateParams {
            title: Some("Changed title".into()),
            ..Default::default()
        },
        TaskUpdateParams {
            description: Some("A different defect".into()),
            ..Default::default()
        },
        TaskUpdateParams {
            acceptance_criteria: Some(vec!["Different repair".into()]),
            ..Default::default()
        },
        TaskUpdateParams {
            context_files: Some(vec!["file:src/lib.rs".into()]),
            ..Default::default()
        },
        TaskUpdateParams {
            tags: Some(vec!["code-review".into(), "changed-evidence".into()]),
            ..Default::default()
        },
    ];
    for (index, edit) in edits.into_iter().enumerate() {
        for reject in [false, true] {
            let task = fixture.finding(
                &format!("Material {index} reject={reject}"),
                TaskComplexity::Medium,
            );
            let prepared = fixture.prepare(&[&task.id]);
            fixture
                .runtime
                .update_task_with_identity(&task.id, edit.clone(), None, None)
                .unwrap();
            let report = if reject {
                rejected(&task, "high")
            } else {
                confirmed(&task, "high")
            };
            let applied = fixture.apply(&prepared, &[report], None);
            assert_eq!(
                outcome(&applied, &task)["outcome"],
                "superseded",
                "{applied}"
            );
            assert_eq!(
                outcome(&applied, &task)["reason"],
                "material_changed",
                "{applied}"
            );
            assert!(fixture.verification(&task).is_none());
            assert_eq!(fixture.status(&task), TaskStatus::Proposed);
        }
    }
    let legacy = fixture.finding("Old prepared checkpoint", TaskComplexity::Medium);
    let mut prepared = fixture.prepare(&[&legacy.id]);
    prepared["tasks"][0]
        .as_object_mut()
        .unwrap()
        .remove("finding_fingerprint");
    let applied = fixture.apply(&prepared, &[confirmed(&legacy, "high")], None);
    assert_eq!(
        outcome(&applied, &legacy)["reason"],
        "material_changed",
        "{applied}"
    );
    assert!(fixture.verification(&legacy).is_none());
}

fn escalation_preserves_material_from_initial_preparation() {
    let fixture = Fixture::new();
    // Cover drift both before escalation is constructed and while the second
    // verifier is working. Neither round may rebind the first report.
    for before_escalation in [true, false] {
        let task = fixture.finding(
            &format!("Escalation drift {before_escalation}"),
            TaskComplexity::Medium,
        );
        let prepared = fixture.prepare(&[&task.id]);
        let edit = || {
            fixture
                .runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        description: Some("Replacement claim during verification".into()),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap()
        };
        if before_escalation {
            edit();
        }
        let first = fixture.apply(&prepared, &[confirmed(&task, "low")], None);
        assert_eq!(first["escalation_count"], 1, "{first}");
        let escalation = &first["escalation_prepared"];
        assert_eq!(
            escalation["tasks"][0]["finding_fingerprint"],
            prepared["tasks"][0]["finding_fingerprint"]
        );
        if !before_escalation {
            edit();
        }
        let second = fixture.apply(
            escalation,
            &[rejected(&task, "high")],
            Some(&first["task_outcomes"]),
        );
        assert_eq!(
            outcome(&second, &task)["reason"],
            "material_changed",
            "{second}"
        );
        assert!(fixture.verification(&task).is_none());
        assert_eq!(fixture.status(&task), TaskStatus::Proposed);
        fixture.confirm(&task);
        fixture.runtime.approve_task(&task.id, None, None).unwrap();
    }
}

fn confirmation_is_fresh_for_promotion_admission_and_observation() {
    let fixture = Fixture::new();
    let proposed = fixture.finding("Confirmed proposed", TaskComplexity::Medium);
    let backlog = fixture.task(
        "Confirmed backlog",
        TaskComplexity::Medium,
        TaskType::Bug,
        &["code-review"],
        Some(TaskStatus::Backlog),
    );
    let commented = fixture.finding("Comment-only edit", TaskComplexity::Medium);
    for task in [&proposed, &backlog, &commented] {
        fixture.confirm(task);
    }
    // Promotion and material replacement in one write must also hold.
    fixture
        .runtime
        .update_task_with_identity(
            &proposed.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                description: Some("Replacement at promotion".into()),
                ..Default::default()
            },
            None,
            None,
        )
        .expect_err("promotion cannot reuse a verdict for replaced material");
    for task in [&proposed, &backlog] {
        fixture
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    description: Some("New claim after confirmation".into()),
                    acceptance_criteria: Some(vec!["Repair the new claim".into()]),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }
    fixture
        .runtime
        .update_task_with_identity(
            &commented.id,
            TaskUpdateParams {
                comment: Some("Extra discussion without changing the claim".into()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    fixture
        .runtime
        .approve_task(&proposed.id, None, None)
        .expect_err("edited finding needs verification");
    fixture
        .runtime
        .approve_task(&commented.id, None, None)
        .unwrap();
    let admission = fixture.action("list_backlog_tasks", json!({}));
    let held = admission["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == backlog.id)
        .unwrap();
    assert_eq!(held["reason"], "unverified_review_finding", "{admission}");
    let pending =
        evaluate_routine(&fixture.runtime, &verification_routine(), false, Utc::now()).unwrap();
    let members = pending.state.unwrap().members.unwrap();
    for task in [&proposed, &backlog] {
        assert!(members.pending.contains_key(&task.id));
    }
    assert!(!members.pending.contains_key(&commented.id));
    for task in [&proposed, &backlog] {
        fixture.confirm(task);
    }
    fixture
        .runtime
        .approve_task(&proposed.id, None, None)
        .unwrap();
    let admission = fixture.action("list_backlog_tasks", json!({}));
    assert!(
        !admission["excluded"].as_array().unwrap().iter().any(
            |entry| entry["id"] == backlog.id && entry["reason"] == "unverified_review_finding"
        ),
        "{admission}"
    );
}

fn legacy_confirmation_is_readable_but_cannot_authorize_promotion() {
    let fixture = Fixture::new();
    let task = fixture.finding("Legacy confirmation", TaskComplexity::Medium);
    fixture.confirm(&task);
    let mut legacy = serde_json::to_value(fixture.verification(&task).unwrap()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("finding_fingerprint");
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &fixture.runtime.global_root(),
        ),
    )
    .unwrap();
    let backends = orbit_store::compose::workspace_coordinated_backends(
        registry,
        fixture.runtime.workspace_id().unwrap(),
        fixture.runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    backends
        .history
        .update_task_history(
            &task.id,
            orbit_store::contracts::TaskHistoryUpdateParams {
                actor: "finding-verification".into(),
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: "finding-verification".into(),
                    event: "finding_verified".into(),
                    note: Some(legacy.to_string()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        fixture.verification(&task).unwrap().verdict,
        FindingVerdict::Confirmed
    );
    assert!(
        fixture
            .verification(&task)
            .unwrap()
            .finding_fingerprint
            .is_none()
    );
    fixture
        .runtime
        .approve_task(&task.id, None, None)
        .expect_err("legacy confirmation is stale");
    fixture.confirm(&task);
    fixture.runtime.approve_task(&task.id, None, None).unwrap();
}

/// Execute the shipped graph and deterministic activities; only verifier
/// sessions are replaced with scripted boundary results.
struct VerificationHost<'a> {
    fixture: &'a Fixture,
    failed_task: Option<&'a Task>,
    escalated_task: Option<&'a Task>,
    applied: Mutex<Vec<Value>>,
}

impl orbit_engine::test_support::NullRuntimeHost for VerificationHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        context: ToolContext,
    ) -> Result<Value, DispatchError> {
        if action == "fixture_verify" {
            let ids = input["task_ids"].as_array().unwrap();
            if self
                .failed_task
                .is_some_and(|task| ids.contains(&json!(task.id)))
            {
                return Err(DispatchError::CliInvocationPermanent(
                    "fixture verifier executable unavailable".into(),
                ));
            }
            let tasks = ids
                .iter()
                .map(|id| {
                    let task = self.fixture.runtime.get_task(id.as_str().unwrap()).unwrap();
                    if self
                        .escalated_task
                        .is_some_and(|escalated| escalated.id == task.id)
                        && input["escalation"] != true
                    {
                        rejected(&task, "high")
                    } else {
                        confirmed(&task, "high")
                    }
                })
                .collect::<Vec<_>>();
            return Ok(json!({"partition_index": input["partition_index"], "tasks": tasks}));
        }
        let result = self
            .fixture
            .runtime
            .run_deterministic(action, config, input, context)?;
        if action == "apply_finding_verification" {
            self.applied.lock().unwrap().push(result.clone());
        }
        Ok(result)
    }
}

fn run_verification_job(
    host: &VerificationHost<'_>,
    tasks: &[&Task],
) -> Result<JobOutcome, DispatchError> {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut job = load_job_asset(
        &std::fs::read_to_string(assets.join("jobs/finding_verification_pipeline.yaml")).unwrap(),
    )
    .unwrap()
    .spec;
    let mut catalog = V2ActivityCatalog::new();
    for name in [
        "prepare_finding_verification",
        "verify_finding",
        "apply_finding_verification",
        "pipeline_success_guard",
    ] {
        let asset = load_activity_asset(
            &std::fs::read_to_string(assets.join(format!("activities/{name}.yaml"))).unwrap(),
        )
        .unwrap();
        let mut activity = asset.spec;
        if name == "verify_finding" {
            activity.spec = ActivityV2Spec::Deterministic(DeterministicSpec {
                action: "fixture_verify".into(),
                config: Value::Null,
            });
        }
        catalog.insert(name, activity);
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let audit = host.fixture._root.path().join("audit");
    let store = Arc::new(orbit_store::Store::open_in_memory().unwrap());
    let writer = Arc::new(V2AuditWriter::new(
        "verification-run",
        "fixture",
        Arc::new(V2SqliteSink::for_audit_root(
            store,
            "ws_fixture",
            "verification-run",
            "fixture",
            None,
            &audit,
        )),
    ));
    execute_job_with_resume(
        &job,
        json!({
            "task_ids": tasks.iter().map(|task| task.id.as_str()).collect::<Vec<_>>(),
            "workspace_path": host.fixture.repo, "base_branch": "main",
            "max_partition_size": 1,
        }),
        "verification-run",
        writer,
        host,
        None,
    )
}

fn shipped_job_applies_successful_partitions_and_retains_a_failed_partitions_error() {
    let fixture = Fixture::new();
    let successful = fixture.finding("Confirmed by a successful worker", TaskComplexity::Medium);
    let failed = fixture.finding("Failed verifier", TaskComplexity::Low);
    let host = VerificationHost {
        fixture: &fixture,
        failed_task: Some(&failed),
        escalated_task: None,
        applied: Mutex::default(),
    };
    let result = run_verification_job(&host, &[&successful, &failed]);
    assert!(
        result.is_err(),
        "the guard still refuses unverified findings: {result:?}"
    );
    let applied = host.applied.lock().unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0]["partition_count"], 2);
    assert_eq!(outcome(&applied[0], &successful)["outcome"], "verified");
    let held = outcome(&applied[0], &failed);
    assert_eq!(held["reason"], "verifier_failed", "{held}");
    let detail = held["detail"].as_str().unwrap();
    assert!(
        detail.contains("fixture verifier executable unavailable"),
        "{held}"
    );
    assert!(detail.contains("low_lane"), "{held}");
    assert!(fixture.verification(&failed).is_none());
    assert_eq!(fixture.status(&failed), TaskStatus::Proposed);
    assert_eq!(
        fixture.verification(&successful).unwrap().verdict,
        FindingVerdict::Confirmed
    );
    assert_eq!(
        shown_verdict(&fixture, &successful).0,
        "finding-verification: confirmed"
    );
}

fn shipped_job_applies_escalation_verdicts_and_empty_selection_is_a_noop() {
    let fixture = Fixture::new();
    let low = fixture.finding("Low tier rejection", TaskComplexity::Low);
    let settled = fixture.finding("Initial confirmation", TaskComplexity::Hard);
    let host = VerificationHost {
        fixture: &fixture,
        failed_task: None,
        escalated_task: Some(&low),
        applied: Mutex::default(),
    };
    let run = run_verification_job(&host, &[&low, &settled]).unwrap();
    assert!(run.success, "{run:?}");
    assert_eq!(run.pipeline["prepare"]["partition_count"], 2);
    assert_eq!(run.pipeline["apply"]["escalation_count"], 1);
    assert_eq!(run.pipeline["apply_escalations"]["recorded_count"], 1);
    let record = fixture.verification(&low).unwrap();
    assert!(record.escalated);
    assert_eq!(record.crews.len(), 2);
    assert_eq!(record.verdict, FindingVerdict::Confirmed);
    assert_eq!(
        shown_verdict(&fixture, &low).0,
        "finding-verification: confirmed"
    );
    assert_eq!(
        outcome(&run.pipeline["apply_escalations"], &settled)["outcome"],
        "verified"
    );

    let empty = Fixture::new();
    let host = VerificationHost {
        fixture: &empty,
        failed_task: None,
        escalated_task: None,
        applied: Mutex::default(),
    };
    let run = run_verification_job(&host, &[]).unwrap();
    assert!(run.success, "{run:?}");
    assert_eq!(run.pipeline["verdict_results"], json!([]));
    assert_eq!(run.pipeline["verdict_errors"], json!([]));
    assert_eq!(run.pipeline["apply"]["recorded_count"], 0);
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    verdicts_are_recorded_with_sha_and_evidence_and_rejection_closes_the_task_scenarios,
    verdicts_are_recorded_with_sha_and_evidence_and_rejection_closes_the_task,
    a_verdict_citing_lines_absent_at_the_pinned_revision_is_not_recorded,
    a_doubtful_verdict_is_reverified_by_the_next_tier_and_names_every_crew,
    the_verifier_is_not_the_filers_crew_when_the_pool_offers_another,
    verification_partitions_record_pressure_in_both_rounds,
    verifiers_are_drawn_only_from_inspection_capable_crews,
    without_a_capable_pool_crew_the_default_verifies_and_without_one_preparation_fails,
    promotion_and_backlog_admission_refuse_an_unverified_review_finding,
    without_the_setting_a_review_finding_is_approved_promoted_and_admitted,
    the_state_trigger_picks_up_only_unverified_review_findings,
    material_edits_supersede_prepared_verdicts,
    escalation_preserves_material_from_initial_preparation,
    confirmation_is_fresh_for_promotion_admission_and_observation
);

crate::scenario_table!(
    legacy_confirmation_is_readable_but_cannot_authorize_promotion_scenarios,
    legacy_confirmation_is_readable_but_cannot_authorize_promotion,
    shipped_job_applies_successful_partitions_and_retains_a_failed_partitions_error,
    shipped_job_applies_escalation_verdicts_and_empty_selection_is_a_noop
);
