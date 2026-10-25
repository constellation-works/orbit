---
title: What Orbit Is
description: "Orbit is a local-first runtime for coding agents. Your agent files a task over MCP, Orbit ships it in an isolated worktree and lands it, with optional review after landing."
template: splash
prev: false
next: false
---

<div class="orbit-landing not-content">

<section class="orbit-hero">
  <div class="orbit-hero-copy">
    <a class="orbit-hero-release" href="/changelog/"><span class="orbit-hero-release-tag">Early access</span><span>What shipped in the latest release</span><span aria-hidden="true">→</span></a>
    <h1 id="_top" class="orbit-hero-headline">Agents write. Orbit delivers.</h1>
    <p class="orbit-hero-lede">Orbit is a local-first runtime for coding agents. Ask the agent you already use for a change: it files a scoped task, Orbit runs it in an isolated worktree and lands it. Enable review after landing to check the results while the queue keeps moving. Every step is on the record.</p>
    <div class="orbit-hero-actions">
      <a class="orbit-button primary" href="/getting-started/">Get started →</a>
      <a class="orbit-button" href="/how-to/mcp-integration/">Connect your agent</a>
    </div>
    <div class="orbit-hero-install">
      <code><span class="orbit-hero-install-prompt" aria-hidden="true">$</span>npm install -g @orbit-tools/cli</code>
      <button type="button" class="orbit-hero-install-copy" data-copy="npm install -g @orbit-tools/cli" aria-label="Copy install command">
        <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
        <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
      </button>
      <span class="orbit-hero-install-status" role="status" aria-live="polite"></span>
    </div>
    <p class="orbit-hero-requirements">Needs Node 18+, one signed-in agent CLI, and the GitHub CLI for pull requests · macOS and Linux · MIT licensed</p>
    <div class="orbit-hero-providers">
      <div class="orbit-hero-providers-label">Runs the agent CLI you already sign in to</div>
      <ul class="orbit-hero-providers-list">
        <li>Claude Code</li>
        <li>Codex</li>
        <li>Antigravity</li>
        <li>Grok</li>
        <li>Copilot</li>
        <li>Muse Code</li>
      </ul>
      <p class="orbit-hero-providers-note">Legacy Gemini CLI is also available. <a href="/concepts/agents/">How agents are invoked →</a></p>
    </div>
  </div>

  <div class="orbit-hero-side">
    <figure class="orbit-demo">
      <video class="orbit-demo-video" src="/media/orbit-dashboard-tour-v3.mp4" poster="/media/orbit-dashboard-tour-v3-poster.jpg" width="1320" height="1100" autoplay controls muted loop playsinline preload="auto" aria-label="A 40-second tour of the real Orbit dashboard while Orbit's agents work on Orbit itself. Agents propose tasks that wait for your approval, and 16 tasks run at once across two hosts, with 100 files locked so overlapping work waits its turn. The auto-drain has 12 running and 19 admitted, fills all 12 slots, and pauses new starts while the host runs above 0.95 times its cores; running tasks carry on. Auto-concurrency forecasts CPU and memory for each admission and enforces the fit. A Codex pull-request pipeline run steps from an isolated worktree through implement, commit, validate, review gate and push to an open pull request: 21 steps in 24 minutes 15 seconds. Routines on a host clock turn red CI and Dependabot alerts into tasks, and 22 auto-task definitions, 14 enabled, file sweeps and reviews on a schedule or after each delivery. Analytics shows 3,277 tasks done in 30 days, 167% more than the 30 days before, and each model's runs and implementation times. The codebase view ranks the paths agents struggle with across 6,002 tasks; 10 of 63 ranked paths turn up in several rankings."></video>
      <figcaption class="orbit-demo-caption">Captured from the real dashboard on a live workspace, 2026-10-17. Counts are from that day.</figcaption>
    </figure>
    <script>
      {
        // Copy commands on the hero and quickstart rows. Clipboard access needs
        // a secure context; without it, select the command for manual copying.
        const status = document.querySelector('.orbit-hero-install-status');
        const timers = new WeakMap();
        document.addEventListener('click', async (event) => {
          const button = event.target instanceof Element ? event.target.closest('.orbit-hero-install-copy, .orbit-quickstart-copy-button') : null;
          if (!button) return;
          const label = button.classList.contains('orbit-hero-install-copy') ? 'Copy install command' : 'Copy command';
          const code = button.closest('li')?.querySelector('code') ?? button.previousElementSibling;
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
          if (status && button.classList.contains('orbit-hero-install-copy')) status.textContent = 'Copied';
          clearTimeout(timers.get(button));
          timers.set(button, setTimeout(() => {
            button.classList.remove('is-copied');
            button.setAttribute('aria-label', label);
            if (status && button.classList.contains('orbit-hero-install-copy')) status.textContent = '';
          }, 1600));
        });
      }
      {
        // Autoplay is the default; a reader who asks for reduced motion gets the
        // poster frame and the native controls instead.
        const video = document.querySelector('.orbit-demo-video');
        const reduce = matchMedia('(prefers-reduced-motion: reduce)');
        const sync = () => (reduce.matches ? video.pause() : video.play().catch(() => {}));
        if (video) {
          if (reduce.matches) video.pause();
          reduce.addEventListener('change', sync);
        }
      }
    </script>
  </div>
</section>

<section class="orbit-guarantees" aria-label="What Orbit guarantees">
  <div class="orbit-guarantee">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg></div>
    <div><h2>Nothing runs until you approve</h2><p>New tasks land in <code>proposed</code>. Your agent can file as many as it likes; each one waits for an explicit approval, from you or from your agent when you tell it to. You can also authorize a drain with <code>--approve-proposed</code> to promote tasks that pass task-pilot verification.</p></div>
  </div>
  <div class="orbit-guarantee">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="12" r="2.5"/><path d="M6 8.5v7"/><path d="M8.5 6H12a3.5 3.5 0 0 1 3.5 3.5"/></svg></div>
    <div><h2>Code lands fast, with optional review</h2><p>Changes land on your configured base branch after the required validation commands pass. Review is off by default. Enable <code>delivery-code-review</code> to review batches after landing and file findings as tasks. Review before landing is optional: set <code>review.timing</code> to <code>before_pr</code> or <code>before_landing</code> if you want a gate. Protected branches still enforce their required checks and approvals.</p></div>
  </div>
  <div class="orbit-guarantee">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 5h11"/><path d="M4 12h11"/><path d="M4 19h7"/><path d="m16 18 2 2 4-4"/></svg></div>
    <div><h2>Every step is on the record</h2><p>Task changes, workflow events, agent turns, and tool calls land in one audit record, with secrets redacted as it is written.</p></div>
  </div>
</section>

<section class="orbit-section">
  <div class="orbit-section-head">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">How it works</p>
      <h2 class="orbit-section-heading">From a spec to merged code. You set the direction and judge the result.</h2>
    </div>
    <p class="orbit-section-lede">Hand a spec to the agent you already use; the <code>orbit-orchestrate</code> skill makes it your orchestrator. It splits the work into tasks and queues them once you approve. Orbit ships them in parallel and, when you authorize it, merges each one as soon as branch protection allows. You come back to results, not a queue of pull requests.</p>
  </div>

  <ol class="orbit-rail" aria-label="Task lifecycle">
    <li><span class="orbit-rail-state">proposed</span></li>
    <li><span class="orbit-rail-state">backlog</span><span class="orbit-rail-note">on your say-so</span></li>
    <li><span class="orbit-rail-state">in-progress</span></li>
    <li><span class="orbit-rail-state">review</span></li>
    <li class="is-stop"><span class="orbit-rail-state">done</span><span class="orbit-rail-note">completion authorized</span></li>
  </ol>

  <div class="orbit-card-grid orbit-card-grid-4">
    <a class="orbit-card" data-tag="01 · Spec" href="/how-to/mcp-integration/">
      <h3>You write the spec</h3>
      <p>Describe the outcome, not the steps: what should exist when it's done, and how you'll know. One command connects Orbit to the agent you already use.</p>
      <div class="orbit-card-cmd">orbit workspace init --mcp</div>
    </a>
    <a class="orbit-card" data-tag="02 · Plan" href="/concepts/tasks/">
      <h3>Your orchestrator files the tasks</h3>
      <p>With the <code>orbit-orchestrate</code> skill, it splits the spec into scoped tasks with acceptance criteria, queues them in the backlog once you approve, and starts a drain.</p>
      <div class="orbit-card-cmd">orbit.task.add</div>
    </a>
    <a class="orbit-card" data-tag="03 · Deliver" href="/how-to/continuous-delivery/">
      <h3>Orbit ships and merges</h3>
      <p>Tasks run in parallel, each in its own worktree and sandbox with a lock on the files it touches. Each is checked against your gates, optionally reviewed by a second agent, and merged as soon as branch protection allows.</p>
      <div class="orbit-card-cmd">orbit run auto --complete</div>
    </a>
    <a class="orbit-card is-stop" data-tag="04 · Review" href="/how-to/dashboard/">
      <h3>You review the outcome</h3>
      <p>Judge what landed against the spec: the merged changes, what the reviewer fixed, and every agent turn and tool call behind them. Anything off becomes the next task.</p>
      <div class="orbit-card-cmd">orbit web serve</div>
    </a>
  </div>

  <p class="orbit-walk-next">Choose a second-agent review point: before PR with <code>review.timing = "before_pr"</code>, before landing with <code>review.timing = "before_landing"</code>, or after landing with <code>orbit auto-task toggle delivery-code-review on</code>. See <a href="concepts/tasks/#review-layers">Review layers</a> for when each runs and what it blocks. With PR delivery, leave off <code>--complete</code> to stop at <code>review</code> with the pull request open; local delivery merges before stopping at <code>review</code>. Or <a href="getting-started/first-task/">ship your first task by hand</a>. Optional <a href="concepts/agents/#crews">read-only exploration and operator steering</a> support work already in progress.</p>
</section>

<section class="orbit-section">
  <div class="orbit-section-head orbit-section-head-single">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">Why Orbit</p>
      <h2 class="orbit-section-heading">Run agents in parallel without losing track of what they did.</h2>
    </div>
  </div>

  <div class="orbit-card-grid orbit-why">
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="7" height="16" rx="2"/><rect x="14" y="4" width="7" height="16" rx="2"/></svg></div>
      <div class="orbit-card-body">
        <h3>Safe in parallel</h3>
        <p>Each task gets its own worktree, a lock on the files it declared, and an OS sandbox (<code>sandbox-exec</code> on macOS, Bubblewrap on Linux), so parallel runs never write over each other.</p>
        <div class="orbit-card-cmd">orbit run auto --concurrency 8</div>
      </div>
    </div>
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3.5"/><path d="M2 12h6.5"/><path d="M15.5 12H22"/></svg></div>
      <div class="orbit-card-body">
        <h3>Traceable to intent</h3>
        <p>Every workflow commit carries its task ID, so any line of code leads back to the request and the acceptance criteria behind it.</p>
        <div class="orbit-card-cmd">git log --grep "$TASK_ID"</div>
      </div>
    </div>
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 5h11"/><path d="M4 12h11"/><path d="M4 19h7"/><path d="m16 18 2 2 4-4"/></svg></div>
      <div class="orbit-card-body">
        <h3>Auditable end to end</h3>
        <p>One record joins task changes, workflow events, agent turns, and tool calls, with secrets redacted at write time.</p>
        <div class="orbit-card-cmd">orbit task show "$TASK_ID"</div>
      </div>
    </div>
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01"/><path d="M7 16.5h.01"/></svg></div>
      <div class="orbit-card-body">
        <h3>Local-first, your accounts</h3>
        <p>Tasks and runs stay on your machine. Model traffic goes through the agent CLI and the account you already have, and Orbit sends no telemetry.</p>
        <div class="orbit-card-cmd">ls .orbit/</div>
      </div>
    </div>
  </div>
</section>

<section class="orbit-section">
  <div class="orbit-section-head">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">When you step away</p>
      <h2 class="orbit-section-heading">The same pipeline, unattended. You choose where it stops.</h2>
    </div>
    <p class="orbit-section-lede"><code>orbit run ship</code> and <code>orbit run auto</code> submit work, print a durable run ID, and return; the run carries on without you. Authorize completion per run with <code>--complete</code>, or configure the owner to finish <a href="/how-to/distributed-drain/">distributed handoffs</a> with <code>workflow.distributed_completion = "done"</code>.</p>
  </div>

  <div class="orbit-card-grid orbit-card-grid-4 orbit-mode-grid">
    <a class="orbit-card orbit-mode" href="/getting-started/workflows/">
      <div class="orbit-mode-head"><h3>One task, one PR</h3><span class="orbit-mode-badge">default</span></div>
      <div class="orbit-card-cmd">orbit run ship "$TASK_ID"</div>
      <dl>
        <div><dt>Stops at</dt><dd><em>review</em>, with the pull request open and not merged</dd></div>
        <div><dt>With <code>--complete</code></dt><dd>merges as soon as branch protection allows, then closes the task</dd></div>
      </dl>
    </a>
    <a class="orbit-card orbit-mode" href="/getting-started/workflows/">
      <div class="orbit-mode-head"><h3>Merge locally</h3></div>
      <div class="orbit-card-cmd">orbit run ship "$TASK_ID" <span class="orbit-command-token">--mode</span> local</div>
      <dl>
        <div><dt>Stops at</dt><dd><em>review</em>, already merged into the base branch. No pull request.</dd></div>
        <div><dt>With <code>--complete</code></dt><dd>closes the task once the work is merged and pushed</dd></div>
      </dl>
    </a>
    <a class="orbit-card orbit-mode" href="/how-to/continuous-delivery/">
      <div class="orbit-mode-head"><h3>Drain a backlog</h3></div>
      <div class="orbit-card-cmd">orbit run auto <span class="orbit-command-token">--for</span> 4h</div>
      <dl>
        <div><dt>Stops at</dt><dd>the end of the window; work already shipping still finishes</dd></div>
        <div><dt>With <code>--complete</code></dt><dd>covers every task the drain admits during the window</dd></div>
      </dl>
    </a>
    <a class="orbit-card orbit-mode" href="/how-to/recurring-work/">
      <div class="orbit-mode-head"><h3>Scheduled sweep</h3><span class="orbit-mode-badge is-muted">scheduler</span></div>
      <div class="orbit-card-cmd">orbit run ship-sweep <span class="orbit-command-token">--dry-run</span></div>
      <dl>
        <div><dt>Stops at</dt><dd><em>review</em>, in each workspace with <code>auto_ship</code> on</dd></div>
        <div><dt>With <code>--complete</code></dt><dd>never; a sweep cannot be granted completion</dd></div>
      </dl>
    </a>
  </div>
</section>

<section class="orbit-section orbit-further">
  <div class="orbit-section-intro">
    <p class="orbit-section-eyebrow">Go further</p>
    <h2 class="orbit-section-heading">When one task at a time is not enough.</h2>
    <a class="orbit-section-link" href="/reference/cli/">Browse the CLI reference →</a>
  </div>
  <ul class="orbit-further-list">
    <li><a href="/how-to/continuous-delivery/"><span class="orbit-further-title">Continuous delivery</span><span class="orbit-further-desc">Check readiness, drain an approved backlog for a set window, and recover failed deliveries.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
    <li><a href="/how-to/recurring-work/"><span class="orbit-further-title">Recurring work</span><span class="orbit-further-desc">Run jobs on a schedule, and let auto-tasks file routine chores.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
    <li><a href="/how-to/distributed-drain/"><span class="orbit-further-title">Multi-machine drains</span><span class="orbit-further-desc">Let other machines pull work from the one that owns your backlog.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
    <li><a href="/how-to/task-publication/"><span class="orbit-further-title">Back up and restore</span><span class="orbit-further-desc">Publish a validated snapshot of your tasks to a Git repository you control, and restore it.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
    <li><a href="/how-to/dashboard/"><span class="orbit-further-title">The dashboard</span><span class="orbit-further-desc">See tasks, runs, errors, agent scorecards, and host resource analytics in a browser, locally or over SSH.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
  </ul>
</section>

<section class="orbit-quickstart" aria-labelledby="orbit-quickstart-title">
  <div class="orbit-quickstart-copy">
    <h2 id="orbit-quickstart-title">Ship your first task.</h2>
    <p>Three commands install Orbit, set up this machine, and connect your agent to a repository; your agent's <code>orbit-setup</code> skill can run the third for you. Choose an enabled default crew as described in the <a href="/getting-started/install/">installation guide</a>. The fourth opens the dashboard, where you approve, ship, and review what your agent files.</p>
    <div class="orbit-hero-actions">
      <a class="orbit-button primary" href="/getting-started/">Read the guide</a>
      <a class="orbit-button" href="https://github.com/constellation-works/orbit">View on GitHub</a>
    </div>
  </div>
  <ol class="orbit-quickstart-steps">
    <li><code>npm install -g @orbit-tools/cli</code><div class="orbit-quickstart-step-tools"><span class="orbit-quickstart-step-label">install</span><button type="button" class="orbit-quickstart-copy-button" aria-label="Copy command">
      <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
      <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
    </button></div></li>
    <li><code>orbit init</code><div class="orbit-quickstart-step-tools"><span class="orbit-quickstart-step-label">once per machine</span><button type="button" class="orbit-quickstart-copy-button" aria-label="Copy command">
      <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
      <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
    </button></div></li>
    <li><code>orbit workspace init --mcp</code><div class="orbit-quickstart-step-tools"><span class="orbit-quickstart-step-label">in your repository</span><button type="button" class="orbit-quickstart-copy-button" aria-label="Copy command">
      <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
      <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
    </button></div></li>
    <li><code>orbit web serve</code><div class="orbit-quickstart-step-tools"><span class="orbit-quickstart-step-label">open the dashboard</span><button type="button" class="orbit-quickstart-copy-button" aria-label="Copy command">
      <svg class="orbit-icon-copy" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>
      <svg class="orbit-icon-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 5 5 9-10"/></svg>
    </button></div></li>
  </ol>
</section>

</div>
