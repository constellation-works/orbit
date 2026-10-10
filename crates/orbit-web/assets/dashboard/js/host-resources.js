// These live chips observe the selected host (the serving host unless the host
// picker names another), independently of workspace scope.
import { el, isHostUnavailable, withHost } from './common.js';

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

const RESOURCES = ['cpu', 'memory', 'disk'];
const CPU_MEASURE = 'cpu load: the 1-minute load average divided by online cores. 100% means every core is busy; above 100% means work is queueing.';

/// CPU load as a multiple of online cores ("1.6×"), the one unit the top bar
/// and the Drain card's throttle note both state it in. `percent` is the host
/// API's cpu percent, where 100 means every core is busy. A `threshold` keeps
/// up to two decimals, so 75% reads 0.75× rather than rounding to 0.8×.
export function cpuLoadMultiple(percent, threshold = false) {
  return `${threshold ? Number((percent / 100).toFixed(2)) : (percent / 100).toFixed(1)}×`;
}

/// How one resource reads in its chip and in the title. CPU is load relative to
/// cores, so it can pass 100%; it is never labelled as a plain CPU percentage.
function describeResource(payload, resource) {
  const { reading, known, severity, held, note } = hostReading(payload, resource);
  const path = resource === 'disk' && reading?.path ? ` ${reading.path}` : '';
  const label = resource === 'cpu' ? 'load' : resource === 'memory' ? 'mem' : 'disk';
  const value = !known ? '-' : resource === 'cpu' ? cpuLoadMultiple(reading.percent) : `${reading.percent.toFixed(0)}%`;
  const detail = !known ? `${resource}${path} ${note}`
    : resource === 'cpu' ? `${CPU_MEASURE} Now ${reading.percent.toFixed(1)}% of cores (${note}).`
    : `${resource}${path} ${reading.percent.toFixed(1)}% (${note})`;
  return { label, value, suffix: known && resource === 'cpu' ? ' cores' : '', detail, severity, held };
}

/// Three chips for the serving host: load, mem and disk, each with its own
/// severity, held state and title. Throttling is a state of the chip that
/// holds admission (class, dot, outline, accessible text), never visible text,
/// and each chip is sized for its widest usual reading, so neither a verdict
/// flip nor a new reading moves the top bar.
export function renderHostResources(payload, host = document.getElementById('host-resource-chips')) {
  if (!host) return;
  const { age, status, reason } = hostVerdict(payload);
  host.replaceChildren(...RESOURCES.map(resource => {
    const { label, value, suffix, detail, severity, held } = describeResource(payload, resource);
    const title = `${detail} · sampled ${age} · Throttle verdict: ${status}${held ? ' on this resource' : ''} · ${reason}`;
    const node = el('span', { class: `host-resource ${severity}${held ? ' throttled' : ''}`, title, role: 'group', 'aria-label': title }, [
      el('span', { class: 'k', text: label }),
      el('span', { class: 'v', text: value }, suffix ? [el('span', { class: 'unit', text: suffix })] : []),
      ...(held ? [el('span', { class: 'host-resource-held', text: 'throttled' })] : []),
    ]);
    node.dataset.resource = resource;
    return node;
  }));
}

const listeners = new Set();
/// Called with each fresh payload (or null when the poll failed).
export function onHostResources(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/// The selected host, with no workspace or window query. Bounded like
/// `fetchJson`: a stalled snapshot (hung mount, half-open connection) rejects
/// instead of pending for the rest of the page's life.
export async function fetchHostResourcePayload() {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30000);
  try {
    const response = await fetch(withHost('/api/host/resources'), { signal: controller.signal });
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

/// Forget the last reading when the selected host changes, so one host's
/// readings never show under another's name; a reading still in flight for
/// the previous host is dropped with it.
export function resetHostResources() {
  sequence += 1;
  lastPayload = null;
  renderHostResources(null);
  for (const listener of listeners) listener(null);
}

export function initHostResources() {
  let pending = false;
  setInterval(async () => {
    if (document.hidden || pending || isHostUnavailable()) return;
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
