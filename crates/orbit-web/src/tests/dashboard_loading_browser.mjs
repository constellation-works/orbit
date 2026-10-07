// Usage: node dashboard_loading_browser.mjs /path/to/playwright/index.mjs /evidence/directory
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const scenarios = fileURLToPath(new URL('./dashboard_loading.mjs', import.meta.url));

// A friction's task may be outside both the active status filter and the
// current server page. Follow its actual link and require the full detail.
async function assertFrictionTaskLinks(page) {
  await page.evaluate(async () => {
    const { setWorkspace } = await import('/js/common.js');
    setWorkspace('one');
    await globalThis.showTaskPaginationEvidence();
    const fixtureFetch = globalThis.fetch;
    globalThis.frictionLinkFixtureFetch = fixtureFetch;
    globalThis.frictionLinkedTask = { id: 'LINK-1', priority: 'medium' };
    globalThis.fetch = async (path, options) => {
      const url = new URL(path, window.location.href);
      const response = payload => ({ ok: true, status: 200, json: async () => payload });
      if (url.pathname === '/api/frictions') {
        return response({ items: url.searchParams.get('status') === 'open' ? [{
          id: 'FRICTION-1', title: 'Task navigation regression', status: 'open',
          during_task: 'LINK-1', created_at: '2026-10-04T12:00:00Z', tags: [],
        }] : [], tags: [] });
      }
      if (url.pathname === '/api/frictions/stats') return response({});
      if (url.pathname === '/api/tasks/LINK-1' && url.searchParams.get('workspace') === 'one') {
        return response(globalThis.frictionLinkedTask);
      }
      return fixtureFetch(path, options);
    };
  });
  try {
    for (const status of ['done', 'archived', 'rejected', 'someday', 'in-progress']) {
      await page.evaluate(async status => {
        Object.assign(globalThis.frictionLinkedTask, {
          status, title: `Linked ${status} task`, description: `Detail for linked ${status} task`,
        });
        const { setActiveTab } = await import('/js/router.js');
        setActiveTab('knowledge/frictions');
      }, status);
      await page.locator('#friction-detail .knowledge-link-row').click();
      const pinned = page.locator('#tasks-body [data-key="pinned-LINK-1"]');
      await pinned.waitFor({ state: 'visible', timeout: 5000 });
      if (await pinned.locator('.title').textContent() !== `Linked ${status} task`
        || !(await pinned.locator('.row-detail').textContent()).includes(`Detail for linked ${status} task`)) {
        throw new Error(`Friction link did not open the ${status} task's full detail`);
      }
      if (await page.locator('#task-filter .chip[data-status="done"]').getAttribute('aria-pressed') !== 'false') {
        throw new Error('Opening a friction task must retain the normal Tasks route status filter');
      }
      if (await page.locator('#tasks-body .task-action-notice').count()) {
        throw new Error('Opening a friction task must not fall back to copying its ID');
      }
      await pinned.locator('button[title="Dismiss global task detail"]').click();
    }
  } finally {
    await page.evaluate(() => {
      globalThis.fetch = globalThis.frictionLinkFixtureFetch;
      delete globalThis.frictionLinkFixtureFetch;
      delete globalThis.frictionLinkedTask;
    });
  }
}

async function assertVisibleTaskRow(page, viewport, pageName) {
  const visible = await page.evaluate(() => {
    const row = document.querySelector('#tasks-body .row[data-key^="task-"]:not(.header)');
    const body = document.getElementById('tasks-body');
    if (!row || !body) return { visible: false, reason: 'task row or body missing' };

    const rowRect = row.getBoundingClientRect();
    const bodyRect = body.getBoundingClientRect();
    const viewportRect = { top: 0, right: window.innerWidth, bottom: window.innerHeight, left: 0 };
    const intersection = (rect, bounds) => ({
      left: Math.max(rect.left, bounds.left),
      top: Math.max(rect.top, bounds.top),
      right: Math.min(rect.right, bounds.right),
      bottom: Math.min(rect.bottom, bounds.bottom),
    });
    const area = (rect) => Math.max(0, rect.right - rect.left) * Math.max(0, rect.bottom - rect.top);
    const bodyIntersection = intersection(rowRect, bodyRect);
    const viewportIntersection = intersection(rowRect, viewportRect);
    const visibleIntersection = intersection(bodyIntersection, viewportIntersection);
    const paintPoint = {
      x: (visibleIntersection.left + visibleIntersection.right) / 2,
      y: (visibleIntersection.top + visibleIntersection.bottom) / 2,
    };
    const topmost = area(visibleIntersection) > 0 ? document.elementFromPoint(paintPoint.x, paintPoint.y) : null;

    return {
      visible: area(bodyIntersection) > 0 && area(viewportIntersection) > 0 && row.contains(topmost),
      row: rowRect.toJSON(),
      body: bodyRect.toJSON(),
      viewport: viewportRect,
      paintTarget: topmost?.className || topmost?.id || null,
    };
  });
  if (!visible.visible) throw new Error(`First task row is clipped on ${pageName} at ${viewport.width}px: ${JSON.stringify(visible)}`);
}

// Every cell of a failed run step must lie inside the run-detail panel and the
// viewport, have width, paint on top, and show its whole text: the panel
// clips overflow, so anything outside it is unreadable.
async function assertReadableFailedStep(page, width) {
  const cells = await page.evaluate(() => {
    const panel = document.getElementById('run-detail-panel').getBoundingClientRect();
    const row = document.querySelector('#run-steps-body .step-row');
    const named = {
      index: row.querySelector('.idx'),
      target: row.querySelector('.target'),
      state: row.querySelector('.state-label'),
      duration: row.querySelector('.duration'),
      exit: row.querySelector('.exit'),
    };
    return Object.entries(named).map(([name, cell]) => {
      const rect = cell.getBoundingClientRect();
      const topmost = document.elementFromPoint((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
      return {
        name,
        text: cell.textContent,
        className: cell.className,
        rect: rect.toJSON(),
        panel: { left: panel.left, right: panel.right },
        inside: rect.width > 0 && rect.height > 0 && rect.left >= panel.left - 0.5 && rect.right <= panel.right + 0.5
          && rect.left >= 0 && rect.right <= window.innerWidth,
        complete: cell.scrollWidth <= cell.clientWidth + 1,
        painted: cell.contains(topmost),
      };
    });
  });
  for (const cell of cells) {
    if (!cell.inside || !cell.complete || !cell.painted) throw new Error(`Failed step ${cell.name} unreadable at ${width}px: ${JSON.stringify(cell)}`);
  }
  const text = Object.fromEntries(cells.map(cell => [cell.name, cell.text]));
  if (text.target !== 'activity:agent_implement' || text.state !== 'failed' || text.duration === '' || text.exit !== '1') {
    throw new Error(`Failed step values missing at ${width}px: ${JSON.stringify(text)}`);
  }
  if (!cells.find(cell => cell.name === 'exit').className.includes('fail')) throw new Error('Nonzero exit code must be marked failed');
}

async function assertRunStepLayout(page) {
  await page.evaluate(async () => {
    const detail = await import('/js/run-detail.js');
    detail.clearExpandedStepIndices();
    detail.setActiveRunDetail({
      run: { run_id: 'jrun-layout', state: 'failed' },
      steps: [{ step_index: 3, target_type: 'activity', target_id: 'agent_implement', state: 'failed', duration_ms: 125000, exit_code: 1, error_code: 'agent_failed', error_message: 'exit status 1' }],
    });
    for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'run-detail');
    document.getElementById('run-steps-body').style.display = '';
    detail.renderRunSteps();
  });
  for (const width of [1280, 480, 390]) {
    await page.setViewportSize({ width, height: 900 });
    await page.locator('#run-steps-body .step-row').scrollIntoViewIfNeeded();
    await assertReadableFailedStep(page, width);
    const row = page.locator('#run-steps-body .step-row');
    await row.click();
    const stepDetail = page.locator('#run-steps-body .step-detail');
    await stepDetail.waitFor({ state: 'visible' });
    if (await row.getAttribute('aria-expanded') !== 'true') throw new Error(`Step row must report expansion at ${width}px`);
    if (!(await stepDetail.textContent()).includes('exit status 1')) throw new Error('Expanded step must show its error');
    await assertReadableFailedStep(page, width);
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error(`Expanded step overflows the page at ${width}px`);
    await page.screenshot({ path: path.join(evidence, `run-step-${width}.png`) });
    await row.focus();
    await page.keyboard.press('Enter');
    await stepDetail.waitFor({ state: 'detached' });
    if (await row.getAttribute('aria-expanded') !== 'false') throw new Error(`Step row must collapse from the keyboard at ${width}px`);
  }
}

// A pull drain's run detail names the crews its window runs and each crew it
// excluded, with the source and reason, readable inside the panel at desktop
// and narrow widths.
async function assertCrewWindow(page) {
  await page.evaluate(async () => {
    const detail = await import('/js/run-detail.js');
    detail.setActiveRunDetail({
      run: { run_id: 'jrun-pull', job_id: 'workspace_pull_pipeline', state: 'running' },
      steps: [],
      crew_window: {
        checked_at: '2026-10-04T13:06:00Z',
        runnable: ['luna'],
        default_crew: 'luna',
        excluded: [
          { crew: 'gemini-flash', source: 'provider_unavailable', reason: 'ORB-1 failed: Antigravity terminal error: authentication failed or timed out' },
          { crew: 'opus', source: 'preflight', reason: 'provider `claude` CLI `claude` was not found on this host' },
        ],
      },
    });
    for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'run-detail');
    detail.renderRunDetailMeta();
  });
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 900 });
    const panel = page.locator('#run-detail-meta .crew-window');
    await panel.scrollIntoViewIfNeeded();
    if (!(await panel.isVisible())) throw new Error(`Crew window invisible at ${width}px`);
    const text = await panel.textContent();
    for (const expected of ['crews runnable: luna', 'excluded gemini-flash (provider unavailable): ORB-1 failed', 'excluded opus (preflight)']) {
      if (!text.includes(expected)) throw new Error(`Crew window missing "${expected}" at ${width}px: ${text}`);
    }
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error(`Crew window overflows the page at ${width}px`);
    await page.screenshot({ path: path.join(evidence, `run-crew-window-${width}.png`) });
  }
}

// A terminal skew failure must lead with the typed code and its repair, with
// both full run diagnostics and the persisted pass fallback.
async function assertProtocolSkewFailure(page) {
  for (const fallback of [false, true]) {
    await page.evaluate(async fallback => {
      const detail = await import('/js/run-detail.js');
      const message = 'caller fingerprint aaa; owner fingerprint bbb';
      detail.setActiveRunDetail({
        run: {
          run_id: 'jrun-skew', job_id: 'workspace_pull_pipeline', state: 'failed',
          ...(fallback ? { drain_last_pass: { last_pass_error_code: 'protocol_skew', last_pass_error: message, degraded: true } }
            : { error_code: 'protocol_skew', error_message: message }),
        }, steps: [],
      });
      for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'run-detail');
      detail.renderRunDetailMeta();
    }, fallback);
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 900 });
      const failure = page.locator('#run-detail-meta .run-failure');
      await failure.scrollIntoViewIfNeeded();
      if (!await failure.isVisible()) throw new Error('Terminal protocol skew failure is hidden');
      if (await failure.locator('.run-failure-code').textContent() !== 'protocol_skew') throw new Error('Typed protocol skew code missing');
      if (!(await failure.locator('.run-failure-message').textContent()).includes('owner fingerprint bbb')) throw new Error('Skew fingerprints missing');
      if (!(await failure.locator('p').textContent()).includes('restart')) throw new Error('Skew repair missing');
      const bounds = await failure.boundingBox();
      if (bounds.x < 0 || bounds.x + bounds.width > width + 1) throw new Error(`Skew failure clipped at ${width}px`);
      await page.screenshot({ path: path.join(evidence, `run-skew-${fallback}-${width}.png`) });
    }
  }
}

// Every scoreboard metric cell must paint its bar and value inside its own
// agent column, and each value must be reachable by scrolling the matrix's
// own wrapper: a fixed-layout table squeezed below its content width paints
// values into the neighbouring agent's column instead.
async function assertScoreboardLayout(page) {
  await page.evaluate(async () => {
    const { getWindow } = await import('/js/common.js');
    const { renderScoreboard } = await import('/js/scoreboard.js');
    const agent = (base, incidents) => ({
      tasks_created: base, tasks_planned: base + 1, tasks_completed: base + 2,
      pr: { review_comments: base + 3 },
      tool_calls_by_surface: { graph: base * 10, task: base * 11 },
      failed_tool_calls: base, tool_calls: base * 100,
      failure_incidents: incidents, failure_incident_events: incidents === null ? null : base * 7,
      friction: { reported: base + 4 },
    });
    // Two agents report grouped failures and two have an unavailable source,
    // so one row mixes populated pairs with "unavailable" markers.
    renderScoreboard({
      window: getWindow(),
      agents: { codex: agent(123, 45), claude: agent(9876, 888888), gemini: agent(123, null), grok: agent(888888, null) },
      coverage: { failure_incidents: { availability: 'partial' } },
    });
    for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'diagnostics');
    document.getElementById('diagnostics-main').style.display = 'none';
    document.getElementById('diagnostics-scoreboard-main').style.display = 'grid';
  });
  for (const width of [1280, 720, 390]) {
    await page.setViewportSize({ width, height: 900 });
    const layout = await page.evaluate(() => {
      const wrap = document.getElementById('scoreboard-body');
      const table = wrap.querySelector('table.sb2-matrix');
      const heads = [...table.querySelectorAll('thead th.col-agent')];
      const agents = heads.map(th => th.firstChild.textContent);
      const within = (inner, outer) => inner.width > 0 && inner.left >= outer.left - 0.5 && inner.right <= outer.right + 0.5;
      const problems = [];
      heads.forEach((th, index) => {
        const column = th.getBoundingClientRect();
        for (const child of th.querySelectorAll('.totals')) {
          if (!within(child.getBoundingClientRect(), column)) problems.push({ header: agents[index], rect: child.getBoundingClientRect().toJSON(), column: column.toJSON() });
        }
      });
      let unavailable = 0;
      let populated = 0;
      for (const row of table.querySelectorAll('tbody tr.metric')) {
        const cells = [...row.querySelectorAll('td.cell')];
        if (cells.map(td => td.dataset.agent).join() !== agents.join()) problems.push({ row: row.dataset.key, order: cells.map(td => td.dataset.agent) });
        cells.forEach((td, index) => {
          const value = td.querySelector('.sb2-cell .v');
          if (value.querySelector('.unavailable')) unavailable++; else if (!value.classList.contains('dim')) populated++;
          value.scrollIntoView({ block: 'nearest', inline: 'nearest' });
          // Measure only after scrolling so every rect shares one scroll offset.
          const column = heads[index].getBoundingClientRect();
          const cell = td.getBoundingClientRect();
          const bar = td.querySelector('.sb2-cell').getBoundingClientRect();
          const rect = value.getBoundingClientRect();
          const bounds = wrap.getBoundingClientRect();
          const topmost = document.elementFromPoint((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
          const attributed = Math.abs(cell.left - column.left) < 1 && Math.abs(cell.right - column.right) < 1;
          const contained = within(bar, cell) && within(rect, cell);
          const reachable = within(rect, bounds) && rect.left >= 0 && rect.right <= window.innerWidth && td.contains(topmost);
          if (!attributed || !contained || !reachable) {
            problems.push({ row: row.dataset.key, agent: td.dataset.agent, text: value.textContent, attributed, contained, reachable, value: rect.toJSON(), bar: bar.toJSON(), cell: cell.toJSON(), column: column.toJSON() });
          }
        });
      }
      wrap.scrollLeft = 0;
      return {
        problems,
        unavailable,
        populated,
        scrolls: wrap.scrollWidth > wrap.clientWidth,
        pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
      };
    });
    if (layout.problems.length) throw new Error(`Scoreboard values leave their agent columns at ${width}px: ${JSON.stringify(layout.problems.slice(0, 4))}`);
    if (layout.unavailable !== 2 || layout.populated < 20) throw new Error(`Scoreboard fixture did not render populated and unavailable metrics at ${width}px: ${JSON.stringify(layout)}`);
    if (layout.pageOverflow) throw new Error(`Scoreboard widens the page instead of scrolling its matrix at ${width}px`);
    if (width === 1280 && layout.scrolls) throw new Error('Desktop scoreboard matrix must fit without horizontal scrolling');
    await page.screenshot({ path: path.join(evidence, `scoreboard-${width}.png`), fullPage: true });
  }
}

// ORB-14481: from 1024px up the top bar is one fixed-height row on every
// route, and the host verdict flipping to throttled neither re-wraps it nor
// moves Refresh. The fixture is the worst case the bar can hold: wide counts,
// both alert tiles, the drain pill, and a held host chip.
async function assertTopbarSingleRow(page) {
  const routes = ['tasks', 'operations/routines', 'config/effective', 'knowledge/frictions', 'plugins'];
  const snapshot = () => page.evaluate(() => {
    const rect = id => document.getElementById(id).getBoundingClientRect();
    const bar = document.querySelector('.topbar').getBoundingClientRect();
    const children = [...document.querySelectorAll('.topbar .crumb, .topbar .kpi, .topbar #refresh-btn, .topbar #global-drain-state')]
      .filter(node => node.getClientRects().length);
    // Items are centred in the row, so one row means one shared vertical centre.
    const centres = children.map(node => { const r = node.getBoundingClientRect(); return r.top + r.height / 2; });
    const rows = Math.max(...centres) - Math.min(...centres) <= 2 ? 1 : 2;
    const refresh = rect('refresh-btn');
    const chip = document.querySelector('#host-resource-chips .host-resource').getBoundingClientRect();
    const strip = document.getElementById('health-strip');
    const lastTile = [...strip.querySelectorAll('.kpi')].filter(node => node.getClientRects().length).pop().getBoundingClientRect();
    return {
      height: bar.height, refreshLeft: refresh.left, chipWidth: chip.width, rows,
      refreshRight: refresh.right, barRight: bar.right, stripOverflows: strip.scrollWidth > strip.clientWidth,
      overlaps: lastTile.right > rect('refresh-btn').left || (!document.getElementById('global-drain-state').hidden && lastTile.right > rect('global-drain-state').left),
    };
  });
  await page.evaluate(async () => {
    const { renderHostResources } = await import('/js/host-resources.js');
    for (const id of ['tile-failed-value', 'tile-denials-value', 'tile-active-value', 'tile-events-value']) document.getElementById(id).textContent = '12,345';
    for (const id of ['tile-failed', 'tile-denials']) document.getElementById(id).classList.add('tile-alert');
    const drain = document.getElementById('global-drain-state');
    drain.hidden = false;
    drain.dataset.drainState = 'draining';
    globalThis.topbarHost = throttle => renderHostResources({
      cpu: { percent: 193.7, severity: 'critical' }, memory: { percent: 40, severity: 'ok' },
      disk: { path: '/workspace', percent: 50, severity: 'ok' }, sample_age_seconds: 1, max_age_seconds: 15,
      stale: false, throttle, pressures: throttle ? [{ resource: 'cpu' }] : [], reason: 'cpu load high', thresholds: { enabled: true },
    });
  });
  try {
    for (const width of [1024, 1280, 1440]) {
      await page.setViewportSize({ width, height: 900 });
      const heights = new Set();
      for (const route of routes) {
        await page.evaluate(async route => {
          const { setActiveTab } = await import('/js/router.js');
          setActiveTab(route);
        }, route);
        const flips = [];
        for (const throttle of [false, true]) {
          await page.evaluate(throttle => globalThis.topbarHost(throttle), throttle);
          flips.push(await snapshot());
        }
        for (const state of flips) {
          heights.add(state.height);
          if (state.height > 64 || state.rows !== 1) throw new Error(`Top bar wraps on #${route} at ${width}px: ${JSON.stringify(state)}`);
          if (state.stripOverflows || state.overlaps || state.refreshRight > state.barRight + 0.5) throw new Error(`Top bar content overflows on #${route} at ${width}px: ${JSON.stringify(state)}`);
        }
        if (flips[0].height !== flips[1].height || flips[0].refreshLeft !== flips[1].refreshLeft || flips[0].chipWidth !== flips[1].chipWidth) {
          throw new Error(`Throttled verdict moved the top bar on #${route} at ${width}px: ${JSON.stringify(flips)}`);
        }
      }
      if (heights.size !== 1) throw new Error(`Top bar height differs across routes at ${width}px: ${[...heights]}`);
      await page.screenshot({ path: path.join(evidence, `topbar-${width}.png`) });
    }
    // Narrower than 1024px the bar may wrap, to two rows at most.
    await page.setViewportSize({ width: 800, height: 900 });
    const narrow = await snapshot();
    if (narrow.height > 100) throw new Error(`Top bar takes more than two rows at 800px (taller than 100px): ${JSON.stringify(narrow)}`);
  } finally {
    await page.evaluate(async () => {
      const { setActiveTab } = await import('/js/router.js');
      document.getElementById('global-drain-state').hidden = true;
      setActiveTab('tasks');
    });
  }
}

// Runs, Audit Events and Errors restack as cards on a phone and must show each
// row's state, name and time without any sideways scrolling; the tables that
// still scroll (Metrics, Scoreboard) keep their first column pinned and show a
// scroll edge. Fixtures stay in this function so it owns its fetch.
async function assertNarrowTableLayouts(page) {
  await page.evaluate(async () => {
    const now = Date.now();
    const iso = (minutes) => new Date(now - minutes * 60000).toISOString();
    const response = payload => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
    const runs = ['failed', 'success', 'running'].map((state, index) => ({
      run_id: `jrun-20261007-0717-c${index}-with-a-long-identifier`, job_id: `a-job-with-a-long-name-${index}`, state,
      created_at: iso(index + 1), duration_ms: 90000,
    }));
    const events = ['success', 'failure'].map((status, index) => ({
      id: 9000 + index, timestamp: iso(index + 1), role: 'implementer', tool_name: 'orbit.task.update',
      command: 'task', subcommand: 'update', target_type: 'task', target_id: `TASK-${index}`,
      status, exit_code: index, duration_ms: 1234,
    }));
    const errors = [0, 1].map(index => ({
      ts: iso(index + 1), source: 'step_failed', job_run: `jrun-error-${index}`, provider: 'claude', step: 'implement',
      message: 'a long failure message that has to wrap onto several lines inside a phone-width card '.repeat(3),
    }));
    const metrics = [0, 1, 2].map(index => ({
      ts: iso(index + 1), step: `step-with-a-long-name-${index}`, actor_identity: 'claude-sonnet-5-5',
      token_usage: 123456, tool_invocations: 77, step_duration_ms: 456000, retry_count: 1,
    }));
    const fixtureFetch = globalThis.fetch;
    globalThis.narrowTableFixtureFetch = fixtureFetch;
    globalThis.fetch = async (path, options) => {
      const url = new URL(path, window.location.href);
      if (url.pathname === '/api/job-runs') return response({ items: runs, total: runs.length, limit: 50, truncated: false });
      if (url.pathname === '/api/audit') return response(events);
      if (url.pathname === '/api/diagnostics/errors') return response(errors);
      if (url.pathname === '/api/diagnostics/metrics') return response(metrics);
      return fixtureFetch(path, options);
    };
  });
  const refresh = async () => {
    await page.evaluate(() => document.getElementById('refresh-btn').click());
  };
  const pageOverflow = () => page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  const scrolls = id => page.evaluate(target => {
    const node = document.getElementById(target);
    return node.scrollWidth > node.clientWidth + 1;
  }, id);
  // Every named element must lie inside the viewport with its text unclipped.
  const assertVisible = async (selectors, scope, label, width) => {
    const problems = await page.evaluate(({ selectors, scope }) => selectors.map(selector => {
      const node = document.querySelector(`${scope} ${selector}`);
      if (!node) return { selector, missing: true };
      const rect = node.getBoundingClientRect();
      const topmost = document.elementFromPoint((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
      const complete = node.scrollWidth <= node.clientWidth + 1;
      const inside = rect.width > 0 && rect.left >= 0 && rect.right <= window.innerWidth + 0.5;
      return inside && complete && node.contains(topmost) ? null : { selector, text: node.textContent, rect: rect.toJSON(), complete, inside };
    }).filter(Boolean), { selectors, scope });
    if (problems.length) throw new Error(`${label} hides content at ${width}px: ${JSON.stringify(problems)}`);
  };
  await page.setViewportSize({ width: 375, height: 900 });

  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/runs'));
  await refresh();
  await page.locator('#runs-body .runs-row[data-key^="run-"]').first().waitFor({ state: 'visible', timeout: 5000 });
  if (await scrolls('runs-body') || await pageOverflow()) throw new Error('Runs must not scroll sideways at 375px');
  await assertVisible(['.runs-scope-note', '.runs-filter', '.runs-filter-button.active', '.runs-row[data-key^="run-"] .state', '.runs-row[data-key^="run-"] .id', '.runs-row[data-key^="run-"] .when'], '#runs-body', 'Runs', 375);
  await page.screenshot({ path: path.join(evidence, 'runs-375.png'), fullPage: true });

  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('audit/events'));
  await refresh();
  await page.locator('#audit-body tr.audit-row').first().waitFor({ state: 'visible', timeout: 5000 });
  if (await scrolls('audit-body') || await pageOverflow()) throw new Error('Audit events must not scroll sideways at 375px');
  await assertVisible(['tr.audit-row .c-status', 'tr.audit-row .c-command', 'tr.audit-row .c-time'], '#audit-body', 'Audit events', 375);
  await page.screenshot({ path: path.join(evidence, 'audit-375.png'), fullPage: true });

  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/errors'));
  await refresh();
  await page.locator('#diag-body tbody tr').first().waitFor({ state: 'visible', timeout: 5000 });
  if (await scrolls('diag-body') || await pageOverflow()) throw new Error('Errors must not scroll sideways at 375px');
  await assertVisible(['tbody tr .c-source', 'tbody tr .c-job_run', 'tbody tr .c-ts', 'tbody tr .c-message'], '#diag-body', 'Errors', 375);
  await page.screenshot({ path: path.join(evidence, 'errors-375.png'), fullPage: true });

  // Still-scrolling tables: first column pinned, scroll edge painted.
  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/metrics'));
  await refresh();
  await page.locator('#diag-body tbody tr').first().waitFor({ state: 'visible', timeout: 5000 });
  for (const width of [375, 768]) {
    await page.setViewportSize({ width, height: 900 });
    const pinned = await page.evaluate(() => {
      const wrap = document.getElementById('diag-body');
      const table = wrap.querySelector('table.scoreboard-table');
      const cell = table.querySelector('tbody tr td:first-child');
      wrap.scrollLeft = wrap.scrollWidth;
      const wrapRect = wrap.getBoundingClientRect();
      const rect = cell.getBoundingClientRect();
      const topmost = document.elementFromPoint(rect.left + 4, (rect.top + rect.bottom) / 2);
      return {
        scrolls: wrap.scrollWidth > wrap.clientWidth + 1,
        scrolled: wrap.scrollLeft > 0,
        stays: Math.abs(rect.left - wrapRect.left) < 1.5,
        painted: cell.contains(topmost),
        edge: getComputedStyle(wrap).backgroundImage.includes('gradient'),
      };
    });
    if (width === 375 && !pinned.scrolls) throw new Error('Metrics must still scroll sideways at 375px for this check to mean anything');
    if (pinned.scrolls && !(pinned.scrolled && pinned.stays && pinned.painted && pinned.edge)) {
      throw new Error(`Metrics first column or scroll edge missing at ${width}px: ${JSON.stringify(pinned)}`);
    }
    await page.screenshot({ path: path.join(evidence, `metrics-${width}.png`), fullPage: true });
  }
  await page.setViewportSize({ width: 375, height: 900 });
  await page.evaluate(async () => {
    const { renderScoreboard } = await import('/js/scoreboard.js');
    const { getWindow } = await import('/js/common.js');
    const agent = base => ({ tasks_created: base, tasks_planned: base, tasks_completed: base, failed_tool_calls: base, tool_calls: base * 100 });
    renderScoreboard({ window: getWindow(), agents: { codex: agent(1), claude: agent(2), gemini: agent(3), grok: agent(4) } });
    for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'diagnostics');
    document.getElementById('diagnostics-main').style.display = 'none';
    document.getElementById('diagnostics-scoreboard-main').style.display = 'grid';
  });
  const matrix = await page.evaluate(() => {
    const wrap = document.getElementById('scoreboard-body');
    const label = wrap.querySelector('table.sb2-matrix td.m-label');
    label.scrollIntoView({ block: 'center' });
    wrap.scrollLeft = wrap.scrollWidth;
    const rect = label.getBoundingClientRect();
    const topmost = document.elementFromPoint(rect.left + 4, (rect.top + rect.bottom) / 2);
    return {
      scrolled: wrap.scrollLeft > 0,
      stays: Math.abs(rect.left - wrap.getBoundingClientRect().left) < 1.5,
      painted: label.contains(topmost),
      edge: getComputedStyle(wrap).backgroundImage.includes('gradient'),
    };
  });
  if (!(matrix.scrolled && matrix.stays && matrix.painted && matrix.edge)) {
    throw new Error(`Scoreboard metric column or scroll edge missing at 375px: ${JSON.stringify(matrix)}`);
  }
  await page.screenshot({ path: path.join(evidence, 'scoreboard-375-pinned.png'), fullPage: true });

  // Desktop keeps the plain tables: no cards, no pinned column, no scroll edge.
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.evaluate(async () => {
    document.getElementById('diagnostics-scoreboard-main').style.display = 'none';
    document.getElementById('diagnostics-main').style.display = '';
    (await import('/js/router.js')).setActiveTab('audit/events');
  });
  await refresh();
  await page.locator('#audit-body tr.audit-row').first().waitFor({ state: 'visible', timeout: 5000 });
  const desktop = await page.evaluate(() => {
    const row = document.querySelector('#audit-body tr.audit-row');
    const cell = row.querySelector('td');
    return {
      rowDisplay: getComputedStyle(row).display,
      headVisible: getComputedStyle(document.querySelector('#audit-body thead')).display !== 'none',
      cellPosition: getComputedStyle(cell).position,
      edge: getComputedStyle(document.getElementById('audit-body')).backgroundImage,
    };
  });
  if (desktop.rowDisplay !== 'table-row' || !desktop.headVisible || desktop.cellPosition !== 'static' || desktop.edge !== 'none') {
    throw new Error(`Desktop audit table changed: ${JSON.stringify(desktop)}`);
  }
  await page.evaluate(() => {
    globalThis.fetch = globalThis.narrowTableFixtureFetch;
    delete globalThis.narrowTableFixtureFetch;
  });
}

const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = name === '/test.mjs' ? { data: fs.readFileSync(scenarios), type: 'text/javascript' } : dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const failures = [];
  page.on('pageerror', error => failures.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=one#tasks`);
  await page.evaluate(() => {
    globalThis.nativeFetch = globalThis.fetch;
    globalThis.setInterval = () => 0;
    globalThis.EventSource = class { close() {} };
  });
  await page.addScriptTag({ type: 'module', url: '/test.mjs' });
  await page.waitForFunction(() => globalThis.loadingTestsPassed, undefined, { timeout: 15000 }).catch(error => {
    throw new Error(`${error.message}\nPage errors: ${failures.join('\n')}`);
  });
  if (failures.length) throw new Error(failures.join('\n'));
  await assertFrictionTaskLinks(page);
  await page.evaluate(() => globalThis.showTaskPaginationEvidence());
  await page.waitForFunction(() => document.getElementById('tasks-count').textContent === '1–20 of 55');
  for (const viewport of [{ name: 'desktop', width: 1280 }, { name: 'mobile', width: 390 }]) {
    await page.setViewportSize({ width: viewport.width, height: 900 });
    await page.evaluate(() => {
      window.scrollTo(0, 0);
      document.getElementById('tasks-body').scrollTop = 0;
    });
    const pager = page.locator('.task-pagination');
    if (!(await pager.isVisible())) throw new Error(`Task pagination invisible at ${viewport.width}px`);
    if (!(await page.locator('#tasks-next').isEnabled())) throw new Error('First task page must enable Next');
    if (await page.locator('#tasks-previous').isEnabled()) throw new Error('First task page must disable Previous');
    await assertVisibleTaskRow(page, viewport, 'page 1');
    await page.screenshot({ path: path.join(evidence, `task-pagination-${viewport.name}.png`), fullPage: true });
  }
  await page.evaluate(() => { document.getElementById('tasks-body').scrollTop = 120; });
  await page.locator('#tasks-next').click();
  await page.waitForFunction(() => document.getElementById('tasks-count').textContent === '21–40 of 55');
  if (await page.locator('#tasks-body').evaluate((body) => body.scrollTop !== 0)) throw new Error('Task page navigation must reset the task-body scroll position');
  if (!(await page.locator('#tasks-previous').isEnabled())) throw new Error('Second task page must enable Previous');
  for (const viewport of [{ name: 'desktop', width: 1280 }, { name: 'mobile', width: 390 }]) {
    await page.setViewportSize({ width: viewport.width, height: 900 });
    await page.evaluate(() => {
      window.scrollTo(0, 0);
      document.getElementById('tasks-body').scrollTop = 0;
    });
    await assertVisibleTaskRow(page, viewport, 'page 2');
    await page.screenshot({ path: path.join(evidence, `task-pagination-${viewport.name}-page-2.png`), fullPage: true });
  }
  await page.locator('#task-filter .chip[data-status="done"]').click();
  await page.waitForFunction(() => document.getElementById('task-filter-summary').textContent.includes('done'));
  await page.locator('#task-filter .chip[data-role="all"]').click();
  const firstTask = page.locator('#tasks-body .row[data-key^="task-"]:not(.header)').first();
  // Open it the way a person does, by its title: on a narrow row the centre
  // of the box is the crew select, which takes the click for itself.
  await firstTask.locator('.title').click();
  const detail = page.locator('#tasks-body .row-detail').first();
  await detail.waitFor({ state: 'visible' });
  const pageOverflowsHorizontally = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  if (pageOverflowsHorizontally) throw new Error('Task filters or expanded details introduced horizontal page clipping');
  await assertNarrowTableLayouts(page);
  await page.evaluate(() => globalThis.showDiagnosticsEvidence());
  // Hold a real visible panel in refresh, then inspect its rendered accessible
  // feedback and retry affordance at desktop and narrow widths.
  await page.evaluate(() => {
    globalThis.fetch = (_path, options) => new Promise((_resolve, reject) => {
      options?.signal?.addEventListener('abort', () => reject(new DOMException('Timed out', 'AbortError')));
    });
    document.getElementById('refresh-btn').click();
  });
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 900 });
    const feedback = page.locator('#diag-body [role="status"]');
    if (!(await feedback.isVisible())) throw new Error(`Refresh feedback invisible at ${width}px`);
    if (!(await feedback.textContent()).includes('Refreshing')) throw new Error('Missing retained-data feedback');
    if (await feedback.getAttribute('aria-live') !== 'polite') throw new Error('Missing accessible live feedback');
    if (!(await page.locator('#refresh-btn').isEnabled())) throw new Error('Retry disabled');
    await page.screenshot({ path: path.join(evidence, `refresh-${width}.png`) });
  }
  await assertScoreboardLayout(page);
  await assertRunStepLayout(page);
  await assertCrewWindow(page);
  await assertTopbarSingleRow(page);
  await assertProtocolSkewFailure(page);
  await new Promise(resolve => server.close(resolve));
  await page.evaluate(() => {
    globalThis.fetch = globalThis.nativeFetch;
    document.getElementById('refresh-btn').click();
  });
  await page.waitForFunction(() => document.getElementById('meta-text').textContent.includes('offline'));
  if (!(await page.locator('#conn-status').getAttribute('class')).includes('red')) throw new Error('Stopped server must show red connection status');
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, scenarios: 'Runs, Audit events and Errors as cards without sideways scrolling and Metrics/Scoreboard pinned first column with scroll edge at 375px, unchanged tables at 1280px; Scoreboard values attributed to and contained in their agent columns, reachable by matrix scrolling, for populated and unavailable metrics at 1280px, 720px and 390px; Task pagination page 1/page 2 with visible, unoccluded first rows and accessible Previous/Next at 1280px and 390px; failed run step target, state, duration and exit code readable with click and keyboard expansion at 1280px, 480px and 390px; single-row top bar of identical height across Tasks, Automation, Settings, Knowledge and Plugins with a fixed host chip and Refresh offset when the throttle verdict flips at 1024px, 1280px and 1440px; terminal protocol skew code, fingerprints and repair at 1280px and 390px; pull drain crew window runnable crews and preflight/provider-unavailable exclusions readable at 1280px and 390px; Tasks, Recent runs, Errors, Operations: cold, stale refresh, scope changes, reordered responses, empty success, network error; Metrics HTTP failure isolation and network offline/recovery' }, null, 2));
  console.log('Chromium dashboard lifecycle and accessible visible feedback passed.');
} finally {
  await browser?.close();
  if (server.listening) await new Promise(resolve => server.close(resolve));
}
