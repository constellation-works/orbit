# Releasing Orbit

How to cut an Orbit release. Follow the checklist top to bottom. Plugin, npm, signing-key, and MCP Registry details live in [docs/runbooks/release.md](docs/runbooks/release.md).

## Branches

- `agent-main` is the dev branch. Releases are prepared and tagged here.
- `main` is the production branch. It only receives release merges and hotfixes.
- Every release ends with a fast-forward promotion of the tagged release commit to `main` and a check that the two agree, in the same session.

## Before you start

You need:

- Actions secrets `ORBIT_RELEASE_SIGNING_KEY_PEM` (signs the checksum manifest) and `TAP_GITHUB_TOKEN` (pushes to `constellation-works/homebrew-tap`).
- npm publish rights on `@orbit-tools` with your OTP. The npm package is published by hand.
- Permission to create GitHub Releases and admin-merge on `constellation-works/orbit`.

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

**Not breaking:**

- Rejecting input that was already invalid by spec, or new guards that match documented behavior.
- MCP description-only wording changes (names, types, requiredness, and shape unchanged).
- Internal refactors, module splits, and performance changes.
- New optional fields with safe defaults.

When in doubt, ask the human in step 3. Don't promote behavior tightening to breaking on your own.

### Breaking `.orbit/` layout changes

A breaking on-disk change must ship its migration in the same PR:

- **SQLite schema:** add to `MIGRATIONS` and bump `SUPPORTED_SCHEMA_VERSION` in `crates/orbit-store/src/driver/sqlite/migration/ledger.rs`.
- **Everything else** (directories, non-SQLite state files, log and index locations, file formats): add a `LAYOUT_MIGRATIONS` entry and bump `SUPPORTED_LAYOUT_VERSION` in `crates/orbit-store/src/workflow/layout/registry.rs`.

Each entry declares `MigrationCompatibility::Additive` (older binaries open the state read-only) or `::Breaking` (older binaries refuse and name the migration). Declare `Breaking` when unsure. The contract is in [docs/design/state-compatibility](docs/design/state-compatibility/2_design.md).

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
```

- Use the last tag whose version files actually match it. A recovery tag (e.g. `v0.10.1`, whose files still said `0.10.0`) is not a baseline.
- With more than about 30 task IDs, file a read-only survey task for the release crew (`luna`) instead of looking each one up in-session. The survey lands as an artifact on that task, not as a file in `docs/`.
- The survey is for understanding and breaking-change triage, not a CHANGELOG inventory.
- Don't start the bump until in-flight delivery has landed or the human says the queue is settled.

### 2. Draft the CHANGELOG entry

The CHANGELOG is a short consumer-facing release note, not a commit log. Non-release task PRs do not edit it. An explicitly authorized release-preparation task compiles the section at release time from the survey.

Add `## <X.Y.Z> — <YYYY-MM-DD>` at the top of `CHANGELOG.md`, with the release's
UTC calendar date in the heading (for example, `## 0.28.0 — 2026-10-07`), and:

1. `### Breaking Changes`: minor bumps only. List every breaking change, one bullet each.
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

The style check lints only `## Unreleased`, so you can iterate there before moving bullets into the version section. Released sections are frozen and never reflowed. The style check does not enforce the task/release boundary. When before-PR review is enabled (`review.before_pr = true`), the reviewer must treat any diff touching `CHANGELOG.md` for a non-release task as an open finding, return `reject` without fixing it, and let the settle step block PR publication. The release exception requires an explicitly authorized task tagged `release` and titled `Prepare v<X.Y.Z> release`; listing `CHANGELOG.md` as a context file alone does not authorize an edit. Deliveries without before-PR review rely on the repository rule in [AGENTS.md](AGENTS.md) and after-landing review. Universal deterministic enforcement is out of scope.

### 3. Confirm breaking changes with the human

Show each candidate with its task ID, title, and why it was flagged. The human accepts, downgrades, or adds. Don't classify autonomously.

### 4. Bump versions

| File | Field |
|---|---|
| `Cargo.toml` | `[workspace.package].version` |
| `Cargo.lock` | `cargo update --workspace` (no third-party drift) |
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
```

- `make build` must be clean. `cargo update --workspace` should re-lock only Orbit crates. Investigate any third-party movement.
- `make release-check` ([what it checks](docs/runbooks/release.md#what-make-release-check-enforces)) fails on any local version drift. Before npm and the GitHub Release exist, drift against the previous remote version is expected.
- If this cycle changed a CLI the npm smoke drives (`orbit init`, `workspace init`, `mcp serve`), update `scripts/smoke-npm-install.sh` in the release commit. The tag-triggered smoke runs the tag's copy of the script, so a later fix can't turn it green.
- If `install.sh` changed, test it locally. The release smoke fetches it from the tag.

### 6. Create the Orbit task

```
title:  Prepare v<X.Y.Z> release
type:   chore
tags:   ["release"]
context_files:
  - file:CHANGELOG.md
  - file:Cargo.toml
  - file:Cargo.lock
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

Acceptance criteria: Cargo, npm, `server.json`, and all three plugin manifests report the new version. `Cargo.lock` is refreshed without third-party drift. The CHANGELOG section is in place. The npm and plugin install smokes pass.

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

After release CI is green, fast-forward `main` to the release commit. A fast-forward adds no merge commit to either branch, so the tag stays reachable from `main` and `agent-main` needs no back-merge. [10c](#10c-confirm-main-and-agent-main-agree) only checks the result.

```sh
git fetch origin
tag=v<X.Y.Z>
release=$(git rev-parse --verify "refs/tags/$tag^{commit}") &&
git merge-base --is-ancestor "$release" origin/agent-main &&   # release must be on agent-main
git merge-base --is-ancestor origin/main "$release" &&         # main must fast-forward to it
git push origin "$release:refs/heads/main"
```

The `^{commit}` suffix peels the annotated tag to the commit it validated. Promote that commit, never `origin/agent-main`: development work that landed on `agent-main` after the tag was cut has not been through the release's checks or artifacts, so it waits for the next release. The chain stops at the first failing command, so nothing is pushed unless both ancestry checks pass.

Why a fast-forward: the `agent-main` ruleset says "This branch must not contain merge commits". The old flow merged `agent-main` into `main` with a promotion-PR merge commit, then back-merged `main` into `agent-main` with `git merge --no-ff`. That back-merge (v0.28.1, `02b2e0aa2`) only landed because the pusher bypassed the rule. GitHub offers no PR merge method that fast-forwards, so promote with a direct push. A direct push to `main` needs the same release-operator rights the old `gh pr merge --admin` step already used. If the push is refused, don't push anything else to `agent-main`. Ask a repository admin to push, or to adjust the `main` ruleset for the release operator.

If the first `is-ancestor` fails, the tag is missing or is not on `agent-main`: stop and find out why before pushing anything. If the second fails, `main` holds commits the release lacks, which is the case after a [hotfix](#hotfix-flow) whose back-merge was skipped. Don't force-push `main`. Complete the [hotfix back-merge](#hotfix-flow) first. The existing tag can never fast-forward `main` past those commits, so release again from the merged `agent-main` with a new tag.

Never promote with a squash or rebase PR: it rewrites SHAs, so the release tag isn't reachable from `main`.

### 10c. Confirm `main` and `agent-main` agree

Do this in the same session:

```sh
git fetch origin
git merge-base --is-ancestor origin/main origin/agent-main && echo in-sync
```

`in-sync` means `main` is the release commit or another ancestor of `agent-main`, so the next promotion is again a fast-forward. `agent-main` may be ahead of `main` by development commits landed since the tag; that is expected. If it prints nothing, `main` holds commits `agent-main` lacks. Follow the [hotfix back-merge](#hotfix-flow), and never force-push either branch to make them match.

The `agent-main` ruleset is a repository ruleset (Settings → Rules), not classic branch protection. It blocks deletion and merge commits and doesn't gate on CI. The `qa-sweep` auto-task picks up CI failures. Read it with `gh api repos/constellation-works/orbit/rules/branches/agent-main`. If `agent-main` goes missing from origin, an admin restores it with `git push origin origin/main:refs/heads/agent-main` and checks the ruleset again.

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
| `publish-release` | Writes `orbit-checksums.txt`, signs it as `orbit-checksums.txt.sig`, and creates the GitHub Release with auto-generated notes |
| `bump-homebrew-tap` | Updates `Formula/orbit.rb` in `constellation-works/homebrew-tap` (macOS only; Linux uses `install.sh`) |
| `smoke-install-macos` / `smoke-install-ubuntu` | Installs from the tag's `install.sh` and runs `orbit --version` |

npm isn't published from CI. The separate `smoke-npm-install.yml` workflow runs weekly, on every tag, and on demand.

Common failures:

- **`--locked` build fails:** `Cargo.lock` wasn't refreshed. Fix it in the next patch.
- **Homebrew step fails:** `TAP_GITHUB_TOKEN` expired, or tap branch protection rejected the push.
- **Installer smoke fails:** there's a regression in the tagged `install.sh`.
- **npm smoke red after publish:** either the tagged script speaks an old CLI contract, or the published artifact is bad. Triage with the [runbook](docs/runbooks/release.md#npm-install-smoke-two-artifacts). Fix a script-only problem on `agent-main` without a patch release, and don't re-dispatch the old tag after the script has moved on.

## When something goes wrong

Tags, GitHub Releases, and npm versions are immutable. Fix forward.

- **Tag on the wrong commit:** don't move it. Cut the next patch.
- **Release CI failed after tagging:** leave the tag. Re-run the job from Actions if it was an infrastructure failure. Otherwise, fix it in the next patch.
- **A breaking change is missing from the CHANGELOG:** note it in the next release's section. Don't rewrite the old one.
- **Never** force-update a tag, overwrite a release asset, or republish an npm version.

## Hotfix flow

Use this for a critical fix on a released `main` that can't wait for the next cycle.

1. Branch from `main`:

   ```sh
   git checkout -b hotfix/<slug> main
   ```

2. Open a PR against `main` with the smallest possible fix. No refactors.
3. Cut a patch release on `main` with checklist steps 1–10, using `main` as the branch: `git push origin main && git push origin v<X.Y.Z+1>`. Skip 10b, because the fix is already on `main`.
4. Back-merge in the same session, so the next promotion doesn't overwrite the fix and `main` is an ancestor of `agent-main` again. Fetch and merge the remote `main` revision (`origin/main`), not the local `main` branch. `git fetch` moves remote-tracking refs only, so a hotfix another operator landed on `origin/main` is absent from a local `main` that is still the previous release. Merging that stale branch reports success, pushes, and leaves the hotfix off `agent-main`, so the next promotion stays refused. The ancestor check must exit 0 before the push; it proves every fetched `main` commit reached `agent-main`. Don't force-push.

   ```sh
   git fetch origin
   git checkout agent-main
   git merge --ff-only origin/agent-main
   git merge --no-ff origin/main
   git merge-base --is-ancestor origin/main HEAD   # must exit 0 before the push
   git push origin agent-main
   ```

   This is the one release step that must create a merge commit on `agent-main`. The ruleset refuses it unless the pusher can bypass it, so the push needs a repository admin or a release operator with bypass rights. A "Bypassed rule violations" notice on the push is expected here. When the fix can wait for the next cycle, prefer landing it on `agent-main` by an ordinary PR and releasing through 10b, which needs no bypass.

5. Resolve conflicts with in-flight `agent-main` work in the back-merge. Don't rebase agent branches onto the new tip.
