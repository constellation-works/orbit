import fs from 'node:fs';
import path from 'node:path';

// Runs inside the full dashboard browser harness. A delivered task links its
// pull request from the review row and from the detail's External refs; a ref
// without an http(s) page stays text in both places.
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
    ].map(task => ({
      ...task, title: `Pull request link ${task.id}`, priority: 'medium', status_transitions: [],
      comments: [], history: [], artifacts: [],
    }));
    const context = {
      getTasks: () => tasks,
      getActiveStatuses: () => new Set(['review', 'in-progress']),
      statusOrder: ['in-progress', 'review'],
      replaceTask: updated => Object.assign(tasks.find(task => task.id === updated.id), updated),
    };
    globalThis.pullRequestLinkFixture = { render: () => renderTasks(tasks, context) };
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
    };
    if (JSON.stringify(rows) !== JSON.stringify(expected)) {
      throw new Error(`Review and in-progress rows must link only an http(s) PR page: ${JSON.stringify(rows)}`);
    }

    const detailRefs = async (id) => {
      await page.locator(`#tasks-body [data-key="task-${id}"] .title`).click();
      const refs = page.locator(`#detail-${id} .external-ref-line`);
      await refs.first().waitFor({ timeout: 5000 });
      return refs.evaluateAll(lines => lines.map(line => {
        const link = line.querySelector('a');
        return { text: line.textContent, href: link ? link.getAttribute('href') : null };
      }));
    };
    const linked = await detailRefs('PRL-1');
    if (JSON.stringify(linked) !== JSON.stringify([{ text: 'PR #4067', href: 'https://github.com/constellation-works/orbit/pull/4067' }])) {
      throw new Error(`The detail must link the delivered pull request: ${JSON.stringify(linked)}`);
    }
    await page.screenshot({ path: path.join(evidence, 'pull-request-links.png') });
    const unlinked = await detailRefs('PRL-3');
    if (JSON.stringify(unlinked) !== JSON.stringify([{ text: 'PR #13', href: null }])) {
      throw new Error(`A ref without an http(s) page must stay text: ${JSON.stringify(unlinked)}`);
    }
    fs.writeFileSync(path.join(evidence, 'pull-request-links-result.json'), JSON.stringify({ passed: true, rows, linked, unlinked }, null, 2));
  } finally {
    await page.evaluate(() => { delete globalThis.pullRequestLinkFixture; });
  }
}
