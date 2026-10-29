// Runs the public auto-task renderer in Chromium and captures filter behavior.
import { pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const harness = `
const { setWorkspace } = await import('/js/common.js');
const shared = await import('/js/operations-shared.js');
const { renderAutoTasks } = await import('/js/operations-auto-tasks.js');
const allowed = { authorized: true, reason: null };
const capabilities = { auto_task_toggle: allowed, auto_task_mint: allowed, auto_task_schedule: allowed };
const definitions = Array.from({ length: 20 }, (_, index) => {
  const name = 'definition-' + (index + 1);
  return {
    name, enabled: true, effective_enabled: true, dedupe: 'always',
    template: { title: name, crew: 'fixture', priority: 'medium', status: 'backlog' },
    template_summary: name, schedule: { cron: '0 9 * * *' }, schedule_summary: 'daily at 09:00',
    next_evaluation: { state: 'scheduled', at: '2026-10-29T09:00:00Z' },
  };
});
const payload = {
  cron_zone: { name: 'America/Los_Angeles', offset_seconds: -25200 }, capabilities, definitions,
  unconditional_mint_warning: "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.",
};
shared.initOperations({ getWorkspaces: () => [{ id: 'one', name: 'one', status: 'active' }] });
setWorkspace('one');
for (let node = document.getElementById('auto-tasks-body'); node; node = node.parentElement) node.hidden = false;
document.querySelector('.tab-pane[data-tab="operations"]')?.classList.add('active');
renderAutoTasks(payload);
globalThis.autoTaskFilterReview = {
  refresh: () => renderAutoTasks(payload),
};
globalThis.autoTaskFilterReady = true;
`;

const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = name === '/auto-task-filter.mjs'
    ? { data: harness, type: 'text/javascript' }
    : dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', served.type);
  res.end(data);
});

await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, timezoneId: 'America/Los_Angeles' });
  const pageErrors = [];
  page.on('pageerror', error => { pageErrors.push(String(error)); console.error(error); });
  await page.goto(`http://127.0.0.1:${server.address().port}/#operations/auto-tasks`);
  await page.addScriptTag({ type: 'module', url: '/auto-task-filter.mjs' });
  await page.waitForFunction(() => globalThis.autoTaskFilterReady, undefined, { timeout: 15000 });

  const filter = page.locator('#auto-tasks-filter-input');
  const cardCount = () => page.locator('#auto-tasks-body .auto-task-card').count();
  if (await cardCount() !== 8) throw new Error(`initial group should be bounded to 8 cards, found ${await cardCount()}`);
  const reveal = page.locator('#auto-tasks-body .auto-tasks-reveal');
  if (!await reveal.textContent().then(text => text.includes('Show all 20 On a schedule definitions'))) {
    throw new Error(`missing bounded-group reveal action: ${await reveal.textContent()}`);
  }
  await reveal.click();
  if (await cardCount() !== 20) throw new Error(`group reveal should show 20 cards, found ${await cardCount()}`);
  await page.locator('#auto-tasks-body .auto-tasks-reveal').click();
  if (await cardCount() !== 8) throw new Error(`group collapse should restore 8 cards, found ${await cardCount()}`);

  await filter.fill('definition-20');
  if (await cardCount() !== 1 || !await page.locator('#auto-tasks-body .auto-task-card strong').textContent().then(text => text === 'definition-20')) {
    throw new Error('filter should reveal only definition-20');
  }
  await filter.evaluate(input => input.setSelectionRange(0, 0));
  await filter.press('x');
  let edit = await filter.evaluate(input => ({ value: input.value, start: input.selectionStart, end: input.selectionEnd }));
  if (edit.value !== 'xdefinition-20' || edit.start !== 1 || edit.end !== 1) {
    throw new Error(`typing at the start should leave the caret after x: ${JSON.stringify(edit)}`);
  }
  await filter.press('y');
  edit = await filter.evaluate(input => ({ value: input.value, start: input.selectionStart }));
  if (edit.value !== 'xydefinition-20' || edit.start !== 2) {
    throw new Error(`the next character should follow x at the start: ${JSON.stringify(edit)}`);
  }

  await filter.fill('definition-20');
  await filter.evaluate(input => input.setSelectionRange(10, 10));
  await filter.press('q');
  edit = await filter.evaluate(input => ({ value: input.value, start: input.selectionStart }));
  if (edit.value !== 'definitionq-20' || edit.start !== 11) {
    throw new Error(`typing in the middle should preserve its caret: ${JSON.stringify(edit)}`);
  }
  await filter.press('r');
  edit = await filter.evaluate(input => ({ value: input.value, start: input.selectionStart }));
  if (edit.value !== 'definitionqr-20' || edit.start !== 12) {
    throw new Error(`the next middle character should follow q: ${JSON.stringify(edit)}`);
  }

  await filter.evaluate(input => {
    input.value = 'definition-20';
    input.setSelectionRange(4, 10, 'backward');
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
  let selection = await filter.evaluate(input => ({
    value: input.value, focused: document.activeElement === input,
    start: input.selectionStart, end: input.selectionEnd, direction: input.selectionDirection,
  }));
  if (selection.value !== 'definition-20' || !selection.focused || selection.start !== 4 || selection.end !== 10 || selection.direction !== 'backward') {
    throw new Error(`filter input should retain its browser selection: ${JSON.stringify(selection)}`);
  }
  await page.evaluate(() => globalThis.autoTaskFilterReview.refresh());
  selection = await filter.evaluate(input => ({
    value: input.value, focused: document.activeElement === input,
    start: input.selectionStart, end: input.selectionEnd, direction: input.selectionDirection,
  }));
  if (!selection.focused || selection.start !== 4 || selection.end !== 10 || selection.direction !== 'backward') {
    throw new Error(`payload refresh should preserve filter focus and selection: ${JSON.stringify(selection)}`);
  }
  await filter.press('x');
  edit = await filter.evaluate(input => ({ value: input.value, start: input.selectionStart }));
  if (edit.value !== 'defix-20' || edit.start !== 5) {
    throw new Error(`typing after refresh should replace the selected text: ${JSON.stringify(edit)}`);
  }

  await page.getByRole('button', { name: 'Clear filter' }).click();
  const clearState = { value: await filter.inputValue(), focused: await filter.evaluate(input => document.activeElement === input) };
  if (clearState.value !== '' || !clearState.focused) {
    throw new Error(`clearing should clear the query and return focus to the input: ${JSON.stringify(clearState)}`);
  }
  if (await cardCount() !== 8) throw new Error(`clearing should restore the bounded 8 cards, found ${await cardCount()}`);

  const otherControl = page.locator('.tab[data-tab="operations"]');
  await otherControl.focus();
  await page.evaluate(() => globalThis.autoTaskFilterReview.refresh());
  if (!await otherControl.evaluate(control => document.activeElement === control)) {
    throw new Error('refresh should not steal focus from another dashboard control');
  }

  await filter.fill('definition-20');
  await filter.evaluate(input => input.setSelectionRange(0, 0));
  await page.locator('#auto-tasks-body').screenshot({ path: path.join(evidence, 'auto-task-filter-dark-1440.png'), animations: 'disabled' });
  await page.setViewportSize({ width: 375, height: 812 });
  await page.locator('#auto-tasks-body').screenshot({ path: path.join(evidence, 'auto-task-filter-dark-375.png'), animations: 'disabled' });

  if (pageErrors.length) throw new Error(`page errors during the auto-task filter scenario: ${pageErrors.join(' | ')}`);
  await page.close();
  console.log(`PASS: auto-task filter typing, selection and refresh in Chromium; screenshots: ${evidence}`);
} finally {
  await browser?.close();
  server.close();
}
