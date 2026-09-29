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
//  * a claim reads as machine-qualified, and a row with no recorded machine reads
//    as *unknown* rather than as this one;
//  * an elapsed reservation is a diagnostic, not a revocation;
//  * a run that executed elsewhere names its machine instead of offering a link
//    into this checkout's job store;
//  * `review` is a delivery handoff awaiting authority, not a code review, and
//    merged is not deployed;
//  * approving and revoking send the exact candidate the operator was shown,
//    with a replay identity, and repaint from the state they produced;
//  * a refused action is honest: a stale conflict re-reads, and an uncertain
//    merge is left alone;
//  * a decision is made once: the panel locks while a request is in flight,
//    destructive actions confirm first, a retry replays the same identity, and
//    the operator sees the outcome even when the detail is rebuilt under it;
//  * the panel never shows more certainty than it has: cached claim state
//    expires, a settled claim reads released rather than expired, and a failed
//    read says so with a way to retry.

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
  executed_on: { known: true, machine_id: "hm_follower", machine_name: "runner-2" },
  run_context: { run_id: "drain-1", job_name: "auto", machine_name: null },
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
    executed_on: { known: true, machine_id: "hm_follower", machine_name: null },
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
let holdConsoleReads = false;
const heldConsoleReads = [];
let failConsoleReads = 0; // fail this many upcoming console reads
let holdPost = null; // a promise the next POSTs wait on
const confirms = [];
let confirmAnswer = true;
// Both harnesses have a `window`; the dashboard asks it before destructive
// owner actions.
window.confirm = (message) => { confirms.push(String(message)); return confirmAnswer; };

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
    if (failConsoleReads > 0) {
      failConsoleReads -= 1;
      return respond({ error: "claims unavailable" }, 500);
    }
    const payload = (workspaceConsoles && workspaceConsoles[workspace]) || consoleBody;
    if (holdConsoleReads) {
      return new Promise((resolve, reject) => {
        heldConsoleReads.push({ payload, workspace, resolve, reject });
      });
    }
    return respond(payload);
  }
  const body = options.body ? JSON.parse(options.body) : null;
  sent.push({ path: url.pathname, body });
  if (holdPost) await holdPost;
  if (nextAction) {
    const refusal = nextAction;
    nextAction = null;
    return respond(refusal.body, refusal.status);
  }
  return respond({ ok: true, result: { handoff_id: "handoff1", claim_id: "claim-1", phase: "handed_off", task_status: "review" } });
};

const distributed = await import("./js/distributed.js");
const { buildDistributedBlock, invalidateDistributedConsole, formatExecutionLocation, CONSOLE_TTL_MS } = distributed;
const { setWorkspace } = await import("./js/common.js");
await import("./js/tasks.js");

const mount = async (taskId = "ORB-2", options = {}) => {
  invalidateDistributedConsole();
  confirms.length = 0;
  confirmAnswer = true;
  const block = buildDistributedBlock(taskId, options);
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
    formatExecutionLocation({ known: true, machine_id: "hm_a", machine_name: "box" }),
    "machine hm_a · name box",
  );
}

{
  const block = await mount();
  const text = block.textContent;

  assert.equal(block.style.display, "", "a task with a claim shows the block");
  assert.equal(block.querySelector("h4").getAttribute("aria-expanded"), "true");
  assert.ok(text.includes("machine hm_follower · name runner-2"), "execution is machine-qualified");

  // The bound run lives in the follower's job store: name the machine to inspect
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
  assert.ok(text.includes("not required · policy none"), text);
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

  // Enums read as words, with the server's summary kept as the note. Raw
  // identifiers are not headlines.
  for (const raw of ["handed_off", "handoff_accepted", "not_authorized", "not_required"]) {
    assert.ok(!text.includes(raw), `raw enum ${raw} leaked into the panel: ${text}`);
  }
  assert.ok(text.includes("handed off"), text);
  assert.ok(text.includes("handoff accepted"), text);
  assert.ok(text.includes("awaiting approval"), text);
  assert.ok(text.includes("not started"), text);

  // Several claims for one task (a retry after recovery) are told apart by id
  // and creation time.
  assert.ok(text.includes("claim claim-1"), text);
  assert.ok(text.includes("created 2026-09-18T00:00:00+00:00"), text);
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
  // An in-flight workspace-A console read that lands after the selector
  // moves to B must not refill the memo. Otherwise the next B detail peeks
  // A's payload and hides B's claim actions.
  const workspaceA = console_([claim({ task_id: "ORB-workspace-a" })]);
  workspaceA.owner_workspace = false;
  workspaceA.refusal_detail = "stale workspace-a replica payload";
  const workspaceB = console_([claim({ claim_id: "claim-workspace-b" })]);
  workspaceConsoles = {
    "workspace-a": workspaceA,
    "workspace-b": workspaceB,
  };
  consoleReads.length = 0;
  sent.length = 0;
  holdConsoleReads = true;

  setWorkspace("workspace-a");
  buildDistributedBlock("ORB-2");
  assert.equal(heldConsoleReads.length, 1, "workspace A console read is in flight");
  assert.equal(heldConsoleReads[0].workspace, "workspace-a");

  setWorkspace("workspace-b");
  const staleReads = heldConsoleReads.splice(0);
  for (const request of staleReads) request.resolve(respond(request.payload));
  await settle();
  holdConsoleReads = false;

  const newWorkspaceBlock = buildDistributedBlock("ORB-2");
  await settle();
  assert.equal(newWorkspaceBlock.style.display, "", "workspace B's live claim renders its panel");
  assert.ok(
    !newWorkspaceBlock.textContent.includes("stale workspace-a replica payload"),
    "stale workspace A payload is not served after the switch",
  );
  assert.equal(
    consoleReads.filter((read) => read.path === "/api/distributed/claims" && read.workspace === "workspace-b").length,
    1,
    "workspace B issues a fresh workspace-scoped claim read after the in-flight A response is discarded",
  );
  assert.ok(button(newWorkspaceBlock, "Approve handoff"), "workspace B renders the approve action");
  assert.ok(button(newWorkspaceBlock, "Recover claim"), "workspace B renders the recover action");

  workspaceConsoles = null;
  holdConsoleReads = false;
  heldConsoleReads.length = 0;
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

  // The owner would refuse both, so neither is offered as a live control — and
  // the reason is visible text, not a tooltip.
  const revoke = button(block, "Revoke authority");
  const recover = recoveryButton(block, "blocked");
  assert.ok(revoke && revoke.disabled, "revoke is disabled while a merge is uncertain");
  assert.ok(recover && recover.disabled, "recover is disabled while a merge is uncertain");
  const reasons = Array.from(block.querySelectorAll(".claim-action-denied")).map((node) => node.textContent);
  assert.ok(reasons.length >= 2 && reasons.every((text) => text.includes("uncertain merge")), JSON.stringify(reasons));
  press(revoke);
  press(recover);
  await settle();
  assert.equal(sent.length, 0, "a disabled action sends nothing");
  assert.equal(confirms.length, 0, "a disabled action does not even ask");
}

{
  // The intent can also appear after the panel was rendered. Then the backend
  // is the boundary, and its refusal is reported with its remedy and left alone.
  sent.length = 0;
  const authorized = claim();
  authorized.handoff.authority = { state: "authorized", summary: "completion authority recorded", authorization_id: "auth-1", recorded_at: null };
  consoleBody = console_([authorized]);
  const block = await mount();
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

  assert.equal(sent.length, 1, "the refusal is the backend's");
  const feedback = block.querySelector(".claim-feedback.uncertain");
  assert.ok(feedback, block.textContent);
  assert.equal(feedback.getAttribute("role"), "alert");
  assert.ok(block.textContent.includes("unresolved external merge intent"), block.textContent);
  assert.ok(block.textContent.includes("reconcile the recorded merge intent"), `the remedy is shown: ${block.textContent}`);
  assert.ok(!button(block, "Revoke authority").disabled, "a refused action releases the panel for the operator's next decision");
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
  // ...and the reason is not tooltip-only: it is text an operator can read.
  assert.ok(denied.textContent.includes("requires operator"), denied.textContent);
  // Inspection is unaffected: read-only provenance is not an operator action.
  assert.ok(block.textContent.includes("machine hm_follower"), block.textContent);
}

// --- a replica checkout stays out of every task detail ---------------------------

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
  // The replica note would repeat on every task; the block stays hidden.
  assert.equal(block.style.display, "none", "a replica shows no per-task block");
  assert.equal(buttons(block).length, 0, "a replica offers no owner action");
}

// --- a review task's plain "approve" goes through its handoff ----------------

{
  // A delivered handoff awaiting authority: the task-level approve must send
  // the claim-scoped approval, because the owner refuses an unscoped status
  // write while the claim protects the task.
  consoleBody = console_([claim()]);
  const pending = await distributed.claimedReviewApproval("ORB-2");
  assert.equal(pending.claim.claim_id, "claim-1");
  const request = distributed.handoffApprovalRequest(pending.claim);
  assert.equal(request.path, "/api/distributed/handoffs/handoff1/approve");
  assert.equal(request.body.expected_candidate_commit, CANDIDATE);
  assert.equal(request.body.expected_base_commit, BASE);

  // Already authorized (an operator, or the owner's completion policy): the
  // landing job completes it, and approving again is not offered as success.
  const authorized = claim();
  authorized.handoff = {
    ...authorized.handoff,
    authority: { state: "authorized", summary: "completion authority recorded; the owner landing job carries it from here" },
  };
  consoleBody = console_([authorized]);
  const decided = await distributed.claimedReviewApproval("ORB-2");
  assert.ok(decided.refusal && decided.refusal.includes("landing job"), JSON.stringify(decided));

  // Each decided state says what is true of it. A revoked handoff is not
  // finished by any landing job, and the server's own summary wins when present.
  const decidedWith = async (authority) => {
    const target = claim();
    target.handoff = { ...target.handoff, authority };
    consoleBody = console_([target]);
    return (await distributed.claimedReviewApproval("ORB-2")).refusal;
  };
  const revokedSummary = "completion authority was withdrawn; the task stays in review";
  assert.equal(await decidedWith({ state: "revoked", summary: revokedSummary }), revokedSummary);
  const revokedFallback = await decidedWith({ state: "revoked" });
  assert.ok(revokedFallback.includes("withdrawn") && !revokedFallback.includes("landing job"), revokedFallback);
  const completed = await decidedWith({ state: "completed" });
  assert.ok(completed.includes("already landed") && !completed.includes("landing job"), completed);

  // Still running: nothing to approve yet.
  consoleBody = console_([claim({ phase: "running", handoff: null })]);
  const running = await distributed.claimedReviewApproval("ORB-2");
  assert.ok(running.refusal && running.refusal.includes("has not handed off"), JSON.stringify(running));

  // No claim for this task, or only a settled one: the ordinary approval applies.
  consoleBody = console_([claim({ phase: "failed", unsettled: false, handoff: null })]);
  assert.equal(await distributed.claimedReviewApproval("ORB-2"), null);
  consoleBody = console_([]);
  assert.equal(await distributed.claimedReviewApproval("ORB-2"), null);
}

// --- destructive actions ask first ------------------------------------------

{
  sent.length = 0;
  const authorized = claim();
  authorized.handoff.authority = { state: "authorized", summary: "completion authority recorded", authorization_id: "auth-1", recorded_at: null };
  consoleBody = console_([authorized]);
  const block = await mount();

  // Declined: nothing is sent, and the panel is still usable.
  confirmAnswer = false;
  reasonInput(block).value = "superseded";
  press(button(block, "Revoke authority"));
  await settle();
  assert.equal(confirms.length, 1, "revoking asks first");
  assert.ok(confirms[0].includes("ORB-2"), `the confirmation names the task: ${confirms[0]}`);
  assert.ok(/withdraws? (completion )?authority/i.test(confirms[0]), `the confirmation names the effect: ${confirms[0]}`);
  press(recoveryButton(block, "backlog"));
  await settle();
  assert.equal(confirms.length, 2, "recovering asks first");
  assert.ok(confirms[1].includes("ORB-2") && confirms[1].includes("backlog"), confirms[1]);
  assert.ok(confirms[1].includes("fences"), `the confirmation names the effect: ${confirms[1]}`);
  assert.equal(sent.length, 0, "a declined confirmation sends nothing");
  assert.ok(!button(block, "Revoke authority").disabled, "declining leaves the panel usable");

  // Accepted: the same click now goes through.
  confirmAnswer = true;
  press(button(block, "Revoke authority"));
  await settle();
  assert.equal(sent.length, 1, "an accepted confirmation sends the decision");
  assert.equal(sent[0].path, "/api/distributed/handoffs/handoff1/revoke");

  // Approving does not need a confirmation: it records authority, it does not
  // withdraw or fence anything.
  confirms.length = 0;
  consoleBody = console_([claim()]);
  const fresh = await mount();
  press(button(fresh, "Approve handoff"));
  await settle();
  assert.equal(confirms.length, 0, "approving does not ask");
}

// --- one decision, one request -------------------------------------------------

{
  sent.length = 0;
  consoleBody = console_([claim()]);
  const block = await mount();
  const approve = button(block, "Approve handoff");
  const recover = recoveryButton(block, "blocked");
  const approved = claim();
  approved.handoff.authority = { state: "authorized", summary: "completion authority recorded", authorization_id: "auth-1", recorded_at: null };
  consoleBody = console_([approved]);

  let release;
  holdPost = new Promise((resolve) => { release = resolve; });
  press(approve);
  press(approve);
  await settle();

  assert.equal(sent.length, 1, "a double click posts once");
  // Every control in the panel is locked while the decision is in flight, and
  // the operator can see why.
  assert.ok(approve.disabled && recover.disabled, "all claim actions are disabled while a request is in flight");
  assert.ok(approve.textContent.includes("working"), approve.textContent);
  assert.equal(block.querySelector(".claim-actions").getAttribute("data-busy"), "true");
  press(recover);
  await settle();
  assert.equal(sent.length, 1, "another action cannot start while one is in flight");
  assert.equal(confirms.length, 0, "a locked action does not ask");

  holdPost = null;
  release();
  await settle();
  assert.equal(sent.length, 1);
  assert.ok(button(block, "Revoke authority"), "the panel repaints from the state the decision produced");
}

{
  // A retry of the same decision replays: it carries the same identity. A
  // different decision (another reason or another status) is a new request.
  sent.length = 0;
  consoleBody = console_([claim({ phase: "running", handoff: null })]);
  const block = await mount();
  reasonInput(block).value = "host was rebuilt";
  nextAction = { status: 500, body: { error: "owner temporarily unavailable" } };
  press(recoveryButton(block, "blocked"));
  await settle();
  assert.ok(block.querySelector(".claim-feedback.error"), "a failed decision is reported");
  assert.ok(!recoveryButton(block, "blocked").disabled, "a failed decision releases the panel");

  press(recoveryButton(block, "blocked"));
  await settle();
  assert.equal(sent.length, 2);
  assert.ok(sent[0].body.request_id, "the decision carries an identity");
  assert.equal(sent[1].body.request_id, sent[0].body.request_id, "retrying the same decision replays its identity");

  consoleBody = console_([claim({ phase: "running", handoff: null })]);
  const other = await mount();
  reasonInput(other).value = "a different reason";
  press(recoveryButton(other, "backlog"));
  await settle();
  assert.ok(sent[2].body.request_id !== sent[0].body.request_id, "a different decision is a different request");
}

// --- claim state does not go stale -------------------------------------------

{
  const realNow = Date.now;
  let clock = realNow();
  Date.now = () => clock;
  try {
    consoleBody = console_([claim()]);
    const first = await mount();
    assert.ok(button(first, "Approve handoff"), "the first read offers approval");
    const reads = consoleReads.length;

    // Inside the window, rows share one read.
    clock += 1000;
    const shared = buildDistributedBlock("ORB-2");
    await settle();
    assert.equal(consoleReads.length, reads, "a detail opened inside the window reuses the read");
    assert.ok(button(shared, "Approve handoff"));

    // The owner approves elsewhere; the dashboard's own refresh never re-reads
    // claims, so only expiry stops the panel offering a decision that is over.
    const approved = claim();
    approved.handoff.authority = { state: "authorized", summary: "completion authority recorded elsewhere", authorization_id: "auth-9", recorded_at: null };
    consoleBody = console_([approved]);
    clock += CONSOLE_TTL_MS + 1;
    const later = buildDistributedBlock("ORB-2");
    await settle();
    assert.equal(consoleReads.length, reads + 1, "a detail opened after the window re-reads");
    assert.ok(!button(later, "Approve handoff"), "the stale approve button is gone");
    assert.ok(later.textContent.includes("completion authority recorded elsewhere"), later.textContent);
  } finally {
    Date.now = realNow;
  }
}

// --- the operator sees the outcome, from the state it produced -----------------

{
  sent.length = 0;
  consoleBody = console_([claim()]);
  const readsAtChange = [];
  let rebuilt = null;
  const block = await mount("ORB-2", {
    // The dashboard refreshes its task list, which rebuilds this detail.
    onTaskChanged: () => {
      readsAtChange.push(consoleReads.length);
      rebuilt = buildDistributedBlock("ORB-2");
    },
  });
  const before = consoleReads.length;
  const approved = claim();
  approved.handoff.authority = { state: "authorized", summary: "completion authority recorded", authorization_id: "auth-1", recorded_at: null };
  consoleBody = console_([approved]);

  press(button(block, "Approve handoff"));
  await settle();

  assert.equal(readsAtChange.length, 1);
  assert.equal(readsAtChange[0], before + 1, "the console is re-read before the task list is told to refresh");
  assert.ok(rebuilt, "the detail was rebuilt");
  assert.ok(!button(rebuilt, "Approve handoff"), "the rebuilt detail never renders the pre-action state with live buttons");
  assert.ok(button(rebuilt, "Revoke authority"));
  // Feedback survives the rebuild: it is in the block the operator now sees.
  assert.ok(rebuilt.querySelector(".claim-feedback.ok"), "the rebuilt detail carries the decision's outcome");
  assert.ok(block.querySelector(".claim-feedback.ok"), "and so does the original if it is still on the page");
}

{
  // A refusal also reaches the block the operator is looking at.
  sent.length = 0;
  consoleBody = console_([claim()]);
  let rebuilt = null;
  const block = await mount("ORB-2", { onTaskChanged: () => { rebuilt = buildDistributedBlock("ORB-2"); } });
  nextAction = { status: 400, body: { error: "candidate not found", code: "no_candidate", remedy: "push the candidate branch and try again" } };
  press(button(block, "Approve handoff"));
  await settle();
  assert.ok(block.querySelector(".claim-feedback.error"), block.textContent);
  assert.ok(block.textContent.includes("push the candidate branch"), `the remedy is rendered: ${block.textContent}`);
  assert.equal(rebuilt, null, "a refusal changes nothing, so the task list is not refreshed");
}

{
  // The action went through but the follow-up read failed. One message, not a
  // green confirmation beside a red error, and a way to try again.
  sent.length = 0;
  consoleBody = console_([claim()]);
  const block = await mount();
  const approved = claim();
  approved.handoff.authority = { state: "authorized", summary: "completion authority recorded", authorization_id: "auth-1", recorded_at: null };
  consoleBody = console_([approved]);
  failConsoleReads = 1;
  press(button(block, "Approve handoff"));
  await settle();

  assert.equal(sent.length, 1);
  assert.ok(!block.querySelector(".claim-feedback.ok"), "no success banner beside a failed read");
  const notes = Array.from(block.querySelectorAll(".claim-feedback"));
  assert.equal(notes.length, 1, `one message, got ${notes.length}: ${block.textContent}`);
  assert.equal(notes[0].getAttribute("role"), "alert");
  assert.ok(notes[0].textContent.includes("decision recorded"), "the decision that went through is not hidden");
  assert.ok(notes[0].textContent.includes("claims unavailable"), notes[0].textContent);
  const retry = block.querySelector("button.claim-retry");
  assert.ok(retry, "a failed read offers retry");
  press(retry);
  await settle();
  assert.ok(button(block, "Revoke authority"), "retry re-reads and repaints");
  assert.ok(!block.querySelector("button.claim-retry"), "the error is gone once the read works");
}

{
  // An initial read failure is the same: an alert and a retry, not a dead message.
  consoleBody = console_([claim()]);
  invalidateDistributedConsole();
  failConsoleReads = 1;
  const block = buildDistributedBlock("ORB-2");
  await settle();
  assert.equal(block.style.display, "", "a failed read is shown");
  const alert = block.querySelector(".claim-feedback.error");
  assert.ok(alert && alert.getAttribute("role") === "alert", "the failure is announced as an alert");
  assert.ok(alert.textContent.includes("claims unavailable"), alert.textContent);
  press(block.querySelector("button.claim-retry"));
  await settle();
  assert.ok(button(block, "Approve handoff"), "retry recovers the panel");
}

// --- a settled claim is history, not a warning ---------------------------------

{
  const landed = claim({ phase: "landed", unsettled: false, footprint_protected: false });
  landed.handoff.authority = { state: "completed", summary: "authority was consumed by a verified merge", authorization_id: "auth-1", recorded_at: null };
  consoleBody = console_([landed]);
  const block = await mount();
  const text = block.textContent;
  assert.ok(text.includes("released"), `a settled reservation reads released: ${text}`);
  assert.ok(!text.includes("expired"), `a settled claim's reservation is not shown as expired: ${text}`);
  assert.ok(!text.includes("still live"), `the live-claim note does not apply to a settled claim: ${text}`);
  assert.equal(block.querySelector(".claim-line.expired"), null, "no expired styling on a settled claim");
  assert.equal(buttons(block).length, 0, "nothing to decide on a landed claim");
}

// --- times and empty fields ------------------------------------------------------

{
  // Times use the page's own formatter, passed in rather than imported.
  consoleBody = console_([claim()]);
  const block = await mount("ORB-2", { formatTime: (value) => `fmt(${value})` });
  assert.ok(block.textContent.includes("expired fmt(2026-09-19T00:00:00+00:00)"), block.textContent);
  assert.ok(block.textContent.includes("created fmt(2026-09-18T00:00:00+00:00)"), block.textContent);
}

{
  // Absent fields read as absent, not as the word "undefined" or a dangling dash.
  const sparse = claim({ phase_summary: "", reservation: { expired: false } });
  consoleBody = console_([sparse]);
  const block = await mount();
  const text = block.textContent;
  assert.ok(!text.includes("undefined"), `no field prints "undefined": ${text}`);
  assert.ok(text.includes("no expiry recorded"), text);
  assert.ok(!/handed off\s*—/.test(text), `an empty summary leaves no dangling dash: ${text}`);
}

{
  // An event this build has no word for stays readable.
  consoleBody = console_([claim({ last_event: "claim_bound" }), claim({ claim_id: "claim-2", last_event: "claim_made_up_event" })]);
  const block = await mount();
  const text = block.textContent;
  assert.ok(text.includes("run bound") && !text.includes("claim_bound"), text);
  assert.ok(text.includes("claim made up event"), text);
  assert.equal(block.querySelectorAll(".claim-panel").length, 2, "each claim gets its own panel");
  assert.ok(text.includes("claim claim-2"), "a second claim is told apart by its header");
}

globalThis.distributedTestsPassed = true;
console.log("dashboard distributed claim provenance and owner handoff actions");
