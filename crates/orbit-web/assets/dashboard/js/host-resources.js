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

export function renderHostResources(payload, host = document.getElementById('host-resource-chips')) {
  if (!host) return;
  const { age, status, reason } = hostVerdict(payload);
  const chip = resource => {
    const { reading, known, severity, held, note } = hostReading(payload, resource);
    const path = resource === 'disk' && reading?.path ? ` · ${reading.path}` : '';
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
  host.replaceChildren(chip('cpu'), chip('memory'), chip('disk'));
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
