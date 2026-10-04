// Capture the dashboard screenshots the docs use, from a running
// `orbit web serve` on the demo workspace that shots.sh seeds.
//
//   node capture.mjs <dashboard-url> <out-dir>
//
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
const settle = () => page.waitForTimeout(800);

// Clip to the union of the given elements' boxes, padded, and never into the
// log ticker along the bottom edge (it prints host paths).
async function shot(name, locators, pad = 12) {
  const boxes = [];
  for (const l of locators) boxes.push(await l.boundingBox());
  const logTop = (await page.locator("#log-statusbar").boundingBox())?.y ?? Infinity;
  const x = Math.max(0, Math.min(...boxes.map((b) => b.x)) - pad);
  const y = Math.max(0, Math.min(...boxes.map((b) => b.y)) - pad);
  const right = Math.max(...boxes.map((b) => b.x + b.width)) + pad;
  const bottom = Math.min(logTop, Math.max(...boxes.map((b) => b.y + b.height)) + pad);
  const file = path.join(outDir, `${name}.png`);
  await page.screenshot({ path: file, clip: { x, y, width: right - x, height: bottom - y } });
  console.log(`wrote ${file}`);
}

async function openTab(tab) {
  await page.locator(`button.tab[data-tab="${tab}"]`).click();
  await settle();
}

await page.goto(base, { waitUntil: "networkidle" });
await page.locator("#tasks-body .row").first().waitFor();
await settle();

// The whole Tasks view: rail, task groups, and the Drain dock.
await shot("dashboard-tasks", [page.locator("body")], 0);

// The task list: Awaiting approval with Approve, the backlog with Ship.
const panel = page.locator("#tasks-panel");
await shot("dashboard-approve-ship", [panel.locator(".head, h2").first(), page.locator("#tasks-body .row").last()]);

// A backlog task opened, down to the actions for its status. The detail is
// taller than the viewport, so grow the page for this one shot.
await page.setViewportSize({ width: WIDTH, height: 1800 });
const row = page.locator('#tasks-body .row:has-text("Add a delete command")');
await row.click();
const detail = row.locator("xpath=following-sibling::*[1]");
await detail.waitFor();
await detail.getByText("Acceptance Criteria").click();
await settle();
const actions = detail.locator(".actions", { has: page.locator('button:text-is("ship")') }).first();
await shot("dashboard-task-detail", [row, actions], 8);
await row.click();
await page.setViewportSize({ width: WIDTH, height: HEIGHT });
await settle();

// The Drain card: window length, parallel tasks, completion, Start.
await shot("dashboard-drain-card", [page.locator("#auto-drain-panel")], 0);

// Automation: the seeded routines, each off until you turn it on.
await page.setViewportSize({ width: WIDTH, height: 1400 });
await openTab("operations");
await shot("dashboard-automation", [page.locator("#routines-panel .operation-group").first()], 0);
await page.setViewportSize({ width: WIDTH, height: HEIGHT });

// A finished ship run and its steps, when shots.sh was asked to make one.
await openTab("runs");
const run = page.locator("#runs-body .row, .runs .row, .row").filter({ hasText: /task_(local|pr)_pipeline/ }).filter({ hasText: /succe/ }).first();
if (await run.count()) {
  await run.click();
  await settle();
  await shot("dashboard-run-detail", [run, run.locator("xpath=following-sibling::*[1]")], 8);
} else {
  console.log("no successful ship run; skipped dashboard-run-detail");
}

await browser.close();
