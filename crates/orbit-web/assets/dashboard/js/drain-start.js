// The Drain page's Start a window composer: every `orbit run auto` start
// option, each beside the flag it sets, the equivalent command and Start.
//
// operations.js owns the readiness poll and hands each payload to
// `renderDrainStart`; the composer keeps the operator's unsent choices here so
// a poll never resets them. What this checkout may start (an owner drains its
// own backlog, a replica pulls from its owner), the configured crews and the
// workspace's complexity pools come from readiness `start_options`. The server
// validates everything again and its refusal is shown as it words it.

import { captureWorkspaceVisit, copyText, el, infoHint, postJson, reviewLayerText, submissionNoticeLines } from './common.js';

const $ = (id) => document.getElementById(id);

// `null` is one admission pass: `orbit run auto` with no `--for`.
const DRAIN_WINDOWS = [["once", "One pass"], ["1h", "1h"], ["2h", "2h"], ["4h", "4h"], ["8h", "8h"], ["12h", "12h"], ["24h", "24h"]];
const DRAIN_TIERS = [["low", "Low"], ["medium", "Medium"], ["hard", "Hard"], ["xhard", "Extra hard"]];
const CONCURRENCY_MAX = 4_294_967_295;
const CONCURRENCY_PROBLEM = "Parallel tasks must be a whole number, 1 or more. Leave it blank for the runtime default.";

export const AUTO_DRAIN_APPROVE_RULE = "Approval needs context files and an assessed complexity (or the no-diff-expected tag) plus a clean task-pilot verification. Tasks tagged no-auto-approve are skipped.";
const APPROVE_WARNING = "The window will approve qualifying proposed tasks, including ones filed while it runs.";
const COMPLETE_WARNING = "Also marks every task this window ships as done (review -> done), not only the ones eligible right now.";
const REVIEW_STOP = "Off: shipped tasks stay in review; a separate action completes them.";
const AUTO_FIX_HELP = "Repair failed required checks and merge conflicts on this window's PRs. Merging still requires Mark shipped tasks done.";
const REVIEW_HELP = "Adds a second-agent review before landing to every task this window admits. It can only add review: the layer workspace policy already applies is kept, and the other layer is refused.";
const MODE_HELP = {
  enforce: "Admits only when the forecast fits.",
  shadow: "Admits freely and records what the forecast would have decided.",
  off: "Admits without forecasting. Host safety limits still apply.",
};
// What `orbit run auto --pull` refuses, in its argument parser's words, and
// why in plain terms. The server refuses the same set the same way.
const PULL_CONFLICTS = {
  mode: ["--auto-concurrency <MODE>", "A pull drain inherits this host's auto-concurrency mode."],
  complete: ["--complete", "A pull replica does not land work: the owner completes what it ships."],
  approve: ["--approve-proposed", "A pull replica cannot approve proposed tasks; the owner machine approves them."],
  autoFix: ["--auto-fix", "A pull replica cannot repair owner pull requests; the owner machine repairs them."],
  strict: ["--strict-worker-containment", "Strict containment is an owner drain option."],
  pools: ["--<tier>-complexity-crews [<CREW>...]", "The owner's pools decide which crew runs a claim."],
  token: ["--claim-token <CLAIM_TOKEN>", "A workspace claim is the owner's; a pull drain sends none."],
};
const PULL_REVIEW_REFUSAL = "--review does not apply to a pull drain: the owner decides the review of the claims it hands out, so request review on the owner's own run or in its workspace policy";

const pullConflictText = (flag) => `the argument '--pull <SELECTOR>' cannot be used with '${flag}'`;

function freshChoices() {
  return {
    // `null` follows the checkout: local on an owner, pull on a replica.
    source: null,
    window: "1h",
    concurrency: "",
    mode: "",
    complete: false,
    approve: false,
    autoFix: false,
    review: "",
    excluded: new Set(),
    pools: { low: "ws", medium: "ws", hard: "ws", xhard: "ws" },
    overrides: { low: null, medium: null, hard: null, xhard: null },
    strict: false,
    // Sent with Start and held only in this module and the password field's
    // value: never in text, a confirm dialog, the command or the receipt.
    claimToken: "",
  };
}

let choices = freshChoices();
let hooks = null;

// operations.js supplies what the composer shares with the status card.
export function initDrainStart(nextHooks) {
  hooks = nextHooks;
}

// A workspace switch clears what belongs to the workspace it was chosen for:
// the source, crews, pools and a claim token. The window and authority
// choices carry over, as they always have.
export function resetDrainStart() {
  const fresh = freshChoices();
  Object.assign(choices, { source: fresh.source, excluded: fresh.excluded, pools: fresh.pools, overrides: fresh.overrides, claimToken: fresh.claimToken });
}

export function drainStartConcurrencyQuery() {
  return choices.concurrency && concurrencyValid() ? choices.concurrency : "";
}

function concurrencyValid() {
  if (choices.concurrency === "") return true;
  if (!/^\d+$/.test(choices.concurrency)) return false;
  const value = Number(choices.concurrency);
  return value >= 1 && value <= CONCURRENCY_MAX;
}

function numberOrNull(value) {
  if (value == null || value === "") return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

// What this checkout may start. An older server without `start_options` is
// read from `replica` alone, with no pull selector to offer.
export function drainCheckout(payload) {
  const options = payload?.start_options && typeof payload.start_options === "object" ? payload.start_options : {};
  const replica = options.checkout ? options.checkout === "replica" : payload?.replica === true;
  const workspaceId = options.workspace_id || hooks?.selectedWorkspace?.()?.id || "";
  return {
    replica,
    workspaceId,
    thisHost: options.this_host || "",
    ownerHost: options.owner_host || "",
    pullSelector: options.pull_selector || "",
    localRefusal: replica
      ? options.local_refusal || "This checkout is a replica; only its workspace's owner drains the backlog. Pull from the owner instead."
      : "",
    pullRefusal: replica ? options.pull_refusal || "" : options.pull_refusal || "`--pull` runs on a replica checkout; this checkout is its workspace's owner, whose drain is plain `orbit run auto`",
    crews: (Array.isArray(options.crews) ? options.crews : []).filter((crew) => crew && crew.enabled !== false && crew.name).map((crew) => String(crew.name)),
    pools: options.complexity_pools && typeof options.complexity_pools === "object" ? options.complexity_pools : null,
  };
}

function sourceOf(checkout) {
  return choices.source || (checkout.replica ? "pull" : "local");
}

function workspacePool(checkout, tier) {
  const pool = checkout.pools?.[tier];
  return (Array.isArray(pool?.crews) ? pool.crews : []).map((entry) => ({ name: String(entry.name), weight: numberOrNull(entry.weight) ?? 1 }));
}

// A pool's `{name, weight}` entries as `--<tier>-complexity-crews` values.
// The runtime refuses a list that mixes bare names with name:weight entries,
// so any weight other than 1 writes every weight.
export function poolEntryTexts(entries) {
  const weight = (entry) => numberOrNull(entry.weight) ?? 1;
  const weighted = entries.some((entry) => weight(entry) !== 1);
  return entries.map((entry) => (weighted ? `${entry.name}:${weight(entry)}` : String(entry.name)));
}

const poolText = (entries) => poolEntryTexts(entries).join(",");

// The override as typed, split as the CLI splits `--<tier>-complexity-crews`.
// Weights are read here only to draw the shares; the server checks the names.
function overrideEntries(checkout, tier) {
  const text = choices.overrides[tier] ?? poolText(workspacePool(checkout, tier));
  return text.split(/[\s,]+/).map((part) => part.trim()).filter(Boolean);
}

function tierEntries(checkout, tier) {
  const source = choices.pools[tier];
  if (source === "off") return [];
  if (source === "ws") return workspacePool(checkout, tier);
  return overrideEntries(checkout, tier).map((part) => {
    const [name, weight] = part.split(":");
    return { name, weight: numberOrNull(weight) ?? 1 };
  });
}

function allowedCrews(checkout) {
  return checkout.crews.filter((crew) => !choices.excluded.has(crew));
}

// Each control's own refusal, worded for the control; the blocking list words
// a chosen-but-refused option as the CLI does.
function controlReasons(payload, checkout, source) {
  const readOnly = hooks?.readOnlyReason?.() || "";
  const unauthorized = payload.controls_authorized === false;
  const pull = source === "pull";
  return {
    submit: readOnly,
    complete: readOnly || (pull ? PULL_CONFLICTS.complete[1]
      : unauthorized ? "Automatic completion requires an authorized operator session; the window can still start with default review completion." : ""),
    approve: readOnly || (pull ? PULL_CONFLICTS.approve[1]
      : unauthorized ? "Approving proposed tasks requires an authorized operator session; the window can still start and leave them for you." : ""),
    autoFix: readOnly || (pull ? PULL_CONFLICTS.autoFix[1]
      : payload.controls_authorized !== true ? "Auto-fix requires an authorized operator session." : ""),
    review: readOnly || (pull ? "A pull replica's owner decides the review of its claims; add review on the owner machine." : ""),
    mode: pull ? PULL_CONFLICTS.mode[1]
      : payload.controls_authorized !== true ? "A per-window mode requires an operator session on an owner workspace." : "",
    strict: pull ? PULL_CONFLICTS.strict[1] : "",
    pools: pull ? PULL_CONFLICTS.pools[1] : "",
    token: pull ? PULL_CONFLICTS.token[1] : "",
  };
}

// Blocking errors stop Start; warnings only inform. Every refusal the CLI
// would print for these choices is printed here first, in its words.
function drainIssues(payload, checkout, source, reasons) {
  const blocking = [];
  const warnings = [];
  if (reasons.submit) blocking.push(reasons.submit);
  if (source === "local" && checkout.localRefusal) blocking.push(checkout.localRefusal);
  if (source === "pull") {
    if (checkout.pullRefusal) blocking.push(checkout.pullRefusal);
    else if (!checkout.pullSelector) blocking.push("This server did not report the owner's selector; start the pull drain with `orbit run auto --pull`.");
    const chosen = [
      [Boolean(choices.mode), PULL_CONFLICTS.mode[0]],
      [choices.complete, PULL_CONFLICTS.complete[0]],
      [choices.approve, PULL_CONFLICTS.approve[0]],
      [choices.autoFix, PULL_CONFLICTS.autoFix[0]],
      [choices.strict, PULL_CONFLICTS.strict[0]],
      ...DRAIN_TIERS.map(([tier]) => [choices.pools[tier] !== "ws", `--${tier}-complexity-crews [<CREW>...]`]),
      [Boolean(choices.claimToken), PULL_CONFLICTS.token[0]],
    ];
    for (const [set, flag] of chosen) if (set) blocking.push(pullConflictText(flag));
    if (choices.review) blocking.push(PULL_REVIEW_REFUSAL);
  } else {
    if (choices.complete && reasons.complete) blocking.push(reasons.complete);
    if (choices.approve && reasons.approve) blocking.push(reasons.approve);
    if (choices.autoFix && reasons.autoFix) blocking.push(reasons.autoFix);
    if (choices.review && reasons.review) blocking.push(reasons.review);
    for (const [tier, label] of DRAIN_TIERS) {
      if (choices.pools[tier] === "ov" && overrideEntries(checkout, tier).length === 0) {
        blocking.push(`Name at least one crew for the ${label.toLowerCase()} override, or turn the pool off.`);
      }
    }
  }
  if (!concurrencyValid()) blocking.push(CONCURRENCY_PROBLEM);
  if (checkout.crews.length && allowedCrews(checkout).length === 0) blocking.push("Allow at least one crew: a window with none admits nothing.");
  if (source === "local") {
    for (const [tier, label] of DRAIN_TIERS) {
      const entries = tierEntries(checkout, tier);
      if (choices.excluded.size && entries.length && entries.every((entry) => choices.excluded.has(entry.name))) {
        warnings.push(`${label} pool has no allowed crew: unassigned ${label.toLowerCase()} tasks will wait.`);
      }
    }
    if (choices.complete) warnings.push(`Blanket authority: ${COMPLETE_WARNING}`);
    if (choices.approve) warnings.push(APPROVE_WARNING);
  }
  return { blocking, warnings };
}

// The settings a start or a recorded run carries, as one shape: the command
// builder and Start both read it.
function composerSettings(checkout, source) {
  const allowed = allowedCrews(checkout);
  const pools = {};
  for (const [tier] of DRAIN_TIERS) {
    if (choices.pools[tier] === "off") pools[tier] = [];
    else if (choices.pools[tier] === "ov") pools[tier] = overrideEntries(checkout, tier);
  }
  return {
    source,
    workspace: checkout.workspaceId,
    pull: checkout.workspaceId || checkout.pullSelector,
    host: checkout.ownerHost,
    selector: checkout.pullSelector,
    window: choices.window,
    concurrency: concurrencyValid() ? choices.concurrency : "",
    mode: choices.mode,
    allowCrews: choices.excluded.size && allowed.length < checkout.crews.length ? allowed : [],
    pools,
    complete: choices.complete,
    approve: choices.approve,
    autoFix: choices.autoFix,
    review: choices.review,
    strict: choices.strict,
    claimToken: Boolean(choices.claimToken),
  };
}

// The `orbit run auto` command for `settings`, one flag per element. A claim
// token is only ever its placeholder.
export function drainCommandParts(settings) {
  const parts = ["orbit run auto"];
  const pull = settings.source === "pull";
  if (pull) parts.push(settings.host ? `--pull ${settings.pull} --host ${settings.host}` : `--pull ${settings.selector || settings.pull}`);
  else if (settings.workspace) parts.push(`--workspace ${settings.workspace}`);
  if (settings.window && settings.window !== "once") parts.push(`--for ${settings.window}`);
  if (settings.concurrency) parts.push(`--concurrency ${settings.concurrency}`);
  if (!pull && settings.mode) parts.push(`--auto-concurrency ${settings.mode}`);
  if (settings.allowCrews?.length) parts.push(`--allow-crew ${settings.allowCrews.join(",")}`);
  if (!pull) {
    for (const [tier] of DRAIN_TIERS) {
      const pool = settings.pools?.[tier];
      if (Array.isArray(pool)) parts.push(pool.length ? `--${tier}-complexity-crews ${pool.join(",")}` : `--${tier}-complexity-crews`);
    }
    if (settings.complete) parts.push("--complete");
    if (settings.approve) parts.push("--approve-proposed");
    if (settings.autoFix) parts.push("--auto-fix");
    if (settings.review) parts.push(`--review ${settings.review}`);
    if (settings.strict) parts.push("--strict-worker-containment");
    if (settings.claimToken) parts.push("--claim-token <token>");
  }
  return parts;
}

// The command as a copyable block, with Copy beside it.
export function drainCommandBlock(parts, { id = null, note = "" } = {}) {
  const text = parts.join(" ");
  const command = el("pre", { class: "drain-command mono", text, title: text });
  if (id) command.id = id;
  command.tabIndex = 0;
  command.setAttribute("aria-label", "Equivalent orbit run auto command");
  const copy = el("button", { class: "operation-button drain-copy", text: "Copy command" });
  copy.type = "button";
  copy.dataset.drainFocus = id ? `${id}-copy` : "command-copy";
  copy.addEventListener("click", async () => {
    const copied = await copyText(text);
    copy.textContent = copied ? "Copied" : "Copy failed";
    setTimeout(() => { copy.textContent = "Copy command"; }, 1500);
  });
  const row = el("div", { class: "drain-command-row" }, [command, copy]);
  return el("div", { class: "drain-command-block" }, [
    row,
    note ? el("span", { class: "drain-field-hint", text: note }) : null,
  ]);
}

// A recorded run's settings, for "Start a window like this". Pull runs keep
// only what a pull drain may choose.
export function startDrainLike(settings) {
  const next = freshChoices();
  next.source = settings.source;
  next.window = settings.window;
  next.concurrency = settings.concurrency ? String(settings.concurrency) : "";
  next.mode = settings.source === "pull" ? "" : settings.mode || "";
  next.complete = settings.source !== "pull" && settings.complete === true;
  next.approve = settings.source !== "pull" && settings.approve === true;
  next.autoFix = settings.source !== "pull" && settings.autoFix === true;
  next.review = settings.source !== "pull" ? settings.review || "" : "";
  next.strict = settings.source !== "pull" && settings.strict === true;
  const crews = drainCheckout(hooks?.lastPayload?.() || {}).crews;
  if (settings.allowCrews?.length) next.excluded = new Set(crews.filter((crew) => !settings.allowCrews.includes(crew)));
  for (const [tier] of DRAIN_TIERS) {
    const pool = settings.source === "pull" ? undefined : settings.pools?.[tier];
    if (!Array.isArray(pool)) continue;
    next.pools[tier] = pool.length ? "ov" : "off";
    if (pool.length) next.overrides[tier] = pool.join(",");
  }
  choices = next;
  hooks?.showStart?.();
  hooks?.rerender?.();
}

function fieldHead(label, flag, help, { warn = false, id = null } = {}) {
  const head = el("span", { class: "drain-field-head" }, [label]);
  if (flag) head.append(el("code", { class: "drain-flag", text: flag }));
  if (help) {
    const hint = infoHint(help, { id });
    if (warn) hint.classList.add("warn");
    head.appendChild(hint);
  }
  return head;
}

function fieldRefusal(text, id = null) {
  if (!text) return null;
  const note = el("p", { class: "drain-field-hint warn", text });
  if (id) note.id = id;
  return note;
}

function section(title, children, extraClass = "") {
  return el("section", { class: `drain-compose-section${extraClass ? ` ${extraClass}` : ""}`, "aria-label": title }, [
    el("h3", { class: "drain-compose-title", text: title }),
    ...children,
  ]);
}

function segmented(label, options, selected, { disabled = () => false, focus, onPick, extraClass = "" }) {
  const group = el("div", { class: `drain-segments${extraClass ? ` ${extraClass}` : ""}`, role: "group", "aria-label": label });
  for (const [value, text] of options) {
    const on = value === selected;
    const option = el("button", { class: `drain-duration${on ? " selected" : ""}`, text });
    option.type = "button";
    option.dataset.drainFocus = `${focus}-${value}`;
    option.setAttribute("aria-pressed", on ? "true" : "false");
    option.disabled = disabled(value);
    option.addEventListener("click", () => onPick(value));
    group.appendChild(option);
  }
  return group;
}

function switchField({ id, label, flag, help, warn = false, checked, disabled = false, refusal = "", extraClass = "", onChange }) {
  const input = el("input", { class: "drain-switch", role: "switch" });
  input.id = id;
  input.type = "checkbox";
  input.checked = checked;
  input.disabled = disabled;
  input.dataset.drainFocus = id;
  if (refusal) input.title = refusal;
  input.setAttribute("aria-describedby", refusal ? `${id}-reason ${id}-help` : `${id}-help`);
  input.addEventListener("change", () => onChange(input.checked));
  const name = el("label", { class: "drain-field-label", text: label });
  name.htmlFor = id;
  return el("div", { class: `drain-field drain-field-row${extraClass ? ` ${extraClass}` : ""}` }, [
    el("div", { class: "drain-field-text" }, [fieldHead(name, flag, help, { warn, id: `${id}-help` }), fieldRefusal(refusal, `${id}-reason`)]),
    input,
  ]);
}

function sourceControl(checkout, source, rerender) {
  const options = [
    ["local", "Local backlog", checkout.replica
      ? "This checkout only holds a replica of the workspace."
      : `${checkout.thisHost || "This host"} owns ${checkout.workspaceId || "this workspace"}: the window admits its backlog and lands here.`, checkout.localRefusal],
    ["pull", "Pull from the owner", checkout.replica
      ? `Claims ${checkout.ownerHost || "the owner"}'s work and hands each result back as a pull request.`
      : "Runs on a replica checkout, which claims this owner's work.", checkout.pullRefusal],
  ];
  const group = el("div", { class: "drain-sources", role: "radiogroup", "aria-label": "Where the work comes from" });
  for (const [value, title, detail, refusal] of options) {
    const on = value === source;
    const option = el("button", { class: `drain-source${on ? " selected" : ""}${refusal ? " blocked" : ""}`, role: "radio", "aria-checked": on ? "true" : "false" }, [
      el("span", { class: "drain-source-dot", "aria-hidden": "true" }),
      el("span", { class: "drain-source-text" }, [
        el("span", { class: "drain-source-title", text: title }),
        el("span", { class: "drain-source-detail", text: detail }),
        refusal ? el("span", { class: "drain-source-refusal", text: `Blocked: ${refusal}` }) : null,
      ]),
    ]);
    option.type = "button";
    option.id = `auto-drain-source-${value}`;
    option.dataset.drainFocus = option.id;
    option.addEventListener("click", () => {
      choices.source = value;
      rerender();
    });
    group.appendChild(option);
  }
  const flags = source === "pull"
    ? (checkout.ownerHost ? `--pull ${checkout.workspaceId} --host ${checkout.ownerHost}` : `--pull ${checkout.pullSelector || "<selector>"}`)
    : `--workspace ${checkout.workspaceId || "<workspace>"}`;
  const resolved = source === "pull" && checkout.pullSelector
    ? el("p", { class: "drain-field-hint", id: "auto-drain-pull-selector" }, ["Resolves to ", el("code", { class: "mono", text: checkout.pullSelector }), checkout.ownerHost ? ` on ${checkout.ownerHost}` : ""])
    : null;
  return section("Where the work comes from", [group, el("p", { class: "drain-field-hint" }, [el("code", { class: "drain-flag", text: flags })]), resolved]);
}

function concurrencyControl(payload, source, form) {
  const limit = numberOrNull(payload.capacity?.max_active_leaf_runs);
  const fallback = limit != null && limit >= 1 ? String(Math.trunc(limit)) : "";
  const label = el("label", { class: "drain-field-label", text: "Parallel tasks" });
  label.htmlFor = "auto-drain-concurrency";
  const input = el("input", { class: "drain-stepper-value mono", title: "Leaf-run concurrency (blank = runtime default)" });
  input.id = "auto-drain-concurrency";
  input.type = "number";
  input.min = "1";
  input.placeholder = fallback || "auto";
  input.value = choices.concurrency;
  input.dataset.drainFocus = "concurrency";
  const problem = el("div", { class: "operation-control-note drain-stop-note disabled-reason" });
  problem.id = "auto-drain-concurrency-problem";
  problem.setAttribute("role", "alert");
  problem.hidden = true;
  form.problem = problem;
  input.setAttribute("aria-describedby", `${problem.id} auto-drain-concurrency-hint`);
  // Typing does not rebuild the composer (that would drop the caret), so the
  // field, its message and the footer are brought in line by hand.
  form.syncConcurrency = () => {
    const valid = concurrencyValid();
    input.setAttribute("aria-invalid", valid ? "false" : "true");
    problem.textContent = valid ? "" : CONCURRENCY_PROBLEM;
    problem.hidden = valid;
    form.syncFoot?.();
  };
  input.addEventListener("input", () => {
    choices.concurrency = input.value.trim();
    form.syncConcurrency();
  });
  const step = (delta, name) => {
    const button = el("button", { class: "drain-step", text: delta < 0 ? "−" : "+" });
    button.type = "button";
    button.dataset.drainFocus = `concurrency${delta}`;
    button.setAttribute("aria-label", `${name} concurrency`);
    button.addEventListener("click", () => {
      const current = Math.trunc(Number(choices.concurrency || fallback || 1));
      choices.concurrency = String(Math.max(1, (Number.isFinite(current) ? current : 1) + delta));
      input.value = choices.concurrency;
      form.syncConcurrency();
    });
    return button;
  };
  const n = choices.concurrency || fallback || "N";
  const help = source === "pull"
    ? `Counts held − min(reviewing, ${n}) against ${n}: at most ${n} implementing, and up to ${n === "N" ? "2N" : 2 * Number(n)} claims in all once leaves reach the owner's review.`
    : "Upper bound; the forecast may admit fewer.";
  return el("div", { class: "drain-field drain-field-row" }, [
    el("div", { class: "drain-field-text" }, [fieldHead(label, "--concurrency", help, { id: "auto-drain-concurrency-hint" })]),
    el("div", { class: "drain-stepper" }, [step(-1, "Decrease"), input, step(1, "Increase")]),
  ]);
}

function modeControl(payload, reasons, rerender) {
  const configured = payload.auto_concurrency?.configured_mode;
  const label = el("label", { class: "drain-field-label", text: "Admission control" });
  label.htmlFor = "auto-drain-mode";
  const select = el("select", { class: "drain-mode-select" });
  select.id = "auto-drain-mode";
  select.dataset.drainFocus = "mode";
  for (const [value, text] of [["", `Host default (${configured || "unknown"})`], ["off", "Off"], ["shadow", "Shadow"], ["enforce", "Enforce"]]) {
    const option = el("option", { text });
    option.value = value;
    select.appendChild(option);
  }
  // A chosen mode stays clearable after it becomes unavailable.
  select.disabled = Boolean(reasons.mode) && !choices.mode;
  select.value = reasons.mode && !choices.mode ? "" : choices.mode;
  select.addEventListener("change", () => {
    choices.mode = select.value;
    rerender();
  });
  const help = choices.mode
    ? `${MODE_HELP[choices.mode]} For this window only; configuration is unchanged.`
    : `Uses this host's auto-concurrency mode (machine.auto_concurrency.mode).${MODE_HELP[configured] ? ` ${MODE_HELP[configured]}` : ""}`;
  return el("div", { class: "drain-field" }, [
    fieldHead(label, "--auto-concurrency", reasons.mode ? "" : help),
    select,
    fieldRefusal(reasons.mode),
  ]);
}

function windowSection(payload, source, reasons, form, rerender) {
  // Recorded CLI windows may have durations outside the quick-start presets.
  // Keep the copied duration visible and selected until another is chosen.
  const windows = DRAIN_WINDOWS.some(([value]) => value === choices.window)
    ? DRAIN_WINDOWS : [...DRAIN_WINDOWS, [choices.window, choices.window]];
  const label = el("span", { class: "drain-field-label", text: "Window length" });
  label.id = "auto-drain-duration-label";
  const help = choices.window === "once"
    ? (source === "pull" ? "One admission pass: claims what fits now and requests no replacements." : "One admission pass: admits what fits now, then nothing more.")
    : "Admissions stop when the window closes; tasks already started run to the end. Extend it later from Overview.";
  return section("Window", [
    el("div", { class: "drain-field drain-field-duration" }, [
      fieldHead(label, choices.window === "once" ? "no --for" : "--for", help),
      segmented("Window duration", windows, choices.window, {
        focus: "duration",
        extraClass: "drain-durations",
        onPick: (value) => { choices.window = value; rerender(); },
      }),
    ]),
    concurrencyControl(payload, source, form),
    form.problem,
    modeControl(payload, reasons, rerender),
  ]);
}

function authoritySection(source, reasons, rerender) {
  const pull = source === "pull";
  const set = (key) => (on) => { choices[key] = on; rerender(); };
  const reviewLabel = el("label", { class: "drain-field-label", text: "Review before landing" });
  reviewLabel.htmlFor = "auto-drain-review";
  const review = el("select", { class: "drain-mode-select" });
  review.id = "auto-drain-review";
  review.dataset.drainFocus = "review";
  for (const [value, text] of [["", "Workspace policy"], ["before-pr", "Add before-PR review"], ["before-landing", "Add before-landing review"]]) {
    const option = el("option", { text });
    option.value = value;
    review.appendChild(option);
  }
  review.disabled = Boolean(reasons.review) && !choices.review;
  review.value = choices.review;
  review.addEventListener("change", () => { choices.review = review.value; rerender(); });
  return section(pull ? "What the window may do · the owner decides" : "What the window may do", [
    switchField({
      id: "auto-drain-complete", label: "Mark shipped tasks done", flag: "--complete",
      help: reasons.complete && !choices.complete && !pull ? reasons.complete : choices.complete ? COMPLETE_WARNING : REVIEW_STOP,
      warn: choices.complete || Boolean(reasons.complete),
      checked: choices.complete, disabled: Boolean(reasons.complete) && !choices.complete,
      refusal: pull ? reasons.complete : "", extraClass: "drain-field-complete", onChange: set("complete"),
    }),
    switchField({
      id: "auto-drain-approve", label: "Approve qualifying proposals", flag: "--approve-proposed",
      help: choices.approve ? `${APPROVE_WARNING}\n${AUTO_DRAIN_APPROVE_RULE}` : `Off: proposed tasks stay proposed until you approve them.\n${AUTO_DRAIN_APPROVE_RULE}`,
      warn: choices.approve, checked: choices.approve, disabled: Boolean(reasons.approve) && !choices.approve,
      refusal: reasons.approve && !reasons.submit ? reasons.approve : "", extraClass: "drain-field-approve", onChange: set("approve"),
    }),
    switchField({
      id: "auto-drain-auto-fix", label: "Auto-fix PR blockers", flag: "--auto-fix", help: AUTO_FIX_HELP,
      checked: choices.autoFix, disabled: Boolean(reasons.autoFix) && !choices.autoFix,
      refusal: reasons.autoFix || "", onChange: set("autoFix"),
    }),
    el("div", { class: "drain-field" }, [
      fieldHead(reviewLabel, "--review", choices.review ? REVIEW_HELP : "The workspace's review.timing setting decides."),
      review,
      fieldRefusal(reasons.review || ""),
    ]),
  ], pull ? "owner-decides" : "");
}

function crewChips(checkout, source, rerender) {
  const allowed = allowedCrews(checkout);
  const all = allowed.length === checkout.crews.length;
  const chips = el("div", { class: "drain-crews", role: "group", "aria-label": "Crews this window may start" });
  for (const crew of checkout.crews) {
    const on = !choices.excluded.has(crew);
    const chip = el("button", { class: `drain-crew mono${on ? " on" : ""}`, text: crew });
    chip.type = "button";
    chip.dataset.crew = crew;
    chip.dataset.drainFocus = `crew-${crew}`;
    chip.setAttribute("aria-pressed", on ? "true" : "false");
    chip.addEventListener("click", () => {
      if (on) choices.excluded.add(crew);
      else choices.excluded.delete(crew);
      rerender();
    });
    chips.appendChild(chip);
  }
  const bulk = (text, apply) => {
    const button = el("button", { class: "drain-link-button", text });
    button.type = "button";
    button.dataset.drainFocus = `crews-${text}`;
    button.addEventListener("click", () => { apply(); rerender(); });
    return button;
  };
  const label = el("span", { class: "drain-field-label", text: "Allowed crews" });
  const help = source === "pull"
    ? "Declared to the owner for this window: it hands this replica only tasks on these crews."
    : "This window only. Nothing is reassigned and no config changes: a task on an excluded crew is not started.";
  return el("div", { class: "drain-field" }, [
    fieldHead(label, "--allow-crew", help),
    el("div", { class: "drain-crews-head" }, [
      el("span", { class: "drain-field-hint", id: "auto-drain-crews-summary", text: checkout.crews.length ? (all ? `All ${checkout.crews.length} crews · flag omitted` : `${allowed.length} of ${checkout.crews.length} allowed`) : "No crews configured." }),
      checkout.crews.length ? el("span", { class: "drain-crews-bulk" }, [
        bulk("All", () => choices.excluded.clear()),
        bulk("None", () => checkout.crews.forEach((crew) => choices.excluded.add(crew))),
      ]) : null,
    ]),
    chips,
  ]);
}

function poolRow(checkout, tier, label, reasons, form, rerender) {
  const source = choices.pools[tier];
  const entries = tierEntries(checkout, tier);
  const total = entries.reduce((sum, entry) => sum + Math.max(0, entry.weight), 0) || 1;
  const bar = el("div", { class: "drain-tier-bar", "aria-hidden": "true" });
  const members = el("div", { class: "drain-tier-members" });
  entries.forEach((entry, index) => {
    const excluded = choices.excluded.has(entry.name);
    const shade = excluded ? "excluded" : `shade-${index % 4}`;
    const part = el("span", { class: `drain-tier-share ${shade}` });
    part.style.flex = String(Math.max(entry.weight, 0) || 0.0001);
    bar.appendChild(part);
    members.appendChild(el("span", { class: `drain-tier-member mono${excluded ? " excluded" : ""}`, title: excluded ? `${entry.name} is excluded by --allow-crew` : "" }, [
      el("span", { class: `drain-tier-swatch ${shade}`, "aria-hidden": "true" }),
      el("span", { class: "drain-tier-name", text: entry.name }),
      el("span", { class: "drain-tier-pct", text: `${Math.round((Math.max(entry.weight, 0) / total) * 100)}%` }),
    ]));
  });
  const flag = `--${tier}-complexity-crews`;
  const picker = segmented(`${label} pool`, [["ws", "Workspace"], ["ov", "Override"], ["off", "Off"]], source, {
    focus: `pool-${tier}`,
    extraClass: "drain-tier-modes",
    disabled: (value) => Boolean(reasons.pools) && value !== source && value !== "ws",
    onPick: (value) => { choices.pools[tier] = value; rerender(); },
  });
  const note = source === "off"
    ? `Off: unassigned ${label.toLowerCase()} tasks use the default crew chain. ${flag} with no crews.`
    : source === "ws"
      ? (entries.length ? `From workflow.${tier}_complexity_crews.` : `workflow.${tier}_complexity_crews is not set.`)
      : "This window only. crew or crew:weight, comma-separated.";
  const row = el("div", { class: "drain-tier" }, [
    el("div", { class: "drain-tier-head" }, [el("span", { class: "drain-tier-label", text: label }), picker]),
  ]);
  row.dataset.tier = tier;
  if (source === "ov") {
    const input = el("input", { class: "drain-tier-input mono", "aria-label": `${label} pool override (${flag})` });
    input.id = `auto-drain-pool-${tier}`;
    input.type = "text";
    input.spellcheck = false;
    input.autocomplete = "off";
    input.value = choices.overrides[tier] ?? poolText(workspacePool(checkout, tier));
    input.dataset.drainFocus = input.id;
    input.addEventListener("input", () => { choices.overrides[tier] = input.value; form.syncFoot?.(); });
    input.addEventListener("change", rerender);
    row.appendChild(input);
  }
  if (entries.length) row.append(bar, members);
  row.appendChild(el("p", { class: "drain-field-hint", text: note }));
  return row;
}

function crewsSection(checkout, source, reasons, form, rerender) {
  const pools = DRAIN_TIERS.map(([tier, label]) => poolRow(checkout, tier, label, reasons, form, rerender));
  const poolsHead = el("span", { class: "drain-field-label", text: "Complexity pools" });
  return section("Crews", [
    crewChips(checkout, source, rerender),
    el("div", { class: `drain-field${reasons.pools ? " owner-decides" : ""}` }, [
      fieldHead(poolsHead, "--<tier>-complexity-crews", "Which crew an unassigned task of each complexity draws. Workspace keeps the configured pool; Override replaces it for this window; Off draws none."),
      fieldRefusal(reasons.pools),
      checkout.pools ? el("div", { class: "drain-tiers" }, pools) : el("p", { class: "drain-field-hint", text: "This server did not report the workspace pools." }),
    ]),
  ]);
}

function advancedSection(source, reasons, form, rerender) {
  const tokenLabel = el("label", { class: "drain-field-label", text: "Workspace claim token" });
  tokenLabel.htmlFor = "auto-drain-claim-token";
  const token = el("input", { class: "drain-token-input mono" });
  token.id = "auto-drain-claim-token";
  token.type = "password";
  token.autocomplete = "off";
  token.spellcheck = false;
  token.placeholder = "Only when another operator holds a claim";
  token.disabled = Boolean(reasons.token) && !choices.claimToken;
  // The property, never the attribute: a password value is not serialized.
  token.value = choices.claimToken;
  token.dataset.drainFocus = token.id;
  token.addEventListener("input", () => { choices.claimToken = token.value; form.syncFoot?.(); });
  return section("Advanced", [
    switchField({
      id: "auto-drain-strict", label: "Strict worker containment", flag: "--strict-worker-containment",
      help: "Require a systemd user scope for the coordinator and every worker; the window refuses to start without one.",
      checked: choices.strict, disabled: Boolean(reasons.strict) && !choices.strict, refusal: reasons.strict,
      onChange: (on) => { choices.strict = on; rerender(); },
    }),
    el("div", { class: "drain-field" }, [
      fieldHead(tokenLabel, "--claim-token", "The token of this workspace's exclusive claim. Sent with Start and never shown again, here, in the command or in the run."),
      token,
      fieldRefusal(reasons.token),
    ]),
  ]);
}

function windowLabel(window) {
  return window === "once" ? "one pass" : window;
}

function startLabel(payload, source) {
  if (source === "pull") return choices.window === "once" ? "Claim once" : `Start pulling for ${choices.window}`;
  if (choices.window === "once") return "Run one pass";
  return `${payload.capacity?.drain_phase === "draining" ? "Start another" : "Start"} ${choices.window} window`;
}

function summaryText(payload, checkout, source) {
  const limit = numberOrNull(payload.capacity?.max_active_leaf_runs);
  const parallel = concurrencyValid() && choices.concurrency ? choices.concurrency : limit != null && limit >= 1 ? String(Math.trunc(limit)) : "";
  const allowed = allowedCrews(checkout);
  const bits = [
    choices.window === "once" ? "One admission pass" : `${choices.window} window`,
    parallel ? (source === "pull" ? `up to ${parallel} implementing` : `up to ${parallel} in parallel`) : "runtime default parallelism",
  ];
  if (source === "pull") bits.push(`pulling from ${checkout.ownerHost || checkout.pullSelector || "the owner"}`);
  else {
    const mode = choices.mode || payload.auto_concurrency?.configured_mode || "";
    if (mode) bits.push(mode === "off" ? "no forecast" : mode);
  }
  if (checkout.crews.length) bits.push(choices.excluded.size ? `${allowed.length} of ${checkout.crews.length} crews` : "all crews");
  if (source === "local") {
    const changed = DRAIN_TIERS.filter(([tier]) => choices.pools[tier] !== "ws").map(([, label]) => label.toLowerCase());
    if (changed.length) bits.push(`${changed.join(", ")} pool${changed.length > 1 ? "s" : ""} changed`);
    if (choices.autoFix) bits.push("auto-fix on");
    if (choices.approve) bits.push("approves proposals");
    if (choices.review) bits.push(`adds ${choices.review} review`);
    if (choices.strict) bits.push("strict containment");
    bits.push(choices.complete ? "marks done" : "stops at review");
  } else bits.push("owner lands");
  return bits.join(" · ");
}

function issueList(issues) {
  const list = el("ul", { class: "drain-issues", id: "auto-drain-issues" });
  for (const text of issues.blocking) list.appendChild(el("li", { class: "drain-issue blocking", text }));
  for (const text of issues.warnings) list.appendChild(el("li", { class: "drain-issue warning", text }));
  list.hidden = !list.children.length;
  return list;
}

function startFoot(payload, checkout, source, reasons, form) {
  const foot = el("div", { class: "drain-start-foot" });
  const summary = el("p", { class: "drain-start-summary", id: "auto-drain-start-summary" });
  const issuesSlot = el("div", { class: "drain-issues-slot" });
  const commandSlot = el("div", { class: "drain-command-slot" });
  const resources = hooks?.throttledResources?.(payload.capacity) || [];
  const held = resources.length
    ? el("p", { class: "drain-start-held" }, [
      el("span", { class: "drain-banner-icon small", "aria-hidden": "true" }),
      el("span", { text: `Starts now, but nothing is admitted until the ${hooks.throttleNames(resources)} throttle clears.` }),
    ])
    : null;
  const button = startButton(payload, checkout, source, form);
  form.syncFoot = () => {
    const issues = drainIssues(payload, checkout, source, reasons);
    summary.textContent = summaryText(payload, checkout, source);
    issuesSlot.replaceChildren(issueList(issues));
    const settings = composerSettings(checkout, source);
    commandSlot.replaceChildren(drainCommandBlock(drainCommandParts(settings), {
      id: "auto-drain-command",
      note: settings.claimToken ? "The claim token stays out: put it in place of <token>." : "",
    }));
    button.disabled = hooks.pending.has("auto-drain:start") || issues.blocking.length > 0;
  };
  foot.append(...[summary, issuesSlot, held, commandSlot, el("div", { class: "drain-start-actions" }, [button])].filter(Boolean));
  return foot;
}

function confirmText(payload, checkout, source, settings) {
  const workspace = hooks.selectedWorkspace();
  const counts = hooks.counts(payload);
  const lines = source === "pull"
    ? [
      `Start a pull drain on ${checkout.thisHost || "this replica"} from ${checkout.ownerHost || checkout.pullSelector}?`,
      `Duration: ${windowLabel(choices.window)} · Concurrency: ${choices.concurrency || "runtime default"}`,
      `Owner: ${checkout.pullSelector}`,
      `Crews: ${settings.allowCrews.length ? settings.allowCrews.join(", ") : "all"}`,
      "",
      "The owner decides completion, review and approval for every claim.",
    ]
    : [
      `Start a bounded auto-delivery window in workspace "${workspace?.name || workspace?.id || checkout.workspaceId}"?`,
      `Duration: ${windowLabel(choices.window)} · Concurrency: ${choices.concurrency || "runtime default"}`,
      `Auto-concurrency: ${choices.mode || "inherit host at submission"}`,
      `Review before landing: ${choices.review ? `add ${choices.review} review` : "workspace policy"}`,
      `Crews: ${settings.allowCrews.length ? settings.allowCrews.join(", ") : "all"}`,
      ...DRAIN_TIERS.filter(([tier]) => Array.isArray(settings.pools[tier])).map(([tier, label]) => `${label} pool: ${settings.pools[tier].length ? settings.pools[tier].join(", ") : "off"}`),
      ...(choices.strict ? ["Strict worker containment: on"] : []),
      `Currently eligible: ${counts.eligible} · waiting: ${counts.waiting}`,
      "",
      choices.complete ? `WARNING: ${COMPLETE_WARNING}` : "Shipped tasks stay in review; a separate action completes them.",
      ...(choices.autoFix ? [`Auto-fix PR blockers: ${AUTO_FIX_HELP}`] : []),
      ...(choices.approve ? [`${APPROVE_WARNING} ${AUTO_DRAIN_APPROVE_RULE}`] : []),
    ];
  return lines.join("\n");
}

// The POST body for these choices: only what the chosen source takes.
function startBody(source, settings) {
  const body = choices.window === "once" ? { one_pass: true } : { for_duration: choices.window };
  if (choices.concurrency) body.concurrency = Number(choices.concurrency);
  if (settings.allowCrews.length) body.allow_crew = settings.allowCrews;
  if (source === "pull") {
    if (settings.host) {
      body.pull = settings.pull;
      body.host = settings.host;
    } else body.pull = settings.selector;
    return body;
  }
  Object.assign(body, { complete: choices.complete, approve_proposed: choices.approve, auto_fix: choices.autoFix });
  if (choices.mode) body.auto_concurrency = choices.mode;
  if (choices.review) body.review = choices.review;
  for (const [tier] of DRAIN_TIERS) {
    if (Array.isArray(settings.pools[tier])) body[`${tier}_complexity_crews`] = settings.pools[tier];
  }
  if (choices.strict) body.strict_worker_containment = true;
  if (choices.claimToken) body.claim_token = choices.claimToken;
  return body;
}

function receiptLines(result) {
  const runId = result?.run_id ?? null;
  const state = result?.state ?? "submitted";
  if (result?.source === "pull") {
    return [
      `Run ${runId ?? "(no run id)"} ${state}: pulling from ${result.selector}.`,
      ...(result.allowed_crews?.length ? [`Crews: ${result.allowed_crews.join(", ")}.`] : []),
      ...submissionNoticeLines(result),
    ];
  }
  return [
    `Run ${runId ?? "(no run id)"} ${state} (completion: ${result?.completion ?? "review"}).${result?.approve_proposed === true ? " Approving qualifying proposed tasks." : ""}`,
    ...(result?.auto_fix === true ? ["Auto-fix PR blockers enabled."] : []),
    ...(result?.auto_concurrency?.mode ? [`Auto-concurrency: ${result.auto_concurrency.mode} (${result.auto_concurrency.source}).`] : []),
    ...(result?.review?.timing ? [`Review before landing: ${reviewLayerText(result.review)}.`] : []),
    ...(result?.allowed_crews?.length ? [`Crews: ${result.allowed_crews.join(", ")}.`] : []),
    ...submissionNoticeLines(result),
  ];
}

function startButton(payload, checkout, source, form) {
  const key = "auto-drain:start";
  const pending = hooks.pending.has(key);
  const button = el("button", {
    class: "operation-button drain-start primary",
    text: pending ? "Starting…" : startLabel(payload, source),
    title: "Submit orbit run auto with these settings",
  });
  button.type = "button";
  button.dataset.drainFocus = "start";
  form.start = button;
  button.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    const reasons = controlReasons(payload, checkout, source);
    if (hooks.pending.has(key) || drainIssues(payload, checkout, source, reasons).blocking.length) return;
    const settings = composerSettings(checkout, source);
    if (!window.confirm(confirmText(payload, checkout, source, settings))) return;
    const body = startBody(source, settings);
    hooks.pending.add(key);
    hooks.feedback("auto-drain-operation-feedback", "pending", source === "pull"
      ? `Starting a ${windowLabel(choices.window)} pull drain…`
      : `Starting a ${windowLabel(choices.window)} auto-delivery window…`);
    hooks.rerender();
    try {
      const result = await postJson(visit.path("/api/workflows/auto"), body);
      if (!visit.isCurrent()) return;
      // A token is for one start; it is not kept for the next.
      choices.claimToken = "";
      hooks.feedback("auto-drain-operation-feedback", "success", receiptLines(result).join("\n"));
      hooks.showOverview?.();
      await hooks.refreshAfterAction();
    } catch (error) {
      if (visit.isCurrent()) hooks.feedback("auto-drain-operation-feedback", "error", error.status
        ? `Auto-delivery window failed to start: ${error.message}`
        : `Submission outcome unknown: ${error.message}. Check the window and run history before starting again; this request was not retried.`);
    } finally {
      hooks.pending.delete(key);
      hooks.rerender();
    }
  });
  return button;
}

// Builds the composer into `#auto-drain-start-body`. A read-only view hides it.
export function renderDrainStart(payload, readOnlyReason) {
  const card = $("auto-drain-start");
  const body = $("auto-drain-start-body");
  if (!card || !body || !hooks) return;
  body.textContent = "";
  card.hidden = Boolean(readOnlyReason);
  if (card.hidden) return;
  const checkout = drainCheckout(payload);
  const source = sourceOf(checkout);
  const counts = hooks.counts(payload);
  const sub = $("auto-drain-start-sub");
  if (sub) sub.textContent = source === "pull"
    ? `Pull from ${checkout.ownerHost || "the owner"} · ${checkout.thisHost || "this replica"}`
    : `${counts.eligible} eligible now · ${counts.waiting} waiting`;
  const rerender = () => hooks.rerender();
  const reasons = controlReasons(payload, checkout, source);
  const form = {};
  const columns = el("div", { class: "drain-compose" }, [
    el("div", { class: "drain-compose-col" }, [
      sourceControl(checkout, source, rerender),
      windowSection(payload, source, reasons, form, rerender),
      authoritySection(source, reasons, rerender),
    ]),
    el("div", { class: "drain-compose-col" }, [
      crewsSection(checkout, source, reasons, form, rerender),
      advancedSection(source, reasons, form, rerender),
    ]),
  ]);
  body.append(columns, startFoot(payload, checkout, source, reasons, form));
  form.syncConcurrency();
}
