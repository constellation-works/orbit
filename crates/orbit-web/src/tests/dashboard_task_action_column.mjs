// Rows reserve a fixed action column so titles and selects line up between
// groups. When no visible row has an action the column is dead space, and the
// list drops it for every row at once. The flag has to agree with what each
// row actually renders, so the scenario compares the two across the statuses
// that do and do not offer an action.
import assert from "node:assert/strict";

const { renderTasks } = await import("./js/tasks.js");

const tasksBody = document.getElementById("tasks-body");
const statuses = ["proposed", "backlog", "in-progress", "review", "blocked", "done"];
const context = (list) => ({
  getTasks: () => list,
  getSearchQuery: () => "",
  getActiveStatuses: () => new Set(statuses),
  statusOrder: statuses,
  statusUpdateTargets: [],
  fmtAbsTime: (value) => value,
  refreshDashboard: () => Promise.resolve(),
});
const render = (list) => renderTasks(list, context(list));
const dropsColumn = () => tasksBody.dataset.quickActions === "none";
const quickCell = (id) => {
  const row = tasksBody.children.find((node) => node.dataset.key === `task-${id}`);
  return row.children.find((cell) => cell.classList.contains("task-quick-cell"));
};

const variants = [
  { id: "P", status: "proposed" },
  { id: "B", status: "backlog" },
  { id: "R", status: "review" },
  { id: "K", status: "blocked" },
  { id: "D", status: "done" },
  { id: "IP", status: "in-progress" },
  { id: "IR", status: "in-progress", job_run_id: "jrun-1" },
  { id: "IX", status: "in-progress", job_run_id: "jrun-2", job_run_navigable: false },
  { id: "IH", status: "in-progress", job_run_id: "jrun-3", job_run_navigable: false, job_run_machine: { machine_id: "hm_1", machine_name: "box" } },
].map((task) => ({ title: `task ${task.id}`, artifacts: [], ...task }));

for (const task of variants) {
  render([task]);
  const offersAction = quickCell(task.id).children.length > 0;
  assert.equal(
    dropsColumn(),
    !offersAction,
    `the action column is dropped exactly when the only visible row (${task.id}, ${task.status}) renders no action`,
  );
}

// One row with an action keeps the column for every row, including rows without one.
render([variants.find((t) => t.id === "D"), variants.find((t) => t.id === "B")]);
assert.equal(dropsColumn(), false, "a single row with an action keeps the column for the whole list");
assert.equal(quickCell("D").children.length, 0, "the row without an action still holds its (empty) cell, so columns stay aligned");

render([variants.find((t) => t.id === "D"), variants.find((t) => t.id === "R")]);
assert.equal(dropsColumn(), true, "a list with no actionable row gives the column back");
