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
  return `${draining ? `Draining · ${draining} of ${known.length}` : winding ? `Winding down · ${winding} of ${known.length}` : `Fleet idle · ${known.length} workspaces`}${unavailable ? ' · partial' : ''}`;
}

export function renderFleetDrains(payload, selectScope) {
  const table = el('table', { class: 'fleet-drains' });
  table.append(el('caption', { text: 'Fleet drains · select a workspace to inspect or control its drain' }));
  const headings = ['Host', 'Workspace', 'Phase', 'Slots', 'Eligible', 'Top wait', 'Holds'];
  table.append(el('thead', {}, [el('tr', {}, headings.map(text => {
    const header = el('th', { text });
    header.setAttribute('scope', 'col');
    return header;
  }))]));
  const body = el('tbody');
  for (const row of payload.items || []) {
    const tr = el('tr', { class: row.error ? 'fleet-unavailable' : '' });
    const values = [row.host, row.workspace_name || 'Unavailable host', row.phase.replaceAll('_', ' '),
      row.slots_used == null ? '—' : `${row.slots_used} / ${row.slots_ceiling}`,
      row.eligible_count == null ? '—' : `${row.eligible_count}${row.counts_truncated ? '+' : ''}`,
      row.top_wait_reason?.replaceAll('_', ' ') || '—', row.error || (row.holds || []).join('; ') || '—'];
    values.forEach((text, index) => {
      const cell = el('td', { text });
      cell.setAttribute('data-label', headings[index]);
      if (index === 1 && row.workspace_id && !row.error) {
        cell.textContent = '';
        const button = el('button', { class: 'fleet-workspace', text, type: 'button' });
        button.setAttribute('aria-label', `Show ${text} on ${row.host}`);
        cell.append(button);
      }
      tr.append(cell);
    });
    if (row.workspace_id && !row.error) {
      tr.classList.add('fleet-selectable');
      tr.addEventListener('click', () => selectScope(row.host_selector, row.workspace_id));
    }
    body.append(tr);
  }
  table.append(body);
  if (!body.children.length) {
    const empty = el('td', { text: 'No registered workspaces' });
    empty.setAttribute('colspan', '7');
    table.append(el('tfoot', {}, [el('tr', {}, [empty])]));
  }
  return table;
}
