// Leases tab: the workspace's path leases per path, with their holders and
// queued requests, and the recent landing notices (path-leases design §6).
//
// The view reads only Orbit Web's lease routes, `GET /api/leases` and
// `GET /api/leases/notices`, which the dashboard serves to any local reader:
// no operator session and no lease token. Requesting, renewing and releasing
// stay on `orbit lease` and the lease tools. Every node is built with `el()`.

import { el, fetchJson, formatClock, formatDateTime, getWorkspaceRevision, isAggregateView, isPanelPoll, renderPanelPlaceholder, requestPanel, syncNodes } from './common.js';

const $ = (id) => document.getElementById(id);

// Notices come oldest first after a cursor and are never pruned, so the view
// keeps its own cursor per workspace visit and reads forward from it. A first
// visit to a long history catches up over a few refreshes instead of one.
const NOTICE_PAGE = 500;
const NOTICE_PAGES_PER_REFRESH = 10;
const RECENT_LANDINGS = 20;
let landings = { revision: null, cursor: 0, notices: [], caughtUp: false, reading: null };

const text = (value) => (typeof value === "string" ? value : "");
const list = (value) => (Array.isArray(value) ? value.filter((item) => typeof item === "string") : []);

// While `lease.enabled` is false, every lease route answers 400 with an error
// naming that key. That is the feature's state, not a failed load: the panels
// show it as a neutral note, and a poll tick stops asking until a refresh (or a
// workspace change) rechecks it.
const OFF = { off: true };
let leasesOffRevision = null;

function leasesOff() {
  return leasesOffRevision === getWorkspaceRevision();
}

// ORB-20522: `/api/workspaces` reports each workspace's `lease_enabled`, so the
// rail hides its Leases item where leases are off instead of offering a tab
// that only says so. Only an explicit `false` hides it: an unknown setting (an
// older server, an unopenable workspace, the aggregate view) keeps the item.
// The tab itself stays routable, so a direct link still shows the off note.
export function syncLeasesRailItem(workspaces, workspaceId) {
  const tab = document.querySelector('.tab[data-tab="leases"]');
  if (!tab) return;
  const entry = workspaceId && Array.isArray(workspaces)
    ? workspaces.find((workspace) => workspace?.id === workspaceId)
    : null;
  const hide = entry?.lease_enabled === false;
  if (hide && document.activeElement === tab) tab.blur();
  tab.hidden = hide;
}

function isLeasesOffError(error) {
  return error?.status === 400 && /\blease\.enabled\b/.test(String(error.message));
}

// Reads one lease route for the workspace `revision` it was started in. A
// disabled workspace resolves to `OFF`; a success clears the flag, and any other
// failure still throws to the panel's error state.
async function readLeaseRoute(revision, read) {
  try {
    const payload = await read();
    if (leasesOffRevision === revision) leasesOffRevision = null;
    return payload;
  } catch (error) {
    if (!isLeasesOffError(error)) throw error;
    leasesOffRevision = revision;
    return OFF;
  }
}

export async function fetchAndRenderLeases() {
  if (isAggregateView()) {
    renderPanelPlaceholder("leases-body");
    $("leases-count").textContent = "—";
    return;
  }
  if (isPanelPoll() && leasesOff()) return;
  const revision = getWorkspaceRevision();
  await requestPanel(
    "leases-body",
    `leases:${revision}`,
    () => readLeaseRoute(revision, () => fetchJson("/api/leases")),
    (payload) => (payload === OFF
      ? renderLeasesOff()
      : renderLeases(Array.isArray(payload?.leases) ? payload.leases : [])),
    "leases-count",
  );
}

export async function fetchAndRenderLeaseLandings() {
  if (isAggregateView()) {
    renderPanelPlaceholder("lease-landings-body");
    $("lease-landings-count").textContent = "—";
    return;
  }
  if (isPanelPoll() && leasesOff()) return;
  const revision = getWorkspaceRevision();
  await requestPanel(
    "lease-landings-body",
    `lease-landings:${revision}`,
    () => readLeaseRoute(revision, readLandings),
    (state) => (state === OFF ? renderLandingsOff() : renderLandings(state)),
    "lease-landings-count",
  );
}

// One reader per visit: an overlapping refresh waits for the read in flight
// rather than advancing the same cursor twice.
function readLandings() {
  const revision = getWorkspaceRevision();
  if (landings.revision !== revision) {
    landings = { revision, cursor: 0, notices: [], caughtUp: false, reading: null };
  }
  const state = landings;
  if (!state.reading) {
    state.reading = readLandingPages(state).finally(() => { state.reading = null; });
  }
  return state.reading;
}

async function readLandingPages(state) {
  for (let page = 0; page < NOTICE_PAGES_PER_REFRESH; page++) {
    const body = await fetchJson(`/api/leases/notices?after=${state.cursor}&limit=${NOTICE_PAGE}`);
    const notices = Array.isArray(body?.notices) ? body.notices : [];
    for (const notice of notices) {
      if (notice?.kind === "overlap_landed" && Number(notice.seq) > state.cursor) state.notices.push(notice);
    }
    if (state.notices.length > RECENT_LANDINGS) state.notices.splice(0, state.notices.length - RECENT_LANDINGS);
    const cursor = Number(body?.cursor);
    const advanced = Number.isSafeInteger(cursor) && cursor > state.cursor;
    if (advanced) state.cursor = cursor;
    state.caughtUp = !advanced || notices.length < NOTICE_PAGE;
    if (state.caughtUp) break;
  }
  return state;
}

// Each path names the leases holding it and the leases waiting for it. A
// granted lease holds its `selectors`; `queued_selectors` is what a queued
// request, or a granted lease's pending expansion, still waits for. A path
// under a held `dir:` selector also lists that holder, so a file inside a
// leased directory never reads as free.
function leasesByPath(leases) {
  const paths = new Map();
  const entry = (path) => {
    if (!paths.has(path)) paths.set(path, { path, held: [], waiting: [] });
    return paths.get(path);
  };
  const granted = leases.filter((lease) => lease?.state === "granted");
  for (const lease of granted) {
    for (const path of list(lease.selectors)) entry(path).held.push({ lease, via: null });
  }
  for (const lease of leases) {
    if (lease?.state === "granted" || lease?.state === "queued") {
      for (const path of list(lease.queued_selectors)) entry(path).waiting.push({ lease, via: null });
    }
  }
  for (const row of paths.values()) {
    for (const lease of granted) {
      const via = list(lease.selectors).find((selector) => encloses(selector, row.path));
      if (via) row.held.push({ lease, via });
    }
  }
  const queuedAt = ({ lease }) => text(lease.queued_at) || text(lease.created_at);
  return [...paths.values()]
    .map((row) => ({ ...row, waiting: row.waiting.sort((a, b) => queuedAt(a).localeCompare(queuedAt(b))) }))
    .sort((a, b) => a.path.localeCompare(b.path));
}

// Whether a `dir:` selector strictly encloses a `file:` or `dir:` selector.
function encloses(selector, path) {
  const dir = selector.startsWith("dir:") ? selector.slice(4).replace(/\/+$/, "") : null;
  const inner = /^(file|dir):/.test(path) ? path.slice(path.indexOf(":") + 1) : null;
  if (dir == null || inner == null || inner === dir) return false;
  return dir === "" || dir === "." || inner.startsWith(`${dir}/`);
}

function chip(label, className = "") {
  return el("span", { class: `lease-chip${className ? ` ${className}` : ""}`, text: label });
}

function holder({ lease, via }, waiting) {
  const facts = [
    chip(text(lease.mode) || "hard", lease.mode === "advisory" ? "lease-chip-advisory" : "lease-chip-hard"),
    chip(`${text(lease.priority) || "normal"} priority`),
  ];
  // Absolute times, so a row kept across refreshes never reads stale.
  const at = waiting ? text(lease.queued_at) || text(lease.created_at) : text(lease.expires_at);
  if (waiting) facts.push(chip(lease.queue_kind === "expansion" ? "expansion" : "request"));
  const when = el("span", { class: "lease-when", text: `${waiting ? "queued" : "expires"} ${formatClock(at, { seconds: false })}` });
  when.title = formatDateTime(at, { seconds: true });
  const principal = text(lease.principal) || "unknown";
  const node = el("li", { class: "lease-holder" }, [
    el("span", { class: "lease-principal", text: principal }),
    el("span", { class: "lease-class", text: text(lease.principal_class) }),
    ...facts,
    when,
    via ? el("span", { class: "lease-via", text: `via ${via}` }) : null,
  ]);
  node.title = text(lease.lease_id);
  node.dataset.lease = text(lease.lease_id);
  return node;
}

function holders(entries, waiting, empty) {
  if (!entries.length) return el("span", { class: "lease-none", text: empty });
  return el("ul", { class: "lease-holders" }, entries.map((entry) => holder(entry, waiting)));
}

function cell(label, child, className = "") {
  const node = el("td", { class: className }, [child]);
  node.dataset.label = label;
  return node;
}

function renderLeases(leases) {
  const body = $("leases-body");
  if (!body) return;
  const active = leases.filter((lease) => lease?.state === "granted" || lease?.state === "queued");
  const queued = active.filter((lease) => lease.state === "queued").length;
  const count = $("leases-count");
  if (count) count.textContent = String(active.length);
  if (!active.length) {
    const note = el("div", { class: "panel-placeholder", text: "No active or queued leases in this workspace. `orbit lease request --principal <name> <selectors>` takes one." });
    note.dataset.key = "leases-empty";
    note.dataset.hash = "empty";
    syncNodes(body, [note]);
    return;
  }
  const rows = leasesByPath(active).map((row) => {
    const tr = el("tr", {}, [
      cell("Path", el("span", { class: "lease-path", text: row.path })),
      cell("Held by", holders(row.held, false, "Not held")),
      cell("Queued", holders(row.waiting, true, "None waiting")),
    ]);
    tr.dataset.path = row.path;
    return tr;
  });
  const header = (label) => {
    const th = el("th", { text: label });
    th.setAttribute("scope", "col");
    return th;
  };
  const summary = el("p", {
    class: "lease-summary",
    text: `${active.length - queued} granted · ${queued} queued · ${rows.length} ${rows.length === 1 ? "path" : "paths"}`,
  });
  summary.dataset.key = "leases-summary";
  summary.dataset.hash = summary.textContent;
  const table = el("table", { class: "lease-table" }, [
    el("caption", { class: "visually-hidden", text: "Active and queued path leases by path" }),
    el("thead", {}, [el("tr", {}, [header("Path"), header("Held by"), header("Queued")])]),
    el("tbody", {}, rows),
  ]);
  table.dataset.key = "leases-table";
  table.dataset.hash = JSON.stringify(active.map((lease) => [
    lease.lease_id, lease.state, lease.selectors, lease.queued_selectors, lease.queue_kind, lease.queued_at,
    lease.expires_at, lease.mode, lease.priority, lease.principal, lease.principal_class,
  ]));
  syncNodes(body, [summary, table]);
}

// The leases panel names the setting; the landings panel only says it is off, so
// the explanation is not repeated in both panels.
function renderLeasesOff() {
  const body = $("leases-body");
  if (!body) return;
  const count = $("leases-count");
  if (count) count.textContent = "—";
  const note = el("div", { class: "panel-placeholder", text: "Path leases are off in this workspace. Set `lease.enabled = true` to accept lease operations." });
  note.dataset.key = "leases-off";
  note.dataset.hash = "off";
  syncNodes(body, [note]);
}

function renderLandingsOff() {
  const body = $("lease-landings-body");
  if (!body) return;
  const count = $("lease-landings-count");
  if (count) count.textContent = "—";
  const note = el("div", { class: "panel-placeholder", text: "Off while path leases are disabled." });
  note.dataset.key = "landings-off";
  note.dataset.hash = "off";
  syncNodes(body, [note]);
}

function renderLandings(state) {
  const body = $("lease-landings-body");
  if (!body) return;
  const notices = [...state.notices].reverse();
  const count = $("lease-landings-count");
  if (count) count.textContent = String(notices.length);
  const nodes = [];
  if (!state.caughtUp) {
    const note = el("p", { class: "lease-summary", text: "Reading older notices; the newest landings appear once the history is caught up." });
    note.dataset.key = "landings-catching-up";
    note.dataset.hash = "catching-up";
    nodes.push(note);
  }
  if (!notices.length) {
    const note = el("div", { class: "panel-placeholder", text: "No landed commit has changed a leased path yet." });
    note.dataset.key = "landings-empty";
    note.dataset.hash = "empty";
    nodes.push(note);
    syncNodes(body, nodes);
    return;
  }
  const items = notices.map((notice) => {
    const payload = notice.payload && typeof notice.payload === "object" ? notice.payload : {};
    const commit = text(payload.commit);
    const task = text(payload.task_id);
    const commitNode = el("span", { class: "lease-commit", text: commit.slice(0, 12) || "unknown commit" });
    commitNode.title = commit;
    const taskNode = task ? el("a", { class: "lease-task", text: task }) : el("span", { class: "lease-none", text: "no task" });
    if (task) taskNode.href = `#tasks/${encodeURIComponent(task)}`;
    const item = el("article", { class: "lease-landing" }, [
      el("div", { class: "lease-landing-head" }, [
        commitNode,
        el("span", { class: "lease-landing-on", text: `landed on ${text(payload.base_branch) || "its base"} by` }),
        taskNode,
      ]),
      el("div", { class: "lease-landing-meta" }, [
        el("span", { text: "Lease " }),
        el("span", { class: "lease-id", text: text(notice.lease_id) }),
      ]),
      el("ul", { class: "lease-landing-paths" }, list(payload.paths).map((path) => el("li", { class: "lease-path", text: path }))),
    ]);
    item.dataset.key = `landing-${notice.seq}`;
    item.dataset.hash = String(notice.seq);
    return item;
  });
  nodes.push(...items);
  syncNodes(body, nodes);
}
