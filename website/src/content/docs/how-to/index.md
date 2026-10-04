---
title: How-to Guides
description: "Guides for running Orbit day to day: the dashboard, delivery windows, recurring work, scopes, activities, and multi-machine setups."
sidebar:
  order: 1
---

New to Orbit? [Start Here](../getting-started/) sets it up and walks one task
from request to merge. These guides pick up from there, one job each. Your
agent can do most of them for you with the bundled `orbit-setup` or
`orbit-orchestrate` skill.

## Day to day

<div class="orbit-card-grid">
  <a class="orbit-card" href="./dashboard/">
    <h3>Use the Dashboard</h3>
    <p>Approve and ship tasks, follow runs, start a drain, and reach a remote host over SSH.</p>
  </a>
  <a class="orbit-card" href="./continuous-delivery/">
    <h3>Run a Delivery Window</h3>
    <p>Prepare the backlog, drain it for a set time, retune or stop the window, and recover failed runs.</p>
  </a>
  <a class="orbit-card" href="./recurring-work/">
    <h3>Schedule Recurring Work</h3>
    <p>Run jobs and file recurring chores on a schedule with the sweep clock, routines, and auto-tasks.</p>
  </a>
</div>

## Shape the work

<div class="orbit-card-grid">
  <a class="orbit-card" href="./scoping-rules/">
    <h3>Choose Scopes</h3>
    <p>Decide where state lives and what an activity may read or modify.</p>
  </a>
  <a class="orbit-card" href="./write-activity/">
    <h3>Write an Activity</h3>
    <p>Define your own agent or deterministic step in activity YAML.</p>
  </a>
</div>

## Operate

<div class="orbit-card-grid">
  <a class="orbit-card" href="./task-publication/">
    <h3>Publish and Restore Tasks</h3>
    <p>Snapshot task records to a dedicated repository, and recover them.</p>
  </a>
  <a class="orbit-card" href="./distributed-drain/">
    <h3>Set Up a Distributed Drain</h3>
    <p>Spread one backlog across machines: one owner, replica checkouts, SSH access.</p>
  </a>
</div>
