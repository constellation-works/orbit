// Settings topic pages: Workflow, Execution, Review, Machine, All keys.
// Rows come from /api/config/effective; the shared drawer writes via
// PUT/DELETE /api/config/keys/:key and re-reads via context.reload.
import { el, fetchJson, getWorkspace, requestJson } from './common.js';

let drawer = null; // {row, scope, text, error, pending, workspace, lockGlobal}
let layerFilter = 'all';
let topicFilter = 'all';
let rawScope = null;
let rawText = '';
let notice = null;

export function resetTopics() { drawer = null; layerFilter = 'all'; topicFilter = 'all'; rawScope = null; rawText = ''; notice = null; }

export function flatten(payload) {
  return (payload?.sections || []).flatMap((s) => s.keys || []);
}
function byKey(payload) { const m = new Map(); for (const r of flatten(payload)) m.set(r.key, r); return m; }
export function display(v) {
  if (v == null) return '–';
  if (Array.isArray(v)) return v.length ? v.join(' ') : '—';
  if (typeof v === 'string') return v === '' ? '""' : v;
  return String(v);
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
  if (key.startsWith('review.') || key === 'workflow.purpose_crews.review' || key === 'workflow.purpose_crews.pre_landing_review') return 'review';
  if (key.startsWith('machine.') || key === 'tasks.id_start' || key.startsWith('workflow.resource_throttle.')) return 'machine';
  if (key.startsWith('workflow.') || key.startsWith('pr.')) return 'workflow';
  if (key.startsWith('crews.')) return 'crews';
  return 'housekeeping';
}

const WORKFLOW_SECTIONS = [
  ['Shipping', (k, p) => ['workflow.base_branch','workflow.auto_ship','workflow.distributed_completion','workflow.required_validation_commands'].includes(k) || k.startsWith('workflow.validation_env.') || k.startsWith('pr.')],
  ['Crew routing', (k) => k === 'workflow.default_crew' || /complexity_crews$/.test(k) || k === 'workflow.crew_selection' || k.startsWith('workflow.crew_performance') || k.startsWith('workflow.provider_limit')],
  ['Task pilot', (k) => k === 'workflow.purpose_crews.task_pilot' || k.startsWith('workflow.task_pilot') || k.includes('task_pilot_freshness') || k === 'workflow.pilot_max_complexity'],
  ['Recovery', (k) => ['workflow.purpose_crews.failure_recovery','workflow.purpose_crews.auto_fix','workflow.purpose_crews.final_recovery','workflow.purpose_crews.baseline_red_repair','workflow.purpose_crews.sweep_tasks'].includes(k)],
  ['Steering & exploration', (k) => k.startsWith('workflow.mid_run_steering.') || k.startsWith('workflow.explore.') || k === 'workflow.purpose_crews.explorer'],
];

function openDrawer(row, context, lockGlobal) {
  if (!context.canEdit || row.settable === false) return;
  drawer = { row, scope: (lockGlobal || row.global_only) ? 'global' : (row.source?.layer === 'global' ? 'global' : 'workspace'),
    text: Array.isArray(row.value) ? row.value.join('\n') : display(row.value) === '–' ? '' : String(row.value ?? ''),
    error: null, pending: false, workspace: getWorkspace(), lockGlobal: !!(lockGlobal || row.global_only) };
  context.rerender?.();
}
function parseDraft(row, text) {
  const t = row.value_type || '';
  if (t === 'bool') return text === 'true';
  if (t === 'integer' || t === 'number') { const n = Number(text); return n; }
  if (Array.isArray(row.value) || t.includes('list') || t.includes('array')) return text.split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
  return text;
}
function cliCmd(row, scope, text) {
  const v = text.includes(' ') || text.includes('"') ? `'${text.replace(/'/g, "'\\''")}'` : (text || '…');
  return `orbit config set ${row.key} ${v}${scope === 'global' ? ' --global' : ''}`;
}

function rowNode(row, context, opts = {}) {
  const layer = row.state === 'set' ? (row.source?.layer || 'set') : row.state;
  const node = el('div', { class: `config-row topic-row${opts.inactive ? ' inactive' : ''}` });
  node.dataset.key = row.key;
  node.id = `key-${row.key.replace(/[^a-z0-9]+/gi, '-')}`;
  const pool = Array.isArray(row.value) && /crews$|purpose_crews/.test(row.key) ? weightBar(row.value) : null;
  node.append(
    el('span', { class: 'topic-label' }, [el('span', { text: row.label || row.key }), el('span', { class: 'config-key mono', text: row.key })]),
    el('span', { class: 'config-value mono', text: display(row.value) }),
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
function weightBar(values) {
  const wrap = el('span', { class: 'topic-pool' });
  let total = 0; const parts = values.map((v) => { const [n, w] = String(v).split(':'); const weight = Number(w) || 1; total += weight; return { n, weight }; });
  for (const p of parts) { const s = el('span', { class: 'topic-pool-seg', text: `${p.n} ${total ? Math.round((p.weight / total) * 100) : 0}%` }); s.style.flexGrow = String(p.weight); wrap.appendChild(s); }
  return wrap;
}
function section(title, rows, context, opts) {
  const sec = el('section', { class: 'panel config-section' });
  sec.appendChild(el('header', {}, [el('span', { class: 'config-section-title', text: title }), el('span', { class: 'count', text: `${rows.length} keys` })]));
  const body = el('div', { class: 'config-section-body' });
  if (!rows.length) body.appendChild(el('div', { class: 'config-empty', text: 'No keys in this section.' }));
  for (const r of rows) body.appendChild(rowNode(r, context, opts));
  sec.appendChild(body); return sec;
}

function drawerNode(payload, context) {
  if (!drawer) return null;
  if (drawer.workspace !== getWorkspace()) { drawer = null; return null; }
  const row = byKey(payload).get(drawer.row.key) || drawer.row;
  const panel = el('aside', { class: 'config-drawer' });
  panel.setAttribute('role', 'dialog'); panel.setAttribute('aria-label', `Edit ${row.key}`);
  panel.appendChild(el('h3', { text: row.label || row.key }));
  panel.appendChild(el('div', { class: 'config-key mono', text: row.key }));
  if (row.description) panel.appendChild(el('p', { text: row.description }));
  const chain = el('div', { class: 'drawer-chain' });
  chain.appendChild(el('div', { text: `built-in: ${display(row.built_in?.value)}` }));
  for (const s of row.shadowed_by || []) chain.appendChild(el('div', { text: `${s.layer}: ${display(s.value)}` }));
  chain.appendChild(el('div', { text: `effective (${row.source?.layer || row.state}): ${display(row.value)}` }));
  if (row.fallback) chain.appendChild(el('div', { text: `if removed → ${row.fallback.layer}: ${display(row.fallback.value)}` }));
  panel.appendChild(chain);
  for (const scope of ['workspace', 'global']) {
    const label = el('label', { class: 'drawer-scope' });
    const input = document.createElement('input'); input.type = 'radio'; input.name = 'drawer-scope'; input.value = scope;
    input.checked = drawer.scope === scope; input.disabled = drawer.lockGlobal && scope !== 'global';
    input.addEventListener('change', () => { drawer.scope = scope; context.rerender?.(); });
    label.append(input, document.createTextNode(scope === 'workspace' ? ' This workspace (.orbit/config.toml)' : ' Every workspace on this machine (~/.orbit/config.toml)'));
    panel.appendChild(label);
  }
  if (drawer.lockGlobal) panel.appendChild(el('div', { class: 'config-note', text: 'This key can only be written to the global file.' }));
  let field;
  if ((row.options || []).length) {
    field = el('select', { class: 'config-input' });
    for (const o of row.options) { const opt = el('option', { text: o }); opt.value = o; if (o === drawer.text) opt.selected = true; field.appendChild(opt); }
    field.addEventListener('change', () => { drawer.text = field.value; });
  } else {
    field = el('textarea', { class: 'config-input' }); field.value = drawer.text; field.rows = Array.isArray(row.value) ? 4 : 2;
    field.addEventListener('input', () => { drawer.text = field.value; const c = panel.querySelector('.drawer-cli'); if (c) c.textContent = cliCmd(row, drawer.scope, drawer.text); });
  }
  panel.appendChild(field);
  panel.appendChild(el('div', { class: 'drawer-cli mono', text: cliCmd(row, drawer.scope, drawer.text) }));
  if (drawer.error) panel.appendChild(el('div', { class: 'config-warning', text: drawer.error }));
  const actions = el('div', { class: 'drawer-actions' });
  const save = el('button', { class: 'config-action', text: drawer.pending ? 'Saving…' : 'Save' }); save.type = 'button'; save.disabled = drawer.pending;
  save.addEventListener('click', async () => {
    const session = drawer; session.pending = true; session.error = null; context.rerender?.();
    try { await requestJson(`/api/config/keys/${encodeURIComponent(row.key)}`, 'PUT', { value: parseDraft(row, session.text), scope: session.scope });
      if (drawer === session) drawer = null; if (session.workspace === getWorkspace()) await context.reload();
    } catch (e) { session.pending = false; session.error = e.message || String(e); if (drawer === session) context.rerender?.(); }
  });
  const remove = el('button', { class: 'config-action ghost', text: 'Remove workspace value' }); remove.type = 'button';
  remove.addEventListener('click', async () => {
    const session = drawer; try { await requestJson(`/api/config/keys/${encodeURIComponent(row.key)}?scope=workspace`, 'DELETE');
      if (drawer === session) drawer = null; await context.reload();
    } catch (e) { session.error = e.message || String(e); context.rerender?.(); }
  });
  const close = el('button', { class: 'config-action ghost', text: 'Close' }); close.type = 'button';
  close.addEventListener('click', () => { drawer = null; context.rerender?.(); });
  actions.append(save, remove, close); panel.appendChild(actions);
  return panel;
}
function finish(body, payload, context, nodes) {
  const wrap = el('div', { class: 'topic-page' });
  if (notice) wrap.appendChild(el('div', { class: 'config-note', text: notice }));
  for (const n of nodes) if (n) wrap.appendChild(n);
  const d = drawerNode(payload, context); if (d) wrap.appendChild(d);
  body.appendChild(wrap);
}

export function renderWorkflow(body, payload, context) {
  const rows = flatten(payload).filter((r) => topicOf(r.key) === 'workflow' && !r.key.startsWith('workflow.resource_throttle.'));
  const used = new Set(); const nodes = [];
  const binding = payload.workspace_binding;
  if (binding) nodes.push(el('div', { class: 'config-note', text: `Registry: base_branch ${binding.base_branch || '–'} · ship_mode ${binding.ship_mode || '–'}` }));
  for (const [title, pred] of WORKFLOW_SECTIONS) {
    const sec = rows.filter((r) => pred(r.key, payload)); sec.forEach((r) => used.add(r.key));
    nodes.push(section(title, sec, context));
  }
  const rest = rows.filter((r) => !used.has(r.key));
  if (rest.length) nodes.push(section('Other workflow keys', rest, context));
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
  nodes.push(section('Finding verification', ['review.verify_findings'].map((k) => map.get(k)).filter(Boolean), context));
  const link = el('a', { class: 'config-action ghost', text: 'finding_verification routine in Automation' }); link.href = '#operations/routines';
  const diag = el('a', { class: 'config-action ghost', text: 'Full review diagnostics' }); diag.href = '#diagnostics';
  nodes.push(el('div', { class: 'topic-links' }, [link, diag]));
  const rest = flatten(payload).filter((r) => topicOf(r.key) === 'review' && !['review.timing','review.minutes','review.before_landing_hosts','review.baseline_commands','review.verify_findings','workflow.purpose_crews.review','workflow.purpose_crews.pre_landing_review'].includes(r.key));
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
  rawScope = scope;
  try { const f = await fetchJson(`/api/config/file?scope=${scope}`); rawText = f.text ?? '(raw TOML not reported by this server)'; }
  catch (e) { rawText = e.message || String(e); }
  context.rerender?.();
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
  controls.append(el('span', { text: 'Layer' }), mkSelect('Layer filter', layerFilter, ['all','built-in','global','workspace'], (v) => { layerFilter = v; context.rerender?.(); }),
    el('span', { text: 'Topic' }), mkSelect('Topic filter', topicFilter, ['all','workflow','execution','review','machine','crews','housekeeping'], (v) => { topicFilter = v; context.rerender?.(); }));
  nodes.push(controls);
  const restates = rows.filter((r) => r.source?.layer === 'workspace' && (r.equals?.global === true || r.equals?.built_in === true));
  if (restates.length) {
    const sec = section('Keys that only repeat the global or built-in value', restates, context);
    const bulk = el('button', { class: 'config-action', text: `Remove all ${restates.length} workspace values` }); bulk.type = 'button';
    bulk.addEventListener('click', async () => {
      let ok = 0; const failures = [];
      for (const r of restates) { try { await requestJson(`/api/config/keys/${encodeURIComponent(r.key)}?scope=workspace`, 'DELETE'); ok++; } catch (e) { failures.push(`${r.key}: ${e.message}`); } }
      notice = `Removed ${ok} workspace value(s).${failures.length ? ' Failed: ' + failures.join('; ') : ''}`;
      await context.reload();
    });
    sec.appendChild(bulk); nodes.push(sec);
  }
  const filtered = rows.filter((r) => (layerFilter === 'all' || (r.source?.layer || r.state) === layerFilter || (layerFilter === 'built-in' && r.state !== 'set')) && (topicFilter === 'all' || topicOf(r.key) === topicFilter));
  nodes.push(section('All keys', filtered, context));
  const raw = el('section', { class: 'panel config-section' });
  raw.appendChild(el('header', {}, [el('span', { class: 'config-section-title', text: 'Raw TOML' })]));
  const rb = el('div', { class: 'config-section-body topic-actions' });
  for (const scope of ['workspace', 'global']) {
    const b = el('button', { class: 'config-action ghost', text: `Show ${scope} file` }); b.type = 'button';
    b.addEventListener('click', () => loadRaw(scope, context)); rb.appendChild(b);
  }
  if (rawScope) { const pre = el('pre', { class: 'topic-raw mono', text: rawText || '(empty file)' }); rb.appendChild(pre); }
  raw.appendChild(rb); nodes.push(raw);
  finish(body, payload, context, nodes);
}
