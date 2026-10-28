// Locks held, in two views the card's tabs switch between. By task lists
// holding tasks, one row each. By file inverts that, since each task's detail
// already lists its own `context_files`: one row per locked path, and the
// tasks holding it in the row's disclosure. A task holds a path through
// its status (`in-progress` or `review`) and, while its delivery runs,
// through the run's reservation over the same files; both fold into one
// holder. A path more than one task holds is marked, and the filter shows
// only the locked paths a given task's `context_files` overlap.

import { bindTaskLink, detailsPanel, el, fetchJson, onWorkspaceChange, statusPill, syncNodes } from './common.js';
import { drainHolderCounts } from './operations.js';

const VIEW_STORAGE_KEY = "orbit.dashboard.locksView";

function storedViews() {
  try {
    const parsed = JSON.parse(localStorage.getItem(VIEW_STORAGE_KEY) || "{}");
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

function currentView() {
  const view = storedViews().tasks || sessionViews.tasks;
  return view === "task" || view === "file" ? view : "file";
}

function chooseView(view) {
  const views = { ...storedViews(), ["tasks"]: view };
  try {
    localStorage.setItem(VIEW_STORAGE_KEY, JSON.stringify(views));
  } catch {
    // Private windows refuse storage; the choice then lasts until reload.
    sessionViews["tasks"] = view;
  }
  if (lastPayload) paintLocks(lastPayload);
  else syncViewTabs(view);
}

// Fallback when storage is unavailable.
const sessionViews = {};

function syncViewTabs(view) {
  for (const tab of document.querySelectorAll("#locks-view [role=tab]")) {
    const on = tab.dataset.view === view;
    tab.classList.toggle("on", on);
    tab.setAttribute("aria-selected", String(on));
    tab.tabIndex = on ? 0 : -1;
  }
}

// The last locks read, repainted when the view or filter changes.
let lastPayload = null;
// The task the filter names: `{ id, state, anchors }`, `state` one of
// `loading`, `ready` or `missing`. `null` shows every locked path.
let filter = null;
let filterTimer = null;
let filterRequest = 0;

// The filesystem anchor a selector locks, read as admission reads it: a
// `symbol:` selector locks its whole file and `dir:` everything beneath it.
// `module:` and `command:` have no anchor and overlap only on equal text; a
// legacy raw path overlaps its descendants too.
export function lockAnchor(selector) {
  const text = String(selector || "").trim();
  const typed = /^(dir|file|symbol):(.*)$/.exec(text);
  if (typed) {
    const [, kind, rest] = typed;
    const hash = kind === "symbol" ? rest.indexOf("#") : -1;
    const path = trimPath(hash >= 0 ? rest.slice(0, hash) : rest);
    return {
      key: `${kind === "dir" ? "dir" : "file"}:${path}`,
      path,
      contains: kind === "dir",
      symbol: hash >= 0 ? rest.slice(hash + 1) : null,
    };
  }
  if (/^(module|command):/.test(text)) return { key: text, path: text, exact: true };
  const path = trimPath(text);
  return { key: `path:${path}`, path, contains: true };
}

function trimPath(path) {
  const trimmed = String(path).replace(/^\.\//, "").replace(/\/+$/, "");
  return trimmed || ".";
}

function isUnder(path, ancestor) {
  return ancestor === "." || path.startsWith(`${ancestor}/`);
}

export function anchorsOverlap(a, b) {
  if (a.exact || b.exact) return Boolean(a.exact && b.exact && a.key === b.key);
  return a.path === b.path
    || (a.contains && isUnder(b.path, a.path))
    || (b.contains && isUnder(a.path, b.path));
}

// Every (holder, selector) pair the payload reports. Task locks carry the
// task's status and title; reservations name their run, and one with no task
// is an operator's manual reservation.
function lockHolds(payload) {
  const byTask = Array.isArray(payload?.by_task) ? payload.by_task : [];
  const byReservation = Array.isArray(payload?.by_reservation) ? payload.by_reservation : [];
  const tasks = new Map(byTask.map(task => [String(task.id || ""), task]));
  const holds = [];
  for (const task of byTask) {
    const id = String(task.id || "");
    for (const selector of Array.isArray(task.context_files) ? task.context_files : []) {
      holds.push({ owner: id, taskId: id, via: "lock", selector: String(selector), run: task.job_run_id || null });
    }
  }
  for (const reservation of byReservation) {
    const files = Array.isArray(reservation.files) ? reservation.files : [];
    const taskIds = Array.isArray(reservation.task_ids) ? reservation.task_ids.map(String) : [];
    const owners = taskIds.length ? taskIds : [null];
    for (const taskId of owners) {
      for (const selector of files) {
        holds.push({
          owner: taskId || `reservation:${reservation.reservation_id}`,
          taskId,
          via: "reservation",
          selector: String(selector),
          run: reservation.owner_run_id || null,
          expires: reservation.expires_at || null,
          actor: reservation.actor || null,
        });
      }
    }
  }
  return { holds, tasks };
}

// One row per locked anchor, holding every hold whose selector overlaps it,
// grouped by owner. Shared rows sort first, then by path.
export function lockRows(payload) {
  const { holds, tasks } = lockHolds(payload);
  const anchored = holds.map(hold => ({ ...hold, anchor: lockAnchor(hold.selector) }));
  const rows = new Map();
  for (const hold of anchored) {
    if (!rows.has(hold.anchor.key)) rows.set(hold.anchor.key, { anchor: hold.anchor, owners: new Map() });
  }
  for (const row of rows.values()) {
    for (const hold of anchored) {
      if (!anchorsOverlap(row.anchor, hold.anchor)) continue;
      let owner = row.owners.get(hold.owner);
      if (!owner) {
        owner = { owner: hold.owner, taskId: hold.taskId, task: tasks.get(hold.taskId) || null, via: new Set(), selectors: new Set(), runs: new Set(), expires: null, actor: null };
        row.owners.set(hold.owner, owner);
      }
      owner.via.add(hold.via);
      if (hold.anchor.key !== row.anchor.key || hold.anchor.symbol) owner.selectors.add(hold.selector);
      if (hold.run) owner.runs.add(hold.run);
      owner.expires = owner.expires || hold.expires || null;
      owner.actor = owner.actor || hold.actor || null;
    }
  }
  return [...rows.values()].sort((a, b) =>
    (b.owners.size > 1) - (a.owners.size > 1) || a.anchor.path.localeCompare(b.anchor.path));
}

// `jrun-20260925-0324-c9` reads as `…0324-c9`: the date is today's more often
// than not, and the tail is what tells two runs apart.
function shortRunId(runId) {
  const text = String(runId || "");
  const match = /^jrun-\d{8}-(.+)$/.exec(text);
  return match ? `…${match[1]}` : text;
}

function displayPath(anchor) {
  return anchor.contains && anchor.key.startsWith("dir:") ? `${anchor.path}/` : anchor.path;
}

function holdLabel(owner) {
  if (!owner.taskId) return "Manual reservation";
  if (owner.via.has("lock") && owner.via.has("reservation")) return "Context lock + run reservation";
  return owner.via.has("lock") ? "Context lock" : "Run reservation";
}

function holderRow(row, owner, blocking) {
  const task = owner.task;
  const title = typeof task?.title === "string" ? task.title.trim() : "";
  const blocks = owner.taskId ? blocking.get(owner.taskId) || 0 : 0;
  let id;
  if (owner.taskId) {
    id = el("a", { class: "lock-holder-id mono", text: owner.taskId, title: title ? `Open ${owner.taskId}: ${title}` : `Open ${owner.taskId}` });
    // Locks live on Drain too, so opening a task must navigate to Tasks.
    bindTaskLink(id, owner.taskId);
  } else {
    id = el("span", { class: "lock-holder-id mono", text: owner.actor || "operator" });
  }
  const runs = [...owner.runs];
  const expiry = owner.expires ? `; reservation expires ${owner.expires}` : "";
  // The symbols it names in this row's file, else the wider or narrower
  // selector it holds the row through.
  const details = [...owner.selectors].map(selector => {
    const anchor = lockAnchor(selector);
    return anchor.symbol && anchor.key === row.anchor.key ? `#${anchor.symbol}` : `via ${selector}`;
  });
  const isFilterTask = filter && owner.taskId === filter.id;
  return el("li", { class: `lock-holder${isFilterTask ? " lock-holder-self" : ""}` }, [
    el("div", { class: "lock-holder-line" }, [
      id,
      task ? statusPill(task.status || "unknown") : null,
      isFilterTask ? el("span", { class: "lock-holder-tag", text: "this task" }) : null,
      blocks > 0 ? el("span", {
        class: "lock-blocks",
        text: `blocks ${blocks}`,
        title: `${blocks} waiting ${blocks === 1 ? "task waits" : "tasks wait"} on ${owner.taskId}`,
      }) : null,
      title ? el("span", { class: "lock-holder-title", text: title, title }) : null,
    ]),
    el("div", { class: "lock-holder-meta" }, [
      el("span", { text: holdLabel(owner), title: `${holdLabel(owner)}${expiry}` }),
      details.length ? el("span", { class: "mono", text: details.join(", ") }) : null,
      ...runs.map(run => el("span", { class: "lock-job mono", text: shortRunId(run), title: `job_run=${run}` })),
    ]),
  ]);
}

function lockRow(row, blocking) {
  const owners = [...row.owners.values()];
  const shared = owners.length > 1;
  const key = `lock-file-${row.anchor.key}`;
  const path = displayPath(row.anchor);
  const group = el("div", { class: `lock-group${shared ? " lock-shared" : ""}` });
  group.dataset.key = key;
  group.dataset.hash = JSON.stringify([
    path,
    owners.map(owner => [owner.owner, [...owner.via], [...owner.selectors], [...owner.runs], owner.task?.status, owner.task?.title, blocking.get(owner.taskId) || 0]),
    filter?.id || null,
  ]);
  const details = detailsPanel(key, { class: "lock-details" });
  const ownerSummary = shared
    ? el("span", { class: "lock-owner-count", text: `${owners.length} tasks`, title: `Held by ${owners.length} tasks` })
    : el("span", { class: "lock-owner-one mono", text: owners[0].taskId || "manual" });
  details.appendChild(el("summary", { class: "lock-header" }, [
    // The path clips from its start (`direction: rtl`); the trailing mark
    // keeps a directory's slash at the end.
    el("span", { class: "lock-path mono", text: `${path}\u200e`, title: path }),
    ownerSummary,
  ]));
  details.appendChild(el("ul", { class: "lock-holder-list" }, owners.map(owner => holderRow(row, owner, blocking))));
  group.appendChild(details);
  return group;
}

function filterNote(rows, total) {
  if (!filter) return null;
  if (filter.state === "loading") return `Reading ${filter.id}…`;
  if (filter.state === "missing") return `No task ${filter.id} in this workspace.`;
  if (!filter.anchors.length) return `${filter.id} declares no context files.`;
  if (!rows.length) return `None of the ${total} locked ${total === 1 ? "path overlaps" : "paths overlap"} ${filter.id}'s context files.`;
  const others = rows.filter(row => [...row.owners.values()].some(owner => owner.taskId !== filter.id)).length;
  return `${rows.length} of ${total} locked ${total === 1 ? "path overlaps" : "paths overlap"} ${filter.id}; ${others} held by another task.`;
}

function paintByFile(payload) {
  const body = document.getElementById("locks-body");
  const count = document.getElementById("locks-count");
  if (!body || !count) return;
  const allRows = lockRows(payload);
  const shared = allRows.filter(row => row.owners.size > 1).length;
  const totalTasks = Number.isFinite(Number(payload?.total_tasks))
    ? Number(payload.total_tasks)
    : new Set(allRows.flatMap(row => [...row.owners.values()].map(owner => owner.owner))).size;
  count.textContent = [
    `${allRows.length} ${allRows.length === 1 ? "file" : "files"}`,
    `${totalTasks} ${totalTasks === 1 ? "task" : "tasks"}`,
    shared ? `${shared} shared` : null,
  ].filter(Boolean).join(" · ");

  const rows = filter?.state === "ready"
    ? allRows.filter(row => filter.anchors.some(anchor => anchorsOverlap(anchor, row.anchor)))
    : filter ? [] : allRows;
  const note = document.getElementById("locks-filter-note");
  if (note) {
    const text = filterNote(rows, allRows.length);
    note.hidden = !text;
    note.textContent = text || "";
  }
  if (!rows.length) {
    if (filter) {
      syncNodes(body, []);
      return;
    }
    const empty = el("div", { class: "locks-empty", text: "No files currently locked." });
    empty.dataset.key = "locks-empty";
    empty.dataset.hash = "locks-empty";
    syncNodes(body, [empty]);
    return;
  }
  const blocking = drainHolderCounts();
  syncNodes(body, rows.map(row => lockRow(row, blocking)));
}

// The filter belongs to the file view.
export function paintLocks(payload) {
  lastPayload = payload;
  const view = currentView();
  syncViewTabs(view);
  const filterBox = document.querySelector("#locks-panel .locks-filter");
  if (filterBox) filterBox.hidden = view !== "file";
  if (view === "file") paintByFile(payload);
  else paintByTask(payload);
}

function paintByTask(payload) {
  const body = document.getElementById("locks-body");
  const count = document.getElementById("locks-count");
  if (!body || !count) return;
  const byTask = Array.isArray(payload && payload.by_task) ? payload.by_task : [];
  const byReservation = Array.isArray(payload && payload.by_reservation) ? payload.by_reservation : [];
  const totalLocked = Number.isFinite(Number(payload && payload.total_locked))
    ? Number(payload.total_locked)
    : 0;
  const totalTasks = Number.isFinite(Number(payload && payload.total_tasks))
    ? Number(payload.total_tasks)
    : byTask.length;
  count.textContent = `${totalLocked} ${totalLocked === 1 ? "file" : "files"} · ${totalTasks} ${totalTasks === 1 ? "task" : "tasks"}`;

  if (byTask.length === 0 && byReservation.length === 0) {
    const empty = el("div", { class: "locks-empty", text: "No files currently locked." });
    empty.dataset.key = "locks-empty";
    empty.dataset.hash = "locks-empty";
    syncNodes(body, [empty]);
    return;
  }

  // One row per task: who holds the locks, its status, how many paths and how
  // many waiting tasks it holds back, then its title and run. The paths fold
  // away and stay open across refreshes once opened, so a dozen tasks never
  // bury the card in file paths. The task link sits outside the disclosure
  // (a link cannot live in a summary); an invisible copy of the id keeps its
  // place in the row, and the link is laid over it.
  const blocking = drainHolderCounts();
  const nodes = byTask.map((task) => {
    const taskId = String(task.id || "");
    const files = Array.isArray(task.context_files) ? task.context_files : [];
    const blocks = blocking.get(taskId) || 0;
    const title = typeof task.title === "string" ? task.title.trim() : "";
    const group = el("div", { class: "lock-task-group" });
    group.dataset.key = `lock-task-${taskId}`;
    group.dataset.hash = JSON.stringify([task, blocks]);
    const details = detailsPanel(`lock-task-${taskId}`, { class: "lock-task-details" });

    const idButton = el("a", {
      class: "lock-task-id mono",
      text: taskId,
      title: title ? `Open ${taskId}: ${title}` : `Open ${taskId} in the task list`,
    });
    // Locks live on Drain, so opening a task must navigate to Tasks too.
    bindTaskLink(idButton, taskId);

    const header = el("summary", { class: "lock-task-header" }, [
      el("span", { class: "lock-task-id-space mono", text: taskId, "aria-hidden": "true" }),
      statusPill(task.status || "unknown"),
      el("span", { class: "lock-count", text: `${files.length} ${files.length === 1 ? "file" : "files"}` }),
      blocks > 0 ? el("span", {
        class: "lock-blocks",
        text: `blocks ${blocks}`,
        title: `${blocks} waiting ${blocks === 1 ? "task waits" : "tasks wait"} on ${taskId}`,
      }) : null,
    ]);
    const job = task.job_run_id ? el("span", {
      class: "lock-job mono",
      text: shortRunId(task.job_run_id),
      title: `job_run=${task.job_run_id}`,
    }) : null;
    if (title || job) {
      header.appendChild(el("span", { class: "lock-task-meta" }, [
        title ? el("span", { class: "lock-task-title", text: title, title }) : null,
        job,
      ]));
    }
    details.appendChild(header);

    const list = el("div", { class: "lock-file-list" });
    for (const path of files) {
      list.appendChild(el("div", {
        class: "lock-file-row mono",
        text: String(path),
        title: String(path),
      }));
    }
    details.appendChild(list);
    group.appendChild(details);
    group.appendChild(idButton);
    return group;
  });
  for (const reservation of byReservation) {
    const key = `lock-reservation-${reservation.reservation_id}`;
    const files = Array.isArray(reservation.files) ? reservation.files : [];
    const taskIds = Array.isArray(reservation.task_ids) ? reservation.task_ids : [];
    const group = el("div", { class: "lock-task-group lock-reservation-group" });
    group.dataset.key = key;
    group.dataset.hash = JSON.stringify(reservation);
    const details = detailsPanel(key, { class: "lock-task-details" });
    const holders = el("span", { class: "lock-reservation-holders" });
    for (const taskId of taskIds) {
      const link = el("a", { class: "mono", text: String(taskId) });
      bindTaskLink(link, String(taskId));
      holders.appendChild(link);
    }
    if (!taskIds.length) holders.appendChild(el("span", { text: "Manual reservation" }));
    const header = el("summary", { class: "lock-task-header" }, [
      el("span", {
        class: "lock-reservation-label",
        text: "Reservation",
        title: reservation.expires_at ? `Expires ${reservation.expires_at}` : "Reserved lock surface",
      }),
      el("span", { class: "lock-count", text: `${files.length} ${files.length === 1 ? "file" : "files"}` }),
      el("span", { class: "lock-task-meta" }, [
        holders,
        reservation.owner_run_id ? el("span", {
          class: "lock-job mono",
          text: shortRunId(reservation.owner_run_id),
          title: `job_run=${reservation.owner_run_id}`,
        }) : null,
      ]),
    ]);
    details.appendChild(header);
    const list = el("div", { class: "lock-file-list" });
    for (const file of files) list.appendChild(el("div", { class: "lock-file-row mono", text: String(file), title: String(file) }));
    details.appendChild(list);
    group.appendChild(details);
    nodes.push(group);
  }
  syncNodes(body, nodes);
}

async function applyFilter(raw) {
  const id = String(raw || "").trim().toUpperCase();
  const request = ++filterRequest;
  if (!id) {
    filter = null;
  } else {
    filter = { id, state: "loading", anchors: [] };
    if (lastPayload) paintLocks(lastPayload);
    try {
      const task = await fetchJson(`/api/tasks/${encodeURIComponent(id)}`);
      if (request !== filterRequest) return;
      const selectors = Array.isArray(task?.context_files) ? task.context_files : [];
      filter = { id: String(task?.id || id), state: "ready", anchors: selectors.map(lockAnchor) };
    } catch {
      if (request !== filterRequest) return;
      filter = { id, state: "missing", anchors: [] };
    }
  }
  if (lastPayload) paintLocks(lastPayload);
}

function wireViewTabs() {
  const tabs = document.getElementById("locks-view");
  if (!tabs) return;
  syncViewTabs(currentView());
  for (const tab of tabs.querySelectorAll("[role=tab]")) {
    tab.addEventListener("click", () => chooseView(tab.dataset.view));
  }
  // Arrow keys, Home and End move between the tabs, as a tablist promises.
  tabs.addEventListener("keydown", event => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const views = ["task", "file"];
    const index = views.indexOf(currentView());
    const next = event.key === "Home" ? 0
      : event.key === "End" ? views.length - 1
        : (index + (event.key === "ArrowLeft" ? -1 : 1) + views.length) % views.length;
    chooseView(views[next]);
    tabs.querySelector(`[data-view="${views[next]}"]`)?.focus();
  });
}

export function initLocks() {
  wireViewTabs();
  const input = document.getElementById("locks-filter");
  if (!input) return;
  input.addEventListener("input", () => {
    clearTimeout(filterTimer);
    filterTimer = setTimeout(() => applyFilter(input.value), 250);
  });
  input.addEventListener("keydown", event => {
    if (event.key === "Escape" && input.value) {
      event.preventDefault();
      input.value = "";
      clearTimeout(filterTimer);
      applyFilter("");
    }
  });
  // Task IDs belong to one workspace; a switch starts the card unfiltered.
  onWorkspaceChange(() => {
    input.value = "";
    clearTimeout(filterTimer);
    filterRequest += 1;
    filter = null;
  });
}
