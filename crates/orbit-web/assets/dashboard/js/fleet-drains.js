// Fleet reads use each host's local aggregate through the existing one-hop
// forward. At most three requests run at once; a failed host remains a row.
// The panel poll is 30s (polling.js). A snapshot stays fresh across that
// interval, so the next poll does not ask again. The indicator's own timer
// refreshes it on this longer cadence, outside the panel poll.
import { el, fetchJson, getRegisteredHosts, getHost } from './common.js';

export const FLEET_CACHE_MS = 60_000;

let cached = null;
let pending = null;
let cachedAt = 0;
let cachedKey = '';

function fleetSources() {
  const hosts = getRegisteredHosts();
  return hosts.length ? hosts : [{ name: getHost() || 'This host', selector: getHost(), fallback: true }];
}

function fleetKey(sources) {
  return JSON.stringify(sources.map(host => [host.name, host.machine_id, host.ssh, host.fallback]));
}

// The snapshot the panel poll may paint. `fresh` is false once it is older
// than one fleet cadence; callers that already have a snapshot do not refetch.
export function peekFleetDrains() {
  const key = fleetKey(fleetSources());
  if (key !== cachedKey || !cached) return null;
  return { payload: cached, fresh: Date.now() - cachedAt < FLEET_CACHE_MS };
}

export function fetchFleetDrains({ force = false } = {}) {
  const sources = fleetSources();
  const key = fleetKey(sources);
  if (!force && key === cachedKey && cached && Date.now() - cachedAt < FLEET_CACHE_MS) return Promise.resolve(cached);
  if (key === cachedKey && pending) return pending;
  if (key !== cachedKey) cached = null;
  cachedKey = key;
  const rows = new Array(sources.length);
  let next = 0;
  const read = async () => {
    while (next < sources.length) {
      const index = next++;
      const host = sources[index];
      try {
        const path = host.fallback ? '/api/fleet/drains' : `/api/on/${encodeURIComponent(host.name)}/fleet/drains`;
        const payload = await fetchJson(path);
        if (!Array.isArray(payload?.items)) throw new Error('Fleet drains are unavailable on this host');
        rows[index] = payload.items.map(row => ({ ...row, host: host.name, host_selector: host.fallback ? host.selector : host.name }));
      } catch (error) {
        rows[index] = [{ host: host.name, phase: 'unavailable', error: error.message || 'Host unavailable' }];
      }
    }
  };
  const request = Promise.all(Array.from({ length: Math.min(3, sources.length) }, read)).then(() => ({ items: rows.flat() }));
  pending = request;
  return request.then(payload => {
    if (cachedKey === key && pending === request) {
      cached = payload;
      cachedAt = Date.now();
      pending = null;
    }
    return payload;
  });
}

export function fleetDrainLabel(payload) {
  const rows = payload.items || [];
  const known = rows.filter(row => row.workspace_id);
  const draining = known.filter(row => ['draining', 'pulling'].includes(row.phase)).length;
  const winding = known.filter(row => row.phase === 'winding_down').length;
  const unavailable = rows.some(row => row.error);
  if (unavailable && known.every(row => row.error)) return 'Fleet unavailable';
  return `${draining ? `${draining} of ${known.length} workspaces draining` : winding ? `${winding} of ${known.length} workspaces winding down` : `Fleet idle · ${known.length} workspaces`}${unavailable ? ' · partial' : ''}`;
}

const LIVE_PHASES = ['draining', 'pulling', 'winding_down'];

// A visible value preceded by its field name for assistive tech only.
function field(name, label, value, cls = '') {
  const node = el('span', { class: `fleet-field ${cls}`.trim() }, [el('span', { class: 'visually-hidden', text: `${label} ` }), value]);
  node.setAttribute('data-field', name);
  return node;
}

function slotsField(row) {
  const used = row.slots_used;
  const ceiling = row.slots_ceiling;
  const fill = ceiling > 0 ? Math.min(100, Math.round((used / ceiling) * 100)) : used > 0 ? 100 : 0;
  const busy = used > 0 ? (used >= ceiling ? ' fleet-slots-busy fleet-slots-full' : ' fleet-slots-busy') : '';
  const node = el('span', { class: `fleet-field fleet-slots${busy}`, title: `${used} of ${ceiling} slots in use` }, [
    el('span', { class: 'visually-hidden', text: 'Slots ' }),
    el('span', { class: 'fleet-meter', 'aria-hidden': 'true' }, [el('span', { style: { width: `${fill}%` } })]),
    `${used}/${ceiling}`,
  ]);
  node.setAttribute('data-field', 'slots');
  return node;
}

// The second line names only what is present: idle rows with nothing waiting
// stay one line.
function detailLine(row) {
  const parts = [];
  if (row.eligible_count) parts.push(field('eligible', 'Eligible', `${row.eligible_count}${row.counts_truncated ? '+' : ''} eligible`, 'fleet-eligible'));
  if (row.top_wait_reason) parts.push(field('top-wait', 'Top wait', `top wait: ${row.top_wait_reason.replaceAll('_', ' ')}`));
  if (row.holds?.length) parts.push(field('holds', 'Holds', `held: ${row.holds.join('; ')}`, 'fleet-holds'));
  const queued = Array.isArray(row.queued_drains) ? row.queued_drains.filter(drain => drain?.run_id) : [];
  if (queued.length) {
    const node = field('queued', 'Queued', `${queued.length} queued window${queued.length === 1 ? '' : 's'}`, 'fleet-queued');
    node.title = queued.map(queuedSummary).join('\n');
    parts.push(node);
    const uncredentialed = [...new Set(queued.flatMap(drain => drain.uncredentialed_crews || []))];
    if (uncredentialed.length) parts.push(field('queued-credential', 'Queued window', `queued without Claude credential: ${uncredentialed.join(', ')}`, 'fleet-holds'));
  }
  return parts.length ? el('div', { class: 'fleet-detail' }, parts) : null;
}

// One queued window in a line: its run, ceiling, completion, crews and length.
function queuedSummary(drain) {
  const crews = drain.allowed_crews?.length ? drain.allowed_crews.join(', ') : 'all crews';
  const seconds = Number(drain.for_seconds) || 0;
  const window = seconds > 0 ? `${Math.floor(seconds / 3600)}h ${String(Math.floor((seconds % 3600) / 60)).padStart(2, '0')}m window` : 'single pass';
  return `${drain.run_id} · ${drain.max_active_leaf_runs} parallel · ${drain.completion || 'review'} · ${crews} · ${window}`;
}

function workspaceRow(row, selectScope) {
  const phase = row.phase || 'idle';
  const live = LIVE_PHASES.includes(phase);
  const item = el('li', { class: `fleet-row fleet-selectable${live ? ' fleet-live' : ''}` });
  item.setAttribute('data-phase', phase);
  const name = row.workspace_name || row.workspace_id;
  const button = el('button', { class: 'fleet-workspace', text: name, type: 'button', title: name });
  button.setAttribute('aria-label', `Show ${name} on ${row.host}`);
  const phaseLabel = field('phase', 'Phase', phase.replaceAll('_', ' '), live ? 'fleet-phase' : 'fleet-phase visually-hidden');
  item.append(el('div', { class: 'fleet-line' }, [
    el('span', { class: `drain-dot ${phase}`, 'aria-hidden': 'true' }), button, phaseLabel,
    row.slots_used == null ? null : slotsField(row),
  ]));
  const detail = detailLine(row);
  if (detail) item.append(detail);
  item.addEventListener('click', () => selectScope(row.host_selector, row.workspace_id));
  return item;
}

// The host heading says the host is unavailable; the row carries why.
function unavailableRow(row) {
  const item = el('li', { class: 'fleet-row fleet-unavailable' });
  item.setAttribute('data-phase', row.phase || 'unavailable');
  item.append(field('error', 'Error', row.error || 'Host unavailable', 'fleet-error'));
  return item;
}

// One group per host, in registry order. Live drains lead their group; the
// rest keep the host's order (the sort is stable). Idle rows show a muted dot and slots only.
export function renderFleetDrains(payload, selectScope) {
  const root = el('div', { class: 'fleet-drains' });
  root.append(el('p', { class: 'fleet-hint', text: 'Select a workspace to inspect or control its drain' }));
  const groups = new Map();
  for (const row of payload.items || []) {
    if (!groups.has(row.host)) groups.set(row.host, []);
    groups.get(row.host).push(row);
  }
  for (const [host, rows] of groups) {
    const isLive = row => Number(LIVE_PHASES.includes(row.phase));
    const ranked = [...rows].sort((a, b) => isLive(b) - isLive(a));
    const known = rows.filter(row => row.workspace_id && !row.error);
    const live = known.filter(isLive).length;
    const count = known.length ? `${known.length} workspace${known.length === 1 ? '' : 's'}${live ? ` · ${live} live` : ''}` : 'unavailable';
    const section = el('section', { class: 'fleet-host' });
    section.append(el('h3', { class: 'fleet-host-name' }, [el('span', { text: host }), el('span', { class: known.length ? 'fleet-host-count' : 'fleet-host-count fleet-host-down', text: count })]));
    const list = el('ul', { class: 'fleet-rows' });
    for (const row of ranked) list.append(row.workspace_id && !row.error ? workspaceRow(row, selectScope) : unavailableRow(row));
    section.append(list);
    root.append(section);
  }
  if (!groups.size) root.append(el('p', { class: 'fleet-empty', text: 'No registered workspaces' }));
  return root;
}
