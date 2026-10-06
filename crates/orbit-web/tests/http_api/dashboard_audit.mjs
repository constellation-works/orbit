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
  get textContent() { return this.text + this.children.map(child => child.textContent ?? child).join(' '); }
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
const document = {
  activeElement: null,
  createElement: tag => new Node(tag),
  createTextNode: text => { const node = new Node('#text'); node.textContent = text; return node; },
  getElementById: id => ({ 'audit-summary-body': container, 'audit-summary-title': title, 'scoreboard-body': scoreboardBody, 'scoreboard-count': scoreboardCount })[id] || null,
  querySelectorAll: () => [],
};
const window = { location: { search: '?workspace=ws_fixture&window=24h', hash: '' } };
const context = vm.createContext({ URLSearchParams, window, document, console });
const common = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/common.js', import.meta.url), 'utf8'), { context });
const audit = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/audit.js', import.meta.url), 'utf8'), { context });
await common.link(() => { throw new Error('unexpected common dependency'); });
await audit.link(name => { assert.equal(name, './common.js'); return common; });
await audit.evaluate();
const payload = {
  window: '24h',
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
const cell = (row, field) => row.children[headers.findIndex(header => header.textContent === field)].textContent;
for (const [field, expected] of Object.entries({ tool: 'orbit.workflow.run.list', failed: '2', total: '9', rate: '22.2%', unexpected: '1', denied: '3' })) {
  assert.equal(cell(rows[0], field), expected, `mixed-call rendered ${field}`);
}
assert.equal(rows.length, 2, 'synthetic unnamed tools stay out of the table');
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
  agents: { codex: { failure_incidents: 2, failure_incident_events: 3 } },
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
scoreboard.namespace.renderScoreboard({
  ...scoreboardPayload, failure_incidents_truncated: null,
  agents: { codex: { failure_incidents: null, failure_incident_events: null } },
  coverage: { failure_incidents: { availability: 'unavailable' } },
});
assert.match(incidentRow().children[1].textContent, /unavailable/i, 'read failure still renders missing coverage rather than a zero');
assert.doesNotMatch(incidentRow().children[0].textContent, /capped/i);
console.log('audit and scoreboard renderers: callable counts, capped coverage, drill-down and refresh passed');
