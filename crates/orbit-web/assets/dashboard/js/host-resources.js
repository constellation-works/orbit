// This panel always observes the HTTP serving host, independently of workspace scope.
import { el } from './common.js';

export function renderHostResources(payload, host = document.getElementById('host-resources')) {
  if (!host) return;
  const tile = (label, reading) => {
    const known = Number.isFinite(reading?.percent) && !payload?.stale;
    const severity = known ? reading.severity : 'unknown';
    return el('div', { class: `host-resource-tile ${severity}` }, [
      el('span', { class: 'host-resource-label', text: label }),
      el('strong', { text: known ? `${reading.percent.toFixed(1)}%` : 'Unknown' }),
      el('span', { class: 'host-resource-severity', text: severity }),
      ...(!known ? [el('span', { class: 'host-resource-note', text: payload?.stale ? 'stale' : reading?.unknown_reason || 'unavailable' })] : []),
    ]);
  };
  const age = Number.isFinite(payload?.sample_age_seconds) ? `${Math.floor(payload.sample_age_seconds)}s ago` : 'age unknown';
  const status = payload?.verdict_unknown ? 'Throttle verdict: unknown' : payload?.throttle ? 'Throttle verdict: held' : payload?.thresholds?.enabled === false ? 'Throttle verdict: disabled' : 'Throttle verdict: open';
  host.replaceChildren(
    el('div', { class: 'host-resource-heading' }, [
      el('strong', { text: 'Serving host resources' }),
      el('span', { text: payload ? `${payload.severity} · sampled ${age}` : 'unknown · sample unavailable' }),
    ]),
    el('div', { class: 'host-resource-grid' }, [
      tile('CPU', payload?.cpu), tile('Memory', payload?.memory),
      ...(payload?.disks?.length ? payload.disks.map(disk => tile(`Disk · ${disk.path}`, disk)) : [tile('Disk', null)]),
    ]),
    el('div', { class: 'host-resource-verdict', text: payload ? `${status} · ${payload.reason}` : 'Throttle verdict unknown · resource API unavailable' }),
  );
}

let sequence = 0;
let lastPayload = null;
let receivedAt = 0;
export async function fetchAndRenderHostResources() {
  const current = ++sequence;
  try {
    // Deliberately bypass workspace/window URL augmentation.
    const response = await fetch('/api/host/resources');
    if (!response.ok) throw new Error(`Host resource API: HTTP ${response.status}`);
    const payload = await response.json();
    if (current === sequence) {
      lastPayload = payload;
      receivedAt = Date.now();
      renderHostResources(payload);
    }
  } catch (error) {
    if (current === sequence) { lastPayload = null; renderHostResources(null); }
    throw error;
  }
}


export function initHostResources() {
  let pending = false;
  setInterval(async () => {
    if (document.hidden || pending) return;
    pending = true;
    try { await fetchAndRenderHostResources(); }
    catch (error) { console.error(error); }
    finally { pending = false; }
  }, 5000);
  setInterval(() => {
    if (!lastPayload || document.hidden) return;
    const age = lastPayload.sample_age_seconds + (Date.now() - receivedAt) / 1000;
    const expired = age > lastPayload.max_age_seconds;
    renderHostResources({ ...lastPayload, sample_age_seconds: age, stale: lastPayload.stale || expired,
      verdict_unknown: expired, throttle: expired ? false : lastPayload.throttle,
      reason: expired ? 'Resource sample expired; awaiting a fresh verdict' : lastPayload.reason });
  }, 1000);
}
