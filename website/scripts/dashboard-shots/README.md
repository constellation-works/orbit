# Dashboard screenshots

The dashboard images in `website/src/assets/dashboard/` are captured from a
live dashboard by `capture.mjs`. It only reads: it switches tabs, sets the Runs
filter to **All**, and opens one run's detail. It never presses Approve, Ship,
Start, or any other action.

Regenerate them after a release that changes the dashboard, with the dashboard
running (`orbit web serve`, or `orbit web connect <host>`):

```bash
npm --prefix .orbit/tmp/orbit-playwright install --no-save playwright
NODE_PATH="$PWD/.orbit/tmp/orbit-playwright/node_modules" \
  node website/scripts/dashboard-shots/capture.mjs \
  "http://localhost:7878/?workspace=ws_orbit" website/src/assets/dashboard
```

The capture uses your installed Google Chrome (`channel: "chrome"`). On a
prepared Linux host, use the shared Playwright and Chromium kit instead:

```bash
. ~/.local/chromium-deps/env.sh
node website/scripts/dashboard-shots/capture.mjs \
  "http://localhost:7878/" .orbit/tmp/dashboard-shots --hosts-only
```

`PLAYWRIGHT_MODULE` selects the kit's Playwright module and bundled Chromium.
`--hosts-only` captures only Settings › Hosts and does not need task or run
fixtures. The default capture includes Hosts after the other views.

For public docs, `--fixture-core` captures only Tasks, Drain, and Automation
with synthetic API responses. It intercepts every dashboard API read, so the
selected workspace can be `ws_demo` even when the local server has no such
workspace. These three screenshots include the top bar and its 24-hour refresh
clock. The browser viewport is 1440 × 900 CSS pixels at device scale factor 2;
the PNG crop stops above the log ticker. Current output sizes are 2064 × 1740
pixels for Tasks and 2856 × 1740 pixels for both Drain and Automation.

```bash
node website/scripts/dashboard-shots/capture.mjs \
  "http://localhost:7878/?workspace=ws_demo" website/src/assets/dashboard --fixture-core
```

Every crop
stops above the log ticker along the bottom edge, which prints host paths.
Before committing, look at each image for anything that shouldn't be public,
such as a task title or a user name; the pages show them as they are.

| File | Shows | Used on |
|---|---|---|
| `dashboard-tasks.png` | Tasks rail and groups, with the resource strip and 24-hour clock | Quickstart, Use the Dashboard |
| `dashboard-approve-ship.png` | Awaiting approval with **Approve**, the backlog with **Ship** | First Task |
| `dashboard-run-detail.png` | A successful `task_pr_pipeline` run and its step timeline | First Task |
| `dashboard-drain-card.png` | The Drain card with the resource strip and 24-hour clock | Delivery Workflows, Use the Dashboard |
| `dashboard-automation.png` | Automation → Routines with the resource strip and 24-hour clock | Use the Dashboard, Schedule Recurring Work |
| `dashboard-hosts.png` | Settings → Hosts add control and host table | Use the Dashboard |

The task, drain, and automation images use the synthetic fixture and were
captured from Orbit 0.28.0 on 2026-10-08. The run-detail image remains from
Orbit 0.26.0, captured 2026-10-04; counts in it are from that moment, not live.

The Hosts image was captured 2026-10-08 from Orbit 0.28.0 on an isolated demo
registry: `local-demo` is local and `build-box` uses `build-box.invalid`, an
intentionally unreachable example target. It shows the real view's error
reporting without publishing production host names. Its crop also excludes
the scope strip and trailing SSH error detail, which can contain host paths.
