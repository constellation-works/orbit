// Cost and tier comparisons are context only, never shared score terms.
import { el } from './common.js';

const stat = (row, key) => (row.stats || []).find(entry => entry.key === key)?.value;
const finite = value => typeof value === 'number' && Number.isFinite(value);
const number = value => finite(value) ? new Intl.NumberFormat('en-US', { maximumFractionDigits: 2 }).format(value) : '—';
const percent = value => finite(value) ? `${number(value)}%` : '—';
const name = row => `${row.crew} · ${row.model}`;

function render({ rows }) {
  const points = rows.map(row => ({ row, cost: stat(row, 'cost_per_weighted_landed') }))
    .filter(point => finite(point.cost) && point.cost >= 0 && finite(point.row.score));
  const supported = points.filter(point => !point.row.below_support);
  const frontier = supported.filter(point => !supported.some(other =>
    other.cost <= point.cost && other.row.score >= point.row.score
    && (other.cost < point.cost || other.row.score > point.row.score)))
    .sort((a, b) => a.cost - b.cost || a.row.score - b.row.score);
  const maxCost = Math.max(1, ...points.map(point => point.cost));
  const minScore = Math.min(0, ...points.map(point => point.row.score));
  const maxScore = Math.max(minScore + 1, ...points.map(point => point.row.score));
  const x = point => 8 + point.cost / maxCost * 84;
  const y = point => 90 - (point.row.score - minScore) / (maxScore - minScore) * 80;
  const plot = el('div', { class: 'agents-cost-plot', role: 'img', 'aria-label': 'Score against cost per weighted landed task. Higher score and lower cost are better. Dashed line: supported frontier.' });
  // A square area keeps percentage coordinates identical for lines and dots.
  frontier.slice(1).forEach((point, index) => {
    const previous = frontier[index];
    const dx = x(point) - x(previous);
    const dy = y(point) - y(previous);
    plot.append(el('span', { class: 'agents-cost-frontier', 'aria-hidden': 'true', style: {
      left: `${x(previous)}%`, top: `${y(previous)}%`, width: `${Math.hypot(dx, dy)}%`,
      transform: `rotate(${Math.atan2(dy, dx)}rad)`,
    } }));
  });
  const legend = el('ul', { class: 'agents-cost-legend' });
  points.forEach(point => {
    const onFrontier = frontier.includes(point);
    const label = `${name(point.row)}: score ${number(point.row.score)}, $${number(point.cost)} per weighted landed task${onFrontier ? ', frontier' : ''}${point.row.below_support ? ', below support' : ''}`;
    const dot = el('span', { class: `agents-cost-point${onFrontier ? ' is-frontier' : ''}${point.row.below_support ? ' agents-row-below' : ''}`, title: label, style: { left: `${x(point)}%`, top: `${y(point)}%` } });
    dot.dataset.row = `${point.row.crew}\0${point.row.model}`;
    plot.append(dot);
    legend.append(el('li', { class: point.row.below_support ? 'agents-row-below' : '', text: label }));
  });
  const table = el('table', { class: 'agents-tier-table' });
  table.append(el('thead', {}, [el('tr', {}, ['Crew · model', 'Tier', 'Tasks', 'First reviews', 'Needing changes', 'Row', 'Same-tier fleet'].map(text => el('th', { text, scope: 'col' })))]));
  const body = el('tbody');
  for (const row of rows) {
    const tiers = stat(row, 'tier_comparison');
    for (const tier of Array.isArray(tiers) ? tiers : []) {
      const tr = el('tr', { class: row.below_support ? 'agents-row-below' : '' }, [
        name(row), tier.tier, number(tier.tasks), number(tier.first_reviews), number(tier.needing_changes),
        percent(tier.needing_changes_percent), percent(tier.fleet_needing_changes_percent),
      ].map(text => el('td', { text })));
      tr.dataset.row = `${row.crew}\0${row.model}`;
      tr.dataset.tier = tier.tier;
      body.append(tr);
    }
  }
  table.append(body);
  return [
    el('p', { class: 'agents-muted', text: 'Higher score, lower cost. The dashed frontier joins supported rows that no other supported row matches or beats on both measures and improves on at least one. Cost never enters the score.' }),
    ...(points.length ? [plot,
      el('div', { class: 'agents-cost-axis', text: `Score ${number(minScore)}–${number(maxScore)} ↑ · Cost $0–$${number(maxCost)} →` }), legend]
      : [el('p', { class: 'agents-cost-empty', text: 'No rows have both recorded cost and weighted landed work in this window.' })]),
    el('p', { class: 'agents-muted', text: `${rows.length - points.length} rows lack comparable cost. Grey points are below support and do not set the frontier.` }),
    el('h4', { text: 'First reviews by tier · shown, not scored' }),
    el('p', { class: 'agents-muted', text: 'Rejects and reviewer repairs need changes. The fleet rate includes all first reviews in the same tier, including this row; tasks with no review have no rate.' }),
    el('div', { class: 'agents-table-wrap', tabindex: '0', 'aria-label': 'First reviews by tier' }, [table]),
  ];
}

export const implementersPanel = { title: 'Score against cost', render };
