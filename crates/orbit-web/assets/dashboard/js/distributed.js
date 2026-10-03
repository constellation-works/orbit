// Orbit dashboard distributed-drain provenance and owner handoff actions
// (ORB-12516). Pure vanilla JS ES module, no build step.
//
// This module renders claim provenance into the *existing* task and run views
// and offers the owner's approve / revoke / recover actions there. It is not a
// distributed console: there is no tab, no route and no navigation entry, so the
// incomplete feature gains no public surface of its own — the panel appears only
// where the owner actually holds a claim for the task being looked at.
//
// Three rules shape every string below.
//
//  1. Unknown stays unknown. A run or artifact recorded before execution
//     provenance existed reads as "unknown", never as "the owner".
//  2. A diagnostic is not a verdict. An elapsed reservation, a claim's age and
//     an absent owner-local run are things to look at, not proof the attempt
//     died and not revocation. V1 has manual reclamation for exactly that
//     reason.
//  3. Say what actually happened. `review` means a delivery handoff is waiting
//     for completion authority, not that a reviewer approved anything; merged
//     means the candidate is on the landing branch, not that it is deployed.
//
// The buttons are a convenience, never the boundary: every action carries the
// exact identity the operator was shown, and the owner re-decides authority,
// currentness and merge certainty inside the transaction that would change
// anything.

import { captureWorkspaceVisit, el, fetchJson, postJson, makeToggleRow, isAggregateView, getWorkspaceRevision } from './common.js';

const CONSOLE_PATH = "/api/distributed/claims";

// One read per short window, shared by however many rows are expanded. The
// memo expires after CONSOLE_TTL_MS so a detail that is opened or rebuilt later
// (the dashboard refreshes tasks on its own timer and never re-reads claims)
// shows current authority rather than a panel hours out of date. It is also
// invalidated on every action and workspace change, so a decision is never
// rendered from the state that preceded it.
export const CONSOLE_TTL_MS = 10000;
let cachedConsole = null;
let cachedAt = 0;
let inflightConsole = null;
// The outcome of the operator's last action, per task. It outlives the block
// that reported it: the detail is rebuilt once the action changes the task, and
// a message written only to the old container would be lost with it.
let lastFeedback = null;

/// Drop the memoized read. Called after any owner action and by the workspace
/// selector, since claim state is per-workspace.
export function invalidateDistributedConsole() {
  cachedConsole = null;
  cachedAt = 0;
  inflightConsole = null;
  lastFeedback = null;
}

function freshConsole() {
  return cachedConsole && Date.now() - cachedAt < CONSOLE_TTL_MS ? cachedConsole : null;
}

export function peekDistributedConsole() {
  return freshConsole();
}

export function loadDistributedConsole({ force = false } = {}) {
  // Claim state is per-workspace. The aggregate view has no concrete workspace
  // to scope to — the endpoint would 400 — so answer as "nothing here" rather
  // than painting a failure across every task detail.
  if (isAggregateView()) return Promise.resolve(null);
  if (force) invalidateDistributedConsole();
  const fresh = freshConsole();
  if (fresh) return Promise.resolve(fresh);
  if (!inflightConsole) {
    // Revision plus request identity: a response issued for workspace A must
    // not populate the memo after a switch (or a forced re-read) the way
    // requestPanel rejects A→B→A and overlapping refreshes.
    const revision = getWorkspaceRevision();
    const request = fetchJson(CONSOLE_PATH)
      .then((payload) => {
        const body = payload || {};
        if (inflightConsole !== request || revision !== getWorkspaceRevision()) {
          return body;
        }
        cachedConsole = body;
        cachedAt = Date.now();
        inflightConsole = null;
        return cachedConsole;
      })
      .catch((error) => {
        if (inflightConsole === request) inflightConsole = null;
        throw error;
      });
    inflightConsole = request;
  }
  return inflightConsole;
}

export function claimsForTask(payload, taskId) {
  const claims = payload && Array.isArray(payload.claims) ? payload.claims : [];
  return claims.filter((claim) => claim && claim.task_id === taskId);
}

// --- shared renderers -------------------------------------------------------

/// Machine-qualified execution provenance, or an explicit unknown.
///
/// `machine_id` is the stable identity; `machine_name` rides along for display
/// and may be renamed, so it is never shown alone.
export function formatExecutionLocation(location) {
  if (!location || location.known !== true || !location.machine_id) {
    return "unknown — recorded before execution provenance was tracked";
  }
  return location.machine_name
    ? `machine ${location.machine_id} · name ${location.machine_name}`
    : `machine ${location.machine_id}`;
}

/// The execution-provenance cell shared by the run meta grid and the task
/// detail's run line.
export function buildExecutionProvenance(location) {
  const known = !!(location && location.known === true && location.machine_id);
  return el("span", {
    class: known ? "exec-origin" : "exec-origin unknown",
    text: formatExecutionLocation(location),
    title: known
      ? "Where this ran. Nothing is inferred from hostname, cwd or SSH target."
      : "No execution machine was recorded for this row. Unknown is not the owner.",
  });
}

function line(label, value, opts = {}) {
  const row = el("div", { class: opts.class ? `claim-line ${opts.class}` : "claim-line" }, [
    el("span", { class: "label", text: label }),
    typeof value === "string" || value == null
      ? el("span", { class: "value", text: value == null ? "—" : value })
      : value,
  ]);
  if (opts.note) row.appendChild(el("div", { class: "claim-note", text: opts.note }));
  return row;
}

function shortCommit(value) {
  return typeof value === "string" && value.length > 12 ? value.slice(0, 12) : (value || "—");
}

function shortId(value) {
  return typeof value === "string" && value.length > 12 ? value.slice(0, 8) : (value || "?");
}

// Known enums read as words; an unknown value stays legible (underscores
// become spaces) rather than being hidden or mislabelled. The server's own
// summary rides along as the note, so nothing here restates what it says.
const PHASE_LABELS = {
  claimed: "claimed",
  running: "running",
  handed_off: "handed off",
  failed: "failed",
  revoked: "revoked",
  landed: "landed",
};
const EVENT_LABELS = {
  claim_bound: "run bound",
  claim_evidence: "evidence recorded",
  claim_failed: "attempt failed",
  claim_friction: "friction reported",
  claim_handed_off: "handed off",
  claim_merge_intent: "merge intent recorded",
  claim_revoked: "claim revoked",
  claim_updated: "claim updated",
  handoff_accepted: "handoff accepted",
  handoff_approved: "handoff approved",
  handoff_revoked: "authority revoked",
  landing_completed: "landing completed",
  landing_dispatched: "landing dispatched",
  landing_stopped: "landing stopped",
};
const AUTHORITY_LABELS = {
  not_authorized: "awaiting approval",
  authorized: "approved",
  revoked: "revoked",
  completed: "consumed by merge",
};
const LANDING_LABELS = {
  none: "not started",
  dispatched: "in progress",
  merged: "merged",
  stopped: "stopped",
};
const REVIEW_LABELS = { not_required: "not required" };

function humanize(value) {
  return String(value).replace(/_/g, " ");
}

function enumLabel(map, value) {
  if (value == null || value === "") return "—";
  return Object.prototype.hasOwnProperty.call(map, value) ? map[value] : humanize(value);
}

/// Times go through the caller's formatter (the task detail's own) so a claim
/// reads in the same zone and shape as every other timestamp on the page. A
/// missing value is `null`, never the string "undefined".
function formatWhen(options, value) {
  if (!value) return null;
  return options && typeof options.formatTime === "function" ? options.formatTime(value) : String(value);
}

// --- claim panel ------------------------------------------------------------

/// Render one claim: provenance, phase, footprint, reservation, handoff and the
/// owner actions the caller wires.
///
/// Pure with respect to the DOM it creates — it reads only its arguments — so
/// the behavior scenarios can drive it without a server. `options` carries the
/// action handlers and an optional `formatTime`.
export function buildClaimPanel(claim, capabilities, options = {}) {
  const panel = el("div", { class: "claim-panel" });
  panel.setAttribute("data-claim-id", claim.claim_id || "");
  panel.setAttribute("data-phase", claim.phase || "");

  // A task can hold several claims (a retry after recovery); the id and birth
  // time are what tell them apart.
  const created = formatWhen(options, claim.created_at);
  panel.appendChild(
    el("div", { class: "claim-header" }, [
      el("span", { class: "claim-id mono", text: `claim ${shortId(claim.claim_id)}`, title: claim.claim_id || "" }),
      created ? el("span", { class: "claim-created", text: `created ${created}` }) : null,
    ]),
  );

  panel.appendChild(line("execution", buildExecutionProvenance(claim.executed_on)));

  const run = claim.bound_run;
  if (run && run.run_id) {
    if (claim.bound_run_navigable) {
      const link = el("a", { class: "value", text: run.run_id });
      link.href = `#runs?run_id=${encodeURIComponent(run.run_id)}`;
      panel.appendChild(line("bound run", link));
    } else {
      // No owner-local run exists for a run that executed elsewhere. Naming the
      // host is the honest answer; an owner-local link would resolve against
      // this checkout's job store and open the wrong thing or nothing.
      panel.appendChild(
        line("bound run", `${run.run_id} on machine ${run.machine_id || "unknown"}`, {
          class: "remote",
          note: claim.inspect_on || "inspect this run on its execution machine",
        }),
      );
    }
  } else {
    panel.appendChild(
      line("bound run", "none yet", {
        note: "a claim exists before its leaf run does; an absent run is not proof the attempt died",
      }),
    );
  }

  panel.appendChild(line("phase", enumLabel(PHASE_LABELS, claim.phase), { note: claim.phase_summary }));
  panel.appendChild(line("last event", enumLabel(EVENT_LABELS, claim.last_event)));

  panel.appendChild(buildReservationLine(claim, options));

  const footprint = Array.isArray(claim.footprint) ? claim.footprint : [];
  const files = el("div", { class: "claim-footprint" });
  for (const entry of footprint) files.appendChild(el("div", { class: "mono", text: entry }));
  panel.appendChild(
    line("footprint", files, {
      note: claim.footprint_protected
        ? `${footprint.length} frozen selector(s), still protected`
        : `${footprint.length} selector(s), no longer protecting files`,
    }),
  );

  if (claim.landing_invalidated) {
    panel.appendChild(
      el("div", { class: "claim-warning", text: "landing authority for this claim was invalidated" }),
    );
  }
  if (claim.unresolved_merge_intent) {
    panel.appendChild(uncertainMergeBanner(claim.unresolved_merge_intent));
  }

  if (claim.handoff) panel.appendChild(buildHandoffPanel(claim.handoff));

  const actions = buildClaimActions(claim, capabilities, options);
  if (actions) panel.appendChild(actions);
  return panel;
}

/// The reservation window only means something while the claim is live. Once a
/// claim has landed, failed or been revoked its reservation is released, so an
/// elapsed timestamp there is history, not a warning — and the server's "still
/// live" note would be false.
function buildReservationLine(claim, options) {
  const reservation = claim.reservation || {};
  if (claim.unsettled !== true) {
    return line("reservation", "released", { class: "released" });
  }
  const expiresAt = formatWhen(options, reservation.expires_at);
  const when = expiresAt ? ` ${expiresAt}` : "";
  const text = reservation.expired
    ? `expired${when}`
    : expiresAt ? `expires ${expiresAt}` : "no expiry recorded";
  return line("reservation", text, {
    class: reservation.expired ? "expired" : "",
    note: reservation.note,
  });
}

/// An external merge whose reply was lost. Until it is reconciled against the
/// provider, neither revocation nor recovery may proceed: the row in the
/// database cannot cancel a request the provider may already have applied.
function uncertainMergeBanner(intentId) {
  return el("div", { class: "claim-warning uncertain-merge" }, [
    el("span", { class: "label", text: "uncertain merge intent" }),
    el("span", { class: "value mono", text: intentId }),
    el("div", {
      class: "claim-note",
      text: "a merge was recorded as sent and its outcome is unknown; reconcile it against the provider before revoking or recovering — revocation and recovery are refused until then",
    }),
  ]);
}

function buildHandoffPanel(handoff) {
  const wrap = el("div", { class: "handoff-panel" });
  wrap.setAttribute("data-handoff-id", handoff.handoff_id || "");
  wrap.appendChild(el("h5", { text: "delivery handoff" }));

  const candidate = handoff.candidate || {};
  const delivery = candidate.delivery || {};
  wrap.appendChild(line("repository", candidate.repository));
  wrap.appendChild(
    line("branches", `${candidate.source_branch || "?"} → ${candidate.base_branch || "?"} (lands on ${candidate.landing_branch || "?"})`),
  );
  wrap.appendChild(
    line(
      "candidate",
      `${shortCommit(candidate.candidate && candidate.candidate.commit)} on base ${shortCommit(candidate.base && candidate.base.commit)}`,
    ),
  );
  wrap.appendChild(line("delivery", deliveryLabel(delivery)));

  const review = handoff.review || {};
  // The typed not-required disposition, said plainly. `review` as a task status
  // means delivery is awaiting completion authority; no reviewer ran.
  wrap.appendChild(
    line("review", `${enumLabel(REVIEW_LABELS, review.disposition)} · policy ${review.policy ? humanize(review.policy) : "?"}`, {
      class: "review-disposition",
      note: review.summary || "review not required — this is not a code review",
    }),
  );

  const validation = Array.isArray(handoff.validation) ? handoff.validation : [];
  const required = Array.isArray(handoff.required_commands) ? handoff.required_commands : [];
  const logs = el("div", { class: "handoff-validation" });
  for (const entry of validation) {
    logs.appendChild(el("div", { class: "mono", text: `${entry.path} · ${shortCommit(entry.sha256)}` }));
  }
  wrap.appendChild(
    line("validation", logs, {
      note: `${validation.length} captured log(s) for ${required.length} owner-required command(s): ${required.join(", ") || "none"}`,
    }),
  );

  const authority = handoff.authority || {};
  wrap.appendChild(
    line("completion authority", enumLabel(AUTHORITY_LABELS, authority.state), {
      class: `authority-${authority.state}`,
      note: authority.summary,
    }),
  );

  const landing = handoff.landing || {};
  wrap.appendChild(
    line("landing", enumLabel(LANDING_LABELS, landing.state), {
      class: `landing-${landing.state}`,
      note: landing.summary,
    }),
  );
  if (landing.evidence) wrap.appendChild(line("landing evidence", landing.evidence));
  return wrap;
}

function deliveryLabel(delivery) {
  switch (delivery && delivery.kind) {
    case "pull_request":
      return delivery.number == null ? "pull request" : `pull request #${delivery.number}`;
    case "local_candidate":
      return "local candidate (owner performs the merge)";
    case "already_landed":
      return `already landed in ${shortCommit(delivery.covering_commit)}`;
    default:
      return "—";
  }
}

// --- owner actions ----------------------------------------------------------

function capabilityFor(capabilities, key) {
  const entry = capabilities && capabilities[key];
  return entry && typeof entry === "object" ? entry : { authorized: false, reason: "unavailable" };
}

/// Destructive owner actions ask first. A missing `window.confirm` is a
/// refusal, never an implicit yes.
function confirmed(message) {
  return typeof window !== "undefined" && typeof window.confirm === "function" && window.confirm(message);
}

const MERGE_INTENT_BLOCK =
  "unavailable until the uncertain merge is reconciled against the provider";

function buildClaimActions(claim, capabilities, handlers) {
  const handoff = claim.handoff;
  const authority = (handoff && handoff.authority) || {};
  const row = el("div", { class: "claim-actions" });
  let any = false;

  // Every control in this row, so one in-flight decision can lock them all and
  // release only the ones that were enabled to begin with.
  const controls = [];
  let busy = false;
  const setBusy = (on, active) => {
    busy = on;
    if (on) row.setAttribute("data-busy", "true");
    else row.setAttribute("data-busy", "false");
    for (const control of controls) {
      control.node.disabled = on || control.blocked;
      if (control.node.tagName === "BUTTON") {
        control.node.textContent = on && control.node === active ? "working…" : control.text;
      }
    }
  };

  // One replay identity per rendered panel and decision: a retry of the same
  // choice (same action, status and reason) replays the stored outcome, while
  // a different decision is a different request. A re-render starts fresh.
  const requestIds = new Map();
  const requestIdFor = (identity) => {
    if (!requestIds.has(identity)) requestIds.set(identity, newRequestId());
    return requestIds.get(identity);
  };

  const add = (key, text, capabilityKey, onRun, opts = {}) => {
    const capability = capabilityFor(capabilities, capabilityKey);
    any = true;
    if (!capability.authorized) {
      // Explain rather than hide: an operator who cannot act should learn why,
      // and the backend refuses the call regardless of what is rendered here.
      const reason = capability.reason || "not authorized";
      row.appendChild(
        el("span", {
          class: "claim-action-denied",
          text: `${text} unavailable — ${reason}`,
          title: reason,
        }),
      );
      return;
    }
    let input = null;
    let problem = null;
    const clearProblem = () => {
      if (input) input.setAttribute("aria-invalid", "false");
      if (problem) problem.textContent = "";
    };
    if (opts.reasonRequired && !opts.blocked) {
      input = el("input", { class: "claim-reason" });
      input.type = "text";
      input.placeholder = opts.reasonPlaceholder || "reason";
      input.setAttribute("aria-label", `${text} reason`);
      input.setAttribute("data-reason-for", key);
      input.addEventListener("input", clearProblem);
      row.appendChild(input);
      controls.push({ node: input, blocked: false });
    }

    const choices = opts.statusChoices || [{ value: null, label: text }];
    for (const choice of choices) {
      const button = el("button", { class: `claim-action ${key}`, text: choice.label });
      button.type = "button";
      button.setAttribute("data-action", key);
      if (choice.value) button.setAttribute("data-status", choice.value);
      const blocked = !!opts.blocked;
      button.disabled = blocked;
      controls.push({ node: button, blocked, text: choice.label });
      button.addEventListener("click", () => {
        if (button.disabled || busy) return;
        const reason = input ? input.value : null;
        // The owner refuses a decision with no reason, but only after the
        // confirmation below has been answered and a replay identity spent.
        // Ask for the reason first, where the operator is already looking.
        if (input && !String(reason).trim()) {
          if (!problem) {
            problem = el("div", { class: "claim-feedback error" });
            problem.setAttribute("role", "alert");
            row.appendChild(problem);
          }
          problem.textContent = `${text} needs a reason.`;
          input.setAttribute("aria-invalid", "true");
          input.focus();
          return;
        }
        clearProblem();
        if (opts.confirm && !confirmed(opts.confirm(reason, choice.value))) return;
        setBusy(true, button);
        const requestId = requestIdFor(`${key}|${choice.value || ""}|${reason || ""}`);
        // An async wrapper: the request starts now, and a synchronous throw in
        // a handler still releases the row.
        (async () => onRun(reason, choice.value, requestId))()
          .catch(() => {})
          .then(() => setBusy(false));
      });
      row.appendChild(button);
    }
    if (opts.blocked) {
      // Visible text, not only a tooltip: a disabled button with no stated
      // reason reads as broken.
      row.appendChild(el("span", { class: "claim-action-denied", text: opts.blocked, title: opts.blocked }));
    }
  };

  if (handoff && claim.phase === "handed_off" && authority.state === "not_authorized") {
    add("approve", "Approve handoff", "handoff_approve", (_reason, _status, requestId) =>
      handlers.onApprove && handlers.onApprove(claim, requestId),
    );
  }
  if (handoff && authority.state === "authorized") {
    add(
      "revoke",
      "Revoke authority",
      "handoff_revoke",
      (reason, _status, requestId) => handlers.onRevoke && handlers.onRevoke(claim, reason, requestId),
      {
        reasonRequired: true,
        reasonPlaceholder: "why authority is withdrawn",
        blocked: claim.unresolved_merge_intent ? MERGE_INTENT_BLOCK : null,
        confirm: () =>
          `Revoke completion authority for task ${claim.task_id}?\n\nThis withdraws authority for this exact candidate, so the owner will not land it. The task stays in review.`,
      },
    );
  }
  if (claim.unsettled) {
    add(
      "recover",
      "Recover claim",
      "claim_recover",
      (reason, status, requestId) => handlers.onRecover && handlers.onRecover(claim, reason, status, requestId),
      {
        reasonRequired: true,
        reasonPlaceholder: "why this attempt is over",
        blocked: claim.unresolved_merge_intent ? MERGE_INTENT_BLOCK : null,
        statusChoices: [
          { value: "blocked", label: "Recover claim → blocked" },
          { value: "backlog", label: "Recover claim → backlog" },
        ],
        confirm: (_reason, status) =>
          `Recover claim ${shortId(claim.claim_id)} for task ${claim.task_id}?\n\nThis fences the current attempt and moves the task to ${status}. Recovery is deliberate and cannot be undone.`,
      },
    );
  }

  if (!any) return null;
  row.appendChild(
    el("div", {
      class: "claim-note",
      text: "Approving records completion authority for this exact candidate; it does not merge. Recovery fences the attempt and is never automatic — there is no heartbeat.",
    }),
  );
  return row;
}

/// A replay identity for one decision. A retried POST with the same value
/// replays the stored outcome instead of recording a second one.
function newRequestId() {
  const random = Math.random().toString(36).slice(2, 10);
  return `dash-${Date.now().toString(36)}-${random}`;
}

function expectedCandidate(claim) {
  const candidate = (claim.handoff && claim.handoff.candidate) || {};
  return {
    expected_candidate_commit: (candidate.candidate && candidate.candidate.commit) || "",
    expected_base_commit: (candidate.base && candidate.base.commit) || "",
  };
}

export function approveHandoff(claim, requestId) {
  const request = handoffApprovalRequest(claim, requestId);
  return postJson(request.path, request.body);
}

/// The owner approval a review task's handed-off claim needs: the endpoint
/// and the exact candidate the operator is approving.
export function handoffApprovalRequest(claim, requestId) {
  return {
    path: `/api/distributed/handoffs/${encodeURIComponent(claim.handoff.handoff_id)}/approve`,
    body: { ...expectedCandidate(claim), request_id: requestId || newRequestId() },
  };
}

// What a plain "approve" on a review task says when its handoff no longer
// awaits approval. The server's own summary is preferred; these only cover a
// payload without one, and each is true of exactly its state.
const DECIDED_REFUSALS = {
  authorized: "completion authority is already recorded for this handoff; the owner landing job completes the task",
  revoked: "completion authority for this handoff was withdrawn; the task stays in review",
  completed: "this handoff already landed; the merge completed the task",
};

/// How a plain "approve" on a review task must be carried out when this
/// workspace holds a claim for it. The owner refuses an unscoped status write
/// while a claim protects the task, so approving a delivered handoff goes
/// through the handoff approval instead. `null` means no claim is involved
/// and the ordinary approval applies.
export async function claimedReviewApproval(taskId) {
  const payload = await loadDistributedConsole({ force: true });
  const live = claimsForTask(payload, taskId).filter(
    (claim) => claim.phase === "handed_off" || claim.unsettled,
  );
  if (live.length === 0) return null;
  const handedOff = live.find((claim) => claim.phase === "handed_off" && claim.handoff);
  if (!handedOff) {
    return { refusal: "this task's distributed claim has not handed off yet; see distributed execution below" };
  }
  const authority = handedOff.handoff.authority || {};
  if (authority.state === "not_authorized") return { claim: handedOff };
  return {
    refusal: authority.summary || DECIDED_REFUSALS[authority.state] || `this handoff is already ${humanize(authority.state || "decided")}`,
  };
}

export function revokeHandoff(claim, reason, requestId) {
  return postJson(`/api/distributed/handoffs/${encodeURIComponent(claim.handoff.handoff_id)}/revoke`, {
    ...expectedCandidate(claim),
    reason: reason || "",
    request_id: requestId || newRequestId(),
  });
}

export function recoverClaim(claim, reason, status, requestId) {
  return postJson(`/api/distributed/claims/${encodeURIComponent(claim.claim_id)}/recover`, {
    expected_phase: claim.phase,
    status,
    reason: reason || "",
    request_id: requestId || newRequestId(),
  });
}

// --- mounting into the task detail -----------------------------------------

const FEEDBACK_TTL_MS = 20000;

/// Fill `container` with the distributed panel for `taskId`, reading the console
/// once and re-reading it after every action.
///
/// The container is left empty when this workspace holds no claim for the task,
/// which is the normal case: the block simply does not appear. A replica
/// checkout keeps it hidden too — the owner machine holds claim state, and a
/// note repeated on every task would be noise.
export function mountTaskClaimPanel(container, taskId, { onTaskChanged, onContent, formatTime } = {}) {
  const visit = captureWorkspaceVisit();
  const announce = (rendered) => {
    if (onContent) onContent(rendered);
    return rendered;
  };

  const feedbackNode = (text, kind, remedy) => {
    const note = el("div", { class: `claim-feedback ${kind}`, text });
    note.setAttribute("role", kind === "error" || kind === "uncertain" ? "alert" : "status");
    if (remedy) note.appendChild(el("div", { class: "claim-note", text: remedy }));
    return note;
  };

  const render = (payload) => {
    if (!visit.isCurrent()) return false;
    container.textContent = "";
    if (payload && payload.owner_workspace === false) return announce(false);
    const claims = claimsForTask(payload, taskId);
    // The common case: this workspace holds no claim for this task, so the
    // block does not appear at all.
    if (claims.length === 0) return announce(false);
    // An outcome reported just before the detail was rebuilt still belongs to
    // this task; show it once here rather than losing it with the old block.
    if (lastFeedback && lastFeedback.taskId === taskId && Date.now() - lastFeedback.at < FEEDBACK_TTL_MS) {
      const carried = feedbackNode(lastFeedback.text, lastFeedback.kind, lastFeedback.remedy);
      container.appendChild(carried);
      claimCarriedFocus(carried);
    }
    const capabilities = (payload && payload.capabilities) || {};
    for (const claim of claims) {
      container.appendChild(
        buildClaimPanel(claim, capabilities, {
          formatTime,
          onApprove: (target, requestId) => act(() => approveHandoff(target, requestId)),
          onRevoke: (target, reason, requestId) => act(() => revokeHandoff(target, reason, requestId)),
          onRecover: (target, reason, status, requestId) => act(() => recoverClaim(target, reason, status, requestId)),
        }),
      );
    }
    return announce(true);
  };

  // Report the outcome in this container (if it is still on the page) and
  // remember it for the block that replaces it.
  const feedback = (text, kind, remedy) => {
    if (!visit.isCurrent()) return;
    lastFeedback = { taskId, text, kind, remedy, at: Date.now() };
    for (const prior of container.querySelectorAll(".claim-feedback")) prior.remove();
    container.insertBefore(feedbackNode(text, kind, remedy), container.firstChild);
  };

  // The button that was pressed is gone once the panel repaints, and focus
  // falls to the page. Put it on the outcome (or, failing that, the first
  // control) so a keyboard or screen-reader user lands where the result is.
  const focusOutcome = () => {
    if (!visit.isCurrent()) return;
    const target = container.querySelector(".claim-feedback") || container.querySelector("button.claim-action");
    if (!target) return;
    // A note is not a tab stop, but it can take programmatic focus.
    if (target.tagName !== "BUTTON") target.tabIndex = -1;
    if (typeof target.focus === "function") target.focus();
    // The task list rebuilds this detail after the action, which takes the
    // focused note with it. The rebuilt block moves focus to its copy.
    if (lastFeedback && lastFeedback.taskId === taskId && target.classList.contains("claim-feedback")) {
      lastFeedback.focused = target;
    }
  };

  // Take focus for the rebuilt copy of an outcome, once, and only while focus
  // is still on the old note or has fallen to the page: an operator who has
  // moved on keeps their place.
  const claimCarriedFocus = (carried) => {
    const previous = lastFeedback.focused;
    if (!previous) return;
    lastFeedback.focused = null;
    // A render from the cached read runs before the caller has placed the
    // rebuilt block on the page, where it could not take focus yet.
    setTimeout(() => {
      if (!visit.isCurrent()) return;
      const active = document.activeElement;
      if (active && active !== document.body && active !== previous) return;
      carried.tabIndex = -1;
      if (typeof carried.focus === "function") carried.focus();
    }, 0);
  };

  const act = async (run) => {
    if (!visit.isCurrent()) return;
    try {
      await perform(run);
    } finally {
      focusOutcome();
    }
  };

  const perform = async (run) => {
    let body;
    try {
      body = await run();
      if (!visit.isCurrent()) return;
    } catch (error) {
      if (!visit.isCurrent()) return;
      // A refused action is the point of the backend check, not a bug in it.
      // Stale state re-reads before the operator decides again; an uncertain
      // merge is left alone until it is reconciled.
      const stale = error && (error.code === "stale_claim" || error.code === "handoff_not_current");
      const message = error && error.message ? error.message : String(error);
      const remedy = error && error.remedy ? error.remedy : null;
      if (!stale) {
        feedback(message, error && error.code === "uncertain_merge_intent" ? "uncertain" : "error", remedy);
        return;
      }
      await refresh();
      feedback(`${message} — refreshed`, "stale", remedy);
      return;
    }
    // Re-read before reporting, and before the task list is told to refresh:
    // the operator should see the decision and the state it produced together,
    // and the rebuilt detail must find the fresh read, never the pre-action one.
    const summary = actionSummary(body);
    const reread = await refresh(summary);
    if (reread) feedback(summary, "ok");
    if (visit.isCurrent() && onTaskChanged) onTaskChanged();
  };

  // A failed read is one message with a way to try again. After a successful
  // action it also carries the decision that was recorded, so the operator never
  // sees a green confirmation next to a red error, or an error that hides that
  // the action went through.
  const failed = (error, recorded) => {
    if (!visit.isCurrent()) return false;
    container.textContent = "";
    const detail = `claim state unavailable: ${error && error.message ? error.message : error}`;
    const note = feedbackNode(recorded ? `${recorded} — ${detail}` : detail, "error");
    const retry = el("button", { class: "claim-action claim-retry", text: "retry" });
    retry.type = "button";
    retry.addEventListener("click", () => {
      if (retry.disabled) return;
      retry.disabled = true;
      refresh(recorded).then(focusOutcome);
    });
    container.appendChild(note);
    container.appendChild(retry);
    return announce(true);
  };

  const refresh = (recorded) =>
    !visit.isCurrent() ? Promise.resolve(false) : loadDistributedConsole({ force: true }).then(
      (payload) => {
        render(payload);
        return true;
      },
      (error) => {
        failed(error, recorded);
        return false;
      },
    );

  const cached = peekDistributedConsole();
  if (cached) return Promise.resolve(render(cached));
  container.appendChild(el("div", { class: "claim-note", text: "reading claim state…" }));
  return loadDistributedConsole().then(render, (error) => failed(error));
}

function actionSummary(body) {
  const result = (body && body.result) || {};
  const phase = enumLabel(PHASE_LABELS, result.phase);
  const status = result.task_status ? humanize(result.task_status) : "?";
  if (result.handoff_id) return `decision recorded: claim ${phase}, task ${status}`;
  return `decision recorded: claim ${shortId(result.claim_id)} ${phase}, task ${status}`;
}

/// Collapsible wrapper matching the task detail's other field blocks, so the
/// panel is reachable by keyboard the same way every other block is.
export function buildDistributedBlock(taskId, opts = {}) {
  const block = el("div", { class: "field-block collapsible distributed-block" });
  // Hidden until the read says this task actually has a claim, so an ordinary
  // single-host task detail is unchanged.
  block.style.display = "none";
  const heading = el("h4", {}, [el("span", { class: "field-title", text: "distributed execution" })]);
  const body = el("div", { class: "claim-body" });
  makeToggleRow(heading, {
    expanded: true,
    onToggle: (event) => {
      event.stopPropagation();
      const collapsed = block.classList.toggle("collapsed");
      heading.setAttribute("aria-expanded", String(!collapsed));
    },
  });
  block.appendChild(heading);
  block.appendChild(body);
  mountTaskClaimPanel(body, taskId, {
    ...opts,
    onContent: (rendered) => {
      block.style.display = rendered ? "" : "none";
      if (opts.onContent) opts.onContent(rendered);
    },
  });
  return block;
}
