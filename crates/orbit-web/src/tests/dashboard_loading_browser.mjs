// Usage: node dashboard_loading_browser.mjs /path/to/playwright/index.mjs /evidence/directory
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const assets = fileURLToPath(new URL('../../assets/dashboard/', import.meta.url));
const scenarios = fileURLToPath(new URL('./dashboard_loading.mjs', import.meta.url));
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const file = name === '/test.mjs' ? scenarios : path.join(assets, name === '/' ? 'index.html' : path.basename(name));
  if (!fs.existsSync(file)) { res.writeHead(404); res.end(); return; }
  let data = fs.readFileSync(file);
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', file.endsWith('.html') ? 'text/html' : file.endsWith('.css') ? 'text/css' : 'text/javascript');
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
  await page.evaluate(() => globalThis.showTaskPaginationEvidence());
  await page.waitForFunction(() => document.getElementById('tasks-count').textContent === '1–20 of 55');
  for (const viewport of [{ name: 'desktop', width: 1280 }, { name: 'mobile', width: 390 }]) {
    await page.setViewportSize({ width: viewport.width, height: 900 });
    const pager = page.locator('.task-pagination');
    if (!(await pager.isVisible())) throw new Error(`Task pagination invisible at ${viewport.width}px`);
    if (!(await page.locator('#tasks-next').isEnabled())) throw new Error('First task page must enable Next');
    if (await page.locator('#tasks-previous').isEnabled()) throw new Error('First task page must disable Previous');
    await page.screenshot({ path: path.join(evidence, `task-pagination-${viewport.name}.png`), fullPage: true });
  }
  await page.locator('#tasks-next').click();
  await page.waitForFunction(() => document.getElementById('tasks-count').textContent === '21–40 of 55');
  await page.evaluate(() => window.scrollTo(0, 0));
  if (!(await page.locator('#tasks-previous').isEnabled())) throw new Error('Second task page must enable Previous');
  await page.screenshot({ path: path.join(evidence, 'task-pagination-mobile-page-2.png'), fullPage: true });
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
  await new Promise(resolve => server.close(resolve));
  await page.evaluate(() => {
    globalThis.fetch = globalThis.nativeFetch;
    document.getElementById('refresh-btn').click();
  });
  await page.waitForFunction(() => document.getElementById('meta-text').textContent.includes('offline'));
  if (!(await page.locator('#conn-status').getAttribute('class')).includes('red')) throw new Error('Stopped server must show red connection status');
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, scenarios: 'Task pagination page 1/page 2 and accessible Previous/Next at 1280px and 390px; Tasks, Recent runs, Errors, Operations: cold, stale refresh, scope changes, reordered responses, empty success, network error; Metrics HTTP failure isolation and network offline/recovery' }, null, 2));
  console.log('Chromium dashboard lifecycle and accessible visible feedback passed.');
} finally {
  await browser?.close();
  if (server.listening) await new Promise(resolve => server.close(resolve));
}
