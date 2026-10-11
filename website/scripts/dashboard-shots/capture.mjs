// Capture the dashboard screenshots the docs use from a running dashboard.
//
//   node capture.mjs <dashboard-url> <out-dir> [--hosts-only|--fixture-core|--run-detail [run-id]]
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

const [base, outDir, selection, runId] = process.argv.slice(2);
const SELECTIONS = ["--hosts-only", "--fixture-core", "--run-detail"];
if (!base || !outDir || (selection && !SELECTIONS.includes(selection)) || (runId && selection !== "--run-detail")) {
  console.error("usage: node capture.mjs <dashboard-url> <out-dir> [--hosts-only|--fixture-core|--run-detail [run-id]]");
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

// A live dashboard keeps log streams open, so the network never goes idle:
// load the page, then wait for the element each shot needs.
async function open(hash, ready) {
  const url = new URL(base);
  url.hash = hash;
  await page.goto(url.href, { waitUntil: "domcontentloaded" });
  await page.locator(ready).first().waitFor();
  await settle();
}

if (selection === "--fixture-core") {
  // Safe, synthetic content for the screenshots published in the docs. All
  // API reads are intercepted so the capture cannot include data from the
  // live workspace or host file of the dashboard server. The page clock is
  // pinned to the fixture's morning so relative times ("in 1 h 4 min") and
  // the refresh clock come out the same on every capture.
  await page.clock.setFixedTime(new Date("2026-10-08T07:26:47-07:00"));
  const resources = {
    sample_age_seconds: 8,
    throttle: false,
    thresholds: { enabled: true },
    cpu: { percent: 32, severity: "ok" },
    memory: { percent: 48, severity: "ok" },
    disk: { percent: 27, severity: "ok" },
    pressures: [],
    reason: "No sustained resource pressure",
  };
  const workspace = { id: "ws_demo", name: "dashboard-demo", role: "owner", owner_machine_id: "hm_demo_local", status: "active" };
  await page.route("**/api/**", async (route) => {
    const { pathname } = new URL(route.request().url());
    let payload = [];
    switch (pathname) {
      case "/api/workspaces":
        payload = [{ id: "ws_demo", name: "dashboard-demo", status: "active", is_default: true }];
        break;
      case "/api/hosts":
        // An isolated demo registry: the serving host, and a remote whose
        // address is intentionally unreachable (`.invalid`), so the view shows
        // its real error reporting without publishing production host names.
        payload = {
          host_file: "~/.orbit/hosts.toml",
          legacy: false,
          generation: 0,
          load_error: null,
          host_edit: { authorized: true, reason: null },
          hosts: [
            {
              name: "local-demo", machine_id: "hm_demo_local", ssh: null, task_prefix: "DEMO", local: true, legacy: false,
              reachable: true, error: null, binary_version: "0.28.1", protocol_fingerprint: "e7525a3b5f1e", skew: false, skew_fields: [],
              workspaces: [workspace],
            },
            {
              name: "build-box", machine_id: "hm_demo_build", ssh: "build-box.invalid", task_prefix: "BOX", local: false, legacy: false,
              reachable: false, error: { code: "unreachable_destination", message: "ssh: Could not resolve hostname build-box.invalid" }, binary_version: null, protocol_fingerprint: null, skew: false, skew_fields: [],
              workspaces: [],
            },
          ],
        };
        break;
      case "/api/tasks":
        payload = {
          items: [
            { id: "DEMO-004", title: "Add hello.txt containing hello", status: "proposed", priority: "medium", crew: "demo" },
            {
              id: "DEMO-003", title: "Build the website", status: "in-progress", priority: "medium", crew: "demo",
              job_run_id: "jrun-20261008-0712-c1", job_run_machine: { machine_name: "local-demo" },
            },
            { id: "DEMO-001", title: "Refresh the dashboard guide", status: "backlog", priority: "medium", crew: "demo" },
            { id: "DEMO-002", title: "Document delivery capacity", status: "backlog", priority: "low", crew: "demo" },
          ],
          total: 4,
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
        payload = resources;
        break;
      case "/api/workflows/auto/readiness":
        payload = {
          capacity: { drain_phase: "idle", active_leaf_runs: 1, max_active_leaf_runs: 5, free_slots: 4 },
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
          machine_name: "local-demo",
          controls_authorized: true,
          capabilities: Object.fromEntries(["routine_toggle", "job_run", "clock_service", "clock_cadence"].map((key) => [key, { authorized: true, reason: null }])),
          session_explanation:
            "Session access: this dashboard server has operator authority. Actions also check workspace and machine selection. Mint creates a task without starting delivery; bounded-window submission has separate permissions.",
          cron_zone: { name: "America/Los_Angeles", offset_seconds: -25200 },
          clock: {
            enabled: true, loaded: true, running: true, provider: "systemd", schedulable: true, health: "healthy",
            configured_cadence_seconds: 60, effective_cadence_seconds: 60, last_tick_at: "2026-10-08T07:26:02-07:00", next_tick_at: null,
          },
          routines: [
            { name: "docs-check", source: "dashboard-demo", target: "job:website_check", enabled: true, effective: true, cron: "0 9 * * 1", next_due: "2026-10-12T09:00:00-07:00" },
            { name: "task-review", source: "dashboard-demo", target: "job:task_review", enabled: true, effective: true, cron: "*/30 * * * *", next_due: "2026-10-08T07:30:00-07:00" },
            { name: "release-notes", source: "dashboard-demo", target: "job:release_notes", enabled: false, effective: false, cron: "0 12 * * 5", next_due: "2026-10-09T12:00:00-07:00" },
          ],
        };
        break;
      case "/api/job-runs":
        payload = { items: [], total: 0, limit: 100, truncated: false };
        break;
      default:
        if (pathname.endsWith("/host/resources")) {
          payload = resources;
        } else if (pathname.endsWith("/workflows/auto/readiness")) {
          payload = {
            capacity: { drain_phase: "idle", active_leaf_runs: 1, max_active_leaf_runs: 5, free_slots: 4, pull_drain_run_id: null },
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
  const viewport = page.viewportSize();
  const logTop = (await page.locator("#log-statusbar").boundingBox())?.y ?? Infinity;
  const x = Math.max(0, Math.min(...boxes.map((b) => b.x)) - pad);
  const y = Math.max(0, Math.min(...boxes.map((b) => b.y)) - pad);
  const right = Math.min(viewport.width, Math.max(...boxes.map((b) => b.x + b.width)) + pad);
  const bottom = Math.min(logTop, viewport.height, Math.max(...boxes.map((b) => b.y + b.height)) + pad);
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
  await open("config/hosts", ".host-list .host-row");
  // Omit the scope strip and trailing error detail, which can print host paths.
  const targets = [
    page.locator(".host-head"),
    ...await page.locator(".host-row > .host-grid").all(),
  ];
  const add = page.locator(".host-add");
  if (await add.count()) targets.unshift(add);
  await shot("dashboard-hosts", targets, 0);
}

// A pull-request run: its header and step timeline. Without a run id, the
// newest successful `task_pr_pipeline` run in the Runs list.
async function runDetailShot(id) {
  if (id) {
    await open(`runs/${id}`, "#run-detail-panel .gantt-panel");
  } else {
    await open("diagnostics/runs", ".runs-row");
    await page.locator(".tab-pane.active button", { hasText: /^All$/ }).click();
    await settle();
    const run = page.locator(".runs-row").filter({ hasText: "task_pr_pipeline" }).filter({ hasText: "success" }).first();
    await run.locator(":scope > *:nth-child(2)").click();
    await page.locator("#run-detail-panel .gantt-panel").waitFor();
  }
  await tall(1600, async () => {
    const panel = page.locator("#run-detail-panel");
    // Public captures retain the dated run's counts and timeline, but omit
    // the live store's artifact identifiers. This edits only the capture DOM.
    await panel.evaluate((root) => {
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      while (walker.nextNode()) {
        const node = walker.currentNode;
        node.textContent = node.textContent
          .replace(/\bORBA?-\d+\b/g, "demo-task")
          .replace(/\bF\d{4}-\d{2}-\d+\b/g, "demo-friction")
          .replace(/\bjrun-\d{8}-\d{4}-[a-z0-9]+\b/gi, "demo-run");
      }
    });
    await shot("dashboard-run-detail", [panel.locator(":scope > header"), panel.locator(".gantt-panel")], 0);
  });
}

if (selection === "--hosts-only" || selection === "--run-detail") {
  try {
    if (selection === "--hosts-only") await hostsShot();
    else await runDetailShot(runId);
  } finally {
    await browser.close();
  }
  process.exit(0);
}

await open("tasks?status=in-progress,review,blocked,proposed,backlog", "#tasks-body .row");

// The whole Tasks view: the top bar with host resources and the refresh
// clock, the rail, the status cards, and the Drain dock beside them.
await shot("dashboard-tasks", [{ x: 0, y: 0, width: WIDTH, height: HEIGHT }], 0);

// The task list from its header through the end of the Backlog card:
// Awaiting approval with Approve, in-flight work, and backlog rows with Ship.
await tall(1600, async () => {
  const end = await page.evaluate(() => {
    const kids = [...document.querySelectorAll("#tasks-body > *")];
    const at = kids.findIndex((k) => k.matches(".group-header") && /Backlog/.test(k.textContent));
    const rows = kids.slice(at + 1).filter((k) => k.matches(".row:not(.header)")).slice(0, 3);
    const b = rows.at(-1).getBoundingClientRect();
    return { x: b.x, y: b.y, width: b.width, height: b.height };
  });
  await shot("dashboard-approve-ship", [page.locator("#tasks-panel > header"), end]);
});

// The Drain dock: delivery state and capacity, then the controls that start a window.
await tall(1600, async () => {
  const targets = [page.locator("#side-dock .dock-head"), page.locator("#auto-drain-panel")];
  if (await page.locator("#auto-drain-start").isVisible()) targets.push(page.locator("#auto-drain-start"));
  await shot("dashboard-drain-card", targets, 12);
});

// Automation: the workspace's routines and whether each will fire.
await page.locator('button.tab[data-tab="operations"]').click();
await settle();
await shot("dashboard-automation", [page.locator(".topbar"), page.locator("#routines-panel"), page.locator("#meta-text")], 24);

if (selection !== "--fixture-core") await runDetailShot();

await hostsShot();
await browser.close();
