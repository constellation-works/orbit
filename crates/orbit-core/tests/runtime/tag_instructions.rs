//! Per-tag instructions through the runtime host: an activity on a task
//! carrying a mapped tag receives trusted text, from Orbit's shipped default
//! or from a repository file read at the run's recorded base revision; an
//! untagged task receives nothing; the delivery's own commits and worktree
//! edits never supply text; an oversized file is refused by name; and the run
//! record lists what each dispatch received.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskAddParams;
use orbit_engine::{DispatchError, RuntimeHost};
use orbit_store::contracts::{JobRunStoreBackend, V2AuditEventInsertParams};
use orbit_types::workflow::{
    MAX_TAG_INSTRUCTION_FILE_BYTES, PipelineState, TagInstruction, TagInstructionSource,
    V2AuditEnvelope, V2AuditEvent, V2AuditEventKind,
};
use serde_json::json;
use tempfile::TempDir;

use crate::git_test_support;

struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Fixture {
    /// A Git repository with `.orbit/config.toml` holding `config`.
    fn new(config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let workspace = repo.join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), config).unwrap();
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "fixture@example.com"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            repo,
            runtime,
            jobs,
        }
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

    fn task(&self, tags: &[&str]) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: "Tag instruction fixture".into(),
                description: "A task whose tags select instructions.".into(),
                acceptance_criteria: vec!["The activity receives its tag's text.".into()],
                tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    /// A pipeline run whose `worktree` step recorded `base_sha`.
    fn run(&self, task: &str, base_sha: Option<&str>) -> String {
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
        if let Some(base_sha) = base_sha {
            state.pipeline = json!({ "worktree": { "base_sha": base_sha } });
        }
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }

    fn instructions(
        &self,
        run: &str,
        activity: &str,
        task: &str,
    ) -> Result<Vec<TagInstruction>, DispatchError> {
        RuntimeHost::tag_instructions_for_activity(
            &self.runtime,
            run,
            activity,
            &json!({ "task_id": task }),
        )
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = git_test_support::run(repo, args).unwrap();
    assert!(output.success, "git {args:?} failed: {}", output.stderr);
    output.stdout
}

fn summary(instructions: &[TagInstruction]) -> Vec<(String, TagInstructionSource)> {
    instructions
        .iter()
        .map(|instruction| (instruction.tag.clone(), instruction.source.clone()))
        .collect()
}

fn tagged_tasks_receive_shipped_defaults_and_untagged_ones_nothing() {
    let fx = Fixture::new("");
    fx.commit("README.md", "fixture\n");
    let tagged = fx.task(&["UI", "No-Diff-Expected", "ui"]);
    let run = fx.run(&tagged, None);

    let implement = fx.instructions(&run, "agent_implement", &tagged).unwrap();
    assert_eq!(
        summary(&implement),
        vec![
            (
                "no-diff-expected".to_string(),
                TagInstructionSource::Default
            ),
            ("ui".to_string(), TagInstructionSource::Default),
        ],
        "tags match case-insensitively, apply once each and in sorted order"
    );
    let review = fx
        .instructions(&run, "agent_review_repair", &tagged)
        .unwrap();
    assert_eq!(
        summary(&review),
        vec![("ui".to_string(), TagInstructionSource::Default)],
        "reviewers receive the ui screenshot check, and no-diff-expected ships no review text"
    );
    assert!(
        fx.instructions(&run, "step_failure_recovery", &tagged)
            .unwrap()
            .is_empty(),
        "no default addresses recovery"
    );
    assert!(
        fx.instructions(&run, "agent_plan", &tagged)
            .unwrap()
            .is_empty(),
        "an activity no role covers receives nothing"
    );

    let untagged = fx.task(&[]);
    let run = fx.run(&untagged, None);
    assert!(
        fx.instructions(&run, "agent_implement", &untagged)
            .unwrap()
            .is_empty(),
        "an untagged task receives no tag instructions"
    );
}

fn workspace_files_replace_defaults_and_are_read_at_the_base_revision() {
    let fx = Fixture::new(
        "[workflow.tag_instructions.UI]\n\
         implement = \"./docs/agents/ui.md\"\n\
         review = false\n\
         recovery = \"docs/agents/added-later.md\"\n",
    );
    let base = fx.commit("docs/agents/ui.md", "Base revision UI rule.\n");
    fx.commit("docs/agents/ui.md", "A delivery's committed rewrite.\n");
    fx.commit("docs/agents/added-later.md", "Added by the delivery.\n");
    std::fs::write(
        fx.repo.join("docs/agents/ui.md"),
        "An uncommitted worktree edit.\n",
    )
    .unwrap();
    let task = fx.task(&["ui"]);
    let run = fx.run(&task, Some(&base));

    let implement = fx.instructions(&run, "agent_implement", &task).unwrap();
    assert_eq!(
        summary(&implement),
        vec![(
            "ui".to_string(),
            TagInstructionSource::Workspace {
                path: "docs/agents/ui.md".to_string(),
                revision: base.clone(),
            }
        )],
        "the workspace file replaces the shipped default"
    );
    assert_eq!(
        implement[0].text, "Base revision UI rule.\n",
        "neither the delivery's commits nor its worktree edits supply the text"
    );
    assert!(
        fx.instructions(&run, "agent_review_repair", &task)
            .unwrap()
            .is_empty(),
        "`false` disables the shipped default"
    );
    let refused = fx
        .instructions(&run, "final_recovery", &task)
        .expect_err("a file absent at the base revision is refused");
    assert!(
        matches!(&refused, DispatchError::CliInvocationPermanent(message)
            if message.starts_with("tag_instruction_unreadable")
                && message.contains("docs/agents/added-later.md")),
        "the refusal names the error and the file: {refused}"
    );
}

fn an_oversized_instruction_file_is_refused_by_name() {
    let fx = Fixture::new("[workflow.tag_instructions.big]\nimplement = \"big.md\"\n");
    let base = fx.commit("big.md", &"x".repeat(MAX_TAG_INSTRUCTION_FILE_BYTES + 1));
    let task = fx.task(&["big"]);
    let run = fx.run(&task, Some(&base));

    let refused = fx
        .instructions(&run, "agent_implement", &task)
        .expect_err("an oversized file is refused, not cut");
    assert!(
        matches!(&refused, DispatchError::CliInvocationPermanent(message)
            if message.starts_with("tag_instruction_too_large") && message.contains("big.md")),
        "the refusal names the error and the file: {refused}"
    );
    let other = fx.task(&["unrelated"]);
    let run = fx.run(&other, Some(&base));
    assert!(
        fx.instructions(&run, "agent_implement", &other)
            .unwrap()
            .is_empty(),
        "a task without the tag is unaffected by its refused file"
    );
}

fn the_run_record_lists_the_instructions_each_dispatch_received() {
    let fx = Fixture::new("");
    fx.commit("README.md", "fixture\n");
    let task = fx.task(&["ui"]);
    let run = fx.run(&task, None);
    assert_eq!(
        fx.runtime.collect_run_tag_instructions(&run).unwrap().state,
        "unavailable",
        "a run without v2 audit evidence cannot say what it received"
    );

    let received = fx.instructions(&run, "agent_implement", &task).unwrap();
    let event = V2AuditEvent {
        envelope: V2AuditEnvelope {
            schema_version: 1,
            event_type: "activity.tag_instructions_applied".into(),
            event_id: "tag-instructions-1".into(),
            ts: Utc::now(),
            run_id: run.clone(),
            agent_identity: "claude".into(),
            parent_event_id: None,
            workspace_path: None,
        },
        kind: V2AuditEventKind::TagInstructionsApplied {
            step_id: "implement".into(),
            activity_name: "agent_implement".into(),
            task_id: task.clone(),
            instructions: received.iter().map(TagInstruction::record).collect(),
        },
    };
    fx.runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: fx.runtime.workspace_id().unwrap(),
            event_id: event.envelope.event_id.clone(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: "tag_instructions_applied".into(),
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
    let projected = listed.to_json();
    assert_eq!(projected["items"][0]["step_id"], "implement");
    assert_eq!(projected["items"][0]["instructions"][0]["tag"], "ui");
    assert_eq!(
        projected["items"][0]["instructions"][0]["source"]["kind"],
        "default"
    );
    assert_eq!(
        projected["items"][0]["instructions"][0]["sha256"],
        received[0].sha256.as_str()
    );
    assert!(
        projected["items"][0]["instructions"][0]
            .get("text")
            .is_none(),
        "the run record carries the digest, not the text"
    );
    assert_eq!(
        listed.header_lines(),
        vec![format!(
            "Tag instructions: implement (agent_implement): ui (Orbit default, sha256 {})",
            &received[0].sha256[..12]
        )]
    );
}

crate::scenario_table!(
    tag_instructions,
    tagged_tasks_receive_shipped_defaults_and_untagged_ones_nothing,
    workspace_files_replace_defaults_and_are_read_at_the_base_revision,
    an_oversized_instruction_file_is_refused_by_name,
    the_run_record_lists_the_instructions_each_dispatch_received,
);
