// Capture the dashboard screenshots the docs use from a running dashboard.
//
//   node capture.mjs <dashboard-url> <out-dir> [--hosts-only|--fixture-core]
//
// <dashboard-url> selects one workspace, e.g.
// http://localhost:7878/?workspace=ws_orbit. The script only reads: it opens
// tabs and a run's detail, and never presses an action button.
// Needs Playwright and a browser; see README.md for the prepared-host kit.
import { mkdir } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";

const require = createRequire(import.meta.url);
const { chromium } = process.env.PLAYWRIGHT_MODULE
  ? await import(process.env.PLAYWRIGHT_MODULE)
  : require("playwright");

const [base, outDir, selection] = process.argv.slice(2);
if (!base || !outDir || (selection && !["--hosts-only", "--fixture-core"].includes(selection))) {
  console.error("usage: node capture.mjs <dashboard-url> <out-dir> [--hosts-only|--fixture-core]");
  process.exit(2);
}
await mkdir(outDir, { recursive: true });

const WIDTH = 1440;
const HEIGHT = 900;
const browser = await chromium.launch(process.env.PLAYWRIGHT_MODULE ? {} : { channel: "chrome" });
const page = await browser.newPage({
  viewport: { width: WIDTH, height: HEIGHT },
  deviceScaleFactor: 2,
  colorScheme: "dark",
  locale: "en-GB",
  timezoneId: "America/Los_Angeles",
});
const settle = (ms = 1200) => page.waitForTimeout(ms);

if (selection === "--fixture-core") {
  // Safe, synthetic content for the three screenshots published in the
  // dashboard guide. All API reads are intercepted so the capture cannot
  // include data from the live workspace selected by the dashboard server.
  await page.route("**/api/**", async (route) => {
    const { pathname } = new URL(route.request().url());
    let payload = [];
    switch (pathname) {
      case "/api/workspaces":
        payload = [{ id: "ws_demo", name: "dashboard-demo", status: "active", is_default: true }];
        break;
      case "/api/tasks":
        payload = {
          items: [
            { id: "DEMO-001", title: "Refresh the dashboard guide", status: "backlog", priority: "medium" },
            { id: "DEMO-002", title: "Document delivery capacity", status: "backlog", priority: "low" },
            { id: "DEMO-003", title: "Build the website", status: "in-progress", priority: "medium" },
          ],
          total: 3,
          limit: 50,
          truncated: false,
        };
        break;
      case "/api/crews":
        payload = { default_crew: "demo", crews: [{ name: "demo", model: "fixture" }] };
        break;
      case "/api/audit/summary":
        payload = { events: 14, failed_runs: 1, window: "24h" };
        break;
      case "/api/host/resources":
        payload = {
          sample_age_seconds: 8,
          throttle: false,
          thresholds: { enabled: true },
          cpu: { percent: 32, severity: "normal" },
          memory: { percent: 48, severity: "normal" },
          disk: { percent: 27, severity: "normal" },
          pressures: [],
          reason: "No sustained resource pressure",
        };
        break;
      case "/api/workflows/auto/readiness":
        payload = {
          capacity: { drain_phase: "idle", active_leaf_runs: 0, max_active_leaf_runs: 5, free_slots: 5 },
          tasks: [
            { task_id: "DEMO-001", status: "backlog", eligible: true, reason: "ready" },
            {
              task_id: "DEMO-002",
              status: "backlog",
              eligible: false,
              reason: "conflict_deferred",
              blocking_task_ids: ["DEMO-003"],
              conflicts: [{ requested_file: "file:website/dashboard.md", blocking_task_id: "DEMO-003" }],
            },
          ],
        };
        break;
      case "/api/routines":
        payload = {
          machine_name: "demo-host",
          cron_zone: { name: "America/Los_Angeles", offset_seconds: -25200 },
          clock: { enabled: true, running: true, provider: "local", schedulable: true, configured_cadence_seconds: 60, effective_cadence_seconds: 60 },
          routines: [
            { name: "docs-check", source: "dashboard-demo", target: "job:website_check", enabled: true, effective: true, cron: "0 9 * * 1", next_due: "2026-10-12T09:00:00-07:00" },
            { name: "task-review", source: "dashboard-demo", target: "job:task_review", enabled: true, effective: true, cron: "*/30 * * * *", next_due: "2026-10-08T08:30:00-07:00" },
            { name: "release-notes", source: "dashboard-demo", target: "job:release_notes", enabled: false, effective: false, cron: "0 12 * * 5", next_due: "2026-10-09T12:00:00-07:00" },
          ],
        };
        break;
      case "/api/job-runs":
        payload = { items: [], total: 0, limit: 100, truncated: false };
        break;
      default:
        if (pathname.endsWith("/host/resources")) {
          payload = {
            sample_age_seconds: 8,
            throttle: false,
            thresholds: { enabled: true },
            cpu: { percent: 32, severity: "ok" },
            memory: { percent: 48, severity: "ok" },
            disk: { percent: 27, severity: "ok" },
            pressures: [],
            reason: "No sustained resource pressure",
          };
        } else if (pathname.endsWith("/workflows/auto/readiness")) {
          payload = {
            capacity: { drain_phase: "idle", active_leaf_runs: 0, max_active_leaf_runs: 5, free_slots: 5, pull_drain_run_id: null },
            tasks: [],
          };
        }
        break;
    }
    await route.fulfill({ json: payload });
  });
}

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

async function hostsShot() {
  const hostsUrl = new URL(base);
  hostsUrl.hash = "config/hosts";
  await page.goto(hostsUrl.href, { waitUntil: "networkidle" });
  await page.locator(".host-list .host-row").first().waitFor();
  await settle();
  // Omit the scope strip and trailing error detail, which can print host paths.
  const targets = [
    page.locator(".host-head"),
    ...await page.locator(".host-row > .host-grid").all(),
  ];
  const add = page.locator(".host-add");
  if (await add.count()) targets.unshift(add);
  await shot("dashboard-hosts", targets, 0);
}

if (selection === "--hosts-only") {
  try {
    await hostsShot();
  } finally {
    await browser.close();
  }
  process.exit(0);
}

const tasksUrl = new URL(base);
tasksUrl.hash = "tasks?status=in-progress,review,blocked,proposed,backlog";
await page.goto(tasksUrl.href, { waitUntil: "networkidle" });
await page.locator("#tasks-body .row").first().waitFor();
await settle();

// The Tasks list with the resource strip and 24-hour refresh clock.
await shot("dashboard-tasks", [
  { x: 0, y: 0, width: 0, height: 0 },
  page.locator("#topbar-crumb"),
  page.locator("#host-resource-chips"),
  page.locator("#tasks-panel"),
  page.locator("#meta-text"),
], 12);

if (selection !== "--fixture-core") {
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
}

// Keep the top bar and 24-hour refresh clock with the Drain card.
await shot("dashboard-drain-card", [page.locator(".topbar"), page.locator("#auto-drain-panel"), page.locator("#meta-text")], 24);

// Automation: the workspace's routines and whether each will fire.
await page.locator('button.tab[data-tab="operations"]').click();
await settle();
await shot("dashboard-automation", [page.locator(".topbar"), page.locator("#routines-panel"), page.locator("#meta-text")], 24);

if (selection === "--fixture-core") {
  await browser.close();
  process.exit(0);
}

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

await hostsShot();
await browser.close();
