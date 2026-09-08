// ORB-11658: every expandable row must be operable from the keyboard, and the
// pointer path it shares must keep behaving exactly as before.
//
// These scenarios drive the shipped modules through the DOM adapter, so they
// observe what the modules actually set on each row. Focus order itself and the
// browser's native Space-activates-a-button rule are properties of the shipped
// markup, asserted against index.html in dashboard_assets.rs.
import assert from "node:assert/strict";

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

// --- Task rows: Enter expands, and the pointer path is untouched -------------

const { renderTasks } = await import("./tasks.js");

const task = { id: "ORB-1", title: "keyboard operable", status: "review", artifacts: [] };
const context = {
  getTasks: () => [task],
  getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["review"]),
  statusOrder: ["review"],
  statusUpdateTargets: [],
  fmtAbsTime: (value) => value,
  refreshDashboard: () => Promise.resolve(),
};

const tasksBody = document.getElementById("tasks-body");
const taskRow = () => tasksBody.children.find((node) => node.dataset.key === "task-ORB-1");
const taskDetail = () => tasksBody.children.find((node) => node.dataset.key === "detail-ORB-1");

// The view transition is part of the existing click behaviour: record that it
// still wraps the toggle, and that the row is named before the transition runs.
let transitions = 0;
let namedDuringTransition = null;
document.startViewTransition = (callback) => {
  transitions += 1;
  namedDuringTransition = taskRow().style.viewTransitionName;
  callback();
  return { finished: Promise.resolve() };
};

renderTasks([task], context);

assert.equal(taskRow().getAttribute("role"), "button", "a task row must expose button semantics");
assert.equal(taskRow().tabIndex, 0, "a task row must be a tab stop");
assert.equal(taskRow().getAttribute("aria-expanded"), "false");
assert.equal(taskDetail(), undefined, "the detail is not rendered while collapsed");
assert.equal(
  taskRow().getAttribute("aria-controls"),
  null,
  "a collapsed row must not point at a detail that is not in the document",
);

taskRow().dispatch("keydown", { key: "Enter" });

assert.equal(taskRow().getAttribute("aria-expanded"), "true", "Enter must expand the row");
assert.equal(taskRow().getAttribute("aria-controls"), "detail-ORB-1");
assert.equal(taskDetail().id, "detail-ORB-1", "the detail the row points at must carry that id");
assert.equal(transitions, 1, "Enter must take the same view-transition path a click takes");
assert.equal(namedDuringTransition, "task-row-ORB-1");
assert.ok(
  taskRow().className.includes("data-changed"),
  "the keyed diff must still highlight the re-rendered row",
);

// A key press that started on a nested control (a status or crew select) bubbles
// to the row; the row must leave it to the control that owns it.
const nestedSelect = document.createElement("select");
taskRow().dispatch("keydown", { key: " ", target: nestedSelect });
assert.equal(taskRow().getAttribute("aria-expanded"), "true", "a bubbled key press must not toggle the row");

// Space on the row itself is an activation.
taskRow().dispatch("keydown", { key: " " });
assert.equal(taskRow().getAttribute("aria-expanded"), "false");

// Clicking still collapses and expands exactly as it did before.
taskRow().dispatch("click");
assert.equal(taskRow().getAttribute("aria-expanded"), "true", "clicking must still expand the row");
// Enter, Space and the click each ran one transition; the bubbled key press ran none.
assert.equal(transitions, 3, "only real activations take the view-transition path");

// --- Audit rows and run-step rows share the same helper ----------------------

const auditEvent = { id: 7, timestamp: "t", role: "agent", tool_name: "orbit", command: "task", target_id: "ORB-1", status: "success", exit_code: 0, duration_ms: 12 };
const logPayload = {
  offset: 0,
  events: [
    { ts: "t", source: "orbit", level: "info", code: "OK", message_html: "started" },
    { ts: "t", source: "orbit", level: "error", code: "ERR", message_html: "failed" },
  ],
};
globalThis.fetch = async (path) => {
  const payload = String(path).startsWith("/api/audit") ? [auditEvent] : logPayload;
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};

const { fetchAndRenderAudit } = await import("./audit.js");
await fetchAndRenderAudit({});
const auditRow = () => document.getElementById("audit-body").querySelector("table.scoreboard-table").querySelector("tbody").children.find((node) => node.dataset.key === "audit-7");

assert.equal(auditRow().getAttribute("role"), "button");
assert.equal(auditRow().tabIndex, 0);
assert.equal(auditRow().getAttribute("aria-expanded"), "false");
auditRow().dispatch("keydown", { key: "Enter" });
assert.equal(auditRow().getAttribute("aria-expanded"), "true", "Enter must expand an audit event");
assert.equal(auditRow().getAttribute("aria-controls"), "audit-detail-7");
assert.ok(auditRow().classList.contains("expanded"), "the expanded row must keep its class through the keyed diff");

const { setActiveRunDetail, renderRunSteps } = await import("./run-detail.js");

setActiveRunDetail({ run: {}, steps: [{ step_index: 0, target_type: "task", target_id: "ORB-1", state: "success", duration_ms: 5, exit_code: 0 }] });
renderRunSteps();
const stepRow = () => document.getElementById("run-steps-body").children.find((node) => node.dataset.key === "step-0");

assert.equal(stepRow().getAttribute("role"), "button");
assert.equal(stepRow().tabIndex, 0);
assert.equal(stepRow().getAttribute("aria-expanded"), "false");
stepRow().dispatch("keydown", { key: " " });
assert.equal(stepRow().getAttribute("aria-expanded"), "true", "Space must expand a run step");
assert.ok(stepRow().classList.contains("expanded"));

// --- Log filter pills: pressing a pill filters and reports its own state -----

const { initLogTail } = await import("./log-tail.js");
initLogTail();
await tick();

const pills = document.querySelectorAll("#side-dock .filter-pill");
const pill = (filter) => pills.find((node) => node.dataset.filter === filter);
const visibleLogCount = () => document.getElementById("log-count").textContent;

assert.equal(visibleLogCount(), "2", "the unfiltered dock shows both lines");
assert.equal(pill("all").getAttribute("aria-pressed"), "true");

// A real <button> turns Space into a click, which is the handler under test.
pill("err").dispatch("click");

assert.equal(pill("err").getAttribute("aria-pressed"), "true", "the pressed pill must report itself pressed");
assert.equal(pill("all").getAttribute("aria-pressed"), "false", "selecting a level must release `all`");
assert.ok(pill("err").classList.contains("on"), "the visual state must track the announced state");
assert.equal(visibleLogCount(), "1", "filtering to err must drop the info line");

pill("err").dispatch("click");

assert.equal(pill("err").getAttribute("aria-pressed"), "false");
assert.equal(pill("all").getAttribute("aria-pressed"), "true", "clearing the last level falls back to `all`");
assert.equal(visibleLogCount(), "2");
