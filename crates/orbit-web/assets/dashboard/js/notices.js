// Notices view and the fleet-notice banner.
//
// Reads `GET /api/notices` (open notices plus the last 24h of closed and
// expired ones) and writes through `POST /api/notices` and
// `POST /api/notices/<id>/close`, the dashboard routes over the same core
// post/close as `orbit.notice.post` / `orbit.notice.close`. Every title,
// body, reason and label is inserted with `el()` / textContent, never as
// HTML, so a notice that contains markup reads as text.

import { el, fetchJson, formatClock, formatDateTime, getHostRevision, getWorkspaceRevision, isAggregateView, onWorkspaceChange, postJson, requestPanel, syncNodes } from './common.js';

const $ = (id) => document.getElementById(id);
const text = (value) => (typeof value === "string" ? value : "");

function noticesPath() {
  return isAggregateView() ? "/api/notices?workspace=all" : "/api/notices";
}

// One /api/notices read serves the banner, the rail count and the Notices
// panel, which all render from the same payload. Concurrent callers join
// the read in flight, and a completed read is reused for half the 30s
// dashboard poll interval: every scheduled poll still reads fresh, while
// a burst of refreshes (tab entry plus an explicit Refresh, or the banner
// beside the Notices panel) shares one request instead of spending the
// ten-request poll budget on a duplicate. A failed read is never reused,
// and a host or workspace switch changes the key, so no payload crosses
// scopes. Post/close mutations clear the shared read before refreshing.
const NOTICES_READ_SHARE_MS = 15_000;
let noticesRead = null;

function fetchNotices() {
  const key = `${getHostRevision()}:${getWorkspaceRevision()}:${noticesPath()}`;
  if (noticesRead?.key === key
    && (noticesRead.pending || Date.now() - noticesRead.at < NOTICES_READ_SHARE_MS)) {
    return noticesRead.promise;
  }
  const read = { key, pending: true, at: 0, promise: null };
  read.promise = fetchJson(noticesPath()).then(
    (payload) => {
      read.pending = false;
      read.at = Date.now();
      return payload;
    },
    (error) => {
      read.pending = false;
      if (noticesRead === read) noticesRead = null;
      throw error;
    },
  );
  noticesRead = read;
  return read.promise;
}

function ageLabel(notice, now = Date.now()) {
  const opened = Date.parse(text(notice?.opened_at));
  if (!Number.isFinite(opened)) return "";
  const seconds = Math.max(0, Math.floor((now - opened) / 1000));
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m old`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h old`;
  return `${Math.floor(seconds / 86400)}d old`;
}

function expiryLabel(notice, now = Date.now()) {
  const expires = Date.parse(text(notice?.expires_at));
  if (!Number.isFinite(expires)) return "";
  const seconds = Math.floor((expires - now) / 1000);
  if (seconds <= 0) return "expired";
  if (seconds < 3600) return `expires in ${Math.max(1, Math.floor(seconds / 60))}m`;
  if (seconds < 86400) return `expires in ${Math.floor(seconds / 3600)}h`;
  return `expires in ${Math.floor(seconds / 86400)}d`;
}

function chip(label, className = "") {
  return el("span", { class: `notice-chip${className ? ` ${className}` : ""}`, text: label });
}

function noticeLink(notice) {
  const link = el("a", { class: "notice-banner-link", text: "Notices" });
  link.href = "#notices";
  link.title = text(notice?.id);
  return link;
}

function setRailNoticeCount(count) {
  const railCount = $("rail-count-notices");
  if (railCount) railCount.textContent = count ? String(count) : "";
}

function hideNoticeBanner() {
  const banner = $("notice-banner");
  if (!banner) return;
  banner.hidden = true;
  banner.replaceChildren();
}

// The banner and its rail count belong to the workspace visit that read them.
// A scope switch clears both, so a read still in flight for the previous scope
// cannot restore them, and a failed read for the new scope shows none of them.
onWorkspaceChange(() => {
  setRailNoticeCount(0);
  hideNoticeBanner();
});

export function renderNoticeBanner(payload) {
  const banner = $("notice-banner");
  if (!banner) return;
  setRailNoticeCount(Array.isArray(payload?.open) ? payload.open.length : 0);
  const notices = Array.isArray(payload?.banner) ? payload.banner : [];
  if (!notices.length) {
    hideNoticeBanner();
    return;
  }
  const now = Date.now();
  const rows = notices.map((notice) => {
    const row = el("div", { class: `notice-banner-row notice-${text(notice.kind)}` }, [
      chip(text(notice.kind) || "notice", `notice-kind-${text(notice.kind)}`),
      el("span", { class: "notice-banner-title", text: text(notice.title) }),
      el("span", { class: "notice-banner-meta", text: [text(notice.scope), ageLabel(notice, now), expiryLabel(notice, now)].filter(Boolean).join(" · ") }),
      noticeLink(notice),
    ]);
    row.dataset.key = `banner-${text(notice.id)}`;
    row.dataset.hash = JSON.stringify([notice.revision, text(notice.kind), text(notice.title), text(notice.scope), ageLabel(notice, now), expiryLabel(notice, now)]);
    return row;
  });
  banner.hidden = false;
  syncNodes(banner, rows);
}

export async function fetchAndRenderNoticeBanner() {
  const banner = $("notice-banner");
  if (!banner) return;
  const revision = getWorkspaceRevision();
  let payload;
  try {
    payload = await fetchNotices();
  } catch (_) {
    // A failed read keeps the banner of its own visit. A failed read from a
    // visit the operator has left changes nothing.
    return;
  }
  // A reply from a visit the operator has left must not replace this one's banner or count.
  if (revision === getWorkspaceRevision()) renderNoticeBanner(payload);
}

function endedText(notice) {
  if (text(notice?.ended_label)) return text(notice.ended_label);
  if (notice?.status === "expired") return `expired at ${formatDateTime(text(notice.expires_at))}`;
  if (notice?.status === "closed") return "closed";
  return "";
}

function noticeRow(notice, canWrite, onClose) {
  const now = Date.now();
  const open = notice?.status === "open";
  const facts = [
    chip(text(notice.kind) || "notice", `notice-kind-${text(notice.kind)}`),
    chip(text(notice.scope) || "workspace"),
    chip(open ? "open" : text(notice.status)),
  ];
  if (notice?.linked_task?.id) facts.push(chip(`task ${text(notice.linked_task.id)}`));
  if (Number(notice?.reports?.count) > 0) facts.push(chip(`${notice.reports.count} reports`));
  const meta = open
    ? [ageLabel(notice, now), expiryLabel(notice, now), `opened ${formatClock(text(notice.opened_at))}`].filter(Boolean).join(" · ")
    : endedText(notice);
  const children = [
    el("div", { class: "notice-row-head" }, [
      el("span", { class: "notice-title", text: text(notice.title) }),
      ...facts,
    ]),
    el("div", { class: "notice-meta", text: meta }),
  ];
  if (text(notice.body)) {
    children.push(el("p", { class: "notice-body", text: text(notice.body) }));
  }
  if (open && canWrite && typeof onClose === "function") {
    const reason = el("input", { class: "notice-close-reason" });
    reason.type = "text";
    reason.placeholder = "Close reason";
    reason.maxLength = 240;
    reason.setAttribute("aria-label", `Close reason for ${text(notice.title)}`);
    const button = el("button", { class: "action notice-close-btn", text: "Close", type: "button" });
    button.addEventListener("click", () => onClose(notice, reason.value, button));
    children.push(el("div", { class: "notice-close-row" }, [reason, button]));
  }
  const row = el("article", { class: `notice-row notice-${text(notice.kind)}${open ? "" : " notice-ended"}` }, children);
  row.dataset.key = `notice-${text(notice.id)}`;
  row.dataset.hash = JSON.stringify([notice.revision, notice.status, notice.title, notice.body, endedText(notice)]);
  return row;
}

function postForm(canWrite, onPost) {
  const form = el("form", { class: "notice-post-form" });
  const title = el("input", { class: "notice-post-title" });
  title.required = true;
  title.maxLength = 120;
  title.placeholder = "Title";
  title.setAttribute("aria-label", "Notice title");
  const body = el("textarea", { class: "notice-post-body" });
  body.rows = 3;
  body.placeholder = "Body (plain text)";
  body.setAttribute("aria-label", "Notice body");
  const kind = el("select", { class: "notice-post-kind" }, [
    el("option", { text: "incident" }),
    el("option", { text: "maintenance" }),
    el("option", { text: "info" }),
  ]);
  kind.setAttribute("aria-label", "Notice kind");
  const scope = el("select", { class: "notice-post-scope" }, [
    el("option", { text: "workspace" }),
    el("option", { text: "global" }),
  ]);
  scope.setAttribute("aria-label", "Notice scope");
  const linked = el("input", { class: "notice-post-linked" });
  linked.placeholder = "Linked task (optional)";
  linked.setAttribute("aria-label", "Linked task");
  const expires = el("input", { class: "notice-post-expires" });
  expires.type = "number";
  expires.min = "1";
  expires.placeholder = "Expires in hours (optional)";
  expires.setAttribute("aria-label", "Expires in hours");
  const submit = el("button", { class: "action primary notice-post-submit", text: "Post notice", type: "submit" });
  const feedback = el("p", { class: "notice-feedback", role: "status" });
  if (!canWrite) {
    submit.disabled = true;
    feedback.textContent = "An operator session is required to post or close notices.";
  }
  form.append(
    el("h3", { class: "notice-form-title", text: "Post a notice" }),
    title, body,
    el("div", { class: "notice-form-row" }, [kind, scope, linked, expires, submit]),
    feedback,
  );
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const payload = {
      kind: kind.value,
      title: title.value,
      body: body.value || undefined,
      scope: scope.value,
      linked_task: linked.value.trim() || undefined,
      expires_in_hours: expires.value ? Number(expires.value) : undefined,
    };
    onPost(payload, feedback, form);
  });
  form.dataset.key = "notice-post-form";
  form.dataset.hash = canWrite ? "write" : "read-only";
  return form;
}

export async function fetchAndRenderNotices() {
  const revision = getWorkspaceRevision();
  await requestPanel(
    "notices-body",
    `notices:${revision}:${isAggregateView() ? "all" : "one"}`,
    () => fetchNotices(),
    (payload) => renderNotices(payload),
    "notices-count",
  );
}

function renderNotices(payload) {
  const body = $("notices-body");
  if (!body) return;
  renderNoticeBanner(payload);
  const open = Array.isArray(payload?.open) ? payload.open : [];
  const recent = Array.isArray(payload?.recent) ? payload.recent : [];
  const canWrite = payload?.can_write === true && !isAggregateView();
  const count = $("notices-count");
  if (count) count.textContent = String(open.length);
  const feedbackFor = () => body.querySelector(".notice-feedback");
  const refresh = () => fetchAndRenderNotices().catch((error) => {
    const feedback = feedbackFor();
    if (feedback) feedback.textContent = String(error?.message || error);
  });
  const onPost = async (input, feedback, form) => {
    try {
      await postJson("/api/notices", input);
      noticesRead = null;
      form.reset();
      await refresh();
    } catch (error) {
      feedback.textContent = String(error?.message || error);
    }
  };
  const onClose = async (notice, reason, button) => {
    if (!reason.trim()) return;
    button.disabled = true;
    try {
      await postJson(`/api/notices/${encodeURIComponent(text(notice.id))}/close`, { reason: reason.trim() });
      noticesRead = null;
      await refresh();
    } catch (error) {
      button.disabled = false;
      const feedback = feedbackFor();
      if (feedback) feedback.textContent = String(error?.message || error);
    }
  };
  const nodes = [postForm(canWrite, onPost)];
  nodes.push(el("h3", { class: "notice-section-title", text: "Open" }));
  if (!open.length) {
    const note = el("div", { class: "panel-placeholder", text: "No open notices in the shown workspaces." });
    note.dataset.key = "notices-open-empty";
    note.dataset.hash = "empty";
    nodes.push(note);
  } else {
    nodes.push(...open.map((notice) => noticeRow(notice, canWrite, onClose)));
  }
  nodes.push(el("h3", { class: "notice-section-title", text: "Closed or expired in the last 24 hours" }));
  if (!recent.length) {
    const note = el("div", { class: "panel-placeholder", text: "No notice closed or expired in the last 24 hours." });
    note.dataset.key = "notices-recent-empty";
    note.dataset.hash = "empty";
    nodes.push(note);
  } else {
    nodes.push(...recent.map((notice) => noticeRow(notice, false, null)));
  }
  for (const node of nodes) {
    if (!node.dataset.key) {
      node.dataset.key = `notices-${nodes.indexOf(node)}`;
      node.dataset.hash = node.textContent;
    }
  }
  syncNodes(body, nodes);
}
