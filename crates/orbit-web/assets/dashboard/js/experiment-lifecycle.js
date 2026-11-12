// Orbit dashboard Analytics › Experiments: the Pause and Resume actions on one
// experiment's page. A running experiment offers Pause and a paused one offers
// Resume; no other state offers either. Each action asks for an optional
// reason, then posts to /api/diagnostics/experiments/{id}/pause or .../resume,
// which runs core's pause or resume, so core's refusal comes back as the
// message shown here. A session without operator authority sees no control.
// Start stays in experiment-form.js. Every node is built with DOM calls.

import { el, captureWorkspaceVisit, onWorkspaceChange, requestJson } from './common.js';

const ENDPOINT = "/api/diagnostics/experiments";

// The action each state offers. `key` names the operation in the page's
// `authoring` payload; `path` is the route's last segment.
const ACTIONS = {
  running: { key: "pause", path: "pause", label: "Pause", verb: "pause", progressText: "Pausing", tone: "preview" },
  paused: { key: "resume", path: "resume", label: "Resume", verb: "resume", progressText: "Resuming", tone: "save" },
};

// In-flight and open confirmations by experiment id, so a poll that rebuilds
// the header keeps an open confirmation and its typed reason.
const confirmations = new Map();

onWorkspaceChange(() => confirmations.clear());

/// The lifecycle confirmation state included in the experiments panel's render
/// hash, so a change to it repaints the page.
export function lifecycleRenderState() {
  return [...confirmations.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([id, state]) => [id, state.action, state.reason, state.pending, state.status]);
}

/// The Pause or Resume control for an experiment in a state that offers one,
/// or null when that state offers none or this session lacks the operator's
/// authority. `refresh` re-reads the page after the change lands.
export function lifecycleControl(experiment, payloadAuthoring, refresh) {
  const action = ACTIONS[experiment.status];
  if (!action || !payloadAuthoring?.[action.key]?.authorized) return null;
  const host = el("div", { class: "exl-lifecycle" });
  host.dataset.key = `lifecycle-${experiment.id}`;
  const paint = () => {
    // A confirmation for the other action is stale once the state has moved.
    let current = confirmations.get(experiment.id) || null;
    if (current && current.action !== action.key) {
      confirmations.delete(experiment.id);
      current = null;
    }
    host.replaceChildren(current
      ? confirmation(experiment, action, current, paint, refresh)
      : opener(experiment, action, paint, host));
  };
  paint();
  return host;
}

function opener(experiment, action, paint, host) {
  const button = el("button", { class: `action ${action.tone} exl-open`, type: "button", text: action.label });
  button.addEventListener("click", () => {
    confirmations.set(experiment.id, { action: action.key, reason: "", pending: false, status: "" });
    paint();
    host.querySelector("textarea")?.focus();
  });
  return button;
}

function confirmation(experiment, action, state, paint, refresh) {
  const reason = el("textarea", { class: "exf-control", "aria-label": `Reason to ${action.verb}` });
  reason.rows = 2;
  reason.value = state.reason;
  reason.disabled = state.pending;
  reason.addEventListener("input", () => { state.reason = reason.value; });
  const confirm = el("button", {
    class: "action ship exl-confirm",
    type: "button",
    text: state.pending ? `${action.progressText}…` : `Confirm ${action.verb}`,
  });
  confirm.disabled = state.pending;
  const back = el("button", { class: "action cancel", type: "button", text: "Cancel" });
  back.disabled = state.pending;
  back.addEventListener("click", () => {
    confirmations.delete(experiment.id);
    paint();
  });
  confirm.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    state.pending = true;
    state.status = "";
    paint();
    try {
      const why = state.reason.trim();
      await requestJson(
        `${ENDPOINT}/${encodeURIComponent(experiment.id)}/${action.path}`,
        "POST",
        why ? { reason: why } : {},
        visit,
      );
    } catch (error) {
      if (!visit.isCurrent() || confirmations.get(experiment.id) !== state) return;
      state.pending = false;
      state.status = error?.message || `Could not ${action.verb} the experiment.`;
      paint();
      return;
    }
    if (!visit.isCurrent()) return;
    confirmations.delete(experiment.id);
    await refresh();
  });
  return el("div", { class: "exf-confirm exl-confirm-group", role: "group", "aria-label": `${action.label} ${experiment.name}` }, [
    el("label", { class: "exf-field" }, [el("span", { text: `Reason (recorded with the ${action.verb})` }), reason]),
    el("div", { class: "exf-buttons" }, [confirm, back]),
    state.status ? el("p", { class: "exf-error", role: "alert", text: state.status }) : null,
  ]);
}
