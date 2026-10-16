//! A before-landing handoff the owner merged after its landing branch moved
//! is still covered when the provider merged the reviewed head and the
//! landing is exactly the reviewed change carried cleanly onto the moved base
//! [ORB-15193]. A conflicting base change or a head pushed after the review
//! stays an after-landing obligation.

use super::before_pr::{ReviewedHandoff, revision};
use super::*;

use orbit_automation::review::REBASED_CLEAN_ASSURANCE;
use orbit_types::workflow::automation::AutomationState;

/// Land an unrelated change on `main` as pull request #41 after the review
/// settled, so the reviewed base is no longer the landing base.
fn advance_main(reviewed: &ReviewedHandoff, file: &str, content: &str) {
    std::fs::write(reviewed.repo.join(file), content).unwrap();
    git(&reviewed.repo, &["add", "-A"]);
    git(&reviewed.repo, &["commit", "-q", "-m", "Unrelated #41"]);
    let head = git(&reviewed.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    reviewed.merged_pull_request(41, &head);
}

fn pending_keys(state: &AutomationState) -> Vec<&str> {
    state
        .pending
        .iter()
        .map(|delivery| delivery.key.as_str())
        .collect()
}

/// The reviewed head squash-lands onto a base an unrelated pull request
/// advanced. The landing is the conflict-free merge of the reviewed change
/// onto that base, so the after-landing consumer excludes it under the
/// distinct `rebased_clean` assurance and queues only the unrelated landing.
fn a_before_landing_handoff_squashed_onto_a_moved_base_is_excluded_as_rebased_clean() {
    let reviewed = ReviewedHandoff::accept(ReviewTiming::BeforeLanding);
    advance_main(&reviewed, "src/other.rs", "fn other() {}\n");
    let landed = reviewed.squash_merge(42, &reviewed.candidate.commit);
    assert_ne!(
        revision(&reviewed.repo, &landed).tree,
        reviewed.candidate.tree,
        "the landing is not the reviewed tree byte for byte"
    );

    let state = reviewed.observe();
    assert_eq!(
        pending_keys(&state),
        vec!["pr:owner/repository:main:41"],
        "{state:#?}"
    );
    let [excluded] = state.excluded.as_slice() else {
        panic!("one exclusion expected: {state:#?}");
    };
    assert_eq!(excluded.delivery.key, "pr:owner/repository:main:42");
    assert_eq!(excluded.delivery.after.commit, landed);
    assert_eq!(excluded.exclusion.assurance, REBASED_CLEAN_ASSURANCE);
    assert_eq!(
        excluded.exclusion.attempt_id,
        reviewed.certificate.attempt_id
    );
    assert_eq!(
        excluded.exclusion.final_candidate_tree,
        reviewed.candidate.tree
    );
}

/// Neither a base change the reviewed patch conflicts with, resolved by hand
/// at landing, nor a head pushed after the review settled is covered by the
/// rebased-clean rule: both landings stay pending for after-landing review.
fn a_conflicting_base_or_a_head_pushed_after_review_stays_pending() {
    // The base rewrote the line the reviewed patch extends; the landing is a
    // hand resolution the reviewer never saw, under the reviewed head.
    let reviewed = ReviewedHandoff::accept(ReviewTiming::BeforeLanding);
    advance_main(&reviewed, "src/work.rs", "fn work() { base() }\n");
    std::fs::write(
        reviewed.repo.join("src/work.rs"),
        "fn work() { base() }\n// reviewed\n",
    )
    .unwrap();
    git(&reviewed.repo, &["commit", "-q", "-am", "Squash-merge #42"]);
    reviewed.merged_pull_request(42, &reviewed.candidate.commit);
    let state = reviewed.observe();
    assert!(state.excluded.is_empty(), "conflict: {state:#?}");
    assert_eq!(
        pending_keys(&state),
        vec!["pr:owner/repository:main:41", "pr:owner/repository:main:42"],
        "conflict: {state:#?}"
    );

    // A commit pushed after the review leaves the reviewed tree unchanged,
    // so the landing equals the clean merge; the provider merged a head that
    // was never reviewed, and that alone keeps it pending.
    let reviewed = ReviewedHandoff::accept(ReviewTiming::BeforeLanding);
    advance_main(&reviewed, "src/other.rs", "fn other() {}\n");
    git(&reviewed.repo, &["checkout", "-q", &reviewed.branch]);
    git(
        &reviewed.repo,
        &["commit", "-q", "--allow-empty", "-m", "Pushed after review"],
    );
    let later = git(&reviewed.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    git(&reviewed.repo, &["checkout", "-q", "main"]);
    reviewed.squash_merge(42, &later);
    let state = reviewed.observe();
    assert!(state.excluded.is_empty(), "later head: {state:#?}");
    assert_eq!(
        pending_keys(&state),
        vec!["pr:owner/repository:main:41", "pr:owner/repository:main:42"],
        "later head: {state:#?}"
    );
}

/// Both a recorded stop and a terminal owner job whose attempt remains
/// dispatched retain the foreign before-landing certificate. Already-merged
/// readback completes without another merge or replacement of the leaf binding.
fn public_retry_reconciles_an_already_merged_native_handoff_without_merging_again() {
    for stop in [true, false] {
        retry_native_handoff(false, true, stop);
    }
}

fn public_retry_adopts_an_already_merged_clean_provider_base_update() {
    retry_native_handoff(true, true, true);
}

fn public_retry_adopts_a_clean_provider_base_update_and_lands_normally() {
    retry_native_handoff(true, false, true);
}

fn retry_native_handoff(base_update: bool, already_merged: bool, stop: bool) {
    let reviewed = ReviewedHandoff::accept_with_commands(
        ReviewTiming::BeforeLanding,
        vec!["test -f src/work.rs".into()],
    );
    let owner = &reviewed.owner;
    let console = owner.distributed_claim_console().unwrap();
    let claim = &console["claims"][0];
    let claim_id = claim["claim_id"].as_str().unwrap();
    let accepted = owner.accepted_task_handoff(claim_id).unwrap();
    let original = owner.get_task(&accepted.handoff.task_id).unwrap();
    assert_eq!(
        accepted.required_commands,
        owner.workflow_required_validation_commands()
    );
    assert_eq!(
        accepted.handoff.validation.len(),
        1,
        "native evidence is pinned before the provider moves the head"
    );

    let jobs = owner.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_landing_pipeline.yaml"),
        jobs.join("task_landing_pipeline.yaml"),
    )
    .unwrap();
    orbit_core::test_support::install_substitute_pipeline_worker(["true".to_string()]);
    owner
        .approve_handoff_as_operator(
            &accepted.handoff_id,
            &orbit_core::application::review::ExpectedCandidate {
                candidate_commit: accepted.handoff.candidate.candidate.commit.clone(),
                base_commit: accepted.handoff.candidate.base.commit.clone(),
            },
            "operator",
            "approve-native-retry",
        )
        .unwrap();
    if stop {
        RuntimeHost::record_handoff_landing(
            owner,
            &orbit_engine::HandoffLandingUpdate {
                handoff_id: accepted.handoff_id.clone(),
                step: orbit_engine::HandoffLandingStep::Stop { repairable: false },
                observed: None,
                evidence: "provider review was not satisfied at the first owner attempt".into(),
            },
        )
        .unwrap();
    }
    let stopped = owner.landing_attempts().unwrap().remove(0);
    orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    )
    .finalize_job_run(
        stopped.job_run_id.as_deref().unwrap(),
        JobRunState::Cancelled,
        Utc::now(),
        None,
    )
    .unwrap();

    let automatic = !base_update && !already_merged;
    if automatic {
        advance_main(
            &reviewed,
            ".github/workflows/ci.yml",
            "name: CI\non:\n  pull_request:\n    branches: [main]\njobs:\n  protected:\n    name: Protected (${{ matrix.language }})\n    strategy:\n      matrix:\n        language: [rust]\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo check\n",
        );
    }
    let provider_head = if base_update {
        advance_main(&reviewed, "src/other.rs", "fn other() {}\n");
        git(&reviewed.repo, &["checkout", "-q", &reviewed.branch]);
        git(
            &reviewed.repo,
            &["merge", "--no-ff", "-qm", "Provider base update", "main"],
        );
        let head = revision(&reviewed.repo, "HEAD").commit;
        git(&reviewed.repo, &["checkout", "-q", "main"]);
        head
    } else {
        reviewed.candidate.commit.clone()
    };
    let merge = already_merged.then(|| reviewed.squash_merge(42, &provider_head));
    // Preserve the provider slug when Git's fixture redirect resolves its URL.
    let remote = reviewed.repo.parent().unwrap().join("owner/repository.git");
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
    git(
        remote.parent().unwrap(),
        &[
            "clone",
            "-q",
            "--bare",
            reviewed.repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    git(
        &reviewed.repo,
        &[
            "config",
            "--unset",
            &format!(
                "url.{}.insteadOf",
                reviewed.repo.with_file_name("origin.git").display()
            ),
        ],
    );
    git(
        &reviewed.repo,
        &[
            "config",
            &format!("url.{}.insteadOf", remote.display()),
            "https://github.com/owner/repository.git",
        ],
    );
    let provider = PathBuf::from(std::env::var("HOME").unwrap()).join("bin/gh");
    let response = provider.with_extension("json");
    std::fs::write(&response, json!({"number":42,"state":"MERGED","mergedAt":"2026-10-14T16:34:09Z","headRefName":reviewed.branch,"headRefOid":provider_head,"baseRefName":"main","mergeCommit":{"oid":merge},"url":"https://github.com/owner/repository/pull/42","mergeStateStatus":"UNKNOWN","reviewDecision":"","statusCheckRollup":[]}).to_string()).unwrap();
    // Every command is captured; this forge refuses any mutation, including
    // pr merge, rerun, branch update or an administrative bypass.
    std::fs::write(&provider, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}.calls'\n[ \"$1 $2 $3\" = 'pr view 42' ] || exit 1\nexec cat '{}'\n", provider.display(), response.display())).unwrap();
    if !already_merged {
        let forge = reviewed.repo.parent().unwrap().join("forge");
        std::fs::create_dir_all(&forge).unwrap();
        for (name, value) in [
            ("remote", remote.to_str().unwrap()),
            ("slug", "owner/repository"),
            ("pr-head", reviewed.branch.as_str()),
            ("pr-base", "main"),
            ("old-base", accepted.handoff.candidate.base.commit.as_str()),
        ] {
            std::fs::write(forge.join(name), value).unwrap();
        }
        if base_update {
            std::fs::write(forge.join("base-applied"), "yes").unwrap();
        }
        if automatic {
            let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
                &orbit_store::maintenance::task_registry::task_registry_path(&owner.global_root()),
            )
            .unwrap();
            let binding = registry
                .find_task_binding(&accepted.handoff.task_id)
                .unwrap()
                .unwrap();
            std::fs::write(
                forge.join("owner-events"),
                binding
                    .canonical_path
                    .join(orbit_types::task::TASK_EVENTS_FILE_NAME)
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
        }
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../orbit-engine/tests/engine/pr_landing/fixtures/base-refresh-forge.sh"),
        )
        .unwrap();
        std::fs::write(
            &provider,
            script.replace("__FORGE__", forge.to_str().unwrap()),
        )
        .unwrap();
    }
    let request = json!({
        "workspace": owner.workspace_id().unwrap(), "handoff_id": accepted.handoff_id,
        "expected_candidate_commit": accepted.handoff.candidate.candidate.commit,
        "expected_base_commit": accepted.handoff.candidate.base.commit,
        "expected_latest_attempt": stopped.attempt, "expected_latest_state":stopped.state,
        "reason":"provider reports the unchanged native head already merged", "request_key":"native-reconcile-1",
    });
    if base_update {
        assert!(
            orbit_engine::observe_published_candidate(
                owner,
                &reviewed.repo,
                &accepted.handoff.candidate
            )
            .is_err(),
            "initial acceptance never adopts a different head"
        );
    }
    if base_update && already_merged {
        let pristine: Value = serde_json::from_slice(&std::fs::read(&response).unwrap()).unwrap();
        let base_side = git(
            &reviewed.repo,
            &["rev-parse", &format!("{provider_head}^2")],
        )
        .trim()
        .to_string();
        let non_merge = git(
            &reviewed.repo,
            &[
                "commit-tree",
                &reviewed.candidate.tree,
                "-p",
                &reviewed.candidate.commit,
                "-m",
                "Unreviewed source",
            ],
        )
        .trim()
        .to_string();
        let dirty = git(
            &reviewed.repo,
            &[
                "commit-tree",
                &reviewed.candidate.tree,
                "-p",
                &reviewed.candidate.commit,
                "-p",
                &base_side,
                "-m",
                "Dirty provider tree",
            ],
        )
        .trim()
        .to_string();
        let foreign_parent = git(
            &reviewed.repo,
            &[
                "commit-tree",
                &reviewed.candidate.tree,
                "-p",
                &base_side,
                "-m",
                "Foreign base",
            ],
        )
        .trim()
        .to_string();
        let multiple = git(
            &reviewed.repo,
            &[
                "commit-tree",
                &reviewed.candidate.tree,
                "-p",
                &reviewed.candidate.commit,
                "-p",
                &base_side,
                "-p",
                &foreign_parent,
                "-m",
                "Three parents",
            ],
        )
        .trim()
        .to_string();
        let foreign_tree = git(
            &reviewed.repo,
            &[
                "merge-tree",
                "--write-tree",
                &reviewed.candidate.commit,
                &foreign_parent,
            ],
        )
        .trim()
        .to_string();
        let foreign = git(
            &reviewed.repo,
            &[
                "commit-tree",
                &foreign_tree,
                "-p",
                &reviewed.candidate.commit,
                "-p",
                &foreign_parent,
                "-m",
                "Foreign base merge",
            ],
        )
        .trim()
        .to_string();
        for (field, value) in [
            ("headRefOid", json!(non_merge)),
            ("headRefOid", json!(dirty)),
            ("headRefOid", json!(multiple)),
            ("headRefOid", json!(foreign)),
            ("headRefName", json!("foreign/branch")),
            ("baseRefName", json!("foreign/base")),
            ("number", json!(43)),
            (
                "url",
                json!("https://github.com/foreign/repository/pull/42"),
            ),
            ("mergeCommit", Value::Null),
            ("mergeCommit", json!({"oid":"f".repeat(40)})),
            (
                "mergeCommit",
                json!({"oid":accepted.handoff.candidate.base.commit}),
            ),
        ] {
            let mut changed = pristine.clone();
            changed[field] = value;
            std::fs::write(&response, changed.to_string()).unwrap();
            assert!(
                owner
                    .run_tool_with_context_and_role(
                        "orbit.task.retry_handoff",
                        request.clone(),
                        Role::Admin,
                        ToolContext {
                            session_context: ToolSessionContext {
                                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    )
                    .is_err(),
                "{field} refuses before dispatch"
            );
            assert_eq!(
                owner.landing_attempts().unwrap()[0],
                stopped,
                "refusal preserves original stop"
            );
        }
        std::fs::write(&response, pristine.to_string()).unwrap();
        let origin = git(&reviewed.repo, &["config", "--get", "remote.origin.url"])
            .trim()
            .to_string();
        git(
            &reviewed.repo,
            &[
                "remote",
                "set-url",
                "origin",
                "/nonexistent/owner/repository.git",
            ],
        );
        assert!(
            owner
                .run_tool_with_context_and_role(
                    "orbit.task.retry_handoff",
                    request.clone(),
                    Role::Admin,
                    ToolContext {
                        session_context: ToolSessionContext {
                            effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                )
                .is_err(),
            "cached proof cannot replace a fresh successful fetch"
        );
        git(&reviewed.repo, &["remote", "set-url", "origin", &origin]);
        assert_eq!(owner.landing_attempts().unwrap()[0], stopped);
    }
    // Corrupt only this isolated fixture's certificate blob: the public retry
    // must re-read its pinned bytes. No handoff or authority row is rewritten.
    let manifest = owner
        .get_task_artifact_manifest(&accepted.handoff.task_id)
        .unwrap();
    let certificate = manifest
        .iter()
        .find(|file| file.path == orbit_types::workflow::REVIEW_GATE_ARTIFACT)
        .unwrap();
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&owner.global_root()),
    )
    .unwrap();
    let binding = registry
        .find_task_binding(&accepted.handoff.task_id)
        .unwrap()
        .unwrap();
    let blob = binding
        .canonical_path
        .join(orbit_types::task::TASK_ARTIFACTS_DIR_NAME)
        .join(&certificate.blob);
    let native = manifest
        .iter()
        .find(|file| file.path == "validation/native-gate-0.json")
        .unwrap();
    let native_blob = binding
        .canonical_path
        .join(orbit_types::task::TASK_ARTIFACTS_DIR_NAME)
        .join(&native.blob);
    let history_before = owner.get_task_history(&accepted.handoff.task_id).unwrap();
    let native_bytes = std::fs::read(&native_blob).unwrap();
    std::fs::write(&native_blob, b"{}").unwrap();
    let update_request = orbit_types::workflow::PrBaseUpdateReservation {
        execution: accepted.handoff_id.clone(),
        repository: accepted.handoff.candidate.repository.clone(),
        pr_number: "42".into(),
        head_sha: accepted.handoff.candidate.candidate.commit.clone(),
        head_branch: accepted.handoff.candidate.source_branch.clone(),
        base_branch: accepted.handoff.candidate.base_branch.clone(),
        base_sha: accepted.handoff.candidate.base.commit.clone(),
        deadline_ms: u64::MAX,
        evidence: json!({"cause":"fixture-authority-control"}),
    };
    assert!(
        RuntimeHost::admit_handoff_base_update(owner, &accepted.handoff_id, &update_request)
            .is_err(),
        "native evidence is rechecked before reserving an external base update"
    );

    assert!(
        owner
            .run_tool_with_context_and_role(
                "orbit.task.retry_handoff",
                request.clone(),
                Role::Admin,
                ToolContext {
                    session_context: ToolSessionContext {
                        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .is_err(),
        "unreadable pinned native evidence cannot dispatch a retry"
    );
    assert_eq!(owner.landing_attempts().unwrap()[0], stopped);
    std::fs::write(&native_blob, native_bytes).unwrap();
    assert_eq!(
        owner.get_task_history(&accepted.handoff.task_id).unwrap(),
        history_before,
        "refused authority spends no base-update reservation"
    );
    let certified_bytes = std::fs::read(&blob).unwrap();
    std::fs::write(&blob, b"{}").unwrap();
    assert!(
        owner
            .run_tool_with_context_and_role(
                "orbit.task.retry_handoff",
                request.clone(),
                Role::Admin,
                ToolContext {
                    session_context: ToolSessionContext {
                        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .is_err(),
        "an invalidated native certificate cannot reserve a retry"
    );
    std::fs::write(&blob, certified_bytes).unwrap();
    assert_eq!(owner.landing_attempts().unwrap()[0], stopped);
    let submitted = owner
        .run_tool_with_context_and_role(
            "orbit.task.retry_handoff",
            request.clone(),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    if stop {
        // Model a lost external reply via the normal trusted intent boundary.
        RuntimeHost::record_handoff_landing(
            owner,
            &orbit_engine::HandoffLandingUpdate {
                handoff_id: accepted.handoff_id.clone(),
                step: orbit_engine::HandoffLandingStep::PublishIntent {
                    intent_id: "native-merge-uncertain".into(),
                },
                observed: Some(accepted.handoff.candidate.clone()),
                evidence: "external merge reply was lost".into(),
            },
        )
        .unwrap();
    }
    let result = orbit_engine::execute_deterministic_action(
        owner,
        "handoff_land",
        &json!({}),
        &json!({"handoff_id":accepted.handoff_id,"run_id":submitted["run_id"]}),
        false,
        &Default::default(),
        None,
    )
    .unwrap();
    assert_eq!(result["phase"], "landed");
    let completed = owner.get_task(&accepted.handoff.task_id).unwrap();
    assert_eq!(completed.status.to_string(), "done");
    assert_eq!(completed.job_run_id, original.job_run_id);
    assert_eq!(completed.job_run_machine, original.job_run_machine);
    assert_eq!(owner.accepted_task_handoff(claim_id).unwrap(), accepted);
    let historical = owner
        .landing_attempt_record(&accepted.handoff_id, stopped.attempt)
        .unwrap()
        .unwrap();
    if stop {
        assert_eq!(historical, stopped);
    } else {
        assert_eq!(
            historical.state,
            orbit_types::workflow::handoff::LandingAttemptState::Stopped
        );
        assert_eq!(historical.job_run_id, stopped.job_run_id);
        assert_eq!(historical.claim_id, stopped.claim_id);
        assert_eq!(historical.authorization_id, stopped.authorization_id);
        assert!(
            historical
                .evidence
                .unwrap()
                .contains(stopped.job_run_id.as_deref().unwrap())
        );
    }
    let settled = owner.landing_attempts().unwrap();
    let replay = owner
        .run_tool_with_context_and_role(
            "orbit.task.retry_handoff",
            request,
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(replay["run_id"], submitted["run_id"]);
    assert_eq!(replay["submitted"], false);
    assert_eq!(owner.landing_attempts().unwrap(), settled);
    assert_eq!(
        owner
            .review_store()
            .unwrap()
            .review_certificate(&owner.workspace_id().unwrap(), "follower-attempt")
            .unwrap(),
        Some(reviewed.certificate.clone()),
        "the original native certificate is never copied onto the provider head"
    );
    if !already_merged {
        assert_eq!(
            std::fs::read_to_string(reviewed.repo.parent().unwrap().join("forge/mutations"))
                .unwrap(),
            if automatic {
                "update\nmerge\n"
            } else {
                "merge\n"
            },
            "one normal merge after the bounded automatic refresh or existing provider update"
        );
        if automatic {
            assert_eq!(
                orbit_types::workflow::pr_base_update_reservations(
                    &owner.get_task_history(&accepted.handoff.task_id).unwrap()
                )
                .unwrap()
                .len(),
                1,
                "the live claim's journal reserved before the provider update"
            );
        }
        return;
    }
    let calls = std::fs::read_to_string(provider.with_extension("calls")).unwrap_or_else(|_| {
        std::fs::read_to_string(format!("{}.calls", provider.display())).unwrap()
    });
    assert!(
        calls.lines().all(|call| call.starts_with("pr view 42 ")),
        "readback never repeats an external merge: {calls}"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_before_landing_handoff_squashed_onto_a_moved_base_is_excluded_as_rebased_clean_scenarios,
    a_before_landing_handoff_squashed_onto_a_moved_base_is_excluded_as_rebased_clean,
    a_conflicting_base_or_a_head_pushed_after_review_stays_pending,
    public_retry_reconciles_an_already_merged_native_handoff_without_merging_again
);
crate::scenario_table!(
    public_retry_provider_update_already_merged,
    public_retry_adopts_an_already_merged_clean_provider_base_update
);
crate::scenario_table!(
    public_retry_provider_update_lands,
    public_retry_adopts_a_clean_provider_base_update_and_lands_normally
);

fn base_update_admission_rechecks_current_native_handoff_authority() {
    use orbit_types::workflow::{PrBaseUpdateAdmission, PrBaseUpdateReservation};
    let reviewed = ReviewedHandoff::accept_with_commands(
        ReviewTiming::BeforeLanding,
        vec!["test -f src/work.rs".into()],
    );
    let owner = &reviewed.owner;
    let claim = owner.distributed_claim_console().unwrap()["claims"][0]["claim_id"]
        .as_str()
        .unwrap()
        .to_string();
    let accepted = owner.accepted_task_handoff(&claim).unwrap();
    let expected = orbit_core::application::review::ExpectedCandidate {
        candidate_commit: accepted.handoff.candidate.candidate.commit.clone(),
        base_commit: accepted.handoff.candidate.base.commit.clone(),
    };
    owner
        .approve_handoff_as_operator(
            &accepted.handoff_id,
            &expected,
            "operator",
            "approve-update-authority",
        )
        .unwrap();
    let request = PrBaseUpdateReservation {
        execution: accepted.handoff_id.clone(),
        repository: accepted.handoff.candidate.repository.clone(),
        pr_number: "42".into(),
        head_sha: accepted.handoff.candidate.candidate.commit.clone(),
        head_branch: accepted.handoff.candidate.source_branch.clone(),
        base_branch: accepted.handoff.candidate.base_branch.clone(),
        base_sha: accepted.handoff.candidate.base.commit.clone(),
        deadline_ms: u64::MAX,
        evidence: json!({"cause":"inherited","classification":"fixture inherited failure"}),
    };
    assert!(matches!(
        RuntimeHost::admit_handoff_base_update(owner, &accepted.handoff_id, &request).unwrap(),
        PrBaseUpdateAdmission::Reserved
    ));
    assert!(
        matches!(
            RuntimeHost::admit_handoff_base_update(owner, &accepted.handoff_id, &request).unwrap(),
            PrBaseUpdateAdmission::Observe(_)
        ),
        "authority recheck never grants another provider update"
    );
    owner
        .revoke_handoff_as_operator(
            &accepted.handoff_id,
            &expected,
            "operator",
            "authority withdrawn before provider call",
            "withdraw-update-authority",
        )
        .unwrap();
    let history = owner.get_task_history(&accepted.handoff.task_id).unwrap();
    assert!(
        RuntimeHost::admit_handoff_base_update(owner, &accepted.handoff_id, &request).is_err(),
        "revocation refuses even a reserved mutation before the provider call"
    );
    assert_eq!(
        owner.get_task_history(&accepted.handoff.task_id).unwrap(),
        history
    );
    assert_eq!(owner.accepted_task_handoff(&claim).unwrap(), accepted);
}
crate::scenario_table!(
    current_base_update_authority,
    base_update_admission_rechecks_current_native_handoff_authority
);

fn public_retry_refreshes_absent_contexts_under_the_accepted_native_authority() {
    retry_native_handoff(false, false, true);
}
crate::scenario_table!(
    public_retry_automatic_base_refresh,
    public_retry_refreshes_absent_contexts_under_the_accepted_native_authority
);
