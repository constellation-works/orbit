// These live chips observe the selected host (the serving host unless the host
// picker names another), independently of workspace scope.
import { el, findRegisteredHost, getHost, getServingHost, hostDisplayName, isHostUnavailable } from './common.js';

/// The throttle verdict as the rail block and the Settings System tab both
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
const IO_MEASURE = 'io: the share of the last minute in which every runnable task was stalled on disk I/O (Linux pressure stall, full avg60).';
const LABELS = { cpu: 'load', memory: 'mem', disk: 'disk', io: 'io' };

/// The resources a host reports, in block order. Disk I/O stall exists only on
/// hosts with pressure stall information (Linux), so `io` follows the other
/// three only when the payload carries it.
export function hostResources(payload) {
  return payload?.io ? [...RESOURCES, 'io'] : RESOURCES;
}

/// CPU load as a multiple of online cores ("1.6×"), the one unit the rail
/// block and the Drain card's throttle note both state it in. `percent` is the
/// host API's cpu percent, where 100 means every core is busy. A `threshold`
/// keeps up to two decimals, so 75% reads 0.75× rather than rounding to 0.8×.
export function cpuLoadMultiple(percent, threshold = false) {
  return `${threshold ? Number((percent / 100).toFixed(2)) : (percent / 100).toFixed(1)}×`;
}

/// How one resource reads in the rail block and in its title. CPU is load
/// relative to cores, so it can pass 100%; it is never labelled as a plain CPU
/// percentage. Settings › Hosts uses the same reading, so a row and the rail
/// block cannot disagree about severity or the load unit.
export function describeResource(payload, resource) {
  const { reading, known, severity, held, note } = hostReading(payload, resource);
  const fill = known ? Math.max(0, Math.min(100, reading.percent)) : 0;
  const path = resource === 'disk' && reading?.path ? ` ${reading.path}` : '';
  const label = LABELS[resource];
  const value = !known ? '-' : resource === 'cpu' ? cpuLoadMultiple(reading.percent) : `${reading.percent.toFixed(0)}%`;
  const detail = !known ? `${resource}${path} ${note}`
    : resource === 'cpu' ? `${CPU_MEASURE} Now ${reading.percent.toFixed(1)}% of cores (${note}).`
    : resource === 'io' ? `${IO_MEASURE} Now ${reading.percent.toFixed(1)}% (${note}).`
    : `${resource}${path} ${reading.percent.toFixed(1)}% (${note})`;
  return { label, value, suffix: known && resource === 'cpu' ? ' cores' : '', detail, severity, held, fill };
}

// Whether the selected host has reported disk I/O stall. Once it has, its
// row stays through a failed poll or a sample without it (reading `-`), so
// the bar does not shift; a host switch forgets it.
let ioReported = false;

const NAMES = { cpu: 'Load', memory: 'Memory', disk: 'Disk', io: 'Disk I/O stall' };
// Server defaults, used when a payload omits a mark. They match
// workflow.resource_throttle.* so a tick is never a made-up threshold.
const MARK_DEFAULTS = {
  cpu: [85, 90], memory: [85, 90], disk: [85, 90], io: [10, 20],
};
const MARK_KEYS = {
  cpu: ['cpu_resume_percent', 'cpu_high_percent'],
  memory: ['memory_resume_percent', 'memory_high_percent'],
  disk: ['disk_resume_percent', 'disk_high_percent'],
  io: ['io_full_resume_percent', 'io_full_high_percent'],
};

let detailOpen = false;

function ageCompact(payload) {
  const seconds = Number(payload?.sample_age_seconds);
  if (!Number.isFinite(seconds)) return null;
  const whole = Math.max(0, Math.floor(seconds));
  if (whole < 60) return `${whole}s`;
  if (whole < 3600) return `${Math.floor(whole / 60)}m`;
  if (whole < 86400) return `${Math.floor(whole / 3600)}h`;
  return `${Math.floor(whole / 86400)}d`;
}

function marks(payload, resource) {
  const thresholds = payload?.thresholds || {};
  const [resumeKey, highKey] = MARK_KEYS[resource];
  const [resumeDefault, highDefault] = MARK_DEFAULTS[resource];
  const resume = Number.isFinite(Number(thresholds[resumeKey])) ? Number(thresholds[resumeKey]) : resumeDefault;
  const high = Number.isFinite(Number(thresholds[highKey])) ? Number(thresholds[highKey]) : highDefault;
  return { resume, high };
}

function alarmRank(row) {
  if (row.held || row.severity === 'critical') return 3;
  if (row.severity === 'elevated') return 2;
  return 0;
}

/// The elevated-or-worse reading the verdict and the warning chip name.
/// A held reading outranks a critical one that is not holding, then the
/// higher percent, so one chip names a single worst resource.
function worstAlarm(rows) {
  let best = null;
  for (const row of rows) {
    const rank = alarmRank(row);
    if (rank < 2) continue;
    const percent = Number.isFinite(row.percent) ? row.percent : -1;
    const better = !best || rank > best.rank
      || (rank === best.rank && row.held && !best.row.held)
      || (rank === best.rank && row.held === best.row.held && percent > best.percent);
    if (better) best = { row, rank, percent };
  }
  return best;
}

function sublabel(row) {
  if (row.resource === 'cpu') return '1-minute load ÷ online cores';
  if (row.resource === 'memory') return 'used';
  if (row.resource === 'disk') return row.path || 'used';
  return 'time waiting on disk';
}

function setHidden(node, hidden) {
  if (!node) return;
  if (hidden) node.setAttribute('hidden', '');
  else node.removeAttribute?.('hidden');
  node.hidden = hidden;
}

function withStyle(node, style) {
  node.setAttribute('style', style);
  return node;
}

/// The rail-foot block: one button for load, mem and disk, plus io once this
/// host has reported disk I/O stall. Each row keeps its severity and held
/// state. The top bar shows one warning chip only while a reading is elevated
/// or worse; that chip and the block open the same dialog.
export function renderHostResources(payload, host = document.getElementById('host-block')) {
  if (!host) return;
  if (payload?.io) ioReported = true;
  const verdict = hostVerdict(payload);
  const resources = ioReported ? [...RESOURCES, 'io'] : RESOURCES;
  const rows = resources.map(resource => {
    const described = describeResource(payload, resource);
    const reading = payload?.[resource];
    const title = `${described.detail} · sampled ${verdict.age} · Throttle verdict: ${verdict.status}${described.held ? ' on this resource' : ''} · ${verdict.reason}`;
    return {
      resource, ...described, title, percent: reading?.percent,
      path: resource === 'disk' && reading?.path ? String(reading.path) : '',
      shown: described.value === '-' ? '–' : described.value,
    };
  });
  const alarm = verdict.verdictUnknown ? null : worstAlarm(rows);
  const tone = verdict.verdictUnknown ? 'stale'
    : rows.some(row => row.held || row.severity === 'critical') ? 'critical'
    : rows.some(row => row.severity === 'elevated') ? 'elevated'
    : 'ok';
  const named = alarm?.row.label;
  const compact = ageCompact(payload);
  const blockText = verdict.verdictUnknown ? (compact ? `stale · ${compact}` : 'unknown')
    : verdict.status === 'disabled' ? 'throttle off'
    : tone === 'critical' ? `holding work · ${named || 'a resource'}`
    : tone === 'elevated' ? `${named} near limit`
    : 'open';
  const summary = rows.map(row => `${row.label} ${row.shown}`).join(', ');
  host.className = `host-block ${tone}${detailOpen ? ' open' : ''}`;
  host.setAttribute('aria-expanded', detailOpen ? 'true' : 'false');
  host.setAttribute('aria-label', `Host resources: ${blockText}. ${summary}`);
  host.replaceChildren(
    el('span', { class: 'host-block-head' }, [
      el('span', { class: 'host-block-kicker', text: 'Host' }),
      el('span', { class: 'host-block-verdict' }, [
        el('span', { class: 'host-block-dot', 'aria-hidden': 'true' }),
        el('span', { class: 'host-block-verdict-text', text: blockText }),
      ]),
    ]),
    ...rows.map(row => {
      const node = el('span', {
        class: `host-resource ${row.severity}${row.held ? ' throttled' : ''}`,
        title: row.title, role: 'group', 'aria-label': row.title,
      }, [
        el('span', { class: 'k', text: row.label }),
        el('span', { class: 'v', text: row.shown }, row.suffix ? [el('span', { class: 'unit', text: row.suffix })] : []),
        el('span', { class: 'host-resource-bar', 'aria-hidden': 'true' }, [meterFill(row.fill)]),
        ...(row.held ? [el('span', { class: 'host-resource-held', text: 'throttled' })] : []),
      ]);
      node.dataset.resource = row.resource;
      return node;
    }),
  );
  const pill = verdict.verdictUnknown ? (compact ? 'Stale' : 'Unknown')
    : verdict.status === 'disabled' ? 'Throttle off'
    : tone === 'critical' ? `Holding work · ${named || 'a resource'}`
    : tone === 'elevated' ? `${named.charAt(0).toUpperCase()}${named.slice(1)} near limit`
    : 'Throttle open';
  renderHostWarning(alarm, tone);
  renderHostDetail(payload, rows, verdict, tone, pill, compact, alarm);
  bindHostDetail();
}

function renderHostWarning(alarm, tone) {
  const warn = document.getElementById('host-warn');
  if (!warn) return;
  const show = !!alarm && (tone === 'elevated' || tone === 'critical');
  setHidden(warn, !show);
  warn.className = `host-warn ${show ? tone : ''}`.trim();
  warn.setAttribute('aria-expanded', detailOpen ? 'true' : 'false');
  const label = document.getElementById('host-warn-label');
  const value = document.getElementById('host-warn-value');
  if (show) {
    if (label) label.textContent = alarm.row.label;
    if (value) value.textContent = alarm.row.shown;
    warn.setAttribute('aria-label', alarm.row.title);
  }
}

function renderHostDetail(payload, rows, verdict, tone, pill, compact, alarm) {
  const panel = document.getElementById('host-detail');
  if (!panel) return;
  const name = hostDisplayName();
  const resource = alarm?.row.resource;
  const limits = resource ? marks(payload, resource) : null;
  const plain = verdict.verdictUnknown
    ? (compact ? `Last sample ${compact} ago. ` : '')
    : verdict.status === 'disabled' ? 'Throttle disabled — pressure is still reported but holds nothing. '
    : tone === 'ok' ? 'Nothing is held back on this host. '
    : tone === 'critical' ? `New tasks wait until ${alarm.row.label} drops below ${limits.resume}%. `
    : `Admission is still open; ${alarm.row.label} is between ${limits.resume}% and its limit at ${limits.high}%. `;
  const foot = `${plain}Throttle verdict: ${verdict.status}. ${verdict.reason}`;
  const link = el('a', { class: 'host-detail-drain', text: 'Open Drain' });
  link.setAttribute('href', '#drain');
  panel.className = `host-detail ${tone}`;
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-label', `${name} resources`);
  setHidden(panel, !detailOpen);
  panel.replaceChildren(
    el('div', { class: 'host-detail-head' }, [
      el('span', { class: 'host-detail-name', id: 'host-detail-name', text: name }),
      el('span', { class: `host-detail-pill ${tone}`, text: pill }),
      el('span', { class: 'host-detail-age', text: verdict.age === 'age unknown' ? 'age unknown' : `sampled ${verdict.age}` }),
    ]),
    ...rows.map(row => detailRow(payload, row)),
    el('div', { class: 'host-detail-foot' }, [
      el('span', { class: 'host-detail-foot-text', text: foot }),
      link,
    ]),
  );
}

function detailRow(payload, row) {
  const { resume, high } = marks(payload, row.resource);
  const meter = el('div', { class: 'host-detail-meter' }, [
    withStyle(el('span', { class: 'host-detail-fill' }), `width: ${row.fill}%`),
    withStyle(el('span', { class: 'host-detail-tick elevated', title: `Elevated at ${resume}%` }), `left: ${Math.max(0, Math.min(100, resume))}%`),
    withStyle(el('span', { class: 'host-detail-tick critical', title: `Critical at ${high}%` }), `left: ${Math.max(0, Math.min(100, high))}%`),
  ]);
  const node = el('div', { class: `host-detail-row ${row.severity}` }, [
    el('div', { class: 'host-detail-row-head' }, [
      el('span', { class: 'host-detail-label', text: NAMES[row.resource] }),
      el('span', { class: 'host-detail-sub', text: sublabel(row) }),
      el('span', { class: 'host-detail-value', text: row.shown }),
    ]),
    meter,
    el('p', { class: 'host-detail-read', text: row.detail }),
  ]);
  node.dataset.resource = row.resource;
  return node;
}

function meterFill(percent) {
  const fill = el('span', { class: 'host-resource-fill' });
  fill.setAttribute('style', `width: ${percent}%`);
  return fill;
}

function placeHostDetail() {
  const panel = document.getElementById('host-detail');
  const block = document.getElementById('host-block');
  const warn = document.getElementById('host-warn');
  if (!panel || panel.hidden || typeof panel.getBoundingClientRect !== 'function') return;
  const anchor = block && block.getClientRects?.().length ? block : warn;
  if (!anchor || typeof anchor.getBoundingClientRect !== 'function') return;
  const box = anchor.getBoundingClientRect();
  const rail = document.querySelector?.('.rail');
  const railBox = rail?.getBoundingClientRect?.();
  const width = Math.min(400, Math.max(240, window.innerWidth - 16));
  panel.style.width = `${width}px`;
  let left = railBox && railBox.width > 80 ? railBox.right + 8 : 8;
  if (left + width > window.innerWidth - 8) left = Math.max(8, window.innerWidth - width - 8);
  panel.style.left = `${Math.round(left)}px`;
  panel.style.bottom = `${Math.round(Math.max(8, window.innerHeight - box.bottom))}px`;
  panel.style.top = 'auto';
}

function setDetailOpen(open) {
  detailOpen = !!open;
  const block = document.getElementById('host-block');
  const warn = document.getElementById('host-warn');
  const panel = document.getElementById('host-detail');
  if (block) block.setAttribute('aria-expanded', detailOpen ? 'true' : 'false');
  if (warn) warn.setAttribute('aria-expanded', detailOpen ? 'true' : 'false');
  if (block) block.classList?.toggle('open', detailOpen);
  if (panel) setHidden(panel, !detailOpen);
  if (detailOpen) placeHostDetail();
}

function bindHostDetail() {
  const block = document.getElementById('host-block');
  if (!block || block.dataset.bound === '1' || typeof block.addEventListener !== 'function') return;
  block.dataset.bound = '1';
  const warn = document.getElementById('host-warn');
  const panel = document.getElementById('host-detail');
  const toggle = event => {
    event.stopPropagation();
    setDetailOpen(!detailOpen);
  };
  block.addEventListener('click', toggle);
  warn?.addEventListener('click', toggle);
  panel?.addEventListener('click', event => {
    if (event.target.closest?.('a')) setDetailOpen(false);
  });
  document.addEventListener('keydown', event => {
    if (event.key === 'Escape' && detailOpen) setDetailOpen(false);
  });
  document.addEventListener('click', event => {
    if (!detailOpen) return;
    const target = event.target;
    if (block.contains(target) || warn?.contains(target) || panel?.contains(target)) return;
    setDetailOpen(false);
  });
  window.addEventListener?.('resize', () => { if (detailOpen) placeHostDetail(); });
  document.querySelector?.('.rail')?.addEventListener?.('scroll', () => { if (detailOpen) placeHostDetail(); });
}

// Provider usage chips: the selected host's readings, one per provider with
// each of its windows side by side, in the resource chips' visual language. They ride the host resource
// poll (`provider_limits` in its payload), so they follow the selected host and
// pause with it in a hidden tab.

// Beyond this many providers the rest fold into one `+N` chip, so the bar
// cannot grow without bound; the summary chip's title still lists every reading.
const MAX_CHIPS = 3;
// A reading within this many points of its limit reads as near it.
const NEAR_POINTS = 10;

const WINDOW_LABELS = { five_hour: '5h', seven_day: '7d', primary: '' };
// Shorter windows read first within a provider's chip; others keep API order.
const WINDOW_ORDER = { five_hour: 0, seven_day: 1 };

/// A model-scoped key (`seven_day_opus` under scope `opus`) loses its scope
/// suffix when `scope` is given, since the chip names the scope on its own.
function windowLabel(window, scope = '') {
  if (window == null || window === '') return '';
  const name = String(window);
  const base = scope && name.endsWith(`_${scope}`) ? name.slice(0, -scope.length - 1) : name;
  return base in WINDOW_LABELS ? WINDOW_LABELS[base] : base.replace(/_/g, ' ');
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
  const account = reading.account ? `${reading.provider}/${reading.account}` : reading.provider;
  const provider = `${account}${reading.scope ? ` ${reading.scope}` : ''}`;
  const key = `${provider}${short ? ` ${short}` : ''}`;
  const value = used != null ? `${Math.round(used)}%` : exhausted ? 'full' : '-';
  const crews = Array.isArray(reading.crews) && reading.crews.length ? `: ${reading.crews.join(', ')} skipped` : '';
  const state = gatingOff ? 'gating off'
    : gated ? `gated${used != null ? ` at ${used}% ≥ ${limit}%` : ''} until ${shortTime(reading.until)}${crews}`
    : reading.gating === false ? 'overage window; never gates'
    : `limit ${limit}%`;
  const detail = `${account}${scope} ${reading.window || 'usage window'} ${used != null ? `${used}%` : 'usage not reported'}`
    + `${reading.partial === true ? ' (partial: some invocations report no cost)' : ''}`
    + ` · observed ${reading.observed_at || 'at an unknown time'} (${age.label}${age.stale ? ' · stale' : ''}) · ${state}`;
  const flag = age.stale ? 'old' : gatingOff ? 'off' : '';
  return { key, provider, account, window: short, baseWindow: windowLabel(reading.window, reading.scope), order: WINDOW_ORDER[reading.window] ?? 2, value, severity, stale: age.stale, gatingOff, flag, detail };
}

// Values are supplied by the host read model; no pressure arithmetic runs here.
function pressureValue(pressure) {
  const value = pressure.pressure;
  return `×${value === 0 || (value >= 0.01 && value < 10000) ? Number(value.toFixed(2)) : value}`;
}

function describePressure(pressure) {
  const label = pressure.account ? `${pressure.provider}/${pressure.account}` : pressure.provider;
  const window = pressure.pressure_window;
  return `${label} ${pressureValue(pressure)} (${window
    ? `${window.window || 'weekly'}${window.scope ? ` [${window.scope}]` : ''} ${window.used_percent}%, resets ${shortTime(window.resets_at)}`
    : 'no live weekly usage'})`;
}

const RANK = { critical: 3, elevated: 2, ok: 1, unknown: 0 };

function chip(className, title, children) {
  return el('span', { class: className, title, role: 'group', 'aria-label': title }, children);
}

const states = item => `${item.stale ? ' stale' : ''}${item.gatingOff ? ' gating-off' : ''}`;
const flagNodes = item => item.flag ? [el('span', { class: 'provider-usage-flag', text: item.flag })] : [];

/// Readings grouped by account when pressure is available, otherwise by provider
/// and scope for older hosts. Each group's windows read shortest
/// first. A group takes its worst reading's severity, and is stale or gating
/// off only when every one of its readings is, since each window also flags
/// itself.
function providerGroups(described, pressures) {
  const groups = new Map();
  for (const item of described) {
    const key = pressures ? item.account : item.provider;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(item);
  }
  for (const pressure of pressures || []) {
    const label = pressure.account ? `${pressure.provider}/${pressure.account}` : pressure.provider;
    if (!groups.has(label)) groups.set(label, []);
  }
  return [...groups].map(([provider, items]) => {
    items.sort((a, b) => a.order - b.order);
    const worst = items.reduce((a, b) => RANK[b.severity] > RANK[a.severity] ? b : a, { severity: 'unknown' });
    const pressure = pressures?.find(value => (value.account ? `${value.provider}/${value.account}` : value.provider) === provider);
    const detail = pressure ? describePressure(pressure) : '';
    return {
      provider, items, pressure, severity: worst.severity,
      stale: items.length > 0 && items.every(item => item.stale), gatingOff: items.length > 0 && items.every(item => item.gatingOff),
      detail: [detail, ...items.map(item => item.detail)].filter(Boolean).join('\n'),
    };
  }).sort((a, b) => RANK[b.severity] - RANK[a.severity]);
}

/// One window inside a provider's chip: its value, its short label and any
/// flag, with a divider before every window but the first.
function windowNodes(item, index) {
  return [
    ...(index > 0 ? [el('span', { class: 'provider-usage-sep', 'aria-hidden': 'true', text: '|' })] : []),
    el('span', { class: 'v', text: item.value }),
    ...(item.window ? [el('span', { class: 'provider-usage-window', text: item.window })] : []),
    ...flagNodes(item),
  ];
}

/// The chips for `payload.provider_limits`: one per provider, worst first, each
/// showing every window of that provider (`claude 15% 5h | 32% 7d`), plus a
/// summary chip (the worst reading, every reading in its title) that CSS shows
/// in place of the list on narrow bars. A stale or gating-off reading says so
/// in a flag beside its window and in the title. A payload without readings
/// (an older host, a failed poll) keeps one placeholder chip, so the bar never
/// loses the group. Values have a minimum width, so a new reading does not
/// move the rest of the bar.
export function renderProviderUsage(payload, host = document.getElementById('provider-usage-chips')) {
  if (!host) return;
  const readings = Array.isArray(payload?.provider_limits)
    ? payload.provider_limits.filter(reading => reading && reading.provider) : null;
  const pressures = Array.isArray(payload?.provider_pressures)
    ? payload.provider_pressures.filter(value => value?.provider && Number.isFinite(value.pressure)) : null;
  if ((!readings || readings.length === 0) && !pressures?.length) {
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
  const described = (readings || []).map(reading => describeProviderReading(reading, now))
    .sort((a, b) => RANK[b.severity] - RANK[a.severity]);
  const groups = providerGroups(described, pressures);
  const all = groups.map(group => group.detail).join('\n');
  const chips = groups.slice(0, MAX_CHIPS).map(group =>
    chip(`host-resource provider-usage provider-usage-item ${group.severity}${states(group)}`, group.detail, [
      el('span', { class: 'k', text: group.provider }),
      ...(group.pressure ? [el('span', { class: 'v', text: pressureValue(group.pressure) })] : []),
      ...group.items.flatMap((item, index) => windowNodes({ ...item,
        window: pressures && item.provider !== item.account ? `${item.provider.slice(item.account.length + 1)} ${item.baseWindow}` : item.window,
      }, index)),
    ]));
  if (groups.length > MAX_CHIPS) {
    chips.push(chip('host-resource provider-usage provider-usage-item provider-usage-more unknown', all, [
      el('span', { class: 'k', text: `+${groups.length - MAX_CHIPS} more` }),
    ]));
  }
  const worst = described[0] || { severity: 'unknown', value: '-', stale: false, gatingOff: false };
  const account = groups[0];
  const summary = chip(`host-resource provider-usage provider-usage-summary ${worst.severity}${states(worst)}`, all, [
    el('span', { class: 'k', text: account?.pressure ? account.provider : 'usage' }),
    el('span', { class: 'v', text: account?.pressure ? pressureValue(account.pressure) : worst.value }),
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
export async function fetchHostResourcePayloadFor(hostName, { shareSelected = false, identity = null } = {}) {
  // Only share the selected host's current topbar read. Other hosts and
  // explicit resource reads still read independently. Matching machine and
  // SSH target prevents a registry edit from reusing the old host's reading.
  // A completed read is shared for at most one second.
  if (shareSelected && selectedRead && (selectedRead.host === hostName || (identity?.local && !selectedRead.host))
      && (!identity || selectedRead.target === resourceTarget(identity))
      && (selectedRead.pending || Date.now() - selectedRead.at < 1000)) {
    return selectedRead.promise;
  }
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
let selectedRead = null;
const resourceTarget = row => row ? JSON.stringify([row.machine_id, row.ssh || null]) : null;
export async function fetchHostResourcePayload() {
  const host = getHost();
  const read = { host, target: resourceTarget(host ? findRegisteredHost(host) : getServingHost()), pending: true, at: 0 };
  selectedRead = read;
  read.promise = fetchHostResourcePayloadFor(read.host);
  try {
    return await read.promise;
  } finally {
    read.pending = false;
    read.at = Date.now();
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
  selectedRead = null;
  ioReported = false;
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
