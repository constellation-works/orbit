import fs from 'node:fs';
import path from 'node:path';

// Runs inside the full dashboard browser harness. A delivered task links its
// pull request from the review row and from the detail's delivery card; a ref
// without an http(s) page stays text in the row and in External refs.
export async function assertPullRequestLinks(page, evidence) {
  await page.evaluate(async () => {
    const { setWorkspace } = await import('/js/common.js');
    const { setActiveTab } = await import('/js/router.js');
    const { renderTasks } = await import('/js/tasks.js');
    setWorkspace('one');
    setActiveTab('tasks', { refresh: false });
    const pr = (id, url) => ({ system: 'github-pr', id, ...(url === undefined ? {} : { url }) });
    const tasks = [
      { id: 'PRL-1', status: 'review', external_refs: [pr('4067', 'https://github.com/constellation-works/orbit/pull/4067')] },
      { id: 'PRL-2', status: 'review', external_refs: [pr('12')] },
      { id: 'PRL-3', status: 'review', external_refs: [pr('13', 'javascript:alert(1)')] },
      { id: 'PRL-4', status: 'in-progress', external_refs: [pr('14', 'https://github.com/o/r/pull/14')] },
      { id: 'PRL-5', status: 'proposed' },
      { id: 'PRL-6', status: 'backlog' },
    ].map(task => ({
      ...task, title: `Pull request link ${task.id} ${"Long task title with wrapping content ".repeat(8)}`, priority: 'medium', status_transitions: [],
      comments: [], history: [], artifacts: [],
    }));
    const context = {
      getTasks: () => tasks,
      getActiveStatuses: () => new Set(['proposed', 'review', 'backlog', 'in-progress']),
      statusOrder: ['proposed', 'review', 'backlog', 'in-progress'],
      replaceTask: updated => Object.assign(tasks.find(task => task.id === updated.id), updated),
    };
    globalThis.pullRequestLinkFixture = { priorFetch: globalThis.fetch, render: () => renderTasks(tasks, context) };
    // A refresh during the test (the 30s poll, or the hash change a tab switch
    // makes) reads these rows back instead of replacing them.
    const json = payload => new Response(JSON.stringify(payload), { status: 200 });
    globalThis.fetch = async (url, options) => {
      const { pathname } = new URL(url, window.location.href);
      if (pathname === '/api/tasks') return json({ items: tasks, total: tasks.length, limit: 50, truncated: false });
      const task = tasks.find(task => pathname === `/api/tasks/${task.id}`);
      if (task && !options?.method) return json(task);
      return globalThis.pullRequestLinkFixture.priorFetch(url, options);
    };
    globalThis.pullRequestLinkFixture.render();
  });
  try {
    const rows = await page.evaluate(() => Object.fromEntries(
      [...document.querySelectorAll('#tasks-body .row[data-key^="task-PRL-"]')].map(row => {
        const link = row.querySelector('.task-quick-cell a');
        return [row.dataset.key.slice('task-'.length), link
          ? { text: link.textContent, href: link.getAttribute('href'), target: link.target, rel: link.rel }
          : null];
      }),
    ));
    const expected = {
      'PRL-1': { text: 'PR #4067', href: 'https://github.com/constellation-works/orbit/pull/4067', target: '_blank', rel: 'noopener noreferrer' },
      'PRL-2': null,
      'PRL-3': null,
      'PRL-4': { text: 'PR #14', href: 'https://github.com/o/r/pull/14', target: '_blank', rel: 'noopener noreferrer' },
      'PRL-5': null,
      'PRL-6': null,
    };
    if (JSON.stringify(Object.entries(rows).sort()) !== JSON.stringify(Object.entries(expected).sort())) {
      throw new Error(`Review and in-progress rows must link only an http(s) PR page: ${JSON.stringify(rows)}`);
    }
    const quickActionPair = await page.locator('#tasks-body').getAttribute('data-quick-action-pair');
    if (quickActionPair !== null) {
      throw new Error(`A single linked review action must not set the quick-action pair flag: ${quickActionPair}`);
    }
    const rowActions = await page.evaluate(() => Object.fromEntries(
      ['PRL-1', 'PRL-2', 'PRL-3', 'PRL-4'].map(id => {
        const cell = document.querySelector(`[data-key="task-${id}"] .task-quick-cell`);
        return [id, { approve: Boolean(cell.querySelector('.task-quick.approve')), controls: cell.children.length }];
      }),
    ));
    if (JSON.stringify(rowActions) !== JSON.stringify({
      'PRL-1': { approve: false, controls: 1 },
      'PRL-2': { approve: true, controls: 1 },
      'PRL-3': { approve: true, controls: 1 },
      'PRL-4': { approve: false, controls: 1 },
    })) {
      throw new Error(`Only review rows with a linked PR omit row Approve: ${JSON.stringify(rowActions)}`);
    }

    const layout = [];
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: 1100 });
      const actionTracks = await page.evaluate(() => ['PRL-5', 'PRL-1', 'PRL-6'].map(id => {
        const row = document.querySelector(`[data-key="task-${id}"]`);
        const columns = getComputedStyle(row).gridTemplateColumns.trim().split(/\s+/);
        return { id, width: Number.parseFloat(columns.at(-1)) };
      }));
      // The shared action column is 108px in the wide panel (tasks.css, container
      // min-width 621px) and keeps its 72px compact fallback at 375px.
      const expectedTrack = width === 1440 ? 108 : 72;
      if (actionTracks.some(track => track.width !== expectedTrack)) {
        throw new Error(`Proposed, review and backlog rows keep a ${expectedTrack}px action track at ${width}: ${JSON.stringify(actionTracks)}`);
      }
      const bounds = await page.locator('[data-key="task-PRL-1"]').evaluate(row => {
        const rect = node => { const r = node.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height, right: r.right, bottom: r.bottom }; };
        return { row: rect(row), cell: rect(row.querySelector('.task-quick-cell')), link: rect(row.querySelector('.task-quick-link')), title: rect(row.querySelector('.title')) };
      });
      if (bounds.link.x < bounds.cell.x - 1 || bounds.link.right > bounds.cell.right + 1
        || bounds.link.y < bounds.cell.y - 1 || bounds.link.bottom > bounds.cell.bottom + 1
        || bounds.cell.right > bounds.row.right + 1) {
        throw new Error(`A linked review row keeps its PR link within the action column at ${width}: ${JSON.stringify(bounds)}`);
      }
      layout.push({ width, actionTracks, ...bounds });
      await page.screenshot({ path: path.join(evidence, `review-group-${width === 375 ? 'mobile-375' : 'desktop'}.png`) });
    }
    await page.setViewportSize({ width: 1440, height: 1100 });

    const detailRefs = async (id, selector = '.external-ref-line') => {
      await page.locator(`#tasks-body [data-key="task-${id}"] .title`).click();
      const refs = page.locator(`#detail-${id} ${selector}`);
      await refs.first().waitFor({ timeout: 5000 });
      return refs.evaluateAll(lines => lines.map(line => {
        const link = line.querySelector('a');
        return { text: line.textContent, href: link ? link.getAttribute('href') : null };
      }));
    };
    // The delivery card links the pull request, and External refs does not
    // repeat it.
    const linked = await detailRefs('PRL-1', '.task-delivery-row.pr .value');
    if (JSON.stringify(linked) !== JSON.stringify([{ text: '#4067', href: 'https://github.com/constellation-works/orbit/pull/4067' }])) {
      throw new Error(`The detail must link the delivered pull request: ${JSON.stringify(linked)}`);
    }
    if (await page.locator('#detail-PRL-1 .external-ref-line').count() !== 0) {
      throw new Error('External refs must not repeat the pull request the delivery card links');
    }
    await page.screenshot({ path: path.join(evidence, 'pull-request-links.png') });
    const unlinked = await detailRefs('PRL-3');
    if (JSON.stringify(unlinked) !== JSON.stringify([{ text: 'PR #13', href: null }])) {
      throw new Error(`A ref without an http(s) page must stay text: ${JSON.stringify(unlinked)}`);
    }
    await page.locator('#tasks-body [data-key="task-PRL-1"] .title').click();
    const approve = page.locator('#detail-PRL-1 .actions > .action.approve');
    if (await approve.count() !== 1) throw new Error('A PR-linked review task keeps Approve in its detail');
    await page.evaluate(() => {
      globalThis.pullRequestLinkFixture.approvals = [];
      globalThis.fetch = async (url, options) => {
        const { pathname } = new URL(url, window.location.href);
        if (pathname === '/api/distributed/claims') return new Response(JSON.stringify({ claims: [] }), { status: 200 });
        if (pathname === '/api/tasks/PRL-1/approve' && options?.method === 'POST') {
          globalThis.pullRequestLinkFixture.approvals.push({ pathname, method: options.method });
          return new Response('{}', { status: 200 });
        }
        return globalThis.pullRequestLinkFixture.priorFetch(url, options);
      };
    });
    await approve.evaluate(button => button.click());
    await page.waitForFunction(() => globalThis.pullRequestLinkFixture.approvals.length === 1);
    fs.writeFileSync(path.join(evidence, 'pull-request-links-result.json'), JSON.stringify({ passed: true, rows, rowActions, linked, unlinked, detailApproval: true, layout }, null, 2));
  } finally {
    await page.evaluate(() => { globalThis.fetch = globalThis.pullRequestLinkFixture.priorFetch; delete globalThis.pullRequestLinkFixture; });
  }
}
