---
title: Application consent
description: "Let a Unix service or CLI child make only the Orbit calls an operator approved in one workspace, with a private credential, rotation and revocation."
---

Application consent lets a Unix service or CLI child make the Orbit calls you
approved, and no others, without operator or runner authority. You approve a
consent request, Orbit writes one credential file, and the service presents it
on each call. Use it for a long-running service with a fixed set of calls, such
as a notifier that marks a reviewed task done.

:::caution
Consent guards against accidents by processes running as your OS user. It is
not a sandbox: keep the agent and plugin sandboxes in place.
:::

## Approve one call

Write the request. This one approves a single exact call; replace `<task id>`
and choose your own expiry:

```json
{
  "application_id": "release-sync",
  "expires_at": "<future RFC 3339 timestamp>",
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

Record it as the host operator, from the workspace checkout you mean to bind:

```bash
orbit --workspace <workspace> application consent \
  --input-file consent.json --credential-file /private/service/credential
```

Orbit binds the consent to this host's Orbit store, the workspace and the
physical checkout root, and prints the consent record with its consent ID. It
writes the credential once, with mode `0600`, to a file that must not already
exist, and never prints it. Keep that file in an owner-only directory, outside
repositories and anywhere agents or plugins can read.

The service opens the file and hands the descriptor to the CLI child. Only the
descriptor number goes in the environment; the credential never appears in
argv, an environment value or a log:

```bash
(
  exec 9</private/service/credential
  export ORBIT_APPLICATION_CREDENTIAL_FD=9
  orbit --workspace <workspace> tool run orbit.task.update \
    --input '{"id":"<task id>","status":"done","comment":"Released"}' --format json
)
```

A native launcher maps the open file onto a child descriptor above 2, clears
close-on-exec for the child, and closes its own copy when the child exits. The
descriptor must be a regular file, not a pipe.

## What consent does not do

- It never elevates a session. A session with agent, operator or runner
  capability, or an agent context, cannot use an application credential.
- It is separate from `agent_callable` in a plugin manifest and from MCP roles,
  toolsets and tool lists. None of those grants application authority.
- It covers workspace `tool run` calls on the host where it was recorded.
  Host-global dispatch, application MCP sessions and Windows credential
  provisioning are refused.
- It does not make a call idempotent or prove a fresh human decision. Consent
  is reusable until it expires or is revoked, so use short expiries and
  idempotency keys in the service.

## Consent request

| Field | Rule |
|---|---|
| `application_id` | Your label for the service: 1 to 128 ASCII letters, digits, `.`, `_` or `-`. |
| `expires_at` | An RFC 3339 time in the future. |
| `scopes` | 1 to 64 scopes. |

| Scope field | Rule |
|---|---|
| `operation` | Required. The call this scope admits. |
| `name` | `[a-z0-9-]{1,48}`, unique within the consent. Required for a constrained scope. |
| `callbacks` | Operations a plugin entry may request after it completes. See [Completion callbacks](#completion-callbacks). |
| `rate_limit` | `admissions` (1 to 10000) per `window_seconds` (1 to 86400), counted over any trailing window. A call over the limit is refused and audited. Without it there is no limit. |

An operation names a `tool` and takes exactly one of:

- `input`: an exact JSON object, compared structurally. A missing key, an added
  key or a changed value is a different call.
- `fields`: a closed map from field name to a [constraint](#constraint-kinds).
  A call that sends an unlisted key is refused.

A plugin tool also needs `plugin`, with the plugin's `name` and the installed
`manifest_digest` you reviewed. Read the digest from
`orbit plugin show <name> --format json`. A built-in tool must not have one.

### Constrained scope with a rate limit

This scope admits a move to `done` only for a task tagged `release` that is in
`review`, with a comment of up to 500 characters, at most 20 times in any hour.
Replace the expiry with a future RFC 3339 timestamp within 30 days:

```json
{
  "application_id": "review-notifier",
  "expires_at": "<future RFC 3339 timestamp>",
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

For a constrained consent, the command's text output adds one line per scope
stating what it admits. Check it against your intent.

### Plugin entry with a bound callback

`example` and `example.apply` stand for your plugin and tool. The `fields` of a
plugin operation must be declared input parameters of that tool. Replace the
expiry with a future RFC 3339 timestamp within 30 days.

```json
{
  "application_id": "approval-service",
  "expires_at": "<future RFC 3339 timestamp>",
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

## Constraint kinds

Each constraint is a JSON object with one key that names the kind.

| Kind | Form | Limits |
|---|---|---|
| `const` | `{"const": value}` | The call's value must equal the constant structurally. |
| `one_of` | `{"one_of": [values]}` | 1 to 16 values. |
| `text` | `{"text": {"max_chars": n}}` | Free text of at most `n` characters, 1 to 4096. Built-in tools accept it only on `comment`, `note`, `plan` and `execution_summary`. |
| `token` | `{"token": {"max_len": n}}` | An ASCII token over `[A-Za-z0-9._:-]`, up to 128 characters. Plugin tools, and built-in tools on non-identity fields. |
| `sha256` | `{"sha256": {}}` | 64 lowercase hex characters. |
| `workspace` | `{"workspace": {}}` | Resolves to the consent's workspace. Valid only on the field `workspace`. |
| `task` | `{"task": {"tags_all": [...], "status": [...]}}` | The task the field names must match on its current tags and status. Needs `tags_all` or `tags_any` (tags of 1 to 64 characters) and 1 to 16 statuses. |
| `transition` | `{"transition": {"from": [...], "to": "..."}}` | Only on the `status` of `orbit.task.update`, in a call that also lists `id`. The task's current status must be in `from`. |
| `bound` | `{"bound": "field"}` | Callbacks only. Admits the value the entry admitted for the named field. |
| `artifact_json` | `{"artifact_json": {"path": "...", "pointer": "..."}}` | Callbacks only. Reads an RFC 6901 pointer inside a task artifact whose digest the entry's `task` predicate bound. |
| `subset_of` | `{"subset_of": [...]}` | 1 to 32 distinct, non-empty strings. |
| `optional` | `{"optional": constraint}` | The field may be absent. It cannot wrap another `optional`. |

Rules for every constrained operation:

- `fields` lists at most 32 fields, each named with 1 to 64 characters from
  `[A-Za-z0-9_-]`. Each operation, callbacks included, serializes to at most
  8 KiB.
- `id`, `name`, `path` and `draft` name objects, so they take only `const`,
  `one_of`, `task`, `bound` or `artifact_json`, or `token` on a plugin tool.
- Statuses are `proposed`, `backlog`, `in-progress`, `review`, `done`,
  `blocked`, `archived`, `rejected` and `someday`.
- A `status` given as `const` or `one_of` may name only `done`, `rejected` or
  `archived`. Other targets need a `transition`.
- A `task` predicate on an entry can bind an artifact's digest with
  `"artifact_sha256": {"path": "...", "field": "..."}`, where `field` names a
  field of the same call constrained as `sha256`.
- A `task` predicate with `pin: true` is allowed only on a callback's `id`.

## Built-in operations

Consent can name only these built-in tools. Any other built-in, and any input
that needs operator authority, is refused when you consent.

| Tool | Exact `input` | Constrained `fields` | Notes |
|---|---|---|---|
| `orbit.task.update` | `id`, `status`, `comment`, `workspace` | `id`, `status`, `comment`, `note`, `plan`, `execution_summary`, `workspace` | Exact `status`: `backlog`, `done`, `rejected` or `archived`. A `transition` may target `in-progress`, `review`, `done`, `rejected`, `archived` or `backlog`, and reaches `backlog` only from `review` or `in-progress`. |
| `orbit.decision.answer` | `id`, `answer`, `note`, `workspace` | None | Exact calls only. The decision handler still checks the request, the allowed answers, expiry and supersession. |
| `orbit.task.show` | None | `id`, `fields`, `workspace` | Callbacks only. `fields` must be a `subset_of` `id`, `status`, `tags`, `terminal`, `artifacts`, `crew`, `updated_at` and `execution_summary`. |
| `orbit.task.artifact.get` | None | `id`, `path`, `workspace` | Callbacks only. `path` must be a non-empty `const` string. |
| `orbit.auto_task.add` | None | `name`, `schedule`, `template`, `description`, `dedupe`, `workspace` | Callbacks only. `name`, `schedule` and `template` are required. Every definition field must be an `artifact_json`, so a callback cannot supply its own value. |
| `orbit.auto_task.update` | None | `name`, `enabled`, `workspace` | Callbacks only. `name` is required and must be an `artifact_json`. `enabled` is a boolean `const` or a `one_of` of booleans. |

## How a call is admitted

Orbit refuses a call, before any mutation, when the consent is unknown, expired
or revoked, when the workspace or checkout differs, when no scope covers the
call, or when the plugin's installed manifest digest differs from the consent's.

A constrained scope admits a call only when every field the call sends is
listed, and every listed field that is not `optional` is present and passes its
constraint against the task's current state. A refusal names the scope, the
first failing field and the constraint kind, never a value from the call. A
write rechecks the status that admission read, so a task that changed in the
meantime is not modified.

### Completion callbacks

Callbacks are separately approved built-in operations listed under a plugin
entry's scope. Approving the entry does not approve them.

- Your service sequences completion: it applies the approved change, then
  requests the callback. Orbit does not infer success from a plugin's return
  value.
- A refused callback does not undo a change already committed, and never falls
  back to an operator call.
- Each callback rechecks expiry, revocation, the plugin's digest and any bound
  artifact digest, so a payload changed after admission is refused.
- The plugin must also request the callback tool in `permissions.orbit_tools`
  and have it granted. See [Plugin manifest](/reference/plugin-manifest/#permission-and-callback-boundaries).
- The plugin child never receives the application's credential.

## Expiry and size limits

| Consent | Latest expiry | Size limit |
|---|---|---|
| Every scope exact, unnamed and without a rate limit | No cap | 4 KiB for the whole request |
| Any named, constrained or rate-limited scope | 30 days from consent | 8 KiB per operation |

## Rotate, revoke and audit

Rotation needs a new review. It revokes the old consent with its callbacks and
writes a new credential file:

```bash
orbit --workspace <workspace> application rotate <consent-id> \
  --input-file replacement-consent.json --credential-file /private/service/new-credential
```

Switch the service to the new file only after the command succeeds. If it fails
partway, the old consent may already be revoked: issue a new consent.

Revoke stops the consent and its callbacks:

```bash
orbit --workspace <workspace> application revoke <consent-id>
```

Neither command rolls back an operation already admitted. Every later call and
callback checks host state again, including from a long-lived backend. Remove
retired credential files through your normal secret management.

To see what was admitted or refused:

```bash
orbit audit list --tool orbit.task.update --format json
orbit audit show <audit id>
```

Each row carries an `application` object: the application and consent IDs, the
workspace and checkout, who consented and when, the scope, the match kind
(exact or constrained) and whether it was a callback. A constrained admission
adds the bound values, a digest and length for each free-text field, and the
task state it read; a refusal adds the field and constraint kind. Credential
values and refused call values never appear.

## If the plugin's manifest changes

A changed manifest has a new digest, so calls and callbacks under the old
consent are refused. Run `orbit plugin show <name> --format json`, review the
new digest, and [rotate](#rotate-revoke-and-audit) to a request that names it.
