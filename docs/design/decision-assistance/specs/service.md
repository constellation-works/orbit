---
title: "Service contract: bounded decision assistance"
owner: codex
last_updated: 2026-11-01
last_validated: 2026-11-01
status: Draft
feature: decision-assistance
doc_role: spec
type: design
summary: "Optional typed application port, additive durable claims, conservative charge reservations and direct Decisions translation; consumers remain off."
tags: [decision-assistance]
---

# Service contract: bounded decision assistance

The generic service and [private task-quality collector](task-quality.md) are
implemented; all host grants remain off by default. The [scope](../1_scope.md)
records parked advisory rollout after both development screens failed, with
the failure consumer and advisory surfaces archived/deferred. Code completion
does not establish effectiveness or adoption. The
[configuration reference](../../../CONFIG.md#assistance--optional-bounded-model-annotations)
describes the single host-only table, exact fields, defaults and off switch.

## Application and evaluation contracts

`orbit_types::assistance` defines provider-neutral predicate and string-choice
questions, the evaluator's `decision-assistance-eval/1` evidence envelope and
answer vocabulary, and `decision-assistance-result/1` provenance. It has no I/O.
The consumer supplies its frozen question set, rubric and evidence-builder
versions; source/content digest; owner/workspace/subject identity; collection
time, permitted references, omissions and truncation; and selected JSON fields.
Criterion references are host-selected index/text-digest pairs checked against
the packet. The model cannot invent criterion references. Consumers that need
per-criterion or pair assessments expand their question set themselves.

`enqueue_expanded_assistance(request, secrets, template)` supports a host-granted
template with a dynamic bounded question set. Its exact template digest must
match the grant. Expanded questions may only use a template name followed by at
most two positive numeric indexes, with identical rubric, answer type, allowed
values and confidence policy. Only criterion references may vary; they are
checked against the immutable evidence packet. Contract versions must match.
This admits per-criterion/pair assessments without requiring a host grant per
task or placing consumer taxonomies in the generic service.

A choice question may instead be packet-bound. On the granted template the
question names one evidence-packet field, an id-count bound of at most 8, an
id byte bound of at most 64, and exactly one abstention value. That abstention
is the question's only `allowed` entry and its only `abstain_values` entry.
The template digest covers the field name, those bounds and that abstention.
It does not cover the per-request ids. `enqueue_expanded_assistance` copies
the field's JSON strings, in order, and appends the abstention. The field must
be one of the grant's evidence fields. Each id is a distinct identifier, at
most the declared byte length, different from the abstention; the list holds
at least one id and at most the declared count. A missing field, a value that
is not an array of strings, a duplicate, or an id past either bound refuses
enqueue. The stored question then drops the binding and is an ordinary choice
over that copied set, so answer checks apply to the copied ids plus the
abstention: the distribution lists exactly those values and sums to one within
0.001, and the chosen value is the most probable. An answer outside that set
is a malformed result. `enqueue_assistance` has no template and refuses a
packet-bound question. A question without the binding, including every
existing expanded template, keeps its fixed allowed values, its contract
digest and its admission behavior. With no packet-bound question, an existing
request is admitted as before.

The generic core contains no failure or task-quality taxonomy. The offline
evaluator remains the owner of the two initial versioned question sets. The
manual fixture consumes its `schema` export directly. Extra generic metadata
(thresholds and criterion refs) is additive to the evaluator's typed answers.
No threshold is enabled by default: an absent threshold means `uncertain`.
Predicate values use probability >= 0.5; the evaluated confidence threshold
applies to the selected true/false value. Choice abstention values come from the
consumer. Truncated/omitted evidence cannot produce `suggested` results.

`orbit_core::application::assistance::AssistanceService` is the supported public
consumer boundary. Construct it with an `AssistanceStoreBackend`, an injected
`AssistanceAdapter`, and a host-settings loader; a clock can be injected for
deterministic fixtures. Production composition is
`composition::host_assistance_service(global_root, host_store)`. The store's
`compose::assistance_store_from_store` projects an already-opened host store,
preserving the runtime's generation admission. Construction starts no threads
and resolves no credentials.

Call `enqueue_assistance` with the immutable request and explicitly supplied
secret values. It returns `Disabled`, `Enqueued`, `Existing` or `QueueFull`.
`process_next_assistance(workspace, use_case)` performs at most one attempt;
the owner controls when to call it. `assistance_assessment(id, current_digest)`
is the operator/history view, including pending work and shadow results.
`assistance_advisory` returns nothing in off/shadow mode or for a stale identity.
A completed result stays current and readable while its source digest and
policy still match, until retention ends or a fresher snapshot supersedes it.
Queue age expires queued work only, including retry-pending work. Past queue
age, that work is not current and presents no result or advisory, whether or
not maintenance has yet recorded its `QueueExpired` gap; once recorded, the gap
is readable with `current` false. Queue age never relabels a completed result
`Superseded`. That label is reserved for a moved slot or a changed
policy/source identity.
Consumers must pass the latest semantic digest on every read and reconcile
creation/edit events durably. Atomic task-event publication is a collector duty,
not an implied side effect of this service.

`assistance_policy_epoch` reads the host grant locally without credentials or
work writes. `process_next_assistance_checked` accepts a consumer freshness
predicate checked immediately before and after adapter I/O, outside store
transactions; superseded subjects finish as stale attempts. Consumers still
revalidate semantic identity when projecting results after completion.

Only exactly requested names and expected types are accepted, with unique
choice values and finite probabilities in [0,1]. Choice distributions contain
exactly the allowed values and sum to one within 0.001. The chosen value must
be the most probable one, and the confidence must equal its probability; both
within 0.01, otherwise the answer is malformed. Missing, refused,
malformed, unavailable and stale responses carry distinct fixed reason codes.
A partial refusal abstains for the whole packet. Raw responses, explanations,
provider errors and chain of thought are never persisted or logged. Results
contain only typed answers, fixed diagnostics, known model identities and
usage/latency/estimated-price metadata. Missing metrics remain null/unknown.
Every result is an annotation: this port has no task, readiness, pilot, human
answer, review, recovery or dispatch write capability.

## Durable facilities and compatibility

The existing automation ledger binds branch coverage and workflow obligations;
review reservations bind candidate authority and reviewer runtime. Reusing
either would entangle unrelated policy and retention. Invocation telemetry has
usage but no atomic work claim or prior charge reservation; leases model held
paths rather than result identity. A compact independent ledger is therefore
required. It reuses the host Store's short immediate transactions, schema
compatibility machinery and private SQLite opening.

Migration v72 adds only `assistance_work`, `assistance_slots`,
`assistance_attempts`, `assistance_budgets` and their indexes. It changes no
existing row, trigger, projection or table shape. It is **Additive**: a v71
reader/writer, including an already-open long-lived client, can keep using its
tables. Existing executable-generation admission remains in effect; no
compatibility fence is bypassed. A later incompatible assistance format must
get its own migration/compatibility classification.

The logical ID hashes the subject slot, full host policy/model configuration,
source digest and immutable redacted packet digest. It binds question/rubric,
builder/result versions and evidence provenance. Duplicate IDs never create
another attempt or result. New input coalesces queued older snapshots and
advances one slot pointer. Claims reserve unique deterministic attempt IDs and
ordinal numbers in an immediate transaction. Completion checks the stored
claim, deadline and current slot. Retries preserve the logical ID; old or
superseded attempts cannot overwrite a newer result. An operator model revision
epoch must advance after a provider alias changes; an alias is not proof that
the provider's underlying revision stayed fixed.

No store connection/transaction remains held during adapter I/O. Concurrency
counts every active claim in the host store, across workspaces and workers.
Queue age, total retained record count, input size/depth, request/response bytes,
timeout, delayed retries and maximum attempts are bounded. Queue overflow
increments a durable coverage-gap counter and invalidates the old slot; the
caller receives `QueueFull`, never an assessed result. When the rejected
snapshot shares a slot with queued work, that displaced record becomes
`Superseded` in the same transaction, freeing pending capacity immediately.
Expired work records an unavailable queue-age gap. Maintenance runs on
admission/claim and is also exposed explicitly for an owner maintenance loop,
including while disabled.

Before each attempted request the ledger reserves the entire configured input
token ceiling and its uncached-price charge bound against the host allocation.
There is no automatic budget renewal. Reservations are never refunded from
missing/optimistic usage: timeout, transport loss, cancellation or worker death
may already have incurred charges. Expired claims record an outcome-unknown
attempt before a delayed retry can reserve another. Late responses are fenced;
allocation counters survive retention. Terminal payloads, attempts and slots
expire after the queue deadline plus retention; owners must call maintenance
when no consumer is polling. Tiny allocation counters are retained for audit.
A claim refused before any outbound request is entered releases its reservation
and records no outcome-unknown attempt; that is not a refund, because nothing was
requested. Connect and client-build failures entered the transport and keep the
reservation. This is conservative reservation accounting, not a vendor billing receipt.
If the settings recheck fails after claiming but before outbound I/O, the service
settles the claim as an unattempted configuration-change gap, releases its
reservation and returns the settings error. Later maintenance cannot charge it
as an expired outcome-unknown claim.

## Direct Decisions adapter and data boundary

The adapter uses the fixed HTTPS Decisions endpoint with no redirects/proxy or
agent subprocess. It resolves only the host grant's `ORBIT_ASSISTANCE_*`
credential reference, immediately before redaction/transmission; its value is
never serialized. Shared value-based redaction handles supplied values,
JSON/Debug encodings, nested keys and credential-shaped/named fields before
persistence. Oversize or structurally unsafe input is refused. Ungranted fields
and references cannot widen scope. Only bounded `input_json` is sent: no file,
image, attachment, URL retrieval, conversation/tool execution or ambient
environment collection. Credential rotation between enqueue and send that
would change evidence invalidates the request rather than changing its digest.

The checked vendor contract is the official
[create reference](https://developers.openai.com/api/reference/resources/decisions/methods/create)
and [Decisions guide](https://developers.openai.com/api/docs/guides/decisions),
retrieved 2026-10-29. Requests carry model, shared input and named questions;
responses carry typed answers (including per-question refusal), model and usage.
The adapter translates predicates to evaluator booleans while retaining their
probabilities, and choices to allowed values plus confidence/distributions.
It accepts only the host's requested model or named revision. Synthetic fixtures
exercise this format; no live conformance or account access is claimed.

The endpoint currently has input-based pricing and no output-token charge.
Orbit ships no assumed Decisions price: an operator must supply a verified
uncached price ceiling and provenance before requests are admitted. Decisions
has no request max-tokens field; the adapter refuses a serialized text/JSON body
whose byte bound plus conservative message/question framing exceeds the reserved
input ceiling. Activation must recheck vendor schema, token/pricing assumptions,
project access and model revision. Recorded usage estimates remain separate
from conservative reservations and actual billing, which is unknown here.

## Offline public-boundary verification

The repository's test admission policy excludes speculative feature tests. The
supported manual fixture executable exercises real public application/store and
adapter boundaries in an isolated child process and temporary root under
`.orbit/tmp/`, with a synthetic credential, recorded transport and deterministic
clock. No external request is made and all labels/prices are synthetic:

```sh
mkdir -p .orbit/tmp
python3 scripts/evaluate-decision-assistance.py schema > .orbit/tmp/assistance-eval-schema.json
scripts/build-budget.py -- cargo run -p orbit-core --example assistance_fixture -- .orbit/tmp/assistance-eval-schema.json
```

It covers off with no credential/queue/call, value redaction and scope, strict
answer translation, missing/extra/invalid/refused answers, probability bounds,
choice abstention and choice confidence/distribution consistency,
packet-bound choice admission (copied ids, answer refusal, id-count and byte
bounds) and unchanged legacy choice serialization,
unavailable budgets/pricing, overflow/queue expiry, completed results
outliving queue age until retention or real supersession, and expired queued
and retry-pending work reading the same before and after maintenance,
restart/claim/retry identities, retained outcome-unknown charges, pre-send refusal
releases (including a settings-loader failure after claiming), retention,
shadow/advisory changes, disabled/superseded/late results, lock release before
I/O, workspace config refusal, child credential exclusion and v71/v72 writer
compatibility. The existing store schema-prefix and compatibility fixtures
remain the regression guards for migration and long-lived clients. No live
evaluation, deployment, configuration mutation or consumer activation is part
of this delivery.

## Precedent provider application boundary

`orbit_core::application::task::precedent_provider` defines
`orbit.precedent-provider/1` request/response records and the shared
`validate_precedent_response` boundary. `OrbitRuntime::retrieve_decision_precedents`
reads only the host's `assistance.precedent_providers` grant for the current
workspace. With no entry it returns disabled evidence, scans no histories and
calls no plugins. Existing decision-request reads are unchanged.

The request contains `contract`, `workspace_id`, `request` (id, gate, category,
question, options, optional topic, raised_at), `as_of` and `k`. `as_of` must equal
`raised_at`. The response contains `contract`, `precedents`, `omissions` and
`truncated`. Each precedent has id, question, options, chosen_option, context,
scope (workspace_id, category, optional topic), decided_at, decided_by,
decided_by_kind, reversed and citation (source, ref, digest).

Fixed version-1 bounds apply to UTF-8 bytes, including JSON encoding for whole
messages:

| Field | Bound |
|---|---|
| Request / response | 65,536 / 262,144 bytes |
| Any string | 8,192 bytes |
| `k` | 1–32 (the consumer's normal packet size is 8) |
| Identifiers, category, source | 256 bytes, nonempty, no control characters |
| Options | 1–8, distinct nonempty ids of at most 64 bytes |
| Omissions | At most 32 per source |
| Citation digest | `sha256:` plus 64 lowercase hex digits |

Unknown fields, invalid shapes or kinds, byte/count violations, duplicate ids,
decisions at or after `as_of`, an unknown chosen option, automation answerers or
foreign-workspace records refuse the entire source response. The returned
`provider_gaps` and a durable `decision/precedent_provider_gap` audit event name
the source and fixed refusal reason; raw response/error text is never audited.
Other validated sources remain available. Consumers must persist the entire
retrieval result with their packet, including gaps, omissions and truncation;
partial evidence cannot justify self-answering. Citations are stored as inert
references, never fetched.

The built-in source scans workspace task histories, using immutable request
and resolution events rather than current task text or folded request state.
Both history writer and answerer must be human labels; `model`, `precedent`
and `is_automation_actor` writers are excluded. It ranks by matching category,
matching topic, question-word overlap and recency, then id. A stored category
is preferred; old requests derive workspace/gate or workspace/agent_request/
topic-file (or `untopiced`). Answer, confirmation and reversal events recorded
at or after the cutoff are excluded. Earlier reversals remain contradiction
evidence, never support.

The human-review owner can append `decision_model_confirmed`, whose JSON note
is `PrecedentHumanConfirmation { request_id, answered_at, confirmed_by }`.
The human writer must equal `confirmed_by`, and the record binds the original
model resolution timestamp. Only then can a model answer appear as
`human_confirmed`, attributed to the confirming human. Availability and
`decided_at` use confirmation time, preventing future confirmations leaking
into replay. This retrieval slice adds no confirmation/answering surface.

Validated sources are ordered by descending recency weight (a positive
half-life, normally 90 days, gives the same ordering as decision time), then
configuration source order, then id. Citations deduplicate by digest and the
result is cut to `k`. Any duplicate marked reversed preserves that contradiction.
Neither retrieval nor merging grants decision authority.

Manual verification uses synthetic histories and a locally installed fixture
plugin through the real public API, dispatch, callback and sandbox. It makes
no external network calls and isolates its home/workspace in a child process:

```sh
mkdir -p .orbit/tmp
scripts/build-budget.py -- cargo run -p orbit-core --example precedent_provider_fixture -- .orbit/tmp
```
