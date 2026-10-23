// Settings › Hosts: the serving host's registered remote hosts [ORB-14451].
//
// Rows are `orbit host list --json` for the host file of the machine serving
// this dashboard. Add, rename and remove call the same operations as
// `orbit host`, so a refusal is the CLI's typed error, shown beside the
// control that caused it. Opening the view, Reload and Add probe the hosts;
// first paint the cached list with pending reachability, then update it when
// the probe finishes. The background poll reads cached fields only and keeps
// each row's last live reading, so a host added from the CLI appears on the
// next refresh without the dashboard opening SSH sessions every 30 seconds.
//
// Health is separate from that probe [ORB-15230]. While this view is open,
// each refresh also reads `/host/resources` and one `/fleet/drains` for every
// reachable host through `/api/on/<host>/` (the serving host's own name is
// answered locally). An unreachable host is not asked; its row shows the
// probe error. One host's failure leaves the others up. Show uses the same
// switch as the host picker.
//
// Editors are inline (the dashboard has no modals): they open with focus in
// their first control, close on Escape as Cancel does unless a request is in
// flight, and hand focus back to the button that opened them.

import { captureFocus, el, fetchJson, getHost, requestHostSwitch, requestJson, requestPanel } from './common.js';
import { describeResource, fetchHostResourcePayloadFor, hostResources, hostVerdict } from './host-resources.js';

const $ = (id) => document.getElementById(id);

const NO_VALUE = "–";
/// What only a probe reports; a `?probe=false` row borrows these from the
/// last probe of the same host at the same SSH target.
const LIVE_FIELDS = ["reachable", "error", "binary_version", "protocol_fingerprint", "skew", "skew_fields", "workspaces", "task_prefix"];

let lastPayload = null;
let live = new Map();
let probeNext = true;
/// The active probe and its SSH targets; cached refreshes must not cancel it.
let pendingProbe = null;
/// The open editor: `{kind: "add" | "rename" | "remove", id, draft, pending,
/// error, code, dependents}`. Operator state, so a refresh never discards it.
let editing = null;
let notice = "";
/// Per-host resource snapshot and drain capacity, keyed by machine id.
/// Replaced as a whole when a refresh's fan-out finishes.
let health = new Map();
/// Bumped when the view is left or a newer refresh starts, so an older
/// refresh cannot paint over the next one.
let epoch = 0;
/// Bumped when the view is left or a newer health load starts, so an older
/// health fan-out cannot paint over newer health readings.
let healthEpoch = 0;

/// Forget everything when the view is left, so reopening it probes again.
export function resetHostsView() {
  epoch += 1;
  healthEpoch += 1;
  lastPayload = null;
  live = new Map();
  probeNext = true;
  pendingProbe = null;
  editing = null;
  notice = "";
  health = new Map();
}

export async function fetchAndRenderHosts({ probe = probeNext } = {}) {
  const token = ++epoch;
  const checking = probe ? { targets: new Map() } : null;
  if (probe) pendingProbe = checking;
  try {
    await requestPanel(
      "config-body",
      "hosts",
      () => fetchJson("/api/hosts?probe=false"),
      (payload) => {
        if (token !== epoch) return;
        if (checking) {
          checking.targets = new Map((payload.hosts || []).filter((row) => !row.local).map((row) => [row.machine_id, row.ssh]));
          probeNext = false;
        }
        lastPayload = payload;
        render();
      },
      "config-count",
    );
  } catch (error) {
    if (pendingProbe === checking) pendingProbe = null;
    throw error;
  }
  if (token !== epoch) {
    if (checking && pendingProbe === checking) {
      pendingProbe = null;
      probeNext = true;
    }
    return;
  }
  if (checking) {
    try {
      const payload = await fetchJson("/api/hosts");
      if (pendingProbe !== checking) return;
      const targets = new Map((lastPayload.hosts || []).map((row) => [row.machine_id, row.ssh]));
      live = new Map([...live].filter(([id, seen]) => targets.get(id) === seen.ssh));
      remember((payload.hosts || []).filter((row) => targets.get(row.machine_id) === row.ssh));
    } catch (error) {
      if (pendingProbe !== checking) return;
      notice = `Host checks failed: ${error.message}. Use Reload to try again.`;
      probeNext = true;
    }
    pendingProbe = null;
    // Keep the latest cached list: a host may have been renamed, removed or
    // retargeted while the probe ran. Live fields match by id and SSH target.
    render();
  }
  await loadHealth();
}

function remember(rows) {
  for (const row of rows) {
    if (row.local || row.reachable == null) continue;
    live.set(row.machine_id, { ssh: row.ssh, fields: Object.fromEntries(LIVE_FIELDS.map((field) => [field, row[field]])) });
  }
}

function withLiveFields(row) {
  if (row.local) return row;
  const seen = live.get(row.machine_id);
  const fields = row.reachable == null && seen?.ssh === row.ssh ? seen.fields : {};
  return {
    ...row, ...fields,
    task_prefix: row.task_prefix ?? fields.task_prefix,
    checking: pendingProbe?.targets.get(row.machine_id) === row.ssh,
  };
}

// ------------------------------------------------------------------ render

function render() {
  const body = $("config-body");
  if (!body || !lastPayload) return;
  if (!editable()) editing = null;
  const restoreBody = captureFocus(body);
  const restoreControls = captureFocus($("config-controls"));
  renderControls();
  body.replaceChildren(...view(lastPayload));
  const hosts = lastPayload.hosts || [];
  const count = $("config-count");
  if (count) count.textContent = `${Math.max(hosts.length - 1, 0)} remote host${hosts.length === 2 ? "" : "s"}`;
  restoreBody();
  restoreControls();
}

function renderControls() {
  const controls = $("config-controls");
  if (!controls) return;
  controls.hidden = false;
  const reload = actionButton("Reload", () => {
    editing = null;
    notice = "";
    fetchAndRenderHosts({ probe: true }).catch((error) => console.error("Failed to reload hosts", error));
  });
  reload.title = "Read the host file again and probe every host";
  controls.replaceChildren(reload);
}

/// Edits are governed; a session without the operator capability sees the
/// hosts read-only rather than an editor that 403s on save.
function editable() {
  return lastPayload?.host_edit?.authorized !== false;
}

function view(payload) {
  const hosts = (payload.hosts || []).map(withLiveFields);
  const local = hosts.find((row) => row.local);
  const nodes = [];
  if (payload.load_error) nodes.push(loadErrorBanner(payload));
  nodes.push(
    el("div", { class: "config-strip host-scope" }, [
      el("div", {
        class: "config-note",
        text: `This dashboard edits the host file of ${local ? local.name : "the serving host"}: ${payload.host_file || "hosts.toml"}.`,
      }),
      // ORB-14680: these routes are never forwarded, so with another host
      // selected the list is still the serving host's file, and says so.
      getHost()
        ? el("div", {
            class: "config-note host-scope-selected",
            text: `Showing ${getHost()} in the other views. This list is still ${local ? local.name : "the serving host"}'s host file, the one the host picker reads.`,
          })
        : null,
      payload.legacy
        ? el("div", {
            class: "config-warning",
            text: "Read from the legacy mcp-destinations.toml. The first add, rename or remove here migrates it to the host file, and needs every retained host to answer.",
          })
        : null,
      editable()
        ? null
        : el("div", {
            class: "config-warning host-read-only",
            text: `Read-only: ${payload.host_edit.reason || "editing the host file needs the operator capability"}`,
          }),
    ]),
  );
  const status = el("div", { class: "host-notice", text: notice });
  status.setAttribute("role", "status");
  status.setAttribute("aria-live", "polite");
  nodes.push(status);
  if (editable()) nodes.push(addSection());
  const list = el("div", { class: "host-list" });
  list.setAttribute("role", "table");
  list.setAttribute("aria-label", "Registered hosts");
  list.appendChild(headRow());
  const names = new Map(hosts.map((host) => [host.machine_id, host.name]));
  for (const row of hosts) list.appendChild(hostRow(row, names));
  nodes.push(list);
  return nodes;
}

function loadErrorBanner(payload) {
  const banner = el("div", { class: "config-review-status alert host-load-error" }, [
    el("span", { class: "config-review-switch", text: `The host file failed to load (${payload.load_error.code})` }),
    el("span", { class: "config-review-reason", text: payload.load_error.message }),
    el("span", {
      class: "config-review-remedy",
      text: "Showing the last valid host file. Correct the file; this view picks it up on the next refresh.",
    }),
  ]);
  banner.setAttribute("role", "alert");
  return banner;
}

function headRow() {
  const head = el("div", { class: "host-grid host-head col-head" }, [
    "Host", "Reachable", "Version", "Protocol", "Skew", "Workspaces", "Health", "",
  ].map((label) => el("span", { text: label })));
  head.setAttribute("role", "row");
  for (const cell of head.children) cell.setAttribute("role", "columnheader");
  return head;
}

function hostRow(row, names) {
  const node = el("div", { class: `host-row${row.local ? " local" : ""}${row.reachable === false ? " unreachable" : ""}` });
  node.dataset.key = row.machine_id;
  const cells = el("div", { class: "host-grid" }, [
    identityCell(row),
    reachableCell(row),
    el("span", { class: "host-version mono", text: row.binary_version || NO_VALUE }),
    el("span", {
      class: "host-protocol mono",
      text: row.protocol_fingerprint ? row.protocol_fingerprint.slice(0, 12) : NO_VALUE,
      title: row.protocol_fingerprint || "not probed",
    }),
    skewCell(row),
    workspacesCell(row, names),
    healthCell(row),
    actionsCell(row),
  ]);
  cells.setAttribute("role", "row");
  for (const cell of cells.children) cell.setAttribute("role", "cell");
  node.appendChild(cells);
  if (row.error && !row.checking) {
    node.appendChild(el("div", { class: "host-error", text: hostError(row), title: row.error.message }));
  }
  if (editing?.id === row.machine_id) {
    node.appendChild(editing.kind === "rename" ? renameEditor(row) : removeEditor(row));
  }
  return node;
}

function identityCell(row) {
  return el("span", { class: "host-identity" }, [
    el("span", { class: "host-name" }, [
      el("span", { class: "host-name-text", text: row.name }),
      row.local ? el("span", { class: "config-source workspace", text: "local · edited here" }) : null,
      row.legacy ? el("span", { class: "config-source unset", text: "legacy" }) : null,
      hostSwitch(row),
    ]),
    el("span", {
      class: "host-facts mono",
      text: [row.machine_id, row.ssh ? `ssh ${row.ssh}` : null, `prefix ${row.task_prefix || "unknown"}`].filter(Boolean).join(" · "),
    }),
  ]);
}

/// Show switches the dashboard the way the host picker does. The href is the
/// same `?host=` the picker writes, so the control still works as a link.
function hostSwitch(row) {
  const selected = getHost();
  const showing = row.local ? !selected : !!selected && selected.toLowerCase() === String(row.name).toLowerCase();
  if (showing) return el("span", { class: "host-showing", text: "showing" });
  const name = row.local ? null : row.name;
  const link = el("a", {
    class: "host-switch",
    text: "Show",
    title: `Switch the dashboard to ${row.name}`,
  });
  link.href = hostSwitchHref(name);
  link.setAttribute("aria-label", `Show ${row.name}`);
  link.addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    requestHostSwitch(name);
  });
  return link;
}

function hostSwitchHref(name) {
  const params = new URLSearchParams(window.location.search || "");
  if (name) params.set("host", name);
  else params.delete("host");
  const query = params.toString();
  const path = window.location.pathname || "";
  const hash = window.location.hash || "";
  return `${path}${query ? `?${query}` : ""}${hash}`;
}

function hostError(row) {
  if (row.error.code === "unreachable_destination") {
    return `${row.name} could not be reached. Check the network connection and SSH access to ${row.ssh || row.name}, then use Reload to try again.`;
  }
  return `${row.error.code}: ${row.error.message}`;
}

function healthCell(row) {
  if (row.checking) return el("span", { class: "host-health host-health-pending", text: "checking…" });
  if (row.reachable === false) {
    const error = !row.error || row.error.code === "unreachable_destination" ? "could not be reached" : hostError(row);
    return el("span", { class: "host-health host-health-error", text: error });
  }
  if (row.reachable == null) return el("span", { class: "host-health host-health-pending", text: "not probed" });
  const reading = health.get(row.machine_id);
  if (!reading || reading.kind !== "ready") {
    return el("span", { class: "host-health host-health-pending", text: "…" });
  }
  const verdict = reading.resources ? hostVerdict(reading.resources) : null;
  const chips = reading.resources
    ? el("span", { class: "host-health-readings" }, hostResources(reading.resources).map((resource) => resourceChip(reading.resources, resource)))
    : el("span", { class: "host-health-error", text: reading.resourcesError || "resources unavailable" });
  const drain = drainLine(reading, verdict);
  const node = el("span", { class: "host-health" }, [chips, drain]);
  const summary = [chips.textContent, drain.textContent].filter(Boolean).join("; ");
  const throttle = verdict ? verdict.status : "unknown";
  node.setAttribute("aria-label", `${row.name}: ${summary}. Throttle verdict: ${throttle}`);
  return node;
}

function resourceChip(payload, resource) {
  const { label, value, suffix, detail, severity, held } = describeResource(payload, resource);
  const node = el("span", {
    class: `host-resource ${severity}${held ? " throttled" : ""}`,
    title: detail,
  }, [
    el("span", { class: "k", text: label }),
    el("span", { class: "v", text: value }, suffix ? [el("span", { class: "unit", text: suffix })] : []),
  ]);
  node.dataset.resource = resource;
  return node;
}

function drainLine(reading, verdict) {
  const { text, active, title } = drainSummary(reading, verdict);
  const throttled = verdict?.status === "held";
  return el("span", {
    class: `host-drain${active ? " active" : ""}${throttled ? " throttled" : ""}`,
    text,
    title,
  });
}

/// One line for every workspace's drain capacity, plus the admission verdict
/// the top bar states. A failed capacity read is named; it does not become "idle".
function drainSummary(reading, verdict) {
  const drains = reading.drains || [];
  const known = drains.filter((drain) => !drain.error);
  const failed = drains.filter((drain) => drain.error);
  const parts = [];
  const draining = known.some((drain) => drain.phase === "draining");
  const winding = known.some((drain) => drain.phase === "winding_down");
  const pulling = known.some((drain) => drain.pull);
  if (draining) parts.push("Draining");
  else if (winding) parts.push("Winding down");
  if (pulling) {
    const live = known.some((drain) => drain.pull && !drain.pullStopped);
    parts.push(live ? "Pull drain" : "Pull drain · admissions stopped");
  }
  if (!parts.length && known.length) parts.push("idle");
  if (!known.length && failed.length) parts.push(failed[0].error);
  if (reading.workspaceCount === 0 && !parts.length) parts.push("idle");
  if (verdict?.status === "held") parts.push("throttled");
  else if (!reading.resources || verdict?.status === "unknown") parts.push("throttle unknown");
  const lines = known.map((drain) => {
    const phase = drain.phase === "draining" ? "Draining" : drain.phase === "winding_down" ? "Winding down" : "idle";
    return `${drain.name}: ${phase}${drain.pull ? ", pull drain" : ""}`;
  });
  for (const drain of failed) lines.push(`${drain.name}: ${drain.error}`);
  if (verdict) lines.push(`Throttle verdict: ${verdict.status}${reading.resources?.reason ? ` · ${reading.resources.reason}` : ""}`);
  else lines.push("Throttle verdict: unknown");
  return { text: parts.join(" · ") || "…", active: draining || winding || pulling, title: lines.join("\n") };
}

async function loadHealth() {
  const token = ++healthEpoch;
  const payload = lastPayload;
  if (!payload || token !== healthEpoch) return;
  const rows = (payload.hosts || []).map(withLiveFields);
  const entries = await Promise.all(rows.map(async (row) => [row.machine_id, await readHealth(row)]));
  if (token !== healthEpoch) return;
  health = new Map(entries);
  render();
}

async function readHealth(row) {
  if (row.checking) return { kind: "checking" };
  if (row.reachable === false) return { kind: "unreachable" };
  if (row.reachable == null) return { kind: "unprobed" };
  const workspaces = Array.isArray(row.workspaces) ? row.workspaces : [];
  const [resources, drains] = await Promise.all([
    fetchHostResourcePayloadFor(row.name, { shareSelected: true, identity: row }).then(
      (body) => ({ resources: body, resourcesError: null }),
      (error) => ({ resources: null, resourcesError: error.message || "resources unavailable" }),
    ),
    readDrains(row.name, workspaces),
  ]);
  return { kind: "ready", ...resources, drains, workspaceCount: workspaces.length };
}

async function readDrains(hostName, workspaces) {
  if (!workspaces.length) return [];
  let items;
  let failure;
  try {
    const payload = await fetchJson(`/api/on/${encodeURIComponent(hostName)}/fleet/drains`);
    if (!Array.isArray(payload?.items)) throw new Error("drain unavailable");
    items = new Map(payload.items.map((item) => [item.workspace_id, item]));
  } catch (error) {
    // Older remote dashboards may predate the aggregate endpoint entirely.
    if (error.status === 404) return Promise.all(workspaces.map((workspace) => readLegacyDrain(hostName, workspace)));
    failure = error.message || "drain unavailable";
  }
  return Promise.all(workspaces.map((workspace) => {
    const id = workspace.id;
    const item = items?.get(id);
    // An older remote aggregate cannot describe concurrent drain/pull state.
    // Keep its existing readiness API as the compatibility path for that row.
    if (item && !item.error && item.phase !== "unavailable" && item.drain_phase === undefined) {
      return readLegacyDrain(hostName, workspace);
    }
    // Missing or malformed rows remain unknown, never silently idle.
    const error = failure || item?.error || (!item?.drain_phase || typeof item.pull_active !== "boolean"
      || typeof item.pull_admissions_stopped !== "boolean" ? "drain unavailable" : null);
    return {
      id,
      name: workspace.name || id,
      phase: error ? null : item.drain_phase,
      pull: !error && item.pull_active,
      pullStopped: !error && item.pull_admissions_stopped,
      error,
    };
  }));
}

async function readLegacyDrain(hostName, workspace) {
  const drain = { id: workspace.id, name: workspace.name || workspace.id, phase: null, pull: false, pullStopped: false, error: null };
  try {
    const payload = await fetchJson(`/api/on/${encodeURIComponent(hostName)}/workflows/auto/readiness?workspace=${encodeURIComponent(workspace.id)}`);
    const capacity = payload?.capacity;
    if (!capacity) throw new Error("drain unavailable");
    drain.phase = capacity.drain_phase || (capacity.drain_run_id ? "draining" : "idle");
    drain.pull = Boolean(capacity.pull_drain_run_id);
    drain.pullStopped = capacity.pull_drain_admissions_stopped === true;
  } catch (error) {
    drain.error = error.message || "drain unavailable";
  }
  return drain;
}

function reachableCell(row) {
  if (row.local) return el("span", { class: "host-reach ok", text: "this host" });
  if (row.checking) return el("span", { class: "host-reach unknown", text: "checking…" });
  if (row.reachable == null) return el("span", { class: "host-reach unknown", text: "not probed" });
  if (row.reachable === false && row.error?.code === "unreachable_destination") return el("span", { class: "host-reach down", text: "could not be reached" });
  if (row.error) {
    return el("span", { class: `host-reach ${row.reachable ? "warn" : "down"}`, text: `${row.reachable ? "answered" : "no"} · ${row.error.code}` });
  }
  return el("span", { class: "host-reach ok", text: "yes" });
}

function skewCell(row) {
  if (!row.skew) return el("span", { class: "host-skew", text: row.reachable ? "none" : NO_VALUE });
  return el("span", { class: "host-skew warn", text: `skew: ${(row.skew_fields || []).join(", ")}` });
}

// `names` maps each host's machine id to its registered name, so a replica
// names its owner as the host file does; an unregistered owner keeps its id.
function workspacesCell(row, names) {
  const workspaces = row.workspaces || [];
  if (!workspaces.length) return el("span", { class: "host-workspaces", text: NO_VALUE });
  return el("span", { class: "host-workspaces" }, workspaces.map((workspace) =>
    el("span", {
      class: "host-workspace",
      text: workspace.role === "replica"
        ? `${workspace.name} (replica of ${names.get(workspace.owner_machine_id) || workspace.owner_machine_id || "unknown"})`
        : `${workspace.name} (owner)`,
      title: workspace.id,
    }),
  ));
}

function actionsCell(row) {
  if (row.local || !editable()) return el("span", { class: "host-actions" });
  const busy = editing?.id === row.machine_id;
  const rename = actionButton("Rename", () => openEditor({ kind: "rename", id: row.machine_id, draft: { name: row.name } }), "ghost host-rename");
  const remove = actionButton("Remove", () => openEditor({ kind: "remove", id: row.machine_id }), "ghost host-remove");
  rename.setAttribute("aria-label", `Rename ${row.name}`);
  remove.setAttribute("aria-label", `Remove ${row.name}`);
  rename.setAttribute("aria-expanded", String(busy && editing.kind === "rename"));
  remove.setAttribute("aria-expanded", String(busy && editing.kind === "remove"));
  return el("span", { class: "host-actions" }, [rename, remove]);
}

// ----------------------------------------------------------------- editors

function addSection() {
  const section = el("div", { class: "host-add" });
  section.dataset.key = "host-add";
  if (editing?.kind !== "add") {
    const open = actionButton("Add host", () => openEditor({ kind: "add", id: null, draft: { ssh: "", name: "" } }), "primary host-add-open");
    open.setAttribute("aria-expanded", "false");
    section.appendChild(open);
    return section;
  }
  const ssh = textInput("host-input-ssh", "SSH target", "alias or user@host", editing.draft, "ssh");
  const name = textInput("host-input-name", "Name (optional)", "defaults to the host's machine.name", editing.draft, "name");
  const form = el("form", { class: "config-editor host-editor" }, [
    el("div", { class: "config-editor-head" }, [
      el("span", { class: "config-section-title", text: "Add a host" }),
      el("span", { class: "config-note", text: "Orbit logs in over SSH, reads the host's identity and writes the entry here. Nothing is written on that host." }),
    ]),
    el("div", { class: "host-fields" }, [ssh.label, name.label]),
    el("div", { class: "config-editor-foot" }, [
      el("span", { class: "config-note", text: editing.pending ? "Probing the host…" : "" }),
      el("span", { class: "config-editor-actions" }, [
        submitButton(editing.pending ? "Adding…" : "Add"),
        actionButton("Cancel", closeEditor, "ghost"),
      ]),
    ]),
    editorError(),
  ]);
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    submitAdd(ssh.read(), name.read());
  });
  section.appendChild(cancelOnEscape(form));
  return section;
}

function renameEditor(row) {
  const name = textInput("host-input-name", "New name", row.name, editing.draft, "name");
  const form = el("form", { class: "config-editor host-editor" }, [
    el("div", { class: "host-fields" }, [name.label]),
    el("div", { class: "config-editor-foot" }, [
      el("span", { class: "config-note", text: `Renames the entry only; ${row.machine_id} and its SSH target are unchanged.` }),
      el("span", { class: "config-editor-actions" }, [
        submitButton(editing.pending ? "Saving…" : "Save"),
        actionButton("Cancel", closeEditor, "ghost"),
      ]),
    ]),
    editorError(),
  ]);
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    submitRename(row, name.read());
  });
  return cancelOnEscape(form);
}

function removeEditor(row) {
  const inUse = editing.code === "host_in_use";
  const children = [];
  if (inUse) {
    children.push(el("div", { class: "config-row-error", text: editing.error }));
    const lines = dependentLines(editing.dependents);
    if (lines.length) {
      children.push(el("div", { class: "config-note", text: "These lose their owner route if the entry goes:" }));
      children.push(el("ul", { class: "host-dependents" }, lines.map((line) => el("li", { class: "mono", text: line }))));
    }
  } else {
    children.push(el("div", {
      class: "config-note",
      text: `Remove ${row.name} (${row.machine_id}) from the host file? Nothing on that host changes.`,
    }));
  }
  children.push(el("div", { class: "config-editor-foot" }, [
    el("span", { class: "config-note", text: editing.pending ? "Removing…" : "" }),
    el("span", { class: "config-editor-actions" }, [
      inUse
        ? actionButton("Force remove", () => submitRemove(row, true), "primary host-confirm danger")
        : actionButton("Remove", () => submitRemove(row, false), "primary host-confirm"),
      actionButton("Cancel", closeEditor, "ghost"),
    ]),
  ]));
  if (!inUse) children.push(editorError());
  const confirm = el("div", { class: "config-editor host-editor host-remove-confirm" }, children);
  confirm.setAttribute("role", "group");
  confirm.setAttribute("aria-label", `Remove ${row.name}`);
  return cancelOnEscape(confirm);
}

function dependentLines(dependents) {
  if (!dependents) return [];
  return [
    ...(dependents.replica_checkouts || []).map((checkout) => `replica checkout ${checkout.workspace_name} (${checkout.workspace_id}) at ${checkout.repo_root}`),
    ...(dependents.pull_drains || []).map((drain) => `${drain.state} pull drain ${drain.run_id} in ${drain.workspace_id}`),
  ];
}

function editorError() {
  return editing?.error ? el("div", { class: "config-row-error", text: editing.error }) : null;
}

function textInput(cls, label, placeholder, draft, field) {
  const input = el("input", { class: `config-input mono ${cls}` });
  input.type = "text";
  input.placeholder = placeholder;
  input.value = draft[field] ?? "";
  input.disabled = Boolean(editing?.pending);
  input.setAttribute("autocomplete", "off");
  input.setAttribute("spellcheck", "false");
  const sync = () => { draft[field] = input.value; };
  input.addEventListener("input", sync);
  const node = el("label", { class: "host-field" }, [el("span", { class: "config-note", text: label }), input]);
  return { label: node, read: () => (sync(), input.value.trim()) };
}

function actionButton(label, onClick, kind = "") {
  const button = el("button", { class: `config-action ${kind}`.trim(), text: label });
  button.type = "button";
  button.disabled = Boolean(editing?.pending);
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    onClick();
  });
  return button;
}

function submitButton(label) {
  const button = el("button", { class: "config-action primary host-submit", text: label });
  button.type = "submit";
  button.disabled = Boolean(editing?.pending);
  return button;
}

// ---------------------------------------------------------------- sessions

function openEditor(next) {
  if (editing?.pending) return;
  editing = { error: null, code: null, dependents: null, pending: false, draft: {}, ...next };
  notice = "";
  render();
  focusEditor();
}

function focusEditor() {
  const body = $("config-body");
  const target = body?.querySelector(".host-editor input") || body?.querySelector(".host-editor .host-confirm");
  target?.focus?.();
}

function closeEditor() {
  const closing = editing;
  editing = null;
  render();
  returnFocus(closing);
}

/// After an editor closes, focus goes back to the button that opened it, or
/// to Add host when that row is gone.
function returnFocus(session) {
  if (!session) return;
  const body = $("config-body");
  if (session.kind !== "add") {
    for (const row of body?.querySelectorAll(".host-row") || []) {
      if (row.dataset.key !== session.id) continue;
      row.querySelector(session.kind === "rename" ? ".host-rename" : ".host-remove")?.focus();
      return;
    }
  }
  body?.querySelector(".host-add-open")?.focus();
}

function cancelOnEscape(node) {
  node.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || editing?.pending) return;
    event.stopPropagation();
    closeEditor();
  });
  return node;
}

async function mutate(session, request, onSuccess) {
  if (session.pending) return;
  session.pending = true;
  session.error = null;
  render();
  try {
    const response = await request();
    if (editing !== session) return;
    editing = null;
    notice = onSuccess(response);
    if (response?.host) remember([response.host]);
    await fetchAndRenderHosts({ probe: false });
    returnFocus(session);
  } catch (error) {
    if (editing !== session) return;
    session.pending = false;
    session.error = error.message;
    session.code = error.code || null;
    session.dependents = error.body?.dependents || null;
    render();
    focusEditor();
  }
}

function submitAdd(ssh, name) {
  const session = editing;
  if (!session || session.kind !== "add") return;
  if (!ssh) {
    session.error = "Enter an SSH alias or user@host.";
    render();
    focusEditor();
    return;
  }
  const body = name ? { ssh, name } : { ssh };
  mutate(session, () => requestJson("/api/hosts", "POST", body), (change) => {
    const migrated = (change.migrated || []).length;
    const verb = change.action === "migrated" ? "Migrated" : "Added";
    return `${verb} ${change.entry.name} (${change.entry.machine_id}, prefix ${change.entry.task_prefix}).${migrated ? ` Migrated ${migrated} legacy host${migrated === 1 ? "" : "s"}.` : ""}`;
  });
}

function submitRename(row, name) {
  const session = editing;
  if (!session || session.kind !== "rename") return;
  if (!name) {
    session.error = "Enter a name.";
    render();
    focusEditor();
    return;
  }
  mutate(session, () => requestJson(`/api/hosts/${encodeURIComponent(row.machine_id)}`, "PATCH", { name }), (change) =>
    `Renamed ${change.previous_name} to ${change.entry.name}.`);
}

function submitRemove(row, force) {
  const session = editing;
  if (!session || session.kind !== "remove") return;
  const path = `/api/hosts/${encodeURIComponent(row.machine_id)}${force ? "?force=true" : ""}`;
  mutate(session, () => requestJson(path, "DELETE"), (change) => {
    const orphaned = dependentLines(change.orphaned);
    return `Removed ${change.entry.name} (${change.entry.machine_id}).${orphaned.length ? ` These lost their owner route: ${orphaned.join("; ")}.` : ""}`;
  });
}
