// Orbit dashboard task field editors, file and context rows, and complexity.

import { captureWorkspaceVisit, el, patchJson, copyText, copyWithFeedback, bindTaskLink, clockStamp, formatDateTime } from './common.js';
import { renderMarkdown, renderMarkdownInline } from './markdown.js';
import { buildInlineFieldEditor } from './field-editor.js';
import { onTaskRuntimeChange, taskContextContenders } from './task-runtime.js';
import { taskState, rerender, canMutateTask, aggregateRefusalTitle, taskMutationPath, isOsTag, MUTATION_UNDO_WINDOW_MS } from './task-shared.js';
import { scheduleFeedbackExpiry, buildMutationFeedback } from './task-crew.js';
import { applyUpdatedTask } from './task-data.js';

// A saved text field has no undo (restoring prose would need a second write of
// its own), so its note only has to stay long enough to be read.
const FIELD_SAVED_NOTICE_MS = 4000;

function buildTagRow(tags) {
  const wrap = el("div", { class: "detail-tag-row" });
  for (const tag of tags) {
    wrap.appendChild(isOsTag(tag)
      ? el("span", { class: "chip os-chip", text: tag, title: `Admitted only on a host running ${tag.slice(3)}` })
      : el("span", { class: "chip", text: tag }));
  }
  return wrap;
}

/* ORB-12235: the task fields the expanded detail can edit in place. Each entry
   reads the task's current value as editor text, renders the read-only view,
   and turns the edited text back into a PATCH body carrying that one field —
   never a whole task, which would clobber whatever an agent wrote concurrently.
   `complexity` is not here: it is a fixed set of choices, so it gets a select
   (buildComplexityUpdateControl) rather than a text editor. */
export const TASK_FIELD_EDITORS = {
  not_before: {
    label: "scheduled time",
    multiline: false,
    toText: (task) => task.not_before || "",
    renderView: (task) => task.not_before
      ? clockStamp(el("span", { text: formatDateTime(task.not_before) }), task.not_before)
      : emptyFieldView("not scheduled"),
    toPayload: (text) => ({ not_before: text.trim() }),
    placeholder: "2099-02-01T10:00:00Z",
    hint: "RFC 3339 with an offset (Z for UTC). Leave empty to clear. Starts on the next admission pass at or after this time.",
  },
  description: {
    label: "description",
    toText: (task) => task.description || "",
    renderView: (task) =>
      task.description && task.description.trim()
        ? markdownView(task.description)
        : emptyFieldView("no description"),
    toPayload: (text) => ({ description: text }),
    placeholder: "Markdown description",
  },
  acceptance_criteria: {
    label: "acceptance criteria",
    toText: (task) => linesToText(task.acceptance_criteria),
    renderView: (task) =>
      Array.isArray(task.acceptance_criteria) && task.acceptance_criteria.length > 0
        ? buildCriteriaList(task.acceptance_criteria)
        : emptyFieldView("no acceptance criteria"),
    toPayload: (text) => ({ acceptance_criteria: textToLines(text) }),
    hint: "One criterion per line.",
  },
  tags: {
    label: "tags",
    multiline: false,
    toText: (task) => (Array.isArray(task.tags) ? task.tags.join(", ") : ""),
    renderView: (task) =>
      Array.isArray(task.tags) && task.tags.length > 0
        ? buildTagRow(task.tags)
        : emptyFieldView("no tags"),
    toPayload: (text) => ({ tags: splitTagText(text) }),
    placeholder: "comma, separated, tags",
  },
  context_files: {
    label: "context files",
    toText: (task) => linesToText(task.context_files),
    // Keep the empty state in the same refreshable list as populated
    // selectors, so a background refresh can replace it without rebuilding an
    // open detail that contains another field's draft.
    renderView: (task) => buildFileList(task),
    // The server refuses a selector that names nothing unless the caller says
    // the task is about to create it, so that escape is the editor's own
    // checkbox rather than a second, silently different code path.
    toPayload: (text, options) => {
      const payload = { context_files: textToLines(text) };
      if (options && options.allow_missing_context) payload.allow_missing_context = true;
      return payload;
    },
    hint: "One selector per line (file:…, dir:…, symbol:…).",
    toggle: {
      key: "allow_missing_context",
      label: "allow missing context",
      title: "Accept a selector naming a target this task will create",
    },
  },
};

export function markdownView(text) {
  const view = el("div", { class: "markdown-body" });
  const rendered = renderMarkdown(text);
  if (rendered !== null) {
    view.innerHTML = rendered;
  } else {
    view.textContent = text;
  }
  return view;
}

function emptyFieldView(text) {
  return el("div", { class: "field-empty", text });
}

export function taskMarkdownPanel(content) {
  return el("div", { class: "task-markdown-panel" }, [content]);
}

// Numbered, because criteria are cited by number ("criterion 3 is met").
function buildCriteriaList(criteria) {
  const ul = el("ol", { class: "ac-list" });
  for (const ac of criteria) {
    const rendered = renderMarkdownInline(ac);
    if (rendered !== null) {
      const li = el("li");
      li.innerHTML = rendered;
      ul.appendChild(li);
    } else {
      ul.appendChild(el("li", { text: ac }));
    }
  }
  return ul;
}

function buildFileList(task) {
  const wrap = el("div", { class: "context-files-view" });
  const summary = el("span", { class: "cf-summary" });
  const copyAll = el("button", { class: "cf-copy-all", type: "button", text: "Copy selectors" });
  copyAll.addEventListener("click", () => {
    const t = ul.contentionTask;
    const paths = t && Array.isArray(t.context_files) ? t.context_files : [];
    if (paths.length) copyWithFeedback(copyAll, paths.join("\n"));
  });
  const header = el("div", { class: "cf-header" }, [summary, copyAll]);
  const banner = el("div", { class: "cf-banner", role: "status" });
  const ul = el("ul", { class: "file-list mono" });
  ul.contentionTask = task;
  ul.cfRefs = { wrap, summary, banner, header };
  wrap.appendChild(header);
  wrap.appendChild(banner);
  wrap.appendChild(ul);
  refreshFileList(ul);
  return wrap;
}

function parseContextSelector(selector) {
  const match = /^([a-z]+):(.*)$/.exec(selector);
  if (!match) return { kind: "file", target: selector, cut: 0, base: selector, dir: "" };
  const [, kind, target] = match;
  let cut = 0;
  if (kind === "file") {
    cut = target.lastIndexOf("/") + 1;
  } else if (kind === "dir") {
    const trimmed = target.endsWith("/") ? target.slice(0, -1) : target;
    cut = trimmed.lastIndexOf("/") + 1;
  }
  return {
    kind,
    target,
    cut,
    base: target.slice(cut),
    dir: target.slice(0, cut),
  };
}

function formatDirectory(dirPath) {
  const span = el("span", { class: "selector-dim" });
  if (!dirPath) return span;
  const parts = dirPath.split("/");
  for (let i = 0; i < parts.length; i++) {
    if (i < parts.length - 1) {
      const seg = `${parts[i]}/`;
      if (i === 0) {
        span.appendChild(el("span", { class: "selector-root", text: seg }));
      } else {
        span.appendChild(document.createTextNode(seg));
      }
      span.appendChild(el("wbr"));
    } else if (parts[i]) {
      span.appendChild(document.createTextNode(parts[i]));
    }
  }
  return span;
}

function cfPathForSelector(p) {
  let target = p.target || "";
  if (p.kind === "symbol") {
    let gt = target.split("#")[0];
    const ci = gt.indexOf("::");
    if (ci > 0) gt = gt.slice(0, ci);
    target = gt;
  }
  return target;
}

// One flat list, ordered by full path so files of one directory sit together.
function cfSortedEntries(entries) {
  return [...entries].sort((x, y) => {
    const a = cfPathForSelector(x.p);
    const b = cfPathForSelector(y.p);
    return a < b ? -1 : a > b ? 1 : x.selector < y.selector ? -1 : x.selector > y.selector ? 1 : 0;
  });
}

function cfSummaryText(entries) {
  const total = entries.length;
  const fileLabel = `${total} file${total === 1 ? "" : "s"}`;
  const dirs = new Set(entries.map((e) => {
    const target = cfPathForSelector(e.p);
    const cut = target.lastIndexOf("/");
    return cut >= 0 ? target.slice(0, cut) : "";
  }));
  if (total === 0) return fileLabel;
  if (dirs.size === 1) return `${fileLabel} in ${[...dirs][0] || "the project root"}`;
  return `${fileLabel} in ${dirs.size} folders`;
}

function cfHolderLabel(holder) { return `${holder.id} · ${String(holder.status || "").replaceAll("-", " ")}`; }

function makeContextRow(entry, task) {
  const { selector, p, matches } = entry;
  const state = matches === null ? "Contention unknown: current workspace data unavailable"
    : matches && matches.length ? task.status === "backlog" ? "Contended context" : "Active context contention" : "";
  const children = [];
  if (p.kind && p.kind !== "file") {
    const kindNode = el("span", { class: "selector-kind", text: `${p.kind}:` });
    kindNode.dataset.kind = p.kind;
    children.push(kindNode);
  }
  if (p.kind !== "symbol") children.push(formatDirectory(p.dir || (p.target.includes("/") ? p.target.slice(0, p.target.lastIndexOf("/") + 1) : "")));
  children.push(el("span", { class: "selector-base", text: p.base || p.target }));
  const link = el("a", { class: "context-selector", title: [selector, state].filter(Boolean).join(" · ") }, children);
  bindTaskLink(link, task.id, task.workspace_id ? { workspace: task.workspace_id } : {});
  if (state) link.setAttribute("aria-label", `${selector}: ${state}`);
  const rowChildren = [link];
  if (matches && matches.length) {
    rowChildren.push(el("span", { class: "cf-pill cf-pill-contended", text: `held by ${matches.map(cfHolderLabel).join(", ")}` }));
  } else if (matches === null && (task.status === "backlog" || task.status === "in-progress")) {
    rowChildren.push(el("span", { class: "cf-pill cf-pill-unknown", text: "contention unknown" }));
  }
  const copyBtn = el("button", { class: "cf-copy", type: "button", title: "Copy selector" }, [el("span", { text: "⧉", "aria-hidden": "true" })]);
  copyBtn.setAttribute("aria-label", `Copy ${selector}`);
  copyBtn.addEventListener("click", (event) => { event.stopPropagation(); copyText(selector); });
  rowChildren.push(copyBtn);
  const li = el("li", { class: "cf-row" }, rowChildren);
  if (matches === null) li.classList.add("context-contention-unknown");
  if (matches && matches.length) li.classList.add(task.status === "backlog" ? "context-contended-backlog" : "context-contended-active");
  return li;
}

export function refreshFileList(ul) {
  const task = ul.contentionTask;
  if (!task) return;
  const applicable = task.status === "backlog" || task.status === "in-progress";
  const rawPaths = Array.isArray(task.context_files) ? task.context_files : [];
  const entries = rawPaths.map((selector) => {
    const p = parseContextSelector(selector);
    return { selector, p, matches: applicable ? taskContextContenders(task, selector) : [] };
  });
  const refs = ul.cfRefs;
  if (refs) {
    refs.header.hidden = entries.length === 0;
    refs.summary.textContent = cfSummaryText(entries);
    const contended = entries.filter((e) => e.matches && e.matches.length);
    if (contended.length) {
      const holders = [...new Set(contended.flatMap((e) => e.matches.map((h) => h.id)))];
      refs.banner.hidden = false;
      refs.banner.textContent = `${contended.length} file${contended.length === 1 ? "" : "s"} overlap${contended.length === 1 ? "s" : ""} an in-progress task: held by ${holders.join(", ")}`;
    } else { refs.banner.hidden = true; refs.banner.textContent = ""; }
  }
  if (entries.length === 0) { ul.replaceChildren(el("li", { class: "field-empty", text: "no context files" })); return; }
  ul.replaceChildren(...cfSortedEntries(entries).map((e) => makeContextRow(e, task)));
}

// Update only the displayed selectors, including details preserved for an
// unsaved editor/comment. Never replace an editor or discard its draft.
onTaskRuntimeChange(() => {
  for (const ul of document.querySelectorAll(".file-list")) {
    if (ul.contentionTask) refreshFileList(ul);
  }
});

export function linesToText(values) {
  return Array.isArray(values) ? values.join("\n") : "";
}

// A blank line is how an operator separates entries while typing, not an empty
// criterion or selector, so it never reaches the server.
function textToLines(text) {
  return String(text)
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

function splitTagText(text) {
  return String(text)
    .split(/[,\n]/)
    .map((tag) => tag.trim())
    .filter(Boolean);
}

export function fieldFeedbackKey(taskId, field) {
  return `${taskId}:${field}`;
}

export function taskFieldHasValue(task, spec) {
  return spec.toText(task).trim() !== "";
}

/* ORB-11655: a detail holding an open editor is reused verbatim by the next
   background refresh, the same way an open comment form is, so the operator's
   unsaved text survives the poll. The open editors are tracked on the node
   itself because that node is the thing being kept, and under their own dataset
   key so closing one cannot cancel a comment draft's claim on the same node. */
function setDetailEditing(detail, field, open) {
  if (!detail) return;
  const fields = new Set((detail.dataset.editing || "").split(",").filter(Boolean));
  if (open) fields.add(field);
  else fields.delete(field);
  if (fields.size > 0) detail.dataset.editing = [...fields].join(",");
  else delete detail.dataset.editing;
}

export function buildTaskFieldEditor(task, field, detail, context, editSlot = null) {
  const visit = captureWorkspaceVisit();
  const spec = TASK_FIELD_EDITORS[field];
  const mutationPath = visit.path(taskMutationPath(task));
  const mutable = canMutateTask(task);
  const editTitle = `Edit ${spec.label} for ${task.id}`;
  const wrap = el("div", { class: "field-editor-cell" });
  wrap.appendChild(
    buildInlineFieldEditor({
      label: spec.label,
      value: spec.toText(task),
      renderView: () => spec.renderView(task),
      multiline: spec.multiline !== false,
      placeholder: spec.placeholder || "",
      hint: spec.hint || "",
      toggle: spec.toggle || null,
      editable: mutable,
      editTitle,
      disabledTitle: aggregateRefusalTitle(editTitle, `edit ${spec.label}`),
      save: (text, options) => patchJson(mutationPath, spec.toPayload(text, options)),
      onSaved: (updatedTask) => {
        if (visit.isCurrent()) completeFieldSave(task.id, field, updatedTask, context);
      },
      onEditingChange: (open) => setDetailEditing(detail, field, open),
      editSlot,
    }),
  );
  const feedback = taskState.fieldFeedback.get(fieldFeedbackKey(task.id, field));
  const feedbackNode = buildMutationFeedback(feedback);
  if (feedbackNode) wrap.appendChild(feedbackNode);
  return wrap;
}

function completeFieldSave(taskId, field, updatedTask, context) {
  applyUpdatedTask(updatedTask, context);
  const key = fieldFeedbackKey(taskId, field);
  taskState.fieldFeedback.set(key, { kind: "success", text: `${TASK_FIELD_EDITORS[field].label} saved` });
  rerender(context);
  scheduleFeedbackExpiry(taskState.fieldFeedback, key, context, FIELD_SAVED_NOTICE_MS);
}

/* ORB-12235: complexity gates dispatch — an unassessed task is withheld from
   implementation — and until now only the CLI could set it. `unassessed` is
   deliberately not an option: the update endpoint rejects it, so offering it
   would only produce a 400. A task that currently carries it (or any value the
   server later adds) keeps a disabled placeholder so the select still shows
   what is stored. */
const TASK_COMPLEXITY_OPTIONS = ["low", "medium", "hard", "xhard"];

export function buildComplexityUpdateControl(task, context) {
  const cell = el("div", { class: "complexity-cell" });
  const mutable = canMutateTask(task);
  const feedback = taskState.complexityFeedback.get(task.id);
  const label = `Update complexity for ${task.id}`;
  const select = el("select", {
    class: "task-complexity-select mono",
    title: mutable ? label : aggregateRefusalTitle(label, "change complexity"),
  });
  select.setAttribute("aria-label", label);

  const currentValue = assessedComplexity(task);
  if (!currentValue) {
    const placeholder = el("option", { text: task.complexity ? String(task.complexity) : "unassessed" });
    placeholder.value = "";
    placeholder.disabled = true;
    select.appendChild(placeholder);
  }
  for (const value of TASK_COMPLEXITY_OPTIONS) {
    const option = el("option", { text: value });
    option.value = value;
    select.appendChild(option);
  }
  select.value = currentValue;
  if (!mutable || (feedback && feedback.kind === "pending")) select.disabled = true;

  select.addEventListener("change", (event) => {
    event.stopPropagation();
    applyTaskComplexityChange(task, select.value, context);
  });
  cell.appendChild(select);
  const feedbackNode = buildMutationFeedback(feedback, () => {
    if (feedback && feedback.undo) {
      applyTaskComplexityChange(task, feedback.undo.previousValue, context);
    }
  });
  if (feedbackNode) cell.appendChild(feedbackNode);
  return cell;
}

function assessedComplexity(task) {
  const value = task && task.complexity ? String(task.complexity) : "";
  return TASK_COMPLEXITY_OPTIONS.includes(value) ? value : "";
}

async function applyTaskComplexityChange(task, nextValue, context) {
  const visit = captureWorkspaceVisit();
  const mutationPath = visit.path(taskMutationPath(task));
  const previousValue = assessedComplexity(task);
  if (!nextValue || nextValue === previousValue || !canMutateTask(task)) return;
  taskState.complexityFeedback.set(task.id, { kind: "pending", text: "saving…" });
  rerender(context);
  try {
    const updatedTask = await patchJson(mutationPath, { complexity: nextValue });
    if (!visit.isCurrent()) return;
    applyUpdatedTask(updatedTask, context);
    taskState.complexityFeedback.set(task.id, {
      kind: "success",
      text: "complexity saved",
      // Undo is offered only back to an assessed value; the endpoint refuses
      // `unassessed`, so a task that arrived unassessed has nothing to restore.
      undo: previousValue
        ? { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS }
        : undefined,
    });
  } catch (error) {
    if (!visit.isCurrent()) return;
    taskState.complexityFeedback.set(task.id, {
      kind: "error",
      text: `complexity update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  rerender(context);
  scheduleFeedbackExpiry(taskState.complexityFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
}
