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
    // The dock lives on Tasks. Show its pane while app.js is stubbed.
    const dock = document.getElementById('side-dock');
    for (const pane of document.querySelectorAll('.tab-pane')) {
      pane.classList.toggle('active', pane.contains(dock));
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
const checkOrder = async (kind, barShowsRow = true) => {
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
  assert.equal(result.source, relay.fields.provider);
  assert.equal(result.target, target);
  assert.equal(result.fullRun, run);
  assert.ok(!result.message.includes('{"type"'), `relay rows must show the event kind, not the provider JSON: ${result.message}`);
  assert.ok(!result.statusBar.includes('{"type"'), `the status bar must not show raw provider JSON: ${result.statusBar}`);
  if (barShowsRow) {
    assert.equal(result.statusSource, relay.fields.provider);
    assert.equal(result.statusTarget, target);
    assert.ok(result.sameHtml, 'dock and status bar must use the same message/context order');
  }
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
  assert.ok(await agent.isVisible(), 'the log fixture must show the dock pane and Log dock');
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
  await checkOrder('item.completed', false);
  assert.ok((await page.locator('#log-statusbar-message').textContent()).startsWith('agent stderr'), 'hidden stdout relays must not replace the status bar');
  await page.click('[data-filter="err"]');
  assert.equal(await visible(), 1, 'severity filters must still select stderr errors');
  await page.click('[data-filter="all"]');
  assert.equal(await visible(), 2, 'all must preserve the independent stdout choice');

  await page.click('#log-follow-tail');
  emit('item.updated');
  await page.waitForFunction(() => document.getElementById('log-buffered-count').textContent.startsWith('1 buffered'));
  assert.ok((await page.locator('#log-statusbar-message').textContent()).startsWith('agent stderr'), 'paused, hidden stdout relays must not replace the status bar');
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

  // With stdout hidden, a relay arriving after an orbit WARN must leave the
  // bar on the WARN; showing agent output again redraws the newest relay.
  const bar = page.locator('#log-statusbar-message');
  await agent.click();
  fs.appendFileSync(logPath, `${JSON.stringify({
    timestamp: '2026-10-07T01:05:00Z', level: 'WARN', target: 'orbit.job.step_started',
    fields: { job_run_id: 'http-log-fixture', step_id: 'throttle-warning' },
  })}\n`);
  await page.waitForFunction(() => document.getElementById('log-statusbar-message').textContent.includes('throttle-warning'));
  assert.equal(await bar.getAttribute('data-level'), 'warn');
  const stdoutBefore = await stdoutRows.count();
  for (const kind of ['item.completed', 'item.started']) emit(kind);
  await page.waitForFunction((count) => document.querySelectorAll('#logInner .log-line[data-agent-stdout="true"]').length === count + 2, stdoutBefore);
  assert.ok((await bar.textContent()).includes('throttle-warning'), 'relays must leave the status bar on the WARN');
  assert.equal(await bar.getAttribute('data-level'), 'warn');
  assert.ok(!(await bar.textContent()).includes('{"type"'));
  await page.screenshot({ path: path.join(evidence, 'statusbar-warn-1440.png'), clip: { x: 0, y: 940, width: 1440, height: 60 } });
  await agent.click();
  assert.ok((await bar.textContent()).startsWith('item.started'), 'enabling agent output must redraw the newest relay');
  assert.ok(!(await bar.textContent()).includes('{"type"'), `relay line must be shortened to its kind: ${await bar.textContent()}`);
  await page.screenshot({ path: path.join(evidence, 'statusbar-relay-1440.png'), clip: { x: 0, y: 940, width: 1440, height: 60 } });

  // ORB-13483 states history dropped while paused with a gap notice. After an
  // ordinary 50-row snapshot, 201 paused events overflow the 200-event
  // buffer by one; resuming must keep that notice past the 200 retained rows
  // rather than trimming it with them. A later short pause has no stale count.
  const orbitLine = (step) => `${JSON.stringify({
    timestamp: '2026-10-07T01:10:00Z', level: 'INFO', target: 'orbit.job.step_started',
    fields: { job_run_id: 'http-log-fixture', step_id: step },
  })}\n`;
  fs.appendFileSync(logPath, Array.from({ length: 50 }, (_, i) => orbitLine(`snapshot-${i}`)).join(''));
  await page.reload();
  await boot();
  await page.waitForFunction(() => document.querySelectorAll('#logInner .log-line').length === 50);
  const buffered = page.locator('#log-buffered-count');
  await page.click('#log-follow-tail');
  fs.appendFileSync(logPath, Array.from({ length: 201 }, (_, i) => orbitLine(`paused-${i}`)).join(''));
  await page.waitForFunction(() => document.getElementById('log-buffered-count').textContent === '200 buffered · 1 discarded');
  await page.click('#log-follow-tail');
  const flushed = await page.evaluate(() => {
    const rows = [...document.querySelectorAll('#logInner .log-line')];
    const gaps = rows.filter(row => row.dataset.gap === 'true');
    return {
      logRows: rows.length - gaps.length,
      gaps: gaps.map(row => ({ text: row.querySelector('.m').textContent, visible: getComputedStyle(row).display !== 'none', index: rows.indexOf(row) })),
      newest: rows[0].textContent.includes('paused-200'),
    };
  });
  assert.equal(flushed.logRows, 200, `the row cap still holds after the flush: ${JSON.stringify(flushed)}`);
  assert.deepEqual(flushed.gaps, [{ text: '1 older line discarded while paused', visible: true, index: 200 }], `the discarded-history notice survives the flush below the newest 200 rows: ${JSON.stringify(flushed)}`);
  assert.ok(flushed.newest, 'the newest paused event leads the dock');
  assert.equal(await buffered.isVisible(), false, 'the paused count clears once the buffer is shown');
  await page.locator('#logInner .log-line[data-gap="true"]').scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(evidence, 'log-gap-after-resume-1440.png'), fullPage: false });
  await page.click('#log-follow-tail');
  fs.appendFileSync(logPath, orbitLine('short-pause'));
  await page.waitForFunction(() => document.getElementById('log-buffered-count').textContent.endsWith('buffered'));
  assert.equal(await buffered.textContent(), '1 buffered', 'a later short pause carries no stale discard count');
  assert.equal(await buffered.getAttribute('title'), '', 'nor a stale discard explanation');
  await page.click('#log-follow-tail');

  assert.deepEqual(errors, [], 'dashboard log module must not raise browser errors');
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, chromium: browser.version(), viewport: { width: 1440, height: 1000 }, initial, final, errors }, null, 2));
  console.log('PASS: real log API, 1440px message priority, path and target titles, snapshot/live/paused filtering, severity composition, reload persistence, and the paused-overflow gap notice kept after resume');
} finally {
  await browser.close();
}
