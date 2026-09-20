# Vendored dashboard JavaScript

The dashboard self-hosts two third-party libraries as checked-in files
(`purify.min.js`, `marked.umd.js`). They are compiled into `orbit-web` and
served from `/static/` under the dashboard CSP (`script-src 'self'`).

| Record | Role |
| --- | --- |
| [`vendor-manifest.json`](vendor-manifest.json) | Library name, exact version, upstream URL, npm tarball, SHA-256 of the checked-in bytes, refresh command |
| [`package.json`](package.json) / [`package-lock.json`](package-lock.json) | npm identity so GitHub's dependency graph, Dependabot, and security alerts can see the pins |

Do not edit the minified blobs by hand.

## Refresh

1. Set new **exact** versions in `package.json` (`dependencies`; no `^` / `~`).
2. From the repository root run `./scripts/refresh-dashboard-vendor.sh`.
   That recopies the npm dist files named in the manifest, rewrites SHA-256
   digests and version fields, and refreshes the lockfile.
3. Confirm `./scripts/check-dashboard-vendor.py` exits 0.
4. Load the dashboard (`orbit web serve`) and check Markdown rendering.

`make ci-fast` runs the same digest/version check. A swapped blob or a
Dependabot version bump that does not refresh the copies fails CI.

## Advisories and new releases

`.github/dependabot.yml` includes an `npm` ecosystem on this directory,
targeting `agent-main`, on a weekly schedule. That is the release-watch:
Dependabot opens version PRs when npm publishes a new `dompurify` or
`marked`.

GitHub security alerts fire against `package-lock.json` for these two
packages. The workspace `dependabot-alert-sweep` job collects those alerts
and files remediation tasks. `cargo-deny` does not cover these files.
