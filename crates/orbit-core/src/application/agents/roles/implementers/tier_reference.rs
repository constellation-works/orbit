//! Sparse-tier expectations use conservative exchanges of rate evidence.
//! Each exchange moves a sparse tier toward its reference and applies the
//! opposite expected-count change to the reference, preserving fleet totals.
use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::performance::{
    ESCAPED_REGRESSIONS, Fleet, MIN_TIER_TASKS, TERMS, is_implementer_review_term,
};

const ORDERED_TIERS: &[&str] = &["low", "medium", "hard", "xhard"];

struct Reference {
    name: &'static str,
    tiers: Vec<&'static str>,
}

/// Per-tier expected rates, with an audit record for each sparse tier.
pub(super) struct TierReferences {
    pub(super) rates: BTreeMap<&'static str, BTreeMap<&'static str, f64>>,
    pub(super) adjustments: BTreeMap<&'static str, Value>,
}

/// Retain a sparse tier's own evidence while borrowing from the nearest
/// supported tier, or a pool when its complexity has no supported neighbour.
pub(super) fn tier_references(fleet: &BTreeMap<&'static str, Fleet>) -> TierReferences {
    let supported: Vec<_> = fleet
        .iter()
        .filter(|(_, stats)| stats.units >= MIN_TIER_TASKS)
        .map(|(&tier, _)| tier)
        .collect();
    let mut references = BTreeMap::new();
    for (&tier, stats) in fleet
        .iter()
        .filter(|(_, stats)| stats.units < MIN_TIER_TASKS)
    {
        let nearest = ORDERED_TIERS
            .iter()
            .position(|&name| name == tier)
            .and_then(|index| {
                ORDERED_TIERS
                    .iter()
                    .enumerate()
                    .filter(|(_, name)| supported.contains(name))
                    .min_by_key(|(other, _)| (index.abs_diff(*other), *other))
                    .map(|(_, &name)| name)
            });
        let reference = match nearest {
            Some(name) => Reference {
                name,
                tiers: vec![name],
            },
            None if !supported.is_empty() => Reference {
                name: "supported_pool",
                tiers: supported.clone(),
            },
            None => Reference {
                name: "fleet_pool",
                tiers: fleet.keys().copied().collect(),
            },
        };
        references.insert(tier, (stats, reference));
    }
    let mut result = TierReferences {
        rates: fleet.keys().map(|&tier| (tier, BTreeMap::new())).collect(),
        adjustments: references
            .iter()
            .map(|(&tier, (stats, reference))| {
                (
                    tier,
                    json!({
                        "tier": tier, "adjusted": true, "method": "conservative_shrinkage",
                        "fleet_units": stats.units, "reference": reference.name,
                        "reference_tiers": reference.tiers, "terms": {},
                    }),
                )
            })
            .collect(),
    };
    for term in TERMS.iter().filter(|term| term.key != ESCAPED_REGRESSIONS) {
        let units: BTreeMap<_, _> = fleet
            .iter()
            .map(|(&tier, stats)| {
                (
                    tier,
                    if is_implementer_review_term(term.key) {
                        stats.reviewed_units as f64
                    } else {
                        stats.units as f64
                    },
                )
            })
            .collect();
        let rates: BTreeMap<_, _> = fleet
            .iter()
            .map(|(&tier, stats)| {
                (
                    tier,
                    stats.counts.get(term.key).copied().unwrap_or(0.0) / units[tier].max(1.0),
                )
            })
            .collect();
        let borrowed = |tier: &str| {
            units[tier] * MIN_TIER_TASKS as f64 / (units[tier] + MIN_TIER_TASKS as f64)
        };
        let mut exchanges = Vec::new();
        if supported.is_empty() {
            // With no supported tier, use a borrowing-weighted fleet pool.
            // One undirected edge per pair balances both ends of the exchange.
            let total: f64 = fleet.keys().map(|tier| borrowed(tier)).sum();
            for (index, &left) in fleet.keys().enumerate() {
                for &right in fleet.keys().skip(index + 1) {
                    if total > 0.0 {
                        exchanges.push((left, right, borrowed(left) * borrowed(right) / total));
                    }
                }
            }
        } else {
            for (&tier, (_, reference)) in &references {
                let total: f64 = reference.tiers.iter().map(|peer| units[peer]).sum();
                for &peer in &reference.tiers {
                    if total > 0.0 {
                        exchanges.push((tier, peer, borrowed(tier) * units[peer] / total));
                    }
                }
            }
        }
        let mut exposure: BTreeMap<_, f64> = fleet.keys().map(|&tier| (tier, 0.0)).collect();
        for &(left, right, mass) in &exchanges {
            *exposure.entry(left).or_default() += mass;
            *exposure.entry(right).or_default() += mass;
        }
        // Several sparse tiers may borrow from the same small reference,
        // especially for review terms. Bound every tier by its own borrowing
        // budget, so references also retain their own evidence. A common
        // scale preserves exchange symmetry and keeps rates convex.
        let scale = exposure
            .iter()
            .filter(|(_, mass)| **mass > 0.0)
            .map(|(&tier, &mass)| borrowed(tier) / mass)
            .fold(1.0, f64::min);
        let mut expected: BTreeMap<_, _> = fleet
            .iter()
            .map(|(&tier, stats)| (tier, stats.counts.get(term.key).copied().unwrap_or(0.0)))
            .collect();
        for &(left, right, mass) in &exchanges {
            let delta = scale * mass * (rates[right] - rates[left]);
            *expected.entry(left).or_default() += delta;
            *expected.entry(right).or_default() -= delta;
        }
        for (&tier, &count) in &expected {
            // Floating-point cancellation at a fully exchanged reference can
            // leave a sub-epsilon negative count, though the rate is convex.
            let rate = count.max(0.0) / units[tier].max(1.0);
            result.rates.entry(tier).or_default().insert(term.key, rate);
            if let Some(adjustment) = result.adjustments.get_mut(tier) {
                adjustment["terms"][term.key] = json!({
                    "units": units[tier],
                    "own_weight": if units[tier] > 0.0 { 1.0 - scale * exposure[tier] / units[tier] } else { 1.0 },
                    "expected_rate": rate,
                });
            }
        }
    }
    result
}
