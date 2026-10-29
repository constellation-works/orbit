// Shared contract-shaped data for the DOM and real-browser fixtures.
export const implementerTerms = [
  ['weighted_landed', 'Landed', 1, 'credit'],
  ['first_pass_accepts', 'First-pass accepts', 1, 'credit'],
  ['review_rejects', 'Review rejects', -1, 'problem'],
  ['reviewer_repairs', 'Reviewer repairs', -0.5, 'problem'],
  ['crew_rework', 'Crew-caused rework', -1, 'problem'],
].map(([key, label, weight, group]) => ({ key, label, weight, group, definition: label }));
const costCoverage = (measured, unpriced, missing) => {
  const total = measured + unpriced + missing;
  return [
    { key: 'cost_coverage', label: 'Cost coverage (counted attempts)', value: `${measured} measured · ${unpriced} unpriced · ${missing} missing run of ${total} counted attempts` },
    { key: 'cost_coverage_attempts', label: 'Cost coverage denominator (counted attempts)', value: total },
    { key: 'cost_measured_attempts', label: 'Cost measured attempts', value: measured },
    { key: 'cost_unpriced_attempts', label: 'Cost unpriced attempts', value: unpriced },
    { key: 'cost_missing_run_attempts', label: 'Cost missing-run attempts', value: missing },
  ];
};
const row = (crew, model, rank, landed, cost) => {
  const units = rank == null ? 2 : 24;
  const small = rank == null;
  const counts = { weighted_landed: landed, first_pass_accepts: small ? 1 : 16, review_rejects: small ? 1 : rank === 1 ? 4 : 3, reviewer_repairs: small ? 0 : 2, crew_rework: small ? 0 : 1, escaped_regressions: small ? 0 : 1 };
  const coverage = crew === 'unpriced' ? costCoverage(18, 4, 2) : costCoverage(units, 0, 0);
  return {
    crew, model, rank, units, score_base: 100, score: 0, raw: 0, below_support: rank == null,
    runs_without_model: 0, provisional: null, counts,
    per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, count * 100 / units])),
    stats: [
      { key: 'escaped_regressions', label: 'Escaped regressions (severity-weighted)', value: counts.escaped_regressions },
      { key: 'regression_residual', label: 'Regression residual (actual − expected)', value: 0 },
      { key: 'diff_economy', label: 'Diff economy (× fleet median)', value: crew === 'unpriced' ? null : 2 },
      { key: 'cost_per_weighted_landed', label: 'Cost per landed task (USD)', value: cost },
      ...coverage,
      { key: 'median_run_seconds', label: 'Median completed run (seconds)', value: 540 },
      { key: 'tasks_by_tier', label: 'Tasks by tier', value: `medium: ${units}` },
    ],
  };
};
export function implementerCard() {
  const rows = [
    row('sol', 'model-b', 1, 22, 10),
    row('sol', 'model-a', 2, 20, 5),
    row('peer', 'model-c', 3, 20, 5),
    row('costly', 'model-d', 4, 18, 15),
    row('unpriced', 'model-e', 5, 16, null),
    row('small-sample', 'model-f', null, 1, 0),
  ];
  const total = rows.reduce((sum, row) => sum + row.units, 0);
  for (const row of rows) {
    row.expected = Object.fromEntries(implementerTerms.map(term => [term.key, rows.reduce((sum, peer) => sum + peer.counts[term.key], 0) * row.units / total]));
    row.raw = 100 + implementerTerms.reduce((sum, term) => sum + term.weight * (row.counts[term.key] - row.expected[term.key]) * 100 / row.units, 0);
  }
  const fleet = rows.reduce((sum, row) => sum + row.units * row.raw, 0) / rows.reduce((sum, row) => sum + row.units, 0);
  rows.forEach(row => { row.score = Math.min(row.raw, (row.units * row.raw + 30 * fleet) / (row.units + 30)); });
  return {
    role: 'implementers', unit: 'implemented tasks', status: 'ok', window: '30d', requested_window: '30d',
    since: '2026-09-19T08:00:00Z', computed_at: '2026-10-19T08:00:00.000Z', min_support: 10,
    cache_ttl_seconds: 300, tasks_scanned: 200, fleet_average: fleet, terms: implementerTerms,
    rows,
  };
}
const radarRow = (crew, model, rank, counts, reviewedUnits) => {
  const units = 40;
  return {
    crew, model, rank, units, score_base: 100, score: 0, raw: 0, below_support: false,
    runs_without_model: 0, provisional: null, counts, reviewed_units: reviewedUnits,
    per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, count * 100 / units])),
    stats: [
      { key: 'escaped_regressions', label: 'Escaped regressions (severity-weighted)', value: counts.escaped_regressions },
      { key: 'regression_residual', label: 'Regression residual (actual − expected)', value: 0 },
      { key: 'diff_economy', label: 'Diff economy (× fleet median)', value: crew === 'unpriced' ? null : 2 },
      { key: 'cost_per_weighted_landed', label: 'Cost per landed task (USD)', value: 5 },
      ...costCoverage(units, 0, 0),
      { key: 'median_run_seconds', label: 'Median completed run (seconds)', value: 540 },
      { key: 'tasks_by_tier', label: 'Tasks by tier', value: `medium: ${units}` },
    ],
  };
};
function finishCard(rows, computedAt) {
  const total = rows.reduce((sum, row) => sum + row.units, 0);
  for (const row of rows) {
    row.expected = Object.fromEntries(implementerTerms.map(term => [term.key, rows.reduce((sum, peer) => sum + peer.counts[term.key], 0) * row.units / total]));
    row.raw = 100 + implementerTerms.reduce((sum, term) => sum + term.weight * (row.counts[term.key] - row.expected[term.key]) * 100 / row.units, 0);
  }
  const fleet = rows.reduce((sum, row) => sum + row.units * row.raw, 0) / total;
  rows.forEach(row => { row.score = Math.min(row.raw, (row.units * row.raw + 30 * fleet) / (row.units + 30)); });
  return {
    role: 'implementers', unit: 'implemented tasks', status: 'ok', window: '30d', requested_window: '30d',
    since: '2026-09-19T08:00:00Z', computed_at: computedAt, min_support: 10,
    cache_ttl_seconds: 300, tasks_scanned: 200, fleet_average: fleet, terms: implementerTerms, rows,
  };
}
// Live-shaped fleet (ORB-18538): the rank-2 sol row is above the median on
// landed and escaped regressions and has no reviewed deliveries.
export function implementerRadarCard() {
  return finishCard([
    radarRow('top', 'model-t', 1, { weighted_landed: 32, first_pass_accepts: 20, review_rejects: 2, reviewer_repairs: 2, crew_rework: 2, escaped_regressions: 1 }, 40),
    radarRow('sol', 'gpt-6.1-sol', 2, { weighted_landed: 36, first_pass_accepts: 0, review_rejects: 0, reviewer_repairs: 0, crew_rework: 4, escaped_regressions: 0 }, 0),
    radarRow('peer-a', 'model-a', 3, { weighted_landed: 28, first_pass_accepts: 16, review_rejects: 4, reviewer_repairs: 4, crew_rework: 4, escaped_regressions: 2 }, 32),
    radarRow('peer-b', 'model-b', 4, { weighted_landed: 24, first_pass_accepts: 12, review_rejects: 6, reviewer_repairs: 4, crew_rework: 6, escaped_regressions: 3 }, 30),
    radarRow('peer-c', 'model-c', 5, { weighted_landed: 20, first_pass_accepts: 10, review_rejects: 8, reviewer_repairs: 6, crew_rework: 8, escaped_regressions: 4 }, 28),
  ], '2026-10-19T09:00:00.000Z');
}
// The same fleet plus one extreme outlier crew (70 escaped regressions and
// 200 crew rework per 100 in the live report, scaled to this fixture).
export function implementerOutlierCard() {
  const card = implementerRadarCard();
  card.rows.push(radarRow('outlier', 'model-x', 6, { weighted_landed: 4, first_pass_accepts: 0, review_rejects: 40, reviewer_repairs: 40, crew_rework: 80, escaped_regressions: 28 }, 40));
  return finishCard(card.rows, '2026-10-19T09:05:00.000Z');
}

// Tier crew means deliberately reverse the all-tier leaders; one row has
// only hard work so selecting low must hide it. Cell n sums to tier support.
export function implementerMatrix(card = implementerCard()) {
  return { role: 'implementers', requested_window: card.requested_window, by_type: true,
    cells: card.rows.flatMap((row, index) => {
      const mix = [ ['low', 'medium'], ['low', 'hard'], ['medium', 'xhard'], ['low', 'xhard'], ['hard'], ['low', 'hard'] ][index] || ['medium'];
      return mix.map(tier => ({ crew: row.crew, model: row.model, tier, type: 'other', n: row.units / mix.length, crew_mean: 90 + index }));
    }) };
}
