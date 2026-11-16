//  `OrbitError` classes are decided where a native error is translated and
//  carried by the variant: an error that merely quotes the text of a class is
//  not in it, and a classified error reads exactly as its unclassified twin.

#[test]
fn denied_io_classifies_as_read_only_and_reads_as_io_error() {
    for denied in [
        io::Error::from(io::ErrorKind::PermissionDenied),
        io::Error::from(io::ErrorKind::ReadOnlyFilesystem),
    ] {
        let text = denied.to_string();
        let error = OrbitError::from(denied);
        assert!(error.is_readonly_or_access_failure(), "{error:?}");
        assert_eq!(error.storage_layer(), Some(StorageLayer::Io));
        assert_eq!(
            error.to_string(),
            OrbitError::Io(text).to_string(),
            "classification must not change the message"
        );
    }
    let missing = OrbitError::from(io::Error::from(io::ErrorKind::NotFound));
    assert!(!missing.is_readonly_or_access_failure(), "{missing:?}");
    assert_eq!(missing.storage_layer(), Some(StorageLayer::Io));
}

#[test]
fn quoted_denial_text_is_not_a_read_only_failure() {
    for quoted in [
        OrbitError::Execution("child stderr: open /data: permission denied".into()),
        OrbitError::Store("attempt to write a readonly database".into()),
        OrbitError::Io("Read-only file system (os error 30)".into()),
        OrbitError::Migration("permission denied".into()),
    ] {
        assert!(!quoted.is_readonly_or_access_failure(), "{quoted:?}");
    }
}

#[test]
fn storage_classification_keeps_each_layer_message() {
    for (layer, plain) in [
        (StorageLayer::Io, OrbitError::Io("denied".into())),
        (StorageLayer::Store, OrbitError::Store("denied".into())),
        (
            StorageLayer::Migration,
            OrbitError::Migration("denied".into()),
        ),
    ] {
        let classified = OrbitError::storage(layer, true, "denied");
        assert!(classified.is_readonly_or_access_failure());
        assert_eq!(classified.storage_layer(), Some(layer));
        assert_eq!(classified.to_string(), plain.to_string());
        let unclassified = OrbitError::storage(layer, false, "denied");
        assert!(!unclassified.is_readonly_or_access_failure());
        assert_eq!(unclassified.to_string(), plain.to_string());
    }
}

#[test]
fn timeout_is_the_variant_not_the_words() {
    let timeout = OrbitError::ExecutionTimeout {
        timeout_ms: 5,
        message: "git fetch timed out after 5ms".into(),
    };
    assert!(timeout.is_timeout());
    assert_eq!(
        timeout.to_string(),
        OrbitError::Execution("git fetch timed out after 5ms".into()).to_string()
    );
    assert!(
        OrbitError::ProcessTimeout {
            timeout_ms: 5,
            detail: "git".into()
        }
        .is_timeout()
    );
    assert!(!OrbitError::Execution("git fetch timed out after 5ms".into()).is_timeout());
}

#[test]
fn claim_refusal_reads_as_invalid_input() {
    let refused = OrbitError::claim_refused(ClaimRefusalKind::HandoffAlreadyLanded);
    assert_eq!(
        refused.claim_refusal(),
        Some(ClaimRefusalKind::HandoffAlreadyLanded)
    );
    assert_eq!(
        refused.to_string(),
        OrbitError::InvalidInput(ClaimRefusalKind::HandoffAlreadyLanded.message().into())
            .to_string()
    );
}

#[test]
fn quoted_refusal_wording_is_not_a_claim_refusal() {
    for kind in [
        ClaimRefusalKind::StaleClaim,
        ClaimRefusalKind::HandoffAlreadyLanded,
        ClaimRefusalKind::UnresolvedMergeIntent,
    ] {
        let quoted = OrbitError::InvalidInput(kind.message().to_string());
        assert_eq!(quoted.claim_refusal(), None, "{kind:?}");
    }
}

#[test]
fn absorbed_candidate_preserves_the_execution_display_and_wire_contract() {
    let candidate = orbit_common::CandidateAbsorbed {
        head: "candidate".into(),
        base_sha: "base-sha".into(),
        base_ref: "agent-main".into(),
        covering_commit: "covering-sha".into(),
    };
    let legacy = OrbitError::Execution("[candidate_absorbed] git_rebase: conflict recovery left 'candidate' on base 'base-sha' with no candidate commit; covering commit covering-sha on 'agent-main' already carries the candidate's change, so verified no-diff settlement completes the task".into());
    let typed = OrbitError::CandidateAbsorbed(Box::new(candidate));
    assert_eq!(
        typed.to_string(),
        legacy.to_string(),
        "persisted absorbed-candidate diagnostics are a compatibility contract"
    );
    assert_eq!(
        serde_json::to_string(&typed).unwrap(),
        serde_json::to_string(&legacy).unwrap(),
        "the error wire shape must remain Execution(string)"
    );
}

#[test]
fn private_vcs_failure_preserves_the_execution_display_and_wire_contract() {
    let message = "completion_observation_unavailable: execution failed: private automation VCS \
                   pr.status failed: HTTP 502";
    let typed = OrbitError::PrivateVcs(Box::new(orbit_common::PrivateVcsFailure {
        operation: "pr.status".into(),
        class: orbit_common::PrivateVcsFailureClass::CompletionObservationUnavailable,
        message: message.into(),
        read_throttle: None,
    }));
    let legacy = OrbitError::Execution(message.into());
    assert_eq!(
        typed.to_string(),
        legacy.to_string(),
        "operators and final recovery evidence read the unchanged message"
    );
    assert_eq!(
        serde_json::to_string(&typed).unwrap(),
        serde_json::to_string(&legacy).unwrap(),
        "the error wire shape must remain Execution(string)"
    );
    assert!(legacy.private_vcs_failure().is_none());
}

#[test]
fn preserved_json_rpc_refusal_classifies_as_owner_route_unavailable() {
    use orbit_types::workflow::StepFailureCode;

    for rpc_code in [orbit_common::JSON_RPC_INVALID_PARAMS_CODE, -32601, -32000] {
        let rpc_error = json!({
            "code": rpc_code,
            "message": "legacy owner rejection",
        });
        let preserved =
            OrbitError::json_rpc_refusal("owner-machine", "orbit.drain.claim.bind", &rpc_error);
        assert!(preserved.is_json_rpc_refusal());
        let step_code = preserved
            .step_failure_code()
            .expect("preserved refusal must yield a step failure code");
        assert_eq!(
            step_code,
            StepFailureCode::OwnerRouteUnavailable,
            "preserved JSON-RPC refusal with code {rpc_code} must classify as owner-route unavailable"
        );
        assert!(step_code.is_owner_route_unavailable());
    }

    for wire_code in ["invalid_input", "not_found", "custom_tool_code"] {
        let ordinary = OrbitError::RemoteTool {
            code: orbit_common::RemoteToolCode::from(wire_code),
            message: "tool failed".to_string(),
            payload: json!({
                "code": wire_code,
                "message": "tool failed",
            }),
        };
        assert!(!ordinary.is_json_rpc_refusal());
        let step_code = ordinary
            .step_failure_code()
            .expect("remote tool error must yield a step failure code");
        assert_eq!(
            step_code,
            StepFailureCode::parse(wire_code),
            "ordinary remote tool answer must contribute its wire code verbatim"
        );
        assert!(!step_code.is_owner_route_unavailable());
    }
}

/// The blocked-note essay the owner quoted: one line names the spec change
/// and one next step, and the three remedy spellings stay in the detail.
/// The stored error's Display text is not rewritten.
#[test]
fn source_repair_reader_line_names_one_next_step_and_keeps_the_diagnostic() {
    let essay = "source repair refused: the task specification changed after its run published \
         pull request #9722, so that candidate no longer implements this task. Nothing was \
         changed. The requeue resumes the pull request its run published once it is open at \
         the recorded head with every required check passed: requeue again when they pass. \
         If the pull request already merged, settle the task with `orbit task settle-merged`. \
         Otherwise, discard the candidate with `orbit task update ORB-1 --status backlog \
         --discard-candidate` on the operator CLI (orbit.task.update MCP does not accept \
         `discard_candidate`), or requeue the blocked task with the discard option (the \
         dashboard's requeue, or `orbit task requeue ORB-1 --discard-candidate`), to close \
         it and let the next run implement fresh.";
    let stored = OrbitError::InvalidInput(essay.to_string());
    let stored_text = stored.to_string();
    assert!(
        stored_text.contains("source repair refused: "),
        "stored text keeps the prefix: {stored_text}"
    );
    assert!(
        stored_text.contains("--discard-candidate") && stored_text.contains("settle-merged"),
        "stored text keeps the remedy spellings: {stored_text}"
    );
    assert_eq!(stored.wire_code(), "invalid_input");

    let shown = orbit_common::source_repair_reader_text(essay)
        .expect("a source-repair refusal has a reader line");
    assert_eq!(
        shown.line,
        "source repair refused: the spec changed after PR #9722 was published. Discard the candidate to rerun fresh."
    );
    assert!(
        !shown.line.contains('\n'),
        "reader line is one line: {}",
        shown.line
    );
    assert_eq!(shown.detail, essay);
    assert!(shown.detail.contains("settle-merged"), "{}", shown.detail);
    assert!(
        shown.detail.contains("--discard-candidate"),
        "{}",
        shown.detail
    );
    assert!(shown.detail.contains("dashboard"), "{}", shown.detail);

    let checks = "source repair refused: required checks failed on the recorded head: Check. \
         Nothing was changed. The requeue resumes the pull request once every required check \
         passed: requeue again when they pass. If the pull request already merged, settle the \
         task with `orbit task settle-merged`. Otherwise, discard the candidate.";
    let checks_line = orbit_common::source_repair_reader_text(checks).expect("checks refusal");
    assert_eq!(
        checks_line.line,
        "source repair refused: required checks failed on the recorded head: Check. Requeue when the required checks pass."
    );
    assert!(
        checks_line.detail.contains("settle-merged"),
        "{}",
        checks_line.detail
    );

    let running = "job validation failed: source repair refused: delivery `jrun-1` is still \
         running, so it cannot be repaired. Cancel it with `orbit run cancel jrun-1 --confirm`. \
         Nothing was changed. To discard the candidate with `orbit task update T --status \
         backlog --discard-candidate`.";
    let running_line = orbit_common::source_repair_reader_text(running).expect("running refusal");
    assert_eq!(
        running_line.line,
        "source repair refused: delivery `jrun-1` is still running, so it cannot be repaired. Cancel it with `orbit run cancel jrun-1 --confirm`."
    );
    assert!(
        running_line.detail.contains("--discard-candidate"),
        "{}",
        running_line.detail
    );

    let short =
        OrbitError::JobValidation("source repair refused: source checkpoints missing".into());
    let short_text = short.to_string();
    let short_line = short
        .source_repair_reader_text()
        .expect("short refusal still renders one line");
    assert_eq!(
        short_line.line,
        "source repair refused: source checkpoints missing. Inspect the source again before retrying."
    );
    assert_eq!(short_line.detail, short_text);
    assert!(
        short_text.contains("source repair refused: "),
        "{short_text}"
    );

    assert!(
        orbit_common::source_repair_reader_text("published candidate refused: binding changed")
            .is_none()
    );
    assert!(
        orbit_common::source_repair_reader_text("invalid input: the task specification changed")
            .is_none()
    );
}

/// The next step follows the cause it names. A discard the operator already
/// made is not offered again, and a closed pull request is not told to requeue
/// on checks alone: the requeue resumes only an open pull request.
#[test]
fn source_repair_reader_next_step_follows_the_cause_it_names() {
    let discarded = "source repair refused: an operator discarded this run's candidate. Nothing was \
         changed. To discard the candidate with `orbit task update T --status backlog \
         --discard-candidate`.";
    let shown = orbit_common::source_repair_reader_text(discarded).expect("refusal has a line");
    assert_eq!(
        shown.line,
        "source repair refused: an operator discarded this run's candidate. Inspect the source again before retrying."
    );
    assert_eq!(shown.detail, discarded);

    let closed = "source repair refused: pull request #12 is not open, or the forge cannot report its required checks";
    let shown = orbit_common::source_repair_reader_text(closed).expect("refusal has a line");
    assert_eq!(
        shown.line,
        "source repair refused: pull request #12 is not open, or the forge cannot report its required checks. Requeue when the pull request is open and its required checks pass."
    );
}
