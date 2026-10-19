// Shared contract-shaped data for the DOM and real-browser fixtures.
export const implementerTerms = [
  ['weighted_landed', 'Weighted landed', 1, 'credit'],
  ['first_pass_accepts', 'First-pass accepts', 1, 'credit'],
  ['review_rejects', 'Review rejects', -1, 'problem'],
  ['reviewer_repairs', 'Reviewer repairs', -0.5, 'problem'],
  ['crew_rework', 'Crew-caused rework', -1, 'problem'],
  ['escaped_regressions', 'Escaped regressions', -2, 'problem'],
].map(([key, label, weight, group]) => ({ key, label, weight, group, definition: label }));
const row = (crew, model, rank, landed, cost) => {
  const units = rank == null ? 2 : 24;
  const small = rank == null;
  const counts = { weighted_landed: landed, first_pass_accepts: small ? 1 : 16, review_rejects: small ? 1 : 3, reviewer_repairs: small ? 0 : 2, crew_rework: small ? 0 : 1, escaped_regressions: small ? 0 : 1 };
  return {
    crew, model, rank, units, score: 0, raw: implementerTerms.reduce((sum, term) => sum + term.weight * counts[term.key] * 100 / units, 0), below_support: rank == null,
    runs_without_model: 0, provisional: null, counts,
    per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, count * 100 / units])),
    stats: [
      { key: 'cost_per_weighted_landed', label: 'Cost per weighted landed task (USD)', value: cost },
      { key: 'median_run_seconds', label: 'Median completed run (seconds)', value: 540 },
      { key: 'tasks_by_tier', label: 'Tasks by tier', value: `medium: ${units}` },
      { key: 'tier_comparison', label: 'First reviews by tier', value: [
        { tier: 'medium', tasks: units, first_reviews: small ? 2 : 20, needing_changes: small ? 1 : 4, needing_changes_percent: small ? 50 : 20, fleet_needing_changes_percent: 20.59 },
        { tier: 'hard', tasks: 0, first_reviews: 0, needing_changes: 0, needing_changes_percent: null, fleet_needing_changes_percent: null },
      ] },
    ],
  };
};
export function implementerCard() {
  const rows = [
    row('sol', 'model-b', 1, 30, 10),
    row('sol', 'model-a', 2, 20, 5),
    row('peer', 'model-c', 3, 20, 5),
    row('costly', 'model-d', 4, 18, 15),
    row('unpriced', 'model-e', 5, 16, null),
    row('small-sample', 'model-f', null, 4, 0),
  ];
  const fleet = rows.reduce((sum, row) => sum + row.units * row.raw, 0) / rows.reduce((sum, row) => sum + row.units, 0);
  rows.forEach(row => { row.score = (row.units * row.raw + 10 * fleet) / (row.units + 10); });
  return {
    role: 'implementers', unit: 'implemented tasks', status: 'ok', window: '30d', requested_window: '30d',
    since: '2026-09-19T08:00:00Z', computed_at: '2026-10-19T08:00:00.000Z', min_support: 10,
    cache_ttl_seconds: 300, tasks_scanned: 200, fleet_average: fleet, terms: implementerTerms,
    rows,
  };
}
export function implementerDetail(crew, model) {
  const card = implementerCard();
  const row = card.rows.find(row => row.crew === crew && row.model === model);
  return { ...card, row, terms: card.terms.map(term => ({ ...term,
    count: row.counts[term.key], per_100: row.per_100[term.key], contribution: row.per_100[term.key] * term.weight,
    tasks: term.key === 'review_rejects' ? [{ task_id: 'ORB-1', title: 'Preserve the delivery result', status: 'done', count: row.counts[term.key], occurrences: [] }] : [],
  })) };
}
