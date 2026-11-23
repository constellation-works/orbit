# Configuration: completion holds and distributed-drain completion

## Contents

- [Completion holds](#completion-holds)
- [Distributed-drain completion policy](#distributed-drain-completion-policy)

## Completion holds

`--complete` (`orbit run ship --complete`, `orbit run auto --complete`, the
dashboard and the desktop drain) merges each task's pull request and moves the
task to `done` with nobody looking, and under
`workflow.distributed_completion = "done"` an owner lands claimed leaves the
same way. `[[completion.hold]]` rules keep a task in
`review` instead when the change is risky. A rule only lowers `done` to
`review`; it never grants completion, and a run without `--complete` is
unchanged.

Each rule sets at least one of:

- `paths` — globs matched against the files the delivery actually changes
  (the pull request diff, or the local/claimed candidate's diff against its
  base; a `!` prefix excludes), never the task's declared context files;
- `complexity_at_least` — `low`, `medium`, `hard` or `xhard`;
- `types` — `feature`, `bug`, `refactor`, `chore`;
- `tags` — task tags, compared case-insensitively;

and an optional `name` that labels it. A rule holds when any of its fields
matches; the first matching rule, in order, is recorded. Put the rules in the
workspace config (`.orbit/config.toml`) of the owner that runs `--complete`.

Example — CI, release and installer files, store migrations, sandbox and
credential code, and the hardest tasks; replace the paths with your
repository's:

```toml
[[completion.hold]]
name = "ci-workflows"
paths = [".github/workflows/**"]

[[completion.hold]]
name = "release-and-installers"
paths = ["install.sh", "RELEASING.md", "scripts/release*", "packaging/**"]

[[completion.hold]]
name = "store-migrations"
paths = ["**/migrations/**", "**/migration/**", "db/schema.sql"]

[[completion.hold]]
name = "sandbox-and-credentials"
paths = ["src/sandbox/**", "src/auth/**", "**/secrets.rs", "!**/*.md"]

[[completion.hold]]
name = "xhard"
complexity_at_least = "xhard"
```

A held run leaves the pull request open and ready and the task in `review`,
as a run without `--complete` would (with `--auto-fix` it still watches and
repairs the PR, but never merges it). A held local (`--mode local`)
candidate is not merged onto the base and not pushed, and a held owner
landing under `workflow.distributed_completion = "done"` stops before
merging or fast-forwarding anything; both leave their task in `review`.

Under the `review` policy nothing is held: a person already approved it. The task gets a comment and a
`completion_held` history entry such as
`held for review: paths .github/workflows/** matched .github/workflows/ci.yml (rule ci-workflows)`;
`orbit task show` prints it as `Completion Hold:` (JSON `completion_hold`),
the dashboard shows it under **Held for review**, and the run's `complete_pr`
step succeeds with the task in `held_task_ids`.

Approve it as any task in
review: merge the PR, then `orbit task update <id> --approve`.

A delivery whose changed files cannot be read is held by any rule with
`paths`. No-diff and already-landed completions have no diff of their own, so
only complexity, type and tag rules apply to them. The `task_complete`
transition also refuses a task matching a complexity, type or tag rule, so
no completion route reaches `done` past it. For an owner landing, the rules
are the owner's, from the owner's workspace config.

Loading fails on an unknown key, an empty rule, a blank name, glob or tag,
a glob that is not a confined relative path, an exclude-only `paths`, an
unknown type, or a complexity outside the four levels (`unassessed` is also
refused). Only errors found after a rule decodes name the rule by its
1-based position (`completion.hold[2]`): an empty rule, a blank name, glob
or tag, a bad glob, an exclude-only `paths` list, or
`complexity_at_least = "unassessed"`.

Schema errors name only
`completion.hold` and the bad field or value, without a rule position: an
unknown key, or an unknown complexity or type value. Verify with
`orbit config show completion.hold` and `orbit config keys`.

## Distributed-drain completion policy

`workflow.distributed_completion` decides how far the owner takes an accepted
distributed-drain handoff. It admits only `review` (the default) or `done`;
any other value fails config load. The value in force is always the owner's
own configuration: admission captures it into the claim, acceptance records
it as the handoff's authorization, and the landing pipeline rechecks it
before merging.

A follower cannot grant itself this authority, and nothing a
follower sends changes what the owner requires.

| Value | Behavior |
|---|---|
| `review` | Every accepted handoff waits for an operator's **Approve handoff** on the owner's dashboard before anything lands. |
| `done` | Acceptance authorizes the handoff and the owner's landing pipeline merges and completes it, as `--complete` does for owner tasks. |

Set it in the owner's workspace, from the owner's registered checkout:

```bash
orbit config set workflow.distributed_completion done
```

Stay on the default until the drain and its landing path are routinely clean,
then opt in one workspace at a time.

The key is read when each owner runtime
opens, so opting in or withdrawing reaches long-lived owner processes only
after they restart: restart `orbit web serve`, any active drain, and any
running landing worker's owner process after changing the key (a landing
worker already running keeps the value it opened with until it finishes),
then verify with `orbit config get workflow.distributed_completion`.

Reverting is the same write with `review`; because landing rechecks the key,
withdrawing `done` stops handoffs authorized by the owner policy that have
not landed yet. It does not stop a handoff an operator already approved,
which lands through the operator authorization path. Claims admitted before
opting in keep their `review` contract and still need **Approve handoff** on
the owner's dashboard.

`done` changes only who authorizes completion. It skips none of the gates
captured at admission: the owner's required validation commands, any review
gate in effect, and owed post-deploy criteria still hold — a handoff with
outstanding post-deploy measurements still settles in `review` until an
operator records each criterion met. For execution mechanics, see
[Distributed drain](../../orbit/references/setup/distributed-drain-setup.md).

Verify the readback and the effective layer with its source:

```bash
orbit config get workflow.distributed_completion
orbit config get --scope workspace workflow.distributed_completion
orbit config show
orbit config show --all
```

`orbit config show` labels configured rows with their `workspace` or `global`
source, and rows with no configured value show `default`. The text rows do not
include file paths: see the `Layers (later wins)` banner for layer paths, or
use `orbit config show --json` for per-key path provenance. A workspace value
overrides the global one. Use `orbit config show --scope workspace` to inspect
one file's values without inheriting the other layer.
