// These live chips always observe the HTTP serving host, independently of workspace scope.
import { el } from './common.js';

/// The throttle verdict as the topbar chips and the Settings System tab both
/// state it: `held`, `open`, `disabled`, or `unknown` (stale or no payload).
export function hostVerdict(payload) {
  const age = Number.isFinite(payload?.sample_age_seconds) ? `${Math.floor(payload.sample_age_seconds)}s ago` : 'age unknown';
  const verdictUnknown = !payload || payload.stale || payload.verdict_unknown;
  const status = verdictUnknown ? 'unknown' : payload.throttle ? 'held' : payload.thresholds?.enabled === false ? 'disabled' : 'open';
  const reason = payload?.reason || 'Resource API unavailable';
  return { age, verdictUnknown, status, reason };
}

/// One resource's live reading: `known`, its `severity`, whether the verdict
/// holds on it, and the `pressures` entries (with `since`) that hold it.
export function hostReading(payload, resource) {
  const reading = payload?.[resource];
  const { verdictUnknown } = hostVerdict(payload);
  const known = Number.isFinite(reading?.percent) && !payload?.stale;
  const severity = known ? reading.severity : 'unknown';
  const pressures = verdictUnknown || !payload.throttle ? [] : (payload.pressures || []).filter(pressure =>
    resource === 'disk' ? pressure.resource.startsWith('disk ') : pressure.resource === resource);
  const note = known ? severity : payload?.stale ? 'stale' : reading?.unknown_reason || 'unavailable';
  return { reading, known, severity, held: pressures.length > 0, pressures, note };
}

const SEVERITY_RANK = { critical: 3, elevated: 2, ok: 1, unknown: 0 };
const CPU_MEASURE = 'cpu load: the 1-minute load average divided by online cores. 100% means every core is busy; above 100% means work is queueing.';

/// How one resource reads in the chip and in the title. CPU is load relative to
/// cores, so it can pass 100%; it is never labelled as a plain CPU percentage.
function describeResource(payload, resource) {
  const { reading, known, severity, held, note } = hostReading(payload, resource);
  const path = resource === 'disk' && reading?.path ? ` ${reading.path}` : '';
  const label = resource === 'cpu' ? 'load' : resource === 'memory' ? 'mem' : 'disk';
  const value = !known ? '-' : resource === 'cpu' ? `${(reading.percent / 100).toFixed(1)}×` : `${reading.percent.toFixed(0)}%`;
  const detail = !known ? `${resource}${path} ${note}`
    : resource === 'cpu' ? `${CPU_MEASURE} Now ${reading.percent.toFixed(1)}% of cores (${note}).`
    : `${resource}${path} ${reading.percent.toFixed(1)}% (${note})`;
  return { resource, label, value, suffix: known && resource === 'cpu' ? ' cores' : '', detail, known, severity, held, percent: known ? reading.percent : -1 };
}

/// One chip for the serving host: the worst resource (held first, then by
/// severity, then by usage) with the whole breakdown in the title. Throttling
/// is a state of the chip (class, dot, accessible text), never extra text, so
/// the top bar keeps one width and one height whatever the verdict.
export function renderHostResources(payload, host = document.getElementById('host-resource-chips')) {
  if (!host) return;
  const { age, status, reason } = hostVerdict(payload);
  const readings = ['cpu', 'memory', 'disk'].map(resource => describeResource(payload, resource));
  // Held resources first, then severity, then usage.
  const score = item => (item.held ? 1e6 : 0) + (SEVERITY_RANK[item.severity] ?? 0) * 1e4 + item.percent;
  const worst = readings.reduce((best, item) => (score(item) > score(best) ? item : best));
  const held = readings.some(item => item.held);
  const shown = worst.known ? worst : { label: 'host', value: '-', suffix: '' };
  const severity = worst.known ? worst.severity : 'unknown';
  const title = [
    `Host resources (serving host) · ${readings.map(item => item.detail).join(' · ')}`,
    `sampled ${age} · Throttle verdict: ${status}${held ? ' (admission held)' : ''} · ${reason}`,
  ].join(' · ');
  const node = el('span', { class: `kpi host-resource ${severity}${held ? ' throttled' : ''}`, title }, [
    el('span', { class: 'k', text: shown.label }),
    el('span', { class: 'v', text: shown.value }, shown.suffix ? [el('span', { class: 'k-more', text: shown.suffix })] : []),
    ...(held ? [el('span', { class: 'host-resource-held', text: 'throttled' })] : []),
  ]);
  node.tabIndex = 0;
  host.replaceChildren(node);
}

const listeners = new Set();
/// Called with each fresh payload (or null when the poll failed).
export function onHostResources(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/// The serving host, with no workspace or window query. Bounded like
/// `fetchJson`: a stalled snapshot (hung mount, half-open connection) rejects
/// instead of pending for the rest of the page's life.
export async function fetchHostResourcePayload() {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30000);
  try {
    const response = await fetch('/api/host/resources', { signal: controller.signal });
    if (!response.ok) throw new Error(`Host resource API: HTTP ${response.status}`);
    return await response.json();
  } catch (error) {
    if (controller.signal.aborted) throw new Error('Request timed out after 30 seconds');
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

let sequence = 0;
let lastPayload = null;
let receivedAt = 0;
export async function fetchAndRenderHostResources() {
  const current = ++sequence;
  try {
    const payload = await fetchHostResourcePayload();
    if (current === sequence) {
      lastPayload = payload;
      receivedAt = Date.now();
      renderHostResources(payload);
      for (const listener of listeners) listener(payload);
    }
  } catch (error) {
    if (current === sequence) {
      lastPayload = null;
      renderHostResources(null);
      for (const listener of listeners) listener(null);
    }
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
