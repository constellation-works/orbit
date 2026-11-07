//! A claimed leaf applies the tag instructions its owner captured in the
//! claim's receipt, not its replica's own `workflow.tag_instructions`. Per-host
//! `.orbit/` configuration is untracked and drifts, so the replica's mapping
//! could otherwise add or drop instructions the owner's runs would not.

use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::{ShipMode, TagInstructionRole, TagInstructionSource};

use super::claimed_review::{ReviewedLeaf, revision};
use super::*;

const OWNER: &str = "[workflow.tag_instructions.no-diff-expected]\nimplement = false\n";
/// A mapping the replica cannot read: consulting it would refuse the task.
const REPLICA: &str = "[workflow.tag_instructions.no-diff-expected]\nimplement = \"missing.md\"\n";

fn implement_tags(host: &OrbitRuntime, task: &str) -> Vec<String> {
    RuntimeHost::tag_instructions_for_activity(
        host,
        "jrun-tag-instructions",
        "agent_implement",
        &json!({ "task_id": task }),
    )
    .expect("tag instructions resolve")
    .into_iter()
    .map(|instruction| instruction.tag)
    .collect()
}

fn a_claimed_leaf_applies_the_owners_captured_tag_instructions() {
    // A claimed task's tags are frozen, so tag it before the leaf claims it.
    let leaf = ReviewedLeaf::admit_launched(OWNER, REPLICA, |pair, drain| {
        pair.wire
            .owner
            .update_task_as_human(
                &pair.tasks[0],
                orbit_core::application::task::TaskUpdateParams {
                    tags: Some(vec!["no-diff-expected".into()]),
                    ..Default::default()
                },
                "fixture operator".into(),
            )
            .unwrap();
        pair.launched_leaf(drain, 1, std::process::id())
    });
    let captured = leaf
        .pair
        .admission(&leaf.leaf)
        .receipt
        .unwrap()
        .tag_instructions
        .expect("the owner captures its tag instructions");
    assert!(
        captured.instructions.iter().all(|instruction| {
            !(instruction.tag == "no-diff-expected"
                && instruction.role == TagInstructionRole::Implement)
        }),
        "a default the owner disabled is not captured"
    );

    assert!(
        implement_tags(&leaf.bound, &leaf.task).is_empty(),
        "the leaf applies the owner's mapping, not the replica's"
    );
    assert!(
        implement_tags(&leaf.pair.wire.owner, &leaf.task).is_empty(),
        "the owner's own runs apply the same instructions"
    );
}

crate::scenario_table!(
    a_claimed_leaf_applies_the_owners_captured_tag_instructions_scenarios,
    a_claimed_leaf_applies_the_owners_captured_tag_instructions,
);

/// Route either a replica or owner-local worker through the public owner bridge.
struct CaptureOwner(OrbitRuntime);

impl OwnerCoordinator for CaptureOwner {
    fn call(
        &self,
        name: &str,
        mut input: Value,
        mut session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let binding = session.worker_invocation.as_ref().unwrap();
        input["workspace"] = json!(binding.owner_workspace_id);
        session.workspace = Some(binding.owner_workspace_id.clone());
        session.caller_machine_id = Some(binding.execution.machine_id.clone());
        session.transport = Some(if binding.execution.machine_id == super::OWNER {
            McpTransport::Local
        } else {
            McpTransport::SshMcp
        });
        session.effective_capabilities = BTreeSet::from([McpCapability::Agent]);
        self.0.execute_owner_coordination(name, input, session)
    }
}

/// Both blobs and refs are fixed before the public pull; no fetch timing or
/// live remote is needed to reproduce stale local guidance.
fn a_pull_captures_the_shipping_modes_authoritative_tag_instructions() {
    for (mode, base, remote_present) in [
        (ShipMode::Pr, "main", true),
        (ShipMode::Pr, "origin/main", true),
        (ShipMode::Local, "main", true),
        (ShipMode::Local, "origin/main", true),
        (ShipMode::Pr, "main", false),
    ] {
        let config = "[workflow.tag_instructions.changed]\nimplement = \"changed.md\"\n\
                      [workflow.tag_instructions.added]\nimplement = \"added.md\"\n";
        let pair = Pair::with_owner_host(
            config,
            "[workflow.tag_instructions.changed]\nimplement = \"missing.md\"\n\
             [workflow.tag_instructions.added]\nimplement = false\n",
            &[None],
            |owner| {
                let repo = owner.paths().repo_root.clone();
                calm_host(
                    OrbitRuntime::from_roots_with_binding(
                        &owner.global_root(),
                        &repo.join(".orbit"),
                        orbit_core::WorkspaceRuntimeBinding {
                            logical_workspace_id: owner.workspace_id().unwrap(),
                            task_partition_id: owner.workspace_id().unwrap(),
                            owner_machine_id: Some(super::OWNER.into()),
                            checkout_role: None,
                            repo_root: repo,
                            ship_mode: mode,
                            base_branch: Some(base.into()),
                        },
                    )
                    .unwrap()
                    .with_automation_machine_identity(Some(super::OWNER.into())),
                )
            },
        );
        let pair = if mode == ShipMode::Local {
            let local = Pair::follower_of(
                pair._root.clone(),
                pair.wire.owner.clone(),
                pair.owner_repo.clone(),
                pair.tasks.clone(),
                super::OWNER,
            );
            *local.wire.local.lock().unwrap() = true;
            local
        } else {
            pair
        };
        let repo = &pair.owner_repo;
        std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
        std::fs::write(repo.join("changed.md"), "Local instruction.\n").unwrap();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "Local base"]);
        let local = revision(repo, "HEAD").commit;
        git(repo, &["checkout", "-q", "-b", "current"]);
        std::fs::write(repo.join("changed.md"), "Current instruction.\n").unwrap();
        std::fs::write(repo.join("added.md"), "New instruction.\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "Remote base"]);
        let remote = revision(repo, "HEAD").commit;
        if remote_present {
            git(repo, &["update-ref", "refs/remotes/origin/main", &remote]);
        }
        // HEAD is deliberately neither base, so it cannot mask a bad lookup.
        std::fs::write(repo.join("changed.md"), "Unrelated HEAD.\n").unwrap();
        git(repo, &["commit", "-q", "-am", "Unrelated HEAD"]);
        pair.wire
            .owner
            .update_task_as_human(
                &pair.tasks[0],
                orbit_core::application::task::TaskUpdateParams {
                    tags: Some(if mode == ShipMode::Local {
                        vec!["changed".into()]
                    } else {
                        vec!["changed".into(), "added".into()]
                    }),
                    ..Default::default()
                },
                "fixture operator".into(),
            )
            .unwrap();
        let drain = pair.run_drain();
        let leaf = pair.launched_leaf(&drain, 1, std::process::id());
        let record = pair.admission(&leaf);
        let receipt = record.receipt.unwrap();
        let captured = receipt.tag_instructions.unwrap();
        let claim = receipt.claim.unwrap();
        let bound = pair
            .follower
            .clone()
            .with_worker_invocation(
                WorkerInvocation {
                    owner_machine_id: super::OWNER.into(),
                    owner_workspace_id: record.destination.owner_workspace_id,
                    owner_destination: record.destination.selector,
                    task_id: claim.task_id.clone(),
                    claim_id: claim.claim_id,
                    execution: claim.executed_on,
                    bound_run_id: leaf.clone(),
                },
                Arc::new(CaptureOwner(pair.wire.owner.clone())),
            )
            .unwrap();
        let received = RuntimeHost::tag_instructions_for_activity(
            &bound,
            &leaf,
            "agent_implement",
            &json!({"task_id": claim.task_id}),
        );
        if mode == ShipMode::Pr && !remote_present {
            assert_eq!(
                captured.revision, None,
                "missing PR base cannot use local or HEAD"
            );
            assert_eq!(captured.refusals.len(), 2);
            assert!(
                received.is_err(),
                "missing authoritative ref refuses the activity"
            );
            continue;
        }
        let expected = if mode == ShipMode::Pr {
            &remote
        } else {
            &local
        };
        assert_eq!(
            captured.revision.as_ref(),
            Some(expected),
            "{mode:?}: {base}"
        );
        let changed = captured
            .instructions
            .iter()
            .find(|i| i.tag == "changed")
            .unwrap();
        assert_eq!(
            changed.text,
            if mode == ShipMode::Pr {
                "Current instruction.\n"
            } else {
                "Local instruction.\n"
            }
        );
        assert_eq!(
            changed.source,
            TagInstructionSource::Workspace {
                path: "changed.md".into(),
                revision: expected.clone(),
            }
        );
        if mode == ShipMode::Pr {
            assert!(captured.refusals.is_empty(), "new PR-base file is readable");
            let added = captured
                .instructions
                .iter()
                .find(|i| i.tag == "added")
                .unwrap();
            assert_eq!(added.text, "New instruction.\n");
            assert_eq!(
                added.source,
                TagInstructionSource::Workspace {
                    path: "added.md".into(),
                    revision: remote.clone(),
                }
            );
            // The replica has neither mapped file and a different mapping.
            assert!(!pair.follower_repo.join("added.md").exists());
            let received = received.expect("captured owner text reaches the claimed activity");
            assert_eq!(received.len(), 2);
            assert!(received.contains(changed));
            assert!(received.contains(added));
        } else {
            assert_eq!(
                captured.refusals.len(),
                1,
                "added file is absent on local base"
            );
            assert_eq!(captured.refusals[0].tag, "added");
            assert_eq!(
                received.expect("local claimed activity uses its captured local base"),
                vec![changed.clone()],
                "the unused added-file refusal does not affect the local task",
            );
        }
    }
}

crate::scenario_table!(
    authoritative_claimed_tag_instructions_scenarios,
    a_pull_captures_the_shipping_modes_authoritative_tag_instructions,
);
