//! `OrbitError` classes are decided where a native error is translated and
//! carried by the variant: an error that merely quotes the text of a class is
//! not in it, and a classified error reads exactly as its unclassified twin.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::io;

use orbit_common::{ClaimRefusalKind, OrbitError, StorageLayer};

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
