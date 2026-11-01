// Usage: node dashboard_decision_label_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/decision-label-browser
// ORB-20834: a decision option whose label is a full sentence wraps inside its button in the
// blocked-task card, in the task dock and the full task page, at 1440 and 375 px. The card must
// not scroll sideways, nothing may leave the pane, and every option's consequence stays readable.
// Before the fix the button stayed on one line and its column took the pane's width.
// Runs against disposable API responses; no orbit binary or real home is involved.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const { dashboardFile } = await import(process.env.ORBIT_DASHBOARD_FIXTURE_MODULE || '../../src/tests/dashboard_static.mjs');
const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const at = '2026-10-20T15:00:00Z';
const longLabel = 'Split the sweep: this task only pins the commit, partitions the tree, and selects the two modules; a follow-up task reads each partition and files its own task.';
const task = {
  id: 'BLOCK-5', title: 'Decision with a sentence-length option', status: 'blocked',
  priority: 'medium', type: 'feature', complexity: 'medium', crew: 'sol',
  description: 'Keep delivery recoverable when a worker needs operator attention.',
  acceptance_criteria: ['Required validation completes on the worker'],
  created_at: at, updated_at: at, status_transitions: [], history: [], artifacts: [],
  comments: [
    { at: '2026-10-19T12:00:00Z', by: 'system', message: 'An older block has already been resolved.' },
    { at, by: 'system', message: 'Final recovery requires operator attention.' },
  ],
  block: { cause: 'escalation', reason: 'Required check test-shard-2 failed on head abc123.', at, run_id: 'jrun-block-fixture', comment_index: 1, next_action: null },
  open_decisions: [{
    id: 'DR-BLOCK-5', gate: 'block_triage', expires_at: '2099-01-01T00:00:00Z', answers: ['1', '2', '3'],
    summary: 'A retired API still backs criterion 2, so the test fails on every retry.',
    options: [
      { id: '1', label: longLabel, consequence: 'One task per module is filed and this task closes once they exist.', recommended: true },
      { id: '2', label: 'Restore the v1 API', consequence: 'The v1 API comes back and the task is re-piloted with criterion 2 kept as written.', recommended: false },
      { id: '3', label: 'Drop criterion 2', consequence: 'The retired API stays retired; the task returns to backlog without that criterion.', recommended: false },
    ],
  }],
};
const fixture = `
globalThis.EventSource = class { close() {} };
const tasks = ${JSON.stringify([task])};
globalThis.fetch = async input => {
  const url = new URL(input, location.href);
  const json = body => new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
  if (url.pathname === '/api/workspaces') return json([{ id: 'one', name: 'one', is_default: true, status: 'active' }]);
  if (url.pathname === '/api/hosts') return json({ hosts: [{ name: 'owner', machine_id: 'owner-id', local: true }] });
  if (url.pathname === '/api/tasks') return json({ items: tasks, total: tasks.length, limit: 50, truncated: false });
  if (url.pathname === '/api/tasks/locks') return json({ by_task: [], total_locked: 0, total_tasks: 0 });
  if (url.pathname === '/api/crews') return json({ default_crew: 'sol', crews: [{ name: 'sol' }] });
  if (url.pathname === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: { drain_phase: 'idle' }, tasks: [] });
  const id = decodeURIComponent(url.pathname.split('/').pop());
  if (url.pathname.startsWith('/api/tasks/') && tasks.some(item => item.id === id)) return json(tasks.find(item => item.id === id));
  return json({ items: [], total: 0 });
};
await import('/app.js');
`;
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  const data = name === '/' ? served.data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, `<script type="module">${fixture}</script>`) : served.data;
  res.setHeader('content-type', served.type);
  res.end(data);
});

// Geometry of the callout against its pane: scrolling ancestors, descendants that leave the
// pane, and each option's button and consequence boxes.
function measureCallout() {
  const box = node => { const b = node.getBoundingClientRect(); return { left: b.left, right: b.right, width: b.width, height: b.height }; };
  const card = document.querySelector('.task-block-callout');
  const pane = document.querySelector('#task-detail-pane');
  const scrollers = [];
  for (let node = card; node && node !== document.body; node = node.parentElement) {
    const overflowX = getComputedStyle(node).overflowX;
    if (node.scrollWidth > node.clientWidth + 1 && /(auto|scroll|hidden)/.test(overflowX)) scrollers.push(node.className || node.tagName);
  }
  const paneRight = pane.getBoundingClientRect().right;
  const escapes = [...card.querySelectorAll('*')]
    .filter(node => node.getBoundingClientRect().width > 0 && node.getBoundingClientRect().right > paneRight + 1)
    .map(node => `${node.tagName.toLowerCase()}.${node.className}`);
  const options = [...card.querySelectorAll('.decision-option')].map(option => {
    const button = option.querySelector('.decision-answer');
    const consequence = option.querySelector('.decision-consequence');
    return {
      label: button.innerText,
      button: { ...box(button), scrollWidth: button.scrollWidth, clientWidth: button.clientWidth },
      consequence: { ...box(consequence), text: consequence.innerText },
    };
  });
  return { scrollers, escapes, options, pane: box(pane), paneRight };
}

const browser = await chromium.launch({ headless: true });
const results = [];
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
try {
  for (const width of [1440, 375]) {
    for (const [view, hash] of [['dock', '#tasks?open=BLOCK-5'], ['page', '#tasks/BLOCK-5']]) {
      const page = await browser.newPage({ viewport: { width, height: width === 375 ? 812 : 900 } });
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=one${hash}`);
      const card = page.locator('.task-block-callout');
      await card.locator('.decision-card').waitFor({ state: 'visible' });
      await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== 'running' || !animation.effect?.target?.closest?.('#task-detail-pane')));
      const m = await page.evaluate(measureCallout);
      const where = `${view} at ${width}px`;
      assert.deepEqual(m.scrollers, [], `The decision card must not scroll sideways (${where})`);
      assert.deepEqual(m.escapes, [], `Nothing in the decision card may leave the task pane (${where})`);
      assert.equal(m.options.length, 3, `Each option renders (${where})`);
      const [long] = m.options;
      assert.equal(long.label, longLabel, `The sentence-length option comes first (${where})`);
      assert.ok(long.button.height > 30, `The sentence-length label wraps onto more than one line (${where}: ${long.button.height}px)`);
      for (const option of m.options) {
        assert.ok(option.button.scrollWidth <= option.button.clientWidth + 1, `An option label stays inside its button (${where}: ${option.label})`);
        assert.ok(option.consequence.text.length > 0, `Each option carries its consequence (${where})`);
        assert.ok(option.consequence.width >= 120, `Each consequence keeps room to read (${where}: ${option.consequence.width}px)`);
        assert.ok(option.consequence.right <= m.paneRight + 1, `Each consequence is inside the pane (${where})`);
      }
      assert.deepEqual(errors, [], `The page raises no errors (${where})`);
      await page.screenshot({ path: path.join(evidence, `decision-label-${view}-dark-${width}.png`) });
      results.push({ view, width, label: long.button.height, consequenceWidths: m.options.map(option => Math.round(option.consequence.width)) });
      await page.close();
    }
  }
  fs.writeFileSync(path.join(evidence, 'results.json'), JSON.stringify({ widths: [1440, 375], views: ['dock', 'page'], results }, null, 2));
} finally {
  await browser.close();
  await new Promise(resolve => server.close(resolve));
}
