---
type: runbook
summary: Bind, authenticate, publish, verify, inspect, and recover an Orbit task-publication repository.
tags: [operations, backup, recovery, git, task-publication]
paths: ["crates/orbit-cli/src/command/task/publication.rs", "crates/orbit-cli/src/command/workspace/publication.rs", "crates/orbit-cli/src/command/workspace/source_remote.rs", "crates/orbit-store/src/workflow/task/**"]
related_features: [task-publication, task-artifacts, host-registry]
related_artifacts: [ORB-11077, ORB-11142, ORB-11145, ORB-11426]
last_validated: 2026-10-25
---

# Publish Orbit Tasks to a Dedicated Repository

Use this runbook to configure task publication for an owned workspace, publish a safe
snapshot, diagnose authentication or workspace-identity failures, or recover that snapshot
under the same authority.

Task publication is an explicit, task-only durability channel. It does not back up audit
events, run history, claims, reservations, configuration, machine identity, or runtime caches.
No task mutation publishes automatically, and v1 seeds no publication routine. Keep the
global-root and database backups described in [Inventory and Protect Orbit State](./state-and-backup.md).

## Prerequisites and safety

Before binding a workspace, prepare:

- a registered local **owner** checkout, not a replica;
- one Git repository dedicated to task publication; it may be shared by several Orbit workspaces, each bound to its own branch and publication ID (the bound branch for this workspace must not exist yet, or must already carry this lineage);
- provider-side private visibility, collaborators, retention, and branch protection;
- working SSH or HTTPS credentials for that repository; and
- the logical workspace selector returned by `orbit workspace list --all --format json`.

Orbit cannot prove provider-side privacy or erase retained Git history. Never reuse the source
repository as the publication destination, put credentials in the remote URL, or assume that a
private repository makes every attachment safe to publish.

Set these shell variables to the real values for the operation:

```sh
ORBIT_WORKSPACE=ws_example
ORBIT_CHECKOUT=/path/to/source-checkout
ORBIT_PUBLICATION_REMOTE=git@example.com:backups/example-tasks.git
ORBIT_PUBLICATION_ID=pub_example_primary
```

Use the stable logical `ws_*` ID or registered workspace name for `ORBIT_WORKSPACE`. A linked
worktree or migrated checkout can use a different runtime task partition internally; that is
not a second logical workspace and is not a reason to rebind publication. Publication commands
honor the selected logical identity after ORB-11142.

## Inspect identity before mutation

Establish the binary, source repository, workspace registration, and existing binding before
changing anything:

```sh
command -v orbit
orbit --version
git -C "$ORBIT_CHECKOUT" remote get-url origin
orbit workspace list --all --format json
orbit --workspace "$ORBIT_WORKSPACE" workspace publication show --json
```

The global `--workspace` option must precede `workspace publication` or `task publication`.
Continue only when the selected registration names the intended owner checkout and source
remote. A missing binding is expected during first setup.

When the source repository itself moves, do not edit the binding fingerprint or registry by
hand. `workspace source-remote rebind` refuses a changed source identity while a publication
binding exists. Capture this command's JSON output, remove the binding explicitly, perform and
verify the source rebind, then use `workspace publication rebind` with the reviewed captured
remote, a new publication ID, and an empty publication branch. The new local binding starts
without last-success metadata; the operation does not migrate or rewrite any existing
publication snapshot. See
[Rebind the source remote after a repository move](./state-and-backup.md#rebind-the-source-remote-after-a-repository-move)
for the full inspection, dry-run, verification, and rollback sequence.

Source rebinding also reconciles the task-registry source pairing. Existing raw-URL rows
remain compatible: pairing comparisons use portable host/owner/repository identity, so SSH
and HTTPS URLs for the same repository compare equal. A different owner or repository still
requires the audited source rebind. If an older version already moved the registered source
but left a stale task pairing, repeat `workspace source-remote rebind --remote <registered-source-url>`
before binding publication. It repairs the pairing even when the source reports `unchanged`.
The workspace registration is saved first; repeat that command after an interruption or a
task-pairing write failure. Dry runs change neither pairing. Never edit either store by hand.

## Choose durable authentication

### SSH

SSH is the simplest durable choice for unattended publication. Use a non-credential-bearing
SSH remote and verify that the current process can reach the Git host before binding. Agent or
key configuration remains an operator responsibility and must not be embedded in the binding.

```sh
git ls-remote "$ORBIT_PUBLICATION_REMOTE"
```

An empty result with exit status 0 is valid for a new empty repository.

### HTTPS with a persistent askpass bridge

Publication deliberately ignores system and global Git configuration, including
helpers installed by `gh auth setup-git`. The supported HTTPS path is an explicit
`GIT_ASKPASS` executable. Orbit passes through `GIT_ASKPASS`, `PATH`, `HOME`,
`XDG_CONFIG_HOME`, `GH_CONFIG_DIR`, `GH_TOKEN` and `GITHUB_TOKEN` when set, so the
bridge can use the service account's existing GitHub CLI authentication. SSH
publication can instead use `SSH_AUTH_SOCK`, `GIT_SSH` or `GIT_SSH_COMMAND` and
that account's SSH configuration. Orbit sets `GIT_TERMINAL_PROMPT=0` and
`GCM_INTERACTIVE=never`; neither path may require a TTY or an unlock prompt.

For GitHub HTTPS, authenticate `gh` once as the account that runs Orbit, with
write access to the dedicated publication repository. Replace `OWNER/WORKSPACE-tasks`
with your empty repository. If already bound, verify the remote with
`orbit workspace publication show` and skip `bind`:

```sh
orbit workspace publication bind \
  --remote https://github.com/OWNER/WORKSPACE-tasks.git \
  --publication-id pub_my_tasks
gh auth login --hostname github.com
gh auth status --hostname github.com
mkdir -p "$HOME/.local/libexec"
cat > "$HOME/.local/libexec/orbit-publication-askpass" <<'ASKPASS'
#!/bin/sh
case "$1" in
  *"Username for 'https://github.com'"*) printf '%s\n' x-access-token ;;
  *"Password for 'https://x-access-token@github.com'"*)
    exec gh auth token --hostname github.com ;;
  *) exit 1 ;;
esac
ASKPASS
chmod 700 "$HOME/.local/libexec/orbit-publication-askpass"
export GIT_ASKPASS="$HOME/.local/libexec/orbit-publication-askpass"
export LC_ALL=C
orbit task publication publish
orbit task publication status
```

The bridge contains no token and sends the token only through Git's credential
pipe. Keep credentials out of URLs, command arguments and diagnostic output;
do not enable shell tracing in the bridge. It only answers prompts for
`github.com`; use a separate host-scoped bridge for another provider.
Publication disables Git tracing and withholds transport stderr because helpers
and servers can echo credentials there. A transport failure should be diagnosed
by checking `gh auth status`, repository access and the service environment.

For a clock-tick/systemd run, keep this bridge at its persistent absolute path
and set `GIT_ASKPASS` and `LC_ALL=C` in the **Orbit service's** environment, not
just an interactive shell. For example, a user-service drop-in can contain:

```ini
[Service]
Environment=GIT_ASKPASS=%h/.local/libexec/orbit-publication-askpass
Environment=LC_ALL=C
```

Reload and restart the installed service after applying its drop-in. Ensure its
`PATH` includes `gh` (or use its absolute executable path in the bridge), and its
`HOME`/`GH_CONFIG_DIR` and credential store are accessible without an interactive
keyring unlock. If using a token environment variable, provision it through the
service's private credential mechanism rather than putting its value in a unit
file or command line. Verify `publish` and `status` from that same service
account/environment with stdin closed before relying on a scheduled run.

## Bind the publication repository

Bind from any checkout only when `--workspace` explicitly selects the declared owner:

```sh
orbit --workspace "$ORBIT_WORKSPACE" workspace publication bind \
  --remote "$ORBIT_PUBLICATION_REMOTE" \
  --publication-id "$ORBIT_PUBLICATION_ID" \
  --branch refs/heads/main \
  --json

orbit --workspace "$ORBIT_WORKSPACE" workspace publication show --json
```

The binding is machine-local and records logical workspace, portable source identity,
publication remote, branch, publication lineage, and authority machine. It must not contain
credentials or checkout paths. The bind and show operations do not contact the remote; add the
persistent `GIT_ASKPASS` environment assignment later to publish, status, inspect, and restore
commands that use HTTPS.

Use `workspace publication rebind` only to deliberately replace the lineage. Use `workspace
publication remove --confirm` only to remove local binding and last-success state; neither
operation deletes or rewrites the remote repository.

## Publish safely

The attachment policy is an explicit exposure decision:

| Policy | Behavior | When to use it |
| --- | --- | --- |
| `fail` | Refuses the entire publication if any attachment exists. | First attempt and strict no-attachment workspaces. |
| `omit` | Publishes core task records plus an omission ledger. | Safe publication when attachments exist but are not approved. |
| `include` | Applies size and deny-pattern limits, then includes admitted bytes. | Only after deliberate content review and scanner policy approval. |

Start with the fail-closed default so Orbit inventories any blocking attachment without
publishing a partial policy choice:

```sh
orbit --workspace "$ORBIT_WORKSPACE" task publication publish \
  --attachments fail \
  --json
```

If known attachments are not approved for Git storage, publish only the core task records:

```sh
orbit --workspace "$ORBIT_WORKSPACE" task publication publish \
  --attachments omit \
  --json
```

The store exposes an attachment sensitivity-scanner hook, but the current Orbit CLI
does not configure a scanner. `include` therefore refuses attached bytes unless the
operator deliberately adds `--allow-unscanned-attachments`; repository privacy is not a
substitute for reviewing those bytes. If a secret ever reaches Git history, rotate the
credential and perform provider-side history remediation. Deleting it from the latest commit
is not erasure.

Publication uses an Orbit-owned cache. It does not check out, switch, stage, or change the
source-worktree branch.

If a push lands but saving the local success record fails or is interrupted, retry the same
publish command. Orbit uses private recovery evidence keyed by commit to report `reconciled`
without another push, even after a concurrent attempt loses its compare-and-swap. Repeated
retries keep this evidence until a later publish observes the durably recorded success.
That publish prunes records through the acknowledged generation; newer attempts remain.
Preserve the owner's `state/task-publication/` cache while recovering a lost success save.
Legacy single-file recovery records are still recognized. An unrelated remote tip continues
to require authority resolution.

## Verify the snapshot

After every publish, compare the owner-local success record with the validated remote tip:

```sh
orbit --workspace "$ORBIT_WORKSPACE" task publication status --json
```

Success has all of these properties:

- `state` is `current`;
- local and remote generation numbers match;
- local and remote commit IDs match; and
- `incomplete_attachments` matches the selected attachment policy (`true` after `omit`).

The first publish creates generation 1 and the configured branch when that branch is absent — including in a repository that already holds other workspaces' publication branches, whose tips it never reads or moves. Branch creation is a compare-and-swap that fails if the branch appears first.
Later publishes advance the same linear lineage with compare-and-swap semantics.

## Inspect and recover a publication

Inspect and restore refuse embedded remote credentials before invoking Git or creating a
consumer cache. This includes a token used as the HTTP(S) username with no password;
use an explicit `GIT_ASKPASS` helper or SSH authentication instead. Refusal diagnostics redact userinfo.

Inspection is read-only and does not require an owner-local publication binding. Supply the
expected pairing facts rather than trusting the repository to declare its own identity:

```sh
orbit --workspace <local-consumer-workspace> task publication inspect \
  --workspace-id <published-logical-workspace-id> \
  --source-remote <published-source-remote> \
  --publication-id <publication-id> \
  --authority-machine-id <authority-machine-id> \
  --remote <publication-remote> \
  --json
```

The result labels every record with publication time, generation, workspace, source identity,
authority, publication ID, commit, freshness, completeness, and `render_authority: snapshot`.
It is not live owner state. Pairing mismatch, unsupported schema, corrupt JSONL, changed bundle
or attachment bytes, invalid Git lineage, or a symlink or other non-regular entry at the
envelope or under `tasks/` returns no trusted task projection. The link is refused before its
target is read.

Restore the global `config.toml` (including its `[machine]` table) plus `workspaces.json` authority evidence first.
V1 has no authority-transfer command: the selected recovery workspace must be an owner checkout
whose machine, logical workspace ID, and source remote match the publication. Start with an
empty canonical task destination:

```sh
orbit --workspace <recovery-logical-workspace-id> task publication restore \
  --workspace-id <published-logical-workspace-id> \
  --source-remote <published-source-remote> \
  --publication-id <publication-id> \
  --authority-machine-id <authority-machine-id> \
  --remote <publication-remote> \
  --confirm \
  --json
```

The confirmation is mandatory. The default refuses any non-empty destination. For an
interrupted or repeated recovery, `--allow-identical-retry` admits only byte-identical task-ID
collisions; one non-identical collision aborts the whole restore without renumbering or partial
replacement. Use ordinary `task export` and `task import` with renumbering for migration between
unrelated authorities.

## Diagnose failures

| Symptom | Check and response |
| --- | --- |
| Git reports that it cannot read a username, password, or terminal prompt. | Global credential helpers are intentionally isolated. Use SSH or pass a persistent `GIT_ASKPASS` bridge explicitly to the Orbit command. |
| A binding appears missing or belongs to another checkout. | Run `orbit workspace list --all --format json`, then repeat `workspace publication show` with the intended logical `--workspace` selector before changing the binding. |
| The logical `ws_*` ID differs from an internal runtime task partition. | This is supported after ORB-11142. Keep selecting the logical workspace; do not rebind or rewrite task storage to make the IDs match. |
| The publication remote is rejected during bind. | Remove credentials from the URL, confirm it differs from the source remote, and use a dedicated repository. |
| First publication finds an unrelated or non-publication commit on its bound branch. | Stop. Orbit will not adopt or overwrite that branch; bind an unused branch instead. Other branches in the same repository do not block a first publish. |
| `--attachments fail` names one or more files. | Review the named files. Use `omit` for a core-record snapshot, or use `include` only after explicit exposure approval. |
| Status reports a moved branch or authority conflict. | Stop publishing and identify the unexpected writer or stale binding. Orbit will not merge or force-push the branch. |
| The registered checkout path is genuinely stale or invalid. | Restore or re-point the checkout and run `orbit workspace init --force` from the intended repository. A different runtime partition alone is not evidence of stale registration. |

If `workspace init --force` cannot reconcile a proven stale registration, preserve
`~/.orbit/workspaces.json`, capture `workspace publication show --json`, stop active Orbit jobs,
and escalate before removing the workspace registration. Deregistration can discard
machine-local publication binding and last-success evidence even though it does not delete the
repository's `.orbit/` directory or the remote publication history.

## Related references

- [Inventory and Protect Orbit State](./state-and-backup.md) — complete state inventory and
  WAL-safe database backup.
- [Recover a Corrupted Orbit Database](./database-recovery.md) — store-level disaster recovery.
- [Task publication design](../design/task-publication/1_overview.md) — authority and protocol
  model behind this procedure.
