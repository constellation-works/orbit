//! Feed proven before-PR coverage to the shared delivery evaluator
//! [ORB-11333].
//!
//! For every landing a source page observed, Core looks up passed
//! certificates whose final tree the landing reproduced, verifies the facts
//! the shared rule needs (objects still present, task meaning unchanged,
//! any managed landing record), and asks `orbit-automation` for the
//! decision. Only accepted exclusions reach the page; every refusal leaves
//! the landing an ordinary obligation.

use orbit_automation::AutomationError;
use orbit_automation::review::{
    LandingFacts, combined_task_meaning_digest, exclusion, task_meaning_digest,
};
use orbit_automation::source::Source;
use orbit_store::contracts::ReviewStoreBackend;
use orbit_types::workflow::ReviewCertificate;
use orbit_types::workflow::automation::{AutomationState, SourcePage};

use crate::OrbitRuntime;

/// Certificates considered per landing; one candidate tree rarely has more.
const CERTIFICATES_PER_LANDING: usize = 5;

pub(crate) fn exclusions(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    state: &AutomationState,
    page: &mut SourcePage,
) -> Result<(), AutomationError> {
    let store = runtime.review_store()?;
    for delivery in &page.deliveries {
        if page.exclusions.contains_key(&delivery.key) {
            continue;
        }
        let certificates = store.review_certificates_for_tree(
            &state.repository,
            &delivery.after.tree,
            CERTIFICATES_PER_LANDING,
        )?;
        for certificate in certificates {
            let facts = landing_facts(runtime, source, store.as_ref(), &certificate)?;
            match exclusion(delivery, &certificate, &facts) {
                Ok(decision) => {
                    page.exclusions.insert(delivery.key.clone(), decision);
                    break;
                }
                Err(reason) => {
                    tracing::debug!(
                        delivery = %delivery.key,
                        attempt_id = %certificate.attempt_id,
                        reason = reason.as_str(),
                        "before-PR certificate does not cover this landing"
                    );
                }
            }
        }
    }
    Ok(())
}

fn landing_facts(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    store: &dyn ReviewStoreBackend,
    certificate: &ReviewCertificate,
) -> Result<LandingFacts, AutomationError> {
    let objects_present = [
        &certificate.base,
        &certificate.reviewed_candidate,
        &certificate.final_candidate,
    ]
    .into_iter()
    .all(|revision| {
        source
            .revision(&revision.commit)
            .is_ok_and(|actual| actual == *revision)
    });
    let task_meaning_current = task_meaning_current(runtime, certificate)?;
    let managed_landing = store
        .review_landings(&certificate.attempt_id)?
        .into_iter()
        .last();
    Ok(LandingFacts {
        objects_present,
        task_meaning_current,
        managed_landing,
    })
}

/// Recompute every task's meaning; a missing task or a changed digest means
/// the certificate no longer describes what was asked.
fn task_meaning_current(
    runtime: &OrbitRuntime,
    certificate: &ReviewCertificate,
) -> Result<bool, AutomationError> {
    let mut digests = Vec::with_capacity(certificate.task_ids.len());
    for task_id in &certificate.task_ids {
        let Ok(task) = runtime.get_task(task_id) else {
            return Ok(false);
        };
        digests.push((task_id.clone(), task_meaning_digest(&task)?));
    }
    Ok(combined_task_meaning_digest(&digests)? == certificate.task_meaning_digest)
}
