// Optional screenshot capture from the isolated task-detail regression server.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const [modulePath, evidence, origin, taskId] = process.argv.slice(2);
const { chromium } = await import(pathToFileURL(modulePath).href);
fs.mkdirSync(evidence, { recursive: true });
const detail = await (await fetch(`${origin}/api/tasks/${taskId}?workspace=ws_http_fixture`)).json();
fs.writeFileSync(path.join(evidence, 'task-detail.json'), JSON.stringify(detail, null, 2));
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`${origin}/?workspace=ws_http_fixture#tasks`);
    await page.getByRole('button', { name: 'Pilot activity attribution 0', exact: true }).click();
    const pilot = page.locator('#task-detail-body .task-hand').filter({ hasText: 'piloted' });
    await pilot.waitFor();
    assert.match(await pilot.innerText(), new RegExp(detail.handled_by[0].crew));
    assert.ok((await pilot.getAttribute('title')).includes(detail.handled_by[0].model));
    assert.ok(await page.locator('#task-detail-body .task-hand').filter({ hasText: 'sol implemented' }).count());
    await page.evaluate(() => document.fonts.ready);
    await page.screenshot({ path: path.join(evidence, `task-detail-dark-${width}.png`), animations: 'disabled' });
  }
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'browser.json'), JSON.stringify({ passed: true, chromium: browser.version(), pilot: detail.handled_by[0], errors }, null, 2));
} finally {
  await browser.close();
}
