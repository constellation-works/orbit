## Browser checks on a prepared host

Browser checks need Playwright and Chromium, plus Chromium's shared libraries and a font configuration on Linux. A prepared host has a kit at `~/.local/chromium-deps`, built and verified by `orbit config browser-kit install` (`status`, `verify` and `remove` manage it; see the orbit-setup [browser kit reference](../../crates/orbit-core/assets/skills/orbit-setup/references/browser-checks.md)). Once it is verified, every agent receives `PLAYWRIGHT_BROWSERS_PATH`, `PLAYWRIGHT_MODULE` (Playwright's `index.mjs`) and `ORBIT_BROWSER_KIT_ENV`, the kit's `env.sh`, which on Linux also exports `LD_LIBRARY_PATH` and `FONTCONFIG_FILE` when the kit unpacked them. Source it in the shell that runs the check; in a hand-run shell, source `~/.local/chromium-deps/env.sh`. `orbit doctor` reports a missing, mismatched or unverified kit, unresolved libraries and `node` missing from workers' PATH. On a host without a kit, run `orbit config browser-kit install` there rather than installing Playwright into a checkout: `.orbit/tmp/` is cleared, so it holds only a sandboxed run's disposable copy when the kit is absent ([runbook](../runbooks/website-validation.md#stage-playwright-and-chromium-without-root)). Skia FontConfig crashes and missing `lib*.so` errors are host evidence, not a failed check.

```bash
. "${ORBIT_BROWSER_KIT_ENV:-$HOME/.local/chromium-deps/env.sh}"
```

`.orbit/tmp/` is cleaned with the worktree, so screenshots kept only there are lost. A task whose diff changes something a user sees (the dashboard, the website, docs pages, a desktop app UI) attaches screenshots of each changed view as task artifacts, whether or not it is tagged `ui`:

- every theme the view supports (the dashboard is dark-only; the website has light and dark), and a 375px phone width when the layout is responsive;
- a before shot from the base revision when it is practical and it shows the change;
- each attached with `orbit.task.artifact` action `put` from `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`) at a stable path such as `screenshots/<view>-<theme>-<width>.png`, and the paths listed in the execution summary;
- each under the 1 MiB artifact limit (`MAX_TASK_ARTIFACT_CONTENT_BYTES`): prefer a viewport capture to a full-page one, or JPEG or WebP when a PNG is too large.

When the host has no browser kit or the view cannot render, say so in the handoff instead of claiming visual verification. A claimed leaf on another host attaches the same way; the broker forwards the put to the owner.

### Website changelog validation

After changing the changelog renderer, run `npm ci`, `npm run build` and `npm run check` in `website/`, serve `website/dist`, then:

```bash
node website/scripts/check-changelog.mjs \
  "$PLAYWRIGHT_MODULE" http://localhost:4187 .orbit/tmp/changelog-browser
```

It checks release dates, merged-PR links, version and subsection anchors, keyboard and no-JavaScript disclosure controls, the version badge and future-dated headings, and records heights and screenshots at 1440px and 375px in both themes. Source `~/.local/chromium-deps/env.sh` for `PLAYWRIGHT_MODULE` and `PLAYWRIGHT_BROWSERS_PATH`. In a restricted executor, keep browser output, screenshots and JSON evidence under `.orbit/tmp/`; point transient `TMPDIR` at a sandbox-writable tmpfs when available. A minimal Linux runner may also need `FONTCONFIG_FILE` naming a config with local fonts.

## MCP Apps compatibility prototype

[The isolated reproduction and native desktop probe](../agents/records/qa/mcp-apps-probe.md) covers the Control Center, operator controls, rendered preview, routing boundaries and evidence template. Record automated protocol/bridge results and native desktop results separately.

## Desktop app

`desktop/` is the Tauri shell around the dashboard, a separate Cargo workspace (its own `[workspace]` table), not a member of the root one. The workspace gates (`make ci-fast`, `ci-test-affected`, `ci-lint`, `goldens`) never build it. Validate a change from `desktop/` with `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`, which drives the server supervisor against a fake `orbit` without a webview. Desktop uses the root `clippy.toml` and enforces source ordering with `-D warnings`. `cargo test --locked --no-default-features --test watcher_notifications` runs the polling/notification integration regressions alone; this target needs no native webview packages. Then run the app with `cargo run` against your installed `orbit`. `cargo clippy` doesn't rebuild the executable, so build before a manual run. [`desktop.yml`](../../.github/workflows/desktop.yml) runs the same checks, including the webview-free watcher regressions, on macOS and Linux. Its separate Linux advisory job checks the committed desktop lockfile without building the native stack. The workflow runs on changes to `desktop/`, the workflow, `clippy.toml`, `deny.toml`, the cargo-deny/download wrappers or the advisory regression; see [Supply-chain](toolchain.md#supply-chain-cargo-deny) for commands and exceptions. A manual run also bundles unsigned packages: a macOS disk image, and the Linux `.deb` and `.AppImage` from the same runners, targets and command as the release's Linux legs. Release tags build the signed bundles in `release.yml` ([Releases](../../desktop/README.md#releases)). Process lifecycle, binary discovery and the security model are in [`desktop/README.md`](../../desktop/README.md).
