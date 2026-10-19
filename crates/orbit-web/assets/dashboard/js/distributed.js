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

import { captureWorkspaceVisit, el, fetchJson, formatDateTime, postJson, makeToggleRow, isAggregateView, getRegisteredHosts, getWorkspace, getWorkspaceRevision, hostWriteRefusal, runHref } from './common.js';

const CONSOLE_PATH = "/api/distributed/claims";

// One task-scoped read per short window, shared by however many panels show
// it. The memo expires after CONSOLE_TTL_MS so a detail that is opened or rebuilt later
// (the dashboard refreshes tasks on its own timer and never re-reads claims)
// shows current authority rather than a panel hours out of date. It is also
// invalidated on every action and workspace change, so a decision is never
// rendered from the state that preceded it.
export const CONSOLE_TTL_MS = 10000;
const cachedConsoles = new Map();
const inflightConsoles = new Map();
// The last read per task in this workspace visit, kept past the TTL. A detail
// rebuilt after the memo expired paints its attention callout from it at once
// instead of flashing empty until the re-read lands; the re-read then replaces
// it. Nothing decides from it: actions always carry a fresh read's identity.
const knownConsoles = new Map();
// The outcome of the operator's last action, per task. It outlives the block
// that reported it: the detail is rebuilt once the action changes the task, and
// a message written only to the old container would be lost with it.
let lastFeedback = null;

/// Drop the memoized read. Called after any owner action and by the workspace
/// selector, since claim state is per-workspace.
export function invalidateDistributedConsole() {
  cachedConsoles.clear();
  inflightConsoles.clear();
  lastFeedback = null;
}

function freshConsole(taskId) {
  const entry = cachedConsoles.get(taskId);
  return entry && Date.now() - entry.at < CONSOLE_TTL_MS ? entry.payload : null;
}

export function peekDistributedConsole(taskId) {
  return freshConsole(taskId);
}

export function loadDistributedConsole({ taskId, force = false } = {}) {
  // Claim state is per-workspace. The aggregate view has no concrete workspace
  // to scope to — the endpoint would 400 — so answer as "nothing here" rather
  // than painting a failure across every task detail.
  if (isAggregateView() || !taskId) return Promise.resolve(null);
  if (force) invalidateDistributedConsole();
  const key = String(taskId);
  const fresh = freshConsole(key);
  if (fresh) return Promise.resolve(fresh);
  if (!inflightConsoles.has(key)) {
    // Revision plus request identity: a response issued for workspace A must
    // not populate the memo after a switch (or a forced re-read) the way
    // requestPanel rejects A→B→A and overlapping refreshes.
    const revision = getWorkspaceRevision();
    // One task's whole history, settled claims in full: the panel renders
    // them, and the endpoint's default (active, compact) would hide them.
    const request = fetchJson(`${CONSOLE_PATH}?task=${encodeURIComponent(key)}&state=all&detail=true`)
      .then((payload) => {
        const body = payload || {};
        if (inflightConsoles.get(key) !== request || revision !== getWorkspaceRevision()) {
          return body;
        }
        cachedConsoles.set(key, { payload: body, at: Date.now() });
        knownConsoles.set(key, { payload: body, revision });
        inflightConsoles.delete(key);
        return body;
      })
      .catch((error) => {
        if (inflightConsoles.get(key) === request) inflightConsoles.delete(key);
        throw error;
      });
    inflightConsoles.set(key, request);
  }
  return inflightConsoles.get(key);
}

export function claimsForTask(payload, taskId) {
  const claims = payload && Array.isArray(payload.claims) ? payload.claims : [];
  return claims.filter((claim) => claim && claim.task_id === taskId);
}

/// The task's claims from the last read in this workspace visit, however old,
/// or `null` when there has been none. For painting only, never for deciding.
export function lastKnownClaims(taskId) {
  if (isAggregateView() || !taskId) return null;
  const entry = knownConsoles.get(String(taskId));
  if (!entry || entry.revision !== getWorkspaceRevision()) return null;
  return entry.payload && entry.payload.owner_workspace === false ? [] : claimsForTask(entry.payload, taskId);
}

// --- what needs the operator ------------------------------------------------

/// Landing evidence is an opaque string: an `owner_landing_refusal` JSON
/// diagnostic, or older plain text. Read it defensively for the parts an
/// operator acts on — the human reason, the required check that failed and the
/// failing test's signature — and keep the raw text (pretty-printed when it is
/// JSON) for the disclosure. Anything unexpected falls back to plain text.
export function readLandingEvidence(evidence) {
  if (evidence == null || evidence === "") return null;
  const raw = typeof evidence === "string" ? evidence : JSON.stringify(evidence);
  let parsed = null;
  if (typeof evidence === "object") {
    parsed = evidence;
  } else {
    try {
      parsed = JSON.parse(raw);
    } catch (_) {
      parsed = null;
    }
  }
  const structured = parsed !== null && typeof parsed === "object";
  let reason = null;
  if (structured && !Array.isArray(parsed)) {
    const value = parsed.reason;
    if (typeof value === "string") reason = value;
    else if (value && typeof value.value === "string") reason = value.value;
  } else if (!structured) {
    reason = raw;
  }
  reason = reason && reason.trim() ? reason.trim() : null;
  const check = reason ? (/status check '([^'\n]+)' failed/.exec(reason) || [])[1] || null : null;
  const test = reason ? (/failed with signature `([^`\n]+)`/.exec(reason) || [])[1] || null : null;
  return {
    raw,
    structured,
    pretty: structured ? JSON.stringify(parsed, null, 2) : raw,
    reason,
    check,
    test,
  };
}

/// The claim the detail's summary describes: the newest live one (unsettled or
/// handed off), else the newest of any.
export function currentClaim(claims) {
  const list = Array.isArray(claims) ? claims.filter(Boolean) : [];
  if (list.length === 0) return null;
  const newest = (rows) => rows.reduce((best, row) =>
    (!best || String(row.created_at || "") > String(best.created_at || "") ? row : best), null);
  return newest(list.filter((claim) => claim.unsettled === true || claim.phase === "handed_off")) || newest(list);
}

/// Whether this claim's stopped (or uncertain) landing may be retried. The
/// claim panel offers Retry landing exactly when this holds.
export function landingRetryOffered(claim) {
  const handoff = claim && claim.handoff;
  const authority = (handoff && handoff.authority) || {};
  return Boolean(handoff && claim.phase === "handed_off" && authority.state === "authorized" &&
    !claim.landing_invalidated && (handoff.landing?.state === "stopped" || claim.unresolved_merge_intent));
}

/// What the task needs from the operator now, from its current claim, or
/// `null` when nothing does. Uncertain merges come first: until one is
/// reconciled nothing else about the claim can move.
export function claimAttention(claims) {
  const claim = currentClaim(claims);
  if (!claim) return null;
  const handoff = claim.handoff || null;
  const landing = (handoff && handoff.landing) || {};
  if (claim.unresolved_merge_intent) {
    return {
      kind: "uncertain-merge",
      tone: "danger",
      claim,
      headline: "Merge outcome unknown — reconcile it before acting",
      body: "A merge was sent and its reply was lost. Revoking and recovering are refused until it is reconciled against the provider.",
      evidence: readLandingEvidence(landing.evidence),
      retry: landingRetryOffered(claim),
    };
  }
  if (handoff && landing.state === "stopped" && (claim.phase === "handed_off" || claim.phase === "repair_pending")) {
    const evidence = readLandingEvidence(landing.evidence);
    const repairing = claim.phase === "repair_pending";
    let headline = "Landing stopped";
    if (evidence && evidence.check) headline = "Landing stopped — a required check failed";
    if (repairing) headline = `${headline}; an automatic repair is pending`;
    return {
      kind: "landing-stopped",
      tone: repairing ? "warning" : "danger",
      claim,
      headline,
      body: landing.summary || null,
      evidence,
      retry: landingRetryOffered(claim),
    };
  }
  if (claim.landing_invalidated && handoff && claim.phase === "handed_off") {
    return {
      kind: "landing-invalidated",
      tone: "warning",
      claim,
      headline: "Landing authority was invalidated",
      body: "The owner will not land this candidate. It stays in review until a new validated handoff.",
      evidence: null,
      retry: false,
    };
  }
  return null;
}

/// The delivery's five stages, each `done`, `failed`, `active` or `pending`,
/// from the current claim's phase, completion authority and landing state.
/// `null` when the task has no claim.
export function deliverySteps(claims) {
  const claim = currentClaim(claims);
  if (!claim) return null;
  const handoff = claim.handoff || null;
  const authority = (handoff && handoff.authority && handoff.authority.state) || null;
  const landing = (handoff && handoff.landing && handoff.landing.state) || null;
  const landed = claim.phase === "landed" || landing === "merged" || authority === "completed";
  const ended = claim.phase === "failed" || claim.phase === "revoked";
  const step = (label, state) => ({ label, state });
  const implemented = handoff || landed ? "done" : ended ? "failed" : "active";
  const handedOff = handoff || landed ? "done" : "pending";
  let approved = "pending";
  if (authority === "authorized" || landed) approved = "done";
  else if (authority === "revoked") approved = "failed";
  else if (handoff && authority === "not_authorized" && !ended) approved = "active";
  let landingStep = "pending";
  let landingLabel = "Landing";
  if (landed) landingStep = "done";
  else if (landing === "stopped") { landingStep = "failed"; landingLabel = "Landing stopped"; }
  else if (landing === "dispatched") landingStep = "active";
  return [
    step("Implemented", implemented),
    step("Handed off", handedOff),
    step("Approved", approved),
    step(landingLabel, landingStep),
    step("Done", landed ? "done" : "pending"),
  ];
}

/// The thin five-segment strip above the detail's tabs.
export function buildDeliveryProgress(steps) {
  if (!Array.isArray(steps) || steps.length === 0) return null;
  const list = el("ol", { class: "delivery-progress", "aria-label": "Delivery progress" });
  for (const { label, state } of steps) {
    const item = el("li", { class: `delivery-step ${state}` }, [
      el("span", { class: "delivery-step-bar" }),
      el("span", { class: "delivery-step-label", text: label }),
    ]);
    item.dataset.state = state;
    if (state === "active") item.setAttribute("aria-current", "step");
    // The colour is never the only signal: a reader hears the state too.
    item.title = `${label}: ${STEP_STATE_WORDS[state] || state}`;
    list.appendChild(item);
  }
  return list;
}

// Server text reads as a sentence here: a capital first letter and a stop.
function sentence(text) {
  const value = text ? String(text).trim() : "";
  if (!value) return null;
  const capital = value.charAt(0).toUpperCase() + value.slice(1);
  return /[.!?…]$/.test(capital) ? capital : `${capital}.`;
}

// The first line of a long reason, bounded: the full text is in the panel's
// raw evidence.
function firstLine(text) {
  const value = text ? String(text).split("\n")[0].trim() : "";
  if (!value) return null;
  return value.length > 240 ? `${value.slice(0, 239)}…` : value;
}

const STEP_STATE_WORDS = { done: "done", failed: "failed", active: "in progress", pending: "not yet" };

/// The callout at the top of the detail. `options.pullRequest` is the task's
/// `{number, url}` when it has one; `options.onRetry(claim)` takes the
/// operator to the claim panel's own Retry landing control, which keeps the
/// one place that sends the request and asks for its reason.
export function buildAttentionCallout(attention, options = {}) {
  if (!attention) return null;
  // A labelled region, not an alert: the detail is rebuilt whenever its task
  // changes, and an alert would be announced again on every rebuild.
  const callout = el("section", { class: `task-attention ${attention.tone}` });
  callout.dataset.attention = attention.kind;
  const headingId = `attention-${attention.claim && attention.claim.claim_id ? attention.claim.claim_id : "claim"}`;
  callout.setAttribute("aria-labelledby", headingId);
  const heading = el("h3", { class: "task-attention-title", id: headingId, text: attention.headline });
  const body = el("p", { class: "task-attention-body" });
  const evidence = attention.evidence;
  // The failing check and test lead when the evidence names them; otherwise
  // the stop's own first line, else the server's summary of the state.
  const summary = sentence(attention.body);
  const reason = evidence && !evidence.check ? firstLine(evidence.reason) : null;
  const hasBody = Boolean((evidence && evidence.check) || reason || summary);
  if (evidence && evidence.check) {
    body.appendChild(el("span", { class: "mono", text: evidence.check }));
    if (evidence.test) {
      body.append(" fails on ");
      body.appendChild(el("span", { class: "mono task-attention-test", text: evidence.test }));
      body.append(".");
    } else {
      body.append(" failed.");
    }
    if (summary) body.append(` ${summary}`);
  } else if (reason) {
    body.textContent = sentence(reason);
  } else if (summary) {
    body.textContent = summary;
  }
  const actions = el("div", { class: "task-attention-actions" });
  const pr = options.pullRequest && options.pullRequest.url ? options.pullRequest : null;
  const link = (text, href, primary) => {
    const anchor = el("a", { class: primary ? "attention-action primary" : "attention-action", text });
    anchor.href = href;
    anchor.target = "_blank";
    anchor.rel = "noopener noreferrer";
    return anchor;
  };
  if (pr && evidence && evidence.check) actions.appendChild(link("View failing checks", `${pr.url.replace(/\/+$/, "")}/checks`, true));
  if (pr) actions.appendChild(link(pr.number ? `Open PR #${pr.number}` : "Open pull request", pr.url, false));
  if (attention.retry && typeof options.onRetry === "function") {
    const retry = el("button", { class: "attention-action", type: "button", text: "Retry landing…" });
    retry.title = "Go to Retry landing in the delivery panel, which asks for a reason";
    retry.addEventListener("click", (event) => {
      event.stopPropagation();
      options.onRetry(attention.claim);
    });
    actions.appendChild(retry);
  }
  const content = el("div", { class: "task-attention-content" }, [
    heading,
    hasBody ? body : null,
    actions.children.length ? actions : null,
  ]);
  callout.appendChild(el("span", { class: "task-attention-icon", "aria-hidden": "true" }));
  callout.appendChild(content);
  return callout;
}

// --- shared renderers -------------------------------------------------------

/// A concise execution location, or an explicit unknown.
export function formatExecutionLocation(location) {
  if (!location || location.known !== true || !location.machine_id) {
    return "unknown — recorded before execution provenance was tracked";
  }
  const machineId = String(location.machine_id);
  const shortId = machineId.length > 7 ? `${machineId.slice(0, 7)}…` : machineId;
  const label = location.machine_name || shortId;
  return `on ${label}`;
}

/// The execution-provenance cell shared by the run meta grid and the task
/// detail's run line. Given the run (`{runId, workspace}`) and an execution
/// machine the serving host's host file registers, "on <machine>" links to
/// that run on that host [ORB-14680]; an unregistered machine stays text.
export function buildExecutionProvenance(location, run = null) {
  const known = !!(location && location.known === true && location.machine_id);
  const host = known && run && run.runId
    ? getRegisteredHosts().find((row) => row.machine_id === location.machine_id)
    : null;
  if (host) {
    const link = el("a", {
      class: "exec-origin remote-run-link",
      text: formatExecutionLocation(location),
      title: `Open run ${run.runId} on ${host.name}`,
    });
    link.href = runHref(run.runId, { host: host.name, workspace: run.workspace || getWorkspace() });
    link.addEventListener("click", (event) => event.stopPropagation());
    return link;
  }
  return el("span", {
    class: known ? "exec-origin" : "exec-origin unknown",
    text: formatExecutionLocation(location),
    title: known
      ? (location.machine_name
        ? `Execution machine: ${location.machine_name} (${location.machine_id})`
        : `Execution machine id: ${location.machine_id}`)
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
  repair_pending: "repair pending",
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
  repair_admitted: "repair admitted",
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

/// Times go through the caller's formatter (the task detail's own), else the
/// dashboard clock, so a claim reads in the same zone and shape as every other
/// timestamp on the page. A missing value is `null`, never "undefined".
function formatWhen(options, value) {
  if (!value) return null;
  return options && typeof options.formatTime === "function" ? options.formatTime(value) : formatDateTime(value);
}

// --- claim panel ------------------------------------------------------------

/// Render one claim: the delivery handoff's facts first (branch, candidate,
/// pull request, review, validation, authority, landing), then the attempt
/// that produced them (provenance, phase, reservation, footprint), the raw
/// landing evidence behind a disclosure, and the owner actions the caller
/// wires.
///
/// Pure with respect to the DOM it creates — it reads only its arguments — so
/// the behavior scenarios can drive it without a server. `options` carries the
/// action handlers, an optional `formatTime` and the task's `pullRequest`.
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

  if (claim.handoff) panel.appendChild(buildHandoffPanel(claim.handoff, options));

  const run = claim.bound_run;
  panel.appendChild(line("execution", buildExecutionProvenance(
    claim.executed_on,
    run && run.run_id ? { runId: run.run_id } : null,
  )));

  if (run && run.run_id) {
    if (claim.bound_run_navigable) {
      const link = el("a", { class: "value mono", text: run.run_id });
      link.href = runHref(run.run_id);
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
  panel.appendChild(buildFootprintLine(claim));

  if (claim.landing_invalidated) {
    panel.appendChild(
      el("div", { class: "claim-warning", text: "landing authority for this claim was invalidated" }),
    );
  }
  if (claim.unresolved_merge_intent) {
    panel.appendChild(uncertainMergeBanner(claim.unresolved_merge_intent));
  }

  const evidence = claim.handoff && claim.handoff.landing ? claim.handoff.landing.evidence : null;
  if (evidence) panel.appendChild(buildEvidenceDisclosure(evidence));

  const actions = buildClaimActions(claim, capabilities, options);
  if (actions) panel.appendChild(actions);
  return panel;
}

function countOf(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

/// The frozen footprint is a list of selectors that can run long, so it sits
/// behind a disclosure whose summary says how many and whether they still
/// protect their files.
function buildFootprintLine(claim) {
  const footprint = Array.isArray(claim.footprint) ? claim.footprint : [];
  const summaryText = claim.footprint_protected
    ? `${countOf(footprint.length, "frozen selector")} · still protected`
    : `${countOf(footprint.length, "selector")} · no longer protecting files`;
  if (footprint.length === 0) return line("footprint", summaryText);
  const files = el("div", { class: "claim-footprint" });
  for (const entry of footprint) files.appendChild(el("div", { class: "mono", text: entry }));
  const details = el("details", { class: "claim-disclosure" }, [
    el("summary", { text: summaryText }),
    files,
  ]);
  return line("footprint", details);
}

function charCount(text) {
  const count = String(text).length;
  return count < 1000 ? `${count} chars` : `${(count / 1000).toFixed(1)}k chars`;
}

/// The landing attempt's raw evidence, collapsed: it can run to kilobytes of
/// diagnostic JSON, and the human reason it carries is already summarised in
/// the landing line and the attention callout. JSON is pretty-printed.
function buildEvidenceDisclosure(evidence) {
  const read = readLandingEvidence(evidence);
  return el("details", { class: "claim-disclosure landing-evidence" }, [
    el("summary", {}, [
      el("span", { class: "claim-disclosure-title", text: "Raw landing evidence" }),
      el("span", { class: "claim-disclosure-meta", text: `${read.structured ? "JSON" : "text"} · ${charCount(read.raw)}` }),
    ]),
    el("pre", { class: "landing-evidence-body mono", text: read.pretty }),
  ]);
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

function buildHandoffPanel(handoff, options = {}) {
  const wrap = el("div", { class: "handoff-panel" });
  wrap.setAttribute("data-handoff-id", handoff.handoff_id || "");

  const candidate = handoff.candidate || {};
  const delivery = candidate.delivery || {};
  wrap.appendChild(line("repository", candidate.repository));
  // Where the work goes: the base it was cut from is named only when it is not
  // the branch it lands on.
  const landingBranch = candidate.landing_branch || candidate.base_branch || "?";
  const base = candidate.base_branch && candidate.base_branch !== landingBranch ? ` (from base ${candidate.base_branch})` : "";
  wrap.appendChild(line("branch", `${candidate.source_branch || "?"} → ${landingBranch}${base}`, { class: "mono-value" }));
  wrap.appendChild(
    line(
      "candidate",
      `${shortCommit(candidate.candidate && candidate.candidate.commit)} on base ${shortCommit(candidate.base && candidate.base.commit)}`,
      { class: "mono-value" },
    ),
  );
  wrap.appendChild(line("delivery", deliveryValue(delivery, options.pullRequest)));

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
  if (validation.length === 0 && required.length === 0) {
    wrap.appendChild(line("validation", "no owner-required commands"));
  } else {
    const logs = el("div", { class: "handoff-validation" });
    for (const entry of validation) {
      logs.appendChild(el("div", { class: "mono", text: `${entry.path} · ${shortCommit(entry.sha256)}` }));
    }
    wrap.appendChild(
      line("validation", logs, {
        note: `${validation.length} captured log(s) for ${required.length} owner-required command(s): ${required.join(", ") || "none"}`,
      }),
    );
  }

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
  return wrap;
}

/// The delivery, linked to its pull request when the task records that pull
/// request's page and the numbers agree.
function deliveryValue(delivery, pullRequest) {
  const text = deliveryLabel(delivery);
  const number = delivery && delivery.kind === "pull_request" && delivery.number != null ? String(delivery.number) : null;
  if (!number || !pullRequest || !pullRequest.url || String(pullRequest.number || "") !== number) return text;
  const link = el("a", { class: "value", text });
  link.href = pullRequest.url;
  link.target = "_blank";
  link.rel = "noopener noreferrer";
  return link;
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
  const refusal = hostWriteRefusal();
  if (refusal) return { authorized: false, reason: refusal };
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
  if (landingRetryOffered(claim)) {
    add("retry-handoff", "Retry landing", "handoff_retry",
      (reason, _status, requestId) => handlers.onRetryHandoff && handlers.onRetryHandoff(claim, reason, requestId),
      { reasonRequired: true, reasonPlaceholder: "why this landing should retry" });
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
  const payload = await loadDistributedConsole({ taskId, force: true });
  const live = claimsForTask(payload, taskId).filter(
    (claim) => claim.phase === "handed_off" || claim.unsettled,
  );
  if (live.length === 0) return null;
  const handedOff = live.find((claim) => claim.phase === "handed_off" && claim.handoff);
  if (!handedOff) {
    return { refusal: "this task's distributed claim has not handed off yet; see Delivery in the task detail" };
  }
  const authority = handedOff.handoff.authority || {};
  if (authority.state === "not_authorized") return { claim: handedOff };
  return {
    refusal: authority.summary || DECIDED_REFUSALS[authority.state] || `this handoff is already ${humanize(authority.state || "decided")}`,
  };
}

export function retryHandoff(claim, reason, requestId) {
  // Keep the exact replay identity across panel rebuilds and browser restarts.
  // If durable browser storage is unavailable, no request has been sent yet.
  const identity = JSON.stringify([getWorkspace(), claim.handoff.handoff_id,
    expectedCandidate(claim), claim.handoff.landing.attempt, claim.handoff.landing.state, reason]);
  const storageKey = `orbit-handoff-retry:${identity}`;
  let durableKey;
  try {
    durableKey = localStorage.getItem(storageKey) || requestId || newRequestId();
    localStorage.setItem(storageKey, durableKey);
  } catch (_) {
    return Promise.reject(new Error("Landing retry needs browser storage to retain its request key; use the owner CLI with a saved request key."));
  }
  return postJson(`/api/distributed/handoffs/${encodeURIComponent(claim.handoff.handoff_id)}/retry`, {
    ...expectedCandidate(claim),
    expected_latest_attempt: claim.handoff.landing.attempt,
    expected_latest_state: claim.handoff.landing.state,
    reason: reason || "",
    request_key: durableKey,
  });
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
export function mountTaskClaimPanel(container, taskId, { onTaskChanged, onContent, onClaims, formatTime, pullRequest } = {}) {
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

  // Every read that lands is also handed to `onClaims`, so the detail's
  // attention callout and progress strip follow the panel's own state.
  const share = (claims, capabilities) => {
    if (onClaims) onClaims(claims, capabilities || {});
  };

  const render = (payload) => {
    if (!visit.isCurrent()) return false;
    container.textContent = "";
    if (payload && payload.owner_workspace === false) {
      share([], null);
      return announce(false);
    }
    const claims = claimsForTask(payload, taskId);
    share(claims, payload && payload.capabilities);
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
          pullRequest,
          onApprove: (target, requestId) => act(() => approveHandoff(target, requestId)),
          onRetryHandoff: (target, reason, requestId) => act(() => retryHandoff(target, reason, requestId)),
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
    !visit.isCurrent() ? Promise.resolve(false) : loadDistributedConsole({ taskId, force: true }).then(
      (payload) => {
        render(payload);
        return true;
      },
      (error) => {
        failed(error, recorded);
        return false;
      },
    );

  const cached = peekDistributedConsole(taskId);
  if (cached) return Promise.resolve(render(cached));
  container.appendChild(el("div", { class: "claim-note", text: "reading claim state…" }));
  return loadDistributedConsole({ taskId }).then(render, (error) => failed(error));
}

function actionSummary(body) {
  const result = (body && body.result) || {};
  const phase = enumLabel(PHASE_LABELS, result.phase);
  const status = result.task_status ? humanize(result.task_status) : "?";
  if (result.run_id) return `landing retry recorded: attempt ${result.attempt}, run ${result.run_id}`;
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
  const heading = el("h4", { class: "section-title" }, [el("span", { class: "field-title", text: "Delivery" })]);
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
