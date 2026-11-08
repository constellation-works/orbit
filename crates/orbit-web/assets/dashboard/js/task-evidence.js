// Orbit dashboard task review, owed criteria and artifacts.

import { el, isAggregateView, makeToggleRow, teardownNode } from './common.js';
import { renderMarkdown } from './markdown.js';
import * as taskShared from './task-shared.js';
import * as taskReadiness from './task-readiness.js';

// ORB-11333: the settled before-PR review gate. Every value comes from the
// certificate the gate wrote; nothing here is inferred from status or tags.
export function buildReviewGate(review) {
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
    `reviewer commit: ${Array.isArray(review.repair_commits) && review.repair_commits.length ? review.repair_commits.map((c) => `${c.commit.slice(0, 12)} by ${c.author}`).join(", ") : "none"}`,
    `findings: ${Array.isArray(review.findings) ? review.findings.length : 0} · validation: ${Array.isArray(review.validation) ? review.validation.length : 0} record(s), complete: ${review.validation_complete ? "yes" : "no"}`,
    `reviewer runtime: ${consumed.seconds ?? 0}s of ${budget.minutes ?? "?"} min`,
  ];
  if (Array.isArray(review.validation_limitations) && review.validation_limitations.length) {
    lines.push(`not established: ${review.validation_limitations.join("; ")}`);
  }
  if (Array.isArray(review.retained_obligations) && review.retained_obligations.length) {
    lines.push(`retained from earlier report revisions: ${review.retained_obligations.map((o) => `${o.validation?.command ?? "?"} ${o.validation?.outcome ?? "?"}`).join(", ")}`);
  }
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

export function buildOwedCriteria(owed, task, detail, context) {
  const wrap = el("div", { class: "owed-criteria" });
  if (task && detail && context) wrap.dataset.operatorRecordFeedback = "true";
  for (const entry of owed) {
    const parts = [
      `${entry.criterion}. [${entry.state}] on ${entry.host}: ${entry.command} (target ${entry.target})`,
    ];
    if (entry.candidate_evidence) parts.push(`candidate: ${entry.candidate_evidence}`);
    if (entry.evidence) parts.push(`recorded by ${entry.recorded_by ?? "—"}: ${entry.evidence}`);
    const row = el("div", { class: "owed-criterion" }, [
      el("span", { text: parts.join(" · ") }),
    ]);
    if (task && detail && context) row.dataset.operatorRecord = "true";
    if (task && detail && context && entry.state !== "met") {
      const mutable = taskShared.canMutateTask(task);
      const mark = el("button", {
        class: "action post-deploy-mark-met",
        type: "button",
        text: "Mark met",
        title: mutable
          ? `Record that the deployed build met criterion ${entry.criterion}, with your measurement evidence; this settles the owed criterion`
          : taskShared.aggregateRefusalTitle("Mark met", "record a post-deploy measurement"),
      });
      mark.disabled = !mutable;
      mark.addEventListener("click", (e) => { e.stopPropagation(); taskReadiness.runOperatorRecord(task, detail, mark, context, { kind: "post-deploy", criterion: entry.criterion }); });
      row.appendChild(mark);
    }
    wrap.appendChild(row);
  }
  if (task && detail && context) {
    const refusal = taskReadiness.operatorRecordRefusalNode(task);
    if (refusal) wrap.appendChild(refusal);
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

function artifactUrl(task, path) {
  const encodedPath = String(path)
    .split("/")
    .map((part) => encodeURIComponent(part))
    .join("/");
  return taskShared.taskMutationPath(task, `/artifacts/${encodedPath}`);
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
const ARTIFACT_FETCH_TIMEOUT_MS = 30000;

function artifactFileName(path) {
  return String(path).split("/").pop() || String(path);
}

function buildArtifactDownloadLink(artifact, objectUrl, text) {
  const link = el("a", { text: text || `Download ${artifact.path}` });
  link.href = objectUrl;
  link.download = artifactFileName(artifact.path);
  return link;
}

// A blob URL pins its bytes until revoked. `syncNodes` tears down a detail it
// replaces, so the preview releases them when the task detail goes away.
function releaseOnTeardown(node, objectUrl) {
  node.teardown = () => URL.revokeObjectURL(objectUrl);
  return node;
}

function buildArtifactImage(artifact, blob) {
  const objectUrl = URL.createObjectURL(blob);
  const figure = releaseOnTeardown(el("div", { class: "artifact-image-view" }), objectUrl);
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
  return response.blob().then((blob) => {
    const objectUrl = URL.createObjectURL(blob);
    return releaseOnTeardown(buildArtifactDownloadLink(artifact, objectUrl), objectUrl);
  });
}

const ARTIFACTS_PAGE_SIZE = 25;

export function buildArtifacts(task) {
  const wrap = el("div", { class: "artifacts" });
  const units = [];
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

    // The preview owns its read. Retiring the task detail aborts a read in
    // flight, and a preview that resolves anyway is released at once: teardown
    // has already passed over this subtree, so a blob URL mounted into it later
    // would never be revoked.
    let request = null;
    let failed = false;
    let retired = false;
    preview.teardown = () => {
      retired = true;
      if (request) request.abort();
    };

    makeToggleRow(row, {
      expanded: false,
      controls: preview.id,
      onToggle: async (e) => {
        e.stopPropagation();
        if (preview.dataset.loaded === "true" || request) {
          revealPreview(preview.hidden);
          return;
        }
        // A failed load stays collapsible: hiding the error is a toggle, and
        // only expanding it again retries the fetch.
        if (failed && !preview.hidden) {
          revealPreview(false);
          return;
        }
        failed = false;
        revealPreview(true);
        preview.textContent = "loading...";
        // Same 30s bound as fetchJson, covering the body read too, so a
        // stalled artifact read ends in an error instead of "loading..." forever.
        const controller = new AbortController();
        request = controller;
        const timeout = setTimeout(() => controller.abort(), ARTIFACT_FETCH_TIMEOUT_MS);
        try {
          if (isAggregateView() && !task.workspace_id) throw new Error("task workspace is unknown");
          const response = await fetch(artifactUrl(task, path), { signal: controller.signal });
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          const view = await buildArtifactPreview(artifact, response);
          if (retired) {
            teardownNode(view);
            return;
          }
          preview.replaceChildren(view);
          preview.dataset.loaded = "true";
        } catch (error) {
          if (retired) return;
          failed = true;
          const reason = error && error.name === "AbortError" ? "request timed out after 30 seconds" : error.message;
          preview.textContent = `Unable to load ${path}: ${reason}`;
        } finally {
          clearTimeout(timeout);
          request = null;
        }
      },
    });
    units.push({ artifact, row, preview, search: `${path} ${mediaType}`.toLowerCase() });
  }
  if (units.length <= ARTIFACTS_PAGE_SIZE) {
    for (const unit of units) {
      wrap.appendChild(unit.row);
      wrap.appendChild(unit.preview);
    }
    return wrap;
  }

  // Hundreds of rows made the pane thousands of pixels tall. Only one page
  // is in the DOM at a time; detached units stay alive (no teardown on
  // detach) so a loaded preview, its blob URL and an in-flight read survive
  // paging and filtering, and return exactly as left.
  const filterId = `artifact-filter-${task.id}`;
  const filterLabel = el("label", { class: "artifacts-filter-label", text: "Filter artifacts" });
  filterLabel.htmlFor = filterId;
  const filterInput = el("input", { class: "artifacts-filter", type: "search" });
  filterInput.id = filterId;
  filterInput.placeholder = "Filter by path or type…";
  filterInput.setAttribute("aria-describedby", `artifact-status-${task.id}`);
  const toolbar = el("div", { class: "artifacts-toolbar" }, [filterLabel, filterInput]);
  const status = el("div", { class: "artifacts-status", role: "status", "aria-live": "polite" });
  status.id = `artifact-status-${task.id}`;
  const list = el("div", { class: "artifacts-list" });
  const empty = el("p", { class: "field-empty artifacts-empty", text: "No artifacts match this filter." });
  empty.hidden = true;
  const prev = el("button", { class: "action artifacts-page-btn", type: "button", text: "Previous" });
  const next = el("button", { class: "action artifacts-page-btn", type: "button", text: "Next" });
  const pageLabel = el("span", { class: "artifacts-page-label", "aria-live": "polite" });
  const pagination = el("nav", { class: "artifacts-pagination", "aria-label": "Artifact pages" }, [prev, pageLabel, next]);

  let filterText = "";
  let page = 0;
  const render = () => {
    const filtered = filterText
      ? units.filter((unit) => unit.search.includes(filterText))
      : units;
    const pageCount = Math.max(1, Math.ceil(filtered.length / ARTIFACTS_PAGE_SIZE));
    page = Math.min(page, pageCount - 1);
    const start = page * ARTIFACTS_PAGE_SIZE;
    const visible = filtered.slice(start, start + ARTIFACTS_PAGE_SIZE);
    // replaceChildren only detaches: it never runs a preview's teardown, so
    // blob URLs stay live and in-flight reads keep their owner.
    list.replaceChildren(...visible.flatMap((unit) => [unit.row, unit.preview]));
    empty.hidden = filtered.length !== 0;
    status.textContent = filtered.length === 0
      ? `No artifacts match this filter. ${units.length} artifacts total.`
      : filterText
        ? `Showing ${start + 1}–${start + visible.length} of ${filtered.length} matching artifacts (${units.length} total)`
        : `Showing ${start + 1}–${start + visible.length} of ${units.length} artifacts`;
    pageLabel.textContent = `Page ${page + 1} of ${pageCount}`;
    prev.disabled = page === 0;
    next.disabled = page >= pageCount - 1;
    pagination.hidden = pageCount <= 1;
  };
  filterInput.addEventListener("input", () => {
    filterText = filterInput.value.trim().toLowerCase();
    page = 0;
    render();
  });
  // A button that disables itself under focus drops focus to the body; hand
  // it to the other page button so keyboard users stay in the pagination.
  prev.addEventListener("click", () => {
    if (page > 0) { page -= 1; render(); if (prev.disabled) next.focus(); }
  });
  next.addEventListener("click", () => {
    page += 1; render(); if (next.disabled) prev.focus();
  });
  // Detached previews are outside this subtree when the detail retires, so
  // the traversal in teardownNode cannot reach them: retire them here.
  // Attached ones are left to that traversal, which runs after this.
  wrap.teardown = () => {
    for (const unit of units) {
      if (!list.contains(unit.preview)) teardownNode(unit.preview);
    }
  };
  wrap.append(toolbar, status, list, empty, pagination);
  render();
  return wrap;
}
