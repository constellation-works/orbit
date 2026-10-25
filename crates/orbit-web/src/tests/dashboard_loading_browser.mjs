// Usage: node dashboard_loading_browser.mjs /path/to/playwright/index.mjs /evidence/directory [--run-detail|--locks-only|--auth-exclusions|--task-lookup|--clock]
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';
import { assertRunDetailActions, assertRunDetailPresentation } from './dashboard_run_detail_browser.mjs';
import { assertWorkspaceScope } from './dashboard_workspace_scope_browser.mjs';
import { assertRunTaskLabels, assertTaskLinkScopeHistory, assertTaskLookupClear, assertFixPrReadinessRefresh } from './dashboard_run_tasks_browser.mjs';
import { assertClockRepaint } from './dashboard_clock_browser.mjs';
import { assertTaskTitleLayout } from '../../tests/http_api/dashboard_task_titles_browser.mjs';
import { assertPullRequestLinks } from '../../tests/http_api/dashboard_pull_request_links_browser.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const scenarios = fileURLToPath(new URL('./dashboard_loading.mjs', import.meta.url));
const locksOnly = process.argv[4] === '--locks-only';
const authExclusionsOnly = process.argv[4] === '--auth-exclusions';
const clockOnly = process.argv[4] === '--clock';
const locksOnlyScenario = `
const tasks = Array.from({ length: 20 }, (_, index) => ({
  id: 'BASE-' + index, title: 'Base task ' + index, status: 'in-progress', priority: 'medium',
}));
globalThis.fetch = async path => {
  const url = new URL(path, window.location.href);
  let payload = { items: [], total: 0, limit: 50, truncated: false };
  if (url.pathname === '/api/workspaces') payload = [{ id: 'one', name: 'one', status: 'active', is_default: true }];
  if (url.pathname === '/api/tasks') payload = { items: tasks, total: 55, limit: 20, truncated: true, next_cursor: 'page-2' };
  if (url.pathname === '/api/tasks/locks') payload = { total_locked: 0, total_tasks: 0, by_task: [] };
  if (url.pathname === '/api/crews') payload = { default_crew: null, crews: [] };
  if (url.pathname === '/api/workflows/auto/readiness') payload = { capacity: {}, tasks: [] };
  if (url.pathname === '/api/routines') payload = { routines: [], clock: {} };
  return new Response(JSON.stringify(payload), { status: 200 });
};
await import('/app.js');
await new Promise(resolve => setTimeout(resolve, 0));
globalThis.loadingTestsPassed = true;
`;

async function assertAccentFocusRing(control) {
  const focused = await control.evaluate(element => {
    const style = getComputedStyle(element);
    return {
      active: document.activeElement === element,
      focusVisible: element.matches(':focus-visible'),
      outlineWidth: style.outlineWidth,
      outlineStyle: style.outlineStyle,
      outlineColor: style.outlineColor,
    };
  });
  if (!focused.active || !focused.focusVisible || focused.outlineWidth !== '2px'
    || focused.outlineStyle !== 'solid' || focused.outlineColor !== 'rgb(122, 167, 255)') {
    throw new Error(`Keyboard control must show a 2px accent focus ring: ${JSON.stringify(focused)}`);
  }
}

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
    for (const status of ['done', 'archived', 'rejected', 'someday', 'in-progress', 'backlog']) {
      await page.evaluate(async status => {
        Object.assign(globalThis.frictionLinkedTask, {
          status, title: `Linked ${status} task`, description: `Detail for linked ${status} task`,
        });
        const { setActiveTab } = await import('/js/router.js');
        setActiveTab('knowledge/frictions');
      }, status);
      if (status === 'done') {
        await page.keyboard.press('Tab');
        const filter = page.locator('#friction-status-filter');
        await filter.focus();
        await assertAccentFocusRing(filter);
      }
      await page.locator('#friction-detail .knowledge-link-row').click();
      const pinned = page.locator('#task-detail-body [data-key="pinned-LINK-1"]');
      await pinned.waitFor({ state: 'visible', timeout: 5000 });
      if (await page.locator('#task-detail-body .task-pane-title').textContent() !== `Linked ${status} task`
        || !(await pinned.locator('.row-detail').textContent()).includes(`Detail for linked ${status} task`)) {
        throw new Error(`Friction link did not open the ${status} task's full detail`);
      }
      if (await page.locator('#task-filter .chip[data-status="done"]').getAttribute('aria-pressed') !== 'false') {
        throw new Error('Opening a friction task must retain the normal Tasks route status filter');
      }
      if (await page.locator('#tasks-body .task-action-notice').count()) {
        throw new Error('Opening a friction task must not fall back to copying its ID');
      }
      if (status === 'backlog') {
        // Comment is offered only on the Comments tab; Reject stays on Overview.
        for (const [action, tab] of [['comment', 'comments'], ['reject', 'overview']]) {
          await pinned.locator(`[role="tab"][data-tab="${tab}"]`).click();
          const button = pinned.locator(`.actions > .action.${action}`);
          await page.keyboard.press('Tab');
          await button.focus();
          await button.press('Enter');
          // Textareas animate their focus styling for 200 ms.
          await page.waitForTimeout(250);
          await assertAccentFocusRing(pinned.locator(`.${action}-form textarea`));
          await pinned.locator(`.${action}-form .action.cancel`).click();
        }
      }
      await page.locator('#task-detail-pane').getByRole('button', { name: 'Close task detail', exact: true }).click();
    }
  } finally {
    await page.evaluate(() => {
      globalThis.fetch = globalThis.frictionLinkFixtureFetch;
      delete globalThis.frictionLinkFixtureFetch;
      delete globalThis.frictionLinkedTask;
    });
  }
}

// axe's nested-interactive rule: an interactive control cannot contain another
// focusable control. This includes summary, which is the disclosure control for
// details; returns every such ancestor under `root`.
function nestedInteractive(page, root) {
  return page.evaluate(root => {
    const focusable = 'button, select, input, textarea, a[href], [tabindex], [role="button"]';
    const interactive = `${root} button, ${root} [role="button"], ${root} summary`;
    return [...document.querySelectorAll(interactive)]
      .filter(node => node.querySelector(focusable))
      .map(node => node.outerHTML.slice(0, 160));
  }, root);
}

async function assertLockedTaskNavigation(page) {
  const tasks = [
    { id: 'LOCKED-1', title: 'First locked task', status: 'in-progress', priority: 'medium' },
    { id: 'LOCKED-2', title: 'Second locked task', status: 'backlog', priority: 'medium' },
  ];
  await page.evaluate(async tasks => {
    const priorFetch = globalThis.fetch;
    globalThis.lockedTaskFixtureFetch = priorFetch;
    globalThis.fetch = async (path, options) => {
      const url = new URL(path, window.location.href);
      const payload = url.pathname === '/api/tasks'
        ? { items: tasks, total: tasks.length, limit: 50, truncated: false }
        : url.pathname === '/api/tasks/locks'
          ? {
              total_locked: tasks.length,
              total_tasks: tasks.length,
              by_task: tasks.map((task, index) => ({
                ...task,
                context_files: [`file:locked-${index + 1}.rs`],
                job_run_id: `jrun-locked-${index + 1}`,
              })),
            }
          : null;
      if (payload) return new Response(JSON.stringify(payload), { status: 200 });
      return priorFetch(path, options);
    };
    (await import('/js/router.js')).setActiveTab('tasks');
  }, tasks);

  try {
    await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('drain'));
    await page.locator('#refresh-btn').click();
    await page.waitForFunction(() =>
      document.querySelectorAll('#locks-body .lock-task-group').length === 2
      && document.querySelector('#tasks-body [data-key="task-LOCKED-1"]')
      && document.querySelector('#tasks-body [data-key="task-LOCKED-2"]'));

    const nested = await nestedInteractive(page, '#drain-main');
    if (nested.length) throw new Error(`Drain pane nests controls in an interactive element: ${nested.join('\n')}`);

    if (process.env.AXE_CORE_SCRIPT) {
      await page.addScriptTag({ path: process.env.AXE_CORE_SCRIPT });
      const axeNodes = await page.evaluate(async () => {
        const result = await globalThis.axe.run('#drain-main', {
          runOnly: { type: 'rule', values: ['nested-interactive'] },
        });
        return result.violations.flatMap(violation => violation.nodes.map(node => node.target.join(' ')));
      });
      if (axeNodes.length) throw new Error(`Axe nested-interactive found nodes in Drain: ${axeNodes.join('\n')}`);
    }

    for (const task of tasks) {
      await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('drain'));
      const group = page.locator(`#locks-body [data-key="lock-task-${task.id}"]`);
      const summary = group.locator(':scope > details > summary');
      const link = group.locator(':scope > .lock-task-id');
      await summary.focus();
      await page.keyboard.press('Tab');
      if (!(await link.evaluate(node => node === document.activeElement && node.tabIndex === 0))) {
        throw new Error(`${task.id} task link must follow its disclosure in the keyboard tab order`);
      }
      await page.keyboard.press('Enter');
      const row = page.locator(`#tasks-body [data-key="task-${task.id}"]`);
      await page.locator(`#task-detail-body #detail-${task.id}`).waitFor({ state: 'visible' });
      if (!(await row.locator('.title').textContent()).includes(task.title)) {
        throw new Error(`${task.id} task link opened the wrong task`);
      }
    }
  } finally {
    await page.evaluate(async () => {
      globalThis.fetch = globalThis.lockedTaskFixtureFetch;
      delete globalThis.lockedTaskFixtureFetch;
      (await import('/js/router.js')).setActiveTab('tasks');
      document.getElementById('refresh-btn').click();
    });
    await page.waitForFunction(() => document.getElementById('tasks-count').textContent === '1–20 of 55');
  }
}

// Each row list is one Tab stop: only the current row's controls are
// tabbable, Up/Down/Home/End move between rows' disclosure buttons, Enter and
// Space toggle the focused row, and its disclosure keeps focus across a
// refresh that rebuilds the row. `/` jumps to the task search.
async function assertRowKeyboard(page) {
  const activeKey = () => page.evaluate(() => {
    const active = document.activeElement;
    return active?.matches('[data-row-focus]') ? active.closest('[data-key]').dataset.key : active?.id || active?.tagName;
  });
  const tabbableRows = root => page.evaluate(root => [...document.querySelectorAll(`${root} [data-roving-row]`)]
    .map(row => [...row.querySelectorAll('button, select, a[href]')].filter(node => node.tabIndex >= 0).length), root);
  const expectFocus = async (key, message) => {
    const actual = await activeKey();
    if (actual !== key) throw new Error(`${message}: expected focus on ${key}, got ${actual}`);
  };

  const nestedTasks = await nestedInteractive(page, '#tasks-body');
  if (nestedTasks.length) throw new Error(`Task rows nest controls in a button: ${nestedTasks.join('\n')}`);
  const keys = await page.locator('#tasks-body .row[data-key^="task-"]:not(.header)').evaluateAll(rows => rows.map(row => row.dataset.key));
  if (keys.length < 4) throw new Error(`Row keyboard check needs several task rows, got ${keys.length}`);
  const title = key => page.locator(`#tasks-body [data-key="${key}"] .title`);
  if (await title(keys[0]).evaluate(node => node.tagName) !== 'BUTTON') throw new Error('Task title must be the row disclosure button');
  if (await page.locator('#tasks-body .row[role]').count()) throw new Error('Task rows must not carry a role of their own');

  await title(keys[0]).focus();
  const tabbable = await tabbableRows('#tasks-body');
  if (!(tabbable[0] > 0 && tabbable.slice(1).every(count => count === 0))) {
    throw new Error(`Only the current task row may be in the tab order: ${JSON.stringify(tabbable)}`);
  }
  await page.keyboard.press('ArrowDown');
  await expectFocus(keys[1], 'ArrowDown');
  await page.keyboard.press('ArrowUp');
  await expectFocus(keys[0], 'ArrowUp');
  await page.keyboard.press('End');
  await expectFocus(keys.at(-1), 'End');
  await page.keyboard.press('Home');
  await expectFocus(keys[0], 'Home');

  // Tab walks into the current row's controls, then leaves the list.
  await page.keyboard.press('ArrowDown');
  const stops = [];
  for (let press = 0; press < 8; press++) {
    await page.keyboard.press('Tab');
    const inside = await page.evaluate(() => {
      const active = document.activeElement;
      const row = active.closest('#tasks-body [data-key]');
      return row ? `${row.dataset.key}:${active.className.split(' ')[0]}` : null;
    });
    if (!inside) break;
    stops.push(inside);
  }
  if (stops.length === 0 || stops.length > 4 || stops.some(stop => !stop.startsWith(`${keys[1]}:`))) {
    throw new Error(`Tab must cross only the current row's controls before leaving the list: ${JSON.stringify(stops)}`);
  }

  await title(keys[1]).focus();
  await page.keyboard.press('Enter');
  const detailId = `detail-${keys[1].slice('task-'.length)}`;
  await page.locator(`#task-detail-body #${detailId}`).waitFor({ state: 'visible' });
  await expectFocus(keys[1], 'Enter selects and keeps focus on the disclosure');
  if (await title(keys[1]).getAttribute('aria-expanded') !== 'true' || await title(keys[1]).getAttribute('aria-controls') !== 'task-detail-pane') {
    throw new Error('Selected task disclosure must report its state and the pane it opens');
  }
  const nestedExpanded = await nestedInteractive(page, '#tasks-body');
  if (nestedExpanded.length) throw new Error(`Expanded task detail nests controls in a button: ${nestedExpanded.join('\n')}`);
  await page.keyboard.press(' ');
  await page.locator(`#task-detail-body #${detailId}`).waitFor({ state: 'detached' });
  await expectFocus(keys[1], 'Space closes the pane and keeps focus on the disclosure');
  if (await title(keys[1]).getAttribute('aria-expanded') !== 'false') throw new Error('Collapsed task disclosure must report its state');

  // A refresh that changes the task rebuilds its row; focus follows the key.
  await title(keys[2]).focus();
  await page.evaluate(() => {
    const prior = globalThis.fetch;
    globalThis.rowKeyboardFixtureFetch = prior;
    globalThis.rowBeforeRefresh = document.activeElement;
    globalThis.fetch = async (path, options) => {
      const response = await prior(path, options);
      if (new URL(path, window.location.href).pathname !== '/api/tasks' || !response.ok) return response;
      const payload = await response.json();
      payload.items = payload.items.map(task => ({ ...task, title: `${task.title} (refreshed)` }));
      return new Response(JSON.stringify(payload), { status: response.status, headers: response.headers });
    };
    document.getElementById('refresh-btn').click();
  });
  await page.waitForFunction(key => document.querySelector(`#tasks-body [data-key="${key}"] .title`)?.textContent.endsWith('(refreshed)'), keys[2]);
  await expectFocus(keys[2], 'Refresh restores focus to the rebuilt disclosure');
  if (await page.evaluate(() => document.activeElement === globalThis.rowBeforeRefresh)) {
    throw new Error('The refresh check must rebuild the focused row for it to mean anything');
  }
  await page.evaluate(() => {
    globalThis.fetch = globalThis.rowKeyboardFixtureFetch;
    delete globalThis.rowKeyboardFixtureFetch;
    delete globalThis.rowBeforeRefresh;
    document.getElementById('refresh-btn').click();
  });
  await page.waitForFunction(key => !document.querySelector(`#tasks-body [data-key="${key}"] .title`)?.textContent.endsWith('(refreshed)'), keys[2]);

  await page.keyboard.press('/');
  await expectFocus('task-search', '/ focuses the task search');
  if (await page.locator('#task-search').inputValue() !== '') throw new Error('/ must not be typed into the search it focuses');
  const typedInField = await page.evaluate(() => {
    const event = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true });
    document.getElementById('task-search').dispatchEvent(event);
    return !event.defaultPrevented;
  });
  if (!typedInField) throw new Error('/ inside a field must stay a character');
  await page.evaluate(() => document.activeElement.blur());

  // Runs and Knowledge friction rows follow the same pattern.
  await page.evaluate(async () => {
    const response = payload => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
    // Failed, so the Runs filter earlier scenarios leave on shows all three.
    const runs = [0, 1, 2].map(index => ({
      run_id: `jrun-keyboard-${index}`, job_id: `keyboard-job-${index}`, state: 'failed', created_at: new Date().toISOString(),
    }));
    const prior = globalThis.fetch;
    globalThis.rowKeyboardFixtureFetch = prior;
    globalThis.fetch = async (path, options) => new URL(path, window.location.href).pathname === '/api/job-runs'
      ? response({ items: runs, total: runs.length, limit: 50, truncated: false })
      : prior(path, options);
    (await import('/js/router.js')).setActiveTab('jobs/history');
    document.getElementById('refresh-btn').click();
  });
  const runButtons = page.locator('#runs-body .runs-row[data-key^="run-"] > .id');
  await page.waitForFunction(() => document.querySelectorAll('#runs-body .runs-row[data-key^="run-"]').length === 3);
  const nestedRuns = await nestedInteractive(page, '#runs-body');
  if (nestedRuns.length) throw new Error(`Run rows nest controls in a button: ${nestedRuns.join('\n')}`);
  if (await runButtons.first().evaluate(node => node.tagName) !== 'BUTTON') throw new Error('Run job cell must be the row button');
  await runButtons.first().focus();
  await page.keyboard.press('ArrowDown');
  if (!(await runButtons.nth(1).evaluate(node => node === document.activeElement))) throw new Error('ArrowDown must move between run rows');
  const runTabbable = await tabbableRows('#runs-body');
  if (!(runTabbable[1] > 0 && runTabbable[0] === 0 && runTabbable[2] === 0)) {
    throw new Error(`Only the current run row may be in the tab order: ${JSON.stringify(runTabbable)}`);
  }
  await page.evaluate(async () => {
    document.activeElement.blur();
    globalThis.fetch = globalThis.rowKeyboardFixtureFetch;
    delete globalThis.rowKeyboardFixtureFetch;
    (await import('/js/router.js')).setActiveTab('knowledge/frictions');
  });
  const frictionTitle = page.locator('#frictions-body .friction-row > .title').first();
  await frictionTitle.waitFor({ state: 'visible' });
  const nestedFrictions = await nestedInteractive(page, '#frictions-body');
  if (nestedFrictions.length) throw new Error(`Friction rows nest controls in a button: ${nestedFrictions.join('\n')}`);
  if (await frictionTitle.evaluate(node => node.tagName) !== 'BUTTON'
    || await frictionTitle.getAttribute('aria-controls') !== 'friction-detail'
    || await frictionTitle.getAttribute('aria-current') !== 'true') {
    throw new Error('The selected friction title must be a button naming the detail pane it fills');
  }
  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('tasks'));
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
        auth_exclusions: [{ provider: 'claude', host: 'fixture-mac', excluded_at: '2026-10-08T13:00:00Z',
          error_class: 'provider_unavailable/auth', relogin_hint: 'Run `claude auth login`.',
          credential_source: 'macOS keychain (no passed token)', next_probe_at: '2026-10-08T13:10:00Z' }],
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
    for (const expected of ['crews runnable: luna', 'excluded gemini-flash (provider unavailable): ORB-1 failed', 'excluded opus (preflight)', 'claude auth failed on fixture-mac', '2026-10-08T13:00:00Z', 'provider_unavailable/auth', 'claude auth login', 'macOS keychain', '2026-10-08T13:10:00Z']) {
      if (!text.includes(expected)) throw new Error(`Crew window missing "${expected}" at ${width}px: ${text}`);
    }
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error(`Crew window overflows the page at ${width}px`);
    await page.screenshot({ path: path.join(evidence, `run-crew-window-${width}.png`) });
  }
}

// A pull drain's run detail lists the backlog its owner kept off this host:
// each task with its reason and blockers, the age of the owner's answer, and
// the idle summary once several passes in a row claimed nothing.
async function assertStillWaiting(page) {
  await page.evaluate(async () => {
    const detail = await import('/js/run-detail.js');
    detail.setActiveRunDetail({
      run: {
        run_id: 'jrun-pull-waiting', job_id: 'workspace_pull_pipeline', state: 'running',
        drain_last_pass: {
          recorded_at: '2026-10-07T06:50:00Z', waiting_recorded_at: '2026-10-07T06:44:53Z',
          queued: 9, excluded_total: 2, consecutive_idle_passes: 4,
          deferred: [{ task_id: 'ORB-101', reason: 'context_lock_conflict', blocked_by: ['ORB-900'] }],
          excluded: [
            { task_id: 'ORB-103', reason: 'dependency_not_done', blocked_by: ['ORB-901'] },
            { task_id: 'ORB-104', reason: 'host_os_mismatch', detail: 'waits for a linux host (os:linux); the executor runs macos' },
          ],
          waiting_by_reason: { context_lock_conflict: 4, owner_hold: 11, dependency_not_done: 1, host_os_mismatch: 1 },
        },
      },
      steps: [],
    });
    for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'run-detail');
    detail.renderRunDetailMeta();
  });
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 900 });
    const panel = page.locator('#run-detail-meta .still-waiting');
    await panel.scrollIntoViewIfNeeded();
    if (!(await panel.isVisible())) throw new Error(`Still-waiting panel invisible at ${width}px`);
    const text = await panel.textContent();
    for (const expected of [
      '3 backlog tasks have recorded wait reasons (1 deferred, 2 excluded)',
      '9 additional admissible tasks were not started and are not listed below',
      // Reasons render as plain wording, not raw codes (f67c654d2): a lock
      // conflict reads "waits on lock", a host mismatch "needs <os> host", and
      // blockers follow a "blocked by" link instead of the old blocked-by= form.
      'Task ORB-101: waits on lock · blocked by ORB-900',
      'Task ORB-103: dependency not done · blocked by ORB-901',
      'Task ORB-104: needs linux host (waits for a linux host',
      'idle: 17 backlog task(s) kept off this host for 4 consecutive passes (11 held on the owner, 4 footprint holds',
    ]) {
      if (!text.includes(expected)) throw new Error(`Still-waiting panel missing "${expected}" at ${width}px: ${text}`);
    }
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error(`Still-waiting panel overflows the page at ${width}px`);
    await page.screenshot({ path: path.join(evidence, `run-still-waiting-${width}.png`) });
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

// Analytics › Cost keeps the managed-execution panel and its window selector,
// and #analytics/cost restores its window. The retired Health routes
// (#diagnostics/cost, #diagnostics/scoreboard) no longer open it. The family
// leaderboard, narrative, agent cards and insights are gone.
async function assertCostPage(page) {
  const orchestration = {
    since: '2026-10-12T00:00:00Z',
    until: '2026-10-13T00:00:00Z',
    as_of: '2026-10-13T00:00:00Z',
    normalized_tokens: {
      normalized_token_total: 1500, invocation_count: 2, linked_task_count: 1,
      uncached_input_tokens: 100, cache_read_tokens: 200, cache_create_tokens: 0,
      cache_create_1h_tokens: 0, output_tokens: 50, covered_invocation_count: 2,
      unknown_input_basis_or_model_count: 0,
    },
    buckets: [{
      kind: 'orchestrator', orchestrator: 'claude', invocation_count: 2, linked_task_count: 1,
      provider_cost_usd: 1.25, provider_cost_count: 2, missing_provider_count: 0,
      derived_cost_usd: 1.5, derived_cost_count: 2, unpriced_derived_count: 0,
      comparable_cost_count: 2, comparable_provider_cost_usd: 1.25,
      comparable_derived_cost_usd: 1.5, comparable_cost_delta_usd: 0.25,
      models: [{ model: 'claude-sonnet', tokens: { normalized_token_total: 1500 } }],
    }, {
      kind: 'unattributed', invocation_count: 1, linked_task_count: 0,
      provider_cost_count: 0, missing_provider_count: 1,
      derived_cost_count: 0, unpriced_derived_count: 1, comparable_cost_count: 0,
      models: [{ model: 'provider-a', tokens: { normalized_token_total: 80 } }],
    }, {
      kind: 'missing', invocation_count: 1, linked_task_count: 0,
      provider_cost_count: 0, missing_provider_count: 1,
      derived_cost_count: 0, unpriced_derived_count: 1, comparable_cost_count: 0,
      models: [{ model: 'provider-b', tokens: { normalized_token_total: 40 } }],
    }],
  };
  const painted = await page.evaluate(async (orchestration) => {
    const { getWindow } = await import('/js/common.js');
    const { renderScoreboard } = await import('/js/scoreboard.js');
    const { setActiveTab } = await import('/js/router.js');
    setActiveTab('analytics/cost', { refresh: false, updateHash: false });
    const selected = getWindow();
    renderScoreboard({ window: selected, agents: {}, orchestration });
    const host = document.getElementById('scoreboard-orchestration');
    const subtab = document.querySelector('#analytics-subtabs [data-subtab="cost"]');
    const windows = [...document.querySelectorAll('#scoreboard-window-selector .scoreboard-window-seg')].map(node => node.dataset.window);
    const removed = ['scoreboard-narrative', 'scoreboard-agent-strip', 'scoreboard-body', 'scoreboard-insights', 'scoreboard-insights-panel']
      .filter(id => document.getElementById(id));
    return {
      text: host.textContent,
      count: document.getElementById('scoreboard-orchestration-count').textContent,
      subtab: subtab ? subtab.textContent.replace(/\s+/g, ' ').trim() : '',
      activeSubtab: subtab?.classList.contains('active') || false,
      costVisible: document.getElementById('analytics-cost-main').checkVisibility(),
      windows,
      removed,
    };
  }, orchestration);
  if (!painted.subtab.startsWith('Cost') || !painted.activeSubtab || !painted.costVisible) {
    throw new Error(`Analytics › Cost must be the visible route: ${JSON.stringify(painted)}`);
  }
  if (painted.windows.join() !== '1h,24h,7d,30d,all') throw new Error(`Cost window selector missing windows: ${painted.windows}`);
  if (painted.removed.length) throw new Error(`Retired scoreboard nodes still in the page: ${painted.removed}`);
  if (!painted.text.includes('$1.25') || !painted.text.includes('$1.50') || !painted.text.includes('1.5K')) {
    throw new Error(`Cost numbers changed for the fixed payload: ${painted.text}`);
  }
  if (!painted.text.includes('Orchestrator: claude') || !painted.text.includes('claude-sonnet 1.5K')) {
    throw new Error(`Cost attribution missing: ${painted.text}`);
  }
  if (painted.count !== '3 buckets') throw new Error(`Cost bucket count changed: ${painted.count}`);
  const contrastEvidence = [];
  for (const width of [1280, 1440, 375]) {
    await page.setViewportSize({ width, height: 900 });
    const layout = await page.evaluate(() => {
      const main = document.getElementById('analytics-cost-main');
      const host = document.getElementById('scoreboard-orchestration');
      const selector = document.getElementById('scoreboard-window-selector');
      const box = host.getBoundingClientRect();
      return {
        shown: getComputedStyle(main).display !== 'none' && box.width > 0 && box.height > 0,
        selector: selector.getBoundingClientRect().width > 0,
        pageOverflow: document.documentElement.scrollWidth > window.innerWidth + 1,
        text: host.textContent,
      };
    });
    if (!layout.shown || !layout.selector) throw new Error(`Cost panel or window selector hidden at ${width}px: ${JSON.stringify(layout)}`);
    if (layout.pageOverflow) throw new Error(`Cost page widens the viewport at ${width}px`);
    if (!layout.text.includes('$1.25')) throw new Error(`Cost dollars missing at ${width}px`);
    const contrast = await page.evaluate(() => {
      const luminance = (color) => {
        const [red, green, blue] = color.match(/[\d.]+/g).slice(0, 3).map(Number).map((channel) => {
          const normalized = channel / 255;
          return normalized <= 0.04045 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4;
        });
        return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
      };
      const ratio = (foreground, background) => {
        const [lighter, darker] = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
        return (lighter + 0.05) / (darker + 0.05);
      };
      const probe = document.createElement('span');
      probe.style.color = 'var(--fg-disabled)';
      document.body.append(probe);
      const disabledColor = getComputedStyle(probe).color;
      probe.remove();
      const labels = [...document.querySelectorAll('#scoreboard-orchestration .scoreboard-orchestration-bucket .token-models-label')]
        .map((label) => {
          let background = 'rgba(0, 0, 0, 0)';
          for (let ancestor = label; ancestor; ancestor = ancestor.parentElement) {
            const color = getComputedStyle(ancestor).backgroundColor;
            if (color !== 'rgba(0, 0, 0, 0)' && color !== 'transparent') {
              background = color;
              break;
            }
          }
          const foreground = getComputedStyle(label).color;
          return { foreground, background, visible: label.checkVisibility(), ratio: ratio(foreground, background) };
        });
      const disabledText = [...document.querySelectorAll('#analytics-cost-main *')]
        .filter((node) => node.children.length === 0 && node.textContent.trim() && node.checkVisibility())
        .filter((node) => getComputedStyle(node).color === disabledColor
          && !node.closest(':disabled, [aria-disabled="true"]'))
        .map((node) => ({ tag: node.tagName, className: node.className, text: node.textContent.trim() }));
      return { disabledColor, bucketCount: document.querySelectorAll('#scoreboard-orchestration .scoreboard-orchestration-bucket').length, labels, disabledText };
    });
    if (contrast.labels.length !== contrast.bucketCount || contrast.bucketCount !== 3) {
      throw new Error(`Every Cost bucket must have a measured model-token caption: ${JSON.stringify(contrast)}`);
    }
    for (const label of contrast.labels) {
      if (!label.visible || !Number.isFinite(label.ratio) || label.ratio < 4.5 || label.foreground === contrast.disabledColor) {
        throw new Error(`Model-token caption must reach 4.5:1 without the disabled token at ${width}px: ${JSON.stringify(label)}`);
      }
    }
    if (contrast.disabledText.length) {
      throw new Error(`Informational Cost text uses the disabled color: ${JSON.stringify(contrast.disabledText)}`);
    }
    contrastEvidence.push({ width, ...contrast });
    if (width === 375) {
      const label = page.locator('#scoreboard-orchestration .token-models-label').first();
      await label.scrollIntoViewIfNeeded();
      const bounds = await label.boundingBox();
      if (!bounds || bounds.x < 0 || bounds.x + bounds.width > width || bounds.y < 0 || bounds.y + bounds.height > 900) {
        throw new Error(`The model-token caption must fit in the 375px Cost viewport: ${JSON.stringify(bounds)}`);
      }
    }
    await page.screenshot({ path: path.join(evidence, `cost-${width}.png`), fullPage: true });
  }
  fs.writeFileSync(path.join(evidence, 'cost-contrast-result.json'), JSON.stringify({ passed: true, viewports: contrastEvidence }, null, 2));
  const routed = await page.evaluate(async () => {
    const { setActiveTab } = await import('/js/router.js');
    // Refresh and the hash write stay off: this fixture has already replaced
    // fetch with a hang, and the assertions have to read the painted dollars
    // before a hashchange refresh can clear them.
    const opts = { refresh: false, updateHash: false };
    const costShown = () => document.getElementById('analytics-cost-main').checkVisibility();
    const retired = {};
    for (const route of ['diagnostics/scoreboard?window=7d', 'diagnostics/cost?window=7d']) {
      setActiveTab(route, opts);
      retired[route] = { pane: document.querySelector('.tab-pane.active')?.dataset.tab, cost: costShown() };
    }
    setActiveTab('analytics/cost?window=7d', opts);
    const subtab = document.querySelector('#analytics-subtabs [data-subtab="cost"]');
    const selected = document.querySelector('#scoreboard-window-selector [aria-selected="true"]');
    const removed = ['scoreboard-narrative', 'scoreboard-agent-strip', 'scoreboard-body', 'scoreboard-insights', 'scoreboard-insights-panel']
      .filter(id => document.getElementById(id));
    return {
      retired,
      pane: document.querySelector('.tab-pane.active')?.dataset.tab,
      active: subtab.classList.contains('active'),
      label: subtab.textContent.replace(/\s+/g, ' ').trim(),
      window: selected && selected.dataset.window,
      shown: costShown(),
      text: document.getElementById('scoreboard-orchestration').textContent,
      removed,
      healthCost: Boolean(document.querySelector('#diag-subtabs [data-subtab="cost"], #diag-subtabs [data-subtab="scoreboard"]')),
    };
  });
  for (const [route, state] of Object.entries(routed.retired)) {
    if (state.pane !== 'diagnostics' || state.cost) throw new Error(`retired #${route} must not open Cost: ${JSON.stringify(state)}`);
  }
  if (routed.pane !== 'analytics' || !routed.active || !routed.label.startsWith('Cost') || routed.healthCost) {
    throw new Error(`#analytics/cost did not select Analytics › Cost: ${JSON.stringify(routed)}`);
  }
  if (routed.window !== '7d' || !routed.shown) throw new Error(`Cost window was not restored: ${JSON.stringify(routed)}`);
  if (!routed.text.includes('$1.25') || routed.removed.length) {
    throw new Error(`Routing disturbed the painted cost panel: ${JSON.stringify({ removed: routed.removed, text: routed.text.slice(0, 240) })}`);
  }
  await assertCostExplorationStatus(page, orchestration);
}

// Cost exploration is a separate tab panel: in aggregate view opened from
// another Analytics subtab both bodies placeholder, and while exploration
// is visible it shows its own loading, cold-error, and stale-refresh notes
// from the single /api/cost GET that also fills Spend.
async function assertCostExplorationStatus(page, orchestration) {
  const aggregate = await page.evaluate(async () => {
    const common = await import('/js/common.js');
    common.setMultiWorkspace(true);
    common.setWorkspace(null);
    const { setActiveTab } = await import('/js/router.js');
    const opts = { refresh: false, updateHash: false };
    setActiveTab('analytics/metrics', opts);
    setActiveTab('analytics/cost?tab=exploration', opts);
    return {
      spend: document.getElementById('scoreboard-orchestration').textContent,
      explore: document.getElementById('scoreboard-exploration').textContent,
      exploreVisible: document.getElementById('scoreboard-exploration').checkVisibility(),
      spendPanelHidden: document.getElementById('scoreboard-panel-spend').hidden,
      explorePanelHidden: document.getElementById('scoreboard-panel-exploration').hidden,
    };
  });
  if (!aggregate.spend.includes('Select a workspace to view this panel')
    || !aggregate.explore.includes('Select a workspace to view this panel')) {
    throw new Error(`Aggregate Cost opened from another subtab must placeholder both bodies: ${JSON.stringify(aggregate)}`);
  }
  if (!aggregate.exploreVisible || aggregate.explorePanelHidden || !aggregate.spendPanelHidden) {
    throw new Error(`Exploration must be the visible Cost panel in the aggregate check: ${JSON.stringify(aggregate)}`);
  }
  await page.evaluate(async (orchestration) => {
    const common = await import('/js/common.js');
    common.setWorkspace('one');
    const cohort = { runs: 1, mean_duration_ms: 60000, mean_cost_usd: 1, priced_runs: 1, footprint_known_runs: 1, footprint_miss_runs: 0, footprint_miss_rate: 0, explore_calls: 1, explore_cost_usd: 0.5 };
    globalThis.__costPayload = {
      window: common.getWindow(), orchestration,
      explore_comparison: { crews: [{ crew: 'browser-crew', with_explore: cohort, without_explore: { ...cohort, explore_calls: 0 } }] },
    };
    globalThis.__costCalls = 0;
    globalThis.fetch = (path) => {
      const url = new URL(path, window.location.href);
      if (url.pathname !== '/api/cost') return new Promise(() => {});
      globalThis.__costCalls += 1;
      return new Promise(resolve => { globalThis.__resolveCost = resolve; });
    };
    const { fetchAndRenderScoreboard } = await import('/js/scoreboard.js');
    globalThis.__costDone = fetchAndRenderScoreboard()
      .then(() => { globalThis.__costOutcome = 'ok'; }, error => { globalThis.__costOutcome = error.message; });
  }, orchestration);
  const loading = await page.evaluate(() => ({
    calls: globalThis.__costCalls,
    explore: document.getElementById('scoreboard-exploration').textContent,
    visible: document.getElementById('scoreboard-exploration').checkVisibility(),
  }));
  if (loading.calls !== 1 || !loading.visible || !loading.explore.includes('Loading…')) {
    throw new Error(`Exploration must show Loading from one in-flight GET: ${JSON.stringify(loading)}`);
  }
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.screenshot({ path: path.join(evidence, 'cost-exploration-loading-1280.png') });
  await page.evaluate(() => {
    globalThis.__resolveCost(new Response(JSON.stringify({ error: 'cost unavailable' }), { status: 500 }));
  });
  await page.waitForFunction(() => globalThis.__costOutcome === 'cost unavailable');
  const coldError = await page.evaluate(() => document.getElementById('scoreboard-exploration').textContent);
  if (!coldError.includes('Unable to load') || coldError.includes('No implement runs in this window.')) {
    throw new Error(`Exploration must show Unable to load after a failed cold load: ${coldError}`);
  }
  await page.evaluate(async () => {
    globalThis.fetch = (path) => {
      const url = new URL(path, window.location.href);
      if (url.pathname !== '/api/cost') return new Promise(() => {});
      globalThis.__costCalls += 1;
      return Promise.resolve(new Response(JSON.stringify(globalThis.__costPayload), { status: 200 }));
    };
    const { fetchAndRenderScoreboard } = await import('/js/scoreboard.js');
    await fetchAndRenderScoreboard();
  });
  const filled = await page.evaluate(() => ({
    calls: globalThis.__costCalls,
    explore: document.getElementById('scoreboard-exploration').textContent,
    spend: document.getElementById('scoreboard-orchestration').textContent,
  }));
  if (filled.calls !== 2 || !filled.explore.includes('browser-crew') || !filled.spend.includes('$1.25')) {
    throw new Error(`One successful GET must fill both Cost bodies: ${JSON.stringify(filled)}`);
  }
  await page.evaluate(async () => {
    globalThis.fetch = (path) => {
      const url = new URL(path, window.location.href);
      if (url.pathname !== '/api/cost') return new Promise(() => {});
      return Promise.resolve(new Response(JSON.stringify({ error: 'cost refresh unavailable' }), { status: 500 }));
    };
    const { fetchAndRenderScoreboard } = await import('/js/scoreboard.js');
    globalThis.__costDone = fetchAndRenderScoreboard()
      .then(() => { globalThis.__costOutcome = 'ok'; }, error => { globalThis.__costOutcome = error.message; });
  });
  await page.waitForFunction(() => globalThis.__costOutcome === 'cost refresh unavailable');
  const stale = await page.evaluate(() => document.getElementById('scoreboard-exploration').textContent);
  if (!stale.includes('Refresh failed; showing stale data') || !stale.includes('browser-crew')) {
    throw new Error(`Exploration must keep crew rows with the stale-data note after a failed refresh: ${stale}`);
  }
  for (const width of [1280, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await page.screenshot({ path: path.join(evidence, `cost-exploration-stale-${width}.png`) });
  }
}

// ORB-14481: from 1024px up the top bar is one fixed-height row on every
// route. Host readings live in the rail; a critical fixture shows one warning
// chip, and a throttle flip neither re-wraps the bar nor resizes that chip,
// the rail rows or Refresh. The fixture is the worst case the bar holds: the
// widest destination, the warning chip, provider usage and the aggregate drain label.
async function assertTopbarSingleRow(page) {
  const routes = ['tasks', 'operations/routines', 'config/effective', 'knowledge/frictions', 'plugins'];
  const snapshot = () => page.evaluate(() => {
    const rect = id => document.getElementById(id).getBoundingClientRect();
    const bar = document.querySelector('.topbar').getBoundingClientRect();
    const children = [...document.querySelectorAll('.topbar .crumb, .topbar .host-warn, .topbar .host-resource, .topbar #refresh-btn, .topbar #global-drain-state')]
      .filter(node => node.getClientRects().length);
    // Items are centred in the row, so one row means one shared vertical centre.
    const centres = children.map(node => { const r = node.getBoundingClientRect(); return r.top + r.height / 2; });
    const rows = Math.max(...centres) - Math.min(...centres) <= 2 ? 1 : 2;
    const refresh = rect('refresh-btn');
    const warn = document.getElementById('host-warn');
    const warnBox = warn.getBoundingClientRect();
    const block = document.getElementById('host-block');
    const chips = [...block.querySelectorAll(':scope > .host-resource')];
    // Provider usage shows its chips on a wide bar and one summary chip on a narrow one.
    const usageStrip = document.getElementById('provider-usage-chips');
    const usage = [...usageStrip.querySelectorAll('.provider-usage')].filter(chip => chip.getClientRects().length);
    const lastChip = usage.at(-1)?.getBoundingClientRect();
    const drain = document.getElementById('global-drain-state');
    return {
      height: bar.height, refreshLeft: refresh.left, rows, drainShown: !drain.hidden,
      topbarHostChips: document.querySelectorAll('.topbar .host-resource[data-resource]').length,
      warn: {
        shown: !warn.hidden && warn.getClientRects().length > 0,
        left: warnBox.left, width: warnBox.width,
        clipped: warn.scrollWidth > warn.clientWidth + 1,
        inside: warnBox.left >= bar.left - 0.5 && warnBox.right <= bar.right + 0.5,
      },
      usage: usage.map(chip => ({
        className: chip.className, width: chip.getBoundingClientRect().width, left: chip.getBoundingClientRect().left,
        clipped: chip.scrollWidth > chip.clientWidth,
      })),
      usageOverflows: usageStrip.scrollWidth > usageStrip.clientWidth,
      chips: chips.map(chip => ({
        resource: chip.dataset.resource, className: chip.className, width: chip.getBoundingClientRect().width,
        left: chip.getBoundingClientRect().left, clipped: chip.scrollWidth > chip.clientWidth,
      })),
      refreshRight: refresh.right, barRight: bar.right,
      overlaps: !!lastChip && (lastChip.right > refresh.left || (!drain.hidden && lastChip.right > rect('global-drain-state').left)),
    };
  });
  await page.evaluate(async () => {
    const { renderHostResources, renderProviderUsage } = await import('/js/host-resources.js');
    const drain = document.getElementById('global-drain-state');
    drain.hidden = false;
    drain.dataset.drainState = 'per-workspace';
    drain.querySelector('.global-drain-label').textContent = 'Per-workspace drain status';
    globalThis.topbarUsage = many => {
      const reading = extra => ({
        provider: 'claude', scope: null, window: 'five_hour', used_percent: 5, exhausted: false, threshold: 90, gating: true,
        gated: false, partial: false, stale: false, age_seconds: 30, observed_at: new Date().toISOString(),
        until: '2026-10-18T03:00:00Z', crews: ['opus'], ...extra,
      });
      renderProviderUsage({ provider_limits: many ? [
        reading({ window: 'seven_day', used_percent: 100, exhausted: true, gated: true }),
        reading({ provider: 'codex', window: 'primary', used_percent: 100, threshold: 'off', stale: true, age_seconds: 3 * 86400 }),
        reading({ scope: 'sonnet', window: 'seven_day', used_percent: 87 }),
        reading({ used_percent: null }),
        reading({ window: 'weekly_extra', used_percent: 12 }),
      ] : [reading({})] });
    };
    globalThis.topbarHost = throttle => renderHostResources({
      cpu: { percent: 1234, severity: 'critical' }, memory: { percent: 100, severity: 'critical' },
      disk: { path: '/workspace', percent: 100, severity: 'critical' }, sample_age_seconds: 1, max_age_seconds: 15,
      stale: false, throttle, pressures: throttle ? [{ resource: 'cpu' }, { resource: 'disk /workspace' }] : [],
      reason: 'cpu load and disk high', thresholds: { enabled: true },
    });
  });
  try {
    for (const width of [1024, 1280, 1440, 1600, 1920]) {
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
          if (!state.drainShown) throw new Error(`The worst-case fixture lost its drain pill on #${route} at ${width}px`);
          if (state.height > 64 || state.rows !== 1) throw new Error(`Top bar wraps on #${route} at ${width}px: ${JSON.stringify(state)}`);
          if (state.usageOverflows || state.overlaps || state.refreshRight > state.barRight + 0.5) throw new Error(`Top bar content overflows on #${route} at ${width}px: ${JSON.stringify(state)}`);
          if (state.topbarHostChips !== 0) throw new Error(`The top bar still shows host readings: ${JSON.stringify(state)}`);
          if (!state.warn.shown || state.warn.clipped || !state.warn.inside) throw new Error(`The warning chip is missing or clipped on #${route} at ${width}px: ${JSON.stringify(state.warn)}`);
          if (state.chips.map(chip => chip.resource).join() !== 'cpu,memory,disk') throw new Error(`The rail block must show load, mem and disk: ${JSON.stringify(state.chips)}`);
          if (state.chips.some(chip => chip.clipped)) throw new Error(`A host reading is cut off on #${route} at ${width}px: ${JSON.stringify(state.chips)}`);
        }
        const [open, held] = flips;
        // The provider readings changing (one chip to the worst case, with a
        // stale, a gating-off, a gated, a missing and a folded one) must not
        // move the host chips, Refresh or the bar's height, nor clip or wrap.
        const usageStates = [];
        for (const many of [false, true, false]) {
          await page.evaluate(many => globalThis.topbarUsage(many), many);
          const state = await snapshot();
          if (state.height > 64 || state.rows !== 1 || state.usageOverflows || state.overlaps || state.refreshRight > state.barRight + 0.5) {
            throw new Error(`Provider usage overflows the top bar on #${route} at ${width}px: ${JSON.stringify(state)}`);
          }
          if (!state.usage.length || state.usage.some(chip => chip.clipped)) throw new Error(`A provider usage reading is cut off on #${route} at ${width}px: ${JSON.stringify(state.usage)}`);
          usageStates.push(state);
        }
        const hostGeometry = state => JSON.stringify([state.height, state.refreshLeft, state.warn.left, state.warn.width, state.chips.map(chip => [chip.left, chip.width])]);
        if (new Set(usageStates.map(hostGeometry)).size !== 1 || hostGeometry(usageStates[0]) !== hostGeometry(open)) {
          throw new Error(`Provider usage readings moved the top bar on #${route} at ${width}px: ${JSON.stringify(usageStates)}`);
        }
        if (held.chips.map(chip => chip.className.includes('throttled')).join() !== 'true,false,true') {
          throw new Error(`Only the held resources must read as throttled: ${JSON.stringify(held.chips)}`);
        }
        const geometry = state => JSON.stringify([state.height, state.refreshLeft, state.warn.left, state.warn.width, state.chips.map(chip => [chip.left, chip.width])]);
        if (geometry(open) !== geometry(held)) {
          throw new Error(`Throttled verdict moved or resized the top bar on #${route} at ${width}px: ${JSON.stringify(flips)}`);
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
// row's state, name and time without any sideways scrolling. Runs keeps its
// first column pinned and shows a scroll edge. Fixtures stay in this function
// so it owns its fetch.
async function assertNarrowTableLayouts(page) {
  const runsLayoutMeasurements = [];
  await page.evaluate(async () => {
    const now = Date.now();
    const iso = (minutes) => new Date(now - minutes * 60000).toISOString();
    const response = (payload, status = 200, headers = {}) => new Response(JSON.stringify(payload), { status, headers });
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
    const olderAuditEvents = [{ ...events[0], id: 8998, timestamp: iso(3), target_id: 'TASK-OLDER' }];
    const auditPageRequests = [];
    const fixtureFetch = globalThis.fetch;
    globalThis.narrowTableFixtureFetch = fixtureFetch;
    globalThis.narrowAuditPageRequests = auditPageRequests;
    globalThis.fetch = async (path, options) => {
      const url = new URL(path, window.location.href);
      if (url.pathname === '/api/job-runs') return response({ items: runs, total: runs.length, limit: 50, truncated: false });
      if (url.pathname === '/api/audit') {
        const cursor = url.searchParams.get('before');
        auditPageRequests.push(cursor);
        return cursor
          ? response(olderAuditEvents)
          : response(events, 200, { 'x-audit-next-before': 'older-audit-page' });
      }
      if (url.pathname === '/api/diagnostics/errors') return response({ items: errors, since: iso(24 * 60), coverage_since: iso(24 * 60) });
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

  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('jobs/history'));
  await refresh();
  await page.locator('#runs-body .runs-row[data-key^="run-"]').first().waitFor({ state: 'visible', timeout: 5000 });
  // Earlier keyboard coverage intentionally leaves the Failed filter active;
  // restore All so this fixture's live run and its Cancel control are present.
  await page.locator('#runs-body .runs-filter-button').first().click();
  await page.waitForFunction(() => document.querySelectorAll('#runs-body .runs-row[data-key^="run-"]').length === 3);
  if (await scrolls('runs-body') || await pageOverflow()) throw new Error('Runs must not scroll sideways at 375px');
  const phoneToolbar = await page.evaluate(() => {
    const wrap = document.getElementById('runs-body');
    const toolbar = wrap.querySelector('.runs-toolbar');
    const rect = toolbar.getBoundingClientRect();
    const style = getComputedStyle(toolbar);
    const wrapRect = wrap.getBoundingClientRect();
    return {
      viewportWidth: window.innerWidth,
      bodyClientWidth: wrap.clientWidth,
      bodyScrollWidth: wrap.scrollWidth,
      pageClientWidth: document.documentElement.clientWidth,
      pageScrollWidth: document.documentElement.scrollWidth,
      toolbarWidth: rect.width,
      marginLeft: parseFloat(style.marginLeft),
      marginRight: parseFloat(style.marginRight),
      toolbarLeft: rect.left,
      toolbarRight: rect.right,
      bodyLeft: wrapRect.left + wrap.clientLeft,
      bodyRight: wrapRect.left + wrap.clientLeft + wrap.clientWidth,
      toolbarScrollWidth: toolbar.scrollWidth,
      toolbarClientWidth: toolbar.clientWidth,
    };
  });
  if (Math.abs(phoneToolbar.toolbarWidth + phoneToolbar.marginLeft + phoneToolbar.marginRight - phoneToolbar.bodyClientWidth) > 1) {
    throw new Error(`Runs toolbar width and margins exceed the 375px body: ${JSON.stringify(phoneToolbar)}`);
  }
  if (phoneToolbar.toolbarLeft < phoneToolbar.bodyLeft - 1 || phoneToolbar.toolbarRight > phoneToolbar.bodyRight + 1) {
    throw new Error(`Runs toolbar falls outside the 375px body: ${JSON.stringify(phoneToolbar)}`);
  }
  runsLayoutMeasurements.push(phoneToolbar);
  await assertVisible(['.runs-scope-note', '.runs-filter', '.runs-filter-button.active', '.runs-query', '.runs-window', '.runs-row[data-key^="run-"] .state', '.runs-row[data-key^="run-"] .id', '.runs-row[data-key^="run-"] .when'], '#runs-body', 'Runs', 375);
  await page.screenshot({ path: path.join(evidence, 'runs-375.png'), fullPage: true });

  for (const width of [601, 700, 768]) {
    await page.setViewportSize({ width, height: 900 });
    const pinned = await page.evaluate(() => {
      const wrap = document.getElementById('runs-body');
      const row = wrap.querySelector('.runs-row[data-key^="run-"]');
      const cell = row.querySelector('.state');
      wrap.scrollLeft = wrap.scrollWidth;
      const wrapRect = wrap.getBoundingClientRect();
      const rect = cell.getBoundingClientRect();
      const toolbar = wrap.querySelector('.runs-toolbar');
      const toolbarRect = toolbar.getBoundingClientRect();
      const toolbarStyle = getComputedStyle(toolbar);
      const topmost = document.elementFromPoint(rect.left + 4, (rect.top + rect.bottom) / 2);
      return {
        viewportWidth: window.innerWidth,
        scrolls: wrap.scrollWidth > wrap.clientWidth + 1,
        scrolled: wrap.scrollLeft > 0,
        stays: Math.abs(rect.left - wrapRect.left) < 1.5,
        painted: cell.contains(topmost),
        edge: getComputedStyle(wrap).backgroundImage.includes('gradient'),
        bodyClientWidth: wrap.clientWidth,
        bodyScrollWidth: wrap.scrollWidth,
        pageClientWidth: document.documentElement.clientWidth,
        pageScrollWidth: document.documentElement.scrollWidth,
        toolbarWidth: toolbarRect.width,
        marginLeft: parseFloat(toolbarStyle.marginLeft),
        marginRight: parseFloat(toolbarStyle.marginRight),
        toolbarLeft: toolbarRect.left,
        toolbarRight: toolbarRect.right,
        bodyLeft: wrapRect.left + wrap.clientLeft,
        bodyRight: wrapRect.left + wrap.clientLeft + wrap.clientWidth,
        toolbarScrollWidth: toolbar.scrollWidth,
        toolbarClientWidth: toolbar.clientWidth,
      };
    });
    if (!pinned.scrolls) throw new Error(`Runs must still scroll sideways at ${width}px for this check to mean anything`);
    if (!(pinned.scrolled && pinned.stays && pinned.painted && pinned.edge)) {
      throw new Error(`Runs first column or scroll edge missing at ${width}px: ${JSON.stringify(pinned)}`);
    }
    if (Math.abs(pinned.toolbarLeft - pinned.bodyLeft) > 1.5 || pinned.toolbarRight > pinned.bodyRight + 1.5 || pinned.toolbarScrollWidth > pinned.toolbarClientWidth + 1) {
      throw new Error(`Runs toolbar is not pinned within its body at ${width}px: ${JSON.stringify(pinned)}`);
    }
    await assertVisible(['.runs-filter', '.runs-filter-button.active', '.runs-query', '.runs-window'], '#runs-body', 'Runs toolbar', width);
    runsLayoutMeasurements.push(pinned);
    await page.screenshot({ path: path.join(evidence, `runs-${width}.png`), fullPage: true });
  }
  fs.writeFileSync(path.join(evidence, 'runs-layout-measurements.json'), `${JSON.stringify({ viewports: runsLayoutMeasurements }, null, 2)}\n`);
  for (const width of [769, 900, 1024, 1100]) {
    await page.setViewportSize({ width, height: 900 });
    const reachable = await page.evaluate(() => {
      const wrap = document.getElementById('runs-body');
      const rows = [...wrap.querySelectorAll('.runs-row[data-key^="run-"]')];
      const cancel = wrap.querySelector('.run-cancel');
      const row = cancel?.closest('.runs-row') ?? rows[0];
      const cell = row.querySelector('.state');
      const overflows = wrap.scrollWidth > wrap.clientWidth + 1;
      const initialPosition = getComputedStyle(cell).position;
      const edge = getComputedStyle(wrap).backgroundImage.includes('gradient');
      wrap.scrollLeft = wrap.scrollWidth;
      if (cancel) cancel.focus();
      const wrapRect = wrap.getBoundingClientRect();
      const cancelRect = cancel?.getBoundingClientRect();
      const stateRect = cell.getBoundingClientRect();
      const topmost = document.elementFromPoint(stateRect.left + 4, (stateRect.top + stateRect.bottom) / 2);
      return {
        overflows,
        initialPosition,
        edge,
        cancelExists: Boolean(cancel),
        cancelEnabled: cancel && !cancel.disabled,
        cancelFocused: cancel && document.activeElement === cancel,
        cancelVisible: cancelRect && cancelRect.width > 0
          && cancelRect.left >= wrapRect.left - 0.5 && cancelRect.right <= wrapRect.right + 0.5,
        statePinned: Math.abs(stateRect.left - wrapRect.left) < 1.5,
        statePainted: cell.contains(topmost),
        runStates: rows.map(item => item.querySelector('.state')?.textContent.trim()),
        cancelCount: wrap.querySelectorAll('.run-cancel').length,
      };
    });
    if (!reachable.overflows) throw new Error(`Runs must still overflow at ${width}px for the pinned-column check to mean anything`);
    if (reachable.initialPosition !== 'sticky' || !reachable.edge || !reachable.statePinned || !reachable.statePainted) {
      throw new Error(`Runs State column or scroll edge missing at ${width}px: ${JSON.stringify(reachable)}`);
    }
    if (!(reachable.cancelExists && reachable.cancelEnabled && reachable.cancelFocused && reachable.cancelVisible)) {
      throw new Error(`Live-run Cancel control is not reachable through the marked scroller at ${width}px: ${JSON.stringify(reachable)}`);
    }
    await page.screenshot({ path: path.join(evidence, `runs-${width}.png`), fullPage: true });
  }
  await page.setViewportSize({ width: 375, height: 900 });

  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/incidents/events'));
  await refresh();
  await page.locator('#audit-body tr.audit-row').first().waitFor({ state: 'visible', timeout: 5000 });
  if (await scrolls('audit-body') || await pageOverflow()) throw new Error('Audit events must not scroll sideways at 375px');
  await assertVisible(['tr.audit-row .c-status', 'tr.audit-row .c-command', 'tr.audit-row .c-time'], '#audit-body', 'Audit events', 375);
  await page.screenshot({ path: path.join(evidence, 'audit-375.png'), fullPage: true });
  await page.locator('#audit-body .audit-more-btn').click();
  await page.locator('#audit-body .audit-more-end').waitFor({ state: 'visible', timeout: 5000 });
  const auditPaging = await page.evaluate(() => ({
    requestCursors: globalThis.narrowAuditPageRequests,
    rows: document.querySelectorAll('#audit-body tr.audit-row').length,
  }));
  const requestedOlderCursor = auditPaging.requestCursors.at(-1) === 'older-audit-page';
  const requestedHeadPage = auditPaging.requestCursors.some(cursor => cursor === null);
  if (!requestedHeadPage || !requestedOlderCursor || auditPaging.rows !== 3) {
    throw new Error(`Audit paging must request the cursor and render its terminal page: ${JSON.stringify(auditPaging)}`);
  }
  if (await scrolls('audit-body') || await pageOverflow()) throw new Error('Paged audit events must not scroll sideways at 375px');
  await assertVisible(['tr.audit-row .c-status', 'tr.audit-row .c-command', 'tr.audit-row .c-time'], '#audit-body', 'Paged audit events', 375);

  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/incidents/errors'));
  await refresh();
  await page.locator('#diag-body tbody tr').first().waitFor({ state: 'visible', timeout: 5000 });
  if (await scrolls('diag-body') || await pageOverflow()) throw new Error('Errors must not scroll sideways at 375px');
  await assertVisible(['tbody tr .c-source', 'tbody tr .c-job_run', 'tbody tr .c-ts', 'tbody tr .c-message'], '#diag-body', 'Errors', 375);
  await page.screenshot({ path: path.join(evidence, 'errors-375.png'), fullPage: true });

  // The runs grid overflows a 1280px card. While the card is narrower than the
  // grid, State stays pinned, the scroll edge shows, and Duration and Actions
  // can be scrolled into view. Once the card can hold the tracks the table is
  // plain. All-workspaces mode adds a column, so that threshold is wider.
  await page.evaluate(async () => {
    document.getElementById('jobs-history-main').style.display = '';
    (await import('/js/router.js')).setActiveTab('jobs/history');
  });
  const renderRunsFixture = async (attributed) => {
    await page.evaluate(async (attributed) => {
      const { renderRuns } = await import('/js/runs.js');
      const now = Date.now();
      const iso = (minutes) => new Date(now - minutes * 60000).toISOString();
      renderRuns(['failed', 'success', 'running'].map((state, index) => ({
        run_id: `jrun-20261007-0717-c${index}-with-a-long-identifier`,
        job_id: `a-job-with-a-long-name-${index}`,
        state,
        created_at: iso(index + 1),
        duration_ms: 90000,
        ...(attributed ? { workspace_id: `ws-${index}`, workspace_name: `workspace-name-${index}` } : {}),
      })));
    }, attributed);
    await page.locator('#runs-body .runs-row[data-key^="run-"]').first().waitFor({ state: 'visible', timeout: 5000 });
  };
  const readRunsLayout = () => page.evaluate(() => {
    const wrap = document.getElementById('runs-body');
    const cancel = wrap.querySelector('.run-cancel');
    const row = cancel?.closest('.runs-row') ?? wrap.querySelector('.runs-row[data-key^="run-"]');
    const state = row.querySelector('.state');
    const duration = row.querySelector('.duration');
    const actions = row.querySelector('.run-actions');
    const inside = (node) => {
      if (!node) return false;
      const rect = node.getBoundingClientRect();
      const bounds = wrap.getBoundingClientRect();
      return rect.width > 0 && rect.left >= bounds.left - 0.5 && rect.right <= bounds.right + 0.5;
    };
    wrap.scrollLeft = 0;
    const columnsInside = [...row.children].every((child) => getComputedStyle(child).display === 'none' || inside(child));
    const overflows = wrap.scrollWidth > wrap.clientWidth + 1;
    const position = getComputedStyle(state).position;
    const edge = getComputedStyle(wrap).backgroundImage.includes('gradient');
    wrap.scrollLeft = wrap.scrollWidth;
    if (cancel) cancel.focus();
    const stateRect = state.getBoundingClientRect();
    const bounds = wrap.getBoundingClientRect();
    const topmost = document.elementFromPoint(stateRect.left + 4, (stateRect.top + stateRect.bottom) / 2);
    return {
      viewport: window.innerWidth,
      clientWidth: wrap.clientWidth,
      scrollWidth: wrap.scrollWidth,
      overflows,
      position,
      edge,
      columnsInside,
      statePinned: Math.abs(stateRect.left - bounds.left) < 1.5,
      statePainted: state.contains(topmost),
      durationReached: inside(duration),
      actionsReached: inside(actions),
      cancelReached: !cancel || (document.activeElement === cancel && inside(cancel)),
      attributed: row.classList.contains('workspace-attributed'),
    };
  });
  const assertRunsCard = async (attributed, widths) => {
    await renderRunsFixture(attributed);
    // Cards at or under this width are narrower than the grid's minimum tracks.
    const overflowLimit = attributed ? 1262 : 1180;
    const seen = [];
    for (const width of widths) {
      await page.setViewportSize({ width, height: 900 });
      const layout = await readRunsLayout();
      const expectOverflow = layout.clientWidth <= overflowLimit;
      seen.push({ width, clientWidth: layout.clientWidth, overflows: layout.overflows, expectOverflow });
      if (layout.attributed !== attributed || layout.overflows !== expectOverflow) {
        throw new Error(`Runs overflow does not follow the card width: ${JSON.stringify({ ...layout, expectOverflow, seen })}`);
      }
      if (expectOverflow) {
        if (layout.position !== 'sticky' || !layout.edge || !layout.statePinned || !layout.statePainted
          || !layout.durationReached || !layout.actionsReached || !layout.cancelReached) {
          throw new Error(`Overflowing runs table lost State, the scroll edge, or Duration and Actions: ${JSON.stringify(layout)}`);
        }
      } else if (layout.position !== 'static' || layout.edge || !layout.columnsInside) {
        throw new Error(`Fitting runs table is not a plain grid: ${JSON.stringify(layout)}`);
      }
    }
    if (!seen.some((sample) => sample.width === 1280 && sample.overflows)) {
      throw new Error(`1280px runs card must overflow: ${JSON.stringify(seen)}`);
    }
    if (!seen.some((sample) => sample.overflows) || !seen.some((sample) => !sample.overflows)) {
      throw new Error(`Runs threshold was not bracketed: ${JSON.stringify(seen)}`);
    }
    return seen;
  };
  // Realistic job names and full run IDs must not clip at 1440px, and a failed
  // run must still name the step it stopped at (Failed at keeps its column).
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.evaluate(async () => {
    const { renderRuns } = await import('/js/runs.js');
    const now = Date.now();
    const iso = (minutes) => new Date(now - minutes * 60000).toISOString();
    renderRuns([
      { run_id: 'jrun-20261020-0422-c1', job_id: 'task_pilot_pipeline', state: 'running', created_at: iso(4), duration_ms: 240000 },
      { run_id: 'jrun-20261020-0410-a7', job_id: 'task_gate_pipeline', state: 'success', created_at: iso(30), duration_ms: 180000 },
      {
        run_id: 'jrun-20261019-2310-f3', job_id: 'task_pr_pipeline', state: 'failed', created_at: iso(180), duration_ms: 360000,
        steps: [{ target_id: 'open-pr', state: 'failed', error_code: 'gh.pr_create_failed', error_message: 'GitHub returned 422' }],
      },
    ]);
  });
  await page.locator('#runs-body .runs-row[data-key^="run-"]').first().waitFor({ state: 'visible', timeout: 5000 });
  const desktopRuns = await page.evaluate(() => ({
    clipped: [...document.querySelectorAll('#runs-body .runs-row:not(.runs-header) .id, #runs-body .runs-row:not(.runs-header) .run-id')]
      .filter(cell => cell.scrollWidth > cell.clientWidth).map(cell => cell.textContent.trim()),
    failedStep: document.querySelector('#runs-body .run-failed-step')?.textContent.trim() || '',
  }));
  if (desktopRuns.clipped.length) throw new Error(`Jobs › History clips job names or run IDs at 1440px: ${JSON.stringify(desktopRuns)}`);
  if (desktopRuns.failedStep !== 'open-pr') throw new Error(`Failed at no longer names the failed step at 1440px: ${JSON.stringify(desktopRuns)}`);
  const runsCardWidths = [1280, 1360, 1396, 1400, 1444, 1460, 1600];
  await assertRunsCard(false, runsCardWidths);
  await assertRunsCard(true, runsCardWidths);
  await page.setViewportSize({ width: 375, height: 900 });
  await renderRunsFixture(true);
  if (await scrolls('runs-body') || await pageOverflow()) throw new Error('All-workspaces runs must not scroll sideways at 375px');
  const workspacePhone = await readRunsLayout();
  if (workspacePhone.position !== 'static' || workspacePhone.edge || workspacePhone.overflows || !workspacePhone.columnsInside) {
    throw new Error(`All-workspaces runs are not cards at 375px: ${JSON.stringify(workspacePhone)}`);
  }
  const workspaceToolbar = await page.evaluate(() => {
    const wrap = document.getElementById('runs-body');
    const toolbar = wrap.querySelector('.runs-toolbar');
    const rect = toolbar.getBoundingClientRect();
    const style = getComputedStyle(toolbar);
    const wrapRect = wrap.getBoundingClientRect();
    return {
      toolbarLeft: rect.left,
      toolbarRight: rect.right,
      bodyLeft: wrapRect.left + wrap.clientLeft,
      bodyRight: wrapRect.left + wrap.clientLeft + wrap.clientWidth,
      toolbarWidth: rect.width,
      marginLeft: parseFloat(style.marginLeft),
      marginRight: parseFloat(style.marginRight),
      bodyClientWidth: wrap.clientWidth,
    };
  });
  if (Math.abs(workspaceToolbar.toolbarWidth + workspaceToolbar.marginLeft + workspaceToolbar.marginRight - workspaceToolbar.bodyClientWidth) > 1
    || workspaceToolbar.toolbarLeft < workspaceToolbar.bodyLeft - 1
    || workspaceToolbar.toolbarRight > workspaceToolbar.bodyRight + 1) {
    throw new Error(`All-workspaces runs toolbar falls outside the 375px body: ${JSON.stringify(workspaceToolbar)}`);
  }
  await renderRunsFixture(false);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/incidents/events'));
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

async function assertSkipLinkAndFocusRings(page) {
  await page.keyboard.press('Tab');
  const firstFocus = await page.evaluate(() => {
    const link = document.getElementById('skip-link');
    const rect = link.getBoundingClientRect();
    const style = getComputedStyle(link);
    return {
      active: document.activeElement === link,
      visible: rect.width > 0 && rect.height > 0 && rect.top >= 0 && rect.bottom <= window.innerHeight,
      outlineWidth: style.outlineWidth,
      outlineStyle: style.outlineStyle,
      outlineColor: style.outlineColor,
    };
  });
  if (!firstFocus.active || !firstFocus.visible) throw new Error(`First Tab must reveal Skip to content: ${JSON.stringify(firstFocus)}`);
  if (firstFocus.outlineWidth !== '2px' || firstFocus.outlineStyle !== 'solid' || firstFocus.outlineColor !== 'rgb(122, 167, 255)') {
    throw new Error(`Skip link focus ring must be 2px accent: ${JSON.stringify(firstFocus)}`);
  }

  await page.keyboard.press('Enter');
  const focusedMain = await page.evaluate(() => {
    const pane = document.querySelector('.tab-pane.active');
    return document.activeElement.tagName === 'MAIN' && pane.contains(document.activeElement);
  });
  if (!focusedMain) throw new Error('Skip to content must move focus into the active pane main');

  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/incidents/errors'));
  const target = await page.evaluate(() => {
    const pane = document.querySelector('.tab-pane.active');
    const main = Array.from(pane.querySelectorAll('main')).find(node => !node.hidden && node.style.display !== 'none');
    return { href: document.getElementById('skip-link').getAttribute('href'), mainId: main?.id };
  });
  if (!target.mainId || target.href !== `#${target.mainId}`) throw new Error(`Skip target must follow the active route: ${JSON.stringify(target)}`);
  await page.locator('#skip-link').evaluate(link => link.click());
  const focusedRoutedMain = await page.evaluate(() => {
    const pane = document.querySelector('.tab-pane.active');
    return document.activeElement.tagName === 'MAIN' && pane.contains(document.activeElement);
  });
  if (!focusedRoutedMain) throw new Error('Skip to content must focus the routed main after navigation');

  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('tasks'));
  await page.evaluate(() => document.activeElement.blur());
  let sawSkipLink = false;
  for (let stop = 0; stop < 160; stop += 1) {
    const focused = await page.evaluate(() => {
      const element = document.activeElement;
      const style = getComputedStyle(element);
      return {
        id: element.id,
        label: element.getAttribute('aria-label') || element.textContent.trim().slice(0, 50),
        focusVisible: element.matches(':focus-visible'),
        outlineWidth: style.outlineWidth,
        outlineStyle: style.outlineStyle,
        outlineColor: style.outlineColor,
      };
    });
    if (stop > 0 && focused.id === 'skip-link') {
      sawSkipLink = true;
      break;
    }
    if (focused.focusVisible && (focused.outlineWidth !== '2px' || focused.outlineStyle !== 'solid' || focused.outlineColor !== 'rgb(122, 167, 255)')) {
      throw new Error(`Keyboard focus ring differs from the 2px accent ring: ${JSON.stringify(focused)}`);
    }
    await page.keyboard.press('Tab');
  }
  if (!sawSkipLink) throw new Error('Keyboard focus walk did not cycle through the dashboard controls');
}

const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const testScenario = (locksOnly || authExclusionsOnly) ? Buffer.from(locksOnlyScenario) : fs.readFileSync(scenarios);
  const served = name === '/test.mjs' ? { data: testScenario, type: 'text/javascript' } : dashboardFile(name);
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
  if (!locksOnly && !authExclusionsOnly) await assertClockRepaint(browser, `http://127.0.0.1:${server.address().port}`, evidence);
  if (clockOnly) console.log('Dashboard clock switch repainted kept task detail and run rows in place.');
  if (!locksOnly && !authExclusionsOnly && !clockOnly) await assertTaskLookupClear(browser, `http://127.0.0.1:${server.address().port}`, evidence);
  if (!locksOnly && !authExclusionsOnly && !clockOnly) await assertFixPrReadinessRefresh(browser, `http://127.0.0.1:${server.address().port}`, evidence);
  if (process.argv[4] === '--task-lookup') {
    console.log('Dashboard task lookup dismissal browser checks passed.');
  }
  if (process.argv[4] !== '--task-lookup' && !clockOnly) {
  if (!locksOnly && !authExclusionsOnly) await assertWorkspaceScope(browser, `http://127.0.0.1:${server.address().port}`, evidence);
  if (!locksOnly && !authExclusionsOnly) await assertTaskLinkScopeHistory(browser, `http://127.0.0.1:${server.address().port}`, evidence);
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
  if (!locksOnly && !authExclusionsOnly && process.argv[4] !== '--run-detail') {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(() => globalThis.showHealthEvidence('errors'));
    const layout = await page.evaluate(() => {
      const panel = document.getElementById('diag-body');
      const errors = panel.querySelector('.diagnostics-errors-main');
      const preview = errors.querySelector('.error-message-preview');
      const panelBounds = panel.getBoundingClientRect();
      const errorsBounds = errors.getBoundingClientRect();
      return {
        viewport: [window.innerWidth, window.innerHeight],
        contained: errorsBounds.left >= panelBounds.left - 1 && errorsBounds.right <= panelBounds.right + 1,
        overflowX: getComputedStyle(errors).overflowX,
        lineClamp: getComputedStyle(preview).webkitLineClamp,
      };
    });
    if (layout.viewport[0] !== 1440 || layout.viewport[1] !== 900 || !layout.contained
      || layout.overflowX !== 'auto' || layout.lineClamp !== '2') {
      throw new Error(`Errors panel must contain its table and wrap messages at 1440x900: ${JSON.stringify(layout)}`);
    }
  }
  if (authExclusionsOnly) {
    await assertCrewWindow(page);
    console.log('Authentication exclusion provider, host, time, class, credential route, login hint and next probe rendered at desktop and mobile widths.');
  } else if (locksOnly) {
    await assertLockedTaskNavigation(page);
    console.log('Locked-task axe guard, keyboard access and task navigation passed.');
  } else {
  await assertSkipLinkAndFocusRings(page);
  console.log('Dashboard skip link and keyboard focus checks passed.');
  await assertRunTaskLabels(page, evidence);
  await assertRunDetailActions(page, evidence);
  if (process.argv[4] === '--run-detail') {
    await assertRunDetailPresentation(page, evidence);
    if (failures.length) throw new Error(failures.join('\n'));
    console.log('Chromium run-detail action feedback, scheduled polling, presentation and log wrapping passed.');
  } else {
    await assertTaskTitleLayout(page, evidence);
    await assertPullRequestLinks(page, evidence);
    await assertFrictionTaskLinks(page);
    await page.evaluate(() => globalThis.showTaskPaginationEvidence());
    await page.waitForFunction(() => document.getElementById('tasks-count').textContent === '1–20 of 55');
    await assertRowKeyboard(page);
    await assertLockedTaskNavigation(page);
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
    // Closed statuses wait behind More, a disclosure that controls their group.
    const more = page.locator('#task-filter .chip-more');
    if (await page.locator('#task-filter .chip[data-status="done"]').isVisible()) throw new Error('An unpressed Done chip must wait behind More');
    await more.click();
    if (await more.getAttribute('aria-expanded') !== 'true') throw new Error('More must report itself expanded once open');
    await page.locator('#task-filter .chip[data-status="done"]').click();
    await page.waitForFunction(() => document.getElementById('task-filter-summary').textContent.includes('done'));
    await page.locator('#task-filter .chip[data-role="all"]').click();
    const firstTask = page.locator('#tasks-body .row[data-key^="task-"]:not(.header)').first();
    // Open it the way a person does, by its title: on a narrow row the centre
    // of the box is the crew select, which takes the click for itself.
    await firstTask.locator('.title').click();
    const detail = page.locator('#task-detail-body .row-detail').first();
    await detail.waitFor({ state: 'visible' });
    const pageOverflowsHorizontally = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
    if (pageOverflowsHorizontally) throw new Error('Task filters or expanded details introduced horizontal page clipping');
    await assertNarrowTableLayouts(page);
    for (const subtab of ['incidents', 'errors', 'metrics']) {
      await page.evaluate(subtab => globalThis.showHealthEvidence(subtab), subtab);
      for (const width of [1440, 390]) {
        await page.setViewportSize({ width, height: 900 });
        const body = page.locator(subtab === 'metrics' ? '#metrics-body' : '#diag-body');
        if (!(await body.isVisible())) throw new Error(`Health ${subtab} invisible at ${width}px`);
        if (subtab === 'metrics') {
          // The trend fills the panel, and a table wider than its card scrolls
          // inside it instead of pushing the page sideways.
          const layout = await page.evaluate(() => {
            const metrics = document.getElementById('metrics-body');
            return {
              overflow: document.documentElement.scrollWidth > window.innerWidth,
              cards: metrics.querySelectorAll('.mx-sc').length,
              plot: metrics.querySelector('.mx-plot')?.getBoundingClientRect().width || 0,
              clipped: [...metrics.querySelectorAll('.mx-table-wrap')]
                .filter(wrap => wrap.scrollWidth > wrap.clientWidth + 1 && getComputedStyle(wrap).overflowX !== 'auto').length,
            };
          });
          if (layout.overflow || layout.cards !== 4 || layout.plot < 200 || layout.clipped) {
            throw new Error(`Metrics layout broken at ${width}px: ${JSON.stringify(layout)}`);
          }
        }
        await page.screenshot({ path: path.join(evidence, `health-${subtab}-${width}.png`), fullPage: true });
      }
    }
    console.log("Health behavior and Metrics layout passed at 1440px and 390px.");
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
      const feedback = page.locator('#metrics-body [role="status"]');
      if (!(await feedback.isVisible())) throw new Error(`Refresh feedback invisible at ${width}px`);
      if (!(await feedback.textContent()).includes('Refreshing')) throw new Error('Missing retained-data feedback');
      if (await feedback.getAttribute('aria-live') !== 'polite') throw new Error('Missing accessible live feedback');
      if (!(await page.locator('#refresh-btn').isEnabled())) throw new Error('Retry disabled');
      await page.screenshot({ path: path.join(evidence, `refresh-${width}.png`) });
    }
    await assertCostPage(page);
    await assertRunStepLayout(page);
    await assertRunDetailPresentation(page, evidence);
    await assertCrewWindow(page);
    await assertStillWaiting(page);
    await assertTopbarSingleRow(page);
    await assertProtocolSkewFailure(page);
    await new Promise(resolve => server.close(resolve));
    await page.evaluate(() => {
      globalThis.fetch = globalThis.nativeFetch;
      document.getElementById('refresh-btn').click();
    });
    await page.waitForFunction(() => document.getElementById('meta-text').textContent.includes('offline'));
    if (!(await page.locator('#conn-status').getAttribute('class')).includes('red')) throw new Error('Stopped server must show red connection status');
    if (failures.length) throw new Error(failures.join('\n'));
    fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, fleetScope: await page.evaluate(() => globalThis.fleetScopeEvidence), scenarios: 'Task, run and friction rows with no controls nested in a button, one Tab stop per row list with Up/Down/Home/End between rows, Enter/Space toggling the focused task, its disclosure keeping focus across a refresh that rebuilds the row, and / focusing the task search; Runs pinned first column and scroll edge while the card is narrower than the grid, including at 1280px, and a plain table once the card fits, for the default grid and All-workspaces; card layout and an in-body toolbar at 375px; Runs, Audit events and Errors as cards without sideways scrolling at 375px, unchanged tables at 1280px; Cost paints provider and derived dollars and normalized tokens for a fixed payload at 1280px and 375px, #analytics/cost restores its window and the retired Health cost routes do not open it; Task pagination page 1/page 2 with visible, unoccluded first rows and accessible Previous/Next at 1280px and 390px; failed run step target, state, duration and exit code readable with click and keyboard expansion at 1280px, 480px and 390px; single-row top bar of identical height across Tasks, Automation, Settings, Knowledge and Plugins with a fixed host chip and Refresh offset when the throttle verdict flips or the provider usage readings change at 1024px, 1280px, 1440px, 1600px and 1920px; terminal protocol skew code, fingerprints and repair at 1280px and 390px; pull drain crew window runnable crews and preflight/provider-unavailable exclusions readable at 1280px and 390px; Tasks, Recent runs, Errors, Operations: cold, stale refresh, scope changes, reordered responses, empty success, network error; Metrics HTTP failure isolation and network offline/recovery' }, null, 2));
    console.log('Chromium dashboard lifecycle and accessible visible feedback passed.');
  }
  }
  }
} finally {
  await browser?.close();
  if (server.listening) await new Promise(resolve => server.close(resolve));
}
