// Orbit dashboard task-domain rendering and actions.
// Pure vanilla JS, split into ES modules with no build step.

import { el, statusPill, patchJson, postJson, syncNodes, isAggregateView, withWorkspace } from './common.js';
import { renderMarkdown, renderMarkdownInline } from './markdown.js';

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
const MUTATION_UNDO_WINDOW_MS = 8000;
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

function statusUpdateTargets(context) {
  return context && Array.isArray(context.statusUpdateTargets)
    ? context.statusUpdateTargets
    : [];
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

// ORB-10874: explicit shown/total/server-limit language instead of the
// ambiguous `N/50` shorthand — `50` could previously have meant a total, a
// page size, or a hard cap with no way to tell which from the UI alone.
export function formatTaskCount(filteredCount, fetchedCount, meta) {
  if (meta && Number.isFinite(meta.total)) {
    const base = filteredCount === fetchedCount
      ? `${filteredCount} shown`
      : `${filteredCount} shown (of ${fetchedCount} fetched)`;
    return meta.truncated
      ? `${base} · ${meta.total} total · server limit ${meta.limit}`
      : `${base} · ${meta.total} total`;
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

function taskMutationPath(task, suffix = "") {
  const base = `/api/tasks/${encodeURIComponent(task.id)}${suffix}`;
  if (isAggregateView() && task.workspace_id) {
    const sep = base.includes("?") ? "&" : "?";
    return `${base}${sep}workspace=${encodeURIComponent(task.workspace_id)}`;
  }
  return base;
}

function scheduleFeedbackExpiry(map, taskId, context, delay) {
  setTimeout(() => {
    const entry = map.get(taskId);
    if (entry && entry.kind !== "pending") {
      map.delete(taskId);
      renderTasks(taskList(context), context);
    }
  }, delay);
}

// Renders the inline pending/success/error text next to a status or crew
// select, plus a bounded "undo" button while `entry.undo` is still live. The
// wrapper is a live region so screen-reader users get the same feedback a
// sighted user reads from the text/color change.
function buildMutationFeedback(entry, onUndo) {
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
  ["complexity", "complexity"],
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

/* Global ID resolver support (ORB-00211): allow rendering detail for a task whose status
   is outside the active chip filter (done/rejected/archived) without adding it to the
   bulk-loaded list or mutating DASHBOARD_TASK_STATUSES. The pinned detail appears in a
   highlighted block at top of #tasks-body; auto-clears if user later enables matching chip. */
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
  const hash = buildTasksHash(context);
  if (window.location.hash !== hash) {
    window.location.hash = hash;
  } else {
    refreshChips(context);
    renderTasks(taskList(context), context);
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
  for (const artifact of task.artifacts) {
    const path = String(artifact.path || "");
    const mediaType = String(artifact.media_type || "application/octet-stream");
    const row = el("div", {
      class: "artifact-row",
      text: `${path} · ${mediaType} · ${fmtSize(artifact.size_bytes)}`,
    });
    const preview = el("div", { class: "artifact-preview" });
    preview.hidden = true;
    row.addEventListener("click", async (e) => {
      e.stopPropagation();
      if (preview.dataset.loaded === "true" && !preview.hidden) {
        preview.hidden = true;
        return;
      }
      if (preview.dataset.loaded === "true") {
        preview.hidden = false;
        return;
      }
      preview.hidden = false;
      preview.textContent = "loading...";
      try {
        const response = await fetch(artifactUrl(task.id, path));
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        preview.replaceChildren(await buildArtifactPreview(artifact, response));
        preview.dataset.loaded = "true";
      } catch (error) {
        preview.textContent = `Unable to load ${path}: ${error.message}`;
      }
    });
    wrap.appendChild(row);
    wrap.appendChild(preview);
  }
  return wrap;
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
      h4.addEventListener("click", (e) => {
        e.stopPropagation();
        block.classList.toggle("collapsed");
      });
    }
    block.appendChild(h4);
    block.appendChild(child);
    parent.appendChild(block);
  };

  if (task.description && task.description.trim()) {
    const view = el("div", { class: "markdown-body" });
    const rendered = renderMarkdown(task.description);
    if (rendered !== null) {
      view.innerHTML = rendered;
    } else {
      view.textContent = task.description;
    }
    addField(leftCol, "description", view);
  }

  if (Array.isArray(task.acceptance_criteria) && task.acceptance_criteria.length > 0) {
    const ul = el("ul", { class: "ac-list" });
    for (const ac of task.acceptance_criteria) {
      const rendered = renderMarkdownInline(ac);
      if (rendered !== null) {
        const li = el("li");
        li.innerHTML = rendered;
        ul.appendChild(li);
      } else {
        ul.appendChild(el("li", { text: ac }));
      }
    }
    addField(leftCol, "acceptance criteria", ul, true, true);
  }

  if (task.plan && task.plan.trim()) {
    const view = el("div", { class: "markdown-body" });
    const rendered = renderMarkdown(task.plan);
    if (rendered !== null) {
      view.innerHTML = rendered;
    } else {
      view.textContent = task.plan;
    }
    addField(leftCol, "plan", view, true, true);
  }

  if (task.execution_summary && task.execution_summary.trim()) {
    const view = el("div", { class: "markdown-body" });
    const rendered = renderMarkdown(task.execution_summary);
    if (rendered !== null) {
      view.innerHTML = rendered;
    } else {
      view.textContent = task.execution_summary;
    }
    addField(leftCol, "execution summary", view, true, true);
  }

  if (Array.isArray(task.artifacts) && task.artifacts.length > 0) {
    addField(leftCol, "artifacts", buildArtifacts(task), true, true);
  }

  if (Array.isArray(task.tags) && task.tags.length > 0) {
    rightCol.appendChild(buildTagRow(task.tags));
  }

  // Routine status/crew controls live inline on the task row; details remain read-only.
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

  if (Array.isArray(task.context_files) && task.context_files.length > 0) {
    const ul = el("ul", { class: "file-list" });
    for (const path of task.context_files) {
      ul.appendChild(el("li", { text: path }));
    }
    addField(rightCol, "context files", ul);
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
  if (task.status !== "archived") {
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
  const targets = statusUpdateTargets(context).filter((status) => status !== task.status);
  const color = `var(--status-${task.status}, var(--fg))`;
  const mutable = canMutateTask(task);
  const feedback = statusFeedback.get(task.id);
  const label = `Update status for ${task.id}`;
  const select = el("select", {
    class: "task-status-select mono",
    title: mutable ? label : `${label} — select a specific workspace to change status in aggregate view`,
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
    title: mutable ? label : `${label} — select a specific workspace to change crew in aggregate view`,
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
  const previousValue = task.status;
  statusFeedback.set(task.id, { kind: "pending", text: "saving…" });
  renderTasks(taskList(context), context);
  try {
    const updatedTask = await patchJson(taskMutationPath(task), { status: nextStatus });
    applyUpdatedTask(updatedTask, context);
    statusFeedback.set(task.id, {
      kind: "success",
      text: "status saved",
      undo: { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS },
    });
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

export function renderTasks(tasks, context) {
  const body = $("tasks-body");
  if (!body) return;

  // ORB-00030: in the aggregate ("All workspaces") view each task carries its
  // owning workspace; show it as a badge in the Title cell. Detected from the
  // data so no extra plumbing is needed (single-workspace lists lack the field).
  const aggregate = Array.isArray(tasks) && tasks.some((t) => t && t.workspace_name);

  // Auto-clear pinned external (global resolver) if its status+search now makes it
  // visible in the normal filtered list (user enabled the chip or cleared search).
  if (pinnedExternalTask && pinnedExternalTask.task) {
    const p = pinnedExternalTask.task;
    const q = (searchQueryValue(context) || "").toLowerCase();
    const act = activeStatusSet(context);
    const matchesQ = !q || (p.id && p.id.toLowerCase().includes(q)) || (p.title && p.title.toLowerCase().includes(q));
    if (act.has(p.status) && matchesQ) {
      pinnedExternalTask = null;
    }
  }

  const frag = document.createDocumentFragment();
  const notice = takeTaskActionNotice();

  // Render pinned external task detail (for statuses outside active filter) at top.
  if (pinnedExternalTask && pinnedExternalTask.task) {
    const ptask = pinnedExternalTask.task;
    const pRow = el("div", {
      class: "row pinned-external",
      title: `${ptask.title} (global resolver; status ${ptask.status})`
    }, [
      el("span", { class: "id mono", text: ptask.id }),
      el("span", { class: "title", text: ptask.title }),
      buildStatusUpdateControl(ptask, context),
      buildCrewUpdateControl(ptask, context),
    ]);
    pRow.addEventListener("click", (e) => {
      e.stopPropagation();
      if (navigator.clipboard) navigator.clipboard.writeText(ptask.id).catch(() => {});
    });
    pRow.dataset.hash = `${ptask.id}-${ptask.title}-${ptask.status}-${ptask.crew || ""}-${ptask.resolved_crew || ""}-${crewOptionsSignature()}-${feedbackSignature(statusFeedback, ptask.id)}-${feedbackSignature(crewFeedback, ptask.id)}`;
    const pDetail = buildTaskDetail(ptask, context);
    // Dismiss button to close the global detail without affecting chips
    const dismiss = el("button", { class: "action", text: "Close" });
    dismiss.title = "Dismiss global task detail";
    dismiss.addEventListener("click", (ev) => {
      ev.stopPropagation();
      pinnedExternalTask = null;
      renderTasks(taskList(context), context);
    });
    let acts = pDetail.querySelector(".actions");
    if (!acts) {
      acts = el("div", { class: "actions" });
      pDetail.appendChild(acts);
    }
    acts.appendChild(dismiss);
    const pWrap = el("div", { class: "pinned-task-wrap" }, [pRow, pDetail]);
    pWrap.dataset.key = `pinned-${ptask.id}`;
    pWrap.dataset.hash = `${pRow.dataset.hash}-${JSON.stringify(ptask)}`;
    frag.appendChild(pWrap);
  }

  const filtered = filterTasks(tasks, context);
  $("tasks-count").textContent = formatTaskCount(filtered.length, tasks.length, tasksMeta(context));
  // ORB-10972: the rail shows the same filtered count the panel header does,
  // so the Tasks entry reads correctly from any other tab.
  const railCount = document.getElementById("rail-count-tasks");
  if (railCount) railCount.textContent = String(filtered.length);
  renderFilterSummary(context);
  if (filtered.length === 0 && frag.children.length === 0) {
    const defaultText = tasks.length === 0 ? "No tasks available." : "No tasks match filter.";
    const emptyState = el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: defaultText })
    ]);
    syncNodes(body, notice ? [notice, emptyState] : [emptyState]);
    return;
  }
  const groups = new Map();
  if (notice) frag.appendChild(notice);

  // Column header strip (once, before first group-header). Uses .row.header so grid
  // (and all @media overrides) are identical to data rows; labels sit over ID/Title/Status/Crew.
  const colHeader = el("div", { class: "row header" }, [
    el("span", { class: "id", text: "ID" }),
    el("span", { class: "title", text: "Title" }),
    el("span", { class: "status-cell", text: "Status" }),
    el("span", { class: "crew-cell", text: "Crew" }),
  ]);
  colHeader.dataset.key = "task-col-header";
  frag.appendChild(colHeader);

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
    frag.appendChild(header);
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
      row.addEventListener("click", () => {
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
      });
      if (expandedTaskIds.has(t.id)) row.classList.add("expanded");
      frag.appendChild(row);
      if (expandedTaskIds.has(t.id)) {
        const detail = buildTaskDetail(t, context);
        detail.dataset.key = `detail-${t.id}`;
        // Diff by full task object stringified
        detail.dataset.hash = JSON.stringify(t);
        frag.appendChild(detail);
      }
    }
  }
  syncNodes(body, Array.from(frag.children));
}
