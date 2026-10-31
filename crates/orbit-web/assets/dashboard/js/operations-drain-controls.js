import { AUTO_DRAIN_APPROVE_RULE, drainCheckout } from './drain-start.js';
import { revealDockStart } from './log-tail.js';
import { captureWorkspaceVisit, describePullSettlements, el, infoHint, postJson, reviewLayerText, submissionNoticeLines } from './common.js';
import { $, autoDrainNumber, feedback, operationsState, pendingOperations, selectedWorkspace } from './operations-shared.js';
import { autoDrainCounts, autoDrainLiveWindow, autoDrainReasons, throttleNames, throttledResources } from './operations-drain-capacity.js';

let drainControlCallbacks = null;

export function configureOperationsDrainControls(callbacks) {
  drainControlCallbacks = callbacks;
}

// The Tasks dock's Start card.
const AUTO_DRAIN_DURATION_SECONDS = { "1h": 3600, "2h": 7200, "4h": 14400, "8h": 28800, "12h": 43200, "24h": 86400 };
const AUTO_DRAIN_DURATIONS = Object.keys(AUTO_DRAIN_DURATION_SECONDS);
const AUTO_DRAIN_APPROVE_WARNING = "The window will approve qualifying proposed tasks, including ones filed while it runs.";
const AUTO_DRAIN_COMPLETE_WARNING = "Also marks every task this window ships as done (review -> done), not only the ones eligible right now.";
let autoDrainDuration = "1h";
let autoDrainConcurrency = "";
let autoDrainComplete = false;
let autoDrainApproveProposed = false;
let autoDrainAutoFix = false;
// "" keeps workspace review policy; otherwise the layer this window adds.
let autoDrainReview = "";
const AUTO_DRAIN_CONCURRENCY_MAX = 4_294_967_295;
const AUTO_DRAIN_CONCURRENCY_PROBLEM = "Parallel tasks must be a whole number, 1 or more. Leave it blank for the runtime default.";

export function autoDrainConcurrencyValid() {
  if (autoDrainConcurrency === "") return true;
  if (!/^\d+$/.test(autoDrainConcurrency)) return false;
  const value = Number(autoDrainConcurrency);
  return value >= 1 && value <= AUTO_DRAIN_CONCURRENCY_MAX;
}

export function autoDrainConcurrencyValue() {
  return autoDrainConcurrency;
}

function drainFieldHead(label, help, { warn = false, id = null } = {}) {
  const head = el("span", { class: "drain-field-head" }, [label]);
  if (help) {
    const hint = infoHint(help, { id });
    if (warn) hint.classList.add("warn");
    head.appendChild(hint);
  }
  return head;
}

// Why a control is unavailable stays visible text: a disabled control with no
// reason in view reads as broken.
function drainFieldRefusal(text, id = null) {
  if (!text) return null;
  const note = el("p", { class: "drain-field-hint warn", text });
  if (id) note.id = id;
  return note;
}

// An on/off choice: the label and its hint on the left, a switch on the right.
function drainSwitchField({ id, label, help, warn = false, checked, disabled = false, title = "", refusal = "", extraClass = "", onChange }) {
  const input = el("input", { class: "drain-switch", role: "switch" });
  input.id = id;
  input.type = "checkbox";
  input.checked = checked;
  input.disabled = disabled;
  input.dataset.drainFocus = id;
  if (title) input.title = title;
  input.setAttribute("aria-describedby", refusal ? `${id}-reason ${id}-help` : `${id}-help`);
  input.addEventListener("change", () => onChange(input.checked));
  const name = el("label", { class: "drain-field-label", text: label });
  name.htmlFor = id;
  return el("div", { class: `drain-field drain-field-row${extraClass ? ` ${extraClass}` : ""}` }, [
    el("div", { class: "drain-field-text" }, [drainFieldHead(name, help, { warn, id: `${id}-help` }), drainFieldRefusal(refusal, `${id}-reason`)]),
    input,
  ]);
}

function autoDrainDurationControl(payload) {
  const segment = el("div", { class: "drain-durations" });
  segment.setAttribute("role", "group");
  segment.setAttribute("aria-label", "Window duration");
  for (const value of AUTO_DRAIN_DURATIONS) {
    const selected = value === autoDrainDuration;
    const option = el("button", { class: `drain-duration mono${selected ? " selected" : ""}`, text: value });
    option.type = "button";
    option.dataset.drainFocus = `duration-${value}`;
    option.setAttribute("aria-pressed", selected ? "true" : "false");
    option.addEventListener("click", () => {
      autoDrainDuration = value;
      drainControlCallbacks.render(payload);
    });
    segment.appendChild(option);
  }
  return segment;
}

// Concurrency is the same blank-means-runtime-default number input as before,
// with − / + around it; the stepper clamps at the input's own minimum of 1.
function autoDrainConcurrencyControl(payload, form) {
  const capacity = payload.capacity || {};
  const limit = autoDrainNumber(capacity.max_active_leaf_runs);
  const fallback = Number.isFinite(limit) && limit >= 1 ? String(Math.trunc(limit)) : "";
  const label = el("label", { class: "drain-field-label", text: "Parallel tasks" });
  label.htmlFor = "auto-drain-concurrency";
  const input = el("input", { class: "drain-stepper-value mono", title: "Leaf-run concurrency (blank = runtime default)" });
  input.id = "auto-drain-concurrency";
  input.type = "number";
  input.min = "1";
  input.placeholder = fallback || "auto";
  input.value = autoDrainConcurrency;
  input.dataset.drainFocus = "concurrency";
  // The message sits under the whole settings row, not in this narrow column.
  const problem = el("div", { class: "operation-control-note drain-stop-note disabled-reason" });
  problem.id = "auto-drain-concurrency-problem";
  problem.setAttribute("role", "alert");
  problem.hidden = true;
  form.problem = problem;
  input.setAttribute("aria-describedby", problem.id);
  // Typing does not rebuild the card (that would drop the caret), so the field,
  // its message and the Start button are brought in line by hand.
  form.syncConcurrency = () => {
    const valid = autoDrainConcurrencyValid();
    input.setAttribute("aria-invalid", valid ? "false" : "true");
    problem.textContent = valid ? "" : AUTO_DRAIN_CONCURRENCY_PROBLEM;
    problem.hidden = valid;
    if (form.start) form.start.disabled = autoDrainStartBlocked(payload);
  };
  input.addEventListener("input", () => {
    autoDrainConcurrency = input.value.trim();
    form.syncConcurrency();
  });
  const step = (delta, name) => {
    const button = el("button", { class: "drain-step", text: delta < 0 ? "−" : "+" });
    button.type = "button";
    button.dataset.drainFocus = `concurrency${delta}`;
    button.setAttribute("aria-label", `${name} concurrency`);
    button.addEventListener("click", () => {
      const current = Math.trunc(Number(autoDrainConcurrency || fallback || 1));
      autoDrainConcurrency = String(Math.max(1, (Number.isFinite(current) ? current : 1) + delta));
      input.value = autoDrainConcurrency;
      form.syncConcurrency();
    });
    return button;
  };
  form.syncConcurrency();
  input.setAttribute("aria-describedby", `${problem.id} auto-drain-concurrency-hint`);
  return el("div", { class: "drain-field drain-field-row" }, [
    el("div", { class: "drain-field-text" }, [
      drainFieldHead(label, "Upper bound; the forecast may admit fewer.", { id: "auto-drain-concurrency-hint" }),
    ]),
    el("div", { class: "drain-stepper" }, [step(-1, "Decrease"), input, step(1, "Increase")]),
  ]);
}

const AUTO_DRAIN_REVIEW_STOP = "Off: shipped tasks stay in review; a separate action completes them.";

// Completion: off stops each shipped task at review, on marks it done. On is
// the riskier choice, so its hint turns amber with the warning.
function autoDrainCompletionControl(payload, reasons) {
  const unavailable = Boolean(reasons.complete) && !autoDrainComplete;
  return drainSwitchField({
    id: "auto-drain-complete",
    label: "Mark shipped tasks done",
    help: unavailable ? reasons.complete : autoDrainComplete ? AUTO_DRAIN_COMPLETE_WARNING : AUTO_DRAIN_REVIEW_STOP,
    warn: autoDrainComplete || unavailable,
    checked: autoDrainComplete,
    disabled: unavailable,
    title: unavailable ? reasons.complete : "",
    // A checked choice can still block Start (a carried-over choice on a
    // replica); its help describes the choice, so the refusal is named here.
    refusal: autoDrainComplete ? reasons.complete : "",
    extraClass: "drain-field-complete",
    onChange: (on) => {
      autoDrainComplete = on;
      drainControlCallbacks.render(payload);
    },
  });
}

// Approving proposed tasks, off by default; on acts on work nobody has looked
// at yet, so its hint turns amber. The qualification rule is in the hint and
// a refusal stays visible.
function autoDrainApproveControl(payload, reasons) {
  return drainSwitchField({
    id: "auto-drain-approve",
    label: "Approve qualifying proposals",
    help: autoDrainApproveProposed ? `${AUTO_DRAIN_APPROVE_WARNING}\n${AUTO_DRAIN_APPROVE_RULE}` : `Off: proposed tasks stay proposed until you approve them.\n${AUTO_DRAIN_APPROVE_RULE}`,
    warn: autoDrainApproveProposed,
    checked: autoDrainApproveProposed,
    disabled: Boolean(reasons.approve) && !autoDrainApproveProposed,
    title: reasons.approve || "",
    refusal: reasons.approve && !reasons.submit ? reasons.approve : "",
    extraClass: "drain-field-approve",
    onChange: (on) => {
      autoDrainApproveProposed = on;
      drainControlCallbacks.render(payload);
    },
  });
}

const AUTO_DRAIN_AUTO_FIX_HELP = "Repair failed required checks and merge conflicts on this window's PRs. Merging still requires Mark shipped tasks done.";

function autoDrainAutoFixControl(payload, reasons) {
  return drainSwitchField({
    id: "auto-drain-auto-fix",
    label: "Auto-fix PR blockers",
    help: AUTO_DRAIN_AUTO_FIX_HELP,
    checked: autoDrainAutoFix,
    // Keep an unavailable selected option clearable after authority changes.
    disabled: Boolean(reasons.autoFix) && !autoDrainAutoFix,
    refusal: reasons.autoFix || "",
    onChange: (on) => {
      autoDrainAutoFix = on;
      drainControlCallbacks.render(payload);
    },
  });
}

const AUTO_DRAIN_REVIEW_HELP = "Adds a second-agent review before landing to every task this window admits. It can only add review: the layer workspace policy already applies is kept, and the other layer is refused.";

// Review before landing for this window only: workspace policy by default, or
// a layer the window adds. Workspace configuration is unchanged.
function autoDrainReviewControl(payload, reasons) {
  const label = el("label", { class: "drain-field-label", text: "Review before landing" });
  label.htmlFor = "auto-drain-review";
  const select = el("select", { class: "drain-mode-select" });
  select.id = "auto-drain-review";
  select.dataset.drainFocus = "review";
  for (const [value, text] of [["", "Workspace policy"], ["before-pr", "Add before-PR review"], ["before-landing", "Add before-landing review"]]) {
    const option = el("option", { text });
    option.value = value;
    select.appendChild(option);
  }
  // Keep an unavailable selected layer clearable after the view changes.
  select.disabled = Boolean(reasons.review) && !autoDrainReview;
  select.value = autoDrainReview;
  select.addEventListener("change", () => {
    autoDrainReview = select.value;
    drainControlCallbacks.render(payload);
  });
  return el("div", { class: "drain-field" }, [
    drainFieldHead(label, autoDrainReview ? AUTO_DRAIN_REVIEW_HELP : "The workspace's review.timing setting decides."),
    select,
    drainFieldRefusal(reasons.review || ""),
  ]);
}

// A replica starts a pull from its owner; an owner starts its own backlog.
function autoDrainStartRefusal(payload) {
  const checkout = drainCheckout(payload, selectedWorkspace()?.id);
  return checkout.replica ? checkout.pullRefusal || (!checkout.ownerHost || !checkout.workspaceId && !checkout.pullSelector
    ? "The owner's host and workspace are unavailable. Refresh before starting a pull window."
    : "") : "";
}

// Why Start is off, in one place so typing in the concurrency field can
// re-evaluate it without a rebuild.
function autoDrainStartBlocked(payload) {
  const reasons = autoDrainReasons(payload);
  return Boolean(reasons.submit || autoDrainStartRefusal(payload))
    || pendingOperations.has("auto-drain:start")
    || !autoDrainConcurrencyValid()
    || (autoDrainComplete && Boolean(reasons.complete))
    || (autoDrainApproveProposed && Boolean(reasons.approve))
    || (autoDrainAutoFix && Boolean(reasons.autoFix))
    || (Boolean(autoDrainReview) && Boolean(reasons.review));
}

function autoDrainStartButton(payload, form) {
  const key = "auto-drain:start";
  const reasons = autoDrainReasons(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: "operation-button drain-start primary",
    text: pending ? "Starting…" : `${payload.capacity?.drain_phase === "draining" ? "Start another" : "Start"} ${autoDrainDuration} window`,
    title: reasons.submit || autoDrainStartRefusal(payload) || "Submit orbit.workflow.auto with this duration and concurrency",
  });
  button.type = "button";
  button.dataset.drainFocus = "start";
  button.disabled = autoDrainStartBlocked(payload);
  form.start = button;
  form.syncConcurrency?.();
  button.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    if (autoDrainStartBlocked(payload)) return;
    const workspace = selectedWorkspace();
    const checkout = drainCheckout(payload, workspace?.id);
    const counts = autoDrainCounts(payload);
    const duration = autoDrainDuration;
    const completeLine = checkout.replica
      ? "The owner decides completion and review for work this replica claims."
      : autoDrainComplete
        ? `WARNING: ${AUTO_DRAIN_COMPLETE_WARNING}`
      : "Shipped tasks stay in review; a separate action completes them.";
    const approveProposed = autoDrainApproveProposed;
    const autoFix = autoDrainAutoFix;
    const review = autoDrainReview;
    const confirmText = [
      checkout.replica
        ? `Start pulling from ${checkout.ownerHost} in workspace "${workspace?.name || workspace?.id}"?`
        : `Start a bounded auto-delivery window in workspace "${workspace?.name || workspace?.id}"?`,
      `Duration: ${duration} · Concurrency: ${autoDrainConcurrency || "runtime default"}`,
      `Review before landing: ${checkout.replica ? "owner policy" : review ? `add ${review} review` : "workspace policy"}`,
      `Currently eligible: ${counts.eligible} · waiting: ${counts.waiting}`,
      "",
      completeLine,
      ...(autoFix ? [`Auto-fix PR blockers: ${AUTO_DRAIN_AUTO_FIX_HELP}`] : []),
      ...(approveProposed ? [`${AUTO_DRAIN_APPROVE_WARNING} ${AUTO_DRAIN_APPROVE_RULE}`] : []),
    ].join("\n");
    if (!window.confirm(confirmText)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", `Starting a ${duration} auto-delivery window…`);
    drainControlCallbacks.render(payload);
    try {
      const body = checkout.replica
        ? { for_duration: duration, pull: checkout.workspaceId || checkout.pullSelector, host: checkout.ownerHost }
        : { for_duration: duration, complete: autoDrainComplete, approve_proposed: approveProposed, auto_fix: autoFix };
      if (!checkout.replica && review) body.review = review;
      if (autoDrainConcurrency) body.concurrency = Number(autoDrainConcurrency);
      const result = await postJson(visit.path("/api/workflows/auto"), body);
      if (!visit.isCurrent()) return;
      const runId = result?.run_id ?? null;
      const state = result?.state ?? "submitted";
      const completion = result?.completion ?? "review";
      const receipt = checkout.replica
        ? `Run ${runId ?? "(no run id)"} ${state}: pulling from ${result?.selector || checkout.pullSelector || checkout.ownerHost}.`
        : `Run ${runId ?? "(no run id)"} ${state} (completion: ${completion}).`;
      feedback("auto-drain-operation-feedback", "success", [`${receipt}${result?.approve_proposed === true ? " Approving qualifying proposed tasks." : ""}`, ...(result?.auto_fix === true ? ["Auto-fix PR blockers enabled."] : []), ...(result?.auto_concurrency?.mode ? [`Auto-concurrency: ${result.auto_concurrency.mode} (${result.auto_concurrency.source}).`] : []), ...(result?.review?.timing ? [`Review before landing: ${reviewLayerText(result.review)}.`] : []), ...submissionNoticeLines(result)].join("\n"));
      await refreshDrainAfterAction();
    } catch (error) {
      if (visit.isCurrent()) feedback("auto-drain-operation-feedback", "error", error.status
        ? `Auto-delivery window failed to start: ${error.message}`
        : `Submission outcome unknown: ${error.message}. Check the window and run history before starting again; this request was not retried.`);
    } finally {
      pendingOperations.delete(key);
      if (operationsState.lastAutoDrain) drainControlCallbacks.render(operationsState.lastAutoDrain);
    }
  });
  return button;
}

// Re-read the card once Start or Stop has reported. The action has already
// happened by then, so a failed read must not replace its result with a
// failure message (the panel's own note says the refresh failed), and the
// state change the action caused must not be announced over that result.
export async function refreshDrainAfterAction() {
  drainControlCallbacks.holdAnnouncement(true);
  try {
    await drainControlCallbacks.fetch();
  } catch (_) {
    /* reported by the panel's own status note */
  } finally {
    drainControlCallbacks.holdAnnouncement(false);
  }
}

const AUTO_DRAIN_STOP_CONFIRM = "Stop new admissions for the active auto-delivery window? Already admitted workers keep running under their captured completion authority. This is not cancellation. Any settlement already recorded is also delivered to its owner.";
// The pull drain's stop reads differently from the auto window's: its leaves
// were assigned by the owner and stay claimed there until they settle.
const PULL_DRAIN_STOP_CONFIRM = "Stop new admissions for this replica's pull drain? Leaves it already admitted keep running and stay claimed by their owner. This is not cancellation, and cancelling one leaf fails its claim on the owner. Any settlement already recorded is also delivered to its owner.";
const AUTO_DRAIN_SETTLE_CONFIRM = "Send completed task results to their owner? This retries results waiting to be delivered, even after the drain has finished. Running tasks keep running.";

// [ORB-12728] Counterpart to `orbit run auto --stop`: stops new admissions on
// the live coordinator without cancelling it or the workers it already
// admitted, then runs the settle-only pass that delivers recorded settlements
// [ORB-13663]. That pass needs no live window, so with none (or with
// admissions already stopped) the button stays usable as "Send pending results" —
// the remedy for leaves stuck `settling` and claims stranded `running`.
// Only a read-only view or a missing operator session disables it, and the
// reason is visible text beside the button rather than a title alone.
// [ORB-20501] The settle-only pass delivers a replica's pull settlements to
// its owner; an owner has none to send, so with no window open there it
// offers nothing ("none") rather than a button that cannot do anything.
function autoDrainStopMode(payload) {
  const live = autoDrainLiveWindow(payload);
  const autoOpen = live.runId && !live.admissionsStopped;
  const pullOpen = live.pullRunId && !live.pullAdmissionsStopped;
  if (autoOpen || pullOpen) return "stop";
  return drainCheckout(payload, selectedWorkspace()?.id).replica ? "settle" : "none";
}

// The windows a Stop would close now, named for confirm and status text.
function autoDrainStopTargets(live) {
  const targets = [];
  if (live.runId && !live.admissionsStopped) targets.push(`auto window ${live.runId}`);
  if (live.pullRunId && !live.pullAdmissionsStopped) targets.push(`pull drain ${live.pullRunId}`);
  return targets;
}

function autoDrainStopCopy(payload) {
  const live = autoDrainLiveWindow(payload);
  if (autoDrainStopMode(payload) === "stop") {
    const autoOpen = live.runId && !live.admissionsStopped;
    const pullOpen = live.pullRunId && !live.pullAdmissionsStopped;
    const confirm = autoOpen && pullOpen
      ? `${AUTO_DRAIN_STOP_CONFIRM} The pull drain ${live.pullRunId} on this replica is stopped too; its admitted leaves stay claimed by their owner.`
      : autoOpen ? AUTO_DRAIN_STOP_CONFIRM : PULL_DRAIN_STOP_CONFIRM;
    const description = autoOpen
      ? "Stop starting new tasks in this window. Started tasks keep running; completed results go to their owner."
      : "Stop starting new tasks on this replica. Started tasks keep running; completed results go to their owner.";
    return { label: "Stop", busy: "Stopping…", aria: "Stop admissions", confirm, description };
  }
  const stoppedBy = (stop) => (stop?.actor ? ` (by ${stop.actor})` : "");
  const stopped = [];
  if (live.runId) stopped.push(`${live.runId}${stoppedBy(live.stop)}`);
  if (live.pullRunId) stopped.push(`pull drain ${live.pullRunId}${stoppedBy(live.pullStop)}`);
  const already = stopped.length
    ? `Admissions are already stopped for ${stopped.join(" and ")}. `
    : "No auto-delivery window is live. ";
  return { label: "Send pending results", busy: "Sending…", aria: "Send pending task results", confirm: AUTO_DRAIN_SETTLE_CONFIRM, description: `${already}Send completed task results to their owner.` };
}

// The outcome word for the admissions half of a stop result.
function autoDrainStopHeadline(outcome) {
  if (outcome === "idle") return "No auto-delivery window was live";
  if (outcome === "unchanged") return "Admissions were already stopped";
  if (outcome === "cancelled_queued") return "Queued window cancelled";
  return `Admissions ${outcome ?? "stop requested"}`;
}

export function autoDrainStopButton(payload) {
  if (autoDrainStopMode(payload) === "none") return null;
  const key = "auto-drain:stop";
  const reasons = autoDrainReasons(payload);
  const live = autoDrainLiveWindow(payload);
  const copy = autoDrainStopCopy(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: `operation-button drain-stop${autoDrainStopMode(payload) === "settle" ? " settle" : ""}`,
    text: pending ? copy.busy : copy.label,
    title: reasons.stop || `${copy.description}${autoDrainStopMode(payload) === "settle" ? " (deliver recorded settlements)" : ""}`,
  });
  button.type = "button";
  button.dataset.drainFocus = "stop";
  button.setAttribute("aria-label", pending ? copy.busy : copy.aria);
  button.disabled = Boolean(reasons.stop) || pending;
  button.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    if (pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const targets = autoDrainStopTargets(live);
    const windowLine = targets.length ? `Window: ${targets.join(" and ")} in workspace "${workspace?.name || workspace?.id}"` : `Workspace "${workspace?.name || workspace?.id}"`;
    if (!window.confirm(`${copy.confirm}\n\n${windowLine}`)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", autoDrainStopMode(payload) === "stop" ? `Stopping admissions for ${targets.join(" and ")}…` : "Sending completed task results…");
    drainControlCallbacks.render(payload);
    try {
      const result = await postJson(visit.path("/api/workflows/auto/stop"), {});
      if (!visit.isCurrent()) return;
      const coordinators = Array.isArray(result?.coordinators) ? result.coordinators : [];
      const remaining = coordinators.reduce((sum, change) => sum + (Array.isArray(change?.remaining_children) ? change.remaining_children.length : 0), 0);
      const changes = coordinators.map((change) => `${change?.run_id ?? "(no run id)"}: ${change?.outcome ?? "?"}`).join(", ");
      const settlements = describePullSettlements(result?.pull_settlements);
      const headline = [
        autoDrainStopHeadline(result?.outcome),
        changes ? `(${changes})` : "",
        remaining > 0 ? `· ${remaining} admitted worker${remaining === 1 ? "" : "s"} still running.` : ".",
      ].filter(Boolean).join(" ");
      // A settlement that did not reach its owner is not a plain success: the
      // stop happened, but the operator still has something to wait out or do.
      feedback(
        "auto-drain-operation-feedback",
        settlements.attention ? "error" : "success",
        settlements.text ? `${headline} Settlements: ${settlements.text}.` : headline,
      );
      await refreshDrainAfterAction();
    } catch (error) {
      if (visit.isCurrent()) feedback("auto-drain-operation-feedback", "error", `${copy.label === "Stop" ? "Stopping admissions" : "Settling"} failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (operationsState.lastAutoDrain) drainControlCallbacks.render(operationsState.lastAutoDrain);
    }
  });
  return button;
}

// Visible text (not only a tooltip, which keyboard and touch users never see)
// says why the control is off, or what it does right now.
export function autoDrainStopNote(payload) {
  if (autoDrainStopMode(payload) === "none") return null;
  const reasons = autoDrainReasons(payload);
  return el("div", {
    class: reasons.stop ? "operation-control-note drain-stop-note disabled-reason" : "operation-control-note drain-stop-note",
    text: reasons.stop || autoDrainStopCopy(payload).description,
  });
}

// The Tasks dock's Start card: window length, parallel tasks and admission
// control, then what happens after a task ships (completion, proposed tasks,
// auto-fix, review). Each choice explains itself in an info hint beside its
// label; a refusal is visible text under the control.
// A sunk footer sums the window up, warns when a throttle will hold its
// admissions, and holds Start.
export function renderAutoDrainStart(payload, reasons) {
  const card = $("auto-drain-quick-start");
  const body = $("auto-drain-quick-start-body");
  if (!card || !body) return;
  body.textContent = "";
  card.hidden = Boolean(reasons.submit);
  if (card.hidden) return;
  const counts = autoDrainCounts(payload);
  const sub = $("auto-drain-quick-start-sub");
  if (sub) sub.textContent = `${counts.eligible} eligible now · ${counts.waiting} waiting`;
  // The concurrency field and the Start button are built apart but depend on
  // each other; they meet here.
  const form = {};
  const durationLabel = el("span", { class: "drain-field-label", text: "Window length" });
  durationLabel.id = "auto-drain-duration-label";
  body.append(
    el("div", { class: "drain-field drain-field-duration" }, [
      drainFieldHead(durationLabel, "Admissions stop when the window closes; tasks already started run to the end."),
      autoDrainDurationControl(payload),
    ]),
    autoDrainConcurrencyControl(payload, form),
    form.problem,
    el("div", { class: "drain-divider", role: "separator", "aria-label": "After a task ships" }, [el("span", { text: "After a task ships" })]),
    autoDrainCompletionControl(payload, reasons),
    autoDrainApproveControl(payload, reasons),
    autoDrainAutoFixControl(payload, reasons),
    autoDrainReviewControl(payload, reasons),
    autoDrainStartFoot(payload, form),
  );
  revealDockStart();
}

// "1h window · up to 5 in parallel · enforce", plus each opt-in that is on.
function autoDrainStartSummary(payload) {
  const capacity = payload.capacity || {};
  const limit = autoDrainNumber(capacity.max_active_leaf_runs);
  const parallel = autoDrainConcurrencyValid() && autoDrainConcurrency
    ? autoDrainConcurrency
    : Number.isFinite(limit) && limit >= 1 ? String(Math.trunc(limit)) : "";
  const mode = payload.auto_concurrency?.configured_mode || "";
  return [
    `${autoDrainDuration} window`,
    parallel ? `up to ${parallel} in parallel` : "runtime default parallelism",
    mode ? (mode === "off" ? "no forecast" : mode) : "",
    autoDrainComplete ? "marks done" : "",
    autoDrainApproveProposed ? "approves proposals" : "",
    autoDrainAutoFix ? "auto-fix on" : "",
    autoDrainReview ? `adds ${autoDrainReview} review` : "",
  ].filter(Boolean).join(" · ");
}

function autoDrainStartFoot(payload, form) {
  const summary = el("p", { class: "drain-start-summary", text: autoDrainStartSummary(payload) });
  const previous = form.syncConcurrency;
  form.syncConcurrency = () => {
    previous?.();
    summary.textContent = autoDrainStartSummary(payload);
  };
  const resources = throttledResources(payload.capacity);
  const held = resources.length
    ? el("p", { class: "drain-start-held" }, [
      el("span", { class: "drain-banner-icon small", "aria-hidden": "true" }),
      el("span", { text: `Starts now, but nothing is admitted until the ${throttleNames(resources)} throttle clears.` }),
    ])
    : null;
  return el("div", { class: "drain-start-foot" }, [summary, held, drainFieldRefusal(autoDrainStartRefusal(payload)), autoDrainStartButton(payload, form)]);
}

// A panel poll paints the snapshot it already has. Opening the fleet card
// refetches only once that snapshot is older than the fleet cadence. The
// indicator timer, not this poll, is what asks the hosts again.
