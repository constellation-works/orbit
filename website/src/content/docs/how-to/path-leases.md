---
title: Hold paths with leases
description: "Announce or reserve workspace paths you are editing outside a drain, with the CLI, MCP tools, or dashboard HTTP API: request, check, renew and release a path lease before its TTL expires."
---

A path lease records that a person or an agent outside a drain is editing
some paths in a workspace, so drains and other holders can see that work and
stay off it. Use a lease when you edit a checkout by hand, or from an agent
session Orbit did not start, and you want that editing to be visible — or, with
a hard lease, to block overlapping work until you finish.

A lease is held by a named principal (a person or an agent), never by a task.
That is the difference from a task reservation: a reservation belongs to a
task's own delivery and ends with it, while a lease is yours, is renewed by
you, and ends when you release it or its time-to-live (TTL) lapses.

Leases are available from the CLI (`orbit lease request`, `renew`,
`release`, `show`, `list` and `check`), from the matching `orbit.lease.*`
MCP tools, and from the dashboard HTTP API. The examples below use the HTTP
API against a running server — drive them with `curl` or any HTTP client —
while the same six operations are available through `orbit lease …` and MCP.

## Before you start

One prerequisite applies to every lease operation; two more apply only to the dashboard HTTP examples below:

1. **Leases are opt-in per workspace.** Set `lease.enabled = true` in the
   workspace's `.orbit/config.toml`:

   ```toml
   [lease]
   enabled = true
   ```

   While it is `false` (the default), every lease operation — request, renew,
   release, show, list and check — is refused with a `400` error naming
   `lease.enabled`. See [Configuration](../../reference/config/) for the six
   `lease.*` keys.

2. **Writes over HTTP need an operator session.** Requesting, renewing,
   releasing and expanding a lease over the dashboard HTTP API change which
   paths other callers may hold, so the dashboard server must be running as
   an operator session (for example `orbit web serve --operator`, which
   `orbit web connect` passes by default). Without one, an HTTP write is
   refused with `403` and `"code": "authorization_denied"`. List, show and
   check over HTTP are reads and do not need an operator session.

3. **Stay on the loopback origin.** The dashboard HTTP API accepts only a loopback `Host`
   (`localhost`, `127.0.0.1` or `[::1]`). An unsafe method (`POST`, `DELETE`)
   must also send an `Origin` that matches that host, and a `Sec-Fetch-Site`
   header, when present, must say `same-origin` or `none`. A mismatch is
   refused with `403` and `"error": "cross-origin requests not allowed"`.
   `curl` sends no `Origin` by default, so add one to writes, as the examples
   do.

For the HTTP calls below, select the workspace with `?workspace=<id>` on
every call, or rely on the server's default workspace. With neither, the
call is refused with a `400` asking you to pass `?workspace=<id>`. The
examples use the conventional loopback address `http://127.0.0.1:7878`;
substitute your server's port.

## Advisory and hard leases

| Mode | Granted | Blocks |
|---|---|---|
| `advisory` | Always | Nothing. The answer names the holders it overlaps. |
| `hard` | Only when its surface overlaps nothing else held | Overlapping hard requests are refused while it is active. |

A hard request uses `workflow.unlocked_context_paths` to determine its stored
surface. If every selector matches, the request keeps all selectors. Only a
mixed request — one with both matching and non-matching selectors — drops the
matching selectors. The request is then granted only when its stored surface
overlaps no context lock of an `in-progress` or `review` task, no task
reservation and no other hard lease. There is no waiting queue: an overlapping
hard request is refused at once (see [Refusals](#refusals)).

For example, with `workflow.unlocked_context_paths = ["docs/**"]`, a docs-only
hard request for `["file:docs/guide.md"]` keeps that selector in its stored
surface and can be refused if another holder locks that guide. A mixed hard
request for `["file:docs/guide.md", "file:src/lib.rs"]` drops the guide and
keeps only `file:src/lib.rs`; a holder of the guide alone does not conflict
with that request.

If you omit `mode`, the request uses `lease.default_mode` (`advisory` unless
configured otherwise). If your principal class is configured with
`lease.hard.<class>` set to `never`, a hard request is granted as advisory
instead: the answer carries `"downgraded": true` and a `notice` saying so.

## Principal, priority and TTL

- **`principal`** (required): the person or agent name the lease is held by,
  for example `"dana"` or `"dana-laptop-agent"`. Send it in the JSON body or
  as `?principal=`; if you send both, they must match or the call is refused
  with a `400`. The name is attribution — it says who holds the paths.
- **Principal class** is decided by the server process, not by the name you
  send: `agent` when the calling process declares an agent or a managed run,
  otherwise `human`. The class selects which `lease.hard.*` and
  `lease.max_priority.*` settings apply to the request. Treat the class and
  its ceilings as a guard, not a security boundary.
- **`priority`**: `low`, `normal` or `high`. A request that names none gets
  `normal`, or the class ceiling when that is lower. Asking above the ceiling
  (`lease.max_priority.human` defaults to `high`,
  `lease.max_priority.agent` to `normal`) is refused with a `400` naming the
  ceiling.
- **`ttl_seconds`**: the lease's lifetime, from `1` to `14400` seconds,
  defaulting to `1800` (30 minutes). The lease ends on release or when the
  TTL lapses, whichever comes first. Renew it to move the expiry forward.
- **`selectors`** (required): one or more canonical `file:` or `dir:`
  selectors inside the workspace, for example `file:src/api.ts` or
  `dir:website/src`. Other selector kinds are refused.

Two callers who send the same `principal` are the same holder as far as the
answer is concerned. Pick a name that identifies the person or session doing
the editing.

## Request a lease

```bash
curl -s -X POST 'http://127.0.0.1:7878/api/leases?workspace=my-workspace' \
  -H 'Content-Type: application/json' \
  -H 'Origin: http://127.0.0.1:7878' \
  -d '{
    "principal": "dana",
    "selectors": ["dir:website/src"],
    "mode": "hard",
    "ttl_seconds": 3600
  }'
```

A granted lease answers `200` with the lease record:

```json
{
  "granted": true,
  "requested_mode": "hard",
  "downgraded": false,
  "unlocked_selectors": [],
  "overlaps": [],
  "lease": {
    "lease_id": "lease-…",
    "selectors": ["dir:website/src"],
    "mode": "hard",
    "priority": "normal",
    "principal": "dana",
    "principal_class": "human",
    "expires_at": "2026-10-26T07:00:00Z",
    "state": "granted"
  }
}
```

Keep the `lease_id`: renew, show and release address the lease by it. An
advisory grant also fills `overlaps` with the holders it overlaps, tasks
first, then reservations, then leases. Each entry names the overlapped
`file`, the `held_by` kind (`task`, `reservation` or `lease`), the
`held_by_id`, and — for a lease — its `mode` and `principal`.

## List and show leases

```bash
# Every active lease in the workspace, oldest first
curl -s 'http://127.0.0.1:7878/api/leases?workspace=my-workspace'

# One lease, in any state
curl -s 'http://127.0.0.1:7878/api/leases/lease-…?workspace=my-workspace'
```

List answers `{"leases": […]}` with advisory and hard leases together. Show
answers `{"lease": {…}}`; a lease past its TTL reads `"state": "expired"`
rather than `granted`, and an unknown ID is refused with a `400` naming the
workspace. A `lease_id` that does not start with `lease-` is refused as
malformed.

## Check paths before you edit

Check is a read: it expires, releases and audits nothing.

```bash
curl -s -X POST 'http://127.0.0.1:7878/api/leases/check?workspace=my-workspace' \
  -H 'Content-Type: application/json' \
  -H 'Origin: http://127.0.0.1:7878' \
  -d '{
    "paths": ["file:website/src/app.ts"],
    "lease_id": "lease-…"
  }'
```

For each path the answer reports `covered` (your `lease_id` holds it),
`unlocked` (it matches `workflow.unlocked_context_paths`), and the other
`holders` overlapping it — task context locks, task reservations and leases.
The top level echoes your `lease_id` and whether it is still active
(`lease_active`). Omit `lease_id` to ask only who else holds the paths.

## Renew a lease

Renewal moves `expires_at` to `ttl_seconds` from now (same bounds as a
request; omit it for the 1800-second default):

```bash
curl -s -X POST 'http://127.0.0.1:7878/api/leases/lease-…/renew?workspace=my-workspace' \
  -H 'Content-Type: application/json' \
  -H 'Origin: http://127.0.0.1:7878' \
  -d '{"ttl_seconds": 3600}'
```

A successful renewal answers `{"renewed": true, "lease": {…}}`. Renewing a
lease that is unknown, released or expired is refused with a `400` telling
you to request a new lease.

## Release a lease

```bash
curl -s -X DELETE 'http://127.0.0.1:7878/api/leases/lease-…?workspace=my-workspace' \
  -H 'Origin: http://127.0.0.1:7878'
```

Release answers `{"released": true, "lease": {…}}` while the lease is
active. Releasing a lease that has already ended answers
`{"released": false, "lease": null}` — that is a normal answer, not an
error. If you do nothing, the lease ends on its own when the TTL lapses.

## Refusals

Lease operations refuse in two distinct ways. Keep them apart when you script
against the API:

| Situation | Result |
|---|---|
| Hard request overlaps a task, reservation or hard lease | `200` with `{"granted": false, "conflicts": […]}` — a structured refusal, not an HTTP error. Each conflict names the `file`, the `held_by` kind and the `held_by_id`. Nothing is recorded as granted; release or wait for the holder, narrow your selectors, or request `advisory` instead. |
| Leases disabled (`lease.enabled = false`) | `400` with an `error` naming `lease.enabled`. |
| Write without an operator session | `403` with `"code": "authorization_denied"` and `"operation": "lease.write"`. |
| Cross-origin or non-loopback call | `403` with `"error": "cross-origin requests not allowed"`. |
| Priority above your class ceiling | `400` naming the priority, the class and its ceiling. |
| No workspace selected and no server default | `400` asking for `?workspace=<id>`. |
| Missing or empty `principal`, empty or unsupported selectors, `ttl_seconds` outside 1–14400, mismatched query/body principal | `400` with an `error` naming the field. |
| Renew of an unknown, released or expired lease | `400` telling you to request a new lease. |
| Show of an unknown lease | `400` naming the lease and workspace. |

## Expansion is not available

`POST /api/leases/{id}/expand` is mounted but not implemented: it answers
`501` with `"code": "not_available_in_this_phase"` and
`"error": "lease expansion is not available in this phase"`. To cover more
paths, release the lease and request a new one with the wider selectors, or
take a second lease.

## Related

- [Configuration](../../reference/config/) lists the six `lease.*` keys,
  their defaults and the per-class ceilings.
- [Use the dashboard](../dashboard/) covers starting the dashboard server
  these calls are made against.
