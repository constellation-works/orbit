// Execute the shipped audit renderer and its common DOM helpers.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

class Node {
  constructor(tag) {
    this.tagName = tag; this.children = []; this.dataset = {}; this.className = '';
    this.text = ''; this.listeners = new Map(); this.style = {};
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
  contains(child) { return this.children.some(node => node === child || node.contains(child)); }
}
const container = new Node('div'); container.id = 'audit-summary-body';
const title = new Node('h3');
const document = {
  activeElement: null,
  createElement: tag => new Node(tag),
  getElementById: id => id === container.id ? container : id === 'audit-summary-title' ? title : null,
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
console.log('audit renderer: mixed counts, denied-only rows, rate, drill-down and refresh passed');
