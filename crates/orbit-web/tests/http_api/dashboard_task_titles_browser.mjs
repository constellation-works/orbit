import fs from 'node:fs';
import path from 'node:path';

// Runs inside the full dashboard browser harness, with its default dock.
export async function assertTaskTitleLayout(page, evidence) {
  await page.evaluate(async () => {
    const { setWorkspace } = await import('/js/common.js');
    const { setActiveTab } = await import('/js/router.js');
    const { renderTasks, cacheCrewPayload } = await import('/js/tasks.js');
    setWorkspace('one');
    await globalThis.showTaskPaginationEvidence();
    setActiveTab('tasks', { refresh: false });
    const longTitle = 'Dashboard task titles should show enough information to recognise the work before opening the full detail panel';
    const tasks = [
      { id: 'TITLE-1', title: longTitle },
      { id: 'TITLE-2', title: longTitle, os_requirement: { any_of: ['macos'] }, readiness: { preparing: true, gaps: [] } },
      { id: 'TITLE-3', title: 'Short title' },
      { id: 'TITLE-4', title: 'LongUnbrokenTitle'.repeat(8) },
    ].map(task => ({ ...task, status: 'backlog', priority: 'medium', status_transitions: [{ status: 'in-progress', required_field: null }] }));
    const context = {
      getTasks: () => tasks,
      getActiveStatuses: () => new Set(['backlog', 'in-progress']),
      statusOrder: ['backlog', 'in-progress'],
      replaceTask: updated => Object.assign(tasks.find(task => task.id === updated.id), updated),
    };
    globalThis.taskTitleFixture = { priorFetch: globalThis.fetch, writes: [], tasks, render: () => renderTasks(tasks, context) };
    globalThis.fetch = async (url, options) => {
      const request = new URL(url, window.location.href);
      if (options?.method === 'PATCH' && /\/tasks\/TITLE-/.test(request.pathname)) {
        const update = JSON.parse(options.body);
        globalThis.taskTitleFixture.writes.push(update);
        const task = tasks.find(task => request.pathname.endsWith('/' + task.id));
        return new Response(JSON.stringify({ ...task, ...update }), { status: 200 });
      }
      return globalThis.taskTitleFixture.priorFetch(url, options);
    };
    cacheCrewPayload({ default_crew: 'opus', crews: [{ name: 'sol' }, { name: 'opus' }] });
    document.querySelector('main.tasks-layout').style.removeProperty('--dock-w');
    globalThis.taskTitleFixture.render();
  });
  try {
    const measurements = [];
    for (const viewport of [{ width: 1280, height: 800 }, { width: 1440, height: 900 }, { width: 390, height: 844 }]) {
      await page.setViewportSize(viewport);
      const layout = await page.evaluate(() => {
        const rows = [...document.querySelectorAll('#tasks-body .row[data-key^="task-TITLE-"]')];
        const titles = rows.slice(0, 2).map(row => {
          const title = row.querySelector('.title');
          const box = title.getBoundingClientRect();
          const text = [...title.childNodes].find(node => node.nodeType === Node.TEXT_NODE);
          const lines = new Set();
          let visibleCharacters = 0;
          for (let index = 0; index < text.length; index++) {
            const range = document.createRange();
            range.setStart(text, index);
            range.setEnd(text, index + 1);
            const rect = range.getBoundingClientRect();
            // Leave room for the ellipsis at the end of the second line.
            const right = box.right - (rect.bottom > box.top + 20 ? 12 : 0);
            if (rect.width > 0 && rect.bottom <= box.bottom + 1 && rect.right <= right && rect.left >= box.left) {
              visibleCharacters++;
              lines.add(Math.round(rect.top));
            }
          }
          return { width: box.width, visibleCharacters, lines: lines.size };
        });
        return {
          panelWidth: document.getElementById('tasks-panel').getBoundingClientRect().width,
          grid: getComputedStyle(rows[0]).gridTemplateColumns,
          heights: rows.map(row => row.getBoundingClientRect().height),
          titles,
          horizontalOverflow: document.getElementById('tasks-body').scrollWidth > document.getElementById('tasks-body').clientWidth + 1,
        };
      });
      measurements.push({ ...viewport, ...layout });
      if (Math.max(...layout.heights) - Math.min(...layout.heights) > 1 || layout.horizontalOverflow) {
        throw new Error(`Task rows must have consistent height without sideways scrolling: ${JSON.stringify(measurements.at(-1))}`);
      }
      if (viewport.width >= 1280 && layout.titles.some(title => title.lines !== 2 || title.visibleCharacters < 60)) {
        throw new Error(`Plain and badged titles must show at least 60 characters in two lines: ${JSON.stringify(measurements.at(-1))}`);
      }
      await page.screenshot({ path: path.join(evidence, `task-titles-${viewport.width}.png`) });
    }
    await page.setViewportSize({ width: 1280, height: 800 });
    const row = page.locator('#tasks-body [data-key="task-TITLE-1"]');
    const status = row.locator('.task-status-select');
    if (await status.evaluate(node => getComputedStyle(node).opacity) !== '0') throw new Error('Idle status must show its chip');
    await row.locator('.title').focus();
    await page.keyboard.press('Tab');
    const focused = await status.evaluate(node => ({ active: document.activeElement === node, opacity: getComputedStyle(node).opacity }));
    if (!focused.active || focused.opacity !== '1') throw new Error(`Tab must reveal the native status editor: ${JSON.stringify(focused)}`);
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Enter');
    await page.keyboard.press('Tab');
    await page.waitForFunction(() => globalThis.taskTitleFixture.tasks[0].status === 'in-progress', undefined, { timeout: 5000 }).catch(async error => {
      const state = await page.evaluate(() => ({
        writes: globalThis.taskTitleFixture.writes,
        task: globalThis.taskTitleFixture.tasks[0],
        feedback: document.getElementById('tasks-body').textContent,
        value: document.querySelector('[data-key="task-TITLE-1"] .task-status-select')?.value,
      }));
      throw new Error(`${error.message}: ${JSON.stringify(state)}`);
    });
    await row.locator('.title').focus();
    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    const crew = row.locator('.task-crew-select');
    if (!(await crew.evaluate(node => document.activeElement === node && getComputedStyle(node).opacity === '1'))) {
      throw new Error('Tab must reach and reveal the native crew editor');
    }
    await page.keyboard.press('End');
    await page.keyboard.press('Enter');
    await page.keyboard.press('Tab');
    await page.waitForFunction(() => globalThis.taskTitleFixture.tasks[0].crew === 'sol', undefined, { timeout: 5000 });
    const writes = await page.evaluate(() => globalThis.taskTitleFixture.writes);
    if (!writes.some(write => write.status === 'in-progress') || !writes.some(write => write.crew === 'sol')) {
      throw new Error(`Keyboard edits must reach the task mutation boundary: ${JSON.stringify(writes)}`);
    }
    fs.writeFileSync(path.join(evidence, 'task-titles-result.json'), JSON.stringify({ passed: true, measurements, writes }, null, 2));
  } finally {
    await page.evaluate(() => { globalThis.fetch = globalThis.taskTitleFixture.priorFetch; delete globalThis.taskTitleFixture; });
  }
}
