# Browser kit for UI checks

Use this on a host whose workers change web pages (a dashboard, a docs site,
any UI) and must show the rendered result. Orbit gives agents no interactive
browser. A worker checks UI by scripting headless Chromium through Playwright:
it serves the page on loopback, drives and asserts it, and saves screenshots
under its worktree's `.orbit/tmp/`, which it can then open and look at.

Build the kit once per host, as the user that runs workers, outside every
repository. A worker cannot build it reliably itself. On Linux, Chromium's
system libraries need root (`playwright install-deps`), and the sandbox
forbids `sudo` (`no new privileges`). Downloading per run is also slow and
leaves each run on a different browser build.

## Layout

```text
~/.local/chromium-deps/
  env.sh        # source before a check
  kit/          # npm project holding the pinned playwright package
  browsers/     # PLAYWRIGHT_BROWSERS_PATH: chromium_headless_shell-<rev>, ffmpeg-<rev>
  root/         # Linux without root only: unpacked libraries and fonts
  debs/         # Linux without root only: the downloaded packages
  fonts.conf    # Linux without root only
  fontconfig-cache/
```

A check runs `. ~/.local/chromium-deps/env.sh` and then passes
`"$PLAYWRIGHT_MODULE"` to its script, which imports `chromium` from it. Pin one
exact Playwright version and use it on every host that runs browser checks, so
evidence from different hosts comes from the same browser build.

Both executor sandboxes accept this kit without policy changes. `linux-bwrap`
and macOS `sandbox-exec` allow host reads, network (loopback included), and
writes to `/tmp` and the worktree's `.orbit/tmp/`. `linux-bwrap` gives each run
its own empty `/tmp`, so keep evidence in `.orbit/tmp/`. macOS denies reads of
browser profile directories under `~/Library/Application Support` (Chrome,
Chromium, Brave, Firefox), so keep the kit out of them.

## macOS

macOS supplies Chromium's libraries and fonts. Only the module and the browser
are needed:

```bash
KIT="$HOME/.local/chromium-deps"
mkdir -p "$KIT/kit" "$KIT/browsers"
cd "$KIT/kit"
[ -f package.json ] || npm init -y >/dev/null
npm install --no-fund --no-audit playwright@<version>
PLAYWRIGHT_BROWSERS_PATH="$KIT/browsers" ./node_modules/.bin/playwright install --only-shell chromium
cat >"$KIT/env.sh" <<'EOF'
export PLAYWRIGHT_BROWSERS_PATH="$HOME/.local/chromium-deps/browsers"
export PLAYWRIGHT_MODULE="$HOME/.local/chromium-deps/kit/node_modules/playwright/index.mjs"
EOF
```

`--only-shell` installs the headless shell that `chromium.launch()` uses by
default and skips the full browser. Workers find `node` through the PATH they
inherit (the login shell for hand-started drains, the clock's environment
otherwise). Check that it resolves there, not only in your interactive shell.

## Linux

Install the module and browser exactly as on macOS. Then provide Chromium's
system libraries.

With root, run once on the host (never inside a worker):
`sudo "$KIT/kit/node_modules/.bin/playwright" install-deps chromium`. It
installs the distribution packages, including some only headed Chromium needs,
and `env.sh` stays the two macOS lines.

Without root (Debian or Ubuntu), unpack only the missing libraries into the
kit. List what the shell cannot resolve:

```bash
CHROME="$(find "$KIT/browsers" -type f -name chrome-headless-shell -perm -u+x -print -quit)"
export LD_LIBRARY_PATH="$KIT/root/usr/lib/x86_64-linux-gnu"
ldd "$CHROME" | grep 'not found'
```

Map each missing library to its package. `playwright install-deps --dry-run
chromium` prints the candidates for this release, and names carry release
suffixes such as `t64` (`libasound2t64`, `libatk1.0-0t64`). Then download and
unpack the packages as yourself:

```bash
mkdir -p "$KIT/debs" "$KIT/root"
cd "$KIT/debs"
apt-get download <packages>
for deb in ./*.deb; do dpkg-deb -x "$deb" "$KIT/root"; done
```

Repeat the `ldd` check until it prints nothing. Unpacked libraries pull in
their own dependencies. On another architecture, use the directory that
`find "$KIT/root" -type d -name '*linux-gnu*'` reports.

A server without fonts crashes Chromium in Skia/FontConfig. If `fc-list` is
missing or prints nothing, also unpack `fontconfig-config`, `libfontconfig1`
and `fonts-dejavu-core`, then write `$KIT/fonts.conf`:

```xml
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir>/home/<user>/.local/chromium-deps/root/usr/share/fonts</dir>
  <cachedir>/home/<user>/.local/chromium-deps/fontconfig-cache</cachedir>
  <include ignore_missing="yes">/home/<user>/.local/chromium-deps/root/etc/fonts/conf.d</include>
</fontconfig>
```

`env.sh` then exports two more variables before the Playwright pair:

```bash
export LD_LIBRARY_PATH="$HOME/.local/chromium-deps/root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export FONTCONFIG_FILE="$HOME/.local/chromium-deps/fonts.conf"
```

## Verify

First check the host. The snippet serves a page on loopback the way a real
check does:

```bash
. ~/.local/chromium-deps/env.sh
node --input-type=module -e '
import http from "node:http";
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE);
const server = http.createServer((_, res) => res.end("<h1>kit ok</h1>"));
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const browser = await chromium.launch();
const page = await browser.newPage();
await page.goto(`http://127.0.0.1:${server.address().port}/`);
console.log(await page.textContent("h1"), browser.version());
await page.screenshot({ path: "/tmp/kit-smoke.png" });
await browser.close(); server.close();'
```

It must print `kit ok` and a version, and the screenshot must show the
heading. A host pass does not prove the worker path. The decisive check is a
sandboxed worker run (a disposable task, or the first UI task) that sources
`env.sh`, runs a browser check and attaches its screenshot. `orbit agent
invoke` runs without the executor sandbox, so it is not that proof.

When a check fails, the cause is usually the kit, not the code under test. A
missing `lib*.so`, a Skia/FontConfig crash or `Executable doesn't exist`
(module and browser revisions out of step) is a host defect. Record it as
host evidence and fix the kit.

## Upgrade or remove

To move every host to a new version, install `playwright@<new>` in `kit/`, run
`playwright install --only-shell chromium` again with the same
`PLAYWRIGHT_BROWSERS_PATH`, and delete the old `chromium_headless_shell-*` and
`ffmpeg-*` directories. On Linux, rerun the `ldd` check, because a new build
can need another library. Remove the kit by deleting `~/.local/chromium-deps`.
Nothing else on the host refers to it.
