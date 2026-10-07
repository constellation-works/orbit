---
title: Concepts
description: "Orbit's five layers: scheduling, tasks, activities and jobs, agents, and policies."
sidebar:
  order: 1
---

Orbit has five layers. Each answers one question: when work runs, what the
work is, how it runs, who runs it, and what bounds it. Each page below covers
one layer: what it guarantees and what it refuses. For commands, see the
[how-to guides](../how-to/).

<nav class="orbit-stack" aria-label="Orbit layers, top to bottom">
  <a class="orbit-stack-layer" href="./scheduling/">
    <span class="orbit-stack-q">When</span>
    <span class="orbit-stack-name">Routines and auto-tasks</span>
    <span class="orbit-stack-desc">Fire jobs on a schedule; file recurring chores as tasks.</span>
  </a>
  <a class="orbit-stack-layer" href="./tasks/">
    <span class="orbit-stack-q">What</span>
    <span class="orbit-stack-name">Tasks</span>
    <span class="orbit-stack-desc">The durable, reviewable unit of work.</span>
  </a>
  <a class="orbit-stack-layer" href="./activities-jobs/">
    <span class="orbit-stack-q">How</span>
    <span class="orbit-stack-name">Activities and jobs</span>
    <span class="orbit-stack-desc">Reusable execution units and the jobs that chain them.</span>
  </a>
  <a class="orbit-stack-layer" href="./agents/">
    <span class="orbit-stack-q">Who</span>
    <span class="orbit-stack-name">Agents</span>
    <span class="orbit-stack-desc">Provider CLIs and the crews tasks run under.</span>
  </a>
  <a class="orbit-stack-layer orbit-stack-layer-guard" href="./policies/">
    <span class="orbit-stack-q">Bounds</span>
    <span class="orbit-stack-name">Policies</span>
    <span class="orbit-stack-desc">Filesystem rules that bound every run.</span>
  </a>
</nav>

## Pages

<div class="orbit-card-grid">
  <a class="orbit-card" href="./tasks/" data-tag="01">
    <h3>Tasks</h3>
    <p>Lifecycle, statuses, the two approval gates, and the transition rules.</p>
  </a>
  <a class="orbit-card" href="./activities-jobs/" data-tag="02">
    <h3>Activities and jobs</h3>
    <p>Reusable execution units, the jobs that chain them, and how a task's required tools reach a run.</p>
  </a>
  <a class="orbit-card" href="./scheduling/" data-tag="03">
    <h3>Routines and auto-tasks</h3>
    <p>The host scheduler clock, routines that fire jobs, auto-tasks that file chores, and the limits on unattended work.</p>
  </a>
  <a class="orbit-card" href="./policies/" data-tag="04">
    <h3>Policies</h3>
    <p>Filesystem profiles and the deny rules that bound what a run can read and write.</p>
  </a>
  <a class="orbit-card" href="./agents/" data-tag="05">
    <h3>Agents</h3>
    <p>Provider CLIs, executors, tool policy, and crews: the named provider and model a task runs under.</p>
  </a>
</div>
