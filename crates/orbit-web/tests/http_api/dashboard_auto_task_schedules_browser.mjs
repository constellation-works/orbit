// Usage: node dashboard_auto_task_schedules_browser.mjs <playwright/index.mjs> <evidence-dir>
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const server = http.createServer((req, res) => {
  const file = dashboardFile(new URL(req.url, 'http://fixture').pathname);
  if (!file) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', file.type); res.end(file.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.route('**/api/**', route => {
    const url = new URL(route.request().url());
    let data = {};
    if (url.pathname === '/api/workspaces') data = [{ id: 'one', name: 'one', status: 'active', is_default: true }];
    else if (url.pathname === '/api/tasks' || url.pathname === '/api/job-runs') data = { items: [], total: 0 };
    else if (url.pathname === '/api/routines') data = { routines: [], clock: {} };
    else if (url.pathname === '/api/auto-tasks') data = { definitions: [
      { name: 'three-days', enabled: true, schedule: { every_minutes: 4320 }, schedule_summary: 'every 4320 minutes' },
      { name: 'seven-days', enabled: true, schedule: { every_minutes: 10080 }, schedule_summary: 'every 10080 minutes' },
      { name: 'two-hours', enabled: true, schedule: { every_minutes: 120 }, schedule_summary: 'every 120 minutes' },
      { name: 'unusual-cron', enabled: true, schedule: { cron: '0 9 1 * *' }, schedule_summary: 'cron 0 9 1 * *' },
      { name: 'daily-cron', enabled: true, schedule: { cron: '0 9 * * *' }, schedule_summary: 'cron 0 9 * * *' },
      ...Array.from({ length: 40 }, (_, i) => ({ name: `other-${i}`, enabled: i < 35, schedule: { every_minutes: 17 }, schedule_summary: 'every 17 minutes' })),
    ] };
    return route.fulfill({ json: data });
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=one#operations/auto-tasks`);
  await page.waitForSelector('.auto-task-card');
  await page.evaluate(async () => {
    await document.fonts.ready;
    await Promise.all(document.getAnimations()
      .filter(animation => animation.effect.getTiming().iterations !== Infinity)
      .map(animation => animation.finished.catch(() => {})));
  });
  await page.screenshot({ path: path.join(evidence, 'auto-tasks-dark-1440.png') });
  const row = name => page.locator('.auto-task-card').filter({ has: page.getByText(name, { exact: true }) });
  for (const [name, expected] of [['three-days', 'every 3 days'], ['seven-days', 'every 7 days'], ['two-hours', 'every 2 hours']]) {
    assert.equal(await row(name).locator('.operation-cell[data-label="Trigger"] .operation-cell-main').textContent(), expected);
  }
  const unusual = await row('unusual-cron').locator('.operation-cell[data-label="Trigger"]').innerText();
  assert.equal((unusual.match(/0 9 1 \* \*/g) || []).length, 1);
  assert.ok(unusual.includes('skip if open'));
  assert.ok((await row('daily-cron').innerText()).includes('0 9 * * *'));
  const label = page.locator('#operations-subtabs [data-subtab="auto-tasks"] .rail-label');
  assert.ok(await label.evaluate(node => node.scrollWidth <= node.clientWidth), 'Auto-tasks rail label fits');
  await page.setViewportSize({ width: 375, height: 812 });
  await page.screenshot({ path: path.join(evidence, 'auto-tasks-dark-375.png') });
  console.log('Auto-task schedule and rail browser checks passed');
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
