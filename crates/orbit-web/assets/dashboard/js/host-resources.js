// These live chips always observe the HTTP serving host, independently of workspace scope.
import { el } from './common.js';

export function renderHostResources(payload, host = document.getElementById('host-resource-chips')) {
  if (!host) return;
  const age = Number.isFinite(payload?.sample_age_seconds) ? `${Math.floor(payload.sample_age_seconds)}s ago` : 'age unknown';
  const verdictUnknown = !payload || payload.stale || payload.verdict_unknown;
  const status = verdictUnknown ? 'unknown' : payload.throttle ? 'held' : payload.thresholds?.enabled === false ? 'disabled' : 'open';
  const reason = payload?.reason || 'Resource API unavailable';
  const chip = (resource, reading) => {
    const known = Number.isFinite(reading?.percent) && !payload?.stale;
    const severity = known ? reading.severity : 'unknown';
    const held = !verdictUnknown && payload.throttle && payload.pressures?.some(pressure =>
      resource === 'disk' ? pressure.resource.startsWith('disk ') : pressure.resource === resource);
    const path = resource === 'disk' && reading?.path ? ` · ${reading.path}` : '';
    const note = known ? severity : payload?.stale ? 'stale' : reading?.unknown_reason || 'unavailable';
    const node = el('span', {
      class: `kpi host-resource ${severity}${held ? ' throttled' : ''}`,
      title: `${resource}${path} · ${note} · sampled ${age} · Throttle verdict: ${status} · ${reason}`,
    }, [
      el('span', { class: 'v', text: known ? `${reading.percent.toFixed(1)}%` : '-' }),
      el('span', { class: 'k', text: resource }),
      ...(held ? [el('span', { class: 'host-resource-held', text: 'throttled' })] : []),
    ]);
    node.tabIndex = 0;
    return node;
  };
  host.replaceChildren(chip('cpu', payload?.cpu), chip('memory', payload?.memory), chip('disk', payload?.disk));
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
