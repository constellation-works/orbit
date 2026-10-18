# Orbit desktop

A native window for the Orbit dashboard, built with [Tauri 2](https://tauri.app) for macOS and Linux.

The app contains no copy of Orbit. When it opens, it starts `orbit web serve` from your installed
`orbit`, then loads the dashboard from that server's loopback address. The UI and API therefore
always match your CLI version. Switching hosts works the same as in the browser, through the
dashboard's own host switcher.

## How it works

- **Server.** The app runs `orbit web serve --port 0 --no-open --operator` with your home directory
  as the working directory. It reads the bound address from each startup line the server prints.
  `--operator` turns on the dashboard's governed controls, as `orbit web connect` does, because an
  app has no terminal to confirm them in.
- **Finding `orbit`.** Apps started from the Dock or a desktop launcher get a minimal `PATH`. The
  app looks in `ORBIT_BIN` (an explicit path that overrides the search), `$ORBIT_INSTALL_DIR`,
  `~/.orbit/bin`, `PATH`, `~/.cargo/bin`, `~/.local/bin`, npm and nvm global bins,
  `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`. The npm package's `orbit` command is a Node
  launcher, so the app runs the native binary inside the package instead.
- **Restarts.** If the server exits, the window returns to its status page and starts the server
  again. Repeated quick failures back off up to 30 seconds; **Retry** skips the wait.
- **Updates.** After `orbit update`, the server hands over to the new binary in the same process.
  It replays `--port 0`, so the new image can bind a different port; it prints its startup line
  again, and the window loads the new address and stops allowing the old one. If the handover fails
  and the server exits, the restart path above takes over.
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
format, Clippy and test checks on macOS and Linux whenever `desktop/` changes. A manual run of that
workflow also uploads unsigned bundles for testing. Releases are built separately, as
described in [Releases](#releases).

The server supervisor lives in the `orbit_desktop` library (`src/sidecar.rs`), apart from the
window, so `tests/sidecar_lifecycle.rs` can drive it without a webview. That test binary acts as
its own fake `orbit`: it re-executes itself on a new port in the same process, and the test checks
that the window follows and that quitting stops the server. Run it with `cargo test` from
`desktop/`.

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

## Releases

A release tag builds the app in [`release.yml`](../.github/workflows/release.yml) and attaches it to
the GitHub Release when the repository variable `ORBIT_DESKTOP_RELEASE` is `true`. Until then the
desktop jobs are skipped, and the CLI release is unaffected either way. The release attaches:

| Asset | Platform |
|---|---|
| `orbit-desktop-universal-apple-darwin.dmg` | macOS 11 or later, Apple silicon and Intel. Signed with a Developer ID certificate, notarized, and stapled. |
| `orbit-desktop-x86_64-unknown-linux-gnu.deb` / `.AppImage` | Linux x86_64, glibc 2.35 or later |
| `orbit-desktop-aarch64-unknown-linux-gnu.deb` / `.AppImage` | Linux arm64, glibc 2.35 or later |
| `orbit-desktop-checksums.txt` / `.sig` | SHA-256 of every desktop asset, signed with the Orbit release key |

The names don't change between versions, so
`https://github.com/constellation-works/orbit/releases/latest/download/<asset>` always points at
the newest stable build. The desktop jobs run after the CLI's `publish-release` job, so a desktop
failure never holds back a release. Re-run the failed job from Actions once it's fixed;
`publish-desktop` then adds the missing assets. It never overwrites an asset that's already
attached. `desktop/Cargo.toml`'s version must match the tag, and the build fails otherwise.
[RELEASING.md](../RELEASING.md#4-bump-versions) bumps it with the others.

To check a download, verify the manifest against the release key, then the files against the
manifest:

```bash
openssl dgst -sha256 -verify release-signing.pub -signature orbit-desktop-checksums.txt.sig orbit-desktop-checksums.txt
shasum -a 256 --check --ignore-missing orbit-desktop-checksums.txt
```

`release-signing.pub` is [`npm/release-signing.pub`](../npm/release-signing.pub) at the release
tag. On macOS, `spctl --assess --type execute --verbose /Applications/Orbit.app` should report
`source=Notarized Developer ID`.

### One-time setup

All of these steps are manual. Nothing in the repository creates or stores these credentials.

1. **Join the Apple Developer Program** (paid yearly) with the account that will own the app. An
   organization enrollment needs a D-U-N-S number and puts the organization's name on the
   signature. An individual enrollment puts the person's name there. Only the team's Account
   Holder can create Developer ID certificates.
2. **Create a Developer ID Application certificate.** Use Xcode → Settings → Accounts → Manage
   Certificates → **+** → *Developer ID Application*, or create one under Certificates on
   developer.apple.com from a Keychain Access signing request. Don't use *Apple Development* or
   *Apple Distribution*: those are for the App Store, and notarization rejects them.
3. **Export the certificate with its private key** from Keychain Access (My Certificates → right-click
   → Export) as a `.p12` with a strong password. Base64-encode it with
   `base64 -i developer-id.p12 | pbcopy`. Delete the `.p12` file once the secret is saved.
4. **Read the signing identity and team ID.** Run `security find-identity -v -p codesigning` to print
   the identity, `Developer ID Application: <name> (<team id>)`. The team ID is the ten
   characters in the parentheses. Membership details on developer.apple.com shows it too.
5. **Create an app-specific password** for the Apple ID at account.apple.com → Sign-In and Security
   → App-Specific Passwords. Notarization signs in with this password, never the account password.
   The Apple ID needs two-factor authentication.
6. **Add six Actions secrets** under Settings → Secrets and variables → Actions:

   | Secret | Value |
   |---|---|
   | `APPLE_CERTIFICATE` | The base64 `.p12` from step 3 |
   | `APPLE_CERTIFICATE_PASSWORD` | The `.p12` export password |
   | `APPLE_SIGNING_IDENTITY` | The full identity string from step 4 |
   | `APPLE_ID` | The Apple ID email that belongs to the team |
   | `APPLE_PASSWORD` | The app-specific password from step 5 |
   | `APPLE_TEAM_ID` | The ten-character team ID |

   The existing `ORBIT_RELEASE_SIGNING_KEY_PEM` secret also signs the desktop manifest. Linux needs
   no other secret.
7. **Turn the jobs on.** Add the repository variable `ORBIT_DESKTOP_RELEASE` = `true` on the
   Variables tab of the same page. The next release tag builds the app. If any Apple secret is
   missing, the macOS job fails and names it, so a release never ships an unsigned app.

Apple's first notarization of a new app can take hours, which is why the build job allows 180
minutes. Later releases usually finish in minutes. The certificate is valid for five years. When
you renew it, update `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` and
`APPLE_SIGNING_IDENTITY`. If you revoke the app-specific password, notarization fails until you
replace `APPLE_PASSWORD`.

### Auto-update

The app doesn't update itself. Install a new release over the old one. Most changes don't need
that anyway, because the dashboard comes from your `orbit` CLI, which `orbit update` upgrades. The
shell itself rarely changes.

[`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/) is left out on purpose because it
doesn't fit Orbit's release-integrity model:

- It only verifies minisign (Ed25519) signatures against one public key compiled into the app. The
  Orbit release key is RSA-3072, so the updater would need a second private key with its own
  custody, plus a fourth trust surface next to `install.sh`, the npm postinstall and
  `TRUSTED_RELEASE_KEYS`.
- That key has no `not_after` or `revoked_at` record. A leaked updater key can be revoked only by
  shipping a release signed with the leaked key.
- It updates the `.app` and AppImage only. `.deb` installs stay manual.
- It reads a `latest.json` that the release would have to publish. Any release without desktop
  assets, such as one made before `ORBIT_DESKTOP_RELEASE` is set, breaks the update check.

Add auto-update as its own change once the signed bundles are in use. Either adopt the updater and
add its key to the rotation runbook in [docs/runbooks/release.md](../docs/runbooks/release.md),
or have the app check `orbit-desktop-checksums.txt.sig` against the existing release trust set.

## Not done yet

- Auto-update (see above) and a Homebrew cask.
- A monochrome template icon for the macOS menu bar; the tray uses the app icon.
- Clicking a notification doesn't open the run or task; desktop notifications have no click action.
