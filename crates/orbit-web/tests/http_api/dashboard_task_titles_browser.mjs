import fs from 'node:fs';
import path from 'node:path';

// A long title, one that fits, and one with no break opportunity at all, long
// enough to overflow the full-width title column at 1440px.
const longTitle = 'Dashboard task titles should show enough information to recognise the work before opening the full detail panel. '.repeat(2).trim();
const titleTexts = [longTitle, longTitle, 'Short title', 'LongUnbrokenTitle'.repeat(12)];

// Runs inside the full dashboard browser harness, with its full-width Tasks page.
export async function assertTaskTitleLayout(page, evidence) {
  await page.evaluate(async titleTexts => {
    const { setWorkspace } = await import('/js/common.js');
    const { setActiveTab } = await import('/js/router.js');
    const { renderTasks, cacheCrewPayload } = await import('/js/tasks.js');
    setWorkspace('one');
    await globalThis.showTaskPaginationEvidence();
    setActiveTab('tasks', { refresh: false });
    // `archived` is off the filter and off the lifecycle table, so moving to it
    // is a forced change that takes the row out of the list.
    const transitionsFor = status => ['backlog', 'in-progress']
      .filter(target => target !== status)
      .map(target => ({ status: target, required_field: null }));
    const tasks = [
      { id: 'TITLE-1', title: titleTexts[0] },
      { id: 'TITLE-2', title: titleTexts[1], os_requirement: { any_of: ['macos'] }, readiness: { preparing: true, gaps: [] } },
      { id: 'TITLE-3', title: titleTexts[2] },
      { id: 'TITLE-4', title: titleTexts[3] },
    ].map(task => ({ ...task, status: 'backlog', priority: 'medium', status_transitions: transitionsFor('backlog') }));
    const context = {
      getTasks: () => tasks,
      getActiveStatuses: () => new Set(['backlog', 'in-progress']),
      statusOrder: ['backlog', 'in-progress', 'archived'],
      replaceTask: updated => Object.assign(tasks.find(task => task.id === updated.id), updated),
    };
    const crews = { default_crew: 'opus', crews: [{ name: 'sol' }, { name: 'opus' }] };
    globalThis.taskTitleFixture = { priorFetch: globalThis.fetch, writes: [], tasks, failStatus: false, render: () => renderTasks(tasks, context) };
    const json = payload => new Response(JSON.stringify(payload), { status: 200 });
    globalThis.fetch = async (url, options) => {
      const request = new URL(url, window.location.href);
      if (options?.method === 'PATCH' && /\/tasks\/TITLE-/.test(request.pathname)) {
        const update = JSON.parse(options.body);
        globalThis.taskTitleFixture.writes.push(update);
        if (update.status && globalThis.taskTitleFixture.failStatus) {
          return new Response(JSON.stringify({ error: 'status store unavailable' }), { status: 500 });
        }
        const task = tasks.find(task => request.pathname.endsWith('/' + task.id));
        const { force, ...fields } = update;
        Object.assign(task, fields);
        if (fields.status) task.status_transitions = transitionsFor(fields.status);
        return json(task);
      }
      // The dashboard's 30s poll can land mid-test: it reads these rows back
      // instead of replacing them with the harness's own task list.
      if (request.pathname === '/api/tasks') return json({ items: tasks, total: tasks.length, limit: 50, truncated: false });
      if (request.pathname === '/api/crews') return json(crews);
      return globalThis.taskTitleFixture.priorFetch(url, options);
    };
    cacheCrewPayload(crews);
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
      // Rows keep one height whatever the title length or badges. A desktop
      // row is two lines by design (the title, then the full ID and its wait or
      // run), so it stays compact at 56px: no title reserves a third line.
      if (Math.max(...layout.heights) - Math.min(...layout.heights) > 1) fail('Task rows must have a consistent height');
      if (!layout.stacked && Math.max(...layout.heights) > 56.5) fail('A desktop task row is two fixed lines, title then ID, so it stays compact (56px at most)');
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
      // The full-width single line must still be wide enough to
      // recognise the work: the title column gained its width in ORB-15231.
      if (viewport.width >= 1280 && (plain.width < 260 || plain.visibleCharacters < 30 || badged.visibleCharacters < 20)) {
        fail('Plain and badged titles must show enough of their text on one line');
      }
      await page.screenshot({ path: path.join(evidence, `task-titles-${viewport.width}.png`) });
    }
    await page.setViewportSize({ width: 1280, height: 800 });
    const row = page.locator('#tasks-body [data-key="task-TITLE-1"]');
    // Rows group by status, so a row edits only its crew; status is a property
    // of the task detail pane.
    if (await row.locator('.task-status-select').count() !== 0) throw new Error('A task row must not carry a status editor');
    await row.locator('.title').focus();
    await page.keyboard.press('Tab');
    const crew = row.locator('.task-crew-select');
    if (!(await crew.evaluate(node => document.activeElement === node && getComputedStyle(node).opacity === '1'))) {
      throw new Error('Tab must reach and reveal the native crew editor');
    }
    await crew.selectOption('sol');
    await page.waitForFunction(() => globalThis.taskTitleFixture.tasks[0].crew === 'sol', undefined, { timeout: 5000 }).catch(async error => {
      const state = await page.evaluate(() => ({
        writes: globalThis.taskTitleFixture.writes,
        task: globalThis.taskTitleFixture.tasks[0],
        value: document.querySelector('[data-key="task-TITLE-1"] .task-crew-select')?.value,
        options: [...(document.querySelector('[data-key="task-TITLE-1"] .task-crew-select')?.options || [])].map(option => option.value),
      }));
      throw new Error(`${error.message}: ${JSON.stringify(state)}`);
    });
    await row.locator('.title').focus();
    await page.keyboard.press('Enter');
    const status = page.locator('#task-detail-body [data-key="detail-TITLE-1"] .task-status-select');
    await status.waitFor({ state: 'visible' });
    await status.focus();
    if (!(await status.evaluate(node => document.activeElement === node && getComputedStyle(node).opacity === '1'))) {
      throw new Error('Focus must reveal the native status editor');
    }
    await status.evaluate(node => node.addEventListener('change', event => {
      globalThis.taskTitleFixture.keyboardStatusChange = { value: node.value, trusted: event.isTrusted };
    }, { once: true }));
    // macOS headless Chromium does not navigate the native popup with
    // ArrowDown/Enter. Native select typeahead commits the matching option
    // directly, including when the current option is hidden and disabled.
    // Use real keyboard input, not selectOption or a dispatched change event.
    await page.keyboard.press('i');
    await page.waitForFunction(() => globalThis.taskTitleFixture.tasks[0].status === 'in-progress', undefined, { timeout: 5000 }).catch(async error => {
      const state = await page.evaluate(() => ({
        writes: globalThis.taskTitleFixture.writes,
        task: globalThis.taskTitleFixture.tasks[0],
        feedback: document.getElementById('tasks-body').textContent,
        value: document.querySelector('[data-key="detail-TITLE-1"] .task-status-select')?.value,
      }));
      throw new Error(`${error.message}: ${JSON.stringify(state)}`);
    });
    const { writes, keyboardStatusChange } = await page.evaluate(() => ({
      writes: globalThis.taskTitleFixture.writes,
      keyboardStatusChange: globalThis.taskTitleFixture.keyboardStatusChange,
    }));
    if (keyboardStatusChange?.value !== 'in-progress' || keyboardStatusChange.trusted !== true) {
      throw new Error(`Keyboard typeahead must emit a native status change: ${JSON.stringify(keyboardStatusChange)}`);
    }
    if (writes.length !== 2 || writes[0].crew !== 'sol' || JSON.stringify(writes[1]) !== JSON.stringify({ status: 'in-progress' })) {
      throw new Error(`Keyboard edits must reach the task mutation boundary: ${JSON.stringify(writes)}`);
    }
    await assertStatusChangeFoldsDetail(page, evidence);
    // The edits' Undo feedback expires with a redraw in this fixture's
    // context; let it land here rather than in a later scenario.
    await page.waitForFunction(() => !document.querySelector('#tasks-body .mutation-feedback'), undefined, { timeout: 15000 });
    fs.writeFileSync(path.join(evidence, 'task-titles-result.json'), JSON.stringify({ passed: true, measurements, writes, keyboardStatusChange }, null, 2));
  } finally {
    await page.evaluate(() => { globalThis.fetch = globalThis.taskTitleFixture.priorFetch; delete globalThis.taskTitleFixture; });
  }
}

// Status lives in the task detail pane and rows group by status, so a status
// change from the detail folds it in. Its feedback and undo follow the row (or a
// notice when the new status is filtered out); a refused change keeps the detail
// open with its feedback; a refresh that changes status leaves rows as they are.
async function assertStatusChangeFoldsDetail(page, evidence) {
  const fail = async reason => {
    const state = await page.evaluate(() => ({
      hash: window.location.hash,
      writes: globalThis.taskTitleFixture.writes,
      tasks: globalThis.taskTitleFixture.tasks.map(({ id, status }) => ({ id, status })),
      body: document.getElementById('tasks-body').innerText,
    }));
    throw new Error(`${reason}: ${JSON.stringify(state)}`);
  };
  const row = id => page.locator(`#tasks-body .row[data-key="task-${id}"]`);
  const detail = id => page.locator(`#task-detail-body [data-key="detail-${id}"]`);
  const note = id => row(id).locator('.task-status-note');
  const notice = id => page.locator(`#tasks-body [data-key="status-notice-${id}"]`);
  const openParam = () => page.evaluate(() => new URLSearchParams(window.location.hash.split('?')[1] || '').get('open'));
  const expanded = async id => (await row(id).locator('.title').getAttribute('aria-expanded')) === 'true';
  const statusOf = id => page.evaluate(id => globalThis.taskTitleFixture.tasks.find(task => task.id === id).status, id);
  const waitFor = (predicate, arg, reason) => page.waitForFunction(predicate, arg, { timeout: 5000 }).catch(() => fail(reason));
  // A row toggle animates through a view transition; capture after it ends.
  const screenshot = async name => {
    await page.waitForFunction(() => !document.getAnimations().some(animation => animation.effect?.pseudoElement?.startsWith('::view-transition')), undefined, { timeout: 5000 }).catch(() => {});
    await page.screenshot({ path: path.join(evidence, name) });
  };
  const open = async id => {
    await row(id).locator('.title').click();
    await detail(id).locator('.task-status-select').waitFor({ state: 'visible' });
    if (await openParam() !== id) await fail(`Opening ${id} must name it in the open= URL parameter`);
  };

  // The keyboard edit above moved TITLE-1 to in-progress from its open detail.
  await row('TITLE-1').locator('.task-status-note').waitFor({ state: 'visible' });
  if (await detail('TITLE-1').count() !== 0 || await expanded('TITLE-1')) await fail('A saved status change must fold the detail in');
  if (await openParam() !== null) await fail('A folded detail must leave the open= URL parameter');
  if (!(await note('TITLE-1').innerText()).includes('status saved')) await fail('The collapsed row must carry the saved feedback');
  await screenshot('task-status-folded.png');

  // Undo is a status change too: it runs from the collapsed row and keeps it folded.
  await note('TITLE-1').getByRole('button', { name: 'undo' }).click();
  await waitFor(() => globalThis.taskTitleFixture.tasks[0].status === 'backlog', undefined, 'Undo on the folded row must restore the prior status');
  await waitFor(() => /status saved/.test(document.querySelector('[data-key="task-TITLE-1"] .task-status-note')?.textContent || ''), undefined, 'The undo must report on the folded row');
  if (await detail('TITLE-1').count() !== 0 || await expanded('TITLE-1')) await fail('Undo must not reopen the folded row');

  // A failed write and a declined forced change both keep the detail open with their feedback.
  await open('TITLE-3');
  await page.evaluate(() => { globalThis.taskTitleFixture.failStatus = true; });
  await detail('TITLE-3').locator('.task-status-select').selectOption('in-progress');
  await waitFor(() => /status update failed/.test(document.querySelector('[data-key="detail-TITLE-3"] .mutation-feedback.error')?.textContent || ''), undefined, 'A failed status change must show its error in the open detail');
  await page.evaluate(() => { globalThis.taskTitleFixture.failStatus = false; });
  if (!(await expanded('TITLE-3')) || await openParam() !== 'TITLE-3') await fail('A failed status change must leave the row open');
  page.once('dialog', dialog => dialog.dismiss());
  await detail('TITLE-3').locator('.task-status-select').selectOption('archived');
  await waitFor(() => /status update cancelled/.test(document.querySelector('[data-key="detail-TITLE-3"] .mutation-feedback.error')?.textContent || ''), undefined, 'A declined forced change must show its refusal in the open detail');
  if (!(await expanded('TITLE-3')) || await openParam() !== 'TITLE-3' || await statusOf('TITLE-3') !== 'backlog') {
    await fail('A declined forced change must write nothing and leave the row open');
  }

  // A status change from elsewhere, seen on refresh, moves rows without opening or folding them.
  await page.evaluate(() => {
    for (const task of globalThis.taskTitleFixture.tasks.filter(task => task.id === 'TITLE-3' || task.id === 'TITLE-2')) task.status = 'in-progress';
    globalThis.taskTitleFixture.render();
  });
  if (!(await expanded('TITLE-3')) || await detail('TITLE-3').count() !== 1 || await openParam() !== 'TITLE-3') await fail('A refreshed status must leave an open row open');
  if (await expanded('TITLE-2') || await note('TITLE-2').count() !== 0) await fail('A refreshed status must leave a closed row closed, without status feedback');
  await page.evaluate(() => {
    for (const task of globalThis.taskTitleFixture.tasks.filter(task => task.id === 'TITLE-3' || task.id === 'TITLE-2')) task.status = 'backlog';
    globalThis.taskTitleFixture.render();
  });
  await row('TITLE-3').locator('.title').click();
  await detail('TITLE-3').waitFor({ state: 'detached', timeout: 5000 }).catch(() => fail('A row must close by hand'));
  if (await note('TITLE-3').count() !== 0) await fail('Closing a row by hand must not carry a refused change onto it');

  // A forced move out of the filter folds the detail too; the row leaves the
  // list, so the feedback and its undo stay reachable in a notice above it.
  await open('TITLE-4');
  page.once('dialog', dialog => dialog.accept());
  await detail('TITLE-4').locator('.task-status-select').selectOption('archived');
  await notice('TITLE-4').waitFor({ state: 'visible', timeout: 5000 }).catch(() => fail('A change that filters the row out must leave a notice'));
  if (await row('TITLE-4').count() !== 0 || await openParam() !== null) await fail('A row moved out of the filter must leave the list and the open= URL parameter');
  const noticeText = await notice('TITLE-4').innerText();
  if (!noticeText.includes('archived') || !noticeText.includes('status forced')) await fail('The notice must name the new status and the forced change');
  await screenshot('task-status-notice.png');
  await notice('TITLE-4').getByRole('button', { name: 'undo' }).click();
  await waitFor(() => globalThis.taskTitleFixture.tasks[3].status === 'backlog', undefined, 'Undo from the notice must restore the prior status');
  await row('TITLE-4').locator('.task-status-note').waitFor({ state: 'visible', timeout: 5000 }).catch(() => fail('The undone row must return folded, with its feedback'));
  if (await notice('TITLE-4').count() !== 0 || await expanded('TITLE-4')) await fail('An undone row must return folded and retire its notice');
}
