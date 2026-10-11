import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const repoRoot = fileURLToPath(new URL("../../", import.meta.url));
const captureScript = fileURLToPath(new URL("dashboard-shots/capture.mjs", import.meta.url));
const require = createRequire(import.meta.url);
const playwrightModule = process.env.PLAYWRIGHT_MODULE
  || pathToFileURL(require.resolve("playwright")).href;
const scratchRoot = path.join(repoRoot, ".orbit/tmp");
await mkdir(scratchRoot, { recursive: true });
const scratch = await mkdtemp(path.join(scratchRoot, "dashboard-capture-"));

// Short, intermediate and maximum-length prefixes, plus legacy ORB and the
// previously supported ORBA. Repeat them in both crop targets, including nested text nodes.
const taskIds = ["ZZ-567", "BOX-345", "ABC-456", "DANI-789", "ABCDE-678", "ORB-123", "ORBA-234"];
const runId = "jrun-20261011-2120-c27";
const controls = `F2026-10-123 ${runId} JRUN-20261011-2120-C27`;
const ordinary = "2026-10-11 21 steps 20m 35s success task_pr_pipeline build check";
const boundaries = "A-1 ABCDEF-2 box-3 XBOX-4suffix";
const source = {
  header: `${taskIds.join(" ")} ${controls} ${ordinary} ${boundaries}`,
  timeline: taskIds.map((id) => `${id}: ${controls} ${ordinary} ${boundaries}`),
};
const expectedControls = "demo-friction demo-run demo-run";
const expected = {
  header: `${taskIds.map(() => "demo-task").join(" ")} ${expectedControls} ${ordinary} ${boundaries}`,
  timeline: taskIds.map(() => `demo-task: ${expectedControls} ${ordinary} ${boundaries}`),
};
const sourceBytes = JSON.stringify(source);
const requests = [];
const html = `<!doctype html>
<meta charset="utf-8">
<style>
  body { margin: 24px; background: #141820; color: #eee; font: 16px sans-serif; }
  #run-detail-panel { width: 1100px; }
  header, .gantt-panel { padding: 20px; background: #222936; }
  header { line-height: 1.8; }
  .gantt-panel p { margin: 12px 0; line-height: 1.8; }
  #log-statusbar { position: fixed; bottom: 0; }
</style>
<aside id="outside">${taskIds[0]}</aside>
<section id="run-detail-panel"><header></header></section>
<div id="log-statusbar">private host log</div>
<script type="module">
  const response = await fetch('/api/run-detail');
  const source = await response.json();
  window.sourceResponse = source;
  document.querySelector('header').textContent = source.header;
  const timeline = document.createElement('div');
  timeline.className = 'gantt-panel';
  for (const text of source.timeline) {
    const row = document.createElement('p');
    const nested = document.createElement('span');
    nested.textContent = text;
    row.append(nested);
    timeline.append(row);
  }
  document.querySelector('#run-detail-panel').append(timeline);
</script>`;
const server = createServer((request, response) => {
  requests.push({ method: request.method, url: request.url });
  response.setHeader("Content-Type", request.url === "/api/run-detail" ? "application/json" : "text/html");
  response.end(request.url === "/api/run-detail" ? sourceBytes : html);
});

// Observe the real page immediately before its real screenshot. No sanitizer
// is copied into the harness: the child runs capture.mjs without modification.
const wrapper = path.join(scratch, "observe-playwright.mjs");
await writeFile(wrapper, `
import { writeFile } from "node:fs/promises";
import { chromium as realChromium } from ${JSON.stringify(playwrightModule)};
export const chromium = {
  async launch() {
    const browser = await realChromium.launch(${process.env.PLAYWRIGHT_MODULE ? "{}" : '{ channel: "chrome" }'});
    const newPage = browser.newPage.bind(browser);
    browser.newPage = async (...args) => {
      const page = await newPage(...args);
      const screenshot = page.screenshot.bind(page);
      page.screenshot = async (options) => {
        const evidence = await page.evaluate(() => ({
          header: document.querySelector('#run-detail-panel > header').textContent,
          timeline: [...document.querySelectorAll('.gantt-panel p')].map(row => row.textContent),
          sourceResponse: window.sourceResponse,
          outside: document.querySelector('#outside').textContent,
          hash: location.hash,
        }));
        await writeFile(options.path + '.json', JSON.stringify(evidence, null, 2));
        return screenshot(options);
      };
      return page;
    };
    return browser;
  }
};
`);

try {
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const base = `http://127.0.0.1:${server.address().port}/`;
  const before = await (await fetch(`${base}api/run-detail`)).text();
  const { stdout, stderr } = await promisify(execFile)(process.execPath,
    [captureScript, base, scratch, "--run-detail", runId], {
      env: { ...process.env, PLAYWRIGHT_MODULE: pathToFileURL(wrapper).href },
      timeout: 60_000,
    });
  const png = path.join(scratch, "dashboard-run-detail.png");
  const evidence = JSON.parse(await readFile(`${png}.json`, "utf8"));
  assert.equal(evidence.header, expected.header, "the captured header must redact all task prefixes and controls, preserving ordinary text");
  assert.deepEqual(evidence.timeline, expected.timeline, "every captured timeline row must redact identifiers, preserving ordinary text");
  assert.deepEqual(evidence.sourceResponse, source, "capture must not mutate the page's source dashboard response");
  assert.equal(evidence.outside, taskIds[0], "redaction must stay inside the run-detail capture panel");
  assert.equal(evidence.hash, `#runs/${runId}`, "the actual run-detail navigation must execute");
  const after = await (await fetch(`${base}api/run-detail`)).text();
  assert.equal(before, sourceBytes);
  assert.equal(after, before, "the source API response must stay unchanged after capture");
  assert.ok(requests.every(({ method }) => method === "GET"), "capture must only read the dashboard");
  assert.ok(requests.filter(({ url }) => url === "/api/run-detail").length >= 3,
    "the browser and before/after checks must read the source response");
  const image = await readFile(png);
  assert.deepEqual(image.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), "capture must write a real PNG");
  assert.ok(image.readUInt32BE(16) > 0 && image.readUInt32BE(20) > 0, "the rendered crop must have positive dimensions");
  console.log(stdout.trim());
  if (stderr) console.error(stderr.trim());
  console.log(`Dashboard capture regression passed; inspect ${png}`);
} finally {
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
}
