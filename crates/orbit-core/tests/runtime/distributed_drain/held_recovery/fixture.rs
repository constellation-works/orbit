//! Authenticated claim, certificate and real Git fixture for recovery tests.
use super::*;

pub(super) const ASSERTION: &str = "sh src/cleanup-assertion.sh";

fn cleanup_assertion(repaired: bool) -> String {
    let cleanup = if repaired {
        "rm \"$1/owned\"; rmdir \"$1\""
    } else {
        "rmdir \"$1\""
    };
    format!(
        "set -eu\nprobe=.orbit/tmp/cleanup-assertion\nmkdir -p \"$probe\"\n: > \"$probe/owned\"\ncleanup() {{ {cleanup}; }}\ncleanup \"$probe\" || :\nif [ -d \"$probe\" ]; then\n  echo 'assertion failed: cleanup left its owned directory' >&2\n  exit 1\nfi\necho 'cleanup assertion passed'\n"
    )
}

pub(super) struct Fixture {
    pub(super) leaf: ReviewedLeaf,
    pub(super) original: HeldRecoveryMeaning,
    pub(super) branch: String,
    pub(super) answer: PathBuf,
    _operator: orbit_common::test_env::ScopedEnv,
    pub(super) identity: orbit_core::ActorIdentity,
}
impl Fixture {
    pub(super) fn new(evidence: bool) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let mut leaf = ReviewedLeaf::admit_from(
            &format!(
                "[review]\nbefore_landing = true\n[operation]\nreview_crew = \"{REVIEW_CREW}\"\n[workflow]\nrequired_validation_commands = [\"{}\"]\n",
                if evidence { "true" } else { ASSERTION }
            ),
            "",
        );
        leaf.gate_input["before_landing"] = json!(true);
        git(
            &leaf.pair.follower_repo,
            &["config", "user.name", "Recovery fixture"],
        );
        git(
            &leaf.pair.follower_repo,
            &["config", "user.email", "fixture@example.test"],
        );
        if !evidence {
            std::fs::write(
                leaf.pair.follower_repo.join("src/cleanup-assertion.sh"),
                cleanup_assertion(false),
            )
            .unwrap();
            git(
                &leaf.pair.follower_repo,
                &["add", "src/cleanup-assertion.sh"],
            );
            git(
                &leaf.pair.follower_repo,
                &["commit", "-qm", "Add the failing cleanup assertion"],
            );
            let failed = std::process::Command::new("sh")
                .arg("src/cleanup-assertion.sh")
                .current_dir(&leaf.pair.follower_repo)
                .output()
                .unwrap();
            assert!(
                !failed.status.success(),
                "the original cleanup must actually fail its assertion"
            );
            assert!(String::from_utf8_lossy(&failed.stderr).contains("assertion failed:"));
        }
        let branch = format!("orbit/{}-retained", leaf.task);
        git(&leaf.pair.follower_repo, &["branch", "-m", &branch]);
        let (root, origin) = publish_origin(&leaf);
        git(&leaf.pair.follower_repo, &["push", "-q", "origin", &branch]);
        owner_checkout(&leaf, &origin);
        for repo in [&leaf.pair.follower_repo, &leaf.pair.owner_repo] {
            git(repo, &["remote", "set-url", "origin", URL]);
            git(
                repo,
                &[
                    "config",
                    &format!("url.{}.insteadOf", origin.display()),
                    URL,
                ],
            );
            git(repo, &["fetch", "-q", "origin", &branch]);
        }
        let head = orbit_engine::review_gate::revision(&leaf.pair.follower_repo, "HEAD").unwrap();
        let answer = root.join("pr.json");
        std::fs::write(
            &answer,
            json!({"number":41,"url":"https://github.com/owner/repository/pull/41",
            "state":"OPEN","headRefName":branch,"headRefOid":head.commit,"baseRefName":"main",
            "autoMergeRequest":null,"mergedAt":null,"mergeCommit":null,"isDraft":false})
            .to_string(),
        )
        .unwrap();
        let bin = PathBuf::from(std::env::var("HOME").unwrap()).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("gh"),
            format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n'pr view') cat '{}' ;;\n*) exit 99 ;;\nesac\n",
                answer.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        leaf.bound
            .apply_task_automation_update(
                &leaf.task,
                orbit_engine::TaskAutomationUpdate {
                    external_refs: vec![orbit_types::task::ExternalRef {
                        system: "github-pr".into(),
                        id: "41".into(),
                        url: Some("https://github.com/owner/repository/pull/41".into()),
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        let admitted = leaf.admit_review();
        let extra = if evidence {
            record("V2", "external native assertion", ValidationOutcome::NotRun)
        } else {
            record("V1", ASSERTION, ValidationOutcome::Failed)
        };
        let mut reviewer = report(
            &admitted,
            ReviewVerdict::Incomplete,
            vec![],
            if evidence {
                vec![record("V1", "true", ValidationOutcome::Passed), extra]
            } else {
                vec![extra]
            },
            Some("The exact native assertion cannot complete on this host."),
        );
        if evidence {
            reviewer.external_evidence = vec![ReviewEvidenceRequirement {
                kind: ReviewEvidenceKind::NativeOs,
                name: "Native assertion".into(),
                command: "external native assertion".into(),
                artifact: "evidence/native.json".into(),
                os: None,
                hosted_check: None,
            }];
        }
        leaf.put_report(&reviewer);
        leaf.settle()
            .expect_err("the original candidate cannot pass without its assertion");
        let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
        assert!(!certificate.verdict.passed());
        let original =
            HeldRecoveryMeaning::from_task(&leaf.pair.wire.owner.get_task(&leaf.task).unwrap());
        let (claim_id, worker) = leaf.claim();
        let candidate = ClaimCandidateRef {
            held_recovery: None,
            branch: branch.clone(),
            head_sha: head.commit,
            pull_request: Some("41".into()),
            pull_request_head: Some(branch.clone()),
            source_run_id: Some(leaf.leaf.clone()),
            failed_step_id: Some("landing_review_gate_settle".into()),
            published: true,
            durable_ref: None,
            carry_failure: None,
            interrupted_review: None,
        };
        let mutation = if evidence {
            let hold: ReviewEvidenceHold = artifact(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).unwrap();
            ClaimMutation::Release(ClaimEvidence {
                summary: Some("review holds for external proof".into()),
                evidence_hold: Some(hold),
                held_pull_request: Some(orbit_store::contracts::ClaimPullRequest {
                    number: "41".into(),
                    head: branch.clone(),
                }),
                ..Default::default()
            })
        } else {
            ClaimMutation::Fail(ClaimEvidence {
                summary: Some("fixture assertion failed".into()),
                failure: Some(ClaimFailure {
                    class: orbit_types::workflow::ClaimFailureClass::Candidate,
                    reason: "fixture assertion failed".into(),
                    crew: Some("sol".into()),
                    candidate: Some(candidate),
                    provider_limit: false,
                    provider_refusal: false,
                }),
                ..Default::default()
            })
        };
        let _ = claim_id;
        leaf.pair
            .wire
            .owner
            .mutate_execution_claim(Some(&worker), "terminal-recovery-fixture", &mutation)
            .unwrap();
        leaf.pair
            .follower_jobs
            .finalize_job_run(&leaf.leaf, JobRunState::Held, Utc::now(), None)
            .unwrap();
        leaf.pair
            .advance(&leaf.leaf, LocalPullMutation::Settle(Box::new(mutation)));
        leaf.pair.advance(&leaf.leaf, LocalPullMutation::Settled);
        let identity = orbit_core::ActorIdentity::from_env();
        let operator = orbit_common::test_env::scoped([("ORBIT_OPERATOR", Some("1"))]);
        Self {
            leaf,
            original,
            branch,
            answer,
            _operator: operator,
            identity,
        }
    }
    pub(super) fn advance_base(&self) {
        let repo = &self.leaf.pair.follower_repo;
        git(repo, &["checkout", "-q", "main"]);
        std::fs::write(
            repo.join("base-advance.txt"),
            "unrelated integration work\n",
        )
        .unwrap();
        git(repo, &["add", "base-advance.txt"]);
        git(repo, &["commit", "-qm", "advance integration base"]);
        git(repo, &["push", "-q", "origin", "main"]);
        git(repo, &["checkout", "-q", &self.branch]);
        git(
            &self.leaf.pair.owner_repo,
            &["fetch", "-q", "origin", "main"],
        );
    }
    pub(super) fn inspect(&self, disposition: &str) -> Value {
        self.leaf.pair.wire.owner.run_tool_as_human("orbit.task.review_recover",json!({"id":self.leaf.task,"action":"inspect","disposition":disposition,"original":self.original,
            "repair_selectors":if disposition == "source_repair" {json!(["file:src/f0.rs", "file:src/cleanup-assertion.sh"])} else {json!([])}})).unwrap()
    }
    pub(super) fn repair_source(&self, checkout: &Path) {
        std::fs::write(checkout.join("src/f0.rs"), "fn work() {}\n").unwrap();
        std::fs::write(
            checkout.join("src/cleanup-assertion.sh"),
            cleanup_assertion(true),
        )
        .unwrap();
    }
    pub(super) fn amendment(&self) {
        let mut selectors = self.original.context_files.clone();
        selectors.push("file:src/cleanup-assertion.sh".into());
        self.leaf.pair.wire.owner.update_task_as_human(&self.leaf.task,orbit_core::application::task::TaskUpdateParams {
            description:Some("Exercise the same cleanup guarantee with a real confined assertion; retain negative incomplete-evidence controls.".into()),
            context_files:Some(selectors),
            ..Default::default()
        },"human:fixture".into()).unwrap();
    }
    pub(super) fn submit(&self, binding: &Value) -> Value {
        json!({"id":self.leaf.task,"action":"submit","request_key":"audited-repair","reason":"Approved the exact prospective contract and scope; all original evidence remains rejected.","binding":binding})
    }
}
