//! Configurable review work policy and immutable batch accounting.

use crate::task::TaskComplexity;
use serde::{Deserialize, Serialize};

/// Positive bounded limits for one after-landing review batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeliveryReviewBudget {
    pub max_points: u32,
    pub low: u32,
    pub medium: u32,
    pub hard: u32,
    pub max_files: u32,
    /// Added plus deleted textual lines, summed over first-parent changes.
    pub max_lines: u32,
    /// Conservative line-equivalent charge per binary path; defaults above
    /// the ordinary line ceiling, requiring an explicit larger-work budget.
    pub binary_line_charge: u32,
}

impl Default for DeliveryReviewBudget {
    fn default() -> Self {
        Self {
            max_points: 8,
            low: 1,
            medium: 4,
            hard: 8,
            max_files: 20,
            max_lines: 2000,
            binary_line_charge: 2001,
        }
    }
}

impl DeliveryReviewBudget {
    pub fn validate(&self) -> Result<(), super::super::error::WorkflowError> {
        if [
            self.max_points,
            self.low,
            self.medium,
            self.hard,
            self.max_files,
            self.max_lines,
            self.binary_line_charge,
        ]
        .iter()
        .any(|value| !(1..=1_000_000).contains(value))
        {
            return Err(super::super::error::WorkflowError::Invalid(
                "review budget limits and weights must be integers in 1..=1000000".into(),
            ));
        }
        Ok(())
    }

    /// Xhard is conservatively twice hard. Missing assessment has no charge.
    pub fn points(&self, complexity: TaskComplexity) -> Option<u32> {
        match complexity {
            TaskComplexity::Low => Some(self.low),
            TaskComplexity::Medium => Some(self.medium),
            TaskComplexity::Hard => Some(self.hard),
            TaskComplexity::XHard => self.hard.checked_mul(2),
            TaskComplexity::Unassessed => None,
        }
    }
}

/// Actual Git workload; binary changes require a larger-work review path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ReviewWorkload {
    pub files: u32,
    pub lines: u64,
    pub binary_files: u32,
}

/// One delivery's assessed, charged and measured work, frozen once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryReviewCharge {
    pub delivery: String,
    /// All task assessments; multi-task deliveries pay their maximum charge.
    pub complexities: Vec<TaskComplexity>,
    pub unattributed: bool,
    pub points: u32,
    pub workload: ReviewWorkload,
}

/// Digest-bound selection policy; absent on historical frozen batches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenReviewWork {
    pub policy_version: u32,
    pub budget: DeliveryReviewBudget,
    pub charges: Vec<DeliveryReviewCharge>,
    pub total_points: u32,
    pub charged_lines: u64,
    /// Entire first-parent range, including excluded/context neighbors.
    pub workload: ReviewWorkload,
    pub stop_reason: String,
}
