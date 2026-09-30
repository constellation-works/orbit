// A keyboard user must keep their place when the dashboard rebuilds what they
// just used.
//
// Expanding a row, a background refresh, or cancelling an inline edit replaces
// nodes, and a browser answers the removal of the focused node by dropping focus
// to <body>: the next Tab starts again from the top of the page. These
// scenarios drive the shipped modules and observe where focus lands. The DOM
// adapter models the browser's rule (removing the focused node blurs it), which
// is the behavior the fix has to answer.
import assert from "node:assert/strict";

const NodeClass = document.createElement("div").constructor;
const originalRemoveChild = NodeClass.prototype.removeChild;
NodeClass.prototype.removeChild = function removeChild(child) {
  const removed = originalRemoveChild.call(this, child);
  if (document.activeElement && child.contains(document.activeElement)) document.activeElement = document.body;
  return removed;
};

// --- An expanded task row keeps focus ---------------------------------------

const { renderTasks } = await import("./js/tasks.js");
const task = { id: "ORB-1", title: "keeps focus", status: "review", artifacts: [] };
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
renderTasks([task], context);

const collapsedRow = taskRow();
collapsedRow.focus();
collapsedRow.dispatch("keydown", { key: "Enter" });
assert.notEqual(taskRow(), collapsedRow, "expanding rebuilds the row, which is what used to strand focus");
assert.equal(document.activeElement, taskRow(), "after Enter expands a row, focus stays on that row");

// The copy button inside a row is rebuilt with it and focus follows to its new copy.
const idButton = () => taskRow().children.find((node) => node.tagName === "BUTTON" && node.classList.contains("copy-id"));
const staleButton = idButton();
staleButton.focus();
taskRow().dispatch("keydown", { key: " " });
assert.equal(taskRow().getAttribute("aria-expanded"), "false", "the toggle ran");
assert.notEqual(idButton(), staleButton, "the row and its button were rebuilt");
assert.equal(document.activeElement, idButton(), "focus follows the copy button to its rebuilt counterpart");

// --- syncNodes: nested controls, and leaving focus alone --------------------

const { syncNodes } = await import("./js/common.js");
const container = document.createElement("div");
const buildRow = (key, hash, labels) => {
  const row = document.createElement("div");
  row.dataset.key = key;
  row.dataset.hash = hash;
  row.tabIndex = 0;
  for (const label of labels) {
    const button = document.createElement("button");
    button.className = `action ${label}`;
    button.tabIndex = 0;
    row.appendChild(button);
  }
  return row;
};

syncNodes(container, [buildRow("a", "1", ["approve", "ship"]), buildRow("b", "1", ["approve"])]);
container.children[0].children[1].focus();
syncNodes(container, [buildRow("a", "2", ["approve", "ship"]), buildRow("b", "1", ["approve"])]);
assert.equal(document.activeElement, container.children[0].children[1], "a nested control of a rebuilt row is matched by tag and class");
assert.equal(document.activeElement.className, "action ship");

// A rebuilt row that no longer has that control puts focus on the row instead.
syncNodes(container, [buildRow("a", "3", ["approve"]), buildRow("b", "1", ["approve"])]);
assert.equal(document.activeElement, container.children[0], "a control that disappeared hands focus to its row");

// A row that is unchanged keeps its own node and focus with it.
container.children[1].children[0].focus();
const stable = container.children[1];
syncNodes(container, [buildRow("a", "3", ["approve"]), buildRow("b", "1", ["approve"])]);
assert.equal(container.children[1], stable);
assert.equal(document.activeElement, stable.children[0]);

// A toggle that changes its own classes when used (a chip going on or off) is
// still the same control after the rebuild.
const chipRow = (on) => {
  const row = document.createElement("div");
  row.dataset.key = "chips";
  row.dataset.hash = on ? "on" : "off";
  const chip = document.createElement("button");
  chip.className = on ? "chip on" : "chip";
  row.appendChild(chip);
  return row;
};
syncNodes(container, [chipRow(false)]);
container.children[0].children[0].focus();
syncNodes(container, [chipRow(true)]);
assert.equal(document.activeElement, container.children[0].children[0], "a control that toggled its own class keeps focus");
syncNodes(container, [buildRow("a", "4", ["approve"]), buildRow("b", "1", ["approve"])]);

// Focus that is not in the container is never taken.
const elsewhere = document.createElement("button");
elsewhere.tabIndex = 0;
elsewhere.focus();
syncNodes(container, [buildRow("a", "4", ["approve"]), buildRow("b", "1", ["approve"])]);
assert.equal(document.activeElement, elsewhere, "a refresh does not steal focus from outside the panel");

// When the focused row is gone for good there is nothing to return to.
container.children[0].focus();
syncNodes(container, [buildRow("b", "1", ["approve"])]);
assert.equal(document.activeElement, document.body, "no counterpart, no invented focus target");

// --- Inline field editor: Escape cancels and focus returns to Edit ----------

const { buildInlineFieldEditor } = await import("./js/field-editor.js");
let editing = false;
const editor = buildInlineFieldEditor({
  label: "title",
  value: "old",
  renderView: () => document.createTextNode("old"),
  save: async () => ({}),
  onSaved: () => {},
  multiline: false,
  onEditingChange: (value) => { editing = value; },
});
const byClass = (name) => editor.children.find((node) => node.classList && node.classList.contains(name));
byClass("field-edit").dispatch("click");
assert.equal(editing, true, "Edit opens the editor");
const input = byClass("field-editor-input");
assert.equal(document.activeElement, input, "opening the editor focuses its field");

input.dispatch("keydown", { key: "x" });
assert.equal(editing, true, "an ordinary key does nothing");
input.dispatch("keydown", { key: "Escape" });
assert.equal(editing, false, "Escape leaves the editor without saving");
assert.equal(byClass("field-editor-input"), undefined, "the editor is gone");
assert.equal(document.activeElement, byClass("field-edit"), "focus returns to the Edit button that opened it");

// A save in flight owns the widget: Escape must not tear it down.
byClass("field-edit").dispatch("click");
const controls = byClass("field-editor-controls");
const saveButton = controls.children.find((node) => node.classList.contains("save"));
saveButton.disabled = true;
byClass("field-editor-input").dispatch("keydown", { key: "Escape" });
assert.equal(editing, true, "Escape is ignored while a save is in flight");
