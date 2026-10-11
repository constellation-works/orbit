// Usage: node dashboard_task_artifacts_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/task-artifacts-browser
//
// A task detail's artifact previews hold their bytes in blob URLs that only
// teardown revokes. Drive the shipped buildArtifacts with real fetches against
// a fixture server that can hold a response before its headers or inside its
// body, retire the detail the ways the dashboard does (closing it, replacing it
// and changing workspace), and check every blob URL the page created is revoked.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64');
const BIN = Buffer.from('fixture artifact bytes that are not an image or text\n'.repeat(4));
const ARTIFACT_PREFIX = '/api/tasks/ART-1/artifacts/';

// `held-head-*` waits before sending headers; `held-body-*` sends headers and
// the first bytes, then waits before finishing the body.
const held = new Map();
const requested = [];
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname === '/artifacts-harness') {
    res.setHeader('content-type', 'text/html');
    res.end('<!doctype html><link rel="stylesheet" href="/static/dashboard.css">'
      + '<div id="tasks-count"></div><div id="tasks-body"></div><div id="ws-count"></div><div id="ws-body"></div>');
    return;
  }
  if (!url.pathname.startsWith(ARTIFACT_PREFIX)) {
    const served = dashboardFile(url.pathname);
    if (!served) { res.writeHead(404); res.end(); return; }
    res.setHeader('content-type', served.type);
    res.end(served.data);
    return;
  }
  const name = decodeURIComponent(url.pathname.slice(ARTIFACT_PREFIX.length));
  requested.push(name);
  const [bytes, type] = name.endsWith('.png') ? [PNG, 'image/png'] : [BIN, 'application/octet-stream'];
  const headers = { 'content-type': type, 'content-length': bytes.length };
  if (!name.startsWith('held-')) {
    res.writeHead(200, headers);
    res.end(bytes);
    return;
  }
  const entry = { aborted: false };
  res.on('close', () => { if (!res.writableFinished) entry.aborted = true; });
  if (name.startsWith('held-head-')) {
    entry.release = () => { if (!res.destroyed) { res.writeHead(200, headers); res.end(bytes); } };
  } else {
    res.writeHead(200, headers);
    res.write(bytes.subarray(0, 8));
    entry.release = () => { if (!res.destroyed) res.end(bytes.subarray(8)); };
  }
  held.set(name, entry);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const assert = (condition, message) => { if (!condition) throw new Error(message); };
const until = async (label, predicate, timeout = 10000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    if (await predicate()) return;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise(resolve => setTimeout(resolve, 25));
  }
};

const results = [];
const pageErrors = [];
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  page.on('pageerror', error => { pageErrors.push(error.message); console.error(error); });
  // Record every blob URL the page creates and revokes, through the real API.
  await page.addInitScript(() => {
    window.objectUrls = { created: [], revoked: [] };
    const create = URL.createObjectURL.bind(URL);
    const revoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = (blob) => { const url = create(blob); window.objectUrls.created.push(url); return url; };
    URL.revokeObjectURL = (url) => { window.objectUrls.revoked.push(url); revoke(url); };
  });
  await page.goto(`${origin}/artifacts-harness`);
  await page.evaluate(async () => {
    const common = await import('/static/js/common.js');
    const tasks = await import('/static/js/tasks.js');
    // Stands in for a fetch whose response was already buffered when the
    // detail retired: it settles regardless of the abort signal.
    const realFetch = window.fetch.bind(window);
    window.lateResponses = [];
    window.fetch = (url, options) => {
      if (!String(url).includes('/late-')) return realFetch(url, options);
      return new Promise(resolve => window.lateResponses.push(async () => {
        const original = await realFetch(String(url).replace('/late-', '/'));
        resolve(new Response(await original.blob(), { status: 200, headers: original.headers }));
      }));
    };
    let mounted = 0;
    // A task detail as the task list keys it, holding the shipped artifact list.
    const detail = (artifacts) => {
      const node = common.el('div', { class: 'row-detail' });
      node.dataset.key = 'task-ART-1';
      node.dataset.hash = String(++mounted);
      node.appendChild(tasks.buildArtifacts({ id: 'ART-1', artifacts }));
      return node;
    };
    const retire = {
      close: () => common.syncNodes(document.getElementById('tasks-body'), []),
      replace: () => common.syncNodes(document.getElementById('tasks-body'), [detail([])]),
      workspace: () => common.setWorkspace(`ws-${mounted}`),
    };
    window.harness = {
      mount(artifacts, how) {
        if (how === 'workspace') {
          common.resetPanel('ws-body', 'ws-count');
          document.getElementById('ws-body').appendChild(detail(artifacts));
        } else {
          common.syncNodes(document.getElementById('tasks-body'), [detail(artifacts)]);
        }
      },
      retire: how => retire[how](),
      liveUrls() {
        const revoked = new Set(window.objectUrls.revoked);
        return window.objectUrls.created.filter(url => !revoked.has(url));
      },
    };
  });

  const counts = () => page.evaluate(() => ({
    created: window.objectUrls.created.length,
    revoked: window.objectUrls.revoked.length,
    live: window.harness.liveUrls(),
  }));
  const artifact = (name) => ({
    path: name,
    media_type: name.endsWith('.png') ? 'image/png' : 'application/octet-stream',
    size_bytes: name.endsWith('.png') ? PNG.length : BIN.length,
  });
  const row = (name) => page.locator('.artifact-row', { hasText: name });
  const check = async (id, body) => {
    const before = await counts();
    await body(before);
    const after = await counts();
    assert(after.live.length === 0, `${id}: every blob URL must be revoked, still live: ${after.live.join(', ')}`);
    results.push({ id, created: after.created - before.created, revoked: after.revoked - before.revoked });
  };

  // 1. A mounted preview works, and normal retirement revokes its blob URL.
  for (const [name, how] of [['shot.png', 'close'], ['bundle.bin', 'replace'], ['diagram.png', 'workspace']]) {
    await check(`mounted-${name}-${how}`, async (before) => {
      await page.evaluate(([artifacts, how]) => window.harness.mount(artifacts, how), [[artifact(name)], how]);
      await row(name).click();
      if (name.endsWith('.png')) {
        const image = page.locator('.artifact-preview img.artifact-image');
        await image.waitFor({ state: 'visible' });
        await until(`${name} decoded`, () => image.evaluate(node => node.complete && node.naturalWidth === 1));
        assert((await image.getAttribute('src')).startsWith('blob:'), `${name}: the image renders from a blob URL`);
        assert(await page.locator('.artifact-preview .artifact-open').getAttribute('href') === await image.getAttribute('src'), `${name}: Open uses the image's blob URL`);
      } else {
        const link = page.locator('.artifact-preview a[download]');
        await link.waitFor({ state: 'visible' });
        assert(await link.getAttribute('download') === name, `${name}: the download keeps the artifact file name`);
        const bytes = await link.evaluate(async node => (await (await fetch(node.href)).text()));
        assert(bytes === BIN.toString(), `${name}: the download link serves the artifact bytes`);
      }
      const mounted = await counts();
      assert(mounted.created === before.created + 1 && mounted.live.length === 1, `${name}: a mounted preview holds exactly one live blob URL`);
      await page.evaluate(how => window.harness.retire(how), how);
    });
  }

  // 2. A response held before its headers or inside its body: retiring the
  // detail cancels the read, and releasing it afterwards creates nothing live.
  for (const [name, how] of [
    ['held-head-shot.png', 'close'],
    ['held-head-bundle.bin', 'workspace'],
    ['held-body-shot.png', 'replace'],
    ['held-body-bundle.bin', 'close'],
  ]) {
    await check(`held-${name}-${how}`, async () => {
      await page.evaluate(([artifacts, how]) => window.harness.mount(artifacts, how), [[artifact(name)], how]);
      await row(name).click();
      await until(`${name} held`, () => held.has(name));
      await page.evaluate(how => window.harness.retire(how), how);
      await until(`${name} request cancelled`, () => held.get(name).aborted);
      held.get(name).release();
      await page.waitForTimeout(200);
    });
  }

  // 3. A response that settles after retirement anyway is released at once,
  // including a download, and is never mounted into the retired detail.
  for (const [name, how] of [['late-shot.png', 'close'], ['late-bundle.bin', 'replace'], ['late-diagram.png', 'workspace']]) {
    await check(`late-${name}-${how}`, async (before) => {
      await page.evaluate(([artifacts, how]) => window.harness.mount(artifacts, how), [[artifact(name)], how]);
      const preview = await row(name).evaluateHandle(node => node.nextElementSibling);
      await row(name).click();
      await until(`${name} pending`, () => page.evaluate(() => window.lateResponses.length === 1));
      await page.evaluate(how => window.harness.retire(how), how);
      await page.evaluate(() => window.lateResponses.shift()());
      await until(`${name} settled`, async () => (await counts()).created === before.created + 1);
      await page.waitForTimeout(50);
      const state = await preview.evaluate(node => ({ connected: node.isConnected, text: node.textContent, links: node.querySelectorAll('a, img').length }));
      assert(!state.connected && state.links === 0 && state.text === 'loading...', `${name}: the retired preview must stay unmounted, got ${JSON.stringify(state)}`);
    });
  }

  // 4. A second click while the read is in flight does not start another one.
  await check('single-read-per-preview', async () => {
    const name = 'held-head-once.png';
    await page.evaluate(([artifacts]) => window.harness.mount(artifacts, 'close'), [[artifact(name)]]);
    await row(name).click();
    await until(`${name} held`, () => held.has(name));
    await row(name).click();
    await row(name).click();
    held.get(name).release();
    await page.locator('.artifact-preview img.artifact-image').waitFor({ state: 'visible' });
    assert(requested.filter(entry => entry === name).length === 1, `${name}: clicks during a read must not refetch`);
    await page.evaluate(() => window.harness.retire('close'));
  });

  assert(pageErrors.length === 0, `page errors: ${pageErrors.join('; ')}`);
  fs.writeFileSync(path.join(evidence, 'task-artifacts-assertions.json'), `${JSON.stringify({ results, requested }, null, 2)}\n`);
  console.log(`task artifacts browser: ${results.length} checks passed`);
} finally {
  await browser?.close();
  server.closeAllConnections?.();
  server.close();
}
