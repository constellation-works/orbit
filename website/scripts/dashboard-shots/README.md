# Dashboard screenshots

The dashboard images in `website/src/assets/dashboard/` are captured from a
live dashboard by `capture.mjs`. It only reads: it switches tabs, sets the Runs
filter to **All**, and opens one run's detail. It never presses Approve, Ship,
Start, or any other action.

Regenerate them after a release that changes the dashboard, with the dashboard
running (`orbit web serve`, or `orbit web connect <host>`):

```bash
npm --prefix /tmp/orbit-playwright install --no-save playwright
NODE_PATH=/tmp/orbit-playwright/node_modules \
  node website/scripts/dashboard-shots/capture.mjs \
  "http://localhost:7878/?workspace=ws_orbit" website/src/assets/dashboard
```

The capture uses your installed Google Chrome (`channel: "chrome"`). Every crop
stops above the log ticker along the bottom edge, which prints host paths.
Before committing, look at each image for anything that shouldn't be public,
such as a task title or a user name; the pages show them as they are.

| File | Shows | Used on |
|---|---|---|
| `dashboard-tasks.png` | The whole Tasks view | Quickstart, Use the Dashboard |
| `dashboard-approve-ship.png` | Awaiting approval with **Approve**, the backlog with **Ship** | First Task |
| `dashboard-run-detail.png` | A successful `task_pr_pipeline` run and its step timeline | First Task |
| `dashboard-drain-card.png` | The Drain card | Delivery Workflows, Use the Dashboard |
| `dashboard-automation.png` | Automation → Routines | Use the Dashboard, Schedule Recurring Work |

The current images show the `ws_orbit` workspace, captured 2026-10-04 from
Orbit 0.26.0. Counts in them (failed runs, events, queue sizes) are that
moment's, not live.
