// Usage: node dashboard_operations_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const assets = fileURLToPath(new URL('../../assets/dashboard/', import.meta.url));
const test = fileURLToPath(new URL('./dashboard_operations.mjs', import.meta.url));
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const file = name === '/test.mjs' ? test : path.join(assets, name === '/' ? 'index.html' : path.basename(name));
  if (!fs.existsSync(file)) { res.writeHead(404); res.end(); return; }
  let data = fs.readFileSync(file);
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', file.endsWith('.html') ? 'text/html' : file.endsWith('.css') ? 'text/css' : 'text/javascript');
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({headless:true});
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  // Serve the actual markup/styles with only the Operations module initialized.
  // All API traffic is fixture data; no live scheduler or dashboard is contacted.
  page.on('pageerror', error => console.error(error));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.addScriptTag({ type: 'module', url: '/test.mjs' });
  await page.waitForFunction(() => globalThis.operationsTestsPassed, undefined, { timeout: 15000 });
  await page.evaluate(() => {
    document.querySelectorAll('.tab-pane').forEach(node => node.classList.toggle('active', node.dataset.tab === 'operations'));
    document.body.classList.add('operations-active');
  });
  for (const width of [1440, 390]) {
    await page.setViewportSize({width, height:1000});
    for (const tab of ['routines','auto-tasks']) {
      await page.evaluate(tab => {
        for (const name of ['routines','auto-tasks','auto-drain']) document.getElementById(`operations-${name}-main`).hidden = name !== tab;
      }, tab);
      await page.waitForTimeout(350);
      await page.screenshot({path:path.join(evidence, `${tab}-${width}.png`),fullPage:true});
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
      if (overflow) throw new Error(`Horizontal overflow at ${width} / ${tab}`);
      const clipped = await page.evaluate(tab => {
        const panel = document.getElementById(`operations-${tab}-main`);
        return Array.from(panel.querySelectorAll('button, select, .operation-clock-summary')).some(node => {
          const bounds = node.getBoundingClientRect();
          return bounds.right > window.innerWidth || node.scrollWidth > node.clientWidth + 1;
        });
      }, tab);
      if (clipped) throw new Error(`Clipped Operations control at ${width} / ${tab}`);
    }
  }
  console.log(`PASS: Chromium Operations fixture; desktop 1440 and narrow 390; no horizontal overflow. Screenshots: ${evidence}`);
} finally {
  await browser?.close(); server.close();
}
