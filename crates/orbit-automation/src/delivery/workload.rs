//! Oldest-prefix admission under a frozen review work policy.

use super::DeliveryHost;
use crate::AutomationError;
use orbit_types::workflow::automation::*;

pub(super) struct ReviewSelection {
    pub deliveries: Vec<Delivery>,
    pub through: SourceRevision,
    pub accounting: FrozenReviewWork,
}

/// A hold leaves all debt and coverage untouched; increasing a configured
/// ceiling uses the ordinary settings-adoption path before a new freeze.
pub(super) fn select_review_prefix(
    host: &dyn DeliveryHost,
    state: &AutomationState,
    trigger: &DeliveryTrigger,
) -> Result<Result<ReviewSelection, String>, AutomationError> {
    let budget = trigger.effective_review_budget();
    let mut accounting = FrozenReviewWork {
        policy_version: 1,
        budget: budget.clone(),
        charges: vec![],
        total_points: 0,
        charged_lines: 0,
        workload: ReviewWorkload::default(),
        stop_reason: "pending_exhausted".into(),
    };
    let mut deliveries = Vec::new();
    let mut through = state.covered.clone();
    for (index, delivery) in state.pending.iter().enumerate().take(trigger.max_items) {
        let complexities = host.review_complexities(delivery)?;
        let unattributed = delivery.task_ids.is_empty();
        let points = if unattributed {
            Some(budget.low.max(budget.medium).max(budget.hard))
        } else if complexities.len() != delivery.task_ids.len() || complexities.is_empty() {
            None
        } else {
            complexities.iter().try_fold(0, |max, complexity| {
                budget.points(*complexity).map(|points| max.max(points))
            })
        };
        let Some(points) = points else {
            let reason = format!(
                "review_assessment_required: {}: assess every landing task before retrying",
                delivery.key
            );
            if deliveries.is_empty() {
                return Ok(Err(reason));
            }
            accounting.stop_reason = reason;
            break;
        };
        let next_points = accounting.total_points + points;
        let candidate_through = if index + 1 == state.pending.len() {
            &state.observed
        } else {
            &delivery.after
        };
        let workload = host.review_workload(&state.covered, candidate_through)?;
        let charged_lines = workload.lines.saturating_add(
            u64::from(workload.binary_files) * u64::from(budget.binary_line_charge),
        );
        let exceeds = if points > budget.max_points || next_points > budget.max_points {
            Some("points")
        } else if workload.files > budget.max_files {
            Some("files")
        } else if charged_lines > u64::from(budget.max_lines) {
            Some(if workload.binary_files > 0 {
                "binary_changes_require_larger_review_budget"
            } else {
                "lines"
            })
        } else {
            None
        };
        if let Some(limit) = exceeds {
            let reason = format!(
                "review_work_oversize: {}: {limit}; points={next_points}/{} files={}/{} textual_lines={} charged_lines={charged_lines}/{} binary={} binary_line_charge={}; retain debt; assess or use a larger configured review_budget and recover --adopt-settings; binary work requires an explicit larger review budget including its binary_line_charge",
                delivery.key,
                budget.max_points,
                workload.files,
                budget.max_files,
                workload.lines,
                budget.max_lines,
                workload.binary_files,
                budget.binary_line_charge
            );
            if deliveries.is_empty() {
                return Ok(Err(reason));
            }
            accounting.stop_reason = reason;
            break;
        }
        let own_workload = host.review_workload(&delivery.before, &delivery.after)?;
        accounting.charges.push(DeliveryReviewCharge {
            delivery: delivery.key.clone(),
            complexities,
            unattributed,
            points,
            workload: own_workload,
        });
        accounting.total_points = next_points;
        accounting.charged_lines = charged_lines;
        accounting.workload = workload;
        deliveries.push(delivery.clone());
        through = candidate_through.clone();
    }
    if deliveries.len() == trigger.max_items && deliveries.len() < state.pending.len() {
        accounting.stop_reason = "max_items".into();
    }
    Ok(Ok(ReviewSelection {
        deliveries,
        through,
        accounting,
    }))
}
