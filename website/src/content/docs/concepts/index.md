---
title: Concepts
description: "The core Orbit concepts: tasks, activities and jobs, routines and auto-tasks, policies, and agent runtimes."
sidebar:
  order: 1
---

Orbit is a small number of primitives arranged in layers. Each layer answers
one question, and each page below describes one layer's contract — what it
guarantees, what it refuses, and why. For the commands, see the
[how-to guides](../how-to/).

<nav class="orbit-stack" aria-label="Orbit layers, top to bottom">
  <a class="orbit-stack-layer" href="./scheduling/">
    <span class="orbit-stack-q">When</span>
    <span class="orbit-stack-name">Routines and auto-tasks</span>
    <span class="orbit-stack-desc">Fire jobs on a cadence; mint recurring chores as tasks.</span>
  </a>
  <a class="orbit-stack-layer" href="./tasks/">
    <span class="orbit-stack-q">What</span>
    <span class="orbit-stack-name">Tasks</span>
    <span class="orbit-stack-desc">The durable, reviewable unit of work.</span>
  </a>
  <a class="orbit-stack-layer" href="./activities-jobs/">
    <span class="orbit-stack-q">How</span>
    <span class="orbit-stack-name">Activities and jobs</span>
    <span class="orbit-stack-desc">Reusable execution units and the workflows that compose them.</span>
  </a>
  <a class="orbit-stack-layer" href="./agents/">
    <span class="orbit-stack-q">Who</span>
    <span class="orbit-stack-name">Agents</span>
    <span class="orbit-stack-desc">Provider CLIs, executors, and the crews tasks run under.</span>
  </a>
  <a class="orbit-stack-layer orbit-stack-layer-guard" href="./policies/">
    <span class="orbit-stack-q">Bounds</span>
    <span class="orbit-stack-name">Policies</span>
    <span class="orbit-stack-desc">Filesystem guardrails applied to every layer above.</span>
  </a>
</nav>

## Pages

<div class="orbit-card-grid">
  <a class="orbit-card" href="./tasks/" data-tag="01">
    <h3>Tasks</h3>
    <p>Lifecycle, statuses, the two human approval gates, and the invariants transitions must respect.</p>
  </a>
  <a class="orbit-card" href="./activities-jobs/" data-tag="02">
    <h3>Activities and Jobs</h3>
    <p>Typed execution units, the orchestration grammar, and how task requirements reach a run.</p>
  </a>
  <a class="orbit-card" href="./scheduling/" data-tag="03">
    <h3>Routines and Auto-Tasks</h3>
    <p>The sweep clock, versioned triggers, recurring chores as data, and what unattended never gets.</p>
  </a>
  <a class="orbit-card" href="./policies/" data-tag="04">
    <h3>Policies</h3>
    <p>Filesystem profiles and the deny rules that bound runtime execution.</p>
  </a>
  <a class="orbit-card" href="./agents/" data-tag="05">
    <h3>Agents</h3>
    <p>Provider CLIs and executors, tool allowlists, and crews — the named provider-model assignments tasks run under.</p>
  </a>
</div>
