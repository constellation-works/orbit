# Task publication and recovery

Publication writes a validated task snapshot to a **dedicated Git repository**.
It supports offline inspection and deliberate recovery. It does not replicate a
live Orbit store, transfer workspace ownership, ship source code, or mark tasks
done. Publishing tasks and pushing an implementation branch are separate actions.

These are CLI administration operations on the intended host. They are not
advertised task MCP tools. Apply the authority rules in
[tool-surface.md](../tool-surface.md) before using them.

## Bind an owned workspace

Prerequisites: initialized host identity, a registered owner checkout with a
portable source Git remote, Git access to a separate publication repository,
and an explicit choice of repository visibility and access. Orbit does not
create the hosted repository or enforce its privacy; output labels privacy
`operator-managed`. Task prose and attachments may contain sensitive information.

From the selected owner checkout:

```bash
orbit workspace show
orbit host show
orbit workspace publication bind --remote <publication-git-url> --publication-id <lineage-id>
orbit workspace publication show --json
```

The default branch is `main`; `--branch <name>` also accepts an ordinary
`refs/heads/*` ref. The opaque publication ID identifies the lineage and must be
unique on this machine. Binding is owner-local registry metadata, not a config
file to copy between hosts. It records workspace ID, source remote fingerprint,
authority machine ID, publication remote/branch, and the last successful
snapshot generation/commit when one exists. Start with an empty publication
branch; an ordinary README/source commit is not a valid publication envelope.

Credentials in a URL, a local path, a checkout-local alias such as `origin`,
and a publication remote equivalent to the source repository are refused.
Use normal Git authentication outside the URL. `bind` establishes the binding;
`rebind` explicitly replaces lineage using the same flags. Do not use rebind as
an automatic conflict-recovery retry.

```bash
orbit workspace publication remove --confirm
```

Remove deletes the local binding and last-success record only; it does not
delete the publication repository or erase published history.

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
the owner-local last-success record with the validated remote tip. **Current
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

Task publication is only one recovery artifact. Preserve host identity and
other required configuration through the operator's separate backup process;
never rewrite machine IDs or registry files to bypass a restore denial.
