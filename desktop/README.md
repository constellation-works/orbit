# Orbit desktop

A native window for the Orbit dashboard, built with [Tauri 2](https://tauri.app) for macOS and Linux.

The app contains no copy of Orbit. When it opens, it starts `orbit web serve` from your installed
`orbit`, then loads the dashboard from that server's loopback address. The UI and API therefore
always match your CLI version. Switching hosts works the same as in the browser, through the
dashboard's own host switcher.

## How it works

- **Server.** The app runs `orbit web serve --port 0 --no-open --operator` with your home directory
  as the working directory. It reads the bound address from the server's startup line. `--operator`
  turns on the dashboard's governed controls, as `orbit web connect` does, because an app has no
  terminal to confirm them in.
- **Finding `orbit`.** Apps started from the Dock or a desktop launcher get a minimal `PATH`. The
  app looks in `ORBIT_BIN` (an explicit path that overrides the search), `$ORBIT_INSTALL_DIR`,
  `~/.orbit/bin`, `PATH`, `~/.cargo/bin`, `~/.local/bin`, npm and nvm global bins,
  `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`. The npm package's `orbit` command is a Node
  launcher, so the app runs the native binary inside the package instead.
- **Restarts.** If the server exits, the window returns to its status page and starts the server
  again. Repeated quick failures back off up to 30 seconds; **Retry** skips the wait. A server that
  replaces itself after `orbit update` keeps its process and port, so the window doesn't notice.
- **Quitting.** Quitting the app, or sending it SIGTERM, SIGINT or SIGHUP, stops the server. The
  server gets up to 12 seconds to drain before it is killed. On Linux the kernel also stops the
  server if the app crashes. On macOS, a crash that skips Rust's panic hook (a SIGKILL, for example)
  can leave the server running until you stop it yourself.
- **Menu bar and tray.** The icon's menu shows how many drains are running and how many runs
  failed in the last 24 hours, with **Open Orbit**, **Restart Dashboard Server**, **Launch at Login**
  and **Quit Orbit**. On macOS the counts also appear beside the icon (`▶` drains, `✕` failed runs),
  and closing the window keeps the app in the menu bar; **Quit Orbit** or ⌘Q ends it. On Linux,
  closing the window quits, because a desktop without an AppIndicator host has no tray to return
  from. Restarting the server reloads the window once the new server is ready.
- **Notifications.** Every 20 seconds the app reads its own server's `/api/fleet/drains`,
  `/api/job-runs/all?state=failed&since=24h` and `/api/tasks/all?status=review`, which cover every
  workspace on this host. A run that newly appears as failed, or a task that newly appears in review,
  raises a native notification naming the task title (or the job) and its workspace. More than three
  at once arrive as one summary. The first read after launch only records what is already there,
  and a workspace that comes back after being unavailable doesn't re-announce its old failures.
  Other hosts' runs aren't watched; switch to them in the dashboard. A development build (`cargo
  run`) posts notifications as Terminal on macOS, because it has no app bundle of its own.
- **Launch at login.** The menu's checkbox adds or removes a login item: a LaunchAgent on macOS and
  an autostart `.desktop` entry on Linux. It opens the app normally, window included.
- **Logs.** Server output goes to `dashboard-server.log` in the app's log directory, readable only by
  you: `~/Library/Logs/works.constellation.orbit.desktop/` on macOS and
  `~/.local/share/works.constellation.orbit.desktop/logs/` on Linux. The log is truncated on launch
  once it passes 8 MiB.

## Security

The app always starts its own server. It never attaches to whatever already listens on port 7878:
on a shared host that could be another user's process, and a dashboard started with a different
`--root` would show the wrong registry.

The dashboard loads from its own `http://127.0.0.1:<port>` origin, so its Host/Origin checks apply
unchanged. To Tauri the dashboard is a remote origin: no capability grants it a command, and the
app's commands are declared in an app manifest, which puts them under the access-control list.
Only the bundled status page (`capabilities/status-page.json`) can call `status` and `retry`.
No page is granted the autostart plugin's commands; only the tray menu changes the login item.
Notifications come from Rust, not from Tauri's notification plugin: that plugin replaces
`window.Notification` in every page, the dashboard included, which would turn the dashboard's own
notification button into IPC calls the dashboard isn't allowed to make. The tray and notifications
read the server with plain loopback GETs; the dashboard page gains nothing. The
window navigates only to the status page and the running dashboard. Every other link, including
`target=_blank` links and `window.open`, opens in your default browser.

## Building

The shell is its own Cargo workspace (its `Cargo.toml` has its own `[workspace]` table), not a member of the root one, so `make ci` and the main CI
jobs never build it. [`.github/workflows/desktop.yml`](../.github/workflows/desktop.yml) runs
format and Clippy checks on macOS and Linux whenever `desktop/` changes. A manual run of that
workflow also uploads unsigned bundles.

Linux needs the WebKitGTK development packages:

```bash
sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev libxdo-dev libssl-dev patchelf
```

Run it in development (from `desktop/`):

```bash
cargo run
```

Build packages with the Tauri CLI (`cargo install tauri-cli --version 2.12.3 --locked`):

```bash
cargo tauri build
```

Bundles land in `target/release/bundle/`: `.app` and `.dmg` on macOS, `.deb` and `.AppImage` on
Linux. To regenerate the icons from `icons/app-icon.svg`, run
`cargo tauri icon icons/app-icon.svg -o icons`, then delete the Android, iOS and Windows Store
outputs.

## Not done yet

- Code signing and notarization for macOS, and attaching bundles to Orbit releases.
- Auto-update.
- A monochrome template icon for the macOS menu bar; the tray uses the app icon.
- Clicking a notification doesn't open the run or task; desktop notifications have no click action.
