//! What the owner says about admitting this drain before it requests: the
//! probe, and whether this drain can run the review its ship contract
//! captured [ORB-13908].

use orbit_common::OrbitError;
use orbit_store::contracts::{AdmissionShipContract, PullDestination};
use serde_json::Value;

use super::crews::crew_window;
use crate::OrbitRuntime;

/// What the owner's probe said about admitting this executor now.
pub(super) struct ProbeVerdict {
    pub(super) ship: Option<AdmissionShipContract>,
    pub(super) refusal: Option<String>,
}

/// Ask the owner whether it would admit this executor now, and for the ship
/// contract a new request must carry. Declaring this binary's version, the
/// protocol schema and the drain's captured `review.before_pr` makes the
/// owner report the first refusal admission would raise, so a mismatch stops
/// new requests before any is persisted.
pub(super) fn probe(
    runtime: &OrbitRuntime,
    run_id: &str,
    transport: &std::sync::Arc<dyn orbit_tools::DrainOwnerTransport>,
    destination: &PullDestination,
) -> Result<ProbeVerdict, OrbitError> {
    let report = crate::application::distributed::probe_pull_contract(
        transport.as_ref(),
        &destination.selector,
        captured_before_pr(runtime, run_id)?,
    )?;
    if report.get("owner_machine_id").and_then(Value::as_str)
        != Some(destination.owner_machine_id.as_str())
    {
        return Ok(ProbeVerdict {
            ship: None,
            refusal: Some(format!(
                "owner answered as {:?}, not the bound owner '{}'",
                report.get("owner_machine_id"),
                destination.owner_machine_id
            )),
        });
    }
    let admits = report.get("admits").and_then(Value::as_bool) == Some(true);
    if !admits {
        let refusal = report
            .get("refusal")
            .and_then(Value::as_str)
            .unwrap_or("refused");
        let diagnostics = report
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        return Ok(ProbeVerdict {
            ship: None,
            refusal: Some(format!("{refusal}: {diagnostics}")),
        });
    }
    let ship = report
        .get("ship")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| OrbitError::Store(format!("owner probe ship contract: {error}")))?;
    if let Some(ship) = &ship
        && let Some(refusal) = reviewer_refusal(runtime, run_id, ship)?
    {
        return Ok(ProbeVerdict {
            ship: None,
            refusal: Some(refusal),
        });
    }
    Ok(ProbeVerdict {
        ship,
        refusal: None,
    })
}

/// Why this drain cannot run the before-PR or before-landing review the
/// owner's ship contract captured [ORB-13908]: the crew is unset, does not
/// resolve here, or the drain's window cannot run it. Claiming anyway would
/// only escalate the task at its gate, so the drain requests nothing until the
/// owner or this host changes, or until a capacity backoff on the crew lifts
/// by itself [ORB-15791].
fn reviewer_refusal(
    runtime: &OrbitRuntime,
    run_id: &str,
    ship: &AdmissionShipContract,
) -> Result<Option<String>, OrbitError> {
    if let Some(refusal) = runtime.claimed_review_refusal(ship) {
        return Ok(Some(refusal));
    }
    let Some(crew) = ship
        .review
        .as_ref()
        .and_then(|review| review.crew.as_deref())
    else {
        return Ok(None);
    };
    let timing = if ship.before_landing {
        "before-landing"
    } else {
        "before-PR"
    };
    // The window without its `--allow-crew` restriction: that selects the
    // claims' implementation crews, not the review each one owes.
    Ok(crew_window(runtime, run_id)?
        .reviewer_unrunnable_reason(crew)
        .map(|reason| {
            format!("before_pr_reviewer_unavailable: the owner's {timing} review {reason}")
        }))
}

/// The `review.before_pr` the pull drain captured at submission
/// [ORB-13992]: turning it on later does not change what a running drain
/// declares. A drain submitted before the capture existed falls back to this
/// host's current setting.
pub(super) fn captured_before_pr(runtime: &OrbitRuntime, run_id: &str) -> Result<bool, OrbitError> {
    Ok(
        crate::application::review::run_review_admission(runtime, run_id)?.map_or_else(
            || runtime.local_review_before_pr(),
            |admission| admission.gates_pr(),
        ),
    )
}
