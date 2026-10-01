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
    <a class="orbit-hero-release" href="/changelog/"><span class="orbit-hero-release-tag">Early access</span><span>See what shipped in the latest release</span><span aria-hidden="true">→</span></a>
    <h1 id="_top" class="orbit-hero-headline">Your agent files the work. Orbit ships it. <span class="orbit-hero-headline-muted">You review the pull request.</span></h1>
    <p class="orbit-hero-lede">Orbit is a local-first runtime for coding agents. Ask for a change in the agent you already use; Orbit turns it into a scoped task, runs it in an isolated worktree, and hands back a pull request with every step on the record.</p>
    <div class="orbit-hero-actions">
      <a class="orbit-button primary" href="/getting-started/">Get started →</a>
      <a class="orbit-button" href="/how-to/mcp-integration/">Connect your agent</a>
    </div>
    <p class="orbit-hero-requirements">Needs Node 18+, one signed-in agent CLI, and the GitHub CLI for pull requests · macOS and Linux · MIT licensed</p>
    <div class="orbit-hero-providers">
      <div class="orbit-hero-providers-label">Drives the agent CLI you already sign in to</div>
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
      <p class="orbit-hero-providers-note">Gemini CLI remains as a legacy executor. <a href="/concepts/agents/">How agents are invoked →</a></p>
    </div>
  </div>

  <div class="orbit-hero-side">
    <figure class="orbit-session">
      <div class="orbit-session-frame" role="img" aria-label="Illustrative session between you, your agent, and Orbit. You ask for the fsProfile lookup to be documented. The agent calls orbit.task.add and Orbit creates a task in proposed. The agent asks whether to approve and ship; you say yes. The agent calls orbit.task.update, which moves the task from proposed to backlog, then orbit.workflow.ship, and Orbit returns a run ID with the task's file scope reserved in an isolated worktree. The agent calls orbit.workflow.run.show: plan, execute and review settled, a pull request opened, and the task in review. The agent tells you the pull request is open and that the diff and the merge are yours.">
        <div class="orbit-session-bar" aria-hidden="true">
          <span class="orbit-session-dots"><span></span><span></span><span></span></span>
          <span class="orbit-session-name">example-repo · your agent, connected to Orbit</span>
        </div>
        <div class="orbit-session-body" aria-hidden="true">
          <div class="orbit-session-turn is-you"><span class="orbit-session-key">you</span><span class="orbit-session-text">The fsProfile lookup is undocumented. Get that fixed.</span></div>
          <div class="orbit-session-turn"><span class="orbit-session-key">agent</span><span class="orbit-session-text">Filing it as a task with acceptance criteria.</span></div>
          <div class="orbit-session-receipt">
            <div class="orbit-session-row"><span class="orbit-session-tool">orbit.task.add</span><span class="orbit-session-arrow">→</span><span class="orbit-session-result">task <span class="orbit-session-id">&lt;task-id&gt;</span> <em>proposed</em></span></div>
          </div>
          <div class="orbit-session-turn"><span class="orbit-session-key">agent</span><span class="orbit-session-text">Filed. Approve it into the backlog and ship?</span></div>
          <div class="orbit-session-turn is-you"><span class="orbit-session-key">you</span><span class="orbit-session-text">Yes.</span></div>
          <div class="orbit-session-receipt">
            <div class="orbit-session-row"><span class="orbit-session-tool">orbit.task.update</span><span class="orbit-session-arrow">→</span><span class="orbit-session-result">proposed → <em>backlog</em></span></div>
            <div class="orbit-session-row"><span class="orbit-session-tool">orbit.workflow.ship</span><span class="orbit-session-arrow">→</span><span class="orbit-session-result">run <span class="orbit-session-id">&lt;run-id&gt;</span> · scope reserved · worktree isolated</span></div>
          </div>
          <div class="orbit-session-turn"><span class="orbit-session-key">agent</span><span class="orbit-session-text">Pull request open. The diff and the merge are yours.</span></div>
          <div class="orbit-session-receipt">
            <div class="orbit-session-row"><span class="orbit-session-tool">orbit.workflow.run.show</span><span class="orbit-session-arrow">→</span><span class="orbit-session-result">plan · execute · review settled · PR opened · task <em>review</em></span></div>
          </div>
        </div>
      </div>
      <figcaption class="orbit-session-caption">Illustrative session, not captured output. Tool names are real; identifiers are placeholders and arguments are omitted.</figcaption>
    </figure>
  </div>
</section>

<section class="orbit-guarantees" aria-label="What Orbit guarantees">
  <div class="orbit-guarantee">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg></div>
    <div><h2>Nothing runs until you approve</h2><p>New tasks land in <code>proposed</code>. Approval into the backlog is its own step, so your agent has to ask.</p></div>
  </div>
  <div class="orbit-guarantee">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="12" r="2.5"/><path d="M6 8.5v7"/><path d="M8.5 6H12a3.5 3.5 0 0 1 3.5 3.5"/></svg></div>
    <div><h2>Nothing merges without you</h2><p>A ship run stops at <code>review</code> with the pull request open. Finishing delivery is an explicit <code>--complete</code> on one run, never a default.</p></div>
  </div>
  <div class="orbit-guarantee">
    <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 5h11"/><path d="M4 12h11"/><path d="M4 19h7"/><path d="m16 18 2 2 4-4"/></svg></div>
    <div><h2>Every step is on the record</h2><p>Task changes, workflow events, agent turns, and tool calls land in one audit record, redacted as it is written.</p></div>
  </div>
</section>

<section class="orbit-section">
  <div class="orbit-section-head">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">How it works</p>
      <h2 class="orbit-section-heading">From one sentence to a pull request, without leaving your agent.</h2>
    </div>
    <p class="orbit-section-lede">Your agent talks to Orbit over MCP. Every task moves through the same states, and you decide at the two gates that matter: approving the work, and accepting it.</p>
  </div>

  <ol class="orbit-rail" aria-label="Task lifecycle">
    <li><span class="orbit-rail-state">proposed</span></li>
    <li><span class="orbit-rail-state">backlog</span></li>
    <li><span class="orbit-rail-state">in-progress</span></li>
    <li class="is-stop"><span class="orbit-rail-state">review</span><span class="orbit-rail-note">ship stops here · PR open, not merged</span></li>
    <li class="is-later"><span class="orbit-rail-state">done</span></li>
  </ol>

  <div class="orbit-card-grid orbit-card-grid-4">
    <a class="orbit-card" data-tag="01 · Ask" href="/how-to/mcp-integration/">
      <h3>Say what you want</h3>
      <p>Describe the change in Claude Code, Codex, or any supported agent. One command connects Orbit to it.</p>
      <div class="orbit-card-cmd">orbit workspace init --mcp</div>
    </a>
    <a class="orbit-card" data-tag="02 · File" href="/concepts/tasks/">
      <h3>Your agent files a task</h3>
      <p>A title, a complexity, and acceptance criteria: the finish line the work is checked against. It waits in <code>proposed</code>.</p>
      <div class="orbit-card-cmd">orbit.task.add</div>
    </a>
    <a class="orbit-card" data-tag="03 · Ship" href="/how-to/task-lifecycle/">
      <h3>You say go. Orbit ships it.</h3>
      <p>Orbit reserves the files the task may touch, runs it in its own worktree and sandbox, checks it against your gates, and opens a pull request.</p>
      <div class="orbit-card-cmd">orbit.workflow.ship</div>
    </a>
    <a class="orbit-card is-stop" data-tag="04 · Review" href="/how-to/task-lifecycle/">
      <h3>You review and merge</h3>
      <p>Read the diff, CI, and the execution summary. Merge on your terms, then approve the task to close it.</p>
      <div class="orbit-card-cmd">orbit task update "$TASK_ID" --approve</div>
    </a>
  </div>

  <p class="orbit-walk-next">Orbit and GitHub stay independent: approving a task never merges its pull request, and merging never closes the task. Prefer the CLI? <a href="/getting-started/first-task/">Ship your first task by hand</a>.</p>
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
        <p>One record joins task changes, workflow events, provider turns, and tool calls, redacted at write time.</p>
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
    <p class="orbit-section-lede">Every <code>orbit run</code> prints a durable run ID and returns before the outcome is known. Finishing delivery always takes an explicit <code>--complete</code> on the command.</p>
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
        <div><dt>With <code>--complete</code></dt><dd>never: nothing can turn it on for a sweep</dd></div>
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
    <li><a href="/how-to/continuous-delivery/"><span class="orbit-further-title">Continuous delivery</span><span class="orbit-further-desc">Check readiness, drain an approved backlog in a bounded window, and recover it cleanly.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
    <li><a href="/how-to/recurring-work/"><span class="orbit-further-title">Recurring work</span><span class="orbit-further-desc">Run jobs on a schedule and let auto-tasks file routine chores for you.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
    <li><a href="/how-to/distributed-drain/"><span class="orbit-further-title">Multi-machine drains</span><span class="orbit-further-desc">Prepare one owner and replica checkouts to share a workspace's backlog.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
    <li><a href="/how-to/task-publication/"><span class="orbit-further-title">Back up and restore</span><span class="orbit-further-desc">Publish a validated snapshot of your tasks to a Git repository you control, and restore from it.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
    <li><a href="/how-to/dashboard/"><span class="orbit-further-title">The dashboard</span><span class="orbit-further-desc">See tasks, runs, and errors in a browser, locally or over SSH.</span><span class="orbit-further-arrow" aria-hidden="true">→</span></a></li>
  </ul>
</section>

<section class="orbit-quickstart" aria-labelledby="orbit-quickstart-title">
  <div class="orbit-quickstart-copy">
    <h2 id="orbit-quickstart-title">Ship your first task.</h2>
    <p>Three commands install Orbit, set up this machine, and connect your agent to a repository. The fourth opens the dashboard, where you approve, ship, and review what your agent files.</p>
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
