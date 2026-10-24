// Provider usage in the top bar: the selected host's readings, one compact chip
// per provider account. A chip opens a dialog with the detail the old title
// tooltip held: every window with its gate and reset, the weekly pressure and
// the crews held back. Hover previews it, focus or click opens it and a click
// or the Pin button keeps it open. The readings ride the host resource poll
// (`provider_limits` and `provider_pressures` in its payload), so they follow
// the selected host and pause with it in a hidden tab.
import { el, formatDateTime } from './common.js';

const DIALOG_ID = 'provider-usage-dialog';
// One dialog lists every provider; it opens from the `+N more` chip and, on
// narrow bars, from the single summary chip.
const ALL_KEY = '*';
// Beyond this many providers the rest fold into one `+N more` chip.
const MAX_CHIPS = 3;
// A reading within this many points of its limit reads as near it.
const NEAR_POINTS = 10;
// Hover opens after this intent delay, and a pointer that leaves has this long
// to reach the dialog before it closes.
const HOVER_DELAY_MS = 150;
const GRACE_MS = 220;
const DIALOG_WIDTH = 420;

const WINDOW_NAMES = { five_hour: '5-hour', seven_day: '7-day' };
// Shorter windows read first when two are equally used; others keep API order.
const WINDOW_ORDER = { five_hour: 0, seven_day: 1 };
const RANK = { critical: 3, elevated: 2, ok: 1, unknown: 0 };

// What the host's pressure means, from the admission contract: a higher value
// expires sooner unused; the owner only orders equal work by it and never
// holds any back.
const PRESSURE_EXPLAINED = 'Above ×1 this provider\'s weekly allowance would expire unused, so its crews are pulled sooner among otherwise equal work; below ×1 they are pulled later; at ×1 nothing changes. It only orders work and never holds any back.';

/// A window id as words: `five_hour` reads `5-hour`, `seven_day_opus` under
/// scope `opus` reads `7-day`, `gemini-argon-wtu-weekly` under scope `argon`
/// reads `gemini · wtu · weekly`. The scope is named by the row, not here.
function windowName(window, scope = '') {
  if (window == null || window === '') return 'usage';
  const id = String(window);
  const base = scope && id.endsWith(`_${scope}`) ? id.slice(0, -scope.length - 1) : id;
  if (base in WINDOW_NAMES) return WINDOW_NAMES[base];
  const words = base.split('-').filter(part => part && part !== scope).map(part => part.replace(/_/g, ' '));
  return words.length ? words.join(' · ') : base.replace(/_/g, ' ');
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

function validDate(iso) {
  const at = iso ? new Date(iso) : null;
  return at && !Number.isNaN(at.getTime()) ? at : null;
}

function spanText(seconds) {
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(seconds / 3600);
  if (minutes < 1) return 'under a minute';
  if (minutes < 60) return `${minutes}m`;
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

/// "2026-10-25 18:00 UTC · in 6h" on the dashboard clock; "" for no time.
function whenText(iso, now) {
  const at = validDate(iso);
  if (!at) return '';
  const seconds = Math.round((at.getTime() - now) / 1000);
  const relative = seconds >= 0 ? `in ${spanText(seconds)}` : `${spanText(-seconds)} ago`;
  return `${formatDateTime(iso)} · ${relative}`;
}

/// How one reading reads in the bar and its dialog. `severity` is `critical`
/// at or over the limit (or exhausted or gated), `elevated` within ten points
/// of it, `ok` below that and `unknown` when the provider reported no usage. A
/// provider whose gate is off has no limit to compare to, so 100% is its
/// limit; it stays marked `gatingOff` because nothing is held back on it. A
/// scoped window (a model family) is named by its scope, with the window as
/// its sublabel.
export function describeProviderReading(reading, now = Date.now()) {
  const age = providerReadingAge(reading, now);
  const gatingOff = reading.threshold === 'off';
  const limit = typeof reading.threshold === 'number' ? reading.threshold : 100;
  const gate = typeof reading.threshold === 'number' && reading.gating !== false ? reading.threshold : null;
  const used = Number.isFinite(reading.used_percent) ? reading.used_percent : null;
  const exhausted = reading.exhausted === true;
  const gated = reading.gated === true;
  const severity = gated || exhausted || (used != null && used >= limit) ? 'critical'
    : used == null ? 'unknown'
    : used >= limit - NEAR_POINTS ? 'elevated'
    : 'ok';
  const account = reading.account ? `${reading.provider}/${reading.account}` : reading.provider;
  const scope = reading.scope || '';
  const sub = windowName(reading.window, scope);
  const value = used != null ? `${Math.round(used)}%` : exhausted ? 'full' : '-';
  return {
    provider: `${account}${scope ? ` ${scope}` : ''}`, account, scope,
    name: scope || sub, sub: scope ? sub : '', label: scope ? `${scope} · ${sub}` : sub,
    order: WINDOW_ORDER[reading.window] ?? 2, used, value,
    fill: used != null ? Math.max(0, Math.min(100, used)) : exhausted ? 100 : 0,
    severity, stale: age.stale, ageLabel: age.label, gatingOff, noGate: gatingOff || reading.gating === false,
    gate, limit, gated, crews: Array.isArray(reading.crews) ? reading.crews : [],
    until: reading.until || null, observedAt: reading.observed_at || null,
    resetsAt: reading.resets_at || null, partial: reading.partial === true,
  };
}

// Values are supplied by the host read model; no pressure arithmetic runs here.
function pressureValue(pressure) {
  const value = pressure.pressure;
  return `×${value === 0 || (value >= 0.01 && value < 10000) ? Number(value.toFixed(2)) : value}`;
}

const accountLabel = pressure => pressure.account ? `${pressure.provider}/${pressure.account}` : pressure.provider;

/// Readings grouped by account when pressure is available, otherwise by
/// provider and scope for older hosts. A group takes its worst reading's
/// severity (then the more used window), and is stale or gating off only when
/// every one of its readings is, since each window also flags itself.
function providerGroups(described, pressures) {
  const groups = new Map();
  for (const item of described) {
    const key = pressures ? item.account : item.provider;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(item);
  }
  for (const pressure of pressures || []) {
    const label = accountLabel(pressure);
    if (!groups.has(label)) groups.set(label, []);
  }
  const byUsage = (a, b) => (b.used ?? -1) - (a.used ?? -1) || a.order - b.order;
  return [...groups].map(([provider, items]) => {
    items.sort(byUsage);
    const worst = items.reduce((a, b) => RANK[b.severity] > RANK[a.severity] ? b : a, items[0] || null);
    return {
      provider, items, worst, pressure: pressures?.find(value => accountLabel(value) === provider) || null,
      severity: worst?.severity || 'unknown',
      stale: items.length > 0 && items.every(item => item.stale),
      gatingOff: items.length > 0 && items.every(item => item.gatingOff),
    };
  }).sort((a, b) => RANK[b.severity] - RANK[a.severity]);
}

function pillFor(group) {
  const live = group.items.filter(item => !item.stale);
  const gated = live.filter(item => item.gated);
  if (group.items.length === 0) return { tone: 'unknown', text: 'no usage reported' };
  if (group.stale) return { tone: 'stale', text: `stale · ${group.items[0].ageLabel}` };
  if (gated.length) {
    const until = gated.map(item => item.until).filter(validDate).sort().at(-1);
    return { tone: 'critical', text: until ? `gated until ${formatDateTime(until)}` : 'gated' };
  }
  if (group.gatingOff) return { tone: 'off', text: 'gating off' };
  return {
    tone: group.severity,
    text: group.severity === 'critical' ? 'at limit' : group.severity === 'elevated' ? 'near limit'
      : group.severity === 'ok' ? 'within limits' : 'no usage reported',
  };
}

function observedText(group, now) {
  const times = group.items.map(item => item.observedAt).filter(validDate).sort();
  if (!times.length) return group.items.length ? 'observed at an unknown time' : '';
  const at = new Date(times.at(-1));
  return `observed ${formatDateTime(times.at(-1))} · ${spanText(Math.max(0, (now - at.getTime()) / 1000))} ago`;
}

function pressureModel(pressure, now) {
  if (!pressure) return null;
  const window = pressure.pressure_window;
  const basis = window
    ? [`${windowName(window.window || 'weekly', window.scope || '')}${window.scope ? ` · ${window.scope}` : ''}`,
      Number.isFinite(window.used_percent) ? `${Math.round(window.used_percent)}% used` : '',
      window.resets_at ? `resets ${whenText(window.resets_at, now)}` : ''].filter(Boolean).join(' · ')
    : 'No live weekly usage, so this reads neutral.';
  return { value: pressureValue(pressure), basis };
}

function rowModel(item, now) {
  const resets = whenText(item.resetsAt, now);
  return {
    name: item.name, sub: item.sub, value: item.value, fill: item.fill, gate: item.gate, severity: item.severity,
    reset: resets ? `${validDate(item.resetsAt).getTime() < now ? 'reset' : 'resets'} ${resets}` : '',
    gateText: item.noGate ? 'no gate' : `gate at ${item.limit}%`,
    flags: [item.stale ? 'old' : '', item.gated && !item.stale ? 'gated' : '', item.partial ? 'partial' : ''].filter(Boolean),
    stale: item.stale,
  };
}

/// Everything the bar and the dialogs print, as plain data: the memo that
/// keeps a poll with nothing new from rebuilding the chips under the pointer.
function groupModel(group, now) {
  const worst = group.worst;
  const pill = pillFor(group);
  const held = [...new Set(group.items.filter(item => item.gated && !item.stale).flatMap(item => item.crews))];
  const pressure = pressureModel(group.pressure, now);
  const flag = group.stale ? 'old' : group.gatingOff ? 'no gate' : '';
  const value = worst?.value ?? '-';
  return {
    key: group.provider, title: group.provider, label: `${group.provider} usage`, pill,
    observed: observedText(group, now), pressure, held, rows: group.items.map(item => rowModel(item, now)),
    chip: {
      name: group.provider, value, fill: worst?.fill ?? 0, severity: group.severity, stale: group.stale,
      gatingOff: group.gatingOff, flag, pressure: pressure?.value || '',
      window: worst?.label || '',
      aria: `${group.provider}: ${value} used${worst ? ` in the ${worst.label} window` : ''}${pressure ? `, weekly pressure ${pressure.value}` : ''}${flag ? `, ${flag === 'old' ? 'stale' : 'no gate'}` : ''}`,
    },
  };
}

function allModel(models) {
  return {
    key: ALL_KEY, title: 'Provider usage', label: 'Provider usage, every provider',
    count: `${models.length} provider${models.length === 1 ? '' : 's'}`,
    rows: models.map(model => ({
      name: model.title, value: model.chip.value, fill: model.chip.fill, severity: model.chip.severity,
      window: model.chip.window, flag: model.chip.flag, stale: model.chip.stale, pressure: model.chip.pressure,
    })),
  };
}

// ---- Bar chips ------------------------------------------------------------

const withStyle = (node, style) => { node.setAttribute('style', style); return node; };
const flagNode = flag => flag ? [el('span', { class: 'provider-usage-flag', text: flag })] : [];
const on = (node, type, handler) => node?.addEventListener?.(type, handler);

function setHidden(node, hidden) {
  if (hidden) node.setAttribute('hidden', '');
  else node.removeAttribute('hidden');
  node.hidden = hidden;
}

const chipClasses = (chip, kind) => `host-resource provider-usage ${kind} ${chip.severity}${chip.stale ? ' stale' : ''}${chip.gatingOff ? ' gating-off' : ''}`;

// The open dialog. `views` maps a key to its model, `chips` a key to the
// buttons that open it (a provider's own chip; the `+N more` and summary chips
// share the all-providers key).
const ui = {
  key: null, pinned: false, anchor: null, host: null, signature: '', views: new Map(), chips: new Map(),
  pointerInside: false, openTimer: null, closeTimer: null, suppressFocus: false, bound: false, pinButton: null,
};

function dialogNode() {
  let node = document.getElementById(DIALOG_ID);
  if (!node) {
    node = el('div', { id: DIALOG_ID, class: 'usage-dialog', role: 'dialog' });
    setHidden(node, true);
    document.body?.append(node);
  }
  return node;
}

function usageButton(className, key, label, children) {
  const node = el('button', {
    class: className, type: 'button', 'aria-label': label,
    'aria-haspopup': 'dialog', 'aria-controls': DIALOG_ID,
  }, children);
  node.setAttribute('aria-expanded', 'false');
  node.dataset.usageKey = key;
  if (!ui.chips.has(key)) ui.chips.set(key, []);
  ui.chips.get(key).push(node);
  bindChip(node, key);
  return node;
}

function chipBody(chip, extra = []) {
  return [
    el('span', { class: 'k', text: chip.name }),
    el('span', { class: 'v', text: chip.value }),
    el('span', { class: 'provider-usage-meter', 'aria-hidden': 'true' }, [
      withStyle(el('span', { class: 'provider-usage-meter-fill' }), `width: ${chip.fill}%`),
    ]),
    ...(chip.pressure ? [el('span', { class: 'provider-usage-pressure', text: chip.pressure })] : []),
    ...flagNode(chip.flag),
    ...extra,
  ];
}

// ---- Dialog content -------------------------------------------------------

function meter(fill, gate) {
  return el('span', { class: 'usage-meter', 'aria-hidden': 'true' }, [
    withStyle(el('span', { class: 'usage-meter-fill' }), `width: ${fill}%`),
    ...(gate != null ? [withStyle(el('span', { class: 'usage-meter-tick' }), `left: ${Math.max(0, Math.min(100, gate))}%`)] : []),
  ]);
}

function windowRow(row) {
  return el('div', { class: `usage-row ${row.severity}${row.stale ? ' stale' : ''}`, role: 'listitem' }, [
    el('div', { class: 'usage-row-head' }, [
      el('span', { class: 'usage-row-name', text: row.name }),
      ...(row.sub ? [el('span', { class: 'usage-row-sub', text: row.sub })] : []),
      ...row.flags.map(flag => el('span', { class: 'provider-usage-flag', text: flag })),
      el('span', { class: 'usage-row-value', text: row.value }),
    ]),
    meter(row.fill, row.gate),
    el('div', { class: 'usage-row-foot' }, [
      el('span', { class: 'usage-row-reset', text: row.reset }),
      el('span', { class: 'usage-row-gate', text: row.gateText }),
    ]),
  ]);
}

function providerRow(row) {
  return el('div', { class: `usage-row usage-row-provider ${row.severity}${row.stale ? ' stale' : ''}`, role: 'listitem' }, [
    el('div', { class: 'usage-row-head' }, [
      el('span', { class: 'usage-row-name', text: row.name }),
      ...flagNode(row.flag),
      el('span', { class: 'usage-row-value', text: row.value }),
    ]),
    meter(row.fill, null),
    el('div', { class: 'usage-row-foot' }, [
      el('span', { class: 'usage-row-reset', text: row.window }),
      ...(row.pressure ? [el('span', { class: 'usage-row-gate', text: `pressure ${row.pressure}` })] : []),
    ]),
  ]);
}

function pinButton() {
  const button = el('button', { class: 'usage-pin', type: 'button', 'aria-pressed': ui.pinned ? 'true' : 'false', text: ui.pinned ? 'Unpin' : 'Pin open' });
  on(button, 'click', () => {
    ui.pinned = !ui.pinned;
    button.textContent = ui.pinned ? 'Unpin' : 'Pin open';
    button.setAttribute('aria-pressed', ui.pinned ? 'true' : 'false');
    if (!ui.pinned && !ui.pointerInside) scheduleClose();
  });
  ui.pinButton = button;
  return button;
}

function providerDialog(view) {
  return [
    el('div', { class: 'usage-head' }, [
      el('span', { class: 'usage-title', text: view.title }),
      el('span', { class: `usage-pill ${view.pill.tone}`, text: view.pill.text }),
      ...(view.observed ? [el('span', { class: 'usage-observed', text: view.observed })] : []),
    ]),
    ...(view.pressure ? [el('section', { class: 'usage-section usage-pressure' }, [
      el('div', { class: 'usage-section-head' }, [
        el('h3', { class: 'usage-section-title', text: 'Weekly pressure' }),
        el('span', { class: 'usage-pressure-value', text: view.pressure.value }),
      ]),
      el('p', { class: 'usage-note', text: PRESSURE_EXPLAINED }),
      el('p', { class: 'usage-note usage-basis', text: view.pressure.basis }),
    ])] : []),
    el('section', { class: 'usage-section' }, [
      el('h3', { class: 'usage-section-title', text: 'Windows' }),
      el('div', { class: 'usage-rows', role: 'list' },
        view.rows.length ? view.rows.map(windowRow) : [el('p', { class: 'usage-note', text: 'No usage windows reported.' })]),
    ]),
    el('div', { class: 'usage-foot' }, [
      el('span', { class: 'usage-foot-text', text: view.held.length ? `Held back: ${view.held.join(', ')}` : 'No crews held back' }),
      pinButton(),
    ]),
  ];
}

function allDialog(view) {
  return [
    el('div', { class: 'usage-head' }, [
      el('span', { class: 'usage-title', text: view.title }),
      el('span', { class: 'usage-observed', text: view.count }),
    ]),
    el('section', { class: 'usage-section' }, [
      el('div', { class: 'usage-rows', role: 'list' }, view.rows.map(providerRow)),
    ]),
    el('div', { class: 'usage-foot' }, [
      el('span', { class: 'usage-foot-text', text: 'Worst first' }),
      pinButton(),
    ]),
  ];
}

// ---- Open and close -------------------------------------------------------

function clearTimers() {
  clearTimeout(ui.openTimer);
  clearTimeout(ui.closeTimer);
  ui.openTimer = null;
  ui.closeTimer = null;
}

/// The button that opens `key` and is on screen: the item chip at 1900px and
/// wider, the summary chip below it.
function visibleChip(key) {
  const nodes = ui.chips.get(key) || [];
  return nodes.find(node => typeof node.getClientRects !== 'function' || node.getClientRects().length) || null;
}

function syncExpanded() {
  for (const [key, nodes] of ui.chips) {
    for (const node of nodes) node.setAttribute('aria-expanded', key === ui.key && node === ui.anchor ? 'true' : 'false');
  }
}

function placeDialog() {
  const dialog = dialogNode();
  const anchor = ui.anchor;
  if (!ui.key || !anchor || typeof anchor.getBoundingClientRect !== 'function') return;
  const box = anchor.getBoundingClientRect();
  const width = Math.min(DIALOG_WIDTH, window.innerWidth - 16);
  const left = Math.max(8, Math.min(box.right - width, window.innerWidth - width - 8));
  const top = Math.round(box.bottom + 4);
  dialog.setAttribute('style', `width: ${width}px; left: ${Math.round(left)}px; top: ${top}px; max-height: ${Math.max(160, Math.round(window.innerHeight - top - 12))}px`);
}

function fillDialog() {
  const view = ui.views.get(ui.key);
  const dialog = dialogNode();
  dialog.setAttribute('role', 'dialog');
  dialog.setAttribute('aria-label', view.label);
  dialog.className = `usage-dialog ${view.key === ALL_KEY ? 'usage-dialog-all' : 'usage-dialog-provider'}`;
  dialog.replaceChildren(...(view.key === ALL_KEY ? allDialog(view) : providerDialog(view)));
}

function closeUsage({ restoreFocus = false } = {}) {
  clearTimers();
  const anchor = ui.anchor;
  const hadFocus = dialogNode().contains?.(document.activeElement);
  ui.key = null;
  ui.pinned = false;
  ui.anchor = null;
  setHidden(dialogNode(), true);
  syncExpanded();
  if (restoreFocus && hadFocus) focusQuietly(anchor);
}

function openUsage(key, anchor, pin) {
  if (!ui.views.has(key) || !anchor) return;
  clearTimers();
  if (ui.key !== key) ui.pinned = false;
  if (pin) ui.pinned = true;
  const refresh = ui.key !== key;
  ui.key = key;
  ui.anchor = anchor;
  if (refresh || !ui.pinButton) fillDialog();
  setHidden(dialogNode(), false);
  placeDialog();
  syncExpanded();
  ui.pinButton?.setAttribute('aria-pressed', ui.pinned ? 'true' : 'false');
  if (ui.pinButton) ui.pinButton.textContent = ui.pinned ? 'Unpin' : 'Pin open';
}

function scheduleClose() {
  clearTimeout(ui.closeTimer);
  ui.closeTimer = setTimeout(() => {
    if (!ui.pointerInside && !ui.pinned) closeUsage();
  }, GRACE_MS);
}

function focusQuietly(node) {
  if (!node || typeof node.focus !== 'function') return;
  ui.suppressFocus = true;
  try { node.focus(); } finally { ui.suppressFocus = false; }
}

const insideUsage = target => !!target?.closest?.('.provider-usage, .usage-dialog');

function bindChip(node, key) {
  const mouse = event => event?.pointerType !== 'touch';
  on(node, 'pointerenter', event => {
    if (!mouse(event)) return;
    ui.pointerInside = true;
    clearTimeout(ui.closeTimer);
    if (ui.key === key || (ui.key && ui.pinned)) return;
    clearTimeout(ui.openTimer);
    ui.openTimer = setTimeout(() => openUsage(key, visibleChip(key) || node, false), HOVER_DELAY_MS);
  });
  on(node, 'pointerleave', event => {
    if (!mouse(event)) return;
    ui.pointerInside = false;
    clearTimeout(ui.openTimer);
    if (ui.key && !ui.pinned) scheduleClose();
  });
  on(node, 'focus', () => { if (!ui.suppressFocus) openUsage(key, node, false); });
  on(node, 'blur', event => {
    if (ui.key !== key || ui.pinned || ui.pointerInside) return;
    if (dialogNode().contains?.(event?.relatedTarget)) return;
    closeUsage();
  });
  on(node, 'click', () => {
    clearTimeout(ui.openTimer);
    if (ui.key === key && ui.pinned) closeUsage();
    else openUsage(key, node, true);
  });
  on(node, 'keydown', event => {
    // Tab moves into the open dialog, which sits at the end of the page.
    if (event.key !== 'Tab' || event.shiftKey || ui.key !== key || !ui.pinButton) return;
    event.preventDefault?.();
    ui.pinButton.focus?.();
  });
}

function bindOnce() {
  if (ui.bound) return;
  ui.bound = true;
  const dialog = dialogNode();
  setHidden(dialog, true);
  on(dialog, 'pointerenter', event => {
    if (event?.pointerType === 'touch') return;
    ui.pointerInside = true;
    clearTimeout(ui.closeTimer);
  });
  on(dialog, 'pointerleave', event => {
    if (event?.pointerType === 'touch') return;
    ui.pointerInside = false;
    if (!ui.pinned) scheduleClose();
  });
  on(dialog, 'keydown', event => {
    if (event.key !== 'Tab' || !ui.anchor) return;
    // Back to the chip; a forward Tab then continues past it.
    if (event.shiftKey) event.preventDefault?.();
    focusQuietly(ui.anchor);
  });
  on(document, 'keydown', event => {
    if (event.key === 'Escape' && ui.key) closeUsage({ restoreFocus: true });
  });
  on(document, 'click', event => {
    if (ui.key && !insideUsage(event.target)) closeUsage();
  });
  const reposition = () => { if (ui.key) placeDialog(); };
  on(window, 'resize', () => {
    if (!ui.key) return;
    // A breakpoint can swap the chip that opened it for another.
    const anchor = visibleChip(ui.key);
    if (!anchor) closeUsage();
    else { ui.anchor = anchor; placeDialog(); }
  });
  window.addEventListener?.('scroll', reposition, true);
}

// ---- Render ---------------------------------------------------------------

function placeholder(title) {
  return el('span', { class: 'host-resource provider-usage unknown', title, role: 'group', 'aria-label': title }, [
    el('span', { class: 'k', text: 'usage' }),
    el('span', { class: 'v', text: '-' }),
  ]);
}

/// The chips for `payload.provider_limits`: one button per provider, worst
/// first, showing its name, its worst window's value, a meter and the weekly
/// pressure (`claude 13% ▮ ×1.09`), with the rest folded into `+N more`; below
/// 1900px CSS shows one summary chip instead. Each opens a dialog with the
/// provider's windows; `+N more` and the summary chip open one that lists
/// every provider. A stale or gating-off provider says so in a flag. A payload
/// without readings (an older host, a failed poll) keeps one placeholder chip
/// that explains itself in its title, so the bar never loses the group. A poll
/// that changes nothing leaves the chips and an open dialog alone.
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
    closeUsage();
    ui.host = host;
    ui.signature = '';
    ui.views = new Map();
    ui.chips = new Map();
    host.replaceChildren(placeholder(title));
    return;
  }
  const now = Date.now();
  const described = (readings || []).map(reading => describeProviderReading(reading, now));
  const groups = providerGroups(described, pressures);
  const models = groups.map(group => groupModel(group, now));
  const everyone = allModel(models);
  const signature = JSON.stringify([models, everyone]);
  if (ui.host === host && ui.signature === signature) return;

  const refocus = document.activeElement?.dataset?.usageKey;
  // The rebuilt chips are under the same pointer, but the removed ones never
  // report it leaving.
  ui.pointerInside = !!(ui.key && (ui.anchor?.matches?.(':hover') || dialogNode().matches?.(':hover')));
  ui.host = host;
  ui.signature = signature;
  ui.views = new Map([...models, everyone].map(model => [model.key, model]));
  ui.chips = new Map();
  const more = models.length - MAX_CHIPS;
  const chips = models.slice(0, MAX_CHIPS).map(model =>
    usageButton(chipClasses(model.chip, 'provider-usage-item'), model.key, model.chip.aria, chipBody(model.chip)));
  if (more > 0) {
    chips.push(usageButton('host-resource provider-usage provider-usage-item provider-usage-more unknown', ALL_KEY,
      `${more} more provider${more === 1 ? '' : 's'}: show every provider's usage`, [el('span', { class: 'k', text: `+${more} more` })]));
  }
  const lead = models[0].chip;
  const summary = usageButton(chipClasses(lead, 'provider-usage-summary'), ALL_KEY,
    `Provider usage: ${models.length} provider${models.length === 1 ? '' : 's'}, worst ${lead.aria}`,
    chipBody(lead, models.length > 1 ? [el('span', { class: 'provider-usage-count', text: `+${models.length - 1}` })] : []));
  bindOnce();
  host.replaceChildren(summary, ...chips);
  if (ui.key && ui.views.has(ui.key)) {
    ui.anchor = visibleChip(ui.key);
    if (ui.anchor) { fillDialog(); setHidden(dialogNode(), false); placeDialog(); syncExpanded(); }
    else closeUsage();
  } else if (ui.key) closeUsage();
  if (refocus && ui.chips.has(refocus)) focusQuietly(ui.chips.get(refocus).find(node => node === visibleChip(refocus)));
}
