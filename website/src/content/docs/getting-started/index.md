---
title: Getting Started
description: "Install Orbit, initialize state, run a first task, and inspect available activities."
sidebar:
  order: 1
---

## Path

Use this section when you are setting up Orbit for the first time.

<div class="orbit-card-grid">
  <a class="orbit-card" href="./install/">
    <h3>Install Orbit</h3>
    <p>Install through npm, a trusted installer checkout, or a source build.</p>
  </a>
  <a class="orbit-card" href="./first-task/">
    <h3>First Task</h3>
    <p>Create a task, approve it, and ship it.</p>
  </a>
  <a class="orbit-card" href="./workflows/">
    <h3>Delivery Workflows</h3>
    <p>The <code>orbit run</code> surface: shipping, backlog drains, and run inspection.</p>
  </a>
  <a class="orbit-card" href="../how-to/dashboard/">
    <h3>Use the Dashboard</h3>
    <p>Open the operator dashboard locally or over SSH to inspect tasks, runs, and Operations.</p>
  </a>
</div>

## Prerequisites

You need an authenticated supported provider CLI — `claude`, `codex`, `gemini`,
or `grok` — because agent activities dispatch through it. PR mode also requires
the GitHub CLI to be authenticated in the environment where Orbit runs.

Orbit itself can be installed without Rust. You only need a Rust toolchain if you build from source or contribute to the Rust workspace.

## Then what

Once one task ships end to end, the next steps are running a whole backlog and
letting Orbit schedule its own work:

- [Use the Dashboard](../how-to/dashboard/)
- [Run a Continuous Delivery Window](../how-to/continuous-delivery/)
- [Schedule Recurring Work](../how-to/recurring-work/)
- [Publish and Restore Tasks](../how-to/task-publication/)
