//! Shared before-PR review coverage rules [ORB-11333].
//!
//! Core collects the facts: the certificate a gate issued, the delivery the
//! source adapter observed, whether the certificate's objects still exist,
//! and whether the task still means what it meant when it was reviewed.
//! This module owns the deterministic decisions over those facts: what a
//! task's reviewable meaning is, whether a certificate is acceptable
//! coverage at all, and whether an actual landing reproduced exactly the
//! reviewed content, or carried it unchanged onto a base that moved.
//! Uncertain content is never excluded.

use orbit_types::task::Task;
use orbit_types::workflow::automation::{Delivery, DeliveryExclusion};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, ReviewCertificate, ReviewInvalidation, ReviewLanding,
};
use serde_json::json;

use crate::AutomationError;
use crate::delivery::json_definition_epoch;

mod validation;

#[cfg(test)]
mod tests;

pub use validation::{
    SkippedCheck, ValidationContext, ValidationDefect, in_scope, mutation_targets,
    same_host_command, validation_evidence, validation_limitations, validation_role_counts,
};

/// Assurance an exclusion carries when the landing is the reviewed final
/// candidate cleanly carried onto a base that moved after review, rather
/// than a byte-identical reproduction of the reviewed trees.
pub const REBASED_CLEAN_ASSURANCE: &str = "rebased_clean";

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

    json_definition_epoch(json!({
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
    json_definition_epoch(json!({
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
/// `validation_complete` flag alone, against the scope and retained
/// obligations the certificate itself records, so a certificate whose records
/// do not support the flag is never spent as coverage.
pub fn certificate_acceptable(certificate: &ReviewCertificate) -> Result<(), ReviewInvalidation> {
    if certificate.schema_version != REVIEW_CONTRACT_VERSION {
        return Err(ReviewInvalidation::MappingUnknown);
    }
    if certificate.required_validation_commands.is_none() {
        return Err(ReviewInvalidation::ValidationContractMissing);
    }
    if !certificate.verdict.passed() || certificate.assurance.is_none() {
        return Err(ReviewInvalidation::VerdictNotPassed);
    }
    let context = ValidationContext {
        scope: &certificate.validation_scope,
        obligations: &certificate.retained_obligations,
        retired: &certificate.retired_validation,
        required_validation_commands: certificate.required_validation_commands.as_deref(),
        baseline_commands: &certificate.baseline_commands,
    };
    if !certificate.validation_complete
        || validation_evidence(&certificate.validation, &context).is_err()
    {
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
    /// The head commit the provider reports the landed pull request had.
    /// `None` for a landing with no pull request identity, or one recorded
    /// before the head was kept.
    pub landed_head: Option<String>,
    /// The tree of a conflict-free three-way merge of the certificate's
    /// final candidate onto the delivery's actual base, with the
    /// certificate's base as the merge base. `None` when the merge
    /// conflicts, the reviewed base is not an ancestor of the actual base,
    /// or Core did not compute it because the exact-tree rule applies.
    pub rebased_tree: Option<String>,
}

/// Decide whether an actual delivery is covered by a certificate.
///
/// The exact-tree rule: the landing starts from the reviewed base tree and
/// produces the reviewed final tree. Squash, merge, rebase, and fast-forward
/// onto the same base all satisfy that, and the exclusion carries the
/// certificate's own assurance.
///
/// The rebased-clean rule applies only when the base moved after review and
/// the landed pull request's head is the certificate's final candidate
/// commit, so the provider merged exactly what was reviewed. The landing
/// must then equal, tree for tree, a conflict-free merge of the reviewed
/// change onto the actual base ([`LandingFacts::rebased_tree`]); the
/// exclusion carries [`REBASED_CLEAN_ASSURANCE`]. A conflicting base change,
/// a head pushed after review, or any other edit stays uncovered.
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
    let rebased = delivery.before.tree != certificate.base.tree;
    if rebased {
        if facts.landed_head.as_deref() != Some(certificate.final_candidate.commit.as_str()) {
            return Err(ReviewInvalidation::BaseChanged);
        }
        match facts.rebased_tree.as_deref() {
            None => return Err(ReviewInvalidation::BaseChanged),
            Some(tree) if tree != delivery.after.tree => {
                return Err(ReviewInvalidation::CandidateChanged);
            }
            Some(_) => {}
        }
    } else if delivery.after.tree != certificate.final_candidate.tree {
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
    let assurance = if rebased {
        REBASED_CLEAN_ASSURANCE
    } else {
        assurance.as_str()
    };
    Ok(DeliveryExclusion {
        attempt_id: certificate.attempt_id.clone(),
        assurance: assurance.to_string(),
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
