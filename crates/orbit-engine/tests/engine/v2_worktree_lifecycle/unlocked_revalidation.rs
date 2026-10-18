//! [ORB-16784] Required validation narrowed to the unlocked-path commands.
//!
//! Once a run's candidate passed the full required list, a later candidate
//! whose tree differs from that one only in `workflow.unlocked_context_paths`
//! runs `workflow.unlocked_path_validation_commands` alone. The tree diff
//! decides: a code change, including one a rebase brought from the base,
//! runs the full list, and so does every candidate while the setting is
//! unset. Each command appends its scope to a trace outside the checkout.

use super::*;

const RUN: &str = "jrun-unlocked-revalidation";
const TASK: &str = "T-NARROW";

/// A run's checkout with one committed code change, validated through the
/// shipped `candidate_validate` action.
struct Candidate {
    fixture: Fixture,
    host: LifecycleHost,
    checkout: Checkout,
    base_sha: String,
    trace: PathBuf,
}

impl Candidate {
    fn new(unlocked: bool) -> Self {
        let fixture = Fixture::new();
        let host = LifecycleHost::new(&fixture.repo);
        host.add_task(TASK, TaskStatus::Backlog);
        let setup = action(&host, "worktree_setup", &setup_input(&[TASK], RUN)).unwrap();
        let checkout = Checkout::from_setup(&setup);
        let trace = fixture.root.path().join("validation-trace");
        let traced = |scope: &str| format!("printf '{scope}\\n' >> '{}'", trace.display());
        host.set_required_commands(&[&traced("full")]);
        if unlocked {
            host.set_unlocked_validation(&["**/*.md"], &[&traced("docs")]);
        }
        commit_file(&checkout.path, "lib.rs", "fn feature() {}\n");
        fs::create_dir_all(checkout.path.join("docs")).unwrap();
        Self {
            base_sha: setup["base_sha"].as_str().unwrap().to_string(),
            fixture,
            host,
            checkout,
            trace,
        }
    }

    fn validate(&self) -> Value {
        action(
            &self.host,
            "candidate_validate",
            &json!({
                "job_run_id": RUN, "completed_task_ids": [TASK],
                "workspace_path": self.checkout.path,
                "base_ref": BASE, "base_sha": self.base_sha,
            }),
        )
        .expect("candidate_validate passes")
    }

    /// The scopes the commands ran under since the last call.
    fn take_trace(&self) -> Vec<String> {
        let trace = fs::read_to_string(&self.trace).unwrap_or_default();
        fs::write(&self.trace, "").unwrap();
        trace.lines().map(str::to_string).collect()
    }

    fn tree(&self) -> String {
        git(&self.checkout.path, &["rev-parse", "HEAD^{tree}"])
    }

    /// The durable log of the latest validation's first command.
    fn log_scope(&self) -> Value {
        let artifacts = self.host.artifacts.lock().unwrap();
        let log = artifacts[TASK]
            .iter()
            .find(|artifact| artifact.path == format!("validation/{RUN}/0.json"))
            .expect("durable validation evidence");
        serde_json::from_slice::<Value>(&log.content).unwrap()["validation_scope"].clone()
    }
}

/// A Markdown-only repair runs only the unlocked-path commands and records
/// the validated tree it was compared with; a code change a rebase brought
/// from the base is a new combination and runs the full list again.
fn a_markdown_only_repair_runs_only_the_unlocked_path_commands() {
    isolated_in(
        module_path!(),
        "a_markdown_only_repair_runs_only_the_unlocked_path_commands",
        || {
            let candidate = Candidate::new(true);
            let first = candidate.validate();
            assert_eq!(candidate.take_trace(), ["full"]);
            assert_eq!(first["validation_scope"]["kind"], "full", "{first}");
            assert_eq!(first["validation_scope"]["reason"], "no_validated_tree");
            let validated_tree = candidate.tree();
            let validated_head = git(&candidate.checkout.path, &["rev-parse", "HEAD"]);

            commit_file(&candidate.checkout.path, "docs/guide.md", "# Guide\n");
            let narrowed = candidate.validate();
            assert_eq!(candidate.take_trace(), ["docs"], "{narrowed}");
            let scope = &narrowed["validation_scope"];
            assert_eq!(scope["kind"], "narrowed", "{narrowed}");
            assert_eq!(scope["reason"], "unlocked_paths_only");
            assert_eq!(scope["validated_tree"], validated_tree.as_str());
            assert_eq!(scope["validated_head"], validated_head.as_str());
            assert_eq!(scope["tested_tree"], candidate.tree().as_str());
            assert_eq!(scope["changed_paths"], json!(["docs/guide.md"]));
            assert_eq!(narrowed["tested_tree"], candidate.tree().as_str());
            assert_eq!(&candidate.log_scope(), scope, "the log records the scope");

            // The base brings a code change; the rebased candidate differs
            // from the validated tree in code as well as prose.
            let base = commit_file(&candidate.fixture.repo, "base.rs", "fn base() {}\n");
            git(&candidate.checkout.path, &["rebase", "-q", BASE]);
            let rebased = candidate.validate();
            assert_eq!(candidate.take_trace(), ["full"], "{rebased}");
            let scope = &rebased["validation_scope"];
            assert_eq!(scope["kind"], "full", "{rebased}");
            assert_eq!(scope["reason"], "changed_locked_paths");
            assert_eq!(scope["locked_paths"], json!(["base.rs"]));
            assert_eq!(
                scope["validated_tree"],
                validated_tree.as_str(),
                "a narrowed pass never becomes the comparison point"
            );
            assert!(is_ancestor(&candidate.checkout.path, &base, "HEAD"));

            // That full pass is the new comparison point.
            commit_file(&candidate.checkout.path, "docs/guide.md", "# Guide v2\n");
            let narrowed = candidate.validate();
            assert_eq!(candidate.take_trace(), ["docs"], "{narrowed}");
            assert_eq!(
                narrowed["validation_scope"]["validated_tree"],
                git(&candidate.checkout.path, &["rev-parse", "HEAD~1^{tree}"]).as_str()
            );
        },
    );
}

/// With `workflow.unlocked_path_validation_commands` unset, a Markdown-only
/// repair runs the full list.
fn without_unlocked_path_commands_every_repair_runs_full_validation() {
    isolated_in(
        module_path!(),
        "without_unlocked_path_commands_every_repair_runs_full_validation",
        || {
            let candidate = Candidate::new(false);
            candidate.validate();
            commit_file(&candidate.checkout.path, "docs/guide.md", "# Guide\n");
            let repaired = candidate.validate();
            assert_eq!(candidate.take_trace(), ["full", "full"], "{repaired}");
            assert_eq!(repaired["validation_scope"]["kind"], "full");
            assert_eq!(
                repaired["validation_scope"]["reason"],
                "unlocked_path_validation_unset"
            );
        },
    );
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn unlocked_revalidation_contracts() {
    isolated_in(module_path!(), "unlocked_revalidation_contracts", || {
        crate::git_fixture::run_scenarios(&[
            (
                "a_markdown_only_repair_runs_only_the_unlocked_path_commands",
                a_markdown_only_repair_runs_only_the_unlocked_path_commands,
            ),
            (
                "without_unlocked_path_commands_every_repair_runs_full_validation",
                without_unlocked_path_commands_every_repair_runs_full_validation,
            ),
        ]);
    });
}
