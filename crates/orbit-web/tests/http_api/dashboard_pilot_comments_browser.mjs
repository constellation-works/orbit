// Usage: node dashboard_pilot_comments_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/pilot-browser
// Capture the shipped task renderer and CSS with disposable fixture data.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';
import { task, message, assessment } from './pilot_comment_fixture.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const initializer = `
  import { renderTasks } from '/js/tasks.js';
  import { setWorkspace, setMultiWorkspace } from '/js/common.js';
  const task = ${JSON.stringify(task)};
  setMultiWorkspace(true);
  setWorkspace('ws_fixture');
  document.querySelector('.tab-pane[data-tab="tasks"]').classList.add('active');
  document.querySelector('.tab[data-tab="tasks"]').classList.add('active');
  renderTasks([task], {
    getActiveStatuses: () => new Set(['in-progress']), statusOrder: ['in-progress'],
    getSearchQuery: () => '', getTasks: () => [task],
  });
  document.querySelector('#tasks-body .row button.title').click();
  window.pilotReady = true;
`;
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname === '/api/distributed/claims') {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ claims: [] }));
    return;
  }
  const served = url.pathname === '/fixture.mjs'
    ? { data: initializer, type: 'text/javascript' } : dashboardFile(url.pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (url.pathname === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '<script type="module" src="/fixture.mjs"></script>');
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1400 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture`);
  await page.waitForFunction(() => window.pilotReady);
  await page.evaluate(() => document.fonts.ready);
  const card = page.locator('.comment-card');
  await card.scrollIntoViewIfNeeded();
  const fields = await card.locator('.comment-body').evaluate(body => Object.fromEntries(
    [...body.querySelectorAll('dt')].map(label => [label.textContent, label.nextElementSibling.textContent]),
  ));
  assert.equal(fields.Disposition, assessment.disposition);
  assert.equal(fields.Confidence, assessment.confidence);
  assert.equal(fields['Recommended crew'], `implementer → ${assessment.recommended_crew}`);
  assert.equal(fields['Recommended complexity'], `low → ${assessment.recommended_complexity}`);
  assert.equal(fields.Rationale, assessment.assessment_rationale);
  assert.ok(!(await card.locator('.comment-body').innerText()).includes('{"'));
  assert.equal(await card.locator('.comment-raw').isVisible(), false);
  await page.screenshot({ path: path.join(evidence, 'assessment-1440.png'), fullPage: true });
  await card.getByRole('button', { name: 'raw', exact: true }).click();
  assert.equal(await card.locator('.comment-raw').textContent(), message);
  assert.equal(await card.locator('.comment-raw').isVisible(), true);
  assert.equal(await card.locator('.comment-body').isVisible(), false);
  await page.screenshot({ path: path.join(evidence, 'raw-1440.png'), fullPage: true });
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, viewport: { width: 1440, height: 1400 }, chromium: browser.version(), fields, raw_preserved: true, errors }, null, 2));
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
