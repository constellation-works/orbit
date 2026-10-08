---
type: design
summary: "Spec: task ids route to the host their prefix names; --host names a host for workspace and pull selection"
last_validated: 2026-10-07
title: Spec — Host routing
owner: opus
status: Implemented
feature: host-registry
tags: [host-registry, federated-mcp, task-migration, spec]
related_features: [host-registry, federated-mcp, task-migration, distributed-drain]
related_artifacts: [ORB-14449, ORB-14448, ORB-14447]
---

# Spec: Host routing

A task id's prefix names the host that holds it. Any task call that addresses a single task
by id, made without an explicit workspace selector, is delivered to that host. If the prefix
is the local host's, the call runs in-process. If it is a registered remote host's, the call
goes there over the federated route. Any other prefix fails closed. Separately, `--host`
names a host for workspace and pull selection, and resolves to the host-qualified selector
that the host itself lists. Status: implemented in [ORB-14449], on top of
[host-commands](./host-commands.md) ([ORB-14448]).

## Why This Exists

The prefix already names a task's only writer ([2_design.md](../2_design.md) §2,
[task-migration decisions](../../task-migration/4_decisions.md)). Nothing routed by it:

- A replica's worktree GC sent every id to its workspace owner, so the Mac asked the box
  about DANI-10433, the Mac's own task, every hour ([ORB-14447]).
- CLI and federated callers needed an explicit selector or an `ssh` hop to read another
  host's task.

The rule is the decision
[A task id routes to the host its prefix names](../4_decisions.md#a-task-id-routes-to-the-host-its-prefix-names).

## Prefix table

Each process builds one table:

- the local `machine.task_prefix` maps to the local host;
- each host-file entry's `task_prefix` maps to that host;
- legacy destination rows, which have no prefix, contribute nothing.

Host-file validation keeps prefixes unique, so lookup is exact and never ambiguous. The
table is read from the host file and never from a probe.

## Routed calls

**Id-routed tools** take exactly one task id as their target and accept an optional
`workspace`. Today these are `orbit.task.show`, `orbit.task.update`, `orbit.task.reject`,
`orbit.task.delete`, `orbit.task.artifact.get`, `orbit.task.artifact.put`,
`orbit.task.review_reset` and `orbit.task.reconcile_review`.

The set is a static list in code. A test fails when a tool whose input schema has a required
task `id` and an optional `workspace` is neither on the list nor explicitly excluded with a
reason. Workspace-scoped tools without an id target are never prefix-routed. `orbit.task.add`,
`list`, `eligible`, `lint`, `pull` and `locks*` keep their selector behavior.

For an id-routed tool:

1. **Explicit selector wins.** When the CLI `--workspace` or the tool's `workspace` field is
   given, behavior is exactly as today.
   - A read through a selector that is not the prefix's host reads that host's mirror.
   - A write there is refused by that host's existing sole-writer rule.
2. **Local prefix.** The call runs in-process, as today.
3. **Registered remote prefix.** The call is delivered to that host through the federated
   client route, the same one pull drains use for their owner. Delivery budgets,
   `outcome_unknown` and the error ladder of
   [federated-workspace-mcp](../../federated-mcp/specs/federated-workspace-mcp.md#fail-closed-routing)
   apply unchanged. The destination resolves the id through its own task registry, as its v1
   id-only path does, and that resolution applies the destination's own checks. An id-routed
   call carries no selector, so the federated rule that refuses task reads on a replica
   selector (`capability_refused`) does not come into play. The prefix's host is the
   task's writer by definition. The `task_reads` entry in the federated conformance reference
   gains the id-only routed case.
4. **Anything else** fails with `unknown_task_prefix`, naming the prefix and pointing to
   `orbit host list` and `orbit host add`.

### Where routing applies

- **CLI.** Every `orbit task` subcommand that wraps an id-routed tool, and
  `orbit tool run <id-routed tool>`.
- **Federated MCP** (`--mode federated`). An id-only call routes by prefix. This amends the
  federated rule that `orbit.task.show` requires a host-qualified selector. A call that
  carries a selector keeps that rule.
- **v1 MCP servers** (`--mode local`, `--mode remote`, `mcp listen`) do not relay, per
  mcp-bridge. An id-only call there whose prefix belongs to a registered remote host fails
  with `task_prefix_remote`, which names that host and its selector. It does not report
  `not_found`.
- **Replica worktree GC.** A lookup for an id with the local prefix reads the local store. An
  id with the owner's prefix goes to the owner through the claim route, as today. An id
  with another registered host's prefix goes to that host. An unregistered prefix is skipped
  with `task_prefix_unroutable` and makes no remote call. This supersedes the interim rule
  in [ORB-14447].

### What routing never does

- **No search.** It never asks several hosts and takes an answer.
- **No mirror fallback.** It never falls back to a local mirror when the prefix's host is
  unreachable. The error says when a local mirror exists and how to read it explicitly with
  `--workspace`.
- **No ownership routing.** It never routes by workspace ownership, caller cwd or session
  defaults.

## Authority

A routed call carries the caller's authority and nothing more:

- An operator CLI is an operator on the destination, under the existing federated session
  rules.
- A process with an agent envelope routes as an agent and never propagates `--operator`.
- The destination's sole-writer rule is the final check, so a write that reaches the wrong
  host is refused there.

## `--host`

`--host <host>` resolves like `orbit host show`: an exact host name or `machine_id`, and the
local host is allowed.

It is accepted:

- with `--workspace` on the routable task commands: `orbit task show`, `update`,
  `artifact put|get`, `review-reset`, and `reconcile-review …`, and on `orbit tool run`;
- with `--pull` on `orbit run auto`.

To resolve, Orbit reads the host's live workspace list and matches the workspace value by
name or `ws_*` id. It then copies that descriptor's `selector`. The selector is never built
by concatenation, which keeps federated selector rule 2.

| Condition | Error |
|---|---|
| host not registered | `unknown_host` |
| host does not answer | `unreachable_destination` |
| host answers but does not list the workspace | `stale_route`, listing the host's workspaces |
| name matches more than one workspace on that host | `unknown_selector` |

`--pull <hm_…/ws_…>` keeps working. `--pull <workspace>` without `--host` is refused unless
the value is a full selector: Orbit never picks a host by itself.

Commands that open a host-local runtime directly reject `--host` at parse time. These include
`task list`, `task add`, other task commands outside the routable set above, `run history`,
`run logs`, `run show`, `doctor`, `workspace …`, `config …`, `update` and the deploy targets.
Pass `--host` after the routable task subcommand, rather than on the `task` group.
The error gives the command to run on that host:
`ssh <entry ssh target> orbit …`. Other host-local commands read files and process state the
tool surface does not expose, and relaying them would make the CLI a remote shell.
Local-only task commands also refuse a remote-qualified `--workspace` before opening a
runtime. For remote task listing or creation, use `orbit tool run orbit.task.list` or
`orbit tool run orbit.task.add` with `--host <host> --workspace <workspace>` and the tool's
JSON input, or run the task command on that host over SSH.

## Non-goals

- No cross-host fan-out for `task list`, search or eligibility.
- No prefix routing for `task add`; it still goes to the selected workspace's control plane.
- No relay from v1 MCP servers.
- No new authority, credential or principal.

## Agent Signature

Specified by opus during on-call ORB-14441 at Daniel's direction (2026-10-07), for
implementation in [ORB-14449].
