// Shared contract-shaped Pilots data for the DOM and real-browser fixtures:
// three crews, two with an assessed-by-observed tier matrix whose observed
// columns keep the assessed proportions, one with no placed task.
export const pilotTerms = [
  ['f1_lift', 'F1 lift', 1, 'credit'],
  ['tier_calibration', 'Tier calibration', 0.25, 'credit'],
  ['costly_misses', 'Costly misses', -1, 'problem'],
  ['lock_wait_hours', 'Lock-wait hours', -1, 'problem'],
].map(([key, label, weight, group]) => ({ key, label, weight, group, definition: label }));

const matrix = (tiers, cells, notPlaced) => {
  let onTier = 0;
  let tooEasy = 0;
  let tooHard = 0;
  cells.forEach((line, assessed) => line.forEach((count, observed) => {
    if (assessed === observed) onTier += count;
    else if (assessed < observed) tooEasy += count;
    else tooHard += count;
  }));
  const placed = onTier + tooEasy + tooHard;
  return {
    tiers, rows: tiers.map((tier, at) => ({ assessed: tier, observed: cells[at] })),
    placed, not_placed: notPlaced, on_tier: onTier, too_easy: tooEasy, too_hard: tooHard,
  };
};

const row = (crew, model, rank, units, counts, recall, tiers) => ({
  crew, model, rank, units, score: 0, raw: 0, below_support: rank == null,
  runs_without_model: 0, provisional: null, counts,
  per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, count * 100 / units])),
  stats: [
    { key: 'recall_percent', label: 'Recall (%)', value: recall },
    { key: 'precision_percent', label: 'Precision (%)', value: 80 },
    { key: 'f1_percent', label: 'F1 (%)', value: 84.21 },
    { key: 'filer_f1_percent', label: "Filer's F1 (%)", value: 40 },
    { key: 'cost_per_assessment', label: 'Cost per assessment (USD)', value: null },
    { key: 'tier_matrix', label: 'Assessed against observed tier', value: tiers },
  ],
});

export function pilotCard() {
  const rows = [
    row('luna', 'luna-model', 1, 14, { f1_lift: 5.6, tier_calibration: 9, costly_misses: 1, lock_wait_hours: 3.5 }, 88.89,
      matrix(['low', 'medium', 'hard'], [[3, 1, 0], [1, 4, 1], [0, 1, 2]], 1)),
    row('haiku', 'claude-haiku-5-5', 2, 12, { f1_lift: 1.2, tier_calibration: 4, costly_misses: 2, lock_wait_hours: 0 }, 66.67,
      matrix(['low', 'medium', 'hard'], [[1, 2, 0], [2, 2, 1], [0, 1, 1]], 2)),
    row('crew unknown', 'model unknown', null, 2, { f1_lift: 1, tier_calibration: 0, costly_misses: 0, lock_wait_hours: 0 }, 100,
      matrix([], [], 2)),
  ];
  for (const each of rows) each.raw = pilotTerms.reduce((sum, term) => sum + term.weight * each.per_100[term.key], 0);
  const fleet = rows.reduce((sum, each) => sum + each.units * each.raw, 0) / rows.reduce((sum, each) => sum + each.units, 0);
  rows.forEach(each => { each.score = (each.units * each.raw + 10 * fleet) / (each.units + 10); });
  return {
    role: 'pilots', unit: 'assessed tasks', status: 'ok', window: '30d', requested_window: '30d',
    since: '2026-09-19T08:00:00Z', computed_at: '2026-10-19T08:00:00.000Z', min_support: 10,
    cache_ttl_seconds: 300, tasks_scanned: 200, fleet_average: fleet, terms: pilotTerms, rows,
  };
}

export function pilotDetail(crew, model) {
  const card = pilotCard();
  const selected = card.rows.find(each => each.crew === crew && each.model === model);
  const tasks = {
    lock_wait_hours: [{ task_id: 'ORB-41', title: 'Reserve the shared parser', status: 'done', count: 3.5, occurrences: [
      { at: '2026-10-12T09:00:00Z', detail: 'ORB-52 waited 3.50 h on src/parser.rs (admitted)', run_id: 'jrun-pilot-1' },
    ] }],
    costly_misses: [{ task_id: 'ORB-43', title: 'Widen the merge queue', status: 'done', count: 1, occurrences: [
      { at: '2026-10-13T09:00:00Z', detail: 'src/queue.rs: uncovered, then a lock conflict', run_id: 'jrun-pilot-2' },
    ] }],
  };
  return { ...card, row: selected, terms: card.terms.map(term => ({ ...term,
    count: selected.counts[term.key], per_100: selected.per_100[term.key], contribution: selected.per_100[term.key] * term.weight,
    tasks: tasks[term.key] || [],
  })) };
}
