---
title: What Orbit Is
description: "Orbit is a local-first runtime for coding agents. Your agent files a task over MCP, Orbit ships it in an isolated worktree, and you review the pull request."
template: splash
prev: false
next: false
---

<div class="orbit-landing not-content">

<section class="orbit-hero">
  <div class="orbit-hero-copy">
    <a class="orbit-hero-release" href="/changelog/"><span class="orbit-hero-release-tag">Early access</span><span>What shipped in the latest release</span><span aria-hidden="true">→</span></a>
    <h1 id="_top" class="orbit-hero-headline">Agents write. Orbit delivers.</h1>
    <p class="orbit-hero-lede">Orbit is a local-first runtime for coding agents. Ask the agent you already use for a change: it files a scoped task, Orbit runs it in an isolated worktree, and you get back a pull request with every step on the record.</p>
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
        <li>Cursor</li>
        <li>OpenCode</li>
        <li>Pi</li>
      </ul>
      <p class="orbit-hero-providers-note"><a href="/concepts/agents/">How agents are invoked →</a></p>
    </div>
  </div>

  <div class="orbit-hero-side">
    <figure class="orbit-demo">
      <video class="orbit-demo-video" src="/media/orbit-dashboard-tour.mp4" poster="/media/orbit-dashboard-tour-poster.jpg" width="1320" height="1100" autoplay controls muted loop playsinline preload="auto" aria-label="A 37-second tour of the real Orbit dashboard on a live workspace. Proposed tasks wait for your approval. An auto-drain runs four tasks in parallel, limit eight, while overlapping work waits on file locks. A task's durable record shows the why, its properties and the job run that executed it. The run list shows 12,796 runs, newest first. A pull-request pipeline run steps from an isolated worktree through implement, commit, review gate and push to pr_open in 31 minutes 46 seconds. The audit log counts 41,598 tool calls in 24 hours, 0.3 percent failed. A scoreboard compares Codex, Claude, Grok and Gemini."></video>
      <figcaption class="orbit-demo-caption">Captured from the real dashboard on a live workspace, 2026-10-03. Counts are from that day.</figcaption>
    </figure>
    <script>
      {
        // Copy the hero install command. Clipboard access needs a secure
        // context; without it, select the command so the reader can copy it.
        const button = document.querySelector('.orbit-hero-install-copy');
        const status = document.querySelector('.orbit-hero-install-status');
        let timer;
        button?.addEventListener('click', async () => {
          try {
            await navigator.clipboard.writeText(button.dataset.copy);
          } catch {
            getSelection().selectAllChildren(button.previousElementSibling);
            return;
          }
          button.classList.add('is-copied');
          status.textContent = 'Copied';
          clearTimeout(timer);
          timer = setTimeout(() => {
            button.classList.remove('is-copied');
            status.textContent = '';
          }, 1600);
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
    <div><h2>Nothing runs until you approve</h2><p>New tasks land in <code>proposed</code>. Your agent can file as many as it likes; each one waits for an explicit approval, from you or from your agent when you tell it to.</p></div>
  </div>
  <div class="orbit-guarantee">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="12" r="2.5"/><path d="M6 8.5v7"/><path d="M8.5 6H12a3.5 3.5 0 0 1 3.5 3.5"/></svg></div>
    <div><h2>Nothing merges unless you ask</h2><p>By default a run stops at <code>review</code> with the pull request open. Merging it takes an explicit <code>--complete</code>, and still waits for your branch protection.</p></div>
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
    <li class="is-stop"><span class="orbit-rail-state">done</span><span class="orbit-rail-note">merged · you review the outcome</span></li>
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

  <p class="orbit-walk-next">Turn on second-agent review with <code>operation.review_policy</code> in your <a href="/reference/config/#settable-keys">workspace config</a>. To merge yourself, leave off <code>--complete</code>: every run then stops at <code>review</code> with the pull request open. Or <a href="/getting-started/first-task/">ship your first task by hand</a>.</p>
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
    <p class="orbit-section-lede">Every <code>orbit run</code> prints a durable run ID and returns at once; the run carries on without you. Finishing delivery always takes an explicit <code>--complete</code>.</p>
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
      <div class="orbit-card-cmd">orbit run ship "$TASK_ID" --mode local</div>
      <dl>
        <div><dt>Stops at</dt><dd><em>review</em>, already merged into the base branch. No pull request.</dd></div>
        <div><dt>With <code>--complete</code></dt><dd>closes the task once the work is merged and pushed</dd></div>
      </dl>
    </a>
    <a class="orbit-card orbit-mode" href="/how-to/continuous-delivery/">
      <div class="orbit-mode-head"><h3>Drain a backlog</h3></div>
      <div class="orbit-card-cmd">orbit run auto --for 4h</div>
      <dl>
        <div><dt>Stops at</dt><dd>the end of the window; work already shipping still finishes</dd></div>
        <div><dt>With <code>--complete</code></dt><dd>covers every task the drain admits during the window</dd></div>
      </dl>
    </a>
    <a class="orbit-card orbit-mode" href="/how-to/recurring-work/">
      <div class="orbit-mode-head"><h3>Scheduled sweep</h3><span class="orbit-mode-badge is-muted">scheduler</span></div>
      <div class="orbit-card-cmd">orbit run ship-sweep --dry-run</div>
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
    <li><a href="/how-to/dashboard/"><span class="orbit-further-title">The dashboard</span><span class="orbit-further-desc">See tasks, runs, and errors in a browser, locally or over SSH.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
  </ul>
</section>

<section class="orbit-quickstart" aria-labelledby="orbit-quickstart-title">
  <div class="orbit-quickstart-copy">
    <h2 id="orbit-quickstart-title">Ship your first task.</h2>
    <p>Three commands install Orbit, set up this machine, and connect your agent to a repository; your agent's <code>orbit-setup</code> skill can run the third for you. The fourth opens the dashboard, where you approve, ship, and review what your agent files.</p>
    <div class="orbit-hero-actions">
      <a class="orbit-button primary" href="/getting-started/">Read the guide</a>
      <a class="orbit-button" href="https://github.com/constellation-works/orbit">View on GitHub</a>
    </div>
  </div>
  <ol class="orbit-quickstart-steps">
    <li><code>npm install -g @orbit-tools/cli</code><span>install</span></li>
    <li><code>orbit init</code><span>once per machine</span></li>
    <li><code>orbit workspace init --mcp</code><span>in your repository</span></li>
    <li><code>orbit web serve</code><span>open the dashboard</span></li>
  </ol>
</section>

</div>
