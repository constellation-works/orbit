// Fleet reads use each host's local aggregate through the existing one-hop
// forward. At most three requests run at once; a failed host remains a row.
// The panel poll is 30s (polling.js). A snapshot stays fresh across that
// interval, so the next poll does not ask again. The indicator's own timer
// refreshes it on this longer cadence, outside the panel poll.
import { fetchJson, getRegisteredHosts, getHost } from './common.js';

export const FLEET_CACHE_MS = 60_000;

let cached = null;
let pending = null;
let cachedAt = 0;
let cachedKey = '';
let partialPayload = null;
const partialListeners = new Set();

function publishPartial(rows, sources) {
  const items = [];
  for (let index = 0; index < sources.length; index++) {
    if (rows[index]) items.push(...rows[index]);
    else items.push({ host: sources[index].name, phase: 'loading', pending: true });
  }
  partialPayload = { items };
  for (const listener of [...partialListeners]) {
    try { listener(partialPayload); } catch (error) { console.error(error); }
  }
}

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

export function fetchFleetDrains({ force = false, onPartial = null } = {}) {
  const sources = fleetSources();
  const key = fleetKey(sources);
  if (typeof onPartial === 'function') partialListeners.add(onPartial);
  if (!force && key === cachedKey && cached && Date.now() - cachedAt < FLEET_CACHE_MS) {
    if (typeof onPartial === 'function') partialListeners.delete(onPartial);
    return Promise.resolve(cached);
  }
  if (key === cachedKey && pending) {
    if (partialPayload && typeof onPartial === 'function') {
      try { onPartial(partialPayload); } catch (error) { console.error(error); }
    }
    return pending;
  }
  if (key !== cachedKey) { cached = null; partialPayload = null; }
  cachedKey = key;
  const rows = new Array(sources.length);
  publishPartial(rows, sources);
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
      if (cachedKey === key) publishPartial(rows, sources);
    }
  };
  const request = Promise.all(Array.from({ length: Math.min(3, sources.length) }, read)).then(() => ({ items: rows.flat() }));
  pending = request;
  return request.then(payload => {
    if (cachedKey === key && pending === request) {
      cached = payload;
      cachedAt = Date.now();
      pending = null;
      partialPayload = null;
      partialListeners.clear();
    }
    return payload;
  });
}
