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
    <p class="orbit-hero-lede">Write a task with acceptance criteria. Orbit reserves its file scope, runs an agent in an isolated worktree, and takes it through a gated pipeline to a pull request you review — with every mutation in a joined audit record. Local-first, driving the provider CLI you already have.</p>
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
      <span>Drives your provider CLI</span>
      <span class="orbit-hero-providers-rule" aria-hidden="true"></span>
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

  <figure class="orbit-dash">
    <div class="orbit-dash-frame" role="img" aria-label="Illustrative operator dashboard. Tasks is selected in the left rail. Placeholder task Document fsProfile resolution is in review, with the pull request open and unmerged. Approve is available; ship is not.">
      <div class="orbit-dash-rail" aria-hidden="true">
        <div class="orbit-dash-brand">
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
            <circle cx="12" cy="12" r="8.5" fill="none" stroke="currentColor" stroke-width="1.6"></circle>
            <circle class="orbit-dash-mark" cx="18.5" cy="6.5" r="2.5"></circle>
          </svg>
          <span>orbit</span>
        </div>
        <div class="orbit-dash-rail-group">
          <div class="orbit-dash-rail-label">Work</div>
          <div class="orbit-dash-rail-item is-active">Tasks</div>
        </div>
        <div class="orbit-dash-rail-group">
          <div class="orbit-dash-rail-label">Observe</div>
          <div class="orbit-dash-rail-item">Audit</div>
          <div class="orbit-dash-rail-item">Diagnostics</div>
        </div>
        <div class="orbit-dash-rail-group">
          <div class="orbit-dash-rail-label">Manage</div>
          <div class="orbit-dash-rail-item">Operations</div>
          <div class="orbit-dash-rail-item">Knowledge</div>
        </div>
        <div class="orbit-dash-rail-foot">example-repo</div>
      </div>
      <div class="orbit-dash-main" aria-hidden="true">
        <div class="orbit-dash-topbar">
          <span class="orbit-dash-crumb">Tasks</span>
          <span class="orbit-dash-jump">Jump to [TASK_ID]</span>
          <span class="orbit-dash-refresh">Refresh</span>
        </div>
        <div class="orbit-dash-body">
          <div class="orbit-dash-panel">
            <div class="orbit-dash-panel-head">
              <span>Tasks</span>
              <span class="orbit-dash-count">1</span>
            </div>
            <div class="orbit-dash-filters">
              <span>All</span>
              <span class="is-on">Review</span>
              <span>Backlog</span>
            </div>
            <div class="orbit-dash-row is-open">
              <span class="orbit-dash-id">[TASK_ID]</span>
              <span class="orbit-dash-title">Document fsProfile resolution</span>
              <span class="orbit-dash-status">review</span>
            </div>
            <div class="orbit-dash-detail">
              <p>Default ship stopped here. Pull request open, unmerged.</p>
              <div class="orbit-dash-actions">
                <span>comment</span>
                <span class="is-primary">approve</span>
                <span>reject</span>
              </div>
            </div>
          </div>
          <div class="orbit-dash-dock">
            <div class="orbit-dash-dock-head">
              <span class="is-on">Status</span>
              <span>Log</span>
            </div>
            <p>Locked files · 0 files / 0 tasks</p>
          </div>
        </div>
      </div>
    </div>
    <figcaption class="orbit-dash-caption">Illustration of the operator dashboard (<code>orbit web serve</code>), not a screenshot. Chrome follows the current Tasks view: left rail, task list, Status dock. The selected row is the walkthrough example after a default PR ship. Identifiers are placeholders, not captured from a live host.</figcaption>
  </figure>
</section>

<h2 class="orbit-section-title">One task, one pull request</h2>

<p class="orbit-section-lede">The default path, with one example. Create the work, run an agent, inspect the result, then review the pull request. The task stops in <code>review</code> with the PR unmerged. Approving the task does not merge the pull request.</p>

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
  <a class="orbit-card" data-tag="04" href="/how-to/task-lifecycle/">
    <h3>Review the pull request</h3>
    <p>Look at the diff, CI, and execution summary. Approving moves the task from <code>review</code> to <code>done</code> — it does not merge the PR by itself.</p>
    <div class="orbit-card-cmd">orbit task update "$TASK_ID" --approve</div>
  </a>
</div>

<p class="orbit-walk-next">Next: <a href="/getting-started/install/">install Orbit</a>, then <a href="/getting-started/first-task/">write your first task</a>.</p>

<h2 class="orbit-section-title">Other delivery modes</h2>

<p class="orbit-section-lede">The walkthrough above is <code>orbit run ship</code>. These other shapes change where the run stops and who authorizes the last step. Every <code>orbit run</code> command is asynchronous: it prints a durable run ID and returns without knowing the outcome. Follow up with <code>orbit run show</code>.</p>

<div class="orbit-flow">
  <div class="orbit-flow-tabs" role="radiogroup" aria-label="Delivery mode">
    <label class="orbit-flow-tab">
      <input type="radio" name="orbit-flow" id="orbit-flow-pr" value="pr" checked />
      <span class="orbit-flow-tab-name">One task, one PR</span>
      <span class="orbit-flow-tab-cmd">run ship</span>
    </label>
    <label class="orbit-flow-tab">
      <input type="radio" name="orbit-flow" id="orbit-flow-local" value="local" />
      <span class="orbit-flow-tab-name">One task, merged locally</span>
      <span class="orbit-flow-tab-cmd">--mode local</span>
    </label>
    <label class="orbit-flow-tab">
      <input type="radio" name="orbit-flow" id="orbit-flow-auto" value="auto" />
      <span class="orbit-flow-tab-name">A bounded window</span>
      <span class="orbit-flow-tab-cmd">run auto</span>
    </label>
    <label class="orbit-flow-tab">
      <input type="radio" name="orbit-flow" id="orbit-flow-sweep" value="sweep" />
      <span class="orbit-flow-tab-name">Unattended sweep</span>
      <span class="orbit-flow-tab-cmd">run ship-sweep</span>
    </label>
  </div>

  <div class="orbit-flow-panels">
    <section class="orbit-flow-panel" data-flow="pr" aria-label="One task, one PR">
      <div class="orbit-flow-main">
        <div class="orbit-flow-cmd">
          <span class="orbit-flow-cmd-prompt" aria-hidden="true">$</span>
          <code>orbit run ship "$TASK_ID"</code>
          <button class="orbit-copy" type="button" data-copy="orbit run ship &quot;$TASK_ID&quot;" hidden>
            <span class="orbit-copy-label">Copy</span><span class="orbit-sr-only"> the ship command</span>
          </button>
          <span class="orbit-copy-status" role="status"></span>
        </div>
        <p>The default. The gated pipeline reserves the task's declared file scope, runs the agent in an isolated worktree, then opens or updates a pull request.</p>
      </div>
      <dl class="orbit-flow-facts">
        <div>
          <dt>Stops at</dt>
          <dd><code>review</code>, with the pull request open and unmerged.</dd>
        </div>
        <div>
          <dt>Base branch</dt>
          <dd><code>[workflow] base_branch</code> from <code>config.toml</code>, or <code>main</code> when unset. Override with <code>--base</code>.</dd>
        </div>
        <div>
          <dt>Authorized completion</dt>
          <dd>Separate and explicit. <code>orbit run ship "$TASK_ID" --complete</code> lets that one run move the task to <code>done</code>, once the PR is verified merged and branch protections and required checks are respected.</dd>
        </div>
      </dl>
      <a class="orbit-flow-link" href="/how-to/task-lifecycle/">Run a task lifecycle →</a>
    </section>
    <section class="orbit-flow-panel" data-flow="local" aria-label="One task, merged locally">
      <div class="orbit-flow-main">
        <div class="orbit-flow-cmd">
          <span class="orbit-flow-cmd-prompt" aria-hidden="true">$</span>
          <code>orbit run ship "$TASK_ID" --mode local</code>
          <button class="orbit-copy" type="button" data-copy="orbit run ship &quot;$TASK_ID&quot; --mode local" hidden>
            <span class="orbit-copy-label">Copy</span><span class="orbit-sr-only"> the local ship command</span>
          </button>
          <span class="orbit-copy-status" role="status"></span>
        </div>
        <p>Delivers in place, with no pull request. The run commits and merges to the configured base <em>before</em> the task reaches <code>review</code>, and may push that base as part of the same delivery.</p>
      </div>
      <dl class="orbit-flow-facts">
        <div>
          <dt>Stops at</dt>
          <dd><code>review</code> — but the merge has already happened, so this is not a pre-merge stop.</dd>
        </div>
        <div>
          <dt>When you omit <code>--mode</code></dt>
          <dd>The mode comes from the workspace's registry entry, falling back to <code>pr</code>.</dd>
        </div>
        <div>
          <dt>Authorized completion</dt>
          <dd>Still separate. <code>--complete</code> moves the task to <code>done</code> once the work is merged and pushed.</dd>
        </div>
      </dl>
      <a class="orbit-flow-link" href="/getting-started/workflows/">Compare the run surface →</a>
    </section>
    <section class="orbit-flow-panel" data-flow="auto" aria-label="A bounded window">
      <div class="orbit-flow-main">
        <div class="orbit-flow-cmd">
          <span class="orbit-flow-cmd-prompt" aria-hidden="true">$</span>
          <code>orbit run auto --for 4h --concurrency 8</code>
          <button class="orbit-copy" type="button" data-copy="orbit run auto --for 4h --concurrency 8" hidden>
            <span class="orbit-copy-label">Copy</span><span class="orbit-sr-only"> the auto drain command</span>
          </button>
          <span class="orbit-copy-status" role="status"></span>
        </div>
        <p>Drains the workspace backlog for a time-bounded window. The drain re-lists the backlog every pass and keeps <code>--concurrency</code> tasks in flight, starting a replacement as each one finishes rather than waiting for a batch.</p>
      </div>
      <dl class="orbit-flow-facts">
        <div>
          <dt>The window</dt>
          <dd><code>--for</code> bounds only the <em>start</em> of new work. A task already being shipped when it expires still finishes. Without <code>--for</code>, the run takes one tick and stops.</dd>
        </div>
        <div>
          <dt>Parallelism</dt>
          <dd>Defaults to 5. An epic root runs alongside the leaves, one at a time. Check first with <code>orbit run readiness</code>, which reserves and submits nothing.</dd>
        </div>
        <div>
          <dt>Authorized completion</dt>
          <dd><code>--complete</code> here is blanket authorization for every task the drain admits during the whole window — not just the backlog visible when you started it.</dd>
        </div>
      </dl>
      <a class="orbit-flow-link" href="/how-to/continuous-delivery/">Run a continuous delivery window →</a>
    </section>
    <section class="orbit-flow-panel" data-flow="sweep" aria-label="Unattended sweep">
      <div class="orbit-flow-main">
        <div class="orbit-flow-cmd">
          <span class="orbit-flow-cmd-prompt" aria-hidden="true">$</span>
          <code>orbit run ship-sweep --dry-run</code>
          <button class="orbit-copy" type="button" data-copy="orbit run ship-sweep --dry-run" hidden>
            <span class="orbit-copy-label">Copy</span><span class="orbit-sr-only"> the ship sweep command</span>
          </button>
          <span class="orbit-copy-status" role="status"></span>
        </div>
        <p>Dispatches a ship run in every registered workspace that has ready backlog tasks. Only workspaces with <code>[workflow] auto_ship = true</code> are swept; everything else is reported as skipped.</p>
      </div>
      <dl class="orbit-flow-facts">
        <div>
          <dt>Intended for</dt>
          <dd>A scheduler. Routines fire it on the <code>orbit sweep</code> clock, which is also what mints recurring auto-tasks.</dd>
        </div>
        <div>
          <dt>Start read-only</dt>
          <dd><code>--dry-run</code> reports what would be swept. Drop it once the selection looks right.</dd>
        </div>
        <div>
          <dt>Authorized completion</dt>
          <dd>Never available here. <code>--complete</code> is off unless you pass it on an invocation, and no workspace setting, environment variable, or unattended routine turns it on.</dd>
        </div>
      </dl>
      <a class="orbit-flow-link" href="/how-to/recurring-work/">Schedule recurring work →</a>
    </section>
  </div>
</div>

<h2 class="orbit-section-title">Why Orbit</h2>

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
    <p>Worktree isolation, file-scope locks, and OS sandboxes (<code>sandbox-exec</code>, <code>bwrap</code>) keep parallel agents from colliding.</p>
  </div>
</div>

<h2 class="orbit-section-title">Go further</h2>

<div class="orbit-card-grid orbit-card-grid-4">
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
  <a class="orbit-card" href="/reference/cli/">
    <h3>CLI reference</h3>
    <p>Every command surface, with the flags, defaults, and JSON output shapes each one accepts.</p>
    <div class="orbit-card-cmd">orbit --help</div>
  </a>
</div>

<h2 class="orbit-section-title">Explore the docs</h2>

<div class="orbit-docs-index">
  <div class="orbit-docs-group">
    <h3 class="orbit-docs-group-title">Getting Started</h3>
    <a href="/getting-started/install/">Install Orbit</a>
    <a href="/getting-started/first-task/">First Task</a>
    <a href="/getting-started/workflows/">Delivery Workflows</a>
  </div>
  <div class="orbit-docs-group">
    <h3 class="orbit-docs-group-title">Concepts</h3>
    <a href="/concepts/tasks/">Tasks</a>
    <a href="/concepts/activities-jobs/">Activities and Jobs</a>
    <a href="/concepts/policies/">Policies</a>
    <a href="/concepts/agents/">Agents</a>
  </div>
  <div class="orbit-docs-group">
    <h3 class="orbit-docs-group-title">How-to Guides</h3>
    <a href="/how-to/task-lifecycle/">Run a Task Lifecycle</a>
    <a href="/how-to/dashboard/">Use the Dashboard</a>
    <a href="/how-to/continuous-delivery/">Run Continuous Delivery</a>
    <a href="/how-to/recurring-work/">Schedule Recurring Work</a>
    <a href="/how-to/task-publication/">Publish and Restore Tasks</a>
    <a href="/how-to/write-activity/">Write an Activity</a>
    <a href="/how-to/scoping-rules/">Choose Scopes</a>
    <a href="/how-to/mcp-integration/">Set Up MCP</a>
  </div>
  <div class="orbit-docs-group">
    <h3 class="orbit-docs-group-title">Reference</h3>
    <a href="/reference/cli/">CLI Commands</a>
    <a href="/reference/activity-job-yaml/">Activity and Job YAML</a>
    <a href="/reference/policy-format/">Policy Format</a>
    <a href="/reference/config/">Configuration</a>
    <a href="/reference/scoping/">Scoping Rules</a>
  </div>
  <div class="orbit-docs-group">
    <h3 class="orbit-docs-group-title">Contributing</h3>
    <a href="/contributing/local-dev/">Local Development</a>
    <a href="/contributing/crate-layout/">Crate Layout</a>
    <a href="/contributing/pr-workflow/">PR Workflow</a>
  </div>
</div>

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
