//! Stable response tokens for cancellation and drain controls.

use orbit_core::application::contracts::PullSettlementOutcome;
use orbit_core::application::distributed::AdmissionReceiptLookupOutcome;
use orbit_core::application::job::{
    DrainAdmissionsStopOutcome, DrainWindowExtensionOutcome, DrainWorkerLimitOutcome,
    JobRunCancelOutcome,
};
use serde::{Serialize, de::DeserializeOwned};

fn check_tokens<T>(cases: &[(&str, T)])
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug + std::fmt::Display,
{
    for (token, expected) in cases {
        let wire = serde_json::json!(token);
        let parsed: T =
            serde_json::from_value(wire.clone()).expect("existing response token parses");
        assert_eq!(&parsed, expected);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
        assert_eq!(parsed.to_string(), *token, "terminal and JSON tokens agree");
    }
    assert!(serde_json::from_value::<T>(serde_json::json!("future_outcome")).is_err());
}

#[test]
fn response_outcomes_keep_every_documented_wire_token() {
    use JobRunCancelOutcome as Cancel;
    check_tokens(&[
        ("cancelled", Cancel::Cancelled),
        ("already_terminal", Cancel::AlreadyTerminal),
        ("cancelling", Cancel::Cancelling),
    ]);
    use DrainAdmissionsStopOutcome as Stop;
    check_tokens(&[
        ("idle", Stop::Idle),
        ("stopped", Stop::Stopped),
        ("unchanged", Stop::Unchanged),
        ("cancelled_queued", Stop::CancelledQueued),
        ("force_cancelled", Stop::ForceCancelled),
    ]);
    use DrainWindowExtensionOutcome as Extend;
    check_tokens(&[
        ("extended", Extend::Extended),
        ("unchanged", Extend::Unchanged),
    ]);
    use DrainWorkerLimitOutcome as Limit;
    check_tokens(&[("updated", Limit::Updated), ("unchanged", Limit::Unchanged)]);
    use AdmissionReceiptLookupOutcome as Lookup;
    check_tokens(&[
        ("found", Lookup::Found),
        ("expired", Lookup::Expired),
        ("not_found", Lookup::NotFound),
    ]);
    use PullSettlementOutcome as Settlement;
    check_tokens(&[
        ("settled", Settlement::Settled),
        ("closed_obsolete", Settlement::ClosedObsolete),
        ("leaf_running", Settlement::LeafRunning),
        ("pending_delivery", Settlement::PendingDelivery),
        ("settlement_refused", Settlement::SettlementRefused),
        ("release_held", Settlement::ReleaseHeld),
        ("owner_unreachable", Settlement::OwnerUnreachable),
        ("awaiting_drain", Settlement::AwaitingDrain),
        ("unanswered_request", Settlement::UnansweredRequest),
        ("launch_uncertain", Settlement::LaunchUncertain),
        ("idle", Settlement::Idle),
        ("refused", Settlement::Refused),
        ("pending", Settlement::Pending),
        ("no_owner_route", Settlement::NoOwnerRoute),
    ]);
}
