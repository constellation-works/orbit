// Usage: node dashboard_operations_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const test = fileURLToPath(new URL('./dashboard_operations.mjs', import.meta.url));
const cronZoneTest = fileURLToPath(new URL('./dashboard_operations_cron_zone.mjs', import.meta.url));
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = name === '/test.mjs' ? { data: fs.readFileSync(test), type: 'text/javascript' }
    : name === '/cron-zone.mjs' ? { data: fs.readFileSync(cronZoneTest), type: 'text/javascript' }
    : dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const pageOverflow = () => document.scrollingElement.scrollWidth > window.innerWidth;
const assertNoOverflow = async (label) => {
  const overflow = await page.evaluate(pageOverflow);
  if (overflow) {
    const width = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, inner: window.innerWidth }));
    throw new Error(`Horizontal overflow at ${label}: scrollWidth=${width.scroll} innerWidth=${width.inner}`);
  }
};
let browser;
let page;
try {
  browser = await chromium.launch({headless:true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined});
  // A fixed zone away from UTC, so local times cannot pass as UTC.
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, timezoneId: 'America/Los_Angeles' });
  const sharedDrainDeadline = new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString();
  await page.addInitScript({ content: `window.__drainDeadline = ${JSON.stringify(sharedDrainDeadline)};` });
  // Serve the actual markup/styles with only the Operations module initialized.
  // All API traffic is fixture data; no live scheduler or dashboard is contacted.
  const pageErrors = [];
  page.on('pageerror', error => { pageErrors.push(String(error)); console.error(error); });
  await page.goto(`http://127.0.0.1:${server.address().port}/#operations/routines`);
  await page.addScriptTag({ type: 'module', url: '/test.mjs' });
  await page.waitForFunction(() => globalThis.operationsTestsPassed, undefined, { timeout: 15000 });
  await page.evaluate(() => {
    const workspace = document.getElementById('rail-workspace');
    if (workspace && !document.getElementById('workspace-select')) {
      workspace.innerHTML = '<select class="workspace-select" id="workspace-select" aria-label="Workspace"><option selected>ws_orbit</option></select>';
      const note = document.createElement('span');
      note.id = 'workspace-scope-note';
      note.className = 'workspace-scope-note';
      note.textContent = 'Fleet-wide on Reliability';
      note.hidden = true;
      workspace.appendChild(note);
    }
    const tasks = document.getElementById('tasks-body');
    if (tasks) {
      tasks.innerHTML = `
        <div class="row"><span class="id">ORB-11559</span><span class="title">Make Operations compact and usable on narrow dashboard widths</span><span class="status-cell"><select class="task-status-select"><option>in-progress</option></select></span><span class="crew-cell"><select class="task-crew-select"><option>grok</option></select></span></div>
        <div class="row"><span class="id">ORB-00001-with-a-very-long-identifier</span><span class="title">A deliberately long title that must wrap instead of forcing horizontal page scroll</span><span class="status-cell"><select class="task-status-select"><option>blocked</option></select></span><span class="crew-cell"><select class="task-crew-select"><option>claude</option></select></span></div>`;
    }
  });
  await page.evaluate(async () => {
    const { initRouter, initTabs } = await import('/js/router.js');
    const { setDockMode } = await import('/js/log-tail.js');
    let tab = 'operations';
    let diag = 'runs';
    let operations = 'routines';
    let knowledge = 'frictions';
    let config = 'effective';
    let runId = null;
    let runSubtab = 'steps';
    initRouter({
      getTab: () => tab, setTab: (value) => { tab = value; },
      getDiagSubtab: () => diag, setDiagSubtab: (value) => { diag = value; },
      getOperationsSubtab: () => operations, setOperationsSubtab: (value) => { operations = value; },
      getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: (value) => { knowledge = value; },
      getConfigSubtab: () => config, setConfigSubtab: (value) => { config = value; },
      getRunId: () => runId, setRunId: (value) => { runId = value; },
      getRunSubtab: () => runSubtab, setRunSubtab: (value) => { runSubtab = value; },
      getRunDetail: () => null, setRunDetail: () => {}, getRunEvents: () => [], setRunEvents: () => {},
      getRunLogs: () => [], setRunLogs: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
      getLastRuns: () => [], refreshDashboard: () => {}, renderDiagnostics: () => {},
      fitLogPanelToViewport: () => {}, showDrainDock: () => setDockMode('drain'),
      getActiveAuditSubtab: () => 'events', setAuditSubtab: () => {}, applyAuditHashQuery: () => {},
      syncAuditControls: () => {}, buildAuditHash: () => '#audit/events',
      setActiveAuditSubtabFromButton: () => {},
    });
    initTabs();
  });
  const viewports = [
    { name: '1440', width: 1440, height: 1000 },
    { name: '672', width: 672, height: 900 },
    { name: '390', width: 390, height: 844 },
    { name: '375x812', width: 375, height: 812 },
  ];

  // ORB-14442: task dock breakpoints must not override Health's router-owned
  // list grid. These checks exercise the shipped CSS and router at the widths
  // where the shared tasks-layout rules used to leave an empty column.
  const diagnosticLayout = async (subtab, width) => {
    await page.setViewportSize({ width, height: 730 });
    await page.click('.tab[data-tab="diagnostics"]');
    await page.click(`#diag-subtabs .subtab[data-subtab="${subtab}"]`);
    const layout = await page.evaluate(() => {
      const main = document.getElementById('diagnostics-main');
      const panel = document.getElementById('diagnostics-panel');
      const side = document.getElementById('diagnostics-side-col');
      const style = getComputedStyle(main);
      const mainBox = main.getBoundingClientRect();
      const panelBox = panel.getBoundingClientRect();
      const paddingLeft = parseFloat(style.paddingLeft);
      const contentWidth = mainBox.width - paddingLeft - parseFloat(style.paddingRight);
      return {
        tracks: getComputedStyle(main).gridTemplateColumns.trim().split(/\s+/).length,
        contentWidth,
        panelWidth: panelBox.width,
        panelLeft: panelBox.left,
        contentLeft: mainBox.left + paddingLeft,
        sideVisible: getComputedStyle(side).display !== 'none' && side.getBoundingClientRect().width > 0,
      };
    });
    if (layout.tracks !== 1 || Math.abs(layout.panelWidth - layout.contentWidth) > 1
      || Math.abs(layout.panelLeft - layout.contentLeft) > 1 || layout.sideVisible) {
      throw new Error(`Health ${subtab} should fill one column at ${width}px: ${JSON.stringify(layout)}`);
    }
  };
  for (const width of [1073, 1250]) {
    for (const subtab of ['incidents', 'errors']) await diagnosticLayout(subtab, width);
    await page.click('#diag-subtabs .subtab[data-subtab="metrics"]');
    const metrics = await page.evaluate(() => {
      const main = document.getElementById('diagnostics-main');
      const side = document.getElementById('diagnostics-side-col');
      return {
        tracks: getComputedStyle(main).gridTemplateColumns.trim().split(/\s+/).length,
        sideVisible: getComputedStyle(side).display !== 'none' && side.getBoundingClientRect().width > 0,
      };
    });
    if (metrics.tracks !== 2 || !metrics.sideVisible) {
      throw new Error(`Health Metrics summary should remain beside the list at ${width}px: ${JSON.stringify(metrics)}`);
    }
  }

  await page.setViewportSize({ width: 1073, height: 730 });
  await page.click('.tab[data-tab="audit"]');
  const audit = await page.evaluate(() => {
    const main = document.querySelector('.tab-pane[data-tab="audit"] > main');
    const first = document.getElementById('audit-pane').getBoundingClientRect();
    const second = document.getElementById('audit-summary-panel').getBoundingClientRect();
    return {
      tracks: getComputedStyle(main).gridTemplateColumns.trim().split(/\s+/).length,
      sideBySide: first.width > 0 && second.width > 0 && first.right <= second.left,
    };
  });
  if (audit.tracks !== 2 || !audit.sideBySide) throw new Error(`Audit columns should remain side by side at 1073px: ${JSON.stringify(audit)}`);

  await page.click('.tab[data-tab="tasks"]');
  for (const width of [1251, 1250, 1073, 901, 900]) {
    await page.setViewportSize({ width, height: 730 });
    const tasks = await page.evaluate(() => {
      const main = document.querySelector('.tab-pane[data-tab="tasks"] > main.tasks-layout');
      const list = document.getElementById('tasks-panel').getBoundingClientRect();
      const dock = document.getElementById('side-dock').getBoundingClientRect();
      const style = getComputedStyle(main);
      const contentWidth = main.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
      const available = contentWidth - parseFloat(style.columnGap || '0');
      const expectedDock = window.innerWidth <= 900
        ? contentWidth
        : window.innerWidth > 1250
          ? Math.min(720, Math.max(336, contentWidth * 0.32))
          : Math.max(280, available * 1.15 / 3.15);
      return {
        tracks: style.gridTemplateColumns.trim().split(/\s+/).length,
        listWidth: list.width,
        dockWidth: dock.width,
        expectedDock,
        mainWidth: contentWidth,
        available,
      };
    });
    const expectedTracks = width > 900 ? 2 : 1;
    if (tasks.tracks !== expectedTracks) throw new Error(`Tasks grid has ${tasks.tracks} tracks at ${width}px, expected ${expectedTracks}: ${JSON.stringify(tasks)}`);
    if (Math.abs(tasks.dockWidth - tasks.expectedDock) > 2) {
      throw new Error(`Tasks dock width changed at ${width}px: ${JSON.stringify(tasks)}`);
    }
    if (width === 900 && Math.abs(tasks.listWidth - tasks.mainWidth) > 1) {
      throw new Error(`Tasks layout should stack at 900px: ${JSON.stringify(tasks)}`);
    }
    if (width > 900 && Math.abs(tasks.listWidth + tasks.dockWidth - tasks.available) > 2) {
      throw new Error(`Tasks list and dock should fill both columns at ${width}px: ${JSON.stringify(tasks)}`);
    }
  }
  await page.click('.tab[data-tab="operations"]');

  for (const viewport of viewports) {
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    await page.evaluate(() => {
      document.querySelectorAll('.operation-details').forEach((node) => { node.open = false; });
    });
    for (const tab of ['routines', 'auto-tasks', 'jobs']) {
      const selector = `#operations-subtabs .subtab[data-subtab="${tab}"]`;
      await page.click(selector);
      await page.waitForTimeout(200);
      const reachable = await page.evaluate(([name, selector]) => {
        const button = document.querySelector(selector);
        const panel = document.getElementById(`operations-${name}-main`);
        const workspace = document.getElementById('workspace-select');
        const bounds = (node) => node.getBoundingClientRect();
        const onscreen = (node) => {
          const box = bounds(node);
          return box.width > 0 && box.height > 0 && box.right > 0 && box.left < window.innerWidth;
        };
        const clipped = Array.from(panel.querySelectorAll('button, select, .operation-row-head, .operation-clock-summary')).some((node) => {
          const box = bounds(node);
          return box.right > window.innerWidth + 1;
        });
        return {
          subtab: onscreen(button),
          panel: panel && !panel.hidden,
          workspace: workspace && onscreen(workspace),
          clipped,
        };
      }, [tab, selector]);
      if (!reachable.subtab) throw new Error(`${tab} subtab not reachable at ${viewport.name}`);
      if (!reachable.panel) throw new Error(`${tab} panel hidden at ${viewport.name}`);
      if (!reachable.workspace) throw new Error(`workspace selector not reachable at ${viewport.name} / ${tab}`);
      if (reachable.clipped) throw new Error(`Clipped Operations control at ${viewport.name} / ${tab}`);
      await assertNoOverflow(`${viewport.name} / ${tab}`);
      await page.screenshot({ path: path.join(evidence, `${tab}-${viewport.name}.png`), fullPage: true });
    }
  }

  // ORB-12898: the retired #auto-drain destination opens Tasks with the Drain
  // dock, and the card fits the dock at its 336px minimum (and the narrower
  // mid-width column) with no horizontal scroll.
  await page.evaluate(async () => {
    const { setDockMode } = await import('/js/log-tail.js');
    const { setActiveTab } = await import('/js/router.js');
    setDockMode('log');
    setActiveTab('auto-drain');
  });
  await page.waitForFunction(() => location.hash.startsWith('#tasks'));
  const drainCheck = async (label, dockWidth) => {
    const result = await page.evaluate((width) => {
      const layout = document.querySelector('main.tasks-layout');
      if (width) layout.style.setProperty('--dock-w', `${width}px`); else layout.style.removeProperty('--dock-w');
      const dock = document.getElementById('side-dock');
      const card = document.getElementById('auto-drain-panel');
      const cardBox = card.getBoundingClientRect();
      const overflowing = Array.from(card.querySelectorAll('*'))
        .filter((node) => node.getClientRects().length > 0 && node.getBoundingClientRect().right > cardBox.right + 1)
        .map((node) => node.className || node.tagName);
      return {
        mode: dock.dataset.mode,
        dockWidth: Math.round(dock.getBoundingClientRect().width),
        cardVisible: cardBox.height > 0,
        firstPanel: dock.querySelector('.dock-pane[data-pane="drain"] > .panel')?.id,
        scroll: card.scrollWidth > card.clientWidth + 1,
        overflowing,
        durations: card.querySelectorAll('.drain-duration[aria-pressed]').length,
        text: card.textContent,
      };
    }, dockWidth);
    if (result.mode !== 'drain' || !result.cardVisible) throw new Error(`#auto-drain did not open the Drain dock at ${label}: ${JSON.stringify(result)}`);
    if (result.firstPanel !== 'auto-drain-panel') throw new Error(`auto-drain card is not the first dock card at ${label}: ${result.firstPanel}`);
    if (result.scroll || result.overflowing.length) throw new Error(`Drain card overflows at ${label} (dock ${result.dockWidth}px): ${result.overflowing}`);
    if (result.durations !== 6 || !result.text.includes('Pool: blocked')) throw new Error(`Drain card incomplete at ${label}: ${result.text}`);
    await page.screenshot({ path: path.join(evidence, `drain-${label}.png`), fullPage: true });
    return result.dockWidth;
  };
  await page.setViewportSize({ width: 1440, height: 1000 });
  const minimum = await drainCheck('1440-dock336', 336);
  if (minimum !== 336) throw new Error(`dock did not sit at its 336px minimum: ${minimum}`);
  for (const [phase, label] of [['draining', 'Draining'], ['winding_down', 'Winding down'], ['idle', 'idle']]) {
    await page.evaluate(state => globalThis.setDrainFixturePhase(state), phase);
    const rendered = await page.evaluate(() => ({
      card: document.getElementById('auto-drain-panel').dataset.drainState,
      header: document.getElementById('auto-drain-live').textContent,
      global: document.getElementById('global-drain-state').textContent,
      globalHidden: document.getElementById('global-drain-state').hidden,
      tab: document.getElementById('dock-drain-state').textContent,
      tabLabel: document.getElementById('dock-tab-drain').getAttribute('aria-label'),
      status: document.getElementById('auto-drain-operation-feedback').textContent,
    }));
    if (rendered.card !== phase || !rendered.header.includes(label)) throw new Error(`Drain ${phase} header: ${JSON.stringify(rendered)}`);
    if (phase === 'draining' && (!rendered.header.includes('left') || !rendered.header.includes('jrun-'))) throw new Error(`Missing server deadline or run link: ${rendered.header}`);
    if (phase !== 'idle' && !rendered.header.includes('This window: 1 running of 3 admitted')) throw new Error(`Missing scoped window count: ${rendered.header}`);
    if (phase === 'idle' ? !rendered.globalHidden || rendered.tab : rendered.globalHidden || !rendered.global.includes(label) || !rendered.tabLabel.includes(label)) throw new Error(`Drain ${phase} indicators: ${JSON.stringify(rendered)}`);
    if (phase === 'draining' && rendered.tab) throw new Error(`ORB-14489: Drain tab repeats draining state: ${rendered.tab}`);
    if (!rendered.status.includes(label)) throw new Error(`Drain ${phase} status announcement: ${rendered.status}`);
    await drainCheck(`state-${phase}`, 336);
  }

  // ORB-14566: changing concurrency alters the readiness URL but must keep
  // the panel scope stable while that refresh is pending, then restore focus.
  await page.evaluate(() => {
    const input = document.getElementById('auto-drain-concurrency');
    input.focus();
    input.value = '6';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    globalThis.focusedDrainInput = input;
    globalThis.startPendingDrainReadinessRefresh();
  });
  await page.waitForFunction(() => globalThis.drainReadinessRequestPending());
  const pendingDrainRefresh = await page.evaluate(() => {
    const body = document.getElementById('auto-drain-body');
    const input = globalThis.focusedDrainInput;
    return {
      bodyBusy: body.getAttribute('aria-busy'),
      inputConnected: input.isConnected,
      inputFocused: document.activeElement === input,
      loading: [...body.querySelectorAll('[data-panel-status]')].some(note => note.textContent === 'Loading…'),
      liveCount: document.getElementById('auto-drain-live').textContent,
      requestedConcurrency: globalThis.drainReadinessConcurrency(),
    };
  });
  if (pendingDrainRefresh.bodyBusy !== 'true' || !pendingDrainRefresh.inputConnected
    || !pendingDrainRefresh.inputFocused || pendingDrainRefresh.loading
    || pendingDrainRefresh.liveCount === '—' || pendingDrainRefresh.requestedConcurrency !== '6') {
    throw new Error(`Concurrency refresh reset the Drain card or lost focus while pending: ${JSON.stringify(pendingDrainRefresh)}`);
  }
  await page.evaluate(() => globalThis.releasePendingDrainReadinessRefresh());
  const completedDrainRefresh = await page.evaluate(() => ({
    inputFocused: document.activeElement?.id === 'auto-drain-concurrency',
    inputValue: document.getElementById('auto-drain-concurrency')?.value,
    liveCount: document.getElementById('auto-drain-live').textContent,
  }));
  if (!completedDrainRefresh.inputFocused || completedDrainRefresh.inputValue !== '6'
    || completedDrainRefresh.liveCount === '—') {
    throw new Error(`Concurrency refresh did not restore focus and updated state: ${JSON.stringify(completedDrainRefresh)}`);
  }

  const timeLeft = async target => {
    await target.evaluate(() => globalThis.setDrainFixturePhase('draining'));
    return target.locator('#auto-drain-live').textContent();
  };
  const firstBrowser = await timeLeft(page);
  const otherPage = await browser.newPage({ timezoneId: 'America/Los_Angeles' });
  await otherPage.addInitScript({ content: `window.__drainDeadline = ${JSON.stringify(sharedDrainDeadline)};` });
  await otherPage.goto(`http://127.0.0.1:${server.address().port}/`);
  await otherPage.addScriptTag({ type: 'module', url: '/test.mjs' });
  await otherPage.waitForFunction(() => globalThis.operationsTestsPassed);
  const secondBrowser = await timeLeft(otherPage);
  await otherPage.reload();
  await otherPage.addScriptTag({ type: 'module', url: '/test.mjs' });
  await otherPage.waitForFunction(() => globalThis.operationsTestsPassed);
  const afterDrainReload = await timeLeft(otherPage);
  for (const [name, value] of [['second browser', secondBrowser], ['reload', afterDrainReload]]) {
    if (!value.includes('left') || !value.includes('jrun-') || !/\d+h \d{2}m left/.test(value)) throw new Error(`Drain deadline missing after ${name}: ${value}`);
    if (value !== firstBrowser) throw new Error(`Drain deadline differs in ${name}: ${value} vs ${firstBrowser}`);
  }
  await otherPage.close();
  await page.evaluate(() => globalThis.setDrainFixturePhase('draining'));

  // ORB-14489: render the live throttle case with spare workspace capacity.
  // A text check alone would pass for clipped IDs, so also measure each link
  // against its row and inspect the clipping styles at the reported width.
  await page.setViewportSize({ width: 1024, height: 900 });
  await page.evaluate(() => globalThis.setDrainFixtureReadiness({ capacity: {
    active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 0,
    occupancy: { runs: [{ task_ids: ['ORB-14488'], phase: 'post_implementation' }] },
    resource_throttle: { resources: [{ resource: 'cpu', percent: 164, high_percent: 90, resume_percent: 75, since: '2026-10-04T08:40:00Z' }] },
  } }));
  await drainCheck('1024-throttled-blockers', null);
  const drainEvidence = await page.evaluate(() => {
    const card = document.getElementById('auto-drain-panel');
    const row = card.querySelector('.drain-blocked-row');
    const who = row.querySelector('.drain-blocked-who');
    const lock = row.querySelector('.drain-blocked-lock');
    const bounds = row.getBoundingClientRect();
    const links = [...who.querySelectorAll('a')].map(link => {
      const box = link.getBoundingClientRect();
      return { text: link.textContent, visible: box.width > 0 && box.left >= bounds.left && box.right <= bounds.right + 1 };
    });
    return {
      summary: card.querySelector('.drain-slots').textContent,
      workspace: card.querySelector('.drain-capacity-count').textContent,
      window: card.querySelector('.drain-window-count').textContent,
      pool: [...card.querySelectorAll('.drain-stat-label')].map(node => node.textContent),
      tab: document.getElementById('dock-tab-drain').textContent.trim(),
      blocker: who.textContent, links,
      idsClipped: getComputedStyle(who).overflow === 'hidden' || who.scrollWidth > who.clientWidth + 1,
      lockTruncated: lock.scrollWidth > lock.clientWidth && getComputedStyle(lock).textOverflow === 'ellipsis',
      stop: card.querySelector('.drain-stop-note:not([hidden])').textContent,
    };
  });
  if (!/throttle.*cpu/.test(drainEvidence.summary) || /finish|must clear/.test(drainEvidence.summary)) throw new Error(`Throttle blamed on running tasks: ${drainEvidence.summary}`);
  if (!drainEvidence.workspace.includes('Workspace: 6 of 12') || !drainEvidence.window.includes('This window: 1 running of 3') || !drainEvidence.pool.every(label => label.startsWith('Pool:'))) throw new Error(`Unscoped Drain counts: ${JSON.stringify(drainEvidence)}`);
  if (drainEvidence.blocker !== 'ORB-14334 waits on ORB-14488' || drainEvidence.links.length !== 2 || drainEvidence.links.some(link => !link.visible) || drainEvidence.idsClipped || !drainEvidence.lockTruncated) throw new Error(`ORB-14489: blocker IDs must stay whole while the lock path truncates at 1024px: ${JSON.stringify(drainEvidence)}`);
  if (drainEvidence.tab !== 'Drain') throw new Error(`Duplicated Drain tab state: ${drainEvidence.tab}`);
  if (!/Stop starting new tasks/.test(drainEvidence.stop) || !/keep running/.test(drainEvidence.stop) || /settlements/.test(drainEvidence.stop)) throw new Error(`Stop help must explain the operator's action: ${drainEvidence.stop}`);
  fs.writeFileSync(path.join(evidence, 'drain-readiness-1024.json'), `${JSON.stringify(drainEvidence, null, 2)}\n`);
  await page.evaluate(() => globalThis.setDrainFixtureReadiness());
  await page.evaluate(async () => { const { setDockMode } = await import('/js/log-tail.js'); setDockMode('log'); });
  const logDrainState = await page.locator('#dock-tab-drain').getAttribute('aria-label');
  if (!logDrainState.includes('Draining')) throw new Error(`Drain tab loses its accessible live state while Log is selected: ${logDrainState}`);
  await page.click('.tab[data-tab="runs"]');
  await page.click('#global-drain-state');
  if (!page.url().includes('#tasks') || await page.locator('#side-dock').getAttribute('data-mode') !== 'drain') throw new Error('Global Drain indicator did not open the card');
  await page.emulateMedia({ reducedMotion: 'reduce' });
  for (const selector of ['#auto-drain-dot', '#global-drain-state .drain-dot']) {
    const animation = await page.locator(selector).evaluate(node => getComputedStyle(node).animationName);
    if (animation !== 'none') throw new Error(`Reduced-motion drain indicator ${selector} animates: ${animation}`);
  }
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await page.setViewportSize({ width: 900, height: 900 });
  await drainCheck('900', null);
  await page.setViewportSize({ width: 375, height: 812 });
  await drainCheck('375x812', null);
  // The approve-proposed opt-in and the live window's approvals line fit the
  // card with no overflow, and the card is captured on its own for review.
  const approveCard = async (label) => {
    await page.evaluate(async () => {
      await globalThis.setDrainFixturePhase('idle');
      document.querySelector('input[name="auto-drain-approve"][value="approve"]').click();
    });
    const form = await page.evaluate(() => {
      const card = document.getElementById('auto-drain-panel');
      const box = card.getBoundingClientRect();
      const label = card.querySelector('.drain-field-approve');
      return {
        selected: card.querySelector('input[name="auto-drain-approve"][value="approve"]').checked,
        overflowing: Array.from(card.querySelectorAll('*')).filter((node) => node.getClientRects().length > 0 && node.getBoundingClientRect().right > box.right + 1).map((node) => node.className || node.tagName),
        reachable: label.getBoundingClientRect().width > 0,
      };
    });
    if (!form.selected || !form.reachable || form.overflowing.length) throw new Error(`Approve-proposed control at ${label}: ${JSON.stringify(form)}`);
    await page.waitForTimeout(400);
    await page.locator('#auto-drain-panel').screenshot({ path: path.join(evidence, `drain-approve-form-${label}.png`) });
    await page.evaluate(async () => {
      await globalThis.setDrainFixturePhase('draining');
      await globalThis.setDrainFixtureApprovals({ enabled: true, approved_total: 3, held_total: 2, held_by_reason: { missing_complexity: 1, pilot_held: 1 }, held: [{ task_id: 'ORB-8', reason: 'missing_complexity' }, { task_id: 'ORB-9', reason: 'pilot_held' }] });
    });
    const live = await page.evaluate(() => {
      const card = document.getElementById('auto-drain-panel');
      const box = card.getBoundingClientRect();
      return {
        text: card.querySelector('.drain-approvals')?.textContent,
        overflowing: Array.from(card.querySelectorAll('*')).filter((node) => node.getClientRects().length > 0 && node.getBoundingClientRect().right > box.right + 1).map((node) => node.className || node.tagName),
      };
    });
    if (live.text !== 'Approving proposed tasks · 3 approved · 2 held' || live.overflowing.length) throw new Error(`Live approvals line at ${label}: ${JSON.stringify(live)}`);
    await page.waitForTimeout(400);
    await page.locator('#auto-drain-panel').screenshot({ path: path.join(evidence, `drain-approve-live-${label}.png`) });
    await page.evaluate(async () => {
      await globalThis.setDrainFixtureApprovals({ enabled: false });
      document.querySelector('input[name="auto-drain-approve"][value="leave"]')?.click();
      await globalThis.setDrainFixturePhase('draining');
    });
  };
  await approveCard('375x812');
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.evaluate(() => document.querySelector('main.tasks-layout').style.setProperty('--dock-w', '336px'));
  await approveCard('1440-dock336');
  await page.evaluate(() => document.querySelector('main.tasks-layout').style.removeProperty('--dock-w'));
  await page.setViewportSize({ width: 375, height: 812 });
  await page.evaluate(() => document.querySelector('main.tasks-layout').style.removeProperty('--dock-w'));

  // The Log dock toolbar keeps every control reachable at the 280px (<=1250px
  // viewport) and 336px dock minimums, while following and while paused with
  // the buffered action shown. Pausing and buffering go through the real
  // follow handler and SSE consumer, fed by a fixture stream.
  await page.evaluate(async () => {
    const fixtureFetch = globalThis.fetch;
    // The first snapshot fails (dashboard restart / 5xx). Later attempts
    // succeed only after the test releases the server, so the retry and the
    // disconnected state are observable before any EventSource exists.
    let logSnapshotAttempts = 0;
    let releaseSnapshot = false;
    globalThis.logSnapshotAttempts = () => logSnapshotAttempts;
    globalThis.releaseLogSnapshot = () => { releaseSnapshot = true; };
    globalThis.fetch = (url, options) => {
      const pathname = new URL(url, location.href).pathname;
      if (pathname !== '/api/log') return fixtureFetch(url, options);
      logSnapshotAttempts += 1;
      if (!releaseSnapshot) {
        return Promise.resolve({
          ok: false,
          status: 503,
          text: async () => JSON.stringify({ error: 'log snapshot unavailable' }),
        });
      }
      return Promise.resolve({
        ok: true,
        status: 200,
        json: async () => ({
          events: [{
            ts: '2026-09-27T12:00:00Z',
            level: 'info',
            code: 'OK',
            source: 'orbit.log',
            message_html: 'snapshot recovered',
          }],
          offset: 42,
        }),
      });
    };
    globalThis.EventSource = class FixtureStream {
      static CLOSED = 2;
      constructor(url) {
        globalThis.logFixtureStream = this;
        globalThis.logFixtureUrl = String(url);
        this.readyState = 1;
        queueMicrotask(() => {
          if (this.readyState === FixtureStream.CLOSED) return;
          if (typeof this.onopen === 'function') this.onopen();
        });
      }
      close() { this.readyState = FixtureStream.CLOSED; }
    };
    const { initLogTail, setDockMode } = await import('/js/log-tail.js');
    initLogTail();
    setDockMode('log');
  });
  await page.waitForFunction(() => {
    const bar = document.getElementById('log-statusbar');
    const box = bar ? bar.getBoundingClientRect() : { width: 0, height: 0 };
    const label = bar?.querySelector('.sb-label')?.textContent;
    return box.width > 0 && box.height > 0
      && bar.classList.contains('disconnected')
      && document.getElementById('side-dock')?.classList.contains('disconnected')
      && label === 'log stream unavailable, retrying'
      && globalThis.logSnapshotAttempts() >= 1
      && !globalThis.logFixtureStream;
  });
  // Hiding the tab must cancel the snapshot backoff. A background tab neither
  // retries nor opens a stream; showing it resumes, and only the recovered
  // snapshot may open the EventSource.
  const attemptsAtFailure = await page.evaluate(() => {
    let hidden = true;
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden });
    document.dispatchEvent(new Event('visibilitychange'));
    globalThis.__setLogHidden = (value) => { hidden = value; };
    return globalThis.logSnapshotAttempts();
  });
  await page.waitForTimeout(1300);
  const whileHidden = await page.evaluate(() => ({
    attempts: globalThis.logSnapshotAttempts(),
    stream: Boolean(globalThis.logFixtureStream),
  }));
  if (whileHidden.attempts !== attemptsAtFailure || whileHidden.stream) {
    throw new Error(`hidden tab retried the snapshot or opened a stream: ${JSON.stringify(whileHidden)} after ${attemptsAtFailure}`);
  }
  await page.evaluate(() => {
    globalThis.__setLogHidden(false);
    document.dispatchEvent(new Event('visibilitychange'));
    delete document.hidden;
    globalThis.releaseLogSnapshot();
  });
  await page.waitForFunction(() => {
    const bar = document.getElementById('log-statusbar');
    const dock = document.getElementById('side-dock');
    const stream = globalThis.logFixtureStream;
    return stream
      && stream.readyState === 1
      && typeof stream.onmessage === 'function'
      && bar
      && !bar.classList.contains('disconnected')
      && dock
      && !dock.classList.contains('disconnected')
      && bar.getAttribute('aria-label') === 'Latest log line';
  }, undefined, { timeout: 20000 });
  const recovered = await page.evaluate(() => ({
    attempts: globalThis.logSnapshotAttempts(),
    url: globalThis.logFixtureUrl,
    text: document.getElementById('logInner')?.textContent || '',
  }));
  if (recovered.attempts <= attemptsAtFailure) {
    throw new Error(`snapshot was not retried after the first failure: ${recovered.attempts} (at failure ${attemptsAtFailure})`);
  }
  if (!String(recovered.url).includes('from=42')) {
    throw new Error(`stream did not resume from the recovered snapshot offset: ${recovered.url}`);
  }
  if (!recovered.text.includes('snapshot recovered')) {
    throw new Error(`recovered snapshot was not rendered: ${recovered.text}`);
  }
  const logToolbarCheck = async (label, viewport, dockWidth, paused) => {
    await page.setViewportSize(viewport);
    // The dashboard re-applies the saved dock width (or clears --dock-w when
    // none is saved) from a window resize listener, and Chromium delivers that
    // event on the next rendered frame. Let it fire before forcing this
    // fixture's width, or it clears the width mid-measurement and the dock
    // falls back to its 32% default (384px at 1440).
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.evaluate((width) => document.querySelector('main.tasks-layout').style.setProperty('--dock-w', `${width}px`), dockWidth);
    const ids = ['all', 'err', 'deny', 'warn'].map(filter => `.log-filters .filter-pill[data-filter="${filter}"]`)
      .concat(['#log-show-agent', '#log-follow-tail', '#log-wrap-lines'], paused ? ['#log-buffered-count'] : []);
    const layout = await page.evaluate((selectors) => {
      const bar = document.querySelector('#side-dock .log-filters');
      const barBox = bar.getBoundingClientRect();
      const unreachable = selectors.filter((selector) => {
        const box = document.querySelector(selector).getBoundingClientRect();
        return box.width === 0 || box.height === 0 || box.left < barBox.left - 1 || box.right > barBox.right + 1
          || box.top < barBox.top - 1 || box.bottom > barBox.bottom + 1;
      });
      return { dockWidth: Math.round(document.getElementById('side-dock').getBoundingClientRect().width), clipped: bar.scrollWidth > bar.clientWidth + 1, unreachable };
    }, ids);
    if (layout.dockWidth !== dockWidth) throw new Error(`Log dock is not ${dockWidth}px at ${label}: ${layout.dockWidth}`);
    if (layout.clipped || layout.unreachable.length) throw new Error(`Log toolbar clips controls at ${label}: ${JSON.stringify(layout)}`);
    for (const selector of ids) {
      await page.focus(selector);
      const focused = await page.evaluate(sel => document.activeElement === document.querySelector(sel), selector);
      if (!focused) throw new Error(`Log toolbar control ${selector} not keyboard focusable at ${label}`);
      // The trial click fails if another element covers the control's centre.
      await page.click(selector, { trial: true, timeout: 2000 });
    }
    await page.screenshot({ path: path.join(evidence, `log-toolbar-${label}.png`), fullPage: true });
  };
  const logToolbarSizes = [
    ['1024-dock280', { width: 1024, height: 900 }, 280],
    ['1440-dock336', { width: 1440, height: 1000 }, 336],
  ];
  for (const [label, viewport, width] of logToolbarSizes) await logToolbarCheck(`${label}-following`, viewport, width, false);
  await page.click('#log-follow-tail');
  await page.evaluate(() => {
    for (let id = 1; id <= 1123; id += 1) {
      globalThis.logFixtureStream.onmessage({ lastEventId: String(id), data: JSON.stringify({ ts: '2026-09-27T12:00:00Z', level: 'info', code: 'fixture', msg: `event ${id}` }) });
    }
  });
  const buffered = await page.locator('#log-buffered-count').textContent();
  if (!/buffered · \d+ discarded/.test(buffered)) throw new Error(`Paused SSE events did not show the buffered action: ${buffered}`);
  for (const [label, viewport, width] of logToolbarSizes) await logToolbarCheck(`${label}-paused`, viewport, width, true);
  await page.click('#log-buffered-count');
  const resumed = await page.evaluate(() => getComputedStyle(document.getElementById('log-buffered-count')).display);
  if (resumed !== 'none') throw new Error('Clicking the buffered action did not flush the paused buffer');
  await page.evaluate(() => document.querySelector('main.tasks-layout').style.removeProperty('--dock-w'));

  await page.setViewportSize({ width: 375, height: 812 });
  await page.click('.tab[data-tab="tasks"]');
  await page.waitForTimeout(200);
  const taskLayout = await page.evaluate(() => {
    const row = document.querySelector('#tasks-body .row');
    const list = document.getElementById('tasks-panel');
    const style = row ? getComputedStyle(row) : null;
    return {
      listWidth: list?.getBoundingClientRect().width || 0,
      inner: window.innerWidth,
      areas: style?.gridTemplateAreas || '',
      columns: style?.gridTemplateColumns || '',
    };
  });
  // 216px rail would leave ~159px. Main padding is 20px on each side, so a
  // full-width list in a 375px viewport is about 335px.
  if (taskLayout.listWidth < taskLayout.inner - 80) {
    throw new Error(`task list is not full-width at 375px: ${taskLayout.listWidth} vs ${taskLayout.inner}`);
  }
  if (!taskLayout.areas.includes('id title') || !taskLayout.areas.includes('status crew')) {
    throw new Error(`375px task row must use the 520px two-row areas, got ${JSON.stringify(taskLayout)}`);
  }
  await assertNoOverflow('375x812 / tasks');
  await page.screenshot({ path: path.join(evidence, 'tasks-375x812.png'), fullPage: true });

  // Exercise the real shell and hash router at phone width. The content is
  // fixture data; navigation, scrolling, keyboard focus and layout are native.
  await page.evaluate(() => {
    for (const [id, value] of Object.entries({
      'rail-count-diag-incidents': '3',
      'rail-count-ops-routines': '4/5',
      'rail-count-ops-auto-tasks': '8/16',
      'rail-count-ops-jobs': '33 running',
    })) document.getElementById(id).textContent = value;
  });
  const mobileEvidence = { routes: [], navigation: [], counts: [] };
  for (const [route, panel] of [
    ['tasks', 'tasks-panel'],
    ['diagnostics/runs', 'diagnostics-panel'],
    ['runs/jrun-dashboard-fixture', 'run-detail-panel'],
    ['diagnostics/incidents', 'diagnostics-panel'],
  ]) {
    await page.evaluate(hash => { location.hash = hash; }, `#${route}`);
    await page.waitForFunction(id => document.getElementById(id).closest('.tab-pane').classList.contains('active'), panel);
    await page.locator(`#${panel}`).evaluate(node => Promise.all(node.closest('.tab-pane').getAnimations().map(animation => animation.finished)));
    const layout = await page.locator(`#${panel}`).evaluate(node => {
      node.closest('.tab-pane').scrollTop = 0;
      const box = node.getBoundingClientRect();
      return { top: box.top, width: box.width, scrollWidth: document.scrollingElement.scrollWidth };
    });
    if (layout.width <= 0 || layout.top < 0 || layout.top > 200) throw new Error(`First panel too low at #${route}: ${JSON.stringify(layout)}`);
    await assertNoOverflow(`375x812 / #${route}`);
    mobileEvidence.routes.push({ route, ...layout });
    await page.screenshot({ path: path.join(evidence, `shell-${route.replaceAll('/', '-')}-375x812.png`), fullPage: true });
  }
  const destinations = await page.locator('.rail .tab').evaluateAll(nodes => nodes.map(node => node.dataset.tab));
  for (const destination of destinations) {
    await page.click(`.rail .tab[data-tab="${destination}"]`);
    const selectors = await page.locator('.rail .tab, .rail-subtabs:not(.dimmed) .subtab').evaluateAll(nodes => nodes.map(node =>
      node.classList.contains('tab') ? `.rail .tab[data-tab="${node.dataset.tab}"]` : `#${node.parentElement.id} .subtab[data-subtab="${node.dataset.subtab}"]`));
    // Tab through every visible entry from the workspace selector. Browser
    // focus must scroll the overflow row without hiding its focus ring.
    await page.focus('#workspace-select');
    for (const selector of selectors) {
      await page.keyboard.press('Tab');
      const focused = await page.locator(selector).evaluate(node => {
        const box = node.getBoundingClientRect();
        const row = document.getElementById('tabs').getBoundingClientRect();
        const style = getComputedStyle(node);
        return {
          active: document.activeElement === node && node.matches(':focus-visible'),
          ring: style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) >= 2,
          onscreen: box.width > 0 && box.height > 0 && box.left >= row.left - 1 && box.right <= row.right + 1,
          sameRow: box.top >= row.top && box.bottom <= row.bottom,
          left: box.left, right: box.right, rowLeft: row.left, rowRight: row.right,
        };
      });
      if (!focused.active || !focused.ring || !focused.onscreen || !focused.sameRow) throw new Error(`Unreachable navigation at ${destination} / ${selector}: ${JSON.stringify(focused)}`);
      await page.click(selector, { trial: true, timeout: 2000 });
    }
    const subtabs = selectors.filter(selector => selector.includes('.subtab'));
    for (const selector of subtabs) {
      await page.click(selector);
      await page.waitForFunction(sel => document.querySelector(sel).classList.contains('active'), selector);
      // Enter must activate the same route through the native button handler.
      await page.focus(selector);
      await page.keyboard.press('Enter');
      const subtab = await page.locator(selector).getAttribute('data-subtab');
      if (!new URL(page.url()).hash.startsWith(`#${destination}/${subtab}`)) throw new Error(`Subtab did not route at ${selector}: ${page.url()}`);
      await assertNoOverflow(`375x812 / ${destination}/${subtab}`);
      const gap = await page.locator(selector).evaluate(node => {
        const count = node.querySelector('.rail-count');
        if (!count?.textContent) return null;
        const label = document.createRange();
        label.selectNodeContents(node.firstChild);
        return count.getBoundingClientRect().left - label.getBoundingClientRect().right;
      });
      if (gap !== null && gap < 6) throw new Error(`Subtab count touches its label at ${selector}: ${gap}px`);
      if (gap !== null) mobileEvidence.counts.push({ selector, gap });
    }
    await assertNoOverflow(`375x812 / ${destination}`);
    mobileEvidence.navigation.push({ destination, controls: selectors.length, subtabs: subtabs.length });
  }
  // The widest readings the chips are sized for, with one resource held.
  const header = await page.evaluate(async () => {
    const { renderHostResources } = await import('/js/host-resources.js');
    renderHostResources({
      cpu: { percent: 1234, severity: 'critical' }, memory: { percent: 100, severity: 'critical' },
      disk: { path: '/workspace', percent: 100, severity: 'critical' }, sample_age_seconds: 1, max_age_seconds: 15,
      stale: false, throttle: true, pressures: [{ resource: 'memory' }], reason: 'memory high', thresholds: { enabled: true },
    });
    const workspace = document.getElementById('workspace-select').getBoundingClientRect();
    const brand = document.querySelector('.rail-brand').getBoundingClientRect();
    const drain = document.getElementById('global-drain-state').getBoundingClientRect();
    const refresh = document.getElementById('refresh-btn').getBoundingClientRect();
    const strip = document.getElementById('host-resource-chips');
    const row = strip.getBoundingClientRect();
    const chips = [...strip.querySelectorAll('.host-resource')].map(node => (
      { resource: node.dataset.resource, box: node.getBoundingClientRect().toJSON(), clipped: node.scrollWidth > node.clientWidth }));
    return {
      actionsInHeader: [brand, drain, refresh].every(box => box.width > 0 && Math.abs(box.top + box.height / 2 - workspace.top - workspace.height / 2) < 1),
      chipsSingleRow: chips.length === 3 && chips.every(({ box }) => box.width > 0 && Math.abs(box.top - chips[0].box.top) < 1),
      // All three readings are in view at 375px without scrolling the row.
      chipsInView: chips.every(({ box, clipped }) => !clipped && box.left >= row.left - 0.5 && box.right <= Math.min(row.right, innerWidth) + 0.5),
      chips,
    };
  });
  if (!header.actionsInHeader || !header.chipsSingleRow || !header.chipsInView) throw new Error(`Phone header or host chips wrapped or clipped: ${JSON.stringify(header)}`);
  await page.screenshot({ path: path.join(evidence, 'topbar-375x812.png') });
  for (const selector of ['#refresh-btn', '#global-drain-state', ...['cpu', 'memory', 'disk'].map(resource => `#host-resource-chips [data-resource="${resource}"]`)]) {
    await page.locator(selector).focus();
    await page.locator(selector).scrollIntoViewIfNeeded();
    await page.locator(selector).click({ trial: true });
  }
  mobileEvidence.header = header;
  fs.writeFileSync(path.join(evidence, 'mobile-shell.json'), `${JSON.stringify(mobileEvidence, null, 2)}\n`);
  console.log(`PASS: phone shell panel positions ${JSON.stringify(mobileEvidence.routes)}; all ${destinations.length} destinations and active rail subtabs reachable; count gaps >=6px; no page overflow.`);

  await page.click('.tab[data-tab="operations"]');
  await page.click('#operations-subtabs .subtab[data-subtab="auto-tasks"]');
  await page.waitForTimeout(150);
  await page.locator('.auto-task-card .operation-details summary').first().focus();
  await page.keyboard.press('Enter');
  const expanded = await page.evaluate(() => document.querySelector('.auto-task-card .operation-details')?.open);
  if (!expanded) throw new Error('keyboard Enter did not expand auto-task details');
  await page.screenshot({ path: path.join(evidence, 'auto-tasks-375x812-expanded.png'), fullPage: true });

  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.addScriptTag({ type: 'module', url: '/test.mjs' });
  await page.waitForFunction(() => globalThis.operationsTestsPassed, undefined, { timeout: 15000 });
  await page.evaluate(async () => {
    const { initRouter, initTabs } = await import('/js/router.js');
    let tab = 'tasks';
    let diag = 'runs';
    let operations = 'routines';
    let knowledge = 'frictions';
    initRouter({
      getTab: () => tab, setTab: (value) => { tab = value; },
      getDiagSubtab: () => diag, setDiagSubtab: (value) => { diag = value; },
      getOperationsSubtab: () => operations, setOperationsSubtab: (value) => { operations = value; },
      getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: (value) => { knowledge = value; },
      getRunId: () => null, setRunId: () => {}, getRunSubtab: () => 'steps', setRunSubtab: () => {},
      getRunDetail: () => null, setRunDetail: () => {}, getRunEvents: () => [], setRunEvents: () => {},
      getRunLogs: () => [], setRunLogs: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
      getLastRuns: () => [], refreshDashboard: () => {}, renderDiagnostics: () => {},
      fitLogPanelToViewport: () => {},
      getActiveAuditSubtab: () => 'events', setAuditSubtab: () => {}, applyAuditHashQuery: () => {},
      syncAuditControls: () => {}, buildAuditHash: () => '#audit/events',
      setActiveAuditSubtabFromButton: () => {},
    });
    initTabs();
  });
  const afterReload = await page.evaluate(() => ({
    hash: location.hash,
    autoTasks: !document.getElementById('operations-auto-tasks-main')?.hidden,
  }));
  if (!afterReload.hash.includes('operations/auto-tasks') || !afterReload.autoTasks) {
    throw new Error(`reload did not restore auto-tasks subtab: ${JSON.stringify(afterReload)}`);
  }
  await page.click('#operations-subtabs .subtab[data-subtab="routines"]');
  await page.waitForFunction(() => location.hash.includes('operations/routines'));
  await page.click('#operations-subtabs .subtab[data-subtab="auto-tasks"]');
  await page.waitForFunction(() => location.hash.includes('operations/auto-tasks'));
  await page.goBack();
  await page.waitForFunction(() => location.hash.includes('operations/routines'));
  const afterBack = await page.evaluate(() => ({
    hash: location.hash,
    routines: !document.getElementById('operations-routines-main')?.hidden,
  }));
  if (!afterBack.hash.includes('operations/routines') || !afterBack.routines) {
    throw new Error(`back did not restore routines: ${JSON.stringify(afterBack)}`);
  }
  await page.goForward();
  await page.waitForFunction(() => location.hash.includes('operations/auto-tasks'));
  const afterForward = await page.evaluate(() => ({
    hash: location.hash,
    autoTasks: !document.getElementById('operations-auto-tasks-main')?.hidden,
  }));
  if (!afterForward.hash.includes('operations/auto-tasks') || !afterForward.autoTasks) {
    throw new Error(`forward did not restore auto-tasks: ${JSON.stringify(afterForward)}`);
  }
  // A browser in UTC, a host cron zone elsewhere: triggers read in the host
  // zone and agree with the UTC next fire beside them.
  const utcPage = await browser.newPage({ viewport: { width: 1440, height: 1000 }, timezoneId: 'UTC' });
  utcPage.on('pageerror', error => { pageErrors.push(String(error)); console.error(error); });
  await utcPage.goto(`http://127.0.0.1:${server.address().port}/#operations/auto-tasks`);
  await utcPage.addScriptTag({ type: 'module', url: '/cron-zone.mjs' });
  await utcPage.waitForFunction(() => globalThis.cronZoneTestsPassed, undefined, { timeout: 15000 });
  await utcPage.screenshot({ path: path.join(evidence, 'cron-zone-utc-browser.png') });
  if (pageErrors.length) throw new Error(`page errors during the scenario: ${pageErrors.join(' | ')}`);
  console.log(`PASS: Chromium Operations fixture; 1440/672/390/375; phone shell, subtabs, Drain dock card at 336/900/375, Log toolbar at dock 280/336 following and paused, reload, history. Screenshots: ${evidence}`);
} finally {
  await browser?.close(); server.close();
}
