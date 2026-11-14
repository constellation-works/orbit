---
title: Orbit
description: "Orbit is a local-first task queue and delivery pipeline for coding agents. Your agent files a task, you approve it, and Orbit runs it in an isolated worktree and opens a pull request."
template: splash
prev: false
next: false
---

<div class="orbit-landing not-content">

<section class="orbit-hero">
  <div class="orbit-hero-copy">
    <h1 id="_top" class="orbit-hero-headline">A task queue and delivery pipeline for coding agents.</h1>
    <p class="orbit-hero-lede">Ask the agent you already use for a change. It files a task; once you approve it, Orbit runs it in an isolated worktree, checks it with your validation commands and opens a pull request. Tasks, runs and the audit log stay on your machine.</p>
    <div class="orbit-command">
      <code><span class="orbit-command-prompt" aria-hidden="true">$</span>npm install -g @orbit-tools/cli</code>
      <button type="button" class="orbit-copy" data-copy="npm install -g @orbit-tools/cli" aria-label="Copy install command">
        <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
        <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
      </button>
      <span class="orbit-copy-status" role="status" aria-live="polite"></span>
    </div>
    <div class="orbit-hero-actions">
      <a class="orbit-button primary" href="/getting-started/">Quickstart <span aria-hidden="true">→</span></a>
      <a class="orbit-button" href="/how-to/mcp-integration/">Connect your agent</a>
    </div>
    <p class="orbit-hero-meta">macOS and Linux · <a href="/getting-started/install/#windows">Windows via WSL2</a> · Node 18+ · MIT licensed</p>
    <p class="orbit-hero-meta">Drives the agent CLI you already sign in to: Claude Code, Codex, Antigravity, Grok, Copilot or Muse Code. <a href="/concepts/agents/">How agents run</a></p>
  </div>

  <figure class="orbit-demo">
    <video class="orbit-demo-video" src="/media/orbit-dashboard-tour-v4.mp4" poster="/media/orbit-dashboard-tour-v4-poster.jpg" width="1320" height="1100" autoplay controls muted loop playsinline preload="auto" aria-label="A 49-second tour of the real Orbit dashboard while Orbit's agents work on Orbit itself. Agents propose tasks that wait for your approval, each showing its regression risk and what its pilot found. Tasks in progress lock their files, 17 at capture, so overlapping work waits. The auto-drain has 5 running and 15 admitted in 5 of 7 slots, forecasts CPU and memory, and estimates the time, cost and tokens to clear the backlog; you set the window, the parallel tasks and what happens after a task ships. One task's record shows who created, piloted and implemented it, its acceptance criteria, its pull request and its run. That run goes from an isolated worktree through implement, validate, review gates, push and pull request to landing: 34 steps in 18 minutes 11 seconds. 47 auto-task definitions, 43 enabled, keep the queue fed with audits, sweeps and reviews, on a schedule or after a delivery. Analytics shows 6,894 tasks done in 30 days, 217% more than the 30 days before. Experiments split real tasks between crews and compare cost, duration and first-pass acceptance. Every crew is ranked on one score from landed work, first-pass accepts, rework and escapes, with cost and time left out. The codebase view shows, across 11,190 tasks, the paths that regress, widen their footprint or get blocked."></video>
    <figcaption class="orbit-demo-caption">The Orbit dashboard on a live workspace, captured 2026-11-06. Counts are from that day.</figcaption>
  </figure>
</section>

<section class="orbit-section" aria-labelledby="orbit-how">
  <h2 id="orbit-how" class="orbit-section-heading">How it works</h2>
  <div class="orbit-how">
    <div class="orbit-transcript" role="group" aria-label="Example conversation with your agent">
      <p><span class="orbit-transcript-who">You</span><span>The fsProfile lookup is undocumented. Get that fixed.</span></p>
      <p><span class="orbit-transcript-who">Agent</span><span>Filed a proposed task with acceptance criteria. Approve it and ship?</span></p>
      <p><span class="orbit-transcript-who">You</span><span>Yes.</span></p>
      <p><span class="orbit-transcript-who">Agent</span><span>Queued. Orbit is running it in its own worktree and will open a pull request.</span></p>
    </div>
    <ol class="orbit-steps">
      <li>
        <h3>Your agent files a task <span class="orbit-state">proposed</span></h3>
        <p>Over MCP, with acceptance criteria and the files it expects to touch. Nothing runs yet.</p>
      </li>
      <li>
        <h3>You approve it <span class="orbit-state">backlog</span></h3>
        <p>In the dashboard, from the terminal, or by telling your agent.</p>
      </li>
      <li>
        <h3>Orbit ships it <span class="orbit-state">in-progress</span></h3>
        <p>The run plans, executes and validates in an isolated worktree, then opens a pull request.</p>
      </li>
      <li>
        <h3>You review the result <span class="orbit-state">review</span></h3>
        <p>Merge the pull request and approve the task to close it. Or pass <code>--complete</code> and Orbit merges as soon as branch protection allows.</p>
      </li>
    </ol>
  </div>
</section>

<section class="orbit-section" aria-labelledby="orbit-guarantees">
  <h2 id="orbit-guarantees" class="orbit-section-heading">What you can rely on</h2>
  <div class="orbit-facts">
    <div class="orbit-fact">
      <h3>Nothing runs unapproved</h3>
      <p>New tasks land in <code>proposed</code> and wait for an explicit approval, from you or from your agent when you tell it to.</p>
    </div>
    <div class="orbit-fact">
      <h3>Parallel runs do not collide</h3>
      <p>Each task gets its own worktree, a lock on the files it declared, and an OS sandbox: <code>sandbox-exec</code> on macOS, Bubblewrap on Linux.</p>
    </div>
    <div class="orbit-fact">
      <h3>Every change traces to a task</h3>
      <p>Workflow commits carry the task ID. Task changes, agent turns and tool calls land in one audit record, with secrets redacted as it is written.</p>
    </div>
    <div class="orbit-fact">
      <h3>Local, on your accounts</h3>
      <p>State lives in <code>~/.orbit</code> and <code>.orbit/</code>. Model traffic goes through the agent CLI you signed in to. Orbit sends no telemetry.</p>
    </div>
  </div>
</section>

<section class="orbit-section" aria-labelledby="orbit-modes">
  <div class="orbit-section-head">
    <h2 id="orbit-modes" class="orbit-section-heading">Choose where a run stops</h2>
    <p class="orbit-section-lede"><code>orbit run ship</code> and <code>orbit run auto</code> print a run ID and return; the work carries on without you. <a href="/getting-started/workflows/">Delivery workflows</a></p>
  </div>
    <table class="orbit-modes">
      <thead>
        <tr><th scope="col">Run</th><th scope="col">Stops at</th><th scope="col">With <code>--complete</code></th></tr>
      </thead>
      <tbody>
        <tr>
          <th scope="row"><code>orbit run ship "$TASK_ID"</code></th>
          <td><code>review</code>, with the pull request open</td>
          <td>Merges when branch protection allows, then closes the task</td>
        </tr>
        <tr>
          <th scope="row"><code>orbit run ship "$TASK_ID" --mode local</code></th>
          <td><code>review</code>, already merged into the base branch; no pull request</td>
          <td>Closes the task once the work is merged and pushed</td>
        </tr>
        <tr>
          <th scope="row"><code>orbit run auto --for 4h</code></th>
          <td>The end of the window; work already shipping finishes</td>
          <td>Applies to every task the drain admits</td>
        </tr>
      </tbody>
    </table>
</section>

<section class="orbit-section" aria-labelledby="orbit-start">
  <div class="orbit-section-head">
    <h2 id="orbit-start" class="orbit-section-heading">Ship your first task</h2>
    <p class="orbit-section-lede">Your agent's <code>orbit-setup</code> skill can run the third command for you. The <a href="/getting-started/">Quickstart</a> covers each step.</p>
  </div>
  <ol class="orbit-commands">
    <li><code>npm install -g @orbit-tools/cli</code><span class="orbit-commands-note">install</span><button type="button" class="orbit-copy" aria-label="Copy command">
      <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
      <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
    </button></li>
    <li><code>orbit init</code><span class="orbit-commands-note">once per machine</span><button type="button" class="orbit-copy" aria-label="Copy command">
      <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
      <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
    </button></li>
    <li><code>orbit workspace init --mcp</code><span class="orbit-commands-note">in your repository</span><button type="button" class="orbit-copy" aria-label="Copy command">
      <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
      <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
    </button></li>
    <li><code>orbit web serve</code><span class="orbit-commands-note">open the dashboard</span><button type="button" class="orbit-copy" aria-label="Copy command">
      <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
      <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
    </button></li>
  </ol>
</section>

<section class="orbit-section" aria-labelledby="orbit-next">
  <h2 id="orbit-next" class="orbit-section-heading">Go further</h2>
  <ul class="orbit-index">
    <li><a href="/how-to/continuous-delivery/"><span class="orbit-index-title">Run a delivery window</span><span class="orbit-index-desc">Drain an approved backlog for a set time and recover failed runs.</span></a></li>
    <li><a href="/how-to/recurring-work/"><span class="orbit-index-title">Schedule recurring work</span><span class="orbit-index-desc">Run jobs on a schedule and let auto-tasks file routine chores.</span></a></li>
    <li><a href="/how-to/distributed-drain/"><span class="orbit-index-title">Set up a distributed drain</span><span class="orbit-index-desc">Let other machines pull work from the one that owns your backlog.</span></a></li>
    <li><a href="/how-to/dashboard/"><span class="orbit-index-title">Use the dashboard</span><span class="orbit-index-desc">Approve, ship and follow runs in a browser, locally or over SSH.</span></a></li>
    <li><a href="/concepts/"><span class="orbit-index-title">Concepts</span><span class="orbit-index-desc">Tasks, agents and crews, activities and jobs, and scheduling.</span></a></li>
    <li><a href="/reference/cli/"><span class="orbit-index-title">CLI commands</span><span class="orbit-index-desc">Every command group and what it does.</span></a></li>
  </ul>
</section>

<script>
  {
    // Copy buttons. Clipboard access needs a secure context; without it,
    // select the command so the reader can copy it by hand.
    const status = document.querySelector('.orbit-copy-status');
    const timers = new WeakMap();
    document.addEventListener('click', async (event) => {
      const button = event.target instanceof Element ? event.target.closest('.orbit-copy') : null;
      if (!button) return;
      const label = button.getAttribute('data-label') ?? button.getAttribute('aria-label');
      button.setAttribute('data-label', label);
      const code = button.parentElement?.querySelector('code');
      const command = button.dataset.copy ?? code?.textContent;
      if (!command) return;
      try {
        await navigator.clipboard.writeText(command);
      } catch {
        if (code) getSelection()?.selectAllChildren(code);
        return;
      }
      button.classList.add('is-copied');
      button.setAttribute('aria-label', 'Copied');
      if (status) status.textContent = 'Copied';
      clearTimeout(timers.get(button));
      timers.set(button, setTimeout(() => {
        button.classList.remove('is-copied');
        button.setAttribute('aria-label', label);
        if (status) status.textContent = '';
      }, 1600));
    });
  }
  {
    // The video autoplays; a reader who asks for reduced motion gets the
    // poster frame and the native controls instead.
    const video = document.querySelector('.orbit-demo-video');
    const reduce = matchMedia('(prefers-reduced-motion: reduce)');
    if (video) {
      const sync = () => (reduce.matches ? video.pause() : video.play().catch(() => {}));
      if (reduce.matches) video.pause();
      reduce.addEventListener('change', sync);
    }
  }
</script>

</div>
