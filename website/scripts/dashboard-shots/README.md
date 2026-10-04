# Dashboard screenshots

The dashboard images in `website/src/assets/dashboard/` are captured from a
throwaway demo workspace, not from anyone's real tasks. `shots.sh` builds that
workspace in a temporary directory under its own Orbit root, seeds six tasks,
serves the dashboard on a spare port, and runs `capture.mjs` against it.
Nothing touches `~/.orbit` or your agent skill links, and the temporary
directory is removed afterwards (`KEEP=1` keeps it).

Regenerate them after a release that changes the dashboard:

```bash
npm --prefix /tmp/orbit-playwright install --no-save playwright
NODE_PATH=/tmp/orbit-playwright/node_modules website/scripts/dashboard-shots/shots.sh
```

The capture uses your installed Google Chrome (`channel: "chrome"`) and the
`orbit` on your `PATH`, so the images show that release's dashboard. Review the
diff before committing: the pages show these images as they are.

`--with-run` also ships one task through a real agent first and adds
`dashboard-run-detail.png`. It needs a signed-in agent CLI and a few minutes,
and it does not work from inside another sandbox (a nested `sandbox-exec`
denies the worker's writes). No page uses that image yet.

Each shot, and where it appears:

| File | Shows | Used on |
|---|---|---|
| `dashboard-tasks.png` | The whole Tasks view | Quickstart, Use the Dashboard |
| `dashboard-approve-ship.png` | Awaiting approval with **Approve**, the backlog with **Ship** | First Task |
| `dashboard-task-detail.png` | An open backlog task and its actions | First Task |
| `dashboard-drain-card.png` | The Drain card | Delivery Workflows, Use the Dashboard |
| `dashboard-automation.png` | The seeded routines, all off | Use the Dashboard, Schedule Recurring Work |

The task data is real Orbit state for the demo workspace at capture time. The
seed runs as `ORBIT_ACTOR=human:you`, and the shots stop above the log ticker,
so no host user name or path appears.
