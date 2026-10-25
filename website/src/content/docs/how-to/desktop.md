---
title: Use the desktop app
description: "Launch the native Orbit desktop app, supervise the dashboard server, control the tray, receive fleet notifications, and diagnose startup issues."
sidebar:
  order: 3
---

The Orbit desktop app is a lightweight native window and system tray wrapper
around the Orbit web dashboard. It manages a local `orbit web serve` process in
the background, provides tray controls for server lifecycle and system updates,
and raises desktop notifications when tasks require review or runs fail across
your fleet.

This desktop shell is distinct from other Orbit surfaces:

- It is **not** a separate web application: it loads your local
  [Orbit dashboard](../dashboard/) over loopback, keeping all Host and Origin
  security checks intact.
- It is **not** the embedded MCP Apps control center, which runs inside MCP
  clients or agent sessions.
- It is **not** the terminal-based [Claude Code mod](../claude-code/), which
  renders task bands and board views directly inside your CLI prompt.

## Prerequisites and installation

The desktop app requires the Orbit CLI to be installed first. The app does not
bundle an independent Orbit runtime; instead, it starts and supervises the
`orbit` executable installed on your machine.

1. Install and initialize the Orbit CLI:
   ```bash
   npm install -g @orbit-tools/cli
   orbit init
   ```
2. Download the desktop app for your operating system from the
   [GitHub releases](https://github.com/constellation-works/orbit/releases) page:
   - **Linux**: `.deb` (Debian/Ubuntu) and `.AppImage` packages are attached for
     x64 and arm64 architectures.
   - **macOS**: Native packages are attached only when a release is built with
     Apple signing credentials. Releases without signing credentials do not
     include macOS desktop bundles.
   - **Windows**: Orbit runs inside WSL2 Linux environments; native Windows
     desktop bundles are not distributed.

### How the CLI executable is resolved

When starting the dashboard server, the desktop shell searches standard
installation paths for an executable `orbit` binary:

- Locations configured in the `ORBIT_INSTALL_DIR` environment variable
- `~/.orbit/bin`
- Directories listed in your system `PATH`
- Global package manager locations: `~/.cargo/bin`, `~/.local/bin`,
  `~/.npm-global/bin`, and Node version manager (`nvm`) active versions
- System directories: `/opt/homebrew/bin`, `/usr/local/bin`, and `/usr/bin`

If `orbit` is installed in a non-standard location or outside your user `PATH`,
set the `ORBIT_BIN` environment variable to the exact executable path:

```bash
export ORBIT_BIN="/custom/path/to/orbit"
```

## First launch and dashboard supervision

When you open Orbit:

1. **Status screen**: The app opens a native window displaying a local status
   page with a progress indicator: *Starting the Orbit dashboard…*.
2. **Sidecar startup**: The app spawns `orbit web serve` on a loopback
   address (`127.0.0.1`). It always starts its own server process rather than
   attaching to an existing port listener, ensuring that the dashboard runs
   against your active configuration and user registry.
3. **Loopback navigation**: As soon as the server announces that its listener
   is bound, the window navigates from the local status page to the dashboard.

All network requests from the dashboard remain strictly same-origin against the
loopback server. External URLs (such as documentation links or repository pull
requests) automatically open in your operating system's default web browser
rather than inside the desktop shell.

## Status screen and startup retry

If the desktop app cannot start the dashboard server (for example, if the Orbit
CLI is not found on your system or the configured `ORBIT_BIN` path is not
executable), the status screen transitions to an error state.

The status page reports:

- A clear summary of the startup error
- Remediation guidance (such as installing `@orbit-tools/cli` or setting
  `ORBIT_BIN`)
- A **Retry** button

Once you have installed the CLI or corrected the environment path, click
**Retry** to reattempt starting and binding the dashboard server. You can also
trigger a restart at any time from the tray menu.

### Server logs and diagnostics

The supervisor redirects server output to a dedicated log file:

- **File location**: `dashboard-server.log` in the app's log directory:
  `~/.local/share/works.constellation.orbit.desktop/logs/` on Linux or
  `~/Library/Logs/works.constellation.orbit.desktop/` on macOS.
- **Permissions**: Created with user-only permissions (`0600`) to protect
  workspace names, task IDs, and repository paths.
- **Log rotation**: Truncated automatically on app launch if the file size
  exceeds 8 MiB.

Inspect `dashboard-server.log` if the server fails to bind or exits
unexpectedly during startup.

## Closing versus quitting

The window behavior differs between platforms to fit desktop conventions:

- **macOS**: Closing the dashboard window (clicking the red traffic light or
  pressing `⌘W`) hides the window without stopping Orbit. The menu bar icon
  remains active, background fleet polling continues, and notifications still
  fire. To shut down the background server and completely exit Orbit, select
  **Quit Orbit** from the menu bar or press `⌘Q`.
- **Linux**: Closing the main window terminates the desktop process and cleanly
  shuts down the supervised `orbit web serve` sidecar process.

## System tray controls

The Orbit desktop app places an icon in your system tray (Linux) or menu bar
(macOS). The tray menu gives quick access to server controls, updates, and
unresolved fleet events:

| Menu Item | Action |
|---|---|
| **Status line** | Shows `Dashboard server not running` when the server is stopped, `Dashboard server not responding` when the server does not answer, otherwise drain and 24 h failure counts (for example, `2 drains running · 1 failure in 24 h`, with `Drains unavailable` or `failures unavailable` when that read failed). |
| **Attention items** | Up to 8 failed runs (`Failed: <title>`) and 8 review tasks (`Review: <title>`) needing action. Clicking an item opens the dashboard at that run or task. |
| **Open Orbit** | Restores and focuses the main dashboard window. |
| **Restart Dashboard Server** | Stops the running `orbit web serve` sidecar and starts a fresh server instance. |
| **Check for Updates…** / **Install Orbit X.Y.Z…** | Checks GitHub releases for verified updates. When an update is ready, clicking prompts for confirmation before downloading and installing. |
| **Launch at Login** | Checkable toggle that configures system startup items so Orbit launches automatically when you log into your desktop. |
| **Quit Orbit** | Gracefully shuts down the background dashboard server and exits the desktop application. |

### Application updates

The desktop app checks GitHub releases for newer versions one minute after
launch, and every 24 hours thereafter:

1. The app verifies the release's signed checksum manifest against built-in
   Orbit release public keys. Unverified manifests or modified packages are
   rejected immediately.
2. If an update is verified, a desktop notification is raised, and the tray menu
   item changes from *Check for Updates…* to *Install Orbit X.Y.Z…*.
3. Selecting the install item presents a confirmation dialog. Orbit never
   downloads or applies an update without your explicit confirmation.
4. You can manually check at any time by selecting *Check for Updates…* from
   the tray.

## Fleet monitoring and notifications

While running in the background, the desktop app acts as a fleet monitor:

- **Fleet polling**: Every 20 seconds, the watcher queries `api/hosts` on the
  local dashboard server. It then checks active drains, failed runs within the
  last 24 hours, and tasks in review across the local host and every registered
  remote machine (via `api/on/<host>/...`).
- **Failure deduplication**: When an activity fails, its leaf run, gate, and
  pipeline runs are grouped under the originating task. One task failure
  produces only one notification and one tray entry.
- **Remote host awareness**: When more than one host is registered in your
  Orbit fleet, notifications and attention labels explicitly name the host and
  workspace, keeping multi-machine activities clear.
- **Unreachable hosts**: If a remote host stops responding, the watcher retains
  its previous baseline rather than resetting counters to zero. When the host
  comes back online, old events are not re-announced as new notifications.
- **Interactive routing**: Clicking a task review or failed run notification
  (or clicking its row in the tray menu) opens the Orbit dashboard directly at
  the corresponding route:
  - Failed runs navigate to `#runs?run_id=<id>`
  - Tasks in review navigate to `#tasks?open=<id>`
