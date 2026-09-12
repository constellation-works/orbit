# Releasing Orbit

Runbook for cutting an Orbit release. Codified from [T20260510-23] (v0.4.0).

See also [docs/runbooks/release.md](docs/runbooks/release.md) for the plugin, npm package, and GitHub Release publishing steps.

## Versioning policy

Pre-1.0 semver: `0.<minor>.<patch>`.

- **Breaking** → bump minor (e.g. `0.3.1` → `0.4.0`).
- **Non-breaking** → bump patch (e.g. `0.3.0` → `0.3.1`).

### What counts as breaking

- CLI command or flag removal/rename.
- MCP tool input or output schema change (including response shape — array → object counts).
- Activity/job YAML schema removal, rename, or load-time validation that rejects previously-parseable input.
- Task storage layout or task-field enum change requiring data migration.
- Any other `.orbit/` on-disk layout change existing workspaces cannot absorb as-is (see [Breaking `.orbit/` layout changes](#breaking-orbit-layout-changes) — these now **require** a layout-migration registry entry).
- Seeded asset removal (skill, activity, job) that external agent prompts may reference.
- Workspace knowledge-graph schema version bump that invalidates cached selectors.

### What does NOT count as breaking

- Validation tightening that rejects inputs that were already invalid by spec.
- New guards that match documented behavior (e.g. MCP surface catching up to CLI).
- MCP tool description-only wording changes that leave tool and parameter names,
  types, requiredness, and input/output shape unchanged.
- Internal module decomposition or refactors with no external API change.
- Performance changes.
- New optional fields with safe defaults.

When in doubt, ask the human during the breaking-change confirmation step (see below) — defaulting conservative, but don't auto-promote behavior tightening to breaking.

### Breaking `.orbit/` layout changes

Since ORB-10012, on-disk `.orbit/` state is versioned end to end and a breaking layout change **requires shipping the migration with it** — an undocumented break is no longer an option:

- **SQLite store schema** changes go through the versioned ledger in `crates/orbit-store/src/sqlite/migration/ledger.rs` (`MIGRATIONS` + `SUPPORTED_SCHEMA_VERSION`, ORB-10003).
- **Everything else about the `.orbit/` layout** — directory structure, non-SQLite state files, log/index locations, persisted file formats — goes through the workspace-layout registry in `crates/orbit-store/src/layout/mod.rs`: append a `LAYOUT_MIGRATIONS` entry (version, name, description, apply fn over the workspace `.orbit` dir) and bump `SUPPORTED_LAYOUT_VERSION`, in the same PR as the layout change.

Layout migrations must be **idempotent or staged (write-new-then-swap)**: they auto-apply during the workspace-open pre-flight and re-run after a crash (the `state/layout.version` marker only advances after an entry's apply succeeds). A workspace written by a newer orbit refuses to open under an older binary (downgrade guard), and `orbit migrate --dry-run` lists pending migrations — with a backup hint — before an upgrade applies them.

Such a change is still **breaking** for versioning purposes (bump minor) and must be listed under Breaking Changes; the registry entry is what makes it *survivable*, not what makes it non-breaking.

### CHANGELOG archiving

On a **major** release, the CHANGELOG history released before that version is archived under `docs/changelogs/` and `CHANGELOG.md` starts fresh (the new version's section, plus a blank `## Unreleased`). Between major releases, `CHANGELOG.md` accumulates every released section and is never split.

Under the versioning policy above, the current 0.x line has no major bump — every release, including breaking ones, is a `0.<minor>.<patch>` bump. So the archive trigger **does not exist yet**: a breaking `0.9.2 → 0.10.0` (or any other `0.x → 0.(x+1).0`) release does not archive, no matter how large `CHANGELOG.md` has grown. The convention first applies at `1.0.0`, and at each major release after that. Until then, `CHANGELOG.md` accumulates unconditionally.

ORB-10429 executed this archive ahead of `0.10.0` and produced a validated, working diff shape before its PR was rejected — on timing (there was no major-release trigger), not on the mechanism itself. Reuse that shape rather than re-deriving it, once a real major release triggers this:

- `CHANGELOG.md` keeps its exact name and repo-root location; the archive gets the new name and location, never the live file. Four things bind to `CHANGELOG.md`'s current path and must keep resolving: `scripts/check-changelog-style.sh` (hardcodes `$repo_root/CHANGELOG.md`), the convention-file allowlist in `crates/orbit-core/src/command/task/paths.rs`, the never-modify list in `.orbit/auto_tasks/doc-duties.yaml`, and the references to it from this file, `CONTRIBUTING.md`, `CLAUDE.md`, ADR-0176, and ADR-0210.
- Relocation of released sections into `docs/changelogs/` is byte-for-byte — stale task-id citations and inconsistent old bullet shapes are provenance, not defects, and must survive the move unedited.
- The live `## Unreleased` section never moves; only already-released `## <X.Y.Z>` sections are archived.
- Cross-link the two locations so neither reads as a dead end: the archive file links back to `CHANGELOG.md`, and `CHANGELOG.md` links forward to `docs/changelogs/` once that directory exists.

## Ownership cutover checklist

This checklist prepares the distribution surface for the transfer; it does not
authorize a repository transfer, remote mutation, release, npm publication,
merge, tag, or credential change. Complete it only after the reviewable PR is
approved and the source-identity migration in ORB-11426 is complete.

1. **Confirm repository availability and identity.** Verify that
   `https://github.com/constellation-works/orbit` is the canonical repository,
   that its `agent-main` and `main` branches are available, and that
   `git remote get-url origin` resolves to the transferred repository. Run
   `orbit workspace list --all --format json` and confirm that every registered
   Orbit workspace reports the same canonical source identity after ORB-11426.
   Do not alter workspace or registry state from this checklist.
2. **Verify release credentials in their new owner boundary.** An authorized
   maintainer must confirm Actions access to `ORBIT_RELEASE_SIGNING_KEY_PEM`
   and `TAP_GITHUB_TOKEN`, write access to
   `constellation-works/homebrew-tap`, npm publish access plus the required
   OTP for `@orbit-tools`, and the GitHub permissions needed to create a
   release. Never paste, log, or rotate those credentials in a PR.
3. **Validate the checked-in distribution contract before publication.** Run
   `make release-check`, `./scripts/smoke-npm-install.sh --dry-run-version-assertion`,
   `./scripts/smoke-plugin-install.sh`, and the managed-asset mirror checks.
   After the repository is available, explicitly verify the transfer-bound
   endpoints with:

   ```sh
   curl --fail --silent --show-error \
     https://raw.githubusercontent.com/constellation-works/orbit/main/install.sh >/dev/null
   gh api repos/constellation-works/orbit --jq .full_name
   gh api repos/constellation-works/homebrew-tap --jq .full_name
   ```

4. **Publish only immutable release artifacts in order.** Merge the approved
   preparation PR to `agent-main`, tag that exact merged commit, wait for the
   release workflow and Homebrew update, then publish the matching npm version
   once. Tags and npm versions are immutable: never retag, overwrite a release
   asset, or republish a version to repair a failure. Re-run the versioned npm
   smoke only after publication as documented in `docs/runbooks/release.md`.
5. **Rollback by moving forward.** If any post-cutover verification fails,
   pause promotion and publication where possible, restore the previous source
   owner only through an authorized transfer decision, and cut a new patch for
   any already-published artifact. Do not force-push, rewrite tags, delete
   immutable releases, or modify the personal `danieljhkim/homebrew-tap`.

Historical GitHub URLs in learning records, incident evidence, and completed
workflow examples intentionally retain their original owner because they name
immutable provenance. Active installer, release, npm, plugin, MCP, website,
and current documentation references use `constellation-works`.

## Release checklist

### 1. Survey commits since last tag

```sh
git log v<prev>..HEAD --pretty='%h%x09%s' --no-merges
git log v<prev>..HEAD --pretty='%s' --no-merges | grep -oE 'T[0-9]{8}-[0-9]+' | sort -u
```

If the unique task ID count exceeds ~30, file an Orbit survey task for the release crew (luna) rather than running per-task lookups in-session. The survey is read-only: `orbit.task.show` each ID, group by theme, flag breaking-change candidates. Do not start the version bump until in-flight delivery has landed or the human says the queue is settled.

Start the range at the last tag whose five version files actually match that tag. A recovery tag (for example `v0.10.1`, whose files still said `0.10.0`) is not a survey baseline.

The survey is for *your* understanding and for breaking-change triage — not a CHANGELOG inventory. Most surveyed items will not make the cut in step 2.

### 2. Draft the CHANGELOG entry

CHANGELOG is the consumer-facing release note, not a commit log. Keep it short. Anyone wanting the full diff runs `git log v<prev>..HEAD` — don't reproduce it here.

Insert a new `## <X.Y.Z>` section at the top of `CHANGELOG.md`. Section order:

1. **Breaking Changes** — only for minor bumps; one bullet per breaking item. Always list every breaking change.
2. **Highlights** — 3–6 bullets covering user-facing features or behavioral improvements that meaningfully change how Orbit is used. Pick the headlines; drop the rest.

Omit entirely: internal refactors, module / crate splits, dashboard JS reorganization, lint or clippy fixes, dependency bumps, docs / ADR / learning churn, release metadata, unattributed cleanup commits, and small bug fixes with no user-visible impact. If you're unsure whether something is a Highlight, it isn't.

Bullet shape:

```
- **Theme name**: one-sentence description that reads in isolation. ([ORB-00013])
```

Group related task IDs into a single themed bullet rather than emitting one bullet per task. Cite the lead task ID only; skip commit SHAs.

#### Compiled at release time, not accumulated per-PR

Task execution never touches `CHANGELOG.md` — no PR adds an `## Unreleased` bullet. Instead, the release drafter compiles the new `## <X.Y.Z>` section directly from `git log v<prev>..HEAD` (step 1's survey) plus the cited Orbit task IDs, using the same bullet shape:

- Format: `- **Theme**: 1–2 sentences that read in isolation. ([ORB-XXXXX])`. Hard cap **~50 words per bullet** (the `scripts/check-changelog-style.sh` guardrail fails past ~60 words or 3 physical lines).
- Migration steps, rationale, rejected alternatives, and test inventories live in the cited Orbit task / ADR / commit message — **the task ID is the pointer, don't duplicate the detail here.** Anyone who wants the full story follows the ID.
- **Breaking changes** get one extra line max, with the migration as a phrase (`x removed → use y`). Multi-step migration guides go in the task or docs, not the CHANGELOG.

`scripts/check-changelog-style.sh` still lints whatever lands under `## Unreleased`, so it's worth drafting bullets there first if that helps you iterate before moving them into the version section — but that section is scratch space at release time now, not a per-PR accumulation target. Released `## <X.Y.Z>` sections are frozen history and are never reflowed.

### 3. Confirm breaking changes with the human

Surface the breaking-change candidate list before drafting the final section. Show each candidate with its task ID, title, and the reason it was flagged. Let the human accept, downgrade, or add to the list. Do not classify autonomously.

### 4. Bump versions

Version-bearing files and the matching MCP launch pins change every release:

| File | Field |
|------|-------|
| `Cargo.toml` | `[workspace.package].version` |
| `Cargo.lock` | refresh via `cargo update --workspace` (no third-party drift) |
| `npm/package.json` | `version` |
| `server.json` | top-level `version` and `packages[0].version` |
| `plugin/.claude-plugin/plugin.json` | `version` |
| `plugin/.codex-plugin/plugin.json` | `version` and `mcpServers.orbit.args` pin |
| `plugin/plugin.json` | `version` |
| `plugin/mcp.json` | MCP launch pin `@orbit-tools/cli@<version>` |
| `plugin/.mcp.json` | MCP launch pin `@orbit-tools/cli@<version>` |

`crates/orbit-core/assets/skills/orbit/` is the canonical skill source. After
editing it, run `scripts/sync-plugin-skills.sh`; CI runs the same script with
`--check` and rejects drift in the committed `plugin/skills/orbit/` package
mirror, so the plugin tree is never maintained independently.

The other `0.X.Y` matches in the repo (install-script doc comments, the website task pages, the Node engine pin in `website/package-lock.json`) are intentional — leave them.

### 5. Verify the build

```sh
make build
```

Must finish clean. `cargo update --workspace` should report only Orbit workspace members re-locked — investigate any third-party version movement before continuing.

If this cycle changed any CLI the npm-install smoke drives (`orbit init`, `workspace init`, or `mcp serve`), update [`scripts/smoke-npm-install.sh`](scripts/smoke-npm-install.sh) on the **same commit as the tag**. The on-tag workflow checks out that script and `server.json`, then runs the exact npm package version declared by the metadata — a post-tag script or metadata fix cannot green a tag-triggered run. Details and the post-npm triage live in [docs/runbooks/release.md](docs/runbooks/release.md#npm-install-smoke-two-artifacts).

The tag-triggered npm-install smoke must be treated as a versioned check: it
fails when the installed `@orbit-tools/cli` version does not equal the tag
version. Publish the matching npm package, then re-run the workflow from
Actions → `smoke-npm-install` → **Run workflow**, entering the release tag
(for example, `v0.14.0`) in the `tag` input. Confirm that this post-publish,
versioned run is green before considering the install chain verified. The
script's assertion can be checked without network access with:

```sh
./scripts/smoke-npm-install.sh --dry-run-version-assertion
```

### 6. Create the Orbit task

```
title:       Prepare v<X.Y.Z> release
type:        chore
tags:        ["release"]
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

Acceptance criteria: Cargo, npm, `server.json`, and all three plugin manifests
report the new version; Cargo.lock is refreshed without third-party drift; the
CHANGELOG section is in place; both npm and plugin install smokes pass.

### 7. Human approval

Per `CLAUDE.md`: do not commit until the Orbit task is explicitly approved by the human. Approval transitions the task `proposed → backlog`; the implementing agent then `start`s it.

### 8. Commit

```sh
git -c user.name='<agent>' -c user.email='<agent-email>' commit \
  --author='<agent> <agent-email>' \
  -m "chore: prepare v<X.Y.Z> release [T<task-id>]

<one or two sentence description>"
```

Use the agent commit identity that matches the model running the release (`claude <noreply@anthropic.com>`, `codex <codex@orbit.local>`, `grok <grok@orbit.local>`, `gemini <gemini@orbit.local>`, etc.) — see existing `git log` for the canonical email per agent.

### 9. Tag

```sh
git tag -a v<X.Y.Z> -m "v<X.Y.Z>

See CHANGELOG.md. Highlights:
- ...
- ...
- N breaking changes (...)"
```

Annotated tag — never lightweight. Keep the message terse; CHANGELOG is the source of truth.

### 10. Push

```sh
git push origin <branch>
git push origin v<X.Y.Z>
```

Branch first, then tag — this lets release CI resolve the tag against an already-pushed commit. `agent-main` may have moved while the prepare commit was in review; pull (rebase or merge) before push. Do not force-push a release commit.

### 10b. Promote to `main`

After the tag pushes and release CI goes green, open a PR `agent-main → main` so the release reaches the production branch. Trial-merge `origin/main` into a throwaway checkout of `agent-main` first — `website/package.json` (js-yaml pin) has conflicted across this boundary; keep the tighter constraint (the one already on `agent-main`):

```sh
gh pr create --base main --head agent-main \
  --title "release: v<X.Y.Z>" \
  --body "Promotes v<X.Y.Z>. See CHANGELOG.md."
```

Merge with a **merge commit** (`gh pr merge <N> --merge --admin`), not squash or rebase, so the release tag remains reachable from `main`'s history. The merge always creates a new commit on `main` — even with no hotfix on `main` — because `agent-main` carries the back-merge commit from the prior release (see §10c).

If `gh pr merge --merge` errors with `Merge commits are not allowed on this repository`, the repo's `allow_merge_commit` setting is off. Flip it on, merge, restore:

```sh
gh api -X PATCH repos/constellation-works/orbit -f allow_merge_commit=true
gh pr merge <N> --merge --admin
gh api -X PATCH repos/constellation-works/orbit -f allow_merge_commit=false
```

### 10c. Post-merge: back-merge to `agent-main`

After the release PR merges, `main` carries the merge commit that `agent-main` doesn't have. Back-merge `main` → `agent-main` in the same session — never defer — so the dev branch stays reachability-equivalent with prod:

```sh
git checkout agent-main
git pull --ff-only origin agent-main      # safety
git merge --no-ff origin/main \
  -m "chore: back-merge main into agent-main after v<X.Y.Z>"
git push origin agent-main
```

`agent-main` has GitHub branch protection blocking deletion (`allow_deletions: false`), so the repo-wide `delete_branch_on_merge: true` won't remove it when the PR merges — the branch is always there to back-merge into. If you find `agent-main` missing from origin (protection got dropped), recreate it from `main` and reapply the minimal protection:

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

Protection on `agent-main` exists only to prevent branch deletion
(`allow_deletions: false`); merges are never gated on CI. CI failures are
consumed asynchronously by the `qa-sweep` auto-task, which appends
remediation tasks to the queue.

If a release ever ships without the back-merge, drift compounds (N commits behind `main` after N skipped releases). Recover by either running the same back-merge above (resolves cleanly regardless of N) or, if `agent-main` has no in-flight work, reset it to `main` directly:

```sh
git push origin origin/main:refs/heads/agent-main --force-with-lease
```

### 11. Mark the Orbit task done

Update with `status: done`, `implemented_by: <agent>`, and an `execution_summary` that records the commit SHA and tag. Future releases will discover this task via the `release` tag.

## Release CI

Pushing a `v*` tag triggers `.github/workflows/release.yml`:

- **`build-release`** — `cargo build -p orbit-cli --release --locked` against four targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`. Tarballs uploaded as workflow artifacts.
- **`publish-release`** — generates `orbit-checksums.txt` (SHA256) and creates the GitHub Release with the four tarballs + checksum file attached. Release notes are auto-generated by `softprops/action-gh-release`.
- **`bump-homebrew-tap`** — rewrites `Formula/orbit.rb` in the `constellation-works/homebrew-tap` repo with the new version and the two macOS SHAs, then pushes via `secrets.TAP_GITHUB_TOKEN`. The formula is **macOS-only**; Linux users go through `install.sh`.
- **`smoke-install-macos`** / **`smoke-install-ubuntu`** — fetches `install.sh` from the tagged ref (`raw.githubusercontent.com/.../<tag>/install.sh`) and verifies `orbit --version`. Note: `install.sh` rides with the release commit — changes land in the same tag.
- **`cursor-marketplace-followup`** — runs after `publish-release` with `continue-on-error: true`. It fails loudly when `.github/cursor-marketplace-followup/<version>.ack` is missing, but it does not publish, retract, or gate the GitHub Release, Homebrew tap, or installer smokes. A warning here means the human Cursor catalog update is still outstanding.

The npm publish step was removed from the tag workflow in v0.3.1; the npm proxy package is published manually if needed.

## Cursor marketplace follow-up

Package release (git tag, GitHub Release, Homebrew, npm) does **not** update
Cursor's curated marketplace. There is no public catalog push API. After the
tag and matching npm version exist:

1. Submit or update [https://cursor.com/marketplace/publish](https://cursor.com/marketplace/publish) using [https://github.com/constellation-works/orbit](https://github.com/constellation-works/orbit) and the `plugin/` subdirectory (Agent Plugins 1.0 root `plugin.json` / `mcp.json`; do not add `.cursor-plugin`).
2. Identify stale listing **2280865**, which still describes `danieljhkim/orbit` at **0.5.1**. Historical provenance stays as-is; active distribution is `constellation-works`.
3. Install through Cursor plugin search and verify the listed version and MCP launch pin (`npx -y @orbit-tools/cli@<version> mcp serve`).
4. If review stalls, escalate to `marketplace-publishing@cursor.com`. Do not send that mail from CI.
5. When the version-specific submission is done, add `.github/cursor-marketplace-followup/<version>.ack` containing `version=<version>` to the maintained `agent-main` branch. Do not move the immutable tag to add a post-release receipt. Re-run that release tag's `cursor-marketplace-followup` job after the receipt lands; it reads `agent-main` while validating the original tag version. That file acknowledges the follow-up; it does not mean the catalog is live.

Do not retag, republish npm, rewrite immutable releases, or treat a green
package pipeline as catalog publication. Manual Cursor install remains
external. Details: [docs/runbooks/release.md](docs/runbooks/release.md#cursor-marketplace-listing).

Watch the Actions tab after pushing the tag. Real failure modes seen historically:

- **`cargo build --locked` fails**: `Cargo.lock` was not refreshed after the version bump (step 4) — fix forward in the next patch.
- **Homebrew tap step**: `secrets.TAP_GITHUB_TOKEN` expired, or the tap repo branch protection rejected the push.
- **Smoke install** (`release.yml`): a regression in `install.sh` itself, since that smoke pulls it from the tagged ref. Verify locally before tagging if `install.sh` changed in this release.
- **Npm-install smoke** (separate workflow, `smoke-npm-install.yml`): a tag run is expected to fail when npm does not yet contain the tag version. After publishing npm, manually dispatch the workflow with that tag in its `tag` input and require the versioned run to go green. A post-publish red run is either the tagged script speaking an old CLI contract, or a bad published artifact. Triage in [docs/runbooks/release.md](docs/runbooks/release.md#npm-install-smoke-two-artifacts) — do not cut a patch for a script-only fix, and do not re-dispatch the old tag after the script has moved on.

## When something goes wrong

- **Tag pushed pointing at the wrong commit**: do NOT force-update the tag. Cut the next patch release with the fix instead.
- **Release CI fails after the tag landed**: leave the tag, fix forward in the next patch release. The GitHub Release can be re-run from the Actions UI once the underlying issue is resolved (if the failure was infrastructure, not artifact-correctness).
- **Breaking change discovered post-tag that wasn't in the CHANGELOG**: amend the next release's CHANGELOG with a backdated note rather than rewriting the prior section.
- **Npm-install smoke red after a successful npm publish**: follow [docs/runbooks/release.md](docs/runbooks/release.md#npm-install-smoke-two-artifacts). A missing `orbit init` flag is a script fix on `agent-main`, not a patch release.

## Hotfix flow

For critical fixes against a released `main` (when waiting for the next `agent-main` release cycle isn't acceptable):

1. **Branch from `main`**:

   ```sh
   git checkout -b hotfix/<slug> main
   ```

2. **Land the fix via PR targeting `main`** (same CI gate as release PRs). Keep the diff minimal — hotfixes are not the place for refactors.

3. **Cut a patch release on `main`**: follow steps 1–10 of the [Release checklist](#release-checklist) but with `main` as the branch, ending with `git push origin main && git push origin v<X.Y.Z+1>`. Skip step 10b (promote) — the fix is already on `main`.

4. **Back-merge `main` → `agent-main`** in the same session — never defer:

   ```sh
   git checkout agent-main
   git merge --no-ff main
   git push origin agent-main  # or via PR if branch-protected
   ```

   This prevents the hotfix from being silently re-overwritten by the next `agent-main → main` release merge. The back-merge runs CI so regressions surface immediately.

5. If the hotfix touches a file with in-flight agent work on `agent-main`, resolve in the back-merge PR; do not rebase agent branches onto the new `agent-main` tip.
