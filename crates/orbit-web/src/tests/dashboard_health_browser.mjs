// Behavior scenarios used by the full dashboard Chromium harness.
export async function assertHealthTriage(page, evidence, path) {
  await page.evaluate(() => {
    globalThis.healthPreviousFetch = globalThis.fetch;
    globalThis.healthQueries = [];
    globalThis.fetch = async (input, options) => {
      const url = new URL(input, location.href);
      const ok = payload => ({ ok: true, status: 200, json: async () => payload });
      const ts = new Date().toISOString();
      const window = url.searchParams.get('since');
      if (url.pathname === '/api/audit/incidents') {
        globalThis.healthQueries.push({ view: 'incidents', window, class: url.searchParams.get('class') });
        return ok({ window, incident_count: 2, raw_failed_events: 2, total_events: 10, affected_run_count: 2,
          failure_categories: { unexpected: { incidents: 1, raw_events: 1, affected_runs: 1 } },
          incidents_by_class: { expected: 1, unexpected: 1 }, raw_events_by_class: { expected: 1, unexpected: 1 },
          incidents: [
            { incident_id: 'expected-fixture', class: 'expected', message: 'Expected fixture', event_count: 1, last_ts: ts },
            { incident_id: 'unexpected-fixture', class: 'unexpected', message: 'Unexpected fixture', event_count: 1, last_ts: ts },
          ].filter(incident => !url.searchParams.get('class') || incident.class === url.searchParams.get('class')) });
      }
      if (url.pathname === '/api/diagnostics/errors') {
        globalThis.healthQueries.push({ view: 'errors', window });
        return ok([
          { ts, event_id: 'process-health', source: 'process', target: 'orbit.job.step_finished', job_run: 'jrun-health', step: 'fulfil', message: 'Candidate failed: invalid output' },
          { ts, event_id: 'fatal-health', source: 'agent-stderr', target: 'provider_auth', message: 'Authentication failed' },
          { ts, event_id: 'stderr-health', source: 'agent-stderr', target: 'model_manager, apply_patch', message: 'Failed to find expected lines in /Users/fixture/workspace/orbit/.orbit/state/worktrees/orbit-jrun-fixture/src/lib.rs' },
        ]);
      }
      if (url.pathname === '/api/diagnostics/metrics') {
        globalThis.healthQueries.push({ view: 'metrics', window });
        return ok([{ ts, step: 'implement_one', actor_identity: 'fixture', token_usage: 1379713, tool_invocations: 20 }]);
      }
      if (url.pathname === '/api/tasks/completion-by-complexity') return ok({ by_complexity: [{ complexity: 'unset', total: 12345, statuses: [{ status: 'done', count: 1234 }] }] });
      if (url.pathname === '/api/diagnostics/implement_one') return ok({ implement_one_by_complexity: [{ complexity: 'unset', n: 532, actors: [{ actor: 'long/actor/identity/for/layout', n: 532, avg: 123456, p50: 98765, p95: 345678 }] }] });
      return globalThis.healthPreviousFetch(input, options);
    };
  });
  const select = async (view, window) => {
    await page.evaluate(async ({ view, window }) => {
      const common = await import('/js/common.js');
      common.setWorkspace('one');
      common.setWindow(window);
      (await import('/js/router.js')).setActiveTab(`diagnostics/${view}`);
      document.getElementById('refresh-btn').click();
    }, { view, window });
    await page.waitForFunction(() => document.getElementById('diag-body').getAttribute('aria-busy') !== 'true');
  };
  try {
    for (const width of [1440, 720, 390]) {
      await page.setViewportSize({ width, height: 1000 });
      for (const window of ['24h', '7d']) {
        await select('incidents', window);
        await page.locator('.incident-row.unexpected').waitFor();
        if (await page.locator('.incident-row.expected').count()) throw new Error('Expected negative paths must start filtered out');
        await page.locator('.incident-class-chip[data-class="all"]').click();
        await page.waitForFunction(() => document.querySelectorAll('.incident-row').length === 2);
        if (await page.locator('.incident-row').count() !== 2 || !(await page.locator('.incident-row').first().getAttribute('class')).includes('unexpected')) throw new Error('All incidents must put unexpected first');
        await page.locator('.incident-class-chip[data-class="expected"]').click();
        await page.locator('.incident-row.expected').waitFor();
        if (await page.locator('.incident-row').count() !== 1 || !await page.locator('.incident-row.expected').isVisible()) throw new Error('Class chip must filter the list');
        await page.locator('.incident-class-chip[data-class="unexpected"]').click();
        await page.locator('.incident-row.unexpected').waitFor();
        await page.screenshot({ path: path.join(evidence, `health-incidents-${window}-${width}.png`) });

        await select('errors', window);
        await page.locator('#diag-body .c-target').first().waitFor();
        const group = page.locator('.diag-stderr-group');
        if (await page.locator('.diag-process-group tbody tr').count() !== 2) throw new Error('Unknown stderr must remain visible beside process failures');
        if (await group.getAttribute('open') !== null) throw new Error('Agent stderr must start collapsed');
        if (!(await page.locator('#diag-body .c-target').first().textContent()).includes('orbit.job.step_finished')) throw new Error('Process target must be visible');
        await group.locator('summary').click();
        const message = group.locator('.c-message');
        if (!(await message.textContent()).includes('…/src/lib.rs') || (await message.textContent()).includes('/Users/')) throw new Error('Worktree path must be shortened');
        if (!(await message.getAttribute('title')).includes('/Users/fixture/')) throw new Error('Full error must remain in tooltip');
        if (!(await page.locator('#diag-count').textContent()).includes(window)) throw new Error('Errors header must match selected range');
        await page.screenshot({ path: path.join(evidence, `health-errors-${window}-${width}.png`) });
        await group.locator('summary').click();

        await select('metrics', window);
        await page.locator('#diag-body .c-token_usage').waitFor();
        if (await page.locator('#diag-body .c-token_usage').textContent() !== '1,379,713') throw new Error('Tokens must be grouped');
        if (!(await page.locator('#diag-count').textContent()).includes(window)) throw new Error('Metrics header must match selected range');
        const layout = await page.locator('#diag-implement-one-body').evaluate(body => ({
          titles: [...body.querySelectorAll('.card-title')].map(title => getComputedStyle(title).textTransform),
          clipped: [...body.querySelectorAll('.summary-table th, .summary-table td')].filter(cell => cell.scrollWidth > cell.clientWidth + 1).map(cell => cell.textContent),
        }));
        if (layout.titles.some(transform => transform !== 'none') || layout.clipped.length) throw new Error(`Summary must preserve titles and fit cells: ${JSON.stringify(layout)}`);
        await page.screenshot({ path: path.join(evidence, `health-metrics-${window}-${width}.png`) });
      }
    }
    const queries = await page.evaluate(() => globalThis.healthQueries);
    if (!queries.some(query => query.view === 'incidents' && query.class === 'unexpected')) throw new Error('Unexpected class must be selected before the API row limit');
    for (const view of ['incidents', 'errors', 'metrics']) for (const window of ['24h', '7d']) {
      if (!queries.some(query => query.view === view && query.window === window)) throw new Error(`${view} must request ${window}`);
    }
  } finally {
    await page.evaluate(() => { globalThis.fetch = globalThis.healthPreviousFetch; delete globalThis.healthPreviousFetch; });
  }
}

// Standalone entry point for Health work without running unrelated layout scenarios.
if (process.argv[1] && (await import('node:url')).fileURLToPath(import.meta.url) === process.argv[1]) {
  const { chromium } = await import((await import('node:url')).pathToFileURL(process.argv[2]).href);
  const fs = await import('node:fs');
  const path = await import('node:path');
  const http = await import('node:http');
  const { dashboardFile } = await import('./dashboard_static.mjs');
  const evidence = path.resolve(process.argv[3]);
  fs.mkdirSync(evidence, { recursive: true });
  const server = http.createServer((req, res) => {
    const name = new URL(req.url, 'http://fixture').pathname;
    const asset = name === '/test.mjs'
      ? { data: fs.readFileSync(new URL('./dashboard_loading.mjs', import.meta.url)), type: 'text/javascript' }
      : dashboardFile(name);
    if (!asset) { res.writeHead(404); res.end(); return; }
    res.setHeader('content-type', asset.type);
    res.end(name === '/' ? asset.data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '') : asset.data);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  let browser;
  try {
    browser = await chromium.launch({ headless: true });
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=one#tasks`);
    await page.evaluate(() => { globalThis.setInterval = () => 0; globalThis.EventSource = class { close() {} }; });
    await page.addScriptTag({ type: 'module', url: '/test.mjs' });
    await page.waitForFunction(() => globalThis.loadingTestsPassed);
    await assertHealthTriage(page, evidence, path);
    if (errors.length) throw new Error(errors.join('\n'));
    fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, views: ['incidents', 'errors', 'metrics'], windows: ['24h', '7d'], widths: [1440, 720, 390] }, null, 2));
    console.log('Health triage, window requests, grouped tokens and readable summary passed in Chromium.');
  } finally {
    await browser?.close();
    await new Promise(resolve => server.close(resolve));
  }
}
