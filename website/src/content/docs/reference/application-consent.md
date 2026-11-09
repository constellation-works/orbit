---
title: Application consent
description: "Let a Unix service or CLI child call Orbit in one workspace under an operator's consent, with constrained calls, a private credential, rotation and revocation."
---

Application consent lets a Unix service or CLI child make Orbit calls that an operator has approved, without that child receiving operator or runner authority. The operator approves a consent request. Orbit writes one new credential file for that consent. The child presents the credential, and Orbit admits only the calls the consent covers.

## When to use scoped applications

Use application consent when a long-running service must make a fixed set of Orbit calls after you approve them. Examples include a notifier that marks a reviewed task done, or a service that comments on a task after it applies an approved change.

Do not use it to widen what an agent or a person can do. Application consent is separate from:

- `agent_callable` in a plugin manifest. Requesting it does not grant application authority.
- MCP roles, toolsets and tool lists. They limit what MCP lists and dispatches. They grant no authority.
- Operator and runner capabilities. A consent never elevates a session. A session that carries Agent, Operator or Runner capabilities, or an agent context, cannot use an application credential.

Application consent covers Unix services and CLI children on the host where the consent was recorded. Application MCP sessions and Windows credential provisioning are not supported. Orbit refuses them rather than falling back to another path. Consent is an accident guard against processes running as your OS user, not a sandbox. Keep the agent and plugin sandboxes in place.

## Consent request

The operator writes a JSON request with these top-level fields:

| Field | Rule |
|---|---|
| `application_id` | 1 to 128 characters from ASCII letters, digits, `.`, `_` and `-`. This is the operator's label for the service. |
| `expires_at` | An RFC 3339 time in the future. |
| `scopes` | 1 to 64 scopes. |

Each scope has one `operation` and may also have `callbacks`, a `name` and a `rate_limit`. An operation names a `tool` and takes exactly one of these:

- `input`: an exact JSON object. Orbit compares it structurally. A missing key, an added key or a changed value is a different call.
- `fields`: a closed map from field name to a typed constraint. Any key the call sends that the map does not list is refused.

Use an exact scope when one fixed call is enough. Use a named, constrained scope when the service needs a family of calls that depend on current task state.

### Exact request

This example is illustrative. Replace `<task id>` with the task you approve, and choose your own expiry.

```json
{
  "application_id": "release-sync",
  "expires_at": "2026-11-16T00:00:00Z",
  "scopes": [
    {
      "operation": {
        "tool": "orbit.task.update",
        "input": {"id": "<task id>", "status": "done", "comment": "Released"},
        "plugin": null
      }
    }
  ]
}
```

### Named, constrained scope with a rate limit

This example is illustrative. It admits a status change to `done` only for a task tagged `release` that is currently in `review`, with a comment of up to 500 characters. It admits at most 20 calls in any trailing hour.

```json
{
  "application_id": "review-notifier",
  "expires_at": "2026-11-30T00:00:00Z",
  "scopes": [
    {
      "name": "approve-review",
      "operation": {
        "tool": "orbit.task.update",
        "fields": {
          "id": {"task": {"tags_all": ["release"], "status": ["review"]}},
          "status": {"const": "done"},
          "comment": {"text": {"max_chars": 500}}
        },
        "plugin": null
      },
      "rate_limit": {"admissions": 20, "window_seconds": 3600}
    }
  ]
}
```

### Plugin entry with a bound callback

A plugin entry names the plugin and the installed manifest digest it was reviewed against. The fields of a plugin operation must be declared input parameters of that plugin tool. This example is illustrative: `example` and `example.apply` are placeholders for your plugin and tool.

```json
{
  "application_id": "approval-service",
  "expires_at": "2026-11-16T00:00:00Z",
  "scopes": [
    {
      "name": "apply-approved-proposal",
      "operation": {
        "tool": "example.apply",
        "fields": {
          "proposal": {"one_of": ["proposal-42"]},
          "task_id": {"task": {"tags_all": ["release"], "status": ["review"]}}
        },
        "plugin": {
          "name": "example",
          "manifest_digest": "<installed manifest digest>"
        }
      },
      "callbacks": [
        {
          "tool": "orbit.task.update",
          "fields": {
            "id": {"bound": "task_id"},
            "status": {"const": "done"}
          },
          "plugin": null
        }
      ],
      "rate_limit": {"admissions": 5, "window_seconds": 3600}
    }
  ]
}
```

Get the installed manifest digest with `orbit plugin show <name> --format json` before you write the request. Plugin tools need `plugin`. Built-in tools must not have it.

## Constraint kinds

A constrained field takes one of these forms. Each form is a JSON object with one key that names the kind.

| Kind | Form | Limits |
|---|---|---|
| `const` | `{"const": value}` | The call's value must equal the constant structurally. |
| `one_of` | `{"one_of": [values]}` | 1 to 16 values. |
| `text` | `{"text": {"max_chars": n}}` | Free text of at most `n` characters, 1 to 4096. Built-in tools accept it only on `comment`, `note`, `plan` and `execution_summary`. |
| `token` | `{"token": {"max_len": n}}` | An ASCII token over `[A-Za-z0-9._:-]`, up to 128 characters. Plugin tools, and built-in tools on non-identity fields. |
| `sha256` | `{"sha256": {}}` | 64 lowercase hex characters. |
| `workspace` | `{"workspace": {}}` | Resolves to the consent's workspace. Valid only on the field `workspace`. |
| `task` | `{"task": {...}}` | The task named by the field must match the predicate on its current tags and status. It can also bind an artifact digest. |
| `transition` | `{"transition": {"from": [...], "to": "..."}}` | Constrains only the `status` of `orbit.task.update`. The call must also have an `id` field. The task's current status must be in `from`. |
| `bound` | `{"bound": "field"}` | Callbacks only. Admits the value the entry admitted for the named field. |
| `artifact_json` | `{"artifact_json": {"path": "...", "pointer": "..."}}` | Callbacks only. Reads an RFC 6901 pointer inside a task artifact that the entry's `task` predicate bound by digest. |
| `subset_of` | `{"subset_of": [...]}` | 1 to 32 distinct, non-empty strings. |
| `optional` | `{"optional": constraint}` | The field may be absent. It cannot wrap another `optional`. |

These rules apply to every constrained operation:

- A `fields` map lists at most 32 fields. Each field name is 1 to 64 characters from `[A-Za-z0-9_-]`.
- The fields `id`, `name`, `path` and `draft` name objects, so they must bind to state. Allowed forms are `const`, `one_of`, `task`, `bound` and `artifact_json`. `token` is also allowed, for plugin tools only.
- A `task` predicate needs `tags_all` or `tags_any`. Tags are 1 to 64 characters. It takes 1 to 16 statuses from `proposed`, `backlog`, `in-progress`, `review`, `done`, `blocked`, `archived`, `rejected` and `someday`.
- A `task` predicate with `pin: true` is allowed only on a callback's `id`.
- A constrained scope needs a `name`. Names match `[a-z0-9-]{1,48}` and are unique within the consent. Exact scopes may omit the name.
- Each serialized operation, including each callback, is limited to 8 KiB.
- A constrained operation whose `status` is a `const` or `one_of` may name only `done`, `rejected` or `archived`. Other targets need a `transition`.

## Built-in operations

Consent can name only the built-in tools in this table. Any other built-in tool is refused when you consent. Inputs that need operator authority are refused too, whichever tool they name.

| Tool | Exact `input` | Constrained `fields` | Notes |
|---|---|---|---|
| `orbit.task.update` | `id`, `status`, `comment`, `workspace` | `id`, `status`, `comment`, `note`, `plan`, `execution_summary`, `workspace` | Exact `status` may be `backlog`, `done`, `rejected` or `archived`. A constrained `status` is a `const`, a `one_of` of `done`, `rejected` or `archived`, or a `transition`. A `transition` may target `in-progress`, `review`, `done`, `rejected`, `archived` or `backlog`. A move to `backlog` is allowed only from `review` or `in-progress`. |
| `orbit.decision.answer` | `id`, `answer`, `note`, `workspace` | None | Exact calls only. The existing decision handler checks the request, the allowed answers, expiry and supersession. |
| `orbit.task.show` | None | `id`, `fields`, `workspace` | Callbacks only. `fields` must be a `subset_of` the readable fields: `id`, `status`, `tags`, `terminal`, `artifacts`, `crew`, `updated_at` and `execution_summary`. |
| `orbit.task.artifact` action `get` | None | `id`, `path`, `workspace` | Callbacks only. `path` must be a non-empty `const` string. |
| `orbit.auto_task` action `add` | None | `name`, `schedule`, `template`, `description`, `dedupe`, `workspace` | Callbacks only. `name`, `schedule` and `template` are required. Every definition field must be an `artifact_json` bound to the approved artifact, so a callback cannot take a caller-supplied value. |
| `orbit.auto_task` action `update` | None | `name`, `enabled`, `workspace` | Callbacks only. `name` is required and must be an `artifact_json`. `enabled`, if present, is a boolean `const` or a `one_of` of booleans. |

The source-status restriction applies only to constrained `transition` scopes: a transition to `backlog` is allowed only from `review` or `in-progress`. An exact scope may name `status: "backlog"` as listed in the table, without this transition-source restriction.

## Consent, invocation and callbacks

### Record the consent

Run the consent command as the host operator, from the workspace checkout you mean to bind:

```sh
orbit --workspace <workspace> application consent \
  --input-file consent.json --credential-file /private/service/credential \
  --format json
```

Orbit validates the request and binds the consent to this host's Orbit store, the workspace id and the physical checkout root. It returns the non-secret consent record. It writes the credential once, to a new file that must not already exist. The file is created with mode `0600`. Orbit never prints the credential. Place the file in an owner-only service directory, outside repositories and outside any directory that agents or plugins can read.

With constrained scopes, the text output adds one plain line per scope. The lines are illustrative, and field lists are sorted by name:

```text
Application consent issued: <consent id>
approve-review: admit orbit.task.update when comment is text up to 500 characters, id is a task tagged release in status review, status is "done"; expires 2026-11-30T00:00:00Z; rate limit 20 admissions per 3600 seconds
```

Consent is reusable for the calls it covers until it expires or is revoked. It does not make each call idempotent, and it does not prove a fresh human decision for each call. Use short expiries and idempotency keys in the service for operations such as sending a message.

### Invoke with a credential descriptor

The service opens the credential file and passes its descriptor to the CLI child. The credential is never in argv, in an environment value, or in logs. Only the descriptor number is in the environment:

```sh
(
  exec 9</private/service/credential
  export ORBIT_APPLICATION_CREDENTIAL_FD=9
  orbit --workspace <workspace> tool run example.apply \
    --input '{"proposal":"proposal-42"}' --format json
)
```

A native service launcher maps the open file onto a child descriptor above 2 and clears close-on-exec for the CLI child. Close the service's own copy when the child exits.

Orbit reads the credential with a bounded positional read, so concurrent calls do not share a file offset. It refuses a descriptor that is not a regular file, including a pipe or an invalid descriptor. Application callers can run only workspace `tool run` calls. Host-global dispatch without a resolved workspace is refused. A plugin child never receives the application's credential. It receives its own host callback descriptor.

Orbit refuses a call when the consent is unknown, expired or revoked, when the workspace or checkout differs, when the call is not covered, or when the plugin's live manifest digest differs from the consent's. Each refusal happens before the mutation.

### Constrained admission

A call under a constrained scope is admitted only when:

- every field it sends is listed in the scope's `fields`, and
- every listed field that is not `optional` is present and passes its constraint against the task's current state.

A `task` predicate reads the task's tags, status and bound artifact digest. A `transition` reads the status of the task named by the call's `id`. A refusal names the scope, the first failing field and the constraint kind. It never echoes a value from the call.

A write then rechecks the status that admission read, so a task that changed in the meantime is not modified.

### Completion callbacks

Callbacks are separately approved operations listed under their parent scope. Approving an entry does not approve its callbacks.

- The consumer sequences completion. It applies the approved change, then requests a callback only after the change succeeded.
- Orbit does not infer success from a plugin's return value. It does not undo a change that was already committed when a callback is refused. A refused callback never falls back to an operator call.
- A `bound` field admits only the value the entry admitted. An `artifact_json` field reads an artifact whose digest the entry's `task` predicate bound. Each callback rechecks that digest, so a payload changed after admission is refused.
- Each callback rechecks expiry, revocation and the plugin's live digest.
- A plugin callback must be requested in the plugin's `permissions.orbit_tools` and granted by the host. The plugin's grants, the built-in ceiling and the consent all apply. None replaces another.

### Plugin digest changes

A plugin entry records the installed `manifest_digest` that the operator reviewed. If the plugin's manifest changes, its digest changes. Calls and callbacks under the old consent are then refused. To resume, run `orbit plugin show <name> --format json`, review the new digest, and rotate the consent to a request that names it.

## Rate limits and expiry

A scope may set `rate_limit`, with `admissions` from 1 to 10000 and `window_seconds` from 1 to 86400. It counts admitted entry calls in any trailing window. A call over the limit is refused and audited. A scope with no `rate_limit` has no limit.

Expiry depends on the record schema. Orbit writes a consent whose every scope is exact, unnamed and unlimited as schema v1. A consent with any named, constrained or rate-limited scope is schema v2, and an older binary that reads only v1 refuses it at admission. The request-size limits follow the same split:

- A schema v1 request is limited to 4 KiB.
- A schema v2 request is limited by the 8 KiB per-operation budget. It is not limited to 4 KiB as a whole.

A schema v2 consent may expire no later than 30 days from the time you consent. The cap is fixed. Consent with only exact scopes is not capped by it.

## Rotate, revoke and audit

Rotation requires a new review. It revokes the old consent and all of its callbacks, and it mints a new credential:

```sh
orbit --workspace <workspace> application rotate <consent-id> \
  --input-file replacement-consent.json --credential-file /private/service/new-credential
```

Switch the service to the new file only after provisioning succeeds. If provisioning fails, no usable new credential exists, and the previous consent may already be revoked. Recover by issuing a new consent.

Revoke stops the consent and all of its callbacks:

```sh
orbit --workspace <workspace> application revoke <consent-id>
```

Revocation and rotation do not roll back an operation that was already admitted. Each later call or callback checks the host state again, including in a long-lived backend. Remove retired credential files through your normal secret-management process.

Audit records show what was admitted. Use:

```sh
orbit audit list --tool orbit.task.update --format json
orbit audit show <audit id>
```

An invocation or refusal row carries an `application` object. It records the application id, the consent id, the workspace and checkout, the operator who consented, the consent time and expiry, the scope name and index, whether the call matched exactly or by constraints, and whether it was a callback. A constrained admission also records the bound identity and digest values, a sha256 and length for each free-text field, and the task state admission read. A constrained refusal records the scope name, the field and the constraint kind. It never records a value from the call. Credential values never appear in an audit row.

## Lifecycle at a glance

1. Install and enable the plugin through the normal operator flow, including its requested grants. Check the installed digest with `orbit plugin show <name> --format json`.
2. Write the consent request. Run `orbit application consent` as the host operator, and store the new credential file in an owner-only directory.
3. Start the service with the credential descriptor. Each call and callback is checked against the consent.
4. Rotate with `orbit application rotate` when the credential must change, or when the plugin's digest changes.
5. Revoke with `orbit application revoke` to stop the consent. Then retire the credential file.
6. Review calls in the audit log.

## Related pages

- [CLI commands](../cli/) lists the global options that `--workspace` and `--format` accept.
- [Plugin manifest](../plugin-manifest/) describes `permissions.orbit_tools` and the manifest a plugin ships.
- [Install and grant plugins](../../how-to/plugins/) covers installing a plugin and granting its permissions.
