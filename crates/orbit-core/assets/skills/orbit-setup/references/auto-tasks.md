# Auto-tasks: recurring work as data

An auto-task is a definition that **mints a task** on its own schedule. The host
clock tick evaluates every definition in each registered owner checkout, so
adding a recurring chore is a new YAML file — never new code or a new routine.

Use an auto-task when the recurring thing is *work someone should do*: a QA
sweep, a dependency audit, a stale-branch review, doc validation. Use a
[routine](automation.md) when the recurring thing is *a pipeline to run*.

## Prerequisites

Definitions fire when their `enabled` switch is on and the host clock
is enabled. See [automation.md](automation.md). No scheduler routine or job is
required.

## Creating one

```bash
orbit auto-task add \
  --name dependency-audit \
  --description "Weekly check for outdated or vulnerable dependencies" \
  --cron "0 9 * * 1" \
  --title "Audit dependencies" \
  --body "<the instruction the executing agent receives>" \
  --criterion "Every outdated dependency is either upgraded or has a filed exception" \
  --criterion "The audit command and its output are recorded in the execution summary" \
  --type chore \
  --tag dependency-audit \
  --required-tools github.run.list \
  --priority medium \
  --complexity medium
```

Definitions land in `.orbit/auto_tasks/<name>.yaml`. `.orbit/` is per-user
checkout state that git ignores, so the file is not reviewed in a PR; read it
back with `orbit auto-task show <name>` before enabling it.

| Flag | Notes |
|---|---|
| `--name` | Unique in the workspace; lowercase alphanumeric plus `-`/`_`. |
| `--cron` / `--every-minutes` / `--deliveries-landed` | Mutually exclusive; `add` requires exactly one. Cron is 5-field, host-local time. `--deliveries-landed` takes the delivery-trigger JSON. |
| `--title` / `--body` | The minted task's title and description. |
| `--criterion` | Repeatable. This is the acceptance criteria of every minted task — write them as observably as you would for a hand-authored task. |
| `--type` | `feature`, `bug`, `refactor`, `chore`. Defaults to `chore`. |
| `--tag` | Repeatable, and worth setting: it is how the minted tasks are found later. A provenance tag is added automatically. |
| `--required-tools` | Repeatable exact canonical tool name. `--required-tool` is an alias. Scheduled fires and manual `mint` copy the normalized list onto each task. |
| `--complexity` | Optional assessed complexity: `low`, `medium`, `hard`, or `xhard`. It is copied to every minted task. |
| `--status` | Status the minted task enters. Defaults to `backlog`; use `proposed` when a human should approve each instance before it becomes shippable work. |
| `--crew` | Crew override for minted tasks. |
| `--dedupe` | `skip-if-open` (default) or `always`. `skip_if_open` is also accepted. The definition file, the JSON document, and `show` all print the canonical `skip_if_open` / `always` token. |

## Minted tasks inherit template complexity

Set `template.complexity` (or `--complexity`) to `low`, `medium`, `hard`, or `xhard` to
give every minted task an explicit assessment. The value round-trips through
the definition YAML, list/show surfaces, and both scheduled and manual minting.
It also allows automatic crew selection to use the matching
`workflow.*_complexity_crews` pool when no template crew overrides it.

Legacy and custom definitions may omit `complexity`. They remain valid and mint
the explicit non-answer `unassessed`, preserving the historical behavior rather
than silently treating omission as `medium`. `unassessed` round-trips through
task persistence, but human and agent `task.add`/`task.update` surfaces still
accept only assessed values (`low`, `medium`, `hard`, `xhard`); they can assess such a minted task but cannot
re-clear the assessment. Ordinary updates that omit complexity continue to
succeed. An omitted template falls through to the default crew because there is
no `unassessed` complexity pool.

## Dedupe is the important field

`skip-if-open` skips the fire while a previously minted instance is still open.
Without it a stalled backlog accumulates identical tasks every cycle — a weekly
chore nobody has picked up becomes fifty copies by year's end. Choose `always`
only when each instance is genuinely independent of the last.

## Skipping a tick when nothing landed

`dedupe` cannot tell whether there is anything to do. A sweep that reviews or
validates what landed on an integration branch has nothing to do while that
branch is quiet, yet it still boots a worktree and an agent to say so. The
optional `skip_if_unchanged` block — a definition-file field, edited in the
YAML rather than through a CLI flag — suppresses that mint:

```yaml
skip_if_unchanged:
  ref: agent-main                      # integration branch whose tip is compared
  cursor:
    tags: [code-review, no-diff-expected, auto-task:code-review]
    legacy_tags: [code-review-sweep, no-diff-expected]
```

The scheduler resolves the ref's tip, finds the newest `done` chore carrying
every tag in `cursor.tags` (falling back to `legacy_tags` only when the current
tags select nothing), and reads that sweep's `sweep-cursor.json` artifact —
`{"schema_version":1,"ref":"<branch>","cursor":"<commit SHA>"}`, which the
sweep's own template is responsible for writing. When the tip is already
covered by that cursor the tick skips without consuming its slot, so the first
new commit fires the pending occurrence. The skip is reported by
`orbit clock tick` and kept on the cursor, so `orbit auto-task show <name>`
and the dashboard name the reason and both SHAs.

It **fails open**: no completed sweep, a missing or malformed cursor, an
unresolvable ref or commit, or any probe failure mints as before and records
why. A precondition that cannot be answered never stops a sweep.

Include the definition's `auto-task:<name>` provenance tag in `cursor.tags`:
only tasks minted from the definition carry it, so a hand-filed chore that
happens to share the other tags can never become the cursor task.

The shipped `code-review`, `qa-sweep` and `qa-full-sweep` defaults carry the
block against the workspace's base branch. Delivery-triggered definitions do not need it — they
never fire on a quiet tree.

## Skipping a tick when nothing is blocked

`skip_unless_blocked_candidates: true` — also a definition-file field — mints
only when at least one blocked task's current block episode awaits triage,
under the rule the [blocked-task triage](automation.md#blocked-task-triage)
applies. Otherwise the tick skips with reason `no_blocked_candidates` and a
detail naming how many blocked tasks it read. The skip is reported and kept
on the cursor like a `skip_if_unchanged` skip, and `orbit auto-task show
<name>` displays both the precondition and the last skip. It fails open too:
a probe that cannot read the blocked tasks mints and records why. No shipped
default carries it: the `blocked-task-triage` routine, which replaced the
retired auto-task of that name, applies the same rule itself
([automation.md](automation.md#blocked-task-triage)).

## Managing definitions

```bash
orbit auto-task list                    # definitions with schedule and state (--all: plugin-off too)
orbit auto-task show <name>
orbit auto-task update <name> --cron "0 9 * * 2"   # present fields only
orbit auto-task toggle <name> on|off    # the kill-switch — preserved, not deleted
orbit auto-task mint <name>             # mint one right now
orbit auto-task delete <name> --reason "<why>"   # remove it for good
orbit auto-task restore <name>          # reinstate a deleted shipped default
```

In a linked worktree, `show` and `list` inspect checkout-local candidate
definitions first and fall back by name to the registered workspace's catalog.
`show` reports the root and file that supplied the body; its settings and plugin
provenance come from that same catalog. A disabled or invalid local definition
does not expose a shared replacement. Inspection accepts `.yaml` and `.yml`
(case-insensitive extensions) and reports the actual file path. If multiple
files in one catalog have the same name, `show` refuses the ambiguous lookup.
Writes and `mint` still require the canonical `<name>.yaml` file and keep their
existing checkout scope; inspecting a shared definition does not authorize
changing or minting it from a worktree. An empty worktree catalog can be a Linux sandbox
write anchor and does not hide workspace definitions.
`orbit doctor` diagnoses both catalogs with their own settings and manifests.

On a shipped default, `toggle` and `update` of settings fields — `enabled`,
the schedule, dedupe, and the template's crew, priority, complexity, and added
tags — are stored in `.orbit/auto_tasks/.orbit-auto-task-settings.json` and
applied over the bundled body, which keeps receiving upstream fixes on
`orbit workspace sync`. Changing any other field forks the definition: Orbit
then preserves the file as written and stops refreshing it, and `show` reports
`body: forked` with the differing fields. `orbit workspace sync` moves an older
fork that differs only in settings back under management.

### Adopting a current bundled body from a stale fork

A genuine custom body is intentionally preserved, even if it still names a
retired tool. Sync recognizes historical shipped bodies by exact body digest;
it cannot decide which local prose to discard. Do not weaken that matcher,
delete/recreate the definition, or use `restore` to replace an existing fork.

Use the owning checkout and its registered workspace explicitly. This is an
operator change after review, separate from preparing or testing a repair:

1. Snapshot the effective definition through `orbit auto-task show <name>
   --workspace <owner> --json`. Agents can read effective definitions through
   the granted `orbit.auto_task.list` tool, selecting the named row; the CLI
   `show` also supplies layering and source diagnostics. Confirm `definition_source`
   points at the intended checkout. Save the complete response, including
   `layering.forked_fields`, not just the YAML or `layering.settings`: settings
   can still live in the fork itself. Record enabled, the complete schedule,
   dedupe, crew, priority, complexity, all tags and any other effective fields.
2. Extract the proposed fields from the **matching installed release's**
   bundled definition. Initialize a fresh disposable workspace with that
   executable and the target's base branch, then use `auto-task show <name>
   --json` there to obtain its uncustomized template. For candidate validation,
   obtain fields from that candidate's bundle. Take `template.description`,
   not the top-level description; parse YAML if using bundled source. Render
   any workspace branch placeholders for the target. Diff the effective template
   against that bundle and review every custom procedural difference. Choose
   explicitly what to replace and what to retain, including title, criteria,
   context files, required tools and comments. Keep a before snapshot, proposed
   patch, readable diff and decision record together as review artifacts.
3. Replace only the approved fields with a field-scoped update. For a
   description-only adoption:

   ```bash
   # bundled-body.txt contains the parsed template.description, reviewed above.
   orbit auto-task update <name> --workspace <owner> --body "$(cat bundled-body.txt)"
   # Or write a JSON file and use the registered tool:
   orbit tool run orbit.auto_task.update --input-file reviewed-body-patch.json
   ```

   The JSON shape is
   `{"name":"<name>","workspace":"<owner>","template_patch":{"description":"<reviewed bundled body>"}}`.
   Include another body field such as `acceptance_criteria` in `template_patch`
   only when its replacement was separately reviewed. Omit operational fields.
   Do not send a whole `template` through MCP: omitted fields receive defaults.
   Do not send tags as a patch to add one tag: that field replaces the list.
   Re-read immediately before applying; if the snapshot changed, review again.
   The body update has no compare-and-swap precondition, so coordinate with
   other editors and avoid concurrent edits during adoption.
4. Read back with `show --json` and compare every saved effective field. Only
   approved body fields and the edit stamps may differ. Stop on a settings
   mismatch; do not blindly replay an old full definition over a newer edit.
   Run `orbit workspace sync --workspace <owner> --check --json`, review the
   **whole** report (sync can affect other managed assets), then apply
   `orbit workspace sync --workspace <owner> --json` when its changes are agreed.
   Inspect the named auto-task's action and read back again. A settings-only
   fork migrates and reports `layering.body: managed`; enabled, schedule,
   dedupe and template settings must still equal the snapshot. Migration
   restores bundled creation metadata; save the original stamps in the review
   record rather than treating them as operational settings. Retained custom
   body fields leave it `forked`, intentionally; name those fields rather than
   claiming upstream refresh has resumed. User-authored definitions remain
   user-authored and do not acquire bundled management through this procedure.
5. Prove the patch first in a disposable workspace in an isolated child process
   with inherited Orbit/Git authority cleared (see the repository development
   guide's safe mutable CLI fixtures). Seed the observed fork and its complete
   settings, apply the exact payload, sync, compare before/after, and mint there
   with `orbit auto-task mint <name>`. Read the minted task back and verify its
   instructions and settings. Keep payload, snapshots, sync actions and mint
   evidence. Never mint on the live workspace merely to verify adoption:
   `mint` creates real work even when disabled. Existing minted tasks keep their
   original instructions and need a separate reviewed action if still active.

For a stale `friction-curation` fork, the adopted listing and group queries
must use `orbit.search` (`kind: friction`, `status: friction:open` for the open
set). Preserve the bundled 1000-hit truncation guidance and tag narrowing.
Review local resolution wording and criteria as well as the retired listing
paragraphs; replacing only the description may leave older criteria forked.
`orbit.friction.list` stays inactive and its refusal points agents to search.
After delivery, the operator still needs to approve the recorded custom-body
decisions and apply the reviewed patch on the owner, then verify settings and
sync as above. A disposable proof is not evidence of live adoption.

`mint` ignores the schedule, the dedupe policy, and `enabled`, and leaves the
scheduler's cursor untouched — so it creates real work even for a disabled definition. Inspect with
`show` first; mint only when creating that task is intended.

`list` and the dashboard read one state per definition. `skipped` means an
enabled delivery definition another machine owns, so this host never admits
work for it. Its reason is printed on stderr and carried as `skipped_reason`
by the MCP listing and the dashboard API, where its next evaluation reads
`unavailable`. `enabled` and `disabled` follow the effective value, so a
`delivery-code-review` that the deprecated `after-landing` policy enables
lists as enabled.

Over MCP: `orbit_auto_task_add`, `orbit_auto_task_list`, `orbit_auto_task_update`, and
`orbit_auto_task_mint`; `orbit_auto_task_update` with `enabled` is the
kill-switch. Adding `expected_enabled` (the observed state) makes it a checked,
atomic toggle that is refused when the definition no longer matches; that form
needs operator authority and an explicit `workspace` and accepts no other
edits. `toggle`, `delete`, `show`, `restore`, `recover`, and `reset` are not
on MCP; use `orbit auto-task` or `orbit tool run orbit.auto_task.<verb>`.
`delete` and `reset` need operator authority on every surface: a
non-interactive caller sets `ORBIT_OPERATOR=1`, which is audited.

### Deleting a definition

`toggle off` pauses a definition and keeps it listed. `delete` removes it: the
YAML file, its scheduler cursor, and — for a `deliveries_landed` definition —
its consumer state. Every delete writes an audit record naming who deleted
what and the optional `--reason`. A delete that fails leaves the definition
and its cursor in place; if it failed releasing a pinned ref, the consumer
reset before it stays applied, recorded as its own audited reset.

- It refuses while a task minted from the definition is still open and names
  those tasks. Finish or close them, or pass `--force`; a forced delete leaves
  the open tasks as they are.
- A delivery consumer is torn down through the audited `reset` (see below),
  and every pinned `refs/orbit/automation/*` ref it held is deleted. Delete
  therefore refuses whenever that reset would — an executing action, or a
  consumer owned by another machine — and points at
  `orbit auto-task reset <name>` to preview it. `--force` abandons an
  executing action, exactly as it does for `reset`.
- Deleting one of the shipped defaults below records an opt-out in
  `.orbit/auto_tasks/.orbit-managed-assets.json`. `orbit workspace init
  --force` and `orbit workspace sync` leave an opted-out default absent, and
  `orbit doctor` does not report it missing. Use this to drop the code
  sweeps from a workspace whose repository holds no code.
- `orbit auto-task restore <name>` is the way back for a deleted shipped
  default. It writes the shipped content (disabled, as shipped) and clears the
  opt-out, so later reseeds manage it again. `restore` refuses a name Orbit
  does not ship and a definition that already exists. `auto-task add` under a
  deleted default's name creates your own definition instead; the opt-out
  stays, so reseeds never overwrite it.

A user-authored definition records no opt-out: delete simply removes it.
`delete` and `restore` are not on MCP; over MCP, disable a definition instead.

Required tools in a template extend an allowlist activity's baseline. In a
deny-list activity they cannot override `tool_disallow_list`. The implementing
activity (`agent_implement`) refuses to start when the list covers one.
`final_recovery`, `step_failure_recovery`, `pr_conflict_recovery`,
`agent_review_repair`, `review_reconciliation_review`, `task_pilot` and
`agent_invoke` drop that requirement from the requested tools and record a
note; the tool stays uncallable. Any other activity keeps the refusal.
They do not replace the list or bypass runtime capability, policy, filesystem,
subprocess, or authentication checks. Invalid, inactive, wildcard, or
non-agent-facing names fail dispatch before the provider starts.

A template that declares exactly `github.auth.status`, `github.run.list`,
`github.run.view`, `github.run.logs`, and `github.pr.list` is the worked
example. A minted instance therefore runs under
`effective_tools = activity baseline ∪ those five names` for allowlist mode.
The shipped deny-list `agent_implement` already includes those GitHub reads;
tasks requesting nothing keep that baseline. Inclusion is only tool membership — a
structured `github.auth.status` answer may still report `available: false` or
`authenticated: false` when the lane has no GitHub CLI or no credentials.
That is unavailable evidence, not a clean CI result.

## Definitions a plugin seeds

An installed plugin may ship auto-task definitions. `orbit plugin enable <ns>`
seeds each one as `.orbit/auto_tasks/<ns>-<name>.yaml` with `enabled: false`
and a `# provenance: plugin:<ns>@<version>` header; a plugin may not ship a
definition that is already enabled. Review it, then `orbit auto-task toggle
<ns>-<name> on` like any other.

An upgrade re-seeds a seeded file only while it still matches what the plugin
wrote; a file you edited is preserved with a warning until
`orbit plugin enable <ns> --force`. While the plugin is disabled, removed, or
switched off in this workspace, the definition stays on disk and is skipped,
and listings hide it: `orbit auto-task list --all` shows it as `inactive` with
a reason naming the plugin, and `orbit auto-task show <name>` still resolves
it. Tasks minted from one carry
`plugin:<ns>` beside `auto-task:<name>`, so their provenance survives the
plugin being removed.

## The twenty-one seeded definitions

`orbit workspace init` seeds all twenty-one, disabled, except any you deleted (see
[Deleting a definition](#deleting-a-definition)):

- **`qa-sweep`** (`medium`) — hourly. Identifies recent changes, exercises them hands-on
  through their real user-facing paths rather than just re-running the test
  suite, and files a task for each non-duplicate issue found. In agent-executor
  sandboxes and linked job-run worktrees, the managed `.git` mount is read-only
  by design (must not be worked around by chmod or host-side gitdir writes).
  Use `mkdir -p .orbit/tmp && mktemp -d .orbit/tmp/qa-base.XXXXXX` for a new, empty
  directory, then `git archive <sha> | tar -x -C <that path>` to build a baseline
  revision without writing `.git`, and `git show HEAD:<path> > <path>`
  to revert a tracked file when `git checkout --` cannot take `index.lock`.
- **`friction-curation`** (`medium`) — daily. Deduplicates the open friction corpus against
  task history, verifies each survivor still reproduces, resolves the ones that
  don't, and files fix tasks for the ones that do. A survivor owned by another
  workspace is re-homed there, or marked `rehome_to` when that workspace is not
  reachable, instead of blocking the run. → [friction.md](../../orbit/references/friction.md)
- **`security-review`** (`hard`) — weekly. Reviews applicable application code,
  dependencies, secret handling, and configuration with evidence; files a
  durable Orbit task for each non-duplicate finding with severity and impact; a
  clean review is a successful no-op.
- **`code-review`** (`hard`) — every six hours. Reviews the two least recently
  reviewed bounded, disjoint modules read in full at one pinned integration
  commit. Each confirmed high or critical finding becomes its own task; a
  module's medium and low findings are grouped into its single cleanup task
  tagged `code-review`. Per-delivery diff review stays separate under
  `delivery-code-review`. Rotation history lives in the `reviewed-modules.json`
  artifact; the `sweep-cursor.json` artifact is the scheduler cursor.
- **`full-code-review`** (`medium`) — on demand (`orbit auto-task mint
  full-code-review`); its monthly cron stays off until you enable it. The minted
  task is a coordinator: it pins the integration branch's tip, splits the tree
  into review areas of at most 25,000 tracked text lines, preferring package
  and module boundaries where they fit that bound, and files one area-review
  chore per area, tagged `full-code-review` and `no-diff-expected` (never
  `code-review`), at `hard` complexity or below with no pinned crew. Each area
  reviewer reads its whole area at that commit in partitions of at most 10,000
  lines and files confirmed findings as bugs tagged `code-review` and
  `full-code-review`;
  a clean area is a successful no-op.
- **`qa-full-sweep`** (`medium`) — on demand (`orbit auto-task mint
  qa-full-sweep`); its monthly cron stays off until you enable it. Where
  `qa-sweep` validates only the changes since its cursor and
  `full-code-review` reads the whole tree without running it, this sweep runs
  the whole product. The minted task is a coordinator: it pins the integration
  branch's tip, inventories the user-facing surface from the repository's own
  docs and entry points (or from its own full-QA runbook or harness when it
  ships one), attaches `qa-full-sweep-inventory.json` with the areas and
  every exclusion, and files one surface-QA chore per area of at most 20
  required scenarios, tagged `qa-full-sweep` and `no-diff-expected`, at `hard`
  complexity or below with no pinned crew. Each area tester builds that commit
  and exercises the area through its real boundaries in disposable roots,
  recording every scenario as `PASS`, `FAIL`, `BLOCKED` or `NOT_RUN` in a
  resumable `qa-progress.json`; a check that needs a missing capability is
  never `PASS`. Non-duplicate failures become bugs tagged `qa-full-sweep`. The
  coordinator also files a roll-up chore that depends on every area and
  records the release decision in `qa-full-sweep-decision.json`: `PASS` only
  when every area is done and every required scenario passed at the pinned
  SHA, otherwise `INCOMPLETE`, including when an area's progress is missing,
  unreadable, or names another SHA or area. The roll-up's description and
  acceptance criteria carry an owner-evidence contract. Area status and
  `qa-progress.json` reads name other tasks, which a claimed worker cannot
  do, so preparation records `required_machine` and only the owning machine
  runs the roll-up. Area tasks stay portable, with no crew and no `os:` tag.
  The coordinator sets those dependencies with `orbit.task.update` on the
  owner and finishes only when the roll-up's `dependencies` field lists
  every area. If that write is unavailable, it returns a
  `qa_rollup_dependencies_unwired` blocker for the owner to apply; a comment
  that names the IDs is not the wiring. The roll-up carries
  `auto-task:qa-full-sweep`, so no new sweep is minted by
  the schedule until it is done, and it records the swept commit as the
  `skip_if_unchanged` cursor. A workspace that already has its own
  `qa-full-sweep.yaml` keeps it as its active definition: sync preserves the
  file byte for byte and warns that it occupies the bundled default's name.
- **`delivery-code-review`** (`hard`) — reviews each frozen delivery batch and
  records typed coverage evidence: for each delivery, the changed paths it
  examined (or skipped, with a reason) matching the frozen diff, a verdict and
  a rationale. A review whose agent persists no execution summary leaves its
  batch owed. Hands-on QA of recent changes is
  `qa-sweep`; hands-on QA of the whole product at one commit is
  `qa-full-sweep`.
- **`doc-duties`** (`low`) — daily. Validates a small batch of the
  least-recently-attempted workspace documentation against current behavior,
  and corrects factual drift and broken links. Each run attaches a
  `doc-duties-ledger.json` artifact recording an outcome (`clean`, `fixed`,
  `partial` or `skipped`) and attempt date per selected document, plus a
  cumulative `state` map of every path's two latest attempts. A run discovers
  each candidate's artifacts through the metadata-only `orbit.task.show`
  (`field: "artifacts"`) projection and fetches a ledger only when listed, so
  absent ledgers are skipped without a failed artifact read. It reads only
  the newest listed ledger's `state` (older ledgers without it are read
  through the 60-run window), so attempts survive task-history truncation.
  Selection orders by the latest attempt, then `last_validated` frontmatter,
  then the git last-touched date, so a skipped document rotates to the back. A
  document whose two latest attempts were `skipped` or `partial` is held back
  and reported as needing its own task. A batch whose claims
  are already accurate is a successful no-diff run with validated no-diff
  evidence. Its template declares `dir:.`
  because a batch may include root and workspace-specific documents, and omits
  `no-diff-expected` so it holds a context lock while correcting drift.
- **`comment-hygiene`** (`low`) — daily. The code-comment counterpart of
  `doc-duties`: checks at most six tracked source files and about 1,500 lines
  against a generic comment standard stated in its body (comments say why,
  doc comments state the contract, no history narration or commented-out
  code, no ownerless TODOs, escape hatches state their reason, stale comments
  are bugs, density stays low), which a workspace can sharpen through a
  `comment-hygiene` tag instruction. Each run attaches a
  `comment-hygiene-ledger.json` artifact with an outcome (`clean`, `fixed`,
  `partial` or `skipped`), attempt date and `checked_through` commit per
  selected file, plus the same cumulative `state` carry-forward as
  `doc-duties`; a file unchanged since its last `clean`/`fixed` check is not
  re-read, files in an open task's footprint are skipped, and two
  `skipped`/`partial` outcomes in a row escalate to a dedicated task. Its
  diff is comment-only — everything outside comments and doc comments stays
  byte-identical — and a stale comment that reveals a real defect is filed
  as a task (at most two per run) instead of being fixed in place. Unlike
  `doc-duties` it declares no `context_files` and mints as `proposed`, so a
  minted instance is no backlog whole-tree holder; pre-admission
  preparation (the task pilot, or an operator by hand) selects the batch
  and persists exactly its `file:` selectors before the task is approved
  to `backlog`, and the run reads them back and edits only those files. A clean batch is a
  successful no-diff run with validated no-diff evidence, and the ledger is
  attached on every run.
- **`backlog-hygiene`** (`medium`) — weekly. Writes one read-only report of
  stalled and untriaged tasks. It does not change task status or dispatch work.
- **`run-failure-patterns`** (`medium`) — weekly. Mines this workspace's run
  evidence, and on an owner the failure and release settlements its followers'
  claimed leaves sent (`orbit run settlements --no-reconcile`), for failures
  that nobody has filed. It folds parent pipelines into their child by the
  recorded `root_cause` (a parent of a `held` child is no failure), splits
  review refusals by escalation category, and also files a single blocking
  failure of a deterministic Orbit action or tool limit. Each untracked
  pattern, its mechanism verified in code, becomes one `proposed` fix task
  tagged `run-failure-patterns` and `no-auto-approve`, so it waits for human
  approval; a friction replaces the task only when no cause can be
  established, never both.
- **`architecture-audit`** (`hard`) — every 14 days. Audits the package and
  module dependency graph, built from the ecosystem's own metadata tool or
  an import scan (the method used is named in the run summary): cycles,
  including up-calls hidden in behavior attached to a shared type from
  another module; distributed types; public-surface width; concept
  placement; and sizes with rebuild reach, all recorded in an
  `architecture.json` artifact carried between passes, plus one scored area
  read in full. Layering is judged against the repository's documented
  architecture when one exists; without one, the run reports the layers it
  inferred and files only true cycles and up-calls, never layering-drift
  tasks. A split, merge, or move candidate whose case rests on unmeasured
  numbers is filed first as a `no-diff-expected` measurement task with a
  go/no-go, and only then as a restructuring task depending on it. Each run
  files at most 3 `proposed` improvement tasks plus measurement
  prerequisites, each with `context_files` and a `spawned_from` relation to
  the minted chore. The chore itself is tagged `no-diff-expected` and never
  builds or runs tests.
- **`escape-to-guard`** (`medium`) — weekly. Classifies `unclassified` and
  absent regression escapes from the regression task and the culprit's
  landed diff, records each classification through the supported
  `orbit.task.update` relations write (or in its artifact plus one
  deduplicated gap task when that write cannot change an existing link),
  clusters regressions by nature × escape × layer plus a one-line
  mechanism, and files at most 3 `proposed` guard tasks for clusters of
  3 or more regressions in 30 days. A guard is a mechanical check inside
  the gates that already exist — a lint, a type, a static guard script, a
  generator check, a skill or review-instruction line for `review_missed`,
  or a regression test for `no_test` — never a new required status check
  or merge gate. Each run attaches `escape-to-guard.json` with the
  clusters, classification counts before and after, and recurrence after
  earlier guards landed, diffed against the previous run; a workspace
  with no regression links is a successful no-op.
- **`code-health-sweep`** (`hard`) — every 3 days. Discovers the repository's
  languages, source roots, gate commands and public surface from tracked
  files, then scans for dead code, inventories compatibility paths, and reads
  one churn-selected area in full for duplication, simplification and
  architectural drift. A dead-code candidate is kept only after the macro,
  string-registry, serialization, FFI and reflection routes are checked, and a
  public command-line, API, configuration or persisted-format surface is never
  filed for deletion by that pass. Compatibility retirement tasks are filed
  only when the repository documents a support window; otherwise the
  inventory is recorded in the run summary and nothing is filed for
  retirement. It does not count lint suppressions; the `suppression-ratchet`
  default owns that. Each run records a `code-health.json` artifact carried
  between passes and files at most 6 `proposed` tasks, each with
  `context_files` and a `spawned_from` relation to the minted chore. The
  chore itself is tagged `no-diff-expected` and never builds or runs tests.
- **`dependency-freshness`** (`medium`) — monthly. Inventories direct
  dependencies in every detected ecosystem (for example
  `Cargo.toml`/`Cargo.lock`, `package.json` with a lockfile,
  `pyproject.toml`/`uv.lock`, `go.mod`) with each ecosystem's own outdated,
  unused, and duplicate tooling, recording unavailable tooling as a gap.
  Dependencies covered by a security advisory or an open advisory task are
  excluded; that work belongs to the security sweeps. Each run attaches
  `dependency-freshness.json` diffed against the previous run and files at
  most 3 `proposed` upgrade or removal tasks ranked by how far behind
  multiplied by import or call-site usage, each quoting the breaking
  changes that apply from upstream release notes. It never bumps the
  repository's own version, tags, or publishes, and with no network or
  registry access it finishes as a successful no-op stating why.
- **`path-risk-review`** (`medium`) — every 3 days. Reads the codebase
  analytics for paths that keep regressing, land outside their declared
  footprint, block, get rejected, or contend for locks, and follows the tasks
  behind the riskiest. It files at most three `proposed` fixes per run, each
  tagged `path-risk-review` and `no-auto-approve`, and each changes a
  mechanism (a footprint or lock rule, a generator check, or a split of a
  coupling hotspot) rather than patching one regression. A run where every
  ranking sits below `min_support` is a successful no-op that reports each
  ranking's below-support counts and files nothing.
- **`structure-sweep`** (`medium`) — weekly. Scans source files and test
  files in one pass at the thresholds stated in its body and recorded in
  every run summary (a non-test source file over 1,000 lines, a module
  index file over 400 lines of mostly logic, and a test file over 2,000
  lines). It discovers languages and source and test roots from tracked
  files and keys layout candidates to the detected language's convention,
  recording rejected leads with a reason. Each run files at most 4
  `proposed` restructuring or test-reduction tasks, largest first and
  grouped by package or subsystem, each with `context_files` and a
  `spawned_from` relation to the minted chore, which is itself tagged
  `no-diff-expected` and never builds or runs tests. A test-reduction
  task keeps that file's coverage — every scenario survives or is named
  with a reason — and reports the test count before and after.
- **`auto-task-yield-review`** (`medium`) — monthly. Reports whether each
  enabled auto-task definition in this workspace pays for itself over the
  last 30 days: minted chores by final status and no-op successes, cost
  per run from run evidence (unknown, never zero, when missing), tasks
  filed through `spawned_from` and their outcomes, filed tasks that later
  regressed, duplicate or invalid rejections, landed tasks per run and
  cost per landed task. Each run attaches `auto-task-yield.json` diffed
  against the previous run and files at most 3 `proposed` retuning tasks
  (cadence, merge, template fix or disable), each naming its definition
  and numbers. It never edits, toggles or mints definitions.

- **`suppression-ratchet`** (`hard`) — weekly. Counts lint suppressions,
  TODO/FIXME/HACK/XXX, ignored tests and language escape hatches per category
  and package/area at a pinned integration-branch SHA. Language-specific
  patterns exclude vendored, generated and build output; justified occurrences
  are counted separately and never filed. `suppression-ratchet.json` stores
  comparable counts and filing evidence: the first run is a baseline with no
  filings; later runs file bugs for increases with introducing commits and at
  most one small reduction in the highest-churn eligible area. At most three
  proposed tasks, grouping increasing categories when necessary, each with
  verified context and `spawned_from`. The minted chore is `no-diff-expected`
  and never builds. This default owns the suppression counts delegated by
  the portable `code-health-sweep`.
- **`flaky-test-hunt`** (`hard`) — weekly. Hunts flaky tests in hosted CI
  history over a 14-day window on the integration branch and recent
  pull-request runs, through 2 signals: a failed run followed by a
  succeeded rerun of the same SHA, and a fail-then-pass pair on
  adjacent integration-branch commits when nothing relevant changed.
  Test names and bounded failure excerpts come from the failed runs'
  logs; unparseable logs are recorded as gaps. Each run attaches
  `flaky-test-hunt.json` (flake count per test, occurrences with run
  ids and SHAs, gaps) diffed against the previous run, and files at
  most 4 `proposed` tasks tagged `ci-flake`, highest flake count
  first, deduplicated against open `ci-flake` and CI-failure-sweep
  tasks. Filed tasks require the root cause fixed or the test deleted
  as redundant, never silent retries or quarantine without an exit
  plan. With no hosted CI or unreachable history it is a successful
  no-op stating the reason. The chore itself is tagged
  `no-diff-expected` and never builds or runs tests.
- **`ci-budget-watch`** (`medium`) — daily. Measures the main hosted test
  job's elapsed time (discovered from the CI workflow files) against the
  repository's documented CI budget (figure, statistic, and population
  from the agent guides, contributing guide, and docs). Attaches
  `ci-budget-watch/report.md` with the last 20 completed runs, the trend
  against the previous report, and the measurement gaps. With no
  documented budget it records the trend and files nothing; with no
  hosted CI or unreachable history it is a successful no-op stating the
  reason. When over budget it keeps exactly one open `ci-budget` task,
  updating an existing one instead of filing a second. The chore itself
  is tagged `no-diff-expected` and never edits source, CI, or tests.

Read them before enabling. They are also the best worked examples of how much
instruction a minted task's body should carry.

Finding tasks tagged `qa-sweep`, `security-review`, `code-review`, or
`friction-curation` receive the matching bracketed title prefix at the shared
task-creation boundary. If more than one of those tags is present, the fixed
precedence is `qa-sweep`, `security-review`, `code-review`, then
`friction-curation`, regardless of tag order. Existing matching prefixes are
not duplicated. Scheduler-minted parent tasks continue to use `[auto-task] `,
which takes precedence when an `auto-task:<name>` provenance tag is present.

## Writing the body

The body is the entire brief the executing agent gets. It arrives with no
conversation, no clarifying questions, and no memory of why the definition
exists. So:

- State what durable output counts as success. If the real deliverable is filed
  tasks or resolved records, say so — narrative output is advisory.
- Name the exact commands and tool surfaces to use, especially where a plausible
  wrong path exists (editing a file instead of calling a tool).
- Say what a clean no-op looks like. A recurring chore that finds nothing should
  succeed, not fail or invent work to justify the run.
- Require a dedupe check against existing tasks before filing anything.

## Delivery consumers: stalls, automatic replay, and reset

A definition scheduled on `deliveries_landed` (the shipped
`delivery-code-review` definition) is not on a clock: it watches a branch, records
which landings it has examined, and mints an examination task when the debt is
due. That recorded position — "observed commit X" — is what a rewritten branch
history breaks.

When the checkout has an `origin` remote, the consumer observes
`origin/<branch>`, not the local branch. Each pass fetches that one ref and
leaves the worktree, index, and local branch untouched, so a pull request
merged on the remote is seen even when the checkout was not fast-forwarded. A
failed fetch defers as `source_fetch_failed` and does not fall back to the
local ref; that deferral retries. `orbit doctor`'s `review` row names the
observed commit and the remote-tracking head, and it is not ok while the cursor
trails that head past the batch's `max_wait_minutes`. A repository with no
remote still watches `refs/heads/<branch>`. Doctor compares the remote-tracking
ref already in the checkout and does not itself fetch.

### A rewritten history is proved, or it stalls

When the observed commit is no longer reachable from the configured branch head,
the evaluator runs a replay proof: every observed commit must map onto a commit
on the new head with the same `.orbit` tree and the same parent-relative patch.

- **Proof succeeds** (a rebase or amend that preserved content) — the consumer
  reconciles itself in that tick, under an audit record attributed to
  `system:automation`, keeping every obligation. One friction is filed so the
  rewrite is visible, and the next tick observes normally. Nothing to do.
- **Proof fails** (a commit was dropped, squashed away, or its debt can no
  longer be identified) — the consumer **stalls**: evaluation suspends, one
  friction is filed listing the obligations that could not be mapped, and the
  tick prints a single `stalled: history_diverged` line instead of an error
  every 60 seconds. Nothing resumes it without an operator.

Frictions are deduped per rewrite, so repeated ticks and the sibling consumer on
the same branch share one record rather than filing hundreds.

Find stalled consumers with `orbit doctor` (the `automation-consumers` row) or
`orbit auto-task recover <name>`, whose stall reason names the cause. A stall
that persists past `automation.stall_window_minutes` (default 60) is also logged
at `warn`, so `orbit log tail --level warn` shows it.

### Clearing a stall

Two commands clear it, and both write an audit record:

```bash
# Retain every obligation: reconcile a rewrite the proof can verify.
orbit auto-task recover delivery-code-review --replay-history          # preview
orbit auto-task recover delivery-code-review --replay-history --reason "<why>"

# Forget the debt: re-baseline at the current branch head.
orbit auto-task reset delivery-code-review                            # preview
orbit auto-task reset delivery-code-review --reason "<why>"
```

Prefer `recover`. Use `reset` when no recovery can repair the consumer — the
rewrite destroyed the debt's identity, or the state predates the recorded
trigger and `recover` refuses it as `coverage_unverifiable`.

### What `reset` does

`reset` is the only Orbit operation that discards coverage debt, so it previews
until it carries `--reason`. The preview prints the consumer key, its
generation, everything that would be forgotten (pending deliveries and commits,
unresolved evidence, waived and excluded landings, accepted receipts), any
executing action, and the head it would re-baseline at. Read it before
authorizing.

An apply writes one `reset` recovery record (who, when, why, the previous
epoch/generation, the forgotten inventory, the new baseline), drops the consumer
state, and deletes the `refs/orbit/automation/<digest>/*` pins of the forgotten
batches. The next tick reports `baselined` at the branch head. Landings before
that baseline are never examined — that is the cost of the command, and the
record is the only trace they existed.

It refuses while an action is executing unless `--force` is passed (the admitted
task is abandoned, not cancelled), and refuses a state-member consumer. Never
hand-edit the `automation_consumers` row instead: that leaves no audit record.

Automation frictions are tagged `automation`, plus `history-diverged` for a
rewritten history. Those tags ship in the default vocabulary; loading the
taxonomy merges any missing defaults into an existing `.orbit/frictions/tags.yaml`
without removing operator-added tags.

## Scheduling notes

The host tick runs minutely by default, so each definition's own cron governs its cadence.
Catch-up collapses: a machine that was asleep through six firings mints once, not
six times. Per-definition cursors and their sidecar lock make evaluation
idempotent and single-flight within the workspace.

Fires are reported by `orbit clock tick` and in the dashboard auto-task panel;
the minted task is the durable record. Evaluation creates no scheduler job run.
