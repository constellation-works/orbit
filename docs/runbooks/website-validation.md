---
type: runbook
summary: Build, preview, and validate Orbit website changes with Playwright inside a job-run sandbox.
tags: [operations, website, sandbox, playwright, validation]
paths: ["website/**"]
related_features: [orbit-docs]
related_artifacts: ["ORB-11329", "ORB-11379"]
last_validated: 2026-09-06
---

# Validate Website Changes in a Job-Run Sandbox

Use this runbook when a website task needs rendered-page evidence from an Orbit job-run
sandbox, including a working `astro preview` and a Chromium launch.

## Prerequisites and safety

This procedure assumes a Debian- or Ubuntu-like host with `node`, `git`, `apt-get`,
`dpkg-deb`, and `ldd`. It does not require root. Replace `<task>` with a short unique
identifier, and set the repository path from the checkout in which the run is executing:

```bash
export REPO="$(git rev-parse --show-toplevel)"
export SANDBOX_ROOT="$(mktemp -d /tmp/orbit-website-<task>.XXXXXX)"
export npm_config_cache="$SANDBOX_ROOT/npm-cache"
mkdir -p "$npm_config_cache"
trap 'rm -rf "$SANDBOX_ROOT"' EXIT
```

Keep the Playwright package and browser, npm cache, downloaded `.deb` files, extracted
libraries, and logs under `SANDBOX_ROOT` or another `/tmp` directory. `npm ci` necessarily
populates `website/node_modules`; record whether that directory existed before the run and
remove it at handoff only when this run created it. The Playwright package and its browsers
must remain outside the worktree, so Playwright-specific handoff cleanup has nothing to undo.

## Establish the Node and npm environment

Do not assume the Node version from another host. Discover the binaries available to this
run and use the npm installation from the same Node toolchain when possible:

```bash
NODE_BIN="$(command -v node)"
NODE_DIR="$(dirname "$(readlink -f "$NODE_BIN")")"
if [ -x "$NODE_DIR/npm" ]; then
  NPM_BIN="$NODE_DIR/npm"
else
  NPM_BIN="$(find "$HOME/.nvm/versions/node" -maxdepth 4 -path '*/bin/npm' -print -quit 2>/dev/null || true)"
fi
test -n "$NPM_BIN" && test -x "$NPM_BIN"
export PATH="$(dirname "$NPM_BIN"):$PATH"
command -v node
command -v npm
node --version
npm --version
```

The dispatched shell can start in the repository root even when a previous command changed
directories. Use `--prefix` on every npm command so the target is unambiguous. The writable
cache is also required because the sandbox may not permit npm's default `~/.npm/_logs` path:

```bash
npm --prefix "$REPO/website" ci
```

Check the output, not only the runner status. A background command can report exit code 0
while its output contains `npm: command not found` (for example, the shell may have launched
the background wrapper successfully). A safe background form records and checks both:

```bash
LOG="$SANDBOX_ROOT/npm-ci.log"
npm --prefix "$REPO/website" ci >"$LOG" 2>&1 &
NPM_PID=$!
wait "$NPM_PID"
NPM_STATUS=$?
if [ "$NPM_STATUS" -ne 0 ] || grep -Eq 'npm: command not found|command not found: npm' "$LOG"; then
  sed -n '1,160p' "$LOG" >&2
  exit 1
fi
```

## Stage Playwright and Chromium without root

Install Playwright and its browser outside the worktree. `playwright install-deps` is not a
solution in a job-run sandbox: it installs operating-system packages and therefore needs
root. `sudo` cannot elevate when the sandbox has Linux `no-new-privs` enabled; it fails with
`The 'no new privileges' flag is set, which prevents sudo from running as root.`

```bash
export PLAYWRIGHT_ROOT="$SANDBOX_ROOT/playwright"
export PLAYWRIGHT_BROWSERS_PATH="$PLAYWRIGHT_ROOT/browsers"
mkdir -p "$PLAYWRIGHT_ROOT" "$PLAYWRIGHT_BROWSERS_PATH"
npm --prefix "$PLAYWRIGHT_ROOT" install --no-save playwright
PLAYWRIGHT="$PLAYWRIGHT_ROOT/node_modules/.bin/playwright"
test -x "$PLAYWRIGHT"
"$PLAYWRIGHT" install chromium
```

The browser download can succeed while Chromium still cannot start if shared libraries are
missing. Find the downloaded `chrome-headless-shell` and inspect all unresolved libraries:

```bash
CHROME="$(find "$PLAYWRIGHT_BROWSERS_PATH" -type f -name chrome-headless-shell -perm -u+x -print -quit)"
test -n "$CHROME"
ldd "$CHROME" | grep 'not found' || true
```

On the affected Ubuntu host, the existing image provided `libnss3`, `libcups`, `libgbm`, and
`libpango`, while five runtime libraries (including GTK, ATK, X11, and audio libraries) were
absent. Package names have release-specific
suffixes (`-t64` on newer Ubuntu), so resolve candidates on the actual host rather than
copying a fixed package list:

```bash
apt-cache policy \
  libatk1.0-0 libatk1.0-0t64 \
  libatk-bridge2.0-0 libatk-bridge2.0-0t64 \
  libatspi2.0-0 libatspi2.0-0t64 \
  libgtk-3-0 libgtk-3-0t64 libxcomposite1
```

Select the available package for each library reported by `ldd`, download the packages as an
unprivileged user, and unpack them into a temporary prefix. `apt-get download` writes files
where it is run and does not install them system-wide:

```bash
export DEB_ROOT="$SANDBOX_ROOT/debs"
export LIB_ROOT="$SANDBOX_ROOT/libs"
mkdir -p "$DEB_ROOT" "$LIB_ROOT"
cd "$DEB_ROOT"
apt-get download <package-for-libatk> <package-for-libatk-bridge> <package-for-libatspi> <package-for-libgtk> <package-for-libxcomposite>
for deb in ./*.deb; do
  dpkg-deb -x "$deb" "$LIB_ROOT"
done
export LD_LIBRARY_PATH="$LIB_ROOT/usr/lib/x86_64-linux-gnu:$LIB_ROOT/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH:-}"
```

Run `ldd "$CHROME" | grep 'not found'` again after every package batch. Repeat the download,
unpack, and `LD_LIBRARY_PATH` update until it prints nothing; the initial launch error often
names only the first missing library. If the host uses a different architecture, replace the
library directory with the directory shown by `find "$LIB_ROOT" -type d -name '*linux-gnu*'`.

## Build, preview, and collect evidence

Build before previewing, then bind the preview to loopback on a known port. Keep the preview
PID so it can be stopped before handoff:

```bash
npm --prefix "$REPO/website" run build
export PREVIEW_PORT=4321
npm --prefix "$REPO/website" run preview -- --host 127.0.0.1 --port "$PREVIEW_PORT" \
  >"$SANDBOX_ROOT/astro-preview.log" 2>&1 &
PREVIEW_PID=$!
export PREVIEW_URL="http://127.0.0.1:$PREVIEW_PORT"
curl --fail --silent --show-error "$PREVIEW_URL/" >/dev/null
```

For security.txt changes, validate both the source and generated static asset:

```bash
npm --prefix "$REPO/website" run validate:security-txt -- public/.well-known/security.txt
npm --prefix "$REPO/website" run validate:security-txt -- dist/.well-known/security.txt
```

The validator rejects missing or malformed RFC 9116 fields, invalid or expired
`Expires`, non-HTTPS `Contact`/`Policy` URIs, an incorrect `Canonical`, invalid
UTF-8, and HTML fallback content. The Orbit maintainers own renewal: review the
file before its `Expires` timestamp and renew it annually. A local build proves
only that the asset is packaged; it does not prove public publication.

Launch Chromium through the staged Playwright package at both representative desktop and
mobile sizes. The following smoke check records HTTP status, heading, rendered text length,
console/page errors, and horizontal overflow; extend `PAGES` with the routes changed by the
task:

```bash
PAGES=(/ /getting-started/install/ /how-to/task-lifecycle/ /reference/cli/)
node --input-type=module - "$PLAYWRIGHT_ROOT/node_modules/playwright/index.mjs" "$PREVIEW_URL" "${PAGES[@]}" <<'NODE'
const [playwrightEntry, baseUrl, ...pages] = process.argv.slice(2);
const { chromium } = await import(`file://${playwrightEntry}`);
const browser = await chromium.launch({ headless: true });
const failures = [];
for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
  const page = await browser.newPage({ viewport });
  page.on('console', message => console.log(JSON.stringify({ type: 'console', viewport, level: message.type(), text: message.text() })));
  page.on('pageerror', error => failures.push({ type: 'pageerror', viewport, text: error.message }));
  for (const route of pages) {
    const response = await page.goto(`${baseUrl}${route}`, { waitUntil: 'networkidle' });
    const result = await page.evaluate(() => ({
      h1: document.querySelector('h1')?.textContent?.trim() ?? null,
      textLength: document.body.innerText.length,
      scrollWidth: document.documentElement.scrollWidth,
      viewportWidth: window.innerWidth,
    }));
    console.log(JSON.stringify({ viewport, route, status: response?.status() ?? null, ...result }));
    if (!response || response.status() >= 400 || result.scrollWidth > result.viewportWidth) failures.push({ type: 'page-check', viewport, route, status: response?.status(), ...result });
  }
  await page.close();
}
await browser.close();
if (failures.length) { console.error(JSON.stringify({ failures }, null, 2)); process.exit(1); }
NODE
```

For changes involving search or navigation, add checks for the live Pagefind UI and the
documentation journeys affected by the change. For example, enter a query in the search
control and assert that a result is visible, then click the links for the relevant
getting-started or how-to journey and assert each destination response is below 400. Save the
command output and the preview log with the job evidence; do not copy either into the
worktree.

Stop the server after validation:

```bash
kill "$PREVIEW_PID" 2>/dev/null || true
wait "$PREVIEW_PID" 2>/dev/null || true
```

## Production publication and evidence

The supported production path is the repository's `Website` GitHub Actions
workflow. Pull requests run only the `Check and build` job. Publication is
release-gated: a `main` push that changes `website/**` or the workflow launches
the distinct `Publish to Cloudflare Pages` job. A maintainer may also dispatch
the workflow against `main` to recover from a failed or missed publication.
Selecting another ref builds it but cannot enter the production job.

The checked-in and public evidence identifies Cloudflare as the intended
publication boundary, but does not expose the current origin project. Public
responses from `orbit-cli.com` and its authoritative nameservers are
Cloudflare. Repository history previously used Wrangler direct upload, and
`website/wrangler.toml` configures Pages static output. However, every one of
the 26 public GitHub Deployment records from that workflow ended in failure.
The former hard-coded project name is not trustworthy:
`https://orbit-website.pages.dev` served an unrelated social-video site when
ORB-11379 was investigated. The repaired workflow therefore obtains the exact
existing project name from the protected `production` environment instead of
guessing it. This evidence supports the repository mechanism; it does not prove
who currently owns the external account, which project owns the custom domain,
or that repository secrets exist. Do not create a project, rotate or invent
credentials, or edit DNS as part of website publication.

Wrangler's Pages configuration validation still requires a top-level `name`, so
`website/wrangler.toml` keeps `name = "orbit-website"`. Treat that value as a
validation placeholder rather than the deployment target: the publish job always
passes `--project-name` from the protected `CLOUDFLARE_PAGES_PROJECT` variable,
which overrides the configured name, so the project still comes from the
environment instead of the repository. Deleting the field to avoid restating an
untrusted name breaks publication rather than hardening it. ORB-11379 removed it
in commit `86d48ebd212a9a7d6f2f36ae25312f6e81e11105` (PR #1425), and `main`
publication then failed Pages configuration validation with `Missing top-level
field "name" in configuration file` until ORB-11511 restored it.

ORB-11379 recorded the publication gap on 2026-09-06:

- `.github/workflows/website.yml` had only a pull-request build trigger and no
  upload step.
- GitHub's newest deployment record was production deployment `4604816061` for
  `agent-main` revision `ec8545b4de696a5b714fe344c9d2d1bca9d21f01`.
  Its build step succeeded, its Cloudflare Pages upload step failed, and its
  workflow/job log is
  `https://github.com/danieljhkim/orbit/actions/runs/25479096320/job/74759046156`.
- The separate deploy workflow that created that record was deleted immediately
  afterward in `fc2ae9d4ef26c92d0e10dc4aff63934d3c007b09`.
- The live homepage still contained the old `v0.9.2` headline and Architecture
  navigation. Successful Website checks on newer commits therefore proved
  buildability, not publication.

The repaired workflow uploads one immutable build artifact and writes
`deployment.json` into it with the full source revision, source ref, and Actions
run URL. Wrangler receives the same revision through `--commit-hash`. Production
deployments are serialized, use only `contents: read` and `deployments: write`,
and are recorded in both the Actions run and GitHub Deployments. The final step
checks the unique homepage headline, the delivery-mode setup explorer, the
install route, and the expected source revision at both the deployment URL and
`orbit-cli.com`. It also checks the Pages `_headers` artifact during build, then
checks the deployed custom domain for `Strict-Transport-Security:
max-age=31536000` on homepage and install-route 200 responses and on a 404
response. Finally, it checks that `http://orbit-cli.com/` redirects to the
canonical HTTPS domain.

The static `website/public/_headers` file is the sole repository-owned HSTS
policy. Its one-year max-age intentionally does not use `includeSubDomains` or
`preload`; the repository has not established that every subdomain is HTTPS-ready
and under compatible operational ownership. HTTP redirect behavior belongs to the
externally managed Cloudflare zone rather than the Pages artifact. Treat a failed
redirect check as an external-zone configuration issue, not a reason to add a
second redirect mechanism to the site.

### Operate and recover

1. Open the `Website` workflow run for the `main` revision. Confirm `Check and
   build` completed before interpreting `Publish to Cloudflare Pages`.
2. Follow the failed step and GitHub Deployment links. A green build with a red
   or absent publication job is not a deployed site.
3. Before the first repaired publication, an authorized Cloudflare account
   owner must identify the existing Pages project whose custom domains include
   `orbit-cli.com`. Set that exact name as the production environment variable
   `CLOUDFLARE_PAGES_PROJECT`. If authentication fails, the owner must restore
   `CLOUDFLARE_ACCOUNT_ID` and a `CLOUDFLARE_API_TOKEN` scoped to Pages edit
   access for that same project. Record the external action; do not replace the
   project or DNS.
4. After correcting an external cause, dispatch `Website` from the `main` ref.
   This rebuilds tracked sources and uploads them; no manual or untracked source
   upload is part of recovery.
5. Require the verification step to pass, then independently open
   `https://orbit-cli.com/`, its delivery-mode explorer, and
   `https://orbit-cli.com/getting-started/install/`. Fetch
   `https://orbit-cli.com/deployment.json` and compare `sourceRevision` with the
   workflow's `github.sha` before declaring publication successful.

For the security document, independently check the canonical endpoint after the
production workflow succeeds:

```bash
curl --fail --show-error --silent --dump-header /tmp/security-txt.headers \
  https://orbit-cli.com/.well-known/security.txt
grep -Eiq '^content-type:[[:space:]]*text/plain(?:;|$)' /tmp/security-txt.headers
```

Then rerun the security scan. A 404, HTML response, stale `Expires`, or failed
scan remains an external publication issue until the exact `main` artifact is
published and verified. The production workflow performs the same status,
`text/plain`, and body validation against both the deployment URL and the
custom domain. Do not treat the local `dist/.well-known/security.txt` check as
public deployment evidence.

If the workflow has not yet reached `main`, the exact existing project mapping
has not been confirmed, or the environment credentials are missing or invalid,
repository-side validation can still succeed but a real publication is
externally blocked. Report the precise missing promotion, environment
ownership, project mapping, or credential repair and never claim that the live
site was updated.

## Preferred long-term fix

Pre-stage the missing GTK/ATK/X11 libraries in the job-run host image. That is more reliable
and faster than downloading and unpacking `.deb` files for every run. Host provisioning is
out of repository scope for this runbook; coordinate it with the sandbox image owner rather
than adding privileged installation commands here.

## Handoff and escalation

Playwright, Chromium, npm's cache, downloaded packages, extracted libraries, logs, and any
temporary validation scripts all live outside the worktree under `SANDBOX_ROOT` and are
removed by the trap. Therefore handoff cleanup has nothing to undo in the repository. Before
handoff, confirm the worktree contains only the intended website changes and report the
preview URL, routes, viewport results, browser launch result, and any console/page errors in
the task execution summary. If Chromium still fails, retain the exact `ldd ... | grep 'not
found'` output and the host package candidates for escalation.

## Related references

- [Runbook conventions](./CONVENTIONS.md)
- [Prepare a Linux host for sandboxed dispatch](./linux-sandbox.md)
- [Orbit website README](../../website/README.md)
