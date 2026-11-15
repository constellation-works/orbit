use super::*;

use super::rollup::*;

/// One ranked pair: its paths, the reservations holding both (the
/// numerator), how many held either, and how many held each path.
pub(in crate::application::codebase) struct CouplingPair<'a> {
    pub(in crate::application::codebase) paths: (String, String),
    pub(in crate::application::codebase) both: Vec<&'a ReservationFacts>,
    pub(in crate::application::codebase) either: usize,
    pub(in crate::application::codebase) counts: (usize, usize),
}

pub(in crate::application::codebase) struct CouplingPairs<'a> {
    /// Pairs at or above the minimum support, in ranked order.
    pub(in crate::application::codebase) ranked: Vec<CouplingPair<'a>>,
    /// Reservations naming each path, over the counted reservations.
    pub(in crate::application::codebase) singles: BTreeMap<String, usize>,
    below_support: usize,
    counted: usize,
    excluded_large: usize,
}

/// Path pairs reserved together: Jaccard overlap of the reservations naming
/// each, `both ÷ either`. Reservations above [`LARGE_FOOTPRINT_CAP`] files
/// are excluded; pairs reserved together fewer than `min_support` times are
/// omitted.
pub(in crate::application::codebase) fn coupling_pairs<'a>(
    facts: &'a CodebaseFacts,
    params: &Params,
) -> CouplingPairs<'a> {
    let mut singles: BTreeMap<String, usize> = BTreeMap::new();
    let mut pairs: BTreeMap<(String, String), Vec<&ReservationFacts>> = BTreeMap::new();
    let mut counted = 0;
    let mut excluded_large = 0;
    for reservation in &facts.reservations {
        if !params.within(reservation.at) {
            continue;
        }
        if reservation.files.len() > LARGE_FOOTPRINT_CAP {
            excluded_large += 1;
            continue;
        }
        counted += 1;
        let keys: Vec<String> = params.rollup.keys(&reservation.files).into_iter().collect();
        for (index, left) in keys.iter().enumerate() {
            *singles.entry(left.clone()).or_default() += 1;
            for right in &keys[index + 1..] {
                pairs
                    .entry((left.clone(), right.clone()))
                    .or_default()
                    .push(reservation);
            }
        }
    }
    let mut below_support = 0;
    let mut ranked: Vec<CouplingPair> = pairs
        .into_iter()
        .filter_map(|((left, right), both)| {
            if both.len() < params.min_support {
                below_support += 1;
                return None;
            }
            let left_count = singles.get(&left).copied().unwrap_or(both.len());
            let right_count = singles.get(&right).copied().unwrap_or(both.len());
            let either = left_count + right_count - both.len();
            Some(CouplingPair {
                paths: (left, right),
                both,
                either,
                counts: (left_count, right_count),
            })
        })
        .collect();
    ranked.sort_by(|left, right| {
        rate(right.both.len(), right.either)
            .total_cmp(&rate(left.both.len(), left.either))
            .then_with(|| right.both.len().cmp(&left.both.len()))
            .then_with(|| left.paths.cmp(&right.paths))
    });
    CouplingPairs {
        ranked,
        singles,
        below_support,
        counted,
        excluded_large,
    }
}

pub(in crate::application::codebase) fn change_coupling(
    facts: &CodebaseFacts,
    params: &Params,
) -> Value {
    let CouplingPairs {
        ranked,
        below_support,
        counted,
        excluded_large,
        ..
    } = coupling_pairs(facts, params);
    let rows: Vec<Value> = ranked
        .into_iter()
        .take(ROW_LIMIT)
        .map(|pair| {
            let (left, right) = pair.paths;
            merge(
                rate_fields(pair.both.len(), pair.either),
                json!({
                    "paths": [left, right],
                    "reservations": [pair.counts.0, pair.counts.1],
                }),
            )
        })
        .collect();
    json!({
        "rows": rows,
        "below_support": below_support,
        "reservations": counted,
        "excluded_large": excluded_large,
        "large_footprint_cap": LARGE_FOOTPRINT_CAP,
    })
}
