# Maintenance: deploying a local build

## Deploying a local build pinned to a source commit

To ship an unreleased fix, build a clean checkout of the full commit SHA and run
the **candidate's** updater so an older installed build is bootstrapped:

```sh
set -eu
SHA='replace-with-the-full-40-or-64-hex-commit'
WORKSPACE='/absolute/path/to/the-intended-workspace'
test -z "$(git status --porcelain)" # start in a clean Orbit source checkout
git fetch --all
git cat-file -e "$SHA^{commit}"
git checkout --detach "$SHA"
test "$(git rev-parse HEAD)" = "$SHA"
test -z "$(git status --porcelain)"
cargo build --release --locked -p orbit-cli
C="$(pwd -P)/target/release/orbit"
"$C" update --local-candidate "$C" --source-commit "$SHA" \
  --write-candidate-manifest ~/orbit-candidate-"$SHA".json
# Handover-capable clients need no quiescing; run from the intended workspace for discovery/convergence.
cd "$WORKSPACE"
"$C" update --local-candidate "$C" --candidate-manifest ~/orbit-candidate-"$SHA".json \
  --source-commit "$SHA" \
  --install-target ~/.orbit/bin/orbit --json
```

Trust is reported literally as `operator_attested` with `signed_release: false`:
Orbit computes the digest from the accepted bytes and the target from the
executable header, but the source commit is the operator's attestation. Release
signature verification is unchanged and never satisfied by a local candidate;
each platform builds its own candidate from the same commit.

`--install-target` must be the managed `orbit` executable (not a symlink, owned
by the invoking user); package-manager or unknown installs are refused. The
update admits the invocation, host-global, and selected workspace roots for the
whole staging, backup, swap and convergence sequence. Workspace discovery
follows the current directory independently of `HOME`; run from the intended
workspace, and use an isolated checkout as well as isolated `HOME` and install
target for smoke checks.

The JSON report's `workspace_root` and
`admission_roots` show what was selected and held. The staged copy is what
installs even if the candidate path changes, an equal version with a different
digest replaces, and only a live long-lived client that registered a resume
capability the candidate resumes is admitted beside it and hands over after the
swap — in this tree that is a non-remote stdio `mcp serve` whose stdin can be
polled and whose capability is `mcp-stdio-v1` —

while short-lived clients,
including clock ticks and one-shot commands, are waited for — do not
quiesce idle handover-capable clients.

A live dashboard (`orbit web serve`), a
drain (`orbit job run-pipeline-worker`), `orbit mcp listen`, an `mcp serve` that
did not register a capability the candidate resumes, and a holder that never
registered (a v1 binary or a sandboxed child that cannot write the Orbit root)
make it refuse before the installed executable is replaced; quiesce that holder,
retry, then reconnect.

Rerunning the
same command is idempotent and finishes partial convergence; `needs_recovery`
(exit 4) carries the exact retry command in `local_candidate.retry_command`.
See the [upgrades runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/upgrades.md) for the full procedure.

### Reclaim what deploys leave behind

Build directories under `~/.orbit/deployments/`, older executable copies in
`~/.orbit/bin/` (`orbit.bak-*`, `orbit.before-*`, `*.stale-*` and similar) and
`~/.orbit/orbit.db.bak*` store copies are not removed by any other GC class.
Once the upgrade is verified:

```sh
orbit gc deployments            # report each entry, its bytes and whether it is kept
orbit gc deployments --confirm  # remove what the report lists as would_remove
```

Each class keeps its newest `--keep` entries (default 2). `orbit.previous`, and
any deployment whose candidate manifest names the installed or rollback
executable's digest, are always kept and do not count toward `--keep`. Links are
unlinked, never followed. See the ["Reclaim old deployments and backups"
section of the upgrades runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/upgrades.md#reclaim-old-deployments-and-backups) for the full rule.
