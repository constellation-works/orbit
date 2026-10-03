# Orbit desktop control-center validation

The candidate adds Tasks, Runs and Review views to the existing Orbit plugin.
It serves `ui://orbit/control-center/v1/index.html` as
`text/html;profile=mcp-app`; the old `ui://orbit/task-panel/v1/index.html` remains
readable so an already-open panel can reload. `orbit_ui_open` advertises a
global entrypoint and `orbit_ui_inspect` a thread entrypoint. Opening either
only reads. Existing data tools have no automatic widget metadata.

**Actual desktop validation is NOT RUN and is deferred.**
Daniel archived ORB-13715; its evidence and handoff remain available without
reactivating that task.
The user authorized implementing the product while that host check is pending.
A passing protocol or DOM test does not prove the installed desktop build can
load a local stdio UI plugin, render the sidebar/thread panel or deliver model
context. Record native refusal as UNSUPPORTED only after observing it; lack of
access is NOT RUN. No live installation, public server or release is part of
this probe.

## Application and resource contract

The adapter embeds `crates/orbit-mcp/assets/task-panel/{index.html,css/task-panel.css,js/task-panel.js}`
into one static resource. No frontend build or separate asset endpoint is
needed. The bundle uses text sinks for task content, restricts external assets,
connections and frames, and never interpolates task text into executable HTML.
Resource lookup accepts only the two exact versioned URIs. It does not resolve
paths, query strings or arbitrary artifact contents.

The panel discovers workspaces and requires an explicit returned selector.
The selected host/workspace/entity stays visible. `orbit_desktop_read` provides
bounded task lists/details and run observations; `orbit_desktop_task_snapshot`
returns an opaque revision plus server-computed actions. The separate
`orbit_desktop_task_write` application operation supports proposed-task creation,
allowed field edits, comments and evidence-bound review. Core retains validation,
workspace routing, audit, lifecycle checks and capability enforcement. Opening a
widget, a model-context message or a click grants no operator authority.

Task list search is case-insensitive public task key/title search, applied
before pagination. History search remains `orbit.search`. Pages contain totals,
offset/limit, explicit truncation and a next offset. Task details independently
page comments, history and artifact metadata; they do not return artifact file
contents. Runs use observed state without reconciliation, keep provider input
and response payloads out of the projection, and expose audited log excerpts.
Run reads retain the canonical operator run-read gate. Usage is explicitly
unavailable rather than zero when no usable measurement exists.

Proven precommit validation failures return `mutation_applied: false` with a
`refusal` object, allowing the preserved draft to be corrected. Generic server
errors do not prove that a write failed; retain the request for reconciliation.
Stale task revisions return a typed `revision_conflict` with a fresh snapshot;
they do not overwrite the task or report a completed write. Review records criterion outcomes, current evidence references, task revision
and current run/PR-head binding where applicable. Changes requested stays in
review. Record-only acceptance also stays in review. Completion requires fresh
eligible state and existing trusted operator authority. It never merges,
publishes or dispatches. Missing/stale evidence or unavailable PR-head validation
refuses the affected action; the UI preserves the draft for reconciliation.

The bridge negotiates MCP Apps `2026-01-26` and requires `serverTools`. The
explicit **Send context to chat** action uses `updateModelContext` when supported;
otherwise it offers a bounded copy reference. References contain destination,
entity identity, observed revision/time and a short title, not full logs or
history. They are untrusted discussion context: reread before acting. Selection
changes, failed reads, timeout and teardown invalidate context. Late responses
cannot retarget another selection. The script harness exercises these cases;
actual model-context delivery still requires the desktop host.

## Retry receipts and their lifetime

Retain the exact request ID and payload when reconciling a timed-out write.
A matching retry returns `replayed: true` and a fresh snapshot; it does not
repeat the mutation. Reusing an ID with changed content is refused. Task edit,
comment and review IDs are scoped to that task. Creation IDs are scoped to the
workspace and reserve a stable task identity.

Creation receipts live in the allocation registry's `task_action_keys` mapping;
other receipts are `desktop_mutation` history records committed with the task
bundle. They survive process restart and have **no TTL**. The guarantee depends
on retaining that registry and task history: destructive store reset or deletion
of a task's history removes the corresponding evidence. This desktop surface
has no deletion operation. Receipts are not a cross-host or cross-workspace
request cache, and a fresh request ID is a new requested operation. Revision
checks include body documents, comments, history and artifact metadata, so a
concurrent comment/evidence change can invalidate an editor's older revision.

## Automated reproduction

Use an existing candidate build directory. Before allocating any new build
directory or checkout, run `df --output=pcent <parent-directory>` and stop at
80% usage. The probe also checks disk usage before creating its fixture.

```sh
./scripts/build-budget.py -- cargo test -p orbit-core -p orbit-store desktop
./scripts/build-budget.py -- cargo test -p orbit-mcp --test mcp_wire_roundtrip
./scripts/build-budget.py -- cargo test -p orbit-cli --test mcp_roundtrip desktop
node --test crates/orbit-mcp/src/adapter/tests/task-panel.mjs
python3 scripts/probe-mcp-apps.py --binary target/debug/orbit
ORBIT_PANEL_RESOURCE=.orbit/tmp/mcp-apps-probe/task-panel.html node --test crates/orbit-mcp/src/adapter/tests/task-panel.mjs
```

Use the actual centrally built candidate path with `--binary`; the example
assumes the existing default target directory. The probe needs Python 3.9+, Git
and that already-built binary. It creates a private HOME and repository beneath
this checkout's `.orbit/tmp`, clears inherited managed-run authority and checks
checkout routing before writing any fixture task. It never changes the normal
HOME, installs a plugin, invokes a provider, starts a workflow or changes release
versions. A local no-op provider stub satisfies disposable initialization only.

The production stdio probe checks:

- Resource discovery, MIME/CSP metadata, global/thread entrypoints, the old URI
  alias, arbitrary-resource refusal and ordinary-client data tools.
- Proposed creation and matching retry, conflicting request-ID refusal,
  filtering/pagination, a fresh edit and stale-revision refusal without overwrite.
- A comment and retry without duplicate evidence; independent detail page inputs.
- Evidence-bound changes requested and record-only acceptance, both staying in
  review; unprivileged completion and run-read refusal; forged authority refusal.
- Explicit destination identity and wrong/missing destination refusal. It emits
  `context-reference.json` from returned state without claiming to deliver it to
  a desktop conversation.

Review fixture setup uses an explicit CLI `task update --force --status review`
inside the disposable repository, with a synthetic execution summary. This is
fixture preparation, not a product review shortcut or a dispatched run. The
probe does not grant MCP operator capability, so run-detail contents and allowed
completion are covered by the isolated Rust tests rather than bypassing the
production gate in the probe.

A failing MCP check exits nonzero and writes its transcript; never accept
partially written evidence as PASS. Successful output includes `evidence.json`,
`protocol-transcript.json`, the emitted resource, fixture identity and a staged
`orbit-probe` package. The evidence records candidate HEAD and a digest including
untracked delivery files. Use a new `--output .orbit/tmp/<unique-name>` directory
for each run. Keep evidence until native handoff is complete. The JavaScript
harness executes the shipped script with deterministic DOM/bridge behavior; it
is not a browser engine or desktop shell.

## Deferred native desktop handoff — archived ORB-13715

1. On the actual desktop host, prepare this candidate using its existing build
   location and run the probe. Retain candidate HEAD/digest, binary/version,
   emitted workspace/task key and package/evidence paths. Copy observations into
   [the evidence template](mcp-apps-evidence-template.json), adding named write,
   retry, run-authority and review scenarios as needed.
2. Record exact desktop product/build/OS, plugin source and installed path,
   selected registration and command/args. Inventory manual `mcp_servers.orbit`
   and `orbit@orbit` plugin registrations; do not assume precedence. The distinct
   `orbit-probe` registration must launch the emitted `serve-candidate.py`.
3. With separate authorization for installing into a disposable desktop profile,
   open the emitted fixture repository and select its staged local marketplace
   candidate. The probe stages an AVAILABLE package only; it does not install or
   enable it. Record the actual installed copy/launcher. If loading fails, record
   the exact refusal and leave dependent scenarios NOT RUN. Do not alter normal
   registrations or create a public HTTP endpoint to circumvent that result.
4. Exercise Tasks, Runs and Review in the sidebar, and the selected entity in a
   conversation panel. Confirm explicit host/workspace identity, literal rendering
   of the script-looking fixture title, filter/page controls, stale display after
   failure, preserved edit drafts and clearly disabled unavailable actions.
   In the unprivileged fixture Runs and completion must refuse; separately
   authorized operator-mode checks require their own recorded fixture/session.
5. Create/edit/comment and record review decisions only in disposable state.
   Confirm previewed effect, successful refresh, stale-edit refusal and retry
   reconciliation. Changes requested and record-only acceptance must stay in
   review. Confirm selection changes never dispatch or mutate an entity.
6. Exercise **Send context to chat** and the copy fallback. Record exactly what
   arrives, bridge refusal/absence, stale-context disabling and conversation-local
   selection. Do not interpret a host message as human/operator authority.
7. Record PASS, FAIL, UNSUPPORTED (observed refusal only) or NOT RUN with a reason
   for each scenario. Attach recordings/screenshots and the exact server transcript.
   Native support remains unproven until those observations exist. Installation,
   merging and releases are separate operations with their own authorization.

## Connection modes and remaining limits

| Mode | Resource owner | Evidence boundary |
| --- | --- | --- |
| Local stdio | Accepting adapter | Real disposable protocol probe; native rendering remains NOT RUN. |
| Direct SSH byte relay | Destination adapter | Destination must run this candidate. Real SSH/native validation requires an authorized disposable destination. |
| Federated local | Accepting mux adapter | Static resource stays local; data and write tools use the qualified-selector route and existing grants. Native validation remains NOT RUN. |
| Federated remote | Accepting mux adapter | Destination must advertise the required desktop contracts; missing tools, stale routes and offline/wrong destinations must refuse without fallback. Real remote/native checks remain NOT RUN. |

Record exact argv separately: `mcp serve`, `mcp serve --mode remote <fixture-host>`
and `mcp serve --mode federated`. The staged launcher is local only. Prepare other
modes exclusively in separately authorized disposable state. Direct SSH invokes
`orbit` on the destination PATH; do not replace a production binary for a probe.
For federation, use only the fixture's `.orbit/mcp-destinations.toml`, pin the
machine identity and copy the opaque returned selector. Never edit the normal
HOME's destination catalog for validation.

Current limits are explicit: pages max 50; task list titles max 512 bytes and
crew labels max 128 bytes after shared redaction, with explicit truncation flags;
relation lists max 50 (oversized identities are omitted rather than retargeted); run listing exposes at most the first
10,000 matching runs; audited logs expose at most the first 200 invocations and
4,096 bytes per stream excerpt; step details expose 50 with a truncation flag.
Totals and truncation remain visible when a cap is reached. No arbitrary artifact
reader, dispatch/retry/cancel, bulk action, merge, release, scheduler editor or
operator-authority acquisition is provided. The candidate does not establish a
full browser accessibility pass, real remote compatibility or long-lived upgrade
compatibility. The old resource alias preserves one concrete reload contract;
it is not proof of every live-client migration scenario.

Metadata and bridge contracts follow the official
[UI guide](https://developers.openai.com/plugins/build/chatgpt-ui),
[entrypoint guide](https://developers.openai.com/plugins/build/extensions) and
[packaging guide](https://developers.openai.com/plugins/build/plugins).
Those contracts do not establish availability in an installed client.
