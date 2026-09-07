//! Shared before-PR review coverage rules [ORB-11333].
//!
//! Core collects the facts: the certificate a gate issued, the delivery the
//! source adapter observed, whether the certificate's objects still exist,
//! and whether the task still means what it meant when it was reviewed.
//! This module owns the deterministic decisions over those facts: what a
//! task's reviewable meaning is, whether a certificate is acceptable
//! coverage at all, and whether an actual landing reproduced exactly the
//! reviewed content. Uncertain content is never excluded.

use orbit_types::task::Task;
use orbit_types::workflow::automation::{Delivery, DeliveryExclusion};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, ReviewCertificate, ReviewInvalidation, ReviewLanding,
};
use serde_json::json;

use crate::AutomationError;
use crate::delivery::definition_epoch;

mod validation;

#[cfg(test)]
mod tests;

pub use validation::{ValidationDefect, validation_evidence, validation_role_counts};

/// Contract label folded into every task-meaning digest.
pub const TASK_MEANING_CONTRACT: &str = "review_task_meaning_v1";

/// The digest of what a reviewer is asked to judge: title, description,
/// acceptance criteria, plan, selectors, tags, relations, and type. Comments,
/// history, priority, execution summaries, and status are deliberately absent
/// so audit writes cannot invalidate a gate, while a criteria edit does.
pub fn task_meaning_digest(task: &Task) -> Result<String, AutomationError> {
    let mut selectors = task.context_files.clone();
    selectors.sort();
    selectors.dedup();

    let mut tags = task.tags.clone();
    tags.sort();
    tags.dedup();

    definition_epoch(&json!({
        "contract": TASK_MEANING_CONTRACT,
        "id": task.id,
        "title": task.title.trim(),
        "description": task.description.trim(),
        "criteria": task.acceptance_criteria,
        "plan": task.plan.trim(),
        "selectors": selectors,
        "tags": tags,
        "relations": task.relations,
        "type": task.task_type,
    }))
}

/// The combined digest for a bundle: each task's digest in task-id order.
pub fn combined_task_meaning_digest(
    digests: &[(String, String)],
) -> Result<String, AutomationError> {
    let mut ordered = digests.to_vec();
    ordered.sort();
    definition_epoch(&json!({
        "contract": TASK_MEANING_CONTRACT,
        "tasks": ordered,
    }))
}

/// Whether a certificate is acceptable coverage on its own terms: current
/// contract, a pass verdict, validation records that establish the final
/// candidate under [`validation_evidence`], and a candidate distinct from
/// its base.
///
/// The validation rules are re-derived here rather than trusting the
/// `validation_complete` flag alone, so a certificate whose records do not
/// support the flag is never spent as coverage.
pub fn certificate_acceptable(certificate: &ReviewCertificate) -> Result<(), ReviewInvalidation> {
    if certificate.schema_version != REVIEW_CONTRACT_VERSION {
        return Err(ReviewInvalidation::MappingUnknown);
    }
    if !certificate.verdict.passed() || certificate.assurance.is_none() {
        return Err(ReviewInvalidation::VerdictNotPassed);
    }
    if !certificate.validation_complete || validation_evidence(&certificate.validation).is_err() {
        return Err(ReviewInvalidation::ValidationIncomplete);
    }
    if certificate.final_candidate.tree == certificate.base.tree {
        return Err(ReviewInvalidation::CandidateChanged);
    }
    Ok(())
}

/// Facts Core verified about a delivery before asking for an exclusion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandingFacts {
    /// The certificate's base, reviewed, and final candidate objects are
    /// still present in the repository.
    pub objects_present: bool,
    /// Every task named by the certificate still has the reviewed meaning.
    pub task_meaning_current: bool,
    /// The managed landing record for this certificate, when completion
    /// wrote one.
    pub managed_landing: Option<ReviewLanding>,
}

/// Decide whether an actual delivery is covered by a certificate.
///
/// V1 is exact-tree: the landing must start from the reviewed base tree and
/// produce the reviewed final tree. Squash, merge, rebase, and fast-forward
/// onto the same base all satisfy that; a different base, any later edit,
/// or an unreviewed conflict repair changes a tree and stays uncovered.
pub fn exclusion(
    delivery: &Delivery,
    certificate: &ReviewCertificate,
    facts: &LandingFacts,
) -> Result<DeliveryExclusion, ReviewInvalidation> {
    certificate_acceptable(certificate)?;
    if !facts.objects_present {
        return Err(ReviewInvalidation::ObjectsMissing);
    }
    if !facts.task_meaning_current {
        return Err(ReviewInvalidation::TaskMeaningChanged);
    }
    if delivery.repository != certificate.repository {
        return Err(ReviewInvalidation::MappingUnknown);
    }
    if delivery.before.tree != certificate.base.tree {
        return Err(ReviewInvalidation::BaseChanged);
    }
    if delivery.after.tree != certificate.final_candidate.tree {
        return Err(ReviewInvalidation::CandidateChanged);
    }
    if let Some(landing) = &facts.managed_landing
        && (!landing.covered || landing.landed.commit != delivery.after.commit)
    {
        return Err(ReviewInvalidation::ExternalLandingRace);
    }
    let assurance = certificate
        .assurance
        .ok_or(ReviewInvalidation::VerdictNotPassed)?;
    Ok(DeliveryExclusion {
        attempt_id: certificate.attempt_id.clone(),
        assurance: assurance.as_str().to_string(),
        task_meaning_digest: certificate.task_meaning_digest.clone(),
        final_candidate_tree: certificate.final_candidate.tree.clone(),
    })
}

/// Facts about the commit that actually landed, gathered by Core after a
/// managed merge, for the record completion writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandedCandidate {
    pub landed_commit: String,
    pub landed_tree: String,
    /// The tree of the integration branch immediately before the landing
    /// span: the first parent of the span's first commit.
    pub base_at_landing_tree: String,
    /// The landed commit equals the reviewed candidate commit.
    pub is_candidate_commit: bool,
    /// Number of parents of the landed commit.
    pub parents: usize,
    /// First-parent commits the landing added to the branch.
    pub span_commits: usize,
}

/// Classify a managed landing against its certificate. Returns the
/// transformation and whether the certificate still covers the landing.
pub fn classify_landing(
    certificate: &ReviewCertificate,
    landed: &LandedCandidate,
) -> (
    orbit_types::workflow::LandingTransformation,
    bool,
    Option<ReviewInvalidation>,
) {
    use orbit_types::workflow::LandingTransformation as T;

    if let Err(reason) = certificate_acceptable(certificate) {
        return (T::Unknown, false, Some(reason));
    }
    if landed.base_at_landing_tree != certificate.base.tree {
        return (T::Unknown, false, Some(ReviewInvalidation::BaseChanged));
    }
    if landed.landed_tree != certificate.final_candidate.tree {
        return (
            T::Unknown,
            false,
            Some(ReviewInvalidation::CandidateChanged),
        );
    }
    let transformation = if landed.is_candidate_commit {
        T::FastForward
    } else if landed.parents > 1 {
        T::MergeCommit
    } else if landed.span_commits > 1 {
        T::Rebase
    } else {
        T::Squash
    };
    (transformation, true, None)
}
