// Observe › Fleet: serving-host registry, probes and guarded registry mutations.
import { captureFocus, el, fetchJson, getHost, onWorkspaceChange, pinnedInfoHint, setRegisteredHosts, requestJson, requestPanel } from './common.js';
import { fetchHostResourcePayloadFor } from './host-resources.js';
import { fleetButton, fleetFacts, fleetHostCards, fleetDrainSection, fleetProviderSection } from './fleet-view.js';

const $ = (id) => document.getElementById(id);

const NO_VALUE = "–";
/// What only a probe reports; a `?probe=false` row borrows these from the
/// last probe of the same host at the same SSH target.
const LIVE_FIELDS = ["reachable", "error", "binary_version", "protocol_fingerprint", "skew", "skew_fields", "workspaces", "task_prefix", "host_os"];

let selectScope = () => {};
let selectedHost = null;
let dialog = null;
let noticeTimer = null;

export function initFleetView(context) {
  selectScope = context.selectScope;
  onWorkspaceChange(resetHostsView);
  // Expire a displayed sample even while the next network read is pending.
  setInterval(() => {
    if (document.hidden || !document.querySelector('.tab-pane[data-tab="fleet"].active')) return;
    let expired = false;
    for (const reading of health.values()) {
      const sample = reading.resources;
      if (!sample || sample.stale || !Number.isFinite(sample.max_age_seconds)) continue;
      const age = sample.sample_age_seconds + (Date.now() - reading.receivedAt) / 1000;
      if (age <= sample.max_age_seconds) continue;
      reading.resources = { ...sample, stale: true, verdict_unknown: true, sample_age_seconds: age, reason: 'Resource sample expired; use Reload to request current readings.' };
      expired = true;
    }
    if (expired && !editing) render();
  }, 5000);
}

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
  selectedHost = null;
  if (dialog?.open) dialog.close();
  clearTimeout(noticeTimer);
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
      "fleet-body",
      "hosts",
      () => fetchJson("/api/hosts?probe=false"),
      (payload) => {
        if (token !== epoch) return;
        if (checking) {
          checking.targets = new Map((payload.hosts || []).filter((row) => !row.local).map((row) => [row.machine_id, row.ssh]));
          probeNext = false;
        }
        lastPayload = payload;
        setRegisteredHosts(payload && payload.hosts);
        render();
      },
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
  const body = $("fleet-body");
  if (!body || !lastPayload) return;
  if (!editable()) editing = null;
  const restoreBody = captureFocus(body);
  const restoreControls = captureFocus($("fleet-controls"));
  renderControls(lastPayload);
  body.replaceChildren(...view(lastPayload));
  restoreBody();
  restoreControls();
  renderDialog();
}

function renderControls(payload) {
  const local = (payload.hosts || []).find(row => row.local);
  const add = editable() ? fleetButton("Add host", "Open a dialog to register a remote host on the serving host", () => openEditor({ kind: "add", id: null }), "host-add-open") : null;
  const reload = fleetButton("Reload", pendingProbe ? "Wait for the current host probes to finish" : editing?.pending ? "Wait for the registry change to finish" : "Reread the serving registry and probe every registered host", () => {
    if (editing?.pending) return;
    editing = null;
    notice = "";
    fetchAndRenderHosts({ probe: true }).catch(error => console.error(error));
  }, "fleet-reload");
  reload.disabled = Boolean(pendingProbe || editing?.pending);
  $("fleet-controls")?.replaceChildren(
    el("h1", { text: "Fleet" }),
    pinnedInfoHint(`Registry on ${local?.name || "the serving host"}: ${payload.host_file || "hosts.toml"}. Add, Rename and Remove write only this registry. Selected host: ${getHost() || local?.name || "local"}; the workspace selection does not filter Fleet.`),
    el("div", { class: "fleet-page-actions" }, [add, reload]),
  );
}

function editable() {
  return lastPayload?.host_edit?.authorized === true;
}

function view(payload) {
  const hosts = (payload.hosts || []).map(withLiveFields);
  const nodes = [];
  if (payload.load_error) nodes.push(loadErrorBanner(payload));
  if (!editable()) nodes.push(el("div", { class: "settings-callout warn", text: `Read-only: ${payload.host_edit?.reason || "registry editing requires operator capability"}` }));
  if (notice) nodes.push(el("div", { class: "host-notice", role: "status" }, [notice, fleetButton("Dismiss", "Dismiss this registry result", () => { notice = ""; render(); })]));
  nodes.push(fleetHostCards(hosts, health, id => { selectedHost = id; renderDialog(); }));
  nodes.push(fleetDrainSection(hosts, health, selectScope));
  nodes.push(fleetProviderSection(hosts, health));
  return nodes;
}

function renderDialog() {
  const row = (lastPayload?.hosts || []).map(withLiveFields).find(row => row.machine_id === selectedHost);
  if (!editing && !row) { selectedHost = null; if (dialog?.open) dialog.close(); return; }
  if (!dialog) {
    dialog = el("dialog", { class: "fleet-dialog", id: "fleet-dialog", "aria-labelledby": "fleet-dialog-title" });
    document.body.append(dialog);
    dialog.addEventListener("cancel", event => { event.preventDefault(); if (!editing?.pending) closeFleetDialog(); });
  }
  const restore = captureFocus(dialog);
  const openDetails = new Set([...dialog.querySelectorAll("details[open]")].map(node => node.querySelector("summary")?.textContent));
  const local = lastPayload.hosts.find(host => host.local);
  const content = el("div", { class: "fleet-dialog-body" });
  const title = editing ? `${editing.kind === "add" ? "Add host" : editing.kind === "rename" ? "Rename" : "Remove"}${row ? ` · ${row.name}` : ""}` : row.name;
  if (editing) {
    content.append(el("span", { class: "muted", text: `Registry on ${local?.name || "the serving host"}` }));
    content.append(editing.kind === "add" ? addSection() : editing.kind === "rename" ? renameEditor(row) : removeEditor(row));
  } else {
    const names = new Map(lastPayload.hosts.map(host => [host.machine_id, host.name]));
    content.append(el("section", { class: "fleet-dialog-section" }, [
      el("h3", { text: "Facts" }),
      fleetFacts([
        ["Machine ID", row.machine_id], ["SSH", row.ssh || "Local"], ["Task prefix", row.task_prefix || "Unknown"],
        ["Orbit version", row.binary_version || "Unknown"], ["Reachability", reachableCell(row)],
        ["Protocol", row.protocol_fingerprint || "Not probed"], ["Compatibility", skewCell(row)],
        ["Workspaces", workspacesCell(row, names)],
      ]),
    ]));
    if (row.error) {
      const cause = hostErrorCause(row.error);
      const items = [
        el("h3", { text: "Probe failure" }),
        el("p", { class: "fleet-dialog-error", text: hostError(row) }),
      ];
      if (cause) {
        items.push(el("p", { class: "fleet-dialog-cause", text: `Cause: ${cause}` }));
      }
      content.append(el("section", { class: "fleet-dialog-section" }, items));
    }
    const reading = health.get(row.machine_id);
    const warnings = (reading?.drains || []).filter(drain => drain.pilotWarning || drain.error);
    if (warnings.length) {
      content.append(el("section", { class: "fleet-dialog-section" }, [
        el("h3", { text: "Drain diagnostics" }),
        ...warnings.map(drain => {
          const detail = drain.pilotWarning || (typeof drain.error === "string" ? drain.error : drain.error?.message || "drain unavailable");
          return el("p", { text: `${drain.name}: ${detail}` });
        }),
      ]));
    }
    if (!row.local && editable()) {
      content.append(el("section", { class: "fleet-dialog-section" }, [
        el("h3", { text: `Registry on ${local?.name || "the serving host"}` }),
        actionsCell(row),
      ]));
    }
  }
  dialog.replaceChildren(el("header", { class: "fleet-dialog-head" }, [el("h2", { id: "fleet-dialog-title", text: title }), fleetButton("Close", "Close diagnostics and discard unsaved edits", closeFleetDialog)]), content);
  for (const detail of dialog.querySelectorAll("details")) detail.open = openDetails.has(detail.querySelector("summary")?.textContent);
  if (!dialog.open) dialog.showModal();
  restore();
}

function closeFleetDialog() {
  if (editing?.pending) return;
  const id = selectedHost;
  selectedHost = null;
  editing = null;
  dialog?.close();
  const card = [...document.querySelectorAll('.fleet-host-card')].find(node => node.dataset.key === id);
  (card || document.querySelector('.host-add-open'))?.focus();
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

function hostError(row) {
  if (!row.error) return "Unknown error occurred.";
  if (row.error.code === "unreachable_destination") {
    return `${row.name} could not be reached. Check the network connection and SSH access to ${row.ssh || row.name}, then use Reload to try again.`;
  }
  if (row.error.code === "process_timeout") {
    return `Probe timed out while connecting to ${row.name}. Check the network connection and SSH access to ${row.ssh || row.name}, then use Reload to try again.`;
  }
  const msg = typeof row.error.message === "string" ? row.error.message.split("\n")[0].trim() : "";
  if (msg && !msg.toLowerCase().includes("ssh:") && !msg.toLowerCase().includes("fatal:")) {
    return `${row.name}: ${msg}`;
  }
  return `${row.name} encountered an error (${row.error.code || "unknown"}). Use Reload to try again.`;
}

function hostErrorCause(error) {
  if (!error) return null;
  switch (error.code) {
    case "unreachable_destination":
      return "SSH connection timed out";
    case "process_timeout":
      return "Connection timed out";
    case "host_identity_mismatch":
      return "Machine ID mismatch";
    case "host_too_old":
      return "Incompatible Orbit version";
    case "unknown_host":
      return "Host is not registered";
    default:
      return error.code ? error.code.replace(/_/g, " ") : null;
  }
}

async function loadHealth() {
  const token = ++healthEpoch;
  const payload = lastPayload;
  if (!payload || token !== healthEpoch) return;
  const rows = (payload.hosts || []).map(withLiveFields);
  const entries = [];
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(3, rows.length) }, async () => {
    while (next < rows.length) {
      const row = rows[next++];
      entries.push([row.machine_id, await readHealth(row)]);
    }
  }));
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
  return { kind: "ready", receivedAt: Date.now(), ...resources, drains, workspaceCount: workspaces.length };
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
      ownerStopped: item?.owner_admissions_stopped,
      runId: item?.drain_run_id,
      pullRunId: item?.pull_drain_run_id,
      work: item?.owner_running_tasks,
      pullWork: item?.pull_running_tasks,
      queued: item?.queued_drains || [],
      holds: item?.holds || [],
      topWait: item?.top_wait_reason,
      pilotWarning: item?.task_pilot_warning,
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
    drain.ownerStopped = capacity.admissions_stopped === true;
    drain.runId = capacity.drain_run_id;
    drain.pullRunId = capacity.pull_drain_run_id;
    drain.work = capacity.running_admitted_workers;
    drain.pullWork = capacity.wrapper_leaf_runs;
    drain.queued = capacity.queued_drains || [];
    drain.holds = [capacity.admissions_stopped || drain.pullStopped ? "admissions stopped" : null, capacity.resource_throttle?.reason].filter(Boolean);
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
  const ssh = textInput("host-input-ssh", "SSH target", "alias or user@host", editing.draft, "ssh");
  const name = textInput("host-input-name", "Name (optional)", "defaults to the host's machine.name", editing.draft, "name");
  const form = el("form", { class: "config-editor host-editor" }, [
    el("div", { class: "config-editor-head" }, [
      el("span", { class: "settings-section-title", text: "Add a host" }),
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
  const input = el("input", { class: `settings-input mono ${cls}` });
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
  const button = el("button", { class: `settings-btn ${kind}`.trim(), text: label });
  button.type = "button";
  const help = { Rename: "Open the registry name editor", Remove: "Review removal from the serving registry", Cancel: "Discard this registry edit", "Force remove": "Remove this registration despite its dependent replicas and drains" }[label] || label;
  const description = editing?.pending ? "Wait for the registry change to finish" : help;
  button.setAttribute("aria-label", label);
  if (description && description !== label) {
    button.setAttribute("aria-description", description);
  }
  button.disabled = Boolean(editing?.pending);
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    onClick();
  });
  return button;
}

function submitButton(label) {
  const button = el("button", { class: "settings-btn primary host-submit", text: label });
  button.type = "submit";
  const description = editing?.pending ? "Wait for the registry change to finish" : "Validate and save this change to the serving registry";
  button.setAttribute("aria-label", label);
  if (description && description !== label) {
    button.setAttribute("aria-description", description);
  }
  button.disabled = Boolean(editing?.pending);
  return button;
}

// ---------------------------------------------------------------- sessions

function openEditor(next) {
  if (editing?.pending) return;
  if (next.id) selectedHost = next.id;
  editing = { error: null, code: null, dependents: null, pending: false, draft: {}, ...next };
  notice = "";
  render();
  focusEditor();
}

function focusEditor() {
  const body = dialog;
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
  if (dialog?.open) {
    dialog.querySelector(session.kind === "rename" ? ".host-rename" : ".host-remove")?.focus();
    return;
  }
  $("fleet-controls")?.querySelector(".host-add-open")?.focus();
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
  let response;
  try {
    response = await request();
  } catch (error) {
    if (editing !== session) return;
    session.pending = false;
    session.error = error.message;
    session.code = error.code || null;
    session.dependents = error.body?.dependents || null;
    render();
    focusEditor();
    return;
  }
  if (editing !== session) return;
  editing = null;
  notice = onSuccess(response);
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => { notice = ""; render(); }, 10000);
  if (response?.host) remember([response.host]);
  try {
    await fetchAndRenderHosts({ probe: false });
  } catch (error) {
    // The mutation already succeeded; a refresh failure must not be
    // swallowed as if the edit were still open or had failed.
    console.error("Failed to refresh hosts after mutation", error);
    notice = `${notice} Refresh failed: ${error.message}. Use Reload to try again.`;
    render();
  }
  returnFocus(session);
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
    return `Added ${change.entry.name} (${change.entry.machine_id}, prefix ${change.entry.task_prefix}).`;
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
