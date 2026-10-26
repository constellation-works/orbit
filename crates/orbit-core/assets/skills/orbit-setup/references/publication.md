# Task publication and recovery

Publication writes a validated task snapshot to a **Git repository dedicated
to task publication**. One such repository may be shared by several
workspaces, each on its own branch (see below). It supports offline inspection
and deliberate recovery. It does not replicate a live Orbit store, transfer
workspace ownership, ship source code, or mark tasks done. Publishing tasks
and pushing an implementation branch are separate actions.

These are CLI administration operations on the intended host. They are not
advertised task MCP tools. Apply the authority rules in
[tool-surface.md](../../orbit/references/tool-surface.md) before using them.

## Bind an owned workspace

Prerequisites: initialized machine identity, a registered owner checkout with a
portable source Git remote, Git access to a separate publication repository,
and an explicit choice of repository visibility and access. Orbit does not
create the hosted repository or enforce its privacy; output labels privacy
`operator-managed`. Task prose and attachments may contain sensitive information.

From the selected owner checkout:

```bash
orbit workspace show
orbit config get machine.id
orbit workspace publication bind --remote <publication-git-url> --publication-id <lineage-id>
orbit workspace publication show --json
```

The default branch is `main`; `--branch <name>` also accepts an ordinary
`refs/heads/*` ref. The opaque publication ID identifies the lineage and must be
unique on this machine. Binding is owner-local registry metadata, not a config
file to copy between hosts. It records workspace ID, source remote fingerprint,
authority machine ID, publication remote/branch, and the last successful
snapshot generation/commit when one exists. Start with an absent publication
branch for this lineage; an ordinary README/source commit already on that
bound branch is not a valid publication envelope and is refused, never adopted
or overwritten.

### Share one repository across workspaces

A lineage is scoped to its bound branch, not to the whole repository. Several
workspaces may bind the same `--remote` when each uses a distinct `--branch`
(for example `--branch ws/<workspace-name>`) and a distinct
`--publication-id`:

```bash
# From workspace A's owner checkout:
orbit workspace publication bind --remote <publication-git-url> --publication-id pub_a --branch ws/a
# From workspace B's owner checkout, same remote:
orbit workspace publication bind --remote <publication-git-url> --publication-id pub_b --branch ws/b
```

The first publish of each lineage creates generation 1 on its own branch
(fast-forward compare-and-swap that only succeeds while that ref is still
absent), even when the repository already holds other workspaces' publication
branches; those other branches are never read, written, or validated, and
their tips do not move. If the bound branch already exists but does not carry
this lineage's valid envelope (foreign content, or a different publication
ID, workspace, or authority), publishing still refuses as an authority
conflict and changes nothing remotely.

By design there is no repo-wide guard against an obviously foreign
repository (for example a non-publication default branch carrying ordinary
source commits): checking it would mean inspecting branches outside this
lineage. The bound branch is the unit of trust — an absent bound branch is
safe to create, and a present bound branch must already be this lineage's
envelope. Keep the repository private and dedicated to publication anyway;
task prose and attachments may contain sensitive information, and anyone with
write access to the repository could create a bound branch first and block
(or, with the matching identity facts, impersonate) a lineage.

Credentials in a URL, a local path, a checkout-local alias such as `origin`,
and a publication remote equivalent to the source repository are refused.
Use the explicit HTTPS askpass bridge below or unattended SSH authentication.
`bind` establishes the binding;
`rebind` explicitly replaces lineage using the same flags. Do not use rebind as
an automatic conflict-recovery retry.

```bash
orbit workspace publication remove --confirm
```

Remove deletes the local binding and last-success record only; it does not
delete the publication repository or erase published history.

## Transfer the source repository

Use `orbit workspace source-remote rebind --remote <new-source-url>` on the
declared owner after a repository transfer. It updates the registered source
and the task-registry pairing in the same audited command. Pairing comparisons
normalize existing raw URLs to their portable host/owner/repository identity:
SSH and HTTPS spellings of one repository compare equal, while a different
owner or repository requires the explicit source rebind. No store edits or
data migration are needed.

An existing publication binding prevents a source identity change. Save
`orbit workspace publication show --json`, remove that binding with
`orbit workspace publication remove --confirm`, preview the source rebind with
`--dry-run --json`, then apply without `--dry-run`. Update Git `origin`
separately. Use `orbit workspace publication rebind` with reviewed settings,
a new publication ID and an empty publication branch, then verify with
`orbit workspace publication show --json`. Old snapshots retain their original
source identity; the new binding starts without last-success metadata.

For a transfer already performed by an older Orbit version, repeat
`orbit workspace source-remote rebind --remote <registered-source-url>`.
It reconciles a stale task pairing even when the source reports `unchanged`.
The workspace registration is saved before the task pairing; if the pairing
write fails or the command is interrupted, repeat the same command to finish.
`--dry-run` leaves both pairings untouched.

## Authenticate unattended HTTPS publication

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

## Publish and choose attachment exposure

```bash
orbit task publication publish --json
orbit task publication status --json
```

Publishing reads the owner's canonical tasks and validates complete bundles
before advancing the remote branch. It publishes a workspace snapshot, not an
individual task selection. It excludes host configuration, credentials, locks,
scheduler state, and execution telemetry as independent stores; those are not
recoverable from a task publication. Task fields and history remain task data.

| Policy | Result |
|---|---|
| `--attachments fail` (default) | Refuse a task snapshot containing attachments. Useful when accidental attachment exposure must stop publication. |
| `--attachments omit` | Publish task content without attachment blobs/manifests; record an omission ledger with task ID, path, size, and hash. Recovery is explicitly incomplete. |
| `--attachments include` | Include validated attachments subject to size, path, and sensitivity checks. |

Included attachments default to 10 MiB per file and 100 MiB total; adjust with
`--max-file-bytes` and `--max-total-bytes`. Repeat `--deny-pattern` to add rejected
path globs. Built-in denials cover `.env`, `.env.*`, PEM/key files, `id_rsa`, and
`credentials.json`. Path, manifest, size, and content-integrity checks still
apply even if scanning is waived.

The shipped CLI has no sensitivity-scanner configuration. Include therefore
fails closed unless the operator deliberately uses
`--attachments include --allow-unscanned-attachments`. That flag does not make
the data safe or private. Prefer `omit` when blobs are unnecessary; review the
content and intended audience before explicitly opting into unchecked inclusion.

Unchanged validated content can be a no-op. New content advances a monotonic
generation with a fast-forward, compare-and-swap push against the observed tip.
A moved, deleted, divergent, or unexpected branch is an authority conflict;
Orbit does not merge competing snapshots, choose by timestamp, or overwrite the
remote. Inspect the binding and remote evidence before repairing authority.

`status` reports `never-published`, `current`, or `authority-conflict` by comparing
the owner-local last-success record with the validated remote tip. A lineage
whose bound branch is absent and which has no recorded success is
`never-published`, regardless of what other branches the repository holds. **Current
means that publication bookkeeping agrees; it does not prove that tasks have
not changed since publication.** Publish again to capture later task mutations.

## Inspect without adopting live state

Obtain expected identity values from the owner, for example from
`workspace publication show --json`; do not trust labels from an unknown
repository as their own authority proof.

```bash
orbit task publication inspect \
  --workspace-id <logical-workspace-id> \
  --source-remote <source-git-url> \
  --publication-id <lineage-id> \
  --authority-machine-id <owner-machine-id> \
  --remote <publication-git-url> --branch main --json
```

Add `--commit <full-object-id>` to pin an exact historical commit. Inspection
fetches and validates identity, envelope, bundle content, and attachment
integrity. It returns labelled snapshot content and completeness information;
it does not import tasks or turn a replica into an owner. Report the commit,
generation, authority, and omissions when answering from a snapshot. A historic
snapshot is not a live answer about task status.

## Restore deliberately

Use the same identity arguments with `orbit task publication restore`, plus
`--confirm`. Inspect and preferably pin `--commit` first. The selected destination
must be the same logical workspace, owned locally by the same authority machine
ID, with an owner checkout and matching source identity. Copying a publication
to a new host does not satisfy this ownership contract; this command is not an
ownership-transfer procedure.

By default recovery requires an empty destination task set. With
`--allow-identical-retry`, existing collisions must be byte-identical; divergent
records are refused rather than merged or silently overwritten. Check
`restored_task_ids`, `already_present_task_ids`, `completeness`,
`omitted_attachments`, and the projection result before claiming recovery.
An omitted-attachment snapshot remains incomplete after restore.

Task publication is only one recovery artifact. Preserve the global
`config.toml` (including its `[machine]` identity) and other required
configuration through the operator's separate backup process;
never rewrite machine IDs or registry files to bypass a restore denial.
