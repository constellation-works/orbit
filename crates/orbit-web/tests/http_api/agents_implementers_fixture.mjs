// Shared contract-shaped data for the DOM and real-browser fixtures.
export const implementerTerms = [
  ['weighted_landed', 'Landed', 1, 'credit'],
  ['first_pass_accepts', 'First-pass accepts', 1, 'credit'],
  ['review_rejects', 'Review rejects', -1, 'problem'],
  ['reviewer_repairs', 'Reviewer repairs', -0.5, 'problem'],
  ['crew_rework', 'Crew-caused rework', -1, 'problem'],
  ['escaped_regressions', 'Escaped regressions', -2, 'problem'],
].map(([key, label, weight, group]) => ({ key, label, weight, group, definition: label }));
const row = (crew, model, rank, landed, cost) => {
  const units = rank == null ? 2 : 24;
  const small = rank == null;
  const counts = { weighted_landed: landed, first_pass_accepts: small ? 1 : 16, review_rejects: small ? 1 : rank === 1 ? 4 : 3, reviewer_repairs: small ? 0 : 2, crew_rework: small ? 0 : 1, escaped_regressions: small ? 0 : 1 };
  return {
    crew, model, rank, units, score_base: 100, score: 0, raw: 0, below_support: rank == null,
    runs_without_model: 0, provisional: null, counts,
    per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, count * 100 / units])),
    stats: [
      { key: 'cost_per_weighted_landed', label: 'Cost per landed task (USD)', value: cost },
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
export function implementerDetail(crew, model) {
  const card = implementerCard();
  const row = card.rows.find(row => row.crew === crew && row.model === model);
  return { ...card, row, terms: card.terms.map(term => ({ ...term,
    count: row.counts[term.key], per_100: row.per_100[term.key], contribution: (row.counts[term.key] - row.expected[term.key]) * 100 / row.units * term.weight,
    tasks: Array.from({ length: term.key === 'review_rejects' ? 1 : row.counts[term.key] }, (_, index) => ({ task_id: `ORB-${index + 1}`, title: 'Preserve the delivery result', status: 'done', count: term.key === 'review_rejects' ? row.counts[term.key] : 1, occurrences: [] })),
  })) };
}
