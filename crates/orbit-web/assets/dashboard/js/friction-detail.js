// Orbit dashboard friction detail: the Knowledge tab's detail pane (and its
// one-column accordion twin) laid out like a task's detail. The head carries
// the copyable id, the title, the editable status pill and the friction's own
// tags; the metadata card and the body use the task detail's classes; Resolve
// sits in the sticky action footer.

import { captureWorkspaceVisit, el, formatDateTime, makeCopyButton, patchJson, postJson, syncNodes } from './common.js';
import { buildInlineFieldEditor } from './field-editor.js';
import { TASK_FIELD_EDITORS, markdownView, taskMarkdownPanel } from './task-fields.js';
import { buildStatusSelectCell } from './task-status-controls.js';

// A friction's status set. `open` and `triaged` are the work in flight;
// `resolved` is what the footer's Resolve and the pill's last option both reach.
export const FRICTION_STATUSES = ["open", "triaged", "resolved"];

const TAGS_SPEC = TASK_FIELD_EDITORS.tags;

/* Paint `friction` into `detail`. `ctx`:
     knownTags      the tags the workspace already uses (shown as the editor's hint)
     openTask(id)   open a task from the During task link
     onChanged(id)  a write landed: reselect `id` and reload the list */
export function paintFrictionDetail(detail, friction, ctx) {
  // An open tag editor holds text the operator is still typing: a refresh of
  // the same friction leaves it alone, as the task detail does.
  if (detail.dataset.frictionId === friction.id && detail.querySelector(".field-editor-input")) return;
  detail.dataset.frictionId = friction.id;
  detail.classList.add("friction-pane");

  // The write handlers report a failure on the content card, under the head.
  const content = el("div", { class: "row-detail task-detail friction-content" });
  const write = (request, failure) => runFrictionWrite(content, friction, ctx, request, failure);

  const status = friction.status || "open";
  const targets = FRICTION_STATUSES.filter((value) => value !== status);
  const label = `Update status for ${friction.id}`;
  const statusCell = buildStatusSelectCell({
    status,
    label,
    title: label,
    targets,
    onSelect: (next, select) => write(
      (visit) => patchJson(frictionPath(visit, friction), { status: next }),
      () => { select.value = ""; },
    ),
  });

  const levers = el("div", { class: "task-levers" }, [
    statusCell,
    el("div", { class: "task-levers-tags" }, [buildFrictionTagsEditor(friction, ctx)]),
  ]);
  const title = el("h2", { class: "task-pane-title", text: friction.title || friction.id });
  const head = el("div", { class: "task-pane-head" }, [
    el("div", { class: "task-pane-bar" }, [
      makeCopyButton(friction.id, { class: "id mono", description: "Copy friction ID" }),
    ]),
    title,
    levers,
  ]);

  content.append(
    buildMetaCard(friction, ctx),
    taskMarkdownPanel(markdownView(friction.body || friction.title || "_No body._")),
    buildResolveFooter(friction, write),
  );
  syncNodes(detail, [head, content]);
}

function buildFrictionTagsEditor(friction, ctx) {
  const visit = captureWorkspaceVisit();
  const known = Array.isArray(ctx.knownTags) ? ctx.knownTags : [];
  const wrap = el("div", { class: "field-editor-cell" });
  wrap.dataset.field = "tags";
  wrap.appendChild(buildInlineFieldEditor({
    label: "tags",
    value: TAGS_SPEC.toText(friction),
    renderView: () => TAGS_SPEC.renderView(friction),
    multiline: false,
    placeholder: TAGS_SPEC.placeholder,
    hint: known.length ? `Known tags: ${known.join(", ")}` : "",
    editTitle: `Edit tags for ${friction.id}`,
    save: (text) => {
      const payload = TAGS_SPEC.toPayload(text);
      if (payload.tags.length === 0) return Promise.reject(new Error("A friction needs at least one tag."));
      return patchJson(frictionPath(visit, friction), payload);
    },
    onSaved: (updated) => {
      if (visit.isCurrent()) ctx.onChanged(updated?.id || friction.id);
    },
  }));
  return wrap;
}

// Model, when it was reported (and resolved) and the task it happened during:
// the task detail's property rows. The id, status and tags live in the head.
function buildMetaCard(friction, ctx) {
  const row = (kind, text, value) =>
    el("div", { class: `property-row fact task-delivery-row ${kind}` }, [el("span", { class: "label", text }), value]);
  const textValue = (text) => el("span", { class: "value", text });
  const rows = [];
  if (friction.model) rows.push(row("model", "Model", textValue(friction.model)));
  rows.push(row("reported", "Reported", textValue(formatDateTime(friction.created_at))));
  if (friction.resolved_at) rows.push(row("resolved", "Resolved", textValue(formatDateTime(friction.resolved_at))));
  if (friction.during_task) {
    const open = el("button", {
      class: "mono task-delivery-relation",
      type: "button",
      text: friction.during_task,
      "aria-description": `Open ${friction.during_task}`,
    });
    open.addEventListener("click", (event) => {
      event.stopPropagation();
      ctx.openTask(friction.during_task);
    });
    rows.push(row("during-task relation", "During task", el("span", { class: "value task-delivery-relations" }, [open])));
  }
  return el("div", { class: "card card-flush card-raised task-delivery-card" }, rows);
}

function buildResolveFooter(friction, write) {
  const resolve = el("button", {
    class: "action approve",
    text: "resolve",
    "aria-description": `Resolve ${friction.id}`,
  });
  resolve.type = "button";
  resolve.disabled = friction.status === "resolved";
  resolve.addEventListener("click", (event) => {
    event.stopPropagation();
    resolve.disabled = true;
    resolve.textContent = "";
    resolve.append(el("span", { class: "spinner" }), "wait");
    write((visit) => postJson(frictionPath(visit, friction, "/resolve")), () => {
      resolve.disabled = false;
      resolve.textContent = "resolve";
    });
  });
  const actions = el("div", { class: "actions task-action-row lifecycle-actions" }, [resolve]);
  return el("div", { class: "task-action-footer", role: "region", "aria-label": "Friction actions" }, [actions]);
}

function frictionPath(visit, friction, suffix = "") {
  return visit.path(`/api/frictions/${encodeURIComponent(friction.id)}${suffix}`);
}

// One write from the pill or the footer: clear the last failure, run it, then
// hand the saved record back so the list reloads, or show why it failed and
// `restore` the control. A reply that lands after a workspace switch is dropped.
async function runFrictionWrite(content, friction, ctx, request, restore) {
  const visit = captureWorkspaceVisit();
  for (const node of content.querySelectorAll(".action-error")) node.remove();
  try {
    const updated = await request(visit);
    if (!visit.isCurrent()) return;
    await ctx.onChanged(updated?.id || friction.id);
  } catch (error) {
    if (!visit.isCurrent()) return;
    content.prepend(el("div", { class: "action-error", text: error.message || "friction update failed" }));
    restore();
  }
}
