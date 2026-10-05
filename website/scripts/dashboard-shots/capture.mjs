// Capture the dashboard screenshots the docs use from a running dashboard.
//
//   node capture.mjs <dashboard-url> <out-dir>
//
// <dashboard-url> selects one workspace, e.g.
// http://localhost:7878/?workspace=ws_orbit. The script only reads: it opens
// tabs and a run's detail, and never presses an action button.
// Needs Playwright (resolved through NODE_PATH) and Google Chrome; see README.md.
import { mkdir } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const [base, outDir] = process.argv.slice(2);
if (!base || !outDir) {
  console.error("usage: node capture.mjs <dashboard-url> <out-dir>");
  process.exit(2);
}
await mkdir(outDir, { recursive: true });

const WIDTH = 1440;
const HEIGHT = 900;
const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage({
  viewport: { width: WIDTH, height: HEIGHT },
  deviceScaleFactor: 2,
  colorScheme: "dark",
});
const settle = (ms = 1200) => page.waitForTimeout(ms);

// Clip to the union of the given boxes, padded, and never into the log ticker
// along the bottom edge: it prints host paths.
async function shot(name, targets, pad = 12) {
  const boxes = [];
  for (const t of targets) boxes.push(typeof t.boundingBox === "function" ? await t.boundingBox() : t);
  const logTop = (await page.locator("#log-statusbar").boundingBox())?.y ?? Infinity;
  const x = Math.max(0, Math.min(...boxes.map((b) => b.x)) - pad);
  const y = Math.max(0, Math.min(...boxes.map((b) => b.y)) - pad);
  const right = Math.min(WIDTH, Math.max(...boxes.map((b) => b.x + b.width)) + pad);
  const bottom = Math.min(logTop, Math.max(...boxes.map((b) => b.y + b.height)) + pad);
  const file = path.join(outDir, `${name}.png`);
  await page.screenshot({ path: file, clip: { x, y, width: right - x, height: bottom - y } });
  console.log(`wrote ${file}`);
}

async function tall(height, fn) {
  await page.setViewportSize({ width: WIDTH, height });
  await settle(600);
  try {
    await fn();
  } finally {
    await page.setViewportSize({ width: WIDTH, height: HEIGHT });
    await settle(600);
  }
}

const tasksUrl = new URL(base);
tasksUrl.hash = "tasks?status=in-progress,review,blocked,proposed,backlog";
await page.goto(tasksUrl.href, { waitUntil: "networkidle" });
await page.locator("#tasks-body .row").first().waitFor();
await settle();

// The whole Tasks view: rail, task groups, and the Drain dock.
await shot("dashboard-tasks", [{ x: 0, y: 0, width: WIDTH, height: HEIGHT }], 0);

// The task list from its header through the first rows of the backlog:
// Awaiting approval with Approve, in-flight work, and backlog rows with Ship.
await tall(1600, async () => {
  const backlogEnd = await page.evaluate(() => {
    const kids = [...document.querySelectorAll("#tasks-body > *")];
    const at = kids.findIndex((k) => k.matches(".group-header") && /Backlog/.test(k.textContent));
    const rows = kids.slice(at + 1).filter((k) => k.matches(".row")).slice(0, 3);
    const b = rows.at(-1).getBoundingClientRect();
    return { x: b.x, y: b.y, width: b.width, height: b.height };
  });
  await shot("dashboard-approve-ship", [page.locator("#tasks-panel .head, #tasks-panel h2").first(), backlogEnd]);
});

// The Drain card: window state, capacity, window length, parallel tasks,
// completion, Start.
await shot("dashboard-drain-card", [page.locator("#auto-drain-panel")], 0);

// Automation: the workspace's routines and whether each will fire.
await tall(1400, async () => {
  await page.locator('button.tab[data-tab="operations"]').click();
  await settle();
  await shot("dashboard-automation", [page.locator("#routines-panel")], 0);
});

// A successful pull-request run: its header and step timeline.
await page.locator('button.tab[data-tab="runs"]').click();
await settle();
await page.locator(".tab-pane.active button", { hasText: /^All$/ }).click();
await settle();
const run = page.locator(".runs-row").filter({ hasText: "task_pr_pipeline" }).filter({ hasText: "success" }).first();
await run.locator(":scope > *:nth-child(2)").click();
await page.locator("#run-detail-panel .gantt-panel").waitFor();
await tall(1600, async () => {
  const panel = page.locator("#run-detail-panel");
  await shot("dashboard-run-detail", [panel.locator(":scope > header"), panel.locator(".gantt-panel")], 0);
});

await browser.close();
