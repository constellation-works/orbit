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
refresh clock come out the same on every capture. For the Drain shot only, the
fixture holds admissions on a CPU throttle, so that page shows its throttle
banner; the other captures keep an unthrottled host.

The run detail uses a dated live capture. The script replaces its task,
friction and run identifiers with demo labels in the capture DOM before
taking the screenshot; the server's records are unchanged. Pick a successful
`task_pr_pipeline` run and capture it by id:

```bash
NODE_PATH="$PWD/.orbit/tmp/orbit-playwright/node_modules" \
  node website/scripts/dashboard-shots/capture.mjs \
  "http://localhost:7878/?workspace=ws_orbit" website/src/assets/dashboard \
  --run-detail <run-id>
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

Run the browser regression with the same Playwright/browser setup:

```bash
npm --prefix website run test:dashboard-capture
```

It runs the actual capture script against a local synthetic dashboard and
checks the header and timeline at screenshot time, plus the unchanged API
response. Task redaction covers all 2–5 uppercase ASCII-letter prefixes,
including historical ORB IDs. It conservatively redacts any text token with
that ID shape; dates, counts and other labels remain. The test keeps its PNG
and text evidence under `.orbit/tmp/dashboard-capture-*` for inspection.
This regression does not replace the rendered-pixel inspection below.

The browser viewport is 1440 × 900 CSS pixels at device scale factor 2, and
taller for the task list and the run detail (1600) and the Drain page (1800) so
nothing is cut. Every crop stops above the log ticker along the bottom edge, which
prints host paths. The Hosts crop also excludes the scope strip and trailing
SSH error detail, which can contain host paths. Before committing, look at
each image for anything that shouldn't be public, including production task,
friction or run identifiers, a task title or a user name. Inspect the rendered
pixels even when the capture script sanitizes text.

| File | Size (px) | Shows | Used on |
|---|---|---|---|
| `dashboard-tasks.png` | 2880 × 1732 | The whole Tasks view: top bar with host resources and the refresh clock, the rail with Drain as its own page, and the status cards | Quickstart, Use the Dashboard |
| `dashboard-approve-ship.png` | 2448 × 1176 | The task list: Awaiting approval with **Approve**, In progress with **View run**, Backlog with its waiting summary, each task's readiness and **Ship** | First Task |
| `dashboard-run-detail.png` | 2396 × 1782 | A successful `task_pr_pipeline` run and its step timeline | First Task |
| `dashboard-drain-card.png` | 2448 × 2878 | The Drain page's Overview: throttle banner, summary tiles, the Auto-drain card, why tasks are waiting and fleet drains, then Start a window and Locks held | Delivery Workflows, Use the Dashboard |
| `dashboard-automation.png` | 2880 × 1732 | Automation → Routines with the sweep clock, the next hour, and each routine | Use the Dashboard, Schedule Recurring Work |
| `dashboard-hosts.png` | 2348 × 498 | Settings → Hosts add control and host table with each host's health | Use the Dashboard |

Five were captured on 2026-10-10 from the redesigned dashboard (status-card
task list, Drain and Log dock, full-width top bar). The Drain, Tasks and
task-list images were recaptured on 2026-10-19 after Drain became its own page. The run detail shows a
red-CI fix, 21 steps in 20m 35s, with its artifact identifiers replaced by
demo labels. Its counts are from that moment, not live.
