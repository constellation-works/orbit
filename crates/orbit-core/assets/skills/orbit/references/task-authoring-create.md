# Authoring a task: prepare and create

Contents:

- [Prepare and create](#prepare-and-create)
  - [Bound the footprint and evidence before filing](#bound-the-footprint-and-evidence-before-filing)
  - [Work only a human can do](#work-only-a-human-can-do)
- [Host OS routing](#host-os-routing)

## Prepare and create

1. Select the authoritative workspace. For findings discovered by review, QA,
   friction or triage, search open and closed work before filing a new owner.
   For an explicit user request, reuse a supplied task; search when duplication is
   plausible rather than turning every request into an investigation.
   See [search](search.md) for lexical query guidance.
2. Inspect the likely modification anchors and relevant interfaces. Background
   reading belongs in the description, not the lock footprint. Use targeted
   reads; a task does not need a dump of the surrounding subsystem.
3. Write the contract and criteria. Confirm the assigned lane can obtain the
   evidence: deterministic fixtures for service/time/filesystem behavior,
   required repository gates, and explicit platform or operator handoffs where
   necessary. See [task fields](task-fields-projections.md) for `required_tools` and capabilities.
4. Create through `orbit.task.add`. Attribution is the session, not a `model`
   argument. Required inputs are
   title, description, workspace and assessed complexity (`low`, `medium`, `hard`,
   `xhard`). An orchestrator also sets `origin`: `operator_request` when a human
   asked for the work (in chat or elsewhere outside Orbit), or
   `orchestrator_initiative` when you decided on your own that it is needed.
   - An orchestrator passes `orchestrator` (its own crew) on every create, so the
     task records that crew's configured model as `orchestrator_model`. Pass
     `orchestrator_model` only when your session runs a different model than that
     crew is configured with; it is recorded once and never updated.
     Use the advertised schema; detailed `plan` belongs to pickup, not task creation.
     Creation does not approve promotion, dispatch or completion.
5. Read the returned ID using `fields: ["id"]` or a JSON parser. Never truncate a
   write response with `head`/`cut`. If the result is uncertain, list/search before
   retrying: a lost reply does not mean the task was not created.
   - `fields: ["id"]`
     returns the bare ID and cannot carry the optional `warnings` metadata (see
     Warnings in [`task-authoring-new-files.md`](task-authoring-new-files.md)); omit `fields` when warnings matter. Never put `warnings`
     in `fields`: it is not a task field, and the add is refused before it is stored.
6. Wire `dependencies` (and any `child_of` relation) with `orbit.task.update`
   on the returned ID; creation refuses `dependencies`.

```bash
orbit tool run orbit.task.add --input '{
  "title": "<concrete outcome>",
  "description": "<problem, scope, prerequisites and constraints>",
  "acceptance_criteria": ["<behavior and evidence>"],
  "context_files": ["file:<verified-modification-target>"],
  "workspace": "<discovered-selector>",
  "complexity": "medium", "type": "feature",
  "origin": "operator_request", "orchestrator": "<your-crew>",
  "fields": ["id"]
}'
```

### Bound the footprint and evidence before filing

File only work the pilot can assess. Before creating the task, establish both
halves of its input:

- **Bounded selectors.** Every `context_files` entry is a verified creation,
  modification or deletion target at the scoped revision: an existing file,
  directory or symbol confirmed by inspection, or a known new target filed
  with `allow_missing_context` and a stated creation intent. See [Modification
  footprint](task-authoring-footprint.md#modification-footprint) for granularity, companions and wiring.
  - Never file a sweep
    of a directory or crate, read-only background, tests or fixtures only
    consulted, or a shared reference doc as the footprint; background belongs
    in the description. A task whose preparation must select the exact batch
    paths cannot be filed until that batch is reconciled from readable
    sources — an oldest-first selection with uncovered history, an unreadable
    predecessor ledger, an unresolvable release reference — has no verified
    footprint yet.
  - File the smallest bounded investigation that establishes it
    instead of the full repair, or leave the repair unfiled until the sources
    are readable.
  - This does not apply to a worker-selected batch inside an
    already declared bounded footprint: when the brief assigns batch selection
    to the executing worker and the task already carries the bounded template
    footprint that brief names as the lock, that declared bound is the verified
    footprint, and an unread predecessor ledger alone is neither an
    unestablished footprint nor a reason to file an investigation instead; the
    worker reconciles inside the kept bound at run time.
- **Readable evidence references.** Every run, log, commit, ledger, tag or
  predecessor artifact the description or criteria names must be readable
  through the task record or the lane's granted tools. Quote the failing
  command and its tested commit rather than a truncated excerpt: a truncated
  log with no commit identifies no repair. Record each ledger's dates and
  sources rather than citing it alone: an unreconciled ledger verifies no
  preparation-selected batch.
  - Resolve release tags and predecessor artifacts
    before filing. An unreadable footprint-defining reference is a filing
    block. Make it readable or re-scope the task. Never file the repair
    expecting the pilot to resolve it. Exception: for a worker-selected batch
    inside an already declared bounded footprint, an unread predecessor ledger
    alone does not block filing; disclose the gap and the brief's documented
    fallbacks in the description instead.
- **Check the pilot's answer.** After filing, read the task's readiness and
  the pilot assessment. A `selectors`, `verified_no_diff` or
  `host_operational` disposition with its lock footprint is the filing proof.
  An `unassessed` recommendation, or a holding unestablished-footprint
  finding, means the filing failed this section. Attach the missing
  selectors or evidence and request a new pilot assessment.

Write the title as a concrete outcome that fits the commit subject. The pipeline
composes the subject as `<type>: <title> [<ID>]` and rewrites a literal `…` to
`...` and commas inside brackets to semicolons. It cuts the title at a word
boundary, never with an ellipsis, to keep the whole subject within 72 characters.
When the subject cuts or rewrites the title, the full title leads the commit body.

Keep titles to at most 55
characters for a bug, 54 for a feature, 53 for a chore and 50 for a refactor,
which is 72 minus the type's prefix and the ` [<ID>]` marker (12 characters for
an `ORB-` id with five digits). A longer id, or the ` (#<PR>)` a squash appends,
takes more room, so a title at the limit may still be cut at a word boundary.
Put the detail in the description, not the title.

Assign `crew` to the actual intended implementer. If you will implement the task
personally, use your own configured crew rather than the crew you would have
chosen for delegation. On an authorized takeover, correct a stale assignment
before continuing; do not leave another crew named for work it is not doing.
Session identity is tool-call provenance (agent family), while `orchestrator`
identifies who prepared/supervised the work; neither substitutes for execution
`crew`.

A managed run's `ORBIT_AGENT_MODEL`, or CLI `--model`, may be a full
model id (`claude-opus-5-5`): `created_by` keeps the family and the full id is
recorded once as `created_by_model`. MCP schemas do not take `model`.
`orchestrator_model` comes from the `orchestrator` crew's configuration unless
you override it; a caller-supplied `model` never sets it.

### Work only a human can do

Credentials, approvals, physical access and operator-only decisions go to the
human, not an agent. Set `crew` to the human-kind crew. Find it with
`orbit config show`, which lists a human crew with provider `human`, or
`orbit config get crews.<name>.kind` (answers `human`). Choose the sole human
crew, or the one matching the affected operator; if several are ambiguous, use
the fallback. A human crew never bypasses an agent gate: the task is never
dispatched, and an agent still cannot close it.

With no matching human crew, do not assign an agent. File a setup chore tagged
`setup`, `needs-operator` and `no-auto-approve` that asks the operator to add
a human crew (`orbit init --human-name <name>`), and leave it `proposed`.

Keep the body short; nobody reads a wall of text. In plain words, it has:

1. a 1-2 line what and why;
2. numbered steps;
3. every command in its own fenced `sh` block, ready to copy and run;
4. a closing `Verify:` line with one fenced command.

Put long evidence in a task artifact, not the body. Filing a human-crew task
returns a warning for missing numbered steps, no fenced block, no `Verify`
block, or a body over 2000 characters; fix the body with `orbit.task.update`.
Example:

````text
Rotate the expired GitHub token on the CI host; CI sweeps fail without it.

1. Create a token: https://github.com/settings/tokens (repo, workflow)
2. Install it:

```sh
gh auth login --with-token < token.txt
```

Verify:

```sh
gh auth status
```
````

Implicit complexity, tag and context-file edits preserve the crew and crew source
of admitted work, including work awaiting setup, in progress or under review.
Stopped deliveries and preserved candidates keep that attribution until a fresh
admission or explicit audited correction replaces it. Those edits still update
the task's scope under the existing authority rules.

They do not change a recorded run's crew or model. Before admission, automatic
assignments can rerate; explicit pins stay pinned. After an authorized recovery
or requeue, a fresh admission evaluates the current scope and records the crew
it actually selects. Resuming a candidate retains its admission provenance.
An explicit authorized crew correction remains an audited assignment.

Task IDs come from the store. Use task tools to change records, never edit their
filesystem projections. Reuse existing tags. Use `feature`, `bug`, `refactor` or
`chore` for type. An update may reassess complexity but cannot reset it to
`unassessed`. See [task fields](task-fields-projections.md) for dependency/relationship semantics,
duplicate handling and additional tool grants.

## Host OS routing

Tag a task `os:linux`, `os:macos` or `os:windows` when only a host running that
OS can do it: a `sandbox-exec` failure needs macOS, a systemd change needs Linux.
No `os:` tag means any host; several mean any one of them. The namespace is
reserved: `task.add` and `task.update` reject any other `os:*` value (`os:mac`,
`os:osx`), so check the spelling the error names. Matching ignores case.

Admission reads the tags every time it considers a task. A local drain, ship
discovery and `orbit run ship` start the task only on a host of a named OS, and
an owner hands a pull-drain claim only to a follower of one. Elsewhere the
task stays in `backlog` with the wait named (`waits for a macos host
(os:macos)`) in readiness, `orbit run show` and the dashboard Drain card.

When task-pilot finds a criterion that needs native evidence from one OS and
the task has no `os:` tag, it adds that tag itself; it never replaces an `os:`
tag you set. If your tag names another OS, a host that cannot produce the
evidence leaves the task waiting as `native_os_required`. When only the
owner's own machine can produce the evidence (timings against its live store,
say), the pilot records a machine requirement and the owner refuses pull
claims from every other machine.

For a criterion that needs a test to run under nested Seatbelt or Bubblewrap,
name the required validation gate that runs that test. The worker sandbox may
prevent the nested sandbox from starting; the sanctioned deferred-sandbox
replay then runs that gate on the claim host, outside the worker sandbox.

A
passing host replay is the native-sandbox evidence and satisfies the criterion,
including on a verified clean-base NoDiff handoff (a `no-diff-expected` skip
runs no gate, so it replays nothing). The in-worker skip is not evidence by
itself; the passing replay is. Do not write acceptance criteria that require
"no sandbox deferral" or say a skip cannot count as evidence for these tests.
The replay covers only a required gate named by the deferred record, not a
standalone test outside that gate.

To route a task an auto-task or a sweep already minted, retag it with
`orbit.task.update`, sending the full tag list (it replaces the stored one):

```bash
orbit tool run orbit.task.update --input '{"id": "<task-id>",
  "tags": ["<existing tags>", "os:macos"], "workspace": "<selector>"}'
```

The next admission honours it. A task already running or claimed is not moved.
