# Dashboard screenshots

The dashboard images in `website/src/assets/dashboard/` are captured from a
running dashboard by `capture.mjs`. It only reads: it opens views and one
run's detail. It never presses Approve, Ship, Start, or any other action.

Regenerate them after a release that changes the dashboard, with the dashboard
running (`orbit web serve`, or `orbit web connect <host>`). Five of the six
come from synthetic data, so the published pages show no live task titles,
user names, or host names:

```bash
npm --prefix .orbit/tmp/orbit-playwright install --no-save playwright
NODE_PATH="$PWD/.orbit/tmp/orbit-playwright/node_modules" \
  node website/scripts/dashboard-shots/capture.mjs \
  "http://localhost:7878/?workspace=ws_demo" website/src/assets/dashboard --fixture-core
```

`--fixture-core` intercepts every dashboard API read, so the selected
workspace can be `ws_demo` even when the local server has no such workspace,
and the host file the pages show is an isolated demo registry: `local-demo`
is local and `build-box` uses `build-box.invalid`, an intentionally
unreachable example target, so the Hosts view shows its real error reporting.
The page clock is pinned to 2026-10-08 07:26:47 PDT, so relative times and the
refresh clock come out the same on every capture.

The run detail is the one live image. Pick a successful `task_pr_pipeline` run
and capture it by id:

```bash
NODE_PATH="$PWD/.orbit/tmp/orbit-playwright/node_modules" \
  node website/scripts/dashboard-shots/capture.mjs \
  "http://localhost:7878/?workspace=ws_orbit" website/src/assets/dashboard \
  --run-detail jrun-20261010-2230-c11
```

Without an id, `--run-detail` opens the newest successful `task_pr_pipeline`
run in the Runs list. With no selection at all, the script captures every
image from the live workspace, which is useful for checking a release but
not for publishing.

The capture uses your installed Google Chrome (`channel: "chrome"`). On a
prepared Linux host, use the shared Playwright and Chromium kit instead:

```bash
. ~/.local/chromium-deps/env.sh
node website/scripts/dashboard-shots/capture.mjs \
  "http://localhost:7878/" .orbit/tmp/dashboard-shots --hosts-only
```

`PLAYWRIGHT_MODULE` selects the kit's Playwright module and bundled Chromium.
`--hosts-only` captures only Settings › Hosts from the live host file.

The browser viewport is 1440 × 900 CSS pixels at device scale factor 2, and
taller (1600) for the task list, the Drain dock, and the run detail so nothing
is cut. Every crop stops above the log ticker along the bottom edge, which
prints host paths. The Hosts crop also excludes the scope strip and trailing
SSH error detail, which can contain host paths. Before committing, look at
each image for anything that shouldn't be public, such as a task title or a
user name; the pages show them as they are.

| File | Size (px) | Shows | Used on |
|---|---|---|---|
| `dashboard-tasks.png` | 2880 × 1732 | The whole Tasks view: top bar with host resources and the refresh clock, the rail, status cards, and the Drain dock | Quickstart, Use the Dashboard |
| `dashboard-approve-ship.png` | 1648 × 1416 | The task list: Awaiting approval with **Approve**, In progress with **View run**, Backlog with its readiness strip and **Ship** | First Task |
| `dashboard-run-detail.png` | 2396 × 1782 | A successful `task_pr_pipeline` run and its step timeline | First Task |
| `dashboard-drain-card.png` | 816 × 1506 | The Drain dock: the Auto-drain card and Start a window | Delivery Workflows, Use the Dashboard |
| `dashboard-automation.png` | 2880 × 1732 | Automation → Routines with the sweep clock, the next hour, and each routine | Use the Dashboard, Schedule Recurring Work |
| `dashboard-hosts.png` | 2348 × 498 | Settings → Hosts add control and host table with each host's health | Use the Dashboard |

All six were captured on 2026-10-10 from the redesigned dashboard (status-card
task list, Drain and Log dock, full-width top bar). The run detail is
`jrun-20261010-2230-c11` on the live `ws_orbit` workspace: ORB-15347, a red-CI
fix, 21 steps in 20m 35s. Its counts are from that moment, not live.
