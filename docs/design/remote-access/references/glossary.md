---
type: design
summary: "Glossary: Remote Access"
last_validated: 2026-09-07
tags: [remote-access]
---

# Glossary: Remote Access

Vocabulary specific to how Orbit is viewed across workspaces and machines. Standard terms (loopback, SSH port forwarding, pty, TOCTOU) are used with their ordinary meaning and excluded here unless remote access gives them a specific role.

| Term | Meaning |
|------|---------|
| **Aggregate view** | The "All workspaces" task list backed by `GET /api/tasks/all`, tagging each task with its workspace. Per-machine, not cross-machine. See [2_design.md §2](../2_design.md). |
| **connect** | `orbit web connect <ssh-host>`: the client-side command that tunnels a remote machine's loopback dashboard over SSH. See [2_design.md §3](../2_design.md). |
| **Global mode** | `orbit web serve`'s only mode since [ORB-10029]: serves every workspace in `~/.orbit/workspaces.json` regardless of cwd. Contrast single mode. See [2_design.md §1](../2_design.md). |
| **Single mode** | Legacy default (pre-[ORB-10029]) of `orbit web serve` inside a workspace without `--global`: served exactly that workspace. No longer reachable from `orbit web serve`; `DashboardState::single` survives for embedding callers and handler tests. See [2_design.md §1](../2_design.md). |
| **`Ws` extractor** | The axum extractor that selects a request's runtime from `?workspace=<id>` (or the default), replacing the former single-runtime `State`. See [2_design.md §2](../2_design.md). |
| **WsEntry** | One servable workspace in `DashboardState` (`id`, `name`, `repo_root`, `orbit_dir`, `active`); inactive entries are listed but never built. See [2_design.md §2](../2_design.md). |
| **Viewing-not-sync** | The load-bearing boundary: remote access shows existing state on a reachable machine and never synchronizes or writes across machines. See [Live remote/multi-workspace dashboard viewing supersedes the git-sync task registry](../4_decisions.md#live-remotemulti-workspace-dashboard-viewing-supersedes-the-git-sync-task-registry). |
