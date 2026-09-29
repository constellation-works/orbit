// DANI-10391: `/api/tasks` rows are summaries. The dashboard must render the
// collapsed row from a summary alone, read `GET /api/tasks/:id` exactly once
// when the row opens, keep an open detail painted from the last read across a
// list refresh, offer a retry when the read fails, and ask the detail endpoint
// for a transition's evidence requirement before a governed status change.
//
// Runs on the keyboard DOM adapter (dashboard_keyboard_dom.mjs), which keeps
// real attributes and dispatches to every registered listener.
import assert from "node:assert/strict";

// Feedback expiry arms multi-second timers; let them not hold the process open.
const nativeSetTimeout = globalThis.setTimeout;
globalThis.setTimeout = (fn, ms, ...args) => {
  const timer = nativeSetTimeout(fn, ms, ...args);
  if (timer && typeof timer.unref === "function") timer.unref();
  return timer;
};
const settle = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => nativeSetTimeout(resolve, 0));
};

const requests = [];
let detailFailure = null;
let patchFailure = null;
const fullTask = () => ({
  id: "ORB-2",
  title: "summary row",
  status: "review",
  crew: null,
  description: "Body text only the detail carries",
  plan: "",
  execution_summary: "",
  acceptance_criteria: ["renders"],
  context_files: [],
  tags: [],
  artifacts: [],
  external_refs: [
    { system: "github", id: "7", url: "https://github.com/example/repo/issues/7" },
    { system: "hostile", id: "1", url: "javascript:alert(1)" },
  ],
  comments: [{ at: "2026-09-15T00:00:00Z", by: "human", message: "detail comment" }],
  history: [{ at: "2026-09-15T00:00:00Z", by: "human", event: "created" }],
  status_transitions: [
    { status: "done", required_field: "execution_summary" },
    { status: "backlog", required_field: null },
  ],
});
const summaryRow = () => ({
  id: "ORB-2",
  title: "summary row",
  status: "review",
  crew: null,
  projection: "summary",
  comment_count: 1,
  history_count: 1,
  artifact_count: 0,
  status_transitions: [{ status: "done" }, { status: "backlog" }],
});
const response = (payload, status = 200) => ({
  ok: status === 200,
  status,
  json: async () => payload,
  text: async () => JSON.stringify(payload),
});
globalThis.fetch = async (path, options = {}) => {
  const url = new URL(String(path), "http://dashboard.test");
  const method = options.method || "GET";
  // ORB-12516: an open detail also reads claim provenance. That read has its
  // own scenario; answer it and leave it out of the detail-read count below.
  if (url.pathname === "/api/distributed/claims") {
    return response({ schema_version: 1, owner_workspace: true, claims: [], capabilities: {} });
  }
  requests.push({ method, path: url.pathname, body: options.body ? JSON.parse(options.body) : null });
  if (url.pathname !== "/api/tasks/ORB-2") throw new Error(`unexpected request ${method} ${url.pathname}`);
  if (method === "PATCH") {
    if (patchFailure) return response({ error: patchFailure }, 500);
    const status = options.body ? JSON.parse(options.body).status : "review";
    const statusTransitions = status === "done"
      ? [{ status: "review", required_field: null }]
      : fullTask().status_transitions;
    return response({ ...fullTask(), status, status_transitions: statusTransitions });
  }
  if (detailFailure) return response({ error: detailFailure }, 500);
  return response(fullTask());
};
const detailReads = () => requests.filter((r) => r.method === "GET").length;

const { renderTasks } = await import("./js/tasks.js");

let tasks = [summaryRow()];
const replaced = [];
const context = {
  getTasks: () => tasks,
  replaceTask: (task) => {
    replaced.push(task);
    tasks = tasks.map((existing) => (existing.id === task.id ? task : existing));
  },
  getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["proposed", "backlog", "review", "done"]),
  statusOrder: ["review", "done", "backlog", "proposed"],
  fmtAbsTime: (value) => value,
  refreshDashboard: () => Promise.resolve(),
};
const tasksBody = document.getElementById("tasks-body");
const row = () => tasksBody.children.find((node) => node.dataset.key === "task-ORB-2");
const detail = () => tasksBody.children.find((node) => node.dataset.key === "detail-ORB-2");

// --- Collapsed: rendered from the summary, no detail read --------------------

renderTasks(tasks, context);
assert.ok(row(), "a summary row renders");
assert.equal(detail(), undefined);
assert.equal(detailReads(), 0, "a collapsed summary row costs no detail read");
const statusSelect = row().querySelector("select.task-status-select");
assert.deepEqual(
  statusSelect.children.filter((option) => option.tagName === "OPTION").map((option) => option.value),
  ["", "done", "backlog"],
  "the governed targets come from the summary's transitions",
);

// --- Expand: one read, a placeholder, then the detail from the read ----------

row().dispatch("click");
assert.equal(detailReads(), 1, "opening the row reads the detail once");
assert.ok(detail(), "the expanded row shows a detail node immediately");
assert.ok(detail().textContent.includes("Loading ORB-2"), "the first paint is a loading placeholder");
assert.equal(detail().querySelector(".panel-placeholder").getAttribute("role"), "status");
renderTasks(tasks, context);
assert.equal(detailReads(), 1, "a re-render while the read is pending does not read again");

await settle();
assert.ok(detail().textContent.includes("Body text only the detail carries"), "the detail renders the fetched body");
assert.ok(detail().textContent.includes("detail comment"), "the detail renders the fetched comments");
assert.ok(!detail().textContent.includes("Loading ORB-2"));
// Task records are agent-writable: an external ref becomes a link only for an
// http(s) URL, opened away from the operator's dashboard tab.
const refLines = detail().querySelectorAll(".external-ref-line");
assert.equal(refLines.length, 2, "both external refs render");
const refLink = refLines[0].querySelector("a");
assert.equal(refLink.href, "https://github.com/example/repo/issues/7");
assert.equal(refLink.target, "_blank");
assert.equal(refLink.rel, "noopener noreferrer");
assert.equal(refLines[1].querySelector("a"), null, "a javascript: ref must never become a link");
assert.equal(refLines[1].textContent, "hostile:1", "a rejected ref still shows its label as text");
assert.equal(replaced.length, 1, "the fetched projection replaces the summary in the list");
assert.equal(replaced[0].projection, undefined, "the replacement is the full projection");
renderTasks(tasks, context);
assert.equal(detailReads(), 1, "a full row is not read again");

// --- Refresh: new summary objects re-read, but the open detail stays painted --

tasks = [summaryRow()];
renderTasks(tasks, context);
assert.equal(detailReads(), 2, "a refreshed summary row re-reads its detail");
assert.ok(
  detail().textContent.includes("Body text only the detail carries"),
  "the open detail keeps the last read while the fresh one is in flight",
);
assert.ok(!detail().textContent.includes("Loading ORB-2"), "no placeholder flashes over an already-read detail");
await settle();
assert.equal(replaced.length, 2);

// --- Failure: the placeholder reports it and Retry reads again ----------------

tasks = [summaryRow()];
detailFailure = "store unavailable";
// A collapse forgets the cached read state for the reopen below; the cached
// projection itself is what paints while the retry is pending.
row().dispatch("click");
assert.equal(detail(), undefined, "collapsing removes the detail");
row().dispatch("click");
await settle();
assert.equal(detailReads(), 3);
// The last successful read still paints; the failure is not shown over it.
assert.ok(detail().textContent.includes("Body text only the detail carries"));

// A task never read successfully shows the failure and a retry.
tasks = [{ ...summaryRow(), id: "ORB-3", title: "never read" }];
globalThis.fetch = ((inner) => async (path, options = {}) => {
  const url = new URL(String(path), "http://dashboard.test");
  if (url.pathname === "/api/tasks/ORB-3") {
    requests.push({ method: options.method || "GET", path: url.pathname, body: null });
    if (detailFailure) return response({ error: detailFailure }, 500);
    return response({ ...fullTask(), id: "ORB-3", title: "never read" });
  }
  return inner(path, options);
})(globalThis.fetch);
renderTasks(tasks, context);
const row3 = () => tasksBody.children.find((node) => node.dataset.key === "task-ORB-3");
const detail3 = () => tasksBody.children.find((node) => node.dataset.key === "detail-ORB-3");
row3().dispatch("click");
await settle();
assert.ok(detail3().textContent.includes("Unable to load ORB-3: store unavailable"), "a failed first read is reported in place");
const retry = detail3().querySelector("button.action");
assert.equal(retry.textContent, "Retry");
const readsBeforeRetry = requests.filter((r) => r.path === "/api/tasks/ORB-3").length;
renderTasks(tasks, context);
assert.equal(
  requests.filter((r) => r.path === "/api/tasks/ORB-3").length,
  readsBeforeRetry,
  "a failed read is not retried by every render",
);
detailFailure = null;
retry.dispatch("click");
await settle();
assert.equal(requests.filter((r) => r.path === "/api/tasks/ORB-3").length, readsBeforeRetry + 1, "Retry reads once more");
assert.ok(detail3().textContent.includes("Body text only the detail carries"), "the retried read renders");

// --- Governed status change from a summary row asks the detail first ---------

// Collapse ORB-2 first so the read below is the change's own, not the open
// detail's.
tasks = [summaryRow()];
renderTasks(tasks, context);
row().dispatch("click");
assert.equal(detail(), undefined);
tasks = [summaryRow()];
detailFailure = null;
const prompts = [];
globalThis.window.prompt = (message, current) => {
  prompts.push({ message, current });
  return "Completed in the dashboard";
};
globalThis.window.confirm = () => {
  throw new Error("a governed target must not be confirmed as a forced move");
};
renderTasks(tasks, context);
const readsBeforeChange = detailReads();
const select = row().querySelector("select.task-status-select");
select.value = "done";
select.dispatch("change");
await settle();
assert.equal(detailReads(), readsBeforeChange + 1, "the change reads the detail for the evidence requirement");
assert.equal(prompts.length, 1, "the detail's requirement prompts for the evidence");
assert.ok(prompts[0].message.includes("completion summary"));
const patch = requests.find((r) => r.method === "PATCH");
assert.ok(patch, "the change is written");
assert.deepEqual(patch.body, { status: "done", execution_summary: "Completed in the dashboard" });
assert.equal(patch.body.force, undefined, "a governed change is never forced");
assert.equal(detail(), undefined, "successful status changes collapse the row");
assert.ok(row().textContent.includes("status saved"), "success feedback stays visible on the collapsed row");
const firstUndo = row().querySelector("button.mutation-undo");
assert.ok(firstUndo, "the bounded undo button stays visible on the collapsed row");

// Undo is also a status change: when invoked from an expanded row it collapses
// on success and leaves its feedback visible in the row header.
row().dispatch("click");
await settle();
assert.ok(detail(), "the row can be expanded before undo");
row().querySelector("button.mutation-undo").dispatch("click");
await settle();
assert.equal(detail(), undefined, "successful undo collapses the expanded row");
assert.ok(row().textContent.includes("status saved"), "undo success feedback remains visible");

// A failed write keeps an expanded row open and exposes the error beside its
// status control.
row().dispatch("click");
await settle();
assert.ok(detail(), "the row is expanded before the failing status change");
patchFailure = "store refused the status update";
const failedSelect = row().querySelector("select.task-status-select");
failedSelect.value = "done";
failedSelect.dispatch("change");
await settle();
assert.ok(detail(), "a failed status change leaves the row expanded");
assert.ok(row().textContent.includes("status update failed: store refused the status update"));
assert.ok(row().querySelector(".mutation-feedback.error"), "the failed PATCH error stays visible");
patchFailure = null;

// A cancelled forced change is also an error state and must not collapse the
// expanded row. Confirming the same forced target and succeeding then does.
globalThis.window.confirm = () => false;
const cancelledSelect = row().querySelector("select.task-status-select");
cancelledSelect.value = "proposed";
cancelledSelect.dispatch("change");
await settle();
assert.ok(detail(), "cancelling a forced change leaves the row expanded");
assert.ok(row().textContent.includes("status update cancelled"));

globalThis.window.confirm = () => true;
const forcedSelect = row().querySelector("select.task-status-select");
forcedSelect.value = "proposed";
forcedSelect.dispatch("change");
await settle();
assert.equal(detail(), undefined, "a successful forced status change collapses the row");
assert.ok(row().textContent.includes("status forced"), "forced success feedback stays visible");
const forcedPatch = requests.filter((r) => r.method === "PATCH").at(-1);
assert.deepEqual(forcedPatch.body, { status: "proposed", force: true });

// ORB-13495: an in-progress summary with a remote machine does not link into
// this host's run store; it names the recorded host. A local machine and a
// historical row with no recorded machine keep View run.
context.getActiveStatuses = () => new Set(["in-progress", "proposed", "backlog", "review", "done"]);
const runSummary = (id, extra) => ({
  id,
  title: id,
  status: "in-progress",
  crew: null,
  projection: "summary",
  comment_count: 0,
  history_count: 0,
  artifact_count: 0,
  status_transitions: [],
  job_run_id: `jrun-${id}`,
  ...extra,
});
tasks = [
  runSummary("ORB-R", {
    job_run_navigable: false,
    job_run_machine: { machine_id: "remote-box", machine_name: "remote-host" },
  }),
  runSummary("ORB-L", {
    job_run_navigable: true,
    job_run_machine: { machine_id: "local-box", machine_name: "this-host" },
  }),
  runSummary("ORB-H", { job_run_navigable: true }),
];
renderTasks(tasks, context);
const quickCell = (id) => {
  const node = tasksBody.children.find((child) => child.dataset.key === `task-${id}`);
  assert.ok(node, `${id} renders from the summary`);
  const cell = node.querySelector(".task-quick-cell");
  assert.ok(cell, `${id} has a quick action`);
  return cell;
};
const remoteCell = quickCell("ORB-R");
assert.equal(remoteCell.querySelector("a"), null, "a remote summary has no local View run link");
assert.equal(remoteCell.textContent.includes("View run"), false);
assert.ok(remoteCell.textContent.includes("remote-box"), "a remote summary names the recorded machine");
assert.ok(remoteCell.textContent.includes("remote-host"), "a remote summary names the recorded host");
for (const id of ["ORB-L", "ORB-H"]) {
  const link = quickCell(id).querySelector("a.task-quick-link");
  assert.ok(link, `${id} keeps View run`);
  assert.equal(link.textContent, "View run");
  assert.equal(link.href, `#runs?run_id=${encodeURIComponent(`jrun-${id}`)}`);
}

// --- Action forms return focus; a failed action is announced ------------------

// Closing the comment or reject form swaps the actions row back in, which drops
// focus to the page. It goes back to the control that opened the form so the
// keyboard user is where they were.
context.getActiveStatuses = () => new Set(["proposed", "backlog", "review", "done"]);
tasks = [summaryRow()];
renderTasks(tasks, context);
if (!detail()) row().dispatch("click");
await settle();
assert.ok(detail(), "the detail is open for the action checks");
const actionButton = (name) => detail().querySelector(`button.action.${name}`);
actionButton("comment").dispatch("click");
assert.ok(detail().querySelector("textarea"), "the comment form opens");
assert.equal(document.activeElement.tagName, "TEXTAREA", "opening the form focuses its field");
actionButton("cancel").dispatch("click");
assert.equal(detail().querySelector("textarea"), null, "cancel closes the comment form");
assert.ok(document.activeElement === actionButton("comment"), "cancelling the comment form returns focus to Comment");
actionButton("reject").dispatch("click");
actionButton("cancel").dispatch("click");
assert.ok(document.activeElement === actionButton("reject"), "cancelling the reject form returns focus to Reject");

// The stub refuses every action endpoint, so the row reports a failure. It
// must be a live alert: a plain div appearing above the buttons is never read.
actionButton("approve").dispatch("click");
await settle();
const actionError = detail().querySelector(".action-error");
assert.ok(actionError, "a refused action is reported in the detail");
assert.equal(actionError.getAttribute("role"), "alert", "a refused action is announced");

console.log("dashboard summary rows expand through the detail endpoint");
