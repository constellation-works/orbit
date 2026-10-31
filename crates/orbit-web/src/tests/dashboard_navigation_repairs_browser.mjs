// Public dashboard interactions for the grouped navigation/accessibility repair.
// The caller boots the shipped app against disposable API responses.
import assert from 'node:assert/strict';
import path from 'node:path';

export async function assertDashboardNavigationRepairs(page, evidence) {
  page.setDefaultTimeout(5000);
  const failures = [];
  const check = async (name, body) => {
    try { await body(); } catch (error) { failures.push(`${name}: ${error.message}`); }
  };
  const shots = async name => {
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: 900 });
      const detail = name === 'codebase-defects-link' ? '.codebase-notes'
        : name.startsWith('metrics-') ? '#metrics-trend-view .mx-notes' : null;
      if (width === 375 && detail) await page.locator(detail).scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, `${name}-dark-${width}.png`), animations: 'disabled' });
    }
    await page.setViewportSize({ width: 1440, height: 900 });
  };
  await page.evaluate(async () => {
    const common = await import('/js/common.js');
    common.setWorkspace('one');
    common.setRegisteredHosts([{ name: 'fixture-host', local: true }]);
    common.persistScopeToUrl();
    globalThis.navigationSentinel = {};
    const previousFetch = globalThis.fetch;
    globalThis.fetch = async (input, options) => {
      const url = new URL(input, location.href);
      const empty = { rows: [], below_support: 0 };
      let data;
      if (url.pathname === '/api/codebase/analytics') data = {
        window: '30d', rollup: 'file', min_support: 3, tasks_scanned: 5,
        regressions: { rows: [{ path: 'crates/fixture.rs', numerator: 1, denominator: 5, rate: 0.2,
          regressed_tasks: [{ task_id: 'LINK-1', title: 'Regressed task', regression_tasks: ['LINK-2'] }] }] },
        footprint_misses: empty, blocked: { ...empty, causes: [] }, rejected: empty,
        change_coupling: empty, lock_contention: empty, overlap: { rows: [], ranked_paths: 1 },
      };
      if (url.pathname === '/api/codebase/related') data = {
        path: 'crates/fixture.rs', path_support: 5,
        rows: [{ path: 'crates/related.rs', jaccard: 0.5, support: { both: 3, either: 6 }, tasks: [{ task_id: 'LINK-3', title: 'Changed together' }] }],
      };
      if (url.pathname === '/api/routines') data = { routines: [{ name: 'history-fixture', source: 'one', enabled: true, target: 'job:fixture' }], clock: {} };
      if (url.pathname === '/api/diagnostics/errors') data = { items: [{ message: 'Long failure message '.repeat(30), source: 'process', job_run: 'disclosure-run', ts: '2026-10-01T00:00:00Z' }] };
      if (url.pathname.startsWith('/api/tasks/LINK-')) data = { id: url.pathname.split('/').at(-1), title: 'Linked task', status: 'done', priority: 'medium' };
      return data ? new Response(JSON.stringify(data)) : previousFetch(input, options);
    };
  });
  await check('routine job Back', async () => {
    await page.evaluate(async () => {
      (await import('/js/router.js')).setActiveTab('operations/routines');
      await (await import('/js/operations.js')).fetchAndRenderOperations('routines');
    });
    const link = page.locator('.operation-job-link').first();
    await link.click();
    await page.waitForURL(url => url.hash === '#jobs/catalogue');
    await page.goBack();
    await page.waitForURL(url => url.hash === '#operations/routines');
    assert.equal(new URL(await link.getAttribute('href'), page.url()).search, new URL(page.url()).search);
    await shots('routines');
  });
  await check('error disclosure click', async () => {
    await page.evaluate(async () => {
      (await import('/js/router.js')).setActiveTab('diagnostics/incidents/errors');
      document.getElementById('refresh-btn').click();
    });
    const summary = page.locator('.error-message summary').first();
    await summary.waitFor();
    await summary.click();
    assert.equal(new URL(page.url()).hash.split('?')[0], '#diagnostics/incidents/errors');
    assert.equal(await page.locator('.error-message').first().evaluate(node => node.open), true);
    await shots('errors-disclosure');
  });
  await check('Codebase links stay in page', async () => {
    const open = async (file = false) => {
      await page.evaluate(async file => {
        (await import('/js/router.js')).setActiveTab(file ? 'analytics/codebase/file?path=crates%2Ffixture.rs' : 'analytics/codebase');
        await (await import('/js/codebase.js')).fetchAndRenderCodebase();
      }, file);
    };
    const openRelated = async () => {
      const details = page.locator('.codebase-related-evidence');
      if (!await details.evaluate(node => node.open)) await details.locator('summary').click();
    };
    await open();
    // A capture at the document tail records the handler's decision, then
    // cancels the test event so an unfixed link cannot reload the fixture.
    const prevented = async link => link.evaluate(node => {
      const event = new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 });
      let handled;
      const stopNavigation = event => { handled = event.defaultPrevented; event.preventDefault(); };
      document.addEventListener('click', stopNavigation);
      node.dispatchEvent(event);
      document.removeEventListener('click', stopNavigation);
      return handled ?? event.defaultPrevented;
    });
    const defects = page.getByRole('link', { name: 'Metrics › Defects', exact: true });
    assert.equal(await prevented(defects), true, 'Defects link intercepts a plain click');
    assert.equal(new URL(page.url()).hash.split('?')[0], '#analytics/metrics');
    for (const id of ['LINK-1', 'LINK-2', 'LINK-3']) {
      await open(true);
      if (id === 'LINK-3') await openRelated();
      const link = id === 'LINK-3' ? page.getByRole('link', { name: 'Changed together', exact: true }) : page.getByRole('link', { name: id, exact: true });
      await link.waitFor({ state: 'attached' });
      const href = new URL(await link.getAttribute('href'), page.url());
      assert.equal(href.searchParams.get('workspace'), 'one');
      assert.equal(href.searchParams.get('host'), 'fixture-host');
      assert.equal(href.hash, `#tasks?open=${id}`);
      assert.equal(await prevented(link), true, `${id} intercepts a plain click`);
      // Follow a real pointer click as well, preserving the Tasks filter and
      // the current document while the link opens its pinned task.
      await open(true);
      if (id === 'LINK-3') await openRelated();
      const actualLink = id === 'LINK-3' ? page.getByRole('link', { name: 'Changed together', exact: true }) : page.getByRole('link', { name: id, exact: true });
      const filter = await page.locator('#task-search').inputValue();
      await actualLink.click();
      await page.waitForURL(url => url.hash.split('?')[0] === '#tasks' && new URLSearchParams(url.hash.split('?')[1]).get('open') === id);
      assert.equal(await page.evaluate(() => Boolean(globalThis.navigationSentinel)), true, 'task navigation keeps the document');
      assert.equal(await page.locator('#task-search').inputValue(), filter, 'task navigation keeps the Tasks filter');
    }
    await open(true);
    await openRelated();
    await shots('codebase-file-links');
    await open();
    await shots('codebase-defects-link');
  });
  await check('Pilots header scopes', async () => {
    await page.evaluate(async () => {
      (await import('/js/router.js')).setActiveTab('analytics/agents/pilots');
      const { pilotsPanel } = await import('/js/agents-pilots.js');
      const tiers = ['low', 'medium', 'hard', 'xhard'];
      document.getElementById('agents-body').replaceChildren(...pilotsPanel.render({ rows: [{ crew: 'fixture', model: 'fixture', stats: [{ key: 'tier_matrix', value: { tiers, rows: tiers.map((assessed, i) => ({ assessed, observed: tiers.map((_, j) => i === j ? 3 : 1) })), placed: 24, on_tier: 12, too_easy: 6, too_hard: 6 } }] }] }));
    });
    assert.deepEqual(await page.locator('.agents-tier-matrix thead th').evaluateAll(nodes => nodes.map(node => node.scope)), Array(5).fill('col'));
    assert.deepEqual(await page.locator('.agents-tier-matrix tbody th').evaluateAll(nodes => nodes.map(node => node.scope)), Array(4).fill('row'));
    await shots('pilots-matrix');
  });
  await check('multi-week metrics labels', async () => {
    await page.evaluate(async () => {
      const previousFetch = globalThis.fetch;
      const window = { key: 'all', bucket_seconds: 1209600, since: '2026-01-05T00:00:00Z', until: '2026-02-16T00:00:00Z' };
      const stats = { n: 3, avg: 60000, p50: 60000, p95: 120000 };
      const totals = { all: stats, low: stats, medium: null, hard: null };
      const done = { done: { total: 3, low: 3, medium: 0, hard: 0, other: 0 }, rejected: 0, archived: 0 };
      const starts = ['2026-01-05T00:00:00Z', '2026-01-19T00:00:00Z', '2026-02-02T00:00:00Z'];
      globalThis.fetch = async (input, options) => {
        const url = new URL(input, location.href);
        let data;
        if (url.pathname === '/api/diagnostics/implement_one') data = { window, outcome: 'succeeded', buckets: starts.map(start => ({ start, ...totals })), current: totals, previous: totals, actors: [], slowest: [] };
        if (url.pathname === '/api/diagnostics/completions') data = { window, buckets: starts.map(start => ({ start, ...done })), current: done, previous: done };
        if (url.pathname === '/api/diagnostics/defects') data = { unavailable: 'fixture' };
        if (url.pathname === '/api/diagnostics/task_tokens') {
          const cell = n => ({ n, mean: n ? 12000 : null, median: n ? 12000 : null, thin: false, excluded_uncovered: 0, excluded_no_invocations: 0 });
          data = { window, scope: 'all', outcome: 'done', tags: [], model: null, min_tasks: 3,
            bucket_starts: starts, series: [{ complexity: 'low', cohort: 'all', points: [cell(3), cell(0), cell(3)], total: cell(6) }],
            total: cell(6), excluded_other_model: 0, available_models: [], available_tags: [] };
        }
        return data ? new Response(JSON.stringify(data)) : previousFetch(input, options);
      };
      (await import('/js/common.js')).setWindow('all');
      (await import('/js/router.js')).setActiveTab('analytics/metrics');
      await (await import('/js/metrics.js')).fetchAndRenderMetrics();
    });
    await page.getByRole('tab', { name: 'Tasks completed', exact: true }).click();
    // The `all` window opens on Cumulative; the label under test is the per-bucket one.
    await page.getByRole('button', { name: 'Per bucket' }).click();
    assert.match(await page.locator('.mx-plot').getAttribute('aria-label'), /per 2 weeks/);
    assert.match(await page.locator('#metrics-trend-view .mx-notes').textContent(), /2-week/);
    await shots('metrics-multiweek');
    await page.getByRole('tab', { name: 'Tokens per task', exact: true }).click();
    await page.locator('#metrics-trend-view .mx-plot').waitFor();
    // The existing line behavior spans an empty bucket; its legend is
    // reviewed in these screenshots rather than pinned as UI wording.
    await shots('metrics-tokens');
  });
  assert.deepEqual(failures, [], failures.join('\n'));
}
