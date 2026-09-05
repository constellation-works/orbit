---
title: CLI Commands
description: "A compact map of the Orbit CLI command surface."
sidebar:
  order: 2
---

## Environment

| Command | Purpose |
|---------|---------|
| `orbit init` | Initialize the global Orbit root, skills, and skill links. |
| `orbit workspace` | Initialize and manage workspaces. |
| `orbit workspace publication bind/show/rebind/remove` | Manage one owner-local binding to a dedicated task-publication repository; privacy remains operator-managed. |
| `orbit config` | Show or update Orbit configuration. |
| `orbit semantic` | Manage the local embedding companion: `install`, `uninstall`, `stats`, and `index`. |

## Operate

| Command | Purpose |
|---------|---------|
| `orbit run job <job_id>` | Submit an arbitrary job by ID (or YAML path) and return its run ID immediately; `--wait` blocks until the run is terminal and exits nonzero unless it succeeded. |
| `orbit run ship [task_id ...]` | Submit backlog or explicitly selected tasks through the gated shipment pipeline and return a run ID immediately. |
| `orbit run ship --mode local [task_id ...]` | Run the local-only task path for backlog or explicitly selected tasks. |
| `orbit run auto [--for <duration>]` | Drain the workspace backlog for a window (loose leaves, plus one epic) and return a run ID immediately. |
| `orbit run auto --stop` | Stop new admissions for this workspace's active auto coordinator. Already admitted workers keep running; this is not cancellation. |
| `orbit run ship --complete` / `orbit run auto --complete` | Additionally authorize the submitted run to finish delivery and move the tasks it ships from `review` to `done`. Off by default. |
| `orbit task` | Create, update, and manage tasks. |
| `orbit task artifact put <task_id> <source_path>` | Store a UTF-8 file under a task's artifacts directory. |
| `orbit task publication publish/status` | Explicitly publish an owned workspace snapshot or validate its recorded status; task mutation never publishes automatically. |
| `orbit task publication inspect` | Validate and render a labelled repository snapshot without adopting it as live task state. |
| `orbit task publication restore --confirm` | Deliberately restore same-authority task IDs into an empty destination; identical retries require a separate flag. |
| `orbit search <query>` | Search tasks, docs, learnings, and ADRs; add `--hybrid` for task/doc vector ranking, use `orbit search similar <id>` for task neighbors, or `orbit search path <path>` for applicability lookup. |
| `orbit docs` | List, show, add, index, and migrate the docs corpus. |
| `orbit learning` | Create, inspect, update, sync, and prune project learnings. |

## Observe

| Command | Purpose |
|---------|---------|
| `orbit audit` | Query the audit event log. |
| `orbit run history` | Show recent job runs. Filter to one job with `-j <job_id>`. |
| `orbit run show [run_id]` | Show structured state and step summary for a job run (defaults to latest). |
| `orbit run logs [run_id]` | Print raw stdout/stderr captured for a job run. |
| `orbit run events [run_id]` | Show audit events recorded for a job run. |
| `orbit run trace [run_id]` | Show audit event parent/child trace for a job run. |

## Definitions

| Command | Purpose |
|---------|---------|
| `orbit activity` | View activity definitions. See [Activities & Jobs](../concepts/activities-jobs). |
| `orbit job` | View and manage job definitions. See [Activities & Jobs](../concepts/activities-jobs). |
| `orbit tool` | View and manage tools and external MCP plugins. |
| `orbit policy` | View filesystem profile policies and runtime scoping. See [Scoping](./scoping) and [Policy Format](./policy-format). |
| `orbit executor` | View executors. |

## Services

| Command | Purpose |
|---------|---------|
| `orbit mcp init` / `orbit mcp remove` | Register or unregister MCP client integration for Claude Code, Codex, Gemini, and Grok Build. |
| `orbit mcp serve` | Serve the MCP tool surface over stdio. |
| `orbit mcp listen` | Serve the same MCP tool surface on a TCP socket, bound to loopback unless `--allow-non-loopback` is passed. |
| `orbit web serve` | Serve the Orbit dashboard. |
