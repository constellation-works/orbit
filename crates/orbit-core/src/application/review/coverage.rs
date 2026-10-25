//! Feed proven before-PR coverage to the shared delivery evaluator
//! [ORB-11333].
//!
//! For every landing a source page observed, Core looks up passed
//! certificates whose final tree the landing reproduced, or whose final tree
//! is the landed pull request's head, verifies the facts the shared rule
//! needs (objects still present, task meaning unchanged, any managed landing
//! record, and for a base that moved, the clean merge of the reviewed change
//! onto it), and asks `orbit-automation` for the decision. Only accepted
//! exclusions reach the page; every refusal leaves the landing an ordinary
//! obligation.
//!
//! [ORB-16855] A landing no certificate covers is also excluded when every
//! task it delivers carries `skip-review` and every path it changes matches
//! `workflow.light_delivery_paths`: such a delivery had no agent review
//! before landing and gets none after.

use orbit_automation::AutomationError;
use orbit_automation::review::{
    LandingFacts, combined_task_meaning_digest, exclusion, task_meaning_digest,
};
use orbit_common::fs::path_filter::PathFilter;
use orbit_store::contracts::ReviewStoreBackend;
use orbit_types::task::{LIGHT_DELIVERY_PATHS_KEY, LightDeliveryJudgement, SKIP_REVIEW_TAG};
use orbit_types::workflow::ReviewCertificate;
use orbit_types::workflow::automation::{AutomationState, Delivery, DeliveryExclusion, SourcePage};

use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::application::task::light_delivery::{
    carries, record_light_delivery, review_skip_decisions,
};

/// Certificates considered per landing; one candidate tree rarely has more.
const CERTIFICATES_PER_LANDING: usize = 5;

pub(crate) fn exclusions(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    state: &AutomationState,
    page: &mut SourcePage,
    dry_run: bool,
) -> Result<(), AutomationError> {
    let store = runtime.review_store()?;
    for delivery in &page.deliveries {
        if page.exclusions.contains_key(&delivery.key) {
            continue;
        }
        let landed_head = page
            .associations
            .get(&delivery.after.commit)
            .and_then(Option::as_ref)
            .and_then(|association| association.head.clone());
        let mut certificates = store.review_certificates_for_tree(
            &state.repository,
            &delivery.after.tree,
            CERTIFICATES_PER_LANDING,
        )?;
        // [ORB-15193] A base that moved after review leaves the landed tree
        // unlike any reviewed one; the pull request's head names the
        // candidate the provider merged.
        if let Some(head) = &landed_head
            && let Ok(revision) = source.revision(head)
            && revision.tree != delivery.after.tree
        {
            certificates.extend(store.review_certificates_for_tree(
                &state.repository,
                &revision.tree,
                CERTIFICATES_PER_LANDING,
            )?);
        }
        for certificate in certificates {
            let facts = landing_facts(
                runtime,
                source,
                store.as_ref(),
                delivery,
                &certificate,
                landed_head.clone(),
            )?;
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
        if !page.exclusions.contains_key(&delivery.key)
            && let Some(decision) = light_delivery_exclusion(runtime, source, delivery, dry_run)?
        {
            page.exclusions.insert(delivery.key.clone(), decision);
        }
    }
    Ok(())
}

/// The exclusion of a fully attributed landing whose tasks all carry
/// `skip-review` and whose changed paths all match this host's
/// `workflow.light_delivery_paths` [ORB-16855].
fn light_delivery_exclusion(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    delivery: &Delivery,
    dry_run: bool,
) -> Result<Option<DeliveryExclusion>, AutomationError> {
    if delivery.task_ids.is_empty() || delivery.unattributed.is_some() {
        return Ok(None);
    }
    let mut tasks = Vec::with_capacity(delivery.task_ids.len());
    let mut digests = Vec::with_capacity(delivery.task_ids.len());
    for task_id in &delivery.task_ids {
        let Ok(task) = runtime.get_task(task_id) else {
            return Ok(None);
        };
        digests.push((task_id.clone(), task_meaning_digest(&task)?));
        tasks.push(task);
    }
    let exclusion = |digests: &[(String, String)]| {
        combined_task_meaning_digest(digests).map(|digest| DeliveryExclusion {
            attempt_id: format!("{LIGHT_DELIVERY_ASSURANCE}:{}", delivery.key),
            assurance: LIGHT_DELIVERY_ASSURANCE.to_string(),
            task_meaning_digest: digest,
            final_candidate_tree: delivery.after.tree.clone(),
        })
    };
    if tasks.iter().all(|task| carries(task, SKIP_REVIEW_TAG)) {
        let filter = PathFilter::new(
            LIGHT_DELIVERY_PATHS_KEY,
            runtime.workflow_light_delivery_paths(),
        )?;
        let changed = source.git_matching_paths(
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                "--end-of-options",
                &delivery.before.commit,
                &delivery.after.commit,
                "--",
            ],
            |_| true,
        )?;
        if LightDeliveryJudgement::new(changed.len(), &filter.outside(&changed)).is_light() {
            return Ok(Some(exclusion(&digests)?));
        }
    }
    // Mapped decisions can execute repository guards and append task history.
    // Preview keeps the landing as ordinary work until real evaluation can
    // authorize those effects, even for a profile with no guard.
    if dry_run {
        return Ok(None);
    }
    // A mapped review-skip tag excludes the landing the same way, under
    // its own paths and guard (read at the landing's base).
    let decisions = review_skip_decisions(
        runtime,
        &runtime.paths().repo_root,
        &delivery.before.commit,
        &delivery.after.commit,
        &tasks,
    )?;
    let mut applies = false;
    for decision in &decisions {
        applies |= decision.applies();
        record_light_delivery(runtime, &delivery.task_ids, decision)?;
    }
    if applies {
        return Ok(Some(exclusion(&digests)?));
    }
    Ok(None)
}

/// The assurance a light-delivery exclusion carries.
const LIGHT_DELIVERY_ASSURANCE: &str = "light_delivery";

fn landing_facts(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    store: &dyn ReviewStoreBackend,
    delivery: &Delivery,
    certificate: &ReviewCertificate,
    landed_head: Option<String>,
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
    let rebased_tree = if objects_present
        && delivery.before.tree != certificate.base.tree
        && landed_head.as_deref() == Some(certificate.final_candidate.commit.as_str())
    {
        rebased_tree(source, delivery, certificate)?
    } else {
        None
    };
    Ok(LandingFacts {
        objects_present,
        task_meaning_current,
        managed_landing,
        landed_head,
        rebased_tree,
    })
}

/// The reviewed change, base to final candidate, merged onto the base the
/// delivery actually landed on. Only a base that descends from the reviewed
/// one qualifies: a rewritten history is not a moved base.
fn rebased_tree(
    source: &Source<'_>,
    delivery: &Delivery,
    certificate: &ReviewCertificate,
) -> Result<Option<String>, AutomationError> {
    if !source.is_ancestor(&certificate.base.commit, &delivery.before.commit)? {
        return Ok(None);
    }
    source.clean_merge_tree(
        &certificate.base.commit,
        &delivery.before.commit,
        &certificate.final_candidate.commit,
    )
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
