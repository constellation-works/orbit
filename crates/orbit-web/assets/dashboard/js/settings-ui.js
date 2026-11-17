// The building blocks every Settings page shares: the bar at the top, section
// panels, and the name / value / source / action row. settings.css styles
// them; config-topics.js (topic pages), config.js (Crews, the throttle panel)
// and hosts.js (Hosts) compose them.
import { el, pinnedInfoHint } from './common.js';
import { svg } from './sparkline.js';

const SETTINGS_ICONS = {
  search: [['circle', { cx: 7, cy: 7, r: 4.5 }], ['path', { d: 'M10.5 10.5 14 14' }]],
  expand: [['path', { d: 'm5 5 3-3 3 3M5 11l3 3 3-3' }]],
  collapse: [['path', { d: 'm5 2 3 3 3-3M5 14l3-3 3 3' }]],
  reload: [['path', { d: 'M13.5 8a5.5 5.5 0 1 1-1.6-3.9M13.5 2.5v3h-3' }]],
  open: [['path', { d: 'm4 6 4 4 4-4' }]],
  closed: [['path', { d: 'm6 4 4 4-4 4' }]],
  pencil: [['path', { d: 'M11 2.5 13.5 5 5.5 13H3v-2.5z' }]],
};
export function settingsIcon(name, size = 14) {
  const icon = svg('svg', { viewBox: '0 0 16 16', width: size, height: size, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true' });
  for (const [tag, attrs] of SETTINGS_ICONS[name]) icon.appendChild(svg(tag, attrs));
  return icon;
}

// The sticky Workflow bar covers the top of the pane, so the pane publishes
// the bar's height and sections reserve it as scroll margin (settings.css): a
// jump or a keyboard focus lands below the bar, not under it.
let settingsBarObserver = null;
export function syncSettingsBarHeight(bar) {
  const pane = bar?.closest('.tab-pane');
  if (!pane) return;
  pane.style.setProperty('--settings-bar-h', bar.classList.contains('sticky') ? `${bar.offsetHeight}px` : '0px');
  if (!settingsBarObserver && typeof ResizeObserver === 'function') {
    settingsBarObserver = new ResizeObserver(() => syncSettingsBarHeight(bar));
    settingsBarObserver.observe(bar);
  }
}

/// The one bar at the top of every Settings page: the page's own controls
/// (`lead`), then its actions and Reload at the right end.
export function renderSettingsBar({ lead = [], actions = [], onReload, reloadDescription = null, sticky = false }) {
  const bar = document.getElementById('config-controls');
  if (!bar) return;
  bar.hidden = false;
  bar.classList.toggle('sticky', sticky);
  const reload = el('button', { class: 'settings-icon-btn settings-reload', "aria-description": reloadDescription, 'aria-label': 'Reload' }, [settingsIcon('reload')]);
  reload.type = 'button';
  reload.addEventListener('click', onReload);
  bar.replaceChildren(...lead.filter(Boolean), el('div', { class: 'settings-bar-actions' }, [...actions.filter(Boolean), reload]));
  syncSettingsBarHeight(bar);
}

/// A search or filter field for the bar. `cls` names the input for its page.
export function settingsSearchField({ cls, placeholder, label, value, onInput }) {
  const input = el('input', { class: cls, type: 'search', placeholder, 'aria-label': label });
  input.value = value;
  input.addEventListener('input', () => onInput(input));
  return el('label', { class: 'settings-search' }, [settingsIcon('search'), input]);
}

/// A section panel: title, count, an optional state pill and, where the
/// section collapses, a chevron toggle. Returns the section and its body.
export function settingsSection(title, { id = null, count = null, state = null, collapsible = false, collapsed = false, onToggle = null } = {}) {
  const section = el('section', { class: 'settings-section' });
  if (id) section.id = id;
  const bodyId = id ? `${id}-body` : null;
  const shut = Boolean(collapsible && collapsed);
  const head = el('header', { class: `settings-section-head${shut ? '' : ' open'}` }, [
    el('h2', { class: 'settings-section-title', text: title }),
    count == null ? null : el('span', { class: 'settings-section-count', text: count }),
    state ? el('span', { class: `settings-section-state${state.tone ? ` ${state.tone}` : ''}`, text: state.text }) : null,
  ]);
  if (collapsible) {
    const toggle = el('button', { class: 'settings-section-toggle' }, [settingsIcon(shut ? 'closed' : 'open')]);
    toggle.type = 'button';
    toggle.setAttribute('aria-expanded', String(!shut));
    if (bodyId) toggle.setAttribute('aria-controls', bodyId);
    toggle.setAttribute('aria-label', `${shut ? 'Expand' : 'Collapse'} ${title}`);
    toggle.addEventListener('click', () => onToggle?.());
    head.appendChild(toggle);
  }
  const body = el('div', { class: 'settings-section-body' });
  if (bodyId) body.id = bodyId;
  if (shut) body.hidden = true;
  section.append(head, body);
  return { section, body };
}

/// One row: name, value, source and action in the same four columns on every
/// page. A `wide` value spans the source and action columns too.
export function settingsRow({ name, value, source = null, action = null, wide = false, dim = false }) {
  const node = el('div', { class: `settings-row${dim ? ' dim' : ''}` }, [
    el('div', { class: 'settings-name' }, name),
    el('div', { class: `settings-val${wide ? ' settings-wide' : ''}` }, value),
  ]);
  if (!wide) node.append(source || el('div', { class: 'settings-src' }), action || el('div', { class: 'settings-act' }));
  return node;
}

/// A key as a row name: the full key in monospace with its prefix dimmed (the
/// part before the row's label, or `prefix` when the section shares one), and
/// the description on a line under it.
export function settingsKeyName(key, { label = null, description = null, prefix = null } = {}) {
  let pre = '';
  if (prefix && key.startsWith(prefix)) pre = prefix;
  else if (label && label !== key && key.endsWith(`.${label}`)) pre = key.slice(0, key.length - label.length);
  return [
    el('div', { class: 'settings-key' }, [pre ? el('span', { class: 'settings-key-pre', text: pre }) : null, key.slice(pre.length)]),
    description ? el('div', { class: 'settings-desc', text: description }) : null,
  ];
}
// A row that is not a key (a review layer, the registry binding, a path).
export function settingsLabelName(label, description = null) {
  return [el('div', { class: 'settings-label', text: label }), description ? el('div', { class: 'settings-desc', text: description }) : null];
}

const SOURCE_LABELS = { default: 'Default', 'built-in': 'Default', unset: 'Unset', workspace: 'Workspace', global: 'Global', registry: 'Registry', environment: 'Environment' };
export function settingsLayerLabel(layer) {
  return SOURCE_LABELS[layer] || (layer ? `${layer[0].toUpperCase()}${layer.slice(1)}` : '');
}
/// Layers that count as set for the source dot: a value written in one of
/// these files (or the environment) gets the accent, `built-in` alone does not.
const SETTING_LAYERS = new Set(['workspace', 'global', 'environment']);
/// Whether a (possibly composite, `+`-joined) source names a layer that sets
/// the value — e.g. `global+workspace` from a crew defined in both files.
export function settingsSourceIsSet(layer) {
  return String(layer ?? '').split('+').some((part) => SETTING_LAYERS.has(part));
}
/// Where a value comes from, as text; the dot marks a value set in a layer.
/// A composite source (`global+workspace`) names each layer it came from.
export function settingsSource(layer, { set = false, title = null } = {}) {
  const text = String(layer ?? '').split('+').map((part) => settingsLayerLabel(part)).join(' + ');
  return el('div', { class: `settings-src${set ? ' set' : ''}`, text, title });
}
export function settingsRowSource(row) {
  const set = row.state === 'set';
  return settingsSource(set ? row.source?.layer || 'set' : row.state, { set, title: row.source?.path || null });
}

/// The edit affordance: "Edit", or a pencil where a table cell has no room.
export function settingsEditButton(onClick, label, { icon = false } = {}) {
  const button = icon
    ? el('button', { class: 'settings-icon-btn settings-edit', 'aria-label': label }, [settingsIcon('pencil', 12)])
    : el('button', { class: 'settings-btn settings-edit', text: 'Edit', 'aria-label': label });
  button.type = 'button';
  button.addEventListener('click', (event) => { event.stopPropagation(); onClick(); });
  return button;
}
export function settingsReadOnlyAction(reason) {
  return el('div', { class: 'settings-act' }, ['Read-only', reason ? pinnedInfoHint(reason) : null]);
}
