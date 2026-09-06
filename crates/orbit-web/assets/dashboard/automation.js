// Persisted delivery observations and accepted coverage, shared by both consumers.
import { el, withWorkspace } from './common.js';

const field = (label, value) => el('div', { class: 'operation-field' }, [
  el('span', { class: 'operation-field-label', text: label }),
  el('span', { class: 'operation-field-value', text: String(value ?? 'Not observed') }),
]);
const revision = value => value ? `${value.commit} (tree ${value.tree})` : 'Not observed';

export function renderAutomation(diagnostic) {
  if (!diagnostic) return null;
  const panel = el('details', { class: 'automation-diagnostic' });
  panel.appendChild(el('summary', { text: `${diagnostic.state?.members ? 'State automation' : 'Delivery coverage'} · ${diagnostic.reason || 'unknown'}` }));
  if (diagnostic.error) panel.appendChild(el('p', { text: diagnostic.error }));
  const state = diagnostic.state;
  if (!state) {
    panel.appendChild(el('p', { text: 'No baseline recorded. Preview before enabling this consumer.' }));
    return panel;
  }
  const members = state.members;
  if (members) {
    const assessed = Object.entries(members.assessed || {}).filter(([key, value]) => !members.pending?.[key] || members.pending[key].fingerprint === value.resulting_fingerprint).map(([, value]) => value);
    panel.appendChild(el('div', { class: 'operation-grid' }, [
      field('Owner / definition', state.consumer),
      field('Pending members', Object.keys(members.pending || {}).length),
      field('Fresh / ready', `${assessed.length} / ${assessed.filter(value => value.ready).length}`),
      field('Withheld members', Object.keys(members.withheld || {}).length),
      field('Exhausted inputs', Object.keys(members.failed || {}).length),
      field('Scan continuation', members.scan_after || 'Inventory complete'),
      field('Usage', 'Unknown'),
    ]));
    const active = members.active;
    if (active) panel.appendChild(el('p', { text: `Member ${active.member.key} · attempt ${active.attempt}/${active.max_attempts} · deadline ${active.deadline} · action ${active.action_id || 'awaiting acknowledgement'}` }));
    const withheld = Object.entries(members.withheld || {}).slice(0, 20);
    if (withheld.length) panel.appendChild(el('pre', { text: withheld.map(([key, reason]) => `${key}: ${reason}`).join('\n') }));
    panel.appendChild(el('p', { text: 'Readiness evidence does not authorize promotion or execution.' }));
  } else {
  panel.appendChild(el('div', { class: 'operation-grid' }, [
    field('Owner / definition', state.consumer),
    field('Baseline exclusion', revision(state.baseline)),
    field('Observed through', revision(state.observed)),
    field('Examined through', revision(state.covered)),
    field('Pending landings / commits', `${state.pending.length} / ${state.pending_commits.length}`),
    field('Waived landings (uncovered)', state.waived?.length || 0),
    field('Unresolved evidence', Object.keys(state.unresolved).length),
    field('Usage', 'Unknown'),
  ]));
  }
  const attempt = state.active;
  if (attempt) {
    panel.appendChild(el('p', { text: `Batch ${attempt.batch.id} · attempt ${attempt.attempt}/${attempt.batch.max_attempts} · ${attempt.state} · action ${attempt.action_id || 'awaiting acknowledgement'}` }));
    if (attempt.reason) panel.appendChild(el('p', { text: attempt.reason }));
    panel.appendChild(el('pre', { text: JSON.stringify(attempt.batch, null, 2) }));
  }
  const gaps = Object.entries(state.unresolved);
  if (gaps.length) panel.appendChild(el('pre', { text: gaps.map(([commit, reason]) => `${commit}: ${reason}`).join('\n') }));
  for (const waiver of diagnostic.waivers || []) panel.appendChild(el('p', { text: `Waived ${waiver.batch_id} by ${waiver.by}: ${waiver.reason}. Coverage did not advance.` }));
  for (const receipt of diagnostic.receipts || []) {
    const row = el('p', { text: `Accepted ${receipt.accepted_at} · ${receipt.evidence_digest} · ${receipt.submitted_by} ` });
    const parts = state.consumer.split('/');
    const kind = parts[parts.length - 2];
    const name = parts[parts.length - 1];
    const link = el('a', { text: 'Accepted evidence' });
    link.href = withWorkspace(`/api/automation/${encodeURIComponent(kind)}/${encodeURIComponent(name)}/coverage/${encodeURIComponent(receipt.batch_id)}/evidence`);
    row.appendChild(link);
    panel.appendChild(row);
  }
  return panel;
}
