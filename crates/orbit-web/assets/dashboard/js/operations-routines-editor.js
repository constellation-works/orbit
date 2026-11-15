// Routine trigger editor bridge for the Routines catalogue.
//
// Narrow interface between the catalogue and the editor:
//   catalogue -> editor: `editorVisibleIn(container)` and
//     `focusEditorIn(container, { focusControl })` only.
//   editor -> catalogue: nothing; the editor never reads catalogue state.
//
// Invariant (stated at this interface): list controls are disabled exactly
// when a visible editor is open, and filter input never writes an editor
// draft. The catalogue asks only whether a visible editor exists; it never
// reads or writes the draft, the open routine, or the observed trigger.
// The draft itself lives in schedule-editor.js; this module owns only the
// routine-specific open/edit/save wiring and focus placement.

import { getWorkspace, postJson } from './common.js';
import { closeEditors, editorObserved, isOpen, openEditor, routineDraft, routineTriggerEditor, scheduleEditButton } from './schedule-editor.js';
import { controlReason, cronDraftText, feedback, selectionSnapshot } from './operations-shared.js';

export function closeRoutineEditors() {
  closeEditors("routine");
}

function routineEditKey(routine) {
  return `routine-trigger:${getWorkspace()}:${routine.name}`;
}

export function routineEditButton({ payload, routine, onOpened }) {
  const key = routineEditKey(routine);
  return scheduleEditButton({
    key,
    label: `${routine.name} trigger`,
    reason: controlReason(payload, "routine_trigger"),
    onOpen: () => {
      openEditor(key, "routine", routineDraft(routine.trigger), { source: routine.source, target: routine.target, trigger: routine.trigger });
      onOpened();
    },
  });
}

export function routineEditorElement({ payload, routine, nextAt, onClose }) {
  const key = routineEditKey(routine);
  if (!isOpen(key)) return null;
  const selection = selectionSnapshot();
  return routineTriggerEditor({
    key,
    routine,
    cronPreview: (cron) => cronDraftText(cron, routine.cron || routine.trigger?.cron, nextAt),
    save: (trigger) => {
      const observed = editorObserved(key) || { source: routine.source, target: routine.target, trigger: routine.trigger };
      return postJson("/api/routines/trigger", {
        name: routine.name, source: observed.source, target: observed.target,
        machine_name: payload.machine_name, expected_trigger: observed.trigger, trigger,
      }).then((result) => {
        if (selection.current()) feedback("routine-operation-feedback", "success", `${result.message}: ${routine.name}.`);
        return result;
      });
    },
    onClose,
  });
}

/** Whether a visible editor is present inside `container`. */
export function editorVisibleIn(container) {
  return [...(container?.querySelectorAll(".schedule-editor") || [])].some((editor) => {
    for (let parent = editor.parentElement; parent && parent !== container; parent = parent.parentElement) {
      if (parent.matches("details:not([open])")) return false;
    }
    return true;
  });
}

/**
 * Focus the first enabled, visible control of the open editor. A cron-only
 * editor disables its Trigger select, and a pending save disables every
 * control, so with none available focus the editor itself, not the page.
 * With `focusControl: false` focus only the panel: later filter characters
 * must not type into a cron input or activate a Trigger select's type-ahead.
 */
export function focusEditorIn(container, { focusControl = true } = {}) {
  const editor = container?.querySelector(".schedule-editor");
  if (!editor) return;
  const control = [...editor.querySelectorAll("input, select, button")]
    .find((node) => !node.disabled && node.checkVisibility());
  if (focusControl && control) control.focus();
  else {
    editor.tabIndex = -1;
    editor.focus();
  }
}
