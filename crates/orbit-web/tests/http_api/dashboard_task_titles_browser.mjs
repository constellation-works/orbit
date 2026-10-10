import fs from 'node:fs';
import path from 'node:path';

// A long title, one that fits, and one with no break opportunity at all.
const longTitle = 'Dashboard task titles should show enough information to recognise the work before opening the full detail panel';
const titleTexts = [longTitle, longTitle, 'Short title', 'LongUnbrokenTitle'.repeat(8)];

// Runs inside the full dashboard browser harness, with its default dock.
export async function assertTaskTitleLayout(page, evidence) {
  await page.evaluate(async titleTexts => {
    const { setWorkspace } = await import('/js/common.js');
    const { setActiveTab } = await import('/js/router.js');
    const { renderTasks, cacheCrewPayload } = await import('/js/tasks.js');
    setWorkspace('one');
    await globalThis.showTaskPaginationEvidence();
    setActiveTab('tasks', { refresh: false });
    const tasks = [
      { id: 'TITLE-1', title: titleTexts[0] },
      { id: 'TITLE-2', title: titleTexts[1], os_requirement: { any_of: ['macos'] }, readiness: { preparing: true, gaps: [] } },
      { id: 'TITLE-3', title: titleTexts[2] },
      { id: 'TITLE-4', title: titleTexts[3] },
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
  }, titleTexts);
  try {
    const measurements = [];
    for (const viewport of [{ width: 1280, height: 800 }, { width: 1440, height: 900 }, { width: 390, height: 844 }, { width: 375, height: 812 }]) {
      await page.setViewportSize(viewport);
      const layout = await page.evaluate(() => {
        const rows = [...document.querySelectorAll('#tasks-body .row[data-key^="task-TITLE-"]')];
        const titles = rows.map(row => {
          const title = row.querySelector('.title');
          const style = getComputedStyle(title);
          const box = title.getBoundingClientRect();
          const text = [...title.childNodes].find(node => node.nodeType === Node.TEXT_NODE);
          const lineTops = new Set();
          let visibleCharacters = 0;
          let characterHeight = 0;
          for (let index = 0; index < text.length; index++) {
            const range = document.createRange();
            range.setStart(text, index);
            range.setEnd(text, index + 1);
            const rect = range.getBoundingClientRect();
            if (rect.width === 0) continue;
            lineTops.add(Math.round(rect.top));
            characterHeight = Math.max(characterHeight, rect.height);
            // Leave room for the ellipsis at the end of the line.
            if (rect.left >= box.left && rect.right <= box.right - 12) visibleCharacters++;
          }
          const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
          return {
            id: row.dataset.key,
            whiteSpace: style.whiteSpace,
            textOverflow: style.textOverflow,
            overflowX: style.overflowX,
            width: box.width,
            contentHeight: title.clientHeight - padding,
            characterHeight,
            textLines: lineTops.size,
            overflowing: title.scrollWidth > title.clientWidth,
            visibleCharacters,
            rowTooltip: row.getAttribute('title'),
          };
        });
        const body = document.getElementById('tasks-body');
        return {
          panelWidth: document.getElementById('tasks-panel').getBoundingClientRect().width,
          grid: getComputedStyle(rows[0]).gridTemplateColumns,
          stacked: getComputedStyle(rows[0]).gridTemplateAreas !== 'none',
          heights: rows.map(row => row.getBoundingClientRect().height),
          titles,
          horizontalOverflow: body.scrollWidth > body.clientWidth + 1,
        };
      });
      const measured = { ...viewport, ...layout };
      measurements.push(measured);
      const fail = reason => { throw new Error(`${reason}: ${JSON.stringify(measured)}`); };
      if (layout.horizontalOverflow) fail('Task rows must not scroll sideways');
      // Rows keep one height whatever the title length or badges, and the
      // desktop row is compact because no title reserves a second line.
      if (Math.max(...layout.heights) - Math.min(...layout.heights) > 1) fail('Task rows must have a consistent height');
      if (!layout.stacked && Math.max(...layout.heights) > 44) fail('A desktop task row must stay compact (44px at most)');
      for (const title of layout.titles) {
        if (title.whiteSpace !== 'nowrap' || title.textLines !== 1) fail(`Task title ${title.id} must render on a single line`);
        if (title.contentHeight >= title.characterHeight * 1.5) fail(`Task title ${title.id} must be one line box tall`);
      }
      const [plain, badged, short, unbroken] = layout.titles;
      for (const title of [plain, badged, unbroken]) {
        if (!title.overflowing || title.textOverflow !== 'ellipsis' || title.overflowX !== 'hidden') {
          fail(`Overflowing title ${title.id} must be clipped with an ellipsis`);
        }
      }
      if (short.overflowing) fail('A short title must show whole, without an ellipsis');
      for (const [index, title] of layout.titles.entries()) {
        if (title.rowTooltip !== titleTexts[index]) fail(`Row ${title.id} must carry the full title as its tooltip`);
      }
      // The clipped text stays reachable by name for assistive technology.
      for (const [index, taskId] of ['TITLE-1', 'TITLE-2', 'TITLE-3', 'TITLE-4'].entries()) {
        const named = await page.locator(`#tasks-body [data-key="task-${taskId}"]`).getByRole('button', { name: titleTexts[index] }).count();
        if (named !== 1) fail(`The ${taskId} title button must be named with its full title`);
      }
      // Beside the default dock the single line must still be wide enough to
      // recognise the work: the title column gained its width in ORB-15231.
      if (viewport.width >= 1280 && (plain.width < 260 || plain.visibleCharacters < 30 || badged.visibleCharacters < 20)) {
        fail('Plain and badged titles must show enough of their text on one line');
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
