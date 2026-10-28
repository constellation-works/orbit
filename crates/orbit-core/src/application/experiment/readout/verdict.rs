use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_store::contracts::ExperimentRecord;

use crate::application::experiment::readout::outcomes::{Kind, MetricSpec, metric_spec};

use super::model::{GuardrailArm, GuardrailReadout, ReadoutMetric, Verdict};

/// A guardrail threshold: its direction (`true` for an upper limit) and
/// its value in the metric's units, given the control's mean.
fn guardrail_limit(
    spec: &MetricSpec,
    threshold: &str,
    control_mean: Option<f64>,
) -> Option<(bool, f64)> {
    let upper = threshold.starts_with('+');
    let rest = threshold.get(1..)?;
    let (number, unit) = if let Some(number) = rest.strip_suffix("pt") {
        (number, "pt")
    } else if let Some(number) = rest.strip_suffix('%') {
        (number, "%")
    } else {
        (rest, "")
    };
    let magnitude: f64 = number.parse().ok()?;
    let magnitude = match unit {
        // Percentage points of a rate, which the readout keeps as a fraction.
        "pt" if spec.kind == Kind::Rate => magnitude / 100.0,
        "pt" => magnitude,
        "%" => magnitude / 100.0 * control_mean?.abs(),
        _ => magnitude,
    };
    Some((upper, if upper { magnitude } else { -magnitude }))
}

pub(super) fn guardrail_readout(
    metric: &str,
    threshold: &str,
    metrics: &[ReadoutMetric],
) -> GuardrailReadout {
    let readout = metrics.iter().find(|candidate| candidate.metric == metric);
    let spec = metric_spec(metric);
    let control_mean = readout.and_then(|readout| {
        let differenced: BTreeSet<&str> =
            readout.differences.iter().map(|d| d.arm.as_str()).collect();
        readout
            .arms
            .iter()
            .find(|arm| !differenced.contains(arm.arm.as_str()))
            .and_then(|arm| arm.estimate.mean)
    });
    let arms = readout
        .map(|readout| {
            readout
                .differences
                .iter()
                .map(|difference| {
                    let limit =
                        spec.and_then(|spec| guardrail_limit(spec, threshold, control_mean));
                    let estimate = difference.overall.estimate;
                    let interval = difference.overall.interval;
                    let status = match (limit, estimate) {
                        (Some((upper, limit)), Some(estimate)) => {
                            let past =
                                |value: f64| if upper { value > limit } else { value < limit };
                            if past(estimate) {
                                "breached"
                            } else if interval.is_none_or(|[low, high]| past(low) || past(high)) {
                                "straddles"
                            } else {
                                "ok"
                            }
                        }
                        _ => "unknown",
                    };
                    GuardrailArm {
                        arm: difference.arm.clone(),
                        limit: limit.map(|(_, limit)| limit),
                        estimate,
                        interval,
                        status,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    GuardrailReadout {
        metric: metric.to_string(),
        threshold: threshold.to_string(),
        arms,
    }
}

/// Every active arm has `min_sample` primary outcomes, or `max_days` have
/// passed since start.
pub(super) fn horizon_reached(
    record: &ExperimentRecord,
    primary: Option<&ReadoutMetric>,
    horizon_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    if horizon_at.is_some_and(|at| now >= at) {
        return true;
    }
    let (Some(min), Some(primary)) = (record.experiment.min_sample_per_arm, primary) else {
        return false;
    };
    record
        .arms
        .iter()
        .filter(|arm| arm.retired_segment.is_none())
        .all(|arm| {
            primary
                .arms
                .iter()
                .find(|estimate| estimate.arm == arm.name)
                .is_some_and(|estimate| estimate.estimate.n >= min as usize)
        })
}

pub(super) fn verdict(
    horizon_reached: bool,
    mismatch: bool,
    started: bool,
    primary: Option<&ReadoutMetric>,
    guardrails: &[GuardrailReadout],
) -> Verdict {
    let make =
        |state: &'static str, arm: Option<String>, label: String, reasons: Vec<String>| Verdict {
            state,
            arm,
            label,
            horizon_reached,
            reasons,
        };
    if mismatch {
        return make(
            "withheld",
            None,
            "no verdict".into(),
            vec!["the sample-ratio check failed".into()],
        );
    }
    if !started || !horizon_reached {
        return make(
            "collecting",
            None,
            "collecting".into(),
            vec!["not every arm has reached min_sample and max_days has not passed".into()],
        );
    }
    let Some(primary) = primary.filter(|primary| primary.measured) else {
        return make(
            "inconclusive",
            None,
            "inconclusive".into(),
            vec!["the primary metric is not measured".into()],
        );
    };
    let higher = primary.better == "higher";
    let mut winners: Vec<(f64, &str)> = Vec::new();
    let mut reasons = Vec::new();
    let mut inconclusive = false;
    for difference in &primary.differences {
        let arm = difference.arm.as_str();
        let (Some(estimate), Some([low, high])) =
            (difference.overall.estimate, difference.overall.interval)
        else {
            inconclusive = true;
            reasons.push(format!("arm `{arm}` has no interval on {}", primary.metric));
            continue;
        };
        let better = if higher { low > 0.0 } else { high < 0.0 };
        if !better {
            reasons.push(format!(
                "arm `{arm}`'s {} interval does not exclude zero in the better direction",
                primary.metric
            ));
            continue;
        }
        let statuses: Vec<(&str, &str)> = guardrails
            .iter()
            .filter_map(|guardrail| {
                guardrail
                    .arms
                    .iter()
                    .find(|candidate| candidate.arm == arm)
                    .map(|candidate| (guardrail.metric.as_str(), candidate.status))
            })
            .collect();
        if let Some((metric, _)) = statuses.iter().find(|(_, status)| *status == "breached") {
            inconclusive = true;
            reasons.push(format!("arm `{arm}` breaches the {metric} guardrail"));
        } else if let Some((metric, status)) = statuses
            .iter()
            .find(|(_, status)| matches!(*status, "straddles" | "unknown"))
        {
            inconclusive = true;
            reasons.push(format!("arm `{arm}`'s {metric} guardrail {status}"));
        } else {
            winners.push((estimate, arm));
        }
    }
    // The largest improvement wins when several treatments do.
    winners.sort_by(|left, right| {
        let order = left.0.total_cmp(&right.0);
        if higher { order.reverse() } else { order }
    });
    if let Some((_, arm)) = winners.first() {
        return make(
            "winner",
            Some((*arm).to_string()),
            format!("winner {arm}"),
            reasons,
        );
    }
    if inconclusive {
        make("inconclusive", None, "inconclusive".into(), reasons)
    } else {
        make("no_difference", None, "no difference".into(), reasons)
    }
}
