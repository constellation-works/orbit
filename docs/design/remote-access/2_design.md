---
title: "Remote Access — Design"
owner: codex
last_updated: 2026-09-10
last_validated: 2026-09-10
status: Accepted
feature: remote-access
doc_role: design
type: design
summary: "Current Orbit Web state, registered runtime composition, and SSH local-forward lifecycle."
tags: [remote-access, orbit-web, ssh]
paths: ["crates/orbit-web/**", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-cli/src/command/web.rs", "crates/orbit-cli/src/command/operation.rs"]
related_features: [remote-access, user-interface, host-registry]
related_artifacts: []
---

# Remote Access — Design

## 1. Ownership

| Concern | Owner |
|---|---|
| HTTP listener, UI, API, workspace selection, Web runtime cache | orbit-web |
| Workspace and checkout registry state | orbit-registry |
| Registered checkout resolution and runtime construction | orbit-cmd RegisteredRuntimeFactory |
| Domain runtime and stores | orbit-core |
| serve/connect CLI parsing and runtime-free dispatch | orbit-cli |
| Web SSH local-forward lifecycle | orbit-web ssh_tunnel |

The Web tunnel is not shared with MCP. MCP remote mode is direct ssh -T stdio owned by orbit-mcp; it has no Web port, health probe, attach mode, or TCP listener.

## 2. orbit web serve

orbit web serve always builds registry-backed multi-workspace state and may run from any directory. The accepted --global flag is a compatibility no-op. A top-level --root selects which registry is served, exactly as it does for every other root-aware command: the server enumerates <root>/workspaces.json and never reads the machine-global registry. It does not register a workspace. Which workspace the dashboard opens on is a separate option, --workspace.

Startup:

1. orbit-registry loads the registry under the resolved Orbit root: <--root>/workspaces.json when the flag is given, the machine-global ~/.orbit/workspaces.json otherwise. orbit-cmd global_root_for owns that resolution for every command, Web included.
2. Local workspace checkouts become dashboard entries. Invalid paths remain visible as inactive entries and are not opened.
3. With --workspace, the workspace it names becomes the default; the selector is a registered name or ws_* ID first and a checkout path when it is path-shaped, and an unmatched value opens aggregate mode rather than falling back to cwd. Without --workspace, the workspace containing cwd becomes the default when one matches.
4. Orbit Web refuses a non-loopback bind before opening the listener.

### Request state

DashboardState stores an immutable workspace snapshot with a monotonically increasing generation and a lazy runtime cache.

At each relevant request boundary, Web reloads the registry, validates it, and pins one complete snapshot. Workspace metadata, default selection, runtime resolution, and aggregate results for that request all use the same generation.

A successful refresh atomically swaps the snapshot and evicts cached runtimes whose workspace was removed, became inactive, or changed binding. A malformed refresh logs a content-safe error and retains the last valid snapshot. Initial-load failure remains fatal.

Runtime construction happens outside state locks. Web calls orbit-cmd's RegisteredRuntimeFactory with the exact logical workspace and checkout binding, then caches the resulting Core runtime only while that binding still matches the pinned snapshot.

### Routing and aggregate views

Workspace-scoped API handlers use ?workspace=<id>, falling back to the pinned default. Unknown workspaces return not found; inactive workspaces return a client error. Omitting the selector with no default is also an error.

GET /api/workspaces lists current entries. `GET /api/tasks` and `GET /api/tasks/all` share the same task-page query contract. Both accept `status`, `tag` (also `tags`), `type` (also `task_type`), `q` (also `search`), `limit`, and `cursor`. Status and tag values may be repeated or comma-separated; `q` is a case-insensitive task ID/title substring; and `limit` is a positive caller-supplied page size, defaulting to the server default only when omitted. Every predicate is applied before the page bound, so the page contains the newest matching tasks rather than the newest tasks filtered afterward.

Both endpoints return `{ items, total, limit, truncated, offset, next_cursor }`. `total` is the count of all matches before the page bound and remains the pre-cursor total on later pages. `truncated` means `total > items.length` against that pre-cursor total; it can therefore remain `true` on a final cursor page. `next_cursor`, not `truncated`, tells a client whether to request another page. `offset` is zero for the first page and records the cursor's position thereafter.

`GET /api/tasks` is scoped to the selected workspace. `GET /api/tasks/all` opens active workspaces, skips ones that cannot be opened, tags rows with workspace metadata, applies the same predicates independently in each workspace, and merges the candidate pages under one global `created_at DESC, id ASC` order. Its `total` is the sum of the untruncated per-workspace match counts, and its caller-supplied `limit` bounds the merged page.

The cursor is opaque and bound to its endpoint scope and the active workspace set: a workspace cursor cannot be used for another workspace, and an aggregate cursor becomes invalid if its active workspace scope changes. It is also bound to the active status, tag, type, search, and limit values, so changing any of them requires a fresh request. The cursor continues the stable `created_at DESC, id ASC` order. Inserts newer than the first page do not disturb an existing continuation, but changes to a task's ordering or filter membership can move it across the boundary; clients that need a new live snapshot must restart without a cursor.

## 3. orbit web connect

connect reads no local workspace state. It selects a local loopback port: an explicit --port must be free; otherwise 7878 is preferred and an ephemeral port is used when needed. The SSH forward explicitly binds that port to 127.0.0.1.

The Web-owned tunnel then follows an attach-first lifecycle:

1. Start ssh -N with ExitOnForwardFailure=yes and -L 127.0.0.1:<local>:localhost:<remote>.
2. Poll GET /healthz through the forward for up to five seconds.
3. If health answers 200, keep that commandless forward and mark the session attached.
4. If nothing answers, tear down the probe and start ssh -tt with the same forward plus orbit web serve --no-open --port <remote-port>.
5. Poll health for up to 30 seconds, then open the local browser unless --no-open was requested.
6. Block until Ctrl-C, SIGTERM, or SSH exit; dropping the tunnel terminates and reaps the local SSH child.

In spawn mode, the forced PTY makes connection teardown deliver SIGHUP to the remote serve process started by this session. In attach mode there is no remote command, so teardown closes only the forward and leaves the pre-existing dashboard running.

connect's --workspace is POSIX-quoted and forwarded to the remote serve as --workspace, only in spawn mode. It is not forwarded as --root: on the remote that would choose a registry rather than preselect a workspace. connect rejects a top-level --root outright, since it reads no local Orbit data directory. --global is also forwarded only in spawn mode and remains useful only for older remote binaries. Attach mode sends no remote command, so no option can change an existing server.

## 4. Security

The dashboard is an unauthenticated read/write HTTP application. check_bindable_host permits only loopback addresses. The Origin middleware reduces browser cross-site request risk but is forgeable by non-browser clients and is not authentication.

Remote confidentiality, server identity, and user authentication are delegated to SSH. The local forward is explicitly loopback-bound, but access to that port is still access to the remote dashboard's authority; connect adds no token, ACL, or Orbit session.

Operational limitations:

- The remote host must be reachable by SSH and orbit must be on its non-interactive PATH.
- Exit 127 is reported as a remote PATH problem; exit 255 as an SSH connection problem.
- Local port selection and attach-versus-spawn contain accepted time-of-check/time-of-use races. SSH or the second Web bind fails loudly if a race is lost.
- A target must remain online. Nothing is synchronized or available offline.
