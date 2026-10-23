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
