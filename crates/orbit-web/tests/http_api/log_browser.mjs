// Invoked by log::dashboard_log_message_priority_and_agent_filter against its
// isolated HTTP server. Every rendered event comes from the Rust log API.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [modulePath, evidencePath, origin, logPath] = process.argv.slice(2);
const { chromium } = await import(pathToFileURL(modulePath).href);
const evidence = path.resolve(evidencePath);
fs.mkdirSync(evidence, { recursive: true });
const inputs = fs.readFileSync(logPath, 'utf8').trim().split('\n').map(line => JSON.parse(line));
const relay = inputs.at(-1);
const target = relay.target;
const run = relay.fields.job_run_id;
const errors = [];
const browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
page.on('pageerror', error => errors.push(String(error)));
// Initialize only the log dock; all markup, CSS and log traffic use the real
// server. Other panels need no fixture state for this boundary check.
await page.route('**/static/app.js', route => route.fulfill({ contentType: 'text/javascript', body: '' }));
const boot = async () => {
  await page.evaluate(async () => {
    for (const pane of document.querySelectorAll('.tab-pane')) {
      pane.classList.toggle('active', pane.dataset.tab === 'tasks');
    }
    const { initLogTail } = await import('/static/js/log-tail.js');
    initLogTail();
  });
  await page.waitForFunction(() => document.querySelectorAll('#logInner .log-line').length >= 3);
};
const emit = (kind) => fs.appendFileSync(logPath, `${JSON.stringify({
  ...relay, fields: { ...relay.fields, line: JSON.stringify({ type: kind, item: { type: 'command_execution' } }) },
})}\n`);
const visible = async () => page.locator('#logInner .log-line:visible').count();
const agent = page.locator('#log-show-agent');
const stdoutRows = page.locator('#logInner .log-line[data-agent-stdout="true"]');
const checkOrder = async (kind) => {
  const result = await page.evaluate(() => {
    const row = document.querySelector('#logInner .log-line[data-agent-stdout="true"]');
    const message = row.querySelector('.m');
    const bar = document.getElementById('log-statusbar-message');
    const source = row.querySelector('.ag');
    const sbSource = document.getElementById('log-statusbar-source');
    const stream = document.querySelector('#side-dock .log-stream').getBoundingClientRect();
    const summary = message.querySelector('code');
    const range = document.createRange();
    range.selectNodeContents(summary);
    const runBox = range.getBoundingClientRect();
    return {
      first60: `${row.querySelector('.t').textContent} ${source.textContent} ${message.textContent}`.slice(0, 60),
      message: message.textContent,
      statusBar: bar.textContent,
      sameHtml: message.innerHTML === bar.innerHTML,
      source: source.textContent, target: source.title,
      statusSource: sbSource.textContent, statusTarget: sbSource.title,
      run: summary.textContent, fullRun: summary.title,
      summaryFits: runBox.right <= stream.right && runBox.left >= stream.left,
      rowVisible: getComputedStyle(row).display !== 'none',
      sourceFits: source.scrollWidth <= source.clientWidth,
      paths: [...message.querySelectorAll('code[title]')].map(node => ({ text: node.textContent, full: node.title })),
      pageOverflow: document.documentElement.scrollWidth > innerWidth + 1,
    };
  });
  assert.ok(result.first60.includes(kind) && result.first60.includes('jrun-…-c12'), `meaning must lead the rendered row at 1440px: ${JSON.stringify(result)}`);
  assert.ok(!result.first60.includes('cwd='), 'bulky cwd must follow the event summary');
  if (result.rowVisible) {
    assert.ok(result.summaryFits, `compact run must be visible without wrapping or scrolling: ${JSON.stringify(result)}`);
    assert.ok(result.sourceFits, 'last target segment must fit its column');
  }
  assert.equal(result.source, 'supervisor');
  assert.equal(result.target, target);
  assert.equal(result.statusSource, 'supervisor');
  assert.equal(result.statusTarget, target);
  assert.equal(result.fullRun, run);
  assert.ok(result.sameHtml, 'dock and status bar must use the same message/context order');
  assert.ok(result.paths.some(value => value.full === relay.fields.cwd && value.text === `${run}/src`), 'worktree path must shorten to the run while retaining its full title');
  assert.ok(!result.pageOverflow, 'log context must scroll inside its dock');
  return result;
};

try {
  await page.goto(`${origin}/#tasks`);
  await boot();
  await page.evaluate(async () => {
    const { setDockMode } = await import('/static/js/log-tail.js');
    setDockMode('log');
  });
  assert.ok(await agent.isVisible(), 'the log fixture must select the Tasks pane and Log dock');
  const initial = await checkOrder('item.started');
  const home = await page.locator('#logInner .log-line[data-level="error"] .m code[title]').allTextContents();
  assert.ok(home.includes('~/workspace/project'), `home path must shorten: ${home}`);
  await page.screenshot({ path: path.join(evidence, 'log-1440.png'), fullPage: true });
  await agent.click();
  assert.equal(await agent.getAttribute('aria-pressed'), 'false');
  assert.equal(await visible(), 2, 'one control must hide only stdout relays');
  assert.equal(await page.locator('#logInner .log-line[data-level="error"]:visible').count(), 1, 'stderr must remain visible');
  emit('item.completed');
  await page.waitForFunction(() => document.querySelectorAll('#logInner .log-line[data-agent-stdout="true"]').length === 2);
  assert.equal(await visible(), 2, 'the saved choice must also hide live stdout');
  await checkOrder('item.completed');
  await page.click('[data-filter="err"]');
  assert.equal(await visible(), 1, 'severity filters must still select stderr errors');
  await page.click('[data-filter="all"]');
  assert.equal(await visible(), 2, 'all must preserve the independent stdout choice');

  await page.click('#log-follow-tail');
  emit('item.updated');
  await page.waitForFunction(() => document.getElementById('log-buffered-count').textContent.startsWith('1 buffered'));
  assert.ok((await page.locator('#log-statusbar-message').textContent()).startsWith('item.updated'), 'paused relays must still update the status bar');
  await page.click('#log-follow-tail');
  assert.equal(await stdoutRows.count(), 3);
  assert.equal(await visible(), 2, 'flushed stdout must respect the filter');
  await agent.click();
  assert.equal(await visible(), 5, 'one click must restore hidden snapshot, live and paused stdout');
  await checkOrder('item.updated');
  await page.click('#log-wrap-lines');
  assert.ok((await stdoutRows.first().locator('.m').textContent()).startsWith('item.updated'));
  await page.click('#log-wrap-lines');

  // Changing the dock mode must preserve the visibility preference; reload
  // proves that the preference is read, not merely held in module state.
  await agent.click();
  await page.evaluate(async () => {
    const { setDockMode } = await import('/static/js/log-tail.js');
    setDockMode('drain');
    setDockMode('log');
  });
  assert.equal(await page.evaluate(() => JSON.parse(localStorage.getItem('orbit.dashboard.logPanel')).showAgent), false);
  await page.reload();
  await boot();
  assert.equal(await agent.getAttribute('aria-pressed'), 'false');
  assert.equal(await page.locator('#side-dock').getAttribute('data-mode'), 'log');
  assert.equal(await visible(), 2, 'reload must hide snapshot stdout using localStorage');
  await agent.click();
  assert.equal(await visible(), 5);
  const final = await checkOrder('item.updated');
  assert.deepEqual(errors, [], 'dashboard log module must not raise browser errors');
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, chromium: browser.version(), viewport: { width: 1440, height: 1000 }, initial, final, errors }, null, 2));
  console.log('PASS: real log API, 1440px message priority, path and target titles, snapshot/live/paused filtering, severity composition, and reload persistence');
} finally {
  await browser.close();
}
