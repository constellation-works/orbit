//! The gated fixture every gate test starts from.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_types::workflow::automation::{AutomationState, Delivery, SourcePage};
use orbit_types::workflow::{REVIEW_GATE_ARTIFACT, ReviewCertificate};
use serde_json::Value;

use crate::application::review::tests::{
    Fixture, admit_input, admitted_run, fixture, git, implement_candidate, seed_task, settle_input,
};
use crate::application::review::{review_gate_admit, review_gate_settle};
use crate::application::task::TaskUpdateParams;

/// Fixture with a task, its admitted run, and a checked-out candidate.
pub(super) struct Gated {
    pub(super) fixture: Fixture,
    pub(super) task_id: String,
    pub(super) run_id: String,
    pub(super) implementation_sha: String,
}

pub(super) fn gated_fixture(config: &str) -> Gated {
    let fixture = fixture(config);
    let task = seed_task(&fixture.runtime, "gated change");
    let run = admitted_run(
        &fixture.runtime,
        "task_pr_pipeline",
        std::slice::from_ref(&task.id),
    );
    let implementation_sha = implement_candidate(&fixture.repo, &task.id);
    Gated {
        fixture,
        task_id: task.id,
        run_id: run.run_id,
        implementation_sha,
    }
}

impl Gated {
    pub(super) fn admit(&self) -> Result<Value, orbit_engine::DispatchError> {
        review_gate_admit(
            &self.fixture.runtime,
            "review_gate_admit",
            &admit_input(
                &self.run_id,
                std::slice::from_ref(&self.task_id),
                &self.fixture.repo,
            ),
        )
    }

    pub(super) fn settle(&self, admission: &Value) -> Result<Value, orbit_engine::DispatchError> {
        review_gate_settle(
            &self.fixture.runtime,
            "review_gate_settle",
            &settle_input(
                &self.run_id,
                std::slice::from_ref(&self.task_id),
                &self.fixture.repo,
                admission,
            ),
        )
    }

    pub(super) fn certificate(&self) -> ReviewCertificate {
        let artifact = self
            .fixture
            .runtime
            .get_task_artifact(&self.task_id, REVIEW_GATE_ARTIFACT)
            .expect("read")
            .expect("certificate artifact");
        serde_json::from_slice(&artifact.content).expect("certificate json")
    }

    pub(super) fn head(&self) -> String {
        git(&self.fixture.repo, &["rev-parse", "HEAD"])
    }

    pub(super) fn author_of(&self, spec: &str) -> String {
        git(
            &self.fixture.repo,
            &["log", "-1", "--format=%an <%ae>", spec],
        )
    }

    pub(super) fn rescope(&self, selectors: &[&str]) {
        self.fixture
            .runtime
            .update_task(
                &self.task_id,
                TaskUpdateParams {
                    context_files: Some(
                        selectors
                            .iter()
                            .map(|selector| (*selector).into())
                            .collect(),
                    ),
                    ..TaskUpdateParams::default()
                },
            )
            .expect("rescope");
    }

    pub(super) fn context_files(&self) -> Vec<String> {
        self.fixture
            .runtime
            .get_task(&self.task_id)
            .expect("task")
            .context_files
    }
}

/// Land the checked-out candidate onto `main` the way a squash merge does
/// and return the delivery the source adapter would observe.
pub(super) fn land_squash(gated: &Gated, repository: &str) -> Delivery {
    let repo = &gated.fixture.repo;
    let candidate = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(repo, &["checkout", "main"]);
    let before = git(repo, &["rev-parse", "HEAD"]);
    let before_tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    git(repo, &["merge", "--squash", &candidate]);
    git(repo, &["commit", "-m", &format!("squash {candidate}")]);
    let after = git(repo, &["rev-parse", "HEAD"]);
    let after_tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    Delivery {
        key: format!("pr:{repository}:main:{}", &after[..8]),
        repository: repository.to_string(),
        branch: "main".to_string(),
        before: orbit_types::workflow::automation::SourceRevision {
            commit: before,
            tree: before_tree,
        },
        after: orbit_types::workflow::automation::SourceRevision {
            commit: after.clone(),
            tree: after_tree,
        },
        commits: vec![after],
        task_ids: vec![],
        evidence_reference: "https://github.com/example/pull/1".to_string(),
        evidence_digest: "digest".to_string(),
        landed_at: Utc::now(),
    }
}

pub(super) fn page_for(delivery: &Delivery) -> (AutomationState, SourcePage) {
    let state = AutomationState {
        members: None,
        consumer: "hm/ws/auto-task/delivery-code-review".into(),
        epoch: "e".into(),
        trigger: None,
        repository: delivery.repository.clone(),
        branch: "main".into(),
        generation: 1,
        baseline: delivery.before.clone(),
        observed: delivery.before.clone(),
        covered: delivery.before.clone(),
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: BTreeMap::new(),
        associations: BTreeMap::new(),
        active: None,
        stall: None,
    };
    let page = SourcePage {
        from: delivery.before.clone(),
        through: delivery.after.clone(),
        commits: delivery.commits.clone(),
        deliveries: vec![delivery.clone()],
        unresolved: BTreeMap::new(),
        associations: BTreeMap::new(),
        exclusions: BTreeMap::new(),
        complete: true,
    };
    (state, page)
}
