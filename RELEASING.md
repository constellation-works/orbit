---
type: context
summary: "Release checklist: versioning, bumps, tagging, CI, promotion and hotfixes"
last_validated: 2026-11-16
---

# Releasing Orbit

How to cut an Orbit release. Follow the checklist top to bottom. Plugin, npm, signing-key, and MCP Registry details live in [docs/runbooks/release.md](docs/runbooks/release.md).

## Branches

- `agent-main` is the dev branch. Releases are prepared and tagged here.
- `main` is the production branch. It only receives release merges and hotfixes.
- Every release ends with a pull request that merges the tagged release commit into `main` with a merge commit, and a check that `main` holds exactly that commit, in the same session.

## Before you start

You need:

- Actions secrets `ORBIT_RELEASE_SIGNING_KEY_PEM` (signs the checksum manifests) and `TAP_GITHUB_TOKEN` (pushes to `constellation-works/homebrew-tap`).
- For the macOS desktop app: the six Apple signing and notarization secrets listed in [desktop/README.md](desktop/README.md#one-time-setup). Without all six, a release still ships the Linux desktop bundles and skips only macOS. The Linux bundles need no extra secret. Setting the repository variable `ORBIT_DESKTOP_RELEASE` to `false` turns the desktop jobs off for tag pushes. Unset or any other value leaves them on.
- npm publish rights on `@orbit-tools` with your OTP. The npm package is published by hand.
- Permission to create GitHub Releases and to push the validated release commit to `main` (or a repository admin who can make that push).

Never paste, log, or rotate these credentials in a PR.

## Versioning

Pre-1.0 semver, `0.<minor>.<patch>`. A breaking change bumps minor (`0.3.1 → 0.4.0`). Anything else bumps patch (`0.3.0 → 0.3.1`).

**Breaking:**

- Removing or renaming a CLI command or flag.
- Changing an MCP tool's input or output schema, including response shape (array → object counts).
- Removing or renaming job or activity YAML fields, or new load-time validation that rejects previously parseable input.
- Task storage or task-field enum changes that need a data migration.
- Any other `.orbit/` on-disk layout change existing workspaces can't absorb as-is (see below).
- Removing a seeded skill, activity, or job.
- Adding a field to a persisted `#[serde(deny_unknown_fields)]` type (task envelope, relation, regression classification). Older binaries, including follower hosts, refuse the new record.

**Not breaking:**

- Rejecting input that was already invalid by spec, or new guards that match documented behavior.
- MCP description-only wording changes (names, types, requiredness, and shape unchanged).
- Internal refactors, module splits, and performance changes.
- New optional fields with safe defaults, except on a persisted `deny_unknown_fields` type (above).

When in doubt, ask the human in step 3. Don't promote behavior tightening to breaking on your own.

### The `breaking-change` tag and `Breaking:` notes

The lists above are the definition. Two records carry it from the task to the release:

- **`breaking-change` tag.** The implementer adds it to a task whose change is breaking, and the reviewer confirms it. A landed task with the tag makes the release a minor bump.
- **`Breaking:` note.** The task's execution summary has one line starting `Breaking:` with the migration as a phrase, for example `Breaking: orbit run ship-sweep removed → use orbit run auto drains or orbit run ship`. It becomes the task's Breaking Changes bullet.

`scripts/release-breaking-changes.py` reads both for the landed tasks in a range. It also detects mechanical signals: a raised `SUPPORTED_SCHEMA_VERSION` or `SUPPORTED_LAYOUT_VERSION`; a command, flag or possible value gone from the CLI help goldens; a non-additive change in the MCP `tools/list` snapshot (tool or property removed or renamed, type changed, enum value removed, property newly required, or a required, enum or whole properties-map constraint relaxed inside a surviving `oneOf` branch); a field added to a `deny_unknown_fields` type in `orbit-types` or `orbit-store`; a new `DEPRECATED_CONFIG_KEYS` entry; or a removed seeded routine, activity or job asset. The bump is **minor** when any landed task carries the tag or any signal fires, otherwise **patch**. A signal on an untagged task is listed as *detected, untagged*.

The detector can't see hidden flags, output-shape changes or new validation, so the tag is still needed for those.

### Breaking `.orbit/` layout changes

A breaking on-disk change must ship its migration in the same PR:

- **SQLite schema:** add to `MIGRATIONS` and bump `SUPPORTED_SCHEMA_VERSION` in `crates/orbit-store/src/driver/sqlite/migration/ledger/registry.rs`.
- **Everything else** (directories, non-SQLite state files, log and index locations, file formats): add a `LAYOUT_MIGRATIONS` entry and bump `SUPPORTED_LAYOUT_VERSION` in `crates/orbit-store/src/workflow/layout/registry.rs`.

Each entry declares `MigrationCompatibility::Additive` (older binaries can read the state and keep writing), `::ReadCompatible` (older binaries open SQLite state read-only; newer layout state requires an updated binary) or `::Breaking` (older binaries refuse and name the migration). Declare `Breaking` when unsure. The contract is in [docs/design/state-compatibility](docs/design/state-compatibility/2_design.md).

Layout migrations must be idempotent or staged (write new, then swap). They auto-apply when a workspace opens and re-run after a crash, because `state/layout.version` only advances after an entry succeeds. `orbit migrate --dry-run` lists pending migrations.

The migration makes the change survivable, not non-breaking. Still bump minor and list it under Breaking Changes.

### CHANGELOG archiving

Archiving happens only on a **major** release, so not before `1.0.0`. Until then `CHANGELOG.md` accumulates every release, however large, including `0.x → 0.(x+1).0` bumps.

On a major release:

- Move the released `## <X.Y.Z>` sections byte-for-byte into `docs/changelogs/`. Don't edit old bullets or stale task IDs. `## Unreleased` never moves.
- Keep `CHANGELOG.md` at the repo root under the same name. `scripts/check-changelog-style.sh` and the convention-file allowlist in `crates/orbit-core/src/application/task/paths.rs` depend on that path.
- Start the live file fresh with the new version's section and a blank `## Unreleased`.
- Link the two locations both ways.

## Release checklist

### 1. Survey commits since the last tag

```sh
git log v<prev>..HEAD --pretty='%h%x09%s' --no-merges
git log v<prev>..HEAD --pretty='%s' --no-merges | grep -oE '\[[A-Z]+-[0-9]+\]' | sort -u
scripts/release-breaking-changes.py --range v<prev>..HEAD
```

- `scripts/release-breaking-changes.py` lists the breaking candidates (tagged, or detected and untagged), decides the bump and drafts the Breaking Changes bullets. Add `--json` for a machine-readable report. It reads tags and execution summaries with `orbit task show` from the checkout. If Orbit is unavailable, or with `--offline`, it takes task IDs from commit subjects only and says so, so tags and notes are missing from that report.
- Use the last tag whose version files actually match it. A recovery tag (e.g. `v0.10.1`, whose files still said `0.10.0`) is not a baseline.
- With more than about 30 task IDs, file a read-only survey task for the release crew (`luna`) instead of looking each one up in-session. The survey lands as an artifact on that task, not as a file in `docs/`.
- The survey is for understanding and breaking-change triage, not a CHANGELOG inventory.
- Don't start the bump until in-flight delivery has landed or the human says the queue is settled.

### 2. Draft the CHANGELOG entry

The CHANGELOG is a short consumer-facing release note, not a commit log. Non-release task PRs do not edit it. An explicitly authorized release-preparation task compiles the section at release time from the survey.

Add `## <X.Y.Z> — <YYYY-MM-DD>` at the top of `CHANGELOG.md`, with the release's
UTC calendar date in the heading (for example, `## 0.28.0 — 2026-10-07`), and:

1. `### Breaking Changes`: minor bumps only. List every breaking change, one bullet each. Start from the script's draft: each bullet comes from the task's `Breaking:` note, and a `TODO` placeholder names a task that has none. Rewrite the theme and fill in every placeholder.
2. `### Highlights`: 3 to 6 user-facing features or behavior changes. If you're unsure whether something is a highlight, it isn't.

The website renders the date beside the version and keeps version-only anchors
stable. Historical headings remain frozen: `website/src/data/release-dates.json`
records the UTC dates of their matching `v<X.Y.Z>` Git tags (tagger timestamps
for annotated tags, tagged commit timestamps for lightweight tags). These
are release-tag dates, rather than a reconstructed GitHub publication time.
New releases use the date in `CHANGELOG.md`; do not add them to that historical
fallback file. The newest three releases are expanded on the website; older
notes remain available through their disclosure controls and existing anchors.

Leave out refactors, crate splits, lint fixes, dependency bumps, docs and ADR churn, release metadata, and bug fixes with no user-visible impact.

Bullet shape:

```
- **Theme**: 1–2 sentences that read in isolation. ([ORB-XXXXX])
```

- Group related tasks into one themed bullet and cite only the lead task ID. No commit SHAs.
- Aim for about 50 words. `scripts/check-changelog-style.sh` fails a bullet over 60 words or 3 non-blank lines.
- Migration steps, rationale, and test inventories stay in the cited task or commit. The task ID is the pointer.
- A breaking bullet gets at most one extra line, with the migration as a phrase (`x removed → use y`).

The style check lints only `## Unreleased`, so you can iterate there before moving bullets into the version section. Released sections are frozen and never reflowed. The style check does not enforce the task/release boundary. When before-PR review is enabled (`review.timing = "before_pr"`), the reviewer must treat any diff touching `CHANGELOG.md` for a non-release task as an open finding, return `reject` without fixing it, and let the settle step block PR publication. The release exception requires an explicitly authorized task tagged `release` and titled `Prepare v<X.Y.Z> release`; listing `CHANGELOG.md` as a context file alone does not authorize an edit. Deliveries without before-PR review rely on the repository rule in [AGENTS.md](AGENTS.md) and after-landing review. Universal deterministic enforcement is out of scope.

### 3. Confirm breaking changes with the human

Show the script's candidate list: each task ID, its title, and its signals or its `breaking-change` tag. The human confirms the list instead of classifying from scratch. They accept, downgrade or add candidates. Don't classify autonomously.

- **Detected, untagged:** once the human confirms one, add the `breaking-change` tag to the task so the record matches the release.
- **Version:** the bump follows the script's decision unless the human overrides it. Record any override and its reason on the release task.

### 4. Bump versions

| File | Field |
|---|---|
| `Cargo.toml` | `[workspace.package].version` |
| `Cargo.lock` | `cargo update --workspace` (no third-party drift) |
| `desktop/Cargo.toml` | `[package].version` |
| `desktop/Cargo.lock` | `cargo update --workspace --manifest-path desktop/Cargo.toml` (no third-party drift) |
| `npm/package.json` | `version` |
| `server.json` | `version` and `packages[0].version` |
| `plugin/.claude-plugin/plugin.json` | `version` |
| `plugin/.codex-plugin/plugin.json` | `version` and the `mcpServers.orbit.args` pin |
| `plugin/plugin.json` | `version` |
| `plugin/mcp.json` | `@orbit-tools/cli@<version>` launch pin |
| `plugin/.mcp.json` | `@orbit-tools/cli@<version>` launch pin |

- Pins are always `npx -y @orbit-tools/cli@<version> mcp serve`, never `@latest`.
- If `crates/orbit-core/assets/skills/orbit/` changed, run `scripts/sync-plugin-skills.sh`. CI runs it with `--check` and rejects drift in `plugin/skills/orbit/`.
- Leave other `0.X.Y` strings alone (install-script comments, website task pages, the Node pin in `website/package-lock.json`).

### 5. Verify

```sh
make build
make release-check
./scripts/smoke-plugin-install.sh
./scripts/smoke-npm-install.sh --dry-run-version-assertion
./scripts/smoke-npm-install.sh --local-package-check
./scripts/smoke-npm-install.sh --local-binary-check target/debug/orbit
```

- `make build` must be clean. `cargo update --workspace` should re-lock only Orbit crates. Investigate any third-party movement.
- `make release-check` ([what it checks](docs/runbooks/release.md#what-make-release-check-enforces)) fails on any local version drift. Before npm and the GitHub Release exist, drift against the previous remote version is expected.
- If this cycle changed a CLI the npm smoke drives (`orbit init`, `workspace init`, `mcp serve`), update `scripts/smoke-npm-install.sh` in the release commit. The tag-triggered smoke runs the tag's copy of the script, so a later fix can't turn it green.
- Before publication, the local package check packs and inspects the candidate without lifecycle scripts; the local binary check exercises isolated init, credential-origin rejection, explicit `ORBIT_ROOT` discovery and agent authority refusals against `make build`'s output. Neither proves the published download/signature chain. Run the default smoke after npm publication for that proof.
- If `install.sh` changed, test it locally. The release smoke fetches it from the tag.
- If this cycle changed `desktop/` or the desktop jobs, prove the Linux desktop legs before tagging: Actions → `Desktop App` → **Run workflow** on the branch you will tag. Its `Release bundle` jobs run `build-desktop`'s Linux legs (`ubuntu-22.04` x86_64 and `ubuntu-22.04-arm` arm64, the same `cargo tauri build --ci --target … -- --locked` command) and check for exactly one `.deb` and one `.AppImage` each. They upload unsigned workflow artifacts and publish nothing.

### 6. Create the Orbit task

```
title:  Prepare v<X.Y.Z> release
type:   chore
tags:   ["release"]
context_files:
  - file:CHANGELOG.md
  - file:Cargo.toml
  - file:Cargo.lock
  - file:desktop/Cargo.toml
  - file:desktop/Cargo.lock
  - file:npm/package.json
  - file:server.json
  - file:plugin/.claude-plugin/plugin.json
  - file:plugin/.codex-plugin/plugin.json
  - file:plugin/plugin.json
  - file:plugin/mcp.json
  - file:plugin/.mcp.json
  - file:scripts/smoke-npm-install.sh
  - file:scripts/smoke-plugin-install.sh
```

Acceptance criteria: Cargo (including `desktop/Cargo.toml`), npm, `server.json`, and all three plugin manifests report the new version. `Cargo.lock` and `desktop/Cargo.lock` are refreshed without third-party drift. The CHANGELOG section is in place. The npm and plugin install smokes pass.

### 7. Get human approval

Don't commit until the human approves the task (`proposed → backlog`). Then start it.

### 8. Commit

```sh
git -c user.name='<agent>' -c user.email='<agent-email>' commit \
  --author='<agent> <agent-email>' \
  -m "chore: prepare v<X.Y.Z> release [<task-id>]

<one or two sentences>"
```

Use the commit identity of the agent running the release. `git log` shows each agent's canonical email.

### 9. Tag

```sh
git tag -a v<X.Y.Z> -m "v<X.Y.Z>

See CHANGELOG.md. Highlights:
- ...
- N breaking changes (...)"
```

Tags are always annotated. Keep the message short, because the CHANGELOG is the source of truth.

### 10. Push and publish

```sh
git push origin agent-main    # pull first if it moved; never force-push a release commit
git push origin v<X.Y.Z>      # branch first, so CI resolves the tag against a pushed commit
```

1. Watch the [release workflow](#release-ci) in Actions.
2. Once the GitHub Release exists, publish npm from the release commit:

   ```sh
   cd npm && npm publish --access public    # prompts for the OTP
   ```

   Keep this gap short, because plugin installs can't fetch the pinned version until npm has it.
3. Re-run Actions → `smoke-npm-install` → **Run workflow** with the release tag in the `tag` input. The tag-triggered run usually fails because npm didn't have the version yet. This post-publish run must be green.
4. Optionally publish the MCP Registry record ([runbook](docs/runbooks/release.md#publish-the-official-mcp-registry-record)).

### 10b. Promote to `main`

After release CI is green, promote the tagged commit to `main` through a pull request. `main` takes changes only through pull requests, so this step never pushes `main` directly. Run the gate first. It pushes one branch, at the tagged commit, and nothing else:

```sh
git fetch origin &&
tag=v<X.Y.Z> &&
release=$(git rev-parse --verify "refs/tags/$tag^{commit}") &&
git merge-base --is-ancestor "$release" origin/agent-main &&
test "$(git merge-tree --write-tree origin/main "$release" | head -n 1)" = "$(git rev-parse "$release^{tree}")" &&
git push origin "$release:refs/heads/promote/$tag"
```

The gate checks that the release is on `agent-main`, and that merging `main` into the release yields exactly the release's tree. So `main` holds nothing the release lacks. `git merge-tree --write-tree` needs Git 2.38 or later.

Then run `gh pr create --base main --head promote/v<X.Y.Z>`, and have a release operator merge the PR with **Create a merge commit**. [10c](#10c-confirm-main-and-agent-main-agree) checks the result.

Promote the tagged commit, never the `agent-main` tip. The tag is the commit that release CI and the artifacts validated. Development work that landed on `agent-main` after the tag was cut has not been through those checks, so it waits for the next release. v0.50.0 is the exception: it promoted the tip at promotion time (`da9501ec7a`, nine commits past its tag).

Why a merge commit: `main`'s ruleset requires a pull request and refuses non-fast-forward pushes, so a direct push cannot move `main`. The merge commit keeps the tagged commit reachable from `main` as the PR's second parent. Squash and rebase merges rewrite the SHA, so they drop that link; never use them for a promotion. `agent-main` never receives the merge, so it needs no back-merge and keeps its no-merge-commits ruleset. Restoring fast-forward promotion needs a ruleset and history change by a repository admin. Until that happens, this is the flow.

If the gate stops, nothing is pushed. A failed `is-ancestor` means the tag is missing or not on `agent-main`: find out why first. A failed tree test means `main` holds content the release lacks, usually a [hotfix](#hotfix-flow) that is not on `agent-main` yet. Back-port it first. Don't force-push anything.

### 10c. Confirm `main` and `agent-main` agree

Do this in the same session, after the PR merges:

```sh
git fetch origin &&
promoted=$(git rev-parse --verify 'origin/main^2') &&
git merge-base --is-ancestor "$promoted" origin/agent-main &&
git diff --quiet "$promoted" origin/main &&
echo "promoted $promoted"
```

`origin/main^2` is the PR head that the merge brought in. The check passes when that commit came from `agent-main` and `main`'s tree is exactly its tree. For a release, `promoted` must equal the tagged commit, `git rev-parse "refs/tags/$tag^{commit}"`. A different commit means the wrong head was merged: stop and tell the operator. Don't force-push `main`.

Nothing printed means `main` holds content the promoted commit lacks, usually a hotfix that reached `main` after the gate ran or was never back-ported. Don't revert `main` or force-push either branch. Back-port the hotfix to `agent-main` ([hotfix flow](#hotfix-flow), step 4). The next promotion's gate refuses until that lands.

`agent-main` may be ahead of `main` by development commits landed since the tag; that is expected. `main` never becomes an ancestor of `agent-main`: each promotion merge is a commit `agent-main` lacks, so an ancestry check cannot pass. The tree check above is what confirms `main`.

The `agent-main` ruleset is a repository ruleset (Settings → Rules), not classic branch protection. It blocks deletion and merge commits and requires the `Check / Clippy / Test` and `Test shard 1/3`, `Test shard 2/3`, and `Test shard 3/3` checks before merge. The `qa-sweep` auto-task picks up CI failures. Read it with `gh api repos/constellation-works/orbit/rules/branches/agent-main`. If `agent-main` goes missing from origin, an admin restores it with `git push origin origin/main:refs/heads/agent-main` and checks the ruleset again.

### 11. Mark the Orbit task done

Set `status: done`, `implemented_by: <agent>`, and an `execution_summary` with the commit SHA and tag. The next release finds it by the `release` tag.

### 12. Cursor marketplace listing

Publishing a release does not update Cursor's curated marketplace, and there is no push API. After the tag and npm version exist:

1. Submit or update the listing at <https://cursor.com/marketplace/publish> using `https://github.com/constellation-works/orbit` and the `plugin/` subdirectory. Don't add `.cursor-plugin`. The stale listing **2280865** (`danieljhkim/orbit` at 0.5.1) stays as-is.
2. Install through Cursor plugin search and check the version and the `npx -y @orbit-tools/cli@<version> mcp serve` pin. If review stalls, email `marketplace-publishing@cursor.com` yourself, never from CI.
Details are in the [runbook](docs/runbooks/release.md#cursor-marketplace-listing).

## Release CI

Pushing a `v*` tag runs `.github/workflows/release.yml`:

| Job | What it does |
|---|---|
| `build-release` | `cargo build -p orbit-cli --release --locked` for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu` |
| `build-bundled-bwrap` | Builds the static bundled Bubblewrap per architecture in a digest-pinned Alpine image and uploads it with its sources and recipe |
| `publish-release` | Writes `orbit-checksums.txt`, signs it as `orbit-checksums.txt.sig`, and creates the GitHub Release with auto-generated notes |
| `desktop-preflight` | Checks the six Apple secrets and plans the desktop build. Linux is always in the plan. macOS is added only when every secret is present; otherwise the run shows a warning naming the missing ones. Turns the desktop jobs off when `ORBIT_DESKTOP_RELEASE` is `false` |
| `build-desktop` | Runs `cargo tauri build` in `desktop/` at the tag to make the `.deb` and `.AppImage` for Linux x86_64 and arm64, and, when the secrets are present, a signed, notarized universal macOS `.dmg` |
| `publish-desktop` | Once `publish-release` has created the release and every planned desktop leg has succeeded, writes `orbit-desktop-version.txt`, signs `orbit-desktop-checksums.txt` over it and exactly the bundles built, and attaches them. A failed Linux leg stops it; a macOS leg the preflight left out does not. Tag pushes, manual backfills, and reruns for the same tag attach one at a time: the publisher already uploading finishes, and a different tag stays independent |
| `bump-homebrew-tap` | Updates `Formula/orbit.rb` in `constellation-works/homebrew-tap` (macOS only; Linux uses `install.sh`) |
| `smoke-install-macos` / `smoke-install-ubuntu` | Installs from the tag's `install.sh` and runs `orbit --version` |
| `smoke-install-ubuntu-2204-bundled-bwrap` | Removes host Bubblewrap on Ubuntu 22.04 to reach the installer fallback, then checks the bundled binary is installed and passes the capability probe |

A release that shipped without desktop bundles, such as v0.29.0, can get its missing desktop assets afterwards from the workflow's manual run: Actions → `release` → **Run workflow**, with `tag` set to the existing release tag. It runs only the desktop jobs, builds the tag's source with the current workflow, and attaches missing assets under the same rules. It refuses a tag without a GitHub Release, and it ignores `ORBIT_DESKTOP_RELEASE`. See the [runbook](docs/runbooks/release.md#desktop-backfill).

npm isn't published from CI. The separate `smoke-npm-install.yml` workflow runs weekly, on every tag, and on demand.

Common failures:

- **`--locked` build fails:** `Cargo.lock` wasn't refreshed. Fix it in the next patch.
- **Homebrew step fails:** `TAP_GITHUB_TOKEN` expired, or tap branch protection rejected the push.
- **Desktop job fails:** the CLI release has already published, so nothing is blocked. A missing Apple secret skips only the macOS leg, and the preflight's warning names it. An expired certificate or a revoked app-specific password shows up as a signing or notarization error. Fix a broken secret and re-run the failed jobs: `publish-desktop` adds only missing assets, hashing downloaded published bundles even if a rebuild differs. It uploads the checksum manifest and signature last; a complete retry verifies them without uploading again. Conflicting metadata from an older partial upload requires a new release rather than replacement assets. A release that published a Linux-only manifest can't gain a macOS bundle later, because that is a manifest conflict by design: macOS ships in the next release. Tag pushes, backfills, and reruns of the same tag share one queue, so a later attempt waits for the publisher already uploading and then applies that conflict check before it adds a disk image. A version mismatch means `desktop/Cargo.toml` wasn't bumped, so fix it in the next patch.
- **Installer smoke fails:** there's a regression in the tagged `install.sh`.
- **npm smoke red after publish:** either the tagged script speaks an old CLI contract, or the published artifact is bad. Triage with the [runbook](docs/runbooks/release.md#npm-install-smoke-two-artifacts). Fix a script-only problem on `agent-main` without a patch release, and don't re-dispatch the old tag after the script has moved on.

## When something goes wrong

Tags, GitHub Releases, and npm versions are immutable. Fix forward.

- **Tag on the wrong commit:** don't move it. Cut the next patch.
- **Release CI failed after tagging:** leave the tag. Re-run the job from Actions if it was an infrastructure failure. Otherwise, fix it in the next patch.
- **A breaking change is missing from the CHANGELOG:** note it in the next release's section. Don't rewrite the old one.
- **Never** force-update a tag, overwrite a release asset, or republish an npm version.

## Hotfix flow

Use this for a critical fix on a released `main` that can't wait for the next cycle. `main` takes changes only through pull requests, so every step below lands through one.

1. Branch from the remote `main`, after a fetch:

   ```sh
   git fetch origin
   git checkout -b hotfix/<slug> origin/main
   ```

2. Open a PR against `main` with the smallest possible fix. No refactors.
3. Cut a patch release from `main` with checklist steps 1–10. Land the version bump and CHANGELOG section on `main` through a PR as well, then tag the merged commit and push the tag: `git tag -a v<X.Y.Z+1> <merged-commit>` and `git push origin v<X.Y.Z+1>`. Skip 10b, because the fix is already on `main`.
4. Back-port to `agent-main` before the next promotion. Stage what `main` has beyond the last promotion on a branch cut from `agent-main`, and land it with an ordinary PR. The 10b gate refuses to promote until `agent-main` holds it. Use `origin/main` after the fetch: a local `main` can be behind the remote.

   ```sh
   git fetch origin &&
   slug=<slug> &&
   git checkout -b "backport/$slug" origin/agent-main &&
   git merge --squash origin/main &&
   git commit -m "fix: back-port $slug from main" &&
   git push origin "backport/$slug"
   ```

   `--squash` stages the changes `main` has beyond the last promotion and creates no merge commit. Open a PR from `backport/<slug>` into `agent-main`, and merge it with squash or rebase. `agent-main` takes no merge commits, so the back-port needs no bypass. Don't force-push.

5. Resolve conflicts with in-flight `agent-main` work in the back-port. Don't rebase agent branches onto the new tip. If the 10b gate later reports a conflict on the version files or CHANGELOG, stop and raise it with the human. The hotfix release and the next release both bump those files, so resolving the conflict changes which commit gets promoted. That is a decision, not a step.
