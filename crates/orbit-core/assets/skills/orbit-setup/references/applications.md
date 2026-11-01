# Scoped application authority

An application can act on an operator's explicit consent without receiving
Operator or Runner capability. This is a host-local API for Unix services and
CLI children. It is separate from `agent_callable`, MCP placement, task tool
allowlists and application labels. Those do not grant application authority.

Like Orbit's other same-user authorization checks, this is an accident guard,
not protection against arbitrary code running as the host's OS user. Keep the
existing agent and plugin sandboxes. Windows credential provisioning and
application MCP sessions are not supported and fail closed.

## Install and consent

Install and enable the plugin through the normal operator flow, including its
requested grants. Inspect `orbit plugin show <namespace> --format json` and
review the installed `manifest_digest`. A manifest's `agent_callable` request
is neither needed nor appropriate for an application approval tool.

The operator reviews a JSON request containing an application id, expiry and
exact operations. For example, replacing the sample digest with the actual
installed manifest digest:

```json
{
  "application_id": "approval-service",
  "expires_at": "2026-11-01T12:00:00Z",
  "scopes": [
    {
      "operation": {
        "tool": "example.apply",
        "input": {"proposal": "proposal-42"},
        "plugin": {
          "name": "example",
          "manifest_digest": "<installed manifest digest>"
        }
      },
      "callbacks": [
        {
          "tool": "orbit.task.update",
          "input": {"id": "<task id>", "comment": "Approved payload applied"},
          "plugin": null
        }
      ]
    }
  ]
}
```

From the intended registered checkout, the operator runs:

```sh
orbit --workspace <workspace> application consent \
  --input-file consent.json --credential-file /private/service/credential \
  --format json
```

This binds consent to this host, logical workspace id and physical checkout
root. It returns the non-secret consent record. The credential is written once
to a **new** `0600` file and is never printed. Place it outside repositories and
agent/plugin readable roots in an owner-only service directory. Existing files
are refused. The application id is the operator's authenticated service label;
a self-declared `ORBIT_APPLICATION_ID` or process ancestor grants nothing.

The request is limited to 4 KiB and 1–64 scopes. Tool names and the entire JSON
input must match structurally. Added keys, omitted keys and changed values are
different operations. There are no wildcards or implied defaults. Use the exact
workspace spelling sent to dispatch when an input includes `workspace` (CLI
routing may canonicalize a selector to its checkout path). Do not include
provider credentials or other secrets in consent inputs: scopes are audit data.

A scope may instead list `fields`, a map from field name to a typed constraint,
in place of `input` (never both). Such a scope needs a `name` and may carry a
`rate_limit`. Orbit records it as a version 2 consent, validates every
constraint against the built-in ceiling at consent time, refuses any operation
or input that needs host operator authority, caps each operation at 8 KiB, and
refuses an `expires_at` beyond `machine.application_consent_max_days` (global
config, default 30 days). Exact scopes keep working as described above.

A direct call (or a plugin entry) under a constrained scope is admitted when
every field it sends is listed and every listed field that is not `optional`
passes its constraint on the task's current state: a `task` predicate reads
the task's tags, status and bound artifact digest, and a `transition` reads
the status of the task the call's `id` names. The first matching scope wins.
A refusal names each constrained scope of that tool with its first failing
field and constraint kind, never a value the call sent. The write then
rechecks the status admission saw, so a task that moved in between is not
changed. A move to `backlog` is admitted only from `review` or `in-progress`.
`orbit.task.update` applies `status`, `comment`, `note`, `plan` and
`execution_summary`; `note` needs a status change. The audit row's
`application` object records the bound identity and digest values, a digest
and length for each free-text field, and the task state admission read.
A plugin entry's callbacks are admitted the same way against the scope's
listed callbacks: `bound` admits only the entry's admitted value for that
field, and a `task` predicate with `pin: true` binds the first task a callback
names for the rest of that entry call, refusing any other. When the entry's
`task` predicate bound an artifact digest, every callback rechecks it, and
`orbit.task.artifact.get` rechecks the bytes it returns, so a payload changed
after the entry was admitted is refused (`bound artifact changed`).
`orbit.task.show` returns only the fields the call names, within `id`,
`status`, `tags`, `terminal`, `artifacts`, `crew`, `updated_at` and
`execution_summary` (all of them when it names none). An application-bound
plugin sends these callbacks to the host that spawned it, because the consent
witness is hidden from the plugin sandbox.

A scope's optional `rate_limit` (`{"admissions": N, "window_seconds": S}`)
caps the calls admitted under it in any trailing window of `S` seconds; one
over the limit is refused and audited. The limit is off unless the operator
sets one: a scope without `rate_limit` has no limit.

`orbit application consent` and `orbit application rotate` print one plain
line for each constrained scope: its name, the tool, each constraint, the
consent expiry, and the rate limit when one is set. An exact scope is not
summarized that way.

Consent is reusable for those exact calls until expiry or revocation; it does
not supply idempotency or prove a fresh human interaction per invocation. Use
short expiries and consumer idempotency keys for operations such as sending a
message. Use a fresh decision request for a snapshot-bound human answer.

## Invoke through a descriptor

The login service opens its private file and passes a read-only regular-file
descriptor to each CLI child. Only its descriptor number is in the environment:

```sh
(
  exec 9</private/service/credential
  export ORBIT_APPLICATION_CREDENTIAL_FD=9
  orbit --workspace <workspace> tool run example.apply \
    --input '{"proposal":"proposal-42"}' --format json
)
```

In a native service launcher, map the open file onto a child descriptor greater
than 2 and clear close-on-exec for the CLI child. Do not put the credential in
argv, environment values, JSON session metadata or logs. Orbit reads it with a
bounded positional read, so calls do not share a changing file offset. Orbit
sets close-on-exec at ingress; plugin children receive their own host callback
descriptor, never the application's bearer credential. Close the service's
copy when the child exits.

Application CLI callers can run only workspace `tool run` calls (including a
plugin-derived spelling). Host-global dispatch without a resolved workspace,
MCP sessions, worker envelopes, and sessions carrying Agent, Operator or Runner
capabilities cannot borrow this credential. Use the workspace-bound runtime API
for a tool otherwise advertised globally. A spoofed label, malformed descriptor,
unknown credential, different workspace/checkout, unlisted call, stale plugin
digest, expired consent or revoked consent is refused before mutation.

## Built-ins and completion callbacks

The supported built-in operations are:

* `orbit.task.update`: exact `id`, optional `status` (`backlog`, `done`,
  `rejected` or `archived`), optional `comment` and optional `workspace`.
  The normal lifecycle, proposal completion and coordination ownership checks
  still apply. No force, tool grants, field edits or review artifacts are
  admitted through this application operation.
* `orbit.decision.answer`: exact `id` and `answer`, optional `note` and
  optional `workspace`. The existing decision handler checks the current
  request, allowed answers, expiry and supersession. This includes an allowed
  `dismiss` answer, which retains its gate's existing consequences.

All other built-ins are specifically **not permitted** by application consent,
including routine control, unconditional auto-task minting, dispatch, lifecycle
force and consent administration. A request naming one is refused at consent
time. Applications must use a separately supported real operator surface for
those operations; a plugin cannot manufacture that authority.

For a plugin's completion callbacks, the operator lists each exact built-in
call beside its parent operation. The plugin must also request the tool in
`permissions.orbit_tools`, and the host must grant `orbit_tools`. Existing plugin
grants, the caller ceiling and the consent are intersected; none replaces the
others. Callbacks run on the spawning host through its authenticated descriptor
route. They cannot invoke other plugins, create new callbacks, change the
workspace, or use another scope in the app's consent. Each callback rechecks
expiry, revocation, protected witness and the original plugin's live digest.
Long-lived MCP backends are separated by consent and exact parent scope.

Completion is explicitly sequenced by the consumer: apply the approved payload,
then request the approved completion callback only after it succeeded. Core
does not infer successful application from a plugin's return value, auto-close
a task, or undo an already committed payload when a callback is refused. The
consumer owns partial-failure recovery and must not report completion until the
callback succeeds. A callback refusal never falls back to a local operator call.

## Rotate, revoke and audit

```sh
orbit --workspace <workspace> application rotate <consent-id> \
  --input-file replacement-consent.json --credential-file /private/service/new-credential
orbit --workspace <workspace> application revoke <consent-id>
```

Rotation requires renewed operator review, revokes the old credential and all
callbacks, and mints a new independent credential. Switch the service to the
new file after provisioning succeeds. Failed provisioning leaves no usable new
credential and may leave the previous consent revoked; recover by issuing a new
consent. Revocation and rotation do not roll back an operation already admitted.
Every subsequent operation or callback checks host state again, including in a
long-lived backend. Remove retired credential files through the service's normal
secret-management lifecycle.

`orbit audit list --tool <tool> --format json` and `orbit audit show` expose an
`application` object on invocations and authenticated refusals. It contains the
host-issued application id, consent id, workspace/root, operator attribution,
consent provenance/time/expiry, the scopes the operator reviewed, the selected
scope name and index, whether the call matched exactly or by constraints
(`match`), and the callback flag. A constrained admission also records
`bindings` (identity and digest values only), `text_digests` (sha256 and
length, never the text) and `predicate` (the task state admission read). Each
callback row sets `entry_execution_id` to the entry row's execution id, and
`orbit audit show` of that entry lists those callbacks. A constrained refusal
adds `refused` with the scope name, the field and the constraint kind, and no
values from the call. The attempted tool remains the call's own audit context;
call arguments are not copied into the application object. Unauthenticated
labels never populate this object. MCP client attribution stays in
`self_reported_actor`; it is not an authenticated app identity. Credential
values never appear in either field. Consent, rotation and revocation also have
their own application audit events.

Authority is held in a host SQLite record plus an owner-only witness under
`state/application-consents/`. Agent and plugin sandboxes hide the witnesses;
a modified database row alone cannot create authority. Witnesses and their
directories are checked for ownership, permissions and symlink substitution.

## Rust integration API

The workspace-bound `OrbitRuntime` exposes `consent_application(request,
operator_session, replaces)` and `revoke_application(consent_id,
operator_session)`. A trusted in-process adapter supplies the existing
host-authenticated operator session; never deserialize operator capabilities
from application input. The mint returns `(ApplicationConsent,
ApplicationCredential)`; consume the latter with `provision(path)`. It has no
serialization or secret-bearing Debug implementation.

Invoke through `execute_tool_command_dispatch_with_session_context` using the
local CLI entry point and a session without capabilities. It supplies the same
credential, scope, callback, lifecycle and audit checks as `tool run`. The
application field of `ToolSessionContext` is host-owned and skipped by serde;
it is populated after credential verification, never from client metadata.
