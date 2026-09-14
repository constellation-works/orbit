---
title: What Orbit Is
description: "Orbit runs your coding agents as tracked, reviewable tasks — isolated worktrees, declared file scope, a gated delivery pipeline, and joined audit records. Local-first, bring your own provider CLI."
template: splash
prev: false
next: false
---

<div class="orbit-landing not-content">

<section class="orbit-hero">
  <div class="orbit-hero-copy">
    <div class="orbit-hero-eyebrow">early access</div>
    <h1 class="orbit-hero-headline">Run coding agents as tracked, reviewable tasks.</h1>
    <p class="orbit-hero-lede">Write a task with acceptance criteria. Orbit runs an agent in an isolated worktree, opens a pull request you review, and keeps every mutation in a joined audit record. Local-first, driving the provider CLI you already have.</p>
    <div class="orbit-hero-install">
      <span class="orbit-hero-install-prompt" aria-hidden="true">$</span>
      <code>npm install -g @orbit-tools/cli</code>
      <button class="orbit-copy" type="button" data-copy="npm install -g @orbit-tools/cli" hidden>
        <span class="orbit-copy-label">Copy</span><span class="orbit-sr-only"> the install command</span>
      </button>
      <span class="orbit-copy-status" role="status"></span>
    </div>
    <div class="orbit-hero-actions">
      <a class="orbit-button primary" href="/getting-started/install/">Install Orbit →</a>
      <a class="orbit-button" href="/getting-started/first-task/">Write your first task</a>
    </div>
    <div class="orbit-hero-providers">
      <div class="orbit-hero-providers-label">Drives the provider CLI you already have</div>
      <div class="orbit-hero-providers-list">
        <span>Claude Code</span>
        <span>Codex</span>
        <span>Antigravity</span>
        <span>Grok</span>
        <span>Copilot</span>
        <span>Cursor</span>
        <span>OpenCode</span>
        <span>Pi</span>
      </div>
      <p class="orbit-hero-providers-note">Gemini CLI ships as a legacy executor for enterprise Gemini Code Assist and API-key accounts. <a href="/concepts/agents/">How agents are invoked →</a></p>
    </div>
  </div>

  <figure class="orbit-session">
    <div class="orbit-session-frame" role="img" aria-label="Illustrative terminal session. orbit task add creates a task in proposed. orbit run ship returns a run ID and reserves the task's file scope in an isolated worktree. orbit run show reports the plan, execute and review steps settled and a pull request opened, with the task in review. orbit task update --approve moves the task to done.">
      <div class="orbit-session-bar" aria-hidden="true">
        <span class="orbit-session-dots"><span></span><span></span><span></span></span>
        <span class="orbit-session-name">example-repo — one task, one pull request</span>
      </div>
      <div class="orbit-session-body" aria-hidden="true">
        <div class="orbit-session-step">
          <div class="orbit-session-cmd"><span class="orbit-session-prompt">$</span><code>orbit task add --title "Document fsProfile resolution"</code></div>
          <div class="orbit-session-out"><span class="orbit-session-mark"></span><span class="orbit-session-key">created</span><span class="orbit-session-id">&lt;task-id&gt;</span><span class="orbit-session-note"><em>proposed</em> — approve it into the backlog</span></div>
        </div>
        <div class="orbit-session-step">
          <div class="orbit-session-cmd"><span class="orbit-session-prompt">$</span><code>orbit run ship "&lt;task-id&gt;"</code></div>
          <div class="orbit-session-out"><span class="orbit-session-mark"></span><span class="orbit-session-key">run</span><span class="orbit-session-id">&lt;run-id&gt;</span><span class="orbit-session-note">file scope reserved · worktree isolated</span></div>
        </div>
        <div class="orbit-session-step">
          <div class="orbit-session-cmd"><span class="orbit-session-prompt">$</span><code>orbit run show</code></div>
          <div class="orbit-session-out"><span class="orbit-session-mark"></span><span class="orbit-session-key">steps</span><span class="orbit-session-id">plan · execute · review</span><span class="orbit-session-note">pull request opened</span></div>
          <div class="orbit-session-out"><span class="orbit-session-mark"></span><span class="orbit-session-key">task</span><span class="orbit-session-id">&lt;task-id&gt;</span><span class="orbit-session-note"><em>review</em> — PR open, unmerged</span></div>
        </div>
        <div class="orbit-session-step">
          <div class="orbit-session-cmd"><span class="orbit-session-prompt">$</span><code>orbit task update "&lt;task-id&gt;" --approve</code></div>
          <div class="orbit-session-out"><span class="orbit-session-mark"></span><span class="orbit-session-key">task</span><span class="orbit-session-id">&lt;task-id&gt;</span><span class="orbit-session-note"><em>done</em> — the PR merges on your terms, not Orbit's</span></div>
        </div>
      </div>
    </div>
    <figcaption class="orbit-session-caption">Illustrative session, not captured output. Identifiers are placeholders.</figcaption>
  </figure>
</section>

<section class="orbit-section">
  <div class="orbit-section-head">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">One task, one pull request</p>
      <h2 class="orbit-section-heading">Four commands from intent to a pull request you review.</h2>
    </div>
    <p class="orbit-section-lede">Create the work, run an agent, inspect the result, then review the pull request. The task stops in <code>review</code> with the PR unmerged. Approving the task does not merge the pull request.</p>
  </div>

  <ol class="orbit-rail" aria-label="Task lifecycle">
    <li><span class="orbit-rail-state">proposed</span></li>
    <li><span class="orbit-rail-state">backlog</span></li>
    <li><span class="orbit-rail-state">in-progress</span></li>
    <li class="is-stop"><span class="orbit-rail-state">review</span><span class="orbit-rail-note">ship stops here · PR open, unmerged</span></li>
    <li class="is-later"><span class="orbit-rail-state">done</span></li>
  </ol>

  <div class="orbit-card-grid orbit-card-grid-4">
    <a class="orbit-card" data-tag="01" href="/getting-started/first-task/">
      <h3>Create a task</h3>
      <p>Acceptance criteria are the finish line — agents self-evaluate against them. A new task starts in <code>proposed</code> until you approve it into the backlog.</p>
      <div class="orbit-card-cmd">orbit task add --title "Document fsProfile resolution"</div>
    </a>
    <a class="orbit-card" data-tag="02" href="/how-to/task-lifecycle/">
      <h3>Ship it</h3>
      <p>Orbit reserves those files, runs an agent in an isolated worktree, and opens a pull request. The command returns a run ID immediately.</p>
      <div class="orbit-card-cmd">orbit run ship "$TASK_ID"</div>
    </a>
    <a class="orbit-card" data-tag="03" href="/how-to/dashboard/">
      <h3>Inspect the result</h3>
      <p>Follow the run until the steps settle. The same task and run show up in the operator dashboard.</p>
      <div class="orbit-card-cmd">orbit run show</div>
    </a>
    <a class="orbit-card is-stop" data-tag="04" href="/how-to/task-lifecycle/">
      <h3>Review the pull request</h3>
      <p>Look at the diff, CI, and execution summary. Approving moves the task from <code>review</code> to <code>done</code> — it does not merge the PR by itself.</p>
      <div class="orbit-card-cmd">orbit task update "$TASK_ID" --approve</div>
    </a>
  </div>

  <p class="orbit-walk-next">Next: <a href="/getting-started/install/">install Orbit</a>, then <a href="/getting-started/first-task/">write your first task</a>.</p>
</section>

<section class="orbit-section">
  <div class="orbit-section-head">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">Other delivery modes</p>
      <h2 class="orbit-section-heading">Same pipeline. You choose where it stops and who authorizes the last step.</h2>
    </div>
    <p class="orbit-section-lede">Every <code>orbit run</code> command is asynchronous: it prints a durable run ID and returns without knowing the outcome. Follow up with <code>orbit run show</code>.</p>
  </div>

  <div class="orbit-modes">
    <table>
      <thead>
        <tr>
          <th scope="col">Shape</th>
          <th scope="col">Command</th>
          <th scope="col">Stops at</th>
          <th scope="col">Authorized completion</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <th scope="row"><a href="/how-to/task-lifecycle/">One task, one PR</a><span class="orbit-modes-sub">the default</span></th>
          <td><code>orbit run ship "$TASK_ID"</code></td>
          <td><em>review</em>, with the pull request open and unmerged. Base branch from <code>[workflow] base_branch</code> in <code>config.toml</code>, or <code>main</code> when unset. Override with <code>--base</code>.</td>
          <td>Separate and explicit. <code>--complete</code> lets that one run move the task to <em>done</em> once the PR is verified merged and branch protections and required checks are respected.</td>
        </tr>
        <tr>
          <th scope="row"><a href="/getting-started/workflows/">One task, merged locally</a></th>
          <td><code>orbit run ship "$TASK_ID" --mode local</code></td>
          <td><em>review</em>, but the merge to the configured base has already happened, so this is not a pre-merge stop. No pull request. When you omit <code>--mode</code>, the workspace's registry entry decides, falling back to <code>pr</code>.</td>
          <td>Still separate. <code>--complete</code> moves the task to <em>done</em> once the work is merged and pushed.</td>
        </tr>
        <tr>
          <th scope="row"><a href="/how-to/continuous-delivery/">A bounded window</a></th>
          <td><code>orbit run auto --for 4h --concurrency 8</code></td>
          <td>When the window closes. <code>--for</code> bounds only the start of new work; a task already being shipped still finishes. Concurrency defaults to 5. Check first with <code>orbit run readiness</code>, which reserves and submits nothing.</td>
          <td>Blanket for the window. <code>--complete</code> covers every task the drain admits during the whole window, not just the backlog visible when you started it.</td>
        </tr>
        <tr>
          <th scope="row"><a href="/how-to/recurring-work/">Unattended sweep</a><span class="orbit-modes-sub">for a scheduler</span></th>
          <td><code>orbit run ship-sweep --dry-run</code></td>
          <td>One ship run in every registered workspace with <code>[workflow] auto_ship = true</code>; everything else is reported as skipped. Routines fire it on the <code>orbit sweep</code> clock. Start read-only, then drop <code>--dry-run</code>.</td>
          <td>Never available here. <code>--complete</code> is off unless you pass it on an invocation, and no workspace setting, environment variable, or unattended routine turns it on.</td>
        </tr>
      </tbody>
    </table>
  </div>
</section>

<section class="orbit-section">
  <div class="orbit-section-head orbit-section-head-single">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">Why Orbit</p>
      <h2 class="orbit-section-heading">Rigor and parallel execution stop trading against each other.</h2>
    </div>
  </div>

  <div class="orbit-card-grid orbit-why">
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 5h11"/><path d="M4 12h11"/><path d="M4 19h7"/><path d="m16 18 2 2 4-4"/></svg></div>
      <div class="orbit-card-body">
        <h3>Auditable</h3>
        <p>Task mutations, workflow events, provider turns, and tool calls emit joined audit records, redacted at write time.</p>
        <div class="orbit-card-cmd">orbit task show "$TASK_ID"</div>
      </div>
    </div>
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3.5"/><path d="M2 12h6.5"/><path d="M15.5 12H22"/></svg></div>
      <div class="orbit-card-body">
        <h3>Intent-attributed</h3>
        <p>Workflow commits carry the allocated task ID, so <code>git log --grep</code> links code history back to the task record.</p>
        <div class="orbit-card-cmd">git log --grep "$TASK_ID"</div>
      </div>
    </div>
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01"/><path d="M7 16.5h.01"/></svg></div>
      <div class="orbit-card-body">
        <h3>Local-first</h3>
        <p>Task and run state stay in your Orbit roots. Provider CLIs handle model traffic using your own provider accounts.</p>
        <div class="orbit-card-cmd">ls .orbit/</div>
      </div>
    </div>
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="7" height="16" rx="2"/><rect x="14" y="4" width="7" height="16" rx="2"/></svg></div>
      <div class="orbit-card-body">
        <h3>Safe parallel</h3>
        <p>Worktree isolation, file-scope locks, and OS sandboxes (<code>sandbox-exec</code>, <code>bwrap</code>) keep parallel agents from colliding.</p>
        <div class="orbit-card-cmd">orbit run auto --concurrency 8</div>
      </div>
    </div>
  </div>
</section>

<section class="orbit-section">
  <div class="orbit-section-head orbit-section-head-link">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">Go further</p>
      <h2 class="orbit-section-heading">When one task at a time is not enough.</h2>
    </div>
    <a class="orbit-section-link" href="/reference/cli/">CLI reference →</a>
  </div>

  <div class="orbit-card-grid orbit-card-grid-3">
    <a class="orbit-card" href="/how-to/continuous-delivery/">
      <h3>Continuous delivery</h3>
      <p>Prepare work, approve it, check readiness, then authorize a bounded drain — and recover it safely.</p>
      <div class="orbit-card-cmd">orbit run readiness</div>
    </a>
    <a class="orbit-card" href="/how-to/recurring-work/">
      <h3>Recurring work</h3>
      <p>Routines fire jobs on a cadence and auto-tasks mint recurring chores. Both run on the sweep clock.</p>
      <div class="orbit-card-cmd">orbit sweep --dry-run</div>
    </a>
    <a class="orbit-card" href="/how-to/task-publication/">
      <h3>Publication and recovery</h3>
      <p>Push a validated snapshot of one workspace's tasks to a Git repository you control, then verify and restore it.</p>
      <div class="orbit-card-cmd">orbit task publication status</div>
    </a>
  </div>

  <div class="orbit-docs-panel">
    <p class="orbit-section-eyebrow orbit-docs-panel-title">Explore the docs</p>
    <div class="orbit-docs-index">
      <div class="orbit-docs-group">
        <h3 class="orbit-docs-group-title">Getting started</h3>
        <a href="/getting-started/install/">Install Orbit</a>
        <a href="/getting-started/first-task/">First task</a>
        <a href="/getting-started/workflows/">Delivery workflows</a>
      </div>
      <div class="orbit-docs-group">
        <h3 class="orbit-docs-group-title">Concepts</h3>
        <a href="/concepts/tasks/">Tasks</a>
        <a href="/concepts/activities-jobs/">Activities and jobs</a>
        <a href="/concepts/scheduling/">Routines and auto-tasks</a>
        <a href="/concepts/policies/">Policies</a>
        <a href="/concepts/agents/">Agents</a>
      </div>
      <div class="orbit-docs-group">
        <h3 class="orbit-docs-group-title">How-to guides</h3>
        <a href="/how-to/task-lifecycle/">Run a task lifecycle</a>
        <a href="/how-to/dashboard/">Use the dashboard</a>
        <a href="/how-to/continuous-delivery/">Run continuous delivery</a>
        <a href="/how-to/recurring-work/">Schedule recurring work</a>
        <a href="/how-to/task-publication/">Publish and restore tasks</a>
        <a href="/how-to/write-activity/">Write an activity</a>
        <a href="/how-to/scoping-rules/">Choose scopes</a>
        <a href="/how-to/mcp-integration/">Set up MCP</a>
      </div>
      <div class="orbit-docs-group">
        <h3 class="orbit-docs-group-title">Reference</h3>
        <a href="/reference/cli/">CLI commands</a>
        <a href="/reference/activity-job-yaml/">Activity and job YAML</a>
        <a href="/reference/policy-format/">Policy format</a>
        <a href="/reference/config/">Configuration</a>
        <a href="/reference/scoping/">Scoping rules</a>
      </div>
      <div class="orbit-docs-group">
        <h3 class="orbit-docs-group-title">Contributing</h3>
        <a href="/contributing/local-dev/">Local development</a>
        <a href="/contributing/crate-layout/">Crate layout</a>
        <a href="/contributing/pr-workflow/">PR workflow</a>
      </div>
    </div>
  </div>
</section>

</div>

<script is:inline>
  (() => {
    const RESET_MS = 2400;
    const timers = new WeakMap();
    /** The `role="status"` span that pairs with a copy button. */
    const statusFor = (btn) => {
      const next = btn.nextElementSibling;
      return next && next.classList.contains("orbit-copy-status") ? next : null;
    };
    /* Feedback goes to two places: the button's own visible label, which is
       kept short so the control never reflows the row it sits in, and an
       adjacent `role="status"` region that carries the full sentence for
       assistive technology. */
    const report = (btn, state, message) => {
      const label = btn.querySelector(".orbit-copy-label");
      btn.dataset.state = state;
      if (label) label.textContent = state === "ok" ? "Copied" : "Failed";
      if (state === "error") btn.title = message; else btn.removeAttribute("title");
      const status = statusFor(btn);
      // Clearing first makes a repeated identical message announce again.
      if (status) {
        status.textContent = "";
        window.setTimeout(() => { status.textContent = message; }, 30);
      }
      window.clearTimeout(timers.get(btn));
      timers.set(btn, window.setTimeout(() => {
        delete btn.dataset.state;
        btn.removeAttribute("title");
        if (label) label.textContent = "Copy";
        if (status) status.textContent = "";
      }, RESET_MS));
    };
    /* Last-resort path for browsers without an async clipboard, or on an
       insecure origin where `navigator.clipboard` is undefined. Returns the
       browser's own verdict so a failure is never reported as a success. */
    const legacyCopy = (text) => {
      const field = document.createElement("textarea");
      field.value = text;
      field.setAttribute("readonly", "");
      field.setAttribute("aria-hidden", "true");
      field.style.cssText = "position:fixed;top:0;left:-9999px;opacity:0;";
      document.body.appendChild(field);
      const selection = document.getSelection();
      const previous = selection && selection.rangeCount > 0 ? selection.getRangeAt(0) : null;
      field.select();
      let copied = false;
      try {
        copied = document.execCommand("copy");
      } catch {
        copied = false;
      }
      field.remove();
      if (previous && selection) { selection.removeAllRanges(); selection.addRange(previous); }
      return copied;
    };
    const copy = async (btn) => {
      const text = btn.dataset.copy || "";
      if (!text) return;
      try {
        if (navigator.clipboard && window.isSecureContext) {
          await navigator.clipboard.writeText(text);
          report(btn, "ok", "Copied to clipboard.");
          return;
        }
      } catch {
        // A rejected permission or a hidden document falls through to the
        // legacy path rather than surfacing an unhandled rejection.
      }
      if (legacyCopy(text)) {
        report(btn, "ok", "Copied to clipboard.");
      } else {
        report(btn, "error", "Copy failed — select the command and copy it manually.");
      }
    };
    // The buttons ship hidden so a JavaScript-free page has no dead control.
    for (const btn of document.querySelectorAll(".orbit-copy")) {
      btn.hidden = false;
    }
    document.addEventListener("click", (event) => {
      const btn = event.target.closest(".orbit-copy");
      if (btn) copy(btn);
    });
  })();
</script>
