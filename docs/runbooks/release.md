---
type: runbook
summary: Cut and verify an Orbit release across agent plugins, Cargo, GitHub artifacts, Homebrew, npm, and the human Cursor marketplace follow-up.
tags: [operations, release, plugins, npm, signing]
paths: [".github/workflows/release.yml", "plugin/**", "npm/**", "scripts/release-check.sh"]
related_features: [orbit-docs-plugin]
last_validated: 2026-10-14
---

# Release Orbit

This runbook keeps the Cargo workspace version, Claude/Codex/Cursor plugin
manifests, the `@orbit-tools/cli` npm proxy, and the GitHub Release tag in
lockstep. The npm package downloads the
native binary for its own version during postinstall, so version drift either
installs the wrong release or fails the download.

See [RELEASING.md](../../RELEASING.md) for the higher-level release checklist,
branch promotion policy, and CHANGELOG process.

## Account setup (one-time)

The `@orbit-tools` scope has publish-time 2FA enabled, so npm releases are
published manually from a maintainer's machine and prompt for an OTP. No
`NPM_TOKEN` secret is needed in this repository.

GitHub Releases require `ORBIT_RELEASE_SIGNING_KEY_PEM`, a PEM-encoded private
key whose public half matches
[`npm/release-signing.pub`](../../npm/release-signing.pub). The release
workflow signs `orbit-checksums.txt` as `orbit-checksums.txt.sig`;
`install.sh`, the npm postinstall, and `orbit update` authenticate
that signature before trusting release-hosted SHA-256 values. The same key
signs `orbit-desktop-checksums.txt` for the desktop app, which the app's update
check verifies before it offers an update
([desktop/README.md](../../desktop/README.md#auto-update)). The app carries its
own copy of the Rust trust set, so it is the fourth trust set in the rotation
below.

The desktop release jobs run on every release tag unless the repository
variable `ORBIT_DESKTOP_RELEASE` is `false`. The Linux bundles need no other
secret. The macOS bundle needs six Apple signing and notarization secrets.
Without all six, the release skips only macOS and warns which secrets are
missing. The account steps and the secret list are in
[desktop/README.md](../../desktop/README.md#one-time-setup).

All four shipped release-signing trust sets contain only `orbit-release-key-3`,
valid through `2029-12-31` and not revoked: [`install.sh`](../../install.sh),
[`npm/scripts/install-binary.js`](../../npm/scripts/install-binary.js),
[`orbit-common::security::release::TRUSTED_RELEASE_KEYS`](../../crates/orbit-common/src/security/release.rs)
(used by `orbit update` and bundled Bubblewrap verification), and the desktop
app's copy in [`desktop/src/update/trust.rs`](../../desktop/src/update/trust.rs).
[`scripts/check-installer-pubkey.sh`](../../scripts/check-installer-pubkey.sh)
fails CI when any of them disagree. No successor is pre-staged. Ship successor
trust in all four surfaces before signing releases with that key.

Key IDs are generation labels, not dates. During verification the installers
try each known public key, then reject a matching key when its `not_after`
date has passed or its `revoked_at` field is set. The shell and npm installers
validate every record's non-empty `not_after` and `revoked_at` as exactly
`YYYY-MM-DD` before accepting any signature. A malformed date aborts installation;
an empty date remains optional.

> **Operator custody requirement.** Keep the successor private key in custody
> independent from the primary. Storing both private halves together defeats
> the overlap window's protection.

## Steps to cut a release

1. **Bump Cargo, npm, and MCP Registry metadata together.**

   - Update `[workspace.package].version` in
     [`Cargo.toml`](../../Cargo.toml).
   - Update `.version` in
     [`npm/package.json`](../../npm/package.json). The npm postinstall in
     [`npm/scripts/install-binary.js`](../../npm/scripts/install-binary.js)
     derives the GitHub binary tag from this version.
   - Update the top-level `.version` and `.packages[0].version` in
     [`server.json`](../../server.json). The registry metadata must reference
     the npm package version that contains its matching `mcpName` ownership
     marker; published npm versions are immutable.
   - Update `.version` in `plugin/.claude-plugin/plugin.json`,
     `plugin/.codex-plugin/plugin.json`, and `plugin/plugin.json`.
   - Pin MCP launch args in `plugin/mcp.json`, `plugin/.mcp.json`, and
     `plugin/.codex-plugin/plugin.json` to
     `npx -y @orbit-tools/cli@<version> mcp serve`. Do not use `@latest`.
   - Run `cargo update --workspace` to refresh `Cargo.lock` without
     third-party dependency drift.
   - Update `[package].version` in
     [`desktop/Cargo.toml`](../../desktop/Cargo.toml) and run
     `cargo update --workspace --manifest-path desktop/Cargo.toml`. The
     desktop build fails when this version doesn't match the tag.

   For a major version bump, first follow
   [RELEASING.md's CHANGELOG archiving policy](../../RELEASING.md#changelog-archiving).

2. **Sync and verify plugin assets.** `crates/orbit-core/assets/skills/orbit/`
   is canonical. Run `scripts/sync-plugin-skills.sh` to refresh the committed
   package mirror. CI runs it with `--check` and rejects drift.

3. **Run the install smokes.** `scripts/smoke-plugin-install.sh` installs the
   repository plugin into scratch Claude Code and Codex config roots and checks
   their MCP and skill assets. Cursor has no supported headless plugin runner,
   so its leg creates the documented local symlink and validates the Agent
   Plugins 1.0 root manifests. npm download/signature coverage stays solely in
   `scripts/smoke-npm-install.sh`.

4. **Run `make release-check`.** Before the new npm package and GitHub Release
   exist, this normally reports only local-to-remote drift against the previous
   version. Any Cargo, `npm/package.json`, or `server.json` mismatch is a local
   error and must be fixed before tagging.

5. **Keep the npm smoke current.** If this release changes any non-interactive
   command driven by
   [`scripts/smoke-npm-install.sh`](../../scripts/smoke-npm-install.sh)
   (`orbit init`, `workspace init`, or `mcp serve`), land the script
   update on the same commit as the tag.

6. **Commit and merge the release preparation to `agent-main`.** Keep the
   Cargo, npm, and `server.json` version bumps, lockfile refresh, and any smoke
   update together.

7. **Tag the merge commit.**

   ```bash
   git tag -a vX.Y.Z -m "orbit vX.Y.Z"
   git push origin vX.Y.Z
   ```

   The metadata step accepts only `vMAJOR.MINOR.PATCH`, optionally followed
   by a prerelease suffix such as `-rc.1`; it rejects other tags before
   writing release outputs. A prerelease tag creates a GitHub Release marked
   as a prerelease and skips the Homebrew tap update; stable tags update the
   tap. Homebrew formula inputs pass through environment
   variables and are validated before becoming Ruby literals. The tap checkout
   disables credential persistence, and only the push command receives the
   token's authorization header. `make ci-fast` checks this job for expressions
   inside shell scripts and runs offline release-workflow regression fixtures.

8. **Watch [`.github/workflows/release.yml`](../../.github/workflows/release.yml).**
   Its jobs:

   - build four platform CLI tarballs;
   - generate and sign the combined checksum manifest, then create the GitHub
     Release;
   - build the static bundled Bubblewrap that the release publishes for Linux
     hosts whose own `bwrap` is missing or lacks `--bind-fd` (see
     [linux-sandbox.md](linux-sandbox.md));
   - update the Homebrew tap;
   - smoke the tagged shell installer and search help on macOS and Ubuntu;
   - smoke the bundled-Bubblewrap fallback on Ubuntu 22.04;
   - unless `ORBIT_DESKTOP_RELEASE` is `false`, build the Linux `.deb` and
     `.AppImage` for x86_64 and arm64 and, when all six Apple secrets are
     present, the signed and notarized universal macOS `.dmg`, then attach
     exactly the bundles built with `orbit-desktop-version.txt` and their
     signed `orbit-desktop-checksums.txt`. `desktop-preflight` decides which
     legs run. A missing secret skips only macOS, with a warning naming it.
     A failed Linux leg holds back the desktop assets until it's fixed and
     re-run. These jobs run beside the CLI release and never block it.

   Review the result of every job, but treat CI as informational on
   `agent-main`: no job is a merge gate. Failures are queued for asynchronous
   remediation by the `qa-sweep` auto-task. Then
   follow [RELEASING.md §10b](../../RELEASING.md#10b-promote-to-main) to
   fast-forward `main` to the tagged release commit (not the
   moving `agent-main` tip), and
   [§10c](../../RELEASING.md#10c-confirm-main-and-agent-main-agree) to
   confirm the branches agree in the same session.

9. **Publish npm manually** from the merged commit:

   ```bash
   cd npm
   npm publish --access public
   # Enter the OTP from your authenticator when prompted.
   ```

   Manual publication cannot use GitHub OIDC provenance. Keep the interval
   between the GitHub Release and npm publish short; plugin MCP configs pin
   `@orbit-tools/cli@<version>`, so a marketplace or plugin install before npm
   publish cannot fetch that exact package.

10. **Verify after npm publish.**

   ```bash
   make release-check
   ./scripts/smoke-npm-install.sh
   ```

   Re-run
   [`.github/workflows/smoke-npm-install.yml`](../../.github/workflows/smoke-npm-install.yml)
   with the release tag in its `tag` input. The tag-triggered run often starts
   before npm publish and is expected to fail its version assertion; the
   post-publish versioned run must be green.

11. **Submit the Cursor marketplace listing manually.** See
    [Cursor marketplace listing](#cursor-marketplace-listing).

## Desktop backfill

A release whose desktop jobs were skipped or never ran (v0.29.0 shipped
without desktop bundles) can get them later without a new tag. In Actions,
open the `release` workflow and choose **Run workflow** on a branch that
carries the current workflow, with `tag` set to the existing release tag. The
manual run:

- runs only `desktop-preflight`, `build-desktop` and `publish-desktop`, never
  a CLI build, the Homebrew tap or the installer smokes;
- refuses a tag that isn't `vMAJOR.MINOR.PATCH` or has no GitHub Release;
- checks out the tag's source and builds it with the current workflow, so the
  tag's `desktop/Cargo.toml` version must still match the tag;
- ignores `ORBIT_DESKTOP_RELEASE`, since the run is itself the request;
- adds macOS only when all six Apple secrets are present, as a tag push does;
- attaches missing assets under the same rules as a release: published bytes
  are never replaced, and a published manifest that doesn't match the
  available bundles stops it before any upload.

A release that already carries a Linux-only manifest can't gain a macOS bundle
this way. That is a manifest conflict by design: fix forward, and macOS ships
in the next release.

A backfill, a tag-push publish, and a rerun for the same tag attach assets one
at a time. The publisher already uploading finishes, and the others wait. The
waiting run then sees the finished manifest, so a full plan that follows a
Linux-only publish stops before uploading the macOS disk image. A different
tag publishes on its own. Each waiting attempt stays queued until the active
publisher finishes.

Running a backfill against a real release is an operator decision. Check the
release's assets afterwards with the verification commands in
[desktop/README.md](../../desktop/README.md#releases).

## Cursor marketplace listing

Cursor's curated catalog is human-reviewed. The tag workflow, GitHub Release,
Homebrew tap, and npm publish do not submit or update the listing.

Required procedure after the git tag and matching `@orbit-tools/cli`
version exist:

1. Submit or update [https://cursor.com/marketplace/publish](https://cursor.com/marketplace/publish)
   using [https://github.com/constellation-works/orbit](https://github.com/constellation-works/orbit)
   and the `plugin/` subdirectory (Agent Plugins 1.0 `plugin.json` and
   `mcp.json` at the plugin root). Do not add `plugin/.cursor-plugin`.
2. Identify stale listing **2280865**, which still installs/describes
   `danieljhkim/orbit` at **0.5.1**. Leave that historical provenance in place;
   active distribution ownership is `constellation-works`.
3. Install through Cursor plugin search (not only a local
   `~/.cursor/plugins/local/orbit` symlink) and verify the listed version plus
   MCP surface match this release:
   `npx -y @orbit-tools/cli@<version> mcp serve`.
4. If review stalls, escalate to `marketplace-publishing@cursor.com`. Do not
   send that mail from CI or assume a public catalog push API exists.

## Publish the Official MCP Registry record

After the GitHub Release and its matching `@orbit-tools/cli` version are
public, a repository maintainer may manually dispatch the
`publish-mcp-registry` workflow. It never runs from a push or tag event.

1. Confirm the release tag is published and non-draft, and that its
   `server.json`, `npm/package.json`, and public npm metadata all identify
   `io.github.constellation-works/orbit`, `@orbit-tools/cli`, and the tag
   version. The registry command remains exactly `mcp serve`; it must not
   include `--operator`.
2. In GitHub Actions, select **publish-mcp-registry**, choose a trusted
   repository ref containing the workflow, and supply the explicit release tag
   (for example, `v0.19.0`). The workflow resolves that tag to a commit after
   checking its published, non-draft GitHub Release, then checks out that
   commit. It does not check out or publish an arbitrary input ref.
3. Review the public readback step. It verifies the registry name, version,
   npm package, and fixed `mcp serve` package arguments after publishing.

The workflow's only credential is the repository Actions secret
`MCP_REGISTRY_PAT_TOKEN`. Keep it dedicated to this purpose: it is used only
for the GitHub membership preflight and the official `mcp-publisher login
github --token` exchange. Do not substitute a personal `gh` credential or the
Homebrew TAP token, and never paste the PAT into workflow inputs or logs.

The preflight calls GitHub's active organization-memberships endpoint and
requires the selected `constellation-works` membership to be `active` with
role `admin` (GitHub's API value for an organization administrator). Configure
the PAT with read-only GitHub access sufficient to
read that organization membership: for a fine-grained PAT, select the
`constellation-works` organization and grant **Organization permissions →
Members: Read-only**. It needs no write, repository-content, or TAP access;
the workflow uses its read-only `GITHUB_TOKEN` to inspect the public release.
Do not broaden the PAT automatically if the preflight fails. A `missing` or
non-admin result means an organization administrator must correct the dedicated
token's grant before another manual dispatch. The workflow prints only the
selected membership state and role, never tokens, authorization headers, raw
JWTs, or raw authentication responses.

The workflow downloads `mcp-publisher` v1.8.1 for Linux amd64 and verifies its
SHA-256 before validating metadata, authenticating, and publishing. It is safe
to rerun only after resolving a failed preflight or public-readback error; npm
versions and release tags are immutable.

## Continuous npm-install verification

The `smoke-npm-install.yml` workflow runs on macOS and Ubuntu weekly, on every
`v*` tag, and by manual dispatch. It resolves the package and exact version from
the checked-out `server.json`, then executes that version through `npx`, thereby
exercising the npm postinstall binary download and checksum/signature
verification. It then initializes an isolated Orbit home and workspace and
drives the metadata-declared `mcp serve` command through JSON-RPC `initialize`
and `tools/list`.

The smoke runs against published artifacts, not the local package. Its pass
criterion is that the installed version matches the requested tag when one is
present and that the MCP response advertises Orbit tools. Windows is not
covered; the npm proxy publishes only macOS and Linux packages.

The tag assertion can be tested without network access:

```bash
./scripts/smoke-npm-install.sh --dry-run-version-assertion
```

## Npm-install smoke: two artifacts

The workflow checks out `scripts/smoke-npm-install.sh` and `server.json` from the
trigger ref, while the script installs the exact package version declared by
that metadata from npm. The checked-out release contract and published package
are independently available until npm publication completes.

| Trigger | Script and metadata come from | CLI comes from |
|---|---|---|
| push of tag `vX.Y.Z` | that tag | npm version declared by that tag's `server.json` |
| `workflow_dispatch` | the selected ref | npm version declared by that ref's `server.json` |
| weekly cron | the default branch | npm version declared by that branch's `server.json` |

A green versioned smoke needs the metadata-declared npm version to be published
and a script on the selected ref that speaks the CLI's current non-interactive
contract.

If the post-publish smoke is red:

1. Confirm `npm view @orbit-tools/cli version` equals the tag without `v`.
2. Read the failing step and the captured stdout/stderr.
3. Classify the failure:
   - A script-only contract issue is fixed on `agent-main`; dispatch that ref.
     Do not retag or publish a second npm version.
   - A bad published binary, package, or release asset requires the next patch
     release. Tags and npm versions are immutable.
4. A maintainer with Actions write permission can dispatch:

   ```bash
   gh workflow run smoke-npm-install.yml --ref agent-main
   gh workflow run smoke-npm-install.yml --ref vX.Y.Z -f tag=vX.Y.Z
   ```

Do not dispatch an old tag after a script-only fix; it checks out the old
script again.

## Release signing key rotation and revocation

Normal rotation uses an overlap window:

1. Generate the successor keypair offline, with independent private-key
   custody. Add the public half to **every** trust set in `install.sh`,
   [`npm/scripts/install-binary.js`](../../npm/scripts/install-binary.js),
   [`orbit-common::security::release::TRUSTED_RELEASE_KEYS`](../../crates/orbit-common/src/security/release.rs)
   and [`desktop/src/update/trust.rs`](../../desktop/src/update/trust.rs)
   with the same new key ID and expiry (`not_after` / npm `notAfter`).
2. Publish a release and npm package still signed by the old key that ship
   both old and successor trust in the shell installer, npm installer,
   native binary and desktop app, with the desktop jobs on (and the Apple
   secrets present, for macOS) so the desktop app updates to the new trust. Verify all four trust sets before
   switching the signer. A desktop app older than this release reports the
   first successor-signed update as rejected, and must be updated by hand.
3. Update `ORBIT_RELEASE_SIGNING_KEY_PEM` and
   [`npm/release-signing.pub`](../../npm/release-signing.pub), then cut the
   first release signed by the successor.
4. After the overlap window, remove the old key or mark its revocation date
   (`revoked_at` / npm `revokedAt`) in all four trust sets.

Emergency revocation only protects users who upgrade: already-published npm
packages retain their embedded trust sets. Publish a patch signed by a
non-revoked key, deprecate affected npm versions, and announce the required
upgrade.

Installer trust overrides are explicit trust-boundary changes.
`ORBIT_RELEASE_TRUSTED_KEYS_FILE` replaces the full key set and requires
`ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1`.
`ORBIT_RELEASE_PUBLIC_KEY_FILE` is a deprecated single-key override and
requires its matching acknowledgement variable. The two overrides cannot be
used together. `orbit update` honors `ORBIT_RELEASE_TRUSTED_KEYS_FILE` with the
same acknowledgement and record format, and does not honor
`ORBIT_RELEASE_PUBLIC_KEY_FILE` on its own. Setting both is an error there too.

## What `make release-check` enforces

[`scripts/release-check.sh`](../../scripts/release-check.sh) compares:

- `[workspace.package].version` in `Cargo.toml`;
- `[package].version` in `desktop/Cargo.toml`;
- `.version` in `npm/package.json`;
- `.version` in the Claude, Codex, and Agent Plugins 1.0 manifests;
- the server name, package identity, launch arguments, and both version fields
  in `server.json`;
- Cursor/Claude/Codex MCP launch pins
  (`npx -y @orbit-tools/cli@<version> mcp serve`, never `@latest`);
- active distribution ownership (`constellation-works` /
  `https://github.com/constellation-works`) on plugin authors, Codex
  `developerName`, and `.claude-plugin/marketplace.json`;
- `npm view @orbit-tools/cli version`, when npm is available;
- the latest `gh release list -L 1` tag, when GitHub CLI access is available.

It also runs both plugin validators and the canonical-skill mirror drift check.
Missing `npm` or `gh` skips that remote source with a stderr note. Local
Cargo/npm/plugin/registry-metadata drift always fails. Cursor marketplace
submission remains a manual post-publish step.

Claude Code installs through `/plugin marketplace add constellation-works/orbit` and
`/plugin install orbit`; Codex installs through its Git marketplace commands.
Cursor uses `~/.cursor/plugins/local/orbit` pointing at `plugin/`, whose root
`plugin.json` and `mcp.json` are the Agent Plugins 1.0 contract. Public Cursor
Marketplace publication remains a human follow-up as documented above.

<!-- ORB-10995 -->
