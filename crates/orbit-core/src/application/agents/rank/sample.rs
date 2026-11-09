//! Resampling: the seeded generator, within-tier draws, and the noise
//! scale calibration (§4.2). Seeds are fixed so the same facts always give
//! the same scales, intervals and bands.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use super::measures::{MEASURE_COUNT, MEASURES, Observations, totals};

/// Draws per resample, for scales and for intervals alike.
pub(super) const DRAWS: usize = 1000;
/// The percentile of weekly estimates a scale is floored at.
const FLOOR_PERCENTILE: f64 = 10.0;
pub(super) const CALIBRATION_SEED: u64 = 0x6f72_6269_745f_7261;
pub(super) const BOOTSTRAP_SEED: u64 = 0x6e6b_5f62_6f6f_7473;

/// SplitMix64: small, seedable and good enough for resampling indices.
pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn seeded(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform index below `n`, which must be positive.
    pub(super) fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// One implemented task as the ranking resamples it.
pub(super) struct Record {
    pub(super) tier: &'static str,
    pub(super) start: DateTime<Utc>,
    pub(super) observed: Observations,
}

/// Records grouped by tier, the strata every resample draws within.
pub(super) fn by_tier<'a>(
    records: impl IntoIterator<Item = &'a Record>,
) -> BTreeMap<&'static str, Vec<&'a Record>> {
    let mut tiers: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for record in records {
        tiers.entry(record.tier).or_default().push(record);
    }
    tiers
}

/// Split `n` across tiers in proportion to `counts`, by largest remainder.
fn allocate(n: usize, counts: &BTreeMap<&'static str, Vec<&Record>>) -> Vec<(&'static str, usize)> {
    let total: usize = counts.values().map(Vec::len).sum();
    if total == 0 {
        return Vec::new();
    }
    let mut shares: Vec<_> = counts
        .iter()
        .map(|(&tier, records)| {
            let exact = n as f64 * records.len() as f64 / total as f64;
            (tier, exact.floor() as usize, exact - exact.floor())
        })
        .collect();
    let assigned: usize = shares.iter().map(|share| share.1).sum();
    let mut order: Vec<usize> = (0..shares.len()).collect();
    order.sort_by(|&a, &b| shares[b].2.total_cmp(&shares[a].2).then(a.cmp(&b)));
    for &index in order.iter().cycle().take(n.saturating_sub(assigned)) {
        shares[index].1 += 1;
    }
    shares.into_iter().map(|(tier, n, _)| (tier, n)).collect()
}

/// Resample `per_tier` records from each tier with replacement.
pub(super) fn draw<'a>(
    rng: &mut Rng,
    strata: &BTreeMap<&'static str, Vec<&'a Record>>,
    per_tier: &[(&'static str, usize)],
) -> Vec<&'a Observations> {
    let mut sample = Vec::new();
    for &(tier, n) in per_tier {
        let Some(records) = strata.get(tier).filter(|records| !records.is_empty()) else {
            continue;
        };
        for _ in 0..n {
            sample.push(&records[rng.below(records.len())].observed);
        }
    }
    sample
}

/// The standard deviation of each measure's residual over `DRAWS` samples
/// of `n_ref` records with the pool's tier mix. `None` for a measure no
/// draw gave units to.
fn reference_deviation(
    pool: &BTreeMap<&'static str, Vec<&Record>>,
    n_ref: usize,
    rng: &mut Rng,
) -> [Option<f64>; MEASURE_COUNT] {
    let per_tier = allocate(n_ref, pool);
    let mut residuals: [Vec<f64>; MEASURE_COUNT] = Default::default();
    for _ in 0..DRAWS {
        let sample = draw(rng, pool, &per_tier);
        for (index, total) in totals(sample).into_iter().enumerate() {
            if let Some(residual) = total.residual(&MEASURES[index]) {
                residuals[index].push(residual);
            }
        }
    }
    std::array::from_fn(|index| deviation(&residuals[index]))
}

fn deviation(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64;
    Some(variance.sqrt())
}

/// The `percentile` (0–100) of `values` by linear interpolation.
pub(super) fn percentile(values: &mut [f64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let rank = percentile / 100.0 * (values.len() - 1) as f64;
    let (low, high) = (rank.floor() as usize, rank.ceil() as usize);
    Some(values[low] + (values[high] - values[low]) * (rank - low as f64))
}

/// One measure's calibrated scale and how it was reached.
#[derive(Clone, Default)]
pub(super) struct Calibrated {
    /// The deviation over the whole calibration window.
    pub(super) raw: Option<f64>,
    /// The 10th percentile of the weekly estimates.
    pub(super) floor: Option<f64>,
    pub(super) weekly: Vec<f64>,
    /// `max(raw, floor)`; `None` when the measure never varied.
    pub(super) scale: Option<f64>,
}

/// Calibrate every measure's noise scale from `records` (the whole fleet's
/// tasks in the calibration window, starting at `since`) for samples of
/// `n_ref` tasks.
pub(super) fn calibrate(
    records: &[&Record],
    since: DateTime<Utc>,
    n_ref: usize,
) -> [Calibrated; MEASURE_COUNT] {
    let mut rng = Rng::seeded(CALIBRATION_SEED);
    let whole = reference_deviation(&by_tier(records.iter().copied()), n_ref, &mut rng);
    let mut weeks: BTreeMap<i64, Vec<&Record>> = BTreeMap::new();
    for &record in records {
        let week = (record.start - since).num_days().div_euclid(7);
        weeks.entry(week).or_default().push(record);
    }
    let weekly: Vec<[Option<f64>; MEASURE_COUNT]> = weeks
        .values()
        .map(|week| reference_deviation(&by_tier(week.iter().copied()), n_ref, &mut rng))
        .collect();
    std::array::from_fn(|index| {
        let mut estimates: Vec<f64> = weekly.iter().filter_map(|week| week[index]).collect();
        let floor = percentile(&mut estimates, FLOOR_PERCENTILE);
        let scale = match (whole[index], floor) {
            (Some(raw), Some(floor)) => Some(raw.max(floor)),
            (raw, floor) => raw.or(floor),
        }
        .filter(|scale| *scale > 0.0);
        Calibrated {
            raw: whole[index],
            floor,
            weekly: estimates,
            scale,
        }
    })
}
