//! Every owner claim refusal kind reaches the console as its fixed refusal
//! code, decided from the error's variant rather than its wording.

use orbit_common::{ClaimRefusalKind, OrbitError};

use super::super::HandoffConsoleRefusal;

/// Each refusal kind and the console code its operator response depends on.
/// The match in the test fails to compile when a kind is added without a row
/// here.
const REFUSALS: [(ClaimRefusalKind, &str); 9] = [
    (ClaimRefusalKind::StaleClaim, "stale_claim"),
    (ClaimRefusalKind::NotCurrent, "handoff_not_current"),
    (ClaimRefusalKind::HandoffCandidateMismatch, "stale_claim"),
    (ClaimRefusalKind::HandoffIdentityMismatch, "stale_claim"),
    (
        ClaimRefusalKind::ValidationRequirementsChanged,
        "stale_claim",
    ),
    (ClaimRefusalKind::LandingAuthorityRevoked, "stale_claim"),
    (ClaimRefusalKind::HandoffAlreadyLanded, "stale_claim"),
    (
        ClaimRefusalKind::UnresolvedMergeIntent,
        "uncertain_merge_intent",
    ),
    (
        ClaimRefusalKind::MergeIntentReplayUnreconciled,
        "uncertain_merge_intent",
    ),
];

#[test]
fn refusal_kinds_map_to_console_codes_regardless_of_wording() {
    let mut tabled = Vec::new();
    for (kind, code) in REFUSALS {
        match kind {
            ClaimRefusalKind::StaleClaim
            | ClaimRefusalKind::NotCurrent
            | ClaimRefusalKind::HandoffCandidateMismatch
            | ClaimRefusalKind::HandoffIdentityMismatch
            | ClaimRefusalKind::ValidationRequirementsChanged
            | ClaimRefusalKind::LandingAuthorityRevoked
            | ClaimRefusalKind::HandoffAlreadyLanded
            | ClaimRefusalKind::UnresolvedMergeIntent
            | ClaimRefusalKind::MergeIntentReplayUnreconciled => {}
        }
        assert!(!tabled.contains(&kind), "{kind:?} is tabled once");
        tabled.push(kind);
        for message in [kind.message().to_string(), "reworded refusal".to_string()] {
            let error = OrbitError::ClaimRefused { kind, message };
            let refusal = error
                .claim_refusal()
                .map(HandoffConsoleRefusal::for_claim_refusal)
                .unwrap_or_else(|| panic!("{kind:?} must classify as a claim refusal"));
            assert_eq!(refusal.code(), code, "{kind:?}");
        }
    }
}
