---
title: What Orbit Is
description: "Orbit is a durable, intent-tracked, auditable task layer for developers driving AI coding agents at high volume — local-first by design."
template: splash
prev: false
next: false
---

<div class="orbit-landing not-content">

<section class="orbit-hero">
  <div class="orbit-hero-copy">
    <div class="orbit-hero-eyebrow">early access</div>
    <h1 class="orbit-hero-headline">The audit log for your AI coding agents.</h1>
    <p class="orbit-hero-lede">Durable task lifecycle, task-attributed workflow commits, and structured audit records for agent workflows. Local-first, bring your own model provider.</p>
    <div class="orbit-hero-install">
      <span class="orbit-hero-install-prompt">$</span>
      <code>npm install -g @orbit-tools/cli</code>
      <button class="orbit-hero-install-copy" type="button" data-copy="npm install -g @orbit-tools/cli">Copy</button>
    </div>
    <div class="orbit-hero-actions">
      <a class="orbit-button primary" href="/getting-started/install/">Install Orbit →</a>
      <a class="orbit-button" href="/reference/cli/">Read the CLI reference</a>
    </div>
    <div class="orbit-hero-providers">
      <span>Runs your provider CLI</span>
      <span class="orbit-hero-providers-rule" aria-hidden="true"></span>
      <span>Claude Code</span>
      <span>Codex</span>
      <span>Gemini</span>
      <span>Grok Build</span>
    </div>
  </div>

  <div class="orbit-terminal">
    <div class="orbit-terminal-bar">
      <span>~/repo</span>
      <span>one task, end to end</span>
    </div>
    <div class="orbit-terminal-body">
      <div><span class="orbit-terminal-prompt">$ </span><span class="orbit-terminal-cmd">orbit task add --title "Document fsProfile resolution" \</span></div>
      <div><span class="orbit-terminal-cmd">    --acceptance-criteria "..." --complexity medium</span></div>
      <div><span class="orbit-terminal-id">[TASK_ID]</span></div>
      <div class="orbit-terminal-gap"></div>
      <div><span class="orbit-terminal-prompt">$ </span><span class="orbit-terminal-cmd">orbit run ship "$TASK_ID"</span></div>
      <div>submitted · run <span class="orbit-terminal-id">[RUN_ID]</span></div>
      <div class="orbit-terminal-gap"></div>
      <div><span class="orbit-terminal-prompt">$ </span><span class="orbit-terminal-cmd">orbit run show</span></div>
      <div class="orbit-terminal-head">step   scope             status   duration</div>
      <div>plan   worktree: iso     <span class="orbit-terminal-id">ok</span>       00:12</div>
      <div>edit   fsProfile: docs   <span class="orbit-terminal-id">ok</span>       01:47</div>
      <div>test   fsProfile: docs   <span class="orbit-terminal-id">ok</span>       02:03</div>
      <div>pr     github            <span class="orbit-terminal-id">ok</span>       00:09</div>
      <div class="orbit-terminal-gap"></div>
      <div><span class="orbit-terminal-prompt">$ </span><span class="orbit-terminal-cmd">git log -1 --grep "$TASK_ID" --oneline</span></div>
      <div><span class="orbit-terminal-id">[SHA]</span> docs: document fsProfile resolution</div>
    </div>
  </div>
</section>

<div class="orbit-section-title">Start here</div>

<div class="orbit-card-grid orbit-card-grid-3">
  <a class="orbit-card" data-tag="01" href="/getting-started/install/">
    <h3>Install</h3>
    <p>One binary, no Rust toolchain. Then <code>orbit init</code> sets up your root and skills.</p>
    <div class="orbit-card-cmd">orbit init</div>
  </a>
  <a class="orbit-card" data-tag="02" href="/getting-started/first-task/">
    <h3>Write a task</h3>
    <p>Acceptance criteria are required — agents self-evaluate against them.</p>
    <div class="orbit-card-cmd">orbit task add --title "…"</div>
  </a>
  <a class="orbit-card" data-tag="03" href="/how-to/task-lifecycle/">
    <h3>Ship it</h3>
    <p>The gated pipeline opens a PR, or stays local with <code>--mode local</code>.</p>
    <div class="orbit-card-cmd">orbit run ship "$TASK_ID"</div>
  </a>
</div>

<div class="orbit-section-title">Why Orbit</div>

<div class="orbit-card-grid orbit-card-grid-4">
  <div class="orbit-card">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 5h11"/><path d="M4 12h11"/><path d="M4 19h7"/><path d="m16 18 2 2 4-4"/></svg></div>
    <h3>Auditable</h3>
    <p>Task mutations, workflow events, provider turns, and tool calls emit joined audit records, redacted at write time.</p>
  </div>
  <div class="orbit-card">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3.5"/><path d="M2 12h6.5"/><path d="M15.5 12H22"/></svg></div>
    <h3>Intent-attributed</h3>
    <p>Workflow commits carry the allocated task ID, so <code>git log --grep</code> links code history back to the task record.</p>
  </div>
  <div class="orbit-card">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01"/><path d="M7 16.5h.01"/></svg></div>
    <h3>Local-first</h3>
    <p>Task and run state stay in your Orbit roots. Provider CLIs handle model traffic using your own provider accounts.</p>
  </div>
  <div class="orbit-card">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="7" height="16" rx="2"/><rect x="14" y="4" width="7" height="16" rx="2"/></svg></div>
    <h3>Safe parallel</h3>
    <p>Worktree isolation and filesystem policies (<code>sandbox-exec</code>, <code>bwrap</code>) keep parallel agents from colliding.</p>
  </div>
</div>

<div class="orbit-section-title">Explore the docs</div>

<div class="orbit-docs-index">
  <div class="orbit-docs-group">
    <div class="orbit-docs-group-title">Getting Started</div>
    <a href="/getting-started/install/">Install Orbit</a>
    <a href="/getting-started/first-task/">First Task</a>
    <a href="/getting-started/workflows/">Delivery Workflows</a>
  </div>
  <div class="orbit-docs-group">
    <div class="orbit-docs-group-title">Concepts</div>
    <a href="/concepts/tasks/">Tasks</a>
    <a href="/concepts/activities-jobs/">Activities and Jobs</a>
    <a href="/concepts/policies/">Policies</a>
    <a href="/concepts/agents/">Agents</a>
  </div>
  <div class="orbit-docs-group">
    <div class="orbit-docs-group-title">How-to Guides</div>
    <a href="/how-to/task-lifecycle/">Run a Task Lifecycle</a>
    <a href="/how-to/continuous-delivery/">Run Continuous Delivery</a>
    <a href="/how-to/recurring-work/">Schedule Recurring Work</a>
    <a href="/how-to/task-publication/">Publish and Restore Tasks</a>
    <a href="/how-to/write-activity/">Write an Activity</a>
    <a href="/how-to/scoping-rules/">Choose Scopes</a>
    <a href="/how-to/mcp-integration/">Set Up MCP</a>
  </div>
  <div class="orbit-docs-group">
    <div class="orbit-docs-group-title">Reference</div>
    <a href="/reference/cli/">CLI Commands</a>
    <a href="/reference/activity-job-yaml/">Activity and Job YAML</a>
    <a href="/reference/policy-format/">Policy Format</a>
    <a href="/reference/config/">Configuration</a>
    <a href="/reference/scoping/">Scoping Rules</a>
  </div>
  <div class="orbit-docs-group">
    <div class="orbit-docs-group-title">Contributing</div>
    <a href="/contributing/local-dev/">Local Development</a>
    <a href="/contributing/crate-layout/">Crate Layout</a>
    <a href="/contributing/pr-workflow/">PR Workflow</a>
  </div>
</div>

</div>

<script is:inline>
  document.addEventListener("click", (e) => {
    const btn = e.target.closest(".orbit-hero-install-copy");
    if (!btn) return;
    const text = btn.dataset.copy || "";
    if (!text || !navigator.clipboard) return;
    navigator.clipboard.writeText(text).then(() => {
      const prev = btn.textContent;
      btn.textContent = "Copied";
      setTimeout(() => { btn.textContent = prev; }, 1400);
    });
  });
</script>
