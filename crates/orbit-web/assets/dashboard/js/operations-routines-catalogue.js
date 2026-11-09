// Routine catalogue: filter, paging groups, paused collapse and list
// control enablement for the Routines view.
//
// This module owns catalogue mutable state only (`routineList`: filter
// text and paused-collapse). It never reads or writes an editor draft.
// It talks to the editor solely through operations-routines-editor.js:
// `editorVisibleIn(container)` and `focusEditorIn(container, ...)`.
//
// Invariant: list controls are disabled exactly when a visible editor is
// open, and filter input never writes an editor draft. Every repaint
// re-derives enablement from `editorVisibleIn`, so paging/filtering back
// to an open editor disables the controls again, and revealing an editor
// mid-query parks focus on its panel instead of an editing control.

import { el } from './common.js';
import { operationColumns, operationGroup } from './operations-shared.js';
import { editorVisibleIn, focusEditorIn } from './operations-routines-editor.js';

const routineList = { query: "", pausedOpen: false };

export function resetRoutineCatalogue() {
  Object.assign(routineList, { query: "", pausedOpen: false });
}

// List navigation is local operator state, retained when fresh status arrives.
// Only the list is filtered: health warnings and the timeline always use every routine.
export function createRoutineCatalogue({ payload, routines, workspaceId, renderRow, renderTimeline, routineState, failingRoutines }) {
  const catalogue = el("div", { class: "routine-catalogue" });
  const search = el("input", { id: "routine-filter", type: "search" });
  search.dataset.routineFocus = "filter";
  search.placeholder = "Name, job or description";
  search.value = routineList.query;
  const label = el("label", { text: "Filter routines" });
  label.htmlFor = search.id;
  const clear = el("button", { class: "operation-button", type: "button", text: "Clear" });
  clear.dataset.routineFocus = "clear";
  const count = el("span", { class: "routine-list-count", role: "status" });
  const groups = el("div", { id: "routine-list-groups" });
  search.setAttribute("aria-controls", groups.id);
  catalogue.append(el("div", { class: "routine-list-toolbar" }, [label, search, clear, count]), renderTimeline(routines), groups);

  const syncControls = () => {
    const editing = editorVisibleIn(groups);
    search.disabled = editing;
    search.title = editing ? "Close the trigger editor to filter routines." : "";
    clear.disabled = editing || !routineList.query;
  };
  const paintGroups = () => {
    groups.textContent = "";
    const query = routineList.query.trim().toLocaleLowerCase();
    const matches = routines.filter(routine => [routine.name, routine.target, routine.description]
      .some(value => String(value || "").toLocaleLowerCase().includes(query)));
    count.textContent = query ? `${matches.length} of ${routines.length} routines match` : `${routines.length} routines`;
    for (const [kind, title, hint] of [
      ["active", "Active", "enabled and delivered by the host clock"],
      ["paused", "Paused", "slots are skipped, not queued"],
    ]) {
      const all = routines.filter(routine => Boolean(routine.enabled) === (kind === "active"));
      if (!all.length) continue;
      const filtered = matches.filter(routine => Boolean(routine.enabled) === (kind === "active"));
      const group = operationGroup(kind, title, query ? `${filtered.length} of ${all.length}` : all.length, hint);
      const states = [
        ["blocked", all.filter(routine => routineState(routine) === "blocked").length],
        ["stalled", all.filter(routine => routine.automation?.condition?.stall).length],
        ["failing repeatedly", failingRoutines(all).length],
      ].filter(([, total]) => total);
      if (states.length) group.firstChild.appendChild(el("span", {
        class: "routine-group-warning", text: `${query ? `Across all ${all.length}: ` : ""}${states.map(([state, total]) => `${total} ${state}`).join(" · ")}`,
      }));
      let container = group;
      if (kind === "paused") {
        container = el("details", { class: "operation-group routine-paused" });
        container.open = routineList.pausedOpen;
        const summary = el("summary", { class: "operation-group-head" });
        summary.dataset.routineFocus = "paused";
        summary.append(...group.firstChild.childNodes);
        container.appendChild(summary);
        container.addEventListener("toggle", () => {
          if (container.isConnected) {
            routineList.pausedOpen = container.open;
            syncControls();
          }
        });
      }
      if (filtered.length) {
        container.append(operationColumns(["", "Routine", "Cadence", "Next fire", "Last run"]));
        for (const routine of filtered) container.appendChild(renderRow(payload, routine, workspaceId));
      } else {
        container.appendChild(el("p", { class: "routine-list-empty", text: `No ${kind} routines match. Clear or change the filter.` }));
      }
      groups.appendChild(container);
    }
    syncControls();
  };
  search.addEventListener("input", () => {
    routineList.query = search.value;
    if (routineList.query.trim()) routineList.pausedOpen = true;
    const typing = document.activeElement === search;
    paintGroups();
    // Revealing an editor locks the filter mid-query. Park focus on its panel
    // until the operator deliberately tabs or clicks into an editing control.
    if (typing && search.disabled) focusEditorIn(groups, { focusControl: false });
  });
  clear.addEventListener("click", () => {
    search.value = "";
    search.dispatchEvent(new Event("input"));
    if (search.disabled) focusEditorIn(groups);
    else search.focus();
  });
  paintGroups();
  return catalogue;
}
