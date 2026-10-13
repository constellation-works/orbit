// Execute the shipped audit renderer and its common DOM helpers.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

class Node {
  constructor(tag) {
    this.tagName = tag; this.children = []; this.dataset = {}; this.className = '';
    this.text = ''; this.listeners = new Map(); this.style = { setProperty() {} };
    this.classList = {
      add: name => { this.className += ` ${name}`; },
      toggle: (name, on) => {
        const names = this.className.split(' ').filter(n => n && n !== name);
        this.className = (on ? [...names, name] : names).join(' ');
      },
    };
  }
  set textContent(value) { this.text = String(value); this.children = []; }
  set innerHTML(value) { assert.equal(value, ''); this.children = []; this.text = ''; }
  get textContent() { return this.text + this.children.map(child => child.textContent ?? child).join(' '); }
  get childNodes() { return this.children; }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  append(child) { this.appendChild(typeof child === 'string' ? document.createTextNode(child) : child); }
  removeChild(child) { this.children.splice(this.children.indexOf(child), 1); child.parentNode = null; }
  insertBefore(child, before) {
    child.parentNode?.removeChild(child);
    this.children.splice(this.children.indexOf(before), 0, child); child.parentNode = this;
  }
  get lastElementChild() { return this.children.at(-1); }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  setAttribute(name, value) { this[name] = value; }
  contains(child) { return this.children.some(node => node === child || node.contains(child)); }
}
const container = new Node('div'); container.id = 'audit-summary-body';
const title = new Node('h3');
const costHost = new Node('div'); costHost.id = 'scoreboard-orchestration';
const costCount = new Node('span');
const diagnosticsBody = new Node('div'); diagnosticsBody.id = 'diag-body';
const diagnosticsCount = new Node('span');
const auditScopeChips = new Node('div');
const auditFilterChips = new Node('div');
const document = {
  activeElement: null,
  createElement: tag => new Node(tag),
  createTextNode: text => { const node = new Node('#text'); node.textContent = text; return node; },
  getElementById: id => ({ 'audit-summary-body': container, 'audit-summary-title': title, 'scoreboard-orchestration': costHost, 'scoreboard-orchestration-count': costCount, 'diag-body': diagnosticsBody, 'diag-count': diagnosticsCount, 'audit-scope-chips': auditScopeChips, 'audit-filter': auditFilterChips })[id] || null,
  querySelectorAll: () => [],
};
const window = {
  location: {
    href: 'http://dashboard.test/?workspace=ws_fixture&window=24h',
    search: '?workspace=ws_fixture&window=24h',
    hash: '',
  },
};
const requestedPaths = [];
const context = vm.createContext({
  URL, URLSearchParams, window, document, console, AbortController, setTimeout, clearTimeout,
  fetch: async path => { requestedPaths.push(path); return { ok: true, headers: { get: () => null }, json: async () => [] }; },
});
const common = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/common.js', import.meta.url), 'utf8'), { context });
const audit = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/audit.js', import.meta.url), 'utf8'), { context });
const codebase = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/codebase.js', import.meta.url), 'utf8'), { context });
await common.link(() => { throw new Error('unexpected common dependency'); });
await codebase.link(name => { assert.equal(name, './common.js'); return common; });
await audit.link(name => ({ './common.js': common, './codebase.js': codebase })[name]);
await audit.evaluate();
const incidents = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/incidents.js', import.meta.url), 'utf8'), { context });
await incidents.link(name => ({ './common.js': common, './audit.js': audit })[name]);
const diagnostics = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/diagnostics.js', import.meta.url), 'utf8'), { context });
await diagnostics.link(name => ({ './common.js': common, './audit.js': audit, './incidents.js': incidents })[name]);
await diagnostics.evaluate();
const payload = {
  window: '24h',
  duration_by_tool: [
    { tool: 'unknown', count: 2000, avg: 90000, p95: 100000 },
    { tool: '', count: 2, avg: 80000, p95: 90000 },
    { tool: 'orbit.search', count: 5, avg: 120, p95: 200 },
  ],
  tool_call_failure_rate: { failed: 2, total: 9, rate: 2 / 9, unexpected: 1, denied: 4 },
  tool_call_failures_by_tool: [
    { tool: 'orbit.workflow.run.list', failed: 2, total: 9, rate: 2 / 9, unexpected: 1, denied: 3 },
    { tool: 'orbit.friction.list', failed: 0, total: 0, rate: 0, unexpected: 0, denied: 1 },
    { tool: 'unknown', failed: 5, total: 5, rate: 1, unexpected: 5, denied: 0 },
  ],
};
audit.namespace.renderAuditSummary(payload);
const card = container.children.find(node => node.dataset.key === 'tool-call-failures-by-tool');
const table = card.children[1].children[0];
const headers = table.children[0].children[0].children;
const rows = table.children[1].children;
const cell = (row, field) => row.children[headers.findIndex(header => header.dataset.column === field)].textContent;
for (const [field, expected] of Object.entries({ tool: 'orbit.workflow.run.list', failed: '2', total: '9', rate: '22.2%', unexpected: '1', denied: '3' })) {
  assert.equal(cell(rows[0], field), expected, `mixed-call rendered ${field}`);
}
assert.equal(rows.length, 2, 'synthetic unnamed tools stay out of the table');
const durationTable = container.children.find(node => node.dataset.key === 'duration-by-tool').children[1].children[0];
assert.equal(durationTable.children[1].children.length, 1, 'duration table excludes unnamed buckets');
assert.equal(durationTable.children[1].children[0].children[0].textContent, 'orbit.search');
assert.match(rows[0].children[0].title, /9.*22\.2%.*1.*3/, 'row title retains the secondary counts on compact cards');
for (const [field, expected] of Object.entries({ tool: 'orbit.friction.list', failed: '0', total: '0', rate: '0.0%', unexpected: '0', denied: '1' })) {
  assert.equal(cell(rows[1], field), expected, `denial-only rendered ${field}`);
}
const rateCard = container.children.find(node => node.dataset.key === 'tool-call-failure-rate');
assert.match(rateCard.textContent, /22\.2%.*2 failed \/ 9 tool calls/);
assert.match(rateCard.textContent, /4 denied calls excluded/);
rows[0].listeners.get('click')();
const route = new URLSearchParams(window.location.hash.split('?')[1]);
assert.equal(common.namespace.getWorkspace(), 'ws_fixture', 'drill-down keeps the selected workspace');
assert.equal(audit.namespace.effectiveAuditWindow(), '24h', 'drill-down keeps the selected window');
assert.equal(route.get('tool'), 'orbit.workflow.run.list');
audit.namespace.renderAuditSummary(payload);
assert.equal(container.children.find(node => node.dataset.key === 'tool-call-failures-by-tool'), card, 'unchanged refresh keeps the rendered card');
audit.namespace.renderAuditSummary({ ...payload, tool_call_failures_by_tool: [] });
assert.equal(container.children.some(node => node.dataset.key === 'tool-call-failures-by-tool'), false, 'empty refresh removes the stale table');
const categoryPayload = {
  window: '24h', failure_incidents_scan_limit: 10000,
  failure_categories: { unexpected: { incidents: 2, raw_events: 3, affected_runs: 1 } },
};
const categoryCard = () => container.children.find(node => node.dataset.key === 'failure-categories');
audit.namespace.renderAuditSummary({ ...categoryPayload, failure_incidents_truncated: true });
assert.match(categoryCard().textContent, /capped/i, 'scan-limited category counts carry a visible qualifier');
assert.match(categoryCard().textContent, /10,000/, 'category coverage displays the payload scan limit');
const cappedCard = categoryCard();
audit.namespace.renderAuditSummary({ ...categoryPayload, failure_incidents_scan_limit: 5000, failure_incidents_truncated: true });
assert.notEqual(categoryCard(), cappedCard, 'scan-limit changes invalidate the cached category card');
assert.match(categoryCard().textContent, /5,000/);
audit.namespace.renderAuditSummary({ ...categoryPayload, failure_incidents_truncated: false });
assert.doesNotMatch(categoryCard().textContent, /capped|partial/i, 'complete refresh removes stale coverage warnings');

const incidentDerivedPayload = {
  window: '7d', failure_incidents_scan_limit: 10000,
  failure_rate_by_tool: [{ tool: 'orbit.task.add', rate: 0.2, failures: 2, successes: 8, total: 10 }],
  failures_by_tool: [{ tool: 'orbit.task.add', count: 2, class: 'unexpected' }],
  failure_categories: { unexpected: { incidents: 2, raw_events: 2, affected_runs: 1 } },
  lifecycle_diagnostic_events: 1, lifecycle_diagnostic_incidents: 1,
  lifecycle_diagnostic_affected_run_count: 1, lifecycle_diagnostic_label: 'Lifecycle diagnostics',
};
const incidentDerivedCard = key => container.children.find(node => node.dataset.key === key);
const toolFailuresCard = () => incidentDerivedCard('failures-by-tool');
const lifecycleCard = () => incidentDerivedCard('lifecycle-diagnostics');
audit.namespace.renderAuditSummary({ ...incidentDerivedPayload, failure_incidents_truncated: true });
assert.match(toolFailuresCard().textContent, /capped counts/i, 'per-tool unexpected failure counts are marked capped');
assert.match(toolFailuresCard().textContent, /10,000/, 'per-tool rates show the incident scan limit');
assert.match(toolFailuresCard().textContent, /successful-call counts cover the full window.*understated/i,
  'per-tool rates disclose the scanned numerator and full-window success counts');
assert.match(lifecycleCard().textContent, /capped counts/i, 'lifecycle counts are marked capped');
assert.match(lifecycleCard().textContent, /10,000/, 'lifecycle coverage shows the incident scan limit');
const cappedToolFailuresCard = toolFailuresCard();
const cappedLifecycleCard = lifecycleCard();
audit.namespace.renderAuditSummary({ ...incidentDerivedPayload, failure_incidents_truncated: false });
assert.notEqual(toolFailuresCard(), cappedToolFailuresCard, 'coverage changes invalidate the per-tool card');
assert.notEqual(lifecycleCard(), cappedLifecycleCard, 'coverage changes invalidate the lifecycle card');
assert.doesNotMatch(toolFailuresCard().textContent, /capped|partial/i, 'complete per-tool rates clear coverage warnings');
assert.doesNotMatch(lifecycleCard().textContent, /capped|partial/i, 'complete lifecycle counts clear coverage warnings');
audit.namespace.renderAuditSummary({
  ...incidentDerivedPayload, failure_incidents_truncated: true, failure_incidents_scan_limit: 5000,
});
assert.match(toolFailuresCard().textContent, /5,000/, 'per-tool card refreshes when the scan limit changes');
assert.match(lifecycleCard().textContent, /5,000/, 'lifecycle card refreshes when the scan limit changes');

const scoreboard = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/scoreboard.js', import.meta.url), 'utf8'), { context });
await scoreboard.link(name => ({ './common.js': common })[name]);
await scoreboard.evaluate();
const descendants = node => [node, ...node.children.flatMap(descendants)];
const removedCostChrome = () => descendants(costHost).some(node => {
  const names = String(node.className).split(' ');
  return ['scoreboard-narrative', 'scoreboard-agent-strip', 'scoreboard-body', 'scoreboard-insights', 'scoreboard-insights-panel'].includes(node.id)
    || ['scoreboard-narrative', 'scoreboard-agent-card', 'sb2-matrix', 'scoreboard-insights'].some(name => names.includes(name));
});
const costBucket = {
  kind: 'orchestrator', orchestrator: 'claude', invocation_count: 2, linked_task_count: 1,
  provider_cost_usd: 1.25, provider_cost_count: 2, missing_provider_count: 0,
  derived_cost_usd: 1.5, derived_cost_count: 2, unpriced_derived_count: 0,
  comparable_cost_count: 2, comparable_provider_cost_usd: 1.25, comparable_derived_cost_usd: 1.5,
  comparable_cost_delta_usd: 0.25,
  models: [{ model: 'claude-sonnet', tokens: { normalized_token_total: 1500 } }],
};
const costOrchestration = {
  since: '2026-10-12T00:00:00Z', until: '2026-10-13T00:00:00Z', as_of: '2026-10-13T00:00:00Z',
  normalized_tokens: {
    normalized_token_total: 1500, invocation_count: 2, linked_task_count: 1,
    uncached_input_tokens: 100, cache_read_tokens: 200, cache_create_tokens: 0,
    cache_create_1h_tokens: 0, output_tokens: 50, covered_invocation_count: 2,
    unknown_input_basis_or_model_count: 0,
  },
  buckets: [costBucket],
};
scoreboard.namespace.renderScoreboard({ window: '24h', agents: {}, orchestration: costOrchestration });
assert.match(costHost.textContent, /\$1\.2500/, 'provider-reported dollars keep four decimal places');
assert.match(costHost.textContent, /\$1\.5000/, 'derived estimate keeps four decimal places');
assert.match(costHost.textContent, /1\.5K/, 'normalized tokens stay compact');
assert.match(costHost.textContent, /named orchestrator: claude/);
assert.match(costHost.textContent, /claude-sonnet 1\.5K/, 'model attribution stays on the same population');
assert.equal(costCount.textContent, '1 buckets');
assert.equal(removedCostChrome(), false, 'cost render has no leaderboard, narrative, agent strip or insights');
scoreboard.namespace.renderScoreboard({
  window: '7d', agents: { codex: { tasks_created: 4 } },
  orchestration: { ...costOrchestration, buckets: [{ ...costBucket, provider_cost_usd: 9.5 }] },
});
assert.match(costHost.textContent, /\$1\.2500/, 'a 7d body is not painted under the 24h window');
assert.doesNotMatch(costHost.textContent, /\$9\.5000/);
scoreboard.namespace.renderScoreboard({ window: '24h', agents: {}, orchestration: null });
assert.match(costHost.textContent, /No managed execution cost data yet/);
assert.equal(costCount.textContent, '—');

// The family leaderboard no longer opens Audit. Incidents still do, through
// the same drill-down, and the landing chips must keep family and non-success.
audit.namespace.navigateToDrilldown({
  agent_family: 'codex', metric: 'failure_incidents', status: 'non_success',
});
const familyRoute = new URLSearchParams(window.location.hash.split('?')[1]);
assert.equal(familyRoute.get('agent_family'), 'codex');
assert.equal(familyRoute.has('role'), false, 'family drill-down uses the family predicate');
assert.equal(familyRoute.get('status'), 'non_success', 'incidents include failed and denied rows');
audit.namespace.applyAuditHashQuery(familyRoute);
assert.equal(audit.namespace.buildAuditHash(), window.location.hash, 'route survives reload');
await audit.namespace.fetchAndRenderAudit();
const familyRequest = new URLSearchParams(requestedPaths.at(-1).split('?')[1]);
assert.equal(familyRequest.get('agent_family'), 'codex', 'family reaches the Events API');
assert.equal(familyRequest.get('workspace'), 'ws_fixture');
assert.equal(familyRequest.get('since'), '24h');
assert.equal(familyRequest.get('status'), 'non_success');
const familyChip = auditScopeChips.children.find(node => node.dataset.chip === 'agent family');
assert.ok(familyChip, 'family filter can be removed');
familyChip.listeners.get('click')({ preventDefault() {} });
assert.equal(new URLSearchParams(window.location.hash.split('?')[1]).has('agent_family'), false);
audit.namespace.buildAuditChips();
const nonSuccessChip = auditFilterChips.children.find(node => node.dataset.status === 'non_success');
assert.ok(nonSuccessChip, 'combined status filter is available in Events');
nonSuccessChip.listeners.get('click')();
assert.equal(new URLSearchParams(window.location.hash.split('?')[1]).has('status'), false, 'combined status can be cleared');
audit.namespace.navigateToRole('gpt-6.1-sol');
const exactRoleRoute = new URLSearchParams(window.location.hash.split('?')[1]);
assert.equal(exactRoleRoute.get('role'), 'gpt-6.1-sol');
assert.equal(exactRoleRoute.has('agent_family'), false, 'exact-role navigation resets the family');

const findNode = (root, predicate) => descendants(root).find(predicate);
const plainClick = (node) => node.listeners.get('click')({
  stopPropagation() {},
  preventDefault() {},
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  button: 0,
});
const hasClass = (node, name) => String(node.className).split(' ').includes(name);
const incidentRows = () => descendants(diagnosticsBody).filter(node => node.tagName === 'button' && hasClass(node, 'incident-row'));
const detailPanel = () => findNode(diagnosticsBody, node => hasClass(node, 'incident-detail'));
const inDetail = (predicate) => descendants(detailPanel()).filter(predicate);
const partOf = (node, name) => descendants(node).find(child => hasClass(child, name));
const detailButton = (text) => inDetail(node => node.tagName === 'button' && node.textContent === text)[0];
const classTile = (key) => findNode(diagnosticsBody, node => hasClass(node, 'incident-tile') && node.dataset.class === key);
const openAuditEvents = () => {
  const button = detailButton('Audit events');
  assert.equal(typeof button?.listeners.get('click'), 'function', 'the selected incident links its audit events');
  button.listeners.get('click')();
};
let incidentPayload = {
  window: '24h', incident_count: 1, raw_failed_events: 65, total_events: 100,
  incidents: [{
    incident_id: 'cli-incident', class: 'unexpected', has_tool_identity: false,
    surface: 'task add', actor: 'codex', event_count: 65, last_ts: '2026-10-07T09:00:00Z',
    events: Array.from({ length: 65 }, (_, index) => ({
      id: index + 1, status: 'failure', tool: null, actor: 'codex', ts: '2026-10-07T09:00:00Z',
    })),
  }],
};
const diagnosticsContext = {
  getActiveDiagSubtab: () => 'incidents',
  getLastDiagnostics: () => ({ incidents: incidentPayload }),
};
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
assert.equal(incidentRows().length, 1);
assert.equal(incidentRows()[0]['aria-pressed'], 'true', 'the only incident is selected for the detail panel');
openAuditEvents();
let incidentRoute = new URLSearchParams(window.location.hash.split('?')[1]);
assert.equal(incidentRoute.get('ids'), Array.from({ length: 65 }, (_, index) => index + 1).join(','));
assert.equal(incidentRoute.has('tool'), false, 'tool-less incidents route by exact event IDs');
assert.equal(incidentRoute.has('status'), false, 'exact event IDs retain stored failure rows');
audit.namespace.applyAuditHashQuery(incidentRoute);
assert.equal(new URLSearchParams(audit.namespace.buildAuditHash().split('?')[1]).get('ids'), incidentRoute.get('ids'));

// A class tile picks the class the list shows.
classTile('denied').listeners.get('click')();
assert.equal(classTile('denied')['aria-pressed'], 'true');
assert.equal(classTile('unexpected')['aria-pressed'], 'false');
incidentPayload = {
  window: '24h', incident_count: 1, raw_failed_events: 1, total_events: 1,
  incidents: [{
    incident_id: 'denied-failure-row', class: 'denied', has_tool_identity: true,
    surface: 'orbit.task.add', actor: 'codex', event_count: 1, last_ts: '2026-10-07T09:00:00Z',
    events: [{ id: 9001, status: 'failure', tool: 'orbit.task.add', actor: 'codex', ts: '2026-10-07T09:00:00Z' }],
  }],
};
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
openAuditEvents();
incidentRoute = new URLSearchParams(window.location.hash.split('?')[1]);
assert.equal(incidentRoute.get('ids'), '9001', 'denied incident points to its stored failure row');
assert.equal(incidentRoute.has('status'), false, 'denied classification does not rewrite the stored status');
assert.equal(incidentRoute.has('tool'), false, 'exact incident rows are not narrowed by surface');
classTile('unexpected').listeners.get('click')();
assert.equal(incidentRows().length, 0, 'the unexpected list does not show a denied incident');

incidentPayload = {
  window: '24h', incident_count: 1, raw_failed_events: 7, total_events: 7,
  incidents: [{
    incident_id: 'doctor-no-message', class: 'unexpected', has_tool_identity: false,
    signature: 'unexpected|role=human|surface=doctor|msg=exit=1',
    surface: 'doctor', actor: 'human', event_count: 7, last_ts: '2026-10-07T09:00:00Z',
    events: [{ id: 1, status: 'failure', tool: null, actor: 'human', ts: '2026-10-07T09:00:00Z' }],
  }],
};
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
const listedMessage = () => partOf(incidentRows()[0], 'incident-headline').textContent;
const fullMessage = () => inDetail(node => hasClass(node, 'incident-full-message'))[0].textContent;
assert.doesNotMatch(listedMessage(), /\|role=/, 'the grouping key is never the incident message');
assert.match(listedMessage(), /doctor failed; no message recorded/, 'a message-less incident reads as such');
assert.equal(partOf(incidentRows()[0], 'incident-flag').textContent, 'no message', 'the row flags it in words');
assert.equal(fullMessage(), 'doctor failed; no message recorded', 'the detail panel never shows the signature as the message');
assert.match(
  inDetail(node => node.className === 'incident-signature')[0].textContent,
  /unexpected\|role=human\|surface=doctor/,
  'the signature stays available in the audit evidence',
);
incidentPayload.incidents[0].message = 'doctor reported findings: 1 failure (review), 0 warnings';
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
assert.match(listedMessage(), /1 failure \(review\)/, 'a recorded message is shown as is');
assert.equal(inDetail(node => hasClass(node, 'incident-message-note')).length, 0, 'a whole message has no truncation note');
incidentPayload.incidents[0].message = 'doctor reported findings: 1 failure (review), 0 warnings, then…';
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
assert.equal(inDetail(node => hasClass(node, 'incident-message-note')).length, 1, 'a message cut before storage says so');
incidentPayload.incidents[0].events[0].message = 'doctor reported findings: 1 failure (review), 0 warnings, then 2 skipped checks';
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
assert.equal(fullMessage(), 'doctor reported findings: 1 failure (review), 0 warnings, then 2 skipped checks', 'an event that kept the whole message supplies it');
assert.equal(inDetail(node => hasClass(node, 'incident-message-note')).length, 0);

// Identical signatures across runs collapse to one row, sorted by last seen.
// Each run stays an incident in the detail panel, with its own id and links.
const sweepMessage = 'execution failed: v2 job dispatch: deterministic action `file_ci_failure_tasks` failed: execution failed: ci_failure_sweep retryable: {"outcome":"retryable_error","stage":"collection_or_investigation"}';
const sweepSignature = 'unexpected|role=human|surface=run-pipeline-worker|msg=ci_failure_sweep retryable';
const sweepFires = [
  ['2026-10-10T07:00:00Z', '2h', 'jrun-sweep-0700', 'inc-sweep-0700'],
  ['2026-10-10T07:20:00Z', '1h', 'jrun-sweep-0720', 'inc-sweep-0720'],
  ['2026-10-10T07:40:00Z', '53m', 'jrun-sweep-0740', 'inc-sweep-0740'],
  ['2026-10-10T08:00:00Z', '33m', 'jrun-sweep-0800', 'inc-sweep-0800'],
  ['2026-10-10T08:20:00Z', '13m', 'jrun-sweep-0820', 'inc-sweep-0820'],
];
const relativeLabels = Object.fromEntries(sweepFires.map(([ts, label]) => [ts, label]));
relativeLabels['2026-10-10T08:40:00Z'] = '7m';
relativeLabels['2026-10-10T08:50:00Z'] = '3m';
const opened = [];
diagnosticsContext.fmtRelative = (value) => relativeLabels[value] || value;
diagnosticsContext.setActiveTab = (route) => { opened.push(route); };
diagnosticsContext.navigateToRun = (runId) => { opened.push(`navigate:${runId}`); };
const sweepIncident = (ts, runId, incidentId, signature = sweepSignature, surface = 'run-pipeline-worker') => ({
  incident_id: incidentId,
  signature,
  class: 'unexpected',
  class_label: 'unexpected failure',
  surface,
  actor: 'human',
  message: sweepMessage,
  event_count: 1,
  first_ts: ts,
  last_ts: ts,
  run_ids: [runId],
  task_ids: ['ORB-9001'],
  has_tool_identity: false,
  events: [{
    id: 42,
    ts,
    status: 'failure',
    actor: 'human',
    surface,
    run_id: runId,
    task_id: 'ORB-9001',
    workspace_id: 'ws_fixture',
    message: sweepMessage,
  }],
});
incidentPayload = {
  window: '24h',
  incident_count: 7,
  shown_incident_count: 7,
  matching_incident_count: 7,
  raw_failed_events: 7,
  total_events: 70,
  incidents: [
    sweepIncident('2026-10-10T08:40:00Z', 'jrun-one-off', 'inc-one-off', 'unexpected|role=human|surface=orbit.task.add|msg=other', 'orbit.task.add'),
    ...sweepFires.map(([ts, , runId, incidentId]) => sweepIncident(ts, runId, incidentId)),
    sweepIncident('2026-10-10T08:50:00Z', 'jrun-other-surface', 'inc-other-surface', 'unexpected|role=human|surface=orbit.doctor|msg=ci_failure_sweep retryable', 'orbit.doctor'),
  ],
};
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
const rowFacts = () => incidentRows().map(row => ({
  sub: partOf(row, 'incident-sub').textContent,
  count: partOf(row, 'incident-count').textContent,
}));
assert.deepEqual(rowFacts(), [
  { sub: 'orbit.doctor · human', count: '1 event' },
  { sub: 'orbit.task.add · human', count: '1 event' },
  { sub: 'run-pipeline-worker · human · every 20 min', count: '5 runs' },
], 'five sweep fires roll up into one row; the same message on another signature and the one-off stay their own rows, newest last seen first');
const sweepRow = () => incidentRows()[2];
assert.equal(sweepRow().textContent.includes('jrun-sweep-0820'), false, 'a row does not list its runs');
plainClick(sweepRow());
assert.equal(sweepRow()['aria-pressed'], 'true', 'clicking a row selects it');
assert.equal(inDetail(node => node.className === 'incident-sentence')[0].textContent, '5 runs since 2h ago, every 20 min. Last seen 13m ago.');
assert.equal(fullMessage(), sweepMessage, 'the detail panel shows the full message');
assert.deepEqual(
  inDetail(node => node.className === 'incident-related-item').map(node => partOf(node, 'mono').textContent),
  ['orbit.doctor · human'],
  'the same message on another signature is related; a different message is not',
);
assert.deepEqual(
  inDetail(node => node.className === 'incident-run').map(node => node.dataset.incident),
  ['inc-sweep-0820', 'inc-sweep-0800', 'inc-sweep-0740', 'inc-sweep-0720', 'inc-sweep-0700'],
  'the runs list each run newest first, keeping the per-run incident id',
);
detailButton('View runs (5)').listeners.get('click')();
assert.equal(inDetail(node => hasClass(node, 'incident-runs'))[0].open, true, 'View runs opens the run list');
const runLink = inDetail(node => node.tagName === 'a' && node.textContent === 'jrun-sweep-0700')[0];
const taskLink = inDetail(node => node.tagName === 'a' && node.textContent === 'ORB-9001')[0];
assert.match(runLink.href, /#runs\?run_id=jrun-sweep-0700/);
assert.match(runLink.href, /workspace=ws_fixture/);
assert.match(taskLink.href, /#tasks\?open=ORB-9001$/);
plainClick(runLink);
assert.deepEqual(opened, ['navigate:jrun-sweep-0700']);
plainClick(incidentRows()[1]);
detailButton('View run').listeners.get('click')();
assert.deepEqual(opened, ['navigate:jrun-sweep-0700', 'runs/jrun-one-off'], 'a single-run incident opens its run');

incidentPayload = {
  window: '24h', incident_count: 2, shown_incident_count: 2, matching_incident_count: 2,
  incidents: [
    { incident_id: 'bare-a', class: 'unexpected', message: sweepMessage, event_count: 1, last_ts: '2026-10-10T08:00:00Z', run_ids: ['jrun-bare-a'] },
    { incident_id: 'bare-b', class: 'unexpected', message: sweepMessage, event_count: 1, last_ts: '2026-10-10T08:20:00Z', run_ids: ['jrun-bare-b'] },
  ],
};
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
assert.equal(incidentRows().length, 2, 'incidents with no signature stay separate even when the message matches');

incidentPayload = {
  window: '24h', incident_count: 4, shown_incident_count: 4, matching_incident_count: 4,
  incidents: ['2026-10-10T08:00:00Z', '2026-10-10T08:05:00Z', '2026-10-10T08:10:00Z', '2026-10-10T09:30:00Z']
    .map((ts, index) => sweepIncident(ts, `jrun-irregular-${index}`, `inc-irregular-${index}`, 'unexpected|role=human|surface=run-pipeline-worker|msg=irregular')),
};
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
assert.match(rowFacts()[0].sub, /about every 5 min/, 'uneven gaps are not labelled as a fixed schedule');

const loaded = [];
diagnosticsContext.loadMoreIncidents = (limit) => { loaded.push(limit); };
incidentPayload = {
  window: '24h', incident_count: 80, shown_incident_count: 50, matching_incident_count: 80, limit: 50,
  incidents: [
    sweepIncident('2026-10-10T08:00:00Z', 'jrun-capped-a', 'inc-capped-a', 'unexpected|role=human|surface=run-pipeline-worker|msg=capped'),
    sweepIncident('2026-10-10T08:20:00Z', 'jrun-capped-b', 'inc-capped-b', 'unexpected|role=human|surface=run-pipeline-worker|msg=capped'),
  ],
};
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
assert.match(partOf(incidentRows()[0], 'incident-count').title, /among the newest 50 of 80 incidents/, 'a capped page says the run count is among the incidents shown');
const loadOlder = findNode(diagnosticsBody, node => node.tagName === 'button' && node.textContent === 'Load older');
assert.equal(typeof loadOlder?.listeners.get('click'), 'function', 'a capped list offers older incidents');
loadOlder.listeners.get('click')();
assert.deepEqual(loaded, [100], 'Load older asks for the next page');
console.log('audit, cost and incident renderers: counts, exact incident drill-down, class tiles, recurrence rollup, detail panel and refresh passed');
