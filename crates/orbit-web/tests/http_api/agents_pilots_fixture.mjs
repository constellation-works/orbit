// Shared contract-shaped Pilots data for the DOM and real-browser fixtures:
// three crews, two with an assessed-by-observed tier matrix whose observed
// columns keep the assessed proportions, one with no placed task. The
// warning terms are rated per 100 settled assessments: luna has enough to
// score them, haiku's are provisional, and no block-triage record ran.
const SETTLED = 'settled assessments';
export const pilotTerms = [
  ['f1_lift', 'F1 lift', 1, 'credit'],
  ['tier_calibration', 'Tier calibration', 0.25, 'credit'],
  ['confirmed_warnings', 'Confirmed warnings', 3, 'credit', SETTLED],
  ['dismissed_warnings', 'Dismissed warnings', -1, 'problem', SETTLED],
  ['missed_scope_blocks', 'Missed scope blocks', -3, 'problem', SETTLED],
  ['costly_misses', 'Costly misses', -1, 'problem'],
  ['lock_wait_hours', 'Lock-wait hours', -1, 'problem'],
].map(([key, label, weight, group, unit]) => ({ key, label, weight, group, definition: label, ...(unit ? { unit } : {}) }));
const WARNING_TERMS = pilotTerms.filter(term => term.unit).map(term => term.key);
const MIN_SETTLED = 10;

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

const row = (crew, model, rank, units, counts, recall, tiers, warnings) => ({
  crew, model, rank, units, score: 0, raw: 0, below_support: rank == null,
  runs_without_model: 0, provisional: null, counts,
  per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, count * 100 / Math.max(1, WARNING_TERMS.includes(key) ? warnings.settled : units)])),
  term_units: Object.fromEntries(WARNING_TERMS.map(key => [key, warnings.settled])),
  provisional_terms: warnings.settled < MIN_SETTLED ? WARNING_TERMS : [],
  stats: [
    { key: 'recall_percent', label: 'Recall (%)', value: recall },
    { key: 'precision_percent', label: 'Precision (%)', value: 80 },
    { key: 'f1_percent', label: 'F1 (%)', value: 84.21 },
    { key: 'filer_f1_percent', label: "Filer's F1 (%)", value: 40 },
    { key: 'cost_per_assessment', label: 'Cost per assessment (USD)', value: null },
    { key: 'tier_matrix', label: 'Assessed against observed tier', value: tiers },
    { key: 'warning_precision_percent', label: 'Warning precision (%)', value: warnings.precision },
    { key: 'holds_per_100_assessments', label: 'Holds per 100 assessments', value: warnings.holds },
    { key: 'settled_assessments', label: 'Settled assessments', value: warnings.settled },
    { key: 'excluded_assessments', label: 'Excluded or pending assessments', value: warnings.excluded },
    { key: 'missed_scope_block_count', label: 'Missed scope blocks', value: counts.missed_scope_blocks },
  ],
});

export function pilotCard() {
  const rows = [
    row('luna', 'luna-model', 1, 14, { f1_lift: 5.6, tier_calibration: 9, confirmed_warnings: 8, dismissed_warnings: 4, missed_scope_blocks: 0, costly_misses: 1, lock_wait_hours: 3.5 }, 88.89,
      matrix(['low', 'medium', 'hard'], [[3, 1, 0], [1, 4, 1], [0, 1, 2]], 1),
      { settled: 20, excluded: 5, precision: 66.67, holds: 40 }),
    row('haiku', 'claude-haiku-5-5', 2, 12, { f1_lift: 1.2, tier_calibration: 4, confirmed_warnings: 2, dismissed_warnings: 3, missed_scope_blocks: 1, costly_misses: 2, lock_wait_hours: 0 }, 66.67,
      matrix(['low', 'medium', 'hard'], [[1, 2, 0], [2, 2, 1], [0, 1, 1]], 2),
      { settled: 6, excluded: 2, precision: 40, holds: 62.5 }),
    row('crew unknown', 'model unknown', null, 2, { f1_lift: 1, tier_calibration: 0, confirmed_warnings: 0, dismissed_warnings: 0, missed_scope_blocks: 0, costly_misses: 0, lock_wait_hours: 0 }, 100,
      matrix([], [], 2),
      { settled: 0, excluded: 0, precision: null, holds: null }),
  ];
  const scored = (term, each) => !each.provisional_terms.includes(term.key);
  for (const each of rows) each.raw = pilotTerms.filter(term => scored(term, each)).reduce((sum, term) => sum + term.weight * each.per_100[term.key], 0);
  const fleet = rows.reduce((sum, each) => sum + each.units * each.raw, 0) / rows.reduce((sum, each) => sum + each.units, 0);
  rows.forEach(each => { each.score = Math.min(each.raw, (each.units * each.raw + 30 * fleet) / (each.units + 30)); });
  return {
    role: 'pilots', unit: 'assessed tasks', status: 'ok', window: '30d', requested_window: '30d',
    since: '2026-09-19T08:00:00Z', computed_at: '2026-10-19T08:00:00.000Z', min_support: 10,
    cache_ttl_seconds: 300, tasks_scanned: 200, fleet_average: fleet, terms: pilotTerms, rows,
    warning_min_support: MIN_SETTLED,
    sources: { missed_scope_blocks: { available: false, triage_records: 0,
      detail: 'Unavailable: no blocked-task triage record in the window, so missed scope blocks read 0 because nothing measured them, not because no clean assessment missed one.' } },
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
    count: selected.counts[term.key], per_100: selected.per_100[term.key],
    contribution: selected.provisional_terms.includes(term.key) ? 0 : selected.per_100[term.key] * term.weight,
    tasks: tasks[term.key] || [],
    ...(term.unit ? { units: selected.term_units[term.key], provisional: selected.provisional_terms.includes(term.key) } : {}),
  })) };
}
