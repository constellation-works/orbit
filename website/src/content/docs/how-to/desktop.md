---
title: Use the desktop app
description: "Install the Orbit desktop app, use its tray menu and notifications, and fix a dashboard that does not start."
---

The Orbit desktop app is a native window and tray icon around the
[dashboard](/how-to/dashboard/). It runs `orbit web serve` for you in the
background, and notifies you when a task needs review or a run fails on any of
your hosts.

It is not the [MCP Apps control center](/how-to/mcp-apps/), which runs inside an
MCP client, or the [Claude Code mod](/how-to/claude-code/), which draws in the
terminal.

## Install

The app does not bundle Orbit. It runs the `orbit` CLI installed on your
machine.

1. Install and initialize the CLI:

   ```bash
   npm install -g @orbit-tools/cli
   orbit init
   ```

2. Download the app from the
   [GitHub releases](https://github.com/constellation-works/orbit/releases) page.

| Platform | Packages |
|---|---|
| Linux | `.deb` and `.AppImage`, for x64 and arm64. |
| macOS | Attached only to releases built with Apple signing credentials. |
| Windows | None. Orbit runs inside WSL2. |

## Start the app

The window opens on a status page, *Starting the Orbit dashboard…*, while the
app starts `orbit web serve` on `127.0.0.1`. When the server is listening, the
window loads the dashboard.

The app always starts its own server. It never attaches to one that is already
running. External links, such as documentation or pull requests, open in your
default browser.

## If the dashboard does not start

The status page shows the error, what to do about it and a **Retry** button.
The usual cause is that the app cannot find `orbit`. It looks in this order:

1. the directory in `ORBIT_INSTALL_DIR`
2. `~/.orbit/bin`
3. your `PATH`
4. `~/.cargo/bin`, `~/.local/bin`, `~/.npm-global/bin`, and the `bin` directory
   of each Node version under `~/.nvm/versions/node`
5. `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`

If `orbit` lives elsewhere, set `ORBIT_BIN` to the executable's path. It
overrides the search:

```bash
export ORBIT_BIN="/custom/path/to/orbit"
```

Then click **Retry**, or choose **Restart Dashboard Server** from the tray.

If the server fails to bind or exits during startup, read
`dashboard-server.log`:

| Platform | Log directory |
|---|---|
| Linux | `~/.local/share/works.constellation.orbit.desktop/logs/` |
| macOS | `~/Library/Logs/works.constellation.orbit.desktop/` |

The file is readable only by you. It is truncated when the server starts, once
it is larger than 8 MiB.

## Close or quit

| Platform | Closing the window |
|---|---|
| macOS | Hides the window. The menu bar icon stays, and polling and notifications continue. To stop the server and exit, choose **Quit Orbit** or press `⌘Q`. |
| Linux | Quits the app and stops the server. |

## Tray menu

The icon sits in the system tray on Linux and the menu bar on macOS.

| Menu item | Action |
|---|---|
| Status line | `Dashboard server not running`, `Dashboard server not responding`, or the drain and 24-hour failure counts, for example `2 drains running · 1 failure in 24 h`. A count that could not be read shows `Drains unavailable` or `failures unavailable`. |
| Attention items | Up to 8 failed runs (`Failed: <title>`) and 8 review tasks (`Review: <title>`). Clicking one opens the dashboard at that run or task. |
| **Open Orbit** | Restores and focuses the window. |
| **Restart Dashboard Server** | Stops the server and starts a fresh one. |
| **Check for Updates…** / **Install Orbit X.Y.Z…** | Checks GitHub releases. When an update is ready, the item offers to install it. |
| **Launch at Login** | Toggles starting Orbit when you log in. |
| **Quit Orbit** | Stops the server and exits. |

## Updates

The app checks GitHub releases one minute after launch and every 24 hours.
**Check for Updates…** checks at once.

- The app verifies the release's signed checksum manifest against the Orbit
  release public keys built into it. It rejects a manifest that fails, and
  discards a download whose hash does not match.
- When a verified update is ready, you get a notification and the menu item
  becomes **Install Orbit X.Y.Z…**.
- Choosing it opens a confirmation dialog. Nothing is downloaded or applied
  until you confirm.

## Notifications

Every 20 seconds the app checks this host and every registered remote host for
active drains, runs that failed in the last 24 hours and tasks in review.

- One failed task produces one notification and one tray entry, however many
  runs failed under it.
- With more than one host registered, a notification names the host and
  workspace. Tray rows show the title only.
- A host that stops responding and comes back does not re-announce its old
  events.
- Clicking a notification or its tray row opens the dashboard at that run
  (`#runs?run_id=<id>`) or task (`#tasks?open=<id>`).
