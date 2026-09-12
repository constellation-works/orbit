// Orbit dashboard task-domain rendering and actions.
// Pure vanilla JS, split into ES modules with no build step.

import { onWorkspaceChange, panelCanRender, el, statusPill, patchJson, postJson, syncNodes, isAggregateView, withWorkspace, makeToggleRow } from './common.js';
import { renderMarkdown, renderMarkdownInline } from './markdown.js';
import { buildInlineFieldEditor } from './field-editor.js';

const $ = (id) => document.getElementById(id);

let lastCrewPayload = { default_crew: null, crews: [] };
let expandedTaskIds = new Set();
let taskActionNotice = null;
let pinnedExternalTask = null;
// ORB-10874: per-task inline-edit feedback for the status/crew selects. Each
// entry is `{ kind: 'pending'|'success'|'error', text, undo }`, where `undo`
// (when present) is `{ previousValue, expiresAt }` — a bounded window in which
// the prior value can still be restored with one click. Cleared automatically
// once that window lapses (see scheduleFeedbackExpiry).
let statusFeedback = new Map();
let crewFeedback = new Map();
// ORB-12235: the same feedback shape for the writable fields in the expanded
// detail — the complexity select and the inline text editors. `fieldFeedback`
// is keyed `<task id>:<field>` because one detail can hold several editors.
let complexityFeedback = new Map();
let fieldFeedback = new Map();
const MUTATION_UNDO_WINDOW_MS = 8000;
// A saved text field has no undo (restoring prose would need a second write of
// its own), so its note only has to stay long enough to be read.
const FIELD_SAVED_NOTICE_MS = 4000;

onWorkspaceChange(() => {
  pinnedExternalTask = null;
  expandedTaskIds.clear();
  statusFeedback.clear();
  crewFeedback.clear();
  complexityFeedback.clear();
  fieldFeedback.clear();
});

// ORB-10444: task ids whose Ship dispatch this page has already issued. Ship is
// a write against a live pipeline, so a second click must not launch a second
// run: the id stays here for the life of the page once a dispatch succeeds (the
// server rejects a duplicate with 409 regardless), and is released only when the
// dispatch failed and retrying is the right move.
let shipInFlightTaskIds = new Set();

function taskList(context) {
  return context && typeof context.getTasks === "function" ? context.getTasks() : [];
}

function searchQueryValue(context) {
  return context && typeof context.getSearchQuery === "function"
    ? context.getSearchQuery()
    : "";
}

function activeStatusSet(context) {
  return context && typeof context.getActiveStatuses === "function"
    ? context.getActiveStatuses()
    : new Set();
}

function setSearchQuery(context, value) {
  if (context && typeof context.setSearchQuery === "function") context.setSearchQuery(value);
}

function setActiveStatuses(context, statuses) {
  if (context && typeof context.setActiveStatuses === "function") context.setActiveStatuses(statuses);
}

function statusOrder(context) {
  return context && Array.isArray(context.statusOrder) ? context.statusOrder : [];
}

function statusTransitions(task) {
  return Array.isArray(task && task.status_transitions)
    ? task.status_transitions.filter((transition) => transition && transition.status)
    : [];
}

function statusTransition(task, targetStatus) {
  return statusTransitions(task).find((transition) => transition.status === targetStatus) || null;
}

function fmtAbsTimeValue(context, value) {
  return context && typeof context.fmtAbsTime === "function"
    ? context.fmtAbsTime(value)
    : (value || "-");
}

function refreshTasks(context) {
  return context && typeof context.refreshDashboard === "function"
    ? context.refreshDashboard()
    : Promise.resolve();
}

function defaultActiveStatuses(context) {
  return context && Array.isArray(context.defaultActiveStatuses)
    ? context.defaultActiveStatuses
    : statusOrder(context);
}

function tasksMeta(context) {
  return context && typeof context.getTasksMeta === "function" ? context.getTasksMeta() : null;
}

// Explicit page range and matching total instead of the ambiguous `N/50`
// shorthand, which could mean either a total or an unreachable hard cap.
export function formatTaskCount(filteredCount, fetchedCount, meta) {
  if (meta && Number.isFinite(meta.total)) {
    const offset = Number.isFinite(meta.offset) ? meta.offset : 0;
    if (fetchedCount === 0) return `0 of ${meta.total}`;
    const range = `${offset + 1}–${offset + fetchedCount} of ${meta.total}`;
    return filteredCount === fetchedCount ? range : `${filteredCount} shown · page ${range}`;
  }
  return filteredCount === fetchedCount
    ? `${fetchedCount} shown`
    : `${filteredCount} shown of ${fetchedCount} fetched`;
}

// ORB-10874: aggregate ("All workspaces") mode has no ambient workspace to
// scope a mutation to. A task fetched through /api/tasks/all carries its own
// `workspace_id`, so that's an explicit, workspace-qualified target; anything
// else is refused rather than silently applied to the wrong (or no) workspace.
function canMutateTask(task) {
  return !isAggregateView() || Boolean(task && task.workspace_id);
}

// One sentence for every inline control that cannot write in the aggregate
// view, so the refusal reads the same whichever control the operator hovers.
function aggregateRefusalTitle(label, action) {
  return `${label} — select a specific workspace to ${action} in aggregate view`;
}

function taskMutationPath(task, suffix = "") {
  const base = `/api/tasks/${encodeURIComponent(task.id)}${suffix}`;
  if (isAggregateView() && task.workspace_id) {
    const sep = base.includes("?") ? "&" : "?";
    return `${base}${sep}workspace=${encodeURIComponent(task.workspace_id)}`;
  }
  return base;
}

function scheduleFeedbackExpiry(map, key, context, delay) {
  setTimeout(() => {
    const entry = map.get(key);
    if (entry && entry.kind !== "pending") {
      map.delete(key);
      renderTasks(taskList(context), context);
    }
  }, delay);
}

// Renders the inline pending/success/error text next to a status or crew
// select, plus a bounded "undo" button while `entry.undo` is still live. The
// wrapper is a live region so screen-reader users get the same feedback a
// sighted user reads from the text/color change.
function buildMutationFeedback(entry, onUndo = () => {}) {
  if (!entry) return null;
  const wrap = el("span", { class: `mutation-feedback ${entry.kind}`, text: entry.text });
  wrap.setAttribute("role", "status");
  wrap.setAttribute("aria-live", entry.kind === "error" ? "assertive" : "polite");
  if (entry.undo) {
    const undoBtn = el("button", { class: "mutation-undo", text: "undo" });
    undoBtn.type = "button";
    undoBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      onUndo();
    });
    wrap.appendChild(undoBtn);
  }
  return wrap;
}

export function normalizeCrewPayload(payload) {
  const crews = Array.isArray(payload && payload.crews)
    ? payload.crews
      .filter((crew) => crew && crew.name)
      .map((crew) => ({
        name: String(crew.name),
        model: crew.model == null ? "" : String(crew.model),
        is_default: Boolean(crew.is_default),
      }))
    : [];
  crews.sort((a, b) => a.name.localeCompare(b.name));
  return {
    default_crew: payload && payload.default_crew ? String(payload.default_crew) : null,
    crews,
  };
}

export function cacheCrewPayload(payload) {
  lastCrewPayload = normalizeCrewPayload(payload);
  return lastCrewPayload;
}

export function hasCrewOptions() {
  return Array.isArray(lastCrewPayload.crews) && lastCrewPayload.crews.length > 0;
}

function crewOptionsSignature() {
  return JSON.stringify(lastCrewPayload);
}

// A stable string for a task's current mutation-feedback entry (or "" when
// none), so a syncNodes row diff picks up pending/success/error/undo
// transitions the same way it picks up any other task field change.
function feedbackSignature(map, taskId) {
  const entry = map.get(taskId);
  if (!entry) return "";
  return `${entry.kind}:${entry.text}:${entry.undo ? entry.undo.expiresAt : ""}`;
}

// The detail's own controls — the complexity select and each field editor —
// keep their feedback outside the task object, so the detail's diff hash has to
// see it the way the row's hash sees the status/crew entries. Without this a
// saved or failed note would never paint.
function detailFeedbackSignature(taskId) {
  const parts = [feedbackSignature(complexityFeedback, taskId)];
  for (const field of Object.keys(TASK_FIELD_EDITORS)) {
    parts.push(feedbackSignature(fieldFeedback, fieldFeedbackKey(taskId, field)));
  }
  return parts.join("|");
}

function explicitCrewValue(task) {
  return task && task.crew ? String(task.crew) : "";
}

function isConfiguredCrewValue(value) {
  const name = value ? String(value) : "";
  return Boolean(name) && lastCrewPayload.crews.some((crew) => crew.name === name);
}

function hasStaleExplicitCrew(task) {
  const currentValue = explicitCrewValue(task);
  return Boolean(currentValue) && !isConfiguredCrewValue(currentValue);
}

function resolvedCrewName(task) {
  if (lastCrewPayload.default_crew) return lastCrewPayload.default_crew;
  if (!explicitCrewValue(task) && task && task.resolved_crew) {
    return String(task.resolved_crew);
  }
  return "workspace";
}

function defaultCrewOptionText(task) {
  return `default: ${resolvedCrewName(task)}`;
}

function crewOptionTitle(crew) {
  return `model=${crew.model || "-"}`;
}

function applyUpdatedTask(updatedTask, context) {
  if (!updatedTask || !updatedTask.id) return;
  if (context && typeof context.replaceTask === "function") {
    context.replaceTask(updatedTask);
  }
  if (pinnedExternalTask && pinnedExternalTask.task && pinnedExternalTask.task.id === updatedTask.id) {
    pinnedExternalTask.task = updatedTask;
  }
}

function stopRowInteraction(node) {
  for (const eventName of ["pointerdown", "mousedown", "click", "keydown"]) {
    node.addEventListener(eventName, (event) => event.stopPropagation());
  }
}

function filterTasks(tasks, context) {
  const q = searchQueryValue(context);
  const activeStatuses = activeStatusSet(context);
  return tasks.filter((t) => {
    if (!activeStatuses.has(t.status)) return false;
    if (!q) return true;
    return (
      (t.id && t.id.toLowerCase().includes(q)) ||
      (t.title && t.title.toLowerCase().includes(q))
    );
  });
}

const TASK_META_FIELDS = [
  ["orchestrator", "orchestrator"],
  ["implemented_by", "implemented_by"],
  ["planned_by", "planned_by"],
  ["created_by", "created_by"],
  ["pr_number", "pr"],
  ["pr_status", "pr_status"],
  ["job_run_id", "job_run"],
  ["created_at", "created"],
  ["updated_at", "updated"],
];

const RELATION_GROUPS = [
  ["blocked_by", "BlockedBy"],
  ["child_of", "ChildOf"],
  ["spawned_from", "SpawnedFrom"],
  ["regression_from", "RegressionFrom"],
  ["supersedes", "Supersedes"],
  ["related_to", "RelatedTo"],
];
const RELATION_GROUP_LABELS = new Map(RELATION_GROUPS);

function relationTypeKey(value) {
  if (!value) return "";
  return String(value)
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/-/g, "_")
    .toLowerCase();
}

export function copyTaskIdWithNotice(taskId, context) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(taskId).catch(() => {});
  }
  taskActionNotice = `${taskId} is not in the filtered task list; copied ID`;
  renderTasks(taskList(context), context);
}

function findTaskRow(taskId) {
  return Array.from(document.querySelectorAll("#tasks-body .row"))
    .find((row) => row.dataset.key === `task-${taskId}`) || null;
}

export function openVisibleTask(taskId, context) {
  const visible = filterTasks(taskList(context), context).some((task) => task.id === taskId);
  if (!visible) {
    copyTaskIdWithNotice(taskId, context);
    return;
  }
  expandedTaskIds.add(taskId);
  renderTasks(taskList(context), context);
  requestAnimationFrame(() => {
    const row = findTaskRow(taskId);
    if (!row) return;
    row.scrollIntoView({ behavior: "smooth", block: "center" });
    row.classList.add("data-changed");
    setTimeout(() => row.classList.remove("data-changed"), 1200);
  });
}

/* Global ID resolver support (ORB-00211): allow rendering detail for a task that
   is outside the filtered list (hidden status, search, or truncated fetch)
   without mutating DASHBOARD_TASK_STATUSES. The pinned detail appears in a
   highlighted block at top of #tasks-body; auto-clears once the filtered list
   actually contains the task. */
export function setPinnedExternalTask(task, context) {
  if (!task || !task.id) return;
  pinnedExternalTask = { task, id: task.id };
}

export function clearPinnedExternalTask(context) {
  pinnedExternalTask = null;
  if (context) renderTasks(taskList(context), context);
}

function refreshChips(context) {
  for (const chip of document.querySelectorAll("#task-filter .chip")) {
    const status = chip.dataset.status;
    const isAll = chip.dataset.role === "all";
    const activeStatuses = activeStatusSet(context);
    const allOn = activeStatuses.size === statusOrder(context).length;
    const on = isAll ? allOn : activeStatuses.has(status);
    chip.classList.toggle("active", on);
    // ORB-10874: chip state must not rely on the active/inactive color
    // difference alone — aria-pressed exposes it to assistive tech too.
    chip.setAttribute("aria-pressed", on ? "true" : "false");
  }
}

// ORB-10874: the status filter and search query are represented in the tasks
// hash (`#tasks?status=a,b&q=...`), mirroring the audit tab's buildAuditHash/
// applyAuditHashQuery pair, so a reload or the browser's back/forward button
// restores the same filtered view instead of resetting to the default.
export function buildTasksHash(context) {
  const sp = new URLSearchParams();
  const order = statusOrder(context);
  const activeStatuses = activeStatusSet(context);
  const selected = order.filter((s) => activeStatuses.has(s));
  if (selected.length === order.length) {
    // An explicit all-status selection must not collapse to the omitted
    // status query, which means the default set and excludes someday.
    sp.set("status", "all");
  } else {
    sp.set("status", selected.length > 0 ? selected.join(",") : "none");
  }
  const q = searchQueryValue(context);
  if (q) sp.set("q", q);
  const qs = sp.toString();
  return qs ? `#tasks?${qs}` : "#tasks";
}

export function applyTasksHashQuery(query, context) {
  if (context && typeof context.resetTaskPagination === "function") {
    context.resetTaskPagination();
  }
  const order = statusOrder(context);
  const statusParam = query.get("status");
  if (statusParam == null) {
    setActiveStatuses(context, new Set(defaultActiveStatuses(context)));
  } else if (statusParam === "none") {
    setActiveStatuses(context, new Set());
  } else if (statusParam === "all") {
    setActiveStatuses(context, new Set(order));
  } else {
    const wanted = new Set(statusParam.split(",").map((s) => s.trim()).filter(Boolean));
    setActiveStatuses(context, new Set(order.filter((s) => wanted.has(s))));
  }
  setSearchQuery(context, (query.get("q") || "").trim().toLowerCase());
}

// Re-syncs the search box and chip states from context after the hash (not a
// direct user interaction with these controls) changed them — e.g. on load,
// or after a back/forward navigation.
export function syncTaskControls(context) {
  const search = $("task-search");
  if (search) {
    const q = searchQueryValue(context);
    if (search.value !== q) search.value = q;
  }
  refreshChips(context);
}

function navigateTasksHash(context) {
  if (context && typeof context.resetTaskPagination === "function") {
    context.resetTaskPagination();
  }
  const hash = buildTasksHash(context);
  if (window.location.hash !== hash) {
    window.location.hash = hash;
  } else {
    refreshChips(context);
    refreshTasks(context).catch((error) => console.error("Failed to reset task pages", error));
  }
}

export function renderTaskPagination(context) {
  const previous = $("tasks-previous");
  const next = $("tasks-next");
  const status = $("tasks-page-status");
  if (!previous || !next || !status) return;
  const state = context && typeof context.getTaskPagination === "function"
    ? context.getTaskPagination()
    : {};
  previous.disabled = Boolean(state.loading) || !state.canPrevious;
  next.disabled = Boolean(state.loading) || !state.canNext;
  status.textContent = state.error
    ? `Page failed: ${state.error}`
    : state.loading ? "Loading task page…" : "";
  status.className = state.error ? "error" : "";
  if (previous.dataset.wired !== "true") {
    previous.dataset.wired = "true";
    previous.addEventListener("click", () => context.navigateTaskPage("previous"));
    next.addEventListener("click", () => context.navigateTaskPage("next"));
  }
}

export function buildChips(context) {
  const container = $("task-filter");
  container.innerHTML = "";
  const allChip = el("button", { class: "chip", text: "all" });
  allChip.type = "button";
  allChip.dataset.role = "all";
  allChip.addEventListener("click", () => {
    setActiveStatuses(context, new Set(statusOrder(context)));
    navigateTasksHash(context);
  });
  container.appendChild(allChip);
  for (const status of statusOrder(context)) {
    const chip = el("button", { class: "chip", text: status });
    chip.type = "button";
    chip.dataset.status = status;
    chip.style.borderLeft = `2px solid var(--status-${status}, var(--border))`;
    chip.addEventListener("click", () => {
      const activeStatuses = activeStatusSet(context);
      if (activeStatuses.has(status)) {
        activeStatuses.delete(status);
      } else {
        activeStatuses.add(status);
      }
      navigateTasksHash(context);
    });
    container.appendChild(chip);
  }
  refreshChips(context);
}

export function wireSearch(context) {
  const input = $("task-search");
  let debounce = null;
  input.addEventListener("input", (e) => {
    setSearchQuery(context, e.target.value.trim().toLowerCase());
    if (debounce) clearTimeout(debounce);
    debounce = setTimeout(() => navigateTasksHash(context), 250);
  });
}

// ORB-10874: a compact, always-visible restatement of the filters currently
// narrowing the list — the chips/search box already encode this, but not in a
// form a screen reader announces or a glance confirms without reading each
// control. Rendered next to the count so "why am I seeing this many" is
// answered in the same place as "how many is this".
function renderFilterSummary(context) {
  const node = $("task-filter-summary");
  if (!node) return;
  const order = statusOrder(context);
  const activeStatuses = activeStatusSet(context);
  const parts = [];
  if (activeStatuses.size === 0) {
    parts.push("status: none selected");
  } else if (activeStatuses.size < order.length) {
    parts.push(`status: ${order.filter((s) => activeStatuses.has(s)).join(", ")}`);
  }
  const q = searchQueryValue(context);
  if (q) parts.push(`search: "${q}"`);
  node.textContent = parts.length > 0 ? `Filtering by ${parts.join(" · ")}` : "Showing all statuses";
}

function buildTagRow(tags) {
  const wrap = el("div", { class: "detail-tag-row" });
  for (const tag of tags) {
    wrap.appendChild(el("span", { class: "chip", text: tag }));
  }
  return wrap;
}

function buildExternalRefs(refs) {
  const wrap = el("div");
  for (const ref of refs) {
    const label = `${ref.system || "external"}:${ref.id || ""}`;
    const line = el("div", { class: "external-ref-line" });
    if (ref.url) {
      const link = el("a", { text: label });
      link.href = ref.url;
      line.appendChild(link);
    } else {
      line.textContent = label;
    }
    wrap.appendChild(line);
  }
  return wrap;
}

function buildRelations(relations, context) {
  const byType = new Map(RELATION_GROUPS.map(([key]) => [key, []]));
  for (const relation of relations) {
    const key = relationTypeKey(relation.relation_type || relation.type);
    const target = relation.target == null ? "" : String(relation.target);
    if (!RELATION_GROUP_LABELS.has(key) || !target) continue;
    byType.get(key).push(target);
  }

  const wrap = el("div");
  for (const [key, label] of RELATION_GROUPS) {
    const targets = byType.get(key);
    if (!targets || targets.length === 0) continue;
    const group = el("div", { class: "relation-group" }, [
      el("span", { class: "label", text: label }),
    ]);
    for (const target of targets) {
      const btn = el("button", { class: "relation-target mono", text: target });
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        openVisibleTask(target, context);
      });
      group.appendChild(btn);
    }
    wrap.appendChild(group);
  }
  return wrap;
}

// ORB-11333: the settled before-PR review gate. Every value comes from the
// certificate the gate wrote; nothing here is inferred from status or tags.
function buildReviewGate(review) {
  const wrap = el("div", { class: "review-gate" });
  if (review.unreadable) {
    wrap.appendChild(el("div", { text: `review evidence unreadable: ${review.unreadable}` }));
    return wrap;
  }
  const reviewer = review.reviewer || {};
  const consumed = review.consumed || {};
  const budget = review.budget || {};
  const lines = [
    `verdict: ${review.verdict}${review.assurance ? ` (${review.assurance})` : ""}`,
    `reviewer: ${reviewer.crew ?? "—"} · ${reviewer.provider ?? "—"} / ${reviewer.model ?? "—"}${reviewer.same_model_as_implementer ? " · same model as implementer" : ""}`,
    `base ${review.base?.commit ?? "—"} → reviewed ${review.reviewed_candidate?.commit ?? "—"} → final ${review.final_candidate?.commit ?? "—"}`,
    `repairs: ${Array.isArray(review.repair_commits) && review.repair_commits.length ? review.repair_commits.map((c) => `${c.commit.slice(0, 12)} by ${c.author}`).join(", ") : "none"}`,
    `findings: ${Array.isArray(review.findings) ? review.findings.length : 0} · validation: ${Array.isArray(review.validation) ? review.validation.length : 0} record(s), complete: ${review.validation_complete ? "yes" : "no"}`,
    `consumed: ${consumed.reviewer_starts ?? 0}/${budget.reviewer_starts ?? "?"} starts · ${consumed.repair_cycles ?? 0}/${budget.repair_cycles ?? "?"} repair cycles · ${consumed.seconds ?? 0}s of ${budget.minutes ?? "?"} min`,
  ];
  if (review.escalation) lines.push(`escalation: ${review.escalation}`);
  if (Array.isArray(review.landings) && review.landings.length) {
    for (const landing of review.landings) {
      lines.push(`landing ${landing.landed?.commit ?? "?"}: ${landing.transformation} · ${landing.covered ? "covered" : `uncovered (${landing.reason ?? "unknown"})`}`);
    }
  }
  if (Array.isArray(review.stale_reasons) && review.stale_reasons.length) {
    lines.push(`stale gate reasons: ${review.stale_reasons.join(", ")}`);
  }
  for (const line of lines) wrap.appendChild(el("div", { text: line }));
  return wrap;
}

function fmtSize(bytes) {
  const value = Number(bytes);
  if (!Number.isFinite(value) || value < 0) return "0 bytes";
  if (value < 1024) return `${value} bytes`;
  const kb = value / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  const mb = kb / 1024;
  return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
}

function artifactUrl(taskId, path) {
  const encodedPath = String(path)
    .split("/")
    .map((part) => encodeURIComponent(part))
    .join("/");
  return `/api/tasks/${encodeURIComponent(taskId)}/artifacts/${encodedPath}`;
}

function artifactMediaType(artifact, response) {
  return String(
    response.headers.get("content-type") || artifact.media_type || "application/octet-stream",
  ).split(";")[0].trim().toLowerCase();
}

function renderArtifactText(mediaType, text) {
  const rendered = mediaType === "text/markdown" ? renderMarkdown(text) : null;
  if (rendered !== null) {
    const view = el("div", { class: "markdown-body" });
    view.innerHTML = rendered;
    return view;
  }
  return el("pre", { text });
}

// The raster formats the server will serve inline. This mirrors the artifact
// policy in orbit-types (`inline_safe_artifact_media_type`), which the artifact
// route enforces with `nosniff`: SVG and HTML are viewable formats that also
// host script, so they are deliberately absent and fall through to download.
const INLINE_IMAGE_MEDIA_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

function artifactFileName(path) {
  return String(path).split("/").pop() || String(path);
}

function buildArtifactDownloadLink(artifact, objectUrl, text) {
  const link = el("a", { text: text || `Download ${artifact.path}` });
  link.href = objectUrl;
  link.download = artifactFileName(artifact.path);
  return link;
}

function buildArtifactImage(artifact, blob) {
  const objectUrl = URL.createObjectURL(blob);
  const figure = el("div", { class: "artifact-image-view" });
  const image = el("img", { class: "artifact-image" });
  image.src = objectUrl;
  // The path is the only description the artifact carries, so it is a more
  // useful alt text than a generic label for anyone reading without the image.
  image.alt = String(artifact.path);
  image.loading = "lazy";
  // A stored artifact can be truncated or mislabeled. When the browser cannot
  // decode it, say so and still offer the bytes rather than leaving a broken
  // image icon behind.
  image.addEventListener("error", () => {
    figure.replaceChildren(
      el("div", {
        class: "artifact-error",
        text: `Unable to display ${artifact.path}: the image could not be decoded.`,
      }),
      buildArtifactDownloadLink(artifact, objectUrl),
    );
  });
  const open = el("a", { class: "artifact-open", text: "Open" });
  open.href = objectUrl;
  open.target = "_blank";
  open.rel = "noopener";
  const actions = el("div", { class: "artifact-actions" });
  actions.appendChild(open);
  actions.appendChild(buildArtifactDownloadLink(artifact, objectUrl, "Download"));
  figure.appendChild(image);
  figure.appendChild(actions);
  return figure;
}

function buildArtifactPreview(artifact, response) {
  const mediaType = artifactMediaType(artifact, response);
  if (INLINE_IMAGE_MEDIA_TYPES.has(mediaType)) {
    return response.blob().then((blob) => buildArtifactImage(artifact, blob));
  }
  if (
    mediaType === "text/markdown" ||
    mediaType === "application/json" ||
    mediaType.endsWith("/yaml") ||
    mediaType.endsWith("+yaml") ||
    mediaType.startsWith("text/")
  ) {
    return response.text().then((text) => renderArtifactText(mediaType, text));
  }
  return response.blob().then((blob) =>
    buildArtifactDownloadLink(artifact, URL.createObjectURL(blob)),
  );
}

export function buildArtifacts(task) {
  const wrap = el("div", { class: "artifacts" });
  for (const [index, artifact] of task.artifacts.entries()) {
    const path = String(artifact.path || "");
    const mediaType = String(artifact.media_type || "application/octet-stream");
    const row = el("div", {
      class: "artifact-row",
      text: `${path} · ${mediaType} · ${fmtSize(artifact.size_bytes)}`,
    });
    const preview = el("div", { class: "artifact-preview" });
    // Artifact paths are free-form, so the position in the list is what makes a
    // stable id the row's `aria-controls` can point at.
    preview.id = `artifact-preview-${task.id}-${index}`;
    preview.hidden = true;

    // Disclosure lives on the preview's `hidden` flag; keeping the two in one
    // setter is what stops the announced state from drifting from the visible one.
    const revealPreview = (visible) => {
      preview.hidden = !visible;
      row.setAttribute("aria-expanded", String(visible));
    };

    makeToggleRow(row, {
      expanded: false,
      controls: preview.id,
      onToggle: async (e) => {
        e.stopPropagation();
        if (preview.dataset.loaded === "true") {
          revealPreview(preview.hidden);
          return;
        }
        revealPreview(true);
        preview.textContent = "loading...";
        try {
          const response = await fetch(artifactUrl(task.id, path));
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          preview.replaceChildren(await buildArtifactPreview(artifact, response));
          preview.dataset.loaded = "true";
        } catch (error) {
          preview.textContent = `Unable to load ${path}: ${error.message}`;
        }
      },
    });
    wrap.appendChild(row);
    wrap.appendChild(preview);
  }
  return wrap;
}

/* ORB-12235: the task fields the expanded detail can edit in place. Each entry
   reads the task's current value as editor text, renders the read-only view,
   and turns the edited text back into a PATCH body carrying that one field —
   never a whole task, which would clobber whatever an agent wrote concurrently.
   `complexity` is not here: it is a fixed set of choices, so it gets a select
   (buildComplexityUpdateControl) rather than a text editor. */
const TASK_FIELD_EDITORS = {
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
    renderView: (task) =>
      Array.isArray(task.context_files) && task.context_files.length > 0
        ? buildFileList(task.context_files)
        : emptyFieldView("no context files"),
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

function markdownView(text) {
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

function buildCriteriaList(criteria) {
  const ul = el("ul", { class: "ac-list" });
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

function buildFileList(paths) {
  const ul = el("ul", { class: "file-list" });
  for (const path of paths) {
    ul.appendChild(el("li", { text: path }));
  }
  return ul;
}

function linesToText(values) {
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

function fieldFeedbackKey(taskId, field) {
  return `${taskId}:${field}`;
}

function taskFieldHasValue(task, spec) {
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

function buildTaskFieldEditor(task, field, detail, context) {
  const spec = TASK_FIELD_EDITORS[field];
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
      save: (text, options) => patchJson(taskMutationPath(task), spec.toPayload(text, options)),
      onSaved: (updatedTask) => completeFieldSave(task.id, field, updatedTask, context),
      onEditingChange: (open) => setDetailEditing(detail, field, open),
    }),
  );
  const feedback = fieldFeedback.get(fieldFeedbackKey(task.id, field));
  const feedbackNode = buildMutationFeedback(feedback);
  if (feedbackNode) wrap.appendChild(feedbackNode);
  return wrap;
}

function completeFieldSave(taskId, field, updatedTask, context) {
  applyUpdatedTask(updatedTask, context);
  const key = fieldFeedbackKey(taskId, field);
  fieldFeedback.set(key, { kind: "success", text: `${TASK_FIELD_EDITORS[field].label} saved` });
  renderTasks(taskList(context), context);
  scheduleFeedbackExpiry(fieldFeedback, key, context, FIELD_SAVED_NOTICE_MS);
}

/* ORB-12235: complexity gates dispatch — an unassessed task is withheld from
   implementation — and until now only the CLI could set it. `unassessed` is
   deliberately not an option: the update endpoint rejects it, so offering it
   would only produce a 400. A task that currently carries it (or any value the
   server later adds) keeps a disabled placeholder so the select still shows
   what is stored. */
const TASK_COMPLEXITY_OPTIONS = ["low", "medium", "hard"];

function buildComplexityUpdateControl(task, context) {
  const cell = el("div", { class: "complexity-cell" });
  const mutable = canMutateTask(task);
  const feedback = complexityFeedback.get(task.id);
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
  const previousValue = assessedComplexity(task);
  if (!nextValue || nextValue === previousValue || !canMutateTask(task)) return;
  complexityFeedback.set(task.id, { kind: "pending", text: "saving…" });
  renderTasks(taskList(context), context);
  try {
    const updatedTask = await patchJson(taskMutationPath(task), { complexity: nextValue });
    applyUpdatedTask(updatedTask, context);
    complexityFeedback.set(task.id, {
      kind: "success",
      text: "complexity saved",
      // Undo is offered only back to an assessed value; the endpoint refuses
      // `unassessed`, so a task that arrived unassessed has nothing to restore.
      undo: previousValue
        ? { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS }
        : undefined,
    });
  } catch (error) {
    complexityFeedback.set(task.id, {
      kind: "error",
      text: `complexity update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  renderTasks(taskList(context), context);
  scheduleFeedbackExpiry(complexityFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
}

function buildTaskDetail(task, context) {
  const detail = el("div", { class: "row-detail split-layout" });
  detail.addEventListener("click", (e) => e.stopPropagation());

  const leftCol = el("div", { class: "detail-main" });
  const rightCol = el("div", { class: "detail-side" });

  const addField = (parent, title, child, collapsible = false, collapsed = false) => {
    let classes = "field-block";
    if (collapsible) classes += " collapsible";
    if (collapsed) classes += " collapsed";
    const block = el("div", { class: classes });
    const h4 = el("h4", { text: title });
    if (collapsible) {
      makeToggleRow(h4, {
        expanded: !collapsed,
        onToggle: (e) => {
          e.stopPropagation();
          const nowCollapsed = block.classList.toggle("collapsed");
          h4.setAttribute("aria-expanded", String(!nowCollapsed));
        },
      });
    }
    block.appendChild(h4);
    block.appendChild(child);
    parent.appendChild(block);
  };

  // An editable field is shown even when it is empty — adding a missing
  // description is exactly the edit an operator comes here for — but only where
  // editing is actually allowed, so the read-only aggregate view stays as terse
  // as it was.
  const editor = (field) => buildTaskFieldEditor(task, field, detail, context);
  const showsEditor = (field) =>
    canMutateTask(task) || taskFieldHasValue(task, TASK_FIELD_EDITORS[field]);

  if (showsEditor("description")) {
    addField(leftCol, "description", editor("description"));
  }

  if (showsEditor("acceptance_criteria")) {
    addField(leftCol, "acceptance criteria", editor("acceptance_criteria"), true, true);
  }

  if (task.plan && task.plan.trim()) {
    addField(leftCol, "plan", markdownView(task.plan), true, true);
  }

  if (task.execution_summary && task.execution_summary.trim()) {
    addField(leftCol, "execution summary", markdownView(task.execution_summary), true, true);
  }

  if (Array.isArray(task.artifacts) && task.artifacts.length > 0) {
    addField(leftCol, "artifacts", buildArtifacts(task), true, true);
  }

  if (task.review && typeof task.review === "object") {
    addField(leftCol, "review gate", buildReviewGate(task.review), true, true);
  }

  addField(rightCol, "complexity", buildComplexityUpdateControl(task, context));

  if (showsEditor("tags")) {
    addField(rightCol, "tags", editor("tags"));
  }

  // Provenance and timestamps: recorded by the pipeline, so read-only here even
  // though the fields above are not.
  const meta = el("div", { class: "meta-list" });
  let metaCount = 0;
  for (const [key, label] of TASK_META_FIELDS) {
    const v = task[key];
    if (v == null || v === "") continue;
    const display = key.endsWith("_at") ? fmtAbsTimeValue(context, v) : String(v);
    const value = el("span", { class: "value" });
    if (key === "job_run_id") {
      const link = el("a", { text: display });
      link.href = `#runs?run_id=${encodeURIComponent(display)}`;
      value.appendChild(link);
    } else {
      value.textContent = display;
    }
    const span = el("div", { class: "meta-item" }, [
      el("span", { class: "label", text: `${label}` }),
      value,
    ]);
    meta.appendChild(span);
    metaCount++;
  }
  if (metaCount > 0) addField(rightCol, "details", meta);

  // ORB-00037: in the aggregate ("All workspaces") view each task carries its
  // owning workspace's filesystem location (home-abbreviated to ~ server-side);
  // show it in full here since the row only has room for the short name badge.
  if (task.workspace_root) {
    const loc = el("span", { class: "ws-location mono", text: task.workspace_root, title: task.workspace_root });
    addField(rightCol, "location", loc);
  }

  if (Array.isArray(task.external_refs) && task.external_refs.length > 0) {
    addField(rightCol, "external refs", buildExternalRefs(task.external_refs));
  }

  if (Array.isArray(task.relations) && task.relations.length > 0) {
    const relations = buildRelations(task.relations, context);
    if (relations.children.length > 0) addField(rightCol, "relations", relations);
  }

  if (showsEditor("context_files")) {
    addField(rightCol, "context files", editor("context_files"));
  }

  if (Array.isArray(task.history) && task.history.length > 0) {
    // ORB-10311: drop legacy bare `commented` stubs *before* the recent-history
    // limit so meaningful status/workflow events are not displaced by comment
    // noise (comments render in their own panel below).
    const meaningful = task.history.filter((h) => h && h.event !== "commented");
    if (meaningful.length > 0) {
      const wrap = el("div");
      const recent = meaningful.slice(-5).reverse();
      for (const h of recent) {
        const note = h.note ? ` (${h.note})` : "";
        const line = el("div", { class: "history-line" }, [
          document.createTextNode(`[${fmtAbsTimeValue(context, h.at)}] `),
          el("span", { class: "actor", text: h.by || "?" }),
          document.createTextNode(`: ${h.event}${note}`),
        ]);
        wrap.appendChild(line);
      }
      addField(rightCol, "recent history", wrap);
    }
  }

  if (Array.isArray(task.comments) && task.comments.length > 0) {
    const wrap = el("div");
    for (const c of task.comments) {
      const line = el("div", { class: "comment-line" }, [
        document.createTextNode(`[${fmtAbsTimeValue(context, c.at)}] `),
        el("span", { class: "author", text: c.by || "?" }),
        document.createTextNode(`: ${c.message || ""}`),
      ]);
      wrap.appendChild(line);
    }
    addField(rightCol, "comments", wrap);
  }

  detail.appendChild(leftCol);
  detail.appendChild(rightCol);
  detail.appendChild(buildActionsRow(task, detail, context));

  return detail;
}

const APPROVE_STATUSES = new Set(["proposed", "review"]);
const REJECT_STATUSES = new Set(["proposed", "review", "backlog"]);
// Ship dispatches a task through the pipeline, which admits it out of backlog —
// so backlog is the only status where the control means anything.
const SHIP_STATUSES = new Set(["backlog"]);

function buildActionsRow(task, detail, context) {
  const actions = el("div", { class: "actions" });
  if (SHIP_STATUSES.has(task.status)) {
    const shipped = shipInFlightTaskIds.has(task.id);
    const btn = el("button", {
      class: "action ship",
      text: shipped ? "shipping" : "ship",
      title: shipped
        ? "A ship run is already in flight for this task"
        : "Dispatch this task through the pipeline with its own crew",
    });
    btn.disabled = shipped;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      shipTask(task, detail, btn, context);
    });
    actions.appendChild(btn);
  }
  {
    const btn = el("button", { class: "action comment", text: "comment" });
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      showCommentForm(task, detail, actions, context);
    });
    actions.appendChild(btn);
  }
  if (APPROVE_STATUSES.has(task.status)) {
    const btn = el("button", { class: "action approve", text: "approve" });
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      runAction(task, "approve", detail, null, btn, context);
    });
    actions.appendChild(btn);
  }
  if (REJECT_STATUSES.has(task.status)) {
    const btn = el("button", { class: "action reject", text: "reject" });
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      showRejectForm(task, detail, actions, context);
    });
    actions.appendChild(btn);
  }
  if (statusTransition(task, "archived")) {
    const btn = el("button", { class: "action archive", text: "archive" });
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (window.confirm(`Archive task ${task.id}?`)) {
        runAction(task, "archive", detail, null, btn, context);
      }
    });
    actions.appendChild(btn);
  }
  return actions;
}

function buildStatusUpdateControl(task, context) {
  const cell = el("span", { class: "status-cell" });
  const targets = statusTransitions(task).map((transition) => transition.status);
  const color = `var(--status-${task.status}, var(--fg))`;
  const mutable = canMutateTask(task);
  const feedback = statusFeedback.get(task.id);
  const label = `Update status for ${task.id}`;
  const select = el("select", {
    class: "task-status-select mono",
    title: mutable ? label : aggregateRefusalTitle(label, "change status"),
    style: {
      color,
      borderLeftColor: color,
    },
  });
  select.setAttribute("aria-label", label);
  const placeholder = el("option", { text: task.status || "status" });
  placeholder.value = "";
  placeholder.disabled = true;
  placeholder.selected = true;
  placeholder.hidden = true;
  select.appendChild(placeholder);

  for (const status of targets) {
    const option = el("option", { text: status });
    option.value = status;
    select.appendChild(option);
  }

  if (targets.length === 0 || !mutable || (feedback && feedback.kind === "pending")) {
    select.disabled = true;
  }

  stopRowInteraction(cell);
  stopRowInteraction(select);
  select.addEventListener("change", (event) => {
    event.stopPropagation();
    const targetStatus = select.value;
    if (!targetStatus) return;
    applyTaskStatusChange(task, targetStatus, context);
  });
  cell.appendChild(select);
  const feedbackNode = buildMutationFeedback(feedback, () => {
    if (feedback && feedback.undo) applyTaskStatusChange(task, feedback.undo.previousValue, context);
  });
  if (feedbackNode) cell.appendChild(feedbackNode);
  return cell;
}

function buildCrewUpdateControl(task, context) {
  const cell = el("span", { class: "crew-cell" });
  const mutable = canMutateTask(task);
  const feedback = crewFeedback.get(task.id);
  const label = `Update crew for ${task.id}`;
  const select = el("select", {
    class: "task-crew-select mono",
    title: mutable ? label : aggregateRefusalTitle(label, "change crew"),
  });
  select.setAttribute("aria-label", label);
  const currentValue = explicitCrewValue(task);
  const staleCurrentValue = hasStaleExplicitCrew(task);
  select.dataset.currentValue = currentValue;
  select.dataset.staleCurrentValue = staleCurrentValue ? currentValue : "";

  const defaultOption = el("option", {
    text: defaultCrewOptionText(task),
  });
  defaultOption.value = "";
  select.appendChild(defaultOption);

  const crews = Array.isArray(lastCrewPayload.crews) ? lastCrewPayload.crews : [];
  for (const crew of crews) {
    const option = el("option", {
      text: crew.name,
      title: crewOptionTitle(crew),
    });
    option.value = crew.name;
    select.appendChild(option);
  }

  if (staleCurrentValue) {
    const option = el("option", {
      text: `${currentValue} (missing)`,
      title: "Configured crew no longer found",
    });
    option.value = currentValue;
    select.appendChild(option);
  }

  if (crews.length === 0) {
    select.disabled = true;
    defaultOption.textContent = "crew unavailable";
  }
  if (!mutable || (feedback && feedback.kind === "pending")) {
    select.disabled = true;
  }

  select.value = currentValue;
  stopRowInteraction(cell);
  stopRowInteraction(select);
  select.addEventListener("change", (event) => {
    event.stopPropagation();
    applyTaskCrewChange(task, select.value || "", context);
  });

  cell.appendChild(select);
  const feedbackNode = buildMutationFeedback(feedback, () => {
    if (feedback && feedback.undo) applyTaskCrewChange(task, feedback.undo.previousValue, context);
  });
  if (feedbackNode) cell.appendChild(feedbackNode);
  return cell;
}

async function applyTaskStatusChange(task, nextStatus, context) {
  if (!nextStatus || nextStatus === task.status || !canMutateTask(task)) return;
  const transition = statusTransition(task, nextStatus);
  if (!transition) {
    statusFeedback.set(task.id, {
      kind: "error",
      text: `status update unavailable: ${task.status} cannot move to ${nextStatus}`,
    });
    renderTasks(taskList(context), context);
    return;
  }

  const payload = { status: nextStatus };
  if (transition.required_field) {
    const evidence = collectStatusTransitionEvidence(task, nextStatus, transition.required_field);
    if (!evidence) {
      statusFeedback.set(task.id, {
        kind: "error",
        text: statusTransitionEvidenceUnavailable(transition.required_field),
      });
      renderTasks(taskList(context), context);
      return;
    }
    payload[transition.required_field] = evidence;
  }

  const previousValue = task.status;
  statusFeedback.set(task.id, { kind: "pending", text: "saving…" });
  renderTasks(taskList(context), context);
  try {
    const updatedTask = await patchJson(taskMutationPath(task), payload);
    applyUpdatedTask(updatedTask, context);
    const feedback = {
      kind: "success",
      text: "status saved",
    };
    const reverse = statusTransition(updatedTask, previousValue);
    if (reverse && !reverse.required_field) {
      feedback.undo = { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS };
    }
    statusFeedback.set(task.id, feedback);
  } catch (error) {
    statusFeedback.set(task.id, {
      kind: "error",
      text: `status update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  renderTasks(taskList(context), context);
  scheduleFeedbackExpiry(statusFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
}

function collectStatusTransitionEvidence(task, nextStatus, requiredField) {
  if (typeof window.prompt !== "function") return null;
  const label = requiredField === "plan" ? "execution plan" : "completion summary";
  const currentValue = requiredField === "plan" ? task.plan : task.execution_summary;
  const value = window.prompt(
    `A non-empty ${label} is required before moving ${task.id} to ${nextStatus}.`,
    currentValue || "",
  );
  return value && value.trim() ? value.trim() : null;
}

function statusTransitionEvidenceUnavailable(requiredField) {
  const label = requiredField === "plan" ? "execution plan" : "completion summary";
  return `status update unavailable: a non-empty ${label} is required`;
}

async function applyTaskCrewChange(task, nextValue, context) {
  const previousValue = explicitCrewValue(task);
  if (nextValue === previousValue || !canMutateTask(task)) return;
  crewFeedback.set(task.id, { kind: "pending", text: "saving…" });
  renderTasks(taskList(context), context);
  try {
    const updatedTask = await patchJson(taskMutationPath(task), { crew: nextValue || null });
    applyUpdatedTask(updatedTask, context);
    crewFeedback.set(task.id, {
      kind: "success",
      text: "crew saved",
      undo: { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS },
    });
  } catch (error) {
    crewFeedback.set(task.id, {
      kind: "error",
      text: `crew update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  renderTasks(taskList(context), context);
  scheduleFeedbackExpiry(crewFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
}

/* ORB-10444: one-click Ship. The dispatch carries only the task id — the
   pipeline resolves the crew from the task's own record and the mode from the
   workspace's configured default — so there is deliberately no crew picker and
   no PR/local toggle here. The resulting run id (or the server's error) is
   surfaced so the operator can see the click took effect. */
async function shipTask(task, detail, btnNode, context) {
  if (shipInFlightTaskIds.has(task.id)) return;
  shipInFlightTaskIds.add(task.id);
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  const oldText = btnNode.textContent;
  btnNode.innerHTML = `<span class="spinner"></span>wait`;
  try {
    const result = await postJson("/api/workflows/ship", { task_ids: [task.id] });
    const runId = result && result.run_id ? result.run_id : "(no run id)";
    const state = result && result.state ? result.state : "submitted";
    taskActionNotice = `${task.id}: ship run ${runId} ${state}`;
    expandedTaskIds.delete(task.id);
    await refreshTasks(context);
  } catch (error) {
    // Only a failed dispatch releases the guard; a succeeded one stays held so
    // a second click cannot queue a duplicate run behind the first.
    shipInFlightTaskIds.delete(task.id);
    for (const b of detail.querySelectorAll(".action")) b.disabled = false;
    btnNode.textContent = oldText;
    detail.prepend(
      el("div", { class: "action-error", text: `ship failed: ${error.message || String(error)}` }),
    );
  }
}

/* ORB-10444: human comments on a task. The write goes to the task's existing
   review-thread structure via POST /api/tasks/<id>/comments, which records a
   human author rather than the server process's ambient identity. */
function showCommentForm(task, detail, actions, context) {
  const form = el("div", { class: "comment-form" });
  form.addEventListener("click", (e) => e.stopPropagation());
  detail.dataset.draft = "comment";
  const ta = el("textarea");
  ta.placeholder = "comment";
  const buttons = el("div", { class: "actions" });
  const submit = el("button", { class: "action comment", text: "post" });
  const cancel = el("button", { class: "action cancel", text: "cancel" });
  submit.addEventListener("click", async (e) => {
    e.stopPropagation();
    const message = ta.value.trim();
    if (!message) {
      ta.focus();
      return;
    }
    const prior = detail.querySelector(".action-error");
    if (prior) prior.remove();
    submit.disabled = true;
    cancel.disabled = true;
    try {
      await postJson(`/api/tasks/${encodeURIComponent(task.id)}/comments`, { message });
      // The draft is spent: let the next render rebuild the detail so the
      // posted comment appears.
      delete detail.dataset.draft;
      await refreshTasks(context);
    } catch (error) {
      submit.disabled = false;
      cancel.disabled = false;
      detail.prepend(
        el("div", {
          class: "action-error",
          text: `comment failed: ${error.message || String(error)}`,
        }),
      );
    }
  });
  cancel.addEventListener("click", (e) => {
    e.stopPropagation();
    delete detail.dataset.draft;
    form.replaceWith(actions);
  });
  buttons.appendChild(submit);
  buttons.appendChild(cancel);
  form.appendChild(ta);
  form.appendChild(buttons);
  actions.replaceWith(form);
  ta.focus();
}

function showRejectForm(task, detail, actions, context) {
  const form = el("div", { class: "reject-form" });
  form.addEventListener("click", (e) => e.stopPropagation());
  detail.dataset.draft = "reject";
  const ta = el("textarea");
  ta.placeholder = "reason for rejection";
  const buttons = el("div", { class: "actions" });
  const submit = el("button", { class: "action reject", text: "submit" });
  const cancel = el("button", { class: "action cancel", text: "cancel" });
  submit.addEventListener("click", (e) => {
    e.stopPropagation();
    const note = ta.value.trim();
    if (!note) {
      ta.focus();
      return;
    }
    runAction(task, "reject", detail, { note }, submit, context);
  });
  cancel.addEventListener("click", (e) => {
    e.stopPropagation();
    delete detail.dataset.draft;
    form.replaceWith(actions);
  });
  buttons.appendChild(submit);
  buttons.appendChild(cancel);
  form.appendChild(ta);
  form.appendChild(buttons);
  actions.replaceWith(form);
  ta.focus();
}

async function runAction(task, kind, detail, body, btnNode, context, opts = {}) {
  // Disable action controls while in flight to prevent double-clicks.
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  let oldText = "";
  if (btnNode && btnNode.tagName === "BUTTON") {
    oldText = btnNode.textContent;
    btnNode.innerHTML = `<span class="spinner"></span>wait`;
  }
  // Clear any prior error
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  try {
    const res = await fetch(withWorkspace(opts.path || `/api/tasks/${encodeURIComponent(task.id)}/${kind}`), {
      method: opts.method || "POST",
      headers: body ? { "content-type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    if (!res.ok) {
      let msg = `${kind} failed: HTTP ${res.status}`;
      try {
        const errBody = await res.json();
        if (errBody && errBody.error) msg = `${kind} failed: ${errBody.error}`;
      } catch (_) {
        /* keep generic msg */
      }
      throw new Error(msg);
    }
    if (opts.collapseOnSuccess !== false) expandedTaskIds.delete(task.id);
    if (opts.successNotice) taskActionNotice = opts.successNotice;
    await refreshTasks(context);
  } catch (err) {
    for (const b of detail.querySelectorAll(".action")) b.disabled = false;
    if (btnNode && btnNode.tagName === "BUTTON") btnNode.textContent = oldText;
    if (opts.onFailure) opts.onFailure();
    const errEl = el("div", { class: "action-error", text: String(err.message || err) });
    detail.prepend(errEl);
  }
}

function takeTaskActionNotice() {
  if (!taskActionNotice) return null;
  const notice = el("div", { class: "task-action-notice", text: taskActionNotice });
  notice.dataset.key = "task-action-notice";
  notice.dataset.hash = taskActionNotice;
  notice.setAttribute("role", "status");
  notice.setAttribute("aria-live", "polite");
  taskActionNotice = null;
  return notice;
}

// The pinned global-resolver result: a task outside the active filter, shown
// above the list with its own dismiss control.
function buildPinnedTask(ptask, context) {
  const row = el("div", {
    class: "row pinned-external",
    title: `${ptask.title} (global resolver; status ${ptask.status})`
  }, [
    el("span", { class: "id mono", text: ptask.id }),
    el("span", { class: "title", text: ptask.title }),
    buildStatusUpdateControl(ptask, context),
    buildCrewUpdateControl(ptask, context),
  ]);
  // The pinned row's detail is always open, so the row is a plain copy-the-id
  // action rather than a disclosure.
  makeToggleRow(row, {
    onToggle: (e) => {
      e.stopPropagation();
      if (navigator.clipboard) navigator.clipboard.writeText(ptask.id).catch(() => {});
    },
  });
  row.dataset.hash = `${ptask.id}-${ptask.title}-${ptask.status}-${ptask.crew || ""}-${ptask.resolved_crew || ""}-${crewOptionsSignature()}-${feedbackSignature(statusFeedback, ptask.id)}-${feedbackSignature(crewFeedback, ptask.id)}`;

  const detail = buildTaskDetail(ptask, context);
  const dismiss = el("button", { class: "action", text: "Close" });
  dismiss.title = "Dismiss global task detail";
  dismiss.addEventListener("click", (ev) => {
    ev.stopPropagation();
    pinnedExternalTask = null;
    renderTasks(taskList(context), context);
  });
  let actions = detail.querySelector(".actions");
  if (!actions) {
    actions = el("div", { class: "actions" });
    detail.appendChild(actions);
  }
  actions.appendChild(dismiss);

  const wrap = el("div", { class: "pinned-task-wrap" }, [row, detail]);
  wrap.dataset.key = `pinned-${ptask.id}`;
  wrap.dataset.hash = `${row.dataset.hash}-${JSON.stringify(ptask)}-${detailFeedbackSignature(ptask.id)}`;
  return wrap;
}

/* ORB-11655: a comment or reject form, or an open field editor (ORB-12235),
   holds text the operator is still typing, so the 30 s refresh must not rebuild
   the node that contains it — not even when the task itself changed. Collect the
   live top-level nodes holding one, keyed the way syncNodes keys them, and reuse
   them verbatim. The detail resumes tracking task data as soon as it closes.
   The nested lookups are for the pinned-task wrapper, whose draft marker sits on
   the detail inside it rather than on the keyed node itself. */
function openDraftNodes(body) {
  const drafts = new Map();
  for (const node of Array.from(body.children)) {
    if (!node.dataset.key) continue;
    const holdsDraft =
      node.dataset.draft ||
      node.dataset.editing ||
      node.querySelector?.("[data-draft]") ||
      node.querySelector?.("[data-editing]");
    if (holdsDraft) drafts.set(node.dataset.key, node);
  }
  return drafts;
}

export function renderTasks(tasks, context) {
  if (!panelCanRender("tasks-body")) return;
  const body = $("tasks-body");
  if (!body) return;

  // ORB-00030: in the aggregate ("All workspaces") view each task carries its
  // owning workspace; show it as a badge in the Title cell. Detected from the
  // data so no extra plumbing is needed (single-workspace lists lack the field).
  const aggregate = Array.isArray(tasks) && tasks.some((t) => t && t.workspace_name);

  // Auto-clear a pinned jump only when the task is actually in the filtered
  // list. Matching status/search chips is not enough: a truncated fetch can
  // omit the jumped task even when its chip is on.
  if (pinnedExternalTask && pinnedExternalTask.task) {
    const p = pinnedExternalTask.task;
    if (filterTasks(tasks, context).some((task) => task.id === p.id)) {
      pinnedExternalTask = null;
    }
  }

  // Collected as a plain array rather than a document fragment: a fragment
  // would detach every reused node from the panel, and moving a node is what
  // costs an open draft its caret. syncNodes leaves a node it already holds in
  // place.
  const nodes = [];
  const drafts = openDraftNodes(body);
  const notice = takeTaskActionNotice();

  // Render pinned external task detail (for statuses outside active filter) at top.
  if (pinnedExternalTask && pinnedExternalTask.task) {
    const ptask = pinnedExternalTask.task;
    nodes.push(drafts.get(`pinned-${ptask.id}`) || buildPinnedTask(ptask, context));
  }

  const filtered = filterTasks(tasks, context);
  $("tasks-count").textContent = formatTaskCount(filtered.length, tasks.length, tasksMeta(context));
  renderTaskPagination(context);
  // ORB-10972: the rail shows the same filtered count the panel header does,
  // so the Tasks entry reads correctly from any other tab.
  const railCount = document.getElementById("rail-count-tasks");
  if (railCount) railCount.textContent = String(filtered.length);
  renderFilterSummary(context);
  if (filtered.length === 0 && nodes.length === 0) {
    const defaultText = tasks.length === 0 ? "No tasks available." : "No tasks match filter.";
    const emptyState = el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: defaultText })
    ]);
    syncNodes(body, notice ? [notice, emptyState] : [emptyState]);
    return;
  }
  const groups = new Map();
  if (notice) nodes.push(notice);

  // Column header strip (once, before first group-header). Uses .row.header so grid
  // (and all @media overrides) are identical to data rows; labels sit over ID/Title/Status/Crew.
  const colHeader = el("div", { class: "row header" }, [
    el("span", { class: "id", text: "ID" }),
    el("span", { class: "title", text: "Title" }),
    el("span", { class: "status-cell", text: "Status" }),
    el("span", { class: "crew-cell", text: "Crew" }),
  ]);
  colHeader.dataset.key = "task-col-header";
  nodes.push(colHeader);

  for (const t of filtered) {
    if (!groups.has(t.status)) groups.set(t.status, []);
    groups.get(t.status).push(t);
  }
  const order = statusOrder(context);
  const ordered = order.filter((s) => groups.has(s)).concat(
    [...groups.keys()].filter((s) => !order.includes(s)),
  );
  for (const status of ordered) {
    const group = groups.get(status);
    const header = el("div", { class: "group-header" }, [
      statusPill(status),
      el("span", { class: "group-count", text: `${group.length}` }),
    ]);
    header.dataset.key = `header-${status}`;
    header.dataset.hash = `${status}-${group.length}`;
    nodes.push(header);
    for (const t of group) {
      const idSpan = el("span", { class: "id mono", text: t.id, title: "Click to copy ID" });
      idSpan.addEventListener("click", (e) => {
        e.stopPropagation();
        navigator.clipboard.writeText(t.id).catch(() => {});
        const oldText = idSpan.textContent;
        idSpan.textContent = "copied!";
        idSpan.style.color = "var(--state-success)";
        setTimeout(() => {
          idSpan.textContent = oldText;
          idSpan.style.color = "";
        }, 1000);
      });
      const titleCell = aggregate && t.workspace_name
        ? el("span", { class: "title" }, [
            el("span", { class: "ws-badge mono", text: t.workspace_name, title: `Workspace: ${t.workspace_name}` }),
            t.title,
          ])
        : el("span", { class: "title", text: t.title });
      const row = el("div", { class: "row", title: t.title }, [
        idSpan,
        titleCell,
        buildStatusUpdateControl(t, context),
        buildCrewUpdateControl(t, context),
      ]);
      row.dataset.key = `task-${t.id}`;
      // Basic hash based on row presentation parameters + expanded state
      row.dataset.hash = `${t.id}-${t.title}-${t.status}-${t.crew || ""}-${t.resolved_crew || ""}-${t.workspace_id || ""}-${crewOptionsSignature()}-${feedbackSignature(statusFeedback, t.id)}-${feedbackSignature(crewFeedback, t.id)}-${expandedTaskIds.has(t.id)}`;
      makeToggleRow(row, {
        expanded: expandedTaskIds.has(t.id),
        // The detail node only exists while the row is open, so the IDREF is
        // only published while it actually resolves.
        controls: expandedTaskIds.has(t.id) ? `detail-${t.id}` : null,
        onToggle: () => {
          const toggle = () => {
            if (expandedTaskIds.has(t.id)) expandedTaskIds.delete(t.id);
            else expandedTaskIds.add(t.id);
            renderTasks(taskList(context), context);
          };
          if (document.startViewTransition) {
            row.style.viewTransitionName = `task-row-${t.id}`;
            document.startViewTransition(toggle).finished.then(() => {
              row.style.viewTransitionName = "";
            });
          } else {
            toggle();
          }
        },
      });
      if (expandedTaskIds.has(t.id)) row.classList.add("expanded");
      nodes.push(row);
      if (expandedTaskIds.has(t.id)) {
        const key = `detail-${t.id}`;
        const draft = drafts.get(key);
        if (draft) {
          nodes.push(draft);
        } else {
          const detail = buildTaskDetail(t, context);
          detail.dataset.key = key;
          // The row's `aria-controls` points here, so the detail needs a real id.
          detail.id = key;
          // Diff by full task object stringified, plus the feedback the detail's
          // own controls render (the row hash only covers status and crew).
          detail.dataset.hash = `${JSON.stringify(t)}-${detailFeedbackSignature(t.id)}`;
          nodes.push(detail);
        }
      }
    }
  }
  syncNodes(body, nodes);
}
