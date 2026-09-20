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

import { el, fetchJson, postJson, makeToggleRow, isAggregateView } from './common.js';

const CONSOLE_PATH = "/api/distributed/claims";

// One read per view, shared by however many rows are expanded. Invalidated on
// every action so a decision is never rendered from the state that preceded it.
let cachedConsole = null;
let inflightConsole = null;

/// Drop the memoized read. Called after any owner action and by the workspace
/// selector, since claim state is per-workspace.
export function invalidateDistributedConsole() {
  cachedConsole = null;
  inflightConsole = null;
}

export function peekDistributedConsole() {
  return cachedConsole;
}

export function loadDistributedConsole({ force = false } = {}) {
  // Claim state is per-workspace. The aggregate view has no concrete workspace
  // to scope to — the endpoint would 400 — so answer as "nothing here" rather
  // than painting a failure across every task detail.
  if (isAggregateView()) return Promise.resolve(null);
  if (force) invalidateDistributedConsole();
  if (cachedConsole) return Promise.resolve(cachedConsole);
  if (!inflightConsole) {
    inflightConsole = fetchJson(CONSOLE_PATH)
      .then((payload) => {
        cachedConsole = payload || {};
        inflightConsole = null;
        return cachedConsole;
      })
      .catch((error) => {
        inflightConsole = null;
        throw error;
      });
  }
  return inflightConsole;
}

export function claimsForTask(payload, taskId) {
  const claims = payload && Array.isArray(payload.claims) ? payload.claims : [];
  return claims.filter((claim) => claim && claim.task_id === taskId);
}

// --- shared renderers -------------------------------------------------------

/// Host-qualified execution provenance, or an explicit unknown.
///
/// `machine_id` is the stable identity; `host_id` rides along for display and
/// may be renamed, so it is never shown alone.
export function formatExecutionLocation(location) {
  if (!location || location.known !== true || !location.machine_id) {
    return "unknown — recorded before execution provenance was tracked";
  }
  return location.host_id
    ? `machine ${location.machine_id} · host ${location.host_id}`
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

// --- claim panel ------------------------------------------------------------

/// Render one claim: provenance, phase, footprint, reservation, handoff and the
/// owner actions the caller wires.
///
/// Pure with respect to the DOM it creates — it reads only its arguments — so
/// the behavior scenarios can drive it without a server.
export function buildClaimPanel(claim, capabilities, handlers = {}) {
  const panel = el("div", { class: "claim-panel" });
  panel.setAttribute("data-claim-id", claim.claim_id || "");
  panel.setAttribute("data-phase", claim.phase || "");

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
        line("bound run", `${run.run_id} on machine ${run.machine_id}`, {
          class: "remote",
          note: claim.inspect_on || "inspect this run on its execution host",
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

  panel.appendChild(line("phase", `${claim.phase} — ${claim.phase_summary || ""}`.trim()));
  panel.appendChild(line("last event", claim.last_event));

  const reservation = claim.reservation || {};
  panel.appendChild(
    line(
      "reservation",
      reservation.expired ? `expired ${reservation.expires_at}` : `expires ${reservation.expires_at}`,
      { class: reservation.expired ? "expired" : "", note: reservation.note },
    ),
  );

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

  const actions = buildClaimActions(claim, capabilities, handlers);
  if (actions) panel.appendChild(actions);
  return panel;
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
    line("branches", `${candidate.source_branch} → ${candidate.base_branch} (lands on ${candidate.landing_branch})`),
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
    line("review", `${review.disposition || "?"} (policy ${review.policy || "?"})`, {
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
    line("completion authority", authority.state, {
      class: `authority-${authority.state}`,
      note: authority.summary,
    }),
  );

  const landing = handoff.landing || {};
  wrap.appendChild(
    line("landing", landing.state, {
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
      return `pull request #${delivery.number}`;
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

function buildClaimActions(claim, capabilities, handlers) {
  const handoff = claim.handoff;
  const authority = (handoff && handoff.authority) || {};
  const row = el("div", { class: "claim-actions" });
  let any = false;

  const add = (key, label, capabilityKey, onRun, opts = {}) => {
    const capability = capabilityFor(capabilities, capabilityKey);
    any = true;
    if (!capability.authorized) {
      // Explain rather than hide: an operator who cannot act should learn why,
      // and the backend refuses the call regardless of what is rendered here.
      row.appendChild(
        el("span", {
          class: "claim-action-denied",
          text: `${label} unavailable`,
          title: capability.reason || "not authorized",
        }),
      );
      return;
    }
    const button = el("button", { class: `claim-action ${key}`, text: label });
    button.type = "button";
    button.setAttribute("data-action", key);
    if (opts.reasonRequired) {
      const input = el("input", { class: "claim-reason" });
      input.type = "text";
      input.placeholder = opts.reasonPlaceholder || "reason";
      input.setAttribute("aria-label", `${label} reason`);
      input.setAttribute("data-reason-for", key);
      row.appendChild(input);
      button.addEventListener("click", () => onRun(input.value));
    } else {
      button.addEventListener("click", () => onRun(null));
    }
    row.appendChild(button);
  };

  if (handoff && claim.phase === "handed_off" && authority.state === "not_authorized") {
    add("approve", "Approve handoff", "handoff_approve", () => handlers.onApprove && handlers.onApprove(claim));
  }
  if (handoff && authority.state === "authorized") {
    add(
      "revoke",
      "Revoke authority",
      "handoff_revoke",
      (reason) => handlers.onRevoke && handlers.onRevoke(claim, reason),
      { reasonRequired: true, reasonPlaceholder: "why authority is withdrawn" },
    );
  }
  if (claim.unsettled) {
    add(
      "recover",
      "Recover claim",
      "claim_recover",
      (reason) => handlers.onRecover && handlers.onRecover(claim, reason),
      { reasonRequired: true, reasonPlaceholder: "why this attempt is over" },
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

export function approveHandoff(claim) {
  return postJson(`/api/distributed/handoffs/${encodeURIComponent(claim.handoff.handoff_id)}/approve`, {
    ...expectedCandidate(claim),
    request_id: newRequestId(),
  });
}

export function revokeHandoff(claim, reason) {
  return postJson(`/api/distributed/handoffs/${encodeURIComponent(claim.handoff.handoff_id)}/revoke`, {
    ...expectedCandidate(claim),
    reason: reason || "",
    request_id: newRequestId(),
  });
}

export function recoverClaim(claim, reason, status = "blocked") {
  return postJson(`/api/distributed/claims/${encodeURIComponent(claim.claim_id)}/recover`, {
    expected_phase: claim.phase,
    status,
    reason: reason || "",
    request_id: newRequestId(),
  });
}

// --- mounting into the task detail -----------------------------------------

/// Fill `container` with the distributed panel for `taskId`, reading the console
/// once and re-reading it after every action.
///
/// The container is left empty when this workspace holds no claim for the task,
/// which is the normal case: the block simply does not appear.
export function mountTaskClaimPanel(container, taskId, { onTaskChanged, onContent } = {}) {
  const announce = (rendered) => {
    if (onContent) onContent(rendered);
    return rendered;
  };
  const render = (payload) => {
    container.textContent = "";
    if (payload && payload.owner_workspace === false) {
      // A replica checkout: claim state lives on the owner machine. Say so
      // rather than rendering an empty panel that reads as "no claims".
      container.appendChild(
        el("div", {
          class: "claim-note",
          text: payload.refusal_detail || "this checkout is a replica; the owner machine holds claim state",
        }),
      );
      return announce(true);
    }
    const claims = claimsForTask(payload, taskId);
    // The common case: this workspace holds no claim for this task, so the
    // block does not appear at all.
    if (claims.length === 0) return announce(false);
    const capabilities = (payload && payload.capabilities) || {};
    for (const claim of claims) {
      container.appendChild(
        buildClaimPanel(claim, capabilities, {
          onApprove: (target) => act(() => approveHandoff(target)),
          onRevoke: (target, reason) => act(() => revokeHandoff(target, reason)),
          onRecover: (target, reason) => act(() => recoverClaim(target, reason)),
        }),
      );
    }
    return announce(true);
  };

  const feedback = (text, kind) => {
    const note = el("div", { class: `claim-feedback ${kind}`, text });
    note.setAttribute("role", "status");
    container.insertBefore(note, container.firstChild);
  };

  const act = (run) =>
    run()
      .then((body) => {
        if (onTaskChanged) onTaskChanged();
        // Re-read before reporting: the operator should see the decision and
        // the state it produced together, never the state that preceded it.
        return refresh().then(() => feedback(actionSummary(body), "ok"));
      })
      .catch((error) => {
        // A refused action is the point of the backend check, not a bug in it.
        // Stale state re-reads before the operator decides again; an uncertain
        // merge is left alone until it is reconciled.
        const stale = error && (error.code === "stale_claim" || error.code === "handoff_not_current");
        const message = error && error.message ? error.message : String(error);
        if (!stale) {
          feedback(message, error && error.code === "uncertain_merge_intent" ? "uncertain" : "error");
          return;
        }
        return refresh().then(() => feedback(`${message} — refreshed`, "stale"));
      });

  const failed = (error) => {
    container.textContent = "";
    container.appendChild(
      el("div", { class: "claim-feedback error", text: `claim state unavailable: ${error.message || error}` }),
    );
    return announce(true);
  };

  const refresh = () => loadDistributedConsole({ force: true }).then(render).catch(failed);

  const cached = peekDistributedConsole();
  if (cached) return Promise.resolve(render(cached));
  container.appendChild(el("div", { class: "claim-note", text: "reading claim state…" }));
  return loadDistributedConsole().then(render).catch(failed);
}

function actionSummary(body) {
  const result = (body && body.result) || {};
  if (result.handoff_id) {
    return `handoff ${result.handoff_id}: claim ${result.phase}, task ${result.task_status}`;
  }
  return `claim ${result.claim_id || "?"}: ${result.phase || "?"}, task ${result.task_status || "?"}`;
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
