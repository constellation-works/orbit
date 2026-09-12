// Orbit dashboard inline field editor.
//
// One field, one widget: a read-only view carrying an `edit` affordance that
// swaps in a text editor with Save and Cancel. The widget owns that edit cycle
// and nothing else — open, save in flight, the inline error a rejected save
// reports, cancel — so it does not know which record it edits or where the
// value is persisted. The caller supplies the current text, a renderer for the
// read-only view, an async `save`, and what to do once a save lands.

import { el } from './common.js';

/* A save that resolves hands the surrounding view back to its own data: the
   caller re-renders, which replaces this node, so the widget deliberately has
   no post-save view of its own. A save that rejects keeps the editor open with
   the operator's text intact — that text is the only copy of the draft. */
export function buildInlineFieldEditor({
  label,
  value,
  renderView,
  save,
  onSaved,
  editable = true,
  editTitle = "",
  disabledTitle = "",
  multiline = true,
  placeholder = "",
  hint = "",
  toggle = null,
  onEditingChange = () => {},
}) {
  const wrap = el("div", { class: "field-editor" });
  const accessibleName = editTitle || `Edit ${label}`;

  // Each phase is built on demand so neither can hold state from the other:
  // cancelling discards the editor outright, and opening discards the view.
  const showView = () => {
    const editButton = el("button", {
      class: "field-edit",
      text: "edit",
      title: editable ? accessibleName : disabledTitle,
    });
    editButton.type = "button";
    editButton.disabled = !editable;
    editButton.setAttribute("aria-label", accessibleName);
    editButton.addEventListener("click", (event) => {
      event.stopPropagation();
      // The disabled attribute already refuses the pointer and the keyboard;
      // the same refusal is re-stated here so "not editable" is a rule of the
      // widget rather than a property of how it happens to be rendered.
      if (editable) showEditor();
    });
    wrap.replaceChildren(renderView(), editButton);
  };

  const showEditor = () => {
    const input = el(multiline ? "textarea" : "input", { class: "field-editor-input mono" });
    if (!multiline) input.type = "text";
    input.value = value;
    if (placeholder) input.placeholder = placeholder;
    input.setAttribute("aria-label", accessibleName);

    const toggleInput = toggle ? el("input", { class: "field-editor-toggle-input" }) : null;
    const toggleRow = toggle ? el("label", { class: "field-editor-toggle", title: toggle.title || "" }) : null;
    if (toggle) {
      toggleInput.type = "checkbox";
      toggleInput.checked = false;
      toggleRow.appendChild(toggleInput);
      toggleRow.appendChild(el("span", { text: toggle.label }));
    }
    const toggleValue = () => (toggle ? { [toggle.key]: Boolean(toggleInput.checked) } : {});

    // The status line is a live region so the save's progress reaches a screen
    // reader, and the error is an alert because it interrupts the save.
    const status = el("span", { class: "field-editor-status" });
    status.setAttribute("role", "status");
    status.setAttribute("aria-live", "polite");
    const error = el("div", { class: "field-editor-error" });
    error.setAttribute("role", "alert");
    error.hidden = true;

    const saveButton = el("button", { class: "action save", text: "save" });
    saveButton.type = "button";
    const cancelButton = el("button", { class: "action cancel", text: "cancel" });
    cancelButton.type = "button";

    saveButton.addEventListener("click", async (event) => {
      event.stopPropagation();
      saveButton.disabled = true;
      cancelButton.disabled = true;
      status.textContent = "saving…";
      error.hidden = true;
      error.textContent = "";
      try {
        const saved = await save(input.value, toggleValue());
        onEditingChange(false);
        onSaved(saved);
      } catch (failure) {
        saveButton.disabled = false;
        cancelButton.disabled = false;
        status.textContent = "";
        error.hidden = false;
        error.textContent = failure.message || String(failure);
      }
    });
    cancelButton.addEventListener("click", (event) => {
      event.stopPropagation();
      onEditingChange(false);
      showView();
    });

    const children = [input];
    if (hint) children.push(el("div", { class: "field-editor-hint", text: hint }));
    if (toggleRow) children.push(toggleRow);
    children.push(el("div", { class: "field-editor-controls" }, [saveButton, cancelButton, status]));
    children.push(error);
    wrap.replaceChildren(...children);
    onEditingChange(true);
    input.focus();
  };

  showView();
  return wrap;
}
