// Run cancel / resume / replay: what the operator is told when an action or the
// refresh after it fails. Drives the shipped `runs.js` against a fetch stub.
class Node {
  constructor(id = "") { this.id = id; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false; }
  appendChild(child) { if (child == null) return child; if (child.parentNode) child.parentNode.removeChild(child); this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { if (child.parentNode) child.parentNode.removeChild(child); const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  getAttribute(name) { return this[name] ?? null; }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this.textContent = value; }
  get innerHTML() { return this.textContent; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
  get classList() { const self = this; return { add: (...classes) => { for (const c of classes) if (!self.className.split(/\s+/).includes(c)) self.className = `${self.className} ${c}`.trim(); }, toggle: (c, on) => { if (on) this.addClass(c); } }; }
  addClass(c) { if (!this.className.split(/\s+/).includes(c)) this.className = `${this.className} ${c}`.trim(); }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
globalThis.document = {
  getElementById: get,
  createElement: () => new Node(),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
};
const location = new URL("http://dashboard.test/");
globalThis.window = { location, innerWidth: 1200, confirm: () => true, prompt: () => "operator cancelled" };
globalThis.history = { replaceState: () => {} };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });

const response = (payload, status = 200) => ({ ok: status < 400, status, json: async () => payload, text: async () => JSON.stringify(payload) });
let nextResponse = () => response({});
globalThis.fetch = async () => nextResponse();

const runs = [
  { workspace_id: "alpha", workspace_name: "Alpha", run_id: "jrun-live", job_id: "ship", state: "running", created_at: "2026-09-05T03:02:00Z" },
  { workspace_id: "alpha", workspace_name: "Alpha", run_id: "jrun-broken", job_id: "ship", state: "failed", created_at: "2026-09-05T03:01:00Z" },
];
let refreshFails = false;
const { initRuns, renderRuns } = await import("./js/runs.js");
initRuns({
  getLastRuns: () => runs,
  getRunsMeta: () => ({ truncated: false }),
  getRunSourcesUnavailable: () => [],
  navigateToRun: () => {},
  fetchAndRenderRuns: async () => {
    if (refreshFails) throw new Error("HTTP 502");
    renderRuns(runs);
  },
  getActiveRunId: () => null,
});
const settle = async () => { for (let i = 0; i < 8; i++) await new Promise((resolve) => setTimeout(resolve, 0)); };
const assert = (condition, message) => { if (!condition) throw new Error(message); };
renderRuns(runs);
const body = get("runs-body");
const rowFor = (id) => body.children.find((node) => node.textContent.includes(id) && node.className.includes("runs-row") && !node.className.includes("runs-header"));
const action = (id, name) => rowFor(id).children.at(-1).children.find((node) => node.className.includes(name));
const press = async (button) => { button.listeners.click({ stopPropagation() {} }); await settle(); };
const errors = () => body.children.filter((node) => node.dataset.key === "run-action-error");

// Every row action names its run (and workspace): "cancel" alone is
// ambiguous once a screen reader lists the controls out of context.
assert(action("jrun-live", "run-cancel").getAttribute("aria-label") === "Cancel run jrun-live in Alpha", `cancel label: ${action("jrun-live", "run-cancel").getAttribute("aria-label")}`);
assert(action("jrun-broken", "run-resume").getAttribute("aria-label") === "Resume run jrun-broken in Alpha", "resume label names its run");

// A refused cancel is reported, and the report outlives the poll that
// re-renders the table: an error written into the unkeyed host was dropped by
// the next render before anyone could read it.
nextResponse = () => response({ error: "run is not cancellable" }, 409);
await press(action("jrun-live", "run-cancel"));
assert(errors().length === 1 && errors()[0].textContent.includes("run is not cancellable"), `refused cancel is reported: ${body.textContent}`);
assert(errors()[0].getAttribute("role") === "alert", "the failure is announced");
assert(!action("jrun-live", "run-cancel").disabled, "a refused cancel re-arms the button");
renderRuns(runs);
renderRuns(runs);
assert(errors().length === 1, "the error survives the re-renders a poll causes");
errors()[0].children.find((node) => node.textContent === "dismiss").listeners.click({ stopPropagation() {} });
assert(errors().length === 0, "dismiss removes the error");

// The cancel went through but the refresh after it failed: the run is
// cancelled, and the message says the view is stale rather than that the
// cancel failed. The button stays spent.
nextResponse = () => response({ run_id: "jrun-live", outcome: "cancelled" });
refreshFails = true;
const cancel = action("jrun-live", "run-cancel");
await press(cancel);
assert(cancel.textContent === "cancelled" && cancel.disabled, `a cancelled run's button is spent: ${cancel.textContent}`);
assert(errors().length === 1 && errors()[0].textContent.includes("jrun-live was cancelled, but the view could not refresh: HTTP 502"), `stale view is not reported as a failed cancel: ${errors()[0]?.textContent}`);
assert(!body.textContent.includes("cancel failed"), "the cancel is not reported as failed");
refreshFails = false;

// Same for resume: the new run exists even though the list did not refresh.
nextResponse = () => response({ run_id: "jrun-resumed" });
refreshFails = true;
await press(action("jrun-broken", "run-resume"));
assert(errors().length === 1 && errors()[0].textContent.includes("jrun-broken was resumed, but the list could not refresh"), `resume with failed refresh: ${errors()[0]?.textContent}`);
refreshFails = false;

// Starting the next action clears the previous report.
nextResponse = () => response({ run_id: "jrun-next" });
await press(action("jrun-broken", "run-resume"));
assert(errors().length === 0, `a new action clears the old error: ${errors().map((node) => node.textContent)}`);

// Cancelling a claimed leaf fails its claim on the owner, so the confirmation
// says so and names the owner task; an ordinary run's prompt is unchanged.
// Prompts return null so nothing is posted: only the confirmation is under test.
const prompts = [];
window.prompt = (text) => { prompts.push(text); return null; };
const requests = [];
const claim = { task_id: "ORB-4242", claim_id: "claim-1", owner: "owner-mac/ws", drain_run_id: "jrun-drain", settlement_phase: "launched", refusal: null, guidance: "" };
const { buildCancelRunButton } = await import("./js/runs.js");
const cancelPrompt = async (run) => {
  prompts.length = 0;
  const button = buildCancelRunButton(run, body);
  await press(button);
  assert(prompts.length === 1, `one confirmation is shown: ${prompts.length}`);
  assert(!button.disabled, "declining the confirmation leaves the button armed");
  return prompts[0];
};
const leafBase = { workspace_id: "alpha", run_id: "jrun-leaf", job_id: "task_claimed_pr_pipeline", state: "running" };

// Run detail: the claim is carried on the run the cancel button is given.
const detailPrompt = await cancelPrompt({ ...leafBase, pull_claim: claim });
assert(detailPrompt.includes("Cancel jrun-leaf?") && detailPrompt.includes("Add a reason (optional):"), `existing wording kept: ${detailPrompt}`);
assert(/fails that claim on owner-mac\/ws/.test(detailPrompt) && detailPrompt.includes("task ORB-4242") && detailPrompt.includes("owner's task is blocked"), `claimed leaf confirmation names the owner claim: ${detailPrompt}`);
const noTaskPrompt = await cancelPrompt({ ...leafBase, pull_claim: { ...claim, task_id: null } });
assert(noTaskPrompt.includes("owner's task is blocked") && !noTaskPrompt.includes("task null"), `no owner task, no invented name: ${noTaskPrompt}`);

// An explicit null claim (run detail of an ordinary run) keeps the plain prompt
// and asks nothing more of the server.
globalThis.fetch = async (url) => { requests.push(String(url)); return response({ run: {}, pull_claim: claim }); };
requests.length = 0;
assert(await cancelPrompt({ ...leafBase, job_id: "ship", pull_claim: null }) === "Cancel jrun-leaf? Add a reason (optional):", "an ordinary run's prompt is unchanged");
assert(await cancelPrompt({ workspace_id: "alpha", run_id: "jrun-plain", job_id: "ship", state: "running" }) === "Cancel jrun-plain? Add a reason (optional):", "a list row of an ordinary job is not looked up");
assert(requests.length === 0, `no detail read for ordinary runs: ${requests}`);

// A list row carries no claim; a claimed-leaf job's row reads the run detail once.
const listPrompt = await cancelPrompt(leafBase);
assert(requests.length === 1 && requests[0].startsWith("/api/runs/jrun-leaf") && requests[0].includes("workspace=alpha"), `list row reads its scoped detail: ${requests}`);
assert(listPrompt.includes("task ORB-4242") && listPrompt.includes("fails that claim"), `list-row claimed leaf confirmation: ${listPrompt}`);

// The detail says it is not a claim, or cannot be read: plain prompt, still cancellable.
globalThis.fetch = async () => response({ run: {}, pull_claim: null });
assert(await cancelPrompt(leafBase) === "Cancel jrun-leaf? Add a reason (optional):", "a leaf job run with no admission keeps the plain prompt");
globalThis.fetch = async () => response({ error: "boom" }, 500);
const unreadable = await cancelPrompt(leafBase);
assert(unreadable === "Cancel jrun-leaf? Add a reason (optional):", `an unreadable claim does not block or alter the prompt: ${unreadable}`);
