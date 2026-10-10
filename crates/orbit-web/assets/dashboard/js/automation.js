// Persisted delivery observations and accepted coverage, shared by both consumers.
import { captureWorkspaceVisit, detailsPanel, el, fetchJson, formatDateTime, onWorkspaceChange, withWorkspace } from './common.js';

const field = (label, value) => el('div', { class: 'operation-field' }, [
  el('span', { class: 'operation-field-label', text: label }),
  el('span', { class: 'operation-field-value', text: String(value ?? 'Not observed') }),
]);
const revision = value => value ? `${value.commit} (tree ${value.tree})` : 'Not observed';

// Why this host cannot admit work for a consumer whose owner it is not, and
// what the operator can do about it.
const ownershipBlocker = ownership => {
  if (!ownership || ownership.owned_here) return null;
  switch (ownership.authority) {
    case 'definition':
    case 'workspace':
      return `Owned by machine ${ownership.owner_machine}. This host reconciles admitted work only; run it where that machine is registered, or set owner_machine to this host.`;
    case 'conflicting':
      return 'Ownership is contradictory: the workspace record and this replica checkout name different owner machines. Repair the workspace registration, or set owner_machine explicitly.';
    default:
      return 'No owner machine is registered for this workspace. Register the workspace owner, or set owner_machine on the definition.';
  }
};

// `key` identifies this diagnostic's disclosure across refreshes; the caller
// owns it because the same consumer can be shown under different cards.
export function renderAutomation(diagnostic, key, consumer) {
  if (!diagnostic) return null;
  const panel = detailsPanel(key, { class: 'automation-diagnostic' });
  panel.appendChild(el('summary', { text: `${diagnostic.state?.members ? 'State automation' : 'Delivery coverage'} · ${diagnostic.reason || 'unknown'}` }));
  if (diagnostic.error) panel.appendChild(el('p', { text: diagnostic.error }));
  const blocker = ownershipBlocker(diagnostic.ownership);
  if (blocker) panel.appendChild(el('p', { text: blocker }));
  const state = diagnostic.state;
  if (!state) {
    panel.appendChild(el('p', { text: 'No baseline recorded. Preview before enabling this consumer.' }));
    return panel;
  }
  const members = state.members;
  if (members) {
    const counts = members.counts;
    panel.appendChild(el('div', { class: 'operation-grid' }, [
      field('Owner / definition', state.consumer),
      field('Pending members', counts.pending),
      field('Fresh / ready', `${counts.fresh} / ${counts.ready}`),
      field('Withheld members', counts.withheld),
      field('Exhausted inputs', counts.failed),
      field('Scan continuation', members.scan_after || 'Inventory complete'),
    ]));
    const active = members.active;
    if (active) {
      const batch = (active.members?.length ? active.members : [active.member]).map(member => member.key).join(', ');
      panel.appendChild(el('p', { text: `Batch ${batch} · attempt ${active.attempt}/${active.max_attempts} · deadline ${formatDateTime(active.deadline)} · action ${active.action_id || 'awaiting acknowledgement'}` }));
    }
    const withheld = Object.entries(members.withheld || {}).slice(0, 20);
    if (withheld.length) panel.appendChild(el('pre', { text: withheld.map(([key, reason]) => `${key}: ${reason}`).join('\n') }));
    panel.appendChild(el('p', { text: 'Readiness evidence does not authorize promotion or execution.' }));
  } else {
  panel.appendChild(el('div', { class: 'operation-grid' }, [
    field('Owner / definition', state.consumer),
    field('Baseline exclusion', revision(state.baseline)),
    field('Observed through', revision(state.observed)),
    field('Examined through', revision(state.covered)),
    field('Pending landings / commits', `${state.counts.pending} / ${state.counts.pending_commits}`),
    field('Waived landings (uncovered)', state.counts.waived),
    field('Excluded landings (before-PR coverage)', state.counts.excluded),
    field('Unresolved evidence', state.counts.unresolved),
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
  for (const excluded of state.excluded || []) panel.appendChild(el('p', { text: `Excluded ${excluded.delivery.key}: certificate ${excluded.exclusion.attempt_id} (${excluded.exclusion.assurance}); examined as context, not an obligation.` }));
  for (const waiver of diagnostic.waivers || []) panel.appendChild(el('p', { text: `Waived ${waiver.batch_id} by ${waiver.by}: ${waiver.reason}. Coverage did not advance.` }));
  for (const receipt of diagnostic.receipts || []) {
    const row = el('p', { text: `Accepted ${formatDateTime(receipt.accepted_at)} · ${receipt.evidence_digest} · ${receipt.submitted_by} ` });
    const parts = state.consumer.split('/');
    const kind = parts[parts.length - 2];
    const name = parts[parts.length - 1];
    const link = el('a', { text: 'Accepted evidence' });
    link.href = withWorkspace(`/api/automation/${encodeURIComponent(kind)}/${encodeURIComponent(name)}/coverage/${encodeURIComponent(receipt.batch_id)}/evidence`);
    row.appendChild(link);
    panel.appendChild(row);
  }
  if (consumer?.workspace) panel.appendChild(fullStateDisclosure(key, consumer));
  return panel;
}

// Full state is loaded only on explicit disclosure, cached across polling,
// and reloadable by the operator. A workspace switch drops retained data.
const fullStates = new Map();
onWorkspaceChange(() => fullStates.clear());

function fullStateDisclosure(key, { kind, name, workspace }) {
  const path = `/api/automation/${encodeURIComponent(kind)}/${encodeURIComponent(name)}/state?workspace=${encodeURIComponent(workspace)}`;
  const full = detailsPanel(`${key}:full:${workspace}`);
  const output = el('pre');
  full.appendChild(el('summary', { text: 'Full persisted state' }));
  let loading = false;
  const load = async (reload = false) => {
    if (loading) return;
    loading = true;
    const visit = captureWorkspaceVisit();
    output.textContent = 'Loading…';
    if (reload) fullStates.delete(path);
    if (!fullStates.has(path)) fullStates.set(path, fetchJson(path));
    const request = fullStates.get(path);
    try {
      const payload = await request;
      if (visit.isCurrent()) output.textContent = JSON.stringify(payload.state, null, 2);
    } catch (error) {
      if (fullStates.get(path) === request) fullStates.delete(path);
      if (visit.isCurrent()) output.textContent = error.message;
    } finally {
      loading = false;
    }
  };
  const reload = el('button', { type: 'button', text: 'Reload full state' });
  reload.addEventListener('click', () => void load(true));
  full.append(reload, output);
  full.addEventListener('toggle', () => { if (full.open) void load(); });
  return full;
}
