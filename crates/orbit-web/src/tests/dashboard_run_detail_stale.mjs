// Deferred run-detail fetches must not paint after the operator has moved on.
// Navigate A→B, across workspaces, and A→B→A; resolve or reject A last.
// The run left on screen keeps its metadata, logs, events, and action targets.

const originalSetTimeout = globalThis.setTimeout;
globalThis.setTimeout = (fn, ms, ...args) => {
  const handle = originalSetTimeout(fn, Number(ms) || 0, ...args);
  if (Number(ms) >= 1000 && handle && typeof handle.unref === "function") handle.unref();
  return handle;
};
const tick = () => new Promise((resolve) => originalSetTimeout(resolve, 0));
const flush = async () => {
  for (let i = 0; i < 6; i++) await tick();
};

location.hash = "#runs/run-a";
window.prompt = () => "operator stop";
window.confirm = () => true;

const pending = [];
const posts = [];

function defer() {
  let resolve;
  const promise = new Promise((res) => { resolve = res; });
  return { promise, resolve };
}

function json(payload, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => payload,
    text: async () => JSON.stringify(payload),
  };
}

function detail(runId, marker) {
  return {
    run: {
      run_id: runId,
      job_id: marker,
      state: "running",
      attempt: 1,
      started_at: "2026-09-28T00:00:00Z",
    },
    steps: [{
      step_index: 0,
      target_type: "agent",
      target_id: "impl",
      state: "ok",
      duration_ms: 12,
      exit_code: 0,
    }],
  };
}

function events(marker) {
  return [{
    event_id: `ev-${marker}`,
    ts: "2026-09-28T00:00:01Z",
    body_kind: "note",
    event_type: "run",
    agent_identity: "grok",
    message: marker,
  }];
}

function logs(marker) {
  return [{
    step_index: 0,
    step_id: "impl",
    stdout_preview: marker,
    stderr_preview: "",
    provider: "cli",
    exit_code: 0,
  }];
}

globalThis.fetch = async (path, options) => {
  const url = new URL(String(path), "http://dashboard.test");
  const method = (options && options.method) || "GET";
  if (method !== "GET") {
    posts.push(url.pathname + url.search);
    return json({ run_id: "jrun-next" });
  }
  const match = url.pathname.match(/^\/api\/runs\/([^/]+)(?:\/(events|logs))?$/);
  if (match) {
    const gate = defer();
    pending.push({
      runId: decodeURIComponent(match[1]),
      kind: match[2] || "detail",
      workspace: url.searchParams.get("workspace"),
      gate,
    });
    const result = await gate.promise;
    if (result && result.__reject) {
      const error = new Error(result.message || "run fetch failed");
      error.status = result.status || 500;
      throw error;
    }
    return json(result);
  }
  if (url.pathname === "/api/workspaces") {
    return json([
      { id: "one", name: "One", status: "active", is_default: true },
      { id: "two", name: "Two", status: "active" },
    ]);
  }
  if (url.pathname === "/api/audit/summary") {
    return json({ events: 1, denials: 0, failed_runs: 0, active_long_runs: 0, sparkline: [], window: "24h" });
  }
  if (url.pathname.startsWith("/api/workflows/auto/readiness")) {
    return json({ tasks: [], controls_authorized: false, capacity: { drain_phase: "idle" } });
  }
  return json([]);
};

function describePending() {
  return pending.map((item) => `${item.workspace}:${item.runId}:${item.kind}`).join(", ");
}

function take(runId, kind, workspace, newest) {
  let index = -1;
  pending.forEach((item, i) => {
    if (item.runId === runId && item.kind === kind && item.workspace === workspace) {
      if (newest || index < 0) index = i;
    }
  });
  if (index < 0) throw new Error(`no pending ${workspace} ${runId} ${kind}; pending=${describePending()}`);
  return pending.splice(index, 1)[0];
}

async function fulfillRun(runId, workspace, marker, newest = false) {
  take(runId, "detail", workspace, newest).gate.resolve(detail(runId, marker));
  take(runId, "events", workspace, newest).gate.resolve(events(marker));
  take(runId, "logs", workspace, newest).gate.resolve(logs(marker));
  await flush();
}

async function rejectRun(runId, workspace) {
  for (const kind of ["detail", "events", "logs"]) {
    take(runId, kind, workspace).gate.resolve({ __reject: true, status: 500, message: `failed ${workspace} ${runId}` });
  }
  await flush();
}

function findButton(node, className) {
  if (!node) return null;
  if (String(node.className || "").split(/\s+/).includes(className)) return node;
  for (const child of node.children || []) {
    const found = findButton(child, className);
    if (found) return found;
  }
  return null;
}

function viewBlob() {
  return [
    get("run-detail-title").textContent,
    get("run-detail-meta").textContent,
    get("run-events-body").textContent,
    get("run-steps-body").textContent,
  ].join("\n");
}

function assertCleared(forbidden) {
  const blob = viewBlob();
  if (blob.includes(forbidden)) throw new Error(`navigation left ${forbidden} on screen: ${blob}`);
  if (findButton(get("run-detail-meta"), "run-cancel") || findButton(get("run-detail-meta"), "run-replay")) {
    throw new Error(`navigation left action buttons mounted: ${get("run-detail-meta").textContent}`);
  }
}

function assertView(runId, marker, workspace) {
  const title = get("run-detail-title").textContent;
  if (title !== `Run ${runId}`) throw new Error(`title is ${title}, expected Run ${runId}`);
  if (getWorkspace() !== workspace) throw new Error(`workspace is ${getWorkspace()}, expected ${workspace}`);
  if (getActiveRunId() !== runId) throw new Error(`active run is ${getActiveRunId()}, expected ${runId}`);
  const run = getActiveRunDetail() && getActiveRunDetail().run;
  if (!run || run.job_id !== marker || run.run_id !== runId) {
    throw new Error(`detail is ${JSON.stringify(run)}, expected ${runId} / ${marker}`);
  }
  const meta = get("run-detail-meta").textContent;
  if (!meta.includes(marker)) throw new Error(`metadata missing ${marker}: ${meta}`);
  const renderedEvents = get("run-events-body").textContent;
  if (!renderedEvents.includes(marker)) throw new Error(`events missing ${marker}: ${renderedEvents}`);
  const storedEvents = getActiveRunEvents();
  if (!storedEvents[0] || storedEvents[0].message !== marker) {
    throw new Error(`event state is ${JSON.stringify(storedEvents)}`);
  }
  const storedLogs = getActiveRunLogs();
  if (!storedLogs[0] || storedLogs[0].stdout_preview !== marker) {
    throw new Error(`log state is ${JSON.stringify(storedLogs)}`);
  }
  let steps = get("run-steps-body").textContent;
  if (!steps.includes(marker)) {
    const row = get("run-steps-body").children.find((node) => String(node.className).includes("step-row"));
    if (!row || !row.listeners.click) throw new Error(`no step row while checking ${marker}: ${steps}`);
    row.listeners.click();
    steps = get("run-steps-body").textContent;
  }
  if (!steps.includes(marker)) throw new Error(`step logs missing ${marker}: ${steps}`);
  const cancel = findButton(get("run-detail-meta"), "run-cancel");
  const replay = findButton(get("run-detail-meta"), "run-replay");
  if (!cancel || cancel.title !== `Cancel ${runId}`) throw new Error(`cancel target ${cancel && cancel.title}`);
  if (!replay || replay.title !== `Replay ${runId}`) throw new Error(`replay target ${replay && replay.title}`);
}

function assertAbsent(marker) {
  const blob = viewBlob();
  if (blob.includes(marker)) throw new Error(`late response painted ${marker}: ${blob}`);
  const run = getActiveRunDetail() && getActiveRunDetail().run;
  if (run && run.job_id === marker) throw new Error(`late response replaced detail with ${marker}`);
}

async function assertCancel(runId, workspace, marker) {
  const before = posts.length;
  const cancel = findButton(get("run-detail-meta"), "run-cancel");
  if (!cancel || !cancel.listeners.click) throw new Error("cancel button missing");
  cancel.listeners.click({ stopPropagation() {} });
  await flush();
  const hit = posts.slice(before).find((url) => url.includes("/cancel"));
  const expected = `/api/runs/${encodeURIComponent(runId)}/cancel?workspace=${encodeURIComponent(workspace)}`;
  if (hit !== expected) throw new Error(`cancel posted ${hit}, expected ${expected}`);
  while (pending.some((item) => item.runId === runId && item.workspace === workspace)) {
    const item = pending.find((entry) => entry.runId === runId && entry.workspace === workspace);
    const payload = item.kind === "detail" ? detail(runId, marker)
      : item.kind === "events" ? events(marker)
      : logs(marker);
    pending.splice(pending.indexOf(item), 1);
    item.gate.resolve(payload);
  }
  await flush();
  assertView(runId, marker, workspace);
}

function workspaceSelect() {
  const select = get("rail-workspace").children.find((node) => node.id === "workspace-select");
  if (!select || !select.listeners.change) throw new Error("workspace selector was not installed");
  return select;
}

await import("./app.js");
await flush();

const { getWorkspace } = await import("./js/common.js");
const { getActiveRunId, getActiveRunDetail, getActiveRunEvents, getActiveRunLogs } = await import("./js/run-detail.js");

// A → B in one workspace. B paints first; A's late success must not replace it.
location.hash = "#runs/run-b";
await fulfillRun("run-b", "one", "bravo-view");
assertView("run-b", "bravo-view", "one");
await fulfillRun("run-a", "one", "alpha-view");
assertView("run-b", "bravo-view", "one");
assertAbsent("alpha-view");
await assertCancel("run-b", "one", "bravo-view");

// A's late failure must not clear B.
location.hash = "#runs/run-a";
assertCleared("bravo-view");
location.hash = "#runs/run-b";
await fulfillRun("run-b", "one", "bravo-kept");
assertView("run-b", "bravo-kept", "one");
await rejectRun("run-a", "one");
assertView("run-b", "bravo-kept", "one");
assertAbsent("failed one run-a");

// A → B → A. The second visit paints, then the middle visit resolves and the
// first visit rejects.
location.hash = "#runs/run-a";
location.hash = "#runs/run-b";
location.hash = "#runs/run-a";
await fulfillRun("run-a", "one", "second-view", true);
assertView("run-a", "second-view", "one");
await fulfillRun("run-b", "one", "middle-view");
await rejectRun("run-a", "one");
assertView("run-a", "second-view", "one");
assertAbsent("middle-view");
assertAbsent("failed one run-a");

// Across workspaces, including a same run id fetched for the previous
// workspace. Late responses must not restore the old action targets.
get("refresh-btn").listeners.click();
const select = workspaceSelect();
select.value = "two";
select.listeners.change();
assertCleared("second-view");
location.hash = "#runs/run-b";
await fulfillRun("run-b", "two", "cross-view");
assertView("run-b", "cross-view", "two");
await rejectRun("run-a", "two");
await fulfillRun("run-a", "one", "stale-workspace");
assertView("run-b", "cross-view", "two");
assertAbsent("stale-workspace");
assertAbsent("second-view");
await assertCancel("run-b", "two", "cross-view");

// Same run id, other workspace: the late response from workspace two must
// not replace workspace one's metadata or point cancel at two.
get("refresh-btn").listeners.click();
select.value = "one";
select.listeners.change();
assertCleared("cross-view");
await fulfillRun("run-b", "one", "home-view");
assertView("run-b", "home-view", "one");
await fulfillRun("run-b", "two", "stale-same-id");
assertView("run-b", "home-view", "one");
assertAbsent("stale-same-id");
await assertCancel("run-b", "one", "home-view");

if (pending.length) throw new Error(`fetches still deferred: ${describePending()}`);
