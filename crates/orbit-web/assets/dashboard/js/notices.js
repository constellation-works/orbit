// Notices view and the fleet-notice banner.
//
// Reads `GET /api/notices` (open notices plus the last 24h of closed and
// expired ones) and writes through `POST /api/notices` and
// `POST /api/notices/<id>/close`, the dashboard routes over the same core
// post/close as `orbit.notice.post` / `orbit.notice.close`. Every title,
// body, reason and label is inserted with `el()` / textContent, never as
// HTML, so a notice that contains markup reads as text.
//
// The page is one list: open notices first, then recently closed ones under
// a "Recently closed" heading. A row scans as severity, title, scope and age
// or expiry; its body opens from the row, and Close asks for its reason only
// once chosen. Posting is a modal dialog with title, severity and body up front.

import { bindTaskLink, captureWorkspaceVisit, el, enableRovingRows, fetchJson, formatDateTime, getHostRevision, getWorkspaceRevision, infoHint, isAggregateView, makeRowDisclosure, onWorkspaceChange, relativeTime, requestJson, requestPanel, syncNodes } from './common.js';

const $ = (id) => document.getElementById(id);
const text = (value) => (typeof value === "string" ? value : "");

function noticesPath() {
  return isAggregateView() ? "/api/notices?workspace=all" : "/api/notices";
}

// One /api/notices read serves the banner, the rail count and the Notices
// panel, which all render from the same payload. Concurrent callers always
// join the read in flight. Scheduled polls may also reuse a completed read
// for half the 30s poll interval, so the banner and panel share one request
// within the ten-request poll budget. Explicit refreshes always start a new
// read. A failed read is never reused, and a host or workspace switch changes
// the key, so no payload crosses scopes. Post/close mutations clear the read.
const NOTICES_READ_SHARE_MS = 15_000;
let noticesRead = null;

function fetchNotices({ shareRecent = false, read: request = null } = {}) {
  const key = `${getHostRevision()}:${getWorkspaceRevision()}:${noticesPath()}`;
  if (!request && noticesRead?.key === key
    && (noticesRead.pending || (shareRecent && Date.now() - noticesRead.at < NOTICES_READ_SHARE_MS))) {
    return noticesRead.promise;
  }
  const read = { key, pending: true, at: 0, promise: null };
  read.promise = (request || fetchJson(noticesPath())).then(
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

export async function fetchAndRenderNoticeBanner({ shareRecent = false, read = null } = {}) {
  const banner = $("notice-banner");
  if (!banner) return;
  const revision = getWorkspaceRevision();
  let payload;
  try {
    payload = await fetchNotices({ shareRecent, read });
  } catch (_) {
    // A failed read keeps the banner of its own visit. A failed read from a
    // visit the operator has left changes nothing.
    return;
  }
  // A reply from a visit the operator has left must not replace this one's banner or count.
  if (revision === getWorkspaceRevision()) renderNoticeBanner(payload);
}

const OPERATOR_REQUIRED = "An operator session is required to post or close notices.";
const WORKSPACE_REQUIRED = "Select one workspace to post or close notices.";
const PAGE_HINT = `Fleet notices in the shown workspaces: open ones, and those closed or expired in the last 24 hours. ${OPERATOR_REQUIRED}`;
const SEVERITY_ORDER = ["incident", "maintenance", "info"];

// What the operator has done on this page. Rows are rebuilt from the last
// payload, so opening a row or choosing Close needs no read and a refresh
// keeps them, a half-typed close reason included. A scope switch starts over.
let lastPayload = null;
const openedNotices = new Set();
// The row asking for its reason: { id, draft, error, pending, visit }. The object
// is the close session. A write's completion touches the page only while the
// same object is still `closing` and its visit is current, so a reply for a
// session that was replaced or left (a workspace switch, A -> B -> A) can never
// clear, relabel or refocus the newer session's reason.
let closing = null;

onWorkspaceChange(() => {
  lastPayload = null;
  openedNotices.clear();
  closing = null;
  const dialog = $("notices-post-dialog");
  if (dialog?.open) dialog.close();
});

const noticesOf = (payload, list) => (Array.isArray(payload?.[list]) ? payload[list] : []);
const canWriteNotices = (payload) => payload?.can_write === true && !isAggregateView();

// Incidents first in either list; within a severity the server's order stands.
function severityRank(notice) {
  const rank = SEVERITY_ORDER.indexOf(text(notice?.kind));
  return rank < 0 ? SEVERITY_ORDER.length : rank;
}

function bySeverity(notices) {
  return [...notices].sort((a, b) => severityRank(a) - severityRank(b));
}

// The row's one time: how soon an open notice lapses when that is within a day,
// otherwise how old it is; for an ended notice, when it ended.
function rowTime(notice, now) {
  if (notice?.status !== "open") {
    const ended = relativeTime(text(notice?.ended_at), now);
    return `${notice?.status === "expired" ? "expired" : "closed"}${ended ? ` ${ended}` : ""}`;
  }
  const expires = Date.parse(text(notice?.expires_at));
  if (Number.isFinite(expires) && expires - now < 86_400_000) {
    const lapse = relativeTime(text(notice.expires_at), now);
    if (lapse) return `expires ${lapse}`;
  }
  return relativeTime(text(notice?.opened_at), now) || "";
}

function endedText(notice) {
  if (notice?.status === "expired") return `expired at ${formatDateTime(text(notice.expires_at))}`;
  if (text(notice?.ended_label)) return text(notice.ended_label);
  return notice?.status === "closed" ? "closed" : "";
}

function factList(notice) {
  const author = text(notice?.author?.class);
  const facts = [`Opened ${formatDateTime(text(notice?.opened_at))}${author ? ` by ${author}` : ""}`];
  if (notice?.status === "open") facts.push(`Expires ${formatDateTime(text(notice?.expires_at))}`);
  // The first report is the post itself.
  if (Number(notice?.reports?.count) > 1) facts.push(`${notice.reports.count} reports`);
  const list = el("div", { class: "notice-facts" }, facts.map((fact) => el("span", { text: fact })));
  const linked = text(notice?.linked_task?.id);
  if (linked) {
    const link = el("a", { text: `Task ${linked}` });
    bindTaskLink(link, linked);
    list.append(link);
  }
  return list;
}

function repaintNoticeList() {
  if (lastPayload) paintNoticeList(lastPayload);
}

function focusRowControl(id, selector) {
  for (const row of document.querySelectorAll("#notices-body .notice-row")) {
    if (row.dataset.key === `notice-${id}`) row.querySelector(selector)?.focus();
  }
}

// While a close is in flight the row waits for its answer: nothing here may
// replace `closing`, or a refusal would have no row to report on.
function toggleNotice(id) {
  if (closing?.pending) return;
  if (closing?.id === id) {
    closing = null;
    openedNotices.delete(id);
  } else if (!openedNotices.delete(id)) {
    openedNotices.add(id);
  }
  repaintNoticeList();
}

function chooseClose(id) {
  if (closing?.pending) return;
  closing = { id, draft: "", error: "", pending: false, visit: captureWorkspaceVisit() };
  repaintNoticeList();
  focusRowControl(id, ".notice-close-reason");
  // Focus alone would stop at the input, leaving its actions below the fold.
  document.activeElement?.closest?.(".notice-close-form")?.scrollIntoView({ block: "nearest" });
}

function cancelClose() {
  if (closing?.pending) return;
  const id = closing?.id;
  closing = null;
  repaintNoticeList();
  focusRowControl(id, ".notice-close-open");
}

async function closeChosenNotice(session) {
  const id = session.id;
  const reason = session.draft.trim();
  if (!reason) {
    session.error = "Enter a reason.";
    repaintNoticeList();
    focusRowControl(id, ".notice-close-reason");
    return;
  }
  session.pending = true;
  session.error = "";
  repaintNoticeList();
  const isCurrentSession = () => closing === session && session.visit.isCurrent();
  try {
    await requestJson(`/api/notices/${encodeURIComponent(id)}/close`, "POST", { reason }, session.visit);
  } catch (error) {
    if (!isCurrentSession()) return; // The scope or session changed meanwhile.
    session.pending = false;
    session.error = String(error?.message || error);
    repaintNoticeList();
    focusRowControl(id, ".notice-close-reason");
    return;
  }
  // The write stands whichever session asked, so the shared read is dropped. A
  // reply for a session that has been replaced or left leaves the page alone: the
  // newer session owns the draft, the open rows and the focus.
  noticesRead = null;
  if (!isCurrentSession()) {
    if (session.visit.isCurrent()) await refreshNoticesAfterWrite();
    return;
  }
  // Only replace focus lost with this form. A control the operator has already
  // moved to owns its focus, and any focus chosen during the refresh does too.
  const focused = document.activeElement;
  let restoreCloseFocus = focused === document.body
    || focused?.closest(".notice-close-form")?.closest(".notice-row")?.dataset.key === `notice-${id}`;
  const supersedeCloseFocus = () => { restoreCloseFocus = false; };
  document.addEventListener("focusin", supersedeCloseFocus);
  closing = null;
  openedNotices.delete(id);
  try {
    await refreshNoticesAfterWrite();
  } finally {
    document.removeEventListener("focusin", supersedeCloseFocus);
  }
  if (!session.visit.isCurrent() || closing || !restoreCloseFocus) return;
  // The closed row took the operator's focus with it.
  document.querySelector("#notices-body .notice-row-toggle")?.focus();
}

// A failed re-read after a successful write leaves its error in the panel's
// status note (`requestPanel`); the write itself stands.
function refreshNoticesAfterWrite() {
  return fetchAndRenderNotices().catch((error) => console.error(error));
}

function closeReasonForm(notice) {
  const id = text(notice.id);
  const session = closing;
  const reason = el("input", { class: "notice-close-reason", type: "text" });
  reason.placeholder = "Why is this over?";
  reason.maxLength = 240;
  reason.required = true;
  reason.value = closing.draft;
  reason.setAttribute("aria-label", `Close reason for ${text(notice.title)}`);
  reason.addEventListener("input", () => { session.draft = reason.value; });
  const submit = el("button", { class: "action save notice-close-submit", type: "submit", text: "Close notice" });
  submit.disabled = closing.pending;
  const cancel = el("button", { class: "action cancel notice-close-cancel", type: "button", text: "Cancel" });
  cancel.disabled = closing.pending;
  cancel.addEventListener("click", cancelClose);
  const form = el("form", { class: "notice-close-form" }, [
    reason,
    el("span", { class: "notice-close-actions" }, [submit, cancel]),
    closing.error ? el("p", { class: "notice-feedback", role: "alert", text: closing.error }) : null,
  ]);
  form.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      cancelClose();
    }
  });
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (closing === session && !session.pending) closeChosenNotice(session);
  });
  return form;
}

function noticeDetail(notice, detailId, askingReason) {
  const ended = notice?.status === "open" ? "" : endedText(notice);
  return el("div", { id: detailId, class: "notice-detail" }, [
    ended ? el("p", { class: "notice-ended-text", text: ended }) : null,
    text(notice.body) ? el("p", { class: "notice-body", text: text(notice.body) }) : null,
    factList(notice),
    askingReason ? closeReasonForm(notice) : null,
  ]);
}

function noticeRow(notice, canWrite, now) {
  const id = text(notice.id);
  const kind = text(notice.kind);
  const isOpen = notice?.status === "open";
  const askingReason = canWrite && isOpen && closing?.id === id;
  const expanded = askingReason || openedNotices.has(id);
  const detailId = `notice-detail-${id}`;
  const when = rowTime(notice, now);
  const toggle = el("button", { class: "notice-row-toggle" }, [
    el("span", { class: "notice-title", text: text(notice.title) }),
  ]);
  const main = el("div", { class: "notice-row-main" }, [
    el("span", { class: `notice-severity notice-severity-${kind}`, text: kind || "notice" }),
    toggle,
    el("span", { class: "notice-scope", text: text(notice.scope) || "workspace" }),
    el("span", { class: "notice-when", text: when }),
  ]);
  makeRowDisclosure(main, toggle, {
    expanded,
    controls: expanded ? detailId : null,
    onToggle: () => toggleNotice(id),
  });
  if (canWrite && isOpen && !askingReason) {
    const close = el("button", { class: "notice-close-open", type: "button", text: "Close" });
    close.setAttribute("aria-label", `Close ${text(notice.title)}`);
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      chooseClose(id);
    });
    main.append(close);
  }
  main.append(el("span", { class: "notice-chevron", "aria-hidden": "true" }));
  const row = el("article", { class: `notice-row notice-${kind}${isOpen ? "" : " notice-ended"}${expanded ? " is-open" : ""}` }, [
    main,
    expanded ? noticeDetail(notice, detailId, askingReason) : null,
  ]);
  row.dataset.key = `notice-${id}`;
  row.dataset.hash = JSON.stringify([
    notice.revision, notice.status, notice.title, notice.body, notice.scope, kind, when, endedText(notice),
    canWrite, expanded, askingReason, askingReason ? [closing.error, closing.pending] : null,
  ]);
  return row;
}

function paintNoticeList(payload) {
  const body = $("notices-body");
  if (!body) return;
  const canWrite = canWriteNotices(payload);
  const now = Date.now();
  const open = bySeverity(noticesOf(payload, "open"));
  const recent = bySeverity(noticesOf(payload, "recent"));
  const nodes = open.map((notice) => noticeRow(notice, canWrite, now));
  if (!open.length) {
    if (!recent.length) {
      const note = el("div", { class: "panel-placeholder", text: "No notices." });
      note.dataset.key = "notices-empty";
      note.dataset.hash = "empty";
      nodes.push(note);
    } else {
      const note = el("div", { class: "notice-open-empty", text: "No open notices." });
      note.dataset.key = "notices-empty-open";
      note.dataset.hash = "empty-open";
      nodes.push(note);
    }
  }
  if (recent.length) {
    const heading = el("h3", {
      class: `notice-group-heading${open.length ? "" : " notice-group-first"}`,
      text: `Recently closed (${recent.length})`,
    });
    heading.dataset.key = "notices-recent-heading";
    heading.dataset.hash = `recent-${recent.length}`;
    nodes.push(heading, ...recent.map((notice) => noticeRow(notice, canWrite, now)));
  }
  syncNodes(body, nodes);
  enableRovingRows(body);
}

function postOptionsSummary() {
  const hours = $("notices-post-expires").value;
  const linked = $("notices-post-linked").value.trim();
  return [
    $("notices-post-scope").value === "global" ? "Global" : "This workspace",
    linked ? `task ${linked}` : "no task",
    hours ? `expires in ${hours}h` : "default expiry",
  ].join(" · ");
}

// Each time the dialog opens it starts a post session: { visit }. A reply
// touches the dialog only while that same session is still open in the visit
// that sent it. Dismissing, Cancel, reopening or a scope switch ends the session,
// so a late reply can never reset, close or relabel a newer draft. One post per
// visit is in flight at a time; its Submit stays disabled until the reply.
function wirePostDialog() {
  const dialog = $("notices-post-dialog");
  const form = $("notices-post-form");
  if (!dialog || !form) return;
  const feedback = $("notices-post-feedback");
  const syncSummary = () => { $("notices-post-summary").textContent = postOptionsSummary(); };
  let session = null;
  let inflight = null; // { visit } of the post awaiting its reply.
  const postBusy = () => !!inflight && inflight.visit.isCurrent();
  const syncSubmit = () => { $("notices-post-submit").disabled = postBusy(); };
  const readDraft = () => ({
    kind: form.querySelector('input[name="notices-post-kind"]:checked')?.value,
    title: $("notices-post-title").value,
    body: $("notices-post-body").value || undefined,
    scope: $("notices-post-scope").value,
    linked_task: $("notices-post-linked").value.trim() || undefined,
    expires_in_hours: $("notices-post-expires").value ? Number($("notices-post-expires").value) : undefined,
  });
  const resetDraft = () => {
    form.reset();
    $("notices-post-options").open = false;
  };
  $("notices-post-toggle")?.addEventListener("click", () => {
    if (!dialog.open) {
      session = { visit: captureWorkspaceVisit() };
      feedback.textContent = "";
      syncSummary();
      syncSubmit();
      dialog.showModal();
    }
    $("notices-post-title").focus();
  });
  // Every way the dialog closes ends its session; a reopen has already replaced it.
  dialog.addEventListener("close", () => { if (!dialog.open) session = null; });
  form.addEventListener("input", syncSummary);
  // A rejected option folded away would otherwise stop the post with no message.
  form.addEventListener("invalid", () => { $("notices-post-options").open = true; }, true);
  // Cancel discards the draft; Escape only dismisses it.
  $("notices-post-cancel").addEventListener("click", () => {
    session = null;
    resetDraft();
    dialog.close();
  });
  window.addEventListener("hashchange", () => {
    if (dialog.open && !window.location.hash.startsWith("#notices")) dialog.close();
  });
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!session || !session.visit.isCurrent() || postBusy()) return;
    const mine = session;
    const input = readDraft();
    const sent = JSON.stringify(input);
    const mineInflight = { visit: mine.visit };
    inflight = mineInflight;
    syncSubmit();
    feedback.textContent = "";
    const isCurrentSession = () => session === mine && mine.visit.isCurrent();
    try {
      await requestJson("/api/notices", "POST", input, mine.visit);
    } catch (error) {
      if (isCurrentSession()) feedback.textContent = String(error?.message || error);
      return;
    } finally {
      if (inflight === mineInflight) inflight = null;
      if (session) syncSubmit();
    }
    // The post stands whichever session sent it, so the shared read is dropped.
    noticesRead = null;
    if (isCurrentSession()) {
      resetDraft();
      dialog.close();
      await refreshNoticesAfterWrite();
      return;
    }
    // A dismissed or replaced session leaves the page alone: the sent draft is
    // cleared only when no dialog is open and nothing was typed over it since.
    if (!session && JSON.stringify(readDraft()) === sent) resetDraft();
    if (mine.visit.isCurrent()) await refreshNoticesAfterWrite();
  });
}

// The heading's info hint, the Post control and the dialog are static page
// furniture: wired once, before the first read so they answer even when that
// read fails. Later renders only update the Post control's state.
let noticesPageWired = false;

function wireNoticesPage() {
  if (noticesPageWired) return;
  noticesPageWired = true;
  $("notices-hint-slot")?.append(infoHint(PAGE_HINT));
  const button = el("button", { class: "action save notice-post-toggle", text: "Post", type: "button", id: "notices-post-toggle" });
  button.setAttribute("aria-haspopup", "dialog");
  button.disabled = true;
  $("notices-post-slot")?.append(button);
  wirePostDialog();
}

function syncPostControl(payload, canWrite) {
  const slot = $("notices-post-slot");
  const button = $("notices-post-toggle");
  if (!slot || !button) return;
  button.disabled = !canWrite;
  const refusal = payload?.can_write === true ? WORKSPACE_REQUIRED : OPERATOR_REQUIRED;
  const hint = slot.querySelector(".info-hint");
  if (canWrite) hint?.remove();
  else if (!hint) slot.append(infoHint(refusal));
  else if (hint.textContent !== refusal) hint.replaceWith(infoHint(refusal));
  const dialog = $("notices-post-dialog");
  if (!canWrite && dialog?.open) dialog.close();
}

export async function fetchAndRenderNotices({ shareRecent = false } = {}) {
  wireNoticesPage();
  const revision = getWorkspaceRevision();
  await requestPanel(
    "notices-body",
    `notices:${revision}:${isAggregateView() ? "all" : "one"}`,
    () => fetchNotices({ shareRecent }),
    (payload) => renderNotices(payload),
  );
}

function renderNotices(payload) {
  if (!$("notices-body")) return;
  lastPayload = payload;
  renderNoticeBanner(payload);
  const open = noticesOf(payload, "open");
  syncPostControl(payload, canWriteNotices(payload));
  // A row asking for its reason that has since ended has nothing left to close.
  if (closing && !closing.pending && !open.some((notice) => text(notice.id) === closing.id)) closing = null;
  paintNoticeList(payload);
}
