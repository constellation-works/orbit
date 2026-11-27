---
title: Use the Claude Code mod
description: "Open Orbit tasks and runs from links in Claude Code replies."
---

The Orbit plugin links task and run IDs in Claude Code assistant replies to the
[dashboard](/how-to/dashboard/). IDs in code, URLs and existing links stay as
written.

## Set up

1. Install the plugin in Claude Code.

   ```text
   /plugin marketplace add constellation-works/orbit
   /plugin install orbit
   ```

2. [Install and initialize Orbit](/getting-started/install/).
3. Open a new Claude Code session. The plugin reads the local host registry and
   dashboard address once at startup.

## Dashboard address

Links use `http://localhost:7878` by default. Start the local dashboard with
`orbit web serve`, or [set the public dashboard address](/how-to/dashboard/#set-the-public-dashboard-address)
if you use a remote dashboard, a different port or a tunnel on another port.
Start a new Claude Code session after changing the address or registering a host.

Task links open the task on the host that owns its prefix. A run link opens a
run on the machine serving the dashboard. If the host registry is unavailable,
assistant replies still link run IDs.

## Upgrading from the task band and panes

Use the dashboard or Orbit's MCP tools to review tasks, follow runs and take
actions. The former band, status line, toasts, Board and Ship panes, prompt
cards, automatic commit trailers and slash commands have been removed. The
mod has no plugin settings.
