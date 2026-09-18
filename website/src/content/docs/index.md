---
title: What Orbit Is
description: "Your coding agent files the task over MCP, Orbit ships it in an isolated worktree with declared file scope and a gated pipeline, and you review the pull request. Every mutation lands in a joined audit record. Local-first, bring your own provider CLI."
template: splash
prev: false
next: false
---

<div class="orbit-landing not-content">

<section class="orbit-hero">
  <div class="orbit-hero-copy">
    <div class="orbit-hero-eyebrow">early access</div>
    <h1 class="orbit-hero-headline">Your agent files the work. Orbit ships it. You review the pull request.</h1>
    <p class="orbit-hero-lede">Say what you want in the agent you already use. Over MCP it files a task with acceptance criteria, Orbit runs it in an isolated worktree, and a pull request comes back for you to review. Every mutation lands in a joined audit record. Local-first, driving the provider CLI you already have.</p>
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
      <a class="orbit-button" href="/how-to/mcp-integration/">Connect your agent</a>
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
    <div class="orbit-session-frame" role="img" aria-label="Illustrative session between you, your agent, and Orbit. You ask for the fsProfile lookup to be documented. The agent calls orbit.task.add and Orbit creates a task in proposed. The agent asks whether to approve and ship; you say yes. The agent calls orbit.task.update, which moves the task from proposed to backlog, then orbit.workflow.ship, and Orbit returns a run ID with the task's file scope reserved in an isolated worktree. The agent calls orbit.workflow.run.show: plan, execute and review settled, a pull request opened, and the task in review. The agent tells you the pull request is open and that the diff and the merge are yours.">
      <div class="orbit-session-bar" aria-hidden="true">
        <span class="orbit-session-dots"><span></span><span></span><span></span></span>
        <span class="orbit-session-name">example-repo — you, your agent, and Orbit</span>
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
    <figcaption class="orbit-session-caption">Illustrative session, not captured output. Identifiers are placeholders; tool names are real, arguments omitted.</figcaption>
  </figure>
</section>

<section class="orbit-section">
  <div class="orbit-section-head">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">One conversation, one pull request</p>
      <h2 class="orbit-section-heading">From a sentence to a pull request you review, without leaving your agent.</h2>
    </div>
    <p class="orbit-section-lede">Your agent files the task, asks for the go-ahead, ships it, and reports back. The task stops in <code>review</code> with the PR open and unmerged. Orbit and GitHub stay separate: approving the task never merges the PR, and merging the PR never completes the task.</p>
  </div>

  <ol class="orbit-rail" aria-label="Task lifecycle">
    <li><span class="orbit-rail-state">proposed</span></li>
    <li><span class="orbit-rail-state">backlog</span></li>
    <li><span class="orbit-rail-state">in-progress</span></li>
    <li class="is-stop"><span class="orbit-rail-state">review</span><span class="orbit-rail-note">ship stops here · PR open, not merged</span></li>
    <li class="is-later"><span class="orbit-rail-state">done</span></li>
  </ol>

  <div class="orbit-card-grid orbit-card-grid-4">
    <a class="orbit-card" data-tag="01" href="/how-to/mcp-integration/">
      <h3>Say what you want</h3>
      <p>Describe the change in the agent you already use. One command registers Orbit's operator-authorized MCP server with it; the bare agent surface can file and read tasks but never dispatch.</p>
      <div class="orbit-card-cmd">orbit workspace init --mcp</div>
    </a>
    <a class="orbit-card" data-tag="02" href="/concepts/tasks/">
      <h3>The agent files it</h3>
      <p>A title, a complexity, and acceptance criteria: the finish line the executing agent self-evaluates against. New tasks land in <code>proposed</code>, so nothing runs yet.</p>
      <div class="orbit-card-cmd">orbit.task.add { title, complexity, … }</div>
    </a>
    <a class="orbit-card" data-tag="03" href="/how-to/task-lifecycle/">
      <h3>You say go, it ships</h3>
      <p>Approval into <code>backlog</code> is a separate write, so the agent asks before taking it. Then it submits the ship workflow: file scope reserved, isolated worktree, gates, pull request. It reads the run back the same way.</p>
      <div class="orbit-card-cmd">orbit.workflow.ship { task_ids }</div>
    </a>
    <a class="orbit-card is-stop" data-tag="04" href="/how-to/task-lifecycle/">
      <h3>You review the pull request</h3>
      <p>Read the diff, CI, and the execution summary, then merge on your terms. Moving the task from <code>review</code> to <code>done</code> is a separate approval. GitHub approvals and merges never touch it.</p>
      <div class="orbit-card-cmd">orbit task update "$TASK_ID" --approve</div>
    </a>
  </div>

  <p class="orbit-walk-next">Next: <a href="/getting-started/install/">install Orbit</a> and <a href="/how-to/mcp-integration/">connect your agent</a>. Prefer to drive it by hand? <a href="/getting-started/first-task/">First task</a> runs the same sequence from the CLI.</p>
</section>

<section class="orbit-section">
  <div class="orbit-section-head">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">When you are not in the loop</p>
      <h2 class="orbit-section-heading">Same pipeline, running unattended. You choose where it stops and who authorizes the last step.</h2>
    </div>
    <p class="orbit-section-lede">Agents and auto-tasks keep filing work; these are the ways it gets shipped without you at the keyboard. Every <code>orbit run</code> command is asynchronous: it prints a durable run ID and returns before the outcome is known. Finishing delivery is always a separate, explicit authorization: <code>--complete</code> on the command, or the <code>complete</code> right on a grant.</p>
  </div>

  <div class="orbit-modes">
    <table>
      <thead>
        <tr>
          <th scope="col">Shape</th>
          <th scope="col">Command</th>
          <th scope="col">Stops at</th>
          <th scope="col">Completing delivery</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <th scope="row"><a href="/how-to/task-lifecycle/">One task, one PR</a><span class="orbit-modes-sub">the default · <code>orbit.workflow.ship</code> over MCP</span></th>
          <td><code>orbit run ship "$TASK_ID"</code></td>
          <td><em>review</em>, with the pull request open and not merged. The base branch is <code>[workflow] base_branch</code> from <code>config.toml</code>, or <code>main</code> when unset; <code>--base</code> overrides it.</td>
          <td><code>--complete</code> merges the PR as soon as GitHub allows it and moves the task to <em>done</em> once the merge is verified. Only branch protection holds it back: with no required checks it does not wait for CI.</td>
        </tr>
        <tr>
          <th scope="row"><a href="/getting-started/workflows/">One task, merged locally</a></th>
          <td><code>orbit run ship "$TASK_ID" --mode local</code></td>
          <td><em>review</em>, but the work is already merged into the base branch, so this is not a pre-merge stop. No pull request. With <code>--mode</code> omitted, the workspace's registry entry decides, defaulting to <code>pr</code>.</td>
          <td><code>--complete</code> moves the task to <em>done</em> once the work is merged and pushed.</td>
        </tr>
        <tr>
          <th scope="row"><a href="/how-to/continuous-delivery/">A bounded window</a></th>
          <td><code>orbit run auto --for 4h --concurrency 8</code></td>
          <td>When the window closes. <code>--for</code> only stops new work from starting; a task already being shipped still finishes. Concurrency defaults to 5. Check first with <code>orbit run readiness</code>, which reserves and submits nothing.</td>
          <td><code>--complete</code> covers every task the drain admits during the window, not just the backlog visible when you started it.</td>
        </tr>
        <tr>
          <th scope="row"><a href="/how-to/continuous-delivery/#running-under-an-operation-mode-grant">A scoped grant</a><span class="orbit-modes-sub">a finite task set</span></th>
          <td><code>orbit operation enable --task "$IDS" \</code><br><code>&nbsp;&nbsp;--for 2h --right prepare,promote</code><br><code>orbit run auto --grant "$GRANT_ID"</code></td>
          <td>When the grant's window closes or its tasks run out, whichever is first. At most 50 tasks and 24 hours. <code>stop</code> halts new admissions; <code>revoke</code> also strips admitted work of privileged actions.</td>
          <td>Only when the grant carries the <code>complete</code> right. <code>--complete</code> is refused alongside <code>--grant</code>; the grant is the authorization.</td>
        </tr>
        <tr>
          <th scope="row"><a href="/how-to/recurring-work/">Unattended sweep</a><span class="orbit-modes-sub">for a scheduler</span></th>
          <td><code>orbit run ship-sweep --dry-run</code></td>
          <td>One ship run in every registered workspace with <code>[workflow] auto_ship = true</code>; every other workspace is reported as skipped. Routines fire it on the <code>orbit sweep</code> clock. Start with <code>--dry-run</code>, then drop it.</td>
          <td>Never. No workspace setting, environment variable, or routine can turn <code>--complete</code> on for an unattended sweep.</td>
        </tr>
      </tbody>
    </table>
  </div>
</section>

<section class="orbit-section">
  <div class="orbit-section-head orbit-section-head-single">
    <div class="orbit-section-intro">
      <p class="orbit-section-eyebrow">Why Orbit</p>
      <h2 class="orbit-section-heading">Parallel agents without giving up rigor.</h2>
    </div>
  </div>

  <div class="orbit-card-grid orbit-why">
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 5h11"/><path d="M4 12h11"/><path d="M4 19h7"/><path d="m16 18 2 2 4-4"/></svg></div>
      <div class="orbit-card-body">
        <h3>Auditable</h3>
        <p>Task changes, workflow events, provider turns, and tool calls land in one joined audit record, redacted at write time.</p>
        <div class="orbit-card-cmd">orbit task show "$TASK_ID"</div>
      </div>
    </div>
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3.5"/><path d="M2 12h6.5"/><path d="M15.5 12H22"/></svg></div>
      <div class="orbit-card-body">
        <h3>Intent-attributed</h3>
        <p>Every workflow commit carries its task ID, so <code>git log --grep</code> links code history back to the task that asked for it.</p>
        <div class="orbit-card-cmd">git log --grep "$TASK_ID"</div>
      </div>
    </div>
    <div class="orbit-card orbit-card-row">
      <div class="orbit-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01"/><path d="M7 16.5h.01"/></svg></div>
      <div class="orbit-card-body">
        <h3>Local-first</h3>
        <p>Task and run state stay on your machine. Model traffic goes through the provider CLI and the accounts you already have.</p>
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
      <p>Prepare and approve a backlog, check readiness, then authorize a bounded drain and recover it safely.</p>
      <div class="orbit-card-cmd">orbit run readiness</div>
    </a>
    <a class="orbit-card" href="/how-to/recurring-work/">
      <h3>Recurring work</h3>
      <p>Routines run jobs on a cadence and auto-tasks mint recurring chores. Both ride the sweep clock.</p>
      <div class="orbit-card-cmd">orbit sweep --dry-run</div>
    </a>
    <a class="orbit-card" href="/how-to/task-publication/">
      <h3>Publication and recovery</h3>
      <p>Push a validated snapshot of a workspace's tasks to a Git repository you control, then verify or restore it.</p>
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
