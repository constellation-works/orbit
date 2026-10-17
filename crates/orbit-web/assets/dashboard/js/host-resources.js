// These live chips observe the selected host (the serving host unless the host
// picker names another), independently of workspace scope.
import { el, getHost, isHostUnavailable } from './common.js';

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
    resource === 'disk' ? pressure.resource.startsWith('disk ') : pressure.resource === (resource === 'io' ? 'io full' : resource));
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
/// Settings › Hosts uses the same reading, so a row and the top bar cannot
/// disagree about severity or the load unit.
export function describeResource(payload, resource) {
  const { reading, known, severity, held, note } = hostReading(payload, resource);
  const fill = known ? Math.max(0, Math.min(100, reading.percent)) : 0;
  const path = resource === 'disk' && reading?.path ? ` ${reading.path}` : '';
  const label = resource === 'cpu' ? 'load' : resource === 'memory' ? 'mem' : 'disk';
  const value = !known ? '-' : resource === 'cpu' ? cpuLoadMultiple(reading.percent) : `${reading.percent.toFixed(0)}%`;
  const detail = !known ? `${resource}${path} ${note}`
    : resource === 'cpu' ? `${CPU_MEASURE} Now ${reading.percent.toFixed(1)}% of cores (${note}).`
    : `${resource}${path} ${reading.percent.toFixed(1)}% (${note})`;
  return { label, value, suffix: known && resource === 'cpu' ? ' cores' : '', detail, severity, held, fill };
}

/// Three chips for the serving host: load, mem and disk, each with its own
/// severity, held state, title and a bar of how full it is (load past every
/// core fills it). Throttling is a state of the chip that
/// holds admission (class, dot, outline, accessible text), never visible text,
/// and each chip is sized for its widest usual reading, so neither a verdict
/// flip nor a new reading moves the top bar.
export function renderHostResources(payload, host = document.getElementById('host-resource-chips')) {
  if (!host) return;
  const { age, status, reason } = hostVerdict(payload);
  host.replaceChildren(...RESOURCES.map(resource => {
    const { label, value, suffix, detail, severity, held, fill } = describeResource(payload, resource);
    const title = `${detail} · sampled ${age} · Throttle verdict: ${status}${held ? ' on this resource' : ''} · ${reason}`;
    const node = el('span', { class: `host-resource ${severity}${held ? ' throttled' : ''}`, title, role: 'group', 'aria-label': title }, [
      el('span', { class: 'k', text: label }),
      el('span', { class: 'v', text: value }, suffix ? [el('span', { class: 'unit', text: suffix })] : []),
      el('span', { class: 'host-resource-bar', 'aria-hidden': 'true' }, [meterFill(fill)]),
      ...(held ? [el('span', { class: 'host-resource-held', text: 'throttled' })] : []),
    ]);
    node.dataset.resource = resource;
    return node;
  }));
}

function meterFill(percent) {
  const fill = el('span', { class: 'host-resource-fill' });
  fill.setAttribute('style', `width: ${percent}%`);
  return fill;
}

// Provider usage chips: the selected host's readings, one per provider and
// window, in the resource chips' visual language. They ride the host resource
// poll (`provider_limits` in its payload), so they follow the selected host and
// pause with it in a hidden tab.

// Beyond this many windows the rest fold into one `+N` chip, so the bar cannot
// grow without bound; the summary chip's title still lists every reading.
const MAX_CHIPS = 3;
// A reading within this many points of its limit reads as near it.
const NEAR_POINTS = 10;

const WINDOW_LABELS = { five_hour: '5h', seven_day: '7d', primary: '' };

function windowLabel(window) {
  if (window == null || window === '') return '';
  return window in WINDOW_LABELS ? WINDOW_LABELS[window] : String(window).replace(/_/g, ' ');
}

/// A reading observed on an earlier UTC date is stale. The payload's `stale`
/// wins; an older payload with only `observed_at` uses that UTC date, so a
/// same-day reading still reads as today. `label` is `today` or `Nm|h|d old`.
export function providerReadingAge(reading, now = Date.now()) {
  const observed = reading?.observed_at ? new Date(reading.observed_at) : null;
  const valid = observed && !Number.isNaN(observed.getTime());
  let stale = reading?.stale === true;
  if (reading?.stale !== true && reading?.stale !== false && valid) {
    stale = observed.toISOString().slice(0, 10) < new Date(now).toISOString().slice(0, 10);
  }
  if (!stale) return { label: 'today', stale: false };
  const seconds = Number.isFinite(Number(reading?.age_seconds))
    ? Math.max(0, Math.floor(Number(reading.age_seconds)))
    : valid ? Math.max(0, Math.floor((now - observed.getTime()) / 1000)) : 0;
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(seconds / 3600);
  const amount = minutes < 60 ? `${seconds > 0 && minutes === 0 ? 1 : minutes}m`
    : hours < 48 ? `${hours}h`
    : `${Math.floor(hours / 24)}d`;
  return { label: `${amount} old`, stale: true };
}

function shortTime(iso) {
  const at = iso ? new Date(iso) : null;
  return at && !Number.isNaN(at.getTime()) ? `${at.toISOString().slice(0, 16).replace('T', ' ')}Z` : 'an unknown time';
}

/// How one reading reads in its chip and title. `severity` is `critical` at or
/// over the limit (or exhausted or gated), `elevated` within ten points of it,
/// `ok` below that and `unknown` when the provider reported no usage. A
/// provider whose gate is off has no limit to compare to, so 100% is its
/// limit; it stays marked `gatingOff` because nothing is held back on it.
export function describeProviderReading(reading, now = Date.now()) {
  const scope = reading.scope ? ` [${reading.scope}]` : '';
  const age = providerReadingAge(reading, now);
  const gatingOff = reading.threshold === 'off';
  const limit = typeof reading.threshold === 'number' ? reading.threshold : 100;
  const used = Number.isFinite(reading.used_percent) ? reading.used_percent : null;
  const exhausted = reading.exhausted === true;
  const gated = reading.gated === true;
  const severity = gated || exhausted || (used != null && used >= limit) ? 'critical'
    : used == null ? 'unknown'
    : used >= limit - NEAR_POINTS ? 'elevated'
    : 'ok';
  const short = windowLabel(reading.window);
  const key = `${reading.provider}${reading.scope ? ` ${reading.scope}` : ''}${short ? ` ${short}` : ''}`;
  const value = used != null ? `${Math.round(used)}%` : exhausted ? 'full' : '-';
  const crews = Array.isArray(reading.crews) && reading.crews.length ? `: ${reading.crews.join(', ')} skipped` : '';
  const state = gatingOff ? 'gating off'
    : gated ? `gated${used != null ? ` at ${used}% ≥ ${limit}%` : ''} until ${shortTime(reading.until)}${crews}`
    : reading.gating === false ? 'overage window; never gates'
    : `limit ${limit}%`;
  const detail = `${reading.provider}${scope} ${reading.window || 'usage window'} ${used != null ? `${used}%` : 'usage not reported'}`
    + `${reading.partial === true ? ' (partial: some invocations report no cost)' : ''}`
    + ` · observed ${reading.observed_at || 'at an unknown time'} (${age.label}${age.stale ? ' · stale' : ''}) · ${state}`;
  const flag = age.stale ? 'old' : gatingOff ? 'off' : '';
  return { key, value, severity, stale: age.stale, gatingOff, flag, detail };
}

const RANK = { critical: 3, elevated: 2, ok: 1, unknown: 0 };

function chip(className, title, children) {
  return el('span', { class: className, title, role: 'group', 'aria-label': title }, children);
}

const states = item => `${item.stale ? ' stale' : ''}${item.gatingOff ? ' gating-off' : ''}`;
const flagNodes = item => item.flag ? [el('span', { class: 'provider-usage-flag', text: item.flag })] : [];

/// The chips for `payload.provider_limits`: one per provider and window, worst
/// first, plus a summary chip (the worst reading, every reading in its title)
/// that CSS shows in place of the list on narrow bars. A stale or gating-off
/// reading keeps its chip and says so in a flag, a class and the title. A
/// payload without readings (an older host, a failed poll) keeps one
/// placeholder chip, so the bar never loses the group. Chips have a minimum
/// width, so a new reading does not move the rest of the bar.
export function renderProviderUsage(payload, host = document.getElementById('provider-usage-chips')) {
  if (!host) return;
  const readings = Array.isArray(payload?.provider_limits)
    ? payload.provider_limits.filter(reading => reading && reading.provider) : null;
  if (!readings || readings.length === 0) {
    const title = !payload ? 'Provider usage unavailable: the host resource API did not answer'
      : readings ? 'No provider usage readings on this host'
      : 'Provider usage is not reported by this host';
    host.replaceChildren(chip('host-resource provider-usage unknown', title, [
      el('span', { class: 'k', text: 'usage' }),
      el('span', { class: 'v', text: '-' }),
    ]));
    return;
  }
  const now = Date.now();
  const described = readings.map(reading => describeProviderReading(reading, now))
    .sort((a, b) => RANK[b.severity] - RANK[a.severity]);
  const all = described.map(item => item.detail).join('\n');
  const chips = described.slice(0, MAX_CHIPS).map(item =>
    chip(`host-resource provider-usage provider-usage-item ${item.severity}${states(item)}`, item.detail, [
      el('span', { class: 'k', text: item.key }),
      el('span', { class: 'v', text: item.value }),
      ...flagNodes(item),
    ]));
  if (described.length > MAX_CHIPS) {
    chips.push(chip('host-resource provider-usage provider-usage-item provider-usage-more unknown', all, [
      el('span', { class: 'k', text: `+${described.length - MAX_CHIPS} more` }),
    ]));
  }
  const worst = described[0];
  const summary = chip(`host-resource provider-usage provider-usage-summary ${worst.severity}${states(worst)}`, all, [
    el('span', { class: 'k', text: 'usage' }),
    el('span', { class: 'v', text: worst.value }),
  ]);
  host.replaceChildren(summary, ...chips);
}

const listeners = new Set();
/// Called with each fresh payload (or null when the poll failed).
export function onHostResources(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/// One host's resource snapshot, with no workspace or window query. Bounded
/// like `fetchJson`: a stalled snapshot (hung mount, half-open connection)
/// rejects instead of pending for the rest of the page's life. A null name is
/// the serving host; any other name goes through `/api/on/<host>/`, which
/// answers the serving host's own name locally.
export async function fetchHostResourcePayloadFor(hostName) {
  const path = hostName
    ? `/api/on/${encodeURIComponent(hostName)}/host/resources`
    : '/api/host/resources';
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30000);
  try {
    const response = await fetch(path, { signal: controller.signal });
    if (!response.ok) {
      let detail = `HTTP ${response.status}`;
      try {
        const body = await response.json();
        if (body && typeof body.error === 'string' && body.error) detail = body.error;
      } catch (_) { /* a non-JSON failure keeps the status */ }
      throw new Error(`Host resource API: ${detail}`);
    }
    return await response.json();
  } catch (error) {
    if (controller.signal.aborted) throw new Error('Request timed out after 30 seconds');
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

/// The selected host. Same path `withHost` would build.
export async function fetchHostResourcePayload() {
  return fetchHostResourcePayloadFor(getHost());
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
      renderProviderUsage(payload);
      for (const listener of listeners) listener(payload);
    }
  } catch (error) {
    if (current === sequence) {
      lastPayload = null;
      renderHostResources(null);
      renderProviderUsage(null);
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
  renderProviderUsage(null);
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
