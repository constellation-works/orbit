// Settings topic pages: Workflow, Execution, Review, Machine, All keys.
// Rows come from /api/config/effective; the shared drawer writes via
// PUT/DELETE /api/config/keys/:key and re-reads via context.reload.
import { captureWorkspaceVisit, el, fetchJson, requestJson } from './common.js';

let drawer = null; // {row, scope, text, error, pending, visit, lockGlobal}
let bulkRemoval = null;
let layerFilter = 'all';
let topicFilter = 'all';
let allKeysQuery = '';
let allKeysPage = 0;
let workflowQuery = '';
let workflowCollapsed = new Set(); // the saved layout, in force when no search is active
let workflowSearchCollapsed = new Set(); // sections collapsed while a search is active
// The deep key each source (the ?key= link, the open drawer) last landed the
// All keys pager on, so later renders leave the pager where the user moved it.
let allKeysLanded = { link: null, drawer: null };
const ALL_KEYS_PAGE_SIZE = 25;
let rawScope = null;
let rawText = null; // null while the latest read is in flight
let rawSeq = 0; // numbers each read so only the latest click can apply its response
let notice = null; // {text, failed}: the last bulk removal's outcome
const NOTICE_MS = 10000;

export function resetTopics() { drawer = null; bulkRemoval = null; layerFilter = 'all'; topicFilter = 'all'; allKeysQuery = ''; allKeysPage = 0; workflowQuery = ''; workflowCollapsed = new Set(); workflowSearchCollapsed = new Set(); allKeysLanded = { link: null, drawer: null }; rawScope = null; rawText = null; notice = null; }

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
function tomlInline(v) {
  if (Array.isArray(v)) return `[${v.map(tomlInline).join(', ')}]`;
  if (v && typeof v === 'object') {
    const key = (k) => (/^[A-Za-z0-9_-]+$/.test(k) ? k : JSON.stringify(k));
    return `{${Object.entries(v).map(([k, value]) => `${key(k)} = ${tomlInline(value)}`).join(', ')}}`;
  }
  return typeof v === 'string' ? JSON.stringify(v) : String(v);
}
function diffText(row) {
  const sh = row.shadowed_by || [];
  if (sh.length) return sh.map((s) => s.note || `${s.layer}: ${display(s.value)}`).join('; ');
  if (row.fallback) return `${row.fallback.layer}: ${display(row.fallback.value)}`;
  if (row.built_in && row.source?.layer !== 'built-in') return `built-in: ${display(row.built_in.value)}`;
  return '';
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

function openDrawer(row, context, lockGlobal) {
  if (!context.canEdit || row.settable === false) return;
  drawer = { row, scope: (lockGlobal || row.global_only) ? 'global' : (row.source?.layer === 'global' ? 'global' : 'workspace'),
    text: isStructured(row) ? JSON.stringify(row.value ?? (isTable(row) ? {} : []), null, 2) : Array.isArray(row.value) ? row.value.join('\n') : display(row.value) === '–' ? '' : String(row.value ?? ''),
    error: null, pending: false, visit: captureWorkspaceVisit(), lockGlobal: !!(lockGlobal || row.global_only) };
  context.rerender?.();
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
    v = shellQuote(parsed && typeof parsed === 'object' ? tomlInline(parsed) : text);
  } else if (isTableList(row)) {
    let parsed = null;
    try { parsed = JSON.parse(text.trim() || '[]'); } catch { parsed = null; }
    v = shellQuote(Array.isArray(parsed) ? tomlInline(parsed) : text);
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

function rowNode(row, context, opts = {}) {
  const layer = row.state === 'set' ? (row.source?.layer || 'set') : row.state;
  const node = el('div', { class: `config-row topic-row${opts.inactive ? ' inactive' : ''}` });
  node.dataset.key = row.key;
  node.id = `key-${row.key.replace(/[^a-z0-9]+/gi, '-')}`;
  const pool = Array.isArray(row.value) && /crews$|purpose_crews/.test(row.key) ? weightBar(row.value) : null;
  node.append(
    el('span', { class: 'topic-label' }, [
      el('span', { text: row.label || row.key }),
      row.description ? el('span', { class: 'topic-description', text: row.description }) : null,
      el('span', { class: 'config-key mono', text: row.key }),
    ]),
    el('span', { class: 'topic-value-cell' }, [el('span', { class: 'config-value mono', text: display(row.value) }), allowedNode(row)]),
    el('span', { class: `config-source ${layer}`, text: layer }),
    el('span', { class: 'topic-diff', text: diffText(row) }),
  );
  if (pool) node.appendChild(pool);
  if (opts.inactive) node.appendChild(el('span', { class: 'config-source topic-inactive', text: 'inactive — review.timing is off' }));
  if (context.canEdit && row.settable !== false && !opts.readOnly) {
    const b = el('button', { class: 'config-action', text: 'Edit' }); b.type = 'button';
    b.addEventListener('click', () => openDrawer(row, context, opts.lockGlobal));
    node.appendChild(b);
  } else if (row.read_only_reason || opts.readOnly) {
    node.appendChild(el('span', { class: 'topic-diff topic-readonly', text: row.read_only_reason || 'read-only' }));
  }
  return node;
}
/// The values a key accepts, beside its value [ORB-22809]: a short enum as
/// chips with the current one marked, anything else as the server's one-line
/// summary (`1–1440 minutes`, a format note).
const MAX_ALLOWED_CHIPS = 6;
function allowedNode(row) {
  const a = row.allowed;
  if (!a?.summary) return null;
  const title = `Accepts: ${a.summary}`;
  if (a.kind === 'enum' && a.values.length <= MAX_ALLOWED_CHIPS) {
    const node = el('span', { class: 'topic-allowed', title, 'aria-label': title });
    for (const v of a.values) node.appendChild(el('span', { class: `topic-allowed-chip mono${v === row.value ? ' current' : ''}`, text: v }));
    return node;
  }
  return el('span', { class: `topic-allowed${a.kind === 'format' ? '' : ' mono'}`, title, text: a.summary });
}
function weightBar(values) {
  const wrap = el('span', { class: 'topic-pool' });
  let total = 0; const parts = values.map((v) => { const [n, w] = String(v).split(':'); const weight = Number(w) || 1; total += weight; return { n, weight }; });
  for (const p of parts) { const s = el('span', { class: 'topic-pool-seg', text: `${p.n} ${total ? Math.round((p.weight / total) * 100) : 0}%` }); s.style.flexGrow = String(p.weight); wrap.appendChild(s); }
  return wrap;
}
function sectionSlug(title) {
  return title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
}
// The Workflow navigation wraps to a different height per width, so the pane
// reserves its measured height as scroll padding (see settings.css).
function syncWorkflowNavOffset(nav) {
  const pane = nav?.isConnected ? nav.closest('.tab-pane') : null;
  if (pane) pane.style.setProperty('--workflow-nav-h', `${nav.offsetHeight}px`);
}
function section(title, rows, context, opts = {}) {
  const sec = el('section', { class: 'panel config-section' });
  if (opts.id) sec.id = opts.id;
  const bodyId = opts.id ? `${opts.id}-body` : null;
  const collapsed = Boolean(opts.collapsible && opts.collapsed);
  const headerRight = [el('span', { class: 'count', text: `${rows.length} keys` })];
  if (opts.collapsible) {
    const toggle = el('button', { class: 'config-action ghost workflow-section-toggle', text: collapsed ? 'Expand' : 'Collapse' });
    toggle.type = 'button';
    toggle.setAttribute('aria-expanded', String(!collapsed));
    if (bodyId) toggle.setAttribute('aria-controls', bodyId);
    toggle.setAttribute('aria-label', `${collapsed ? 'Expand' : 'Collapse'} ${title}`);
    toggle.addEventListener('click', () => opts.onToggle?.());
    headerRight.push(toggle);
  }
  sec.appendChild(el('header', {}, [el('span', { class: 'config-section-title', text: title }), el('span', { class: 'config-header-right' }, headerRight)]));
  const body = el('div', { class: 'config-section-body' });
  if (bodyId) body.id = bodyId;
  if (collapsed) body.hidden = true;
  if (!rows.length) body.appendChild(el('div', { class: 'config-empty', text: 'No keys in this section.' }));
  for (const r of rows) body.appendChild(rowNode(r, context, opts));
  sec.appendChild(body); return sec;
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
  } catch (error) {
    if (!ownsDrawerMutation(session)) return;
    session.pending = false; session.error = error.message || String(error);
    context.rerender?.();
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
  const panel = el('aside', { class: 'config-drawer' });
  panel.setAttribute('role', 'dialog'); panel.setAttribute('aria-label', `Edit ${row.key}`);
  panel.appendChild(el('h3', { text: row.label || row.key }));
  panel.appendChild(el('div', { class: 'config-key mono', text: row.key }));
  if (row.description) panel.appendChild(el('p', { text: row.description }));
  if (row.detail) panel.appendChild(el('p', { class: 'drawer-detail', text: row.detail }));
  if (row.allowed?.summary) panel.appendChild(el('div', { class: 'config-note drawer-allowed', text: `Accepts: ${row.allowed.summary}` }));
  const chain = el('div', { class: 'drawer-chain' });
  chain.appendChild(el('div', { text: `built-in: ${display(row.built_in?.value)}` }));
  for (const s of row.shadowed_by || []) chain.appendChild(el('div', { text: `${s.layer}: ${display(s.value)}` }));
  chain.appendChild(el('div', { text: `effective (${row.source?.layer || row.state}): ${display(row.value)}` }));
  if (row.fallback) chain.appendChild(el('div', { text: `if removed → ${row.fallback.layer}: ${display(row.fallback.value)}` }));
  panel.appendChild(chain);
  for (const scope of ['workspace', 'global']) {
    const label = el('label', { class: 'drawer-scope' });
    const input = document.createElement('input'); input.type = 'radio'; input.name = 'drawer-scope'; input.value = scope;
    input.checked = session.scope === scope; input.disabled = session.lockGlobal && scope !== 'global';
    input.addEventListener('change', () => { session.scope = scope; context.rerender?.(); });
    label.append(input, document.createTextNode(scope === 'workspace' ? ' This workspace (.orbit/config.toml)' : ' Every workspace on this machine (~/.orbit/config.toml)'));
    panel.appendChild(label);
  }
  if (session.lockGlobal) panel.appendChild(el('div', { class: 'config-note', text: 'This key can only be written to the global file.' }));
  let field;
  if ((row.options || []).length) {
    const options = row.options;
    // An unset choice has an empty draft, but a select shows its first option.
    // Copy that option into the draft so Save and the CLI hint match the display.
    if (session.text === '') session.text = options[0];
    field = el('select', { class: 'config-input' });
    for (const o of options) { const opt = el('option', { text: o }); opt.value = o; if (o === session.text) opt.selected = true; field.appendChild(opt); }
    field.value = session.text;
    field.addEventListener('change', () => { session.text = field.value; const c = panel.querySelector('.drawer-cli'); if (c) c.textContent = cliCmd(row, session.scope, session.text); });
  } else if (row.allowed?.kind === 'range' && (row.value_type === 'integer' || row.value_type === 'number')) {
    field = el('input', { class: 'config-input mono', type: 'number' });
    if (row.allowed.min != null) field.min = String(row.allowed.min);
    if (row.allowed.max != null) field.max = String(row.allowed.max);
    field.step = row.allowed.integer ? '1' : 'any';
    field.value = session.text;
    field.addEventListener('input', () => { session.text = field.value; const c = panel.querySelector('.drawer-cli'); if (c) c.textContent = cliCmd(row, session.scope, session.text); });
  } else {
    field = el('textarea', { class: 'config-input' }); field.value = session.text; field.rows = isTableList(row) ? 8 : Array.isArray(row.value) ? 4 : 2;
    field.addEventListener('input', () => { session.text = field.value; const c = panel.querySelector('.drawer-cli'); if (c) c.textContent = cliCmd(row, session.scope, session.text); });
  }
  panel.appendChild(field);
  panel.appendChild(el('div', { class: 'drawer-cli mono', text: cliCmd(row, session.scope, session.text) }));
  if (session.error) panel.appendChild(el('div', { class: 'config-warning', text: session.error }));
  const actions = el('div', { class: 'drawer-actions' });
  const saveWith = (init) => () => submitDrawerMutation(session, context, (visit) =>
    requestJson(`/api/config/keys/${encodeURIComponent(row.key)}`, 'PUT', { value: parseDraft(row, session.text), scope: session.scope, ...(init ? { init } : {}) }, visit));
  // A first workspace write offers only the explicit choices, so no click
  // creates the policy file implicitly; Close leaves it uncreated.
  const choosesInit = session.scope === 'workspace' && workspacePolicyMissing(payload);
  if (choosesInit) panel.appendChild(el('div', { class: 'config-note drawer-init-note', text: 'This workspace has no config policy file yet, and saving creates one. Seed from global copies the global policy (orbit config set --seed-from-global); Start empty uses built-in security defaults (--fresh).' }));
  const saves = choosesInit
    ? [['Seed from global', 'seed-from-global'], ['Start empty', 'fresh']]
    : [['Save', null]];
  for (const [label, init] of saves) {
    const save = el('button', { class: 'config-action', text: session.pending ? 'Saving…' : label }); save.type = 'button'; save.disabled = session.pending;
    if (init) save.dataset.init = init;
    save.addEventListener('click', saveWith(init));
    actions.appendChild(save);
  }
  const remove = el('button', { class: 'config-action ghost', text: 'Remove workspace value' }); remove.type = 'button'; remove.disabled = session.pending;
  remove.addEventListener('click', () => submitDrawerMutation(session, context, (visit) =>
    requestJson(`/api/config/keys/${encodeURIComponent(row.key)}?scope=workspace`, 'DELETE', undefined, visit)));
  const close = el('button', { class: 'config-action ghost', text: 'Close' }); close.type = 'button';
  close.addEventListener('click', () => { drawer = null; context.rerender?.(); });
  actions.append(remove, close); panel.appendChild(actions);
  return panel;
}
// The bulk-removal outcome. A clean removal expires on its own; one that names
// failures stays until dismissed, since the operator acts on it.
function noticeNode(context) {
  if (!notice) return null;
  const shown = notice;
  const dismiss = el('button', { class: 'config-action ghost', text: 'Dismiss', title: 'Dismiss this notice' }); dismiss.type = 'button';
  dismiss.addEventListener('click', () => { if (notice === shown) { notice = null; context.rerender?.(); } });
  return el('div', { class: 'config-note', role: 'status' }, [el('span', { text: shown.text }), ' ', dismiss]);
}
function showNotice(next, context) {
  notice = next;
  if (next.failed) return;
  setTimeout(() => { if (notice === next) { notice = null; context.rerender?.(); } }, NOTICE_MS);
}
function finish(body, payload, context, nodes) {
  const wrap = el('div', { class: 'topic-page' });
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
  if (rest.length) groups.push({ title: 'Other workflow keys', id: `workflow-section-${sectionSlug('Other workflow keys')}`, rows: rest });
  const query = workflowQuery.trim().toLowerCase();
  const matches = (r) => !query || `${r.key} ${r.label || ''} ${display(r.value)}`.toLowerCase().includes(query);
  const shown = groups
    .map((g) => ({ ...g, rows: query ? g.rows.filter(matches) : g.rows }))
    .filter((g) => !query || g.rows.length);
  const nodes = [];
  const binding = payload.workspace_binding;
  if (binding) nodes.push(el('div', { class: 'config-note', text: `Registry: base_branch ${binding.base_branch || '–'} · ship_mode ${binding.ship_mode || '–'}` }));
  // Direct navigation inside the pane: jump to any section (expanding it),
  // filter to matching keys, or collapse sections to a bounded subset. Every
  // control is a native button/input so the whole set is keyboard reachable.
  const nav = el('nav', { class: 'topic-controls workflow-nav' });
  nav.setAttribute('aria-label', 'Workflow sections');
  nav.appendChild(el('span', { class: 'workflow-nav-label', text: 'Sections' }));
  for (const g of shown) {
    const jump = el('button', { class: 'config-action ghost workflow-nav-link', text: g.title });
    jump.type = 'button';
    jump.dataset.workflowTarget = g.id;
    jump.addEventListener('click', () => {
      workflowCollapsedSet().delete(g.id);
      context.rerender?.();
      const target = document.getElementById(g.id);
      if (!target) return;
      syncWorkflowNavOffset(document.querySelector('nav.workflow-nav'));
      target.scrollIntoView({ block: 'start' });
      target.querySelector('.workflow-section-toggle')?.focus({ preventScroll: true });
    });
    nav.appendChild(jump);
  }
  const search = el('input', { class: 'config-filter workflow-search' });
  search.type = 'search'; search.placeholder = 'Search key, label or value…'; search.value = workflowQuery;
  search.setAttribute('aria-label', 'Search workflow keys');
  search.addEventListener('input', () => {
    workflowQuery = search.value;
    if (!workflowQuery.trim()) workflowSearchCollapsed = new Set();
    context.rerender?.();
    const rebuilt = document.querySelector('input.workflow-search');
    if (rebuilt) { rebuilt.focus({ preventScroll: true }); rebuilt.setSelectionRange(rebuilt.value.length, rebuilt.value.length); }
  });
  nav.append(el('span', { text: 'Search' }), search);
  const expandAll = el('button', { class: 'config-action ghost workflow-expand-all', text: 'Expand all' }); expandAll.type = 'button';
  // Expand all and Collapse all act on the sections the view shows, so a search
  // collapses only its matches. Each keeps keyboard focus on its own button.
  expandAll.addEventListener('click', () => {
    workflowCollapsedSet().clear(); context.rerender?.();
    document.querySelector('button.workflow-expand-all')?.focus({ preventScroll: true });
  });
  const collapseAll = el('button', { class: 'config-action ghost workflow-collapse-all', text: 'Collapse all' }); collapseAll.type = 'button';
  collapseAll.addEventListener('click', () => {
    const collapsed = workflowCollapsedSet();
    collapsed.clear(); for (const g of shown) collapsed.add(g.id);
    context.rerender?.();
    document.querySelector('button.workflow-collapse-all')?.focus({ preventScroll: true });
  });
  nav.append(expandAll, collapseAll);
  if (typeof ResizeObserver === 'function') new ResizeObserver(() => syncWorkflowNavOffset(nav)).observe(nav);
  nodes.push(nav);
  if (query && !shown.length) {
    const empty = el('section', { class: 'panel config-section' });
    empty.appendChild(el('div', { class: 'config-empty', text: 'No workflow keys match this search. Clear the search to show every key.' }));
    nodes.push(empty);
  }
  for (const g of shown) {
    nodes.push(section(g.title, g.rows, context, {
      id: g.id,
      collapsible: true,
      collapsed: workflowCollapsedSet().has(g.id),
      onToggle: () => {
        const collapsed = workflowCollapsedSet();
        if (collapsed.has(g.id)) collapsed.delete(g.id); else collapsed.add(g.id);
        context.rerender?.();
        const toggle = document.getElementById(g.id)?.querySelector('.workflow-section-toggle');
        toggle?.focus({ preventScroll: true });
      },
    }));
  }
  finish(body, payload, context, nodes);
}
export function renderExecution(body, payload, context) {
  const rows = flatten(payload).filter((r) => r.key.startsWith('execution.'));
  const nodes = [];
  if (payload.layers?.execution_not_inherited) nodes.push(el('div', { class: 'config-warning', text: 'execution.* keys in the global file are not inherited while this workspace sets execution policy; the workspace values below are in force.' }));
  nodes.push(section('Execution', rows, context));
  const paths = el('section', { class: 'panel config-section' });
  paths.appendChild(el('header', {}, [el('span', { class: 'config-section-title', text: 'Paths' })]));
  const pb = el('div', { class: 'config-section-body' });
  for (const p of payload.paths || []) pb.appendChild(el('div', { class: 'config-row topic-row topic-plain' }, [el('span', { class: 'config-key mono', text: p.label || p.name || '' }), el('span', { class: 'config-value mono', text: p.path || p.value || '' })]));
  paths.appendChild(pb); nodes.push(paths);
  finish(body, payload, context, nodes);
}
export function renderReview(body, payload, context) {
  const map = byKey(payload); const nodes = [];
  const review = payload.review || {};
  const layers = el('section', { class: 'panel config-section' });
  layers.appendChild(el('header', {}, [el('span', { class: 'config-section-title', text: 'Review layers' })]));
  const lb = el('div', { class: 'config-section-body' });
  for (const [id, label] of [['before_pr','Before PR'],['before_landing','Before landing'],['after_landing','After landing']]) {
    const step = review[id] || {};
    lb.appendChild(el('div', { class: 'config-row topic-row topic-plain' }, [el('span', { class: 'topic-label', text: label }), el('span', { class: 'topic-value-cell' }, [el('span', { class: `config-source ${step.enabled ? 'workspace' : ''}`, text: step.enabled ? 'on' : 'off' }), el('span', { class: 'config-value', text: step.line || '' })])]));
  }
  if (review.healthy === false) lb.appendChild(el('div', { class: 'config-warning', text: review.error || 'Review diagnostics unavailable.' }));
  layers.appendChild(lb); nodes.push(layers);
  const timing = map.get('review.timing'); const timingOff = !timing || timing.value === 'off' || timing.value == null;
  nodes.push(section('Timing', [timing].filter(Boolean), context));
  nodes.push(section('Reviewer pool', ['workflow.purpose_crews.review','workflow.purpose_crews.pre_landing_review'].map((k) => map.get(k)).filter(Boolean), context));
  nodes.push(section('Before landing', ['review.minutes','review.before_landing_hosts','review.baseline_commands'].map((k) => map.get(k)).filter(Boolean), context, { inactive: timingOff }));
  const diag = el('a', { class: 'config-action ghost', text: 'Full review diagnostics' }); diag.href = '#diagnostics';
  nodes.push(el('div', { class: 'topic-links' }, [diag]));
  const rest = flatten(payload).filter((r) => topicOf(r.key) === 'review' && !['review.timing','review.minutes','review.before_landing_hosts','review.baseline_commands','workflow.purpose_crews.review','workflow.purpose_crews.pre_landing_review'].includes(r.key));
  if (rest.length) nodes.push(section('Other review keys', rest, context));
  finish(body, payload, context, nodes);
}
export function renderMachine(body, payload, context, throttleNode) {
  // payload is the global file view merged with effective+host by config.js
  const eff = payload.effective || payload; const map = byKey(eff);
  const nodes = [el('div', { class: 'config-note', text: 'Edits on this page write only ~/.orbit/config.toml (the global file).' })];
  const identity = ['machine.id','machine.name','machine.task_prefix'].map((k) => map.get(k)).filter(Boolean);
  const idSec = el('section', { class: 'panel config-section' });
  idSec.appendChild(el('header', {}, [el('span', { class: 'config-section-title', text: 'Machine identity' })]));
  const ib = el('div', { class: 'config-section-body' });
  for (const r of identity) ib.appendChild(rowNode(r, context, { lockGlobal: true, readOnly: r.key !== 'machine.name' }));
  idSec.appendChild(ib); nodes.push(idSec);
  nodes.push(section('Auto-concurrency', flatten(eff).filter((r) => r.key.startsWith('machine.auto_concurrency.')), context, { lockGlobal: true }));
  nodes.push(section('Machine', flatten(eff).filter((r) => r.key.startsWith('machine.') && !r.key.startsWith('machine.auto_concurrency.') && !identity.includes(r)), context, { lockGlobal: true }));
  if (throttleNode) nodes.push(throttleNode);
  nodes.push(section('Tasks', ['tasks.id_start'].map((k) => map.get(k)).filter(Boolean), context, { lockGlobal: true }));
  finish(body, eff, context, nodes);
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
// Pages to the page holding `key` the first time `source` names it. Once it has
// landed, later renders with the same key leave the pager where the user moved
// it; a render with no key re-arms the landing for the next time it is named.
function landAllKeysPage(source, key, filtered) {
  if (!key) { allKeysLanded[source] = null; return; }
  if (key === allKeysLanded[source]) return;
  const at = filtered.findIndex((r) => r.key === key);
  if (at < 0) return;
  allKeysPage = Math.floor(at / ALL_KEYS_PAGE_SIZE);
  allKeysLanded[source] = key;
}
export function renderAllKeys(body, payload, context) {
  const rows = flatten(payload);
  const nodes = [];
  const controls = el('div', { class: 'topic-controls' });
  const mkSelect = (label, value, options, onChange) => {
    const s = el('select', { class: 'config-input' }); s.setAttribute('aria-label', label);
    for (const o of options) { const opt = el('option', { text: o }); opt.value = o; if (o === value) opt.selected = true; s.appendChild(opt); }
    s.addEventListener('change', () => onChange(s.value)); return s;
  };
  controls.append(el('span', { text: 'Layer' }), mkSelect('Layer filter', layerFilter, ['all','built-in','global','workspace'], (v) => { layerFilter = v; allKeysPage = 0; context.rerender?.(); }),
    el('span', { text: 'Topic' }), mkSelect('Topic filter', topicFilter, ['all','workflow','execution','review','machine','crews','housekeeping'], (v) => { topicFilter = v; allKeysPage = 0; context.rerender?.(); }));
  const search = el('input', { class: 'config-filter all-keys-search' });
  search.type = 'search'; search.placeholder = 'Search key, label or value…'; search.value = allKeysQuery;
  search.setAttribute('aria-label', 'Search keys');
  search.addEventListener('input', () => {
    allKeysQuery = search.value; allKeysPage = 0; context.rerender?.();
    // The rerender rebuilt this input; put the caret back at its end so the
    // next keystroke lands in the new field instead of the detached one.
    const rebuilt = document.querySelector('input.all-keys-search');
    if (rebuilt) { rebuilt.focus({ preventScroll: true }); rebuilt.setSelectionRange(rebuilt.value.length, rebuilt.value.length); }
  });
  controls.append(el('span', { text: 'Search' }), search);
  const jumpRaw = el('button', { class: 'config-action ghost all-keys-jump-raw', text: 'Jump to Raw TOML' }); jumpRaw.type = 'button';
  jumpRaw.addEventListener('click', () => {
    const target = document.getElementById('config-raw-toml');
    if (!target) return;
    target.scrollIntoView({ block: 'start' });
    target.querySelector('button')?.focus({ preventScroll: true });
  });
  controls.appendChild(jumpRaw);
  nodes.push(controls);
  const restates = rows.filter(isFallbackPreserving);
  if (restates.length) {
    const sec = section('Keys that only repeat the global or built-in value', restates, context);
    const bulk = el('button', { class: 'config-action', text: `Remove all ${restates.length} workspace values` }); bulk.type = 'button';
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
    sec.appendChild(bulk); nodes.push(sec);
  }
  const query = allKeysQuery.trim().toLowerCase();
  const filtered = rows.filter((r) => (layerFilter === 'all' || (r.source?.layer || r.state) === layerFilter || (layerFilter === 'built-in' && r.state !== 'set')) && (topicFilter === 'all' || topicOf(r.key) === topicFilter)
    && (!query || `${r.key} ${r.label || ''} ${display(r.value)}`.toLowerCase().includes(query)));
  // A deep link (?key=…) or an open drawer names an intended key: land on the
  // page holding it, once per key, so Next and Previous are not snapped back.
  landAllKeysPage('link', new URLSearchParams((window.location.hash.split('?')[1] || '')).get('key'), filtered);
  landAllKeysPage('drawer', drawer?.row?.key || null, filtered);
  const pageCount = Math.max(1, Math.ceil(filtered.length / ALL_KEYS_PAGE_SIZE));
  allKeysPage = Math.min(Math.max(0, allKeysPage), pageCount - 1);
  const pageStart = allKeysPage * ALL_KEYS_PAGE_SIZE;
  const pageRows = filtered.slice(pageStart, pageStart + ALL_KEYS_PAGE_SIZE);
  nodes.push(section('All keys', pageRows, context));
  const pager = el('div', { class: 'all-keys-pager' });
  const prev = el('button', { class: 'config-action ghost', text: 'Previous' }); prev.type = 'button'; prev.disabled = allKeysPage === 0;
  prev.addEventListener('click', () => { allKeysPage = Math.max(0, allKeysPage - 1); context.rerender?.(); });
  const next = el('button', { class: 'config-action ghost', text: 'Next' }); next.type = 'button'; next.disabled = allKeysPage >= pageCount - 1;
  next.addEventListener('click', () => { allKeysPage = Math.min(pageCount - 1, allKeysPage + 1); context.rerender?.(); });
  const range = filtered.length ? `Showing ${pageStart + 1}–${pageStart + pageRows.length} of ${filtered.length} keys · Page ${allKeysPage + 1} of ${pageCount}` : 'No keys match';
  const rangeNode = el('span', { class: 'all-keys-range', text: range }); rangeNode.setAttribute('aria-live', 'polite');
  pager.append(prev, rangeNode, next);
  nodes.push(pager);
  const raw = el('section', { class: 'panel config-section' });
  raw.id = 'config-raw-toml';
  raw.appendChild(el('header', {}, [el('span', { class: 'config-section-title', text: 'Raw TOML' })]));
  const rb = el('div', { class: 'config-section-body topic-actions' });
  for (const scope of ['workspace', 'global']) {
    const b = el('button', { class: 'config-action ghost', text: `Show ${scope} file` }); b.type = 'button';
    b.addEventListener('click', () => loadRaw(scope, context)); rb.appendChild(b);
  }
  if (rawScope) {
    const where = rawScope === 'global' ? 'every workspace on this machine (~/.orbit/config.toml)' : 'this workspace (.orbit/config.toml)';
    rb.appendChild(el('div', { class: 'topic-raw-label mono', text: `Showing ${where}` }));
    rb.appendChild(el('pre', { class: 'topic-raw mono', text: rawText == null ? 'Loading…' : (rawText || '(empty file)') }));
  }
  raw.appendChild(rb); nodes.push(raw);
  finish(body, payload, context, nodes);
}
