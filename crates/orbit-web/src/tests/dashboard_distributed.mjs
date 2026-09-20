// ORB-12516: distributed claim provenance and the owner's handoff actions, as
// the operator actually meets them.
//
// The scenario drives the *shipped* `distributed.js` against a fetch stub, so
// what is asserted is what the dashboard paints and what it sends — not an
// implementation shape. It runs unchanged in two places: on the keyboard DOM
// adapter under `cargo test`, and inside a real Chromium via
// `dashboard_distributed_browser.mjs`, which is why every interaction goes
// through `press()` and every lookup through class selectors both support.
//
// What it has to prove, in the order an operator meets it:
//
//  * a claim reads as host-qualified, and a row with no recorded machine reads
//    as *unknown* rather than as this one;
//  * an elapsed reservation is a diagnostic, not a revocation;
//  * a run that executed elsewhere names its host instead of offering a link
//    into this checkout's job store;
//  * `review` is a delivery handoff awaiting authority, not a code review, and
//    merged is not deployed;
//  * approving and revoking send the exact candidate the operator was shown,
//    with a replay identity, and repaint from the state they produced;
//  * a refused action is honest: a stale conflict re-reads, and an uncertain
//    merge is left alone.

// Runs against the shipped modules in both the Node DOM harness and Chromium,
// so the assertions are plain functions rather than a Node import.
const fail = (message) => { throw new Error(message); };
const assert = {
  ok: (condition, message) => { if (!condition) fail(message || "expected a truthy value"); },
  equal: (actual, expected, message) => {
    if (actual !== expected) fail(message || `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
};

const press = (node) => (typeof node.click === "function" ? node.click() : node.dispatch("click"));
const settle = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};
const buttons = (root) => Array.from(root.querySelectorAll("button.claim-action"));
const button = (root, label) => buttons(root).find((node) => node.textContent.includes(label));
const recoveryButton = (root, status) => buttons(root).find(
  (node) => node.getAttribute("data-action") === "recover" && node.getAttribute("data-status") === status,
);
const reasonInput = (root) => root.querySelector("input.claim-reason");

const CANDIDATE = "a".repeat(40);
const BASE = "c".repeat(40);

const claim = (overrides = {}) => ({
  claim_id: "claim-1",
  task_id: "ORB-2",
  request_id: "pull-1",
  phase: "handed_off",
  phase_summary: "delivery handed off and awaiting completion authority — this is not a code review",
  authorizes_execution: false,
  unsettled: true,
  executed_on: { known: true, machine_id: "hm_follower", host_id: "runner-2" },
  run_context: { run_id: "drain-1", job_name: "auto", host_id: null },
  bound_run: { machine_id: "hm_follower", run_id: "leaf-1" },
  bound_run_navigable: false,
  inspect_on: "inspect this run on machine hm_follower (no owner-local run exists)",
  footprint: ["file:src/a.rs"],
  footprint_protected: true,
  reservation: {
    id: "res-1",
    expires_at: "2026-09-19T00:00:00+00:00",
    expired: true,
    note: "reservation window elapsed — the claim is still live and its frozen footprint still protects these files; expiry is not revocation and not proof the attempt died",
  },
  created_at: "2026-09-18T00:00:00+00:00",
  updated_at: "2026-09-19T00:00:00+00:00",
  last_event: "handoff_accepted",
  age_seconds: 90000,
  unresolved_merge_intent: null,
  landing_invalidated: false,
  handoff: {
    handoff_id: "handoff1",
    accepted_at: "2026-09-19T00:00:00+00:00",
    task_id: "ORB-2",
    claim_id: "claim-1",
    executed_on: { known: true, machine_id: "hm_follower", host_id: null },
    run_id: "leaf-1",
    execution_summary: "Outcome: success",
    candidate: {
      repository: "owner/repository",
      source_branch: "attempt/leaf",
      base_branch: "agent-main",
      landing_branch: "agent-main",
      candidate: { commit: CANDIDATE, tree: "b".repeat(40) },
      base: { commit: BASE, tree: "d".repeat(40) },
      delivery: { kind: "pull_request", number: 7 },
    },
    review: {
      policy: "none",
      disposition: "not_required",
      is_code_review: false,
      summary: "review not required (policy none) — no reviewer ran and no verdict exists",
    },
    required_commands: ["make ci"],
    validation: [{ path: "checks.json", sha256: "e".repeat(64) }],
    authority: { state: "not_authorized", summary: "no completion authority is recorded; this handoff waits for explicit owner approval", authorization_id: null, recorded_at: null },
    landing: { state: "none", summary: "no landing attempt has been reserved", attempt: null, job_run_id: null, evidence: null, merged: false, deployed: null },
    uncertain_merge_intent: null,
  },
  ...overrides,
});

const console_ = (claims, capabilities) => ({
  schema_version: 1,
  owner_workspace: true,
  refusal: null,
  refusal_detail: null,
  distributed_execution_enabled: false,
  claims,
  capabilities: capabilities || {
    handoff_approve: { authorized: true, reason: null },
    handoff_revoke: { authorized: true, reason: null },
    claim_recover: { authorized: true, reason: null },
  },
});

// --- fetch stub -------------------------------------------------------------

let consoleBody = console_([claim()]);
let nextAction = null; // { status, body } for the next POST, else success
const sent = [];
const consoleReads = [];
let workspaceConsoles = null;

const respond = (payload, status = 200) => ({
  ok: status >= 200 && status < 300,
  status,
  json: async () => payload,
  text: async () => JSON.stringify(payload),
});

globalThis.fetch = async (path, options = {}) => {
  const url = new URL(String(path), "http://dashboard.test");
  const method = (options && options.method) || "GET";
  if (method === "GET") {
    const workspace = url.searchParams.get("workspace");
    consoleReads.push({ path: url.pathname, workspace });
    return respond((workspaceConsoles && workspaceConsoles[workspace]) || consoleBody);
  }
  const body = options.body ? JSON.parse(options.body) : null;
  sent.push({ path: url.pathname, body });
  if (nextAction) {
    const refusal = nextAction;
    nextAction = null;
    return respond(refusal.body, refusal.status);
  }
  return respond({ ok: true, result: { handoff_id: "handoff1", claim_id: "claim-1", phase: "handed_off", task_status: "review" } });
};

const distributed = await import("./distributed.js");
const { buildDistributedBlock, invalidateDistributedConsole, formatExecutionLocation } = distributed;
const { setWorkspace } = await import("./common.js");
await import("./tasks.js");

const mount = async (taskId = "ORB-2") => {
  invalidateDistributedConsole();
  const block = buildDistributedBlock(taskId);
  await settle();
  return block;
};

// --- provenance -------------------------------------------------------------

{
  // A row with no recorded execution machine is unknown. It is never labelled
  // as the owner, which is the whole reason the field is nullable.
  const unknown = formatExecutionLocation(null);
  assert.ok(unknown.includes("unknown"), unknown);
  assert.ok(!/owner/i.test(unknown), `unknown provenance must not name an owner: ${unknown}`);
  assert.equal(formatExecutionLocation({ known: true, machine_id: "hm_a" }), "machine hm_a");
  assert.equal(
    formatExecutionLocation({ known: true, machine_id: "hm_a", host_id: "box" }),
    "machine hm_a · host box",
  );
}

{
  const block = await mount();
  const text = block.textContent;

  assert.equal(block.style.display, "", "a task with a claim shows the block");
  assert.equal(block.querySelector("h4").getAttribute("aria-expanded"), "true");
  assert.ok(text.includes("machine hm_follower · host runner-2"), "execution is host-qualified");

  // The bound run lives in the follower's job store: name the host to inspect
  // rather than linking into this checkout.
  assert.equal(block.querySelector("a"), null, "no owner-local link is minted for a remote run");
  assert.ok(text.includes("inspect this run on machine hm_follower"), text);

  // An elapsed reservation is a diagnostic. Nothing here may read as revoked.
  assert.ok(text.includes("expired 2026-09-19T00:00:00+00:00"), text);
  assert.ok(text.includes("expiry is not revocation"), text);
  assert.ok(!/revoked/i.test(text), `an expired reservation must not read as revoked: ${text}`);

  // The frozen footprint keeps protecting its files past expiry.
  assert.ok(text.includes("file:src/a.rs"), text);
  assert.ok(text.includes("still protected"), text);

  // A typed not-required disposition, said plainly.
  assert.ok(text.includes("not_required (policy none)"), text);
  assert.ok(text.includes("this is not a code review"), text);

  // Candidate, base and validation evidence are all present and exact.
  assert.ok(text.includes(CANDIDATE.slice(0, 12)), text);
  assert.ok(text.includes(BASE.slice(0, 12)), text);
  assert.ok(text.includes("checks.json"), text);
  assert.ok(text.includes("1 captured log(s) for 1 owner-required command(s): make ci"), text);
  assert.ok(text.includes("pull request #7"), text);

  // Authority and landing are separate facts, and merged is not deployed.
  assert.ok(text.includes("waits for explicit owner approval"), text);
  assert.ok(text.includes("no landing attempt has been reserved"), text);
}

{
  // A task this workspace holds no claim for gets no block at all.
  const block = await mount("ORB-999");
  assert.equal(block.style.display, "none", "a task with no claim shows nothing");
}

// --- workspace switching invalidates the distributed-console cache ---------

{
  const workspaceA = console_([claim({ task_id: "ORB-workspace-a" })]);
  const workspaceB = console_([claim({ claim_id: "claim-workspace-b" })]);
  const authorizedB = claim({ claim_id: "claim-workspace-b" });
  authorizedB.handoff.authority = {
    state: "authorized",
    summary: "completion authority recorded in workspace B",
    authorization_id: "auth-workspace-b",
    recorded_at: "2026-09-19T02:00:00+00:00",
  };
  workspaceConsoles = {
    "workspace-a": workspaceA,
    "workspace-b": workspaceB,
  };
  consoleReads.length = 0;
  sent.length = 0;

  setWorkspace("workspace-a");
  const oldWorkspaceBlock = buildDistributedBlock("ORB-2");
  await settle();
  assert.equal(oldWorkspaceBlock.style.display, "none", "workspace A has no claim for the task");
  assert.equal(
    consoleReads.filter((read) => read.path === "/api/distributed/claims" && read.workspace === "workspace-a").length,
    1,
    "workspace A claim state is read once",
  );

  // This is the dashboard selector path: tasks.js observes the workspace
  // change and must drop the distributed.js memo before the next detail mount.
  setWorkspace("workspace-b");
  const newWorkspaceBlock = buildDistributedBlock("ORB-2");
  await settle();
  assert.equal(newWorkspaceBlock.style.display, "", "workspace B's live claim renders its panel");
  assert.equal(
    consoleReads.filter((read) => read.path === "/api/distributed/claims" && read.workspace === "workspace-b").length,
    1,
    "workspace B issues a fresh workspace-scoped claim read",
  );
  assert.ok(button(newWorkspaceBlock, "Approve handoff"), "workspace B renders the approve action");
  assert.ok(button(newWorkspaceBlock, "Recover claim"), "workspace B renders the recover action");

  // An approval repaint proves that the same newly selected workspace can
  // expose the other capability-gated action from the live claim state.
  workspaceConsoles["workspace-b"] = console_([authorizedB]);
  press(button(newWorkspaceBlock, "Approve handoff"));
  await settle();
  assert.ok(button(newWorkspaceBlock, "Revoke authority"), "workspace B renders the revoke action after approval");

  workspaceConsoles = null;
  setWorkspace(null);
  invalidateDistributedConsole();
  consoleBody = console_([claim()]);
  sent.length = 0;
}

{
  // Merged says the candidate is on the landing branch. Nothing more.
  const merged = claim();
  merged.handoff.authority = { state: "completed", summary: "authority was consumed by a verified merge", authorization_id: "auth-1", recorded_at: null };
  merged.handoff.landing = {
    state: "merged",
    summary: "merged into the landing branch against verified evidence — merged is not deployed",
    attempt: 1, job_run_id: "landing-1", evidence: "merge commit verified", merged: true, deployed: null,
  };
  consoleBody = console_([merged]);
  const block = await mount();
  assert.ok(block.textContent.includes("merged is not deployed"), block.textContent);
  assert.ok(!/deployed to/i.test(block.textContent));
  consoleBody = console_([claim()]);
}

// --- approval ---------------------------------------------------------------

{
  const block = await mount();
  const approve = button(block, "Approve handoff");
  assert.ok(approve, "an unauthorized handoff offers approval");
  assert.equal(approve.getAttribute("data-action"), "approve");

  // The state the operator will see once the decision lands.
  const approved = claim();
  approved.handoff.authority = { state: "authorized", summary: "completion authority recorded; the owner landing job carries it from here", authorization_id: "auth-1", recorded_at: "2026-09-19T01:00:00+00:00" };
  consoleBody = console_([approved]);

  press(approve);
  await settle();

  assert.equal(sent.length, 1);
  assert.equal(sent[0].path, "/api/distributed/handoffs/handoff1/approve");
  // The exact candidate the panel rendered, plus a replay identity.
  assert.equal(sent[0].body.expected_candidate_commit, CANDIDATE);
  assert.equal(sent[0].body.expected_base_commit, BASE);
  assert.ok(sent[0].body.request_id, "a decision carries a replay identity");

  const text = block.textContent;
  assert.ok(text.includes("completion authority recorded"), `the panel repaints from the new state: ${text}`);
  assert.ok(!button(block, "Approve handoff"), "an authorized handoff no longer offers approval");
  assert.ok(button(block, "Revoke authority"), "an authorized handoff offers revocation");
  assert.ok(block.querySelector(".claim-feedback.ok"), "the decision is reported");
  assert.equal(block.querySelector(".claim-feedback").getAttribute("role"), "status");
}

// --- revocation -------------------------------------------------------------

{
  sent.length = 0;
  const authorized = claim();
  authorized.handoff.authority = { state: "authorized", summary: "completion authority recorded", authorization_id: "auth-1", recorded_at: null };
  consoleBody = console_([authorized]);
  const block = await mount();

  const input = reasonInput(block);
  assert.ok(input, "revocation asks for a reason");
  assert.equal(input.getAttribute("aria-label"), "Revoke authority reason");
  input.value = "candidate superseded";

  const revoked = claim();
  revoked.handoff.authority = { state: "revoked", summary: "completion authority was withdrawn; the task stays in review", authorization_id: "auth-1", recorded_at: null };
  consoleBody = console_([revoked]);

  press(button(block, "Revoke authority"));
  await settle();

  assert.equal(sent[0].path, "/api/distributed/handoffs/handoff1/revoke");
  assert.equal(sent[0].body.reason, "candidate superseded");
  assert.equal(sent[0].body.expected_candidate_commit, CANDIDATE);
  assert.ok(block.textContent.includes("the task stays in review"), block.textContent);
}

// --- recovery ---------------------------------------------------------------

{
  sent.length = 0;
  consoleBody = console_([claim({ phase: "running", handoff: null, unsettled: true })]);
  const block = await mount();
  const blockedRecover = recoveryButton(block, "blocked");
  const backlogRecover = recoveryButton(block, "backlog");
  assert.ok(blockedRecover, "an unsettled claim can be recovered as blocked");
  assert.ok(backlogRecover, "an unsettled claim can be recovered as backlog");
  assert.ok(block.textContent.includes("never automatic — there is no heartbeat"), block.textContent);

  reasonInput(block).value = "host was rebuilt";
  press(blockedRecover);
  await settle();

  assert.equal(sent[0].path, "/api/distributed/claims/claim-1/recover");
  // The phase the operator saw travels with the decision, so a claim that moved
  // on is refused rather than recovered.
  assert.equal(sent[0].body.expected_phase, "running");
  assert.equal(sent[0].body.status, "blocked");
  assert.equal(sent[0].body.reason, "host was rebuilt");

  // The retry choice is an independent operator decision, not a UI default.
  sent.length = 0;
  reasonInput(block).value = "host was rebuilt; retry elsewhere";
  press(recoveryButton(block, "backlog"));
  await settle();
  assert.equal(sent[0].body.status, "backlog");
  assert.equal(sent[0].body.reason, "host was rebuilt; retry elsewhere");
}

// --- a stale action re-reads before the operator decides again ---------------

{
  sent.length = 0;
  consoleBody = console_([claim()]);
  const block = await mount();
  assert.ok(button(block, "Approve handoff"));

  // The owner moved on: this handoff was already approved elsewhere.
  const moved = claim();
  moved.handoff.authority = { state: "authorized", summary: "completion authority recorded elsewhere", authorization_id: "auth-9", recorded_at: null };
  consoleBody = console_([moved]);
  nextAction = {
    status: 409,
    body: {
      error: "this action was prepared against candidate aaaa; the owner now holds another (stale_claim)",
      code: "stale_claim",
      remedy: "refresh the view: the owner's claim or handoff state changed since this action was prepared",
    },
  };

  press(button(block, "Approve handoff"));
  await settle();

  const text = block.textContent;
  assert.ok(block.querySelector(".claim-feedback.stale"), `a stale refusal is reported as stale: ${text}`);
  assert.ok(text.includes("refreshed"), text);
  assert.ok(text.includes("completion authority recorded elsewhere"), `the panel shows the owner's current state: ${text}`);
  assert.ok(!button(block, "Approve handoff"), "the stale action is gone after the refresh");
}

// --- an uncertain merge is refused and left alone ---------------------------

{
  sent.length = 0;
  const uncertain = claim();
  uncertain.unresolved_merge_intent = "intent-1";
  uncertain.handoff.authority = { state: "authorized", summary: "completion authority recorded", authorization_id: "auth-1", recorded_at: null };
  uncertain.handoff.uncertain_merge_intent = "intent-1";
  consoleBody = console_([uncertain]);
  const block = await mount();

  assert.ok(block.querySelector(".claim-warning.uncertain-merge"), "an unresolved intent is surfaced");
  assert.ok(block.textContent.includes("reconcile it against the provider"), block.textContent);

  nextAction = {
    status: 409,
    body: {
      error: "unresolved external merge intent",
      code: "uncertain_merge_intent",
      remedy: "reconcile the recorded merge intent against the provider's actual state before revoking or recovering this claim",
    },
  };
  reasonInput(block).value = "withdraw";
  press(button(block, "Revoke authority"));
  await settle();

  assert.equal(sent.length, 1, "the refusal is the backend's, not a disabled button");
  assert.ok(block.querySelector(".claim-feedback.uncertain"), block.textContent);
  assert.ok(block.textContent.includes("unresolved external merge intent"), block.textContent);
  // Still refused, still shown: nothing here quietly resolves the intent.
  assert.ok(block.querySelector(".claim-warning.uncertain-merge"), block.textContent);
}

// --- a session without operator authority is told why ------------------------

{
  consoleBody = console_([claim()], {
    handoff_approve: { authorized: false, reason: "'handoff.approve' requires operator" },
    handoff_revoke: { authorized: false, reason: "'handoff.revoke' requires operator" },
    claim_recover: { authorized: false, reason: "'claim.recover' requires operator" },
  });
  const block = await mount();
  assert.equal(buttons(block).length, 0, "an unauthorized session gets no action buttons");
  const denied = block.querySelector(".claim-action-denied");
  assert.ok(denied, "the reason is shown rather than the action silently vanishing");
  // `title` is the reflected property in both harnesses; the stub DOM sets it
  // as a property rather than an attribute.
  assert.ok(String(denied.title).includes("operator"), String(denied.title));
  // Inspection is unaffected: read-only provenance is not an operator action.
  assert.ok(block.textContent.includes("machine hm_follower"), block.textContent);
}

// --- a replica checkout says where the state lives ---------------------------

{
  consoleBody = {
    schema_version: 1,
    owner_workspace: false,
    refusal: "replica_checkout",
    refusal_detail: "control_plane coordination writes are refused in this replica checkout; workspace is owned by machine 'hm_owner'",
    distributed_execution_enabled: false,
    claims: [],
    capabilities: { handoff_approve: { authorized: true, reason: null }, handoff_revoke: { authorized: true, reason: null }, claim_recover: { authorized: true, reason: null } },
  };
  const block = await mount();
  assert.equal(block.style.display, "", "a replica says so rather than rendering nothing");
  assert.ok(block.textContent.includes("owned by machine 'hm_owner'"), block.textContent);
  assert.equal(buttons(block).length, 0, "a replica offers no owner action");
}

globalThis.distributedTestsPassed = true;
console.log("dashboard distributed claim provenance and owner handoff actions");
