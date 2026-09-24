# Releasing Orbit

How to cut an Orbit release. Follow the checklist top to bottom. Plugin, npm, signing-key, and MCP Registry details live in [docs/runbooks/release.md](docs/runbooks/release.md).

## Branches

- `agent-main` is the dev branch. Releases are prepared and tagged here.
- `main` is the production branch. It only receives release merges and hotfixes.
- Every release ends with `agent-main → main` promotion and a `main → agent-main` back-merge, in the same session.

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
- **Everything else** (directories, non-SQLite state files, log and index locations, file formats): add a `LAYOUT_MIGRATIONS` entry and bump `SUPPORTED_LAYOUT_VERSION` in `crates/orbit-store/src/workflow/layout/mod.rs`.

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
- With more than about 30 task IDs, file a read-only survey task for the release crew (`luna`) instead of looking each one up in-session. [docs/runbooks/release-survey.md](docs/runbooks/release-survey.md) is an example.
- The survey is for understanding and breaking-change triage, not a CHANGELOG inventory.
- Don't start the bump until in-flight delivery has landed or the human says the queue is settled.

### 2. Draft the CHANGELOG entry

The CHANGELOG is a short consumer-facing release note, not a commit log. PRs never touch it. You compile the section at release time from the survey.

Add `## <X.Y.Z>` at the top of `CHANGELOG.md` with:

1. `### Breaking Changes`: minor bumps only. List every breaking change, one bullet each.
2. `### Highlights`: 3 to 6 user-facing features or behavior changes. If you're unsure whether something is a highlight, it isn't.

Leave out refactors, crate splits, lint fixes, dependency bumps, docs and ADR churn, release metadata, and bug fixes with no user-visible impact.

Bullet shape:

```
- **Theme**: 1–2 sentences that read in isolation. ([ORB-XXXXX])
```

- Group related tasks into one themed bullet and cite only the lead task ID. No commit SHAs.
- Aim for about 50 words. `scripts/check-changelog-style.sh` fails a bullet over 60 words or 3 non-blank lines.
- Migration steps, rationale, and test inventories stay in the cited task or commit. The task ID is the pointer.
- A breaking bullet gets at most one extra line, with the migration as a phrase (`x removed → use y`).

The style check lints only `## Unreleased`, so you can iterate there before moving bullets into the version section. Released sections are frozen and never reflowed. Nothing mechanically blocks a non-release `CHANGELOG.md` edit. The rule is in [AGENTS.md](AGENTS.md) and review.

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

After release CI is green, open the promotion PR. First trial-merge `origin/main` into a throwaway checkout of `agent-main`. If `website/package.json` (the js-yaml pin) conflicts, keep `agent-main`'s tighter constraint.

```sh
gh pr create --base main --head agent-main \
  --title "release: v<X.Y.Z>" --body "Promotes v<X.Y.Z>. See CHANGELOG.md."
gh pr merge <N> --merge --admin
```

Always use a merge commit, never squash or rebase, so the tag stays reachable from `main`. If GitHub says merge commits aren't allowed, turn them on for this merge, then off again:

```sh
gh api -X PATCH repos/constellation-works/orbit -f allow_merge_commit=true
gh pr merge <N> --merge --admin
gh api -X PATCH repos/constellation-works/orbit -f allow_merge_commit=false
```

### 10c. Post-merge: back-merge to `agent-main`

Do this right away, in the same session:

```sh
git checkout agent-main
git pull --ff-only origin agent-main
git merge --no-ff origin/main -m "chore: back-merge main into agent-main after v<X.Y.Z>"
git push origin agent-main
```

If a back-merge was skipped, run the same commands. They resolve cleanly however far behind `agent-main` is. If `agent-main` has no in-flight work, you can reset it instead with `git push origin origin/main:refs/heads/agent-main --force-with-lease`.

Branch protection on `agent-main` exists only to block deletion. It never gates merges on CI, and the `qa-sweep` auto-task picks up CI failures. If `agent-main` goes missing from origin, recreate it and restore the protection:

```sh
git push origin origin/main:refs/heads/agent-main
cat <<'EOF' | gh api -X PUT repos/constellation-works/orbit/branches/agent-main/protection --input -
{
  "required_status_checks": null,
  "enforce_admins": false,
  "required_pull_request_reviews": null,
  "restrictions": null,
  "required_linear_history": false,
  "allow_force_pushes": true,
  "allow_deletions": false,
  "block_creations": false,
  "required_conversation_resolution": false,
  "lock_branch": false,
  "allow_fork_syncing": false
}
EOF
```

### 11. Mark the Orbit task done

Set `status: done`, `implemented_by: <agent>`, and an `execution_summary` with the commit SHA and tag. The next release finds it by the `release` tag.

### 12. Cursor marketplace follow-up

Publishing a release does not update Cursor's curated marketplace, and there is no push API. After the tag and npm version exist:

1. Submit or update the listing at <https://cursor.com/marketplace/publish> using `https://github.com/constellation-works/orbit` and the `plugin/` subdirectory. Don't add `.cursor-plugin`. The stale listing **2280865** (`danieljhkim/orbit` at 0.5.1) stays as-is.
2. Install through Cursor plugin search and check the version and the `npx -y @orbit-tools/cli@<version> mcp serve` pin. If review stalls, email `marketplace-publishing@cursor.com` yourself, never from CI.
3. Add `.github/cursor-marketplace-followup/<version>.ack` containing `version=<version>` to `agent-main`, then re-run the tag's `cursor-marketplace-followup` job. Never move the tag to add the receipt.

The ack records that you did the follow-up. It doesn't mean the listing is live. Details are in the [runbook](docs/runbooks/release.md#cursor-marketplace-listing).

## Release CI

Pushing a `v*` tag runs `.github/workflows/release.yml`:

| Job | What it does |
|---|---|
| `build-release` | `cargo build -p orbit-cli --release --locked` for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu` |
| `publish-release` | Writes `orbit-checksums.txt`, signs it as `orbit-checksums.txt.sig`, and creates the GitHub Release with auto-generated notes |
| `bump-homebrew-tap` | Updates `Formula/orbit.rb` in `constellation-works/homebrew-tap` (macOS only; Linux uses `install.sh`) |
| `smoke-install-macos` / `smoke-install-ubuntu` | Installs from the tag's `install.sh` and runs `orbit --version` |
| `cursor-marketplace-followup` | A reminder with `continue-on-error`. It fails until the `.ack` exists, and it never gates or retracts anything |

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
4. Back-merge in the same session, so the next promotion doesn't overwrite the fix:

   ```sh
   git checkout agent-main
   git merge --no-ff main
   git push origin agent-main
   ```

5. Resolve conflicts with in-flight `agent-main` work in the back-merge. Don't rebase agent branches onto the new tip.
