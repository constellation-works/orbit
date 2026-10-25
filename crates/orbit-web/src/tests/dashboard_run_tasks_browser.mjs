import fs from 'node:fs';
import path from 'node:path';

export async function assertRunTaskLabels(page, evidence) {
  await page.evaluate(async () => {
    const { setWorkspace, persistScopeToUrl } = await import('/js/common.js');
    const { setRunFilter } = await import('/js/runs.js');
    const { setActiveTab } = await import('/js/router.js');
    setWorkspace('one');
    persistScopeToUrl();
    const pilotTask = {
      id: 'RUN-TASK-PILOT', title: 'Task held by pilot findings', status: 'proposed', priority: 'medium',
      pilot_warnings: [{ kind: 'utility_warnings', text: 'Fixture warning that must appear on the task' }],
    };
    globalThis.runTaskFixture = {
      previousFetch: globalThis.fetch,
      task: { id: 'RUN-TASK-1', title: 'A long delivered task title <b>shown as text</b> '.repeat(8), status: 'done', priority: 'medium' },
      pilotTask,
      taskListRequests: [],
    };
    globalThis.fetch = async (input, options) => {
      const url = new URL(input, window.location.href);
      const { task, pilotTask, previousFetch } = globalThis.runTaskFixture;
      const labels = [{ id: task.id, title: task.title }];
      const run = { run_id: 'jrun-task-label', job_id: 'task_pr_pipeline', state: 'success', task_ids: [task.id], tasks: labels,
        child_dispatches: [{ child_run_id: 'jrun-child-label', job_name: 'task_pr_pipeline', phase: 'submitted', task_ids: [task.id], tasks: labels }] };
      const response = payload => ({ ok: true, status: 200, json: async () => payload });
      if (url.pathname === '/api/job-runs') return response({ items: [run, { run_id: 'jrun-plain', job_id: 'maintenance', state: 'success', task_ids: null, tasks: null }], total: 2, limit: 25 });
      if (url.pathname === '/api/tasks') {
        globalThis.runTaskFixture.taskListRequests.push(url.search);
        const items = url.searchParams.get('pilot') === 'warnings' ? [pilotTask] : [task];
        return response({ items, total: items.length, limit: 50 });
      }
      if (url.pathname === `/api/tasks/${task.id}`) return response(task);
      if (url.pathname === `/api/tasks/${pilotTask.id}`) return response(pilotTask);
      if (url.pathname === '/api/runs/jrun-task-label') return response({ run, steps: [] });
      if (url.pathname.startsWith('/api/runs/jrun-task-label/')) return response([]);
      return previousFetch(input, options);
    };
    setRunFilter('all');
    setActiveTab('jobs/history');
    document.getElementById('refresh-btn').click();
  });
  try {
    const row = page.locator('#runs-body .runs-row[data-key="run-one:jrun-task-label"]');
    await row.waitFor({ state: 'visible' });
    await page.setViewportSize({ width: 1440, height: 900 });
    if (!await page.getByRole('button', { name: 'Task', exact: true }).isVisible()) throw new Error('Runs must expose the Task column');
    const link = row.locator('.run-task-link');
    const title = link.locator('.run-task-title');
    const layout = await title.evaluate(node => ({ overflow: getComputedStyle(node).textOverflow, clipped: node.scrollWidth > node.clientWidth, children: node.childElementCount }));
    if (layout.overflow !== 'ellipsis' || !layout.clipped || layout.children !== 0) throw new Error(`Task titles must truncate and render as text: ${JSON.stringify(layout)}`);
    const href = new URL(await link.getAttribute('href'), page.url());
    if (href.searchParams.get('workspace') !== 'one' || href.hash !== '#tasks?open=RUN-TASK-1') throw new Error(`Task links must preserve workspace and name only the task, never a filter: ${href.href}`);
    if (await page.locator('#runs-body [data-key="run-one:jrun-plain"] .run-tasks').textContent() !== '—') throw new Error('Task-free rows must have an empty task cell');
    await page.screenshot({ path: path.join(evidence, 'run-task-column.png'), fullPage: true });
    await page.evaluate(() => {
      globalThis.runTaskFixture.task.title = `Updated ${globalThis.runTaskFixture.task.title}`;
      document.getElementById('refresh-btn').click();
    });
    await page.waitForFunction(() => document.querySelector('#runs-body .run-task-title').textContent.startsWith('Updated '));
    await page.setViewportSize({ width: 390, height: 900 });
    if (!await link.isVisible() || await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error('Task links must fit mobile run cards');
    const taskFilter = () => page.evaluate(() => ({
      // The label, not the whole chip: a chip's count follows the task list.
      chips: [...document.querySelectorAll('#task-filter .chip.active')].map(chip => (chip.querySelector('.chip-label') || chip).textContent),
      search: document.getElementById('task-search').value,
    }));
    const filterBefore = await taskFilter();
    // The default filter hides done tasks, so this link has to reach a task its
    // list does not hold, without widening the filter to do it.
    await link.click();
    const pinned = page.locator('#task-detail-body:has(.pinned-external)');
    await pinned.waitFor({ state: 'visible' });
    if (!(await pinned.textContent()).includes('RUN-TASK-1')) throw new Error('A task link must open that task expanded');
    if (JSON.stringify(await taskFilter()) !== JSON.stringify(filterBefore)) throw new Error('Following a task link must not change the Tasks filter');
    const openUrl = page.url();
    if (!openUrl.includes('open=RUN-TASK-1') || openUrl.includes('q=')) throw new Error(`The open task must be in the URL, with no search injected: ${openUrl}`);
    // A copied URL opens the same task in a fresh tab.
    const fresh = await page.context().browser().newPage();
    try {
      await fresh.route('**/api/**', route => {
        const { pathname } = new URL(route.request().url());
        const body = pathname === '/api/tasks/RUN-TASK-1' ? { id: 'RUN-TASK-1', title: 'Deep linked task', status: 'done', priority: 'medium' }
          : pathname === '/api/tasks' ? { items: [], total: 0, limit: 50 }
          : pathname === '/api/workspaces' ? [{ id: 'one', name: 'one', status: 'active', is_default: true }]
          : pathname === '/api/crews' ? { default_crew: null, crews: [] }
          : { items: [], total: 0, limit: 50 };
        return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
      });
      await fresh.goto(openUrl);
      await fresh.evaluate(() => {
        globalThis.setInterval = () => 0;
        globalThis.EventSource = class { close() {} };
      });
      // The fixture server leaves app.js out of the page; load it like a visit.
      await fresh.addScriptTag({ type: 'module', url: '/app.js' });
      await fresh.locator('#task-detail-body:has(.pinned-external)').waitFor({ state: 'visible' });
      if (!(await fresh.locator('#task-detail-body:has(.pinned-external)').textContent()).includes('RUN-TASK-1')) throw new Error('A copied task URL must open the same task in a new tab');
    } finally {
      await fresh.close();
    }
    // Back is one step to the Runs list: the link added a single history entry.
    await page.goBack();
    await page.waitForFunction(() => document.querySelector('.tab-pane.active')?.dataset.tab === 'jobs');
    // With the task inside the operator's filter it opens in place in the list.
    await page.evaluate(async () => {
      const { setActiveTab } = await import('/js/router.js');
      setActiveTab('tasks?status=done');
    });
    await page.locator('#tasks-body .row[data-key="task-RUN-TASK-1"]').waitFor({ state: 'visible' });
    const listFilter = await taskFilter();
    await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('jobs/history'));
    await link.click();
    await page.locator('#tasks-body .row[data-key="task-RUN-TASK-1"].expanded').waitFor({ state: 'visible' });
    if (JSON.stringify(await taskFilter()) !== JSON.stringify(listFilter)) throw new Error('A task link must leave the operator\'s status filter as it was');
    // Closing the task removes it from the URL, so a copy no longer names it.
    // At this phone width the task is a full-screen sheet; Back closes it.
    await page.locator('#task-detail-pane[role="dialog"] .task-pane-back').click();
    await page.waitForFunction(() => !window.location.hash.includes('open='));
    await page.evaluate(async () => (await import('/js/runs.js')).setRunFilter('failed'));
    const historyFilters = new URL(page.url()).search;
    await page.evaluate(async () => (await import('/js/router.js')).navigateToRun('jrun-task-label'));
    const header = page.locator('#run-detail-meta .run-meta-task .run-task-link');
    await header.waitFor({ state: 'visible' });
    if (await header.locator('.run-task-id').textContent() !== 'RUN-TASK-1' || !(await header.textContent()).includes('<b>shown as text</b>')) throw new Error('Run header must show the task ID and title');
    if (await page.locator('#run-detail-meta .child-dispatch-row .run-task-id').textContent() !== 'RUN-TASK-1') throw new Error('Coordinator child runs must name their tasks');
    await page.setViewportSize({ width: 390, height: 900 });
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error('Task labels must fit the mobile run detail');
    await page.screenshot({ path: path.join(evidence, 'run-task-detail-mobile.png'), fullPage: true });
    if (await page.locator('#topbar-crumb').textContent() !== 'Jobs › History') throw new Error('Run detail must belong to Jobs History');
    await page.locator('#run-detail-meta .run-detail-actions .back-action').click();
    await page.waitForFunction(() => location.hash.startsWith('#jobs/history'));
    if (new URL(page.url()).search !== historyFilters) throw new Error('Back to Jobs History changed the run filters or scope');
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('tasks?status=proposed&pilot=warnings'));
    const pilotRow = page.locator('#tasks-body .row[data-key="task-RUN-TASK-PILOT"]');
    await pilotRow.waitFor({ state: 'visible' });
    const pilotFilterState = await page.evaluate(() => {
      const filter = document.querySelector('#task-filter [data-role="pilot-warnings"]');
      const requests = globalThis.runTaskFixture.taskListRequests.map(search => new URLSearchParams(search));
      return {
        active: filter?.getAttribute('aria-pressed') === 'true',
        requested: requests.some(params => params.get('status') === 'proposed' && params.get('pilot') === 'warnings'),
        route: location.hash,
      };
    });
    if (!pilotFilterState.active || !pilotFilterState.requested || !pilotFilterState.route.includes('pilot=warnings')) {
      throw new Error(`Pilot warnings filter must reach the proposed task query: ${JSON.stringify(pilotFilterState)}`);
    }
    const pilotBadge = pilotRow.locator('.pilot-warning-chip');
    if (await pilotBadge.getAttribute('aria-label') !== 'Pilot warnings: 1 holding findings') {
      throw new Error(`The task row must expose its holding-finding count: ${await pilotBadge.getAttribute('aria-label')}`);
    }
    await pilotRow.locator('.title').click();
    const pilotDetail = page.locator('#task-detail-body .pilot-warnings');
    await pilotDetail.waitFor({ state: 'visible' });
    const pilotFinding = await pilotDetail.locator('.pilot-warning-findings').textContent();
    if (!pilotFinding.includes('Utility: Fixture warning that must appear on the task')) {
      throw new Error(`The task detail must show its pilot finding: ${pilotFinding}`);
    }
    await page.evaluate(async () => (await import('/js/runs.js')).setRunFilter('all'));
    fs.writeFileSync(path.join(evidence, 'run-task-result.json'), JSON.stringify({ column: true, truncatedTitle: true, taskNavigation: true, header: true, childLabels: true, mobile: true, jobsHistoryBackLink: true, preservedFilters: true, pilotWarningsFilter: true, pilotWarningRowBadge: true, pilotWarningDetail: true }));
  } finally {
    await page.evaluate(() => {
      if (globalThis.runTaskFixture) globalThis.fetch = globalThis.runTaskFixture.previousFetch;
      delete globalThis.runTaskFixture;
    });
    await page.setViewportSize({ width: 1440, height: 900 });
  }
}

// A task link that changes workspace adds one history entry and leaves the
// entry it was clicked on alone, so Back and Forward restore each entry's own
// scope. Drives the shipped app and the real runTaskLinks/bindTaskLink path.
export async function assertTaskLinkScopeHistory(browser, origin, evidence) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.route('**/api/**', route => {
      const url = new URL(route.request().url());
      const workspace = url.searchParams.get('workspace');
      const task = id => ({ id, title: `Task ${id}`, status: 'in-progress', priority: 'medium' });
      const run = (workspaceId, taskId, attributed) => ({
        run_id: `jrun-${taskId}`, job_id: 'task_pr_pipeline', state: 'success', task_ids: [taskId], tasks: [{ id: taskId, title: `Task ${taskId}` }],
        ...(attributed ? { workspace_id: workspaceId, workspace_name: workspaceId } : {}),
      });
      let body = { items: [], total: 0, limit: 50 };
      if (url.pathname === '/api/workspaces') body = ['one', 'two'].map(id => ({ id, name: id, status: 'active', is_default: id === 'one' }));
      else if (url.pathname === '/api/diagnostics/friction') body = [];
      else if (url.pathname === '/api/crews') body = { default_crew: null, crews: [] };
      else if (url.pathname === '/api/job-runs/all') body = { items: [run('one', 'ONE-1', true), run('two', 'TWO-1', true)], total: 2, limit: 25 };
      else if (url.pathname === '/api/job-runs') {
        // Workspace one also lists a run owned by two, the cross-workspace link.
        body = { items: workspace === 'one' ? [run('one', 'ONE-1', false), run('two', 'TWO-1', true)] : [run('two', 'TWO-1', false)], total: 1, limit: 25 };
      } else if (url.pathname === '/api/tasks' || url.pathname === '/api/tasks/all') body = { items: [], total: 0, limit: 50 };
      else if (/^\/api\/tasks\/[A-Z]+-\d+$/.test(url.pathname)) body = task(url.pathname.split('/').pop());
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
    });
    const state = () => page.evaluate(async () => {
      const common = await import('/js/common.js');
      const url = new URL(window.location.href);
      return {
        workspaceParam: url.searchParams.get('workspace'),
        hash: url.hash,
        workspace: common.getWorkspace(),
        aggregate: common.isAggregateView(),
        selector: document.getElementById('workspace-select')?.value,
        historyLength: history.length,
      };
    });
    // Opening the task, and the runs list, must each load for the scope in force.
    const requests = [];
    page.on('request', request => {
      const url = new URL(request.url());
      if (url.pathname.startsWith('/api/job-runs') || url.pathname.startsWith('/api/tasks/')) requests.push(`${url.pathname}?workspace=${url.searchParams.get('workspace') || ''}`);
    });
    const runsLink = taskId => page.locator('#runs-body .run-task-link', { hasText: taskId });
    const waitForRuns = taskId => runsLink(taskId).waitFor({ state: 'visible' });
    const expect = (actual, expected, what) => {
      for (const [key, value] of Object.entries(expected)) {
        if (actual[key] !== value) throw new Error(`${what}: ${key} must be ${JSON.stringify(value)}: ${JSON.stringify(actual)}`);
      }
    };
    const openTask = async taskId => {
      const pinned = page.locator('#task-detail-body:has(.pinned-external)');
      await pinned.waitFor({ state: 'visible' });
      if (!(await pinned.textContent()).includes(taskId)) throw new Error(`The link must open ${taskId} expanded`);
    };
    const boot = async search => {
      await page.goto(`${origin}/${search}#jobs/history?window=24h`);
      await page.evaluate(async () => {
        globalThis.setInterval = () => 0;
        globalThis.EventSource = class { close() {} };
        await import('/app.js');
      });
      await page.locator('#workspace-select').waitFor({ state: 'attached' });
    };

    // All workspaces to a task in one.
    await boot('?workspace=all');
    await waitForRuns('ONE-1');
    const source = await state();
    expect(source, { workspaceParam: 'all', workspace: null, aggregate: true, selector: '' }, 'All workspaces Runs');
    const mark = requests.length;
    await runsLink('ONE-1').click();
    await openTask('ONE-1');
    const destination = await state();
    expect(destination, { workspaceParam: 'one', workspace: 'one', aggregate: false, selector: 'one', historyLength: source.historyLength + 1 }, 'Task link');
    if (!destination.hash.startsWith('#tasks')) throw new Error(`The link must land on Tasks: ${destination.hash}`);
    if (!requests.slice(mark).includes('/api/tasks/ONE-1?workspace=one')) throw new Error(`The task must load for its own workspace: ${requests.slice(mark)}`);
    await page.goBack();
    await waitForRuns('ONE-1');
    expect(await state(), { workspaceParam: 'all', workspace: null, aggregate: true, selector: '', hash: source.hash }, 'Back to All workspaces');
    const restored = requests.slice(mark).filter(entry => entry.startsWith('/api/job-runs'));
    if (restored[restored.length - 1] !== '/api/job-runs/all?workspace=') throw new Error(`Back must request the aggregate runs: ${restored}`);
    if (!await runsLink('TWO-1').isVisible()) throw new Error('Back must show the aggregate rows again');
    await page.goForward();
    await openTask('ONE-1');
    expect(await state(), { workspaceParam: 'one', workspace: 'one', aggregate: false, selector: 'one', historyLength: destination.historyLength }, 'Forward to the task');

    // Workspace one to a task in two.
    await boot('?workspace=one');
    await waitForRuns('TWO-1');
    const fromOne = await state();
    expect(fromOne, { workspaceParam: 'one', workspace: 'one', selector: 'one' }, 'Workspace one Runs');
    await runsLink('TWO-1').click();
    await openTask('TWO-1');
    expect(await state(), { workspaceParam: 'two', workspace: 'two', selector: 'two', historyLength: fromOne.historyLength + 1 }, 'Cross-workspace link');
    await page.goBack();
    await waitForRuns('TWO-1');
    expect(await state(), { workspaceParam: 'one', workspace: 'one', selector: 'one', hash: fromOne.hash }, 'Back to workspace one');
    if (requests.filter(entry => entry.startsWith('/api/job-runs')).pop() !== '/api/job-runs?workspace=one') throw new Error(`Back must request workspace one's runs: ${requests}`);
    await page.goForward();
    await openTask('TWO-1');
    expect(await state(), { workspaceParam: 'two', workspace: 'two', selector: 'two' }, 'Forward to workspace two');

    // A modified click and the copied address still name the task and scope.
    await page.goBack();
    await waitForRuns('TWO-1');
    const href = new URL(await runsLink('TWO-1').getAttribute('href'), page.url());
    if (href.searchParams.get('workspace') !== 'two' || href.hash !== '#tasks?open=TWO-1') throw new Error(`The copied link must name its workspace and task: ${href.href}`);
    const before = await state();
    await runsLink('TWO-1').click({ modifiers: ['Shift'], noWaitAfter: true }).catch(() => {});
    await page.waitForTimeout(100);
    for (const opened of page.context().pages()) if (opened !== page) await opened.close();
    expect(await state(), { workspaceParam: 'one', workspace: 'one', hash: before.hash, historyLength: before.historyLength }, 'Modified click');
    if (errors.length) throw new Error(errors.join('\n'));
    fs.writeFileSync(path.join(evidence, 'task-link-history-result.json'), JSON.stringify({ allToWorkspace: true, workspaceToWorkspace: true, modifiedClick: true }));
  } finally {
    await page.close();
  }
}

// Search dismissal must retire both text filtering and asynchronously opened
// details. Held network responses make these races deterministic.
export async function assertTaskLookupClear(browser, origin, evidence) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const tasks = [
    { id: 'ORB-16021', title: 'Completed lookup target', status: 'done', priority: 'medium', description: 'Full lookup detail' },
    { id: 'ORB-16022', title: 'Listed lookup target', status: 'in-progress', priority: 'medium' },
  ];
  const filters = 'in-progress,review,blocked,proposed,backlog';
  const base = `${origin}/?host=dk-server-2&workspace=ws_orbit&window=24h#tasks?status=${encodeURIComponent(filters)}`;
  const held = [];
  let holdDetail = false;
  let holdList = false;
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const list = page.locator('#tasks-body .row[data-key="task-ORB-16022"]');
  const pinned = page.locator('#task-detail-body [data-key="pinned-ORB-16021"]');
  const search = page.locator('#task-search');
  const clear = page.getByRole('button', { name: 'Clear task search and open result', exact: true });
  const boot = async url => {
    await page.goto(url);
    await page.evaluate(async () => {
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      await import('/app.js');
    });
  };
  const lookup = async id => { await search.fill(id); await search.press('Enter'); };
  // At phone width the open task is a full-screen sheet over the list and its
  // search; the sheet's Back returns to them.
  const backToList = async width => {
    if (width !== 375) return;
    const sheet = page.locator('#task-detail-pane[role="dialog"][aria-modal="true"]');
    await sheet.waitFor({ state: 'visible' });
    await sheet.locator('.task-pane-back').click();
    await sheet.waitFor({ state: 'hidden' });
  };
  const routeState = () => page.evaluate(() => {
    const url = new URL(location.href);
    return { scope: url.search, params: Object.fromEntries(new URLSearchParams(url.hash.split('?')[1])), query: document.getElementById('task-search').value };
  });
  const restored = async () => {
    await list.waitFor({ state: 'visible' });
    await page.waitForFunction(() => !document.querySelector('#task-detail-body .pinned-task-wrap, #tasks-body .row.expanded'));
    const state = await routeState();
    check(!state.params.open && !state.params.q && state.query === '', `Clear must retire open and query: ${JSON.stringify(state)}`);
    check(state.scope === '?workspace=ws_orbit&window=24h' && state.params.status === filters, `Clear must retain scope and status: ${JSON.stringify(state)}`);
    check(await page.locator('#task-lookup-status').textContent() === '', 'Clear must retire lookup feedback');
    check(await pinned.count() === 0, 'Clear must remove the out-of-filter target');
  };
  const nextHeld = async () => {
    await page.waitForFunction(() => document.getElementById('task-lookup-status').textContent.includes('Looking up'));
    // The request event precedes route interception; await the actual held route.
    await new Promise(resolve => {
      if (held.length) resolve();
      else held.push({ notify: resolve });
    });
    return held.shift();
  };
  const release = async read => {
    const response = page.waitForResponse(reply => reply.request() === read.request);
    await read.fulfill();
    await (await response).finished();
    await page.evaluate(async () => { await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); });
  };
  try {
    await page.route('**/api/**', async route => {
      const url = new URL(route.request().url());
      const pathname = url.pathname.replace(/^\/api\/on\/[^/]+/, '/api');
      let status = 200;
      let body = { items: [], total: 0, limit: 50 };
      if (pathname === '/api/hosts') body = { hosts: [{ name: 'dk-server-2', local: true, reachable: true }] };
      else if (pathname === '/api/workspaces') body = [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }, { id: 'ws_other', name: 'other', status: 'active' }];
      else if (pathname === '/api/diagnostics/friction') body = [];
      else if (pathname === '/api/crews') body = { default_crew: null, crews: [] };
      else if (pathname === '/api/tasks') {
        const q = url.searchParams.get('q')?.toLowerCase() || '';
        const selected = (url.searchParams.get('status') || filters).split(',');
        const items = tasks.filter(t => selected.includes(t.status) && `${t.id} ${t.title}`.toLowerCase().includes(q));
        body = { items, total: items.length, limit: 50 };
      } else if (/^\/api\/tasks\/ORB-\d+$/.test(pathname)) {
        body = tasks.find(t => t.id === pathname.split('/').pop());
        if (pathname.endsWith('16025') && url.searchParams.get('workspace') === 'ws_other') body = { id: 'ORB-16025', title: 'Other workspace result', status: 'done', priority: 'medium' };
        if (!body) { status = pathname.endsWith('16024') ? 503 : 404; body = { error: 'Fixture lookup unavailable' }; }
      }
      if (holdDetail && body?.id) body = { ...body, title: 'Obsolete held lookup' };
      const fulfill = () => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
      if ((holdDetail && /^\/api\/tasks\/ORB-\d+$/.test(pathname)) || (holdList && pathname === '/api/tasks')) {
        const waiter = held.find(entry => entry.notify);
        if (waiter) held.splice(held.indexOf(waiter), 1);
        held.push({ fulfill, pathname, request: route.request() });
        waiter?.notify();
        return;
      }
      await fulfill();
    });
    await boot(base);
    await list.waitFor({ state: 'visible' });
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
      await lookup('ORB-16021');
      await pinned.waitFor({ state: 'visible' });
      check(await search.inputValue() === '', 'A successful lookup empties search');
      await page.screenshot({ path: path.join(evidence, `task-lookup-open-${width}.png`) });
      await backToList(width);
      check(await clear.isVisible(), 'An empty search with a lookup result must expose Clear beside search');
      const layout = await clear.evaluate(node => {
        const rect = node.getBoundingClientRect();
        const search = document.getElementById('task-search').getBoundingClientRect();
        return { inside: rect.left >= search.left && rect.right <= search.right && rect.top === search.top, width: rect.width, height: rect.height, hit: node.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2)) };
      });
      check(layout.inside && layout.hit && layout.width >= 40 && layout.height >= 40, `Clear must be reachable beside search at ${width}px: ${JSON.stringify(layout)}`);
      await search.focus(); await page.keyboard.press('Tab');
      check(await clear.evaluate(node => document.activeElement === node && node.matches(':focus-visible')), 'Tab from search must reach the visibly focused Clear button');
      await page.keyboard.press(width === 375 ? 'Space' : 'Enter');
      await restored();
      check(await search.evaluate(node => document.activeElement === node), 'Clear returns focus to usable search');
      await page.screenshot({ path: path.join(evidence, `task-lookup-cleared-${width}.png`) });
      await lookup('ORB-16022');
      await list.and(page.locator('.expanded')).waitFor({ state: 'visible' });
      check(!(await routeState()).params.q, 'Successful listed lookup must remove stale q from route');
      await backToList(width);
      await clear.click(); await restored();
      await boot(`${base}&open=ORB-16021`);
      await pinned.waitFor({ state: 'visible' });
      await page.screenshot({ path: path.join(evidence, `task-deep-link-${width}.png`) });
      await backToList(width);
      await clear.click(); await restored();
    }
    await page.setViewportSize({ width: 1440, height: 900 });
    // Copied links retain filters, and their cleared route survives reload.
    await boot(`${base}&open=ORB-16021`);
    await pinned.waitFor({ state: 'visible' });
    await clear.click(); await restored();
    await boot(page.url()); await restored();
    // The pane's Close and row collapse still remove open=.
    await lookup('ORB-16021'); await pinned.waitFor({ state: 'visible' });
    await page.locator('#task-detail-pane').getByRole('button', { name: 'Close task detail', exact: true }).click(); await restored();
    await lookup('ORB-16022'); await list.and(page.locator('.expanded')).waitFor({ state: 'visible' });
    await list.locator('.title').click(); await restored();
    for (const id of ['ORB-16023', 'ORB-16024']) {
      await lookup(id);
      await page.waitForFunction(() => document.querySelector('.task-search-wrap.error'));
      check((await routeState()).params.q === id.toLowerCase(), 'Failed lookup must retain its text query in the route until cleared');
      await clear.click(); await restored();
    }
    await search.fill('Listed');
    await page.waitForFunction(() => location.hash.includes('q=listed'));
    await clear.click(); await restored();
    // Hold an ID read, clear it, then let its response arrive.
    holdDetail = true;
    await lookup('ORB-16021'); const delayed = await nextHeld();
    check((await routeState()).params.q === 'orb-16021', 'Pending lookup must represent its text query in the route');
    await clear.click(); await restored();
    holdDetail = false; await release(delayed);
    await restored();
    holdDetail = true;
    await lookup('ORB-16024'); const delayedError = await nextHeld();
    await clear.click(); await restored();
    holdDetail = false; await release(delayedError);
    await restored();
    // Hash-driven reads use a generation as well as ID: clear and reopen the
    // same target before the first response returns (the ABA case).
    holdDetail = true;
    const hashRequest = page.waitForRequest(request => new URL(request.url()).pathname.endsWith('/api/tasks/ORB-16021'));
    await page.evaluate(() => { location.hash += '&open=ORB-16021'; });
    await hashRequest;
    await new Promise(resolve => { if (held.length) resolve(); else held.push({ notify: resolve }); });
    const old = held.shift();
    await clear.click(); await restored();
    holdDetail = false;
    await lookup('ORB-16021'); await pinned.waitFor({ state: 'visible' });
    await release(old);
    check(!(await pinned.textContent()).includes('Obsolete held lookup'), 'A retired hash lookup must not replace a newer result for the same ID');
    await clear.click(); await restored();
    // A refresh already reading a list when Clear is clicked may complete
    // later; it must not restore the detail or its former query.
    await lookup('ORB-16021'); await pinned.waitFor({ state: 'visible' });
    holdList = true;
    const request = page.waitForRequest(req => new URL(req.url()).pathname === '/api/tasks');
    await page.locator('#refresh-btn').click(); await request;
    await new Promise(resolve => { if (held.length) resolve(); else held.push({ notify: resolve }); });
    const refresh = held.shift(); holdList = false;
    await clear.click(); await restored(); await release(refresh);
    await restored();
    // Back/Forward reads each entry's open/query state rather than preserving
    // an old pin when returning to a cleared list.
    await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('jobs/history'));
    await page.waitForFunction(() => location.hash.startsWith('#jobs/history'));
    await page.evaluate(() => { location.hash = '#tasks?open=ORB-16021'; });
    await pinned.waitFor({ state: 'visible' });
    await page.goBack(); await page.waitForFunction(() => location.hash.startsWith('#jobs/history'));
    await page.goBack(); await restored();
    await page.goForward(); await page.waitForFunction(() => location.hash.startsWith('#jobs/history'));
    await page.goForward(); await pinned.waitFor({ state: 'visible' });
    await clear.click(); await restored();
    // Lookup must preserve a non-default selection too, rather than route
    // through bare #tasks and silently reset the operator's chips.
    await boot(base.replace(encodeURIComponent(filters), 'in-progress'));
    await list.waitFor({ state: 'visible' });
    await lookup('ORB-16021'); await pinned.waitFor({ state: 'visible' });
    check((await routeState()).params.status === 'in-progress', 'Lookup must preserve a custom status selection');
    await clear.click(); await list.waitFor({ state: 'visible' });
    check((await routeState()).params.status === 'in-progress' && !(await routeState()).params.open, 'Clear must preserve a custom status selection');
    // Aggregate/other-workspace resolution adopts the owner before awaiting
    // its refresh. Replaying its own normalized route must not cancel it.
    await boot(base);
    await list.waitFor({ state: 'visible' });
    await lookup('ORB-16025');
    await page.locator('#task-detail-body [data-key="pinned-ORB-16025"]').waitFor({ state: 'visible' });
    check((await routeState()).scope === '?workspace=ws_other&window=24h', 'Lookup must still adopt its owning workspace');
    await clear.click();
    await page.waitForFunction(() => !document.querySelector('.pinned-task-wrap'));
    check((await routeState()).scope === '?workspace=ws_other&window=24h' && !(await routeState()).params.open, 'Clear must retain the adopted workspace');
    check(errors.length === 0, errors.join('\n'));
    fs.writeFileSync(path.join(evidence, 'task-lookup-clear-result.json'), JSON.stringify({ passed: true, widths: [1440, 375], keyboard: ['Enter', 'Space'], deepLink: true, listedAndFilteredOut: true, reload: true, history: true, notFoundAndError: true, delayedLookup: true, delayedHashLookupSameId: true, delayedRefresh: true, retainedScopeAndFilters: true }, null, 2));
  } finally {
    for (const response of held) if (response.fulfill) await response.fulfill().catch(() => {});
    await page.close();
  }
}

// An unchanged review task must re-read Fix PR readiness once the cache
// interval has passed when the dashboard refreshes, without duplicate reads
// inside the interval or while a read is pending. Drives the shipped
// dashboard through its real Refresh button and render path.
export async function assertFixPrReadinessRefresh(browser, origin, evidence) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const tasks = [
    { id: 'FIXPR-1', title: 'Review task with a pull request', status: 'review', priority: 'medium', pr_number: 42 },
    { id: 'FIXPR-2', title: 'Running task with a pull request', status: 'in-progress', priority: 'medium', pr_number: 43 },
    { id: 'FIXPR-3', title: 'Review task without a pull request', status: 'review', priority: 'medium' },
  ];
  const readinessRequests = [];
  let readiness = { available: false, reason: 'no_blocker', pr_number: 42 };
  let holdReadiness = false;
  const held = [];
  const filters = 'review,in-progress';
  const base = `${origin}/?workspace=ws_orbit&window=24h#tasks?status=${encodeURIComponent(filters)}`;
  const fixPrButton = page.locator('#task-detail-body .action.fix-pr');
  const counts = () => ({
    total: readinessRequests.length,
    forTask: id => readinessRequests.filter(entry => entry === id).length,
  });
  const buttonState = () => page.evaluate(() => {
    const btn = document.querySelector('#task-detail-body .action.fix-pr');
    return btn ? { disabled: btn.disabled, title: btn.getAttribute('title') || '' } : null;
  });
  const refreshAndSettle = async () => {
    const before = await page.evaluate(() => globalThis.__fixPrTaskListRequests || 0);
    await page.locator('#refresh-btn').click();
    await page.waitForFunction(previous => (globalThis.__fixPrTaskListRequests || 0) > previous, before);
    await page.evaluate(async () => { await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); });
  };
  try {
    await page.route('**/api/**', async route => {
      const url = new URL(route.request().url());
      const pathname = url.pathname.replace(/^\/api\/on\/[^/]+/, '/api');
      let status = 200;
      let body = { items: [], total: 0, limit: 50 };
      if (pathname === '/api/hosts') body = { hosts: [{ name: 'dk-server-2', local: true, reachable: true }] };
      else if (pathname === '/api/workspaces') body = [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }];
      else if (pathname === '/api/diagnostics/friction') body = [];
      else if (pathname === '/api/crews') body = { default_crew: null, crews: [] };
      else if (pathname === '/api/tasks') body = { items: tasks, total: tasks.length, limit: 50 };
      else if (/^\/api\/tasks\/FIXPR-\d+\/auto-fix$/.test(pathname)) {
        const id = pathname.split('/')[3];
        readinessRequests.push(id);
        if (holdReadiness) {
          held.push({ fulfill: () => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(readiness) }) });
          return;
        }
        body = readiness;
      } else if (/^\/api\/tasks\/FIXPR-\d+$/.test(pathname)) {
        body = tasks.find(task => task.id === pathname.split('/').pop());
      }
      await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
    });
    await page.goto(base);
    await page.evaluate(async () => {
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      const realNow = Date.now;
      globalThis.__fixPrClockOffset = 0;
      Date.now = () => realNow() + globalThis.__fixPrClockOffset;
      const realFetch = globalThis.fetch;
      globalThis.fetch = async (input, options) => {
        const url = new URL(typeof input === 'string' ? input : input.url, window.location.href);
        if (url.pathname.replace(/^\/api\/on\/[^/]+/, '/api') === '/api/tasks') globalThis.__fixPrTaskListRequests = (globalThis.__fixPrTaskListRequests || 0) + 1;
        return realFetch(input, options);
      };
      await import('/app.js');
    });
    const row = id => page.locator(`#tasks-body .row[data-key="task-${id}"]`);
    await row('FIXPR-1').waitFor({ state: 'visible' });
    await row('FIXPR-1').locator('.title').click();
    await fixPrButton.waitFor({ state: 'visible' });
    await page.waitForFunction(() => {
      const btn = document.querySelector('#task-detail-body .action.fix-pr');
      return btn && btn.disabled && (btn.getAttribute('title') || '').includes('nothing to repair');
    });
    check(counts().total === 1 && counts().forTask('FIXPR-1') === 1, `Opening the review task must read readiness once: ${JSON.stringify(readinessRequests)}`);
    await page.screenshot({ path: path.join(evidence, 'fix-pr-disabled-1440.png') });
    // A refresh inside the cache interval reuses the detail and the reading.
    await refreshAndSettle();
    check(counts().total === 1, `A refresh inside the cache interval must not re-read readiness: ${JSON.stringify(readinessRequests)}`);
    check((await buttonState()).disabled, 'Fix PR must stay disabled while readiness is unchanged inside the interval');
    // The pull request becomes repairable on GitHub; the task itself does not
    // change. After the interval, the real Refresh button must fetch again.
    readiness = { available: true, pr_number: 42, blocker: 'conflict' };
    await page.evaluate(() => { globalThis.__fixPrClockOffset += 61000; });
    await refreshAndSettle();
    await page.waitForFunction(() => {
      const btn = document.querySelector('#task-detail-body .action.fix-pr');
      return btn && !btn.disabled && (btn.getAttribute('title') || '').includes('Hand pull request #42');
    });
    check(counts().total === 2, `A refresh after the cache interval must re-read readiness exactly once: ${JSON.stringify(readinessRequests)}`);
    await page.screenshot({ path: path.join(evidence, 'fix-pr-enabled-1440.png') });
    await page.setViewportSize({ width: 375, height: 812 });
    await page.screenshot({ path: path.join(evidence, 'fix-pr-enabled-375.png') });
    await page.setViewportSize({ width: 1440, height: 900 });
    // While the next post-expiry read is pending, repeated refreshes must not
    // issue a duplicate request for the same task.
    readiness = { available: false, reason: 'no_blocker', pr_number: 42 };
    holdReadiness = true;
    await page.evaluate(() => { globalThis.__fixPrClockOffset += 61000; });
    await refreshAndSettle();
    await page.waitForFunction(() => {
      const btn = document.querySelector('#task-detail-body .action.fix-pr');
      return btn && btn.disabled && (btn.getAttribute('title') || '').includes('checking the pull request');
    });
    check(counts().total === 3, `Expiry must start exactly one new readiness read: ${JSON.stringify(readinessRequests)}`);
    await refreshAndSettle();
    await refreshAndSettle();
    check(counts().total === 3, `Refreshes while a readiness read is pending must not duplicate it: ${JSON.stringify(readinessRequests)}`);
    holdReadiness = false;
    for (const entry of held.splice(0)) await entry.fulfill();
    await page.waitForFunction(() => {
      const btn = document.querySelector('#task-detail-body .action.fix-pr');
      return btn && btn.disabled && (btn.getAttribute('title') || '').includes('nothing to repair');
    });
    await page.setViewportSize({ width: 375, height: 812 });
    await page.screenshot({ path: path.join(evidence, 'fix-pr-disabled-375.png') });
    // Tasks outside the Fix PR gate never read readiness when selected.
    await page.setViewportSize({ width: 1440, height: 900 });
    await row('FIXPR-2').locator('.title').click();
    await page.waitForFunction(() => document.querySelector('#task-detail-body #detail-FIXPR-2'));
    await row('FIXPR-3').locator('.title').click();
    await page.waitForFunction(() => document.querySelector('#task-detail-body #detail-FIXPR-3'));
    check(counts().forTask('FIXPR-2') === 0 && counts().forTask('FIXPR-3') === 0, `Non-review and no-PR tasks must not read readiness: ${JSON.stringify(readinessRequests)}`);
    check(errors.length === 0, errors.join('\n'));
    fs.writeFileSync(path.join(evidence, 'fix-pr-readiness-result.json'), JSON.stringify({ passed: true, initialRead: 1, withinIntervalReads: 1, afterExpiryReads: 2, pendingReads: 3, gatedTasksRead: 0, widths: [1440, 375] }, null, 2));
  } finally {
    for (const entry of held.splice(0)) await entry.fulfill().catch(() => {});
    await page.close();
  }
}
