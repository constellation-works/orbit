//! The statistics a readout reports: means and medians with seeded
//! percentile-bootstrap intervals, Wilson intervals for rates, a ratio of
//! sums with a paired task bootstrap, a post-stratified difference between
//! two arms, and the chi-square test of observed arm counts against their
//! weights.

/// Bootstrap resamples per interval.
const RESAMPLES: usize = 2_000;
/// The 97.5th percentile of the standard normal distribution.
const Z_95: f64 = 1.959_963_984_540_054;
/// Every bootstrap stream starts from this seed, mixed with the label of what
/// it resamples, so a readout is reproducible and its streams independent.
const SEED: u64 = 0x0e57_a11e_2026_1026;

/// A 95% interval, low then high.
pub(super) type Interval = [f64; 2];

/// splitmix64: small, fast and fully determined by its seed.
struct Stream(u64);

impl Stream {
    fn seeded(label: &str) -> Self {
        // FNV-1a over the label, then mixed into the fixed seed.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in label.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        Self(SEED ^ hash)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// An index below `len`, which must be positive.
    fn index(&mut self, len: usize) -> usize {
        // `len` fits in u64 and the remainder is below it, so both casts hold.
        (self.next() % len as u64) as usize
    }

    fn resample(&mut self, values: &[f64], into: &mut Vec<f64>) {
        into.clear();
        into.extend((0..values.len()).map(|_| values[self.index(values.len())]));
    }
}

pub(super) fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

pub(super) fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    Some(if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    })
}

/// The 2.5th and 97.5th percentiles of `draws`, linearly interpolated.
fn percentile_interval(mut draws: Vec<f64>) -> Option<Interval> {
    if draws.is_empty() {
        return None;
    }
    draws.sort_by(f64::total_cmp);
    let at = |q: f64| {
        let position = q * (draws.len() - 1) as f64;
        let low = position.floor() as usize;
        let high = position.ceil() as usize;
        draws[low] + (draws[high] - draws[low]) * (position - low as f64)
    };
    Some([at(0.025), at(0.975)])
}

/// A percentile-bootstrap interval of `statistic` over `values`, resampled by
/// task; `None` below two values.
pub(super) fn bootstrap(
    label: &str,
    values: &[f64],
    statistic: fn(&[f64]) -> Option<f64>,
) -> Option<Interval> {
    if values.len() < 2 {
        return None;
    }
    let mut stream = Stream::seeded(label);
    let mut sample = Vec::with_capacity(values.len());
    let draws = (0..RESAMPLES)
        .filter_map(|_| {
            stream.resample(values, &mut sample);
            statistic(&sample)
        })
        .collect();
    percentile_interval(draws)
}

/// The Wilson score interval of `successes` in `n` trials.
pub(super) fn wilson(successes: usize, n: usize) -> Option<Interval> {
    if n == 0 {
        return None;
    }
    let trials = n as f64;
    let p = successes as f64 / trials;
    let z2 = Z_95 * Z_95;
    let centre = (p + z2 / (2.0 * trials)) / (1.0 + z2 / trials);
    let half =
        Z_95 * (p * (1.0 - p) / trials + z2 / (4.0 * trials * trials)).sqrt() / (1.0 + z2 / trials);
    // The bounds are exactly 0 and 1 at the edges; rounding would leave
    // residue there.
    let low = if successes == 0 {
        0.0
    } else {
        (centre - half).max(0.0)
    };
    let high = if successes == n {
        1.0
    } else {
        (centre + half).min(1.0)
    };
    Some([low, high])
}

/// One task's numerator and its reviewable-line denominator.
#[derive(Clone, Copy)]
pub(super) struct WeightedPair {
    pub(super) numerator: f64,
    pub(super) lines: u64,
}

/// Sum of numerators divided by sum of lines. `None` when the lines sum to
/// zero, which included pairs never do.
pub(super) fn ratio_of_sums(pairs: &[WeightedPair]) -> Option<f64> {
    let lines: u64 = pairs.iter().map(|pair| pair.lines).sum();
    (lines > 0).then(|| pairs.iter().map(|pair| pair.numerator).sum::<f64>() / lines as f64)
}

/// A percentile-bootstrap interval of the ratio of sums, resampling tasks.
/// `None` below two pairs.
pub(super) fn paired_ratio_bootstrap(label: &str, pairs: &[WeightedPair]) -> Option<Interval> {
    if pairs.len() < 2 {
        return None;
    }
    let mut stream = Stream::seeded(label);
    let draws = (0..RESAMPLES)
        .filter_map(|_| {
            let sample: Vec<WeightedPair> = (0..pairs.len())
                .map(|_| pairs[stream.index(pairs.len())])
                .collect();
            ratio_of_sums(&sample)
        })
        .collect();
    percentile_interval(draws)
}

/// Treatment ratio minus control ratio. The interval resamples each arm's
/// tasks on its own. `None` when either arm has no lines. The interval is
/// `None` while either arm has fewer than two pairs.
pub(super) fn paired_ratio_difference(
    label: &str,
    control: &[WeightedPair],
    treatment: &[WeightedPair],
) -> Option<(f64, Option<Interval>)> {
    let estimate = ratio_of_sums(treatment)? - ratio_of_sums(control)?;
    if control.len() < 2 || treatment.len() < 2 {
        return Some((estimate, None));
    }
    let mut stream = Stream::seeded(label);
    let draws = (0..RESAMPLES)
        .filter_map(|_| {
            let control_sample: Vec<WeightedPair> = (0..control.len())
                .map(|_| control[stream.index(control.len())])
                .collect();
            let treatment_sample: Vec<WeightedPair> = (0..treatment.len())
                .map(|_| treatment[stream.index(treatment.len())])
                .collect();
            Some(ratio_of_sums(&treatment_sample)? - ratio_of_sums(&control_sample)?)
        })
        .collect();
    Some((estimate, percentile_interval(draws)))
}

/// One stratum's control and treatment values.
pub(super) struct Cell<'a> {
    pub(super) control: &'a [f64],
    pub(super) treatment: &'a [f64],
}

/// The treatment-minus-control difference in means, post-stratified: each
/// stratum's difference weighted by its share of both arms' tasks. Only
/// strata where both arms have a value count. Returns the estimate and its
/// stratified-bootstrap interval (`None` while any used stratum has fewer
/// than two values in an arm).
pub(super) fn stratified_difference(
    label: &str,
    cells: &[Cell<'_>],
) -> Option<(f64, Option<Interval>)> {
    let used: Vec<&Cell<'_>> = cells
        .iter()
        .filter(|cell| !cell.control.is_empty() && !cell.treatment.is_empty())
        .collect();
    let total: usize = used
        .iter()
        .map(|cell| cell.control.len() + cell.treatment.len())
        .sum();
    if total == 0 {
        return None;
    }
    let weight =
        |cell: &Cell<'_>| (cell.control.len() + cell.treatment.len()) as f64 / total as f64;
    let estimate = used
        .iter()
        .map(|cell| Some(weight(cell) * (mean(cell.treatment)? - mean(cell.control)?)))
        .sum::<Option<f64>>()?;
    let resamplable = used
        .iter()
        .all(|cell| cell.control.len() >= 2 && cell.treatment.len() >= 2);
    if !resamplable {
        return Some((estimate, None));
    }
    let mut stream = Stream::seeded(label);
    let mut control = Vec::new();
    let mut treatment = Vec::new();
    let draws = (0..RESAMPLES)
        .filter_map(|_| {
            used.iter()
                .map(|cell| {
                    stream.resample(cell.control, &mut control);
                    stream.resample(cell.treatment, &mut treatment);
                    Some(weight(cell) * (mean(&treatment)? - mean(&control)?))
                })
                .sum::<Option<f64>>()
        })
        .collect();
    Some((estimate, percentile_interval(draws)))
}

/// Pearson's chi-square of `observed` counts against `expected` ones, and
/// its upper-tail p-value with `observed.len() - 1` degrees of freedom summed
/// over every group. Groups are (observed, expected) pairs per stratum.
pub(super) fn chi_square(groups: &[Vec<(f64, f64)>]) -> Option<(f64, u32, f64)> {
    let mut statistic = 0.0;
    let mut df = 0u32;
    for group in groups {
        let cells: Vec<_> = group
            .iter()
            .filter(|(_, expected)| *expected > 0.0)
            .collect();
        if cells.len() < 2 {
            continue;
        }
        statistic += cells
            .iter()
            .map(|(observed, expected)| (observed - expected).powi(2) / expected)
            .sum::<f64>();
        df += u32::try_from(cells.len() - 1).unwrap_or(u32::MAX);
    }
    if df == 0 {
        return None;
    }
    Some((
        statistic,
        df,
        upper_gamma(f64::from(df) / 2.0, statistic / 2.0),
    ))
}

/// The regularized upper incomplete gamma function Q(a, x).
fn upper_gamma(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    let log_prefix = a * x.ln() - x - ln_gamma(a);
    if x < a + 1.0 {
        // Series for P(a, x).
        let mut term = 1.0 / a;
        let mut sum = term;
        let mut denominator = a;
        for _ in 0..500 {
            denominator += 1.0;
            term *= x / denominator;
            sum += term;
            if term.abs() < sum.abs() * 1e-15 {
                break;
            }
        }
        (1.0 - sum * log_prefix.exp()).clamp(0.0, 1.0)
    } else {
        // Lentz's continued fraction for Q(a, x).
        let tiny = 1e-300;
        let mut b = x + 1.0 - a;
        let mut c = 1.0 / tiny;
        let mut d = 1.0 / b;
        let mut h = d;
        for i in 1..500 {
            let i = f64::from(i);
            let an = -i * (i - a);
            b += 2.0;
            d = an * d + b;
            if d.abs() < tiny {
                d = tiny;
            }
            c = b + an / c;
            if c.abs() < tiny {
                c = tiny;
            }
            d = 1.0 / d;
            let delta = d * c;
            h *= delta;
            if (delta - 1.0).abs() < 1e-15 {
                break;
            }
        }
        (log_prefix.exp() * h).clamp(0.0, 1.0)
    }
}

/// ln Γ(x) for x > 0 (Lanczos, g = 7).
fn ln_gamma(x: f64) -> f64 {
    const COEFFICIENTS: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let t = x + 7.5;
    let series = COEFFICIENTS[1..]
        .iter()
        .enumerate()
        .fold(COEFFICIENTS[0], |sum, (i, c)| {
            sum + c / (x + i as f64 + 1.0)
        });
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + series.ln()
}
