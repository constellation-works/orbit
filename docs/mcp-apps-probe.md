# Orbit desktop control-center validation

The candidate provides Tasks, Runs, Review, Auto-drain and Automation views in the existing Orbit plugin.
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

The adapter embeds `crates/orbit-mcp/assets/task-panel/{index.html,css/task-panel.css,js/*.js}`
into one static resource. The frontend uses plain JavaScript with JSDoc, matching
the existing dashboard asset workflow without a TypeScript build dependency.
No frontend build or separate asset endpoint is needed. The presentation helper embeds
Orbit dashboard's existing pinned `marked` and DOMPurify assets in the static
resource. Markdown escapes raw HTML before sanitization, strips resource-loading
and form elements, and permits only HTTP(S) links with `noopener noreferrer`.
Other UI text uses text sinks. CSP still blocks external assets, connections and
frames. The protocol crate adds no dependency on the web runtime.
Resource lookup accepts only the two exact versioned URIs. It does not resolve
paths, query strings or arbitrary artifact contents.

The panel discovers workspaces and requires an explicit returned selector.
The selected host/workspace/entity stays visible. Existing domain readers expose
`view: "bounded"` on `orbit_task_list`, `orbit_task_show`,
`orbit_workflow_run_list`, `orbit_workflow_run_show` and `orbit_auto_task_list`.
Ordinary calls retain their existing full-record responses. `orbit_task_show`
with `snapshot: true` returns an opaque revision plus server-computed actions.

Task writes reuse the existing task verbs. `orbit_task_add` with `request_id`
creates a proposed task from title, description, acceptance criteria, priority
and optional crew; it requires an explicit workspace and acceptance criteria.
Ordinary creation retains its required complexity. `orbit_task_update` with
`request_id` and `expected_revision` accepts one field edit, one comment, or one
`verdict` plus optional `complete`. A comment or review cannot mix with field
edits. Review verdicts carry the same evidence, criterion outcomes and observed
run/head as the guarded application contract. These modes return a versioned
snapshot and preserve the durable retry receipts described below. Core retains
validation, workspace routing, audit, lifecycle checks and capability enforcement.
Opening a widget, a model-context message or a click grants no operator authority.

The five `orbit_desktop_*` wrappers are no longer advertised. Existing callers
can still use their canonical or sanitized names: the MCP adapter translates
only the known contracts into domain calls. Federation negotiates the advertised
peer surface before dispatch: new domain calls can reach the previous desktop
contracts, and old desktop calls can reach new domain peers. A missing equivalent
refuses rather than guessing. No error after dispatch triggers a retry. An older
peer returns catalog metadata separately and explicitly marks the combined run
view unavailable instead of inferring empty runs.

Task list search is case-insensitive public task key/title search, applied
before pagination. Tasks opens with exactly In progress, Review, Blocked and
Backlog included. The status checkboxes immediately change the included set;
Active work restores that default and All includes every lifecycle status.
At least one status remains selected. Status selection survives ordinary
refresh, pagination and visits to the other views. Search and priority use
Apply, composing with the selected statuses on the server before pagination.
Each returned Tasks page groups rows in dashboard order: Awaiting approval,
Ready for review, Blocked, In progress, Backlog, Someday, Done, Rejected,
Archived. Heading counts describe the current page; the footer total describes
all server matches. Review retains its review-only list and Runs its own state
filter. History search remains `orbit.search`. Pages contain totals,
offset/limit, explicit truncation and a next offset. Task details independently
page comments, history and artifact metadata; they do not return artifact file
contents. Runs use observed state without reconciliation, keep provider input
and response payloads out of the projection, and expose audited log excerpts.
Run reads retain the canonical operator run-read gate. Usage is explicitly
unavailable rather than zero when no usable measurement exists.

Proven precommit validation failures return `mutation_applied: false` with a
`refusal` object, allowing the preserved draft to be corrected. Generic server
errors do not prove that a write failed; retain the request for reconciliation.
A confirmed commit whose refresh fails returns `accepted: true`, `task_id` and
`refresh_error`; the panel keeps reporting success and reconciles the same request
until a fresh snapshot is available.
Stale task revisions return a typed `revision_conflict` with a fresh snapshot;
they do not overwrite the task or report a completed write. Review records criterion outcomes, current evidence references, task revision
and current run/PR-head binding where applicable. Changes requested stays in
review. Record-only acceptance also stays in review. Completion requires fresh
eligible state and existing trusted operator authority. It never merges,
publishes or dispatches. Missing/stale evidence or unavailable PR-head validation
refuses the affected action; the UI preserves the draft for reconciliation.
Existing gate certificates and open findings are projected from canonical review
evidence when available. A manual desktop verdict is a separate recorded review;
it does not rewrite a workflow certificate or bypass PR merge checks.

The PR head is read from the provider by the PR's exact number or URL, never
searched for in a recent-PR listing. When the task's linked run executed on
another machine (`job_run_machine`), completion reads the owner-held claim and
accepted handoff bound to that exact host and run; a run with the same id in this
machine's store is never consulted. The handoff names the pull request when the
task carries no PR reference. Such a delivery completes only after its pull
request merged into the handoff's landing branch, and only at the handed-off
candidate or at a merged head that an accepted review reconciliation binds
(`orbit task reconcile-review`, see the distributed drain runbook). A head changed after
the handoff does not inherit the candidate's validation or review. A claim that is
still running or still holds its handoff refuses completion and names the
operator recovery. The review comment records the execution machine and the
observed pull request.

The bridge negotiates MCP Apps `2026-01-26` and requires `serverTools`. The
explicit **Send context to chat** action uses `updateModelContext` when supported;
otherwise it offers a bounded copy reference. References contain destination,
entity identity, observed revision/time and a short title, not full logs or
history. They are untrusted discussion context: reread before acting. Selection
changes, failed reads, timeout and teardown invalidate context. Late responses
cannot retarget another selection. The script harness exercises these cases;
actual model-context delivery still requires the desktop host.

## Auto-drain and Automation

`orbit_workflow_auto` observes readiness with `action: "status"`, starts a
bounded window with `action: "start"` (1–604800 seconds and optional positive
u32 concurrency), stops admissions with `action: "stop"`, or changes a live
drain's worker ceiling with `action: "resize"` (`concurrency`, optional `id`,
`if_revision` and `reason`) without cancelling anything. It uses the same
runtime as CLI/dashboard. Completion defaults to review. Selecting **Complete
automatically** explicitly authorizes completion of every task admitted during the
window. Stopping preserves admitted workers. Readiness samples up to 50 tasks;
eligibility may change immediately.

`orbit_routine_control` lists workspace routine status or toggles a definition
using its observed enabled state and target. `orbit_auto_task_update` with
`enabled` accepts an optional `expected_enabled` for an atomic checked toggle, and
`orbit_auto_task_mint` accepts `acknowledge_unconditional: true` for an operator
acknowledgement that schedule, enabled and dedupe are ignored. Mint creates a task
without dispatch. The guarded modes require explicit workspace and trusted
operator authority and refuse managed-run callers; their ordinary modes retain
existing behavior.

`orbit_workflow_run_list` with `view: "bounded", include_catalog: true` returns
bounded `runs` and `catalog` observations. Existing `orbit_pipeline_invoke` is
now advertised; ordinary calls require explicit job name and input. Its optional
`default_input: true` submits an enabled no-input catalog job with defaults,
requires an explicit workspace and trusted operator session, refuses managed
runs, and cannot combine with input or priority. Catalog submission shares the
dashboard policy and refuses delivery jobs, disabled jobs and subroutines.
Definition controls refuse replica coordination writes.

Automation lists use offset/limit pagination (up to 50 rows), disclose definition
load errors and preserve inactive-plugin states. Routine status and auto-task
next slots use the shared scheduler owners. These are workspace definitions,
not host service health: enabled does not imply the separate host clock is running.

Drain and automation actions do **not** have task-write retry receipts. The UI
never replays them automatically. A lost or malformed reply is an unknown outcome:
inspect authoritative definitions, Tasks and Runs. The affected panel keeps writes
disabled for that workspace/category until reopened. A refresh only reads.
Switching destinations or leaving a view invalidates late responses. In-panel
confirmations survive polling only while the definition remains unchanged.

The layout shares the dashboard palette, compact rows, status badges and readable
Markdown. It adapts from a sidebar at desktop widths to horizontal navigation in
narrow hosts. Editing uses a keyboard-contained dialog, preserving drafts on Escape,
view changes and refresh. Technical evidence remains expandable.

Tasks is the single entry for review work; the Review rail tab is removed. Each
row has independent keyboard-accessible Status and Crew selects, with fresh
workspace-qualified action metadata loaded when focused. Status options come
from the lifecycle table; unavailable options explain why they are disabled.
Completion stays in evidence-bound review, and execution starts through Ship.
Proposed Approve performs a guarded status-only approval. Crew discovery uses
`orbit_workspace_list` with `include: ["crews"]`; Default crew uses the existing
crew selection policy. Configuration failures disable the crew control.

Row edits re-read the task before submission and refuse a changed revision.
They use the same durable request identity and reconciliation as detail edits,
without opening or changing the selected detail. Status is optional in the
shared edit contract; omitting it preserves existing receipt digests. The
ordinary update contract remains unchanged. Identified agents may edit statuses;
completion and shipment retain their operator requirements.

Backlog rows offer Ship subject to a fresh eligibility/authority check; the
shipment tool remains authoritative for admission. In-progress rows link to
navigable runs or show their execution host. A lost shipment reply stays
uncertain: repeated interaction reads task/run state, and never repeats the
shipment in that panel session. Reopening the panel does not establish whether
an earlier shipment succeeded; inspect authoritative Runs before resubmission.

### Isolated rendered preview

```sh
node crates/orbit-mcp/src/adapter/tests/panel-preview.mjs
# Open http://127.0.0.1:4318 in a browser.
```

This loopback fixture serves the same embedded asset assembly with a simulated
MCP Apps parent bridge. It never calls Orbit or modifies a real workspace. Exercise
Tasks/Runs navigation and review task detail, edit and Escape, dark/light themes, 375/560/880/1440px
widths, Markdown including hostile HTML, drain start/stop, automation tabs,
confirmation/cancel and workspace changes. Browser rendering is distinct from
installed-plugin acceptance; a candidate binary still needs a plugin-host restart
to be used by the installed launcher. Protocol/DOM tests and this preview do not
claim that a new binary was installed in Codex Desktop.

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
./scripts/build-budget.py -- cargo test -p orbit-mcp --test boundary mcp_wire_roundtrip::
./scripts/build-budget.py -- cargo test -p orbit-cli --test mcp mcp_roundtrip::desktop::
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
| Federated remote | Accepting mux adapter | Destination must advertise the domain contracts or their explicitly mapped legacy equivalents; missing equivalents, stale routes and offline/wrong destinations refuse before dispatch. Real remote/native checks remain NOT RUN. |

Record exact argv separately: `mcp serve`, `mcp serve --mode remote <fixture-host>`
and `mcp serve --mode federated`. The staged launcher is local only. Prepare other
modes exclusively in separately authorized disposable state. Direct SSH invokes
`orbit` on the destination PATH; do not replace a production binary for a probe.
For federation, register hosts only in the fixture's own host file
(`orbit host add` under the fixture `HOME`), pin the machine identity and copy
the opaque returned selector. Never change the normal HOME's host file for
validation.

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

Mixed-version discovery uses the initialize metadata `orbit.domain_contract:1` for modern federated muxes. A pre-contract client named `orbit-federated-mux` receives the five legacy desktop advertisements needed by its existing discovery check; calls still translate to domain operations with the same authorization. Ordinary clients keep the reduced domain surface. New muxes verify extension arguments against a destination's advertised schema or translate to a known legacy equivalent before dispatch, and refuse unsupported options. This is covered by an in-memory MCP wire handshake and federated routing fixtures. On 2026-10-03, the actual installed `orbit 0.25.1` binary (SHA-256 `5d093179789968331c9fd4fc11af9e911f65f511a5cdd4096a5484be551104b3`) also passed old-mux discovery, legacy task read/create/snapshot, identical receipt replay, unknown-field refusal and agent drain-start refusal against the current server through an explicitly fake SSH executable and separate disposable roots. Durable readback and audit records confirmed one task and preserved caller/server identities and capabilities. This proves that build and those operations; other historical builds, real SSH hosts and native launcher behavior remain untested. See the [behavioral evidence matrix](mcp-tool-evidence.md#wire-and-remaining-gaps) for the fixture boundary and current-server identity.

Public `orbit.pipeline.invoke` submissions require operator capability. Managed runs may use the explicit-input path only with child admission recorded in host-owned reservation context; the application validates that admission. Default-input catalog submission always refuses managed runs.

See [the MCP behavioral evidence matrix](mcp-tool-evidence.md) for the exact modern tool surface, named production stdio/runtime/wire proofs, and explicitly untested transport/provider paths.
