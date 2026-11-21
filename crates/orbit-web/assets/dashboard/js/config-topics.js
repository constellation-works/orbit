// Settings topic pages: Workflow, Execution, Review, Machine, Other, built
// from the shared blocks in settings-ui.js.
// Rows come from /api/config/effective; the shared drawer writes via
// PUT/DELETE /api/config/keys/:key and re-reads via context.reload.
import { captureWorkspaceVisit, el, escCloseButton, fetchJson, requestJson } from './common.js';
import { settingsEditButton, settingsIcon, settingsKeyName, settingsLabelName, settingsLayerLabel, settingsReadOnlyAction, settingsRow, settingsRowSource, settingsSearchField, settingsSection, settingsSource, syncSettingsBarHeight } from './settings-ui.js';

let drawer = null; // {row, scope, text, error, pending, visit, lockGlobal}
let bulkRemoval = null;
let workflowQuery = '';
let workflowCollapsed = new Set(); // the saved layout, in force when no search is active
let workflowSearchCollapsed = new Set(); // sections collapsed while a search is active
let expandedValues = new Set(); // rows whose long value ("N more", "Show full") is open
let rawScope = null;
let rawText = null; // null while the latest read is in flight
let rawSeq = 0; // numbers each read so only the latest click can apply its response
let notice = null; // {text, failed}: the last bulk removal's outcome
let landedOtherHash = null; // the key link already landed during this visit

window.addEventListener('hashchange', (event) => {
  if (landedOtherHash && new URL(event.oldURL).hash === landedOtherHash && new URL(event.newURL).hash !== landedOtherHash) {
    landedOtherHash = null;
  }
});
const NOTICE_MS = 10000;

export function resetTopics() { drawer = null; bulkRemoval = null; workflowQuery = ''; workflowCollapsed = new Set(); workflowSearchCollapsed = new Set(); expandedValues = new Set(); rawScope = null; rawText = null; notice = null; landedOtherHash = null; }

// The Machine page's key drawer is this module's state. config.js's host
// poll must not rebuild the tab while it is open.
export function topicDrawerOpen() {
  return drawer != null;
}

export function flatten(payload) {
  return (payload?.sections || []).flatMap((s) => s.keys || []);
}
function byKey(payload) { const m = new Map(); for (const r of flatten(payload)) m.set(r.key, r); return m; }
export function display(v) {
  if (v == null) return '–';
  if (Array.isArray(v)) return v.length ? v.map(listEntryText).join(v.some(isObject) ? '; ' : ' ') : '—';
  if (typeof v === 'string') return v === '' ? '""' : v;
  if (typeof v === 'object') {
    const leaves = tableLeaves(v);
    return leaves.length ? leaves.map(([path, leaf]) => `${path}: ${display(leaf)}`).join('; ') : '—';
  }
  return String(v);
}
function isObject(v) {
  return Boolean(v) && typeof v === 'object';
}
// One list entry: a string or number as itself, a table such as a
// completion.hold rule as `{name: x; paths: a b}`.
function listEntryText(v) {
  if (Array.isArray(v)) return `[${display(v)}]`;
  return isObject(v) ? `{${display(v)}}` : String(v);
}
// A table value's leaves as [dotted path, value], so a nested table such as
// workflow.type_complexity_crews reads `ui.hard: opus` rather than an object.
function tableLeaves(table, prefix = '') {
  return Object.entries(table).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    return value && typeof value === 'object' && !Array.isArray(value) ? tableLeaves(value, path) : [[path, value]];
  });
}
function isTable(row) {
  return (row.value_type || '').startsWith('table<');
}
// A list of tables (completion.hold, workflow.task_types, workflow.lock_modes):
// edited as a JSON array, like a table is edited as a JSON object.
function isTableList(row) {
  return (row.value_type || '').startsWith('array<table')
    || (Array.isArray(row.value) && row.value.some(isObject));
}
// Keys whose drawer text is JSON rather than one entry per line.
function isStructured(row) {
  return isTable(row) || isTableList(row);
}
// TOML inline-table text for a JSON table, as `orbit config set` reads it.
// TOML has no null, so a null leaf cannot be shown as a runnable command.
function tomlInline(v) {
  if (v === null) throw new Error('TOML cannot represent null');
  if (Array.isArray(v)) return `[${v.map(tomlInline).join(', ')}]`;
  if (v && typeof v === 'object') {
    const key = (k) => (/^[A-Za-z0-9_-]+$/.test(k) ? k : JSON.stringify(k));
    return `{${Object.entries(v).map(([k, value]) => `${key(k)} = ${tomlInline(value)}`).join(', ')}}`;
  }
  return typeof v === 'string' ? JSON.stringify(v) : String(v);
}
export function topicOf(key) {
  if (key.startsWith('execution.')) return 'execution';
  if (key.startsWith('review.') || key.startsWith('completion.') || key === 'workflow.purpose_crews.review' || key === 'workflow.purpose_crews.pre_landing_review') return 'review';
  if (key.startsWith('machine.') || key === 'tasks.id_start' || key.startsWith('workflow.resource_throttle.')) return 'machine';
  if (key.startsWith('workflow.') || key.startsWith('pr.')) return 'workflow';
  if (key.startsWith('crews.')) return 'crews';
  return 'housekeeping';
}

const WORKFLOW_SECTIONS = [
  ['Shipping', (k, p) => ['workflow.base_branch','workflow.distributed_completion','workflow.required_validation_commands'].includes(k) || k.startsWith('workflow.validation_env.') || k.startsWith('pr.')],
  ['Crew routing', (k) => k === 'workflow.default_crew' || /complexity_crews$/.test(k) || k === 'workflow.crew_selection' || k === 'workflow.task_types' || k.startsWith('workflow.provider_limit')],
  ['Task pilot', (k) => k === 'workflow.purpose_crews.task_pilot' || k.startsWith('workflow.task_pilot') || k.includes('task_pilot_freshness') || k === 'workflow.pilot_max_complexity'],
  ['Recovery', (k) => ['workflow.purpose_crews.failure_recovery','workflow.purpose_crews.auto_fix','workflow.purpose_crews.final_recovery','workflow.purpose_crews.baseline_red_repair','workflow.purpose_crews.sweep_tasks'].includes(k)],
  ['Steering', (k) => k.startsWith('workflow.mid_run_steering.')],
];

function openDrawer(row, context, lockGlobal, sectionTitle) {
  // The drawer rows stay reachable behind it, so an Edit on another row must
  // not replace a write still in flight: its result would be dropped.
  if (!context.canEdit || row.settable === false || drawer?.pending) return;
  drawer = { row, scope: (lockGlobal || row.global_only) ? 'global' : (row.source?.layer === 'global' ? 'global' : 'workspace'),
    text: isStructured(row) ? JSON.stringify(row.value ?? (isTable(row) ? {} : []), null, 2) : Array.isArray(row.value) ? row.value.join('\n') : display(row.value) === '–' ? '' : String(row.value ?? ''),
    error: null, pending: false, visit: captureWorkspaceVisit(), lockGlobal: !!(lockGlobal || row.global_only),
    sectionTitle: sectionTitle || null };
  context.rerender?.();
  document.querySelector('.config-drawer .settings-input')?.focus();
}
// Closing the drawer rebuilds the page, so the Edit control that opened it is
// gone. Focus goes to the rebuilt Edit control for the same key; a row that is
// no longer shown keeps whatever focus the rerender left.
function focusRowEdit(key, sectionTitle) {
  const rows = [...document.querySelectorAll('#config-body [data-key]')].filter((node) => node.dataset.key === key);
  const section = sectionTitle ? sectionSlug(sectionTitle) : '';
  const row = (section && rows.find((node) => node.id.startsWith(`key-${section}-`))) || rows[0];
  row?.querySelector('.settings-edit')?.focus();
}
function closeDrawer(session, context) {
  // A write in flight keeps its editor open: its result, success or refusal,
  // lands in this drawer, and dismissing it would drop that result.
  if (session.pending) return;
  drawer = null;
  context.rerender?.();
  focusRowEdit(session.row.key, session.sectionTitle);
}
function shellQuote(text) {
  return `'${text.replace(/'/g, "'\\''")}'`;
}
function shellWord(text) {
  return /^[A-Za-z0-9_.\/:@%+=,-]+$/.test(text) ? text : shellQuote(text);
}

export function parseDraft(row, text) {
  const t = row.value_type || '';
  if (t === 'bool') {
    const trimmed = text.trim();
    if (trimmed === 'true') return true;
    if (trimmed === 'false') return false;
    throw new Error(`${row.key} takes true or false`);
  }
  if (t === 'integer') {
    const trimmed = text.trim();
    if (!/^-?\d+$/.test(trimmed)) throw new Error(`${row.key} takes a whole number`);
    return inAllowedRange(row, Number(trimmed));
  }
  if (t === 'number') {
    const trimmed = text.trim();
    const n = Number(trimmed);
    if (trimmed === '' || Number.isNaN(n) || !Number.isFinite(n)) throw new Error(`${row.key} takes a number`);
    return inAllowedRange(row, n);
  }
  const choices = row.allowed?.kind === 'enum' ? row.allowed.values : null;
  if (choices && !isStructured(row) && !Array.isArray(row.value)) {
    const trimmed = text.trim();
    if (!choices.includes(trimmed)) {
      throw new Error(`${row.key} takes one of: ${choices.join(', ')}`);
    }
    return trimmed;
  }
  if (isTable(row)) {
    let parsed;
    try { parsed = JSON.parse(text.trim() || '{}'); } catch { throw new Error(`${row.key} takes a JSON object`); }
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error(`${row.key} takes a JSON object`);
    return parsed;
  }
  if (isTableList(row)) {
    let parsed;
    try { parsed = JSON.parse(text.trim() || '[]'); } catch { throw new Error(`${row.key} takes a JSON array of objects`); }
    if (!Array.isArray(parsed) || !parsed.every((entry) => isObject(entry) && !Array.isArray(entry))) throw new Error(`${row.key} takes a JSON array of objects`);
    return parsed;
  }
  if (Array.isArray(row.value) || t.includes('list') || t.includes('array')) return text.split('\n').map((s) => s.trim()).filter(Boolean);
  return text;
}
/// A number outside the key's declared range is refused before the save, with
/// the same range the server checks [ORB-22809].
function inAllowedRange(row, n) {
  const a = row.allowed;
  if (a?.kind !== 'range') return n;
  if ((a.min != null && n < a.min) || (a.max != null && n > a.max)) throw new Error(`${row.key} takes ${a.summary}`);
  return n;
}
// A bare TOML integer token must fit signed 64-bit. String() writes a whole number
// from 2^63 up to 1e21 as an integer token, so those use exponent (float) form.
function numberLiteral(n) {
  return Number.isInteger(n) && Math.abs(n) >= 2 ** 63 ? n.toExponential() : String(n);
}
export function cliCmd(row, scope, text) {
  const t = row.value_type || '';
  let v;
  if (t === 'bool' || t === 'integer') {
    v = text.trim() === '' ? "''" : shellWord(text.trim());
  } else if (t === 'number') {
    const trimmed = text.trim();
    const n = Number(trimmed);
    if (trimmed !== '' && Number.isFinite(n)) {
      v = numberLiteral(n);
    } else {
      v = trimmed === '' ? "''" : shellWord(trimmed);
    }
  } else if (isTable(row)) {
    let parsed = null;
    try { parsed = JSON.parse(text.trim() || '{}'); } catch { parsed = null; }
    if (parsed && typeof parsed === 'object') {
      try { v = shellQuote(tomlInline(parsed)); } catch { return '# TOML cannot represent null: remove null values to use this command'; }
    } else v = shellQuote(text);
  } else if (isTableList(row)) {
    let parsed = null;
    try { parsed = JSON.parse(text.trim() || '[]'); } catch { parsed = null; }
    if (Array.isArray(parsed)) {
      try { v = shellQuote(tomlInline(parsed)); } catch { return '# TOML cannot represent null: remove null values to use this command'; }
    } else v = shellQuote(text);
  } else if (Array.isArray(row.value) || t.includes('list') || t.includes('array')) {
    const entries = text.split('\n').map((s) => s.trim()).filter(Boolean);
    v = shellQuote(`[${entries.map((s) => JSON.stringify(s)).join(', ')}]`);
  } else if (row.allowed?.kind === 'enum' && !isStructured(row) && !Array.isArray(row.value)) {
    const trimmed = text.trim();
    if (/^(true|false|[+-]?(\d|inf|nan).*|[[{"'].*)$/i.test(trimmed) || trimmed === '') {
      v = shellQuote(JSON.stringify(trimmed));
    } else {
      v = shellWord(trimmed);
    }
  } else {
    // A free-form string keeps its whitespace: Save submits parseDraft's
    // untrimmed text, so the CLI literal must quote that exact value.
    v = shellQuote(JSON.stringify(text));
  }
  return `orbit config set ${row.key} ${v}${scope === 'global' ? ' --global' : ''}`;
}

function keyCount(n) {
  return `${n} key${n === 1 ? '' : 's'}`;
}

function hasValue(v) {
  if (v == null) return false;
  if (Array.isArray(v)) return v.length > 0;
  return typeof v !== 'object' || Object.keys(v).length > 0;
}
function tagsNode(entries) {
  return el('div', { class: 'settings-tags' }, entries.map(([text, note]) =>
    el('span', { class: 'badge mono', title: note == null ? text : `${text} ${note}` }, [text, note == null ? null : el('span', { class: 'settings-tag-note', text: note })])));
}
// A missing weight is one ticket. Weight 0 parks the crew: it stays listed
// and takes none of the draw (`CrewPoolEntry`).
function poolWeight(raw) {
  if (raw == null || String(raw).trim() === '') return 1;
  const weight = Number(raw);
  return Number.isFinite(weight) ? weight : 1;
}
// A crew pool as tags, each with its share of the draw (`name:weight`).
// Shares use every weight, so a parked entry is 0% and the others split
// only what still draws.
function poolTags(values) {
  const parts = values.map((v) => { const [name, w] = String(v).split(':'); return { name, weight: poolWeight(w) }; });
  const total = parts.reduce((sum, p) => sum + p.weight, 0);
  return tagsNode(parts.map((p) => {
    const share = total ? Math.round((p.weight / total) * 100) : 0;
    return [p.name, p.weight === 0 ? `${share}% (parked)` : `${share}%`];
  }));
}
// A table, or a list of tables, as key/value lines; past three the rest sit
// behind an "N more" toggle whose state survives a re-render.
const MAX_VALUE_LINES = 3;
function valueLines(row, lines) {
  const list = el('dl', { class: 'settings-kv' });
  const extra = [];
  lines.forEach(([name, text], index) => {
    const pair = [el('dt', { text: name }), el('dd', { text })];
    if (index >= MAX_VALUE_LINES) extra.push(...pair);
    list.append(...pair);
  });
  if (!extra.length) return [list];
  const more = el('button', { class: 'settings-more' }); more.type = 'button';
  const sync = () => {
    const open = expandedValues.has(row.key);
    for (const node of extra) node.hidden = !open;
    more.textContent = open ? 'Show fewer' : `${extra.length / 2} more`;
    more.setAttribute('aria-expanded', String(open));
  };
  more.addEventListener('click', () => {
    if (!expandedValues.delete(row.key)) expandedValues.add(row.key);
    sync();
  });
  sync();
  return [list, more];
}
function tableEntryLine(entry, index) {
  if (!isObject(entry) || Array.isArray(entry)) return [String(index + 1), display(entry)];
  const { name, ...rest } = entry;
  return typeof name === 'string' && name ? [name, display(rest)] : [String(index + 1), display(entry)];
}
function valueNodes(row) {
  const v = row.value;
  if (!hasValue(v)) return [el('div', { class: 'settings-value none', text: '—' })];
  if (Array.isArray(v)) {
    if (v.some(isObject)) return valueLines(row, v.map(tableEntryLine));
    return [/crews$|purpose_crews/.test(row.key) ? poolTags(v) : tagsNode(v.map((entry) => [String(entry)]))];
  }
  if (typeof v === 'object') return valueLines(row, tableLeaves(v).map(([path, leaf]) => [path, display(leaf)]));
  return [el('div', { class: 'settings-value', text: display(v) })];
}
/// The muted line under a value: what the key accepts [ORB-22809], then the
/// value it overrides or falls back to, when there is one.
function valueMeta(row) {
  const parts = [];
  const a = row.allowed;
  if (a?.summary) parts.push([el('span', { class: a.kind === 'format' ? '' : 'mono', title: `Accepts: ${a.summary}`, text: a.summary })]);
  const lower = (row.shadowed_by || []).map((s) => (s.note ? [s.note] : [`${settingsLayerLabel(s.layer)}: `, el('span', { class: 'mono', text: display(s.value) })]));
  if (!lower.length && row.state === 'set' && row.fallback && hasValue(row.fallback.value)) {
    lower.push([`${settingsLayerLabel(row.fallback.layer)}: `, el('span', { class: 'mono', text: display(row.fallback.value) })]);
  }
  parts.push(...lower);
  if (!parts.length) return null;
  const meta = el('div', { class: 'settings-meta' });
  parts.forEach((part, index) => { if (index) meta.append(' · '); meta.append(...part); });
  return meta;
}

function rowNode(row, context, opts = {}) {
  let action = null;
  if (context.canEdit && row.settable !== false && !opts.readOnly) {
    action = el('div', { class: 'settings-act' }, [settingsEditButton(() => openDrawer(row, context, opts.lockGlobal, opts.sectionTitle), `Edit ${row.key}`)]);
  } else if (row.read_only_reason || opts.readOnly) {
    action = settingsReadOnlyAction(row.read_only_reason);
  }
  const node = settingsRow({
    name: settingsKeyName(row.key, { label: row.label, description: row.description, prefix: opts.prefix }),
    value: [...valueNodes(row), valueMeta(row)],
    source: settingsRowSource(row),
    action,
    dim: opts.inactive,
  });
  node.dataset.key = row.key;
  node.id = rowDomId(row.key, opts.sectionTitle, opts.usedIds);
  return node;
}
// One id per rendered row. The section keeps a key that also appears in
// another section (Settings › Other lists some keys twice) from sharing an
// id, and a suffix separates keys whose slugs collide (`a.b` and `a-b`).
function rowDomId(key, sectionTitle, used) {
  const slug = String(key).replace(/[^a-z0-9]+/gi, '-').replace(/^-+|-+$/g, '') || 'key';
  const section = sectionSlug(sectionTitle || 'settings');
  let id = `key-${section}-${slug}`;
  if (used) {
    let n = 2;
    while (used.has(id)) {
      id = `key-${section}-${slug}-${n}`;
      n += 1;
    }
    used.add(id);
  }
  return id;
}
function sectionSlug(title) {
  return title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
}
// A section of key rows. `opts.lead` rows (not keys) come first and are not counted.
function keySection(title, rows, context, opts = {}) {
  const { section, body } = settingsSection(title, { ...opts, count: keyCount(rows.length) });
  const lead = opts.lead || [];
  if (!rows.length && !lead.length) body.appendChild(el('div', { class: 'settings-empty', text: 'No keys in this section.' }));
  body.append(...lead);
  const used = new Set();
  for (const r of rows) body.appendChild(rowNode(r, context, { ...opts, sectionTitle: title, usedIds: used }));
  return section;
}

function ownsDrawerMutation(session) {
  return drawer === session && session.visit.isCurrent();
}

async function submitDrawerMutation(session, context, request) {
  if (!ownsDrawerMutation(session) || session.pending) return;
  session.pending = true; session.error = null; context.rerender?.();
  try {
    await request(session.visit);
    if (!ownsDrawerMutation(session)) return;
    drawer = null;
    await context.reload();
    // The reload can outlive its visit: a switch to another workspace while it
    // is in flight superseded it, and that visit's editor must keep focus. Only
    // an uninterrupted save returns focus to its Edit control.
    if (session.visit.isCurrent() && !drawer) focusRowEdit(session.row.key, session.sectionTitle);
  } catch (error) {
    if (!ownsDrawerMutation(session)) return;
    session.pending = false; session.error = error.message || String(error);
    context.rerender?.();
    // The rerender rebuilt the pressed Save or Remove control. Keep focus in the
    // drawer, on the field the error is about, so Escape still reaches it.
    document.querySelector('.config-drawer .settings-input')?.focus();
  }
}

// Whether this workspace has no config policy file yet: none at all, or one
// holding only plugin toggles. `layers.execution_not_inherited` is set exactly
// when a workspace file separate from the global one sets policy. The first
// workspace write then needs an explicit init choice, as `orbit config set`
// asks for --seed-from-global or --fresh, and is refused without one.
export function workspacePolicyMissing(payload) {
  if (payload?.workspace_file_exists === false) return true;
  const layers = payload?.layers;
  return Boolean(layers?.workspace?.path) && layers.workspace.path !== layers.global?.path && layers.execution_not_inherited === false;
}

function drawerNode(payload, context) {
  if (!drawer) return null;
  if (!drawer.visit.isCurrent()) { drawer = null; return null; }
  const session = drawer;
  const row = byKey(payload).get(session.row.key) || session.row;
  const panel = el('aside', { class: 'config-drawer dialog-surface dialog-drawer' });
  panel.setAttribute('role', 'dialog'); panel.setAttribute('aria-label', `Edit ${row.key}`);
  panel.addEventListener('keydown', event => {
    if (event.key === 'Escape' && !session.pending) { event.preventDefault(); closeDrawer(session, context); }
  });
  panel.appendChild(el('header', { class: 'dialog-header' }, [el('h3', { text: row.label || row.key }), escCloseButton('Close editor (Esc)', { onClick: () => closeDrawer(session, context) })]));
  const body = el('div', { class: 'dialog-body' });
  panel.appendChild(body);
  body.appendChild(el('div', { class: 'config-key mono', text: row.key }));
  if (row.description) body.appendChild(el('p', { text: row.description }));
  if (row.detail) body.appendChild(el('p', { class: 'drawer-detail', text: row.detail }));
  if (row.allowed?.summary) body.appendChild(el('div', { class: 'config-note drawer-allowed', text: `Accepts: ${row.allowed.summary}` }));
  const chain = el('div', { class: 'drawer-chain' });
  chain.appendChild(el('div', { text: `built-in: ${display(row.built_in?.value)}` }));
  for (const s of row.shadowed_by || []) chain.appendChild(el('div', { text: `${s.layer}: ${display(s.value)}` }));
  chain.appendChild(el('div', { text: `effective (${row.source?.layer || row.state}): ${display(row.value)}` }));
  if (row.fallback) chain.appendChild(el('div', { text: `if removed → ${row.fallback.layer}: ${display(row.fallback.value)}` }));
  body.appendChild(chain);
  for (const scope of ['workspace', 'global']) {
    const label = el('label', { class: 'drawer-scope' });
    const input = document.createElement('input'); input.type = 'radio'; input.name = 'drawer-scope'; input.value = scope;
    input.checked = session.scope === scope; input.disabled = session.lockGlobal && scope !== 'global';
    input.addEventListener('change', () => { session.scope = scope; context.rerender?.(); });
    label.append(input, document.createTextNode(scope === 'workspace' ? ' This workspace (.orbit/config.toml)' : ' Every workspace on this machine (~/.orbit/config.toml)'));
    body.appendChild(label);
  }
  if (session.lockGlobal) body.appendChild(el('div', { class: 'config-note', text: 'This key can only be written to the global file.' }));
  let field;
  if ((row.options || []).length) {
    const options = row.options;
    // An unset choice has an empty draft, but a select shows its first option.
    // Copy that option into the draft so Save and the CLI hint match the display.
    if (session.text === '') session.text = options[0];
    field = el('select', { class: 'settings-input' });
    for (const o of options) { const opt = el('option', { text: o }); opt.value = o; if (o === session.text) opt.selected = true; field.appendChild(opt); }
    field.value = session.text;
    field.addEventListener('change', () => { session.text = field.value; const c = panel.querySelector('.drawer-cli'); if (c) c.textContent = cliCmd(row, session.scope, session.text); });
  } else if (row.allowed?.kind === 'range' && (row.value_type === 'integer' || row.value_type === 'number')) {
    field = el('input', { class: 'settings-input mono', type: 'number' });
    if (row.allowed.min != null) field.min = String(row.allowed.min);
    if (row.allowed.max != null) field.max = String(row.allowed.max);
    field.step = row.allowed.integer ? '1' : 'any';
    field.value = session.text;
    field.addEventListener('input', () => { session.text = field.value; const c = panel.querySelector('.drawer-cli'); if (c) c.textContent = cliCmd(row, session.scope, session.text); });
  } else {
    field = el('textarea', { class: 'settings-input' }); field.value = session.text; field.rows = isTableList(row) ? 8 : Array.isArray(row.value) ? 4 : 2;
    field.addEventListener('input', () => { session.text = field.value; const c = panel.querySelector('.drawer-cli'); if (c) c.textContent = cliCmd(row, session.scope, session.text); });
  }
  body.appendChild(field);
  body.appendChild(el('div', { class: 'drawer-cli mono', text: cliCmd(row, session.scope, session.text) }));
  if (session.error) body.appendChild(el('div', { class: 'config-warning', text: session.error }));
  const actions = el('div', { class: 'dialog-actions' });
  const saveWith = (init) => () => submitDrawerMutation(session, context, (visit) =>
    requestJson(`/api/config/keys/${encodeURIComponent(row.key)}`, 'PUT', { value: parseDraft(row, session.text), scope: session.scope, ...(init ? { init } : {}) }, visit));
  // A first workspace write offers only the explicit choices, so no click
  // creates the policy file implicitly; closing the drawer leaves it uncreated.
  const choosesInit = session.scope === 'workspace' && workspacePolicyMissing(payload);
  if (choosesInit) body.appendChild(el('div', { class: 'config-note drawer-init-note', text: 'This workspace has no config policy file yet, and saving creates one. Seed from global copies the global policy (orbit config set --seed-from-global); Start empty uses built-in security defaults (--fresh).' }));
  const saves = choosesInit
    ? [['Seed from global', 'seed-from-global'], ['Start empty', 'fresh']]
    : [['Save', null]];
  for (const [label, init] of saves) {
    const save = el('button', { class: 'settings-btn primary', text: session.pending ? 'Saving…' : label }); save.type = 'button'; save.disabled = session.pending;
    if (init) save.dataset.init = init;
    save.addEventListener('click', saveWith(init));
    actions.appendChild(save);
  }
  // A global-only drawer (lockGlobal) has no workspace value to clear, so its
  // Remove clears the global value; the label names the scope it removes.
  const removeScope = session.lockGlobal ? 'global' : 'workspace';
  const remove = el('button', { class: 'settings-btn ghost', text: `Remove ${removeScope} value` }); remove.type = 'button'; remove.disabled = session.pending;
  remove.addEventListener('click', () => submitDrawerMutation(session, context, (visit) =>
    requestJson(`/api/config/keys/${encodeURIComponent(row.key)}?scope=${removeScope}`, 'DELETE', undefined, visit)));
  // The header's esc control is the drawer's one dismiss control [STD-07 §R44].
  actions.append(remove); panel.appendChild(actions);
  return panel;
}
// The bulk-removal outcome. A clean removal expires on its own; one that names
// failures stays until dismissed, since the operator acts on it.
function noticeNode(context) {
  if (!notice) return null;
  const shown = notice;
  const dismiss = el('button', { class: 'settings-btn ghost', text: 'Dismiss', "aria-description": 'Dismiss this notice' }); dismiss.type = 'button';
  dismiss.addEventListener('click', () => { if (notice === shown) { notice = null; context.rerender?.(); } });
  return el('div', { class: 'settings-callout', role: 'status' }, [el('span', { text: shown.text }), ' ', dismiss]);
}
function showNotice(next, context) {
  notice = next;
  if (next.failed) return;
  setTimeout(() => { if (notice === next) { notice = null; context.rerender?.(); } }, NOTICE_MS);
}
function finish(body, payload, context, nodes) {
  const wrap = el('div', { class: 'settings-stack' });
  const note = noticeNode(context); if (note) wrap.appendChild(note);
  for (const n of nodes) if (n) wrap.appendChild(n);
  const d = drawerNode(payload, context); if (d) wrap.appendChild(d);
  body.appendChild(wrap);
}

// The collapsed sections in force. A search keeps its own set: its matches open
// for discoverability, explicit collapses made during it stay in that set, and
// the saved layout is untouched until the search clears.
function workflowCollapsedSet() {
  return workflowQuery.trim() ? workflowSearchCollapsed : workflowCollapsed;
}

export function renderWorkflow(body, payload, context) {
  const rows = flatten(payload).filter((r) => topicOf(r.key) === 'workflow' && !r.key.startsWith('workflow.resource_throttle.'));
  const used = new Set();
  const groups = [];
  for (const [title, pred] of WORKFLOW_SECTIONS) {
    const sec = rows.filter((r) => pred(r.key, payload)); sec.forEach((r) => used.add(r.key));
    groups.push({ title, id: `workflow-section-${sectionSlug(title)}`, rows: sec });
  }
  const rest = rows.filter((r) => !used.has(r.key));
  if (rest.length) groups.push({ title: 'Other', id: 'workflow-section-other', rows: rest });
  const query = workflowQuery.trim().toLowerCase();
  const matches = (r) => !query || `${r.key} ${r.label || ''} ${display(r.value)}`.toLowerCase().includes(query);
  const shown = groups
    .map((g) => ({ ...g, rows: query ? g.rows.filter(matches) : g.rows }))
    .filter((g) => !query || g.rows.length);
  const nodes = [];
  // Direct navigation from the bar: jump to any section (expanding it),
  // filter to matching keys, or collapse sections to a bounded subset. Every
  // control is a native button/input so the whole set is keyboard reachable.
  const jumps = el('nav', { class: 'settings-jump' });
  jumps.setAttribute('aria-label', 'Workflow sections');
  for (const g of shown) {
    const jump = el('button', { class: 'settings-jump-link', text: g.title });
    jump.type = 'button';
    jump.dataset.workflowTarget = g.id;
    jump.addEventListener('click', () => {
      workflowCollapsedSet().delete(g.id);
      context.rerender?.();
      const target = document.getElementById(g.id);
      if (!target) return;
      syncSettingsBarHeight(document.getElementById('config-controls'));
      target.scrollIntoView({ block: 'start' });
      target.querySelector('.settings-section-toggle')?.focus({ preventScroll: true });
    });
    jumps.appendChild(jump);
  }
  const search = settingsSearchField({
    cls: 'workflow-search', placeholder: 'Search settings', label: 'Search workflow keys', value: workflowQuery,
    onInput: (input) => {
      workflowQuery = input.value;
      if (!workflowQuery.trim()) workflowSearchCollapsed = new Set();
      context.rerender?.();
      const rebuilt = document.querySelector('input.workflow-search');
      if (rebuilt) { rebuilt.focus({ preventScroll: true }); rebuilt.setSelectionRange(rebuilt.value.length, rebuilt.value.length); }
    },
  });
  // Expand all and Collapse all act on the sections the view shows, so a search
  // collapses only its matches. Each keeps keyboard focus on its own button.
  // The text hides in a narrow pane (settings.css), so the label is also the name.
  const quiet = (cls, icon, label, onClick) => {
    const button = el('button', { class: `settings-quiet ${cls}`, 'aria-label': label }, [settingsIcon(icon), el('span', { class: 'settings-quiet-label', text: label })]);
    button.type = 'button';
    button.addEventListener('click', () => {
      onClick(); context.rerender?.();
      document.querySelector(`button.${cls}`)?.focus({ preventScroll: true });
    });
    return button;
  };
  const expandAll = quiet('workflow-expand-all', 'expand', 'Expand all', () => workflowCollapsedSet().clear());
  const collapseAll = quiet('workflow-collapse-all', 'collapse', 'Collapse all', () => {
    const collapsed = workflowCollapsedSet();
    collapsed.clear(); for (const g of shown) collapsed.add(g.id);
  });
  if (query && !shown.length) {
    const empty = el('section', { class: 'settings-section' });
    empty.appendChild(el('div', { class: 'settings-empty', text: 'No workflow keys match this search. Clear the search to show every key.' }));
    nodes.push(empty);
  }
  // Shipping opens with what the workspace registry recorded: it is not a
  // config key, so it is a read-only row and stays out of the key count.
  const binding = payload.workspace_binding;
  const bindingRow = () => settingsRow({
    name: settingsLabelName('Workspace registry', 'Recorded when the workspace was registered.'),
    value: [tagsNode([['base_branch', binding.base_branch || '–'], ['ship_mode', binding.ship_mode || '–']])],
    source: settingsSource('registry'),
    action: settingsReadOnlyAction(null),
  });
  for (const g of shown) {
    nodes.push(keySection(g.title, g.rows, context, {
      id: g.id,
      lead: binding && !query && g.title === 'Shipping' ? [bindingRow()] : [],
      collapsible: true,
      collapsed: workflowCollapsedSet().has(g.id),
      onToggle: () => {
        const collapsed = workflowCollapsedSet();
        if (collapsed.has(g.id)) collapsed.delete(g.id); else collapsed.add(g.id);
        context.rerender?.();
        document.getElementById(g.id)?.querySelector('.settings-section-toggle')?.focus({ preventScroll: true });
      },
    }));
  }
  finish(body, payload, context, nodes);
  return { lead: [jumps, search], actions: [expandAll, collapseAll], sticky: true };
}
export function renderExecution(body, payload, context) {
  const rows = flatten(payload).filter((r) => r.key.startsWith('execution.'));
  const nodes = [];
  if (payload.layers?.execution_not_inherited) nodes.push(el('div', { class: 'settings-callout warn', text: 'execution.* keys in the global file are not inherited while this workspace sets execution policy; the workspace values below are in force.' }));
  nodes.push(keySection('Execution', rows, context));
  const paths = settingsSection('Paths');
  for (const p of payload.paths || []) {
    paths.body.appendChild(settingsRow({ name: settingsLabelName(p.label || p.name || ''), value: [el('div', { class: 'settings-value', text: p.path || p.value || '' })], wide: true }));
  }
  nodes.push(paths.section);
  finish(body, payload, context, nodes);
  return {};
}
// The server's one-line account of a review layer runs long, so it is clamped
// to one line; "Show full" appears only while the line is actually cut.
let clampObserver = null;
function syncClamp(clamp) {
  const more = clamp.nextElementSibling;
  if (more && !clamp.classList.contains('full')) more.hidden = clamp.scrollWidth <= clamp.clientWidth;
}
function clampedLine(id, text) {
  const clamp = el('span', { class: 'settings-clamp', text });
  const more = el('button', { class: 'settings-more' }); more.type = 'button';
  const sync = () => {
    const open = expandedValues.has(id);
    clamp.classList.toggle('full', open);
    more.textContent = open ? 'Show less' : 'Show full';
    more.setAttribute('aria-expanded', String(open));
    if (open) more.hidden = false; else syncClamp(clamp);
  };
  more.addEventListener('click', () => {
    if (!expandedValues.delete(id)) expandedValues.add(id);
    sync();
  });
  sync();
  if (typeof ResizeObserver === 'function') {
    clampObserver ||= new ResizeObserver((entries) => { for (const entry of entries) syncClamp(entry.target); });
    clampObserver.observe(clamp);
    // The observer keeps a target until unobserve. A Settings poll replaces
    // this line, and resetPanel drops the body; both run teardownNode.
    clamp.teardown = () => clampObserver.unobserve(clamp);
  }
  return el('div', { class: 'settings-meta settings-line' }, [clamp, more]);
}
export function renderReview(body, payload, context) {
  const map = byKey(payload); const nodes = [];
  const review = payload.review || {};
  const layers = settingsSection('Review layers');
  for (const [id, label] of [['before_pr','Before PR'],['before_landing','Before landing'],['after_landing','After landing']]) {
    const step = review[id] || {};
    layers.body.appendChild(settingsRow({
      name: settingsLabelName(label),
      value: [
        el('div', { class: 'settings-label' }, [el('span', { class: `settings-dot${step.enabled ? ' on' : ''}` }), step.enabled ? 'On' : 'Off']),
        step.line ? clampedLine(`review-layer:${id}`, step.line) : null,
      ],
      wide: true,
    }));
  }
  if (review.healthy === false) layers.body.appendChild(el('div', { class: 'settings-note warn', text: review.error || 'Review diagnostics unavailable.' }));
  nodes.push(layers.section);
  const timing = map.get('review.timing'); const timingOff = !timing || timing.value === 'off' || timing.value == null;
  nodes.push(keySection('Timing', [timing].filter(Boolean), context));
  nodes.push(keySection('Reviewer pool', ['workflow.purpose_crews.review','workflow.purpose_crews.pre_landing_review'].map((k) => map.get(k)).filter(Boolean), context, { prefix: 'workflow.purpose_crews.' }));
  // The inactive state is said once, on the header; its rows only dim.
  nodes.push(keySection('Before landing', ['review.minutes','review.before_landing_hosts','review.baseline_commands'].map((k) => map.get(k)).filter(Boolean), context, { inactive: timingOff, state: timingOff ? { text: 'Inactive: review.timing is off' } : null }));
  const rest = flatten(payload).filter((r) => topicOf(r.key) === 'review' && !['review.timing','review.minutes','review.before_landing_hosts','review.baseline_commands','workflow.purpose_crews.review','workflow.purpose_crews.pre_landing_review'].includes(r.key));
  if (rest.length) nodes.push(keySection('Other review keys', rest, context));
  finish(body, payload, context, nodes);
  const diag = el('a', { text: 'Review diagnostics' }); diag.href = '#diagnostics';
  return { lead: [el('div', { class: 'settings-bar-text' }, [diag])] };
}
export function renderMachine(body, payload, context, throttleNode) {
  // payload is the global file view merged with effective+host by config.js
  const eff = payload.effective || payload; const map = byKey(eff);
  const nodes = [];
  const identity = ['machine.id','machine.name','machine.task_prefix'].map((k) => map.get(k)).filter(Boolean);
  const idSec = settingsSection('Machine identity', { count: keyCount(identity.length) });
  const identityUsed = new Set();
  for (const r of identity) idSec.body.appendChild(rowNode(r, context, { lockGlobal: true, readOnly: r.key !== 'machine.name', sectionTitle: 'Machine identity', usedIds: identityUsed }));
  nodes.push(idSec.section);
  nodes.push(keySection('Auto-concurrency', flatten(eff).filter((r) => r.key.startsWith('machine.auto_concurrency.')), context, { lockGlobal: true }));
  nodes.push(keySection('Machine', flatten(eff).filter((r) => r.key.startsWith('machine.') && !r.key.startsWith('machine.auto_concurrency.') && !identity.includes(r)), context, { lockGlobal: true }));
  if (throttleNode) nodes.push(throttleNode);
  // The throttle panel shows only the enabled flag and the per-resource
  // marks; any other throttle key (today cpu_source) still belongs on this
  // page, so no machine-topic key is left without a page.
  const throttleShown = new Set(['workflow.resource_throttle.enabled']);
  for (const resource of ['cpu', 'memory', 'disk', 'io']) {
    throttleShown.add(`workflow.resource_throttle.${resource}_high_percent`);
    throttleShown.add(`workflow.resource_throttle.${resource}_resume_percent`);
  }
  const throttleRest = flatten(eff).filter((r) => r.key.startsWith('workflow.resource_throttle.') && !throttleShown.has(r.key));
  if (throttleRest.length) nodes.push(keySection('Other throttle keys', throttleRest, context, { lockGlobal: true, prefix: 'workflow.resource_throttle.' }));
  nodes.push(keySection('Tasks', ['tasks.id_start'].map((k) => map.get(k)).filter(Boolean), context, { lockGlobal: true }));
  finish(body, eff, context, nodes);
  return { lead: [el('div', { class: 'settings-bar-text' }, ['Edits on this page write the global file ', el('span', { class: 'mono', text: '~/.orbit/config.toml' })])] };
}
export async function loadRaw(scope, context) {
  const seq = ++rawSeq;
  rawScope = scope; rawText = null;
  context.rerender?.();
  let text;
  try { const f = await fetchJson(`/api/config/file?scope=${scope}`); text = f.text ?? '(raw TOML not reported by this server)'; }
  catch (e) { text = e.message || String(e); }
  if (seq !== rawSeq) return; // a later click owns the block; drop this response, success or error
  rawText = text;
  context.rerender?.();
}
// A workspace value is only bulk-removable when its admitted removal
// fallback preserves the effective value. An intentional override whose
// value equals the built-in default but differs from the global fallback
// must stay; an unknown (null) fallback is never claimed safe. This mirrors
// the config layering contract: ordinary keys fall back to the global value,
// replace-only security keys fall back to built-in while the workspace file
// exists, and a missing prediction stays null.
function valuesEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== typeof b || a === null || b === null) return false;
  if (typeof a !== 'object') return Object.is(a, b);
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) {
    if (a.length !== b.length) return false;
    return a.every((value, index) => valuesEqual(value, b[index]));
  }
  const aKeys = Object.keys(a).sort();
  const bKeys = Object.keys(b).sort();
  if (aKeys.length !== bKeys.length || aKeys.some((key, index) => key !== bKeys[index])) return false;
  return aKeys.every((key) => valuesEqual(a[key], b[key]));
}
function isFallbackPreserving(row) {
  if (row?.source?.layer !== 'workspace') return false;
  if (row.fallback == null) return false;
  return valuesEqual(row.value, row.fallback.value);
}
// Topics that have their own Settings page. Other shows every key
// topicOf() files elsewhere, so a key added later cannot end up on no page.
const TOPIC_PAGES = new Set(['workflow', 'execution', 'review', 'machine', 'crews']);
export function renderOther(body, payload, context) {
  const rows = flatten(payload).filter((r) => !TOPIC_PAGES.has(topicOf(r.key)));
  const nodes = [keySection('Other', rows, context)];
  // The cleanup section moved here whole from All keys: it lists every
  // workspace value whose removal keeps the effective value, on any topic.
  const restates = flatten(payload).filter(isFallbackPreserving);
  if (restates.length) {
    const sec = keySection('Keys that only repeat the global or built-in value', restates, context);
    const bulk = el('button', { class: 'settings-btn', text: `Remove all ${restates.length} workspace values` }); bulk.type = 'button';
    bulk.disabled = !context.canEdit || bulkRemoval !== null;
    bulk.addEventListener('click', async () => {
      if (!context.canEdit || bulkRemoval) return;
      const operation = { visit: captureWorkspaceVisit() };
      bulkRemoval = operation;
      const ownsRemoval = () => bulkRemoval === operation && operation.visit.isCurrent();
      notice = null;
      context.rerender?.();
      let ok = 0; const failures = [];
      try {
        for (const r of restates) {
          if (!ownsRemoval()) return;
          try {
            await requestJson(`/api/config/keys/${encodeURIComponent(r.key)}?scope=workspace`, 'DELETE', undefined, operation.visit);
            if (!ownsRemoval()) return;
            ok++;
          } catch (error) {
            if (!ownsRemoval()) return;
            failures.push(`${r.key}: ${error.message}`);
          }
        }
        showNotice({ text: `Removed ${ok} workspace value(s).${failures.length ? ' Failed: ' + failures.join('; ') : ''}`, failed: failures.length > 0 }, context);
        await context.reload();
      } finally {
        if (ownsRemoval()) { bulkRemoval = null; context.rerender?.(); }
      }
    });
    sec.appendChild(el('div', { class: 'settings-actions' }, [bulk])); nodes.push(sec);
  }
  const raw = settingsSection('Raw TOML', { id: 'config-raw-toml' });
  const rb = el('div', { class: 'settings-actions' });
  for (const scope of ['workspace', 'global']) {
    const b = el('button', { class: 'settings-btn ghost', text: `Show ${scope} file` }); b.type = 'button';
    b.addEventListener('click', () => loadRaw(scope, context)); rb.appendChild(b);
  }
  if (rawScope) {
    const where = rawScope === 'global' ? 'every workspace on this machine (~/.orbit/config.toml)' : 'this workspace (.orbit/config.toml)';
    rb.appendChild(el('div', { class: 'settings-raw-label', text: `Showing ${where}` }));
    rb.appendChild(el('pre', { class: 'settings-raw mono', text: rawText == null ? 'Loading…' : (rawText || '(empty file)') }));
  }
  raw.body.appendChild(rb); nodes.push(raw.section);
  finish(body, payload, context, nodes);
  // A deep link (?key=…) that lands here names a row on this short page:
  // bring it into view once per visit, preserving operator scrolling during
  // raw-file reads, reloads and drawer rerenders. Other keys route elsewhere.
  const linked = new URLSearchParams((window.location.hash.split('?')[1] || '')).get('key');
  if (linked && window.location.hash !== landedOtherHash && rows.some((r) => r.key === linked)) {
    // The Other list is rendered first, so a key that is also in the repeat
    // section scrolls to that first copy. Ids differ per row.
    const target = [...document.querySelectorAll('#config-body [data-key]')].find((node) => node.dataset.key === linked);
    if (target && typeof target.scrollIntoView === 'function') {
      target.scrollIntoView({ block: 'start' });
      landedOtherHash = window.location.hash;
    }
  }
  return {};
}
