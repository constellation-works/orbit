// Settings › Global file [ORB-15432]: what `~/.orbit/config.toml` sets, and
// which of those keys actually reach the workspace selected in the sidebar.
//
// A machine card leads with the `[machine]` identity: the id and task prefix
// are read-only, the name renames. Three reach cards count the keys the file
// sets as in effect, overridden by the workspace file (and how many of those
// with the same value) or not inherited, and filter the table. The table
// groups the other keys by TOML table; each row says what the key does in the
// workspace. The inline editor previews `orbit config set --global …` and
// says whether the change reaches the workspace or only other workspaces.
//
// Every fact is the payload's: `reach` on each row of
// `GET /api/config/file?scope=global`, plus the workspace file's own values
// (`?scope=workspace`, best effort) to name what it sets for a key it does not
// inherit. A row whose reach the payload omits reads "not reported". Nothing
// is inferred from a key name. Writes use the PUT and DELETE key routes with
// `scope: "global"`, then the view re-reads the file.

import { copyWithFeedback, el, getWorkspace, requestJson } from './common.js';
import { button, cliCommand, display, draftText, draftValue, valueField, valueTokens } from './config-effective.js';

/// The `[machine]` identity `orbit init` writes; the machine card shows these
/// instead of table rows.
const IDENTITY = [
  { key: "machine.id", label: "Machine id" },
  { key: "machine.name", label: "Name" },
  { key: "machine.task_prefix", label: "Task prefix" },
];
const IDENTITY_KEYS = new Set(IDENTITY.map((entry) => entry.key));
const REACH = {
  "in-effect": { id: "effect", label: "In effect" },
  overridden: { id: "overridden", label: "Overridden" },
  "not-inherited": { id: "noinh", label: "Not inherited" },
};
const UNREPORTED = { id: "unreported", label: "Not reported" };

// Operator state that outlives a re-render (a poll, a save): the reach
// filter, the open inline edit, rename or Add key draft, and the last
// write's outcome.
let filter = "all";
let draft = null;
let adding = null;
let renaming = null;
let pending = false;
let status = null;
let lastRender = null;

/// Forget the filter and drafts; reach is relative to another workspace now.
export function resetGlobalFile() {
  filter = "all";
  draft = null;
  adding = null;
  renaming = null;
  pending = false;
  status = null;
}

/// Render the Global file view into `body`. `payload` is the global file
/// view with `workspace_file` (the workspace file view, or null) beside it;
/// `context` is the one Settings passes every view.
export function renderGlobalFile(body, payload, context) {
  lastRender = { body, payload, context };
  const model = buildModel(payload);
  const workspace = getWorkspace();
  if (draft && (!model.byKey.has(draft.key) || draft.workspace !== workspace)) draft = null;
  if (adding && adding.workspace !== workspace) adding = null;
  if (renaming && renaming.workspace !== workspace) renaming = null;
  const canEdit = context.canEdit;
  if (!canEdit) {
    draft = null;
    adding = null;
    renaming = null;
  }

  const root = el("div", { class: "cfx cwf cgf" });
  root.appendChild(header(payload, model, canEdit));
  if (!canEdit) {
    root.appendChild(el("p", { class: "cfx-readonly", text: `Read-only here: ${context.refusal || "this session cannot change configuration"}.` }));
  }
  root.appendChild(machineCard(model, canEdit));
  root.appendChild(reachSection(model));
  root.appendChild(el("div", { class: "cwf-main" }, [
    keysPanel(payload, model, canEdit),
    sidebar(payload, model),
  ]));
  body.appendChild(root);
}

export function globalFileCount(payload) {
  const n = buildModel(payload).rows.length;
  return `${n} ${n === 1 ? "key" : "keys"} in file`;
}

function rerender() {
  if (lastRender) lastRender.context.render(lastRender.payload);
}

function workspaceName() {
  return getWorkspace() || "this workspace";
}

function plural(n, one, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

// ------------------------------------------------------------------ model

/// One key this file sets, with what it does in the bound workspace.
function describe(row, workspaceValues) {
  const reach = row.reach && typeof row.reach === "object" ? row.reach : null;
  const kind = REACH[reach?.state] || UNREPORTED;
  const table = row.key.includes(".") ? row.key.slice(0, row.key.lastIndexOf(".")) : row.key;
  return {
    row, reach, table,
    reachId: kind.id,
    reachLabel: kind.label,
    name: row.key.slice(table.length + 1) || row.key,
    // A not-inherited key may still be set by the workspace file; that value,
    // when the workspace file was readable, is what the workspace runs.
    workspaceValue: workspaceValues?.has(row.key) ? { value: workspaceValues.get(row.key) } : null,
    workspaceRead: workspaceValues != null,
    note: row.equals?.built_in === true ? "Same as the built-in default." : "",
  };
}

function buildModel(payload) {
  const workspaceValues = workspaceFileValues(payload.workspace_file);
  const rows = [];
  const byKey = new Map();
  const tables = new Map();
  const identity = new Map();
  const candidates = [];
  const unsetSections = [];
  let total = 0;
  for (const section of payload.sections || []) {
    if (section.kind === "crews") continue;
    const keys = section.keys || [];
    total += keys.length;
    const set = keys.filter((row) => row.state === "set");
    if (keys.length && !set.length) unsetSections.push(section);
    for (const row of keys) {
      if (IDENTITY_KEYS.has(row.key)) identity.set(row.key, row);
      if (row.state !== "set" && row.settable !== false && !IDENTITY_KEYS.has(row.key)) candidates.push({ section, row });
    }
    for (const row of set) {
      const info = describe(row, workspaceValues);
      rows.push(info);
      byKey.set(row.key, info);
      if (IDENTITY_KEYS.has(row.key)) continue;
      if (!tables.has(info.table)) tables.set(info.table, []);
      tables.get(info.table).push(info);
    }
  }
  const counts = { effect: 0, overridden: 0, same: 0, noinh: 0, unreported: 0, identity: 0 };
  for (const info of rows) {
    counts[info.reachId] += 1;
    if (info.reachId === "overridden" && info.reach.equals_global === true) counts.same += 1;
    if (info.reachId === "effect" && IDENTITY_KEYS.has(info.row.key)) counts.identity += 1;
  }
  const listed = rows.filter((info) => !IDENTITY_KEYS.has(info.row.key)).length;
  return { rows, byKey, tables, identity, counts, listed, candidates, unsetSections, total, workspaceValues };
}

/// The values the workspace file sets, by key, or null when it was not read.
function workspaceFileValues(file) {
  if (!file || !Array.isArray(file.sections)) return null;
  const values = new Map();
  for (const section of file.sections) {
    for (const row of section.keys || []) if (row.state === "set") values.set(row.key, row.value);
  }
  return values;
}

function visible(info) {
  return !IDENTITY_KEYS.has(info.row.key) && (filter === "all" || info.reachId === filter);
}

function setFilter(next) {
  filter = filter === next ? "all" : next;
  rerender();
}

// ----------------------------------------------------------------- header

function header(payload, model, canEdit) {
  const file = payload.layers?.global || {};
  const path = file.path || "";
  const copy = el("button", { class: "cfx-btn quiet cwf-copy", type: "button", text: "Copy path", "aria-label": "Copy the file path" });
  copy.addEventListener("click", () => copyWithFeedback(copy, path));
  const machine = model.identity.get("machine.name")?.value;
  const facts = file.exists === false
    ? "Read by every workspace on this machine · not created yet"
    : `Read by every workspace on this machine · ${plural(model.rows.length, "key")}`;
  const actions = el("div", { class: "cwf-head-actions" }, [
    button("Reload", () => {
      draft = null;
      adding = null;
      renaming = null;
      lastRender?.context.reload();
    }),
  ]);
  if (canEdit) {
    const add = button("+ Add key", () => startAdd(model), "cfx-btn primary");
    add.disabled = pending || !model.candidates.length;
    if (!model.candidates.length) add.title = "Every registered settable key is already set in the global file";
    actions.appendChild(add);
  }
  const node = el("div", { class: "cwf-head" }, [
    el("div", { class: "cwf-head-text" }, [
      el("span", { class: "cfx-section-note", text: `Settings · ${machine || "this machine"}` }),
      el("h2", { class: "cwf-title" }, ["Global file ", el("span", { class: "cfx-pill global", text: "machine-wide · under every workspace file" })]),
      el("div", { class: "cwf-path" }, [
        el("code", { class: "mono", text: path || "path not reported", title: path }),
        path ? copy : null,
      ]),
      el("span", { class: "cfx-section-note cgf-facts", text: facts }),
    ]),
    actions,
  ]);
  node.dataset.key = "cgf-head";
  return node;
}

// ----------------------------------------------------------- machine card

function machineCard(model, canEdit) {
  const title = el("h2", { class: "cfx-h2", text: "This machine " });
  title.id = "cgf-machine-title";
  title.appendChild(el("span", { class: "cfx-section-note mono", text: "[machine]" }));
  const ids = el("div", { class: "cgf-ids" });
  for (const { key, label } of IDENTITY) {
    const row = model.identity.get(key);
    const value = row?.state === "set" ? display(row.value) : "not set";
    const id = el("div", { class: "cgf-id" }, [
      el("span", { class: "cfx-eyebrow", text: label }),
    ]);
    id.dataset.key = key;
    const renamable = canEdit && row && row.settable !== false;
    const line = el("span", { class: "cgf-id-line" }, [el("span", { class: "mono cgf-id-value", text: value })]);
    if (renamable && renaming?.key !== key) {
      const rename = button("Rename", () => startRename(row), "cfx-btn quiet");
      rename.disabled = pending;
      rename.setAttribute("aria-label", `Rename this machine (${key})`);
      line.appendChild(rename);
    }
    id.appendChild(line);
    const hint = [row?.description, row?.settable === false ? "not settable" : ""].filter(Boolean).join(" · ");
    const hintNode = el("span", { class: "cfx-hint", text: hint });
    if (row?.read_only_reason) hintNode.title = row.read_only_reason;
    id.appendChild(hintNode);
    if (renamable && renaming?.key === key) id.appendChild(renameForm(row));
    ids.appendChild(id);
  }
  const node = el("section", { class: "cfx-card cgf-machine", "aria-labelledby": title.id }, [
    el("div", { class: "cfx-section-head" }, [title, el("span", { class: "cfx-section-note", text: "machine.* lives only in this file" })]),
    ids,
  ]);
  node.dataset.key = "cgf-machine";
  return node;
}

function startRename(row) {
  if (pending) return;
  renaming = { key: row.key, workspace: getWorkspace(), text: draftText(row), scope: "global", error: null };
  draft = null;
  adding = null;
  status = null;
  rerender();
  document.getElementById("cgf-rename-value")?.focus();
}

function renameForm(row) {
  const form = el("form", { class: "cgf-rename" });
  form.noValidate = true;
  const field = valueField(row, renaming, { id: "cgf-rename-value", disabled: pending });
  const label = el("label", { class: "cfx-label", text: `New ${row.key}` });
  label.htmlFor = "cgf-rename-value";
  const cli = el("code", { class: "cfx-cli mono cwf-cli", text: cliCommand(row, renaming) });
  field.onInput(() => {
    cli.textContent = cliCommand(row, renaming);
  });
  const save = el("button", { class: "cfx-btn primary", type: "submit", text: pending ? "Saving…" : "Rename" });
  save.disabled = pending;
  const cancel = button("Cancel", () => {
    renaming = null;
    rerender();
  }, "cfx-btn quiet");
  cancel.disabled = pending;
  form.append(
    el("div", { class: "cwf-field" }, [label, field.node]),
    el("div", { class: "cfx-actions" }, [cancel, save]),
    el("div", { class: "cfx-cli-wrap cwf-cli-wrap" }, [el("span", { class: "cfx-hint", text: "Same as running" }), cli]),
  );
  if (renaming.error) form.appendChild(el("p", { class: "cfx-notice error", role: "alert", text: renaming.error }));
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    saveKey(row, renaming, `Renamed this machine to ${renaming.text.trim()}.`);
  });
  return cancelOnEscape(form, () => {
    renaming = null;
  });
}

// ------------------------------------------------------------ reach cards

function reachSection(model) {
  const ws = workspaceName();
  const { counts } = model;
  const cards = [
    {
      id: "effect", label: `In effect for ${ws}`, n: counts.effect,
      hint: counts.identity ? `Includes the ${plural(counts.identity, "machine identity key")}.` : `Editing one changes what ${ws} runs.`,
    },
    {
      id: "overridden", label: "Overridden by its workspace file", n: counts.overridden,
      hint: counts.overridden ? `${counts.same} of them with the same value.` : "Its workspace file sets none of these keys.",
    },
    {
      id: "noinh", label: "Not inherited", n: counts.noinh,
      hint: "Keys it takes from its own workspace file or the built-in default.",
    },
  ];
  const row = el("div", { class: "cwf-cards" });
  for (const card of cards) {
    const on = filter === card.id;
    const node = el("button", { class: `cwf-card cgf-card ${card.id}`, type: "button", "aria-pressed": String(on) }, [
      el("span", { class: "cwf-card-label" }, [el("span", { class: "cfx-dot", "aria-hidden": "true" }), card.label]),
      el("span", { class: "cwf-card-n mono", text: String(card.n) }),
      el("span", { class: "cwf-card-hint", text: card.hint }),
    ]);
    node.dataset.reach = card.id;
    node.title = on ? "Show every key in this file again" : `Show only the keys ${card.label.toLowerCase()}`;
    node.addEventListener("click", () => setFilter(card.id));
    row.appendChild(node);
  }
  const never = counts.overridden + counts.noinh;
  const sentences = [];
  if (never) {
    sentences.push(`${never} of the ${plural(model.rows.length, "key")} here never reach ${ws}: its workspace file sets them first, or it does not inherit them while it has a workspace file. Editing those keys changes other workspaces only.`);
  } else if (model.rows.length && !counts.unreported) {
    sentences.push(`Every key here reaches ${ws}.`);
  }
  if (counts.unreported) {
    sentences.push(`The reach of ${plural(counts.unreported, "key")} is not reported: the global and workspace files did not load together.`);
  }
  const title = el("h2", { class: "cfx-eyebrow", text: `What this file sets for ${ws}` });
  const node = el("section", { class: "cwf-summary cgf-reach", "aria-label": `What this file sets for ${ws}` }, [
    title,
    row,
    sentences.length ? el("p", { class: "cgf-reach-sentence", text: sentences.join(" ") }) : null,
  ]);
  node.dataset.key = "cgf-reach";
  return node;
}

// -------------------------------------------------------------- key table

function keysPanel(payload, model, canEdit) {
  const panel = el("section", { class: "cfx-card cwf-keys", "aria-labelledby": "cgf-keys-title" });
  const shown = model.rows.filter(visible).length;
  const title = el("h2", { class: "cfx-h2", text: "Keys in this file " });
  title.id = "cgf-keys-title";
  title.appendChild(el("span", {
    class: "cfx-section-note mono cgf-shown",
    text: filter === "all" ? `${plural(shown, "key")} outside the machine identity` : `${shown} shown`,
  }));
  const toolbar = el("div", { class: "cfx-toolbar" }, [title]);
  if (filter !== "all") toolbar.appendChild(button(`Show all ${model.listed}`, () => setFilter("all"), "cfx-btn quiet"));
  toolbar.dataset.key = "cgf-toolbar";
  panel.appendChild(toolbar);
  if (canEdit && adding) panel.appendChild(addForm(model));

  panel.appendChild(el("div", { class: "cgf-colhead", "aria-hidden": "true" }, [
    el("span", { text: "Key" }),
    el("span", { text: "Value" }),
    el("span", { text: `In ${workspaceName()}` }),
    el("span"),
  ]));
  const groups = el("div", { class: "cwf-groups" });
  for (const [table, infos] of model.tables) {
    const rows = infos.filter(visible);
    if (!rows.length) continue;
    const group = el("div", { class: "cwf-group" }, [
      el("div", { class: "cwf-group-head" }, [
        el("h3", { class: "cwf-table mono", text: `[${table}]` }),
        el("span", { class: "cfx-section-note", text: plural(rows.length, "key") }),
      ]),
    ]);
    group.dataset.table = table;
    for (const info of rows) group.appendChild(keyRow(info, canEdit));
    groups.appendChild(group);
  }
  if (!shown) {
    const empty = model.listed
      ? "No key in this file has that reach."
      : payload.layers?.global?.exists === false
        ? "There is no global config.toml yet; Add key creates it."
        : "This file sets no key outside the machine identity; every workspace uses its own file or the built-in defaults.";
    groups.appendChild(el("p", { class: "cfx-none", text: empty }));
  }
  panel.appendChild(groups);
  panel.appendChild(statusNode());
  return panel;
}

function keyRow(info, canEdit) {
  const { row } = info;
  const editable = canEdit && row.settable !== false;
  const editing = draft?.key === row.key;
  const node = el("div", { class: `cgf-row${editing ? " editing" : ""}` });
  node.dataset.key = row.key;
  node.dataset.reach = info.reachId;
  node.appendChild(el("span", { class: "cfx-key mono cwf-name", text: info.name, title: row.key }));
  node.appendChild(el("span", { class: "cwf-value" }, [valueTokens(row.value)]));
  node.appendChild(reachCell(info));
  const actions = el("span", { class: "cwf-actions" });
  if (editable) {
    const edit = button(editing ? "Close" : "Edit", () => toggleEdit(info), "cfx-btn quiet");
    edit.setAttribute("aria-expanded", String(editing));
    edit.setAttribute("aria-label", `${editing ? "Close" : "Edit"} ${row.key}`);
    edit.disabled = pending;
    actions.appendChild(edit);
  } else if (row.settable === false) {
    actions.appendChild(el("span", { class: "cfx-status ro", text: "read-only", title: row.read_only_reason || "orbit config set refuses this key" }));
  }
  node.appendChild(actions);
  if (info.note) node.appendChild(el("span", { class: "cgf-note", text: info.note }));
  if (editable && editing) node.appendChild(editForm(info));
  return node;
}

/// What the key does in the bound workspace: in effect; overridden, with the
/// workspace file's value; or not inherited, with what the workspace runs
/// instead when the workspace file was read.
function reachCell(info) {
  let detail = "";
  let tone = info.reachId;
  if (info.reachId === "overridden") {
    const same = info.reach.equals_global === true;
    tone = same ? "same" : "diff";
    detail = ` · workspace file sets ${same ? "the same value" : display(info.reach.workspace_value)}`;
  } else if (info.reachId === "noinh") {
    detail = notInheritedDetail(info);
  } else if (info.reachId === "unreported") {
    detail = " · the global and workspace files did not load together";
  }
  const cell = el("span", { class: `cgf-reach-cell ${tone}` }, [
    el("span", { class: "cfx-dot", "aria-hidden": "true" }),
    el("span", {}, [
      el("span", { class: "cgf-reach-label", text: info.reachLabel }),
      detail ? el("span", { class: "cgf-reach-detail", text: detail }) : null,
    ]),
  ]);
  cell.dataset.reach = info.reachId;
  return cell;
}

function notInheritedDetail(info) {
  if (info.workspaceValue) return ` · workspace file sets ${display(info.workspaceValue.value)}`;
  // The workspace file was read and does not set the key: the built-in
  // default applies there.
  if (info.workspaceRead) {
    const builtIn = info.row.built_in;
    return builtIn?.state === "default" ? ` · built-in default applies: ${display(builtIn.value)}` : " · unset there (no built-in default)";
  }
  return " · its workspace file or the built-in default decides";
}

function statusNode() {
  const node = el("div", { class: `cwf-status${status ? ` ${status.kind}` : ""}`, role: "status" });
  node.dataset.key = "cgf-status";
  if (status) node.appendChild(el("p", { class: `cfx-notice ${status.kind}`, text: status.text }));
  return node;
}

// ------------------------------------------------------------ inline edit

function toggleEdit(info) {
  if (pending) return;
  if (draft?.key === info.row.key) {
    draft = null;
    rerender();
    return;
  }
  draft = { key: info.row.key, workspace: getWorkspace(), text: draftText(info.row), scope: "global", error: null };
  adding = null;
  renaming = null;
  status = null;
  rerender();
  document.getElementById("cgf-edit-value")?.focus();
}

/// Whether a change to this key reaches the bound workspace, said before the
/// operator saves it.
function reachWarning(info, key) {
  const ws = workspaceName();
  if (info?.reachId === "effect") {
    return { kind: "reaches", text: `This changes what ${ws} runs, and every other workspace that does not set ${key}.` };
  }
  if (info?.reachId === "overridden") {
    return { kind: "elsewhere", text: `No effect on ${ws}: its workspace file sets ${key}. Other workspaces that do not set it pick up the change.` };
  }
  if (info?.reachId === "noinh") {
    return { kind: "elsewhere", text: `No effect on ${ws}: it does not inherit ${key} from this file while it has a workspace file. Workspaces without a config file of their own pick up the change.` };
  }
  return null;
}

function editForm(info) {
  const { row } = info;
  const form = el("form", { class: "cwf-editor cgf-editor" });
  form.noValidate = true;
  const field = valueField(row, draft, { id: "cgf-edit-value", disabled: pending });
  const label = el("label", { class: "cfx-label", text: `New global value for ${row.key}` });
  label.htmlFor = "cgf-edit-value";
  const cli = el("code", { class: "cfx-cli mono cwf-cli", text: cliCommand(row, draft) });
  field.onInput(() => {
    cli.textContent = cliCommand(row, draft);
  });
  const save = el("button", { class: "cfx-btn primary", type: "submit", text: pending ? "Saving…" : "Save to global file" });
  save.disabled = pending;
  const cancel = button("Cancel", () => {
    draft = null;
    rerender();
  }, "cfx-btn quiet");
  cancel.disabled = pending;
  const remove = button("Remove from file", () => removeKey(row, draft), "cfx-btn quiet");
  remove.disabled = pending;
  remove.setAttribute("aria-label", `Remove ${row.key} from the global file`);
  form.append(
    el("div", { class: "cwf-field" }, [label, field.node, field.hint ? el("span", { class: "cfx-hint", text: field.hint }) : null]),
    el("div", { class: "cfx-actions" }, [remove, cancel, save]),
  );
  form.appendChild(warningNode(reachWarning(info, row.key)));
  form.appendChild(el("div", { class: "cfx-cli-wrap cwf-cli-wrap" }, [el("span", { class: "cfx-hint", text: "Same as running" }), cli]));
  if (draft.error) form.appendChild(el("p", { class: "cfx-notice error", role: "alert", text: draft.error }));
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    saveKey(row, draft, `Saved ${row.key} to the global file.`);
  });
  return cancelOnEscape(form, () => {
    draft = null;
  });
}

function warningNode(warning) {
  const node = el("p", { class: `cgf-reach-warn${warning ? ` ${warning.kind}` : ""}`, text: warning?.text || "" });
  node.dataset.reach = warning?.kind || "";
  return node;
}

function cancelOnEscape(form, close) {
  form.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || pending) return;
    event.stopPropagation();
    close();
    rerender();
  });
  return form;
}

/// One write to the global file, then a re-read. The outcome lands only on
/// the draft that issued it, and only in the workspace it was opened in.
async function write(session, request, done) {
  if (pending || !lastRender) return;
  const { context } = lastRender;
  pending = true;
  session.error = null;
  rerender();
  try {
    await request();
    pending = false;
    if (session.workspace !== getWorkspace()) return;
    if (draft === session) draft = null;
    if (adding === session) adding = null;
    if (renaming === session) renaming = null;
    status = { kind: "ok", text: done };
    await context.reload();
  } catch (error) {
    pending = false;
    if (session.workspace !== getWorkspace()) return;
    session.error = error.message || String(error);
    rerender();
    // The Save button was disabled while the write was in flight, which drops
    // focus; the refused value is what the operator corrects next.
    const id = session === adding ? "cgf-add-value" : session === renaming ? "cgf-rename-value" : "cgf-edit-value";
    document.getElementById(id)?.focus();
  }
}

async function saveKey(row, session, done) {
  const parsed = draftValue(row, session.text);
  if (parsed.error) {
    session.error = parsed.error;
    rerender();
    return;
  }
  await write(session, () => requestJson(`/api/config/keys/${encodeURIComponent(row.key)}`, "PUT", {
    value: parsed.value,
    scope: "global",
  }), done);
}

async function removeKey(row, session) {
  await write(
    session,
    () => requestJson(`/api/config/keys/${encodeURIComponent(row.key)}?scope=global`, "DELETE"),
    `Removed ${row.key} from the global file.`,
  );
}

// ---------------------------------------------------------------- add key

function startAdd(model) {
  if (pending || !model.candidates.length) return;
  const first = model.candidates[0].row;
  adding = { key: first.key, workspace: getWorkspace(), text: draftText(first), scope: "global", error: null };
  draft = null;
  renaming = null;
  status = null;
  rerender();
  document.getElementById("cgf-add-key")?.focus();
}

function addForm(model) {
  const candidate = model.candidates.find((entry) => entry.row.key === adding.key) || model.candidates[0];
  if (!candidate) return el("p", { class: "cfx-muted", text: "Every registered settable key is already set in the global file." });
  const { row } = candidate;
  adding.key = row.key;
  const form = el("form", { class: "cwf-add" });
  form.noValidate = true;
  form.dataset.key = "cgf-add";

  const picker = el("select", { class: "cfx-input mono" });
  picker.id = "cgf-add-key";
  picker.disabled = pending;
  let group = null;
  for (const entry of model.candidates) {
    if (group?.dataset.section !== entry.section.token) {
      group = el("optgroup");
      group.label = entry.section.title || entry.section.token || "Settings";
      group.dataset.section = entry.section.token || "";
      picker.appendChild(group);
    }
    const option = el("option", { text: entry.row.key });
    option.value = entry.row.key;
    group.appendChild(option);
  }
  picker.value = row.key;
  picker.addEventListener("change", () => {
    const next = model.candidates.find((entry) => entry.row.key === picker.value);
    if (!next) return;
    adding = { ...adding, key: next.row.key, text: draftText(next.row), error: null };
    rerender();
    document.getElementById("cgf-add-key")?.focus();
  });
  const keyLabel = el("label", { class: "cfx-label", text: "Key" });
  keyLabel.htmlFor = "cgf-add-key";

  const field = valueField(row, adding, { id: "cgf-add-value", disabled: pending });
  const valueLabel = el("label", { class: "cfx-label", text: field.label });
  valueLabel.htmlFor = "cgf-add-value";
  const cli = el("code", { class: "cfx-cli mono cwf-cli", text: cliCommand(row, adding) });
  field.onInput(() => {
    cli.textContent = cliCommand(row, adding);
  });
  const submit = el("button", { class: "cfx-btn primary", type: "submit", text: pending ? "Adding…" : "Add to global file" });
  submit.disabled = pending;
  const cancel = button("Cancel", () => {
    adding = null;
    rerender();
  }, "cfx-btn quiet");
  cancel.disabled = pending;
  // A new key has no reach yet; the one thing known up front is that a
  // workspace file setting it keeps the new value away from this workspace.
  const shadowed = model.workspaceValues?.has(row.key)
    ? { kind: "elsewhere", text: `No effect on ${workspaceName()}: its workspace file sets ${row.key}. Other workspaces that do not set it pick up the value.` }
    : null;
  form.append(
    el("div", { class: "cwf-field" }, [keyLabel, picker, row.description ? el("span", { class: "cfx-hint", text: row.description }) : null]),
    el("div", { class: "cwf-field" }, [valueLabel, field.node, field.hint ? el("span", { class: "cfx-hint", text: field.hint }) : null]),
    el("div", { class: "cfx-actions" }, [cancel, submit]),
    warningNode(shadowed),
    el("div", { class: "cfx-cli-wrap cwf-cli-wrap" }, [el("span", { class: "cfx-hint", text: "Same as running" }), cli]),
  );
  if (adding.error) form.appendChild(el("p", { class: "cfx-notice error", role: "alert", text: adding.error }));
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    saveKey(row, adding, `Added ${row.key} to the global file.`);
  });
  return cancelOnEscape(form, () => {
    adding = null;
  });
}

// ---------------------------------------------------------------- sidebar

function sidebar(payload, model) {
  const ws = workspaceName();
  const fact = (term, detail, cls = "") => el("div", { class: `cwf-fact ${cls}`.trim() }, [el("dt", { text: term }), el("dd", {}, detail)]);
  const about = el("dl", { class: "cwf-facts" }, [
    fact("Scope", ["Every workspace on this machine reads it, under its own workspace file."]),
    fact("Written by", [el("code", { class: "mono", text: "orbit config set --global" }), " and Settings edits aimed at the global file. ", el("code", { class: "mono", text: "orbit init" }), " writes [machine]."]),
    fact("Precedence", ["Beats the built-in defaults; loses to any workspace file that sets the same key."]),
  ]);
  if (payload.layers?.execution_not_inherited) {
    about.appendChild(fact("execution.*", [`While ${ws} has a workspace file, it takes no execution key from this file.`], "warn"));
  }
  const crewTables = Array.isArray(payload.crew_tables) ? payload.crew_tables : null;
  if (crewTables?.length) {
    const link = el("a", { text: "Crews" });
    link.href = "#config/crews";
    const crews = fact("Crews", [`Defines ${plural(crewTables.length, "crew table")}: ${crewTables.join(", ")}. Edit them under `, link, "."]);
    crews.dataset.key = "cgf-crews";
    about.appendChild(crews);
  }

  const notSet = el("ul", { class: "cwf-notset" });
  for (const section of model.unsetSections) {
    const keys = section.keys || [];
    const item = el("li", {}, [
      el("span", { text: section.title || section.token }),
      el("span", { class: "cfx-section-note", text: `0 of ${plural(keys.length, "key")}` }),
    ]);
    item.dataset.section = section.token || "";
    notSet.appendChild(item);
  }
  if (crewTables && !crewTables.length) {
    const item = el("li", {}, [el("span", { text: "Crews" }), el("span", { class: "cfx-section-note", text: "none in this file" })]);
    item.dataset.section = "crews";
    notSet.appendChild(item);
  }
  if (!notSet.children.length) notSet.appendChild(el("li", { class: "cfx-muted", text: "Every section has a key set here." }));
  const effective = el("a", { class: "cwf-effective-link", text: `See what ${ws} runs in Effective →` });
  effective.href = "#config/effective";

  const aboutTitle = el("h2", { class: "cfx-h2", text: "About this file" });
  aboutTitle.id = "cgf-about-title";
  const aboutPanel = el("section", { class: "cfx-card cwf-side", "aria-labelledby": aboutTitle.id }, [aboutTitle, about]);
  aboutPanel.dataset.key = "cgf-about";
  const notSetTitle = el("h2", { class: "cfx-h2", text: "Not set in this file" });
  notSetTitle.id = "cgf-notset-title";
  const notSetPanel = el("section", { class: "cfx-card cwf-side", "aria-labelledby": notSetTitle.id }, [notSetTitle, notSet, effective]);
  notSetPanel.dataset.key = "cgf-notset";
  return el("aside", { class: "cwf-aside" }, [aboutPanel, notSetPanel]);
}
