// Execute the shipped audit renderer and its common DOM helpers.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

class Node {
  constructor(tag) {
    this.tagName = tag; this.children = []; this.dataset = {}; this.className = '';
    this.text = ''; this.listeners = new Map(); this.style = { setProperty() {} };
    this.classList = { add: name => { this.className += ` ${name}`; } };
  }
  set textContent(value) { this.text = String(value); this.children = []; }
  set innerHTML(value) { assert.equal(value, ''); this.children = []; this.text = ''; }
  get textContent() { return this.text + this.children.map(child => child.textContent ?? child).join(' '); }
  get childNodes() { return this.children; }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  append(child) { this.appendChild(child); }
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
const scoreboardBody = new Node('div'); scoreboardBody.id = 'scoreboard-body';
const scoreboardCount = new Node('span');
const scoreboardAgentStrip = new Node('div');
const diagnosticsBody = new Node('div'); diagnosticsBody.id = 'diag-body';
const diagnosticsCount = new Node('span');
const auditScopeChips = new Node('div');
const auditFilterChips = new Node('div');
const document = {
  activeElement: null,
  createElement: tag => new Node(tag),
  createTextNode: text => { const node = new Node('#text'); node.textContent = text; return node; },
  getElementById: id => ({ 'audit-summary-body': container, 'audit-summary-title': title, 'scoreboard-body': scoreboardBody, 'scoreboard-count': scoreboardCount, 'scoreboard-agent-strip': scoreboardAgentStrip, 'diag-body': diagnosticsBody, 'diag-count': diagnosticsCount, 'audit-scope-chips': auditScopeChips, 'audit-filter': auditFilterChips })[id] || null,
  querySelectorAll: () => [],
};
const window = { location: { search: '?workspace=ws_fixture&window=24h', hash: '' } };
const requestedPaths = [];
const context = vm.createContext({
  URLSearchParams, window, document, console, AbortController, setTimeout, clearTimeout,
  fetch: async path => { requestedPaths.push(path); return { ok: true, json: async () => [] }; },
});
const common = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/common.js', import.meta.url), 'utf8'), { context });
const audit = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/audit.js', import.meta.url), 'utf8'), { context });
await common.link(() => { throw new Error('unexpected common dependency'); });
await audit.link(name => { assert.equal(name, './common.js'); return common; });
await audit.evaluate();
const diagnostics = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/diagnostics.js', import.meta.url), 'utf8'), { context });
await diagnostics.link(name => ({ './common.js': common, './audit.js': audit })[name]);
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

const scoreboard = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/scoreboard.js', import.meta.url), 'utf8'), { context });
await scoreboard.link(name => ({ './common.js': common, './audit.js': audit })[name]);
await scoreboard.evaluate();
const descendants = node => [node, ...node.children.flatMap(descendants)];
const incidentRow = () => descendants(scoreboardBody).find(node => node.dataset.key === 'scoreboard-Operations-failure_incidents');
const scoreboardPayload = {
  window: '24h', failure_incidents_scan_limit: 10000,
  agents: { codex: { tasks_created: 1, tool_calls: 5, failed_tool_calls: 1, failure_incidents: 2, failure_incident_events: 3 } },
};
scoreboard.namespace.renderScoreboard({
  ...scoreboardPayload, failure_incidents_truncated: true,
  coverage: { failure_incidents: { availability: 'partial', detail: 'Only the newest 10000 failure rows were scanned.' } },
});
assert.match(incidentRow().children[0].textContent, /capped/i, 'scoreboard incident column visibly qualifies capped counts');
assert.match(incidentRow().children[1].title, /10,000/);
assert.match(scoreboardBody.textContent, /Only the newest 10000/, 'partial coverage remains visible alongside populated rows');
scoreboard.namespace.renderScoreboard({
  ...scoreboardPayload, agents: { codex: { failure_incidents: 0, failure_incident_events: 0 } },
  failure_incidents_truncated: true,
  coverage: { failure_incidents: { availability: 'partial', detail: 'Capped sample.' } },
});
assert.ok(incidentRow(), 'a zero in a capped sample keeps the partial row visible');
assert.match(incidentRow().children[0].textContent, /capped/i);
scoreboard.namespace.renderScoreboard({
  ...scoreboardPayload, failure_incidents_truncated: false,
  coverage: { failure_incidents: { availability: 'observed' } },
});
assert.doesNotMatch(incidentRow().textContent, /capped/i, 'complete scoreboard refresh clears the qualifier');
assert.doesNotMatch(scoreboardBody.textContent, /Only the newest|Capped sample/);

// Drive the actual card/header/cell listeners, then reload the route and fetch
// Audit. This catches losing family or non-success filters at any UI boundary.
const familyTargets = [...descendants(scoreboardAgentStrip), ...descendants(scoreboardBody)].filter(node =>
  node.className.split(' ').includes('scoreboard-agent-card')
  || node.className.split(' ').includes('col-agent')
  || node.dataset.agent === 'codex');
assert.ok(familyTargets.some(node => node.tagName === 'button'), 'agent card is exercised');
assert.ok(familyTargets.some(node => node.tagName === 'th'), 'column header is exercised');
assert.ok(familyTargets.some(node => node.dataset.metric === 'failure_incidents'), 'incident cell is exercised');
for (const target of familyTargets) {
  const family = target.dataset.agent || (target.tagName === 'th'
    ? target.children[0].textContent
    : descendants(target).find(node => node.className === 'scoreboard-agent-name').textContent);
  target.listeners.get('click')();
  const familyRoute = new URLSearchParams(window.location.hash.split('?')[1]);
  assert.equal(familyRoute.get('agent_family'), family);
  assert.equal(familyRoute.has('role'), false, 'scoreboard uses the family predicate');
  if (target.dataset.metric === 'failure_incidents') {
    assert.equal(familyRoute.get('status'), 'non_success', 'incidents include failed and denied rows');
  } else if (['tools', 'failed_tool_calls'].includes(target.dataset.metric)) {
    assert.equal(familyRoute.get('status'), 'failure', 'raw failed calls retain their exact status');
  } else {
    assert.equal(familyRoute.has('status'), false, 'other metrics and agent views include all statuses');
  }
  audit.namespace.applyAuditHashQuery(familyRoute);
  assert.equal(audit.namespace.buildAuditHash(), window.location.hash, 'route survives reload');
  await audit.namespace.fetchAndRenderAudit();
  const request = new URLSearchParams(requestedPaths.at(-1).split('?')[1]);
  assert.equal(request.get('agent_family'), family, 'family reaches the Events API');
  assert.equal(request.get('workspace'), 'ws_fixture');
  assert.equal(request.get('since'), '24h');
  assert.equal(request.get('status'), familyRoute.get('status'));
}
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
scoreboard.namespace.renderScoreboard({
  ...scoreboardPayload, failure_incidents_truncated: null,
  agents: { codex: { failure_incidents: null, failure_incident_events: null } },
  coverage: { failure_incidents: { availability: 'unavailable' } },
});
assert.match(incidentRow().children[1].textContent, /unavailable/i, 'read failure still renders missing coverage rather than a zero');
assert.doesNotMatch(incidentRow().children[0].textContent, /capped/i);

const findNode = (root, predicate) => descendants(root).find(predicate);
const expandIncidentAndOpenRawEvents = () => {
  findNode(diagnosticsBody, node => node.className === 'incident-head').listeners.get('click')();
  const rawButton = findNode(diagnosticsBody, node => node.tagName === 'button'
    && node.className === 'chip' && node.textContent === 'Open raw audit events');
  assert.equal(typeof rawButton?.listeners.get('click'), 'function', 'expanded incident exposes its raw-audit action');
  rawButton.listeners.get('click')();
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
expandIncidentAndOpenRawEvents();
let incidentRoute = new URLSearchParams(window.location.hash.split('?')[1]);
assert.equal(incidentRoute.get('ids'), Array.from({ length: 65 }, (_, index) => index + 1).join(','));
assert.equal(incidentRoute.has('tool'), false, 'tool-less incidents route by exact event IDs');
assert.equal(incidentRoute.has('status'), false, 'exact event IDs retain stored failure rows');
assert.ok(incidentRoute.get('ids').split(',').length >= 65, 'exact route includes at least the incident event count');
audit.namespace.applyAuditHashQuery(incidentRoute);
assert.equal(new URLSearchParams(audit.namespace.buildAuditHash().split('?')[1]).get('ids'), incidentRoute.get('ids'));

findNode(diagnosticsBody, node => node.dataset.class === 'all').listeners.get('click')();
incidentPayload = {
  window: '24h', incident_count: 1, raw_failed_events: 1, total_events: 1,
  incidents: [{
    incident_id: 'denied-failure-row', class: 'denied', has_tool_identity: true,
    surface: 'orbit.task.add', actor: 'codex', event_count: 1, last_ts: '2026-10-07T09:00:00Z',
    events: [{ id: 9001, status: 'failure', tool: 'orbit.task.add', actor: 'codex', ts: '2026-10-07T09:00:00Z' }],
  }],
};
diagnostics.namespace.renderDiagnostics(diagnosticsContext);
expandIncidentAndOpenRawEvents();
incidentRoute = new URLSearchParams(window.location.hash.split('?')[1]);
assert.equal(incidentRoute.get('ids'), '9001', 'denied incident points to its stored failure row');
assert.equal(incidentRoute.has('status'), false, 'denied classification does not rewrite the stored status');
assert.equal(incidentRoute.has('tool'), false, 'exact incident rows are not narrowed by surface');
console.log('audit, scoreboard and incident renderers: counts, exact incident drill-down and refresh passed');
