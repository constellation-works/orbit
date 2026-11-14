---
title: "Bounded application approvals — Design"
owner: claude
last_updated: 2026-11-14
last_validated: 2026-11-01
status: Draft
feature: application-approvals
doc_role: design
type: design
summary: "Design: options compared, threat model, consent schema v2 with typed field constraints, admission algorithm, office action map, audit record, migration and implementation slices for bounded application approvals"
tags: [application-approvals, application-authority, plugins, security]
paths: ["crates/orbit-runtime/src/application_consent/**", "crates/orbit-types/src/tool/application.rs", "crates/orbit-common/src/governance/authorization/catalog/application.rs", "crates/orbit-core/src/application/application_authority.rs", "crates/orbit-tools/src/plugin/callback/**"]
related_features: [plugins, human-decision-channel, auditability, state-compatibility]
related_artifacts: [ORB-20852, ORB-20362, ORB-20363]
---

# Bounded application approvals — Design

This doc specifies the mechanisms behind [1_scope.md](./1_scope.md). It compares the
three candidate flows and states the chosen flow's threat model, consent schema,
admission algorithm and audit record. It then maps each office app approval action to
allowed or refused, gives the migration for existing consents, and lists the
implementation slices. The original refusal snapshot is `9a03c7cc5` (2026-11-01).
Orbit slices 1-5 have since shipped as [ORB-21038], [ORB-21039], [ORB-21040], [ORB-21041] and [ORB-21042]; §1 describes current
admission, while §6 retains the original office action map. Line numbers are for orientation
and drift between revisions; the symbols are the anchors. orbit-office
paths are from that repository at `b5654763e`, written as `orbit-office/<path>`.

## 1. What admission does today

The flow has four parts. The runtime implementation is grouped under
`crates/orbit-runtime/src/application_consent/`: `authority.rs` owns consent and
revocation, `validation.rs` owns operation validation, `admission.rs` evaluates
constraints, and `invocation.rs` owns call admission, audit, rate charging and
witnesses. The snapshot line references below describe the original
`application_authority.rs` layout; use the named symbols in these modules.
Other crates are named explicitly.

1. **Consent** (`consent_application`, :68-179). It is operator-only. It refuses agent
   envelopes, application sessions and plugin callbacks (:79-88). The request needs an
   application id, a future expiry and 1-64 scopes. The 4 KiB total budget applies
   only to v1 requests; v2 uses an 8 KiB per-operation budget and the host expiry cap
   (§4.1). Each scope's operation and callbacks pass `validate_application_operation`.
   Orbit mints a credential and writes a SQLite record plus an owner-only witness under
   `state/application-consents/` (:130-169). It then writes a `consent` or `rotate`
   lifecycle audit event (:170-177).
2. **Operation validation** (`validate_application_operation`). An operation must carry
   exactly one of an exact JSON object (`input`) or a constrained field map (`fields`).
   A plugin tool must name its installed, enabled manifest digest. A built-in must
   have a row in `APPLICATION_OPERATION_CEILINGS`
   (`crates/orbit-common/src/governance/authorization/catalog/application.rs`). Exact
   operations retain their key and status lists; constrained operations also pass the
   field, identity, transition and callback rules in `validation.rs` (§4.4). Admission
   runs validation again on every call, so a stale plugin digest refuses later calls.
3. **Admission** (`admit_application_call`, :310-412). Orbit resolves the invocation
   from an inherited callback record, the session, or the process credential descriptor
   (:317-342). It refuses agent and capability sessions (:347-354). It checks the record,
   the witness, `schema_version` 1 or 2, revocation, record equality, expiry (:355-368),
   and the workspace id and checkout root (:369-375). A callback must match a listed
   exact callback or satisfy its constrained fields against the entry's bindings and
   host-held task pin; the entry-bound artifact digest is rechecked. A direct call may
   match an exact scope's tool and input or satisfy a constrained `fields` scope
   (:395-404). The admitted invocation goes into the session and the thread-local audit
   slot (:409-411).
4. **Execution and audit.** An admitted built-in runs through `execute_application_builtin`
   (`crates/orbit-core/src/application/application_authority.rs:9-80`). It runs as the
   actor `application:<id>` through `update_task_as_application` or
   `answer_decision_request_as_human`. An admitted `orbit.task.update` can pass `status`,
   `comment`, `note`, `plan` and `execution_summary` to `update_task_as_application`;
   `note` is accepted only when `status` is also supplied (:45-60). The dispatch audit
   row takes the invocation
   (`crates/orbit-core/src/adapter/command/dispatch/execute/audit_store.rs:106-216`) and
   stores it as the redacted `application` JSON column
   (`crates/orbit-store/src/driver/sqlite/audit_event_store/insert.rs:274`).

Admission runs before the capability chokepoint in `authorize_registered_tool`
(`crates/orbit-runtime/src/tool_exec.rs:175`), in `run_tool_with_context_and_capability`
(`crates/orbit-core/src/adapter/tool_execution.rs:122-135`) and on every plugin callback
(`crates/orbit-core/src/adapter/command/dispatch/execute/runtime.rs:513-520`).
App-bound completion callbacks are sent to the host, because the witness is hidden from
plugin sandboxes. `routes_on_host` in
`crates/orbit-tools/src/plugin/callback/host_route.rs` routes every application callback
listed by `application_callback_tool` in the ceiling table: task updates, exact decision
answers, constrained task/artifact reads and payload-bound auto-task add/update calls.
A plugin child's session record carries the bound invocation (`bind_application`,
`crates/orbit-tools/src/plugin/callback/session.rs:152-177`, limited to 8 KiB). A
long-lived MCP backend is keyed by the serialized invocation
(`crates/orbit-tools/src/plugin/mcp.rs:167-185`).

## 2. Options compared

| | (a) Parameterised consent scopes | (b) Per-click decision request | (c) Operator-held approval surface |
|---|---|---|---|
| How the user's click becomes authority | The app server presents a call that satisfies a consented constraint bound to current task state | The app answers a decision request that Orbit raised for that click; Orbit applies the answer | The user approves in an Orbit surface that already holds operator authority (dashboard or CLI) |
| What a stolen app credential can do | Approve what is pending, at its current payload, within the profile, until expiry or revocation | The same: the app server that relays clicks can answer every request it can read | Nothing new: the app holds no approval authority |
| Strength against a forged click | None beyond the relayer, which is also true of (b) | None, unless the human authenticates on an independent channel | Human at an operator surface |
| New Orbit machinery | Constraint evaluator, ceiling table, bound callbacks, consent v2 | Constrained consent for `orbit.decision.answer` (its `id` is per click too), plus an "apply via plugin as the human" step, because Orbit core cannot apply an office payload | A generic "run a plugin approval tool as operator" dashboard action, plus a payload preview the dashboard can render |
| Generic for other apps | Yes: tags, statuses, artifact paths and pointers | Partly: a decision kind per approval type | Partly: the dashboard must learn each plugin's preview |
| Office UX | Unchanged: one click on Today | One click plus a request round trip. Accept-all creates N requests | Broken front door: the user leaves the app for the dashboard for every approval |
| Audit | Per call: scope, bound values, predicate evidence | Per click: request, answer, apply | Operator audit rows, as today |
| Replay | Bounded by the predicate on current state ([§3.3](#33-replay)) | Single-use request | Operator session |

**Choice: (a).** (b) adds a record, a round trip and an apply orchestrator. It still has
to parameterise the answer call, because each answer names a fresh request id. So it
needs (a)'s machinery as well, and the relayed click is no better authenticated
([4_decisions.md](./4_decisions.md#a-relayed-click-is-no-stronger-than-its-relayer)).
(c) is the strongest authority, but it gives up the product goal of ORB-20363: approvals
from the app's Today screen. It also pulls plugin payload rendering into Orbit. (c)
stays the answer for unattended-execution controls: routine toggles and unconditional
mints remain operator surfaces ([4_decisions.md](./4_decisions.md#operator-floored-operations-stay-off-application-authority)).
(a) takes two ideas from (b): every admission records its bound evidence, and every
check is on current state, so a click on stale state is refused.

## 3. Threat model

Assets: task lifecycle state (approval, closure, rework), proposal payload application,
the auto-task catalogue (unattended minting), routines, and outbound messages through
the gateway. Trust boundary: Orbit's same-user checks guard against accidents and
confined code. They do not defend against arbitrary code running as the host's OS user
(the `applications.md` reference says this plainly). Agent and plugin sandboxes hide the
credential file, the witnesses and the consent store.

### 3.1 Stolen app credential

*Attacker:* a process outside the agent and plugin sandboxes that holds the credential
bytes, for example through a misplaced backup, a log or a copied service directory.

*Can:* within one workspace and checkout, until expiry or revocation, make any call the
profile admits. In practice it can accept every pending `proposal`-tagged proposal at
its current payload digest (it can read the digests). It can dismiss office approval
tasks, send `review` approvals back to `backlog`, close office approval tasks, and send
pending gateway drafts at their current digest.

*Cannot:*

- change a payload: the entry's `task` predicate binds the artifact digest, and every
  bound read and write rechecks it ([§5](#5-admission-algorithm) step 6);
- touch a task outside the predicate's tags and statuses;
- make work dispatchable: `proposed`, `someday` and `blocked` to `backlog` are never
  admitted
  ([§4.4](#44-ceilings));
- edit fields beyond the ceilings (no `granted_tools`, `force`, context files or crew);
- run routines, mint, dispatch, or administer consent;
- reach another workspace or host;
- act after revocation: every admission rereads the record and the witness.

*Bounds:* constrained consents expire within 30 days. Daniel set that default on
host-only `machine.application_consent_max_days` ("30 can be default but
configurable"); [ORB-22808] fixed the cap at 30 days and ignores the key
([§4.1](#41-request)). Revocation is immediate for the next admission. Every admission is
audited with its bound values, so misuse is visible after the fact. A scope may carry an
optional rate limit (Daniel: "configurable, default off"): none is set unless the
operator writes one ([§4.2](#42-scope-and-operation)). Both answers are recorded in
[1_scope.md](./1_scope.md#open-questions).

### 3.2 Malicious plugin

*Attacker:* the code of a consented plugin. Either it was malicious when consented, or it
was swapped after consent.

- **Swapped plugin.** The scope pins the installed manifest digest. Consent and every
  admission recheck the live install path, the enabled flag and the digest
  (`application_authority.rs:231-266`). A swap refuses the call.
- **Consented plugin, inside a bound call.** Its Orbit callbacks are admitted only when
  they satisfy one of the scope's listed callbacks. Each callback's `id` is `bound` to
  the entry's task or pinned on first use, so a plugin cannot reach a second task.
  `artifact_json` binds auto-task definitions to the reviewed payload bytes. Free text is
  limited to `plan`, `comment`, `note` and `execution_summary`, with length caps and a
  redaction check.
- **Effects outside Orbit.** Orbit cannot see what a plugin writes outside Orbit, such
  as Library pages or HTTP. The pinned digest is the only control, as today.
- **Plugin outside a bound call.** It keeps its ordinary `permissions.orbit_tools` grant,
  confined to its sandbox. An app binding never widens it: callbacks are the
  intersection of plugin grant, caller ceiling and consent.

### 3.3 Replay

- **The same click again.** Each predicate reads current state. A replayed accept fails
  `status: proposed`, because the first accept moved the task. The start transition to
  `in-progress` is the atomic pickup that Orbit's lifecycle already enforces, so two
  concurrent accepts cannot both pass. A replayed dismiss or send-back fails its
  `transition.from`.
- **An old click on changed state.** The payload digest no longer matches, so the call
  is refused before any write.
- **A captured callback reference.** The invocation in a plugin child's session record
  works only inside that host-minted session and on the host route. It is rechecked
  against the stored record on every call, as today.
- **Residual.** A comment-only call has no state precondition, so replaying it appends a
  duplicate comment. The profile below admits no comment-only call. A consumer that
  needs one must accept that it can be duplicated.

## 4. Consent schema v2

### 4.1 Request

```json
{
  "application_id": "orbit-office",
  "expires_at": "2026-12-01T00:00:00Z",
  "scopes": [ApplicationScope, ...]
}
```

The rules are unchanged for a request whose every scope is exact, unnamed and
unlimited: it is written as v1 and keeps its 4 KiB request budget. A request with any
constrained, named or rate-limited scope is written as v2 and differs in two ways:

- Each operation (entry or callback), serialized, is at most 8 KiB. The 4 KiB request
  budget does not apply.
- Its `expires_at` must be at most 30 days after consent. The cap was the global-only
  `machine.application_consent_max_days` key until [ORB-22808] fixed it at its default;
  the key is now deprecated and ignored.

### 4.2 Scope and operation

```json
{
  "name": "proposal-accept",
  "operation": ApplicationOperation,
  "callbacks": [ApplicationOperation, ...],
  "rate_limit": {"admissions": 60, "window_seconds": 3600}
}
```

- `rate_limit` is optional, and absent by default: a scope without one has no limit.
  When set, `admissions` is 1-10000 and `window_seconds` 1-86400; consent validation
  refuses anything else. Admission enforces it from slice 2: each admitted entry
  call is charged once, however many chokepoints admit it, to a private admission
  log beside the consent witness, and a call over the limit is refused and audited.

- `name` is new. It is optional on an exact scope and required on a constrained one. It
  matches `[a-z0-9-]{1,48}` and is unique within the consent. Audit rows and refusals
  use it.
- `ApplicationOperation` keeps `tool` and `plugin`, and holds **exactly one** of:
  - `input`: today's exact JSON object (v1 semantics, unchanged); or
  - `fields`: a closed map from field name to a constraint.
  Both or neither is refused at consent time. `deny_unknown_fields` stays.

### 4.3 Constraints

A constraint is a JSON object with exactly one key.

| Key | Value | Admits | Where |
|---|---|---|---|
| `const` | any JSON | a structurally equal value | entry, callback |
| `one_of` | array of 1-16 JSON values | a value equal to one element | entry, callback |
| `text` | `{"max_chars": 1..4096}` | a string of at most `max_chars` Unicode scalar values, no control characters except `\n` and `\t`, unchanged by `redact_sensitive_env_json` | entry, callback |
| `token` | `{"max_len": 1..128}` | a non-empty ASCII string over `[A-Za-z0-9._:-]` | entry, callback |
| `sha256` | `{}` | 64 lowercase hex characters | entry, callback |
| `workspace` | `{}` | a selector that resolves to the consent's `workspace_id` | entry, callback |
| `task` | `TaskPredicate` | the id of an existing task in this workspace whose current state satisfies the predicate | entry, callback |
| `transition` | `{"from": [status, ...], "to": status}` | the string `to`, when the current status of the task named by this call's `id` is in `from` | entry, callback |
| `bound` | entry field name | a value equal to the entry call's admitted value for that field | callback |
| `artifact_json` | `{"path": "...", "pointer": "/..."}` | a value equal to the JSON at an RFC 6901 pointer inside the entry-bound task's artifact `path`, whose digest the entry bound | callback |
| `subset_of` | array of 1-32 strings | an array of distinct strings, each in the set | entry, callback |
| `optional` | another constraint | absence, or a value that satisfies the inner constraint | entry, callback |

`TaskPredicate`:

```json
{
  "tags_all": ["proposal"],
  "tags_any": ["approval", "script_output"],
  "status": ["proposed"],
  "artifact_sha256": {"path": "proposal.json", "field": "sha256"},
  "pin": false
}
```

- At least one of `tags_all` or `tags_any` is required, and `status` (1-16 values) is
  required. A predicate that would match every task is refused at consent time.
- `artifact_sha256` requires the sha256 of the task's stored artifact bytes at `path`
  (the bytes `orbit.task.artifact` action `get` returns) to equal the call's field `field`. That
  field must itself be constrained `sha256`. It is allowed only on an entry.
- `pin: true` is allowed only on a callback `id`. The first admitted callback that names
  a task binds that task for the rest of the invocation. Every later callback with a
  pinned or `bound` id must name the same task.

Field rules:

- Every field the call sends must appear in `fields`. Every field not wrapped in
  `optional` must be sent. There are no defaults.
- An **identity field** (`id`, `name`, `path`, `draft`) must be `const`, `one_of`,
  `task`, `bound`, `token` or `artifact_json`, never `text`. `token` is allowed only for
  plugin-internal ids that Orbit cannot resolve (the gateway draft id).
  [4_decisions.md](./4_decisions.md#identity-fields-bind-to-state-not-to-free-text)
  records the standing rule.
- Free-text fields (`comment`, `note`, `plan`, `execution_summary`) may be `text`.

### 4.4 Ceilings

A closed table in `crates/orbit-common/src/governance/authorization/catalog/application.rs`
(`APPLICATION_OPERATION_CEILINGS`) replaces `APPLICATION_BUILTIN_OPERATIONS`. A consent
may narrow a ceiling, never exceed it. Consent-time validation checks every scope
against it, and admission checks again. Each row also keeps the v1 exact-form key list
(`exact_fields`): `orbit.task.update` and `orbit.decision.answer` only, unchanged. The
`orbit.auto_task.*` callback rows shipped in slice 5 (ORB-21042).

| Tool | Entry | Callback | Fields at most | Status targets at most |
|---|---|---|---|---|
| `orbit.task.update` | yes | yes | `id`, `status`, `comment`, `note`, `plan`, `execution_summary`, `workspace` | `in-progress`, `review`, `done`, `rejected`, `archived`, and `backlog` only through `transition` with `from` within {`review`, `in-progress`} |
| `orbit.decision.answer` | yes (exact only) | yes (exact only) | `id`, `answer`, `note`, `workspace` | — |
| `orbit.task.show` | no | yes | `id`, `fields` (subset of `id`, `status`, `tags`, `terminal`, `artifacts`, `crew`, `updated_at`, `execution_summary`), `workspace` | — |
| `orbit.task.artifact` action `get` | no | yes | `id`, `path` (`const` only), `workspace` | — |
| `orbit.auto_task` action `add` | no | yes | `name`, `description`, `schedule`, `template`, `dedupe`, `workspace`; every field except `workspace` must be `artifact_json` | — |
| `orbit.auto_task` action `update` | no | yes | `name` (`artifact_json`), `enabled`, `workspace` | — |
| plugin tool | yes | no (plugins cannot call plugins) | declared input schema | — |

`orbit.task.update` targets `in-progress`, `review` and `backlog` only through a
`transition` constraint; `status` as `const` or `one_of` may name only `rejected`,
`archived` and `done`. Orbit's ordinary lifecycle checks still apply on top: an
application write never forces a transition.

**Always refused**, whatever the consent says: every operation or input that carries an
Operator floor in the governed-operation catalogue, including the input-dependent floors
in `authorize_registered_tool` (`crates/orbit-runtime/src/tool_exec.rs:163-227`).
Consent-time validation derives this from the catalogue, not from a list
(`application_operator_floored`): a built-in whose governed row admits `Operator`, or an
input field listed in `INPUT_DEPENDENT_OPERATION_FLOORS` whose operation does. Today that
covers `orbit.routine.control`, `orbit.auto_task` action `mint` with `acknowledge_unconditional`,
`orbit.auto_task` action `update` with `expected_enabled`, `orbit.auto_task.delete` and `reset`,
`orbit.workflow.*` and pipeline dispatch, `granted_tools`, and every `application`
command. Outside the ceiling table, `orbit.task.add` and lifecycle `force` are refused
too. Also refused: `backlog` from `proposed`, `someday` or `blocked`, which is the
approval-for-dispatch transition.

### 4.5 Example: the office profile

Abridged to accept and send-back. Slice 6 owns the full profile.

```json
{
  "application_id": "orbit-office",
  "expires_at": "2026-12-01T00:00:00Z",
  "scopes": [
    {
      "name": "proposal-accept",
      "operation": {
        "tool": "office.proposal-accept",
        "plugin": {"name": "office", "manifest_digest": "<installed digest>"},
        "fields": {
          "id": {"task": {"tags_all": ["proposal"], "status": ["proposed"],
                          "artifact_sha256": {"path": "proposal.json", "field": "sha256"}}},
          "sha256": {"sha256": {}},
          "choice": {"optional": {"one_of": ["theirs", "mine"]}}
        }
      },
      "callbacks": [
        {"tool": "orbit.task.show", "plugin": null,
         "fields": {"id": {"bound": "id"}, "fields": {"subset_of": ["id", "status", "tags"]}}},
        {"tool": "orbit.task.artifact.get", "plugin": null,
         "fields": {"id": {"bound": "id"}, "path": {"const": "proposal.json"}}},
        {"tool": "orbit.task.update", "plugin": null,
         "fields": {"id": {"bound": "id"},
                    "status": {"transition": {"from": ["proposed"], "to": "in-progress"}},
                    "plan": {"text": {"max_chars": 512}},
                    "comment": {"text": {"max_chars": 512}}}},
        {"tool": "orbit.task.update", "plugin": null,
         "fields": {"id": {"bound": "id"},
                    "status": {"transition": {"from": ["in-progress"], "to": "review"}},
                    "execution_summary": {"text": {"max_chars": 4096}}}},
        {"tool": "orbit.task.update", "plugin": null,
         "fields": {"id": {"bound": "id"},
                    "status": {"transition": {"from": ["review"], "to": "done"}},
                    "execution_summary": {"text": {"max_chars": 4096}}}}
      ]
    },
    {
      "name": "approval-send-back",
      "operation": {
        "tool": "orbit.task.update", "plugin": null,
        "fields": {
          "id": {"task": {"tags_all": ["approval"], "status": ["review"]}},
          "status": {"transition": {"from": ["review"], "to": "backlog"}},
          "comment": {"text": {"max_chars": 2048}}
        }
      },
      "callbacks": []
    }
  ]
}
```

### 4.6 Persisted types

The changes in `crates/orbit-types/src/tool/application.rs`, all additive:

- `ApplicationConsent.schema_version` is `2` when any scope is constrained, named or
  rate-limited. Any other consent is still written as `1` (see
  [§8](#8-migration-and-compatibility)).
- `ApplicationScope` gains `name: Option<String>` and
  `rate_limit: Option<ApplicationRateLimit>`.
- `ApplicationOperation` gains `fields: Option<BTreeMap<String, ApplicationConstraint>>`
  next to `input`. Exactly one is set. `input` becomes `Option<Value>` with serde default
  absent, so v1 records still read.
- `ApplicationInvocation` gains `bindings: BTreeMap<String, Value>` (the entry's admitted
  values for identity and digest fields), `text_digests: BTreeMap<String, TextDigest>`
  (`{sha256, chars}` for each admitted text field), `pinned_task: Option<String>` and
  `entry_execution_id: Option<String>`. All are serde-defaulted.

## 5. Admission algorithm

`admit_application_call(tool, input, session, enforcement)`, with the current steps kept
and the new ones marked **new**:

1. Resolve the invocation from the inherited callback record, the session, or the
   credential descriptor (:317-342). No invocation means an ordinary caller: return
   `false`.
2. Refuse agent envelopes, worker invocations and capability-bearing sessions (:347-354).
3. Load the record and verify the witness. **New:** accept `schema_version` 1 or 2.
   Refuse when revoked, when the record differs from the invocation's consent, or when
   expired (:355-368).
4. Refuse when the workspace id or checkout root differs (:369-375).
5. **Entry** (`invocation.callback == false`). For each scope in order whose
   `operation.tool == tool`:
   - exact form: match when `operation.input == input` (unchanged);
   - constrained form (**new**): evaluate `fields` against `input` (§5.1). It matches
     when every field passes.

   The first match wins and sets `scope_index`, `bindings` and `text_digests`. No match
   is a refusal: `operation or input is outside application consent`. **New:** for a
   constrained scope of the same tool, the refusal also names the scope and the first
   failing field and constraint kind. It never echoes a value. Then run
   `validate_application_operation` on the matched scope: live plugin digest, plus the
   ceiling table for built-ins.
6. **Callback** (`invocation.callback == true`). Take the scope at `scope_index`. For
   each listed callback whose `tool == tool`, evaluate it against `input` with the
   invocation's `bindings` and `pinned_task` in context. The first match wins. **New:**
   if the entry bound `artifact_sha256`, recompute the bound task's artifact digest and
   refuse a mismatch (`bound artifact changed`). If a `pin` predicate matched with no
   pin set yet, record `pinned_task` in the session's host route
   (`pin_routed_application_task`), which the host owns and which lives exactly as long
   as the session. The child can neither write nor reset it, and a rewrite of the
   session record cannot race `bind_pid`. A pin outside a host-route call is refused.
   No match is a refusal: `completion callback is outside application consent`.
7. Record the invocation in the audit slot and the session (:409-411).

Execution (**new**, `crates/orbit-core/src/application/application_authority.rs`):

- `orbit.task.update` passes `plan`, `execution_summary` and `note` through to
  `update_task_as_application` (`crates/orbit-core/src/application/task/update/entry.rs`).
  The ordinary lifecycle checks run, including the atomic start transition, and the
  write rechecks under the task lock the status admission saw (the invocation's
  `predicate.status_seen`).
- `orbit.task.show` runs the ordinary read, limited to the admitted `fields`.
- `orbit.task.artifact` action `get` runs the ordinary read. When the invocation bound that
  artifact, Orbit rechecks the returned bytes' digest before returning them, which
  closes the gap between step 6 and the read.
- `orbit.auto_task` action `add` and `update` (slice 5) run the ordinary handlers as
  `application:<id>`. Their own catalogue rows and floors still apply.

Host route (**new**): an app-bound session sends every consented callback tool to the
host, not only the two completion tools. The routed set is the ceiling table's callback
column (`application_callback_tool` in
`crates/orbit-common/src/governance/authorization/catalog/application.rs`), so `orbit.task.show` and
`orbit.task.artifact` action `get` are routed too. The read executors run in the adapter
(`execute_application_read`, `crates/orbit-core/src/adapter/tool_host/task/read.rs`),
because the projection and artifact serialization they reuse live there.

### 5.1 Evaluating a constrained operation

```
eval(fields, input, ctx):
  if input is not an object: refuse
  for key in input.keys: if key not in fields: refuse(key, "unlisted")
  for (key, c) in fields:
    v = input.get(key)
    if v is None: if c is optional: continue else refuse(key, "missing")
    check(c, v, ctx) or refuse(key, kind(c))
  return bindings, text_digests

check(task P, v):    v is str; t = ctx.get_task(v) in this workspace;
                     P.tags_all ⊆ t.tags; P.tags_any ∩ t.tags ≠ ∅ (if given);
                     t.status ∈ P.status;
                     if P.artifact_sha256: sha256(artifact(t, path).bytes) == input[field]
                     if P.pin: ctx.pinned_task is None or == v
check(transition T, v): v == T.to and ctx.get_task(input.id).status ∈ T.from
check(bound f, v):   v == ctx.bindings[f]   (or ctx.pinned_task, for id)
check(artifact_json A, v): v == json_pointer(artifact(ctx.bound_task, A.path), A.pointer)
                     and that artifact's digest == the entry binding
```

The evaluator reads state through `ApplicationPort`: `get_task` and, from slice 2,
`task_artifact_sha256`. The kernel never reaches into `orbit-core`
directly (`crates/orbit-runtime/CLAUDE.md`). Predicate reads and the write are not one
transaction. Lifecycle preconditions on the write (the atomic start, `transition.from`
rechecked by the update itself) close the race for status. The digest recheck closes
it for payloads.

## 6. Office approval actions

Each action named in ORB-20363, mapped to the exact-only admission snapshot at
`9a03c7cc5` and to its outcome under the chosen flow. The refusal column is historical;
Orbit slices 1-5 have shipped. "Profile" means the consent the operator grants in slice
6, still tracked by ORB-20363. "Allowed after slice N" records the Orbit prerequisite;
it does not establish that the office profile has been installed or exercised.

| Action (app code) | Orbit calls | Original refusal | Under (a) | Code it changes |
|---|---|---|---|---|
| **Accept** (`accept`, `orbit-office/app/packages/server/src/features/approvals/service.ts`) | `office.proposal-accept {id, sha256, choice?}`. Callbacks (`orbit-office/crates/office-tools/src/tools/proposals/accept.rs`): `orbit.task.show {id, fields}`, `orbit.task.artifact action get {id, path}`, `orbit.task.update` to `in-progress` with `plan` and `comment`, to `review` and to `done` with `execution_summary`. Auto-task kinds: `orbit.auto_task` action `add` and `orbit.auto_task action update {name, enabled}` | Entry: exact input (`application_authority.rs:395-404`). Callbacks: reads not consentable (`ApplicationOperationCeiling` docs in `catalog/application.rs`, `application_authority.rs:120-126`). `plan` and `execution_summary` keys (:270-286). `in-progress` and `review` statuses (:287-296). Host route lists only the two completion tools (`host_route.rs:53-76`) | **Allowed** after slices 1-3 for knowledge, runbook, retire-page and shared-conflict kinds. `auto_task` and retire-auto-task kinds are **allowed after slice 5**, with the office payload carrying the exact `orbit.auto_task` action `add` input (slice 6). Until then they are **refused** with the not-permitted message | Constraint evaluator and bindings (admission steps 5-6), ceiling table replacing `APPLICATION_BUILTIN_OPERATIONS`, executor fields, host-route callback list |
| **Accept-all** (`acceptAll`) | One `office.proposal-accept` per proposal, in turn | Same as accept | **Allowed**, admitted per item. A refused item is reported and the rest continue, as today | Same |
| **Dismiss** (`dismiss`) | `orbit.task.update {id, status: rejected, comment}` | Exact input (`application_authority.rs:399`). The status is already in the v1 list (:291) | **Allowed**: `id` is a `task` predicate whose `tags_any` names the tags of the three dismissable kinds (proposal, script output, gateway draft; slice 6 fixes the exact tag names) and the open statuses, `status` is `const` `rejected`, `comment` is `text`. `office.proposal-reconcile`, which ORB-20363 names, is not in the orbit-office checkout read. If it lands, it gets an entry scope with accept's read callbacks and the `rejected` transition | Constraint evaluator (step 5) |
| **Approve** (`approve`) | `office.artifact-promote {task}`. Callbacks (`orbit-office/crates/office-tools/src/tools/artifacts/promote.rs`): `orbit.task.show` with fields `id`, `status`, `tags`, `artifacts`, `crew`, `updated_at`, and `orbit.task.artifact` action `get`. Then direct `orbit.task.update` steps toward `done` with `comment` and `execution_summary` | Entry exact; read callbacks not consentable; `in-progress` and `review` statuses; `execution_summary` key | **Allowed** from `review`, `in-progress` and `backlog`. A step from `proposed` to `backlog` is **refused** (approval-for-dispatch). Slice 6 changes the app's `closeSteps` to go from `proposed` straight to `in-progress` (the start transition), as accept does | Ceiling table (no `backlog` from `proposed`), `transition` constraint |
| **Send back** (`sendBack`) | `orbit.task.update {id, status: backlog, comment}` | Exact input (`application_authority.rs:399`) | **Allowed**: `transition` from `review` to `backlog`, a `task` predicate on the approval tag (exact name fixed by slice 6), `comment` as `text` | `transition` constraint and the `backlog` ceiling rule |
| **Gateway send** (`send`; `orbit-office/gateway/src/orbit_gateway/drafts.py`) | `gateway.http-send {draft, sha256}`. Callbacks: `orbit.task.show {id, fields}` and `orbit.task.update` with `comment`, `note`, a close walk and `execution_summary` | Entry exact; `orbit.task.show` not consentable; walk statuses; `note` and `execution_summary` keys | **Allowed**: `draft` is a `token`, `sha256` is a `sha256`, and the callback `id` is a `task` predicate with tag `gateway-draft` and `pin: true`. The walk from `proposed` must avoid `backlog`: slice 6 changes the gateway's `_CLOSE_FROM["proposed"]` to start at `in-progress`. Orbit does not bind the draft digest. The gateway plugin checks it, as today (resolved question 3: "No, plugin checks it") | `token` and `pin`, host-route callback list |
| **Routine control** (`orbit-office/app/packages/server/src/features/routines/index.ts`) | `orbit.routine.control {action: toggle, ...}` | Consent time: not an application built-in (`application_authority.rs:298-302`). Operator-only catalogue row (`orbit.routine.control` in `catalog/operations.rs`) | **Refused (out).** Stays on the operator surfaces: dashboard `routine.toggle` (`DASHBOARD_ROUTINE_TOGGLE` in `catalog/types.rs`) and the CLI. The app keeps its not-permitted message | Named in the always-refused list (§4.4); no admission change |
| **Auto-task mint** (`mintAutoTask`, `orbit-office/app/packages/server/src/orbit/client.ts`) | `orbit.auto_task action mint {..., acknowledge_unconditional}` | Consent time (`application_authority.rs:298-302`). The `acknowledge_unconditional` input takes the `orbit.routine.control` floor (`tool_exec.rs:195-205`) | **Refused (out).** Stays on the dashboard `auto_task.mint` row (`DASHBOARD_AUTO_TASK_MINT` in `catalog/types.rs`) | Named in the always-refused list (§4.4) |

The app's plain reads (`orbit.task.show` and `orbit.task.artifact` action `get` before an action,
`service.ts`) and its auto-task enable toggle (`orbit.auto_task action update {name, enabled}`,
`routines/index.ts`) are not approvals. They stay on the ordinary path, made without the
credential descriptor. The app passes the descriptor only to the calls above.

## 7. Audit record

Every admitted or refused application call already carries the `application` object
(`ApplicationInvocation`, redacted, in the audit row's `application` column). It holds the
consent record, `scope_index` and `callback`. v2 adds these fields, all non-secret:

```json
{
  "consent": { "...": "the consent record, as today" },
  "scope_index": 0,
  "callback": true,
  "scope_name": "proposal-accept",
  "match": "constrained",
  "bindings": {"id": "DANI-10926", "sha256": "9f2c…"},
  "text_digests": {"execution_summary": {"sha256": "51ab…", "chars": 212}},
  "predicate": {
    "task": "DANI-10926", "status_seen": "proposed",
    "tags_matched": ["proposal"],
    "artifact": {"path": "proposal.json", "sha256": "9f2c…"}
  },
  "pinned_task": null,
  "entry_execution_id": "exe_…"
}
```

- `bindings` holds identity and digest values only. Text fields are recorded as digest
  and length, not content, because the task's own history already holds the comment or
  summary.
- `entry_execution_id` links each callback row to its entry row, so
  `orbit audit show <entry>` can list the callbacks one approval made.
- A refusal from a constrained scope adds `refused: {scope_name, field, constraint}` and
  no input values. Lifecycle events (`consent`, `rotate`, `revoke`) are unchanged, but
  their consent body now shows the constraints.
- Task history attribution is unchanged: the writer is `application:<id>`, and the
  provenance is `application consent <id> by <operator> (<provenance>)`.

`docs/design/auditability/specs/event-schema.md` and the installed `applications.md`
reference were updated in slice 4 (ORB-21041).

## 8. Migration and compatibility

**Existing v1 consents keep working unchanged.** Exact matching stays a permanent form,
not a deprecated one. The v1 reader and v1 admission semantics (key and status lists)
apply to every `input`-form scope.

**Writer and window.** The 30-day, release-gated window follows
[state-compatibility](../state-compatibility/4_decisions.md#compatibility-paths-expire-on-a-30-day-release-gated-window)
([ORB-15732]).

1. A request whose scopes are all exact, unnamed and unlimited is still written as
   `schema_version: 1`. A host rolled back to 0.41.x keeps honoring it.
2. A request with any constrained, named or rate-limited scope is written as
   `schema_version: 2`. An older binary refuses it at admission (`application_authority.rs:360`). It fails closed, with the
   "revoked, expired or replaced" message. Hosts upgrade in lockstep (polaris RB-15), so
   only a rollback sees this.
3. After the window (slice 7), the writer emits `2` for every new consent. The v1
   *record* reader stays until a read-only scan of every host finds no live v1 record
   (state-compatibility condition 4). Each v1 record is then removed by expiry,
   revocation or `orbit application rotate`.

**No automatic rewrite.** A migration must never rewrite a consent from its SQLite row,
because the witness is what stops a modified row from creating authority. Upgrading a
v1 consent means operator rotation, which is a fresh review.

**Running plugin backends.** Slice 3 moved the callback session schema from 4 to 5
(`SESSION_SCHEMA_VERSION`, `crates/orbit-tools/src/plugin/callback/session.rs`). The
reader accepts schema 4 records too, including app-bound ones, by defaulting the new
invocation fields (`record_schema_is_readable`,
`crates/orbit-tools/src/plugin/callback/storage.rs`). The session byte limit remains
8 KiB (`MAX_SESSION_BYTES`); the proposed increase to 16 KiB has not shipped. This keeps
readable long-lived backend records working across the upgrade. A schema 4 record reads
with `pinned_task` defaulted; any bindings a slice 2 host wrote into it are host-written and read as they
are. The MCP backend key already serializes the invocation (`mcp.rs:167-185`), so
constrained invocations get one backend per bound entry, never a shared one; slice 3
changes no key. The pin is per host route, so it never crosses backends.

**Audit rows.** The new fields are additive and serde-defaulted. Old rows read as
`match: exact` with no bindings.

**Consumers.** orbit-office (ORB-20363) is the only application consumer. It moves to a v2
profile in slice 6. Until then its exact consents, if any, behave as today.

## 9. Implementation slices

The original delivery sequence is retained below. Slices 1-5 shipped as [ORB-21038],
[ORB-21039], [ORB-21040], [ORB-21041] and [ORB-21042], respectively. Slice 6 remains [ORB-20363]
(`someday`), and slice 7 is [ORB-21043] (`someday`, compatibility window outstanding).
Slices 1-5 and 7 belong to `ws_orbit`, slice 6 to `ws_orbit-office`. Statements about a
slice's future work below describe its original deliverable, not a current admission
restriction.

1. **Application consent v2 schema and consent-time validation.** Add the constraint
   types, scope names, the `fields` / `input` exclusivity, the ceiling table
   (`APPLICATION_OPERATION_CEILINGS`, which replaced `APPLICATION_BUILTIN_OPERATIONS`),
   the always-refused list, the 8 KiB per-operation
   budget, the expiry cap (30 days, fixed since [ORB-22808]) and the
   optional scope rate limit. Version selection by content (v1 when exact-only).
   Admission refuses constrained and rate-limited scopes (`not yet admitted`) until
   slice 2. Tests: consent refusals for each
   invalid constraint, ceiling breach and always-refused tool, at the `orbit application
   consent` CLI boundary.
2. **Constrained entry admission.** Add the evaluator (§5.1), scope rate-limit enforcement, `ApplicationPort` reads for
   the task and artifact digest, invocation `bindings` and `text_digests`, the field-level
   refusal, executor support for `plan`, `execution_summary` and `note`, and the
   `transition` and `backlog` rules. Tests: dismiss and send-back admitted on matching
   state and refused on changed state, wrong tag, `proposed` to `backlog`, and after
   revocation, through `orbit tool run` with a descriptor.
3. **Bound callbacks and read callbacks.** Add `bound` and `pin`, host routing for every
   consented callback tool, the `orbit.task.show` and `orbit.task.artifact` action `get` executors
   with digest recheck, callback session schema 5 with the schema 4 reader, and backend
   keys. Tests: a fixture plugin's bound accept succeeds end to end. A second task id, a
   changed artifact and an unlisted callback are refused.
4. **Audit and documentation.** Add `scope_name`, `match`, `predicate`,
   `entry_execution_id` and refusal fields to the audit object. Update the
   `orbit audit show` rendering, `docs/design/auditability/specs/event-schema.md`, and
   the installed `orbit-setup` `applications.md` reference (then
   `scripts/sync-plugin-skills.sh`).
5. **Payload-bound auto-task callbacks.** Add the `artifact_json` constraint, and the
   `orbit.auto_task` action `add` and `orbit.auto_task` action `update` ceilings and executors. Tests: an
   add whose input differs from the artifact pointer is refused.
6. **orbit-office: approval profile** (`ws_orbit-office`; re-scopes ORB-20363). Add the
   full consent profile, installer consent step and credential descriptor wiring, the
   approve and gateway close walks without `proposed` to `backlog`, the auto-task
   proposal payload carrying the exact `orbit.auto_task` action `add` input, and not-permitted
   messages for routine control and mint.
7. **Retire v1-only writing** (after the 30-day window). The writer emits v2 for every
   consent. Remove the v1 record reader only after a scan of every host finds no live v1
   record.

## 10. Concerns & Honest Limitations

- **The approval is the relayer's.** Orbit binds a click to current state but cannot
  tell a real click from the app server or a credential thief making the same call. That
  is the cost of keeping approvals in the app. A host that needs click authentication
  needs an independent channel, such as the decision channel's signed reply (see
  [human-decision-channel](../human-decision-channel/2_design.md)), and that would be a
  separate design.
- **Predicate reads are not transactional with the write.** Status races are closed by
  lifecycle preconditions on the write. Payload races are closed by the digest recheck
  on read. A tag removed between admission and write is not rechecked. Tags only select.
- **Constraint evaluation is new security code.** Every constraint kind needs a refusal
  test at the CLI boundary. A logic slip admits more than the operator reviewed. The
  closed constraint set, the ceilings and the always-refused list limit how much a slip
  can admit.
- **Consent review gets harder.** A constrained scope is harder for an operator to read
  than an exact call. `orbit application consent` now prints a plain summary of each
  constrained scope (slice 4), for example: "accept any task tagged proposal in status
  proposed whose proposal.json digest equals the call's sha256".
- **The expiry cap adds operator work.** Monthly rotation at the 30-day cap is a real
  cost on a one-user host. The cap was configurable (resolved question 1) until
  [ORB-22808] fixed it at 30 days.
- **Office close walks change.** The approve and gateway paths must stop passing through
  `backlog` from `proposed`. Until slice 6 lands, those closes fail with the specific
  refusal. The send has happened or the promote stands, and the app already reports
  `closed: false`.

## Task References

- [ORB-15732] — recorded the 30-day, release-gated compatibility window.
- [ORB-20362] — shipped scoped, audited application authority (orbit 0.41.0).
- [ORB-20363] — wires the office app's approval actions to application authority;
  office profile integration remains outstanding (slice 6).
- [ORB-20852] — this design.
- [ORB-21038] — shipped Orbit slice 1 (schema and consent-time validation).
- [ORB-21039] — shipped Orbit slice 2 (constrained entry admission).
- [ORB-21040] — shipped Orbit slice 3 (bound and read callbacks).
- [ORB-21041] — shipped Orbit slice 4 (audit fields and documentation).
- [ORB-21042] — shipped Orbit slice 5 (payload-bound auto-task callbacks).
- [ORB-21043] — compatibility retirement (slice 7).
- [ORB-22808] — fixed the constrained-consent expiry cap at 30 days and ignores `machine.application_consent_max_days`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
